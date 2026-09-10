//! TRACK_STATUS 関連
//!
//! draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS) に対応する
//! `impl Session` の送受信メソッドをまとめる。

use crate::error::{REQUEST_DOES_NOT_EXIST, SESSION_PROTOCOL_VIOLATION};
use crate::message::{
    ControlMessage, TrackStatus as WireTrackStatus, common::Location, common::TrackNamespace,
};
use crate::message_parameter::MessageParameters;
use alloc::vec::Vec;

use super::super::core::Session;
use super::super::subscription::delivery::update_largest_object_in_parameters;
use super::super::types::{
    RequestKind, RequestStreamEnd, SendRequestError, SessionError, SessionEvent, TerminationReason,
    TrackRole, TrackStatusEntry, TrackStatusResponse,
};
use super::terminationreason_from_end;

impl Session {
    // ─── クエリ API ─────────────────────────────────────────

    /// TRACK_STATUS 参照
    pub fn track_status_request(&self, request_id: u64) -> Option<&TrackStatusEntry> {
        self.track_status_requests.get(&request_id)
    }

    /// 全 TRACK_STATUS エントリの反復子を返す
    pub fn track_status_requests(&self) -> impl Iterator<Item = &TrackStatusEntry> {
        self.track_status_requests.values()
    }

    /// 応答済み (Ok / Error) の TRACK_STATUS を除去する
    ///
    /// draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS): TRACK_STATUS_OK / REQUEST_ERROR は
    /// FIN で送られる。bidi stream 終端を `recv_request_stream_closed` で通知してから
    /// 呼ぶこと (終端前に除去すると、後から届く終端が unknown id となり
    /// `PROTOCOL_VIOLATION` で session が Closing になる)。応答前に stream が終端した
    /// 場合は `close_track_status_on_stream_end` が `Error` を記録するため除去できる。
    pub fn forget_track_status(&mut self, request_id: u64) -> Option<TrackStatusEntry> {
        let entry = self.track_status_requests.get(&request_id)?;
        entry.response.as_ref()?;
        self.request_streams.remove(&request_id);
        self.track_status_requests.remove(&request_id)
    }

    // ─── 送信 API ──────────────────────────────────────────

