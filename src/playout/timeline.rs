//! 受信した音声と映像を鳴らす・表示する時刻を決める共通の時間軸
//!
//! 送信側の TIMESTAMP (LOC の TIMESTAMP) と受信側の時計を結び、トラックごとの遅れを
//! 足した「鳴らす時刻・表示する時刻」を返す。表示時刻は
//! 「TIMESTAMP + 基準の遅れ + 表示の遅れ」であり、音声と映像で同じ式を使うため、
//! A/V の同期はトラックごとの遅れの差だけで決まる。時計もデバイスも触らず、時刻は
//! すべて呼び出し側が引数で渡す。
//!
//! - 基準の遅れ: トラックごとの (復号の出力の時刻 − TIMESTAMP) の直近 10 秒の最小値。
//!   受信側と送信側の時計のずれと、経路と復号の最小遅延を含む。遅れは到着ではなく
//!   復号の出力の時刻で測る (表示できる時刻には復号の時間も含まれるため)
//! - 表示の遅れ (音声): [`crate::playout::delay`] の目標遅延
//! - 表示の遅れ (映像): (遅れ − 基準の遅れ) の揺らぎの百分位。表示時刻の後に届く
//!   フレームが 1 秒に [`TIMELINE_LATE_FRAMES_PER_SECOND`] 枚までになる値である
//! - A/V 同期: 表示の遅れの差が [`TIMELINE_SYNC_MIN_DELTA_US`] を超えたら、先行する側へ
//!   足して合わせる (観測のたび)。足した分は毎秒
//!   [`TIMELINE_DELAY_DECAY_US_PER_SECOND`] までで戻す。相手側へ移す分は
//!   [`TIMELINE_MAX_COMPENSATED_DIFFERENCE_US`] までにする
//! - 基準を共有しない判定: 2 つのトラックの基準の差が、表示の遅れの上限から求まる
//!   閾値を超えたときと、差が [`TIMELINE_BASE_DRIFT_US`] を超えて動き続けている
//!   (TIMESTAMP が壁時計からずれている) ときである。共有しない間は、その時点までに
//!   足した分を戻す
//! - 基準を共有できない側が音声のときは、同期をやめずに映像だけを音声の到着基準の遅れ
//!   ([`audio_arrival_delay_us`]) へ合わせる。合わせないと相対関係を見る相手がいなくなり、
//!   音声と映像が別々の基準で並ぶ
//! - いったん共有をやめたら [`TIMELINE_BASE_UNSHARED_HOLD_US`] の間は戻さない。閾値は
//!   表示の遅れで動くため、差が変わらなくても共有と解除を往復し得る
//! - 遅延の内訳 (基準の遅れ・揺らぎ・足した分・共有できているかとその理由・差の動き) は
//!   [`PlayoutTimeline::delay_breakdown`] が返す

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::playout::delay::AudioDelayManager;

/// 基準の遅れと揺らぎを求める直近の窓 (マイクロ秒)
pub const TIMELINE_WINDOW_US: i64 = 10_000_000;

/// TIMESTAMP と壁時計の関係が基準からこれ以上離れたら基準を取り直す (マイクロ秒)
///
/// 配信元の切り替えや、配信が長く止まったあとの再開で基準が古くなるのを避ける。
pub const TIMELINE_DISCONTINUITY_US: i64 = 2_000_000;

/// 表示時刻の後に届くことを許すフレームの数 (1 秒あたり)
///
/// 表示時刻の後に届いたフレームはその表示周期に描けず、止まりになる。許す数を
/// 1 秒あたりで決めることで、配信 fps が高いほど多くの遅れを許さないようにする。
pub const TIMELINE_LATE_FRAMES_PER_SECOND: i64 = 1;

/// 映像の表示の遅れの目標にする揺らぎの百分位の下限 (1000 分率)
///
/// 配信 fps が低いと 1 秒に 1 枚は 5% を超えるため、95% のフレームは表示時刻までに
/// 届く長さを保つ。経路のまれな大きな遅れまで吸収しようとすると、常に大きく遅れて
/// 表示することになるため、百分位は 100% にしない。
pub const TIMELINE_MIN_PERCENTILE_MILLI: i64 = 950;

/// 表示の遅れの上限の既定値 (ミリ秒)
pub const TIMELINE_MAX_PRESENTATION_DELAY_MS: i64 = 500;

/// 映像の表示待ちのキューの上限のうち、揺らぎで一時的に増える分として空けておく枚数
///
/// 表示の遅れは (キューの上限 − この枚数) 枚分のフレーム間隔までに抑える。フレーム
/// 間隔が短いほど長く待てない。
pub const TIMELINE_QUEUE_HEADROOM_FRAMES: usize = 4;

/// 2 つのトラックの基準の遅れの差の閾値の下限 (ミリ秒)
///
/// 閾値が 0 に近いと、同期の制御と解除を往復して基準の学習と並べ直しが繰り返し
/// 起きるため、下限を置く。
pub const TIMELINE_MIN_BASE_DIFFERENCE_MS: i64 = 100;

/// フレーム間隔の中央値を求めるために保持する TIMESTAMP の差の数
pub const TIMELINE_FRAME_INTERVAL_SAMPLES: usize = 32;

/// 実績を A/V のずれの推定に使う期間 (マイクロ秒)
pub const TIMELINE_SKEW_WINDOW_US: i64 = 1_000_000;

/// 追いつき中かを確かめる間隔 (マイクロ秒)
pub const TIMELINE_CATCH_UP_CHECK_INTERVAL_US: i64 = 250_000;

/// 追いつき中とみなす、間隔の間の基準の遅れの下がり幅 (マイクロ秒)
///
/// 実時間の 1.1 倍の速さで追いつく場合も 25 ms 下がる。live に追いついた後の経路の
/// 最小の遅延の変化 (数ミリ秒) より十分大きくする。
pub const TIMELINE_CATCH_UP_MIN_BASE_DROP_US: i64 = 20_000;

/// 目標が下がったときに表示の遅れを下げる速さ (マイクロ秒/秒)
pub const TIMELINE_DELAY_DECAY_US_PER_SECOND: i64 = 20_000;

/// A/V 同期の制御で許す、表示時刻の差の下限 (マイクロ秒)
///
/// この不感帯の中では遅延を変えないため、先行する側は最大この値だけ先行できる。
pub const TIMELINE_SYNC_MIN_DELTA_US: i64 = 30_000;

/// 音声の再生の遅れの下限 (マイクロ秒)
///
/// 到着基準で並べるときも、共有の時間軸で並べるときも、これより短くは並べない。
pub const TIMELINE_AUDIO_DELAY_FLOOR_US: i64 = 80_000;

/// 音声を到着基準で並べるときの再生の遅れの上限 (マイクロ秒)
///
/// 学習した遅れは経路の揺らぎを吸収するために必要である。ただし上限を超える分は使わない。
/// TIMESTAMP が壁時計からずれているトラックでは、ずれそのものを揺らぎとして学習して
/// しまうためである。呼び出し側 (音声を鳴らす時刻を決めるとき) と時間軸 (映像を
/// 合わせるとき) の両方が [`audio_arrival_delay_us`] を使う。値がずれると A/V の
/// 合わせ先がずれる。
pub const TIMELINE_ARRIVAL_DELAY_US: i64 = 100_000;

/// 2 つのトラックの基準の差が、この幅を超えて動いたら時計がずれているとみなす (マイクロ秒)
///
/// 差が大きいだけなら「経路と復号の遅い側」であり、同期の制御で合わせられる。しかし差が
/// 動き続ける場合は経路の遅れではなく、片方の TIMESTAMP が壁時計からずれていくことを
/// 意味する。ずれ続ける差を合わせると、もう片方の表示の遅れが上限まで伸びて戻せない。
///
/// 実時間に対する時計の進み方の違いは 500 ppm (毎秒 0.5 ms) 未満であり、経路と復号の
/// 最小遅延の差も毎秒ミリ秒の桁でしか動かない。したがってこの幅 (5 秒で 50 ms = 毎秒
/// 10 ms) を超える動きは時計のずれとみなしてよい。
pub const TIMELINE_BASE_DRIFT_US: i64 = 50_000;

/// 基準の差の動きを見る窓 (マイクロ秒)
pub const TIMELINE_BASE_DRIFT_WINDOW_US: i64 = 5_000_000;

/// A/V 同期で合わせる、2 つのトラックの基準の差の上限 (マイクロ秒)
///
/// 同じ publisher・同じ経路の 2 つのトラックで、経路と復号の「最小」遅延がこれ以上違う
/// ことはない。これを超える差は TIMESTAMP の時計のずれであることが多く、合わせても
/// 実際のずれは減らないまま、相手側の表示の遅れだけが伸びる。時計のずれの証拠を
/// 見たかどうかには依らず、常に掛ける。
pub const TIMELINE_MAX_COMPENSATED_DIFFERENCE_US: i64 = 100_000;

/// いったん基準を共有しないと決めた後、判定を戻さない時間 (マイクロ秒)
///
/// 閾値は「表示の遅れの上限 − そのトラックの遅延」で決まるため、jitter buffer の目標
/// 遅延が段差で動くたびに閾値も動く。往復のたびに、足した分を戻して (間に合わない
/// フレームを捨てる) すぐ足し直す (表示が待って止まる) ことになるため、しばらくは
/// 戻さない。
pub const TIMELINE_BASE_UNSHARED_HOLD_US: i64 = 30_000_000;

/// 基準の差を記録する間隔 (マイクロ秒)
const TIMELINE_BASE_DIFFERENCE_SAMPLE_INTERVAL_US: i64 = 250_000;

/// 基準の差の動きを見るときに使う、直近の基準を求める窓 (マイクロ秒)
///
/// 窓全体 ([`TIMELINE_WINDOW_US`]) の最小値は、TIMESTAMP が壁時計から遅れていくときも
/// 窓が埋まるまで動かない。短い窓で取り直すことで、合わせる側の遅れが上限へ伸びる前に
/// 動きを見つける。短くするほど経路の揺らぎの影響を受けやすいため、映像の到着が
/// まとまっていても最小値が動かない長さにする。
const TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US: i64 = 2_000_000;

