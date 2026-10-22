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
//! - 表示のための整形 (UTC の ISO 8601 など) は持たない。記録の時刻は呼び出し側が渡した
//!   軸のままであり、ログの整形は呼び出し側 (example) の責務である

use alloc::vec::Vec;

use crate::playout::scheduler::{AudioPlayoutBasis, AudioPlayoutPlay};
use crate::playout::timeline::percentile_index;

/// 分布を求める直近の窓 (マイクロ秒)
pub const AUDIO_PLAYOUT_TIMING_WINDOW_US: i64 = 10_000_000;

/// 残す直近の「鳴らさなかった音」の数
pub const MAX_RECENT_AUDIO_MISSES: usize = 30;

/// 目標遅延を閉ループで決めるときに読む短い窓 (マイクロ秒)
///
/// [`AudioPlayoutTimingStats::audio_delay_feedback`] が使う。表示用の分布
/// ([`AUDIO_PLAYOUT_TIMING_WINDOW_US`]) は、目標を増やした結果が現れるまでに数秒かかる
/// ため、制御には短い窓を使う。
pub const AUDIO_DELAY_FEEDBACK_WINDOW_US: i64 = 1_000_000;

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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
    /// 隙間を材料化した回数 (累積)
    ///
    /// 呼び出し側が隙間の音を積むたびに
    /// [`AudioPlayoutTimingStats::record_gap`] で数える。出力が FIFO の場合、隙間を積まないと
    /// 後続の音がその分だけ早く鳴るため、隙間は必ず材料化する。この数だけでは「隙間が
    /// 埋まったか」は分からないため、長さも数える。
    pub gaps: u64,
    /// 材料化した隙間の長さの合計 (マイクロ秒、累積)
    pub gap_total_us: i64,
    /// 材料化した隙間の長さの最大 (マイクロ秒)
    pub gap_max_us: i64,
    /// 出力が枯れた回数 (累積)
    ///
    /// 呼び出し側が枯れが明けたときに 1 回だけ
    /// [`AudioPlayoutTimingStats::record_underrun`] で数える。アンダーランは事後には埋められ
    /// ないため、閉ループの目標遅延を上げる根拠になる。
    pub underruns: u64,
    /// 出力が枯れた長さの合計 (マイクロ秒、累積)
    pub underrun_total_us: i64,
    /// 出力が枯れた長さの最大 (マイクロ秒)
    pub underrun_max_us: i64,
    /// 時間圧縮と補間を使えないサンプルレートで、鳴らす組み立てへ渡した音の数 (累積)
    ///
    /// 4 kHz へ間引くフィルタの表 (8 kHz / 16 kHz / 32 kHz / 48 kHz) に無いサンプルレート
    /// (44.1 kHz の AAC など) では、ピッチの周期を求められないため時間圧縮も補間も働かない。
    /// 落ちずに「操作しない」を返すので、この数が増えていることで気付く
    /// ([`is_time_stretch_available`](crate::playout::stretch::is_time_stretch_available))。
    /// 0 なら使えている。受信側の待ち行列 (example の jitter buffer) の溢れで捨てられた音は、
    /// この数える位置へ届かないため含まない
    pub stretch_unavailable_frames: u64,
}

