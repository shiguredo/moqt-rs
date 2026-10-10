//! Track Alias 未確立の Subgroup ストリームを短時間保持するバッファ
//!
//! draft-ietf-moq-transport-22 §3.1.3.1 (Unknown Track Alias):
//!
//! > When an endpoint receives a datagram or a new stream with a Track Alias that is not yet
//! > associated with an Established subscription, it MAY drop the data or buffer it briefly to
//! > handle reordering with the control message that establishes the Track Alias. For streams,
//! > the endpoint MAY withhold stream flow control beyond the stream header until the Track
//! > Alias has been established. To prevent deadlocks, endpoints MUST allocate connection flow
//! > control to control streams before allocating it to any data streams; otherwise a receiver
//! > might wait for a control message containing a Track Alias to release flow control, while
//! > the sender waits for flow control to send the message.
//!
//! この節の "buffer it briefly" を実装するためのバッファである。無制限に保持すると
//! メモリを圧迫するため、per-stream / per-session のバイト上限とタイムアウトを備える。
//! 上限を超えた entry のチャンクはその場で捨てる。保持したままにすると、破棄したはずの
//! バイトが session の集計に残り、無関係な stream を巻き添えで上限超過させる。
//!
//! このモジュールは sans I/O である。時刻は引数 (マイクロ秒の `i64`) で受け、
//! タイムアウトの判定は [`PendingSubgroupBuffer::take_ready`] と
//! [`PendingSubgroupBuffer::take_ready_for`] の中だけで行う。呼び出し側は I/O の
//! 待ち受けと同じ周期でどちらかを呼ぶこと。
//!
//! # entry のライフサイクル
//!
//! - entry は Subgroup ストリーム 1 本に対応する。同じ Track Alias に複数の entry が
//!   並び得る (alias ごとの一覧を持つ) ため、`add` の戻り値の識別子で区別する
//! - `add` は entry を alias の一覧へ追加し、タイムアウトの期限 (追加時刻 + `timeout_us`)
//!   を記録する
//! - `push` はチャンクを足してバイト数を集計する。上限に収まらないチャンクは足さず、
//!   entry ごと破棄する
//! - 購読が確立した alias は `note_subscriber` で、session の終了は `note_session_close` で、
//!   ストリームの終端は `note_end_of_stream` で通知する。通知した entry は
//!   `take_ready` または `take_ready_for` が引き取り待ちとして返す
//! - `take_ready` はバッファ全体で 1 つの entry を、`take_ready_for` は指定した識別子の
//!   entry だけを返す。1 つのバッファを複数の主体で共有する場合は、各主体が自分の entry の
//!   識別子を保持して `take_ready_for` を呼ぶこと
//! - どちらも引き取った entry のチャンクを所有権ごと呼び出し側へ移す (バイト数は移した分だけ
//!   集計から外れる)。entry 自体は削除せず、削除は `remove` に委ねる。同じ entry を 2 度
//!   返すことはない
//!
//! # タイムアウトと通知の順序
//!
//! `take_ready(now_us)` は、まず全 entry のうち期限を過ぎたものを `Timeout` として確定させ、
//! そのうえで引き取り可能な entry を追加順に 1 つ返す。`take_ready_for(id, now_us)` は、
//! 指定した entry だけを期限切れの確定の対象にし、その entry だけを返す。他の entry の状態を
//! 変えないのは、entry の所有者が別の主体であり得るためである (期限切れの確定は、所有者が
//! 自分の周期で `take_ready_for` を呼ぶことでも行われる)。通知は最初の 1 回だけ有効であり、
//! 後から別の理由で `note_*` を呼んでも理由は上書きしない (moqt-js の `notify` と同じ)。
//!
//! `Subscriber` と `Timeout` で引き取った entry は「破棄済み」とは扱わない。所有者がまだ
//! 保持しているチャンクを引き取る可能性があるため、削除は所有者の `remove` に委ねる。
//! 破棄済み (上限超過) の entry にだけは以後のチャンクを足さない。
//!
//! 根拠の節番号・規則はドラフト由来であり、将来の改訂で変わる可能性がある。

use alloc::vec::Vec;
use core::fmt;
use core::mem;
use hashbrown::HashMap;

