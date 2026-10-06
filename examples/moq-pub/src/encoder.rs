//! ビデオエンコーダの Enum 抽象化
//!
//! codec ごとの実装を [`VideoEncoder`] の variant に持ち、`match` で分岐する。
//! 具体実装は codec 別サブモジュールに分離する。

use shiguredo_video_device::VideoFrameOwned;

use crate::error::Result;

pub(crate) mod av1;
#[cfg(target_os = "macos")]
pub(crate) mod h264;
#[cfg(target_os = "macos")]
pub(crate) mod h265;
pub(crate) mod opus;

/// エンコード済みフレーム
pub struct EncodedFrame {
    /// 圧縮済みビットストリーム
    pub data: Vec<u8>,
    /// キーフレームかどうか
    pub is_keyframe: bool,
    /// タイムスタンプ
    ///
    /// live capture では Unix epoch マイクロ秒 (Timescale を載せないため LOC の既定。
    /// draft-ietf-moq-loc-04 §2.3.1.1)、MP4 の経路では入力トラックの timescale 単位である。
    pub timestamp: u64,
    /// PROP_VIDEO_CONFIG に載せる codec 固有の設定データ
    ///
    /// キーフレームの場合のみ `Some`。
    /// - AV1: Sequence Header OBU
    /// - H.264: AVCDecoderConfigurationRecord (ISO/IEC 14496-15 §5.2.4.1.1)
    /// - H.265: HEVCDecoderConfigurationRecord (ISO/IEC 14496-15 §8.3.3.1.2)
    pub video_config: Option<Vec<u8>>,
}

/// サンプル publisher が扱うビデオエンコーダ
///
/// codec ごとの実装を variant に持つ Enum とし、呼び出し側は本型のメソッド経由で
/// 利用する。macOS 限定の codec は variant 単位で `cfg` を付ける。
pub(crate) enum VideoEncoder {
    /// AV1 エンコーダ
    ///
    /// `shiguredo_aom::Encoder` が大きいため、Enum 全体のサイズを抑える目的で Box 化する。
    Av1(Box<av1::Av1Encoder>),
    /// H.264 エンコーダ (Apple Video Toolbox、macOS 限定)
    #[cfg(target_os = "macos")]
    H264(h264::H264Encoder),
    /// H.265 エンコーダ (Apple Video Toolbox、macOS 限定)
    #[cfg(target_os = "macos")]
    H265(h265::H265Encoder),
}

impl VideoEncoder {
    /// 1 フレームをエンコードして 0 個以上のエンコード済みフレームを返す
    ///
    /// `timestamp_us` は入力フレームのメディア時刻 (マイクロ秒)。先頭の出力フレームへ
    /// そのまま載せ、1 つの入力から複数の出力フレームが出た場合は 1 フレームずつ進める。
    pub fn encode(
        &mut self,
        frame: &VideoFrameOwned,
        timestamp_us: u64,
    ) -> Result<Vec<EncodedFrame>> {
        match self {
            VideoEncoder::Av1(e) => e.encode(frame, timestamp_us),
            #[cfg(target_os = "macos")]
            VideoEncoder::H264(e) => e.encode(frame, timestamp_us),
            #[cfg(target_os = "macos")]
            VideoEncoder::H265(e) => e.encode(frame, timestamp_us),
        }
    }

    /// MSF catalog に載せる RFC 6381 形式の codec 文字列
    pub fn catalog_codec_string(&self) -> &str {
        match self {
            VideoEncoder::Av1(e) => e.catalog_codec_string(),
            #[cfg(target_os = "macos")]
            VideoEncoder::H264(e) => e.catalog_codec_string(),
            #[cfg(target_os = "macos")]
            VideoEncoder::H265(e) => e.catalog_codec_string(),
        }
    }
}

/// 出力フレームの Timestamp を決める
///
/// 入力の Timestamp をそのまま先頭の出力フレームへ載せ、`output_index` ぶんだけ
/// 1 フレーム間隔を進める。エンコーダが遅延を取り戻すために 1 つの入力から複数の
/// 出力を返すことがあり、その場合も同一の Timestamp を複数の Object に載せないため。
fn output_timestamp_us(input_timestamp_us: u64, output_index: u64, frame_interval_us: u64) -> u64 {
    input_timestamp_us.saturating_add(output_index.saturating_mul(frame_interval_us))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 先頭の出力は入力の Timestamp をそのまま使うこと
    #[test]
    fn first_output_frame_keeps_input_timestamp() {
        assert_eq!(
            output_timestamp_us(1_700_000_000_000_000, 0, 33_333),
            1_700_000_000_000_000,
            "先頭の出力フレームは入力の Timestamp をそのまま使うこと"
        );
    }

    /// 複数の出力フレームの Timestamp が 1 フレーム間隔ずつ進むこと
    ///
    /// 同一の入力から 2 つ以上の Object を送るときに、同一の Timestamp を載せない。
    #[test]
    fn additional_output_frames_advance_by_one_frame_interval() {
        let base = 1_700_000_000_000_000;
        let timestamps: Vec<u64> = (0..3)
            .map(|index| output_timestamp_us(base, index, 33_333))
            .collect();
        assert_eq!(
            timestamps,
            vec![base, base + 33_333, base + 66_666],
            "出力フレームごとに 1 フレーム間隔ずつ進むこと"
        );
        assert!(
            timestamps.windows(2).all(|pair| pair[0] < pair[1]),
            "Timestamp が単調に増加すること"
        );
    }

    /// 桁あふれする値でも飽和して単調性を保つこと
    #[test]
    fn output_timestamp_saturates_instead_of_wrapping() {
        assert_eq!(
            output_timestamp_us(u64::MAX, 1, 33_333),
            u64::MAX,
            "桁あふれは飽和させること"
        );
        assert_eq!(
            output_timestamp_us(0, u64::MAX, 33_333),
            u64::MAX,
            "間隔の積が桁あふれする場合も飽和させること"
        );
    }
}
