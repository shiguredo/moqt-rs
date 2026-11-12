//! REQUEST_OK / REQUEST_ERROR の送受信とタイマー処理
//! draft-ietf-moq-transport-23 §9.3-§9.4, §9.5.1 に対応。将来 draft 側で変更される可能性がある。

use crate::error::{
    PUBLISH_DONE_UPDATE_FAILED, SESSION_INVALID_PARAMETER, SESSION_PROTOCOL_VIOLATION,
    SESSION_ROLE_VIOLATION, SESSION_UNEXPECTED_MESSAGE, STREAM_DELIVERY_TIMEOUT,
};
use crate::message::{
    ControlMessage, ErrorDetails, PUBLISH_OK_ALLOWED_PARAMS, PublishDone,
    REQUEST_UPDATE_OK_ALLOWED_PARAMS, ReasonPhrase, common::Location,
};
use crate::message_parameter::{
    LocationFilterContext, LocationFilterUpdate, MessageParameters, PARAM_LARGEST_OBJECT,
};
use alloc::vec::Vec;

use super::super::core::Session;
use super::super::types::{
    DataStreamId, PendingUpdateGroup, RequestKind, ResetTracking, SessionError, SessionEvent,
    SubscriptionInitiator, SubscriptionState, TrackRole,
};
use super::delivery::{
    effective_largest_object, set_subscription_expires, update_largest_object_in_parameters,
    update_subscription_expires_if_present,
    update_subscription_subscriber_delivery_timeouts_if_present,
};
use super::validation::{apply_range_filter_update, resolve_location_filter, validate_paused};

/// `send_ok_for_subscription` が実際に送った応答
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubscriptionOkResponse {
    /// REQUEST_OK (PUBLISH_OK / REQUEST_UPDATE_OK) を送る
    Ok,
    /// 先頭が拒否待ちグループだったため REQUEST_ERROR を送った
    Rejected,
}