/// 到着基準で並べるときの再生の遅れを求める (マイクロ秒)
///
/// 学習した遅れを下限 [`TIMELINE_AUDIO_DELAY_FLOOR_US`] と上限
/// [`TIMELINE_ARRIVAL_DELAY_US`] の間に切る。呼び出し側 (音声を鳴らす時刻を決めるとき) と
/// 時間軸 (基準を共有できないときに映像を合わせるとき) の両方がこれを使う。値がずれると
/// A/V の合わせ先がずれる。
pub fn audio_arrival_delay_us(learned_delay_us: i64) -> i64 {
    learned_delay_us.clamp(TIMELINE_AUDIO_DELAY_FLOOR_US, TIMELINE_ARRIVAL_DELAY_US)
}

/// 窓に保持する観測の上限
///
/// 観測の頻度は呼び出し側の供給量で決まる。同じ時刻の観測が繰り返し届いても
/// メモリが増え続けないよう、件数でも上限を置く。
const TIMELINE_WINDOW_MAX_ENTRIES: usize = 4096;

/// 表示時刻を決める相手
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    /// 音声
    Audio,
    /// 映像
    Video,
}

impl Track {
    /// 配列の添字
    fn index(self) -> usize {
        match self {
            Self::Audio => 0,
            Self::Video => 1,
        }
    }
}

/// 2 つのトラックで基準を共有できているか、できていない理由
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsharedReason {
    /// 共有している (どちらも TIMESTAMP を使えている)
    None,
    /// 基準がまだ足りない (どちらかを観測していない)
    Unobserved,
    /// 基準の差が表示の遅れの上限を超えている (上限では合わせられない)
    Difference,
    /// 基準の差が動き続けている (TIMESTAMP が壁時計からずれている)
    Drift,
    /// 直前にやめた判定を保持している (閾値が動いても往復させない)
    Hold,
}

/// トラックごとの表示時刻の内訳 (遅延の解析に使う)
///
/// 表示時刻 = TIMESTAMP + 基準の遅れ + 表示の遅れ であり、表示の遅れは jitter buffer の
/// 遅延と `targetLatency` の大きい方に、同期の制御が足した分を加えて上限で切った値である。
/// どこで遅れが生じているかを分けるために、この 3 つを別々に出す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackBreakdown {
    /// 基準の遅れ (マイクロ秒)。送受信の時計のずれと、経路と復号の最小遅延。未観測なら None
    pub base_delay_us: Option<i64>,
    /// jitter buffer の遅延 (マイクロ秒)。自分の揺らぎから求めた値。未観測なら None
    pub jitter_delay_us: Option<i64>,
    /// 同期の制御が足した分 (マイクロ秒)。0 以上
    pub sync_extra_delay_us: i64,
    /// TIMESTAMP から表示時刻までの差 (マイクロ秒)。上限で切った後であり、そのトラックの
    /// TIMESTAMP を使わないとき (未観測、または基準がずれている) は None
    pub presentation_delay_us: Option<i64>,
    /// 表示の遅れの上限 (マイクロ秒)。切り下げが起きているかはこの値との比較で分かる
    pub presentation_delay_cap_us: i64,
}

/// 音声と映像の遅延の内訳 (遅延の解析に使う)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayBreakdown {
    /// 音声の内訳
    pub audio: TrackBreakdown,
    /// 映像の内訳
    pub video: TrackBreakdown,
    /// 基準の差「音声 − 映像」(マイクロ秒)。どちらかを観測していなければ None
    pub base_difference_us: Option<i64>,
    /// 2 つのトラックで基準を共有しているか
    pub sharing_bases: bool,
    /// 共有できていない理由
    pub unshared_reason: UnsharedReason,
    /// 直近の基準の差の動き (マイクロ秒/秒)。まだ履歴が無ければ None
    pub base_drift_us_per_second: Option<i64>,
    /// 時計のずれとみなす、基準の差の動きの幅 (マイクロ秒)
    pub base_drift_limit_us: i64,
    /// jitter buffer の遅延を切り下げる上限 (マイクロ秒)
    pub presentation_delay_cap_us: i64,
}

/// 時間軸の設定
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineConfig {
    /// 映像の表示待ちのキューの上限 (枚)
    pub video_queue_limit: usize,
    /// 表示の遅れの上限 (ミリ秒)
    pub max_presentation_delay_ms: i64,
    /// MSF の `targetLatency` (ミリ秒、draft-ietf-moq-msf-01 §5.2.8 (Target latency))。
    /// 0 のときは下限にしない
    pub target_latency_ms: i64,
}

impl Default for TimelineConfig {
    fn default() -> Self {
        Self {
            video_queue_limit: 24,
            max_presentation_delay_ms: TIMELINE_MAX_PRESENTATION_DELAY_MS,
            target_latency_ms: 0,
        }
    }
}

/// 昇順に並べた値の nearest-rank 法の百分位の添字を求める
///
/// `numerator_milli` は百分位を 1000 分率で表した値 (950 なら 0.95)。添字は
/// `ceil(numerator_milli * 個数 / 1000) - 1` を 0 以上 `count` 未満に切った値である。
fn percentile_index(count: usize, numerator_milli: i64) -> usize {
    if count == 0 {
        return 0;
    }
    let scaled = numerator_milli.saturating_mul(count as i64);
    let ceil = (scaled + 999) / 1_000;
    (ceil.max(1) as usize - 1).min(count - 1)
}

/// 映像の表示の遅れの目標にする百分位 (1000 分率)
///
/// `max(0.95, 1 - フレーム間隔 (ミリ秒) / 1000)` を整数で求める。フレーム間隔が
/// まだ分からないときは下限 (0.95) を使う。
fn video_percentile_milli(frame_interval_us: Option<i64>) -> i64 {
    let numerator_milli = match frame_interval_us {
        Some(frame_interval_us) => {
            1_000 - (frame_interval_us / 1_000) * TIMELINE_LATE_FRAMES_PER_SECOND
        }
        None => 0,
    };
    numerator_milli.clamp(TIMELINE_MIN_PERCENTILE_MILLI, 1_000)
}

/// 表示の遅れの上限 (マイクロ秒)
///
/// 「表示の遅れの上限 (設定値)」と「表示待ちのキューが吸収できる長さ
/// ((キューの上限 − 余裕) 枚分のフレーム間隔)」の小さい方である。フレーム間隔が
/// まだ分からないときは設定値だけを使い、計算結果が負のときは 0 とする。
fn presentation_cap_us(config: &TimelineConfig, frame_interval_us: Option<i64>) -> i64 {
    let max_us = config.max_presentation_delay_ms.saturating_mul(1_000);
    let queue_us = match frame_interval_us {
        Some(frame_interval_us) => (config
            .video_queue_limit
            .saturating_sub(TIMELINE_QUEUE_HEADROOM_FRAMES)
            as i64)
            .max(0)
            .saturating_mul(frame_interval_us),
        None => max_us,
    };
    max_us.min(queue_us)
}

/// live に追いつくまでの間かを決める
///
/// [`TIMELINE_CATCH_UP_CHECK_INTERVAL_US`] ごとに基準の遅れの下がり幅を見て、
/// [`TIMELINE_CATCH_UP_MIN_BASE_DROP_US`] より小さければ追いついたとみなす。
/// 一度追いついたら、基準を取り直すまで追いつき中に戻らない。
fn is_catching_up(state: &mut TrackState, now_us: i64, base_us: i64) -> bool {
    if !state.catching_up {
        return false;
    }
    let Some((checkpoint_at_us, checkpoint_base_us)) = state.catch_up_checkpoint else {
        state.catch_up_checkpoint = Some((now_us, base_us));
        return true;
    };
    if now_us.saturating_sub(checkpoint_at_us) < TIMELINE_CATCH_UP_CHECK_INTERVAL_US {
        return true;
    }
    if checkpoint_base_us.saturating_sub(base_us) >= TIMELINE_CATCH_UP_MIN_BASE_DROP_US {
        state.catch_up_checkpoint = Some((now_us, base_us));
        return true;
    }
    state.catching_up = false;
    state.catch_up_checkpoint = None;
    false
}

/// 観測の時刻付きの値の窓 (古い順)
#[derive(Debug, Default)]
struct TimedWindow {
    entries: VecDeque<(i64, i64)>,
}

impl TimedWindow {
    /// 観測を 1 つ足す
    fn push(&mut self, at_us: i64, value_us: i64) {
        self.entries.push_back((at_us, value_us));
        while self.entries.len() > TIMELINE_WINDOW_MAX_ENTRIES {
            self.entries.pop_front();
        }
    }

    /// `oldest_us` より前の観測を捨てる
    fn prune(&mut self, oldest_us: i64) {
        while self
            .entries
            .front()
            .is_some_and(|(at_us, _)| *at_us < oldest_us)
        {
            self.entries.pop_front();
        }
    }

    /// 一番小さい値
    fn min_value(&self) -> Option<i64> {
        self.entries.iter().map(|(_, value_us)| *value_us).min()
    }

    /// 窓の中の最も古い記録 (時刻, 値)。無ければ None
    ///
    /// 値が窓の中でどれだけ動いたかを見るために使う。
    fn oldest(&self) -> Option<(i64, i64)> {
        self.entries.front().copied()
    }

    /// `since_us` 以降の記録の最小値。無ければ None
    ///
    /// 窓より短い区間の最小値を取り直すために使う。窓全体の最小値は、値が単調に動いて
    /// いるときに最も古い観測を指したままになる。
    fn min_after(&self, since_us: i64) -> Option<i64> {
        let mut min: Option<i64> = None;
        for (at_us, value_us) in self.entries.iter().rev() {
            if *at_us < since_us {
                break;
            }
            min = Some(min.map_or(*value_us, |min| min.min(*value_us)));
        }
        min
    }

    /// 値のイテレータ
    fn values(&self) -> impl Iterator<Item = i64> + '_ {
        self.entries.iter().map(|(_, value_us)| *value_us)
    }
}

