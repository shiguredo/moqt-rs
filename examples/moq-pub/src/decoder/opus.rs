//! Opus デコーダラッパ
//!
//! MP4 再エンコード配信の入力 Opus を PCM にデコードする。

use shiguredo_opus::{Decoder, DecoderConfig};

use crate::error::Result;

/// Opus デコーダ
pub struct OpusDecoder {
    decoder: Decoder,
}

impl OpusDecoder {
    /// Opus デコーダを作成する
    ///
    /// ステレオ入力は libopus のダウンミックスに任せるため、`channels` には
    /// 出力したいチャンネル数 (1) を渡す。
    pub fn new(sample_rate: u32, channels: u8) -> Result<Self> {
        let decoder = Decoder::new(DecoderConfig::new(sample_rate, channels))?;
        Ok(Self { decoder })
    }

    /// 1 Opus パケットをデコードして PCM (S16 interleaved) を返す
    pub fn decode(&mut self, data: &[u8]) -> Result<Vec<i16>> {
        Ok(self.decoder.decode(data)?)
    }
}
