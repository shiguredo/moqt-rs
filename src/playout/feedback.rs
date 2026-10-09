//! 音声の目標遅延を、実際に鳴った結果から閉ループで決める
//!
//! [`crate::playout::delay`] の学習 (NetEq の移植) は「直近の窓で最も早く届いた音を基準に
//! した相対的な到着の遅れ」の分位点であり、経路の揺らぎは吸収できるが、ストリーム全体が
//! 一様に遅れている分は見えない。基準 (最小値) そのものが下へ動くため、相対の遅れは 0 の
//! ままになるためである。実際には、到着から鳴り始めるまでに復号・予約・出力のバッファ・
//! まとめて届いた山の分だけ時間がかかり、学習した目標より遅れて鳴る。すると予定を過ぎて
//! 鳴り続け、並べすぎの音が捨てられる。
//!
//! そこで、実際に観測した「予定をどれだけ過ぎて鳴ったか」(遅れ) と「並べすぎで捨てた量」を
//! 見て目標を増減する。
//!
//! - 遅れが続くなら目標を増やす (乱れと、到着から鳴るまでの経路の分を吸収する)
//! - 並べすぎで捨てたなら、捨てた長さぶんを吸収できるだけ増やす
//! - 遅れが許容の中に収まり、捨てが無いなら目標を減らす (必要以上に遅らせない)
//! - 増減は毎秒 1 回までとし、1 回の増加は [`AUDIO_DELAY_FEEDBACK_MAX_STEP_US`] までにする
//!   (数秒スケール。行き過ぎを防ぐ)
//! - 目標は [`AUDIO_DELAY_FEEDBACK_MIN_US`] から [`AUDIO_DELAY_FEEDBACK_MAX_US`] の間に
//!   収める
//! - `targetLatency` を明示設定したときは、その値を自動で超えない
//!   ([`AudioDelayFeedback::set_ceiling_us`] で渡す)。jitter buffer の学習値
//!   ([`AudioDelayFeedback::target_delay_us`] へ渡す値) には掛けない。既存の揺らぎの吸収を
//!   変えないためである
//!
//! 判断に使う分布は、表示用の 10 秒の窓ではなく短い窓
//! ([`crate::playout::timing::AUDIO_DELAY_FEEDBACK_WINDOW_US`]) の値にする。目標を増やした
//! 結果が現れるまで 10 秒の窓は待つため、その窓だけで増やすと行き過ぎる (実際の遅れが
//! 消えた後も増え続けて上限に張り付く)。増やす量も、遅れの p50 を使う。まれな大きな跳ね
//! (p95) まで吸収しようとすると、目標が上限に張り付いて遅延だけが増えるためである。
//!
//! 時刻はすべて呼び出し側が引数で渡すマイクロ秒である。時計もデバイスも触らない。
//! 観測は購読側が作る値
//! ([`AudioPlayoutTimingStats::audio_delay_feedback`](crate::playout::timing::AudioPlayoutTimingStats::audio_delay_feedback))
//! をそのまま [`AudioDelayFeedback::update`] へ渡す。

use crate::playout::timing::AudioDelayFeedbackObservation;

/// 目標遅延の下限 (マイクロ秒)
///
/// これより下げると到着の揺らぎを吸収できず、音が途切れる。
pub const AUDIO_DELAY_FEEDBACK_MIN_US: i64 = 80_000;

/// 目標遅延の上限 (マイクロ秒)
///
/// まれな大きな乱れまで吸収しようとすると、常に大きく遅れて鳴ることになるため上限を置く。
/// 表示の遅れの上限 ([`crate::playout::timeline::TIMELINE_MAX_PRESENTATION_DELAY_MS`] の
/// 500 ms) より小さい。
pub const AUDIO_DELAY_FEEDBACK_MAX_US: i64 = 300_000;

/// 最初の目標遅延 (マイクロ秒)
///
/// 観測がまだ無いときに使う。jitter buffer の学習の初期値
/// ([`crate::playout::delay::AUDIO_DELAY_START_MS`] の 80 ms) より少し大きくし、到着から
/// 鳴るまでの経路の分を最初から見込む。
pub const AUDIO_DELAY_FEEDBACK_START_US: i64 = 100_000;

/// 目標を動かす間隔 (マイクロ秒)
///
/// 数秒スケールで動かし、行き過ぎと往復を避ける。観測が来るたびに動かすと、増やした結果が
/// 観測へ現れる前に増やし続けて上限に張り付く。
pub const AUDIO_DELAY_FEEDBACK_INTERVAL_US: i64 = 1_000_000;

