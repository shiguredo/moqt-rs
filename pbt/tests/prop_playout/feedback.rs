//! 音声の目標遅延の閉ループのプロパティテスト
//!
//! 任意の操作列 (観測・明示の上限の設定と解除・リセット) に対して、目標が上下限と明示の
//! 上限の中に収まり、実際に使う値が jitter buffer の学習値以上になり、1 回の増分が
//! 20〜40 ms に収まり、減少量が毎秒 10 ms を超えず、並べすぎの累積が戻っても増えないことを
//! 検証する。分岐ごとに到達を数え、検証が空振りしていないことも確かめる。

use std::cell::Cell;

use pbt::common::test_runner;
use shiguredo_moqt::playout::feedback::{
    AUDIO_DELAY_FEEDBACK_DECREASE_US_PER_SECOND, AUDIO_DELAY_FEEDBACK_INTERVAL_US,
    AUDIO_DELAY_FEEDBACK_MAX_STEP_US, AUDIO_DELAY_FEEDBACK_MAX_US,
    AUDIO_DELAY_FEEDBACK_MIN_STEP_US, AUDIO_DELAY_FEEDBACK_MIN_US, AUDIO_DELAY_FEEDBACK_START_US,
    AUDIO_DELAY_FEEDBACK_TOLERANCE_US, AudioDelayFeedback, AudioDelayFeedbackReason,
    AudioDelayFeedbackSnapshot,
};
use shiguredo_moqt::playout::timing::{AudioDelayFeedbackObservation, TimingSummary};

/// 操作の重み (観測 / 明示の上限の設定 / 明示の上限の解除 / リセット)
///
/// 観測を主にし、上限の設定 / 解除とリセットも混ぜる。観測は添字 0 である。
const OP_WEIGHTS: [u32; 4] = [10, 2, 1, 1];

/// 明示の上限を設定する操作の添字 ([`OP_WEIGHTS`])
const OP_SET_CEILING: usize = 1;
/// 明示の上限を解除する操作の添字 ([`OP_WEIGHTS`])
const OP_CLEAR_CEILING: usize = 2;
/// リセットする操作の添字 ([`OP_WEIGHTS`])
const OP_RESET: usize = 3;

/// 鳴り遅れの分布の選び方の重み (まだ鳴らしていない / 許容の中 / 許容の上 / 大きな遅れ)
const LATENESS_WEIGHTS: [u32; 4] = [2, 3, 5, 2];

/// 並べすぎの累積の選び方の重み (増えない / 小さく増える / 大きく増える / 戻る)
///
/// 「戻る」は購読のやり直しで累積が 0 に戻る場合であり、負の差で増やさないことを見る。
const BACKLOG_WEIGHTS: [u32; 4] = [5, 3, 2, 2];

/// 1 件の値だけを持つ分布
fn summary(value_us: i64) -> TimingSummary {
    TimingSummary {
        p50: value_us,
        p95: value_us,
        max: value_us,
    }
}

/// 観測を 1 つ作る
fn observation(
    at_us: i64,
    lateness_p50_us: Option<i64>,
    backlog_misses: u64,
    backlog_us: i64,
) -> AudioDelayFeedbackObservation {
    AudioDelayFeedbackObservation {
        at_us,
        lateness_us: lateness_p50_us.map(summary),
        start_delay_us: Some(summary(120_000)),
        slack_us: Some(summary(20_000)),
        backlog_misses,
        backlog_us,
    }
}

/// 明示の上限から、目標の範囲 (下限, 上限) を求める
///
/// 明示の上限が下限 (80 ms) を下回るときは上限を優先する (moqt-js と同じ)。
fn target_limits(ceiling_us: Option<i64>) -> (i64, i64) {
    let upper_us =
        AUDIO_DELAY_FEEDBACK_MAX_US.min(ceiling_us.unwrap_or(AUDIO_DELAY_FEEDBACK_MAX_US));
    (AUDIO_DELAY_FEEDBACK_MIN_US.min(upper_us), upper_us)
}

