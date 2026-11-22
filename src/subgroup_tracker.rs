//! Subgroup 再オープン禁止の追跡 (draft-ietf-moq-transport-23 §2.2 (Subgroups) / draft-ietf-moq-transport-23 §11.3.2 (Closing Subgroup Streams))
//!
//! Subgroup ストリームは一度終端したら同一 `(track_alias, group_id, subgroup_id)` を
//! 別ストリームで再オープンしてはならない。ただし draft-ietf-moq-transport-23 Appendix A.6 (Since draft-ietf-moq-transport-17) #1583 により、
//! STOP_SENDING で停止されたサブグループは、REQUEST_UPDATE で購読が再開された
//! (PAUSED が 1→0 に変わった) 場合に再オープン可能。本モジュールは session 内でのストリーム
//! ライフサイクルを追跡し、違反時に `PROTOCOL_VIOLATION` として通知するユーティリティ
//! を提供する。
//!
//! # 根拠資料
//!
//! - draft-ietf-moq-transport-23 §2.2 (Subgroups) `from the same Subgroup MUST NOT be sent on different
//!   streams, unless one of the streams was reset prematurely, or upstream conditions
//!   have forced objects from a Subgroup to be sent out of Object ID order.`
//! - draft-ietf-moq-transport-23 §11.3.2 (Closing Subgroup Streams) `A publisher that receives a STOP_SENDING on a
//!   Subgroup stream SHOULD NOT attempt to open a new stream to deliver additional
//!   Objects in that Subgroup.`
//! - draft-ietf-moq-transport-23 §11.3.2 (Closing Subgroup Streams): STOP_SENDING 単独では publisher は新 stream を開くべきではない。
//!   REQUEST_UPDATE で購読が再開された (PAUSED が 1→0) 場合のみ再オープン MAY
//!   (draft-ietf-moq-transport-23 Appendix A.6 (Since draft-ietf-moq-transport-17) #1583)
//!
//! # 注意
//!
//! 本仕様は draft 由来であり、将来の改訂で例外条件 (prematurely reset, out-of-order
//! upstream) の具体的な許容範囲が変更される可能性がある。

use alloc::vec::Vec;
use hashbrown::HashMap;

use crate::error::MessageError;

/// Subgroup ストリームの状態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubgroupStreamState {
    /// SUBGROUP_HEADER を受信してストリームがオープン中
    Open,
    /// FIN で正常終端した
    ///
    /// FIN は最優先の終端理由であり、この状態は後から RESET / STOP_SENDING を記録しても
    /// 変わらない (draft-ietf-moq-transport-23 §11.3.2 (Closing Subgroup Streams): "If the
    /// application receives all data on the stream and the FIN, it can ignore any subsequent
    /// reset.")。FIN 前に受信した最終 Object ID は [`SubgroupTracker`] が状態とは別に保持し、
    /// §12.1 (Malformed Tracks) 条件 2/3 の検出に使う。
    ClosedFin,
    /// RESET_STREAM / RESET_STREAM_AT で終端した
    ///
    /// draft-ietf-moq-transport-23 §11.3.2 (Closing Subgroup Streams): RESET_STREAM_AT では
    /// `reliable_size` が指定される。古い RESET_STREAM では `None`。
    Reset {
        /// RESET_STREAM_AT の reliable サイズ (古い RESET_STREAM では `None`)
        ///
        /// 受信側の wire の値であり、WebTransport over HTTP/3 ではストリームヘッダーを
        /// 含む。送信側の `SessionEvent::ResetDataStream` は MOQT のバイト数で表す。
        reliable_size: Option<u64>,
    },
    /// Subscriber 側から STOP_SENDING を受けて終端した
    ///
    /// draft-ietf-moq-transport-23 §11.3.2 (Closing Subgroup Streams) /
    /// Appendix A.6 (Since draft-ietf-moq-transport-17) #1583:
    /// REQUEST_UPDATE で購読が再開された (PAUSED が 1→0) 場合に再オープン可能
    StoppedByPeer,
}

