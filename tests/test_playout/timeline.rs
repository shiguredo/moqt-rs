//! 音声と映像の共通の時間軸のテスト
//!
//! 公開 API (`PlayoutTimeline`) の契約を確認する。

use shiguredo_moqt::playout::delay::AUDIO_DELAY_START_MS;
use shiguredo_moqt::playout::feedback::{
    AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
    AUDIO_DELAY_FEEDBACK_START_US, AudioDelayFeedbackReason,
};
use shiguredo_moqt::playout::scheduler::{AudioPlayoutBasis, AudioPlayoutPlay};
use shiguredo_moqt::playout::timeline::{
    PlayoutTimeline, TIMELINE_ARRIVAL_DELAY_US, TIMELINE_AUDIO_DELAY_FLOOR_US,
    TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_FLOOR_US, TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_MAX_US,
    TIMELINE_BASE_DRIFT_US, TIMELINE_MAX_COMPENSATED_DIFFERENCE_US, TIMELINE_SYNC_MIN_DELTA_US,
    TimelineConfig, Track, UnsharedReason, audio_arrival_delay_us, audio_only_arrival_delay_us,
    sync_compensation_limit_us,
};
use shiguredo_moqt::playout::timing::{
    AudioDelayFeedbackObservation, AudioPlayoutTimingStats, TimingSummary,
};

/// 閉ループへ渡す観測を 1 つ作る
///
/// `lateness_p50_us` が None のときは「まだ鳴らしていない」観測になる。鳴り始めまでの
/// 時間と余裕は、JS のテスト (`timeline.test.ts`) が使う 50 ms に揃える。
fn feedback_observation(at_us: i64, lateness_p50_us: Option<i64>) -> AudioDelayFeedbackObservation {
    AudioDelayFeedbackObservation {
        at_us,
        lateness_us: lateness_p50_us.map(|p50| TimingSummary {
            p50,
            p95: p50,
            max: p50,
        }),
        start_delay_us: Some(TimingSummary {
            p50: 50_000,
            p95: 50_000,
            max: 50_000,
        }),
        slack_us: Some(TimingSummary {
            p50: 50_000,
            p95: 50_000,
            max: 50_000,
        }),
        backlog_misses: 0,
        backlog_us: 0,
        underrun_ms: 0,
        compression_us: 0,
        jitter_target_us: 0,
        // 穴と、音声だけの購読の到着基準の下限は、このヘルパを使うテストでは使わない
        gap_count: 0,
        gap_us: 0,
        arrival_delay_floor_us: None,
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
    // 2 つのトラックの表示時刻の差は、補償上限までで決まる。自然な差は
    // 300 ms (基準の差) − 80 ms (音声の遅れ) = 220 ms であり、そこから
    // TIMELINE_MAX_COMPENSATED_DIFFERENCE_US (補償上限) を引いた 120 ms になる
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

/// 閾値だけが動いた解除も、差が戻れば 2 秒で共有へ戻ること
///
/// 差が同じ水準のまま、jitter buffer の目標遅延が動いて閾値が下がると解除される。解除の
/// きっかけが動きだったかは見ない。差が水準へ戻り、直近に往復していなければ、同じ水準へ
/// 留まる理由が無いため保持を待たずに戻す。
#[test]
fn a_threshold_only_unshare_is_released_when_the_difference_returns() {
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

    // 閾値を戻すと、差が戻った状態が 2 秒続いた時点で保持を待たずに戻る
    timeline.set_target_latency_ms(0);
    wall_us += 100_000;
    timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
    timeline.observe(Track::Video, wall_us, wall_us - 1_250_000);
    assert_eq!(
        timeline.unshared_reason(),
        UnsharedReason::Hold,
        "戻った状態が続くまでは保持すること"
    );
    for _ in 0..30 {
        wall_us += 100_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - 1_250_000);
    }
    assert_eq!(
        timeline.unshared_reason(),
        UnsharedReason::None,
        "原因が去って差が戻ったら保持を待たずに戻す"
    );
    assert!(timeline.sharing_bases(), "共有へ戻ること");
}

/// 30 秒以内に再び解除したら、差が戻っても保持を待つこと
///
/// 往復のたびに、同期が足した分を戻して (間に合わないフレームを捨てる) すぐ足し直す
/// (表示が待って止まる) ことになる。2 回の解除が保持の時間の内側で起きたら、閾値が動き
/// 続けているとみなして落ち着くまで解かない。
#[test]
fn a_repeated_unshare_within_the_hold_is_kept() {
    let mut timeline = PlayoutTimeline::new();
    let mut wall_us = 10_000_000;
    for _ in 0..110 {
        wall_us += 100_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - 1_250_000);
    }

    // 1 回目の解除。きっかけは閾値だけで、差は動いていない
    timeline.set_target_latency_ms(400);
    wall_us += 100_000;
    timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
    timeline.observe(Track::Video, wall_us, wall_us - 1_250_000);
    assert_eq!(timeline.unshared_reason(), UnsharedReason::Difference);

    // 閾値を戻すと、原因が去って差が戻るため 2 秒で保持が解ける (1 回目の解除の時刻は残る)
    timeline.set_target_latency_ms(0);
    for _ in 0..30 {
        wall_us += 100_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - 1_250_000);
    }
    assert_eq!(timeline.unshared_reason(), UnsharedReason::None);

    // 30 秒の内側で 2 回目の解除をする
    timeline.set_target_latency_ms(400);
    wall_us += 100_000;
    timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
    timeline.observe(Track::Video, wall_us, wall_us - 1_250_000);
    assert_eq!(timeline.unshared_reason(), UnsharedReason::Difference);

    // 閾値を戻しても、直近に往復しているため保持を待たずには戻さない
    timeline.set_target_latency_ms(0);
    for _ in 0..30 {
        wall_us += 100_000;
        timeline.observe(Track::Audio, wall_us, wall_us - 1_000_000);
        timeline.observe(Track::Video, wall_us, wall_us - 1_250_000);
    }
    assert_eq!(
        timeline.unshared_reason(),
        UnsharedReason::Hold,
        "30 秒以内の再解除は保持すること"
    );
    assert!(!timeline.sharing_bases());
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

