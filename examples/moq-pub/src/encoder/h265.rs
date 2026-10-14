//! Apple Video Toolbox による H.265 (HEVC) エンコーダ実装
//!
//! macOS 専用。`shiguredo_video_toolbox` を利用しハードウェアアクセラレーションで
//! HEVC (hvc1) をエンコードする。

use std::num::NonZeroU32;
use std::sync::mpsc;

use shiguredo_video_device::VideoFrameOwned;
use shiguredo_video_toolbox::{
    CodecConfig, EncodeOptions, Encoder, EncoderConfig, FnEncodeHandler, FrameData,
    HevcEncoderConfig, HevcProfile, PictureType, PixelFormat,
};

use super::EncodedFrame;
use crate::error::{Error, Result};

/// Video Toolbox のコールバックが返すエンコード結果
///
/// このモジュールの [`EncodedFrame`] とは別の型である。ハンドラは Video Toolbox の
/// コールバックスレッドから呼ばれるため、結果はチャネルを経由して運ぶ。
/// `()` は `Encoder::encode` へ渡すユーザーデータである。
type ToolboxEncodeResult = std::result::Result<shiguredo_video_toolbox::EncodedFrame<()>, Error>;

/// 既定の HEVC catalog codec 文字列 (Main@L4.0, Apple 典型値)
///
/// `H265Encoder::new` 時点では SPS が得られないため Main@L4.0 相当を仮置きし、
/// 最初のキーフレームで SPS の profile_tier_level から `hvc1.PPP.CCCCCCCC.LLL.CC` 形式に置き換える。
const DEFAULT_HEVC_CATALOG_CODEC_STRING: &str = "hvc1.1.6.L120.B0";

/// HEVC NAL の長さプレフィックスサイズ (HVCC 4 バイト前提)
///
/// 根拠: ISO/IEC 14496-15 §8.3.3.1.2 `lengthSizeMinusOne`
/// (将来の改訂で値が変更される可能性あり)
const HVCC_LENGTH_SIZE: u8 = 4;

/// HEVC NAL unit type (VPS / SPS / PPS)
///
/// 根拠: ITU-T H.265 / ISO/IEC 23008-2 §7.4.2.2 Table 7-1
/// (将来の改訂で値が変更される可能性あり)
const HEVC_NAL_VPS: u8 = 32;
const HEVC_NAL_SPS: u8 = 33;
const HEVC_NAL_PPS: u8 = 34;

/// H.265 (HEVC) エンコーダ (Apple Video Toolbox)
pub struct H265Encoder {
    encoder: Encoder<FnEncodeHandler<(), Error>>,
    /// エンコード結果を受け取るチャネルの受信側
    ///
    /// Video Toolbox はエンコード結果をコールバックスレッドから通知するため、
    /// [`H265Encoder::encode`] の呼び出し時に溜まっている分を取り出す。
    results: mpsc::Receiver<ToolboxEncodeResult>,
    /// 1 フレームの表示間隔 (マイクロ秒)
    ///
    /// 1 つの入力から複数の出力フレームが出たときに Timestamp を進める量。
    frame_interval_us: u64,
    /// MSF catalog に載せる codec 文字列
    ///
    /// `new` 時点では既定値。最初のキーフレーム到来時に SPS から再構築する。
    catalog_codec_string: String,
}

