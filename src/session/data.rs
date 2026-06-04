//! data stream / datagram の送受信 state
//!
//! draft-ietf-moq-transport-21 §6.3 (Session initialization) / §6.4.1 (Unidirectional Streams) /
//! §11 (Data Streams and Datagrams) に基づき、
//! Session が raw byte parse を持たずに data plane の protocol state を扱うための
//! sans I/O API を提供する。

use crate::error::{
    PUBLISH_DONE_UPDATE_FAILED, SESSION_KEY_VALUE_FORMATTING_ERROR, SESSION_LOCAL_DATAGRAM_TIMEOUT,
    SESSION_LOCAL_FILTER_MISMATCH, SESSION_PROTOCOL_VIOLATION, STREAM_CANCELLED,
    STREAM_MALFORMED_TRACK,
};
use crate::message::{ControlMessage, PublishDone, ReasonPhrase, common::Location};
use crate::stream::{
    DataStreamType, OBJECT_STATUS_END_OF_GROUP, OBJECT_STATUS_END_OF_TRACK, PADDING_DATAGRAM_TYPE,
    classify_data_stream_type,
    datagram::{ObjectDatagram, validate_object_datagram_type},
    decoder::DecodedSubgroupObject,
    fetch::FetchHeader,
    subgroup::{SubgroupHeader, SubgroupIdMode},
};
use crate::varint;
use alloc::vec::Vec;

use super::core::{
    MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION, PUBLISH_DONE_STREAM_COUNT_UNKNOWN, Session,
};
use super::subscription::validation::{
    ObjectFilterInput, header_passes_filters, object_passes_filters,
};
use super::types::{
    DataStreamId, DataStreamResetReason, DatagramAcceptance, FetchState,
    PUBLISHER_PRIORITY_DEFAULT, RecvDataStreamError, RequestKind, RequestStreamEnd, SessionError,
    SessionEvent, SessionState, Subscription, SubscriptionState, TerminationReason,
    TrackDataAcceptance, TrackRole,
};

/// subscription がキャンセル由来 `Terminated` かどうかを判定する
///
/// キャンセル経路は `publish_done` を設定しないため、`Terminated` かつ `publish_done` が
/// `None` であればキャンセル由来と判定できる。PUBLISH_DONE 受信による `Terminated`
/// （`publish_done` が `Some`）は drain 期間中の遅延データを受理するため対象外
/// (draft-ietf-moq-transport-21 §9.9)。
fn is_cancelled_terminated(subscription: &Subscription) -> bool {
    subscription.state == SubscriptionState::Terminated && subscription.publish_done.is_none()
}

/// 受信中の uni data stream の内部状態
#[derive(Debug, Clone, Copy)]
pub(super) enum IncomingDataStream {
    AwaitingHeader {
        kind: DataStreamType,
    },
    Subgroup {
        request_id: u64,
        track_alias: u64,
        group_id: u64,
        subgroup_id: Option<u64>,
        /// 先頭 object を受信済みか (draft-ietf-moq-transport-21 §10.1-12.2: Object Property による
        /// delivery timeout オーバーライドは先頭 object のみ有効)
        first_object_received: bool,
        /// SUBGROUP_HEADER の END_OF_GROUP bit (draft §11.3.1 (Subgroup Header))
        ///
        /// §11.3.1: "indicates that this subgroup contains the largest Object in the Group.
        /// When set to 1, the subscriber can infer the final Object in the Group when the data
        /// stream is terminated by a FIN."
        /// FIN 時に `last_object_id` と組み合わせて Group 終端を確定する。
        end_of_group: bool,
        /// この stream で最後に受信した Object ID (FIN 時の Group 終端確定に使う)
        last_object_id: Option<u64>,
    },
    Fetch {
        request_id: u64,
    },
    /// 破棄対象の受信 data stream
    ///
    /// キャンセル由来 `Terminated`（`publish_done` が `None`）の subscription に属する
    /// stream、または `recv_subgroup_header` が `Discarded` を返した新規 stream を表す。
    /// 以後の受信・終端（SUBGROUP_HEADER / object / FIN / RESET_STREAM / STOP_SENDING）は
    /// すべて no-op で吸収し、`peer_subgroups` tracker 等の内部状態を汚染しない
    /// (draft-ietf-moq-transport-21 §3.1.2: "Objects can arrive after a subscription has been
    /// cancelled. Subscribers SHOULD retain sufficient state to quickly discard these unwanted
    /// Objects, rather than treating them as belonging to an unknown Track Alias.")。
    ///
    /// `request_id` はキャンセル由来候補の subscription が存在する場合に `Some`、
    /// `forget_subscription` 後（候補が存在しない場合）は `None` になる。
    Discarded {
        request_id: Option<u64>,
    },
}

/// 自端点が送信中の FETCH data stream の状態
///
/// draft-ietf-moq-transport-21 §11.4.1 (Fetch Header): FETCH_HEADER は Request ID のみを持ち、
/// Track Alias / Group ID / Subgroup ID を持たない。[`OutgoingDataStream`] とはフィールド構成が
/// 根本的に異なるので別構造体にする (enum 化すると match 腕が肥大化する)。
#[derive(Debug)]
pub(super) struct OutgoingFetchStream {
    pub(super) request_id: u64,
}

/// 自端点が送信中の uni subgroup data stream の内部状態
///
/// draft-ietf-moq-transport-21 §6.4.1 Table 3 は data stream を SUBGROUP_HEADER /
/// FETCH_HEADER の 2 種に enumerate する。本 struct は subgroup stream 専用で、
/// フィールド構成は §11.3.1 (Subgroup Header) の wire 構造に対応し、`subgroup_id` の
/// `Option` は SUBGROUP_ID_MODE `0b01` (FirstObjectId) の解決遅延を表す。
///
/// §11.4.1 (Fetch Header) は Request ID のみで Track Alias / Group ID /
/// Subgroup ID を持たないため本 struct を流用できない。outgoing FETCH stream は
/// [`OutgoingFetchStream`] が別途担当する (enum 化するとフィールド差異により
/// match 腕が肥大化するため別構造体にしている)。
#[derive(Debug)]
pub(super) struct OutgoingDataStream {
    pub(super) request_id: u64,
    pub(super) track_alias: u64,
    pub(super) group_id: u64,
    pub(super) subgroup_id: Option<u64>,
    /// SUBGROUP_HEADER が示す Publisher Priority (draft §10.4 (DEFAULT PUBLISHER PRIORITY))
    ///
    /// DEFAULT_PRIORITY bit が立っている場合は Track Property → 既定値 128 の順で解決した値。
    /// §3.3.2 (Range Filters) の PRIORITY_FILTER 評価に使う。
    pub(super) publisher_priority: u8,
}

impl Session {
    /// subscriber 側の per-subgroup effective delivery timeout を返す
    ///
    /// draft-ietf-moq-transport-21 §10.1-12.2: subgroup の先頭 object に付与された
    /// Object Property による Track-level 値の per-subgroup オーバーライドを反映した
    /// effective timeout を計算して返す。アプリケーション層が incoming ストリームの
    /// timeout 判定に使用する。
    ///
    /// 返り値: (subgroup_delivery_timeout_ms, object_delivery_timeout_ms)
    ///
    /// 計算規則 (draft-ietf-moq-transport-21 §5.2):
    /// - publisher's value = Object Property (先頭 object に存在する場合) or Track Property
    /// - effective = min(publisher's value, subscriber's value) (両方 non-zero の場合)
    pub fn subgroup_effective_delivery_timeout(
        &self,
        request_id: u64,
        group_id: u64,
        subgroup_id: u64,
    ) -> (Option<u64>, Option<u64>) {
        let Some(subscription) = self.subscriptions.get(&request_id) else {
            return (None, None);
        };
        let override_entry = subscription
            .delivery_timeouts
            .subgroup_overrides
            .get(&(group_id, subgroup_id));
        // publisher's value: Object Property override or Track Property fallback
        let publisher_subgroup = match override_entry.and_then(|(s, _)| *s) {
            Some(v) => Some(v),
            None => subscription.delivery_timeouts.publisher_subgroup_ms,
        };
        let publisher_object = match override_entry.and_then(|(_, o)| *o) {
            Some(v) => Some(v),
            None => subscription.delivery_timeouts.publisher_object_ms,
        };
        // effective = min(publisher, subscriber) (draft-ietf-moq-transport-21 §5.2)
        let effective_subgroup =
            super::subscription::delivery::compute_effective_delivery_timeout_ms(
                subscription.delivery_timeouts.subscriber_subgroup_ms,
                publisher_subgroup,
            );
        let effective_object = super::subscription::delivery::compute_effective_delivery_timeout_ms(
            subscription.delivery_timeouts.subscriber_object_ms,
            publisher_object,
        );
        (effective_subgroup, effective_object)
    }

