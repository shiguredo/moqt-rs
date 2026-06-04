//! FETCH 関連
//!
//! draft-ietf-moq-transport-21 §3.2.1 (Fetch State Management) / §9.11 (FETCH) / §9.12 (FETCH_OK) に対応する
//! `impl Session` の送受信メソッドをまとめる。将来 draft 側で変更される可能性がある。

use crate::error::{REQUEST_DOES_NOT_EXIST, REQUEST_INVALID_RANGE, SESSION_PROTOCOL_VIOLATION};
use crate::message::{
    ControlMessage, FETCH_ALLOWED_PARAMS, FETCH_OK_ALLOWED_PARAMS, Fetch as WireFetch,
    FetchOk as WireFetchOk,
    common::{Location, TrackNamespace},
};
use crate::message_parameter::{LocationFilterContext, LocationFilterUpdate, MessageParameters};
use crate::track_properties::TrackProperties;
use alloc::vec::Vec;

use super::core::Session;
use super::data::IncomingDataStream;
use super::namespace::terminationreason_from_end;
use super::types::{
    DataStreamId, Fetch, FetchState, RequestKind, RequestStreamEnd, SendRequestError, SessionError,
    SessionEvent, TerminationReason, TrackRole,
};

/// fetch が破棄可能な状態条件を満たしているか
///
/// subscriber 側は `Terminated`、publisher 側は `Terminated` またはデータストリーム
/// 終端済み (`send_fetch_data_stream_closed` 呼び出し済み。`Established` のままでも破棄可能。
/// draft-ietf-moq-transport-21 §3.2.1 の "It can remove all FETCH state after closing the
/// data stream with a FIN." に基づく)。
fn fetch_finished(fetch: &Fetch) -> bool {
    match fetch.my_role {
        TrackRole::Publisher => fetch.state == FetchState::Terminated || fetch.data_stream_finished,
        TrackRole::Subscriber => fetch.state == FetchState::Terminated,
    }
}

/// subscriber 側 fetch 終端通知の呼び出し元種別 (エラーメッセージの出し分け用)
///
/// 文字列分岐ではリネームで静かに壊れるため enum で表現する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubscriberFetchCloseKind {
    DataStreamFin,
    DataStreamReset,
    StopSending,
}

impl Session {
    // ─── クエリ API ─────────────────────────────────────────

    /// 指定 Request ID の Fetch を参照する
    pub fn fetch(&self, request_id: u64) -> Option<&Fetch> {
        self.fetches.get(&request_id)
    }

    /// すべての Fetch をイテレートする
    pub fn fetches(&self) -> impl Iterator<Item = &Fetch> {
        self.fetches.values()
    }

