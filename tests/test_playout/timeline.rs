//! 音声と映像の共通の時間軸のテスト
//!
//! 公開 API (`PlayoutTimeline`) の契約を確認する。

use shiguredo_moqt::playout::delay::AUDIO_DELAY_START_MS;
use shiguredo_moqt::playout::feedback::{
    AUDIO_DELAY_FEEDBACK_DECREASE_US_PER_SECOND, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
    AUDIO_DELAY_FEEDBACK_START_US, AUDIO_DELAY_FEEDBACK_TOLERANCE_US, AudioDelayFeedbackReason,
};
use shiguredo_moqt::playout::scheduler::{AudioPlayoutBasis, AudioPlayoutPlay};
use shiguredo_moqt::playout::timeline::{
    PlayoutTimeline, TIMELINE_ARRIVAL_DELAY_US, TIMELINE_AUDIO_DELAY_FLOOR_US,
    TIMELINE_BASE_DRIFT_US, TIMELINE_MAX_COMPENSATED_DIFFERENCE_US, TIMELINE_SYNC_MIN_DELTA_US,
    TimelineConfig, Track, UnsharedReason, audio_arrival_delay_us,
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
        compression_us: 0,
        jitter_target_us: 0,
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
    // まず 11 秒ぶん、差の無い観測を流して水準を決める。判定は落ち着いた水準を観測してから
    // 始める (購読の直後は relay の cache から届いた分をまとめて復号しており、基準が
    // 数百 ms から数秒動く)
    let mut wall_us = 10_000_000;
    for _ in 0..110 {
        wall_us += 100_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - 1_050_000);
    }

    // 映像の基準が毎秒 40 ms ずつ遅れていく。差の大きさ (最大 450 ms) は閾値 (表示の遅れの
    // 上限から求まる値) に届かないため、差の動き (6 秒で 240 ms) で検出できないと共有した
    // ままになる。持続の確認 (6 秒) を超える長さを見る
    for step in 0..100i64 {
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
}

#[test]
fn the_unshared_hold_is_released_when_the_difference_returns() {
    // 保持 (30 秒) は閾値が動くたびに共有と解除を往復しないために置く。解除のきっかけが
    // 去って差が水準へ戻った後は、同じ水準へ留まる理由が無いため、保持を待たずに戻す
    let mut timeline = PlayoutTimeline::new();
    let mut wall_us = 10_000_000;
    for _ in 0..110 {
        wall_us += 100_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - 1_050_000);
    }
    for step in 0..100i64 {
        wall_us += 100_000;
        let video_offset_us = 1_050_000 + step * 4_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - video_offset_us);
    }
    assert_eq!(timeline.unshared_reason(), UnsharedReason::Drift);

    // ドリフトが止まり、差が元の水準 (50 ms) へ戻る
    for _ in 0..30 {
        wall_us += 100_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - 1_050_000);
    }
    assert_eq!(
        timeline.unshared_reason(),
        UnsharedReason::None,
        "きっかけが去って差が戻ったら保持を待たずに戻す"
    );
    assert!(timeline.sharing_bases(), "共有へ戻ること");
}

/// 一過性の動きでは共有をやめない
///
/// 読み出しが一瞬遅れただけでも、直近の窓の最小値の差は動く。しかし離れ幅が段差とみなす
/// 大きさに届かず、参照 (表示時刻が使う基準の差) も動いていなければ、経路と復号の乱れで
/// ある。持続の確認の前に戻るため、解除しない。
#[test]
fn a_transient_move_does_not_unshare_the_bases() {
    let mut timeline = PlayoutTimeline::new();
    let mut wall_us = 10_000_000;
    for _ in 0..110 {
        wall_us += 100_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - 1_050_000);
    }

    // 映像の基準が一瞬 150 ms 遅れる (段差とみなす 200 ms には届かない)。直近の窓 (2 秒) が
    // 入れ替わる長さだけ続ける
    for _ in 0..20 {
        wall_us += 100_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - 1_200_000);
    }
    for _ in 0..40 {
        wall_us += 100_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - 1_050_000);
        assert_ne!(
            timeline.unshared_reason(),
            UnsharedReason::Drift,
            "一過性の動きを時計のずれとみなさない"
        );
    }
    assert_eq!(
        timeline.unshared_reason(),
        UnsharedReason::None,
        "一過性の動きでは共有をやめない"
    );
    assert!(timeline.sharing_bases(), "共有したままであること");
}

