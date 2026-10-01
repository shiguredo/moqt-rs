//! 到着の遅れの分布から音声の目標遅延を決める
//!
//! 到着の揺らぎから、途切れの確率が一定以下になる jitter buffer の遅延 (目標遅延) を
//! 到着の遅れの分布の分位点から求める。送受信の時計のずれと経路の最小遅延は、直近の
//! 窓で最も早く届いた観測を基準にすることで引き算で消す。時刻やデバイスには触れない。

use alloc::collections::VecDeque;

/// 到着の遅れを観測する窓 (ミリ秒)
pub const AUDIO_DELAY_HISTORY_WINDOW_MS: i64 = 2_000;

/// ヒストグラムのバケット数
pub const AUDIO_DELAY_BUCKETS: usize = 100;

/// ヒストグラムのバケットの幅 (ミリ秒)
pub const AUDIO_DELAY_BUCKET_MS: i64 = 20;

/// 目標にする分位点 (途切れの確率が 1 - この値未満になる遅延を目標にする)
pub const AUDIO_DELAY_QUANTILE: f64 = 0.95;

/// ヒストグラムの忘れ係数 (収束後)
pub const AUDIO_DELAY_FORGET_FACTOR: f64 = 0.983;

/// ヒストグラムへ入れる間隔 (ミリ秒)
///
/// 区間の最大の遅れだけを入れる。平均ではなく最大を使うことで、揺らぎの尾を
/// 目標遅延へ反映させる。
pub const AUDIO_DELAY_RESAMPLE_INTERVAL_MS: i64 = 500;

/// 観測が無いときの目標遅延 (ミリ秒)
pub const AUDIO_DELAY_START_MS: i64 = 80;

/// 最初の数回の忘れ方の重み (収束を速くする)
const AUDIO_DELAY_START_FORGET_WEIGHT: i64 = 2;

/// 保持する観測の上限
///
/// 窓の中の観測数は到着の頻度で決まるが、同じ時刻の観測が繰り返し届いても
/// メモリが増え続けないよう、件数でも上限を置く。
const AUDIO_DELAY_MAX_ARRIVALS: usize = 512;

/// 忘れ方と重みを持つヒストグラム
///
/// バケットの合計は常に 1 で、追加のたびに全体を忘れ係数倍し、観測したバケットへ
/// `1 - 忘れ係数` を足す。最初の数回は忘れ方を速くし、目標を観測へすぐ合わせる。
struct DelayHistogram {
    buckets: [f64; AUDIO_DELAY_BUCKETS],
    forget_factor: f64,
    add_count: u64,
}

impl DelayHistogram {
    /// 初期分布で作る
    fn new() -> Self {
        let mut histogram = Self {
            buckets: [0.0; AUDIO_DELAY_BUCKETS],
            forget_factor: 0.0,
            add_count: 0,
        };
        histogram.reset();
        histogram
    }

    /// 初期分布 (`0.5^(i + 1)`) に戻す
    fn reset(&mut self) {
        let mut weight = 1.0f64;
        for bucket in self.buckets.iter_mut() {
            weight *= 0.5;
            *bucket = weight;
        }
        self.forget_factor = 0.0;
        self.add_count = 0;
    }

    /// 観測したバケットの重みを上げ、ほかのバケットの重みを下げる
    fn add(&mut self, value: usize) {
        for bucket in self.buckets.iter_mut() {
            *bucket *= self.forget_factor;
        }
        self.buckets[value] += 1.0 - self.forget_factor;
        self.add_count += 1;
        let factor = 1.0 - AUDIO_DELAY_START_FORGET_WEIGHT as f64 / (self.add_count + 1) as f64;
        self.forget_factor = factor.clamp(0.0, AUDIO_DELAY_FORGET_FACTOR);
    }

    /// 先頭から足し上げた確率 (累積分布) が `probability` 以上になる最小のバケットを返す
    fn quantile(&self, probability: f64) -> usize {
        let inverse_probability = 1.0 - probability;
        let mut index = 0usize;
        let mut sum = 1.0 - self.buckets[0];
        while sum > inverse_probability && index < AUDIO_DELAY_BUCKETS - 1 {
            index += 1;
            sum -= self.buckets[index];
        }
        index
    }
}

