//! 音声の目標遅延の閉ループのプロパティテスト
//!
//! 任意の操作列 (観測・明示の上限の設定と解除) に対して、目標が上下限と明示の
//! 上限の中に収まり、実際に使う値が jitter buffer の学習値以上になり、1 回の増分が
//! 20〜40 ms に収まり、減少量が毎秒 10 ms を超えず、並べすぎの累積が戻っても増えないことを
//! 検証する。分岐ごとに到達を数え、検証が空振りしていないことも確かめる。

use std::cell::Cell;

use pbt::common::test_runner;
use shiguredo_moqt::playout::feedback::{
    AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US, AUDIO_DELAY_FEEDBACK_ARTIFACT_RELEASE_US,
    AUDIO_DELAY_FEEDBACK_COMPRESSION_CONFIRM_US,
    AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND,
    AUDIO_DELAY_FEEDBACK_DECREASE_US_PER_SECOND, AUDIO_DELAY_FEEDBACK_GAP_QUIET_US,
    AUDIO_DELAY_FEEDBACK_INTERVAL_US, AUDIO_DELAY_FEEDBACK_MARGIN_US,
    AUDIO_DELAY_FEEDBACK_MAX_STEP_US, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
    AUDIO_DELAY_FEEDBACK_MAX_US, AUDIO_DELAY_FEEDBACK_MIN_STEP_US, AUDIO_DELAY_FEEDBACK_MIN_US,
    AUDIO_DELAY_FEEDBACK_TOLERANCE_US, AudioDelayFeedback, AudioDelayFeedbackReason,
    AudioDelayFeedbackSnapshot,
};
use shiguredo_moqt::playout::scheduler::AUDIO_PLAYOUT_STRETCH_MIN_US;
use shiguredo_moqt::playout::timing::{AudioDelayFeedbackObservation, TimingSummary};

/// 操作の重み (観測 / 明示の上限の設定 / 明示の上限の解除)
///
/// 観測を主にし、上限の設定 / 解除も混ぜる。観測は添字 0 である。
const OP_WEIGHTS: [u32; 3] = [10, 2, 1];

/// 明示の上限を設定する操作の添字 ([`OP_WEIGHTS`])
const OP_SET_CEILING: usize = 1;
/// 明示の上限を解除する操作の添字 ([`OP_WEIGHTS`])
const OP_CLEAR_CEILING: usize = 2;

/// 鳴り遅れの分布の選び方の重み (まだ鳴らしていない / 許容の中 / 許容の上 / 大きな遅れ)
const LATENESS_WEIGHTS: [u32; 4] = [2, 3, 5, 2];

/// 並べすぎの累積の選び方の重み (増えない / 小さく増える / 大きく増える / 戻る)
///
/// 「戻る」は購読のやり直しで累積が 0 に戻る場合であり、負の差で増やさないことを見る。
const BACKLOG_WEIGHTS: [u32; 4] = [5, 3, 2, 2];

/// 圧縮の増分の選び方の重み (無し / 閾値未満 / 閾値を超える / 大きく超える)
const COMPRESSION_WEIGHTS: [u32; 4] = [5, 3, 3, 1];

/// 穴の累積の選び方の重み (増えない / 小さく増える / 大きく増える / 0 に戻る)
///
/// 「0 に戻る」は購読のやり直しで累積が 0 に戻る場合であり、負の差で増やさないことを見る。
const GAP_WEIGHTS: [u32; 4] = [5, 3, 2, 2];

/// 1 件の値だけを持つ分布
fn summary(value_us: i64) -> TimingSummary {
    TimingSummary {
        p50: value_us,
        p95: value_us,
        max: value_us,
    }
}

/// 観測を 1 つ作る
///
/// 圧縮は無し (0)、揺らぎだけの目標は 0 (経路は 100 ms 以内で足りている)、穴も無し、
/// 到着基準の追加の下限も無しとして作る。
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
        underrun_ms: 0,
        compression_us: 0,
        jitter_target_us: 0,
        gap_count: 0,
        gap_us: 0,
        arrival_delay_floor_us: None,
    }
}

/// アンダーランの長さを指定して観測を作る
///
/// `underrun_ms` は前回の制御からの出力が枯れた合計 (ミリ秒) である。遅れは許容の中に
/// 収めた観測にする (枯れだけで目標が上がることを確かめるため)。
fn observation_with_underrun(at_us: i64, underrun_ms: i64) -> AudioDelayFeedbackObservation {
    let mut observation = observation(at_us, Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US), 0, 0);
    observation.underrun_ms = underrun_ms;
    observation
}

/// 圧縮の増分と、揺らぎだけの目標遅延を指定して観測を作る
fn observation_with_compression(
    at_us: i64,
    lateness_p50_us: Option<i64>,
    backlog_misses: u64,
    backlog_us: i64,
    compression_us: i64,
    jitter_target_us: i64,
) -> AudioDelayFeedbackObservation {
    let mut observation = observation(at_us, lateness_p50_us, backlog_misses, backlog_us);
    observation.compression_us = compression_us;
    observation.jitter_target_us = jitter_target_us;
    observation
}

/// 明示設定と、上限を広げているかから、実際に適用する上限を求める
///
/// 広げていないときは [`AUDIO_DELAY_FEEDBACK_MAX_TARGET_US`] (100 ms)。広げているときは
/// `max(100 ms, min(160 ms, 明示設定 ?? 160 ms))` である。
fn applied_ceiling_us(ceiling_us: Option<i64>, widened: bool) -> i64 {
    if !widened {
        return AUDIO_DELAY_FEEDBACK_MAX_TARGET_US;
    }
    AUDIO_DELAY_FEEDBACK_MAX_TARGET_US.max(
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US
            .min(ceiling_us.unwrap_or(AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US)),
    )
}

