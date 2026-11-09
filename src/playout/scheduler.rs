//! 復号した音声を鳴らす時刻を決める
//!
//! 予約を「計画」と「確定」に分ける。計画 ([`AudioPlayoutScheduler::schedule`]) は音を
//! 鳴らす時刻と、詰める・伸ばす長さと、前の音との隙間だけを返し、状態を進めない。呼び出し
//! 側が実際に PCM を加工して出力へ積めた後に、実際の開始位置と長さで確定
//! ([`AudioPlayoutScheduler::commit`]) する。加工や投入に失敗した計画は次の計画で失効し、
//! 予約位置・時間伸縮・再同期の実績を巻き戻す必要が無い。
//!
//! 時刻基準 (TIMESTAMP から目標の時刻が決まるとき) と到着基準は、目標の作り方だけを変え、
//! 予約の上限・前の音との接続・圧縮と伸長の選択は同じ規則を使う。どちらの基準でも、遅れた
//! という理由だけで音は捨てない。未加工の終端が予約の上限を超える場合だけ捨てる。
//!
//! 時刻はすべて呼び出し側が引数で渡し、デバイス API やタイマーには触れない。単位は i64 の
//! マイクロ秒である。moqt-js は秒の浮動小数で計算するが、本実装はマイクロ秒の整数で計算し、
//! 判定に影響しない 1 マイクロ秒未満の差だけを丸め誤差として許す。
//!
//! moqt-js の `src/playout/audioPlayout.ts` (HEAD `f6a171e8`) に対応する。moqt-js は出力側の
//! `AudioWorklet` が実際の欠落を検出して補間するため、計画に補間の予約位置を持たない。
//! moqt-rs の出力 (`raw_player`) は FIFO であり、隙間を書き込みの前に材料化しないと後続が
//! 早く鳴って出力が枯れる。そのため計画には、材料化に使う隙間の全長と、補間してよい長さと、
//! 予約できなかった前置きの長さを moqt-rs 側の追加情報として持たせる。

