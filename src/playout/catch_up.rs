//! 受信側の音声が遅れて固定されたら、復号器の作り直しを求める
//!
//! 復号器の中に未処理の音が溜まると、溜まった分だけ復号の出力が送られた TIMESTAMP から
//! 遅れたまま固定される。溜まった分は実時間と同じ速さでしか出てこないため、待っても戻らない。
//! 鳴らす音を捨てても、捨てた分は復号器から出てきた後の音であり、中の分は減らない。そこで
//! 遅れが健全な値から外れたら復号器の作り直しを求め、溜まった分を捨てて live へ戻す。
//!
//! 判定に使う遅れは、復号の出力の遅れ ([`AudioReceiveCatchUpObservation::lag_us`]) そのもの
//! ではなく、そこから受信の入口で測った入力の遅れ
//! ([`AudioReceiveCatchUpObservation::input_lag_us`]、Object の到着の壁時計 − LOC TIMESTAMP)
//! を引いた「復号が足している遅れ」である。出力の遅れだけでは、次の 2 つを区別できない。
//!
//! - 経路が恒久的に悪化した: Object の到着も復号の出力も同じだけ遅れる。復号器の中に溜まった
//!   分は増えていないため、作り直しても遅れは戻らない (moqt-js の実測: メインスレッドを 2 秒
//!   止めると、到着の遅れも復号の出力の遅れも 2017.9 ms になり、止めた後に溜まった分を復号し
//!   終えると 20 ms 前後へ戻った)。ここで作り直しを繰り返すと、音を飛ばすだけになる
//! - 受信側が溜めたまま戻らない: Object は到着している (入力の遅れは健全) のに、復号の出力
//!   だけが遅れている。復号器の中に溜まった音は実時間でしか出てこないため、作り直して捨て
//!   ない限り戻らない。これがこの判定が直す状態である
//!
//! 入力の遅れが測れないとき (TIMESTAMP が無い、Timescale がある) は、出力の遅れをそのまま
//! 使う。入力の遅れを導入する前と同じ挙動であり、測れないことを理由に作り直しを止めない。
//!
//! - 健全な遅れ (床) は直近 [`AUDIO_RECEIVE_CATCH_UP_FLOOR_WINDOW_MS`] の最小値である。
//!   経路の遅延はここへ吸収され、その上での増加だけを見る。床を求める値も「復号が足している
//!   遅れ」である。入力の遅れを引かないと、受信側が溜めたまま戻らない状態でも床が出力の遅れ
//!   へ追いつく (moqt-js の実測: 8〜20 秒を 250 ms に固定し、20 秒以降も復号器に溜まったまま
//!   にすると、13 秒で床が 250 ms へ追いつき、20〜60 秒の 40 秒間は作り直しが 0 回のまま
//!   250 ms で固定された)。入力の遅れを引けば、経路が遅いだけの状態 (入力の遅れも同じだけ
//!   上がる) では床が上がらず、受信側が溜めた状態では床が上がる前に作り直しが要求される
//!   (同じ実測で、段差の直後に 1 回で 30 ms へ復帰した)
//! - 上限は `max(`[`AUDIO_RECEIVE_CATCH_UP_MIN_MS`]`, 床 + `[`AUDIO_RECEIVE_CATCH_UP_GROWTH_MS`]`)`
//!   である。遅れが上限の中へ戻ったら追いつきを終える
//! - 始める条件は 2 つある。床から [`AUDIO_RECEIVE_CATCH_UP_JUMP_MS`] を超えて遅れている
//!   ときは、A/V の同期では合わせられない状態であり、その場で始める。超えないときは
//!   [`AUDIO_RECEIVE_CATCH_UP_CONFIRM_MS`] 続いた場合に始める。どちらも直近に始めていれば
//!   [`AUDIO_RECEIVE_CATCH_UP_COOLDOWN_MS`] たっていることを要求する
//! - 追いつきの間だけ、TIMESTAMP の跳びから飛んだ音を数える
//!
//! 判定だけを行い、復号器には触れない。時刻と値は呼び出し側が引数で渡す。

use alloc::collections::VecDeque;