/// [`PendingSubgroupBuffer`] の設定
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingSubgroupBufferOptions {
    /// per-stream のバイト上限
    ///
    /// この上限に収まらないチャンクを受けた entry は、それまで保持していたチャンクごと
    /// 破棄する ([`PendingNotifyReason::OverflowPerStream`])。
    pub per_stream_max_bytes: usize,
    /// per-session の合計バイト上限
    ///
    /// この上限に収まらないチャンクを受けた entry は、それまで保持していたチャンクごと
    /// 破棄する ([`PendingNotifyReason::OverflowPerSession`])。破棄するのは上限を超えさせた
    /// entry だけであり、関係のない entry は巻き込まない。
    pub per_session_max_bytes: usize,
    /// "buffer it briefly" の上限マイクロ秒
    ///
    /// 追加からこの時間が経過した entry は [`PendingNotifyReason::Timeout`] になる。
    /// 判定は [`PendingSubgroupBuffer::take_ready`] と
    /// [`PendingSubgroupBuffer::take_ready_for`] でだけ行う。
    pub timeout_us: i64,
}

/// [`PendingSubgroupBufferOptions`] の既定値
///
/// - `per_stream_max_bytes`: 1 MiB
/// - `per_session_max_bytes`: 16 MiB
/// - `timeout_us`: 5 秒 (draft-ietf-moq-transport-22 §3.1.3.1 "buffer it briefly")
pub const DEFAULT_PENDING_SUBGROUP_BUFFER_OPTIONS: PendingSubgroupBufferOptions =
    PendingSubgroupBufferOptions {
        per_stream_max_bytes: 1 << 20,
        per_session_max_bytes: 16 << 20,
        timeout_us: 5_000_000,
    };

/// entry を引き取る理由
///
/// [`PendingSubgroupBuffer::take_ready`] と [`PendingSubgroupBuffer::take_ready_for`] が
/// `Some` で返す値に含まれる。moqt-js の `PendingNotifyReason` と同じ意味を持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PendingNotifyReason {
    /// 購読が確立したので引き取れる
    ///
    /// 引き取ったチャンクを読み直すか、そのまま stream の続きを読むかを選ぶのは
    /// 呼び出し側である。
    Subscriber,
    /// 追加から `timeout_us` が経過した
    Timeout,
    /// per-stream のバイト上限を超えたので破棄した
    OverflowPerStream,
    /// per-session の合計バイト上限を超えたので破棄した
    OverflowPerSession,
    /// session が終了した
    SessionClose,
    /// ストリームが終端した (FIN / RESET_STREAM)
    EndOfStream,
}

/// [`PendingSubgroupBuffer::add`] が返す entry の識別子
///
/// 同じ Track Alias に複数の entry が並び得るため、entry は Track Alias ではなく
/// この識別子で指す。`add` のたびに新しい値を払い出すため、削除済みの識別子が
/// 別の entry を指すことはない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PendingSubgroupEntryId(u64);

/// [`PendingSubgroupBuffer::take_ready`] または
/// [`PendingSubgroupBuffer::take_ready_for`] が返す引き取り済み entry
///
/// チャンクの所有権は呼び出し側へ移っており、バッファの集計からは既に外れている。
/// entry 自体は `remove` を呼ぶまで残る。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSubgroupReady {
    /// 引き取った entry の識別子 (`remove` に渡す)
    pub id: PendingSubgroupEntryId,
    /// 引き取った entry の Track Alias
    pub track_alias: u64,
    /// 引き取った理由
    pub reason: PendingNotifyReason,
    /// 受信順のチャンク列
    ///
    /// [`PendingNotifyReason::OverflowPerStream`] と
    /// [`PendingNotifyReason::OverflowPerSession`] では破棄済みのため空になる。
    pub chunks: Vec<Vec<u8>>,
}

