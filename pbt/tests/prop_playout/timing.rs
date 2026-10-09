//! 音声の再生の観測値のプロパティテスト
//!
//! 任意の記録列 (鳴らすと決めた音・鳴らさなかった音・再生の停止・リセット) に対して、
//! 観測値が記録の仕様どおりになることを検証する。分布は直近の窓の中の値だけから求め、
//! 窓の外の記録は分布から落とすが累積には残す。累積は減らず、直近の一覧は上限を超えず、
//! リセットで初期状態へ戻る。分岐ごとに到達を数え、検証が空振りしていないことも確かめる。

use std::cell::Cell;

use pbt::common::test_runner;
use shiguredo_moqt::playout::scheduler::{AudioPlayoutBasis, AudioPlayoutPlay};
use shiguredo_moqt::playout::timing::{
    AUDIO_DELAY_FEEDBACK_WINDOW_US, AUDIO_PLAYOUT_TIMING_WINDOW_US, AudioMissEvent,
    AudioMissReason, AudioMissTotal, AudioMissTotals, AudioPlayoutMiss, AudioPlayoutTimingSnapshot,
    AudioPlayoutTimingStats, MAX_RECENT_AUDIO_MISSES, TimingSummary,
};

/// 記録列の操作。重みは [`OP_WEIGHTS`] と対応する
const OP_PLAY: usize = 0;
const OP_MISS: usize = 1;
const OP_STOPPED: usize = 2;
const OP_RESET: usize = 3;
const OP_BURST: usize = 4;

/// 操作の重み。リセットは稀に選び、直近の一覧の上限に届くまとまった記録も選ぶ
const OP_WEIGHTS: [u32; 5] = [5, 4, 3, 1, 2];

/// まとめて記録する鳴らさなかった音の数
///
/// 1 ステップずつでは直近の一覧の上限 ([`MAX_RECENT_AUDIO_MISSES`]) に届かないため、
/// 上限を超えて古い方から捨てる分岐へ届く数にする。
const BURST_MISSES: usize = MAX_RECENT_AUDIO_MISSES + 3;

/// 理由ごとの累積を並べる順 (公開 API の並びに合わせる)
const REASONS: [AudioMissReason; 4] = [
    AudioMissReason::Backlog,
    AudioMissReason::CatchUp,
    AudioMissReason::Error,
    AudioMissReason::Stopped,
];

/// 理由の並びの中の位置
fn reason_index(reason: AudioMissReason) -> usize {
    match reason {
        AudioMissReason::Backlog => 0,
        AudioMissReason::CatchUp => 1,
        AudioMissReason::Error => 2,
        AudioMissReason::Stopped => 3,
    }
}

/// 鳴らさなかった音 1 件の記録 (直近の一覧の照合用)
#[derive(Debug, Clone, Copy)]
struct MissRecord {
    at_us: i64,
    reason: AudioMissReason,
    duration_us: i64,
    slack_us: Option<i64>,
}

/// 記録列から期待される観測値
///
/// 仕様 (窓の外の記録は分布から落とす、累積は理由ごとに数える、直近の一覧は上限を超えたら
/// 古い方から捨てる) をそのまま書き、実装とは独立に期待値を求める。
struct TimingModel {
    /// 分布を求める直近の窓 (マイクロ秒)
    window_us: i64,
    /// 鳴らすと決めた音 (古い順)
    plays: Vec<AudioPlayoutPlay>,
    /// 鳴らさなかった音 (古い順)
    misses: Vec<MissRecord>,
    /// 予約したが、まだ鳴り始めていない音の区間 (鳴り始める時刻, 鳴り終わる時刻)
    pending: Vec<(i64, i64)>,
    /// 理由ごとの累積 (REASONS の並び)
    totals: [AudioMissTotal; 4],
    played_frames: u64,
    played_us: i64,
    arrival_planned_frames: u64,
    unplanned_frames: u64,
    missed_frames: u64,
    missed_us: i64,
    /// 予定を過ぎて届いた (余裕が負の) 記録がある
    negative_slack: bool,
    /// 残りの長さを数えた停止がある
    stopped_counted: bool,
    /// 数える音の無かった停止がある
    stopped_not_counted: bool,
}