/// 健全な遅れ (床) を求める窓 (ミリ秒)
///
/// 短くするほど環境の変化へ速く追従し、経路と復号の揺らぎが床に現れやすくなる。5 秒は、
/// 定常値を保つのに十分で、環境が変わったときに数秒で追従する長さである。全期間の最小値に
/// しないのは、購読の直後に relay の cache から届いた分をまとめて復号すると 1 音だけ小さな
/// 遅れが出ることがあり、それを床にすると健全な状態が上限を超えたままに見えるためである。
pub const AUDIO_RECEIVE_CATCH_UP_FLOOR_WINDOW_MS: i64 = 5_000;

/// 床が小さく観測されたときに許す、遅れの上限 (ミリ秒)
///
/// 音声の 1 パケットは 20 ms であり、これは 3 パケット分にあたる。復号の出力は投入から
/// 1 パケット以上遅れて返るため、健全な状態でも 10〜20 ms の遅れがある。床を実際より
/// 小さく観測しても、上限がこれより下がらないようにする。
pub const AUDIO_RECEIVE_CATCH_UP_MIN_MS: i64 = 60;

/// 健全な遅れからさらに許す遅れ (ミリ秒)
///
/// 2 パケット分である。遅れは経路と復号の揺らぎで数十 ms 動くため、1 音の揺らぎで始めない
/// 幅としてこれを許す。
pub const AUDIO_RECEIVE_CATCH_UP_GROWTH_MS: i64 = 40;

/// 健全な遅れを超えた分がこれを超えたら、持続を待たずに始める (ミリ秒)
///
/// A/V の同期で合わせられる量の上限
/// ([`crate::playout::timeline::TIMELINE_MAX_COMPENSATED_DIFFERENCE_US`] の 100 ms) と
/// 同じ値である。復号が足している遅れ (出力の遅れ − 入力の遅れ) が健全な遅れからこれを
/// 超えているとき、受信側は音声を映像へ合わせられない。待たずに始めて溜まった分を捨てる
/// 方が、遅れた音を鳴らし続けるより良い。経路そのものが遅いだけの状態では、入力の遅れも
/// 同じだけ上がるためここへは来ない (作り直しても戻らないため、来ないのが正しい)。
pub const AUDIO_RECEIVE_CATCH_UP_JUMP_MS: i64 = 100;

/// 追いつきを始める前に、上限を超えた状態が続くことを要求する時間 (ミリ秒)
///
/// 1 音の観測だけで始めると、経路と復号が一瞬つまずいただけでも始まる。100 ms (20 ms の
/// 音で 5 個) の持続を要求する。[`AUDIO_RECEIVE_CATCH_UP_JUMP_MS`] を超える遅れは、この
/// 持続を待たずに始める。待つと、その間に映像だけが先へ進んでずれが開くためである。
pub const AUDIO_RECEIVE_CATCH_UP_CONFIRM_MS: i64 = 100;

/// 追いつきを始めた後、次を始めない時間 (ミリ秒)
///
/// 1 回の作り直しで戻らない状態がある。負荷の間は作り直した後も溜まり直すためであり、
/// 遅れが上限へ戻るまではこの間隔で始め直す。上限をまたぐたびに始め直す状態にはならない
/// (床と上限は窓と共に動く)。
pub const AUDIO_RECEIVE_CATCH_UP_COOLDOWN_MS: i64 = 1_000;

/// 追いつきで音が飛んだとみなす、1 音の長さを超える最小の跳び (ミリ秒)
///
/// 復号の出力の TIMESTAMP は投入の順に進むため、飛びが無ければ 1 音の長さずつ進む。
/// これより小さい差は TIMESTAMP の丸めとみなし、捨てた音として数えない。
pub const AUDIO_RECEIVE_CATCH_UP_SKIP_MIN_MS: i64 = 1;

/// ミリ秒の定数をマイクロ秒へ換算する (内部の計算用)
///
/// 公開定数は moqt-js と同じミリ秒のまま残し、内部の計算だけをマイクロ秒で行う。
const fn ms_to_us(ms: i64) -> i64 {
    ms.saturating_mul(1_000)
}

