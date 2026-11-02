//! Apple Video Toolbox による H.265 (HEVC) デコーダ実装
//!
//! macOS 専用。`shiguredo_video_toolbox` を用いた HEVC (hvc1 / hev1) デコードを提供する。
//! 入力は MP4 のサンプル (length-prefixed NAL) で、hvcC のレコード本体を設定として渡す。

use std::sync::mpsc;

use shiguredo_video_toolbox::{
    DecodedFrame, Decoder, DecoderCodec, DecoderConfig, FnDecodeHandler, PixelFormat,
};

use crate::decoder::{DecodedVideoFrame, copy_plane, read_ps_nal};
use crate::error::{Error, Result};

/// ISO/IEC 14496-15 §8.3.3.1.2 `HEVCDecoderConfigurationRecord` 固定部サイズ
const HVC_CONFIG_MIN_LEN: usize = 23;

/// HEVC NAL ユニット種別 (ITU-T H.265 §7.4.2.2)
const HEVC_NAL_VPS: u8 = 32;
const HEVC_NAL_SPS: u8 = 33;
const HEVC_NAL_PPS: u8 = 34;

/// デコード結果を運ぶチャネルのメッセージ
///
/// Video Toolbox はデコード結果をコールバックスレッドから通知するため、チャネルを経由して
/// `decode()` を呼び出したスレッドへ運ぶ。`Ok(None)` は「デコードは完了したがピクセルが
/// 得られなかった」を意味する。
type DecodeResult = std::result::Result<Option<DecodedVideoFrame>, Error>;

/// hvcC の到着時に初期化する H.265 デコーダ
pub struct H265Decoder {
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

impl H265Decoder {
    /// H.265 デコーダを作成する
    pub fn new() -> Result<Self> {
        let (results_tx, results) = mpsc::channel();
        Ok(Self {
            decoder: None,
            results,
            results_tx,
            last_config: Vec::new(),
        })
    }

    /// hvcC から VPS / SPS / PPS / `nalu_len_bytes` を取り出してデコーダを (再) 構築する
    fn apply_config(&mut self, config: &[u8]) -> Result<()> {
        let HvcConfig {
            vps,
            sps,
            pps,
            nalu_len_bytes,
        } = parse_hvc_config(config)?;
        let decoder_codec = DecoderCodec::Hevc {
            vps: &vps,
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

impl H265Decoder {
    /// 1 サンプル分のエンコード済みペイロードをデコードする
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
            return Err(Error::Other(
                "H.265 decoder is not initialized; the hvcC record is required".to_string(),
            ));
        };
        decoder.decode(payload, ())?;
        // Video Toolbox はデコード完了をコールバックスレッドから通知し、1 つの入力について
        // 必ず 1 回通知する。対応する結果が届くまで待ってから返すことで、同期デコードで
        // 結果を直接返していた従来と同じく、入力 1 件につきその結果を返す。
        let result = self
            .results
            .recv()
            .map_err(|_| Error::Other("H.265 decode result channel is closed".to_string()))?;
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
            "H.265 decoder returned NV12 but I420 was requested".to_string(),
        )),
        Err(e) => Err(e),
    }
}

struct HvcConfig {
    vps: Vec<u8>,
    sps: Vec<u8>,
    pps: Vec<u8>,
    nalu_len_bytes: u32,
}

