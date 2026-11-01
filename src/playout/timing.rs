//! 音声の再生 (鳴らす時刻) の観測値
//!
//! 受信した音声の「鳴るはずの時刻 (再生予定時刻)」「到着した時刻」「実際に鳴り始める
//! 時刻」を記録し、予定に対する余裕 (間に合ったか) と、鳴らなかった量を出す。
//!
//! 音声の語尾が聞こえないとき、原因が「再生予定に間に合わなかったこと」なのか、それとも
//! 別の段 (受信・復号・音声出力) なのかを数値で切り分けるために使う。累積の数だけでは
//! 「何マイクロ秒分の音が鳴らなかったか」が分からないため、長さも数える。
//!
//! - 時刻はすべて呼び出し側が引数で渡す。値は
//!   [`AudioPlayoutScheduler`](crate::playout::scheduler::AudioPlayoutScheduler) が決めた
//!   時刻と同じ軸のマイクロ秒である
//! - 分布 (p50 / p95 / max) は直近の窓 ([`AUDIO_PLAYOUT_TIMING_WINDOW_US`]) の値から求める
//! - 鳴らなかった量は購読の開始 ([`AudioPlayoutTimingStats::reset`]) からの累積である。
//!   理由ごとに数え、その和は合計に一致する
//! - 出力の穴 (補間で埋まらなかった区間) とバッファ境界の段差も累積で数える。穴は
//!   [`AudioPlayoutTimingStats::record_gap`] に「補間で埋まらなかった長さ」と「予約できな
//!   かった長さ」を渡し、出力側が実際に描画した値を測れる場合は
//!   [`AudioPlayoutTimingStats::record_rendered_gap`] と
//!   [`AudioPlayoutTimingStats::record_rendered_boundary`] を使う
//! - 停止時に予約済みの音を鳴らし切るまでの残りは
//!   [`AudioPlayoutTimingStats::pending_us`] と [`AudioPlayoutTimingStats::pending_end_us`]
//!   で読める
//! - 表示のための整形 (UTC の ISO 8601 など) は持たない。記録の時刻は呼び出し側が渡した
//!   軸のままであり、ログの整形は呼び出し側 (example) の責務である

use alloc::vec::Vec;

use crate::playout::scheduler::{AudioPlayoutBasis, AudioPlayoutPlay};
use crate::playout::timeline::percentile_index;

/// 分布を求める直近の窓 (マイクロ秒)
pub const AUDIO_PLAYOUT_TIMING_WINDOW_US: i64 = 10_000_000;

/// 残す直近の「鳴らさなかった音」の数
pub const MAX_RECENT_AUDIO_MISSES: usize = 30;

/// 鳴らさなかった理由
///
/// - 並べすぎて捨てた ([`AudioMissReason::Backlog`]。再生が追いついていない)
/// - relay の cache から追いつく途中で鳴らさなかった ([`AudioMissReason::CatchUp`]。意図的な
///   もの)
/// - 鳴らす準備 (詰め・補間・予約) の途中で失敗した ([`AudioMissReason::Error`])
/// - 予約したまま再生を止めた ([`AudioMissReason::Stopped`]。予約済みの音が切り捨てられた)
///
/// 鳴り遅れを理由に捨てることはない。音がまだ鳴っている間は遅れたまま鳴らし続け、音が
/// 途切れたときだけ到着基準へ並べ直す
/// ([`AudioPlayoutScheduler`](crate::playout::scheduler::AudioPlayoutScheduler::schedule) を
/// 参照)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioMissReason {
    /// 並べすぎて捨てた (再生が追いついていない)
    Backlog,
    /// relay の cache から追いつく途中で鳴らさなかった (意図的なもの)
    CatchUp,
    /// 鳴らす準備 (詰め・補間・予約) の途中で失敗した
    Error,
    /// 予約したまま再生を止めた (予約済みの音が切り捨てられた)
    Stopped,
}

/// 理由ごとの、鳴らさなかった音の数と長さの累積
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioMissTotal {
    /// 鳴らさなかった音の数
    pub count: u64,
    /// 鳴らさなかった音の長さの合計 (マイクロ秒)
    pub duration_us: i64,
}

/// 鳴らさなかった理由ごとの累積
///
/// 和は [`AudioPlayoutTimingSnapshot::missed_frames`] と
/// [`AudioPlayoutTimingSnapshot::missed_us`] に一致する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioMissTotals {
    /// 並べすぎて捨てた分
    pub backlog: AudioMissTotal,
    /// relay の cache から追いつく途中で鳴らさなかった分
    pub catch_up: AudioMissTotal,
    /// 鳴らす準備の途中で失敗した分
    pub error: AudioMissTotal,
    /// 予約したまま再生を止めた分
    pub stopped: AudioMissTotal,
}

