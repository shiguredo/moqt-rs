//! SUBSCRIBE / PUBLISH / PUBLISH_DONE / REQUEST_UPDATE / PUBLISH_STATE_NOTIFY の受信
//!
//! draft-ietf-moq-transport-23 §9.6-§9.9 と
//! draft-ietf-moq-transport-23 §9.10 (PUBLISH_STATE_NOTIFY) に対応。
//! 将来 draft 側で変更される可能性がある。

use crate::error::{
    REQUEST_DOES_NOT_EXIST, REQUEST_INVALID_FILTER, REQUEST_MALFORMED_AUTH_TOKEN,
    REQUEST_UNSUPPORTED_EXTENSION, SESSION_DUPLICATE_TRACK_ALIAS, SESSION_INVALID_PARAMETER,
    SESSION_PROTOCOL_VIOLATION, SESSION_ROLE_VIOLATION, SESSION_TOO_MANY_REQUEST_UPDATES,
    SESSION_UNEXPECTED_MESSAGE, STREAM_CANCELLED,
};
use crate::message::{
    ErrorDetail, ErrorDetails, FETCH_UPDATE_ALLOWED_PARAMS, Publish, PublishDone,
    PublishStateNotify, ReasonPhrase, RequestUpdate, SUBSCRIPTION_UPDATE_ALLOWED_PARAMS, Subscribe,
    SubscribeOk, UnsupportedProperty,
    common::{Location, TrackNamespace},
};
use crate::message_parameter::{
    LocationFilter, LocationFilterContext, LocationFilterUpdate, MessageParameters,
    PARAM_FILL_PARAMETERS,
};
use alloc::collections::VecDeque;
use hashbrown::HashMap;

use super::super::core::{
    RequestTable, Session, alias_used_by_different_track, range_filter_usage,
};
use super::super::types::{
    DeadlineTimer, DeliveryTimeoutState, DynamicGroupsSupport, MAX_PENDING_UPDATE_GROUPS,
    PendingUpdateGroup, PendingUpdateRejection, RequestKind, SessionError, SessionEvent,
    StreamCountState, Subscription, SubscriptionInitiator, SubscriptionRangeFilters,
    SubscriptionState, TerminationReason, TrackRole,
};
use super::delivery::{
    compute_effective_delivery_timeout_ms, record_publish_done, set_subscription_expires,
    set_subscription_publisher_object_delivery_timeout,
    set_subscription_publisher_subgroup_delivery_timeout,
};
use super::validation::{
    extract_range_filters, resolve_location_filter, validate_group_order,
    validate_include_properties, validate_paused,
};

