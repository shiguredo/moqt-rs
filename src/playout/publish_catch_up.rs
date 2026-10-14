//! 配信側の音声が遅れたらフレームを捨てて live へ戻す
//!
//! 音声の配信は、読み出したフレームを符号化の出力を待たずに投入する。符号化が実時間に
//! 追いつかなくなると投入したフレームがキューに溜まり、出力が返るまでの待ちが伸びる。
//! キューに溜まった分は実時間と同じ速さでしかはけないため、投入を続ける限り遅れは減らない。
//! 遅れは受信側の基準 (復号の出力 − 送られた TIMESTAMP) をそのまま押し上げ、A/V の基準の
//! 差を開かせる。
//!
//! - 読み出しの遅れが上限を超えたフレームは、その場で捨てる。受信側の基準の遅れにそのまま
//!   出るためであり、キューに溜まっていないため状態は持ち越さない
//! - 符号化のキューが上限を超えたら、[`AUDIO_PUBLISH_CATCH_UP_RESUME_MS`] (1 パケット) 以下に
//!   減るまで投入をやめる。減る前に再開すると、キューに残った分がはけないまま次のフレームが
//!   入り、捨てるかどうかが往復して音声が送られなくなる
//! - 始めるのは、上限を超えた状態が続いたときだけにする。読み出しがまとめて行われると
//!   1 フレームだけ超過することがあり、その場で始めると遅れが減らないまま状態が往復する
//! - 投入の記録は FIFO で持ち、符号化の出力とは順番で対応づける。出力の TIMESTAMP は
//!   「符号化したサンプル数」から作る連続値であり、捨てた分だけ投入より古くなるためである
//!
//! 判定だけを行い、符号化器には触れない。時刻と値は呼び出し側が引数で渡す。

use alloc::collections::{BTreeMap, VecDeque};

/// 健全な遅れが小さいときに許す、遅れの上限 (ミリ秒)
///
/// 音声の 1 パケットは 20 ms であり、これは 3 パケット分にあたる。
pub const AUDIO_PUBLISH_CATCH_UP_MIN_MS: i64 = 60;

/// 健全な遅れが大きい環境で、そこからさらに許す遅れ (ミリ秒)
///
/// 2 パケット分である。遅れは経路と符号化の揺らぎで数十 ms 動くため、1 フレームの揺らぎで
/// 始めない幅としてこれを許す。
pub const AUDIO_PUBLISH_CATCH_UP_GROWTH_MS: i64 = 40;

/// 追いつきをやめて投入を再開する、キューに残ってよい音声の長さ (ミリ秒)
///
/// 1 パケット分である。これ以下なら、投入を再開しても次のフレームが待たずに符号化される。
pub const AUDIO_PUBLISH_CATCH_UP_RESUME_MS: i64 = 20;

/// 追いつきを始める前に、上限を超えた状態が続くことを要求する時間 (ミリ秒)
///
/// 1 フレームの観測だけで始めると、読み出しがまとめて行われただけでも始まる。100 ms (20 ms
/// のフレームで 5 個) の持続を要求する。
pub const AUDIO_PUBLISH_CATCH_UP_CONFIRM_MS: i64 = 100;

/// 符号化のキューが単独で上限を超えた状態が続くことを要求するフレーム数
///
/// キューが単独で上限を超えている状態は、実時間に追いつかない状態そのものであり、キューが
/// はけるまで続く。読み出しの揺らぎでは起きないため、短い確認で始めてよい。
pub const AUDIO_PUBLISH_CATCH_UP_QUEUE_CONFIRM_FRAMES: u64 = 2;

/// 追いつきをやめた後、次を始めない時間 (ミリ秒)
///
/// 遅れだけを根拠にした開始を止める。キューが単独で上限を超え続けている場合は、待つと
/// 遅れが伸びるため直ちに始める。
pub const AUDIO_PUBLISH_CATCH_UP_COOLDOWN_MS: i64 = 1_000;

