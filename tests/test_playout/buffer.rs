//! 表示時刻に合わせてフレームを選ぶキューのテスト
//!
//! 公開 API (`PlayoutBuffer`) の契約を確認する。moqt-js の `src/playout/buffer.test.ts` の
//! 意図 (目標表示時刻に最も近いフレームの選択、許容幅と遅れてよい上限、表示時刻を決められない
//! フレームの到着順、キューの上限、引数の負値の扱い (debug ビルドでは誤りとして停止し、
//! release ビルドでは 0 として扱う)、学習した再生遅延への追随) を移植する。
//!
//! 時刻はすべてマイクロ秒である。時間軸へ渡す観測の時刻は受信側の壁時計 (Unix epoch) であり、
//! 選択へ渡す目標表示時刻も同じ軸 (Unix epoch) である。moqt-js は選択を
//! `performance.now()` の軸で行うが、本実装は `present_us` が返す epoch の軸で行うため、
//! 原点の変換は要らない。

use shiguredo_moqt::playout::buffer::{
    JITTER_BUFFER_MAX_QUEUED_FRAMES, MAX_PRESENTATION_LAG_US, PlayoutBuffer,
};
use shiguredo_moqt::playout::timeline::{PlayoutTimeline, Track};

/// 送信側の壁時計 (Unix epoch マイクロ秒)。メディア時刻 0 のフレームの TIMESTAMP にする
const EPOCH_US: i64 = 1_790_263_445_000_000;

/// 受信側の観測の原点。メディア時刻 0 のフレームが揺らぎ無しで復号の出力へ出る時刻
const LOCAL_ORIGIN_US: i64 = 1_000_000;

/// 30 fps のフレーム間隔 (マイクロ秒)
const FRAME_US: i64 = 1_000_000 / 30;

/// デフォルト設定の時間軸を作る
fn create_timeline() -> PlayoutTimeline {
    PlayoutTimeline::new()
}

/// メディア時刻 (マイクロ秒) のフレームの TIMESTAMP (Unix epoch マイクロ秒)
fn timestamp_of(media_us: i64) -> i64 {
    EPOCH_US + media_us
}

/// 復号の出力を時間軸へ観測させる
///
/// `jitter_us` は経路と復号の揺らぎである。表示時刻は
/// 「メディア時刻 + 基準の遅れ + 表示の遅れ」で決まる。
fn observe_frame(timeline: &mut PlayoutTimeline, track: Track, media_us: i64, jitter_us: i64) {
    let local_us = LOCAL_ORIGIN_US + media_us + jitter_us;
    timeline.observe(track, EPOCH_US + local_us, timestamp_of(media_us));
}

/// 音声と映像の両方を観測する時間軸を作る
///
/// 音声を購読している購読と同じ構成にする (同期の制御が働き得る状態にする)。映像の表示の
/// 遅れは映像の揺らぎだけで決まり、音声の jitter buffer の遅延には引きずられない。
fn create_shared_timeline() -> PlayoutTimeline {
    let mut timeline = create_timeline();
    observe_frame(&mut timeline, Track::Audio, 0, 0);
    observe_frame(&mut timeline, Track::Video, 0, 0);
    timeline
}

/// フレームの表示時刻 (Unix epoch マイクロ秒)
fn presentation_us(buffer: &PlayoutBuffer<i64>, timeline: &PlayoutTimeline, media_us: i64) -> i64 {
    buffer
        .presentation_time_us(Some(timestamp_of(media_us)), timeline)
        .expect("観測済みのトラックの表示時刻が決まる")
}

