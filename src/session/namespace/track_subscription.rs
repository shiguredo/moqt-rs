//! SUBSCRIBE_TRACKS / PUBLISH_SKIPPED 関連
//!
//! draft-ietf-moq-transport-21 §9.18 (SUBSCRIBE_TRACKS) / §9.19 (PUBLISH_SKIPPED) に対応する
//! `impl Session` の送受信メソッドをまとめる。

use crate::error::{
    REQUEST_DOES_NOT_EXIST, REQUEST_INVALID_FILTER, REQUEST_PREFIX_OVERLAP,
    SESSION_PROTOCOL_VIOLATION,
};
use crate::message::{ControlMessage, PublishSkipped, SubscribeTracks, common::TrackNamespace};
use crate::message_parameter::{MessageParameters, PARAM_TRACK_PROPERTY_FILTER};
use alloc::vec::Vec;

use super::super::core::Session;
use super::super::types::{
    RequestKind, RequestStreamEnd, SendRequestError, SessionError, SessionEvent, TerminationReason,
    TrackRole, TrackSubscription, TrackSubscriptionState,
};
use super::{
    effective_prefix, pop_pending_prefix_update, prefix_overlaps, push_pending_prefix_update,
    terminationreason_from_end,
};

impl Session {
    // ─── クエリ API ─────────────────────────────────────────

    /// SUBSCRIBE_TRACKS 参照
    pub fn track_subscription(&self, request_id: u64) -> Option<&TrackSubscription> {
        self.track_subscriptions.get(&request_id)
    }

    /// 全 SUBSCRIBE_TRACKS エントリの反復子を返す
    pub fn track_subscriptions(&self) -> impl Iterator<Item = &TrackSubscription> {
        self.track_subscriptions.values()
    }

    /// Pending 以外の SUBSCRIBE_TRACKS を除去
    pub fn forget_track_subscription(&mut self, request_id: u64) -> Option<TrackSubscription> {
        let entry = self.track_subscriptions.get(&request_id)?;
        if entry.state == TrackSubscriptionState::Pending {
            return None;
        }
        self.request_streams.remove(&request_id);
        self.remove_request_update_credit_entries(request_id);
        self.pending_prefix_updates.remove(&request_id);
        self.track_subscriptions.remove(&request_id)
    }

    // ─── 送信 API ──────────────────────────────────────────