/// トラック 1 つぶんの状態
#[derive(Debug)]
struct TrackState {
    /// 到着の揺らぎから目標遅延を求める (音声だけが使う)
    delay: AudioDelayManager,
    /// (復号の出力の時刻 − TIMESTAMP) の窓 (直近 10 秒)。基準を求める
    offsets: TimedWindow,
    /// 揺らぎの学習に使う (復号の出力の時刻 − TIMESTAMP) の窓 (映像だけが使う)
    learning_offsets: TimedWindow,
    /// 基準の遅れ (マイクロ秒)
    base_us: Option<i64>,
    /// 自分の揺らぎから求めた表示の遅れ (マイクロ秒)
    own_delay_us: i64,
    /// A/V 同期の制御が足した遅れ (マイクロ秒)。自分の遅れの上に乗る
    sync_extra_us: i64,
    /// 直近の観測の復号の出力の時刻 (マイクロ秒)
    last_arrival_us: Option<i64>,
    /// 直近の観測の TIMESTAMP (マイクロ秒)
    last_timestamp_us: Option<i64>,
    /// 表示の遅れを最後に更新した時刻 (減衰の経過時間を求める)
    last_delay_update_us: i64,
    /// 直近のフレーム間隔 (TIMESTAMP の差、マイクロ秒) の窓
    frame_intervals_us: VecDeque<i64>,
    /// live に追いつくまでの間か
    catching_up: bool,
    /// 追いつき中の確認点 (時刻, 基準)
    catch_up_checkpoint: Option<(i64, i64)>,
}

impl TrackState {
    /// 観測の無い状態で作る
    fn new() -> Self {
        Self {
            delay: AudioDelayManager::new(),
            offsets: TimedWindow::default(),
            learning_offsets: TimedWindow::default(),
            base_us: None,
            own_delay_us: 0,
            sync_extra_us: 0,
            last_arrival_us: None,
            last_timestamp_us: None,
            last_delay_update_us: 0,
            frame_intervals_us: VecDeque::new(),
            catching_up: true,
            catch_up_checkpoint: None,
        }
    }

    /// 直近のフレーム間隔の中央値 (マイクロ秒)。まだ分からなければ None
    fn frame_interval_us(&self) -> Option<i64> {
        if self.frame_intervals_us.is_empty() {
            return None;
        }
        let mut sorted: Vec<i64> = self.frame_intervals_us.iter().copied().collect();
        // 整数なので通常の比較で並べ替える
        sorted.sort_unstable();
        Some(sorted[percentile_index(sorted.len(), 500)])
    }
}

/// 直近に表示すると決めた実績
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PresentationRecord {
    /// 表示した TIMESTAMP (マイクロ秒)
    timestamp_us: i64,
    /// 実際に表示する (鳴らす) 時刻 (受信側の壁時計、マイクロ秒)
    presented_wall_clock_us: i64,
}

/// 音声と映像に共通の再生時刻を決める
///
/// 観測のたびに [`PlayoutTimeline::observe`] を呼び、鳴らす・表示する時刻を
/// [`PlayoutTimeline::present_us`] で求める。A/V 同期の制御も観測のたびに動く。
#[derive(Debug)]
pub struct PlayoutTimeline {
    /// 設定
    config: TimelineConfig,
    /// 音声と映像の状態
    tracks: [TrackState; 2],
    /// 直近に同期の制御を行った時刻 (足した遅延を戻す速さの経過時間を求める)
    last_sync_us: Option<i64>,
    /// 直近に観測した、同期の制御が足していない方の遅れ
    ///
    /// 自分の遅れが下がった分だけ、足した遅延を戻す量を減らすために使う。
    last_own_floor_us: Option<[i64; 2]>,
    /// 基準を共有しないと決めた直近の時刻と、そのときの側。往復を防ぐ
    last_unshared_at_us: Option<i64>,
    /// 基準を共有しないと決めた直近の側
    last_unshared_track: Option<Track>,
    /// 2 つのトラックの基準の差の直近の履歴 (マイクロ秒)。差が動き続けていれば時計のずれ
    base_differences: TimedWindow,
    /// 直前に基準の差を記録した時刻 (マイクロ秒)。まだ記録していなければ None
    last_base_difference_at_us: Option<i64>,
    /// 直前に記録した基準の差 (マイクロ秒)。履歴と同じ求め方であり、動きの今側の値になる
    last_base_difference_value_us: Option<i64>,
    /// 基準を取り直した回数
    generation: u64,
    /// 直近の表示の実績
    presented: [Option<PresentationRecord>; 2],
}

impl PlayoutTimeline {
    /// 既定の設定で作る
    pub fn new() -> Self {
        Self::with_config(TimelineConfig::default())
    }

    /// 設定を決めて作る
    ///
    /// 負のミリ秒の値は 0 として扱う。
    pub fn with_config(mut config: TimelineConfig) -> Self {
        config.max_presentation_delay_ms = config.max_presentation_delay_ms.max(0);
        config.target_latency_ms = config.target_latency_ms.max(0);
        Self {
            config,
            tracks: [TrackState::new(), TrackState::new()],
            last_sync_us: None,
            last_own_floor_us: None,
            last_unshared_at_us: None,
            last_unshared_track: None,
            base_differences: TimedWindow::default(),
            last_base_difference_at_us: None,
            last_base_difference_value_us: None,
            generation: 0,
            presented: [None, None],
        }
    }

    /// 使っている設定
    pub fn config(&self) -> TimelineConfig {
        self.config
    }

    /// 使う `targetLatency` (ミリ秒) を決める
    ///
    /// 同じ render group のトラックは同じ値でなければならないため、音声と映像で
    /// 1 つの値を使う。解決の規則は呼び出し側が持ち、ここへは確定した値だけを渡す。
    /// 値は A/V 同期の基準の遅延になり、2 つのトラックの表示の遅れの下限になる。
    pub fn set_target_latency_ms(&mut self, target_latency_ms: i64) {
        self.config.target_latency_ms = target_latency_ms.max(0);
        // 明示された値は基準を共有するかの閾値にも効くため、保持の起点を取り直す
        self.refresh_unshared_state();
        // 下限は 2 つのトラックの表示の遅れの下限になる。片方だけがこの下限に当たることが
        // あるため、差が開いていれば次の観測を待たずにその場で合わせ直す (戻す向きは毎秒の
        // 速さに限るので、ここでは足す向きだけを直す)
        if let Some(natural_us) = self.sync_natural_presentation_us() {
            self.align_sync_extras(natural_us);
        }
    }

    /// 使っている `targetLatency` (ミリ秒)
    pub fn target_latency_ms(&self) -> i64 {
        self.config.target_latency_ms
    }

    /// 復号の出力を 1 つ記録する
    ///
    /// `wall_clock_us` は復号の出力の時刻 (受信側の壁時計)、`timestamp_us` はその
    /// データの TIMESTAMP (どちらもマイクロ秒)。TIMESTAMP と壁時計の関係が基準から
    /// [`TIMELINE_DISCONTINUITY_US`] 以上離れたときは、配信元の切り替えとみなして
    /// 基準を取り直す (世代を進める)。
    pub fn observe(&mut self, track: Track, wall_clock_us: i64, timestamp_us: i64) {
        let offset_us = wall_clock_us.saturating_sub(timestamp_us);
        if let Some(base_us) = self.tracks[track.index()].base_us
            && offset_us.saturating_sub(base_us).saturating_abs() >= TIMELINE_DISCONTINUITY_US
        {
            self.reset();
        }
        let config = self.config;
        let state = &mut self.tracks[track.index()];

        // フレーム間隔を更新し、揺らぎの学習に使うかを決める
        let mut learns = false;
        if let (Some(last_timestamp_us), Some(last_arrival_us)) =
            (state.last_timestamp_us, state.last_arrival_us)
        {
            let interval_us = timestamp_us.saturating_sub(last_timestamp_us);
            if interval_us > 0 {
                state.frame_intervals_us.push_back(interval_us);
                while state.frame_intervals_us.len() > TIMELINE_FRAME_INTERVAL_SAMPLES {
                    state.frame_intervals_us.pop_front();
                }
            }
            // まとまって届いた観測 (前の観測からフレーム間隔の半分未満) は学習に使わない。
            // フレーム間隔は直近の中央値を使う (まだ 1 つも無ければ今回の差)
            let frame_interval_us = state.frame_interval_us().unwrap_or(interval_us);
            learns = wall_clock_us.saturating_sub(last_arrival_us) >= frame_interval_us / 2;
        }
        state.last_arrival_us = Some(wall_clock_us);
        state.last_timestamp_us = Some(timestamp_us);

        // 基準の遅れ (直近 10 秒の最小値) を更新する
        state.offsets.push(wall_clock_us, offset_us);
        state
            .offsets
            .prune(wall_clock_us.saturating_sub(TIMELINE_WINDOW_US));
        if let Some(min_us) = state.offsets.min_value() {
            state.base_us = Some(min_us);
        }

        match track {
            Track::Audio => {
                // 音声の表示の遅れは目標遅延の学習 (`playout::delay`) から求める
                state.delay.observe(wall_clock_us, timestamp_us);
                state.own_delay_us = state.delay.target_delay_ms().saturating_mul(1_000);
            }
            Track::Video => {
                if let Some(base_us) = state.base_us {
                    // live に追いつくまでに届いたフレームの遅れは経路の揺らぎではない
                    if learns && !is_catching_up(state, wall_clock_us, base_us) {
                        state.learning_offsets.push(wall_clock_us, offset_us);
                    }
                    state
                        .learning_offsets
                        .prune(wall_clock_us.saturating_sub(TIMELINE_WINDOW_US));

                    let cap_us = presentation_cap_us(&config, state.frame_interval_us());
                    let target_us =
                        video_target_us(state, base_us, state.frame_interval_us()).min(cap_us);
                    if target_us >= state.own_delay_us {
                        // まだ学習していない、または目標が上がったときはすぐ反映する
                        state.own_delay_us = target_us;
                    } else {
                        // 目標が下がるときは少しずつ下げる。上限が下がったときは直ちに従う
                        let elapsed_us = wall_clock_us
                            .saturating_sub(state.last_delay_update_us)
                            .max(0);
                        let decay_us = TIMELINE_DELAY_DECAY_US_PER_SECOND
                            .saturating_mul(elapsed_us)
                            / 1_000_000;
                        let decayed_us = state.own_delay_us.saturating_sub(decay_us);
                        state.own_delay_us = target_us.max(decayed_us).min(cap_us);
                    }
                    state.last_delay_update_us = wall_clock_us;
                }
            }
        }

        // 基準の差の動きを見るために記録する (基準がそろってから意味を持つ)
        self.record_base_difference(wall_clock_us);

        // ここまでの更新で表示の遅れが変わったため、A/V 同期の制御をそろえる
        self.update_sync_delays(wall_clock_us);
    }

