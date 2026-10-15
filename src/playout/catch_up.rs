//! 受信側の音声が遅れて固定されたら、復号器の作り直しを求める
//!
//! 復号器の中に未処理の音が溜まると、溜まった分だけ復号の出力が送られた TIMESTAMP から
//! 遅れたまま固定される。溜まった分は実時間と同じ速さでしか出てこないため、待っても戻らない。
//! 鳴らす音を捨てても、捨てた分は復号器から出てきた後の音であり、中の分は減らない。そこで
//! 遅れが健全な値から外れたら復号器の作り直しを求め、溜まった分を捨てて live へ戻す。
//!
//! - 健全な遅れ (床) は直近 [`AUDIO_RECEIVE_CATCH_UP_FLOOR_WINDOW_MS`] の最小値である。
//!   経路の遅延はここへ吸収され、その上での増加だけを見る
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
/// 同じ値である。復号の出力が健全な遅れからこれを超えて遅れているとき、受信側は音声を
/// 映像へ合わせられない。待たずに始めて溜まった分を捨てる方が、遅れた音を鳴らし続けるより
/// 良い。
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

/// 床を求める 1 つの観測 (ミリ秒)
#[derive(Debug, Clone, Copy)]
struct FloorSample {
    /// 観測した時刻
    at_ms: i64,
    /// そのときの遅れ
    lag_ms: i64,
}

