//! 音声の目標遅延を、実際に鳴った結果から閉ループで決める
//!
//! [`crate::playout::delay`] の学習 (NetEq の移植) は「直近の窓で最も早く届いた音を基準に
//! した相対的な到着の遅れ」の分位点であり、経路の揺らぎは吸収できるが、ストリーム全体が
//! 一様に遅れている分は見えない。基準 (最小値) そのものが下へ動くため、相対の遅れは 0 の
//! ままになるためである。実際には、到着から鳴り始めるまでに復号・予約・出力のバッファ・
//! まとめて届いた山の分だけ時間がかかり、学習した目標より遅れて鳴る。すると予定を過ぎて
//! 鳴り続け、並べすぎの音が捨てられる。
//!
//! そこで、実際に観測した「予定をどれだけ過ぎて鳴ったか」(遅れ) と「並べすぎで捨てた量」を
//! 見て目標を増減する。
//!
//! - 遅れが続くなら目標を増やす (乱れと、到着から鳴るまでの経路の分を吸収する)
//! - 並べすぎで捨てたなら、捨てた長さぶんを吸収できるだけ増やす
//! - 出力が枯れたなら、枯れた長さぶんを吸収できるだけ増やす。アンダーランは事後には
//!   埋められないため、目標を上げる以外に減らせない
//! - 遅れが許容の中に収まり、捨ても枯れも無いなら目標を減らす (必要以上に遅らせない)
//! - 増減は毎秒 1 回までとし、1 回の増加は [`AUDIO_DELAY_FEEDBACK_MAX_STEP_US`] までにする
//!   (数秒スケール。行き過ぎを防ぐ)
//! - 目標は [`AUDIO_DELAY_FEEDBACK_MIN_US`] から、実際に適用する上限
//!   ([`AudioDelayFeedback::applied_ceiling_us`]) の間に収める。適用は
//!   [`AudioDelayFeedback::target_delay_us`] が同じ上限で切るため、内部の目標だけがそれを
//!   超えて育つと、超えた分は決して適用されないまま残り、遅れが収まってから減らし切るまで
//!   (毎秒 [`AUDIO_DELAY_FEEDBACK_DECREASE_US_PER_SECOND`]) の時間だけ復帰が遅れる
//! - 適用する上限は、圧縮 (時間圧縮 = 粗さ) が続くときだけ
//!   [`AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US`] (160 ms) へ広げる。圧縮は「経路が
//!   [`AUDIO_DELAY_FEEDBACK_MAX_TARGET_US`] (100 ms) では足りていない」証拠であり、目標を
//!   上げれば遅れて届く音が減って圧縮そのものが減るためである。広げる条件と戻す条件は
//!   [`AudioDelayFeedback::update`] にある
//! - `targetLatency` を明示設定したときは、その値を自動で超えない
//!   ([`AudioDelayFeedback::set_ceiling_us`] で渡す)。jitter buffer の学習値
//!   ([`AudioDelayFeedback::target_delay_us`] へ渡す値) には掛けない。既存の揺らぎの吸収を
//!   変えないためである
//!
//! 判断に使う分布は、表示用の 10 秒の窓ではなく短い窓
//! ([`crate::playout::timing::AUDIO_DELAY_FEEDBACK_WINDOW_US`]) の値にする。目標を増やした
//! 結果が現れるまで 10 秒の窓は待つため、その窓だけで増やすと行き過ぎる (実際の遅れが
//! 消えた後も増え続けて上限に張り付く)。増やす量も、遅れの p50 を使う。まれな大きな跳ね
//! (p95) まで吸収しようとすると、目標が上限に張り付いて遅延だけが増えるためである。
//!
//! 時刻はすべて呼び出し側が引数で渡すマイクロ秒である。時計もデバイスも触らない。
//! 観測は購読側が作る値
//! ([`AudioPlayoutTimingStats::audio_delay_feedback`](crate::playout::timing::AudioPlayoutTimingStats::audio_delay_feedback))
//! をそのまま [`AudioDelayFeedback::update`] へ渡す。

use crate::playout::scheduler::AUDIO_PLAYOUT_STRETCH_MIN_US;
use crate::playout::timing::AudioDelayFeedbackObservation;

