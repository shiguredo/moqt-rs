#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::c4m::cat::{CatClaims, ClaimValidationOptions};

/// fuzz 入力
#[derive(Arbitrary, Debug)]
struct FuzzInput {
    /// CAT クレームセットの CBOR 候補
    claims: Vec<u8>,
    /// 検証に使う現在時刻
    reference_time_seconds: f64,
    /// `exp` / `nbf` に許容するずれ
    clock_tolerance_seconds: f64,
    /// 受信者が再検証できる最小間隔。`None` は再検証できないことを表す
    revalidation_interval_seconds: Option<f64>,
    /// 期待する `iss`
    expected_issuers: Vec<String>,
    /// 期待する `aud`
    expected_audiences: Vec<String>,
}

// 任意の CBOR を CAT のクレームセットとしてデコードし、任意の ClaimValidationOptions で
// CatClaims::validate を呼んで panic しないことを fuzz する
fuzz_target!(|input: FuzzInput| {
    let Ok(value) = shiguredo_moqt::c4m::cbor::decode(&input.claims) else {
        return;
    };
    let Ok(claims) = CatClaims::decode(&value) else {
        return;
    };
    let expected_issuers: Vec<&str> = input.expected_issuers.iter().map(String::as_str).collect();
    let expected_audiences: Vec<&str> = input
        .expected_audiences
        .iter()
        .map(String::as_str)
        .collect();
    let options = ClaimValidationOptions {
        reference_time_seconds: input.reference_time_seconds,
        clock_tolerance_seconds: input.clock_tolerance_seconds,
        expected_issuers: &expected_issuers,
        expected_audiences: &expected_audiences,
        revalidation_interval_seconds: input.revalidation_interval_seconds,
    };
    let _ = claims.validate(&options);
    // 既定のオプション (再検証できない受信者) でも検証する
    let _ = claims.validate(&ClaimValidationOptions::default());
});
