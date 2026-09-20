//! draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation): MSF URI fragment の
//! パースの property-based test。

use pbt::common::test_runner;
use shiguredo_moqt::{
    message::common::TrackNamespace, msf::uri::parse_msf_fragment, name::serialize_name,
};

/// 空でない namespace フィールド (1..=16 バイト、任意値)
fn sample_field(ctx: &mut noprop::TestCaseContext) -> Vec<u8> {
    let len = noprop::sample_usize_in(ctx, 1..=16);
    noprop::sample_bytes_vec(ctx, len)
}

/// TrackNamespace::new の制約を満たす namespace (フィールド数 0..=6)
fn sample_namespace(ctx: &mut noprop::TestCaseContext) -> TrackNamespace {
    let field_count = noprop::sample_usize_in(ctx, 0..=6);
    let fields = (0..field_count).map(|_| sample_field(ctx)).collect();
    TrackNamespace::new(fields).expect("小さな非空フィールドは正当である")
}

/// track name (0..=16 バイト、空を含む任意値)
fn sample_track_name(ctx: &mut noprop::TestCaseContext) -> Vec<u8> {
    let len = noprop::sample_usize_in(ctx, 0..=16);
    noprop::sample_bytes_vec(ctx, len)
}

/// serialize_name の出力は `msf:` fragment としてパースでき、namespace / track name が一致する
#[test]
fn fragment_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let namespace = sample_namespace(ctx);
        let track_name = sample_track_name(ctx);
        let serialized = serialize_name(&namespace, &track_name);
        let fragment = parse_msf_fragment(&format!("msf:{serialized}"))
            .expect("serialize_name の出力は MSF fragment としてパースできる");
        assert_eq!(fragment.namespace, namespace);
        assert_eq!(fragment.track_name, track_name);
        Ok(())
    })?;
    Ok(())
}

// 予約パラメータ (connection / location-range / wallclock-range) の値型パースは
// URI 構造が固定で入力のランダム化が結果に寄与しないため、単体テスト
// (tests/test_msf/uri.rs) が担う。