impl TimingModel {
    /// 何も記録していない状態で作る
    fn new(window_us: i64) -> Self {
        Self {
            window_us,
            plays: Vec::new(),
            misses: Vec::new(),
            pending: Vec::new(),
            totals: [AudioMissTotal::default(); 4],
            played_frames: 0,
            played_us: 0,
            arrival_planned_frames: 0,
            unplanned_frames: 0,
            missed_frames: 0,
            missed_us: 0,
            negative_slack: false,
            stopped_counted: false,
            stopped_not_counted: false,
        }
    }

    /// 鳴らすと決めた音を記録する
    fn record_play(&mut self, play: AudioPlayoutPlay) {
        // 鳴り終わった予約は、この記録の時刻の時点で落とす
        self.pending.retain(|(_, end_us)| *end_us > play.arrival_us);
        let played_us = play.played_us.max(0);
        self.played_frames += 1;
        self.played_us += played_us;
        match play.basis {
            AudioPlayoutBasis::Arrival => self.arrival_planned_frames += 1,
            AudioPlayoutBasis::Timestamp => {
                if play.target_start_us.is_none() {
                    self.unplanned_frames += 1;
                }
            }
        }
        self.pending
            .push((play.start_at_us, play.start_at_us + played_us));
        if play
            .target_start_us
            .is_some_and(|target_us| target_us < play.arrival_us)
        {
            self.negative_slack = true;
        }
        self.plays.push(play);
    }

    /// 鳴らさなかった音を記録する
    fn record_miss(&mut self, miss: AudioPlayoutMiss) {
        self.pending.retain(|(_, end_us)| *end_us > miss.at_us);
        let slack_us = match (miss.target_us, miss.arrival_us) {
            (Some(target_us), Some(arrival_us)) => Some(target_us - arrival_us),
            _ => None,
        };
        self.add_miss(miss.at_us, miss.reason, miss.duration_us, slack_us);
    }

    /// 鳴らさずに再生を止めた分を記録する
    fn record_stopped(&mut self, now_us: i64) {
        self.pending.retain(|(_, end_us)| *end_us > now_us);
        // 数えながら一覧を書き換えられないため、いったん取り出す
        let pending = std::mem::take(&mut self.pending);
        let mut counted = false;
        for (start_us, end_us) in pending {
            let remaining_us = end_us - start_us.max(now_us);
            if remaining_us <= 0 {
                continue;
            }
            counted = true;
            self.add_miss(now_us, AudioMissReason::Stopped, remaining_us, None);
        }
        if counted {
            self.stopped_counted = true;
        } else {
            self.stopped_not_counted = true;
        }
    }

    /// 記録をすべて捨てる
    fn reset(&mut self) {
        self.plays.clear();
        self.misses.clear();
        self.pending.clear();
        self.totals = [AudioMissTotal::default(); 4];
        self.played_frames = 0;
        self.played_us = 0;
        self.arrival_planned_frames = 0;
        self.unplanned_frames = 0;
        self.missed_frames = 0;
        self.missed_us = 0;
        self.negative_slack = false;
        self.stopped_counted = false;
        self.stopped_not_counted = false;
    }

    /// 鳴らさなかった 1 件を累積と直近の一覧へ加える
    fn add_miss(
        &mut self,
        at_us: i64,
        reason: AudioMissReason,
        duration_us: i64,
        slack_us: Option<i64>,
    ) {
        let duration_us = duration_us.max(0);
        self.missed_frames += 1;
        self.missed_us += duration_us;
        let total = &mut self.totals[reason_index(reason)];
        total.count += 1;
        total.duration_us += duration_us;
        if slack_us.is_some_and(|slack_us| slack_us < 0) {
            self.negative_slack = true;
        }
        self.misses.push(MissRecord {
            at_us,
            reason,
            duration_us,
            slack_us,
        });
    }

    /// 公開 API の並びの理由別累積
    fn totals_snapshot(&self) -> AudioMissTotals {
        AudioMissTotals {
            backlog: self.totals[reason_index(AudioMissReason::Backlog)],
            catch_up: self.totals[reason_index(AudioMissReason::CatchUp)],
            error: self.totals[reason_index(AudioMissReason::Error)],
            stopped: self.totals[reason_index(AudioMissReason::Stopped)],
        }
    }