/// 遅れを許容する量 (マイクロ秒)
///
/// これ以下なら「予定どおり鳴っている」とみなし、目標を減らす向きに動かす。
pub const AUDIO_DELAY_FEEDBACK_TOLERANCE_US: i64 = 10_000;

/// 目標を増やすときに足す余白 (マイクロ秒)
///
/// 許容の境目で往復しないようにする。
pub const AUDIO_DELAY_FEEDBACK_MARGIN_US: i64 = 20_000;

/// 1 回で増やす下限 (マイクロ秒)
pub const AUDIO_DELAY_FEEDBACK_MIN_STEP_US: i64 = 20_000;

/// 1 回で増やす上限 (マイクロ秒)
///
/// 数秒で収束させつつ、行き過ぎないようにする。
pub const AUDIO_DELAY_FEEDBACK_MAX_STEP_US: i64 = 40_000;

/// 目標を減らす速さ (マイクロ秒 / 秒)
///
/// 増やす向きより遅くし、往復を避ける。速すぎると、遅れが現れるまでに下げすぎて
/// (観測の窓が追いつく前に) 遅れを作り直す。
pub const AUDIO_DELAY_FEEDBACK_DECREASE_US_PER_SECOND: i64 = 10_000;

/// 目標を動かした理由
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDelayFeedbackReason {
    /// まだ動かしていない (初期値のまま)
    Initial,
    /// 並べすぎで捨てたため増やした
    Backlog,
    /// 予定を過ぎて鳴ったため増やした
    Lateness,
    /// 遅れが許容の中に収まり、捨てが無いため減らした
    Settled,
    /// 動かす条件がそろっていない (観測が無い、または前回から間隔が空いていない)
    Waiting,
}

/// 閉ループの状態 (計器とテスト用)
///
/// 実際に使う目標遅延 (jitter buffer の学習値との大きい方) は
/// [`AudioDelayFeedback::target_delay_us`] が返す。ここには閉ループが決めた分だけを出す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioDelayFeedbackSnapshot {
    /// 閉ループが決めた目標遅延 (マイクロ秒)。下限と上限と明示の上限を適用した値
    pub target_us: i64,
    /// 直前に目標を動かした理由
    pub reason: AudioDelayFeedbackReason,
    /// 明示設定の上限 (マイクロ秒)。無ければ None
    pub ceiling_us: Option<i64>,
    /// 直前の制御で動かした量 (マイクロ秒)。0 なら動かしていない
    pub last_change_us: i64,
    /// 目標を動かした回数
    pub adjustments: u64,
    /// 直近の観測の、予定を過ぎて鳴った量の p50 (マイクロ秒)。観測が無ければ None
    pub lateness_p50_us: Option<i64>,
    /// 直近の観測の、到着から鳴り始めるまでの時間の p50 (マイクロ秒)。観測が無ければ None
    pub start_delay_p50_us: Option<i64>,
    /// 直近の観測の、予定に対する余裕の p50 (マイクロ秒)。観測が無ければ None
    pub slack_p50_us: Option<i64>,
}

/// 目標を下限と上限の中に収める
///
/// 上限が下限を下回るときは上限を優先する (明示された `targetLatency` を下限より優先する)。
/// `i64::clamp` は下限が上限を超えるとパニックするため使わない。
fn clamp_target_us(value_us: i64, lower_us: i64, upper_us: i64) -> i64 {
    value_us.max(lower_us).min(upper_us)
}

/// 音声の目標遅延を、実際に鳴った結果から決める
///
/// 音声トラックごとではなく、共有の時間軸 ([`crate::playout::timeline::PlayoutTimeline`]) に
/// 1 つ持つ。目標は到着から鳴るまでの経路 (復号・予約・出力のバッファ) の学習であり、
/// 購読のやり直しや TIMESTAMP の段差では変わらないためである。
#[derive(Debug)]
pub struct AudioDelayFeedback {
    /// 閉ループが決めた目標遅延 (マイクロ秒)
    target_us: i64,
    /// 直前に目標を動かした理由
    reason: AudioDelayFeedbackReason,
    /// 明示設定の上限 (マイクロ秒)。無ければ None
    ceiling_us: Option<i64>,
    /// 直前に目標を動かした時刻 (マイクロ秒)。まだ動かしていなければ None
    last_update_at_us: Option<i64>,
    /// 直前に読んだ、累積で並べすぎて捨てた音の数
    last_backlog_misses: u64,
    /// 直前に読んだ、累積で並べすぎて捨てた音の長さ (マイクロ秒)
    last_backlog_us: i64,
    /// 直前の制御で動かした量 (マイクロ秒)
    last_change_us: i64,
    /// 目標を動かした回数
    adjustments: u64,
    /// 直近の観測
    latest: Option<AudioDelayFeedbackObservation>,
    /// 実際に鳴った結果を 1 つでも観測したか。観測が無い間は閉ループの目標を使わない
    observed: bool,
}