/// 音声だけの購読の到着基準の遅れが、下限と上限の間に切られること
///
/// 下限 (`floor_us`) を必ず確保し、その上で学習した値を上限
/// ([`TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_MAX_US`]) まで使う。式の順序は JS と同じで、
/// `min` してから `max` するため、下限が上限より大きいときは下限が勝つ。
#[test]
fn audio_only_arrival_delay_is_clamped_between_the_floor_and_the_max() {
    // 学習した遅れが下限を下回るときは下限を使う
    assert_eq!(
        audio_only_arrival_delay_us(0, TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_FLOOR_US),
        TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_FLOOR_US,
        "学習した遅れが 0 でも下限を確保すること"
    );
    assert_eq!(
        audio_only_arrival_delay_us(
            TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_FLOOR_US - 1,
            TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_FLOOR_US
        ),
        TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_FLOOR_US,
        "下限を 1 マイクロ秒下回る遅れは下限まで上げること"
    );
    // 上限ちょうどは切らない
    assert_eq!(
        audio_only_arrival_delay_us(
            TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_MAX_US,
            TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_FLOOR_US
        ),
        TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_MAX_US,
        "上限ちょうどの遅れはそのまま使うこと"
    );
    // 上限を超える分は使わない
    assert_eq!(
        audio_only_arrival_delay_us(
            TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_MAX_US + 1,
            TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_FLOOR_US
        ),
        TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_MAX_US,
        "上限を超える学習値は上限まで下げること"
    );
    // 下限が上限より大きいときは下限を優先する (JS の Math.max(floor, Math.min(learned, max)))
    assert_eq!(
        audio_only_arrival_delay_us(0, TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_MAX_US + 1),
        TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_MAX_US + 1,
        "下限が上限より大きいときは下限を優先すること"
    );
    // 共有の時間軸用のクランプ (50〜100 ms) とは別の関数である。下限 0 なら学習値をそのまま使う
    assert_eq!(
        audio_only_arrival_delay_us(900_000, 0),
        TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_MAX_US,
        "下限が無ければ学習値は上限で切ること"
    );
}