/// 到着の遅れの観測 (どちらもマイクロ秒)
#[derive(Debug, Clone, Copy)]
struct Arrival {
    /// 復号の出力の時刻
    arrival_us: i64,
    /// そのデータの TIMESTAMP
    capture_us: i64,
}

/// 音声の目標遅延を到着の遅れの分布から決める
///
/// トラックごとに 1 つ持つ。`observe` に「復号の出力の時刻」と「そのデータの
/// TIMESTAMP」をマイクロ秒で渡し、目標遅延は [`AudioDelayManager::target_delay_ms`]
/// で読む。
pub struct AudioDelayManager {
    histogram: DelayHistogram,
    /// 直近の観測。窓より古いものは捨てる
    arrivals: VecDeque<Arrival>,
    /// 区間の開始時刻 (マイクロ秒)。まだ観測していなければ None
    interval_start_us: Option<i64>,
    /// 区間の最大の相対遅延 (マイクロ秒)
    max_in_interval_us: i64,
    /// 直近の観測の TIMESTAMP の最大値 (マイクロ秒)。まだ観測していなければ None
    latest_capture_us: Option<i64>,
    /// 求めた目標遅延 (ミリ秒)。まだヒストグラムへ入れていなければ None
    optimal_delay_ms: Option<i64>,
}

impl AudioDelayManager {
    /// 観測の無い状態で作る
    pub fn new() -> Self {
        Self {
            histogram: DelayHistogram::new(),
            arrivals: VecDeque::new(),
            interval_start_us: None,
            max_in_interval_us: 0,
            latest_capture_us: None,
            optimal_delay_ms: None,
        }
    }

    /// 復号の出力を 1 つ記録する
    ///
    /// `arrival_us` は復号の出力の時刻、`capture_us` はそのデータの TIMESTAMP
    /// (どちらもマイクロ秒)。時刻の差は 1000 で割り、0 方向への切り捨てでミリ秒へ
    /// 換算して扱う。極端な値は飽和する。TIMESTAMP が窓より大きく戻ったときは、
    /// 配信元の切り替えとみなして履歴を作り直す。
    pub fn observe(&mut self, arrival_us: i64, capture_us: i64) {
        let window_us = AUDIO_DELAY_HISTORY_WINDOW_MS * 1_000;
        if let Some(latest_capture_us) = self.latest_capture_us
            && capture_us < latest_capture_us.saturating_sub(window_us)
        {
            // 配信元の切り替えなどで TIMESTAMP が窓より大きく戻ったときは、
            // 履歴と区間を作り直し、新しい TIMESTAMP を基準に戻す
            self.arrivals.clear();
            self.interval_start_us = None;
            self.max_in_interval_us = 0;
            self.latest_capture_us = None;
        }
        let relative_us = self.relative_delay_us(arrival_us, capture_us);
        self.arrivals.push_back(Arrival {
            arrival_us,
            capture_us,
        });
        self.latest_capture_us = Some(match self.latest_capture_us {
            Some(latest_capture_us) => latest_capture_us.max(capture_us),
            None => capture_us,
        });
        self.prune_arrivals(capture_us);

        let interval_start_us = match self.interval_start_us {
            Some(start_us) => start_us,
            None => {
                self.interval_start_us = Some(arrival_us);
                arrival_us
            }
        };
        // 区間の最大の遅れだけをヒストグラムへ入れる
        let mut update_us = None;
        if arrival_us.saturating_sub(interval_start_us) > AUDIO_DELAY_RESAMPLE_INTERVAL_MS * 1_000 {
            update_us = Some(self.max_in_interval_us);
            self.interval_start_us = Some(arrival_us);
            self.max_in_interval_us = 0;
        }
        self.max_in_interval_us = self.max_in_interval_us.max(relative_us);
        let Some(update_us) = update_us else {
            return;
        };
        let relative_ms = update_us / 1_000;
        let index = relative_ms / AUDIO_DELAY_BUCKET_MS;
        if !(0..AUDIO_DELAY_BUCKETS as i64).contains(&index) {
            return;
        }
        self.histogram.add(index as usize);
        let bucket = self.histogram.quantile(AUDIO_DELAY_QUANTILE);
        self.optimal_delay_ms = Some((bucket as i64 + 1) * AUDIO_DELAY_BUCKET_MS);
    }