/// 保持中の entry
struct PendingEntry {
    /// SUBGROUP_HEADER の Track Alias
    track_alias: u64,
    /// タイムアウトの期限 (追加時刻 + `timeout_us`)
    deadline_us: i64,
    /// 受信順のチャンク列
    chunks: Vec<Vec<u8>>,
    /// `chunks` の合計バイト数
    total_bytes: usize,
    /// 確定した通知理由 (最初の 1 回だけ設定する)
    reason: Option<PendingNotifyReason>,
    /// 上限超過でチャンクを破棄済みか
    ///
    /// [`PendingNotifyReason::Subscriber`] / [`PendingNotifyReason::Timeout`] で通知済みでも
    /// 上限超過は起こり得るため、通知理由とは別に保持する。
    discarded: bool,
    /// チャンクを呼び出し側へ引き渡し済みか
    taken: bool,
}

impl PendingEntry {
    /// 上限超過でチャンクを破棄済みか
    fn is_discarded(&self) -> bool {
        self.discarded
    }

    /// 期限を過ぎていれば `Timeout` として確定させる (通知理由は最初の 1 回だけ確定させる)
    fn expire(&mut self, now_us: i64) {
        if self.reason.is_none() && now_us >= self.deadline_us {
            self.reason = Some(PendingNotifyReason::Timeout);
        }
    }
}

/// Track Alias 未確立の Subgroup ストリームを短時間保持するバッファ
///
/// 使い方はモジュールドキュメントの「entry のライフサイクル」を参照すること。
/// 1 つのバッファを複数の主体で共有する場合は、各主体が自分の entry の識別子を保持し、
/// [`PendingSubgroupBuffer::take_ready_for`] で自分の entry だけを引き取ること。
/// [`PendingSubgroupBuffer::take_ready`] はバッファ全体で 1 つの entry を返すため、
/// 1 つの待ち手だけが使う場合の API である。
pub struct PendingSubgroupBuffer {
    /// 識別子から entry を引く表
    entries: HashMap<PendingSubgroupEntryId, PendingEntry>,
    /// Track Alias から entry の識別子を引く表 (追加順)
    entries_by_alias: HashMap<u64, Vec<PendingSubgroupEntryId>>,
    /// 次に払い出す識別子
    next_id: u64,
    /// 保持しているチャンクの合計バイト数
    total_bytes: usize,
    /// 上限とタイムアウト
    options: PendingSubgroupBufferOptions,
}

impl PendingSubgroupBuffer {
    /// 設定を指定してバッファを作る
    pub fn new(options: PendingSubgroupBufferOptions) -> Self {
        Self {
            entries: HashMap::new(),
            entries_by_alias: HashMap::new(),
            next_id: 0,
            total_bytes: 0,
            options,
        }
    }

    /// 新しい entry を作り、タイムアウトを起動する
    ///
    /// `now_us` は呼び出し側の単調増加するマイクロ秒時刻であり、entry の期限は
    /// `now_us + timeout_us` になる (`i64` の範囲へ飽和させる)。期限の判定は
    /// [`PendingSubgroupBuffer::take_ready`] と [`PendingSubgroupBuffer::take_ready_for`] で
    /// だけ行う。
    ///
    /// 同じ Track Alias に対して何度でも呼べる。返る識別子は entry ごとに異なる。
    pub fn add(&mut self, track_alias: u64, now_us: i64) -> PendingSubgroupEntryId {
        let id = PendingSubgroupEntryId(self.next_id);
        // 識別子は単調に増やし、削除済みの識別子が別の entry を指さないようにする。
        // u64 を使い切ることは実質無い (1 entry あたり最低でも数十バイト必要であり、
        // 2^64 回の add は起きない) が、飽和して同じ値を再利用する場合でも集計が
        // ずれないよう、同じ識別子の entry が残っていれば先に削除する
        self.next_id = self.next_id.saturating_add(1);
        if self.entries.contains_key(&id) {
            self.remove(id);
        }
        let entry = PendingEntry {
            track_alias,
            deadline_us: now_us.saturating_add(self.options.timeout_us),
            chunks: Vec::new(),
            total_bytes: 0,
            reason: None,
            discarded: false,
            taken: false,
        };
        self.entries.insert(id, entry);
        self.entries_by_alias
            .entry(track_alias)
            .or_default()
            .push(id);
        id
    }

