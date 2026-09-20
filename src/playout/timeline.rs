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
//! - A/V 同期: [`crate::playout::sync`] の制御を 1 秒ごとに 1 回だけ呼ぶ

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::playout::delay::AudioDelayManager;
use crate::playout::sync::{
    StreamSynchronization, SyncDelays, SyncMeasurement, compute_relative_delay,
};

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

/// A/V 同期の制御を行う間隔 (マイクロ秒)
pub const TIMELINE_SYNC_INTERVAL_US: i64 = 1_000_000;

/// A/V 同期の制御の間隔とみなす許容 (マイクロ秒)
///
/// 呼び出し側のタイマーが 1 秒よりわずかに早く来ることがあるため、この分は同じ
/// 間隔とみなす。
pub const TIMELINE_SYNC_TOLERANCE_US: i64 = 50_000;

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
    /// 表示に使う遅れ (マイクロ秒)
    presentation_delay_us: i64,
    /// 直近の観測の復号の出力の時刻 (マイクロ秒)
    last_arrival_us: Option<i64>,
    /// 直近の観測の TIMESTAMP (マイクロ秒)
    last_timestamp_us: Option<i64>,
    /// 直近に観測した時刻 (同期の制御に新しい観測があるかの判定に使う)
    last_observation_us: Option<i64>,
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
            presentation_delay_us: 0,
            last_arrival_us: None,
            last_timestamp_us: None,
            last_observation_us: None,
            last_delay_update_us: 0,
            frame_intervals_us: VecDeque::new(),
            catching_up: true,
            catch_up_checkpoint: None,
        }
    }

    /// 同期の制御に渡す実測
    fn measurement(&self) -> Option<SyncMeasurement> {
        let (last_arrival_us, last_timestamp_us) = (self.last_arrival_us?, self.last_timestamp_us?);
        Some(SyncMeasurement {
            latest_receive_time_us: last_arrival_us,
            latest_capture_time_us: last_timestamp_us,
        })
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
/// [`PlayoutTimeline::present_us`] で求める。1 秒ごとに [`PlayoutTimeline::sync`] を
/// 呼ぶと、A/V 同期の制御 ([`crate::playout::sync`]) が動く。
#[derive(Debug)]
pub struct PlayoutTimeline {
    /// 設定
    config: TimelineConfig,
    /// 音声と映像の状態
    tracks: [TrackState; 2],
    /// 音声と映像の遅延の差を制御する
    sync: StreamSynchronization,
    /// 直近に同期の制御を行った時刻
    last_sync_us: Option<i64>,
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
            sync: StreamSynchronization::new(),
            last_sync_us: None,
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
        // 下限が上がった分は、次の観測を待たずにその場で反映する
        let target_us = self.config.target_latency_ms.saturating_mul(1_000);
        for state in &mut self.tracks {
            if state.base_us.is_some() {
                let natural_us = state.own_delay_us.max(target_us);
                state.presentation_delay_us = state.presentation_delay_us.max(natural_us);
            }
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
        state.last_observation_us = Some(wall_clock_us);

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

        // 表示に使う遅れを自分の遅れ (と targetLatency) 以上に保つ
        let target_us = config.target_latency_ms.saturating_mul(1_000);
        let natural_us = state.own_delay_us.max(target_us);
        if state.presentation_delay_us < natural_us {
            state.presentation_delay_us = natural_us;
        }
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

    /// A/V 同期の制御を行う
    ///
    /// [`TIMELINE_SYNC_INTERVAL_US`] ごとに 1 回だけ動く。両方のトラックに前回の
    /// 制御より新しい観測があり、基準の差が閾値の中にあるときだけ
    /// [`crate::playout::sync`] の制御を呼び、返ってきた遅延の下限をそのまま表示の
    /// 遅れとして保持する。制御が動かなかったときは、表示の遅れを自分の遅れへ向けて
    /// 毎秒 [`TIMELINE_DELAY_DECAY_US_PER_SECOND`] だけ下げる。
    pub fn sync(&mut self, now_us: i64) -> Option<SyncDelays> {
        let previous_sync_us = self.last_sync_us;
        if let Some(last_sync_us) = previous_sync_us
            && now_us.saturating_sub(last_sync_us)
                < TIMELINE_SYNC_INTERVAL_US - TIMELINE_SYNC_TOLERANCE_US
        {
            return None;
        }
        self.last_sync_us = Some(now_us);

        // 基準が大きく離れたトラックがある間は制御しない (ずれの推定が単調に増える)
        if self.drifted_track().is_some() {
            return None;
        }
        // 前回の制御より新しい観測が両方に無いときは制御しない
        let has_new_observation = |index: usize| {
            self.tracks[index]
                .last_observation_us
                .is_some_and(|last_us| {
                    previous_sync_us.is_none_or(|previous_us| last_us > previous_us)
                })
        };
        if !has_new_observation(Track::Audio.index()) || !has_new_observation(Track::Video.index())
        {
            self.decay_presentation_delays(previous_sync_us, now_us);
            return None;
        }

        let Some(relative_delay_ms) = self.relative_delay_ms() else {
            self.decay_presentation_delays(previous_sync_us, now_us);
            return None;
        };
        // A/V 同期の基準の遅延は targetLatency である
        self.sync
            .set_target_buffering_delay(self.config.target_latency_ms);
        let current_audio_delay_ms = self.delay_us(Track::Audio) / 1_000;
        let current_video_delay_ms = self.delay_us(Track::Video) / 1_000;
        let Some(delays) = self.sync.compute_delays(
            relative_delay_ms,
            current_audio_delay_ms,
            current_video_delay_ms,
        ) else {
            // ずれが不感帯の中では制御しない。目標が下がっている分だけ下げる
            self.decay_presentation_delays(previous_sync_us, now_us);
            return None;
        };
        self.apply_sync_lower_bounds(previous_sync_us, now_us, delays);
        Some(delays)
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
    /// どちらかがまだ観測されていないときも true (まだずれは分からない)。
    pub fn sharing_bases(&self) -> bool {
        self.drifted_track().is_none()
    }

    /// 1 つのトラックの基準と学習と実績を消す (音声の再生を止めたときなど)
    ///
    /// 世代は進めない。世代を進めると、もう片方に既に積んだフレームの表示時刻が
    /// 決められなくなるためである。`targetLatency` は設定の値であり残す。
    pub fn reset_track(&mut self, track: Track) {
        self.tracks[track.index()] = TrackState::new();
        self.presented[track.index()] = None;
        // 同期の制御の状態も、そのトラックの観測が無い状態に戻す。もう片方に足した分は
        // 消したトラックとの差を合わせるためのものなので残さない
        self.sync = StreamSynchronization::new();
        self.last_sync_us = None;
    }

    /// 基準と学習をすべて消す (TIMESTAMP の飛び、購読のやり直し)。世代を進める
    pub fn reset(&mut self) {
        self.generation = self.generation.saturating_add(1);
        for track in [Track::Audio, Track::Video] {
            self.tracks[track.index()] = TrackState::new();
            self.presented[track.index()] = None;
        }
        self.sync = StreamSynchronization::new();
        self.last_sync_us = None;
    }

    /// 基準を取り直した回数
    ///
    /// 取り直すと、それより前に積んだフレームの表示時刻は新しい基準では決められない
    /// (飛びの分だけ未来になる)。積む側が世代を見て、古いフレームを到着順に扱える
    /// ようにする。
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 音声と映像の経路の相対遅延 (ミリ秒) を求める
    ///
    /// 直近の観測どうしを比べる。片方の観測がまだ無いときと、極端に離れている
    /// ときは None。
    fn relative_delay_ms(&self) -> Option<i64> {
        let audio = self.tracks[Track::Audio.index()].measurement()?;
        let video = self.tracks[Track::Video.index()].measurement()?;
        compute_relative_delay(audio, video)
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
    /// 表示の遅れの下限と同期の制御の結果を、表示の遅れの上限で切った値である。
    fn delay_us(&self, track: Track) -> i64 {
        self.tracks[track.index()]
            .presentation_delay_us
            .min(self.presentation_cap_us())
    }

    /// 表示の遅れの上限 (マイクロ秒)
    fn presentation_cap_us(&self) -> i64 {
        presentation_cap_us(
            &self.config,
            self.tracks[Track::Video.index()].frame_interval_us(),
        )
    }

    /// 表示の遅れを自分の遅れへ向けて少しずつ下げる
    ///
    /// 経過した時間に応じて毎秒 [`TIMELINE_DELAY_DECAY_US_PER_SECOND`] まで下げる。
    fn decay_presentation_delays(&mut self, previous_sync_us: Option<i64>, now_us: i64) {
        let budget_us = self.decay_budget_us(previous_sync_us, now_us);
        if budget_us <= 0 {
            return;
        }
        for track in [Track::Audio, Track::Video] {
            let natural_us = self.natural_delay_us(track);
            let state = &mut self.tracks[track.index()];
            if state.presentation_delay_us > natural_us {
                state.presentation_delay_us =
                    (state.presentation_delay_us - budget_us).max(natural_us);
            }
        }
    }

    /// `crate::playout::sync` の制御が返した遅延の下限を表示の遅れへ反映する
    ///
    /// 返した値は表示の遅れの下限として保持する。上げる向きはその場で反映し、下げる
    /// 向きは毎秒 [`TIMELINE_DELAY_DECAY_US_PER_SECOND`] までにする (急に下げると、
    /// 既に積んだフレームが表示時刻を過ぎて捨てられる)。
    fn apply_sync_lower_bounds(
        &mut self,
        previous_sync_us: Option<i64>,
        now_us: i64,
        delays: SyncDelays,
    ) {
        let budget_us = self.decay_budget_us(previous_sync_us, now_us);
        for (track, delay_ms) in [
            (Track::Audio, delays.audio_delay_ms),
            (Track::Video, delays.video_delay_ms),
        ] {
            let floor_us = self
                .natural_delay_us(track)
                .max(delay_ms.saturating_mul(1_000));
            let state = &mut self.tracks[track.index()];
            let lowered_us = state.presentation_delay_us.saturating_sub(budget_us);
            state.presentation_delay_us = floor_us.max(lowered_us);
        }
    }

    /// 一度に下げてよい量 (マイクロ秒)
    ///
    /// まだ一度も制御していないときは、制御の間隔 1 秒ぶんにする。
    fn decay_budget_us(&self, previous_sync_us: Option<i64>, now_us: i64) -> i64 {
        let elapsed_us = match previous_sync_us {
            Some(previous_sync_us) => now_us.saturating_sub(previous_sync_us).max(0),
            None => TIMELINE_SYNC_INTERVAL_US,
        };
        TIMELINE_DELAY_DECAY_US_PER_SECOND.saturating_mul(elapsed_us) / 1_000_000
    }

    /// 基準の差が閾値を超えているトラック (遅れている方)。無ければ None
    fn drifted_track(&self) -> Option<Track> {
        let audio_base_us = self.tracks[Track::Audio.index()].base_us?;
        let video_base_us = self.tracks[Track::Video.index()].base_us?;
        if audio_base_us.saturating_sub(video_base_us).saturating_abs()
            <= self.base_difference_limit_us()
        {
            return None;
        }
        Some(if audio_base_us > video_base_us {
            Track::Audio
        } else {
            Track::Video
        })
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

    /// 同期は両方のトラックに新しい観測が揃ってから動くこと
    #[test]
    fn sync_needs_new_observations_on_both_tracks() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        assert!(
            timeline.sync(10_000_000).is_none(),
            "映像の観測が無ければ制御しない"
        );
        // 前回の同期より後に映像を観測しても、音声に新しい観測が無ければ制御しない
        timeline.observe(Track::Video, 11_000_000, 2_000_000);
        assert!(
            timeline.sync(11_500_000).is_none(),
            "音声に新しい観測が無ければ制御しない"
        );
        // 両方に新しい観測が付くと制御する
        observe_audio(&mut timeline, 12_000_000, 3_000_000);
        timeline.observe(Track::Video, 13_000_000, 3_500_000);
        assert!(
            timeline.sync(13_500_000).is_some(),
            "両方に新しい観測が付くと制御する"
        );
    }

    /// 映像が遅れているとき、制御が返した音声の下限が表示の遅れに反映されること
    #[test]
    fn sync_raises_the_audio_toward_the_late_video() {
        let mut timeline = PlayoutTimeline::new();
        // 音声は 10 ms で届き、映像は同じ TIMESTAMP で 300 ms 遅れて届く
        observe_audio(&mut timeline, 10_010_000, 1_000_000);
        timeline.observe(Track::Video, 10_310_000, 1_000_000);
        // 制御を繰り返すと、返る音声の下限が段々と上がる
        let mut raised_us = 0;
        let mut wall_us = 10_010_000;
        for step in 1..=4i64 {
            let timestamp_us = 1_000_000 + step * TIMELINE_SYNC_INTERVAL_US;
            wall_us = timestamp_us + 9_010_000;
            observe_audio(&mut timeline, wall_us, timestamp_us);
            timeline.observe(Track::Video, wall_us + 300_000, timestamp_us);
            let delays = timeline.sync(wall_us).expect("ずれが大きいので制御する");
            assert!(
                timeline.presentation_delay_us(Track::Audio).unwrap_or(0)
                    >= delays.audio_delay_ms * 1_000,
                "返した下限以上になる"
            );
            raised_us = delays.audio_delay_ms * 1_000;
        }
        assert!(raised_us > 80_000, "下限が 80 ms を超える: {raised_us}");
        assert!(
            timeline.presentation_delay_us(Track::Audio).unwrap_or(0) > 80_000,
            "返した下限が表示の遅れに反映される"
        );
        assert!(
            timeline.sync(wall_us + 500_000).is_none(),
            "間隔を空けずに呼んでも制御しない"
        );
    }

    /// 制御が動かないときは学習した目標遅延へ向けて下がること
    #[test]
    fn delays_decay_toward_the_learned_target() {
        let mut timeline = PlayoutTimeline::new();
        // 揺らぎが無い観測を続けて学習を 20 ms まで下げる
        for index in 0..40i64 {
            let time_us = 1_000_000 + index * 20_000;
            observe_audio(&mut timeline, time_us, time_us);
            timeline.observe(Track::Video, time_us, time_us);
        }
        assert_eq!(timeline.learned_delay_us(Track::Audio), 20_000);
        assert_eq!(timeline.presentation_delay_us(Track::Audio), Some(80_000));
        // 制御が動かない状態で 1 秒ごとに観測を足すと、20 ms ずつ下がる
        let mut wall_us = 2_000_000;
        timeline.sync(wall_us);
        assert_eq!(
            timeline.presentation_delay_us(Track::Audio),
            Some(60_000),
            "20 ms 下がる"
        );
        wall_us += TIMELINE_SYNC_INTERVAL_US;
        observe_audio(&mut timeline, wall_us, wall_us);
        timeline.observe(Track::Video, wall_us, wall_us);
        timeline.sync(wall_us);
        assert_eq!(timeline.presentation_delay_us(Track::Audio), Some(40_000));
        wall_us += TIMELINE_SYNC_INTERVAL_US;
        observe_audio(&mut timeline, wall_us, wall_us);
        timeline.observe(Track::Video, wall_us, wall_us);
        timeline.sync(wall_us);
        assert_eq!(
            timeline.presentation_delay_us(Track::Audio),
            Some(20_000),
            "学習した目標遅延で止まる"
        );
        wall_us += TIMELINE_SYNC_INTERVAL_US;
        observe_audio(&mut timeline, wall_us, wall_us);
        timeline.observe(Track::Video, wall_us, wall_us);
        timeline.sync(wall_us);
        assert_eq!(
            timeline.presentation_delay_us(Track::Audio),
            Some(20_000),
            "目標より下げない"
        );
    }

    /// 1 秒よりわずかに早い呼び出しでも間隔が空いたとみなすこと
    #[test]
    fn sync_tolerates_a_slightly_early_tick() {
        let mut timeline = PlayoutTimeline::new();
        for index in 0..40i64 {
            let time_us = 1_000_000 + index * 20_000;
            observe_audio(&mut timeline, time_us, time_us);
            timeline.observe(Track::Video, time_us, time_us);
        }
        assert_eq!(timeline.presentation_delay_us(Track::Audio), Some(80_000));
        timeline.sync(2_000_000);
        assert_eq!(timeline.presentation_delay_us(Track::Audio), Some(60_000));
        // 990 ms 後でも下がる (タイマーの揺れを許す)。下げ幅は毎秒 20 ms 以下
        let mut wall_us = 2_990_000;
        observe_audio(&mut timeline, wall_us, wall_us);
        timeline.observe(Track::Video, wall_us, wall_us);
        timeline.sync(wall_us);
        let after_us = timeline.presentation_delay_us(Track::Audio).unwrap_or(0);
        assert!(
            (40_000..60_000).contains(&after_us),
            "990 ms でも間隔が空いたとみなして下がる: {after_us}"
        );
        // 500 ms では動かない
        wall_us += 500_000;
        observe_audio(&mut timeline, wall_us, wall_us);
        timeline.observe(Track::Video, wall_us, wall_us);
        timeline.sync(wall_us);
        assert_eq!(
            timeline.presentation_delay_us(Track::Audio),
            Some(after_us),
            "間隔が短ければ動かない"
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
        assert!(
            timeline.sync(10_500_000).is_none(),
            "ずれている間は同期の制御を呼ばない"
        );
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

    /// 同期の制御が 1 秒ごとに 1 回だけ動くこと
    #[test]
    fn sync_runs_once_per_second() {
        let mut timeline = PlayoutTimeline::new();
        // 映像が 300 ms 遅れている状態を保ちながら、1 秒ごとに観測を足す
        for step in 0..3i64 {
            let wall_us = 10_000_000 + step * TIMELINE_SYNC_INTERVAL_US;
            let timestamp_us = 1_000_000 + step * TIMELINE_SYNC_INTERVAL_US;
            observe_audio(&mut timeline, wall_us, timestamp_us);
            timeline.observe(Track::Video, wall_us + 300_000, timestamp_us);
            let now_us = wall_us + 400_000;
            assert!(timeline.sync(now_us).is_some(), "{step} 回目は制御する");
            assert!(
                timeline.sync(now_us + 500_000).is_none(),
                "間隔の中では制御しない"
            );
        }
    }
}