    /// 直近の一覧に残るはずの記録 (古い順、最大 `MAX_RECENT_AUDIO_MISSES` 件)
    fn recent_misses(&self) -> Vec<AudioMissEvent> {
        let start = self.misses.len().saturating_sub(MAX_RECENT_AUDIO_MISSES);
        self.misses[start..]
            .iter()
            .map(|miss| AudioMissEvent {
                at_us: miss.at_us,
                reason: miss.reason,
                duration_us: miss.duration_us,
                slack_us: miss.slack_us,
            })
            .collect()
    }

    /// 到着から鳴り始めるまでの時間の列 (記録の時刻, 値)。古い順
    fn start_delay_series(&self) -> Vec<(i64, i64)> {
        self.plays
            .iter()
            .map(|play| (play.arrival_us, play.start_at_us - play.arrival_us))
            .collect()
    }

    /// 予定に対する余裕の列 (記録の時刻, 値)。予定の無い音は入れない
    fn slack_series(&self) -> Vec<(i64, i64)> {
        self.plays
            .iter()
            .filter_map(|play| {
                play.target_start_us
                    .map(|target_us| (play.arrival_us, target_us - play.arrival_us))
            })
            .collect()
    }

    /// 予定からどれだけ過ぎて鳴るかの列 (記録の時刻, 値)。予定の無い音は入れない
    fn lateness_series(&self) -> Vec<(i64, i64)> {
        self.plays
            .iter()
            .filter_map(|play| {
                play.target_start_us
                    .map(|target_us| (play.arrival_us, play.start_at_us - target_us))
            })
            .collect()
    }
}

/// 分岐ごとの到達を数えるゲート
///
/// 検証が空振りしていないことを確かめるために使う。1 つでも false のままなら、生成器が
/// その分岐へ到達していない。
#[derive(Default)]
struct Gates {
    /// 時間軸の目標で鳴らした音 (余裕と遅れを分布へ入れる)
    timestamp_with_target: Cell<bool>,
    /// 到着基準の計画で鳴らした音
    arrival_basis: Cell<bool>,
    /// 計画を持たないまま鳴らした音
    unplanned: Cell<bool>,
    /// 余裕の分かる鳴らさなかった音
    miss_slack_known: Cell<bool>,
    /// 余裕の分からない鳴らさなかった音
    miss_slack_unknown: Cell<bool>,
    /// 予定を過ぎて届いた (余裕が負の) 記録
    negative_slack: Cell<bool>,
    /// 残りの長さを数えた停止
    stopped_counted: Cell<bool>,
    /// 数える音の無かった停止
    stopped_not_counted: Cell<bool>,
    /// 窓の中に値のある分布
    window_values: Cell<bool>,
    /// 窓の外へ落とした値のある分布
    pruned_values: Cell<bool>,
    /// 記録はあるが窓の中に値が無い分布
    empty_window_with_records: Cell<bool>,
    /// 直近の一覧が上限に届いた
    recent_limit: Cell<bool>,
    /// 4 つの理由すべてを数えた
    all_reasons: Cell<bool>,
}

/// nearest-rank 法 (昇順に並べて `ceil(p * n)` 番目) で分布を要約する
///
/// 期待値は実装とは別に、仕様をそのまま書いて求める。
fn nearest_rank_summary(values: &[i64]) -> Option<TimingSummary> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = |numerator: usize, denominator: usize| {
        (sorted.len() * numerator).div_ceil(denominator).max(1) - 1
    };
    Some(TimingSummary {
        p50: sorted[index(1, 2)],
        p95: sorted[index(19, 20)],
        max: sorted[index(1, 1)],
    })
}

