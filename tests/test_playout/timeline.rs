//! 音声と映像の共通の時間軸のテスト
//!
//! 公開 API (`PlayoutTimeline`) の契約を確認する。

use shiguredo_moqt::playout::delay::AUDIO_DELAY_START_MS;
use shiguredo_moqt::playout::feedback::{
    AUDIO_DELAY_FEEDBACK_MAX_STEP_US, AUDIO_DELAY_FEEDBACK_START_US, AudioDelayFeedbackReason,
};
use shiguredo_moqt::playout::scheduler::{AudioPlayoutBasis, AudioPlayoutPlay};
use shiguredo_moqt::playout::timeline::{
    PlayoutTimeline, TIMELINE_ARRIVAL_DELAY_US, TIMELINE_AUDIO_DELAY_FLOOR_US,
    TIMELINE_BASE_DRIFT_US, TIMELINE_MAX_COMPENSATED_DIFFERENCE_US, TimelineConfig, Track,
    UnsharedReason, audio_arrival_delay_us,
};
use shiguredo_moqt::playout::timing::{
    AudioDelayFeedbackObservation, AudioPlayoutTimingStats, TimingSummary,
};

/// 閉ループへ渡す観測を 1 つ作る
///
/// `lateness_p50_us` が None のときは「まだ鳴らしていない」観測になる。
fn feedback_observation(at_us: i64, lateness_p50_us: Option<i64>) -> AudioDelayFeedbackObservation {
    AudioDelayFeedbackObservation {
        at_us,
        lateness_us: lateness_p50_us.map(|p50| TimingSummary {
            p50,
            p95: p50,
            max: p50,
        }),
        start_delay_us: Some(TimingSummary {
            p50: 120_000,
            p95: 130_000,
            max: 140_000,
        }),
        slack_us: Some(TimingSummary {
            p50: 20_000,
            p95: 25_000,
            max: 30_000,
        }),
        backlog_misses: 0,
        backlog_us: 0,
    }
}

/// 映像の揺らぎを 40 ms、フレーム間隔を 33 ms にして観測を並べる
fn observe_jittered_video(timeline: &mut PlayoutTimeline, count: i64) {
    for index in 0..count {
        // 10 枚に 1 枚だけ 40 ms 遅れて届く
        let jitter_us = if index % 10 == 0 { 40_000 } else { 0 };
        let wall_us = 10_000_000 + index * 33_333 + jitter_us;
        let timestamp_us = 1_000_000 + index * 33_333;
        timeline.observe(Track::Video, wall_us, timestamp_us);
    }
}

#[test]
fn present_time_is_timestamp_plus_basis_and_delay() {
    let mut timeline = PlayoutTimeline::new();
    // 復号の出力が TIMESTAMP より 9 秒遅れている
    timeline.observe(Track::Audio, 10_000_000, 1_000_000);
    // 表示時刻 = TIMESTAMP + 基準の遅れ (9 秒) + 表示の遅れ (80 ms)
    assert_eq!(
        timeline.present_us(Track::Audio, 1_000_000),
        Some(10_080_000)
    );
    // 表示の遅れはスケジューラへ渡せる形で読める
    assert_eq!(timeline.presentation_delay_us(Track::Audio), Some(80_000));
    // 観測が無いトラックの表示時刻は返らない
    assert_eq!(timeline.present_us(Track::Video, 1_000_000), None);
}

#[test]
fn audio_delay_follows_the_learned_target() {
    let mut timeline = PlayoutTimeline::new();
    // 揺らぎの無い音声を 20 ms 間隔で届ける
    for index in 0..40i64 {
        let time_us = 1_000_000 + index * 20_000;
        timeline.observe(Track::Audio, time_us, time_us);
    }
    // 目標遅延は分位点の最小 (20 ms) まで下がり、表示の遅れもそこへ追随する
    assert_eq!(timeline.learned_delay_us(Track::Audio), 20_000);
    assert_eq!(
        timeline.presentation_delay_us(Track::Audio),
        Some(20_000),
        "学習した目標遅延がそのまま表示の遅れになる"
    );
}