/// 目標表示時刻に最も近いフレームを 1 枚描く
#[test]
fn select_nearest_draws_the_frame_nearest_to_the_target() {
    let mut timeline = create_shared_timeline();
    observe_frame(&mut timeline, Track::Video, FRAME_US, 0);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(0, Some(timestamp_of(0)), &timeline);
    buffer.enqueue(1, Some(timestamp_of(FRAME_US)), &timeline);
    let first_us = presentation_us(&buffer, &timeline, 0);
    let second_us = presentation_us(&buffer, &timeline, FRAME_US);

    // 目標表示時刻をちょうど 1 枚目の表示時刻にすると、1 枚目を描く
    let first = buffer.select_nearest(first_us, 1_000, MAX_PRESENTATION_LAG_US, &timeline);
    assert_eq!(first.draw, Some(0), "1 枚目を描くこと");
    assert!(first.dropped.is_empty(), "捨てるフレームは無いこと");
    assert_eq!(first.draw_presentation_us, Some(first_us));
    assert!(!first.draw_late, "目標どおりであること");
    // 目標表示時刻を 2 枚目の表示時刻にすると、2 枚目を描く
    let second = buffer.select_nearest(second_us, 1_000, MAX_PRESENTATION_LAG_US, &timeline);
    assert_eq!(second.draw, Some(1), "2 枚目を描くこと");
    assert_eq!(second.draw_presentation_us, Some(second_us));
}

/// 許容幅より古いフレームを捨て、目標に合う最新のフレームを描く
#[test]
fn select_nearest_drops_frames_older_than_the_tolerance_and_draws_the_latest() {
    let mut timeline = create_shared_timeline();
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    for index in 0..3i64 {
        observe_frame(&mut timeline, Track::Video, index * FRAME_US, 0);
        buffer.enqueue(index, Some(timestamp_of(index * FRAME_US)), &timeline);
    }
    // 3 枚目の表示時刻を目標にする。1 枚目と 2 枚目は許容幅 (1 ms) より古い
    let target_us = presentation_us(&buffer, &timeline, 2 * FRAME_US);
    let selection = buffer.select_nearest(target_us, 1_000, MAX_PRESENTATION_LAG_US, &timeline);
    assert_eq!(selection.draw, Some(2));
    assert_eq!(selection.dropped, vec![0, 1]);
    assert_eq!(selection.draw_presentation_us, Some(target_us));
}

/// 未来のフレームしかなければ描かず、古いフレームだけなら捨てる
#[test]
fn select_nearest_waits_for_future_frames_and_drops_old_frames() {
    let mut timeline = create_shared_timeline();
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    observe_frame(&mut timeline, Track::Video, FRAME_US, 0);
    buffer.enqueue(0, Some(timestamp_of(0)), &timeline);
    buffer.enqueue(1, Some(timestamp_of(FRAME_US)), &timeline);

    let first_us = presentation_us(&buffer, &timeline, 0);
    // 表示時刻より十分前なら、未来のフレームしかないため何も描かない
    let waiting = buffer.select_nearest(
        first_us - 100_000,
        1_000,
        MAX_PRESENTATION_LAG_US,
        &timeline,
    );
    assert_eq!(waiting.draw, None);
    assert!(waiting.dropped.is_empty());
    // 2 枚とも許容幅より古い目標なら、両方とも捨てる
    let second_us = presentation_us(&buffer, &timeline, FRAME_US);
    let old = buffer.select_nearest(
        second_us + 100_000,
        1_000,
        MAX_PRESENTATION_LAG_US,
        &timeline,
    );
    assert_eq!(old.draw, None);
    assert_eq!(old.dropped, vec![0, 1]);
}

/// 許容幅の中では目標に最も近いフレームを選ぶ
#[test]
fn select_nearest_chooses_the_nearest_frame_inside_the_tolerance() {
    let mut timeline = create_shared_timeline();
    observe_frame(&mut timeline, Track::Video, FRAME_US, 0);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(0, Some(timestamp_of(0)), &timeline);
    buffer.enqueue(1, Some(timestamp_of(FRAME_US)), &timeline);
    let first_us = presentation_us(&buffer, &timeline, 0);
    let second_us = presentation_us(&buffer, &timeline, FRAME_US);
    // 2 枚の表示時刻の中間より 1 ms だけ 1 枚目寄りの目標にする
    let target_us = (first_us + second_us) / 2 - 1_000;
    let selection =
        buffer.select_nearest(target_us, FRAME_US / 2, MAX_PRESENTATION_LAG_US, &timeline);
    assert_eq!(selection.draw, Some(0), "近い方のフレームを選ぶこと");
    assert!(selection.dropped.is_empty());
    // 続けて次の表示周期では 2 枚目を選ぶ
    let next = buffer.select_nearest(
        target_us + FRAME_US,
        FRAME_US / 2,
        MAX_PRESENTATION_LAG_US,
        &timeline,
    );
    assert_eq!(next.draw, Some(1));
}

