//! AV1 デコーダ (dav1d)
//!
//! MP4 再エンコード配信の入力 AV1 をデコードする。

use crate::decoder::{DecodedVideoFrame, copy_plane};
use crate::error::Result;

/// dav1d を用いた AV1 デコーダ
pub struct Av1Decoder {
    decoder: shiguredo_dav1d::Decoder,
}

impl Av1Decoder {
    /// AV1 デコーダを作成する
    pub fn new() -> Result<Self> {
        let decoder = shiguredo_dav1d::Decoder::new(shiguredo_dav1d::DecoderConfig::new())?;
        Ok(Self { decoder })
    }
}

impl Av1Decoder {
    /// 1 サンプル分のエンコード済みペイロードをデコードする
    ///
    /// AV1 では `video_config` は使わない (payload 内の Sequence Header を前提とする)。
    pub fn decode(
        &mut self,
        payload: &[u8],
        _video_config: Option<&[u8]>,
    ) -> Result<Vec<DecodedVideoFrame>> {
        self.decoder.decode(payload)?;
        let mut frames = Vec::new();
        while let Some(frame) = self.decoder.next_frame()? {
            let w = frame.width();
            let h = frame.height();
            let y = copy_plane(frame.y_plane(), frame.y_stride(), w, h);
            let u = copy_plane(frame.u_plane(), frame.u_stride(), w / 2, h / 2);
            let v = copy_plane(frame.v_plane(), frame.v_stride(), w / 2, h / 2);
            frames.push(DecodedVideoFrame {
                y,
                u,
                v,
                width: w as i32,
                height: h as i32,
            });
        }
        Ok(frames)
    }

    /// デコーダの内部状態をリセットする (周回の先頭で使う)
    ///
    /// 未消費のデータやバッファ中のフレームは破棄される。
    pub fn reset(&mut self) {
        self.decoder.flush();
    }
}
