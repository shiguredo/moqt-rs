//! Apple Video Toolbox による H.264 エンコーダ実装
//!
//! macOS 専用。`shiguredo_video_toolbox` を利用しハードウェアアクセラレーションで
//! H.264 (avc1) をエンコードする。

use std::num::NonZeroU32;

use shiguredo_video_device::VideoFrameOwned;
use shiguredo_video_toolbox::{
    CodecConfig, EncodeOptions, Encoder, EncoderConfig, FrameData, H264EncoderConfig,
    H264EntropyMode, H264Profile, PixelFormat,
};

use super::EncodedFrame;
use crate::error::{Error, Result};

/// 既定の H.264 catalog codec 文字列 (High@L4.0, 4:2:0 8-bit)
///
/// H264Encoder::new 時点では SPS が得られないため High@L4.0 相当を仮置きし、
/// 最初のキーフレームで SPS 先頭 3 バイトから `avc1.PPCCLL` 形式に置き換える。
const DEFAULT_AVC_CATALOG_CODEC_STRING: &str = "avc1.640028";

/// H.264 NAL の長さプレフィックスサイズ (AVCC 4 バイト前提)
///
/// 根拠: ISO/IEC 14496-15 §5.2.4.1.1 `lengthSizeMinusOne`
/// (将来の改訂で値が変更される可能性あり)
const AVCC_LENGTH_SIZE: u8 = 4;

/// H.264 エンコーダ (Apple Video Toolbox)
pub struct H264Encoder {
    encoder: Encoder,
    /// タイムスケール (fps * 1000)
    timescale: u64,
    /// フレームカウンタ
    frame_count: u64,
    /// FPS
    fps: u32,
    /// MSF catalog に載せる codec 文字列
    ///
    /// `new` 時点では既定値。最初のキーフレーム到来時に SPS から再構築する。
    catalog_codec_string: String,
}

impl H264Encoder {
    /// H.264 エンコーダを作成する
    pub fn new(
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        keyframe_interval: u32,
    ) -> Result<Self> {
        let config = EncoderConfig {
            width,
            height,
            codec: CodecConfig::H264(H264EncoderConfig {
                profile: H264Profile::High,
                entropy_mode: H264EntropyMode::Cabac,
            }),
            pixel_format: PixelFormat::Nv12,
            average_bitrate: Some(bitrate as u64 * 1000),
            fps_numerator: fps,
            fps_denominator: 1,
            prioritize_encoding_speed_over_quality: false,
            real_time: true,
            maximize_power_efficiency: false,
            allow_frame_reordering: false,
            allow_temporal_compression: true,
            max_key_frame_interval: NonZeroU32::new(keyframe_interval),
            max_key_frame_interval_duration: None,
            max_frame_delay_count: None,
        };

        let encoder = Encoder::new(config)?;
        let timescale = fps as u64 * 1000;

        tracing::info!(
            "H.264 encoder created ({}x{}, {} fps, {} kbps, keyframe_interval={}, timescale={})",
            width,
            height,
            fps,
            bitrate,
            keyframe_interval,
            timescale
        );

        Ok(Self {
            encoder,
            timescale,
            frame_count: 0,
            fps,
            catalog_codec_string: DEFAULT_AVC_CATALOG_CODEC_STRING.to_string(),
        })
    }
}

impl H264Encoder {
    /// 1 フレームをエンコードして 0 個以上のエンコード済みフレームを返す
    pub fn encode(&mut self, frame: &VideoFrameOwned) -> Result<Vec<EncodedFrame>> {
        let data = FrameData::Nv12 {
            y: &frame.data,
            uv: frame.uv_data.as_deref().unwrap_or(&[]),
        };

        let options = EncodeOptions {
            force_key_frame: false,
        };

        self.encoder.encode(&data, &options)?;

        let mut frames = Vec::new();
        while let Some(encoded) = self.encoder.next_frame()? {
            let timestamp = self.frame_count * self.timescale / self.fps as u64;
            let is_keyframe = encoded.keyframe;
            let video_config = if is_keyframe {
                if encoded.sps_list.is_empty() {
                    return Err(Error::Other("H.264 keyframe is missing SPS".to_string()));
                }
                if encoded.pps_list.is_empty() {
                    return Err(Error::Other("H.264 keyframe is missing PPS".to_string()));
                }
                if let Some(cs) = build_avc_catalog_codec_string(&encoded.sps_list[0]) {
                    self.catalog_codec_string = cs;
                }
                Some(build_avc_decoder_config_record(
                    &encoded.sps_list,
                    &encoded.pps_list,
                )?)
            } else {
                None
            };
            frames.push(EncodedFrame {
                data: encoded.data,
                is_keyframe,
                timestamp,
                video_config,
            });
            self.frame_count += 1;
        }

        Ok(frames)
    }