#[test]
fn sync_raises_the_leading_track_toward_the_late_one() {
    let mut timeline = PlayoutTimeline::new();
    // 音声は 10 ms で届き、映像は同じ TIMESTAMP で 300 ms 遅れて届く
    timeline.observe(Track::Audio, 10_010_000, 1_000_000);
    timeline.observe(Track::Video, 10_310_000, 1_000_000);
    // 2 つのトラックの表示時刻の差は、目標の差に収まる。自然な差は
    // 300 ms (基準の差) − 80 ms (音声の遅れ) = 220 ms であり、そこから
    // TIMELINE_MAX_COMPENSATED_DIFFERENCE_US を引いた 120 ms になる
    let audio_present_us = timeline
        .present_us(Track::Audio, 1_000_000)
        .expect("基準がある");
    let video_present_us = timeline
        .present_us(Track::Video, 1_000_000)
        .expect("基準がある");
    assert_eq!(
        video_present_us - audio_present_us,
        220_000 - TIMELINE_MAX_COMPENSATED_DIFFERENCE_US,
        "相手側へ移す分は上限までにする"
    );
}

#[test]
fn sync_control_runs_on_every_observation() {
    let mut timeline = PlayoutTimeline::new();
    // 映像が 300 ms 遅れている状態を保ちながら、100 ms ごとに観測を足す
    for step in 0..3i64 {
        let wall_us = 10_000_000 + step * 100_000;
        let timestamp_us = 1_000_000 + step * 100_000;
        timeline.observe(Track::Audio, wall_us, timestamp_us);
        timeline.observe(Track::Video, wall_us + 300_000, timestamp_us);
        assert!(
            timeline.presentation_delay_us(Track::Audio).unwrap_or(0) > 80_000,
            "{step} 回目の観測で遅れを上げる"
        );
    }
}

#[test]
fn video_delay_stays_under_the_queue_cap() {
    // 表示待ちのキューを 5 枚にすると、表示の遅れは 1 枚分 (33 ms) までに抑えられる
    let config = TimelineConfig {
        video_queue_limit: 5,
        ..TimelineConfig::default()
    };
    let mut timeline = PlayoutTimeline::with_config(config);
    observe_jittered_video(&mut timeline, 300);
    assert_eq!(
        timeline.learned_delay_us(Track::Video),
        33_333,
        "揺らぎ (40 ms) よりキューの上限 (33 ms) が先に効く"
    );
}

/// 末尾 6 枚だけ揺らす (94 枚は揺らぎ無し、95・96 枚目は 20 ms、97 枚目以降は 40 ms)
fn observe_jitter_tail(timeline: &mut PlayoutTimeline, interval_us: i64) {
    for index in 0..100i64 {
        let jitter_us = match index {
            94 | 95 => 20_000,
            96..=99 => 40_000,
            _ => 0,
        };
        let wall_us = 10_000_000 + index * interval_us + jitter_us;
        let timestamp_us = 1_000_000 + index * interval_us;
        timeline.observe(Track::Video, wall_us, timestamp_us);
    }
}

#[test]
fn video_delay_uses_the_percentile_for_the_frame_rate() {
    // 33 ms 間隔 (30 fps) では百分位 96.7% になり、96 番目の揺らぎ (40 ms) を拾う
    let mut fast = PlayoutTimeline::new();
    observe_jitter_tail(&mut fast, 33_333);
    assert_eq!(fast.learned_delay_us(Track::Video), 40_000);

    // 100 ms 間隔 (10 fps) では百分位 95% になり、94 番目の揺らぎ (20 ms) を拾う
    let mut slow = PlayoutTimeline::new();
    observe_jitter_tail(&mut slow, 100_000);
    assert_eq!(slow.learned_delay_us(Track::Video), 20_000);
}

