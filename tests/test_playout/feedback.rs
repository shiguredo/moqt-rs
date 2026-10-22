//! 音声の目標遅延を決める閉ループのテスト
//!
//! 公開 API (`AudioDelayFeedback`) の契約を確認する。moqt-js の
//! `src/audioDelayFeedback.test.ts` が固定する規則 (増減の条件、速さ、上下限、
//! `targetLatency` との関係、補間で埋まらなかった穴への対処、再開時の初期化) を移植する。

use shiguredo_moqt::playout::feedback::{
    AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US, AUDIO_DELAY_FEEDBACK_ARTIFACT_RELEASE_US,
    AUDIO_DELAY_FEEDBACK_COMPRESSION_CONFIRM_US,
    AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND,
    AUDIO_DELAY_FEEDBACK_DECREASE_US_PER_SECOND, AUDIO_DELAY_FEEDBACK_GAP_QUIET_US,
    AUDIO_DELAY_FEEDBACK_INTERVAL_US, AUDIO_DELAY_FEEDBACK_MARGIN_US,
    AUDIO_DELAY_FEEDBACK_MAX_STEP_US, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
    AUDIO_DELAY_FEEDBACK_MAX_US, AUDIO_DELAY_FEEDBACK_MIN_US, AUDIO_DELAY_FEEDBACK_START_US,
    AUDIO_DELAY_FEEDBACK_TOLERANCE_US, AudioDelayFeedback, AudioDelayFeedbackReason,
};
use shiguredo_moqt::playout::scheduler::AUDIO_PLAYOUT_STRETCH_MIN_US;
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
/// `lateness_p50_us` が None のときは「まだ鳴らしていない」観測になる。圧縮は無し
/// (0)、揺らぎだけの目標は 0 (経路は 100 ms 以内で足りている)、穴も無し、到着基準の
/// 追加の下限も無しとして作る。
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

/// 穴 (補間で埋まらなかった無音) の累積を指定して観測を作る
///
/// `gap_count` / `gap_us` は購読の開始からの累積である。閉ループが前回との差を取る。
fn observation_with_gap(
    at_us: i64,
    lateness_p50_us: Option<i64>,
    jitter_target_us: i64,
    gap_count: u64,
    gap_us: i64,
) -> AudioDelayFeedbackObservation {
    let mut observation = observation(at_us, lateness_p50_us, 0, 0);
    observation.jitter_target_us = jitter_target_us;
    observation.gap_count = gap_count;
    observation.gap_us = gap_us;
    observation
}

/// 穴の累積と、到着基準の再生の下限を指定して観測を作る
///
/// 音声だけを購読するときは、到着基準で並べる再生に別の下限 (moqt-js は 140 ms) がある。
/// 穴への対処は、内部の目標ではなく実際に適用していた深さから増やす必要がある。
fn observation_with_gap_and_floor(
    at_us: i64,
    lateness_p50_us: Option<i64>,
    jitter_target_us: i64,
    gap_count: u64,
    gap_us: i64,
    arrival_delay_floor_us: i64,
) -> AudioDelayFeedbackObservation {
    let mut observation =
        observation_with_gap(at_us, lateness_p50_us, jitter_target_us, gap_count, gap_us);
    observation.arrival_delay_floor_us = Some(arrival_delay_floor_us);
    observation
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

/// 圧縮と、揺らぎだけの目標遅延を指定して観測を作る
///
/// `compression_us` は前の観測から増えた圧縮の長さ (増分)、`jitter_target_us` は揺らぎだけ
/// から求めた目標遅延である。どちらも既定 (0) は [`observation`] が作る。
fn observation_with_compression(
    at_us: i64,
    lateness_p50_us: Option<i64>,
    compression_us: i64,
    jitter_target_us: i64,
) -> AudioDelayFeedbackObservation {
    let mut observation = observation(at_us, lateness_p50_us, 0, 0);
    observation.compression_us = compression_us;
    observation.jitter_target_us = jitter_target_us;
    observation
}

/// 圧縮が続いた状態にして、適用する上限を 160 ms へ広げる
///
/// 広げる条件は「圧縮の速さが 5 ms/秒 を超える状態が 2 秒続くこと」である。初期値 (50 ms) は
/// 下限と同じであり、広げない状態では増やす余地が 90 ms しか無い。増分の規則を観測する
/// テストは、この関数で余地を作ってから観測を渡す。
///
/// 広げるまでの観測は収束 (遅れは許容の中) にしてある。初期値が下限と同じため、内部の
/// 目標は 50 ms のまま留まる。
fn widen_ceiling(feedback: &mut AudioDelayFeedback) {
    for index in 0..3 {
        feedback.update(observation_with_compression(
            index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
            AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND + 1_000,
            0,
        ));
    }
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "適用する上限が広がっていない"
    );
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MIN_US,
        "広げるまでの収束で内部の目標が下限に留まっていない"
    );
}

/// 遅れが許容の中に収まっている観測だけを与え、目標が下限に留まることを確かめる
///
/// 初期値は下限 (50 ms) と同じである。ここでは 5 秒ぶんの安定した観測を与え、増える向きの
/// 規則を確認する前提を作る (moqt-js の `observeStableAtMinimum` と同じ)。
fn observe_stable_at_minimum(feedback: &mut AudioDelayFeedback) {
    for index in 0..6 {
        feedback.update(observation(
            index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
            0,
            0,
        ));
    }
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MIN_US,
        "安定した観測で目標が下限に留まっていない"
    );
}

#[test]
fn uses_the_jitter_target_until_the_first_observation() {
    // 閉ループの目標は初期値 (50 ms) から始める。ただし、実際に鳴った結果をまだ 1 つも
    // 観測していない間は使わない (観測が無い間に動かすと、まだ鳴らしていない間の表示の
    // 遅れが変わる)
    let feedback = AudioDelayFeedback::new();
    let snapshot = feedback.snapshot(0);
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
    // 観測が無い間は、既存の規則 (揺らぎだけから求めた目標) をそのまま使う。ただし上限
    // (実際に使う値の上限) は掛ける
    assert_eq!(
        feedback.target_delay_us(0),
        0,
        "観測が無ければ閉ループの目標を使わない"
    );
    assert_eq!(
        feedback.target_delay_us(AUDIO_DELAY_FEEDBACK_MAX_TARGET_US),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "揺らぎだけの目標をそのまま使う"
    );
    assert_eq!(feedback.feedback_target_us(), AUDIO_DELAY_FEEDBACK_START_US);

    // 観測を 1 つ受けたら、閉ループの目標 (初期値 50 ms) と大きい方を使う
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
        feedback.target_delay_us(AUDIO_DELAY_FEEDBACK_MAX_TARGET_US),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "揺らぎだけの目標の方が大きければそれを使う"
    );
}

#[test]
fn lateness_raises_the_target_by_the_clamped_step() {
    // 遅れが続くなら目標を増やす。増やす量は「遅れ - 許容 + 余白」であり、1 回の上限
    // (40 ms) までにする。許容のすぐ上で往復しないよう余白を足す。遅れからは時間圧縮の
    // 不感帯 (20 ms) の分を引く (playout が意図して残した遅れでは目標を動かさない)
    let mut feedback = AudioDelayFeedback::new();
    widen_ceiling(&mut feedback);

    // 遅れ 60 ms: 不感帯を引いた 40 ms に対して 40 - 10 + 20 = 50 だが、1 回の上限
    // (40) までにする
    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 3,
        Some(60_000),
        0,
        0,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MIN_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "上限まで増やす"
    );
    assert_eq!(
        feedback.snapshot(0).reason,
        AudioDelayFeedbackReason::Lateness,
        "理由が遅れである"
    );

    // 遅れ 40 ms: 不感帯を引いた 20 ms に対して 20 - 10 + 20 = 30 だけ増やす
    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 4,
        Some(40_000),
        0,
        0,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MIN_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US + 30_000,
        "遅れに応じた分だけ増やす"
    );
}

