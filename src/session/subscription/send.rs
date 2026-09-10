//! SUBSCRIBE / PUBLISH / REQUEST_UPDATE / PUBLISH_DONE / PUBLISH_STATE_NOTIFY の送信
//!
//! draft-ietf-moq-transport-21 §9.6-§9.9 と
//! draft-ietf-moq-transport-21 §9.10 (PUBLISH_STATE_NOTIFY) に対応。
//! 将来 draft 側で変更される可能性がある。

use crate::error::{
    PUBLISH_DONE_INTERNAL_ERROR, SESSION_DUPLICATE_TRACK_ALIAS, SESSION_PROTOCOL_VIOLATION,
    is_local_error_code,
};
use crate::message::{
    ControlMessage, FETCH_UPDATE_ALLOWED_PARAMS, NAMESPACE_PUBLICATION_UPDATE_ALLOWED_PARAMS,
    NAMESPACE_SUBSCRIPTION_UPDATE_ALLOWED_PARAMS, PUBLISH_ALLOWED_PARAMS,
    PUBLISH_STATE_NOTIFY_ALLOWED_PARAMS, Publish, PublishDone, PublishStateNotify, ReasonPhrase,
    RequestUpdate, SUBSCRIBE_ALLOWED_PARAMS, SUBSCRIBE_OK_ALLOWED_PARAMS,
    SUBSCRIPTION_UPDATE_ALLOWED_PARAMS, Subscribe, SubscribeOk,
    TRACK_SUBSCRIPTION_UPDATE_ALLOWED_PARAMS, common::TrackNamespace,
};
use crate::message_parameter::{LocationFilterContext, LocationFilterUpdate, MessageParameters};
use crate::track_properties::TrackProperties;
use alloc::vec::Vec;
use hashbrown::HashMap;

use super::super::core::{
    PUBLISH_DONE_STREAM_COUNT_UNKNOWN, RequestTable, Session, alias_used_by_different_track,
    insert_alias_holder,
};
use super::super::namespace::terminationreason_from_end;
use super::super::types::{
    DeadlineTimer, DeliveryTimeoutState, RequestKind, RequestStreamEnd, SendRequestError,
    SessionError, SessionEvent, StreamCountState, Subscription, SubscriptionInitiator,
    SubscriptionRangeFilters, SubscriptionState, TerminationReason, TrackRole,
};
use super::delivery::{
    compute_effective_delivery_timeout_ms, effective_largest_object, set_subscription_expires,
    set_subscription_publisher_object_delivery_timeout,
    set_subscription_publisher_subgroup_delivery_timeout, update_largest_object_in_parameters,
    update_subscription_subscriber_delivery_timeouts_if_present,
};
use super::validation::{
    extract_forward_state, extract_range_filters, resolve_location_filter, track_properties_pass,
    validate_forward, validate_group_order, validate_include_properties,
};

