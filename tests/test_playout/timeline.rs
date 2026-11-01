//! 音声と映像の共通の時間軸のテスト
//!
//! moqt-js の `timeline.test.ts` / `timelineLowLatency.test.ts` の意図を、マイクロ秒の
//! 整数で動く [`PlayoutTimeline`] の公開 API に対して固定する。表示時刻の式
//! (TIMESTAMP + 基準の遅れ + 表示の遅れ)、音声の自動予算 (直近 2 秒の揺らぎの百分位 +
//! 直近 1 秒の実出力先行量の p95 を 50〜100 ms に切る)、A/V 同期の不感帯と補償の上限、
//! 基準を共有しない判定 (差・ドリフト・保持)、追いつきの除外を確かめる。
//!
//! PBT (観測の任意の列に対する不変条件) は `pbt/tests/prop_playout/timeline.rs` が固定する。

use shiguredo_moqt::playout::timeline::{
    PlayoutTimeline, TIMELINE_ARRIVAL_DELAY_US, TIMELINE_AUDIO_DELAY_FLOOR_US,
    TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US, TIMELINE_BASE_DRIFT_CONFIRM_US,
    TIMELINE_BASE_DRIFT_US, TIMELINE_BASE_UNSHARED_HOLD_US, TIMELINE_BASE_UNSHARED_RELEASE_US,
    TIMELINE_DISCONTINUITY_US, TIMELINE_JITTER_WINDOW_US, TIMELINE_MAX_COMPENSATED_DIFFERENCE_US,
    TIMELINE_MAX_PRESENTATION_DELAY_MS, TIMELINE_MIN_BASE_DIFFERENCE_MS,
    TIMELINE_MIN_COMPRESSION_DEADBAND_US, TIMELINE_MIN_PLAYOUT_DELAY_PERCENTILE_PPM,
    TIMELINE_QUEUE_HEADROOM_FRAMES, TIMELINE_SYNC_MIN_DELTA_US, TIMELINE_WINDOW_US, TimelineConfig,
    Track, UnsharedReason, audio_arrival_delay_us, playout_delay_percentile, playout_queue_cap_us,
    sync_compensation_limit_us,
};

/// 送信側の壁時計 (Unix epoch マイクロ秒)。メディア時刻 0 の TIMESTAMP にする
const EPOCH_US: i64 = 1_790_263_445_000_000;

/// 30 fps のフレーム間隔 (マイクロ秒)
const FRAME_US: i64 = 1_000_000 / 30;

/// 120 fps のフレーム間隔 (マイクロ秒)
const FAST_FRAME_US: i64 = 1_000_000 / 120;

/// 表示待ちのキューの上限 (枚)。[`TimelineConfig`] の既定値と同じ値にする
const MAX_QUEUED_FRAMES: usize = 24;

/// 丸めの差を許す幅 (マイクロ秒)
///
/// moqt-js はミリ秒の浮動小数、本実装はマイクロ秒の整数で計算する。判定に影響しない
/// 1 マイクロ秒未満の差と、テストが与えたフレーム間隔と実装が測った間隔の差だけを許す。
const TOLERANCE_US: i64 = 10;

/// 音声の TIMESTAMP が壁時計から遅れる速さ (毎秒マイクロ秒)。実測 (336 秒で 16027 ms)
const DRIFT_US_PER_SECOND: i64 = 48_000;

/// メディア時刻のフレームの TIMESTAMP (Unix epoch マイクロ秒)
fn timestamp_of(media_us: i64) -> i64 {
    EPOCH_US + media_us
}

/// 表示時刻を求める時間軸を作る
fn create_timeline() -> PlayoutTimeline {
    PlayoutTimeline::new()
}

/// 表示時刻 (Unix epoch マイクロ秒) を求める
fn wall_us_of(timeline: &PlayoutTimeline, track: Track, media_us: i64) -> i64 {
    timeline
        .present_us(track, timestamp_of(media_us))
        .expect("表示時刻が決まること")
}

/// 2 つのトラックの表示の遅れの差 (マイクロ秒)
fn presentation_delay_gap_us(timeline: &PlayoutTimeline) -> i64 {
    let breakdown = timeline.delay_breakdown();
    breakdown
        .audio
        .presentation_delay_us
        .expect("音声の表示の遅れが決まること")
        - breakdown
            .video
            .presentation_delay_us
            .expect("映像の表示の遅れが決まること")
}

/// 音声も映像も同一の時刻に観測する (揺らぎ 0)
fn observe_pair(timeline: &mut PlayoutTimeline, wall_us: i64, media_us: i64) {
    timeline.observe(Track::Audio, wall_us, timestamp_of(media_us));
    timeline.observe(Track::Video, wall_us, timestamp_of(media_us));
}

/// 音声と映像の到着列 (どちらも揺らぎ 0、音声は 5 個に 1 個が 40 ms 遅れ) を与える
///
/// 音声は 20 ms ごと (Opus)、映像は 30 fps。`duration_us` の間、実時間どおりに届く。
fn observe_both_streams(timeline: &mut PlayoutTimeline, duration_us: i64) {
    let mut arrivals: Vec<(i64, Track, i64)> = Vec::new();
    let mut index = 0i64;
    while index * 20_000 < duration_us {
        let media_us = index * 20_000;
        let jitter_us = if index % 5 == 4 { 40_000 } else { 0 };
        arrivals.push((EPOCH_US + media_us + jitter_us, Track::Audio, media_us));
        index += 1;
    }
    let mut index = 0i64;
    while index * FRAME_US < duration_us {
        let media_us = index * FRAME_US;
        arrivals.push((EPOCH_US + media_us, Track::Video, media_us));
        index += 1;
    }
    arrivals.sort_by_key(|(at_us, _, _)| *at_us);
    for (at_us, track, media_us) in arrivals {
        timeline.observe(track, at_us, timestamp_of(media_us));
    }
}

/// 音声の TIMESTAMP だけが壁時計から遅れていく到着列を与える (実測)
///
/// 音声と映像はどちらも揺らぎ 0 で同じ時刻に届く。音声の TIMESTAMP だけが毎秒
/// [`DRIFT_US_PER_SECOND`] ずつ壁時計から遅れる。返り値は最後に観測したメディア時刻。
fn observe_drifted_streams(timeline: &mut PlayoutTimeline, frame_us: i64, duration_us: i64) -> i64 {
    let mut media_us = 0i64;
    while media_us < duration_us {
        let observed_at_us = EPOCH_US + media_us;
        timeline.observe(Track::Video, observed_at_us, timestamp_of(media_us));
        // 音声の TIMESTAMP は毎秒 [`DRIFT_US_PER_SECOND`] ずつ古くなる。整数の切り捨てで
        // 1 µs 未満の差が出るが、判定に影響しない
        let drift_us = media_us.saturating_mul(DRIFT_US_PER_SECOND) / 1_000_000;
        timeline.observe(
            Track::Audio,
            observed_at_us,
            timestamp_of(media_us.saturating_sub(drift_us)),
        );
        media_us += frame_us;
    }
    media_us
}

/// 音声の開始時は 50 ms の下限を使い、映像はその不感帯の手前まで待つこと
#[test]
fn starts_without_a_delay() {
    let timeline = create_timeline();
    assert_eq!(timeline.present_us(Track::Video, timestamp_of(0)), None);
    assert_eq!(timeline.present_us(Track::Audio, timestamp_of(0)), None);
    assert_eq!(timeline.presentation_delay_us(), None);
    assert_eq!(timeline.playout_delay_us(), None);
    assert_eq!(timeline.video_delay_us(), None);
    assert_eq!(timeline.presentation_extra_delay_us(), None);
    assert_eq!(timeline.audio_delay_stats().jitter_us, None);
    assert_eq!(timeline.audio_delay_stats().output_lead_us, None);
    assert_eq!(
        timeline.audio_delay_stats().target_us,
        TIMELINE_AUDIO_DELAY_FLOOR_US,
        "未観測でも自動予算の計器は下限を返すこと"
    );
    let breakdown = timeline.delay_breakdown();
    assert!(!breakdown.sharing_bases);
    assert_eq!(breakdown.unshared_reason, UnsharedReason::Unobserved);
    assert_eq!(breakdown.base_difference_us, None);
    assert_eq!(breakdown.base_difference_recent_us, None);
    assert_eq!(breakdown.base_drift_us_per_second, None);
}

/// 最初の観測で TIMESTAMP + 基準の遅れ + 表示の遅れになること
#[test]
fn first_observation_uses_the_audio_floor_and_the_sync_deadband() {
    let mut timeline = create_timeline();
    observe_pair(&mut timeline, EPOCH_US, 0);

    assert_eq!(
        timeline.playout_delay_us(),
        Some(TIMELINE_AUDIO_DELAY_FLOOR_US),
        "出力時計が未観測でも音声は下限を使うこと"
    );
    assert_eq!(
        timeline.video_delay_us(),
        Some(TIMELINE_AUDIO_DELAY_FLOOR_US - TIMELINE_SYNC_MIN_DELTA_US),
        "映像は音声の不感帯の手前まで遅れること"
    );
    assert_eq!(
        wall_us_of(&timeline, Track::Audio, 0),
        EPOCH_US + TIMELINE_AUDIO_DELAY_FLOOR_US
    );
    assert_eq!(
        wall_us_of(&timeline, Track::Video, 0),
        EPOCH_US + TIMELINE_AUDIO_DELAY_FLOOR_US - TIMELINE_SYNC_MIN_DELTA_US
    );
    assert_eq!(
        timeline.presentation_delay_us(),
        Some(TIMELINE_AUDIO_DELAY_FLOOR_US - TIMELINE_SYNC_MIN_DELTA_US),
        "内訳の遅れは映像を優先して返すこと"
    );
}

/// TIMESTAMP の差どおりに表示時刻が進むこと
#[test]
fn present_time_follows_the_timestamp() {
    let mut timeline = create_timeline();
    // 復号の出力が TIMESTAMP より 9 秒遅れている
    timeline.observe(Track::Audio, 10_000_000, 1_000_000);
    // 表示時刻 = TIMESTAMP + 基準の遅れ (9 秒) + 表示の遅れ (50 ms)
    assert_eq!(
        timeline.present_us(Track::Audio, 1_000_000),
        Some(10_050_000)
    );
    assert_eq!(
        timeline.presentation_extra_delay_us(),
        Some(TIMELINE_AUDIO_DELAY_FLOOR_US)
    );
    // TIMESTAMP が 20 ms 進めば表示時刻も 20 ms 進む
    assert_eq!(
        timeline.present_us(Track::Audio, 1_020_000),
        Some(10_070_000)
    );
    // 観測が無いトラックの表示時刻は返らない
    assert_eq!(timeline.present_us(Track::Video, 1_000_000), None);
}