    /// チャンクを entry へ積み、バイト数を集計する
    ///
    /// 次の場合は何もしない。
    ///
    /// - 識別子が未登録、または [`PendingSubgroupBuffer::remove`] 済み
    /// - entry が上限超過で破棄済み (破棄したバイトを集計へ戻さないため)
    /// - entry のチャンクを [`PendingSubgroupBuffer::take_ready`] または
    ///   [`PendingSubgroupBuffer::take_ready_for`] で引き渡し済み
    ///
    /// 積んだ結果が per-stream の上限を超える場合は、その entry を破棄する
    /// ([`PendingNotifyReason::OverflowPerStream`])。per-session の合計上限を超える場合も、
    /// 上限を超えさせた entry だけを破棄する ([`PendingNotifyReason::OverflowPerSession`])。
    /// どちらの場合もチャンクは entry へ積まず、それまで保持していたチャンクも捨てて集計から
    /// 外す。積んでから判定すると、破棄したはずのバイトが `total_bytes` に残り、合計が
    /// `per_session_max_bytes` を超えるためである。
    pub fn push(&mut self, id: PendingSubgroupEntryId, chunk: &[u8]) {
        // 未登録・削除済みの識別子への push は no-op。引き渡し済み (taken) と
        // 破棄済み (discarded) の entry にも足さない (足すと集計から外したはずの
        // バイトが積み上がり、無関係な stream を巻き込んで上限超過させる)
        let (entry_total, ignore) = match self.entries.get(&id) {
            Some(entry) => (entry.total_bytes, entry.taken || entry.is_discarded()),
            None => return,
        };
        if ignore {
            return;
        }
        let chunk_len = chunk.len();
        if entry_total.saturating_add(chunk_len) > self.options.per_stream_max_bytes {
            self.discard(id, PendingNotifyReason::OverflowPerStream);
            return;
        }
        if self.total_bytes.saturating_add(chunk_len) > self.options.per_session_max_bytes {
            self.discard(id, PendingNotifyReason::OverflowPerSession);
            return;
        }
        let entry = self
            .entries
            .get_mut(&id)
            .expect("entry presence already checked");
        entry.chunks.push(chunk.to_vec());
        entry.total_bytes += chunk_len;
        self.total_bytes += chunk_len;
    }

    /// チャンクを捨てて集計から外し、entry を通知済みにする
    ///
    /// entry 自体は残す。所有者が [`PendingSubgroupBuffer::take_ready`] または
    /// [`PendingSubgroupBuffer::take_ready_for`] で理由を受け取り、
    /// [`PendingSubgroupBuffer::remove`] で削除するまで保持する。
    fn discard(&mut self, id: PendingSubgroupEntryId, reason: PendingNotifyReason) {
        let Some(entry) = self.entries.get_mut(&id) else {
            return;
        };
        let released = entry.total_bytes;
        entry.chunks.clear();
        entry.total_bytes = 0;
        entry.discarded = true;
        self.total_bytes = self.total_bytes.saturating_sub(released);
        // 通知理由は最初の 1 回だけ確定させる
        if entry.reason.is_none() {
            entry.reason = Some(reason);
        }
    }

    /// 該当 Track Alias の全 entry に購読の確立を通知する
    ///
    /// 通知された entry は [`PendingSubgroupBuffer::take_ready`] または
    /// [`PendingSubgroupBuffer::take_ready_for`] で引き取れるようになる。
    /// バッファからの削除は entry の所有者が [`PendingSubgroupBuffer::remove`] で行う。
    pub fn note_subscriber(&mut self, track_alias: u64) {
        self.notify_alias(track_alias, PendingNotifyReason::Subscriber);
    }

    /// 該当 Track Alias の全 entry にストリームの終端を通知する
    ///
    /// FIN や RESET_STREAM でストリームが終わったことを知らせる。保持しているチャンクは
    /// 捨てない (所有者が読み直せるように残す)。
    pub fn note_end_of_stream(&mut self, track_alias: u64) {
        self.notify_alias(track_alias, PendingNotifyReason::EndOfStream);
    }

    /// 全 entry に session の終了を通知する
    ///
    /// 保持しているチャンクは捨てない。session を閉じたあとは、チャンクを使わないので
    /// あれば [`PendingSubgroupBuffer::reset`] でまとめて捨ててよい。
    pub fn note_session_close(&mut self) {
        self.notify_alias_all(PendingNotifyReason::SessionClose);
    }