#[test]
fn no_adjustment_before_the_interval() {
    // 目標を動かすのは間隔ごとに 1 回だけである。増やした結果が観測へ現れる前に増やし
    // 続けると、上限に張り付いて遅延だけが増える
    let mut feedback = AudioDelayFeedback::new();
    observe_stable_at_minimum(&mut feedback);

    // 遅れ 100 ms (不感帯を引いた 80 ms) では 50 + 40 = 90 ms まで上がる
    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 6,
        Some(100_000),
        0,
        0,
    ));
    let after_increase_us = feedback.feedback_target_us();
    assert_eq!(
        after_increase_us,
        AUDIO_DELAY_FEEDBACK_MIN_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "遅れの分だけ増やす"
    );

    // 間隔の途中では動かさない
    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 6 + AUDIO_DELAY_FEEDBACK_INTERVAL_US / 2,
        Some(100_000),
        0,
        0,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        after_increase_us,
        "間隔の途中では動かさない"
    );
    assert_eq!(
        feedback.snapshot(0).reason,
        AudioDelayFeedbackReason::Waiting,
        "動かしていないことが分かる"
    );

    // 間隔が空いたらまた動かす (50 → 90 → 130 だが、適用する上限の 100 ms で切られる)
    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 7,
        Some(100_000),
        0,
        0,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "間隔が空いたら動かす"
    );
}

#[test]
fn backlog_raises_the_target_by_the_clamped_step() {
    // 並べすぎで捨てたなら、捨てた長さぶんを吸収できるだけ増やす
    // (捨てた長さ + 余白、1 回の上限まで)
    let mut feedback = AudioDelayFeedback::new();
    widen_ceiling(&mut feedback);

    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 3,
        Some(0),
        5,
        100_000,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MIN_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "捨てた長さと余白の合計を上限で切って増やす"
    );
    assert_eq!(
        feedback.snapshot(0).reason,
        AudioDelayFeedbackReason::Backlog,
        "理由が並べすぎである"
    );

    // 捨てが続いていない (累積が増えていない) なら、遅れの判断へ移る
    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 4,
        Some(0),
        5,
        100_000,
    ));
    assert_eq!(
        feedback.snapshot(0).reason,
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
    // 目標は下限 (50 ms) から、実際に適用する上限の間に収める。下限より下げると揺らぎを
    // 吸収できず、上限より上げると常に大きく遅れて鳴る。適用する上限は既定で 100 ms で
    // あり、絶対の上限 (300 ms) まで上げるのは圧縮か穴が続くときだけである
    assert_eq!(
        AUDIO_DELAY_FEEDBACK_MIN_US, 50_000,
        "目標遅延の下限は 50 ms であること"
    );
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

    // 下限ちょうどの目標は切らない。さらに下げる判断を重ねても下限のままである
    feedback.update(observation(1_000_000_000, Some(0), 0, 0));
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MIN_US,
        "下限ちょうどの目標は切らないこと"
    );

    // 十分な回数を増やしても、適用する上限 (圧縮が無いときは 100 ms) を超えない。絶対の
    // 上限 (300 ms) は適用されない値であり、そこまで育てると遅れが収まってから減らし切る
    // までの時間だけ復帰が遅れる
    for index in 0..100 {
        feedback.update(observation(
            2_000_000_000 + index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(400_000),
            0,
            0,
        ));
    }
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "適用する上限 (100 ms) で止まる"
    );
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "適用する上限は 100 ms のままであること"
    );
    assert!(
        feedback.feedback_target_us() < AUDIO_DELAY_FEEDBACK_MAX_US,
        "絶対の上限 (300 ms) までは育てないこと"
    );
}

#[test]
fn explicit_ceiling_caps_the_target() {
    // 明示設定 (`targetLatency`) は、自動で決める目標の上限として尊重する。
    // 値が変わった時点で、既に超えていればその場で収める
    let mut feedback = AudioDelayFeedback::new();
    observe_stable_at_minimum(&mut feedback);

    // 遅れで適用する上限 (100 ms) まで上げる。初期値が下限と同じため、1 回で 90 ms、
    // 2 回目で上限に達する
    for index in 0..10 {
        feedback.update(observation(
            1_000_000_000 + index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(400_000),
            0,
            0,
        ));
    }
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "適用する上限 (100 ms) まで戻る"
    );

    // 明示設定が適用する上限 (100 ms) より大きいときは、適用する上限を変えない (広げた分に
    // だけ掛かる)。閉ループの内部の目標は 100 ms のままである
    feedback.set_ceiling_us(Some(120_000));
    assert_eq!(feedback.feedback_target_us(), 100_000, "上限に収める");
    assert_eq!(feedback.ceiling_us(), Some(120_000), "上限を保持する");

    // 上限より大きい値へは増えない。前回の更新から間隔が空いているため、毎回判断は行う
    for index in 0..10 {
        feedback.update(observation(
            2_000_000_000 + index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(400_000),
            0,
            0,
        ));
    }
    assert_eq!(
        feedback.feedback_target_us(),
        100_000,
        "適用する上限を超えない"
    );
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        100_000,
        "広げていないときは既定の上限のままである"
    );

    // 明示設定が適用する上限 (100 ms) より小さいときは、そちらが効く
    feedback.set_ceiling_us(Some(90_000));
    assert_eq!(feedback.feedback_target_us(), 90_000, "明示設定まで下がる");

    // 上限を外すと、適用する上限 (100 ms) まで戻る
    feedback.set_ceiling_us(None);
    assert_eq!(
        feedback.feedback_target_us(),
        90_000,
        "外した時点では目標を動かさない"
    );
    feedback.update(observation(3_000_000_000, Some(400_000), 0, 0));
    assert_eq!(
        feedback.feedback_target_us(),
        100_000,
        "上限が無ければ適用する上限まで増える"
    );
    assert_eq!(feedback.ceiling_us(), None, "上限を外した状態になる");
}

#[test]
fn explicit_ceiling_below_the_floor_wins_over_the_floor() {
    // 明示の上限が下限を下回るときは上限を優先する (利用者の指定より自動で大きくしない)。
    // このとき実際に使う目標は、明示の値をそのまま使う
    let mut feedback = AudioDelayFeedback::new();
    feedback.update(observation(0, Some(400_000), 0, 0));
    feedback.set_ceiling_us(Some(AUDIO_DELAY_FEEDBACK_MIN_US - 20_000));
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MIN_US - 20_000,
        "上限まで下がる"
    );
    assert_eq!(
        feedback.target_delay_us(0),
        AUDIO_DELAY_FEEDBACK_MIN_US - 20_000,
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
    // 揺らぎだけから求めた目標 (学習値) の方が大きいときは、それを使う。明示設定の上限は
    // 自動で決める分にだけ掛け、学習値には掛けない (既存の揺らぎの吸収を変えないため)
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
        feedback.target_delay_us(95_000),
        95_000,
        "学習値の方が大きければそれを使う (実際に使う値の上限の内側)"
    );
    assert_eq!(
        feedback.target_delay_us(260_000),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "学習値が実際に使う値の上限を超えたら切る"
    );

    let snapshot = feedback.snapshot(95_000);
    assert_eq!(snapshot.target_us, 90_000, "閉ループが決めた目標を出す");
    assert_eq!(snapshot.ceiling_us, Some(90_000), "明示された上限を出す");
}

