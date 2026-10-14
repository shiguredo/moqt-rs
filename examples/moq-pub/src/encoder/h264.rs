//! Apple Video Toolbox による H.264 エンコーダ実装
//!
//! macOS 専用。`shiguredo_video_toolbox` を利用しハードウェアアクセラレーションで
//! H.264 (avc1) をエンコードする。

use std::num::NonZeroU32;
use std::sync::mpsc;

use shiguredo_video_device::VideoFrameOwned;
use shiguredo_video_toolbox::{
    CodecConfig, EncodeOptions, Encoder, EncoderConfig, FnEncodeHandler, FrameData,
    H264EncoderConfig, H264EntropyMode, H264Profile, PictureType, PixelFormat,
};

use super::EncodedFrame;
use crate::error::{Error, Result};

/// Video Toolbox のコールバックが返すエンコード結果
///
/// このモジュールの [`EncodedFrame`] とは別の型である。ハンドラは Video Toolbox の
/// コールバックスレッドから呼ばれるため、結果はチャネルを経由して運ぶ。
/// `()` は `Encoder::encode` へ渡すユーザーデータである。
type ToolboxEncodeResult = std::result::Result<shiguredo_video_toolbox::EncodedFrame<()>, Error>;

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
    encoder: Encoder<FnEncodeHandler<(), Error>>,
    /// エンコード結果を受け取るチャネルの受信側
    ///
    /// Video Toolbox はエンコード結果をコールバックスレッドから通知するため、
    /// [`H264Encoder::encode`] の呼び出し時に溜まっている分を取り出す。
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

impl H264Encoder {
    /// H.264 エンコーダを作成する
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
            codec: CodecConfig::H264(H264EncoderConfig {
                profile: Some(H264Profile::High),
                entropy_mode: Some(H264EntropyMode::Cabac),
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
            "H.264 encoder created ({}x{}, {} fps, {} kbps, keyframe_interval_frames={}, frame_interval_us={})",
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
            catalog_codec_string: DEFAULT_AVC_CATALOG_CODEC_STRING.to_string(),
        })
    }
}

impl H264Encoder {
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
            output_index += 1;
        }

        Ok(frames)
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
        let Err(error) = H264Encoder::new(64, 48, 0, 500, 60) else {
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
            H264Encoder::new(64, 48, 30, 500, 0).is_err(),
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
            H264Encoder::new(64, 48, 30, 500, 600).expect("H.264 エンコーダを作成できること");
        let frame = crate::encoder::test_frame(64, 48);
        let mut frames = Vec::new();
        for index in 0..FRAME_COUNT {
            let encoded = encoder
                .encode(&frame, index as u64 * 33_333, index == FORCED_INDEX)
                .expect("H.264 のエンコードに成功すること");
            frames.extend(encoded);
        }
        // Video Toolbox が保持している出力をフラッシュして受け取る
        encoder
            .encoder
            .finish()
            .expect("H.264 エンコーダをフラッシュできること");
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
    fn collect_frames(encoder: &mut H264Encoder, expected: usize) -> Vec<EncodedFrame> {
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