impl AudioMissTotals {
    /// 理由を指定して読む
    pub fn get(&self, reason: AudioMissReason) -> AudioMissTotal {
        match reason {
            AudioMissReason::Backlog => self.backlog,
            AudioMissReason::CatchUp => self.catch_up,
            AudioMissReason::Error => self.error,
            AudioMissReason::Stopped => self.stopped,
        }
    }

    /// 理由を指定して書き換える (記録の内部で使う)
    fn get_mut(&mut self, reason: AudioMissReason) -> &mut AudioMissTotal {
        match reason {
            AudioMissReason::Backlog => &mut self.backlog,
            AudioMissReason::CatchUp => &mut self.catch_up,
            AudioMissReason::Error => &mut self.error,
            AudioMissReason::Stopped => &mut self.stopped,
        }
    }
}

/// 鳴らさなかった 1 件の記録 (時刻は [`AudioPlayoutScheduler`] と同じ軸のマイクロ秒)
///
/// [`AudioPlayoutTimingStats::record_miss`] へ渡す。
///
/// [`AudioPlayoutScheduler`]: crate::playout::scheduler::AudioPlayoutScheduler
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPlayoutMiss {
    /// 鳴らさなかった時刻
    pub at_us: i64,
    /// 鳴らさなかった理由
    pub reason: AudioMissReason,
    /// 鳴らさなかった音の長さ (マイクロ秒)
    pub duration_us: i64,
    /// 再生予定時刻。予定が無ければ None
    pub target_us: Option<i64>,
    /// 到着した時刻。分からなければ None
    pub arrival_us: Option<i64>,
}

/// 補間で埋まらなかった隙間 1 件の記録
///
/// [`AudioPlayoutTimingStats::record_gap`] へ渡す。`unrecoverable_us` は
/// `uncovered_us` に含まれる長さである。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioPlayoutGap {
    /// 補間で埋まらなかった長さ (マイクロ秒)。0 以下は記録しない
    pub uncovered_us: i64,
    /// そのうち、予約できる最も早い時刻より前だった長さ (マイクロ秒)
    pub unrecoverable_us: i64,
}

/// 直近に鳴らさなかった音 1 件の記録
///
/// [`AudioPlayoutTimingSnapshot::recent_misses`] に古い順で残る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioMissEvent {
    /// 鳴らさなかった時刻 (記録に渡した軸のマイクロ秒)
    ///
    /// 壁時計へ換算するのは呼び出し側の責務である (このモジュールは時刻の軸を知らない)。
    pub at_us: i64,
    /// 鳴らさなかった理由
    pub reason: AudioMissReason,
    /// 鳴らさなかった音の長さ (マイクロ秒)
    pub duration_us: i64,
    /// そのときの、予定に対する余裕 (マイクロ秒)。予定が無ければ None
    pub slack_us: Option<i64>,
}

/// 分布の要約 (マイクロ秒)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimingSummary {
    /// 中央値
    pub p50: i64,
    /// 95 パーセンタイル
    pub p95: i64,
    /// 最大値
    pub max: i64,
}

/// 値の分布を p50 / p95 / max に要約する
///
/// 百分位は nearest-rank 法 (昇順に並べて `ceil(p * n)` 番目) で求める。値が無ければ None。
/// 渡した値は並べ替えない (呼び出し側が窓の値を持ち続けるため)。
pub fn summarize_timings(values: &[i64]) -> Option<TimingSummary> {
    if values.is_empty() {
        return None;
    }
    // 昇順に並べた複製から分位点を取る。渡した値は並べ替えない
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let length = sorted.len();
    // 百分位の添字の求め方 (nearest-rank 法) は映像の表示の遅れと同じにする
    Some(TimingSummary {
        p50: sorted[percentile_index(length, 500)],
        p95: sorted[percentile_index(length, 950)],
        max: sorted[percentile_index(length, 1_000)],
    })
}

