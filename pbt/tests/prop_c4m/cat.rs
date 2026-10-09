//! CAT のクレームの property テスト
//!
//! - クレームセットのラウンドトリップ
//! - compact 形式と COSE 形式のトークンのデコード
//! - `CatClaims::validate` の判定が仕様のモデルと一致すること

use pbt::common::{sample_bytes, test_runner};
use shiguredo_moqt::c4m::CatDpop;
use shiguredo_moqt::c4m::cat::{
    CatClaims, CatError, CatToken, ClaimValidationError, ClaimValidationOptions, Confirmation,
    TokenFormat,
};
use shiguredo_moqt::c4m::cbor::{self, Value};
use shiguredo_moqt::c4m::cose::{Algorithm, CoseEncodingOptions, CoseMac0, CoseMessage, CoseSign1};

use super::common::{
    base64url_encode, base64url_encode_padded, sample_catdpop, sample_moqt_claim, sample_safe_text,
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
            c4m_draft_jwk_thumbprint: if noprop::sample_bool(ctx) {
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

/// `CatClaims::validate` の判定順をそのまま実装したモデル
fn expected_validation_error(
    claims: &CatClaims,
    options: &ClaimValidationOptions<'_>,
) -> Option<ClaimValidationError> {
    if !options.reference_time_seconds.is_finite()
        || !options.clock_tolerance_seconds.is_finite()
        || options.clock_tolerance_seconds < 0.0
    {
        return Some(ClaimValidationError::InvalidReferenceTime);
    }
    // 公開フィールドのため、デコード以外の経路で非有限値が入り得る
    for (number, name) in [
        (claims.expiration, "exp"),
        (claims.not_before, "nbf"),
        (claims.issued_at, "iat"),
        (claims.moqt_reval, "moqt-reval"),
    ] {
        if number.is_some_and(|number| !number.is_finite()) {
            return Some(ClaimValidationError::NonFiniteClaim(name));
        }
    }
    if let Some(catdpop) = &claims.catdpop
        && catdpop
            .window_seconds
            .is_some_and(|window| !window.is_finite())
    {
        return Some(ClaimValidationError::NonFiniteClaim("catdpop window"));
    }
    if let Some(expiration) = claims.expiration
        && options.reference_time_seconds > expiration + options.clock_tolerance_seconds
    {
        return Some(ClaimValidationError::Expired);
    }
    if let Some(not_before) = claims.not_before
        && options.reference_time_seconds + options.clock_tolerance_seconds < not_before
    {
        return Some(ClaimValidationError::NotYetValid);
    }
    if !options.expected_issuers.is_empty() {
        let matches = claims
            .issuer
            .as_deref()
            .is_some_and(|issuer| options.expected_issuers.contains(&issuer));
        if !matches {
            return Some(ClaimValidationError::IssuerMismatch);
        }
    }
    if !options.expected_audiences.is_empty() {
        let matches = claims
            .audience
            .iter()
            .any(|audience| options.expected_audiences.contains(&audience.as_str()));
        if !matches {
            return Some(ClaimValidationError::AudienceMismatch);
        }
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
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
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
                    |ctx| noprop::sample_f64_in(ctx, 0.0, 100.0),
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
        let options = ClaimValidationOptions {
            reference_time_seconds: reference,
            clock_tolerance_seconds: tolerance,
            expected_issuers: &expected_issuer_refs,
            expected_audiences: &expected_audience_refs,
        };

        let expected = expected_validation_error(&claims, &options);
        assert_eq!(
            claims.validate(&options),
            match expected {
                Some(error) => Err(error),
                None => Ok(()),
            },
            "検証結果がモデルと一致しない: claims={claims:?} reference={reference} tolerance={tolerance}"
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
    Ok(())
}