    /// 目標遅延 (ミリ秒)。まだ観測から求めていないときは [`AUDIO_DELAY_START_MS`]
    pub fn target_delay_ms(&self) -> i64 {
        self.optimal_delay_ms.unwrap_or(AUDIO_DELAY_START_MS)
    }

    /// 観測と学習を消す (購読のやり直し)
    pub fn reset(&mut self) {
        self.histogram.reset();
        self.arrivals.clear();
        self.interval_start_us = None;
        self.max_in_interval_us = 0;
        self.latest_capture_us = None;
        self.optimal_delay_ms = None;
    }

    /// 直近の窓で最も早く届いた観測を基準にした相対遅延 (マイクロ秒)
    fn relative_delay_us(&self, arrival_us: i64, capture_us: i64) -> i64 {
        let oldest_us = capture_us.saturating_sub(AUDIO_DELAY_HISTORY_WINDOW_MS * 1_000);
        let mut best: Option<Arrival> = None;
        for entry in &self.arrivals {
            if entry.capture_us < oldest_us {
                continue;
            }
            let is_better = match best {
                Some(best) => {
                    entry.arrival_us.saturating_sub(entry.capture_us)
                        < best.arrival_us.saturating_sub(best.capture_us)
                }
                None => true,
            };
            if is_better {
                best = Some(*entry);
            }
        }
        let Some(best) = best else {
            return 0;
        };
        arrival_us
            .saturating_sub(best.arrival_us)
            .saturating_sub(capture_us.saturating_sub(best.capture_us))
            .max(0)
    }

    /// 窓より古い観測を捨て、件数の上限も守る
    fn prune_arrivals(&mut self, capture_us: i64) {
        let oldest_us = capture_us.saturating_sub(AUDIO_DELAY_HISTORY_WINDOW_MS * 1_000);
        self.arrivals.retain(|entry| entry.capture_us >= oldest_us);
        while self.arrivals.len() > AUDIO_DELAY_MAX_ARRIVALS {
            self.arrivals.pop_front();
        }
    }
}

impl Default for AudioDelayManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_arrivals_are_pruned() {
        // 2 秒の窓より古い観測が残り続けないこと
        let mut manager = AudioDelayManager::new();
        for index in 0..10_000i64 {
            let time_us = index * 1_000;
            manager.observe(time_us, time_us);
        }
        assert!(manager.arrivals.len() <= AUDIO_DELAY_HISTORY_WINDOW_MS as usize + 1);

        // 同じ時刻の観測が繰り返し届いても件数の上限を超えないこと
        let mut manager = AudioDelayManager::new();
        for _ in 0..10_000 {
            manager.observe(1_000_000, 1_000_000);
        }
        assert!(manager.arrivals.len() <= AUDIO_DELAY_MAX_ARRIVALS);
    }

    #[test]
    fn backward_capture_jump_restarts_the_history() {
        let mut manager = AudioDelayManager::new();
        // 大きな TIMESTAMP で 100 ms の揺らぎを入れて目標遅延を上げる
        for index in 0..80i64 {
            let capture_us = (1_000_000 + index * 20) * 1_000;
            let delay_ms = if index % 2 == 0 { 0 } else { 100 };
            manager.observe(capture_us + delay_ms * 1_000, capture_us);
        }
        assert_eq!(manager.target_delay_ms(), 120);

        // TIMESTAMP が大きく戻っても履歴を作り直し、揺らぎの無い観測で下がる
        for index in 0..300i64 {
            let capture_us = index * 600 * 1_000;
            manager.observe(capture_us, capture_us);
        }
        assert_eq!(manager.target_delay_ms(), 20);
    }

    #[test]
    fn reset_clears_the_learning() {
        let mut manager = AudioDelayManager::new();
        for index in 0..40i64 {
            let capture_us = (1_000 + index * 20) * 1_000;
            manager.observe(capture_us, capture_us);
        }
        manager.reset();
        assert!(manager.arrivals.is_empty());
        assert!(manager.latest_capture_us.is_none());
        assert!(manager.optimal_delay_ms.is_none());
    }
}