/// 揺らぎの観測で映像の表示の遅れが決まること (配信 fps 依存の百分位)
#[test]
fn video_delay_comes_from_the_jitter_percentile() {
    // 音声は観測しない (同期の制御を働かせない)
    let mut timeline = create_timeline();
    // 追いつき中の学習を終えるため、揺らぎ 0 のフレームで確認の間隔を超える
    for index in 0..30i64 {
        let media_us = index * FRAME_US;
        timeline.observe(Track::Video, EPOCH_US + media_us, timestamp_of(media_us));
    }
    assert_eq!(timeline.video_delay_us(), Some(0), "揺らぎ 0 なら遅れない");
    // 100 ms 遅れて届いたフレームが 2 枚あり、2 秒の窓の百分位が 100 ms になる
    for index in 30..32i64 {
        let media_us = index * FRAME_US;
        timeline.observe(
            Track::Video,
            EPOCH_US + media_us + 100_000,
            timestamp_of(media_us),
        );
    }
    assert_eq!(
        timeline.video_delay_us(),
        Some(100_000),
        "窓の百分位 (100 ms) になる"
    );
    assert_eq!(wall_us_of(&timeline, Track::Video, 0), EPOCH_US + 100_000);
}

/// 音声は到着の揺らぎと実出力の先行量の合計から予算を決めること
#[test]
fn audio_budget_adds_the_jitter_and_the_output_lead() {
    // 20 ms ごとに届き、5 個に 1 個が 40 ms 遅れて届く (揺らぎの百分位が 40 ms)
    let mut timeline = create_timeline();
    for index in 0..200i64 {
        let media_us = index * 20_000;
        let jitter_us = if index % 5 == 4 { 40_000 } else { 0 };
        timeline.observe(
            Track::Audio,
            EPOCH_US + media_us + jitter_us,
            timestamp_of(media_us),
        );
    }
    // 実出力の先行量が未観測なら、揺らぎ 40 ms は下限 50 ms に切られる
    assert_eq!(
        timeline.playout_delay_us(),
        Some(TIMELINE_AUDIO_DELAY_FLOOR_US)
    );
    assert_eq!(
        timeline.audio_delay_stats().jitter_us,
        Some(40_000),
        "媒体時刻からの到着の幅を測ること"
    );
    assert_eq!(timeline.audio_delay_stats().output_lead_us, None);
    // 実出力の先行量を記録すると、揺らぎと足した 83 ms が予算になる
    let now_us = EPOCH_US + 200 * 20_000;
    timeline.observe_audio_output_lead(43_000, now_us);
    let stats = timeline.audio_delay_stats();
    assert_eq!(stats.output_lead_us, Some(43_000));
    assert_eq!(stats.jitter_us, Some(40_000));
    assert_eq!(stats.target_us, 83_000, "50〜100 ms に切った合計になること");
    assert_eq!(timeline.playout_delay_us(), Some(83_000));
}

/// 一定の遅れは基準へ入るため、揺らぎの予算は上がらないこと
#[test]
fn audio_budget_keeps_the_floor_for_a_steady_delay() {
    let mut timeline = create_timeline();
    for index in 0..200i64 {
        let media_us = index * 20_000;
        timeline.observe(
            Track::Audio,
            EPOCH_US + media_us + 40_000,
            timestamp_of(media_us),
        );
    }
    assert_eq!(
        timeline.audio_delay_stats().jitter_us,
        Some(0),
        "一定の遅れは基準に入り、揺らぎにならない"
    );
    assert_eq!(
        timeline.playout_delay_us(),
        Some(TIMELINE_AUDIO_DELAY_FLOOR_US)
    );
}

/// ずれが不感帯を超えたら、先行する側 (ここでは映像) の遅れを不感帯の手前まで上げること
#[test]
fn sync_raises_the_late_video_to_the_deadband() {
    let mut timeline = create_timeline();
    observe_both_streams(&mut timeline, 12_000_000);
    let audio_delay_us = timeline.playout_delay_us().expect("音声を観測している");
    let video_delay_us = timeline.video_delay_us().expect("映像を観測している");
    assert_eq!(audio_delay_us, TIMELINE_AUDIO_DELAY_FLOOR_US);
    assert_eq!(
        video_delay_us,
        TIMELINE_AUDIO_DELAY_FLOOR_US - TIMELINE_SYNC_MIN_DELTA_US,
        "映像の遅れが音声の不感帯の手前まで上がること"
    );
    // 同時刻の表示時刻の差は不感帯ちょうどになる
    let difference_us = wall_us_of(&timeline, Track::Audio, 11_000_000)
        - wall_us_of(&timeline, Track::Video, 11_000_000);
    assert_eq!(difference_us, TIMELINE_SYNC_MIN_DELTA_US);
}

/// 音声が先行していても、許す幅は映像と同じ不感帯であること
#[test]
fn sync_raises_the_leading_audio_to_the_deadband() {
    // 音声は揺らぎ 0 (予算は下限の 50 ms)、映像は 5 枚に 1 枚が 100 ms 遅れて届く
    let mut arrivals: Vec<(i64, Track, i64)> = Vec::new();
    let mut index = 0i64;
    while index * 20_000 < 12_000_000 {
        let media_us = index * 20_000;
        arrivals.push((EPOCH_US + media_us, Track::Audio, media_us));
        index += 1;
    }
    let mut index = 0i64;
    while index * FRAME_US < 12_000_000 {
        let media_us = index * FRAME_US;
        let jitter_us = if index % 5 == 4 { 100_000 } else { 0 };
        arrivals.push((EPOCH_US + media_us + jitter_us, Track::Video, media_us));
        index += 1;
    }
    arrivals.sort_by_key(|(at_us, _, _)| *at_us);
    let mut timeline = create_timeline();
    for (at_us, track, media_us) in arrivals {
        timeline.observe(track, at_us, timestamp_of(media_us));
    }

    let audio_delay_us = timeline.playout_delay_us().expect("音声を観測している");
    let video_delay_us = timeline.video_delay_us().expect("映像を観測している");
    assert_eq!(audio_delay_us, TIMELINE_AUDIO_DELAY_FLOOR_US);
    assert!(video_delay_us > audio_delay_us, "映像の方が遅いこと");
    // 音声へ足す分は「映像の表示の遅れ - 不感帯 - 音声の表示の遅れ」になる
    let breakdown = timeline.delay_breakdown();
    assert_eq!(
        breakdown.audio.sync_extra_delay_us,
        video_delay_us - TIMELINE_SYNC_MIN_DELTA_US - audio_delay_us,
        "音声が先行していても不感帯まで詰めること"
    );
}

/// targetLatency と再生遅延の大きい方を使うこと
#[test]
fn target_latency_is_the_lower_bound_for_both_tracks() {
    let mut timeline = create_timeline();
    timeline.set_target_latency_ms(300);
    observe_pair(&mut timeline, EPOCH_US, 0);
    for index in 0..30i64 {
        let media_us = index * FRAME_US;
        timeline.observe(Track::Video, EPOCH_US + media_us, timestamp_of(media_us));
    }
    assert_eq!(timeline.target_latency_ms(), 300);
    assert_eq!(
        timeline.presentation_extra_delay_us(),
        Some(300_000),
        "自動の音声予算 50 ms より targetLatency を優先すること"
    );
    assert_eq!(timeline.video_delay_us(), Some(300_000));
    assert_eq!(timeline.presentation_delay_us(), Some(300_000));
    assert_eq!(wall_us_of(&timeline, Track::Audio, 0), EPOCH_US + 300_000);
    assert_eq!(wall_us_of(&timeline, Track::Video, 0), EPOCH_US + 300_000);
    assert_eq!(timeline.target_latency_limited_us(), 0, "上限に収まること");
    // 再生遅延より小さい targetLatency を渡しても、遅い方 (再生遅延) を使う
    timeline.set_target_latency_ms(10);
    assert_eq!(timeline.target_latency_ms(), 10);
    assert_eq!(
        timeline.presentation_extra_delay_us(),
        Some(TIMELINE_AUDIO_DELAY_FLOOR_US)
    );
    // 0 を渡すと「下限にしない」の意味になる
    timeline.set_target_latency_ms(0);
    assert_eq!(timeline.target_latency_ms(), 0);
}

/// 下限が変わったらその場で表示時刻を合わせ直すこと
#[test]
fn target_latency_change_realigns_immediately() {
    let mut timeline = create_timeline();
    // 音声の表示の遅れ (下限の 50 ms) との差で、映像に不感帯の手前までの 45 ms が足されて
    // いる
    observe_pair(&mut timeline, EPOCH_US, 0);
    assert_eq!(
        presentation_delay_gap_us(&timeline),
        TIMELINE_SYNC_MIN_DELTA_US
    );

    // 音声の下限より大きい targetLatency を渡すと、両方の下限が 300 ms になる。映像に
    // 足していた分はそのままでは差になるため、その場で合わせ直す
    timeline.set_target_latency_ms(300);
    let difference_us =
        wall_us_of(&timeline, Track::Audio, 0) - wall_us_of(&timeline, Track::Video, 0);
    assert!(
        difference_us.saturating_abs() <= TIMELINE_SYNC_MIN_DELTA_US + TOLERANCE_US,
        "下限の変更で表示時刻の差が開いたままにならないこと: {difference_us}"
    );
    assert!(
        timeline.presentation_extra_delay_us().unwrap_or(0) >= 300_000,
        "音声の表示の遅れが新しい下限を下回らないこと"
    );
    assert!(timeline.video_delay_us().unwrap_or(0) >= 300_000);
}