/// 明示の上限と、実際に適用する上限から、目標の範囲 (下限, 上限) を求める
///
/// 明示の上限が下限 (50 ms) を下回るときは上限を優先する (moqt-js と同じ)。
fn target_limits(ceiling_us: Option<i64>, applied_ceiling_us: i64) -> (i64, i64) {
    let upper_us = AUDIO_DELAY_FEEDBACK_MAX_US
        .min(ceiling_us.unwrap_or(AUDIO_DELAY_FEEDBACK_MAX_US))
        .min(applied_ceiling_us);
    (AUDIO_DELAY_FEEDBACK_MIN_US.min(upper_us), upper_us)
}

/// 閉ループと同じ規則で、適用する上限の状態を 1 回進める
///
/// 引数は (広げているか, 圧縮の連続時間, 静かな連続時間) と、今回の制御で観測した
/// (制御からの時間, その間に増えた圧縮, 揺らぎだけの目標遅延) である。同じ 3 つ組を返す。
/// 速さは「制御の間に増えた圧縮 / 制御からの時間」であり、時間が 0 以下のときは 0 とする。
fn advance_ceiling(
    widened: bool,
    compression_streak_us: i64,
    quiet_streak_us: i64,
    span_us: i64,
    compression_us: i64,
    jitter_target_us: i64,
) -> (bool, i64, i64) {
    // i64 の整除で丸まらないよう、先に掛けてから割る
    let rate_us_per_second = if span_us > 0 {
        compression_us.saturating_mul(1_000_000) / span_us
    } else {
        0
    };
    let (compression_streak_us, quiet_streak_us) =
        if rate_us_per_second > AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND {
            // 100 ms では足りていない証拠が続いている
            (compression_streak_us.saturating_add(span_us), 0)
        } else if compression_us == 0 && jitter_target_us <= AUDIO_DELAY_FEEDBACK_MAX_TARGET_US {
            // 圧縮が無く、経路も 100 ms 以内で足りている
            (0, quiet_streak_us.saturating_add(span_us))
        } else {
            // 圧縮は出ているが閾値未満、または経路がまだ 100 ms を必要としている
            (0, 0)
        };
    let mut widened = widened;
    if !widened && compression_streak_us >= AUDIO_DELAY_FEEDBACK_COMPRESSION_CONFIRM_US {
        widened = true;
    } else if widened && quiet_streak_us >= AUDIO_DELAY_FEEDBACK_ARTIFACT_RELEASE_US {
        widened = false;
    }
    (widened, compression_streak_us, quiet_streak_us)
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
    let saw_compression_rate = Cell::new(false);

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
        // SUT と同じ規則で、適用する上限の状態を写し取る。観測のたびに圧縮の増分を足し、
        // 制御 (間隔が空いた観測) のたびに速さを求めて、広げる / 戻すを判断する
        let mut widened_ceiling = false;
        let mut compression_streak_us = 0i64;
        let mut quiet_streak_us = 0i64;
        let mut pending_compression_us = 0i64;
        let mut now_us = noprop::sample_usize_in(ctx, 0..1_000_000) as i64;

        for _ in 0..noprop::sample_usize_in(ctx, 1..=24) {
            if last_update_at_us.is_none() {
                // まだ実際に鳴った結果を観測していない。学習値をそのまま使う (上限は掛ける)
                let jitter_us = noprop::sample_usize_in(ctx, 0..500_000) as i64;
                assert_eq!(
                    feedback.target_delay_us(jitter_us),
                    jitter_us.min(AUDIO_DELAY_FEEDBACK_MAX_TARGET_US),
                    "観測を受ける前は学習値をそのまま使う"
                );
                let applied_ceiling_us = feedback.snapshot(0).applied_ceiling_us;
                assert_eq!(
                    applied_ceiling_us, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
                    "観測を受ける前は上限を広げない"
                );
                let (lower_us, upper_us) = target_limits(feedback.ceiling_us(), applied_ceiling_us);
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
                    let snapshot = feedback.snapshot(0);
                    let (lower_us, upper_us) =
                        target_limits(Some(ceiling_us), snapshot.applied_ceiling_us);
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
                    let snapshot = feedback.snapshot(0);
                    let (lower_us, upper_us) =
                        target_limits(None, snapshot.applied_ceiling_us);
                    assert!(
                        (lower_us..=upper_us).contains(&snapshot.target_us),
                        "上限の解除で目標が範囲の外に出た: target={}",
                        snapshot.target_us
                    );
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
                    // 鳴り遅れの p50。閉ループは時間圧縮の不感帯 (20 ms) の分を引いてから
                    // 判断するため、不感帯と許容の境界の前後を必ず含める
                    let lateness_p50_us = match noprop::sample_weighted_index(ctx, &LATENESS_WEIGHTS)
                    {
                        0 => None,
                        1 => Some(
                            noprop::sample_with_boundaries(
                                ctx,
                                &[
                                    0,
                                    AUDIO_PLAYOUT_STRETCH_MIN_US - 1,
                                    AUDIO_PLAYOUT_STRETCH_MIN_US,
                                ],
                                noprop::Ratio::one_nth(3),
                                |ctx| {
                                    noprop::sample_usize_in(
                                        ctx,
                                        0..=AUDIO_DELAY_FEEDBACK_TOLERANCE_US as usize,
                                    ) as i64
                                },
                            ),
                        ),
                        2 => Some(noprop::sample_with_boundaries(
                            ctx,
                            &[
                                AUDIO_PLAYOUT_STRETCH_MIN_US,
                                AUDIO_PLAYOUT_STRETCH_MIN_US + AUDIO_DELAY_FEEDBACK_TOLERANCE_US - 1,
                                AUDIO_PLAYOUT_STRETCH_MIN_US + AUDIO_DELAY_FEEDBACK_TOLERANCE_US,
                                AUDIO_PLAYOUT_STRETCH_MIN_US + AUDIO_DELAY_FEEDBACK_TOLERANCE_US + 1,
                            ],
                            noprop::Ratio::one_nth(3),
                            |ctx| {
                                noprop::sample_usize_in(
                                    ctx,
                                    AUDIO_PLAYOUT_STRETCH_MIN_US as usize
                                        ..=AUDIO_DELAY_FEEDBACK_MAX_US as usize,
                                ) as i64
                            },
                        )),
                        _ => Some(noprop::sample_usize_in(ctx, 0..=2_000_000) as i64),
                    };
                    // 閉ループが根拠にする遅れは、不感帯の分を引いた値である
                    let effective_lateness_p50_us = lateness_p50_us.map(|lateness_p50_us| {
                        lateness_p50_us
                            .saturating_sub(AUDIO_PLAYOUT_STRETCH_MIN_US)
                            .max(0)
                    });
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

                    // 圧縮 (時間圧縮) の増分。購読側が累積の差として渡す値である。0 (圧縮なし)
                    // と、閾値 (5 ms/秒) の前後、大きな値を混ぜる
                    let compression_us = match noprop::sample_weighted_index(ctx, &COMPRESSION_WEIGHTS)
                    {
                        0 => 0,
                        1 => noprop::sample_with_boundaries(
                            ctx,
                            &[0, AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND],
                            noprop::Ratio::one_nth(3),
                            |ctx| {
                                noprop::sample_usize_in(
                                    ctx,
                                    0..=AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND as usize,
                                ) as i64
                            },
                        ),
                        2 => noprop::sample_with_boundaries(
                            ctx,
                            &[
                                AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND,
                                AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND + 1,
                            ],
                            noprop::Ratio::one_nth(3),
                            |ctx| noprop::sample_usize_in(ctx, 0..=20_000) as i64,
                        ),
                        _ => noprop::sample_usize_in(ctx, 0..=200_000) as i64,
                    };
                    // 揺らぎだけから求めた目標遅延。上限を戻してよいかの判断に使うため、
                    // 境界 (100 ms) の前後を必ず含める
                    let jitter_target_us = noprop::sample_with_boundaries(
                        ctx,
                        &[
                            0,
                            AUDIO_DELAY_FEEDBACK_MAX_TARGET_US - 1,
                            AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
                            AUDIO_DELAY_FEEDBACK_MAX_TARGET_US + 1,
                        ],
                        noprop::Ratio::one_nth(3),
                        |ctx| noprop::sample_usize_in(ctx, 0..=300_000) as i64,
                    );

                    let previous_target_us = feedback.feedback_target_us();
                    let previous_adjustments = feedback.snapshot(0).adjustments;
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
                    // 適用する上限の状態を、閉ループと同じ規則で写し取る。圧縮の増分は制御
                    // (間隔が空いた観測) まで積み、制御のたびに速さを求めて判断する
                    pending_compression_us =
                        pending_compression_us.saturating_add(compression_us.max(0));
                    if interval_passed {
                        let span_us = elapsed_us.unwrap_or(0);
                        let consumed_us = pending_compression_us;
                        pending_compression_us = 0;
                        let (widened, compression_streak, quiet_streak) = advance_ceiling(
                            widened_ceiling,
                            compression_streak_us,
                            quiet_streak_us,
                            span_us,
                            consumed_us,
                            jitter_target_us,
                        );
                        widened_ceiling = widened;
                        compression_streak_us = compression_streak;
                        quiet_streak_us = quiet_streak;
                        if span_us > 0 && consumed_us > 0 {
                            saw_compression_rate.set(true);
                        }
                    }
                    feedback.update(observation_with_compression(
                        now_us,
                        lateness_p50_us,
                        backlog_misses,
                        backlog_us,
                        compression_us,
                        jitter_target_us,
                    ));
                    if interval_passed {
                        last_update_at_us = Some(now_us);
                        consumed_backlog_misses = backlog_misses;
                        consumed_backlog_us = backlog_us;
                        if backlog_returned {
                            saw_backlog_returned.set(true);
                        }
                    }

                    let snapshot = feedback.snapshot(0);
                    let ceiling_us = feedback.ceiling_us();
                    // 適用する上限も、写し取った状態と同じ規則で求める
                    let expected_applied_ceiling_us =
                        applied_ceiling_us(ceiling_us, widened_ceiling);
                    let (lower_us, upper_us) =
                        target_limits(ceiling_us, snapshot.applied_ceiling_us);
                    assert_eq!(
                        snapshot.applied_ceiling_us, expected_applied_ceiling_us,
                        "適用する上限が規則と違う: widened={widened_ceiling} ceiling={ceiling_us:?} {snapshot:?}"
                    );
                    // 規則を写し取った値だけでなく、値そのものの範囲も確かめる
                    assert!(
                        (AUDIO_DELAY_FEEDBACK_MAX_TARGET_US..=AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US)
                            .contains(&snapshot.applied_ceiling_us),
                        "適用する上限が 100 ms から 160 ms の外に出た: {snapshot:?}"
                    );
                    // 明示設定があるときは、広げる分もその値までに切る (既定の上限は下回らない)。
                    // 定数の大小はここで固定されているため clamp を使ってよい
                    let widened_limit_us = ceiling_us.map_or(
                        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
                        |ceiling_us| {
                            ceiling_us.clamp(
                                AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
                                AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
                            )
                        },
                    );
                    assert!(
                        snapshot.applied_ceiling_us <= widened_limit_us,
                        "適用する上限が明示設定を超えた: ceiling={ceiling_us:?} limit={widened_limit_us} {snapshot:?}"
                    );
                    // 計器に出す遅れは、閉ループが判断に使った値 (不感帯を引いた後) である
                    assert_eq!(
                        snapshot.lateness_p50_us, effective_lateness_p50_us,
                        "計器の遅れが不感帯を引いた値でない: lateness={lateness_p50_us:?} {snapshot:?}"
                    );
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
                    // 実際に使う値は、学習値と閉ループの目標の大きい方を、いま適用する上限で
                    // 切った値である
                    let jitter_us = noprop::sample_usize_in(ctx, 0..500_000) as i64;
                    let applied_us = feedback.target_delay_us(jitter_us);
                    assert!(
                        applied_us <= snapshot.applied_ceiling_us,
                        "実際に使う値が適用する上限を超えた: jitter={jitter_us} applied={applied_us} ceiling={} {snapshot:?}",
                        snapshot.applied_ceiling_us
                    );
                    assert!(
                        applied_us >= jitter_us.min(snapshot.applied_ceiling_us),
                        "実際に使う値が学習値 (上限の内側) を下回った: jitter={jitter_us} applied={applied_us} {snapshot:?}"
                    );
                    // 学習値が適用する上限を超えるときは、その上限ちょうどになる
                    if jitter_us >= snapshot.applied_ceiling_us {
                        assert_eq!(
                            applied_us, snapshot.applied_ceiling_us,
                            "学習値が上限を超えたら上限ちょうどになる: jitter={jitter_us} {snapshot:?}"
                        );
                    }
                    // 観測を受けた後は、閉ループの目標 (上限の内側) も反映する。観測を
                    // 受ける前は閉ループの目標を使わないため確かめない
                    if !matches!(snapshot.reason, AudioDelayFeedbackReason::Initial) {
                        assert!(
                            applied_us >= snapshot.target_us.min(snapshot.applied_ceiling_us),
                            "実際に使う値が閉ループの目標 (上限の内側) を下回った: target={} applied={applied_us}",
                            snapshot.target_us
                        );
                    }
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
                                effective_lateness_p50_us.is_some_and(
                                    |effective_lateness_p50_us| effective_lateness_p50_us
                                        > AUDIO_DELAY_FEEDBACK_TOLERANCE_US
                                ),
                                "不感帯を引いた遅れが許容の中なのに理由が遅れになった: lateness={lateness_p50_us:?} effective={effective_lateness_p50_us:?}"
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
                                effective_lateness_p50_us.is_some_and(
                                    |effective_lateness_p50_us| effective_lateness_p50_us
                                        <= AUDIO_DELAY_FEEDBACK_TOLERANCE_US
                                ),
                                "不感帯を引いた遅れが許容の外なのに理由が収束になった: lateness={lateness_p50_us:?} effective={effective_lateness_p50_us:?}"
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
                        AudioDelayFeedbackReason::Gap => {
                            // このテストは穴を渡していない (穴の分岐は別のテストで見る)
                            panic!("穴を渡していないのに理由が穴になった: {snapshot:?}");
                        }
                        AudioDelayFeedbackReason::Underrun => {
                            // このテストは枯れを渡していない (枯れの分岐は別のテストで見る)
                            panic!("枯れを渡していないのに理由が枯れになった: {snapshot:?}");
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
        saw_compression_rate.get(),
        "圧縮の速さを観測する場合を通っていない\n{runner}"
    );
    Ok(())
}

#[test]
fn the_ceiling_widens_with_compression_and_returns_when_quiet() -> noprop::TestResult {
    // 圧縮が続く区間と、静かな区間を任意の長さで作る。適用する上限が、広げる条件 (圧縮の
    // 速さが 5 ms/秒 を超える状態が 2 秒) と戻す条件 (圧縮が 0 で、揺らぎだけの目標が
    // 100 ms 以下である状態が 15 秒) を満たしたときにだけ変わることを確かめる。ここでは
    // 観測を 1 秒間隔で渡すため、1 回の制御が 1 秒ぶんになる
    let saw_widened = Cell::new(false);
    let saw_released = Cell::new(false);
    let saw_kept_widened_by_the_path = Cell::new(false);

    let mut runner = test_runner()?;
    runner.run(64, |ctx| {
        let mut feedback = AudioDelayFeedback::new();
        // 明示設定を混ぜる。適用する上限は、広げる分にもこの値を掛ける
        let ceiling_us = if noprop::sample_bool(ctx) {
            let ceiling_us = noprop::sample_usize_in(ctx, 0..=300_000) as i64;
            feedback.set_ceiling_us(Some(ceiling_us));
            Some(ceiling_us)
        } else {
            None
        };
        // 閉ループと同じ規則で写し取る。最初の制御は前回が無いため、制御からの時間を 0 と
        // して扱う (速さを求められない)
        let mut widened = false;
        let mut compression_streak_us = 0i64;
        let mut quiet_streak_us = 0i64;
        let mut last_control_at_us: Option<i64> = None;
        let mut now_us = 0i64;

        // 圧縮が続く区間。速さは 20 ms/秒 以上にする (閾値の 5 ms/秒 を必ず超える)。長さは
        // 広げる条件を必ず満たすようにする (最初の制御は前回が無いため、連続時間を数えない)
        let confirm_steps =
            (AUDIO_DELAY_FEEDBACK_COMPRESSION_CONFIRM_US / AUDIO_DELAY_FEEDBACK_INTERVAL_US) as usize;
        let compression_span = confirm_steps + 1 + noprop::sample_usize_in(ctx, 0..=5);
        for _ in 0..compression_span {
            now_us += AUDIO_DELAY_FEEDBACK_INTERVAL_US;
            let compression_us = noprop::sample_usize_in(ctx, 20_000..=100_000) as i64;
            let span_us = last_control_at_us.map_or(0, |last_control_at_us| {
                now_us.saturating_sub(last_control_at_us)
            });
            feedback.update(observation_with_compression(
                now_us,
                Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
                0,
                0,
                compression_us,
                0,
            ));
            let was_widened = widened;
            (widened, compression_streak_us, quiet_streak_us) = advance_ceiling(
                widened,
                compression_streak_us,
                quiet_streak_us,
                span_us,
                compression_us,
                0,
            );
            last_control_at_us = Some(now_us);
            if !was_widened && widened {
                saw_widened.set(true);
            }
            assert_eq!(
                feedback.snapshot(0).applied_ceiling_us,
                applied_ceiling_us(ceiling_us, widened),
                "圧縮が続く区間で適用する上限が規則と違う: compression_span={compression_span} span={span_us} widened={widened} {ceiling_us:?}"
            );
        }

        // 圧縮が 0 で、揺らぎだけの目標が 100 ms 以下の区間。長さは戻す条件を必ず満たすように
        // する (経路がまだ 100 ms を必要としている回を混ぜ、その回で数え直すことも確かめる)
        let release_steps =
            (AUDIO_DELAY_FEEDBACK_ARTIFACT_RELEASE_US / AUDIO_DELAY_FEEDBACK_INTERVAL_US) as usize;
        let quiet_span = release_steps + 2 + noprop::sample_usize_in(ctx, 0..=20);
        // 経路が乱れる回は先頭に置く (後ろに戻す条件ぶんの長さを残す)
        let disturbed_at = noprop::sample_usize_in(ctx, 0..=1);
        for index in 0..quiet_span {
            now_us += AUDIO_DELAY_FEEDBACK_INTERVAL_US;
            let disturbed = index == disturbed_at;
            let jitter_target_us = if disturbed {
                AUDIO_DELAY_FEEDBACK_MAX_TARGET_US + 50_000
            } else {
                noprop::sample_usize_in(ctx, 0..=AUDIO_DELAY_FEEDBACK_MAX_TARGET_US as usize)
                    as i64
            };
            let span_us = last_control_at_us.map_or(0, |last_control_at_us| {
                now_us.saturating_sub(last_control_at_us)
            });
            feedback.update(observation_with_compression(
                now_us,
                Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
                0,
                0,
                0,
                jitter_target_us,
            ));
            let was_widened = widened;
            (widened, compression_streak_us, quiet_streak_us) = advance_ceiling(
                widened,
                compression_streak_us,
                quiet_streak_us,
                span_us,
                0,
                jitter_target_us,
            );
            last_control_at_us = Some(now_us);
            if disturbed {
                // 経路がまだ 100 ms を必要としているため、どちらの条件も満たさない
                saw_kept_widened_by_the_path.set(true);
            }
            if was_widened && !widened {
                saw_released.set(true);
            }
            assert_eq!(
                feedback.snapshot(0).applied_ceiling_us,
                applied_ceiling_us(ceiling_us, widened),
                "静かな区間で適用する上限が規則と違う: index={index} disturbed={disturbed} widened={widened} {ceiling_us:?}"
            );
            // 内部の目標は、常に適用する上限を超えない
            assert!(
                feedback.feedback_target_us() <= feedback.snapshot(0).applied_ceiling_us,
                "内部の目標が適用する上限を超えた"
            );
        }
        Ok(())
    })?;

    assert!(
        saw_widened.get(),
        "圧縮が続いて上限を広げる場合を通っていない\n{runner}"
    );
    assert!(
        saw_released.get(),
        "静かな状態が続いて上限を戻す場合を通っていない\n{runner}"
    );
    assert!(
        saw_kept_widened_by_the_path.get(),
        "経路が 100 ms を超えるため戻さない場合を通っていない\n{runner}"
    );
    Ok(())
}

/// アンダーランは目標を上げ、適用する上限を超えない
///
/// 枯れが無い観測では従来どおり減る。枯れがある観測では、枯れた長さ (1 回の増加の上限まで)
/// だけ目標が上がり、理由は枯れを優先する。適用する上限 (圧縮が続くときだけ広げた 160 ms)
/// を超えない。
#[test]
fn underruns_raise_the_target_within_the_ceiling() -> noprop::TestResult {
    let saw_underrun = std::cell::Cell::new(0usize);
    let saw_no_underrun = std::cell::Cell::new(0usize);
    let saw_ceiling = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let mut feedback = AudioDelayFeedback::new();
        // 適用する上限を 160 ms へ広げる (これが無いと初期値が上限と同じで増える余地が無い)
        for index in 0..3 {
            feedback.update(observation_with_compression(
                index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
                Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
                0,
                0,
                AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND + 1_000,
                0,
            ));
        }
        assert_eq!(
            feedback.snapshot(0).applied_ceiling_us,
            AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
            "上限を広げられていない"
        );
        let mut now_us = 3 * AUDIO_DELAY_FEEDBACK_INTERVAL_US;
        // 枯れで上限まで上げる場合を確実に通す (この後の増分が上限で切られることも見る)。
        // 初期値は下限 (50 ms) であるため、40 ms ずつ 3 回で 160 ms に達する
        for _ in 0..3 {
            now_us += AUDIO_DELAY_FEEDBACK_INTERVAL_US;
            feedback.update(observation_with_underrun(now_us, 1_000));
        }
        assert_eq!(
            feedback.feedback_target_us(),
            AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
            "枯れで上限まで上がらない"
        );
        saw_ceiling.set(1);
        let mut previous_target_us = feedback.feedback_target_us();
        // 15 秒で上限が戻らない範囲にとどめる (静かな状態が続くと 15 秒で戻る)
        for _ in 0..noprop::sample_usize_in(ctx, 1..=8) {
            now_us += AUDIO_DELAY_FEEDBACK_INTERVAL_US;
            // 0 (枯れなし) と、1 ms、1 回の増加の上限 (40 ms)、その前後、大きな値を混ぜる
            let underrun_ms = noprop::sample_with_boundaries(
                ctx,
                &[
                    0,
                    1,
                    AUDIO_DELAY_FEEDBACK_MAX_STEP_US / 1_000,
                    AUDIO_DELAY_FEEDBACK_MAX_STEP_US / 1_000 + 1,
                    1_000,
                ],
                noprop::Ratio::one_nth(3),
                |ctx| noprop::sample_usize_in(ctx, 0..=1_000) as i64,
            );
            feedback.update(observation_with_underrun(now_us, underrun_ms));
            let snapshot = feedback.snapshot(0);
            let target_us = feedback.feedback_target_us();
            assert!(
                target_us <= snapshot.applied_ceiling_us,
                "目標が適用する上限を超えた: target={target_us} ceiling={} {snapshot:?}",
                snapshot.applied_ceiling_us
            );
            if underrun_ms > 0 {
                saw_underrun.set(saw_underrun.get() + 1);
                assert_eq!(
                    snapshot.reason,
                    AudioDelayFeedbackReason::Underrun,
                    "枯れが理由にならない: {snapshot:?}"
                );
                // 枯れた長さ (上限まで) だけ増える。上限に張り付いたときは残りの分だけ動く
                let step_us = underrun_ms
                    .min(AUDIO_DELAY_FEEDBACK_MAX_STEP_US / 1_000)
                    .saturating_mul(1_000);
                let expected_us = previous_target_us
                    .saturating_add(step_us)
                    .max(AUDIO_DELAY_FEEDBACK_MIN_US)
                    .min(snapshot.applied_ceiling_us);
                assert_eq!(
                    target_us, expected_us,
                    "枯れによる増分が規則と違う: underrun_ms={underrun_ms} previous={previous_target_us} {snapshot:?}"
                );
                if target_us == snapshot.applied_ceiling_us {
                    saw_ceiling.set(saw_ceiling.get() + 1);
                }
            } else {
                saw_no_underrun.set(saw_no_underrun.get() + 1);
                assert!(
                    target_us <= previous_target_us,
                    "枯れが無いのに目標が上がった: previous={previous_target_us} target={target_us} {snapshot:?}"
                );
            }
            previous_target_us = target_us;
        }
        Ok(())
    })?;
    assert!(
        saw_underrun.get() > 0,
        "枯れで目標が上がる場合を通っていない\n{runner}"
    );
    assert!(
        saw_no_underrun.get() > 0,
        "枯れが無い場合を通っていない\n{runner}"
    );
    assert!(
        saw_ceiling.get() > 0,
        "上限に張り付く場合を通っていない\n{runner}"
    );
    Ok(())
}

