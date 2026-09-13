//! REQUEST_OK / REQUEST_ERROR の送受信とタイマー処理
//! draft-ietf-moq-transport-21 §9.3-§9.4, §9.5.1 に対応。将来 draft 側で変更される可能性がある。

use crate::error::{SESSION_PROTOCOL_VIOLATION, STREAM_DELIVERY_TIMEOUT};
use crate::message::{
    PUBLISH_OK_ALLOWED_PARAMS, REQUEST_UPDATE_OK_ALLOWED_PARAMS, common::Location,
};
use crate::message_parameter::{
    LocationFilterContext, LocationFilterUpdate, MessageParameters, PARAM_LARGEST_OBJECT,
};
use alloc::vec::Vec;

use super::super::core::Session;
use super::super::types::{
    DataStreamId, RequestKind, SessionError, SessionEvent, SubscriptionState, TrackRole,
};
use super::delivery::{
    effective_largest_object, set_subscription_expires, update_largest_object_in_parameters,
    update_subscription_expires_if_present,
    update_subscription_subscriber_delivery_timeouts_if_present,
};
use super::validation::{extract_range_filters, resolve_location_filter, validate_forward};

impl Session {
    pub(crate) fn send_ok_for_subscription(
        &mut self,
        request_id: u64,
        parameters: &mut MessageParameters,
    ) -> Result<(), SessionError> {
        let now_ms = self.timing.last_tick_ms;
        let subscription = self
            .subscriptions
            .get(&request_id)
            .expect("locate_request guarantees key presence");
        if subscription.is_initiator_self() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_ok (subscription) can only be sent by responder",
            ));
        }
        // 末尾の LARGEST_OBJECT 保存判定に使う `my_role` を、match による更新前に
        // Copy 退避し、冒頭の immutable な get 借用をここで終わらせる。
        let my_role = subscription.my_role;
        // draft §9.20.1 (Parameter Scope): Subscription context は state で
        // PUBLISH_OK / REQUEST_UPDATE_OK に分岐する。状態遷移の前にスコープ検証を
        // 行うため、状態遷移前の context を保存する。末尾の LARGEST_OBJECT 注入判定
        // (context が許可する場合のみ注入) にも使い回す。
        // 将来 draft 改版で変更される可能性がある。
        let allowed_params_for_context: Option<(&[u64], &str)> = match subscription.state {
            SubscriptionState::Pending if subscription.is_pending_publisher() => Some((
                PUBLISH_OK_ALLOWED_PARAMS,
                "REQUEST_OK (publish) parameter not allowed in this context",
            )),
            SubscriptionState::Established => Some((
                REQUEST_UPDATE_OK_ALLOWED_PARAMS,
                "REQUEST_OK (request_update) parameter not allowed in this context",
            )),
            _ => None,
        };
        // draft §9.20.1 (Parameter Scope): スコープ外パラメータを含む REQUEST_OK では
        // 状態を一切変更せずエラーを返す。受信側はスコープ外パラメータで
        // PROTOCOL_VIOLATION により接続を閉じる (MUST) ため、送信側でも
        // 状態遷移・パラメータ適用より前に検証して自側の状態整合性を保つ。
        // 検証が遷移後に走ると、PUBLISH_OK 経路では subscription が Established のまま、
        // REQUEST_UPDATE_OK 経路では pending_update_params が消費されパラメータが
        // 適用済みのまま peer と乖離して孤立する。
        if let Some((allowed, message)) = allowed_params_for_context
            && parameters.validate_scope(allowed).is_err()
        {
            return Err(SessionError::new(SESSION_PROTOCOL_VIOLATION, message));
        }
        match subscription.state {
            // draft §9.3 (REQUEST_OK): PUBLISH_OK context → REQUEST_OK に統一。
            // Pending(Publisher) → Established 遷移
            SubscriptionState::Pending if subscription.is_pending_publisher() => {
                // draft-ietf-moq-transport-21 Appendix A.1 #1790 (Subscription parameters
                // appear in REQUEST_UPDATE, not PUBLISH_OK): PUBLISH_OK は EXPIRES のみを
                // 運び、subscription 更新用パラメータは REQUEST_UPDATE (要求) に出現する。
                // スコープ検証で EXPIRES 以外は既に弾かれているため、ここでは EXPIRES の
                // 適用のみ行う。filter / delivery timeout / priority / Range Filter は
                // 既定値のままであり、forward_state は PUBLISH 送受信時の値のまま維持する。
                // 更新は REQUEST_UPDATE (要求) 経路に委譲する。
                self.clear_control_message_deadline(request_id);
                let subscription = self
                    .subscriptions
                    .get_mut(&request_id)
                    .expect("locate_request guarantees key presence");
                subscription.state = SubscriptionState::Established;
                set_subscription_expires(subscription, parameters.expires(), now_ms);
            }
            // REQUEST_UPDATE への成功応答: state は維持
            SubscriptionState::Established => {
                let subscription = self
                    .subscriptions
                    .get_mut(&request_id)
                    .expect("locate_request guarantees key presence");
                // draft §9.5.1 (Updating Subscriptions): 合体された REQUEST_UPDATE の
                // 累積パラメータを subscription state に適用する。
                // 現状の draft-ietf-moq-transport-21 では REQUEST_UPDATE に EXPIRES は
                // 許可されていないため、pending_update_params から EXPIRES が到達することは
                // ないが、将来の draft 変更に備えて適用しておく。
                if let Some(pending_params) = subscription.pending_update_params.take() {
                    update_subscription_subscriber_delivery_timeouts_if_present(
                        subscription,
                        &pending_params,
                    );
                    update_subscription_expires_if_present(subscription, &pending_params, now_ms);
                    if let Some(f) = pending_params.forward() {
                        let new_forward = validate_forward(f)?;
                        // draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams):
                        // STOP_SENDING 後の再オープンは Forward State が 0 から 1 へ変わった
                        // REQUEST_UPDATE が受理された場合のみ許可する。現在値が 1 である
                        // ことでは解除しない (PUBLISH_STATE_NOTIFY などの他経路でも解除しない)
                        if subscription.forward_state == 0 && new_forward == 1 {
                            self.stopped_outgoing_subgroups.remove(&request_id);
                        }
                        subscription.forward_state = new_forward;
                    }
                    match pending_params.location_filter_update() {
                        // 省略時は値 unchanged、Length 0 はフィルタ削除
                        Ok(LocationFilterUpdate::Unchanged) => {}
                        Ok(LocationFilterUpdate::Removed) => {
                            subscription.filter = None;
                            subscription.filter_start = None;
                            subscription.filter_end = None;
                        }
                        Ok(LocationFilterUpdate::Set(filter)) => {
                            subscription.filter = Some(filter);
                        }
                        Err(_) => {
                            // draft-ietf-moq-transport-21 §3.3.1 (Location Filters): End Group 溢出は
                            // MUST close the session with PROTOCOL_VIOLATION。壊れた値は §8.3 の
                            // KEY_VALUE_FORMATTING_ERROR だが、セッション層では同じく閉じる
                            let err = SessionError::new(
                                SESSION_PROTOCOL_VIOLATION,
                                "invalid subscription filter encoding in pending REQUEST_UPDATE",
                            );
                            self.fail(err.clone());
                            return Err(err);
                        }
                    }
                    if let Some(priority) = pending_params.subscriber_priority() {
                        subscription.subscriber_priority = Some(priority);
                    }
                    // draft §3.3.2 (Range Filters): "In REQUEST_UPDATE, Length of 0 removes the
                    // filter; non-zero replaces it entirely. If a filter parameter is omitted
                    // from REQUEST_UPDATE, it is unchanged."
                    // `merge_from` が型単位の全置換と Length=0 削除を済ませているので、
                    // 累積パラメータからそのまま作り直せばこの規則を満たす。
                    if pending_params.has_range_filters() {
                        subscription.range_filters = extract_range_filters(&pending_params);
                    }
                    // draft §3.3.1 (Location Filters): 相対フィルタは更新時点の largest で解決する。
                    // フィルタ不変 (Unchanged) の場合も応答時点の largest で再解決する (旧来挙動)。
                    let largest = subscription.largest_received_location;
                    let (start, end) = resolve_location_filter(
                        subscription.filter.as_ref(),
                        largest.as_ref(),
                        LocationFilterContext::Subscription,
                    );
                    subscription.filter_start = start;
                    subscription.filter_end = end;
                }
                // REQUEST_OK / REQUEST_UPDATE_OK パラメータに EXPIRES が含まれれば更新し、
                // 含まれなければ前回値を維持する。
                update_subscription_expires_if_present(subscription, parameters, now_ms);
            }
            _ => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "request_ok (subscription) requires Pending(Publisher) or Established state",
                ));
            }
        }
        // draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter): LARGEST_OBJECT は actual received 以上でなければならない
        if let Some(subscription) = self.subscriptions.get_mut(&request_id) {
            // LARGEST_OBJECT を許可する context (REQUEST_UPDATE_OK / TRACK_STATUS_OK 経路) でのみ
            // max 合流を行う。PUBLISH_OK context では §9.20.18 (LARGEST OBJECT Parameter) が
            // PUBLISH_OK を列挙していないため注入しない。
            let context_allows_largest_object = allowed_params_for_context
                .is_some_and(|(allowed, _)| allowed.contains(&PARAM_LARGEST_OBJECT));
            if context_allows_largest_object
                && let Some(ref effective) = effective_largest_object(subscription)
            {
                update_largest_object_in_parameters(parameters, effective);
            }
            // LARGEST_OBJECT の保存は自側が subscriber の場合に限る
            // (旧称 Joining Location の publisher 側保存は draft-20 で廃止された)。
            // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
            if my_role != TrackRole::Publisher
                && let Some((group_id, object_id)) = parameters.largest_object()
            {
                subscription.largest_location = Some(Location {
                    group_id,
                    object_id,
                });
            }
        }

        Ok(())
    }

    /// subscription 用 REQUEST_ERROR の送信時状態遷移
    ///
    /// 戻り値:
    /// - `Ok(Some(stream_count))`: Established から Terminated に遷移した。
    ///   呼出元は PUBLISH_DONE(UPDATE_FAILED) を送信する。open 中の outgoing subgroup
    ///   stream がある場合は §9.9 の MUST NOT に従い push を保留し、全 stream 終端後に
    ///   自動送信する (送信の実装は `send_request_error` 側)。
    /// - `Ok(None)`: Pending から Terminated に遷移した。PUBLISH_DONE は不要。
    /// - `Err(_)`: Terminated からの送信は不正。
    pub(crate) fn send_err_for_subscription(
        &mut self,
        request_id: u64,
    ) -> Result<Option<u64>, SessionError> {
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        // draft §9.4 (REQUEST_ERROR) / §3.1 (Subscriptions): subscription 用 REQUEST_ERROR は
        //   - `Pending` → 初回 SUBSCRIBE / PUBLISH への失敗応答 (Terminated に遷移)
        //   - `Established` → REQUEST_UPDATE への失敗応答 (§9.5 (REQUEST_UPDATE))。
        //     Established からの送信は本関数で Terminated に遷移し、呼出元が
        //     PUBLISH_DONE(UPDATE_FAILED) を送信する
        //     (draft §9.5.1 (Updating Subscriptions): "the publisher MUST also
        //     terminate the subscription by sending a PUBLISH_DONE with error
        //     code UPDATE_FAILED")
        // の 2 ケース。Terminated からの送信は不正。
        match subscription.state {
            SubscriptionState::Pending => {
                subscription.state = SubscriptionState::Terminated;
                // draft-ietf-moq-transport-21 §3.4.1 (Opening and Closing Fill Fetch Streams):
                // subscription の終了時は open 中の fill fetch stream を reset する (MUST)。
                // SUBSCRIBE 処理直後に開いた fill stream が残っている場合が対象。
                self.reset_open_fill_streams(request_id);
                Ok(None)
            }
            SubscriptionState::Established => {
                // draft §9.5.1: REQUEST_UPDATE 失敗時、publisher は
                // PUBLISH_DONE(UPDATE_FAILED) を送り subscription を終了する MUST。
                // draft §9.9 (PUBLISH_DONE) は "A publisher sends a PUBLISH_DONE message" と
                // 規定するため、subscriber 側からの REQUEST_ERROR 応答 → PUBLISH_DONE 送出は
                // ロール逆転 (draft §9 の MUST 違反) となる。ここで検出しないと release ビルド
                // では debug_assert が消え、後続で subscriber 側から PUBLISH_DONE が誤送出される。
                if subscription.my_role != TrackRole::Publisher {
                    let err = SessionError::new(
                        SESSION_PROTOCOL_VIOLATION,
                        "REQUEST_ERROR on Established subscription requires publisher role",
                    );
                    self.fail(err.clone());
                    return Err(err);
                }
                let stream_count = subscription.stream_counts.published_count;
                subscription.pending_update_params = None;
                subscription.state = SubscriptionState::Terminated;
                // draft-ietf-moq-transport-21 §3.4.1 (Opening and Closing Fill Fetch Streams):
                // subscription の終了時は open 中の fill fetch stream を reset する (MUST)。
                // PUBLISH_DONE push は send_request_error 側で行う (§9.9 の MUST NOT に
                // 従い、open 中の stream がある場合は保留される。fill stream も
                // `has_open_outgoing_data_streams_for_request` の対象に含まれる)。
                self.reset_open_fill_streams(request_id);
                // PUBLISH_DONE push は send_request_error 側で行う (§9.9 の MUST NOT に
                // 従い、open 中の outgoing subgroup stream がある場合は保留される)
                Ok(Some(stream_count))
            }
            SubscriptionState::Terminated => Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "send_request_error requires non-terminated state (subscription)",
            )),
        }
    }

    pub(crate) fn handle_ok_for_subscription(
        &mut self,
        request_id: u64,
        parameters: &MessageParameters,
    ) -> Result<(), SessionError> {
        let now_ms = self.timing.last_tick_ms;
        let subscription = self
            .subscriptions
            .get(&request_id)
            .expect("locate_request guarantees key presence");
        if !subscription.is_initiator_self() {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_OK received on responder side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft §9.20.1 (Parameter Scope): Subscription context の REQUEST_OK は
        // subscription.state で PUBLISH_OK (Pending(Publisher)) と REQUEST_UPDATE_OK
        // (Established) に確定する。それぞれの許可パラメータ集合で検証し、許可外
        // パラメータは PROTOCOL_VIOLATION でセッションをクローズする。Established 分岐は
        // 元々 largest_object / expires しか参照しないため、この検証追加は additive。
        let context_allowed = match subscription.state {
            SubscriptionState::Pending if subscription.is_pending_publisher() => Some((
                PUBLISH_OK_ALLOWED_PARAMS,
                "REQUEST_OK (publish) parameter not allowed in this context",
            )),
            SubscriptionState::Established => Some((
                REQUEST_UPDATE_OK_ALLOWED_PARAMS,
                "REQUEST_OK (request_update) parameter not allowed in this context",
            )),
            _ => None,
        };
        if let Some((allowed, message)) = context_allowed
            && parameters.validate_scope(allowed).is_err()
        {
            let err = SessionError::new(SESSION_PROTOCOL_VIOLATION, message);
            self.fail(err.clone());
            return Err(err);
        }
        // context 検証後、状態遷移のため可変借用を取り直す (検証中は共有借用 + self.fail のため
        // get_mut を保持できなかった)
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        match subscription.state {
            // draft §9.3 (REQUEST_OK): PUBLISH_OK context → REQUEST_OK に統一。
            // Pending(Publisher) → Established 遷移
            SubscriptionState::Pending if subscription.is_pending_publisher() => {
                // draft-ietf-moq-transport-21 Appendix A.1 #1790 (Subscription parameters
                // appear in REQUEST_UPDATE, not PUBLISH_OK): PUBLISH_OK は EXPIRES のみを
                // 運ぶ。スコープ検証で EXPIRES 以外は既に弾かれているため、ここでは
                // EXPIRES の適用と Established 遷移のみ行う。filter / forward 等の
                // 更新は REQUEST_UPDATE (要求) 経路に委譲する。
                self.clear_control_message_deadline(request_id);
                let subscription = self
                    .subscriptions
                    .get_mut(&request_id)
                    .expect("locate_request guarantees key presence");
                subscription.state = SubscriptionState::Established;
                set_subscription_expires(subscription, parameters.expires(), now_ms);
                self.events.push_back(SessionEvent::RequestOkReceived {
                    request_id,
                    request_kind: RequestKind::Publish,
                    parameters: parameters.clone(),
                });
            }
            // REQUEST_UPDATE への成功応答: state は維持
            SubscriptionState::Established => {
                // LARGEST_OBJECT の保存は自側が subscriber の場合に限る。
                // publisher 側の保存は draft-20 で廃止された
                // (draft-ietf-moq-transport-21 Appendix A.1 #1872)。
                // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
                if subscription.my_role != TrackRole::Publisher
                    && let Some((group_id, object_id)) = parameters.largest_object()
                {
                    subscription.largest_location = Some(Location {
                        group_id,
                        object_id,
                    });
                }
                update_subscription_expires_if_present(subscription, parameters, now_ms);
                self.events.push_back(SessionEvent::RequestOkReceived {
                    request_id,
                    request_kind: match subscription.my_role {
                        TrackRole::Publisher => RequestKind::Publish,
                        TrackRole::Subscriber => RequestKind::Subscribe,
                    },
                    parameters: parameters.clone(),
                });
            }
            _ => {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_OK for subscription requires Pending(Publisher) or Established state",
                );
                self.fail(err.clone());
                return Err(err);
            }
        }
        Ok(())
    }

    /// subscription 用 REQUEST_ERROR の受信時状態遷移
    ///
    /// draft-ietf-moq-transport-21 §3.1.1 (Subscription State Management):
    /// "A subscriber keeps subscription state until it cancels the request
    /// (see Section 6.4.2.3), or until receipt of a PUBLISH_DONE or REQUEST_ERROR."
    /// REQUEST_ERROR の受信を subscription state を終える明示的な条件として列挙するため、
    /// REQUEST_UPDATE 失敗応答 (Established 分岐) でも `Terminated` に遷移させる
    /// (§9.5 (REQUEST_UPDATE) の失敗応答も「REQUEST_ERROR を受信した」に変わりはない)。
    /// これにより、subscriber 側が Established を維持したまま再 REQUEST_UPDATE を送り、
    /// 既に `Terminated` に遷移済みの publisher 側で PROTOCOL_VIOLATION が発生する窓を閉じる。
    ///
    /// 遷移後の帰結:
    /// - `send_request_update` は `Established` 要求のためエラーを返し、再送信されない
    /// - `forget_subscription` が可能になる
    /// - publisher が §9.5.1 の MUST に基づき送信する PUBLISH_DONE(UPDATE_FAILED) は
    ///   `handle_peer_publish_done` が `Terminated` かつ `publish_done` が `None` の
    ///   subscription への受信を許容することで受理される
    pub(crate) fn handle_err_for_subscription(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let subscription = self
            .subscriptions
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        match subscription.state {
            SubscriptionState::Pending => {
                subscription.state = SubscriptionState::Terminated;
                // draft-ietf-moq-transport-21 §3.1.1: REQUEST_ERROR → 即時破棄 (drain timer 不要)
                subscription.publish_done = None;
            }
            SubscriptionState::Established => {
                // draft §3.1.1: REQUEST_ERROR の受信で subscription state を終える。
                // REQUEST_UPDATE 失敗応答も同じく Terminated に遷移させ、再 REQUEST_UPDATE の窓を閉じる。
                subscription.state = SubscriptionState::Terminated;
                // draft-ietf-moq-transport-21 §3.1.1: REQUEST_ERROR → 即時破棄 (drain timer 不要)
                subscription.publish_done = None;
            }
            SubscriptionState::Terminated => {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_ERROR on terminated subscription",
                );
                self.fail(err.clone());
                return Err(err);
            }
        }
        Ok(())
    }

    pub(crate) fn tick_subscription_timeouts(&mut self, now_ms: u64) {
        for subscription in self.subscriptions.values_mut() {
            if let Some(expires) = subscription.expires.as_mut() {
                expires.tick(now_ms);
            }
            if let Some(publish_done) = subscription.publish_done.as_mut() {
                publish_done.drain.tick(now_ms);
            }
        }
        // OBJECT_DELIVERY_TIMEOUT の判定
        // draft-ietf-moq-transport-21 §5.2 (Delivery Timeouts and Data Reliability): object header の
        // 最終バイト提供からの経過時間が OBJECT_DELIVERY_TIMEOUT を超過した場合、
        // DELIVERY_TIMEOUT で stream を reset する (MUST)
        // キーは (stream_id, object_id) の複合キー。同一 stream_id のいずれかの object が
        // timeout を超過したら、当該ストリーム全体を reset する。
        let mut timed_out: Vec<DataStreamId> = self
            .timing
            .object_delivery_header_complete_ms
            .iter()
            .filter_map(|(&(stream_id, _object_id), &header_complete_opt)| {
                let stream = self.data_streams.outgoing.get(&stream_id)?;
                let request_id = &stream.request_id;
                let subscription = self.subscriptions.get(request_id)?;
                let timeout_ms = subscription.delivery_timeouts.effective_object_ms?;
                let header_complete_ms = header_complete_opt.unwrap_or(now_ms);
                if now_ms.saturating_sub(header_complete_ms) >= timeout_ms {
                    Some(stream_id)
                } else {
                    None
                }
            })
            .collect();
        // 未確定のエントリを現在時刻で確定する (tick 初回到達時)
        for (&(stream_id, _object_id), header_complete_opt) in
            self.timing.object_delivery_header_complete_ms.iter_mut()
        {
            if header_complete_opt.is_none() && self.data_streams.outgoing.contains_key(&stream_id)
            {
                *header_complete_opt = Some(now_ms);
            }
        }
        // datagram_header_complete_ms の未確定エントリを現在時刻で確定する (tick 初回到達時)
        // 全走査ではなく Pending 集合のみ確定する。確定済みエントリは触らないため、
        // subscription の forget まで保持される件数が増えても tick コストは O(pending) のまま。
        let pending: Vec<(u64, u64, u64)> = self.timing.datagram_pending_ms.drain().collect();
        for key in pending {
            if let Some(slot) = self.timing.datagram_header_complete_ms.get_mut(&key)
                && slot.is_none()
            {
                *slot = Some(now_ms);
            }
        }
        // subgroup_delivery_fin_ms の未確定エントリを現在時刻で確定する
        // object_delivery_header_complete_ms と異なり stream は send_data_stream_closed で
        // 既に data_streams.outgoing から除去されているため、生存チェックは不要。
        for (_stream_id, (fin_opt, _request_id)) in self.timing.subgroup_delivery_fin_ms.iter_mut()
        {
            if fin_opt.is_none() {
                *fin_opt = Some(now_ms);
            }
        }
        // 同一 stream_id の重複を排除する (複数 object が同時に timeout しうる)
        timed_out.sort_unstable();
        timed_out.dedup();
        for stream_id in timed_out {
            // 複合キー (stream_id, object_id) のため、同一 stream_id の全エントリを削除する
            self.timing
                .object_delivery_header_complete_ms
                .retain(|&(sid, _), _| sid != stream_id);
            self.timing.subgroup_delivery_fin_ms.remove(&stream_id);
            self.events.push_back(SessionEvent::ResetDataStream {
                stream_id,
                error_code: STREAM_DELIVERY_TIMEOUT,
                // Session 自動発火のため RESET_STREAM_AT の自動計算はしない (別途検討)
                reliable_size: None,
            });
        }
        // SUBGROUP_DELIVERY_TIMEOUT の判定
        // draft-ietf-moq-transport-21 §5.2 (Delivery Timeouts and Data Reliability): subgroup stream の FIN 受信から
        // SUBGROUP_DELIVERY_TIMEOUT を超過した場合、DELIVERY_TIMEOUT で stream を reset する (MUST)
        let subgroup_timed_out: Vec<DataStreamId> = self
            .timing
            .subgroup_delivery_fin_ms
            .iter()
            .filter_map(|(&stream_id, &(fin_opt, request_id))| {
                let fin_ms = fin_opt?;
                let subscription = self.subscriptions.get(&request_id)?;
                let timeout_ms = subscription.delivery_timeouts.effective_subgroup_ms?;
                if now_ms.saturating_sub(fin_ms) >= timeout_ms {
                    Some(stream_id)
                } else {
                    None
                }
            })
            .collect();
        for stream_id in subgroup_timed_out {
            self.timing.subgroup_delivery_fin_ms.remove(&stream_id);
            self.events.push_back(SessionEvent::ResetDataStream {
                stream_id,
                error_code: STREAM_DELIVERY_TIMEOUT,
                // Session 自動発火のため RESET_STREAM_AT の自動計算はしない (別途検討)
                reliable_size: None,
            });
        }
    }
}