/// キュー由来の上限で targetLatency を切り下げ、切り下げた分を計器に出すこと
#[test]
fn queue_cap_limits_the_target_latency() {
    let mut timeline = create_timeline();
    // 120 fps のフレーム間隔を覚えさせる。キューが吸収できる長さは (上限 - 余裕) 枚分
    for index in 0..60i64 {
        let media_us = index * FAST_FRAME_US;
        timeline.observe(Track::Video, EPOCH_US + media_us, timestamp_of(media_us));
    }
    let queue_cap_us = (MAX_QUEUED_FRAMES - TIMELINE_QUEUE_HEADROOM_FRAMES) as i64 * FAST_FRAME_US;
    assert!(queue_cap_us < TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000);
    timeline.set_target_latency_ms(4_000);
    assert_eq!(timeline.presentation_delay_us(), Some(queue_cap_us));
    assert_eq!(
        timeline.target_latency_limited_us(),
        4_000_000 - queue_cap_us,
        "切り下げた分が読めること"
    );
    // 基準の遅れには上限を掛けない。音声と映像は同じ値に切り下げられるため同じ表示時刻
    // になる
    timeline.observe(Track::Audio, EPOCH_US, timestamp_of(0));
    assert_eq!(
        wall_us_of(&timeline, Track::Audio, 1_000_000),
        wall_us_of(&timeline, Track::Video, 1_000_000),
        "切り下げても同じ表示時刻になること"
    );
}

/// 上限が既定値のときはその値で切り下げること
#[test]
fn max_presentation_delay_caps_the_target_latency() {
    let mut timeline = create_timeline();
    // フレーム間隔を覚えていない間は、キュー由来の上限が既定値 (500 ms) になる
    timeline.observe(Track::Video, EPOCH_US, timestamp_of(0));
    timeline.set_target_latency_ms(5_000);
    assert_eq!(
        timeline.presentation_delay_us(),
        Some(TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000)
    );
    assert_eq!(
        timeline.target_latency_limited_us(),
        5_000_000 - TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000
    );
}

/// 表示の遅れは時計のずれの分だけ負になりうること
#[test]
fn presentation_delay_can_be_negative() {
    // 音声は観測しない (同期の制御を働かせない)
    let mut timeline = create_timeline();
    // 受信側の壁時計では EPOCH のときに、TIMESTAMP が 5 秒先のフレームが届く
    timeline.observe(Track::Video, EPOCH_US, timestamp_of(5_000_000));
    assert_eq!(timeline.presentation_delay_us(), Some(-5_000_000));
    // 表示時刻 = TIMESTAMP + 負の遅れ = 受信側の壁時計
    assert_eq!(wall_us_of(&timeline, Track::Video, 5_000_000), EPOCH_US);
}

/// 基準が大きく離れたトラックの表示時刻を返さないこと
#[test]
fn drifted_track_has_no_presentation_time() {
    let mut timeline = create_timeline();
    // 音声の基準が 1 秒、映像の基準が 2 秒 (閾値 450 ms を超える)
    timeline.observe(Track::Audio, EPOCH_US, timestamp_of(0) - 1_000_000);
    timeline.observe(Track::Video, EPOCH_US, timestamp_of(0) - 2_000_000);
    assert!(!timeline.sharing_bases());
    assert_eq!(timeline.unshared_reason(), UnsharedReason::Difference);
    assert!(
        timeline
            .present_us(Track::Audio, timestamp_of(0) - 1_000_000)
            .is_some()
    );
    assert!(
        timeline
            .present_us(Track::Video, timestamp_of(0) - 2_000_000)
            .is_none(),
        "遅れている側の表示時刻は返さないこと"
    );
    assert!(timeline.presentation_extra_delay_us().is_some());
}

/// 音声の TIMESTAMP がドリフトしたら共有せず、映像を到着基準の時刻へ合わせること
#[test]
fn audio_drift_unshares_and_video_follows_the_arrival_delay() {
    for frame_us in [FRAME_US, FRAME_US / 2] {
        let mut timeline = create_timeline();
        // 120 秒
        let media_us = observe_drifted_streams(&mut timeline, frame_us, 120_000_000);
        assert!(
            !timeline.sharing_bases(),
            "frame_us={frame_us}: 基準を共有しないこと"
        );
        assert_eq!(timeline.unshared_reason(), UnsharedReason::Drift);
        // ずれた側 (音声) は表示時刻を返さず、到着基準の再生へ落ちる
        assert!(
            timeline
                .present_us(Track::Audio, timestamp_of(media_us))
                .is_none(),
            "frame_us={frame_us}: ずれた側の表示時刻を返さないこと"
        );
        // 映像は自分の基準を使い、表示時刻が「観測の時刻 + 上限」に収まる
        let video_present_us = wall_us_of(&timeline, Track::Video, media_us);
        assert!(
            video_present_us
                <= EPOCH_US + media_us + TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000 + TOLERANCE_US,
            "frame_us={frame_us}: 映像の表示時刻が未来へ伸びないこと: {video_present_us}"
        );
        // 映像の遅れは、音声の到着基準の遅れから不感帯を引いた値になる
        let video_delay_us = timeline.video_delay_us().expect("映像の遅れが決まること");
        let arrival_delay_us = timeline
            .audio_arrival_delay_us()
            .expect("音声の到着基準の遅れが決まること");
        assert_eq!(
            video_delay_us,
            arrival_delay_us - TIMELINE_SYNC_MIN_DELTA_US,
            "frame_us={frame_us}: 映像を音声の到着基準の時刻へ合わせること"
        );
        assert!(
            timeline.playout_delay_us().unwrap_or(0) >= arrival_delay_us,
            "frame_us={frame_us}: 音声の遅れが上限を超えないこと"
        );
        // 実際に鳴るのは「到着 + 到着基準の遅れ」である。映像が音声より遅れないこと
        let audio_plays_at_us = EPOCH_US + media_us + arrival_delay_us;
        let video_shows_at_us = EPOCH_US + video_present_us - EPOCH_US;
        assert!(
            video_shows_at_us.saturating_sub(audio_plays_at_us) <= TIMELINE_SYNC_MIN_DELTA_US,
            "frame_us={frame_us}: 映像が音声より遅れて表示されないこと"
        );
    }
}

/// ドリフトは差が閾値に達する前に、動き続けていることで検出すること
#[test]
fn audio_drift_is_detected_before_the_difference_threshold() {
    let mut timeline = create_timeline();
    // 閾値に達する前に検出できていれば、映像へ足した分は動きの幅の中に留まる
    observe_drifted_streams(&mut timeline, FRAME_US, 20_000_000);
    assert!(!timeline.sharing_bases(), "基準を共有しないこと");
    // 検出した後は、映像へ足す先が音声の到着基準の遅れ (上限 100 ms) に変わる
    assert!(
        timeline.video_delay_us().unwrap_or(0)
            <= TIMELINE_ARRIVAL_DELAY_US - TIMELINE_SYNC_MIN_DELTA_US + TOLERANCE_US,
        "映像へ足した分が到着基準の上限の中に収まること"
    );
    // 動きの速さが入力と同じ値になる
    let drift_us_per_second = timeline
        .delay_breakdown()
        .base_drift_us_per_second
        .expect("履歴ができている");
    assert!(
        (drift_us_per_second - DRIFT_US_PER_SECOND).saturating_abs() < 1_000,
        "動きの速さが入力と同じであること: {drift_us_per_second}"
    );
}

/// 基準の差が大きくても相手側へ足す分は上限までにすること
#[test]
fn large_base_difference_is_compensated_only_up_to_the_limit() {
    let mut timeline = create_timeline();
    // 音声だけが TIMESTAMP より 300 ms 遅れて届き (定常)、5 秒目から TIMESTAMP が
    // さらに 150 ms 古くなる。差が動き続ける場合 (ドリフト) と同じ条件にする
    let audio_frame_us = 20_000;
    let mut next_video_us = 0i64;
    for wall_us in (0..20_000_000).step_by(audio_frame_us as usize) {
        while next_video_us <= wall_us {
            timeline.observe(
                Track::Video,
                EPOCH_US + next_video_us,
                timestamp_of(next_video_us),
            );
            next_video_us += FRAME_US;
        }
        let step_us = if wall_us < 5_000_000 { 0 } else { 150_000 };
        let audio_timestamp_us = EPOCH_US + wall_us - 300_000 - step_us;
        timeline.observe(Track::Audio, EPOCH_US + wall_us, audio_timestamp_us);
    }
    let breakdown = timeline.delay_breakdown();
    assert!(
        breakdown
            .base_difference_us
            .unwrap_or(0)
            .saturating_sub(450_000)
            .saturating_abs()
            <= 1_000,
        "差が開いていること: {:?}",
        breakdown.base_difference_us
    );
    // 映像へ足すのは上限まで。残りの差は A/V のずれとして残す
    assert!(
        breakdown.video.sync_extra_delay_us <= TIMELINE_MAX_COMPENSATED_DIFFERENCE_US,
        "映像へ足す分が上限を超えないこと: {}",
        breakdown.video.sync_extra_delay_us
    );
    assert!(
        timeline.video_delay_us().unwrap_or(i64::MAX)
            <= TIMELINE_MAX_COMPENSATED_DIFFERENCE_US + 1_000,
        "映像の表示の遅れが上限を超えないこと"
    );
}

/// 音声の TIMESTAMP に段差があっても、映像へ足す遅延は上限までにすること
#[test]
fn audio_timestamp_step_does_not_grow_the_audio_budget() {
    let mut timeline = create_timeline();
    let audio_frame_us = 20_000;
    let step_at_us = 5_000_000;
    let step_us = 600_000;
    let mut next_video_us = 0i64;
    let mut wall_us = 0i64;
    while wall_us < 10_000_000 {
        while next_video_us <= wall_us {
            timeline.observe(
                Track::Video,
                EPOCH_US + next_video_us,
                timestamp_of(next_video_us),
            );
            next_video_us += FRAME_US;
        }
        // 段差の後は、音声の TIMESTAMP だけが 600 ms 古くなる (到着は変わらない)
        let audio_timestamp_us = if wall_us < step_at_us {
            EPOCH_US + wall_us
        } else {
            EPOCH_US + wall_us - step_us
        };
        timeline.observe(Track::Audio, EPOCH_US + wall_us, audio_timestamp_us);
        wall_us += audio_frame_us;
    }
    // 未来へ進む時計の段差を、恒常的な待ちへ移さない
    assert!(
        timeline.playout_delay_us().unwrap_or(i64::MAX) <= 100_000,
        "音声の予算が段差で無制限に育たないこと: {:?}",
        timeline.playout_delay_us()
    );
    assert!(
        timeline.delay_breakdown().video.sync_extra_delay_us
            <= TIMELINE_MAX_COMPENSATED_DIFFERENCE_US,
        "映像へ足す分が上限を超えないこと"
    );
    assert!(
        timeline.video_delay_us().unwrap_or(i64::MAX)
            <= TIMELINE_MAX_COMPENSATED_DIFFERENCE_US + 1_000,
        "映像の表示待ちが上限に留まること"
    );
}

