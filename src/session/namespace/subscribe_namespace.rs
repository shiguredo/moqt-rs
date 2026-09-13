//! SUBSCRIBE_NAMESPACE / NAMESPACE / NAMESPACE_DONE 関連
//!
//! draft-ietf-moq-transport-21 §4 (Namespace Discovery) / §9.16 (NAMESPACE) /
//! §9.17 (NAMESPACE_DONE) / §9.15 (SUBSCRIBE_NAMESPACE) に対応する
//! `impl Session` の送受信メソッドをまとめる。

use crate::error::{REQUEST_DOES_NOT_EXIST, REQUEST_PREFIX_OVERLAP, SESSION_PROTOCOL_VIOLATION};
use crate::message::{
    ControlMessage, Namespace, NamespaceDone, SubscribeNamespace, common::TrackNamespace,
};
use crate::message_parameter::MessageParameters;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

use super::super::core::Session;
use super::super::types::{
    NamespaceSubscription, NamespaceSubscriptionState, RequestKind, RequestStreamEnd,
    SendRequestError, SessionError, SessionEvent, TerminationReason, TrackRole,
};
use super::{
    effective_prefix, pop_pending_prefix_update, prefix_overlaps, push_pending_prefix_update,
    require_advertisable_suffix, terminationreason_from_end,
};

/// prefix と suffix から full Track Namespace のフィールド列を作る
fn full_namespace_fields(prefix: &TrackNamespace, suffix: &TrackNamespace) -> Vec<Vec<u8>> {
    let mut fields = Vec::new();
    fields.extend(prefix.fields().iter().cloned());
    fields.extend(suffix.fields().iter().cloned());
    fields
}

/// full namespace 集合から現在の prefix 配下の suffix 投影を作り直す
///
/// prefix が変わると、新 prefix 配下にない full namespace は投影から外れる (full namespace 自体は
/// NAMESPACE_DONE の照合のため保持する)。新 prefix 基準で切り出した suffix が TrackNamespace と
/// して不正な場合は投影に含めない。
fn refresh_active_suffixes(
    active_suffixes: &mut hashbrown::HashSet<TrackNamespace>,
    prefix: &TrackNamespace,
    active_full_namespaces: &hashbrown::HashSet<Vec<Vec<u8>>>,
) {
    active_suffixes.clear();
    let prefix_fields = prefix.fields();
    for full in active_full_namespaces {
        if let Some(tail) = full.strip_prefix(prefix_fields)
            && let Ok(suffix) = TrackNamespace::new(tail.to_vec())
        {
            active_suffixes.insert(suffix);
        }
    }
}

/// 受信メッセージの full namespace を解決する候補 prefix 一覧を返す
///
/// draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions): responder は複数 REQUEST_UPDATE を
/// 累積結果のみ適用してよい (coalescing)。その場合 REQUEST_OK の間に届くメッセージは、適用済みの
/// 中間 prefix ではなく累積結果の prefix 基準でありうるため、現在の prefix に加えて確定待ちの
/// prefix でも照合する。
fn candidate_prefixes<'a>(
    current: &'a TrackNamespace,
    pending: Option<&'a VecDeque<Option<TrackNamespace>>>,
) -> Vec<&'a TrackNamespace> {
    let mut prefixes = Vec::new();
    prefixes.push(current);
    if let Some(pending) = pending {
        for prefix in pending.iter().flatten() {
            if !prefixes.contains(&prefix) {
                prefixes.push(prefix);
            }
        }
    }
    prefixes
}

impl Session {
    // ─── クエリ API ─────────────────────────────────────────

    /// SUBSCRIBE_NAMESPACE 参照
    pub fn namespace_subscription(&self, request_id: u64) -> Option<&NamespaceSubscription> {
        self.namespaces.subscriptions.get(&request_id)
    }

    /// 全 SUBSCRIBE_NAMESPACE エントリの反復子を返す
    pub fn namespace_subscriptions(&self) -> impl Iterator<Item = &NamespaceSubscription> {
        self.namespaces.subscriptions.values()
    }