impl Session {
    /// subscription 向け REQUEST_OK の送信時処理
    ///
    /// draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE): 未応答の更新は受信順に保持され、
    /// 応答も受信順に返す必要がある ("REQUEST_UPDATE_OKs MUST be returned in the order the
    /// REQUEST_UPDATEs were received.")。そのため `Established` では呼び出し意図にかかわらず
    /// キューの先頭グループを 1 つだけ適用する。
    ///
    /// 先頭が拒否待ちグループ (`PendingUpdateGroup::is_rejection`) の場合は OK を送らず、
    /// 記録済みの拒否内容で REQUEST_ERROR を送出して [`SubscriptionOkResponse::Rejected`] を
    /// 返す。呼び出し元は REQUEST_OK メッセージを積まないこと。
    pub(crate) fn send_ok_for_subscription(
        &mut self,
        request_id: u64,
        parameters: &mut MessageParameters,
    ) -> Result<SubscriptionOkResponse, SessionError> {
        let now_ms = self.timing.last_tick_ms;
        let subscription = self
            .subscriptions
            .get(&request_id)
            .expect("locate_request guarantees key presence");
        // 判定に使う状態と役割を Copy で退避し、拒否応答の送出 (可変借用) より前に
        // この共有借用を終わらせる。
        let state = subscription.state;
        let pending_publisher = subscription.is_pending_publisher();
        let my_role = subscription.my_role;
        // draft §9.5 (REQUEST_UPDATE): REQUEST_OK (REQUEST_UPDATE_OK) を返せるのは、
        // peer が送った request の responder と、PUBLISH 起点 subscription の publisher
        // (initiator) が peer subscriber の REQUEST_UPDATE に応答する場合である
        // ("A subscriber can also send REQUEST_UPDATE to modify parameters of a
        // subscription established with PUBLISH.")。
        // 後者を許可するのは Established の応答に限る。Pending(Publisher) の PUBLISH_OK は
        // responder である自側 subscriber が返す経路であり、自側 publisher まで開けると
        // 自分が送った PUBLISH を自分の応答で確立扱いにしてしまう。
        // この拒否条件は handle_update_for_subscription の受理条件と対称である。
        // PUBLISH 起点かどうかは購読を開始したメッセージ種別 (`Subscription::initiator`) で
        // 判定する (`my_role` は自側が担う役割であり、単独では起点を表さない)。
        // 外側の `is_initiator_self()` により、この条件が真のとき自側は publisher である。
        let established_publish_origin = state == SubscriptionState::Established
            && subscription.initiator == SubscriptionInitiator::Publisher;
        if subscription.is_initiator_self() && !established_publish_origin {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_ok (subscription) can only be sent by responder-role or by the publisher-role of an established PUBLISH subscription",
            ));
        }
        // draft §9.19.1 (Parameter Scope): Subscription context は state で
        // PUBLISH_OK / REQUEST_UPDATE_OK に分岐する。状態遷移の前にスコープ検証を
        // 行うため、状態遷移前の context を保存する。末尾の LARGEST_OBJECT 注入判定
        // (context が許可する場合のみ注入) にも使い回す。
        // 将来 draft 改版で変更される可能性がある。
        let allowed_params_for_context: Option<(&[u64], &str)> = match state {
            SubscriptionState::Pending if pending_publisher => Some((
                PUBLISH_OK_ALLOWED_PARAMS,
                "REQUEST_OK (publish) parameter not allowed in this context",
            )),
            SubscriptionState::Established => Some((
                REQUEST_UPDATE_OK_ALLOWED_PARAMS,
                "REQUEST_OK (request_update) parameter not allowed in this context",
            )),
            _ => None,
        };
        // draft §9.19.1 (Parameter Scope): スコープ外パラメータを含む REQUEST_OK では
        // 状態を一切変更せずエラーを返す。受信側はスコープ外パラメータで
        // INVALID_PARAMETER により接続を閉じる (MUST) ため、送信側でも
        // 状態遷移・パラメータ適用より前に検証して自側の状態整合性を保つ。
        // 検証が遷移後に走ると、PUBLISH_OK 経路では subscription が Established のまま、
        // REQUEST_UPDATE_OK 経路では pending_update_params が消費されパラメータが
        // 適用済みのまま peer と乖離して孤立する。
        if let Some((allowed, message)) = allowed_params_for_context
            && parameters.validate_scope(allowed).is_err()
        {
            return Err(SessionError::new(SESSION_PROTOCOL_VIOLATION, message));
        }
        // 先頭が拒否待ちグループなら、REQUEST_OK ではなく REQUEST_ERROR を送る。
        // draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE): 応答は受信順に返す必要がある
        // ため、先行グループが残っている間に拒否を即時送出してはならない
        // (`reject_request_update` の doc を参照)。拒否グループはアプリへ通知していないので、
        // アプリからの応答対象にはならない。ここに到達するのは、応答順序を守るために
        // キューへ積まれた拒否が先頭に来た場合だけである。
        // 拒否の送出は REQUEST_ERROR が最終応答になるため、REQUEST_OK 側の状態遷移より前に
        // 済ませてよい (拒否グループの `parameters` は空であり適用する値を持たない)。
        let front_rejection = self
            .subscriptions
            .get(&request_id)
            .filter(|subscription| subscription.state == SubscriptionState::Established)
            .and_then(|subscription| subscription.pending_update_params.front())
            .filter(|group| group.is_rejection())
            .and_then(|group| group.rejection.clone());
        if let Some(rejection) = front_rejection {
            self.subscriptions
                .get_mut(&request_id)
                .expect("locate_request guarantees key presence")
                .pending_update_params
                .pop_front();
            self.send_subscription_update_rejection(
                request_id,
                rejection.error_code,
                rejection.reason,
            )?;
            return Ok(SubscriptionOkResponse::Rejected);
        }
        match state {
            // draft §9.3 (REQUEST_OK): PUBLISH_OK context → REQUEST_OK に統一。
            // Pending(Publisher) → Established 遷移
            SubscriptionState::Pending if pending_publisher => {
                // draft-ietf-moq-transport-23 Appendix A.4 #1790 (Subscription parameters
                // appear in REQUEST_UPDATE, not PUBLISH_OK): PUBLISH_OK は EXPIRES のみを
                // 運び、subscription 更新用パラメータは REQUEST_UPDATE (要求) に出現する。
                // スコープ検証で EXPIRES 以外は既に弾かれているため、ここでは EXPIRES の
                // 適用のみ行う。filter / delivery timeout / priority / Range Filter は
                // 既定値のままであり、paused は PUBLISH 送受信時の値のまま維持する。
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
                // draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE): 未応答の更新のうち
                // 最も古いグループ (合体された累積パラメータ) を subscription state に適用し、
                // キューから取り除く。応答は受信順に返す必要があるため、呼び出し側がどの
                // 更新への応答を意図していても、常に先頭グループが適用対象になる。
                // FILL_PARAMETERS を含む更新は前後の更新と合体しない独立したグループであり、
                // この時点の subscription 状態 (先行グループ適用後) で個別に適用される。
                // 現状の draft では REQUEST_UPDATE に EXPIRES は許可されていないため、
                // グループから EXPIRES が到達することはないが、将来の draft 変更に備えて
                // 適用しておく。
                if let Some(group) = subscription.pending_update_params.pop_front() {
                    let pending_params = group.parameters;
                    update_subscription_subscriber_delivery_timeouts_if_present(
                        subscription,
                        &pending_params,
                    );
                    update_subscription_expires_if_present(subscription, &pending_params, now_ms);
                    if let Some(paused) = pending_params.paused() {
                        let new_paused = validate_paused(paused)?;
                        // draft-ietf-moq-transport-23 §11.3.2 (Closing Subgroup Streams):
                        // STOP_SENDING 後の再オープンは、購読を再開する REQUEST_UPDATE
                        // (PAUSED が 1 から 0 へ変わる) が受理された場合のみ許可する。
                        // 現在値が unpaused であることでは解除しない
                        // (PUBLISH_STATE_NOTIFY などの他経路でも解除しない)
                        if subscription.paused && !new_paused {
                            self.stopped_outgoing_subgroups.remove(&request_id);
                        }
                        subscription.paused = new_paused;
                    }
                    match pending_params.location_filter_update() {
                        // 省略時は値 unchanged、Location Filter Type 0x00 (None) はフィルタ削除
                        Ok(LocationFilterUpdate::Unchanged) => {}
                        Ok(LocationFilterUpdate::Removed) => {
                            subscription.filter = None;
                            subscription.filter_start = None;
                            subscription.filter_end = None;
                        }
                        Ok(LocationFilterUpdate::Set(filter)) => {
                            subscription.filter = Some(filter);
                        }
                        Err(msg_err) => {
                            // draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter):
                            // 未知の Location Filter Type は INVALID_PARAMETER、StartGroup +
                            // EndGroupDelta のオーバーフローは DELTA_ENCODING_OVERFLOW で閉じる
                            // (MUST)。受信メッセージでは decode 層が先に拒否するため到達せず、
                            // API 経由で手組みしたメッセージに対してのみ閉じる
                            let err = crate::session::core::session_error_from_message(
                                &msg_err,
                                SESSION_INVALID_PARAMETER,
                            );
                            self.fail(err.clone());
                            return Err(err);
                        }
                    }
                    if let Some(priority) = pending_params.subscriber_priority() {
                        subscription.subscriber_priority = Some(priority);
                    }
                    // draft-ietf-moq-transport-23 §3.3.2 (Range Filters):
                    // "In REQUEST_UPDATE, Length of 0 removes the filter; non-zero replaces
                    // it entirely. If a filter parameter is omitted from REQUEST_UPDATE, it
                    // is unchanged."
                    //
                    // 未応答の更新は FILL_PARAMETERS の境界でグループに分かれるため、
                    // 先行グループで設定した型がこのグループのパラメータに現れないことが
                    // ある。4 型を丸ごと置き換えると省略された型が消えてしまうため、
                    // パラメータに現れた型だけを差し替える。subscription に累積パラメータを
                    // 別途持つ案は、適用済みの `range_filters` と二重管理になるため採らない。
                    apply_range_filter_update(&mut subscription.range_filters, &pending_params);
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
        // draft-ietf-moq-transport-23 §9.19.17 (LARGEST OBJECT Parameter): LARGEST_OBJECT は actual received 以上でなければならない
        // draft-ietf-moq-transport-23 §9.19.17 (LARGEST OBJECT Parameter) /
        // §3.1.4 (Largest Object): LARGEST_OBJECT は Track 単位の値である。
        // 自側 publisher 役では `publisher_track_largest` が返す Track 最大 (live な
        // subscription 群の最大値と、Track 単位で保持した自側 publish 済み最大の大きい方) を
        // 使い、自側 subscriber 役では従来どおり当該 subscription の
        // effective_largest_object を使う
        // (subscriber 役は peer から受信した値を保持しており、購読横断の集約は対象外)。
        // self の可変借用より前に求める (不変借用が必要なため)。
        let track_largest = self
            .subscriptions
            .get(&request_id)
            .and_then(|subscription| {
                if subscription.my_role == TrackRole::Publisher {
                    self.publisher_track_largest(
                        &subscription.track_namespace,
                        &subscription.track_name,
                    )
                } else {
                    effective_largest_object(subscription)
                }
            });
        if let Some(subscription) = self.subscriptions.get_mut(&request_id) {
            // LARGEST_OBJECT を許可する context (REQUEST_UPDATE_OK / TRACK_STATUS_OK 経路) でのみ
            // max 合流を行う。PUBLISH_OK context では §9.19.17 (LARGEST OBJECT Parameter) が
            // PUBLISH_OK を列挙していないため注入しない。
            let context_allows_largest_object = allowed_params_for_context
                .is_some_and(|(allowed, _)| allowed.contains(&PARAM_LARGEST_OBJECT));
            if context_allows_largest_object && let Some(ref effective) = track_largest {
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

        Ok(SubscriptionOkResponse::Ok)
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
        //
        // どちらの遷移でも request は終端するため、残っている適用待ちの
        // REQUEST_UPDATE と MAX_REQUEST_UPDATES のクレジット会計を解放する
        // (`release_request_update_state` の doc を参照)。
        let result = match subscription.state {
            SubscriptionState::Pending => {
                subscription.state = SubscriptionState::Terminated;
                // draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams):
                // subscription の終了時は open 中の fill fetch stream を reset する (MUST)。
                // SUBSCRIBE 処理直後に開いた fill stream が残っている場合が対象。
                self.reset_open_fill_streams(request_id);
                // draft-ietf-moq-transport-23 §9.8 (PUBLISH): 不要な PUBLISH を拒否したとき、
                // 当該 subscription に属する open な受信 stream の読み取りを STOP_SENDING で
                // abandon する。PUBLISH 受理時点で Track Alias を登録するため、応答前に
                // publisher 起点 stream が開いていることがある。
                self.discard_rejected_subscription_incoming_streams(request_id);
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
                // 失敗応答では合体した複数の更新に単一の REQUEST_ERROR を返すため、
                // 未応答のグループをすべて破棄する (以後の REQUEST_UPDATE_OK は適用されない)
                subscription.pending_update_params.clear();
                subscription.state = SubscriptionState::Terminated;
                // draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams):
                // subscription の終了時は open 中の fill fetch stream を reset する (MUST)。
                // PUBLISH_DONE push は send_request_error 側で行う (§9.9 の MUST NOT に
                // 従い、open 中の stream がある場合は保留される。fill stream も
                // `has_open_outgoing_data_streams_for_request` の対象に含まれる)。
                self.reset_open_fill_streams(request_id);
                // PUBLISH_DONE push は send_request_error 側で行う (§9.9 の MUST NOT に
                // 従い、open 中の outgoing data stream (subgroup / fill fetch) がある場合は
                // 保留される。ここで fill fetch stream を reset 済みのため、実際に残るのは
                // subgroup stream だけである)
                Ok(Some(stream_count))
            }
            SubscriptionState::Terminated => Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "send_request_error requires non-terminated state (subscription)",
            )),
        };
        // Terminated への遷移 (Ok) のときだけ、キューとクレジットを解放する。
        // 既に Terminated の request は状態を変えないため何もしない。
        if result.is_ok() {
            self.release_request_update_state(request_id);
        }
        result
    }

    /// subscription の REQUEST_UPDATE を REQUEST_ERROR で拒否する応答を送る
    ///
    /// 自側が publisher の場合は `send_request_error` と同じ経路で
    /// REQUEST_ERROR → Terminated → PUBLISH_DONE(UPDATE_FAILED) まで進める
    /// (draft-ietf-moq-transport-23 §9.5.1 (Updating Subscriptions))。
    /// 自側が subscriber の場合は REQUEST_ERROR のみを送り、PUBLISH_DONE の送出は
    /// publisher である peer の責務に委ねる。どちらの場合も REQUEST_ERROR が最終応答
    /// (FIN) になるため、以後の REQUEST_UPDATE_OK は送れない。残っている未応答グループと
    /// MAX_REQUEST_UPDATES (§9.1.7) のクレジットを解放する。
    pub(crate) fn send_subscription_update_rejection(
        &mut self,
        request_id: u64,
        error_code: u64,
        reason: &'static str,
    ) -> Result<(), SessionError> {
        let my_role = self
            .subscriptions
            .get(&request_id)
            .expect("locate_request guarantees key presence")
            .my_role;
        match my_role {
            TrackRole::Publisher => self.send_request_error(
                request_id,
                error_code,
                0,
                ReasonPhrase::new(reason)
                    .expect("request update rejection reason must be <= 1024 bytes"),
                ErrorDetails::new(),
            ),
            TrackRole::Subscriber => {
                self.emit_request_error(request_id, error_code, reason);
                self.release_request_update_state(request_id);
                Ok(())
            }
        }
    }

    /// 先頭に詰まった拒否待ちグループの REQUEST_ERROR を送出する
    ///
    /// 受理済みグループの REQUEST_UPDATE_OK を送った直後に呼ぶ。拒否は受信時に
    /// キューの末尾へ積まれているため、先頭に来た時点で受信順の応答が可能になる
    /// (draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE))。REQUEST_ERROR の送出で
    /// 購読が終端するか送信方向が FIN するため、続けて応答を返すことはない。
    pub(crate) fn flush_deferred_update_rejections(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let rejection = {
            let Some(subscription) = self.subscriptions.get_mut(&request_id) else {
                return Ok(());
            };
            if !subscription
                .pending_update_params
                .front()
                .is_some_and(PendingUpdateGroup::is_rejection)
            {
                return Ok(());
            }
            subscription
                .pending_update_params
                .pop_front()
                .and_then(|group| group.rejection)
                .expect("front group was checked as rejection")
        };
        self.send_subscription_update_rejection(request_id, rejection.error_code, rejection.reason)
    }

    pub(crate) fn handle_ok_for_subscription(
        &mut self,
        request_id: u64,
        parameters: &MessageParameters,
    ) -> Result<(), SessionError> {
        let now_ms = self.timing.last_tick_ms;
        // 借用の都合で判定を共有借用より前に済ませる
        let locally_terminated = self.is_locally_terminated_request(request_id);
        let subscription = self
            .subscriptions
            .get(&request_id)
            .expect("locate_request guarantees key presence");
        // draft §9.5 (REQUEST_UPDATE): REQUEST_UPDATE を送れるのは request の initiator と、
        // PUBLISH 起点 subscription の subscriber である。REQUEST_OK を受理できるのは
        // それを送れる側だからで、受理条件は send_update_for_subscription の送信条件と
        // 対称にする。outstanding な REQUEST_UPDATE との対応付けまでは検証しない
        // (subscription の REQUEST_OK は従来から対応付けを持たない)。
        // 受理は Established に限る。Pending(Publisher) の REQUEST_OK は PUBLISH_OK として
        // initiator である自側 publisher だけが受け、responder 側が受けると応答を送って
        // いない購読を確立扱いにしてしまう。
        // PUBLISH 起点かどうかは購読を開始したメッセージ種別 (`Subscription::initiator`) で
        // 判定する (`my_role` は自側が担う役割であり、単独では起点を表さない)。
        // `!is_initiator_self()` により、この条件が真のとき自側は subscriber である。
        let established_publish_origin = subscription.state == SubscriptionState::Established
            && subscription.initiator == SubscriptionInitiator::Publisher;
        // ローカル終端済みの request では終端前の state が失われるため
        // `established_publish_origin` を判定できない。自側が subscriber の場合は
        // PUBLISH 起点 / SUBSCRIBE 起点のどちらでも自側から REQUEST_UPDATE を送れるため
        // (`send_update_for_subscription` の送信条件)、遅延した REQUEST_UPDATE_OK は
        // 正当な応答になりうる。自側が publisher の場合はガードを維持する。
        if !(subscription.is_initiator_self()
            || established_publish_origin
            || (locally_terminated && subscription.my_role == TrackRole::Subscriber))
        {
            let err = SessionError::new(
                SESSION_ROLE_VIOLATION,
                "REQUEST_OK (subscription) received on responder side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft §9.19.1 (Parameter Scope): Subscription context の REQUEST_OK は
        // subscription.state で PUBLISH_OK (Pending(Publisher)) と REQUEST_UPDATE_OK
        // (Established) に確定する。それぞれの許可パラメータ集合で検証し、許可外
        // パラメータは INVALID_PARAMETER でセッションをクローズする。Established 分岐は
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
            // ローカル終端済みでは PUBLISH_OK / REQUEST_UPDATE_OK の区別が state から
            // 失われるため、下の両 context 検証で判定する
            _ => None,
        };
        if let Some((allowed, message)) = context_allowed
            && parameters.validate_scope(allowed).is_err()
        {
            // draft-ietf-moq-transport-23 §9.19.1 (Parameter Scope): INVALID_PARAMETER (0x305)。
            let err = SessionError::new(SESSION_INVALID_PARAMETER, message);
            self.fail(err.clone());
            return Err(err);
        }
        // draft §9.19.1 (Parameter Scope): ローカル終端済みの request は context を特定できない
        // ため、どちらの context の許可集合にも含まれないパラメータだけを違反とする
        // (和集合の定数に依存させず、両集合の変更に追随させる)。自側 publisher で initiator の
        // 場合は PUBLISH_OK context でも LARGEST_OBJECT を許容する残差がある
        // (終端で context を復元できないため)。
        if context_allowed.is_none()
            && locally_terminated
            && ![PUBLISH_OK_ALLOWED_PARAMS, REQUEST_UPDATE_OK_ALLOWED_PARAMS]
                .iter()
                .any(|allowed| parameters.validate_scope(allowed).is_ok())
        {
            let err = SessionError::new(
                SESSION_INVALID_PARAMETER,
                "REQUEST_OK parameter not allowed in any context",
            );
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
                // draft-ietf-moq-transport-23 Appendix A.4 #1790 (Subscription parameters
                // appear in REQUEST_UPDATE, not PUBLISH_OK): PUBLISH_OK は EXPIRES のみを
                // 運ぶ。スコープ検証で EXPIRES 以外は既に弾かれているため、ここでは
                // EXPIRES の適用と Established 遷移のみ行う。filter / PAUSED 等の
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
                // (draft-ietf-moq-transport-23 Appendix A.4 #1872)。
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
                    // draft §9.5 (REQUEST_UPDATE): REQUEST_UPDATE_OK は SUBSCRIBE 起点でも
                    // PUBLISH 起点でも発火するため、自側が担う役割 (`my_role`) ではなく
                    // 購読を開始したメッセージ種別 (`Subscription::initiator`) で判定する。
                    request_kind: match subscription.initiator {
                        SubscriptionInitiator::Subscriber => RequestKind::Subscribe,
                        SubscriptionInitiator::Publisher => RequestKind::Publish,
                    },
                    parameters: parameters.clone(),
                });
            }
            _ => {
                // ローカル終端済みの request はここまでの検証 (送信者・パラメータスコープ)
                // だけを行い、状態遷移とイベント発行はしない (peer が reset を観測する前に
                // 送った正当な応答が遅延して届く)。context 別スコープの検証は上の両 context
                // 検証で済んでいる。
                if locally_terminated {
                    return Ok(());
                }
                let err = SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
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
    /// draft-ietf-moq-transport-23 §3.1.2 (Subscription State Management):
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
        // ローカル終端済みの request は検証 (呼び出し元の REDIRECT Detail の検証) だけを行い、
        // 状態遷移はしない (peer が reset を観測する前に送った正当な応答が遅延して届く)。
        // `RequestErrorReceived` は呼び出し元 `handle_peer_request_error` が通知する。
        //
        // STOP_SENDING などの cancel で `Terminated` かつ `publish_done` 未設定になった購読も
        // 同じ扱いにする (`is_cancelled_terminated_subscription`)。cancel は
        // `rejected_request_ids` に記録しないため `is_locally_terminated_request` だけでは
        // 拾えず、遅延 REQUEST_ERROR が UNEXPECTED_MESSAGE でセッションを閉じていた。
        if self.is_locally_terminated_request(request_id)
            || self.is_cancelled_terminated_subscription(request_id)
        {
            return Ok(());
        }
        // 自側 publisher の PUBLISH 起点 subscription で REQUEST_UPDATE 失敗応答
        // (REQUEST_ERROR) を受信した場合に送る PUBLISH_DONE (UPDATE_FAILED) の
        // stream count。`None` は送らない。
        let mut publish_done_stream_count = None;
        {
            let subscription = self
                .subscriptions
                .get_mut(&request_id)
                .expect("locate_request guarantees key presence");
            match subscription.state {
                SubscriptionState::Pending => {
                    subscription.state = SubscriptionState::Terminated;
                    // draft-ietf-moq-transport-23 §3.1.2: REQUEST_ERROR → 即時破棄 (drain timer 不要)
                    subscription.publish_done = None;
                    // 自側が失敗応答を送る `send_err_for_subscription` と対称に、open 中の
                    // fill fetch stream を reset する (§3.4.1 の MUST)
                    self.reset_open_fill_streams(request_id);
                }
                SubscriptionState::Established => {
                    // draft §3.1.2: REQUEST_ERROR の受信で subscription state を終える。
                    // REQUEST_UPDATE 失敗応答も同じく Terminated に遷移させ、再 REQUEST_UPDATE の窓を閉じる。
                    subscription.state = SubscriptionState::Terminated;
                    // draft-ietf-moq-transport-23 §3.1.2: REQUEST_ERROR → 即時破棄 (drain timer 不要)
                    subscription.publish_done = None;
                    // draft-ietf-moq-transport-23 §9.5.1 (Updating Subscriptions):
                    // "When a REQUEST_UPDATE is unsuccessful, the publisher MUST also
                    //  terminate the subscription by sending a PUBLISH_DONE with error
                    //  code UPDATE_FAILED."
                    // 自側が PUBLISH の initiator (publisher) の場合、Established 後に
                    // 受信する REQUEST_ERROR は自側 REQUEST_UPDATE の失敗応答である。
                    // REQUEST_UPDATE を送れるのは initiator 自身または自側 Subscriber であり
                    // (send_update_for_subscription の role 検証)、PUBLISH_DONE を送れるのは
                    // publisher だけなので、両方を満たすこの組合せだけが対象になる。
                    if subscription.my_role == TrackRole::Publisher
                        && subscription.is_initiator_self()
                    {
                        publish_done_stream_count =
                            Some(subscription.stream_counts.published_count);
                    }
                    // draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams):
                    // subscription の終了時は open 中の fill fetch stream を reset する (MUST)。
                    // 自側が失敗応答を送る `send_err_for_subscription` と対称にする。片方だけ
                    // reset しないと、§9.9 (PUBLISH_DONE) の MUST NOT で保留した PUBLISH_DONE が
                    // fill fetch stream の終端通知では flush されず (§9.9 により fill の終端だけでは
                    // 送れない)、§9.5.1 の MUST を果たせないまま残る。
                    // ここで reset すれば、保留中に open であり続ける stream は subgroup stream だけになり、
                    // その終端 (`Session::send_data_stream_closed`) が flush 契機になる。
                    self.reset_open_fill_streams(request_id);
                }
                SubscriptionState::Terminated => {
                    let err = SessionError::new(
                        SESSION_UNEXPECTED_MESSAGE,
                        "REQUEST_ERROR on terminated subscription",
                    );
                    self.fail(err.clone());
                    return Err(err);
                }
            }
        }
        // 失敗応答の受信で subscription が終端したため、未応答の REQUEST_UPDATE と
        // MAX_REQUEST_UPDATES のクレジットを解放する。以後の応答は返らない
        // (draft-ietf-moq-transport-23 §9.5.1 (Updating Subscriptions))。
        self.release_request_update_state(request_id);
        // draft-ietf-moq-transport-23 §9.9 (PUBLISH_DONE): "A sender MUST NOT send
        // PUBLISH_DONE until it has closed all streams it will ever open, and has no
        // further datagrams to send, for a subscription." の MUST NOT と §9.5.1 の MUST を
        // 両立するため、open 中の outgoing data stream がある間は push を保留し
        // (`Subscription::pending_publish_done`)、全 stream 終端後に自動送信する。
        // 失敗応答を返す responder 側 (`send_err_for_subscription`) と同じ扱いである。
        if let Some(stream_count) = publish_done_stream_count {
            if self.has_open_outgoing_data_streams_for_request(request_id) {
                self.subscriptions
                    .get_mut(&request_id)
                    .expect("handle_err_for_subscription guarantees key presence")
                    .pending_publish_done = Some(stream_count);
            } else {
                // PUBLISH_DONE は subscription の最終メッセージ (FIN) のため、request stream
                // GOAWAY の reset deadline は不要になる
                self.clear_request_stream_goaway_deadline(request_id);
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
                // 保留側 (`Session::maybe_flush_pending_publish_done`) と揃えて、送信方向が
                // 閉じた事実を記録する。
                self.mark_send_direction_closed_with_fin(request_id);
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
        // draft-ietf-moq-transport-23 §5.2 (Delivery Timeouts and Data Reliability): object header の
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
                tracking: ResetTracking::Retained,
            });
        }
        // SUBGROUP_DELIVERY_TIMEOUT の判定
        // draft-ietf-moq-transport-23 §5.2 (Delivery Timeouts and Data Reliability): subgroup stream の FIN 受信から
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
                tracking: ResetTracking::Retained,
            });
        }
    }
}
