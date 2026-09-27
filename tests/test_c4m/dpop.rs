//! DPoP proof (draft-nandakumar-moq-generic-dpop-proof) のテスト

use base64ct::{Base64UrlUnpadded, Encoding};
use shiguredo_moqt::c4m::MoqtAction;
use shiguredo_moqt::c4m::dpop::{
    AuthorizationContext, DpopError, DpopProof, DpopReplayCache, MOQT_AUTHORIZATION_CONTEXT_TYPE,
};
use shiguredo_moqt::message::common::TrackNamespace;

#[cfg(feature = "aws-lc-rs")]
use super::helpers::decode_hex;
#[cfg(feature = "aws-lc-rs")]
use super::vectors::{
    ES256_PRIVATE_KEY_HEX, ES256_PUBLIC_KEY_X_HEX, ES256_PUBLIC_KEY_Y_HEX, HMAC_KEY_HEX,
};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::cat::{CatToken, CatTokenBuilder, Confirmation};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::crypto::{CoseCrypto, CoseKey};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::dpop::{DPOP_PROOF_JWT_TYPE, DpopProofBuilder, DpopVerification};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::jwk::Jwk;
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::{MoqtClaim, MoqtScope};

/// パディング無しの base64url でエンコードする
fn base64url(bytes: &[u8]) -> String {
    Base64UrlUnpadded::encode_string(bytes)
}

/// テスト用の ES256 公開鍵 JWK
#[cfg(feature = "aws-lc-rs")]
fn es256_jwk() -> Jwk {
    Jwk::Ec {
        curve: String::from("P-256"),
        x: base64url(&decode_hex(ES256_PUBLIC_KEY_X_HEX)),
        y: base64url(&decode_hex(ES256_PUBLIC_KEY_Y_HEX)),
    }
}

/// テスト用の namespace
fn namespace() -> TrackNamespace {
    TrackNamespace::new(vec![b"example.com".to_vec()]).expect("名前空間を作れる")
}

/// テスト用の Authorization Context
fn authorization_context() -> AuthorizationContext {
    AuthorizationContext {
        context_type: String::from(MOQT_AUTHORIZATION_CONTEXT_TYPE),
        action: String::from("PUB_NS"),
        track_namespace: String::from("example.2ecom"),
        track_name: Some(String::from("live")),
        resource: Some(String::from(
            "moqt://relay.example.com?tns=example.2ecom&tn=live",
        )),
        raw: String::new(),
    }
}

/// 署名なしの JWT を組み立てる (デコードのテスト用)
fn unsigned_jwt(header_json: &str, payload_json: &str) -> String {
    format!(
        "{}.{}.{}",
        base64url(header_json.as_bytes()),
        base64url(payload_json.as_bytes()),
        base64url(b"signature")
    )
}