/// 復号の出力 1 つの観測
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioReceiveCatchUpObservation {
    /// 復号の出力が届いた時刻 (呼び出し側の単調な時計のミリ秒)
    pub now_ms: i64,
    /// 復号の出力の遅れ (ミリ秒)
    ///
    /// 到着の壁時計 − TIMESTAMP である。壁時計の TIMESTAMP を持たない音では測れないため、
    /// 呼び出し側が観測しない。
    pub lag_ms: i64,
    /// 音の長さ (ミリ秒)
    pub duration_ms: i64,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioReceiveCatchUpStats {
    /// 直近に観測した遅れ (ミリ秒)。まだ観測していなければ `None`
    pub lag_ms: Option<i64>,
    /// 直近の窓で観測した健全な遅れ (床、ミリ秒)。まだ観測していなければ `None`
    pub floor_ms: Option<i64>,
    /// 観測した最大の遅れ (ミリ秒)。まだ観測していなければ `None`
    pub max_lag_ms: Option<i64>,
    /// いまの遅れの上限 (ミリ秒)
    pub limit_ms: i64,
    /// 追いつきの最中か
    pub catching_up: bool,
    /// 追いつきを始めた回数
    pub catch_up_starts: u64,
    /// 追いつきで飛んだ音の数
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
pub struct AudioReceiveCatchUp {
    /// 床を求める観測 (古い順)
    floor_samples: VecDeque<FloorSample>,
    /// 直近の窓の最小の遅れ (ミリ秒)。まだ観測していなければ `None`
    floor_ms: Option<i64>,
    /// 直近に観測した遅れ (ミリ秒)
    last_lag_ms: Option<i64>,
    /// 観測した最大の遅れ (ミリ秒)
    max_lag_ms: Option<i64>,
    /// 上限を超え始めた時刻 (ミリ秒)。超えていなければ `None`
    over_limit_since_ms: Option<i64>,
    /// 追いつきの最中か
    catching_up: bool,
    /// 直近に追いつきを始めた時刻 (ミリ秒)
    started_at_ms: Option<i64>,
    /// 直近に観測した TIMESTAMP (マイクロ秒)
    last_timestamp_us: Option<i64>,
    /// 直近に観測した時刻 (ミリ秒)
    last_observed_at_ms: Option<i64>,
    /// 観測が途切れたときに保つ、途切れる前の床 (ミリ秒)
    held_floor_ms: Option<i64>,
    /// 保つ床の期限 (ミリ秒)
    held_floor_until_ms: Option<i64>,
    catch_up_starts: u64,
    skipped_frames: u64,
    skipped_ms: i64,
    last_skipped_ms: Option<i64>,
}

impl AudioReceiveCatchUp {
    /// 何も観測していない状態で作る
    pub fn new() -> Self {
        Self {
            floor_samples: VecDeque::new(),
            floor_ms: None,
            last_lag_ms: None,
            max_lag_ms: None,
            over_limit_since_ms: None,
            catching_up: false,
            started_at_ms: None,
            last_timestamp_us: None,
            last_observed_at_ms: None,
            held_floor_ms: None,
            held_floor_until_ms: None,
            catch_up_starts: 0,
            skipped_frames: 0,
            skipped_ms: 0,
            last_skipped_ms: None,
        }
    }

    /// 復号の出力 1 つを観測し、復号器を作り直すかを決める
    ///
    /// 遅れが上限の中へ戻ったら追いつきを終える。超えていれば、始める条件を満たすときに
    /// [`AudioReceiveCatchUpDecision::rebuild_decoder`] を真にして返す。
    ///
    /// 遅れが戻るまで始め直すことと、穴 (TIMESTAMP の跳び) の直後でも始めることは、実測に
    /// 基づく。次の実測は moqt-js が実リレー、パケロス 5 %、40 秒で測ったものである
    /// (moqt-rs で実リレーの再測が必要になった時点で測り直す)。
    ///
    /// - 作り直す間隔を、遅れが戻らないまま繰り返すときだけ 2 倍にする案は効かなかった。
    ///   損失下では遅れが一度上限へ戻ってからまた跳ぶため、間隔を伸ばす条件が成立せず、
    ///   作り直しの回数は現行と同じ 30〜31 回だった
    /// - 穴の直後を始めない案も効かなかった。穴は 0.4 秒ごとに空くため、直後の 1 秒を避けても
    ///   作り直しの回数は 30 回のままだった。60 秒間避けると作り直しは 1 回になるが、遅れが
    ///   1458 ms まで育ち、鳴らした長さも 30 秒で 27835 ms へ落ちた (捨てない分だけ遅れが
    ///   積み上がる)
    /// - どちらの案も採らない。作り直しは損失下でも live を保つ役目を果たしており、飛ぶ音は
    ///   遅れを積み上げないための代価である
    pub fn observe(
        &mut self,
        observation: AudioReceiveCatchUpObservation,
    ) -> AudioReceiveCatchUpDecision {
        let lag_ms = observation.lag_ms;
        self.last_lag_ms = Some(lag_ms);
        self.max_lag_ms = Some(
            self.max_lag_ms
                .map_or(lag_ms, |max_lag_ms| max_lag_ms.max(lag_ms)),
        );
        self.update_floor(observation.now_ms, lag_ms);
        let limit_ms = self.limit_ms();

        // 追いつきで飛んだ音を、TIMESTAMP の跳びから測る (遅れが戻る判定より先に行う)
        self.measure_skip(observation);

        if lag_ms <= limit_ms {
            // 上限の中へ戻った (作り直しが効いた)。次に上限を超えたら、また始められる
            self.catching_up = false;
            self.over_limit_since_ms = None;
            return AudioReceiveCatchUpDecision {
                rebuild_decoder: false,
            };
        }
        if self.over_limit_since_ms.is_none() {
            self.over_limit_since_ms = Some(observation.now_ms);
        }
        if !self.should_start(observation.now_ms, lag_ms) {
            return AudioReceiveCatchUpDecision {
                rebuild_decoder: false,
            };
        }
        // 遅れが上限へ戻るまで始め直す。1 回の作り直しでは戻らない状態がある
        self.catching_up = true;
        self.started_at_ms = Some(observation.now_ms);
        self.catch_up_starts = self.catch_up_starts.saturating_add(1);
        AudioReceiveCatchUpDecision {
            rebuild_decoder: true,
        }
    }

    /// 観測値 (統計とログに出す)
    pub fn snapshot(&self) -> AudioReceiveCatchUpStats {
        AudioReceiveCatchUpStats {
            lag_ms: self.last_lag_ms,
            floor_ms: self.floor_ms,
            max_lag_ms: self.max_lag_ms,
            limit_ms: self.limit_ms(),
            catching_up: self.catching_up,
            catch_up_starts: self.catch_up_starts,
            skipped_frames: self.skipped_frames,
            skipped_ms: self.skipped_ms,
            last_skipped_ms: self.last_skipped_ms,
        }
    }

    /// 観測と累積を消す (購読のやり直し)
    pub fn reset(&mut self) {
        self.floor_samples.clear();
        self.floor_ms = None;
        self.last_lag_ms = None;
        self.max_lag_ms = None;
        self.over_limit_since_ms = None;
        self.catching_up = false;
        self.started_at_ms = None;
        self.last_timestamp_us = None;
        self.last_observed_at_ms = None;
        self.held_floor_ms = None;
        self.held_floor_until_ms = None;
        self.catch_up_starts = 0;
        self.skipped_frames = 0;
        self.skipped_ms = 0;
        self.last_skipped_ms = None;
    }

    /// いまの遅れの上限 (ミリ秒)。床からの増加と絶対値の下限の大きい方
    fn limit_ms(&self) -> i64 {
        AUDIO_RECEIVE_CATCH_UP_MIN_MS.max(
            self.floor_ms
                .unwrap_or(0)
                .saturating_add(AUDIO_RECEIVE_CATCH_UP_GROWTH_MS),
        )
    }

    /// 追いつきを始めてよいか
    ///
    /// 床から [`AUDIO_RECEIVE_CATCH_UP_JUMP_MS`] を超えて遅れている場合は、音声を映像へ
    /// 合わせられない状態であり、その場で始める。上限を超えた状態が
    /// [`AUDIO_RECEIVE_CATCH_UP_CONFIRM_MS`] 続いた場合も始める。どちらも、直近に始めて
    /// いれば [`AUDIO_RECEIVE_CATCH_UP_COOLDOWN_MS`] たっていることを要求する。
    fn should_start(&self, now_ms: i64, lag_ms: i64) -> bool {
        if let Some(started_at_ms) = self.started_at_ms
            && now_ms.saturating_sub(started_at_ms) < AUDIO_RECEIVE_CATCH_UP_COOLDOWN_MS
        {
            return false;
        }
        if lag_ms
            > self
                .floor_ms
                .unwrap_or(0)
                .saturating_add(AUDIO_RECEIVE_CATCH_UP_JUMP_MS)
        {
            return true;
        }
        let Some(over_limit_since_ms) = self.over_limit_since_ms else {
            return false;
        };
        now_ms.saturating_sub(over_limit_since_ms) >= AUDIO_RECEIVE_CATCH_UP_CONFIRM_MS
    }

    /// 健全な遅れ (床) を、直近の窓の最小値として更新する
    ///
    /// 窓より古い観測は捨てる。観測が窓の長さより途切れたときは、途切れる前の床を窓の長さの
    /// 分だけ保つ。途切れている間に溜まった分をそのまま床にすると、遅れが健全な値とみなされ、
    /// 追いつきが始まらなくなる。
    fn update_floor(&mut self, now_ms: i64, lag_ms: i64) {
        let previous_at_ms = self.last_observed_at_ms;
        self.last_observed_at_ms = Some(now_ms);
        if let Some(previous_at_ms) = previous_at_ms
            && now_ms.saturating_sub(previous_at_ms) > AUDIO_RECEIVE_CATCH_UP_FLOOR_WINDOW_MS
        {
            self.held_floor_ms = self.floor_ms;
            self.held_floor_until_ms =
                Some(now_ms.saturating_add(AUDIO_RECEIVE_CATCH_UP_FLOOR_WINDOW_MS));
        }
        self.floor_samples.push_back(FloorSample {
            at_ms: now_ms,
            lag_ms,
        });
        let oldest_ms = now_ms.saturating_sub(AUDIO_RECEIVE_CATCH_UP_FLOOR_WINDOW_MS);
        while let Some(sample) = self.floor_samples.front() {
            if sample.at_ms > oldest_ms {
                break;
            }
            self.floor_samples.pop_front();
        }
        let mut floor_ms = lag_ms;
        for sample in &self.floor_samples {
            floor_ms = floor_ms.min(sample.lag_ms);
        }
        if let (Some(held_floor_ms), Some(held_floor_until_ms)) =
            (self.held_floor_ms, self.held_floor_until_ms)
            && now_ms < held_floor_until_ms
        {
            floor_ms = floor_ms.min(held_floor_ms);
        }
        self.floor_ms = Some(floor_ms);
    }

    /// 追いつきで飛んだ音の長さを、TIMESTAMP の跳びから測る
    ///
    /// 復号器を作り直すと、中に溜まっていた投入が捨てられる。捨てられた分は、次に復号された
    /// 音の TIMESTAMP が前の音から跳ぶことで現れる。跳びから 1 音の長さを引いた分が鳴らなかった
    /// 音である。数えるのは追いつきの最中だけであり、それ以外の跳び (配信側の欠落、relay の
    /// cache の再送など) は数えない。
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
        let frame_us = observation.duration_ms.max(0).saturating_mul(1_000);
        if frame_us == 0 {
            return;
        }
        let step_us = observation.timestamp_us.saturating_sub(previous_us);
        if step_us.saturating_sub(frame_us)
            <= AUDIO_RECEIVE_CATCH_UP_SKIP_MIN_MS.saturating_mul(1_000)
        {
            return;
        }
        if !self.catching_up {
            return;
        }
        let skipped_us = step_us.saturating_sub(frame_us);
        // 飛んだ音の数は、跳びが 1 音の長さの何個分かを四捨五入して 1 を引いた数にする
        // (TIMESTAMP の格子が音の長さと一致しない環境でも 1 以上になる)
        let frames = step_us
            .saturating_add(frame_us / 2)
            .saturating_div(frame_us)
            .saturating_sub(1)
            .max(1);
        let Ok(frames) = u64::try_from(frames) else {
            return;
        };
        self.skipped_frames = self.skipped_frames.saturating_add(frames);
        self.skipped_ms = self.skipped_ms.saturating_add(skipped_us / 1_000);
        self.last_skipped_ms = Some(skipped_us / 1_000);
    }
}

impl Default for AudioReceiveCatchUp {
    fn default() -> Self {
        Self::new()
    }
}