impl SubgroupStreamState {
    /// 再オープン可能かどうか
    ///
    /// この判定は終端状態の種別のみを見る。STOP_SENDING 起因の再オープン禁止は
    /// `Session` が request 単位で別途保持し、購読を再開する REQUEST_UPDATE
    /// (PAUSED が 1→0 に変わる) が受理されるまで送信 API
    /// (`send_subgroup_header` / `send_subgroup_object`) が拒否する。
    ///
    /// - draft-ietf-moq-transport-23 §2.2 (Subgroups): premature reset からの再オープン
    /// - draft-ietf-moq-transport-23 §11.3.2 (Closing Subgroup Streams): "A publisher that
    ///   receives a STOP_SENDING on a Subgroup stream SHOULD NOT attempt to open a new stream
    ///   to deliver additional Objects in that Subgroup. However, if the publisher subsequently
    ///   receives a REQUEST_UPDATE that resumes the subscription, it MAY open a
    ///   new stream ..." STOP_SENDING 後の再オープンは購読の再開 (PAUSED 1→0) の場合のみ
    /// - draft-ietf-moq-transport-23 §5.2 (Delivery Timeouts and Data Reliability):
    ///   DELIVERY_TIMEOUT 起因の reset 後の再送は SHOULD NOT。本判定は終端種別のみを見るため
    ///   この条件は区別せず、reason (`DataStreamResetReason::DeliveryTimeout`) に応じて
    ///   再送を控えるのはアプリの責務とする
    ///
    /// §2.2: "Objects from the same Subgroup MUST NOT be sent on different streams,
    /// unless one of the streams was reset prematurely"
    /// §11.3.2 (Closing Subgroup Streams): relay は next Object か不明な場合 "it MUST reset
    /// the Subgroup stream and open a new one to forward it"
    ///
    /// 本判定では StoppedByPeer (STOP_SENDING) と Reset (STOP_SENDING 後の reset や
    /// 送信側 reset) の両方で再オープンを許可し、PAUSED による再開条件の判定は Session に
    /// 委ねる (STOP_SENDING を受けていない reset は PAUSED 条件なしで再オープンできる)。
    /// ClosedFin (正常終了) からの再オープンは引き続き禁止する。
    pub fn can_reopen(&self) -> bool {
        matches!(self, Self::StoppedByPeer | Self::Reset { .. })
    }
}

/// `(track_alias, group_id, subgroup_id)` の 3 点組をキーとする
type SubgroupKey = (u64, u64, u64);

/// Track Alias ごとに保持する Subgroup 追跡キー数の上限
///
/// 長寿命の subscription では Group が進むたびに `(track_alias, group_id, subgroup_id)` の
/// キーが増える。追跡キー数がこの上限を超えたら、当該 Track Alias の終端済みキー
/// (`SubgroupStreamState::Open` 以外のキーと、状態エントリの無い優先度 / FIN 記録) を
/// まとめて破棄する。`Open` のキーは破棄しない。
///
/// 1 キーあたりの保持量はキー 3 つの `u64` と enum / `Option<u64>` であり、10 万件でも
/// 数十 MB に収まる。30 fps で 1 Group あたり 1 Subgroup なら約 1 時間分の検出窓になる。
/// 値は既存の datagram 追跡エントリ上限
/// (`MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION`) と同じ 100,000 とする。
///
/// 破棄されたキーは未知キーとして扱われるため、破棄後の再オープンは成功し、
/// `check_object_after_fin` / `record_priority` / `mark_fin` の条件 1〜3 の検出は
/// 行われない (検出漏れに倒す known limitation)。
pub const MAX_TRACKED_SUBGROUP_KEYS_PER_TRACK_ALIAS: usize = 100_000;