impl H265Encoder {
    /// H.265 エンコーダを作成する
    ///
    /// `keyframe_interval_frames` はキーフレーム間隔のフレーム数上限であり、時間ベースの
    /// キーフレーム要求が効かない場合の安全弁として使う。1 以上であること。
    pub fn new(
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        keyframe_interval_frames: u32,
    ) -> Result<Self> {
        if fps == 0 {
            return Err(Error::Other(
                "fps must be non-zero to compute the frame interval".to_string(),
            ));
        }
        // 0 をエンコーダへ渡すと上限無しとして扱われる実装がある
        // (`NonZeroU32` / `NonZeroUsize`)。安全弁が黙って消えないよう拒否する
        if keyframe_interval_frames == 0 {
            return Err(Error::Other(
                "keyframe interval must be at least one frame".to_string(),
            ));
        }
        let config = EncoderConfig {
            width,
            height,
            codec: CodecConfig::Hevc(HevcEncoderConfig {
                profile: Some(HevcProfile::Main),
                allow_open_gop: Some(false),
            }),
            pixel_format: PixelFormat::Nv12,
            average_bitrate: Some(bitrate as u64 * 1000),
            fps_numerator: fps,
            fps_denominator: 1,
            prioritize_encoding_speed_over_quality: Some(false),
            real_time: Some(true),
            maximize_power_efficiency: Some(false),
            allow_frame_reordering: Some(false),
            allow_temporal_compression: Some(true),
            max_key_frame_interval: NonZeroU32::new(keyframe_interval_frames),
            max_key_frame_interval_duration: None,
            max_frame_delay_count: None,
            data_rate_limits: Vec::new(),
        };

        // ハンドラは Video Toolbox のコールバックスレッドから呼ばれるため、
        // 結果はチャネルへ送り、encode() を呼び出したスレッドで取り出す
        let (results_tx, results) = mpsc::channel();
        let encoder = Encoder::new(
            config,
            FnEncodeHandler::new(move |result: ToolboxEncodeResult| {
                // 受信側が破棄された後 (エンコーダ破棄時) の通知は捨てる
                let _ = results_tx.send(result);
            }),
        )?;
        let frame_interval_us = 1_000_000 / u64::from(fps);

        tracing::info!(
            "H.265 encoder created ({}x{}, {} fps, {} kbps, keyframe_interval_frames={}, frame_interval_us={})",
            width,
            height,
            fps,
            bitrate,
            keyframe_interval_frames,
            frame_interval_us
        );

        Ok(Self {
            encoder,
            results,
            frame_interval_us,
            catalog_codec_string: DEFAULT_HEVC_CATALOG_CODEC_STRING.to_string(),
        })
    }
}

impl H265Encoder {
    /// 1 フレームをエンコードして 0 個以上のエンコード済みフレームを返す
    ///
    /// `timestamp_us` は入力フレームのメディア時刻 (マイクロ秒)。先頭の出力フレームへ
    /// そのまま載せ、複数の出力フレームがある場合は 1 フレームずつ進める。
    ///
    /// `force_keyframe` が真なら、このフレームをキーフレームとしてエンコードする。
    pub fn encode(
        &mut self,
        frame: &VideoFrameOwned,
        timestamp_us: u64,
        force_keyframe: bool,
    ) -> Result<Vec<EncodedFrame>> {
        let data = FrameData::Nv12 {
            y: &frame.data,
            uv: frame.uv_data.as_deref().unwrap_or(&[]),
        };

        let options = EncodeOptions {
            force_key_frame: force_keyframe,
        };

        self.encoder.encode(&data, &options, ())?;

        self.drain_encoded_frames(timestamp_us)
    }

