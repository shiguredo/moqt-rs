//! 音声の目標遅延を決める閉ループのテスト
//!
//! 公開 API (`AudioDelayFeedback`) の契約を確認する。moqt-js の
//! `src/audioDelayFeedback.test.ts` が固定する規則 (増減の条件、速さ、上下限、
//! `targetLatency` との関係) を移植する。

use shiguredo_moqt::playout::feedback::{
    AUDIO_DELAY_FEEDBACK_DECREASE_US_PER_SECOND, AUDIO_DELAY_FEEDBACK_INTERVAL_US,
    AUDIO_DELAY_FEEDBACK_MAX_STEP_US, AUDIO_DELAY_FEEDBACK_MAX_US, AUDIO_DELAY_FEEDBACK_MIN_US,
    AUDIO_DELAY_FEEDBACK_START_US, AUDIO_DELAY_FEEDBACK_TOLERANCE_US, AudioDelayFeedback,
    AudioDelayFeedbackReason,
};
use shiguredo_moqt::playout::timing::{AudioDelayFeedbackObservation, TimingSummary};

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
/// `lateness_p50_us` が None のときは「まだ鳴らしていない」観測になる。
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

#[test]
fn uses_the_jitter_target_until_the_first_observation() {
    // 閉ループの目標は初期値 (100 ms) から始める。ただし、実際に鳴った結果をまだ 1 つも
    // 観測していない間は使わない (観測が無い間に動かすと、まだ鳴らしていない間の表示の
    // 遅れが変わる)
    let feedback = AudioDelayFeedback::new();
    let snapshot = feedback.snapshot();
    assert_eq!(
        snapshot.target_us, AUDIO_DELAY_FEEDBACK_START_US,
        "初期値から始める"
    );
    assert_eq!(
        snapshot.reason,
        AudioDelayFeedbackReason::Initial,
        "理由が初期状態である"
    );
    assert_eq!(snapshot.adjustments, 0, "まだ動かしていない");
    assert_eq!(snapshot.lateness_p50_us, None, "観測が無い");
    // 観測が無い間は、既存の規則 (揺らぎだけから求めた目標) をそのまま使う
    assert_eq!(
        feedback.target_delay_us(0),
        0,
        "観測が無ければ閉ループの目標を使わない"
    );
    assert_eq!(
        feedback.target_delay_us(300_000),
        300_000,
        "揺らぎだけの目標をそのまま使う"
    );
    assert_eq!(feedback.feedback_target_us(), AUDIO_DELAY_FEEDBACK_START_US);

    // 観測を 1 つ受けたら、閉ループの目標 (初期値 100 ms) と大きい方を使う
    let mut feedback = AudioDelayFeedback::new();
    feedback.update(observation(
        0,
        Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
        0,
        0,
    ));
    assert_eq!(
        feedback.target_delay_us(0),
        AUDIO_DELAY_FEEDBACK_START_US,
        "観測後は初期値を使う"
    );
    assert_eq!(
        feedback.target_delay_us(300_000),
        300_000,
        "揺らぎだけの目標の方が大きければそれを使う"
    );
}

#[test]
fn lateness_raises_the_target_by_the_clamped_step() {
    // 遅れが続くなら目標を増やす。増やす量は「遅れ - 許容 + 余白」であり、1 回の上限
    // (40 ms) までにする。許容のすぐ上で往復しないよう余白を足す
    let mut feedback = AudioDelayFeedback::new();

    // 遅れ 60 ms: 60 - 10 + 20 = 70 だが、1 回の上限 (40) までにする
    feedback.update(observation(0, Some(60_000), 0, 0));
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_START_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "上限まで増やす"
    );
    assert_eq!(
        feedback.snapshot().reason,
        AudioDelayFeedbackReason::Lateness,
        "理由が遅れである"
    );

    // 遅れ 20 ms: 20 - 10 + 20 = 30 だけ増やす
    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(20_000),
        0,
        0,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_START_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US + 30_000,
        "遅れに応じた分だけ増やす"
    );
}