/// 音声だけの購読の定数が閉ループの上限と一致すること
///
/// 上限 (160 ms) は、閉ループが穴か圧縮で広げる適用上限と一致させる。値がずれると、
/// 閉ループが広げた目標を到着基準の遅れへ使えなくなる。循環参照を避けて数値で持っている
/// ため、一致はテストで固定する。
#[test]
fn audio_only_arrival_delay_constants_match_the_feedback_limits() {
    assert_eq!(
        TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_MAX_US, AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "音声だけの到着基準の遅れの上限が、閉ループの広げた適用上限と一致すること"
    );
    assert_eq!(
        TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_MAX_US, 160_000,
        "音声だけの到着基準の遅れの上限は 160 ms であること"
    );
    assert_eq!(
        TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_FLOOR_US - TIMELINE_ARRIVAL_DELAY_US,
        40_000,
        "音声だけの購読の下限 (140 ms) が共有の時間軸の上限 (100 ms) より大きいこと"
    );
    assert_eq!(
        TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_FLOOR_US, 140_000,
        "音声だけの購読の下限は実測で決めた 140 ms であること"
    );
}

/// 補償の予算は基準の差だけを保護し、同じ向きのバッファ差は別に足すこと
///
/// 基準の差には [`TIMELINE_MAX_COMPENSATED_DIFFERENCE_US`] の保護を掛ける。同じ向きの
/// 既知のバッファ差は全額補償し、バッファが基準の差を相殺しているときは予算を増やさない。
/// 先行側は必ず両方を合成した実際の表示時刻から決めるため、この予算はそのまま足し算になる。
#[test]
fn sync_compensation_limit_protects_only_the_base_difference() {
    assert_eq!(
        TIMELINE_MAX_COMPENSATED_DIFFERENCE_US, 100_000,
        "基準の差の保護は 100 ms であること"
    );
    for (base_difference_us, buffer_difference_us, expected_us) in [
        // バッファ差が基準の差と同方向なら、その全額を予算へ足す
        (0, 80_000, 180_000),
        (130_000, 80_000, 180_000),
        // 全体が負でバッファ差も負なら、逆向きのバッファ差を足す (JS の
        // Math.sign(base + buffer) * buffer)
        (0, -80_000, 180_000),
        (-130_000, -80_000, 180_000),
        // バッファが基準の差を相殺している (全体が正だがバッファは負) なら、足さない
        (200_000, -80_000, 100_000),
        (0, 0, 100_000),
    ] {
        assert_eq!(
            sync_compensation_limit_us(base_difference_us, buffer_difference_us),
            expected_us,
            "base={base_difference_us} buffer={buffer_difference_us} の補償の予算"
        );
    }
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

    // 予定を 60 ms 過ぎて鳴った。時間圧縮の不感帯 (20 ms) を引いた 40 ms が許容 (10 ms) を
    // 超えるため、閉ループの目標は 1 回の増分の上限 (40 ms) まで増え、開始値 (50 ms) から
    // 90 ms になる
    timeline.observe_audio_playout(feedback_observation(10_020_000, Some(60_000)));
    let raised_us = 90_000;
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
fn reset_track_clears_the_audio_closed_loop_target() {
    // 音声の再生を取り直したときは、過去の穴と遅れを持ち込まないように閉ループも
    // 初期化する。初期化しないと、再開直後に前回の穴で広げた目標が残り、余分に遅れる。
    // 閉ループの開始値は下限と同じ 50 ms であるため、下げる向きには動かせない。上げた
    // 目標が開始値へ戻ることを確かめる
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Audio, 10_000_000, 1_000_000);
    // 予定を 60 ms 過ぎて鳴った観測で、閉ループの目標を開始値 (50 ms) から 90 ms へ上げる
    timeline.observe_audio_playout(feedback_observation(10_020_000, Some(60_000)));
    assert_eq!(
        timeline.delay_breakdown().audio_delay_feedback.target_us,
        90_000,
        "閉ループの目標が開始値から上がる"
    );
    assert_eq!(
        timeline.delay_breakdown().audio_delay_feedback.adjustments,
        1,
        "1 回だけ動かした"
    );

    timeline.reset_track(Track::Audio);
    assert!(
        timeline.present_us(Track::Audio, 1_000_000).is_none(),
        "基準は消える"
    );
    let breakdown = timeline.delay_breakdown();
    assert_eq!(
        breakdown.audio_delay_feedback.target_us, AUDIO_DELAY_FEEDBACK_START_US,
        "閉ループの目標は開始値へ戻る"
    );
    assert_eq!(
        breakdown.audio_delay_feedback.reason,
        AudioDelayFeedbackReason::Initial,
        "理由も初期状態へ戻る"
    );
    assert_eq!(
        breakdown.audio_delay_feedback.adjustments, 0,
        "動かした回数も 0 へ戻る"
    );

    // 観測し直しても、前回の目標ではなく揺らぎの学習の初期値 (80 ms) から始まる
    timeline.observe(Track::Audio, 20_000_000, 11_000_000);
    assert_eq!(
        timeline.learned_delay_us(Track::Audio),
        AUDIO_DELAY_START_MS * 1_000,
        "購読のやり直しで過去の目標を持ち込まない"
    );

    // すべてリセットしても同じである。いったん目標を上げ直してから確かめる
    timeline.observe_audio_playout(feedback_observation(21_020_000, Some(60_000)));
    assert_eq!(
        timeline.delay_breakdown().audio_delay_feedback.target_us,
        90_000,
        "全リセットの前にもう一度目標を上げる"
    );
    timeline.reset();
    let breakdown = timeline.delay_breakdown();
    assert_eq!(
        breakdown.audio_delay_feedback.target_us, AUDIO_DELAY_FEEDBACK_START_US,
        "全リセットでも閉ループの目標は開始値へ戻る"
    );
    assert_eq!(
        breakdown.audio_delay_feedback.reason,
        AudioDelayFeedbackReason::Initial,
        "全リセットでも理由は初期状態へ戻る"
    );
    assert_eq!(
        breakdown.audio_delay_feedback.adjustments, 0,
        "全リセットでも動かした回数は 0 へ戻る"
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
        underrun_ms: 0,
        compression_us: 0,
        jitter_target_us: 0,
        gap_count: 0,
        gap_us: 0,
        arrival_delay_floor_us: None,
    });
    let after = timeline.delay_breakdown().audio_delay_feedback;
    // 遅れの p50 は 60 ms であり、時間圧縮の不感帯 (20 ms) を引いた 40 ms が許容 (10 ms) を
    // 超える。1 回の増分の上限 (40 ms) だけ上がり、開始値 (50 ms) から 90 ms になる
    assert_eq!(
        after.target_us, 90_000,
        "閉ループの目標は 1 回の増分の上限まで上がる"
    );
    assert_eq!(after.reason, AudioDelayFeedbackReason::Lateness);
    assert_eq!(
        after.last_change_us, 40_000,
        "開始値から 1 回の増分の上限だけ動く"
    );
    assert_eq!(after.adjustments, 1, "1 回動かした");
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
    // 閉ループが決めた目標と、揺らぎの学習値を合成した実際に使う値も読める
    assert_eq!(
        after.jitter_target_us,
        AUDIO_DELAY_START_MS * 1_000,
        "揺らぎだけから求めた目標が出る"
    );
    assert_eq!(
        after.applied_us, 90_000,
        "実際に使う値が出る (揺らぎの 80 ms より閉ループの目標が大きい)"
    );
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
    // 予定を 80 ms 過ぎて鳴った。時間圧縮の不感帯 (20 ms) を引いた 60 ms が許容 (10 ms) を
    // 超えるため、1 回の増分の上限 (40 ms) だけ上がり、開始値 (50 ms) から 90 ms になる
    timeline.observe_audio_playout(stats.audio_delay_feedback(10_100_000));
    assert_eq!(
        timeline.learned_delay_us(Track::Audio),
        90_000,
        "計器の観測でも閉ループが動く"
    );
}