/// 出力が返らない記録を捨てるまでの時間 (ミリ秒)
///
/// 符号化の失敗や配信の停止で完了が返らない記録が残り続けると、キューの長さが実際より
/// 大きく見えたままになる。
pub const AUDIO_PUBLISH_CATCH_UP_PENDING_TIMEOUT_MS: i64 = 5_000;

/// 追いつきの方針
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPublishCatchUpPolicy {
    /// 遅れたフレームを捨てて live へ戻す
    Drop,
    /// 捨てずに観測だけを続ける (音楽・効果音のように欠落を許さない音向け)
    Keep,
}

/// 読み出したフレーム 1 つの観測 (マイクロ秒とミリ秒)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPublishCatchUpFrame {
    /// 読み出した時刻 (呼び出し側の単調な時計のミリ秒)
    pub now_ms: i64,
    /// フレームの TIMESTAMP (マイクロ秒)
    pub timestamp_us: i64,
    /// TIMESTAMP の補正を決めるために読んだ壁時計 (Unix epoch マイクロ秒)
    pub read_wall_clock_us: i64,
    /// TIMESTAMP に足している補正 (マイクロ秒)。まだ決まっていなければ `None`
    pub applied_offset_us: Option<i64>,
    /// フレームの長さ (マイクロ秒)。分からなければ `None` (直前のフレームと同じとみなす)
    pub duration_us: Option<i64>,
}

/// 符号化の出力 1 つの観測
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPublishCatchUpChunk {
    /// 出力の TIMESTAMP (マイクロ秒。対応づけには使わない)
    pub timestamp_us: i64,
    /// 出力が覆う長さ (マイクロ秒)。分からなければ `None` (1 フレーム分とみなす)
    pub duration_us: Option<i64>,
}

/// 符号化の出力を記録した結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPublishCatchUpChunkResult {
    /// この出力が覆う最初の投入の TIMESTAMP (マイクロ秒)。対応づけられなければ `None`
    pub timestamp_us: Option<i64>,
}

/// 送信キューへ入れたことの記録
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPublishCatchUpSendStart {
    /// 入れた時刻 (呼び出し側の単調な時計のミリ秒)
    pub now_ms: i64,
    /// 送るフレームの TIMESTAMP (マイクロ秒)
    pub timestamp_us: i64,
    /// そのフレームを読んだ壁時計 (Unix epoch ミリ秒)
    pub captured_wall_clock_ms: i64,
    /// フレームの長さ (ミリ秒)
    pub duration_ms: i64,
}

/// 送出が完了したことの記録
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPublishCatchUpSendComplete {
    /// 完了した時刻 (呼び出し側の単調な時計のミリ秒)
    pub now_ms: i64,
    /// 送ったフレームの TIMESTAMP (マイクロ秒)
    pub timestamp_us: i64,
    /// 送信が終わった壁時計 (Unix epoch ミリ秒)
    pub completed_wall_clock_ms: i64,
}

/// 配信側の音声の追いつきの観測値 (統計とログに出す)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPublishCatchUpStats {
    /// いまの方針
    pub policy: AudioPublishCatchUpPolicy,
    /// 捨てたフレームの数
    pub dropped_frames: u64,
    /// 捨てたフレームの長さの合計 (ミリ秒)
    pub dropped_ms: i64,
    /// 直近に観測した遅れ (読み出し + キュー、ミリ秒)。まだ観測していなければ `None`
    pub lag_ms: Option<i64>,
    /// 観測した最小の遅れ (床、ミリ秒)。まだ観測していなければ `None`
    pub floor_ms: Option<i64>,
    /// 観測した最大の遅れ (ミリ秒)
    pub max_lag_ms: Option<i64>,
    /// 符号化のキューに残っている音声の長さ (ミリ秒)
    pub pending_ms: i64,
    /// 直近に観測した読み出しの遅れ (ミリ秒)
    pub read_lag_ms: i64,
    /// いまの遅れの上限 (ミリ秒)
    pub limit_ms: i64,
    /// 符号化のキューに残っているフレームの数
    pub pending_frames: u64,
    /// 追いつきの最中か
    pub catching_up: bool,
    /// 追いつきを始めた回数
    pub catch_up_starts: u64,
    /// 送信のキューに残っている音声の長さ (ミリ秒)
    pub send_queue_ms: i64,
    /// 送信のキューに残っているフレームの数
    pub send_queue_frames: u64,
    /// 直近に観測した「読んでから送り終えるまで」の長さ (ミリ秒)
    pub send_lag_ms: Option<i64>,
    /// 観測した「読んでから送り終えるまで」の最大 (ミリ秒)
    pub max_send_lag_ms: Option<i64>,
}

