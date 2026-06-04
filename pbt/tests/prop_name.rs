//! draft-ietf-moq-transport-21 §8.8 (Representing Namespace and Track Names) / §8.8.1 (Parsing Serialized Names): Namespace / Track Name のシリアライズ・パースの property-based test。
//! bijective 性 (binary <-> シリアライズ文字列) のラウンドトリップを検証する。

use pbt::common::test_runner;
use shiguredo_moqt::{message::common::TrackNamespace, name::parse_name, name::serialize_name};

/// 空でない namespace フィールド (1..=16 バイト、任意値)
fn sample_field(ctx: &mut noprop::TestCaseContext) -> Vec<u8> {
    let len = noprop::sample_usize_in(ctx, 1..=16);
    noprop::sample_bytes_vec(ctx, len)
}

/// TrackNamespace::new の制約を満たす namespace。フィールド数 0..=6 (0 個を含む)、各フィールド
/// 1+ バイト、合計は小さく 4096 以下に収まる。
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

/// binary -> serialize -> parse -> binary のラウンドトリップが一致する (bijective)。
///
/// namespace 0 個 (フィールド数 0) と track name 空 (長さ 0) も生成範囲に含み、
/// 両方が実際に観測されたかをカバレッジゲートで検証する。
#[test]
fn roundtrip() -> noprop::TestResult {
    let empty_namespace_seen = std::cell::Cell::new(false);
    let empty_track_name_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let namespace = sample_namespace(ctx);
        if namespace.fields().is_empty() {
            empty_namespace_seen.set(true);
        }
        let track = sample_track_name(ctx);
        if track.is_empty() {
            empty_track_name_seen.set(true);
        }
        let serialized = serialize_name(&namespace, &track);
        let (parsed_ns, parsed_track) =
            parse_name(&serialized).expect("serialize_name の出力は parse_name で必ずパースできる");
        assert_eq!(&parsed_ns, &namespace);
        assert_eq!(&parsed_track, &track);

        // 逆方向: serialize -> parse -> serialize が同一文字列になる (正規形の安定性)。
        // 冗長 hex や曖昧 separator を出さないことを固定する。
        let reserialized = serialize_name(&parsed_ns, &parsed_track);
        assert_eq!(&reserialized, &serialized);
        Ok(())
    })?;
    assert!(
        empty_namespace_seen.get(),
        "フィールド数 0 の namespace のケースが生成されなかった\n{runner}"
    );
    assert!(
        empty_track_name_seen.get(),
        "長さ 0 の track name のケースが生成されなかった\n{runner}"
    );
    Ok(())
}