/// 再生の遅れの下限 (マイクロ秒)
///
/// 揺らぎから求めた遅れがまだ無いときの値。呼び出し側が `delay_us` へ渡す。共有の時間軸が
/// 使う下限 ([`crate::playout::timeline::TIMELINE_AUDIO_DELAY_FLOOR_US`]) と同じ値であり、
/// 定義は時間軸側に置く (揺らぎから求めた遅れをその値で下限にするため)。ここではその定義を
/// そのまま使う。時間軸の設定で下限を変えた場合、呼び出し側は
/// [`crate::playout::timeline::PlayoutTimeline::config`] の
/// [`crate::playout::timeline::TimelineConfig::audio_min_delay_us`] を使う。
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
///
/// moqt-js の `AUDIO_PLAYOUT_MIN_LEAD_SECONDS` に対応する。描画量子とメッセージの配送の
/// ための先行量であり、実際に投入する直前にも時計を読み直す。moqt-rs の出力は FIFO で
/// あり、この値は「今からこれだけ先に置けば積める」という下限として使う。
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
/// 到着基準へ並べ直さず、遅れたまま鳴らし続ける ([`AudioPlayoutScheduler::schedule`] を
/// 参照)。
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
/// 前の音の終わりへ揃えて穴を作らない。
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
    /// 共有の時間軸が学習した再生の遅れ (揺らぎから求めた音声の遅れ)
    ///
    /// 並べすぎの上限 ([`AUDIO_PLAYOUT_BACKLOG_US`] を足す相手) に使う。到着基準で鳴らす
    /// ときの遅れは [`AudioPlayoutInput::arrival_delay_us`] を使う。
    pub delay_us: i64,
    /// 到着基準で鳴らすときの再生の遅れ (マイクロ秒)
    ///
    /// 呼び出し側が
    /// [`crate::playout::timeline::audio_arrival_delay_us_with_min`] で求めた値 (下限は
    /// 時間軸の設定、上限は [`crate::playout::timeline::TIMELINE_ARRIVAL_DELAY_US`]) を
    /// 渡す。共有の時間軸が学習した遅れをそのまま渡すと、TIMESTAMP が壁時計からずれて
    /// いるトラックではそのずれの分だけ大きく育った値で鳴ってしまう。
    pub arrival_delay_us: i64,
    /// 表示の遅れ (表示時刻に使う遅れ。時間軸が渡す)
    pub presentation_delay_us: i64,
    /// 目標ごとの圧縮の不感帯 (マイクロ秒)
    ///
    /// 呼び出し側が時間軸の
    /// [`crate::playout::timeline::PlayoutTimeline::compression_deadband_us`] で求めた値を
    /// 渡す。時刻基準と到着基準の両方が使い、`None` のときは scheduler の既定値
    /// ([`AUDIO_PLAYOUT_STRETCH_MIN_US`]) を使う。低遅延の指定があるときは、停止後の
    /// 余分な待ちを固定しないよう、呼び出し側が下限の半分 (最小 5 ms、最大 20 ms) に
    /// 抑えた値を渡す。
    pub stretch_min_us: Option<i64>,
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
/// [`AudioPlayoutScheduler::commit`] が返す。音声の再生の観測値
/// ([`crate::playout::timing::AudioPlayoutTimingStats`]) へ渡す値であり、予定に対する
/// 余裕 (再生予定時刻 - 到着) と到着から鳴り始めるまでの時間 (鳴り始める時刻 - 到着) は
/// ここから求められる。長さは実際に出力へ積んだ PCM の長さであり、時間圧縮と時間伸長を
/// 適用した後の値である。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPlayoutPlay {
    /// 到着した音がまだ鳴っていない位置 ([`AudioPlayoutInput::arrival_us`])
    pub arrival_us: i64,
    /// 目標の開始時刻 ([`AudioPlayoutInput::target_start_us`])。無いこともある
    pub target_start_us: Option<i64>,
    /// 実際に鳴り始める時刻
    pub start_at_us: i64,
    /// 実際に鳴る長さ (0 より大きい)
    pub played_us: i64,
    /// 鳴らす時刻を決めるのに使った計画
    pub basis: AudioPlayoutBasis,
}

/// PCM を加工して出力へ積んだ後に、計画を確定する値 (すべてマイクロ秒)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPlayoutCommit {
    /// 実際に鳴り始める時刻。計画した開始時刻より前にはできない
    ///
    /// 呼び出し側が出力の空きに合わせて開始を後ろへ押し直したときは、押し直した後の値
    /// (実際に積んだ位置) を渡す。時間伸長を適用した場合は、伸ばした分の先頭が計画より
    /// 前になることがあるため、押し直した後の値を渡すこと。
    pub start_at_us: i64,
    /// 実際に鳴る長さ (0 より大きい)
    ///
    /// 時間圧縮や時間伸長を適用した後の、実際に積んだ PCM の長さである。計画の長さとの
    /// 差が、詰めた量・伸ばした量として累積へ入る。
    pub played_us: i64,
}

/// 計画を確定できなかった理由
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlayoutCommitError {
    /// 確定できる計画が無い (まだ計画していない、次の計画で失効した、または確定済み)
    NoPendingPlan,
    /// 実際の開始時刻が計画より前である
    StartBeforePlan,
    /// 実際に鳴る長さが 0 以下である
    EmptyPlayed,
}

/// 鳴らす時刻の計画
///
/// [`AudioPlayoutScheduler::schedule`] が返す。計画の内容は [`AudioPlayoutPlan::decision`] で
/// 読み、実際に出力へ積んだ後に [`AudioPlayoutPlan`] をそのまま
/// [`AudioPlayoutScheduler::commit`] へ渡す。計画ごとに識別子を持つため、確定する前に次の
/// 音を計画した場合や、やり直した場合、同じ計画を二重に確定した場合は、古い計画では
/// 確定できない (計画の取り違えを防ぐ)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPlayoutPlan {
    /// 計画の識別子。schedule のたびに増える
    sequence: u64,
    /// 計画の内容
    decision: AudioPlayoutDecision,
}