/// 2 つのトラックの表示時刻の差が不感帯に収まること
#[test]
fn presentation_times_stay_within_the_deadband() {
    let mut timeline = create_timeline();
    observe_both_streams(&mut timeline, 30_000_000);
    let difference_us = wall_us_of(&timeline, Track::Audio, 29_000_000)
        - wall_us_of(&timeline, Track::Video, 29_000_000);
    assert!(
        difference_us.saturating_abs() <= TIMELINE_SYNC_MIN_DELTA_US + 5_000,
        "差が不感帯に収まること: {difference_us}"
    );
    assert!(difference_us > 0, "映像の方が先に出ること");
}

/// 一過性の基準のずれでは共有をやめないこと
#[test]
fn transient_basis_move_does_not_unshare() {
    let mut timeline = create_timeline();
    // 5 秒目から 3 秒間、音声の TIMESTAMP だけが 95 ms 新しくなる (到着は変わらない)
    let mut reasons = Vec::new();
    let mut next_video_us = 0i64;
    let mut wall_us = 0i64;
    while wall_us < 20_000_000 {
        while next_video_us <= wall_us {
            timeline.observe(
                Track::Video,
                EPOCH_US + next_video_us,
                timestamp_of(next_video_us),
            );
            next_video_us += FRAME_US;
        }
        let shift_us = if (5_000_000..8_000_000).contains(&wall_us) {
            95_000
        } else {
            0
        };
        timeline.observe(
            Track::Audio,
            EPOCH_US + wall_us,
            timestamp_of(wall_us + shift_us),
        );
        reasons.push(timeline.unshared_reason());
        wall_us += 20_000;
    }
    assert!(
        !reasons.contains(&UnsharedReason::Drift),
        "一過性の動きを時計のずれと判定しないこと"
    );
    assert!(timeline.sharing_bases(), "観測の後も基準を共有していること");
}

/// 一過性の動きが去って差が戻ったら、保持を待たずに共有を戻すこと
#[test]
fn difference_return_releases_the_hold() {
    let mut timeline = create_timeline();
    // 5 秒目に音声の基準が 900 ms 開き、3 秒かけて実時間に追いつく。音声の TIMESTAMP を
    // 古い側へずらすと、受信側の「到着 - TIMESTAMP」= 基準の遅れはその分だけ大きくなる
    let excursion_start_us = 5_000_000;
    let catch_up_us = 3_000_000;
    let excursion_us = 900_000;
    let audio_frame_us = 20_000;
    let mut next_video_us = 0i64;
    let mut samples: Vec<(i64, UnsharedReason)> = Vec::new();
    let mut wall_us = 0i64;
    while wall_us < 25_000_000 {
        while next_video_us <= wall_us {
            timeline.observe(
                Track::Video,
                EPOCH_US + next_video_us,
                timestamp_of(next_video_us),
            );
            next_video_us += FRAME_US;
        }
        let shifted_us = if wall_us < excursion_start_us {
            0
        } else {
            let progress =
                (wall_us - excursion_start_us).min(catch_up_us) as f64 / catch_up_us as f64;
            // 音声の TIMESTAMP を古い側へずらす (基準の遅れが開く)。切り捨てで 1 µs 未満の
            // 差が出るが判定に影響しない
            -((excursion_us as f64) * (1.0 - progress)) as i64
        };
        timeline.observe(
            Track::Audio,
            EPOCH_US + wall_us,
            timestamp_of(wall_us + shifted_us),
        );
        samples.push((wall_us, timeline.unshared_reason()));
        wall_us += audio_frame_us;
    }

    // 動いている間は解除する (差が閾値を超えている、または離れた幅が続いている) ため、
    // 一過性の動きでも解除のきっかけで保持に入る
    assert!(
        samples
            .iter()
            .any(|(_, reason)| *reason == UnsharedReason::Hold),
        "一過性の動きで解除の保持に入ること"
    );
    assert!(
        timeline.sharing_bases(),
        "一過性の動きが去った後は共有を戻すこと"
    );
    let released_at_us = samples
        .iter()
        .find(|(at_us, reason)| *at_us > excursion_start_us && *reason == UnsharedReason::None)
        .map(|(at_us, _)| *at_us)
        .expect("観測の間に共有が戻ること");
    assert!(
        released_at_us
            <= excursion_start_us
                + catch_up_us
                + TIMELINE_BASE_UNSHARED_RELEASE_US
                + audio_frame_us,
        "差が戻ってから解除の条件が続く時間のうちに戻すこと: {released_at_us}"
    );
    assert!(
        released_at_us < excursion_start_us + TIMELINE_BASE_UNSHARED_HOLD_US,
        "保持の満了 (30 秒) を待たずに戻すこと: {released_at_us}"
    );
}

/// 映像の復号がのこぎり波で遅れても共有をやめないこと
#[test]
fn decoder_sawtooth_does_not_unshare() {
    let mut timeline = create_timeline();
    let mut reasons = Vec::new();
    let mut next_video_us = 0i64;
    let mut next_audio_us = 0i64;
    let mut next_sample_us = 0i64;
    let mut wall_us = 0i64;
    while wall_us < 40_000_000 {
        while next_video_us <= wall_us {
            // 5 秒ごとに 1000 ms まで遅れ、次の観測で戻る (のこぎり波)
            let lag_us = (next_video_us % 5_000_000) * 1_000_000 / 5_000_000;
            timeline.observe(
                Track::Video,
                EPOCH_US + next_video_us + lag_us,
                timestamp_of(next_video_us),
            );
            next_video_us += FRAME_US;
        }
        while next_audio_us <= wall_us {
            timeline.observe(
                Track::Audio,
                EPOCH_US + next_audio_us,
                timestamp_of(next_audio_us),
            );
            next_audio_us += 20_000;
        }
        while next_sample_us <= wall_us {
            reasons.push(timeline.unshared_reason());
            next_sample_us += 1_000_000;
        }
        wall_us += 5_000;
    }
    let unshared = reasons
        .iter()
        .filter(|reason| **reason != UnsharedReason::None)
        .count();
    assert_eq!(unshared, 0, "復号の乱れを時計のずれと判定しないこと");
    assert!(timeline.sharing_bases(), "観測の後も基準を共有していること");
}

/// 復号の遅れが去った後に解除が残らないこと
#[test]
fn decoder_lag_gone_leaves_no_unshared_hold() {
    let mut timeline = create_timeline();
    let mut reasons = Vec::new();
    let mut next_video_us = 0i64;
    let mut next_audio_us = 0i64;
    let mut next_sample_us = 0i64;
    let mut wall_us = 0i64;
    while wall_us < 60_000_000 {
        while next_video_us <= wall_us {
            // 10 秒目まではのこぎり波で遅れ (判定を始める水準の決定がこの期間に入る)、
            // 以後は遅れない
            let lag_us = if next_video_us >= 10_000_000 {
                0
            } else {
                (next_video_us % 5_000_000) * 1_000_000 / 5_000_000
            };
            timeline.observe(
                Track::Video,
                EPOCH_US + next_video_us + lag_us,
                timestamp_of(next_video_us),
            );
            next_video_us += FRAME_US;
        }
        while next_audio_us <= wall_us {
            timeline.observe(
                Track::Audio,
                EPOCH_US + next_audio_us,
                timestamp_of(next_audio_us),
            );
            next_audio_us += 20_000;
        }
        while next_sample_us <= wall_us {
            reasons.push(timeline.unshared_reason());
            next_sample_us += 1_000_000;
        }
        wall_us += 5_000;
    }
    let after_settled = reasons
        .iter()
        .skip(10)
        .filter(|reason| **reason != UnsharedReason::None)
        .count();
    assert_eq!(after_settled, 0, "復号の遅れが去った後に解除が残らないこと");
}

/// 購読の直後の過渡では共有をやめないこと
#[test]
fn startup_transient_does_not_unshare() {
    let mut timeline = create_timeline();
    // 音声の復号が 150 ms 遅れて始まり、2 秒かけて実時間に追いつく (cache から届いた分を
    // まとめて復号している間)
    let catch_up_us = 150_000;
    let catch_up_duration_us = 2_000_000;
    let mut reasons = Vec::new();
    let mut next_video_us = 0i64;
    let mut wall_us = 0i64;
    while wall_us < 25_000_000 {
        while next_video_us <= wall_us {
            timeline.observe(
                Track::Video,
                EPOCH_US + next_video_us,
                timestamp_of(next_video_us),
            );
            next_video_us += FRAME_US;
        }
        let shift_us = if wall_us < catch_up_duration_us {
            -(catch_up_us * (catch_up_duration_us - wall_us) / catch_up_duration_us)
        } else {
            0
        };
        timeline.observe(
            Track::Audio,
            EPOCH_US + wall_us,
            timestamp_of(wall_us + shift_us),
        );
        reasons.push(timeline.unshared_reason());
        wall_us += 20_000;
    }
    assert!(
        !reasons.contains(&UnsharedReason::Drift),
        "購読の直後の過渡を時計のずれと判定しないこと"
    );
    assert!(
        timeline.sharing_bases(),
        "追いついた後は基準を共有していること"
    );
}

/// 戻らない基準のずれは共有をやめること (判定は持続の確認を待つ)
#[test]
fn persistent_difference_unshares_after_the_confirm_window() {
    let mut timeline = create_timeline();
    let excursion_start_us = 5_000_000;
    let excursion_us = 95_000;
    let mut reasons = Vec::new();
    let mut next_video_us = 0i64;
    let mut wall_us = 0i64;
    while wall_us < 20_000_000 {
        while next_video_us <= wall_us {
            timeline.observe(
                Track::Video,
                EPOCH_US + next_video_us,
                timestamp_of(next_video_us),
            );
            next_video_us += FRAME_US;
        }
        let shift_us = if wall_us >= excursion_start_us {
            excursion_us
        } else {
            0
        };
        timeline.observe(
            Track::Audio,
            EPOCH_US + wall_us,
            timestamp_of(wall_us + shift_us),
        );
        reasons.push(timeline.unshared_reason());
        wall_us += 20_000;
    }
    let drift_index = reasons
        .iter()
        .position(|reason| *reason == UnsharedReason::Drift)
        .expect("戻らないずれを時計のずれと判定すること");
    // 判定は離れた幅が続いた時間を待ってからである。一瞬の動きで解除しないための待ち
    assert!(
        (drift_index as i64) * 20_000 >= excursion_start_us + TIMELINE_BASE_DRIFT_CONFIRM_US,
        "離れた幅が続いた時間を待ってから判定すること: drift_index={drift_index}"
    );
    assert!(
        !timeline.sharing_bases(),
        "ずれた側があるときは基準を共有しないこと"
    );
}