/// Subgroup ストリームのライフサイクル追跡
///
/// draft-ietf-moq-transport-23 §2.2 (Subgroups) / draft-ietf-moq-transport-23 §11.3.2 (Closing Subgroup Streams) に基づき、同一キーの再オープン / 並行 2 本ストリームを拒否する。
///
/// また draft-ietf-moq-transport-23 §12.1 (Malformed Tracks) の条件 1 (Publisher Priority
/// 不一致) / 条件 2 (FIN 済み Subgroup への最終 Object 超過) / 条件 3 (複数 FIN の最終
/// Object 不一致) の検出に必要な状態を保持する。
///
/// # 追跡キーの上限
///
/// `track_alias` ごとの追跡キー数が [`MAX_TRACKED_SUBGROUP_KEYS_PER_TRACK_ALIAS`] を
/// 超えると、当該 alias の終端済みキーを 3 つの表からまとめて破棄する。`Open` のキーは
/// 破棄しない。破棄されたキーは未知キーとして扱われるため、再オープンは成功し、
/// 条件 1〜3 の検出はそのキーについては行われない (known limitation)。上限は
/// `track_alias` ごとであり、`remove_track_alias` で件数も削除する。
#[derive(Debug, Default, Clone)]
pub struct SubgroupTracker {
    entries: HashMap<SubgroupKey, SubgroupStreamState>,
    /// Subgroup キーごとに初回オープン時の Publisher Priority を保持する
    /// (draft-ietf-moq-transport-23 §12.1 (Malformed Tracks) 条件 1 の検出用)
    priorities: HashMap<SubgroupKey, u8>,
    /// FIN 済み Subgroup の最終 Object ID を保持する (条件 2/3 の検出用)
    ///
    /// 最終 Object ID の唯一の記録元である (`SubgroupStreamState::ClosedFin` は値を持たない)。
    /// 状態エントリの無いキーの FIN もここに記録する。Object を 1 つも受信せずに FIN した
    /// 場合は `None` を記録する。
    ///
    /// 追跡キー数が [`MAX_TRACKED_SUBGROUP_KEYS_PER_TRACK_ALIAS`] を超えると破棄される
    /// ことがある。破棄されたキーの FIN 記録は失われ、条件 2/3 の検出は行われない
    /// (known limitation)。
    fin_last_object_ids: HashMap<SubgroupKey, Option<u64>>,
    /// Track Alias ごとの追跡キー数
    ///
    /// `entries` / `priorities` / `fin_last_object_ids` のキーの和集合の件数である。
    /// 同じキーが複数の表にあっても 1 と数える。上限判定を全走査せずに行うために保持し、
    /// キーが和集合に新規に加わったときだけ増やし、破棄したときに減らす。
    counts: HashMap<u64, usize>,
}

impl SubgroupTracker {
    /// 空のトラッカーを作成する
    pub fn new() -> Self {
        Self::default()
    }

    /// キーが 3 つの表のいずれかに存在するか
    fn known_key(&self, key: &SubgroupKey) -> bool {
        self.entries.contains_key(key)
            || self.priorities.contains_key(key)
            || self.fin_last_object_ids.contains_key(key)
    }

    /// キーが和集合に新規に加わるときだけ、Track Alias の追跡キー数を増やす
    ///
    /// 呼び出し側はこの直後にキーをいずれかの表へ挿入する。
    fn note_inserted_key(&mut self, key: SubgroupKey) {
        if !self.known_key(&key) {
            *self.counts.entry(key.0).or_insert(0) += 1;
        }
    }

    /// 追跡キー数が上限を超えていれば、終端済みキーをまとめて破棄する
    fn enforce_limit(&mut self, track_alias: u64) {
        self.enforce_limit_with(track_alias, MAX_TRACKED_SUBGROUP_KEYS_PER_TRACK_ALIAS);
    }