    /// 指定 Request ID の fetch が cleanup 可能か返す
    ///
    /// 破棄可能な状態条件 (subscriber 側は `Terminated`、publisher 側は `Terminated`
    /// またはデータストリーム終端済み) を満たし、かつ open 中の受信データストリームが
    /// 無ければ `Some(true)` を返す。
    /// draft-ietf-moq-transport-21 §9.11 (FETCH) により、データストリームの
    /// FIN 後に FETCH_OK / REQUEST_ERROR が届くことがあるため、応答未受領の
    /// `Terminated` fetch も存在しうる。subscriber 側の fetch では `response_received`
    /// (subscriber 側専用のフィールド) を確認してから `forget_fetch` を呼ぶこと。
    /// ただし応答は bidi request stream 上で送られ、QUIC の in-order 保証により
    /// 同ストリームの FIN より先に届くため、bidi request stream の終端
    /// (`recv_request_stream_closed` / `RequestTerminated`) を確認してから破棄する場合は
    /// 確認不要。publisher 側は応答送信を API 呼び出しで行うため、この確認は不要。
    ///
    /// 判定は incoming データストリームのみを対象とし、publisher 側の open 中の
    /// outgoing FETCH stream があっても `Some(true)` を返す。`forget_fetch` は open 中の
    /// outgoing FETCH stream エントリもまとめて除去する安全網を兼ねるため、publisher 側の
    /// fetch は先に open ストリームの終端通知 (`send_fetch_data_stream_closed`) を済ませる
    /// こと。bidi request stream 終端 (キャンセル) 経路でデータストリームの送信がまだ
    /// 終わっていない場合に `forget_fetch` すると送信ストリームの追跡が失われ、
    /// 以後の `send_fetch_object` / `send_fetch_data_stream_closed` は
    /// `SESSION_PROTOCOL_VIOLATION` で fail する (エラー文言はそれぞれ
    /// "outgoing fetch object sent before fetch header" / "outgoing fetch stream close received
    /// for unknown stream id")。終端通知を呼ばずに `forget_fetch` した場合も同様である。
    ///
    /// publisher 側の終端済み fetch の破棄経路は draft-ietf-moq-transport-21 §3.2.1
    /// "It can remove all FETCH state after closing the data stream with a FIN." に基づく。
    /// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn fetch_cleanup_ready(&self, request_id: u64) -> Option<bool> {
        let entry = self.fetches.get(&request_id)?;
        if !fetch_finished(entry) {
            return Some(false);
        }
        let has_open_data_stream = self.data_streams.incoming.values().any(|stream| {
            matches!(
                stream,
                IncomingDataStream::Fetch {
                    request_id: stream_request_id,
                    ..
                } if *stream_request_id == request_id
            )
        });
        Some(!has_open_data_stream)
    }

    /// Fetch をセッションから除去する
    ///
    /// 破棄可能な状態条件は [`fetch_cleanup_ready`](Self::fetch_cleanup_ready) と同じ
    /// (subscriber 側は `Terminated`、publisher 側は `Terminated` または
    /// データストリーム終端済み)。`fetch_cleanup_ready` は open 中の受信データストリームが
    /// 無いことも要求するが、本関数は open 中の受信データストリームがあっても受理し、
    /// 掃除を兼ねる (下記)。draft-ietf-moq-transport-21 §9.11 (FETCH)
    /// により、データストリームの FIN 後に FETCH_OK / REQUEST_ERROR が届くことがあるため、
    /// 応答未受領の `Terminated` fetch も存在しうる。subscriber 側の fetch では応答受領
    /// (`response_received`) を確認してから破棄すること (bidi request stream の終端を
    /// 確認してから破棄する場合は QUIC の in-order 保証により応答が先に届いているため
    /// 確認不要)。エントリ不在時は `None` を返す。
    ///
    /// 掃除を兼ねる: 該当 request の open 中の outgoing FETCH stream エントリも除去する
    /// (安全網。subgroup 側の `remove_outgoing_data_plane_for_request` に相当する
    /// fetch 用の掃除。アプリが `forget_fetch` を呼んだ場合にのみ実行される)。
    /// アプリは先に終端通知 API (`send_fetch_data_stream_closed`) を呼ぶのが正規の流れで、
    /// ここで除去されるのは終端通知漏れの残骸のみ。
    ///
    /// publisher 側・データストリーム終端済みの fetch を破棄する場合は、peer が bidi
    /// request stream を閉じる前でも破棄できる。破棄後に届く bidi request stream close は
    /// `rejected_request_ids` で no-op で吸収する。REQUEST_UPDATE は unknown request id
    /// として PROTOCOL_VIOLATION で fail する (REQUEST_ERROR / REQUEST_OK も同様)
    /// (draft §9.5 の "An endpoint that receives a REQUEST_UPDATE other than in the two
    /// cases above MUST close the session with a PROTOCOL_VIOLATION." に従う。
    /// 破棄済み request への REQUEST_UPDATE は「sender が同じ bidi stream に後から
    /// REQUEST_UPDATE を送れる」規定の対象外と解釈する)。
    /// なお control message deadline は publisher 側では開始されない (subscriber 側の
    /// 送信 API のみが開始する) ため、この破棄経路では deadline の考慮は不要。
    pub fn forget_fetch(&mut self, request_id: u64) -> Option<Fetch> {
        let entry = self.fetches.get(&request_id)?;
        if !fetch_finished(entry) {
            return None;
        }
        let publisher_finished =
            entry.my_role == TrackRole::Publisher && entry.data_stream_finished;
        if publisher_finished {
            // 破棄後に届く bidi request stream close を no-op で吸収する
            // (アプリは peer が bidi を閉じる前に破棄できるため。エントリは
            // close 通知の受信時に削除される。subscriber 側は既存契約どおり
            // bidi 終端を確認してから破棄するため記録しない)。
            // 既に bidi close 済み (Terminated) の fetch を破棄した場合は close 通知が
            // 再送されないためエントリがセッション寿命まで残るが、u64 1 個分の
            // 残留であり実害はない (二重 close 通知は no-op で吸収される)
            self.rejected_request_ids.insert(request_id);
        }
        self.request_streams.remove(&request_id);
        self.remove_incoming_data_streams_for_request(request_id);
        // outgoing_fetch には timing エントリが紐付かないため、エントリの除去のみでよい
        // (prior_location はエントリ内に保持され、タイムアウト追跡は存在しない)
        self.data_streams
            .outgoing_fetch
            .retain(|&_, stream| stream.request_id != request_id);
        self.remove_request_update_credit_entries(request_id);
        self.fetches.remove(&request_id)
    }

    /// bidi request stream 終端時の FETCH 側の処理
    ///
    /// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection) に従い、bidi request
    /// stream の終端で Fetch の state を `Terminated` に遷移させる。これは
    /// FETCH データ stream (uni) の終端とは別で、
    /// request 自体の打ち切り処理。
    pub(super) fn close_fetch_on_stream_end(
        &mut self,
        request_id: u64,
        end: RequestStreamEnd,
    ) -> Result<TerminationReason, SessionError> {
        let Some(fetch) = self.fetches.get_mut(&request_id) else {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "bidi request stream close for unknown FETCH request id",
            ));
        };
        fetch.state = FetchState::Terminated;
        Ok(terminationreason_from_end(end))
    }

    // ─── 送信 API ──────────────────────────────────────────

    /// FETCH を送信する (draft-ietf-moq-transport-21 §9.11 (FETCH)、自側が subscriber)
    ///
    /// range は LOCATION_FILTER パラメータで指定する (省略時は track 先頭から
    /// Largest Object まで)。この節番号・規則は draft 由来であり将来の draft 改版で
    /// 変わる可能性がある。
    ///
    /// 失敗条件はすべて `SendRequestError::Session` (`SESSION_PROTOCOL_VIOLATION` 起因)
    /// で返し、セッションは閉じない。scope ・空 range の検証のみ request_id 発行より
    /// 前に行い (欠番を作らない)、値域検証は発行後のままである。
    pub fn send_fetch(
        &mut self,
        track_namespace: TrackNamespace,
        track_name: Vec<u8>,
        parameters: MessageParameters,
    ) -> Result<u64, SendRequestError> {
        self.require_established()?;
        self.check_peer_goaway()?;
        // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces): single period `.` 予約名前空間への FETCH は送信不可
        if track_namespace.is_single_period() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "application cannot use single-period reserved namespace",
            )
            .into());
        }
        // draft-ietf-moq-transport-21 §3.3.2 (Range Filters): peer が必ず INVALID_FILTER で
        // 拒否するメッセージを無警告で送出しないよう、送信前に自側で弾く
        self.validate_outgoing_range_filters(&parameters)?;
        // draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope): FETCH で許可されない
        // パラメータを含む送信は API 呼び出し時に拒否する。scope 検証に限っては
        // request_id 発行より前に置き、エラー時に欠番を作らない
        // (値域検証は発行後のまま。SUBSCRIBE 送信と同じ設計判断)。
        if parameters.validate_scope(FETCH_ALLOWED_PARAMS).is_err() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "FETCH parameter not allowed in this context",
            )
            .into());
        }
        // 要求 range の解決と空 range の事前検証。Largest は送信時点では未知のため
        // `None` で解決する (absolute 指定は正確に、相対指定は先頭寄りに解決される)。
        // 空 range (Start > End) の FETCH は送らない (受信側は INVALID_RANGE で
        // 拒否するのが本実装の選択であり、peer も同様と期待する)。
        let (fetch_start, fetch_end) = match parameters.location_filter_update() {
            Ok(LocationFilterUpdate::Set(filter)) => (
                filter.effective_start_location(None),
                filter.effective_end_location(None, LocationFilterContext::Fetch),
            ),
            Ok(_) => (
                Some(Location {
                    group_id: 0,
                    object_id: 0,
                }),
                None,
            ),
            Err(_) => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "invalid fetch filter encoding",
                )
                .into());
            }
        };
        if let (Some(start), Some(end)) = (fetch_start.as_ref(), fetch_end.as_ref())
            && start > end
        {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "fetch location filter range is empty",
            )
            .into());
        }
        let request_id = self.request_ids.local_generator.next_id();
        let fetch = Fetch {
            request_id,
            my_role: TrackRole::Subscriber,
            track_namespace: Some(track_namespace.clone()),
            track_name: Some(track_name.clone()),
            fetch_start,
            state: FetchState::Pending,
            end_location: None,
            end_of_track: false,
            response_received: false,
            data_stream_finished: false,
            subscriber_priority: parameters.subscriber_priority(),
            group_order: parameters.group_order(),
            // 自側は subscriber のため FETCH_OK を送らず、INCLUDE_PROPERTIES の保持は不要
            include_properties: None,
        };
        if let Some(order) = fetch.group_order {
            super::subscription::validation::validate_group_order(order)?;
        }
        // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
        // 送信前に値域を検証し、不正値の送出と状態登録を防ぐ
        if let Some(v) = parameters.include_properties() {
            super::subscription::validation::validate_include_properties(v)?;
        }
        self.fetches.insert(request_id, fetch);
        self.request_streams.insert(request_id, RequestKind::Fetch);
        self.start_control_message_deadline(request_id);
        let msg = ControlMessage::Fetch(WireFetch {
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

    /// FETCH_OK を送信する (draft-ietf-moq-transport-21 §9.12 (FETCH_OK)、publisher 側)
    pub fn send_fetch_ok(
        &mut self,
        request_id: u64,
        end_of_track: u8,
        end_location: Location,
        parameters: MessageParameters,
        track_properties: TrackProperties,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        // 存在・ role ・ state を先に検証する
        {
            let fetch = self.fetches.get(&request_id).ok_or_else(|| {
                SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "fetch not found for send_fetch_ok",
                )
            })?;
            if fetch.my_role != TrackRole::Publisher {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "send_fetch_ok requires publisher role",
                ));
            }
            if fetch.state != FetchState::Pending {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "send_fetch_ok requires Pending state",
                ));
            }
        }
        // draft-ietf-moq-transport-21 §9.12 (FETCH_OK): End Location はアプリが
        // 渡した値をそのまま使う (Joining FETCH 廃止に伴い導出は行わない)。
        // draft §9.20.1 (Parameter Scope): FETCH_OK はパラメータを運べない
        // (FETCH_OK_ALLOWED_PARAMS は空集合。draft-ietf-moq-transport-21 §9.12 (FETCH_OK)
        // の Parameters フィールドに許可されるパラメータは 0 個)。
        // スコープ外パラメータを含む FETCH_OK では状態を一切変更せずエラーを返す。
        // 受信側はワイヤ層 (`FetchOk::decode_message_body` の validate_scope) で検証済みのため、
        // ここは送信側の状態整合性のための検証。検証が状態遷移後に走ると、fetch が
        // `Established` に遷移済みのまま後段のエンコード失敗で peer に FETCH_OK が送られない
        // 孤児状態が残る (エンコードは sans-I/O のため I/O 層で行われる)。
        // なお本検証はパラメータのみを対象とし、`end_of_track > 1` や不正な `track_properties`
        // はエンコード時検証のまま残る (同型の孤児化経路。値域・内容検証は本検証の対象外)。
        if parameters.validate_scope(FETCH_OK_ALLOWED_PARAMS).is_err() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "FETCH_OK parameter not allowed in this context",
            ));
        }
        let fetch = self
            .fetches
            .get_mut(&request_id)
            .expect("fetch existence checked above");
        fetch.state = FetchState::Established;
        fetch.end_of_track = end_of_track != 0;
        fetch.end_location = Some(end_location);
        // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
        // peer が FETCH で INCLUDE_PROPERTIES=0 を指定したとき、FETCH_OK の
        // Track Properties は存在するが空にする (SHOULD)。
        let track_properties = if fetch.include_properties == Some(0) {
            TrackProperties::new()
        } else {
            track_properties
        };
        let msg = ControlMessage::FetchOk(WireFetchOk {
            end_of_track,
            end_location,
            parameters,
            track_properties,
        });
        self.events.push_back(SessionEvent::SendOnStream {
            request_id,
            message: msg,
            fin: false,
        });
        Ok(())
    }

    pub(super) fn send_update_for_fetch(
        &mut self,
        request_id: u64,
        parameters: &MessageParameters,
    ) -> Result<(), SessionError> {
        let fetch = self
            .fetches
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if fetch.my_role != TrackRole::Subscriber {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_update (fetch) can only be sent by subscriber-role",
            ));
        }
        if fetch.state != FetchState::Established {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_update requires Established fetch",
            ));
        }
        // draft-ietf-moq-transport-21 §9.20.8 (SUBSCRIBER PRIORITY Parameter): REQUEST_UPDATE に SUBSCRIBER_PRIORITY が含まれる場合は更新
        if let Some(priority) = parameters.subscriber_priority() {
            fetch.subscriber_priority = Some(priority);
        }
        // draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): GROUP_ORDER は確定後変更不可 (draft-ietf-moq-transport-21 §5.1.1 (Definitions)) のため更新しない
        Ok(())
    }

    // ─── FETCH stream 終端遷移 API ─────────────────────────
    //
    // draft §3.2.1 (Fetch State Management): subscriber 側は FIN / RESET_STREAM 受信、または
    // STOP_SENDING 送信で FETCH state を終える。publisher 側は STOP_SENDING
    // 受信で fetch state を破棄できる。I/O 層が QUIC stream イベントを観測
    // してから本関数を呼ぶ sans-I/O な API。

    /// subscriber 側: FETCH データストリーム (uni) の終端を通知する
    ///
    /// 通常の FETCH 応答 stream 用であり、`request_id` には `Fetch` の Request ID を
    /// 渡す。fill fetch stream (subscription の Request ID を載せる) の終端は
    /// stream id 系 (`recv_data_stream_closed`) で通知すること
    /// (fill 終端は subscription に影響なく吸収される)。
    ///
    /// draft-ietf-moq-transport-21 §3.2.1 (Fetch State Management): subscriber は FETCH_HEADER
    /// stream の FIN 受信、または publisher 側 RESET_STREAM 受信で FETCH state を
    /// 終える。state は `Terminated` に遷移する。
    ///
    /// `RequestStreamEnd::Reset { error_code, .. }` は state 遷移には影響しないが、
    /// 呼び出し側が診断ログに使えるよう取り出しておく。これは FETCH **bidi
    /// request stream** とは別経路で、
    /// draft §6.4.1 (Unidirectional Streams) どおり uni stream の終端は application state と
    /// 独立して処理する。将来 draft が変更される可能性がある。
    pub fn recv_fetch_data_stream_closed(
        &mut self,
        request_id: u64,
        end: RequestStreamEnd,
    ) -> Result<(), SessionError> {
        let kind = match end {
            RequestStreamEnd::Fin => SubscriberFetchCloseKind::DataStreamFin,
            RequestStreamEnd::Reset { .. } => SubscriberFetchCloseKind::DataStreamReset,
        };
        self.terminate_subscriber_fetch(request_id, kind)
    }

    /// subscriber 側: FETCH データストリームに対して STOP_SENDING を送出した
    ///
    /// 通常の FETCH 応答 stream 用であり、`request_id` には `Fetch` の Request ID を
    /// 渡す。fill fetch stream の cancel は stream id 系
    /// (`send_data_stream_stop_sending`) で通知すること。
    ///
    /// draft §3.2.1 (Fetch State Management): subscriber が自ら cancel する経路。
    /// state は `Terminated` に遷移する。I/O 層が QUIC STOP_SENDING を送出
    /// したうえで本関数を呼ぶ (session 層は frame 送出しない)。
    /// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection):
    /// cancel は開いている方向を打ち切る。I/O 層は `ResetRequestStream` を受けて
    /// bidi request stream の送信方向を reset する。
    pub fn send_fetch_stop_sending(&mut self, request_id: u64) -> Result<(), SessionError> {
        self.terminate_subscriber_fetch(request_id, SubscriberFetchCloseKind::StopSending)?;
        // subscriber による cancel のため bidi 送信方向を reset する (§6.4.2.3)。
        // CANCELLED (0x1) はいずれかの端点による cancel に該当する (§12.5)。
        self.events.push_back(SessionEvent::ResetRequestStream {
            request_id,
            error_code: super::types::DataStreamResetReason::Cancelled.error_code(),
        });
        Ok(())
    }

    /// publisher 側: FETCH データストリームで STOP_SENDING を受信した
    ///
    /// draft §3.2.1 (Fetch State Management): publisher は STOP_SENDING 受信で fetch state を
    /// 破棄してよい。state は `Terminated` に遷移する。
    ///
    /// draft §3.2.1: "The Publisher can remove fetch state as soon as it has received a
    /// STOP_SENDING. It MUST reset the bidi request stream and unidirectional data stream
    /// associated with the FETCH." (MUST) のとおり、bidi request stream の reset は
    /// `SessionEvent::ResetRequestStream` で I/O 層に指示する。データストリームの reset は
    /// アプリケーション層が本関数呼び出し後に行い、`send_fetch_data_stream_closed` を
    /// 呼んで終端を通知すること。アプリが `forget_fetch` を呼んだ場合はその安全網が
    /// 残った outgoing FETCH stream エントリを掃除する。終端通知を済ませた fetch は
    /// bidi request stream の終端を待たずに破棄できる
    /// (`forget_fetch` の doc 参照。終端通知漏れの場合は従来どおり破棄前に
    /// bidi request stream の終端 (`RequestTerminated`) を確認すること)。
    /// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn fetch_stop_sending_received(&mut self, request_id: u64) -> Result<(), SessionError> {
        self.require_established()?;
        {
            let fetch = self.fetches.get_mut(&request_id).ok_or_else(|| {
                SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "fetch not found for stream event",
                )
            })?;
            if fetch.my_role != TrackRole::Publisher {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "fetch_stop_sending_received requires publisher role",
                ));
            }
            if fetch.state == FetchState::Terminated {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "fetch_stop_sending_received on terminated fetch",
                ));
            }
            fetch.state = FetchState::Terminated;
        }
        self.remove_incoming_data_streams_for_request(request_id);
        // §3.2.1 MUST の bidi reset を I/O 層に指示する。CANCELLED (0x1) は
        // subscriber による cancel に該当する (§12.5)。
        self.events.push_back(SessionEvent::ResetRequestStream {
            request_id,
            error_code: super::types::DataStreamResetReason::Cancelled.error_code(),
        });
        Ok(())
    }

    fn terminate_subscriber_fetch(
        &mut self,
        request_id: u64,
        kind: SubscriberFetchCloseKind,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        {
            let fetch = self.fetches.get_mut(&request_id).ok_or_else(|| {
                SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "fetch not found for stream event",
                )
            })?;
            if fetch.my_role != TrackRole::Subscriber {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    match kind {
                        SubscriberFetchCloseKind::DataStreamFin => {
                            "recv_fetch_data_stream_closed(Fin) requires subscriber role"
                        }
                        SubscriberFetchCloseKind::DataStreamReset => {
                            "recv_fetch_data_stream_closed(Reset) requires subscriber role"
                        }
                        SubscriberFetchCloseKind::StopSending => {
                            "send_fetch_stop_sending requires subscriber role"
                        }
                    },
                ));
            }
            if fetch.state == FetchState::Terminated {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "fetch stream event on terminated fetch",
                ));
            }
            fetch.state = FetchState::Terminated;
        }
        self.remove_incoming_data_streams_for_request(request_id);
        Ok(())
    }

    // ─── 受信ハンドラ ──────────────────────────────────────

    pub(super) fn handle_peer_fetch(&mut self, fetch: WireFetch) -> Result<(), SessionError> {
        let request_id = fetch.request_id;
        if !self.accept_peer_request(request_id, &fetch.parameters)? {
            return Ok(());
        }
        // draft §9.20.3 (AUTHORIZATION TOKEN Parameter): AUTHORIZATION_TOKEN Register/Delete/Use を peer cache に反映
        self.apply_peer_message_auth_tokens(&fetch.parameters)?;
        // draft-ietf-moq-transport-21 §3.3.2 (Range Filters): MAX_FILTER_RANGES 超過は INVALID_FILTER で拒否
        if !self.check_incoming_range_filters(request_id, &fetch.parameters) {
            return Ok(());
        }
        // draft-ietf-moq-transport-21 §9.11 (FETCH) / §3.3.1 (Location Filters):
        // Fetch range は LOCATION_FILTER パラメータで指定する。省略時と Length 0 は
        // unfiltered (先頭から Largest Object まで) として扱う。
        // 不正エンコーディングはデコード層で検証済みのためここでは到達しないが、
        // 安全側にセッションを閉じる (SUBSCRIBE 受信経路と対称)。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        let filter = match fetch.parameters.location_filter_typed() {
            Ok(filter) => filter,
            Err(_) => {
                let err =
                    SessionError::new(SESSION_PROTOCOL_VIOLATION, "invalid fetch filter encoding");
                self.fail(err.clone());
                return Err(err);
            }
        };
        let track_ns = fetch.track_namespace;
        let track_name = fetch.track_name;
        // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces): single period `.` 予約名前空間の FETCH は拒否
        if track_ns.is_single_period() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "reserved single-period namespace",
            );
            return Ok(());
        }
        // draft §6.5 (Session-Level Tracks and Namespaces): .session 名前空間の空トラック名は DOES_NOT_EXIST で拒否
        if track_ns.is_session_level() && track_name.is_empty() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "empty track name in .session namespace",
            );
            return Ok(());
        }
        // draft-ietf-moq-transport-21 §9.11 (FETCH): オブジェクトが公開されていない
        // トラックへの FETCH は INVALID_RANGE。対象 subscription があり、Largest が
        // 一度も観測されていない場合が該当する。subscription 自体が無いときは受理する。
        let key = (track_ns.clone(), track_name.clone(), TrackRole::Publisher);
        let observed_largest = self
            .aliases
            .subscriptions_by_track
            .get(&key)
            .and_then(|ids| ids.first())
            .and_then(|&sub_request_id| self.subscriptions.get(&sub_request_id))
            .and_then(|sub| {
                if sub.largest_location.is_none() && sub.largest_received_location.is_none() {
                    None
                } else {
                    super::subscription::delivery::effective_largest_object(sub)
                }
            });
        let has_track_subscription = self
            .aliases
            .subscriptions_by_track
            .get(&key)
            .is_some_and(|ids| !ids.is_empty());
        if has_track_subscription && observed_largest.is_none() {
            self.emit_request_error(
                request_id,
                REQUEST_INVALID_RANGE,
                "track has no published objects",
            );
            return Ok(());
        }
        // Fetch range の解決と検証。Fetch 規則では End 省略時は End = Largest Object。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        let (start, end) = match filter.as_ref() {
            None => (
                Some(Location {
                    group_id: 0,
                    object_id: 0,
                }),
                observed_largest,
            ),
            Some(filter) => (
                filter.effective_start_location(observed_largest.as_ref()),
                filter.effective_end_location(
                    observed_largest.as_ref(),
                    LocationFilterContext::Fetch,
                ),
            ),
        };
        // 空 range (Start > End) は INVALID_RANGE で拒否する
        if let (Some(start), Some(end)) = (start.as_ref(), end.as_ref())
            && start > end
        {
            self.emit_request_error(
                request_id,
                REQUEST_INVALID_RANGE,
                "fetch location filter range is empty",
            );
            return Ok(());
        }
        // draft-ietf-moq-transport-21 §9.11 (FETCH): Start Location が Largest Object を
        // 上回る範囲外要求は INVALID_RANGE (`start == largest` は範囲内として受理)。
        // Largest 未観測のときは検証をスキップして受理する。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        if let (Some(start), Some(largest)) = (start.as_ref(), observed_largest.as_ref())
            && start > largest
        {
            self.emit_request_error(
                request_id,
                REQUEST_INVALID_RANGE,
                "fetch start location exceeds largest object",
            );
            return Ok(());
        }
        let group_order = fetch.parameters.group_order();
        if let Some(order) = group_order {
            // draft §9.20.9 (GROUP ORDER Parameter): 値域外の受信はセッションを閉じる
            if let Err(err) = super::subscription::validation::validate_group_order(order) {
                self.fail(err.clone());
                return Err(err);
            }
        }
        // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
        // 値域外 (0 / 1 以外) の受信は MUST でセッションを PROTOCOL_VIOLATION により閉じる。
        let include_properties = fetch.parameters.include_properties();
        if let Some(v) = include_properties
            && let Err(err) = super::subscription::validation::validate_include_properties(v)
        {
            self.fail(err.clone());
            return Err(err);
        }
        let entry = Fetch {
            request_id,
            my_role: TrackRole::Publisher,
            track_namespace: Some(track_ns),
            track_name: Some(track_name),
            // publisher 側は FETCH_OK の End 検証を行わないため要求 Start を保持しない
            fetch_start: None,
            state: FetchState::Pending,
            end_location: None,
            end_of_track: false,
            response_received: false,
            data_stream_finished: false,
            subscriber_priority: fetch.parameters.subscriber_priority(),
            group_order,
            // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
            // peer が FETCH で指定した値を保持し、FETCH_OK 送信時に参照する
            include_properties,
        };
        self.fetches.insert(request_id, entry);
        self.request_streams.insert(request_id, RequestKind::Fetch);
        Ok(())
    }

    pub(super) fn handle_update_for_fetch(
        &mut self,
        request_id: u64,
        parameters: MessageParameters,
    ) -> Result<(), SessionError> {
        let fetch = self
            .fetches
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if fetch.my_role != TrackRole::Publisher {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_UPDATE (fetch) received on subscriber side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        if fetch.state != FetchState::Established {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_UPDATE requires Established fetch",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §9.20.8 (SUBSCRIBER PRIORITY Parameter): REQUEST_UPDATE に SUBSCRIBER_PRIORITY が含まれる場合は更新
        if let Some(priority) = parameters.subscriber_priority() {
            fetch.subscriber_priority = Some(priority);
        }
        // draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): GROUP_ORDER は確定後変更不可 (draft-ietf-moq-transport-21 §5.1.1 (Definitions)) のため更新しない
        self.events.push_back(SessionEvent::RequestUpdateReceived {
            request_id,
            parameters,
        });
        Ok(())
    }

    pub(super) fn handle_peer_fetch_ok(
        &mut self,
        request_id: u64,
        ok: WireFetchOk,
    ) -> Result<(), SessionError> {
        self.clear_control_message_deadline(request_id);
        let Some(fetch) = self.fetches.get(&request_id) else {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "FETCH_OK received for unknown request id",
            );
            self.fail(err.clone());
            return Err(err);
        };
        if fetch.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "FETCH_OK received on publisher side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §3.2.1 (Fetch State Management): publisher は FETCH に
        // 対してちょうど 1 個の FETCH_OK / REQUEST_ERROR を送る。受信済みフラグによる
        // 二重応答の検出を検証順序の先頭で行う (draft-ietf-moq-transport-21 §9.11
        // (FETCH) は応答がデータストリームの FIN 前後どちらでもよいとするため、
        // state ベースの二重応答判定は FIN 後到着を誤って fail させる)。
        if fetch.response_received {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "FETCH_OK received twice for the same fetch",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §9.11 (FETCH): FETCH_OK / REQUEST_ERROR は
        // データストリームの FIN 後に届いても受理する。`Pending` は従来どおり
        // `Established` へ遷移させ、`Terminated` は維持する (`Established` に戻すと
        // `fetch_cleanup_ready` の判定により fetch が回収不能になるため)。
        // subscriber 側で `Established` になるのは本関数の受理経路のみであり、その時点で
        // `response_received` が必ず true になる (invariant: subscriber 側 `Established` ⇒
        // `response_received == true`)。よってこのチェックに到達するのは `Pending` /
        // `Terminated` のみで、`Established` 分岐は invariant が崩れた場合の防御となる。
        if fetch.state == FetchState::Established {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "FETCH_OK in unexpected fetch state",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §9.12 (FETCH_OK): End Location < Start Location なら
        // PROTOCOL_VIOLATION。Start Location は送信時に LOCATION_FILTER から解決した
        // `fetch_start` を使う。相対指定は Largest 未知のため先頭寄りに解決され、
        // 真の Start を下回る End の検出漏れがありうる (送信者が peer の Largest を
        // 知り得ないための限定事項)。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        let request_start = self
            .fetches
            .get(&request_id)
            .and_then(|fetch| fetch.fetch_start.as_ref());
        if let Some(start) = request_start
            && ok.end_location < *start
        {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "FETCH_OK End Location smaller than Start Location",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft §2.5.1 (Mandatory Track Properties): 未知の必須プロパティを含む FETCH_OK は fetch をキャンセルする
        // FIN 後の `Terminated` でも一律スキップはしない (アプリが unknown mandatory による
        // キャンセルを検知する手段が失われるため)。bidi request stream 終端 (cancel) 経路で
        // `RequestTerminated` を発行済みの場合は 2 回 push されうるため、アプリは冪等に扱うこと。
        if ok.track_properties.has_unknown_mandatory() {
            let fetch = self
                .fetches
                .get_mut(&request_id)
                .expect("fetch entry presence checked above");
            fetch.state = FetchState::Terminated;
            fetch.response_received = true;
            self.events.push_back(SessionEvent::RequestTerminated {
                request_id,
                kind: RequestKind::Fetch,
                reason: TerminationReason::LocalCancel,
            });
            return Ok(());
        }
        let fetch = self
            .fetches
            .get_mut(&request_id)
            .expect("fetch entry presence checked above");
        if fetch.state == FetchState::Pending {
            fetch.state = FetchState::Established;
        }
        fetch.response_received = true;
        fetch.end_of_track = ok.end_of_track != 0;
        fetch.end_location = Some(ok.end_location);
        // FETCH_OK 受信をアプリへ能動通知する (FIN 前後どちらの到着もここに至る)。
        // 終端情報は `fetch()` ポーリングでも参照できるが、イベント駆動のために載せる。
        let end_location = fetch
            .end_location
            .expect("end_location assigned just above");
        let end_of_track = fetch.end_of_track;
        self.events.push_back(SessionEvent::FetchOkReceived {
            request_id,
            end_location,
            end_of_track,
        });
        Ok(())
    }

    pub(super) fn send_ok_for_fetch(&mut self, request_id: u64) -> Result<(), SessionError> {
        let fetch = self
            .fetches
            .get(&request_id)
            .expect("locate_request guarantees key presence");
        if fetch.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_ok (fetch) can only be sent by publisher-role",
            ));
        }
        if fetch.state != FetchState::Established {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_ok (fetch) requires Established state",
            ));
        }
        Ok(())
    }

    pub(super) fn handle_ok_for_fetch(
        &mut self,
        request_id: u64,
        parameters: &MessageParameters,
    ) -> Result<(), SessionError> {
        let fetch = self
            .fetches
            .get(&request_id)
            .expect("locate_request guarantees key presence");
        if fetch.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_OK (fetch) received on publisher side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // REQUEST_OK は REQUEST_UPDATE への応答 (REQUEST_UPDATE_OK) のみであり
        // (draft §10.5 (REQUEST_OK) の応答対象に FETCH は含まれない)、REQUEST_UPDATE は
        // 複数回送れるため二重応答フラグの対象外とする。FETCH への応答は FETCH_OK /
        // REQUEST_ERROR のみ (draft §5.2 (Fetch State Management))。
        // データストリームの FIN 後に REQUEST_UPDATE_OK が届くこともあるため、
        // `Terminated` でも受理する (REQUEST_OK は bidi request stream で送られ、
        // データストリームの終端とは独立であることからの推論。draft-ietf-moq-transport-21
        // §9.11 (FETCH) の "can come at any time" は FETCH_OK / REQUEST_ERROR の
        // タイミングを述べたもの。将来の draft 改版で変わる可能性がある)。
        if fetch.state == FetchState::Pending {
            // REQUEST_UPDATE は実装が `Established` でのみ送るため、
            // `Pending` での REQUEST_OK は peer の違反。仕様上も
            // draft-ietf-moq-transport-21 §9.12 (FETCH_OK): "A publisher sends a FETCH_OK as the first message
            // on the bidi stream" により、REQUEST_OK が FETCH_OK より先に届くことはない。
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_OK for fetch requires Established or Terminated state",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter): REQUEST_UPDATE_OK で新しい LARGEST_OBJECT が通知される
        self.events.push_back(SessionEvent::RequestOkReceived {
            request_id,
            request_kind: RequestKind::Fetch,
            parameters: parameters.clone(),
        });
        Ok(())
    }

    // ─── REQUEST_ERROR per-kind dispatch ───────────────────────

    /// FETCH への REQUEST_ERROR 送信時の fetch 状態遷移
    ///
    /// draft-ietf-moq-transport-21 §9.5.1: REQUEST_UPDATE 失敗時、publisher は
    /// FETCH データストリームをリセットしなければならない (MUST)。本関数は
    /// `Established` 分岐で fetch state を `Terminated` に遷移させ、対応する outgoing
    /// FETCH data stream を `outgoing_fetch` から除去する。実際の `ResetDataStream`
    /// イベント push (と `Ok` を返した後の呼出元 `send_request_error` によるワイヤ順序制御)
    /// のため、reset 対象の stream id を戻り値で返す。ワイヤ順序は
    /// `REQUEST_ERROR (bidi) → RESET_STREAM (uni data)` を保つ (subscription 側の
    /// PUBLISH_DONE 遅延 push と対称)。
    ///
    /// 戻り値:
    /// - `Ok(Some(stream_id))`: `Established` からの遷移で outgoing FETCH data stream が
    ///   存在した場合。呼出元が `SessionEvent::ResetDataStream` を push する
    /// - `Ok(None)`: `Established` からの遷移で app が既に自分で reset 済み、または
    ///   `Pending` 分岐 (FETCH_HEADER 未送信のため outgoing stream 無し)
    /// - `Err(_)`: `Terminated` からの送信 or 非 publisher role からの送信は不正
    ///
    /// draft-ietf-moq-transport-21 §9.11 (FETCH): 1 つの FETCH に対する応答は 1 本の
    /// data stream で送る (§11: "sends Objects matching a FETCH request on one Data Stream")。
    /// したがって `Established` の fetch に対応する outgoing_fetch エントリは 0 or 1 本のみ。
    ///
    /// `Established` 分岐 (REQUEST_UPDATE 失敗応答) は `Terminated` に遷移する
    /// (draft §3.2.1 の "A REQUEST_ERROR indicates that both endpoints can immediately
    /// remove state." の破棄許可を行使する。§9.5.1 の MUST でデータストリームは
    /// リセット必須のため、`Established` のまま維持すると「再配信不能な状態」と
    /// 状態機械が矛盾するため)。
    /// `Pending` 分岐も `Terminated` に遷移するが、`send_fetch_header` が
    /// `Established` を要求するため outgoing FETCH stream エントリは構造上存在せず、
    /// 掃除対象は無い。
    /// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub(super) fn send_err_for_fetch(
        &mut self,
        request_id: u64,
    ) -> Result<Option<DataStreamId>, SessionError> {
        let fetch = self
            .fetches
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if fetch.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_error (fetch) can only be sent by publisher-role",
            ));
        }
        match fetch.state {
            FetchState::Pending => {
                fetch.state = FetchState::Terminated;
                Ok(None)
            }
            FetchState::Established => {
                // REQUEST_UPDATE 失敗応答: §3.2.1 の破棄許可を行使して Terminated に遷移する
                // (データストリームは §9.5.1 の MUST でリセット必須。以下で Session から
                // 自動発行する)
                fetch.state = FetchState::Terminated;
                // draft §9.5.1: publisher MUST reset the FETCH data stream. Session から
                // 自動発行するため、対応する outgoing_fetch (§9.11 により最大 1 本) を除去する。
                let stream_id = self
                    .data_streams
                    .outgoing_fetch
                    .iter()
                    .find_map(|(&sid, s)| (s.request_id == request_id).then_some(sid));
                if let Some(stream_id) = stream_id {
                    self.data_streams.outgoing_fetch.remove(&stream_id);
                    if let Some(fetch) = self.fetches.get_mut(&request_id) {
                        // 終端済み記録 (draft §3.2.1 "It can remove all FETCH state after
                        // closing the data stream with a FIN." に相当。RESET も破棄可能な
                        // 終端種別に含まれる。send_fetch_data_stream_closed を経由しない
                        // 直接除去のため明示的に設定する)
                        fetch.data_stream_finished = true;
                    }
                }
                Ok(stream_id)
            }
            FetchState::Terminated => Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "send_request_error requires non-terminated state (fetch)",
            )),
        }
    }

    pub(super) fn handle_err_for_fetch(&mut self, request_id: u64) -> Result<(), SessionError> {
        let fetch = self
            .fetches
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if fetch.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_ERROR (fetch) received on publisher side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-21 §9.11 (FETCH): REQUEST_ERROR は
        // FETCH データストリームの FIN 前後どちらでも届きうるため、state によらず受理する。
        // REQUEST_UPDATE 失敗応答としての REQUEST_ERROR (`Established` 分岐) と
        // FETCH への応答 (`Pending` 遷移 / `Terminated` 受理) は state ベースでは区別不能であり、
        // フラグ判定すると正当な順序 (FETCH_OK 受信 → REQUEST_UPDATE 送信 → FIN →
        // REQUEST_UPDATE 失敗応答) を誤って二重応答として fail させるため、フラグは判定しない。
        // 帰結として REQUEST_ERROR → REQUEST_ERROR の二重応答は検出されない既知の制約
        // (draft §3.2.1 (Fetch State Management) の "exactly one" は publisher への MUST であり、
        // 受信側にセッションクローズを明示的に要求する文言ではない。将来の draft 改版で
        // 変わる可能性がある)。
        // `Terminated` 分岐でフラグを立てて安全なのは、REQUEST_UPDATE が `Established` でのみ
        // 送信されるため、`Terminated` かつフラグ未設定の fetch への REQUEST_ERROR は必ず
        // FETCH への応答であることによる (REQUEST_UPDATE 失敗応答は `Established` 分岐で
        // 処理され、フラグは FETCH_OK 受信時に設定済み)。
        match fetch.state {
            FetchState::Pending => {
                fetch.state = FetchState::Terminated;
                fetch.response_received = true;
            }
            FetchState::Established => {
                // REQUEST_UPDATE 失敗応答: §3.2.1 の "A subscriber keeps FETCH state until
                // ... receives REQUEST_ERROR" に従い Terminated に遷移する (REQUEST_ERROR
                // を受信したことに変わりはなく、データストリームも §9.5.1 の MUST で
                // publisher 側がリセット済み。RESET が後から届いても `Terminated` のため
                // `recv_data_stream_closed` の dispatch ガードで吸収される)。
                // フラグは FETCH_OK 受信時に設定済みであり、ここでは立てない
                // (REQUEST_ERROR を FETCH への応答として受け取ったわけではないため)。
                fetch.state = FetchState::Terminated;
            }
            FetchState::Terminated => {
                fetch.response_received = true;
            }
        }
        Ok(())
    }
}