    /// ハンドラが受け取ったエンコード結果のうち、今取り出せる分を [`EncodedFrame`] へ詰め替える
    ///
    /// Video Toolbox はエンコード結果をコールバックスレッドから通知するため、`encode()` の
    /// 呼び出し時点で届いていない結果は次の呼び出し以降で取り出す。これは、内部で
    /// `try_recv()` する非ブロッキングな取り出しを繰り返していた従来の挙動と同じである。
    fn drain_encoded_frames(&mut self, timestamp_us: u64) -> Result<Vec<EncodedFrame>> {
        let mut frames = Vec::new();
        let mut output_index: u64 = 0;
        while let Ok(result) = self.results.try_recv() {
            let encoded = result?;
            let timestamp =
                super::output_timestamp_us(timestamp_us, output_index, self.frame_interval_us);
            // キーフレームかどうかはピクチャータイプで判定する
            let is_keyframe = encoded.picture_type == PictureType::I;
            let video_config = if is_keyframe {
                if encoded.vps_list.is_empty() {
                    return Err(Error::Other("HEVC keyframe is missing VPS".to_string()));
                }
                if encoded.sps_list.is_empty() {
                    return Err(Error::Other("HEVC keyframe is missing SPS".to_string()));
                }
                if encoded.pps_list.is_empty() {
                    return Err(Error::Other("HEVC keyframe is missing PPS".to_string()));
                }
                if let Some(cs) = build_hvc_catalog_codec_string(&encoded.sps_list[0]) {
                    self.catalog_codec_string = cs;
                }
                Some(build_hvc_decoder_config_record(
                    &encoded.vps_list,
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
            output_index += 1;
        }

        Ok(frames)
    }

    /// MSF catalog に載せる RFC 6381 形式の codec 文字列
    pub fn catalog_codec_string(&self) -> &str {
        &self.catalog_codec_string
    }
}

/// HEVC SPS から profile_tier_level を取り出し `hvc1.PPP.CCCCCCCC.LLL.CC` を構築する
///
/// 根拠 (将来の改訂で SPS / 文字列形式が変更される可能性あり):
///
/// - ITU-T H.265 / ISO/IEC 23008-2 §7.3.2.2.1 Sequence parameter set RBSP syntax
/// - ISO/IEC 14496-15 Annex E.3 / RFC 7798 §7.1
///
/// SPS レイアウト (先頭から):
/// - NAL header (2 bytes)
/// - sps_video_parameter_set_id(4) | sps_max_sub_layers_minus1(3) | sps_temporal_id_nesting_flag(1) = 1 byte
/// - profile_tier_level 一般部 (12 bytes): 総計 96 bit
///   - general_profile_space(2) | general_tier_flag(1) | general_profile_idc(5) = 1 byte
///   - general_profile_compatibility_flag (32 bits) = 4 bytes
///   - general_constraint_indicator_flags (48 bits) = 6 bytes
///   - general_level_idc (8 bits) = 1 byte
fn build_hvc_catalog_codec_string(sps: &[u8]) -> Option<String> {
    if sps.len() < 15 {
        return None;
    }
    let ptl_byte0 = sps[3];
    let profile_space = (ptl_byte0 >> 6) & 0x03;
    let tier_flag = (ptl_byte0 >> 5) & 0x01;
    let profile_idc = ptl_byte0 & 0x1F;
    let compat_u32 = u32::from_be_bytes([sps[4], sps[5], sps[6], sps[7]]);
    let constraint_bytes: [u8; 6] = sps[8..14].try_into().ok()?;
    let level_idc = sps[14];

    let profile_part = if profile_space == 0 {
        format!("{profile_idc}")
    } else {
        let letter = (b'A' + profile_space - 1) as char;
        format!("{letter}{profile_idc}")
    };

    let compat_hex = format!("{compat_u32:08X}");
    let compat_trimmed = compat_hex.trim_end_matches('0');
    let compat_part = if compat_trimmed.is_empty() {
        "0".to_string()
    } else {
        compat_trimmed.to_string()
    };

    let tier_letter = if tier_flag == 1 { 'H' } else { 'L' };
    let level_part = format!("{tier_letter}{level_idc}");

    let mut constraint_parts: Vec<String> = constraint_bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect();
    while matches!(constraint_parts.last(), Some(s) if s == "00") {
        constraint_parts.pop();
    }
    let constraint_part = if constraint_parts.is_empty() {
        "0".to_string()
    } else {
        constraint_parts.join(".")
    };

    Some(format!(
        "hvc1.{profile_part}.{compat_part}.{level_part}.{constraint_part}"
    ))
}

/// VPS / SPS / PPS から HEVCDecoderConfigurationRecord を組み立てる
///
/// 根拠: ISO/IEC 14496-15 §8.3.3.1.2
/// (将来の改訂でフィールド構成が変更される可能性あり)
///
/// 入力が NV12 (4:2:0 8-bit) 固定のため chroma_format_idc=1 /
/// bit_depth_luma_minus8=0 / bit_depth_chroma_minus8=0 を固定で埋める。
fn build_hvc_decoder_config_record(
    vps_list: &[Vec<u8>],
    sps_list: &[Vec<u8>],
    pps_list: &[Vec<u8>],
) -> Result<Vec<u8>> {
    let sps = sps_list.first().ok_or_else(|| {
        Error::Other("HEVCDecoderConfigurationRecord requires at least one SPS".to_string())
    })?;
    if sps.len() < 15 {
        return Err(Error::Other(
            "HEVC SPS too short to read profile_tier_level".to_string(),
        ));
    }
    for nal in vps_list
        .iter()
        .chain(sps_list.iter())
        .chain(pps_list.iter())
    {
        if nal.len() > u16::MAX as usize {
            return Err(Error::Other(format!(
                "HEVC parameter set NAL too long ({} > 65535)",
                nal.len()
            )));
        }
    }
    for list in [vps_list, sps_list, pps_list] {
        if list.len() > u16::MAX as usize {
            return Err(Error::Other(format!(
                "too many HEVC parameter set NAL units ({}, max 65535)",
                list.len()
            )));
        }
    }

    let ptl = &sps[3..15]; // 12 bytes: profile_tier_level 一般部
    let general_profile_space_tier_flag_profile_idc = ptl[0];
    let general_profile_compatibility_flags: [u8; 4] = ptl[1..5]
        .try_into()
        .expect("profile_tier_level must contain 4 compatibility flag bytes");
    let general_constraint_indicator_flags: [u8; 6] = ptl[5..11]
        .try_into()
        .expect("profile_tier_level must contain 6 constraint indicator bytes");
    let general_level_idc = ptl[11];

    let mut buf = Vec::with_capacity(128);
    buf.push(1); // configurationVersion
    buf.push(general_profile_space_tier_flag_profile_idc);
    buf.extend_from_slice(&general_profile_compatibility_flags);
    buf.extend_from_slice(&general_constraint_indicator_flags);
    buf.push(general_level_idc);
    // reserved(4='1111') | min_spatial_segmentation_idc(12=0)
    buf.extend_from_slice(&[0xF0, 0x00]);
    // reserved(6='111111') | parallelismType(2=0 unknown)
    buf.push(0xFC);
    // reserved(6='111111') | chroma_format_idc(2=1 for 4:2:0)
    buf.push(0xFC | 0x01);
    // reserved(5='11111') | bit_depth_luma_minus8(3=0 for 8-bit)
    buf.push(0xF8);
    // reserved(5='11111') | bit_depth_chroma_minus8(3=0 for 8-bit)
    buf.push(0xF8);
    // avgFrameRate(16=0 unspecified)
    buf.extend_from_slice(&[0x00, 0x00]);
    // constantFrameRate(2=0) | numTemporalLayers(3=1) | temporalIdNested(1=0) | lengthSizeMinusOne(2)
    buf.push((1 << 3) | (HVCC_LENGTH_SIZE - 1));
    // numOfArrays
    buf.push(3);

    for (nal_type, list) in [
        (HEVC_NAL_VPS, vps_list),
        (HEVC_NAL_SPS, sps_list),
        (HEVC_NAL_PPS, pps_list),
    ] {
        // array_completeness(1=1) | reserved(1=0) | NAL_unit_type(6)
        buf.push(0x80 | nal_type);
        buf.extend_from_slice(&(list.len() as u16).to_be_bytes());
        for nal in list {
            buf.extend_from_slice(&(nal.len() as u16).to_be_bytes());
            buf.extend_from_slice(nal);
        }
    }

    Ok(buf)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// fps が 0 の場合はエラーになること
    ///
    /// 依存クレートも fps 0 を拒否するため、`is_err()` だけではこちらの検証が働いているかを
    /// 区別できない。ローカルの検証が返すエラーであることを確かめる。
    #[test]
    fn zero_fps_is_rejected() {
        let Err(error) = H265Encoder::new(64, 48, 0, 500, 60) else {
            panic!("fps が 0 の場合はエラーになること");
        };
        assert!(
            matches!(error, Error::Other(_)),
            "ローカルの検証で拒否すること: {error:?}"
        );
    }

    /// キーフレーム間隔が 0 の場合はエラーになること
    ///
    /// 0 を上限として渡すとエンコーダによっては上限無しとして扱われ、安全弁が消える。
    /// 実際に NV12 入力をエンコードできることは
    /// [`tests::forced_frame_is_encoded_as_a_keyframe`] で確認する。
    #[test]
    fn zero_keyframe_interval_is_rejected() {
        assert!(
            H265Encoder::new(64, 48, 30, 500, 0).is_err(),
            "キーフレーム間隔が 0 の場合はエラーになること"
        );
    }

    /// キーフレームを強制したフレームがキーフレームとして出力されること
    ///
    /// Group 長を時間ベースで守るには、任意のフレームでキーフレームを要求できる必要がある。
    /// Video Toolbox の出力は入力より遅れるため、強制した入力の出力は数フレーム後になる。
    /// キーフレーム間隔の上限を 600 フレームにして、先頭フレーム以外でキーフレームが
    /// 出るのが強制した 6 番目の入力の出力だけであることを固定する。
    /// NV12 入力のエンコードは依存クレートの 2026.1.1 では必ず失敗していたが、
    /// 現在の canary では成功する。
    #[test]
    fn forced_frame_is_encoded_as_a_keyframe() {
        const FRAME_COUNT: usize = 12;
        const FORCED_INDEX: usize = 5;

        let mut encoder =
            H265Encoder::new(64, 48, 30, 500, 600).expect("H.265 エンコーダを作成できること");
        let frame = crate::encoder::test_frame(64, 48);
        let mut frames = Vec::new();
        for index in 0..FRAME_COUNT {
            let encoded = encoder
                .encode(&frame, index as u64 * 33_333, index == FORCED_INDEX)
                .expect("H.265 のエンコードに成功すること");
            frames.extend(encoded);
        }
        // Video Toolbox が保持している出力をフラッシュして受け取る
        encoder
            .encoder
            .finish()
            .expect("H.265 エンコーダをフラッシュできること");
        frames.extend(collect_frames(&mut encoder, FRAME_COUNT - frames.len()));

        assert!(
            frames[0].is_keyframe,
            "先頭フレームがキーフレームであること"
        );
        assert!(
            frames[1..FORCED_INDEX]
                .iter()
                .all(|frame| !frame.is_keyframe),
            "強制しなければキーフレームが出ないこと: {:?}",
            frame_types(&frames)
        );
        assert!(
            frames[FORCED_INDEX].is_keyframe,
            "強制したフレームがキーフレームとして出力されること: {:?}",
            frame_types(&frames)
        );
        assert!(
            !frames[FORCED_INDEX + 1].is_keyframe,
            "強制した次のフレームはキーフレームにならないこと: {:?}",
            frame_types(&frames)
        );
    }

    /// 期限までに届いたエンコード結果を集める
    ///
    /// Video Toolbox はエンコード結果をコールバックスレッドから通知するため、
    /// フラッシュの直後でも結果が届いていないことがある。
    fn collect_frames(encoder: &mut H265Encoder, expected: usize) -> Vec<EncodedFrame> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut frames = Vec::new();
        while frames.len() < expected && Instant::now() < deadline {
            frames.extend(
                encoder
                    .drain_encoded_frames(0)
                    .expect("エンコード結果を取り出せること"),
            );
            if frames.len() < expected {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        assert_eq!(
            frames.len(),
            expected,
            "エンコード結果が {expected} 件届くこと"
        );
        frames
    }

    /// テストの失敗時にキーフレームの並びを分かるようにする
    fn frame_types(frames: &[EncodedFrame]) -> Vec<bool> {
        frames.iter().map(|frame| frame.is_keyframe).collect()
    }
}