/// 音声の再生の観測値
///
/// [`AudioPlayoutTimingStats::snapshot`] が返す。時刻はすべて記録に渡した軸のマイクロ秒で
/// あり、分布は直近の窓 ([`AUDIO_PLAYOUT_TIMING_WINDOW_US`]) の値から求める。何も記録して
/// いないときの観測値は `Default` で作れる (すべて None、0、空になる)。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioPlayoutTimingSnapshot {
    /// 直近に鳴らすと決めた音の再生予定時刻 (マイクロ秒)。予定を決められなければ None
    pub last_target_us: Option<i64>,
    /// 直近に鳴らすと決めた音の到着時刻 (マイクロ秒)
    pub last_arrival_us: Option<i64>,
    /// 直近に鳴らすと決めた音が鳴り始める時刻 (マイクロ秒)
    pub last_start_us: Option<i64>,
    /// 直近に鳴らすと決めた音の、予定に対する余裕 (マイクロ秒。負なら予定を過ぎて届いた)
    pub last_slack_us: Option<i64>,
    /// 直近に鳴らすと決めた音の、到着から鳴り始めるまでの時間 (マイクロ秒)
    pub last_start_delay_us: Option<i64>,
    /// 直近に鳴らすと決めた音の、予定からどれだけ過ぎて鳴るか (マイクロ秒)
    pub last_lateness_us: Option<i64>,
    /// 予定に対する余裕 (予定 - 到着) の分布 (直近の窓、マイクロ秒)
    ///
    /// 負なら、届いた時点ですでに予定を過ぎている (間に合っていない)。
    pub slack_us: Option<TimingSummary>,
    /// 到着から鳴り始めるまでの時間の分布 (直近の窓、マイクロ秒)
    pub start_delay_us: Option<TimingSummary>,
    /// 予定からどれだけ過ぎて鳴るかの分布 (直近の窓、マイクロ秒)。0 なら予定どおり
    pub lateness_us: Option<TimingSummary>,
    /// 鳴らすと決めた音の数 (累積。到着基準で鳴らした音を含む)
    pub played_frames: u64,
    /// 鳴らすと決めた音の長さの合計 (マイクロ秒、累積。詰めた分を引いた後)
    pub played_us: i64,
    /// 到着基準の計画で鳴らした音の数 (累積)
    ///
    /// 到着基準になるのは、時間軸の再生予定時刻を使えなかった音 (基準がまだ無い、基準が
    /// ずれている、目標から離れすぎて基準を取り直した) だけではない。予定を決められても
    /// 使わなかった音 (呼び出し側が `enforce_target` を false にしたとき。配信が終わった
    /// あとに保持している音を吐き出すときなど) も到着基準で鳴らす。したがって、この数だけ
    /// では「時間軸が目標を決められていない」と読めない。予定を決められた音が 1 つも無ければ
    /// [`AudioPlayoutTimingSnapshot::slack_us`] と
    /// [`AudioPlayoutTimingSnapshot::lateness_us`] のどちらも None になるため、時間軸が予定を
    /// 決められているかはその 2 つで読む。
    pub arrival_planned_frames: u64,
    /// 到着基準の計画も持たないまま鳴らした音の数 (累積。通常は 0)
    pub unplanned_frames: u64,
    /// 鳴らさなかった音の数 (累積)
    pub missed_frames: u64,
    /// 鳴らさなかった音の長さの合計 (マイクロ秒、累積)
    pub missed_us: i64,
    /// 鳴らさなかった理由ごとの数と長さ (累積)
    pub missed_by_reason: AudioMissTotals,
    /// 直近に鳴らさなかった音 (古い順、最大 [`MAX_RECENT_AUDIO_MISSES`] 件)
    pub recent_misses: Vec<AudioMissEvent>,
    /// 補間で埋まらなかった区間の回数 (累積)
    ///
    /// 呼び出し側が、隙間の全長から実際に補間で埋めた長さを引いた残りを
    /// [`AudioPlayoutTimingStats::record_gap`] で数える。出力が FIFO の場合、隙間を積まない
    /// と後続の音がその分だけ早く鳴るため、隙間は必ず材料化する。この数だけでは
    /// 「どれだけ音が欠けたか」は分からないため、長さも数える。moqt-js の `gapCount`
    /// に対応する
    pub gaps: u64,
    /// 補間で埋まらなかった区間の長さの合計 (マイクロ秒、累積)
    pub gap_total_us: i64,
    /// 補間で埋まらなかった区間の長さの最大 (マイクロ秒)
    pub gap_max_us: i64,
    /// 未補間区間のうち、予約できなかった区間の回数 (累積)
    ///
    /// 現行の出力 (raw_player へ書き込む経路) では、材料化した隙間を補間で埋められなかった
    /// 長さを [`AudioPlayoutTimingStats::record_gap`] が数える。moqt-js の
    /// `unrecoverableGapCount` に対応し、過去の計測との互換のために残す
    pub unrecoverable_gaps: u64,
    /// 未補間区間のうち、予約できなかった長さの合計 (マイクロ秒、累積)
    pub unrecoverable_gap_total_us: i64,
    /// 出力の切り替え位置の段差が大きかった回数 (累積)
    ///
    /// 呼び出し側が実際に書き込んだ PCM の境界で測る。moqt-js の
    /// `boundaryStepCount` に対応する
    pub boundary_step_count: u64,
    /// バッファ境界の段差の最大 (PCM の振幅。0〜1)
    pub boundary_step_max: f32,
    /// 出力が枯れた回数 (累積)
    ///
    /// 呼び出し側が枯れが明けたときに 1 回だけ
    /// [`AudioPlayoutTimingStats::record_underrun`] で数える。アンダーランは事後には埋められ
    /// ないため、再生予算を増やす根拠として読む。
    pub underruns: u64,
    /// 出力が枯れた長さの合計 (マイクロ秒、累積)
    pub underrun_total_us: i64,
    /// 出力が枯れた長さの最大 (マイクロ秒)
    pub underrun_max_us: i64,
    /// 時間圧縮と補間を使えないサンプルレートで、鳴らす組み立てへ渡した音の数 (累積)
    ///
    /// 4 kHz へ間引くフィルタの表 (8 kHz / 16 kHz / 32 kHz / 48 kHz) に無いサンプルレート
    /// では、ピッチの周期を求められないため時間圧縮も補間も働かない。44.1 kHz は解析用の
    /// 信号だけを 48 kHz の格子へ換算して周期を求めるため使える。落ちずに「操作しない」を
    /// 返すので、この数が増えていることで気付く
    /// ([`is_time_stretch_available`](crate::playout::stretch::is_time_stretch_available))。
    /// 0 なら使えている。受信側の待ち行列 (example の jitter buffer) の溢れで捨てられた音は、
    /// この数える位置へ届かないため含まない
    pub stretch_unavailable_frames: u64,
}

