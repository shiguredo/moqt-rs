//! 音声と映像の遅延を相対的に制御して同期させる
//!
//! 2 つのトラックのずれの推定を平滑化し、不感帯を置き、片側だけを上限付きで
//! 動かす。ずれの制御量は「映像の遅延 - 音声の遅延 + 経路の相対遅延」であり、
//! 映像が音声よりどれだけ遅れて出るかに等しい。時刻やデバイスには触れない。

/// 1 回の制御で動かす量の上限 (ミリ秒)
pub const SYNC_MAX_CHANGE_MS: i64 = 80;

/// ずれの推定を捨てる閾値 (ミリ秒)
///
/// これを超えるずれは、配信元の切り替えや時計の飛びとみなして制御しない。
pub const SYNC_MAX_DELTA_DELAY_MS: i64 = 10_000;

/// ずれの推定を平滑化する係数
pub const SYNC_FILTER_LENGTH: i64 = 4;

/// 制御を行うずれの下限 (ミリ秒)
///
/// この不感帯の中では遅延を変えないため、映像は音声より最大この値だけ先行できる。
pub const SYNC_MIN_DELTA_MS: i64 = 30;

/// 同期の制御に使う 1 つのトラックの実測 (どちらもマイクロ秒)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncMeasurement {
    /// 直近に受信 (復号の出力) した時刻 (受信側の壁時計、マイクロ秒)
    pub latest_receive_time_us: i64,
    /// そのデータの TIMESTAMP (送信側の壁時計、マイクロ秒)
    pub latest_capture_time_us: i64,
}

/// 同期の制御が決めた、各トラックの遅延の下限 (ミリ秒)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncDelays {
    /// 音声の遅延の下限 (ミリ秒)
    pub audio_delay_ms: i64,
    /// 映像の遅延の下限 (ミリ秒)
    pub video_delay_ms: i64,
}

/// 音声と映像の経路の相対遅延 (ミリ秒) を求める
///
/// 正の値は映像の方が遅く届いていることを表す。時刻の差は 1000 で割り、0 方向への
/// 切り捨てでミリ秒へ換算する。極端な値は飽和する。[`SYNC_MAX_DELTA_DELAY_MS`] を
/// 超える場合は制御しないため None を返す。
pub fn compute_relative_delay(audio: SyncMeasurement, video: SyncMeasurement) -> Option<i64> {
    let relative_us = video
        .latest_receive_time_us
        .saturating_sub(audio.latest_receive_time_us)
        .saturating_sub(video.latest_capture_time_us)
        .saturating_add(audio.latest_capture_time_us);
    let relative_ms = relative_us / 1_000;
    if relative_ms.saturating_abs() > SYNC_MAX_DELTA_DELAY_MS {
        return None;
    }
    Some(relative_ms)
}

/// 制御の対象にする遅延 (ミリ秒)
#[derive(Debug, Clone, Copy, Default)]
struct SynchronizationDelay {
    /// 基準の遅延を含む、制御中の遅延
    extra_ms: i64,
    /// 直前に返した値
    last_ms: i64,
}

/// 音声と映像の遅延を相対的に制御する (トラックの組ごとに 1 つ持つ)
///
/// 相対遅延と、現在の音声の遅延と映像の遅延を呼び出しごとに引数で受け取る。
/// 呼び出しは 1 秒ごとに 1 回を想定する。
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamSynchronization {
    /// 基準の遅延 (ミリ秒)。[`StreamSynchronization::set_target_buffering_delay`] で決める
    base_target_delay_ms: i64,
    audio_delay: SynchronizationDelay,
    video_delay: SynchronizationDelay,
    /// ずれの推定 (ミリ秒)。制御に使ったら 0 へ戻す
    average_diff_ms: i64,
}

impl StreamSynchronization {
    /// 基準の遅延が 0 の状態で作る
    pub fn new() -> Self {
        Self::default()
    }

    /// 基準の遅延を決める (ミリ秒)
    ///
    /// 音声と映像の両方が最低でもこの値だけ遅れる (MSF の `targetLatency` に
    /// 相当する)。
    pub fn set_target_buffering_delay(&mut self, target_delay_ms: i64) {
        let difference_ms = target_delay_ms.saturating_sub(self.base_target_delay_ms);
        self.audio_delay.extra_ms = self.audio_delay.extra_ms.saturating_add(difference_ms);
        self.audio_delay.last_ms = self.audio_delay.last_ms.saturating_add(difference_ms);
        self.video_delay.last_ms = self.video_delay.last_ms.saturating_add(difference_ms);
        self.video_delay.extra_ms = self.video_delay.extra_ms.saturating_add(difference_ms);
        self.base_target_delay_ms = target_delay_ms;
    }