    /// SUBSCRIBE_TRACKS を送信する (draft §9.18 (SUBSCRIBE_TRACKS)、自側が subscriber)
    pub fn send_subscribe_tracks(
        &mut self,
        prefix: TrackNamespace,
        parameters: MessageParameters,
    ) -> Result<u64, SendRequestError> {
        self.require_established()?;
        self.check_peer_goaway()?;
        // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces): single period `.` 予約名前空間への SUBSCRIBE_TRACKS は送信不可
        if prefix.is_single_period() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "application cannot use single-period reserved namespace",
            )
            .into());
        }
        // draft §9.18 (SUBSCRIBE_TRACKS): SUBSCRIBE_TRACKS テーブル内での prefix overlap チェック。
        // 確定待ちの REQUEST_UPDATE がある購読とは反映後の実効 prefix で比較し、
        // Terminated の購読は active ではないため対象外とする
        // (draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions))。
        for (existing_id, existing) in &self.track_subscriptions {
            if existing.my_role != TrackRole::Subscriber
                || existing.state == TrackSubscriptionState::Terminated
            {
                continue;
            }
            let existing_effective =
                effective_prefix(&self.pending_prefix_updates, *existing_id, &existing.prefix);
            if prefix_overlaps(&prefix, &existing_effective) {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "local subscribe_tracks prefix overlaps existing subscription",
                )
                .into());
            }
        }
        // draft-ietf-moq-transport-21 §3.3.2 (Range Filters): peer が必ず INVALID_FILTER で
        // 拒否するメッセージを無警告で送出しないよう、送信前に自側で弾く
        self.validate_outgoing_range_filters(&parameters)?;
        // draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): 値域は
        // Ascending (0x1) / Descending (0x2) のみ。値域外は API 呼び出し時にエラーを返す
        // (検証がないと、TrackSubscription の登録・ control deadline の開始・ SendRequest の
        // push まで完了した後に I/O 層のエンコード時 (validate_param_encoding) で非同期に
        // 失敗し、アプリがエンコードエラーを無視すると control deadline タイムアウトという
        // 後続の誤作動につながる)。
        // 検証は request_id 発行より前に置き、エラー時に欠番を作らない (採番の稠密性を
        // 維持してデバッグ・トレースの見通しを保つ。欠番自体はプロトコル上合法)。
        // この「発行前検証で欠番を作らない」設計は本 API のみの選択であり、既存の
        // `send_subscribe` は request_id 発行後に検証する
        // (エラー時に欠番が発生する)。`send_fetch` は scope ・空 range のみ発行前に
        // 検証し、値域検証は発行後のままである。
        // なお本検証は `parameters.group_order()` (Uint8 値のみを対象とし、最初の 1 件のみ
        // 返す検索) に依存するため、型不一致 (例: VarInt) の GROUP_ORDER と複数出現の
        // 2 番目以降はすり抜け、encode 時に失敗する (型検証は validate_param_encoding、
        // 重複検出は MessageParameters::encode 本体。残存ギャップ)。
        // 将来 draft 改版で変更される可能性がある。
        if let Some(order) = parameters.group_order() {
            super::super::subscription::validation::validate_group_order(order)?;
        }
        // draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter): 値域は 0 (送らない) /
        // 1 (送る) のみ。値域外は API 呼び出し時にエラーを返す (検証がないと、
        // TrackSubscription の登録・ control deadline の開始・ SendRequest の push まで
        // 完了した後に I/O 層のエンコード時 (validate_param_encoding) で非同期に失敗し、
        // アプリがエンコードエラーを無視すると control deadline タイムアウトという後続の
        // 誤作動につながる)。
        // 検証は request_id 発行より前に置き、エラー時に欠番を作らない (GROUP_ORDER 検証と
        // 同じ設計)。検証は行うが、subscriber 役 TrackSubscription の forward_state は
        // 1 固定のまま変更しない (どの役割でも forward_state は未参照)。
        // なお本検証は `parameters.forward()` (Uint8 値のみを対象とし、最初の 1 件のみ
        // 返す検索) に依存するため、型不一致 (例: VarInt) の FORWARD と複数出現の 2 番目
        // 以降はすり抜け、encode 時に失敗する (GROUP_ORDER 検証と同様の残存ギャップ)。
        if let Some(f) = parameters.forward() {
            super::super::subscription::validation::validate_forward(f)?;
        }
        // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
        // 送信前に値域を検証し、不正値の送出と状態登録を防ぐ
        // (GROUP_ORDER / FORWARD 検証と同じ設計で、request_id 発行より前に置く)。
        if let Some(v) = parameters.include_properties() {
            super::super::subscription::validation::validate_include_properties(v)?;
        }
        // draft §9.20.1 (Parameter Scope): SUBSCRIBE_TRACKS で許可されないパラメータを含む
        // 送信は API 呼び出し時に拒否する。検証がないと、TrackSubscription の登録・
        // control deadline の開始・ SendRequest の push まで完了した後に I/O 層のエンコード
        // 時 (validate_scope) で非同期に失敗し、アプリがエンコードエラーを無視すると
        // control deadline タイムアウトという後続の誤作動につながる (受信側はワイヤ層で
        // 検証済みのため、ここは送信側の状態整合性のための検証)。
        // 検証は request_id 発行より前に置き、エラー時に欠番を作らない (GROUP_ORDER /
        // FORWARD 検証と同じ設計)。なお本検証はパラメータのスコープのみを対象とし、
        // スコープ内の同一型の重複出現 (例: FORWARD 2 件) は検出されず encode 層の重複
        // 検出で失敗する残存ギャップがある (SUBSCRIBE_OK 送信側の parameter scope 検証と
        // 同じ制約)。
        if parameters
            .validate_scope(crate::message::SUBSCRIBE_TRACKS_ALLOWED_PARAMS)
            .is_err()
        {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "SUBSCRIBE_TRACKS parameter not allowed in this context",
            )
            .into());
        }
        let request_id = self.request_ids.local_generator.next_id();
        self.track_subscriptions.insert(
            request_id,
            TrackSubscription {
                request_id,
                my_role: TrackRole::Subscriber,
                prefix: prefix.clone(),
                state: TrackSubscriptionState::Pending,
                forward_state: 1,
                active_track_aliases: hashbrown::HashSet::new(),
                skipped_tracks: hashbrown::HashSet::new(),
                // draft §3.3.2 (Range Filters): 自側は subscriber なので選別には使わないが、
                // peer 側と対称に状態として持つ
                track_property_filters: parameters.range_filter_sets(PARAM_TRACK_PROPERTY_FILTER),
                // 自側は subscriber のため resulting PUBLISH を送らず、INCLUDE_PROPERTIES の保持は不要
                include_properties: None,
            },
        );
        self.request_streams
            .insert(request_id, RequestKind::SubscribeTracks);
        self.start_control_message_deadline(request_id);
        let msg = ControlMessage::SubscribeTracks(SubscribeTracks {
            request_id,
            track_namespace_prefix: prefix,
            parameters,
        });
        self.events.push_back(SessionEvent::SendRequest {
            request_id,
            message: msg,
        });
        Ok(request_id)
    }

    /// PUBLISH_SKIPPED を送信する (draft-ietf-moq-transport-21 §9.19 (PUBLISH_SKIPPED): SUBSCRIBE_TRACKS 専用)
    pub fn send_publish_skipped(
        &mut self,
        request_id: u64,
        suffix: TrackNamespace,
        track_name: Vec<u8>,
    ) -> Result<(), SessionError> {
        self.require_track_subscription_publisher(request_id)?;
        // draft-ietf-moq-transport-21 §9.19 (PUBLISH_SKIPPED): 送信済みの Track を記録し、
        // 後続の同一 Track PUBLISH を禁止する (MUST NOT)
        if let Some(ts) = self.track_subscriptions.get_mut(&request_id) {
            ts.skipped_tracks
                .insert((suffix.clone(), track_name.clone()));
        }
        let msg = ControlMessage::PublishSkipped(PublishSkipped {
            track_namespace_suffix: suffix,
            track_name,
        });
        self.events.push_back(SessionEvent::SendOnStream {
            request_id,
            message: msg,
            fin: false,
        });
        Ok(())
    }

    // ─── 受信ハンドラ ──────────────────────────────────────

    pub(crate) fn handle_peer_subscribe_tracks(
        &mut self,
        msg: SubscribeTracks,
    ) -> Result<(), SessionError> {
        let request_id = msg.request_id;
        if !self.accept_peer_request(request_id, &msg.parameters)? {
            return Ok(());
        }
        // draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter): REGISTER token は
        // session error でない限り MUST で保存する。名前空間チェック (REQUEST_ERROR) より前に処理する。
        self.apply_peer_message_auth_tokens(&msg.parameters)?;
        // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces): single period `.` 予約名前空間への SUBSCRIBE_TRACKS は拒否
        if msg.track_namespace_prefix.is_single_period() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "reserved single-period namespace",
            );
            return Ok(());
        }
        // draft-ietf-moq-transport-21 §6.5 (Session-Level Tracks and Namespaces): .session 予約名前空間への SUBSCRIBE_TRACKS は拒否
        if msg.track_namespace_prefix.is_session_level() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "session-level track does not exist",
            );
            return Ok(());
        }
        // draft-ietf-moq-transport-21 §3.3.2 (Range Filters): MAX_FILTER_RANGES 超過は INVALID_FILTER で拒否
        if let Err(reason) = self.check_incoming_range_filters(&msg.parameters) {
            self.emit_request_error(request_id, REQUEST_INVALID_FILTER, reason);
            return Ok(());
        }
        // draft §9.18 (SUBSCRIBE_TRACKS): SUBSCRIBE_TRACKS テーブル内での prefix overlap チェック
        for existing in self.track_subscriptions.values() {
            if existing.my_role == TrackRole::Publisher
                && existing.state == TrackSubscriptionState::Established
                && prefix_overlaps(&msg.track_namespace_prefix, &existing.prefix)
            {
                self.emit_request_error(
                    request_id,
                    REQUEST_PREFIX_OVERLAP,
                    "subscribe_tracks prefix overlaps",
                );
                return Ok(());
            }
        }
        // draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): SUBSCRIBE_TRACKS は
        // GROUP_ORDER の出現を許可する (MAY)。値域外の受信はセッションを閉じる。
        // 検証のみ行い、値は保持しない (TrackSubscription に group_order フィールドは無い)。
        if let Some(order) = msg.parameters.group_order()
            && let Err(err) = super::super::subscription::validation::validate_group_order(order)
        {
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter): FORWARD パラメータの抽出 (不在時はデフォルト 1)
        // 値域外の受信は MUST でセッションを PROTOCOL_VIOLATION により閉じる
        // (GROUP_ORDER の値域検証と同じパターン。`?` 伝播のみだとセッションが開いたまま残る)。
        // 検証の `Ok` 値を `forward_state` の設定に使い、`forward()` の検索結果を検証に再利用する
        // (検証を呼び出しと分離し、同じパラメータを 2 回解釈しない)。
        let forward_state = match msg.parameters.forward() {
            Some(f) => match super::super::subscription::validation::validate_forward(f) {
                Ok(validated) => validated,
                Err(err) => {
                    self.fail(err.clone());
                    return Err(err);
                }
            },
            None => 1,
        };
        // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
        // 値域外 (0 / 1 以外) の受信は MUST でセッションを PROTOCOL_VIOLATION により閉じる。
        let include_properties = msg.parameters.include_properties();
        if let Some(v) = include_properties
            && let Err(err) = super::super::subscription::validation::validate_include_properties(v)
        {
            self.fail(err.clone());
            return Err(err);
        }
        self.track_subscriptions.insert(
            request_id,
            TrackSubscription {
                request_id,
                my_role: TrackRole::Publisher,
                prefix: msg.track_namespace_prefix.clone(),
                state: TrackSubscriptionState::Pending,
                forward_state,
                active_track_aliases: hashbrown::HashSet::new(),
                skipped_tracks: hashbrown::HashSet::new(),
                // draft §3.3.2 (Range Filters): peer subscriber が指定した TRACK_PROPERTY_FILTER を
                // 保持し、自側 PUBLISH の選別に使う
                track_property_filters: msg
                    .parameters
                    .range_filter_sets(PARAM_TRACK_PROPERTY_FILTER),
                // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
                // peer が SUBSCRIBE_TRACKS で指定した値を保持する。resulting PUBLISH の
                // Track Properties 空化は application 層が `track_subscription()` で参照して行う
                include_properties,
            },
        );
        self.request_streams
            .insert(request_id, RequestKind::SubscribeTracks);
        self.events
            .push_back(SessionEvent::SubscribeTracksReceived {
                request_id,
                prefix: msg.track_namespace_prefix,
                parameters: msg.parameters,
            });
        Ok(())
    }

    pub(crate) fn handle_peer_publish_skipped(
        &mut self,
        request_id: u64,
        msg: PublishSkipped,
    ) -> Result<(), SessionError> {
        // draft-ietf-moq-transport-21 §9.19 (PUBLISH_SKIPPED): PUBLISH_SKIPPED は SUBSCRIBE_TRACKS 専用
        if let Some(&kind) = self.request_streams.get(&request_id)
            && kind == RequestKind::SubscribeNamespace
        {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH_SKIPPED received on SUBSCRIBE_NAMESPACE stream",
            );
            self.fail(err.clone());
            return Err(err);
        }
        let Some(entry) = self.track_subscriptions.get(&request_id) else {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH_SKIPPED received for unknown request id",
            );
            self.fail(err.clone());
            return Err(err);
        };
        if entry.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH_SKIPPED received on publisher side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        if entry.state != TrackSubscriptionState::Established {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH_SKIPPED received before REQUEST_OK on SUBSCRIBE_TRACKS stream",
            );
            self.fail(err.clone());
            return Err(err);
        }
        self.events.push_back(SessionEvent::PublishSkippedReceived {
            request_id,
            suffix: msg.track_namespace_suffix,
            track_name: msg.track_name,
        });
        Ok(())
    }

    /// bidi request stream 終端時の SUBSCRIBE_TRACKS 側の処理
    ///
    /// draft-ietf-moq-transport-21 §4.1 (Subscribing to Namespaces) / §6.4.2.3 (Request
    /// Cancellation and Rejection): SUBSCRIBE_TRACKS は §6.4.2.3 に従って cancel され、
    /// cancel しても original publisher の以後の PUBLISH 送信を禁じない。SUBSCRIBE_TRACKS の
    /// 終端で `active_track_aliases` に残存する subscription を暗黙終端するのは本実装の扱いである。
    pub(crate) fn close_track_subscription_on_stream_end(
        &mut self,
        request_id: u64,
        end: RequestStreamEnd,
    ) -> Result<TerminationReason, SessionError> {
        // active_track_aliases に残っている全 track alias に対応する Subscription を終端。
        // ループ内で `&mut self` を取るメソッドを呼ぶため、alias を所有値で取り出して
        // `track_subscriptions` の借用をここで閉じる。
        let aliases: Vec<u64> = {
            let Some(entry) = self.track_subscriptions.get_mut(&request_id) else {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "bidi request stream close for unknown SUBSCRIBE_TRACKS request id",
                ));
            };
            entry.active_track_aliases.drain().collect()
        };
        // bidi stream 終端で確定待ちは破棄する (REQUEST_OK は届かない)
        self.pending_prefix_updates.remove(&request_id);
        // SUBSCRIBE_TRACKS の終端種別 (FIN / RESET) を派生 subscription の終端通知にも使う
        let reason = terminationreason_from_end(end);
        for &alias in &aliases {
            // draft §3.1 (Subscriptions): alias は同一 Track の複数 subscription で共有されうる
            // ため、この alias に紐づく全 subscription を終端する
            let sub_request_ids: Vec<u64> = self
                .aliases
                .peer_publisher_aliases
                .get(&alias)
                .cloned()
                .unwrap_or_default();
            for sub_request_id in sub_request_ids {
                if let Some(sub) = self.subscriptions.get_mut(&sub_request_id) {
                    use super::super::types::SubscriptionState;
                    if sub.state != SubscriptionState::Terminated {
                        sub.state = SubscriptionState::Terminated;
                    }
                }
                let key = self
                    .subscriptions
                    .get(&sub_request_id)
                    .map(|s| (s.track_namespace.clone(), s.track_name.clone(), s.my_role));
                if let Some(key) = key {
                    self.remove_subscription_track_index(sub_request_id, &key);
                }
                // alias holder が空になったときだけ SubgroupTracker の alias 単位エントリを
                // 除去する (共有 alias の他 subscription が残っている間は除去しない)
                if self.release_peer_alias(alias, sub_request_id) {
                    self.peer_subgroups.remove_track_alias(alias);
                }
                // draft-ietf-moq-transport-21 §6.4.2.2 (Graceful Request Stream Closure):
                // peer は後から PUBLISH の bidi request stream を FIN / RESET で閉じる。
                // close 未受信 (request_streams に登録あり) のときだけ rejected_request_ids に
                // 登録し、後続のクローズを no-op で吸収する (`forget_fetch` と同じ前例)。
                // 既にクローズ済み (`close_subscription_on_stream_end` が除去済み) のときは
                // 再登録も RequestTerminated の再発行もしない (二重イベントと close 済み id の
                // 永久残留を避ける)。
                // kind は request_streams の登録値を使う。共有 alias では SUBSCRIBE_OK 由来の
                // subscription が混在しうるため `RequestKind::Publish` 固定にはしない。
                if let Some(kind) = self.request_streams.remove(&sub_request_id) {
                    self.rejected_request_ids.insert(sub_request_id);
                    self.events.push_back(SessionEvent::RequestTerminated {
                        request_id: sub_request_id,
                        kind,
                        reason: reason.clone(),
                    });
                }
            }
        }
        if let Some(entry) = self.track_subscriptions.get_mut(&request_id) {
            entry.state = TrackSubscriptionState::Terminated;
        }
        Ok(reason)
    }

    fn require_track_subscription_publisher(&self, request_id: u64) -> Result<(), SessionError> {
        self.require_established()?;
        let entry = self.track_subscriptions.get(&request_id).ok_or_else(|| {
            SessionError::new(SESSION_PROTOCOL_VIOLATION, "track subscription not found")
        })?;
        if entry.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "operation requires publisher-role track subscription",
            ));
        }
        if entry.state != TrackSubscriptionState::Established {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "track subscription must be Established before sending stream messages",
            ));
        }
        Ok(())
    }

    // ─── REQUEST_OK / REQUEST_ERROR per-kind dispatch (SUBSCRIBE_TRACKS) ──

    pub(crate) fn send_ok_for_track_subscription(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let entry = self
            .track_subscriptions
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_ok (subscribe_tracks) can only be sent by publisher-role",
            ));
        }
        match entry.state {
            TrackSubscriptionState::Pending => {
                entry.state = TrackSubscriptionState::Established;
            }
            TrackSubscriptionState::Established => {
                // REQUEST_UPDATE への成功応答: state は維持する
            }
            TrackSubscriptionState::Terminated => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "request_ok (subscribe_tracks) requires non-terminated state",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn send_err_for_track_subscription(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let entry = self
            .track_subscriptions
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_error (subscribe_tracks) can only be sent by publisher-role",
            ));
        }
        match entry.state {
            TrackSubscriptionState::Pending => {
                entry.state = TrackSubscriptionState::Terminated;
            }
            TrackSubscriptionState::Established => {
                // REQUEST_UPDATE 失敗応答: draft-ietf-moq-transport-21 §9.5.1 (Updating
                // Subscriptions) の "the responder MUST close the bidi stream" により以後
                // REQUEST_UPDATE の送受信は不可能。state を Terminated に遷移させ、
                // `send_update_for_track_subscription` / `handle_update_for_track_subscription`
                // の Established 要求ガードで再 REQUEST_UPDATE を拒否する。
                // bidi 実際の close (RESET_STREAM / STOP_SENDING) は app が担当する
                // (`SessionEvent::ResetRequestStream` は fetch 経路で実装済み。本経路では
                //  Session は close を通知しない)。
                entry.state = TrackSubscriptionState::Terminated;
            }
            TrackSubscriptionState::Terminated => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "send_request_error requires non-terminated state (subscribe_tracks)",
                ));
            }
        }
        Ok(())
    }

    /// peer からの REQUEST_OK を SUBSCRIBE_TRACKS に反映する
    ///
    /// draft-ietf-moq-transport-21 §9.3 (REQUEST_OK): NAMESPACE_OK_ALLOWED_PARAMS は
    /// EXPIRES のみを許可するため、受信パラメータをそのまま
    /// [`SessionEvent::RequestOkReceived`] に格納して application へ通知する。
    pub(crate) fn handle_ok_for_track_subscription(
        &mut self,
        request_id: u64,
        parameters: &MessageParameters,
    ) -> Result<(), SessionError> {
        let entry = self
            .track_subscriptions
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_OK (subscribe_tracks) received on responder side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        match entry.state {
            TrackSubscriptionState::Pending => {
                entry.state = TrackSubscriptionState::Established;
            }
            TrackSubscriptionState::Established => {
                // REQUEST_UPDATE への成功応答: 送信順に積んだ確定待ちの先頭を適用する。
                // draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions): "The receiver MUST
                // still send a REQUEST_OK for each successful update" のため、対応する
                // 確定待ちが無い REQUEST_OK はプロトコル違反とする。
                // なお responder は複数更新を累積結果のみ適用してよい (同節の coalescing) が、
                // 成功応答は更新ごとに届くため OK 1 件を送信順の更新 1 件に対応付けて適用する
                // (非 coalescing 前提の解釈)。
                let Some(pending) =
                    pop_pending_prefix_update(&mut self.pending_prefix_updates, request_id)
                else {
                    let err = SessionError::new(
                        SESSION_PROTOCOL_VIOLATION,
                        "REQUEST_OK (subscribe_tracks) without outstanding REQUEST_UPDATE",
                    );
                    self.fail(err.clone());
                    return Err(err);
                };
                if let Some(new_prefix) = pending {
                    entry.prefix = new_prefix;
                }
            }
            TrackSubscriptionState::Terminated => {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_OK (subscribe_tracks) on terminated request",
                );
                self.fail(err.clone());
                return Err(err);
            }
        }
        // Pending / Established のいずれでも REQUEST_OK 受信を application へ通知する
        // (EXPIRES 等の許可済みパラメータはそのまま伝播する)
        self.events.push_back(SessionEvent::RequestOkReceived {
            request_id,
            request_kind: RequestKind::SubscribeTracks,
            parameters: parameters.clone(),
        });
        Ok(())
    }

    pub(crate) fn handle_err_for_track_subscription(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let entry = self
            .track_subscriptions
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_ERROR (subscribe_tracks) received on responder side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        match entry.state {
            TrackSubscriptionState::Pending => {
                entry.state = TrackSubscriptionState::Terminated;
            }
            TrackSubscriptionState::Established => {
                // REQUEST_UPDATE 失敗応答: draft §9.5.1 の MUST に基づき responder が bidi
                // stream を閉じる。initiator (自側) も state を Terminated に遷移させ、
                // 再 REQUEST_UPDATE を `send_update_for_track_subscription` の
                // Established 要求ガードで拒否する。
                entry.state = TrackSubscriptionState::Terminated;
            }
            TrackSubscriptionState::Terminated => {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_ERROR (subscribe_tracks) on terminated request",
                );
                self.fail(err.clone());
                return Err(err);
            }
        }
        // REQUEST_ERROR 受信で確定待ちは破棄する (REQUEST_OK は届かない)
        self.pending_prefix_updates.remove(&request_id);
        Ok(())
    }

    // ─── REQUEST_UPDATE ────────────────────────────────────

    pub(crate) fn send_update_for_track_subscription(
        &mut self,
        request_id: u64,
        parameters: &MessageParameters,
    ) -> Result<(), SessionError> {
        let entry = self
            .track_subscriptions
            .get(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Subscriber {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_update (subscribe_tracks) can only be sent by subscriber-role",
            ));
        }
        if entry.state != TrackSubscriptionState::Established {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_update requires Established subscribe_tracks",
            ));
        }
        // draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions): "If the update is accepted,
        // NAMESPACE and NAMESPACE_DONE messages following the REQUEST_OK will contain Track
        // Namespace suffixes relative to the updated prefix." prefix 更新は REQUEST_OK
        // 受信までローカルへ反映しない。送信前に SUBSCRIBE_TRACKS 作成時と同じ条件で
        // overlap をローカル検査し、overlap する場合は確定待ちも登録しない。
        // 比較対象は確定待ちを含めた実効 prefix とする。
        let own_effective =
            effective_prefix(&self.pending_prefix_updates, request_id, &entry.prefix);
        let pending_prefix = if let Some(new_prefix) = parameters.track_namespace_prefix()
            && *new_prefix != own_effective
        {
            for (other_id, existing) in &self.track_subscriptions {
                if other_id == &request_id
                    || existing.my_role != TrackRole::Subscriber
                    || existing.state == TrackSubscriptionState::Terminated
                {
                    continue;
                }
                let other_effective =
                    effective_prefix(&self.pending_prefix_updates, *other_id, &existing.prefix);
                if prefix_overlaps(new_prefix, &other_effective) {
                    return Err(SessionError::new(
                        SESSION_PROTOCOL_VIOLATION,
                        "local subscribe_tracks prefix overlaps existing subscription",
                    ));
                }
            }
            Some(new_prefix.clone())
        } else {
            None
        };
        // REQUEST_OK は同一 bidi stream 上で送信順に届く。prefix 変更を含まない
        // 更新も 1 件積み、REQUEST_OK と確定待ちの対応を保つ。
        push_pending_prefix_update(&mut self.pending_prefix_updates, request_id, pending_prefix);
        Ok(())
    }

    pub(crate) fn handle_update_for_track_subscription(
        &mut self,
        request_id: u64,
        parameters: MessageParameters,
    ) -> Result<(), SessionError> {
        let entry = self
            .track_subscriptions
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Publisher {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_UPDATE (subscribe_tracks) received on subscriber side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        if entry.state != TrackSubscriptionState::Established {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_UPDATE requires Established subscribe_tracks",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter): SUBSCRIBE_TRACKS の
        // REQUEST_UPDATE で FORWARD を更新する。将来マッチする subscription の
        // Forwarding State を指定し、既存 Established subscription には影響しない。
        // 値域外 (0 / 1 以外) の受信は MUST でセッションを PROTOCOL_VIOLATION により閉じる。
        // MUST close 系の検証は状態変更より前に行い、エラー時に部分更新を残さない。
        // また REQUEST_ERROR 継続系 (prefix overlap) より先に行う。
        // なお正常 FORWARD と overlap prefix の複合時は forward_state の保存後に
        // REQUEST_ERROR で復帰する部分適用になる (仕様に原子性規定なし。従来の
        // Range Filter と同一順序)。
        let forward_update = if let Some(f) = parameters.forward() {
            match super::super::subscription::validation::validate_forward(f) {
                Ok(validated) => Some(validated),
                Err(err) => {
                    self.fail(err.clone());
                    return Err(err);
                }
            }
        } else {
            None
        };
        // draft §3.3.2 (Range Filters): "In REQUEST_UPDATE, Length can be 0 to remove a filter
        // parameter or non-zero to replace that entire filter parameter including all sets and
        // Property Types. If a filter parameter is omitted from REQUEST_UPDATE, the value is
        // unchanged."
        //
        // `range_filter_sets` は Length=0 のインスタンスを除外するので、パラメータが存在する
        // (= 明示された) ときだけ全置換すれば規則を満たす。Length=0 のみが来た場合は空 Vec に
        // なり、フィルタ削除として機能する。
        if parameters.has_range_filters() {
            entry.track_property_filters =
                parameters.range_filter_sets(PARAM_TRACK_PROPERTY_FILTER);
        }
        if let Some(validated) = forward_update {
            entry.forward_state = validated;
        }
        // draft-ietf-moq-transport-21 §9.20.21 (TRACK_NAMESPACE_PREFIX Parameter): TRACK_NAMESPACE_PREFIX パラメータで prefix を更新
        if let Some(new_prefix) = parameters.track_namespace_prefix()
            && *new_prefix != entry.prefix
        {
            // 更新後の prefix が同タイプの他購読と overlap していないかチェック
            for (other_id, existing) in &self.track_subscriptions {
                if other_id != &request_id
                    && existing.my_role == TrackRole::Publisher
                    && existing.state == TrackSubscriptionState::Established
                    && prefix_overlaps(new_prefix, &existing.prefix)
                {
                    self.emit_request_error(
                        request_id,
                        REQUEST_PREFIX_OVERLAP,
                        "track namespace prefix update overlaps",
                    );
                    return Ok(());
                }
            }
            let entry = self
                .track_subscriptions
                .get_mut(&request_id)
                .expect("locate_request guarantees key presence");
            entry.prefix = new_prefix.clone();
        }
        self.events.push_back(SessionEvent::RequestUpdateReceived {
            request_id,
            parameters,
        });
        Ok(())
    }
}
