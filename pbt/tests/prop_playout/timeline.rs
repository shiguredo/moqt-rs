//! 音声と映像の共通の時間軸のプロパティテスト
//!
//! moqt-js の `timeline.prop.ts` / `timelineLowLatency.prop.ts` の意図を、マイクロ秒の
//! 整数で動く [`PlayoutTimeline`] に対して固定する。観測の順序と揺らぎの任意の列に対して、
//! 表示時刻の式・遅延の上限・音声予算の範囲・同期の補償の上限・共有の判定の整合を確かめる。
//! 個別の規則 (基準の差が閾値を超えたときのフォールバック、ドリフトの持続判定、保持、
//! 切り下げの計器、TIMESTAMP の飛び) は `tests/test_playout/timeline.rs` の単体テストが
//! 固定する。

use pbt::common::test_runner;
use shiguredo_moqt::playout::timeline::{
    PlayoutTimeline, TIMELINE_ARRIVAL_DELAY_US, TIMELINE_AUDIO_DELAY_FLOOR_US,
    TIMELINE_BASE_DRIFT_US, TIMELINE_BASE_UNSHARED_RELEASE_US, TIMELINE_DISCONTINUITY_US,
    TIMELINE_MAX_COMPENSATED_DIFFERENCE_US, TIMELINE_MAX_PRESENTATION_DELAY_MS,
    TIMELINE_MIN_BASE_DIFFERENCE_MS, TIMELINE_QUEUE_HEADROOM_FRAMES, TIMELINE_SYNC_MIN_DELTA_US,
    TIMELINE_WINDOW_US, TimelineConfig, Track, UnsharedReason, playout_queue_cap_us,
};

/// 送信側の壁時計 (Unix epoch マイクロ秒)。メディア時刻 0 の TIMESTAMP にする
const EPOCH_US: i64 = 1_790_263_445_000_000;

/// 30 fps のフレーム間隔 (マイクロ秒)
const FRAME_US: i64 = 1_000_000 / 30;

/// 表示待ちのキューの上限 (枚)。[`TimelineConfig`] の既定値と同じ値にする
const MAX_QUEUED_FRAMES: usize = 24;

/// 同時刻の表示時刻の差として許す幅 (マイクロ秒)
///
/// moqt-js はミリ秒の浮動小数で計算するため 0.01 ms。本実装はマイクロ秒の整数であり、
/// テストが与えたフレーム間隔と実装が測った間隔の差だけを許す。
const TOLERANCE_US: i64 = 10;

/// メディア時刻のフレームの TIMESTAMP (Unix epoch マイクロ秒)
fn timestamp_of(media_us: i64) -> i64 {
    EPOCH_US + media_us
}

/// 2 つのトラックの素の表示の遅れ (基準の遅れ + jitter buffer の遅延) の差 (マイクロ秒)
///
/// 同期の制御が足す前の差である。合わせる量の上限はこの差に掛かる。
fn natural_difference_us_of(timeline: &PlayoutTimeline) -> i64 {
    let breakdown = timeline.delay_breakdown();
    let natural_of = |track: shiguredo_moqt::playout::timeline::TrackBreakdown| {
        track.base_delay_us.unwrap_or(0) + track.jitter_delay_us.unwrap_or(0)
    };
    (natural_of(breakdown.audio) - natural_of(breakdown.video)).saturating_abs()
}

/// 決定論的な擬似乱数 (0 以上 1 未満)。moqt-js の `seededRandom` と同じ線形合同法
fn seeded_random(seed: i64) -> impl FnMut() -> f64 {
    let mut state = seed % 2_147_483_647;
    if state <= 0 {
        state += 2_147_483_646;
    }
    move || {
        state = (state.saturating_mul(16_807)) % 2_147_483_647;
        (state - 1) as f64 / 2_147_483_646.0
    }
}

/// 表示時刻は TIMESTAMP の差だけ動き、観測が無いトラックは返らないこと
#[test]
fn presentation_time_follows_the_timestamp() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
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
            .present_us(Track::Audio, timestamp_us.saturating_add(delta_us))
            .expect("観測済みのトラックの表示時刻が読める");
        assert_eq!(second_us - first_us, delta_us);
        Ok(())
    })?;
    Ok(())
}