    /// Terminated の SUBSCRIBE_NAMESPACE を除去
    pub fn forget_namespace_subscription(
        &mut self,
        request_id: u64,
    ) -> Option<NamespaceSubscription> {
        let entry = self.namespaces.subscriptions.get(&request_id)?;
        if entry.state == NamespaceSubscriptionState::Pending {
            return None;
        }
        self.request_streams.remove(&request_id);
        self.remove_request_update_credit_entries(request_id);
        self.pending_prefix_updates.remove(&request_id);
        self.namespaces.active_full_namespaces.remove(&request_id);
        self.clear_request_stream_goaway_deadline(request_id);
        self.namespaces.subscriptions.remove(&request_id)
    }

    // ─── 送信 API ──────────────────────────────────────────

    /// SUBSCRIBE_NAMESPACE を送信する (draft §9.15 (SUBSCRIBE_NAMESPACE)、自側が subscriber)
    pub fn send_subscribe_namespace(
        &mut self,
        prefix: TrackNamespace,
        parameters: MessageParameters,
    ) -> Result<u64, SendRequestError> {
        self.require_established()?;
        self.check_peer_goaway()?;
        // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces): single period `.` 予約名前空間への SUBSCRIBE_NAMESPACE は送信不可
        if prefix.is_single_period() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "application cannot use single-period reserved namespace",
            )
            .into());
        }
        // draft §9.15 (SUBSCRIBE_NAMESPACE): SUBSCRIBE_NAMESPACE テーブル内での prefix overlap チェック。
        // 確定待ちの REQUEST_UPDATE がある購読とは反映後の実効 prefix で比較し、
        // Terminated の購読は active ではないため対象外とする
        // (draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions))。
        for (existing_id, existing) in &self.namespaces.subscriptions {
            if existing.my_role != TrackRole::Subscriber
                || existing.state == NamespaceSubscriptionState::Terminated
            {
                continue;
            }
            let existing_effective =
                effective_prefix(&self.pending_prefix_updates, *existing_id, &existing.prefix);
            if prefix_overlaps(&prefix, &existing_effective) {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "local subscribe_namespace prefix overlaps existing subscription",
                )
                .into());
            }
        }
        // draft §9.20.1 (Parameter Scope): SUBSCRIBE_NAMESPACE で許可されないパラメータを
        // 含む送信は API 呼び出し時に拒否する。検証がないと、NamespaceSubscription の登録・
        // control deadline の開始・ SendRequest の push まで完了した後に I/O 層のエンコード
        // 時 (validate_scope) で非同期に失敗し、アプリがエンコードエラーを無視すると
        // control deadline タイムアウトという後続の誤作動につながる。
        // 検証は request_id 発行より前に置き、エラー時に欠番を作らない
        // (send_subscribe_tracks と同じ設計)。
        if parameters
            .validate_scope(crate::message::SUBSCRIBE_NAMESPACE_ALLOWED_PARAMS)
            .is_err()
        {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "SUBSCRIBE_NAMESPACE parameter not allowed in this context",
            )
            .into());
        }
        let request_id = self.request_ids.local_generator.next_id();
        self.namespaces.subscriptions.insert(
            request_id,
            NamespaceSubscription {
                request_id,
                my_role: TrackRole::Subscriber,
                prefix: prefix.clone(),
                state: NamespaceSubscriptionState::Pending,
                active_suffixes: hashbrown::HashSet::new(),
            },
        );
        self.request_streams
            .insert(request_id, RequestKind::SubscribeNamespace);
        self.start_control_message_deadline(request_id);
        let msg = ControlMessage::SubscribeNamespace(SubscribeNamespace {
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

    /// NAMESPACE を送信する (SUBSCRIBE_NAMESPACE 応答 stream、publisher 側)
    ///
    /// # Errors
    ///
    /// 空 prefix の購読で suffix の先頭フィールドが予約名前空間 (`.` / `.session`) の場合は
    /// `SESSION_PROTOCOL_VIOLATION` を返す (draft-ietf-moq-transport-21 §6.5 / §2.4.2)。
    /// request id が存在しない場合、publisher 役でない場合、セッションが Established でない
    /// 場合も同じコードを返す。
    pub fn send_namespace(
        &mut self,
        request_id: u64,
        suffix: TrackNamespace,
    ) -> Result<(), SessionError> {
        self.require_namespace_subscription_publisher(request_id)?;
        let prefix = &self
            .namespaces
            .subscriptions
            .get(&request_id)
            .expect("require_namespace_subscription_publisher guarantees key presence")
            .prefix;
        require_advertisable_suffix(prefix, &suffix)?;
        let msg = ControlMessage::Namespace(Namespace {
            track_namespace_suffix: suffix,
        });
        self.events.push_back(SessionEvent::SendOnStream {
            request_id,
            message: msg,
            fin: false,
        });
        Ok(())
    }

    /// NAMESPACE_DONE を送信する
    ///
    /// # Errors
    ///
    /// 空 prefix の購読で suffix の先頭フィールドが予約名前空間 (`.` / `.session`) の場合は
    /// `SESSION_PROTOCOL_VIOLATION` を返す (draft-ietf-moq-transport-21 §6.5 / §2.4.2)。
    /// request id が存在しない場合、publisher 役でない場合、セッションが Established でない
    /// 場合も同じコードを返す。
    pub fn send_namespace_done(
        &mut self,
        request_id: u64,
        suffix: TrackNamespace,
    ) -> Result<(), SessionError> {
        self.require_namespace_subscription_publisher(request_id)?;
        let prefix = &self
            .namespaces
            .subscriptions
            .get(&request_id)
            .expect("require_namespace_subscription_publisher guarantees key presence")
            .prefix;
        require_advertisable_suffix(prefix, &suffix)?;
        let msg = ControlMessage::NamespaceDone(NamespaceDone {
            track_namespace_suffix: suffix,
        });
        self.events.push_back(SessionEvent::SendOnStream {
            request_id,
            message: msg,
            fin: false,
        });
        Ok(())
    }

    /// bidi request stream 終端時の SUBSCRIBE_NAMESPACE 側の処理
    ///
    /// draft-ietf-moq-transport-21 §9.15 (SUBSCRIBE_NAMESPACE): stream 終端時は active だった
    /// 全 suffix に implicit NAMESPACE_DONE が届いたとみなす。subscription 状態は
    /// `Terminated` に遷移する。`active_suffixes` が空ならそのまま `end` 由来の
    /// `TerminationReason` を返し、非空なら `NamespaceImplicitDone` に置き換える。
    pub(crate) fn close_namespace_subscription_on_stream_end(
        &mut self,
        request_id: u64,
        end: RequestStreamEnd,
    ) -> Result<TerminationReason, SessionError> {
        let Some(entry) = self.namespaces.subscriptions.get_mut(&request_id) else {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "bidi request stream close for unknown SUBSCRIBE_NAMESPACE request id",
            ));
        };
        let implicit_done: Vec<TrackNamespace> = entry.active_suffixes.drain().collect();
        entry.state = NamespaceSubscriptionState::Terminated;
        // Terminated 後は NAMESPACE / NAMESPACE_DONE を受理しないため full namespace も破棄する
        self.namespaces.active_full_namespaces.remove(&request_id);
        // bidi stream 終端で確定待ちは破棄する (REQUEST_OK は届かない)
        self.pending_prefix_updates.remove(&request_id);
        let reason = if implicit_done.is_empty() {
            terminationreason_from_end(end)
        } else {
            TerminationReason::NamespaceImplicitDone {
                suffixes: implicit_done,
            }
        };
        Ok(reason)
    }

    fn require_namespace_subscription_publisher(
        &self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        let entry = self
            .namespaces
            .subscriptions
            .get(&request_id)
            .ok_or_else(|| {
                SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "namespace subscription not found",
                )
            })?;
        if entry.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "operation requires publisher-role namespace subscription",
            ));
        }
        // draft §4.1 (Subscribing to Namespaces) / §9.15 (SUBSCRIBE_NAMESPACE): 応答 stream の first frame は
        // REQUEST_OK / REQUEST_ERROR でなければならない。REQUEST_OK 送信 (= state
        // Pending → Established) 前に NAMESPACE / NAMESPACE_DONE / PUBLISH_SKIPPED
        // を書き出すのは spec 違反となるため、送信は Established 状態のみ許す。
        if entry.state != NamespaceSubscriptionState::Established {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "namespace subscription must be Established before sending stream messages",
            ));
        }
        Ok(())
    }

    // ─── 受信ハンドラ ──────────────────────────────────────

    pub(crate) fn handle_peer_subscribe_namespace(
        &mut self,
        msg: SubscribeNamespace,
    ) -> Result<(), SessionError> {
        let request_id = msg.request_id;
        if !self.accept_peer_request(request_id, &msg.parameters)? {
            return Ok(());
        }
        self.apply_peer_message_auth_tokens(&msg.parameters)?;
        // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces): single period `.` 予約名前空間の SUBSCRIBE_NAMESPACE は拒否
        if msg.track_namespace_prefix.is_single_period() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "reserved single-period namespace",
            );
            return Ok(());
        }
        // draft §6.5 (Session-Level Tracks and Namespaces): .session 名前空間の SUBSCRIBE_NAMESPACE は内部処理
        if msg.track_namespace_prefix.is_session_level() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "session-level namespace does not exist",
            );
            return Ok(());
        }
        // draft §9.15 (SUBSCRIBE_NAMESPACE): SUBSCRIBE_NAMESPACE テーブル内での prefix overlap チェック
        for existing in self.namespaces.subscriptions.values() {
            if existing.my_role == TrackRole::Publisher
                && existing.state == NamespaceSubscriptionState::Established
                && prefix_overlaps(&msg.track_namespace_prefix, &existing.prefix)
            {
                self.emit_request_error(
                    request_id,
                    REQUEST_PREFIX_OVERLAP,
                    "subscribe_namespace prefix overlaps",
                );
                return Ok(());
            }
        }
        self.namespaces.subscriptions.insert(
            request_id,
            NamespaceSubscription {
                request_id,
                my_role: TrackRole::Publisher,
                prefix: msg.track_namespace_prefix,
                state: NamespaceSubscriptionState::Pending,
                active_suffixes: hashbrown::HashSet::new(),
            },
        );
        self.request_streams
            .insert(request_id, RequestKind::SubscribeNamespace);
        Ok(())
    }

    pub(crate) fn handle_peer_namespace(
        &mut self,
        request_id: u64,
        msg: Namespace,
    ) -> Result<(), SessionError> {
        // 該当する SUBSCRIBE_NAMESPACE (自側 my_role=Subscriber) が必要
        let Some(entry) = self.namespaces.subscriptions.get_mut(&request_id) else {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "NAMESPACE received for unknown request id",
            );
            self.fail(err.clone());
            return Err(err);
        };
        if entry.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "NAMESPACE received on publisher side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft §4.1 (Subscribing to Namespaces) / §9.15 (SUBSCRIBE_NAMESPACE): 応答 stream の first frame は
        // REQUEST_OK / REQUEST_ERROR でなければならない。state が Pending のまま
        // NAMESPACE を受信した場合は REQUEST_OK 以前に他 frame が来たことを意味するので
        // PROTOCOL_VIOLATION でセッションを閉じる。
        if entry.state != NamespaceSubscriptionState::Established {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "NAMESPACE received before REQUEST_OK on SUBSCRIBE_NAMESPACE stream",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §9.15 (SUBSCRIBE_NAMESPACE) / §9.16 (NAMESPACE) /
        // §9.17 (NAMESPACE_DONE): 同一 suffix の NAMESPACE 重複受信を違反とする規定はないため、
        // MUST でない条件でセッションを閉じない。prefix を跨いだ同一性を判定するため、
        // 候補 prefix (現在 + 確定待ち) で解決した full namespace のフィールド列で一意化し、
        // 重複は無視して `NamespaceReceived` も再発行しない。NAMESPACE_DONE で削除された後の
        // 再告知は新規として受理する。
        let suffix = msg.track_namespace_suffix;
        let pending = self.pending_prefix_updates.get(&request_id);
        let prefixes = candidate_prefixes(&entry.prefix, pending);
        let duplicate = prefixes.iter().any(|prefix| {
            self.namespaces
                .active_full_namespaces
                .get(&request_id)
                .is_some_and(|active| active.contains(&full_namespace_fields(prefix, &suffix)))
        });
        if duplicate {
            return Ok(());
        }
        // 新しい full namespace は現在の prefix 基準で登録する
        let full = full_namespace_fields(&entry.prefix, &suffix);
        self.namespaces
            .active_full_namespaces
            .entry(request_id)
            .or_default()
            .insert(full);
        entry.active_suffixes.insert(suffix.clone());
        self.events
            .push_back(SessionEvent::NamespaceReceived { request_id, suffix });
        Ok(())
    }

    pub(crate) fn handle_peer_namespace_done(
        &mut self,
        request_id: u64,
        msg: NamespaceDone,
    ) -> Result<(), SessionError> {
        let Some(entry) = self.namespaces.subscriptions.get_mut(&request_id) else {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "NAMESPACE_DONE received for unknown request id",
            );
            self.fail(err.clone());
            return Err(err);
        };
        if entry.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "NAMESPACE_DONE received on publisher side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft §4.1 (Subscribing to Namespaces) / §9.15 (SUBSCRIBE_NAMESPACE): first frame 制約。
        // REQUEST_OK 受信前は Established になっていないので violation。
        if entry.state != NamespaceSubscriptionState::Established {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "NAMESPACE_DONE received before REQUEST_OK on SUBSCRIBE_NAMESPACE stream",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft §9.15 (SUBSCRIBE_NAMESPACE): 対応する NAMESPACE を受けていない NAMESPACE_DONE は
        // PROTOCOL_VIOLATION。prefix を跨いだ同一性判定と coalescing の OK 間メッセージ対応のため、
        // 候補 prefix (現在 + 確定待ち) で解決した full namespace のフィールド列で照合する。
        let suffix = msg.track_namespace_suffix;
        let pending = self.pending_prefix_updates.get(&request_id);
        let prefixes = candidate_prefixes(&entry.prefix, pending);
        let mut removed_any = false;
        for prefix in &prefixes {
            let full = full_namespace_fields(prefix, &suffix);
            if let Some(active) = self.namespaces.active_full_namespaces.get_mut(&request_id)
                && active.remove(&full)
            {
                removed_any = true;
            }
        }
        if !removed_any {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "NAMESPACE_DONE received before corresponding NAMESPACE",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // 空になったエントリは除去し、投影の単一の真実である full namespace 集合から作り直す
        if self
            .namespaces
            .active_full_namespaces
            .get(&request_id)
            .is_some_and(|active| active.is_empty())
        {
            self.namespaces.active_full_namespaces.remove(&request_id);
        }
        if let Some(active) = self.namespaces.active_full_namespaces.get(&request_id) {
            refresh_active_suffixes(&mut entry.active_suffixes, &entry.prefix, active);
        } else {
            entry.active_suffixes.clear();
        }
        self.events
            .push_back(SessionEvent::NamespaceDoneReceived { request_id, suffix });
        Ok(())
    }

    // ─── REQUEST_UPDATE ────────────────────────────────────

    pub(crate) fn send_update_for_namespace_subscription(
        &mut self,
        request_id: u64,
        parameters: &MessageParameters,
    ) -> Result<(), SessionError> {
        let entry = self
            .namespaces
            .subscriptions
            .get(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Subscriber {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_update (subscribe_namespace) can only be sent by subscriber-role",
            ));
        }
        if entry.state != NamespaceSubscriptionState::Established {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_update requires Established subscribe_namespace",
            ));
        }
        // draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions): "If the update is accepted,
        // NAMESPACE and NAMESPACE_DONE messages following the REQUEST_OK will contain Track
        // Namespace suffixes relative to the updated prefix." prefix 更新は REQUEST_OK
        // 受信までローカルへ反映しない。送信前に SUBSCRIBE_NAMESPACE 作成時と同じ条件で
        // overlap をローカル検査し、overlap する場合は確定待ちも登録しない。
        // 比較対象は確定待ちを含めた実効 prefix とする。
        let own_effective =
            effective_prefix(&self.pending_prefix_updates, request_id, &entry.prefix);
        // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces) / §6.5 (Session-Level Tracks and Namespaces):
        // 確立後の prefix 更新でも初回購読時の受信検証と同じ条件で予約名前空間をローカル拒否する。
        // 判定は最初の名前空間フィールドのみを見る。そのため `[".", "x"]` のような複数フィールド形も
        // 拒否対象になる。
        // 現在の prefix と同一の値でも検査するため、変更判定より前に置く。
        if let Some(new_prefix) = parameters.track_namespace_prefix() {
            if new_prefix.is_single_period() {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "application cannot use single-period reserved namespace",
                ));
            }
            if new_prefix.is_session_level() {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "application cannot use .session reserved namespace",
                ));
            }
        }
        let pending_prefix = if let Some(new_prefix) = parameters.track_namespace_prefix()
            && *new_prefix != own_effective
        {
            for (other_id, existing) in &self.namespaces.subscriptions {
                if other_id == &request_id
                    || existing.my_role != TrackRole::Subscriber
                    || existing.state == NamespaceSubscriptionState::Terminated
                {
                    continue;
                }
                let other_effective =
                    effective_prefix(&self.pending_prefix_updates, *other_id, &existing.prefix);
                if prefix_overlaps(new_prefix, &other_effective) {
                    return Err(SessionError::new(
                        SESSION_PROTOCOL_VIOLATION,
                        "local subscribe_namespace prefix overlaps existing subscription",
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

    pub(crate) fn handle_update_for_namespace_subscription(
        &mut self,
        request_id: u64,
        parameters: MessageParameters,
    ) -> Result<(), SessionError> {
        let entry = self
            .namespaces
            .subscriptions
            .get(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Publisher {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_UPDATE (subscribe_namespace) received on subscriber side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        if entry.state != NamespaceSubscriptionState::Established {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_UPDATE requires Established subscribe_namespace",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces) / §6.5 (Session-Level Tracks and Namespaces):
        // 確立後の prefix 更新でも初回の SUBSCRIBE_NAMESPACE と同じ予約名前空間を再検証し、
        // 初回拒否を更新経路で迂回できないようにする。判定は最初の名前空間フィールドのみを見る。
        // overlap より先に判定する。
        if let Some(new_prefix) = parameters.track_namespace_prefix() {
            if new_prefix.is_single_period() {
                self.emit_request_error(
                    request_id,
                    REQUEST_DOES_NOT_EXIST,
                    "reserved single-period namespace",
                );
                return Ok(());
            }
            if new_prefix.is_session_level() {
                self.emit_request_error(
                    request_id,
                    REQUEST_DOES_NOT_EXIST,
                    "session-level namespace does not exist",
                );
                return Ok(());
            }
        }
        // draft-ietf-moq-transport-21 §9.20.21 (TRACK_NAMESPACE_PREFIX Parameter): TRACK_NAMESPACE_PREFIX パラメータで prefix を更新
        if let Some(new_prefix) = parameters.track_namespace_prefix()
            && *new_prefix != entry.prefix
        {
            // 更新後の prefix が同タイプの他購読と overlap していないかチェック
            for (other_id, existing) in &self.namespaces.subscriptions {
                if other_id != &request_id
                    && existing.my_role == TrackRole::Publisher
                    && existing.state == NamespaceSubscriptionState::Established
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
                .namespaces
                .subscriptions
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

    // ─── REQUEST_OK / REQUEST_ERROR per-kind dispatch (SUBSCRIBE_NAMESPACE) ──

    pub(crate) fn send_ok_for_namespace_subscription(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let entry = self
            .namespaces
            .subscriptions
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_ok (subscribe_namespace) can only be sent by publisher-role",
            ));
        }
        match entry.state {
            NamespaceSubscriptionState::Pending => {
                entry.state = NamespaceSubscriptionState::Established;
            }
            NamespaceSubscriptionState::Established => {
                // REQUEST_UPDATE への成功応答: state は維持する
            }
            NamespaceSubscriptionState::Terminated => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "request_ok (subscribe_namespace) requires non-terminated state",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn send_err_for_namespace_subscription(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let entry = self
            .namespaces
            .subscriptions
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_error (subscribe_namespace) can only be sent by publisher-role",
            ));
        }
        match entry.state {
            NamespaceSubscriptionState::Pending => {
                entry.state = NamespaceSubscriptionState::Terminated;
            }
            NamespaceSubscriptionState::Established => {
                // REQUEST_UPDATE 失敗応答: draft-ietf-moq-transport-21 §9.5.1 (Updating
                // Subscriptions) の "the responder MUST close the bidi stream" により以後
                // REQUEST_UPDATE の送受信は不可能。state を Terminated に遷移させ、
                // `send_update_for_namespace_subscription` / `handle_update_for_namespace_subscription`
                // の Established 要求ガードで再 REQUEST_UPDATE を拒否する。
                // bidi 実際の close (RESET_STREAM / STOP_SENDING) は app が担当する
                // (`SessionEvent::ResetRequestStream` は fetch 経路で実装済み。本経路では
                //  Session は close を通知しない)。
                entry.state = NamespaceSubscriptionState::Terminated;
            }
            NamespaceSubscriptionState::Terminated => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "send_request_error requires non-terminated state (subscribe_namespace)",
                ));
            }
        }
        Ok(())
    }

    /// peer からの REQUEST_OK を SUBSCRIBE_NAMESPACE に反映する
    ///
    /// draft-ietf-moq-transport-21 §9.3 (REQUEST_OK): NAMESPACE_OK_ALLOWED_PARAMS は
    /// EXPIRES のみを許可するため、受信パラメータをそのまま
    /// [`SessionEvent::RequestOkReceived`] に格納して application へ通知する。
    pub(crate) fn handle_ok_for_namespace_subscription(
        &mut self,
        request_id: u64,
        parameters: &MessageParameters,
    ) -> Result<(), SessionError> {
        let entry = self
            .namespaces
            .subscriptions
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_OK (subscribe_namespace) received on responder side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        match entry.state {
            NamespaceSubscriptionState::Pending => {
                entry.state = NamespaceSubscriptionState::Established;
            }
            NamespaceSubscriptionState::Established => {
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
                        "REQUEST_OK (subscribe_namespace) without outstanding REQUEST_UPDATE",
                    );
                    self.fail(err.clone());
                    return Err(err);
                };
                if let Some(new_prefix) = pending {
                    entry.prefix = new_prefix;
                    // draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions):
                    // prefix 更新後の NAMESPACE / NAMESPACE_DONE は新 prefix 相対になる。
                    // 新 prefix 配下にない full namespace は投影から外す (full namespace は
                    // NAMESPACE_DONE の照合のため保持する)。coalescing した peer が中間 prefix を
                    // 適用していない場合でも、旧基準の NAMESPACE_DONE を §9.15 に従って照合できる
                    // (draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions))。
                    if let Some(active) = self.namespaces.active_full_namespaces.get(&request_id) {
                        refresh_active_suffixes(&mut entry.active_suffixes, &entry.prefix, active);
                    } else {
                        entry.active_suffixes.clear();
                    }
                }
            }
            NamespaceSubscriptionState::Terminated => {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_OK (subscribe_namespace) on terminated request",
                );
                self.fail(err.clone());
                return Err(err);
            }
        }
        // Pending / Established のいずれでも REQUEST_OK 受信を application へ通知する
        // (EXPIRES 等の許可済みパラメータはそのまま伝播する)
        self.events.push_back(SessionEvent::RequestOkReceived {
            request_id,
            request_kind: RequestKind::SubscribeNamespace,
            parameters: parameters.clone(),
        });
        Ok(())
    }

    pub(crate) fn handle_err_for_namespace_subscription(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let entry = self
            .namespaces
            .subscriptions
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_ERROR (subscribe_namespace) received on responder side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        match entry.state {
            NamespaceSubscriptionState::Pending => {
                entry.state = NamespaceSubscriptionState::Terminated;
            }
            NamespaceSubscriptionState::Established => {
                // REQUEST_UPDATE 失敗応答: draft §9.5.1 の MUST に基づき responder が bidi
                // stream を閉じる。initiator (自側) も state を Terminated に遷移させ、
                // 再 REQUEST_UPDATE を `send_update_for_namespace_subscription` の
                // Established 要求ガードで拒否する。
                entry.state = NamespaceSubscriptionState::Terminated;
            }
            NamespaceSubscriptionState::Terminated => {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_ERROR (subscribe_namespace) on terminated request",
                );
                self.fail(err.clone());
                return Err(err);
            }
        }
        // REQUEST_ERROR 受信で確定待ちは破棄する (REQUEST_OK は届かない)
        self.pending_prefix_updates.remove(&request_id);
        // Terminated 後は NAMESPACE / NAMESPACE_DONE を受理しないため full namespace も破棄する
        self.namespaces.active_full_namespaces.remove(&request_id);
        Ok(())
    }
}