/// 記録した時刻の順に並んだ値の列 (マイクロ秒)
///
/// 記録の時刻は単調に増えるため、窓より古い値は先頭にある。先頭から捨てるときに配列を
/// 詰め直さないよう、読み始めの位置を進め、半分を超えたらまとめて詰める。
#[derive(Debug, Default)]
struct TimedValues {
    /// 記録した時刻 (古い順)
    times_us: Vec<i64>,
    /// 記録した値 (古い順)
    values_us: Vec<i64>,
    /// 読み始めの位置
    head: usize,
}

impl TimedValues {
    /// 値を記録する
    fn push(&mut self, at_us: i64, value_us: i64) {
        self.times_us.push(at_us);
        self.values_us.push(value_us);
    }

    /// `at_us` が `min_at_us` より前の値を捨てる
    fn prune(&mut self, min_at_us: i64) {
        while self.head < self.times_us.len() && self.times_us[self.head] < min_at_us {
            self.head += 1;
        }
        if self.head > 0 && self.head * 2 >= self.times_us.len() {
            self.times_us.drain(..self.head);
            self.values_us.drain(..self.head);
            self.head = 0;
        }
    }

    /// 窓の中の値 (古い順)
    fn current(&self) -> &[i64] {
        &self.values_us[self.head..]
    }

    /// 記録をすべて捨てる
    fn clear(&mut self) {
        self.times_us.clear();
        self.values_us.clear();
        self.head = 0;
    }
}

/// 直近に鳴らすと決めた音の値 (観測値へ出す形)
#[derive(Debug, Clone, Copy)]
struct LastPlayRecord {
    /// 再生予定時刻。予定が無ければ None
    target_us: Option<i64>,
    /// 到着した時刻
    arrival_us: i64,
    /// 鳴り始める時刻
    start_us: i64,
    /// 予定に対する余裕。予定が無ければ None
    slack_us: Option<i64>,
    /// 到着から鳴り始めるまでの時間
    start_delay_us: i64,
    /// 予定からどれだけ過ぎて鳴るか。予定が無ければ None
    lateness_us: Option<i64>,
}

/// まだ鳴り終わっていない音の区間 (マイクロ秒)
#[derive(Debug, Clone, Copy)]
struct PendingSound {
    /// 鳴り始める時刻
    start_us: i64,
    /// 鳴り終わる時刻
    end_us: i64,
}