/// マイクロ秒をミリ秒へ丸める (0.5 は正の無限大の側へ丸める)
///
/// moqt-js の `Math.round` と同じ向きである。統計はミリ秒で公開するため、マイクロ秒で
/// 持っている値をここで丸める。負の値も `Math.round` と同じく 0.5 を正の無限大の側へ丸める
/// (例: -1.5 ms は -1 ms)。
fn round_us_to_ms(value_us: i64) -> i64 {
    // floor((value + 0.5 ms) / 1 ms) を、0 方向への切り捨てになる整数除算で表す
    let shifted_us = value_us.saturating_add(500);
    let quotient = shifted_us / 1_000;
    let remainder = shifted_us % 1_000;
    if remainder != 0 && shifted_us < 0 {
        quotient - 1
    } else {
        quotient
    }
}

/// 床を求める 1 つの観測 (マイクロ秒)
#[derive(Debug, Clone, Copy)]
struct FloorSample {
    /// 観測した時刻 (マイクロ秒)
    at_us: i64,
    /// そのときの「復号が足している遅れ」 (マイクロ秒)
    lag_us: i64,
}

/// 復号の出力 1 つの観測
///
/// 時刻はすべて呼び出し側が渡すマイクロ秒である。moqt-js の
/// `AudioReceiveCatchUpObservation` はミリ秒の浮動小数で受けるが、本実装は
/// マイクロ秒の整数で受ける (音の長さが 23.22 ms のようなフレームでも切り捨てないため)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioReceiveCatchUpObservation {
    /// 復号の出力が届いた時刻 (呼び出し側の単調な時計のマイクロ秒)
    pub now_us: i64,
    /// 復号の出力の遅れ (マイクロ秒)
    ///
    /// 到着の壁時計 − TIMESTAMP である。入力の遅れが測れるときは生のまま統計に出し、判定には
    /// 使わない ([`AudioReceiveCatchUpObservation::input_lag_us`] を引いた「復号が足している
    /// 遅れ」を使う)。入力の遅れが測れないときはこの値をそのまま判定に使う。TIMESTAMP が無い
    /// 音ではこの値自体を測れないため、呼び出し側は観測しない
    pub lag_us: i64,
    /// 受信の入口で測った入力の遅れ (マイクロ秒)。測れないときは `None`
    ///
    /// Object の到着の壁時計 − LOC TIMESTAMP である。判定は、この値を `lag_us` から引いた
    /// 「復号が足している遅れ」で行う (このモジュールの先頭のコメントを参照)。TIMESTAMP が
    /// 無い / Timescale がある (メディア時刻である) 音では測れないため、呼び出し側が `None`
    /// を渡す。`None` のときは `lag_us` をそのまま使う (入力の遅れを導入する前と同じ挙動)。
    ///
    /// moqt-js の `intentionalDelayMs` (復号を意図的に遅らせた分) は、moqt-rs に復号前の
    /// 遅延 (PreDecodeDelay) の段が無いため持たない。復号前の遅延を移植するときは、
    /// 意図した遅延を引くフィールドを追加し、`decode_lag_us = max(0, lag_us - input_lag_us -
    /// intentional_delay_us)` にすること。追加しないと、復号を意図的に遅らせただけで復号器が
    /// 詰まっていると誤判定し、クールダウンごとに復号器の作り直しを要求する
    pub input_lag_us: Option<i64>,
    /// 音の長さ (マイクロ秒)
    pub duration_us: i64,
    /// 復号の出力の TIMESTAMP (マイクロ秒)。追いつきで飛んだ長さを測る
    pub timestamp_us: i64,
}

/// 復号器を作り直すかの判定
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioReceiveCatchUpDecision {
    /// 復号器を作り直すか
    pub rebuild_decoder: bool,
}

