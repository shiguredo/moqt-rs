//! bearer トークンの接続束縛のテスト
//!
//! draft-ietf-moq-c4m-02 §3 / §7.2 の catreplay=1 を、同じ接続では何度でも
//! 再利用でき、別の接続ではトークンが受理され得る間ずっと拒否する「単一接続」の
//! 意味で検証する。

use shiguredo_moqt::c4m::cat::{BearerReplayCache, BearerReplayError};

#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::cat::{
    BearerVerificationOptions, CAT_CONTENT_TYPE, CatError, CatReplay, CatToken, CatTokenBuilder,
    ClaimValidationError, VerifyOptions,
};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::cose::Algorithm;
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::crypto::CoseKey;
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::{MoqtAction, MoqtClaim, MoqtScope};

/// テストで使う期待 issuer
#[cfg(feature = "aws-lc-rs")]
const ISSUER: &str = "https://auth.example.com";

/// テストで使う期待 audience
#[cfg(feature = "aws-lc-rs")]
const AUDIENCE: &str = "https://relay.example.com";

/// テストで使う名前空間のフィールド
#[cfg(feature = "aws-lc-rs")]
const NAMESPACE_FIELD: &[u8] = b"example";

/// テストで使う名前空間
#[cfg(feature = "aws-lc-rs")]
const NAMESPACE: &[&[u8]] = &[NAMESPACE_FIELD];

/// テストで使うトラック名
#[cfg(feature = "aws-lc-rs")]
const TRACK_NAME: &[u8] = b"video";

/// 期待する issuer の一覧
#[cfg(feature = "aws-lc-rs")]
const EXPECTED_ISSUERS: &[&str] = &[ISSUER];

/// 期待する audience の一覧
#[cfg(feature = "aws-lc-rs")]
const EXPECTED_AUDIENCES: &[&str] = &[AUDIENCE];

/// テストで使う別の issuer
#[cfg(feature = "aws-lc-rs")]
const OTHER_ISSUERS: &[&str] = &["https://other.example.com"];

/// cache の構築時に leeway を検証すること
#[test]
fn bearer_replay_cache_rejects_invalid_leeway() {
    // 非有限値と負の値は構築できない
    for leeway in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        assert_eq!(
            BearerReplayCache::new(leeway).map(|_| ()),
            Err(BearerReplayError::InvalidLeeway),
            "不正な leeway を受理した: {leeway}"
        );
    }
    // 許容ずれ 0 (leeway なし) でも構築でき、replay 保護は有効なままである
    let cache = BearerReplayCache::new(0.0).expect("leeway 0 で構築できる");
    assert_eq!(cache.leeway_seconds(), 0.0);
    assert!(cache.is_empty());
}

/// 同じ cti を同じ接続で使うと再利用として受理すること
#[test]
fn bearer_replay_cache_allows_reuse_on_same_connection() {
    let mut cache = BearerReplayCache::new(10.0).expect("cache を構築できる");
    cache
        .check_and_bind(b"cti-1", b"conn-a", 1100.0, 1000.0)
        .expect("初回の束縛は登録できる");
    cache
        .check_and_bind(b"cti-1", b"conn-a", 1100.0, 1000.0)
        .expect("同じ接続なら何度でも再利用できる");
    assert_eq!(cache.len(), 1);
    assert!(!cache.is_empty());
}

