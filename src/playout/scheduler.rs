//! 復号した音声を鳴らす時刻を決める
//!
//! 目標の時刻に従って音を並べる。目標に間に合わない音も捨てず、今から鳴らせる最も
//! 早い時刻へずらして鳴らし、ずらした分を時間圧縮で目標へ戻す。時刻はすべて呼び出し
//! 側が引数で渡し、デバイス API やタイマーには触れない。

/// 再生の遅れの下限 (マイクロ秒)
///
/// 揺らぎから求めた遅れがまだ無いときの値 ([`crate::playout::delay::AUDIO_DELAY_START_MS`]
/// と同じ 80 ms)。呼び出し側が `delay_us` へ渡す。下限の [`AUDIO_PLAYOUT_MIN_LEAD_US`]
/// 以上を渡すこと。
pub const AUDIO_PLAYOUT_DELAY_US: i64 = 80_000;

/// 並べすぎとみなす余裕 (マイクロ秒)
pub const AUDIO_PLAYOUT_BACKLOG_US: i64 = 220_000;

/// 鳴らす時刻の下限 (今からどれだけ先にするか、マイクロ秒)
pub const AUDIO_PLAYOUT_MIN_LEAD_US: i64 = 10_000;

/// 目標から離れすぎとみなす量 (マイクロ秒)
///
/// 経路の停止などで目標から大きく離れた音を鳴らすと、その分だけ音が遅れたままに
/// なるため、一度捨てて目標へ戻す。
pub const AUDIO_PLAYOUT_MAX_LATENESS_US: i64 = 500_000;

/// 1 つの音を鳴らすかを決める入力 (すべてマイクロ秒)
#[derive(Debug, Clone, Copy)]
pub struct AudioPlayoutInput {
    /// 今の時刻 ([`AudioPlayoutDecision::Play::start_at_us`] と同じ軸)
    pub now_us: i64,
    /// 音の TIMESTAMP
    pub timestamp_us: i64,
    /// 音の長さ (0 以上)
    pub duration_us: i64,
    /// 目標の開始時刻。無いこともある
    pub target_start_us: Option<i64>,
    /// 目標を守るか
    pub enforce_target: bool,
    /// 再生の遅れ (揺らぎから求めた音声の遅れ)。下限 [`AUDIO_PLAYOUT_MIN_LEAD_US`] 以上
    pub delay_us: i64,
    /// 表示の遅れ (表示時刻に使う遅れ。時間軸が渡す)
    pub presentation_delay_us: i64,
}

/// 鳴らす時刻と詰める長さ (マイクロ秒)、または捨てる
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlayoutDecision {
    /// 鳴らす
    Play {
        /// 鳴らす時刻 (今と同じ軸のマイクロ秒)
        start_at_us: i64,
        /// 波形の周期で詰める長さ (マイクロ秒)。0 なら詰めない
        compress_us: i64,
    },
    /// 捨てる
    Drop,
}

/// 基準: この timestamp の音をこの時刻に鳴らす
#[derive(Debug, Clone, Copy)]
struct PlayoutAnchor {
    time_us: i64,
    timestamp_us: i64,
}

/// 音を鳴らす時刻を決める (音声トラックごとに 1 つ持つ)
///
/// [`AudioPlayoutScheduler::schedule`] に入力を渡し、返ってきた命令を呼び出し側が
/// 実行する。実際に詰められた長さは [`AudioPlayoutScheduler::confirm_stretch`] で
/// 返す。
pub struct AudioPlayoutScheduler {
    anchor: Option<PlayoutAnchor>,
    /// 直前に鳴らすと決めた音の終わり
    last_end_us: Option<i64>,
    /// 直前に鳴らすと決めた音の TIMESTAMP
    last_timestamp_us: Option<i64>,
    /// 目標に対して今どれだけ遅れているか
    lateness_us: i64,
    /// 前の音に要求した詰める量。`confirm_stretch` で置き換える
    requested_us: i64,
    rebase_count: u64,
    drop_count: u64,
    compressed_us: i64,
}

