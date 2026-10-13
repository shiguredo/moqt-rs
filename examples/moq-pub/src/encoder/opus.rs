//! shiguredo_opus による Opus エンコーダ実装
//!
//! 48 kHz / 1ch / 20 ms フレーム前提。`Application::Audio` (汎用) を選択し、
//! catalog の codec 文字列は RFC 6381 に従って `"opus"` を返す。

use shiguredo_opus::{Application, Encoder, EncoderConfig, FrameDuration};

use crate::error::Result;

/// MSF catalog の Opus codec 文字列 (RFC 6381)
const OPUS_CATALOG_CODEC_STRING: &str = "opus";

/// Opus フレーム長 (ミリ秒)
///
/// レイテンシとフレームサイズのトレードオフ、および fake_audio_capture の 960 sample / 20 ms との整合のため 20 ms 固定とする。
/// (将来 FrameDuration を CLI 化する場合はここを差し替える)
const OPUS_FRAME_DURATION_MS: u32 = 20;

/// Opus エンコーダ
pub struct OpusEncoder {
    encoder: Encoder,
}

impl OpusEncoder {
    /// Opus エンコーダを作成する
    pub fn new(sample_rate: u32, channels: u8, bitrate: u32) -> Result<Self> {
        let mut config = EncoderConfig::new(sample_rate, channels);
        config.bitrate = Some(bitrate);
        config.application = Some(Application::Audio);
        config.frame_duration = Some(FrameDuration::Ms20);

        let encoder = Encoder::new(config)?;
        let frame_samples = encoder.frame_samples();

        tracing::info!(
            "Opus encoder created ({} Hz, {} ch, {} bps, {} ms / {} samples per frame)",
            sample_rate,
            channels,
            bitrate,
            OPUS_FRAME_DURATION_MS,
            frame_samples,
        );

        Ok(Self { encoder })
    }

    /// インターリーブ済み i16 PCM (`samples_per_frame * channels` サンプル) を
    /// 1 フレームエンコードする
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>> {
        Ok(self.encoder.encode(pcm)?)
    }

    /// 1 フレームあたりのサンプル数 (チャンネル単位)
    pub fn samples_per_frame(&self) -> usize {
        self.encoder.frame_samples()
    }

    /// MSF catalog に載せる RFC 6381 形式の codec 文字列
    pub fn catalog_codec_string(&self) -> &str {
        OPUS_CATALOG_CODEC_STRING
    }
}

/// OpusHead パケットを構築する (RFC 7845 §5.1 (Identification Header))
///
/// Audio Config プロパティ (draft-ietf-moq-loc-04 §2.3.3.1 (Audio Config)) に載せる
/// Opus の configuration bytes を生成する。
///
/// フォーマット:
/// - Magic: "OpusHead" (8 bytes)
/// - Version: 1 (1 byte)
/// - Channel Count: channels (1 byte)
/// - Pre-skip: 0 (2 bytes, little-endian)
/// - Input Sample Rate: sample_rate (4 bytes, little-endian)
/// - Output Gain: 0 (2 bytes, little-endian)
/// - Channel Mapping Family: 0 (1 byte)
pub(crate) fn build_opus_head(sample_rate: u32, channels: u8) -> Vec<u8> {
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // Version
    head.push(channels); // Channel Count
    head.extend_from_slice(&0u16.to_le_bytes()); // Pre-skip
    head.extend_from_slice(&sample_rate.to_le_bytes()); // Input Sample Rate
    head.extend_from_slice(&0u16.to_le_bytes()); // Output Gain
    head.push(0); // Channel Mapping Family
    head
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OpusHead 構築: RFC 7845 §5.1 (Identification Header) のフォーマットで 19 バイトのパケットが生成されること
    #[test]
    fn build_opus_head_format() {
        let head = build_opus_head(48_000, 1);
        assert_eq!(head.len(), 19, "OpusHead は 19 バイトであること");
        assert_eq!(&head[0..8], b"OpusHead", "Magic は OpusHead であること");
        assert_eq!(head[8], 1, "Version は 1 であること");
        assert_eq!(head[9], 1, "Channel Count は 1 であること");
        assert_eq!(&head[10..12], &[0x00, 0x00], "Pre-skip は 0 であること");
        assert_eq!(
            &head[12..16],
            &48_000u32.to_le_bytes(),
            "Input Sample Rate は 48000 であること"
        );
        assert_eq!(&head[16..18], &[0x00, 0x00], "Output Gain は 0 であること");
        assert_eq!(head[18], 0, "Channel Mapping Family は 0 であること");
    }
}