/// 同じ cti を別の接続で使うと保持期限まで拒否されること
#[test]
fn bearer_replay_cache_rejects_other_connection_until_retention() {
    let mut cache = BearerReplayCache::new(10.0).expect("cache を構築できる");
    cache
        .check_and_bind(b"cti-1", b"conn-a", 1100.0, 1000.0)
        .expect("conn-a に束縛できる");

    // 接続を削除する操作が無くても、別接続からの利用は拒否される
    assert_eq!(
        cache.check_and_bind(b"cti-1", b"conn-b", 1100.0, 1000.0),
        Err(BearerReplayError::ConnectionMismatch)
    );
    // 参照時刻が保持期限と等しい間は記録を保持する (期限の境界)
    assert_eq!(
        cache.check_and_bind(b"cti-1", b"conn-b", 1100.0, 1100.0),
        Err(BearerReplayError::ConnectionMismatch)
    );
    // 保持期限を超えると記録が解放され、別接続で登録できる
    cache
        .check_and_bind(b"cti-1", b"conn-b", 1200.0, 1100.001)
        .expect("保持期限を超えたら別接続で再利用できる");
    assert_eq!(cache.len(), 1);
}

/// 未失効の記録を追い出さないこと
#[test]
fn bearer_replay_cache_does_not_evict_live_entries() {
    let mut cache = BearerReplayCache::new(0.0).expect("cache を構築できる");
    for cti in [b"cti-1".as_slice(), b"cti-2", b"cti-3", b"cti-4"] {
        cache
            .check_and_bind(cti, b"conn-a", 1100.0, 1000.0)
            .expect("期限内の記録を登録できる");
    }
    assert_eq!(cache.len(), 4);

    // 期限内の参照時刻では、新しい cti を登録しても既存の記録は残る
    cache
        .check_and_bind(b"cti-5", b"conn-b", 2000.0, 1050.0)
        .expect("期限内なら別の記録を追加できる");
    assert_eq!(cache.len(), 5);

    // 参照時刻が保持期限を超えた記録だけが解放される。cti-5 の保持期限は 2000 なので
    // 残り、新しく登録した cti-6 と合わせて 2 件になる
    cache
        .check_and_bind(b"cti-6", b"conn-c", 3000.0, 1100.001)
        .expect("期限切れの記録を解放して登録できる");
    assert_eq!(cache.len(), 2);
    // 残った cti-5 は conn-b に束縛されたままである
    assert_eq!(
        cache.check_and_bind(b"cti-5", b"conn-a", 2000.0, 1150.0),
        Err(BearerReplayError::ConnectionMismatch)
    );
}

/// 同じ接続での再利用時に保持期限が短くならないこと
#[test]
fn bearer_replay_cache_extends_retention_on_same_connection() {
    let mut cache = BearerReplayCache::new(0.0).expect("cache を構築できる");
    cache
        .check_and_bind(b"cti-1", b"conn-a", 1100.0, 1000.0)
        .expect("conn-a に束縛できる");
    // 同じ cti で期限の長いトークンを同じ接続で使うと保持期限が延びる
    cache
        .check_and_bind(b"cti-1", b"conn-a", 2000.0, 1000.0)
        .expect("同じ接続なら再利用できる");
    // 元の保持期限を過ぎても、延びた保持期限までは別接続を拒否する
    assert_eq!(
        cache.check_and_bind(b"cti-1", b"conn-b", 2000.0, 1500.0),
        Err(BearerReplayError::ConnectionMismatch)
    );
    // 期限の短い値では保持期限を縮めない
    cache
        .check_and_bind(b"cti-1", b"conn-a", 1600.0, 1500.0)
        .expect("同じ接続なら再利用できる");
    assert_eq!(
        cache.check_and_bind(b"cti-1", b"conn-b", 2000.0, 1900.0),
        Err(BearerReplayError::ConnectionMismatch)
    );
}

/// 非有限の参照時刻と保持期限を拒否し、記録を変更しないこと
#[test]
fn bearer_replay_cache_rejects_non_finite_arguments() {
    let mut cache = BearerReplayCache::new(10.0).expect("cache を構築できる");
    assert_eq!(
        cache.check_and_bind(b"cti-1", b"conn-a", f64::INFINITY, 1000.0),
        Err(BearerReplayError::NonFiniteRetention)
    );
    assert_eq!(
        cache.check_and_bind(b"cti-1", b"conn-a", 1100.0, f64::NAN),
        Err(BearerReplayError::NonFiniteReferenceTime)
    );
    assert!(cache.is_empty());
}