/// 目標遅延の下限 (マイクロ秒)
///
/// 閉ループが「遅れが許容の中に収まり、捨てが無い」と判断して下げ続けても、これ以下には
/// しない。時間軸側の下限
/// ([`crate::playout::timeline::TIMELINE_AUDIO_DELAY_FLOOR_US`]) と同じ値にする。閉ループの
/// 値は到着基準で鳴らすときの遅れと、映像を合わせる先の両方に効くため、片方だけを変えると
/// 基準を共有できない状態で A/V のずれが残る。
///
/// 値は実リレーの実測で決めた (表と根拠は
/// [`crate::playout::timeline::TIMELINE_AUDIO_DELAY_FLOOR_US`] にある)。観測が無い間の
/// 揺らぎの学習の初期値 ([`crate::playout::delay::AUDIO_DELAY_START_MS`] の 80 ms) とは
/// 別の値であり、揃える必要はない。初期値は閉ループの観測が始まる前にだけ使われ、閉ループは
/// 観測が 1 つでも届けば目標を実測から動かす。
pub const AUDIO_DELAY_FEEDBACK_MIN_US: i64 = 50_000;

/// 目標遅延の上限 (マイクロ秒)
///
/// まれな大きな乱れまで吸収しようとすると、常に大きく遅れて鳴ることになるため上限を置く。
/// 表示の遅れの上限 ([`crate::playout::timeline::TIMELINE_MAX_PRESENTATION_DELAY_MS`] の
/// 500 ms) より小さい。
///
/// 実際に適用する上限 ([`AudioDelayFeedback::applied_ceiling_us`]) は 100 ms (広げても
/// 160 ms) であるため、閉ループの目標がこの値に達することは無い。明示設定
/// ([`AudioDelayFeedback::set_ceiling_us`]) がこれより大きいときに、その値をそのまま
/// 使わないための絶対の上限として残す。
pub const AUDIO_DELAY_FEEDBACK_MAX_US: i64 = 300_000;

/// 実際に使う目標遅延の上限 (マイクロ秒)
///
/// 揺らぎの学習 ([`crate::playout::delay`]) と閉ループの目標の大きい方をそのまま使うと、
/// 到着が大きく跳ねた分まで目標になり、その分だけ常に遅れて鳴る。moqt-js の実測 (実リレー
/// 経由) では到着の跳ねの最大が 271 ms あり、目標 100 ms の内側に収まらない分は受信側の
/// 追いつきが吸収する分担にした。
///
/// 到着基準で並べるときの遅れ ([`crate::playout::timeline::TIMELINE_ARRIVAL_DELAY_US`]) と
/// 同じ値である。A/V の合わせ先が 2 つの経路で食い違わないようにするためである。
///
/// この上限は閉ループの内部値にも掛ける。適用する値の上限と同じ値で切らないと、超えた分は
/// 決して適用されないまま残り、遅れが収まってから減らし切るまでの時間だけ復帰が遅れる。
/// 圧縮が続くときだけ [`AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US`] へ広げる
/// ([`AudioDelayFeedback::applied_ceiling_us`])。
pub const AUDIO_DELAY_FEEDBACK_MAX_TARGET_US: i64 = 100_000;

/// 圧縮 (時間圧縮 = 粗さ) が続くときにだけ使う上限 (マイクロ秒)
///
/// 既定の上限 ([`AUDIO_DELAY_FEEDBACK_MAX_TARGET_US`] の 100 ms) を超えて目標を上げてよいのは、
/// 圧縮が続いているときだけである。圧縮は「経路が 100 ms では足りていない」証拠であり、
/// 目標を上げれば遅れて届く音が減って圧縮そのものが減る。moqt-js の実測 (2026-10-16、実リレー
/// 60 秒) では揺らぎだけの目標が 100〜340 ms に育ち、上限 100 ms が常時拘束されて時間圧縮が
/// 60 秒で 550〜1000 ms (静かな回でも 556〜996 ms) 続いた。上限を静的に 160 ms へ上げると
/// 圧縮は約 3 割減る回がある一方、鳴り始めが 115 ms から 155 ms へ +40 ms になる。160 ms は、
/// 実測の揺らぎ (100〜340 ms) の下限側と遅延の代償 (+40 ms) の釣り合いで決めた、圧縮を
/// 止められる最小限の幅である。
///
/// 広げる分にも明示設定 ([`AudioDelayFeedback::set_ceiling_us`]) を掛ける。利用者が宣言した
/// 遅延の予算を、自動の判断で超えないためである。
pub const AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US: i64 = 160_000;

/// 上限を上げる根拠にする圧縮の速さ (マイクロ秒 / 秒)
///
/// moqt-js の実測の圧縮は 60 秒で 550〜1000 ms であり、毎秒 9〜17 ms に相当する。5 ms/秒 は
/// その半分以下である。不感帯 ([`AUDIO_PLAYOUT_STRETCH_MIN_US`]) を超える遅れが続く正常な
/// 状態でも数 ms/秒 の圧縮は出るため (実測: 圧縮が 0 だった秒は 59 秒中 4〜7 秒)、これを
/// 下回る圧縮で上限を上げると遅延の代償だけを払うことになる。逆に、これを超える圧縮が
/// 続くなら 100 ms では足りていないと判断できる。
pub const AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND: i64 = 5_000;