/// 音声の開始時は到着推定の 80 ms を使い、閉ループの初回観測で跳ね上げないこと
///
/// 音声の観測 (observe) がまだ 1 つしか無い間は、揺らぎの学習の開始値 (80 ms) をそのまま
/// 使い、閉ループの初回観測だけで目標を増やさない。映像は音声の遅れから不感帯を引いた分だけ
/// 待つ。
#[test]
fn audio_starts_with_the_arrival_estimate_and_does_not_jump() {
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Video, 1_000_000, 1_000_000);
    timeline.observe(Track::Audio, 1_000_000, 1_000_000);
    assert_eq!(
        timeline.playout_delay_us(),
        Some(AUDIO_DELAY_START_MS * 1_000),
        "到着の分布が揃うまでは揺らぎの学習の開始値を保つこと"
    );
    timeline.observe_audio_playout(feedback_observation(1_000_000, Some(0)));
    assert_eq!(
        timeline.playout_delay_us(),
        Some(AUDIO_DELAY_START_MS * 1_000),
        "閉ループの初回観測で目標を跳ね上げないこと"
    );
    assert_eq!(
        timeline.presentation_delay_us(Track::Video),
        Some(75_000),
        "映像は音声の 80 ms の遅れから不感帯 (5 ms) を引いた分だけ待つこと"
    );
}

