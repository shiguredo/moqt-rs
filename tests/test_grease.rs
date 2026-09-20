/// GREASE ユーティリティの単体テスト
///
/// draft-ietf-moq-transport-21 §13 (Grease) の境界条件および near-miss 値など、
/// PBT では書きにくいケースを集中的に検証する。
use shiguredo_moqt::grease::{GREASE_BASE, GREASE_INTERVAL, GREASE_MAX, generate, is_grease};

#[test]
fn generate_small_values_match_pattern() {
    for n in 0..=100u64 {
        let v = generate(n).expect("小さな n はオーバーフローしない");
        assert_eq!(v, GREASE_INTERVAL * n + GREASE_BASE);
        assert!(
            is_grease(v),
            "n={n} のとき is_grease({v:#x}) は true であるべき"
        );
    }
}

#[test]
fn is_grease_rejects_below_base() {
    for v in 0..GREASE_BASE {
        assert!(!is_grease(v), "{v:#x} は false が期待される");
    }
}

#[test]
fn generate_rejects_beyond_max() {
    let max_n = (GREASE_MAX - GREASE_BASE) / GREASE_INTERVAL;
    assert_eq!(generate(max_n + 1), None);
}
