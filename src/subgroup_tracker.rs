//! Subgroup 再オープン禁止の追跡 (draft-ietf-moq-transport-21 §2.2 (Subgroups) / draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams))
//!
//! Subgroup ストリームは一度終端したら同一 `(track_alias, group_id, subgroup_id)` を
//! 別ストリームで再オープンしてはならない。ただし draft-ietf-moq-transport-21 Appendix A.3 (Since draft-ietf-moq-transport-17) #1583 により、
//! STOP_SENDING で停止されたサブグループは REQUEST_UPDATE で Forward State が
//! 0→1 に変更された場合に再オープン可能。本モジュールは session 内でのストリーム
//! ライフサイクルを追跡し、違反時に `PROTOCOL_VIOLATION` として通知するユーティリティ
//! を提供する。
//!
//! # 根拠資料
//!
//! - draft-ietf-moq-transport-21 §2.2 (Subgroups) `from the same Subgroup MUST NOT be sent on different
//!   streams, unless one of the streams was reset prematurely, or upstream conditions
//!   have forced objects from a Subgroup to be sent out of Object ID order.`
//! - draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) `A publisher that receives a STOP_SENDING on a
//!   Subgroup stream SHOULD NOT attempt to open a new stream to deliver additional
//!   Objects in that Subgroup.`
//! - draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams): STOP_SENDING 単独では publisher は新 stream を開くべきではない。
//!   REQUEST_UPDATE で Forward State が 0→1 に変わった場合のみ再オープン MAY
//!   (draft-ietf-moq-transport-21 Appendix A.3 (Since draft-ietf-moq-transport-17) #1583)
//!
//! # 注意
//!
//! 本仕様は draft 由来であり、将来の改訂で例外条件 (prematurely reset, out-of-order
//! upstream) の具体的な許容範囲が変更される可能性がある。

use hashbrown::HashMap;

use crate::error::SESSION_PROTOCOL_VIOLATION;
use crate::session::types::SessionError;

/// Subgroup ストリームの状態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubgroupStreamState {
    /// SUBGROUP_HEADER を受信してストリームがオープン中
    Open,
    /// FIN で正常終端した
    ///
    /// `last_object_id` は FIN 前に受信した最終 Object の ID (draft-ietf-moq-transport-21
    /// §12.1 (Malformed Tracks) 条件 2/3 の検出に使う)。Object を 1 つも受信せずに
    /// FIN した場合は `None`。
    ClosedFin {
        /// FIN 前に受信した最終 Object ID (Object 未受信の場合は `None`)
        last_object_id: Option<u64>,
    },
    /// RESET_STREAM / RESET_STREAM_AT で終端した
    ///
    /// draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams): RESET_STREAM_AT では
    /// `reliable_size` が指定される。古い RESET_STREAM では `None`。
    Reset {
        /// RESET_STREAM_AT の reliable サイズ (古い RESET_STREAM では `None`)
        reliable_size: Option<u64>,
    },
    /// Subscriber 側から STOP_SENDING を受けて終端した
    ///
    /// draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) /
    /// Appendix A.3 (Since draft-ietf-moq-transport-17) #1583:
    /// REQUEST_UPDATE で Forward State が 0→1 に変わった場合に再オープン可能
    StoppedByPeer,
}

impl SubgroupStreamState {
    /// 再オープン可能かどうか
    ///
    /// - draft-ietf-moq-transport-21 §2.2 (Subgroups): premature reset からの再オープン
    /// - draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) /
    ///   Appendix A.3 (Since draft-ietf-moq-transport-17) #1583:
    ///   STOP_SENDING 後に REQUEST_UPDATE で Forward State が 0→1 になった場合の再オープン
    ///
    /// §2.2: "Objects from the same Subgroup MUST NOT be sent on different streams,
    /// unless one of the streams was reset prematurely"
    /// §11.4.3: relay は next Object か不明な場合 "it MUST reset the Subgroup stream
    /// and open a new one to forward it"
    ///
    /// 仕様は reset の理由で区別していないため、StoppedByPeer (STOP_SENDING) と
    /// Reset (DELIVERY_TIMEOUT 等を含む送信側 reset) の両方で再オープンを許可する。
    /// ClosedFin (正常終了) からの再オープンは引き続き禁止する。
    pub fn can_reopen(&self) -> bool {
        matches!(self, Self::StoppedByPeer | Self::Reset { .. })
    }
}

/// `(track_alias, group_id, subgroup_id)` の 3 点組をキーとする
type SubgroupKey = (u64, u64, u64);

/// Subgroup ストリームのライフサイクル追跡
///
/// draft-ietf-moq-transport-21 §2.2 (Subgroups) / draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) に基づき、同一キーの再オープン / 並行 2 本ストリームを拒否する。
///
/// また draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) の条件 1 (Publisher Priority
/// 不一致) / 条件 2 (FIN 済み Subgroup への最終 Object 超過) / 条件 3 (複数 FIN の最終
/// Object 不一致) の検出に必要な状態を保持する。
#[derive(Debug, Default, Clone)]
pub struct SubgroupTracker {
    entries: HashMap<SubgroupKey, SubgroupStreamState>,
    /// Subgroup キーごとに初回オープン時の Publisher Priority を保持する
    /// (draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 1 の検出用)
    priorities: HashMap<SubgroupKey, u8>,
    /// FIN 済み Subgroup の最終 Object ID を保持する (条件 2/3 の検出用)
    ///
    /// `ClosedFin` 状態のエントリが削除されても、条件 2 の検出には過去の最終 Object ID が
    /// 必要なため、state とは別に保持する。
    fin_last_object_ids: HashMap<SubgroupKey, Option<u64>>,
}