/// 1 回で大きく離れた段差は、持続を待たずに共有をやめる
#[test]
fn a_step_away_from_the_level_unshares_immediately() {
    let mut timeline = PlayoutTimeline::new();
    let mut wall_us = 10_000_000;
    for _ in 0..110 {
        wall_us += 100_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - 1_050_000);
    }

    // 映像の基準が 1 回で 500 ms 遅れる (離れ幅が段差とみなす 200 ms を超える)。直近の窓
    // (2 秒) が入れ替わって初めて差に現れる
    let mut seen_drift = false;
    for _ in 0..30 {
        wall_us += 100_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - 1_550_000);
        if timeline.unshared_reason() == UnsharedReason::Drift {
            seen_drift = true;
        }
    }
    assert!(seen_drift, "段差は持続を待たずに見つける");
    assert_eq!(
        timeline.unshared_reason(),
        UnsharedReason::Hold,
        "解除した後は保持の間は戻さない"
    );
}

/// 閾値だけが動いた解除は、差が戻っても保持を待たずには戻さない
///
/// 差が同じ水準のまま、jitter buffer の目標遅延が動いて閾値が下がると解除される。この
/// 解除は動きがきっかけではないため、閾値が戻っても差は元の水準の側にある。往復を防ぐ
/// ために保持する。
#[test]
fn a_threshold_only_unshare_is_held() {
    let mut timeline = PlayoutTimeline::new();
    let mut wall_us = 10_000_000;
    // 映像が 200 ms 遅い状態で水準を決める (差は 250 ms で一定)
    for _ in 0..110 {
        wall_us += 100_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - 1_250_000);
    }
    assert!(
        timeline.sharing_bases(),
        "差が閾値の中なら共有したままであること"
    );

    // 明示の `targetLatency` を上げると、基準の差の閾値が下がって差が超える。差は動いて
    // いないため、解除のきっかけに動きは無い
    timeline.set_target_latency_ms(400);
    wall_us += 100_000;
    timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
    timeline.observe(Track::Video, wall_us, wall_us - 1_250_000);
    assert_eq!(
        timeline.unshared_reason(),
        UnsharedReason::Difference,
        "動かない差でも、閾値を超えたら解除する"
    );

    // 閾値を戻しても、差が戻った状態が続くだけでは保持を待たずには戻さない
    timeline.set_target_latency_ms(0);
    for _ in 0..30 {
        wall_us += 100_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - 1_250_000);
    }
    assert_eq!(
        timeline.unshared_reason(),
        UnsharedReason::Hold,
        "動きがきっかけでない解除は保持する"
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
    // 映像は音声の到着基準の遅れ (80 ms) から不感帯 (5 ms) を引いた 75 ms へ上げる
    assert_eq!(timeline.presentation_delay_us(Track::Video), Some(75_000));
    assert_eq!(timeline.audio_arrival_delay_us(), Some(80_000));
}