    /// 上限を引数で受け取る [`Self::enforce_limit`]
    ///
    /// 単体テストで小さい上限を使って破棄の規則を確認するために分けている。公開 API は
    /// 常に [`MAX_TRACKED_SUBGROUP_KEYS_PER_TRACK_ALIAS`] を使う。
    fn enforce_limit_with(&mut self, track_alias: u64, limit: usize) {
        let count = self.counts.get(&track_alias).copied().unwrap_or(0);
        if count <= limit {
            return;
        }
        self.prune_terminal(track_alias);
    }

    /// Track Alias の終端済みキーを 3 つの表からまとめて破棄する
    ///
    /// 終端済みキーとは、`entries` の値が `Open` 以外のキーと、`entries` に存在しない
    /// `priorities` / `fin_last_object_ids` のキーである。`Open` のキーは破棄しない。
    /// 和集合の件数カウンタは、破棄したキー数だけ減らす。
    fn prune_terminal(&mut self, track_alias: u64) {
        let mut terminal: Vec<SubgroupKey> = Vec::new();
        for (key, state) in &self.entries {
            if key.0 == track_alias && !matches!(state, SubgroupStreamState::Open) {
                terminal.push(*key);
            }
        }
        for key in self.priorities.keys() {
            if key.0 == track_alias && !self.entries.contains_key(key) {
                terminal.push(*key);
            }
        }
        for key in self.fin_last_object_ids.keys() {
            if key.0 == track_alias
                && !self.entries.contains_key(key)
                && !self.priorities.contains_key(key)
            {
                terminal.push(*key);
            }
        }
        for key in &terminal {
            self.entries.remove(key);
            self.priorities.remove(key);
            self.fin_last_object_ids.remove(key);
        }
        if !terminal.is_empty() {
            let removed = terminal.len();
            let remaining = self.counts.get(&track_alias).copied().unwrap_or(0);
            debug_assert!(remaining >= removed);
            let remaining = remaining - removed;
            if remaining == 0 {
                self.counts.remove(&track_alias);
            } else {
                self.counts.insert(track_alias, remaining);
            }
        }
    }

    /// 新しい Subgroup ストリームのオープンを記録する
    ///
    /// すでに同一キーのエントリが Open 中の場合は `PROTOCOL_VIOLATION` を返す。
    /// 終端済みの場合は draft-ietf-moq-transport-23 Appendix A.6 (Since draft-ietf-moq-transport-17) #1583
    /// (REQUEST_UPDATE で購読再開 (PAUSED 1→0)) に基づき、`can_reopen()` が true なら
    /// 再オープンを許可する。それ以外の終端状態は `PROTOCOL_VIOLATION` を返す。
    ///
    /// 上限超過で破棄されたキーは未知キーとして扱うため、以前の終端状態にかかわらず
    /// 再オープンは成功する (known limitation)。
    pub fn open(
        &mut self,
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
    ) -> Result<(), MessageError> {
        let key = (track_alias, group_id, subgroup_id);
        match self.entries.get(&key) {
            Some(state) if state.can_reopen() => {
                // 既存キーの状態だけを変えるため、追跡キー数は変わらない
                self.entries.insert(key, SubgroupStreamState::Open);
                self.enforce_limit(track_alias);
                Ok(())
            }
            Some(SubgroupStreamState::Open) => Err(MessageError::ProtocolViolation(
                "concurrent Subgroup stream open for the same (track_alias, group_id, subgroup_id)",
            )),
            Some(_) => Err(MessageError::ProtocolViolation(
                "reopening a terminated Subgroup stream is prohibited (draft-ietf-moq-transport-23 §2.2 (Subgroups) / draft-ietf-moq-transport-23 §11.3.2 (Closing Subgroup Streams))",
            )),
            None => {
                // 状態エントリの無いキーでも、優先度 / FIN 記録があれば和集合には既にある
                self.note_inserted_key(key);
                self.entries.insert(key, SubgroupStreamState::Open);
                self.enforce_limit(track_alias);
                Ok(())
            }
        }
    }

