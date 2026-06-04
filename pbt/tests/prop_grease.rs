use pbt::common::test_runner;
use shiguredo_moqt::grease::{GREASE_BASE, GREASE_INTERVAL, GREASE_MAX, generate, is_grease};

/// `generate(n)` が返す値が `Some` になる最大の n
///
/// `GREASE_MAX` を超える値は `generate` が `None` を返すため、Some/None の両方の
/// 分岐を観測できるように境界値として用いる。
const GENERATE_MAX_N: u64 = (GREASE_MAX - GREASE_BASE) / GREASE_INTERVAL;

/// `generate(n)` が返した値は必ず `is_grease` で true になる
#[test]
fn generate_roundtrip_to_is_grease() -> noprop::TestResult {
    // `generate(n)` が Some を返す分岐と None を返す分岐の両方を観測できたかを数える
    let some_seen = std::cell::Cell::new(false);
    let none_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let n = noprop::sample_with_boundaries(
            ctx,
            &[0u64, 1, GENERATE_MAX_N / 2, GENERATE_MAX_N, u64::MAX],
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_u64(ctx),
        );
        if let Some(v) = generate(n) {
            assert!(is_grease(v));
            assert!(v <= GREASE_MAX);
            assert_eq!(v, GREASE_INTERVAL.wrapping_mul(n).wrapping_add(GREASE_BASE));
            some_seen.set(true);
        } else {
            none_seen.set(true);
        }
        Ok(())
    })?;
    assert!(
        some_seen.get(),
        "generate(n) が Some を返すケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        none_seen.get(),
        "generate(n) が None を返すケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// パターンに合致する任意の値は GREASE として認識される
#[test]
fn pattern_match_is_recognized() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let n = noprop::sample_u64_in(ctx, 0..=1_000_000);
        let v = generate(n).expect("n は GREASE_MAX に収まること");
        assert!(is_grease(v));
        // パターン外の near-miss は非 GREASE
        for offset in 1..GREASE_INTERVAL {
            assert!(!is_grease(v + offset));
        }
        Ok(())
    })?;
    Ok(())
}
