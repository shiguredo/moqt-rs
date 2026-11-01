//! 復号した音声を鳴らす時刻を決める
//!
//! 目標の時刻に従って音を並べる。目標に間に合わない音も捨てず、今から鳴らせる最も
//! 早い時刻へずらして鳴らし、ずらした分を、遅れと音の長さの半分とキューの余裕の最小値
//! まで時間圧縮で目標へ戻す (キューの余裕が無いときは詰めない)。前の音の終わりと今回の
//! 開始の間に空いた分は、補間できる区間と、既に予約できない区間 (無音のまま残る) に
//! 分けて返し、呼び出し側が隙間を材料化できるようにする。目標から離れすぎた音も捨てず、
//! 直前の音がまだ鳴っている間は遅れたまま鳴らし、音が途切れているときだけ到着基準へ
//! 並べ直す。到着基準では、過ぎてから届いた音に目標の深さを再度足さず、後続の音の
//! 時間伸長で深さを戻す。出力が枯れた時間は隙間として二重に数えないよう、呼び出し側が
//! [`AudioPlayoutScheduler::note_underrun`] で知らせる。時刻はすべて呼び出し側が引数で
//! 渡し、デバイス API やタイマーには触れない。
//!
//! moqt-js の `src/playout/audioPlayout.ts` (HEAD) に対応する。時刻はすべて i64 の
//! マイクロ秒である。moqt-js は秒の浮動小数で計算するが、本実装はマイクロ秒の整数で
//! 計算し、判定に影響しない 1 マイクロ秒未満の差だけを丸め誤差として許す。

/// 再生の遅れの下限 (マイクロ秒)
///
/// 揺らぎから求めた遅れがまだ無いときの値。呼び出し側が `delay_us` へ渡す。共有の時間軸が
/// 使う下限 ([`crate::playout::timeline::TIMELINE_AUDIO_DELAY_FLOOR_US`]) と同じ値であり、
/// 定義は時間軸側に置く (揺らぎから求めた遅れをその値で下限にするため)。ここではその定義を
/// そのまま使う。下限の [`AUDIO_PLAYOUT_MIN_LEAD_US`] 以上を渡すこと。
pub const AUDIO_PLAYOUT_DELAY_US: i64 = crate::playout::timeline::TIMELINE_AUDIO_DELAY_FLOOR_US;

/// 並べすぎとみなす余裕 (マイクロ秒)
///
/// 並べすぎの判定は「再生の遅れ + この余裕」で行い、その合計を
/// [`AUDIO_PLAYOUT_MAX_DELAY_US`] (300 ms) に保つ。
pub const AUDIO_PLAYOUT_BACKLOG_US: i64 = AUDIO_PLAYOUT_MAX_DELAY_US - AUDIO_PLAYOUT_DELAY_US;

/// 並べすぎとみなす、再生の遅れの合計の上限 (マイクロ秒)
///
/// moqt-js の `AUDIO_PLAYOUT_MAX_DELAY_SECONDS` に対応する。並べすぎの判定は
/// `max(学習した遅れ, 表示の遅れ) + 余裕` がこの値を超えるかで行う。
pub const AUDIO_PLAYOUT_MAX_DELAY_US: i64 = 300_000;

/// 鳴らす時刻の下限 (今からどれだけ先にするか、マイクロ秒)
pub const AUDIO_PLAYOUT_MIN_LEAD_US: i64 = 10_000;

/// 時間圧縮を始める遅れ (マイクロ秒)
///
/// 目標に対する遅れがこれ未満のときは詰めない。時間圧縮は波形のピッチ周期 (2.5 ms 以上
/// 15 ms 以下) を 1 つ削る操作であり、削る長さは周期で決まって要求では選べない。遅れが
/// 数 ms しかないときに動かすと、削った分だけ「早すぎる」側へ振れ、次の音でまた遅れて
/// 詰める往復になる。往復のたびに周期が 1 つ消えるため、声では粗さ・ざらつきとして聞こえる。
///
/// moqt-js の実測 (2026-10-14、実リレー経由の 60 秒、16 kHz モノラル) では、遅れが約
/// 2.5 ms で安定しているのに 60 秒の再生で 2727 ms を詰めていた。この値にすると
/// 722〜780 ms まで減り、鳴り始めの遅れは 98 ms から 113〜114 ms へ僅かに伸びるだけで
/// あった。時間圧縮を始めるかどうかの閾値そのものであり、A/V 同期の不感帯
/// ([`crate::playout::timeline::TIMELINE_SYNC_MIN_DELTA_US`]) とは独立に決めた値である。
///
/// これは scheduler の既定値であり、時間軸が観測から求めた値
/// ([`crate::playout::timeline::PlayoutTimeline::compression_deadband_us`]) を
/// [`AudioPlayoutInput::stretch_min_us`] で渡すと、timestamp 経路だけそちらを使う。
/// 到着基準の予約は経路の揺らぎを吸収するための余裕であり、この値のままにする。
pub const AUDIO_PLAYOUT_STRETCH_MIN_US: i64 = 20_000;

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
/// これ以下の隙間は補間せず、無音のまま残す。以前は 5 ms だったが、実リレーの収録では
/// 5 ms 以下の無音が 27 個 (合計 62.9 ms) あり、その両端の段差がクリックとして聞こえて
/// いた。0.5 ms は 48 kHz で 24 サンプルである。これ以下の隙間も無音として残さず、
/// 前の音の終わりへ揃えて穴を作らない
/// (`snap_to_previous_end`)。目標の表示時刻の更新や丸めで生じる
/// 20 µs〜0.5 ms の前進を連続再生で吸収するため。
pub const AUDIO_PLAYOUT_MIN_CONCEAL_US: i64 = 500;