/// 同じ目標表示時刻に入った 2 枚は 1 tick に 1 枚ずつ出す
#[test]
fn select_nearest_draws_one_frame_per_tick_when_two_frames_share_a_target() {
    let mut timeline = create_shared_timeline();
    observe_frame(&mut timeline, Track::Video, FRAME_US / 2, 0);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(0, Some(timestamp_of(0)), &timeline);
    buffer.enqueue(1, Some(timestamp_of(FRAME_US / 2)), &timeline);
    let first_us = presentation_us(&buffer, &timeline, 0);
    let second_us = presentation_us(&buffer, &timeline, FRAME_US / 2);
    let target_us = (first_us + second_us) / 2;
    let first = buffer.select_nearest(target_us, FRAME_US / 2, MAX_PRESENTATION_LAG_US, &timeline);
    assert!(first.draw.is_some(), "1 tick 目で 1 枚描くこと");
    let second = buffer.select_nearest(
        target_us + 1_000_000 / 60,
        FRAME_US / 2,
        MAX_PRESENTATION_LAG_US,
        &timeline,
    );
    assert!(second.draw.is_some(), "2 tick 目でもう 1 枚描くこと");
    assert_ne!(first.draw, second.draw, "同じフレームを 2 回出さないこと");
}

/// 許容幅の中でも、選ばれたフレームより前にあるフレームは提示できないため捨てる
#[test]
fn select_nearest_drops_window_frames_before_the_chosen_one() {
    let frame_us = 1_000_000 / 60;
    let mut timeline = create_shared_timeline();
    observe_frame(&mut timeline, Track::Video, frame_us, 0);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(0, Some(timestamp_of(0)), &timeline);
    buffer.enqueue(1, Some(timestamp_of(frame_us)), &timeline);
    let first_us = presentation_us(&buffer, &timeline, 0);
    let second_us = presentation_us(&buffer, &timeline, frame_us);
    let tolerance_us = frame_us / 2;
    // 2 枚目から 6.67 ms 前を目標にする。1 枚目は遅れ 10 ms で max_late (20 ms) 以内、
    // 許容幅 (8.33 ms) の外側。2 枚目が選ばれ、1 枚目は窓内だが古いため捨てられる
    let target_us = second_us - 6_670;
    assert!(first_us < target_us - tolerance_us);
    let selection =
        buffer.select_nearest(target_us, tolerance_us, MAX_PRESENTATION_LAG_US, &timeline);
    assert_eq!(selection.draw, Some(1));
    assert_eq!(selection.dropped, vec![0]);
}

/// 負の tolerance は呼び出し側の誤りとして debug ビルドでは停止する
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "tolerance_us must be non-negative")]
fn select_nearest_rejects_a_negative_tolerance_in_debug_builds() {
    let timeline = create_timeline();
    let mut buffer = PlayoutBuffer::<i64>::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(0, None, &timeline);
    let _ = buffer.select_nearest(0, -1, MAX_PRESENTATION_LAG_US, &timeline);
}

/// 負の遅れの上限は呼び出し側の誤りとして debug ビルドでは停止する
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "max_late_us must be non-negative")]
fn select_nearest_rejects_a_negative_lag_limit_in_debug_builds() {
    let timeline = create_timeline();
    let mut buffer = PlayoutBuffer::<i64>::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(0, None, &timeline);
    let _ = buffer.select_nearest(0, 0, -1, &timeline);
}

