//! Opus デコーダラッパ
//!
//! MP4 再エンコード配信の入力 Opus と、subscriber が受信した Opus を PCM にデコードする。

use shiguredo_opus::{Decoder, DecoderConfig};

use crate::error::Result;

/// Opus デコーダ
pub struct OpusDecoder {
    decoder: Decoder,
    /// デコード後のサンプルレート (Hz)
    ///
    /// デコーダを作り直すときに使う。
    sample_rate: u32,
    /// デコード後のチャンネル数
    ///
    /// デコーダを作り直すときに使う。
    channels: u8,
}

impl OpusDecoder {
    /// Opus デコーダを作成する
    ///
    /// `sample_rate` と `channels` はデコード後の PCM のサンプルレート (Hz) と
    /// チャンネル数である。
    pub fn new(sample_rate: u32, channels: u8) -> Result<Self> {
        let decoder = Decoder::new(DecoderConfig::new(sample_rate, channels))?;
        Ok(Self {
            decoder,
            sample_rate,
            channels,
        })
    }

    /// デコーダを作り直す
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

    /// デコード後のサンプルレート (Hz) を返す
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// デコード後のチャンネル数を返す
    pub fn channels(&self) -> u8 {
        self.channels
    }
}
