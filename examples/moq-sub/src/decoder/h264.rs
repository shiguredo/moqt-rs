//! Apple Video Toolbox による H.264 デコーダ実装
//!
//! macOS 専用。`shiguredo_video_toolbox` を用いた AVC (H.264) デコードを提供する。

use std::sync::mpsc;

use shiguredo_video_toolbox::{
    DecodedFrame, Decoder, DecoderCodec, DecoderConfig, FnDecodeHandler, PixelFormat,
};

use crate::decoder::{DecodedVideoFrame, copy_plane, read_ps_nal};
use crate::error::{Error, Result};

/// ISO/IEC 14496-15 §5.2.4.1.1 AVCDecoderConfigurationRecord 最小サイズ (lengthSizeMinusOne まで)
const AVC_CONFIG_MIN_LEN: usize = 7;

/// デコード結果を運ぶチャネルのメッセージ
///
/// Video Toolbox はデコード結果をコールバックスレッドから通知するため、チャネルを経由して
/// `decode()` を呼び出したスレッドへ運ぶ。`Ok(None)` は「デコードは完了したがピクセルが
/// 得られなかった」を意味する。
type DecodeResult = std::result::Result<Option<DecodedVideoFrame>, Error>;

/// 最初のキーフレーム到達までデコーダを遅延初期化する H.264 デコーダ
pub struct H264Decoder {
    decoder: Option<Decoder<FnDecodeHandler<(), Error>>>,
    /// デコード結果を受け取るチャネルの受信側
    results: mpsc::Receiver<DecodeResult>,
    /// ハンドラへ渡す送信側
    ///
    /// デコーダを (再) 構築するたびに複製してハンドラへ渡す。すべての送信側が破棄されると
    /// チャネルが切断され、受信側で結果を待てなくなるため本構造体でも保持する。
    results_tx: mpsc::Sender<DecodeResult>,
    nalu_len_bytes: u32,
    last_config: Vec<u8>,
}

impl H264Decoder {
    pub fn new() -> Result<Self> {
        let (results_tx, results) = mpsc::channel();
        Ok(Self {
            decoder: None,
            results,
            results_tx,
            nalu_len_bytes: 4,
            last_config: Vec::new(),
        })
    }

    /// avcC から SPS / PPS / `nalu_len_bytes` を取り出してデコーダを (再) 構築する
    fn apply_config(&mut self, config: &[u8]) -> Result<()> {
        let AvcConfig {
            sps,
            pps,
            nalu_len_bytes,
        } = parse_avc_config(config)?;
        let decoder_codec = DecoderCodec::H264 {
            sps: &sps,
            pps: &pps,
            nalu_len_bytes,
        };
        match self.decoder.as_mut() {
            Some(decoder) => decoder.update_format(decoder_codec)?,
            None => {
                let cfg = DecoderConfig {
                    codec: decoder_codec,
                    pixel_format: PixelFormat::I420,
                };
                self.decoder = Some(Decoder::new(cfg, decode_handler(&self.results_tx))?);
            }
        }
        self.nalu_len_bytes = nalu_len_bytes;
        self.last_config = config.to_vec();
        Ok(())
    }
}

impl H264Decoder {
    /// 1 オブジェクト分のエンコード済みペイロードをデコードする
    pub fn decode(
        &mut self,
        payload: &[u8],
        video_config: Option<&[u8]>,
    ) -> Result<Vec<DecodedVideoFrame>> {
        if let Some(config) = video_config
            && config != self.last_config.as_slice()
        {
            self.apply_config(config)?;
        }
        let Some(decoder) = self.decoder.as_mut() else {
            // avcC 未到達: キーフレーム前の受信なので破棄して次を待つ
            tracing::warn!("H.264 decoder not initialized yet (waiting for avcC)");
            return Ok(Vec::new());
        };
        decoder.decode(payload, ())?;
        // Video Toolbox はデコード完了をコールバックスレッドから通知し、1 つの入力について
        // 必ず 1 回通知する。対応する結果が届くまで待ってから返すことで、同期デコードで
        // 結果を直接返していた従来と同じく、入力 1 件につきその結果を返す。
        let result = self
            .results
            .recv()
            .map_err(|_| Error::Other("H.264 decode result channel is closed".to_string()))?;
        match result? {
            Some(frame) => Ok(vec![frame]),
            None => Ok(Vec::new()),
        }
    }
}