    /// 遅延の制御を行う
    ///
    /// `relative_delay_ms` は [`compute_relative_delay`] が求めた経路の相対遅延、
    /// `current_audio_delay_ms` と `current_video_delay_ms` は今使っている各トラックの
    /// 遅延 (どちらもミリ秒)。不感帯の中では None を返す。
    pub fn compute_delays(
        &mut self,
        relative_delay_ms: i64,
        current_audio_delay_ms: i64,
        current_video_delay_ms: i64,
    ) -> Option<SyncDelays> {
        // 映像がどれだけ遅れているか (A/V のずれ)
        let current_diff_ms = current_video_delay_ms
            .saturating_sub(current_audio_delay_ms)
            .saturating_add(relative_delay_ms);
        // ずれの推定を平滑化する (単純な移動平均ではなく漸化式)
        self.average_diff_ms = (self
            .average_diff_ms
            .saturating_mul(SYNC_FILTER_LENGTH - 1)
            .saturating_add(current_diff_ms))
            / SYNC_FILTER_LENGTH;
        if self.average_diff_ms.saturating_abs() < SYNC_MIN_DELTA_MS {
            // 不感帯の中。映像は音声より最大 SYNC_MIN_DELTA_MS だけ先行できる
            return None;
        }
        // 1 回に動かす量。平均の半分を上限で切る
        let diff_ms = (self.average_diff_ms / 2).clamp(-SYNC_MAX_CHANGE_MS, SYNC_MAX_CHANGE_MS);
        // 動かしたら平均を戻す (行き過ぎない)
        self.average_diff_ms = 0;

        // 動かす側を決めて遅延を更新する
        let video_moved;
        if diff_ms > 0 {
            // 映像が音声より遅れている。映像の余分な遅延を削るか、音声を遅らせる
            if self.video_delay.extra_ms > self.base_target_delay_ms {
                self.video_delay.extra_ms = self.video_delay.extra_ms.saturating_sub(diff_ms);
                self.audio_delay.extra_ms = self.base_target_delay_ms;
                video_moved = true;
            } else {
                self.audio_delay.extra_ms = self.audio_delay.extra_ms.saturating_add(diff_ms);
                self.video_delay.extra_ms = self.base_target_delay_ms;
                video_moved = false;
            }
        } else {
            // 映像が音声より進んでいる。音声の余分な遅延を削るか、映像を遅らせる
            if self.audio_delay.extra_ms > self.base_target_delay_ms {
                self.audio_delay.extra_ms = self.audio_delay.extra_ms.saturating_add(diff_ms);
                self.video_delay.extra_ms = self.base_target_delay_ms;
                video_moved = false;
            } else {
                self.video_delay.extra_ms = self.video_delay.extra_ms.saturating_sub(diff_ms);
                self.audio_delay.extra_ms = self.base_target_delay_ms;
                video_moved = true;
            }
        }

        // 遅延は基準の遅延から 10 秒以内に保つ (内部の状態も同じ範囲に収める)
        let limit_ms = self
            .base_target_delay_ms
            .saturating_add(SYNC_MAX_DELTA_DELAY_MS);
        self.audio_delay.extra_ms = self
            .audio_delay
            .extra_ms
            .clamp(self.base_target_delay_ms, limit_ms);
        self.video_delay.extra_ms = self
            .video_delay
            .extra_ms
            .clamp(self.base_target_delay_ms, limit_ms);

        // 動かした側は動かした後の値、動かさなかった側は前回の値を返す
        let (audio_delay_ms, video_delay_ms) = if video_moved {
            (self.audio_delay.last_ms, self.video_delay.extra_ms)
        } else {
            (self.audio_delay.extra_ms, self.video_delay.last_ms)
        };
        self.audio_delay.last_ms = audio_delay_ms;
        self.video_delay.last_ms = video_delay_ms;
        Some(SyncDelays {
            audio_delay_ms,
            video_delay_ms,
        })
    }
}