/// 穴で広げた上限の保持中も、現在の明示上限を適用する
///
/// 穴より前に明示上限を設定しても、保持中に下げても、次の適用値は同じ予算に収まる。
/// 揺らぎだけの目標が内部目標より大きい場合を通し、内部目標だけの切り下げでは通らない
/// 検証にする (moqt-js の `audioDelayFeedback.prop.ts` と同じ)。
#[test]
fn gap_ceiling_uses_the_current_explicit_ceiling() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(64, |ctx| {
        // 既定の上限 (100 ms) 以上、artifact ceiling (160 ms) 未満の予算を選ぶ
        let ceiling_us =
            AUDIO_DELAY_FEEDBACK_MAX_TARGET_US + noprop::sample_usize_in(ctx, 0..60_000) as i64;
        let mut feedback = AudioDelayFeedback::new();
        // 穴より前に広げた状態を作る。揺らぎだけの目標の方を大きくし、内部目標だけを
        // 切り下げても通らない検証にする
        for index in 0..2 {
            let mut observation =
                observation(index as i64 * AUDIO_DELAY_FEEDBACK_INTERVAL_US, None, 0, 0);
            observation.jitter_target_us = AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US;
            observation.gap_count = index as u64;
            observation.gap_us = index as i64 * 60_000;
            feedback.update(observation);
        }
        assert_eq!(
            feedback.target_delay_us(AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US),
            AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
            "穴で広げた上限まで実際に使うこと"
        );
        feedback.set_ceiling_us(Some(ceiling_us));
        assert_eq!(
            feedback.target_delay_us(AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US),
            ceiling_us,
            "保持中の穴の上限にも新しい予算を使うこと: ceiling={ceiling_us}"
        );
        Ok(())
    })?;
    Ok(())
}

