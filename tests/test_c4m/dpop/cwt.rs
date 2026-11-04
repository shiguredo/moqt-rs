//! CWT 形式の DPoP proof (draft-ietf-moq-c4m-02 §4.2) のテスト
//!
//! `actx` と `ath` の claim label はドラフトで `TBD_ACTX` / `TBD_ATH` のままなので、
//! テストでは Private Use の範囲の値を明示する。署名 / 検証のテストは aws-lc-rs
//! feature で実鍵を使う。

use shiguredo_moqt::c4m::cat::{CLAIM_CWT_ID, CLAIM_ISSUED_AT};
use shiguredo_moqt::c4m::cbor::{self, CborError, Value};
use shiguredo_moqt::c4m::dpop::{
    CwtAuthorizationContext, CwtDpopProofClaims, CwtDpopProofCodec, DpopError,
    MOQT_AUTHORIZATION_CONTEXT_TYPE,
};

use super::super::helpers::decode_hex;

/// テストで使う `actx` の claim label (TBD のため Private Use の値を使う)
const ACTX_LABEL: i64 = -65539;

/// テストで使う `ath` の claim label (TBD のため Private Use の値を使う)
const ATH_LABEL: i64 = -65540;

/// テストで使うコーデックを作る
fn codec() -> CwtDpopProofCodec {
    CwtDpopProofCodec::new(ACTX_LABEL, ATH_LABEL).expect("label 設定を作れる")
}

/// テストで使うクレームを作る
fn claims() -> CwtDpopProofClaims {
    CwtDpopProofClaims {
        cti: b"proof-1".to_vec(),
        issued_at: 1_749_999_000,
        authorization_context: CwtAuthorizationContext {
            context_type: String::from(MOQT_AUTHORIZATION_CONTEXT_TYPE),
            action: 4,
            namespace: Some(vec![b"cdn".to_vec(), b"example.com".to_vec()]),
            track_name: Some(b"/sports/live".to_vec()),
            raw: Vec::new(),
        },
        access_token_hash: vec![0xab; 32],
        // 未知の通常クレームは raw に保持される
        raw: vec![(
            Value::integer(300),
            Value::TextString(String::from("extension")),
        )],
    }
}

/// 完全なクレームの CBOR マップを作る
fn claims_value() -> Value {
    Value::Map(vec![
        (
            Value::integer(CLAIM_ISSUED_AT),
            Value::integer(1_749_999_000),
        ),
        (
            Value::integer(CLAIM_CWT_ID),
            Value::ByteString(b"proof-1".to_vec()),
        ),
        (
            Value::integer(ACTX_LABEL),
            Value::Map(vec![
                (
                    Value::integer(0),
                    Value::TextString(String::from(MOQT_AUTHORIZATION_CONTEXT_TYPE)),
                ),
                (Value::integer(1), Value::integer(4)),
                (
                    Value::integer(2),
                    Value::Array(vec![
                        Value::ByteString(b"cdn".to_vec()),
                        Value::ByteString(b"example.com".to_vec()),
                    ]),
                ),
                (
                    Value::integer(3),
                    Value::ByteString(b"/sports/live".to_vec()),
                ),
            ]),
        ),
        (Value::integer(ATH_LABEL), Value::ByteString(vec![0xab; 32])),
    ])
}

/// クレームのラベル設定の衝突を設定時に拒否する
#[test]
fn codec_rejects_conflicting_claim_labels() {
    // actx と ath が同じ
    assert_eq!(
        CwtDpopProofCodec::new(100, 100),
        Err(DpopError::ClaimLabelConflict)
    );
    // actx が iat (6) / cti (7) と衝突する
    assert_eq!(
        CwtDpopProofCodec::new(CLAIM_ISSUED_AT, 100),
        Err(DpopError::ReservedClaimLabel("actx"))
    );
    assert_eq!(
        CwtDpopProofCodec::new(CLAIM_CWT_ID, 100),
        Err(DpopError::ReservedClaimLabel("actx"))
    );
    // ath が iat (6) / cti (7) と衝突する
    assert_eq!(
        CwtDpopProofCodec::new(100, CLAIM_ISSUED_AT),
        Err(DpopError::ReservedClaimLabel("ath"))
    );
    assert_eq!(
        CwtDpopProofCodec::new(100, CLAIM_CWT_ID),
        Err(DpopError::ReservedClaimLabel("ath"))
    );
    // CWT の登録済み claim key (1〜8) との衝突はすべて拒否する
    for reserved in 1..=8 {
        assert_eq!(
            CwtDpopProofCodec::new(reserved, 100),
            Err(DpopError::ReservedClaimLabel("actx")),
            "actx_label={reserved}"
        );
        assert_eq!(
            CwtDpopProofCodec::new(100, reserved),
            Err(DpopError::ReservedClaimLabel("ath")),
            "ath_label={reserved}"
        );
    }
    // 衝突しない設定は受理し、設定した label をそのまま返す
    let codec = CwtDpopProofCodec::new(ACTX_LABEL, ATH_LABEL).expect("label 設定を作れる");
    assert_eq!(codec.actx_label(), ACTX_LABEL);
    assert_eq!(codec.ath_label(), ATH_LABEL);
}

/// クレームのラウンドトリップで bytes と未知のクレームが保たれる
#[test]
fn claims_round_trip_preserves_bytes_and_unknown_claims() {
    let codec = codec();
    let mut claims = claims();
    // 名前空間とトラック名は UTF-8 でないバイト列もそのまま保持する
    claims.authorization_context.namespace =
        Some(vec![vec![0xff, 0x00, 0x80], b"example.com".to_vec()]);
    claims.authorization_context.track_name = Some(vec![0xff, b'/', 0x00]);
    // 未知の action は整数のまま保持する
    claims.authorization_context.action = 99;
    // actx の未知のメンバーは raw に保持する
    claims.authorization_context.raw.push((
        Value::integer(9),
        Value::TextString(String::from("extension")),
    ));
    // 未知の通常クレームは値の型を問わず raw に保持する
    claims.raw.push((
        Value::integer(200),
        Value::Array(vec![
            Value::TextString(String::from("extension")),
            Value::ByteString(vec![0x00, 0xff]),
        ]),
    ));

    let value = codec
        .encode_claims(&claims)
        .expect("クレームをエンコードできる");
    let decoded = codec
        .decode_claims(&value)
        .expect("クレームをデコードできる");
    assert_eq!(decoded, claims, "クレームのラウンドトリップで値が変わる");
}

/// namespace と track を省略した actx (SETUP など) を表現できる
#[test]
fn claims_allow_namespace_and_track_to_be_absent() {
    let codec = codec();
    let mut claims = claims();
    // SETUP は名前空間もトラックも対象にしない
    claims.authorization_context.action = 1;
    claims.authorization_context.namespace = None;
    claims.authorization_context.track_name = None;
    let value = codec
        .encode_claims(&claims)
        .expect("クレームをエンコードできる");
    // 省略したメンバーは actx の map に出力しない
    let Value::Map(entries) = &value else {
        panic!("クレームはマップ");
    };
    let actx = entries
        .iter()
        .find(|(key, _)| key.as_int() == Some(ACTX_LABEL))
        .map(|(_, value)| value)
        .expect("actx がある");
    let actx_entries = actx.as_map().expect("actx はマップ");
    assert!(
        !actx_entries
            .iter()
            .any(|(key, _)| matches!(key.as_int(), Some(2) | Some(3))),
        "省略した namespace / track を出力した"
    );
    assert_eq!(
        codec
            .decode_claims(&value)
            .expect("クレームをデコードできる"),
        claims
    );
}

