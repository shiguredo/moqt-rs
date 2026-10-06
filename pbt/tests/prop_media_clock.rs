//! メディア時刻から壁時計への換算のプロパティテスト
//!
//! 任意の観測列で、換算した時刻が単調に増え、Unix epoch より前にならないことを検証する。

use pbt::common::test_runner;
use shiguredo_moqt::media_clock::WallClockMapper;

/// 任意の観測列で、換算した時刻が単調に増え、負にならない
#[test]
fn converted_times_are_monotonic_and_non_negative() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut mapper = WallClockMapper::new();
        let mut media_us = noprop::sample_usize_in(ctx, 0..1_000_000) as i64;
        let mut wall_clock_us = noprop::sample_usize_in(ctx, 0..1_000_000) as i64;
        let mut previous = None;
        for _ in 0..noprop::sample_usize_in(ctx, 1..64) {
            let interval_us = noprop::sample_usize_in(ctx, 1..100_000) as i64;
            media_us += interval_us;
            // 撮影から読むまでの遅れを任意に与える
            wall_clock_us += interval_us + noprop::sample_usize_in(ctx, 0..500_000) as i64;
            mapper.observe(media_us, wall_clock_us);
            let converted = mapper
                .to_wall_clock_us(media_us, None)
                .expect("記録したので換算できること");
            assert!(converted >= 0, "converted={converted}");
            if let Some(previous) = previous {
                assert!(
                    converted > previous,
                    "previous={previous} converted={converted}"
                );
            }
            previous = Some(converted);
        }
        Ok(())
    })?;
    Ok(())
}
