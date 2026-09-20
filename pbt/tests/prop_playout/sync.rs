//! A/V 同期の遅延制御のプロパティテスト
//!
//! 相対遅延の範囲判定と、制御が返す遅延の上限を検証する。

use pbt::common::test_runner;
use shiguredo_moqt::playout::sync::{
    SYNC_MAX_DELTA_DELAY_MS, StreamSynchronization, SyncMeasurement, compute_relative_delay,
};

/// 相対遅延は範囲内なら入力どおり、超えるなら制御しない (None)
#[test]
fn relative_delay_is_none_outside_the_limit() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let audio = SyncMeasurement {
            latest_receive_time_us: 0,
            latest_capture_time_us: 0,
        };
        let receive_ms = noprop::sample_usize_in(ctx, 0..40_000) as i64 - 20_000;
        let video = SyncMeasurement {
            latest_receive_time_us: receive_ms * 1_000,
            latest_capture_time_us: 0,
        };
        match compute_relative_delay(audio, video) {
            Some(relative_ms) => {
                assert_eq!(relative_ms, receive_ms);
                assert!(relative_ms.saturating_abs() <= SYNC_MAX_DELTA_DELAY_MS);
            }
            None => {
                assert!(receive_ms.saturating_abs() > SYNC_MAX_DELTA_DELAY_MS);
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// 制御が返す遅延は、基準の遅延から 10 秒以内に収まる
#[test]
fn computed_delays_are_within_the_limit() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut synchronization = StreamSynchronization::new();
        let base_ms = noprop::sample_usize_in(ctx, 0..1_000) as i64;
        synchronization.set_target_buffering_delay(base_ms);
        for _ in 0..noprop::sample_usize_in(ctx, 1..16) {
            let relative_ms = noprop::sample_usize_in(ctx, 0..30_000) as i64 - 15_000;
            let audio_ms = noprop::sample_usize_in(ctx, 0..15_000) as i64;
            let video_ms = noprop::sample_usize_in(ctx, 0..15_000) as i64;
            if let Some(delays) = synchronization.compute_delays(relative_ms, audio_ms, video_ms) {
                assert!(
                    delays.audio_delay_ms >= base_ms,
                    "audio={} base={base_ms}",
                    delays.audio_delay_ms
                );
                assert!(
                    delays.video_delay_ms >= base_ms,
                    "video={} base={base_ms}",
                    delays.video_delay_ms
                );
                assert!(delays.audio_delay_ms <= base_ms + SYNC_MAX_DELTA_DELAY_MS);
                assert!(delays.video_delay_ms <= base_ms + SYNC_MAX_DELTA_DELAY_MS);
            }
        }
        Ok(())
    })?;
    Ok(())
}