/// 差が動き続けている間は保持を解除しないこと
#[test]
fn moving_difference_keeps_the_hold() {
    let mut timeline = create_timeline();
    // 5 秒目から、音声の TIMESTAMP が毎秒 [`DRIFT_US_PER_SECOND`] ずつ壁計から遅れる
    let excursion_start_us = 5_000_000;
    let mut reasons = Vec::new();
    let mut next_video_us = 0i64;
    let mut wall_us = 0i64;
    while wall_us < 60_000_000 {
        while next_video_us <= wall_us {
            timeline.observe(
                Track::Video,
                EPOCH_US + next_video_us,
                timestamp_of(next_video_us),
            );
            next_video_us += FRAME_US;
        }
        let shift_us = if wall_us < excursion_start_us {
            0
        } else {
            -(DRIFT_US_PER_SECOND * (wall_us - excursion_start_us) / 1_000_000)
        };
        timeline.observe(
            Track::Audio,
            EPOCH_US + wall_us,
            timestamp_of(wall_us + shift_us),
        );
        reasons.push(timeline.unshared_reason());
        wall_us += 20_000;
    }
    assert!(
        reasons.contains(&UnsharedReason::Drift),
        "動き続けるずれを時計のずれと判定すること"
    );
    // 保持の満了 (30 秒) を過ぎても解除しない。解除の条件を数えていないことでも確かめる
    assert!(
        !timeline.sharing_bases(),
        "差が動き続けている間は共有しないこと"
    );
    assert_eq!(
        timeline.delay_breakdown().base_unshared_return_us,
        None,
        "動きが去っていないため解除の条件を数えないこと"
    );
}

/// 段差の後の差が動かないまま保持されること
#[test]
fn unshared_hold_prevents_flapping() {
    let mut timeline = create_timeline();
    // 5 秒目に音声の TIMESTAMP が 150 ms 古くなる (段差)。以後は動かない
    let mut next_video_us = 0i64;
    let mut wall_us = 0i64;
    while wall_us < 15_000_000 {
        while next_video_us <= wall_us {
            timeline.observe(
                Track::Video,
                EPOCH_US + next_video_us,
                timestamp_of(next_video_us),
            );
            next_video_us += FRAME_US;
        }
        let shift_us = if wall_us < 5_000_000 { 0 } else { 150_000 };
        timeline.observe(
            Track::Audio,
            EPOCH_US + wall_us,
            timestamp_of(wall_us + shift_us),
        );
        wall_us += 20_000;
    }
    assert!(!timeline.sharing_bases(), "保持の間は基準を共有しないこと");
}

/// 保持を早く解除する条件が続いている時間を読めること
#[test]
fn unshared_return_is_reported_while_counting() {
    let mut timeline = create_timeline();
    // 一過性の動きで解除した後、差が戻った状態が続くと解除の条件を数え始める
    let mut next_video_us = 0i64;
    let mut wall_us = 0i64;
    while wall_us < 20_000_000 {
        while next_video_us <= wall_us {
            timeline.observe(
                Track::Video,
                EPOCH_US + next_video_us,
                timestamp_of(next_video_us),
            );
            next_video_us += FRAME_US;
        }
        let shift_us = if (5_000_000..8_000_000).contains(&wall_us) {
            900_000
        } else {
            0
        };
        timeline.observe(
            Track::Audio,
            EPOCH_US + wall_us,
            timestamp_of(wall_us - shift_us),
        );
        wall_us += 20_000;
    }
    assert!(
        timeline.sharing_bases(),
        "一過性の動きが去れば共有へ戻ること"
    );
    assert_eq!(
        timeline.delay_breakdown().base_unshared_return_us,
        None,
        "解除した後は数えていないこと"
    );
}

/// 閾値だけが動いた解除でも、閾値が戻れば解除を早めること
#[test]
fn threshold_only_unshare_is_released() {
    let mut timeline = create_timeline();
    let mut next_video_us = 0i64;
    let mut wall_us = 0i64;
    let observe_until = |timeline: &mut PlayoutTimeline,
                         next_video_us: &mut i64,
                         wall_us: &mut i64,
                         until_us: i64|
     -> Vec<(i64, UnsharedReason)> {
        let mut reasons = Vec::new();
        while *wall_us < until_us {
            while *next_video_us <= *wall_us {
                timeline.observe(
                    Track::Video,
                    EPOCH_US + *next_video_us,
                    timestamp_of(*next_video_us),
                );
                *next_video_us += FRAME_US;
            }
            // 音声の基準だけ 400 ms 遅らせる
            timeline.observe(
                Track::Audio,
                EPOCH_US + *wall_us,
                timestamp_of(*wall_us - 300_000),
            );
            reasons.push((*wall_us, timeline.unshared_reason()));
            *wall_us += 20_000;
        }
        reasons
    };

    // 差 400 ms は、閾値 (表示の遅れの上限 500 ms - 音声の表示の遅れ 50 ms = 450 ms) の
    // 内側である
    observe_until(&mut timeline, &mut next_video_us, &mut wall_us, 10_000_000);
    assert!(timeline.sharing_bases(), "差が閾値の内側なら共有すること");
    // targetLatency を上げると閾値が下がり、差が変わらないまま解除される
    timeline.set_target_latency_ms(450);
    observe_until(&mut timeline, &mut next_video_us, &mut wall_us, 11_000_000);
    assert!(!timeline.sharing_bases(), "閾値を超えたら共有しないこと");
    assert_eq!(timeline.unshared_reason(), UnsharedReason::Difference);
    // 閾値を戻すと差は再び閾値の内側になる。直ちには戻さないが、解除の条件が続けば解除する
    timeline.set_target_latency_ms(0);
    let reasons = observe_until(&mut timeline, &mut next_video_us, &mut wall_us, 20_000_000);
    let released_at_us = reasons
        .iter()
        .find(|(_, reason)| *reason == UnsharedReason::None)
        .map(|(at_us, _)| *at_us)
        .expect("閾値が戻った後に解除すること");
    assert!(
        released_at_us <= 11_000_000 + TIMELINE_BASE_UNSHARED_RELEASE_US + 1_000_000,
        "解除の条件が続いた後に解除すること: {released_at_us}"
    );
    assert!(
        timeline.sharing_bases(),
        "閾値が戻って差が水準のままなら解除すること"
    );
}

/// 閾値が行き来する間は解除を早めないこと
#[test]
fn repeated_unshare_within_the_hold_is_kept() {
    let mut timeline = create_timeline();
    let mut next_video_us = 0i64;
    let mut wall_us = 0i64;
    let observe_until = |timeline: &mut PlayoutTimeline,
                         next_video_us: &mut i64,
                         wall_us: &mut i64,
                         until_us: i64| {
        while *wall_us < until_us {
            while *next_video_us <= *wall_us {
                timeline.observe(
                    Track::Video,
                    EPOCH_US + *next_video_us,
                    timestamp_of(*next_video_us),
                );
                *next_video_us += FRAME_US;
            }
            timeline.observe(
                Track::Audio,
                EPOCH_US + *wall_us,
                timestamp_of(*wall_us - 300_000),
            );
            *wall_us += 20_000;
        }
    };
    observe_until(&mut timeline, &mut next_video_us, &mut wall_us, 10_000_000);
    // 1 回目の解除と、閾値が戻った後の早期解除
    timeline.set_target_latency_ms(450);
    observe_until(&mut timeline, &mut next_video_us, &mut wall_us, 11_000_000);
    timeline.set_target_latency_ms(0);
    observe_until(&mut timeline, &mut next_video_us, &mut wall_us, 20_000_000);
    assert!(timeline.sharing_bases(), "1 回目は解除すること");
    // 2 回目の解除 (往復)。差は同じ水準のまま閾値だけが動く
    timeline.set_target_latency_ms(450);
    observe_until(&mut timeline, &mut next_video_us, &mut wall_us, 21_000_000);
    assert!(!timeline.sharing_bases(), "2 回目も解除されること");
    // 閾値を戻しても、直近に解除しているため解除を早めない
    timeline.set_target_latency_ms(0);
    observe_until(&mut timeline, &mut next_video_us, &mut wall_us, 31_000_000);
    assert!(!timeline.sharing_bases(), "往復している間は保持すること");
    assert_eq!(
        timeline.delay_breakdown().base_unshared_return_us,
        None,
        "解除の条件を数えないこと (往復している)"
    );
}

