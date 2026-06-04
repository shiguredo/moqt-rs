//! TRACK_STATUS 関連 (自側 subscriber の送信側のみ)
//!
//! draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS) に対応する。
//! TRACK_STATUS は "A potential subscriber sends TRACK_STATUS ... to obtain information about
//! the current status of a given track." と定義され、本ライブラリは subscriber 側の送信と
//! 応答受信のみを扱う。publisher 側の受信処理は実装しない (peer から TRACK_STATUS を
//! 受信した場合は未対応 request として `SESSION_PROTOCOL_VIOLATION` になる)。

use crate::error::SESSION_PROTOCOL_VIOLATION;
use crate::message::{
    ControlMessage, TrackStatus as WireTrackStatus, common::Location, common::TrackNamespace,
};
use crate::message_parameter::MessageParameters;
use alloc::vec::Vec;

use super::core::Session;
use super::types::terminationreason_from_end;
use super::types::{
    RequestKind, RequestStreamEnd, SendRequestError, SessionError, SessionEvent, TerminationReason,
    TrackStatusEntry, TrackStatusResponse,
};

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
        self.clear_request_stream_goaway_deadline(request_id);
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
            super::subscription::validation::validate_include_properties(v)?;
        }
        // draft §9.20.1 (Parameter Scope): TRACK_STATUS で許可されないパラメータを含む送信は
        // API 呼び出し時に拒否する。検証がないと、TrackStatusEntry の登録・ control deadline
        // の開始・ SendRequest の push まで完了した後に I/O 層のエンコード時 (validate_scope)
        // で非同期に失敗し、アプリがエンコードエラーを無視すると control deadline
        // タイムアウトという後続の誤作動につながる。
        // 検証は request_id 発行より前に置き、エラー時に欠番を作らない。
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
                track_namespace: track_namespace.clone(),
                track_name: track_name.clone(),
                response: None,
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

    pub(crate) fn handle_ok_for_track_status(
        &mut self,
        request_id: u64,
        parameters: &MessageParameters,
    ) -> Result<(), SessionError> {
        let entry = self
            .track_status_requests
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
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
