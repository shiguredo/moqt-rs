//! デルタエンコードされた KVP (Key-Value-Pair) の delta-key 骨格
//!
//! draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure)
//!
//! 複数の KVP モジュール (`loc` / `parameter` / `track_properties` / `object_properties` /
//! `message_parameter`) が共有する「キー昇順ソート → 前要素との delta を varint で読み書き →
//! delta の overflow guard」という骨格のうち、値型に依存しない delta-key の codec のみをここへ集約する。
//!
//! 値形式 (偶数/奇数キーの varint ・長さ付きバイト列、count プレフィックス、型依存エンコーディング) と
//! delta==0 重複検出ポリシー・型固有バリデーションはモジュールごとに異なるため、各モジュールに残す。
//! トレイト・マクロ・クロージャ・汎用中間表現は導入しない (CODEBASE.md 規約)。

use crate::{error::MessageError, varint};
use alloc::vec::Vec;

/// 昇順ソート済みキー列の delta-key を `buf` に書き込む
///
/// `key.checked_sub(prev)` で delta を求め varint で追記する。`key < prev` は呼び出し側が
/// 昇順ソートを保証する契約に反する実装バグなので `.expect("sorted order invariant violated")`
/// で panic する (各モジュールが従来から用いていた挙動と等価。Result 化すると呼び出し側の
/// 挙動が panic から伝播へ変わる破壊的変更になるため行わない)。
pub(crate) fn encode_delta_key(prev: u64, key: u64, buf: &mut Vec<u8>) {
    let delta = key
        .checked_sub(prev)
        .expect("sorted order invariant violated");
    varint::encode(delta, buf);
}

/// `buf` の `pos` 位置から delta varint を読み、絶対キーを復元して `(key, delta)` を返す
///
/// `prev.checked_add(delta)` の overflow は PROTOCOL_VIOLATION で弾く
/// (draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure))。delta を併せて返すのは、
/// 各モジュールの delta==0 重複検出が delta を直接参照するため (隠蔽すると検出を残せない)。
pub(crate) fn decode_delta_key(
    prev: u64,
    buf: &[u8],
    pos: &mut usize,
) -> Result<(u64, u64), MessageError> {
    let (delta, n) = varint::decode(&buf[*pos..])?;
    *pos += n;
    let key = prev
        .checked_add(delta)
        .ok_or(MessageError::ProtocolViolation("KVP type delta overflow"))?;
    Ok((key, delta))
}