/// 上限を上げるまでに、圧縮が続くのを待つ時間 (マイクロ秒)
///
/// 到着が一時的に乱れただけでも圧縮は出る。2 秒続いたときだけ「経路が 100 ms では足りない」
/// とみなす。詰め始めてから、その結果が観測の窓
/// ([`crate::playout::timing::AUDIO_DELAY_FEEDBACK_WINDOW_US`]) に現れるまでを待つ意味もある。
/// 目標を動かすのは毎秒 1 回 ([`AUDIO_DELAY_FEEDBACK_INTERVAL_US`]) であるため、観測 2 回ぶん
/// になる。
pub const AUDIO_DELAY_FEEDBACK_COMPRESSION_CONFIRM_US: i64 = 2_000_000;

/// 上限を [`AUDIO_DELAY_FEEDBACK_MAX_TARGET_US`] へ戻すまでに、圧縮が 0 で経路が静かな状態が
/// 続くのを待つ時間 (マイクロ秒)
///
/// 戻す条件に「揺らぎだけの目標 ([`AudioDelayFeedbackObservation::jitter_target_us`]) が
/// [`AUDIO_DELAY_FEEDBACK_MAX_TARGET_US`] 以下」を必ず入れる。圧縮が 0 になっただけで戻すと、
/// 揺らぎが 100 ms を超えている経路では、戻した直後に遅れが生じて圧縮が再発し、2 秒でまた
/// 160 ms へ上げる往復になる (周期 20 秒前後で遅延が 60 ms 上下する)。15 秒は、moqt-js の
/// 実測のまれな跳ね (最大 271 ms) が引いてから揺らぎの推定が下がるまでの数秒より十分に
/// 長い。
pub const AUDIO_DELAY_FEEDBACK_ARTIFACT_RELEASE_US: i64 = 15_000_000;

/// 最初の目標遅延 (マイクロ秒)
///
/// 観測がまだ無いときに使う。jitter buffer の学習の初期値
/// ([`crate::playout::delay::AUDIO_DELAY_START_MS`] の 80 ms) より少し大きくし、到着から
/// 鳴るまでの経路の分を最初から見込む。
///
/// [`AUDIO_DELAY_FEEDBACK_MAX_TARGET_US`] (適用する上限) と同じ値であるため、既定の構成では
/// 内部の目標が初期値で上限に張り付き、増やす規則 (遅れや並べすぎ) で目標が上がる余地が無い。
/// 増えるのは、圧縮が続いて上限を [`AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US`] へ広げた後か、
/// 収束で下げた後に再び遅れが出たときである (moqt-js も同じ関係である)。
pub const AUDIO_DELAY_FEEDBACK_START_US: i64 = 100_000;

/// 目標を動かす間隔 (マイクロ秒)
///
/// 数秒スケールで動かし、行き過ぎと往復を避ける。観測が来るたびに動かすと、増やした結果が
/// 観測へ現れる前に増やし続けて上限に張り付く。
pub const AUDIO_DELAY_FEEDBACK_INTERVAL_US: i64 = 1_000_000;

/// 遅れを許容する量 (マイクロ秒)
///
/// これ以下なら「予定どおり鳴っている」とみなし、目標を減らす向きに動かす。
pub const AUDIO_DELAY_FEEDBACK_TOLERANCE_US: i64 = 10_000;

/// 目標を増やすときに足す余白 (マイクロ秒)
///
/// 許容の境目で往復しないようにする。
pub const AUDIO_DELAY_FEEDBACK_MARGIN_US: i64 = 20_000;

/// 1 回で増やす下限 (マイクロ秒)
pub const AUDIO_DELAY_FEEDBACK_MIN_STEP_US: i64 = 20_000;

/// 1 回で増やす上限 (マイクロ秒)
///
/// 数秒で収束させつつ、行き過ぎないようにする。
pub const AUDIO_DELAY_FEEDBACK_MAX_STEP_US: i64 = 40_000;

/// 目標を減らす速さ (マイクロ秒 / 秒)
///
/// 増やす向きより遅くし、往復を避ける。速すぎると、遅れが現れるまでに下げすぎて
/// (観測の窓が追いつく前に) 遅れを作り直す。
pub const AUDIO_DELAY_FEEDBACK_DECREASE_US_PER_SECOND: i64 = 10_000;