#[test]
fn video_delay_decays_toward_the_target() {
    let mut timeline = PlayoutTimeline::new();
    observe_jittered_video(&mut timeline, 300);
    assert_eq!(timeline.learned_delay_us(Track::Video), 40_000);

    // 以降は 1 秒間隔で揺らぎの無いフレームだけが届く。古い観測が窓 (10 秒) から
    // 抜けると目標が 0 になり、表示の遅れは毎秒 20 ms ずつ下がる
    let mut wall_us = 10_000_000 + 300 * 33_333;
    let mut timestamp_us = 1_000_000 + 300 * 33_333;
    let mut previous_us = timeline.learned_delay_us(Track::Video);
    for _ in 0..20 {
        wall_us += 1_000_000;
        timestamp_us += 1_000_000;
        timeline.observe(Track::Video, wall_us, timestamp_us);
        let learned_us = timeline.learned_delay_us(Track::Video);
        assert!(
            learned_us <= previous_us,
            "下がり続ける: {previous_us} -> {learned_us}"
        );
        assert!(
            previous_us - learned_us <= 20_000,
            "下がる速さは毎秒 20 ms まで: {previous_us} -> {learned_us}"
        );
        previous_us = learned_us;
    }
    assert_eq!(
        timeline.learned_delay_us(Track::Video),
        0,
        "目標が 0 になったら 0 で止まる"
    );
}

#[test]
fn target_latency_is_shared_by_both_tracks_and_the_cut_is_readable() {
    let config = TimelineConfig {
        target_latency_ms: 300,
        max_presentation_delay_ms: 200,
        ..TimelineConfig::default()
    };
    let mut timeline = PlayoutTimeline::with_config(config);
    timeline.observe(Track::Audio, 10_000_000, 9_000_000);
    timeline.observe(Track::Video, 10_000_000, 9_000_000);
    // 下限 (300 ms) は上限 (200 ms) に収まらないため、切り下げた分が読める
    assert_eq!(timeline.limited_us(), 100_000);
    assert_eq!(timeline.target_latency_ms(), 300);
    // 表示の遅れは 2 つのトラックとも上限で切られる
    assert_eq!(timeline.presentation_delay_us(Track::Audio), Some(200_000));
    assert_eq!(timeline.presentation_delay_us(Track::Video), Some(200_000));
}

#[test]
fn timestamp_jump_starts_a_new_generation() {
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Audio, 10_000_000, 1_000_000);
    let generation = timeline.generation();
    // TIMESTAMP が大きく戻った (配信元の切り替えなど)
    timeline.observe(Track::Audio, 10_000_000, 8_000_000);
    assert_eq!(timeline.generation(), generation + 1);
    // 取り直した後も表示時刻が求まる
    assert!(timeline.present_us(Track::Audio, 8_000_000).is_some());
}

#[test]
fn a_drifting_track_has_no_presentation_time() {
    let mut timeline = PlayoutTimeline::new();
    // 音声は 1 秒の基準、映像は 2 秒の基準 (閾値より大きい)
    timeline.observe(Track::Audio, 10_000_000, 9_000_000);
    timeline.observe(Track::Video, 10_000_000, 8_000_000);
    assert!(!timeline.sharing_bases());
    assert!(timeline.present_us(Track::Audio, 9_000_000).is_some());
    assert!(timeline.present_us(Track::Video, 8_000_000).is_none());
    assert!(timeline.presentation_delay_us(Track::Video).is_none());
}

#[test]
fn skew_is_readable_from_the_presented_records() {
    let mut timeline = PlayoutTimeline::new();
    timeline.record_presentation(Track::Audio, 1_000_000, 5_000_000);
    timeline.record_presentation(Track::Video, 1_000_000, 5_040_000);
    assert_eq!(timeline.skew_us(), Some(40_000), "映像が 40 ms 遅れている");
    // 1 秒より古い実績は使わない
    timeline.record_presentation(Track::Audio, 1_000_000, 4_000_000);
    assert_eq!(timeline.skew_us(), None);
}