    /// 自端点が送る SUBGROUP_HEADER を Session に通知する
    ///
    /// publisher 側 subscription が確立済みであること、`track_alias` が subscription と
    /// 一致すること、同一 subgroup を再オープンしていないことを検証する。stream は
    /// open 直後に `published_stream_count` へ反映される。
    pub fn send_subgroup_header(
        &mut self,
        stream_id: DataStreamId,
        request_id: u64,
        header: &SubgroupHeader,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        if self.data_streams.outgoing.contains_key(&stream_id) {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing data stream id already registered",
            ));
        }
        let subscription = self.subscriptions.get(&request_id).ok_or_else(|| {
            SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "subscription not found for outgoing subgroup stream",
            )
        })?;
        if subscription.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing subgroup stream requires publisher role",
            ));
        }
        if subscription.state != SubscriptionState::Established {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing subgroup stream requires Established subscription",
            ));
        }
        let Some(track_alias) = subscription.track_alias else {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "publisher subscription is missing track alias",
            ));
        };
        if header.track_alias != track_alias {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing subgroup header track alias does not match subscription",
            ));
        }
        let subgroup_id = match header.subgroup_id {
            SubgroupIdMode::Zero => Some(0),
            SubgroupIdMode::Explicit(id) => Some(id),
            SubgroupIdMode::FirstObjectId => None,
        };
        if let Some(subgroup_id) = subgroup_id {
            self.my_subgroups
                .open(track_alias, header.group_id, subgroup_id)?;
        }
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("subscription must exist while registering outgoing subgroup stream");
        let counts = &mut subscription.stream_counts;
        counts.published_count = counts
            .published_count
            .checked_add(1)
            .ok_or(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "published stream count overflow",
            ))?;
        // draft §10.4 (DEFAULT PUBLISHER PRIORITY): DEFAULT_PRIORITY bit (= header の値が None)
        // なら Track Property → 既定値 128 の順で解決する
        let publisher_priority =
            subscription.resolve_header_publisher_priority(header.publisher_priority);
        self.data_streams.outgoing.insert(
            stream_id,
            OutgoingDataStream {
                request_id,
                track_alias,
                group_id: header.group_id,
                subgroup_id,
                publisher_priority,
            },
        );
        Ok(())
    }

    /// 自端点が送る subgroup object を Session に通知する
    ///
    /// `SubgroupIdMode::FirstObjectId` を使って stream を開いた場合のみ、最初の object を
    /// 見て実効 subgroup id を確定する。その他の stream では no-op。
    ///
    /// `Established` 状態の subscription に属する stream のみ受理する
    /// (draft-ietf-moq-transport-21 §3.1.1 (Subscription State Management):
    /// "Objects MUST NOT be sent for requests that end with an error.")。
    /// `Terminated` 後の outgoing stream は `forget_subscription` まで
    /// `data_streams.outgoing` に残るため、`send_subgroup_header` と同じエラー種別・
    /// メッセージの Established 検証を本関数にも置く。実行順は
    /// Established 検証 → filter 評価 → FirstObjectId 解決 → 簿記であり、
    /// 拒否される呼び出しで内部状態を汚染しない。
    ///
    /// draft-ietf-moq-transport-21 §3.3.3 (Combining Filters) の Pass 評価を行い、
    /// フィルタを通らない Object は [`SESSION_LOCAL_FILTER_MISMATCH`] で拒否する。
    /// `properties_bytes` は Object Properties の生バイト列 (Properties Length varint +
    /// データ) で、OBJECT_PROPERTY_FILTER (0x28) の評価に使う。Object Properties を
    /// 付けない場合は `None` を渡す。
    /// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある
    /// (§5.1.1 の "Objects MUST NOT be sent" と §5.1.5 の Pass 評価の両方に係る)。
    pub fn send_subgroup_object(
        &mut self,
        stream_id: DataStreamId,
        object_id: u64,
        properties_bytes: Option<&[u8]>,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        let Some((request_id, track_alias, group_id, subgroup_id)) =
            self.data_streams.outgoing.get(&stream_id).map(|stream| {
                (
                    stream.request_id,
                    stream.track_alias,
                    stream.group_id,
                    stream.subgroup_id,
                )
            })
        else {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing subgroup object received for unknown stream id",
            ));
        };
        // draft §3.1.1 (Subscription State Management): "Objects MUST NOT be sent for requests
        // that end with an error." REQUEST_UPDATE 失敗応答 (`send_request_error`) で `Terminated` に
        // 遷移した subscription の outgoing stream は `forget_subscription` まで残るため、
        // `Established` でない subscription への送信をここで拒否する
        // (`send_subgroup_header` と同じエラー種別・メッセージ)。
        // 検証は FirstObjectId 解決 (`my_subgroups.open` / `stream.subgroup_id` 代入) より
        // 前に置き、拒否されるはずの呼び出しで内部状態を汚染しない。
        let subscription = self.subscriptions.get(&request_id).ok_or_else(|| {
            SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "subscription not found for outgoing subgroup object",
            )
        })?;
        if subscription.state != SubscriptionState::Established {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing subgroup stream requires Established subscription",
            ));
        }
        // draft §5.1.5 (Combining Filters): Pass = Forward AND Location Filters AND Range Filters。
        // 通らない Object は送信を拒否する (§5.1.5 の "The publisher MUST forward only objects
        // that pass all filters")。filter 評価は FirstObjectId 解決より前に実行し、拒否される
        // 呼び出しで内部状態 (`my_subgroups.open` / `stream.subgroup_id` 代入) を汚染しない。
        // 評価用 subgroup_id は `stream.subgroup_id.or(Some(object_id))` で計算する
        // (draft §11.4.2 の SUBGROUP_ID_MODE 0b01 "the Subgroup ID is the Object ID of the
        // first Object transmitted in this Subgroup" により、未解決なら今回送信するオブジェクト
        // の ID。拒否されたオブジェクトは送信されないため、通過した最初のオブジェクトの ID が
        // subgroup id になる。単純な「移動」では実装できない: 解決前の `subgroup_id` は `None`
        // であり、`range_filters_pass` の `None => true` により SUBGROUP_FILTER が常に通過して
        // しまい、§5.1.5 の MUST に反する転送が防げなくなるため)。
        let evaluation_subgroup_id = subgroup_id.or(Some(object_id));
        let publisher_priority = self
            .data_streams
            .outgoing
            .get(&stream_id)
            .map_or(PUBLISHER_PRIORITY_DEFAULT, |stream| {
                stream.publisher_priority
            });
        let input = ObjectFilterInput {
            location: Location {
                group_id,
                object_id,
            },
            subgroup_id: evaluation_subgroup_id,
            publisher_priority,
            properties_bytes,
        };
        if !object_passes_filters(subscription, &input) {
            return Err(SessionError::new(
                SESSION_LOCAL_FILTER_MISMATCH,
                "outgoing subgroup object does not pass subscription filters",
            ));
        }
        // filter 通過後に FirstObjectId 解決の副作用を実行する (未解決のときのみ。
        // `my_subgroups.open` は同一キーが Open 中のとき `PROTOCOL_VIOLATION` を返すため、
        // 解決済み stream の 2 回目以降の送信で実行しないこと)
        if subgroup_id.is_none() {
            self.my_subgroups.open(track_alias, group_id, object_id)?;
            let Some(stream) = self.data_streams.outgoing.get_mut(&stream_id) else {
                unreachable!(
                    "outgoing subgroup stream must still exist while resolving subgroup id"
                );
            };
            stream.subgroup_id = Some(object_id);
        }
        // alias ではなく stream が保持する request_id を使う。alias は同一 Track の複数
        // subscription で共有されうる (draft §5.1) が、送信 stream は必ず 1 subscription に
        // 属するため、alias 索引を引くと共有時に別 subscription の状態を更新してしまう。
        {
            let published_location = Location {
                group_id,
                object_id,
            };
            if let Some(subscription) = self.subscriptions.get_mut(&request_id) {
                subscription.record_largest_received_location(published_location);
                // delivery_timeouts.effective_object_ms が Some かつ未記録の場合のみ記録。
                // last_tick_ms が None の場合は None で挿入し、最初の tick で確定する。
                // draft-ietf-moq-transport-21 §5.2: object 単位で object header 提供完了時刻を保持する (MUST)。
                // `send_subgroup_object` 呼び出し tick が last header byte 提供時刻の近似になる。
                if subscription.delivery_timeouts.effective_object_ms.is_some() {
                    self.timing
                        .object_delivery_header_complete_ms
                        .entry((stream_id, object_id))
                        .or_insert(self.timing.last_tick_ms);
                }
            }
        }
        Ok(())
    }

    /// 自端点が送信中の data stream を理由付きで reset する
    /// (draft §11.3.2 (Closing Subgroup Streams) / §12.5 (Stream Reset Error Codes))
    ///
    /// §12.5: "The application SHOULD use a relevant error code when resetting or sending
    /// STOP_SENDING on any stream." 理由から適切なコードを [`DataStreamResetReason`] が決める。
    ///
    /// uni stream の早期終了は MOQT application state に影響しないため、本 API は
    /// **subscription の状態を変更しない** (draft-ietf-moq-transport-21 §6.4.1 (Unidirectional Streams)。
    /// 旧 §11.4.1 (Stream Cancellation) は -21 で削除された)。
    /// stream の追跡だけを終端し、`ResetDataStream` イベントを発行する。
    ///
    /// I/O 層はイベントを受けて当該 uni stream を RESET_STREAM で閉じる。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn reset_outgoing_data_stream(
        &mut self,
        stream_id: DataStreamId,
        reason: DataStreamResetReason,
    ) -> Result<(), SessionError> {
        self.reset_outgoing_data_stream_at(stream_id, reason, None)
    }

    /// 生の Stream Reset Error Code を指定して data stream を reset する
    ///
    /// [`DataStreamResetReason`] に無いコード (将来の draft 追加や実装独自の値) を使う場合の
    /// 逃げ道。通常は [`reset_outgoing_data_stream`](Self::reset_outgoing_data_stream) を使う。
    /// subgroup stream と fill fetch stream (draft-ietf-moq-transport-21 §3.4.1 の
    /// 失敗時即 reset 経路を含む) の両方に使える。
    pub fn reset_outgoing_data_stream_with_code(
        &mut self,
        stream_id: DataStreamId,
        error_code: u64,
    ) -> Result<(), SessionError> {
        self.reset_outgoing_data_stream_at_with_code(stream_id, error_code, None)
    }

    /// RESET_STREAM_AT で data stream を reset する
    ///
    /// [`reset_outgoing_data_stream`](Self::reset_outgoing_data_stream) と同じだが、
    /// `reliable_size` を指定して RESET_STREAM_AT を発行できる
    /// (draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams))。
    /// `reliable_size` が `Some(n)` の場合は RESET_STREAM_AT を `n` で発行し、
    /// `None` の場合は従来の RESET_STREAM を発行する。
    /// 渡した値は `ResetDataStream` イベントの同名フィールドと自側 stream 追跡の
    /// 終端状態の両方に一貫して反映される。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn reset_outgoing_data_stream_at(
        &mut self,
        stream_id: DataStreamId,
        reason: DataStreamResetReason,
        reliable_size: Option<u64>,
    ) -> Result<(), SessionError> {
        self.reset_outgoing_data_stream_at_with_code(stream_id, reason.error_code(), reliable_size)
    }

    /// 生の Stream Reset Error Code を指定して RESET_STREAM_AT で data stream を reset する
    ///
    /// [`reset_outgoing_data_stream_with_code`](Self::reset_outgoing_data_stream_with_code) と同じだが、
    /// `reliable_size` を指定して RESET_STREAM_AT を発行できる
    /// (draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams))。
    /// `reliable_size` が `Some(n)` の場合は RESET_STREAM_AT を `n` で発行し、
    /// `None` の場合は従来の RESET_STREAM を発行する。
    /// 渡した値は `ResetDataStream` イベントの同名フィールドと自側 stream 追跡の
    /// 終端状態の両方に一貫して反映される。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn reset_outgoing_data_stream_at_with_code(
        &mut self,
        stream_id: DataStreamId,
        error_code: u64,
        reliable_size: Option<u64>,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        // RESET として stream 追跡を終端する。§6.4.1 (Unidirectional Streams) により subscription 状態は変えない。
        // request_id は `send_data_stream_closed` が stream を除去する前に取得する
        // (保留 PUBLISH_DONE の送信判定に使う)
        if self.data_streams.outgoing.contains_key(&stream_id) {
            let request_id = self
                .data_streams
                .outgoing
                .get(&stream_id)
                .map(|s| s.request_id)
                .ok_or_else(|| {
                    SessionError::new(
                        SESSION_PROTOCOL_VIOLATION,
                        "outgoing data stream close received for unknown stream id",
                    )
                })?;
            self.send_data_stream_closed(
                stream_id,
                RequestStreamEnd::Reset {
                    error_code,
                    reliable_size,
                },
            )?;
            self.events.push_back(SessionEvent::ResetDataStream {
                stream_id,
                error_code,
                reliable_size,
            });
            // 保留中の PUBLISH_DONE (UPDATE_FAILED) は `ResetDataStream` イベントより後に push する
            // (ワイヤ順序を RESET_STREAM → PUBLISH_DONE に保ち、§9.9 の MUST NOT を満たす。
            // stream 終端時点で該当 request の全 stream が閉じ、subscription が Terminated で
            // 保留がある場合のみ送信される)
            self.maybe_flush_pending_publish_done(request_id);
            return Ok(());
        }
        // fill fetch stream (draft-ietf-moq-transport-21 §3.4) の単体 reset。
        // subgroup 用 tracker がないため除去とイベント発行のみ行う
        // (例: fill 失敗時に FETCH_HEADER 直後で即 reset する §3.4.1 の経路)。
        // 保留 PUBLISH_DONE の扱いは subgroup 経路と対称である。
        if let Some(request_id) = self
            .data_streams
            .outgoing_fill
            .get(&stream_id)
            .map(|s| s.request_id)
        {
            self.data_streams.outgoing_fill.remove(&stream_id);
            self.events.push_back(SessionEvent::ResetDataStream {
                stream_id,
                error_code,
                reliable_size,
            });
            self.maybe_flush_pending_publish_done(request_id);
            return Ok(());
        }
        Err(SessionError::new(
            SESSION_PROTOCOL_VIOLATION,
            "outgoing data stream close received for unknown stream id",
        ))
    }

    /// 自端点が開いた uni data stream の終端を Session に通知する
    ///
    /// publisher 側 subgroup stream のみを扱う。FIN / RESET のどちらでも open stream
    /// 集合から除去し、reopen prohibition 用 tracker を終端状態へ進める。
    ///
    /// RESET 時の適切なエラーコード選択は
    /// [`reset_outgoing_data_stream`](Self::reset_outgoing_data_stream) が行う。
    /// 本 API は既に I/O 層が閉じた stream を Session へ通知する用途で、
    /// `ResetDataStream` イベントは発行しない。
    pub fn send_data_stream_closed(
        &mut self,
        stream_id: DataStreamId,
        end: RequestStreamEnd,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        let Some(stream) = self.data_streams.outgoing.remove(&stream_id) else {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing data stream close received for unknown stream id",
            ));
        };
        // ストリーム終端時に delivery timeout 追跡を除去する
        // キーが (stream_id, object_id) の複合キーのため、同一 stream_id の全エントリを削除する
        self.timing
            .object_delivery_header_complete_ms
            .retain(|&(sid, _), _| sid != stream_id);
        self.timing.subgroup_delivery_fin_ms.remove(&stream_id);
        let OutgoingDataStream {
            track_alias,
            group_id,
            subgroup_id,
            request_id,
            ..
        } = stream;
        // SUBGROUP_DELIVERY_TIMEOUT: subgroup stream の FIN 検出時にタイマーを開始する
        // draft-ietf-moq-transport-21 §5.2 (Delivery Timeouts and Data Reliability): subgroup stream が
        // FIN で終了した場合、SUBGROUP_DELIVERY_TIMEOUT のタイマーを開始し、
        // 期限内に全てのデータがコミットされなければ DELIVERY_TIMEOUT でリセットする (MUST)
        if matches!(end, RequestStreamEnd::Fin)
            && let Some(subscription) = self.subscriptions.get(&request_id)
            && subscription
                .delivery_timeouts
                .effective_subgroup_ms
                .is_some()
        {
            self.timing
                .subgroup_delivery_fin_ms
                .insert(stream_id, (self.timing.last_tick_ms, request_id));
        }
        if let Some(subgroup_id) = subgroup_id {
            match end {
                RequestStreamEnd::Fin => {
                    // 送信側は条件 3 の検出対象外 (自端点が publisher のため)
                    let _ = self
                        .my_subgroups
                        .mark_fin(track_alias, group_id, subgroup_id, None);
                }
                RequestStreamEnd::Reset { reliable_size, .. } => {
                    self.my_subgroups
                        .mark_reset(track_alias, group_id, subgroup_id, reliable_size);
                }
            }
        }
        // FIN 終端時のみ、保留中の PUBLISH_DONE (UPDATE_FAILED) を自動送信する
        // (RESET 経路は `reset_outgoing_data_stream_with_code` が `ResetDataStream` イベントを
        // 先に push してから送信する。ここで送るとワイヤ順序が PUBLISH_DONE → RESET_STREAM
        // になり §9.9 の MUST NOT に反する)
        if matches!(end, RequestStreamEnd::Fin) {
            self.maybe_flush_pending_publish_done(request_id);
        }
        Ok(())
    }

    /// 保留中の PUBLISH_DONE (UPDATE_FAILED) を全 stream 終端後に自動送信する
    ///
    /// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE) の MUST NOT (全 stream を閉じるまで
    /// PUBLISH_DONE を送ってはならない) により `send_request_error` が保留した
    /// `Subscription::pending_publish_done` を、該当 request の全 outgoing subgroup stream が
    /// 閉じた時点で送信する (§9.5.1 の MUST)。
    /// 条件は「全 stream 終端 (open なし) + subscription が Terminated + 保留あり」。
    /// `take()` で取り出すため二重 push は起きない。
    /// delivery timeout によるリセット経路 (`tick_subscription_timeouts`) は outgoing から
    /// 除去しないため open が残り、本関数は発火しない (アプリの終端通知まで保留される。
    /// ワイヤ順序は RESET_STREAM 先行で違反にはならない)。
    /// アプリが stream を閉じないまま `forget_subscription` を呼んだ場合は、保留中の
    /// PUBLISH_DONE は push されずに破棄される (§9.9 の MUST NOT により stream が閉じる
    /// まで送れないため。§9.5.1 の MUST が果たせない場合の帰結であり、REQUEST_UPDATE
    /// 失敗応答後は全 stream を閉じること)。
    pub(super) fn maybe_flush_pending_publish_done(&mut self, request_id: u64) {
        if self.has_open_outgoing_data_streams_for_request(request_id) {
            return;
        }
        let Some(subscription) = self.subscriptions.get_mut(&request_id) else {
            return;
        };
        if subscription.state != SubscriptionState::Terminated {
            return;
        }
        let Some(stream_count) = subscription.pending_publish_done.take() else {
            return;
        };
        self.events.push_back(SessionEvent::SendOnStream {
            request_id,
            message: ControlMessage::PublishDone(PublishDone {
                status_code: PUBLISH_DONE_UPDATE_FAILED,
                stream_count,
                reason: ReasonPhrase::new("").expect("empty reason phrase is always valid"),
            }),
            // PUBLISH_DONE が最終メッセージのため送信後に FIN する (§9.9)
            fin: true,
        });
    }

    /// peer から自端点が開いた uni data stream へ STOP_SENDING が届いたことを通知する
    ///
    /// subgroup stream に対する `STOP_SENDING` は reopen prohibition 用 tracker に
    /// 反映するが、local endpoint がまだ FIN / RESET を流していない可能性があるため、
    /// open stream 集合からは即時には除去しない。
    /// fill fetch stream に対する `STOP_SENDING` は subscriber による独立 cancel であり
    /// (draft-ietf-moq-transport-21 §3.4.1)、当該 stream のみ除去して `Ok` で吸収する
    /// (subscription には影響しない。保留 PUBLISH_DONE があり最後の stream が
    /// なくなれば flush する)。
    pub fn recv_data_stream_stop_sending(
        &mut self,
        stream_id: DataStreamId,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        // 保留 PUBLISH_DONE の flush は `reset_outgoing_data_stream_with_code` と対称に扱う。
        if let Some(request_id) = self
            .data_streams
            .outgoing_fill
            .get(&stream_id)
            .map(|stream| stream.request_id)
        {
            self.data_streams.outgoing_fill.remove(&stream_id);
            self.maybe_flush_pending_publish_done(request_id);
            return Ok(());
        }
        let Some((track_alias, group_id, subgroup_id)) = self
            .data_streams
            .outgoing
            .get(&stream_id)
            .map(|stream| (stream.track_alias, stream.group_id, stream.subgroup_id))
        else {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "STOP_SENDING received for unknown outgoing data stream id",
            ));
        };
        if let Some(subgroup_id) = subgroup_id {
            self.my_subgroups
                .mark_stop_sending(track_alias, group_id, subgroup_id);
        }
        Ok(())
    }

    /// 受信 uni stream の type varint を Session に通知する
    ///
    /// control stream は対象外。caller は control stream を別途扱い、
    /// それ以外の uni stream について本 API を呼ぶ。
    ///
    /// draft §6.3 (Session initialization) に従い、SETUP 完了前の到着は `BeforeSessionEstablished`
    /// を返して caller に buffer 判断を委ねる。unknown stream type は draft §6.4.1 (Unidirectional Streams) に
    /// 従い `PROTOCOL_VIOLATION` で session を閉じる。
    pub fn recv_data_stream_type(
        &mut self,
        stream_id: DataStreamId,
        stream_type: u64,
    ) -> Result<DataStreamType, RecvDataStreamError> {
        if self.state != SessionState::Established {
            return Err(RecvDataStreamError::BeforeSessionEstablished);
        }
        if self.data_streams.incoming.contains_key(&stream_id) {
            return Err(RecvDataStreamError::InvalidInput(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "data stream type already registered for stream id",
            )));
        }
        let Some(kind) = classify_data_stream_type(stream_type) else {
            let err = SessionError::new(SESSION_PROTOCOL_VIOLATION, "unknown data stream type");
            self.fail(err.clone());
            return Err(RecvDataStreamError::Session(err));
        };
        // 破棄対象の保持集合に含まれる stream id への再通知は kind の分類のみ行い、
        // 再登録しない (DATA_STREAM_TIMEOUT の監視対象に戻さない。以降の受信・終端は
        // 保持集合のチェックで no-op で吸収される)
        if self.data_streams.discarded.contains_key(&stream_id) {
            return Ok(kind);
        }
        self.data_streams
            .incoming
            .insert(stream_id, IncomingDataStream::AwaitingHeader { kind });
        // draft-ietf-moq-transport-21 §11.5.1 (Padding Streams): padding stream は
        // 後続データ (ヘッダ・ Object) を持たないため、DATA_STREAM_TIMEOUT の
        // 監視対象に含めない。登録すると timestamp が永久に更新されず、
        // 必ず timeout を誘発してしまう。
        if kind != DataStreamType::Padding
            && let Some(now_ms) = self.timing.last_tick_ms
        {
            self.timing
                .data_stream_last_activity_ms
                .insert(stream_id, now_ms);
        }
        Ok(kind)
    }

    /// 受信した SUBGROUP_HEADER を Session に通知する
    ///
    /// unknown Track Alias は draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header) に従い session close にせず、
    /// `UnknownTrackAlias` を返す。caller は stream を abandon するか、
    /// control message 到着を待って短時間 buffer するかを選べる。
    pub fn recv_subgroup_header(
        &mut self,
        stream_id: DataStreamId,
        header: &SubgroupHeader,
    ) -> Result<TrackDataAcceptance, SessionError> {
        self.require_established()?;
        // 保持集合に含まれる stream id（終端済みの破棄対象 stream）への再受信は
        // no-op で吸収する
        if self.data_streams.discarded.contains_key(&stream_id) {
            return Ok(TrackDataAcceptance::Discarded);
        }
        let mut cancelled_request_id = None;
        match self.data_streams.incoming.get(&stream_id) {
            Some(IncomingDataStream::AwaitingHeader {
                kind: DataStreamType::Subgroup,
            }) => {}
            Some(IncomingDataStream::AwaitingHeader {
                kind: DataStreamType::Fetch,
            }) => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "subgroup header received on fetch stream",
                ));
            }
            Some(IncomingDataStream::Subgroup { request_id, .. }) => {
                if self.is_cancelled_terminated_subscription(*request_id) {
                    // キャンセル由来 Terminated に属する既存 stream への再 SUBGROUP_HEADER は
                    // 破棄対象として no-op で吸収する
                    cancelled_request_id = Some(*request_id);
                } else {
                    return Err(SessionError::new(
                        SESSION_PROTOCOL_VIOLATION,
                        "subgroup header already received for stream id",
                    ));
                }
            }
            Some(IncomingDataStream::Fetch { .. }) => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "subgroup header received after fetch header",
                ));
            }
            Some(IncomingDataStream::AwaitingHeader {
                kind: DataStreamType::Padding,
            }) => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "subgroup header received on padding stream",
                ));
            }
            Some(IncomingDataStream::Discarded { .. }) => {
                return Ok(TrackDataAcceptance::Discarded);
            }
            None => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "subgroup header received for unknown stream id",
                ));
            }
        }
        if let Some(request_id) = cancelled_request_id {
            self.register_discarded_stream(stream_id, Some(request_id));
            return Ok(TrackDataAcceptance::Discarded);
        }

        // draft §3.1 (Subscriptions): alias は同一 Track の複数 subscription で共有されうる。
        // "the subscriber re-applies each subscription's filter to determine which subscription
        // a received Object belongs to."
        // header 時点で評価できるフィルタ (Location Filter の Group 部分・ SUBGROUP_FILTER ・
        // PRIORITY_FILTER) で候補を絞り、最初に通過した subscription に紐づける。
        // OBJECTID_FILTER と OBJECT_PROPERTY_FILTER は Object 受信時まで判定できないため
        // header 時点では評価対象外とする。
        let candidates = self.resolve_peer_track_alias(header.track_alias)?;
        if candidates.is_empty() {
            // draft §3.1.2 (Track Alias): キャンセル直後の遅延 Object は未知 alias ではなく
            // 「不要 Object」として破棄させる
            if self.peer_alias_is_tombstoned(header.track_alias) {
                // 新規 stream は破棄対象として登録する (forget 後は request_id が不明のため None)
                self.register_discarded_stream(stream_id, None);
                return Ok(TrackDataAcceptance::Discarded);
            }
            return Ok(TrackDataAcceptance::UnknownTrackAlias);
        }
        let subgroup_id = match header.subgroup_id {
            SubgroupIdMode::Zero => Some(0),
            SubgroupIdMode::Explicit(id) => Some(id),
            SubgroupIdMode::FirstObjectId => None,
        };
        // 候補を順に評価し、最初に通過した subscription に紐づける。
        // publisher_priority の解決は subscription ごとに異なるため候補ごとに計算する。
        // キャンセル由来 `Terminated` の候補は受理対象から除外しつつフィルタ評価だけは
        // 実行し、合格したキャンセル由来候補のみの場合は `Discarded` で返り値を一意に決める
        // (draft §3.1.2 の「不要 Object として破棄」)。`Established` の候補が合格した場合は
        // 従来どおり受理する。
        let mut matched_request_id = None;
        let mut cancelled_matched_request_id = None;
        for &candidate_id in &candidates {
            if let Some(subscription) = self.subscriptions.get(&candidate_id) {
                let publisher_priority =
                    subscription.resolve_header_publisher_priority(header.publisher_priority);
                if header_passes_filters(
                    subscription,
                    header.group_id,
                    subgroup_id,
                    publisher_priority,
                ) {
                    if is_cancelled_terminated(subscription) {
                        // 複数合格時は既存の候補評価ループと同じく最初に合格した候補を保持する
                        // (上書きしない。後続の Established 候補の評価を失わないため break もしない)
                        if cancelled_matched_request_id.is_none() {
                            cancelled_matched_request_id = Some(candidate_id);
                        }
                    } else {
                        matched_request_id = Some(candidate_id);
                        break;
                    }
                }
            }
        }
        let Some(request_id) = matched_request_id else {
            if let Some(request_id) = cancelled_matched_request_id {
                // 合格した候補がキャンセル由来のみ。stream を破棄対象として登録し、
                // tombstone 判定にフォールバックしない (保持期間中は unknown alias 扱いに
                // してはならない。draft §3.1.2 の "rather than treating them as belonging to
                // an unknown Track Alias" に反する)。
                self.register_discarded_stream(stream_id, Some(request_id));
                return Ok(TrackDataAcceptance::Discarded);
            }
            return Ok(TrackDataAcceptance::FilteredOut);
        };
        if let Some(subgroup_id) = subgroup_id
            && let Err(err) =
                self.peer_subgroups
                    .open(header.track_alias, header.group_id, subgroup_id)
        {
            self.fail(err.clone());
            return Err(err);
        }
        if let Err(err) = self.note_incoming_subgroup_stream_opened(request_id) {
            self.fail(err.clone());
            return Err(err);
        }
        self.data_streams.incoming.insert(
            stream_id,
            IncomingDataStream::Subgroup {
                request_id,
                track_alias: header.track_alias,
                group_id: header.group_id,
                subgroup_id,
                first_object_received: false,
                end_of_group: header.end_of_group,
                last_object_id: None,
            },
        );
        // draft-ietf-moq-transport-21 §10.4 (DEFAULT PUBLISHER PRIORITY):
        // "Subgroups and Datagrams for this subscription inherit this priority, unless they
        // specifically override it." / "If omitted, the Default Publisher Priority is 128."
        //
        // SUBGROUP_HEADER の DEFAULT_PRIORITY bit が立っている (= `publisher_priority` が
        // `None`) 場合は override が無いので、Track Property の DEFAULT_PUBLISHER_PRIORITY を
        // 使い、それも無ければ既定値 128 を適用する。bit が立っていなければ header の値が
        // override になる。
        let resolved_priority = if let Some(subscription) = self.subscriptions.get_mut(&request_id)
        {
            let priority =
                subscription.resolve_header_publisher_priority(header.publisher_priority);
            subscription.publisher_priority = Some(priority);
            priority
        } else {
            header
                .publisher_priority
                .unwrap_or(PUBLISHER_PRIORITY_DEFAULT)
        };
        // draft §12.1 (Malformed Tracks) 条件 1: 同一 Subgroup ID の直前 Object と
        // Publisher Priority が異なる場合を検出する。subgroup_id が確定している場合のみ
        // 記録できる (FirstObjectId モードは recv_subgroup_object で解決後に記録する)。
        if let Some(subgroup_id) = subgroup_id
            && let Err(err) = self.peer_subgroups.record_priority(
                header.track_alias,
                header.group_id,
                subgroup_id,
                resolved_priority,
            )
        {
            self.terminate_malformed_track(request_id, Some(stream_id), err.reason);
            return Err(err);
        }
        if let Some(now_ms) = self.timing.last_tick_ms {
            self.timing
                .data_stream_last_activity_ms
                .insert(stream_id, now_ms);
        }
        Ok(TrackDataAcceptance::Accepted)
    }

    /// `DecodedSubgroupObject` を Session に通知する
    ///
    /// caller は対応する `SubgroupStreamDecoder` を回し、header 受理後に object を
    /// 逐次通知する。`SubgroupIdMode::FirstObjectId` は最初の object 受信時に解決する。
    pub fn recv_subgroup_object(
        &mut self,
        stream_id: DataStreamId,
        object: &DecodedSubgroupObject,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        // 破棄対象 stream への object 受信は no-op で吸収する (保持集合・ Discarded variant)
        if self.data_streams.discarded.contains_key(&stream_id) {
            return Ok(());
        }
        // キャンセル由来 Terminated に属する既存 stream を破棄対象へ変換して no-op で吸収する
        // (内部状態を汚染しない)
        if let Some(IncomingDataStream::Subgroup { request_id, .. }) =
            self.data_streams.incoming.get(&stream_id)
            && self.is_cancelled_terminated_subscription(*request_id)
        {
            self.register_discarded_stream(stream_id, Some(*request_id));
            return Ok(());
        }
        let (request_id, track_alias, group_id, resolve_subgroup_id, is_first_object) =
            match self.data_streams.incoming.get_mut(&stream_id) {
                Some(IncomingDataStream::Subgroup {
                    request_id,
                    track_alias,
                    group_id,
                    subgroup_id,
                    first_object_received,
                    last_object_id,
                    ..
                }) => {
                    let is_first = !*first_object_received;
                    *first_object_received = true;
                    // FIN 時に Group 終端を確定するため最終 Object ID を覚えておく
                    // (draft §11.3.1 (Subgroup Header) の END_OF_GROUP bit)
                    *last_object_id = Some(object.object_id);
                    (
                        *request_id,
                        *track_alias,
                        *group_id,
                        subgroup_id.is_none(),
                        is_first,
                    )
                }
                Some(IncomingDataStream::AwaitingHeader { .. }) => {
                    return Err(SessionError::new(
                        SESSION_PROTOCOL_VIOLATION,
                        "subgroup object received before subgroup header",
                    ));
                }
                Some(IncomingDataStream::Fetch { .. }) => {
                    return Err(SessionError::new(
                        SESSION_PROTOCOL_VIOLATION,
                        "subgroup object received on fetch stream",
                    ));
                }
                Some(IncomingDataStream::Discarded { .. }) => {
                    return Ok(());
                }
                None => {
                    return Err(SessionError::new(
                        SESSION_PROTOCOL_VIOLATION,
                        "subgroup object received for unknown stream id",
                    ));
                }
            };

        if resolve_subgroup_id {
            if let Err(err) = self
                .peer_subgroups
                .open(track_alias, group_id, object.object_id)
            {
                self.fail(err.clone());
                return Err(err);
            }
            match self.data_streams.incoming.get_mut(&stream_id) {
                Some(IncomingDataStream::Subgroup { subgroup_id, .. }) => {
                    *subgroup_id = Some(object.object_id);
                }
                _ => {
                    return Err(SessionError::new(
                        SESSION_PROTOCOL_VIOLATION,
                        "subgroup stream state changed before first object",
                    ));
                }
            }
        }

        // draft-ietf-moq-transport-21 §10.1-12.2: subgroup の先頭 object に付与された
        // SUBGROUP_DELIVERY_TIMEOUT / OBJECT_DELIVERY_TIMEOUT は Track-level 値を
        // per-subgroup で上書きする。先頭以外の object では無視される。
        if is_first_object
            && let Some(properties_bytes) = object.properties_bytes.as_deref()
            && let Ok((properties, _)) =
                crate::object_properties::ObjectProperties::decode(properties_bytes)
        {
            let subgroup_timeout = properties.subgroup_delivery_timeout();
            let object_timeout = properties.object_delivery_timeout();
            if subgroup_timeout.is_some() || object_timeout.is_some() {
                // subgroup_id 解決後に現在の subgroup_id を取得する
                let resolved_subgroup_id =
                    self.data_streams
                        .incoming
                        .get(&stream_id)
                        .and_then(|stream| match stream {
                            IncomingDataStream::Subgroup { subgroup_id, .. } => *subgroup_id,
                            _ => None,
                        });
                if let (Some(subgroup_id), Some(subscription)) = (
                    resolved_subgroup_id,
                    self.subscriptions.get_mut(&request_id),
                ) {
                    subscription
                        .delivery_timeouts
                        .subgroup_overrides
                        .insert((group_id, subgroup_id), (subgroup_timeout, object_timeout));
                }
            }
        }

        // draft §12.1 (Malformed Tracks) 条件 2: FIN 済み Subgroup に対して最終 Object ID より
        // 大きい Object ID の Object が来たら Malformed Track。
        // 条件 1 (FirstObjectId モード): subgroup_id が最初の Object で解決される場合、
        // この時点で priority を記録する。
        if let Some(IncomingDataStream::Subgroup {
            subgroup_id: Some(resolved_subgroup_id),
            track_alias: resolved_track_alias,
            ..
        }) = self.data_streams.incoming.get(&stream_id)
        {
            let resolved_subgroup_id = *resolved_subgroup_id;
            let resolved_track_alias = *resolved_track_alias;
            if let Some(reason) = self.peer_subgroups.check_object_after_fin(
                resolved_track_alias,
                group_id,
                resolved_subgroup_id,
                object.object_id,
            ) {
                let err = SessionError::new(SESSION_PROTOCOL_VIOLATION, reason);
                self.terminate_malformed_track(request_id, Some(stream_id), reason);
                return Err(err);
            }
            // 条件 1: FirstObjectId モードで subgroup_id が遅延解決された場合、
            // header 時点では記録できないためここで priority を記録する。
            if resolve_subgroup_id && let Some(subscription) = self.subscriptions.get(&request_id) {
                let priority = subscription.effective_publisher_priority();
                if let Err(err) = self.peer_subgroups.record_priority(
                    resolved_track_alias,
                    group_id,
                    resolved_subgroup_id,
                    priority,
                ) {
                    self.terminate_malformed_track(request_id, Some(stream_id), err.reason);
                    return Err(err);
                }
            }
        }

        let received_location = Location {
            group_id,
            object_id: object.object_id,
        };
        if let Some(subscription) = self.subscriptions.get_mut(&request_id) {
            subscription.record_largest_received_location(received_location);
        }

        // draft §11.1.2 (Object Status) / §12.1 (Malformed Tracks): 終端宣言後の Object は
        // Malformed Track にあたる。該当 subscription だけを cancel する。
        if let Some(reason) = self.object_after_track_end(
            request_id,
            &Location {
                group_id,
                object_id: object.object_id,
            },
        ) {
            let err = SessionError::new(SESSION_PROTOCOL_VIOLATION, reason);
            self.terminate_malformed_track(request_id, Some(stream_id), reason);
            return Err(err);
        }
        // draft §11.1.2 (Object Status): End of Group / End of Track を状態として記録する
        self.record_object_status_end(request_id, group_id, object.object_id, object.status);
        if let Err(msg_err) = self
            .peer_object_properties
            .entry(request_id)
            .or_default()
            .observe_object(
                group_id,
                object.object_id,
                object.properties_bytes.as_deref(),
            )
        {
            // draft §12.1 (Malformed Tracks) 条件 3/4/5: `ObjectPropertyTracker::observe_object`
            // は PRIOR_GROUP_ID_GAP / PRIOR_OBJECT_ID_GAP の一貫性違反 (gap 内に後続 Object を受信する等)
            // を Malformed Track として検出する。IMMUTABLE_PROPERTIES (0x0B) の raw バイト列不一致
            // (条件 6 の "other immutable properties") は Sans I/O + no_std 制約下では保持不能のため、
            // `ObjectFieldTracker` の doc のとおり Session では検出しない (app / relay 層の責務)。
            // Malformed Track はセッション全体ではなく該当 subscription だけを cancel する。
            let err = session_error_from_data_message(msg_err);
            self.terminate_malformed_track(request_id, Some(stream_id), err.reason);
            return Err(err);
        }
        // draft §12.1 (Malformed Tracks) 条件 6/7, §7.1 (Caching Relays):
        // 重複 Object の Forwarding Preference / Subgroup ID / Priority 一貫性を検証する。
        // subgroup stream 経由なので is_subgroup = true。
        {
            let resolved_subgroup_id =
                self.data_streams
                    .incoming
                    .get(&stream_id)
                    .and_then(|stream| match stream {
                        IncomingDataStream::Subgroup { subgroup_id, .. } => *subgroup_id,
                        _ => None,
                    });
            let publisher_priority = self
                .subscriptions
                .get(&request_id)
                .map_or(PUBLISHER_PRIORITY_DEFAULT, |s| {
                    s.effective_publisher_priority()
                });
            if let Err(mismatch) = self
                .peer_object_fields
                .entry(request_id)
                .or_default()
                .observe_object_fields(
                    group_id,
                    object.object_id,
                    true,
                    resolved_subgroup_id,
                    publisher_priority,
                )
            {
                self.terminate_malformed_track(request_id, Some(stream_id), mismatch.reason);
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    mismatch.reason,
                ));
            }
        }
        // draft-ietf-moq-transport-21 §6.6 (Termination): DATA_STREAM_TIMEOUT は
        // "an object header within a data stream" も activity に含む。
        // Object 受信ごとに timestamp を更新し、継続受信中のストリームが
        // 誤って timeout で閉じられるのを防ぐ。
        if let Some(now_ms) = self.timing.last_tick_ms {
            self.timing
                .data_stream_last_activity_ms
                .insert(stream_id, now_ms);
        }
        Ok(())
    }

    /// 自端点が送る FETCH_HEADER を Session に通知する (draft §11.4.1 (Fetch Header))
    ///
    /// publisher 側の FETCH が Established であることを検証し、outgoing FETCH stream として
    /// 登録する。§11.4.1 の FETCH_HEADER は Request ID のみを持つので引数もそれだけである。
    ///
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn send_fetch_header(
        &mut self,
        stream_id: DataStreamId,
        request_id: u64,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        if self.data_streams.outgoing.contains_key(&stream_id)
            || self.data_streams.outgoing_fetch.contains_key(&stream_id)
            || self.data_streams.outgoing_fill.contains_key(&stream_id)
        {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing data stream id already registered",
            ));
        }
        let fetch = self.fetches.get(&request_id).ok_or_else(|| {
            SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "fetch not found for outgoing fetch stream",
            )
        })?;
        if fetch.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing fetch stream requires publisher role",
            ));
        }
        if fetch.state != FetchState::Established {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing fetch stream requires Established fetch",
            ));
        }
        self.data_streams
            .outgoing_fetch
            .insert(stream_id, OutgoingFetchStream { request_id });
        Ok(())
    }

    /// 自端点が送る fill fetch stream の FETCH_HEADER を Session に通知する
    /// (draft-ietf-moq-transport-21 §3.4 (Fill Semantics) / §11.4.1 (Fetch Header))
    ///
    /// `SessionEvent::OpenFillFetchStream` を受けたアプリが uni stream を開き、
    /// FETCH_HEADER (起因 SUBSCRIBE / REQUEST_UPDATE の Request ID を載せる) を
    /// 書いたときに呼ぶ。`subscription_request_id` には fill 対象 subscription の
    /// Request ID を渡す。通常の FETCH 応答 (`send_fetch_header`) とは異なり
    /// `Fetch` 状態に紐づかず subscription に direct に紐づくため、1 つの
    /// subscription に複数本が同時に開くことがある。
    ///
    /// 失敗条件: 未確立セッション / `stream_id` の二重登録 / 未知 subscription /
    /// 非 publisher 役 / `Terminated` subscription。SUBSCRIBE 処理直後の
    /// `Pending` は許容する (確立前の Object 送出可否はアプリが判断する)。
    ///
    /// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE): Stream Count は fill fetch
    /// stream を含むため、登録時に `published_count` を加算する。アプリは
    /// `send_publish_done` の `stream_count` に fill stream 数を含めること。
    ///
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn send_fill_fetch_header(
        &mut self,
        stream_id: DataStreamId,
        subscription_request_id: u64,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        if self.data_streams.outgoing.contains_key(&stream_id)
            || self.data_streams.outgoing_fetch.contains_key(&stream_id)
            || self.data_streams.outgoing_fill.contains_key(&stream_id)
        {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing data stream id already registered",
            ));
        }
        let subscription = self
            .subscriptions
            .get(&subscription_request_id)
            .ok_or_else(|| {
                SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "subscription not found for outgoing fill fetch stream",
                )
            })?;
        if subscription.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing fill fetch stream requires publisher role",
            ));
        }
        // SUBSCRIBE 処理直後 (Pending) の fill 開設を許す。確立前の Object 送出可否は
        // アプリが判断する。Terminated からの開設は認めない。
        if subscription.state == SubscriptionState::Terminated {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing fill fetch stream requires non-terminated subscription",
            ));
        }
        let subscription = self
            .subscriptions
            .get_mut(&subscription_request_id)
            .expect("subscription presence already checked above");
        let counts = &mut subscription.stream_counts;
        counts.published_count = counts
            .published_count
            .checked_add(1)
            .ok_or(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "published stream count overflow",
            ))?;
        self.data_streams.outgoing_fill.insert(
            stream_id,
            OutgoingFetchStream {
                request_id: subscription_request_id,
            },
        );
        Ok(())
    }

    /// 自端点が送る FETCH Object を Session に通知する (draft §11.4.1 (Fetch Header))
    ///
    /// 登録済み stream への送信であることだけ検証する。Object の Location は
    /// ペイロードと共にアプリが直接書き出す (Sans I/O のため Session は運ばない)。
    /// fill fetch stream (draft-ietf-moq-transport-21 §3.4) 上の Object も
    /// 本 API で通知する。
    pub fn send_fetch_object(&mut self, stream_id: DataStreamId) -> Result<(), SessionError> {
        self.require_established()?;
        let known = self.data_streams.outgoing_fetch.contains_key(&stream_id)
            || self.data_streams.outgoing_fill.contains_key(&stream_id);
        if !known {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing fetch object sent before fetch header",
            ));
        }
        Ok(())
    }

    /// 自端点が開いた FETCH data stream の終端を Session に通知する
    ///
    /// outgoing FETCH stream の索引から除去し、fetch エントリに「データストリーム終端済み」
    /// (`data_stream_finished`) を記録する。この記録により、publisher 側の fetch は
    /// `Established` のままでも `forget_fetch` で破棄可能になる
    /// (draft-ietf-moq-transport-21 §3.2.1 の "It can remove all FETCH state after closing
    /// the data stream with a FIN.")。FETCH の制御状態 (Pending / Established / Terminated)
    /// 自体は変更しない。stream の早期終了は §6.4.1 (Unidirectional Streams) により
    /// 制御状態には影響しない。
    ///
    /// FIN での正常終端だけでなく、RESET での終端も同じ API で通知する。
    /// draft-ietf-moq-transport-21 §3.2.1: "The Publisher can remove fetch state as soon as it
    /// has received a STOP_SENDING. It MUST reset the bidi request stream and unidirectional
    /// data stream associated with the FETCH." (STOP_SENDING 受信後) の reset 実行後に、
    /// この API を呼んで終端を通知する (終端種別は区別しない)。
    /// なお draft-ietf-moq-transport-21 §9.5.1: "When a REQUEST_UPDATE fails for a FETCH, the
    /// publisher MUST reset the FETCH data stream." (REQUEST_UPDATE 失敗応答後) の reset は
    /// `send_request_error` が Session から自動発行するため、本 API を呼ぶ必要はない
    /// (outgoing_fetch エントリの除去と `data_stream_finished` 記録も自動で行われる)。
    /// fill fetch stream (draft-ietf-moq-transport-21 §3.4) の終端通知も本 API で行う
    /// (fill stream には `Fetch` 状態がないため `data_stream_finished` 記録は行わない)。
    /// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn send_fetch_data_stream_closed(
        &mut self,
        stream_id: DataStreamId,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        // fill fetch stream の終端。対応する `Fetch` 状態は存在しないため除去のみで
        // 終端記録は不要。subscription 側の終端は FIN / RESET とは独立している
        // (draft-ietf-moq-transport-21 §3.4.1)。
        if self.data_streams.outgoing_fill.remove(&stream_id).is_some() {
            return Ok(());
        }
        let Some(stream) = self.data_streams.outgoing_fetch.remove(&stream_id) else {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing fetch stream close received for unknown stream id",
            ));
        };
        // 終端済み記録の根拠 (公開 doc の §3.2.1 引用に加えて):
        // RESET 後の REQUEST_UPDATE 失敗応答 fetch も破棄可能になるのは
        // draft-ietf-moq-transport-21 §3.2.1 の "A REQUEST_ERROR indicates that both
        // endpoints can immediately remove state." に適合する
        if let Some(fetch) = self.fetches.get_mut(&stream.request_id) {
            fetch.data_stream_finished = true;
        }
        Ok(())
    }

    /// 指定 subscription で現在 open している送信 fill fetch stream 数を返す (診断用)
    ///
    /// draft-ietf-moq-transport-21 §3.4 (Fill Semantics): 1 つの subscription に
    /// 複数本の fill fetch stream が同時に開くことがある (REQUEST_UPDATE ごとに新規開設し、
    /// 既存 stream を暗黙キャンセルしない)。通常は 0 以上で、上限は設けない。
    pub fn open_outgoing_fill_stream_count(&self, subscription_request_id: u64) -> usize {
        self.data_streams
            .outgoing_fill
            .values()
            .filter(|stream| stream.request_id == subscription_request_id)
            .count()
    }

    /// 指定 subscription に紐づく open 中の fill fetch stream をすべて reset する
    ///
    /// draft-ietf-moq-transport-21 §3.4.1 (Opening and Closing Fill Fetch Streams):
    /// "When the subscription is cancelled, the publisher MUST reset any open fill
    /// fetch streams." アプリが QUIC RESET_STREAM を送るための
    /// `SessionEvent::ResetDataStream` を stream ごとに発火し、索引から除去する。
    /// error code は §12.5 (Stream Reset Error Codes) の `CANCELLED` (0x1) を使う
    /// (いずれかの端点による cancel に相当)。
    /// 呼び出し側は subscription の Terminated 遷移と対にして呼ぶこと。
    pub(super) fn reset_open_fill_streams(&mut self, subscription_request_id: u64) {
        let mut stream_ids: Vec<DataStreamId> = Vec::new();
        self.data_streams
            .outgoing_fill
            .retain(|&stream_id, stream| {
                if stream.request_id == subscription_request_id {
                    stream_ids.push(stream_id);
                    false
                } else {
                    true
                }
            });
        for stream_id in stream_ids {
            self.events.push_back(SessionEvent::ResetDataStream {
                stream_id,
                error_code: STREAM_CANCELLED,
                // Session 自動発火のため RESET_STREAM_AT の自動計算はしない (別途検討)
                reliable_size: None,
            });
        }
    }

    /// 受信した FETCH stream の entry を Session に取り込む (draft §11.4.1 (Fetch Header))
    ///
    /// entry の内容自体は検証しない (codec 側で検証済みのため)。Session は次を行う。
    /// - FETCH_HEADER 受信済みの fetch stream であることの検証
    /// - DATA_STREAM_TIMEOUT 用の activity 記録の更新
    ///
    /// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) の条件 4 は
    /// "If the end of a Group is implicitly determined via a gap in a FETCH response, the final
    /// Object in the Group remains unknown." と述べるので、End of Range から Group 終端を
    /// 推論することはしない (`Subscription::ended_groups` (EndOfGroup / EndOfTrack 用の
    /// group 終端追跡) には登録しない)。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn recv_fetch_entry(&mut self, stream_id: DataStreamId) -> Result<(), SessionError> {
        self.require_established()?;
        match self.data_streams.incoming.get_mut(&stream_id) {
            Some(IncomingDataStream::Fetch { .. }) => {
                // draft-ietf-moq-transport-21 §6.6 (Termination): DATA_STREAM_TIMEOUT は
                // "an object header within a data stream" も activity に含む。
                // fetch entry (Object / EndOfRange いずれも) 受信ごとに timestamp を更新する。
                if let Some(now_ms) = self.timing.last_tick_ms {
                    self.timing
                        .data_stream_last_activity_ms
                        .insert(stream_id, now_ms);
                }
                Ok(())
            }
            Some(IncomingDataStream::AwaitingHeader { .. }) => Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "fetch entry received before fetch header",
            )),
            Some(IncomingDataStream::Subgroup { .. }) => Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "fetch entry received on subgroup stream",
            )),
            Some(IncomingDataStream::Discarded { .. }) => Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "fetch entry received on discarded stream",
            )),
            None => Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "fetch entry received for unknown stream id",
            )),
        }
    }

    /// 受信した FETCH_HEADER を Session に通知する
    ///
    /// セッション確立後のみ受け付ける (`require_established`)。
    /// FETCH request 自体は既に `send_fetch` で
    /// 登録済みであることを前提にし、pending / established のどちらでも受理する。
    /// `Fetch` 状態を先に引き、なければ fill fetch stream として subscription を引く。
    /// fill fetch stream の FETCH_HEADER は起因 SUBSCRIBE / REQUEST_UPDATE の
    /// Request ID (= subscription の Request ID) を載せ、自側 `Subscriber` 役の
    /// `Pending` / `Established` subscription に対して受理する。
    /// それ以外 (未知 ID / 非 subscriber 役 / `Terminated`) は
    /// PROTOCOL_VIOLATION でセッションを閉じる。
    /// fill は複数本の同時存在を許すため `has_other_fetch_stream` 検証は行わない。
    /// なお FETCH と fill の判別は Session が行うため、アプリは request id 種別を
    /// 自前で管理する必要はない。
    pub fn recv_fetch_header(
        &mut self,
        stream_id: DataStreamId,
        header: &FetchHeader,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        match self.data_streams.incoming.get(&stream_id) {
            Some(IncomingDataStream::AwaitingHeader {
                kind: DataStreamType::Fetch,
            }) => {}
            Some(IncomingDataStream::AwaitingHeader {
                kind: DataStreamType::Subgroup,
            }) => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "fetch header received on subgroup stream",
                ));
            }
            Some(IncomingDataStream::Fetch { .. }) => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "fetch header already received for stream id",
                ));
            }
            Some(IncomingDataStream::Subgroup { .. }) => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "fetch header received after subgroup header",
                ));
            }
            Some(IncomingDataStream::Discarded { .. }) => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "fetch header received on discarded stream",
                ));
            }
            Some(IncomingDataStream::AwaitingHeader {
                kind: DataStreamType::Padding,
            }) => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "fetch header received on padding stream",
                ));
            }
            None => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "fetch header received for unknown stream id",
                ));
            }
        }

        // FETCH 応答か fill fetch stream かを解決する。borrow を閉じてから
        // `fail` を呼ぶため、判定に必要な値はコピーして取り出す。
        let fetch_state = self
            .fetches
            .get(&header.request_id)
            .map(|fetch| (fetch.my_role, fetch.state));
        if let Some((role, state)) = fetch_state {
            if role != TrackRole::Subscriber {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "FETCH_HEADER received on publisher side",
                );
                self.fail(err.clone());
                return Err(err);
            }
            if state == FetchState::Terminated {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "FETCH_HEADER received for terminated fetch",
                );
                self.fail(err.clone());
                return Err(err);
            }
            if self.has_other_fetch_stream(stream_id, header.request_id) {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "concurrent FETCH data streams for the same request id",
                );
                self.fail(err.clone());
                return Err(err);
            }
        } else {
            // draft-ietf-moq-transport-21 §3.4 (Fill Semantics): fill fetch stream の
            // FETCH_HEADER は起因 SUBSCRIBE / REQUEST_UPDATE の Request ID
            // (= subscription の Request ID) を載せる。`Fetch` 状態を持たないため
            // subscription 側で受理する。複数本の同時存在を許すため
            // `has_other_fetch_stream` 検証は行わない (FIN / RESET 後の後始末は
            // `recv_data_stream_closed` / `send_data_stream_stop_sending` が
            // subscription に影響なく吸収する)。
            let fill_state = self
                .subscriptions
                .get(&header.request_id)
                .map(|subscription| (subscription.my_role, subscription.state));
            match fill_state {
                Some((TrackRole::Subscriber, SubscriptionState::Pending))
                | Some((TrackRole::Subscriber, SubscriptionState::Established)) => {}
                _ => {
                    let err = SessionError::new(
                        SESSION_PROTOCOL_VIOLATION,
                        "FETCH_HEADER received for unknown request id",
                    );
                    self.fail(err.clone());
                    return Err(err);
                }
            }
        }
        self.data_streams.incoming.insert(
            stream_id,
            IncomingDataStream::Fetch {
                request_id: header.request_id,
            },
        );
        if let Some(now_ms) = self.timing.last_tick_ms {
            self.timing
                .data_stream_last_activity_ms
                .insert(stream_id, now_ms);
        }
        Ok(())
    }

    /// 自端点が Object Datagram を送信することを Session に通知する
    ///
    /// publisher 側 subscription が確立済みであること、`track_alias` が割り当て済みであることを
    /// 検証する。ObjectDatagram のエンコードは呼び出し側が行う。
    ///
    /// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): サブスクリプション経由で
    /// Object が公開または受信されたときに最大位置を更新する。status は問わず更新対象となる。
    /// 節番号・規定は draft 由来であり将来の draft 改版で変更される可能性がある。
    /// draft-ietf-moq-transport-21 §11.2.1 (Object Datagram): Datagram では Properties Length = 0 はプロトコル違反。
    /// draft-ietf-moq-transport-21 §11.1.3 (Object Properties): 非 Normal status に Properties は不可。
    pub fn send_object_datagram(
        &mut self,
        request_id: u64,
        group_id: u64,
        object_id: u64,
        properties_data: Option<Vec<u8>>,
        status: Option<u64>,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        let subscription = self.subscriptions.get(&request_id).ok_or_else(|| {
            SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "subscription not found for outgoing object datagram",
            )
        })?;
        if subscription.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing object datagram requires publisher role",
            ));
        }
        if subscription.state != SubscriptionState::Established {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "outgoing object datagram requires Established subscription",
            ));
        }
        if subscription.track_alias.is_none() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "publisher subscription is missing track alias",
            ));
        }
        // draft-ietf-moq-transport-21 §11.2.1 (Object Datagram): Datagram では Properties Length = 0 はプロトコル違反
        if let Some(ref data) = properties_data
            && data.is_empty()
        {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "datagram properties data must not be empty when present",
            ));
        }
        // draft §3.3.3 (Combining Filters): Pass = Forward AND Location Filters AND Range Filters。
        // datagram は §11.2.1 のワイヤ構造に Subgroup ID フィールドを持たないので
        // SUBGROUP_FILTER は評価対象外 (`subgroup_id: None`)。Publisher Priority は
        // §10.4 の解決結果を使う。
        {
            let input = ObjectFilterInput {
                location: Location {
                    group_id,
                    object_id,
                },
                subgroup_id: None,
                publisher_priority: subscription.effective_publisher_priority(),
                properties_bytes: properties_data.as_deref(),
            };
            if !object_passes_filters(subscription, &input) {
                return Err(SessionError::new(
                    SESSION_LOCAL_FILTER_MISMATCH,
                    "outgoing object datagram does not pass subscription filters",
                ));
            }
        }
        // draft-ietf-moq-transport-21 §11.1.3 (Object Properties): 非 Normal status に Properties は不可
        if properties_data.is_some() && matches!(status, Some(s) if s != 0) {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "properties on non-Normal status object is not allowed",
            ));
        }
        // draft-ietf-moq-transport-21 §5.2 (Delivery Timeouts and Data Reliability):
        // "For datagrams, the implementation MUST drop the datagrams if the time elapsed
        // exceeds OBJECT_DELIVERY_TIMEOUT." 起点は object header の最終バイト。
        // "For objects with Object Forwarding Preference set to Datagram, the
        // SUBGROUP_DELIVERY_TIMEOUT acts the same way as OBJECT_DELIVERY_TIMEOUT;
        // if both are non-zero, the smaller of the two is used."
        {
            let subscription = self.subscriptions.get(&request_id).expect("checked above");
            let effective_timeout_ms = match (
                subscription.delivery_timeouts.effective_object_ms,
                subscription.delivery_timeouts.effective_subgroup_ms,
            ) {
                (Some(obj), Some(sub)) => Some(obj.min(sub)),
                (Some(obj), None) => Some(obj),
                (None, Some(sub)) => Some(sub),
                (None, None) => None,
            };
            if let Some(timeout_ms) = effective_timeout_ms {
                let key = (request_id, group_id, object_id);
                // object header 提供完了 tick は最初の tick で確定する (tick 未到達は None)
                // (sans-I/O の制約により、 `send_object_datagram` 呼び出し tick を
                // last header byte 提供時刻の近似とする)。
                // 未確定の間は経過時間を判定せず、drop もしない。
                // drop 時はエントリを削除しない (削除すると同じオブジェクトの再送が
                // 新しい object header 提供完了時刻として記録され、経過時間判定がリセットされるため)。
                // 件数は subscription 単位で上限
                // (`MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION`) を設け、超過時は
                // 最も古い Group から丸ごと破棄する。破棄範囲の再送は新規扱いで再計時される。
                // 通常時は全体件数 (O(1)) で早期判定し、上限到達後のみ該当 request を走査する。
                if !self.timing.datagram_header_complete_ms.contains_key(&key) {
                    if self.timing.datagram_header_complete_ms.len()
                        >= MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION
                        && self
                            .timing
                            .datagram_header_complete_ms
                            .keys()
                            .filter(|&&(rid, _, _)| rid == request_id)
                            .count()
                            >= MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION
                        && let Some(oldest_group) = self
                            .timing
                            .datagram_header_complete_ms
                            .keys()
                            .filter(|&&(rid, _, _)| rid == request_id)
                            .map(|&(_, g, _)| g)
                            .min()
                    {
                        self.timing
                            .datagram_header_complete_ms
                            .retain(|&(rid, g, _), _| !(rid == request_id && g == oldest_group));
                        self.timing
                            .datagram_pending_ms
                            .retain(|&(rid, g, _)| !(rid == request_id && g == oldest_group));
                    }
                    let is_pending = self.timing.last_tick_ms.is_none();
                    self.timing
                        .datagram_header_complete_ms
                        .insert(key, self.timing.last_tick_ms);
                    if is_pending {
                        self.timing.datagram_pending_ms.insert(key);
                    }
                }
                let header_complete_ms = self
                    .timing
                    .datagram_header_complete_ms
                    .get(&key)
                    .copied()
                    .flatten();
                if let Some(header_complete_ms) = header_complete_ms {
                    let now_ms = self.timing.last_tick_ms.unwrap_or(0);
                    if now_ms.saturating_sub(header_complete_ms) >= timeout_ms {
                        return Err(SessionError::new(
                            SESSION_LOCAL_DATAGRAM_TIMEOUT,
                            "datagram delivery timeout exceeded, dropping datagram",
                        ));
                    }
                }
            }
        }
        // 検証済みの publisher subscription の最大位置を更新する。
        // draft-ietf-moq-transport-21 §3.3.1 (Location Filters) に従い、
        // サブスクリプション経由で公開または受信された Object で最大位置を更新し、
        // status は問わず対象とする。
        // 節番号・規定は draft 由来であり将来の draft 改版で変更される可能性がある。
        let published_location = Location {
            group_id,
            object_id,
        };
        if let Some(subscription) = self.subscriptions.get_mut(&request_id) {
            subscription.record_largest_received_location(published_location);
        }
        Ok(())
    }

    /// 受信 datagram を type で分岐して Session に取り込む (draft §11 (Data Streams and Datagrams))
    ///
    /// datagram 受信の共通入口。I/O 層は受信した datagram をそのまま渡すだけでよい。
    ///
    /// §11 は "All MOQT datagrams start with a variable-length integer indicating the type
    /// of the datagram." と "An endpoint that receives an unknown datagram type MUST close
    /// the session." を規定する。エラーコードは §11 では指定されていないため、§11.2.1 が
    /// 不正な Object Datagram Type 値に対して規定する PROTOCOL_VIOLATION に準拠する。
    ///
    /// Sans I/O の Session が raw bytes を受け取るのは、先頭 varint による type 分岐が
    /// プロトコル状態の判定にあたるためである。分岐後の処理は
    /// [`recv_object_datagram`](Self::recv_object_datagram) /
    /// [`recv_padding_datagram`](Self::recv_padding_datagram) に委ねる。
    ///
    /// `raw` は厳密に 1 つの datagram 全体でなければならない
    /// ([`ObjectDatagram::decode`] が残りバイト数を payload 長として検証するため)。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn recv_datagram(&mut self, raw: &[u8]) -> Result<DatagramAcceptance, SessionError> {
        self.require_established()?;
        let Ok((type_id, _)) = varint::decode(raw) else {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "datagram type varint could not be decoded",
            );
            self.fail(err.clone());
            return Err(err);
        };
        // draft §11.5.2 (Padding Datagrams): 既知 type。ペイロードは無条件に破棄する
        if type_id == PADDING_DATAGRAM_TYPE {
            self.recv_padding_datagram()?;
            return Ok(DatagramAcceptance::Padding);
        }
        // draft §11.2.1 (Object Datagram): type 値の妥当性だけを先に判定し、
        // unknown type とヘッダ本体の不正を別の reason で区別する
        if validate_object_datagram_type(type_id).is_err() {
            let err = SessionError::new(SESSION_PROTOCOL_VIOLATION, "unknown datagram type");
            self.fail(err.clone());
            return Err(err);
        }
        let Ok((datagram, _)) = ObjectDatagram::decode(raw) else {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "malformed OBJECT_DATAGRAM header",
            );
            self.fail(err.clone());
            return Err(err);
        };
        self.recv_object_datagram(&datagram)
            .map(DatagramAcceptance::Object)
    }

    /// 受信 datagram を Session に通知する
    ///
    /// unknown Track Alias は draft §11.2 (Datagrams) に従い session close にせず、
    /// `UnknownTrackAlias` を返す。
    ///
    /// type 分岐を含む共通入口は [`recv_datagram`](Self::recv_datagram)。unknown datagram
    /// type に対する §11 の MUST (セッションクローズ) はそちらで実行される。
    pub fn recv_object_datagram(
        &mut self,
        datagram: &ObjectDatagram,
    ) -> Result<TrackDataAcceptance, SessionError> {
        self.require_established()?;
        // draft §3.1 (Subscriptions): 共有 alias の候補をフィルタ再適用で振り分ける
        let candidates = self.resolve_peer_track_alias(datagram.track_alias)?;
        if candidates.is_empty() {
            // draft §3.1.2 (Track Alias): キャンセル直後の遅延 Object は未知 alias ではなく
            // 「不要 Object」として破棄させる
            return Ok(if self.peer_alias_is_tombstoned(datagram.track_alias) {
                TrackDataAcceptance::Discarded
            } else {
                TrackDataAcceptance::UnknownTrackAlias
            });
        }
        // Object Properties の生バイト列を構築する (Properties Length varint + データ)
        let properties_bytes = datagram.properties_data.as_ref().map(|properties_data| {
            let mut encoded = Vec::new();
            varint::encode(properties_data.len() as u64, &mut encoded);
            encoded.extend_from_slice(properties_data);
            encoded
        });
        // draft §3.1 (Subscriptions): "the subscriber re-applies each subscription's filter
        // to determine which subscription a received Object belongs to."
        // 候補を順に評価し、最初に通過した subscription に紐づける。
        // publisher は matching subscription ごとに Object を 1 回ずつ送るため、
        // 1 つの datagram は 1 つの subscription に対応するのが本来の姿である。
        // SUBGROUP_FILTER は §11.2.1 のワイヤ構造に Subgroup ID が無いので評価対象外。
        // キャンセル由来候補のスキップ方式は `recv_subgroup_header` の候補評価ループと
        // 同じ (datagram は stream を持たないため request_id の保持は不要)。
        let mut matched_request_id = None;
        let mut cancelled_matched = false;
        for &candidate_id in &candidates {
            if let Some(subscription) = self.subscriptions.get(&candidate_id) {
                let publisher_priority = datagram
                    .publisher_priority
                    .unwrap_or_else(|| subscription.effective_publisher_priority());
                let input = ObjectFilterInput {
                    location: Location {
                        group_id: datagram.group_id,
                        object_id: datagram.object_id,
                    },
                    subgroup_id: None,
                    publisher_priority,
                    properties_bytes: properties_bytes.as_deref(),
                };
                if object_passes_filters(subscription, &input) {
                    if is_cancelled_terminated(subscription) {
                        cancelled_matched = true;
                    } else {
                        matched_request_id = Some(candidate_id);
                        break;
                    }
                }
            }
        }
        let Some(request_id) = matched_request_id else {
            // 合格した候補がキャンセル由来のみの場合は不要 Object として破棄する
            // (datagram は stream を持たないため登録対象はない)
            if cancelled_matched {
                return Ok(TrackDataAcceptance::Discarded);
            }
            return Ok(TrackDataAcceptance::FilteredOut);
        };
        let received_location = Location {
            group_id: datagram.group_id,
            object_id: datagram.object_id,
        };
        if let Some(subscription) = self.subscriptions.get_mut(&request_id) {
            subscription.record_largest_received_location(received_location);
        }
        // draft §11.1.2 (Object Status) / §12.1 (Malformed Tracks): 終端宣言後の Object は
        // Malformed Track にあたる
        if let Some(reason) = self.object_after_track_end(
            request_id,
            &Location {
                group_id: datagram.group_id,
                object_id: datagram.object_id,
            },
        ) {
            let err = SessionError::new(SESSION_PROTOCOL_VIOLATION, reason);
            self.terminate_malformed_track(request_id, None, reason);
            return Err(err);
        }
        // draft §11.1.2 (Object Status): Object Status 由来の終端宣言を記録する
        self.record_object_status_end(
            request_id,
            datagram.group_id,
            datagram.object_id,
            datagram.status,
        );
        // draft §11.2.1 (Object Datagram): END_OF_GROUP bit は「この位置より大きい Object ID が
        // 存在しない」ことを宣言する
        if datagram.end_of_group {
            self.record_group_end_after(request_id, datagram.group_id, datagram.object_id);
        }
        if let Err(msg_err) = self
            .peer_object_properties
            .entry(request_id)
            .or_default()
            .observe_object(
                datagram.group_id,
                datagram.object_id,
                properties_bytes.as_deref(),
            )
        {
            // draft §12.1 (Malformed Tracks): datagram 経路も同じ扱い。datagram は stream を
            // 持たないので reset 対象の stream_id は無い。
            let err = session_error_from_data_message(msg_err);
            self.terminate_malformed_track(request_id, None, err.reason);
            return Err(err);
        }
        // draft §12.1 (Malformed Tracks) 条件 6/7, §7.1 (Caching Relays):
        // 重複 Object の Forwarding Preference / Subgroup ID / Priority 一貫性を検証する。
        // datagram 経由なので is_subgroup = false、subgroup_id = None。
        {
            let publisher_priority = datagram.publisher_priority.unwrap_or_else(|| {
                self.subscriptions
                    .get(&request_id)
                    .map_or(PUBLISHER_PRIORITY_DEFAULT, |s| {
                        s.effective_publisher_priority()
                    })
            });
            if let Err(mismatch) = self
                .peer_object_fields
                .entry(request_id)
                .or_default()
                .observe_object_fields(
                    datagram.group_id,
                    datagram.object_id,
                    false,
                    None,
                    publisher_priority,
                )
            {
                self.terminate_malformed_track(request_id, None, mismatch.reason);
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    mismatch.reason,
                ));
            }
        }
        Ok(TrackDataAcceptance::Accepted)
    }

    /// 終端宣言済みの範囲に入る Object かどうかを判定する
    ///
    /// draft-ietf-moq-transport-21 §11.1.2 (Object Status) の End of Group / End of Track と
    /// §11.3.1 (Subgroup Header) / §11.2.1 (Object Datagram) の END_OF_GROUP bit で宣言された
    /// 終端位置を参照する。終端後の Object は §12.1 (Malformed Tracks) の条件 4 / 5 に該当する。
    ///
    /// 該当する場合は理由文字列を返す。
    pub(super) fn object_after_track_end(
        &self,
        request_id: u64,
        location: &Location,
    ) -> Option<&'static str> {
        let subscription = self.subscriptions.get(&request_id)?;
        // §11.1.2: End of Track は "location that is equal to or greater than the one
        // specified" が存在しないことを宣言する
        if let Some(end) = subscription.end_of_track.as_ref()
            && location >= end
        {
            return Some("object received after End of Track");
        }
        // Group 終端は「存在しない最小の Object ID」として正規化済み
        if let Some(&end_object_id) = subscription.ended_groups.get(&location.group_id)
            && location.object_id >= end_object_id
        {
            return Some("object received after End of Group");
        }
        None
    }

    /// Object Status から End of Group / End of Track を状態へ記録する
    ///
    /// draft-ietf-moq-transport-21 §11.1.2 (Object Status):
    /// - 0x3 (End of Group): "no objects with the specified Group ID and the Object ID that is
    ///   greater than or equal to the one specified exist" → その位置自身が存在しない最小 ID
    /// - 0x4 (End of Track): "no objects with the location that is equal to or greater than the
    ///   one specified exist" → その位置自身が存在しない最小 Location
    pub(super) fn record_object_status_end(
        &mut self,
        request_id: u64,
        group_id: u64,
        object_id: u64,
        status: Option<u64>,
    ) {
        let Some(status) = status else { return };
        let Some(subscription) = self.subscriptions.get_mut(&request_id) else {
            return;
        };
        match status {
            OBJECT_STATUS_END_OF_GROUP => {
                // 既にもっと手前で終端宣言されている場合は狭い方 (小さい方) を残す
                let entry = subscription
                    .ended_groups
                    .entry(group_id)
                    .or_insert(object_id);
                *entry = (*entry).min(object_id);
            }
            OBJECT_STATUS_END_OF_TRACK => {
                let location = Location {
                    group_id,
                    object_id,
                };
                subscription.end_of_track = Some(match subscription.end_of_track.take() {
                    Some(existing) => existing.min(location),
                    None => location,
                });
            }
            _ => {}
        }
    }

    /// END_OF_GROUP bit が立った Group の終端を「存在しない最小 Object ID」で記録する
    ///
    /// draft-ietf-moq-transport-21 §11.2.1 (Object Datagram) の END_OF_GROUP bit は
    /// "no Object with the same Group ID and an Object ID greater than the Object ID in this
    /// datagram exists" なので、宣言位置の 1 つ後ろが存在しない最小 ID になる。
    /// §11.3.1 (Subgroup Header) の END_OF_GROUP bit + FIN も同じ表現に正規化する
    /// (§12.1 の "the last Object before a FIN in a Subgroup which has the END_OF_GROUP bit set")。
    pub(super) fn record_group_end_after(
        &mut self,
        request_id: u64,
        group_id: u64,
        last_object_id: u64,
    ) {
        let Some(subscription) = self.subscriptions.get_mut(&request_id) else {
            return;
        };
        let end = last_object_id.saturating_add(1);
        let entry = subscription.ended_groups.entry(group_id).or_insert(end);
        *entry = (*entry).min(end);
    }

    /// Malformed Track を検出した request を終端する (draft §12.1 (Malformed Tracks))
    ///
    /// §12.1: "When a subscriber detects a Malformed Track, it MUST cancel any corresponding
    /// subscription or fetches for that Track from that publisher (see Section 3.3.3), and
    /// SHOULD deliver an error to the application."
    ///
    /// **セッション全体は閉じない。** 該当 request だけを Terminated にし、他の request は
    /// 生き残る。§12.1 は subscription / fetch 単位の cancel を要求しており、session error を
    /// 要求していない。
    ///
    /// 行うこと:
    /// - 該当 subscription / fetch を `Terminated` へ遷移させる
    /// - `stream_id` が与えられていれば、その data stream を
    ///   [`STREAM_MALFORMED_TRACK`] で `ResetDataStream` する
    ///   (§12.1 の "reset any fetch streams with Status Code MALFORMED_TRACK" に対応)
    /// - `RequestTerminated { reason: MalformedTrack }` を発行してアプリへエラーを届ける
    ///   (§12.1 の SHOULD)
    ///
    /// 既に `Terminated` のエントリには `RequestTerminated` を発行しない (二重発行の抑止)。
    /// PUBLISH_DONE 受信済み (`publish_done` が `Some`) の subscription は `publish_done` を
    /// `None` にリセットして drain モードを解除し、以後のデータを破棄対象にする
    /// (draft §12.1 の MUST cancel に従い、malformed 検出後のデータは破棄する)。
    /// このとき `stream_id` が与えられていれば `ResetDataStream` は発行する
    /// (draft §12.1 の reset 要求を維持。抑止するのは `RequestTerminated` の発行のみ)。
    /// `publish_done` が `None` (キャンセル由来) の `Terminated` は早期 return で何もしない。
    ///
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub(super) fn terminate_malformed_track(
        &mut self,
        request_id: u64,
        stream_id: Option<DataStreamId>,
        reason: &'static str,
    ) {
        let kind = if let Some(subscription) = self.subscriptions.get_mut(&request_id) {
            if subscription.state == SubscriptionState::Terminated {
                // 二重発行の抑止。キャンセル由来 (`publish_done` が `None`) は何もしない
                if is_cancelled_terminated(subscription) {
                    return;
                }
                // PUBLISH_DONE 受信済み: drain モードを解除し、以後のデータを破棄対象にする。
                // `RequestTerminated` は発行せず、`ResetDataStream` のみ発行する
                subscription.publish_done = None;
                if let Some(stream_id) = stream_id {
                    self.events.push_back(SessionEvent::ResetDataStream {
                        stream_id,
                        error_code: STREAM_MALFORMED_TRACK,
                        // malformed 検出時は header の完全性が保証できないため RESET_STREAM とする
                        reliable_size: None,
                    });
                }
                return;
            }
            subscription.state = SubscriptionState::Terminated;
            // subscription は SUBSCRIBE 由来と PUBLISH 由来がある。request_streams の
            // 記録が正しい種別を持つのでそれを優先する。
            self.request_streams
                .get(&request_id)
                .copied()
                .unwrap_or(RequestKind::Subscribe)
        } else if let Some(fetch) = self.fetches.get_mut(&request_id) {
            // 既に `Terminated` の fetch には二重発行しない。
            // 現時点の呼び出し経路はすべて subscription 由来であり到達不能だが、
            // 将来 fetch 経路の malformed 検出が追加された場合に備える防御コードである
            if fetch.state == FetchState::Terminated {
                return;
            }
            fetch.state = FetchState::Terminated;
            RequestKind::Fetch
        } else {
            // 対象が既に回収されている場合は通知だけ行わない (二重終端を避ける)
            return;
        };
        self.clear_control_message_deadline(request_id);
        if let Some(stream_id) = stream_id {
            // draft §12.1 (Malformed Tracks): Malformed Track を運んだ data stream は
            // MALFORMED_TRACK で reset する
            self.events.push_back(SessionEvent::ResetDataStream {
                stream_id,
                error_code: STREAM_MALFORMED_TRACK,
                // malformed 検出時は header の完全性が保証できないため RESET_STREAM とする
                reliable_size: None,
            });
        }
        self.events.push_back(SessionEvent::RequestTerminated {
            request_id,
            kind,
            reason: TerminationReason::MalformedTrack { reason },
        });
    }

    /// 受信したパディングデータグラムを Session に通知する (draft §11.5.2 (Padding Datagrams))
    ///
    /// §11.5.2 は "The receiver MUST discard all data received in a padding datagram."
    /// と無条件の破棄を要求する。ペイロードの内容による分岐は無いので、本 API は
    /// ペイロードを受け取らず、状態も追跡せずに受諾する。
    ///
    /// 送信側には全バイト 0x00 の MUST があるが (§11.5.2)、**非 0 バイトを受信した場合の
    /// 受信側の動作を draft は規定していない**。破棄が MUST である以上、非 0 を
    /// protocol error として扱う根拠は無いため、無条件破棄とする。したがって I/O 層も
    /// ペイロードの検査は不要であり、Session へ渡す必要もない。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn recv_padding_datagram(&mut self) -> Result<(), SessionError> {
        self.require_established()?;
        Ok(())
    }

    /// シリアライズ途中の Object で stream が graceful 終了したことを報告する
    /// (draft §11.3 (Subgroup Streams))
    ///
    /// §11.3: "If a stream ends gracefully (i.e., the stream terminates with a FIN) in the
    /// middle of a serialized Object, the session SHOULD be closed with a PROTOCOL_VIOLATION."
    ///
    /// Sans I/O の Session は decoder を持たない (`SubgroupStreamDecoder` /
    /// `FetchStreamDecoder` はアプリケーションが Session の外で保持する) ため、
    /// decoder の `finish()` が `UnexpectedEof` を返したことを Session が自力で知れない。
    /// アプリケーションがそれを検出したときに本 API を呼ぶ。
    ///
    /// §11.3 は SHOULD なので、本 API を **呼ぶこと自体が** セッションを閉じる判断である。
    /// 呼び出すと `PROTOCOL_VIOLATION` で session fail し `Err` を返す。
    ///
    /// [`recv_data_stream_closed`](Self::recv_data_stream_closed) との呼び出し順序は問わない。
    /// stream が既に登録解除されていても閉じる (順序で挙動が変わると、アプリケーションが
    /// FIN 通知と decoder 検査のどちらを先に行うかで結果が変わってしまう)。
    ///
    /// RESET による途中終了は §11.3.2 の別経路であり本 API の対象ではない。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn report_mid_object_fin(&mut self, stream_id: DataStreamId) -> Result<(), SessionError> {
        self.require_established()?;
        // 破棄対象 stream での mid-object FIN は no-op で受理する
        // (アプリが decoder を回して mid-object FIN を検出した場合でも、
        // 破棄対象 stream でセッションを fail させない)
        let removed = self.data_streams.incoming.remove(&stream_id);
        self.timing.data_stream_last_activity_ms.remove(&stream_id);
        // 破棄対象: Discarded variant、またはキャンセル由来 Terminated に属する
        // 既存 stream (Subgroup variant)。保持集合に含まれる id も対象
        let discarded = match &removed {
            Some(IncomingDataStream::Discarded { .. }) => true,
            Some(IncomingDataStream::Subgroup { request_id, .. }) => {
                self.is_cancelled_terminated_subscription(*request_id)
            }
            _ => false,
        };
        if discarded {
            // 除去した id は保持集合へ移し、以後の終端通知 (FIN / RESET_STREAM) も
            // no-op で吸収する (recv_data_stream_closed との呼び出し順序に依存させない)
            self.retain_discarded_stream_id(stream_id);
            return Ok(());
        }
        if self.data_streams.discarded.contains_key(&stream_id) {
            return Ok(());
        }
        let err = SessionError::new(
            SESSION_PROTOCOL_VIOLATION,
            "data stream finished in the middle of a serialized object",
        );
        self.fail(err.clone());
        Err(err)
    }

    /// 受信 uni data stream の終端を Session に通知する
    ///
    /// subgroup stream は subgroup tracker に反映し、fetch stream は既存の
    /// `recv_fetch_data_stream_closed` に dispatch する。header 未受理のまま終わった
    /// stream は単に破棄する。
    ///
    /// 本 API は Object 境界での終端を前提とする。シリアライズ途中の Object で FIN した
    /// 場合は draft §11.3 によりセッションを閉じるべきなので、アプリケーションは
    /// decoder の `finish()` 失敗を検出して
    /// [`report_mid_object_fin`](Self::report_mid_object_fin) を呼ぶ。Session は decoder を
    /// 持たないため、本 API 側でその判定はできない。
    pub fn recv_data_stream_closed(
        &mut self,
        stream_id: DataStreamId,
        end: RequestStreamEnd,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        let Some(stream) = self.data_streams.incoming.remove(&stream_id) else {
            // 破棄対象の保持集合に含まれる stream id の終端は no-op で吸収する
            // (STOP_SENDING 送信後に届く peer の FIN / RESET_STREAM など。保持期間中は
            // 吸収し続け、掃除は tick_discarded_data_stream_ids が行う)
            if self.data_streams.discarded.contains_key(&stream_id) {
                return Ok(());
            }
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "data stream close received for unknown stream id",
            ));
        };
        self.timing.data_stream_last_activity_ms.remove(&stream_id);
        match stream {
            IncomingDataStream::AwaitingHeader { .. } => Ok(()),
            IncomingDataStream::Subgroup {
                request_id,
                track_alias,
                group_id,
                subgroup_id,
                end_of_group,
                last_object_id,
                ..
            } => {
                // キャンセル由来 Terminated に属する既存 stream の終端は破棄対象として
                // no-op で吸収する (tracker を汚染しない)。id は保持集合へ移して
                // 以後の受信・終端も吸収し続ける (draft §3.1.2)
                if self.is_cancelled_terminated_subscription(request_id) {
                    self.retain_discarded_stream_id(stream_id);
                    return Ok(());
                }
                // draft §11.3.1 (Subgroup Header) / §12.1 (Malformed Tracks):
                // END_OF_GROUP bit が立った subgroup stream が FIN で終わったら、その stream の
                // 最終 Object が Group の最終 Object になる。以降その Group に来る大きい
                // Object ID は Malformed Track として扱う。
                if end_of_group && matches!(end, RequestStreamEnd::Fin) {
                    if let Some(last_object_id) = last_object_id {
                        self.record_group_end_after(request_id, group_id, last_object_id);
                    } else {
                        // END_OF_GROUP 空 Subgroup（オブジェクト 0 個）の FIN:
                        // draft §11.3.1 (Subgroup Header) の END_OF_GROUP bit
                        // ("indicates that this subgroup contains the largest Object in
                        // the Group") をオブジェクト 0 個に適用すると、この Group には
                        // 配達すべきオブジェクトが存在しないことを宣言したことになる。
                        // 「存在しない最小の Object ID = 0」として終端を記録し、後から
                        // この Group に届くオブジェクトは Malformed Track として検出する
                        // （draft §12.1 (Malformed Tracks)）。
                        // `record_group_end_after` は last_object_id + 1 を記録するため
                        // 0 を渡すと 1 になり「Object ID 0 すら存在しない」という空 Group
                        // の意味論を壊す。ここでは 0 を直接記録する。
                        if let Some(subscription) = self.subscriptions.get_mut(&request_id) {
                            subscription.ended_groups.insert(group_id, 0);
                        }
                    }
                }
                if let Some(subgroup_id) = subgroup_id {
                    // per-subgroup delivery timeout オーバーライドのエントリを削除する
                    if let Some(subscription) = self.subscriptions.get_mut(&request_id) {
                        subscription
                            .delivery_timeouts
                            .subgroup_overrides
                            .remove(&(group_id, subgroup_id));
                    }
                    match end {
                        RequestStreamEnd::Fin => {
                            // draft §12.1 (Malformed Tracks) 条件 3: 同一 Subgroup が複数
                            // stream で FIN され最終 Object が異なる場合を検出する
                            if let Err(err) = self.peer_subgroups.mark_fin(
                                track_alias,
                                group_id,
                                subgroup_id,
                                last_object_id,
                            ) {
                                self.terminate_malformed_track(request_id, None, err.reason);
                                return Err(err);
                            }
                        }
                        RequestStreamEnd::Reset { reliable_size, .. } => {
                            self.peer_subgroups.mark_reset(
                                track_alias,
                                group_id,
                                subgroup_id,
                                reliable_size,
                            );
                        }
                    }
                }
                if let Err(err) = self.note_incoming_subgroup_stream_closed(request_id) {
                    self.fail(err.clone());
                    return Err(err);
                }
                Ok(())
            }
            IncomingDataStream::Discarded { .. } => {
                // 破棄対象 stream の終端は no-op で吸収し、id を保持集合へ移す
                self.retain_discarded_stream_id(stream_id);
                Ok(())
            }
            IncomingDataStream::Fetch { request_id, .. } => {
                if self
                    .fetches
                    .get(&request_id)
                    .is_none_or(|fetch| fetch.state == FetchState::Terminated)
                {
                    return Ok(());
                }
                self.recv_fetch_data_stream_closed(request_id, end)
            }
        }
    }

    /// 受信 uni data stream に対して local endpoint が STOP_SENDING を送ることを通知する
    ///
    /// fetch stream は既存の `send_fetch_stop_sending` に dispatch し、subgroup stream は
    /// reopen prohibited の追跡用に `StoppedByPeer` として記録する。
    /// draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) /
    /// draft-ietf-moq-transport-21 Appendix A.3 (Since draft-ietf-moq-transport-17) #1583: REQUEST_UPDATE で Forward State が
    /// 0→1 に変わった場合のみ再オープン MAY。
    pub fn send_data_stream_stop_sending(
        &mut self,
        stream_id: DataStreamId,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        let Some(stream) = self.data_streams.incoming.remove(&stream_id) else {
            // 破棄対象の保持集合に含まれる stream id への STOP_SENDING は no-op で吸収する
            // (保持期間中は吸収し続け、掃除は tick_discarded_data_stream_ids が行う)
            if self.data_streams.discarded.contains_key(&stream_id) {
                return Ok(());
            }
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "STOP_SENDING sent for unknown data stream id",
            ));
        };
        self.timing.data_stream_last_activity_ms.remove(&stream_id);
        match stream {
            IncomingDataStream::AwaitingHeader { .. } => Ok(()),
            IncomingDataStream::Subgroup {
                request_id,
                track_alias,
                group_id,
                subgroup_id,
                ..
            } => {
                // キャンセル由来 Terminated に属する既存 stream への STOP_SENDING は
                // 破棄対象として no-op で吸収する (tracker を汚染しない)。id は保持集合へ
                // 移して以後の受信・終端も吸収し続ける (draft §11.1)
                if self.is_cancelled_terminated_subscription(request_id) {
                    self.retain_discarded_stream_id(stream_id);
                    return Ok(());
                }
                if let Some(subgroup_id) = subgroup_id {
                    // per-subgroup delivery timeout オーバーライドのエントリを削除する
                    if let Some(subscription) = self.subscriptions.get_mut(&request_id) {
                        subscription
                            .delivery_timeouts
                            .subgroup_overrides
                            .remove(&(group_id, subgroup_id));
                    }
                    self.peer_subgroups
                        .mark_stop_sending(track_alias, group_id, subgroup_id);
                }
                if let Err(err) = self.note_incoming_subgroup_stream_closed(request_id) {
                    self.fail(err.clone());
                    return Err(err);
                }
                Ok(())
            }
            IncomingDataStream::Discarded { .. } => {
                // 破棄対象 stream への STOP_SENDING は no-op で吸収し、id を保持集合へ移す
                self.retain_discarded_stream_id(stream_id);
                Ok(())
            }
            IncomingDataStream::Fetch { request_id, .. } => {
                if self
                    .fetches
                    .get(&request_id)
                    .is_none_or(|fetch| fetch.state == FetchState::Terminated)
                {
                    return Ok(());
                }
                self.send_fetch_stop_sending(request_id)
            }
        }
    }

    pub(super) fn remove_incoming_data_streams_for_request(&mut self, request_id: u64) {
        let mut removed_ids = Vec::new();
        let mut removed_fetch_ids = Vec::new();
        self.data_streams
            .incoming
            .retain(|stream_id, stream| match stream {
                IncomingDataStream::AwaitingHeader { .. } => true,
                IncomingDataStream::Subgroup {
                    request_id: stream_request_id,
                    ..
                }
                | IncomingDataStream::Discarded {
                    request_id: Some(stream_request_id),
                } => {
                    if *stream_request_id == request_id {
                        removed_ids.push(*stream_id);
                        false
                    } else {
                        true
                    }
                }
                IncomingDataStream::Discarded { request_id: None } => true,
                IncomingDataStream::Fetch {
                    request_id: stream_request_id,
                    ..
                } => {
                    if *stream_request_id == request_id {
                        removed_fetch_ids.push(*stream_id);
                        false
                    } else {
                        true
                    }
                }
            });
        for id in removed_fetch_ids {
            self.timing.data_stream_last_activity_ms.remove(&id);
            // fetch stream は破棄対象の保持集合へは移さない
            // (fetch は 1 request 1 stream で共有 alias がなく、Terminated 後の終端は
            // 既存の no-op 吸収が担う。保持集合は subscription の破棄対象 stream 専用)
        }
        for id in removed_ids {
            self.timing.data_stream_last_activity_ms.remove(&id);
            // アプリは open 中 stream が残ったまま `forget_subscription` を呼びうる
            // (キャンセル由来 `Terminated` は `cleanup_ready()` が即 true)。
            // 除去した破棄対象 stream id は保持集合へ移し、以後の受信・終端を
            // no-op で吸収し続ける (draft §11.1)
            self.retain_discarded_stream_id(id);
        }
        // per-subgroup delivery timeout オーバーライドを全クリアする
        if let Some(subscription) = self.subscriptions.get_mut(&request_id) {
            subscription.delivery_timeouts.subgroup_overrides.clear();
        }
        self.peer_object_properties.remove(&request_id);
        self.peer_object_fields.remove(&request_id);
    }

    /// 指定 request の outgoing subgroup stream と datagram の追跡エントリを掃除する
    ///
    /// `forget_subscription` からのみ呼ばれる subscription 用の掃除。
    /// outgoing FETCH stream は対象外で、fetch 用の掃除は `forget_fetch`
    /// (`src/session/fetch.rs`) と終端通知 API (`send_fetch_data_stream_closed`) が行う。
    pub(super) fn remove_outgoing_data_plane_for_request(&mut self, request_id: u64) {
        let mut removed_ids: Vec<DataStreamId> = Vec::new();
        self.data_streams.outgoing.retain(|&stream_id, stream| {
            if stream.request_id == request_id {
                removed_ids.push(stream_id);
                false
            } else {
                true
            }
        });
        // fill fetch stream は subscription に direct に紐づくため、fetch 用の掃除
        // (`forget_fetch`) では除去されない。subscription の掃除でまとめて除去する。
        // 残骸が残るのはアプリが終端通知漏れ・ reset 漏れをした場合のみであり、
        // イベント発行なしの silent 除去でよい (state は Terminated 確定済み)。
        // fill stream は delivery timeout の時刻追跡 (`object_delivery_header_complete_ms` 等)
        // を使わないため、subgroup 側のような timing 掃除は不要である。
        self.data_streams
            .outgoing_fill
            .retain(|_, stream| stream.request_id != request_id);
        for id in removed_ids {
            self.timing
                .object_delivery_header_complete_ms
                .retain(|&(sid, _), _| sid != id);
            self.timing.subgroup_delivery_fin_ms.remove(&id);
        }
        // datagram の object header 提供完了追跡エントリも掃除する
        // (datagram 送信は subscription 経由のみで、fetch の request_id では
        // エントリは作られないため、request_id 単位の削除で過不足ない)
        self.timing
            .datagram_header_complete_ms
            .retain(|&(rid, _, _), _| rid != request_id);
        self.timing
            .datagram_pending_ms
            .retain(|&(rid, _, _)| rid != request_id);
    }

    pub(super) fn has_open_outgoing_data_streams_for_request(&self, request_id: u64) -> bool {
        // draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE): "A sender MUST NOT send
        // PUBLISH_DONE until it has closed all streams it will ever open" の対象に
        // fill fetch stream (§3.4) も含まれるため `outgoing_fill` も見る。
        // `outgoing_fetch` は別 request 種別 (FETCH) のため対象外のまま据え置く。
        self.data_streams
            .outgoing
            .values()
            .any(|stream| stream.request_id == request_id)
            || self
                .data_streams
                .outgoing_fill
                .values()
                .any(|stream| stream.request_id == request_id)
    }

    fn note_incoming_subgroup_stream_opened(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let subscription = self.subscriptions.get_mut(&request_id).ok_or_else(|| {
            SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "incoming subgroup stream points to missing subscription",
            )
        })?;
        let counts = &mut subscription.stream_counts;
        counts.incoming_subgroup_count =
            counts
                .incoming_subgroup_count
                .checked_add(1)
                .ok_or(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "incoming subgroup stream count overflow",
                ))?;
        counts.open_incoming_subgroup_count = counts
            .open_incoming_subgroup_count
            .checked_add(1)
            .ok_or(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "open incoming subgroup stream count overflow",
            ))?;
        let incoming_subgroup_count = counts.incoming_subgroup_count;
        if let Some(publish_done) = subscription.publish_done.as_mut()
            // draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE):
            // sentinel (`PUBLISH_DONE_STREAM_COUNT_UNKNOWN`) を受信した場合、publisher は
            // exact 数を表明していないので overrun の比較対象が存在しない。フラグは false のまま据え置く。
            && publish_done.stream_count != PUBLISH_DONE_STREAM_COUNT_UNKNOWN
            && incoming_subgroup_count > publish_done.stream_count
        {
            publish_done.stream_count_overrun = true;
        }
        Ok(())
    }

    fn note_incoming_subgroup_stream_closed(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let subscription = self.subscriptions.get_mut(&request_id).ok_or_else(|| {
            SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "incoming subgroup stream points to missing subscription",
            )
        })?;
        let counts = &mut subscription.stream_counts;
        counts.open_incoming_subgroup_count = counts
            .open_incoming_subgroup_count
            .checked_sub(1)
            .ok_or(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "open incoming subgroup stream count underflow",
            ))?;
        Ok(())
    }

    /// 指定 request の subscription がキャンセル由来 `Terminated` かどうかを判定する
    ///
    /// キャンセル経路（`stop_sending` / `handle_err_for_subscription` /
    /// `close_subscription_on_stream_end` / `terminate_malformed_track`）は `publish_done` を
    /// 設定しないため、`Terminated` かつ `publish_done` が `None` であればキャンセル由来と
    /// 判定できる。PUBLISH_DONE 受信による `Terminated`（`publish_done` が `Some`）は
    /// drain 期間中の遅延データを受理するため対象外
    /// (draft-ietf-moq-transport-21 §9.9)。
    fn is_cancelled_terminated_subscription(&self, request_id: u64) -> bool {
        self.subscriptions
            .get(&request_id)
            .is_some_and(is_cancelled_terminated)
    }

    /// 破棄対象 stream を `IncomingDataStream::Discarded` として登録する
    ///
    /// `request_id` はキャンセル由来候補の subscription が存在する場合に `Some`、
    /// `forget_subscription` 後（候補が存在しない場合）は `None` になる。
    ///
    /// この variant は終端 (FIN / RESET_STREAM / STOP_SENDING / `report_mid_object_fin` /
    /// forget による除去) が来るまで `incoming` に残る。データが流れ続ける間は
    /// 吸収し続けるのが設計であり (draft §3.1.2)、期限管理の対象は終端済み id を移す
    /// 保持集合 (`data_streams.discarded`) のみ。peer が終端を送らず放置した場合は
    /// I/O 層が STOP_SENDING を送って閉じることで初めて保持集合 (期限管理) に入る。
    ///
    /// 既存 `Subgroup` variant からの掩き替えでは `open_incoming_subgroup_count` を
    /// デクリメントする。`Discarded` variant の終端 (`retain_discarded_stream_id`) は
    /// `note_incoming_subgroup_stream_closed` を呼ばないため、掩き替え時点で補正しないと
    /// 計数が張り付いたまま残り、`cleanup_ready()` が `open_incoming_subgroup_count == 0`
    /// を要求するために `forget_subscription` が永久に不可能になる (subscription リーク)。
    fn register_discarded_stream(&mut self, stream_id: DataStreamId, request_id: Option<u64>) {
        let existing_subgroup_request_id = match self.data_streams.incoming.get(&stream_id) {
            Some(IncomingDataStream::Subgroup { request_id, .. }) => Some(*request_id),
            _ => None,
        };
        self.data_streams
            .incoming
            .insert(stream_id, IncomingDataStream::Discarded { request_id });
        // DATA_STREAM_TIMEOUT の監視対象から外す
        self.timing.data_stream_last_activity_ms.remove(&stream_id);
        if let Some(subgroup_request_id) = existing_subgroup_request_id {
            // Subgroup variant として登録されていれば subscription は必ず存在する不変条件
            // (forget_subscription は remove_incoming_data_streams_for_request で incoming
            // からも同時に除去するため)。それでも subscription が消えていた場合は
            // 計数対象がないので何もしない (Err の伝播は不要)。
            let _ = self.note_incoming_subgroup_stream_closed(subgroup_request_id);
        }
    }

    /// stream id を破棄対象の保持集合へ登録する
    ///
    /// 保持期間は登録時点から `peer_alias_retention_ms`（alias tombstone と同じ長さ）。
    /// 既に登録済みの id は期限を更新しない。
    fn retain_discarded_stream_id(&mut self, stream_id: DataStreamId) {
        self.data_streams
            .discarded
            .entry(stream_id)
            .or_insert_with(|| {
                super::types::DeadlineTimer::new(
                    self.timing.peer_alias_retention_ms,
                    self.timing.last_tick_ms,
                )
            });
        // DATA_STREAM_TIMEOUT の監視対象から外す (更新を止めた stream を監視対象に
        // 残すと、データが流れ続けていてもセッションが fail する)
        self.timing.data_stream_last_activity_ms.remove(&stream_id);
    }

    /// 終端済みの破棄対象 stream id の保持期間を進め、期限切れを掃除する
    ///
    /// 対象は保持集合 (`data_streams.discarded`) に移された終端済み id のみで、
    /// `incoming` に残る `Discarded` variant (未終端) は対象外
    /// ([`register_discarded_stream`](Self::register_discarded_stream) の doc 参照)。
    /// alias tombstone（`tick_peer_alias_tombstones`）には連動させない
    /// (共有 alias で `Established` subscription が残っている間は tombstone が
    /// 登録されず、連動すると掃除が発火しない)。
    pub(super) fn tick_discarded_data_stream_ids(&mut self, now_ms: u64) {
        for timer in self.data_streams.discarded.values_mut() {
            timer.tick(now_ms);
        }
        self.data_streams
            .discarded
            .retain(|_, timer| !timer.expired);
    }

    /// peer publisher の Track Alias から候補 subscription をすべて解決する
    ///
    /// draft-ietf-moq-transport-21 §3.1 (Subscriptions): "Because subscriptions can share a
    /// Track Alias, the subscriber re-applies each subscription's filter to determine which
    /// subscription a received Object belongs to."
    ///
    /// 同一 Track への複数 subscription が alias を共有しうるため候補は複数になる。
    /// 呼び出し側は `header_passes_filters` / `object_passes_filters` で候補を絞り込み、
    /// 最初に通過した subscription に紐づける。キャンセル由来 `Terminated` の候補は
    /// 受理対象から除外され、合格した候補がキャンセル由来のみの場合は `Discarded` になる
    /// (draft §3.1.2)。
    /// 返り値が空なら未知 alias である。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    fn resolve_peer_track_alias(&self, track_alias: u64) -> Result<Vec<u64>, SessionError> {
        let Some(ids) = self.aliases.peer_publisher_aliases.get(&track_alias) else {
            return Ok(Vec::new());
        };
        for request_id in ids {
            let Some(subscription) = self.subscriptions.get(request_id) else {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "peer track alias points to missing subscription",
                ));
            };
            if subscription.my_role != TrackRole::Subscriber {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "peer track alias points to non-subscriber subscription",
                ));
            }
        }
        Ok(ids.clone())
    }

    fn has_other_fetch_stream(&self, stream_id: DataStreamId, request_id: u64) -> bool {
        self.data_streams
            .incoming
            .iter()
            .any(|(existing_id, stream)| {
                *existing_id != stream_id
                    && matches!(
                        stream,
                        IncomingDataStream::Fetch {
                            request_id: existing_request_id,
                            ..
                        } if *existing_request_id == request_id
                    )
            })
    }
}

fn session_error_from_data_message(err: crate::error::MessageError) -> SessionError {
    match err {
        crate::error::MessageError::ProtocolViolation(msg) => {
            SessionError::new(SESSION_PROTOCOL_VIOLATION, msg)
        }
        crate::error::MessageError::KeyValueFormattingError(msg) => {
            SessionError::new(SESSION_KEY_VALUE_FORMATTING_ERROR, msg)
        }
        _ => SessionError::new(SESSION_PROTOCOL_VIOLATION, "data plane decode error"),
    }
}