/// 目標を動かした理由
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDelayFeedbackReason {
    /// まだ動かしていない (初期値のまま)
    Initial,
    /// 並べすぎで捨てたため増やした
    Backlog,
    /// 出力が枯れたため増やした
    ///
    /// 並べすぎの捨てや鳴り遅れと同時に起きたときも、事後には埋められないアンダーランの
    /// 方を優先してこの理由にする。
    Underrun,
    /// 予定を過ぎて鳴ったため増やした
    Lateness,
    /// 遅れが許容の中に収まり、捨てが無いため減らした
    Settled,
    /// 動かす条件がそろっていない (観測が無い、または前回から間隔が空いていない)
    Waiting,
}

/// 閉ループの状態 (計器とテスト用)
///
/// 実際に使う目標遅延 (jitter buffer の学習値との大きい方) は
/// [`AudioDelayFeedback::target_delay_us`] が返す。ここには閉ループが決めた分だけを出す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioDelayFeedbackSnapshot {
    /// 閉ループが決めた目標遅延 (マイクロ秒)。下限と上限と明示の上限を適用した値
    pub target_us: i64,
    /// 直前に目標を動かした理由
    pub reason: AudioDelayFeedbackReason,
    /// 明示設定の上限 (マイクロ秒)。無ければ None
    pub ceiling_us: Option<i64>,
    /// 直前の制御で動かした量 (マイクロ秒)。0 なら動かしていない
    pub last_change_us: i64,
    /// 目標を動かした回数
    pub adjustments: u64,
    /// 直近の観測の、予定を過ぎて鳴った量の p50 (マイクロ秒)。観測が無ければ None
    pub lateness_p50_us: Option<i64>,
    /// 直近の観測の、到着から鳴り始めるまでの時間の p50 (マイクロ秒)。観測が無ければ None
    pub start_delay_p50_us: Option<i64>,
    /// 直近の観測の、予定に対する余裕の p50 (マイクロ秒)。観測が無ければ None
    pub slack_p50_us: Option<i64>,
    /// いま実際に適用している値の上限 (マイクロ秒)
    ///
    /// 圧縮が続いていないときは [`AUDIO_DELAY_FEEDBACK_MAX_TARGET_US`] (100 ms)。圧縮が
    /// 続くと [`AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US`] (160 ms) まで広がるが、明示設定
    /// ([`AudioDelayFeedback::set_ceiling_us`]) があるときはその値までに切る (既定の
    /// 100 ms は下回らない)。実際に適用された値は [`AudioDelayFeedback::target_delay_us`] が
    /// 返す。
    pub applied_ceiling_us: i64,
    /// 直近の制御で観測した圧縮 (時間圧縮) の速さ (マイクロ秒 / 秒)。上限を上げる判断に
    /// 使った値
    pub compression_us_per_second: i64,
}

/// 目標を下限と上限の中に収める
///
/// 上限が下限を下回るときは上限を優先する (明示された `targetLatency` を下限より優先する)。
/// `i64::clamp` は下限が上限を超えるとパニックするため使わない。
fn clamp_target_us(value_us: i64, lower_us: i64, upper_us: i64) -> i64 {
    value_us.max(lower_us).min(upper_us)
}

/// 音声の目標遅延を、実際に鳴った結果から決める
///
/// 音声トラックごとではなく、共有の時間軸 ([`crate::playout::timeline::PlayoutTimeline`]) に
/// 1 つ持つ。目標は到着から鳴るまでの経路 (復号・予約・出力のバッファ) の学習であり、
/// 購読のやり直しや TIMESTAMP の段差では変わらないためである。
#[derive(Debug)]
pub struct AudioDelayFeedback {
    /// 閉ループが決めた目標遅延 (マイクロ秒)
    target_us: i64,
    /// 直前に目標を動かした理由
    reason: AudioDelayFeedbackReason,
    /// 明示設定の上限 (マイクロ秒)。無ければ None
    ceiling_us: Option<i64>,
    /// 直前に目標を動かした時刻 (マイクロ秒)。まだ動かしていなければ None
    last_update_at_us: Option<i64>,
    /// 直前に読んだ、累積で並べすぎて捨てた音の数
    last_backlog_misses: u64,
    /// 直前に読んだ、累積で並べすぎて捨てた音の長さ (マイクロ秒)
    last_backlog_us: i64,
    /// 直前の制御で動かした量 (マイクロ秒)
    last_change_us: i64,
    /// 目標を動かした回数
    adjustments: u64,
    /// 直近の観測
    latest: Option<AudioDelayFeedbackObservation>,
    /// 閉ループへ観測が 1 つでも届いたか。届くまでは閉ループの目標を使わない
    ///
    /// 鳴っていない音の観測 (捨てた音や、鳴り終わっていない音を止めたときの通知) でも
    /// `true` になるため、「実際に鳴った結果を観測したか」ではない
    /// ([`AudioDelayFeedback::target_delay_us`] が参照する)。
    received_observation: bool,
    /// 圧縮が続くため上限を広げているか
    widened_ceiling: bool,
    /// 上限を上げる根拠 (圧縮の速さが閾値を超えた状態) が続いている時間 (マイクロ秒)
    compression_streak_us: i64,
    /// 上限を戻す条件 (圧縮が 0 で、揺らぎだけの目標が 100 ms 以下) が続いている時間
    /// (マイクロ秒)
    quiet_streak_us: i64,
    /// 前回の制御から増えた圧縮の合計 (マイクロ秒)。観測のたびに足し、制御のたびに 0 に戻す
    compression_since_update_us: i64,
    /// 直近の制御で観測した圧縮の速さ (マイクロ秒 / 秒)。計器に出す
    compression_us_per_second: i64,
}

