//! CWT DPoP の受理入口 (draft-ietf-moq-c4m-02 §4.3) のテスト

#[cfg(feature = "aws-lc-rs")]
use super::*;

// ---------------------------------------------------------------------------
// CWT DPoP の受理入口 (draft-ietf-moq-c4m-02 §4.3)
// ---------------------------------------------------------------------------

#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::cat::{
    CAT_CONTENT_TYPE, CatClaims, CatError, CatReplay, ClaimValidationError, ClaimValidationOptions,
    VerifyOptions,
};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::crypto::DigestAlgorithm;
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::dpop::{CwtDpopReplayCache, CwtDpopVerification};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::{CatDpop, MoqtClaim, MoqtScope};

/// 受理入口のテストで使う参照時刻 (UNIX 秒)
#[cfg(feature = "aws-lc-rs")]
const CWT_REFERENCE_TIME: f64 = 1_749_999_000.0;

/// CAT の発行者
#[cfg(feature = "aws-lc-rs")]
const CWT_ISSUER: &str = "issuer.example";

/// CAT の宛先
#[cfg(feature = "aws-lc-rs")]
const CWT_AUDIENCE: &str = "relay.example";

/// 期待する発行者の一覧
#[cfg(feature = "aws-lc-rs")]
const CWT_ISSUERS: &[&str] = &[CWT_ISSUER];

/// 期待する宛先の一覧
#[cfg(feature = "aws-lc-rs")]
const CWT_AUDIENCES: &[&str] = &[CWT_AUDIENCE];

/// テスト用の track name
#[cfg(feature = "aws-lc-rs")]
const CWT_TRACK_NAME: &[u8] = b"/sports/live";

/// テスト用の namespace ("cdn", "example.com")
#[cfg(feature = "aws-lc-rs")]
fn cwt_namespace() -> [&'static [u8]; 2] {
    [b"cdn".as_slice(), b"example.com".as_slice()]
}

/// `cwt_namespace` のフィールド列を proof のクレーム用に複製する
#[cfg(feature = "aws-lc-rs")]
fn cwt_namespace_fields() -> Vec<Vec<u8>> {
    cwt_namespace().iter().map(|field| field.to_vec()).collect()
}

/// COSE Key Thumbprint (RFC 9679 §3) を計算する
#[cfg(feature = "aws-lc-rs")]
fn cwt_ckt(key: &CoseKey) -> Vec<u8> {
    key.cose_key_thumbprint_sha256(&AwsLcRsCrypto::new())
        .expect("ckt を計算できる")
}

/// JWK サムプリント (RFC 7638) を計算する
#[cfg(feature = "aws-lc-rs")]
fn cwt_jkt(key: &CoseKey) -> Vec<u8> {
    Jwk::from_cose_key(key)
        .expect("COSE_Key を等価な JWK へ変換できる")
        .thumbprint_sha256(&AwsLcRsCrypto::new())
        .expect("jkt を計算できる")
}

/// 指定した action だけをすべての namespace / track で許可する `moqt` クレームを作る
#[cfg(feature = "aws-lc-rs")]
fn cwt_scope(actions: &[MoqtAction]) -> MoqtClaim {
    MoqtClaim::new().scope(MoqtScope::new(actions.iter().copied()))
}

/// テスト用の proof 公開鍵 (ES256) の ckt による `cnf` を作る
#[cfg(feature = "aws-lc-rs")]
fn cwt_ckt_confirmation() -> Confirmation {
    Confirmation {
        cose_key_thumbprint: Some(cwt_ckt(&es256_public_key())),
        ..Confirmation::default()
    }
}

/// 指定した `cnf` / `catdpop` / scope を持つ CAT の COSE バイト列を発行する
///
/// 署名鍵はテスト用の ES256 鍵である。タグの付与は `options` で変えられるため、
/// 同じクレームでも異なるバイト列のトークンを作れる。
#[cfg(feature = "aws-lc-rs")]
fn cwt_cat_token_bytes_with_options(
    confirmation: Option<Confirmation>,
    catdpop: Option<CatDpop>,
    actions: &[MoqtAction],
    expiration: f64,
    options: &CoseEncodingOptions,
) -> Vec<u8> {
    let crypto = AwsLcRsCrypto::new();
    let claims = CatClaims {
        issuer: Some(String::from(CWT_ISSUER)),
        audience: vec![String::from(CWT_AUDIENCE)],
        expiration: Some(expiration),
        issued_at: Some(CWT_REFERENCE_TIME - 10.0),
        cwt_id: Some(b"cat-token-1".to_vec()),
        confirmation,
        catdpop,
        moqt: Some(cwt_scope(actions)),
        ..CatClaims::default()
    };
    let builder = CatTokenBuilder {
        claims,
        ..CatTokenBuilder::default()
    };
    builder
        .build_cose_with(&crypto, &es256_private_key(), options)
        .expect("CAT を発行できる")
}

/// 既定のタグ付けで CAT の COSE バイト列を発行する
#[cfg(feature = "aws-lc-rs")]
fn cwt_cat_token_bytes(
    confirmation: Option<Confirmation>,
    catdpop: Option<CatDpop>,
    actions: &[MoqtAction],
    expiration: f64,
) -> Vec<u8> {
    cwt_cat_token_bytes_with_options(
        confirmation,
        catdpop,
        actions,
        expiration,
        &CoseEncodingOptions::default(),
    )
}

/// 指定した `cnf` / `catdpop` / scope を持つ CAT を発行してデコードする
#[cfg(feature = "aws-lc-rs")]
fn cwt_cat_token(
    confirmation: Option<Confirmation>,
    catdpop: Option<CatDpop>,
    actions: &[MoqtAction],
    expiration: f64,
) -> CatToken {
    CatToken::decode(&cwt_cat_token_bytes(
        confirmation,
        catdpop,
        actions,
        expiration,
    ))
    .expect("発行した CAT をデコードできる")
}

/// SUBSCRIBE を許可し、300 秒のウィンドウで replay detection を行う既定の CAT
#[cfg(feature = "aws-lc-rs")]
fn cwt_default_token() -> CatToken {
    cwt_cat_token(
        Some(cwt_ckt_confirmation()),
        Some(CatDpop::new(300.0, true)),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    )
}

/// 指定した action だけを許可し、replay detection を行わない CAT
#[cfg(feature = "aws-lc-rs")]
fn cwt_action_token(action: MoqtAction) -> CatToken {
    cwt_cat_token(
        Some(cwt_ckt_confirmation()),
        Some(CatDpop::new(300.0, false)),
        &[action],
        CWT_REFERENCE_TIME + 3600.0,
    )
}