/// 穴で広げた上限の保持と、穴が続く間の artifact ceiling の広がりを検証する
///
/// 観測を 1 秒間隔で渡す。穴で広げた上限は最後の穴から 5 秒の間だけ保持し、穴が 2 秒続くと
/// artifact ceiling を広げ、穴が止まって 15 秒続くと 100 ms へ戻す。明示設定があるときは、
/// 広げる分もその値で切る。
#[test]
fn gap_ceiling_holds_and_returns_by_the_rules() -> noprop::TestResult {
    let saw_gap = Cell::new(false);
    let saw_hold = Cell::new(false);
    let saw_release = Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(64, |ctx| {
        // 明示設定を混ぜる (穴の上限も artifact ceiling もこの値で切る)
        let ceiling_us = match noprop::sample_usize_in(ctx, 0..3) {
            0 => None,
            1 => Some(
                noprop::sample_usize_in(
                    ctx,
                    0..=AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US as usize,
                ) as i64,
            ),
            _ => {
                Some(noprop::sample_usize_in(ctx, 0..=AUDIO_DELAY_FEEDBACK_MAX_US as usize) as i64)
            }
        };
        // 穴や圧縮で広げてよい上限
        let artifact_limit_us = AUDIO_DELAY_FEEDBACK_MAX_TARGET_US.max(
            AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US
                .min(ceiling_us.unwrap_or(AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US)),
        );
        let mut feedback = AudioDelayFeedback::new();
        if let Some(ceiling_us) = ceiling_us {
            feedback.set_ceiling_us(Some(ceiling_us));
        }

        // 操作列を作る。最初の 3 回の穴で artifact ceiling を広げ、その直後の穴なしの観測で
        // 保持に入ることを確かめる。最後に 20 秒の静かな区間を置き、戻ることを保証する
        let mut steps = vec![
            // (累積の穴の数, 累積の穴の長さ, 揺らぎだけの目標遅延)
            (1, 20_000, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US),
            (2, 40_000, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US),
            (3, 60_000, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US),
            (3, 60_000, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US),
        ];
        let mut gap_count = 3u64;
        let mut gap_us = 60_000i64;
        for _ in 0..noprop::sample_usize_in(ctx, 0..=12) {
            match noprop::sample_weighted_index(ctx, &GAP_WEIGHTS) {
                0 => {}
                1 => {
                    gap_count += 1;
                    gap_us += noprop::sample_usize_in(ctx, 1_000..=40_000) as i64;
                }
                2 => {
                    gap_count += noprop::sample_usize_in(ctx, 2..=3) as u64;
                    gap_us += noprop::sample_usize_in(ctx, 60_000..=200_000) as i64;
                }
                _ => {
                    // 購読のやり直しで累積が 0 に戻った状態 (負の差で増やさない)
                    gap_count = 0;
                    gap_us = 0;
                }
            }
            let jitter_target_us = noprop::sample_with_boundaries(
                ctx,
                &[
                    0,
                    AUDIO_DELAY_FEEDBACK_MAX_TARGET_US - 1,
                    AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
                    AUDIO_DELAY_FEEDBACK_MAX_TARGET_US + 1,
                ],
                noprop::Ratio::one_nth(4),
                |ctx| {
                    noprop::sample_usize_in(
                        ctx,
                        0..=AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US as usize,
                    ) as i64
                },
            );
            steps.push((gap_count, gap_us, jitter_target_us));
        }
        // 静かな区間。穴は出さず、揺らぎだけの目標も 100 ms 以下にする (戻る条件を満たす)
        for _ in 0..20 {
            steps.push((gap_count, gap_us, 0));
        }

        // SUT と同じ規則で、穴で広げた上限と artifact ceiling を写し取る
        let mut previous_gap_count = 0u64;
        let mut previous_gap_us = 0i64;
        let mut last_gap_at_us: Option<i64> = None;
        let mut gap_ceiling_us: Option<i64> = None;
        let mut gap_ceiling_until_us = 0i64;
        let mut widened = false;
        let mut compression_streak_us = 0i64;
        let mut quiet_streak_us = 0i64;
        let mut last_control_at_us: Option<i64> = None;
        let mut now_us = 0i64;

        for (step_index, (step_gap_count, step_gap_us, jitter_target_us)) in
            steps.iter().enumerate()
        {
            now_us += AUDIO_DELAY_FEEDBACK_INTERVAL_US;
            let step_gap_count = *step_gap_count;
            let step_gap_us = *step_gap_us;
            let jitter_target_us = *jitter_target_us;
            let gap_delta = step_gap_count.saturating_sub(previous_gap_count);
            let gap_us_delta = step_gap_us.saturating_sub(previous_gap_us).max(0);
            let span_us = last_control_at_us
                .map(|last_control_at_us| now_us.saturating_sub(last_control_at_us))
                .unwrap_or(0);
            let mut observation = observation(
                now_us,
                Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
                0,
                0,
            );
            observation.gap_count = step_gap_count;
            observation.gap_us = step_gap_us;
            observation.jitter_target_us = jitter_target_us;
            let previous_target_us = feedback.feedback_target_us();
            let previous_adjustments = feedback.snapshot(0).adjustments;
            // 穴への対処の基点 (SUT と同じく、今回の観測を反映する前の実際の深さ)
            let previous_applied_us = feedback.target_delay_us(jitter_target_us).max(0);

            // 穴 1 個で広げる上限のモデル (SUT と同じ式)
            if gap_delta > 0 {
                let widened_gap_us = artifact_limit_us.min(
                    previous_applied_us
                        .saturating_add(gap_us_delta)
                        .saturating_add(AUDIO_DELAY_FEEDBACK_MARGIN_US)
                        .max(AUDIO_DELAY_FEEDBACK_MAX_TARGET_US),
                );
                gap_ceiling_us = Some(gap_ceiling_us.unwrap_or(0).max(widened_gap_us));
                gap_ceiling_until_us =
                    now_us.saturating_add(AUDIO_DELAY_FEEDBACK_GAP_QUIET_US);
                last_gap_at_us = Some(now_us);
            } else if gap_ceiling_us.is_some() && now_us >= gap_ceiling_until_us {
                gap_ceiling_us = None;
            }
            // artifact ceiling のモデル (圧縮は 0 である)
            if gap_delta > 0 {
                compression_streak_us = compression_streak_us.saturating_add(span_us);
                quiet_streak_us = 0;
            } else if jitter_target_us <= AUDIO_DELAY_FEEDBACK_MAX_TARGET_US {
                quiet_streak_us = quiet_streak_us.saturating_add(span_us);
                compression_streak_us = 0;
            } else {
                compression_streak_us = 0;
                quiet_streak_us = 0;
            }
            let was_widened = widened;
            if !widened && compression_streak_us >= AUDIO_DELAY_FEEDBACK_COMPRESSION_CONFIRM_US {
                widened = true;
            } else if widened && quiet_streak_us >= AUDIO_DELAY_FEEDBACK_ARTIFACT_RELEASE_US {
                widened = false;
            }
            if was_widened && !widened {
                saw_release.set(true);
            }

            feedback.update(observation);
            last_control_at_us = Some(now_us);
            previous_gap_count = step_gap_count;
            previous_gap_us = step_gap_us;

            let snapshot = feedback.snapshot(jitter_target_us);
            // 適用する上限は、artifact ceiling と穴で広げた上限の大きい方である
            let expected_applied_ceiling_us = (if widened {
                artifact_limit_us
            } else {
                AUDIO_DELAY_FEEDBACK_MAX_TARGET_US
            })
            .max(if gap_ceiling_us.is_some() && now_us < gap_ceiling_until_us {
                gap_ceiling_us
                    .unwrap_or(AUDIO_DELAY_FEEDBACK_MAX_TARGET_US)
                    .min(artifact_limit_us)
            } else {
                0
            });
            assert_eq!(
                snapshot.applied_ceiling_us, expected_applied_ceiling_us,
                "適用する上限が規則と違う: step={step_index} {snapshot:?}"
            );
            assert!(
                (AUDIO_DELAY_FEEDBACK_MAX_TARGET_US..=artifact_limit_us)
                    .contains(&snapshot.applied_ceiling_us),
                "適用する上限が範囲の外に出た: step={step_index} {snapshot:?}"
            );
            let (lower_us, upper_us) = target_limits(ceiling_us, snapshot.applied_ceiling_us);
            assert!(
                (lower_us..=upper_us).contains(&snapshot.target_us),
                "目標が上下限の外に出た: step={step_index} target={} lower={lower_us} upper={upper_us} {snapshot:?}",
                snapshot.target_us
            );
            // 実際に使う値は、受け取った揺らぎだけの目標との大きい方を適用の上限で切った値
            assert_eq!(
                snapshot.jitter_target_us, jitter_target_us,
                "揺らぎだけの目標をそのまま出す: step={step_index} {snapshot:?}"
            );
            assert_eq!(
                snapshot.applied_us,
                feedback.target_delay_us(jitter_target_us),
                "実際に使う値が snapshot と違う: step={step_index} {snapshot:?}"
            );
            assert!(
                snapshot.applied_us <= snapshot.applied_ceiling_us,
                "実際に使う値が適用する上限を超えた: step={step_index} {snapshot:?}"
            );

            if gap_delta > 0 {
                saw_gap.set(true);
                assert_eq!(
                    snapshot.reason,
                    AudioDelayFeedbackReason::Gap,
                    "穴が理由にならない: step={step_index} {snapshot:?}"
                );
                assert_eq!(
                    snapshot.adjustments,
                    previous_adjustments + u64::from(snapshot.last_change_us != 0),
                    "動かした回数の数え方が変わった: step={step_index} {snapshot:?}"
                );
            } else if last_gap_at_us.is_some_and(|last_gap_at_us| {
                now_us.saturating_sub(last_gap_at_us) < AUDIO_DELAY_FEEDBACK_GAP_QUIET_US
            }) {
                // 穴の後のホールド中は、遅れが許容内でも下げない
                saw_hold.set(true);
                assert_eq!(
                    snapshot.reason,
                    AudioDelayFeedbackReason::Waiting,
                    "穴の後のホールドで待たない: step={step_index} {snapshot:?}"
                );
                assert_eq!(
                    snapshot.target_us, previous_target_us,
                    "穴の後のホールドで目標が動いた: step={step_index} {snapshot:?}"
                );
                assert_eq!(
                    snapshot.adjustments, previous_adjustments,
                    "穴の後のホールドで動かした回数が増えた: step={step_index} {snapshot:?}"
                );
            } else {
                assert_eq!(
                    snapshot.reason,
                    AudioDelayFeedbackReason::Settled,
                    "穴が無くホールドの外では減らす判断へ進む: step={step_index} {snapshot:?}"
                );
            }
        }

        // 静かな区間の最後では、artifact ceiling も穴で広げた上限も戻っている
        assert_eq!(
            feedback.snapshot(0).applied_ceiling_us,
            AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
            "静かな状態が続いた後に上限が戻っていない"
        );
        Ok(())
    })?;
    assert!(
        saw_gap.get(),
        "穴で目標を上げる場合を通っていない\n{runner}"
    );
    assert!(
        saw_hold.get(),
        "穴の後のホールドで下げない場合を通っていない\n{runner}"
    );
    assert!(
        saw_release.get(),
        "静かな状態が続いて上限を戻す場合を通っていない\n{runner}"
    );
    Ok(())
}