impl Session {
    /// Subscribe / Publish の bidi request stream 終端時の共通処理
    ///
    /// draft-ietf-moq-transport-21 §6.4.2.2 (Graceful Request Stream Closure) / §6.4.2.3
    /// (Request Cancellation and Rejection): FIN はその方向に送るメッセージが終わったこと
    /// だけを示し、cancel は RESET_STREAM / STOP_SENDING で表現される。state を `Terminated`
    /// にし、`request_streams` から除去することで、以降の close 通知が重複処理されないようにする。
    pub(crate) fn close_subscription_on_stream_end(
        &mut self,
        request_id: u64,
        end: RequestStreamEnd,
    ) -> Result<TerminationReason, SessionError> {
        let Some(sub) = self.subscriptions.get_mut(&request_id) else {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "bidi request stream close for unknown subscription request id",
            ));
        };
        sub.state = SubscriptionState::Terminated;
        // draft-ietf-moq-transport-21 §3.4.1 (Opening and Closing Fill Fetch Streams):
        // subscription のキャンセル時は open 中の fill fetch stream を reset する (MUST)。
        self.reset_open_fill_streams(request_id);
        // クローズ通知を受信済みの request は request_streams から除去する。これにより
        // SUBSCRIBE_TRACKS の bidi stream 終端が後から来ても、同じ subscription に対して
        // `close_track_subscription_on_stream_end` が二重に RequestTerminated を発行したり
        // `rejected_request_ids` に close 済み id を登録したりしない。
        self.request_streams.remove(&request_id);
        Ok(terminationreason_from_end(end))
    }

    // ─── 送信 API ──────────────────────────────────────────

    /// SUBSCRIBE を送信する (自側が subscriber)
    ///
    /// draft §3.1 (Subscriptions): Idle → Pending (Subscriber) に遷移し、
    /// [`SessionEvent::SendRequest`] を発行する。
    pub fn send_subscribe(
        &mut self,
        track_namespace: TrackNamespace,
        track_name: Vec<u8>,
        parameters: MessageParameters,
    ) -> Result<u64, SendRequestError> {
        self.require_established()?;
        self.check_peer_goaway()?;
        // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces): single period `.` 予約名前空間への SUBSCRIBE は送信不可
        if track_namespace.is_single_period() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "application cannot use single-period reserved namespace",
            )
            .into());
        }
        // draft-ietf-moq-transport-21 §3.3.2 (Range Filters): peer の MAX_FILTER_RANGES 超過は送信不可
        self.validate_outgoing_range_filters(&parameters)?;
        // draft §9.20.1 (Parameter Scope): SUBSCRIBE で許可されないパラメータを含む
        // 送信は API 呼び出し時に拒否する。検証がないと request 登録・ SendRequest の push
        // まで完了した後に I/O 層のエンコード時 (validate_scope) で非同期に失敗し、アプリ
        // は送信成功と誤認したまま peer に届かない (受信側はワイヤ層で検証済みのため、
        // ここは送信側の状態整合性のための検証)。
        // 検証は request_id 発行より前に置き、エラー時に欠番を作らない (SUBSCRIBE_TRACKS 送信
        // (`send_subscribe_tracks`) と同じ設計判断)。なお本検証はパラメータのスコープのみを
        // 対象とし、スコープ内の同一型の重複出現 (例: OBJECT_DELIVERY_TIMEOUT 2 件) は
        // 検出されず encode 層の重複検出で失敗する残存ギャップがある (SUBSCRIBE_OK 送信側
        // parameter scope 検証と同じ制約)。
        if parameters.validate_scope(SUBSCRIBE_ALLOWED_PARAMS).is_err() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "SUBSCRIBE parameter not allowed in this context",
            )
            .into());
        }
        // draft-ietf-moq-transport-21 §3.1.1: 同一 Track への複数同時 subscription が許可されたため、
        // 自側 subscriber role の重複チェックは行わない。
        //
        // draft §10.4 / §10.5: SUBSCRIBE に TrackProperties は乗らない。peer publisher が
        // SUBSCRIBE_OK で発行した値を handle_subscribe_ok が反映する。
        let default_publisher_priority = None;
        let default_publisher_group_order = None;
        let request_id = self.request_ids.local_generator.next_id();
        if let Some(f) = parameters.forward() {
            validate_forward(f)?;
        }
        let forward_state = extract_forward_state(&parameters);
        let subscriber_object_delivery_timeout_ms = parameters.object_delivery_timeout();
        let subscriber_subgroup_delivery_timeout_ms = parameters.subgroup_delivery_timeout();
        let subscriber_rendezvous_timeout_ms = parameters.rendezvous_timeout();
        let filter = match parameters.location_filter_update() {
            Ok(LocationFilterUpdate::Set(filter)) => Some(filter),
            // 省略時と Length 0 (no filter) はどちらも unfiltered として扱う
            Ok(_) => None,
            Err(_) => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "invalid subscription filter encoding",
                )
                .into());
            }
        };
        // draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter):
        // FILL 内側の Table 6 スコープと LOCATION_FILTER も送信前に検証し、
        // 不正値の送出と状態登録を防ぐ
        super::fill::validate_outgoing_fill_parameters(&parameters)?;
        // draft §5.1.2 / §5.1.4: 自側が送ったフィルタを保持する。自側は subscriber なので
        // Pass 評価には使わないが、状態として peer 側と対称に持つ。
        let (filter_start, filter_end) =
            resolve_location_filter(filter.as_ref(), None, LocationFilterContext::Subscription);
        let range_filters = extract_range_filters(&parameters);
        let subscriber_priority = parameters.subscriber_priority();
        let group_order = parameters.group_order();
        if let Some(order) = group_order {
            validate_group_order(order)?;
        }
        // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
        // 送信前に値域を検証し、不正値の送出と状態登録を防ぐ
        if let Some(v) = parameters.include_properties() {
            validate_include_properties(v)?;
        }
        let subscription = Subscription {
            request_id,
            initiator: SubscriptionInitiator::Subscriber,
            my_role: TrackRole::Subscriber,
            track_namespace: track_namespace.clone(),
            track_name: track_name.clone(),
            track_alias: None,
            state: SubscriptionState::Pending,
            forward_state,
            largest_location: None,
            largest_received_location: None,
            delivery_timeouts: DeliveryTimeoutState {
                subscriber_object_ms: subscriber_object_delivery_timeout_ms,
                subscriber_subgroup_ms: subscriber_subgroup_delivery_timeout_ms,
                publisher_object_ms: None,
                publisher_subgroup_ms: None,
                effective_object_ms: compute_effective_delivery_timeout_ms(
                    subscriber_object_delivery_timeout_ms,
                    None,
                ),
                effective_subgroup_ms: compute_effective_delivery_timeout_ms(
                    subscriber_subgroup_delivery_timeout_ms,
                    None,
                ),
                subgroup_overrides: HashMap::new(),
            },
            subscriber_rendezvous_timeout_ms,
            expires: None,
            // Subscriber 役では track_properties は持たない (受信した SUBSCRIBE_OK 内の値で
            // publisher 側が判定する)。NEW_GROUP_REQUEST 検証は publisher 側で行う。
            dynamic_groups: false,
            publisher_priority: None,
            default_publisher_priority,
            default_publisher_group_order,
            stream_counts: StreamCountState {
                published_count: 0,
                incoming_subgroup_count: 0,
                open_incoming_subgroup_count: 0,
            },
            publish_done: None,
            pending_publish_done: None,
            filter,
            subscriber_priority,
            group_order,
            // 自側は subscriber のため SUBSCRIBE_OK を送らず、INCLUDE_PROPERTIES の保持は不要
            include_properties: None,
            filter_start,
            filter_end,
            range_filters,
            ended_groups: HashMap::new(),
            end_of_track: None,
            pending_update_params: None,
        };
        self.register_subscription(request_id, subscription);
        self.request_streams
            .insert(request_id, RequestKind::Subscribe);
        self.start_control_message_deadline(request_id);
        let msg = ControlMessage::Subscribe(Subscribe {
            request_id,
            track_namespace,
            track_name,
            parameters,
        });
        self.events.push_back(SessionEvent::SendRequest {
            request_id,
            message: msg,
        });
        Ok(request_id)
    }

    /// PUBLISH を送信する (自側が publisher)
    ///
    /// draft §3.1 (Subscriptions): Idle → Pending (Publisher) に遷移し、
    /// [`SessionEvent::SendRequest`] を発行する。`track_alias` は draft §3.1.2 (Track Alias) に従い自側が一意に採番する。
    /// peer の TRACK_PROPERTY_FILTER を通らない Track は
    /// [`SendRequestError::LocalFilterMismatch`] を返して送信を抑止する。
    pub fn send_publish(
        &mut self,
        track_namespace: TrackNamespace,
        track_name: Vec<u8>,
        track_alias: u64,
        parameters: MessageParameters,
        track_properties: TrackProperties,
    ) -> Result<u64, SendRequestError> {
        self.require_established()?;
        self.check_peer_goaway()?;
        // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces): single period `.` 予約名前空間への PUBLISH は送信不可
        if track_namespace.is_single_period() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "application cannot use single-period reserved namespace",
            )
            .into());
        }
        // draft §6.5 (Session-Level Tracks and Namespaces): Application は .session 名前空間に publish できない
        if track_namespace.is_session_level() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "application cannot publish to .session namespace",
            )
            .into());
        }
        // draft-ietf-moq-transport-21 §9.19 (PUBLISH_SKIPPED): PUBLISH_SKIPPED 送信済みの Track には
        // 同一 SUBSCRIBE_TRACKS に対して PUBLISH を送信してはならない (MUST NOT)
        for ts in self.track_subscriptions.values() {
            if ts.my_role != TrackRole::Publisher {
                continue;
            }
            let prefix_fields = ts.prefix.fields();
            let ns_fields = track_namespace.fields();
            if ns_fields.len() >= prefix_fields.len()
                && ns_fields[..prefix_fields.len()] == *prefix_fields
            {
                let suffix_fields = ns_fields[prefix_fields.len()..].to_vec();
                if let Ok(suffix) = TrackNamespace::new(suffix_fields)
                    && ts.skipped_tracks.contains(&(suffix, track_name.clone()))
                {
                    return Err(SessionError::new(
                        SESSION_PROTOCOL_VIOLATION,
                        "cannot PUBLISH a Track after PUBLISH_SKIPPED was sent",
                    )
                    .into());
                }
                // draft §3.3.2 (Range Filters): "PUBLISH messages which pass the filter will be
                // forwarded while those which do not pass it will not be forwarded nor will any
                // Objects." peer subscriber が TRACK_PROPERTY_FILTER を指定していて Track が
                // それを通らないなら PUBLISH を送らない。
                //
                // prefix が一致する publisher 役の SUBSCRIBE_TRACKS は
                // `prefix_overlaps` により最大 1 件なので、ここでの判定は 1 度しか成立しない。
                if !track_properties_pass(&ts.track_property_filters, &track_properties) {
                    return Err(SendRequestError::LocalFilterMismatch);
                }
            }
        }
        // draft §11.1 (Track Alias): "The same Track Alias MUST NOT be used by a publisher to
        // refer to two different Tracks simultaneously in the same session."
        // 同一 Track への共有は §5.1 が明示的に許可しているので拒否しない。
        if alias_used_by_different_track(
            &self.aliases.my_publisher_aliases,
            &self.subscriptions,
            track_alias,
            &track_namespace,
            &track_name,
            None,
            false,
        ) {
            return Err(SessionError::new(
                SESSION_DUPLICATE_TRACK_ALIAS,
                "local publisher reused track alias for a different track",
            )
            .into());
        }
        // draft-ietf-moq-transport-21 §3.1.1: 同一 Track への複数同時 subscription が許可されたため、
        // 自側 publisher role の重複チェックは行わない。
        // draft §9.20.1 (Parameter Scope): PUBLISH で許可されないパラメータを含む送信は
        // API 呼び出し時に拒否する。検証がないと request 登録・ SendRequest の push まで
        // 完了した後に I/O 層のエンコード時 (validate_scope) で非同期に失敗し、アプリは
        // 送信成功と誤認したまま peer に届かない (受信側はワイヤ層で検証済みのため、
        // ここは送信側の状態整合性のための検証)。
        // 検証は request_id 発行より前に置き、エラー時に欠番を作らない (SUBSCRIBE_TRACKS 送信
        // (`send_subscribe_tracks`) と同じ設計判断)。なお本検証はパラメータのスコープのみを
        // 対象とし、スコープ内の同一型の重複出現 (例: EXPIRES 2 件) は検出されず encode
        // 層の重複検出で失敗する残存ギャップがある (SUBSCRIBE_OK 送信側 parameter scope
        // 検証と同じ制約)。
        if parameters.validate_scope(PUBLISH_ALLOWED_PARAMS).is_err() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH parameter not allowed in this context",
            )
            .into());
        }
        // draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): PUBLISH に含まれる
        // GROUP_ORDER は SUBSCRIBE_TRACKS からの伝播 (§9.18.1) であり、値域は Ascending
        // (0x1) / Descending (0x2) のみ。値域外は API 呼び出し時に拒否する (検証がないと
        // request 登録・ SendRequest の push まで完了した後に I/O 層のエンコード時
        // (validate_param_encoding) で非同期に失敗する)。
        // 検証は request_id 発行より前に置き、エラー時に欠番を作らない。なお本検証は
        // Uint8 型の最初の 1 件のみを対象とし、型不一致 (VarInt)・複数出現の 2 番目以降は
        // すり抜けて encode 時に失敗する残存ギャップがある (SUBSCRIBE_TRACKS 送信の
        // GROUP_ORDER 値域検証と同じ制約)。
        if let Some(order) = parameters.group_order() {
            validate_group_order(order)?;
        }
        // draft-ietf-moq-transport-21 §9.20.10 (LOCATION FILTER Parameter):
        // 不正形式の LOCATION_FILTER は送信前に拒否し、不正値の送出と状態登録を防ぐ
        // (request_id 発行より前に置き、エラー時に欠番を作らない)。
        // `location_filter_update()` は `Result<LocationFilterUpdate, MessageError>` を
        // 返し、`MessageError` から `SessionError` への `From` 実装はないため
        // `?` では伝播できない。ここで束縛した filter は後段の状態構築に使い回す。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        let filter = match parameters.location_filter_update() {
            Ok(LocationFilterUpdate::Set(filter)) => Some(filter),
            Ok(_) => None,
            Err(_) => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "invalid filter encoding in PUBLISH",
                )
                .into());
            }
        };
        let request_id = self.request_ids.local_generator.next_id();
        if let Some(f) = parameters.forward() {
            validate_forward(f)?;
        }
        let forward_state = extract_forward_state(&parameters);
        let publisher_object_delivery_timeout_ms = track_properties.object_delivery_timeout();
        let publisher_subgroup_delivery_timeout_ms = track_properties.subgroup_delivery_timeout();
        let expires = parameters
            .expires()
            .map(|expires_ms| DeadlineTimer::new(expires_ms, self.timing.last_tick_ms));
        // draft-ietf-moq-transport-21 §10.6 (DYNAMIC GROUPS) / §9.20.20 (NEW GROUP REQUEST Parameter): 自側 Publisher が発行する DYNAMIC_GROUPS Property の値を
        // 覚えておく。peer からの REQUEST_UPDATE に NEW_GROUP_REQUEST が
        // 含まれる場合の検証に用いる。
        let dynamic_groups = track_properties.dynamic_groups() == Some(1);
        // draft §10.4 (DEFAULT PUBLISHER PRIORITY) / §10.5 (DEFAULT PUBLISHER GROUP ORDER)
        let default_publisher_priority = track_properties.default_publisher_priority();
        let default_publisher_group_order = track_properties.default_publisher_group_order();
        // draft-ietf-moq-transport-21 §9.8 (PUBLISH) / §9.18.1: PUBLISH の Parameters に
        // 現れる subscription パラメータは初期状態として保持する (受信側と対称)。
        // 解決時点の largest を持たないため、相対フィルタは先頭から解決される。
        // delivery timeout の Parameters は保持しない。§5.2 の規則
        // (publisher は Track Property、subscriber は SUBSCRIBE / REQUEST_UPDATE の
        // Message Parameters で伝える) に従い、publisher 側値は Track Properties、
        // subscriber 側値は REQUEST_UPDATE 経路が担う。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        let (filter_start, filter_end) =
            resolve_location_filter(filter.as_ref(), None, LocationFilterContext::Subscription);
        let range_filters = SubscriptionRangeFilters::default();
        // draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): GROUP_ORDER は SUBSCRIBE / PUBLISH /
        // SUBSCRIBE_TRACKS / FETCH に出現可能。PUBLISH には §9.18.1 (Parameters on
        // SUBSCRIBE_TRACKS) の伝播として含めることができる。初期状態として保持する。
        let subscription = Subscription {
            request_id,
            initiator: SubscriptionInitiator::Publisher,
            my_role: TrackRole::Publisher,
            track_namespace: track_namespace.clone(),
            track_name: track_name.clone(),
            track_alias: Some(track_alias),
            state: SubscriptionState::Pending,
            forward_state,
            largest_location: None,
            largest_received_location: None,
            delivery_timeouts: DeliveryTimeoutState {
                subscriber_object_ms: None,
                subscriber_subgroup_ms: None,
                publisher_object_ms: publisher_object_delivery_timeout_ms,
                publisher_subgroup_ms: publisher_subgroup_delivery_timeout_ms,
                effective_object_ms: compute_effective_delivery_timeout_ms(
                    None,
                    publisher_object_delivery_timeout_ms,
                ),
                effective_subgroup_ms: compute_effective_delivery_timeout_ms(
                    None,
                    publisher_subgroup_delivery_timeout_ms,
                ),
                subgroup_overrides: HashMap::new(),
            },
            subscriber_rendezvous_timeout_ms: None,
            expires,
            dynamic_groups,
            publisher_priority: None,
            default_publisher_priority,
            default_publisher_group_order,
            stream_counts: StreamCountState {
                published_count: 0,
                incoming_subgroup_count: 0,
                open_incoming_subgroup_count: 0,
            },
            publish_done: None,
            pending_publish_done: None,
            filter,
            subscriber_priority: parameters.subscriber_priority(),
            group_order: parameters.group_order(),
            // PUBLISH に INCLUDE_PROPERTIES は出現しない (draft-ietf-moq-transport-21 §9.20.22)
            include_properties: None,
            filter_start,
            filter_end,
            range_filters,
            ended_groups: HashMap::new(),
            end_of_track: None,
            pending_update_params: None,
        };
        self.register_subscription(request_id, subscription);
        self.request_streams
            .insert(request_id, RequestKind::Publish);
        self.start_control_message_deadline(request_id);
        insert_alias_holder(
            &mut self.aliases.my_publisher_aliases,
            track_alias,
            request_id,
        );
        // publisher 側は LARGEST_OBJECT を保存しない
        // (旧称 Joining Location の保持 MUST は draft-ietf-moq-transport-21 で廃止。
        // publisher の観測 Largest は `largest_received_location` で追跡する)。
        let msg = ControlMessage::Publish(Publish {
            request_id,
            track_namespace,
            track_name,
            track_alias,
            parameters,
            track_properties,
        });
        self.events.push_back(SessionEvent::SendRequest {
            request_id,
            message: msg,
        });
        Ok(request_id)
    }

    /// SUBSCRIBE_OK を応答送信する (自側が publisher、Subscribe を受信した側)
    ///
    /// draft §3.1 (Subscriptions): Pending (Subscriber) → Established に遷移する。
    /// `track_alias` は draft §3.1.2 (Track Alias) に従い自側 (publisher) が一意に採番する。
    ///
    /// draft §9.20.1 (Parameter Scope): SUBSCRIBE_OK で許可されるパラメータは
    /// EXPIRES / LARGEST_OBJECT のみ。スコープ外パラメータを含む SUBSCRIBE_OK は
    /// `SESSION_PROTOCOL_VIOLATION` で拒否され、subscription の状態・ track_alias ・
    /// パラメータは一切変更されない。
    ///
    /// 同様に、エンコード時に失敗する不正な `track_properties` (同一 prop_type の重複・
    /// 型不整合・値域違反・ IMMUTABLE_PROPERTIES 内側の不正) も `SESSION_PROTOCOL_VIOLATION`
    /// で拒否され、subscription の状態・ track_alias ・パラメータは一切変更されない。
    pub fn send_subscribe_ok(
        &mut self,
        request_id: u64,
        track_alias: u64,
        mut parameters: MessageParameters,
        track_properties: TrackProperties,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        let now_ms = self.timing.last_tick_ms;
        let subscription = self.subscriptions.get_mut(&request_id).ok_or_else(|| {
            SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "subscription not found for send_subscribe_ok",
            )
        })?;
        if subscription.my_role != TrackRole::Publisher || !subscription.is_pending_subscriber() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "send_subscribe_ok requires publisher role in Pending(Subscriber) state",
            ));
        }
        // draft §9.20.1 (Parameter Scope): SUBSCRIBE_OK で許可されるパラメータは
        // EXPIRES / LARGEST_OBJECT のみ (draft-ietf-moq-transport-21 §9.20.17 (EXPIRES Parameter) /
        // §9.20.18 (LARGEST OBJECT Parameter) の MAY appear 列挙)。
        // スコープ外パラメータを含む SUBSCRIBE_OK では状態を一切変更せずエラーを返す。
        // 受信側はワイヤ層 (`SubscribeOk::decode_message_body` の validate_scope) で検証済みのため、
        // ここは送信側の状態整合性のための検証。検証が状態遷移後に走ると、subscription が
        // `Established` に遷移済みのまま後段のエンコード失敗で peer に SUBSCRIBE_OK が送られない
        // 孤児状態が残る (エンコードは sans-I/O のため I/O 層で行われる)。
        // 検証は LARGEST_OBJECT 注入処理 (`update_largest_object_in_parameters`) より前に置き、
        // 注入ロジックを失わない (注入は検証通過後のパラメータに対して行われる)。
        // なお本検証はパラメータのスコープのみを対象とし、スコープ外でないがエンコード時に
        // 失敗する入力のうち `track_properties` は後述の事前検証で弾かれ、同一パラメータの
        // 重複はエンコード時検証のまま残る (同型の孤児化経路)。
        if parameters
            .validate_scope(SUBSCRIBE_OK_ALLOWED_PARAMS)
            .is_err()
        {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "SUBSCRIBE_OK parameter not allowed in this context",
            ));
        }
        // track_properties の事前検証: `SubscribeOk::encode_message_body` の
        // `track_properties.encode(buf)?` と同一の検証 (同一 prop_type の重複・型整合性・
        // 値域 (`validate_track_property_value_range`)・ IMMUTABLE_PROPERTIES 内側の KVP
        // 列検証) を状態遷移・ track_alias 代入・パラメータ適用より前に通す。検証がないと、
        // それらの適用後に I/O 層のエンコードで失敗し、subscription が Established に遷移
        // 済みのまま peer に SUBSCRIBE_OK が送られない孤児状態が残る (エンコードは
        // sans-I/O のため I/O 層で行われる)。事前呼び出しの成功は track_properties 部分の
        // I/O 層エンコード成功を保証する (`validate_track_property_value_range` のみの
        // 公開化では値域のみで、型整合・重複・ IMMUTABLE 内側の孤児化経路が残る)。
        // `encode` は `MessageError` を返すため `?` では伝播できず、`is_err()` 判定で
        // `SessionError` に変換する。
        let mut buf = Vec::new();
        if track_properties.encode(&mut buf).is_err() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "invalid track properties in SUBSCRIBE_OK",
            ));
        }
        // draft §3.1.2 (Track Alias): 異なる Track への alias 再利用のみ拒否する。
        // 同一 Track の別 subscription が同じ alias を使うのは §3.1 が許可している。
        let (ns_for_alias, name_for_alias) = (
            subscription.track_namespace.clone(),
            subscription.track_name.clone(),
        );
        if alias_used_by_different_track(
            &self.aliases.my_publisher_aliases,
            &self.subscriptions,
            track_alias,
            &ns_for_alias,
            &name_for_alias,
            Some(request_id),
            false,
        ) {
            return Err(SessionError::new(
                SESSION_DUPLICATE_TRACK_ALIAS,
                "local publisher reused track alias in subscribe_ok for a different track",
            ));
        }
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("subscription presence already checked above");
        subscription.track_alias = Some(track_alias);
        subscription.state = SubscriptionState::Established;
        // draft-ietf-moq-transport-21 §10.6 (DYNAMIC GROUPS) / §9.20.20 (NEW GROUP REQUEST Parameter): SUBSCRIBE_OK で発行する DYNAMIC_GROUPS を記録する
        subscription.dynamic_groups = track_properties.dynamic_groups() == Some(1);
        // draft §10.4 (DEFAULT PUBLISHER PRIORITY) / §10.5 (DEFAULT PUBLISHER GROUP ORDER)
        subscription.default_publisher_priority = track_properties.default_publisher_priority();
        subscription.default_publisher_group_order =
            track_properties.default_publisher_group_order();
        set_subscription_publisher_object_delivery_timeout(
            subscription,
            track_properties.object_delivery_timeout(),
        );
        set_subscription_publisher_subgroup_delivery_timeout(
            subscription,
            track_properties.subgroup_delivery_timeout(),
        );
        set_subscription_expires(subscription, parameters.expires(), now_ms);
        // draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter): LARGEST_OBJECT は actual received 以上でなければならない
        let effective = effective_largest_object(subscription);
        if let Some(ref loc) = effective {
            update_largest_object_in_parameters(&mut parameters, loc);
        }
        insert_alias_holder(
            &mut self.aliases.my_publisher_aliases,
            track_alias,
            request_id,
        );
        // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
        // peer が SUBSCRIBE で INCLUDE_PROPERTIES=0 を指定したとき、SUBSCRIBE_OK の
        // Track Properties は存在するが空にする (SHOULD)。状態への反映
        // (dynamic_groups 等) には元の発行値を使い、wire 上の値だけ空化する。
        let track_properties = if subscription.include_properties == Some(0) {
            TrackProperties::new()
        } else {
            track_properties
        };
        let msg = ControlMessage::SubscribeOk(SubscribeOk {
            track_alias,
            parameters,
            track_properties,
        });
        self.events.push_back(SessionEvent::SendOnStream {
            request_id,
            message: msg,
            fin: false,
        });
        // draft-ietf-moq-transport-21 Appendix A.3 (Since draft-ietf-moq-transport-17) #1609:
        // 後続処理の前に pending REQUEST_UPDATE を適用する。
        // forward_state 変更が pending にあれば先に反映する。
        if let Some(pending_params) = subscription.pending_update_params.take()
            && let Some(f) = pending_params.forward()
        {
            subscription.forward_state = validate_forward(f)?;
        }
        Ok(())
    }

    /// REQUEST_UPDATE を送信する
    ///
    /// draft §10.9 (REQUEST_UPDATE): request の sender は同じ bidi request stream に
    /// REQUEST_UPDATE を書ける。Session は `request_id` の種別ごとに送信可否を
    /// 検証する。subscription のみ、FORWARD parameter が含まれていれば
    /// `forward_state` を楽観的に更新する。
    ///
    /// draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES): クレジットは送信が
    /// 確定したときのみ消費される (検証失敗で送信されない場合は消費されない。
    /// "A REQUEST_UPDATE is considered outstanding from when it is sent until the sender
    /// receives the corresponding REQUEST_OK or REQUEST_ERROR response.")。
    pub fn send_request_update(
        &mut self,
        request_id: u64,
        parameters: MessageParameters,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        // draft-ietf-moq-transport-21 §3.3.2 (Range Filters): peer の MAX_FILTER_RANGES 超過は送信不可
        self.validate_outgoing_range_filters(&parameters)?;

        // draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES): peer の SETUP で宣言された
        // MAX_REQUEST_UPDATES を超える outstanding REQUEST_UPDATE の送信を禁止する。
        // デフォルト値 0 は無制限を意味する。
        // 上限チェックは楽観的に state を更新する検証 (`send_update_for_*` 内、
        // 例: subscription の `forward_state`) の前に残す: 検証の後方に移動すると
        // `TOO_MANY_REQUEST_UPDATES` の Err なのに state が更新済みになる不整合が生じる。
        // 帰結として、outstanding 満杯時は `TOO_MANY_REQUEST_UPDATES` が
        // `send_update_for_*` 内の `SESSION_PROTOCOL_VIOLATION` より優先される
        // (満杯 + 状態違反なら上限違反が返る。なお `require_established` と
        // `validate_outgoing_range_filters` は上限チェックより前にあり優先され、
        // `locate_request` による未知 request id の検出とパラメータ scope 検証は
        // 上限チェックより後方にあるため満杯時は到達しない)。
        // クレジット加算 (エントリ生成) は送信が確定する全検証の後に移動し、
        // 送信されなかった REQUEST_UPDATE がクレジットを消費しないようにする
        // (draft §9.1.7: "A REQUEST_UPDATE is considered outstanding from when it is sent
        // until the sender receives the corresponding REQUEST_OK or REQUEST_ERROR response.")。
        // 上限チェックはエントリを生成せずに読む。
        let peer_max = self
            .setup
            .peer
            .as_ref()
            .and_then(|s| s.options.max_request_updates())
            .unwrap_or(0);
        if peer_max > 0 {
            let count = self
                .outgoing_request_updates
                .get(&request_id)
                .copied()
                .unwrap_or(0);
            if count >= peer_max {
                return Err(SessionError::new(
                    crate::error::SESSION_TOO_MANY_REQUEST_UPDATES,
                    "outgoing REQUEST_UPDATE exceeds peer MAX_REQUEST_UPDATES",
                ));
            }
        }

        // draft §9.20.1 (Parameter Scope): context 別の許可パラメータ集合で
        // 検証する。
        let table = self.locate_request(request_id);
        let context_allowed = match table {
            Some(RequestTable::Subscription) => Some((
                SUBSCRIPTION_UPDATE_ALLOWED_PARAMS,
                "REQUEST_UPDATE (subscription) parameter not allowed in this context",
            )),
            Some(RequestTable::Fetch) => Some((
                FETCH_UPDATE_ALLOWED_PARAMS,
                "REQUEST_UPDATE (fetch) parameter not allowed in this context",
            )),
            Some(RequestTable::NamespacePublication) => Some((
                NAMESPACE_PUBLICATION_UPDATE_ALLOWED_PARAMS,
                "REQUEST_UPDATE (namespace_publication) parameter not allowed",
            )),
            Some(RequestTable::NamespaceSubscription) => Some((
                NAMESPACE_SUBSCRIPTION_UPDATE_ALLOWED_PARAMS,
                "REQUEST_UPDATE (namespace_subscription) parameter not allowed",
            )),
            Some(RequestTable::TrackSubscription) => Some((
                TRACK_SUBSCRIPTION_UPDATE_ALLOWED_PARAMS,
                "REQUEST_UPDATE (track_subscription) parameter not allowed",
            )),
            Some(RequestTable::TrackStatus) | None => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "request_update not allowed for this request id",
                ));
            }
        };
        if let Some((allowed, message)) = context_allowed
            && parameters.validate_scope(allowed).is_err()
        {
            return Err(SessionError::new(SESSION_PROTOCOL_VIOLATION, message));
        }

        match table {
            Some(RequestTable::Subscription) => {
                self.send_update_for_subscription(request_id, &parameters)?
            }
            Some(RequestTable::Fetch) => self.send_update_for_fetch(request_id, &parameters)?,
            Some(RequestTable::NamespacePublication) => {
                self.send_update_for_namespace_publication(request_id)?
            }
            Some(RequestTable::NamespaceSubscription) => {
                self.send_update_for_namespace_subscription(request_id)?
            }
            Some(RequestTable::TrackSubscription) => {
                self.send_update_for_track_subscription(request_id)?
            }
            Some(RequestTable::TrackStatus) | None => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "request_update not allowed for this request id",
                ));
            }
        }
        // 全検証が通ったため送信が確定する。ここでクレジットを消費する
        // (検証失敗時は消費しない。クレジットの回復は REQUEST_OK / REQUEST_ERROR 応答受信時のみ)。
        if peer_max > 0 {
            *self.outgoing_request_updates.entry(request_id).or_insert(0) += 1;
        }
        self.start_control_message_deadline(request_id);
        let msg = ControlMessage::RequestUpdate(RequestUpdate {
            request_id,
            parameters,
        });
        self.events.push_back(SessionEvent::SendOnStream {
            request_id,
            message: msg,
            fin: false,
        });
        Ok(())
    }

    pub(crate) fn send_update_for_subscription(
        &mut self,
        request_id: u64,
        parameters: &MessageParameters,
    ) -> Result<(), SessionError> {
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        // draft §9.5 (REQUEST_UPDATE): REQUEST_UPDATE の送信は
        //   (a) 元 request の sender (= initiator) が送る (primary rule)
        //   (b) PUBLISH で確立された subscription に対して subscriber も送れる
        //       (exception: "A subscriber can also send REQUEST_UPDATE to
        //        modify parameters of a subscription established with PUBLISH.")
        // 2 ケースを合わせると「initiator 自身」または「自側が Subscriber」であれば OK。
        // 反対側 (SUBSCRIBE 経由の Publisher responder) は送信禁止。
        if !(subscription.is_initiator_self() || subscription.my_role == TrackRole::Subscriber) {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_update cannot be sent by publisher responder of a SUBSCRIBE",
            ));
        }
        // draft §3.1 (Subscriptions) / state diagram:
        // REQUEST_UPDATE は Established の self loop として定義されており、
        // Pending* / Terminated からの遷移は state machine に存在しない。
        if subscription.state != SubscriptionState::Established {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_update requires Established subscription",
            ));
        }
        // 値の検証をパラメータの適用より前に実行する。検証エラー時は `Err` のみを返し、
        // 適用済みパラメータを残留させない (ローカルと peer の subscription パラメータの
        // 乖離を防ぐ)。検証の順序 (forward → filter) と適用順序は旧実装から変えない。
        let new_forward = parameters.forward().map(validate_forward).transpose()?;
        // `location_filter_update()` は `Result<LocationFilterUpdate, MessageError>` を
        // 返し、`MessageError` から `SessionError` への `From` 実装はないため
        // `?` では伝播できない。エラーメッセージ・エラーコードは既存挙動から変えない
        let filter_update = match parameters.location_filter_update() {
            Ok(update) => update,
            Err(_) => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "invalid subscription filter encoding in REQUEST_UPDATE",
                ));
            }
        };
        // draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter):
        // FILL 内側の Table 6 スコープと LOCATION_FILTER も送信前に検証し、
        // 不正値の送出を防ぐ (楽観的状態更新より前に置き、エラー時に更新済み状態を残さない)
        super::fill::validate_outgoing_fill_parameters(parameters)?;
        update_subscription_subscriber_delivery_timeouts_if_present(subscription, parameters);
        if let Some(new_forward) = new_forward {
            subscription.forward_state = new_forward;
        }
        // draft-ietf-moq-transport-21 §3.3.1 (Location Filters) / §9.20.10 (LOCATION FILTER Parameter):
        // REQUEST_UPDATE に LOCATION_FILTER が含まれる場合は更新し、Length 0 なら削除する。
        // 省略時は値 unchanged。解決済み値もフィルタに追随させる。
        match filter_update {
            LocationFilterUpdate::Unchanged => {}
            LocationFilterUpdate::Removed => {
                subscription.filter = None;
                subscription.filter_start = None;
                subscription.filter_end = None;
            }
            LocationFilterUpdate::Set(filter) => {
                subscription.filter = Some(filter);
                // 解決済み値もフィルタに追随させる。相対フィルタは更新時点の largest で
                // 解決する (REQUEST_UPDATE_OK 応答時の pending 適用と対称)。
                let largest = subscription.largest_received_location;
                let (start, end) = resolve_location_filter(
                    subscription.filter.as_ref(),
                    largest.as_ref(),
                    LocationFilterContext::Subscription,
                );
                subscription.filter_start = start;
                subscription.filter_end = end;
            }
        }
        // draft-ietf-moq-transport-21 §9.20.8 (SUBSCRIBER PRIORITY Parameter): REQUEST_UPDATE に SUBSCRIBER_PRIORITY が含まれる場合は更新
        if let Some(priority) = parameters.subscriber_priority() {
            subscription.subscriber_priority = Some(priority);
        }
        // draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): GROUP_ORDER は確定後変更不可 (draft-ietf-moq-transport-21 §5.1.1 (Definitions)) のため更新しない
        Ok(())
    }

    /// PUBLISH_STATE_NOTIFY を送信する (自側が publisher)
    ///
    /// draft-ietf-moq-transport-21 §9.10 (PUBLISH_STATE_NOTIFY):
    /// subscriber 発 REQUEST_UPDATE 以外の理由で subscription 状態が変わったことを
    /// 通知する一方向メッセージ。応答を要求せず、MAX_REQUEST_UPDATES の
    /// クレジットも消費しない。LARGEST_OBJECT は観測済みなら必須
    /// (未公開なら省略可) のため、観測値がある場合は送出パラメータに自動で補う
    /// (補完値は wire のみで `largest_location` には保存しない)。
    /// FORWARD / LOCATION_FILTER は自側の subscription 状態にも反映する。
    ///
    /// 未応答の REQUEST_UPDATE と競合する FORWARD / filter 変更はアプリ側で
    /// 直列化すること。応答待ち UPDATE の累積パラメータは REQUEST_OK 応答時に
    /// 適用されるため、NOTIFY 送信後に OK を返すと両端の状態が乖離しうる。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fn send_publish_state_notify(
        &mut self,
        request_id: u64,
        mut parameters: MessageParameters,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        let subscription = self.subscriptions.get(&request_id).ok_or_else(|| {
            SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "subscription not found for publish_state_notify",
            )
        })?;
        if subscription.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "publish_state_notify can only be sent by publisher",
            ));
        }
        // draft §5.1 (Subscriptions): REQUEST_UPDATE と同様、確立済みの
        // subscription に対する状態通知のみ送れる。
        if subscription.state != SubscriptionState::Established {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "publish_state_notify requires Established subscription",
            ));
        }
        // 値の検証をパラメータの適用より前に実行する。検証エラー時は `Err` のみを返し、
        // 適用済みパラメータを残留させない (REQUEST_UPDATE 送信経路と同じ設計判断)。
        // draft §9.20.1 (Parameter Scope): スコープ外パラメータを含む PUBLISH_STATE_NOTIFY では
        // 状態を一切変更せずエラーを返す。受信側はワイヤ層で検証済みのため、
        // ここは送信側の状態整合性のための検証。なお本検証はパラメータのスコープのみを
        // 対象とし、スコープ内の同一型の重複出現・型不一致は検出されず encode 層で
        // 失敗する残存ギャップがある (SUBSCRIBE 送信側の検証と同じ制約)。
        if parameters
            .validate_scope(PUBLISH_STATE_NOTIFY_ALLOWED_PARAMS)
            .is_err()
        {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH_STATE_NOTIFY parameter not allowed in this context",
            ));
        }
        let new_forward = parameters.forward().map(validate_forward).transpose()?;
        // `location_filter_update()` は `Result<LocationFilterUpdate, MessageError>` を
        // 返し、`MessageError` から `SessionError` への `From` 実装はないため
        // `?` では伝播できない。
        let filter_update = match parameters.location_filter_update() {
            Ok(update) => update,
            Err(_) => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "invalid filter encoding in PUBLISH_STATE_NOTIFY",
                ));
            }
        };
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("subscription presence already checked above");
        if let Some(new_forward) = new_forward {
            subscription.forward_state = new_forward;
        }
        // draft-ietf-moq-transport-21 §3.3.1 (Location Filters) / §9.20.10 (LOCATION FILTER Parameter):
        // LOCATION_FILTER が含まれる場合は更新し、Length 0 なら削除する。
        // 省略時は値 unchanged。解決済み値もフィルタに追随させる。
        match filter_update {
            LocationFilterUpdate::Unchanged => {}
            LocationFilterUpdate::Removed => {
                subscription.filter = None;
                subscription.filter_start = None;
                subscription.filter_end = None;
            }
            LocationFilterUpdate::Set(filter) => {
                subscription.filter = Some(filter);
                let largest = subscription.largest_received_location;
                let (start, end) = resolve_location_filter(
                    subscription.filter.as_ref(),
                    largest.as_ref(),
                    LocationFilterContext::Subscription,
                );
                subscription.filter_start = start;
                subscription.filter_end = end;
            }
        }
        // draft-ietf-moq-transport-21 §9.10 (PUBLISH_STATE_NOTIFY) /
        // §9.20.18 (LARGEST OBJECT Parameter): LARGEST_OBJECT は観測済みなら必須の
        // ため、観測値がある場合は送出パラメータに補う (未公開なら省略可のまま残す)。
        // 自側 publisher の観測 Largest は `effective_largest_object` で求める。
        if let Some(ref effective) = effective_largest_object(subscription) {
            update_largest_object_in_parameters(&mut parameters, effective);
        }
        let msg = ControlMessage::PublishStateNotify(PublishStateNotify { parameters });
        self.events.push_back(SessionEvent::SendOnStream {
            request_id,
            message: msg,
            fin: false,
        });
        Ok(())
    }

    /// PUBLISH_DONE を送信する (publisher 側のみ)
    ///
    /// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE): subscription 終了を通知する最後のメッセージ。
    /// subscription state を Terminated に遷移させる。
    ///
    /// # Errors
    ///
    /// publisher 役でない、Publish Side の状態が Pending(Publisher) / Established でない、
    /// 未終端の outgoing data stream がある場合は `SESSION_PROTOCOL_VIOLATION` を返す。
    /// `stream_count` は `published_count == 0` のとき `0` のみ、`published_count > 0` のとき
    /// 追跡値との一致または `PUBLISH_DONE_STREAM_COUNT_UNKNOWN` のみを許可し、
    /// それ以外は `SESSION_PROTOCOL_VIOLATION` を返す。
    /// `status_code` にローカル専用コード (`SESSION_LOCAL_FILTER_MISMATCH` /
    /// `SESSION_LOCAL_DATAGRAM_TIMEOUT`) を渡した場合は `PUBLISH_DONE_INTERNAL_ERROR` に置換する。
    pub fn send_publish_done(
        &mut self,
        request_id: u64,
        status_code: u64,
        stream_count: u64,
        reason: ReasonPhrase,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        // ローカル専用コードが wire に流出しないよう、PUBLISH_DONE レジストリの
        // INTERNAL_ERROR に置換する
        let status_code = if is_local_error_code(status_code) {
            PUBLISH_DONE_INTERNAL_ERROR
        } else {
            status_code
        };
        let subscription = self.subscriptions.get(&request_id).ok_or_else(|| {
            SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "subscription not found for send_publish_done",
            )
        })?;
        if subscription.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "publish_done can only be sent by publisher",
            ));
        }
        // draft §3.1 (Subscriptions): "The publisher terminates a subscription in
        // the Pending (Publisher) or Established states by sending PUBLISH_DONE".
        // 自側 SUBSCRIBE responder の Pending(Subscriber) / Terminated からは送信不可。
        if !(subscription.is_pending_publisher()
            || subscription.state == SubscriptionState::Established)
        {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "publish_done requires Pending(Publisher) or Established state",
            ));
        }
        let published_stream_count = subscription.stream_counts.published_count;
        if self.has_open_outgoing_data_streams_for_request(request_id) {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "publish_done requires all outgoing streams closed",
            ));
        }
        // draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE):
        // - stream を 1 本も開かなかった場合 (published_stream_count == 0) は MUST 0
        // - published_stream_count > 0 のときは Session が tracking した exact 値との一致、
        //   または正確な数を表明できない場合の escape として
        //   `PUBLISH_DONE_STREAM_COUNT_UNKNOWN` (2^64 - 1) を許可する
        let stream_count_accepted = if published_stream_count == 0 {
            stream_count == 0
        } else {
            stream_count == published_stream_count
                || stream_count == PUBLISH_DONE_STREAM_COUNT_UNKNOWN
        };
        if !stream_count_accepted {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "publish_done stream_count does not match tracked data streams",
            ));
        }
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("subscription must exist while transitioning to Terminated by PUBLISH_DONE");
        subscription.state = SubscriptionState::Terminated;
        // draft-ietf-moq-transport-21 §3.4.1 (Opening and Closing Fill Fetch Streams):
        // subscription の終了時は open 中の fill fetch stream を reset する (MUST)。
        // PUBLISH_DONE の MUST NOT (全 stream 終端まで送信禁止) と両立するため、
        // 呼び出し側は本関数より前に fill stream を終端していること。
        // そのため本呼び出しは通常 no-op の防御であり、到達時は終端通知漏れの残骸掃除になる。
        self.reset_open_fill_streams(request_id);
        let msg = ControlMessage::PublishDone(PublishDone {
            status_code,
            stream_count,
            reason,
        });
        self.events.push_back(SessionEvent::SendOnStream {
            request_id,
            message: msg,
            // PUBLISH_DONE は subscription の最終メッセージのため送信後に FIN する (§9.9)
            fin: true,
        });
        Ok(())
    }
}
