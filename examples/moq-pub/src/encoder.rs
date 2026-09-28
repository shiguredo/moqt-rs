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
    /// タイムスタンプ (timescale 単位)
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
    pub fn encode(&mut self, frame: &VideoFrameOwned) -> Result<Vec<EncodedFrame>> {
        match self {
            VideoEncoder::Av1(e) => e.encode(frame),
            #[cfg(target_os = "macos")]
            VideoEncoder::H264(e) => e.encode(frame),
            #[cfg(target_os = "macos")]
            VideoEncoder::H265(e) => e.encode(frame),
        }
    }

    /// 1 秒あたりの timestamp 単位数
    pub fn timescale(&self) -> u64 {
        match self {
            VideoEncoder::Av1(e) => e.timescale(),
            #[cfg(target_os = "macos")]
            VideoEncoder::H264(e) => e.timescale(),
            #[cfg(target_os = "macos")]
            VideoEncoder::H265(e) => e.timescale(),
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