    /// この TIMESTAMP を鳴らす・表示する時刻 (受信側の壁時計のマイクロ秒)
    ///
    /// 基準がまだ無いときと、そのトラックの TIMESTAMP を使わないとき (基準がずれて
    /// いるとき) は None。None のときは、呼び出し側が届いた順に鳴らす経路へ委ねる。
    pub fn present_us(&self, track: Track, timestamp_us: i64) -> Option<i64> {
        if self.drifted_track() == Some(track) {
            return None;
        }
        let state = &self.tracks[track.index()];
        let base_us = state.base_us?;
        Some(
            timestamp_us
                .saturating_add(base_us)
                .saturating_add(self.delay_us(track)),
        )
    }

    /// トラックの表示に使う遅れ (マイクロ秒)
    ///
    /// 表示時刻から基準の遅れを除いた分であり、自分の揺らぎから求めた遅れと
    /// `targetLatency` の大きい方に、A/V 同期の制御の結果を反映し、表示の遅れの上限で
    /// 切った値である。基準がまだ無いときと、基準がずれているときは None。音声の値は
    /// スケジューラ ([`crate::playout::scheduler::AudioPlayoutInput::presentation_delay_us`])
    /// へそのまま渡せる (観測がまだ無いときは呼び出し側の既定値を使う)。
    pub fn presentation_delay_us(&self, track: Track) -> Option<i64> {
        if self.drifted_track() == Some(track) {
            return None;
        }
        self.tracks[track.index()].base_us?;
        Some(self.delay_us(track))
    }

    /// 自分の揺らぎから求めた表示の遅れ (マイクロ秒)
    ///
    /// 音声は `crate::playout::delay` の目標遅延、映像は揺らぎの百分位である。
    /// `targetLatency` と同期の制御の分を含まない。観測が無いときは 0 になる。
    pub fn learned_delay_us(&self, track: Track) -> i64 {
        self.tracks[track.index()].own_delay_us
    }

    /// 上限に収まらず切り下げた `targetLatency` の分 (マイクロ秒)
    pub fn limited_us(&self) -> i64 {
        self.config
            .target_latency_ms
            .saturating_mul(1_000)
            .saturating_sub(self.presentation_cap_us())
            .max(0)
    }

    /// 表示した実績を記録する
    ///
    /// 呼び出し側が表示時刻を決めて実行したあとに呼ぶ。実績は
    /// [`PlayoutTimeline::skew_us`] の推定に使う。
    pub fn record_presentation(
        &mut self,
        track: Track,
        timestamp_us: i64,
        presented_wall_clock_us: i64,
    ) {
        self.presented[track.index()] = Some(PresentationRecord {
            timestamp_us,
            presented_wall_clock_us,
        });
    }

    /// 実績から求めた A/V のずれ (マイクロ秒)
    ///
    /// 直近に表示した音声と映像それぞれの「表示時刻 − TIMESTAMP」の差を取る。映像の
    /// 表示が音声より遅れていれば正。両方の実績が
    /// [`TIMELINE_SKEW_WINDOW_US`] 以内にあるときだけ値を返す。
    pub fn skew_us(&self) -> Option<i64> {
        let audio = self.presented[Track::Audio.index()]?;
        let video = self.presented[Track::Video.index()]?;
        let now_us = audio
            .presented_wall_clock_us
            .max(video.presented_wall_clock_us);
        if now_us.saturating_sub(audio.presented_wall_clock_us) > TIMELINE_SKEW_WINDOW_US
            || now_us.saturating_sub(video.presented_wall_clock_us) > TIMELINE_SKEW_WINDOW_US
        {
            return None;
        }
        let audio_offset_us = audio
            .presented_wall_clock_us
            .saturating_sub(audio.timestamp_us);
        let video_offset_us = video
            .presented_wall_clock_us
            .saturating_sub(video.timestamp_us);
        Some(video_offset_us.saturating_sub(audio_offset_us))
    }

    /// 2 つのトラックで基準を共有しているか
    ///
    /// 共有できないのは、どちらかをまだ観測していないときと、基準の差が
    /// 「遅い側を待つ」ことで合わせられないときである (差が表示の遅れの上限を超えている、
    /// または差が動き続けている = TIMESTAMP が壁時計からずれている)。理由は
    /// [`PlayoutTimeline::unshared_reason`] と [`PlayoutTimeline::delay_breakdown`] に出る。
    pub fn sharing_bases(&self) -> bool {
        self.unshared_reason() == UnsharedReason::None
    }

    /// 基準を共有できていない理由
    ///
    /// 未観測 (どちらかの基準がまだ無い) を先に見る。差と動きの判定は基準がそろってから
    /// 意味を持つ (差が 0 であるとも、動きが無いとも言えない)。動きは差より先に見る。
    /// 動き続けている差は経路の遅れではなく時計のずれであり、合わせることをやめる原因
    /// そのものであるため、差の大きさより先に知りたい。
    pub fn unshared_reason(&self) -> UnsharedReason {
        if self.tracks[Track::Audio.index()].base_us.is_none()
            || self.tracks[Track::Video.index()].base_us.is_none()
        {
            return UnsharedReason::Unobserved;
        }
        if self.base_difference_drifted() {
            return UnsharedReason::Drift;
        }
        let difference_us = self.current_base_difference_us().unwrap_or(0);
        if difference_us.saturating_abs() > self.base_difference_limit_us() {
            return UnsharedReason::Difference;
        }
        // 直前にやめた判定を保持している (閾値が動いても往復させない)
        if self.held_unshared_track().is_some() {
            return UnsharedReason::Hold;
        }
        UnsharedReason::None
    }

    /// 音声と映像の遅延の内訳 (遅延の解析に使う)
    ///
    /// 「表示の遅れがどこで生じているか」と「2 つのトラックを同じ時計として扱えているか」を
    /// 1 つの値にまとめる。表示時刻そのものは [`PlayoutTimeline::present_us`] が返す。
    pub fn delay_breakdown(&self) -> DelayBreakdown {
        DelayBreakdown {
            audio: self.track_breakdown(Track::Audio),
            video: self.track_breakdown(Track::Video),
            base_difference_us: self.current_base_difference_us(),
            sharing_bases: self.sharing_bases(),
            unshared_reason: self.unshared_reason(),
            base_drift_us_per_second: self.base_drift_us_per_second(),
            base_drift_limit_us: TIMELINE_BASE_DRIFT_US,
            presentation_delay_cap_us: self.presentation_cap_us(),
        }
    }

    /// 音声の jitter buffer の遅延 (マイクロ秒、上限適用後)。まだ観測していなければ None
    ///
    /// 壁時計の TIMESTAMP を持たない音を到着基準で並べるときの再生の遅れに使う。
    /// `targetLatency` と同期の制御による下限は含まない
    /// ([`PlayoutTimeline::presentation_delay_us`] を使う)。
    pub fn playout_delay_us(&self) -> Option<i64> {
        self.tracks[Track::Audio.index()].base_us?;
        Some(
            self.tracks[Track::Audio.index()]
                .own_delay_us
                .min(self.presentation_cap_us()),
        )
    }

    /// 音声を到着基準で並べるときの再生の遅れ (マイクロ秒)。まだ観測していなければ None
    ///
    /// 音声の TIMESTAMP が信用できないとき (基準を共有できないとき) に、映像を合わせる先の
    /// 時刻である。呼び出し側が音声を鳴らすときに使う値と同じ規則で求める
    /// ([`audio_arrival_delay_us`])。値がずれると A/V の合わせ先がずれる。
    pub fn audio_arrival_delay_us(&self) -> Option<i64> {
        Some(audio_arrival_delay_us(self.playout_delay_us()?))
    }

    /// 1 つのトラックの基準と学習と実績を消す (音声の再生を止めたときなど)
    ///
    /// 世代は進めない。世代を進めると、もう片方に既に積んだフレームの表示時刻が
    /// 決められなくなるためである。`targetLatency` は設定の値であり残す。
    pub fn reset_track(&mut self, track: Track) {
        self.tracks[track.index()] = TrackState::new();
        self.presented[track.index()] = None;
        // 同期の制御の状態も、そのトラックの観測が無い状態に戻す。もう片方に足した分は
        // 消したトラックとの差を合わせるためのものなので、足した分をすべて消す
        for state in &mut self.tracks {
            state.sync_extra_us = 0;
        }
        self.last_sync_us = None;
        self.last_own_floor_us = None;
        // 基準の差の履歴も消す (片方の基準が無い状態の差に意味は無い)
        self.clear_base_differences();
    }

    /// 基準と学習をすべて消す (TIMESTAMP の飛び、購読のやり直し)。世代を進める
    pub fn reset(&mut self) {
        self.generation = self.generation.saturating_add(1);
        for track in [Track::Audio, Track::Video] {
            self.tracks[track.index()] = TrackState::new();
            self.presented[track.index()] = None;
        }
        self.last_sync_us = None;
        self.last_own_floor_us = None;
        // 基準の差の履歴も消す (片方の基準が無い状態の差に意味は無い)
        self.clear_base_differences();
    }

    /// 基準を取り直した回数
    ///
    /// 取り直すと、それより前に積んだフレームの表示時刻は新しい基準では決められない
    /// (飛びの分だけ未来になる)。積む側が世代を見て、古いフレームを到着順に扱える
    /// ようにする。
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// トラックの表示の遅れの下限 (マイクロ秒)
    ///
    /// 自分の揺らぎから求めた遅れと `targetLatency` の大きい方である。表示の遅れの
    /// 上限は掛けない。
    fn natural_delay_us(&self, track: Track) -> i64 {
        self.tracks[track.index()]
            .own_delay_us
            .max(self.config.target_latency_ms.saturating_mul(1_000))
    }

    /// 表示に使う遅れ (マイクロ秒)
    ///
    /// 表示時刻から基準の遅れを除いた分であり、自分の揺らぎから求めた遅れと
    /// `targetLatency` の大きい方に、A/V 同期の制御が足した分を加えた値を上限で切った
    /// ものである。
    fn delay_us(&self, track: Track) -> i64 {
        let state = &self.tracks[track.index()];
        self.natural_delay_us(track)
            .saturating_add(state.sync_extra_us)
            .min(self.presentation_cap_us())
    }

