//! 表示時刻に合わせてフレームを選ぶキューのテスト
//!
//! 公開 API (`PlayoutBuffer`) の契約を確認する。

use shiguredo_moqt::playout::buffer::{
    JITTER_BUFFER_MAX_QUEUED_FRAMES, MAX_PRESENTATION_LAG_US, PlayoutBuffer,
};
use shiguredo_moqt::playout::timeline::{PlayoutTimeline, TimelineConfig, Track};

/// 表示の遅れを固定した時間軸を作る
///
/// `targetLatency` を下限に使うため、自分の揺らぎの学習が 0 でも表示の遅れが決まる。
fn timeline_with_delay(target_latency_ms: i64) -> PlayoutTimeline {
    PlayoutTimeline::with_config(TimelineConfig {
        target_latency_ms,
        ..TimelineConfig::default()
    })
}

/// 壁時計と TIMESTAMP の差を固定して映像を 1 つ観測する
///
/// これで `TIMESTAMP` の表示時刻が `wall_clock_us + target_latency` になる。
fn observe_video(timeline: &mut PlayoutTimeline, timestamp_us: i64, wall_clock_us: i64) {
    timeline.observe(Track::Video, wall_clock_us, timestamp_us);
}

/// 保持していないときは何も描かない
#[test]
fn select_returns_none_when_empty() {
    let (timeline, now_us) = (timeline_with_delay(100), 10_000_000);
    let mut buffer = PlayoutBuffer::<i64>::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    let selection = buffer.select(now_us, &timeline);
    assert!(selection.draw.is_none(), "空なら描かないこと");
    assert!(selection.late.is_empty(), "捨てるフレームも無いこと");
    assert_eq!(selection.draw_presentation_us, None, "表示時刻も無いこと");
}

/// 表示時刻まで待ってから描く
#[test]
fn select_waits_until_presentation_time() {
    let mut timeline = timeline_with_delay(100);
    let timestamp_us = 9_000_000;
    let now_us = 10_000_000;
    observe_video(&mut timeline, timestamp_us, now_us);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    assert!(
        buffer.enqueue(1, Some(timestamp_us), &timeline).is_empty(),
        "上限に収まること"
    );
    assert!(
        buffer.select(now_us, &timeline).draw.is_none(),
        "表示時刻の前は描かないこと"
    );
    assert!(
        buffer
            .select(now_us + 100_000 - 1, &timeline)
            .draw
            .is_none(),
        "表示時刻の直前は描かないこと"
    );
    let selection = buffer.select(now_us + 100_000, &timeline);
    assert_eq!(selection.draw, Some(1), "表示時刻になったら描くこと");
    assert_eq!(
        selection.draw_presentation_us,
        Some(now_us + 100_000),
        "描くフレームの表示時刻を返すこと"
    );
}

/// 表示時刻を決められないフレームは届いた順に 1 枚ずつ描く
#[test]
fn select_draws_arrival_order_without_timestamp() {
    let mut timeline = timeline_with_delay(100);
    observe_video(&mut timeline, 9_000_000, 10_000_000);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(1, None, &timeline);
    let selection = buffer.select(10_000_000, &timeline);
    assert_eq!(selection.draw, Some(1), "その場で描くこと");
    assert_eq!(
        selection.draw_presentation_us, None,
        "表示時刻は返さないこと"
    );
}

/// 表示時刻を過ぎたフレームが 2 枚以上あるときは、最新を残して 1 つ前を描く
#[test]
fn select_draws_previous_frame_when_two_are_due() {
    let mut timeline = timeline_with_delay(100);
    let now_us = 10_000_000;
    let first_us = 9_000_000;
    let second_us = first_us + 10_000;
    observe_video(&mut timeline, first_us, now_us);
    observe_video(&mut timeline, second_us, now_us + 10_000);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(1, Some(first_us), &timeline);
    buffer.enqueue(2, Some(second_us), &timeline);
    // 2 枚とも表示時刻を過ぎているが、遅れは上限の中にある
    let selection = buffer.select(now_us + 110_000, &timeline);
    assert_eq!(selection.draw, Some(1), "最新を残して 1 つ前を描くこと");
    assert!(selection.late.is_empty(), "捨てるフレームは無いこと");
    let next = buffer.select(now_us + 110_000, &timeline);
    assert_eq!(next.draw, Some(2), "次は残した最新を描くこと");
}

