//! C4M (CAT / CWT / COSE / CBOR) のテスト
//!
//! draft-02 付録 A の公式ベクタ (`tests/test_c4m/vectors_draft02.rs`) と、
//! draft-01 由来の compact ベクタ (`tests/test_c4m/vectors.rs`) を分けて扱う。
//! トークン / DPoP proof の発行と検証のラウンドトリップもここで検証する。

#[path = "test_c4m/helpers.rs"]
mod helpers;

#[path = "test_c4m/vectors.rs"]
mod vectors;

#[path = "test_c4m/vectors_draft02.rs"]
mod vectors_draft02;

#[path = "test_c4m/draft02.rs"]
mod draft02;

#[path = "test_c4m/cbor.rs"]
mod cbor;

#[path = "test_c4m/cose.rs"]
mod cose;

#[path = "test_c4m/cat.rs"]
mod cat;

#[path = "test_c4m/moqt.rs"]
mod moqt;

#[path = "test_c4m/jwk.rs"]
mod jwk;

#[path = "test_c4m/jwt.rs"]
mod jwt;

#[path = "test_c4m/dpop.rs"]
mod dpop;
