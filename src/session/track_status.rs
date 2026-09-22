//! TRACK_STATUS 関連 (送信側と受信側の両方)
//!
//! draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS) に対応する。
//! TRACK_STATUS は "A potential subscriber sends TRACK_STATUS ... to obtain information about
//! the current status of a given track." と定義され、本ライブラリは subscriber 側の送信と
//! 応答受信に加え、publisher 側の受信と応答送信も扱う。
//!
//! §9.13: "The receiver of a TRACK_STATUS message treats it identically as if it had received
//! a SUBSCRIBE message, except it does not create downstream subscription state or send any
//! Objects. If successful, the publisher responds with a TRACK_STATUS_OK with the same
//! parameters and Track Properties it would have set in a SUBSCRIBE_OK. Track Alias is not
//! used. ... The bidi stream is closed with a FIN after TRACK_STATUS_OK or REQUEST_ERROR are
//! sent." したがって受信側は subscription state も Track Alias も作らず、Objects も送らない。
//! この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use crate::error::SESSION_PROTOCOL_VIOLATION;
use crate::message::{
    ControlMessage, TrackStatus as WireTrackStatus, common::Location, common::TrackNamespace,
};
use crate::message_parameter::MessageParameters;
use crate::track_properties::TrackProperties;
use alloc::vec::Vec;