/// 受信側の音声の追いつきの観測値 (統計とログに出す)
///
/// ミリ秒のフィールドは、マイクロ秒で持っている値を moqt-js の `Math.round` と同じ向き
/// (0.5 は正の無限大の側) で丸めた値である。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioReceiveCatchUpStats {
    /// 直近に観測した、復号の出力の遅れ (ミリ秒)。まだ観測していなければ `None`
    pub lag_ms: Option<i64>,
    /// 直近に観測した、受信の入口の入力の遅れ (ミリ秒)。測れなければ `None`
    pub input_lag_ms: Option<i64>,
    /// 直近に観測した、復号が足している遅れ (ミリ秒)。まだ観測していなければ `None`
    ///
    /// 出力の遅れ − 入力の遅れである。判定 (床と上限) はこの値で行う。入力の遅れが測れない
    /// ときは出力の遅れそのものを使う。どちらの場合も負になったら 0 に寄せる (配信側の
    /// 時計が先へ進んでいるとき、または 2 つの遅れを別の時点で測った丸めである。
    /// moqt-js の `max(0, lagMs - inputLagMs - intentionalDelayMs)` に対応し、復号前の
    /// 遅延を移植しないため `intentionalDelayMs` は引かない)
    pub decode_lag_ms: Option<i64>,
    /// 健全時に観測した、復号が足している遅れ (床、ミリ秒)。まだ観測していなければ `None`
    pub floor_ms: Option<i64>,
    /// 観測した最大の、復号の出力の遅れ (ミリ秒)。まだ観測していなければ `None`
    pub max_lag_ms: Option<i64>,
    /// いまの遅れの上限 (ミリ秒)
    pub limit_ms: i64,
    /// 追いつきの最中か
    pub catching_up: bool,
    /// 追いつきを始めた回数
    pub catch_up_starts: u64,
    /// 追いつきで飛んだ音の数
    ///
    /// 跳びが 1 音の長さの何個分かを四捨五入して 1 を引いた数である
    /// (TIMESTAMP の格子が音の長さと一致しない環境でも 1 以上になる)。
    pub skipped_frames: u64,
    /// 追いつきで飛んだ音の長さの合計 (ミリ秒)
    pub skipped_ms: i64,
    /// 直近に飛んだ音の長さ (ミリ秒)。飛んでいなければ `None`
    pub last_skipped_ms: Option<i64>,
}

/// 受信側の音声の追いつき (購読ごとに 1 つ持つ)
///
/// [`AudioReceiveCatchUp::observe`] に復号の出力の観測を渡し、返ってきた判定が真なら
/// 呼び出し側が復号器を作り直す。
///
/// 判定の閾値はすべてこのモジュールの定数であり、実測で決めた値である。購読ごとに変える
/// ものではないため、設定は受け取らない。
pub struct AudioReceiveCatchUp {
    /// 床を求める観測 (古い順)
    floor_samples: VecDeque<FloorSample>,
    /// 直近の窓の最小の遅れ (マイクロ秒)。まだ観測していなければ `None`
    ///
    /// 「復号が足している遅れ」の最小値である (出力の遅れではない)
    floor_us: Option<i64>,
    /// 直近に観測した、復号の出力の遅れ (マイクロ秒)
    last_lag_us: Option<i64>,
    /// 直近に観測した、受信の入口の入力の遅れ (マイクロ秒)。測れなければ `None`
    last_input_lag_us: Option<i64>,
    /// 直近に観測した、「復号が足している遅れ」 (マイクロ秒)
    last_decode_lag_us: Option<i64>,
    /// 観測した最大の、復号の出力の遅れ (マイクロ秒)
    max_lag_us: Option<i64>,
    /// 上限を超え始めた時刻 (マイクロ秒)。超えていなければ `None`
    over_limit_since_us: Option<i64>,
    /// 追いつきの最中か
    catching_up: bool,
    /// 直近に追いつきを始めた時刻 (マイクロ秒)
    started_at_us: Option<i64>,
    /// 直近に観測した TIMESTAMP (マイクロ秒)
    last_timestamp_us: Option<i64>,
    /// 直近に観測した時刻 (マイクロ秒)
    last_observed_at_us: Option<i64>,
    /// 観測が途切れたときに保つ、途切れる前の床 (マイクロ秒)
    held_floor_us: Option<i64>,
    /// 保つ床の期限 (マイクロ秒)
    held_floor_until_us: Option<i64>,
    catch_up_starts: u64,
    skipped_frames: u64,
    /// 飛んだ音の長さの合計 (マイクロ秒)
    skipped_us: i64,
    /// 直近に飛んだ音の長さ (マイクロ秒)
    last_skipped_us: Option<i64>,
}