    /// 1 秒あたりの timestamp 単位数
    pub fn timescale(&self) -> u64 {
        self.timescale
    }

    /// MSF catalog に載せる RFC 6381 形式の codec 文字列
    pub fn catalog_codec_string(&self) -> &str {
        &self.catalog_codec_string
    }
}

/// SPS 先頭 3 バイトから `avc1.PPCCLL` 形式の codec 文字列を構築する
///
/// 根拠: ISO/IEC 14496-10 §7.3.2.1 Sequence parameter set RBSP syntax
/// (NAL unit header 1 バイトを除いた body 先頭 3 バイトが
///  profile_idc / constraint_flags / level_idc)
/// (将来の改訂で SPS レイアウトが変更される可能性あり)
fn build_avc_catalog_codec_string(sps: &[u8]) -> Option<String> {
    if sps.len() < 4 {
        return None;
    }
    let profile_idc = sps[1];
    let profile_compatibility = sps[2];
    let level_idc = sps[3];
    Some(format!(
        "avc1.{profile_idc:02X}{profile_compatibility:02X}{level_idc:02X}"
    ))
}

/// SPS / PPS から AVCDecoderConfigurationRecord を組み立てる
///
/// 根拠: ISO/IEC 14496-15 §5.2.4.1.1
/// (将来の改訂でフィールド構成が変更される可能性あり)
///
/// 入力が NV12 (4:2:0 8-bit) 固定のため High profile 拡張フィールドは
/// chroma_format=1 / bit_depth_luma_minus8=0 / bit_depth_chroma_minus8=0 を固定で埋める。
fn build_avc_decoder_config_record(sps_list: &[Vec<u8>], pps_list: &[Vec<u8>]) -> Result<Vec<u8>> {
    let sps = sps_list.first().ok_or_else(|| {
        Error::Other("AVCDecoderConfigurationRecord requires at least one SPS".to_string())
    })?;
    if sps.len() < 4 {
        return Err(Error::Other(
            "H.264 SPS too short to read profile/level".to_string(),
        ));
    }
    if sps_list.len() > 0x1F {
        return Err(Error::Other(format!(
            "too many SPS NAL units ({}, max 31)",
            sps_list.len()
        )));
    }
    if pps_list.len() > 0xFF {
        return Err(Error::Other(format!(
            "too many PPS NAL units ({}, max 255)",
            pps_list.len()
        )));
    }
    for nal in sps_list.iter().chain(pps_list.iter()) {
        if nal.len() > u16::MAX as usize {
            return Err(Error::Other(format!(
                "H.264 parameter set NAL too long ({} > 65535)",
                nal.len()
            )));
        }
    }

    let profile_idc = sps[1];
    let profile_compatibility = sps[2];
    let level_idc = sps[3];

    let mut buf = Vec::with_capacity(64);
    buf.push(1); // configurationVersion
    buf.push(profile_idc); // AVCProfileIndication
    buf.push(profile_compatibility); // profile_compatibility
    buf.push(level_idc); // AVCLevelIndication
    // 6 bits reserved ('111111') | 2 bits lengthSizeMinusOne
    buf.push(0xFC | (AVCC_LENGTH_SIZE - 1));
    // 3 bits reserved ('111') | 5 bits numOfSequenceParameterSets
    buf.push(0xE0 | (sps_list.len() as u8));
    for sps in sps_list {
        buf.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        buf.extend_from_slice(sps);
    }
    buf.push(pps_list.len() as u8);
    for pps in pps_list {
        buf.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        buf.extend_from_slice(pps);
    }

    // High / High10 / High 4:2:2 / High 4:4:4 profile はチャンネル拡張フィールドを追加する
    // (§5.2.4.1.1 「if( profile_idc == 100 || 110 || 122 || 144 )」)
    if matches!(profile_idc, 100 | 110 | 122 | 144) {
        // 6 bits reserved ('111111') | 2 bits chroma_format (1 = 4:2:0)
        buf.push(0xFC | 0x01);
        // 5 bits reserved ('11111') | 3 bits bit_depth_luma_minus8 (0 = 8-bit)
        buf.push(0xF8);
        // 5 bits reserved ('11111') | 3 bits bit_depth_chroma_minus8 (0 = 8-bit)
        buf.push(0xF8);
        // numOfSequenceParameterSetExt = 0 (VT は SPS Extension を出さない)
        buf.push(0x00);
    }

    Ok(buf)
}