/// 分布が窓の中の値だけから求めた要約と一致し、値域の順序を守ることを確かめる
///
/// `inclusive` が true なら `min_at_us` 以降、false なら `min_at_us` より後の値を窓の中と
/// みなす (短い窓の `since` は境目を含めない)。戻り値は (窓の中の値の数, 窓の外の値の数)。
fn check_distribution(
    summary: &Option<TimingSummary>,
    series: &[(i64, i64)],
    min_at_us: i64,
    inclusive: bool,
    label: &str,
) -> (usize, usize) {
    let values: Vec<i64> = series
        .iter()
        .filter(|(at_us, _)| {
            if inclusive {
                *at_us >= min_at_us
            } else {
                *at_us > min_at_us
            }
        })
        .map(|(_, value_us)| *value_us)
        .collect();
    let expected = nearest_rank_summary(&values);
    assert_eq!(
        summary, &expected,
        "{label}: 窓の中の値の要約と一致しない: min_at_us={min_at_us}"
    );
    if let Some(summary) = summary {
        // 分布の値域: 窓の中の値の範囲に収まり、p50 <= p95 <= max であること
        let min_value = values.iter().min().copied().expect("窓の中に値がある");
        let max_value = values.iter().max().copied().expect("窓の中に値がある");
        assert!(
            min_value <= summary.p50
                && summary.p50 <= summary.p95
                && summary.p95 <= summary.max
                && summary.max <= max_value,
            "{label}: 分布の値域が崩れた: {summary:?} values=[{min_value}, {max_value}]"
        );
    }
    (values.len(), series.len() - values.len())
}

