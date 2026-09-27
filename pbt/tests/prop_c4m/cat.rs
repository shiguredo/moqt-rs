//! CAT のクレームの property テスト

use pbt::common::{sample_bytes, test_runner};
use shiguredo_moqt::c4m::cat::{CatClaims, Confirmation};
use shiguredo_moqt::c4m::cbor::{self, Value};

use super::common::{sample_catdpop, sample_moqt_claim};

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
        if claims.moqt.is_some() {
            moqt_seen.set(moqt_seen.get() + 1);
        }
        if claims.catdpop.is_some() {
            catdpop_seen.set(catdpop_seen.get() + 1);
        }
        if !claims.raw.is_empty() {
            raw_seen.set(raw_seen.get() + 1);
        }
        let value = claims.encode().expect("クレームをエンコードできる");
        let encoded = cbor::encode(&value).expect("CBOR をエンコードできる");
        let decoded = CatClaims::decode(&cbor::decode(&encoded).expect("CBOR をデコードできる"))
            .expect("クレームをデコードできる");
        assert_eq!(decoded, claims, "ラウンドトリップでクレームが変わる");
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