    /// 該当 Track Alias の全 entry に通知する
    fn notify_alias(&mut self, track_alias: u64, reason: PendingNotifyReason) {
        let Some(ids) = self.entries_by_alias.get(&track_alias) else {
            return;
        };
        for id in ids.clone() {
            if let Some(entry) = self.entries.get_mut(&id)
                && entry.reason.is_none()
            {
                entry.reason = Some(reason);
            }
        }
    }

    /// 全 entry に通知する
    fn notify_alias_all(&mut self, reason: PendingNotifyReason) {
        for entry in self.entries.values_mut() {
            if entry.reason.is_none() {
                entry.reason = Some(reason);
            }
        }
    }

    /// 全 entry の期限を判定し、過ぎたものを `Timeout` として確定させる
    fn expire(&mut self, now_us: i64) {
        for entry in self.entries.values_mut() {
            entry.expire(now_us);
        }
    }

    /// 通知済みの entry を 1 つ引き取り、チャンクを呼び出し側へ移す
    ///
    /// 未登録・未通知・引き取り済みの entry に対しては `None` を返す。
    fn take_entry(&mut self, id: PendingSubgroupEntryId) -> Option<PendingSubgroupReady> {
        let entry = self.entries.get_mut(&id)?;
        if entry.taken {
            return None;
        }
        let reason = entry.reason?;
        entry.taken = true;
        let chunks = mem::take(&mut entry.chunks);
        let moved = entry.total_bytes;
        entry.total_bytes = 0;
        self.total_bytes = self.total_bytes.saturating_sub(moved);
        Some(PendingSubgroupReady {
            id,
            track_alias: entry.track_alias,
            reason,
            chunks,
        })
    }