/// 符号化へ渡したまま出力が返っていないフレーム
#[derive(Debug, Clone, Copy)]
struct PendingFrame {
    /// 投入したフレームの TIMESTAMP (マイクロ秒)
    timestamp_us: i64,
    /// 読み出した時刻 (ミリ秒)
    read_at_ms: i64,
    /// 残っている長さ (ミリ秒)。出力が覆った分だけ減る
    duration_ms: i64,
}

/// 送信のキューへ入れたまま完了が返っていないフレーム
#[derive(Debug, Clone, Copy)]
struct PendingSend {
    /// 入れた時刻 (ミリ秒)
    queued_at_ms: i64,
    /// そのフレームを読んだ壁時計 (Unix epoch ミリ秒)
    captured_wall_clock_ms: i64,
    /// フレームの長さ (ミリ秒)
    duration_ms: i64,
}

/// 配信側の音声の追いつき (配信ごとに 1 つ持つ)
///
/// [`AudioPublishCatchUp::evaluate`] に読み出したフレームを渡し、返ってきた真偽で符号化へ
/// 渡すかどうかを決める。符号化の出力は [`AudioPublishCatchUp::record_encoded_chunk`] で
/// 記録する。
pub struct AudioPublishCatchUp {
    /// いまの方針
    policy: AudioPublishCatchUpPolicy,
    /// 符号化へ渡したまま出力が返っていないフレーム (投入した順)
    pending: VecDeque<PendingFrame>,
    /// 符号化のキューに残っている長さ (ミリ秒)
    pending_ms: i64,
    /// 送信のキューへ入れたまま完了が返っていないフレーム (TIMESTAMP ごと、古い順)
    pending_sends: BTreeMap<i64, VecDeque<PendingSend>>,
    /// 送信のキューに残っているフレームの数
    pending_send_frames: u64,
    /// 送信のキューに残っている長さ (ミリ秒)
    pending_send_ms: i64,
    /// 観測した最小の遅れ (ミリ秒)。まだ観測していなければ `None`
    floor_ms: Option<i64>,
    /// 観測した最大の遅れ (ミリ秒)
    max_lag_ms: Option<i64>,
    /// 直近に観測した遅れ (ミリ秒)
    last_lag_ms: Option<i64>,
    /// 直近に観測した読み出しの遅れ (ミリ秒)
    last_read_lag_ms: i64,
    /// 直近に観測した「読んでから送り終えるまで」の長さ (ミリ秒)
    last_send_lag_ms: Option<i64>,
    /// 観測した「読んでから送り終えるまで」の最大 (ミリ秒)
    max_send_lag_ms: Option<i64>,
    /// 追いつきの最中か
    catching_up: bool,
    /// 上限を超え始めた時刻 (ミリ秒)。超えていなければ `None`
    over_limit_since_ms: Option<i64>,
    /// 符号化のキューが単独で上限を超えた状態が続いたフレーム数
    queue_over_frames: u64,
    /// 直近に追いつきをやめた時刻 (ミリ秒)
    resumed_at_ms: Option<i64>,
    /// 追いつきを始めた回数
    catch_up_starts: u64,
    /// 捨てたフレームの数
    dropped_frames: u64,
    /// 捨てたフレームの長さの合計 (ミリ秒)
    dropped_ms: i64,
    /// 直前に読み出したフレームの長さ (ミリ秒)。長さが分からないフレームに使う
    last_frame_duration_ms: i64,
}

