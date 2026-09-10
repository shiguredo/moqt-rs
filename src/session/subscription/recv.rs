//! SUBSCRIBE / PUBLISH / PUBLISH_DONE / REQUEST_UPDATE / PUBLISH_STATE_NOTIFY の受信
//!
//! draft-ietf-moq-transport-21 §9.6-§9.9 と
//! draft-ietf-moq-transport-21 §9.10 (PUBLISH_STATE_NOTIFY) に対応。
//! 将来 draft 側で変更される可能性がある。

use crate::error::{
    REQUEST_DOES_NOT_EXIST, REQUEST_UNSUPPORTED_EXTENSION, SESSION_DUPLICATE_TRACK_ALIAS,
    SESSION_PROTOCOL_VIOLATION,
};
use crate::message::{
    FETCH_UPDATE_ALLOWED_PARAMS, NAMESPACE_PUBLICATION_UPDATE_ALLOWED_PARAMS,
    NAMESPACE_SUBSCRIPTION_UPDATE_ALLOWED_PARAMS, Publish, PublishDone, PublishStateNotify,
    RequestUpdate, SUBSCRIPTION_UPDATE_ALLOWED_PARAMS, Subscribe, SubscribeOk,
    TRACK_SUBSCRIPTION_UPDATE_ALLOWED_PARAMS, common::Location,
};
use crate::message_parameter::{
    LocationFilter, LocationFilterContext, LocationFilterUpdate, MessageParameters,
    PARAM_FILL_PARAMETERS,
};
use hashbrown::HashMap;

use super::super::core::{RequestTable, Session, alias_used_by_different_track};
use super::super::types::{
    DeadlineTimer, DeliveryTimeoutState, RequestKind, SessionError, SessionEvent, StreamCountState,
    Subscription, SubscriptionInitiator, SubscriptionRangeFilters, SubscriptionState,
    TerminationReason, TrackRole,
};
use super::delivery::{
    compute_effective_delivery_timeout_ms, effective_largest_object, record_publish_done,
    set_subscription_expires, set_subscription_publisher_object_delivery_timeout,
    set_subscription_publisher_subgroup_delivery_timeout,
};
use super::validation::{
    extract_range_filters, resolve_location_filter, validate_forward, validate_group_order,
    validate_include_properties,
};