/// 増える向きの理由の増分が規則の範囲に収まっていることを確かめる
///
/// 1 回の増分は [`AUDIO_DELAY_FEEDBACK_MIN_STEP_US`] から
/// [`AUDIO_DELAY_FEEDBACK_MAX_STEP_US`] までである。ただし上限に当たったときは、残りの
/// 分だけしか動かない。
fn check_increase(snapshot: AudioDelayFeedbackSnapshot, upper_us: i64, saw_increase: &Cell<bool>) {
    assert!(
        snapshot.last_change_us >= 0,
        "増える向きの理由で目標が下がった: {snapshot:?}"
    );
    if snapshot.last_change_us == 0 {
        // 上限に張り付いているときは動かない
        assert_eq!(
            snapshot.target_us, upper_us,
            "増分が 0 なのに上限に達していない: {snapshot:?}"
        );
        return;
    }
    saw_increase.set(true);
    assert!(
        snapshot.last_change_us <= AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "1 回の増分が上限を超えた: change={} max={AUDIO_DELAY_FEEDBACK_MAX_STEP_US}",
        snapshot.last_change_us
    );
    if snapshot.target_us < upper_us {
        assert!(
            snapshot.last_change_us >= AUDIO_DELAY_FEEDBACK_MIN_STEP_US,
            "1 回の増分が下限を下回った: change={} min={AUDIO_DELAY_FEEDBACK_MIN_STEP_US}",
            snapshot.last_change_us
        );
    }
}