/// 時計に由来しうる基準の差だけを 100 ms で保護すること
///
/// 基準の差 (130 ms) は上限 (100 ms) までしか補償せず、残りの 30 ms は A/V のずれとして
/// 残す。音声に必要な 80 ms の jitter buffer は映像へも反映する。
#[test]
fn only_the_base_difference_is_protected_from_compensation() {
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Video, 1_000_000, 1_000_000);
    timeline.observe(Track::Audio, 1_130_000, 1_000_000);
    assert!(timeline.sharing_bases(), "基準の差は共有できること");
    assert_eq!(
        timeline.presentation_delay_us(Track::Video),
        Some(180_000),
        "基準の 100 ms とバッファの 80 ms を補償すること"
    );
    let breakdown = timeline.delay_breakdown();
    let audio_presentation_us = breakdown
        .audio
        .presentation_delay_us
        .expect("音声の基準がある");
    let video_presentation_us = breakdown
        .video
        .presentation_delay_us
        .expect("映像の基準がある");
    assert_eq!(
        audio_presentation_us - video_presentation_us,
        30_000,
        "保護した基準の差を補償しないこと"
    );
}

/// 基準の差とバッファの差が逆向きでも、実際に先行する映像を待たせること
///
/// 映像の jitter buffer (160 ms) が基準の差 (200 ms) を部分的に相殺する。補償の予算は
/// 同じ向きの既知のバッファ差だけを足すため、実際の表示時刻の差は基準の差からバッファ差を
/// 引いた 20 ms になる。
#[test]
fn the_video_waits_even_when_the_buffer_difference_reverses_the_base_difference() {
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Video, 1_000_000, 1_000_000);
    timeline.observe(Track::Audio, 1_200_000, 1_000_000);
    for index in 0..20i64 {
        let timestamp_us = 1_180_000 + index * 20_000;
        timeline.observe(Track::Video, timestamp_us + 160_000, timestamp_us);
    }
    let breakdown = timeline.delay_breakdown();
    assert!(breakdown.sharing_bases, "基準の差は共有できること");
    assert_eq!(
        breakdown.video.jitter_delay_us,
        Some(160_000),
        "映像のバッファが基準の差を部分的に相殺すること"
    );
    assert_eq!(
        breakdown.audio.sync_extra_delay_us, 0,
        "実際には遅い音声をさらに遅らせないこと"
    );
    let audio_presentation_us = breakdown
        .audio
        .presentation_delay_us
        .expect("音声の基準がある");
    let video_presentation_us = breakdown
        .video
        .presentation_delay_us
        .expect("映像の基準がある");
    assert_eq!(
        audio_presentation_us - video_presentation_us,
        20_000,
        "基準の差と逆向きのバッファ差を補償した後の差であること"
    );
}