impl AudioDelayFeedback {
    /// 観測の無い状態で作る
    pub fn new() -> Self {
        Self {
            target_us: AUDIO_DELAY_FEEDBACK_START_US,
            reason: AudioDelayFeedbackReason::Initial,
            ceiling_us: None,
            last_update_at_us: None,
            last_backlog_misses: 0,
            last_backlog_us: 0,
            last_change_us: 0,
            adjustments: 0,
            latest: None,
            observed: false,
        }
    }

    /// 明示設定の上限を渡す (`targetLatency`)
    ///
    /// 自動で決めた目標がこれを超えないようにする。値が変わった時点で、既に超えていれば
    /// その場で収める。`None` を渡すと上限を外す。負の値は 0 として扱う。
    pub fn set_ceiling_us(&mut self, ceiling_us: Option<i64>) {
        self.ceiling_us = ceiling_us.map(|ceiling_us| ceiling_us.max(0));
        self.target_us =
            clamp_target_us(self.target_us, AUDIO_DELAY_FEEDBACK_MIN_US, self.upper_us());
    }

    /// 今の上限 (マイクロ秒)。明示設定が無ければ [`AUDIO_DELAY_FEEDBACK_MAX_US`]
    pub fn ceiling_us(&self) -> Option<i64> {
        self.ceiling_us
    }

    /// 閉ループが決めた目標 (マイクロ秒)。jitter buffer の学習値を含まない
    pub fn feedback_target_us(&self) -> i64 {
        clamp_target_us(self.target_us, AUDIO_DELAY_FEEDBACK_MIN_US, self.upper_us())
    }

    /// 実際に使う目標遅延 (マイクロ秒)
    ///
    /// jitter buffer の学習値と、閉ループが決めた目標の大きい方にする。上限は閉ループの
    /// 値にだけ掛ける。学習値は既存の揺らぎの吸収そのものであり、明示設定で切り下げると
    /// 挙動が変わるためである。
    ///
    /// 実際に鳴った結果をまだ 1 つも観測していないときは、閉ループの目標を使わず
    /// `jitter_target_us` をそのまま返す。閉ループは観測を入力にする制御であり、観測が
    /// 無い間に目標を動かすと、まだ鳴らしていない間の表示の遅れ (購読を始めた直後の基準) が
    /// 変わるためである。
    pub fn target_delay_us(&self, jitter_target_us: i64) -> i64 {
        if !self.observed {
            return jitter_target_us;
        }
        jitter_target_us.max(self.feedback_target_us())
    }