#[test]
fn target_stays_within_the_limits_and_moves_by_the_rules() -> noprop::TestResult {
    // 到達した分岐を数える。0 のままなら検証が空振りしている
    let saw_before_observation = Cell::new(false);
    let saw_jitter_larger = Cell::new(false);
    let saw_lateness = Cell::new(false);
    let saw_backlog = Cell::new(false);
    let saw_settled = Cell::new(false);
    let saw_increase = Cell::new(false);
    let saw_decrease = Cell::new(false);
    let saw_floor = Cell::new(false);
    let saw_upper = Cell::new(false);
    let saw_waiting_for_interval = Cell::new(false);
    let saw_waiting_without_lateness = Cell::new(false);
    let saw_waiting_for_the_first_settled = Cell::new(false);
    let saw_ceiling = Cell::new(false);
    let saw_ceiling_removed = Cell::new(false);
    let saw_ceiling_below_floor = Cell::new(false);
    let saw_ceiling_clamp = Cell::new(false);
    let saw_backlog_returned = Cell::new(false);
    let saw_reset_after_adjustment = Cell::new(false);
    let saw_reset_clears_the_observation = Cell::new(false);

    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut feedback = AudioDelayFeedback::new();
        // SUT と同じ規則で「前回判断した時刻」を写し取る (減少量の上限を求めるのに使う)
        let mut last_update_at_us: Option<i64> = None;
        let mut backlog_misses = 0u64;
        let mut backlog_us = 0i64;
        // 前回「判断した」観測の累積 (間隔が空いていない観測は累積を読まずに戻る)
        let mut consumed_backlog_misses = 0u64;
        let mut consumed_backlog_us = 0i64;
        let mut now_us = noprop::sample_usize_in(ctx, 0..1_000_000) as i64;

        for _ in 0..noprop::sample_usize_in(ctx, 1..=24) {
            if last_update_at_us.is_none() {
                // まだ実際に鳴った結果を観測していない。学習値をそのまま使う
                let jitter_us = noprop::sample_usize_in(ctx, 0..500_000) as i64;
                assert_eq!(
                    feedback.target_delay_us(jitter_us),
                    jitter_us,
                    "観測を受ける前は学習値をそのまま使う"
                );
                let (lower_us, upper_us) = target_limits(feedback.ceiling_us());
                assert!(
                    (lower_us..=upper_us).contains(&feedback.feedback_target_us()),
                    "観測を受ける前の目標が範囲の外に出た: target={} lower={lower_us} upper={upper_us}",
                    feedback.feedback_target_us()
                );
                saw_before_observation.set(true);
            }

            match noprop::sample_weighted_index(ctx, &OP_WEIGHTS) {
                OP_SET_CEILING => {
                    let ceiling_us = noprop::sample_with_boundaries(
                        ctx,
                        &[
                            0,
                            AUDIO_DELAY_FEEDBACK_MIN_US,
                            AUDIO_DELAY_FEEDBACK_MAX_US,
                            1_000_000,
                        ],
                        noprop::Ratio::one_nth(4),
                        |ctx| noprop::sample_usize_in(ctx, 0..=500_000) as i64,
                    );
                    feedback.set_ceiling_us(Some(ceiling_us));
                    saw_ceiling.set(true);
                    assert_eq!(
                        feedback.ceiling_us(),
                        Some(ceiling_us),
                        "明示の上限が保持されない: ceiling={ceiling_us}"
                    );
                    if ceiling_us < AUDIO_DELAY_FEEDBACK_MIN_US {
                        saw_ceiling_below_floor.set(true);
                    }
                    // 上限を掛けた時点で、既に超えていればその場で収まる
                    let snapshot = feedback.snapshot();
                    let (lower_us, upper_us) = target_limits(Some(ceiling_us));
                    assert!(
                        (lower_us..=upper_us).contains(&snapshot.target_us),
                        "上限の設定で目標が範囲の外に出た: target={} lower={lower_us} upper={upper_us}",
                        snapshot.target_us
                    );
                    if snapshot.target_us == upper_us {
                        saw_ceiling_clamp.set(true);
                    }
                }
                OP_CLEAR_CEILING => {
                    feedback.set_ceiling_us(None);
                    saw_ceiling_removed.set(true);
                    assert_eq!(feedback.ceiling_us(), None, "上限の解除が反映されない");
                    let snapshot = feedback.snapshot();
                    let (lower_us, upper_us) = target_limits(None);
                    assert!(
                        (lower_us..=upper_us).contains(&snapshot.target_us),
                        "上限の解除で目標が範囲の外に出た: target={}",
                        snapshot.target_us
                    );
                }
                OP_RESET => {
                    let adjustments_before_reset = feedback.snapshot().adjustments;
                    let ceiling_us = feedback.ceiling_us();
                    feedback.reset();
                    last_update_at_us = None;
                    backlog_misses = 0;
                    backlog_us = 0;
                    consumed_backlog_misses = 0;
                    consumed_backlog_us = 0;

                    let snapshot = feedback.snapshot();
                    let (lower_us, upper_us) = target_limits(ceiling_us);
                    let expected_us = AUDIO_DELAY_FEEDBACK_START_US.max(lower_us).min(upper_us);
                    assert_eq!(
                        snapshot.target_us, expected_us,
                        "リセットで初期値へ戻らない: {snapshot:?}"
                    );
                    assert_eq!(
                        snapshot.reason,
                        AudioDelayFeedbackReason::Initial,
                        "リセットで理由が戻らない: {snapshot:?}"
                    );
                    assert_eq!(snapshot.adjustments, 0, "リセットで回数が戻らない");
                    assert_eq!(snapshot.last_change_us, 0, "リセットで増分が残る");
                    assert_eq!(snapshot.lateness_p50_us, None, "リセットで観測が残る");
                    assert_eq!(snapshot.start_delay_p50_us, None, "リセットで観測が残る");
                    assert_eq!(snapshot.slack_p50_us, None, "リセットで観測が残る");
                    assert_eq!(
                        feedback.ceiling_us(),
                        ceiling_us,
                        "リセットで明示の上限が消えた"
                    );
                    // 観測が無い扱いへ戻るため、学習値をそのまま使う
                    let jitter_us = noprop::sample_usize_in(ctx, 0..500_000) as i64;
                    assert_eq!(
                        feedback.target_delay_us(jitter_us),
                        jitter_us,
                        "リセットで観測が無い扱いへ戻らない"
                    );
                    saw_reset_clears_the_observation.set(true);
                    if adjustments_before_reset > 0 {
                        saw_reset_after_adjustment.set(true);
                    }
                }
                _ => {
                    // 残るのは観測 (添字 0) だけである
                    // 観測の時刻を進める。間隔の境目と、間隔を大きく超える場合を混ぜる
                    now_us = now_us.saturating_add(noprop::sample_with_boundaries(
                        ctx,
                        &[
                            0,
                            AUDIO_DELAY_FEEDBACK_INTERVAL_US - 1,
                            AUDIO_DELAY_FEEDBACK_INTERVAL_US,
                            AUDIO_DELAY_FEEDBACK_INTERVAL_US * 3,
                        ],
                        noprop::Ratio::one_nth(3),
                        |ctx| noprop::sample_usize_in(ctx, 0..=2_500_000) as i64,
                    ));
                    // 鳴り遅れの p50
                    let lateness_p50_us = match noprop::sample_weighted_index(ctx, &LATENESS_WEIGHTS)
                    {
                        0 => None,
                        1 => Some(
                            noprop::sample_usize_in(
                                ctx,
                                0..=AUDIO_DELAY_FEEDBACK_TOLERANCE_US as usize,
                            ) as i64,
                        ),
                        2 => Some(
                            noprop::sample_usize_in(
                                ctx,
                                AUDIO_DELAY_FEEDBACK_TOLERANCE_US as usize
                                    ..=AUDIO_DELAY_FEEDBACK_MAX_US as usize,
                            ) as i64,
                        ),
                        _ => Some(noprop::sample_usize_in(ctx, 0..=2_000_000) as i64),
                    };
                    // 並べすぎの累積。購読のやり直しに相当する「戻る」も混ぜる
                    match noprop::sample_weighted_index(ctx, &BACKLOG_WEIGHTS) {
                        0 => {}
                        1 => {
                            backlog_misses += noprop::sample_usize_in(ctx, 1..=3) as u64;
                            backlog_us += noprop::sample_usize_in(ctx, 0..=100_000) as i64;
                        }
                        2 => {
                            backlog_misses += noprop::sample_usize_in(ctx, 1..=3) as u64;
                            backlog_us += noprop::sample_usize_in(ctx, 0..=500_000) as i64;
                        }
                        _ => {
                            backlog_misses = 0;
                            backlog_us = 0;
                        }
                    }

                    let previous_target_us = feedback.feedback_target_us();
                    let previous_adjustments = feedback.snapshot().adjustments;
                    let elapsed_us = last_update_at_us
                        .map(|last_update_at_us| now_us.saturating_sub(last_update_at_us).max(0));
                    let interval_passed = elapsed_us
                        .is_none_or(|elapsed_us| elapsed_us >= AUDIO_DELAY_FEEDBACK_INTERVAL_US);
                    // 閉ループが読む差は、前回「判断した」観測との差である。間隔が空いて
                    // いない観測は累積を読まずに戻るため、前回の観測ではなく前回の判断と
                    // の差で比べる
                    let backlog_increased = interval_passed && backlog_misses > consumed_backlog_misses;
                    let backlog_returned = interval_passed
                        && (backlog_misses < consumed_backlog_misses
                            || backlog_us < consumed_backlog_us);
                    feedback.update(observation(
                        now_us,
                        lateness_p50_us,
                        backlog_misses,
                        backlog_us,
                    ));
                    if interval_passed {
                        last_update_at_us = Some(now_us);
                        consumed_backlog_misses = backlog_misses;
                        consumed_backlog_us = backlog_us;
                        if backlog_returned {
                            saw_backlog_returned.set(true);
                        }
                    }

                    let snapshot = feedback.snapshot();
                    let ceiling_us = feedback.ceiling_us();
                    let (lower_us, upper_us) = target_limits(ceiling_us);
                    assert!(
                        (lower_us..=upper_us).contains(&snapshot.target_us),
                        "目標が上下限の外に出た: target={} lower={lower_us} upper={upper_us} {snapshot:?}",
                        snapshot.target_us
                    );
                    if let Some(ceiling_us) = ceiling_us {
                        assert!(
                            snapshot.target_us <= ceiling_us,
                            "目標が明示の上限を超えた: target={} ceiling={ceiling_us}",
                            snapshot.target_us
                        );
                    }
                    if snapshot.target_us == lower_us {
                        saw_floor.set(true);
                    }
                    if snapshot.target_us == upper_us {
                        saw_upper.set(true);
                    }
                    // 実際に使う値は、学習値と閉ループの目標の大きい方である
                    let jitter_us = noprop::sample_usize_in(ctx, 0..500_000) as i64;
                    assert!(
                        feedback.target_delay_us(jitter_us) >= jitter_us,
                        "実際に使う値が学習値を下回った: jitter={jitter_us} applied={}",
                        feedback.target_delay_us(jitter_us)
                    );
                    assert!(
                        feedback.target_delay_us(jitter_us) >= snapshot.target_us,
                        "実際に使う値が閉ループの目標を下回った: target={} applied={}",
                        snapshot.target_us,
                        feedback.target_delay_us(jitter_us)
                    );
                    if jitter_us > snapshot.target_us {
                        saw_jitter_larger.set(true);
                    }
                    // 動かした回数は、目標が動いた回数だけ増える
                    assert!(
                        snapshot.adjustments >= previous_adjustments,
                        "動かした回数が減った: {} -> {}",
                        previous_adjustments,
                        snapshot.adjustments
                    );
                    match snapshot.reason {
                        AudioDelayFeedbackReason::Lateness => {
                            saw_lateness.set(true);
                            assert_eq!(
                                snapshot.adjustments,
                                previous_adjustments + u64::from(snapshot.last_change_us != 0),
                                "動かした回数の数え方が変わった: {snapshot:?}"
                            );
                            assert!(
                                !backlog_increased,
                                "並べすぎが増えていないのに理由が遅れになった: {snapshot:?}"
                            );
                            assert!(
                                lateness_p50_us.is_some_and(|lateness_p50_us| lateness_p50_us
                                    > AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
                                "遅れが許容の中なのに理由が遅れになった: lateness={lateness_p50_us:?}"
                            );
                            check_increase(snapshot, upper_us, &saw_increase);
                        }
                        AudioDelayFeedbackReason::Backlog => {
                            saw_backlog.set(true);
                            assert_eq!(
                                snapshot.adjustments,
                                previous_adjustments + u64::from(snapshot.last_change_us != 0),
                                "動かした回数の数え方が変わった: {snapshot:?}"
                            );
                            assert!(
                                backlog_increased,
                                "並べすぎが増えていないのに理由が並べすぎになった: {snapshot:?}"
                            );
                            check_increase(snapshot, upper_us, &saw_increase);
                        }
                        AudioDelayFeedbackReason::Settled => {
                            saw_settled.set(true);
                            assert_eq!(
                                snapshot.adjustments,
                                previous_adjustments + u64::from(snapshot.last_change_us != 0),
                                "動かした回数の数え方が変わった: {snapshot:?}"
                            );
                            let elapsed_us = elapsed_us.expect("収束の判断には前回の観測が要る");
                            assert!(
                                !backlog_increased,
                                "並べすぎが増えているのに理由が収束になった: {snapshot:?}"
                            );
                            assert!(
                                lateness_p50_us.is_some_and(|lateness_p50_us| lateness_p50_us
                                    <= AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
                                "遅れが許容の外なのに理由が収束になった: lateness={lateness_p50_us:?}"
                            );
                            assert!(
                                snapshot.last_change_us <= 0,
                                "収束の判断で目標が上がった: {snapshot:?}"
                            );
                            let budget_us = AUDIO_DELAY_FEEDBACK_DECREASE_US_PER_SECOND
                                .saturating_mul(elapsed_us)
                                / 1_000_000;
                            assert!(
                                snapshot.last_change_us.saturating_neg() <= budget_us,
                                "毎秒の速さを超えて減らした: change={} budget={budget_us} elapsed={elapsed_us}",
                                snapshot.last_change_us
                            );
                            if snapshot.last_change_us < 0 {
                                saw_decrease.set(true);
                            }
                        }
                        AudioDelayFeedbackReason::Waiting => {
                            assert_eq!(
                                snapshot.target_us, previous_target_us,
                                "待ちの判断で目標が動いた: {snapshot:?}"
                            );
                            assert_eq!(
                                snapshot.adjustments, previous_adjustments,
                                "待ちの判断で動かした回数が増えた: {snapshot:?}"
                            );
                            if let Some(elapsed_us) = elapsed_us
                                && elapsed_us < AUDIO_DELAY_FEEDBACK_INTERVAL_US
                            {
                                saw_waiting_for_interval.set(true);
                            } else if lateness_p50_us.is_none() {
                                saw_waiting_without_lateness.set(true);
                            } else {
                                assert!(
                                    elapsed_us.is_none(),
                                    "前回の観測があるのに減らさず待った: {snapshot:?}"
                                );
                                saw_waiting_for_the_first_settled.set(true);
                            }
                        }
                        AudioDelayFeedbackReason::Initial => {
                            panic!("観測を渡した後に理由が初期状態になった: {snapshot:?}");
                        }
                    }
                }
            }
        }
        Ok(())
    })?;

    // 到達した分岐を確かめる。0 のままなら検証が空振りしている
    assert!(
        saw_before_observation.get(),
        "観測を受ける前の目標を使う場合を通っていない\n{runner}"
    );
    assert!(
        saw_jitter_larger.get(),
        "学習値の方が大きくなる場合を通っていない\n{runner}"
    );
    assert!(
        saw_lateness.get(),
        "鳴り遅れで増える分岐を通っていない\n{runner}"
    );
    assert!(
        saw_backlog.get(),
        "並べすぎで増える分岐を通っていない\n{runner}"
    );
    assert!(
        saw_settled.get(),
        "許容の中に収まって減る分岐を通っていない\n{runner}"
    );
    assert!(
        saw_increase.get(),
        "目標が増える場合を通っていない\n{runner}"
    );
    assert!(saw_decrease.get(), "目標が減る場合を通っていない\n{runner}");
    assert!(saw_floor.get(), "下限に当たる場合を通っていない\n{runner}");
    assert!(saw_upper.get(), "上限に当たる場合を通っていない\n{runner}");
    assert!(
        saw_waiting_for_interval.get(),
        "間隔が空いていないため待つ分岐を通っていない\n{runner}"
    );
    assert!(
        saw_waiting_without_lateness.get(),
        "遅れの観測が無いため待つ分岐を通っていない\n{runner}"
    );
    assert!(
        saw_waiting_for_the_first_settled.get(),
        "前回の観測が無いため減らさず待つ分岐を通っていない\n{runner}"
    );
    assert!(
        saw_ceiling.get(),
        "明示の上限を設定する場合を通っていない\n{runner}"
    );
    assert!(
        saw_ceiling_removed.get(),
        "明示の上限を解除する場合を通っていない\n{runner}"
    );
    assert!(
        saw_ceiling_below_floor.get(),
        "明示の上限が下限を下回る場合を通っていない\n{runner}"
    );
    assert!(
        saw_ceiling_clamp.get(),
        "上限の設定で目標が収まる場合を通っていない\n{runner}"
    );
    assert!(
        saw_backlog_returned.get(),
        "並べすぎの累積が戻る場合を通っていない\n{runner}"
    );
    assert!(
        saw_reset_after_adjustment.get(),
        "目標を動かした後のリセットを通っていない\n{runner}"
    );
    assert!(
        saw_reset_clears_the_observation.get(),
        "リセットで観測が消える場合を通っていない\n{runner}"
    );
    Ok(())
}