/// 音声の残差が周期的に上下しても共通の遅延を積み上げないこと
///
/// 過去に映像へ足した補償を必要値の算定へ戻すと、残差が上下するたびに共通の待ち時間が
/// 増え続ける。共有する表示時刻の下限を必要値から決め直すことで、映像の遅れは音声の
/// 自然な遅れと残差の最大に留まる。
#[test]
fn the_shared_floor_does_not_accumulate_the_playout_residual() {
    let mut timeline = PlayoutTimeline::new();
    for index in 0..1_000i64 {
        let at_us = 1_000_000 + index * 20_000;
        timeline.observe(Track::Audio, at_us, at_us);
        timeline.observe(Track::Video, at_us, at_us);
        let target_us = timeline
            .present_us(Track::Audio, at_us)
            .expect("音声の基準がある");
        let residual_us = if (index / 50) % 2 == 0 { 80_000 } else { 0 };
        timeline.record_presentation_with_target(
            Track::Audio,
            at_us,
            target_us + residual_us,
            Some(target_us),
        );
        timeline.observe_audio_playout(feedback_observation(at_us, Some(0)));
        // 開始時を含む音声の自然な遅れ (80 ms) と残差の最大 (80 ms) の和までに留まること。
        // 過去に足した補償を入力へ戻すと 370 ms まで増える
        assert!(
            timeline
                .presentation_delay_us(Track::Video)
                .unwrap_or(i64::MAX)
                <= 160_010,
            "補償した遅延を再び足さないこと: index={index}"
        );
    }
}

/// 音声の目標が広がった呼び出しの中で映像も同期すること
///
/// 穴で閉ループの目標が 160 ms まで広がるとき、次の復号出力を待たずに同期も更新する。
/// 復号出力まで待つと、その間だけ音声と映像の時刻が離れる。
#[test]
fn sync_runs_inside_the_call_that_widens_the_audio_target() {
    let mut timeline = PlayoutTimeline::new();
    for index in 0..5i64 {
        let at_us = 1_000_000 + index * 1_000_000;
        timeline.observe(Track::Audio, at_us, at_us);
        timeline.observe(Track::Video, at_us, at_us);
        let mut observation = feedback_observation(at_us, Some(0));
        observation.gap_count = index as u64;
        observation.gap_us = index * 20_000;
        timeline.observe_audio_playout(observation);
        let breakdown = timeline.delay_breakdown();
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
            "次の復号出力を待たずに表示時刻が揃うこと: index={index}"
        );
    }
    assert_eq!(
        timeline.playout_delay_us(),
        Some(AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US),
        "音切れを避ける適応を残すこと"
    );
    assert!(
        timeline.presentation_delay_us(Track::Video).unwrap_or(0) > 100_000,
        "既知のバッファ差を 100 ms で打ち切らないこと"
    );
    timeline.reset_track(Track::Audio);
    assert_eq!(
        timeline.presentation_delay_us(Track::Video),
        Some(0),
        "音声 OFF で補償を解くこと"
    );
    timeline.observe(Track::Audio, 6_000_000, 6_000_000);
    assert_eq!(
        timeline.playout_delay_us(),
        Some(AUDIO_DELAY_START_MS * 1_000),
        "再開時に過去の穴で深くなった目標を持ち込まないこと"
    );
}

/// 映像は音声の実際の出力時刻へ追従し、同じ残差で遅延を積み上げないこと
///
/// 音声は出力の都合で予定より遅く鳴ることがある。予定どうしの差だけを揃えると、実際に
/// 鳴る音から映像が先行したままになる。予約の結果 (実際の出力) を合わせ先として使う。
#[test]
fn the_video_follows_the_actual_audio_output_without_accumulating_the_residual() {
    let mut timeline = PlayoutTimeline::new();
    for index in 0..100i64 {
        let at_us = 1_000_000 + index * 20_000;
        timeline.observe(Track::Audio, at_us, at_us);
        timeline.observe(Track::Video, at_us, at_us);
        let target_us = timeline
            .present_us(Track::Audio, at_us)
            .expect("音声の基準がある");
        let actual_us = target_us + 20_000;
        timeline.record_presentation_with_target(Track::Audio, at_us, actual_us, Some(target_us));
        // JS のテスト (timeline.test.ts) と同じく、鳴り始めまでの時間だけ 70 ms にする
        let mut observation = feedback_observation(at_us, Some(0));
        observation.start_delay_us = Some(TimingSummary {
            p50: 70_000,
            p95: 70_000,
            max: 70_000,
        });
        timeline.observe_audio_playout(observation);
        let video_present_us = timeline
            .present_us(Track::Video, at_us)
            .expect("映像の基準がある");
        assert!(
            (actual_us - video_present_us).abs() <= 5_010,
            "音声の実際の出力時刻へ映像が追従すること: index={index}"
        );
        assert!(
            timeline
                .presentation_delay_us(Track::Video)
                .unwrap_or(i64::MAX)
                <= 100_000,
            "残差を繰り返し加算しないこと: index={index}"
        );
    }
    timeline.reset_track(Track::Audio);
    assert_eq!(
        timeline.delay_breakdown().audio_playout_correction_us,
        0,
        "停止で出力の実績を消すこと"
    );
}

