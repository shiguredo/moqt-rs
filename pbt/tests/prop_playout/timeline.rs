//! 音声と映像の共通の時間軸のプロパティテスト
//!
//! 表示時刻の線形性、リセット、世代、表示の遅れの上下限、実績のずれを検証する。

use pbt::common::test_runner;
use shiguredo_moqt::playout::timeline::{
    PlayoutTimeline, TIMELINE_BASE_DRIFT_US, TIMELINE_DISCONTINUITY_US,
    TIMELINE_MAX_COMPENSATED_DIFFERENCE_US, TIMELINE_MAX_PRESENTATION_DELAY_MS,
    TIMELINE_SYNC_MIN_DELTA_US, TimelineConfig, Track, UnsharedReason,
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

/// 同期の制御は移す分の上限を超えず、共有の判定と内訳が常に整合する
///
/// 基準がずれ続ける (ドリフトする) 到着列でも、共有している間は 2 つのトラックの
/// 表示時刻の差が目標の差 (自然な差から上限を引いた値) を超えないこと、ずれている側は
/// 表示時刻を返さないこと、表示時刻が内訳から計算できることを検証する。
#[test]
fn sync_never_moves_more_than_the_compensation_limit() -> noprop::TestResult {
    let shared_seen = std::cell::Cell::new(0usize);
    let unobserved_seen = std::cell::Cell::new(0usize);
    let unshared_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let mut timeline = PlayoutTimeline::new();
        let interval_us = noprop::sample_u64_in(ctx, 8_000..=50_000) as i64;
        // トラックごとの基準のドリフト (毎秒)。0 と上限付近の値を混ぜる
        let audio_drift_us_per_second = noprop::sample_with_boundaries(
            ctx,
            &[-40_000, -20_000, 0, 20_000, 40_000],
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_u64_in(ctx, 0..=80_000) as i64 - 40_000,
        );
        let video_drift_us_per_second = noprop::sample_with_boundaries(
            ctx,
            &[-40_000, -20_000, 0, 20_000, 40_000],
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_u64_in(ctx, 0..=80_000) as i64 - 40_000,
        );
        let mut audio_offset_us = noprop::sample_u64_in(ctx, 900_000..=1_100_000) as i64;
        let mut video_offset_us =
            audio_offset_us + noprop::sample_u64_in(ctx, 0..=600_000) as i64 - 300_000;
        let mut wall_us: i64 = 100_000_000;
        let steps = noprop::sample_usize_in(ctx, 20..=200);
        // 内訳と共有の判定、表示時刻の整合を確かめる
        let check = |timeline: &PlayoutTimeline,
                     audio_timestamp_us: Option<i64>,
                     video_timestamp_us: Option<i64>| {
            let breakdown = timeline.delay_breakdown();
            assert_eq!(
                breakdown.sharing_bases,
                breakdown.unshared_reason == UnsharedReason::None,
                "共有の判定と理由が一致しない: {breakdown:?}"
            );
            assert_eq!(breakdown.base_drift_limit_us, TIMELINE_BASE_DRIFT_US);
            for (track, timestamp_us, part) in [
                (Track::Audio, audio_timestamp_us, breakdown.audio),
                (Track::Video, video_timestamp_us, breakdown.video),
            ] {
                let Some(timestamp_us) = timestamp_us else {
                    continue;
                };
                // 表示時刻 = TIMESTAMP + 内訳の表示の遅れ (基準の遅れを含む)
                assert_eq!(
                    timeline.present_us(track, timestamp_us),
                    part.presentation_delay_us
                        .map(|delay_us| timestamp_us + delay_us),
                    "表示時刻と内訳が一致しない: {track:?} {breakdown:?}"
                );
            }
            if !breakdown.sharing_bases {
                return false;
            }
            let (Some(audio_delay_us), Some(video_delay_us)) = (
                breakdown.audio.presentation_delay_us,
                breakdown.video.presentation_delay_us,
            ) else {
                return false;
            };
            // 自然な差は基準の遅れと自分の揺らぎの和の差である (targetLatency は既定の 0)
            let natural_audio_us = breakdown
                .audio
                .base_delay_us
                .unwrap_or(0)
                .saturating_add(breakdown.audio.jitter_delay_us.unwrap_or(0));
            let natural_video_us = breakdown
                .video
                .base_delay_us
                .unwrap_or(0)
                .saturating_add(breakdown.video.jitter_delay_us.unwrap_or(0));
            let difference_us = natural_audio_us
                .saturating_sub(natural_video_us)
                .saturating_abs();
            let desired_us = TIMELINE_SYNC_MIN_DELTA_US
                .max(difference_us.saturating_sub(TIMELINE_MAX_COMPENSATED_DIFFERENCE_US));
            let gap_us = audio_delay_us
                .saturating_sub(video_delay_us)
                .saturating_abs();
            assert!(
                gap_us <= desired_us,
                "移す分が上限を超えた: gap={gap_us} desired={desired_us} {breakdown:?}"
            );
            true
        };

        for _ in 0..steps {
            wall_us = wall_us.saturating_add(interval_us);
            audio_offset_us += audio_drift_us_per_second.saturating_mul(interval_us) / 1_000_000;
            video_offset_us += video_drift_us_per_second.saturating_mul(interval_us) / 1_000_000;
            let audio_timestamp = wall_us.saturating_sub(audio_offset_us);
            let video_timestamp = wall_us.saturating_sub(video_offset_us);

            // 片方だけの観測でも整合していなければならない
            timeline.observe(Track::Audio, wall_us, audio_timestamp);
            if check(&timeline, Some(audio_timestamp), None) {
                shared_seen.set(shared_seen.get() + 1);
            } else if timeline.unshared_reason() == UnsharedReason::Unobserved {
                unobserved_seen.set(unobserved_seen.get() + 1);
            } else {
                unshared_seen.set(unshared_seen.get() + 1);
            }

            timeline.observe(Track::Video, wall_us, video_timestamp);
            if check(&timeline, Some(audio_timestamp), Some(video_timestamp)) {
                shared_seen.set(shared_seen.get() + 1);
            } else {
                unshared_seen.set(unshared_seen.get() + 1);
            }
        }
        Ok(())
    })?;
    assert!(
        shared_seen.get() > 0,
        "共有しているケースが生成されなかった\n{runner}"
    );
    assert!(
        unobserved_seen.get() > 0,
        "片方だけの観測のケースが生成されなかった\n{runner}"
    );
    assert!(
        unshared_seen.get() > 0,
        "共有しないケースが生成されなかった\n{runner}"
    );
    Ok(())
}