/// 補間する隙間の上限 (マイクロ秒)
///
/// これより長い欠落は補間で埋めきらず、超えた分は無音のまま残す。
pub const AUDIO_PLAYOUT_MAX_CONCEAL_US: i64 = 100_000;

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
    /// 目標ごとの圧縮の不感帯 (マイクロ秒)。timestamp 経路だけが使う
    ///
    /// 呼び出し側が時間軸の
    /// [`crate::playout::timeline::PlayoutTimeline::compression_deadband_us`] で求めた値を
    /// 渡す。到着基準の予約は scheduler の既定値
    /// ([`AUDIO_PLAYOUT_STRETCH_MIN_US`]) を使う。
    pub stretch_min_us: Option<i64>,
}

/// 補間する隙間の 3 分割 (マイクロ秒)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioPlayoutConcealment {
    /// 補間を予約した先頭 (前の音の終わり、または今 + 最小の余裕)。補間しないときは None
    pub gap_start_us: Option<i64>,
    /// 補間を予約した長さ (上限で切った後)。0 なら補間しない
    pub conceal_us: i64,
    /// 前の音の終わりから今回の開始までの全長。材料化する長さ
    pub total_gap_us: i64,
    /// 予約できる最も早い時刻より前の長さ
    ///
    /// 既に過去の分と、未来でも余裕に満たない分を含む。
    pub unrecoverable_gap_us: i64,
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
    /// 実際に鳴る長さ (0 以上)
    ///
    /// [`AudioPlayoutScheduler::schedule`] の時点では、音の長さから要求した詰める長さを
    /// 引いた値である。その後に [`AudioPlayoutScheduler::confirm_stretch`] を呼ぶと実際に
    /// 詰めた長さが、[`AudioPlayoutScheduler::confirm_expansion`] を呼ぶと実際に伸ばした
    /// 長さが、[`AudioPlayoutScheduler::confirm_playout_end`] を呼ぶと確定した出力の終端
    /// までの長さが反映される。moqt-js の `playSound` が時間圧縮・時間伸長・接続フェード・
    /// サンプルへの丸めの後の実フレーム数を `recordPlay` へ渡すのと同じ値である。
    ///
    /// 計器 ([`crate::playout::timing::AudioPlayoutTimingStats::record_play`]) へ渡す前に、
    /// 実際に適用した値の confirm をすべて呼ぶこと。
    pub played_us: i64,
    /// 鳴らす時刻を決めるのに使った計画
    pub basis: AudioPlayoutBasis,
}

/// 鳴らす時刻と詰める長さと隙間 (マイクロ秒)、または捨てる
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
        ///
        /// 遅れと音の長さの半分とキューの余裕 (直前の音の終わり − (今 +
        /// [`AUDIO_PLAYOUT_MIN_LEAD_US`])) の最小値であり、キューの余裕が 0 以下のときは
        /// 0 である。実際に詰められるかは波形しだいであり、適用した結果を
        /// [`AudioPlayoutScheduler::confirm_stretch`] で返す
        compress_us: i64,
        /// 到着基準の深さを戻すために伸長してよい長さ (マイクロ秒)。0 なら伸長しない
        ///
        /// 再同期の直後だけ要求する。実際に伸ばした長さを
        /// [`AudioPlayoutScheduler::confirm_expansion`] へ返す。
        expand_us: i64,
        /// 補間を予約した先頭 (前の音の終わり、または今 + 最小の余裕)。補間しないときは None
        gap_start_us: Option<i64>,
        /// 補間を予約した長さ (上限で切った後)。0 なら補間しない
        conceal_us: i64,
        /// 前の音の終わりから今回の開始までの隙間の全長。0 なら隙間が無い
        ///
        /// 下限でも上限でも切り捨てない。呼び出し側が隙間を必ず材料化する
        /// ([`crate::playout::gap::materialize`]) ために、材料化する長さそのものを返す
        total_gap_us: i64,
        /// 隙間のうち、予約できる最も早い時刻より前で補間できない長さ (マイクロ秒)
        ///
        /// 既に過去の分と、未来でも余裕に満たない分を含む。呼び出し側は、この長さを
        /// 無音の前置きとして材料化したうえで、その後に補間を置く。
        unrecoverable_gap_us: i64,
    },
    /// 捨てる
    Drop {
        /// 捨てた理由
        reason: AudioPlayoutDropReason,
    },
}

/// 鳴らす時刻の決定の設定
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPlayoutConfig {
    /// 並べすぎとみなす余裕 (マイクロ秒)
    pub backlog_us: i64,
    /// 鳴らす時刻の下限 (今からどれだけ先にするか、マイクロ秒)
    pub min_lead_us: i64,
    /// 到着基準と、目標ごとの指定が無い timestamp 経路の圧縮の不感帯 (マイクロ秒)
    pub stretch_min_us: i64,
}