/// CAT の提示バイト列に束縛した proof のクレームを作る
#[cfg(feature = "aws-lc-rs")]
fn cwt_claims(
    token: &CatToken,
    action: MoqtAction,
    namespace: Option<Vec<Vec<u8>>>,
    track_name: Option<Vec<u8>>,
    issued_at: i64,
    cti: &[u8],
) -> CwtDpopProofClaims {
    let crypto = AwsLcRsCrypto::new();
    let ath = crypto
        .digest(DigestAlgorithm::Sha256, token.raw_token())
        .expect("ath を計算できる");
    CwtDpopProofClaims {
        cti: cti.to_vec(),
        issued_at,
        authorization_context: CwtAuthorizationContext {
            context_type: String::from(MOQT_AUTHORIZATION_CONTEXT_TYPE),
            action: action.key(),
            namespace,
            track_name,
            raw: Vec::new(),
        },
        access_token_hash: ath,
        raw: Vec::new(),
    }
}

/// 既定の target (SUBSCRIBE / `cwt_namespace` / `CWT_TRACK_NAME`) のクレームを作る
#[cfg(feature = "aws-lc-rs")]
fn cwt_subscribe_claims(token: &CatToken, issued_at: i64, cti: &[u8]) -> CwtDpopProofClaims {
    cwt_claims(
        token,
        MoqtAction::Subscribe,
        Some(cwt_namespace_fields()),
        Some(CWT_TRACK_NAME.to_vec()),
        issued_at,
        cti,
    )
}

/// 指定した鍵で proof を発行してデコードする
#[cfg(feature = "aws-lc-rs")]
fn cwt_proof_with_key(claims: &CwtDpopProofClaims, key: &CoseKey) -> CwtDpopProof {
    let crypto = AwsLcRsCrypto::new();
    let codec = codec();
    let bytes = codec
        .build(&crypto, key, claims)
        .expect("CWT 形式の proof を発行できる");
    codec
        .decode(&bytes)
        .expect("発行した proof をデコードできる")
}

/// テスト用の ES256 秘密鍵で proof を発行してデコードする
#[cfg(feature = "aws-lc-rs")]
fn cwt_proof(claims: &CwtDpopProofClaims) -> CwtDpopProof {
    cwt_proof_with_key(claims, &es256_private_key())
}

/// 受理入口の呼び出しをまとめるテスト用の検証器
///
/// 既定値で成功経路を組み立て、失敗経路のテストでは必要なフィールドだけを差し替える。
#[cfg(feature = "aws-lc-rs")]
struct CwtVerifier {
    /// 検証に使う暗号実装
    crypto: AwsLcRsCrypto,
    /// CAT の署名検証に使う信頼済み鍵
    token_key: CoseKey,
    /// CAT のクレームと proof の鮮度に使う参照時刻
    reference_time_seconds: f64,
    /// `catdpop` にウィンドウが無い場合の既定値
    default_window_seconds: f64,
    /// 期待する発行者
    expected_issuers: &'static [&'static str],
    /// 期待する宛先
    expected_audiences: &'static [&'static str],
    /// 受信者が再検証できる最小間隔。`None` は再検証できないことを表す
    revalidation_interval_seconds: Option<f64>,
    /// proof の claim label 設定
    codec: CwtDpopProofCodec,
    /// CAT の署名検証オプション
    token_verify_options: VerifyOptions<'static>,
}

#[cfg(feature = "aws-lc-rs")]
impl CwtVerifier {
    /// 既定の検証器を作る
    fn new() -> Self {
        Self {
            crypto: AwsLcRsCrypto::new(),
            token_key: es256_public_key(),
            reference_time_seconds: CWT_REFERENCE_TIME,
            default_window_seconds: 300.0,
            expected_issuers: CWT_ISSUERS,
            expected_audiences: CWT_AUDIENCES,
            revalidation_interval_seconds: None,
            codec: codec(),
            token_verify_options: VerifyOptions {
                expected_algorithm: Some(Algorithm::Es256),
                expected_type: Some(CAT_CONTENT_TYPE),
            },
        }
    }

    /// CWT 用の空のリプレイキャッシュ (最大ウィンドウ 300 秒) を作る
    fn cache(&self) -> CwtDpopReplayCache {
        CwtDpopReplayCache::new(300.0).expect("リプレイキャッシュを作れる")
    }

    /// 受理入口を呼ぶ
    fn verify(
        &self,
        proof: &CwtDpopProof,
        token: &CatToken,
        action: MoqtAction,
        namespace: Option<&[&[u8]]>,
        track_name: Option<&[u8]>,
        cache: &mut CwtDpopReplayCache,
    ) -> Result<(), DpopError> {
        let request = CwtDpopVerification {
            token,
            token_key: &self.token_key,
            token_verify_options: self.token_verify_options,
            claim_options: ClaimValidationOptions {
                reference_time_seconds: self.reference_time_seconds,
                clock_tolerance_seconds: 0.0,
                expected_issuers: self.expected_issuers,
                expected_audiences: self.expected_audiences,
                revalidation_interval_seconds: self.revalidation_interval_seconds,
            },
            action,
            namespace,
            track_name,
            codec: self.codec,
            default_window_seconds: self.default_window_seconds,
        };
        proof.verify_against_cat_token(&self.crypto, &request, cache)
    }
}

/// 実 ES256 の CAT と CWT proof が ckt / jkt / 両方の `cnf` で受理されること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_accepts_es256_proof_with_cnf_thumbprints() {
    let namespace = cwt_namespace();
    let cases = [
        (
            "ckt",
            Confirmation {
                cose_key_thumbprint: Some(cwt_ckt(&es256_public_key())),
                ..Confirmation::default()
            },
        ),
        (
            "jkt",
            Confirmation {
                jwk_thumbprint: Some(cwt_jkt(&es256_public_key())),
                ..Confirmation::default()
            },
        ),
        (
            "ckt と jkt",
            Confirmation {
                cose_key_thumbprint: Some(cwt_ckt(&es256_public_key())),
                jwk_thumbprint: Some(cwt_jkt(&es256_public_key())),
                ..Confirmation::default()
            },
        ),
    ];
    for (name, confirmation) in cases {
        let token = cwt_cat_token(
            Some(confirmation),
            Some(CatDpop::new(300.0, true)),
            &[MoqtAction::Subscribe],
            CWT_REFERENCE_TIME + 3600.0,
        );
        let proof = cwt_proof(&cwt_subscribe_claims(
            &token,
            CWT_REFERENCE_TIME as i64,
            b"proof-1",
        ));
        let verifier = CwtVerifier::new();
        let mut cache = verifier.cache();
        let result = verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        );
        if let Err(error) = result {
            panic!("{name} の cnf で受理できなかった: {error}");
        }
        assert_eq!(cache.len(), 1, "{name}: honor_jti が真なので記録される");
    }
}