/// リセット後は表示時刻が返らず、世代は残ること
#[test]
fn reset_hides_the_presentation_time() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
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

/// 基準から 2 秒以上離れた観測で世代が進むこと
#[test]
fn generation_advances_on_a_discontinuity() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
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

/// 表示の遅れは上限を超えず、targetLatency の下限を守ること
#[test]
fn presentation_delays_respect_the_limits() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
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
        let cap_us = timeline.delay_breakdown().presentation_delay_cap_us;
        for (track, delay_us) in [
            (Track::Audio, timeline.presentation_extra_delay_us()),
            (Track::Video, timeline.video_delay_us()),
        ] {
            let delay_us = delay_us.expect("観測後は読める");
            assert!(
                delay_us <= cap_us,
                "{track:?} の表示の遅れが上限を超えない: {delay_us} cap={cap_us}"
            );
            assert!(
                delay_us >= (target_latency_ms * 1_000).min(cap_us),
                "{track:?} の表示の遅れが下限を下回らない: {delay_us}"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// 表示時刻と内訳と共有の判定が常に整合すること
#[test]
fn presentation_and_breakdown_stay_consistent() -> noprop::TestResult {
    let shared_seen = std::cell::Cell::new(0usize);
    let unobserved_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(64, |ctx| {
        let mut timeline = PlayoutTimeline::new();
        let interval_us = noprop::sample_u64_in(ctx, 8_000..=50_000) as i64;
        // トラックごとの基準のドリフト (毎秒)。0 と上限付近の値を混ぜ、差が反転する場合を
        // 通す (片側へ既に足した分が残ったまま反転すると、移す分が上限を超えやすい)
        let audio_drift_us_per_second =
            noprop::sample_u64_in(ctx, 0..=200_000) as i64 - 100_000;
        let video_drift_us_per_second =
            noprop::sample_u64_in(ctx, 0..=200_000) as i64 - 100_000;
        let mut audio_offset_us = noprop::sample_u64_in(ctx, 900_000..=1_100_000) as i64;
        let offset_difference_us = noprop::sample_u64_in(ctx, 0..=1_600_000) as i64 - 800_000;
        let mut video_offset_us = audio_offset_us + offset_difference_us;
        let mut wall_us: i64 = 100_000_000;
        let steps = noprop::sample_usize_in(ctx, 20..=200);
        for step in 0..steps {
            wall_us = wall_us.saturating_add(interval_us);
            audio_offset_us += audio_drift_us_per_second.saturating_mul(interval_us) / 1_000_000;
            video_offset_us += video_drift_us_per_second.saturating_mul(interval_us) / 1_000_000;
            let audio_timestamp_us = wall_us.saturating_sub(audio_offset_us);
            let video_timestamp_us = wall_us.saturating_sub(video_offset_us);
            if step % 2 == 0 {
                timeline.observe(Track::Audio, wall_us, audio_timestamp_us);
            } else {
                timeline.observe(Track::Video, wall_us, video_timestamp_us);
            }

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
                assert_eq!(
                    timeline.present_us(track, timestamp_us),
                    part.presentation_delay_us
                        .map(|delay_us| timestamp_us + delay_us),
                    "表示時刻と内訳が一致しない: {track:?} {breakdown:?}"
                );
            }
            if breakdown.sharing_bases {
                shared_seen.set(shared_seen.get() + 1);
            } else if breakdown.unshared_reason == UnsharedReason::Unobserved {
                unobserved_seen.set(unobserved_seen.get() + 1);
            }
            // 表示の遅れから基準の遅れを除いた分 (jitter buffer の遅延) は上限を超えない
            for part in [breakdown.audio, breakdown.video] {
                if let (Some(presentation_us), base_us) =
                    (part.presentation_delay_us, part.base_delay_us.unwrap_or(0))
                {
                    assert!(
                        presentation_us - base_us <= part.presentation_delay_cap_us,
                        "jitter buffer の遅延が上限を超えた: {presentation_us} base={base_us} cap={}",
                        part.presentation_delay_cap_us
                    );
                }
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
        "未観測のケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// 音声の予算が常に 50〜100 ms に収まること
#[test]
fn the_audio_budget_stays_between_the_floor_and_the_cap() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(64, |ctx| {
        let arrivals_len = noprop::sample_usize_in(ctx, 100..=250);
        let mut timeline = PlayoutTimeline::new();
        let mut previous_arrival_us = 0i64;
        for index in 0..arrivals_len {
            let timestamp_us = 1_000_000 + index as i64 * 20_000;
            let jitter_us = noprop::sample_usize_in(ctx, 0..=80_000) as i64;
            let lead_us = noprop::sample_usize_in(ctx, 0..=150_000) as i64;
            let arrival_us = (previous_arrival_us + 100).max(timestamp_us + jitter_us);
            previous_arrival_us = arrival_us;
            timeline.observe(Track::Audio, arrival_us, timestamp_us);
            timeline.observe_audio_output_lead(lead_us, arrival_us);
            let delay = timeline.audio_delay_stats();
            assert!(
                (TIMELINE_AUDIO_DELAY_FLOOR_US..=TIMELINE_ARRIVAL_DELAY_US)
                    .contains(&delay.target_us),
                "音声の予算が 50〜100 ms の外に出た: {}",
                delay.target_us
            );
            assert!(
                delay
                    .output_lead_us
                    .is_some_and(|lead_us| (0..=150_000).contains(&lead_us)),
                "出力の先行量が 0〜150 ms の外に出た: {:?}",
                delay.output_lead_us
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// 起動から周期的な揺らぎが続いても、初期判定で学習を止め続けないこと
#[test]
fn initial_learning_is_not_stopped_by_periodic_jitter() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(40, |ctx| {
        let fps = noprop::sample_choice(ctx, &[20usize, 30, 50, 60, 120]);
        let jitter_us = noprop::sample_usize_in(ctx, 20_000..=140_000) as i64;
        let mut timeline = PlayoutTimeline::new();
        let mut previous_arrival_us = 0i64;
        for index in 0..fps * 3 {
            let media_us = index as i64 * 1_000_000 / fps as i64;
            // 遅着の後も出力順と壁時計の単調性を保つ。ピークだけが通常間隔になる場合も
            // 含む
            let arrival_us = (previous_arrival_us + 100)
                .max(media_us + 20_000 + if index % 3 == 2 { jitter_us } else { 0 });
            previous_arrival_us = arrival_us;
            timeline.observe(Track::Audio, arrival_us, media_us);
            timeline.observe_audio_output_lead(43_000, arrival_us);
        }
        assert_eq!(
            timeline.audio_delay_stats().jitter_us,
            Some(jitter_us),
            "fps={fps} の周期的な揺らぎを学習すること"
        );
        Ok(())
    })?;
    Ok(())
}

/// 起動中の一度の停滞が解消したら、配信周期にかかわらず初期の待ちを保持しないこと
#[test]
fn a_single_stall_is_not_kept() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(40, |ctx| {
        let fps = noprop::sample_choice(ctx, &[20usize, 30, 50, 60, 120]);
        let stall_us = noprop::sample_usize_in(ctx, 220_000..=800_000) as i64;
        let stream = noprop::sample_choice(ctx, &[Track::Audio, Track::Video]);
        let mut timeline = PlayoutTimeline::new();
        let mut previous_arrival_us = 0i64;
        let mut index = 0i64;
        while index * 1_000_000 / fps as i64 <= 100_000 + stall_us + 600_000 {
            let media_us = index * 1_000_000 / fps as i64;
            let extra_us = if (100_000..100_000 + stall_us).contains(&media_us) {
                100_000 + stall_us - media_us
            } else {
                0
            };
            let arrival_us = (previous_arrival_us + 100).max(media_us + 20_000 + extra_us);
            previous_arrival_us = arrival_us;
            timeline.observe(stream, arrival_us, media_us);
            if stream == Track::Audio {
                timeline.observe_audio_output_lead(43_000, arrival_us);
            }
            index += 1;
        }
        if stream == Track::Audio {
            assert_eq!(
                timeline.audio_delay_stats().target_us,
                TIMELINE_AUDIO_DELAY_FLOOR_US,
                "fps={fps}: 解消した停滞を予算に残さないこと"
            );
        } else {
            assert_eq!(timeline.video_delay_us(), Some(0));
        }
        Ok(())
    })?;
    Ok(())
}

/// 媒体が欠落しても正常な到着の予算と同期を変えないこと
#[test]
fn missing_media_keeps_the_budget() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(40, |ctx| {
        let losses_len = noprop::sample_usize_in(ctx, 100..=300);
        let losses: Vec<bool> = (0..losses_len).map(|_| noprop::sample_bool(ctx)).collect();
        let mut timeline = PlayoutTimeline::new();
        for (index, lost) in losses.iter().enumerate() {
            let media_us = 1_000_000 + index as i64 * 20_000;
            timeline.observe(Track::Video, media_us + 20_000, media_us);
            if !*lost {
                timeline.observe(Track::Audio, media_us + 20_000, media_us);
                timeline.observe_audio_output_lead(43_000, media_us + 20_000);
            }
            // 音声が一度観測された後は、欠落している各区間も含めて検証する
            if timeline.playout_delay_us().is_some() {
                assert_eq!(
                    timeline.audio_delay_stats().target_us,
                    TIMELINE_AUDIO_DELAY_FLOOR_US,
                    "欠落しても予算を変えないこと: index={index}"
                );
                let breakdown = timeline.delay_breakdown();
                let difference_us = breakdown
                    .audio
                    .presentation_delay_us
                    .unwrap_or(0)
                    .saturating_sub(breakdown.video.presentation_delay_us.unwrap_or(0));
                assert!(
                    difference_us.saturating_abs() <= TIMELINE_SYNC_MIN_DELTA_US + TOLERANCE_US,
                    "欠落しても同期を変えないこと: index={index} difference={difference_us}"
                );
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// 出力の残差の任意の増減でも遅延は必要値の最大を超えず、静まったら戻ること
#[test]
fn an_arbitrary_residual_keeps_the_wait_below_the_maximum_and_settles() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(16, |ctx| {
        let steps_len = noprop::sample_usize_in(ctx, 50..=100);
        // 残差の増減はミリ秒で生成する (JS の prop と同じ ±10 ms)
        let steps: Vec<i64> = (0..steps_len)
            .map(|_| (noprop::sample_usize_in(ctx, 0..=20) as i64 - 10) * 1_000)
            .collect();
        let mut timeline = PlayoutTimeline::new();
        let mut residual_us = 0i64;
        let settle_steps = 220usize;
        // 残差の列の後ろへ、収束を確かめるための 0 (減少のみ) を足して 1 本の列として回す。
        // 添字で元の列を引かないよう、イテレータを連結する
        let residuals = steps
            .iter()
            .copied()
            .chain(std::iter::repeat_n(0i64, settle_steps));
        for (index, step_us) in residuals.enumerate() {
            residual_us = if index < steps_len {
                (residual_us + step_us).clamp(0, 80_000)
            } else {
                (residual_us - 10_000).max(0)
            };
            let at_us = 1_000_000 + index as i64 * 20_000;
            timeline.observe(Track::Audio, at_us, at_us);
            timeline.observe(Track::Video, at_us, at_us);
            let target_us = timeline
                .present_us(Track::Audio, at_us)
                .expect("音声の基準がある");
            let actual_us = target_us + residual_us;
            timeline.record_presentation_with_target(
                Track::Audio,
                at_us,
                actual_us,
                Some(target_us),
            );
            assert!(
                timeline.video_delay_us().unwrap_or(i64::MAX) <= 130_010,
                "残差の上限以上に補償を重ねないこと: index={index}"
            );
            let correction_us = timeline.delay_breakdown().audio_playout_correction_us;
            assert!(
                (0..=80_000).contains(&correction_us),
                "出力の残差は 0〜80 ms に収まること: index={index} correction={correction_us}"
            );
        }
        let breakdown = timeline.delay_breakdown();
        assert!(
            breakdown.audio_playout_correction_us.abs() < 100,
            "静まった残差へ収束すること: {}",
            breakdown.audio_playout_correction_us
        );
        assert!(
            timeline.video_delay_us().unwrap_or(i64::MAX) <= TIMELINE_AUDIO_DELAY_FLOOR_US + 10,
            "余分な共通の待ち時間を解放すること: {:?}",
            timeline.video_delay_us()
        );
        Ok(())
    })?;
    Ok(())
}

/// 共通の基準遅延を解消したら待ち時間へ持ち越さないこと
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
        // 基準を含む表示の遅れは、基準が 0 なので遅れのみと一致する。過去の基準遅延を
        // 残さず、通常の深さ (音声の下限 50 ms 以下) へ戻ること
        for presentation_us in [
            breakdown.audio.presentation_delay_us,
            breakdown.video.presentation_delay_us,
        ] {
            assert!(
                presentation_us.unwrap_or(i64::MAX) <= TIMELINE_AUDIO_DELAY_FLOOR_US + 10,
                "過去の基準遅延を待ち時間へ残さないこと: {presentation_us:?}"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// 差が戻ってからの早期解除は、戻った状態が 2 秒続いてから起きること
#[test]
fn the_unshared_hold_is_released_after_the_return_streak() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(64, |ctx| {
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

        // 明示の `target_latency` を上げると、差は動いていないのに閾値が下がって解除される
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
        // 解除の数え直しは基準の差を記録するたび (250 ms か観測の間隔の大きい方) なので、
        // その分だけ遅れる
        let timeout_us = TIMELINE_BASE_UNSHARED_RELEASE_US + 3 * interval_us + 500_000;
        let mut released_at_us = None;
        for step in 1..=200 {
            wall_us += interval_us;
            timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
            timeline.observe(Track::Video, wall_us, wall_us - 1_250_000);
            if timeline.sharing_bases() {
                released_at_us = Some(step * interval_us);
                break;
            }
            // 差が戻った状態を数え始めるのは、閾値を戻した後の最初の記録である。
            // 2 秒を超えて数えているのに共有へ戻っていないのは不正である
            let returned_us = step * interval_us;
            assert!(
                returned_us < TIMELINE_BASE_UNSHARED_RELEASE_US + timeout_us,
                "早期解除に到達していない: step={step} interval_us={interval_us}"
            );
        }
        let released_at_us = released_at_us.expect("早期解除に到達していない");
        assert!(
            released_at_us >= TIMELINE_BASE_UNSHARED_RELEASE_US - interval_us,
            "戻った状態が 2 秒続く前に共有へ戻った: released={released_at_us}"
        );
        assert!(
            released_at_us <= timeout_us,
            "解除の条件が続いた後に共有へ戻ること: released={released_at_us} timeout={timeout_us}"
        );
        Ok(())
    })?;
    Ok(())
}

/// 同期の補償は移す分の上限を超えず、同時刻の表示時刻の差は合わせない分に収まること
#[test]
fn sync_never_moves_more_than_the_compensation_limit() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(32, |ctx| {
        let mut timeline = PlayoutTimeline::new();
        let interval_us = noprop::sample_u64_in(ctx, 8_000..=50_000) as i64;
        let audio_drift_us_per_second = noprop::sample_u64_in(ctx, 0..=200_000) as i64 - 100_000;
        let video_drift_us_per_second = noprop::sample_u64_in(ctx, 0..=200_000) as i64 - 100_000;
        let mut audio_offset_us = noprop::sample_u64_in(ctx, 900_000..=1_100_000) as i64;
        let offset_difference_us = noprop::sample_u64_in(ctx, 0..=1_600_000) as i64 - 800_000;
        let mut video_offset_us = audio_offset_us + offset_difference_us;
        let mut wall_us: i64 = 100_000_000;
        let steps = noprop::sample_usize_in(ctx, 20..=200);
        for _ in 0..steps {
            wall_us = wall_us.saturating_add(interval_us);
            audio_offset_us += audio_drift_us_per_second.saturating_mul(interval_us) / 1_000_000;
            video_offset_us += video_drift_us_per_second.saturating_mul(interval_us) / 1_000_000;
            timeline.observe(Track::Audio, wall_us, wall_us.saturating_sub(audio_offset_us));
            timeline.observe(Track::Video, wall_us, wall_us.saturating_sub(video_offset_us));

            let breakdown = timeline.delay_breakdown();
            assert_eq!(
                breakdown.sharing_bases,
                breakdown.unshared_reason == UnsharedReason::None,
                "共有の判定と理由が一致しない: {breakdown:?}"
            );
            if !breakdown.sharing_bases {
                continue;
            }
            let (Some(audio_delay_us), Some(video_delay_us)) = (
                breakdown.audio.presentation_delay_us,
                breakdown.video.presentation_delay_us,
            ) else {
                continue;
            };
            // 素の表示の遅れの差から、補償の上限を引いた分までが不感帯へ収まる
            let natural_difference_us = natural_difference_us_of(&timeline);
            let allowed_us = TIMELINE_SYNC_MIN_DELTA_US.max(
                natural_difference_us.saturating_sub(TIMELINE_MAX_COMPENSATED_DIFFERENCE_US),
            );
            // 足すのは即座、戻すのは毎秒の速さまで、という設計のため、後行側が前の状態で
            // 足した分はまだ戻り切っていないことがある。その分は差に残る
            let audio_total_us = breakdown.audio.base_delay_us.unwrap_or(0)
                + breakdown.audio.jitter_delay_us.unwrap_or(0)
                + breakdown.audio.sync_extra_delay_us;
            let video_total_us = breakdown.video.base_delay_us.unwrap_or(0)
                + breakdown.video.jitter_delay_us.unwrap_or(0)
                + breakdown.video.sync_extra_delay_us;
            let trailing_extra_us = if audio_total_us >= video_total_us {
                breakdown.audio.sync_extra_delay_us
            } else {
                breakdown.video.sync_extra_delay_us
            };
            let gap_us = audio_delay_us.saturating_sub(video_delay_us).saturating_abs();
            assert!(
                gap_us <= allowed_us + trailing_extra_us + TOLERANCE_US * 100,
                "合わせない分を超えてずれが広がった: gap={gap_us} allowed={allowed_us} trailing={trailing_extra_us} {breakdown:?}"
            );
            // 表示の遅れは上限を超えない
            assert!(
                video_delay_us - breakdown.video.base_delay_us.unwrap_or(0)
                    <= breakdown.presentation_delay_cap_us
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// 120 秒の到着列でも同時刻の表示時刻の差は合わせない分に収まること
#[test]
fn long_arrivals_keep_the_difference_within_the_uncompensated_part() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(5, |ctx| {
        let seed = noprop::sample_u64_in(ctx, 1..=1_000_000) as i64;
        let duration_us = 120_000_000i64;
        let audio_frame_us = 20_000i64;
        let video_frame_us = FRAME_US;
        let mut audio_random = seeded_random(seed);
        let mut video_random = seeded_random(seed + 977);
        let jitter_of = |random: &mut dyn FnMut() -> f64| -> i64 {
            let roll = random();
            if roll > 0.995 {
                return 200_000 + (random() * 40_000.0) as i64;
            }
            if roll > 0.975 {
                return 40_000 + (random() * 30_000.0) as i64;
            }
            (random() * 30_000.0) as i64
        };
        let mut arrivals: Vec<(i64, Track, i64)> = Vec::new();
        let mut index = 0i64;
        while index * audio_frame_us < duration_us {
            let media_us = index * audio_frame_us;
            arrivals.push((
                EPOCH_US + media_us + jitter_of(&mut audio_random),
                Track::Audio,
                media_us,
            ));
            index += 1;
        }
        let mut index = 0i64;
        while index * video_frame_us < duration_us {
            let media_us = index * video_frame_us;
            arrivals.push((
                EPOCH_US + media_us + jitter_of(&mut video_random),
                Track::Video,
                media_us,
            ));
            index += 1;
        }
        arrivals.sort_by_key(|(at_us, _, _)| *at_us);
        let mut timeline = PlayoutTimeline::new();
        for (at_us, track, media_us) in arrivals {
            timeline.observe(track, at_us, timestamp_of(media_us));
        }

        // 最後の 10 秒の同時刻の表示時刻の差の最大値
        let mut maximum_difference_us = 0i64;
        let mut media_us = duration_us - 10_000_000;
        while media_us < duration_us {
            let audio_us = timeline
                .present_us(Track::Audio, timestamp_of(media_us))
                .expect("音声の表示時刻が決まる");
            let video_us = timeline
                .present_us(Track::Video, timestamp_of(media_us))
                .expect("映像の表示時刻が決まる");
            maximum_difference_us =
                maximum_difference_us.max(audio_us.saturating_sub(video_us).saturating_abs());
            media_us += video_frame_us;
        }
        // 合わせるのは上限までであり、超えた分は A/V のずれとして残す。差が上限の中に
        // あるときだけ不感帯に収まる
        let natural_difference_us = natural_difference_us_of(&timeline);
        let allowed_us = TIMELINE_SYNC_MIN_DELTA_US.max(
            natural_difference_us.saturating_sub(TIMELINE_MAX_COMPENSATED_DIFFERENCE_US),
        );
        let breakdown = timeline.delay_breakdown();
        let audio_delay_us = breakdown.audio.base_delay_us.unwrap_or(0)
            + breakdown.audio.jitter_delay_us.unwrap_or(0)
            + breakdown.audio.sync_extra_delay_us;
        let video_delay_us = breakdown.video.base_delay_us.unwrap_or(0)
            + breakdown.video.jitter_delay_us.unwrap_or(0)
            + breakdown.video.sync_extra_delay_us;
        let trailing_extra_us = if audio_delay_us >= video_delay_us {
            breakdown.audio.sync_extra_delay_us
        } else {
            breakdown.video.sync_extra_delay_us
        };
        // 丸め (TIMESTAMP の µs 変換) と、分布の窓と最後の観測の差を許す幅
        let allowance_us = 100;
        assert!(
            maximum_difference_us <= allowed_us + trailing_extra_us + allowance_us,
            "seed={seed}: 同時刻の表示時刻の差が合わせない分に収まること: {maximum_difference_us} allowed={allowed_us} trailing={trailing_extra_us}"
        );
        // 100 ms は時計の基準の差に対する上限。既知のバッファ差は別に補償する
        let buffer_difference_us = (breakdown.audio.jitter_delay_us.unwrap_or(0)
            - breakdown.video.jitter_delay_us.unwrap_or(0))
        .saturating_abs();
        let compensation_budget_us = TIMELINE_MAX_COMPENSATED_DIFFERENCE_US + buffer_difference_us;
        assert!(breakdown.audio.sync_extra_delay_us <= compensation_budget_us + TOLERANCE_US);
        assert!(breakdown.video.sync_extra_delay_us <= compensation_budget_us + TOLERANCE_US);

        // 実績から求める同期ずれも、合わせない分までになる
        let timestamp_us = timestamp_of(duration_us - 1_000_000);
        let audio_present_us = timeline
            .present_us(Track::Audio, timestamp_us)
            .expect("音声の表示時刻が決まる");
        let video_present_us = timeline
            .present_us(Track::Video, timestamp_us)
            .expect("映像の表示時刻が決まる");
        timeline.record_presentation(Track::Audio, timestamp_us, audio_present_us);
        timeline.record_presentation(Track::Video, timestamp_us, video_present_us);
        let skew_us = timeline.skew_us().expect("同期ずれが求まること");
        assert!(
            skew_us.saturating_abs() <= allowed_us + trailing_extra_us + allowance_us,
            "seed={seed}: 同期ずれが合わせない分に収まること: {skew_us}"
        );
        // キューの上限を確かめるための公開 API の値 (テストが独立に計算できる量)
        assert!(playout_queue_cap_us(MAX_QUEUED_FRAMES, None) >= TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000 / 2);
        Ok(())
    })?;
    Ok(())
}

/// 音声の予算と同期の範囲が公開定数と整合すること
#[test]
fn the_audio_budget_uses_the_documented_constants() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(16, |ctx| {
        assert_eq!(
            TIMELINE_QUEUE_HEADROOM_FRAMES, 4,
            "キューの余裕は 4 枚であること"
        );
        assert_eq!(TIMELINE_WINDOW_US, 10_000_000);
        assert_eq!(TIMELINE_MIN_BASE_DIFFERENCE_MS, 100);
        let target_latency_ms = noprop::sample_usize_in(ctx, 0..=600) as i64;
        let mut timeline = PlayoutTimeline::with_config(TimelineConfig {
            target_latency_ms,
            ..TimelineConfig::default()
        });
        timeline.observe(Track::Audio, 10_000_000, 9_000_000);
        timeline.observe(Track::Video, 10_000_000, 9_000_000);
        // 上限に収まらない分は切り下げとして読める
        let cap_us = timeline.delay_breakdown().presentation_delay_cap_us;
        assert_eq!(
            timeline.target_latency_limited_us(),
            (target_latency_ms * 1_000).saturating_sub(cap_us).max(0)
        );
        Ok(())
    })?;
    Ok(())
}