impl AudioPublishCatchUp {
    /// 既定の方針 (`drop`) で作る
    pub fn new() -> Self {
        Self::with_policy(AudioPublishCatchUpPolicy::Drop)
    }

    /// 方針を決めて作る
    pub fn with_policy(policy: AudioPublishCatchUpPolicy) -> Self {
        Self {
            policy,
            pending: VecDeque::new(),
            pending_ms: 0,
            pending_sends: BTreeMap::new(),
            pending_send_frames: 0,
            pending_send_ms: 0,
            floor_ms: None,
            max_lag_ms: None,
            last_lag_ms: None,
            last_read_lag_ms: 0,
            last_send_lag_ms: None,
            max_send_lag_ms: None,
            catching_up: false,
            over_limit_since_ms: None,
            queue_over_frames: 0,
            resumed_at_ms: None,
            catch_up_starts: 0,
            dropped_frames: 0,
            dropped_ms: 0,
            last_frame_duration_ms: 0,
        }
    }

    /// いまの方針
    pub fn policy(&self) -> AudioPublishCatchUpPolicy {
        self.policy
    }

    /// 読み出したフレームを符号化するかを決める
    ///
    /// 真を返したら符号化へ渡し、[`AudioPublishCatchUp::record_encoded_chunk`] で出力を
    /// 記録する。偽なら符号化せずに捨てる (呼び出し側がフレームを閉じる)。
    pub fn evaluate(&mut self, frame: AudioPublishCatchUpFrame) -> bool {
        let read_lag_ms = self.read_lag_ms(&frame);
        let lag_ms = read_lag_ms.saturating_add(self.pending_ms);
        self.last_read_lag_ms = read_lag_ms;
        self.last_lag_ms = Some(lag_ms);
        self.max_lag_ms = Some(
            self.max_lag_ms
                .map_or(lag_ms, |max_lag_ms| max_lag_ms.max(lag_ms)),
        );
        self.floor_ms = Some(
            self.floor_ms
                .map_or(lag_ms, |floor_ms| floor_ms.min(lag_ms)),
        );

        // 長さが分からないフレームは、直前のフレームと同じ長さとみなす (捨てた長さと
        // キューの長さの集計用)
        let duration_ms = self.duration_ms(&frame);
        self.last_frame_duration_ms = duration_ms;

        // 出力が返らなかった記録を捨てる (記録が増え続けないようにする)
        self.prune_pending(frame.now_ms);
        self.prune_pending_sends(frame.now_ms);

        if self.should_drop(lag_ms, read_lag_ms, frame.now_ms) {
            self.dropped_frames = self.dropped_frames.saturating_add(1);
            self.dropped_ms = self.dropped_ms.saturating_add(duration_ms);
            return false;
        }
        self.pending.push_back(PendingFrame {
            timestamp_us: frame.timestamp_us,
            read_at_ms: frame.now_ms,
            duration_ms,
        });
        self.pending_ms = self.pending_ms.saturating_add(duration_ms);
        true
    }

    /// 符号化の出力を記録し、この出力が覆う最初の投入の TIMESTAMP を返す
    ///
    /// 出力は 1 つ以上の投入を覆いうる (10 ms のフレームが 20 ms のパケットにまとまるなど)。
    /// 出力の TIMESTAMP で対応づけないのは、それが「符号化したサンプル数」から作る連続した
    /// 値であり、フレームを捨てた分だけ投入より古くなるためである。出力は投入の順に返る
    /// ため、順番で対応づける。
    pub fn record_encoded_chunk(
        &mut self,
        chunk: AudioPublishCatchUpChunk,
    ) -> AudioPublishCatchUpChunkResult {
        let Some(first) = self.pending.front() else {
            // 対応づけられる投入が無い (配信を始める前の出力、記録を捨てた後など)
            return AudioPublishCatchUpChunkResult { timestamp_us: None };
        };
        let first_timestamp_us = first.timestamp_us;
        // 消費する長さ。長さが分からない出力は 1 フレーム分だけ消費する
        let mut remain_ms = match chunk.duration_us {
            Some(duration_us) if duration_us > 0 => duration_us / 1_000,
            _ => first.duration_ms,
        };
        while remain_ms > 0 {
            let Some(frame) = self.pending.front_mut() else {
                // 記録より長い出力 (捨てた分を覆った、配信の停止をまたいだなど)。残りは捨てる
                break;
            };
            if frame.duration_ms <= remain_ms {
                let duration_ms = frame.duration_ms;
                self.pending.pop_front();
                self.pending_ms = self.pending_ms.saturating_sub(duration_ms);
                remain_ms -= duration_ms;
                continue;
            }
            // 1 つの投入が 2 つの出力にまたがる場合は、残りを次に持ち越す
            frame.duration_ms -= remain_ms;
            self.pending_ms = self.pending_ms.saturating_sub(remain_ms);
            remain_ms = 0;
        }
        self.pending_ms = self.pending_ms.max(0);
        AudioPublishCatchUpChunkResult {
            timestamp_us: Some(first_timestamp_us),
        }
    }

