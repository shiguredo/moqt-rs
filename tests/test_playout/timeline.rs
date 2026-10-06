//! 音声と映像の共通の時間軸のテスト
//!
//! 公開 API (`PlayoutTimeline`) の契約を確認する。

use shiguredo_moqt::playout::timeline::{
    PlayoutTimeline, TIMELINE_SYNC_MIN_DELTA_US, TimelineConfig, Track,
};

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
    // 先行する音声の遅れが上がり、表示時刻の差が不感帯に収まる
    let audio_delay_us = timeline
        .presentation_delay_us(Track::Audio)
        .expect("基準があるので遅れが決まる");
    assert!(
        audio_delay_us > timeline.learned_delay_us(Track::Audio),
        "自分の遅れより上に足す: {audio_delay_us}"
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
        "後行側から不感帯だけ手前へ寄せる"
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