#[test]
fn reset_track_keeps_the_other_track() {
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Audio, 10_000_000, 1_000_000);
    timeline.observe(Track::Video, 10_000_000, 1_000_000);
    let generation = timeline.generation();
    timeline.reset_track(Track::Audio);
    assert!(timeline.present_us(Track::Audio, 1_000_000).is_none());
    assert!(timeline.present_us(Track::Video, 1_000_000).is_some());
    assert_eq!(timeline.generation(), generation, "世代は進めない");
}

#[test]
fn only_one_track_does_not_share_the_basis() {
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Audio, 10_000_000, 1_000_000);
    // 観測が片方だけでは、差が 0 であるとも動きが無いとも言えない
    assert!(!timeline.sharing_bases(), "片方だけでは共有しない");
    assert_eq!(timeline.unshared_reason(), UnsharedReason::Unobserved);
    let breakdown = timeline.delay_breakdown();
    assert_eq!(breakdown.video.base_delay_us, None);
    assert_eq!(breakdown.video.jitter_delay_us, None);
    assert_eq!(breakdown.video.presentation_delay_us, None);
    assert_eq!(breakdown.base_difference_us, None);
    assert_eq!(breakdown.audio.base_delay_us, Some(9_000_000));
    assert_eq!(breakdown.audio.jitter_delay_us, Some(80_000));
    assert_eq!(breakdown.base_drift_limit_us, TIMELINE_BASE_DRIFT_US);
}

#[test]
fn delay_breakdown_splits_the_basis_the_jitter_and_the_sync() {
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Audio, 10_000_000, 9_000_000);
    timeline.observe(Track::Video, 10_300_000, 9_000_000);
    let breakdown = timeline.delay_breakdown();
    assert_eq!(breakdown.audio.base_delay_us, Some(1_000_000));
    assert_eq!(breakdown.video.base_delay_us, Some(1_300_000));
    assert_eq!(breakdown.audio.jitter_delay_us, Some(80_000));
    assert_eq!(breakdown.base_difference_us, Some(-300_000));
    assert!(breakdown.sharing_bases);
    assert_eq!(breakdown.unshared_reason, UnsharedReason::None);
    // 表示の遅れは「基準の遅れ + jitter buffer の遅れ + 同期で足した分」である
    assert_eq!(breakdown.audio.sync_extra_delay_us, 100_000);
    assert_eq!(breakdown.audio.presentation_delay_us, Some(1_180_000));
    assert_eq!(breakdown.video.sync_extra_delay_us, 0);
    assert_eq!(breakdown.video.presentation_delay_us, Some(1_300_000));
    // 表示時刻は内訳と同じ値になる
    assert_eq!(
        timeline.present_us(Track::Audio, 9_000_000),
        Some(10_180_000)
    );
    assert_eq!(
        timeline.present_us(Track::Video, 9_000_000),
        Some(10_300_000)
    );
}

#[test]
fn a_moving_basis_is_treated_as_a_clock_drift() {
    let mut timeline = PlayoutTimeline::new();
    // 音声の基準は 1 秒で一定、映像の基準は 1.05 秒から毎秒 40 ms ずつ遅れていく。
    // 差の大きさ (最大 250 ms) は閾値 (表示の遅れの上限から求まる値) に届かないため、
    // 差の動き (5 秒で 200 ms) で検出できないと共有したままになる
    let mut wall_us = 10_000_000;
    for step in 0..60i64 {
        wall_us += 100_000;
        let video_offset_us = 1_050_000 + step * 4_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - video_offset_us);
    }
    assert_eq!(
        timeline.unshared_reason(),
        UnsharedReason::Drift,
        "差の動きで時計のずれを見つける"
    );
    assert!(!timeline.sharing_bases());
    // 遅れて届いている側 (映像) の表示時刻は返らない
    let timestamp_us = wall_us - 1_050_000;
    assert!(timeline.present_us(Track::Video, timestamp_us).is_none());
    assert!(timeline.present_us(Track::Audio, timestamp_us).is_some());
    assert!(
        timeline
            .delay_breakdown()
            .base_drift_us_per_second
            .is_some()
    );

    // ドリフトが止まっても、保持の間 (30 秒) は共有に戻さない
    for _ in 0..60 {
        wall_us += 100_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - 1_260_000);
    }
    assert_eq!(
        timeline.unshared_reason(),
        UnsharedReason::Hold,
        "一度やめた判定は保持する"
    );
}