impl Session {
    /// subscriber 側で QUIC STOP_SENDING を発行したことを通知する
    ///
    /// draft §3.1.1 (Subscription State Management): subscriber 主導の終了。subscription state を Terminated に遷移。
    /// session 層はイベントを発行しない (QUIC STOP_SENDING は I/O 層が実行済み)。
    pub fn stop_sending(&mut self, request_id: u64) -> Result<(), SessionError> {
        self.require_established()?;
        let subscription = self.subscriptions.get_mut(&request_id).ok_or_else(|| {
            SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "subscription not found for stop_sending",
            )
        })?;
        if subscription.my_role != TrackRole::Subscriber {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "stop_sending can only be issued by subscriber",
            ));
        }
        // draft §3.1 (Subscriptions): "The subscriber terminates a subscription in
        // the Pending (Subscriber) or Established states by sending STOP_SENDING".
        // 自側 PUBLISH responder の Pending(Publisher) / Terminated からは送信不可。
        if !(subscription.is_pending_subscriber()
            || subscription.state == SubscriptionState::Established)
        {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "stop_sending requires Pending(Subscriber) or Established state",
            ));
        }
        subscription.state = SubscriptionState::Terminated;
        Ok(())
    }

    // ─── 受信ハンドラ ──────────────────────────────────────

    pub(crate) fn handle_peer_subscribe(
        &mut self,
        subscribe: Subscribe,
    ) -> Result<(), SessionError> {
        let request_id = subscribe.request_id;
        if !self.accept_peer_request(request_id, &subscribe.parameters)? {
            return Ok(());
        }
        // draft §9.20.3 (AUTHORIZATION TOKEN Parameter): AUTHORIZATION_TOKEN Register/Delete/Use を peer cache に反映
        self.apply_peer_message_auth_tokens(&subscribe.parameters)?;
        // draft-ietf-moq-transport-21 §3.3.2 (Range Filters): MAX_FILTER_RANGES 超過は INVALID_FILTER で拒否
        if !self.check_incoming_range_filters(request_id, &subscribe.parameters) {
            return Ok(());
        }
        // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces): single period `.` 予約名前空間の SUBSCRIBE は拒否
        if subscribe.track_namespace.is_single_period() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "reserved single-period namespace",
            );
            return Ok(());
        }
        // draft §6.5 (Session-Level Tracks and Namespaces): .session 名前空間の SUBSCRIBE は内部処理
        // 未知のセッションレベルトラック/空トラック名は DOES_NOT_EXIST で拒否
        if subscribe.track_namespace.is_session_level() {
            let reason = if subscribe.track_name.is_empty() {
                "empty track name in .session namespace"
            } else {
                "session-level track does not exist"
            };
            self.emit_request_error(request_id, REQUEST_DOES_NOT_EXIST, reason);
            return Ok(());
        }
        let key = (
            subscribe.track_namespace.clone(),
            subscribe.track_name.clone(),
            TrackRole::Publisher,
        );
        // draft-ietf-moq-transport-21 §3.1.1: 同一 Track への複数同時 subscription が許可されたため、
        // DUPLICATE_SUBSCRIPTION による拒否は行わない。
        // draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter): FORWARD パラメータの
        // 抽出 (不在時はデフォルト 1)。値域外 (0 / 1 以外) の受信は MUST でセッションを
        // PROTOCOL_VIOLATION により閉じる (GROUP_ORDER の値域検証と同じパターン。`?`
        // 伝播のみだとセッションが開いたまま残る)。検証の `Ok` 値を `forward_state` の
        // 設定に使い、`forward()` の検索結果を検証に再利用する (検証を呼び出しと分離し、
        // 同じパラメータを 2 回解釈しない)。
        let forward_state = match subscribe.parameters.forward() {
            Some(f) => match validate_forward(f) {
                Ok(validated) => validated,
                Err(err) => {
                    self.fail(err.clone());
                    return Err(err);
                }
            },
            None => 1,
        };
        let subscriber_object_delivery_timeout_ms = subscribe.parameters.object_delivery_timeout();
        let subscriber_subgroup_delivery_timeout_ms =
            subscribe.parameters.subgroup_delivery_timeout();
        let subscriber_rendezvous_timeout_ms = subscribe.parameters.rendezvous_timeout();
        let filter = match subscribe.parameters.location_filter_update() {
            Ok(LocationFilterUpdate::Set(filter)) => Some(filter),
            // 省略時と Length 0 (no filter) はどちらも unfiltered として扱う
            Ok(_) => None,
            Err(_) => {
                // draft-ietf-moq-transport-21 §3.3.1 (Location Filters): End Group 溢出は
                // MUST close the session with PROTOCOL_VIOLATION。壊れた値は §8.3 の
                // KEY_VALUE_FORMATTING_ERROR だが、セッション層では同じく閉じる
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "invalid subscription filter encoding",
                );
                self.fail(err.clone());
                return Err(err);
            }
        };
        let subscriber_priority = subscribe.parameters.subscriber_priority();
        let group_order = subscribe.parameters.group_order();
        // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
        // 値域外 (0 / 1 以外) の受信は MUST でセッションを PROTOCOL_VIOLATION により閉じる。
        let include_properties = subscribe.parameters.include_properties();
        if let Some(v) = include_properties
            && let Err(err) = validate_include_properties(v)
        {
            self.fail(err.clone());
            return Err(err);
        }
        // draft §10.4 / §10.5: SUBSCRIBE には TrackProperties が含まれない。Publisher である
        // 自側が send_subscribe_ok で発行するタイミングで DEFAULT_PUBLISHER_* を設定する。
        let default_publisher_priority = None;
        let default_publisher_group_order = None;
        // draft §3.3.1 (Location Filters) / §3.3.2 (Range Filters): subscriber が SUBSCRIBE で
        // 指定したフィルタを publisher 側の状態として取り込む。新規 subscription は解決時点の
        // largest を持たないため、同一 track の既存 publisher 役 subscription が観測した
        // 最大位置 (なければ先頭) で相対フィルタを解決する。
        // 仕様の Largest Object は publisher がメッセージを処理する視点で定義されるため、
        // 未配信 track では `None` 基準 (先頭から) になる。
        // この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
        let track_largest = self
            .aliases
            .subscriptions_by_track
            .get(&key)
            .into_iter()
            .flatten()
            .filter_map(|id| self.subscriptions.get(id))
            .filter(|sub| sub.my_role == TrackRole::Publisher)
            .filter_map(effective_largest_object)
            .max();
        let (filter_start, filter_end) = resolve_location_filter(
            filter.as_ref(),
            track_largest.as_ref(),
            LocationFilterContext::Subscription,
        );
        let range_filters = extract_range_filters(&subscribe.parameters);
        if let Some(order) = group_order {
            // draft §9.20.9 (GROUP ORDER Parameter): 値域外の受信はセッションを閉じる
            if let Err(err) = validate_group_order(order) {
                self.fail(err.clone());
                return Err(err);
            }
        }
        let subscription = Subscription {
            request_id,
            initiator: SubscriptionInitiator::Subscriber,
            my_role: TrackRole::Publisher,
            track_namespace: subscribe.track_namespace,
            track_name: subscribe.track_name,
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
            // SUBSCRIBE には TrackProperties は含まれない。Publisher である自側が
            // send_subscribe_ok で TrackProperties を発行するタイミングで dynamic_groups を設定する。
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
            // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
            // peer が SUBSCRIBE で指定した値を保持し、SUBSCRIBE_OK 送信時に参照する
            include_properties,
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
        // draft-ietf-moq-transport-21 §3.4.1 (Opening and Closing Fill Fetch Streams):
        // FILL_PARAMETERS 付き SUBSCRIBE を Forward State 1 で処理したら
        // fill fetch stream を開く。fill range が empty または Largest Object より
        // 後に始まる場合・ Largest Object 未知の場合は開設しない。
        self.maybe_open_fill_stream(request_id, &subscribe.parameters, filter, forward_state);
        Ok(())
    }

    pub(crate) fn handle_peer_publish(&mut self, publish: Publish) -> Result<bool, SessionError> {
        let request_id = publish.request_id;
        if !self.accept_peer_request(request_id, &publish.parameters)? {
            return Ok(false);
        }
        // draft §9.20.3 (AUTHORIZATION TOKEN Parameter): AUTHORIZATION_TOKEN Register/Delete/Use を peer cache に反映
        self.apply_peer_message_auth_tokens(&publish.parameters)?;
        // draft-ietf-moq-transport-21 §3.3.2 (Range Filters): MAX_FILTER_RANGES 超過は INVALID_FILTER で拒否
        if !self.check_incoming_range_filters(request_id, &publish.parameters) {
            return Ok(false);
        }
        // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces): single period `.` 予約名前空間の PUBLISH は拒否
        if publish.track_namespace.is_single_period() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "reserved single-period namespace",
            );
            return Ok(false);
        }
        // draft §6.5 (Session-Level Tracks and Namespaces): .session 名前空間の PUBLISH は内部処理
        // 未知のセッションレベルトラックは DOES_NOT_EXIST で拒否
        if publish.track_namespace.is_session_level() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "session-level track does not exist",
            );
            return Ok(false);
        }
        // draft §3.6 (Mandatory Track Properties): 未知の必須トラックプロパティを含む PUBLISH は
        // REQUEST_ERROR (UNSUPPORTED_EXTENSION) で拒否する
        if publish.track_properties.has_unknown_mandatory() {
            self.emit_request_error(
                request_id,
                REQUEST_UNSUPPORTED_EXTENSION,
                "unsupported mandatory extension",
            );
            return Ok(false);
        }
        // draft §3.1.2 (Track Alias): "If a subscriber receives a PUBLISH or SUBSCRIBE_OK that
        // uses the same Track Alias as a different Track with an Established subscription, it
        // MUST close the session with error DUPLICATE_TRACK_ALIAS."
        // 同一 Track への共有は §3.1 が許可しており、Pending 状態との衝突は MUST の条件外。
        if alias_used_by_different_track(
            &self.aliases.peer_publisher_aliases,
            &self.subscriptions,
            publish.track_alias,
            &publish.track_namespace,
            &publish.track_name,
            None,
            true,
        ) {
            let err = SessionError::new(
                SESSION_DUPLICATE_TRACK_ALIAS,
                "peer publisher reused track alias for a different track",
            );
            self.fail(err.clone());
            return Err(err);
        }
        let key = (
            publish.track_namespace.clone(),
            publish.track_name.clone(),
            TrackRole::Subscriber,
        );
        // draft-ietf-moq-transport-21 §3.1.1: 同一 Track への複数同時 subscription が許可されたため、
        // DUPLICATE_SUBSCRIPTION による拒否は行わない。
        // ただし Pending(Subscriber) に対する PUBLISH は SUBSCRIBE と PUBLISH が交差した場合の
        // 救済措置として既存 subscription を Terminated に遷移させる (draft §3.1.1)。
        if let Some(ids) = self.aliases.subscriptions_by_track.get(&key) {
            for &existing_id in ids {
                let is_pending_subscriber = self
                    .subscriptions
                    .get(&existing_id)
                    .is_some_and(|s| s.is_pending_subscriber());
                if is_pending_subscriber {
                    self.supersede_pending_subscriber(existing_id, request_id);
                    break;
                }
            }
        }
        // draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter): FORWARD パラメータの
        // 抽出 (不在時はデフォルト 1)。値域外 (0 / 1 以外) の受信は MUST でセッションを
        // PROTOCOL_VIOLATION により閉じる (GROUP_ORDER の値域検証と同じパターン。`?`
        // 伝播のみだとセッションが開いたまま残る)。検証の `Ok` 値を `forward_state` の
        // 設定に使い、`forward()` の検索結果を検証に再利用する (検証を呼び出しと分離し、
        // 同じパラメータを 2 回解釈しない)。
        let forward_state = match publish.parameters.forward() {
            Some(f) => match validate_forward(f) {
                Ok(validated) => validated,
                Err(err) => {
                    self.fail(err.clone());
                    return Err(err);
                }
            },
            None => 1,
        };
        // draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): 値域外の受信は
        // MUST でセッションを PROTOCOL_VIOLATION により閉じる。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        let group_order = publish.parameters.group_order();
        if let Some(order) = group_order
            && let Err(err) = validate_group_order(order)
        {
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §9.8 (PUBLISH) / §9.20.10 (LOCATION FILTER Parameter):
        // PUBLISH の Parameters は initial subscription parameters として保持する。
        // Length 0 と省略はどちらも unfiltered として扱う。新規 PUBLISH は解決時点の
        // largest を持たないため、相対フィルタは先頭から解決される。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        let filter = match publish.parameters.location_filter_update() {
            Ok(LocationFilterUpdate::Set(filter)) => Some(filter),
            Ok(_) => None,
            Err(_) => {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "invalid publish filter encoding",
                );
                self.fail(err.clone());
                return Err(err);
            }
        };
        let subscriber_priority = publish.parameters.subscriber_priority();
        // delivery timeout の Parameters は状態に保持しない。§8 の規則
        // (publisher は Track Property、subscriber は SUBSCRIBE / REQUEST_UPDATE の
        // Message Parameters で伝える) に従い、publisher 側値は Track Properties、
        // subscriber 側値は REQUEST_UPDATE 経路が担う。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        let publisher_object_delivery_timeout_ms =
            publish.track_properties.object_delivery_timeout();
        let publisher_subgroup_delivery_timeout_ms =
            publish.track_properties.subgroup_delivery_timeout();
        let expires = publish
            .parameters
            .expires()
            .map(|expires_ms| DeadlineTimer::new(expires_ms, self.timing.last_tick_ms));
        // draft-ietf-moq-transport-21 §10.6 (DYNAMIC GROUPS) / §9.20.20 (NEW GROUP REQUEST Parameter): peer が発行した PUBLISH の TrackProperties に
        // DYNAMIC_GROUPS=1 があれば記録する。自側が (responder として) REQUEST_UPDATE 応答可否を
        // 判断する際の NEW_GROUP_REQUEST 検証の材料となる。
        let dynamic_groups = publish.track_properties.dynamic_groups() == Some(1);
        // draft §10.4 (DEFAULT PUBLISHER PRIORITY) / §10.5 (DEFAULT PUBLISHER GROUP ORDER)
        let default_publisher_priority = publish.track_properties.default_publisher_priority();
        let default_publisher_group_order =
            publish.track_properties.default_publisher_group_order();
        // draft §3.3.1 (Location Filters): 解決済み値を求める。
        // 新規 PUBLISH は解決時点の largest を持たないため、相対フィルタは
        // 先頭から解決される。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        let (filter_start, filter_end) =
            resolve_location_filter(filter.as_ref(), None, LocationFilterContext::Subscription);
        let range_filters = SubscriptionRangeFilters::default();
        // draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): GROUP_ORDER は SUBSCRIBE / PUBLISH /
        // SUBSCRIBE_TRACKS / FETCH に出現可能。PUBLISH には §9.18.1 (Parameters on
        // SUBSCRIBE_TRACKS) の伝播として含めることができる。
        // 受信した PUBLISH の GROUP_ORDER は本処理で値域検証済みであり、初期状態として保持する。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        let subscription = Subscription {
            request_id,
            initiator: SubscriptionInitiator::Publisher,
            my_role: TrackRole::Subscriber,
            track_namespace: publish.track_namespace,
            track_name: publish.track_name,
            track_alias: Some(publish.track_alias),
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
            subscriber_priority,
            group_order,
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
        self.register_peer_alias(publish.track_alias, request_id);
        // draft §3.1 (Subscriptions) / draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter):
        // PUBLISH の parameters に LARGEST_OBJECT があれば広告値として保存する
        if let Some((group_id, object_id)) = publish.parameters.largest_object()
            && let Some(sub) = self.subscriptions.get_mut(&request_id)
        {
            sub.largest_location = Some(Location {
                group_id,
                object_id,
            });
        }
        Ok(true)
    }

    pub(crate) fn handle_peer_subscribe_ok(
        &mut self,
        request_id: u64,
        ok: SubscribeOk,
    ) -> Result<(), SessionError> {
        let now_ms = self.timing.last_tick_ms;
        let Some(subscription) = self.subscriptions.get_mut(&request_id) else {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "SUBSCRIBE_OK received for unknown request id",
            );
            self.fail(err.clone());
            return Err(err);
        };
        if subscription.my_role != TrackRole::Subscriber || !subscription.is_pending_subscriber() {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "SUBSCRIBE_OK in unexpected subscription state",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft §3.1.2 (Track Alias): 異なる Track の Established subscription と衝突する場合のみ
        // DUPLICATE_TRACK_ALIAS でセッションを閉じる。同一 Track への alias 共有は §3.1 が許可。
        let (ns_for_alias, name_for_alias) = (
            subscription.track_namespace.clone(),
            subscription.track_name.clone(),
        );
        if alias_used_by_different_track(
            &self.aliases.peer_publisher_aliases,
            &self.subscriptions,
            ok.track_alias,
            &ns_for_alias,
            &name_for_alias,
            Some(request_id),
            true,
        ) {
            let err = SessionError::new(
                SESSION_DUPLICATE_TRACK_ALIAS,
                "peer publisher reused track alias in SUBSCRIBE_OK for a different track",
            );
            self.fail(err.clone());
            return Err(err);
        }
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("subscription presence already checked above");
        // draft §3.6 (Mandatory Track Properties): 未知の必須プロパティを含む SUBSCRIBE_OK は購読をキャンセルする
        if ok.track_properties.has_unknown_mandatory() {
            subscription.state = SubscriptionState::Terminated;
            self.clear_control_message_deadline(request_id);
            self.events.push_back(SessionEvent::RequestTerminated {
                request_id,
                kind: RequestKind::Subscribe,
                reason: TerminationReason::LocalCancel,
            });
            return Ok(());
        }
        self.clear_control_message_deadline(request_id);
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("validated above");
        subscription.track_alias = Some(ok.track_alias);
        subscription.state = SubscriptionState::Established;
        // draft §3.1 (Subscriptions) / §9.20.18 (LARGEST OBJECT Parameter):
        // LARGEST_OBJECT parameter があれば広告値として保存する
        if let Some((group_id, object_id)) = ok.parameters.largest_object() {
            subscription.largest_location = Some(Location {
                group_id,
                object_id,
            });
        }
        // draft-ietf-moq-transport-21 §10.6 (DYNAMIC GROUPS) / §9.20.20 (NEW GROUP REQUEST Parameter): SUBSCRIBE_OK で peer publisher が発行する
        // DYNAMIC_GROUPS を記録する (自側が subscriber なので REQUEST_UPDATE 送信時の
        // NEW_GROUP_REQUEST 可否を判断するために使う)
        subscription.dynamic_groups = ok.track_properties.dynamic_groups() == Some(1);
        // draft §10.4 (DEFAULT PUBLISHER PRIORITY) / §10.5 (DEFAULT PUBLISHER GROUP ORDER)
        subscription.default_publisher_priority = ok.track_properties.default_publisher_priority();
        subscription.default_publisher_group_order =
            ok.track_properties.default_publisher_group_order();
        set_subscription_publisher_object_delivery_timeout(
            subscription,
            ok.track_properties.object_delivery_timeout(),
        );
        set_subscription_publisher_subgroup_delivery_timeout(
            subscription,
            ok.track_properties.subgroup_delivery_timeout(),
        );
        set_subscription_expires(subscription, ok.parameters.expires(), now_ms);
        // draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): GROUP_ORDER は SUBSCRIBE / SUBSCRIBE_TRACKS / FETCH に含まれる。
        // SUBSCRIBE_OK には含まれないため、group_order は SUBSCRIBE 送信時の値のまま保持される。
        self.register_peer_alias(ok.track_alias, request_id);
        // PUBLISH_OK 経路 (dispatch.rs) と対称に、SUBSCRIBE_OK による確立をアプリケーションに通知する
        self.events.push_back(SessionEvent::RequestOkReceived {
            request_id,
            request_kind: RequestKind::Subscribe,
            parameters: ok.parameters,
        });
        Ok(())
    }

    pub(crate) fn handle_peer_request_update(
        &mut self,
        request_id: u64,
        update: RequestUpdate,
    ) -> Result<(), SessionError> {
        // RequestUpdate wire format の request_id と stream context の request_id は
        // 一致していなければならない。mismatch は PROTOCOL_VIOLATION
        if update.request_id != request_id {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_UPDATE request_id mismatch with stream context",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES): peer からの outstanding REQUEST_UPDATE 数が
        // 自側 SETUP で宣言した MAX_REQUEST_UPDATES を超えたら TOO_MANY_REQUEST_UPDATES でセッションを閉じる。
        // デフォルト値 0 は無制限を意味する。
        let local_max = self
            .setup
            .local
            .as_ref()
            .and_then(|s| s.options.max_request_updates())
            .unwrap_or(0);
        if local_max > 0 {
            let count = self.incoming_request_updates.entry(request_id).or_insert(0);
            *count += 1;
            if *count > local_max {
                let err = SessionError::new(
                    crate::error::SESSION_TOO_MANY_REQUEST_UPDATES,
                    "peer exceeded MAX_REQUEST_UPDATES",
                );
                self.fail(err.clone());
                return Err(err);
            }
        }
        // draft §9.20.3 (AUTHORIZATION TOKEN Parameter): AUTHORIZATION_TOKEN Register/Delete/Use を peer cache に反映
        self.apply_peer_message_auth_tokens(&update.parameters)?;
        // draft §9.20.1 (Parameter Scope): context 別の許可パラメータ集合で
        // 検証する。違反時は PROTOCOL_VIOLATION でセッションをクローズする (MUST)。
        //
        // この検証は Range Filter の内容検証より先に行う。Range Filter を許可しない
        // context (FETCH_UPDATE_ALLOWED_PARAMS など) に Range Filter が来た場合、
        // 内容検証を先に走らせると §3.3.2 の INVALID_FILTER (REQUEST_ERROR で継続) が
        // 発火してしまい、§9.20.1 が MUST とするセッションクローズに到達しない。
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
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_UPDATE received for unknown or unsupported request id",
                );
                self.fail(err.clone());
                return Err(err);
            }
        };
        if let Some((allowed, message)) = context_allowed
            && update.parameters.validate_scope(allowed).is_err()
        {
            let err = SessionError::new(SESSION_PROTOCOL_VIOLATION, message);
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §3.3.2 (Range Filters): MAX_FILTER_RANGES 超過は INVALID_FILTER で拒否。
        // スコープ上正当な Range Filter に対してのみ内容検証を行う。
        if !self.check_incoming_range_filters(request_id, &update.parameters) {
            return Ok(());
        }
        match table {
            Some(RequestTable::Subscription) => {
                self.handle_update_for_subscription(request_id, update.parameters)
            }
            Some(RequestTable::Fetch) => {
                self.handle_update_for_fetch(request_id, update.parameters)
            }
            Some(RequestTable::NamespacePublication) => {
                self.handle_update_for_namespace_publication(request_id, update.parameters)
            }
            Some(RequestTable::NamespaceSubscription) => {
                self.handle_update_for_namespace_subscription(request_id, update.parameters)
            }
            Some(RequestTable::TrackSubscription) => {
                self.handle_update_for_track_subscription(request_id, update.parameters)
            }
            Some(RequestTable::TrackStatus) | None => {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_UPDATE received for unknown or unsupported request id",
                );
                self.fail(err.clone());
                Err(err)
            }
        }
    }

    pub(crate) fn handle_update_for_subscription(
        &mut self,
        request_id: u64,
        parameters: MessageParameters,
    ) -> Result<(), SessionError> {
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        // draft §3.1 (Subscriptions) / state diagram:
        // REQUEST_UPDATE は Established の self loop。Pending* / Terminated で
        // 受信した場合は PROTOCOL_VIOLATION としてセッションを閉じる。
        if subscription.state != SubscriptionState::Established {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_UPDATE requires Established subscription",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft §9.5 (REQUEST_UPDATE): REQUEST_UPDATE を送れる側は
        //   (a) 元 request の initiator (= 自側は responder)
        //   (b) PUBLISH 経由の subscription では subscriber も送れる
        // 自側から見ると: peer が initiator (= !is_initiator_self) か、
        // または peer が subscriber (= 自側 my_role == Publisher) なら受理できる。
        // それ以外 (自側 initiator かつ my_role == Subscriber = SUBSCRIBE 経由で
        // peer が publisher) での受信は PROTOCOL_VIOLATION。
        if subscription.is_initiator_self() && subscription.my_role != TrackRole::Publisher {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_UPDATE cannot be received by subscriber initiator",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §9.20.20 (NEW GROUP REQUEST Parameter): subscriber MUST NOT send NEW_GROUP_REQUEST in
        // REQUEST_UPDATE if Track did not include DYNAMIC_GROUPS Property with value 1.
        if parameters.new_group_request().is_some() && !subscription.dynamic_groups {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "NEW_GROUP_REQUEST in REQUEST_UPDATE without DYNAMIC_GROUPS=1 track",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter): 値域外 (0 / 1 以外) の
        // 受信は MUST でセッションを PROTOCOL_VIOLATION により閉じる。検証がないと
        // `pending_update_params` に保存され、応答送信時 (REQUEST_UPDATE_OK 分岐) に初めて
        // エラーになる遅延検証になる。検証はここで行い、累積 range filter 超過チェック
        // (下記の `merged.count_range_filters()`) より前に置く (複合違反時は §9.20.19 の
        // MUST に従い FORWARD のセッションクローズが優先される)。
        // `?` 伝播のみだとセッション state が Closing に遷移せず開いたまま残る
        // (GROUP_ORDER の値域検証と同じパターンで `self.fail()` に接続する)。
        if let Some(f) = parameters.forward()
            && let Err(err) = validate_forward(f)
        {
            self.fail(err.clone());
            return Err(err);
        }
        // draft §9.5.1 (Updating Subscriptions): REQUEST_UPDATE のパラメータを合体 (coalesce)
        // する。後続の REQUEST_UPDATE が届くたびに pending_update_params へマージし
        // (後の値が前を上書き)、send_request_ok 応答時に累積パラメータを適用する。
        //
        // `merge_from` は破壊的なのでコピー上でマージし、累積の上限検証を通ってから確定させる。
        // 上限違反で拒否したのに累積側が更新されていると、以降の正当な REQUEST_UPDATE も
        // 超過状態のまま評価されてしまう。
        let merged = match subscription.pending_update_params.as_ref() {
            Some(pending) => {
                let mut copy = pending.clone();
                copy.merge_from(&parameters);
                copy
            }
            None => parameters.clone(),
        };
        // draft-ietf-moq-transport-21 §3.3.2 / §9.1.6 (MAX FILTER RANGES): 上限は
        // "the total number of Ranges ... allowed concurrently in all Range filter parameters
        // for a given subscription or fetch" であり、1 メッセージ単位ではなく subscription 単位の
        // 同時保持数である。`merge_from` は Range Filter を型単位で全置換するので同一型では
        // 累積しないが、型をまたぐと累積するため、マージ後の総数を再検証する。
        if merged.has_range_filters()
            && merged.count_range_filters() > self.local_max_filter_ranges()
        {
            self.emit_request_error(
                request_id,
                crate::error::REQUEST_INVALID_FILTER,
                "cumulative Range Filters exceed MAX_FILTER_RANGES",
            );
            return Ok(());
        }
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        // draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter):
        // FILL_PARAMETERS は運んできたメッセージにのみ適用され subscription 状態として
        // 保持されない (sticky 対象外) ため、累積パラメータへの保持から除外する。
        // アプリへ通知する累積表示 (`merged`) には残す。
        let mut pending = merged.clone();
        pending.remove_type(PARAM_FILL_PARAMETERS);
        subscription.pending_update_params = Some(pending);
        // draft-ietf-moq-transport-21 §3.4.1 (Opening and Closing Fill Fetch Streams):
        // FILL_PARAMETERS 付き REQUEST_UPDATE を処理したら fill fetch stream の
        // 開設を判定する。処理後観点の forward と filter で評価する。
        let post_forward = merged.forward().unwrap_or(subscription.forward_state);
        let post_filter: Option<LocationFilter> = match merged.location_filter_update() {
            Ok(LocationFilterUpdate::Set(filter)) => Some(filter),
            // Length 0 はフィルタ削除、省略時は現 filter を使う
            Ok(LocationFilterUpdate::Removed) => None,
            Ok(LocationFilterUpdate::Unchanged) => subscription.filter,
            // デコード済みメッセージでは到達しない。安全側に現 filter を使う。
            Err(_) => subscription.filter,
        };
        // draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): GROUP_ORDER は確定後変更不可 (draft-ietf-moq-transport-21 §5.1.1 (Definitions)) のため更新しない
        self.events.push_back(SessionEvent::RequestUpdateReceived {
            request_id,
            parameters: merged,
        });
        self.maybe_open_fill_stream(request_id, &parameters, post_filter, post_forward);
        Ok(())
    }

    /// peer publisher からの PUBLISH_STATE_NOTIFY を受信する
    ///
    /// draft-ietf-moq-transport-21 §9.10 (PUBLISH_STATE_NOTIFY):
    /// publisher のみが subscription の bidi stream 上で送る一方向通知であり、
    /// 応答は不要、MAX_REQUEST_UPDATES のクレジットも消費しない。
    /// subscription 以外への受信・ subscriber からの受信は MUST でセッションを
    /// PROTOCOL_VIOLATION により閉じる。FORWARD / LOCATION_FILTER /
    /// LARGEST_OBJECT は subscription 状態に即時反映する。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub(crate) fn handle_peer_publish_state_notify(
        &mut self,
        request_id: u64,
        notify: PublishStateNotify,
    ) -> Result<(), SessionError> {
        let Some(subscription) = self.subscriptions.get(&request_id) else {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH_STATE_NOTIFY received for unknown or unsupported request id",
            );
            self.fail(err.clone());
            return Err(err);
        };
        // draft-ietf-moq-transport-21 §9.10 (PUBLISH_STATE_NOTIFY):
        // subscription 以外・ subscriber からの受信は PROTOCOL_VIOLATION (MUST)。
        // `subscriptions` 索引に無い request (fetch / namespace 系等) は上記で弾かれる。
        if subscription.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH_STATE_NOTIFY received on publisher side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft §3.1 (Subscriptions): REQUEST_UPDATE と同様、確立済みの
        // subscription に対する通知のみ受理する。
        if subscription.state != SubscriptionState::Established {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH_STATE_NOTIFY requires Established subscription",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter): 値域外 (0 / 1 以外) の
        // 受信は MUST でセッションを PROTOCOL_VIOLATION により閉じる。
        if let Some(f) = notify.parameters.forward()
            && let Err(err) = validate_forward(f)
        {
            self.fail(err.clone());
            return Err(err);
        }
        // LOCATION_FILTER の形式検証を受信状態の適用より前に行う。`MessageError` から
        // `SessionError` への `From` 実装はないため `?` では伝播できない。
        let filter_update = match notify.parameters.location_filter_update() {
            Ok(update) => update,
            Err(_) => {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "invalid filter encoding in PUBLISH_STATE_NOTIFY",
                );
                self.fail(err.clone());
                return Err(err);
            }
        };
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("subscription presence already checked above");
        if let Some(f) = notify.parameters.forward() {
            subscription.forward_state = f;
        }
        // draft-ietf-moq-transport-21 §3.3.1 (Location Filters): Length 0 は
        // フィルタ削除、省略時は値 unchanged。解決済み値もフィルタに追随させる。
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
        // draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter):
        // 通知された Largest Object を広告値として保存する。
        if let Some((group_id, object_id)) = notify.parameters.largest_object() {
            subscription.largest_location = Some(Location {
                group_id,
                object_id,
            });
        }
        self.events
            .push_back(SessionEvent::PublishStateNotifyReceived {
                request_id,
                parameters: notify.parameters,
            });
        Ok(())
    }

    pub(crate) fn handle_peer_publish_done(
        &mut self,
        request_id: u64,
        done: PublishDone,
    ) -> Result<(), SessionError> {
        let now_ms = self.timing.last_tick_ms;
        let Some(subscription) = self.subscriptions.get_mut(&request_id) else {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH_DONE received for unknown request id",
            );
            self.fail(err.clone());
            return Err(err);
        };
        if subscription.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH_DONE received on publisher side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft §3.1 (Subscriptions): publisher は Pending (Publisher) / Established
        // からしか PUBLISH_DONE を送れない。したがって peer が SUBSCRIBE の
        // initiator であり自側が publisher responder となる Pending(Subscriber) からの
        // PUBLISH_DONE 受信は PROTOCOL_VIOLATION。
        //
        // ただし REQUEST_UPDATE 失敗応答経路の `Terminated` (draft §3.1.1 の
        // REQUEST_ERROR 受信で subscription state を終える帰結として
        // `handle_err_for_subscription` の Established 分岐で遷移) には、
        // publisher が §9.5.1 の MUST に基づき送る PUBLISH_DONE(UPDATE_FAILED) が続く。
        // これを拒否するとセッションが PROTOCOL_VIOLATION で閉じられる回帰になるため、
        // 未記録 (`publish_done` が `None`) の `Terminated` に届く PUBLISH_DONE を受理する。
        // 受理条件はデータプレーン吸収 `is_cancelled_terminated` (`src/session/data.rs`) と同一で、
        // 記録済み (`publish_done` が `Some`) への重複 PUBLISH_DONE は従来どおり拒否される。
        // draft §3.1.1: "A REQUEST_ERROR indicates no objects will be delivered" は
        // publisher 側の送信禁止であり、REQUEST_ERROR 後に届く PUBLISH_DONE の drain 期間に
        // オブジェクトが届く実害はない。
        let allow_publish_done = subscription.is_pending_publisher()
            || subscription.state == SubscriptionState::Established
            || (subscription.state == SubscriptionState::Terminated
                && subscription.publish_done.is_none());
        if !allow_publish_done {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH_DONE requires Pending(Publisher), Established, or unrecorded Terminated state",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // Pending(Publisher) / Established / Terminated (未記録) のいずれも Terminated に確定させる
        subscription.state = SubscriptionState::Terminated;
        record_publish_done(subscription, &done, now_ms);
        self.events.push_back(SessionEvent::PublishDoneReceived {
            request_id,
            status_code: done.status_code,
            stream_count: done.stream_count,
            reason: done.reason,
        });
        Ok(())
    }

    // ─── REQUEST_OK / REQUEST_ERROR per-kind dispatch ──────────
}