impl AudioDelayFeedback {
    /// 観測の無い状態で作る
    pub fn new() -> Self {
        Self {
            target_us: AUDIO_DELAY_FEEDBACK_START_US,
            reason: AudioDelayFeedbackReason::Initial,
            ceiling_us: None,
            last_update_at_us: None,
            last_backlog_misses: 0,
            last_backlog_us: 0,
            last_change_us: 0,
            adjustments: 0,
            latest: None,
            received_observation: false,
            widened_ceiling: false,
            compression_streak_us: 0,
            quiet_streak_us: 0,
            compression_since_update_us: 0,
            compression_us_per_second: 0,
        }
    }

    /// 明示設定の上限を渡す (`targetLatency`)
    ///
    /// 自動で決めた目標がこれを超えないようにする。値が変わった時点で、既に超えていれば
    /// その場で収める。`None` を渡すと上限を外す。負の値は 0 として扱う。
    ///
    /// 圧縮が続いて上限を広げている間も、広げた分はこの値を超えない
    /// ([`AudioDelayFeedback::applied_ceiling_us`])。ただし既定の
    /// [`AUDIO_DELAY_FEEDBACK_MAX_TARGET_US`] は下回らない。揺らぎだけの目標
    /// ([`AudioDelayFeedback::target_delay_us`] へ渡す値) に明示設定を掛けない既存の規則を
    /// 変えないためである。
    pub fn set_ceiling_us(&mut self, ceiling_us: Option<i64>) {
        self.ceiling_us = ceiling_us.map(|ceiling_us| ceiling_us.max(0));
        self.target_us =
            clamp_target_us(self.target_us, AUDIO_DELAY_FEEDBACK_MIN_US, self.upper_us());
    }

    /// 明示設定の上限 (マイクロ秒)。無ければ None
    pub fn ceiling_us(&self) -> Option<i64> {
        self.ceiling_us
    }

    /// 閉ループが決めた目標 (マイクロ秒)。jitter buffer の学習値を含まない
    ///
    /// 下限と、いま適用している上限 ([`AudioDelayFeedback::applied_ceiling_us`]) の間に
    /// 収める。適用されない値へ育てないためである。
    pub fn feedback_target_us(&self) -> i64 {
        clamp_target_us(self.target_us, AUDIO_DELAY_FEEDBACK_MIN_US, self.upper_us())
    }

    /// 実際に使う目標遅延 (マイクロ秒)
    ///
    /// jitter buffer の学習値と、閉ループが決めた目標の大きい方にし、最後に、いま適用して
    /// いる上限 ([`AudioDelayFeedback::applied_ceiling_us`]) で切る。明示設定の上限
    /// ([`AudioDelayFeedback::set_ceiling_us`]) は閉ループの値にだけ掛ける。学習値は既存の
    /// 揺らぎの吸収そのものであり、明示設定で切り下げると挙動が変わるためである。適用の上限
    /// は、学習値も含めて掛ける。到着が大きく跳ねた分まで目標にすると、その分だけ常に遅れて
    /// 鳴るためである。
    ///
    /// 閉ループへ観測がまだ 1 つも届いていないときは、閉ループの目標を使わず
    /// `jitter_target_us` を上限 ([`AUDIO_DELAY_FEEDBACK_MAX_TARGET_US`] = 100 ms) で切って
    /// 返す。閉ループは観測を入力にする制御であり、観測が無い間に目標を動かすと、まだ
    /// 鳴らしていない間の表示の遅れ (購読を始めた直後の基準) が変わるためである。
    pub fn target_delay_us(&self, jitter_target_us: i64) -> i64 {
        if !self.received_observation {
            return jitter_target_us.min(AUDIO_DELAY_FEEDBACK_MAX_TARGET_US);
        }
        jitter_target_us
            .max(self.feedback_target_us())
            .min(self.applied_ceiling_us())
    }