/// 解除のきっかけが去っても、往復している間は 30 秒の保持が満了するまで共有しないこと
///
/// 早期解除 (差が戻った状態が 2 秒続いたら戻す) は、直近に共有と解除を往復している間は
/// 働かない。その場合は 30 秒の保持の満了だけで共有が戻る。以前のテストは映像の観測を
/// 止めていたため、記録が止まった後の状態を確かめておらず、満了そのものを検証していな
/// かった。
#[test]
fn hold_expires_after_the_full_hold_window() {
    let mut timeline = create_timeline();
    let step_us = 20_000;
    let mut wall_us = 0i64;
    // 音声の TIMESTAMP だけ 300 ms 古い状態で観測する (基準の差が 300 ms ある)
    let observe_until = |timeline: &mut PlayoutTimeline, wall_us: &mut i64, until_us: i64| {
        while *wall_us < until_us {
            timeline.observe(Track::Video, EPOCH_US + *wall_us, timestamp_of(*wall_us));
            timeline.observe(
                Track::Audio,
                EPOCH_US + *wall_us,
                timestamp_of(*wall_us - 300_000),
            );
            *wall_us += step_us;
        }
    };

    // 10 秒: 差 300 ms は閾値 (20 ms ごとに観測しているためキューが吸収できる長さは
    // 400 ms、そこから音声の遅れ 50 ms を引いた 350 ms) の内側であり、共有している
    observe_until(&mut timeline, &mut wall_us, 10_000_000);
    assert!(timeline.sharing_bases(), "差が閾値の内側なら共有すること");

    // 1 回目の解除: targetLatency を上げると閾値が下がり、差 300 ms が閾値 100 ms を超える
    timeline.set_target_latency_ms(450);
    observe_until(&mut timeline, &mut wall_us, 11_000_000);
    assert_eq!(timeline.unshared_reason(), UnsharedReason::Difference);
    // 閾値を戻すと差は内側へ戻り、2 秒の早期解除で共有が戻る
    timeline.set_target_latency_ms(0);
    observe_until(&mut timeline, &mut wall_us, 14_000_000);
    assert!(
        timeline.sharing_bases(),
        "1 回目は原因が去って 2 秒で早期解除されること"
    );

    // 2 回目の解除: 直近の解除から 30 秒以内のため、以後は早期解除が働かない
    timeline.set_target_latency_ms(450);
    observe_until(&mut timeline, &mut wall_us, 15_000_000);
    assert_eq!(
        timeline.unshared_reason(),
        UnsharedReason::Difference,
        "2 回目の解除が始まること"
    );
    timeline.set_target_latency_ms(0);
    // 原因 (閾値の低下) は去っているが、往復しているため早期解除は働かない
    observe_until(&mut timeline, &mut wall_us, 20_000_000);
    assert!(!timeline.sharing_bases(), "往復している間は保持すること");
    assert_eq!(timeline.unshared_reason(), UnsharedReason::Hold);

    // 2 回目の解除の 29 秒後はまだ保持している
    observe_until(&mut timeline, &mut wall_us, 15_000_000 + 29_000_000);
    assert_eq!(
        timeline.unshared_reason(),
        UnsharedReason::Hold,
        "30 秒の保持が満了する 1 秒前はまだ保持すること"
    );
    assert!(!timeline.sharing_bases());

    // 2 回目の解除の 31 秒後は、保持が満了して共有が戻る
    observe_until(&mut timeline, &mut wall_us, 15_000_000 + 31_000_000);
    assert!(
        timeline.sharing_bases(),
        "30 秒の保持が満了して共有が戻ること"
    );
}

/// 映像の学習分布が上限超過だけになっても、空を遅延 0 として適用しないこと
#[test]
fn only_over_cap_jitter_keeps_the_previous_video_delay() {
    let mut timeline = PlayoutTimeline::new();
    for index in 0..160i64 {
        let media_us = index * 50_000;
        // 基準の 0 ms は 10 秒窓へ残すが、2 秒の学習窓は 40 ms だけにしておく
        let jitter_us = if index < 40 {
            0
        } else if index < 100 {
            40_000
        } else {
            600_000
        };
        timeline.observe(
            Track::Video,
            media_us + 20_000 + jitter_us,
            EPOCH_US + media_us,
        );
        if index >= 100 {
            assert_eq!(
                timeline.video_delay_us(),
                Some(40_000),
                "有効な観測が無い間は直前値を保つこと: index={index}"
            );
        }
    }
}

/// 圧縮の不感帯は有効な学習の 1,999 ms まで保ち、2,000 ms で下限へ移ること
#[test]
fn compression_deadband_follows_the_learning_window() {
    let mut timeline = PlayoutTimeline::new();
    assert_eq!(
        timeline.compression_deadband_us(20_000),
        20_000,
        "未観測なら初期値を保つこと"
    );
    let mut learned_at_us = 0i64;
    let mut at_us = 0i64;
    while at_us < 1_000_000 {
        timeline.observe(Track::Audio, at_us, at_us);
        if timeline.audio_delay_stats().jitter_us.is_some() {
            learned_at_us = at_us;
            break;
        }
        at_us += 20_000;
    }
    assert!(learned_at_us > 0, "実際に初期学習を終えてから窓を測ること");
    let mut at_us = learned_at_us + 20_000;
    while at_us < learned_at_us + 1_999_000 {
        timeline.observe(Track::Audio, at_us, at_us);
        at_us += 20_000;
    }
    timeline.observe(
        Track::Audio,
        learned_at_us + 1_999_000,
        learned_at_us + 1_999_000,
    );
    assert_eq!(
        timeline.compression_deadband_us(20_000),
        20_000,
        "1,999 ms までは初期値を保つこと"
    );
    timeline.observe(
        Track::Audio,
        learned_at_us + 2_000_000,
        learned_at_us + 2_000_000,
    );
    assert_eq!(
        timeline.compression_deadband_us(20_000),
        TIMELINE_MIN_COMPRESSION_DEADBAND_US,
        "2,000 ms で下限へ移ること"
    );
}

/// 到着基準の同期先は目標値で代用せず、実際の深さとその回復に追従すること
#[test]
fn audio_arrival_delay_observation_drives_the_sync() {
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Audio, 1_000_000, 1_000_000);
    timeline.observe(Track::Video, 1_000_000, 1_000_000);
    assert_eq!(timeline.audio_arrival_delay_us(), Some(50_000));
    timeline.observe_audio_arrival_delay(Some(10_000), 1_000_000);
    assert_eq!(
        timeline.audio_arrival_delay_us(),
        Some(10_000),
        "再同期直後の浅い実出力を隠さないこと"
    );
    timeline.observe_audio_arrival_delay(Some(50_000), 1_100_000);
    assert_eq!(
        timeline.audio_arrival_delay_us(),
        Some(50_000),
        "深さが戻った後は実値を使うこと"
    );
    timeline.reset_track(Track::Audio);
    assert_eq!(
        timeline.audio_arrival_delay_us(),
        None,
        "出力を止めた後へ古い実値を持ち越さないこと"
    );
}

/// 映像の一時的な遅延が消えたら、同期用の待ちを保持せず通常の予算へ戻すこと
#[test]
fn temporary_video_delay_is_released() {
    let mut timeline = create_timeline();
    let mut available_us = 0i64;
    let mut maximum_delay_us = 0i64;
    for index in 0..400i64 {
        let media_us = index * FRAME_US;
        let extra_us = if (100..150).contains(&index) {
            120_000
        } else {
            0
        };
        available_us = (available_us + 100).max(media_us + extra_us);
        timeline.observe(
            Track::Video,
            EPOCH_US + available_us,
            timestamp_of(media_us),
        );
        maximum_delay_us = maximum_delay_us.max(timeline.video_delay_us().unwrap_or(0));
    }
    assert!(
        maximum_delay_us >= 100_000,
        "実際に遅れている間は到着を待つこと: {maximum_delay_us}"
    );
    assert!(
        timeline.video_delay_us().unwrap_or(i64::MAX) <= TOLERANCE_US,
        "過去の遅れを引きずらないこと: {:?}",
        timeline.video_delay_us()
    );
}

/// 音声の残差が周期的に上下しても共通の遅延を積み上げないこと
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
        // 開始時を含む音声の自然な遅れ (50 ms) と残差の最大 (80 ms) の和までに留まること
        assert!(
            timeline.video_delay_us().unwrap_or(i64::MAX) <= 130_010,
            "補償した遅延を再び足さないこと: index={index}"
        );
    }
}

/// 映像は音声の実際の出力時刻へ追従し、同じ残差で遅延を積み上げないこと
#[test]
fn the_video_follows_the_actual_audio_output() {
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
        let video_present_us = timeline
            .present_us(Track::Video, at_us)
            .expect("映像の基準がある");
        assert!(
            (actual_us - video_present_us).saturating_abs() <= 5_010,
            "音声の実際の出力時刻へ映像が追従すること: index={index}"
        );
        assert!(
            timeline.video_delay_us().unwrap_or(i64::MAX) <= 100_000,
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
#[test]
fn lost_output_clock_expires_the_residual() {
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
        timeline.video_delay_us(),
        Some(9_000),
        "現フレームの間隔で学習を判定し、9 ms の遅れを学習すること"
    );
}

/// まとまって届いたフレーム (cache の再生) を表示の遅れの目標に使わないこと
#[test]
fn cache_replay_is_not_learned() {
    // 30 fps のフレームが 2 ms 間隔で 90 枚 (メディア時刻で 3 秒分) 届く
    let mut timeline = create_timeline();
    for index in 0..90i64 {
        let media_us = index * FRAME_US;
        timeline.observe(
            Track::Video,
            EPOCH_US + index * 2_000,
            timestamp_of(media_us),
        );
    }
    assert_eq!(timeline.video_delay_us(), Some(0));
}

/// live に追いつくまでに届いた揺らぎは学習しないこと
#[test]
fn catch_up_arrivals_are_not_learned() {
    let mut timeline = PlayoutTimeline::new();
    // 基準の遅れが 250 ms ごとに 20 ms 以上下がる (live へ追いついている最中) 観測を続ける。
    // 遅れは大きいが、これは経路の揺らぎではない
    for index in 0..30i64 {
        let wall_us = 10_000_000 + index * 33_333 + 100_000;
        let timestamp_us = 1_000_000 + index * 53_333;
        timeline.observe(Track::Video, wall_us, timestamp_us);
    }
    assert_eq!(
        timeline.video_delay_us(),
        Some(0),
        "追いつき中は学習しないこと"
    );
    // 追いついた後に届いた揺らぎは学習する
    let mut wall_us = 11_000_000;
    let mut timestamp_us = 2_599_990;
    for index in 0..100i64 {
        let jitter_us = if index % 10 == 0 { 40_000 } else { 0 };
        wall_us += 33_333;
        timestamp_us += 33_333;
        timeline.observe(Track::Video, wall_us + jitter_us, timestamp_us);
    }
    assert_eq!(
        timeline.video_delay_us(),
        Some(40_000),
        "追いついた後の揺らぎだけを学習すること"
    );
}

/// 音声の開始時の揺らぎが 40 ms でも、出力の先行量が未観測なら下限を使うこと
#[test]
fn audio_budget_floor_holds_until_the_output_clock_arrives() {
    let mut timeline = PlayoutTimeline::new();
    for index in 0..200i64 {
        let media_us = index * 20_000;
        let jitter_us = if index % 5 == 4 { 40_000 } else { 0 };
        timeline.observe(
            Track::Audio,
            EPOCH_US + media_us + jitter_us,
            timestamp_of(media_us),
        );
    }
    assert_eq!(timeline.audio_delay_stats().jitter_us, Some(40_000));
    assert_eq!(timeline.playout_delay_us(), Some(50_000));
    // 音声の再開では旧 Context の出力先行量を捨てる
    timeline.reset_track(Track::Audio);
    assert_eq!(timeline.audio_delay_stats().output_lead_us, None);
    assert_eq!(timeline.audio_delay_stats().target_us, 50_000);
}

/// 実績から A/V のずれが読めること
#[test]
fn skew_uses_fresh_presentations() {
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Audio, 1_000_000, 1_000_000);
    timeline.observe(Track::Video, 1_000_000, 1_000_000);
    let timestamp_us = timestamp_of(1_000_000);
    let delay_us = timeline
        .presentation_delay_us()
        .expect("表示の遅れが決まる");
    let presented_us = timestamp_us + delay_us;
    // 片方だけでは推定しない
    timeline.record_presentation(Track::Audio, timestamp_us, presented_us);
    assert_eq!(timeline.skew_us(), None);
    timeline.record_presentation(Track::Video, timestamp_us, presented_us);
    assert_eq!(timeline.skew_us(), Some(0), "同じ差ならずれは 0");
    // 映像だけ 30 ms 遅らせると正のずれになる
    timeline.record_presentation(Track::Video, timestamp_us, presented_us + 30_000);
    assert_eq!(timeline.skew_us(), Some(30_000));
    // 1 秒より古い実績は使わない
    timeline.record_presentation(Track::Audio, timestamp_us, presented_us - 2_000_000);
    assert_eq!(timeline.skew_us(), None);
}