impl Default for AudioPlayoutConfig {
    fn default() -> Self {
        Self {
            backlog_us: AUDIO_PLAYOUT_BACKLOG_US,
            min_lead_us: AUDIO_PLAYOUT_MIN_LEAD_US,
            stretch_min_us: AUDIO_PLAYOUT_STRETCH_MIN_US,
        }
    }
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
/// 実際に伸ばした長さは [`AudioPlayoutScheduler::confirm_expansion`] で、
/// 実際に補間した長さは [`AudioPlayoutScheduler::confirm_concealment`] で返す。
pub struct AudioPlayoutScheduler {
    /// 設定
    config: AudioPlayoutConfig,
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
    /// 到着基準の圧縮では、確定した削減量だけ timestamp と再生時刻の対応も前へ戻す
    requested_by_arrival: bool,
    /// 前の音に要求した伸長の長さ。`confirm_expansion` で置き換える
    requested_expansion_us: i64,
    /// 再同期後の深さの回復で実際に伸ばした合計
    expanded_us: i64,
    /// 再同期後の深さ回復の最中か
    arrival_depth_recovery: bool,
    rebase_count: u64,
    drop_count: u64,
    compressed_us: i64,
    /// 補間した合計
    concealed_us: i64,
    /// 補間した回数
    conceal_count: u64,
}

impl AudioPlayoutScheduler {
    /// 設定の既定値で作る
    pub fn new() -> Self {
        Self::with_config(AudioPlayoutConfig::default())
    }

    /// 設定を決めて作る
    ///
    /// 負の値は 0 として扱う。
    pub fn with_config(mut config: AudioPlayoutConfig) -> Self {
        config.backlog_us = config.backlog_us.max(0);
        config.min_lead_us = config.min_lead_us.max(0);
        config.stretch_min_us = config.stretch_min_us.max(0);
        Self {
            config,
            anchor: None,
            last_end_us: None,
            last_timestamp_us: None,
            last_play: None,
            lateness_us: 0,
            requested_us: 0,
            requested_conceal_us: 0,
            requested_by_arrival: false,
            requested_expansion_us: 0,
            expanded_us: 0,
            arrival_depth_recovery: false,
            rebase_count: 0,
            drop_count: 0,
            compressed_us: 0,
            concealed_us: 0,
            conceal_count: 0,
        }
    }

    /// 使っている設定
    pub fn config(&self) -> AudioPlayoutConfig {
        self.config
    }