/// 必須クレームの欠落と型違反を拒否する
#[test]
fn claims_require_well_typed_members() {
    let codec = codec();
    // 欠落
    for (label, name) in [
        (CLAIM_CWT_ID, "cti"),
        (CLAIM_ISSUED_AT, "iat"),
        (ACTX_LABEL, "actx"),
        (ATH_LABEL, "ath"),
    ] {
        let mut entries = match claims_value() {
            Value::Map(entries) => entries,
            _ => unreachable!("クレームはマップ"),
        };
        entries.retain(|(key, _)| key.as_int() != Some(label));
        assert_eq!(
            codec.decode_claims(&Value::Map(entries)),
            Err(DpopError::MissingClaim(name)),
            "label {label} を欠いたクレームが受理された"
        );
    }

    // 型違反
    let replacements = [
        (CLAIM_CWT_ID, Value::TextString(String::from("id")), "cti"),
        (CLAIM_ISSUED_AT, Value::Float(1.5), "iat"),
        (ACTX_LABEL, Value::Array(Vec::new()), "actx"),
        (ATH_LABEL, Value::integer(1), "ath"),
    ];
    for (label, replacement, name) in replacements {
        let mut entries = match claims_value() {
            Value::Map(entries) => entries,
            _ => unreachable!("クレームはマップ"),
        };
        for (key, value) in &mut entries {
            if key.as_int() == Some(label) {
                *value = replacement.clone();
            }
        }
        assert_eq!(
            codec.decode_claims(&Value::Map(entries)),
            Err(DpopError::UnexpectedType(name)),
            "label {label} の型違反が受理された"
        );
    }

    // actx のメンバーの欠落と型違反
    for (actx, expected) in [
        (
            Value::Map(vec![(
                Value::integer(0),
                Value::TextString(String::from("moqt")),
            )]),
            DpopError::MissingClaim("actx.action"),
        ),
        (
            Value::Map(vec![(Value::integer(1), Value::integer(4))]),
            DpopError::MissingClaim("actx.type"),
        ),
        (
            Value::Map(vec![
                (Value::integer(0), Value::integer(0)),
                (Value::integer(1), Value::integer(4)),
            ]),
            DpopError::UnexpectedType("actx.type"),
        ),
        (
            Value::Map(vec![
                (Value::integer(0), Value::TextString(String::from("moqt"))),
                (Value::integer(1), Value::TextString(String::from("PUB_NS"))),
            ]),
            DpopError::UnexpectedType("actx.action"),
        ),
        (
            Value::Map(vec![
                (Value::integer(0), Value::TextString(String::from("moqt"))),
                (Value::integer(1), Value::integer(4)),
                (Value::integer(2), Value::ByteString(b"cdn".to_vec())),
            ]),
            DpopError::UnexpectedType("actx.namespace"),
        ),
        (
            Value::Map(vec![
                (Value::integer(0), Value::TextString(String::from("moqt"))),
                (Value::integer(1), Value::integer(4)),
                (
                    Value::integer(2),
                    Value::Array(vec![Value::TextString(String::from("cdn"))]),
                ),
            ]),
            DpopError::UnexpectedType("actx.namespace"),
        ),
        (
            Value::Map(vec![
                (Value::integer(0), Value::TextString(String::from("moqt"))),
                (Value::integer(1), Value::integer(4)),
                (Value::integer(3), Value::integer(1)),
            ]),
            DpopError::UnexpectedType("actx.track"),
        ),
    ] {
        let mut entries = match claims_value() {
            Value::Map(entries) => entries,
            _ => unreachable!("クレームはマップ"),
        };
        for (key, value) in &mut entries {
            if key.as_int() == Some(ACTX_LABEL) {
                *value = actx.clone();
            }
        }
        assert_eq!(
            codec.decode_claims(&Value::Map(entries)),
            Err(expected),
            "actx の型違反が受理された: {actx:?}"
        );
    }
}

/// 重複したクレームを拒否する
#[test]
fn claims_reject_duplicate_labels() {
    let codec = codec();
    // `Value` を直接組み立てた場合の重複
    let mut entries = match claims_value() {
        Value::Map(entries) => entries,
        _ => unreachable!("クレームはマップ"),
    };
    entries.push((
        Value::integer(CLAIM_ISSUED_AT),
        Value::integer(1_749_999_001),
    ));
    assert_eq!(
        codec.decode_claims(&Value::Map(entries)),
        Err(DpopError::DuplicateClaim(CLAIM_ISSUED_AT))
    );

    // 未知のテキストクレームの重複も拒否する
    let mut entries = match claims_value() {
        Value::Map(entries) => entries,
        _ => unreachable!("クレームはマップ"),
    };
    entries.push((Value::TextString(String::from("ext")), Value::integer(1)));
    entries.push((Value::TextString(String::from("ext")), Value::integer(2)));
    assert_eq!(
        codec.decode_claims(&Value::Map(entries)),
        Err(DpopError::DuplicateMember(String::from("ext")))
    );

    // CBOR のマップとして重複したクレームは CBOR のデコーダが拒否する
    // (a2 06 01 06 02 = {6: 1, 6: 2})
    let duplicate_claims = decode_hex("a206010602");
    assert_eq!(
        cbor::decode(&duplicate_claims),
        Err(CborError::DuplicateMapKey)
    );
}

/// クレームのエンコードで必須クレームの label を raw に置けない
#[test]
fn claims_encode_rejects_raw_duplicates() {
    let codec = codec();
    let mut duplicated = claims();
    duplicated
        .raw
        .push((Value::integer(CLAIM_CWT_ID), Value::ByteString(vec![0])));
    assert_eq!(
        codec.encode_claims(&duplicated),
        Err(DpopError::DuplicateClaim(CLAIM_CWT_ID))
    );

    let mut duplicated = claims();
    duplicated
        .raw
        .push((Value::integer(ACTX_LABEL), Value::Array(Vec::new())));
    assert_eq!(
        codec.encode_claims(&duplicated),
        Err(DpopError::DuplicateClaim(ACTX_LABEL))
    );
}

// ---------------------------------------------------------------------------
// ここから下は aws-lc-rs feature で実鍵を使うテスト
// ---------------------------------------------------------------------------

#[cfg(feature = "aws-lc-rs")]
use super::super::vectors::{
    ES256_PRIVATE_KEY_HEX, ES256_PUBLIC_KEY_X_HEX, ES256_PUBLIC_KEY_Y_HEX, HMAC_KEY_HEX,
};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::MoqtAction;
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::cat::{
    CLAIM_CONFIRMATION, CONFIRMATION_COSE_KEY, CatToken, CatTokenBuilder, Confirmation,
};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::cose::{
    Algorithm, CoseEncodingOptions, CoseError, CoseMessage, CoseSign1, HEADER_ALGORITHM,
    HEADER_CRITICAL, HEADER_KCCS, HEADER_TYPE,
};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::crypto::{CoseCrypto, CoseKey, CryptoError, EcCurve, OkpCurve};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::dpop::{CwtDpopProof, DPOP_PROOF_CWT_TYPE};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::jwk::Jwk;

/// 付録 A.1 の ES256 秘密鍵
#[cfg(feature = "aws-lc-rs")]
fn es256_private_key() -> CoseKey {
    CoseKey::ec2_with_private_key(
        EcCurve::P256,
        decode_hex(ES256_PUBLIC_KEY_X_HEX),
        decode_hex(ES256_PUBLIC_KEY_Y_HEX),
        decode_hex(ES256_PRIVATE_KEY_HEX),
    )
}

/// 付録 A.1 の ES256 公開鍵
#[cfg(feature = "aws-lc-rs")]
fn es256_public_key() -> CoseKey {
    CoseKey::ec2(
        EcCurve::P256,
        decode_hex(ES256_PUBLIC_KEY_X_HEX),
        decode_hex(ES256_PUBLIC_KEY_Y_HEX),
    )
}

