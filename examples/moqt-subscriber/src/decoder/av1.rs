//! AV1 デコーダ (dav1d)

use crate::decoder::{DecodedVideoFrame, copy_plane};
use crate::error::Result;

/// dav1d を用いた AV1 デコーダ
pub struct Av1Decoder {
    decoder: shiguredo_dav1d::Decoder,
}

impl Av1Decoder {
    pub fn new() -> Result<Self> {
        let decoder = shiguredo_dav1d::Decoder::new(shiguredo_dav1d::DecoderConfig::new())?;
        Ok(Self { decoder })
    }
}

impl Av1Decoder {
    /// 1 オブジェクト分のエンコード済みペイロードをデコードする
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
}