/// 観測値がモデルと一致し、不変条件を守ることを確かめ、到達した分岐を数える
fn check_snapshot(
    gates: &Gates,
    snapshot: &AudioPlayoutTimingSnapshot,
    model: &TimingModel,
    now_us: i64,
) {
    // 累積 (モデルの数え直しと一致すること)
    assert_eq!(
        snapshot.played_frames, model.played_frames,
        "鳴らすと決めた音の数が一致しない: now_us={now_us}"
    );
    assert_eq!(
        snapshot.played_us, model.played_us,
        "鳴らすと決めた音の長さが一致しない: now_us={now_us}"
    );
    assert_eq!(
        snapshot.arrival_planned_frames, model.arrival_planned_frames,
        "到着基準で鳴らした音の数が一致しない: now_us={now_us}"
    );
    assert_eq!(
        snapshot.unplanned_frames, model.unplanned_frames,
        "計画を持たない音の数が一致しない: now_us={now_us}"
    );
    assert_eq!(
        snapshot.missed_frames, model.missed_frames,
        "鳴らさなかった音の数が一致しない: now_us={now_us}"
    );
    assert_eq!(
        snapshot.missed_us, model.missed_us,
        "鳴らさなかった音の長さが一致しない: now_us={now_us}"
    );

    // 理由ごとの累積と、その和が合計に一致すること
    let mut counted = 0u64;
    let mut counted_us = 0i64;
    for reason in REASONS {
        let total = model.totals[reason_index(reason)];
        assert_eq!(
            snapshot.missed_by_reason.get(reason),
            total,
            "理由別の累積が一致しない: reason={reason:?} now_us={now_us}"
        );
        counted += total.count;
        counted_us += total.duration_us;
    }
    assert_eq!(
        snapshot.missed_by_reason,
        model.totals_snapshot(),
        "理由別の累積の並びが一致しない: now_us={now_us}"
    );
    assert_eq!(
        counted, snapshot.missed_frames,
        "理由別の件数の和が合計に一致しない: now_us={now_us}"
    );
    assert_eq!(
        counted_us, snapshot.missed_us,
        "理由別の長さの和が合計に一致しない: now_us={now_us}"
    );

    // 直近の一覧は上限を超えず、記録の末尾と一致すること
    assert!(
        snapshot.recent_misses.len() <= MAX_RECENT_AUDIO_MISSES,
        "直近の一覧が上限を超えた: len={} now_us={now_us}",
        snapshot.recent_misses.len()
    );
    assert_eq!(
        snapshot.recent_misses,
        model.recent_misses(),
        "直近の一覧が一致しない: now_us={now_us}"
    );

    // 直近に鳴らすと決めた音の値
    match model.plays.last() {
        Some(play) => {
            assert_eq!(
                snapshot.last_target_us, play.target_start_us,
                "直近の再生予定時刻が一致しない: now_us={now_us}"
            );
            assert_eq!(
                snapshot.last_arrival_us,
                Some(play.arrival_us),
                "直近の到着時刻が一致しない: now_us={now_us}"
            );
            assert_eq!(
                snapshot.last_start_us,
                Some(play.start_at_us),
                "直近に鳴り始める時刻が一致しない: now_us={now_us}"
            );
            assert_eq!(
                snapshot.last_slack_us,
                play.target_start_us
                    .map(|target_us| target_us - play.arrival_us),
                "直近の余裕が一致しない: now_us={now_us}"
            );
            assert_eq!(
                snapshot.last_start_delay_us,
                Some(play.start_at_us - play.arrival_us),
                "直近の鳴るまでの時間が一致しない: now_us={now_us}"
            );
            assert_eq!(
                snapshot.last_lateness_us,
                play.target_start_us
                    .map(|target_us| play.start_at_us - target_us),
                "直近の遅れが一致しない: now_us={now_us}"
            );
        }
        None => {
            assert_eq!(
                snapshot.last_target_us, None,
                "鳴らしていなければ直近の再生予定時刻は無い: now_us={now_us}"
            );
            assert_eq!(snapshot.last_arrival_us, None);
            assert_eq!(snapshot.last_start_us, None);
            assert_eq!(snapshot.last_slack_us, None);
            assert_eq!(snapshot.last_start_delay_us, None);
            assert_eq!(snapshot.last_lateness_us, None);
        }
    }

    // 分布 (窓の中の値だけから求めること)
    let min_at_us = now_us - model.window_us;
    let (delay_in, delay_out) = check_distribution(
        &snapshot.start_delay_us,
        &model.start_delay_series(),
        min_at_us,
        true,
        "到着から鳴り始めるまでの時間",
    );
    let (slack_in, slack_out) = check_distribution(
        &snapshot.slack_us,
        &model.slack_series(),
        min_at_us,
        true,
        "予定に対する余裕",
    );
    let (lateness_in, lateness_out) = check_distribution(
        &snapshot.lateness_us,
        &model.lateness_series(),
        min_at_us,
        true,
        "予定からの遅れ",
    );

    // 到達した分岐を数える (検証と同じ場所で数える)
    if snapshot.played_frames > snapshot.arrival_planned_frames + snapshot.unplanned_frames {
        gates.timestamp_with_target.set(true);
    }
    if snapshot.arrival_planned_frames > 0 {
        gates.arrival_basis.set(true);
    }
    if snapshot.unplanned_frames > 0 {
        gates.unplanned.set(true);
    }
    if model.misses.iter().any(|miss| miss.slack_us.is_some()) {
        gates.miss_slack_known.set(true);
    }
    if model.misses.iter().any(|miss| miss.slack_us.is_none()) {
        gates.miss_slack_unknown.set(true);
    }
    if model.negative_slack {
        gates.negative_slack.set(true);
    }
    if model.stopped_counted {
        gates.stopped_counted.set(true);
    }
    if model.stopped_not_counted {
        gates.stopped_not_counted.set(true);
    }
    if delay_in > 0 || slack_in > 0 || lateness_in > 0 {
        gates.window_values.set(true);
    }
    if delay_out > 0 || slack_out > 0 || lateness_out > 0 {
        gates.pruned_values.set(true);
    }
    if (delay_in == 0 && delay_out > 0)
        || (slack_in == 0 && slack_out > 0)
        || (lateness_in == 0 && lateness_out > 0)
    {
        gates.empty_window_with_records.set(true);
    }
    if snapshot.recent_misses.len() == MAX_RECENT_AUDIO_MISSES {
        gates.recent_limit.set(true);
    }
    if REASONS
        .iter()
        .all(|reason| snapshot.missed_by_reason.get(*reason).count > 0)
    {
        gates.all_reasons.set(true);
    }
}