    /// 音を鳴らす時刻を決める
    ///
    /// `target_start_us` が無い、または `enforce_target` が false のときは
    /// 到着基準の並べ方を使う。目標から離れすぎた音も捨てず、直前の音がまだ鳴っている
    /// (`last_end_us > now_us`) 間は遅れたまま鳴らし続け、音が途切れているときだけ到着
    /// 基準へ並べ直す (並べ直すと媒体時刻が跳び、鳴っている音の続きが前へずれるため)。
    /// 前の音に要求した詰める量と補間の長さと伸長の長さが返ってきていなければ、適用
    /// されなかったものとして扱う (どちらの並べ方でも同じ)。
    ///
    /// 目標に間に合わない音へ要求する詰める量は、遅れと音の長さの半分とキューの余裕
    /// (直前の音の終わり − (今 + [`AUDIO_PLAYOUT_MIN_LEAD_US`])) の最小値にする。
    /// 遅れが不感帯 ([`AudioPlayoutInput::stretch_min_us`] があればその値、無ければ
    /// [`AUDIO_PLAYOUT_STRETCH_MIN_US`]) 未満のときと、キューの余裕が 0 以下のときは
    /// 0 にする。返した `compress_us` は削ってよい長さの上限でもあるため、呼び出し側は
    /// そのまま [`crate::playout::stretch::compress`] へ渡すこと。
    pub fn schedule(&mut self, input: AudioPlayoutInput) -> AudioPlayoutDecision {
        // 前の音に要求した補間・伸長・圧縮の確定が返っていなければ、適用されなかったもの
        // として扱う。分岐の前に戻すため、到着基準の経路でも要求が残らない
        self.requested_conceal_us = 0;
        self.requested_expansion_us = 0;
        self.confirm_stretch(0);
        if input.target_start_us.is_none() || !input.enforce_target {
            return self.schedule_by_arrival(input);
        }
        // timestamp 経路では、再同期の回復は今回の並べ方で決め直す
        self.arrival_depth_recovery = false;
        let stretch_min_us = input.stretch_min_us.unwrap_or(self.config.stretch_min_us);
        let target_start_us = input.target_start_us.expect("分岐で確認済み");
        let limit_us = input
            .delay_us
            .max(input.presentation_delay_us)
            .saturating_add(self.config.backlog_us);
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
            .saturating_add(self.config.min_lead_us)
            .max(previous_end_us.unwrap_or(i64::MIN));
        let start_at_us = self.snap_to_previous_end(
            target_start_us.max(earliest_us),
            previous_end_us,
            input.now_us,
        );
        // スナップで目標より前に鳴ることがあるため、遅れは負にしない
        let lateness_us = start_at_us.saturating_sub(target_start_us).max(0);
        if lateness_us > AUDIO_PLAYOUT_MAX_LATENESS_US {
            // 目標から離れすぎている。直前の音がまだ鳴っている間は、音が連続している
            // ため到着基準へ並べ直さない (並べ直すと媒体時刻が跳び、鳴っている音の続きが
            // 前へずれる)。遅れたままでも順序と連続性を保つ方がよい
            let interrupted =
                previous_end_us.is_none_or(|previous_end_us| previous_end_us <= input.now_us);
            if interrupted {
                // キューが空である (まだ一度も鳴らしていない、または直前の音が既に鳴り
                // 終わっている)。到着は乱れていないのに予定だけが過去にある
                // (TIMESTAMP が壁時計からずれている)。捨てると語尾が切れるため、到着基準の
                // 小さな目標へ並べ直して鳴らす
                return self.rebase_by_arrival(input);
            }
        }
        self.lateness_us = lateness_us;
        // キューの余裕 = 直前の音の終わり − (今 + 最小の余裕)。既に並べてある音のうち、
        // 予約できる最も早い時刻より先に置かれている分である。詰めるとこの音の終わりが
        // 早くなり、次に並べる音の下限がその分だけ前へ動くため、余裕の範囲でだけ遅れを
        // 減らせる。余裕が 0 以下 (出力のバッファが下限を決めている) ときは、詰めても
        // 遅れは減らない
        let queue_slack_us = previous_end_us.map(|previous_end_us| {
            previous_end_us.saturating_sub(input.now_us.saturating_add(self.config.min_lead_us))
        });
        // 詰める量は「遅れ」「音の長さの半分」「キューの余裕」の最小値にする
        let compress_us = match queue_slack_us {
            Some(queue_slack_us) if queue_slack_us > 0 && lateness_us >= stretch_min_us => {
                lateness_us
                    .min(input.duration_us.max(0) / 2)
                    .min(queue_slack_us)
            }
            _ => 0,
        };
        self.requested_us = compress_us;
        self.last_end_us = Some(
            start_at_us
                .saturating_add(input.duration_us)
                .saturating_sub(compress_us),
        );
        self.last_timestamp_us = Some(input.timestamp_us);
        let concealment = self.concealment_of(input.now_us, previous_end_us, start_at_us);
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
            expand_us: 0,
            gap_start_us: concealment.gap_start_us,
            conceal_us: concealment.conceal_us,
            total_gap_us: concealment.total_gap_us,
            unrecoverable_gap_us: concealment.unrecoverable_gap_us,
        }
    }

    /// 実際に詰めた長さを記録する (呼び出し側が時間圧縮を適用した後に呼ぶ)
    ///
    /// 要求した量より多く詰めても要求までに切る。要求した量より少なくしか詰められな
    /// かったとき (波形が繰り返していないとき) は、詰められなかった分だけ音が後ろへ
    /// 伸び、遅れとして残る。到着基準の圧縮を確定したときは、削減した分だけ基準
    /// (timestamp と再生時刻の対応) も前へ戻す。
    pub fn confirm_stretch(&mut self, applied_us: i64) {
        let requested_us = self.requested_us;
        self.requested_us = 0;
        if requested_us == 0 {
            return;
        }
        // 要求を超えた詰めは切り捨てる (呼び出し側の推定より多く削られても、
        // 音の終わりを要求以上に前へ動かさない)
        let applied_us = applied_us.clamp(0, requested_us);
        if self.requested_by_arrival
            && let Some(anchor) = self.anchor.as_mut()
        {
            // 到着基準で詰めた分だけ、timestamp と再生時刻の対応も前へ戻す。戻さないと、
            // 詰めた分が次の音との穴になる
            anchor.time_us = anchor.time_us.saturating_sub(applied_us);
        }
        self.requested_by_arrival = false;
        self.compressed_us = self.compressed_us.saturating_add(applied_us);
        // 直前に鳴らすと決めた音の長さも、実際に詰めた長さで直す。詰められなかった分は
        // 音が後ろへ伸びる
        if let Some(last_play) = self.last_play.as_mut() {
            last_play.played_us = last_play
                .played_us
                .saturating_add(requested_us.saturating_sub(applied_us))
                .max(0);
        }
        if applied_us < requested_us
            && let Some(last_end_us) = self.last_end_us.as_mut()
        {
            // 詰められなかった分は音が後ろへ伸びる
            *last_end_us = last_end_us.saturating_add(requested_us - applied_us);
        }
    }