/// 音声の再生の観測を記録し、観測値を求める
///
/// トラックごとに 1 つ持つ。[`AudioPlayoutScheduler`] が鳴らすと決めた音を
/// [`AudioPlayoutTimingStats::record_play`] で、鳴らさなかった音を
/// [`AudioPlayoutTimingStats::record_miss`] で、再生を止めた分を
/// [`AudioPlayoutTimingStats::record_stopped`] で記録し、観測値は
/// [`AudioPlayoutTimingStats::snapshot`] で読む。
///
/// [`AudioPlayoutScheduler`]: crate::playout::scheduler::AudioPlayoutScheduler
#[derive(Debug)]
pub struct AudioPlayoutTimingStats {
    /// 分布を求める直近の窓 (マイクロ秒)
    window_us: i64,
    /// 予定に対する余裕 (予定 - 到着)
    slacks_us: TimedValues,
    /// 到着から鳴り始めるまでの時間
    start_delays_us: TimedValues,
    /// 予定からどれだけ過ぎて鳴るか
    latenesses_us: TimedValues,
    /// 直近に鳴らすと決めた音の値
    last_play: Option<LastPlayRecord>,
    /// まだ鳴り終わっていない音の区間。鳴らさずに止めた分を数えるために持つ
    /// (鳴り始めていても終わっていない区間を含む。[`AudioPlayoutTimingStats::record_stopped`]
    /// が残りの長さを数える)
    pending: Vec<PendingSound>,
    /// 鳴らすと決めた音の数
    played_frames: u64,
    /// 鳴らすと決めた音の長さの合計
    played_us: i64,
    /// 到着基準の計画で鳴らした音の数
    arrival_planned_frames: u64,
    /// 到着基準の計画も持たないまま鳴らした音の数
    unplanned_frames: u64,
    /// 鳴らさなかった音の数
    missed_frames: u64,
    /// 鳴らさなかった音の長さの合計
    missed_us: i64,
    /// 鳴らさなかった理由ごとの数と長さ
    missed_totals: AudioMissTotals,
    /// 直近に鳴らさなかった音 (古い順)
    recent_misses: Vec<AudioMissEvent>,
    /// 隙間を材料化した回数
    gaps: u64,
    /// 材料化した隙間の長さの合計
    gap_total_us: i64,
    /// 材料化した隙間の長さの最大
    gap_max_us: i64,
    /// 予約できなかった未補間区間の回数
    unrecoverable_gaps: u64,
    /// 予約できなかった未補間区間の長さの合計
    unrecoverable_gap_total_us: i64,
    /// 出力の切り替え位置の段差が大きかった回数
    boundary_step_count: u64,
    /// バッファ境界の段差の最大
    boundary_step_max: f32,
    /// 出力が枯れた回数
    underruns: u64,
    /// 出力が枯れた長さの合計
    underrun_total_us: i64,
    /// 出力が枯れた長さの最大
    underrun_max_us: i64,
    /// 時間圧縮と補間を使えないサンプルレートで受け取った音の数
    stretch_unavailable_frames: u64,
}

impl AudioPlayoutTimingStats {
    /// 既定の窓 ([`AUDIO_PLAYOUT_TIMING_WINDOW_US`]) で作る
    pub fn new() -> Self {
        Self::with_window_us(AUDIO_PLAYOUT_TIMING_WINDOW_US)
    }

    /// 窓を指定して作る
    ///
    /// `window_us` は分布を求める直近の窓 (マイクロ秒) である。0 以上を渡すこと。
    pub fn with_window_us(window_us: i64) -> Self {
        Self {
            window_us,
            slacks_us: TimedValues::default(),
            start_delays_us: TimedValues::default(),
            latenesses_us: TimedValues::default(),
            last_play: None,
            pending: Vec::new(),
            played_frames: 0,
            played_us: 0,
            arrival_planned_frames: 0,
            unplanned_frames: 0,
            missed_frames: 0,
            missed_us: 0,
            missed_totals: AudioMissTotals::default(),
            recent_misses: Vec::new(),
            gaps: 0,
            gap_total_us: 0,
            gap_max_us: 0,
            unrecoverable_gaps: 0,
            unrecoverable_gap_total_us: 0,
            boundary_step_count: 0,
            boundary_step_max: 0.0,
            underruns: 0,
            underrun_total_us: 0,
            underrun_max_us: 0,
            stretch_unavailable_frames: 0,
        }
    }