/// 目標の有無と計画の種類を独立に選んだ、鳴らすと決めた音を作る
fn sample_play(ctx: &mut noprop::TestCaseContext, now_us: i64) -> AudioPlayoutPlay {
    // 予定は到着の前後から選ぶ (過去の予定は余裕を負にする)
    let target_start_us = if noprop::sample_bool(ctx) {
        Some(
            now_us
                + noprop::sample_with_boundaries(
                    ctx,
                    &[-100_000, -1_000, 0, 1_000, 100_000],
                    noprop::Ratio::one_nth(4),
                    |ctx| noprop::sample_usize_in(ctx, 0..=200_000) as i64 - 100_000,
                ),
        )
    } else {
        None
    };
    let start_at_us = now_us + noprop::sample_usize_in(ctx, 0..=100_000) as i64;
    AudioPlayoutPlay {
        arrival_us: now_us,
        target_start_us,
        start_at_us,
        played_us: noprop::sample_usize_in(ctx, 0..=50_000) as i64,
        // 到着基準と時間軸の目標の両方を選ぶ
        basis: if noprop::sample_bool(ctx) {
            AudioPlayoutBasis::Timestamp
        } else {
            AudioPlayoutBasis::Arrival
        },
    }
}

/// 予定と到着を独立に選んだ、鳴らさなかった音を作る (余裕の有無を作る)
fn sample_miss(ctx: &mut noprop::TestCaseContext, now_us: i64) -> AudioPlayoutMiss {
    AudioPlayoutMiss {
        at_us: now_us,
        // 4 つの理由を満遍なく選ぶ (並べすぎは閉ループの観測にも使う)
        reason: REASONS[noprop::sample_weighted_index(ctx, &[1, 1, 1, 1])],
        duration_us: noprop::sample_usize_in(ctx, 0..=50_000) as i64,
        target_us: if noprop::sample_bool(ctx) {
            Some(
                now_us
                    + noprop::sample_with_boundaries(
                        ctx,
                        &[-100_000, -1_000, 0, 1_000, 100_000],
                        noprop::Ratio::one_nth(4),
                        |ctx| noprop::sample_usize_in(ctx, 0..=200_000) as i64 - 100_000,
                    ),
            )
        } else {
            None
        },
        arrival_us: if noprop::sample_bool(ctx) {
            Some(now_us - noprop::sample_usize_in(ctx, 0..=50_000) as i64)
        } else {
            None
        },
    }
}