impl AudioPlayoutScheduler {
    /// 基準の無い状態で作る
    pub fn new() -> Self {
        Self {
            anchor: None,
            last_end_us: None,
            last_timestamp_us: None,
            lateness_us: 0,
            requested_us: 0,
            rebase_count: 0,
            drop_count: 0,
            compressed_us: 0,
        }
    }

    /// 音を鳴らす時刻を決める
    ///
    /// `target_start_us` が無い、または `enforce_target` が false のときは
    /// 到着基準の並べ方を使う。前の音に要求した詰める量が返ってきていなければ、
    /// 適用されなかったものとして扱う (どちらの並べ方でも同じ)。
    pub fn schedule(&mut self, input: AudioPlayoutInput) -> AudioPlayoutDecision {
        // 前の音に要求した詰める量が返ってきていなければ、適用されなかったものとして扱う
        self.confirm_stretch(0);
        if !input.enforce_target {
            return self.schedule_by_arrival(input);
        }
        let Some(target_start_us) = input.target_start_us else {
            return self.schedule_by_arrival(input);
        };

        let limit_us = input
            .delay_us
            .max(input.presentation_delay_us)
            .saturating_add(AUDIO_PLAYOUT_BACKLOG_US);
        if target_start_us > input.now_us.saturating_add(limit_us) {
            // 並べる音が溜まりすぎている
            self.drop_count += 1;
            return AudioPlayoutDecision::Drop;
        }
        // 目標を過ぎて届いた音も前の音と重なる音も捨てない。今から鳴らせる最も早い
        // 時刻へずらして鳴らし、ずらした分を時間圧縮で目標へ戻す
        let earliest_us = input
            .now_us
            .saturating_add(AUDIO_PLAYOUT_MIN_LEAD_US)
            .max(self.last_end_us.unwrap_or(i64::MIN));
        let start_at_us = target_start_us.max(earliest_us);
        let lateness_us = start_at_us.saturating_sub(target_start_us);
        if lateness_us > AUDIO_PLAYOUT_MAX_LATENESS_US {
            // 目標から離れすぎている。一度捨てて目標へ戻す
            self.drop_count += 1;
            self.lateness_us = 0;
            return AudioPlayoutDecision::Drop;
        }
        self.lateness_us = lateness_us;
        // 詰める量は、遅れの分と音の長さの半分の小さい方にする
        let compress_us = lateness_us.min(input.duration_us.max(0) / 2);
        self.requested_us = compress_us;
        self.last_end_us = Some(
            start_at_us
                .saturating_add(input.duration_us)
                .saturating_sub(compress_us),
        );
        self.last_timestamp_us = Some(input.timestamp_us);
        AudioPlayoutDecision::Play {
            start_at_us,
            compress_us,
        }
    }

    /// 実際に詰めた長さを記録する (呼び出し側が時間圧縮を適用した後に呼ぶ)
    ///
    /// 要求した量より少なくしか詰められなかったとき (波形が繰り返していないとき) は、
    /// 詰められなかった分だけ音が後ろへ伸び、遅れとして残る。
    pub fn confirm_stretch(&mut self, applied_us: i64) {
        let requested_us = self.requested_us;
        self.requested_us = 0;
        if requested_us == 0 {
            return;
        }
        let applied_us = applied_us.clamp(0, requested_us);
        self.compressed_us = self.compressed_us.saturating_add(applied_us);
        if applied_us < requested_us
            && let Some(last_end_us) = self.last_end_us.as_mut()
        {
            // 詰められなかった分は音が後ろへ伸びる
            *last_end_us = last_end_us.saturating_add(requested_us - applied_us);
        }
    }

    /// 基準と前の音の終わりと現在の遅れを消す (購読のやり直し)。累積統計は消さない
    pub fn reset(&mut self) {
        self.anchor = None;
        self.last_end_us = None;
        self.last_timestamp_us = None;
        self.lateness_us = 0;
        self.requested_us = 0;
    }