    /// 表示の遅れの上限 (マイクロ秒)
    fn presentation_cap_us(&self) -> i64 {
        presentation_cap_us(
            &self.config,
            self.tracks[Track::Video.index()].frame_interval_us(),
        )
    }

    /// A/V 同期の制御に使う、同期が足した分を除いた表示の遅れ (マイクロ秒)
    ///
    /// 基準の遅れと、自分の揺らぎから求めた遅れ (`targetLatency` との大きい方) の和である。
    /// 2 つのトラックのこの値の差が、同期で合わせる対象になる。どちらかが未観測、または
    /// 基準がずれているときは `None` を返す。
    fn sync_natural_presentation_us(&self) -> Option<[i64; 2]> {
        let audio_base_us = self.tracks[Track::Audio.index()].base_us?;
        let video_base_us = self.tracks[Track::Video.index()].base_us?;
        if self.drifted_track().is_some() {
            return None;
        }
        Some([
            audio_base_us.saturating_add(self.natural_delay_us(Track::Audio)),
            video_base_us.saturating_add(self.natural_delay_us(Track::Video)),
        ])
    }

    /// ずれが目標の差を超えていれば、先行する側へ足す遅延を増やして合わせる
    ///
    /// 後行側の表示時刻から、目標の差 ([`PlayoutTimeline::desired_difference_us`]) だけ
    /// 手前へ寄せる。既に足している分は減らさない (減らすのは
    /// [`PlayoutTimeline::decay_sync_extras`] の目標へ戻す処理だけにする)。
    fn align_sync_extras(&mut self, natural_us: [i64; 2]) {
        let audio_us = natural_us[Track::Audio.index()]
            .saturating_add(self.tracks[Track::Audio.index()].sync_extra_us);
        let video_us = natural_us[Track::Video.index()]
            .saturating_add(self.tracks[Track::Video.index()].sync_extra_us);
        let target_us = audio_us
            .max(video_us)
            .saturating_sub(self.desired_difference_us(natural_us));
        if audio_us < target_us {
            self.tracks[Track::Audio.index()].sync_extra_us =
                target_us.saturating_sub(natural_us[Track::Audio.index()]);
        } else if video_us < target_us {
            self.tracks[Track::Video.index()].sync_extra_us =
                target_us.saturating_sub(natural_us[Track::Video.index()]);
        }
    }

    /// 同期の制御で目指す、2 つのトラックの表示時刻の差 (マイクロ秒)
    ///
    /// 不感帯 ([`TIMELINE_SYNC_MIN_DELTA_US`]) に収める。ただし合わせる量は
    /// [`TIMELINE_MAX_COMPENSATED_DIFFERENCE_US`] までにする。それを超える差は経路の
    /// 遅れではなく TIMESTAMP の時計のずれであることが多く、合わせても実際のずれは
    /// 減らないまま相手側の表示の遅れだけが伸びるためである。時計のずれの証拠
    /// ([`PlayoutTimeline::base_difference_drifted`]) を見たかどうかに関わらず常に掛ける。
    /// 差は「足した分を含まない素の値」から決める。今の値から決めると、足すたびに目標が
    /// 上がり続けて上限まで届くまで足してしまう。
    fn desired_difference_us(&self, natural_us: [i64; 2]) -> i64 {
        let difference_us = natural_us[Track::Audio.index()]
            .saturating_sub(natural_us[Track::Video.index()])
            .saturating_abs();
        TIMELINE_SYNC_MIN_DELTA_US
            .max(difference_us.saturating_sub(TIMELINE_MAX_COMPENSATED_DIFFERENCE_US))
    }

    /// 2 つのトラックの遅れが目標の差になるために、先行する側へ足す分 (マイクロ秒)
    ///
    /// 素の値で先行する側 (小さい方) へ、後行側から
    /// [`PlayoutTimeline::desired_difference_us`] だけ手前になるまでの分を足す。戻す向きは
    /// [`PlayoutTimeline::decay_sync_extras`] だけが行う。
    fn target_extra_us(&self, natural_us: [i64; 2]) -> [i64; 2] {
        let desired_us = self.desired_difference_us(natural_us);
        let audio_us = natural_us[Track::Audio.index()];
        let video_us = natural_us[Track::Video.index()];
        if audio_us <= video_us {
            [
                video_us.saturating_sub(desired_us).saturating_sub(audio_us),
                0,
            ]
        } else {
            [
                0,
                audio_us.saturating_sub(desired_us).saturating_sub(video_us),
            ]
        }
    }

    /// A/V 同期の制御を行う (観測のたびに呼ぶ)
    ///
    /// 表示時刻は「基準の遅れ + 表示の遅れ」であり、2 つのトラックの差はこの和の差である。
    /// したがって合わせる量は、同期が足した分を含まない表示の遅れの差そのものであり、
    /// 経路の相対遅延 (直近の観測の差) ではない。直近の観測を使うと、観測のたびに動く
    /// 揺らぎがそのまま制御量に入り、表示時刻の差を合わせられない。
    ///
    /// - 音声の TIMESTAMP が信用できないときは、映像だけを音声の到着基準の遅れへ合わせる
    ///   ([`PlayoutTimeline::update_sync_delays_to_arrival_audio`])
    /// - 基準を共有できないときは、足した分を自分の基準だけの表示時刻へ戻す (合わせる
    ///   相手がいないため)
    /// - ずれが目標の差を超えたら、先行する側へ足して合わせる (即座に行う)
    /// - 足した分は、目標の差から決まる値へ毎秒
    ///   [`TIMELINE_DELAY_DECAY_US_PER_SECOND`] までで戻す。自分の下限が同時に下がって
    ///   いるときは、その分だけ戻す量を減らす
    /// - 戻したあとにもう一度そろえる (片側だけ戻すと、その分だけずれが開く)
    fn update_sync_delays(&mut self, now_us: i64) {
        // 差と動きから共有をやめた側を記録する (観測のたびに判定し直す)
        self.refresh_unshared_state();
        if self.drifted_track() == Some(Track::Audio) {
            // 音声の TIMESTAMP が信用できず、到着基準で鳴っている。基準は共有できないが、
            // 音声の並べ方は分かっているため、映像だけをその時刻へ合わせる
            self.update_sync_delays_to_arrival_audio(now_us);
            return;
        }
        let Some(natural_us) = self.sync_natural_presentation_us() else {
            // 2 つのトラックの基準を共有できない (基準がずれている、またはまだ観測して
            // いない)。合わせる相手がいないため、足した分を自分の基準だけの表示時刻へ
            // 戻す。戻さないと、ずれたトラックへ合わせて足した分がそのまま残り、その分
            // だけ 2 つの表示時刻が離れたままになる
            self.decay_sync_extras(now_us, |_| 0);
            return;
        };

        // 1) ずれを不感帯に収める (先行する側へ足す。即座に行う)
        self.align_sync_extras(natural_us);

        // 2) 足した分を目標へ戻す (毎秒の速さまで。自分の下限が下がった分だけ減らす。
        //    観測の間隔で按分するため、観測が疎でも速さは変わらない)
        let target_extra_us = self.target_extra_us(natural_us);
        self.decay_sync_extras(now_us, |track| target_extra_us[track.index()]);

        // 3) 戻した後のずれをもう一度そろえる (片側だけ戻すと、その分だけずれが開く)
        self.align_sync_extras(natural_us);
    }

    /// 音声が到着基準で鳴っているときの同期の制御 (映像だけを音声へ合わせる)
    ///
    /// 音声の TIMESTAMP が信用できないと 2 つのトラックの基準は共有できず、これまでは
    /// 同期の制御そのものを止めていた。止めると相対関係を見る相手がいなくなり、音声は
    /// 「到着 + 到着基準の再生の遅れ」、映像は自分の jitter buffer の遅延で別々に並ぶ。
    ///
    /// 音声の並べ方は分かっている ([`audio_arrival_delay_us`]) ため、映像の到着からの
    /// 遅れ ([`PlayoutTimeline::natural_delay_known_us`]) をその値へ合わせることはできる。
    /// これで基準を共有できない状態でもリップシンクが取れる。音声と映像の到着が同じ
    /// 時刻であることは前提にする (同じ publisher・同じ経路の 2 つのトラック)。
    ///
    /// - 足すのは映像だけである。音声を遅らせると到着から鳴るまでの時間がその分だけ
    ///   増える (音声の目標は [`TIMELINE_ARRIVAL_DELAY_US`] であり、それ以上は遅らせない)
    /// - 足す量は [`TIMELINE_MAX_COMPENSATED_DIFFERENCE_US`] までにする。映像が音声より
    ///   遅いときは戻さない (音声を遅らせないと合わせられないため、その分は A/V のずれと
    ///   して残す)
    /// - 足すのは即座、戻すのは毎秒 [`TIMELINE_DELAY_DECAY_US_PER_SECOND`] までにする
    fn update_sync_delays_to_arrival_audio(&mut self, now_us: i64) {
        let Some(audio_us) = self.audio_arrival_delay_us() else {
            // 音声の jitter buffer の遅延がまだ決まっていない
            self.decay_sync_extras(now_us, |_| 0);
            return;
        };
        let Some(video_us) = self.natural_delay_known_us(Track::Video) else {
            // 映像の jitter buffer の遅延がまだ決まっていない
            self.decay_sync_extras(now_us, |_| 0);
            return;
        };
        // 目指す差は不感帯にする。ただし合わせる量は上限までにする (足りない分は A/V の
        // ずれとして残す)。差が不感帯に収まる場合も、既存の規則と同じく足さない
        let desired_us = TIMELINE_SYNC_MIN_DELTA_US.max(
            audio_us
                .saturating_sub(video_us)
                .saturating_abs()
                .saturating_sub(TIMELINE_MAX_COMPENSATED_DIFFERENCE_US),
        );
        let target_extra_us = if video_us < audio_us {
            audio_us.saturating_sub(desired_us).saturating_sub(video_us)
        } else {
            0
        };
        // 戻す向きは毎秒の速さまでにする (音声は足さない。足すと到着から鳴るまでが増える)
        self.decay_sync_extras(now_us, |track| {
            if track == Track::Video {
                target_extra_us
            } else {
                0
            }
        });
        // 足す向きは即座に行う (減らすのは decay_sync_extras だけにする)
        let video_with_extra_us =
            video_us.saturating_add(self.tracks[Track::Video.index()].sync_extra_us);
        if video_with_extra_us < audio_us.saturating_sub(desired_us) {
            self.tracks[Track::Video.index()].sync_extra_us =
                audio_us.saturating_sub(desired_us).saturating_sub(video_us);
        }
    }