    /// 観測を 1 つ受け取り、必要なら目標を動かす
    ///
    /// 目標を動かすのは [`AUDIO_DELAY_FEEDBACK_INTERVAL_US`] ごとに 1 回だけである。観測の
    /// 窓が短いため、間隔を空けずに動かすと、増やした結果が現れる前に増やし続ける。
    ///
    /// 並べすぎの累積は、購読のやり直しで 0 に戻っても負の差で増やさない。
    pub fn update(&mut self, observation: AudioDelayFeedbackObservation) {
        self.latest = Some(observation);
        // 1 つでも観測を受けたら、以降は閉ループの目標を使う
        self.observed = true;
        let elapsed_us = self
            .last_update_at_us
            .map(|last_update_at_us| observation.at_us.saturating_sub(last_update_at_us).max(0));
        if let Some(elapsed_us) = elapsed_us
            && elapsed_us < AUDIO_DELAY_FEEDBACK_INTERVAL_US
        {
            // まだ間隔に達していない。目標は動かさない
            self.reason = AudioDelayFeedbackReason::Waiting;
            return;
        }
        // 累積の捨てを、前回からの差にする (購読のやり直しで 0 に戻っても負にしない)
        let backlog_delta = observation
            .backlog_misses
            .saturating_sub(self.last_backlog_misses);
        let backlog_us_delta = observation
            .backlog_us
            .saturating_sub(self.last_backlog_us)
            .max(0);
        self.last_backlog_misses = observation.backlog_misses;
        self.last_backlog_us = observation.backlog_us;
        self.last_update_at_us = Some(observation.at_us);

        let lateness_p50_us = observation.lateness_us.map(|summary| summary.p50);
        let previous_target_us = self.feedback_target_us();
        let change_us = if backlog_delta > 0 {
            // 並べすぎで捨てた。捨てた長さぶんを吸収できるだけ増やす
            self.reason = AudioDelayFeedbackReason::Backlog;
            clamp_target_us(
                backlog_us_delta.saturating_add(AUDIO_DELAY_FEEDBACK_MARGIN_US),
                AUDIO_DELAY_FEEDBACK_MIN_STEP_US,
                AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
            )
        } else if let Some(lateness_p50_us) = lateness_p50_us
            && lateness_p50_us > AUDIO_DELAY_FEEDBACK_TOLERANCE_US
        {
            // 予定を過ぎて鳴っている。過ぎた分を吸収できるだけ増やす
            self.reason = AudioDelayFeedbackReason::Lateness;
            clamp_target_us(
                lateness_p50_us
                    .saturating_sub(AUDIO_DELAY_FEEDBACK_TOLERANCE_US)
                    .saturating_add(AUDIO_DELAY_FEEDBACK_MARGIN_US),
                AUDIO_DELAY_FEEDBACK_MIN_STEP_US,
                AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
            )
        } else if lateness_p50_us.is_some() {
            // 遅れが許容の中に収まり、捨てが無い。余裕が続くため減らす。減らす速さは前回の
            // 観測からの時間で決めるため、前回が無いときは動かさない (1 回の観測だけで
            // 「余裕が続いている」とは言えない)
            let Some(elapsed_us) = elapsed_us else {
                self.reason = AudioDelayFeedbackReason::Waiting;
                return;
            };
            self.reason = AudioDelayFeedbackReason::Settled;
            AUDIO_DELAY_FEEDBACK_DECREASE_US_PER_SECOND
                .saturating_mul(elapsed_us)
                .saturating_div(1_000_000)
                .saturating_neg()
        } else {
            // まだ鳴らしていないため、遅れが分からない。目標は動かさない
            self.reason = AudioDelayFeedbackReason::Waiting;
            return;
        };
        self.target_us = clamp_target_us(
            self.target_us.saturating_add(change_us),
            AUDIO_DELAY_FEEDBACK_MIN_US,
            self.upper_us(),
        );
        self.last_change_us = self.target_us.saturating_sub(previous_target_us);
        if self.last_change_us != 0 {
            self.adjustments = self.adjustments.saturating_add(1);
        }
    }

    /// 閉ループの状態を求める (計器用)
    pub fn snapshot(&self) -> AudioDelayFeedbackSnapshot {
        AudioDelayFeedbackSnapshot {
            target_us: self.feedback_target_us(),
            reason: self.reason,
            ceiling_us: self.ceiling_us,
            last_change_us: self.last_change_us,
            adjustments: self.adjustments,
            lateness_p50_us: self
                .latest
                .and_then(|latest| latest.lateness_us)
                .map(|s| s.p50),
            start_delay_p50_us: self
                .latest
                .and_then(|latest| latest.start_delay_us)
                .map(|s| s.p50),
            slack_p50_us: self
                .latest
                .and_then(|latest| latest.slack_us)
                .map(|s| s.p50),
        }
    }

    /// 学習を消して初期状態に戻す
    ///
    /// 目標は「到着から鳴るまでの経路」の学習であり購読のやり直しでは変わらないため、
    /// 通常は呼ばない (購読のやり直しで目標を初期値へ戻すと、その間だけ遅れが戻る)。
    /// 明示設定の上限は呼び出し側が決めた値であるため残す。
    pub fn reset(&mut self) {
        self.target_us = AUDIO_DELAY_FEEDBACK_START_US;
        self.reason = AudioDelayFeedbackReason::Initial;
        self.last_update_at_us = None;
        self.last_backlog_misses = 0;
        self.last_backlog_us = 0;
        self.last_change_us = 0;
        self.adjustments = 0;
        self.latest = None;
        self.observed = false;
    }

    /// 今の上限 (マイクロ秒)。明示設定が無ければ [`AUDIO_DELAY_FEEDBACK_MAX_US`]
    fn upper_us(&self) -> i64 {
        match self.ceiling_us {
            Some(ceiling_us) => AUDIO_DELAY_FEEDBACK_MAX_US.min(ceiling_us),
            None => AUDIO_DELAY_FEEDBACK_MAX_US,
        }
    }
}

impl Default for AudioDelayFeedback {
    fn default() -> Self {
        Self::new()
    }
}
