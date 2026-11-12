//! TRACK_STATUS 関連 (送信側と受信側の両方)
//!
//! draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS) に対応する。
//! TRACK_STATUS は "A potential subscriber sends TRACK_STATUS ... to obtain information about
//! the current status of a given track." と定義され、本ライブラリは subscriber 側の送信と
//! 応答受信に加え、publisher 側の受信と応答送信も扱う。
//!
//! §9.12: "The receiver of a TRACK_STATUS message treats it identically as if it had received
//! a SUBSCRIBE message, except it does not create downstream subscription state or send any
//! Objects. If successful, the publisher responds with a TRACK_STATUS_OK with the same
//! parameters and Track Properties it would have set in a SUBSCRIBE_OK. Track Alias is not
//! used. ... The bidi stream is closed with a FIN after TRACK_STATUS_OK or TRACK_STATUS_ERROR
//! are sent." したがって受信側は subscription state も Track Alias も作らず、Objects も送らない。
//! この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use crate::error::{
    SESSION_PROTOCOL_VIOLATION, SESSION_ROLE_VIOLATION, SESSION_UNEXPECTED_MESSAGE,
};
use crate::message::{
    ControlMessage, TrackStatus as WireTrackStatus, common::Location, common::TrackNamespace,
};
use crate::message_parameter::MessageParameters;
use crate::track_properties::TrackProperties;
use alloc::vec::Vec;

use super::core::Session;
use super::subscription::delivery::update_largest_object_in_parameters;
use super::types::terminationreason_from_end;
use super::types::{
    RequestKind, RequestStreamEnd, SendRequestError, SessionError, SessionEvent, TerminationReason,
    TrackRole, TrackStatusEntry, TrackStatusResponse,
};