/// デコード結果をチャネルへ送るハンドラを作る
///
/// Video Toolbox のコールバックが返す [`DecodedFrame`] はピクセルバッファへの生ポインタを
/// 持つためスレッド間で移動できず、そのままではチャネルで送れない。そのためコールバックの
/// 中でプレーンをコピーした [`DecodedVideoFrame`] へ詰め替えてから送る。
fn decode_handler(results_tx: &mpsc::Sender<DecodeResult>) -> FnDecodeHandler<(), Error> {
    let results_tx = results_tx.clone();
    FnDecodeHandler::new(
        move |result: std::result::Result<DecodedFrame<()>, Error>| {
            // 受信側が破棄された後 (デコーダ破棄時) の通知は捨てる
            let _ = results_tx.send(convert_decoded_frame(result));
        },
    )
}

/// コールバックが返したデコード結果を、送信できるフレームへ詰め替える
///
/// 従来の実装と同じく、プレーンをコピーして stride を除去した I420 にする。
/// `Ok(None)` は「ピクセルが得られなかった」を意味し、フレーム無しとして扱う。
fn convert_decoded_frame(result: std::result::Result<DecodedFrame<()>, Error>) -> DecodeResult {
    match result {
        Ok(DecodedFrame::I420 { frame, .. }) => {
            let w = frame.width();
            let h = frame.height();
            if frame.y_plane().is_empty() {
                return Ok(None);
            }
            let y = copy_plane(frame.y_plane(), frame.y_stride(), w, h);
            let u = copy_plane(frame.u_plane(), frame.u_stride(), w / 2, h / 2);
            let v = copy_plane(frame.v_plane(), frame.v_stride(), w / 2, h / 2);
            Ok(Some(DecodedVideoFrame {
                y,
                u,
                v,
                width: w as i32,
                height: h as i32,
                // Timestamp は Object の Properties から pipeline が載せる
                timestamp_us: None,
            }))
        }
        Ok(DecodedFrame::Nv12 { .. }) => Err(Error::Other(
            "H.264 decoder returned NV12 but I420 was requested".to_string(),
        )),
        Err(e) => Err(e),
    }
}

struct AvcConfig {
    sps: Vec<u8>,
    pps: Vec<u8>,
    nalu_len_bytes: u32,
}

/// ISO/IEC 14496-15 §5.2.4.1.1 `AVCDecoderConfigurationRecord` をパースする
///
/// 最初の SPS / PPS のみ取り出す (複数 SPS / PPS は想定外として捨てる)。
fn parse_avc_config(buf: &[u8]) -> Result<AvcConfig> {
    if buf.len() < AVC_CONFIG_MIN_LEN {
        return Err(Error::Other(format!(
            "AVCDecoderConfigurationRecord too short: {}",
            buf.len()
        )));
    }
    if buf[0] != 1 {
        return Err(Error::Other(format!(
            "AVCDecoderConfigurationRecord: unsupported configurationVersion {}",
            buf[0]
        )));
    }
    let nalu_len_bytes = u32::from((buf[4] & 0x03) + 1);
    let num_sps = (buf[5] & 0x1F) as usize;
    if num_sps == 0 {
        return Err(Error::Other(
            "AVCDecoderConfigurationRecord has no SPS".to_string(),
        ));
    }
    let mut pos = 6;
    let sps = read_ps_nal(buf, &mut pos, "AVCDecoderConfigurationRecord")?;
    // 追加 SPS は 14496-15 で許容されるが subscriber では先頭のみを採用
    for _ in 1..num_sps {
        skip_ps_nal(buf, &mut pos)?;
    }
    if pos >= buf.len() {
        return Err(Error::Other(
            "AVCDecoderConfigurationRecord: truncated before PPS count".to_string(),
        ));
    }
    let num_pps = buf[pos] as usize;
    pos += 1;
    if num_pps == 0 {
        return Err(Error::Other(
            "AVCDecoderConfigurationRecord has no PPS".to_string(),
        ));
    }
    let pps = read_ps_nal(buf, &mut pos, "AVCDecoderConfigurationRecord")?;
    for _ in 1..num_pps {
        skip_ps_nal(buf, &mut pos)?;
    }
    Ok(AvcConfig {
        sps,
        pps,
        nalu_len_bytes,
    })
}

/// `u16 length + NAL` を読み飛ばす
fn skip_ps_nal(buf: &[u8], pos: &mut usize) -> Result<()> {
    if buf.len() < *pos + 2 {
        return Err(Error::Other(
            "AVCDecoderConfigurationRecord: truncated NAL length".to_string(),
        ));
    }
    let len = u16::from_be_bytes([buf[*pos], buf[*pos + 1]]) as usize;
    *pos += 2 + len;
    if buf.len() < *pos {
        return Err(Error::Other(
            "AVCDecoderConfigurationRecord: truncated NAL payload".to_string(),
        ));
    }
    Ok(())
}