    /// いま適用している値の上限 (マイクロ秒)
    ///
    /// 圧縮が続いていないときは [`AUDIO_DELAY_FEEDBACK_MAX_TARGET_US`] (100 ms)。圧縮が
    /// 続いて上限を広げているときは `max(100 ms, min(160 ms, 明示設定 ?? 160 ms))` である。
    /// 利用者が宣言した予算を超えず、既定の 100 ms を下回らない。
    pub fn applied_ceiling_us(&self) -> i64 {
        if !self.widened_ceiling {
            return AUDIO_DELAY_FEEDBACK_MAX_TARGET_US;
        }
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US.max(
            AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US.min(
                self.ceiling_us
                    .unwrap_or(AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US),
            ),
        )
    }

    /// 観測を 1 つ受け取り、必要なら目標を動かす
    ///
    /// 目標を動かすのは [`AUDIO_DELAY_FEEDBACK_INTERVAL_US`] ごとに 1 回だけである。観測の
    /// 窓が短いため、間隔を空けずに動かすと、増やした結果が現れる前に増やし続ける。
    ///
    /// 並べすぎの累積は、購読のやり直しで 0 に戻っても負の差で増やさない。
    ///
    /// 出力が枯れた ([`AudioDelayFeedbackObservation::underrun_ms`] が 0 より大きい) ときは、
    /// 既存の増加判断と独立に、枯れた長さ (1 回の増加の上限まで) だけ目標を上げる。並べすぎや
    /// 鳴り遅れによる増加と同時なら大きい方を使い、理由は
    /// [`AudioDelayFeedbackReason::Underrun`] を優先する。枯れは事後には埋められないため、
    /// 遅れが許容の中に収まっていても減らさずに増やす。
    ///
    /// 遅れの p50 からは、時間圧縮の不感帯 ([`AUDIO_PLAYOUT_STRETCH_MIN_US`]) の分を引く。
    /// 不感帯の中で意図して残した遅れは「予定の方が僅かにずれている」だけであり、目標を
    /// 上げても playout は詰めないため、目標だけが上がり続けて遅延が伸びる。裾 (p95 と max)
    /// は引かない。引くと「たまに大きく遅れた」事実まで消えてしまう。
    ///
    /// 目標を動かす前に、圧縮 (時間圧縮 = 粗さ) が続いているかを見て、適用する上限を
    /// 上げる / 戻す (条件は [`AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND`] と
    /// [`AUDIO_DELAY_FEEDBACK_ARTIFACT_RELEASE_US`] の doc にある)。上げた上限は同じ観測の
    /// 判断から使う。
    ///
    /// [`AudioDelayFeedback::snapshot`] が返す直近の観測は、引いた後の値である (閉ループが
    /// 判断に使った値を見せる)。
    pub fn update(&mut self, mut observation: AudioDelayFeedbackObservation) {
        // 不感帯の中で playout が意図して残した遅れを、目標を動かす根拠から外す
        if let Some(lateness_us) = observation.lateness_us.as_mut() {
            lateness_us.p50 = lateness_us
                .p50
                .saturating_sub(AUDIO_PLAYOUT_STRETCH_MIN_US)
                .max(0);
        }
        self.latest = Some(observation);
        // 1 つでも観測を受けたら、以降は閉ループの目標を使う (鳴っていない音の観測でも同じ)
        self.received_observation = true;
        // 圧縮の増分を、次の制御 (毎秒 1 回) まで積む。音ごとの増分をそのまま速さとして見ると、
        // 1 秒間に出た圧縮を取りこぼす。負の値は 0 とみなす (購読のやり直しで累積が戻っても
        // 負の差を作らない)
        self.compression_since_update_us = self
            .compression_since_update_us
            .saturating_add(observation.compression_us.max(0));
        let elapsed_us = self
            .last_update_at_us
            .map(|last_update_at_us| observation.at_us.saturating_sub(last_update_at_us).max(0));
        if let Some(elapsed_us) = elapsed_us
            && elapsed_us < AUDIO_DELAY_FEEDBACK_INTERVAL_US
        {
            // まだ間隔に達していない。目標は動かさない
            self.reason = AudioDelayFeedbackReason::Waiting;
            return;
        }
        // 累積の捨てを、前回からの差にする (購読のやり直しで 0 に戻っても負にしない)
        let backlog_delta = observation
            .backlog_misses
            .saturating_sub(self.last_backlog_misses);
        let backlog_us_delta = observation
            .backlog_us
            .saturating_sub(self.last_backlog_us)
            .max(0);
        self.last_backlog_misses = observation.backlog_misses;
        self.last_backlog_us = observation.backlog_us;
        self.last_update_at_us = Some(observation.at_us);

        // 圧縮が続くときだけ上限を広げる。目標を動かす前に判断する (同じ観測で新しい上限まで
        // 上げてよい)
        self.update_artifact_ceiling(elapsed_us, observation.jitter_target_us);

        // 枯れた長さはミリ秒で渡る。1 回で増やす上限をミリ秒へ直した値と比べてから、
        // マイクロ秒へ戻す (枯れた長さが上限を超えても、1 回の増加は上限までにする)
        let underrun_step_us = observation
            .underrun_ms
            .clamp(0, AUDIO_DELAY_FEEDBACK_MAX_STEP_US / 1_000)
            .saturating_mul(1_000);
        let lateness_p50_us = observation.lateness_us.map(|summary| summary.p50);
        let previous_target_us = self.feedback_target_us();
        // 増やす向きの根拠 (並べすぎ / 鳴り遅れ)。無ければ 0 である
        let increase_us = if backlog_delta > 0 {
            // 並べすぎで捨てた。捨てた長さぶんを吸収できるだけ増やす
            self.reason = AudioDelayFeedbackReason::Backlog;
            clamp_target_us(
                backlog_us_delta.saturating_add(AUDIO_DELAY_FEEDBACK_MARGIN_US),
                AUDIO_DELAY_FEEDBACK_MIN_STEP_US,
                AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
            )
        } else if let Some(lateness_p50_us) = lateness_p50_us
            && lateness_p50_us > AUDIO_DELAY_FEEDBACK_TOLERANCE_US
        {
            // 予定を過ぎて鳴っている。過ぎた分を吸収できるだけ増やす
            self.reason = AudioDelayFeedbackReason::Lateness;
            clamp_target_us(
                lateness_p50_us
                    .saturating_sub(AUDIO_DELAY_FEEDBACK_TOLERANCE_US)
                    .saturating_add(AUDIO_DELAY_FEEDBACK_MARGIN_US),
                AUDIO_DELAY_FEEDBACK_MIN_STEP_US,
                AUDIO_DELAY_FEEDBACK_MAX_STEP_US,
            )
        } else {
            0
        };
        let change_us = if increase_us > 0 || underrun_step_us > 0 {
            // アンダーランは既存の増加判断と独立に目標を上げる。両方あるときは大きい方を使い、
            // 理由は事後には埋められないアンダーランを優先する
            if underrun_step_us > 0 {
                self.reason = AudioDelayFeedbackReason::Underrun;
            }
            increase_us.max(underrun_step_us)
        } else if lateness_p50_us.is_some() {
            // 遅れが許容の中に収まり、捨てが無い。余裕が続くため減らす。減らす速さは前回の
            // 観測からの時間で決めるため、前回が無いときは動かさない (1 回の観測だけで
            // 「余裕が続いている」とは言えない)
            let Some(elapsed_us) = elapsed_us else {
                self.reason = AudioDelayFeedbackReason::Waiting;
                return;
            };
            self.reason = AudioDelayFeedbackReason::Settled;
            AUDIO_DELAY_FEEDBACK_DECREASE_US_PER_SECOND
                .saturating_mul(elapsed_us)
                .saturating_div(1_000_000)
                .saturating_neg()
        } else {
            // まだ鳴らしていないため、遅れが分からない。目標は動かさない
            self.reason = AudioDelayFeedbackReason::Waiting;
            return;
        };
        self.target_us = clamp_target_us(
            self.target_us.saturating_add(change_us),
            AUDIO_DELAY_FEEDBACK_MIN_US,
            self.upper_us(),
        );
        self.last_change_us = self.target_us.saturating_sub(previous_target_us);
        if self.last_change_us != 0 {
            self.adjustments = self.adjustments.saturating_add(1);
        }
    }