/// TRACK_STATUS entry を破棄できるか
///
/// 応答済み (`response` が `Some`) か、送信方向が閉じた (`terminated`) entry は破棄できる。
/// [`Session::forget_track_status`] の受理条件であり、
/// [`Session::goaway_drain_snapshot`](crate::session::core::Session::goaway_drain_snapshot) の
/// blocker 判定も「破棄できない entry だけを blocker にする」という同じ条件を使う
/// (条件を 2 箇所に手書きすると片方だけ更新されて乖離するため、本関数に集約する)。
pub(super) fn is_track_status_discardable(entry: &TrackStatusEntry) -> bool {
    entry.response.is_some() || entry.terminated
}

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
    /// draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS): TRACK_STATUS_OK / REQUEST_ERROR は
    /// FIN で送られる。bidi stream 終端を `recv_request_stream_closed` で通知してから
    /// 呼ぶこと (終端前に除去すると、後から届く終端が unknown id となり
    /// `PROTOCOL_VIOLATION` で session が Closing になる)。応答前に stream が終端した
    /// 場合は `close_track_status_on_stream_end` が requester 側で `Error` を記録し、
    /// responder 側では `terminated` を立てるため、どちらも除去できる。request stream 上の
    /// GOAWAY の deadline 満了で終端した場合も responder 側で `terminated` を立てる
    /// (GOAWAY を送れるのは request の受信側、すなわち TRACK_STATUS では publisher であり、
    /// その送信方向を reset 済みで応答を送れないため `Error` は合成しない)。
    /// 受理条件は内部の `is_track_status_discardable` に集約する。
    pub fn forget_track_status(&mut self, request_id: u64) -> Option<TrackStatusEntry> {
        let entry = self.track_status_requests.get(&request_id)?;
        if !is_track_status_discardable(entry) {
            return None;
        }
        self.request_streams.remove(&request_id);
        self.peer_fin_received.remove(&request_id);
        self.local_fin_sent.remove(&request_id);
        self.clear_request_stream_goaway_deadline(request_id);
        self.track_status_requests.remove(&request_id)
    }

    // ─── 送信 API ──────────────────────────────────────────

    /// TRACK_STATUS を送信する (draft §9.12 (TRACK_STATUS)、自側が subscriber)
    pub fn send_track_status(
        &mut self,
        track_namespace: TrackNamespace,
        track_name: Vec<u8>,
        parameters: MessageParameters,
    ) -> Result<u64, SendRequestError> {
        self.require_established()?;
        self.check_peer_goaway()?;
        // draft-ietf-moq-transport-23 §2.4.1 (Track Naming): 空の Track Name は送出しない
        Self::validate_outgoing_track_name(&track_name)?;
        // draft-ietf-moq-transport-23 §2.4.3 (Reserved Namespaces): single period `.` 予約名前空間への TRACK_STATUS は送信不可
        if track_namespace.is_single_period() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "application cannot use single-period reserved namespace",
            )
            .into());
        }
        // draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter):
        // 送信前に値域を検証し、不正値の送出と状態登録を防ぐ
        if let Some(v) = parameters.include_properties() {
            super::subscription::validation::validate_include_properties(v)?;
        }
        // draft §9.19.1 (Parameter Scope): TRACK_STATUS で許可されないパラメータを含む送信は
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
    /// draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS): "The receiver of a TRACK_STATUS
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
            crate::message::TRACK_STATUS_ALLOWED_PARAMS,
            "TRACK_STATUS parameter not allowed in this context",
        )? {
            return Ok(());
        }
        // draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter):
        // 値域外 (0 / 1 以外) の受信は MUST でセッションを INVALID_PARAMETER (0x305) により
        // 閉じる (draft-ietf-moq-transport-23 §12.2 (Session Termination Codes))。
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
    /// draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS): "If successful, the publisher
    /// responds with a TRACK_STATUS_OK with the same parameters and Track Properties it would
    /// have set in a SUBSCRIBE_OK." 送信する Track Properties はアプリが渡した値であり、
    /// 本関数は `INCLUDE_PROPERTIES=0` のときの空化と応答済み記録を行う。
    ///
    /// draft-ietf-moq-transport-23 §9.19.17 (LARGEST OBJECT Parameter): 送信端点がその
    /// Track で Object を publish / 受信していれば、TRACK_STATUS_OK に LARGEST_OBJECT を
    /// 載せることを MUST とする。SUBSCRIBE_OK / PUBLISH / subscription の REQUEST_UPDATE_OK /
    /// PUBLISH_STATE_NOTIFY と同じく、[`Session::publisher_track_largest`] が返す Track 最大を
    /// [`update_largest_object_in_parameters`] で自動補完する。同関数はアプリ指定値との
    /// 大きい方を採るため、アプリが明示した値を下げない。Object 未 publish の Track では
    /// 補完しない (アプリ指定値もそのまま残す)。
    ///
    /// 最終的に載った LARGEST_OBJECT は [`TrackStatusResponse::Ok::largest_location`] に
    /// 記録する。送信した応答でも「LARGEST_OBJECT parameter があれば保持する」という
    /// 同 enum の doc に合わせた挙動である。
    ///
    /// draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter): "If
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
        parameters: &mut MessageParameters,
        track_properties: TrackProperties,
    ) -> Result<TrackProperties, SessionError> {
        // 検証とフィールドの取り出しは不変借用で行う。`publisher_track_largest` も `self` を
        // 不変借用するため、entry の可変借用を先に取ると両立しない。
        let (track_namespace, track_name, include_properties) = {
            let Some(entry) = self.track_status_requests.get(&request_id) else {
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
            (
                entry.track_namespace.clone(),
                entry.track_name.clone(),
                entry.include_properties,
            )
        };
        // draft-ietf-moq-transport-23 §9.19.17 (LARGEST OBJECT Parameter): publisher 役
        // subscription をすべて forget した後も Track 単位の送信実績から補完できる
        if let Some(largest) = self.publisher_track_largest(&track_namespace, &track_name) {
            update_largest_object_in_parameters(parameters, &largest);
        }
        // 最終的に載った値を応答の記録に残す (アプリ指定値と自動補完値の大きい方)
        let largest_location = parameters
            .largest_object()
            .map(|(group_id, object_id)| Location {
                group_id,
                object_id,
            });
        let entry = self
            .track_status_requests
            .get_mut(&request_id)
            .expect("track_status presence checked above");
        entry.response = Some(TrackStatusResponse::Ok {
            largest_location,
            // EXPIRES は自側が送る応答では保持しない (送信側の entry だけが peer から
            // 受信した値を保持する)
            expires_ms: None,
        });
        self.clear_control_message_deadline(request_id);
        Ok(if include_properties == Some(0) {
            TrackProperties::new()
        } else {
            track_properties
        })
    }

    /// 受信した TRACK_STATUS への REQUEST_ERROR 送信時の状態遷移
    ///
    /// draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS): "A publisher responds to a failed
    /// TRACK_STATUS with an appropriate TRACK_STATUS_ERROR message. The bidi stream is closed
    /// with a FIN after TRACK_STATUS_OK or TRACK_STATUS_ERROR are sent." 自側が responder の entry を
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
    /// 自側が responder のときは `entry.response` を変更しない。draft-ietf-moq-transport-23
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
        // draft-ietf-moq-transport-23 §6.4.2.3 (Request Cancellation and Rejection): "Once a
        // request stream has been opened, the request MAY be cancelled by either endpoint."
        // 自側の送信方向が閉じたため、以後の応答は送れない。
        entry.terminated = true;
        if entry.my_role == TrackRole::Subscriber && entry.response.is_none() {
            entry.response = Some(TrackStatusResponse::Error);
        }
        Ok(terminationreason_from_end(end))
    }

    /// 受信した REQUEST_OK (TRACK_STATUS_OK) を requester 側の entry に反映する
    ///
    /// draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS): TRACK_STATUS_OK を送るのは publisher
    /// (responder) であり、自側は subscriber (requester) である。
    ///
    /// draft-ietf-moq-transport-23 §9.3 (REQUEST_OK): TRACK_STATUS_OK は EXPIRES と
    /// LARGEST_OBJECT を運べる。EXPIRES は同じ Track に対して SUBSCRIBE_OK を返すなら
    /// 提示した値であり (§9.19.16 (EXPIRES Parameter))、TRACK_STATUS 要求自体の期限では
    /// ないため、応答の成功確定時は値だけを保持し、期限タイマーは起動しない。
    ///
    /// draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties): 0x4000-0x7FFF
    /// の Mandatory Track Property のうち意味を理解しない型を含む TRACK_STATUS_OK を受信した
    /// endpoint は、その Track を処理してはならない (MUST)。本ライブラリはリレーではないため
    /// 同節がリレーに求める downstream への TRACK_STATUS_ERROR 伝播は行わず、応答を成功として
    /// 確定せずに `TrackStatusResponse::Unsupported` を記録し、
    /// `SessionEvent::TrackStatusUnsupported` で未対応の型をアプリへ通知する。
    ///
    /// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1 (Malformed Tracks)
    /// 条件 8 / 9: この実装が意味を理解する Property をスコープ外で受信したら Track を malformed
    /// とする (MUST)。TRACK_STATUS_OK の Track Properties は検証対象であり、違反時は応答を
    /// 成功として確定せず、同じ Track の subscription / FETCH を cancel してアプリへ
    /// `MalformedTrack` を通知する (既存の malformed Track 経路)。draft が未知 Mandatory
    /// Track Property と malformed Track の優先順位を定めていないため、§3.7 の判定を先に行う。
    ///
    /// ローカル終端済み (`rejected_request_ids` に記録済み) の entry は遅延応答を吸収し、
    /// 状態遷移とイベント発行を行わない。この実装が TRACK_STATUS の requester entry を
    /// ローカル終端するのは、同じ Track の Malformed Track 検出が sibling として cancel した
    /// 場合だけである。自側が request stream の送信方向を reset しても peer の送信方向は開いた
    /// ままなので (§6.4.2.2 (Graceful Request Stream Closure))、peer が reset を観測する前に
    /// 送った TRACK_STATUS_OK は正当に届く。publisher 側の entry に REQUEST_OK が届くのは role
    /// 違反であり、下の role 検証が状態判定より先に拒否する。
    pub(crate) fn handle_ok_for_track_status(
        &mut self,
        request_id: u64,
        parameters: &MessageParameters,
        track_properties: &TrackProperties,
    ) -> Result<(), SessionError> {
        // 借用の都合で可変借用より前に判定する
        let locally_terminated = self.is_locally_terminated_request(request_id);
        let entry = self
            .track_status_requests
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        // REQUEST_OK を送るのは TRACK_STATUS を受けた側 (responder) であり、自側が
        // responder の entry に REQUEST_OK が届くのは peer の違反である (§9.12)
        if entry.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_ROLE_VIOLATION,
                "REQUEST_OK (track_status) received on responder side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // ローカル終端済みの entry は遅延して届いた正当な応答を吸収する。応答は既に
        // `Error` として確定しており、未知 Mandatory Track Property の判定も行わない
        // (利用しないと決めた Track の未対応型を改めて通知する意味がない)。
        if locally_terminated {
            return Ok(());
        }
        if entry.response.is_some() {
            let err = SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "REQUEST_OK (track_status) on already-responded entry",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties): 未知の
        // Mandatory Track Property を含む TRACK_STATUS_OK は成功として確定しない。応答自体は
        // 受領済みなので「応答待ち」に戻さず、未対応結果として記録してアプリへ型を通知する。
        // 既に届いた応答に対して REQUEST_ERROR を返すことはしない (peer は応答を送り終えて
        // おり、返すと応答の二重送信になる)。
        let unsupported_types = track_properties.unknown_mandatory_types();
        if !unsupported_types.is_empty() {
            entry.response = Some(TrackStatusResponse::Unsupported);
            self.events.push_back(SessionEvent::TrackStatusUnsupported {
                request_id,
                unsupported_property_types: unsupported_types,
            });
            // 応答を受領したため CONTROL_MESSAGE_TIMEOUT の deadline を解除する。FIN の交換と
            // `forget_track_status` の受理条件は通常の TRACK_STATUS_OK と同じである
            // (`is_track_status_discardable` が `response` の有無だけを見る)。
            self.clear_control_message_deadline(request_id);
            return Ok(());
        }
        // draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1
        // (Malformed Tracks) 条件 8 / 9: Track Properties のスコープと多重可否を検証する。
        // 違反時は TrackStatusResponse::Ok を記録せず、malformed Track の終端経路へ渡す
        // (この request 自身は応答済みの失敗として確定し、アプリへ理由を届ける)。
        if let Err(err) = track_properties.validate() {
            self.terminate_malformed_track(request_id, None, err.reason());
            return Ok(());
        }
        // draft §9.19.17 (LARGEST OBJECT Parameter): TRACK_STATUS への REQUEST_OK は LARGEST_OBJECT を含み得る
        let largest_location = parameters
            .largest_object()
            .map(|(group_id, object_id)| Location {
                group_id,
                object_id,
            });
        // draft-ietf-moq-transport-23 §9.19.16 (EXPIRES Parameter): TRACK_STATUS_OK の
        // EXPIRES は、同じ Track に対して SUBSCRIBE_OK を返すなら提示した値である。
        // TRACK_STATUS は 1 回の応答で完結する単発要求であるため、この値で期限タイマーを
        // 起動せず、要求を再び生かすこともしない。利用側が仮の購読期限として参照できる
        // ように値だけを保持する (FIN と破棄の条件は `response` の有無だけで決まる)。
        let expires_ms = parameters.expires();
        entry.response = Some(TrackStatusResponse::Ok {
            largest_location,
            expires_ms,
        });
        // application へ REQUEST_OK 受信を通知する
        self.events.push_back(SessionEvent::RequestOkReceived {
            request_id,
            request_kind: RequestKind::TrackStatus,
            parameters: parameters.clone(),
        });
        self.clear_control_message_deadline(request_id);
        Ok(())
    }

    /// 受信した REQUEST_ERROR を requester 側の entry に反映する
    ///
    /// draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS): REQUEST_ERROR を送るのは publisher
    /// (responder) であり、自側は subscriber (requester) である。ローカル終端済みの entry を
    /// 吸収する経路を持たない理由は
    /// [`Session::handle_ok_for_track_status`] の doc と同じである。
    pub(crate) fn handle_err_for_track_status(
        &mut self,
        request_id: u64,
    ) -> Result<(), SessionError> {
        let entry = self
            .track_status_requests
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        // REQUEST_ERROR を送るのは TRACK_STATUS を受けた側 (responder) であり、自側が
        // responder の entry に REQUEST_ERROR が届くのは peer の違反である (§9.12)
        if entry.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_ROLE_VIOLATION,
                "REQUEST_ERROR (track_status) received on responder side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        if entry.response.is_some() {
            let err = SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "REQUEST_ERROR (track_status) on already-responded entry",
            );
            self.fail(err.clone());
            return Err(err);
        }
        entry.response = Some(TrackStatusResponse::Error);
        Ok(())
    }
}
