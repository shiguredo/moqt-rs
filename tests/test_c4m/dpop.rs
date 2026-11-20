//! DPoP proof (draft-nandakumar-moq-generic-dpop-proof) のテスト

use base64ct::{Base64UrlUnpadded, Encoding};
use shiguredo_moqt::c4m::MoqtAction;
use shiguredo_moqt::c4m::dpop::{
    AuthorizationContext, DEFAULT_MAX_WINDOW_SECONDS, DpopError, DpopProof, DpopReplayCache,
    MOQT_AUTHORIZATION_CONTEXT_TYPE,
};
use shiguredo_moqt::message::common::TrackNamespace;

#[cfg(feature = "aws-lc-rs")]
use super::helpers::decode_hex;
#[cfg(feature = "aws-lc-rs")]
use super::vectors::{
    ES256_PRIVATE_KEY_HEX, ES256_PUBLIC_KEY_X_HEX, ES256_PUBLIC_KEY_Y_HEX, HMAC_KEY_HEX,
};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::cat::{
    CatClaims, CatToken, CatTokenBuilder, Confirmation, ConfirmationKey3Interpretation,
};
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

/// actx の JSON を埋め込んだ署名なし proof を組み立てる
///
/// 署名を検証しない検証 API のテストで使う。埋め込み JWK はデコードできる形であればよい。
fn proof_with_authorization_context(actx: &str) -> DpopProof {
    DpopProof::decode(&unsigned_jwt(
        r#"{"alg":"ES256","typ":"dpop-proof+jwt","jwk":{"kty":"EC","crv":"P-256","x":"AQ","y":"AQ"}}"#,
        &format!(r#"{{"jti":"id","iat":1,"actx":{actx}}}"#),
    ))
    .expect("proof をデコードできる")
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
        .verify_key_binding(
            &crypto,
            &confirmation,
            ConfirmationKey3Interpretation::IanaKid,
        )
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
        confirmation_key3: ConfirmationKey3Interpretation::IanaKid,
        namespace: &namespace,
        track_name: b"live",
        reference_time_seconds: 1700000010.0,
        default_window_seconds: 60.0,
        access_token: None,
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

    // 既定の IanaKid では IANA 登録の jkt (323) が無いと鍵束縛を検証できない
    assert_eq!(
        proof.verify_key_binding(
            &crypto,
            &Confirmation::default(),
            ConfirmationKey3Interpretation::IanaKid,
        ),
        Err(DpopError::MissingThumbprint)
    );
    // jkt が一致しない
    assert_eq!(
        proof.verify_key_binding(
            &crypto,
            &Confirmation {
                jwk_thumbprint: Some(vec![0xab; 32]),
                ..Confirmation::default()
            },
            ConfirmationKey3Interpretation::IanaKid,
        ),
        Err(DpopError::KeyBindingMismatch)
    );
    let thumbprint = jwk
        .thumbprint_sha256(&crypto)
        .expect("サムプリントを計算できる");
    // IanaKid では confirmation key 3 は kid であり、jkt として比較しない
    let draft_only = Confirmation {
        confirmation_key_3: Some(thumbprint.clone()),
        ..Confirmation::default()
    };
    assert_eq!(
        proof.verify_key_binding(
            &crypto,
            &draft_only,
            ConfirmationKey3Interpretation::IanaKid,
        ),
        Err(DpopError::MissingThumbprint)
    );
    assert_eq!(draft_only.jkt(), None);
    assert_eq!(
        draft_only.jkt_with(ConfirmationKey3Interpretation::IanaKid),
        None
    );
    assert_eq!(
        draft_only.kid(ConfirmationKey3Interpretation::IanaKid),
        Some(thumbprint.as_slice())
    );
    assert_eq!(
        draft_only.kid(ConfirmationKey3Interpretation::C4mDraftJkt),
        None
    );
    // C4mDraftJkt では confirmation key 3 を jkt として比較する
    assert_eq!(
        draft_only.jkt_with(ConfirmationKey3Interpretation::C4mDraftJkt),
        Some(thumbprint.as_slice())
    );
    proof
        .verify_key_binding(
            &crypto,
            &draft_only,
            ConfirmationKey3Interpretation::C4mDraftJkt,
        )
        .expect("C4mDraftJkt では confirmation key 3 で検証できる");
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
    // 旧 JWT 形式に対応する文字列を持たない SUBSCRIBE_TRACKS は、この actx の
    // action 値 (PUB_NS) に対して拒否する
    assert_eq!(
        context.verify_action(MoqtAction::SubscribeTracks),
        Err(DpopError::ActionMismatch)
    );
    // SETUP の旧文字列 "SETUP" は Setup に対して受理する
    let setup = AuthorizationContext {
        action: String::from("SETUP"),
        ..authorization_context()
    };
    setup
        .verify_action(MoqtAction::Setup)
        .expect("SETUP は Setup と一致する");
    assert_eq!(
        setup.verify_action(MoqtAction::SubscribeTracks),
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
        .verify_resource_consistency(MoqtAction::PublishNamespace)
        .expect("resource が tns / tn と整合する");
}

#[test]
fn resource_consistency_for_namespace_and_track_actions() {
    let mut context = authorization_context();
    // namespace 操作: tns の不一致
    context.resource = Some(String::from("moqt://relay.example.com?tns=other&tn=live"));
    assert_eq!(
        context.verify_resource_consistency(MoqtAction::PublishNamespace),
        Err(DpopError::ResourceInconsistent)
    );
    // namespace 操作: actx とクエリの両方に tn があり、値が一致しない
    context.resource = Some(String::from(
        "moqt://relay.example.com?tns=example.2ecom&tn=other",
    ));
    assert_eq!(
        context.verify_resource_consistency(MoqtAction::SubscribeNamespace),
        Err(DpopError::ResourceInconsistent)
    );
    // namespace 操作: §3.1.3 の形式どおりクエリに tn が無い場合は、actx の tn と
    // 比較せずに受理する
    context.resource = Some(String::from("moqt://relay.example.com?tns=example.2ecom"));
    context
        .verify_resource_consistency(MoqtAction::PublishNamespace)
        .expect("namespace 操作の resource は tn を持たなくてよい");
    context
        .verify_resource_consistency(MoqtAction::SubscribeNamespace)
        .expect("namespace 操作の resource は tn を持たなくてよい");
    context
        .verify_resource_consistency(MoqtAction::SubscribeTracks)
        .expect("SUBSCRIBE_TRACKS も namespace 操作として扱う");
    // namespace 操作ではクエリに tn があっても、actx に tn が無ければ比較しない
    let mut without_tn = context.clone();
    without_tn.track_name = None;
    without_tn.resource = Some(String::from(
        "moqt://relay.example.com?tns=example.2ecom&tn=live",
    ));
    without_tn
        .verify_resource_consistency(MoqtAction::PublishNamespace)
        .expect("actx に tn が無ければクエリの tn を比較しない");

    // track 操作: クエリに tn が無い resource は整合しない
    context.resource = Some(String::from("moqt://relay.example.com?tns=example.2ecom"));
    for action in [
        MoqtAction::Subscribe,
        MoqtAction::RequestUpdate,
        MoqtAction::Publish,
        MoqtAction::Fetch,
        MoqtAction::TrackStatus,
    ] {
        assert_eq!(
            context.verify_resource_consistency(action),
            Err(DpopError::ResourceInconsistent),
            "track 操作 {action:?} が tn の無い resource を受理した"
        );
    }
    // track 操作: tn の不一致
    context.resource = Some(String::from(
        "moqt://relay.example.com?tns=example.2ecom&tn=other",
    ));
    assert_eq!(
        context.verify_resource_consistency(MoqtAction::Subscribe),
        Err(DpopError::ResourceInconsistent)
    );
    // track 操作: tns と tn の両方が一致すれば受理する
    context.resource = Some(String::from(
        "moqt://relay.example.com?tns=example.2ecom&tn=live",
    ));
    for action in [
        MoqtAction::Subscribe,
        MoqtAction::RequestUpdate,
        MoqtAction::Publish,
        MoqtAction::Fetch,
        MoqtAction::TrackStatus,
    ] {
        context
            .verify_resource_consistency(action)
            .expect("track 操作の tns / tn が一致する");
    }
    // resource が無ければ action によらず検証しない
    context.resource = None;
    for action in MoqtAction::ALL {
        context
            .verify_resource_consistency(action)
            .expect("resource が無ければ検証しない");
    }
}

/// namespace / track 操作の resource も `moqt://<authority>` の形式を検証すること
///
/// クエリの `tns` / `tn` が一致していても、scheme や authority が不正な resource は
/// 旧 draft-ietf-moq-c4m-01 §3.1.3 の形式ではないため拒否する。
#[test]
fn resource_consistency_rejects_malformed_resource_prefix() {
    let mut context = authorization_context();
    for resource in [
        "https://relay.example.com?tns=example.2ecom&tn=live",
        "MOQT://relay.example.com?tns=example.2ecom&tn=live",
        "?tns=example.2ecom&tn=live",
        "moqt://?tns=example.2ecom&tn=live",
        "moqt://relay.example.com/path?tns=example.2ecom&tn=live",
        "moqt://alice:s3cr3t@relay.example.com?tns=example.2ecom&tn=live",
        "moqt://relay example.com?tns=example.2ecom&tn=live",
    ] {
        context.resource = Some(String::from(resource));
        for action in [MoqtAction::Subscribe, MoqtAction::PublishNamespace] {
            assert_eq!(
                context.verify_resource_consistency(action),
                Err(DpopError::ResourceInconsistent),
                "{resource} が {action:?} で受理された"
            );
        }
    }
}

#[test]
fn setup_resource_consistency_accepts_authority_only() {
    let mut context = AuthorizationContext {
        action: String::from("SETUP"),
        resource: None,
        ..authorization_context()
    };
    // §3.1.3 の SETUP 用 resource。tns / tn は比較しない
    context.resource = Some(String::from("moqt://relay.example.com"));
    context
        .verify_resource_consistency(MoqtAction::Setup)
        .expect("SETUP の resource を受理する");
    context.resource = Some(String::from("moqt://relay.example.com:443"));
    context
        .verify_resource_consistency(MoqtAction::Setup)
        .expect("port 付きの authority を受理する");
    // SETUP 以外では SETUP 用 resource を受理しない (tns が無いため)
    context.resource = Some(String::from("moqt://relay.example.com"));
    assert_eq!(
        context.verify_resource_consistency(MoqtAction::PublishNamespace),
        Err(DpopError::ResourceInconsistent)
    );
    // SETUP の resource として不正な形式
    for resource in [
        // scheme が違う / 大文字
        "https://relay.example.com",
        "MOQT://relay.example.com",
        // authority が空
        "moqt://",
        // クエリ / フラグメント / パス / userinfo
        "moqt://relay.example.com?tns=example.2ecom",
        "moqt://relay.example.com#fragment",
        "moqt://relay.example.com/path",
        "moqt://user@relay.example.com",
        // 空白と制御文字
        "moqt://relay example.com",
        "moqt://relay.example.com\u{7f}",
        " moqt://relay.example.com",
    ] {
        context.resource = Some(String::from(resource));
        assert_eq!(
            context.verify_resource_consistency(MoqtAction::Setup),
            Err(DpopError::ResourceInconsistent),
            "SETUP の resource {resource:?} を受理した"
        );
    }
    // resource が無い場合は SETUP でも任意
    context.resource = None;
    context
        .verify_resource_consistency(MoqtAction::Setup)
        .expect("resource が無ければ検証しない");
}

#[test]
fn setup_authorization_context_skips_target() {
    // SETUP では tns / tn を target と比較しない
    let proof = proof_with_authorization_context(
        r#"{"type":"moqt","action":"SETUP","tns":"example.2ecom","tn":"live","resource":"moqt://relay.example.com"}"#,
    );
    proof
        .verify_authorization_context(MoqtAction::Setup, &namespace(), b"live")
        .expect("SETUP は target と比較しない");
    let empty_namespace = TrackNamespace::new(Vec::new()).expect("空の名前空間を作れる");
    proof
        .verify_authorization_context(MoqtAction::Setup, &empty_namespace, b"")
        .expect("SETUP は呼び出し側の target を使わない");

    // SETUP でも resource の形式は検証する
    let proof = proof_with_authorization_context(
        r#"{"type":"moqt","action":"SETUP","tns":"example.2ecom","tn":"live","resource":"https://relay.example.com"}"#,
    );
    assert_eq!(
        proof.verify_authorization_context(MoqtAction::Setup, &namespace(), b"live"),
        Err(DpopError::ResourceInconsistent)
    );

    // actx に tn が無い SETUP は MissingTrackName で拒否する
    let proof = proof_with_authorization_context(
        r#"{"type":"moqt","action":"SETUP","tns":"example.2ecom","resource":"moqt://relay.example.com"}"#,
    );
    assert_eq!(
        proof.verify_authorization_context(MoqtAction::Setup, &namespace(), b"live"),
        Err(DpopError::MissingTrackName)
    );

    // SETUP 以外では resource と target の一致を引き続き要求する
    let proof = proof_with_authorization_context(
        r#"{"type":"moqt","action":"PUBLISH","tns":"example.2ecom","tn":"live","resource":"moqt://relay.example.com?tns=example.2ecom&tn=live"}"#,
    );
    proof
        .verify_authorization_context(MoqtAction::Publish, &namespace(), b"live")
        .expect("resource と target が一致する");
    assert_eq!(
        proof.verify_authorization_context(MoqtAction::Publish, &namespace(), b"other"),
        Err(DpopError::TargetMismatch)
    );
}

#[test]
fn namespace_and_track_authorization_context_use_action_specific_resource() {
    // namespace 操作では §3.1.3 の tns だけの resource を受理する
    let proof = proof_with_authorization_context(
        r#"{"type":"moqt","action":"PUB_NS","tns":"example.2ecom","tn":"live","resource":"moqt://relay.example.com?tns=example.2ecom"}"#,
    );
    proof
        .verify_authorization_context(MoqtAction::PublishNamespace, &namespace(), b"live")
        .expect("namespace 操作の resource を受理する");

    // namespace 操作: tns の不一致
    let proof = proof_with_authorization_context(
        r#"{"type":"moqt","action":"PUB_NS","tns":"example.2ecom","tn":"live","resource":"moqt://relay.example.com?tns=other"}"#,
    );
    assert_eq!(
        proof.verify_authorization_context(MoqtAction::PublishNamespace, &namespace(), b"live"),
        Err(DpopError::ResourceInconsistent)
    );

    // namespace 操作: actx とクエリの両方にある tn の不一致
    let proof = proof_with_authorization_context(
        r#"{"type":"moqt","action":"SUB_NS","tns":"example.2ecom","tn":"live","resource":"moqt://relay.example.com?tns=example.2ecom&tn=other"}"#,
    );
    assert_eq!(
        proof.verify_authorization_context(MoqtAction::SubscribeNamespace, &namespace(), b"live"),
        Err(DpopError::ResourceInconsistent)
    );

    // track 操作: actx に tn が無い場合は verify_target が MissingTrackName で拒否する
    let proof = proof_with_authorization_context(
        r#"{"type":"moqt","action":"SUBSCRIBE","tns":"example.2ecom","resource":"moqt://relay.example.com?tns=example.2ecom"}"#,
    );
    assert_eq!(
        proof.verify_authorization_context(MoqtAction::Subscribe, &namespace(), b"live"),
        Err(DpopError::MissingTrackName)
    );
}

#[test]
fn replay_cache_records_and_prunes() {
    let mut cache = DpopReplayCache::new();
    // 既定の最大ウィンドウは 300 秒
    assert_eq!(cache.max_window_seconds(), DEFAULT_MAX_WINDOW_SECONDS);
    assert!(cache.is_empty());
    cache
        .check_and_record("a", 100.0, 60.0, 100.0)
        .expect("1 回目は記録できる");
    cache
        .check_and_record("b", 100.0, 60.0, 100.0)
        .expect("別の jti は記録できる");
    assert_eq!(cache.len(), 2);
    // 保持期限は iat + max_window_seconds (100 + 300 = 400) であり、現在の proof の
    // ウィンドウ (60 秒) ではない。130 秒後でも記録は残る
    assert_eq!(
        cache.check_and_record("a", 100.0, 60.0, 130.0),
        Err(DpopError::Replayed)
    );
    // 小さいウィンドウの proof を処理しても、既に記録した jti は破棄されない
    cache
        .check_and_record("c", 130.0, 10.0, 130.0)
        .expect("小さいウィンドウの proof も記録できる");
    assert_eq!(
        cache.check_and_record("a", 100.0, 300.0, 135.0),
        Err(DpopError::Replayed),
        "小さいウィンドウの proof で大きいウィンドウの jti が再受理された"
    );
    assert_eq!(cache.len(), 3);
    // 参照時刻が保持期限と等しい間は記録を保持する
    assert_eq!(
        cache.check_and_record("a", 100.0, 60.0, 400.0),
        Err(DpopError::Replayed)
    );
    // 保持期限を超えた記録だけを破棄する。a と b は 400 で失効し、c は 430 まで残る
    cache
        .check_and_record("a", 100.0, 60.0, 400.001)
        .expect("保持期限を過ぎた記録は破棄される");
    assert_eq!(cache.len(), 2);
    assert_eq!(
        cache.check_and_record("c", 130.0, 10.0, 400.001),
        Err(DpopError::Replayed),
        "失効していない記録まで破棄された"
    );
}

#[test]
fn replay_cache_with_max_window() {
    // 既定値は new() と Default で同じ
    assert_eq!(
        DpopReplayCache::new().max_window_seconds(),
        DEFAULT_MAX_WINDOW_SECONDS
    );
    assert_eq!(
        DpopReplayCache::default().max_window_seconds(),
        DEFAULT_MAX_WINDOW_SECONDS
    );
    assert_eq!(DEFAULT_MAX_WINDOW_SECONDS, 300.0);

    // 最大ウィンドウとして不正な値
    for max_window in [0.0, -0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(
            matches!(
                DpopReplayCache::with_max_window(max_window),
                Err(DpopError::InvalidReplayWindow)
            ),
            "最大ウィンドウ {max_window} を受理した"
        );
    }

    // 最大ウィンドウを超える proof のウィンドウは fail-closed で拒否する
    let mut cache = DpopReplayCache::with_max_window(60.0).expect("正の有限値で構築できる");
    assert_eq!(cache.max_window_seconds(), 60.0);
    assert_eq!(
        cache.check_and_record("a", 100.0, 61.0, 100.0),
        Err(DpopError::ReplayWindowTooLarge)
    );
    // 最大ウィンドウと等しいウィンドウは受理する
    cache
        .check_and_record("a", 100.0, 60.0, 100.0)
        .expect("最大ウィンドウと等しいウィンドウは受理する");
    // 保持期限は iat + 最大ウィンドウ (100 + 60 = 160)。等しい間は保持する
    assert_eq!(
        cache.check_and_record("a", 100.0, 60.0, 160.0),
        Err(DpopError::Replayed)
    );
    cache
        .check_and_record("a", 100.0, 60.0, 160.001)
        .expect("保持期限を過ぎた記録は破棄される");

    // 負のウィンドウは InvalidFreshnessWindow で拒否する
    assert_eq!(
        cache.check_and_record("b", 100.0, -1.0, 100.0),
        Err(DpopError::InvalidFreshnessWindow)
    );
    // 非有限のウィンドウは現行どおり NonFiniteNumber で拒否する
    assert_eq!(
        cache.check_and_record("b", 100.0, f64::NAN, 100.0),
        Err(DpopError::NonFiniteNumber("freshness window"))
    );
    // 保持期限 (`iat` + 最大ウィンドウ) が有限でない場合は記録しない
    let mut large = DpopReplayCache::with_max_window(f64::MAX).expect("正の有限値で構築できる");
    assert_eq!(
        large.check_and_record("b", f64::MAX, f64::MAX, 0.0),
        Err(DpopError::NonFiniteNumber("replay retention"))
    );
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
        confirmation_key3: ConfirmationKey3Interpretation::IanaKid,
        namespace: &namespace,
        track_name: b"live",
        // iat から 31 秒後はウィンドウ外
        reference_time_seconds: 1700000031.0,
        // 既定値も 30 秒だが、catdpop の値が優先されることを確認する
        default_window_seconds: 300.0,
        access_token: None,
    };
    assert_eq!(
        proof.verify_against_token(&crypto, &request, None),
        Err(DpopError::ProofExpired)
    );
}

#[test]
fn duplicate_members_are_rejected() {
    // RFC 7515 §4 は JWS ヘッダのメンバー名の一意性を MUST とする
    let jws_error = DpopProof::decode(&unsigned_jwt(
        r#"{"alg":"ES256","alg":"ES256","typ":"dpop-proof+jwt","jwk":{"kty":"EC","crv":"P-256","x":"AQ","y":"AQ"}}"#,
        r#"{"jti":"id","iat":1,"actx":{"type":"moqt","action":"PUB_NS","tns":"a","tn":"b"}}"#,
    ));
    assert_eq!(
        jws_error,
        Err(DpopError::Jwt(
            shiguredo_moqt::c4m::jwt::JwtError::DuplicateMember(String::from("alg"))
        ))
    );

    // ペイロードのクレーム名の重複も拒否する
    let claims_error = DpopProof::decode(&unsigned_jwt(
        r#"{"alg":"ES256","typ":"dpop-proof+jwt","jwk":{"kty":"EC","crv":"P-256","x":"AQ","y":"AQ"}}"#,
        r#"{"jti":"id","jti":"other","iat":1,"actx":{"type":"moqt","action":"PUB_NS","tns":"a","tn":"b"}}"#,
    ));
    assert_eq!(
        claims_error,
        Err(DpopError::DuplicateMember(String::from("jti")))
    );

    // actx のメンバー名の重複も拒否する
    let actx_error = DpopProof::decode(&unsigned_jwt(
        r#"{"alg":"ES256","typ":"dpop-proof+jwt","jwk":{"kty":"EC","crv":"P-256","x":"AQ","y":"AQ"}}"#,
        r#"{"jti":"id","iat":1,"actx":{"type":"moqt","type":"moqt","action":"PUB_NS","tns":"a","tn":"b"}}"#,
    ));
    assert_eq!(
        actx_error,
        Err(DpopError::DuplicateMember(String::from("type")))
    );
}

#[test]
fn non_finite_iat_is_rejected() {
    // JSON の 1e999 は f64 では無限大になる
    assert_eq!(
        DpopProof::decode(&unsigned_jwt(
            r#"{"alg":"ES256","typ":"dpop-proof+jwt","jwk":{"kty":"EC","crv":"P-256","x":"AQ","y":"AQ"}}"#,
            r#"{"jti":"id","iat":1e999,"actx":{"type":"moqt","action":"PUB_NS","tns":"a","tn":"b"}}"#,
        )),
        Err(DpopError::NonFiniteNumber("iat"))
    );
}

#[test]
fn non_finite_freshness_values_are_rejected() {
    let proof = DpopProof::decode(&unsigned_jwt(
        r#"{"alg":"ES256","typ":"dpop-proof+jwt","jwk":{"kty":"EC","crv":"P-256","x":"AQ","y":"AQ"}}"#,
        r#"{"jti":"id","iat":1,"actx":{"type":"moqt","action":"PUB_NS","tns":"a","tn":"b"}}"#,
    ))
    .expect("デコードできる");
    assert_eq!(
        proof.verify_freshness(1.0, f64::NAN),
        Err(DpopError::NonFiniteNumber("freshness window"))
    );
    assert_eq!(
        proof.verify_freshness(f64::INFINITY, 10.0),
        Err(DpopError::NonFiniteNumber("reference time"))
    );

    let mut cache = DpopReplayCache::new();
    assert_eq!(
        cache.check_and_record("id", 1.0, f64::NAN, 1.0),
        Err(DpopError::NonFiniteNumber("freshness window"))
    );
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn access_token_is_required_for_ath_binding() {
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
        .digest(DigestAlgorithm::Sha256, b"access-token")
        .expect("ハッシュを計算できる");
    let text = DpopProofBuilder::new("id-1", 1700000000.0, authorization_context())
        .access_token_hash(base64url(&digest))
        .build(&crypto, &key, &jwk)
        .expect("proof を発行できる");
    let proof = DpopProof::decode(&text).expect("proof をデコードできる");

    let namespace = namespace();
    let request = DpopVerification {
        token_claims: &CatClaims::default(),
        action: MoqtAction::PublishNamespace,
        confirmation_key3: ConfirmationKey3Interpretation::IanaKid,
        namespace: &namespace,
        track_name: b"live",
        reference_time_seconds: 1700000000.0,
        default_window_seconds: 60.0,
        access_token: None,
    };
    // ath があるのにアクセストークンを渡さない場合は検証しない (fail-closed)
    assert_eq!(
        proof.verify_against_token(&crypto, &request, None),
        Err(DpopError::MissingAccessToken)
    );

    // アクセストークンを渡せば署名検証のあと ath が照合される
    let request_with_token = DpopVerification {
        access_token: Some(b"access-token"),
        ..request
    };
    assert_eq!(
        proof.verify_against_token(&crypto, &request_with_token, None),
        Err(DpopError::MissingConfirmation)
    );

    // ath が無い proof にアクセストークンを渡すと必須違反になる
    let text = DpopProofBuilder::new("id-2", 1700000000.0, authorization_context())
        .build(&crypto, &key, &jwk)
        .expect("proof を発行できる");
    let proof = DpopProof::decode(&text).expect("proof をデコードできる");
    assert_eq!(
        proof.verify_against_token(&crypto, &request_with_token, None),
        Err(DpopError::MissingAccessTokenHash)
    );
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_against_cat_token_uses_the_raw_token_for_ath() {
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
    let token_text = CatTokenBuilder::new()
        .issuer("https://auth.example.com")
        .moqt(MoqtClaim::new().scope(MoqtScope::new([MoqtAction::PublishNamespace])))
        .jwk_thumbprint(
            jwk.thumbprint_sha256(&crypto)
                .expect("サムプリントを計算できる"),
        )
        .catdpop(300.0, true)
        .build_compact(&crypto, &CoseKey::symmetric(decode_hex(HMAC_KEY_HEX)))
        .expect("トークンを発行できる");
    let token = CatToken::decode(token_text.as_bytes()).expect("トークンをデコードできる");

    // アクセストークン = トークンの生バイト (compact ではトークン文字列)
    let digest = crypto
        .digest(DigestAlgorithm::Sha256, token.raw_token())
        .expect("ハッシュを計算できる");
    let text = DpopProofBuilder::new("id-1", 1700000000.0, authorization_context())
        .access_token_hash(base64url(&digest))
        .build(&crypto, &key, &jwk)
        .expect("proof を発行できる");
    let proof = DpopProof::decode(&text).expect("proof をデコードできる");

    let namespace = namespace();
    let mut cache = DpopReplayCache::new();
    proof
        .verify_against_cat_token(
            &crypto,
            &token,
            MoqtAction::PublishNamespace,
            &namespace,
            b"live",
            1700000010.0,
            60.0,
            Some(&mut cache),
        )
        .expect("CAT トークンに束縛された proof を検証できる");

    // 別のトークン文字列に対しては ath が一致しない
    let other_token = CatToken::decode(
        CatTokenBuilder::new()
            .issuer("https://other.example.com")
            .moqt(MoqtClaim::new().scope(MoqtScope::new([MoqtAction::PublishNamespace])))
            .jwk_thumbprint(
                jwk.thumbprint_sha256(&crypto)
                    .expect("サムプリントを計算できる"),
            )
            .catdpop(300.0, true)
            .build_compact(&crypto, &CoseKey::symmetric(decode_hex(HMAC_KEY_HEX)))
            .expect("トークンを発行できる")
            .as_bytes(),
    )
    .expect("トークンをデコードできる");
    assert_eq!(
        proof.verify_against_cat_token(
            &crypto,
            &other_token,
            MoqtAction::PublishNamespace,
            &namespace,
            b"live",
            1700000010.0,
            60.0,
            None,
        ),
        Err(DpopError::AccessTokenHashMismatch)
    );
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_against_cat_token_with_uses_c4m_draft_key3() {
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
    let thumbprint = jwk
        .thumbprint_sha256(&crypto)
        .expect("サムプリントを計算できる");
    // draft-01 のベクタと同じく、cnf の confirmation key 3 だけに jkt を入れる
    let token_text = CatTokenBuilder::new()
        .moqt(MoqtClaim::new().scope(MoqtScope::new([MoqtAction::PublishNamespace])))
        .c4m_draft_jwk_thumbprint(thumbprint.clone())
        .catdpop(300.0, false)
        .build_compact(&crypto, &CoseKey::symmetric(decode_hex(HMAC_KEY_HEX)))
        .expect("トークンを発行できる");
    let token = CatToken::decode(token_text.as_bytes()).expect("トークンをデコードできる");
    // IanaKid の解釈では key 3 は kid として取り出せる
    assert_eq!(
        token
            .claims()
            .confirmation
            .as_ref()
            .and_then(|confirmation| { confirmation.kid(ConfirmationKey3Interpretation::IanaKid) }),
        Some(thumbprint.as_slice())
    );

    let digest = crypto
        .digest(DigestAlgorithm::Sha256, token.raw_token())
        .expect("ハッシュを計算できる");
    let text = DpopProofBuilder::new("id-1", 1700000000.0, authorization_context())
        .access_token_hash(base64url(&digest))
        .build(&crypto, &key, &jwk)
        .expect("proof を発行できる");
    let proof = DpopProof::decode(&text).expect("proof をデコードできる");
    let namespace = namespace();

    // 既定の IanaKid では key 3 を jkt として使わないため鍵束縛を検証できない
    assert_eq!(
        proof.verify_against_cat_token(
            &crypto,
            &token,
            MoqtAction::PublishNamespace,
            &namespace,
            b"live",
            1700000010.0,
            60.0,
            None,
        ),
        Err(DpopError::MissingThumbprint)
    );
    assert_eq!(
        proof.verify_against_cat_token_with(
            &crypto,
            &token,
            MoqtAction::PublishNamespace,
            ConfirmationKey3Interpretation::IanaKid,
            &namespace,
            b"live",
            1700000010.0,
            60.0,
            None,
        ),
        Err(DpopError::MissingThumbprint)
    );
    // C4mDraftJkt では key 3 を jkt として鍵束縛を検証する
    proof
        .verify_against_cat_token_with(
            &crypto,
            &token,
            MoqtAction::PublishNamespace,
            ConfirmationKey3Interpretation::C4mDraftJkt,
            &namespace,
            b"live",
            1700000010.0,
            60.0,
            None,
        )
        .expect("C4mDraftJkt では confirmation key 3 で鍵束縛を検証できる");
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn dpop_builder_rejects_non_finite_iat() {
    use shiguredo_moqt::c4m::crypto::EcCurve;
    use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;

    let crypto = AwsLcRsCrypto::new();
    let key = CoseKey::ec2_with_private_key(
        EcCurve::P256,
        decode_hex(ES256_PUBLIC_KEY_X_HEX),
        decode_hex(ES256_PUBLIC_KEY_Y_HEX),
        decode_hex(ES256_PRIVATE_KEY_HEX),
    );
    assert_eq!(
        DpopProofBuilder::new("id-1", f64::NAN, authorization_context()).build(
            &crypto,
            &key,
            &es256_jwk()
        ),
        Err(DpopError::NonFiniteNumber("iat"))
    );
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn key_binding_prefers_the_iana_thumbprint() {
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
    let thumbprint = jwk
        .thumbprint_sha256(&crypto)
        .expect("サムプリントを計算できる");

    // 323 が一致すれば 3 の値は、どちらの解釈でも見ない
    let confirmation = Confirmation {
        jwk_thumbprint: Some(thumbprint.clone()),
        confirmation_key_3: Some(vec![0xab; 32]),
        ..Confirmation::default()
    };
    for interpretation in [
        ConfirmationKey3Interpretation::IanaKid,
        ConfirmationKey3Interpretation::C4mDraftJkt,
    ] {
        proof
            .verify_key_binding(&crypto, &confirmation, interpretation)
            .expect("323 が一致すれば検証できる");
        assert_eq!(
            confirmation.jkt_with(interpretation),
            Some(thumbprint.as_slice()),
            "323 を優先しなかった"
        );
    }
    // 323 が無い場合、IanaKid では 3 を jkt として使わない
    let kid_only = Confirmation {
        confirmation_key_3: Some(thumbprint.clone()),
        ..Confirmation::default()
    };
    assert_eq!(
        proof.verify_key_binding(&crypto, &kid_only, ConfirmationKey3Interpretation::IanaKid,),
        Err(DpopError::MissingThumbprint)
    );

    // 323 があって不一致なら、3 が一致していても拒否する (323 を優先する)
    assert_eq!(
        proof.verify_key_binding(
            &crypto,
            &Confirmation {
                jwk_thumbprint: Some(vec![0xab; 32]),
                confirmation_key_3: Some(thumbprint),
                ..Confirmation::default()
            },
            ConfirmationKey3Interpretation::C4mDraftJkt,
        ),
        Err(DpopError::KeyBindingMismatch)
    );
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn builder_nonce_and_proof_accessors() {
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
        .nonce("server-nonce")
        .build(&crypto, &key, &jwk)
        .expect("proof を発行できる");
    let proof = DpopProof::decode(&text).expect("proof をデコードできる");
    assert_eq!(proof.claims().nonce.as_deref(), Some("server-nonce"));
    assert!(!proof.signing_input().is_empty());
    assert_eq!(proof.signature().len(), 64);
}

#[path = "dpop/cwt.rs"]
mod cwt;

/// 拒否した場合は既存の記録を解放しないこと
///
/// 最大ウィンドウ超過と負のウィンドウは purge より前に判定するため、期限切れの記録が
/// 残っていても解放しない。
#[test]
fn replay_cache_does_not_purge_when_rejecting() {
    let mut cache = DpopReplayCache::with_max_window(60.0).expect("キャッシュを作れる");
    cache
        .check_and_record("first", 1_000.0, 60.0, 1_000.0)
        .expect("記録できる");

    // 参照時刻 2_000 では期限切れだが、最大ウィンドウ超過で拒否されるため解放しない
    assert_eq!(
        cache.check_and_record("second", 2_000.0, 120.0, 2_000.0),
        Err(DpopError::ReplayWindowTooLarge)
    );
    assert_eq!(cache.len(), 1, "拒否した場合は既存の記録を解放しないこと");

    // 負のウィンドウでも同じ
    assert_eq!(
        cache.check_and_record("third", 3_000.0, -1.0, 3_000.0),
        Err(DpopError::InvalidFreshnessWindow)
    );
    assert_eq!(cache.len(), 1, "拒否した場合は既存の記録を解放しないこと");
}

/// キャッシュを渡した場合、replay detection を要求しない proof でも最大ウィンドウを
/// 超えたら拒否すること
///
/// CWT 形式の受理 API と同じく、`honor_jti` の有無にかかわらず fail-closed に倒す。
#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_against_token_rejects_window_larger_than_replay_cache() {
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
    // replay detection を要求しない (honor_jti = false) トークンでも最大ウィンドウを検査する
    let token_text = CatTokenBuilder::new()
        .issuer("https://auth.example.com")
        .moqt(MoqtClaim::new().scope(MoqtScope::new([MoqtAction::PublishNamespace])))
        .jwk_thumbprint(thumbprint)
        .catdpop(61.0, false)
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
        confirmation_key3: ConfirmationKey3Interpretation::IanaKid,
        namespace: &namespace,
        track_name: b"live",
        reference_time_seconds: 1700000010.0,
        default_window_seconds: 60.0,
        access_token: None,
    };
    let mut cache = DpopReplayCache::with_max_window(60.0).expect("キャッシュを作れる");
    assert_eq!(
        proof.verify_against_token(&crypto, &request, Some(&mut cache)),
        Err(DpopError::ReplayWindowTooLarge),
        "最大ウィンドウを超えた proof は replay detection の要否にかかわらず拒否すること"
    );
    assert_eq!(cache.len(), 0, "拒否した proof は記録しないこと");
}