/// 空の cti と接続識別子を拒否し、記録を変更しないこと
#[test]
fn bearer_replay_cache_rejects_empty_identifiers() {
    let mut cache = BearerReplayCache::new(10.0).expect("cache を構築できる");
    assert_eq!(
        cache.check_and_bind(b"", b"conn-a", 1100.0, 1000.0),
        Err(BearerReplayError::EmptyCti)
    );
    assert_eq!(
        cache.check_and_bind(b"cti-1", b"", 1100.0, 1000.0),
        Err(BearerReplayError::EmptyConnectionId)
    );
    assert!(cache.is_empty());
    // 既存の束縛がある状態でも、空の識別子はその束縛を変更しない
    cache
        .check_and_bind(b"cti-1", b"conn-a", 1100.0, 1000.0)
        .expect("conn-a に束縛できる");
    assert_eq!(
        cache.check_and_bind(b"cti-1", b"", 1100.0, 1000.0),
        Err(BearerReplayError::EmptyConnectionId)
    );
    assert_eq!(
        cache.check_and_bind(b"cti-1", b"conn-b", 1100.0, 1000.0),
        Err(BearerReplayError::ConnectionMismatch)
    );
}

/// bearer トークンを発行する
///
/// スコープは action だけを制限し、名前空間とトラック名は制限しない。
#[cfg(feature = "aws-lc-rs")]
fn bearer_token(
    expiration: Option<f64>,
    cti: Option<&[u8]>,
    cat_replay: Option<CatReplay>,
    actions: &[MoqtAction],
) -> CatToken {
    let crypto = AwsLcRsCrypto::new();
    let mut builder = CatTokenBuilder::new().issuer(ISSUER).audience(AUDIENCE);
    if let Some(expiration) = expiration {
        builder = builder.expiration(expiration);
    }
    if let Some(cti) = cti {
        builder = builder.cwt_id(cti.to_vec());
    }
    if let Some(cat_replay) = cat_replay {
        builder = builder.cat_replay(cat_replay);
    }
    let token_bytes = builder
        .moqt(MoqtClaim::new().scope(MoqtScope::new(actions.iter().copied())))
        .build_cose(&crypto, &super::hmac_key())
        .expect("bearer トークンを発行できる");
    CatToken::decode(&token_bytes).expect("bearer トークンをデコードできる")
}

/// bearer 検証のオプションを組み立てる
#[cfg(feature = "aws-lc-rs")]
fn verification_options<'a>(
    reference_time_seconds: f64,
    action: MoqtAction,
    connection_id: &'a [u8],
) -> BearerVerificationOptions<'a> {
    BearerVerificationOptions {
        reference_time_seconds,
        expected_issuers: EXPECTED_ISSUERS,
        expected_audiences: EXPECTED_AUDIENCES,
        verify_options: VerifyOptions {
            expected_algorithm: Some(Algorithm::HmacSha256),
            expected_type: Some(CAT_CONTENT_TYPE),
        },
        action,
        namespace: NAMESPACE,
        track_name: TRACK_NAME,
        connection_id,
    }
}

/// 同じ接続では同じトークンを複数の action で再利用できること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_bearer_allows_multiple_actions_on_same_connection() {
    let token = bearer_token(
        Some(1000.0),
        Some(b"cti-1"),
        Some(CatReplay::SingleConnection),
        &[MoqtAction::Setup, MoqtAction::Subscribe],
    );
    let crypto = AwsLcRsCrypto::new();
    let mut cache = BearerReplayCache::new(10.0).expect("cache を構築できる");

    token
        .verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1000.0, MoqtAction::Setup, b"conn-a"),
            &mut cache,
        )
        .expect("SETUP は受理される");
    token
        .verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1000.0, MoqtAction::Subscribe, b"conn-a"),
            &mut cache,
        )
        .expect("同じ接続の後続 action は受理される");
    assert_eq!(cache.len(), 1);
}

