//! Apple Video Toolbox による H.265 (HEVC) デコーダ実装
//!
//! macOS 専用。`shiguredo_video_toolbox` を用いた HEVC (hvc1 / hev1) デコードを提供する。

use shiguredo_video_toolbox::{DecodedFrame, Decoder, DecoderCodec, DecoderConfig, PixelFormat};

use crate::decoder::{DecodedVideoFrame, copy_plane, read_ps_nal};
use crate::error::{Error, Result};

/// ISO/IEC 14496-15 §8.3.3.1.2 `HEVCDecoderConfigurationRecord` 固定部サイズ
const HVC_CONFIG_MIN_LEN: usize = 23;

/// HEVC NAL ユニット種別 (ITU-T H.265 §7.4.2.2)
const HEVC_NAL_VPS: u8 = 32;
const HEVC_NAL_SPS: u8 = 33;
const HEVC_NAL_PPS: u8 = 34;

/// 最初のキーフレーム到達までデコーダを遅延初期化する H.265 デコーダ
pub struct H265Decoder {
    decoder: Option<Decoder>,
    nalu_len_bytes: u32,
    last_config: Vec<u8>,
}

impl H265Decoder {
    pub fn new() -> Result<Self> {
        Ok(Self {
            decoder: None,
            nalu_len_bytes: 4,
            last_config: Vec::new(),
        })
    }

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
                self.decoder = Some(Decoder::new(cfg)?);
            }
        }
        self.nalu_len_bytes = nalu_len_bytes;
        self.last_config = config.to_vec();
        Ok(())
    }
}

impl H265Decoder {
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
            tracing::warn!("H.265 decoder not initialized yet (waiting for hvcC)");
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
                "H.265 decoder returned NV12 but I420 was requested".to_string(),
            )),
            None => Ok(Vec::new()),
        }
    }
}

struct HvcConfig {
    vps: Vec<u8>,
    sps: Vec<u8>,
    pps: Vec<u8>,
    nalu_len_bytes: u32,
}

/// ISO/IEC 14496-15 §8.3.3.1.2 `HEVCDecoderConfigurationRecord` をパースする
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