impl AudioReceiveCatchUp {
    /// 何も観測していない状態で作る
    pub fn new() -> Self {
        Self {
            floor_samples: VecDeque::new(),
            floor_us: None,
            last_lag_us: None,
            last_input_lag_us: None,
            last_decode_lag_us: None,
            max_lag_us: None,
            over_limit_since_us: None,
            catching_up: false,
            started_at_us: None,
            last_timestamp_us: None,
            last_observed_at_us: None,
            held_floor_us: None,
            held_floor_until_us: None,
            catch_up_starts: 0,
            skipped_frames: 0,
            skipped_us: 0,
            last_skipped_us: None,
        }
    }

    /// 復号の出力 1 つを観測し、復号器を作り直すかを決める
    ///
    /// 判定に使うのは、出力の遅れから入力の遅れを引いた「復号が足している遅れ」である
    /// (このモジュールの先頭のコメントを参照)。入力の遅れが測れないときは出力の遅れを
    /// そのまま使う。
    ///
    /// 内部の計算はすべてマイクロ秒で行う。ミリ秒の整数で計算すると、23.22 ms のような
    /// 音の長さで切り捨てが起き、飛んだ音の数と長さの統計が moqt-js とずれるためである。
    pub fn observe(
        &mut self,
        observation: AudioReceiveCatchUpObservation,
    ) -> AudioReceiveCatchUpDecision {
        let lag_us = observation.lag_us;
        // 復号が足している遅れ。入力の遅れが測れないときは出力の遅れそのものを使うが、
        // 負の遅れ (配信側の時計が先へ進んでいる) は健全な状態として 0 に寄せる。測れた
        // ときは 2 つの遅れを別の時点で測った丸めで負になりうるため、同じく 0 に寄せる
        // (moqt-js の `max(0, lagMs - inputLagMs - intentionalDelayMs)` に対応する。
        // 復号前の遅延を移植しないため intentionalDelayMs は引かない)
        let input_lag_us = observation.input_lag_us;
        let decode_lag_us = input_lag_us
            .map_or(lag_us, |input_lag_us| lag_us.saturating_sub(input_lag_us))
            .max(0);
        self.last_lag_us = Some(lag_us);
        self.last_input_lag_us = input_lag_us;
        self.last_decode_lag_us = Some(decode_lag_us);
        self.max_lag_us = Some(
            self.max_lag_us
                .map_or(lag_us, |max_lag_us| max_lag_us.max(lag_us)),
        );
        self.update_floor(observation.now_us, decode_lag_us);
        let limit_us = self.limit_us();

        // 追いつきで飛んだ音を、TIMESTAMP の跳びから測る (遅れが戻る判定より先に行う)
        self.measure_skip(observation);

        if decode_lag_us <= limit_us {
            // 上限の中へ戻った (作り直しが効いた)。次に上限を超えたら、また始められる
            self.catching_up = false;
            self.over_limit_since_us = None;
            return AudioReceiveCatchUpDecision {
                rebuild_decoder: false,
            };
        }
        if self.over_limit_since_us.is_none() {
            self.over_limit_since_us = Some(observation.now_us);
        }
        if !self.should_start(observation.now_us, decode_lag_us) {
            return AudioReceiveCatchUpDecision {
                rebuild_decoder: false,
            };
        }
        // 遅れが上限へ戻るまで始め直す。1 回の作り直しでは戻らない状態がある
        self.catching_up = true;
        self.started_at_us = Some(observation.now_us);
        self.catch_up_starts = self.catch_up_starts.saturating_add(1);
        AudioReceiveCatchUpDecision {
            rebuild_decoder: true,
        }
    }