impl AudioPlayoutPlan {
    /// 計画の内容 (鳴らす時刻・詰める長さ・隙間、または捨てる)
    pub fn decision(&self) -> AudioPlayoutDecision {
        self.decision
    }
}

/// 鳴らす時刻と詰める長さと隙間 (マイクロ秒)、または捨てる
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlayoutDecision {
    /// 鳴らす計画。実行して [`AudioPlayoutScheduler::commit`] で確定する
    Play {
        /// 鳴らす時刻 (今と同じ軸のマイクロ秒)
        start_at_us: i64,
        /// 鳴らす時刻を決めるのに使った計画。到着基準 ([`AudioPlayoutBasis::Arrival`]) の
        /// ときは、時間軸の再生予定時刻に従っていない (予定そのものは観測値として渡してよい)
        basis: AudioPlayoutBasis,
        /// 波形の周期で詰めてよい長さ (マイクロ秒)。0 なら詰めない
        ///
        /// 遅れと音の長さの半分とキューの余裕 (直前の音の終わり − (今 +
        /// [`AUDIO_PLAYOUT_MIN_LEAD_US`])) の最小値であり、キューの余裕が 0 以下のときと、
        /// 隙間を挟む到着基準の音では 0 である。実際に詰められるかは波形しだいであり、
        /// 詰めた後に、実際に積んだ長さで [`AudioPlayoutScheduler::commit`] を呼ぶ。
        compress_us: i64,
        /// 到着基準の深さを戻すために伸長してよい長さ (マイクロ秒)。0 なら伸長しない
        ///
        /// 到着基準で、予定が到着 + 再生の遅れより不感帯以上に前になっているときに要求する。
        /// 実際に伸ばした後に、伸ばした長さで [`AudioPlayoutScheduler::commit`] を呼ぶ。
        expand_us: i64,
        /// 前の音の終わりから今回の開始までの隙間の全長 (マイクロ秒)。0 なら隙間が無い
        ///
        /// 下限でも上限でも切り捨てない。呼び出し側が隙間を必ず材料化する
        /// ([`crate::playout::gap::materialize`]) ために、材料化する長さそのものを返す。
        total_gap_us: i64,
        /// 隙間のうち、補間してよい長さ (マイクロ秒)。0 なら補間しない
        ///
        /// 予約できる最も早い時刻から今回の開始までを、上限
        /// ([`AUDIO_PLAYOUT_MAX_CONCEAL_US`]) で切った値である。moqt-rs の出力は FIFO
        /// であり、材料化するのは全長 ([`AudioPlayoutDecision::Play::total_gap_us`]) だが、
        /// 続き音で埋めるのはこの長さまでにする。
        conceal_us: i64,
        /// 隙間のうち、予約できる最も早い時刻より前で補間できない長さ (マイクロ秒)
        ///
        /// 既に過去の分と、未来でも余裕に満たない分を含む。moqt-rs の出力は FIFO であり、
        /// 隙間は先頭から連続して材料化できるため、材料化の置き場所には使わない。計器に
        /// 残すときだけ使う。
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
    /// 目標ごとの指定が無いときの圧縮の不感帯 (マイクロ秒)
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

/// 確定待ちの計画
#[derive(Debug, Clone, Copy)]
struct PendingPlayout {
    /// 計画の識別子
    sequence: u64,
    /// 計画 (呼び出し側へ返した値)
    decision: AudioPlayoutDecision,
    /// 到着した音がまだ鳴っていない位置
    arrival_us: i64,
    /// 入力の目標の開始時刻
    target_start_us: Option<i64>,
    /// 音の TIMESTAMP
    timestamp_us: i64,
    /// 計画に使った音の長さ (0 以上)
    duration_us: i64,
    /// 目標にした時刻 (timestamp 経路なら目標の開始、到着基準なら到着 + 遅れ)
    target_us: i64,
    /// この計画で基準を取り直したか (確定したときだけ回数へ入れる)
    rebased: bool,
}

/// 音を鳴らす時刻を決める (音声トラックごとに 1 つ持つ)
///
/// [`AudioPlayoutScheduler::schedule`] にいま届いた音の情報を渡し、返った計画を実行して、
/// 実際に出力へ積んだ開始位置と長さで [`AudioPlayoutScheduler::commit`] を呼ぶ。計画だけを
/// 作って確定しなかった場合、予約位置も実績も変わらない。
///
/// moqt-rs の出力は FIFO であり、出力が枯れている間は前の音の終わりを今へ進める必要が
/// ある。そのため moqt-js には無い [`AudioPlayoutScheduler::note_underrun`] を持つ。
pub struct AudioPlayoutScheduler {
    /// 設定
    config: AudioPlayoutConfig,
    /// 到着基準で並べるときの、TIMESTAMP と再生時刻の対応
    anchor: Option<PlayoutAnchor>,
    /// 直前に確定した音の終わり
    last_end_us: Option<i64>,
    /// 確定待ちの計画。次の計画・確定・やり直しで失効する
    pending: Option<PendingPlayout>,
    /// 次に返す計画の識別子
    next_sequence: u64,
    /// 目標に対して今どれだけ遅れているか
    lateness_us: i64,
    rebase_count: u64,
    drop_count: u64,
    compressed_us: i64,
    expanded_us: i64,
    /// 出力側が実際に補間した合計
    concealed_us: i64,
    /// 出力側が補間を始めた回数
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
            pending: None,
            next_sequence: 0,
            lateness_us: 0,
            rebase_count: 0,
            drop_count: 0,
            compressed_us: 0,
            expanded_us: 0,
            concealed_us: 0,
            conceal_count: 0,
        }
    }

    /// 使っている設定
    pub fn config(&self) -> AudioPlayoutConfig {
        self.config
    }

    /// 音を鳴らす時刻の計画を作る
    ///
    /// `target_start_us` が無い、または `enforce_target` が false のときは到着基準の
    /// 並べ方を使う。目標から離れすぎた音も捨てず、直前の音がまだ鳴っている
    /// (`last_end_us > now_us`) 間は遅れたまま鳴らし続け、音が途切れているときだけ到着
    /// 基準へ並べ直す (並べ直すと媒体時刻が跳び、鳴っている音の続きが前へずれるため)。
    ///
    /// 返した計画は、次の [`AudioPlayoutScheduler::schedule`]・
    /// [`AudioPlayoutScheduler::commit`]・[`AudioPlayoutScheduler::reset`] のどれかで
    /// 失効する。計画を作っただけでは予約位置も統計も変わらない。
    ///
    /// 予約の上限は、時刻基準では「学習した遅れ」と「表示の遅れ」の大きい方に
    /// [`AudioPlayoutConfig::backlog_us`] を足した値、到着基準では到着基準の遅れに同じ
    /// 余裕を足した値である。時間圧縮できるかどうかに関係なく、未加工の音の終端がこの
    /// 上限を超える音は捨てる。上限を「実際に詰めた後」ではなく「詰める前」で見るのは、
    /// 圧縮が成立しない波形でも保持量が増えすぎないようにするためである。
    pub fn schedule(&mut self, input: AudioPlayoutInput) -> AudioPlayoutPlan {
        // 前の計画は実行されなかったものとして失効させる。計画は実投入の確定後だけ進める
        self.pending = None;
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        let previous_end_us = self.last_end_us;
        let duration_us = input.duration_us.max(0);
        let earliest_us = input
            .now_us
            .saturating_add(self.config.min_lead_us)
            .max(previous_end_us.unwrap_or(i64::MIN));
        // 到着した音がまだ鳴っていない位置から、到着基準の再生の遅れだけ後ろに置いた時刻
        let arrival_target_us = input.arrival_us.saturating_add(input.arrival_delay_us);

        // 目標を守れるなら時刻基準、守れないなら到着基準で始める
        let mut basis = if input.enforce_target && input.target_start_us.is_some() {
            AudioPlayoutBasis::Timestamp
        } else {
            AudioPlayoutBasis::Arrival
        };
        // 並べすぎの上限。時刻基準では表示時刻に使う遅れ、到着基準では到着の遅れを使う
        let mut limit_us = match basis {
            AudioPlayoutBasis::Timestamp => input
                .delay_us
                .max(input.presentation_delay_us)
                .saturating_add(self.config.backlog_us),
            AudioPlayoutBasis::Arrival => input
                .arrival_delay_us
                .saturating_add(self.config.backlog_us),
        };

        let mut rebased = false;
        let desired_us = match basis {
            AudioPlayoutBasis::Timestamp => {
                let target_start_us = input.target_start_us.expect("分岐で確認済み");
                // 共有時刻が大きく外れ、出力も途切れていた場合だけ到着の基準を取り直す
                if earliest_us.saturating_sub(target_start_us) > AUDIO_PLAYOUT_MAX_LATENESS_US
                    && previous_end_us.is_none_or(|previous_end_us| previous_end_us <= input.now_us)
                {
                    basis = AudioPlayoutBasis::Arrival;
                    limit_us = input
                        .arrival_delay_us
                        .saturating_add(self.config.backlog_us);
                    rebased = true;
                    arrival_target_us
                } else {
                    target_start_us
                }
            }
            AudioPlayoutBasis::Arrival => {
                // TIMESTAMP が進んだ音は、基準からの差でそのまま並べる。戻った音は前の音の
                // すぐ後ろに置く (媒体時刻が巻き戻っても前の音へ重ねない)
                let desired_us = match self.anchor {
                    None => arrival_target_us,
                    Some(anchor) if input.timestamp_us > anchor.timestamp_us => anchor
                        .time_us
                        .saturating_add(input.timestamp_us.saturating_sub(anchor.timestamp_us)),
                    Some(_) => previous_end_us.unwrap_or(arrival_target_us),
                };
                if desired_us > input.now_us.saturating_add(limit_us) {
                    // TIMESTAMP が大きく飛んだ。確定するまでは基準を変更しない
                    rebased = true;
                    arrival_target_us
                } else {
                    desired_us
                }
            }
        };
        if basis == AudioPlayoutBasis::Arrival
            && input
                .now_us
                .saturating_add(self.config.min_lead_us)
                .saturating_sub(desired_us)
                >= AUDIO_PLAYOUT_RESYNC_MIN_JUMP_US
        {
            // 予約できる最も早い時刻が予定より 10 ms 以上先である。遅れて届いた音として
            // 数え、確定したら再同期の回数へ入れる
            rebased = true;
        }

        // 予定が過去でも、今から積める最も早い時刻へずらして鳴らす。音は捨てない
        let mut start_at_us = desired_us.max(earliest_us);
        // 前の音の終わりとの差が下限以下なら、その終わりへ揃えて 1〜2 サンプルの無音を
        // 作らない。前の音の終わりが今より過去なら、過去への予約になるため揃えない
        if let Some(previous_end_us) = previous_end_us
            && previous_end_us >= input.now_us
            && start_at_us.saturating_sub(previous_end_us) <= AUDIO_PLAYOUT_MIN_CONCEAL_US
        {
            start_at_us = previous_end_us;
        }
        // 圧縮の成否に関係なく、未加工の終端で上限を検査する
        if start_at_us
            .saturating_add(duration_us)
            .saturating_sub(input.now_us)
            > limit_us
        {
            self.drop_count += 1;
            return AudioPlayoutPlan {
                sequence,
                decision: AudioPlayoutDecision::Drop {
                    reason: AudioPlayoutDropReason::Backlog,
                },
            };
        }

        let total_gap_us = previous_end_us.map_or(0, |previous_end_us| {
            start_at_us.saturating_sub(previous_end_us).max(0)
        });
        // 目標にした時刻。時刻基準は時間軸が決めた予定、到着基準は到着 + 再生の遅れ
        let target_us = match basis {
            AudioPlayoutBasis::Timestamp => desired_us,
            AudioPlayoutBasis::Arrival => arrival_target_us,
        };
        let error_us = start_at_us.saturating_sub(target_us);
        // キューの余裕 = 直前の音の終わり − (今 + 最小の余裕)。既に並べてある音のうち、
        // 予約できる最も早い時刻より先に置かれている分である。詰めるとこの音の終わりが
        // 早くなり、次に並べる音の下限がその分だけ前へ動く
        let slack_us = previous_end_us.map_or(0, |previous_end_us| {
            previous_end_us
                .saturating_sub(input.now_us)
                .saturating_sub(self.config.min_lead_us)
                .max(0)
        });
        let deadband_us = input.stretch_min_us.unwrap_or(self.config.stretch_min_us);
        // 詰める量は「目標からの遅れ」「音の長さの半分」「キューの余裕」の最小値にする。
        // 到着基準では、隙間を挟む音 (遅れて届いた音) は詰めない。隙間を埋める音と重なると
        // 継ぎ目が崩れるためである
        let compress_us = if error_us >= deadband_us
            && slack_us > 0
            && (basis == AudioPlayoutBasis::Timestamp || total_gap_us == 0)
        {
            error_us.min(duration_us / 2).min(slack_us).max(0)
        } else {
            0
        };
        // 浅いキューは停止を挟まずに伸ばす。同じ誤差の符号で圧縮と伸長を排他的に選ぶ。
        // 伸ばせる量は、詰めすぎて上限を超えない範囲にする
        let expand_us = if basis == AudioPlayoutBasis::Arrival && error_us < -deadband_us {
            error_us.saturating_neg().min(duration_us).min(
                input
                    .now_us
                    .saturating_add(limit_us)
                    .saturating_sub(start_at_us)
                    .saturating_sub(duration_us)
                    .max(0),
            )
        } else {
            0
        };
        let (total_gap_us, conceal_us, unrecoverable_gap_us) =
            self.concealment_of(input.now_us, previous_end_us, start_at_us, total_gap_us);
        let decision = AudioPlayoutDecision::Play {
            start_at_us,
            basis,
            compress_us,
            expand_us,
            total_gap_us,
            conceal_us,
            unrecoverable_gap_us,
        };
        self.pending = Some(PendingPlayout {
            sequence,
            decision,
            arrival_us: input.arrival_us,
            target_start_us: input.target_start_us,
            timestamp_us: input.timestamp_us,
            duration_us,
            target_us,
            rebased,
        });
        AudioPlayoutPlan { sequence, decision }
    }

    /// 計画を、実際に出力へ積んだ開始位置と長さで確定する
    ///
    /// 呼び出し側は、時間圧縮・時間伸長・サンプルへの丸めを適用して出力へ積んだ後に呼ぶ。
    /// この時点で初めて、予約位置・TIMESTAMP と再生時刻の対応・累積統計が進む。開始位置が
    /// 計画より前、長さが 0 以下、または確定できる計画が無いときは、何も変えずにエラーを
    /// 返す。確定できる計画が無いのは、計画した後に別の音を計画したか、やり直したか、
    /// 既に確定した場合である。
    ///
    /// 予約する時刻を押し直した場合は、押し直した後の開始位置を渡すこと。押し直した分は
    /// 次に並べる音の下限へ引き継がれる。
    pub fn commit(
        &mut self,
        plan: AudioPlayoutPlan,
        actual: AudioPlayoutCommit,
    ) -> Result<AudioPlayoutPlay, AudioPlayoutCommitError> {
        let Some(pending) = self
            .pending
            .as_ref()
            .filter(|pending| pending.sequence == plan.sequence)
        else {
            return Err(AudioPlayoutCommitError::NoPendingPlan);
        };
        if actual.played_us <= 0 {
            return Err(AudioPlayoutCommitError::EmptyPlayed);
        }
        if actual.start_at_us < pending.decision_start_at_us() {
            return Err(AudioPlayoutCommitError::StartBeforePlan);
        }
        let pending = self.pending.take().expect("直前に確認済み");
        let last_end_us = actual.start_at_us.saturating_add(actual.played_us);
        self.last_end_us = Some(last_end_us);
        // 今の媒体先頭に、加工後の長さの差を加えた一点を持つ。次の媒体位置からの差分だけで、
        // 圧縮・伸長・投入時の押し直しと本当の媒体欠落を同時に表せる
        self.anchor = Some(PlayoutAnchor {
            time_us: last_end_us.saturating_sub(pending.duration_us),
            timestamp_us: pending.timestamp_us,
        });
        self.compressed_us = self
            .compressed_us
            .saturating_add(pending.duration_us.saturating_sub(actual.played_us).max(0));
        self.expanded_us = self
            .expanded_us
            .saturating_add(actual.played_us.saturating_sub(pending.duration_us).max(0));
        self.lateness_us = actual.start_at_us.saturating_sub(pending.target_us).max(0);
        if pending.rebased {
            self.rebase_count = self.rebase_count.saturating_add(1);
        }
        Ok(AudioPlayoutPlay {
            arrival_us: pending.arrival_us,
            target_start_us: pending.target_start_us,
            start_at_us: actual.start_at_us,
            played_us: actual.played_us,
            basis: match pending.decision {
                AudioPlayoutDecision::Play { basis, .. } => basis,
                AudioPlayoutDecision::Drop { .. } => {
                    unreachable!("確定待ちの計画は Play だけである")
                }
            },
        })
    }

    /// 出力側が実際に補間した量と、補間を始めた回数を反映する
    ///
    /// 要求を経由せずに出力側が補間した量を数える。負の長さは無視する。moqt-rs では
    /// 隙間を出力へ書き込む前に材料化するため、実際に材料化できた長さを渡す。
    pub fn record_rendered_concealment(&mut self, concealed_us: i64, count: u64) {
        if concealed_us < 0 {
            return;
        }
        self.concealed_us = self.concealed_us.saturating_add(concealed_us);
        self.conceal_count = self.conceal_count.saturating_add(count);
    }

    /// 直前に確定した音の終わり (今と同じ軸のマイクロ秒)
    ///
    /// まだ一度も確定していなければ None。次に鳴らす音の隙間の起点であり、出力が枯れて
    /// いるかどうかを呼び出し側が判定するのにも使う
    /// ([`AudioPlayoutScheduler::note_underrun`])。購読のやり直し
    /// ([`AudioPlayoutScheduler::reset`]) で消える。
    pub fn last_end_us(&self) -> Option<i64> {
        self.last_end_us
    }

    /// 出力が枯れていることを知らせる (枯れている間、呼び出し側が呼ぶ)
    ///
    /// 前に確定した音の終わり (`last_end_us`) が今より前 (出力が枯れている) ときは、終わりを
    /// 今へ進め、現在の遅れを 0 にする。枯れた時間は音として鳴っていないため、次の音の
    /// 隙間として数え直すと、実際には無音だった時間を補間で埋めようとする (二重に材料化
    /// する)。終わりを今へ進めることで、次の音の隙間は今から並べた分だけになる。
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
        self.lateness_us = 0;
    }

    /// 基準と前の音の終わりと確定待ちの計画を消す (購読のやり直し)。累積統計は消さない
    pub fn reset(&mut self) {
        self.anchor = None;
        self.last_end_us = None;
        self.pending = None;
        self.lateness_us = 0;
    }

    /// 累積統計と現在の遅れを消す
    pub fn reset_stats(&mut self) {
        self.rebase_count = 0;
        self.drop_count = 0;
        self.compressed_us = 0;
        self.expanded_us = 0;
        self.concealed_us = 0;
        self.conceal_count = 0;
        self.lateness_us = 0;
    }

    /// 基準を取り直した回数
    ///
    /// 計画しただけでは増えない。確定したときに、その計画が基準を取り直していた場合だけ
    /// 増える。
    pub fn rebases(&self) -> u64 {
        self.rebase_count
    }

    /// 捨てた音の数
    pub fn drops(&self) -> u64 {
        self.drop_count
    }

    /// 実際に詰めた合計 (マイクロ秒)
    ///
    /// 計画の長さと確定した長さの差のうち、短くなった分である。要求した量ではなく、実際に
    /// 鳴る長さの差だけを数える。
    pub fn compressed_us(&self) -> i64 {
        self.compressed_us
    }

    /// 実際に伸ばした合計 (マイクロ秒)
    ///
    /// 計画の長さと確定した長さの差のうち、長くなった分である。
    pub fn expanded_us(&self) -> i64 {
        self.expanded_us
    }

    /// 実際に補間した合計 (マイクロ秒)
    pub fn concealed_us(&self) -> i64 {
        self.concealed_us
    }

    /// 実際に補間した回数 (補間を始めた音の数)
    pub fn concealments(&self) -> u64 {
        self.conceal_count
    }

    /// 目標に対して今どれだけ遅れているか (マイクロ秒)
    ///
    /// 確定した音の開始位置と目標の差である。時刻基準と到着基準の両方で更新する。
    pub fn lateness_us(&self) -> i64 {
        self.lateness_us
    }

    /// 前の音の終わりと今回の開始の間から、隙間の全長と補間してよい長さを求める
    ///
    /// 隙間を「材料化する全長」「補間してよい長さ (`conceal_us`、上限
    /// [`AUDIO_PLAYOUT_MAX_CONCEAL_US`] で切る)」「予約できなかった前置き」に分ける。
    /// 下限 ([`AUDIO_PLAYOUT_MIN_CONCEAL_US`]) 以下の隙間は補間しない。
    fn concealment_of(
        &self,
        now_us: i64,
        previous_end_us: Option<i64>,
        start_at_us: i64,
        total_gap_us: i64,
    ) -> (i64, i64, i64) {
        let Some(previous_end_us) = previous_end_us else {
            return (0, 0, 0);
        };
        if total_gap_us <= 0 {
            return (0, 0, 0);
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
            return (total_gap_us, 0, unrecoverable_gap_us);
        }
        // 予約できる区間 = 予約できる先頭から開始まで (上限で切る)
        let conceal_us = start_at_us
            .saturating_sub(reservable_start_us)
            .clamp(0, AUDIO_PLAYOUT_MAX_CONCEAL_US);
        if conceal_us <= AUDIO_PLAYOUT_MIN_CONCEAL_US {
            return (total_gap_us, 0, unrecoverable_gap_us);
        }
        (total_gap_us, conceal_us, unrecoverable_gap_us)
    }
}

impl PendingPlayout {
    /// 計画した開始時刻
    fn decision_start_at_us(&self) -> i64 {
        match self.decision {
            AudioPlayoutDecision::Play { start_at_us, .. } => start_at_us,
            AudioPlayoutDecision::Drop { .. } => {
                unreachable!("確定待ちの計画は Play だけである")
            }
        }
    }
}

impl Default for AudioPlayoutScheduler {
    fn default() -> Self {
        Self::new()
    }
}