#[test]
fn decode_errors() {
    // typ が違う
    assert_eq!(
        DpopProof::decode(&unsigned_jwt(
            r#"{"alg":"ES256","typ":"JWT","jwk":{"kty":"EC","crv":"P-256","x":"AQ","y":"AQ"}}"#,
            r#"{"jti":"id","iat":1,"actx":{"type":"moqt","action":"PUB_NS","tns":"a","tn":"b"}}"#,
        )),
        Err(DpopError::InvalidType(String::from("JWT")))
    );
    // typ が無い
    assert_eq!(
        DpopProof::decode(&unsigned_jwt(
            r#"{"alg":"ES256","jwk":{"kty":"EC","crv":"P-256","x":"AQ","y":"AQ"}}"#,
            r#"{"jti":"id","iat":1,"actx":{"type":"moqt","action":"PUB_NS","tns":"a","tn":"b"}}"#,
        )),
        Err(DpopError::MissingHeader("typ"))
    );
    // jwk が無い
    assert_eq!(
        DpopProof::decode(&unsigned_jwt(
            r#"{"alg":"ES256","typ":"dpop-proof+jwt"}"#,
            r#"{"jti":"id","iat":1,"actx":{"type":"moqt","action":"PUB_NS","tns":"a","tn":"b"}}"#,
        )),
        Err(DpopError::MissingHeader("jwk"))
    );
    // 対称鍵アルゴリズムは使えない
    assert_eq!(
        DpopProof::decode(&unsigned_jwt(
            r#"{"alg":"HS256","typ":"dpop-proof+jwt","jwk":{"kty":"EC","crv":"P-256","x":"AQ","y":"AQ"}}"#,
            r#"{"jti":"id","iat":1,"actx":{"type":"moqt","action":"PUB_NS","tns":"a","tn":"b"}}"#,
        )),
        Err(DpopError::UnsupportedAlgorithm)
    );
    // actx が無い
    assert!(matches!(
        DpopProof::decode(&unsigned_jwt(
            r#"{"alg":"ES256","typ":"dpop-proof+jwt","jwk":{"kty":"EC","crv":"P-256","x":"AQ","y":"AQ"}}"#,
            r#"{"jti":"id","iat":1}"#,
        )),
        Err(DpopError::Json(_))
    ));
    // jti が無い
    assert!(matches!(
        DpopProof::decode(&unsigned_jwt(
            r#"{"alg":"ES256","typ":"dpop-proof+jwt","jwk":{"kty":"EC","crv":"P-256","x":"AQ","y":"AQ"}}"#,
            r#"{"iat":1,"actx":{"type":"moqt","action":"PUB_NS","tns":"a","tn":"b"}}"#,
        )),
        Err(DpopError::Json(_))
    ));
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn build_and_verify_es256_proof() {
    use shiguredo_moqt::c4m::cose::Algorithm;
    use shiguredo_moqt::c4m::crypto::EcCurve;
    use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;

    let crypto = AwsLcRsCrypto::new();
    let jwk = es256_jwk();
    let key = CoseKey::ec2_with_private_key(
        EcCurve::P256,
        decode_hex(ES256_PUBLIC_KEY_X_HEX),
        decode_hex(ES256_PUBLIC_KEY_Y_HEX),
        decode_hex(ES256_PRIVATE_KEY_HEX),
    );
    let context = authorization_context();
    let text = DpopProofBuilder::new("id-1", 1700000000.0, context.clone())
        .key_id("key-1")
        .build(&crypto, &key, &jwk)
        .expect("proof を発行できる");
    let proof = DpopProof::decode(&text).expect("proof をデコードできる");
    assert_eq!(proof.header().typ, DPOP_PROOF_JWT_TYPE);
    assert_eq!(proof.header().algorithm, Algorithm::Es256);
    assert_eq!(proof.header().jwk, jwk);
    assert_eq!(proof.header().key_id.as_deref(), Some("key-1"));
    assert_eq!(proof.claims().jti, "id-1");
    assert_eq!(proof.claims().issued_at, 1700000000.0);
    let decoded_context = &proof.claims().authorization_context;
    assert_eq!(decoded_context.context_type, context.context_type);
    assert_eq!(decoded_context.action, context.action);
    assert_eq!(decoded_context.track_namespace, context.track_namespace);
    assert_eq!(decoded_context.track_name, context.track_name);
    assert_eq!(decoded_context.resource, context.resource);
    assert!(!decoded_context.raw.is_empty());
    proof.verify_signature(&crypto).expect("署名を検証できる");

    let thumbprint = jwk
        .thumbprint_sha256(&crypto)
        .expect("サムプリントを計算できる");
    let confirmation = Confirmation {
        jwk_thumbprint: Some(thumbprint),
        ..Confirmation::default()
    };
    proof
        .verify_key_binding(&crypto, &confirmation)
        .expect("鍵バインディングを検証できる");
    proof
        .verify_authorization_context(MoqtAction::PublishNamespace, &namespace(), b"live")
        .expect("Authorization Context を検証できる");
    proof
        .verify_freshness(1700000060.0, 60.0)
        .expect("ウィンドウ内の iat は受理される");
    assert_eq!(
        proof.verify_freshness(1700000061.0, 60.0),
        Err(DpopError::ProofExpired)
    );
    assert_eq!(
        proof.verify_freshness(1699999939.0, 60.0),
        Err(DpopError::ProofNotYetValid)
    );
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn access_token_hash_round_trip() {
    use shiguredo_moqt::c4m::crypto::DigestAlgorithm;
    use shiguredo_moqt::c4m::crypto::EcCurve;
    use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;

    let crypto = AwsLcRsCrypto::new();
    let jwk = es256_jwk();
    let key = CoseKey::ec2_with_private_key(
        EcCurve::P256,
        decode_hex(ES256_PUBLIC_KEY_X_HEX),
        decode_hex(ES256_PUBLIC_KEY_Y_HEX),
        decode_hex(ES256_PRIVATE_KEY_HEX),
    );
    let digest = crypto
        .digest(DigestAlgorithm::Sha256, b"token-abc")
        .expect("ハッシュを計算できる");
    let text = DpopProofBuilder::new("id-1", 1700000000.0, authorization_context())
        .access_token_hash(base64url(&digest))
        .build(&crypto, &key, &jwk)
        .expect("proof を発行できる");
    let proof = DpopProof::decode(&text).expect("proof をデコードできる");
    proof
        .verify_access_token_hash(&crypto, "token-abc")
        .expect("ath を検証できる");
    assert_eq!(
        proof.verify_access_token_hash(&crypto, "other"),
        Err(DpopError::AccessTokenHashMismatch)
    );

    // ath が無い proof は何もしない
    let text = DpopProofBuilder::new("id-2", 1700000000.0, authorization_context())
        .build(&crypto, &key, &jwk)
        .expect("proof を発行できる");
    let proof = DpopProof::decode(&text).expect("proof をデコードできる");
    proof
        .verify_access_token_hash(&crypto, "token-abc")
        .expect("ath が無ければ検証しない");
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_against_token_with_replay_cache() {
    use shiguredo_moqt::c4m::crypto::EcCurve;
    use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;

    let crypto = AwsLcRsCrypto::new();
    let jwk = es256_jwk();
    let key = CoseKey::ec2_with_private_key(
        EcCurve::P256,
        decode_hex(ES256_PUBLIC_KEY_X_HEX),
        decode_hex(ES256_PUBLIC_KEY_Y_HEX),
        decode_hex(ES256_PRIVATE_KEY_HEX),
    );
    let thumbprint = jwk
        .thumbprint_sha256(&crypto)
        .expect("サムプリントを計算できる");
    let token_text = CatTokenBuilder::new()
        .issuer("https://auth.example.com")
        .moqt(MoqtClaim::new().scope(MoqtScope::new([MoqtAction::PublishNamespace])))
        .jwk_thumbprint(thumbprint)
        .catdpop(300.0, true)
        .build_compact(&crypto, &CoseKey::symmetric(decode_hex(HMAC_KEY_HEX)))
        .expect("トークンを発行できる");
    let token = CatToken::decode(token_text.as_bytes()).expect("トークンをデコードできる");
    let text = DpopProofBuilder::new("id-1", 1700000000.0, authorization_context())
        .build(&crypto, &key, &jwk)
        .expect("proof を発行できる");
    let proof = DpopProof::decode(&text).expect("proof をデコードできる");

    let namespace = namespace();
    let request = DpopVerification {
        token_claims: token.claims(),
        action: MoqtAction::PublishNamespace,
        namespace: &namespace,
        track_name: b"live",
        reference_time_seconds: 1700000010.0,
        default_window_seconds: 60.0,
    };
    let mut cache = DpopReplayCache::new();
    proof
        .verify_against_token(&crypto, &request, Some(&mut cache))
        .expect("1 回目は検証できる");
    assert_eq!(cache.len(), 1);
    assert_eq!(
        proof.verify_against_token(&crypto, &request, Some(&mut cache)),
        Err(DpopError::Replayed)
    );
    assert_eq!(
        proof.verify_against_token(&crypto, &request, None),
        Err(DpopError::ReplayCacheRequired)
    );
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn key_binding_errors() {
    use shiguredo_moqt::c4m::crypto::EcCurve;
    use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;

    let crypto = AwsLcRsCrypto::new();
    let jwk = es256_jwk();
    let key = CoseKey::ec2_with_private_key(
        EcCurve::P256,
        decode_hex(ES256_PUBLIC_KEY_X_HEX),
        decode_hex(ES256_PUBLIC_KEY_Y_HEX),
        decode_hex(ES256_PRIVATE_KEY_HEX),
    );
    let text = DpopProofBuilder::new("id-1", 1700000000.0, authorization_context())
        .build(&crypto, &key, &jwk)
        .expect("proof を発行できる");
    let proof = DpopProof::decode(&text).expect("proof をデコードできる");

    // jkt が無い
    assert_eq!(
        proof.verify_key_binding(&crypto, &Confirmation::default()),
        Err(DpopError::MissingThumbprint)
    );
    // jkt が一致しない
    assert_eq!(
        proof.verify_key_binding(
            &crypto,
            &Confirmation {
                jwk_thumbprint: Some(vec![0xab; 32]),
                ..Confirmation::default()
            }
        ),
        Err(DpopError::KeyBindingMismatch)
    );
    // ドラフトのベクタが使う confirmation key 3 でも一致すればよい
    let thumbprint = jwk
        .thumbprint_sha256(&crypto)
        .expect("サムプリントを計算できる");
    proof
        .verify_key_binding(
            &crypto,
            &Confirmation {
                c4m_draft_jwk_thumbprint: Some(thumbprint),
                ..Confirmation::default()
            },
        )
        .expect("confirmation key 3 でも検証できる");
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn builder_rejects_wrong_key_and_hmac() {
    use shiguredo_moqt::c4m::crypto::EcCurve;
    use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;

    let crypto = AwsLcRsCrypto::new();
    let key = CoseKey::ec2_with_private_key(
        EcCurve::P256,
        decode_hex(ES256_PUBLIC_KEY_X_HEX),
        decode_hex(ES256_PUBLIC_KEY_Y_HEX),
        decode_hex(ES256_PRIVATE_KEY_HEX),
    );
    // 署名鍵と JWK が一致しない場合は発行しない
    let other_jwk = Jwk::Ec {
        curve: String::from("P-256"),
        x: base64url(&[1u8; 32]),
        y: base64url(&[2u8; 32]),
    };
    assert_eq!(
        DpopProofBuilder::new("id-1", 1700000000.0, authorization_context())
            .build(&crypto, &key, &other_jwk),
        Err(DpopError::KeyMismatch)
    );
    // 対称鍵は使えない
    assert_eq!(
        DpopProofBuilder::new("id-1", 1700000000.0, authorization_context()).build(
            &crypto,
            &CoseKey::symmetric(decode_hex(HMAC_KEY_HEX)),
            &es256_jwk()
        ),
        Err(DpopError::UnsupportedAlgorithm)
    );
}

#[test]
fn authorization_context_errors() {
    let context = authorization_context();
    assert_eq!(
        context.verify_context_type("other"),
        Err(DpopError::ContextTypeMismatch)
    );
    context
        .verify_context_type(MOQT_AUTHORIZATION_CONTEXT_TYPE)
        .expect("type が一致する");
    context
        .verify_action(MoqtAction::PublishNamespace)
        .expect("action が一致する");
    assert_eq!(
        context.verify_action(MoqtAction::Subscribe),
        Err(DpopError::ActionMismatch)
    );
    context
        .verify_target(&namespace(), b"live")
        .expect("tns / tn が一致する");
    assert_eq!(
        context.verify_target(&namespace(), b"other"),
        Err(DpopError::TargetMismatch)
    );
    let other_namespace = TrackNamespace::new(vec![b"other.example".to_vec()]).expect("作れる");
    assert_eq!(
        context.verify_target(&other_namespace, b"live"),
        Err(DpopError::TargetMismatch)
    );
    // tn が無い場合は C4M の要求を満たさない
    let mut without_tn = context.clone();
    without_tn.track_name = None;
    assert_eq!(
        without_tn.verify_target(&namespace(), b"live"),
        Err(DpopError::MissingTrackName)
    );
    context
        .verify_resource_consistency()
        .expect("resource が tns / tn と整合する");
}

#[test]
fn resource_consistency_errors() {
    let mut context = authorization_context();
    context.resource = Some(String::from("moqt://relay.example.com?tns=other&tn=live"));
    assert_eq!(
        context.verify_resource_consistency(),
        Err(DpopError::ResourceInconsistent)
    );
    context.resource = Some(String::from(
        "moqt://relay.example.com?tns=example.2ecom&tn=other",
    ));
    assert_eq!(
        context.verify_resource_consistency(),
        Err(DpopError::ResourceInconsistent)
    );
    context.resource = Some(String::from("moqt://relay.example.com"));
    assert_eq!(
        context.verify_resource_consistency(),
        Err(DpopError::ResourceInconsistent)
    );
    context.resource = None;
    context
        .verify_resource_consistency()
        .expect("resource が無ければ検証しない");
}

#[test]
fn replay_cache_records_and_prunes() {
    let mut cache = DpopReplayCache::new();
    assert!(cache.is_empty());
    cache
        .check_and_record("a", 100.0, 60.0, 100.0)
        .expect("1 回目は記録できる");
    cache
        .check_and_record("b", 100.0, 60.0, 100.0)
        .expect("別の jti は記録できる");
    assert_eq!(cache.len(), 2);
    assert_eq!(
        cache.check_and_record("a", 100.0, 60.0, 130.0),
        Err(DpopError::Replayed)
    );
    // ウィンドウを過ぎた記録は破棄される
    cache
        .check_and_record("a", 100.0, 60.0, 200.0)
        .expect("古い記録は破棄される");
    assert_eq!(cache.len(), 1);
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_against_token_detects_old_proof() {
    use shiguredo_moqt::c4m::crypto::EcCurve;
    use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;

    let crypto = AwsLcRsCrypto::new();
    let jwk = es256_jwk();
    let key = CoseKey::ec2_with_private_key(
        EcCurve::P256,
        decode_hex(ES256_PUBLIC_KEY_X_HEX),
        decode_hex(ES256_PUBLIC_KEY_Y_HEX),
        decode_hex(ES256_PRIVATE_KEY_HEX),
    );
    let thumbprint = jwk
        .thumbprint_sha256(&crypto)
        .expect("サムプリントを計算できる");
    let token_text = CatTokenBuilder::new()
        .moqt(MoqtClaim::new().scope(MoqtScope::new([MoqtAction::PublishNamespace])))
        .jwk_thumbprint(thumbprint)
        // catdpop のウィンドウは 30 秒
        .catdpop(30.0, false)
        .build_compact(&crypto, &CoseKey::symmetric(decode_hex(HMAC_KEY_HEX)))
        .expect("トークンを発行できる");
    let token = CatToken::decode(token_text.as_bytes()).expect("トークンをデコードできる");
    let text = DpopProofBuilder::new("id-1", 1700000000.0, authorization_context())
        .build(&crypto, &key, &jwk)
        .expect("proof を発行できる");
    let proof = DpopProof::decode(&text).expect("proof をデコードできる");
    let namespace = namespace();
    let request = DpopVerification {
        token_claims: token.claims(),
        action: MoqtAction::PublishNamespace,
        namespace: &namespace,
        track_name: b"live",
        // iat から 31 秒後はウィンドウ外
        reference_time_seconds: 1700000031.0,
        // 既定値も 30 秒だが、catdpop の値が優先されることを確認する
        default_window_seconds: 300.0,
    };
    assert_eq!(
        proof.verify_against_token(&crypto, &request, None),
        Err(DpopError::ProofExpired)
    );
}