/// `cnf` を持たないトークンを拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_rejects_missing_confirmation() {
    let token = cwt_cat_token(
        None,
        Some(CatDpop::new(300.0, false)),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let proof = cwt_proof(&cwt_subscribe_claims(
        &token,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::MissingConfirmation)
    );
    assert!(cache.is_empty(), "拒否した proof を記録した");
}

/// `cnf` が draft-01 の key 3 だけを持つ場合は鍵束縛を判定できないため拒否すること
///
/// IANA のレジストリでは key 3 は `kid` であり、CWT 形式の鍵束縛には使わない。key 3 と
/// 併存する `ckt` は通常どおり照合し、key 3 の値は無視する。
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_does_not_use_draft_key3_thumbprint() {
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();

    // key 3 だけの cnf は MissingThumbprint で拒否する
    let token = cwt_cat_token(
        Some(Confirmation {
            c4m_draft_jwk_thumbprint: Some(cwt_jkt(&es256_public_key())),
            ..Confirmation::default()
        }),
        Some(CatDpop::new(300.0, false)),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let proof = cwt_proof(&cwt_subscribe_claims(
        &token,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::MissingThumbprint)
    );
    assert!(cache.is_empty(), "拒否した proof を記録した");

    // ckt と併存する key 3 が一致しなくても、ckt だけで束縛を判定する
    let token = cwt_cat_token(
        Some(Confirmation {
            cose_key_thumbprint: Some(cwt_ckt(&es256_public_key())),
            c4m_draft_jwk_thumbprint: Some(vec![0x5c; 32]),
            ..Confirmation::default()
        }),
        Some(CatDpop::new(300.0, false)),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let proof = cwt_proof(&cwt_subscribe_claims(
        &token,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    verifier
        .verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        )
        .expect("key 3 は鍵束縛の判定に使わない");
}

/// proof の署名を改変すると拒否し、リプレイ状態を記録しないこと
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_rejects_tampered_proof_signature() {
    let token = cwt_default_token();
    let claims = cwt_subscribe_claims(&token, CWT_REFERENCE_TIME as i64, b"proof-1");
    let crypto = AwsLcRsCrypto::new();
    let bytes = codec()
        .build(&crypto, &es256_private_key(), &claims)
        .expect("CWT 形式の proof を発行できる");
    let mut tampered = bytes;
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    let proof = codec()
        .decode(&tampered)
        .expect("署名だけを改変した proof をデコードできる");
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    let error = verifier
        .verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        )
        .expect_err("署名改変を拒否する");
    assert_eq!(
        error,
        DpopError::Crypto(CryptoError::SignatureVerificationFailed)
    );
    assert!(cache.is_empty(), "拒否した proof を記録した");
}

/// トークンの `cnf` に束縛されていない鍵で署名した proof を拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_rejects_proof_signed_by_another_key() {
    let token = cwt_default_token();
    let claims = cwt_subscribe_claims(&token, CWT_REFERENCE_TIME as i64, b"proof-1");
    let (es384_private_key, _) = es384_keys();
    let proof = cwt_proof_with_key(&claims, &es384_private_key);
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::KeyBindingMismatch)
    );
    assert!(cache.is_empty(), "拒否した proof を記録した");
}

/// 別のトークン用の `ath` を持つ proof を拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_rejects_proof_for_another_token() {
    let token_a = cwt_default_token();
    let proof = cwt_proof(&cwt_subscribe_claims(
        &token_a,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    // 同じ鍵で署名した別のトークン。クレームが違うため提示バイト列も変わる
    let token_b = cwt_cat_token(
        Some(cwt_ckt_confirmation()),
        Some(CatDpop::new(200.0, true)),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 7200.0,
    );
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &token_b,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::AccessTokenHashMismatch)
    );
    assert!(cache.is_empty(), "拒否した proof を記録した");
}

/// 同じ意味のトークンでも提示バイト列が異なれば `ath` を流用できないこと
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_ath_requires_exact_token_bytes() {
    let token_bytes = cwt_cat_token_bytes(
        Some(cwt_ckt_confirmation()),
        Some(CatDpop::new(300.0, false)),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let token = CatToken::decode(&token_bytes).expect("CAT をデコードできる");
    let proof = cwt_proof(&cwt_subscribe_claims(
        &token,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();

    // 同じ COSE を Base64url で包んだ表現。デコード後のクレームと署名は同じだが、
    // 提示されたバイト列が違うため ath は一致しない
    let wrapped = {
        use base64ct::{Base64UrlUnpadded, Encoding};
        Base64UrlUnpadded::encode_string(&token_bytes)
    };
    let wrapped_token =
        CatToken::decode(wrapped.as_bytes()).expect("Base64url で包んだ CAT をデコードできる");
    assert_eq!(
        wrapped_token.claims(),
        token.claims(),
        "同じトークンのクレームとしてデコードされる"
    );
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &wrapped_token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::AccessTokenHashMismatch),
        "Base64 で包み直したトークンに ath を流用できた"
    );
    assert!(cache.is_empty(), "拒否した proof を記録した");

    // CWT タグ (61) を付けずに再エンコードした表現。署名対象は変わらない
    let reencoded = cwt_cat_token_bytes_with_options(
        Some(cwt_ckt_confirmation()),
        Some(CatDpop::new(300.0, false)),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
        &CoseEncodingOptions {
            cose_tag: true,
            cwt_tag: false,
        },
    );
    let reencoded_token =
        CatToken::decode(&reencoded).expect("再エンコードした CAT をデコードできる");
    assert_eq!(
        reencoded_token.claims(),
        token.claims(),
        "同じトークンのクレームとしてデコードされる"
    );
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &reencoded_token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::AccessTokenHashMismatch),
        "再エンコードしたトークンに ath を流用できた"
    );
    assert!(cache.is_empty(), "拒否した proof を記録した");

    // 提示されたバイト列そのものに対しては受理する
    let mut cache = verifier.cache();
    verifier
        .verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        )
        .expect("提示されたバイト列の ath は一致する");
}

/// `actx.action` が要求 action と一致しない proof を拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_rejects_action_mismatch() {
    let token = cwt_default_token();
    let claims = cwt_claims(
        &token,
        MoqtAction::Fetch,
        Some(cwt_namespace_fields()),
        Some(CWT_TRACK_NAME.to_vec()),
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    );
    let proof = cwt_proof(&claims);
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::ActionMismatch)
    );
    assert!(cache.is_empty(), "拒否した proof を記録した");
}