    /// 実際に伸ばした長さを記録する (呼び出し側が時間伸長を適用した後に呼ぶ)
    ///
    /// 要求した量より多く伸ばしても要求までに切る。基準と直前の音の終わりの両方が
    /// あるときだけ、伸ばした分だけ両方を後ろへ動かし、累積へ加える。直前に鳴らすと
    /// 決めた音 ([`AudioPlayoutScheduler::last_play`]) の長さにも伸ばした分を足す
    /// (moqt-js は伸長を適用した後の実フレーム数を計器へ渡すため)。
    pub fn confirm_expansion(&mut self, applied_us: i64) {
        let requested_us = self.requested_expansion_us;
        self.requested_expansion_us = 0;
        let applied_us = applied_us.clamp(0, requested_us.max(0));
        if let (Some(anchor), Some(last_end_us)) = (self.anchor.as_mut(), self.last_end_us.as_mut())
        {
            anchor.time_us = anchor.time_us.saturating_add(applied_us);
            *last_end_us = last_end_us.saturating_add(applied_us);
            self.expanded_us = self.expanded_us.saturating_add(applied_us);
            // 実際に鳴る長さにも伸ばした分を足す。moqt-js の `playSound` は伸長を適用した
            // 後の実フレーム数を `recordPlay` へ渡す
            if let Some(last_play) = self.last_play.as_mut() {
                last_play.played_us = last_play.played_us.saturating_add(applied_us);
            }
        }
    }

    /// 実際に補間した長さを記録する (呼び出し側が隙間を埋めた後に呼ぶ)
    ///
    /// 要求した長さより長い分は要求までに切る。0 のときは数えない (末尾の相関が足りない、
    /// 継ぎ目の段差が大きい、予約できないときは補間できていないため)。
    ///
    /// moqt-js の HEAD は、現行の出力側補間では要求と別に実際に描画した量を受け取る
    /// [`AudioPlayoutScheduler::record_rendered_concealment`] を使う。moqt-rs の補間は
    /// この要求から呼び出し側が作るため、この経路が「実際に補間した量」の経路になる。
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

    /// 出力側が実際に補間した量と、補間を始めた回数を反映する
    ///
    /// 要求を経由せずに出力側が補間した量を数える。負の長さは無視する。
    pub fn record_rendered_concealment(&mut self, concealed_us: i64, count: u64) {
        if concealed_us < 0 {
            return;
        }
        self.concealed_us = self.concealed_us.saturating_add(concealed_us);
        self.conceal_count = self.conceal_count.saturating_add(count);
    }

    /// サンプルへの丸めと開始の押し直しを含む、実際のバッファの終わりを確定する
    ///
    /// 直前の音の終わりがあるときだけ、0 以上の値で置き換える。直前に鳴らすと決めた音
    /// ([`AudioPlayoutScheduler::last_play`]) の長さも、確定した終端から開始までとして
    /// 直す。moqt-js の `playSound` が実際にバッファへ書いたフレーム数を `recordPlay` へ
    /// 渡すのと同じ結果になる。
    ///
    /// 呼び出し側は、時間伸長 (`confirm_expansion`) と時間圧縮 (`confirm_stretch`) を
    /// 適用してバッファへ書き終えた後に呼び、その後に
    /// [`AudioPlayoutScheduler::last_play`] を計器へ渡すこと。確定より先に
    /// `record_play` を呼ぶと、伸長した分と実際の終端が計器に入らない。
    pub fn confirm_playout_end(&mut self, end_us: i64) {
        if self.last_end_us.is_some() && end_us >= 0 {
            self.last_end_us = Some(end_us);
            if let Some(last_play) = self.last_play.as_mut() {
                last_play.played_us = end_us.saturating_sub(last_play.start_at_us).max(0);
            }
        }
    }

    /// 直前に鳴らすと決めた音の終わり (今と同じ軸のマイクロ秒)
    ///
    /// まだ一度も鳴らすと決めていなければ None。次に鳴らす音の隙間の起点であり、出力が
    /// 枯れているかどうかを呼び出し側が判定するのにも使う
    /// ([`AudioPlayoutScheduler::note_underrun`])。詰める長さを実際に適用した後
    /// ([`AudioPlayoutScheduler::confirm_stretch`]) に読むと、適用した長さで直った値に
    /// なる。予約した終端は [`AudioPlayoutScheduler::confirm_playout_end`] でも確定できる。
    /// 購読のやり直し ([`AudioPlayoutScheduler::reset`]) で消える。
    pub fn last_end_us(&self) -> Option<i64> {
        self.last_end_us
    }

    /// 出力が枯れていることを知らせる (枯れている間、呼び出し側が呼ぶ)
    ///
    /// 前に並べた音の終わり (`last_end_us`) が今より前 (出力が枯れている) ときは、終わりを
    /// 今へ進め、要求中の詰める長さ・補間の長さ・伸長の長さ・遅れを 0 にする。枯れた時間は
    /// 音として鳴っていないため、次の音の隙間として数え直すと、実際には無音だった時間を
    /// 補間で埋めようとする (二重に材料化する)。終わりを今へ進めることで、次の音の隙間は
    /// 今から並べた分だけになる。
    ///
    /// 枯れていないときは何もしない。枯れている間は毎回呼んでよい (呼ぶたびに終わりを
    /// 今へ進める)。計器へ枯れた長さを記録するのは呼び出し側の責務であり、枯れが明けた
    /// ときに 1 回だけ記録する
    /// ([`crate::playout::timing::AudioPlayoutTimingStats::record_underrun`])。
    pub fn note_underrun(&mut self, now_us: i64) {
        let Some(last_end_us) = self.last_end_us else {
            return;
        };
        if last_end_us >= now_us {
            return;
        }
        self.last_end_us = Some(now_us);
        self.requested_us = 0;
        self.requested_by_arrival = false;
        self.requested_conceal_us = 0;
        self.requested_expansion_us = 0;
        self.lateness_us = 0;
    }