/// 出力時計の対応が途切れたら古い残差を失効させること
///
/// 音声の予約は続くが、有効な出力時計で測った残差は更新できない状態を作る。再生の実績の
/// 時刻を失効の基準にすると、古い残差が永久に残ってしまう。最後に有効な対応を測った時刻から
/// 1 秒 (`TIMELINE_SKEW_WINDOW_US`) を超えたら失効させる。
#[test]
fn a_lost_output_clock_expires_the_playout_residual() {
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Audio, 1_000_000, 1_000_000);
    timeline.observe(Track::Video, 1_000_000, 1_000_000);
    // 予定 (1_050_000) より 20 ms 遅れて鳴った
    timeline.record_presentation_with_target(Track::Audio, 1_000_000, 1_070_000, Some(1_050_000));
    // 音声の予約は続くが、target を渡さないため残差の時刻は更新されない
    timeline.record_presentation(Track::Audio, 2_100_000, 2_170_000);
    timeline.observe(Track::Video, 2_200_000, 2_200_000);
    assert_eq!(
        timeline.delay_breakdown().audio_playout_correction_us,
        0,
        "最後の有効な対応から 1 秒で失効すること"
    );
}

/// 学習に使うかの判定は、中央値ではなく現フレームの TIMESTAMP 間隔で行うこと
///
/// 間隔の長いフレームが続いた後でも、現フレームの間隔が短ければ、その半分未満で届いた
/// 観測は学習しない。間隔の中央値で判定すると、短い間隔で届いた観測を学習してしまい、
/// 1 フレーム遅れて届いただけの揺らぎを経路の揺らぎとして取り込む。
#[test]
fn learns_uses_the_current_frame_interval() {
    let mut timeline = PlayoutTimeline::new();
    let mut wall_us = 10_000_000;
    let mut timestamp_us = 1_000_000;
    // 40 ms 間隔で届く映像を 30 枚流し、間隔の中央値を 40 ms にし、追いつきを終わらせる
    for _ in 0..30 {
        timeline.observe(Track::Video, wall_us, timestamp_us);
        wall_us += 40_000;
        timestamp_us += 40_000;
    }
    // 以降は 40 ms 間隔のフレームの後に、TIMESTAMP が 10 ms しか進んでいないフレームが
    // 19 ms 後に届く組を繰り返す。現フレームの間隔 (10 ms) の半分 (5 ms) 以上なので
    // 学習し、9 ms の遅れとして目標に現れる。中央値 (40 ms) の半分 (20 ms) 未満なので、
    // 中央値で判定すると学習しない
    for _ in 0..10 {
        // 間隔の長いフレーム (40 ms、到着は 31 ms 後) で中央値を保つ
        wall_us += 31_000;
        timestamp_us += 40_000;
        timeline.observe(Track::Video, wall_us, timestamp_us);
        // 間隔の短いフレーム (10 ms、到着は 19 ms 後)
        wall_us += 19_000;
        timestamp_us += 10_000;
        timeline.observe(Track::Video, wall_us, timestamp_us);
    }
    assert_eq!(
        timeline.learned_delay_us(Track::Video),
        9_000,
        "現フレームの間隔で学習を判定し、9 ms の遅れを学習すること"
    );
}