/// `actx` の namespace / track が要求とバイト単位で一致しない proof を拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_rejects_target_mismatches() {
    let token = cwt_default_token();
    let verifier = CwtVerifier::new();
    let namespace = cwt_namespace();

    // namespace のフィールド値が違う
    let claims = cwt_claims(
        &token,
        MoqtAction::Subscribe,
        Some(vec![b"cdn".to_vec(), b"example.org".to_vec()]),
        Some(CWT_TRACK_NAME.to_vec()),
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    );
    let proof = cwt_proof(&claims);
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::TargetMismatch),
        "namespace の不一致を受理した"
    );

    // namespace のフィールド数が違う
    let claims = cwt_claims(
        &token,
        MoqtAction::Subscribe,
        Some(vec![b"cdn".to_vec()]),
        Some(CWT_TRACK_NAME.to_vec()),
        CWT_REFERENCE_TIME as i64,
        b"proof-2",
    );
    let proof = cwt_proof(&claims);
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::TargetMismatch),
        "namespace の長さの不一致を受理した"
    );

    // track name のバイト列が違う
    let claims = cwt_claims(
        &token,
        MoqtAction::Subscribe,
        Some(cwt_namespace_fields()),
        Some(b"/sports/vod".to_vec()),
        CWT_REFERENCE_TIME as i64,
        b"proof-3",
    );
    let proof = cwt_proof(&claims);
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::TargetMismatch),
        "track name の不一致を受理した"
    );
    assert!(cache.is_empty(), "拒否した proof を記録した");
}

/// SETUP は namespace / track の欠落を受理し、余分な target を拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_setup_accepts_missing_target_and_rejects_extra() {
    let token = cwt_action_token(MoqtAction::Setup);
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    let claims = cwt_claims(
        &token,
        MoqtAction::Setup,
        None,
        None,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    );
    let proof = cwt_proof(&claims);
    verifier
        .verify(&proof, &token, MoqtAction::Setup, None, None, &mut cache)
        .expect("SETUP は namespace / track を持たなくても受理する");

    // proof 側に余分な namespace がある
    let claims = cwt_claims(
        &token,
        MoqtAction::Setup,
        Some(Vec::new()),
        None,
        CWT_REFERENCE_TIME as i64,
        b"proof-2",
    );
    let proof = cwt_proof(&claims);
    assert_eq!(
        verifier.verify(&proof, &token, MoqtAction::Setup, None, None, &mut cache),
        Err(DpopError::UnexpectedActxTarget("namespace"))
    );

    // 空の track でも、存在すれば SETUP には余分である
    let claims = cwt_claims(
        &token,
        MoqtAction::Setup,
        None,
        Some(Vec::new()),
        CWT_REFERENCE_TIME as i64,
        b"proof-3",
    );
    let proof = cwt_proof(&claims);
    assert_eq!(
        verifier.verify(&proof, &token, MoqtAction::Setup, None, None, &mut cache),
        Err(DpopError::UnexpectedActxTarget("track"))
    );

    // 要求側に余分な namespace / track がある
    let claims = cwt_claims(
        &token,
        MoqtAction::Setup,
        None,
        None,
        CWT_REFERENCE_TIME as i64,
        b"proof-4",
    );
    let proof = cwt_proof(&claims);
    let empty_namespace: [&[u8]; 0] = [];
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Setup,
            Some(&empty_namespace),
            None,
            &mut cache,
        ),
        Err(DpopError::UnexpectedRequestTarget("namespace"))
    );
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Setup,
            None,
            Some(b""),
            &mut cache,
        ),
        Err(DpopError::UnexpectedRequestTarget("track"))
    );
}

/// namespace を対象とする action は namespace を必須とし、track を拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_namespace_actions_require_namespace_only() {
    let token = cwt_action_token(MoqtAction::PublishNamespace);
    let verifier = CwtVerifier::new();
    let namespace = cwt_namespace();
    let mut cache = verifier.cache();

    let claims = cwt_claims(
        &token,
        MoqtAction::PublishNamespace,
        Some(cwt_namespace_fields()),
        None,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    );
    let proof = cwt_proof(&claims);
    verifier
        .verify(
            &proof,
            &token,
            MoqtAction::PublishNamespace,
            Some(&namespace),
            None,
            &mut cache,
        )
        .expect("namespace だけを対象とする action を受理する");

    // proof に余分な track がある
    let claims = cwt_claims(
        &token,
        MoqtAction::PublishNamespace,
        Some(cwt_namespace_fields()),
        Some(Vec::new()),
        CWT_REFERENCE_TIME as i64,
        b"proof-2",
    );
    let proof = cwt_proof(&claims);
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::PublishNamespace,
            Some(&namespace),
            None,
            &mut cache,
        ),
        Err(DpopError::UnexpectedActxTarget("track"))
    );

    // 要求側に track がある
    let claims = cwt_claims(
        &token,
        MoqtAction::PublishNamespace,
        Some(cwt_namespace_fields()),
        None,
        CWT_REFERENCE_TIME as i64,
        b"proof-3",
    );
    let proof = cwt_proof(&claims);
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::PublishNamespace,
            Some(&namespace),
            Some(b""),
            &mut cache,
        ),
        Err(DpopError::UnexpectedRequestTarget("track"))
    );

    // proof に namespace が無い
    let claims = cwt_claims(
        &token,
        MoqtAction::PublishNamespace,
        None,
        None,
        CWT_REFERENCE_TIME as i64,
        b"proof-4",
    );
    let proof = cwt_proof(&claims);
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::PublishNamespace,
            Some(&namespace),
            None,
            &mut cache,
        ),
        Err(DpopError::MissingActxTarget("namespace"))
    );

    // 要求に namespace が無い
    let claims = cwt_claims(
        &token,
        MoqtAction::PublishNamespace,
        Some(cwt_namespace_fields()),
        None,
        CWT_REFERENCE_TIME as i64,
        b"proof-5",
    );
    let proof = cwt_proof(&claims);
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::PublishNamespace,
            None,
            None,
            &mut cache,
        ),
        Err(DpopError::MissingRequestTarget("namespace"))
    );
    assert!(cache.is_empty(), "拒否した proof を記録した");
}

/// track を対象とする action は namespace と track の両方を必須とすること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_track_actions_require_namespace_and_track() {
    let token = cwt_default_token();
    let verifier = CwtVerifier::new();
    let namespace = cwt_namespace();
    let mut cache = verifier.cache();

    // 要求に track が無い
    let claims = cwt_claims(
        &token,
        MoqtAction::Subscribe,
        Some(cwt_namespace_fields()),
        Some(CWT_TRACK_NAME.to_vec()),
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    );
    let proof = cwt_proof(&claims);
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            None,
            &mut cache,
        ),
        Err(DpopError::MissingRequestTarget("track"))
    );

    // 要求に namespace が無い
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            None,
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::MissingRequestTarget("namespace"))
    );

    // proof に track が無い
    let claims = cwt_claims(
        &token,
        MoqtAction::Subscribe,
        Some(cwt_namespace_fields()),
        None,
        CWT_REFERENCE_TIME as i64,
        b"proof-2",
    );
    let proof = cwt_proof(&claims);
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::MissingActxTarget("track"))
    );

    // proof に namespace が無い
    let claims = cwt_claims(
        &token,
        MoqtAction::Subscribe,
        None,
        Some(CWT_TRACK_NAME.to_vec()),
        CWT_REFERENCE_TIME as i64,
        b"proof-3",
    );
    let proof = cwt_proof(&claims);
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::MissingActxTarget("namespace"))
    );
}

