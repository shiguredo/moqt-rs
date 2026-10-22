//! 音声と映像の共通の時間軸のプロパティテスト
//!
//! 表示時刻の線形性、リセット、世代、表示の遅れの上下限、実績のずれ、閉ループの目標の
//! 適用、出力の残差の扱いを検証する。

use pbt::common::test_runner;
use shiguredo_moqt::playout::feedback::AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US;
use shiguredo_moqt::playout::timeline::{
    PlayoutTimeline, TIMELINE_BASE_DRIFT_US, TIMELINE_BASE_UNSHARED_RELEASE_US,
    TIMELINE_DISCONTINUITY_US, TIMELINE_MAX_COMPENSATED_DIFFERENCE_US,
    TIMELINE_MAX_PRESENTATION_DELAY_MS, TIMELINE_SYNC_MIN_DELTA_US, TimelineConfig, Track,
    UnsharedReason,
};
use shiguredo_moqt::playout::timing::{AudioDelayFeedbackObservation, TimingSummary};

/// 閉ループへ渡す観測を 1 つ作る
///
/// `lateness_p50_us` が None のときは「まだ鳴らしていない」観測になる。鳴り始めまでの
/// 時間と余裕は、JS のテスト (`timeline.prop.ts`) と同じ値 (None または 50 ms) を渡す。
fn feedback_observation(
    at_us: i64,
    lateness_p50_us: Option<i64>,
    start_delay_p50_us: Option<i64>,
    slack_p50_us: Option<i64>,
) -> AudioDelayFeedbackObservation {
    let summary = |p50: i64| TimingSummary {
        p50,
        p95: p50,
        max: p50,
    };
    AudioDelayFeedbackObservation {
        at_us,
        lateness_us: lateness_p50_us.map(summary),
        start_delay_us: start_delay_p50_us.map(summary),
        slack_us: slack_p50_us.map(summary),
        backlog_misses: 0,
        backlog_us: 0,
        underrun_ms: 0,
        compression_us: 0,
        jitter_target_us: 0,
        // 穴と、音声だけの購読の到着基準の下限は、テストごとに上書きする
        gap_count: 0,
        gap_us: 0,
        arrival_delay_floor_us: None,
    }
}

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
        // トラックごとの基準のドリフト (毎秒)。0 と上限付近の値を混ぜ、差が反転する場合を
        // 通す (片側へ既に足した分が残ったまま反転すると、移す分が上限を超えやすい)
        let audio_drift_us_per_second = noprop::sample_with_boundaries(
            ctx,
            &[-100_000, -40_000, 0, 40_000, 100_000],
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_u64_in(ctx, 0..=200_000) as i64 - 100_000,
        );
        let video_drift_us_per_second = noprop::sample_with_boundaries(
            ctx,
            &[-100_000, -40_000, 0, 40_000, 100_000],
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_u64_in(ctx, 0..=200_000) as i64 - 100_000,
        );
        let mut audio_offset_us = noprop::sample_u64_in(ctx, 900_000..=1_100_000) as i64;
        // 2 つのトラックの基準の差。共有が続く範囲で大きく動かす
        let offset_difference_us = noprop::sample_with_boundaries(
            ctx,
            &[-800_000, -400_000, -300_000, 0, 300_000, 400_000, 800_000],
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_u64_in(ctx, 0..=1_600_000) as i64 - 800_000,
        );
        let mut video_offset_us = audio_offset_us + offset_difference_us;
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
            // 素の表示の遅れ (基準の遅れ + jitter buffer の遅延) の差を求める。補償の上限は
            // この差に掛かる。targetLatency は既定の 0 であるため jitter の遅延がそのまま
            // 表示の遅れになる
            let audio_base_us = breakdown.audio.base_delay_us.unwrap_or(0);
            let video_base_us = breakdown.video.base_delay_us.unwrap_or(0);
            let audio_jitter_us = breakdown.audio.jitter_delay_us.unwrap_or(0);
            let video_jitter_us = breakdown.video.jitter_delay_us.unwrap_or(0);
            let natural_audio_us = audio_base_us.saturating_add(audio_jitter_us);
            let natural_video_us = video_base_us.saturating_add(video_jitter_us);
            let natural_difference_us = natural_audio_us
                .saturating_sub(natural_video_us)
                .saturating_abs();
            // 合わせるのは上限 (TIMELINE_MAX_COMPENSATED_DIFFERENCE_US) までであり、超えた分は
            // A/V のずれとして残す。差が上限の中にあるときだけ不感帯 (TIMELINE_SYNC_MIN_DELTA_US)
            // に収まる
            let allowed_us = TIMELINE_SYNC_MIN_DELTA_US.max(
                natural_difference_us.saturating_sub(TIMELINE_MAX_COMPENSATED_DIFFERENCE_US),
            );
            // 足すのは即座、戻すのは毎秒の速さまで、という設計のため、後行側が前の状態で
            // 足した分はまだ戻り切っていないことがある。その分は差に残る
            let audio_total_us =
                natural_audio_us.saturating_add(breakdown.audio.sync_extra_delay_us);
            let video_total_us =
                natural_video_us.saturating_add(breakdown.video.sync_extra_delay_us);
            let trailing_extra_us = if audio_total_us >= video_total_us {
                breakdown.audio.sync_extra_delay_us
            } else {
                breakdown.video.sync_extra_delay_us
            };
            let gap_us = audio_delay_us
                .saturating_sub(video_delay_us)
                .saturating_abs();
            // 1 ms は丸め (TIMESTAMP の µs 変換) と、分布の窓と最後の観測の差を許す幅である
            assert!(
                gap_us <= allowed_us + trailing_extra_us + 1_000,
                "合わせない分を超えてずれが広がった: gap={gap_us} allowed={allowed_us} trailing_extra={trailing_extra_us} {breakdown:?}"
            );
            // この到着列は observe_audio_playout を呼ばないため、出力の残差は 0 のままである
            assert_eq!(
                breakdown.audio_playout_correction_us, 0,
                "出力の残差は観測していない: {breakdown:?}"
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

/// 差が戻ってからの早期解除は、戻った状態が 2 秒続いてから起きること
///
/// 閾値だけが動いた解除でも、解除のきっかけが動きだったかは見ない。差が水準へ戻った状態が
/// [`TIMELINE_BASE_UNSHARED_RELEASE_US`] 続いた時点で保持を待たずに共有へ戻る。観測の間隔を
/// 変えても、解除が早すぎたり遅すぎたりしないことを確かめる。
#[test]
fn the_unshared_hold_is_released_after_the_return_streak() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let interval_us = noprop::sample_u64_in(ctx, 20_000..=200_000) as i64;
        let mut timeline = PlayoutTimeline::new();
        let mut wall_us = 10_000_000;
        // 映像が 250 ms 遅い状態で水準を決める
        for _ in 0..110 {
            wall_us += interval_us;
            timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
            timeline.observe(Track::Video, wall_us, wall_us - 1_250_000);
        }
        assert!(timeline.sharing_bases(), "差が閾値の中なら共有すること");

        // 明示の `targetLatency` を上げると、差は動いていないのに閾値が下がって解除される
        timeline.set_target_latency_ms(400);
        wall_us += interval_us;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - 1_250_000);
        assert_eq!(
            timeline.unshared_reason(),
            UnsharedReason::Difference,
            "閾値を超えたら解除すること"
        );

        // 閾値を戻す。差が戻った状態が 2 秒続くまでは保持し、続いたら戻る
        timeline.set_target_latency_ms(0);
        let steps = (TIMELINE_BASE_UNSHARED_RELEASE_US / interval_us) + 2;
        let mut released = false;
        for step in 1..=steps {
            wall_us += interval_us;
            timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
            timeline.observe(Track::Video, wall_us, wall_us - 1_250_000);
            // 戻った状態を数え始めた時刻は、閾値を戻した後の最初の観測である
            let returned_us = (step - 1).saturating_mul(interval_us);
            if returned_us < TIMELINE_BASE_UNSHARED_RELEASE_US {
                assert!(
                    !timeline.sharing_bases(),
                    "戻った状態が 2 秒続く前に共有へ戻った: step={step} interval_us={interval_us}"
                );
                assert_eq!(
                    timeline.unshared_reason(),
                    UnsharedReason::Hold,
                    "保持していること: step={step}"
                );
            } else {
                assert!(
                    timeline.sharing_bases(),
                    "戻った状態が 2 秒続いたら共有へ戻ること: step={step} interval_us={interval_us}"
                );
                released = true;
                break;
            }
        }
        assert!(
            released,
            "早期解除に到達していない: interval_us={interval_us}"
        );
        Ok(())
    })?;
    Ok(())
}