    /// 圧縮 (時間圧縮 = 粗さ) が続くときだけ、適用する上限を上げる / 戻す
    ///
    /// - 上げる: 圧縮の速さが [`AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND`] を超える
    ///   状態が [`AUDIO_DELAY_FEEDBACK_COMPRESSION_CONFIRM_US`] 続いたとき。100 ms では
    ///   足りない証拠がそろったということである
    /// - 戻す: 揺らぎだけの目標 ([`AudioDelayFeedbackObservation::jitter_target_us`]) が
    ///   [`AUDIO_DELAY_FEEDBACK_MAX_TARGET_US`] 以下であり、かつ圧縮が 0 の状態が
    ///   [`AUDIO_DELAY_FEEDBACK_ARTIFACT_RELEASE_US`] 続いたとき。圧縮が 0 だけでは戻さない。
    ///   揺らぎが 100 ms を超えている経路では、戻した直後に圧縮が再発して 2 秒でまた上げる
    ///   往復になる (このファイルの定数のコメントを参照)
    ///
    /// どちらの条件も満たさないとき (圧縮は出ているが閾値未満、または経路がまだ 100 ms を
    /// 必要としている) は、両方の連続時間を 0 に戻す。
    ///
    /// 目標そのものは動かさない。広げた上限は [`AudioDelayFeedback::target_delay_us`] が使い、
    /// 戻した上限は [`AudioDelayFeedback::feedback_target_us`] が切る。内部の目標を下げるのは
    /// 既存の減らす規則 ([`AUDIO_DELAY_FEEDBACK_DECREASE_US_PER_SECOND`]) に任せる。
    ///
    /// `elapsed_us` は前回の制御からの時間 (マイクロ秒)。前回が無ければ None であり、そのとき
    /// は速さを求められないため 0 とする。
    fn update_artifact_ceiling(&mut self, elapsed_us: Option<i64>, jitter_target_us: i64) {
        let span_us = elapsed_us.unwrap_or(0);
        let compression_us = self.compression_since_update_us;
        self.compression_since_update_us = 0;
        // 観測は音ごとに来るが、制御は毎秒 1 回である。間隔が分からないとき (最初の観測) と
        // 間隔が空いていないときは速さを求められないため 0 とする。i64 の整除で丸まらない
        // よう、先に掛けてから割る
        let rate_us_per_second = if span_us > 0 {
            compression_us.saturating_mul(1_000_000) / span_us
        } else {
            0
        };
        self.compression_us_per_second = rate_us_per_second;
        if rate_us_per_second > AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND {
            // 100 ms では足りていない証拠が続いている
            self.compression_streak_us = self.compression_streak_us.saturating_add(span_us);
            self.quiet_streak_us = 0;
        } else if compression_us == 0 && jitter_target_us <= AUDIO_DELAY_FEEDBACK_MAX_TARGET_US {
            // 圧縮が無く、経路も 100 ms 以内で足りている
            self.quiet_streak_us = self.quiet_streak_us.saturating_add(span_us);
            self.compression_streak_us = 0;
        } else {
            // 圧縮は出ているが閾値未満、または経路がまだ 100 ms を必要としている。どちらの
            // 条件も満たしていないため、数え直す
            self.compression_streak_us = 0;
            self.quiet_streak_us = 0;
        }
        if !self.widened_ceiling
            && self.compression_streak_us >= AUDIO_DELAY_FEEDBACK_COMPRESSION_CONFIRM_US
        {
            self.widened_ceiling = true;
        } else if self.widened_ceiling
            && self.quiet_streak_us >= AUDIO_DELAY_FEEDBACK_ARTIFACT_RELEASE_US
        {
            // 上限だけを戻す。内部の目標は既存の減らす規則に任せる (feedback_target_us が
            // 新しい上限で切るため、実際に使う値はここで 100 ms 以下になる)
            self.widened_ceiling = false;
        }
    }