#[test]
fn reset_restores_the_initial_state_and_keeps_the_ceiling() -> noprop::TestResult {
    let saw_reset_after_adjustment = Cell::new(false);
    let saw_reset_with_ceiling = Cell::new(false);
    let saw_reset_without_ceiling = Cell::new(false);

    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let mut feedback = AudioDelayFeedback::new();
        if noprop::sample_bool(ctx) {
            let ceiling_us =
                noprop::sample_usize_in(ctx, 0..=AUDIO_DELAY_FEEDBACK_MAX_US as usize) as i64;
            feedback.set_ceiling_us(Some(ceiling_us));
        }
        let mut now_us = 0i64;
        for _ in 0..noprop::sample_usize_in(ctx, 0..=8) {
            now_us = now_us.saturating_add(noprop::sample_with_boundaries(
                ctx,
                &[0, AUDIO_DELAY_FEEDBACK_INTERVAL_US],
                noprop::Ratio::one_nth(3),
                |ctx| noprop::sample_usize_in(ctx, 0..=2_000_000) as i64,
            ));
            let lateness_p50_us = match noprop::sample_weighted_index(ctx, &LATENESS_WEIGHTS) {
                0 => None,
                _ => Some(noprop::sample_usize_in(ctx, 0..=400_000) as i64),
            };
            feedback.update(observation(now_us, lateness_p50_us, 0, 0));
        }

        let adjustments_before_reset = feedback.snapshot().adjustments;
        let ceiling_us = feedback.ceiling_us();
        feedback.reset();

        let snapshot = feedback.snapshot();
        let (lower_us, upper_us) = target_limits(ceiling_us);
        let expected_us = AUDIO_DELAY_FEEDBACK_START_US.max(lower_us).min(upper_us);
        assert_eq!(
            snapshot.target_us, expected_us,
            "リセットで初期値 (明示の上限の中) へ戻らない: {snapshot:?}"
        );
        assert_eq!(
            snapshot.reason,
            AudioDelayFeedbackReason::Initial,
            "リセットで理由が戻らない: {snapshot:?}"
        );
        assert_eq!(snapshot.adjustments, 0, "リセットで回数が戻らない");
        assert_eq!(snapshot.last_change_us, 0, "リセットで増分が残る");
        assert_eq!(snapshot.lateness_p50_us, None, "リセットで観測が残る");
        assert_eq!(snapshot.start_delay_p50_us, None, "リセットで観測が残る");
        assert_eq!(snapshot.slack_p50_us, None, "リセットで観測が残る");
        // 明示設定の上限は呼び出し側が決めた値であるため残す
        assert_eq!(feedback.ceiling_us(), ceiling_us, "リセットで上限が消えた");
        // 観測が無い扱いへ戻るため、学習値をそのまま使う
        let jitter_us = noprop::sample_usize_in(ctx, 0..500_000) as i64;
        assert_eq!(
            feedback.target_delay_us(jitter_us),
            jitter_us,
            "リセットで観測が無い扱いへ戻らない"
        );

        if adjustments_before_reset > 0 {
            saw_reset_after_adjustment.set(true);
        }
        if ceiling_us.is_some() {
            saw_reset_with_ceiling.set(true);
        } else {
            saw_reset_without_ceiling.set(true);
        }
        Ok(())
    })?;

    assert!(
        saw_reset_after_adjustment.get(),
        "目標を動かした後のリセットを通っていない\n{runner}"
    );
    assert!(
        saw_reset_with_ceiling.get(),
        "明示の上限がある状態のリセットを通っていない\n{runner}"
    );
    assert!(
        saw_reset_without_ceiling.get(),
        "明示の上限が無い状態のリセットを通っていない\n{runner}"
    );
    Ok(())
}