/// 穴で広げた目標を上限の変更時に更新すること
///
/// `set_target_latency_ms` は閉ループの計器だけでなく、実際に使う jitter 遅れもその場で
/// 更新する。次の音声観測を待つと、停止中に上限を変更した利用者へ古い値が残る。
#[test]
fn set_target_latency_updates_the_applied_audio_target() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(64, |ctx| {
        let ceiling_ms = noprop::sample_usize_in(ctx, 100..=159) as i64;
        let mut timeline = PlayoutTimeline::new();
        for index in 0..4i64 {
            let at_us = index * 1_000_000;
            timeline.observe(Track::Audio, at_us, at_us);
            timeline.observe(Track::Video, at_us, at_us);
            // 穴の数と長さを毎秒増やし、閉ループの目標を上限まで広げる
            let mut observation = feedback_observation(at_us, None, None, None);
            observation.gap_count = index as u64;
            observation.gap_us = index * 80_000;
            timeline.observe_audio_playout(observation);
        }
        assert_eq!(
            timeline.playout_delay_us(),
            Some(AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US),
            "穴で広げた目標がそのまま使われること"
        );
        timeline.set_target_latency_ms(ceiling_ms);
        assert_eq!(
            timeline.playout_delay_us(),
            Some(ceiling_ms * 1_000),
            "上限の変更が次の観測を待たずに適用されること"
        );
        assert_eq!(
            timeline.delay_breakdown().audio.jitter_delay_us,
            Some(ceiling_ms * 1_000),
            "内訳の jitter 遅延も新しい上限になること"
        );
        assert_eq!(
            timeline.audio_delay_feedback_stats().applied_us,
            ceiling_ms * 1_000,
            "計器の適用値と実際に使う値が一致すること"
        );
        Ok(())
    })?;
    Ok(())
}