/// 実際に使う目標遅延は適用する上限 (既定 100 ms) で切る
///
/// 到着が大きく跳ねた分まで目標にすると、その分だけ常に遅れて鳴る。100 ms は到着基準で
/// 並べるときの遅れと同じ値であり、収まらない分は受信側の追いつきが吸収する。閉ループの
/// 内部の目標も同じ上限で切る。適用されない値へ育てると、遅れが収まってから減らし切る
/// までの時間だけ復帰が遅れる。
#[test]
fn the_applied_target_is_capped() {
    let mut feedback = AudioDelayFeedback::new();

    // 観測が無い間も、学習値が上限を超えたら切る
    assert_eq!(
        feedback.target_delay_us(AUDIO_DELAY_FEEDBACK_MAX_TARGET_US + 1),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "学習値が上限を超えたら切る"
    );
    assert_eq!(
        feedback.target_delay_us(AUDIO_DELAY_FEEDBACK_MAX_TARGET_US),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "ちょうど上限は切らない"
    );

    // 観測後も同じ上限で切る。初期値は下限 (50 ms) なので、遅れで 100 ms まで上げる
    for index in 0..2 {
        feedback.update(observation(
            index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(300_000),
            0,
            0,
        ));
    }
    assert_eq!(
        feedback.target_delay_us(300_000),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "閉ループの目標と学習値の大きい方を上限で切る"
    );
    // 閉ループの内部の目標も適用する上限で切る。圧縮が無いため広げていない
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "内部の目標も適用する上限で止まる"
    );
    let snapshot = feedback.snapshot(300_000);
    assert_eq!(
        snapshot.target_us, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "計器に出す内部の目標も適用する上限で止まる"
    );
    assert_eq!(
        snapshot.applied_ceiling_us, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "適用する上限は既定の 100 ms のままである"
    );
    assert_eq!(
        feedback.target_delay_us(300_000),
        snapshot.applied_ceiling_us,
        "実際に使う値は適用する上限を超えない"
    );
}

/// 適応の上限の定数を固定する
///
/// moqt-js の実測 (実リレー) で決めた値であり、変えるときは遅延と圧縮の釣り合いを取り直す
/// 必要がある。
#[test]
fn the_adaptive_ceiling_constants_are_fixed() {
    assert_eq!(
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US, 160_000,
        "圧縮か穴が続くときにだけ使う上限は 160 ms であること"
    );
    assert_eq!(
        AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND, 5_000,
        "上限を上げる根拠にする圧縮の速さは 5 ms/秒 であること"
    );
    assert_eq!(
        AUDIO_DELAY_FEEDBACK_COMPRESSION_CONFIRM_US, 2_000_000,
        "上限を上げるまでに圧縮か穴が続くのを待つ時間は 2 秒であること"
    );
    assert_eq!(
        AUDIO_DELAY_FEEDBACK_ARTIFACT_RELEASE_US, 15_000_000,
        "上限を戻すまでに静かな状態が続くのを待つ時間は 15 秒であること"
    );
    assert_eq!(
        AUDIO_DELAY_FEEDBACK_GAP_QUIET_US, 5_000_000,
        "穴が空いた後に目標を下げない時間は 5 秒であること"
    );
    // 閉ループは安全下限から始める (最初の観測だけで固定の 100 ms を適用しない)
    assert_eq!(
        AUDIO_DELAY_FEEDBACK_START_US, AUDIO_DELAY_FEEDBACK_MIN_US,
        "最初の目標遅延は下限と同じであること"
    );
    // 定数どうしの関係は、実行時ではなくコンパイル時に固定する (実行時に確かめても同じ値に
    // なるため、ビルドが通ること自体を検査にする)
    const _: () = assert!(
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US > AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "広げる上限は既定の上限 (100 ms) より広いこと"
    );
    const _: () = assert!(
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US < AUDIO_DELAY_FEEDBACK_MAX_US,
        "広げる上限は絶対の上限 (300 ms) より小さいこと"
    );
    const _: () = assert!(
        AUDIO_DELAY_FEEDBACK_ARTIFACT_RELEASE_US > AUDIO_DELAY_FEEDBACK_COMPRESSION_CONFIRM_US,
        "戻すまでに待つ時間は、広げるまでに待つ時間より長いこと (往復を避ける)"
    );
    const _: () = assert!(
        AUDIO_DELAY_FEEDBACK_GAP_QUIET_US > AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        "穴の後のホールドは制御の間隔より長いこと (数回ぶん構える)"
    );
    const _: () = assert!(
        AUDIO_DELAY_FEEDBACK_START_US == AUDIO_DELAY_FEEDBACK_MIN_US,
        "最初の目標遅延は下限と同じであること"
    );
}

/// 圧縮が続くときだけ、適用する上限を 160 ms へ広げる
///
/// 上げる条件は「圧縮の速さが 5 ms/秒 を超える状態が 2 秒続くこと」である。閾値ちょうどと、
/// 一時的な圧縮では上げない。上げた後は内部の目標も 100 ms を超えて上がり得る。
#[test]
fn the_ceiling_widens_only_after_sustained_compression() {
    let mut feedback = AudioDelayFeedback::new();

    // 最初の制御。前回が無いため速さは求まらない (0 とみなす)
    feedback.update(observation_with_compression(
        0,
        Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
        0,
        0,
    ));
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "圧縮が無ければ広げない"
    );
    assert_eq!(
        feedback.snapshot(0).compression_us_per_second,
        0,
        "前回が無ければ速さは 0 である"
    );

    // 圧縮の速さが閾値ちょうど (5 ms/秒) の 1 秒。超えていないため根拠にしない
    feedback.update(observation_with_compression(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
        AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND,
        0,
    ));
    assert_eq!(
        feedback.snapshot(0).compression_us_per_second,
        AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND,
        "閾値ちょうどの速さを計器に出す"
    );
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "閾値ちょうどでは広げない"
    );

    // 閾値を超える速さが 1 秒続いた。まだ 2 秒に足りない
    feedback.update(observation_with_compression(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 2,
        Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
        AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND + 1_000,
        0,
    ));
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "1 秒では広げない"
    );

    // 2 秒続いた。ここで広げる
    feedback.update(observation_with_compression(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 3,
        Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
        AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND + 1_000,
        0,
    ));
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "2 秒続いたら 160 ms へ広げる"
    );

    // 広げている間は、内部の目標も 100 ms を超えて上がり得る。遅れの観測で 160 ms まで
    // 上げる (50 → 90 → 130 → 160)
    for index in 0..3 {
        feedback.update(observation_with_compression(
            AUDIO_DELAY_FEEDBACK_INTERVAL_US * (4 + index),
            Some(60_000),
            AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND + 1_000,
            0,
        ));
    }
    let raised_us = feedback.feedback_target_us();
    assert_eq!(
        raised_us, AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "広げた分まで内部の目標を上げる"
    );
    assert!(
        raised_us > AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "広げなければ適用されない値まで上がる: {raised_us}"
    );
    assert_eq!(
        feedback.target_delay_us(0),
        raised_us,
        "広げた上限まで実際に使う"
    );
    assert!(
        feedback.target_delay_us(0) > AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "適用する値が既定の上限を超える"
    );
}