    /// FIN での正常終端を記録する
    ///
    /// `last_object_id` は FIN 前に受信した最終 Object の ID。Object を 1 つも受信せずに
    /// FIN した場合は `None`。draft-ietf-moq-transport-23 §12.1 (Malformed Tracks) 条件 2/3
    /// の検出に使う。
    ///
    /// 状態エントリがあるキーは `ClosedFin` にする。状態エントリの無いキーには状態エントリを
    /// 作らないが、最終 Object ID は記録する (条件 2/3 の検出に必要なため)。
    ///
    /// 条件 3: 同一 Subgroup キーが 2 回目の FIN を迎えたとき、記録済みの最終 Object ID と
    /// 異なれば Malformed Track に該当する。この場合は状態エントリがあるかどうかにかかわらず
    /// `Err` を返し、状態も最終 Object ID の記録も更新しない。
    ///
    /// 記録後に追跡キー数が上限を超えた場合は、このキーを含む終端済みキーが破棄される
    /// ことがある。破棄されたキーの FIN 記録は失われ、条件 3 の比較は行われない
    /// (known limitation)。
    pub fn mark_fin(
        &mut self,
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
        last_object_id: Option<u64>,
    ) -> Result<(), MessageError> {
        let key = (track_alias, group_id, subgroup_id);
        // 条件 3: 同一 Subgroup が複数 stream で FIN され最終 Object が異なる
        if let Some(&prev) = self.fin_last_object_ids.get(&key)
            && prev != last_object_id
        {
            return Err(MessageError::MalformedTrack(
                "same Subgroup FINed with different final Object IDs",
            ));
        }
        // FIN 記録が初めてのキーだけ、追跡キー数を増やす
        self.note_inserted_key(key);
        self.fin_last_object_ids.insert(key, last_object_id);
        if let Some(state) = self.entries.get_mut(&key) {
            *state = SubgroupStreamState::ClosedFin;
        }
        self.enforce_limit(track_alias);
        Ok(())
    }

    /// RESET_STREAM / RESET_STREAM_AT での終端を記録する
    ///
    /// 状態エントリの無いキーと、`ClosedFin` のキーに対しては何もしない。FIN を受けた後の
    /// reset は無視でき (draft-ietf-moq-transport-23 §11.3.2 (Closing Subgroup Streams))、
    /// 正常終了の事実を上書きすると FIN 後の再オープンを許してしまう (§2.2 (Subgroups))。
    /// `Reset` / `StoppedByPeer` からは後勝ちで `Reset` にする。
    ///
    /// 上限超過で破棄済みのキーに対しては、状態エントリが無いため no-op になる
    /// (known limitation)。
    pub fn mark_reset(
        &mut self,
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
        reliable_size: Option<u64>,
    ) {
        let key = (track_alias, group_id, subgroup_id);
        if let Some(state) = self.entries.get_mut(&key)
            && *state != SubgroupStreamState::ClosedFin
        {
            *state = SubgroupStreamState::Reset { reliable_size };
            // Open から終端状態へ変わったことで破棄できるキーが増える
            self.enforce_limit(track_alias);
        }
    }

    /// STOP_SENDING 受信による終端を記録する
    ///
    /// 状態エントリの無いキーと、`ClosedFin` のキーに対しては何もしない (FIN を最優先の
    /// 終端理由として扱い、正常終了の事実を上書きしない)。`Reset` / `StoppedByPeer` からは
    /// 後勝ちで `StoppedByPeer` にする。
    ///
    /// 上限超過で破棄済みのキーに対しては、状態エントリが無いため no-op になる
    /// (known limitation)。
    pub fn mark_stop_sending(&mut self, track_alias: u64, group_id: u64, subgroup_id: u64) {
        let key = (track_alias, group_id, subgroup_id);
        if let Some(state) = self.entries.get_mut(&key)
            && *state != SubgroupStreamState::ClosedFin
        {
            *state = SubgroupStreamState::StoppedByPeer;
            // Open から終端状態へ変わったことで破棄できるキーが増える
            self.enforce_limit(track_alias);
        }
    }