/// 欠落した target と空の target を区別すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_distinguishes_missing_and_empty_targets() {
    let token = cwt_default_token();
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    let empty_namespace: [&[u8]; 0] = [];

    // 空の namespace と空の track は、要求側も空なら存在する target として一致する
    let claims = cwt_claims(
        &token,
        MoqtAction::Subscribe,
        Some(Vec::new()),
        Some(Vec::new()),
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    );
    let proof = cwt_proof(&claims);
    verifier
        .verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&empty_namespace),
            Some(b""),
            &mut cache,
        )
        .expect("空の target 同士は一致する");

    // 空の namespace を持つ proof は、SETUP の欠落と区別して拒否する
    let setup_token = cwt_action_token(MoqtAction::Setup);
    let claims = cwt_claims(
        &setup_token,
        MoqtAction::Setup,
        Some(Vec::new()),
        None,
        CWT_REFERENCE_TIME as i64,
        b"proof-2",
    );
    let proof = cwt_proof(&claims);
    assert_eq!(
        verifier.verify(
            &proof,
            &setup_token,
            MoqtAction::Setup,
            None,
            None,
            &mut cache
        ),
        Err(DpopError::UnexpectedActxTarget("namespace")),
        "空の namespace を SETUP の欠落として扱った"
    );

    // 空の namespace と 1 フィールドの namespace は区別する
    let namespace = cwt_namespace();
    let claims = cwt_claims(
        &token,
        MoqtAction::Subscribe,
        Some(Vec::new()),
        Some(CWT_TRACK_NAME.to_vec()),
        CWT_REFERENCE_TIME as i64,
        b"proof-3",
    );
    let proof = cwt_proof(&claims);
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::TargetMismatch)
    );

    // 空の track name と track の欠落は区別する
    let claims = cwt_claims(
        &token,
        MoqtAction::Subscribe,
        Some(cwt_namespace_fields()),
        Some(Vec::new()),
        CWT_REFERENCE_TIME as i64,
        b"proof-4",
    );
    let proof = cwt_proof(&claims);
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            None,
            &mut cache,
        ),
        Err(DpopError::MissingRequestTarget("track"))
    );
}

/// 鮮度は iat の前後に同じウィンドウを適用し、境界を受理すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_freshness_accepts_window_boundaries() {
    let token = cwt_cat_token(
        Some(cwt_ckt_confirmation()),
        Some(CatDpop::new(300.0, false)),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    let reference = CWT_REFERENCE_TIME as i64;

    for (offset, expected) in [
        (0, None),
        (300, None),
        (-300, None),
        // iat が未来すぎる場合は ProofNotYetValid、古すぎる場合は ProofExpired
        (301, Some(DpopError::ProofNotYetValid)),
        (-301, Some(DpopError::ProofExpired)),
    ] {
        let claims = cwt_subscribe_claims(&token, reference + offset, b"proof-1");
        let proof = cwt_proof(&claims);
        let result = verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        );
        assert_eq!(result, expected.map_or(Ok(()), Err), "offset={offset}");
    }
}

/// `catdpop` にウィンドウが無い場合は呼び出し側が指定した既定値を使うこと
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_freshness_uses_default_window() {
    let token = cwt_cat_token(
        Some(cwt_ckt_confirmation()),
        None,
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    let reference = CWT_REFERENCE_TIME as i64;

    let claims = cwt_subscribe_claims(&token, reference - 300, b"proof-1");
    let proof = cwt_proof(&claims);
    verifier
        .verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        )
        .expect("既定のウィンドウの境界を受理する");

    let claims = cwt_subscribe_claims(&token, reference - 301, b"proof-2");
    let proof = cwt_proof(&claims);
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::ProofExpired)
    );

    // ウィンドウを持たない catdpop (honor_jti だけ) でも既定値を使う
    let token = cwt_cat_token(
        Some(cwt_ckt_confirmation()),
        Some(CatDpop {
            window_seconds: None,
            honor_jti: Some(false),
            raw: Vec::new(),
        }),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let claims = cwt_subscribe_claims(&token, reference - 300, b"proof-3");
    let proof = cwt_proof(&claims);
    verifier
        .verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        )
        .expect("catdpop にウィンドウが無ければ既定値を用いる");
}

/// 負のウィンドウや非有限の参照時刻を拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_rejects_invalid_freshness_parameters() {
    let token = cwt_cat_token(
        Some(cwt_ckt_confirmation()),
        Some(CatDpop::new(-1.0, false)),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    let claims = cwt_subscribe_claims(&token, CWT_REFERENCE_TIME as i64, b"proof-1");
    let proof = cwt_proof(&claims);
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::InvalidFreshnessWindow),
        "負の catdpop ウィンドウを受理した"
    );

    // catdpop が無く、既定値が負の場合も拒否する
    let token = cwt_cat_token(
        Some(cwt_ckt_confirmation()),
        None,
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let claims = cwt_subscribe_claims(&token, CWT_REFERENCE_TIME as i64, b"proof-2");
    let proof = cwt_proof(&claims);
    let mut negative = CwtVerifier::new();
    negative.default_window_seconds = -1.0;
    assert_eq!(
        negative.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::InvalidFreshnessWindow),
        "負の既定ウィンドウを受理した"
    );

    // 単体 API では非有限の参照時刻とウィンドウも拒否する
    assert_eq!(
        proof.verify_freshness(f64::NAN, 0.0),
        Err(DpopError::NonFiniteNumber("reference time"))
    );
    assert_eq!(
        proof.verify_freshness(CWT_REFERENCE_TIME, f64::NAN),
        Err(DpopError::InvalidFreshnessWindow)
    );
    assert_eq!(
        proof.verify_freshness(CWT_REFERENCE_TIME, f64::NEG_INFINITY),
        Err(DpopError::InvalidFreshnessWindow)
    );
}

/// 同じ `cti` の proof を再送すると replay として拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_rejects_same_cti_replay() {
    let token = cwt_default_token();
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    let claims = cwt_subscribe_claims(&token, CWT_REFERENCE_TIME as i64, b"proof-1");
    let proof = cwt_proof(&claims);
    verifier
        .verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        )
        .expect("1 回目は受理する");
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::Replayed)
    );
    assert_eq!(cache.len(), 1, "記録数が増減した");
}