/// テスト専用に生成した固定の ES384 鍵
#[cfg(feature = "aws-lc-rs")]
fn es384_keys() -> (CoseKey, CoseKey) {
    let (x, y, d) = (
        decode_hex(super::super::helpers::ES384_PUBLIC_KEY_X_HEX),
        decode_hex(super::super::helpers::ES384_PUBLIC_KEY_Y_HEX),
        decode_hex(super::super::helpers::ES384_PRIVATE_KEY_HEX),
    );
    (
        CoseKey::ec2_with_private_key(EcCurve::P384, x.clone(), y.clone(), d),
        CoseKey::ec2(EcCurve::P384, x, y),
    )
}

/// RFC 8032 §7.1 のテストベクタ 1 の Ed25519 鍵
#[cfg(feature = "aws-lc-rs")]
fn ed25519_keys() -> (CoseKey, CoseKey) {
    const PUBLIC: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    const PRIVATE: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    (
        CoseKey::ed25519_with_private_key(decode_hex(PUBLIC), decode_hex(PRIVATE)),
        CoseKey::ed25519(decode_hex(PUBLIC)),
    )
}

/// protected ヘッダを組み立てる
#[cfg(feature = "aws-lc-rs")]
fn protected_header(algorithm: Algorithm, kccs: Option<Value>) -> Value {
    let mut entries = vec![
        (
            Value::integer(HEADER_ALGORITHM),
            Value::integer(algorithm.identifier()),
        ),
        (
            Value::integer(HEADER_TYPE),
            Value::TextString(String::from(DPOP_PROOF_CWT_TYPE)),
        ),
    ];
    if let Some(kccs) = kccs {
        entries.push((Value::integer(HEADER_KCCS), kccs));
    }
    Value::Map(entries)
}

/// `kccs` (CWT Claims Set の `cnf` に COSE_Key を置く) を組み立てる
#[cfg(feature = "aws-lc-rs")]
fn kccs(key: Value) -> Value {
    Value::Map(vec![(
        Value::integer(CLAIM_CONFIRMATION),
        Value::Map(vec![(Value::integer(CONFIRMATION_COSE_KEY), key)]),
    )])
}

/// ES256 の公開鍵の COSE_Key を組み立てる
#[cfg(feature = "aws-lc-rs")]
fn es256_cose_key_value() -> Value {
    es256_public_key()
        .to_cose_key_map()
        .expect("公開鍵を COSE_Key へ変換できる")
}

/// ES256 で署名した COSE_Sign1 を組み立てる
///
/// ヘッダの欠落や誤配置など、通常の発行 API では作れない proof を作るために使う。
#[cfg(feature = "aws-lc-rs")]
fn sign_proof(protected: Value, unprotected: Vec<(Value, Value)>, claims: Value) -> Vec<u8> {
    sign_proof_with_key(
        protected,
        unprotected,
        claims,
        &es256_private_key(),
        Algorithm::Es256,
    )
}

/// ES256 の実署名で発行・解析・署名検証のラウンドトリップを行う
#[cfg(feature = "aws-lc-rs")]
#[test]
fn es256_round_trip_preserves_bytes() {
    let crypto = AwsLcRsCrypto::new();
    let codec = codec();
    let mut claims = claims();
    // 名前空間とトラック名は UTF-8 でないバイト列もそのまま保持する
    claims.authorization_context.namespace = Some(vec![b"cdn".to_vec(), vec![0xff, 0x00, 0x80]]);
    claims.authorization_context.track_name = Some(vec![0xff, b'/', 0x00]);
    claims.authorization_context.action = MoqtAction::Subscribe.key();
    claims
        .authorization_context
        .raw
        .push((Value::integer(9), Value::ByteString(vec![0x00, 0x01])));

    let bytes = codec
        .build(&crypto, &es256_private_key(), &claims)
        .expect("CWT 形式の proof を発行できる");
    let proof = codec.decode(&bytes).expect("発行した proof を解析できる");

    assert_eq!(proof.header().algorithm, Algorithm::Es256);
    assert_eq!(proof.header().typ, DPOP_PROOF_CWT_TYPE);
    assert_eq!(proof.header().confirmation_key, es256_public_key());
    assert_eq!(proof.claims(), &claims);
    // actx の名前空間 / トラック名 / action が bytes と整数のまま保たれる
    assert_eq!(
        proof.claims().authorization_context.namespace,
        Some(vec![b"cdn".to_vec(), vec![0xff, 0x00, 0x80]])
    );
    assert_eq!(
        proof.claims().authorization_context.track_name,
        Some(vec![0xff, b'/', 0x00])
    );
    assert_eq!(proof.claims().authorization_context.action, 4);
    proof
        .verify_signature(&crypto)
        .expect("発行した proof の署名を検証できる");
    assert!(!proof.signature().is_empty());
    assert!(!proof.signing_input().is_empty());

    // COSE_Sign1 タグ (18) だけを付け、CWT タグ (61) は付けない
    match CoseMessage::decode(&bytes).expect("COSE メッセージとして読める") {
        CoseMessage::Sign1(message) => {
            assert!(message.cose_tagged);
            assert!(!message.cwt_tagged);
        }
        CoseMessage::Mac0(_) => panic!("COSE_Sign1 として発行した"),
    }
}

/// ES384 と EdDSA の実署名で発行・解析・署名検証のラウンドトリップを行う
#[cfg(feature = "aws-lc-rs")]
#[test]
fn es384_and_eddsa_round_trip() {
    let crypto = AwsLcRsCrypto::new();
    let codec = codec();
    let (es384_private, es384_public) = es384_keys();
    let (ed25519_private, ed25519_public) = ed25519_keys();
    for (private_key, public_key, algorithm) in [
        (es384_private, es384_public, Algorithm::Es384),
        (ed25519_private, ed25519_public, Algorithm::EdDsa),
    ] {
        let bytes = codec
            .build(&crypto, &private_key, &claims())
            .expect("CWT 形式の proof を発行できる");
        let proof = codec.decode(&bytes).expect("発行した proof を解析できる");
        assert_eq!(proof.header().algorithm, algorithm);
        assert_eq!(proof.header().confirmation_key, public_key);
        proof
            .verify_signature(&crypto)
            .expect("発行した proof の署名を検証できる");
    }
    // 曲線の識別子 (COSE_Key の crv) を確認する
    assert_eq!(OkpCurve::Ed25519.identifier(), 6);
}

/// 対称 alg (HMAC) の proof を発行・受理しない
#[cfg(feature = "aws-lc-rs")]
#[test]
fn rejects_symmetric_algorithm() {
    let crypto = AwsLcRsCrypto::new();
    let codec = codec();
    // 発行: 対称鍵では proof を発行しない
    let hmac_key = CoseKey::symmetric(vec![0x11; 32]);
    assert_eq!(
        codec.build(&crypto, &hmac_key, &claims()),
        Err(DpopError::UnsupportedAlgorithm)
    );

    // 受理: COSE_Mac0 は COSE_Sign1 ではない
    let protected = cbor::encode(&protected_header(
        Algorithm::HmacSha256,
        Some(kccs(es256_cose_key_value())),
    ))
    .expect("protected ヘッダをエンコードできる");
    let payload = cbor::encode(
        &codec
            .encode_claims(&claims())
            .expect("クレームをエンコードできる"),
    )
    .expect("claims をエンコードできる");
    let message = CoseMessage::Mac0(shiguredo_moqt::c4m::cose::CoseMac0 {
        protected,
        unprotected: Vec::new(),
        payload: Some(payload),
        tag: vec![0x00; 32],
        cose_tagged: true,
        cwt_tagged: false,
    });
    let bytes = message
        .encode(&CoseEncodingOptions::default())
        .expect("COSE_Mac0 をエンコードできる");
    assert_eq!(codec.decode(&bytes), Err(DpopError::NotCoseSign1));

    // 受理: COSE_Sign1 でも alg が対称なら拒否する
    let protected = cbor::encode(&protected_header(
        Algorithm::HmacSha256,
        Some(kccs(es256_cose_key_value())),
    ))
    .expect("protected ヘッダをエンコードできる");
    let payload = cbor::encode(
        &codec
            .encode_claims(&claims())
            .expect("クレームをエンコードできる"),
    )
    .expect("claims をエンコードできる");
    let message = CoseMessage::Sign1(shiguredo_moqt::c4m::cose::CoseSign1 {
        protected,
        unprotected: Vec::new(),
        payload: Some(payload),
        signature: vec![0x00; 64],
        cose_tagged: true,
        cwt_tagged: false,
    });
    let bytes = message
        .encode(&CoseEncodingOptions::default())
        .expect("COSE_Sign1 をエンコードできる");
    assert_eq!(
        codec.decode(&bytes),
        Err(DpopError::Cose(CoseError::AlgorithmClassMismatch))
    );
}

