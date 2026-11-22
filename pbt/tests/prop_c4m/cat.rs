//! CAT のクレームの property テスト
//!
//! - クレームセットのラウンドトリップ
//! - compact 形式と COSE 形式のトークンのデコード
//! - `CatClaims::validate` の判定が仕様のモデルと一致すること
//! - `catreplay` の型付きデコードと bearer の接続束縛 (保持期限・解放) の一致性

use pbt::common::{sample_bytes, test_runner};
use shiguredo_moqt::c4m::CatDpop;
use shiguredo_moqt::c4m::cat::{
    BearerReplayCache, BearerReplayError, CatClaims, CatError, CatReplay, CatToken,
    ClaimValidationError, ClaimValidationOptions, Confirmation, TokenFormat,
};
use shiguredo_moqt::c4m::cbor::{self, Value};
use shiguredo_moqt::c4m::cose::{Algorithm, CoseEncodingOptions, CoseMac0, CoseMessage, CoseSign1};

use super::common::{
    base64url_encode, base64url_encode_padded, sample_cat_replay, sample_catdpop,
    sample_moqt_claim, sample_safe_text,
};
use super::cose::sample_header;

/// 検証に使う時刻の境界値 (非有限値を含む)
const TIME_BOUNDARIES: &[f64] = &[
    0.0,
    -0.0,
    1.0,
    50.0,
    100.0,
    200.0,
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
];

/// 印字可能 ASCII 文字列を生成する
fn sample_text(ctx: &mut noprop::TestCaseContext) -> String {
    let len = noprop::sample_usize_in(ctx, 0..=16);
    noprop::sample_ascii_printable_string(ctx, len)
}

/// クレームセットを生成する
fn sample_claims(ctx: &mut noprop::TestCaseContext) -> CatClaims {
    let audience_count = noprop::sample_usize_in(ctx, 0..=3);
    let audience = (0..audience_count).map(|_| sample_text(ctx)).collect();

    let confirmation = if noprop::sample_bool(ctx) {
        Some(Confirmation {
            jwk_thumbprint: if noprop::sample_bool(ctx) {
                Some(sample_bytes(ctx, 32))
            } else {
                None
            },
            cose_key_thumbprint: if noprop::sample_bool(ctx) {
                // ckt は SHA-256 のサムプリント長 (32 バイト) でなければデコードできない
                Some(noprop::sample_bytes_vec(ctx, 32))
            } else {
                None
            },
            confirmation_key_3: if noprop::sample_bool(ctx) {
                Some(sample_bytes(ctx, 32))
            } else {
                None
            },
            raw: Vec::new(),
        })
    } else {
        None
    };

    // 型付きで解釈しないクレーム。キーは昇順に生成する
    let raw_count = noprop::sample_usize_in(ctx, 0..=3);
    let start_key = noprop::sample_u64_in(ctx, 100..=120);
    let raw: Vec<(Value, Value)> = (start_key..)
        .take(raw_count)
        .map(|key| {
            (
                Value::Unsigned(key),
                Value::Unsigned(noprop::sample_u64(ctx)),
            )
        })
        .collect();

    CatClaims {
        issuer: if noprop::sample_bool(ctx) {
            Some(sample_text(ctx))
        } else {
            None
        },
        subject: if noprop::sample_bool(ctx) {
            Some(sample_text(ctx))
        } else {
            None
        },
        audience,
        expiration: if noprop::sample_bool(ctx) {
            Some(noprop::sample_f64(ctx))
        } else {
            None
        },
        not_before: if noprop::sample_bool(ctx) {
            Some(noprop::sample_f64(ctx))
        } else {
            None
        },
        issued_at: if noprop::sample_bool(ctx) {
            Some(noprop::sample_f64(ctx))
        } else {
            None
        },
        cwt_id: if noprop::sample_bool(ctx) {
            Some(sample_bytes(ctx, 8))
        } else {
            None
        },
        confirmation,
        moqt: if noprop::sample_bool(ctx) {
            Some(sample_moqt_claim(ctx))
        } else {
            None
        },
        moqt_reval: if noprop::sample_bool(ctx) {
            Some(noprop::sample_f64_in(ctx, 0.0, 3600.0))
        } else {
            None
        },
        catdpop: if noprop::sample_bool(ctx) {
            Some(sample_catdpop(ctx))
        } else {
            None
        },
        cat_replay: if noprop::sample_bool(ctx) {
            Some(sample_cat_replay(ctx))
        } else {
            None
        },
        raw,
    }
}

