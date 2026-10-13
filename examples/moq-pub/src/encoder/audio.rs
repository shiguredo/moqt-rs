//! 音声エンコーダの Enum 抽象化
//!
//! codec ごとの実装を [`AudioEncoder`] の variant に持ち、`match` で分岐する。
//! 具体実装は codec 別サブモジュールに分離する。macOS 限定の codec は variant 単位で
//! `cfg` を付ける。

#[cfg(target_os = "macos")]
use super::aac::AacEncoder;
use super::opus::OpusEncoder;
use crate::cli::AudioCodec;
use crate::error::Result;

/// サンプル publisher が扱う音声エンコーダ
///
/// 呼び出し側は本型のメソッド経由で利用する。AAC は Apple Audio Toolbox を使うため
/// macOS 限定であり、variant 単位で `cfg` を付ける。
pub(crate) enum AudioEncoder {
    /// Opus エンコーダ (libopus、全プラットフォーム)
    Opus(OpusEncoder),
    /// AAC-LC エンコーダ (Apple Audio Toolbox、macOS 限定)
    #[cfg(target_os = "macos")]
    Aac(AacEncoder),
}

impl AudioEncoder {
    /// 指定したコーデックの音声エンコーダを作成する
    ///
    /// `bitrate` は kbps 単位である。
    pub(crate) fn new(
        codec: AudioCodec,
        sample_rate: u32,
        channels: u8,
        bitrate: u32,
    ) -> Result<Self> {
        match codec {
            AudioCodec::Opus => Ok(Self::Opus(OpusEncoder::new(
                sample_rate,
                channels,
                bitrate.saturating_mul(1_000),
            )?)),
            #[cfg(target_os = "macos")]
            AudioCodec::Aac => Ok(Self::Aac(AacEncoder::new(sample_rate, channels, bitrate)?)),
            #[cfg(not(target_os = "macos"))]
            AudioCodec::Aac => Err(crate::error::Error::Other(
                "AAC encoder is only supported on macOS".to_string(),
            )),
        }
    }

    /// 1 フレーム分の PCM をエンコードして 1 パケットを返す
    ///
    /// AAC はエンコードを専用スレッドが行うため、完了を待って返す。
    pub(crate) async fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>> {
        match self {
            Self::Opus(e) => e.encode(pcm),
            #[cfg(target_os = "macos")]
            Self::Aac(e) => e.encode(pcm).await,
        }
    }

    /// 1 フレームあたりのサンプル数 (チャンネル単位)
    pub(crate) fn samples_per_frame(&self) -> usize {
        match self {
            Self::Opus(e) => e.samples_per_frame(),
            #[cfg(target_os = "macos")]
            Self::Aac(e) => e.samples_per_frame(),
        }
    }

    /// MSF catalog に載せる RFC 6381 形式の codec 文字列
    pub(crate) fn catalog_codec_string(&self) -> &str {
        match self {
            Self::Opus(e) => e.catalog_codec_string(),
            #[cfg(target_os = "macos")]
            Self::Aac(e) => e.catalog_codec_string(),
        }
    }

    /// Audio Config プロパティ (LOC draft-ietf-moq-loc-04 §2.3.3.1) に載せる
    /// codec 固有の設定データを構築する
    ///
    /// - Opus: OpusHead (RFC 7845 §5.1)
    /// - AAC-LC: AudioSpecificConfig (ISO/IEC 14496-3 §1.6.2.1)
    pub(crate) fn audio_config(&self, sample_rate: u32, channels: u8) -> Result<Vec<u8>> {
        match self {
            Self::Opus(_) => Ok(super::opus::build_opus_head(sample_rate, channels)),
            #[cfg(target_os = "macos")]
            Self::Aac(_) => super::aac::audio_specific_config(sample_rate, channels),
        }
    }
}
