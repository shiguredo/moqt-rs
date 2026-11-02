//! 表示時刻に合わせてフレームを選ぶキューのプロパティテスト
//!
//! moqt-js の `src/playout/buffer.prop.ts` の意図を、マイクロ秒の整数で動く
//! [`PlayoutBuffer`] に対して固定する。任意の到着列と選択の列に対して、次の性質を確かめる。
//!
//! - フレームを並べ替えない (描く・捨てる順は積んだ順であり、各フレームは 1 回だけ出る)
//! - 描くフレームは目標表示時刻から許容幅以内、または遅れてよい上限以内であり、遅れたときだけ
//!   `draw_late` が真になる
//! - 描いたフレームの表示時刻は、目標表示時刻から遅れてよい上限より古くない
//! - 揺らぎの p95 が最大の到着列でも、表示間隔が半分にならない

use pbt::common::test_runner;
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

/// メディア時刻 (マイクロ秒) のフレームの TIMESTAMP (Unix epoch マイクロ秒)
fn timestamp_of(media_us: i64) -> i64 {
    EPOCH_US + media_us
}

/// 任意の操作の列で、描く・捨てる順が積んだ順のままであり、各フレームが 1 回だけ出ること
#[test]
fn selection_conserves_frames_and_keeps_order() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        // 壁時計の TIMESTAMP を持つ場合と持たない場合を試す
        let with_timestamp = noprop::sample_bool(ctx);
        let mut timeline = PlayoutTimeline::new();
        let max_queued_frames = noprop::sample_usize_in(ctx, 1..=JITTER_BUFFER_MAX_QUEUED_FRAMES);
        let mut buffer = PlayoutBuffer::new(max_queued_frames, Track::Video);
        let mut removed: Vec<u64> = Vec::new();
        let mut enqueued = 0u64;
        let mut local_us = LOCAL_ORIGIN_US;
        let mut media_us = 0i64;
        for _ in 0..noprop::sample_usize_in(ctx, 1..=80) {
            let enqueue = noprop::sample_bool(ctx);
            if enqueue {
                media_us += FRAME_US;
                // 復号の出力の時刻は前のフレームより早くならない
                local_us = (local_us + FRAME_US).max(LOCAL_ORIGIN_US + media_us);
                if with_timestamp {
                    timeline.observe(Track::Video, EPOCH_US + local_us, timestamp_of(media_us));
                    removed.extend(buffer.enqueue(
                        enqueued,
                        Some(timestamp_of(media_us)),
                        &timeline,
                    ));
                } else {
                    removed.extend(buffer.enqueue(enqueued, None, &timeline));
                }
                enqueued += 1;
                assert!(
                    buffer.len() <= max_queued_frames,
                    "キューの上限を超えた: len={} max={max_queued_frames}",
                    buffer.len()
                );
            } else {
                local_us += noprop::sample_usize_in(ctx, 0..=200_000) as i64;
                let target_us = EPOCH_US + local_us;
                // 負の値は呼び出し側の誤りである (debug ビルドでは debug_assert! で停止する)
                // ため、ここでは非負の値だけを使う
                let tolerance_us = noprop::sample_usize_in(ctx, 0..=FRAME_US as usize) as i64;
                let max_late_us =
                    noprop::sample_usize_in(ctx, 0..=MAX_PRESENTATION_LAG_US as usize) as i64;
                let selection =
                    buffer.select_nearest(target_us, tolerance_us, max_late_us, &timeline);
                removed.extend(selection.dropped);
                if let Some(draw) = selection.draw {
                    removed.push(draw);
                }
            }
        }
        removed.extend(buffer.clear());
        assert_eq!(
            removed,
            (0..enqueued).collect::<Vec<u64>>(),
            "描く・捨てる順が積んだ順のままではない"
        );
        Ok(())
    })?;
    Ok(())
}