/// release ビルドでは、負の許容幅と負の遅れの上限を 0 として扱う
///
/// 負の値は呼び出し側の誤りであり debug ビルドでは `debug_assert!` で停止するが、
/// release ビルドではフレームを落としすぎないよう安全側へ丸める。
#[cfg(not(debug_assertions))]
#[test]
fn select_nearest_clamps_negative_arguments_in_release_builds() {
    // 負の許容幅は 0 として扱う。目標に最も近いフレームを選ぶ対象は変わらない
    let mut timeline = create_shared_timeline();
    observe_frame(&mut timeline, Track::Video, FRAME_US, 0);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(0, Some(timestamp_of(0)), &timeline);
    let first_us = presentation_us(&buffer, &timeline, 0);
    let selection = buffer.select_nearest(first_us, -1, MAX_PRESENTATION_LAG_US, &timeline);
    assert_eq!(
        selection.draw,
        Some(0),
        "負の許容幅でも同じフレームを選ぶこと"
    );
    assert!(!selection.draw_late);

    // 負の遅れの上限は 0 として扱う。目標より古いフレームは逃げ道を使えず捨てる
    let mut timeline = create_shared_timeline();
    observe_frame(&mut timeline, Track::Video, FRAME_US, 0);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(0, Some(timestamp_of(0)), &timeline);
    let first_us = presentation_us(&buffer, &timeline, 0);
    let dropped = buffer.select_nearest(first_us + 10_000, 0, -1, &timeline);
    assert_eq!(
        dropped.draw, None,
        "遅れてよい上限 0 では古いフレームを描かないこと"
    );
    assert_eq!(dropped.dropped, vec![0]);
}

/// 表示時刻から `max_late_us` より古いフレームは、逃げ道でも描かないこと
///
/// 時間軸の skew 予算テストは定常の表示の遅れ (ループ周期) を扱い、最悪値 (逃げ道の上限
/// 20 ms + ループ周期) はこの不変条件で固定する。
#[test]
fn select_nearest_never_draws_older_than_the_lag_limit() {
    for late_us in [0i64, 10_000, 19_999, 20_000, 20_001] {
        let mut timeline = create_shared_timeline();
        observe_frame(&mut timeline, Track::Video, FRAME_US, 0);
        let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
        buffer.enqueue(0, Some(timestamp_of(0)), &timeline);
        let frame_presentation_us = presentation_us(&buffer, &timeline, 0);
        let target_us = frame_presentation_us + late_us;
        let selection = buffer.select_nearest(target_us, 0, MAX_PRESENTATION_LAG_US, &timeline);
        if let Some(presentation_us) = selection.draw_presentation_us {
            assert!(
                presentation_us >= target_us.saturating_sub(MAX_PRESENTATION_LAG_US),
                "表示時刻から上限より古いフレームを描いた: presentation={presentation_us} target={target_us}"
            );
            assert_eq!(
                selection.draw_late,
                late_us > 0,
                "遅れて描いたかどうかが表示時刻との関係と一致しない: late_us={late_us}"
            );
        } else {
            assert!(
                late_us > MAX_PRESENTATION_LAG_US,
                "上限以内なのに描かなかった: late_us={late_us}"
            );
            assert_eq!(selection.dropped, vec![0]);
        }
    }
}

/// 遅れが 20 ms 以内なら遅れて描き、超えたら捨てる
#[test]
fn select_nearest_draws_a_frame_late_only_within_the_lag_limit() {
    let mut timeline = create_shared_timeline();
    observe_frame(&mut timeline, Track::Video, FRAME_US, 0);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(0, Some(timestamp_of(0)), &timeline);
    let frame_presentation_us = presentation_us(&buffer, &timeline, 0);
    let within = buffer.select_nearest(
        frame_presentation_us + 19_900,
        1_000,
        MAX_PRESENTATION_LAG_US,
        &timeline,
    );
    assert_eq!(within.draw, Some(0), "20 ms 以内は遅れて描くこと");
    assert!(within.draw_late, "遅れて描いたことを返すこと");

    let mut timeline = create_shared_timeline();
    observe_frame(&mut timeline, Track::Video, FRAME_US, 0);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(0, Some(timestamp_of(0)), &timeline);
    let frame_presentation_us = presentation_us(&buffer, &timeline, 0);
    let beyond = buffer.select_nearest(
        frame_presentation_us + 20_100,
        1_000,
        MAX_PRESENTATION_LAG_US,
        &timeline,
    );
    assert_eq!(beyond.draw, None, "20 ms を超えたら描かないこと");
    assert_eq!(beyond.dropped, vec![0]);
}