impl SubgroupTracker {
    /// 空のトラッカーを作成する
    pub fn new() -> Self {
        Self::default()
    }

    /// 新しい Subgroup ストリームのオープンを記録する
    ///
    /// すでに同一キーのエントリが Open 中の場合は `PROTOCOL_VIOLATION` を返す。
    /// 終端済みの場合は draft-ietf-moq-transport-21 Appendix A.3 (Since draft-ietf-moq-transport-17) #1583
    /// (REQUEST_UPDATE forward 0→1) に基づき、`can_reopen()` が true なら
    /// 再オープンを許可する。それ以外の終端状態は `PROTOCOL_VIOLATION` を返す。
    pub fn open(
        &mut self,
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
    ) -> Result<(), SessionError> {
        let key = (track_alias, group_id, subgroup_id);
        match self.entries.get(&key) {
            Some(state) if state.can_reopen() => {
                self.entries.insert(key, SubgroupStreamState::Open);
                Ok(())
            }
            Some(SubgroupStreamState::Open) => Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "concurrent Subgroup stream open for the same (track_alias, group_id, subgroup_id)",
            )),
            Some(_) => Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "reopening a terminated Subgroup stream is prohibited (draft-ietf-moq-transport-21 §2.2 (Subgroups) / draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams))",
            )),
            None => {
                self.entries.insert(key, SubgroupStreamState::Open);
                Ok(())
            }
        }
    }

    /// FIN での正常終端を記録する (未知キーに対しては無視)
    ///
    /// `last_object_id` は FIN 前に受信した最終 Object の ID。Object を 1 つも受信せずに
    /// FIN した場合は `None`。draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 2/3
    /// の検出に使う。
    ///
    /// 条件 3: 同一 Subgroup キーが 2 回目の FIN を迎えたとき、記録済みの最終 Object ID と
    /// 異なれば Malformed Track に該当する。
    pub fn mark_fin(
        &mut self,
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
        last_object_id: Option<u64>,
    ) -> Result<(), SessionError> {
        let key = (track_alias, group_id, subgroup_id);
        // 条件 3: 同一 Subgroup が複数 stream で FIN され最終 Object が異なる
        if let Some(&prev) = self.fin_last_object_ids.get(&key)
            && prev != last_object_id
        {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "malformed track: same Subgroup FINed with different final Object IDs",
            ));
        }
        self.fin_last_object_ids.insert(key, last_object_id);
        if let Some(state) = self.entries.get_mut(&key) {
            *state = SubgroupStreamState::ClosedFin { last_object_id };
        }
        Ok(())
    }

    /// RESET_STREAM / RESET_STREAM_AT での終端を記録する
    pub fn mark_reset(
        &mut self,
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
        reliable_size: Option<u64>,
    ) {
        let key = (track_alias, group_id, subgroup_id);
        if let Some(state) = self.entries.get_mut(&key) {
            *state = SubgroupStreamState::Reset { reliable_size };
        }
    }

    /// STOP_SENDING 受信による終端を記録する
    pub fn mark_stop_sending(&mut self, track_alias: u64, group_id: u64, subgroup_id: u64) {
        let key = (track_alias, group_id, subgroup_id);
        if let Some(state) = self.entries.get_mut(&key) {
            *state = SubgroupStreamState::StoppedByPeer;
        }
    }

    /// キーに対応する現在の状態を返す
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
    pub fn remove_track_alias(&mut self, track_alias: u64) {
        self.entries
            .retain(|(alias, _, _), _| *alias != track_alias);
        self.priorities
            .retain(|(alias, _, _), _| *alias != track_alias);
        self.fin_last_object_ids
            .retain(|(alias, _, _), _| *alias != track_alias);
    }

    /// Subgroup の Publisher Priority を記録し、既存記録と不一致なら条件 1 として検出する
    ///
    /// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 1:
    /// "An Object with a particular Subgroup ID is received, but its Publisher Priority is
    /// different from that of the previous Object with the same Subgroup ID."
    ///
    /// 初回記録時は Ok。2 回目以降で値が異なれば Err を返す。
    pub fn record_priority(
        &mut self,
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
        publisher_priority: u8,
    ) -> Result<(), SessionError> {
        let key = (track_alias, group_id, subgroup_id);
        match self.priorities.get(&key) {
            Some(&prev) if prev != publisher_priority => Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "malformed track: Publisher Priority differs from previous Object with same Subgroup ID",
            )),
            Some(_) => Ok(()),
            None => {
                self.priorities.insert(key, publisher_priority);
                Ok(())
            }
        }
    }

    /// FIN 済み Subgroup に対して最終 Object ID より大きい Object ID が到着したか検査する
    ///
    /// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 2:
    /// "An Object is received whose Object ID is larger than the final Object in the Subgroup.
    /// The final Object in a Subgroup is the last Object received on a Subgroup stream before
    /// a FIN."
    ///
    /// 違反なら理由文字列を返す。FIN 記録がない・最終 Object ID が不明・ Object ID が
    /// 最終以下の場合は `None` を返す。
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