#[test]
fn video_follows_the_audio_arrival_delay_when_the_audio_drifts() {
    let mut timeline = PlayoutTimeline::new();
    // 音声の基準が 2 秒、映像が 1 秒。音声の TIMESTAMP は信用できないとみなす
    timeline.observe(Track::Audio, 10_000_000, 8_000_000);
    timeline.observe(Track::Video, 10_000_000, 9_000_000);
    assert_eq!(timeline.unshared_reason(), UnsharedReason::Difference);
    assert!(
        timeline.presentation_delay_us(Track::Audio).is_none(),
        "ずれている側の表示時刻は決めない"
    );
    // 映像は音声の到着基準の遅れ (80 ms) から不感帯 (30 ms) を引いた 50 ms へ上げる
    assert_eq!(timeline.presentation_delay_us(Track::Video), Some(50_000));
    assert_eq!(timeline.audio_arrival_delay_us(), Some(80_000));
}

#[test]
fn audio_arrival_delay_is_clamped_between_the_floor_and_the_cap() {
    assert_eq!(audio_arrival_delay_us(0), TIMELINE_AUDIO_DELAY_FLOOR_US);
    assert_eq!(audio_arrival_delay_us(90_000), 90_000);
    assert_eq!(audio_arrival_delay_us(1_000_000), TIMELINE_ARRIVAL_DELAY_US);
    // 観測が無ければ読めない
    let timeline = PlayoutTimeline::new();
    assert_eq!(timeline.audio_arrival_delay_us(), None);
    assert_eq!(timeline.playout_delay_us(), None);
}

#[test]
fn observe_audio_playout_raises_the_audio_delay_to_the_closed_loop_target() {
    // 鳴り遅れの観測を渡すと、揺らぎの学習値だけでは足りない分を閉ループが補い、
    // 音声の表示の遅れがその場で上がる
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Audio, 10_000_000, 1_000_000);
    assert_eq!(
        timeline.presentation_delay_us(Track::Audio),
        Some(AUDIO_DELAY_START_MS * 1_000),
        "まずは揺らぎの学習値を使う"
    );

    // 予定を 60 ms 過ぎて鳴った。1 回の増分の上限 (40 ms) まで増える
    timeline.observe_audio_playout(feedback_observation(10_020_000, Some(60_000)));
    let raised_us = AUDIO_DELAY_FEEDBACK_START_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US;
    assert_eq!(
        timeline.learned_delay_us(Track::Audio),
        raised_us,
        "閉ループの目標へ上がる"
    );
    assert_eq!(
        timeline.presentation_delay_us(Track::Audio),
        Some(raised_us),
        "表示の遅れもその場で上がる"
    );
    // 表示時刻 = TIMESTAMP + 基準の遅れ + 表示の遅れ
    assert_eq!(
        timeline.present_us(Track::Audio, 1_000_000),
        Some(10_000_000 + raised_us),
        "鳴らす時刻も新しい目標で求める"
    );
    assert_eq!(
        timeline.delay_breakdown().audio_delay_feedback.reason,
        AudioDelayFeedbackReason::Lateness,
        "閉ループの理由が遅れである"
    );
}

