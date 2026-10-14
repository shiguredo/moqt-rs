//! Opus デコーダラッパ

use shiguredo_opus::{Decoder, DecoderConfig};

use crate::error::Result;

/// Opus デコーダ
pub struct OpusDecoder {
    decoder: Decoder,
    sample_rate: u32,
    channels: u8,
}

impl OpusDecoder {
    pub fn new(sample_rate: u32, channels: u8) -> Result<Self> {
        let decoder = Decoder::new(DecoderConfig::new(sample_rate, channels))?;
        Ok(Self {
            decoder,
            sample_rate,
            channels,
        })
    }

    /// 復号器を作り直す
    ///
    /// 中に溜まった未処理の入力を捨てて live へ戻す。受信側の追いつきで、遅れが固定された
    /// ときに使う。
    pub fn reset(&mut self) -> Result<()> {
        self.decoder = Decoder::new(DecoderConfig::new(self.sample_rate, self.channels))?;
        Ok(())
    }

    /// 1 Opus パケットをデコードして PCM (S16 interleaved) を返す
    pub fn decode(&mut self, data: &[u8]) -> Result<Vec<i16>> {
        Ok(self.decoder.decode(data)?)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u8 {
        self.channels
    }
}
