//! 復号した音声を鳴らす時刻を決める
//!
//! 目標の時刻に従って音を並べる。目標に間に合わない音も捨てず、今から鳴らせる最も
//! 早い時刻へずらして鳴らし、ずらした分を時間圧縮で目標へ戻す。前の音の終わりと今回の
//! 開始の間に空いた分は、呼び出し側が直前の音を時間伸長して埋められるよう隙間として
//! 返す。目標から離れすぎた音も捨てず、直前の音がまだ鳴っている間は遅れたまま鳴らし、
//! 音が途切れているときだけ到着基準へ並べ直す。時刻はすべて呼び出し側が引数で渡し、
//! デバイス API やタイマーには触れない。

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
/// 経路の停止などで目標から大きく離れた音を鳴らすと、その分だけ音が遅れたままになる。
/// ただし鳴り遅れを理由に音は捨てない (捨てると語尾が切れる)。音がまだ鳴っている間は
/// 到着基準へ並べ直さず、遅れたまま鳴らし続ける
/// ([`AudioPlayoutScheduler::schedule`] を参照)。
pub const AUDIO_PLAYOUT_MAX_LATENESS_US: i64 = 500_000;

/// 到着基準へ並べ直したとみなす、ずらす幅の下限 (マイクロ秒)
///
/// 予約できる最も早い時刻へずらすだけのとき (出力のバッファが既に到着 + 再生の遅れより
/// 先に進んでいて、音が連続したまま鳴るとき) は、ずらす幅が数ミリ秒以下になる。遅れて
/// 届いた音を到着 + 再生の遅れへ置き直すときは数百ミリ秒になる。音 1 つ (Opus で 20 ms)
/// の半分未満のずれは、媒体時刻が跳んだとはみなさない (基準を取り直した回数に数えない)。
pub const AUDIO_PLAYOUT_RESYNC_MIN_JUMP_US: i64 = 10_000;

/// 補間する隙間の下限 (マイクロ秒)
///
/// これ以下の隙間は補間の継ぎ目が耳につくため補間せず、無音のまま残す。
pub const AUDIO_PLAYOUT_MIN_CONCEAL_US: i64 = 5_000;

/// 補間する隙間の上限 (マイクロ秒)
///
/// これより長い欠落は補間で埋めきらず、超えた分は無音のまま残す。埋めた音の末尾の振幅を
/// 下げきる長さ ([`crate::playout::stretch::TIME_STRETCH_MAX_CONCEAL_US`]) と同じにする。
/// これより長く埋めても繰り返しの音を抑える効果が増えないためである。
pub const AUDIO_PLAYOUT_MAX_CONCEAL_US: i64 = crate::playout::stretch::TIME_STRETCH_MAX_CONCEAL_US;

/// 1 つの音を鳴らすかを決める入力 (すべてマイクロ秒)
#[derive(Debug, Clone, Copy)]
pub struct AudioPlayoutInput {
    /// 今の時刻 ([`AudioPlayoutDecision::Play::start_at_us`] と同じ軸)
    ///
    /// 出力のバッファへ既に積まれた分だけ、実際に鳴る位置より先に進む。到着基準の遅れは
    /// [`AudioPlayoutInput::arrival_us`] から数える。
    pub now_us: i64,
    /// 到着した音がまだ鳴っていない位置 ([`AudioPlayoutInput::now_us`] と同じ軸)
    ///
    /// 呼び出し側が、音が届いた時点で実際に鳴っている位置を渡す。到着基準で並べるときの
    /// 遅れ ([`AudioPlayoutInput::arrival_delay_us`]) はこの位置から数える。`now_us` から
    /// 数えると、実際に鳴るのは「到着 + 到着基準の遅れ + 出力のバッファの分」になる。
    /// 時計の対応が無いときは `now_us` と同じ値を渡す。
    pub arrival_us: i64,
    /// 音の TIMESTAMP
    pub timestamp_us: i64,
    /// 音の長さ (0 以上)
    pub duration_us: i64,
    /// 目標の開始時刻。無いこともある
    pub target_start_us: Option<i64>,
    /// 目標を守るか
    pub enforce_target: bool,
    /// 共有の時間軸が学習した再生の遅れ (揺らぎから求めた音声の遅れ)。下限
    /// [`AUDIO_PLAYOUT_MIN_LEAD_US`] 以上
    ///
    /// 並べすぎの上限 ([`AUDIO_PLAYOUT_BACKLOG_US`] を足す相手) に使う。到着基準で鳴らす
    /// ときの遅れは [`AudioPlayoutInput::arrival_delay_us`] を使う。
    pub delay_us: i64,
    /// 到着基準で鳴らすときの再生の遅れ (マイクロ秒)
    ///
    /// 呼び出し側が [`crate::playout::timeline::audio_arrival_delay_us`] で求めた値
    /// (下限 [`crate::playout::timeline::TIMELINE_AUDIO_DELAY_FLOOR_US`]、上限
    /// [`crate::playout::timeline::TIMELINE_ARRIVAL_DELAY_US`]) を渡す。共有の時間軸が
    /// 学習した遅れをそのまま渡すと、TIMESTAMP が壁時計からずれているトラックではその
    /// ずれの分だけ大きく育った値で鳴ってしまう。
    pub arrival_delay_us: i64,
    /// 表示の遅れ (表示時刻に使う遅れ。時間軸が渡す)
    pub presentation_delay_us: i64,
}