/// reset は基準と学習と実績を消し、targetLatency は残すこと
#[test]
fn reset_clears_everything_but_the_target_latency() {
    let mut timeline = create_timeline();
    observe_pair(&mut timeline, EPOCH_US, 0);
    timeline.set_target_latency_ms(300);
    let timestamp_us = timestamp_of(1_000_000);
    let presented_us = timeline
        .present_us(Track::Audio, timestamp_us)
        .expect("表示時刻が決まる");
    timeline.record_presentation(Track::Audio, timestamp_us, presented_us);
    timeline.record_presentation(Track::Video, timestamp_us, presented_us);
    assert_eq!(timeline.skew_us(), Some(0));
    let generation = timeline.generation();

    timeline.reset();
    assert_eq!(timeline.generation(), generation + 1);
    assert_eq!(timeline.presentation_delay_us(), None);
    assert_eq!(timeline.playout_delay_us(), None);
    assert_eq!(timeline.skew_us(), None);
    assert_eq!(timeline.present_us(Track::Video, timestamp_of(0)), None);
    // targetLatency は呼び出し側が決めた値であり、reset では消さない
    assert_eq!(timeline.target_latency_ms(), 300);
    // 次の観測で作り直す
    timeline.observe(Track::Video, EPOCH_US, timestamp_of(0));
    assert_eq!(timeline.presentation_delay_us(), Some(300_000));
}

/// 音声だけを消すと映像だけの値になり、世代は進まないこと
#[test]
fn reset_stream_keeps_the_other_track_and_the_generation() {
    let mut timeline = create_timeline();
    timeline.observe(Track::Audio, EPOCH_US, timestamp_of(0));
    for index in 0..30i64 {
        let media_us = index * FRAME_US;
        timeline.observe(Track::Video, EPOCH_US + media_us, timestamp_of(media_us));
    }
    assert_eq!(
        timeline.playout_delay_us(),
        Some(TIMELINE_AUDIO_DELAY_FLOOR_US)
    );
    // 音声の実績 (同期ずれの推定に使う) も作っておく
    let timestamp_us = timestamp_of(1_000_000);
    let presented_us = timeline
        .present_us(Track::Video, timestamp_us)
        .expect("映像の表示時刻が決まる");
    timeline.record_presentation(Track::Audio, timestamp_us, presented_us);
    timeline.record_presentation(Track::Video, timestamp_us, presented_us);
    assert_eq!(timeline.skew_us(), Some(0));
    let generation = timeline.generation();

    timeline.reset_track(Track::Audio);

    // 音声の値は消える
    assert_eq!(timeline.playout_delay_us(), None);
    // 映像の表示の遅れは揺らぎ 0 のまま (音声の遅延を含まない)
    assert_eq!(timeline.video_delay_us(), Some(0));
    assert_eq!(wall_us_of(&timeline, Track::Video, 0), EPOCH_US);
    assert_eq!(
        wall_us_of(&timeline, Track::Video, 1_000_000),
        EPOCH_US + 1_000_000
    );
    // 消したトラックの実績は残らない
    assert_eq!(timeline.skew_us(), None, "音声の実績を消すこと");
    // 世代は進めない
    assert_eq!(timeline.generation(), generation, "世代を進めないこと");
}

/// TIMESTAMP が飛んだら基準を取り直すこと
#[test]
fn timestamp_jump_starts_a_new_generation() {
    let mut timeline = PlayoutTimeline::new();
    timeline.observe(Track::Audio, EPOCH_US, timestamp_of(0));
    let generation = timeline.generation();
    // TIMESTAMP が 3 秒進んだフレームが届く
    let jump_us = 3_000_000;
    assert!(jump_us >= TIMELINE_DISCONTINUITY_US);
    timeline.observe(
        Track::Audio,
        EPOCH_US + 2 * FRAME_US,
        timestamp_of(2 * FRAME_US + jump_us),
    );
    assert_eq!(timeline.generation(), generation + 1, "取り直すこと");
    // 取り直した後も表示時刻が求まる
    assert!(
        timeline
            .present_us(Track::Audio, timestamp_of(2 * FRAME_US + jump_us))
            .is_some()
    );
}

/// 負の設定値は 0 として扱うこと
#[test]
fn negative_config_values_are_clamped() {
    let config = TimelineConfig {
        max_presentation_delay_ms: -100,
        target_latency_ms: -50,
        ..TimelineConfig::default()
    };
    let mut timeline = PlayoutTimeline::with_config(config);
    timeline.observe(Track::Audio, 10_000_000, 1_000_000);
    assert_eq!(timeline.target_latency_ms(), 0);
    assert_eq!(
        timeline.presentation_extra_delay_us(),
        Some(0),
        "上限が 0 なら表示の遅れも 0"
    );
    assert_eq!(timeline.target_latency_limited_us(), 0);
}

/// 再生遅延の目標にする百分位が配信 fps から決まること
#[test]
fn playout_delay_percentile_follows_the_frame_rate() {
    assert_eq!(
        playout_delay_percentile(None),
        TIMELINE_MIN_PLAYOUT_DELAY_PERCENTILE_PPM,
        "フレーム間隔が不明なら下限を使うこと"
    );
    assert_eq!(
        playout_delay_percentile(Some(0)),
        TIMELINE_MIN_PLAYOUT_DELAY_PERCENTILE_PPM
    );
    assert_eq!(
        playout_delay_percentile(Some(-1)),
        TIMELINE_MIN_PLAYOUT_DELAY_PERCENTILE_PPM
    );
    // 30 fps (33.3 ms) で約 96.7%
    assert_eq!(playout_delay_percentile(Some(FRAME_US)), 966_667);
    // 120 fps (8.3 ms) で約 99.2%
    assert_eq!(playout_delay_percentile(Some(FAST_FRAME_US)), 991_667);
    // 10 fps (100 ms) は下限の 95% に切られる
    assert_eq!(
        playout_delay_percentile(Some(100_000)),
        TIMELINE_MIN_PLAYOUT_DELAY_PERCENTILE_PPM
    );
}

/// 表示待ちのキューが吸収できる表示の遅れが枚数から決まること
#[test]
fn playout_queue_cap_follows_the_queue_size() {
    assert_eq!(
        playout_queue_cap_us(MAX_QUEUED_FRAMES, None),
        TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000,
        "フレーム間隔が不明なら上限を返すこと"
    );
    assert_eq!(
        playout_queue_cap_us(MAX_QUEUED_FRAMES, Some(FRAME_US)),
        (MAX_QUEUED_FRAMES - TIMELINE_QUEUE_HEADROOM_FRAMES) as i64 * FRAME_US
    );
    assert_eq!(
        playout_queue_cap_us(TIMELINE_QUEUE_HEADROOM_FRAMES, Some(FRAME_US)),
        0,
        "余裕分しか無ければ 0 になること"
    );
    assert_eq!(
        playout_queue_cap_us(0, Some(FRAME_US)),
        0,
        "上限が 0 でも負にならないこと"
    );
}

/// 音声を到着基準で並べるときの再生の遅れが下限と上限の間に切られること
#[test]
fn audio_arrival_delay_clamps_between_floor_and_cap() {
    assert_eq!(
        TIMELINE_AUDIO_DELAY_FLOOR_US, 50_000,
        "音声の再生の遅れの下限は 50 ms であること"
    );
    assert_eq!(
        audio_arrival_delay_us(TIMELINE_AUDIO_DELAY_FLOOR_US),
        TIMELINE_AUDIO_DELAY_FLOOR_US,
        "下限ちょうどの遅れはそのまま使うこと"
    );
    assert_eq!(
        audio_arrival_delay_us(TIMELINE_AUDIO_DELAY_FLOOR_US - 1),
        TIMELINE_AUDIO_DELAY_FLOOR_US,
        "下限を下回る遅れは下限まで上げること"
    );
    assert_eq!(audio_arrival_delay_us(0), TIMELINE_AUDIO_DELAY_FLOOR_US);
    assert_eq!(audio_arrival_delay_us(90_000), 90_000);
    assert_eq!(audio_arrival_delay_us(1_000_000), TIMELINE_ARRIVAL_DELAY_US);
    assert_eq!(
        audio_arrival_delay_us(TIMELINE_ARRIVAL_DELAY_US),
        TIMELINE_ARRIVAL_DELAY_US,
        "上限ちょうどの遅れはそのまま使うこと"
    );
}

/// 補償の予算は基準の差だけを保護し、同じ向きのバッファ差は別に足すこと
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

/// 基準の差が閾値を超えたら共有せず、大きい側は表示時刻を返さないこと
#[test]
fn sharing_bases_follows_the_difference_limit() {
    // 30 fps のフレーム間隔を覚えさせる (キューが吸収できる長さが上限の 500 ms を超える)
    let mut timeline = create_timeline();
    for index in 0..30i64 {
        let media_us = index * FRAME_US;
        timeline.observe(Track::Video, EPOCH_US + media_us, timestamp_of(media_us));
    }
    // 閾値は表示の遅れの上限 (500 ms) から音声の表示の遅れ (下限の 50 ms) を引いた 450 ms
    timeline.observe(
        Track::Audio,
        EPOCH_US
            + (TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000 - TIMELINE_AUDIO_DELAY_FLOOR_US + 1_000),
        timestamp_of(0),
    );
    assert!(!timeline.sharing_bases(), "閾値を超えたら共有しないこと");
    assert_eq!(timeline.unshared_reason(), UnsharedReason::Difference);
}