#[test]
fn no_adjustment_before_the_interval() {
    // 目標を動かすのは間隔ごとに 1 回だけである。増やした結果が観測へ現れる前に増やし
    // 続けると、上限に張り付いて遅延だけが増える
    let mut feedback = AudioDelayFeedback::new();

    feedback.update(observation(0, Some(100_000), 0, 0));
    let after_first_us = feedback.feedback_target_us();
    assert!(
        after_first_us > AUDIO_DELAY_FEEDBACK_START_US,
        "まず増える: {after_first_us}"
    );

    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US - 1,
        Some(100_000),
        0,
        0,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        after_first_us,
        "間隔の途中では動かさない"
    );
    assert_eq!(
        feedback.snapshot().reason,
        AudioDelayFeedbackReason::Waiting,
        "動かしていないことが分かる"
    );

    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(100_000),
        0,
        0,
    ));
    assert!(
        feedback.feedback_target_us() > after_first_us,
        "間隔が空いたら動かす: {}",
        feedback.feedback_target_us()
    );
}

#[test]
fn backlog_raises_the_target_by_the_clamped_step() {
    // 並べすぎで捨てたなら、捨てた長さぶんを吸収できるだけ増やす
    // (捨てた長さ + 余白、1 回の上限まで)
    let mut feedback = AudioDelayFeedback::new();

    feedback.update(observation(0, Some(0), 5, 100_000));
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_START_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "捨てた長さと余白の合計を上限で切って増やす"
    );
    assert_eq!(
        feedback.snapshot().reason,
        AudioDelayFeedbackReason::Backlog,
        "理由が並べすぎである"
    );

    // 捨てが続いていない (累積が増えていない) なら、遅れの判断へ移る
    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(0),
        5,
        100_000,
    ));
    assert_eq!(
        feedback.snapshot().reason,
        AudioDelayFeedbackReason::Settled,
        "捨てが無ければ減らす判断へ移る"
    );
}

#[test]
fn tolerance_decreases_the_target_proportionally_to_elapsed_time() {
    // 遅れが許容の中に収まり、捨てが無いなら目標を減らす。減らす速さは時間に比例させ、
    // 観測が疎でも速さを変えない
    let mut feedback = AudioDelayFeedback::new();
    feedback.update(observation(0, Some(100_000), 0, 0));
    let increased_us = feedback.feedback_target_us();

    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
        0,
        0,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        increased_us - AUDIO_DELAY_FEEDBACK_DECREASE_US_PER_SECOND,
        "1 秒ぶん減らす"
    );

    // 2 秒空いたら 2 秒ぶん減らす (速さは変わらない)
    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 3,
        Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
        0,
        0,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        increased_us - AUDIO_DELAY_FEEDBACK_DECREASE_US_PER_SECOND * 3,
        "空いた時間の分だけ減らす"
    );
}

#[test]
fn target_stays_between_the_floor_and_the_ceiling() {
    // 目標は下限 (80 ms) から上限 (300 ms) の間に収める。下限より下げると揺らぎを
    // 吸収できず、上限より上げると常に大きく遅れて鳴る
    let mut feedback = AudioDelayFeedback::new();

    // 十分な回数を減らしても下限を下回らない
    for index in 0..100 {
        feedback.update(observation(
            index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(0),
            0,
            0,
        ));
    }
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MIN_US,
        "下限で止まる"
    );

    // 十分な回数を増やしても上限を超えない
    for index in 0..100 {
        feedback.update(observation(
            1_000_000_000 + index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(400_000),
            0,
            0,
        ));
    }
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MAX_US,
        "上限で止まる"
    );
}

