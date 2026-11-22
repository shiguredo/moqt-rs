//! Apple Video Toolbox による H.264 デコーダ実装
//!
//! macOS 専用。`shiguredo_video_toolbox` を用いた AVC (H.264) デコードを提供する。
//! 入力は MP4 のサンプル (length-prefixed NAL) で、avcC のレコード本体を設定として渡す。

use std::sync::mpsc;

use shiguredo_video_toolbox::{
    DecodedFrame, Decoder, DecoderCodec, DecoderConfig, FnDecodeHandler, PixelFormat,
};

use crate::error::{Error, Result};
use crate::{DecodedVideoFrame, copy_plane, read_ps_nal, skip_ps_nal};

/// ISO/IEC 14496-15 §5.2.4.1.1 AVCDecoderConfigurationRecord 最小サイズ (lengthSizeMinusOne まで)
const AVC_CONFIG_MIN_LEN: usize = 7;

/// デコード結果を運ぶチャネルのメッセージ
///
/// Video Toolbox はデコード結果をコールバックスレッドから通知するため、チャネルを経由して
/// `decode()` を呼び出したスレッドへ運ぶ。`Ok(None)` は「デコードは完了したがピクセルが
/// 得られなかった」を意味する。
type DecodeResult = std::result::Result<Option<DecodedVideoFrame>, Error>;

/// avcC の到着時に初期化する H.264 デコーダ
pub struct H264Decoder {
    decoder: Option<Decoder<FnDecodeHandler<(), Error>>>,
    /// デコード結果を受け取るチャネルの受信側
    results: mpsc::Receiver<DecodeResult>,
    /// ハンドラへ渡す送信側
    ///
    /// デコーダを (再) 構築するたびに複製してハンドラへ渡す。すべての送信側が破棄されると
    /// チャネルが切断され、受信側で結果を待てなくなるため本構造体でも保持する。
    results_tx: mpsc::Sender<DecodeResult>,
    last_config: Vec<u8>,
}

impl H264Decoder {
    /// H.264 デコーダを作成する
    ///
    /// 実際のデコーダは avcC が渡るまで初期化しない。
    pub fn new() -> Result<Self> {
        let (results_tx, results) = mpsc::channel();
        Ok(Self {
            decoder: None,
            results,
            results_tx,
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
        self.last_config = config.to_vec();
        Ok(())
    }
}

impl H264Decoder {
    /// 1 サンプル分のエンコード済みペイロードをデコードする
    ///
    /// avcC がまだ渡っていない場合は [`Error::NotInitialized`] を返す。
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
            // avcC 未到達ではデコーダを初期化できない
            return Err(Error::NotInitialized {
                codec: "H.264",
                record: "avcC",
            });
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
            }))
        }
        Ok(DecodedFrame::Nv12 { .. }) => Err(Error::Other(
            "H.264 decoder returned NV12 but I420 was requested".to_string(),
        )),
        Err(e) => Err(e),
    }
}

/// avcC から取り出した SPS / PPS と NAL 長フィールド幅
struct AvcConfig {
    /// SPS (Sequence Parameter Set: シーケンスパラメータセット)
    sps: Vec<u8>,
    /// PPS (Picture Parameter Set: ピクチャパラメータセット)
    pps: Vec<u8>,
    /// NAL 長フィールドのバイト数
    nalu_len_bytes: u32,
}

/// ISO/IEC 14496-15 §5.2.4.1.1 `AVCDecoderConfigurationRecord` をパースする
///
/// 最初の SPS / PPS のみ取り出す (複数 SPS / PPS は先頭のみ採用する)。
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
    for _ in 1..num_sps {
        skip_ps_nal(buf, &mut pos, "AVCDecoderConfigurationRecord")?;
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
        skip_ps_nal(buf, &mut pos, "AVCDecoderConfigurationRecord")?;
    }
    Ok(AvcConfig {
        sps,
        pps,
        nalu_len_bytes,
    })
}
