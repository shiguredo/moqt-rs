//! C4M (CAT / CWT / COSE / CBOR) のテスト
//!
//! C4M のテストベクタ (draft-01 付録 A 由来、moqt claim 値は draft-02 §A.5 と一致) と、
//! トークン / DPoP proof の発行と検証のラウンドトリップを扱う。

#[path = "test_c4m/helpers.rs"]
mod helpers;

#[path = "test_c4m/vectors.rs"]
mod vectors;

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