/// 描くフレームは目標表示時刻から許容幅以内、または遅れてよい上限以内であり、遅れた
/// ときだけ `draw_late` が真になること
#[test]
fn drawn_frames_stay_within_the_tolerance_or_the_lag_limit() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let mut timeline = PlayoutTimeline::new();
        let mut buffer = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
        let mut enqueued = 0u64;
        let mut local_us = LOCAL_ORIGIN_US;
        for _ in 0..noprop::sample_usize_in(ctx, 1..=60) {
            let enqueue = noprop::sample_bool(ctx);
            if enqueue {
                let media_us = enqueued as i64 * FRAME_US;
                // 到着の揺らぎ (§0〜40 ms) を混ぜる。基準の取り直しは起こさない
                let jitter_us = noprop::sample_usize_in(ctx, 0..=40_000) as i64;
                local_us = (local_us + FRAME_US).max(LOCAL_ORIGIN_US + media_us + jitter_us);
                timeline.observe(Track::Video, EPOCH_US + local_us, timestamp_of(media_us));
                buffer.enqueue(
                    local_us - LOCAL_ORIGIN_US,
                    Some(timestamp_of(media_us)),
                    &timeline,
                );
                enqueued += 1;
            } else {
                local_us += noprop::sample_usize_in(ctx, 0..=200_000) as i64;
                let target_us = EPOCH_US + local_us;
                let tolerance_us = noprop::sample_usize_in(ctx, 0..=FRAME_US as usize) as i64;
                let max_late_us =
                    noprop::sample_usize_in(ctx, 0..=MAX_PRESENTATION_LAG_US as usize) as i64;
                let selection =
                    buffer.select_nearest(target_us, tolerance_us, max_late_us, &timeline);
                if let Some(presentation_us) = selection.draw_presentation_us {
                    // 逃げ道でも、目標から遅れてよい上限より古いフレームは描かない
                    assert!(
                        presentation_us >= target_us.saturating_sub(max_late_us),
                        "遅れてよい上限より古いフレームを描いた: presentation={presentation_us} target={target_us} max_late={max_late_us}"
                    );
                    let distance_us = presentation_us.saturating_sub(target_us).saturating_abs();
                    if selection.draw_late {
                        assert!(
                            presentation_us < target_us.saturating_sub(tolerance_us),
                            "遅れたと返したフレームが許容幅の中にある: presentation={presentation_us} target={target_us} tolerance={tolerance_us}"
                        );
                        assert!(
                            distance_us <= max_late_us,
                            "遅れてよい上限を超えたフレームを描いた: distance={distance_us} max_late={max_late_us}"
                        );
                    } else {
                        assert!(
                            distance_us <= tolerance_us,
                            "許容幅を超えたフレームを遅れずに描いた: distance={distance_us} tolerance={tolerance_us}"
                        );
                    }
                }
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// 揺らぎの p95 が最大の到着列でも、表示間隔が半分にならず、フレームを捨てないこと
///
/// moqt-js の `buffer.prop.ts` の「p95 が最大の揺らぎでも表示間隔が半分にならない」に
/// 対応する。3 枚に 1 枚が遅れて届き、復号の出力もその分だけ後ろへずれた到着列を作り、
/// 1 ms ごとの選択で表示周期ごとに 1 枚ずつ選ばれることを確かめる。
#[test]
fn jittery_arrivals_do_not_halve_the_presentation_interval() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(16, |ctx| {
        let jitter_us = noprop::sample_usize_in(ctx, 5_000..=150_000) as i64;
        let frames = noprop::sample_usize_in(ctx, 100..=160) as i64;
        let mut timeline = PlayoutTimeline::new();
        let mut buffer = PlayoutBuffer::new(frames as usize, Track::Video);
        // 3 枚に 1 枚が `jitter_us` 遅れて届く。復号は順に行うため、遅れは 1 フレームずつ
        // 解消される (moqt-js の buffer.test.ts と同じ到着列の作り方)
        let mut arrivals: Vec<(i64, i64)> = Vec::new();
        let mut delayed_us = 0i64;
        for index in 0..frames {
            delayed_us = (delayed_us - FRAME_US).max(if index % 3 == 2 { jitter_us } else { 0 });
            arrivals.push((
                LOCAL_ORIGIN_US + index * FRAME_US + delayed_us,
                index * FRAME_US,
            ));
        }
        let mut draws: Vec<(i64, i64)> = Vec::new();
        let mut dropped_after_warmup = 0usize;
        let mut next = 0usize;
        let mut tick_us = 0i64;
        let end_us = frames * FRAME_US + 100_000;
        while tick_us < end_us {
            while next < arrivals.len() && arrivals[next].0 - LOCAL_ORIGIN_US <= tick_us {
                let (local_us, media_us) = arrivals[next];
                timeline.observe(Track::Video, EPOCH_US + local_us, timestamp_of(media_us));
                buffer.enqueue(next as i64, Some(timestamp_of(media_us)), &timeline);
                next += 1;
            }
            let selection = buffer.select_nearest(
                EPOCH_US + tick_us,
                FRAME_US / 4,
                MAX_PRESENTATION_LAG_US,
                &timeline,
            );
            if next >= 30 && !selection.dropped.is_empty() {
                dropped_after_warmup += selection.dropped.len();
            }
            if let Some(draw) = selection.draw
                && draw >= 30
            {
                draws.push((draw, tick_us));
            }
            tick_us += 1_000;
        }
        assert!(
            draws.len() > 30,
            "選択の列が短すぎる: draws={}",
            draws.len()
        );
        for pair in draws.windows(2) {
            assert_eq!(
                pair[1].0 - pair[0].0,
                1,
                "表示の順序が飛んだ: {:?} -> {:?}",
                pair[0],
                pair[1]
            );
            let interval_us = pair[1].1 - pair[0].1;
            // 選択の刻みは 1 ms で、許容幅 (frameMs / 4) の分だけ前後し得る
            assert!(
                interval_us >= FRAME_US - 9_000,
                "表示間隔が半分になった: interval_us={interval_us}"
            );
            assert!(
                interval_us <= FRAME_US + 9_000,
                "表示間隔が 1 フレームを超えた: interval_us={interval_us}"
            );
        }
        assert_eq!(
            dropped_after_warmup, 0,
            "揺らぎの定常状態でフレームを捨てた"
        );
        // 揺らぎは µs に丸めた TIMESTAMP の差から求めるため、1 ms の丸めを許す
        let playout_delay_us = buffer.playout_delay_us(&timeline).unwrap_or(-1);
        assert!(
            (playout_delay_us - jitter_us).saturating_abs() <= 1_000,
            "再生遅延が揺らぎと一致しない: {playout_delay_us} jitter={jitter_us}"
        );
        Ok(())
    })?;
    Ok(())
}