    /// TRACK_STATUS を送信する (draft §9.13 (TRACK_STATUS)、自側が subscriber)
    pub fn send_track_status(
        &mut self,
        track_namespace: TrackNamespace,
        track_name: Vec<u8>,
        parameters: MessageParameters,
    ) -> Result<u64, SendRequestError> {
        self.require_established()?;
        self.check_peer_goaway()?;
        // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces): single period `.` 予約名前空間への TRACK_STATUS は送信不可
        if track_namespace.is_single_period() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "application cannot use single-period reserved namespace",
            )
            .into());
        }
        // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
        // 送信前に値域を検証し、不正値の送出と状態登録を防ぐ
        if let Some(v) = parameters.include_properties() {
            super::super::subscription::validation::validate_include_properties(v)?;
        }
        // draft §9.20.1 (Parameter Scope): TRACK_STATUS で許可されないパラメータを含む送信は
        // API 呼び出し時に拒否する。検証がないと、TrackStatusEntry の登録・ control deadline
        // の開始・ SendRequest の push まで完了した後に I/O 層のエンコード時 (validate_scope)
        // で非同期に失敗し、アプリがエンコードエラーを無視すると control deadline
        // タイムアウトという後続の誤作動につながる。
        // 検証は request_id 発行より前に置き、エラー時に欠番を作らない
        // (send_subscribe_tracks と同じ設計)。
        if parameters
            .validate_scope(crate::message::TRACK_STATUS_ALLOWED_PARAMS)
            .is_err()
        {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "TRACK_STATUS parameter not allowed in this context",
            )
            .into());
        }
        let request_id = self.request_ids.local_generator.next_id();
        self.track_status_requests.insert(
            request_id,
            TrackStatusEntry {
                request_id,
                my_role: TrackRole::Subscriber,
                track_namespace: track_namespace.clone(),
                track_name: track_name.clone(),
                response: None,
                // 自側は subscriber のため TRACK_STATUS_OK を送らず、INCLUDE_PROPERTIES の保持は不要
                include_properties: None,
            },
        );
        self.request_streams
            .insert(request_id, RequestKind::TrackStatus);
        self.start_control_message_deadline(request_id);
        let msg = ControlMessage::TrackStatus(WireTrackStatus {
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

    /// bidi request stream 終端時の TRACK_STATUS 側の処理
    ///
    /// Pending 状態のまま stream が閉じた場合は `Failed` に遷移。既に Completed /
    /// Failed なら状態維持 (応答確定済み、stream 終端は情報量ゼロ)。
    pub(crate) fn close_track_status_on_stream_end(
        &mut self,
        request_id: u64,
        end: RequestStreamEnd,
    ) -> Result<TerminationReason, SessionError> {
        let Some(entry) = self.track_status_requests.get_mut(&request_id) else {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "bidi request stream close for unknown TRACK_STATUS request id",
            ));
        };
        if entry.response.is_none() {
            entry.response = Some(TrackStatusResponse::Error);
        }
        Ok(terminationreason_from_end(end))
    }

    // ─── 受信ハンドラ ──────────────────────────────────────

    pub(crate) fn handle_peer_track_status(
        &mut self,
        msg: WireTrackStatus,
    ) -> Result<(), SessionError> {
        let request_id = msg.request_id;
        if !self.accept_peer_request(request_id, &msg.parameters)? {
            return Ok(());
        }
        // draft §9.20.3 (AUTHORIZATION TOKEN Parameter): AUTHORIZATION_TOKEN Register/Delete/Use を peer cache に反映
        self.apply_peer_message_auth_tokens(&msg.parameters)?;
        // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces): single period `.` 予約名前空間の TRACK_STATUS は拒否
        // 仕様は "MUST NOT be used for any purpose" のため track_name 不問で拒否する
        if msg.track_namespace.is_single_period() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "reserved single-period namespace",
            );
            return Ok(());
        }
        // draft §6.5 (Session-Level Tracks and Namespaces): .session 名前空間のリクエストは
        // Application へ渡さず DOES_NOT_EXIST で拒否する。空トラック名は仕様上存在しないものと
        // 定義され、非空トラック名も未認識のセッションレベルトラックとして拒否する
        // (本ライブラリはセッションレベルトラックを登録しないため、非空トラック名はすべて未認識になる)
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある
        if msg.track_namespace.is_session_level() {
            let reason = if msg.track_name.is_empty() {
                "empty track name in .session namespace"
            } else {
                "session-level track does not exist"
            };
            self.emit_request_error(request_id, REQUEST_DOES_NOT_EXIST, reason);
            return Ok(());
        }
        // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
        // 値域外 (0 / 1 以外) の受信は MUST でセッションを PROTOCOL_VIOLATION により閉じる。
        let include_properties = msg.parameters.include_properties();
        if let Some(v) = include_properties
            && let Err(err) = super::super::subscription::validation::validate_include_properties(v)
        {
            self.fail(err.clone());
            return Err(err);
        }
        self.track_status_requests.insert(
            request_id,
            TrackStatusEntry {
                request_id,
                my_role: TrackRole::Publisher,
                track_namespace: msg.track_namespace,
                track_name: msg.track_name,
                response: None,
                // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
                // peer が TRACK_STATUS で指定した値を保持し、TRACK_STATUS_OK 送信時に参照する
                include_properties,
            },
        );
        self.request_streams
            .insert(request_id, RequestKind::TrackStatus);
        Ok(())
    }

    // ─── REQUEST_OK / REQUEST_ERROR per-kind dispatch (TRACK_STATUS) ──

    pub(crate) fn send_ok_for_track_status(
        &mut self,
        request_id: u64,
        parameters: &mut MessageParameters,
    ) -> Result<(), SessionError> {
        let (track_namespace, track_name) = {
            let entry = self
                .track_status_requests
                .get(&request_id)
                .expect("locate_request guarantees key presence");
            if entry.my_role != TrackRole::Publisher {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "request_ok (track_status) can only be sent by publisher-role",
                ));
            }
            if entry.response.is_some() {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "track_status already responded",
                ));
            }
            (entry.track_namespace.clone(), entry.track_name.clone())
        };
        // draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter): Objects が
        // 公開済みなら TRACK_STATUS_OK に LARGEST_OBJECT を含める。TRACK_STATUS は
        // Subscription を作らないため、対象 Track の publisher 役 subscription 群が
        // 観測した largest を Track 索引から引き当てる。
        // 注: largest は publisher 役 subscription が保持するため、対象 subscription を
        // forget 済みの Track では未知として省略する (Terminated でも保持中なら寄与する)。
        if let Some(location) = self.publisher_track_largest(&track_namespace, &track_name) {
            update_largest_object_in_parameters(parameters, &location);
        }
        // wire に載った最終値 (アプリ指定値を含む) をローカル状態にも反映する
        let largest_location = parameters
            .largest_object()
            .map(|(group_id, object_id)| Location {
                group_id,
                object_id,
            });
        let entry = self
            .track_status_requests
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        entry.response = Some(TrackStatusResponse::Ok { largest_location });
        Ok(())
    }

    pub(crate) fn send_err_for_track_status(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let entry = self
            .track_status_requests
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.response.is_some() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "send_request_error requires unanswered track_status",
            ));
        }
        entry.response = Some(TrackStatusResponse::Error);
        self.clear_control_message_deadline(request_id);
        Ok(())
    }

    pub(crate) fn handle_ok_for_track_status(
        &mut self,
        request_id: u64,
        parameters: &MessageParameters,
    ) -> Result<(), SessionError> {
        let entry = self
            .track_status_requests
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_OK (track_status) received on responder side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        if entry.response.is_some() {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_OK (track_status) on already-responded entry",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft §9.20.18 (LARGEST OBJECT Parameter): TRACK_STATUS への REQUEST_OK は LARGEST_OBJECT を含み得る
        let largest_location = parameters
            .largest_object()
            .map(|(group_id, object_id)| Location {
                group_id,
                object_id,
            });
        entry.response = Some(TrackStatusResponse::Ok { largest_location });
        // application へ REQUEST_OK 受信を通知する
        self.events.push_back(SessionEvent::RequestOkReceived {
            request_id,
            request_kind: RequestKind::TrackStatus,
            parameters: parameters.clone(),
        });
        self.clear_control_message_deadline(request_id);
        Ok(())
    }

    pub(crate) fn handle_err_for_track_status(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let entry = self
            .track_status_requests
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if entry.response.is_some() {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_ERROR (track_status) on already-responded entry",
            );
            self.fail(err.clone());
            return Err(err);
        }
        entry.response = Some(TrackStatusResponse::Error);
        Ok(())
    }
}