    /// キーに対応する現在の状態を返す
    ///
    /// 上限超過で破棄されたキーは `None` を返す。破棄後は状態を失うため、以前の終端状態
    /// (再オープン禁止など) は観測できない (known limitation)。
    pub fn get(
        &self,
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
    ) -> Option<&SubgroupStreamState> {
        self.entries.get(&(track_alias, group_id, subgroup_id))
    }

    /// 指定 Track Alias に紐づくすべての Subgroup 状態を削除する
    ///
    /// Track Alias は session 内で同時利用だけが禁止されており、subscription を完全に
    /// 回収した後は再利用されうる。alias 再利用時に古い subgroup 履歴が干渉しないよう、
    /// `forget_subscription` などの cleanup から呼び出す。
    ///
    /// 3 つの表のキーと、alias ごとの追跡キー数カウンタを削除する。
    pub fn remove_track_alias(&mut self, track_alias: u64) {
        self.entries
            .retain(|(alias, _, _), _| *alias != track_alias);
        self.priorities
            .retain(|(alias, _, _), _| *alias != track_alias);
        self.fin_last_object_ids
            .retain(|(alias, _, _), _| *alias != track_alias);
        // alias 再利用時に以前の件数が残らないようカウンタも削除する
        self.counts.remove(&track_alias);
    }

    /// Subgroup の Publisher Priority を記録し、既存記録と不一致なら条件 1 として検出する
    ///
    /// draft-ietf-moq-transport-23 §12.1 (Malformed Tracks) 条件 1:
    /// "An Object with a particular Subgroup ID is received, but its Publisher Priority is
    /// different from that of the previous Object with the same Subgroup ID."
    ///
    /// 初回記録時は Ok。2 回目以降で値が異なれば Err を返す。
    ///
    /// 上限超過で破棄されたキーは、破棄後の最初の記録を初回として扱う。そのため破棄前の
    /// 優先度との比較は行われない (known limitation)。
    pub fn record_priority(
        &mut self,
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
        publisher_priority: u8,
    ) -> Result<(), MessageError> {
        let key = (track_alias, group_id, subgroup_id);
        match self.priorities.get(&key) {
            Some(&prev) if prev != publisher_priority => Err(MessageError::MalformedTrack(
                "Publisher Priority differs from previous Object with same Subgroup ID",
            )),
            Some(_) => Ok(()),
            None => {
                // 優先度だけを保持するキーは終端済みキーであり、新規なら和集合に加わる
                self.note_inserted_key(key);
                self.priorities.insert(key, publisher_priority);
                self.enforce_limit(track_alias);
                Ok(())
            }
        }
    }