#[test]
fn records_match_the_model_and_stay_within_the_window() -> noprop::TestResult {
    let gates = Gates::default();
    let reset_with_records = Cell::new(false);
    let recorded_after_reset = Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        // 窓は 0 (境目だけ) から既定の 10 秒まで、境界を含めて選ぶ
        let window_us = noprop::sample_with_boundaries(
            ctx,
            &[
                0,
                1_000,
                AUDIO_PLAYOUT_TIMING_WINDOW_US / 10,
                AUDIO_PLAYOUT_TIMING_WINDOW_US,
            ],
            noprop::Ratio::one_nth(4),
            |ctx| {
                noprop::sample_usize_in(ctx, 1..=(AUDIO_PLAYOUT_TIMING_WINDOW_US as usize)) as i64
            },
        );
        let mut stats = AudioPlayoutTimingStats::with_window_us(window_us);
        let mut model = TimingModel::new(window_us);
        let mut now_us = noprop::sample_usize_in(ctx, 0..1_000_000) as i64;
        let mut previous = AudioPlayoutTimingSnapshot::default();
        let mut reset_at: Option<usize> = None;
        for step in 0..noprop::sample_usize_in(ctx, 1..=24) {
            // 時刻は窓より大きく進むこともある (窓の外の記録を作る)
            now_us += noprop::sample_with_boundaries(
                ctx,
                &[0, 1_000, 100_000, AUDIO_PLAYOUT_TIMING_WINDOW_US],
                noprop::Ratio::one_nth(4),
                |ctx| noprop::sample_usize_in(ctx, 0..=2_000_000) as i64,
            );
            let had_records = model.played_frames > 0 || model.missed_frames > 0;
            let op = noprop::sample_weighted_index(ctx, &OP_WEIGHTS);
            match op {
                OP_PLAY => {
                    let play = sample_play(ctx, now_us);
                    stats.record_play(play);
                    model.record_play(play);
                }
                OP_MISS => {
                    let miss = sample_miss(ctx, now_us);
                    stats.record_miss(miss);
                    model.record_miss(miss);
                }
                OP_STOPPED => {
                    stats.record_stopped(now_us);
                    model.record_stopped(now_us);
                }
                OP_BURST => {
                    // 直近の一覧の上限を超えるまで、鳴らさなかった音をまとめて記録する
                    for _ in 0..BURST_MISSES {
                        let miss = sample_miss(ctx, now_us);
                        stats.record_miss(miss);
                        model.record_miss(miss);
                    }
                }
                _ => {
                    stats.reset();
                    model.reset();
                }
            }
            if op == OP_RESET {
                if had_records {
                    reset_with_records.set(true);
                }
                reset_at = Some(step);
            } else if reset_at.is_some() {
                recorded_after_reset.set(true);
            }

            let snapshot = stats.snapshot(now_us);
            if op != OP_RESET {
                // 累積は減らないこと
                assert!(
                    snapshot.played_frames >= previous.played_frames,
                    "鳴らすと決めた音の数が減った: step={step} now_us={now_us}"
                );
                assert!(
                    snapshot.played_us >= previous.played_us,
                    "鳴らすと決めた音の長さが減った: step={step} now_us={now_us}"
                );
                assert!(
                    snapshot.arrival_planned_frames >= previous.arrival_planned_frames,
                    "到着基準で鳴らした音の数が減った: step={step} now_us={now_us}"
                );
                assert!(
                    snapshot.unplanned_frames >= previous.unplanned_frames,
                    "計画を持たない音の数が減った: step={step} now_us={now_us}"
                );
                assert!(
                    snapshot.missed_frames >= previous.missed_frames,
                    "鳴らさなかった音の数が減った: step={step} now_us={now_us}"
                );
                assert!(
                    snapshot.missed_us >= previous.missed_us,
                    "鳴らさなかった音の長さが減った: step={step} now_us={now_us}"
                );
            }
            check_snapshot(&gates, &snapshot, &model, now_us);
            previous = snapshot;
        }
        Ok(())
    })?;

    // 到達した分岐を確かめる。0 のままなら検証が空振りしている
    assert!(
        gates.timestamp_with_target.get(),
        "時間軸の目標で鳴らした音を通っていない\n{runner}"
    );
    assert!(
        gates.arrival_basis.get(),
        "到着基準で鳴らした音を通っていない\n{runner}"
    );
    assert!(
        gates.unplanned.get(),
        "計画を持たないまま鳴らした音を通っていない\n{runner}"
    );
    assert!(
        gates.miss_slack_known.get(),
        "余裕の分かる鳴らさなかった音を通っていない\n{runner}"
    );
    assert!(
        gates.miss_slack_unknown.get(),
        "余裕の分からない鳴らさなかった音を通っていない\n{runner}"
    );
    assert!(
        gates.negative_slack.get(),
        "予定を過ぎて届いた音を通っていない\n{runner}"
    );
    assert!(
        gates.stopped_counted.get(),
        "残りの長さを数える停止を通っていない\n{runner}"
    );
    assert!(
        gates.stopped_not_counted.get(),
        "数える音の無い停止を通っていない\n{runner}"
    );
    assert!(
        gates.window_values.get(),
        "窓の中に値のある分布を通っていない\n{runner}"
    );
    assert!(
        gates.pruned_values.get(),
        "窓の外へ落とす値のある分布を通っていない\n{runner}"
    );
    assert!(
        gates.empty_window_with_records.get(),
        "記録はあるが窓の中に値が無い分布を通っていない\n{runner}"
    );
    assert!(
        gates.recent_limit.get(),
        "直近の一覧が上限に届くまで記録した場合を通っていない\n{runner}"
    );
    assert!(
        gates.all_reasons.get(),
        "鳴らさなかった理由を 4 つとも数えた場合を通っていない\n{runner}"
    );
    assert!(
        reset_with_records.get(),
        "記録を入れた後のリセットを通っていない\n{runner}"
    );
    assert!(
        recorded_after_reset.get(),
        "リセットの後に記録を入れ直す場合を通っていない\n{runner}"
    );
    Ok(())
}