/// `catdpop` が replay detection を要求していなければ記録しないこと
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_does_not_record_without_replay_detection() {
    let token = cwt_cat_token(
        Some(cwt_ckt_confirmation()),
        Some(CatDpop::new(300.0, false)),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    let claims = cwt_subscribe_claims(&token, CWT_REFERENCE_TIME as i64, b"proof-1");
    let proof = cwt_proof(&claims);
    for _ in 0..2 {
        verifier
            .verify(
                &proof,
                &token,
                MoqtAction::Subscribe,
                Some(&namespace),
                Some(CWT_TRACK_NAME),
                &mut cache,
            )
            .expect("replay detection が無効なら再送も受理する");
    }
    assert!(cache.is_empty(), "要求されていない cti を記録した");
}

/// CAT の署名が不正な場合は拒否し、リプレイ状態を記録しないこと
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_rejects_invalid_cat_signature_without_recording() {
    let mut token_bytes = cwt_cat_token_bytes(
        Some(cwt_ckt_confirmation()),
        Some(CatDpop::new(300.0, true)),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let last = token_bytes.len() - 1;
    token_bytes[last] ^= 0x01;
    let token = CatToken::decode(&token_bytes).expect("署名を改変した CAT をデコードできる");
    let proof = cwt_proof(&cwt_subscribe_claims(
        &token,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    let error = verifier
        .verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        )
        .expect_err("CAT の署名不正を拒否する");
    assert!(
        matches!(
            error,
            DpopError::Cat(CatError::Crypto(CryptoError::SignatureVerificationFailed))
        ),
        "想定していないエラー: {error}"
    );
    assert!(cache.is_empty(), "拒否した proof を記録した");

    // 同じ cti を持つ正しい proof は受理され、その再送は replay で拒否される
    let good_token = cwt_default_token();
    let good_proof = cwt_proof(&cwt_subscribe_claims(
        &good_token,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    verifier
        .verify(
            &good_proof,
            &good_token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        )
        .expect("失敗後の正しい proof は受理する");
    assert_eq!(
        verifier.verify(
            &good_proof,
            &good_token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::Replayed)
    );
}

/// `moqt-reval` を持つ CAT を、`claim_options` の再検証能力に応じて受理・拒否すること
///
/// draft-ietf-moq-c4m-02 §2.2。拒否した場合は proof の `cti` を記録しない。
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_checks_moqt_reval_against_revalidation_capability() {
    let namespace = cwt_namespace();
    let claims = CatClaims {
        issuer: Some(String::from(CWT_ISSUER)),
        audience: vec![String::from(CWT_AUDIENCE)],
        expiration: Some(CWT_REFERENCE_TIME + 3600.0),
        issued_at: Some(CWT_REFERENCE_TIME - 10.0),
        cwt_id: Some(b"cat-token-reval".to_vec()),
        confirmation: Some(cwt_ckt_confirmation()),
        catdpop: Some(CatDpop::new(300.0, true)),
        moqt: Some(cwt_scope(&[MoqtAction::Subscribe])),
        moqt_reval: Some(120.0),
        ..CatClaims::default()
    };
    let token_bytes = CatTokenBuilder {
        claims,
        ..CatTokenBuilder::default()
    }
    .build_cose_with(
        &AwsLcRsCrypto::new(),
        &es256_private_key(),
        &CoseEncodingOptions::default(),
    )
    .expect("moqt-reval 付きの CAT を発行できる");
    let token = CatToken::decode(&token_bytes).expect("発行した CAT をデコードできる");
    let proof = cwt_proof(&cwt_subscribe_claims(
        &token,
        CWT_REFERENCE_TIME as i64,
        b"proof-reval",
    ));

    // 既定 (再検証できない) では拒否し、記録しない
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::Cat(CatError::Claims(
            ClaimValidationError::RevalidationUnsupported
        )))
    );
    assert!(cache.is_empty(), "再検証できない受信者で記録した");

    // 最小間隔が moqt-reval より長い受信者は拒否し、記録しない
    let mut too_slow = CwtVerifier::new();
    too_slow.revalidation_interval_seconds = Some(300.0);
    assert_eq!(
        too_slow.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::Cat(CatError::Claims(
            ClaimValidationError::RevalidationIntervalTooShort
        )))
    );
    assert!(cache.is_empty(), "最小間隔を下回る moqt-reval で記録した");

    // moqt-reval の間隔以下で再検証できる受信者は受理し、cti を記録する
    let mut capable = CwtVerifier::new();
    capable.revalidation_interval_seconds = Some(60.0);
    capable
        .verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        )
        .expect("moqt-reval の間隔で再検証できる受信者は受理する");
    assert_eq!(cache.len(), 1);
}

/// CAT の通常クレーム (期限・issuer・audience) が不正な場合は拒否し、記録しないこと
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_rejects_invalid_cat_claims_without_recording() {
    let namespace = cwt_namespace();

    // 期限切れ
    let expired = cwt_cat_token(
        Some(cwt_ckt_confirmation()),
        Some(CatDpop::new(300.0, true)),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME - 1.0,
    );
    let proof = cwt_proof(&cwt_subscribe_claims(
        &expired,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &expired,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::Cat(CatError::Claims(
            ClaimValidationError::Expired
        )))
    );
    assert!(cache.is_empty(), "期限切れの CAT で記録した");

    // issuer の不一致
    let token = cwt_default_token();
    let proof = cwt_proof(&cwt_subscribe_claims(
        &token,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    let mut wrong_issuer = CwtVerifier::new();
    wrong_issuer.expected_issuers = &["other.example"];
    assert_eq!(
        wrong_issuer.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::Cat(CatError::Claims(
            ClaimValidationError::IssuerMismatch
        )))
    );
    assert!(cache.is_empty(), "issuer が不一致の CAT で記録した");

    // audience の不一致
    let mut wrong_audience = CwtVerifier::new();
    wrong_audience.expected_audiences = &["other.example"];
    assert_eq!(
        wrong_audience.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::Cat(CatError::Claims(
            ClaimValidationError::AudienceMismatch
        )))
    );
    assert!(cache.is_empty(), "audience が不一致の CAT で記録した");

    // 失敗後、同じ cti の正しい proof は受理され、再送は replay で拒否される
    let good_token = cwt_default_token();
    let good_proof = cwt_proof(&cwt_subscribe_claims(
        &good_token,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    verifier
        .verify(
            &good_proof,
            &good_token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        )
        .expect("失敗後の正しい proof は受理する");
    assert_eq!(
        verifier.verify(
            &good_proof,
            &good_token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::Replayed)
    );
}

/// `moqt` クレームの scope が要求 action を認可しない場合は拒否し、記録しないこと
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_rejects_unauthorized_scope_without_recording() {
    let token = cwt_cat_token(
        Some(cwt_ckt_confirmation()),
        Some(CatDpop::new(300.0, true)),
        &[MoqtAction::Fetch],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let proof = cwt_proof(&cwt_subscribe_claims(
        &token,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::Cat(CatError::ActionNotAuthorized(
            MoqtAction::Subscribe
        )))
    );
    assert!(cache.is_empty(), "scope 非認可の proof を記録した");

    // 同じ cti の正しい proof は受理され、再送は replay で拒否される
    let good_token = cwt_default_token();
    let good_proof = cwt_proof(&cwt_subscribe_claims(
        &good_token,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    verifier
        .verify(
            &good_proof,
            &good_token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        )
        .expect("失敗後の正しい proof は受理する");
    assert_eq!(
        verifier.verify(
            &good_proof,
            &good_token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::Replayed)
    );
}

/// 小さいウィンドウの proof の後でも、大きいウィンドウの記録を早期に破棄しないこと
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_replay_cache_keeps_entries_across_windows() {
    let namespace = cwt_namespace();
    let mut verifier = CwtVerifier::new();
    let mut cache = verifier.cache();

    // window 60 秒の proof を cti "proof-a" で記録する
    let token_a = cwt_cat_token(
        Some(cwt_ckt_confirmation()),
        Some(CatDpop::new(60.0, true)),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let reference = CWT_REFERENCE_TIME as i64;
    let proof_a = cwt_proof(&cwt_subscribe_claims(&token_a, reference, b"proof-a"));
    verifier
        .verify(
            &proof_a,
            &token_a,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        )
        .expect("1 つ目の proof を受理する");

    // 5 秒後に window 10 秒の proof を処理する。現在のウィンドウ (10 秒) で古い記録を
    // 破棄すると、proof-a の記録は残り 300 秒の保持期間内にもかかわらず消える
    verifier.reference_time_seconds = CWT_REFERENCE_TIME + 5.0;
    let token_b = cwt_cat_token(
        Some(cwt_ckt_confirmation()),
        Some(CatDpop::new(10.0, true)),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let proof_b = cwt_proof(&cwt_subscribe_claims(&token_b, reference, b"proof-b"));
    verifier
        .verify(
            &proof_b,
            &token_b,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        )
        .expect("2 つ目の proof を受理する");
    assert_eq!(cache.len(), 2, "大きいウィンドウの記録を早期に破棄した");

    // proof-a はまだ鮮度ウィンドウ内 (60 秒) なので、再送は replay で拒否する
    assert_eq!(
        verifier.verify(
            &proof_a,
            &token_a,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::Replayed)
    );
}

/// キャッシュの最大ウィンドウを超える proof を拒否し、より大きいキャッシュでは受理すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_replay_cache_rejects_window_over_maximum() {
    let token = cwt_cat_token(
        Some(cwt_ckt_confirmation()),
        Some(CatDpop::new(300.0, true)),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let proof = cwt_proof(&cwt_subscribe_claims(
        &token,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();

    let mut small = CwtDpopReplayCache::new(60.0).expect("キャッシュを作れる");
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut small,
        ),
        Err(DpopError::ReplayWindowTooLarge)
    );
    assert!(small.is_empty(), "ウィンドウ超過の proof を記録した");

    // 失敗後に最大ウィンドウの大きいキャッシュで再試行すると受理される
    let mut large = verifier.cache();
    verifier
        .verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut large,
        )
        .expect("十分に大きい最大ウィンドウなら受理する");
    assert_eq!(large.len(), 1);
}

/// リプレイキャッシュの構築時に有限かつ正の最大ウィンドウを要求すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_replay_cache_rejects_invalid_maximum_window() {
    for max_window_seconds in [0.0, -0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(
            matches!(
                CwtDpopReplayCache::new(max_window_seconds),
                Err(DpopError::InvalidReplayWindow)
            ),
            "max_window_seconds={max_window_seconds} を受理した"
        );
    }
    let cache = CwtDpopReplayCache::new(300.0).expect("キャッシュを作れる");
    assert_eq!(cache.max_window_seconds(), 300.0);
    assert!(cache.is_empty());
}

/// 保持期限 (`iat` + 最大ウィンドウ) と等しい時刻までは記録を保持すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_replay_cache_holds_until_iat_plus_max_window() {
    let mut cache = CwtDpopReplayCache::new(300.0).expect("キャッシュを作れる");
    cache
        .check_and_record(b"cti", 100.0, 100.0, 60.0)
        .expect("記録できる");

    // 保持期限と等しい時刻では保持する
    assert_eq!(
        cache.check_and_record(b"cti", 100.0, 400.0, 60.0),
        Err(DpopError::Replayed)
    );
    // 保持期限を超えた後だけ解放する
    cache
        .check_and_record(b"cti", 100.0, 400.001, 60.0)
        .expect("保持期限を超えたら再利用を検出しない");
    assert_eq!(cache.len(), 1);

    // 現在の proof のウィンドウは保持期間に使わない
    let mut cache = CwtDpopReplayCache::new(300.0).expect("キャッシュを作れる");
    cache
        .check_and_record(b"cti", 100.0, 100.0, 10.0)
        .expect("記録できる");
    assert_eq!(
        cache.check_and_record(b"cti", 100.0, 200.0, 10.0),
        Err(DpopError::Replayed),
        "小さいウィンドウで記録を早期に破棄した"
    );

    // 不正な入力は記録を変えない
    assert_eq!(
        cache.check_and_record(b"cti-2", f64::NAN, 100.0, 10.0),
        Err(DpopError::NonFiniteNumber("iat"))
    );
    assert_eq!(
        cache.check_and_record(b"cti-2", 100.0, f64::NAN, 10.0),
        Err(DpopError::NonFiniteNumber("reference time"))
    );
    assert_eq!(
        cache.check_and_record(b"cti-2", 100.0, 100.0, f64::NAN),
        Err(DpopError::InvalidFreshnessWindow)
    );
    assert_eq!(
        cache.check_and_record(b"cti-2", 100.0, 100.0, -1.0),
        Err(DpopError::InvalidFreshnessWindow)
    );
    assert_eq!(
        cache.check_and_record(b"cti-2", 100.0, 100.0, 301.0),
        Err(DpopError::ReplayWindowTooLarge)
    );
    assert_eq!(cache.len(), 1, "拒否した入力で記録が変化した");
}

/// proof をデコードした label 設定と検証時の設定が矛盾する場合は拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_rejects_claim_label_mismatch() {
    let token = cwt_default_token();
    let mut claims = cwt_subscribe_claims(&token, CWT_REFERENCE_TIME as i64, b"proof-1");
    // デコードに使う label 設定とは別の label を未知のクレームとして持たせる
    claims
        .raw
        .push((Value::integer(-65541), Value::ByteString(vec![0x01, 0x02])));
    let proof = cwt_proof(&claims);
    let namespace = cwt_namespace();
    let mut verifier = CwtVerifier::new();
    verifier.codec = CwtDpopProofCodec::new(-65541, -65542).expect("label 設定を作れる");
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::ClaimLabelMismatch)
    );
    assert!(cache.is_empty(), "拒否した proof を記録した");
}

/// 保持期限が有限でない場合は拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_replay_cache_rejects_non_finite_retention() {
    // iat と最大ウィンドウの両方が大きいと `iat + max_window_seconds` が無限大になる
    let mut cache = CwtDpopReplayCache::new(f64::MAX).expect("キャッシュを作れる");
    assert_eq!(
        cache.check_and_record(b"cti", f64::MAX, 0.0, 1.0),
        Err(DpopError::NonFiniteNumber("replay retention"))
    );
    assert!(cache.is_empty());
}

/// replay detection を行わない proof でも、cache の最大ウィンドウを超える proof は拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_rejects_window_over_maximum_without_replay_detection() {
    // honor_jti が偽でも catdpop の window は鮮度に使われる
    let token = cwt_cat_token(
        Some(cwt_ckt_confirmation()),
        Some(CatDpop::new(301.0, false)),
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let proof = cwt_proof(&cwt_subscribe_claims(
        &token,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::ReplayWindowTooLarge),
        "replay detection が無くても cache の上限超過は拒否する"
    );
    assert!(cache.is_empty());
}

/// `cnf` と `catreplay` が併存するトークンを拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_rejects_catreplay_with_confirmation() {
    let crypto = AwsLcRsCrypto::new();
    let claims = CatClaims {
        issuer: Some(String::from(CWT_ISSUER)),
        audience: vec![String::from(CWT_AUDIENCE)],
        expiration: Some(CWT_REFERENCE_TIME + 3600.0),
        cwt_id: Some(b"cat-token-1".to_vec()),
        confirmation: Some(cwt_ckt_confirmation()),
        cat_replay: Some(CatReplay::SingleConnection),
        moqt: Some(cwt_scope(&[MoqtAction::Subscribe])),
        ..CatClaims::default()
    };
    let token_bytes = CatTokenBuilder {
        claims,
        ..CatTokenBuilder::default()
    }
    .build_cose_with(
        &crypto,
        &es256_private_key(),
        &CoseEncodingOptions::default(),
    )
    .expect("CAT を発行できる");
    let token = CatToken::decode(&token_bytes).expect("CAT をデコードできる");
    let proof = cwt_proof(&cwt_subscribe_claims(
        &token,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::CatReplayWithConfirmation)
    );
    assert!(cache.is_empty());
}