#[test]
fn explicit_ceiling_caps_the_target() {
    // 明示設定 (`targetLatency`) は、自動で決める目標の上限として尊重する。
    // 値が変わった時点で、既に超えていればその場で収める
    let mut feedback = AudioDelayFeedback::new();
    for index in 0..10 {
        feedback.update(observation(
            index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(400_000),
            0,
            0,
        ));
    }
    assert!(
        feedback.feedback_target_us() > 120_000,
        "まず自動で増える: {}",
        feedback.feedback_target_us()
    );

    feedback.set_ceiling_us(Some(120_000));
    assert_eq!(feedback.feedback_target_us(), 120_000, "上限に収める");
    assert_eq!(feedback.ceiling_us(), Some(120_000), "上限を保持する");

    // 上限より大きい値へは増えない。前回の更新から間隔が空いているため、毎回判断は行う
    for index in 0..10 {
        feedback.update(observation(
            1_000_000_000 + index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(400_000),
            0,
            0,
        ));
    }
    assert_eq!(feedback.feedback_target_us(), 120_000, "上限を超えない");

    // 上限を外すと、また自動で増える
    feedback.set_ceiling_us(None);
    feedback.update(observation(2_000_000_000, Some(400_000), 0, 0));
    assert!(
        feedback.feedback_target_us() > 120_000,
        "上限が無ければ増える: {}",
        feedback.feedback_target_us()
    );
    assert_eq!(feedback.ceiling_us(), None, "上限を外した状態になる");
}

#[test]
fn explicit_ceiling_below_the_floor_wins_over_the_floor() {
    // 明示の上限が下限を下回るときは上限を優先する (利用者の指定より自動で大きくしない)。
    // このとき実際に使う目標は、明示の値をそのまま使う
    let mut feedback = AudioDelayFeedback::new();
    feedback.update(observation(0, Some(400_000), 0, 0));
    feedback.set_ceiling_us(Some(50_000));
    assert_eq!(feedback.feedback_target_us(), 50_000, "上限まで下がる");
    assert_eq!(
        feedback.target_delay_us(0),
        50_000,
        "明示の上限をそのまま使う"
    );
    // 上限を外すと、下限より下にあった目標は下限まで戻る (自動で増やすのは次の判断から)
    feedback.set_ceiling_us(None);
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MIN_US,
        "下限まで戻る"
    );
    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(400_000),
        0,
        0,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MIN_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "次の判断で増える"
    );
}

#[test]
fn the_larger_of_the_jitter_and_feedback_targets_is_used() {
    // 揺らぎだけから求めた目標 (学習値) の方が大きいときは、それを使う。上限は自動で
    // 決める分にだけ掛け、学習値には掛けない (既存の揺らぎの吸収を変えないため)
    let mut feedback = AudioDelayFeedback::new();
    feedback.set_ceiling_us(Some(90_000));
    // 観測を受けて、閉ループの目標を使い始めさせる (遅れが続いているため上限まで増える)
    feedback.update(observation(0, Some(100_000), 0, 0));

    assert_eq!(
        feedback.target_delay_us(20_000),
        90_000,
        "閉ループの目標 (上限まで) を使う"
    );
    assert_eq!(
        feedback.target_delay_us(260_000),
        260_000,
        "学習値の方が大きければそれを使う"
    );

    let snapshot = feedback.snapshot();
    assert_eq!(snapshot.target_us, 90_000, "閉ループが決めた目標を出す");
    assert_eq!(snapshot.ceiling_us, Some(90_000), "明示された上限を出す");
}