impl Session {
    /// subscriber 側で QUIC STOP_SENDING を発行したことを通知する
    ///
    /// draft §3.1.2 (Subscription State Management): subscriber 主導の終了。subscription state を Terminated に遷移。
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
        // subscriber 主導の cancel で購読が終端したため、未応答の REQUEST_UPDATE と
        // MAX_REQUEST_UPDATES のクレジットを解放する
        // (draft-ietf-moq-transport-23 §6.4.2.3 (Request Cancellation and Rejection))。
        self.release_request_update_state(request_id);
        Ok(())
    }

    // ─── 受信ハンドラ ──────────────────────────────────────

    /// SUBSCRIBE / TRACK_STATUS の受信前に共通で行う検証
    ///
    /// draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS): "The receiver of a TRACK_STATUS
    /// message treats it identically as if it had received a SUBSCRIBE message, except it
    /// does not create downstream subscription state or send any Objects." したがって
    /// 受信前の検証 (Request ID / メッセージパラメータのスコープ / AUTHORIZATION_TOKEN /
    /// Range Filter 数 / 予約名前空間 / `.session` 名前空間) は両メッセージで共通である。
    /// `allowed_params` は呼び出し側のメッセージ種別が許可するパラメータ集合
    /// (`SUBSCRIBE_ALLOWED_PARAMS` / `TRACK_STATUS_ALLOWED_PARAMS`) を渡す。
    ///
    /// `false` を返した場合は request を受理しない (REQUEST_ERROR は本関数内で発行済み、
    /// または `Err` でセッションを閉じている)。呼び出し側は `Ok(false)` で早期 return する。
    /// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub(crate) fn accept_incoming_track_request(
        &mut self,
        request_id: u64,
        track_namespace: &TrackNamespace,
        track_name: &[u8],
        parameters: &MessageParameters,
        allowed_params: &[u64],
        scope_error_reason: &'static str,
    ) -> Result<bool, SessionError> {
        // draft-ietf-moq-transport-23 §2.4.1 (Track Naming): 空の Track Name は
        // PROTOCOL_VIOLATION でセッションを閉じる (MUST)。request 単位の拒否ではないため、
        // 以下の request ID 検証・メッセージ単位の拒否より先に検証する。
        self.validate_peer_track_name(track_name)?;
        // SUBSCRIBE と TRACK_STATUS は自側が publisher として応答する request である
        if !self.accept_peer_request(
            request_id,
            parameters,
            true,
            allowed_params,
            scope_error_reason,
        )? {
            return Ok(false);
        }
        // draft §8.9 (Authorization Token Compression): AUTHORIZATION_TOKEN Register/Delete/Use を peer cache に反映
        self.apply_peer_message_auth_tokens(parameters)?;
        // draft-ietf-moq-transport-23 §3.3.2 (Range Filters): MAX_FILTER_RANGES 超過は INVALID_FILTER で拒否
        if let Err(reason) = self.check_incoming_range_filters(parameters) {
            self.emit_request_error(request_id, REQUEST_INVALID_FILTER, reason);
            return Ok(false);
        }
        // draft-ietf-moq-transport-23 は値域 MUST (§9.19.8 (GROUP ORDER Parameter) /
        // §9.19.22 (INCLUDE_PROPERTIES Parameter) 等) と予約名前空間拒否の優先順位を規定しない。
        // 宛先自体が仕様上存在しない予約名前空間の拒否を優先する意図した選択である
        // (値域 MUST は wire 経路では decode 層がハンドラ到達前に発火するため、優先順位が
        // 観測されるのは API 経路で手組みしたメッセージのみ)。
        // draft-ietf-moq-transport-23 §2.4.3 (Reserved Namespaces): single period `.` 予約名前空間は拒否
        if track_namespace.is_single_period() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "reserved single-period namespace",
            );
            return Ok(false);
        }
        // draft §6.5 (Session-Level Tracks and Namespaces): .session 名前空間は内部処理
        // 未知のセッションレベルトラックは DOES_NOT_EXIST で拒否する (空の Track Name は
        // 上の §2.4.1 (Track Naming) の検証でセッションを閉じるため、ここには到達しない)
        if track_namespace.is_session_level() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "session-level track does not exist",
            );
            return Ok(false);
        }
        Ok(true)
    }

    pub(crate) fn handle_peer_subscribe(
        &mut self,
        subscribe: Subscribe,
    ) -> Result<(), SessionError> {
        let request_id = subscribe.request_id;
        if !self.accept_incoming_track_request(
            request_id,
            &subscribe.track_namespace,
            &subscribe.track_name,
            &subscribe.parameters,
            crate::message::SUBSCRIBE_ALLOWED_PARAMS,
            "SUBSCRIBE parameter not allowed in this context",
        )? {
            return Ok(());
        }
        // draft-ietf-moq-transport-23 §3.1 (Subscriptions): 同一 Track への複数同時 subscription が許可されたため、
        // DUPLICATE_SUBSCRIPTION による拒否は行わない。
        // draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter): PAUSED パラメータの
        // 抽出 (不在時は既定型 0 = unpaused)。値域外 (0 / 1 以外) の受信は MUST で
        // セッションを INVALID_PARAMETER (0x305) により閉じる (GROUP_ORDER の値域検証と
        // 同じパターン。`?` 伝播のみだとセッションが開いたまま残る)。検証の `Ok` 値を
        // `paused` の設定に使い、`paused()` の検索結果を検証に再利用する
        // (検証を呼び出しと分離し、同じパラメータを 2 回解釈しない)。
        let paused = match subscribe.parameters.paused() {
            Some(value) => match validate_paused(value) {
                Ok(validated) => validated,
                Err(err) => {
                    self.fail(err.clone());
                    return Err(err);
                }
            },
            None => false,
        };
        let subscriber_object_delivery_timeout_ms = subscribe.parameters.object_delivery_timeout();
        let subscriber_subgroup_delivery_timeout_ms =
            subscribe.parameters.subgroup_delivery_timeout();
        let filter = match subscribe.parameters.location_filter_update() {
            Ok(LocationFilterUpdate::Set(filter)) => Some(filter),
            // 省略時と Location Filter Type 0x00 (None) はどちらも unfiltered として扱う
            Ok(_) => None,
            Err(msg_err) => {
                // draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter): 未知の
                // Location Filter Type は INVALID_PARAMETER、StartGroup + EndGroupDelta の
                // オーバーフローは DELTA_ENCODING_OVERFLOW で閉じる (MUST)。wire 経路では
                // decode 層が先に拒否するため、ここへ届くのは API 経由で手組みした
                // メッセージだけである。
                let err = crate::session::core::session_error_from_message(
                    &msg_err,
                    SESSION_INVALID_PARAMETER,
                );
                self.fail(err.clone());
                return Err(err);
            }
        };
        let subscriber_priority = subscribe.parameters.subscriber_priority();
        let group_order = subscribe.parameters.group_order();
        // draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter):
        // 値域外 (0 / 1 以外) の受信は MUST でセッションを INVALID_PARAMETER (0x305) により
        // 閉じる (draft-ietf-moq-transport-23 §12.2 (Session Termination Codes))。
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
        // largest を持たないため、`publisher_track_largest` が返す Track 最大 (live な
        // subscription の観測値と、Track 単位で保持した自側 publish 済み最大の大きい方。
        // 未 publish なら先頭) で相対フィルタを解決する。
        // 仕様の Largest Object は publisher がメッセージを処理する視点で定義されるため、
        // 未配信 track では `None` 基準 (先頭から) になる。
        // この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
        let track_largest =
            self.publisher_track_largest(&subscribe.track_namespace, &subscribe.track_name);
        let (filter_start, filter_end) = resolve_location_filter(
            filter.as_ref(),
            track_largest.as_ref(),
            LocationFilterContext::Subscription,
        );
        let range_filters = extract_range_filters(&subscribe.parameters);
        if let Some(order) = group_order {
            // draft §9.19.8 (GROUP ORDER Parameter): 値域外の受信はセッションを閉じる
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
            paused,
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
            expires: None,
            // SUBSCRIBE には TrackProperties は含まれない。Publisher である自側が
            // send_subscribe_ok で TrackProperties を発行するタイミングで対応状況を確定する。
            // 確定するまでは未判明である。
            dynamic_groups: DynamicGroupsSupport::Unknown,
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
            // SUBSCRIBE に SUBSCRIBE_TRACKS_ID は出現しない
            // (draft-ietf-moq-transport-23 §9.19.21 (SUBSCRIBE_TRACKS_ID Parameter))
            subscribe_tracks_id: None,
            // draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter):
            // peer が SUBSCRIBE で指定した値を保持し、SUBSCRIBE_OK 送信時に参照する
            include_properties,
            filter_start,
            filter_end,
            range_filters,
            ended_groups: HashMap::new(),
            end_of_track: None,
            pending_update_params: VecDeque::new(),
        };
        self.register_subscription(request_id, subscription);
        self.request_streams
            .insert(request_id, RequestKind::Subscribe);
        // draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams):
        // FILL_PARAMETERS 付き SUBSCRIBE を unpaused (PAUSED=0) で処理したら
        // fill fetch stream を開く。fill range が empty または Largest Object より
        // 後に始まる場合・ Largest Object 未知の場合は開設しない。
        // SUBSCRIBE は LARGEST_OBJECT を運ばないため、Largest Object は自側の観測値から
        // 解決する (`largest_view` に `None` を渡す)。
        self.maybe_open_fill_stream(
            request_id,
            request_id,
            subscribe.parameters.fill_parameters(),
            filter,
            None,
            paused,
        );
        Ok(())
    }

    /// peer publisher からの PUBLISH を受信する (自側が subscriber)
    ///
    /// draft-ietf-moq-transport-23 §9.8 (PUBLISH) / §9.19.15 (FILL PARAMETERS Parameter):
    /// PUBLISH は FILL_PARAMETERS を運べる。受信側である自側はこのパラメータを受理して
    /// subscription の状態を作るが、fill fetch stream は開設しない。開設するのは
    /// draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams) が定める
    /// publisher の責務であり、自側は publisher が開いた fill fetch stream を
    /// `Session::recv_fetch_header` で受理する側である。この役割分担により、PUBLISH_OK を
    /// 返す subscriber が誤って fill を送出することはない。
    pub(crate) fn handle_peer_publish(&mut self, publish: Publish) -> Result<(), SessionError> {
        let request_id = publish.request_id;
        // draft-ietf-moq-transport-23 §2.4.1 (Track Naming): 空の Track Name は
        // PROTOCOL_VIOLATION でセッションを閉じる (MUST)。request 単位の拒否より先に検証する。
        self.validate_peer_track_name(&publish.track_name)?;
        // PUBLISH は自側が subscriber として受ける request であり、§9.2 の publisher 限定の
        // 拒否 MAY の対象外である
        if !self.accept_peer_request(
            request_id,
            &publish.parameters,
            false,
            crate::message::PUBLISH_ALLOWED_PARAMS,
            "PUBLISH parameter not allowed in this context",
        )? {
            return Ok(());
        }
        // draft §8.9 (Authorization Token Compression): AUTHORIZATION_TOKEN Register/Delete/Use を peer cache に反映
        self.apply_peer_message_auth_tokens(&publish.parameters)?;
        // draft-ietf-moq-transport-23 §3.3.2 (Range Filters): MAX_FILTER_RANGES 超過は INVALID_FILTER で拒否
        if let Err(reason) = self.check_incoming_range_filters(&publish.parameters) {
            self.emit_request_error(request_id, REQUEST_INVALID_FILTER, reason);
            return Ok(());
        }
        // draft-ietf-moq-transport-23 は値域 MUST (§9.19.8 (GROUP ORDER Parameter) /
        // §9.19.22 (INCLUDE_PROPERTIES Parameter) 等) と予約名前空間拒否の優先順位を規定しない。
        // 宛先自体が仕様上存在しない予約名前空間の拒否を優先する意図した選択である
        // (値域 MUST は wire 経路では decode 層がハンドラ到達前に発火するため、優先順位が
        // 観測されるのは API 経路で手組みしたメッセージのみ)。
        // draft-ietf-moq-transport-23 §2.4.3 (Reserved Namespaces): single period `.` 予約名前空間の PUBLISH は拒否
        if publish.track_namespace.is_single_period() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "reserved single-period namespace",
            );
            return Ok(());
        }
        // draft §6.5 (Session-Level Tracks and Namespaces): .session 名前空間の PUBLISH は内部処理
        // 未知のセッションレベルトラックは DOES_NOT_EXIST で拒否
        if publish.track_namespace.is_session_level() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "session-level track does not exist",
            );
            return Ok(());
        }
        // draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties):
        // 未知の Mandatory Track Property を含む PUBLISH は REQUEST_ERROR
        // (UNSUPPORTED_EXTENSION) で拒否する。理解できなかった型は
        // draft-ietf-moq-transport-23 §9.4.2.2 (UNSUPPORTED_PROPERTY) の Error Detail で
        // 1 型 1 Detail として伝える (同節の SHOULD)。複数型をまとめて伝えられるよう
        // 複数インスタンスを許す型として生成する。型の列挙は IMMUTABLE_PROPERTIES の内側も
        // 対象にし、GREASE は対象外になる (TrackProperties::unknown_mandatory_types)。
        let unsupported_types = publish.track_properties.unknown_mandatory_types();
        if !unsupported_types.is_empty() {
            let mut error_details = ErrorDetails::new();
            for property_type in unsupported_types {
                error_details.push(ErrorDetail::UnsupportedProperty(UnsupportedProperty {
                    property_type,
                }));
            }
            self.emit_request_error_with_details(
                request_id,
                REQUEST_UNSUPPORTED_EXTENSION,
                "unsupported mandatory extension",
                error_details,
            );
            return Ok(());
        }
        // draft-ietf-moq-transport-23 §8.4 (Track and Object Properties): "An endpoint that
        // supports a Property and receives it outside its scope (e.g. a Track-only Property as
        // an Object Property) MUST treat the Track as malformed" / §12.1 (Malformed Tracks)
        // 条件 8。同じ §8.4 は、定義が複数を許す場合を除いて 1 インスタンスまでと定め、
        // 超過受信も malformed Track とする (§12.1 条件 9)。この実装が意味を理解する
        // Property のスコープと多重可否を検証する。
        // 初回 PUBLISH は購読を登録する前なので、受信メッセージの Request ID と Full Track
        // Name から同じ Track の既存要求を取消対象として解決し、この PUBLISH 自身も購読と
        // して登録せず bidi request stream の cancel と `RequestTerminated` で終端する
        // (アプリへ正常な受信として通知しない)。未知 Mandatory Track Property の拒否
        // (§3.7) を先に判定するのは、draft が両条件の優先順位を定めておらず、既存の
        // request 拒否を維持するためである。
        if let Err(err) = publish.track_properties.validate() {
            self.terminate_malformed_track_for_publish(
                request_id,
                &publish.track_namespace,
                &publish.track_name,
                err.reason(),
            );
            return Ok(());
        }
        // draft §3.1.3 (Track Alias): "If a subscriber receives a PUBLISH or SUBSCRIBE_OK that
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
        // draft-ietf-moq-transport-23 §3.1 (Subscriptions): 同一 Track への複数同時 subscription が許可されたため、
        // DUPLICATE_SUBSCRIPTION による拒否は行わない。
        // ただし Pending(Subscriber) に対する PUBLISH は SUBSCRIBE と PUBLISH が交差した場合の
        // 救済措置として既存 subscription を Terminated に遷移させる (draft §3.1)。
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
        // draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter): PAUSED パラメータの
        // 抽出 (不在時は既定型 0 = unpaused)。値域外 (0 / 1 以外) の受信は MUST で
        // セッションを INVALID_PARAMETER (0x305) により閉じる (GROUP_ORDER の値域検証と
        // 同じパターン。`?` 伝播のみだとセッションが開いたまま残る)。検証の `Ok` 値を
        // `paused` の設定に使い、`paused()` の検索結果を検証に再利用する
        // (検証を呼び出しと分離し、同じパラメータを 2 回解釈しない)。
        let paused = match publish.parameters.paused() {
            Some(value) => match validate_paused(value) {
                Ok(validated) => validated,
                Err(err) => {
                    self.fail(err.clone());
                    return Err(err);
                }
            },
            None => false,
        };
        // draft-ietf-moq-transport-23 §9.19.8 (GROUP ORDER Parameter): 値域外の受信は
        // MUST でセッションを INVALID_PARAMETER (0x305) により閉じる。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        let group_order = publish.parameters.group_order();
        if let Some(order) = group_order
            && let Err(err) = validate_group_order(order)
        {
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-23 §9.8 (PUBLISH) / §9.19.9 (LOCATION FILTER Parameter):
        // PUBLISH の Parameters は initial subscription parameters として保持する。
        // Location Filter Type 0x00 (None) と省略はどちらも unfiltered として扱う。新規 PUBLISH は解決時点の
        // largest を持たないため、相対フィルタは先頭から解決される。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        let filter = match publish.parameters.location_filter_update() {
            Ok(LocationFilterUpdate::Set(filter)) => Some(filter),
            Ok(_) => None,
            // 受信メッセージでは decode 層が値域と形式を検証済みのため到達しない。
            // API 経由で手組みしたメッセージに対しては安全側にセッションを閉じる。
            Err(msg_err) => {
                let err = crate::session::core::session_error_from_message(
                    &msg_err,
                    SESSION_INVALID_PARAMETER,
                );
                self.fail(err.clone());
                return Err(err);
            }
        };
        let subscriber_priority = publish.parameters.subscriber_priority();
        // draft-ietf-moq-transport-23 §9.19.21 (SUBSCRIBE_TRACKS_ID Parameter):
        // SUBSCRIBE_TRACKS 起因の PUBLISH を識別する Request ID を相関情報として保持する。
        // 本ライブラリは SUBSCRIBE_TRACKS を実装せず要求との照合状態も持たないため、
        // 対応する要求が無い PUBLISH でも値を保持するだけで拒否せず、通常の PUBLISH の
        // 検証とアプリケーションによる受理・拒否判断へ進む。
        let subscribe_tracks_id = publish.parameters.subscribe_tracks_id();
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
        // draft-ietf-moq-transport-23 §10.6 (DYNAMIC GROUPS) / §9.19.19 (NEW GROUP REQUEST
        // Parameter): peer が発行した PUBLISH の TrackProperties から対応状況を求める。
        // PUBLISH は INCLUDE_PROPERTIES を運ばず (draft-ietf-moq-transport-23 §9.19.22 の
        // MAY appear 列挙に含まれない)、本実装は SUBSCRIBE_TRACKS も実装しないため、
        // 受け取った Track Properties は常に判明値として扱う。
        let dynamic_groups =
            DynamicGroupsSupport::from_peer_properties(&publish.track_properties, None);
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
        // draft-ietf-moq-transport-23 §9.19.8 (GROUP ORDER Parameter): GROUP_ORDER は
        // SUBSCRIBE / PUBLISH / SUBSCRIBE_TRACKS / FETCH に出現可能であり、PUBLISH にも
        // 直接含められる。
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
            paused,
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
            expires,
            dynamic_groups,
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
            subscribe_tracks_id,
            // PUBLISH に INCLUDE_PROPERTIES は出現しない (draft-ietf-moq-transport-23 §9.19.22)
            include_properties: None,
            filter_start,
            filter_end,
            range_filters,
            ended_groups: HashMap::new(),
            end_of_track: None,
            pending_update_params: VecDeque::new(),
        };
        self.register_subscription(request_id, subscription);
        self.request_streams
            .insert(request_id, RequestKind::Publish);
        // draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams): publisher は
        // PUBLISH を送るときに初回 fill の fill fetch stream を開く。受信側は PUBLISH が運んだ
        // FILL_PARAMETERS を、その fill stream の暗黙 gap 判定に使う。
        self.remember_fill_context(request_id, publish.parameters.fill_parameters());
        self.register_peer_alias(publish.track_alias, request_id);
        // draft §3.1 (Subscriptions) / §9.19.17 (LARGEST OBJECT Parameter):
        // PUBLISH の parameters に LARGEST_OBJECT があれば広告値として保存する
        if let Some((group_id, object_id)) = publish.parameters.largest_object()
            && let Some(sub) = self.subscriptions.get_mut(&request_id)
        {
            sub.largest_location = Some(Location {
                group_id,
                object_id,
            });
        }
        Ok(())
    }

    pub(crate) fn handle_peer_subscribe_ok(
        &mut self,
        request_id: u64,
        ok: SubscribeOk,
    ) -> Result<(), SessionError> {
        let now_ms = self.timing.last_tick_ms;
        // ローカル終端済み (`rejected_request_ids` に記録済み) の request は送信方向を
        // reset 済みだが、peer が reset を観測する前に送った SUBSCRIBE_OK は正当に届く。
        // その場合は本関数の検証 (Track Alias の衝突) だけを行い、状態遷移とイベント発行は
        // しない。draft §3.7 (Mandatory to Understand Track Properties) の cancel は
        // 既に終端済みのため不要である。
        let locally_terminated = self.is_locally_terminated_request(request_id);
        let Some(subscription) = self.subscriptions.get_mut(&request_id) else {
            let err = SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "SUBSCRIBE_OK received for unknown request id",
            );
            self.fail(err.clone());
            return Err(err);
        };
        // peer の role 違反 (publisher 側の購読への SUBSCRIBE_OK) と、role は正しいが
        // 状態が不正 (重複応答など) を分ける。前者は draft-ietf-moq-transport-23 §12.2 の
        // ROLE_VIOLATION、後者は UNEXPECTED_MESSAGE で閉じる
        if subscription.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_ROLE_VIOLATION,
                "SUBSCRIBE_OK received for a publisher-side subscription",
            );
            self.fail(err.clone());
            return Err(err);
        }
        if !(subscription.is_pending_subscriber() || locally_terminated) {
            let err = SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "SUBSCRIBE_OK in unexpected subscription state",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft §3.1.3 (Track Alias): 異なる Track の Established subscription と衝突する場合のみ
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
        // ローカル終端済みの request はここまでの MUST 検証だけを行い、状態遷移しない
        if locally_terminated {
            return Ok(());
        }
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("subscription presence already checked above");
        // draft §3.7 (Mandatory to Understand Track Properties): 未知の必須プロパティを含む SUBSCRIBE_OK は購読をキャンセルする
        if ok.track_properties.has_unknown_mandatory() {
            // 本経路に到達する subscription は `handle_peer_subscribe_ok` の事前検証で
            // Pending(Subscriber) に限られるため、この判定は現状つねに true になる。
            // `StopSendingRequestStream` の doc が定める「既に Terminated の経路では
            // 発行しない」規則を将来の経路追加に対して保つための防御である。
            let newly_terminated = subscription.state != SubscriptionState::Terminated;
            subscription.state = SubscriptionState::Terminated;
            self.clear_control_message_deadline(request_id);
            // ローカルで購読をキャンセルするため、request stream GOAWAY の reset deadline も解除する
            self.clear_request_stream_goaway_deadline(request_id);
            // draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties) / §6.4.2.3
            // (Request Cancellation and Rejection): cancel は開いている方向をストリーム終端で
            // 打ち切る。受信方向を STOP_SENDING、送信方向を RESET_STREAM で打ち切るよう
            // I/O 層へ指示する。エラーコードは §12.5 (Stream Reset Error Codes) の
            // CANCELLED (自端の購読キャンセルであり、relay publisher の検出を表す
            // MALFORMED_TRACK ではない)。
            if newly_terminated {
                self.events
                    .push_back(SessionEvent::StopSendingRequestStream {
                        request_id,
                        error_code: STREAM_CANCELLED,
                    });
                self.events.push_back(SessionEvent::ResetRequestStream {
                    request_id,
                    error_code: STREAM_CANCELLED,
                });
            }
            self.events.push_back(SessionEvent::RequestTerminated {
                request_id,
                kind: RequestKind::Subscribe,
                reason: TerminationReason::LocalCancel,
            });
            return Ok(());
        }
        // draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1
        // (Malformed Tracks) 条件 8 / 9: この実装が意味を理解する Property をスコープ外で
        // 受信した場合と、定義が複数を許す Property を超過して受信した場合に Track を
        // malformed とする (MUST)。購読を cancel し、同じ Track の FETCH も取消対象にして、
        // アプリへ `MalformedTrack` を通知する (既存の malformed Track 経路)。未知
        // Mandatory Track Property の cancel (§3.7) を先に判定するのは、draft が両条件の
        // 優先順位を定めていないためである。
        if let Err(err) = ok.track_properties.validate() {
            self.terminate_malformed_track(request_id, None, err.reason());
            return Ok(());
        }
        self.clear_control_message_deadline(request_id);
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("validated above");
        subscription.track_alias = Some(ok.track_alias);
        subscription.state = SubscriptionState::Established;
        // draft §3.1 (Subscriptions) / §9.19.17 (LARGEST OBJECT Parameter):
        // LARGEST_OBJECT parameter があれば広告値として保存する
        if let Some((group_id, object_id)) = ok.parameters.largest_object() {
            subscription.largest_location = Some(Location {
                group_id,
                object_id,
            });
        }
        // draft-ietf-moq-transport-23 §10.6 (DYNAMIC GROUPS) / §9.19.19 (NEW GROUP REQUEST
        // Parameter): SUBSCRIBE_OK で peer publisher が発行した Track Properties から
        // DYNAMIC_GROUPS の対応状況を求める (自側が subscriber なので REQUEST_UPDATE 送信時の
        // NEW_GROUP_REQUEST 可否を判断するために使う)。INCLUDE_PROPERTIES=0 を指定して空の
        // Track Properties を受け取った場合は、peer が告知しなかっただけであり判明していない
        // ため未判明を保つ ([`DynamicGroupsSupport::from_peer_properties`])。
        let requested_include_properties = subscription.include_properties;
        subscription.dynamic_groups = DynamicGroupsSupport::from_peer_properties(
            &ok.track_properties,
            requested_include_properties,
        );
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
        // draft-ietf-moq-transport-23 §9.19.8 (GROUP ORDER Parameter): GROUP_ORDER は
        // SUBSCRIBE / PUBLISH / SUBSCRIBE_TRACKS / FETCH に含まれる。
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
        // draft-ietf-moq-transport-23 §6.4.2.1 (Request ID): REQUEST_UPDATE も Request ID を
        // 消費するメッセージとして列挙される。peer から受信した Request ID の parity 違反と
        // 重複を検証し、違反時は INVALID_REQUEST_ID でセッションを Closing に遷移させる (MUST)。
        // 検証は本関数の先頭で 1 回だけ行い、subscription / fetch の分岐より前に済ませる。
        //
        // ここでは `accept_peer_request` ではなく `validate_peer_request_id` を呼ぶ。
        // draft §9.2 (GOAWAY) は "The GOAWAY message does not impact subscription state." と
        // 定めており、GOAWAY 送信後に拒否してよいのは新規 request であって、既存の bidi request
        // stream 上を流れる REQUEST_UPDATE は含まない。draft §9.5 (REQUEST_UPDATE) も受信側に
        // REQUEST_OK / REQUEST_ERROR のいずれか 1 つで応答することを MUST としているため、
        // GOING_AWAY による拒否経路は適用しない。
        self.validate_peer_request_id(update.request_id)?;
        // draft-ietf-moq-transport-23 §6.4.2.1 (Request ID) / §9.5 (REQUEST_UPDATE):
        // REQUEST_UPDATE は独立した Request ID を消費する。対象の request は
        // 「同じ bidi stream 上で送る」ことで識別されるため、wire の
        // `update.request_id` は stream context の `request_id` と一致しない。
        // 対象 request の解決には stream context の `request_id` を使う。
        // draft-ietf-moq-transport-23 §9.1.7 (MAX_REQUEST_UPDATES): peer からの outstanding
        // REQUEST_UPDATE 数が自側 SETUP で宣言した MAX_REQUEST_UPDATES を超えたら
        // TOO_MANY_REQUEST_UPDATES でセッションを閉じる。デフォルト値 0 は無制限を意味する。
        let local_max = self.local_max_request_updates();
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
        // draft §9.19.1 (Parameter Scope): context 別の許可パラメータ集合で
        // 検証する。違反時は INVALID_PARAMETER でセッションをクローズする (MUST)。
        // あわせて draft §9.5 (REQUEST_UPDATE): "An endpoint that receives a REQUEST_UPDATE
        // other than in the two cases above MUST close the session with UNEXPECTED_MESSAGE."
        // を満たすため、更新できない request (TRACK_STATUS / 未知 id) もここで閉じる。
        // セッション終了コードは draft-ietf-moq-transport-23 §12.2 (Session Termination Codes)
        // に従い UNEXPECTED_MESSAGE / ROLE_VIOLATION / INVALID_PARAMETER に細分類する。
        // 対象 request の state / role (§3.1 / §3.2 / §9.5) もここで検証する。
        //
        // この検証は Range Filter の内容検証より先に行う。Range Filter を許可しない
        // context (FETCH_UPDATE_ALLOWED_PARAMS など) に Range Filter が来た場合、
        // 内容検証を先に走らせると §3.3.2 の INVALID_FILTER (REQUEST_ERROR で継続) が
        // 発火してしまい、§9.19.1 が MUST とするセッションクローズに到達しない。
        // 同じ理由で、同じくメッセージ単位の reject である不正トークン判定 (§8.9) よりも
        // 先に置き、セッション終了の MUST が REQUEST_ERROR に隠れないようにする。
        // context 別の許可集合は decode 時の `REQUEST_UPDATE_ALLOWED_PARAMS`
        // (draft-ietf-moq-transport-23 §9.5 の全 context の和集合) より狭いため、
        // wire 経路でも context 逸脱を検出できるのはこの検証だけである。
        //
        // state / role の検証はローカル終端済みの request でも行う (state のみ緩める)。
        // 応答を送れないことと、送信者として正当かは独立である。
        let locally_terminated = self.is_locally_terminated_request(request_id);
        let table = self.locate_request(request_id);
        let (table, allowed, scope_error) = match table {
            Some(t @ RequestTable::Subscription) => {
                self.validate_subscription_request_update(request_id, locally_terminated)?;
                (
                    t,
                    SUBSCRIPTION_UPDATE_ALLOWED_PARAMS,
                    "REQUEST_UPDATE (subscription) parameter not allowed in this context",
                )
            }
            Some(t @ RequestTable::Fetch) => {
                self.validate_fetch_request_update(request_id, locally_terminated)?;
                (
                    t,
                    FETCH_UPDATE_ALLOWED_PARAMS,
                    "REQUEST_UPDATE (fetch) parameter not allowed in this context",
                )
            }
            Some(RequestTable::TrackStatus) | None => {
                let err = SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "REQUEST_UPDATE received for unknown or unsupported request id",
                );
                self.fail(err.clone());
                return Err(err);
            }
        };
        // draft-ietf-moq-transport-23 §9.19.1 (Parameter Scope): INVALID_PARAMETER (0x305)。
        if update.parameters.validate_scope(allowed).is_err() {
            let err = SessionError::new(SESSION_INVALID_PARAMETER, scope_error);
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-23 §9.19.15 (FILL PARAMETERS Parameter): 内側の
        // パラメータも許可集合に限る。wire 経路では decode 層が検証済みだが、decode 済み
        // メッセージを直接受け取る API 経路ではここで検証する。
        if let Err(msg_err) = update.parameters.validate_fill_parameters_scope() {
            // draft-ietf-moq-transport-23 §9.19.15 (FILL PARAMETERS Parameter):
            // 内側スコープ違反は INVALID_PARAMETER (0x305)。
            let err = SessionError::new(SESSION_INVALID_PARAMETER, msg_err.reason());
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter):
        // PAUSED は 0 (unpaused) / 1 (paused) のみが有効であり、値域外を受信した
        // endpoint は INVALID_PARAMETER (0x305) でセッションを閉じる MUST。Range Filter の
        // 拒否 (INVALID_FILTER の REQUEST_ERROR) より先に検証し、MUST close が
        // REQUEST_ERROR に隠れないようにする。
        if let Some(value) = update.parameters.paused()
            && let Err(err) = validate_paused(value)
        {
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-23 §8.9 (Authorization Token Compression): "The receiver
        // of a message containing a well-formed Token structure that is otherwise invalid
        // MUST reject that message with an MALFORMED_AUTH_TOKEN error."
        // REQUEST_UPDATE は Request ID を持つ request メッセージのため、メッセージ単位の
        // reject は §9.5 (REQUEST_UPDATE) が MUST とする REQUEST_ERROR で表す。
        // 内容の不正は alias 解決後の (Token Type, Token Value) の重複であり
        // `Session::validate_peer_message_auth_token_uniqueness` が判定する。判定は token cache
        // の適用より先に行う。reject するメッセージの DELETE / USE_ALIAS を適用すると、
        // 未登録 alias の USE_ALIAS が UNKNOWN_AUTH_TOKEN_ALIAS として session を kill し、
        // reject のはずの失敗を session error に格上げしてしまう (GOAWAY 拒否経路と同じ理由。
        // `apply_peer_message_auth_token_registers_only` の doc 参照)。この検証は一時
        // キャッシュで alias を解決するだけで実キャッシュを変更せず、解決できない alias も
        // 判定対象から外すため、未登録 alias の扱いは適用経路のまま変わらない。
        // セッション終了の検証 (§9.19.1 の context スコープ / §9.5 と §3.1・§3.2 の
        // state・role / §9.19.18 の PAUSED 値域) は上で済ませてある。§9.19.19
        // (NEW_GROUP_REQUEST) は subscriber の MUST NOT を送信側で担保するものであり、
        // 受信側では同節の無視する規則に従って拒否しない (`handle_update_for_subscription`)。
        if self
            .validate_peer_message_auth_token_uniqueness(&update.parameters)
            .is_err()
        {
            // §8.9 の REGISTER MUST は「session error にならないメッセージの失敗」に適用される
            self.apply_peer_message_auth_token_registers_only(&update.parameters)?;
            // ローカル終端済みの request は送信方向を reset 済みで拒否応答を送れないため、
            // 状態を変えずにメッセージを捨てる (Range Filter 拒否と同じ扱い)
            if locally_terminated {
                return Ok(());
            }
            self.reject_request_update(
                request_id,
                REQUEST_MALFORMED_AUTH_TOKEN,
                "malformed AUTHORIZATION_TOKEN",
            )?;
            return Ok(());
        }
        // draft §8.9 (Authorization Token Compression): AUTHORIZATION_TOKEN Register/Delete/Use を peer cache に反映
        // §9.19.1 のスコープ検証より後に行う。スコープ違反で session error になる
        // メッセージのトークンは適用しない (§8.9 の REGISTER MUST は
        // "does not result in a Session error" が前提)
        self.apply_peer_message_auth_tokens(&update.parameters)?;
        // draft-ietf-moq-transport-23 §3.3.2 (Range Filters): MAX_FILTER_RANGES 超過は INVALID_FILTER で拒否。
        // スコープ上正当な Range Filter に対してのみ内容検証を行う。
        // 自側 publisher の Established subscription では PUBLISH_DONE(UPDATE_FAILED) まで
        // 終端する (§9.5.1 の MUST)。subscriber 側は REQUEST_ERROR のみで従来どおり。
        // ローカル終端済みの request は送信方向を reset 済みで拒否応答 (REQUEST_ERROR) を
        // 送れないため、拒否せず dispatch へ進む。Range Filter の内容検証自体は省略され
        // (draft §3.3.2 の MUST は REQUEST_ERROR による拒否であり、応答を送れない以上
        // 満たせない)、PAUSED の MUST 検証は dispatch 前に行い、状態遷移はしない
        if let Err(reason) = self.check_incoming_range_filters(&update.parameters)
            && !locally_terminated
        {
            self.reject_request_update(request_id, REQUEST_INVALID_FILTER, reason)?;
            return Ok(());
        }
        // draft-ietf-moq-transport-23 §3.4 (Fill Semantics): REQUEST_UPDATE 起因の
        // fill fetch stream は REQUEST_UPDATE 自身の Request ID を FETCH_HEADER に載せる。
        // 受信した fill fetch stream をこの subscription へ帰属させるため対応を登録する。
        // fill fetch stream を開きうるのは FILL_PARAMETERS を持つ REQUEST_UPDATE だけなので、
        // その場合のみ登録する。
        if update.parameters.fill_parameters().is_some() {
            // ローカル終端済みでも登録する。peer が reset を観測する前に開いた fill fetch
            // stream を subscription へ解決できるようにするためである
            // (`handle_update_for_subscription` はこの後に状態遷移せず return する)。
            self.register_fill_request_subscription(update.request_id, request_id);
        }
        match table {
            RequestTable::Subscription => self.handle_update_for_subscription(
                request_id,
                update.request_id,
                update.parameters,
                locally_terminated,
            ),
            RequestTable::Fetch => {
                self.handle_update_for_fetch(request_id, update.parameters, locally_terminated)
            }
            // 上の検証で TRACK_STATUS は UNEXPECTED_MESSAGE としてセッション終了済み (防御)
            RequestTable::TrackStatus => {
                let err = SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "REQUEST_UPDATE received for unknown or unsupported request id",
                );
                self.fail(err.clone());
                Err(err)
            }
        }
    }

    /// REQUEST_UPDATE を REQUEST_ERROR で拒否するときの終端処理
    ///
    /// `error_code` は拒否理由 (Range Filter の INVALID_FILTER、不正トークンの
    /// MALFORMED_AUTH_TOKEN など) を表す。あわせて draft-ietf-moq-transport-23 §9.5.1
    /// (Updating Subscriptions): "When a REQUEST_UPDATE is unsuccessful, the publisher MUST
    /// also terminate the subscription by sending a PUBLISH_DONE with error code
    /// UPDATE_FAILED." を満たす。
    ///
    /// 呼び出し元 (`handle_peer_request_update` の拒否経路と、`handle_update_for_subscription`
    /// の累積 Range Filter 超過) は、事前に §9.5 / §3.1 / §3.2 の state・role 検証
    /// (`validate_subscription_request_update` /
    /// `validate_fetch_request_update`) を通している。更新できない request は
    /// REQUEST_ERROR ではなくセッション終了になっているため、本関数は
    /// 拒否できる request (Established) だけを扱う。state は見ない前提であり、
    /// TRACK_STATUS / 未知 id だけを防御として閉じる。
    ///
    /// draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE): "REQUEST_UPDATE_OKs MUST be
    /// returned in the order the REQUEST_UPDATEs were received." のため、先行する未応答
    /// グループがある状態で REQUEST_ERROR を即座に送ってはならない。先行グループの OK より
    /// 先に ERROR が wire に出ると順序違反になるうえ、現実装では ERROR の終端処理が
    /// キューを解放するため先行グループの OK を永久に送れなくなる。
    ///
    /// そこで subscription では次のように振る舞う。
    ///
    /// - 未応答グループが無い: 従来どおり即時に REQUEST_ERROR を送る。
    /// - 未応答グループがある: 拒否内容を [`PendingUpdateGroup`] としてキューへ積み、
    ///   先行グループの OK が送出された時点 (`Session::send_request_ok`) で
    ///   REQUEST_ERROR を自動送出する。拒否した更新はアプリへ通知しない
    ///   (アプリは拒否された更新への応答を返さない)。
    ///
    /// FETCH は受信側キューを持たず更新を即時処理するため、即時応答のままで順序が保たれる。
    ///
    /// - Established かつ自側 publisher: `send_request_error` と同じ経路で
    ///   REQUEST_ERROR (`error_code`) → Terminated → PUBLISH_DONE(UPDATE_FAILED)。
    ///   open 中の outgoing data stream (subgroup / fill fetch) がある場合は PUBLISH_DONE を保留する。
    /// - Established かつ自側 subscriber (peer が PUBLISH 起点の publisher):
    ///   REQUEST_ERROR のみを送る。PUBLISH_DONE の送出は publisher である peer の責務であり、
    ///   state の終端は peer の PUBLISH_DONE / bidi 終端処理に委ねる。
    /// - FETCH: `send_request_error` (`send_err_for_fetch`) を通し、draft §9.5.1 の MUST
    ///   (FETCH data stream の reset) と fetch state の `Terminated` 遷移を行う。
    ///   Terminated の fetch は事前検証でセッション終了になるため REQUEST_ERROR は送られない。
    /// - ローカル終端済み (`rejected_request_ids` に記録済み) の request: 呼び出し元が
    ///   拒否応答を積まずに吸収する (送信方向を reset 済みで応答を送れないため)。
    ///   本関数には到達しない。
    /// - Pending / Terminated: 呼び出し元の state 検証でセッション終了になるため
    ///   到達しない (本関数は state を見ない前提)。
    /// - TRACK_STATUS / 未知 id: 呼び出し元の request 種別判定 (`locate_request`) で
    ///   UNEXPECTED_MESSAGE に落ちるため到達しない (防御として閉じる)。
    ///
    /// `reason` は §8.5 (Reason Phrase Structure) の 1024 バイト上限以下であること
    /// (超過時は panic)。
    fn reject_request_update(
        &mut self,
        request_id: u64,
        error_code: u64,
        reason: &'static str,
    ) -> Result<(), SessionError> {
        match self.locate_request(request_id) {
            // draft-ietf-moq-transport-23 §9.5.1 (Updating Subscriptions): "When a
            // REQUEST_UPDATE fails for a FETCH, the publisher MUST reset the FETCH data
            // stream." の MUST と fetch state の遷移は `send_request_error`
            // (`send_err_for_fetch`) が担う。`emit_request_error` では reset が発行されない。
            // FETCH は未応答キューを持たないため、即時応答でも受信順を崩さない。
            Some(RequestTable::Fetch) => self.send_request_error(
                request_id,
                error_code,
                0,
                ReasonPhrase::new(reason)
                    .expect("request update rejection reason must be <= 1024 bytes"),
                ErrorDetails::new(),
            ),
            Some(RequestTable::Subscription) => {
                // 先行する未応答グループがあれば、拒否をキューへ積んで OK の後ろに回す
                let has_pending = self
                    .subscriptions
                    .get(&request_id)
                    .is_some_and(|subscription| !subscription.pending_update_params.is_empty());
                if has_pending {
                    return self.push_pending_update_group(
                        request_id,
                        PendingUpdateGroup::rejected(PendingUpdateRejection { error_code, reason }),
                    );
                }
                self.send_subscription_update_rejection(request_id, error_code, reason)
            }
            // draft §9.5 (REQUEST_UPDATE): 更新できない request への受信は
            // UNEXPECTED_MESSAGE で閉じる。呼び出し元の request 種別判定
            // (`locate_request`) で検出済み (防御)
            Some(RequestTable::TrackStatus) | None => {
                let err = SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "REQUEST_UPDATE received for unknown or unsupported request id",
                );
                self.fail(err.clone());
                Err(err)
            }
        }
    }

    /// subscription の未応答グループをキューへ追加する
    ///
    /// 自側 MAX_REQUEST_UPDATES が 0 (無制限) のときだけ [`MAX_PENDING_UPDATE_GROUPS`] を
    /// 実装上限として適用する。宣言済みなら peer はその数を超えられず
    /// (`handle_peer_request_update` が先に TOO_MANY_REQUEST_UPDATES で閉じる)、本上限は
    /// 発動しない。実装上限を超えた場合は、同じく未応答更新の過多を表す
    /// draft-ietf-moq-transport-23 §9.1.7 / §12.2 (Session Termination Codes) の
    /// `TOO_MANY_REQUEST_UPDATES` (0x1B) でセッションを閉じる。
    /// §12.2 は "the application MAY use any error message and SHOULD use a relevant code"
    /// と定めるため、実装固有の上限超過にも最も関連する本コードを使う
    /// (INTERNAL_ERROR では原因が伝わらない)。
    fn push_pending_update_group(
        &mut self,
        request_id: u64,
        group: PendingUpdateGroup,
    ) -> Result<(), SessionError> {
        let count = self
            .subscriptions
            .get(&request_id)
            .expect("locate_request guarantees key presence")
            .pending_update_params
            .len();
        // draft-ietf-moq-transport-23 §9.1.7 (MAX_REQUEST_UPDATES): 値 0 は
        // "the endpoint does not limit REQUEST_UPDATE concurrency" であるため、
        // 実装上限は宣言していない (local_max == 0) 場合だけ適用する。
        if self.local_max_request_updates() == 0 && count >= MAX_PENDING_UPDATE_GROUPS {
            let err = SessionError::new(
                SESSION_TOO_MANY_REQUEST_UPDATES,
                "pending REQUEST_UPDATE groups exceed implementation limit",
            );
            self.fail(err.clone());
            return Err(err);
        }
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        subscription.pending_update_params.push_back(group);
        Ok(())
    }

    /// REQUEST_UPDATE を受信できる Subscription かを検証する
    ///
    /// draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE): "An endpoint that receives a
    /// REQUEST_UPDATE other than in the two cases above MUST close the session with
    /// UNEXPECTED_MESSAGE." の MUST を満たす。
    ///
    /// - draft §3.1 (Subscriptions) / state diagram: REQUEST_UPDATE は Established の
    ///   self loop。Pending* / Terminated での受信は UNEXPECTED_MESSAGE
    /// - REQUEST_UPDATE を送れる側は (a) 元 request の initiator (自側は responder)、
    ///   (b) PUBLISH 経由の subscription の subscriber。自側が initiator かつ subscriber
    ///   (SUBSCRIBE 経由で peer が publisher) での受信は ROLE_VIOLATION
    ///
    /// `locally_terminated` は送信方向を reset 済みの request を表す。応答を送れないため
    /// state 検証のみ緩める (role 検証は応答の有無に依存しないため常に行う)。
    ///
    /// 違反時は session を Closing に遷移させて Err を返す。メッセージ単位の reject
    /// (§8.9 の不正トークン / §3.3.2 の INVALID_FILTER) より先に呼ぶこと。
    fn validate_subscription_request_update(
        &mut self,
        request_id: u64,
        locally_terminated: bool,
    ) -> Result<(), SessionError> {
        let subscription = self
            .subscriptions
            .get(&request_id)
            .expect("locate_request guarantees key presence");
        let state = subscription.state;
        let my_role = subscription.my_role;
        let is_initiator_self = subscription.is_initiator_self();
        if state != SubscriptionState::Established && !locally_terminated {
            let err = SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "REQUEST_UPDATE requires Established subscription",
            );
            self.fail(err.clone());
            return Err(err);
        }
        if is_initiator_self && my_role != TrackRole::Publisher {
            let err = SessionError::new(
                SESSION_ROLE_VIOLATION,
                "REQUEST_UPDATE cannot be received by subscriber initiator",
            );
            self.fail(err.clone());
            return Err(err);
        }
        Ok(())
    }

    fn handle_update_for_subscription(
        &mut self,
        request_id: u64,
        fill_request_id: u64,
        parameters: MessageParameters,
        locally_terminated: bool,
    ) -> Result<(), SessionError> {
        // ローカル終端済みの request (`rejected_request_ids` に記録。GOAWAY の deadline 満了や
        // malformed 終端) は送信方向を reset 済みだが、peer が reset を観測する前に送った
        // REQUEST_UPDATE は正当に届く。その場合は状態遷移とイベント発行をせずに return する
        // (Request ID / MAX_REQUEST_UPDATES / state・role / auth token / パラメータスコープ /
        // PAUSED 値域の検証は呼び出し元 `handle_peer_request_update` で済んでいる)。
        // なお state テーブルから破棄済み (`forget_*` 後) の request への REQUEST_UPDATE は、
        // 送信者を特定できないため従来どおり UNEXPECTED_MESSAGE になる。
        let subscription = self
            .subscriptions
            .get(&request_id)
            .expect("locate_request guarantees key presence");
        // draft-ietf-moq-transport-23 §9.19.19 (NEW GROUP REQUEST Parameter): subscriber には
        // "MUST NOT send this parameter in REQUEST_UPDATE if it has received the Track's
        // Properties and they did not include the DYNAMIC_GROUPS Property with value 1." が
        // 課されるが、同節は受信側の close を規定しない。同節は動的 Group に対応しない
        // original publisher について "it ignores the parameter" と定めるため、本実装は
        // NEW_GROUP_REQUEST を拒否せず無視する (パラメータはアプリへ渡し、キーフレームを
        // 要求できるかはアプリが `dynamic_groups` と自側の能力から判断する)。
        // 本実装はリレーではないため、同節の Relay Handling (Established でない subscription
        // の upstream への転送、outstanding な要求の管理) は行わない。
        //
        // draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter) の値域検証は
        // 呼び出し元 `handle_peer_request_update` が dispatch 前に済ませている
        // (同一の `MessageParameters` を渡すため、ここで再検証しても到達しない)。
        // ローカル終端済みの request は、呼び出し元 `handle_peer_request_update` と
        // `validate_subscription_request_update` が済ませた MUST 検証だけを通し、状態遷移と
        // イベント発行 (pending_update_params / RequestUpdateReceived / fill stream 開設) はしない
        if locally_terminated {
            return Ok(());
        }
        // draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE): 受信側は REQUEST_UPDATE を
        // 受信順に処理し、更新ごとに 1 つの REQUEST_UPDATE_OK / REQUEST_UPDATE_ERROR を返す。
        // 処理の合体 (coalescing) は累積結果のみを適用する形で許されるが、FILL_PARAMETERS を
        // 含む更新は合体できない (§9.19.15 (FILL PARAMETERS Parameter))。そのため未応答の
        // 更新は「受信順のグループ」として保持し、FILL_PARAMETERS を含む更新の前後を境界に
        // する。先頭グループは `send_request_ok` が応答時に適用して取り除く。
        //
        // `view` は未応答の全グループと今回のメッセージを先頭から合体した
        // 「受信順に処理した場合の累積表示」である。累積上限の検証と fill fetch stream の
        // 開設判定、アプリへの通知に使う (合体は複製の上で行う)。
        let view = subscription.pending_update_view(&parameters);
        // draft-ietf-moq-transport-23 §9.1.6 (MAX FILTER RANGES) が宣言する上限は、同節の
        // "the peer's total number of Ranges (Start/End pairs) allowed concurrently in all
        // Range filter Section 3.3.2 parameters for a given subscription or fetch" であり、
        // 1 メッセージ単位ではなく subscription 単位の同時保持数である。`merge_from` は
        // Range Filter を型単位で全置換するので同一型では累積しないが、型をまたぐと
        // 累積するため、マージ後の総数を再検証する。
        //
        // 総数は FILL_PARAMETERS の内側も合算する (`range_filter_usage` の doc を参照)。
        // グループのパラメータからは FILL_PARAMETERS を除去しているため `view` の内側は
        // 常に今回のメッセージのものであり、合算値は「マージ後の外側の Range 総数 (同一型は
        // 今回のメッセージの値で置換済み) + 今回メッセージの内側の Range 総数」になる
        // (前回メッセージの内側を二重計上しない)。
        // 有無の判定は総数の比較に含まれる (総数が 1 以上なら Range Filter パラメータも必ず
        // 存在する) ため、ここでは総数だけを見る。
        let (_, accumulated_ranges) = range_filter_usage(&view);
        if accumulated_ranges > self.local_max_filter_ranges() {
            self.reject_request_update(
                request_id,
                REQUEST_INVALID_FILTER,
                "cumulative Range Filters exceed MAX_FILTER_RANGES",
            )?;
            return Ok(());
        }
        // グループの境界を決めて今回の更新を蓄積する。FILL_PARAMETERS を含む更新は
        // 単独のグループとし (前の通常更新にも後続の通常更新にも合体しない)、
        // FILL_PARAMETERS を含むグループの直後の通常更新も新しいグループを開始する。
        // 拒否待ちグループもパラメータを適用しない境界であるため、その後続は合体せず
        // 新しいグループを開始する。それ以外の通常更新は末尾グループへ累積する
        // (後の値が前を上書きする)。
        let has_fill_parameters = parameters.fill_parameters().is_some();
        let start_new_group = {
            let subscription = self
                .subscriptions
                .get(&request_id)
                .expect("locate_request guarantees key presence");
            let tail = subscription.pending_update_params.back();
            has_fill_parameters
                || tail.is_some_and(|group| group.has_fill_parameters)
                || tail.is_some_and(PendingUpdateGroup::is_rejection)
                || subscription.pending_update_params.is_empty()
        };
        if start_new_group {
            // draft-ietf-moq-transport-23 §9.19.15 (FILL PARAMETERS Parameter):
            // FILL_PARAMETERS は運んできたメッセージにのみ適用され subscription 状態として
            // 保持されない (sticky 対象外) ため、グループのパラメータから除外する。
            // アプリへ通知する累積表示 (`view`) には残る。
            let mut group_parameters = parameters.clone();
            group_parameters.remove_type(PARAM_FILL_PARAMETERS);
            self.push_pending_update_group(
                request_id,
                PendingUpdateGroup {
                    parameters: group_parameters,
                    has_fill_parameters,
                    rejection: None,
                },
            )?;
        } else {
            let subscription = self
                .subscriptions
                .get_mut(&request_id)
                .expect("locate_request guarantees key presence");
            if let Some(tail) = subscription.pending_update_params.back_mut() {
                tail.parameters.merge_from(&parameters);
            }
        }
        let subscription = self
            .subscriptions
            .get(&request_id)
            .expect("locate_request guarantees key presence");
        // draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams):
        // FILL_PARAMETERS 付き REQUEST_UPDATE を処理したら fill fetch stream の
        // 開設を判定する。処理後観点の paused と filter で評価する
        // (PAUSED を省略した REQUEST_UPDATE は現在値の維持なので、現状値へフォールバックする)。
        // この判定は今回の更新の受信時点で行うため、後続の更新の値は反映されない。
        let post_paused = match view.paused() {
            Some(value) => validate_paused(value)?,
            None => subscription.paused,
        };
        let post_filter: Option<LocationFilter> = match view.location_filter_update() {
            Ok(LocationFilterUpdate::Set(filter)) => Some(filter),
            // Type 0x00 (None) はフィルタ削除、省略時は現 filter を使う
            Ok(LocationFilterUpdate::Removed) => None,
            Ok(LocationFilterUpdate::Unchanged) => subscription.filter,
            // デコード済みメッセージでは到達しない。安全側に現 filter を使う。
            Err(_) => subscription.filter,
        };
        // draft-ietf-moq-transport-23 §9.19.8 (GROUP ORDER Parameter): GROUP_ORDER は確定後変更不可 (draft-ietf-moq-transport-23 §5.1.1 (Definitions)) のため更新しない
        self.events.push_back(SessionEvent::RequestUpdateReceived {
            request_id,
            parameters: view,
        });
        self.maybe_open_fill_stream(
            fill_request_id,
            request_id,
            parameters.fill_parameters(),
            post_filter,
            // REQUEST_UPDATE は LARGEST_OBJECT を運ばないため、Largest Object は
            // 自側の観測値から解決する
            None,
            post_paused,
        );
        Ok(())
    }

    /// peer publisher からの PUBLISH_STATE_NOTIFY を受信する
    ///
    /// draft-ietf-moq-transport-23 §9.10 (PUBLISH_STATE_NOTIFY):
    /// publisher のみが subscription の bidi stream 上で送る一方向通知であり、
    /// 応答は不要、MAX_REQUEST_UPDATES のクレジットも消費しない。
    /// subscription 以外への受信・ subscriber からの受信は MUST でセッションを
    /// UNEXPECTED_MESSAGE / ROLE_VIOLATION により閉じる。PAUSED / LOCATION_FILTER /
    /// LARGEST_OBJECT は subscription 状態に即時反映する。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub(crate) fn handle_peer_publish_state_notify(
        &mut self,
        request_id: u64,
        notify: PublishStateNotify,
    ) -> Result<(), SessionError> {
        let Some(subscription) = self.subscriptions.get(&request_id) else {
            let err = SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "PUBLISH_STATE_NOTIFY received for unknown or unsupported request id",
            );
            self.fail(err.clone());
            return Err(err);
        };
        // draft-ietf-moq-transport-23 §9.10 (PUBLISH_STATE_NOTIFY):
        // subscription 以外・ subscriber からの受信は ROLE_VIOLATION (MUST)。
        // `subscriptions` 索引に無い request (fetch / namespace 系等) は上記で弾かれる。
        if subscription.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_ROLE_VIOLATION,
                "PUBLISH_STATE_NOTIFY received on publisher side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft §3.1 (Subscriptions): REQUEST_UPDATE と同様、確立済みの
        // subscription に対する通知のみ受理する。ローカル終端済みの request
        // (`rejected_request_ids` に記録) では、peer が reset を観測する前に送った通知が
        // 正当に届くため、残りの検証 (PAUSED / LOCATION_FILTER) だけを行い状態遷移はしない。
        // 破棄済み (`forget_*` 後) の request への通知は送信者を特定できないため
        // 従来どおり UNEXPECTED_MESSAGE になる。
        let locally_terminated = self.is_locally_terminated_request(request_id);
        if subscription.state != SubscriptionState::Established && !locally_terminated {
            let err = SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "PUBLISH_STATE_NOTIFY requires Established subscription",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter): 値域外
        // (0 / 1 以外) の受信は MUST でセッションを INVALID_PARAMETER (0x305) により閉じる。
        if let Some(value) = notify.parameters.paused()
            && let Err(err) = validate_paused(value)
        {
            self.fail(err.clone());
            return Err(err);
        }
        // LOCATION_FILTER の形式検証を受信状態の適用より前に行う。`MessageError` から
        // `SessionError` への `From` 実装はないため `?` では伝播できない。
        let filter_update = match notify.parameters.location_filter_update() {
            Ok(update) => update,
            // 受信メッセージでは decode 層が値域と形式を検証済みのため到達しない。
            // API 経由で手組みしたメッセージに対しては安全側にセッションを閉じる。
            Err(msg_err) => {
                let err = crate::session::core::session_error_from_message(
                    &msg_err,
                    SESSION_INVALID_PARAMETER,
                );
                self.fail(err.clone());
                return Err(err);
            }
        };
        // ローカル終端済みの request はここまでの MUST 検証だけを行い、状態遷移と
        // イベント発行はしない
        if locally_terminated {
            return Ok(());
        }
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("subscription presence already checked above");
        // draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter): PUBLISH_STATE_NOTIFY で
        // PAUSED を省略した場合は現在値を維持する。通知は publisher が「購読が paused か
        // どうか」を報告するものであり、ここでは受信側の追跡状態へそのまま反映する。
        if let Some(value) = notify.parameters.paused() {
            subscription.paused = validate_paused(value)?;
        }
        // draft-ietf-moq-transport-23 §3.3.1 (Location Filters) / §9.19.9 (LOCATION FILTER Parameter):
        // Location Filter Type 0x00 (None) はフィルタ削除、省略時は値 unchanged。解決済み値もフィルタに追随させる。
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
        // draft-ietf-moq-transport-23 §9.19.17 (LARGEST OBJECT Parameter):
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
        // 借用の都合で可変借用より前に判定する
        let locally_terminated = self.is_locally_terminated_request(request_id);
        let Some(subscription) = self.subscriptions.get_mut(&request_id) else {
            let err = SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "PUBLISH_DONE received for unknown request id",
            );
            self.fail(err.clone());
            return Err(err);
        };
        if subscription.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_ROLE_VIOLATION,
                "PUBLISH_DONE received on publisher side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        if locally_terminated {
            return Ok(());
        }
        // draft §3.1 (Subscriptions): publisher は Pending (Publisher) / Established
        // からしか PUBLISH_DONE を送れない。したがって peer が SUBSCRIBE の
        // initiator であり自側が publisher responder となる Pending(Subscriber) からの
        // PUBLISH_DONE 受信は UNEXPECTED_MESSAGE。
        //
        // ただし REQUEST_UPDATE 失敗応答経路の `Terminated` (draft §3.1.2 の
        // REQUEST_ERROR 受信で subscription state を終える帰結として
        // `handle_err_for_subscription` の Established 分岐で遷移) には、
        // publisher が §9.5.1 の MUST に基づき送る PUBLISH_DONE(UPDATE_FAILED) が続く。
        // これを拒否するとセッションが UNEXPECTED_MESSAGE で閉じられる回帰になるため、
        // 未記録 (`publish_done` が `None`) の `Terminated` に届く PUBLISH_DONE を受理する。
        // 受理条件はデータプレーン吸収 `is_cancelled_terminated` (`src/session/data.rs`) と同一で、
        // 記録済み (`publish_done` が `Some`) への重複 PUBLISH_DONE は従来どおり拒否される。
        // draft §3.1.2: "A REQUEST_ERROR indicates no objects will be delivered" は
        // publisher 側の送信禁止であり、REQUEST_ERROR 後に届く PUBLISH_DONE の drain 期間に
        // オブジェクトが届く実害はない。
        let allow_publish_done = subscription.is_pending_publisher()
            || subscription.state == SubscriptionState::Established
            || (subscription.state == SubscriptionState::Terminated
                && subscription.publish_done.is_none());
        if !allow_publish_done {
            let err = SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "PUBLISH_DONE requires Pending(Publisher), Established, or unrecorded Terminated state",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-23 §6.4.2.2 (Graceful Request Stream Closure):
        // "A FIN sent by the responder after its response and any subsequent messages for the
        // request signals that the request is complete". PUBLISH 起点の subscription では
        // 自側が PUBLISH を受けた subscriber responder であり、PUBLISH_DONE の受信で
        // 自側が送るべきメッセージは無くなる。したがって送信方向を FIN で閉じる。
        //
        // §6.4.2.2 "An endpoint MUST NOT send a FIN on a direction of a request stream until
        // it has sent all required messages on that direction for its request type." より、
        // 必須応答 (PUBLISH_OK / REQUEST_ERROR) を送り終えた `Established` からの遷移に限る。
        // `Pending(Publisher)` からの FIN は peer に request 失敗と扱われるため発行しない。
        // 既に `Terminated` の経路 (REQUEST_UPDATE 失敗応答に伴う PUBLISH_DONE(UPDATE_FAILED)
        // の受信) も対象外であり、そちらは SUBSCRIBE 起点 requester の事象として
        // `Session::recv_request_stream_closed` の requester 経路が FIN を担う。
        let was_established = subscription.state == SubscriptionState::Established;
        // Pending(Publisher) / Established / Terminated (未記録) のいずれも Terminated に確定させる
        subscription.state = SubscriptionState::Terminated;
        record_publish_done(subscription, &done, now_ms);
        // PUBLISH_DONE の受信で subscription が終端したため、未応答の REQUEST_UPDATE と
        // MAX_REQUEST_UPDATES のクレジットを解放する。
        self.release_request_update_state(request_id);
        if was_established {
            self.events
                .push_back(SessionEvent::FinishRequestStream { request_id });
        }
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