/// 圧縮の増分は、制御までの間ぶんを足し合わせて速さにする
///
/// 観測は音ごとに来るが、目標を動かすのは毎秒 1 回である。1 回の観測の増分をそのまま
/// 速さとして見ると、1 秒間に出た圧縮を取りこぼす。負の増分は 0 とみなす。
#[test]
fn compression_is_accumulated_until_the_next_control() {
    let mut feedback = AudioDelayFeedback::new();
    feedback.update(observation_with_compression(0, Some(0), 0, 0));

    // 制御の間隔 (1 秒) より短い間隔で 4 件届く。合計 10 ms の圧縮である
    feedback.update(observation_with_compression(200_000, Some(0), 3_000, 0));
    feedback.update(observation_with_compression(400_000, Some(0), 3_000, 0));
    feedback.update(observation_with_compression(600_000, Some(0), 2_000, 0));
    feedback.update(observation_with_compression(800_000, Some(0), 2_000, 0));
    // 間隔が空いていないため、この時点では制御しない
    assert_eq!(
        feedback.snapshot(0).compression_us_per_second,
        0,
        "制御していない間は速さを更新しない"
    );

    // 1 秒に達したため制御する。蓄えた 10 ms を 1 秒で割って 10 ms/秒 になる
    feedback.update(observation_with_compression(1_000_000, Some(0), 0, 0));
    assert_eq!(
        feedback.snapshot(0).compression_us_per_second,
        10_000,
        "制御までの間の増分を足し合わせて速さにする"
    );
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "1 秒ぶんでは広げない"
    );

    // 負の増分 (購読のやり直しで累積が戻った場合) は 0 とみなす
    feedback.update(observation_with_compression(2_000_000, Some(0), -50_000, 0));
    assert_eq!(
        feedback.snapshot(0).compression_us_per_second,
        0,
        "負の増分は圧縮として数えない"
    );
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "負の増分では広げない"
    );
}

/// 上限は、圧縮が 0 で経路が 100 ms 以内の状態が 15 秒続いたときだけ戻す
///
/// 圧縮が 0 になっただけで戻すと、揺らぎが 100 ms を超えている経路では、戻した直後に
/// 圧縮が再発して 2 秒でまた上げる往復になる。揺らぎだけの目標が 100 ms を超えている間は
/// 戻さない。
#[test]
fn the_ceiling_returns_only_when_the_path_is_quiet() {
    let mut feedback = AudioDelayFeedback::new();
    // 圧縮が続いて上限を広げ、遅れで内部の目標も上げておく
    for index in 0..4 {
        feedback.update(observation_with_compression(
            index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(60_000),
            AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND + 1_000,
            0,
        ));
    }
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "まず広げておく"
    );
    let widened_target_us = feedback.feedback_target_us();
    assert!(
        widened_target_us > AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "内部の目標も上がっている: {widened_target_us}"
    );

    // 圧縮は 0 だが、揺らぎだけの目標が 100 ms を超えている。15 秒では戻さない
    let mut now_us = AUDIO_DELAY_FEEDBACK_INTERVAL_US * 4;
    for _ in 0..AUDIO_DELAY_FEEDBACK_ARTIFACT_RELEASE_US / AUDIO_DELAY_FEEDBACK_INTERVAL_US {
        now_us += AUDIO_DELAY_FEEDBACK_INTERVAL_US;
        feedback.update(observation_with_compression(
            now_us,
            Some(60_000),
            0,
            150_000,
        ));
    }
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "経路が 100 ms を超えている間は戻さない"
    );

    // 経路が静かになった。15 秒に 1 秒足りない間は戻さない
    for _ in 0..AUDIO_DELAY_FEEDBACK_ARTIFACT_RELEASE_US / AUDIO_DELAY_FEEDBACK_INTERVAL_US - 1 {
        now_us += AUDIO_DELAY_FEEDBACK_INTERVAL_US;
        feedback.update(observation_with_compression(
            now_us,
            Some(60_000),
            0,
            90_000,
        ));
    }
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "15 秒に足りない間は戻さない"
    );
    assert_eq!(
        feedback.snapshot(0).target_us,
        widened_target_us,
        "戻すまでは内部の目標も下げない"
    );

    // 15 秒続いた。100 ms へ戻す。内部の目標も新しい上限で切られる
    now_us += AUDIO_DELAY_FEEDBACK_INTERVAL_US;
    feedback.update(observation_with_compression(
        now_us,
        Some(60_000),
        0,
        90_000,
    ));
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "15 秒続いたら 100 ms へ戻す"
    );
    assert_eq!(
        feedback.snapshot(0).target_us,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "戻した上限で内部の目標も切る"
    );
    assert_eq!(
        feedback.target_delay_us(0),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "実際に使う値も 100 ms 以下になる"
    );

    // 圧縮が出ているが閾値未満の秒は、戻す条件の連続時間を数え直す
    feedback.update(observation_with_compression(
        now_us + AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(60_000),
        AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND,
        0,
    ));
    assert_eq!(
        feedback.snapshot(0).compression_us_per_second,
        AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND,
        "閾値未満の圧縮も計器に出す"
    );
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "戻した後は閾値未満の圧縮では広げない"
    );
}

/// 明示設定は、広げる分にも掛かる
///
/// 利用者が宣言した遅延の予算を、自動の判断で超えない。ただし既定の上限 (100 ms) は
/// 下回らない (揺らぎだけの目標に明示設定を掛けない既存の規則を変えないため)。
#[test]
fn explicit_ceiling_caps_the_widened_ceiling() {
    let mut feedback = AudioDelayFeedback::new();
    feedback.set_ceiling_us(Some(120_000));
    for index in 0..4 {
        feedback.update(observation_with_compression(
            index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(60_000),
            AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND + 1_000,
            0,
        ));
    }
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        120_000,
        "広げても明示設定を超えない"
    );
    // 遅れが続いても、明示設定までしか上がらない
    for index in 4..10 {
        feedback.update(observation_with_compression(
            index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(400_000),
            AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND + 1_000,
            0,
        ));
    }
    assert_eq!(
        feedback.snapshot(0).target_us,
        120_000,
        "内部の目標も明示設定で止まる"
    );
    assert_eq!(
        feedback.target_delay_us(0),
        120_000,
        "実際に使う値も明示設定で止まる"
    );

    // 明示設定が既定の上限 (100 ms) を下回るときは、既定の上限を下回らない。内部の目標は
    // 明示設定で切る (既存の規則)
    feedback.set_ceiling_us(Some(90_000));
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "広げる分は既定の上限を下回らない"
    );
    assert_eq!(
        feedback.snapshot(0).target_us,
        90_000,
        "内部の目標は明示設定で切る"
    );

    // 明示設定を外すと、広げた上限 (160 ms) まで使える
    feedback.set_ceiling_us(None);
    feedback.update(observation_with_compression(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 10,
        Some(400_000),
        AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND + 1_000,
        0,
    ));
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "明示設定が無ければ 160 ms まで使える"
    );
    assert_eq!(
        feedback.snapshot(0).compression_us_per_second,
        AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND + 1_000,
        "計器に圧縮の速さを出す"
    );
}

#[test]
fn a_decreasing_backlog_does_not_raise_the_target_and_missing_lateness_waits() {
    // 購読のやり直しで累積の捨てが 0 に戻っても、負の差で増やさない。
    // 遅れの観測が無い (まだ鳴らしていない) ときは目標を動かさない
    let mut feedback = AudioDelayFeedback::new();
    feedback.update(observation(0, Some(0), 5, 100_000));
    assert_eq!(
        feedback.snapshot(0).reason,
        AudioDelayFeedbackReason::Backlog,
        "捨てで増える"
    );

    // 購読のやり直しで累積が 0 に戻る
    feedback.update(observation(AUDIO_DELAY_FEEDBACK_INTERVAL_US, Some(0), 0, 0));
    assert_eq!(
        feedback.snapshot(0).reason,
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
        feedback.snapshot(0).reason,
        AudioDelayFeedbackReason::Waiting,
        "理由が待ちである"
    );
}