    /// 送信のキューへ入れたことを記録する
    ///
    /// 配信側が足す遅れには、符号化のキューだけでなく送信のキューに残っている分も含まれる。
    /// 送信の完了は送信 API の返値であり、ストリームの生成と書き込みの待ち (backpressure) を
    /// 含む。計器として残す。
    pub fn record_send_start(&mut self, start: AudioPublishCatchUpSendStart) {
        let send = PendingSend {
            queued_at_ms: start.now_ms,
            captured_wall_clock_ms: start.captured_wall_clock_ms,
            duration_ms: start.duration_ms,
        };
        self.pending_sends
            .entry(start.timestamp_us)
            .or_default()
            .push_back(send);
        self.pending_send_frames = self.pending_send_frames.saturating_add(1);
        self.pending_send_ms = self.pending_send_ms.saturating_add(start.duration_ms);
    }

    /// 送出が完了したことを記録する
    ///
    /// 「読んでから送信が終わるまで」を測る。配信側が足した遅れのうち、受信側の基準の遅れへ
    /// そのまま出る値である。対応づけは TIMESTAMP で行うため完了が順不同でもよい。同じ
    /// TIMESTAMP のフレームが続くときは、先に入れた記録 (古い方) から消す。
    pub fn record_send_complete(&mut self, complete: AudioPublishCatchUpSendComplete) {
        let send = {
            let Some(sends) = self.pending_sends.get_mut(&complete.timestamp_us) else {
                return;
            };
            let Some(send) = sends.pop_front() else {
                return;
            };
            send
        };
        if self
            .pending_sends
            .get(&complete.timestamp_us)
            .is_some_and(VecDeque::is_empty)
        {
            self.pending_sends.remove(&complete.timestamp_us);
        }
        self.pending_send_frames = self.pending_send_frames.saturating_sub(1);
        self.pending_send_ms = self.pending_send_ms.saturating_sub(send.duration_ms).max(0);
        let lag_ms = complete
            .completed_wall_clock_ms
            .saturating_sub(send.captured_wall_clock_ms);
        self.last_send_lag_ms = Some(lag_ms);
        self.max_send_lag_ms = Some(
            self.max_send_lag_ms
                .map_or(lag_ms, |max_send_lag_ms| max_send_lag_ms.max(lag_ms)),
        );
    }

    /// 観測値 (統計とログに出す)
    pub fn snapshot(&self) -> AudioPublishCatchUpStats {
        AudioPublishCatchUpStats {
            policy: self.policy,
            dropped_frames: self.dropped_frames,
            dropped_ms: self.dropped_ms,
            lag_ms: self.last_lag_ms,
            floor_ms: self.floor_ms,
            max_lag_ms: self.max_lag_ms,
            pending_ms: self.pending_ms,
            read_lag_ms: self.last_read_lag_ms,
            limit_ms: self.limit_ms(),
            pending_frames: self.pending_frames(),
            catching_up: self.catching_up,
            catch_up_starts: self.catch_up_starts,
            send_queue_ms: self.pending_send_ms,
            send_queue_frames: self.pending_send_frames,
            send_lag_ms: self.last_send_lag_ms,
            max_send_lag_ms: self.max_send_lag_ms,
        }
    }