/// protected ヘッダの必須項目 (alg / typ / kccs) の欠落を拒否する
#[cfg(feature = "aws-lc-rs")]
#[test]
fn rejects_missing_protected_header_members() {
    let codec = codec();
    let claims = codec
        .encode_claims(&claims())
        .expect("クレームをエンコードできる");
    let kccs = kccs(es256_cose_key_value());

    // alg の欠落
    let protected = Value::Map(vec![
        (
            Value::integer(HEADER_TYPE),
            Value::TextString(String::from(DPOP_PROOF_CWT_TYPE)),
        ),
        (Value::integer(HEADER_KCCS), kccs.clone()),
    ]);
    assert_eq!(
        codec.decode(&sign_proof(protected, Vec::new(), claims.clone())),
        Err(DpopError::MissingHeader("alg"))
    );

    // typ の欠落
    let protected = Value::Map(vec![
        (Value::integer(HEADER_ALGORITHM), Value::integer(-7)),
        (Value::integer(HEADER_KCCS), kccs.clone()),
    ]);
    assert_eq!(
        codec.decode(&sign_proof(protected, Vec::new(), claims.clone())),
        Err(DpopError::MissingHeader("typ"))
    );

    // kccs の欠落
    let protected = Value::Map(vec![
        (Value::integer(HEADER_ALGORITHM), Value::integer(-7)),
        (
            Value::integer(HEADER_TYPE),
            Value::TextString(String::from(DPOP_PROOF_CWT_TYPE)),
        ),
    ]);
    assert_eq!(
        codec.decode(&sign_proof(protected, Vec::new(), claims.clone())),
        Err(DpopError::MissingHeader("kccs"))
    );

    // typ の誤り
    let protected = Value::Map(vec![
        (Value::integer(HEADER_ALGORITHM), Value::integer(-7)),
        (
            Value::integer(HEADER_TYPE),
            Value::TextString(String::from("dpop-proof+jwt")),
        ),
        (Value::integer(HEADER_KCCS), kccs),
    ]);
    assert_eq!(
        codec.decode(&sign_proof(protected, Vec::new(), claims)),
        Err(DpopError::InvalidType(String::from("dpop-proof+jwt")))
    );
}

/// protected 必須項目の誤配置 (unprotected ヘッダ) を拒否する
#[cfg(feature = "aws-lc-rs")]
#[test]
fn rejects_misplaced_protected_header_members() {
    let codec = codec();
    let claims = codec
        .encode_claims(&claims())
        .expect("クレームをエンコードできる");
    let kccs = kccs(es256_cose_key_value());

    // kccs を unprotected ヘッダに置く
    let bytes = sign_proof(
        protected_header(Algorithm::Es256, None),
        vec![(Value::integer(HEADER_KCCS), kccs.clone())],
        claims.clone(),
    );
    assert_eq!(
        codec.decode(&bytes),
        Err(DpopError::Cose(CoseError::UnprotectedKccsHeader))
    );

    // typ を unprotected ヘッダに置く
    let bytes = sign_proof(
        Value::Map(vec![
            (Value::integer(HEADER_ALGORITHM), Value::integer(-7)),
            (Value::integer(HEADER_KCCS), kccs),
        ]),
        vec![(
            Value::integer(HEADER_TYPE),
            Value::TextString(String::from(DPOP_PROOF_CWT_TYPE)),
        )],
        claims,
    );
    assert_eq!(
        codec.decode(&bytes),
        Err(DpopError::Cose(CoseError::UnprotectedTypeHeader))
    );
}

/// COSE_Sign1 でない形式と detached payload を拒否する
#[cfg(feature = "aws-lc-rs")]
#[test]
fn rejects_non_sign1_and_detached_payload() {
    let codec = codec();
    // detached payload (payload が null)。署名対象を組み立てられないため署名は付けない
    let bytes = {
        let protected = cbor::encode(&protected_header(
            Algorithm::Es256,
            Some(kccs(es256_cose_key_value())),
        ))
        .expect("protected ヘッダをエンコードできる");
        CoseMessage::Sign1(CoseSign1 {
            protected,
            unprotected: Vec::new(),
            payload: None,
            signature: vec![0x00; 64],
            cose_tagged: true,
            cwt_tagged: false,
        })
        .encode(&CoseEncodingOptions {
            cose_tag: true,
            cwt_tag: false,
        })
        .expect("COSE_Sign1 をエンコードできる")
    };
    assert_eq!(
        codec.decode(&bytes),
        Err(DpopError::Cose(CoseError::DetachedPayloadUnsupported))
    );

    // COSE の構造ではない CBOR
    assert!(matches!(
        codec.decode(&cbor::encode(&Value::Map(Vec::new())).expect("CBOR をエンコードできる")),
        Err(DpopError::Cose(_))
    ));
}

/// `crit` に 14 を含む proof を受理し、未理解の critical label を拒否する
#[cfg(feature = "aws-lc-rs")]
#[test]
fn accepts_understood_critical_header() {
    let crypto = AwsLcRsCrypto::new();
    let codec = codec();
    let claims = codec
        .encode_claims(&claims())
        .expect("クレームをエンコードできる");
    let kccs = kccs(es256_cose_key_value());

    // crit に kccs (14) を含む protected ヘッダは受理する
    let protected = Value::Map(vec![
        (Value::integer(HEADER_ALGORITHM), Value::integer(-7)),
        (
            Value::integer(HEADER_TYPE),
            Value::TextString(String::from(DPOP_PROOF_CWT_TYPE)),
        ),
        (Value::integer(HEADER_KCCS), kccs.clone()),
        (
            Value::integer(HEADER_CRITICAL),
            Value::Array(vec![Value::integer(HEADER_KCCS)]),
        ),
    ]);
    let bytes = sign_proof(protected, Vec::new(), claims.clone());
    let proof = codec
        .decode(&bytes)
        .expect("crit に kccs を含む proof を解析できる");
    proof
        .verify_signature(&crypto)
        .expect("crit に kccs を含む proof の署名を検証できる");

    // crit に kccs を含むのに kccs が無い場合は拒否する
    let protected = Value::Map(vec![
        (Value::integer(HEADER_ALGORITHM), Value::integer(-7)),
        (
            Value::integer(HEADER_TYPE),
            Value::TextString(String::from(DPOP_PROOF_CWT_TYPE)),
        ),
        (
            Value::integer(HEADER_CRITICAL),
            Value::Array(vec![Value::integer(HEADER_KCCS)]),
        ),
    ]);
    assert_eq!(
        codec.decode(&sign_proof(protected, Vec::new(), claims.clone())),
        Err(DpopError::Cose(CoseError::CriticalHeaderNotPresent))
    );

    // 未理解の critical label は拒否する
    let protected = Value::Map(vec![
        (Value::integer(HEADER_ALGORITHM), Value::integer(-7)),
        (
            Value::integer(HEADER_TYPE),
            Value::TextString(String::from(DPOP_PROOF_CWT_TYPE)),
        ),
        (Value::integer(HEADER_KCCS), kccs),
        (
            Value::integer(HEADER_CRITICAL),
            Value::Array(vec![Value::integer(99)]),
        ),
    ]);
    assert_eq!(
        codec.decode(&sign_proof(protected, Vec::new(), claims)),
        Err(DpopError::Cose(CoseError::UnsupportedCriticalHeader))
    );
}