#[test]
fn the_first_settled_observation_does_not_decrease_the_target() {
    // 減らす速さは前回の観測からの時間で決めるため、前回が無いときは減らさない
    // (1 回の観測だけで「余裕が続いている」とは言えない)。初期値は下限と同じであるため、
    // 値では「減らさなかった」ことを観測できない。理由と下限に留まることで確かめる
    let mut feedback = AudioDelayFeedback::new();
    feedback.update(observation(
        0,
        Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
        0,
        0,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MIN_US,
        "前回が無ければ減らさない (下限に留まる)"
    );
    assert_eq!(
        feedback.snapshot(0).reason,
        AudioDelayFeedbackReason::Waiting,
        "理由が待ちである"
    );
    assert_eq!(feedback.snapshot(0).adjustments, 0, "動かしていない");

    // 前回があるときは減らす判断へ進む。下限に当たるため値は変わらない
    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
        0,
        0,
    ));
    assert_eq!(
        feedback.snapshot(0).reason,
        AudioDelayFeedbackReason::Settled,
        "前回があれば減らす判断へ進む"
    );
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MIN_US,
        "下限を下回らない"
    );
    assert_eq!(
        feedback.snapshot(0).last_change_us,
        0,
        "下限に当たるため動かしていない"
    );
}

#[test]
fn snapshot_reports_the_latest_observation() {
    // 計器として、直近の観測の分布と動かした回数が読めること
    let mut feedback = AudioDelayFeedback::new();
    // 圧縮が続いて適用する上限を広げ、内部の目標が動ける余地を作る
    widen_ceiling(&mut feedback);
    let adjustments_before_us = feedback.snapshot(0).adjustments;

    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 3,
        Some(60_000),
        0,
        0,
    ));
    let snapshot = feedback.snapshot(0);
    assert_eq!(
        snapshot.lateness_p50_us,
        Some(40_000),
        "遅れの p50 を出す (時間圧縮の不感帯の 20 ms を引いた値)"
    );
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
    assert_eq!(
        snapshot.adjustments,
        adjustments_before_us + 1,
        "動かした回数を数える"
    );
    assert_eq!(snapshot.ceiling_us, None, "明示設定の上限は無い");
    assert_eq!(
        snapshot.applied_ceiling_us, AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "適用する上限 (広げている) を出す"
    );
    assert_eq!(
        snapshot.compression_us_per_second, 0,
        "この観測では圧縮を観測していない"
    );
    assert_eq!(
        feedback.snapshot(0).target_us,
        AUDIO_DELAY_FEEDBACK_MIN_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US
    );
    // 実際に使う値は、揺らぎだけの目標との大きい方を適用の上限で切った値である
    assert_eq!(
        snapshot.applied_us,
        AUDIO_DELAY_FEEDBACK_MIN_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "実際に使う値を出す"
    );
    assert_eq!(
        feedback.snapshot(200_000).applied_us,
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "揺らぎだけの目標が大きければ適用の上限で切る"
    );
}

/// 揺らぎだけの目標と、実際に使う値を計器に出す
///
/// moqt-js の `snapshot(jitterTargetMs)` と同じく、引数で渡した揺らぎだけの目標をそのまま
/// `jitter_target_us` に出し、実際に使う値を `applied_us` に出す。`delay_breakdown` と
/// example のログはこの 2 つを読む。
#[test]
fn snapshot_reports_the_jitter_target_and_the_applied_value() {
    let mut feedback = AudioDelayFeedback::new();

    // 観測が無い間は、揺らぎだけの目標を上限 (100 ms) で切った値が実際に使う値になる
    let snapshot = feedback.snapshot(30_000);
    assert_eq!(snapshot.jitter_target_us, 30_000, "引数の値をそのまま出す");
    assert_eq!(snapshot.target_us, AUDIO_DELAY_FEEDBACK_MIN_US);
    assert_eq!(
        snapshot.applied_us, 30_000,
        "観測が無ければ揺らぎだけの目標を使う"
    );
    assert_eq!(
        feedback.snapshot(200_000).applied_us,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "観測が無くても上限で切る"
    );

    // 観測を受けた後は、閉ループの目標との大きい方を使う
    feedback.update(observation(
        0,
        Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
        0,
        0,
    ));
    let snapshot = feedback.snapshot(30_000);
    assert_eq!(
        snapshot.jitter_target_us, 30_000,
        "揺らぎだけの小さい目標はそのまま出す"
    );
    assert_eq!(
        snapshot.applied_us, AUDIO_DELAY_FEEDBACK_MIN_US,
        "閉ループの目標 (下限) の方が大きい"
    );
    assert_eq!(
        feedback.snapshot(90_000).applied_us,
        90_000,
        "揺らぎだけの目標の方が大きければそれを使う"
    );
}

#[test]
fn the_stretch_deadband_is_not_counted_as_lateness() {
    // 不感帯 (20 ms) の中の遅れは、playout が時間圧縮せずに意図して残した分である。これを
    // そのまま学習へ渡すと、playout は詰めないのに目標だけが上がり続けて遅延が伸びる。
    // 裾 (p95 と max) は引かない (「たまに大きく遅れた」事実を消さないため)
    let mut feedback = AudioDelayFeedback::new();

    // 不感帯ちょうどの遅れ (20 ms) では目標を動かす根拠にしない
    feedback.update(observation(0, Some(AUDIO_PLAYOUT_STRETCH_MIN_US), 0, 0));
    assert_eq!(
        feedback.snapshot(0).reason,
        AudioDelayFeedbackReason::Waiting,
        "不感帯の中の遅れでは目標を動かさないこと"
    );
    assert_eq!(
        feedback.snapshot(0).lateness_p50_us,
        Some(0),
        "不感帯の分を引いた遅れを残すこと"
    );
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_START_US,
        "目標は初期値のままであること"
    );

    // 不感帯を超えた遅れ (40 ms) では、超えた分 (20 ms) を遅れとして扱う
    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(40_000),
        0,
        0,
    ));
    assert_eq!(
        feedback.snapshot(0).reason,
        AudioDelayFeedbackReason::Lateness,
        "不感帯を超えた分は遅れとして扱うこと"
    );
    assert_eq!(
        feedback.snapshot(0).lateness_p50_us,
        Some(20_000),
        "不感帯を超えた分だけを遅れとして残すこと"
    );
}