/// 鳴らす時刻を決めるのに使った計画
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlayoutBasis {
    /// 時間軸が TIMESTAMP から決めた目標の時刻に従う
    Timestamp,
    /// 目標の時刻を使えない、または守れないため、到着から一定の遅れで鳴らす
    Arrival,
}

/// 音を捨てた理由
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlayoutDropReason {
    /// 並べる音が溜まりすぎている (再生が追いついていない)
    Backlog,
}

/// 鳴らすと決めた音の値 (すべてマイクロ秒)
///
/// [`AudioPlayoutScheduler::last_play`] が返す。音声の再生の観測値
/// ([`crate::playout::timing::AudioPlayoutTimingStats`]) へ渡す値であり、予定に対する
/// 余裕 (再生予定時刻 - 到着) と到着から鳴り始めるまでの時間 (鳴り始める時刻 - 到着) は
/// ここから求められる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPlayoutPlay {
    /// 到着した音がまだ鳴っていない位置 ([`AudioPlayoutInput::arrival_us`])
    pub arrival_us: i64,
    /// 目標の開始時刻 ([`AudioPlayoutInput::target_start_us`])。無いこともある
    pub target_start_us: Option<i64>,
    /// 鳴り始める時刻 ([`AudioPlayoutDecision::Play::start_at_us`])
    pub start_at_us: i64,
    /// 実際に鳴る長さ (詰めた分を引いた後、0 以上)
    ///
    /// 詰める長さを実際に適用した後 ([`AudioPlayoutScheduler::confirm_stretch`]) に読むと、
    /// 適用した長さで直った値になる。
    pub played_us: i64,
    /// 鳴らす時刻を決めるのに使った計画
    pub basis: AudioPlayoutBasis,
}