    /// 配信のやり直しで観測と累積を消す
    pub fn reset(&mut self) {
        self.pending.clear();
        self.pending_ms = 0;
        self.pending_sends.clear();
        self.pending_send_frames = 0;
        self.pending_send_ms = 0;
        self.floor_ms = None;
        self.max_lag_ms = None;
        self.last_lag_ms = None;
        self.last_read_lag_ms = 0;
        self.last_send_lag_ms = None;
        self.max_send_lag_ms = None;
        self.catching_up = false;
        self.over_limit_since_ms = None;
        self.queue_over_frames = 0;
        self.resumed_at_ms = None;
        self.catch_up_starts = 0;
        self.dropped_frames = 0;
        self.dropped_ms = 0;
        self.last_frame_duration_ms = 0;
    }

    /// いまの遅れの上限 (ミリ秒)
    ///
    /// 健全な遅れ (床) からの増加と、絶対値の下限の大きい方である。
    pub fn limit_ms(&self) -> i64 {
        AUDIO_PUBLISH_CATCH_UP_MIN_MS.max(
            self.floor_ms
                .unwrap_or(0)
                .saturating_add(AUDIO_PUBLISH_CATCH_UP_GROWTH_MS),
        )
    }

    /// いま符号化せずに捨てるべきか
    ///
    /// - 読み出しの遅れが上限を超えたフレームは、その場で捨てる (状態を持ち越さない)
    /// - 追いつきの最中は、キューが 1 パケット以下まで減るか記録が空になるまで捨て続ける
    /// - 始めるのは、キューが単独で上限を超えた状態が続いたか、遅れが上限を超えた状態が
    ///   続いたときだけにする。遅れだけのときは、やめた直後のクールダウンも守る
    fn should_drop(&mut self, lag_ms: i64, read_lag_ms: i64, now_ms: i64) -> bool {
        if self.policy == AudioPublishCatchUpPolicy::Keep {
            return false;
        }
        let start_ms = self.limit_ms();
        // 上限を超えた状態がいつから続いているかを記録する (始めるかどうかは続いた時間で決める)
        if lag_ms > start_ms {
            if self.over_limit_since_ms.is_none() {
                self.over_limit_since_ms = Some(now_ms);
            }
        } else {
            self.over_limit_since_ms = None;
        }
        // 符号化のキューが単独で上限を超えているか (読み出しの遅れによらない、実時間に
        // 追いつかない状態の証拠)
        self.queue_over_frames = if self.pending_ms > start_ms {
            self.queue_over_frames.saturating_add(1)
        } else {
            0
        };
        if read_lag_ms > start_ms {
            return true;
        }
        if self.catching_up {
            // キューが 1 パケット以下まで減ったら投入を再開する。記録が 1 つも無いときも同じ
            // 扱いにするのは、追いつきの間は投入しないため符号化の出力も返らず、長さの集計が
            // ずれたまま残ると解除できないためである
            if self.pending_ms <= AUDIO_PUBLISH_CATCH_UP_RESUME_MS || self.pending_frames() == 0 {
                self.catching_up = false;
                self.resumed_at_ms = Some(now_ms);
                // やめた時点から数え直す (続いていた超過をそのまま次の開始の条件に使わない)
                self.over_limit_since_ms = if lag_ms > start_ms {
                    Some(now_ms)
                } else {
                    None
                };
            }
            return self.catching_up;
        }
        let queue_confirmed = self.queue_over_frames >= AUDIO_PUBLISH_CATCH_UP_QUEUE_CONFIRM_FRAMES;
        let lag_confirmed = self.over_limit_since_ms.is_some_and(|since_ms| {
            now_ms.saturating_sub(since_ms) >= AUDIO_PUBLISH_CATCH_UP_CONFIRM_MS
        });
        if !queue_confirmed && !lag_confirmed {
            return false;
        }
        // 遅れだけが超過している場合 (キューは上限を超えていない) は、やめた直後は始めない。
        // キューが単独で上限を超え続けている場合は実時間に追いつかない状態そのものであり、
        // 待つと遅れが伸びるため直ちに始める
        if !queue_confirmed
            && self.resumed_at_ms.is_some_and(|resumed_at_ms| {
                now_ms.saturating_sub(resumed_at_ms) < AUDIO_PUBLISH_CATCH_UP_COOLDOWN_MS
            })
        {
            return false;
        }
        self.catching_up = true;
        self.catch_up_starts = self.catch_up_starts.saturating_add(1);
        true
    }

