//! C4M (CAT / CWT / COSE / CBOR) のプロパティベーステスト
//!
//! - CBOR のラウンドトリップと決定論的エンコードの安定性
//! - COSE 構造のラウンドトリップ
//! - CAT クレームのラウンドトリップと、compact / COSE 形式のトークンのデコード
//! - `moqt` クレームの認可判定と仕様のモデルとの一致
//! - DPoP proof のデコードと、暗号を必要としない検証 (actx / 鮮度 / リプレイ)
//! - JWS compact のヘッダとトークンのデコード
//! - JWK のデコード / 正規化 JSON / 鍵変換
//!
//! 署名 / 検証 (aws-lc-rs feature) を使う property はここには置かず、
//! `tests/test_c4m/` のテストで固定する。

mod cat;
mod cbor;
mod common;
mod cose;
mod dpop;
mod jwk;
mod jwt;
mod moqt;