/// 目標に合うフレームが無く、遅れが上限以内なら 1 枚だけ遅れて描く
#[test]
fn select_nearest_draws_late_when_no_frame_matches_the_target() {
    let mut timeline = create_shared_timeline();
    observe_frame(&mut timeline, Track::Video, FRAME_US, 0);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(0, Some(timestamp_of(0)), &timeline);
    buffer.enqueue(1, Some(timestamp_of(FRAME_US)), &timeline);
    let second_us = presentation_us(&buffer, &timeline, FRAME_US);
    // 2 枚目の表示時刻を 3 ms 過ぎた目標にする。許容幅 1 ms には合わないが、遅れは 3 ms
    let selection =
        buffer.select_nearest(second_us + 3_000, 1_000, MAX_PRESENTATION_LAG_US, &timeline);
    assert_eq!(selection.draw, Some(1));
    assert!(selection.draw_late, "遅れて描いたことを返すこと");
    assert_eq!(selection.dropped, vec![0]);
}

/// TIMESTAMP を持たないフレームは届いた順に 1 枚ずつ描く
#[test]
fn select_nearest_draws_frames_without_timestamp_in_arrival_order() {
    let timeline = create_timeline();
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(0, None, &timeline);
    buffer.enqueue(1, None, &timeline);
    buffer.enqueue(2, None, &timeline);
    for expected in 0..3 {
        let selection = buffer.select_nearest(0, 1_000, MAX_PRESENTATION_LAG_US, &timeline);
        assert_eq!(selection.draw, Some(expected), "届いた順に描くこと");
        assert!(selection.dropped.is_empty());
        assert_eq!(selection.draw_presentation_us, None);
        assert!(!selection.draw_late);
    }
    let empty = buffer.select_nearest(0, 1_000, MAX_PRESENTATION_LAG_US, &timeline);
    assert_eq!(empty.draw, None);
}

/// 基準を取り直した後に残っているフレームは届いた順に描く
#[test]
fn select_nearest_draws_arrival_order_after_generation_change() {
    let mut timeline = create_timeline();
    observe_frame(&mut timeline, Track::Video, 0, 0);
    observe_frame(&mut timeline, Track::Video, FRAME_US, 0);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(0, Some(timestamp_of(0)), &timeline);
    buffer.enqueue(1, Some(timestamp_of(FRAME_US)), &timeline);
    // 1 時間前の TIMESTAMP のフレームが届き、基準を取り直す (世代が進む)
    let jumped_media_us = 2 * FRAME_US - 3_600_000_000;
    let now_us = EPOCH_US + LOCAL_ORIGIN_US + 2 * FRAME_US;
    timeline.observe(Track::Video, now_us, timestamp_of(jumped_media_us));
    buffer.enqueue(2, Some(timestamp_of(jumped_media_us)), &timeline);

    // 取り直す前に積んだフレームは新しい基準では表示時刻を決められないため、届いた順に描く
    for expected in [0, 1] {
        let selection = buffer.select_nearest(now_us, 0, MAX_PRESENTATION_LAG_US, &timeline);
        assert_eq!(selection.draw, Some(expected));
        assert_eq!(selection.draw_presentation_us, None);
    }
    // 取り直した後のフレームは、新しい基準の表示時刻を過ぎたら描く
    let jumped_presentation_us = presentation_us(&buffer, &timeline, jumped_media_us);
    let selection = buffer.select_nearest(
        jumped_presentation_us,
        1_000,
        MAX_PRESENTATION_LAG_US,
        &timeline,
    );
    assert_eq!(
        selection.draw,
        Some(2),
        "表示時刻を過ぎた新しいフレームを描くこと"
    );
    assert_eq!(selection.draw_presentation_us, Some(jumped_presentation_us));
}