    /// 基準と前の音の終わりと現在の遅れを消す (購読のやり直し)。累積統計は消さない
    pub fn reset(&mut self) {
        self.anchor = None;
        self.last_end_us = None;
        self.last_timestamp_us = None;
        self.last_play = None;
        self.lateness_us = 0;
        self.requested_us = 0;
        self.requested_by_arrival = false;
        self.requested_conceal_us = 0;
        self.requested_expansion_us = 0;
        self.arrival_depth_recovery = false;
    }

    /// 累積統計と現在の遅れを消す
    pub fn reset_stats(&mut self) {
        self.rebase_count = 0;
        self.drop_count = 0;
        self.compressed_us = 0;
        self.concealed_us = 0;
        self.conceal_count = 0;
        self.expanded_us = 0;
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

    /// 再同期後の深さの回復で実際に伸ばした合計 (マイクロ秒)
    pub fn expanded_us(&self) -> i64 {
        self.expanded_us
    }

    /// 目標に対して今どれだけ遅れているか (マイクロ秒)
    ///
    /// 目標ありの並べ方と到着基準の並べ方の両方で更新する。詰めた分は前の音の終わりが
    /// 早くなることで次の音へ引き継がれる (積み上げない)。
    pub fn lateness_us(&self) -> i64 {
        self.lateness_us
    }

    /// 直前に鳴らすと決めた音の値
    ///
    /// [`AudioPlayoutScheduler::schedule`] が [`AudioPlayoutDecision::Play`] を返したときに
    /// 更新する。捨てる決定では更新しない (直前に鳴らすと決めた音の値が残る)。
    ///
    /// 実際に鳴る長さ ([`AudioPlayoutPlay::played_us`]) は、実際に適用した詰める長さ
    /// ([`AudioPlayoutScheduler::confirm_stretch`]) と伸ばした長さ
    /// ([`AudioPlayoutScheduler::confirm_expansion`])、確定した出力の終端
    /// ([`AudioPlayoutScheduler::confirm_playout_end`]) をすべて返した後に読むこと。
    /// 計器へ渡す場合も同じ順序で、`record_play` は最後に呼ぶ。購読のやり直し
    /// ([`AudioPlayoutScheduler::reset`]) で消える。
    pub fn last_play(&self) -> Option<AudioPlayoutPlay> {
        self.last_play
    }

    /// 前の音の終わりとの差が下限 ([`AUDIO_PLAYOUT_MIN_CONCEAL_US`]) 以下なら、開始時刻を
    /// 前の音の終わりへ揃える
    ///
    /// 目標の表示時刻は、時間軸の基準 (直近 10 秒の最小値) の更新や丸めで、前のフレームの
    /// 目標から 20 µs〜0.5 ms だけ後ろへ動くことがある。その分を隙間として扱うと、補間にも
    /// フェードにもならない 1〜2 サンプル (20〜40 µs) のデジタル無音が残り、両側の段差が
    /// クリックになる。補間やフェードではなく、前の音の終わりに揃えて連続再生で吸収する
    /// (最大 0.5 ms 早く鳴るだけで、目標に対する遅れは無視できる)。
    ///
    /// 前の音の終わりが今 + 余裕よりわずかに過去のときも、この範囲なら前の音の終わりは
    /// まだ未来 (今 + 余裕 − 0.5 ms 以降) であり、予約できる。ただし今より過去へは戻さない
    /// (余裕を 0 に設定したときに、過去への予約にならないようにするため)。
    fn snap_to_previous_end(
        &self,
        start_at_us: i64,
        previous_end_us: Option<i64>,
        now_us: i64,
    ) -> i64 {
        let Some(previous_end_us) = previous_end_us else {
            return start_at_us;
        };
        let gap_us = start_at_us.saturating_sub(previous_end_us);
        if gap_us <= 0 || gap_us > AUDIO_PLAYOUT_MIN_CONCEAL_US {
            return start_at_us;
        }
        if previous_end_us < now_us {
            // 過去へは予約しない (時計の進み方によっては開始が即時になり重なる)
            return start_at_us;
        }
        previous_end_us
    }

    /// 前の音の終わりと今回の開始時刻の間から、補間する隙間を求める
    ///
    /// 隙間を「予約できる最も早い時刻より前の分 (`unrecoverable_gap_us`。既に過去の分と、
    /// 未来でも余裕に満たない分の両方を含む)」「予約できる分 (`conceal_us`、上限
    /// [`AUDIO_PLAYOUT_MAX_CONCEAL_US`] で切る)」「上限を超えて無音で残る分」に分ける。
    /// 下限 ([`AUDIO_PLAYOUT_MIN_CONCEAL_US`]) 以下の隙間は補間しない。
    fn concealment_of(
        &mut self,
        now_us: i64,
        previous_end_us: Option<i64>,
        start_at_us: i64,
    ) -> AudioPlayoutConcealment {
        let Some(previous_end_us) = previous_end_us else {
            return AudioPlayoutConcealment::default();
        };
        // 隙間が無ければ何も返さない。下限以下の隙間は schedule / schedule_by_arrival が
        // 前の音の終わりへ揃えるため通常はここへ届かないが、揃えられなかった場合 (前の音の
        // 終わりが今より過去で、過去へは予約しないため) は、実際に無音として出る。そのため
        // 下限以下でも全長と予約できない長さを返す
        let total_gap_us = start_at_us.saturating_sub(previous_end_us);
        if total_gap_us <= 0 {
            return AudioPlayoutConcealment::default();
        }
        // 補間バッファを予約できる最も早い時刻。前の音の終わりが今 + 余裕より前でも、
        // 今回の音の開始がその先なら、未来の分だけは補間できる
        let reservable_start_us =
            previous_end_us.max(now_us.saturating_add(self.config.min_lead_us));
        let unrecoverable_gap_us = reservable_start_us
            .saturating_sub(previous_end_us)
            .min(total_gap_us)
            .max(0);
        if total_gap_us <= AUDIO_PLAYOUT_MIN_CONCEAL_US {
            // 下限以下の隙間は補間しない。揃えられなかった分は無音として残る
            return AudioPlayoutConcealment {
                gap_start_us: None,
                conceal_us: 0,
                total_gap_us,
                unrecoverable_gap_us,
            };
        }
        // 予約できる区間 = 予約できる先頭から開始まで (上限で切る)
        let reservable_us = start_at_us
            .saturating_sub(reservable_start_us)
            .min(AUDIO_PLAYOUT_MAX_CONCEAL_US);
        if reservable_us <= AUDIO_PLAYOUT_MIN_CONCEAL_US {
            // 予約できる区間が下限以下。隙間はあるが補間しない
            return AudioPlayoutConcealment {
                gap_start_us: None,
                conceal_us: 0,
                total_gap_us,
                unrecoverable_gap_us,
            };
        }
        self.requested_conceal_us = reservable_us;
        AudioPlayoutConcealment {
            gap_start_us: Some(reservable_start_us),
            conceal_us: reservable_us,
            total_gap_us,
            unrecoverable_gap_us,
        }
    }

    /// 目標を使わないときの決め方
    ///
    /// 最初の音で基準を決め、TIMESTAMP の間隔どおりに並べる。過ぎてから届いた音は基準を
    /// 取り直し (ずらす幅が [`AUDIO_PLAYOUT_RESYNC_MIN_JUMP_US`] 未満のときは数えない)、
    /// 再同期の直後だけ時間伸長で深さを戻す。TIMESTAMP が大きく飛んだ音は前の音のすぐ
    /// 後ろと到着 + 再生の遅れの遅い方から並べ直し、並べすぎの音は捨てる。
    ///
    /// 並べる基準は `arrival_us` (到着した音がまだ鳴っていない位置) である。`now_us` は
    /// 既に出力のバッファへ積まれた分だけ先に進んでいるため、ここから遅れを数えると
    /// 実際に鳴るのは「到着 + 遅れ + バッファの分」になる。
    fn schedule_by_arrival(&mut self, input: AudioPlayoutInput) -> AudioPlayoutDecision {
        let limit_us = input
            .arrival_delay_us
            .saturating_add(self.config.backlog_us);
        let previous_end_us = self.last_end_us;
        // 予約できる最も早い時刻。到着から再生の遅れだけ後ろに置いた時刻がこれより前なら、
        // その時刻にはもう予約できない (出力のバッファが先に進んでいる)
        let earliest_us = input.now_us.saturating_add(self.config.min_lead_us);
        let mut start_at_us =
            self.expected_start_at(input.timestamp_us, input.arrival_us, input.arrival_delay_us);
        if start_at_us < earliest_us {
            // 過ぎてから届いた (前の音はすべて今より前に終わっているため重ならない)。今から
            // 鳴らせる最も早い時刻へずらす。ずらす幅が無視できるときは基準を取り直さない
            // (取り直すと媒体時刻が跳び、その回数を「並べ直し」として数えてしまう)。
            // 既に待ちを使い切った音へ目標の深さを再度足さない。代わりに後続の音を時間
            // 伸長して深さを戻す
            let resynced_us = earliest_us;
            if resynced_us.saturating_sub(start_at_us) >= AUDIO_PLAYOUT_RESYNC_MIN_JUMP_US {
                self.rebase(resynced_us, input.timestamp_us);
                self.arrival_depth_recovery = true;
            }
            start_at_us = resynced_us;
        } else if start_at_us > input.now_us.saturating_add(limit_us) {
            // TIMESTAMP が大きく飛んだ。前の音のすぐ後ろ (か到着 + 再生の遅れ) から並べ直す
            let rebase_us = input
                .arrival_us
                .saturating_add(input.arrival_delay_us)
                .max(earliest_us)
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
        start_at_us = self.snap_to_previous_end(start_at_us, previous_end_us, input.now_us);
        // 停滞で後ろへ動かした基準は、経路が回復しても自然には戻らない。到着 + 目標の
        // 深さより余分に溜まった分だけを、既存の不感帯とキューの余裕の中で圧縮する。
        // 隙間を挟む音は詰めない。確定した量は confirm_stretch で基準へ反映する
        let arrival_target_us = input.arrival_us.saturating_add(input.arrival_delay_us);
        self.lateness_us = start_at_us.saturating_sub(arrival_target_us).max(0);
        let queue_slack_us =
            previous_end_us.map(|previous_end_us| previous_end_us.saturating_sub(earliest_us));
        let compress_us = match (previous_end_us, queue_slack_us) {
            (Some(previous_end_us), Some(queue_slack_us))
                if start_at_us <= previous_end_us
                    && queue_slack_us > 0
                    && self.lateness_us >= self.config.stretch_min_us =>
            {
                self.lateness_us
                    .min(input.duration_us.max(0) / 2)
                    .min(queue_slack_us)
            }
            _ => 0,
        };
        self.requested_us = compress_us;
        self.requested_by_arrival = compress_us > 0;
        // 再開は遅らせず、伸長できる波形だけで目標の深さへ戻す。圧縮とは同時に要求しない
        if start_at_us >= arrival_target_us {
            self.arrival_depth_recovery = false;
        }
        let expand_us = if self.arrival_depth_recovery {
            arrival_target_us
                .saturating_sub(start_at_us)
                .max(0)
                .min(input.duration_us.max(0))
        } else {
            0
        };
        self.requested_expansion_us = expand_us;
        self.last_end_us = Some(
            start_at_us
                .saturating_add(input.duration_us)
                .saturating_sub(compress_us),
        );
        self.last_timestamp_us = Some(input.timestamp_us);
        let concealment = self.concealment_of(input.now_us, previous_end_us, start_at_us);
        self.remember_play(&input, start_at_us, compress_us, AudioPlayoutBasis::Arrival);
        AudioPlayoutDecision::Play {
            start_at_us,
            basis: AudioPlayoutBasis::Arrival,
            compress_us,
            expand_us,
            gap_start_us: concealment.gap_start_us,
            conceal_us: concealment.conceal_us,
            total_gap_us: concealment.total_gap_us,
            unrecoverable_gap_us: concealment.unrecoverable_gap_us,
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
    /// 今回の開始の間の隙間として返す (呼び出し側が上限まで補間できる)。この経路では前の
    /// 音の終わりに揃えない (前の音は既に鳴り終わっている前提であり、揃えると過去への
    /// 予約になるため)。
    fn rebase_by_arrival(&mut self, input: AudioPlayoutInput) -> AudioPlayoutDecision {
        let previous_end_us = self.last_end_us;
        // 到着から再生の遅れだけ後ろへ置く (到着した音がまだ鳴っていない位置から数える)。
        // 前の音の終わりより前には鳴らさない (重ねない)
        let start_at_us = input
            .arrival_us
            .saturating_add(input.arrival_delay_us)
            .max(input.now_us.saturating_add(self.config.min_lead_us))
            .max(previous_end_us.unwrap_or(i64::MIN));
        self.rebase(start_at_us, input.timestamp_us);
        self.last_end_us = Some(start_at_us.saturating_add(input.duration_us));
        self.last_timestamp_us = Some(input.timestamp_us);
        // 到着基準へ移ったため、目標に対する遅れは無い (詰めない)
        self.lateness_us = 0;
        self.requested_us = 0;
        self.requested_by_arrival = false;
        self.requested_expansion_us = 0;
        let concealment = self.concealment_of(input.now_us, previous_end_us, start_at_us);
        self.remember_play(&input, start_at_us, 0, AudioPlayoutBasis::Arrival);
        AudioPlayoutDecision::Play {
            start_at_us,
            basis: AudioPlayoutBasis::Arrival,
            compress_us: 0,
            expand_us: 0,
            gap_start_us: concealment.gap_start_us,
            conceal_us: concealment.conceal_us,
            total_gap_us: concealment.total_gap_us,
            unrecoverable_gap_us: concealment.unrecoverable_gap_us,
        }
    }

    /// 基準と前の音から、この音を鳴らす時刻を求める (前の音の終わりより前にしない)
    ///
    /// `arrival_us` は到着した音がまだ鳴っていない位置である。最初の音の基準は、この位置
    /// から到着基準の再生の遅れ (`arrival_delay_us`) だけ後ろへ置いた時刻になる。映像の
    /// 購読を解除して時刻基準から初めて移るときも、先行予約を追い越さないようにする。
    fn expected_start_at(
        &mut self,
        timestamp_us: i64,
        arrival_us: i64,
        arrival_delay_us: i64,
    ) -> i64 {
        let Some(anchor) = self.anchor else {
            let time_us = arrival_us
                .saturating_add(arrival_delay_us)
                .max(self.last_end_us.unwrap_or(i64::MIN));
            self.anchor = Some(PlayoutAnchor {
                time_us,
                timestamp_us,
            });
            return time_us;
        };
        let advanced = self
            .last_timestamp_us
            .is_some_and(|last_timestamp_us| timestamp_us > last_timestamp_us);
        let by_timestamp_us = anchor
            .time_us
            .saturating_add(timestamp_us.saturating_sub(anchor.timestamp_us));
        let after_previous_us = self.last_end_us.unwrap_or(by_timestamp_us);
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