#[test]
fn underrun_alone_raises_the_target() {
    // 枯れは事後には埋められないため、遅れが許容の中に収まっていても目標を上げる
    let mut feedback = AudioDelayFeedback::new();
    widen_ceiling(&mut feedback);

    let before_adjustments = feedback.snapshot(0).adjustments;
    // 100 ms 枯れた。1 回で増やす上限 (40 ms) まで増える
    feedback.update(observation_with_underrun(
        3 * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        100,
    ));
    let snapshot = feedback.snapshot(0);
    assert_eq!(
        snapshot.reason,
        AudioDelayFeedbackReason::Underrun,
        "枯れが理由になること"
    );
    assert_eq!(
        snapshot.last_change_us, AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "枯れた長さは 1 回で増やす上限までにすること"
    );
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MIN_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "枯れた長さぶん目標を上げること"
    );
    assert_eq!(
        snapshot.adjustments,
        before_adjustments + 1,
        "動かした回数を数えること"
    );

    // さらに枯れた。90 ms から 130 ms へ上がる
    feedback.update(observation_with_underrun(
        4 * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        100,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_MIN_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US * 2,
        "枯れが続けばさらに上げること"
    );
    // 適用する上限 (160 ms) で止まる
    feedback.update(observation_with_underrun(
        5 * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        100,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "上限を超えないこと"
    );
    // 上限に張り付いた後に枯れても、それ以上は上がらない
    feedback.update(observation_with_underrun(
        6 * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        100,
    ));
    let snapshot = feedback.snapshot(0);
    assert_eq!(
        feedback.feedback_target_us(),
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "上限を超えないこと"
    );
    assert_eq!(snapshot.last_change_us, 0, "上限に張り付いたら動かないこと");
}

#[test]
fn underrun_does_not_raise_the_target_beyond_the_explicit_ceiling() {
    // 明示された `targetLatency` は、枯れによる増加にも掛かる
    let mut feedback = AudioDelayFeedback::new();
    widen_ceiling(&mut feedback);
    feedback.set_ceiling_us(Some(120_000));
    // 1 回では 90 ms までしか上がらないため、120 ms に到達させるには 2 回渡す
    for index in 0..2 {
        feedback.update(observation_with_underrun(
            (3 + index) * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            100,
        ));
    }
    assert_eq!(
        feedback.feedback_target_us(),
        120_000,
        "明示の上限で止まること"
    );
    assert_eq!(
        feedback.snapshot(0).reason,
        AudioDelayFeedbackReason::Underrun,
        "枯れが理由になること"
    );
}

#[test]
fn underrun_uses_the_larger_of_it_and_the_existing_increase() {
    // 既存の増加 (鳴り遅れ) の方が大きければ、その量を使う。理由は枯れを優先する
    let mut feedback = AudioDelayFeedback::new();
    widen_ceiling(&mut feedback);
    let mut observation = observation_with_underrun(3 * AUDIO_DELAY_FEEDBACK_INTERVAL_US, 10);
    // 不感帯 (20 ms) を引いた後に許容 (10 ms) を超える遅れにする
    observation.lateness_us = Some(summary(AUDIO_PLAYOUT_STRETCH_MIN_US + 80_000));
    feedback.update(observation);
    let snapshot = feedback.snapshot(0);
    assert_eq!(
        snapshot.reason,
        AudioDelayFeedbackReason::Underrun,
        "理由は枯れを優先すること"
    );
    assert_eq!(
        snapshot.last_change_us, AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "遅れによる増加の方が大きいこと"
    );

    // 枯れの方が大きければ、枯れの量を使う
    let mut feedback = AudioDelayFeedback::new();
    widen_ceiling(&mut feedback);
    let mut observation = observation_with_underrun(3 * AUDIO_DELAY_FEEDBACK_INTERVAL_US, 100);
    // 並べすぎによる増加 (30 ms) を混ぜる
    observation.backlog_misses = 1;
    observation.backlog_us = 10_000;
    feedback.update(observation);
    let snapshot = feedback.snapshot(0);
    assert_eq!(snapshot.reason, AudioDelayFeedbackReason::Underrun);
    assert_eq!(
        snapshot.last_change_us, AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "枯れによる増加 (40 ms) を選ぶこと"
    );
}

#[test]
fn no_underrun_keeps_the_existing_behavior() {
    // 枯れが無ければ、従来の増減の規則とまったく同じになる
    let mut with_underrun = AudioDelayFeedback::new();
    let mut without_underrun = AudioDelayFeedback::new();
    let observations = [
        observation(0, Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US), 0, 0),
        observation(
            AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
            0,
            0,
        ),
        observation(
            2 * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(AUDIO_PLAYOUT_STRETCH_MIN_US + 60_000),
            0,
            0,
        ),
        observation(
            3 * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(AUDIO_DELAY_FEEDBACK_TOLERANCE_US),
            1,
            10_000,
        ),
    ];
    for observation in observations {
        // どちらも枯れ無し (underrun_ms は 0) の同じ観測を渡す
        with_underrun.update(observation);
        without_underrun.update(observation);
    }
    assert_eq!(
        with_underrun.snapshot(0),
        without_underrun.snapshot(0),
        "枯れが無ければ従来と同じ結果になること"
    );
}

// ここから moqt-js で追加された、穴への対処と再開の規則のテスト

/// 穴 1 個で、その穴を吸収できる上限まで一時的に広げて目標を増やす
///
/// 穴は事後には埋められない。1 個でも「経路の揺らぎが目標を超えた」証拠であるため、圧縮が
/// 2 秒続くのを待たずに適用の上限を「実際に使っていた深さ + 穴の長さ + 余白」まで広げる。
#[test]
fn gap_raises_the_target_and_widens_the_applied_ceiling() {
    let mut feedback = AudioDelayFeedback::new();
    // 最初の観測 (遅れは許容の中) は間隔が無いため動かさない
    feedback.update(observation(0, Some(0), 0, 0));
    assert_eq!(
        feedback.snapshot(0).reason,
        AudioDelayFeedbackReason::Waiting,
        "最初の観測では動かさないこと"
    );

    // 1 秒後に穴 (20 ms) が空いた。揺らぎだけの目標は 100 ms であり、実際に使っていた
    // 深さは 100 ms である。穴の長さ + 余白 (20 + 20 ms) を吸収できる 140 ms まで上限を
    // 一時的に広げ、その上限まで目標を増やす
    feedback.update(observation_with_gap(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(0),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        1,
        20_000,
    ));
    let first = feedback.snapshot(AUDIO_DELAY_FEEDBACK_MAX_TARGET_US);
    assert_eq!(
        first.reason,
        AudioDelayFeedbackReason::Gap,
        "理由が穴であること"
    );
    assert_eq!(
        first.applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US + 40_000,
        "穴の長さ + 余白まで上限を広げること"
    );
    assert_eq!(
        first.target_us,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US + 40_000,
        "広げた上限まで増やすこと"
    );
    assert_eq!(
        first.applied_us - AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        40_000,
        "実際に使っていた 100 ms から 40 ms 増やすこと"
    );
    assert_eq!(first.adjustments, 1);

    // 穴が続くと、穴の長さ + 余白のぶんさらに増える (1 回の増加は上限 40 ms まで)。
    // 上限は穴を吸収できる 160 ms まで広がり、目標は +20 ms だけ動く (140 → 160)
    feedback.update(observation_with_gap(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 2,
        Some(0),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        2,
        60_000,
    ));
    let expanded = feedback.snapshot(AUDIO_DELAY_FEEDBACK_MAX_TARGET_US);
    assert_eq!(expanded.reason, AudioDelayFeedbackReason::Gap);
    assert_eq!(
        expanded.applied_ceiling_us, AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "穴が続くと artifact ceiling まで広がること"
    );
    assert_eq!(
        expanded.target_us, AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "広げた上限まで増やすこと"
    );
    assert_eq!(
        expanded.last_change_us, 20_000,
        "上限までの残りだけ動くこと"
    );

    // 上限に達した後は増えない
    feedback.update(observation_with_gap(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 3,
        Some(0),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        3,
        80_000,
    ));
    let capped = feedback.snapshot(AUDIO_DELAY_FEEDBACK_MAX_TARGET_US);
    assert_eq!(capped.reason, AudioDelayFeedbackReason::Gap);
    assert_eq!(
        capped.target_us, AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "160 ms を超えないこと"
    );
    assert_eq!(capped.last_change_us, 0, "上限に達したら動かさないこと");
}