#[test]
fn target_latency_caps_the_closed_loop_target() {
    // 明示された `targetLatency` は、閉ループが自動で超えない上限になる
    let mut timeline = PlayoutTimeline::new();
    timeline.set_target_latency_ms(120);
    timeline.observe(Track::Audio, 10_000_000, 1_000_000);

    // 遅れが大きくても、閉ループの目標は上限で止まる
    timeline.observe_audio_playout(feedback_observation(10_020_000, Some(400_000)));
    assert_eq!(
        timeline.learned_delay_us(Track::Audio),
        120_000,
        "上限で止まる"
    );
    assert_eq!(timeline.presentation_delay_us(Track::Audio), Some(120_000));

    // 遅れが続いても上限を超えない
    timeline.observe_audio_playout(feedback_observation(11_020_000, Some(400_000)));
    assert_eq!(
        timeline.learned_delay_us(Track::Audio),
        120_000,
        "何度観測しても上限を超えない"
    );
    let breakdown = timeline.delay_breakdown();
    assert_eq!(
        breakdown.audio_delay_feedback.target_us, 120_000,
        "内訳に閉ループの目標が出る"
    );
    assert_eq!(
        breakdown.audio_delay_feedback.ceiling_us,
        Some(120_000),
        "内訳に明示の上限が出る"
    );

    // 0 を渡すと「下限にしない」の意味であり、上限が外れる
    timeline.set_target_latency_ms(0);
    assert_eq!(
        timeline.delay_breakdown().audio_delay_feedback.ceiling_us,
        None,
        "0 で上限を外す"
    );
    timeline.observe_audio_playout(feedback_observation(12_020_000, Some(400_000)));
    assert!(
        timeline.learned_delay_us(Track::Audio) > 120_000,
        "上限が無ければ自動で増える: {}",
        timeline.learned_delay_us(Track::Audio)
    );
}

#[test]
fn target_latency_does_not_cap_the_learned_jitter_target() {
    // 明示の上限は閉ループの目標にだけ掛ける。揺らぎの学習値は既存の揺らぎの吸収そのもの
    // であり、切り下げると挙動が変わるため掛けない
    let mut timeline = PlayoutTimeline::new();
    // 学習の初期値 (80 ms) より小さい上限にする
    timeline.set_target_latency_ms(50);
    timeline.observe(Track::Audio, 10_000_000, 1_000_000);
    timeline.observe_audio_playout(feedback_observation(10_020_000, Some(60_000)));

    assert_eq!(
        timeline.delay_breakdown().audio_delay_feedback.target_us,
        50_000,
        "閉ループの目標は上限で止まる"
    );
    assert_eq!(
        timeline.learned_delay_us(Track::Audio),
        AUDIO_DELAY_START_MS * 1_000,
        "学習値は上限で切り下げない"
    );
    assert_eq!(
        timeline.presentation_delay_us(Track::Audio),
        Some(AUDIO_DELAY_START_MS * 1_000),
        "表示の遅れは学習値のままになる"
    );
}

#[test]
fn reset_track_keeps_the_closed_loop_target() {
    // 購読のやり直しでは、jitter buffer の学習だけを消し、閉ループの目標は戻さない
    // (戻すと、その間だけ遅れが戻る)
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Audio, 10_000_000, 1_000_000);
    timeline.observe_audio_playout(feedback_observation(10_020_000, Some(60_000)));
    let raised_us = AUDIO_DELAY_FEEDBACK_START_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US;
    assert_eq!(timeline.learned_delay_us(Track::Audio), raised_us);

    timeline.reset_track(Track::Audio);
    assert!(
        timeline.present_us(Track::Audio, 1_000_000).is_none(),
        "基準は消える"
    );
    let breakdown = timeline.delay_breakdown();
    assert_eq!(
        breakdown.audio_delay_feedback.target_us, raised_us,
        "閉ループの目標は残る"
    );
    assert_eq!(
        breakdown.audio_delay_feedback.reason,
        AudioDelayFeedbackReason::Lateness,
        "理由も残る"
    );
    assert_eq!(
        breakdown.audio_delay_feedback.adjustments, 1,
        "動かした回数も残る"
    );

    // 観測し直しても、揺らぎの学習の初期値 (80 ms) ではなく閉ループの目標を使う
    timeline.observe(Track::Audio, 20_000_000, 11_000_000);
    assert_eq!(
        timeline.learned_delay_us(Track::Audio),
        raised_us,
        "購読のやり直しで目標を戻さない"
    );

    // すべてリセットしても同じである
    timeline.reset();
    assert_eq!(
        timeline.delay_breakdown().audio_delay_feedback.target_us,
        raised_us,
        "全リセットでも閉ループの目標は残る"
    );
}