/// `kccs` の秘密鍵パラメータと未知のパラメータを拒否する
#[cfg(feature = "aws-lc-rs")]
#[test]
fn rejects_private_and_unknown_cose_key_parameters() {
    let codec = codec();
    let claims = codec
        .encode_claims(&claims())
        .expect("クレームをエンコードできる");
    let ec2 = es256_cose_key_value();
    let Value::Map(mut ec2_entries) = ec2 else {
        panic!("COSE_Key はマップ")
    };

    // 秘密鍵パラメータ (d = -4)
    let mut entries = ec2_entries.clone();
    entries.push((
        Value::integer(-4),
        Value::ByteString(decode_hex(ES256_PRIVATE_KEY_HEX)),
    ));
    let protected = protected_header(Algorithm::Es256, Some(kccs(Value::Map(entries))));
    assert_eq!(
        codec.decode(&sign_proof(protected, Vec::new(), claims.clone())),
        Err(DpopError::Cose(CoseError::PrivateKeyParameter(-4)))
    );

    // 公開鍵の復元に不要な任意属性 (kid = 2) は無視して受理する (RFC 8747 §3.2)
    let mut entries = ec2_entries.clone();
    entries.push((Value::integer(2), Value::ByteString(b"key-1".to_vec())));
    let protected = protected_header(Algorithm::Es256, Some(kccs(Value::Map(entries))));
    let proof = codec
        .decode(&sign_proof(protected, Vec::new(), claims.clone()))
        .expect("kid 付き COSE_Key を受理する");
    assert_eq!(proof.header().algorithm, Algorithm::Es256);

    // 対称鍵 (kty = 4)
    let protected = protected_header(
        Algorithm::Es256,
        Some(kccs(Value::Map(vec![
            (Value::integer(1), Value::integer(4)),
            (Value::integer(-1), Value::ByteString(vec![0x11; 32])),
        ]))),
    );
    assert_eq!(
        codec.decode(&sign_proof(protected, Vec::new(), claims.clone())),
        Err(DpopError::Cose(CoseError::SymmetricKeyNotAllowed))
    );

    // 対応していない kty
    let protected = protected_header(
        Algorithm::Es256,
        Some(kccs(Value::Map(vec![(
            Value::integer(1),
            Value::integer(3),
        )]))),
    );
    assert_eq!(
        codec.decode(&sign_proof(protected, Vec::new(), claims.clone())),
        Err(DpopError::Cose(CoseError::UnsupportedKeyType(3)))
    );

    // x の欠落
    ec2_entries.retain(|(key, _)| key.as_int() != Some(-2));
    let protected = protected_header(
        Algorithm::Es256,
        Some(kccs(Value::Map(ec2_entries.clone()))),
    );
    assert_eq!(
        codec.decode(&sign_proof(protected, Vec::new(), claims.clone())),
        Err(DpopError::Cose(CoseError::MissingKeyParameter("x")))
    );

    // x の長さ違反
    ec2_entries.push((Value::integer(-2), Value::ByteString(vec![0x00; 3])));
    let protected = protected_header(Algorithm::Es256, Some(kccs(Value::Map(ec2_entries))));
    assert_eq!(
        codec.decode(&sign_proof(protected, Vec::new(), claims.clone())),
        Err(DpopError::Cose(CoseError::InvalidKeyLength("x")))
    );

    // cnf の欠落
    let protected = protected_header(Algorithm::Es256, Some(Value::Map(Vec::new())));
    assert_eq!(
        codec.decode(&sign_proof(protected, Vec::new(), claims.clone())),
        Err(DpopError::MissingKccsConfirmation)
    );

    // cnf に COSE_Key が無い
    let protected = protected_header(
        Algorithm::Es256,
        Some(Value::Map(vec![(
            Value::integer(CLAIM_CONFIRMATION),
            Value::Map(Vec::new()),
        )])),
    );
    assert_eq!(
        codec.decode(&sign_proof(protected, Vec::new(), claims)),
        Err(DpopError::MissingConfirmationKey)
    );
}

/// 重複した claim label を持つ proof を拒否する
#[cfg(feature = "aws-lc-rs")]
#[test]
fn proof_rejects_duplicate_claims() {
    let crypto = AwsLcRsCrypto::new();
    let codec = codec();
    let bytes = codec
        .build(&crypto, &es256_private_key(), &claims())
        .expect("CWT 形式の proof を発行できる");
    let CoseMessage::Sign1(sign1) =
        CoseMessage::decode(&bytes).expect("COSE メッセージとして読める")
    else {
        panic!("COSE_Sign1 として発行した")
    };
    // {6: 1, 6: 2} のように iat が重複したペイロードへ差し替える
    let duplicated = decode_hex("a206010602");
    let bytes = CoseMessage::Sign1(CoseSign1 {
        protected: sign1.protected,
        unprotected: Vec::new(),
        payload: Some(duplicated),
        signature: sign1.signature,
        cose_tagged: true,
        cwt_tagged: false,
    })
    .encode(&CoseEncodingOptions {
        cose_tag: true,
        cwt_tag: false,
    })
    .expect("COSE_Sign1 をエンコードできる");
    assert_eq!(
        codec.decode(&bytes),
        Err(DpopError::Cose(CoseError::Cbor(CborError::DuplicateMapKey)))
    );
}

/// 改ざんされた proof と別鍵で署名された proof の署名検証を拒否する
#[cfg(feature = "aws-lc-rs")]
#[test]
fn rejects_tampered_or_wrong_key_signature() {
    let crypto = AwsLcRsCrypto::new();
    let codec = codec();
    let bytes = codec
        .build(&crypto, &es256_private_key(), &claims())
        .expect("CWT 形式の proof を発行できる");
    let message = CoseMessage::decode(&bytes).expect("COSE メッセージとして読める");
    let CoseMessage::Sign1(sign1) = message else {
        panic!("COSE_Sign1 として発行した")
    };

    // claims を改ざんしても署名は元のまま
    let mut tampered_claims = codec.decode(&bytes).expect("解析できる").claims().clone();
    tampered_claims.issued_at += 1;
    let payload = cbor::encode(
        &codec
            .encode_claims(&tampered_claims)
            .expect("クレームをエンコードできる"),
    )
    .expect("claims をエンコードできる");
    let tampered = CoseMessage::Sign1(CoseSign1 {
        protected: sign1.protected.clone(),
        unprotected: Vec::new(),
        payload: Some(payload),
        signature: sign1.signature.clone(),
        cose_tagged: true,
        cwt_tagged: false,
    })
    .encode(&CoseEncodingOptions {
        cose_tag: true,
        cwt_tag: false,
    })
    .expect("COSE_Sign1 をエンコードできる");
    let tampered = codec
        .decode(&tampered)
        .expect("改ざんされた proof もデコードできる");
    assert_eq!(
        tampered.verify_signature(&crypto),
        Err(DpopError::Crypto(CryptoError::SignatureVerificationFailed))
    );

    // 埋め込む公開鍵だけを別の鍵に差し替え、署名は元の鍵のまま
    // (y 座標を変えた P-256 の公開鍵を埋め込み、署名の検証を失敗させる)
    let mut other_y = decode_hex(ES256_PUBLIC_KEY_Y_HEX);
    other_y[0] ^= 0x01;
    let other_public_key = CoseKey::ec2(EcCurve::P256, decode_hex(ES256_PUBLIC_KEY_X_HEX), other_y);
    let protected = protected_header(
        Algorithm::Es256,
        Some(kccs(
            other_public_key
                .to_cose_key_map()
                .expect("公開鍵を COSE_Key へ変換できる"),
        )),
    );
    let wrong_key = sign_proof_with_key(
        protected,
        Vec::new(),
        codec
            .encode_claims(&claims())
            .expect("クレームをエンコードできる"),
        &es256_private_key(),
        Algorithm::Es256,
    );
    let wrong_key = codec
        .decode(&wrong_key)
        .expect("別鍵を埋め込んだ proof もデコードできる");
    assert_eq!(
        wrong_key.verify_signature(&crypto),
        Err(DpopError::Crypto(CryptoError::SignatureVerificationFailed))
    );
}

