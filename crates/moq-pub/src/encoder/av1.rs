//! shiguredo_aom (libaom) による AV1 エンコーダ実装
//!
//! NV12 入力をリアルタイム CBR で AV1 にエンコードする。

use shiguredo_aom::{
    AomRational, EncodeOptions, Encoder, EncoderConfig, ImageData, ImageFormat, RateControlMode,
    Usage,
};
use shiguredo_video_device::VideoFrameOwned;

use super::EncodedFrame;
use crate::error::{Error, Result};

/// MSF catalog の AV1 codec 文字列 (Profile 0 / Level 4.0 / Main tier 相当)
const AV1_CATALOG_CODEC_STRING: &str = "av01.0.08M.08";

/// AV1 OBU_SEQUENCE_HEADER の type
///
/// 根拠: AV1 Bitstream and Decoding Process Specification §5.3.2 Table 5
/// (将来の改訂で OBU フォーマットが変更される可能性あり)
const AV1_OBU_SEQUENCE_HEADER: u8 = 1;

/// AV1 エンコーダ
pub struct Av1Encoder {
    encoder: Encoder,
    /// 1 フレームの表示間隔 (マイクロ秒)
    ///
    /// 1 つの入力から複数の出力フレームが出たときに Timestamp を進める量。
    frame_interval_us: u64,
}

impl Av1Encoder {
    /// AV1 エンコーダを作成する
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
        // 0 だと上限を設定せずにエンコーダの既定値へ委ねるか、上限無しとして扱われる
        // (`NonZeroUsize` が `None` になる)。意図した安全弁にならないため拒否する
        if keyframe_interval_frames == 0 {
            return Err(Error::Other(
                "keyframe interval must be at least one frame".to_string(),
            ));
        }
        let mut config = EncoderConfig::new(width, height, ImageFormat::Nv12);
        config.g_usage = Usage::Realtime;
        config.rc_end_usage = RateControlMode::Cbr;
        config.rc_target_bitrate = bitrate;
        config.g_timebase = AomRational {
            num: 1,
            den: fps as i32,
        };
        config.g_threads = Some(4);
        config.cpu_used = Some(8);
        config.kf_max_dist = Some(keyframe_interval_frames);
        config.kf_min_dist = Some(0);

        let encoder = Encoder::new(config)?;
        let frame_interval_us = 1_000_000 / u64::from(fps);

        tracing::info!(
            "AV1 encoder created ({}x{}, {} fps, {} kbps, keyframe_interval_frames={}, frame_interval_us={})",
            width,
            height,
            fps,
            bitrate,
            keyframe_interval_frames,
            frame_interval_us
        );

        Ok(Self {
            encoder,
            frame_interval_us,
        })
    }
}

impl Av1Encoder {
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
        let image = ImageData::Nv12 {
            y: &frame.data,
            uv: frame.uv_data.as_deref().unwrap_or(&[]),
        };

        let options = EncodeOptions { force_keyframe };

        self.encoder.encode(&image, &options)?;

        let mut frames = Vec::new();
        let mut output_index: u64 = 0;
        while let Some(encoded) = self.encoder.next_frame() {
            let timestamp =
                super::output_timestamp_us(timestamp_us, output_index, self.frame_interval_us);
            let data = encoded.data()?.to_vec();
            let is_keyframe = encoded.is_keyframe();
            let video_config = if is_keyframe {
                extract_av1_sequence_header(&data)
            } else {
                None
            };
            frames.push(EncodedFrame {
                data,
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
        AV1_CATALOG_CODEC_STRING
    }
}

/// AV1 ビットストリームから Sequence Header OBU を抽出する
///
/// MP4 パススルーのリーダーも、サンプルに Sequence Header が含まれるかの判定に使う。
///
/// 根拠: AV1 Bitstream and Decoding Process Specification §5.3
/// (将来の改訂で OBU フォーマットが変更される可能性あり)
pub(crate) fn extract_av1_sequence_header(data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() {
        return None;
    }
    let mut pos = 0;
    while pos < data.len() {
        let header_byte = data[pos];
        let obu_type = (header_byte >> 3) & 0x0F;
        let has_extension = (header_byte >> 2) & 0x01 == 1;
        let has_size = (header_byte >> 1) & 0x01 == 1;
        let header_size = if has_extension { 2 } else { 1 };
        if !has_size {
            if obu_type == AV1_OBU_SEQUENCE_HEADER {
                return Some(data[pos..].to_vec());
            }
            break;
        }
        if pos + header_size >= data.len() {
            break;
        }
        let (obu_size, leb_len) = read_leb128(&data[pos + header_size..])?;
        let total_size = header_size + leb_len + obu_size;
        if obu_type == AV1_OBU_SEQUENCE_HEADER {
            let end = pos + total_size;
            if end <= data.len() {
                return Some(data[pos..end].to_vec());
            }
        }
        pos += total_size;
    }
    None
}

/// LEB128 エンコードされた値を読む
fn read_leb128(data: &[u8]) -> Option<(usize, usize)> {
    let mut value: usize = 0;
    for (i, &byte) in data.iter().enumerate().take(8) {
        value |= ((byte & 0x7F) as usize) << (i * 7);
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::encoder::test_frame;

    /// キーフレームを強制したフレームがキーフレームとして出力されること
    ///
    /// Group 長を時間ベースで守るには、任意のフレームでキーフレームを要求できる必要がある。
    /// libaom は要求した呼び出しの中でキーフレームを返す (遅延しない) ため、強制した入力の
    /// 出力がキーフレームになり、その次の入力はキーフレームにならないことを固定する。
    #[test]
    fn forced_frame_is_encoded_as_a_keyframe() {
        // 上限を 600 フレームにして、強制しなければキーフレームが出ないようにする
        let mut encoder =
            Av1Encoder::new(64, 48, 30, 500, 600).expect("AV1 エンコーダを作成できること");
        let mut keyframes = Vec::new();
        for index in 0..8u64 {
            let frames = encoder
                .encode(&test_frame(64, 48), index * 33_333, index == 5)
                .expect("AV1 エンコードに成功すること");
            keyframes.push(frames.iter().any(|frame| frame.is_keyframe));
        }
        assert!(keyframes[0], "先頭フレームがキーフレームであること");
        assert!(
            keyframes[1..5].iter().all(|is_keyframe| !is_keyframe),
            "強制しなければキーフレームが出ないこと: {keyframes:?}"
        );
        assert!(
            keyframes[5],
            "強制したフレームがキーフレームとして出力されること: {keyframes:?}"
        );
        assert!(
            !keyframes[6],
            "強制した次のフレームはキーフレームにならないこと: {keyframes:?}"
        );
    }

    /// fps が 0 の場合はエラーになること
    #[test]
    fn zero_fps_is_rejected() {
        assert!(
            Av1Encoder::new(64, 48, 0, 500, 60).is_err(),
            "fps が 0 の場合はエラーになること"
        );
    }

    /// キーフレーム間隔が 0 の場合はエラーになること
    ///
    /// 0 を上限として渡すとエンコーダによっては上限無しとして扱われ、安全弁が消える。
    #[test]
    fn zero_keyframe_interval_is_rejected() {
        assert!(
            Av1Encoder::new(64, 48, 30, 500, 0).is_err(),
            "キーフレーム間隔が 0 の場合はエラーになること"
        );
    }
}
