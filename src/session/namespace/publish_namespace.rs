//! PUBLISH_NAMESPACE 関連
//!
//! draft-ietf-moq-transport-21 §9.14 (PUBLISH_NAMESPACE) に対応する
//! `impl Session` の送受信メソッドをまとめる。

use crate::error::{REQUEST_DOES_NOT_EXIST, SESSION_PROTOCOL_VIOLATION};
use crate::message::{ControlMessage, PublishNamespace, common::TrackNamespace};
use crate::message_parameter::MessageParameters;

use super::super::core::Session;
use super::super::types::{
    NamespacePublication, NamespacePublicationState, RequestKind, RequestStreamEnd,
    SendRequestError, SessionError, SessionEvent, TerminationReason, TrackRole,
};
use super::terminationreason_from_end;

impl Session {
    // ─── クエリ API ─────────────────────────────────────────

    /// PUBLISH_NAMESPACE 参照
    pub fn namespace_publication(&self, request_id: u64) -> Option<&NamespacePublication> {
        self.namespaces.publications.get(&request_id)
    }

    /// 全 PUBLISH_NAMESPACE エントリの反復子を返す
    pub fn namespace_publications(&self) -> impl Iterator<Item = &NamespacePublication> {
        self.namespaces.publications.values()
    }

    /// Terminated / Established の PUBLISH_NAMESPACE を除去
    pub fn forget_namespace_publication(
        &mut self,
        request_id: u64,
    ) -> Option<NamespacePublication> {
        let entry = self.namespaces.publications.get(&request_id)?;
        if entry.state == NamespacePublicationState::Pending {
            return None;
        }
        self.request_streams.remove(&request_id);
        self.remove_request_update_credit_entries(request_id);
        self.namespaces.publications.remove(&request_id)
    }

    // ─── 送信 API ──────────────────────────────────────────