    /// 引き取り可能な entry を 1 つ返す
    ///
    /// まず全 entry のうち期限を過ぎたものを `Timeout` として確定させ、そのうえで通知済みかつ
    /// 未引き取りの entry を追加順に 1 つ返す。返した entry のチャンクは所有権ごと呼び出し側へ
    /// 移り、バイト数は集計から外れる。entry 自体は [`PendingSubgroupBuffer::remove`] を呼ぶ
    /// まで残り、同じ entry が 2 度返ることはない。
    ///
    /// 呼び出し側は I/O の待ち受けと同じ周期でこの関数を呼ぶこと。呼ばないと期限切れの
    /// entry のチャンクが集計に残り続ける。
    ///
    /// 1 つのバッファを複数の主体で共有する場合は、他の主体の entry を引き取ってしまうため
    /// この関数ではなく [`PendingSubgroupBuffer::take_ready_for`] を使うこと。
    ///
    /// `now_us` は単調増加するマイクロ秒時刻を前提とする。
    pub fn take_ready(&mut self, now_us: i64) -> Option<PendingSubgroupReady> {
        self.expire(now_us);
        // 引き取り可能な entry のうち、識別子が最も小さいもの (= 追加が最も早いもの) を返す。
        // hashbrown の反復順は不定であるため、最小値で順序を決める。
        let id = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.reason.is_some() && !entry.taken)
            .map(|(id, _)| *id)
            .min()?;
        self.take_entry(id)
    }

    /// 指定した entry が引き取り可能なら引き取る
    ///
    /// [`PendingSubgroupBuffer::take_ready`] がバッファ全体で 1 つの entry を返すのに対し、
    /// こちらは `id` の entry だけを返す。1 つのバッファを複数の主体で共有する場合は、各主体が
    /// 自分の entry の識別子を保持してこれを呼ぶことで、他の主体の entry を引き取らずに済む。
    ///
    /// 期限切れの確定も `id` の entry だけを対象にする。他の entry は別の主体が所有し得るため、
    /// 状態を勝手に変えない方が安全である。代わりに、それぞれの主体が自分の周期でこの関数を
    /// 呼ばないと自分の entry は期限切れにならない。
    ///
    /// 上限超過でチャンクを破棄した entry は、[`PendingSubgroupBuffer::take_ready`] と同じく
    /// `Some` で理由 (`OverflowPerStream` / `OverflowPerSession`) を返す (チャンクは空)。
    /// ここで `None` を返すと、所有者が上限超過を観測して `remove` する手段を失うためである。
    ///
    /// 次の場合は `None` を返す。
    ///
    /// - 識別子が未登録、または [`PendingSubgroupBuffer::remove`] で削除済み
    /// - entry がまだ通知されていない
    /// - entry を [`PendingSubgroupBuffer::take_ready`] または本関数で引き取り済み
    ///
    /// `now_us` は単調増加するマイクロ秒時刻を前提とする。
    pub fn take_ready_for(
        &mut self,
        id: PendingSubgroupEntryId,
        now_us: i64,
    ) -> Option<PendingSubgroupReady> {
        match self.entries.get_mut(&id) {
            Some(entry) => entry.expire(now_us),
            None => return None,
        }
        self.take_entry(id)
    }

    /// entry をバッファから削除する
    ///
    /// 保持していたチャンクのバイト数を集計から外す ([`PendingSubgroupBuffer::take_ready`] または
    /// [`PendingSubgroupBuffer::take_ready_for`] で引き取り済みなら 0 である)。未登録または
    /// 削除済みの識別子に対する呼び出しは no-op。
    ///
    /// 所有者は [`PendingSubgroupBuffer::take_ready`] または
    /// [`PendingSubgroupBuffer::take_ready_for`] で引き取ったあと、必ずこれを呼ぶこと。
    pub fn remove(&mut self, id: PendingSubgroupEntryId) {
        let Some(entry) = self.entries.remove(&id) else {
            return;
        };
        self.total_bytes = self.total_bytes.saturating_sub(entry.total_bytes);
        // alias の一覧からも外す。空になった一覧は表から消す
        let empty = {
            let Some(ids) = self.entries_by_alias.get_mut(&entry.track_alias) else {
                return;
            };
            if let Some(position) = ids.iter().position(|candidate| *candidate == id) {
                ids.remove(position);
            }
            ids.is_empty()
        };
        if empty {
            self.entries_by_alias.remove(&entry.track_alias);
        }
    }

    /// 保持している entry の数 (全 Track Alias の合計)
    ///
    /// 上限超過でチャンクを破棄した entry も、所有者が [`PendingSubgroupBuffer::remove`] を
    /// 呼ぶまでは数える。
    pub fn stream_count(&self) -> usize {
        self.entries.len()
    }

    /// 保持しているチャンクの合計バイト数
    ///
    /// 上限超過で破棄したチャンクと、[`PendingSubgroupBuffer::take_ready`] または
    /// [`PendingSubgroupBuffer::take_ready_for`] で引き渡したチャンクは含まない。
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// 全 entry を削除して集計を 0 に戻す
    ///
    /// session を閉じたあとの後始末に使う。識別子は払い出し済みの値を再利用しないため、
    /// reset の前に得た識別子が reset 後の entry を指すことはない。
    pub fn reset(&mut self) {
        self.entries.clear();
        self.entries_by_alias.clear();
        self.total_bytes = 0;
    }
}

impl fmt::Debug for PendingSubgroupBuffer {
    /// チャンクの中身は出さず、entry ごとのバイト数と状態だけを出す
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct Entries<'a>(&'a HashMap<PendingSubgroupEntryId, PendingEntry>);

        impl fmt::Debug for Entries<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let mut list = f.debug_list();
                for (id, entry) in self.0 {
                    list.entry(&Entry(id, entry));
                }
                list.finish()
            }
        }

        struct Entry<'a>(&'a PendingSubgroupEntryId, &'a PendingEntry);

        impl fmt::Debug for Entry<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct("PendingEntry")
                    .field("id", self.0)
                    .field("track_alias", &self.1.track_alias)
                    .field("total_bytes", &self.1.total_bytes)
                    .field("chunks", &self.1.chunks.len())
                    .field("reason", &self.1.reason)
                    .field("discarded", &self.1.discarded)
                    .field("taken", &self.1.taken)
                    .finish()
            }
        }

        f.debug_struct("PendingSubgroupBuffer")
            .field("options", &self.options)
            .field("stream_count", &self.stream_count())
            .field("total_bytes", &self.total_bytes)
            .field("entries", &Entries(&self.entries))
            .finish()
    }
}
