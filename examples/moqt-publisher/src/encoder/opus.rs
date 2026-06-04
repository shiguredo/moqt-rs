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
    /// PROP_TIMESCALE に載せるサンプリングレート
    sample_rate: u32,
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

        Ok(Self {
            encoder,
            sample_rate,
        })
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

    /// PROP_TIMESCALE に載せる値 (= サンプリングレート Hz)
    pub fn timescale(&self) -> u64 {
        self.sample_rate as u64
    }

    /// MSF catalog に載せる RFC 6381 形式の codec 文字列
    pub fn catalog_codec_string(&self) -> &str {
        OPUS_CATALOG_CODEC_STRING
    }
}