/// クレームセットの encode -> decode が一致する
#[test]
fn claims_roundtrip() -> noprop::TestResult {
    let moqt_seen = std::cell::Cell::new(0usize);
    let catdpop_seen = std::cell::Cell::new(0usize);
    let raw_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let claims = sample_claims(ctx);
        let value = claims.encode().expect("クレームをエンコードできる");
        let encoded = cbor::encode(&value).expect("CBOR をエンコードできる");
        let decoded = CatClaims::decode(&cbor::decode(&encoded).expect("CBOR をデコードできる"))
            .expect("クレームをデコードできる");
        assert_eq!(decoded, claims, "ラウンドトリップでクレームが変わる");
        // 不変条件を評価したあとでカバレッジを数える
        if claims.moqt.is_some() {
            moqt_seen.set(moqt_seen.get() + 1);
        }
        if claims.catdpop.is_some() {
            catdpop_seen.set(catdpop_seen.get() + 1);
        }
        if !claims.raw.is_empty() {
            raw_seen.set(raw_seen.get() + 1);
        }
        Ok(())
    })?;
    assert!(
        moqt_seen.get() > 0,
        "moqt クレームのケースが生成されなかった\n{runner}"
    );
    assert!(
        catdpop_seen.get() > 0,
        "catdpop クレームのケースが生成されなかった\n{runner}"
    );
    assert!(
        raw_seen.get() > 0,
        "raw クレームのケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// COSE のアルゴリズムを 1 つ生成する
fn sample_algorithm(ctx: &mut noprop::TestCaseContext) -> Algorithm {
    noprop::sample_choice(
        ctx,
        &[
            Algorithm::HmacSha256,
            Algorithm::HmacSha384,
            Algorithm::HmacSha512,
            Algorithm::Es256,
            Algorithm::Es384,
            Algorithm::Es512,
            Algorithm::EdDsa,
        ],
    )
}

/// compact 形式のデコードが生成した 3 分割と一致する
#[test]
fn compact_token_roundtrip() -> noprop::TestResult {
    let ok_seen = std::cell::Cell::new(0usize);
    let padded_seen = std::cell::Cell::new(0usize);
    let invalid_base64_seen = std::cell::Cell::new(0usize);
    let format_error_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let claims = sample_claims(ctx);
        let algorithm = sample_algorithm(ctx);
        let header = sample_header(ctx, algorithm);
        let protected = cbor::encode(&header.encode().expect("ヘッダをエンコードできる"))
            .expect("CBOR をエンコードできる");
        let payload = cbor::encode(&claims.encode().expect("クレームをエンコードできる"))
            .expect("CBOR をエンコードできる");
        let signature = sample_bytes(ctx, 8);
        // パディング付きの base64url も受理される
        let padded = noprop::sample_bool(ctx);
        let payload_segment = if padded {
            base64url_encode_padded(&payload)
        } else {
            base64url_encode(&payload)
        };
        let signature_segment = if padded {
            base64url_encode_padded(&signature)
        } else {
            base64url_encode(&signature)
        };
        let protected_segment = base64url_encode(&protected);
        let text = format!("{protected_segment}.{payload_segment}.{signature_segment}");

        let token = CatToken::decode(text.as_bytes()).expect("compact 形式はデコードできる");
        ok_seen.set(ok_seen.get() + 1);
        if padded {
            padded_seen.set(padded_seen.get() + 1);
        }
        assert_eq!(token.format(), TokenFormat::Compact);
        assert_eq!(token.claims(), &claims);
        assert_eq!(token.protected_header(), protected.as_slice());
        assert_eq!(token.payload(), payload.as_slice());
        assert_eq!(token.signature(), signature.as_slice());
        // 署名対象は base64url のままの protected と claims (draft-ietf-moq-c4m-01 付録 A)
        assert_eq!(
            token.signing_input(),
            format!("{protected_segment}.{payload_segment}").as_bytes()
        );
        assert_eq!(token.raw_token(), text.as_bytes());
        // decode_compact を直接呼んでも同じ結果になる
        let direct = CatToken::decode_compact(&text).expect("decode_compact は成功する");
        assert_eq!(direct.claims(), &claims);
        assert_eq!(direct.format(), TokenFormat::Compact);

        // base64url として不正な分割はデコードできない
        let broken = format!("{protected_segment}.{payload_segment}.*");
        assert_eq!(
            CatToken::decode(broken.as_bytes())
                .expect_err("base64url が不正な compact 形式は失敗する"),
            CatError::InvalidBase64
        );
        assert_eq!(
            CatToken::decode_compact(&broken)
                .expect_err("base64url が不正な compact 形式は失敗する"),
            CatError::InvalidBase64
        );
        invalid_base64_seen.set(invalid_base64_seen.get() + 1);

        // 3 分割でない場合は形式エラーになる
        assert_eq!(
            CatToken::decode_compact("a.b").expect_err("2 分割は形式エラーになる"),
            CatError::InvalidTokenFormat
        );
        assert_eq!(
            CatToken::decode_compact("a.b.c.d").expect_err("4 分割は形式エラーになる"),
            CatError::InvalidTokenFormat
        );
        format_error_seen.set(format_error_seen.get() + 1);
        Ok(())
    })?;
    assert!(
        ok_seen.get() > 0,
        "compact 形式をデコードできたケースが生成されなかった\n{runner}"
    );
    assert!(
        padded_seen.get() > 0,
        "パディング付きのケースが生成されなかった\n{runner}"
    );
    assert!(
        invalid_base64_seen.get() > 0,
        "base64url が不正なケースが生成されなかった\n{runner}"
    );
    assert!(
        format_error_seen.get() > 0,
        "3 分割でないケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// COSE 形式のデコードが生成したメッセージと一致する
#[test]
fn cose_token_roundtrip() -> noprop::TestResult {
    let sign1_seen = std::cell::Cell::new(0usize);
    let mac0_seen = std::cell::Cell::new(0usize);
    let wrapped_seen = std::cell::Cell::new(0usize);
    let detached_seen = std::cell::Cell::new(0usize);
    let missing_algorithm_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let claims = sample_claims(ctx);
        let algorithm = sample_algorithm(ctx);
        // 0: そのまま / 1: 署名を base64url で包む / 2: detached payload / 3: alg 無し
        let kind = noprop::sample_weighted_index(ctx, &[5, 2, 1, 1]);
        let mut header = sample_header(ctx, algorithm);
        if kind == 3 {
            header.algorithm = None;
            header.algorithm_identifier = None;
        }
        let protected = cbor::encode(&header.encode().expect("ヘッダをエンコードできる"))
            .expect("CBOR をエンコードできる");
        let payload = cbor::encode(&claims.encode().expect("クレームをエンコードできる"))
            .expect("CBOR をエンコードできる");
        let signature = sample_bytes(ctx, 8);
        let message = if algorithm.is_mac() {
            CoseMessage::Mac0(CoseMac0 {
                protected: protected.clone(),
                unprotected: Vec::new(),
                payload: (kind != 2).then(|| payload.clone()),
                tag: signature.clone(),
                cose_tagged: true,
                cwt_tagged: true,
            })
        } else {
            CoseMessage::Sign1(CoseSign1 {
                protected: protected.clone(),
                unprotected: Vec::new(),
                payload: (kind != 2).then(|| payload.clone()),
                signature: signature.clone(),
                cose_tagged: true,
                cwt_tagged: true,
            })
        };
        let encoded = message
            .encode(&CoseEncodingOptions::default())
            .expect("COSE メッセージをエンコードできる");
        let expected_format = if algorithm.is_mac() {
            TokenFormat::CoseMac0
        } else {
            TokenFormat::CoseSign1
        };

        if kind == 2 {
            // detached payload はデコードできない
            assert_eq!(
                CatToken::decode_cose(&encoded).expect_err("detached payload は失敗する"),
                CatError::DetachedPayload
            );
            detached_seen.set(detached_seen.get() + 1);
            return Ok(());
        }
        if kind == 3 {
            // `alg` が無いトークンは拒否される
            assert_eq!(
                CatToken::decode_cose(&encoded).expect_err("alg が無いトークンは失敗する"),
                CatError::MissingAlgorithm
            );
            missing_algorithm_seen.set(missing_algorithm_seen.get() + 1);
            return Ok(());
        }

        let token = CatToken::decode_cose(&encoded).expect("COSE 形式はデコードできる");
        assert_eq!(token.format(), expected_format);
        assert_eq!(token.claims(), &claims);
        assert_eq!(token.protected_header(), protected.as_slice());
        assert_eq!(token.payload(), payload.as_slice());
        assert_eq!(token.signature(), signature.as_slice());
        assert_eq!(token.raw_token(), encoded.as_slice());
        if expected_format == TokenFormat::CoseMac0 {
            mac0_seen.set(mac0_seen.get() + 1);
        } else {
            sign1_seen.set(sign1_seen.get() + 1);
        }

        if kind == 1 {
            // base64url で包んだテキストも自動判別で受理される
            let text = base64url_encode(&encoded);
            let wrapped = CatToken::decode(text.as_bytes())
                .expect("base64url で包んだ COSE 形式はデコードできる");
            assert_eq!(wrapped.format(), expected_format);
            assert_eq!(wrapped.claims(), &claims);
            wrapped_seen.set(wrapped_seen.get() + 1);
        } else {
            // CBOR のバイト列は先頭が compact 形式に見えないため COSE 形式として解釈される
            if !core::str::from_utf8(&encoded).is_ok_and(|text| text.matches('.').count() == 2) {
                let decoded =
                    CatToken::decode(&encoded).expect("COSE 形式は自動判別でもデコードできる");
                assert_eq!(decoded.format(), expected_format);
                assert_eq!(decoded.claims(), &claims);
            }
        }
        Ok(())
    })?;
    assert!(
        sign1_seen.get() > 0,
        "COSE_Sign1 のケースが生成されなかった\n{runner}"
    );
    assert!(
        mac0_seen.get() > 0,
        "COSE_Mac0 のケースが生成されなかった\n{runner}"
    );
    assert!(
        wrapped_seen.get() > 0,
        "base64url で包んだケースが生成されなかった\n{runner}"
    );
    assert!(
        detached_seen.get() > 0,
        "detached payload のケースが生成されなかった\n{runner}"
    );
    assert!(
        missing_algorithm_seen.get() > 0,
        "alg が無いケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// 検証に使う現在時刻と許容ずれが、時間比較の前提条件を満たすかを判定するモデル
///
/// これらはトークンのクレームではなく、公開 API の入力の前提条件である。有限でない
/// 値や負の許容ずれを時間比較に使うと、`NaN` の比較が常に偽になるなどして `exp` /
/// `nbf` の検証を素通りする。そのためライブラリの契約として
/// `InvalidReferenceTime` で拒否する。
fn model_invalid_reference_time(
    options: &ClaimValidationOptions<'_>,
) -> Option<ClaimValidationError> {
    if !options.reference_time_seconds.is_finite()
        || !options.clock_tolerance_seconds.is_finite()
        || options.clock_tolerance_seconds < 0.0
    {
        Some(ClaimValidationError::InvalidReferenceTime)
    } else {
        None
    }
}

/// 数値クレームが有限であるという API 前提条件を判定するモデル
///
/// クレームは公開フィールドのため、CBOR のデコード以外の経路でも値を設定できる。
/// `NaN` / 無限大は期限の比較を素通りさせるため、ライブラリの契約として
/// `NonFiniteClaim` で拒否する。`iat` は仕様上トークンの発行時刻を示すだけで、
/// `validate` は `exp` / `nbf` のような時刻比較を行わない (有限性だけを検査する)。
/// 判定順はライブラリの契約であり、`exp` → `nbf` → `iat` → `moqt-reval` の順に
/// 最初の非有限クレームを返す。
fn model_non_finite_claim(claims: &CatClaims) -> Option<ClaimValidationError> {
    [
        (claims.expiration, "exp"),
        (claims.not_before, "nbf"),
        (claims.issued_at, "iat"),
        (claims.moqt_reval, "moqt-reval"),
    ]
    .into_iter()
    .find_map(|(number, name)| {
        number
            .is_some_and(|number| !number.is_finite())
            .then_some(ClaimValidationError::NonFiniteClaim(name))
    })
}

/// `catdpop` のウィンドウが有効な値かを判定するモデル
///
/// `catdpop` のウィンドウは proof の鮮度を判定する秒数である (CTA-5007-B §4.8 /
/// draft-ietf-moq-c4m-02 §4.1)。有限でない値は比較を素通りし、負の値はすべての
/// proof を期限切れにするため、ライブラリの契約として拒否する。
fn model_invalid_catdpop_window(claims: &CatClaims) -> Option<ClaimValidationError> {
    let window = claims.catdpop.as_ref()?.window_seconds?;
    if !window.is_finite() {
        return Some(ClaimValidationError::NonFiniteClaim("catdpop window"));
    }
    if window < 0.0 {
        return Some(ClaimValidationError::NegativeClaim("catdpop window"));
    }
    None
}

/// `moqt-reval` の受理可否を draft-ietf-moq-c4m-02 §2.2 の MUST から判定するモデル
///
/// - 再検証できない受信者 (`None`) は `moqt-reval` を持つトークンを値にかかわらず
///   すべて拒否する (§2.2 の "it MUST reject all tokens with a "moqt-reval" claim")
/// - 再検証できる受信者 (`Some(x)`) でも、再検証間隔が `x` を下回る場合は拒否する
///   (§2.2 の "If the revalidation interval is smaller than the recipient is prepared
///   or able to revalidate, the recipient MUST reject the token.")
/// - `moqt-reval` の値が 0 の場合は再検証間隔ではなく「再検証してはならない」指示
///   なので、`Some(x)` の受信者は受理する (§2.2 の "When the value of this claim is
///   zero, the token MUST NOT be revalidated.")
/// - `x` が有限でない / 負である場合は `moqt_reval < x` の比較が fail-open になる
///   ため、クレームの有無にかかわらずライブラリの契約として拒否する
///
/// `moqt_reval` は呼び出し前に有限性が検査されている。
fn model_moqt_reval(
    claims: &CatClaims,
    options: &ClaimValidationOptions<'_>,
) -> Option<ClaimValidationError> {
    if let Some(interval) = options.revalidation_interval_seconds
        && (!interval.is_finite() || interval < 0.0)
    {
        return Some(ClaimValidationError::InvalidRevalidationInterval);
    }
    let moqt_reval = claims.moqt_reval?;
    let Some(interval) = options.revalidation_interval_seconds else {
        return Some(ClaimValidationError::RevalidationUnsupported);
    };
    if moqt_reval == 0.0 {
        return None;
    }
    if moqt_reval < interval {
        return Some(ClaimValidationError::RevalidationIntervalTooShort);
    }
    None
}

/// `exp` の期限判定を RFC 8392 §3.1.4 / RFC 7519 §4.1.4 から導くモデル
///
/// `exp` は「この時刻以降はトークンを受理してはならない」期限である。RFC 7519
/// §4.1.4 は実装が小さな時計ずれの許容を認めている。ライブラリは許容値を期限に
/// 加えた時刻を境界とし、参照時刻が境界を厳密に超えた場合だけ `Expired` とする
/// (境界と等しい場合は受理する)。
fn model_expiration(
    claims: &CatClaims,
    options: &ClaimValidationOptions<'_>,
) -> Option<ClaimValidationError> {
    let expiration = claims.expiration?;
    (options.reference_time_seconds > expiration + options.clock_tolerance_seconds)
        .then_some(ClaimValidationError::Expired)
}

/// `nbf` の開始時刻判定を RFC 8392 §3.1.5 / RFC 7519 §4.1.5 から導くモデル
///
/// RFC 7519 §4.1.5 は、現在時刻が `nbf` より前の場合はトークンを受理してはならない
/// とする。ライブラリは許容値を引いた時刻を境界とし、参照時刻に許容値を加えた時刻が
/// `nbf` を厳密に下回る場合だけ `NotYetValid` とする (境界と等しい場合は受理する)。
fn model_not_before(
    claims: &CatClaims,
    options: &ClaimValidationOptions<'_>,
) -> Option<ClaimValidationError> {
    let not_before = claims.not_before?;
    (options.reference_time_seconds + options.clock_tolerance_seconds < not_before)
        .then_some(ClaimValidationError::NotYetValid)
}

/// `iss` の一致判定を RFC 8392 §3.1.1 / RFC 7519 §4.1.1 から導くモデル
///
/// `iss` の処理はアプリケーション依存である (RFC 7519 §4.1.1)。このライブラリは
/// 期待する発行者のリストが空でない場合に、`iss` が存在してリストのいずれかと
/// 一致することを要求する。`iss` が無い場合は一致しないものとして拒否する。
fn model_issuer(
    claims: &CatClaims,
    options: &ClaimValidationOptions<'_>,
) -> Option<ClaimValidationError> {
    if options.expected_issuers.is_empty() {
        return None;
    }
    let matches = claims
        .issuer
        .as_deref()
        .is_some_and(|issuer| options.expected_issuers.contains(&issuer));
    (!matches).then_some(ClaimValidationError::IssuerMismatch)
}

/// `aud` の一致判定を RFC 8392 §3.1.3 / RFC 7519 §4.1.3 から導くモデル
///
/// RFC 7519 §4.1.3 は、トークンを処理する主体が `aud` の値のいずれかで自分を識別
/// できなければトークンを拒否する MUST を定める。このライブラリは期待する宛先の
/// リストが空でない場合に、`aud` のいずれかがリストに含まれることを要求する。
/// `aud` が無い / 空の場合は一致しないものとして拒否する。
fn model_audience(
    claims: &CatClaims,
    options: &ClaimValidationOptions<'_>,
) -> Option<ClaimValidationError> {
    if options.expected_audiences.is_empty() {
        return None;
    }
    let matches = claims
        .audience
        .iter()
        .any(|audience| options.expected_audiences.contains(&audience.as_str()));
    (!matches).then_some(ClaimValidationError::AudienceMismatch)
}

/// `CatClaims::validate` の判定結果を、仕様から導いた条件とライブラリの契約から組み立てるモデル
///
/// 判定順は RFC 8392 §7.2 が「The order of the steps is not significant ...」と
/// しているため仕様では定まらない。ライブラリの契約として次の順に固定する。
///
/// 1. 参照時刻 / 許容ずれの妥当性 (公開 API の前提条件)
/// 2. 数値クレームの有限性 (公開 API の前提条件)
/// 3. `catdpop` のウィンドウの有限性と非負性
/// 4. `moqt-reval` の受理可否 (draft-ietf-moq-c4m-02 §2.2)
/// 5. `exp` (RFC 8392 §3.1.4)
/// 6. `nbf` (RFC 8392 §3.1.5)
/// 7. `iss` (RFC 8392 §3.1.1)
/// 8. `aud` (RFC 8392 §3.1.3)
fn expected_validation_error(
    claims: &CatClaims,
    options: &ClaimValidationOptions<'_>,
) -> Option<ClaimValidationError> {
    if let Some(error) = model_invalid_reference_time(options) {
        return Some(error);
    }
    if let Some(error) = model_non_finite_claim(claims) {
        return Some(error);
    }
    if let Some(error) = model_invalid_catdpop_window(claims) {
        return Some(error);
    }
    if let Some(error) = model_moqt_reval(claims, options) {
        return Some(error);
    }
    if let Some(error) = model_expiration(claims, options) {
        return Some(error);
    }
    if let Some(error) = model_not_before(claims, options) {
        return Some(error);
    }
    if let Some(error) = model_issuer(claims, options) {
        return Some(error);
    }
    if let Some(error) = model_audience(claims, options) {
        return Some(error);
    }
    None
}

/// 数値クレームを生成する (非有限値を混ぜる)
fn sample_optional_number(ctx: &mut noprop::TestCaseContext) -> Option<f64> {
    noprop::sample_bool(ctx).then(|| {
        noprop::sample_with_boundaries(ctx, TIME_BOUNDARIES, noprop::Ratio::one_nth(3), |ctx| {
            noprop::sample_f64_in(ctx, -100.0, 300.0)
        })
    })
}

/// クレームの検証が仕様のモデルと一致する
#[test]
fn claims_validate_matches_model() -> noprop::TestResult {
    let ok_seen = std::cell::Cell::new(0usize);
    let invalid_reference_seen = std::cell::Cell::new(0usize);
    let non_finite_claim_seen = std::cell::Cell::new(0usize);
    let expired_seen = std::cell::Cell::new(0usize);
    let not_yet_valid_seen = std::cell::Cell::new(0usize);
    let issuer_mismatch_seen = std::cell::Cell::new(0usize);
    let audience_mismatch_seen = std::cell::Cell::new(0usize);
    let revalidation_unsupported_seen = std::cell::Cell::new(0usize);
    let revalidation_too_short_seen = std::cell::Cell::new(0usize);
    let invalid_revalidation_interval_seen = std::cell::Cell::new(0usize);
    let negative_claim_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    // 再検証判定が iss / aud の検証より前にあり後段まで届くケースが減るため、
    // 他の検証と同じ 256 回では後段のカウンタが 0 になる seed があり得る。回数を増やす
    runner.run(512, |ctx| {
        // 期限 / 開始時刻 / moqt-reval の等号境界は一様な生成ではほぼ踏めない。
        // 浮動小数点で正確に表せる整数を使って境界ちょうどと境界の外側を明示的に
        // 生成し、モデルと実装の判定を比較する
        if noprop::sample_ratio(ctx, noprop::Ratio::one_nth(4)) {
            let reference = noprop::sample_usize_in(ctx, 0..=1000) as f64;
            let tolerance = noprop::sample_usize_in(ctx, 0..=16) as f64;
            let boundary_options = ClaimValidationOptions {
                reference_time_seconds: reference,
                clock_tolerance_seconds: tolerance,
                expected_issuers: &[],
                expected_audiences: &[],
                revalidation_interval_seconds: None,
            };
            let at_boundary = CatClaims {
                expiration: Some(reference - tolerance),
                not_before: Some(reference + tolerance),
                ..CatClaims::default()
            };
            assert_eq!(
                at_boundary.validate(&boundary_options),
                Ok(()),
                "期限 / 開始時刻が境界ちょうどのクレームが拒否された: reference={reference} tolerance={tolerance}"
            );
            assert_eq!(
                expected_validation_error(&at_boundary, &boundary_options),
                None,
                "モデルが境界ちょうどの期限 / 開始時刻を拒否した"
            );
            ok_seen.set(ok_seen.get() + 1);

            let expired = CatClaims {
                expiration: Some(reference - tolerance - 1.0),
                ..at_boundary.clone()
            };
            assert_eq!(
                expired.validate(&boundary_options),
                Err(ClaimValidationError::Expired),
                "期限を 1 秒でも超えたクレームが受理された"
            );
            assert_eq!(
                expected_validation_error(&expired, &boundary_options),
                Some(ClaimValidationError::Expired),
                "モデルが期限切れのクレームを受理した"
            );
            expired_seen.set(expired_seen.get() + 1);

            let not_yet_valid = CatClaims {
                expiration: None,
                not_before: Some(reference + tolerance + 1.0),
                ..CatClaims::default()
            };
            assert_eq!(
                not_yet_valid.validate(&boundary_options),
                Err(ClaimValidationError::NotYetValid),
                "開始時刻より前のクレームが受理された"
            );
            assert_eq!(
                expected_validation_error(&not_yet_valid, &boundary_options),
                Some(ClaimValidationError::NotYetValid),
                "モデルが開始時刻より前のクレームを受理した"
            );
            not_yet_valid_seen.set(not_yet_valid_seen.get() + 1);

            // moqt-reval が受信者の最小間隔と等しい場合は受理する (§2.2 の
            // "smaller than the recipient is prepared or able to revalidate" の境界)
            let interval = noprop::sample_usize_in(ctx, 1..=300) as f64;
            let revalidation_options = ClaimValidationOptions {
                reference_time_seconds: 0.0,
                clock_tolerance_seconds: 0.0,
                expected_issuers: &[],
                expected_audiences: &[],
                revalidation_interval_seconds: Some(interval),
            };
            let at_interval = CatClaims {
                moqt_reval: Some(interval),
                ..CatClaims::default()
            };
            assert_eq!(
                at_interval.validate(&revalidation_options),
                Ok(()),
                "moqt-reval が最小間隔と等しいクレームが拒否された"
            );
            assert_eq!(
                expected_validation_error(&at_interval, &revalidation_options),
                None,
                "モデルが moqt-reval の等号境界を拒否した"
            );
            ok_seen.set(ok_seen.get() + 1);

            // 0 は再検証しない指示のため、最小間隔にかかわらず受理する
            let zero_reval = CatClaims {
                moqt_reval: Some(0.0),
                ..CatClaims::default()
            };
            assert_eq!(
                zero_reval.validate(&revalidation_options),
                Ok(()),
                "moqt-reval が 0 のクレームが拒否された"
            );
            assert_eq!(
                expected_validation_error(&zero_reval, &revalidation_options),
                None,
                "モデルが moqt-reval が 0 のクレームを拒否した"
            );
            ok_seen.set(ok_seen.get() + 1);

            // 非ゼロで最小間隔を下回る場合は拒否する
            let shorter = if interval > 1.0 {
                interval - 1.0
            } else {
                0.5
            };
            let too_short = CatClaims {
                moqt_reval: Some(shorter),
                ..CatClaims::default()
            };
            assert_eq!(
                too_short.validate(&revalidation_options),
                Err(ClaimValidationError::RevalidationIntervalTooShort),
                "最小間隔を下回る moqt-reval のクレームが受理された: interval={interval} shorter={shorter}"
            );
            assert_eq!(
                expected_validation_error(&too_short, &revalidation_options),
                Some(ClaimValidationError::RevalidationIntervalTooShort),
                "モデルが最小間隔を下回る moqt-reval のクレームを受理した"
            );
            revalidation_too_short_seen.set(revalidation_too_short_seen.get() + 1);
            return Ok(());
        }

        let expiration = sample_optional_number(ctx);
        let not_before = sample_optional_number(ctx);
        let issued_at = sample_optional_number(ctx);
        let moqt_reval = sample_optional_number(ctx);
        let catdpop = noprop::sample_bool(ctx).then(|| {
            CatDpop::new(
                noprop::sample_with_boundaries(
                    ctx,
                    TIME_BOUNDARIES,
                    noprop::Ratio::one_nth(3),
                    |ctx| noprop::sample_f64_in(ctx, -100.0, 100.0),
                ),
                noprop::sample_bool(ctx),
            )
        });
        let issuer = noprop::sample_bool(ctx).then(|| sample_safe_text(ctx, 6));
        let audience: Vec<String> = (0..noprop::sample_usize_in(ctx, 0..=2))
            .map(|_| sample_safe_text(ctx, 6))
            .collect();
        let claims = CatClaims {
            issuer: issuer.clone(),
            subject: None,
            audience: audience.clone(),
            expiration,
            not_before,
            issued_at,
            cwt_id: None,
            confirmation: None,
            moqt: None,
            moqt_reval,
            catdpop,
            cat_replay: None,
            raw: Vec::new(),
        };

        let reference = noprop::sample_with_boundaries(
            ctx,
            TIME_BOUNDARIES,
            noprop::Ratio::one_nth(3),
            |ctx| noprop::sample_f64_in(ctx, -100.0, 300.0),
        );
        let tolerance = noprop::sample_with_boundaries(
            ctx,
            &[0.0, 1.0, 10.0, -1.0, f64::NAN],
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_f64_in(ctx, 0.0, 10.0),
        );
        // 期待値リストは、空 / クレームと同じ値 / 別の値の 3 種類を生成する
        let expected_issuers: Vec<String> = match noprop::sample_weighted_index(ctx, &[3, 3, 2]) {
            0 => Vec::new(),
            1 => issuer.iter().cloned().collect(),
            _ => vec![sample_safe_text(ctx, 6)],
        };
        let expected_audiences: Vec<String> = match noprop::sample_weighted_index(ctx, &[3, 3, 2]) {
            0 => Vec::new(),
            1 => audience.iter().take(1).cloned().collect(),
            _ => vec![sample_safe_text(ctx, 6)],
        };
        let expected_issuer_refs: Vec<&str> =
            expected_issuers.iter().map(String::as_str).collect();
        let expected_audience_refs: Vec<&str> =
            expected_audiences.iter().map(String::as_str).collect();
        // 受信者の再検証能力は、再検証できない / 有効な最小間隔 / 不正な最小間隔の
        // 3 種類を生成する。有効な最小間隔は moqt-reval の生成範囲と重ねて大小両方を作る
        let revalidation_interval = match noprop::sample_weighted_index(ctx, &[2, 5, 1]) {
            0 => None,
            1 => Some(noprop::sample_with_boundaries(
                ctx,
                // 50 / 100 / 200 は TIME_BOUNDARIES と重なり、非ゼロの等号境界を踏む
                &[0.0, -0.0, 1.0, 50.0, 100.0, 200.0, 300.0],
                noprop::Ratio::one_nth(3),
                |ctx| noprop::sample_f64_in(ctx, 0.0, 300.0),
            )),
            _ => Some(noprop::sample_with_boundaries(
                ctx,
                &[-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY],
                noprop::Ratio::one_nth(2),
                |ctx| noprop::sample_f64_in(ctx, -100.0, -0.001),
            )),
        };
        let options = ClaimValidationOptions {
            reference_time_seconds: reference,
            clock_tolerance_seconds: tolerance,
            expected_issuers: &expected_issuer_refs,
            expected_audiences: &expected_audience_refs,
            revalidation_interval_seconds: revalidation_interval,
        };

        let expected = expected_validation_error(&claims, &options);
        assert_eq!(
            claims.validate(&options),
            match expected {
                Some(error) => Err(error),
                None => Ok(()),
            },
            "検証結果がモデルと一致しない: claims={claims:?} reference={reference} tolerance={tolerance} revalidation_interval={revalidation_interval:?}"
        );
        match expected {
            None => ok_seen.set(ok_seen.get() + 1),
            Some(ClaimValidationError::InvalidReferenceTime) => {
                invalid_reference_seen.set(invalid_reference_seen.get() + 1);
            }
            Some(ClaimValidationError::NonFiniteClaim(_)) => {
                non_finite_claim_seen.set(non_finite_claim_seen.get() + 1);
            }
            Some(ClaimValidationError::Expired) => {
                expired_seen.set(expired_seen.get() + 1);
            }
            Some(ClaimValidationError::NotYetValid) => {
                not_yet_valid_seen.set(not_yet_valid_seen.get() + 1);
            }
            Some(ClaimValidationError::IssuerMismatch) => {
                issuer_mismatch_seen.set(issuer_mismatch_seen.get() + 1);
            }
            Some(ClaimValidationError::AudienceMismatch) => {
                audience_mismatch_seen.set(audience_mismatch_seen.get() + 1);
            }
            Some(ClaimValidationError::RevalidationUnsupported) => {
                revalidation_unsupported_seen.set(revalidation_unsupported_seen.get() + 1);
            }
            Some(ClaimValidationError::RevalidationIntervalTooShort) => {
                revalidation_too_short_seen.set(revalidation_too_short_seen.get() + 1);
            }
            Some(ClaimValidationError::InvalidRevalidationInterval) => {
                invalid_revalidation_interval_seen
                    .set(invalid_revalidation_interval_seen.get() + 1);
            }
            Some(ClaimValidationError::NegativeClaim(_)) => {
                negative_claim_seen.set(negative_claim_seen.get() + 1);
            }
        }
        Ok(())
    })?;
    assert!(
        ok_seen.get() > 0,
        "検証に成功するケースが生成されなかった\n{runner}"
    );
    assert!(
        invalid_reference_seen.get() > 0,
        "現在時刻 / 許容ずれが不正なケースが生成されなかった\n{runner}"
    );
    assert!(
        non_finite_claim_seen.get() > 0,
        "非有限のクレームのケースが生成されなかった\n{runner}"
    );
    assert!(
        expired_seen.get() > 0,
        "exp を過ぎたケースが生成されなかった\n{runner}"
    );
    assert!(
        not_yet_valid_seen.get() > 0,
        "nbf より前のケースが生成されなかった\n{runner}"
    );
    assert!(
        issuer_mismatch_seen.get() > 0,
        "iss が一致しないケースが生成されなかった\n{runner}"
    );
    assert!(
        audience_mismatch_seen.get() > 0,
        "aud が一致しないケースが生成されなかった\n{runner}"
    );
    assert!(
        revalidation_unsupported_seen.get() > 0,
        "再検証できない受信者が moqt-reval を拒否するケースが生成されなかった\n{runner}"
    );
    assert!(
        revalidation_too_short_seen.get() > 0,
        "moqt-reval が最小間隔を下回るケースが生成されなかった\n{runner}"
    );
    assert!(
        negative_claim_seen.get() > 0,
        "負の数値クレームのケースが生成されなかった\n{runner}"
    );
    assert!(
        invalid_revalidation_interval_seen.get() > 0,
        "不正な最小間隔のケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// `catreplay` の整数変換とクレームのラウンドトリップ
#[test]
fn cat_replay_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let replay = sample_cat_replay(ctx);
        // key と from_key は相互に変換できる
        assert_eq!(CatReplay::from_key(replay.key()), Some(replay));
        // 任意の整数は 0 / 1 / 2 のときだけ型付きになる
        let arbitrary = noprop::sample_i64(ctx);
        assert_eq!(
            CatReplay::from_key(arbitrary).is_some(),
            (0..=2).contains(&arbitrary)
        );
        // クレームの encode -> decode で値が保たれ、整数で書き出される
        let claims = CatClaims {
            cat_replay: Some(replay),
            ..CatClaims::default()
        };
        let value = claims.encode().expect("catreplay をエンコードできる");
        assert_eq!(
            value.map_get(&Value::integer(shiguredo_moqt::c4m::cat::CLAIM_CAT_REPLAY)),
            Some(&Value::integer(replay.key()))
        );
        assert_eq!(
            CatClaims::decode(&value).expect("catreplay をデコードできる"),
            claims
        );
        Ok(())
    })?;
    Ok(())
}