/// 鳴らす時刻と詰める長さと補間する隙間 (マイクロ秒)、または捨てる
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlayoutDecision {
    /// 鳴らす
    Play {
        /// 鳴らす時刻 (今と同じ軸のマイクロ秒)
        start_at_us: i64,
        /// 鳴らす時刻を決めるのに使った計画。到着基準 ([`AudioPlayoutBasis::Arrival`]) の
        /// ときは、時間軸の再生予定時刻に従っていない (予定そのものは観測値として渡してよい)
        basis: AudioPlayoutBasis,
        /// 波形の周期で詰める長さ (マイクロ秒)。0 なら詰めない
        compress_us: i64,
        /// 補間する隙間の開始時刻 (今と同じ軸のマイクロ秒)。`gap_us` が 0 のときは使わない
        gap_start_us: i64,
        /// 補間する隙間の長さ (マイクロ秒)。上限で切った値であり、0 なら補間しない
        gap_us: i64,
    },
    /// 捨てる
    Drop {
        /// 捨てた理由
        reason: AudioPlayoutDropReason,
    },
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
/// 実行する。実際に詰められた長さは [`AudioPlayoutScheduler::confirm_stretch`] で、
/// 実際に補間した長さは [`AudioPlayoutScheduler::confirm_concealment`] で返す。
pub struct AudioPlayoutScheduler {
    anchor: Option<PlayoutAnchor>,
    /// 直前に鳴らすと決めた音の終わり
    last_end_us: Option<i64>,
    /// 直前に鳴らすと決めた音の TIMESTAMP
    last_timestamp_us: Option<i64>,
    /// 直前に鳴らすと決めた音の値。まだ鳴らすと決めていなければ None
    last_play: Option<AudioPlayoutPlay>,
    /// 目標に対して今どれだけ遅れているか
    lateness_us: i64,
    /// 前の音に要求した詰める量。`confirm_stretch` で置き換える
    requested_us: i64,
    /// 前の音に要求した補間の長さ。`confirm_concealment` で置き換える
    requested_conceal_us: i64,
    rebase_count: u64,
    drop_count: u64,
    compressed_us: i64,
    /// 補間した合計
    concealed_us: i64,
    /// 補間した回数
    conceal_count: u64,
}

impl AudioPlayoutScheduler {
    /// 基準の無い状態で作る
    pub fn new() -> Self {
        Self {
            anchor: None,
            last_end_us: None,
            last_timestamp_us: None,
            last_play: None,
            lateness_us: 0,
            requested_us: 0,
            requested_conceal_us: 0,
            rebase_count: 0,
            drop_count: 0,
            compressed_us: 0,
            concealed_us: 0,
            conceal_count: 0,
        }
    }

    /// 音を鳴らす時刻を決める
    ///
    /// `target_start_us` が無い、または `enforce_target` が false のときは
    /// 到着基準の並べ方を使う。目標から離れすぎた音も捨てず、直前の音がまだ鳴っている
    /// (`last_end_us > now_us`) 間は遅れたまま鳴らし続け、音が途切れているときだけ到着
    /// 基準へ並べ直す (並べ直すと媒体時刻が跳び、鳴っている音の続きが前へずれるため)。
    /// 前の音に要求した詰める量と補間の長さが返ってきていなければ、適用されなかった
    /// ものとして扱う (どちらの並べ方でも同じ)。
    pub fn schedule(&mut self, input: AudioPlayoutInput) -> AudioPlayoutDecision {
        // 前の音に要求した詰める量が返ってきていなければ、適用されなかったものとして扱う
        self.confirm_stretch(0);
        // 前の音に要求した補間の長さも同様に扱う
        self.requested_conceal_us = 0;
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
            return AudioPlayoutDecision::Drop {
                reason: AudioPlayoutDropReason::Backlog,
            };
        }
        // 目標を過ぎて届いた音も前の音と重なる音も捨てない。今から鳴らせる最も早い
        // 時刻へずらして鳴らし、ずらした分を時間圧縮で目標へ戻す
        let previous_end_us = self.last_end_us;
        let earliest_us = input
            .now_us
            .saturating_add(AUDIO_PLAYOUT_MIN_LEAD_US)
            .max(self.last_end_us.unwrap_or(i64::MIN));
        let start_at_us = target_start_us.max(earliest_us);
        let lateness_us = start_at_us.saturating_sub(target_start_us);
        if lateness_us > AUDIO_PLAYOUT_MAX_LATENESS_US {
            // 目標から離れすぎている。直前の音がまだ鳴っている間は、音が連続しているため
            // 到着基準へ並べ直さない (並べ直すと媒体時刻が跳び、鳴っている音の続きが前へ
            // ずれる)。遅れたままでも順序と連続性を保つ方がよい
            let interrupted =
                previous_end_us.is_none_or(|previous_end_us| previous_end_us <= input.now_us);
            if interrupted {
                // 音が途切れている (まだ一度も鳴らしていない、または直前の音が既に鳴り
                // 終わっている)。到着は乱れていないのに予定だけが過去にある
                // (TIMESTAMP が壁時計からずれている)。捨てると語尾が切れるため、到着基準の
                // 小さな目標へ並べ直して鳴らす
                return self.rebase_by_arrival(input);
            }
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
        let (gap_start_us, gap_us) =
            self.concealment_of(input.now_us, previous_end_us, start_at_us);
        self.remember_play(
            &input,
            start_at_us,
            compress_us,
            AudioPlayoutBasis::Timestamp,
        );
        AudioPlayoutDecision::Play {
            start_at_us,
            basis: AudioPlayoutBasis::Timestamp,
            compress_us,
            gap_start_us,
            gap_us,
        }
    }

    /// 実際に詰めた長さを記録する (呼び出し側が時間圧縮を適用した後に呼ぶ)
    ///
    /// 要求した量より少なくしか詰められなかったとき (波形が繰り返していないとき) は、
    /// 詰められなかった分だけ音が後ろへ伸び、遅れとして残る。逆に要求より長く詰められた
    /// とき ([`crate::playout::stretch::compress`] は波形の周期単位でしか削れないため、要求が
    /// 周期より短いと起きる) は、その分だけ音が手前で終わる。どちらも前の音の終わりへ
    /// 反映しないと、続く隙間を見落とす。
    pub fn confirm_stretch(&mut self, applied_us: i64) {
        let requested_us = self.requested_us;
        self.requested_us = 0;
        if requested_us == 0 {
            return;
        }
        let applied_us = applied_us.max(0);
        // 実際に詰めた長さを記録する (要求値では切らない)
        self.compressed_us = self.compressed_us.saturating_add(applied_us);
        // 直前に鳴らすと決めた音の長さも、実際に詰めた長さで直す。詰められなかった分は音が
        // 後ろへ伸び、詰めすぎた分は手前で終わる (前の音の終わりと同じ直し方)
        if let Some(last_play) = self.last_play.as_mut() {
            last_play.played_us = last_play
                .played_us
                .saturating_add(requested_us.saturating_sub(applied_us))
                .max(0);
        }
        let Some(last_end_us) = self.last_end_us.as_mut() else {
            return;
        };
        if applied_us < requested_us {
            // 詰められなかった分は音が後ろへ伸びる
            *last_end_us = last_end_us.saturating_add(requested_us - applied_us);
        } else if applied_us > requested_us {
            // 詰めすぎた分は音が手前で終わる
            *last_end_us = last_end_us.saturating_sub(applied_us - requested_us);
        }
    }

    /// 実際に補間した長さを記録する (呼び出し側が隙間を埋めた後に呼ぶ)
    ///
    /// 要求した長さより長い分は要求までに切る。0 のときは数えない (末尾の相関が足りない、
    /// 継ぎ目の段差が大きい、予約できないときは補間できていないため)。
    pub fn confirm_concealment(&mut self, applied_us: i64) {
        let requested_us = self.requested_conceal_us;
        self.requested_conceal_us = 0;
        if requested_us == 0 {
            return;
        }
        let applied_us = applied_us.clamp(0, requested_us);
        if applied_us == 0 {
            return;
        }
        self.conceal_count += 1;
        self.concealed_us = self.concealed_us.saturating_add(applied_us);
    }

    /// 基準と前の音の終わりと現在の遅れを消す (購読のやり直し)。累積統計は消さない
    pub fn reset(&mut self) {
        self.anchor = None;
        self.last_end_us = None;
        self.last_timestamp_us = None;
        self.last_play = None;
        self.lateness_us = 0;
        self.requested_us = 0;
        self.requested_conceal_us = 0;
    }

    /// 累積統計と現在の遅れを消す
    pub fn reset_stats(&mut self) {
        self.rebase_count = 0;
        self.drop_count = 0;
        self.compressed_us = 0;
        self.concealed_us = 0;
        self.conceal_count = 0;
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

    /// 補間した合計 (マイクロ秒)
    pub fn concealed_us(&self) -> i64 {
        self.concealed_us
    }

    /// 補間した回数 (実際に埋められた音の数)
    pub fn concealments(&self) -> u64 {
        self.conceal_count
    }

    /// 目標に対して今どれだけ遅れているか (マイクロ秒)
    ///
    /// 目標ありの並べ方で更新する。到着基準の間は最後の目標ありの値のまま。
    pub fn lateness_us(&self) -> i64 {
        self.lateness_us
    }

    /// 直前に鳴らすと決めた音の値
    ///
    /// [`AudioPlayoutScheduler::schedule`] が [`AudioPlayoutDecision::Play`] を返したときに
    /// 更新する。捨てる決定では更新しない (直前に鳴らすと決めた音の値が残る)。詰める長さを
    /// 実際に適用した後 ([`AudioPlayoutScheduler::confirm_stretch`]) に読むと、実際に鳴る
    /// 長さが得られる。購読のやり直し ([`AudioPlayoutScheduler::reset`]) で消える。
    pub fn last_play(&self) -> Option<AudioPlayoutPlay> {
        self.last_play
    }

    /// 前の音の終わりと今回の開始の間から、補間する隙間を求める
    ///
    /// 下限 (`AUDIO_PLAYOUT_MIN_CONCEAL_US`) 以下の隙間と、開始 (`previous_end_us`) が
    /// 今 + 余裕 (`AUDIO_PLAYOUT_MIN_LEAD_US`) より前の隙間 (予約できない。過去も含む) は
    /// 補間しない。上限 (`AUDIO_PLAYOUT_MAX_CONCEAL_US`) を超える分は切り、残りは無音の
    /// まま残す。見つかった隙間は `confirm_concealment` で実際に補間した長さを返す要求と
    /// して記録する。補間しないときは開始も長さも 0 にする。
    fn concealment_of(
        &mut self,
        now_us: i64,
        previous_end_us: Option<i64>,
        start_at_us: i64,
    ) -> (i64, i64) {
        let Some(previous_end_us) = previous_end_us else {
            return (0, 0);
        };
        let gap_us = start_at_us.saturating_sub(previous_end_us);
        if gap_us <= AUDIO_PLAYOUT_MIN_CONCEAL_US {
            return (0, 0);
        }
        if previous_end_us < now_us.saturating_add(AUDIO_PLAYOUT_MIN_LEAD_US) {
            return (0, 0);
        }
        let capped_us = gap_us.min(AUDIO_PLAYOUT_MAX_CONCEAL_US);
        self.requested_conceal_us = capped_us;
        (previous_end_us, capped_us)
    }

    /// 目標を使わないときの決め方
    ///
    /// 最初の音で基準を決め、TIMESTAMP の間隔どおりに並べる。過ぎてから届いた音は
    /// 基準を取り直し (ずらす幅が [`AUDIO_PLAYOUT_RESYNC_MIN_JUMP_US`] 未満のときは
    /// 数えない)、TIMESTAMP が大きく飛んだ音は前の音のすぐ後ろと到着 + 再生の遅れの
    /// 遅い方から並べ直し、並べすぎの音は捨てる。
    ///
    /// 並べる基準は `arrival_us` (到着した音がまだ鳴っていない位置) である。`now_us` は
    /// 既に出力のバッファへ積まれた分だけ先に進んでいるため、ここから遅れを数えると
    /// 実際に鳴るのは「到着 + 遅れ + バッファの分」になる。
    fn schedule_by_arrival(&mut self, input: AudioPlayoutInput) -> AudioPlayoutDecision {
        let limit_us = input
            .arrival_delay_us
            .saturating_add(AUDIO_PLAYOUT_BACKLOG_US);
        let previous_end_us = self.last_end_us;
        // 予約できる最も早い時刻。到着から再生の遅れだけ後ろに置いた時刻がこれより前なら、
        // その時刻にはもう予約できない (出力のバッファが先に進んでいる)
        let earliest_us = input.now_us.saturating_add(AUDIO_PLAYOUT_MIN_LEAD_US);
        let mut start_at_us =
            self.expected_start_at(input.timestamp_us, input.arrival_us, input.arrival_delay_us);
        if start_at_us < earliest_us {
            // 過ぎてから届いた (前の音はすべて今より前に終わっているため重ならない)。今から
            // 鳴らせる最も早い時刻へずらす。ずらす幅が無視できるときは基準を取り直さない
            // (取り直すと媒体時刻が跳び、その回数を「並べ直し」として数えてしまう)
            let resynced_us =
                earliest_us.max(input.arrival_us.saturating_add(input.arrival_delay_us));
            if resynced_us.saturating_sub(start_at_us) >= AUDIO_PLAYOUT_RESYNC_MIN_JUMP_US {
                self.rebase(resynced_us, input.timestamp_us);
            }
            start_at_us = resynced_us;
        } else if start_at_us > input.now_us.saturating_add(limit_us) {
            // TIMESTAMP が大きく飛んだ。前の音のすぐ後ろ (か到着 + 再生の遅れ) から並べ直す
            let rebase_us = earliest_us
                .max(input.arrival_us.saturating_add(input.arrival_delay_us))
                .max(self.last_end_us.unwrap_or(i64::MIN));
            if rebase_us > input.now_us.saturating_add(limit_us) {
                // 並べる音が溜まりすぎている。捨てて次の音を目標へ戻す
                self.drop_count += 1;
                return AudioPlayoutDecision::Drop {
                    reason: AudioPlayoutDropReason::Backlog,
                };
            }
            start_at_us = rebase_us;
            self.rebase(start_at_us, input.timestamp_us);
        }
        self.last_end_us = Some(start_at_us.saturating_add(input.duration_us));
        self.last_timestamp_us = Some(input.timestamp_us);
        let (gap_start_us, gap_us) =
            self.concealment_of(input.now_us, previous_end_us, start_at_us);
        self.remember_play(&input, start_at_us, 0, AudioPlayoutBasis::Arrival);
        AudioPlayoutDecision::Play {
            start_at_us,
            basis: AudioPlayoutBasis::Arrival,
            compress_us: 0,
            gap_start_us,
            gap_us,
        }
    }

    /// 目標から離れすぎた音を、到着基準の小さな目標へ並べ直す
    ///
    /// 音が途切れていて (直前の音が無い、または既に鳴り終わっていて) 目標だけが過去にある
    /// とき、ずれているのは予定の方である (TIMESTAMP が壁時計からずれている)。詰めて目標へ
    /// 戻すのではなく、基準そのものを到着基準へ置き直し、以降の音も同じ遅れで並ぶように
    /// する。音がまだ鳴っている間は呼ばない ([`AudioPlayoutScheduler::schedule`] を参照)。
    /// 並べ直すと媒体時刻が跳び、鳴っている音の続きが前へずれるためである。
    ///
    /// 音は捨てない。捨てると語尾が切れるためである。並べ直しで空いた分は、前の音の終わりと
    /// 今回の開始の間の隙間として返す (呼び出し側が上限まで補間できる)。
    fn rebase_by_arrival(&mut self, input: AudioPlayoutInput) -> AudioPlayoutDecision {
        let previous_end_us = self.last_end_us;
        // 到着から再生の遅れだけ後ろへ置く (到着した音がまだ鳴っていない位置から数える)。
        // 前の音の終わりより前には鳴らさない (重ねない)
        let start_at_us = input
            .arrival_us
            .saturating_add(input.arrival_delay_us)
            .max(input.now_us.saturating_add(AUDIO_PLAYOUT_MIN_LEAD_US))
            .max(self.last_end_us.unwrap_or(i64::MIN));
        self.rebase(start_at_us, input.timestamp_us);
        self.last_end_us = Some(start_at_us.saturating_add(input.duration_us));
        self.last_timestamp_us = Some(input.timestamp_us);
        // 到着基準へ移ったため、目標に対する遅れは無い (詰めない)
        self.lateness_us = 0;
        self.requested_us = 0;
        let (gap_start_us, gap_us) =
            self.concealment_of(input.now_us, previous_end_us, start_at_us);
        self.remember_play(&input, start_at_us, 0, AudioPlayoutBasis::Arrival);
        AudioPlayoutDecision::Play {
            start_at_us,
            basis: AudioPlayoutBasis::Arrival,
            compress_us: 0,
            gap_start_us,
            gap_us,
        }
    }

    /// 基準と前の音から、この音を鳴らす時刻を求める (前の音の終わりより前にしない)
    ///
    /// `arrival_us` は到着した音がまだ鳴っていない位置である。最初の音の基準は、この位置
    /// から到着基準の再生の遅れ (`arrival_delay_us`) だけ後ろへ置いた時刻になる。
    fn expected_start_at(
        &mut self,
        timestamp_us: i64,
        arrival_us: i64,
        arrival_delay_us: i64,
    ) -> i64 {
        let Some(anchor) = self.anchor else {
            let time_us = arrival_us.saturating_add(arrival_delay_us);
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

    /// 鳴らすと決めた音の値を残す
    ///
    /// 鳴る長さは、要求した詰める長さを引いた値にする。実際に詰めた長さが返ってきたときは
    /// [`AudioPlayoutScheduler::confirm_stretch`] が直す。
    fn remember_play(
        &mut self,
        input: &AudioPlayoutInput,
        start_at_us: i64,
        compress_us: i64,
        basis: AudioPlayoutBasis,
    ) {
        self.last_play = Some(AudioPlayoutPlay {
            arrival_us: input.arrival_us,
            target_start_us: input.target_start_us,
            start_at_us,
            played_us: input.duration_us.max(0).saturating_sub(compress_us.max(0)),
            basis,
        });
    }
}

impl Default for AudioPlayoutScheduler {
    fn default() -> Self {
        Self::new()
    }
}