/// 指定した鍵とアルゴリズムで COSE_Sign1 を組み立てる
#[cfg(feature = "aws-lc-rs")]
fn sign_proof_with_key(
    protected: Value,
    unprotected: Vec<(Value, Value)>,
    claims: Value,
    key: &CoseKey,
    algorithm: Algorithm,
) -> Vec<u8> {
    let crypto = AwsLcRsCrypto::new();
    let protected = cbor::encode(&protected).expect("protected ヘッダをエンコードできる");
    let payload = cbor::encode(&claims).expect("claims をエンコードできる");
    let mut message = CoseMessage::Sign1(CoseSign1 {
        protected,
        unprotected,
        payload: Some(payload),
        signature: Vec::new(),
        cose_tagged: true,
        cwt_tagged: false,
    });
    let signing_input = message.signing_input().expect("署名対象を組み立てられる");
    let signature = crypto
        .sign(algorithm, key, &signing_input)
        .expect("署名できる");
    if let CoseMessage::Sign1(sign1) = &mut message {
        sign1.signature = signature;
    }
    message
        .encode(&CoseEncodingOptions {
            cose_tag: true,
            cwt_tag: false,
        })
        .expect("COSE メッセージをエンコードできる")
}

/// MOQT 以外の `actx.type` を持つ proof を発行しないこと
#[cfg(feature = "aws-lc-rs")]
#[test]
fn build_rejects_non_moqt_context_type() {
    let crypto = AwsLcRsCrypto::new();
    let codec = codec();
    let mut claims = claims();
    claims.authorization_context.context_type = String::from("other");
    assert_eq!(
        codec.build(&crypto, &es256_private_key(), &claims),
        Err(DpopError::ContextTypeMismatch)
    );
}

/// 秘密鍵を含まない鍵では proof を発行しないこと
#[cfg(feature = "aws-lc-rs")]
#[test]
fn build_requires_private_key() {
    let crypto = AwsLcRsCrypto::new();
    let codec = codec();
    assert_eq!(
        codec.build(&crypto, &es256_public_key(), &claims()),
        Err(DpopError::Crypto(
            shiguredo_moqt::c4m::crypto::CryptoError::MissingPrivateKey
        ))
    );
}

/// kccs / cnf / COSE_Key が map でない場合は拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn rejects_non_map_kccs_and_confirmation_key() {
    let codec = codec();
    let claims = codec
        .encode_claims(&claims())
        .expect("クレームをエンコードできる");

    // kccs が map でない
    let protected = protected_header(
        Algorithm::Es256,
        Some(Value::TextString(String::from("kccs"))),
    );
    assert_eq!(
        codec.decode(&sign_proof(protected, Vec::new(), claims.clone())),
        Err(DpopError::UnexpectedType("kccs"))
    );

    // cnf が map でない
    let protected = protected_header(
        Algorithm::Es256,
        Some(Value::Map(vec![(
            Value::integer(CLAIM_CONFIRMATION),
            Value::TextString(String::from("cnf")),
        )])),
    );
    assert_eq!(
        codec.decode(&sign_proof(protected, Vec::new(), claims.clone())),
        Err(DpopError::UnexpectedType("cnf"))
    );

    // COSE_Key が map でない
    let protected = protected_header(
        Algorithm::Es256,
        Some(kccs(Value::TextString(String::from("key")))),
    );
    assert_eq!(
        codec.decode(&sign_proof(protected, Vec::new(), claims)),
        Err(DpopError::Cose(CoseError::UnexpectedType("COSE_Key")))
    );
}

/// COSE_Sign1 のタグの有無と CWT タグの有無にかかわらず解析できること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn accepts_tagged_untagged_and_cwt_tagged_sign1() {
    let codec = codec();
    let claims = codec
        .encode_claims(&claims())
        .expect("クレームをエンコードできる");
    let protected = cbor::encode(&protected_header(
        Algorithm::Es256,
        Some(kccs(es256_cose_key_value())),
    ))
    .expect("protected ヘッダをエンコードできる");
    let payload = cbor::encode(&claims).expect("claims をエンコードできる");
    for (cose_tag, cwt_tag) in [(true, false), (false, false), (true, true)] {
        let message = CoseMessage::Sign1(shiguredo_moqt::c4m::cose::CoseSign1 {
            protected: protected.clone(),
            unprotected: Vec::new(),
            payload: Some(payload.clone()),
            signature: vec![0x00; 64],
            cose_tagged: cose_tag,
            cwt_tagged: cwt_tag,
        });
        let bytes = message
            .encode(&CoseEncodingOptions { cose_tag, cwt_tag })
            .expect("COSE_Sign1 をエンコードできる");
        let proof = codec
            .decode(&bytes)
            .unwrap_or_else(|error| panic!("cose_tag={cose_tag}, cwt_tag={cwt_tag}: {error}"));
        assert_eq!(proof.header().algorithm, Algorithm::Es256);
    }
}

/// actx の既知ラベルを raw に重複させると拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn claims_reject_duplicate_actx_members() {
    let codec = codec();
    let mut claims = claims();
    // 既知ラベル (action = 1) を raw に置くと、エンコード時の重複になる
    claims
        .authorization_context
        .raw
        .push((Value::integer(1), Value::integer(4)));
    assert_eq!(
        codec.encode_claims(&claims),
        Err(DpopError::DuplicateClaim(1))
    );

    // decode でも actx 内の重複ラベルを拒否する。wire 入力では CBOR デコーダが先に
    // 拒否するため、これは公開 decode_claims に手組み Value を渡した場合の防御である
    let mut entries = match claims_value() {
        Value::Map(entries) => entries,
        _ => panic!("クレームはマップ"),
    };
    for (_, value) in &mut entries {
        if value.as_map().is_some() {
            *value = Value::Map(vec![
                (Value::integer(0), Value::TextString(String::from("moqt"))),
                (Value::integer(1), Value::integer(4)),
                (Value::integer(1), Value::integer(6)),
            ]);
        }
    }
    assert_eq!(
        codec.decode_claims(&Value::Map(entries)),
        Err(DpopError::DuplicateClaim(1))
    );
}