/// `catdpop` に window が無い場合の既定値も入口で検証すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_rejects_invalid_default_window() {
    let token = cwt_cat_token(
        Some(cwt_ckt_confirmation()),
        None,
        &[MoqtAction::Subscribe],
        CWT_REFERENCE_TIME + 3600.0,
    );
    let proof = cwt_proof(&cwt_subscribe_claims(
        &token,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    let namespace = cwt_namespace();
    let mut verifier = CwtVerifier::new();
    verifier.default_window_seconds = f64::NAN;
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::InvalidFreshnessWindow)
    );
    assert!(cache.is_empty());
}

/// 実 ES384 / EdDSA の proof でも受理入口を通ること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_accepts_es384_and_eddsa_proofs() {
    let namespace = cwt_namespace();
    let (es384_private, es384_public) = es384_keys();
    let (ed25519_private, ed25519_public) = ed25519_keys();
    for (private_key, public_key) in [
        (es384_private, es384_public),
        (ed25519_private, ed25519_public),
    ] {
        let token = cwt_cat_token(
            Some(Confirmation {
                cose_key_thumbprint: Some(cwt_ckt(&public_key)),
                ..Confirmation::default()
            }),
            Some(CatDpop::new(300.0, false)),
            &[MoqtAction::Subscribe],
            CWT_REFERENCE_TIME + 3600.0,
        );
        let proof = cwt_proof_with_key(
            &cwt_subscribe_claims(&token, CWT_REFERENCE_TIME as i64, b"proof-1"),
            &private_key,
        );
        let verifier = CwtVerifier::new();
        let mut cache = verifier.cache();
        verifier
            .verify(
                &proof,
                &token,
                MoqtAction::Subscribe,
                Some(&namespace),
                Some(CWT_TRACK_NAME),
                &mut cache,
            )
            .expect("ES384 / EdDSA の proof が受理される");
    }
}