use super::core::Session;
use super::types::terminationreason_from_end;
use super::types::{
    RequestKind, RequestStreamEnd, SendRequestError, SessionError, SessionEvent, TerminationReason,
    TrackRole, TrackStatusEntry, TrackStatusResponse,
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

    /// 応答済み (Ok / Error) または終端済みの TRACK_STATUS を除去する
    ///
    /// draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS): TRACK_STATUS_OK / REQUEST_ERROR は
    /// FIN で送られる。bidi stream 終端を `recv_request_stream_closed` で通知してから
    /// 呼ぶこと (終端前に除去すると、後から届く終端が unknown id となり
    /// `PROTOCOL_VIOLATION` で session が Closing になる)。応答前に stream が終端した
    /// 場合は `close_track_status_on_stream_end` が requester 側で `Error` を記録し、
    /// responder 側では `terminated` を立てるため、どちらも除去できる。
    pub fn forget_track_status(&mut self, request_id: u64) -> Option<TrackStatusEntry> {
        let entry = self.track_status_requests.get(&request_id)?;
        if entry.response.is_none() && !entry.terminated {
            return None;
        }
        self.request_streams.remove(&request_id);
        self.peer_fin_received.remove(&request_id);
        self.local_fin_sent.remove(&request_id);
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
                my_role: TrackRole::Subscriber,
                track_namespace: track_namespace.clone(),
                track_name: track_name.clone(),
                // 自側が送る要求では INCLUDE_PROPERTIES を保持しない (要求側の値であり
                // 応答の Track Properties を空にする判断には使わない)
                include_properties: None,
                response: None,
                terminated: false,
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

    // ─── 受信ハンドラ ──────────────────────────────────────

    /// peer から TRACK_STATUS を受信する (自側が publisher)
    ///
    /// draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS): "The receiver of a TRACK_STATUS
    /// message treats it identically as if it had received a SUBSCRIBE message, except it
    /// does not create downstream subscription state or send any Objects." 受信前の検証は
    /// SUBSCRIBE と共通の `Session::accept_incoming_track_request` を使い、`Track Alias is
    /// not used.` のとおり Track Alias も subscription state も作らない。本関数は
    /// `INCLUDE_PROPERTIES` の値域検証と `track_status_requests` / `request_streams` への
    /// 登録を行う。応答の送信はアプリが `Session::send_request_ok` (TRACK_STATUS_OK) /
    /// `Session::send_request_error` (REQUEST_ERROR) を呼ぶ。
    /// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub(crate) fn handle_peer_track_status(
        &mut self,
        track_status: WireTrackStatus,
    ) -> Result<(), SessionError> {
        let request_id = track_status.request_id;
        if !self.accept_incoming_track_request(
            request_id,
            &track_status.track_namespace,
            &track_status.track_name,
            &track_status.parameters,
        )? {
            return Ok(());
        }
        // draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope): TRACK_STATUS で許可される
        // パラメータは AUTHORIZATION_TOKEN と INCLUDE_PROPERTIES のみ。wire 経路では decode
        // 層が検証済みだが、API 経路 (`recv_request`) で手組みされたメッセージは素通りする
        // ため、受信側の状態整合性のためここでも検証する。
        if track_status
            .parameters
            .validate_scope(crate::message::TRACK_STATUS_ALLOWED_PARAMS)
            .is_err()
        {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "TRACK_STATUS parameter not allowed in this context",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
        // 値域外 (0 / 1 以外) の受信は MUST でセッションを PROTOCOL_VIOLATION により閉じる
        let include_properties = track_status.parameters.include_properties();
        if let Some(v) = include_properties
            && let Err(err) = super::subscription::validation::validate_include_properties(v)
        {
            self.fail(err.clone());
            return Err(err);
        }
        self.track_status_requests.insert(
            request_id,
            TrackStatusEntry {
                request_id,
                // 自側が publisher (responder) として応答する側である
                my_role: TrackRole::Publisher,
                track_namespace: track_status.track_namespace,
                track_name: track_status.track_name,
                include_properties,
                response: None,
                terminated: false,
            },
        );
        self.request_streams
            .insert(request_id, RequestKind::TrackStatus);
        Ok(())
    }

    /// TRACK_STATUS_OK 送信時の状態遷移と Track Properties の解決
    ///
    /// draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS): "If successful, the publisher
    /// responds with a TRACK_STATUS_OK with the same parameters and Track Properties it would
    /// have set in a SUBSCRIBE_OK." 送信する Track Properties はアプリが渡した値であり、
    /// 本関数は `INCLUDE_PROPERTIES=0` のときの空化と応答済み記録だけを行う。
    ///
    /// draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter): "If
    /// INCLUDE_PROPERTIES is 0, the Track Properties are still present in the message, but
    /// they SHOULD be empty." 空化は `Session::send_subscribe_ok` と同じく entry に保持した
    /// 値で判断する。
    ///
    /// 応答は 1 回だけであり、応答済みの entry への再送は `SESSION_PROTOCOL_VIOLATION` を
    /// 返す。呼び出し元 (`Session::send_request_ok`) が `fin: true` で bidi stream を閉じる。
    /// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub(crate) fn send_ok_for_track_status(
        &mut self,
        request_id: u64,
        track_properties: TrackProperties,
    ) -> Result<TrackProperties, SessionError> {
        let Some(entry) = self.track_status_requests.get_mut(&request_id) else {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "track_status not found for send_ok_for_track_status",
            ));
        };
        if entry.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "TRACK_STATUS_OK can only be sent by the responder side",
            ));
        }
        if entry.terminated {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "TRACK_STATUS_OK on a request whose stream is already closed",
            ));
        }
        if entry.response.is_some() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "TRACK_STATUS_OK on already-responded entry",
            ));
        }
        entry.response = Some(TrackStatusResponse::Ok {
            // 自側が送る応答では Largest Location を保持しない (送信側の entry だけが
            // peer から受信した値を保持する)
            largest_location: None,
        });
        // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter): 空化の判断は
        // entry の借用を解放してから行う
        let include_properties = entry.include_properties;
        self.clear_control_message_deadline(request_id);
        Ok(if include_properties == Some(0) {
            TrackProperties::new()
        } else {
            track_properties
        })
    }

    /// 受信した TRACK_STATUS への REQUEST_ERROR 送信時の状態遷移
    ///
    /// draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS): "A publisher responds to a failed
    /// TRACK_STATUS with an appropriate REQUEST_ERROR message. The bidi stream is closed with
    /// a FIN after TRACK_STATUS_OK or REQUEST_ERROR are sent." 自側が responder の entry を
    /// 応答済み (`Error`) として確定し、呼び出し元 (`Session::send_request_error`) が
    /// `fin: true` で REQUEST_ERROR を送る。
    ///
    /// 応答は 1 回だけであり、応答済み・終端済みの entry への再送は
    /// `SESSION_PROTOCOL_VIOLATION` を返す。
    /// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub(crate) fn send_err_for_track_status(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let Some(entry) = self.track_status_requests.get_mut(&request_id) else {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "track_status not found for send_err_for_track_status",
            ));
        };
        if entry.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_ERROR (track_status) can only be sent by the responder side",
            ));
        }
        if entry.terminated {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_ERROR on a track_status whose stream is already closed",
            ));
        }
        if entry.response.is_some() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_ERROR on already-responded track_status",
            ));
        }
        entry.response = Some(TrackStatusResponse::Error);
        self.clear_control_message_deadline(request_id);
        Ok(())
    }

    /// bidi request stream 終端時の TRACK_STATUS 側の処理
    ///
    /// 自側が requester (TRACK_STATUS を送った側) のときは、応答前に stream が閉じた
    /// 要求を `Error` として確定する。既に `Ok` / `Error` なら状態維持 (応答確定済みで
    /// stream 終端は情報量ゼロ)。
    ///
    /// 自側が responder のときは `entry.response` を変更しない。draft-ietf-moq-transport-21
    /// §6.4.2.2 (Graceful Request Stream Closure) の FIN は方向ごとの終端であり cancel では
    /// ないため、requester の FIN を受けても TRACK_STATUS_OK / REQUEST_ERROR の送出経路を
    /// 塞がない (呼び出し元が peer FIN の受信を記録して終端を遅延させる)。
    /// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
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
        // draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection): "Once a
        // request stream has been opened, the request MAY be cancelled by either endpoint."
        // 自側の送信方向が閉じたため、以後の応答は送れない。
        entry.terminated = true;
        if entry.my_role == TrackRole::Subscriber && entry.response.is_none() {
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
        // REQUEST_OK を送るのは TRACK_STATUS を受けた側 (responder) であり、自側が
        // responder の entry に REQUEST_OK が届くのは peer の違反である (§9.13)
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
        // REQUEST_ERROR を送るのは TRACK_STATUS を受けた側 (responder) であり、自側が
        // responder の entry に REQUEST_ERROR が届くのは peer の違反である (§9.13)
        if entry.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_ERROR (track_status) received on responder side",
            );
            self.fail(err.clone());
            return Err(err);
        }
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