/// 閾値の下限を下回る差でも共有しないこと
#[test]
fn sharing_bases_use_the_difference_floor() {
    let mut timeline = create_timeline();
    timeline.observe(Track::Video, EPOCH_US, timestamp_of(0));
    timeline.observe(Track::Audio, EPOCH_US + 150_000, timestamp_of(0));
    // キューが吸収できる 500 ms から targetLatency の 450 ms を引くと 50 ms になり、
    // 下限の 100 ms を使う。差 150 ms は閾値を超える
    timeline.set_target_latency_ms(TIMELINE_MAX_PRESENTATION_DELAY_MS - 50);
    assert_eq!(TIMELINE_MIN_BASE_DIFFERENCE_MS, 100);
    assert!(!timeline.sharing_bases());
    // 差が下限の内側なら共有する
    let mut shared = create_timeline();
    shared.observe(Track::Video, EPOCH_US, timestamp_of(0));
    shared.observe(
        Track::Audio,
        EPOCH_US + TIMELINE_MIN_BASE_DIFFERENCE_MS * 1_000,
        timestamp_of(0),
    );
    shared.set_target_latency_ms(TIMELINE_MAX_PRESENTATION_DELAY_MS - 50);
    assert!(shared.sharing_bases(), "差が下限の内側なら共有すること");
}

/// 遅延の内訳が表示の遅れの内訳と一致すること
#[test]
fn delay_breakdown_splits_basis_jitter_and_sync() {
    let mut timeline = create_timeline();
    observe_pair(&mut timeline, EPOCH_US, 0);
    let breakdown = timeline.delay_breakdown();
    assert!(breakdown.sharing_bases);
    assert_eq!(breakdown.unshared_reason, UnsharedReason::None);
    assert_eq!(breakdown.base_drift_limit_us, TIMELINE_BASE_DRIFT_US);
    assert_eq!(
        breakdown.presentation_delay_cap_us,
        TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000
    );
    for track in [breakdown.audio, breakdown.video] {
        let base_us = track.base_delay_us.expect("基準の遅れが決まる");
        let jitter_us = track.jitter_delay_us.expect("jitter buffer の遅延が決まる");
        // 表示の遅れ = 基準の遅れ + max(jitter buffer の遅延, targetLatency) + 同期の分
        assert_eq!(
            track.presentation_delay_us.expect("表示の遅れが決まる"),
            base_us
                + jitter_us.max(timeline.target_latency_ms() * 1_000)
                + track.sync_extra_delay_us
        );
    }
    assert_eq!(
        breakdown.base_difference_us.expect("差が決まる"),
        breakdown.audio.base_delay_us.expect("音声の基準がある")
            - breakdown.video.base_delay_us.expect("映像の基準がある")
    );
    assert_eq!(
        breakdown.audio_delay.target_us,
        TIMELINE_AUDIO_DELAY_FLOOR_US
    );
}

/// 動きの判定に使う直近の窓が公開されていること
#[test]
fn delay_breakdown_reports_the_recent_window() {
    assert_eq!(TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US, 2_000_000);
    assert_eq!(TIMELINE_JITTER_WINDOW_US, 2_000_000);
    assert_eq!(TIMELINE_WINDOW_US, 10_000_000);
}

/// メディア時刻ごとの揺らぎ (マイクロ秒) の列を作る決定論的な擬似乱数
///
/// moqt-js の `timeline.test.ts` の `jitterSequence` と同じく、同じ seed から同じ列を
/// 作る (JS と同じ倍精度の計算で剰余を取る)。2% が 40 ms、0.1% が 200 ms 遅れ、それ以外は
/// 0 ms である。
fn jitter_sequence(count: usize, seed: f64) -> Vec<i64> {
    let mut values = Vec::with_capacity(count);
    let mut state = seed;
    for _ in 0..count {
        state = (state * 1_103_515_245.0 + 12_345.0) % 2_147_483_648.0;
        let roll = state / 2_147_483_648.0;
        if roll < 0.02 {
            values.push(40_000);
        } else if roll > 0.999 {
            values.push(200_000);
        } else {
            values.push(0);
        }
    }
    values
}

/// 120 秒の到着列を作る (音声 20 ms、映像 30 fps、どちらも揺らぎあり)
///
/// 返り値は観測の時刻の順に並べた (観測の時刻, トラック, メディア時刻) の列である。
/// 音声と映像の揺らぎは別の seed から作る (moqt-js の `buildArrivals` と同じ)。
fn build_jittered_arrivals() -> Vec<(i64, Track, i64)> {
    let duration_us = 120_000_000i64;
    let audio_frame_us = 20_000i64;
    let audio_count = (duration_us / audio_frame_us) as usize;
    let video_count = (duration_us / FRAME_US) as usize;
    let audio_jitter = jitter_sequence(audio_count, 7.0);
    let video_jitter = jitter_sequence(video_count, 11.0);
    let mut arrivals: Vec<(i64, Track, i64)> = Vec::new();
    for (index, jitter_us) in audio_jitter.iter().enumerate() {
        let media_us = index as i64 * audio_frame_us;
        arrivals.push((EPOCH_US + media_us + jitter_us, Track::Audio, media_us));
    }
    for (index, jitter_us) in video_jitter.iter().enumerate() {
        let media_us = index as i64 * FRAME_US;
        arrivals.push((EPOCH_US + media_us + jitter_us, Track::Video, media_us));
    }
    arrivals.sort_by_key(|(at_us, _, _)| *at_us);
    arrivals
}

/// 時計の対応付けの不感帯と映像の write の遅れを同時に与えても、同期ずれの推定が
/// その予算に収まること
///
/// moqt-js の `timeline.test.ts` の「recordPresentation: 不感帯と write の遅れを含めても
/// skewMs が ±50 ms に収まる」に対応する。120 秒の到着列で、音声は時計の対応付けの
/// 不感帯 (30 ms) まで、映像は write の遅れ (最大 20 ms) まで実績をずらして記録し、
/// 定常状態の推定が write の遅れ + 不感帯 + 不感帯 (A/V 同期) に収まり、推移中も表示の
/// 遅れの上限 (500 ms) を超えないことを確かめる。
#[test]
fn skew_stays_within_the_clock_deadband_and_write_delay_budget() {
    // 音声の時計の対応付けの不感帯 (30 ms) と、映像の write の遅れの上限 (20 ms)
    const AUDIO_CLOCK_DEADBAND_US: i64 = 30_000;
    const MAX_PRESENTATION_LAG_US: i64 = 20_000;
    let mut timeline = create_timeline();
    let arrivals = build_jittered_arrivals();
    let mut max_skew_us = 0i64;
    let mut last_skew_us = None;
    for (index, (at_us, track, media_us)) in arrivals.iter().enumerate() {
        let timestamp_us = timestamp_of(*media_us);
        timeline.observe(*track, *at_us, timestamp_us);
        let presentation_us = timeline
            .present_us(*track, timestamp_us)
            .expect("表示時刻が決まること");
        // 音声は時計の対応付けの不感帯 (30 ms) まで、映像は write の遅れ (20 ms) までずらす
        let deviation_us = match track {
            Track::Audio => AUDIO_CLOCK_DEADBAND_US * (index % 2) as i64,
            Track::Video => MAX_PRESENTATION_LAG_US * ((index % 3) / 2) as i64,
        };
        timeline.record_presentation(
            *track,
            timestamp_us,
            presentation_us.saturating_add(deviation_us),
        );
        if let Some(skew_us) = timeline.skew_us() {
            max_skew_us = max_skew_us.max(skew_us.saturating_abs());
            last_skew_us = Some(skew_us);
        }
    }
    // 音声の目標遅延が段差で上がった直後は、次の観測で同期の制御が合わせるまで映像が
    // 先行する。定常状態では不感帯と write の遅れの予算に収まる
    let last_skew_us = last_skew_us.expect("最後の同期ずれが求められること");
    assert!(
        last_skew_us.saturating_abs()
            <= MAX_PRESENTATION_LAG_US + AUDIO_CLOCK_DEADBAND_US + TIMELINE_SYNC_MIN_DELTA_US,
        "定常状態のずれが write の遅れ + 不感帯の予算を超えた: {last_skew_us}"
    );
    assert!(
        max_skew_us <= TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000,
        "推移中のずれが表示の遅れの上限を超えた: {max_skew_us}"
    );
}

/// 出力の残差の平滑化は四捨五入すること
///
/// 切り捨てを続けると、再帰フィルタの定常状態で下振れが積もる (レビュー M-2)。
/// 1 µs の残差を入れた後に残差 0 の記録が来たとき、切り捨てなら 0 µs、四捨五入なら
/// 1 µs になる値で固定する。
#[test]
fn the_output_residual_is_rounded_to_the_nearest_microsecond() {
    let mut timeline = PlayoutTimeline::new();
    let timestamp_us = timestamp_of(0);
    timeline.observe(Track::Audio, EPOCH_US, timestamp_us);
    timeline.observe(Track::Video, EPOCH_US, timestamp_us);
    let audio_target_us = timeline
        .present_us(Track::Audio, timestamp_us)
        .expect("音声の表示時刻が決まる");
    // 最初の記録は残差そのもの (平滑化前) になる。1 µs 遅れて鳴った
    timeline.record_presentation_with_target(
        Track::Audio,
        timestamp_us,
        audio_target_us + 1,
        Some(audio_target_us),
    );
    assert_eq!(
        timeline.delay_breakdown().audio_playout_correction_us,
        1,
        "最初の残差をそのまま使うこと"
    );
    // 19.999 ms 後に残差 0 で鳴った。(1 µs * 250 ms) / (250 ms + 19.999 ms) = 0.926 µs
    // であり、切り捨てなら 0、四捨五入なら 1 になる
    let later_target_us = audio_target_us + 20_000;
    timeline.record_presentation_with_target(
        Track::Audio,
        timestamp_us,
        later_target_us,
        Some(later_target_us),
    );
    assert_eq!(
        timeline.delay_breakdown().audio_playout_correction_us,
        1,
        "平滑化は四捨五入すること (切り捨てなら 0 になる)"
    );
}