    /// 閉ループの状態を求める (計器用)
    pub fn snapshot(&self) -> AudioDelayFeedbackSnapshot {
        AudioDelayFeedbackSnapshot {
            target_us: self.feedback_target_us(),
            reason: self.reason,
            ceiling_us: self.ceiling_us,
            last_change_us: self.last_change_us,
            adjustments: self.adjustments,
            lateness_p50_us: self
                .latest
                .and_then(|latest| latest.lateness_us)
                .map(|s| s.p50),
            start_delay_p50_us: self
                .latest
                .and_then(|latest| latest.start_delay_us)
                .map(|s| s.p50),
            slack_p50_us: self
                .latest
                .and_then(|latest| latest.slack_us)
                .map(|s| s.p50),
            applied_ceiling_us: self.applied_ceiling_us(),
            compression_us_per_second: self.compression_us_per_second,
        }
    }

    /// 閉ループの目標を持つのに使える上限 (マイクロ秒)
    ///
    /// 実際に適用する値の上限と同じにする。適用は
    /// [`AUDIO_DELAY_FEEDBACK_MAX_TARGET_US`] (広げているときは
    /// [`AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US`]) で切られるため、それより大きい目標を
    /// 持っても適用されない。持つと、遅れが収まった後に減らし切るまでの時間だけ復帰が
    /// 遅れる。
    fn upper_us(&self) -> i64 {
        AUDIO_DELAY_FEEDBACK_MAX_US
            .min(self.ceiling_us.unwrap_or(AUDIO_DELAY_FEEDBACK_MAX_US))
            .min(self.applied_ceiling_us())
    }
}

impl Default for AudioDelayFeedback {
    fn default() -> Self {
        Self::new()
    }
}