    /// FIN 済み Subgroup に対して最終 Object ID より大きい Object ID が到着したか検査する
    ///
    /// draft-ietf-moq-transport-23 §12.1 (Malformed Tracks) 条件 2:
    /// "An Object is received whose Object ID is larger than the final Object in the Subgroup.
    /// The final Object in a Subgroup is the last Object received on a Subgroup stream before
    /// a FIN."
    ///
    /// 違反なら理由文字列を返す。FIN 記録がない・最終 Object ID が不明・ Object ID が
    /// 最終以下の場合は `None` を返す。
    ///
    /// 上限超過で破棄されたキーの FIN 記録は失われるため、そのキーについては条件 2 の
    /// 検出を行わない (known limitation)。
    pub fn check_object_after_fin(
        &self,
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
        object_id: u64,
    ) -> Option<&'static str> {
        let key = (track_alias, group_id, subgroup_id);
        if let Some(&Some(fin_last)) = self.fin_last_object_ids.get(&key)
            && object_id > fin_last
        {
            return Some("malformed track: Object ID larger than final Object in FINed Subgroup");
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;

    /// 追跡キーの和集合の件数を、3 つの表を走査して求める
    fn union_count(tracker: &SubgroupTracker, track_alias: u64) -> usize {
        let mut keys = BTreeSet::new();
        for key in tracker.entries.keys() {
            if key.0 == track_alias {
                keys.insert(*key);
            }
        }
        for key in tracker.priorities.keys() {
            if key.0 == track_alias {
                keys.insert(*key);
            }
        }
        for key in tracker.fin_last_object_ids.keys() {
            if key.0 == track_alias {
                keys.insert(*key);
            }
        }
        keys.len()
    }

    /// カウンタが 3 つの表のキーの和集合の件数と一致すること
    ///
    /// 同じキーが entries / priorities / fin_last_object_ids の複数に現れても 1 と数える。
    #[test]
    fn counter_matches_union_of_three_maps() {
        let mut tracker = SubgroupTracker::new();

        // open だけのキー
        tracker
            .open(1, 0, 0)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(tracker.counts.get(&1).copied(), Some(1));
        assert_eq!(union_count(&tracker, 1), 1);

        // open → FIN → 優先度記録。同じキーなので件数は増えない
        tracker
            .mark_fin(1, 0, 0, Some(3))
            .expect("テストフィクスチャの前提条件を満たす");
        tracker
            .record_priority(1, 0, 0, 7)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(tracker.counts.get(&1).copied(), Some(1));
        assert_eq!(union_count(&tracker, 1), 1);

        // 状態エントリの無いキーの FIN。新しいキーなので増える
        tracker
            .mark_fin(1, 1, 0, None)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(tracker.counts.get(&1).copied(), Some(2));
        assert_eq!(union_count(&tracker, 1), 2);

        // 優先度だけのキー。新しいキーなので増える
        tracker
            .record_priority(1, 2, 0, 9)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(tracker.counts.get(&1).copied(), Some(3));
        assert_eq!(union_count(&tracker, 1), 3);

        // 別の Subgroup を open すると増える
        tracker
            .open(1, 3, 0)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(tracker.counts.get(&1).copied(), Some(4));
        assert_eq!(union_count(&tracker, 1), 4);
    }

    /// Track Alias ごとに独立して数えること
    #[test]
    fn counters_are_independent_per_track_alias() {
        let mut tracker = SubgroupTracker::new();
        tracker
            .open(1, 0, 0)
            .expect("テストフィクスチャの前提条件を満たす");
        tracker
            .open(2, 0, 0)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(tracker.counts.get(&1).copied(), Some(1));
        assert_eq!(tracker.counts.get(&2).copied(), Some(1));
        assert_eq!(union_count(&tracker, 1), 1);
        assert_eq!(union_count(&tracker, 2), 1);
    }

    /// 上限超過時に終端済みキーをまとめて破棄し、Open のキーを残すこと
    #[test]
    fn prune_removes_terminal_keys_and_keeps_open_keys() {
        let mut tracker = SubgroupTracker::new();
        // Open のキー
        tracker
            .open(1, 0, 0)
            .expect("テストフィクスチャの前提条件を満たす");
        // 状態エントリが終端したキー
        tracker
            .open(1, 1, 0)
            .expect("テストフィクスチャの前提条件を満たす");
        tracker
            .mark_fin(1, 1, 0, Some(5))
            .expect("テストフィクスチャの前提条件を満たす");
        // 優先度だけの終端済みキー
        tracker
            .record_priority(1, 2, 0, 11)
            .expect("テストフィクスチャの前提条件を満たす");
        // FIN 記録だけの終端済みキー
        tracker
            .mark_fin(1, 3, 0, Some(2))
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(tracker.counts.get(&1).copied(), Some(4));

        tracker.enforce_limit_with(1, 1);

        assert_eq!(
            tracker.get(1, 0, 0),
            Some(&SubgroupStreamState::Open),
            "Open のキーは破棄しないこと"
        );
        assert_eq!(tracker.get(1, 1, 0), None, "終端済みキーは破棄すること");
        assert!(
            !tracker.priorities.contains_key(&(1, 2, 0)),
            "優先度だけのキーも破棄すること"
        );
        assert!(
            !tracker.fin_last_object_ids.contains_key(&(1, 3, 0)),
            "FIN 記録だけのキーも破棄すること"
        );
        assert_eq!(tracker.counts.get(&1).copied(), Some(1));
        assert_eq!(union_count(&tracker, 1), 1);
    }

    /// Open のキーだけで上限を超える場合は破棄しないこと
    #[test]
    fn prune_keeps_all_keys_when_every_key_is_open() {
        let mut tracker = SubgroupTracker::new();
        for subgroup_id in 0..3 {
            tracker
                .open(1, 0, subgroup_id)
                .expect("テストフィクスチャの前提条件を満たす");
        }
        assert_eq!(tracker.counts.get(&1).copied(), Some(3));
        tracker.enforce_limit_with(1, 1);
        assert_eq!(tracker.counts.get(&1).copied(), Some(3));
        assert_eq!(union_count(&tracker, 1), 3);
    }

    /// 破棄されたキーは未知キーとして扱われること
    #[test]
    fn pruned_key_is_treated_as_unknown() {
        let mut tracker = SubgroupTracker::new();
        // Open のキーを 1 つ残す
        tracker
            .open(1, 0, 0)
            .expect("テストフィクスチャの前提条件を満たす");
        // 終端済みのキー
        tracker
            .open(1, 1, 0)
            .expect("テストフィクスチャの前提条件を満たす");
        tracker
            .mark_fin(1, 1, 0, Some(5))
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(tracker.counts.get(&1).copied(), Some(2));

        tracker.enforce_limit_with(1, 1);
        assert_eq!(
            tracker.get(1, 1, 0),
            None,
            "破棄されたキーの状態は観測できないこと"
        );
        assert_eq!(
            tracker.check_object_after_fin(1, 1, 0, 6),
            None,
            "破棄された FIN 記録との条件 2 の比較は行わないこと"
        );

        // 未知キーとして再オープンでき、以前の終端状態は影響しない
        tracker
            .open(1, 1, 0)
            .expect("破棄されたキーは再オープンできること");
        assert_eq!(tracker.get(1, 1, 0), Some(&SubgroupStreamState::Open));
        assert_eq!(tracker.counts.get(&1).copied(), Some(2));
    }

    /// 同じ FIN を繰り返しても件数が増えないこと
    #[test]
    fn repeated_fin_with_same_last_object_does_not_increment_counter() {
        let mut tracker = SubgroupTracker::new();
        tracker
            .mark_fin(1, 0, 0, Some(4))
            .expect("最初の FIN は成功すること");
        tracker
            .mark_fin(1, 0, 0, Some(4))
            .expect("同じ最終 Object ID の FIN は成功すること");
        assert_eq!(tracker.counts.get(&1).copied(), Some(1));
        assert_eq!(union_count(&tracker, 1), 1);
    }

    /// remove_track_alias がカウンタも削除すること
    #[test]
    fn remove_track_alias_removes_counter() {
        let mut tracker = SubgroupTracker::new();
        tracker
            .open(1, 0, 0)
            .expect("テストフィクスチャの前提条件を満たす");
        tracker
            .mark_fin(1, 1, 0, Some(1))
            .expect("テストフィクスチャの前提条件を満たす");
        tracker
            .record_priority(2, 0, 0, 3)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(tracker.counts.get(&1).copied(), Some(2));

        tracker.remove_track_alias(1);

        assert_eq!(tracker.counts.get(&1), None);
        assert_eq!(union_count(&tracker, 1), 0);
        assert_eq!(
            tracker.counts.get(&2).copied(),
            Some(1),
            "別の alias の件数は残ること"
        );
    }
}