/// A/V 同期の不感帯の境界で、ちょうどは動かさず、超えた分だけ合わせること
///
/// 不感帯の中では遅延を変えないため、2 つのトラックは最大この値だけ離れたままになる。
/// 超えた分は先行する側へ足して不感帯へ収める。
#[test]
fn sync_deadband_boundary_is_exact() {
    assert_eq!(
        TIMELINE_SYNC_MIN_DELTA_US, 5_000,
        "A/V 同期の不感帯は 5 ms であること"
    );
    // 音声の素の表示の遅れは 80 ms (揺らぎの学習の初期値)、映像は 0 ms である。映像の基準を
    // 音声より 85 ms 遅らせると、表示時刻の差はちょうど不感帯 (5 ms) になる
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Audio, 10_000_000, 1_000_000);
    timeline.observe(Track::Video, 10_085_000, 1_000_000);
    assert_eq!(
        timeline.presentation_delay_us(Track::Audio),
        Some(80_000),
        "不感帯ちょうどの差では音声を遅らせないこと"
    );
    assert_eq!(
        timeline.presentation_delay_us(Track::Video),
        Some(0),
        "不感帯ちょうどの差では映像を動かさないこと"
    );

    // 差を 1 µs だけ超えさせると、先行する音声へ超えた分 (1 µs) を足して不感帯へ収める
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Audio, 10_000_000, 1_000_000);
    timeline.observe(Track::Video, 10_085_001, 1_000_000);
    assert_eq!(
        timeline.presentation_delay_us(Track::Audio),
        Some(80_001),
        "不感帯を超えた分だけ音声を遅らせること"
    );
    assert_eq!(
        timeline.presentation_delay_us(Track::Video),
        Some(0),
        "映像は動かさないこと"
    );
    let audio_present_us = timeline
        .present_us(Track::Audio, 1_000_000)
        .expect("基準がある");
    let video_present_us = timeline
        .present_us(Track::Video, 1_000_000)
        .expect("基準がある");
    assert_eq!(
        video_present_us - audio_present_us,
        TIMELINE_SYNC_MIN_DELTA_US,
        "合わせた後の差は不感帯ちょうどになること"
    );
}