/// 同じ cti の別接続での利用が保持期限まで拒否され、期限後に解放されること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_bearer_rejects_other_connection_until_retention() {
    // 同じ cti を持つ 2 つのトークン。2 つ目は期限が後なので、1 つ目の期限を
    // 過ぎた後も通常の期限検証は通る
    let first = bearer_token(
        Some(1000.0),
        Some(b"cti-1"),
        Some(CatReplay::SingleConnection),
        &[MoqtAction::Setup],
    );
    let later = bearer_token(
        Some(2000.0),
        Some(b"cti-1"),
        Some(CatReplay::SingleConnection),
        &[MoqtAction::Setup],
    );
    let crypto = AwsLcRsCrypto::new();
    let mut cache = BearerReplayCache::new(10.0).expect("cache を構築できる");

    first
        .verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1000.0, MoqtAction::Setup, b"conn-a"),
            &mut cache,
        )
        .expect("conn-a で受理される");

    // 切断に相当する操作は無い。別接続からの利用は拒否される
    assert_eq!(
        first.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1000.0, MoqtAction::Setup, b"conn-b"),
            &mut cache,
        ),
        Err(CatError::BearerReplay(
            BearerReplayError::ConnectionMismatch
        ))
    );
    // 保持期限 (1000 + 10) と等しい間は束縛を保持する
    assert_eq!(
        later.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1010.0, MoqtAction::Setup, b"conn-b"),
            &mut cache,
        ),
        Err(CatError::BearerReplay(
            BearerReplayError::ConnectionMismatch
        ))
    );
    // 保持期限を超えたら記録が解放され、同じ cti を別接続で使える
    later
        .verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1010.5, MoqtAction::Setup, b"conn-b"),
            &mut cache,
        )
        .expect("保持期限を超えたら別接続で再利用できる");
    assert_eq!(cache.len(), 1);
}

/// `exp` + leeway と等しい参照時刻は受理し、超えると拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_bearer_expiration_boundary_matches_cache_leeway() {
    let token = bearer_token(
        Some(100.0),
        Some(b"cti-1"),
        Some(CatReplay::SingleConnection),
        &[MoqtAction::Setup],
    );
    let crypto = AwsLcRsCrypto::new();

    // 保持期限と等しい参照時刻は受理する (validate の境界と同じ)
    let mut cache = BearerReplayCache::new(10.0).expect("cache を構築できる");
    token
        .verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(110.0, MoqtAction::Setup, b"conn-a"),
            &mut cache,
        )
        .expect("exp + leeway と等しい間は受理する");

    // 0.001 秒でも超えると期限切れになる
    let mut cache = BearerReplayCache::new(10.0).expect("cache を構築できる");
    assert_eq!(
        token.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(110.001, MoqtAction::Setup, b"conn-a"),
            &mut cache,
        ),
        Err(CatError::Claims(ClaimValidationError::Expired))
    );
    assert!(cache.is_empty());
}

/// catreplay=1 では cti と exp が必須であること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_bearer_requires_cti_and_expiration() {
    let crypto = AwsLcRsCrypto::new();
    let mut cache = BearerReplayCache::new(10.0).expect("cache を構築できる");

    // cti が無いトークンは拒否される
    let no_cti = bearer_token(
        Some(1000.0),
        None,
        Some(CatReplay::SingleConnection),
        &[MoqtAction::Setup],
    );
    assert_eq!(
        no_cti.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1000.0, MoqtAction::Setup, b"conn-a"),
            &mut cache,
        ),
        Err(CatError::CatReplayRequiresClaim("cti"))
    );

    // exp が無いトークンは拒否される
    let no_expiration = bearer_token(
        None,
        Some(b"cti-1"),
        Some(CatReplay::SingleConnection),
        &[MoqtAction::Setup],
    );
    assert_eq!(
        no_expiration.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1000.0, MoqtAction::Setup, b"conn-a"),
            &mut cache,
        ),
        Err(CatError::CatReplayRequiresClaim("exp"))
    );
    assert!(cache.is_empty());
}

