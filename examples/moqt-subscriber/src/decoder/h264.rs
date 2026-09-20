//! Apple Video Toolbox による H.264 デコーダ実装
//!
//! macOS 専用。`shiguredo_video_toolbox` を用いた AVC (H.264) デコードを提供する。

use shiguredo_video_toolbox::{DecodedFrame, Decoder, DecoderCodec, DecoderConfig, PixelFormat};

use crate::decoder::{DecodedVideoFrame, copy_plane, read_ps_nal};
use crate::error::{Error, Result};

/// ISO/IEC 14496-15 §5.2.4.1.1 AVCDecoderConfigurationRecord 最小サイズ (lengthSizeMinusOne まで)
const AVC_CONFIG_MIN_LEN: usize = 7;

/// 最初のキーフレーム到達までデコーダを遅延初期化する H.264 デコーダ
pub struct H264Decoder {
    decoder: Option<Decoder>,
    nalu_len_bytes: u32,
    last_config: Vec<u8>,
}

impl H264Decoder {
    pub fn new() -> Result<Self> {
        Ok(Self {
            decoder: None,
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
                self.decoder = Some(Decoder::new(cfg)?);
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
        match decoder.decode(payload)? {
            Some(DecodedFrame::I420(frame)) => {
                let w = frame.width();
                let h = frame.height();
                if frame.y_plane().is_empty() {
                    return Ok(Vec::new());
                }
                let y = copy_plane(frame.y_plane(), frame.y_stride(), w, h);
                let u = copy_plane(frame.u_plane(), frame.u_stride(), w / 2, h / 2);
                let v = copy_plane(frame.v_plane(), frame.v_stride(), w / 2, h / 2);
                Ok(vec![DecodedVideoFrame {
                    y,
                    u,
                    v,
                    width: w as i32,
                    height: h as i32,
                }])
            }
            Some(DecodedFrame::Nv12(_)) => Err(Error::Other(
                "H.264 decoder returned NV12 but I420 was requested".to_string(),
            )),
            None => Ok(Vec::new()),
        }
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