    /// 読み出した時点の遅れ (ミリ秒)。補正がまだ決まっていなければ 0
    fn read_lag_ms(&self, frame: &AudioPublishCatchUpFrame) -> i64 {
        let Some(applied_offset_us) = frame.applied_offset_us else {
            return 0;
        };
        let lag_us = frame
            .read_wall_clock_us
            .saturating_sub(frame.timestamp_us)
            .saturating_sub(applied_offset_us);
        // 補正は読み出しの遅れの床であるため、床より早く読めたフレームの遅れは 0 にする
        lag_us.max(0) / 1_000
    }

    /// フレームの長さ (ミリ秒)。分からなければ直前のフレームの長さを使う
    fn duration_ms(&self, frame: &AudioPublishCatchUpFrame) -> i64 {
        match frame.duration_us {
            Some(duration_us) if duration_us > 0 => duration_us / 1_000,
            _ => self.last_frame_duration_ms,
        }
    }

    /// 符号化へ渡したまま出力が返っていないフレームの数
    fn pending_frames(&self) -> u64 {
        u64::try_from(self.pending.len()).unwrap_or(u64::MAX)
    }

    /// 出力が返らないままになった記録を捨てる
    ///
    /// 記録は読み出した順に入るため、古い方から見て残っていれば以降も残っている。
    fn prune_pending(&mut self, now_ms: i64) {
        let oldest_ms = now_ms.saturating_sub(AUDIO_PUBLISH_CATCH_UP_PENDING_TIMEOUT_MS);
        while let Some(frame) = self.pending.front() {
            if frame.read_at_ms >= oldest_ms {
                break;
            }
            let duration_ms = frame.duration_ms;
            self.pending.pop_front();
            self.pending_ms = self.pending_ms.saturating_sub(duration_ms);
        }
        self.pending_ms = self.pending_ms.max(0);
    }

    /// 送信が終わらないままになった記録を捨てる
    ///
    /// 送信の失敗や配信の停止で完了が返らない記録が残り続けると、送信のキューに残っている
    /// 長さが増え続ける。1 つの TIMESTAMP に複数の記録がありうるため、TIMESTAMP ごとに
    /// 古い方から捨てる。
    fn prune_pending_sends(&mut self, now_ms: i64) {
        let oldest_ms = now_ms.saturating_sub(AUDIO_PUBLISH_CATCH_UP_PENDING_TIMEOUT_MS);
        let mut empty_timestamps: alloc::vec::Vec<i64> = alloc::vec::Vec::new();
        for (&timestamp_us, sends) in self.pending_sends.iter_mut() {
            while let Some(send) = sends.front() {
                if send.queued_at_ms >= oldest_ms {
                    break;
                }
                let duration_ms = send.duration_ms;
                sends.pop_front();
                self.pending_send_frames = self.pending_send_frames.saturating_sub(1);
                self.pending_send_ms = self.pending_send_ms.saturating_sub(duration_ms).max(0);
            }
            if sends.is_empty() {
                empty_timestamps.push(timestamp_us);
            }
        }
        for timestamp_us in empty_timestamps {
            self.pending_sends.remove(&timestamp_us);
        }
        self.pending_send_ms = self.pending_send_ms.max(0);
    }
}

impl Default for AudioPublishCatchUp {
    fn default() -> Self {
        Self::new()
    }
}