    /// 鳴らすと決めた音を記録する
    ///
    /// 余裕と遅れは、時間軸が決めた再生予定時刻との差である。到着基準で鳴らした音でも、
    /// 時間軸が予定を決めていればその予定との差を記録する (音が予定よりどれだけ遅れて
    /// 鳴っているかを読むため)。到着から鳴り始めるまでの時間は、予定の有無に関係なく
    /// 記録する。
    /// 鳴らした音の実績を 1 つ記録する
    ///
    /// `play` は [`crate::playout::scheduler::AudioPlayoutScheduler::last_play`] の値である。
    /// 実際に鳴る長さ ([`AudioPlayoutPlay::played_us`]) を確定した後に記録すること
    /// (scheduler の `confirm_stretch` / `confirm_expansion` / `confirm_playout_end` を
    /// すべて返した後に `last_play` を読む)。記録した長さは、`pending_us` /
    /// `pending_end_us` と、停止時に切った長さ (`record_stopped`) の元になる。
    pub fn record_play(&mut self, play: AudioPlayoutPlay) {
        self.prune_pending(play.arrival_us);
        let start_delay_us = play.start_at_us.saturating_sub(play.arrival_us);
        let slack_us = play
            .target_start_us
            .map(|target_us| target_us.saturating_sub(play.arrival_us));
        // 前の音の終わりへ揃えるスナップで、音が予定より最大 0.5 ms 早く鳴ることがある。
        // scheduler の遅れ (AudioPlayoutScheduler::lateness_us) と同じく 0 で丸める
        let lateness_us = play
            .target_start_us
            .map(|target_us| play.start_at_us.saturating_sub(target_us).max(0));
        self.start_delays_us.push(play.arrival_us, start_delay_us);
        match play.basis {
            AudioPlayoutBasis::Arrival => {
                // 到着基準の計画に載せた音である (時間軸が予定を決めていても使っていない)
                self.arrival_planned_frames += 1;
            }
            AudioPlayoutBasis::Timestamp => {
                if play.target_start_us.is_none() {
                    // 到着基準の計画も持たない音。呼び出し側が計画を渡していない取りこぼしで
                    // あり、通常は 0 になる
                    self.unplanned_frames += 1;
                }
            }
        }
        if let (Some(slack_us), Some(lateness_us)) = (slack_us, lateness_us) {
            self.slacks_us.push(play.arrival_us, slack_us);
            self.latenesses_us.push(play.arrival_us, lateness_us);
        }
        self.last_play = Some(LastPlayRecord {
            target_us: play.target_start_us,
            arrival_us: play.arrival_us,
            start_us: play.start_at_us,
            slack_us,
            start_delay_us,
            lateness_us,
        });
        self.played_frames += 1;
        let played_us = play.played_us.max(0);
        self.played_us = self.played_us.saturating_add(played_us);
        self.pending.push(PendingSound {
            start_us: play.start_at_us,
            end_us: play.start_at_us.saturating_add(played_us),
        });
    }

    /// 鳴らさなかった音を記録する
    ///
    /// 音が捨てられた時刻で数える。予定と到着の両方が分かるときだけ、捨てた時点の余裕を
    /// 残す。
    pub fn record_miss(&mut self, miss: AudioPlayoutMiss) {
        self.prune_pending(miss.at_us);
        let slack_us = match (miss.target_us, miss.arrival_us) {
            (Some(target_us), Some(arrival_us)) => Some(target_us.saturating_sub(arrival_us)),
            _ => None,
        };
        self.add_miss(miss.at_us, miss.reason, miss.duration_us, slack_us);
    }

    /// 鳴らさずに再生を止めた分を記録する
    ///
    /// 予約済みでまだ鳴り始めていない音は、音声の出力を閉じる (再生の停止、購読の解除) と
    /// 鳴らないまま切り捨てられる。ここで数えないと、この分はどの統計にも現れない。既に
    /// 鳴り始めている音は、残りの長さだけを数える。
    pub fn record_stopped(&mut self, now_us: i64) {
        self.prune_pending(now_us);
        // 数えながら一覧を書き換えられないため、いったん取り出す
        let pending = core::mem::take(&mut self.pending);
        for sound in &pending {
            let remaining_us = sound.end_us.saturating_sub(sound.start_us.max(now_us));
            if remaining_us <= 0 {
                continue;
            }
            self.add_miss(now_us, AudioMissReason::Stopped, remaining_us, None);
        }
    }

