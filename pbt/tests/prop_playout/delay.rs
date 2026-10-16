//! 到着の遅れからの目標遅延の学習のプロパティテスト
//!
//! 任意の観測列で目標遅延が定義された範囲に収まることを検証する。あわせて、目標が分布の
//! バケットの倍数であり観測した遅れの最大を超えないこと、揺らぎが無ければ下限まで下がること、
//! 同じ到着列なら同じ学習になることを確かめる。

use pbt::common::test_runner;
use shiguredo_moqt::playout::delay::{
    AUDIO_DELAY_BUCKET_MS, AUDIO_DELAY_BUCKETS, AudioDelayManager,
};

/// 音 1 つの長さ (ミリ秒)。Opus の 1 フレームと同じ 20 ms にする
const FRAME_MS: i64 = 20;

/// 一定の遅れで届く音を `frame_count` 個観測する
///
/// 到着は TIMESTAMP + `delay_ms` にする。送受信の時計のずれと経路の遅れを含んだ値である。
fn observe_constant(manager: &mut AudioDelayManager, frame_count: usize, delay_ms: i64) {
    for index in 0..frame_count {
        let capture_ms = index as i64 * FRAME_MS;
        manager.observe((capture_ms + delay_ms) * 1_000, capture_ms * 1_000);
    }
}

/// 与えた遅れの列を繰り返して `frame_count` 個観測する
fn observe_series(manager: &mut AudioDelayManager, frame_count: usize, delays_ms: &[i64]) {
    for index in 0..frame_count {
        let delay_ms = delays_ms[index % delays_ms.len()];
        let capture_ms = index as i64 * FRAME_MS;
        manager.observe((capture_ms + delay_ms) * 1_000, capture_ms * 1_000);
    }
}

/// 任意の観測列で、目標遅延が分位点の範囲 (20 から 2000 ms) に収まる
#[test]
fn target_delay_stays_in_range() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut manager = AudioDelayManager::new();
        let mut capture_ms = noprop::sample_usize_in(ctx, 0..1_000_000) as i64;
        for _ in 0..noprop::sample_usize_in(ctx, 1..64) {
            capture_ms += noprop::sample_usize_in(ctx, 0..5_000) as i64;
            let delay_ms = noprop::sample_usize_in(ctx, 0..3_000) as i64;
            manager.observe((capture_ms + delay_ms) * 1_000, capture_ms * 1_000);
            let target_ms = manager.target_delay_ms();
            assert!(
                (AUDIO_DELAY_BUCKET_MS..=AUDIO_DELAY_BUCKETS as i64 * AUDIO_DELAY_BUCKET_MS)
                    .contains(&target_ms),
                "target_ms={target_ms}"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// 任意の観測列で、目標遅延がバケットの倍数であり観測した遅れの最大 + 1 バケットを超えない
///
/// 分布はバケットごとに数えるため、目標はバケットの幅の倍数になる。相対の遅れは「最も早く
/// 届いた音」との差であり、観測した遅れの最大を超えないので、目標もその 1 バケット上までに
/// 収まる。
#[test]
fn target_delay_is_a_bucket_multiple_within_the_observed_delays() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut manager = AudioDelayManager::new();
        // ヒストグラムへ入れる区間 (500 ms) を必ず 1 回以上またぐ音の数にする。それ未満では
        // 初期値のままになり、学習した値の性質を確かめられない
        let frame_count = noprop::sample_usize_in(ctx, 50..400);
        let delay_count = noprop::sample_usize_in(ctx, 1..400);
        let delays_ms: Vec<i64> = (0..delay_count)
            .map(|_| noprop::sample_usize_in(ctx, 0..1_500) as i64)
            .collect();
        observe_series(&mut manager, frame_count, &delays_ms);
        let mut maximum_ms = 0;
        for index in 0..frame_count {
            maximum_ms = maximum_ms.max(delays_ms[index % delays_ms.len()]);
        }
        let target_ms = manager.target_delay_ms();
        assert_eq!(
            target_ms % AUDIO_DELAY_BUCKET_MS,
            0,
            "目標がバケットの幅 ({AUDIO_DELAY_BUCKET_MS} ms) の倍数であること: target_ms={target_ms}"
        );
        assert!(
            target_ms >= AUDIO_DELAY_BUCKET_MS,
            "下限のバケット以上であること: target_ms={target_ms}"
        );
        assert!(
            target_ms <= maximum_ms + AUDIO_DELAY_BUCKET_MS,
            "目標が観測した遅れの最大 + 1 バケットを超えないこと: target_ms={target_ms} maximum_ms={maximum_ms}"
        );
        assert!(
            target_ms <= AUDIO_DELAY_BUCKETS as i64 * AUDIO_DELAY_BUCKET_MS,
            "バケットの範囲を超えないこと: target_ms={target_ms}"
        );
        Ok(())
    })?;
    Ok(())
}

/// 揺らぎの無い到着列では、目標遅延が下限のバケットまで下がる
///
/// 一定の遅れで届く音だけなら相対の遅れは常に 0 になり、分布は最初のバケットだけになる。
/// 時計のずれと経路の最小遅延がいくつであっても変わらない。
#[test]
fn target_delay_drops_to_the_floor_bucket_without_jitter() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let constant_delay_ms = noprop::sample_usize_in(ctx, 0..2_000) as i64;
        let frame_count = noprop::sample_usize_in(ctx, 50..400);
        let mut manager = AudioDelayManager::new();
        observe_constant(&mut manager, frame_count, constant_delay_ms);
        assert_eq!(
            manager.target_delay_ms(),
            AUDIO_DELAY_BUCKET_MS,
            "一定の遅れで届く音だけなら下限のバケットになること: delay_ms={constant_delay_ms} frames={frame_count}"
        );
        Ok(())
    })?;
    Ok(())
}

/// 同じ到着列なら同じ目標になり、`reset` の後も同じ学習になる
///
/// 学習に乱数を使わないため、同じ入力列からは同じ目標が決まる。購読をやり直した後も同じ
/// 到着列であれば同じ目標へ戻る。
#[test]
fn target_delay_is_deterministic_and_reset_restarts_the_learning() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let frame_count = noprop::sample_usize_in(ctx, 1..200);
        let delay_count = noprop::sample_usize_in(ctx, 1..200);
        let delays_ms: Vec<i64> = (0..delay_count)
            .map(|_| noprop::sample_usize_in(ctx, 0..600) as i64)
            .collect();
        let mut first = AudioDelayManager::new();
        observe_series(&mut first, frame_count, &delays_ms);
        let mut second = AudioDelayManager::new();
        observe_series(&mut second, frame_count, &delays_ms);
        assert_eq!(
            first.target_delay_ms(),
            second.target_delay_ms(),
            "同じ到着列なら同じ目標になること"
        );
        // reset してから同じ到着列を与えても、同じ目標になる
        first.reset();
        observe_series(&mut first, frame_count, &delays_ms);
        assert_eq!(
            first.target_delay_ms(),
            second.target_delay_ms(),
            "reset の後の学習が同じ結果になること"
        );
        Ok(())
    })?;
    Ok(())
}
