//! 音声と映像の共通の時間軸のプロパティテスト
//!
//! 表示時刻の線形性、リセット、世代、表示の遅れの上下限、実績のずれを検証する。

use pbt::common::test_runner;
use shiguredo_moqt::playout::timeline::{
    PlayoutTimeline, TIMELINE_DISCONTINUITY_US, TIMELINE_MAX_PRESENTATION_DELAY_MS, TimelineConfig,
    Track,
};

/// 表示時刻は TIMESTAMP の差だけ動き、観測が無いトラックは返らない
#[test]
fn presentation_time_follows_the_timestamp() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut timeline = PlayoutTimeline::new();
        assert!(timeline.present_us(Track::Audio, 0).is_none());
        assert!(timeline.present_us(Track::Video, 0).is_none());

        let wall_us = noprop::sample_usize_in(ctx, 0..1_000_000_000) as i64;
        let timestamp_us = noprop::sample_usize_in(ctx, 0..1_000_000_000) as i64;
        timeline.observe(Track::Audio, wall_us, timestamp_us);
        // 観測したトラックだけ表示時刻が返る
        assert!(timeline.present_us(Track::Audio, timestamp_us).is_some());
        assert!(timeline.present_us(Track::Video, timestamp_us).is_none());

        // 同じ状態で求めた表示時刻は、TIMESTAMP の差だけ進む
        let delta_us = noprop::sample_usize_in(ctx, 0..1_000_000_000) as i64;
        let first_us = timeline
            .present_us(Track::Audio, timestamp_us)
            .expect("観測済みのトラックの表示時刻が読める");
        let second_us = timeline
            .present_us(Track::Audio, timestamp_us + delta_us)
            .expect("観測済みのトラックの表示時刻が読める");
        assert_eq!(second_us - first_us, delta_us);
        Ok(())
    })?;
    Ok(())
}

/// リセット後は表示時刻が返らず、世代は残る
#[test]
fn reset_hides_the_presentation_time() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut timeline = PlayoutTimeline::new();
        let wall_us = 100_000_000;
        let timestamp_us = noprop::sample_usize_in(ctx, 0..100_000_000) as i64;
        timeline.observe(Track::Audio, wall_us, timestamp_us);
        timeline.observe(Track::Video, wall_us, timestamp_us);
        let generation = timeline.generation();
        timeline.reset();
        assert!(timeline.present_us(Track::Audio, timestamp_us).is_none());
        assert!(timeline.present_us(Track::Video, timestamp_us).is_none());
        assert_eq!(timeline.generation(), generation + 1);
        Ok(())
    })?;
    Ok(())
}

/// 基準から 2 秒以上離れた観測で世代が進む
#[test]
fn generation_advances_on_a_discontinuity() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut timeline = PlayoutTimeline::new();
        let wall_us = 100_000_000;
        let timestamp_us = 10_000_000;
        timeline.observe(Track::Audio, wall_us, timestamp_us);
        let generation = timeline.generation();
        let jump_us = 900_000 + noprop::sample_usize_in(ctx, 0..2_500_000) as i64;
        timeline.observe(Track::Audio, wall_us, timestamp_us + jump_us);
        if jump_us >= TIMELINE_DISCONTINUITY_US {
            assert_eq!(timeline.generation(), generation + 1);
        } else {
            assert_eq!(timeline.generation(), generation);
        }
        Ok(())
    })?;
    Ok(())
}

/// 表示の遅れは上限を超えず、targetLatency の下限を守る
#[test]
fn presentation_delay_respects_the_limits() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let target_latency_ms = noprop::sample_usize_in(ctx, 0..400) as i64;
        let config = TimelineConfig {
            target_latency_ms,
            ..TimelineConfig::default()
        };
        let mut timeline = PlayoutTimeline::with_config(config);
        let wall_us = 100_000_000;
        let timestamp_us = 10_000_000;
        timeline.observe(Track::Audio, wall_us, timestamp_us);
        timeline.observe(Track::Video, wall_us, timestamp_us);
        for track in [Track::Audio, Track::Video] {
            let delay_us = timeline
                .presentation_delay_us(track)
                .expect("観測後は読める");
            assert!(
                delay_us <= TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000,
                "上限を超えない: {delay_us}"
            );
            assert!(
                delay_us >= target_latency_ms * 1_000,
                "下限を下回らない: {delay_us}"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// 実績が両方そろわないと A/V のずれは読めない
#[test]
fn skew_needs_both_records() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut timeline = PlayoutTimeline::new();
        assert_eq!(timeline.skew_us(), None);
        let timestamp_us = noprop::sample_usize_in(ctx, 0..100_000_000) as i64;
        let presented_us = noprop::sample_usize_in(ctx, 0..100_000_000) as i64;
        timeline.record_presentation(Track::Audio, timestamp_us, presented_us);
        assert_eq!(timeline.skew_us(), None, "片方だけでは読めない");
        timeline.record_presentation(Track::Video, timestamp_us, presented_us);
        assert_eq!(timeline.skew_us(), Some(0), "同じ実績ならずれは 0");
        Ok(())
    })?;
    Ok(())
}