#[test]
fn audio_arrival_delay_is_clamped_between_the_floor_and_the_cap() {
    // 下限は 50 ms である。到着基準の遅れと、映像を合わせる先の両方がこの値を使うため、
    // 値そのものを固定する
    assert_eq!(
        TIMELINE_AUDIO_DELAY_FLOOR_US, 50_000,
        "音声の再生の遅れの下限は 50 ms であること"
    );
    // 下限ちょうどは切らない
    assert_eq!(
        audio_arrival_delay_us(TIMELINE_AUDIO_DELAY_FLOOR_US),
        TIMELINE_AUDIO_DELAY_FLOOR_US,
        "下限ちょうどの遅れはそのまま使うこと"
    );
    // 下限の 1 µs 手前は下限まで上げる
    assert_eq!(
        audio_arrival_delay_us(TIMELINE_AUDIO_DELAY_FLOOR_US - 1),
        TIMELINE_AUDIO_DELAY_FLOOR_US,
        "下限を下回る遅れは下限まで上げること"
    );
    assert_eq!(audio_arrival_delay_us(0), TIMELINE_AUDIO_DELAY_FLOOR_US);
    assert_eq!(audio_arrival_delay_us(90_000), 90_000);
    assert_eq!(audio_arrival_delay_us(1_000_000), TIMELINE_ARRIVAL_DELAY_US);
    // 上限ちょうどは切らない
    assert_eq!(
        audio_arrival_delay_us(TIMELINE_ARRIVAL_DELAY_US),
        TIMELINE_ARRIVAL_DELAY_US,
        "上限ちょうどの遅れはそのまま使うこと"
    );
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

    // 予定を 60 ms 過ぎて鳴った。閉ループの目標は 1 回の増分の上限 (40 ms) まで増えるが、
    // 実際に使う値は上限 (100 ms) で切られる
    timeline.observe_audio_playout(feedback_observation(10_020_000, Some(60_000)));
    let raised_us = AUDIO_DELAY_FEEDBACK_MAX_TARGET_US;
    assert_eq!(
        timeline.learned_delay_us(Track::Audio),
        raised_us,
        "閉ループの目標へ上がる (実際に使う値の上限まで)"
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
    // 明示された `targetLatency` は、閉ループが自動で超えない上限になる。実際に使う値の
    // 上限 (100 ms) より厳しい値を渡し、こちらが効くことを確かめる
    let mut timeline = PlayoutTimeline::new();
    timeline.set_target_latency_ms(90);
    timeline.observe(Track::Audio, 10_000_000, 1_000_000);

    // 遅れが大きくても、閉ループの目標は上限で止まる
    timeline.observe_audio_playout(feedback_observation(10_020_000, Some(400_000)));
    assert_eq!(
        timeline.learned_delay_us(Track::Audio),
        90_000,
        "上限で止まる"
    );
    assert_eq!(timeline.presentation_delay_us(Track::Audio), Some(90_000));

    // 遅れが続いても上限を超えない
    timeline.observe_audio_playout(feedback_observation(11_020_000, Some(400_000)));
    assert_eq!(
        timeline.learned_delay_us(Track::Audio),
        90_000,
        "何度観測しても上限を超えない"
    );
    let breakdown = timeline.delay_breakdown();
    assert_eq!(
        breakdown.audio_delay_feedback.target_us, 90_000,
        "内訳に閉ループの目標が出る"
    );
    assert_eq!(
        breakdown.audio_delay_feedback.ceiling_us,
        Some(90_000),
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
    // 明示の上限が外れると閉ループの内部値は増え続ける
    assert!(
        timeline.delay_breakdown().audio_delay_feedback.target_us > 90_000,
        "上限が無ければ自動で増える: {}",
        timeline.delay_breakdown().audio_delay_feedback.target_us
    );
    // 実際に使う値は、明示の上限とは別の上限 (100 ms) で止まる
    assert_eq!(
        timeline.learned_delay_us(Track::Audio),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "実際に使う値は上限で止まる"
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
    // 収束 (遅れが許容の中) の観測を 2 つ渡し、目標を 1 回だけ下げる。適用する上限が初期値と
    // 同じ 100 ms であるため、遅れでは増やせない (増やす余地は、圧縮が続いて上限を広げた
    // ときにだけ生まれる)
    timeline.observe_audio_playout(feedback_observation(
        10_020_000,
        Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
    ));
    timeline.observe_audio_playout(feedback_observation(
        11_020_000,
        Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
    ));
    let lowered_us =
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US - AUDIO_DELAY_FEEDBACK_DECREASE_US_PER_SECOND;
    assert_eq!(timeline.learned_delay_us(Track::Audio), lowered_us);

    timeline.reset_track(Track::Audio);
    assert!(
        timeline.present_us(Track::Audio, 1_000_000).is_none(),
        "基準は消える"
    );
    let breakdown = timeline.delay_breakdown();
    assert_eq!(
        breakdown.audio_delay_feedback.target_us, lowered_us,
        "閉ループの目標 (内部値) は残る"
    );
    assert_eq!(
        breakdown.audio_delay_feedback.reason,
        AudioDelayFeedbackReason::Settled,
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
        lowered_us,
        "購読のやり直しで目標を戻さない"
    );

    // すべてリセットしても同じである
    timeline.reset();
    assert_eq!(
        timeline.delay_breakdown().audio_delay_feedback.target_us,
        lowered_us,
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
        compression_us: 0,
        jitter_target_us: 0,
    });
    let after = timeline.delay_breakdown().audio_delay_feedback;
    // 閉ループの内部の目標も、実際に適用する上限 (100 ms) で切る。初期値が既にその上限で
    // あるため、遅れが続いても増えない。適用されない値へ育てると、遅れが収まってから
    // 減らし切る (毎秒 10 ms) までの時間だけ復帰が遅れる
    assert_eq!(
        after.target_us, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "閉ループの目標は適用する上限で止まる"
    );
    assert_eq!(after.reason, AudioDelayFeedbackReason::Lateness);
    assert_eq!(
        after.last_change_us, 0,
        "既に適用する上限にあるため増分は出ない"
    );
    assert_eq!(after.adjustments, 0, "既に適用する上限にあるため動かさない");
    assert_eq!(
        after.lateness_p50_us,
        Some(40_000),
        "遅れの p50 が出る (不感帯の 20 ms を引いた値)"
    );
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
    // 予定を 80 ms 過ぎて鳴ったため、1 回の上限まで増える。実際に使う値は上限 (100 ms)
    // で切られる
    timeline.observe_audio_playout(stats.audio_delay_feedback(10_100_000));
    assert_eq!(
        timeline.learned_delay_us(Track::Audio),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "計器の観測でも閉ループが動く"
    );
}