    /// 累積統計と現在の遅れを消す
    pub fn reset_stats(&mut self) {
        self.rebase_count = 0;
        self.drop_count = 0;
        self.compressed_us = 0;
        self.lateness_us = 0;
    }

    /// 基準を取り直した回数
    pub fn rebases(&self) -> u64 {
        self.rebase_count
    }

    /// 捨てた音の数
    pub fn drops(&self) -> u64 {
        self.drop_count
    }

    /// 波形の周期で詰めた合計 (マイクロ秒)
    pub fn compressed_us(&self) -> i64 {
        self.compressed_us
    }

    /// 目標に対して今どれだけ遅れているか (マイクロ秒)
    ///
    /// 目標ありの並べ方で更新する。到着基準の間は最後の目標ありの値のまま。
    pub fn lateness_us(&self) -> i64 {
        self.lateness_us
    }

    /// 目標を使わないときの決め方
    ///
    /// 最初の音で基準を決め、TIMESTAMP の間隔どおりに並べる。過ぎてから届いた音は
    /// 基準を取り直し、並べすぎの音は捨てる。
    fn schedule_by_arrival(&mut self, input: AudioPlayoutInput) -> AudioPlayoutDecision {
        let limit_us = input.delay_us.saturating_add(AUDIO_PLAYOUT_BACKLOG_US);
        let mut start_at_us =
            self.expected_start_at(input.timestamp_us, input.now_us, input.delay_us);
        if start_at_us < input.now_us.saturating_add(AUDIO_PLAYOUT_MIN_LEAD_US) {
            // 過ぎてから届いた。前の音はすべて今より前に終わっているため重ならない
            start_at_us = input.now_us.saturating_add(input.delay_us);
            self.rebase(start_at_us, input.timestamp_us);
        } else if start_at_us > input.now_us.saturating_add(limit_us) {
            let earliest_us = input
                .now_us
                .saturating_add(input.delay_us)
                .max(self.last_end_us.unwrap_or(i64::MIN));
            if earliest_us > input.now_us.saturating_add(limit_us) {
                // 並べる音が溜まりすぎている。捨てて次の音を目標へ戻す
                self.drop_count += 1;
                return AudioPlayoutDecision::Drop;
            }
            // TIMESTAMP が大きく飛んだ。前の音のすぐ後ろ (か今 + 再生の遅れ) から並べ直す
            start_at_us = earliest_us;
            self.rebase(start_at_us, input.timestamp_us);
        }
        self.last_end_us = Some(start_at_us.saturating_add(input.duration_us));
        self.last_timestamp_us = Some(input.timestamp_us);
        AudioPlayoutDecision::Play {
            start_at_us,
            compress_us: 0,
        }
    }

    /// 基準と前の音から、この音を鳴らす時刻を求める (前の音の終わりより前にしない)
    fn expected_start_at(&mut self, timestamp_us: i64, now_us: i64, delay_us: i64) -> i64 {
        let Some(anchor) = self.anchor else {
            let time_us = now_us.saturating_add(delay_us);
            self.anchor = Some(PlayoutAnchor {
                time_us,
                timestamp_us,
            });
            return time_us;
        };
        let by_timestamp_us = anchor
            .time_us
            .saturating_add(timestamp_us.saturating_sub(anchor.timestamp_us));
        let after_previous_us = self.last_end_us.unwrap_or(by_timestamp_us);
        let advanced = self
            .last_timestamp_us
            .is_some_and(|last_timestamp_us| timestamp_us > last_timestamp_us);
        if advanced {
            by_timestamp_us.max(after_previous_us)
        } else {
            after_previous_us
        }
    }

    fn rebase(&mut self, time_us: i64, timestamp_us: i64) {
        self.anchor = Some(PlayoutAnchor {
            time_us,
            timestamp_us,
        });
        self.rebase_count += 1;
    }
}

impl Default for AudioPlayoutScheduler {
    fn default() -> Self {
        Self::new()
    }
}