    /// 観測値 (統計とログに出す)
    ///
    /// ミリ秒の値は、マイクロ秒の値から `round_us_to_ms` で丸める (moqt-js の
    /// `Math.round` と同じ向き)。飛んだ長さの累積はマイクロ秒のまま足し、ここで一度だけ
    /// 丸める (1 件ずつ丸めると誤差が積もるため)。
    pub fn snapshot(&self) -> AudioReceiveCatchUpStats {
        AudioReceiveCatchUpStats {
            lag_ms: self.last_lag_us.map(round_us_to_ms),
            input_lag_ms: self.last_input_lag_us.map(round_us_to_ms),
            decode_lag_ms: self.last_decode_lag_us.map(round_us_to_ms),
            floor_ms: self.floor_us.map(round_us_to_ms),
            max_lag_ms: self.max_lag_us.map(round_us_to_ms),
            limit_ms: round_us_to_ms(self.limit_us()),
            catching_up: self.catching_up,
            catch_up_starts: self.catch_up_starts,
            skipped_frames: self.skipped_frames,
            skipped_ms: round_us_to_ms(self.skipped_us),
            last_skipped_ms: self.last_skipped_us.map(round_us_to_ms),
        }
    }

    /// 観測と累積を消す (購読のやり直し)
    pub fn reset(&mut self) {
        self.floor_samples.clear();
        self.floor_us = None;
        self.last_lag_us = None;
        self.last_input_lag_us = None;
        self.last_decode_lag_us = None;
        self.max_lag_us = None;
        self.over_limit_since_us = None;
        self.catching_up = false;
        self.started_at_us = None;
        self.last_timestamp_us = None;
        self.last_observed_at_us = None;
        self.held_floor_us = None;
        self.held_floor_until_us = None;
        self.catch_up_starts = 0;
        self.skipped_frames = 0;
        self.skipped_us = 0;
        self.last_skipped_us = None;
    }

    /// いまの遅れの上限 (マイクロ秒)。床からの増加と絶対値の下限の大きい方
    fn limit_us(&self) -> i64 {
        ms_to_us(AUDIO_RECEIVE_CATCH_UP_MIN_MS).max(
            self.floor_us
                .unwrap_or(0)
                .saturating_add(ms_to_us(AUDIO_RECEIVE_CATCH_UP_GROWTH_MS)),
        )
    }

    /// 追いつきを始めてよいか
    ///
    /// 床から [`AUDIO_RECEIVE_CATCH_UP_JUMP_MS`] を超えて遅れている場合は、音声を映像へ
    /// 合わせられない状態であり、その場で始める。上限を超えた状態が
    /// [`AUDIO_RECEIVE_CATCH_UP_CONFIRM_MS`] 続いた場合も始める。どちらも、直近に始めて
    /// いれば [`AUDIO_RECEIVE_CATCH_UP_COOLDOWN_MS`] たっていることを要求する。
    ///
    /// 見る値は「復号が足している遅れ」である ([`AudioReceiveCatchUp::observe`] を参照)。
    fn should_start(&self, now_us: i64, decode_lag_us: i64) -> bool {
        if let Some(started_at_us) = self.started_at_us
            && now_us.saturating_sub(started_at_us) < ms_to_us(AUDIO_RECEIVE_CATCH_UP_COOLDOWN_MS)
        {
            return false;
        }
        if decode_lag_us
            > self
                .floor_us
                .unwrap_or(0)
                .saturating_add(ms_to_us(AUDIO_RECEIVE_CATCH_UP_JUMP_MS))
        {
            return true;
        }
        let Some(over_limit_since_us) = self.over_limit_since_us else {
            return false;
        };
        now_us.saturating_sub(over_limit_since_us) >= ms_to_us(AUDIO_RECEIVE_CATCH_UP_CONFIRM_MS)
    }