/// catreplay=2 (実装定義) を 0 と同じ扱いにせず拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_bearer_rejects_implementation_defined_replay() {
    let token = bearer_token(
        Some(1000.0),
        Some(b"cti-1"),
        Some(CatReplay::ImplementationDefined),
        &[MoqtAction::Setup],
    );
    let crypto = AwsLcRsCrypto::new();
    let mut cache = BearerReplayCache::new(10.0).expect("cache を構築できる");
    assert_eq!(
        token.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1000.0, MoqtAction::Setup, b"conn-a"),
            &mut cache,
        ),
        Err(CatError::UnsupportedCatReplay(
            CatReplay::ImplementationDefined
        ))
    );
    assert!(cache.is_empty());
}

/// catreplay が無い / 0 のトークンは cache に記録しないこと
#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_bearer_without_replay_protection_does_not_bind() {
    let crypto = AwsLcRsCrypto::new();
    for cat_replay in [None, Some(CatReplay::NotProtected)] {
        let token = bearer_token(
            Some(1000.0),
            Some(b"cti-1"),
            cat_replay,
            &[MoqtAction::Setup],
        );
        let mut cache = BearerReplayCache::new(10.0).expect("cache を構築できる");
        token
            .verify_bearer(
                &crypto,
                &super::hmac_key(),
                &verification_options(1000.0, MoqtAction::Setup, b"conn-a"),
                &mut cache,
            )
            .expect("conn-a で受理される");
        // replay 保護がないため、同じトークンを別接続で使っても拒否されない
        token
            .verify_bearer(
                &crypto,
                &super::hmac_key(),
                &verification_options(1000.0, MoqtAction::Setup, b"conn-b"),
                &mut cache,
            )
            .expect("catreplay が無い / 0 なら別接続でも受理される");
        assert!(cache.is_empty(), "catreplay が無い / 0 なら記録しない");
    }
}

/// `cnf` 付きトークンを bearer 検証で拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_bearer_rejects_confirmation() {
    let crypto = AwsLcRsCrypto::new();
    let token_bytes = CatTokenBuilder::new()
        .issuer(ISSUER)
        .audience(AUDIENCE)
        .expiration(1000.0)
        .cwt_id(b"cti-1".to_vec())
        .cat_replay(CatReplay::SingleConnection)
        .jwk_thumbprint(vec![0xab; 32])
        .moqt(MoqtClaim::new().scope(MoqtScope::new([MoqtAction::Setup])))
        .build_cose(&crypto, &super::hmac_key())
        .expect("cnf 付きトークンを発行できる");
    let token = CatToken::decode(&token_bytes).expect("デコードできる");
    let mut cache = BearerReplayCache::new(10.0).expect("cache を構築できる");
    assert_eq!(
        token.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1000.0, MoqtAction::Setup, b"conn-a"),
            &mut cache,
        ),
        Err(CatError::BearerConfirmationNotSupported)
    );
    assert!(cache.is_empty());
}