/// `key_ops` の文字列 "verify" を受理し、COSE_Key 以外の confirmation method を無視し、
/// 整数でもテキストでもない claim key を拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn accepts_verify_operation_and_ignores_other_confirmation_methods() {
    let codec = codec();
    let claims = codec
        .encode_claims(&claims())
        .expect("クレームをエンコードできる");
    let Value::Map(mut key_entries) = es256_cose_key_value() else {
        panic!("COSE_Key はマップ");
    };
    key_entries.push((
        Value::integer(4),
        Value::Array(vec![Value::TextString(String::from("verify"))]),
    ));
    // cnf に COSE_Key 以外の confirmation method を併記しても無視する
    let kccs_value = Value::Map(vec![(
        Value::integer(CLAIM_CONFIRMATION),
        Value::Map(vec![
            (Value::integer(1), Value::Map(key_entries)),
            (Value::integer(2), Value::ByteString(vec![0x11; 32])),
        ]),
    )]);
    let protected = protected_header(Algorithm::Es256, Some(kccs_value));
    codec
        .decode(&sign_proof(protected, Vec::new(), claims.clone()))
        .expect("key_ops の verify と他の confirmation method を受理する");

    // 整数でもテキストでもない claim key は拒否する
    let mut entries = match claims {
        Value::Map(entries) => entries,
        _ => panic!("クレームはマップ"),
    };
    entries.push((Value::ByteString(b"raw-key".to_vec()), Value::integer(1)));
    assert_eq!(
        codec.decode_claims(&Value::Map(entries)),
        Err(DpopError::UnexpectedType("claim key"))
    );
}

// ---------------------------------------------------------------------------
// cnf の ckt / jkt による鍵束縛 (draft-ietf-moq-c4m-02 §4.1 / §4.3)
// ---------------------------------------------------------------------------

/// 実 ES256 鍵で発行した CWT 形式の proof と、その公開鍵の ckt / jkt を返す
#[cfg(feature = "aws-lc-rs")]
fn es256_proof_with_thumbprints() -> (CwtDpopProof, Vec<u8>, Vec<u8>) {
    let crypto = AwsLcRsCrypto::new();
    let codec = codec();
    let bytes = codec
        .build(&crypto, &es256_private_key(), &claims())
        .expect("CWT 形式の proof を発行できる");
    let proof = codec.decode(&bytes).expect("発行した proof を解析できる");
    let public_key = proof.header().confirmation_key.clone();
    let ckt = public_key
        .cose_key_thumbprint_sha256(&crypto)
        .expect("ckt を計算できる");
    let jkt = Jwk::from_cose_key(&public_key)
        .expect("公開鍵を等価な JWK へ変換できる")
        .thumbprint_sha256(&crypto)
        .expect("jkt を計算できる");
    (proof, ckt, jkt)
}

/// ckt だけ / jkt だけ / 両方の各組み合わせで束縛を検証すること
///
/// `ckt` と `jkt` が両方存在する場合は両方の一致を要求し、片方だけの一致では受理
/// しない。key 3 と未知の method は認可の根拠にしない。
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cnf_binding_requires_every_present_thumbprint_to_match() {
    let crypto = AwsLcRsCrypto::new();
    let (proof, ckt, jkt) = es256_proof_with_thumbprints();
    let mismatched = vec![0x5c; 32];

    // ckt のみ一致
    let confirmation = Confirmation {
        cose_key_thumbprint: Some(ckt.clone()),
        ..Confirmation::default()
    };
    proof
        .verify_key_binding(&crypto, &confirmation)
        .expect("ckt のみ一致すれば受理する");

    // jkt のみ一致
    let confirmation = Confirmation {
        jwk_thumbprint: Some(jkt.clone()),
        ..Confirmation::default()
    };
    proof
        .verify_key_binding(&crypto, &confirmation)
        .expect("jkt のみ一致すれば受理する");

    // 両方一致
    let confirmation = Confirmation {
        jwk_thumbprint: Some(jkt.clone()),
        cose_key_thumbprint: Some(ckt.clone()),
        c4m_draft_jwk_thumbprint: None,
        raw: Vec::new(),
    };
    proof
        .verify_key_binding(&crypto, &confirmation)
        .expect("ckt と jkt の両方が一致すれば受理する");

    // ckt 一致 + jkt 不一致
    let confirmation = Confirmation {
        jwk_thumbprint: Some(mismatched.clone()),
        cose_key_thumbprint: Some(ckt.clone()),
        ..Confirmation::default()
    };
    assert_eq!(
        proof.verify_key_binding(&crypto, &confirmation),
        Err(DpopError::KeyBindingMismatch),
        "jkt が不一致なのに ckt の一致だけで受理した"
    );

    // ckt 不一致 + jkt 一致
    let confirmation = Confirmation {
        jwk_thumbprint: Some(jkt.clone()),
        cose_key_thumbprint: Some(mismatched.clone()),
        ..Confirmation::default()
    };
    assert_eq!(
        proof.verify_key_binding(&crypto, &confirmation),
        Err(DpopError::KeyBindingMismatch),
        "ckt が不一致なのに jkt の一致だけで受理した"
    );

    // ckt のみ不一致
    let confirmation = Confirmation {
        cose_key_thumbprint: Some(mismatched.clone()),
        ..Confirmation::default()
    };
    assert_eq!(
        proof.verify_key_binding(&crypto, &confirmation),
        Err(DpopError::KeyBindingMismatch)
    );

    // jkt のみ不一致
    let confirmation = Confirmation {
        jwk_thumbprint: Some(mismatched),
        ..Confirmation::default()
    };
    assert_eq!(
        proof.verify_key_binding(&crypto, &confirmation),
        Err(DpopError::KeyBindingMismatch)
    );

    // どちらも無い
    assert_eq!(
        proof.verify_key_binding(&crypto, &Confirmation::default()),
        Err(DpopError::MissingThumbprint)
    );
}

/// 未知の confirmation method を認可の根拠にしないこと
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cnf_binding_rejects_unknown_confirmation_methods() {
    let crypto = AwsLcRsCrypto::new();
    let (proof, ckt, jkt) = es256_proof_with_thumbprints();
    // IANA のレジストリに無い confirmation method のラベル
    let unknown = |value: Vec<u8>| (Value::integer(999), Value::ByteString(value));

    // 未知の method だけの cnf は拒否する
    let confirmation = Confirmation {
        raw: vec![unknown(ckt.clone())],
        ..Confirmation::default()
    };
    assert_eq!(
        proof.verify_key_binding(&crypto, &confirmation),
        Err(DpopError::MissingThumbprint),
        "未知の confirmation method だけで受理した"
    );

    // 空の cnf も拒否する
    assert_eq!(
        proof.verify_key_binding(&crypto, &Confirmation::default()),
        Err(DpopError::MissingThumbprint)
    );

    // 既知の method と未知の method が併存する場合、未知の method は raw に保持され、
    // 認可の根拠にはならない。既知の ckt が一致すれば受理する
    let confirmation = Confirmation {
        cose_key_thumbprint: Some(ckt.clone()),
        raw: vec![unknown(jkt.clone())],
        ..Confirmation::default()
    };
    proof
        .verify_key_binding(&crypto, &confirmation)
        .expect("既知の ckt が一致すれば未知の method が併存しても受理する");

    // 既知の ckt が不一致なら、未知の method が一致していても拒否する
    let confirmation = Confirmation {
        cose_key_thumbprint: Some(vec![0x7f; 32]),
        raw: vec![unknown(ckt)],
        ..Confirmation::default()
    };
    assert_eq!(
        proof.verify_key_binding(&crypto, &confirmation),
        Err(DpopError::KeyBindingMismatch),
        "未知の method の一致で ckt の不一致を救済した"
    );

    // ドラフトのベクタが jkt に使う key 3 は、この API の認可の根拠にしない
    let confirmation = Confirmation {
        c4m_draft_jwk_thumbprint: Some(jkt),
        ..Confirmation::default()
    };
    assert_eq!(
        proof.verify_key_binding(&crypto, &confirmation),
        Err(DpopError::MissingThumbprint),
        "key 3 だけの cnf で受理した"
    );
}