    /// 観測値を求める
    ///
    /// 窓は `now_us` から遡る。窓の外に出た記録は分布から落とすが、累積の数と長さと直近の
    /// 値は残る。
    pub fn snapshot(&mut self, now_us: i64) -> AudioPlayoutTimingSnapshot {
        self.prune(now_us);
        let mut snapshot = AudioPlayoutTimingSnapshot::default();
        if let Some(play) = &self.last_play {
            snapshot.last_target_us = play.target_us;
            snapshot.last_arrival_us = Some(play.arrival_us);
            snapshot.last_start_us = Some(play.start_us);
            snapshot.last_slack_us = play.slack_us;
            snapshot.last_start_delay_us = Some(play.start_delay_us);
            snapshot.last_lateness_us = play.lateness_us;
        }
        snapshot.slack_us = summarize_timings(self.slacks_us.current());
        snapshot.start_delay_us = summarize_timings(self.start_delays_us.current());
        snapshot.lateness_us = summarize_timings(self.latenesses_us.current());
        snapshot.played_frames = self.played_frames;
        snapshot.played_us = self.played_us;
        snapshot.arrival_planned_frames = self.arrival_planned_frames;
        snapshot.unplanned_frames = self.unplanned_frames;
        snapshot.missed_frames = self.missed_frames;
        snapshot.missed_us = self.missed_us;
        snapshot.missed_by_reason = self.missed_totals;
        snapshot.recent_misses = self.recent_misses.clone();
        snapshot.gaps = self.gaps;
        snapshot.gap_total_us = self.gap_total_us;
        snapshot.gap_max_us = self.gap_max_us;
        snapshot.unrecoverable_gaps = self.unrecoverable_gaps;
        snapshot.unrecoverable_gap_total_us = self.unrecoverable_gap_total_us;
        snapshot.boundary_step_count = self.boundary_step_count;
        snapshot.boundary_step_max = self.boundary_step_max;
        snapshot.underruns = self.underruns;
        snapshot.underrun_total_us = self.underrun_total_us;
        snapshot.underrun_max_us = self.underrun_max_us;
        snapshot.stretch_unavailable_frames = self.stretch_unavailable_frames;
        snapshot
    }

    /// 時間圧縮と補間を使えないサンプルレートの音を 1 つ数える
    ///
    /// 4 kHz へ間引くフィルタの表に無いサンプルレート (96 kHz など) では、ピッチの
    /// 周期を求められないため時間圧縮も補間も働かない。44.1 kHz は解析用の信号だけを
    /// 48 kHz の格子へ換算して周期を求めるため使える。落ちずに「操作しない」を返すので、
    /// この数が増えていることで気付く。鳴らす側が、復号した音を鳴らす組み立てへ渡す時点で
    /// [`is_time_stretch_available`](crate::playout::stretch::is_time_stretch_available) を
    /// 確かめて呼ぶ。数えるだけで鳴らし方は変えない
    pub fn record_stretch_unavailable(&mut self) {
        self.stretch_unavailable_frames += 1;
    }

    /// 出力の穴として残した区間を 1 つ記録する
    ///
    /// 呼び出し側が、隙間の全長から実際に補間で埋めた長さを引いた残りを渡す。予約できる
    /// 最も早い時刻より前だった長さは `unrecoverable_us` として別に数える
    /// (`unrecoverable_us` は `uncovered_us` に含まれる)。0 以下の長さは数えない。
    /// moqt-js の `recordGap({ uncoveredMs, unrecoverableMs })` に対応する。購読の開始
    /// ([`AudioPlayoutTimingStats::reset`]) からの累積である。
    pub fn record_gap(&mut self, gap: AudioPlayoutGap) {
        let uncovered_us = gap.uncovered_us.max(0);
        if uncovered_us == 0 {
            return;
        }
        self.gaps += 1;
        self.gap_total_us = self.gap_total_us.saturating_add(uncovered_us);
        self.gap_max_us = self.gap_max_us.max(uncovered_us);
        let unrecoverable_us = uncovered_us.min(gap.unrecoverable_us.max(0));
        if unrecoverable_us > 0 {
            self.unrecoverable_gaps += 1;
            self.unrecoverable_gap_total_us = self
                .unrecoverable_gap_total_us
                .saturating_add(unrecoverable_us);
        }
    }

    /// 出力側が実際に描画した未補間区間を反映する
    ///
    /// 補間を予約した長さではなく、出力が実際に描画した未補間の長さと回数を渡す。
    /// moqt-rs の現行の出力は `raw_player` へ書き込む前の隙間の材料化であり、この計器は
    /// 呼び出し側が実際に書き込んだ PCM を測ったときに使う。負の長さは無視する。
    /// moqt-js の `recordRenderedGap` に対応する。
    pub fn record_rendered_gap(&mut self, uncovered_us: i64, count: u64, maximum_us: i64) {
        if uncovered_us < 0 || maximum_us < 0 {
            return;
        }
        self.gaps = self.gaps.saturating_add(count);
        self.gap_total_us = self.gap_total_us.saturating_add(uncovered_us);
        self.gap_max_us = self.gap_max_us.max(maximum_us);
        self.unrecoverable_gaps = self.unrecoverable_gaps.saturating_add(count);
        self.unrecoverable_gap_total_us =
            self.unrecoverable_gap_total_us.saturating_add(uncovered_us);
    }