/// 目標遅延を閉ループで決めるための観測
///
/// [`AudioPlayoutTimingStats::audio_delay_feedback`] が返す。分布は短い窓
/// ([`AUDIO_DELAY_FEEDBACK_WINDOW_US`]) の値から求める。捨てた量と、材料化した隙間の累積は
/// 購読の開始からの値であり、差を取るのは呼び出し側の仕事である。
///
/// 圧縮 (時間圧縮) の増分と、出力が枯れた長さと、揺らぎだけから求めた目標遅延は、この計器
/// では分からない。圧縮と枯れは購読側 (再生の経路) が、揺らぎだけの目標は時間軸
/// ([`PlayoutTimeline::observe_audio_playout`](crate::playout::timeline::PlayoutTimeline::observe_audio_playout))
/// が埋める。どれも既定は 0 であり、埋めなければ「圧縮なし」「枯れなし」「経路は 100 ms
/// 以内」とみなされる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioDelayFeedbackObservation {
    /// 観測した時刻 (マイクロ秒)
    pub at_us: i64,
    /// 予定からどれだけ過ぎて鳴るかの分布 (短い窓、マイクロ秒)
    pub lateness_us: Option<TimingSummary>,
    /// 到着から鳴り始めるまでの時間の分布 (短い窓、マイクロ秒)
    pub start_delay_us: Option<TimingSummary>,
    /// 予定に対する余裕の分布 (短い窓、マイクロ秒)
    pub slack_us: Option<TimingSummary>,
    /// 累積で並べすぎて捨てた音の数
    pub backlog_misses: u64,
    /// 累積で並べすぎて捨てた音の長さ (マイクロ秒)
    pub backlog_us: i64,
    /// 前回の観測から、出力が枯れた合計 (ミリ秒)
    ///
    /// 購読側が、出力が枯れていた長さの増分を渡す。アンダーランは事後には埋められないため、
    /// 閉ループはこれを根拠に目標遅延を上げる以外に減らせない。観測は音ごとに来るが、目標を
    /// 動かすのは毎秒 1 回であるため、制御までの間ぶんを足し合わせて渡す。分からなければ 0
    /// (枯れていない) とみなす。購読のやり直しで累積が 0 に戻っても負の差を作らないよう、
    /// 累積ではなく増分を渡す。
    pub underrun_ms: i64,
    /// 前の観測から増えた圧縮 (時間圧縮 = 粗さ) の長さ (マイクロ秒、増分)
    ///
    /// 購読側が
    /// [`AudioPlayoutScheduler::compressed_us`](crate::playout::scheduler::AudioPlayoutScheduler::compressed_us)
    /// の増分を渡す。観測は音ごとに来るが、目標を動かすのは毎秒 1 回であるため、閉ループが
    /// 制御までの間を足し合わせて速さ (マイクロ秒 / 秒) を求める。累積ではなく増分を渡すのは、
    /// 購読のやり直しで累積が 0 に戻っても負の差を作らないためである。分からなければ 0
    /// (圧縮なし) とみなす。
    pub compression_us: i64,
    /// 揺らぎだけから求めた目標遅延 (マイクロ秒)
    ///
    /// 上限を [`crate::playout::feedback::AUDIO_DELAY_FEEDBACK_MAX_TARGET_US`] へ戻してよいかの
    /// 判断に使う。「圧縮が 0」だけでは、揺らぎが 100 ms を超えている経路で上限を戻した直後に
    /// 圧縮が再発する (往復になる)。この値が 100 ms 以下であることを戻す条件に加える。
    pub jitter_target_us: i64,
    /// 購読の開始からの、材料化した隙間の数 (累積)
    ///
    /// 計器が [`AudioPlayoutTimingStats::record_gap`] で数えた累積である。閉ループは前回の
    /// 観測との差を取る。moqt-js の `AudioDelayFeedbackObservation.gapCount` に対応する。
    pub gap_count: u64,
    /// 購読の開始からの、材料化した隙間の長さの合計 (マイクロ秒、累積)
    ///
    /// moqt-js の `AudioDelayFeedbackObservation.gapMs` に対応する。隙間は事後には埋められ
    /// ないため、閉ループはこれを根拠に目標遅延を上げる以外に減らせない。
    pub gap_us: i64,
    /// 到着基準の再生に別の下限がある場合、その値から穴を吸収する分を増やす (マイクロ秒)
    ///
    /// moqt-js の `arrivalDelayFloorMs` に対応する。音声だけを購読するときに到着基準の再生へ
    /// 足す下限 ([`crate::playout::timeline::TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_FLOOR_US`]) が
    /// これにあたる。購読側が音声だけのときに埋める。None なら閉ループの目標だけを実際の
    /// 深さとみなす。
    pub arrival_delay_floor_us: Option<i64>,
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

    /// `at_us` が `since_us` より後の値 (古い順)
    ///
    /// 窓 (`prune`) より短い区間の分布を取り直すために使う。記録の時刻は単調に増えるため、
    /// 条件を満たす値は末尾に連続して並ぶ。
    fn since(&self, since_us: i64) -> &[i64] {
        let mut start = self.head;
        while start < self.times_us.len() && self.times_us[start] <= since_us {
            start += 1;
        }
        &self.values_us[start..]
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
    pub fn record_play(&mut self, play: AudioPlayoutPlay) {
        self.prune_pending(play.arrival_us);
        let start_delay_us = play.start_at_us.saturating_sub(play.arrival_us);
        let slack_us = play
            .target_start_us
            .map(|target_us| target_us.saturating_sub(play.arrival_us));
        let lateness_us = play
            .target_start_us
            .map(|target_us| play.start_at_us.saturating_sub(target_us));
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

    /// 目標遅延を閉ループで決めるための観測を求める
    ///
    /// 表示用の分布 ([`AudioPlayoutTimingStats::snapshot`]) は直近の窓
    /// ([`AUDIO_PLAYOUT_TIMING_WINDOW_US`]) である。目標を増やした結果がそこへ現れるまで
    /// には数秒かかるため、制御には短い窓 ([`AUDIO_DELAY_FEEDBACK_WINDOW_US`]) の値を使う。
    /// 捨てた量と隙間の累積は購読の開始からの値であり、差を取るのは呼び出し側の仕事である。
    ///
    /// 圧縮の増分と、揺らぎだけから求めた目標遅延は、この計器では分からないため 0 にする。
    /// 呼び出し側 (購読側と時間軸) が埋めること
    /// ([`AudioDelayFeedbackObservation::compression_us`] と
    /// [`AudioDelayFeedbackObservation::jitter_target_us`] を参照)。
    pub fn audio_delay_feedback(&mut self, now_us: i64) -> AudioDelayFeedbackObservation {
        self.audio_delay_feedback_since(
            now_us,
            now_us.saturating_sub(AUDIO_DELAY_FEEDBACK_WINDOW_US),
        )
    }

    /// 目標遅延を閉ループで決めるための観測を求める (制御の開始時刻を指定する)
    ///
    /// 表示用の分布 ([`AudioPlayoutTimingStats::snapshot`]) と累積は残したまま、制御へ渡す
    /// 分布だけを `since_us` より後の記録に限る。音声の再生を取り直したとき、再開前の遅れを
    /// 新しい再生の制御へ混ぜないために使う (moqt-js の
    /// `audioDelayFeedback(nowMs, sinceMs)` と同じ)。`since_us` は再開した時刻 (観測の
    /// `at_us` と同じ軸のマイクロ秒) を渡す。
    ///
    /// 窓の先頭 (`now_us - AUDIO_DELAY_FEEDBACK_WINDOW_US`) より `since_us` が古いときは、
    /// 窓の先頭から使う (分布が短い窓より広がることはない)。
    ///
    /// # 引数
    ///
    /// - `now_us`: 求める時刻 (マイクロ秒)
    /// - `since_us`: 制御へ渡す分布の開始時刻。この時刻以前の記録は分布に使わない
    pub fn audio_delay_feedback_since(
        &mut self,
        now_us: i64,
        since_us: i64,
    ) -> AudioDelayFeedbackObservation {
        // 表示用の分布と同じ長さまでを残し、制御にはその中でも短い窓を使う
        self.prune(now_us);
        // 表示用の分布と累積は残す一方、再開前の遅れを新しい再生の制御へ混ぜない
        let control_min_us = now_us
            .saturating_sub(AUDIO_DELAY_FEEDBACK_WINDOW_US)
            .max(since_us);
        AudioDelayFeedbackObservation {
            at_us: now_us,
            lateness_us: summarize_timings(self.latenesses_us.since(control_min_us)),
            start_delay_us: summarize_timings(self.start_delays_us.since(control_min_us)),
            slack_us: summarize_timings(self.slacks_us.since(control_min_us)),
            backlog_misses: self.missed_totals.backlog.count,
            backlog_us: self.missed_totals.backlog.duration_us,
            underrun_ms: 0,
            compression_us: 0,
            jitter_target_us: 0,
            // 材料化した隙間を、閉ループが目標を増やす根拠として渡す
            gap_count: self.gaps,
            gap_us: self.gap_total_us,
            // 到着基準の再生に別の下限がある場合、購読側が上書きする。計器では分からない
            arrival_delay_floor_us: None,
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
        snapshot.underruns = self.underruns;
        snapshot.underrun_total_us = self.underrun_total_us;
        snapshot.underrun_max_us = self.underrun_max_us;
        snapshot.stretch_unavailable_frames = self.stretch_unavailable_frames;
        snapshot
    }

    /// 時間圧縮と補間を使えないサンプルレートの音を 1 つ数える
    ///
    /// 4 kHz へ間引くフィルタの表に無いサンプルレート (44.1 kHz の AAC など) では、ピッチの
    /// 周期を求められないため時間圧縮も補間も働かない。落ちずに「操作しない」を返すので、
    /// この数が増えていることで気付く。鳴らす側が、復号した音を鳴らす組み立てへ渡す時点で
    /// [`is_time_stretch_available`](crate::playout::stretch::is_time_stretch_available) を
    /// 確かめて呼ぶ。数えるだけで鳴らし方は変えない
    pub fn record_stretch_unavailable(&mut self) {
        self.stretch_unavailable_frames += 1;
    }

    /// 材料化した隙間を 1 つ記録する
    ///
    /// 呼び出し側が隙間の音を積むたびに、その全長 (`gap_us`) で呼ぶ。0 以下の長さは
    /// 材料化していないため数えない。隙間の長さは回数だけでは分からないため、合計と最大も
    /// 残す。購読の開始 ([`AudioPlayoutTimingStats::reset`]) からの累積である。
    pub fn record_gap(&mut self, gap_us: i64) {
        let gap_us = gap_us.max(0);
        if gap_us == 0 {
            return;
        }
        self.gaps += 1;
        self.gap_total_us = self.gap_total_us.saturating_add(gap_us);
        self.gap_max_us = self.gap_max_us.max(gap_us);
    }

    /// 出力が枯れた区間を 1 つ記録する
    ///
    /// 呼び出し側が枯れが明けたときに 1 回だけ、枯れていた長さ (`underrun_us`) で呼ぶ。
    /// 0 以下の長さは枯れていないため数えない。アンダーランは事後には埋められないため、
    /// 長さは閉ループが目標遅延を上げる根拠になる
    /// ([`AudioDelayFeedbackObservation::underrun_ms`])。購読の開始
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
