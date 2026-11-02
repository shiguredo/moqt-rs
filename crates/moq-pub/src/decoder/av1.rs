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
        self.take_frames_until_eagain()
    }

    /// デコーダが内部に保持している遅延フレームを吐き切る (周回の末尾で使う)
    ///
    /// alt-ref などの遅延フレームを持つ AV1 では、`next_frame` が `None` を返した時点で
    /// まだ内部にデコード済みのフレームが残っている。`shiguredo_dav1d` の `Decoder::finish`
    /// の NOTE は「`dav1d_get_picture` が EAGAIN を返した後にもう一度呼び出すと、強制的に
    /// バッファ内のデコード画像が取得される」と定めるため、`None` の後にもう一度列挙して
    /// 残りを取り出す。
    ///
    /// 周回の先頭で `reset` (dav1d の `flush`) を呼ぶと「未消費のデータやバッファ中の
    /// フレームは全て破棄される」ため、`reset` より前に呼ぶ必要がある。
    pub fn drain_delayed(&mut self) -> Result<Vec<DecodedVideoFrame>> {
        // finish 自体は何もしない (crate の NOTE 参照) が、他のデコーダのインタフェースに
        // 合わせて「これ以上データが来ない」ことを伝えてから吐き切る
        self.decoder.finish()?;
        let mut frames = self.take_frames_until_eagain()?;
        // NOTE の効力で取得できるフレームは 1 回目で出切ることが多いが、
        // 実装依存のため列挙が空になるまで繰り返す
        loop {
            let more = self.take_frames_until_eagain()?;
            if more.is_empty() {
                break;
            }
            frames.extend(more);
        }
        Ok(frames)
    }

    /// デコーダの内部状態をリセットする (周回の先頭で使う)
    ///
    /// 未消費のデータやバッファ中のフレームは破棄される。周回の末尾では
    /// [`Av1Decoder::drain_delayed`] を先に呼び、遅延フレームを失わないようにする。
    pub fn reset(&mut self) {
        self.decoder.flush();
    }

    /// `next_frame` が `None` を返すまでフレームを取り出す
    fn take_frames_until_eagain(&mut self) -> Result<Vec<DecodedVideoFrame>> {
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