    /// 足した分を目標へ戻す (毎秒 [`TIMELINE_DELAY_DECAY_US_PER_SECOND`] まで)
    ///
    /// 観測の間隔で按分するため、観測が疎でも速さは変わらない。自分の遅延の下限が同時に
    /// 下がっているときは、その分だけ戻す量を減らす (下限が下がるだけでも表示時刻は前に
    /// 動くため、戻しすぎると不感帯を通り越す)。
    fn decay_sync_extras(&mut self, now_us: i64, target_extra_us: impl Fn(Track) -> i64) {
        let previous_sync_us = self.last_sync_us;
        self.last_sync_us = Some(now_us);
        let elapsed_us = previous_sync_us.map_or(0, |previous_sync_us| {
            now_us.saturating_sub(previous_sync_us).max(0)
        });
        let budget_us = TIMELINE_DELAY_DECAY_US_PER_SECOND.saturating_mul(elapsed_us) / 1_000_000;
        for track in [Track::Audio, Track::Video] {
            let index = track.index();
            // 自分の遅れが下がった分は、戻す量から差し引く (基準の遅れは含めない)
            let own_decrease_us = self.last_own_floor_us.map_or(0, |floor_us| {
                floor_us[index]
                    .saturating_sub(self.natural_delay_us(track))
                    .max(0)
            });
            let allowed_us = budget_us.saturating_sub(own_decrease_us).max(0);
            let excess_us = self.tracks[index]
                .sync_extra_us
                .saturating_sub(target_extra_us(track))
                .max(0);
            self.tracks[index].sync_extra_us = self.tracks[index]
                .sync_extra_us
                .saturating_sub(allowed_us.min(excess_us));
        }
        // 次の制御で「自分の下限が下がった分」を求めるために、今の下限を残す
        self.last_own_floor_us = Some([
            self.natural_delay_us(Track::Audio),
            self.natural_delay_us(Track::Video),
        ]);
    }

    /// 基準を共有できないトラック。無ければ None (共有している)
    ///
    /// 次のどちらかで共有できないと判定する。
    ///
    /// - 2 つのトラックの基準の差が [`PlayoutTimeline::base_difference_limit_us`] を
    ///   超えている。上限で切られる分は合わせられないため、同期の制御では足りない
    ///   (キューが保持する時間は「表示時刻 − 復号の出力時刻」= 2 つのトラックの基準の差 +
    ///   表示の遅れであり、基準の遅れそのものは含まない)
    /// - 基準の差が動き続けている ([`PlayoutTimeline::base_difference_drifted`])。これは
    ///   経路の遅れではなく TIMESTAMP の時計のずれであり、合わせると片側の表示の遅れが
    ///   上限まで伸びる
    ///
    /// 一度やめた側は [`TIMELINE_BASE_UNSHARED_HOLD_US`] の間は保持する
    /// ([`PlayoutTimeline::held_unshared_track`])。大きい側 (遅れて届いている側) が、
    /// TIMESTAMP が壁時計からずれている側である。
    fn drifted_track(&self) -> Option<Track> {
        let difference_us = self.current_base_difference_us()?;
        let track = if difference_us > 0 {
            Track::Audio
        } else {
            Track::Video
        };
        if difference_us.saturating_abs() > self.base_difference_limit_us()
            || self.base_difference_drifted()
        {
            return Some(track);
        }
        self.held_unshared_track()
    }

    /// 直前に共有をやめた側。保持の時間を過ぎていれば None
    ///
    /// 閾値は jitter buffer の目標遅延で動くため、差が変わらなくても共有と解除を
    /// 往復し得る。往復のたびに、足した分を戻して (フレームを捨てる) すぐ足し直す
    /// (表示が止まる) ため、一度やめたらしばらくは戻さない。
    fn held_unshared_track(&self) -> Option<Track> {
        let last_unshared_track = self.last_unshared_track?;
        let last_unshared_at_us = self.last_unshared_at_us?;
        let last_base_difference_at_us = self.last_base_difference_at_us?;
        if last_base_difference_at_us.saturating_sub(last_unshared_at_us)
            >= TIMELINE_BASE_UNSHARED_HOLD_US
        {
            return None;
        }
        Some(last_unshared_track)
    }

    /// 共有をやめた側の記録を更新する
    ///
    /// 判定そのものは [`PlayoutTimeline::drifted_track`] が毎回計算する。保持の起点だけを
    /// ここで記録する。基準の差は観測のたびにしか動かないため、観測のたびに呼べば足りる。
    fn refresh_unshared_state(&mut self) {
        let Some(difference_us) = self.current_base_difference_us() else {
            return;
        };
        if difference_us.saturating_abs() <= self.base_difference_limit_us()
            && !self.base_difference_drifted()
        {
            return;
        }
        self.last_unshared_at_us = self.last_base_difference_at_us;
        self.last_unshared_track = Some(if difference_us > 0 {
            Track::Audio
        } else {
            Track::Video
        });
    }

    /// 直近の基準の差の動き (マイクロ秒/秒)。まだ履歴が無ければ None
    fn base_drift_us_per_second(&self) -> Option<i64> {
        let (movement_us, span_us) = self.base_difference_history()?;
        if span_us <= 0 {
            return None;
        }
        Some(movement_us.saturating_mul(1_000_000) / span_us)
    }

    /// トラックごとの表示時刻の内訳
    fn track_breakdown(&self, track: Track) -> TrackBreakdown {
        let state = &self.tracks[track.index()];
        TrackBreakdown {
            base_delay_us: state.base_us,
            jitter_delay_us: state.base_us.map(|_| state.own_delay_us),
            sync_extra_delay_us: state.sync_extra_us,
            presentation_delay_us: self.presentation_delay_total_us(track),
            presentation_delay_cap_us: self.presentation_cap_us(),
        }
    }

    /// TIMESTAMP から表示時刻までの差 (基準の遅れを含む)。まだ使えないときは None
    fn presentation_delay_total_us(&self, track: Track) -> Option<i64> {
        let base_us = self.tracks[track.index()].base_us?;
        if self.drifted_track() == Some(track) {
            return None;
        }
        Some(base_us.saturating_add(self.delay_us(track)))
    }

    /// 2 つのトラックの基準の差を記録する (一定の間隔で)
    ///
    /// 差が動き続けているかを見るために使う ([`PlayoutTimeline::base_difference_drifted`])。
    /// 記録するのは窓全体の最小値ではなく短い区間の最小値である。窓全体の最小値は、
    /// TIMESTAMP が壁時計から遅れていくときも窓が埋まるまで動かないため、動きを早く
    /// 見つけられない。
    fn record_base_difference(&mut self, now_us: i64) {
        if self
            .last_base_difference_at_us
            .is_some_and(|last_base_difference_at_us| {
                now_us.saturating_sub(last_base_difference_at_us)
                    < TIMELINE_BASE_DIFFERENCE_SAMPLE_INTERVAL_US
            })
        {
            return;
        }
        let Some(audio_us) = self.recent_base_us(Track::Audio, now_us) else {
            return;
        };
        let Some(video_us) = self.recent_base_us(Track::Video, now_us) else {
            return;
        };
        self.last_base_difference_at_us = Some(now_us);
        let difference_us = audio_us.saturating_sub(video_us);
        self.last_base_difference_value_us = Some(difference_us);
        self.base_differences.push(now_us, difference_us);
        self.base_differences
            .prune(now_us.saturating_sub(TIMELINE_BASE_DRIFT_WINDOW_US));
    }

    /// 直近の基準 (マイクロ秒)。まだ観測していなければ None
    ///
    /// 窓全体の最小値 (`base_us`) ではなく短い区間の最小値である。基準が単調に動いて
    /// いるとき、窓全体の最小値は最も古い観測を指したままになる。
    fn recent_base_us(&self, track: Track, now_us: i64) -> Option<i64> {
        self.tracks[track.index()]
            .offsets
            .min_after(now_us.saturating_sub(TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US))
    }

    /// 基準の差が動き続けているか (時計がずれているとみなすか)
    fn base_difference_drifted(&self) -> bool {
        self.base_difference_history()
            .is_some_and(|(movement_us, _)| movement_us.saturating_abs() > TIMELINE_BASE_DRIFT_US)
    }

    /// 基準の差の履歴の、最も古い記録から今までの動き
    ///
    /// 履歴の値と今の差は同じ求め方 (直近の窓の最小値の差) でなければ比べられない。
    /// 窓全体の最小値と比べると、TIMESTAMP が遅れていくときに符号が逆になる。
    ///
    /// 戻り値は (動いた幅, その幅を測った時間)。まだ記録が無ければ None。
    fn base_difference_history(&self) -> Option<(i64, i64)> {
        let (oldest_at_us, oldest_value_us) = self.base_differences.oldest()?;
        let last_value_us = self.last_base_difference_value_us?;
        let last_at_us = self.last_base_difference_at_us?;
        Some((
            last_value_us.saturating_sub(oldest_value_us),
            last_at_us.saturating_sub(oldest_at_us),
        ))
    }

    /// 今の「音声の基準 − 映像の基準」(マイクロ秒)。どちらかを観測していなければ None
    fn current_base_difference_us(&self) -> Option<i64> {
        let audio_base_us = self.tracks[Track::Audio.index()].base_us?;
        let video_base_us = self.tracks[Track::Video.index()].base_us?;
        Some(audio_base_us.saturating_sub(video_base_us))
    }

    /// 基準の差の履歴を消す
    fn clear_base_differences(&mut self) {
        self.base_differences = TimedWindow::default();
        self.last_base_difference_at_us = None;
        self.last_base_difference_value_us = None;
    }

    /// 同期が足した分を含まない、トラックの表示の遅れ (マイクロ秒)。未観測なら None
    ///
    /// 自分の jitter buffer の遅延と `targetLatency` の大きい方である。
    fn natural_delay_known_us(&self, track: Track) -> Option<i64> {
        self.tracks[track.index()].base_us?;
        Some(self.natural_delay_us(track))
    }

