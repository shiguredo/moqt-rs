//! 表示時刻に合わせてフレームを選ぶキューのプロパティテスト
//!
//! 任意の観測列と到着列で、選んだフレームと捨てたフレームと残ったフレームの合計が
//! 積んだ数と一致し、描く順が積んだ順のままであることを検証する。

use std::cell::Cell;

use pbt::common::test_runner;
use shiguredo_moqt::playout::buffer::{
    JITTER_BUFFER_MAX_QUEUED_FRAMES, MAX_PRESENTATION_LAG_US, PlayoutBuffer,
};
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

/// 表示時刻を過ぎたフレームが複数あるときも、1 回の選択で描くのは 1 枚だけであり、
/// 遅れすぎたフレームだけを捨て、最新の 1 枚は必ず残すこと
///
/// 期待値は公開 API (`presentation_time_us`) から独立に組み立て、選んだフレーム・
/// 捨てたフレーム・残ったフレームがそのとおりになることを確かめる。
#[test]
fn selection_draws_one_frame_and_drops_only_too_late_frames() -> noprop::TestResult {
    let one_frame_seen = Cell::new(0usize);
    let dropped_seen = Cell::new(0usize);
    let all_late_seen = Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut timeline = PlayoutTimeline::new();
        // 1 枚目の復号の出力を基準にする (表示の遅れは 0 ms)
        let first_timestamp_us = 1_000_000;
        timeline.observe(Track::Video, 10_000_000, first_timestamp_us);
        let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
        let count = noprop::sample_usize_in(ctx, 1..=6);
        let interval_us = noprop::sample_usize_in(ctx, 1..=30_000) as i64;
        let mut timestamps_us = Vec::new();
        for index in 0..count {
            let timestamp_us = first_timestamp_us + index as i64 * interval_us;
            buffer.enqueue(index as u32, Some(timestamp_us), &timeline);
            timestamps_us.push(timestamp_us);
        }
        // 最新のフレームの表示時刻から 0〜60 ms 後に選ぶ。遅れすぎたフレームが出る場合と
        // 出ない場合の両方を通す
        let presentations_us: Vec<i64> = timestamps_us
            .iter()
            .map(|timestamp_us| {
                buffer
                    .presentation_time_us(Some(*timestamp_us), &timeline)
                    .expect("観測済みのトラックの表示時刻が決まる")
            })
            .collect();
        let newest_presentation_us = *presentations_us
            .last()
            .expect("1 枚以上は積んでいる");
        let select_at_us =
            newest_presentation_us + noprop::sample_usize_in(ctx, 0..=60_000) as i64;
        let selection = buffer.select(select_at_us, &timeline);

        // 期待値を独立に組み立てる
        let mut last_due_index = None;
        for (index, presentation_us) in presentations_us.iter().enumerate() {
            if *presentation_us <= select_at_us {
                last_due_index = Some(index);
            }
        }
        let Some(last_due_index) = last_due_index else {
            assert!(selection.draw.is_none(), "表示時刻前のフレームを描いた");
            assert!(selection.late.is_empty());
            assert_eq!(buffer.len(), count);
            return Ok(());
        };
        let too_late_before_us = select_at_us.saturating_sub(MAX_PRESENTATION_LAG_US);
        let mut late_count = 0;
        while late_count < last_due_index && presentations_us[late_count] < too_late_before_us {
            late_count += 1;
        }
        assert_eq!(
            selection.draw,
            Some(late_count as u32),
            "描くのは遅れすぎた分を捨てた直後の 1 枚: presentations={presentations_us:?} select_at_us={select_at_us}"
        );
        assert_eq!(
            selection.late,
            (0..late_count as u32).collect::<Vec<u32>>(),
            "捨てるのは遅れすぎたフレームだけ: presentations={presentations_us:?} select_at_us={select_at_us}"
        );
        assert_eq!(
            selection.draw_presentation_us,
            Some(presentations_us[late_count])
        );
        assert_eq!(
            buffer.len(),
            count - late_count - 1,
            "残るのは描いた 1 枚より後のフレームだけ"
        );
        if late_count == 0 {
            one_frame_seen.set(one_frame_seen.get() + 1);
        } else {
            dropped_seen.set(dropped_seen.get() + 1);
        }
        if late_count == last_due_index {
            all_late_seen.set(all_late_seen.get() + 1);
        }
        Ok(())
    })?;
    assert!(
        one_frame_seen.get() > 0,
        "捨てずに 1 枚だけ描く場合を通っていない\n{runner}"
    );
    assert!(
        dropped_seen.get() > 0,
        "遅れすぎたフレームを捨てる場合を通っていない\n{runner}"
    );
    assert!(
        all_late_seen.get() > 0,
        "表示時刻を過ぎたフレームがすべて遅れすぎの場合を通っていない\n{runner}"
    );
    Ok(())
}
