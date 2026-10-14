//! 受信側の音声の追いつきのプロパティテスト
//!
//! 任意の観測列に対して、遅れが上限の中へ戻れば必ず追いつきが終わること、追いつきを
//! 始めた間隔がクールダウン以上であること、上限が下限以上であること、TIMESTAMP の跳びが
//! 無ければ飛んだ音を数えないことを検証する。分岐ごとに到達を数え、検証が空振りして
//! いないことも確かめる。

use std::cell::Cell;

use pbt::common::test_runner;
use shiguredo_moqt::playout::catch_up::{
    AUDIO_RECEIVE_CATCH_UP_COOLDOWN_MS, AUDIO_RECEIVE_CATCH_UP_MIN_MS, AudioReceiveCatchUp,
    AudioReceiveCatchUpObservation,
};

/// 遅れが上限へ戻れば必ず追いつきが終わり、始めた間隔がクールダウン以上であること
#[test]
fn the_catch_up_always_ends_and_respects_the_cooldown() -> noprop::TestResult {
    // 追いつきを始めた場合と、遅れが上限へ戻った場合の両方を通ったことを数える
    let started_seen = Cell::new(0usize);
    let ended_seen = Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut catch_up = AudioReceiveCatchUp::new();
        let mut now_ms = 0i64;
        let mut timestamp_us = 0i64;
        let mut started_at_ms: Option<i64> = None;
        let records = noprop::sample_usize_in(ctx, 1..=200);
        for _ in 0..records {
            now_ms = now_ms.saturating_add(noprop::sample_with_boundaries(
                ctx,
                &[20, 100, 250, 1_000],
                noprop::Ratio::one_nth(4),
                |ctx| noprop::sample_usize_in(ctx, 1..=2_000) as i64,
            ));
            // 遅れは健全な値・上限の前後・大きな跳びを混ぜる
            let lag_ms = noprop::sample_with_boundaries(
                ctx,
                &[0, 20, 60, 100, 200, 500],
                noprop::Ratio::one_nth(4),
                |ctx| noprop::sample_usize_in(ctx, 0..=800) as i64,
            ) as i64;
            let duration_ms = if noprop::sample_bool(ctx) { 20 } else { 10 };
            // 復号の出力の TIMESTAMP は投入の順に 1 音ずつ進む (跳びが無い)
            timestamp_us = timestamp_us.saturating_add(duration_ms * 1_000);

            let decision = catch_up.observe(AudioReceiveCatchUpObservation {
                now_ms,
                lag_ms,
                duration_ms,
                timestamp_us,
            });
            let stats = catch_up.snapshot();

            assert!(
                stats.limit_ms >= AUDIO_RECEIVE_CATCH_UP_MIN_MS,
                "上限が下限を下回った: {stats:?}"
            );
            assert!(
                stats.skipped_frames == 0,
                "跳びが無いのに飛んだ音を数えた: {stats:?}"
            );
            if decision.rebuild_decoder {
                if let Some(previous_ms) = started_at_ms {
                    assert!(
                        now_ms.saturating_sub(previous_ms) >= AUDIO_RECEIVE_CATCH_UP_COOLDOWN_MS,
                        "クールダウンの間で始めた: previous={previous_ms} now={now_ms}"
                    );
                }
                started_at_ms = Some(now_ms);
                started_seen.set(started_seen.get() + 1);
            }
            // 遅れが上限の中へ戻った観測の後は、追いつきが終わっている
            if lag_ms <= stats.limit_ms {
                assert!(
                    !stats.catching_up,
                    "遅れが上限の中なのに追いつきが続いている: lag={lag_ms} {stats:?}"
                );
                ended_seen.set(ended_seen.get() + 1);
            }
        }
        Ok(())
    })?;
    assert!(
        started_seen.get() > 0,
        "追いつきを始める場合を一度も検証していない\n{runner}"
    );
    assert!(
        ended_seen.get() > 0,
        "遅れが上限へ戻る場合を一度も検証していない\n{runner}"
    );
    Ok(())
}

/// 追いつきで飛んだ音の合計が、TIMESTAMP の跳びと一致すること
#[test]
fn skipped_audio_matches_the_timestamp_jumps() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut catch_up = AudioReceiveCatchUp::new();
        // 遅れ 20 ms の健全な観測を 5 秒ぶん流し、床を決める
        let mut now_ms = 0i64;
        let mut timestamp_us = 0i64;
        for _ in 0..25 {
            now_ms += 250;
            timestamp_us += 20_000;
            catch_up.observe(AudioReceiveCatchUpObservation {
                now_ms,
                lag_ms: 20,
                duration_ms: 20,
                timestamp_us,
            });
        }
        assert_eq!(catch_up.snapshot().floor_ms, Some(20));

        // 追いつきを始める (床 + 100 ms を超える遅れ)
        now_ms += 250;
        timestamp_us += 20_000;
        assert!(
            catch_up
                .observe(AudioReceiveCatchUpObservation {
                    now_ms,
                    lag_ms: 200,
                    duration_ms: 20,
                    timestamp_us,
                })
                .rebuild_decoder,
            "床から 100 ms を超えたら始めること"
        );

        // 追いつきの間の跳びを、観測するたびに足し合わせる
        let mut expected_skipped_ms = 0i64;
        let mut expected_frames = 0u64;
        let jumps = noprop::sample_usize_in(ctx, 1..=8);
        for _ in 0..jumps {
            // 跳び (1 音の長さに 0〜10 音を足した分)
            let extra_frames = noprop::sample_usize_in(ctx, 0..=10) as u64;
            let step_us = (1 + extra_frames) as i64 * 20_000;
            now_ms += 250;
            timestamp_us += step_us;
            if step_us - 20_000 > 1_000 {
                expected_skipped_ms += (step_us - 20_000) / 1_000;
                expected_frames += extra_frames.max(1);
            }
            catch_up.observe(AudioReceiveCatchUpObservation {
                now_ms,
                lag_ms: 200,
                duration_ms: 20,
                timestamp_us,
            });
            let stats = catch_up.snapshot();
            assert_eq!(
                stats.skipped_ms, expected_skipped_ms,
                "飛んだ長さの合計が跳びと一致しない: {stats:?}"
            );
            assert_eq!(
                stats.skipped_frames, expected_frames,
                "飛んだ音の数が跳びと一致しない: {stats:?}"
            );
        }
        Ok(())
    })?;
    Ok(())
}
