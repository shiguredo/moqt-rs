//! C4M (CAT / CWT / COSE / CBOR) のプロパティベーステスト
//!
//! - CBOR のラウンドトリップと決定論的エンコードの安定性
//! - COSE 構造のラウンドトリップ
//! - CAT クレームのラウンドトリップ
//! - `moqt` クレームの認可判定と仕様のモデルとの一致
//!
//! 署名 / 検証 (aws-lc-rs feature) を使う property はここには置かず、
//! `tests/test_c4m/` のテストで固定する。

mod cat;
mod cbor;
mod common;
mod cose;
mod moqt;