/// 実 ES256 の proof と実 cnf で束縛の成功と失敗を確認すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn es256_proof_binds_to_a_real_cnf_claim() {
    let crypto = AwsLcRsCrypto::new();
    let (proof, ckt, _) = es256_proof_with_thumbprints();
    proof
        .verify_signature(&crypto)
        .expect("proof の署名を検証できる");

    // ckt を実際の CAT トークンの cnf に載せ、デコードした cnf で束縛を検証する
    let token_text = CatTokenBuilder::new()
        .issuer("https://auth.example.com")
        .cose_key_thumbprint(ckt)
        .build_compact(&crypto, &CoseKey::symmetric(decode_hex(HMAC_KEY_HEX)))
        .expect("CAT トークンを発行できる");
    let token = CatToken::decode(token_text.as_bytes()).expect("CAT トークンをデコードできる");
    let confirmation = token
        .claims()
        .confirmation
        .as_ref()
        .expect("発行したトークンに cnf がある");
    proof
        .verify_key_binding(&crypto, confirmation)
        .expect("実 cnf の ckt に束縛できる");

    // 別の鍵 (ES384) で発行した proof は ckt が一致しないため拒否する
    let (es384_private, _) = es384_keys();
    let bytes = codec()
        .build(&crypto, &es384_private, &claims())
        .expect("ES384 の CWT 形式の proof を発行できる");
    let other_proof = codec()
        .decode(&bytes)
        .expect("発行した ES384 の proof を解析できる");
    other_proof
        .verify_signature(&crypto)
        .expect("ES384 の proof の署名を検証できる");
    assert_eq!(
        other_proof.verify_key_binding(&crypto, confirmation),
        Err(DpopError::KeyBindingMismatch),
        "異なる鍵の proof が ckt に束縛された"
    );
}

/// ES384 / EdDSA の proof でも cnf の ckt に束縛できること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn es384_and_eddsa_proofs_bind_to_a_cnf_claim() {
    let crypto = AwsLcRsCrypto::new();
    let codec = codec();
    let (es384_private, _) = es384_keys();
    let (ed25519_private, _) = ed25519_keys();
    for (private_key, algorithm) in [
        (es384_private, Algorithm::Es384),
        (ed25519_private, Algorithm::EdDsa),
    ] {
        let bytes = codec
            .build(&crypto, &private_key, &claims())
            .expect("CWT 形式の proof を発行できる");
        let proof = codec.decode(&bytes).expect("proof を解析できる");
        let ckt = proof
            .header()
            .confirmation_key
            .cose_key_thumbprint_sha256(&crypto)
            .expect("ckt を計算できる");
        let confirmation = Confirmation {
            cose_key_thumbprint: Some(ckt),
            ..Confirmation::default()
        };
        proof
            .verify_key_binding(&crypto, &confirmation)
            .unwrap_or_else(|error| {
                panic!("algorithm={algorithm:?} の束縛検証に失敗した: {error}")
            });
        // 別の鍵の thumbprint では拒否する
        let other = Confirmation {
            cose_key_thumbprint: Some(vec![0x5c; 32]),
            ..Confirmation::default()
        };
        assert_eq!(
            proof.verify_key_binding(&crypto, &other),
            Err(DpopError::KeyBindingMismatch),
            "algorithm={algorithm:?} の不一致を受理した"
        );
    }
}

/// draft §4.2.1 と付録 A.4 の固定値で、実 ES256 proof が cnf に束縛されること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn known_vector_cnf_binds_to_es256_proof() {
    let (ckt_hex, jkt_hex) = (
        super::super::vectors::CKT_DRAFT_C4M_DOPP_PROOF_HEX,
        super::super::vectors::JKT_DRAFT_C4M_ES256_HEX,
    );
    let crypto = AwsLcRsCrypto::new();
    let codec = codec();
    let bytes = codec
        .build(&crypto, &es256_private_key(), &claims())
        .expect("CWT 形式の proof を発行できる");
    let proof = codec.decode(&bytes).expect("proof を解析できる");

    // ckt (draft §4.2.1 の連携鍵のサムプリント)
    let confirmation = Confirmation {
        cose_key_thumbprint: Some(decode_hex(ckt_hex)),
        ..Confirmation::default()
    };
    proof
        .verify_key_binding(&crypto, &confirmation)
        .expect("固定ベクタの ckt と一致する");

    // jkt (付録 A.4 の JWK サムプリント)
    let confirmation = Confirmation {
        jwk_thumbprint: Some(decode_hex(jkt_hex)),
        ..Confirmation::default()
    };
    proof
        .verify_key_binding(&crypto, &confirmation)
        .expect("固定ベクタの jkt と一致する");

    // 両方を載せても同じ鍵として受理する
    let confirmation = Confirmation {
        cose_key_thumbprint: Some(decode_hex(ckt_hex)),
        jwk_thumbprint: Some(decode_hex(jkt_hex)),
        ..Confirmation::default()
    };
    proof
        .verify_key_binding(&crypto, &confirmation)
        .expect("固定ベクタの ckt と jkt が併存しても受理する");

    // 1 バイトでも違えば拒否する
    let mut mismatched = decode_hex(ckt_hex);
    mismatched[0] ^= 0xff;
    let confirmation = Confirmation {
        cose_key_thumbprint: Some(mismatched),
        ..Confirmation::default()
    };
    assert_eq!(
        proof.verify_key_binding(&crypto, &confirmation),
        Err(DpopError::KeyBindingMismatch)
    );
}

/// EdDSA proof でも jkt に束縛できること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn eddsa_proof_binds_with_jkt() {
    let crypto = AwsLcRsCrypto::new();
    let codec = codec();
    let (ed25519_private, _) = ed25519_keys();
    let bytes = codec
        .build(&crypto, &ed25519_private, &claims())
        .expect("EdDSA の proof を発行できる");
    let proof = codec.decode(&bytes).expect("proof を解析できる");
    let jkt = Jwk::from_cose_key(&proof.header().confirmation_key)
        .expect("公開鍵を等価な JWK へ変換できる")
        .thumbprint_sha256(&crypto)
        .expect("jkt を計算できる");
    let confirmation = Confirmation {
        jwk_thumbprint: Some(jkt),
        ..Confirmation::default()
    };
    proof
        .verify_key_binding(&crypto, &confirmation)
        .expect("EdDSA proof が jkt に束縛される");
    let mismatched = Confirmation {
        jwk_thumbprint: Some(vec![0x5c; 32]),
        ..Confirmation::default()
    };
    assert_eq!(
        proof.verify_key_binding(&crypto, &mismatched),
        Err(DpopError::KeyBindingMismatch)
    );
}

/// 解釈しない confirmation method (COSE_Key) は認可の根拠にしないこと
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cnf_binding_ignores_uninterpreted_confirmation_methods() {
    let crypto = AwsLcRsCrypto::new();
    let (proof, ckt, _) = es256_proof_with_thumbprints();
    // ckt は一致するが、別の鍵を指す COSE_Key (1) は解釈しないため無視する
    let other_key = CoseKey::ec2(EcCurve::P256, vec![0x11; 32], vec![0x22; 32]);
    let confirmation = Confirmation {
        cose_key_thumbprint: Some(ckt),
        raw: vec![(
            Value::integer(1),
            other_key
                .to_cose_key_map()
                .expect("別の COSE_Key を組み立てられる"),
        )],
        ..Confirmation::default()
    };
    proof
        .verify_key_binding(&crypto, &confirmation)
        .expect("解釈しない method は束縛の判定に使わない");

    // COSE_Key だけの cnf は解釈しないため拒否する
    let confirmation = Confirmation {
        raw: vec![(
            Value::integer(1),
            other_key
                .to_cose_key_map()
                .expect("別の COSE_Key を組み立てられる"),
        )],
        ..Confirmation::default()
    };
    assert_eq!(
        proof.verify_key_binding(&crypto, &confirmation),
        Err(DpopError::MissingThumbprint)
    );
}

#[path = "cwt/verification.rs"]
mod verification;