/// 穴 1 個で広げた上限は、最後の穴から 5 秒の間だけ保持し、その後に戻す
///
/// 戻した直後に目標が下がり、次に同じ揺らぎが来ても穴にならないよう、その 5 秒の間は
/// 遅れが許容内でも下げない。
#[test]
fn one_gap_ceiling_returns_after_five_seconds() {
    let mut feedback = AudioDelayFeedback::new();
    feedback.update(observation(0, Some(0), 0, 0));
    feedback.update(observation_with_gap(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(0),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        1,
        20_000,
    ));
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US + 40_000,
        "穴 1 個で上限が広がること"
    );

    // 最後の穴 (1 秒) から 5 秒が経つまでは、遅れが許容内でも上限を戻さない
    for index in 2..=5 {
        feedback.update(observation(
            index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(0),
            0,
            0,
        ));
        let snapshot = feedback.snapshot(0);
        assert_eq!(
            snapshot.applied_ceiling_us,
            AUDIO_DELAY_FEEDBACK_MAX_TARGET_US + 40_000,
            "5 秒間は上限を保持すること"
        );
        assert_eq!(
            snapshot.reason,
            AudioDelayFeedbackReason::Waiting,
            "5 秒間は下げないこと"
        );
    }

    // 5,000 ms 経過 (6,000 ms の観測) で上限を戻し、目標も 100 ms へ戻る
    feedback.update(observation(
        6 * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(0),
        0,
        0,
    ));
    let snapshot = feedback.snapshot(0);
    assert_eq!(
        snapshot.applied_ceiling_us, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "5 秒後に上限を戻すこと"
    );
    assert_eq!(
        snapshot.target_us, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "戻した上限まで目標を下げること"
    );
    assert_eq!(
        snapshot.reason,
        AudioDelayFeedbackReason::Settled,
        "5 秒後は減らす判断へ進むこと"
    );
}

/// 穴 1 個で広げる上限は、明示設定 (`targetLatency`) で切る
///
/// 明示設定は利用者が宣言した遅延の予算であり、適応で超えてはならない。
#[test]
fn gap_ceiling_is_capped_by_the_explicit_ceiling() {
    let mut feedback = AudioDelayFeedback::new();
    feedback.set_ceiling_us(Some(120_000));
    feedback.update(observation(0, Some(0), 0, 0));
    // 揺らぎだけの目標が 100 ms であり、穴の長さ + 余白を吸収しようとすると 160 ms に
    // なるが、明示設定の 120 ms で切る
    feedback.update(observation_with_gap(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(0),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        1,
        40_000,
    ));
    let snapshot = feedback.snapshot(AUDIO_DELAY_FEEDBACK_MAX_TARGET_US);
    assert_eq!(
        snapshot.applied_ceiling_us, 120_000,
        "明示設定の 120 ms で切ること"
    );
    assert_eq!(snapshot.target_us, 120_000, "明示設定の上限まで増やすこと");
}

/// 穴が続くと artifact ceiling (100 → 160 ms) を広げる
///
/// 穴 1 個の一時的な広げ (穴の長さ + 余白) と段階を分ける。1 個では穴を吸収できる上限まで
/// しか広げず、2 秒ぶんの証拠がそろってから artifact ceiling を広げる。
#[test]
fn consecutive_gaps_widen_the_artifact_ceiling() {
    let mut feedback = AudioDelayFeedback::new();
    // 1 個だけでは、その穴を吸収できる上限 (140 ms) までしか広げない
    feedback.update(observation_with_gap(
        0,
        Some(0),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        1,
        20_000,
    ));
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US + 40_000,
        "穴 1 個は穴の長さ + 余白までであること"
    );
    // 2 個目の穴では、穴の長さ + 余白 (40 + 20 ms) を吸収できる 160 ms まで広がる
    feedback.update(observation_with_gap(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(0),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        2,
        40_000,
    ));
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "2 個目で上限が 160 ms まで広がること"
    );
    // 2 秒ぶんの証拠がそろったため artifact ceiling (160 ms) まで広げる
    feedback.update(observation_with_gap(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 2,
        Some(0),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        3,
        60_000,
    ));
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "artifact ceiling まで広がること"
    );
}

/// 穴が 0 でない間は artifact ceiling を 100 ms へ戻さない
///
/// 戻す条件は「圧縮も穴も 0 で、揺らぎだけの目標が 100 ms 以下」が 15 秒続くことである。
#[test]
fn gaps_keep_the_artifact_ceiling_widened() {
    let mut feedback = AudioDelayFeedback::new();
    // 2 秒ぶんの穴で artifact ceiling を広げる
    for index in 0..3 {
        feedback.update(observation_with_gap(
            index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(0),
            AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
            index as u64 + 1,
            (index + 1) * 20_000,
        ));
    }
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "まず artifact ceiling を広げておく"
    );

    // 穴が 1 つでも出続ける間は、圧縮 0・揺らぎ 100 ms 以下でも戻さない
    let mut gap_count = 3u64;
    for index in 3..=20 {
        gap_count += 1;
        feedback.update(observation_with_gap(
            index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(0),
            AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
            gap_count,
            gap_count as i64 * 20_000,
        ));
        assert_eq!(
            feedback.snapshot(0).applied_ceiling_us,
            AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
            "穴が続く間は広げたままであること (index={index})"
        );
    }

    // 穴が止まってから 15 秒で戻す
    for index in 21..=37 {
        feedback.update(observation_with_gap(
            index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(0),
            AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
            gap_count,
            gap_count as i64 * 20_000,
        ));
    }
    assert_eq!(
        feedback.snapshot(0).applied_ceiling_us,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "穴が 15 秒止まったら戻すこと"
    );
}

/// 再開時は過去の穴を捨て、再開後の新しい穴だけに適応する
///
/// 明示された予算は同じ配信の設定なので残す。再開後の最初の観測は累積の差分の基準を
/// 合わせ直すため、過去の穴を新しい穴と数えない。
#[test]
fn reset_discards_past_gaps_and_adapts_to_new_gaps() {
    let mut feedback = AudioDelayFeedback::new();
    feedback.set_ceiling_us(Some(120_000));
    feedback.update(observation(0, Some(0), 0, 0));
    feedback.update(observation_with_gap(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(0),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        1,
        40_000,
    ));
    assert_eq!(
        feedback.feedback_target_us(),
        120_000,
        "穴で明示設定の上限まで上がること"
    );

    feedback.reset();
    // 再開後の最初の観測は、累積 (1 個 / 40 ms) を差分の基準としてだけ使う
    feedback.update(observation_with_gap(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 2,
        Some(0),
        20_000,
        1,
        40_000,
    ));
    assert_eq!(
        feedback.target_delay_us(20_000),
        AUDIO_DELAY_FEEDBACK_START_US,
        "過去の穴で増やし直さないこと"
    );
    assert_eq!(
        feedback.ceiling_us(),
        Some(120_000),
        "明示された予算は残すこと"
    );
    // 再開後の 2 個目の穴 (累積 2 個 / 60 ms) には適応する
    feedback.update(observation_with_gap(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 3,
        Some(0),
        20_000,
        2,
        60_000,
    ));
    assert_eq!(
        feedback.target_delay_us(20_000),
        AUDIO_DELAY_FEEDBACK_MIN_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "再開後の穴には適応すること"
    );
}

/// 穴への対処は、内部の目標ではなく実際に適用していた深さから増やす
///
/// 音声だけの再生では到着基準の再生に別の下限 (140 ms) がある。内部の目標 (50 ms) へ足す
/// だけでは実際の再生の深さが変わらないため、実際の深さから増やす。
#[test]
fn gap_increase_starts_from_the_actual_applied_floor() {
    let mut feedback = AudioDelayFeedback::new();
    feedback.update(observation_with_gap_and_floor(
        0,
        Some(0),
        20_000,
        0,
        0,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US + 40_000,
    ));
    feedback.update(observation_with_gap_and_floor(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(0),
        20_000,
        1,
        20_000,
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US + 40_000,
    ));
    assert_eq!(
        feedback.target_delay_us(20_000),
        AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US,
        "下限より浅い内部目標を増やすだけで終わらないこと"
    );
}