/// 共通の基準遅延を解消したら待ち時間へ持ち越さないこと
///
/// 共通の cache 遅延が減った分を同期の待ち時間へ移してはならない。両側の観測順も変え、
/// 片側ずつ更新される実際の入口で、追いつき後に通常の深さへ戻ることを確かめる。
#[test]
fn a_resolved_common_base_delay_is_not_carried_into_the_waiting_time() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(32, |ctx| {
        let frames = noprop::sample_usize_in(ctx, 5..=100) as i64;
        let audio_first = noprop::sample_bool(ctx);
        let mut timeline = PlayoutTimeline::new();
        let observe = |timeline: &mut PlayoutTimeline, at_us: i64, media_us: i64| {
            let tracks = if audio_first {
                [Track::Audio, Track::Video]
            } else {
                [Track::Video, Track::Audio]
            };
            for track in tracks {
                timeline.observe(track, at_us, media_us);
            }
            timeline.observe_audio_playout(feedback_observation(at_us, Some(0), None, None));
        };
        // cache から追いつく間の到着列 (基準がフレームごとに下がる)
        for index in 0..=frames {
            observe(
                &mut timeline,
                (frames * 10 + index * 10) * 1_000,
                index * 20_000,
            );
        }
        // 追いついた後の到着列 (基準が 0 で安定する)
        for elapsed_ms in (20..=2_000).step_by(20) {
            let at_us = (frames * 20 + elapsed_ms as i64) * 1_000;
            observe(&mut timeline, at_us, at_us);
        }
        let breakdown = timeline.delay_breakdown();
        assert_eq!(
            breakdown.audio.base_delay_us,
            Some(0),
            "音声の基準が 0 へ戻ること"
        );
        assert_eq!(
            breakdown.video.base_delay_us,
            Some(0),
            "映像の基準も 0 へ戻ること"
        );
        // 基準を含む表示の遅れは、基準が 0 なので遅れのみと一致する。過去の基準遅延を残さず、
        // 通常の深さ (音声の揺らぎの学習の 80 ms 以下) へ戻ること
        for (track, presentation_us) in [
            (Track::Audio, breakdown.audio.presentation_delay_us),
            (Track::Video, breakdown.video.presentation_delay_us),
        ] {
            assert!(
                presentation_us.unwrap_or(i64::MAX) <= 80_010,
                "過去の基準遅延を待ち時間へ残さないこと: {track:?} {presentation_us:?}"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// 壁時計の観測なしでも音声の閉ループ目標を適用すること
///
/// 壁時計の TIMESTAMP が無い音声でも再生結果は観測できる。基準を捏造せず、穴への適応値を
/// 実際の到着基準の再生へ返し、A/V の基準は共有しないことを確かめる。
#[test]
fn the_audio_closed_loop_target_applies_without_a_wall_clock_observation() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(32, |ctx| {
        let floor_ms = noprop::sample_usize_in(ctx, 100..=140) as i64;
        let floor_us = floor_ms * 1_000;
        let mut timeline = PlayoutTimeline::new();
        for index in 0..2i64 {
            let at_us = index * 1_000_000;
            let mut observation = feedback_observation(at_us, None, None, None);
            observation.gap_count = index as u64;
            observation.gap_us = index * 20_000;
            observation.arrival_delay_floor_us = Some(floor_us);
            timeline.observe_audio_playout(observation);
        }
        let playout_delay_us = timeline
            .playout_delay_us()
            .expect("再生の観測が 1 つ以上ある");
        assert_eq!(
            playout_delay_us,
            timeline.audio_delay_feedback_stats().applied_us,
            "計器の適用値と実際に使う値が一致すること"
        );
        assert!(
            playout_delay_us > floor_us,
            "穴を吸収する深さを返すこと: {playout_delay_us} floor={floor_us}"
        );
        assert_eq!(
            timeline.delay_breakdown().audio.base_delay_us,
            None,
            "壁時計の基準を捏造しないこと"
        );
        assert!(!timeline.sharing_bases(), "A/V の基準を共有しないこと");
        Ok(())
    })?;
    Ok(())
}

/// 残差の任意の増減でも遅延は必要値の最大を超えず、静まったら戻ること
///
/// 出力の残差を、実際の開始時刻が逆行しない小さな増減で動かす。過去の補償を必要値へ
/// 戻す制御では、残差の上限が一定でも共通の待ち時間だけが増え続ける。
#[test]
fn an_arbitrary_residual_keeps_the_wait_below_the_maximum_and_settles() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(16, |ctx| {
        let steps_len = noprop::sample_usize_in(ctx, 50..=100);
        let steps: Vec<i64> = (0..steps_len)
            .map(|_| noprop::sample_usize_in(ctx, 0..=20) as i64 - 10)
            .collect();
        let mut timeline = PlayoutTimeline::new();
        let mut residual_ms = 0i64;
        // 残差を決めた後は、毎回 10 ms ずつ静める
        let settle_steps = 220;
        let steps_all = steps
            .iter()
            .copied()
            .chain(std::iter::repeat_n(-10, settle_steps));
        for (index, step) in steps_all.enumerate() {
            residual_ms = if index < steps_len {
                (residual_ms + step).clamp(0, 80)
            } else {
                (residual_ms + step).max(0)
            };
            let at_us = 1_000_000 + index as i64 * 20_000;
            timeline.observe(Track::Audio, at_us, at_us);
            timeline.observe(Track::Video, at_us, at_us);
            let target_us = timeline
                .present_us(Track::Audio, at_us)
                .expect("音声の基準がある");
            let actual_us = target_us + residual_ms * 1_000;
            timeline.record_presentation_with_target(
                Track::Audio,
                at_us,
                actual_us,
                Some(target_us),
            );
            timeline.observe_audio_playout(feedback_observation(
                at_us,
                Some(0),
                Some(50_000),
                Some(50_000),
            ));
            let breakdown = timeline.delay_breakdown();
            assert!(
                timeline
                    .presentation_delay_us(Track::Video)
                    .unwrap_or(i64::MAX)
                    <= 160_010,
                "残差の上限以上に補償を重ねないこと: index={index}"
            );
            assert!(
                (0..=80_000).contains(&breakdown.audio_playout_correction_us),
                "出力の残差は 0〜80 ms に収まること: index={index} correction={}",
                breakdown.audio_playout_correction_us
            );
        }
        let breakdown = timeline.delay_breakdown();
        // 平滑化は重み付き平均 (correction * 時定数 + residual * elapsed) / (時定数 + elapsed)
        // を整数で求めるため、残差が 0 になった後も 0 へ収束する (差分へ掛ける形だと切り捨てで
        // 数 µs が残る)
        assert_eq!(
            breakdown.audio_playout_correction_us, 0,
            "静まった残差へ収束すること"
        );
        assert!(
            timeline
                .presentation_delay_us(Track::Video)
                .unwrap_or(i64::MAX)
                <= 50_010,
            "余分な共通の待ち時間を解放すること"
        );
        Ok(())
    })?;
    Ok(())
}

/// 穴と回復の任意の列でも既知のバッファ差を補償すること
///
/// 基準の差と、音切れで広がる目標を独立に動かす。時計の差が保護の内側で、キューに余裕が
/// ある場合は、制御のたびに音声と映像の予定が不感帯に収まらなければならない。
#[test]
fn gaps_and_recovery_still_compensate_a_known_buffer_difference() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(32, |ctx| {
        let base_difference_us = noprop::sample_usize_in(ctx, 0..=160_000) as i64 - 80_000;
        let gaps_len = noprop::sample_usize_in(ctx, 5..=40);
        let gaps: Vec<bool> = (0..gaps_len).map(|_| noprop::sample_bool(ctx)).collect();
        let mut timeline = PlayoutTimeline::new();
        let mut gap_count = 0u64;
        for (index, gap) in gaps.iter().enumerate() {
            let at_us = 1_000_000 + index as i64 * 1_000_000;
            // 音声と映像の観測の時刻をずらし、基準の遅れの差を作る
            timeline.observe(Track::Audio, at_us + base_difference_us.max(0), at_us);
            timeline.observe(Track::Video, at_us + (-base_difference_us).max(0), at_us);
            if *gap {
                gap_count += 1;
            }
            // 穴の累積を、閉ループが広げた目標として渡す
            let mut observation =
                feedback_observation(at_us + 80_000, Some(0), Some(50_000), Some(50_000));
            observation.gap_count = gap_count;
            observation.gap_us = gap_count as i64 * 20_000;
            timeline.observe_audio_playout(observation);
            let breakdown = timeline.delay_breakdown();
            assert!(
                breakdown.sharing_bases,
                "基準を共有すること: index={index} {breakdown:?}"
            );
            let audio_presentation_us = breakdown
                .audio
                .presentation_delay_us
                .expect("音声の基準がある");
            let video_presentation_us = breakdown
                .video
                .presentation_delay_us
                .expect("映像の基準がある");
            assert!(
                (audio_presentation_us - video_presentation_us).abs() <= 5_010,
                "制御を反映した直後から、既知の遅れの差が補償されること: index={index} {breakdown:?}"
            );
            assert!(
                timeline
                    .presentation_delay_us(Track::Video)
                    .unwrap_or(i64::MAX)
                    <= TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000,
                "映像の表示の遅れが上限を超えないこと: index={index}"
            );
        }
        Ok(())
    })?;
    Ok(())
}