/// `catreplay` の decode が 0 / 1 / 2 だけを受理すること
#[test]
fn cat_replay_decode_accepts_only_supported_integers() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        // 0 / 1 / 2 の受理 branch と、境界・範囲外の拒否 branch を確実に踏むため、
        // 一様乱数だけでなく仕様上の境界値を混ぜる
        let value = match noprop::sample_usize_in(ctx, 0..5) {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => 3,
            _ => noprop::sample_i64(ctx),
        };
        let claims = Value::Map(vec![(
            Value::integer(shiguredo_moqt::c4m::cat::CLAIM_CAT_REPLAY),
            Value::integer(value),
        )]);
        match value {
            0 => assert_eq!(
                CatClaims::decode(&claims)
                    .expect("0 は型付きでデコードできる")
                    .cat_replay,
                Some(CatReplay::NotProtected)
            ),
            1 => assert_eq!(
                CatClaims::decode(&claims)
                    .expect("1 は型付きでデコードできる")
                    .cat_replay,
                Some(CatReplay::SingleConnection)
            ),
            2 => assert_eq!(
                CatClaims::decode(&claims)
                    .expect("2 は型付きでデコードできる")
                    .cat_replay,
                Some(CatReplay::ImplementationDefined)
            ),
            value if value < 0 => assert_eq!(
                CatClaims::decode(&claims),
                Err(CatError::UnexpectedType("catreplay"))
            ),
            value => assert_eq!(
                CatClaims::decode(&claims),
                Err(CatError::InvalidCatReplayValue(value as u64))
            ),
        }
        Ok(())
    })?;
    Ok(())
}