/// 穴が空いた後は、最後の穴から 5 秒が経つまで目標を下げない
///
/// 下げた直後に同じ揺らぎで穴が再発する往復を避けるためである (この間も上げる向きは許す)。
#[test]
fn a_gap_holds_the_decrease_for_five_seconds() {
    let mut feedback = AudioDelayFeedback::new();
    // 初期値は安全下限なので、まず実際の遅れで目標を上げてから減衰を確認する。
    // 不感帯 (20 ms) を引いて 60 ms の遅れになるようにする
    feedback.update(observation(0, Some(80_000), 0, 0));
    let increased_us = feedback.feedback_target_us();
    assert_eq!(
        increased_us,
        AUDIO_DELAY_FEEDBACK_MIN_US + AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
        "まず遅れで上げること"
    );

    // 許容内の遅れが続き、目標を 10 ms/秒で下げる
    feedback.update(observation(AUDIO_DELAY_FEEDBACK_INTERVAL_US, Some(0), 0, 0));
    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 2,
        Some(0),
        0,
        0,
    ));
    assert!(
        feedback.snapshot(0).target_us < increased_us,
        "許容内の遅れで目標が下がること"
    );
    let before_gap_us = feedback.snapshot(0).target_us;

    // 3 秒目に穴 (20 ms) が空いた。穴の長さ + 余白だけ増やそうとする
    feedback.update(observation_with_gap(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 3,
        Some(0),
        0,
        1,
        20_000,
    ));
    let after_gap_us = feedback.snapshot(0).target_us;
    assert_eq!(
        feedback.snapshot(0).reason,
        AudioDelayFeedbackReason::Gap,
        "穴が理由になること"
    );
    assert!(
        after_gap_us > before_gap_us,
        "穴で目標を上げること: before={before_gap_us} after={after_gap_us}"
    );

    // 穴の後 1〜4 秒 (4,000〜7,000 ms) は許容内でも下げない
    for index in 4..=7 {
        feedback.update(observation(
            index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(0),
            0,
            0,
        ));
        let snapshot = feedback.snapshot(0);
        assert_eq!(
            snapshot.reason,
            AudioDelayFeedbackReason::Waiting,
            "穴の後は待つこと"
        );
        assert_eq!(snapshot.target_us, after_gap_us, "穴の後は下げないこと");
    }

    // 最後の穴 (3,000 ms) から 5,000 ms 経った 8,000 ms で下げる
    feedback.update(observation(
        8 * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
        Some(0),
        0,
        0,
    ));
    let snapshot = feedback.snapshot(0);
    assert_eq!(
        snapshot.reason,
        AudioDelayFeedbackReason::Settled,
        "5 秒経過後に減らす判断へ進むこと"
    );
    assert!(
        snapshot.target_us < after_gap_us,
        "5 秒経過後に下げること: after_gap={after_gap_us} target={}",
        snapshot.target_us
    );
}

/// 穴の後のホールド中でも、遅れによる上昇は許す
///
/// 深めに構える方向は止めない (穴の再発を避けるために上げるのは構わない)。
#[test]
fn gap_increase_is_allowed_during_the_decrease_hold() {
    let mut feedback = AudioDelayFeedback::new();
    feedback.update(observation(0, Some(0), 0, 0));
    // 安全下限で落ち着いた状態を作る
    for index in 1..=3 {
        feedback.update(observation(
            index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(0),
            0,
            0,
        ));
    }
    let low_us = feedback.snapshot(0).target_us;
    assert!(
        low_us < AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "まず下限に落ち着く: {low_us}"
    );

    // 4 秒目に穴 (1 ms)。ホールドに入る
    feedback.update(observation_with_gap(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 4,
        Some(0),
        0,
        1,
        1_000,
    ));
    let after_gap_us = feedback.snapshot(0).target_us;
    assert_eq!(
        after_gap_us,
        AUDIO_DELAY_FEEDBACK_MIN_US + 1_000 + AUDIO_DELAY_FEEDBACK_MARGIN_US,
        "穴の長さ + 余白だけ増やすこと"
    );

    // 5 秒目 (穴の 1 秒後) に遅れが許容を超えた。ホールド中でも上げる
    feedback.update(observation(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 5,
        Some(80_000),
        0,
        0,
    ));
    let snapshot = feedback.snapshot(0);
    assert_eq!(
        snapshot.reason,
        AudioDelayFeedbackReason::Lateness,
        "ホールド中でも遅れでは上げること"
    );
    assert!(
        snapshot.target_us > after_gap_us,
        "ホールド中でも上げること: after_gap={after_gap_us} target={}",
        snapshot.target_us
    );
}

/// 穴の後のホールドは 5,000 ms である
///
/// 実測の穴の再発間隔 (数秒) より長く、制御の間隔 (1 秒) の数回ぶんである。
#[test]
fn the_gap_quiet_hold_is_five_seconds() {
    assert_eq!(
        AUDIO_DELAY_FEEDBACK_GAP_QUIET_US, 5_000_000,
        "穴の後のホールドは 5 秒であること"
    );
}

/// reset は目標と制御の状態を初期化し、明示設定の上限だけを残す
///
/// 再生を止めたとき、前の穴と制御の待ち時間を次の再生へ持ち込まない。
#[test]
fn reset_returns_to_the_initial_state_and_keeps_the_ceiling() {
    let mut feedback = AudioDelayFeedback::new();
    feedback.set_ceiling_us(Some(120_000));
    // 圧縮で上限を広げ、遅れと穴で目標を動かしておく
    for index in 0..4 {
        feedback.update(observation_with_compression(
            index * AUDIO_DELAY_FEEDBACK_INTERVAL_US,
            Some(60_000),
            AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND + 1_000,
            0,
        ));
    }
    feedback.update(observation_with_gap(
        AUDIO_DELAY_FEEDBACK_INTERVAL_US * 4,
        Some(0),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        1,
        20_000,
    ));
    assert!(
        feedback.snapshot(0).adjustments > 0,
        "まず目標を動かしておく"
    );

    feedback.reset();
    let snapshot = feedback.snapshot(0);
    assert_eq!(
        snapshot.target_us, AUDIO_DELAY_FEEDBACK_START_US,
        "目標を最初の値へ戻すこと"
    );
    assert_eq!(
        snapshot.reason,
        AudioDelayFeedbackReason::Initial,
        "理由を初期状態へ戻すこと"
    );
    assert_eq!(snapshot.adjustments, 0, "動かした回数を 0 に戻すこと");
    assert_eq!(snapshot.last_change_us, 0, "直前の増分を 0 に戻すこと");
    assert_eq!(snapshot.lateness_p50_us, None, "直近の観測を捨てること");
    assert_eq!(
        snapshot.applied_ceiling_us, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "広げた上限を戻すこと"
    );
    assert_eq!(
        snapshot.compression_us_per_second, 0,
        "圧縮の速さを 0 に戻すこと"
    );
    assert_eq!(
        snapshot.ceiling_us,
        Some(120_000),
        "明示設定の上限は残すこと"
    );
    // 観測が無い状態に戻るため、閉ループの目標は使わず、揺らぎだけの目標を上限で切る
    assert_eq!(snapshot.jitter_target_us, 0);
    assert_eq!(
        snapshot.applied_us, 0,
        "再開直後は揺らぎだけの目標を使うこと"
    );
    assert_eq!(
        feedback.target_delay_us(AUDIO_DELAY_FEEDBACK_MAX_TARGET_US),
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
        "観測が無い間は学習値を使うこと"
    );
}