/// 署名・scope・期限の検証に失敗したトークンが束縛状態を汚染しないこと
#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_bearer_failures_do_not_pollute_cache() {
    let token = bearer_token(
        Some(1000.0),
        Some(b"cti-1"),
        Some(CatReplay::SingleConnection),
        &[MoqtAction::Setup],
    );
    let crypto = AwsLcRsCrypto::new();
    let mut cache = BearerReplayCache::new(10.0).expect("cache を構築できる");

    // 期待 issuer と一致しない
    let mut options = verification_options(1000.0, MoqtAction::Setup, b"conn-a");
    options.expected_issuers = OTHER_ISSUERS;
    assert_eq!(
        token.verify_bearer(&crypto, &super::hmac_key(), &options, &mut cache),
        Err(CatError::Claims(ClaimValidationError::IssuerMismatch))
    );
    assert!(cache.is_empty(), "issuer の失敗で束縛してはならない");

    // 署名が不正
    let wrong_key = CoseKey::symmetric(vec![0x00; 32]);
    assert!(matches!(
        token.verify_bearer(
            &crypto,
            &wrong_key,
            &verification_options(1000.0, MoqtAction::Setup, b"conn-a"),
            &mut cache,
        ),
        Err(CatError::Crypto(_))
    ));
    assert!(cache.is_empty(), "署名の失敗で束縛してはならない");

    // scope が action を認可しない
    assert_eq!(
        token.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1000.0, MoqtAction::Fetch, b"conn-a"),
            &mut cache,
        ),
        Err(CatError::ActionNotAuthorized(MoqtAction::Fetch))
    );
    assert!(cache.is_empty(), "scope の失敗で束縛してはならない");

    // 期限切れ (exp + leeway を超える)
    assert_eq!(
        token.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1010.001, MoqtAction::Setup, b"conn-a"),
            &mut cache,
        ),
        Err(CatError::Claims(ClaimValidationError::Expired))
    );
    assert!(cache.is_empty(), "期限切れで束縛してはならない");

    // その後、正しい検証は成功し、別接続からの利用は拒否される
    token
        .verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1000.0, MoqtAction::Setup, b"conn-a"),
            &mut cache,
        )
        .expect("正しい検証は成功する");

    // 既存の束縛がある状態でも、失敗した検証はその束縛を変更しない
    let mut options = verification_options(1000.0, MoqtAction::Setup, b"conn-a");
    options.expected_issuers = OTHER_ISSUERS;
    assert_eq!(
        token.verify_bearer(&crypto, &super::hmac_key(), &options, &mut cache),
        Err(CatError::Claims(ClaimValidationError::IssuerMismatch))
    );
    assert_eq!(
        token.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1000.0, MoqtAction::Fetch, b"conn-a"),
            &mut cache,
        ),
        Err(CatError::ActionNotAuthorized(MoqtAction::Fetch))
    );
    assert_eq!(
        token.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1000.0, MoqtAction::Setup, b"conn-b"),
            &mut cache,
        ),
        Err(CatError::BearerReplay(
            BearerReplayError::ConnectionMismatch
        )),
        "conn-a の束縛は失敗後も維持されること"
    );
    assert_eq!(
        token.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1000.0, MoqtAction::Setup, b"conn-b"),
            &mut cache,
        ),
        Err(CatError::BearerReplay(
            BearerReplayError::ConnectionMismatch
        ))
    );
}

/// 署名検証オプションが bearer 検証でも使われること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_bearer_uses_signature_verification_options() {
    let token = bearer_token(
        Some(1000.0),
        Some(b"cti-1"),
        Some(CatReplay::SingleConnection),
        &[MoqtAction::Setup],
    );
    let crypto = AwsLcRsCrypto::new();
    let mut cache = BearerReplayCache::new(10.0).expect("cache を構築できる");
    let mut options = verification_options(1000.0, MoqtAction::Setup, b"conn-a");
    options.verify_options = VerifyOptions {
        expected_algorithm: Some(Algorithm::Es256),
        ..VerifyOptions::default()
    };
    assert_eq!(
        token.verify_bearer(&crypto, &super::hmac_key(), &options, &mut cache),
        Err(CatError::AlgorithmMismatch {
            token: Algorithm::HmacSha256,
            expected: Algorithm::Es256,
        })
    );
    assert!(cache.is_empty());
}