    /// 2 つのトラックの基準の遅れの差の閾値 (マイクロ秒)
    ///
    /// 表示の遅れの上限から、上限を適用する前の 2 つのトラックの表示の遅れの大きい方
    /// (自分の揺らぎから求めた遅れと `targetLatency` の大きい方) を引いた値である。
    /// 上限で切られる分は合わせられないためである。同期の制御が足した分は含めない
    /// (含めると、合わせるために遅らせた結果で閾値が下がり、合わせた直後に「基準が
    /// ずれている」と判定されてしまう)。
    fn base_difference_limit_us(&self) -> i64 {
        let delay_us = self
            .natural_delay_us(Track::Audio)
            .max(self.natural_delay_us(Track::Video));
        self.presentation_cap_us()
            .saturating_sub(delay_us)
            .max(TIMELINE_MIN_BASE_DIFFERENCE_MS * 1_000)
    }
}

impl Default for PlayoutTimeline {
    fn default() -> Self {
        Self::new()
    }
}

/// 揺らぎの百分位から映像の表示の遅れの目標を求める (マイクロ秒)
///
/// 学習に使う観測の (遅れ − 基準の遅れ) を並べ、百分位の値を返す。観測が無いときは 0。
fn video_target_us(state: &TrackState, base_us: i64, frame_interval_us: Option<i64>) -> i64 {
    let mut jitters: Vec<i64> = state
        .learning_offsets
        .values()
        .map(|offset_us| offset_us.saturating_sub(base_us).max(0))
        .collect();
    if jitters.is_empty() {
        return 0;
    }
    jitters.sort_unstable();
    jitters[percentile_index(jitters.len(), video_percentile_milli(frame_interval_us))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playout::delay::AUDIO_DELAY_START_MS;

    /// 音声の観測を 1 つ足す
    fn observe_audio(timeline: &mut PlayoutTimeline, wall_clock_us: i64, timestamp_us: i64) {
        timeline.observe(Track::Audio, wall_clock_us, timestamp_us);
    }

    /// 観測が無い間は表示の遅れが無いこと
    #[test]
    fn starts_without_a_delay() {
        let timeline = PlayoutTimeline::new();
        assert!(
            timeline.presentation_delay_us(Track::Audio).is_none(),
            "観測が無ければ表示の遅れは無い"
        );
        assert_eq!(timeline.learned_delay_us(Track::Audio), 0);
    }

    /// 音声の表示の遅れが既定の目標遅延から始まること
    #[test]
    fn audio_starts_with_the_default_learned_delay() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        assert_eq!(
            timeline.learned_delay_us(Track::Audio),
            AUDIO_DELAY_START_MS * 1_000,
            "最初の観測では既定の目標遅延を使う"
        );
        assert_eq!(
            timeline.presentation_delay_us(Track::Audio),
            Some(AUDIO_DELAY_START_MS * 1_000)
        );
    }

    /// TIMESTAMP の差どおりに鳴らす時刻が進むこと
    #[test]
    fn present_time_follows_the_timestamp() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        assert_eq!(
            timeline.present_us(Track::Audio, 1_000_000),
            Some(10_080_000),
            "基準の時刻に表示の遅れを足した時刻に鳴らす"
        );
        assert_eq!(
            timeline.present_us(Track::Audio, 1_020_000),
            Some(10_100_000),
            "TIMESTAMP が 20 ms 進めば鳴らす時刻も 20 ms 進む"
        );
    }

    /// 遅れて届いた観測が基準を動かさないこと
    #[test]
    fn late_arrival_does_not_move_the_basis() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        let present_us = timeline.present_us(Track::Audio, 1_000_000);
        // 同じ TIMESTAMP のフレームが 50 ms 遅れて届いても、基準は早く届いた方のまま
        observe_audio(&mut timeline, 10_050_000, 1_000_000);
        assert_eq!(
            timeline.present_us(Track::Audio, 1_000_000),
            present_us,
            "遅れて届いた観測では基準が動かない"
        );
    }

    /// 早く届いた観測で基準が動くこと
    #[test]
    fn earlier_arrival_moves_the_basis() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_050_000, 1_000_000);
        observe_audio(&mut timeline, 10_050_000, 1_020_000);
        // 経路が空いて 20 ms 早く届くようになった
        observe_audio(&mut timeline, 10_040_000, 1_040_000);
        assert_eq!(
            timeline.present_us(Track::Audio, 1_040_000),
            Some(10_040_000 + timeline.presentation_delay_us(Track::Audio).unwrap_or(0)),
            "早く届いた観測が新しい基準になる"
        );
    }

    /// TIMESTAMP が基準から大きく離れたら取り直すこと
    #[test]
    fn timestamp_jump_starts_a_new_generation() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        let generation = timeline.generation();
        // TIMESTAMP だけが 3 秒進んだ (配信元の切り替えなど)
        observe_audio(&mut timeline, 10_000_000, 4_000_000);
        assert_eq!(
            timeline.generation(),
            generation + 1,
            "大きく離れたら取り直す"
        );
        assert_eq!(
            timeline.present_us(Track::Audio, 4_000_000),
            Some(10_000_000 + timeline.presentation_delay_us(Track::Audio).unwrap_or(0)),
            "新しい基準で鳴らす時刻を求める"
        );
        // TIMESTAMP が大きく戻ったときも取り直す
        observe_audio(&mut timeline, 20_000_000, 2_000_000);
        assert_eq!(
            timeline.generation(),
            generation + 2,
            "戻ったときも取り直す"
        );
    }

    /// 同期の制御は両方のトラックに基準ができてから動くこと
    #[test]
    fn sync_needs_bases_on_both_tracks() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        assert_eq!(
            timeline.presentation_delay_us(Track::Audio),
            Some(80_000),
            "映像の基準が無ければ自分の遅れのまま"
        );
        assert!(
            timeline.presentation_delay_us(Track::Video).is_none(),
            "映像はまだ観測していない"
        );
        // 映像が 300 ms 遅れて届くと、先行する音声を後行の映像に合わせて遅らせる。
        // ただし移す分は上限 (TIMELINE_MAX_COMPENSATED_DIFFERENCE_US) までであり、
        // 残りは A/V のずれとして受け入れる
        timeline.observe(Track::Video, 10_300_000, 1_000_000);
        assert_eq!(
            timeline.presentation_delay_us(Track::Audio),
            Some(180_000),
            "音声の遅れを上げて映像に合わせる"
        );
    }

    /// 映像が遅れて届くとき、先行する音声の遅れを上げて表示時刻を合わせること
    #[test]
    fn sync_raises_the_audio_toward_the_late_video() {
        let mut timeline = PlayoutTimeline::new();
        // 音声は 10 ms で届き、映像は同じ TIMESTAMP で 300 ms 遅れて届く
        observe_audio(&mut timeline, 10_010_000, 1_000_000);
        timeline.observe(Track::Video, 10_310_000, 1_000_000);
        let audio_us = timeline
            .presentation_delay_us(Track::Audio)
            .expect("基準があるので遅れが決まる");
        assert!(audio_us > 80_000, "音声の遅れが上がる: {audio_us}");
        assert!(audio_us < 300_000, "後行側を追い越さない: {audio_us}");
        // 2 つのトラックの表示時刻の差は、目標の差に収まる。自然な差は
        // 300 ms (基準の差) − 80 ms (音声の遅れ) = 220 ms であり、そこから
        // TIMELINE_MAX_COMPENSATED_DIFFERENCE_US を引いた 120 ms になる
        let audio_present_us = timeline
            .present_us(Track::Audio, 1_000_000)
            .expect("基準がある");
        let video_present_us = timeline
            .present_us(Track::Video, 1_000_000)
            .expect("基準がある");
        assert_eq!(
            video_present_us - audio_present_us,
            120_000,
            "相手側へ移す分は上限までにする"
        );
    }

    /// 足した遅延は、差が無くなると毎秒の速さで戻ること
    #[test]
    fn sync_extra_decays_toward_the_natural_floor() {
        let mut timeline = PlayoutTimeline::new();
        // 1 回目: 映像が 300 ms 遅れて届き、先行する音声の遅れが上がる
        observe_audio(&mut timeline, 10_010_000, 1_000_000);
        timeline.observe(Track::Video, 10_310_000, 1_000_000);
        let raised_us = timeline.presentation_delay_us(Track::Audio).unwrap_or(0);
        assert!(raised_us > 80_000, "{raised_us}");

        // 2 回目: 映像が同じ遅れで届くようになる (基準の差が無くなる)
        observe_audio(&mut timeline, 11_010_000, 2_000_000);
        timeline.observe(Track::Video, 11_010_000, 2_000_000);
        let second_us = timeline.presentation_delay_us(Track::Audio).unwrap_or(0);

        // 3 回目: 差が無いまま 1 秒進むと、足した分が毎秒の速さだけ戻る
        observe_audio(&mut timeline, 12_010_000, 3_000_000);
        timeline.observe(Track::Video, 12_010_000, 3_000_000);
        let third_us = timeline.presentation_delay_us(Track::Audio).unwrap_or(0);
        assert_eq!(
            third_us,
            second_us - TIMELINE_DELAY_DECAY_US_PER_SECOND,
            "毎秒 {} マイクロ秒だけ戻す: second={second_us} third={third_us} raised={raised_us}",
            TIMELINE_DELAY_DECAY_US_PER_SECOND
        );
        assert!(
            third_us >= timeline.learned_delay_us(Track::Audio),
            "自分の遅れの下へは戻さない: {third_us}"
        );
    }

    /// 戻す速さは観測の間隔で按分されること
    #[test]
    fn decay_is_prorated_by_elapsed_time() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_010_000, 1_000_000);
        timeline.observe(Track::Video, 10_310_000, 1_000_000);
        observe_audio(&mut timeline, 11_010_000, 2_000_000);
        timeline.observe(Track::Video, 11_010_000, 2_000_000);
        let second_us = timeline.presentation_delay_us(Track::Audio).unwrap_or(0);
        // 2 秒空けると、戻す量も 2 秒ぶんになる
        observe_audio(&mut timeline, 13_010_000, 4_000_000);
        timeline.observe(Track::Video, 13_010_000, 4_000_000);
        let third_us = timeline.presentation_delay_us(Track::Audio).unwrap_or(0);
        assert_eq!(
            third_us,
            second_us - 2 * TIMELINE_DELAY_DECAY_US_PER_SECOND,
            "観測が疎でも速さは変わらない"
        );
    }

    /// 映像の表示の遅れが揺らぎの百分位から求まり、上限を超えないこと
    #[test]
    fn video_delay_comes_from_the_jitter_percentile() {
        let mut timeline = PlayoutTimeline::new();
        let start_wall_us = 10_000_000;
        for index in 0..300i64 {
            // 10 枚に 1 枚だけ 40 ms 遅れて届く
            let jitter_us = if index % 10 == 0 { 40_000 } else { 0 };
            let wall_us = start_wall_us + index * 33_333 + jitter_us;
            let timestamp_us = 1_000_000 + index * 33_333;
            timeline.observe(Track::Video, wall_us, timestamp_us);
        }
        let learned_us = timeline.learned_delay_us(Track::Video);
        assert_eq!(
            learned_us, 40_000,
            "揺らぎの百分位 (40 ms) になる: {learned_us}"
        );
        assert!(
            learned_us <= TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000,
            "上限を超えない"
        );
    }

    /// 表示待ちのキューが浅いと表示の遅れが上限で切られること
    #[test]
    fn video_delay_is_capped_by_the_queue() {
        let config = TimelineConfig {
            video_queue_limit: 5,
            ..TimelineConfig::default()
        };
        let mut timeline = PlayoutTimeline::with_config(config);
        let start_wall_us = 10_000_000;
        for index in 0..300i64 {
            let jitter_us = if index % 10 == 0 { 40_000 } else { 0 };
            let wall_us = start_wall_us + index * 33_333 + jitter_us;
            let timestamp_us = 1_000_000 + index * 33_333;
            timeline.observe(Track::Video, wall_us, timestamp_us);
        }
        // (キューの上限 - 余裕 4 枚) x フレーム間隔 (33 ms) で切られる
        assert_eq!(timeline.learned_delay_us(Track::Video), 33_333);
    }

    /// 表示の遅れの上限 (500 ms) では吸収できない揺らぎが目標を超えないこと
    #[test]
    fn video_delay_is_capped_by_the_maximum() {
        let mut timeline = PlayoutTimeline::new();
        // 2 枚に 1 枚が 600 ms 遅れて届く
        for index in 0..300i64 {
            let jitter_us = if index % 2 == 0 { 600_000 } else { 0 };
            let wall_us = 10_000_000 + index * 33_333 + jitter_us;
            let timestamp_us = 1_000_000 + index * 33_333;
            timeline.observe(Track::Video, wall_us, timestamp_us);
        }
        assert_eq!(
            timeline.learned_delay_us(Track::Video),
            TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000
        );
    }

    /// 表示待ちのキューが余裕分しか無いときは表示の遅れが 0 になること
    #[test]
    fn video_delay_is_zero_when_the_queue_has_no_room() {
        let config = TimelineConfig {
            video_queue_limit: TIMELINE_QUEUE_HEADROOM_FRAMES,
            ..TimelineConfig::default()
        };
        let mut timeline = PlayoutTimeline::with_config(config);
        let start_wall_us = 10_000_000;
        for index in 0..300i64 {
            let jitter_us = if index % 10 == 0 { 40_000 } else { 0 };
            let wall_us = start_wall_us + index * 33_333 + jitter_us;
            let timestamp_us = 1_000_000 + index * 33_333;
            timeline.observe(Track::Video, wall_us, timestamp_us);
        }
        assert_eq!(timeline.learned_delay_us(Track::Video), 0);
    }

    /// live に追いつくまでに届いた揺らぎは学習しないこと
    #[test]
    fn catch_up_arrivals_are_not_learned() {
        let mut timeline = PlayoutTimeline::new();
        // 基準が 250 ms ごとに 20 ms 以上下がる (live へ追いついている最中) 観測を続ける。
        // 遅れは大きいが、これは経路の揺らぎではない
        for index in 0..30i64 {
            let wall_us = 10_000_000 + index * 33_333 + 100_000;
            let timestamp_us = 1_000_000 + index * 53_333;
            timeline.observe(Track::Video, wall_us, timestamp_us);
        }
        assert_eq!(
            timeline.learned_delay_us(Track::Video),
            0,
            "追いつき中は学習しない"
        );
        // 追いついた後に届いた揺らぎは学習する
        let mut wall_us = 11_000_000;
        let mut timestamp_us = 2_599_990;
        for index in 0..100i64 {
            let jitter_us = if index % 10 == 0 { 40_000 } else { 0 };
            wall_us += 33_333;
            timestamp_us += 33_333;
            timeline.observe(Track::Video, wall_us + jitter_us, timestamp_us);
        }
        assert_eq!(
            timeline.learned_delay_us(Track::Video),
            40_000,
            "追いついた後の揺らぎだけを学習する"
        );
    }

    /// 負の設定値は 0 として扱うこと
    #[test]
    fn negative_config_values_are_clamped() {
        let config = TimelineConfig {
            max_presentation_delay_ms: -100,
            target_latency_ms: -50,
            ..TimelineConfig::default()
        };
        let mut timeline = PlayoutTimeline::with_config(config);
        observe_audio(&mut timeline, 10_000_000, 9_000_000);
        assert_eq!(timeline.target_latency_ms(), 0);
        assert_eq!(
            timeline.presentation_delay_us(Track::Audio),
            Some(0),
            "上限が 0 なら表示の遅れも 0"
        );
        assert_eq!(timeline.limited_us(), 0);
    }

    /// targetLatency が 2 つのトラックの表示の遅れの下限になり、切り下げ分が読めること
    #[test]
    fn target_latency_is_the_lower_bound() {
        let config = TimelineConfig {
            target_latency_ms: 120,
            ..TimelineConfig::default()
        };
        let mut timeline = PlayoutTimeline::with_config(config);
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        timeline.observe(Track::Video, 10_000_000, 1_000_000);
        assert_eq!(timeline.presentation_delay_us(Track::Audio), Some(120_000));
        assert_eq!(timeline.presentation_delay_us(Track::Video), Some(120_000));
        assert_eq!(timeline.limited_us(), 0, "上限 (500 ms) に収まっている");

        // 後から上げたときもその場で下限へ反映される
        timeline.set_target_latency_ms(200);
        assert_eq!(timeline.presentation_delay_us(Track::Audio), Some(200_000));

        // 上限を 100 ms にすると 20 ms 切り下げられ、表示の遅れも上限で切られる
        let config = TimelineConfig {
            target_latency_ms: 120,
            max_presentation_delay_ms: 100,
            ..TimelineConfig::default()
        };
        let mut timeline = PlayoutTimeline::with_config(config);
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        assert_eq!(timeline.limited_us(), 20_000, "切り下げた分が読める");
        assert_eq!(
            timeline.presentation_delay_us(Track::Audio),
            Some(100_000),
            "表示の遅れは上限で切られる"
        );
    }

    /// 基準が大きく離れたトラックの表示時刻を返さないこと
    #[test]
    fn a_drifting_track_has_no_presentation_time() {
        let mut timeline = PlayoutTimeline::new();
        // 音声は 1 s の基準、映像は 2 s の基準 (閾値 420 ms を超える)
        observe_audio(&mut timeline, 10_000_000, 9_000_000);
        timeline.observe(Track::Video, 10_000_000, 8_000_000);
        assert!(!timeline.sharing_bases(), "基準を共有していない");
        assert!(timeline.present_us(Track::Audio, 9_000_000).is_some());
        assert!(
            timeline.present_us(Track::Video, 8_000_000).is_none(),
            "遅れている側の表示時刻は返さない"
        );
        assert!(timeline.presentation_delay_us(Track::Video).is_none());
    }

    /// 実績から A/V のずれが読めること
    #[test]
    fn skew_comes_from_the_presented_records() {
        let mut timeline = PlayoutTimeline::new();
        timeline.record_presentation(Track::Audio, 1_000_000, 5_000_000);
        timeline.record_presentation(Track::Video, 1_000_000, 5_040_000);
        assert_eq!(timeline.skew_us(), Some(40_000), "映像が 40 ms 遅れている");
        // 1 秒より古い実績は使わない
        timeline.record_presentation(Track::Audio, 1_000_000, 4_000_000);
        assert_eq!(timeline.skew_us(), None);
    }

    /// 1 つのトラックだけリセットできること
    #[test]
    fn reset_track_keeps_the_other_track() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        timeline.observe(Track::Video, 10_000_000, 1_000_000);
        let generation = timeline.generation();
        timeline.reset_track(Track::Audio);
        assert!(
            timeline.present_us(Track::Audio, 1_000_000).is_none(),
            "音声の基準が消える"
        );
        assert!(
            timeline.present_us(Track::Video, 1_000_000).is_some(),
            "映像の基準は残る"
        );
        assert_eq!(timeline.generation(), generation, "世代は進めない");
    }

    /// すべてリセットしても世代は残ること
    #[test]
    fn reset_clears_the_bases_and_keeps_the_generation() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        timeline.observe(Track::Video, 10_000_000, 1_000_000);
        let generation = timeline.generation();
        timeline.reset();
        assert!(timeline.present_us(Track::Audio, 1_000_000).is_none());
        assert!(timeline.present_us(Track::Video, 1_000_000).is_none());
        assert_eq!(timeline.generation(), generation + 1);
    }

    /// 同期の制御が観測のたびに動くこと (1 秒間隔を待たない)
    #[test]
    fn sync_runs_on_every_observation() {
        let mut timeline = PlayoutTimeline::new();
        // 映像が 300 ms 遅れている状態を保ちながら、100 ms ごとに観測を足す
        for step in 0..3i64 {
            let wall_us = 10_000_000 + step * 100_000;
            let timestamp_us = 1_000_000 + step * 100_000;
            observe_audio(&mut timeline, wall_us, timestamp_us);
            timeline.observe(Track::Video, wall_us + 300_000, timestamp_us);
            assert!(
                timeline.presentation_delay_us(Track::Audio).unwrap_or(0) > 80_000,
                "{step} 回目の観測で遅れを上げる"
            );
        }
    }
}