/// 共有へ戻った直後に A/V のずれをそろえ直すこと
///
/// 共有を解除している間は相手がいないものとして足した分を戻すだけであり、共有へ戻った
/// 最初の観測で足し直さないと、表示時刻の差が 1 観測分だけ大きいまま残る。
/// プロパティテストで見つかった入力 (13.72 ms 間隔、音声 +94.418 ms/秒、映像
/// -100 ms/秒、基準の差 300 ms、197 ステップ) を固定して、共有中は差が「補償しない分 +
/// 出力の残差 + 不感帯」を超えないことを確かめる。
#[test]
fn sharing_resumption_realigns_the_sync_delays() {
    /// 共有中なら、表示時刻の差が「補償しない分 + 出力の残差 + 不感帯」以内であることを確かめる
    ///
    /// 補償の予算は、基準の差の保護 ([`TIMELINE_MAX_COMPENSATED_DIFFERENCE_US`]) と、同じ
    /// 向きの既知のバッファ差の和である。その予算で補償しきれない差と、音声の出力の残差と、
    /// 先行側に残す不感帯だけが、合わせた後の表示時刻の差として残る。
    fn check(timeline: &PlayoutTimeline, label: &str) {
        let breakdown = timeline.delay_breakdown();
        if !breakdown.sharing_bases {
            return;
        }
        let (Some(audio_presentation_us), Some(video_presentation_us)) = (
            breakdown.audio.presentation_delay_us,
            breakdown.video.presentation_delay_us,
        ) else {
            return;
        };
        let (Some(audio_base_us), Some(video_base_us)) =
            (breakdown.audio.base_delay_us, breakdown.video.base_delay_us)
        else {
            return;
        };
        let cap_us = breakdown.presentation_delay_cap_us;
        // 同期が足す前の表示時刻を、出力の残差 (C) を含めて組み立て直す
        let audio_own_us = breakdown
            .audio
            .jitter_delay_us
            .unwrap_or(0)
            .min(cap_us)
            .saturating_add(breakdown.audio_playout_correction_us);
        let video_own_us = breakdown.video.jitter_delay_us.unwrap_or(0).min(cap_us);
        let audio_us = audio_base_us.saturating_add(audio_own_us);
        let video_us = video_base_us.saturating_add(video_own_us);
        let difference_us = audio_us.saturating_sub(video_us);
        // 補償の予算は、基準の差の保護と、同じ向きの既知のバッファ差の和である
        let limit_us = sync_compensation_limit_us(
            audio_base_us.saturating_sub(video_base_us),
            audio_own_us.saturating_sub(video_own_us),
        );
        let uncorrected_us = difference_us
            .saturating_abs()
            .saturating_sub(limit_us)
            .saturating_sub(TIMELINE_SYNC_MIN_DELTA_US)
            .max(0);
        let gap_us = audio_presentation_us
            .saturating_sub(video_presentation_us)
            .saturating_abs();
        // 1 ms は内訳の整数化の丸めを許す
        assert!(
            gap_us
                <= uncorrected_us
                    + breakdown.audio_playout_correction_us
                    + TIMELINE_SYNC_MIN_DELTA_US
                    + 1_000,
            "{label}: 共有へ戻った直後にずれが残っている: gap={gap_us} uncorrected={uncorrected_us} {breakdown:?}"
        );
        // 表示の遅れは上限を超えない
        for track in [Track::Audio, Track::Video] {
            let delay_us = timeline
                .presentation_delay_us(track)
                .expect("共有中は表示の遅れが読める");
            assert!(
                delay_us <= cap_us,
                "{label}: 表示の遅れが上限を超えている: {track:?} delay={delay_us} cap={cap_us}"
            );
        }
    }

    let mut timeline = PlayoutTimeline::new();
    let interval_us = 13_720_i64;
    let mut audio_offset_us = 997_952_i64;
    // 映像の基準は音声より 300 ms 遅い状態から、毎秒 100 ms ずつ音声へ近づく
    let mut video_offset_us = audio_offset_us + 300_000;
    let mut wall_us = 100_000_000_i64;
    for _ in 0..197 {
        wall_us = wall_us.saturating_add(interval_us);
        audio_offset_us += 94_418 * interval_us / 1_000_000;
        video_offset_us -= 100_000 * interval_us / 1_000_000;
        let audio_timestamp_us = wall_us.saturating_sub(audio_offset_us);
        let video_timestamp_us = wall_us.saturating_sub(video_offset_us);
        timeline.observe(Track::Audio, wall_us, audio_timestamp_us);
        check(&timeline, "音声の観測の後");
        timeline.observe(Track::Video, wall_us, video_timestamp_us);
        check(&timeline, "映像の観測の後");
    }
}