/// `exp` と leeway の加算が無限大になる場合は保持期限を作れず拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_bearer_rejects_non_finite_retention() {
    let token = bearer_token(
        Some(f64::MAX),
        Some(b"cti-1"),
        Some(CatReplay::SingleConnection),
        &[MoqtAction::Setup],
    );
    let crypto = AwsLcRsCrypto::new();
    let mut cache = BearerReplayCache::new(f64::MAX).expect("cache を構築できる");
    assert_eq!(
        token.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(1000.0, MoqtAction::Setup, b"conn-a"),
            &mut cache,
        ),
        Err(CatError::NonFiniteNumber("catreplay retention"))
    );
    assert!(cache.is_empty(), "保持期限の失敗で束縛してはならない");
}

/// verify_bearer が nbf / aud / 参照時刻 / typ の失敗を cache 非汚染で拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_bearer_rejects_claim_and_option_failures_without_binding() {
    let crypto = AwsLcRsCrypto::new();
    let mut cache = BearerReplayCache::new(10.0).expect("cache を構築できる");

    // nbf より前の参照時刻は NotYetValid
    let token = {
        let token_bytes = CatTokenBuilder::new()
            .issuer(ISSUER)
            .audience(AUDIENCE)
            .expiration(1000.0)
            .not_before(900.0)
            .cwt_id(b"cti-1".to_vec())
            .cat_replay(CatReplay::SingleConnection)
            .moqt(MoqtClaim::new().scope(MoqtScope::new([MoqtAction::Setup])))
            .build_cose(&crypto, &super::hmac_key())
            .expect("トークンを発行できる");
        CatToken::decode(&token_bytes).expect("トークンをデコードできる")
    };
    assert_eq!(
        token.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(800.0, MoqtAction::Setup, b"conn-a"),
            &mut cache,
        ),
        Err(CatError::Claims(ClaimValidationError::NotYetValid))
    );
    assert!(cache.is_empty(), "nbf の失敗で束縛してはならない");

    // aud が一致しない
    let token = bearer_token(
        Some(1000.0),
        Some(b"cti-2"),
        Some(CatReplay::SingleConnection),
        &[MoqtAction::Setup],
    );
    let mut options = verification_options(900.0, MoqtAction::Setup, b"conn-a");
    options.expected_audiences = &["https://other.example.com"];
    assert_eq!(
        token.verify_bearer(&crypto, &super::hmac_key(), &options, &mut cache),
        Err(CatError::Claims(ClaimValidationError::AudienceMismatch))
    );
    assert!(cache.is_empty());

    // 参照時刻が有限でない
    assert_eq!(
        token.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(f64::NAN, MoqtAction::Setup, b"conn-a"),
            &mut cache,
        ),
        Err(CatError::Claims(ClaimValidationError::InvalidReferenceTime))
    );
    assert!(cache.is_empty());

    // typ が一致しない
    let mut options = verification_options(900.0, MoqtAction::Setup, b"conn-a");
    options.verify_options.expected_type = Some("other");
    assert_eq!(
        token.verify_bearer(&crypto, &super::hmac_key(), &options, &mut cache),
        Err(CatError::TypeMismatch)
    );
    assert!(cache.is_empty());

    // 空の connection_id は cache の契約違反として拒否する
    assert_eq!(
        token.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(900.0, MoqtAction::Setup, b""),
            &mut cache,
        ),
        Err(CatError::BearerReplay(BearerReplayError::EmptyConnectionId))
    );
    assert!(cache.is_empty());

    // 空の cti を持つ catreplay=1 のトークンも拒否する
    let token = bearer_token(
        Some(1000.0),
        Some(b""),
        Some(CatReplay::SingleConnection),
        &[MoqtAction::Setup],
    );
    assert_eq!(
        token.verify_bearer(
            &crypto,
            &super::hmac_key(),
            &verification_options(900.0, MoqtAction::Setup, b"conn-a"),
            &mut cache,
        ),
        Err(CatError::BearerReplay(BearerReplayError::EmptyCti))
    );
    assert!(cache.is_empty());
}