/// 表示時刻は時間軸が決める
#[test]
fn presentation_time_comes_from_the_timeline() {
    // 音声は観測しない (同期の制御を働かせない)。基準の遅れは観測の時刻 (LOCAL_ORIGIN_US) に
    // なる。映像の表示の遅れは揺らぎ (0 ms) だけである
    let mut timeline = create_timeline();
    observe_frame(&mut timeline, Track::Video, 0, 0);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    assert_eq!(
        buffer.playout_delay_us(&timeline),
        Some(0),
        "映像の表示の遅れは自分の揺らぎ (0 ms) だけであること"
    );
    buffer.enqueue(0, Some(timestamp_of(0)), &timeline);
    let frame_presentation_us = presentation_us(&buffer, &timeline, 0);
    assert_eq!(frame_presentation_us, EPOCH_US + LOCAL_ORIGIN_US);
    // 時間軸が決めた表示時刻 (epoch) の前は描かず、目標に選ぶと描く
    let waiting = buffer.select_nearest(
        frame_presentation_us - 1,
        0,
        MAX_PRESENTATION_LAG_US,
        &timeline,
    );
    assert_eq!(waiting.draw, None);
    let selection =
        buffer.select_nearest(frame_presentation_us, 0, MAX_PRESENTATION_LAG_US, &timeline);
    assert_eq!(selection.draw, Some(0));
    assert_eq!(selection.draw_presentation_us, Some(frame_presentation_us));
}

/// 再生遅延は時間軸の学習結果へ追随する
#[test]
fn playout_delay_follows_the_timeline_learning() {
    let mut timeline = create_timeline();
    let buffer = PlayoutBuffer::<i64>::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    // 揺らぎ 0 のフレームを 1 秒分観測する
    for index in 0..30i64 {
        observe_frame(&mut timeline, Track::Video, index * FRAME_US, 0);
    }
    assert_eq!(
        buffer.playout_delay_us(&timeline),
        Some(0),
        "揺らぎ 0 なら再生遅延も 0 であること"
    );
    // 100 ms 遅れたフレームが 2 枚 (窓の百分位に入る) 届くと直ちに 100 ms にする
    observe_frame(&mut timeline, Track::Video, 30 * FRAME_US, 100_000);
    observe_frame(&mut timeline, Track::Video, 31 * FRAME_US, 100_000);
    assert_eq!(
        buffer.playout_delay_us(&timeline),
        Some(100_000),
        "揺らぎが増えたら直ちに上げること"
    );
}

/// キューの上限を超えたフレームは古い方から返し、clear ですべて返す
#[test]
fn enqueue_returns_overflow_from_oldest_and_clear_returns_all() {
    let timeline = create_timeline();
    let mut buffer = PlayoutBuffer::new(2, Track::Video);
    assert!(buffer.enqueue(1, None, &timeline).is_empty());
    assert!(buffer.enqueue(2, None, &timeline).is_empty());
    assert_eq!(
        buffer.enqueue(3, None, &timeline),
        vec![1],
        "古い方から返すこと"
    );
    assert_eq!(buffer.len(), 2);
    assert_eq!(buffer.clear(), vec![2, 3], "積んだ順に返すこと");
    assert!(buffer.is_empty());
}

/// 30 fps のフレームを 60 Hz の表示周期で、位相を変えても 1 枚ずつ落とさずに描く
#[test]
fn select_nearest_keeps_every_30fps_frame_at_60hz_with_any_phase() {
    let refresh_us = 1_000_000 / 60;
    for phase_us in [0, 4_000, 8_330] {
        let mut timeline = create_timeline();
        let frames = 60i64;
        let mut buffer = PlayoutBuffer::new(frames as usize, Track::Video);
        for index in 0..frames {
            let media_us = index * FRAME_US;
            observe_frame(&mut timeline, Track::Video, media_us, 0);
            buffer.enqueue(index, Some(timestamp_of(media_us)), &timeline);
        }
        let mut draws = Vec::new();
        let mut tick_us = EPOCH_US + LOCAL_ORIGIN_US + phase_us;
        while tick_us < EPOCH_US + LOCAL_ORIGIN_US + frames * FRAME_US + 100_000 {
            let selection =
                buffer.select_nearest(tick_us, refresh_us / 2, MAX_PRESENTATION_LAG_US, &timeline);
            if let Some(draw) = selection.draw {
                draws.push(draw);
            }
            tick_us += refresh_us;
        }
        assert_eq!(
            draws,
            (0..frames).collect::<Vec<i64>>(),
            "位相 {phase_us} us でフレームを落とさないこと"
        );
    }
}