    /// PUBLISH_NAMESPACE を送信する (draft §9.14 (PUBLISH_NAMESPACE)、自側が publisher)
    pub fn send_publish_namespace(
        &mut self,
        track_namespace: TrackNamespace,
        parameters: MessageParameters,
    ) -> Result<u64, SendRequestError> {
        self.require_established()?;
        self.check_peer_goaway()?;
        // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces): single period `.` 予約名前空間への PUBLISH_NAMESPACE は送信不可
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
        // draft §9.20.1 (Parameter Scope): PUBLISH_NAMESPACE で許可されないパラメータを含む
        // 送信は API 呼び出し時に拒否する。検証がないと、NamespacePublication の登録・
        // control deadline の開始・ SendRequest の push まで完了した後に I/O 層のエンコード
        // 時 (validate_scope) で非同期に失敗し、アプリがエンコードエラーを無視すると
        // control deadline タイムアウトという後続の誤作動につながる。
        // 検証は request_id 発行より前に置き、エラー時に欠番を作らない
        // (send_subscribe_tracks と同じ設計)。
        if parameters
            .validate_scope(crate::message::PUBLISH_NAMESPACE_ALLOWED_PARAMS)
            .is_err()
        {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH_NAMESPACE parameter not allowed in this context",
            )
            .into());
        }
        let request_id = self.request_ids.local_generator.next_id();
        self.namespaces.publications.insert(
            request_id,
            NamespacePublication {
                request_id,
                my_role: TrackRole::Publisher,
                track_namespace: track_namespace.clone(),
                state: NamespacePublicationState::Pending,
            },
        );
        self.request_streams
            .insert(request_id, RequestKind::PublishNamespace);
        self.start_control_message_deadline(request_id);
        let msg = ControlMessage::PublishNamespace(PublishNamespace {
            request_id,
            track_namespace,
            parameters,
        });
        self.events.push_back(SessionEvent::SendRequest {
            request_id,
            message: msg,
        });
        Ok(request_id)
    }

    /// bidi request stream 終端時の PUBLISH_NAMESPACE 側の処理
    pub(crate) fn close_namespace_publication_on_stream_end(
        &mut self,
        request_id: u64,
        end: RequestStreamEnd,
    ) -> Result<TerminationReason, SessionError> {
        let Some(entry) = self.namespaces.publications.get_mut(&request_id) else {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "bidi request stream close for unknown PUBLISH_NAMESPACE request id",
            ));
        };
        entry.state = NamespacePublicationState::Terminated;
        Ok(terminationreason_from_end(end))
    }

    // ─── 受信ハンドラ ──────────────────────────────────────

    pub(crate) fn handle_peer_publish_namespace(
        &mut self,
        msg: PublishNamespace,
    ) -> Result<(), SessionError> {
        let request_id = msg.request_id;
        if !self.accept_peer_request(request_id, &msg.parameters)? {
            return Ok(());
        }
        // draft §9.20.3 (AUTHORIZATION TOKEN Parameter): AUTHORIZATION_TOKEN Register/Delete/Use を peer cache に反映
        self.apply_peer_message_auth_tokens(&msg.parameters)?;
        // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces): single period `.` 予約名前空間の PUBLISH_NAMESPACE は拒否
        if msg.track_namespace.is_single_period() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "reserved single-period namespace",
            );
            return Ok(());
        }
        // draft §6.5 (Session-Level Tracks and Namespaces): .session 名前空間の PUBLISH_NAMESPACE は内部処理
        if msg.track_namespace.is_session_level() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "session-level namespace does not exist",
            );
            return Ok(());
        }
        self.namespaces.publications.insert(
            request_id,
            NamespacePublication {
                request_id,
                my_role: TrackRole::Subscriber,
                track_namespace: msg.track_namespace,
                state: NamespacePublicationState::Pending,
            },
        );
        self.request_streams
            .insert(request_id, RequestKind::PublishNamespace);
        Ok(())
    }

    // ─── REQUEST_UPDATE ────────────────────────────────────

    pub(crate) fn send_update_for_namespace_publication(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let entry = self
            .namespaces
            .publications
            .get(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_update (publish_namespace) can only be sent by publisher-role",
            ));
        }
        if entry.state != NamespacePublicationState::Established {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_update requires Established publish_namespace",
            ));
        }
        Ok(())
    }

    pub(crate) fn handle_update_for_namespace_publication(
        &mut self,
        request_id: u64,
        parameters: MessageParameters,
    ) -> Result<(), SessionError> {
        let entry = self
            .namespaces
            .publications
            .get(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_UPDATE (publish_namespace) received on publisher side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        if entry.state != NamespacePublicationState::Established {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_UPDATE requires Established publish_namespace",
            );
            self.fail(err.clone());
            return Err(err);
        }
        self.events.push_back(SessionEvent::RequestUpdateReceived {
            request_id,
            parameters,
        });
        Ok(())
    }

    // ─── REQUEST_OK / REQUEST_ERROR per-kind dispatch (PUBLISH_NAMESPACE) ──

    pub(crate) fn send_ok_for_namespace_publication(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let entry = self
            .namespaces
            .publications
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Subscriber {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_ok (publish_namespace) can only be sent by subscriber-role",
            ));
        }
        match entry.state {
            NamespacePublicationState::Pending => {
                entry.state = NamespacePublicationState::Established;
            }
            NamespacePublicationState::Established => {
                // REQUEST_UPDATE への成功応答: state は維持する
            }
            NamespacePublicationState::Terminated => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "request_ok (publish_namespace) requires non-terminated state",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn send_err_for_namespace_publication(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let entry = self
            .namespaces
            .publications
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Subscriber {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_error (publish_namespace) can only be sent by subscriber-role",
            ));
        }
        match entry.state {
            NamespacePublicationState::Pending => {
                entry.state = NamespacePublicationState::Terminated;
            }
            NamespacePublicationState::Established => {
                // REQUEST_UPDATE 失敗応答: draft-ietf-moq-transport-21 §9.5.1 (Updating
                // Subscriptions) の "the responder MUST close the bidi stream" により以後
                // REQUEST_UPDATE の送受信は不可能。state を Terminated に遷移させ、
                // `send_update_for_namespace_publication` / `handle_update_for_namespace_publication`
                // の Established 要求ガードで再 REQUEST_UPDATE を拒否する。
                // bidi 実際の close (RESET_STREAM / STOP_SENDING) は app が担当する
                // (Session が close 意図を通知する SessionEvent variant は未実装)。
                entry.state = NamespacePublicationState::Terminated;
            }
            NamespacePublicationState::Terminated => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "send_request_error requires non-terminated state (publish_namespace)",
                ));
            }
        }
        Ok(())
    }

    /// peer からの REQUEST_OK を PUBLISH_NAMESPACE に反映する
    ///
    /// draft-ietf-moq-transport-21 §9.3 (REQUEST_OK): NAMESPACE_OK_ALLOWED_PARAMS は
    /// EXPIRES のみを許可するため、受信パラメータをそのまま
    /// [`SessionEvent::RequestOkReceived`] に格納して application へ通知する。
    pub(crate) fn handle_ok_for_namespace_publication(
        &mut self,
        request_id: u64,
        parameters: &MessageParameters,
    ) -> Result<(), SessionError> {
        let entry = self
            .namespaces
            .publications
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Publisher {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_OK (publish_namespace) received on responder side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        match entry.state {
            NamespacePublicationState::Pending => {
                entry.state = NamespacePublicationState::Established;
            }
            NamespacePublicationState::Established => {
                // REQUEST_UPDATE への成功応答: state は維持する
            }
            NamespacePublicationState::Terminated => {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_OK (publish_namespace) on terminated request",
                );
                self.fail(err.clone());
                return Err(err);
            }
        }
        // Pending / Established のいずれでも REQUEST_OK 受信を application へ通知する
        // (EXPIRES 等の許可済みパラメータはそのまま伝播する)
        self.events.push_back(SessionEvent::RequestOkReceived {
            request_id,
            request_kind: RequestKind::PublishNamespace,
            parameters: parameters.clone(),
        });
        Ok(())
    }

    pub(crate) fn handle_err_for_namespace_publication(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let entry = self
            .namespaces
            .publications
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Publisher {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_ERROR (publish_namespace) received on responder side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        match entry.state {
            NamespacePublicationState::Pending => {
                entry.state = NamespacePublicationState::Terminated;
            }
            NamespacePublicationState::Established => {
                // REQUEST_UPDATE 失敗応答: draft §9.5.1 の MUST に基づき responder が bidi
                // stream を閉じる。initiator (自側) も state を Terminated に遷移させ、
                // 再 REQUEST_UPDATE を `send_update_for_namespace_publication` の
                // Established 要求ガードで拒否する。
                entry.state = NamespacePublicationState::Terminated;
            }
            NamespacePublicationState::Terminated => {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_ERROR (publish_namespace) on terminated request",
                );
                self.fail(err.clone());
                return Err(err);
            }
        }
        Ok(())
    }
}