/// 表示時刻から上限を超えて遅れたフレームは捨てる
#[test]
fn select_drops_frame_that_is_too_late() {
    let mut timeline = timeline_with_delay(100);
    let now_us = 10_000_000;
    let first_us = 9_000_000;
    let second_us = first_us + 33_333;
    observe_video(&mut timeline, first_us, now_us);
    observe_video(&mut timeline, second_us, now_us + 33_333);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(1, Some(first_us), &timeline);
    buffer.enqueue(2, Some(second_us), &timeline);
    // 1 枚目の表示時刻は 2 枚目の表示時刻より 33.3 ms 前であり、上限 (20 ms) を超える
    let selection = buffer.select(now_us + 133_333, &timeline);
    assert_eq!(selection.draw, Some(2), "最新を描くこと");
    assert_eq!(selection.late, vec![1], "遅れすぎたフレームを捨てること");
    assert!(
        now_us + 133_333 - (now_us + 100_000) > MAX_PRESENTATION_LAG_US,
        "上限を超える遅れであること"
    );
}

/// 上限を超えたフレームは古い方から返す
#[test]
fn enqueue_returns_overflow_from_oldest() {
    let mut timeline = timeline_with_delay(100);
    observe_video(&mut timeline, 9_000_000, 10_000_000);
    let mut buffer = PlayoutBuffer::new(2, Track::Video);
    assert!(buffer.enqueue(1, None, &timeline).is_empty());
    assert!(buffer.enqueue(2, None, &timeline).is_empty());
    assert_eq!(
        buffer.enqueue(3, None, &timeline),
        vec![1],
        "古い方から返すこと"
    );
    assert_eq!(buffer.len(), 2, "上限を超えないこと");
}

/// 基準を取り直したあとに残っているフレームは届いた順に描く
#[test]
fn select_draws_arrival_order_after_generation_change() {
    let mut timeline = timeline_with_delay(100);
    let timestamp_us = 9_000_000;
    let now_us = 10_000_000;
    observe_video(&mut timeline, timestamp_us, now_us);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(1, Some(timestamp_us), &timeline);
    // 基準を取り直すと世代が進み、積んだときの世代と変わる
    timeline.reset();
    assert_eq!(timeline.generation(), 1, "世代が進むこと");
    let selection = buffer.select(now_us, &timeline);
    assert_eq!(
        selection.draw,
        Some(1),
        "表示時刻を決められないフレームとして描くこと"
    );
}

/// 表示の遅れは時間軸から読む
#[test]
fn playout_delay_is_read_from_timeline() {
    let mut timeline = timeline_with_delay(100);
    let buffer = PlayoutBuffer::<i64>::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    assert_eq!(
        buffer.playout_delay_us(&timeline),
        None,
        "観測が無ければ遅れも無いこと"
    );
    observe_video(&mut timeline, 9_000_000, 10_000_000);
    assert_eq!(
        buffer.playout_delay_us(&timeline),
        Some(100_000),
        "表示の遅れを返すこと"
    );
}

/// 保持しているフレームをすべて取り出せる
#[test]
fn clear_returns_all_frames() {
    let mut timeline = timeline_with_delay(100);
    observe_video(&mut timeline, 9_000_000, 10_000_000);
    let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    buffer.enqueue(1, None, &timeline);
    buffer.enqueue(2, None, &timeline);
    assert_eq!(buffer.clear(), vec![1, 2], "積んだ順に返すこと");
    assert!(buffer.is_empty(), "取り出したら空になること");
}
