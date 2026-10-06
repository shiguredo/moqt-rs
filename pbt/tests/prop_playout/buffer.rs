//! 表示時刻に合わせてフレームを選ぶキューのプロパティテスト
//!
//! 任意の観測列と到着列で、選んだフレームと捨てたフレームと残ったフレームの合計が
//! 積んだ数と一致し、描く順が積んだ順のままであることを検証する。

use pbt::common::test_runner;
use shiguredo_moqt::playout::buffer::{JITTER_BUFFER_MAX_QUEUED_FRAMES, PlayoutBuffer};
use shiguredo_moqt::playout::timeline::{PlayoutTimeline, TimelineConfig, Track};

/// 任意の到着列で、フレームの数が保存され、描く順が積んだ順のままである
#[test]
fn selection_conserves_frames_and_keeps_order() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let config = TimelineConfig {
            target_latency_ms: noprop::sample_usize_in(ctx, 0..200) as i64,
            ..TimelineConfig::default()
        };
        let mut timeline = PlayoutTimeline::with_config(config);
        let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
        let mut timestamp_us = 1_000_000;
        let mut now_us = 10_000_000;
        let mut enqueued = 0;
        let mut overflow = 0;
        let mut drawn = Vec::new();
        let mut late = 0;
        for _ in 0..noprop::sample_usize_in(ctx, 1..64) {
            let interval_us = noprop::sample_usize_in(ctx, 1..100_000) as i64;
            timestamp_us += interval_us;
            // 到着は TIMESTAMP の間隔に揺らぎを足したものにする
            now_us += interval_us + noprop::sample_usize_in(ctx, 0..50_000) as i64;
            timeline.observe(Track::Video, now_us, timestamp_us);
            overflow += buffer
                .enqueue(timestamp_us, Some(timestamp_us), &timeline)
                .len();
            enqueued += 1;
            let selection = buffer.select(now_us, &timeline);
            if let Some(item) = selection.draw {
                drawn.push(item);
            }
            late += selection.late.len();
            if let Some(presentation_us) = selection.draw_presentation_us {
                assert!(
                    presentation_us <= now_us,
                    "now_us={now_us} presentation_us={presentation_us}"
                );
            }
        }
        assert_eq!(
            enqueued,
            drawn.len() + late + buffer.len() + overflow,
            "積んだ数と描いた数と捨てた数と残った数が一致すること"
        );
        for pair in drawn.windows(2) {
            assert!(pair[0] < pair[1], "drawn={drawn:?}");
        }
        Ok(())
    })?;
    Ok(())
}