#[test]
fn a_decreasing_backlog_does_not_raise_the_target_and_missing_lateness_waits() {
    // 購読のやり直しで累積の捨てが 0 に戻っても、負の差で増やさない。
    // 遅れの観測が無い (まだ鳴らしていない) ときは目標を動かさない
    let mut feedback = AudioDelayFeedback::new();
    feedback.update(observation(0, Some(0), 5, 100_000));
    assert_eq!(
        feedback.snapshot().reason,
        AudioDelayFeedbackReason::Backlog,
        "捨てで増える"
    );

    // 購読のやり直しで累積が 0 に戻る
    feedback.update(observation(AUDIO_DELAY_FEEDBACK_INTERVAL_US, Some(0), 0, 0));
    assert_eq!(
        feedback.snapshot().reason,
        AudioDelayFeedbackReason::Settled,
        "捨ての差が負でも増やさない"
    );

    // 遅れの観測が無いときは動かさない
    let before_us = feedback.feedback_target_us();
    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 2,
        None,
        0,
        0,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        before_us,
        "観測が無ければ動かさない"
    );
    assert_eq!(
        feedback.snapshot().reason,
        AudioDelayFeedbackReason::Waiting,
        "理由が待ちである"
    );
}

#[test]
fn the_first_settled_observation_does_not_decrease_the_target() {
    // 減らす速さは前回の観測からの時間で決めるため、前回が無いときは減らさない
    // (1 回の観測だけで「余裕が続いている」とは言えない)
    let mut feedback = AudioDelayFeedback::new();
    feedback.update(observation(
        0,
        Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
        0,
        0,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_START_US,
        "前回が無ければ減らさない"
    );
    assert_eq!(
        feedback.snapshot().reason,
        AudioDelayFeedbackReason::Waiting,
        "理由が待ちである"
    );
    assert_eq!(feedback.snapshot().adjustments, 0, "動かしていない");

    // 前回があるときは減らす
    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
        0,
        0,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_START_US - AUDIO_DELAY_FEEDBACK_DECREASE_US_PER_SECOND,
        "前回からの時間ぶん減らす"
    );
    assert_eq!(
        feedback.snapshot().reason,
        AudioDelayFeedbackReason::Settled,
        "理由が収束である"
    );
}

#[test]
fn snapshot_reports_the_latest_observation() {
    // 計器として、直近の観測の分布と動かした回数が読めること
    let mut feedback = AudioDelayFeedback::new();
    feedback.update(observation(0, Some(60_000), 0, 0));
    let snapshot = feedback.snapshot();
    assert_eq!(snapshot.lateness_p50_us, Some(60_000), "遅れの p50 を出す");
    assert_eq!(
        snapshot.start_delay_p50_us,
        Some(120_000),
        "到着から鳴るまでの p50 を出す"
    );
    assert_eq!(snapshot.slack_p50_us, Some(20_000), "余裕の p50 を出す");
    assert_eq!(
        snapshot.last_change_us, AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "直前の制御で動かした量を出す"
    );
    assert_eq!(snapshot.adjustments, 1, "動かした回数を数える");
    assert_eq!(snapshot.ceiling_us, None, "上限は無い");
    assert_eq!(
        feedback.snapshot().target_us,
        AUDIO_DELAY_FEEDBACK_START_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US
    );
}

#[test]
fn reset_restores_the_initial_state_and_keeps_the_ceiling() {
    // 学習を消すと初期状態に戻る (テストと、購読を作り直す呼び出し側のためのもの)
    let mut feedback = AudioDelayFeedback::new();
    feedback.set_ceiling_us(Some(200_000));
    feedback.update(observation(0, Some(100_000), 0, 0));
    feedback.reset();

    let snapshot = feedback.snapshot();
    assert_eq!(
        snapshot.target_us, AUDIO_DELAY_FEEDBACK_START_US,
        "初期値へ戻る"
    );
    assert_eq!(
        snapshot.reason,
        AudioDelayFeedbackReason::Initial,
        "理由も戻る"
    );
    assert_eq!(snapshot.adjustments, 0, "回数も戻る");
    assert_eq!(snapshot.lateness_p50_us, None, "観測も消える");
    // 明示設定の上限は残す (呼び出し側が決めた値である)
    assert_eq!(feedback.ceiling_us(), Some(200_000), "明示設定は残る");
    // リセット後は観測が無い扱いへ戻るため、学習値をそのまま使う
    assert_eq!(feedback.target_delay_us(0), 0, "観測が無い扱いへ戻る");
}