    /// 健全な遅れ (床) を、直近の窓の最小値として更新する
    ///
    /// 窓より古い観測は捨てる。観測が窓の長さより途切れたときは、途切れる前の床を窓の長さの
    /// 分だけ保つ。途切れている間に溜まった分をそのまま床にすると、遅れが健全な値とみなされ、
    /// 追いつきが始まらなくなる。
    ///
    /// `decode_lag_us` は「復号が足している遅れ」 (出力の遅れ − 入力の遅れ) である。入力の
    /// 遅れを引かずに出力の遅れで床を求めると、受信側が溜めたまま戻らない状態でも床が出力の
    /// 遅れへ追いつき、作り直しが要求されなくなる (このモジュールの先頭のコメントを参照)。
    fn update_floor(&mut self, now_us: i64, decode_lag_us: i64) {
        let previous_at_us = self.last_observed_at_us;
        self.last_observed_at_us = Some(now_us);
        if let Some(previous_at_us) = previous_at_us
            && now_us.saturating_sub(previous_at_us)
                > ms_to_us(AUDIO_RECEIVE_CATCH_UP_FLOOR_WINDOW_MS)
        {
            self.held_floor_us = self.floor_us;
            self.held_floor_until_us =
                Some(now_us.saturating_add(ms_to_us(AUDIO_RECEIVE_CATCH_UP_FLOOR_WINDOW_MS)));
        }
        self.floor_samples.push_back(FloorSample {
            at_us: now_us,
            lag_us: decode_lag_us,
        });
        let oldest_us = now_us.saturating_sub(ms_to_us(AUDIO_RECEIVE_CATCH_UP_FLOOR_WINDOW_MS));
        while let Some(sample) = self.floor_samples.front() {
            if sample.at_us > oldest_us {
                break;
            }
            self.floor_samples.pop_front();
        }
        let mut floor_us = decode_lag_us;
        for sample in &self.floor_samples {
            floor_us = floor_us.min(sample.lag_us);
        }
        if let (Some(held_floor_us), Some(held_floor_until_us)) =
            (self.held_floor_us, self.held_floor_until_us)
            && now_us < held_floor_until_us
        {
            floor_us = floor_us.min(held_floor_us);
        }
        self.floor_us = Some(floor_us);
    }

    /// 追いつきで飛んだ音の長さを、TIMESTAMP の跳びから測る
    ///
    /// 復号器を作り直すと、中に溜まっていた投入が捨てられる。捨てられた分は、次に復号された
    /// 音の TIMESTAMP が前の音から跳ぶことで現れる。跳びから 1 音の長さを引いた分が鳴らなかった
    /// 音である。数えるのは追いつきの最中だけであり、それ以外の跳び (配信側の欠落、relay の
    /// cache の再送など) は数えない。
    ///
    /// 1 音の長さと跳びはマイクロ秒で扱う。ミリ秒の整数で計算すると、23.22 ms のフレームで
    /// 0.22 ms の切り捨てが起き、飛んだ音の数と長さの統計が moqt-js とずれる。
    ///
    /// 判定は、TIMESTAMP の更新、TIMESTAMP が無い場合、1 音の長さが 0 の場合、跳びの判定、
    /// 追いつき中かの判定の順に行う。どこで打ち切っても数える値は変わらないため、順序自体は
    /// 観測される値に影響しない。跳びの判定を追いつき中かの判定より先に置くのは、数える対象を
    /// 「追いつきの最中に起きた跳び」として読めるようにするためである。
    fn measure_skip(&mut self, observation: AudioReceiveCatchUpObservation) {
        let previous_us = self.last_timestamp_us;
        self.last_timestamp_us = Some(observation.timestamp_us);
        let Some(previous_us) = previous_us else {
            return;
        };
        let frame_us = observation.duration_us.max(0);
        if frame_us == 0 {
            return;
        }
        let step_us = observation.timestamp_us.saturating_sub(previous_us);
        if step_us.saturating_sub(frame_us) <= ms_to_us(AUDIO_RECEIVE_CATCH_UP_SKIP_MIN_MS) {
            return;
        }
        if !self.catching_up {
            return;
        }
        let skipped_us = step_us.saturating_sub(frame_us);
        // 飛んだ音の数は、跳びが 1 音の長さの何個分かを四捨五入して 1 を引いた数にする
        // (TIMESTAMP の格子が 1 音の長さと一致しない環境でも 1 以上になる)。四捨五入は
        // 0.5 を正の無限大の側へ丸める (JS の Math.round と同じ向き)
        let step_us = i128::from(step_us);
        let frame_us = i128::from(frame_us);
        let rounded_ratio = (step_us * 2 + frame_us) / (frame_us * 2);
        let frames = rounded_ratio.saturating_sub(1).max(1);
        let Ok(frames) = u64::try_from(frames) else {
            return;
        };
        self.skipped_frames = self.skipped_frames.saturating_add(frames);
        self.skipped_us = self.skipped_us.saturating_add(skipped_us);
        self.last_skipped_us = Some(skipped_us);
    }
}

impl Default for AudioReceiveCatchUp {
    fn default() -> Self {
        Self::new()
    }
}