#[test]
fn audio_delay_feedback_uses_the_short_window() -> noprop::TestResult {
    let with_values = Cell::new(false);
    let empty_window = Cell::new(false);
    let narrower_than_display = Cell::new(false);
    let backlog_seen = Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let mut stats = AudioPlayoutTimingStats::new();
        let mut model = TimingModel::new(AUDIO_PLAYOUT_TIMING_WINDOW_US);
        let mut now_us = noprop::sample_usize_in(ctx, 0..1_000_000) as i64;
        for step in 0..noprop::sample_usize_in(ctx, 1..=16) {
            now_us += noprop::sample_with_boundaries(
                ctx,
                &[0, 1_000, AUDIO_DELAY_FEEDBACK_WINDOW_US, 1_000_000],
                noprop::Ratio::one_nth(4),
                |ctx| noprop::sample_usize_in(ctx, 0..=500_000) as i64,
            );
            match noprop::sample_weighted_index(ctx, &[4, 2, 2]) {
                0 => {
                    let play = sample_play(ctx, now_us);
                    stats.record_play(play);
                    model.record_play(play);
                }
                1 => {
                    let miss = sample_miss(ctx, now_us);
                    stats.record_miss(miss);
                    model.record_miss(miss);
                }
                _ => {
                    // 短い窓より長く、表示用の窓より短い分だけ進める。短い窓が空になり、
                    // 表示用の窓には値が残る場合を作る
                    now_us += AUDIO_DELAY_FEEDBACK_WINDOW_US
                        + noprop::sample_usize_in(ctx, 0..=500_000) as i64;
                }
            }

            let feedback = stats.audio_delay_feedback(now_us);
            assert_eq!(
                feedback.at_us, now_us,
                "観測した時刻を返すこと: step={step}"
            );
            // 捨てた量は購読の開始からの累積であること
            let backlog = model.totals[reason_index(AudioMissReason::Backlog)];
            assert_eq!(
                feedback.backlog_misses, backlog.count,
                "累積の捨てた数が一致しない: step={step} now_us={now_us}"
            );
            assert_eq!(
                feedback.backlog_us, backlog.duration_us,
                "累積の捨てた長さが一致しない: step={step} now_us={now_us}"
            );
            // 分布は短い窓 (1 秒) の中の値だけから求めること。境目は含めない
            let control_min_us = now_us - AUDIO_DELAY_FEEDBACK_WINDOW_US;
            let (delay_in, _) = check_distribution(
                &feedback.start_delay_us,
                &model.start_delay_series(),
                control_min_us,
                false,
                "短い窓の鳴るまでの時間",
            );
            let (slack_in, _) = check_distribution(
                &feedback.slack_us,
                &model.slack_series(),
                control_min_us,
                false,
                "短い窓の余裕",
            );
            let (lateness_in, _) = check_distribution(
                &feedback.lateness_us,
                &model.lateness_series(),
                control_min_us,
                false,
                "短い窓の遅れ",
            );

            // 表示用の窓 (10 秒) には値があるのに、短い窓 (1 秒) には無い場合
            let display = stats.snapshot(now_us);
            if display.start_delay_us.is_some()
                && feedback.start_delay_us.is_none()
                && !model.start_delay_series().is_empty()
            {
                narrower_than_display.set(true);
            }
            if delay_in > 0 || slack_in > 0 || lateness_in > 0 {
                with_values.set(true);
            }
            if feedback.start_delay_us.is_none()
                && feedback.slack_us.is_none()
                && feedback.lateness_us.is_none()
                && !model.start_delay_series().is_empty()
            {
                empty_window.set(true);
            }
            if backlog.count > 0 {
                backlog_seen.set(true);
            }
        }
        Ok(())
    })?;

    assert!(
        with_values.get(),
        "短い窓に値のある観測を通っていない\n{runner}"
    );
    assert!(
        empty_window.get(),
        "記録はあるが短い窓に値が無い観測を通っていない\n{runner}"
    );
    assert!(
        narrower_than_display.get(),
        "短い窓の方が表示用の窓より狭い場合を通っていない\n{runner}"
    );
    assert!(
        backlog_seen.get(),
        "並べすぎて捨てた音のある観測を通っていない\n{runner}"
    );
    Ok(())
}