/// 表示周期が揺れても、各フレームを表示時刻に最も近い tick へ描く
#[test]
fn select_nearest_draws_each_frame_at_the_nearest_tick() {
    let duration_us = 5_000_000i64;
    let phase_jitter_us = [0i64, 3_000, -3_000];
    for fps in [30i64, 60] {
        for refresh_hz in [60i64, 120] {
            let frame_us = 1_000_000 / fps;
            let refresh_us = 1_000_000 / refresh_hz;
            let frames = (duration_us / frame_us) as usize;
            let mut timeline = create_timeline();
            timeline.set_target_latency_ms(120);
            let mut buffer = PlayoutBuffer::new(frames, Track::Video);
            for index in 0..frames as i64 {
                let media_us = index * frame_us;
                observe_frame(&mut timeline, Track::Video, media_us, 0);
                buffer.enqueue(index, Some(timestamp_of(media_us)), &timeline);
            }
            // 表示周期の tick を位相 ±3 ms で作る
            let mut ticks = Vec::new();
            let mut index = 0i64;
            while (index * refresh_us) < duration_us + 500_000 {
                ticks.push(
                    EPOCH_US
                        + LOCAL_ORIGIN_US
                        + index * refresh_us
                        + phase_jitter_us[(index as usize) % phase_jitter_us.len()],
                );
                index += 1;
            }
            let mut draws: Vec<(i64, i64)> = Vec::new();
            for tick_us in &ticks {
                let selection = buffer.select_nearest(
                    *tick_us,
                    refresh_us / 2,
                    MAX_PRESENTATION_LAG_US,
                    &timeline,
                );
                if let Some(draw) = selection.draw {
                    draws.push((draw, *tick_us));
                }
            }
            if fps * 2 <= refresh_hz {
                // 表示周期に十分収まる fps では、すべてのフレームを 1 回ずつ出す
                assert_eq!(
                    draws.iter().map(|(index, _)| *index).collect::<Vec<i64>>(),
                    (0..frames as i64).collect::<Vec<i64>>(),
                    "{fps} fps / {refresh_hz} Hz でフレームを落とした"
                );
            } else {
                // 表示周期と近い、または速い fps では位相の揺れで間引かれる。順序は保つ
                assert!(
                    draws.len() > frames / 2,
                    "{fps} fps / {refresh_hz} Hz で出したフレームが少なすぎる: {}",
                    draws.len()
                );
                for pair in draws.windows(2) {
                    assert!(
                        pair[0].0 < pair[1].0,
                        "{fps} fps / {refresh_hz} Hz で順序が逆転した"
                    );
                }
            }
            // 各フレームの提示 tick が、その表示時刻に最も近い実際の tick であること
            for (draw, tick_us) in &draws {
                let frame_presentation_us = presentation_us(&buffer, &timeline, *draw * frame_us);
                let mut nearest_tick_us = ticks[0];
                for candidate_us in &ticks {
                    if (*candidate_us - frame_presentation_us).saturating_abs()
                        < (nearest_tick_us - frame_presentation_us).saturating_abs()
                    {
                        nearest_tick_us = *candidate_us;
                    }
                }
                assert!(
                    (*tick_us - nearest_tick_us).saturating_abs() <= 1_000,
                    "{fps} fps / {refresh_hz} Hz: tick={tick_us} nearest={nearest_tick_us} presentation={frame_presentation_us}"
                );
            }
        }
    }
}
