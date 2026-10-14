//! 配信側の音声の追いつきのプロパティテスト
//!
//! 任意の観測列に対して、「キューがはけて読み出しが健全になれば、必ず投入を再開する」こと
//! と、「捨てたフレームは符号化へ渡らない」ことを検証する。追いつきの間は投入しないため
//! 符号化の出力も返らない。集計がずれたまま残ると解除できず、音声が一切送られなくなる。

use std::cell::Cell;

use pbt::common::test_runner;
use shiguredo_moqt::playout::publish_catch_up::{
    AudioPublishCatchUp, AudioPublishCatchUpChunk, AudioPublishCatchUpFrame,
};

/// TIMESTAMP に足している補正 (マイクロ秒)
const APPLIED_OFFSET_US: i64 = 1_000_000;

/// 読み出しの遅れ `read_lag_ms` で 20 ms のフレームを読む
fn frame(now_ms: i64, timestamp_us: i64, read_lag_ms: i64) -> AudioPublishCatchUpFrame {
    AudioPublishCatchUpFrame {
        now_ms,
        timestamp_us,
        read_wall_clock_us: timestamp_us + APPLIED_OFFSET_US + read_lag_ms * 1_000,
        applied_offset_us: Some(APPLIED_OFFSET_US),
        duration_us: Some(20_000),
    }
}

/// 符号化の出力を返す (長さの分だけキューを消費する)
fn encode(catch_up: &mut AudioPublishCatchUp, timestamp_us: i64, duration_us: i64) {
    catch_up.record_encoded_chunk(AudioPublishCatchUpChunk {
        timestamp_us,
        duration_us: Some(duration_us),
    });
}

/// キューがはけて読み出しが健全になれば、必ず投入を再開すること
#[test]
fn the_catch_up_always_ends_when_the_queue_is_drained() -> noprop::TestResult {
    // 追いつきを始めた場合を通ったことを数える
    let catching_seen = Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut catch_up = AudioPublishCatchUp::new();
        let mut now_ms = 0i64;
        let steps = noprop::sample_usize_in(ctx, 1..=100);
        for _ in 0..steps {
            now_ms += 20;
            // 読み出しの遅れは健全な値・上限の前後・大きな遅れを混ぜる
            let read_lag_ms = noprop::sample_with_boundaries(
                ctx,
                &[0, 10, 60, 100, 500],
                noprop::Ratio::one_nth(4),
                |ctx| noprop::sample_usize_in(ctx, 0..=600) as i64,
            ) as i64;
            let timestamp_us = now_ms * 1_000;
            if catch_up.evaluate(frame(now_ms, timestamp_us, read_lag_ms)) {
                // 出力がすぐ返るかどうかを選ぶ (返らなければキューが伸びる)
                if noprop::sample_bool(ctx) {
                    encode(&mut catch_up, timestamp_us, 20_000);
                }
            } else {
                assert_eq!(
                    catch_up.snapshot().dropped_ms % 20,
                    0,
                    "捨てた長さは 20 ms の倍数であること"
                );
            }
        }
        let stats = catch_up.snapshot();
        if stats.catching_up {
            catching_seen.set(catching_seen.get() + 1);
        }

        // キューをはけさせ、読み出しを健全にする。追いつきの最中でも必ず再開する
        let pending_ms = stats.pending_ms;
        encode(&mut catch_up, 0, pending_ms * 1_000);
        now_ms += 20;
        assert!(
            catch_up.evaluate(frame(now_ms, now_ms * 1_000, 10)),
            "キューをはけさせて読み出しが健全になったら必ず再開すること: {stats:?}"
        );
        let stats = catch_up.snapshot();
        assert!(
            !stats.catching_up,
            "追いつきを終えること: pending_ms={} pending_frames={}",
            stats.pending_ms, stats.pending_frames
        );
        assert_eq!(stats.pending_ms, 20, "投入した 1 フレームだけが残ること");
        Ok(())
    })?;
    Ok(())
}

/// 観測した回数が、符号化した回数と捨てた回数の和に一致すること
#[test]
fn every_frame_is_either_encoded_or_dropped() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut catch_up = AudioPublishCatchUp::new();
        let mut now_ms = 0i64;
        let mut encoded = 0u64;
        let steps = noprop::sample_usize_in(ctx, 1..=200);
        for _ in 0..steps {
            now_ms += 20;
            let timestamp_us = now_ms * 1_000;
            let read_lag_ms = noprop::sample_with_boundaries(
                ctx,
                &[0, 10, 60, 100, 500],
                noprop::Ratio::one_nth(4),
                |ctx| noprop::sample_usize_in(ctx, 0..=600) as i64,
            ) as i64;
            if catch_up.evaluate(frame(now_ms, timestamp_us, read_lag_ms)) {
                encoded += 1;
                // 出力が返るかどうかを選ぶ
                if noprop::sample_bool(ctx) {
                    encode(&mut catch_up, timestamp_us, 20_000);
                }
            }
        }
        let stats = catch_up.snapshot();
        assert_eq!(
            encoded + stats.dropped_frames,
            steps as u64,
            "すべてのフレームが符号化か棄却のどちらかであること: {stats:?}"
        );
        assert_eq!(
            stats.dropped_ms,
            stats.dropped_frames as i64 * 20,
            "捨てた長さが数と一致すること: {stats:?}"
        );
        Ok(())
    })?;
    Ok(())
}
