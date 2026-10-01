//! 到着の遅れからの目標遅延の学習のプロパティテスト
//!
//! 任意の観測列で目標遅延が定義された範囲に収まることを検証する。

use pbt::common::test_runner;
use shiguredo_moqt::playout::delay::{
    AUDIO_DELAY_BUCKET_MS, AUDIO_DELAY_BUCKETS, AudioDelayManager,
};

/// 任意の観測列で、目標遅延が分位点の範囲 (20 から 2000 ms) に収まる
#[test]
fn target_delay_stays_in_range() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut manager = AudioDelayManager::new();
        let mut capture_ms = noprop::sample_usize_in(ctx, 0..1_000_000) as i64;
        for _ in 0..noprop::sample_usize_in(ctx, 1..64) {
            capture_ms += noprop::sample_usize_in(ctx, 0..5_000) as i64;
            let delay_ms = noprop::sample_usize_in(ctx, 0..3_000) as i64;
            manager.observe((capture_ms + delay_ms) * 1_000, capture_ms * 1_000);
            let target_ms = manager.target_delay_ms();
            assert!(
                (AUDIO_DELAY_BUCKET_MS..=AUDIO_DELAY_BUCKETS as i64 * AUDIO_DELAY_BUCKET_MS)
                    .contains(&target_ms),
                "target_ms={target_ms}"
            );
        }
        Ok(())
    })?;
    Ok(())
}