/// `catdpop` に window があっても既定 window の不正値を拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_rejects_invalid_default_window_even_with_catdpop_window() {
    let token = cwt_default_token();
    let proof = cwt_proof(&cwt_subscribe_claims(
        &token,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    let namespace = cwt_namespace();
    let mut verifier = CwtVerifier::new();
    verifier.default_window_seconds = f64::NAN;
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::InvalidFreshnessWindow)
    );
    assert!(cache.is_empty());
}

/// `catreplay` が 0 なら受理し、2 (実装定義) は拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cwt_dpop_handles_catreplay_values() {
    let crypto = AwsLcRsCrypto::new();
    let build_token = |replay| {
        let claims = CatClaims {
            issuer: Some(String::from(CWT_ISSUER)),
            audience: vec![String::from(CWT_AUDIENCE)],
            expiration: Some(CWT_REFERENCE_TIME + 3600.0),
            cwt_id: Some(b"cat-token-1".to_vec()),
            confirmation: Some(cwt_ckt_confirmation()),
            cat_replay: Some(replay),
            moqt: Some(cwt_scope(&[MoqtAction::Subscribe])),
            ..CatClaims::default()
        };
        CatToken::decode(
            &CatTokenBuilder {
                claims,
                ..CatTokenBuilder::default()
            }
            .build_cose_with(
                &crypto,
                &es256_private_key(),
                &CoseEncodingOptions::default(),
            )
            .expect("CAT を発行できる"),
        )
        .expect("CAT をデコードできる")
    };
    let namespace = cwt_namespace();
    let verifier = CwtVerifier::new();

    // catreplay=0 は replay 保護なしとして受理する
    let token = build_token(CatReplay::NotProtected);
    let proof = cwt_proof(&cwt_subscribe_claims(
        &token,
        CWT_REFERENCE_TIME as i64,
        b"proof-1",
    ));
    let mut cache = verifier.cache();
    verifier
        .verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        )
        .expect("catreplay=0 は受理する");
    assert!(cache.is_empty(), "catreplay=0 で記録した");

    // catreplay=2 (実装定義) は cnf との併存を解釈しないため拒否する
    let token = build_token(CatReplay::ImplementationDefined);
    let proof = cwt_proof(&cwt_subscribe_claims(
        &token,
        CWT_REFERENCE_TIME as i64,
        b"proof-2",
    ));
    let mut cache = verifier.cache();
    assert_eq!(
        verifier.verify(
            &proof,
            &token,
            MoqtAction::Subscribe,
            Some(&namespace),
            Some(CWT_TRACK_NAME),
            &mut cache,
        ),
        Err(DpopError::CatReplayWithConfirmation)
    );
}