/// ISO/IEC 14496-15 §8.3.3.1.2 `HEVCDecoderConfigurationRecord` をパースする
///
/// VPS / SPS / PPS はそれぞれ先頭の 1 個を採用する。
fn parse_hvc_config(buf: &[u8]) -> Result<HvcConfig> {
    if buf.len() < HVC_CONFIG_MIN_LEN {
        return Err(Error::Other(format!(
            "HEVCDecoderConfigurationRecord too short: {}",
            buf.len()
        )));
    }
    if buf[0] != 1 {
        return Err(Error::Other(format!(
            "HEVCDecoderConfigurationRecord: unsupported configurationVersion {}",
            buf[0]
        )));
    }
    let nalu_len_bytes = u32::from((buf[21] & 0x03) + 1);
    let num_arrays = buf[22] as usize;

    let mut pos = 23;
    let mut vps = None;
    let mut sps = None;
    let mut pps = None;

    for _ in 0..num_arrays {
        if buf.len() < pos + 3 {
            return Err(Error::Other(
                "HEVCDecoderConfigurationRecord: truncated array header".to_string(),
            ));
        }
        let nal_type = buf[pos] & 0x3F;
        let num_nals = u16::from_be_bytes([buf[pos + 1], buf[pos + 2]]) as usize;
        pos += 3;
        for i in 0..num_nals {
            let nal = read_ps_nal(buf, &mut pos, "HEVCDecoderConfigurationRecord")?;
            if i == 0 {
                match nal_type {
                    HEVC_NAL_VPS if vps.is_none() => vps = Some(nal),
                    HEVC_NAL_SPS if sps.is_none() => sps = Some(nal),
                    HEVC_NAL_PPS if pps.is_none() => pps = Some(nal),
                    _ => {}
                }
            }
        }
    }

    let vps = vps.ok_or_else(|| Error::Other("HEVC config missing VPS".to_string()))?;
    let sps = sps.ok_or_else(|| Error::Other("HEVC config missing SPS".to_string()))?;
    let pps = pps.ok_or_else(|| Error::Other("HEVC config missing PPS".to_string()))?;

    Ok(HvcConfig {
        vps,
        sps,
        pps,
        nalu_len_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::h265::H265Encoder;

    /// エンコードした H.265 のフレームをデコードできること
    ///
    /// デコーダの入力は publisher が組み立てた hvcC と length-prefixed NAL のサンプルである。
    /// 実際の Video Toolbox でエンコードしたフレームをそのまま入力に使い、入力 1 件につき
    /// 1 フレームがデコードされることと、プレーンが stride 除去済みの I420 で返ることを
    /// 確認する。Video Toolbox は出力を遅延させるため、出力が届くまで 30 フレーム投入する。
    #[test]
    fn encoded_frames_are_decoded() {
        let mut encoder =
            H265Encoder::new(64, 48, 30, 500, 60).expect("H.265 エンコーダを作成できること");
        let frame = crate::encoder::test_frame(64, 48);
        let mut encoded_frames = Vec::new();
        let mut video_config = None;
        for index in 0..30u64 {
            for encoded in encoder
                .encode(&frame, index * 33_333, index == 0)
                .expect("H.265 のエンコードに成功すること")
            {
                if let Some(config) = &encoded.video_config {
                    video_config = Some(config.clone());
                }
                encoded_frames.push(encoded);
            }
        }
        let video_config = video_config.expect("キーフレームの hvcC が得られること");
        assert!(
            encoded_frames.len() >= 2,
            "エンコード結果が 2 件以上あること"
        );

        let mut decoder = H265Decoder::new().expect("H.265 デコーダを作成できること");
        let mut decoded_frames = Vec::new();
        for encoded in &encoded_frames {
            decoded_frames.extend(
                decoder
                    .decode(&encoded.data, Some(&video_config))
                    .expect("H.265 のデコードに成功すること"),
            );
        }
        assert_eq!(
            decoded_frames.len(),
            encoded_frames.len(),
            "入力 1 件につき 1 フレームがデコードされること"
        );
        for decoded in &decoded_frames {
            assert_eq!(
                (decoded.width, decoded.height),
                (64, 48),
                "映像サイズが入力と同じであること"
            );
            assert_eq!(
                decoded.y.len(),
                64 * 48,
                "Y プレーンが stride 除去済みであること"
            );
            assert_eq!(
                decoded.u.len(),
                32 * 24,
                "U プレーンが stride 除去済みであること"
            );
            assert_eq!(
                decoded.v.len(),
                32 * 24,
                "V プレーンが stride 除去済みであること"
            );
        }
    }
}
