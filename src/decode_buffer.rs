//! インクリメンタルデコーダ共通の受信バッファ
//!
//! 受信バッファの先頭からメッセージを 1 個ずつ取り出すとき、`Vec::drain(..n)` を
//! メッセージごとに呼ぶと、残りのバイト列を毎回先頭へ詰め直すため
//! `O(バッファ長 × メッセージ数)` のコピーが発生する。1 つの受信チャンクに
//! 小さなメッセージが大量に含まれる場合、これが二乗のコストになる。
//!
//! このバッファは読み出し開始位置 `start` を保持し、メッセージの取り出しでは
//! `start` を進めるだけにする。`push` 時に消費済みの領域がバッファの半分以上の
//! ときだけ 1 回詰め直すため、コピーの総量は償却 `O(受信バイト数)` になる。
//! 先頭のバイト列を連続したスライスとして見せる必要がある varint デコードとは
//! `available` がそのスライスを返すことで両立する。

use alloc::vec::Vec;

/// 消費済み領域を詰め直す最小サイズ (バイト)
///
/// これ未満では詰め直さず、push のたびに小さな memmove を行わないようにする。
const COMPACT_MIN_BYTES: usize = 4096;

/// インクリメンタルデコーダ用の受信バッファ
///
/// 不変条件: `start <= buf.len()`。`buf[..start]` は消費済みで、`buf[start..]` が
/// まだデコード対象のバイト列である。
#[derive(Debug, Clone)]
pub(crate) struct DecodeBuffer {
    buf: Vec<u8>,
    start: usize,
}

impl DecodeBuffer {
    /// 空のバッファを作成する
    pub(crate) const fn new() -> Self {
        Self {
            buf: Vec::new(),
            start: 0,
        }
    }

    /// 受信データを末尾に追加する
    ///
    /// 消費済み領域が十分大きい場合だけ、追加前に 1 回だけ先頭へ詰め直す。
    /// 詰め直しのコストは直前の詰め直し以降に消費したバイト数以下であり、
    /// 全体では受信バイト数に比例する (償却 O(1))。
    pub(crate) fn push(&mut self, data: &[u8]) {
        if self.start == self.buf.len() {
            // 全て消費済み: 容量は維持したまま先頭から再利用する
            self.buf.clear();
            self.start = 0;
        } else if self.start >= COMPACT_MIN_BYTES
            && self.start >= self.buf.len().saturating_sub(self.start)
        {
            // 残りが消費済み以下になったら、残りだけを先頭へ移す
            self.buf.drain(..self.start);
            self.start = 0;
        }
        self.buf.extend_from_slice(data);
    }

    /// 未消費のバイト列を返す
    pub(crate) fn available(&self) -> &[u8] {
        &self.buf[self.start..]
    }

    /// 未消費のバイト数を返す
    pub(crate) fn len(&self) -> usize {
        self.buf.len() - self.start
    }

    /// 未消費のバイト列が空かを返す
    pub(crate) fn is_empty(&self) -> bool {
        self.start == self.buf.len()
    }

    /// 先頭 `n` バイトを消費済みにする
    ///
    /// 呼び出し側は `n <= self.len()` を保証すること。デコード済みのバイト列は
    /// 移動せず、読み出し位置だけを進める。
    pub(crate) fn advance(&mut self, n: usize) {
        debug_assert!(n <= self.len(), "advance beyond buffered bytes");
        self.start += n;
        if self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        }
    }

    /// 先頭 `n` バイトを `Vec` として取り出して消費する
    ///
    /// ペイロードのように呼び出し側へ所有権ごと渡すデータに使う。バッファが
    /// `n` バイト未満の場合は何もせず `None` を返す。
    pub(crate) fn take(&mut self, n: usize) -> Option<Vec<u8>> {
        if n > self.len() {
            return None;
        }
        let bytes = self.buf[self.start..self.start + n].to_vec();
        self.advance(n);
        Some(bytes)
    }
}

impl Default for DecodeBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 追加したデータがそのまま未消費列として見えること
    #[test]
    fn push_then_available_returns_all_bytes() {
        let mut buffer = DecodeBuffer::new();
        assert!(buffer.is_empty());
        buffer.push(&[1, 2, 3]);
        buffer.push(&[4]);
        assert_eq!(buffer.available(), &[1, 2, 3, 4]);
        assert_eq!(buffer.len(), 4);
        assert!(!buffer.is_empty());
    }

    /// 消費した分だけ先頭が進み、残りは変わらないこと
    #[test]
    fn advance_moves_start_without_copying_remaining() {
        let mut buffer = DecodeBuffer::new();
        buffer.push(&[1, 2, 3, 4, 5]);
        buffer.advance(2);
        assert_eq!(buffer.available(), &[3, 4, 5]);
        assert_eq!(buffer.len(), 3);
        buffer.advance(3);
        assert!(buffer.is_empty());
        assert_eq!(buffer.available(), &[] as &[u8]);
    }

    /// 全消費後に追加すると、前回の内容が残らないこと
    #[test]
    fn push_after_full_consume_reuses_empty_buffer() {
        let mut buffer = DecodeBuffer::new();
        buffer.push(&[1, 2, 3]);
        buffer.advance(3);
        buffer.push(&[9]);
        assert_eq!(buffer.available(), &[9]);
    }

    /// 消費済みが閾値を超えると push 時に詰め直され、未消費列が保たれること
    #[test]
    fn push_compacts_consumed_prefix() {
        let mut buffer = DecodeBuffer::new();
        let data = alloc::vec![7u8; COMPACT_MIN_BYTES * 2];
        buffer.push(&data);
        buffer.advance(COMPACT_MIN_BYTES + 1);
        let remaining = buffer.available().to_vec();
        assert_eq!(remaining.len(), COMPACT_MIN_BYTES - 1);
        // ここで詰め直しが走る
        buffer.push(&[8]);
        assert_eq!(buffer.available().len(), COMPACT_MIN_BYTES);
        assert_eq!(buffer.available()[0], 7);
        assert_eq!(buffer.available()[COMPACT_MIN_BYTES - 1], 8);
    }

    /// `take` が先頭から指定長を取り出し、残りを保つこと
    #[test]
    fn take_returns_prefix_and_keeps_remainder() {
        let mut buffer = DecodeBuffer::new();
        buffer.push(&[1, 2, 3, 4]);
        assert_eq!(buffer.take(2), Some(alloc::vec![1, 2]));
        assert_eq!(buffer.available(), &[3, 4]);
        assert_eq!(buffer.take(3), None);
        assert_eq!(buffer.available(), &[3, 4]);
    }
}