#[test]
fn delay_breakdown_reports_the_closed_loop_state() {
    // 遅延の内訳に、閉ループがどう動いたかが出る
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Audio, 10_000_000, 1_000_000);
    let before = timeline.delay_breakdown().audio_delay_feedback;
    assert_eq!(before.target_us, AUDIO_DELAY_FEEDBACK_START_US);
    assert_eq!(before.reason, AudioDelayFeedbackReason::Initial);
    assert_eq!(before.adjustments, 0, "まだ動かしていない");
    assert_eq!(before.ceiling_us, None, "明示の上限は無い");
    assert_eq!(before.lateness_p50_us, None, "観測が無い");

    timeline.observe_audio_playout(AudioDelayFeedbackObservation {
        at_us: 10_020_000,
        lateness_us: Some(TimingSummary {
            p50: 60_000,
            p95: 80_000,
            max: 90_000,
        }),
        start_delay_us: Some(TimingSummary {
            p50: 120_000,
            p95: 130_000,
            max: 140_000,
        }),
        slack_us: Some(TimingSummary {
            p50: 20_000,
            p95: 25_000,
            max: 30_000,
        }),
        backlog_misses: 0,
        backlog_us: 0,
    });
    let after = timeline.delay_breakdown().audio_delay_feedback;
    assert_eq!(
        after.target_us,
        AUDIO_DELAY_FEEDBACK_START_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "閉ループの目標が出る"
    );
    assert_eq!(after.reason, AudioDelayFeedbackReason::Lateness);
    assert_eq!(
        after.last_change_us, AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "直前の増分が出る"
    );
    assert_eq!(after.adjustments, 1, "動かした回数が出る");
    assert_eq!(after.lateness_p50_us, Some(60_000), "遅れの p50 が出る");
    assert_eq!(
        after.start_delay_p50_us,
        Some(120_000),
        "到着から鳴るまでの p50 が出る"
    );
    assert_eq!(after.slack_p50_us, Some(20_000), "余裕の p50 が出る");
}

#[test]
fn closed_loop_accepts_the_observation_from_the_timing_stats() {
    // 計器 (`AudioPlayoutTimingStats`) が作る観測をそのまま渡しても閉ループが動くこと
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Audio, 10_000_000, 1_000_000);
    assert_eq!(
        timeline.learned_delay_us(Track::Audio),
        AUDIO_DELAY_START_MS * 1_000,
        "まずは揺らぎの学習値を使う"
    );

    let mut stats = AudioPlayoutTimingStats::new();
    stats.record_play(AudioPlayoutPlay {
        arrival_us: 10_020_000,
        target_start_us: Some(10_000_000),
        start_at_us: 10_080_000,
        played_us: 20_000,
        basis: AudioPlayoutBasis::Timestamp,
    });
    // 予定 (10_000_000) を 80 ms 過ぎて鳴ったため、1 回の上限まで増える
    timeline.observe_audio_playout(stats.audio_delay_feedback(10_100_000));
    assert_eq!(
        timeline.learned_delay_us(Track::Audio),
        AUDIO_DELAY_FEEDBACK_START_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "計器の観測でも閉ループが動く"
    );
}