    /// 出力側が描画したバッファ境界の段差を反映する
    ///
    /// 段差の最大 (PCM の振幅。0〜1) と、記録した回数を渡す。負の値は無視する。
    /// moqt-js の `recordRenderedBoundary` に対応する。
    pub fn record_rendered_boundary(&mut self, maximum: f32, count: u64) {
        if !maximum.is_finite() || maximum < 0.0 {
            return;
        }
        self.boundary_step_max = self.boundary_step_max.max(maximum);
        self.boundary_step_count = self.boundary_step_count.saturating_add(count);
    }

    /// 最後に鳴り終わる予約の時刻 (マイクロ秒)。鳴り終わっていない予約が無ければ None
    ///
    /// 停止時に「あとどれだけ待てば予約済みが鳴り終わるか」を決めるのに使う。moqt-js の
    /// `pendingEndMs` に対応する。
    pub fn pending_end_us(&mut self, now_us: i64) -> Option<i64> {
        self.prune_pending(now_us);
        self.pending.iter().map(|sound| sound.end_us).max()
    }

    /// 予約済みでまだ鳴り終わっていない長さ (マイクロ秒)
    ///
    /// 停止した時点で切ることになる量である。moqt-js の `pendingMs` に対応する。
    pub fn pending_us(&mut self, now_us: i64) -> i64 {
        self.prune_pending(now_us);
        self.pending.iter().fold(0i64, |total_us, sound| {
            total_us.saturating_add(
                sound
                    .end_us
                    .saturating_sub(sound.start_us.max(now_us))
                    .max(0),
            )
        })
    }

    /// 出力が枯れた区間を 1 つ記録する
    ///
    /// 呼び出し側が枯れが明けたときに 1 回だけ、枯れていた長さ (`underrun_us`) で呼ぶ。
    /// 0 以下の長さは枯れていないため数えない。アンダーランは事後には埋められないため、
    /// 長さは再生予算を増やす根拠として読む。購読の開始
    /// ([`AudioPlayoutTimingStats::reset`]) からの累積である。
    pub fn record_underrun(&mut self, underrun_us: i64) {
        let underrun_us = underrun_us.max(0);
        if underrun_us == 0 {
            return;
        }
        self.underruns += 1;
        self.underrun_total_us = self.underrun_total_us.saturating_add(underrun_us);
        self.underrun_max_us = self.underrun_max_us.max(underrun_us);
    }

    /// 記録をすべて捨てて初期状態に戻す (購読のやり直し)
    pub fn reset(&mut self) {
        self.slacks_us.clear();
        self.start_delays_us.clear();
        self.latenesses_us.clear();
        self.last_play = None;
        self.pending.clear();
        self.played_frames = 0;
        self.played_us = 0;
        self.arrival_planned_frames = 0;
        self.unplanned_frames = 0;
        self.missed_frames = 0;
        self.missed_us = 0;
        self.missed_totals = AudioMissTotals::default();
        self.recent_misses.clear();
        self.gaps = 0;
        self.gap_total_us = 0;
        self.gap_max_us = 0;
        self.unrecoverable_gaps = 0;
        self.unrecoverable_gap_total_us = 0;
        self.boundary_step_count = 0;
        self.boundary_step_max = 0.0;
        self.underruns = 0;
        self.underrun_total_us = 0;
        self.underrun_max_us = 0;
        self.stretch_unavailable_frames = 0;
    }

    /// 窓より古い記録を分布から落とす
    fn prune(&mut self, now_us: i64) {
        let min_at_us = now_us.saturating_sub(self.window_us);
        self.slacks_us.prune(min_at_us);
        self.start_delays_us.prune(min_at_us);
        self.latenesses_us.prune(min_at_us);
    }

    /// 鳴り終わった予約を落とす
    fn prune_pending(&mut self, now_us: i64) {
        self.pending.retain(|sound| sound.end_us > now_us);
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
        self.missed_us = self.missed_us.saturating_add(duration_us);
        let total = self.missed_totals.get_mut(reason);
        total.count += 1;
        total.duration_us = total.duration_us.saturating_add(duration_us);
        self.recent_misses.push(AudioMissEvent {
            at_us,
            reason,
            duration_us,
            slack_us,
        });
        // 上限を超えたら古い方から捨てる
        if self.recent_misses.len() > MAX_RECENT_AUDIO_MISSES {
            self.recent_misses.remove(0);
        }
    }
}

impl Default for AudioPlayoutTimingStats {
    fn default() -> Self {
        Self::new()
    }
}