/// `BearerReplayCache` の束縛・再利用・解放がモデルと一致する
#[test]
fn bearer_replay_cache_matches_model() -> noprop::TestResult {
    let mismatch_seen = std::cell::Cell::new(0usize);
    let release_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let leeway = noprop::sample_f64_in(ctx, 0.0, 10.0);
        let mut cache =
            BearerReplayCache::new(leeway).expect("有限で 0 以上の leeway で構築できる");
        // cti と接続識別子は小さな集合から選び、衝突と再利用を高い確率で作る
        let ctis = [b"cti-0".as_slice(), b"cti-1".as_slice(), b"cti-2".as_slice()];
        let connections = [
            b"connection-0".as_slice(),
            b"connection-1".as_slice(),
            b"connection-2".as_slice(),
        ];
        // モデル: cti -> (接続識別子, 保持期限)
        let mut model: std::collections::BTreeMap<Vec<u8>, (Vec<u8>, f64)> =
            std::collections::BTreeMap::new();
        for _ in 0..noprop::sample_usize_in(ctx, 1..=16) {
            let cti = ctis[noprop::sample_usize_in(ctx, 0..ctis.len())];
            let connection = connections[noprop::sample_usize_in(ctx, 0..connections.len())];
            let reference = noprop::sample_f64_in(ctx, 0.0, 100.0);
            let retain_until = noprop::sample_f64_in(ctx, 0.0, 120.0);
            // 未失効の束縛が別の接続にある場合は、解放も含めて何も変更せずに拒否する
            let rejected = model
                .get(cti)
                .is_some_and(|(bound, until)| bound.as_slice() != connection && reference <= *until);
            let expected = if rejected {
                mismatch_seen.set(mismatch_seen.get() + 1);
                Err(BearerReplayError::ConnectionMismatch)
            } else {
                // 参照時刻が保持期限を超えた記録だけを解放する (等しい間は保持する)
                let before_release = model.len();
                model.retain(|_, (_, until)| reference <= *until);
                if model.len() < before_release {
                    release_seen.set(release_seen.get() + 1);
                }
                match model.get_mut(cti) {
                    Some((_, until)) => {
                        // 同じ接続の再利用は許し、保持期限は後ろにだけ延ばす
                        if retain_until > *until {
                            *until = retain_until;
                        }
                        Ok(())
                    }
                    None => {
                        model.insert(cti.to_vec(), (connection.to_vec(), retain_until));
                        Ok(())
                    }
                }
            };
            assert_eq!(
                cache.check_and_bind(cti, connection, retain_until, reference),
                expected,
                "クレーム: cti={cti:?} connection={connection:?} reference={reference} retain_until={retain_until}"
            );
            assert_eq!(cache.len(), model.len(), "記録数がモデルと一致しない");
        }
        // 参照時刻 0 ではどの記録も失効しないため、モデルの接続と保持期限が残っている
        for (cti, (connection, until)) in &model {
            cache
                .check_and_bind(cti, connection, *until, 0.0)
                .expect("モデルに残る cti は同じ接続で再利用できる");
        }
        assert_eq!(cache.len(), model.len());
        Ok(())
    })?;
    assert!(
        mismatch_seen.get() > 0,
        "別接続による拒否のケースが生成されなかった\n{runner}"
    );
    assert!(
        release_seen.get() > 0,
        "保持期限を過ぎた記録の解放ケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// `cnf` を生成する (323 と confirmation key 3 の有無を独立に選ぶ)
fn sample_confirmation(ctx: &mut noprop::TestCaseContext) -> Confirmation {
    Confirmation {
        jwk_thumbprint: if noprop::sample_bool(ctx) {
            Some(sample_bytes(ctx, 32))
        } else {
            None
        },
        confirmation_key_3: if noprop::sample_bool(ctx) {
            Some(sample_bytes(ctx, 32))
        } else {
            None
        },
        ..Confirmation::default()
    }
}

/// `cnf` の confirmation key 3 の解釈が仕様どおりであること
///
/// RFC 8747 §3.4 では key 3 は `kid`、旧 draft-ietf-moq-c4m-01 のベクタでは `jkt` である。
#[test]
fn confirmation_key3_interpretation_matches_spec() -> noprop::TestResult {
    use shiguredo_moqt::c4m::cat::ConfirmationKey3Interpretation;

    let both_seen = std::cell::Cell::new(0usize);
    let only_key3_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let confirmation = sample_confirmation(ctx);
        if confirmation.jwk_thumbprint.is_some() && confirmation.confirmation_key_3.is_some() {
            both_seen.set(both_seen.get() + 1);
        }
        if confirmation.jwk_thumbprint.is_none() && confirmation.confirmation_key_3.is_some() {
            only_key3_seen.set(only_key3_seen.get() + 1);
        }
        // jkt は IANA 登録の 323 だけを返す
        assert_eq!(
            confirmation.jkt(),
            confirmation.jwk_thumbprint.as_deref(),
            "jkt は confirmation key 323 だけを返す"
        );
        for interpretation in [
            ConfirmationKey3Interpretation::IanaKid,
            ConfirmationKey3Interpretation::C4mDraftJkt,
        ] {
            // 323 を優先し、323 が無い場合だけ C4mDraftJkt で key 3 にフォールバックする
            let expected_jkt = match interpretation {
                ConfirmationKey3Interpretation::IanaKid => confirmation.jwk_thumbprint.as_deref(),
                ConfirmationKey3Interpretation::C4mDraftJkt => confirmation
                    .jwk_thumbprint
                    .as_deref()
                    .or(confirmation.confirmation_key_3.as_deref()),
            };
            assert_eq!(
                confirmation.jkt_with(interpretation),
                expected_jkt,
                "jkt_with の解釈が仕様と一致しない ({interpretation:?})"
            );
            // kid は IanaKid のときだけ key 3 を返す
            let expected_kid = match interpretation {
                ConfirmationKey3Interpretation::IanaKid => {
                    confirmation.confirmation_key_3.as_deref()
                }
                ConfirmationKey3Interpretation::C4mDraftJkt => None,
            };
            assert_eq!(
                confirmation.kid(interpretation),
                expected_kid,
                "kid の解釈が仕様と一致しない ({interpretation:?})"
            );
        }
        Ok(())
    })?;
    assert!(
        both_seen.get() > 0,
        "323 と key 3 が併存するケースが生成されなかった\n{runner}"
    );
    assert!(
        only_key3_seen.get() > 0,
        "key 3 だけのケースが生成されなかった\n{runner}"
    );
    Ok(())
}
