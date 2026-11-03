//! FETCH 関連
//!
//! draft-ietf-moq-transport-23 §3.2 (Fetch) / §3.2.4 (Fetch State Management) / §9.11 (FETCH) /
//! §9.3 (REQUEST_OK) に対応する `impl Session` の送受信メソッドをまとめる。FETCH の初回成功応答は
//! REQUEST_OK (Type 0x07) であり、FETCH の REQUEST_UPDATE_OK とはセッションの状態で区別する。
//! 単独 FETCH の相対 Location Filter は §3.2 (Fetch) に基づき REQUEST_ERROR (INVALID_RANGE) で
//! 拒否する。将来 draft 側で変更される可能性がある。

use crate::error::{
    REQUEST_DOES_NOT_EXIST, REQUEST_INVALID_FILTER, REQUEST_INVALID_RANGE, SESSION_INVALID_FIELD,
    SESSION_INVALID_PARAMETER, SESSION_PROTOCOL_VIOLATION, SESSION_ROLE_VIOLATION,
    SESSION_UNEXPECTED_MESSAGE, STREAM_CANCELLED,
};
use crate::message::{
    ControlMessage, FETCH_ALLOWED_PARAMS, FETCH_OK_ALLOWED_PARAMS, Fetch as WireFetch, RequestOk,
    common::{Location, TrackNamespace},
};
use crate::message_parameter::{LocationFilter, LocationFilterContext, MessageParameters};
use crate::track_properties::TrackProperties;
use alloc::vec::Vec;

use super::core::Session;
use super::data::IncomingDataStream;
use super::subscription::delivery::update_largest_object_in_parameters;
use super::types::{
    DataStreamId, Fetch, FetchState, RequestKind, RequestStreamEnd, SendRequestError, SessionError,
    SessionEvent, TerminationReason, TrackRole, terminationreason_from_end,
};

/// fetch が破棄可能な状態条件を満たしているか
///
/// subscriber 側は `Terminated`、publisher 側は `Terminated` またはデータストリーム
/// 終端済み (`send_fetch_data_stream_closed` 呼び出し済み。`Established` のままでも破棄可能。
/// draft-ietf-moq-transport-23 §3.2.4 の "It can remove all FETCH state after closing the
/// data stream with a FIN." に基づく)。
fn fetch_finished(fetch: &Fetch) -> bool {
    match fetch.my_role {
        TrackRole::Publisher => fetch.state == FetchState::Terminated || fetch.data_stream_finished,
        TrackRole::Subscriber => fetch.state == FetchState::Terminated,
    }
}

/// Location が FETCH の要求範囲内かを返す
///
/// draft-ietf-moq-transport-23 §3.2 (Fetch): "If Largest Object is within the requested
/// range, the FETCH response ends at Largest Object and the publisher MUST include the
/// LARGEST_OBJECT parameter in FETCH_OK. Otherwise, the response ends at the requested
/// End Location." 範囲は Start と End の両端を含む。
///
/// `request_end` が `None` のときは要求が End を省略しており、その End は Largest Object
/// である (§9.19.9 (LOCATION FILTER Parameter)) ため、Start だけを下限として判定する。
/// この場合 Largest Object は常に範囲内になる。
fn fetch_location_within_range(
    request_start: Option<Location>,
    request_end: Option<Location>,
    location: Location,
) -> bool {
    if let Some(start) = request_start
        && location < start
    {
        return false;
    }
    if let Some(end) = request_end
        && location > end
    {
        return false;
    }
    true
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
    /// draft-ietf-moq-transport-23 §3.2 (Fetch) により、データストリームの
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
    /// publisher 側の終端済み fetch の破棄経路は draft-ietf-moq-transport-23 §3.2.4
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
                IncomingDataStream::Fetch(stream) if stream.request_id == request_id
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
    /// 掃除を兼ねる (下記)。draft-ietf-moq-transport-23 §3.2 (Fetch)
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
    /// `rejected_request_ids` 経由で no-op で吸収する。応答 (REQUEST_OK / REQUEST_ERROR) と
    /// REQUEST_UPDATE は、request 種別と状態が失われているため unknown request id として
    /// UNEXPECTED_MESSAGE で fail する
    /// (アプリは peer が request stream を閉じるまで `forget_fetch` を待つこと)
    /// (draft §9.5 の "An endpoint that receives a REQUEST_UPDATE other than in the two
    /// cases above MUST close the session with UNEXPECTED_MESSAGE." に従い、セッション終了
    /// コードは draft-ietf-moq-transport-23 §12.2 (Session Termination Codes) の
    /// UNEXPECTED_MESSAGE / ROLE_VIOLATION に細分類する。
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
        self.peer_fin_received.remove(&request_id);
        self.local_fin_sent.remove(&request_id);
        self.clear_request_stream_goaway_deadline(request_id);
        self.remove_incoming_data_streams_for_request(request_id);
        self.release_peer_track_history(request_id);
        self.pending_fetch_tails.remove(&request_id);
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
    /// draft-ietf-moq-transport-23 §6.4.2.3 (Request Cancellation and Rejection) に従い、bidi
    /// request stream の終端で Fetch の state を `Terminated` に遷移させる。FETCH データ
    /// stream (uni) の終端とは別で、request 自体の打ち切り処理である。
    ///
    /// 到達しうるのは `RequestStreamEnd::Reset` (cancel) と、自側が requester のときの
    /// responder の FIN (要求の完了通知) である。自側が responder の FETCH では peer FIN を
    /// 終端として扱わないため、呼び出し元 (`recv_request_stream_closed`) が先に分岐する。
    /// subscriber 役の fetch は応答 (FETCH_OK / REQUEST_ERROR) もデータストリームも送信せず、
    /// fill fetch stream のような付随 stream も持たないため、state の遷移だけで足りる。
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
        // 応答 (FETCH_OK) が来ないまま request が終端したため、応答終端を待つ gap 判定を
        // 破棄する。
        self.pending_fetch_tails.remove(&request_id);
        // cancel (§6.4.2.3 (Request Cancellation and Rejection)) または requester 側の
        // 完了通知 (§6.4.2.2 (Graceful Request Stream Closure)) で request が終端したため、
        // REQUEST_UPDATE のクレジット会計を解放する。FETCH の REQUEST_UPDATE は
        // subscriber 役だけが送信でき (draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE))、
        // 受信側の適用待ちキューは subscription にしか無い。
        self.remove_request_update_credit_entries(request_id);
        Ok(terminationreason_from_end(end))
    }

    // ─── 送信 API ──────────────────────────────────────────

    /// FETCH を送信する (draft-ietf-moq-transport-23 §9.11 (FETCH)、自側が subscriber)
    ///
    /// range は LOCATION_FILTER パラメータで指定する (省略時は track 先頭から
    /// Largest Object まで)。draft-ietf-moq-transport-23 §3.2 (Fetch) により、相対
    /// Location Filter (Type 0x01 / 0x05) を持つ単独 FETCH は peer が必ず
    /// REQUEST_ERROR (INVALID_RANGE) で拒否するため、送信前にエラーで拒否する。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    ///
    /// 失敗条件はすべて `SendRequestError::Session` (`SESSION_PROTOCOL_VIOLATION` 起因)
    /// で返し、セッションは閉じない。scope ・相対フィルタ・空 range の検証のみ request_id
    /// 発行より前に行い (欠番を作らない)、値域検証は発行後のままである。
    pub fn send_fetch(
        &mut self,
        track_namespace: TrackNamespace,
        track_name: Vec<u8>,
        parameters: MessageParameters,
    ) -> Result<u64, SendRequestError> {
        self.require_established()?;
        self.check_peer_goaway()?;
        // draft-ietf-moq-transport-23 §2.4.1 (Track Naming): 空の Track Name は送出しない
        Self::validate_outgoing_track_name(&track_name)?;
        // draft-ietf-moq-transport-23 §2.4.3 (Reserved Namespaces): single period `.` 予約名前空間への FETCH は送信不可
        if track_namespace.is_single_period() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "application cannot use single-period reserved namespace",
            )
            .into());
        }
        // draft-ietf-moq-transport-23 §3.3.2 (Range Filters): peer が必ず INVALID_FILTER で
        // 拒否するメッセージを無警告で送出しないよう、送信前に自側で弾く
        self.validate_outgoing_range_filters(&parameters)?;
        // draft-ietf-moq-transport-23 §9.19.1 (Parameter Scope): FETCH で許可されない
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
        let filter = match parameters.location_filter_typed() {
            Ok(filter) => filter,
            Err(_) => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "invalid fetch filter encoding",
                )
                .into());
            }
        };
        // draft-ietf-moq-transport-23 §3.2 (Fetch): 相対 Location Filter を持つ単独 FETCH は
        // peer が必ず REQUEST_ERROR (INVALID_RANGE) で拒否する。判定は解決後の Location では
        // なくフィルタの形式で行う (Largest Object 未観測の相対フィルタは `{0, 0}` に解決され、
        // 絶対指定と区別できないため)。request_id 発行前に置き、エラー時に欠番を作らない。
        // FILL_PARAMETERS の内側の相対フィルタは fill range の解決規則が別
        // (draft-ietf-moq-transport-23 §3.4 (Fill Semantics)) であり、この禁止の対象外である。
        if filter.is_some_and(LocationFilter::is_relative) {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "relative location filter is not allowed in FETCH",
            )
            .into());
        }
        // 要求 range の解決と空 range の事前検証。Largest は送信時点では未知のため
        // `None` で解決する (絶対値で範囲が定まる形式のみがここに到達する)。
        // 空 range (Start > End) の FETCH は送らない (受信側は INVALID_RANGE で
        // 拒否するのが本実装の選択であり、peer も同様と期待する)。
        // 保持する `request_end` は要求が明示した終端だけであり、End を省略した要求では
        // `None` のままにする (応答範囲の終端は FETCH_OK の LARGEST_OBJECT から解決する)。
        let (fetch_start, fetch_end) = match filter.as_ref() {
            Some(filter) => (
                filter.effective_start_location(None),
                filter.effective_end_location(None, LocationFilterContext::Fetch),
            ),
            None => (
                Some(Location {
                    group_id: 0,
                    object_id: 0,
                }),
                None,
            ),
        };
        let request_end = filter
            .as_ref()
            .and_then(LocationFilter::explicit_end_location);
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
            request_end,
            state: FetchState::Pending,
            end_location: None,
            // 初回応答 (受信側) または FETCH_OK 送信時に設定する
            expires_ms: None,
            response_received: false,
            data_stream_finished: false,
            local_cancel_sent: false,
            peer_stop_sending_error_code: None,
            subscriber_priority: parameters.subscriber_priority(),
            group_order: parameters.group_order(),
            // 自側は subscriber のため FETCH_OK を送らず、INCLUDE_PROPERTIES の保持は不要
            include_properties: None,
            has_range_filters: parameters.has_range_filters(),
        };
        if let Some(order) = fetch.group_order {
            super::subscription::validation::validate_group_order(order)?;
        }
        // draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter):
        // 送信前に値域を検証し、不正値の送出と状態登録を防ぐ
        if let Some(v) = parameters.include_properties() {
            super::subscription::validation::validate_include_properties(v)?;
        }
        self.acquire_peer_track_history(request_id, &track_namespace, &track_name);
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

    /// FETCH の初回成功応答 (REQUEST_OK、別名 FETCH_OK) を送信する (publisher 側)
    ///
    /// draft-ietf-moq-transport-23 §9.3 (REQUEST_OK): FETCH の初回成功応答は Type 0x07 の
    /// REQUEST_OK であり、FETCH_OK はその別名である。FETCH の REQUEST_UPDATE_OK と
    /// 同じメッセージ型になるため、両者はセッションの状態 (初回応答済みか) で区別する。
    ///
    /// 応答範囲の終端は本呼び出しの時点で確定させる (draft §3.2 (Fetch))。
    /// Largest Object が要求範囲内ならその位置へ応答を切り詰め、LARGEST_OBJECT を必ず載せる。
    /// 範囲外なら要求で明示した End Location を使う (§3.2.2 (Gaps in a Fetch Stream))。
    /// 要求が End を省略している場合の End は Largest Object であるため
    /// (§9.19.9 (LOCATION FILTER Parameter))、`parameters` に LARGEST_OBJECT を渡すか、
    /// 自側が publish 済みの Object から Largest Object を観測できている必要がある。
    ///
    /// `parameters` の LARGEST_OBJECT は Session が観測した Track の最大 Location との
    /// 大きい方に正規化し、要求範囲内なら必ず載せ、範囲外なら取り除く。応答範囲と矛盾する
    /// LARGEST_OBJECT を載せると、§3.2.2 が End Location を LARGEST_OBJECT と定めるために
    /// 受信側の範囲解釈がぶれるためである。EXPIRES はそのまま載せ、送信した値を
    /// [`Fetch::expires_ms`] に保持する (§9.19.16 (EXPIRES Parameter))。
    ///
    /// 戻り値が `Err` のときは fetch の状態を変更しない (エンコードは sans-I/O のため
    /// I/O 層で行われ、状態遷移後の失敗は peer に応答が届かない孤児状態を残す)。
    ///
    /// # Errors
    ///
    /// - セッションが確立していない: `SESSION_PROTOCOL_VIOLATION`
    /// - `request_id` の fetch が無い、publisher 役でない、`Pending` でない:
    ///   `SESSION_PROTOCOL_VIOLATION`
    /// - `parameters` に FETCH_OK で許可されないパラメータがある: `SESSION_PROTOCOL_VIOLATION`
    /// - 応答範囲の終端を確定できない (要求が End を省略し、Largest Object も不明):
    ///   `SESSION_PROTOCOL_VIOLATION`
    pub fn send_fetch_ok(
        &mut self,
        request_id: u64,
        parameters: MessageParameters,
        track_properties: TrackProperties,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        // 存在・ role ・ state を先に検証する。Track の名前は Largest Object の観測に使う
        let (request_start, request_end, track_namespace, track_name) = {
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
            // fetch の namespace / name は送受信経路で常に設定される契約である
            // (types.rs の Fetch の doc 参照)
            (
                fetch.fetch_start,
                fetch.request_end,
                fetch
                    .track_namespace
                    .clone()
                    .expect("fetch track_namespace is always set"),
                fetch
                    .track_name
                    .clone()
                    .expect("fetch track_name is always set"),
            )
        };
        // draft-ietf-moq-transport-23 §9.19.1 (Parameter Scope): FETCH_OK は EXPIRES と
        // LARGEST_OBJECT だけを運べる (§9.3)。スコープ外のパラメータを含む応答では状態を
        // 一切変更せずエラーを返す。
        if parameters.validate_scope(FETCH_OK_ALLOWED_PARAMS).is_err() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "FETCH_OK parameter not allowed in this context",
            ));
        }
        let mut parameters = parameters;
        // draft-ietf-moq-transport-23 §3.1.4 (Largest Object) / §9.19.17 (LARGEST OBJECT
        // Parameter): Largest Object は送信端点が把握している Track の最大 Location である。
        // アプリが渡した値 (自前のキャッシュ等) と Session が観測した値の大きい方を採る
        // (観測値でアプリ指定値を下げない)。
        if let Some(largest) = self.publisher_track_largest(&track_namespace, &track_name) {
            update_largest_object_in_parameters(&mut parameters, &largest);
        }
        let largest = parameters
            .largest_object()
            .map(|(group_id, object_id)| Location {
                group_id,
                object_id,
            });
        // draft-ietf-moq-transport-23 §3.2 (Fetch): Largest Object が要求範囲内なら応答は
        // そこへ切り詰め、LARGEST_OBJECT を必ず載せる。範囲外なら要求の End Location を
        // 使い、LARGEST_OBJECT は載せない (§3.2.2 (Gaps in a Fetch Stream) は End Location を
        // LARGEST_OBJECT があればその値と定めるため、範囲外の値を残すと解釈がぶれる)。
        let end_location = match largest {
            Some(largest) if fetch_location_within_range(request_start, request_end, largest) => {
                largest
            }
            _ => {
                parameters.remove_largest_object();
                match request_end {
                    Some(end) => end,
                    // 要求が End を省略している場合の End は Largest Object であり、
                    // Largest Object が不明なら応答範囲を確定できない。
                    None => {
                        return Err(SessionError::new(
                            SESSION_PROTOCOL_VIOLATION,
                            "cannot determine the FETCH_OK end location: \
                             LARGEST_OBJECT is required for an open-ended request",
                        ));
                    }
                }
            }
        };
        let fetch = self
            .fetches
            .get_mut(&request_id)
            .expect("fetch existence checked above");
        fetch.state = FetchState::Established;
        fetch.end_location = Some(end_location);
        // draft-ietf-moq-transport-23 §9.19.16 (EXPIRES Parameter): FETCH_OK の EXPIRES は
        // sender が request を終了するまでの時間である。送信した値を状態から参照できる
        // ように保持する (本実装は FETCH の期限タイマーを起動しない)。
        fetch.expires_ms = parameters.expires();
        // draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter):
        // peer が FETCH で INCLUDE_PROPERTIES=0 を指定したとき、FETCH_OK の
        // Track Properties は存在するが空にする (SHOULD)。
        let track_properties = if fetch.include_properties == Some(0) {
            TrackProperties::new()
        } else {
            track_properties
        };
        let msg = ControlMessage::RequestOk(RequestOk {
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
        // draft-ietf-moq-transport-23 §9.19.7 (SUBSCRIBER PRIORITY Parameter): REQUEST_UPDATE に SUBSCRIBER_PRIORITY が含まれる場合は更新
        if let Some(priority) = parameters.subscriber_priority() {
            fetch.subscriber_priority = Some(priority);
        }
        // draft-ietf-moq-transport-23 §9.19.8 (GROUP ORDER Parameter): GROUP_ORDER は確定後変更不可 (draft-ietf-moq-transport-23 §5.1.1 (Definitions)) のため更新しない
        Ok(())
    }

    // ─── FETCH stream 終端遷移 API ─────────────────────────
    //
    // draft §3.2.4 (Fetch State Management): subscriber 側は FIN / RESET_STREAM 受信、または
    // STOP_SENDING 送信で FETCH state を終える。publisher 側は STOP_SENDING
    // 受信で fetch state を破棄できる。I/O 層が QUIC stream イベントを観測
    // してから本関数を呼ぶ sans-I/O な API。

    /// subscriber 側: FETCH データストリーム (uni) の終端を通知する
    ///
    /// 通常の FETCH 応答 stream 用であり、`request_id` には `Fetch` の Request ID を
    /// 渡す。fill fetch stream (subscription の Request ID を載せる) の終端は
    /// stream id 系 (`recv_data_stream_closed`) で通知すること
    /// (fill 終端は subscription の state を変えず、Stream Count の open 数だけを戻す)。
    ///
    /// draft-ietf-moq-transport-23 §3.2.4 (Fetch State Management): subscriber は FETCH_HEADER
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
    /// draft §3.2.4 (Fetch State Management): subscriber が自ら cancel する経路。
    /// state は `Terminated` に遷移する。I/O 層が QUIC STOP_SENDING を送出
    /// したうえで本関数を呼ぶ (session 層は frame 送出しない)。
    /// draft-ietf-moq-transport-23 §6.4.2.3 (Request Cancellation and Rejection):
    /// cancel は開いている方向を打ち切る。I/O 層は `ResetRequestStream` を受けて
    /// bidi request stream の送信方向を reset する。
    pub fn send_fetch_stop_sending(&mut self, request_id: u64) -> Result<(), SessionError> {
        self.terminate_subscriber_fetch(request_id, SubscriberFetchCloseKind::StopSending)?;
        if let Some(fetch) = self.fetches.get_mut(&request_id) {
            // bidi 送信方向を RESET_STREAM で閉じるため、以後の cancel 発行は不要になる
            fetch.local_cancel_sent = true;
        }
        // ローカル送信方向を reset で閉じるため、request stream GOAWAY の reset deadline は
        // 不要になる
        self.clear_request_stream_goaway_deadline(request_id);
        // subscriber による cancel のため bidi 送信方向を reset する (§6.4.2.3)。
        // CANCELLED (0x1) はいずれかの端点による cancel に該当する (§12.5)。
        self.events.push_back(SessionEvent::ResetRequestStream {
            request_id,
            error_code: super::types::DataStreamResetReason::Cancelled.error_code(),
        });
        // cancel で応答を返す経路が無くなるため、REQUEST_UPDATE のクレジット会計を解放する
        self.remove_request_update_credit_entries(request_id);
        Ok(())
    }

    /// publisher 側: FETCH データストリームで STOP_SENDING を受信した
    ///
    /// draft §3.2.4 (Fetch State Management): publisher は STOP_SENDING 受信で fetch state を
    /// 破棄してよい。state は `Terminated` に遷移する。
    ///
    /// draft §3.2.4: "The Publisher can remove fetch state as soon as it has received a
    /// STOP_SENDING. It MUST reset the bidi request stream and unidirectional data stream
    /// associated with the FETCH." (MUST) のとおり、bidi request stream の reset は
    /// `SessionEvent::ResetRequestStream` で I/O 層に指示する。データストリームの reset は
    /// アプリケーション層が本関数呼び出し後に行い、`send_fetch_data_stream_closed` を
    /// 呼んで終端を通知すること。アプリが `forget_fetch` を呼んだ場合はその安全網が
    /// 残った outgoing FETCH stream エントリを掃除する。終端通知を済ませた fetch は
    /// bidi request stream の終端を待たずに破棄できる
    /// (`forget_fetch` の doc 参照。終端通知漏れの場合は従来どおり破棄前に
    /// bidi request stream の終端 (`RequestTerminated`) を確認すること)。
    ///
    /// `error_code` は peer が STOP_SENDING に載せたアプリケーションエラーコードである
    /// (`None` は「アプリケーションエラーコード無し」。意味は [`RequestStreamEnd::Reset`] の
    /// `error_code` を参照する)。受信したコードは `Fetch` に保持し、
    /// [`Session::fetch`] の戻り値から参照できる。
    /// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn fetch_stop_sending_received(
        &mut self,
        request_id: u64,
        error_code: Option<u64>,
    ) -> Result<(), SessionError> {
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
            // cancel の理由を参照できるよう、解釈できたコード / できなかったことを保持する
            fetch.peer_stop_sending_error_code = Some(error_code);
        }
        self.remove_incoming_data_streams_for_request(request_id);
        // ローカル送信方向を reset で閉じるため、request stream GOAWAY の reset deadline は
        // 不要になる
        self.clear_request_stream_goaway_deadline(request_id);
        // §3.2.4 MUST の bidi reset を I/O 層に指示する。CANCELLED (0x1) は
        // subscriber による cancel に該当する (§12.5)。
        self.events.push_back(SessionEvent::ResetRequestStream {
            request_id,
            error_code: super::types::DataStreamResetReason::Cancelled.error_code(),
        });
        // peer の cancel で応答を返す経路が無くなるため、REQUEST_UPDATE のクレジット会計を
        // 解放する (draft-ietf-moq-transport-23 §6.4.2.3 (Request Cancellation and Rejection))
        self.remove_request_update_credit_entries(request_id);
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
        self.pending_fetch_tails.remove(&request_id);
        self.remove_incoming_data_streams_for_request(request_id);
        Ok(())
    }

    // ─── 受信ハンドラ ──────────────────────────────────────

    pub(super) fn handle_peer_fetch(&mut self, fetch: WireFetch) -> Result<(), SessionError> {
        let request_id = fetch.request_id;
        // draft-ietf-moq-transport-23 §2.4.1 (Track Naming): 空の Track Name は
        // PROTOCOL_VIOLATION でセッションを閉じる (MUST)。request 単位の拒否より先に検証する。
        self.validate_peer_track_name(&fetch.track_name)?;
        // FETCH は自側が publisher として応答する request である
        if !self.accept_peer_request(
            request_id,
            &fetch.parameters,
            true,
            FETCH_ALLOWED_PARAMS,
            "FETCH parameter not allowed in this context",
        )? {
            return Ok(());
        }
        // draft §8.9 (Authorization Token Compression): AUTHORIZATION_TOKEN Register/Delete/Use を peer cache に反映
        self.apply_peer_message_auth_tokens(&fetch.parameters)?;
        // draft-ietf-moq-transport-23 §3.3.2 (Range Filters): MAX_FILTER_RANGES 超過は INVALID_FILTER で拒否
        if let Err(reason) = self.check_incoming_range_filters(&fetch.parameters) {
            self.emit_request_error(request_id, REQUEST_INVALID_FILTER, reason);
            return Ok(());
        }
        let track_ns = fetch.track_namespace;
        let track_name = fetch.track_name;
        // draft-ietf-moq-transport-23 は値域 MUST (§9.19.9 (LOCATION FILTER Parameter) の
        // StartGroup + EndGroupDelta オーバーフローや §9.19.8 (GROUP ORDER Parameter) 等) と
        // 予約名前空間拒否の優先順位を規定しない。宛先自体が仕様上存在しない予約名前空間の拒否を
        // LOCATION_FILTER の decode より先に行う意図した選択である (値域 MUST は wire 経路では
        // decode 層がハンドラ到達前に発火するため、優先順位が観測されるのは API 経路で値域外の
        // パラメータを手組みしたメッセージのみ。本ハンドラ自身は LOCATION_FILTER 以外の値を
        // 解釈しない)。
        // draft-ietf-moq-transport-23 §2.4.3 (Reserved Namespaces): single period `.` 予約名前空間の FETCH は拒否
        if track_ns.is_single_period() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "reserved single-period namespace",
            );
            return Ok(());
        }
        // draft §6.5 (Session-Level Tracks and Namespaces): .session 名前空間のリクエストは
        // Application へ渡さず DOES_NOT_EXIST で拒否する。本ライブラリはセッションレベル
        // トラックを登録しないため、非空トラック名はすべて未認識のセッションレベルトラック
        // になる (空の Track Name は上の §2.4.1 (Track Naming) の検証でセッションを閉じるため
        // ここには到達しない)。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある
        if track_ns.is_session_level() {
            self.emit_request_error(
                request_id,
                REQUEST_DOES_NOT_EXIST,
                "session-level track does not exist",
            );
            return Ok(());
        }
        // draft-ietf-moq-transport-23 §9.11 (FETCH) / §3.3.1 (Location Filters) /
        // §9.19.9 (LOCATION FILTER Parameter):
        // Fetch range は LOCATION_FILTER パラメータで指定する。省略時と Location Filter
        // Type 0x00 (None) は unfiltered (先頭から Largest Object まで) として扱う。
        // 不正エンコーディングは wire 経路ではデコード層が検証するため到達せず、API 経路で
        // 手組みしたメッセージのみ到達する。安全側にセッションを閉じる (SUBSCRIBE 受信経路と対称)。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        let filter = match fetch.parameters.location_filter_typed() {
            Ok(filter) => filter,
            Err(msg_err) => {
                // draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter): 未知の
                // Location Filter Type は INVALID_PARAMETER、StartGroup + EndGroupDelta の
                // オーバーフローは DELTA_ENCODING_OVERFLOW で閉じる (MUST)。
                let err = crate::session::core::session_error_from_message(
                    &msg_err,
                    SESSION_INVALID_PARAMETER,
                );
                self.fail(err.clone());
                return Err(err);
            }
        };
        // draft-ietf-moq-transport-23 §3.2 (Fetch): "If the Location filter is relative
        // (see Section 9.19.9), the publisher MUST return REQUEST_ERROR with error code
        // INVALID_RANGE." 相対フィルタは解決後の Location ではなくフィルタの形式で判別する
        // (Largest Object 未観測の相対フィルタは `{0, 0}` に解決され、絶対指定と区別できない
        // ため)。Largest Object の観測有無や範囲の空・超過とは独立した MUST であり、
        // 範囲判定より前に拒否する。Fetch state を作らず REQUEST_ERROR だけを返し、
        // セッションは維持する (§6.4.2.3 (Request Cancellation and Rejection) の拒否経路)。
        // FILL_PARAMETERS の内側の相対フィルタは fill range の解決規則が別
        // (draft-ietf-moq-transport-23 §3.4 (Fill Semantics)) であり、この禁止の対象外である。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        if filter.is_some_and(LocationFilter::is_relative) {
            self.emit_request_error(
                request_id,
                REQUEST_INVALID_RANGE,
                "relative location filter is not allowed in FETCH",
            );
            return Ok(());
        }
        // draft-ietf-moq-transport-23 §3.2 (Fetch): オブジェクトが公開されていない
        // トラックへの FETCH は INVALID_RANGE。対象 subscription があり、Largest が
        // 一度も観測されていない場合が該当する。subscription 自体が無いときは受理する。
        let key = (track_ns.clone(), track_name.clone(), TrackRole::Publisher);
        // Largest Object は Track 単位 (§3.1.4) なので、同一 Track に複数の
        // publisher 役 subscription がある場合は全候補の `effective_largest_object` の
        // 最大を使う。先頭 1 件だけを見ると、先頭が未観測・他が観測済みの場合に
        // INVALID_RANGE を誤判定する。
        let observed_largest = self.publisher_track_largest(&track_ns, &track_name);
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
        // 相対フィルタは上で拒否済みのため、ここに到達するのは絶対値で範囲が定まる形式
        // (省略 / Type 0x00 / 0x02 / 0x03 / 0x04) だけであり、`observed_largest` は
        // End の既定値と範囲判定にのみ使う。
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
        // draft-ietf-moq-transport-23 §3.2 (Fetch): Start Location が Largest Object を
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
        // 要求が明示した終端だけを保持する。End を省略した要求の End は Largest Object で
        // あり (draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter))、要求時点で
        // 固定された値ではないため、応答範囲の終端は `send_fetch_ok` が FETCH_OK の送信時点で
        // 決める (§3.2 (Fetch) の "the FETCH_OK MUST NOT be sent until the publisher knows
        // whether Largest Object is within the requested range.")。
        let request_end = filter
            .as_ref()
            .and_then(LocationFilter::explicit_end_location);
        let group_order = fetch.parameters.group_order();
        if let Some(order) = group_order {
            // draft §9.19.8 (GROUP ORDER Parameter): 値域外の受信はセッションを閉じる
            if let Err(err) = super::subscription::validation::validate_group_order(order) {
                self.fail(err.clone());
                return Err(err);
            }
        }
        // draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter):
        // 値域外 (0 / 1 以外) の受信は MUST でセッションを INVALID_PARAMETER (0x305) により
        // 閉じる (draft-ietf-moq-transport-23 §12.2 (Session Termination Codes))。
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
            // 送信 Object が要求範囲の内側かを検証するため、解決済みの start を保持する
            fetch_start: start,
            request_end,
            state: FetchState::Pending,
            end_location: None,
            // 初回応答 (受信側) または FETCH_OK 送信時に設定する
            expires_ms: None,
            response_received: false,
            data_stream_finished: false,
            local_cancel_sent: false,
            peer_stop_sending_error_code: None,
            subscriber_priority: fetch.parameters.subscriber_priority(),
            group_order,
            // draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter):
            // peer が FETCH で指定した値を保持し、FETCH_OK 送信時に参照する
            include_properties,
            // publisher 役は応答を送る側であり、受信側の暗黙 gap 判定には使わない。
            // 要求内容の対称性のため保持する。
            has_range_filters: fetch.parameters.has_range_filters(),
        };
        self.fetches.insert(request_id, entry);
        self.request_streams.insert(request_id, RequestKind::Fetch);
        Ok(())
    }

    /// REQUEST_UPDATE を受信できる FETCH かを検証する
    ///
    /// draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE): "An endpoint that receives a
    /// REQUEST_UPDATE other than in the two cases above MUST close the session with
    /// UNEXPECTED_MESSAGE." の MUST を満たす。
    ///
    /// - 送信者: REQUEST_UPDATE を送れるのは元 FETCH の送信者だけであり、自側は publisher
    ///   であること (§9.5 の 2 ケースの 1 つ目に FETCH が含まれる)
    /// - 状態: Established であること。draft §3.2.4 (Fetch State Management) は fetch 状態の
    ///   保持期間と終端手順を定めるが、"Established" という状態は §3.1 (Subscriptions) の
    ///   購読にしか定義されていない。Established を要求する本検証は本実装の判断である。
    ///   Terminated の fetch には更新対象が無く、応答 (REQUEST_ERROR) も送れないため、
    ///   subscription と同様に UNEXPECTED_MESSAGE でセッションを閉じる
    ///   (tests/test_session/fetch/request_update.rs の
    ///   `terminated_fetch_rejects_late_request_update` と同じ判断)
    ///
    /// `locally_terminated` は送信方向を reset 済みの request を表す。応答を送れないため
    /// state 検証のみ緩める (role 検証は応答の有無に依存しないため常に行う)。
    ///
    /// 違反時は session を Closing に遷移させて Err を返す。メッセージ単位の reject
    /// (§8.9 の不正トークン / §3.3.2 の INVALID_FILTER) より先に呼ぶこと。
    pub(super) fn validate_fetch_request_update(
        &mut self,
        request_id: u64,
        locally_terminated: bool,
    ) -> Result<(), SessionError> {
        let fetch = self
            .fetches
            .get(&request_id)
            .expect("locate_request guarantees key presence");
        let my_role = fetch.my_role;
        let state = fetch.state;
        if my_role != TrackRole::Publisher {
            let err = SessionError::new(
                SESSION_ROLE_VIOLATION,
                "REQUEST_UPDATE (fetch) received on subscriber side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        if state != FetchState::Established && !locally_terminated {
            let err = SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "REQUEST_UPDATE requires Established fetch",
            );
            self.fail(err.clone());
            return Err(err);
        }
        Ok(())
    }

    pub(super) fn handle_update_for_fetch(
        &mut self,
        request_id: u64,
        parameters: MessageParameters,
        locally_terminated: bool,
    ) -> Result<(), SessionError> {
        // ローカル終端済みの request (`rejected_request_ids` に記録) は送信方向を reset 済みだが、
        // peer が reset を観測する前に送った REQUEST_UPDATE は正当に届く。その場合は
        // 状態遷移とイベント発行をしない (Request ID / MAX_REQUEST_UPDATES / state・role /
        // auth token / パラメータスコープの検証は呼び出し元 `handle_peer_request_update` で
        // 済んでいる)。
        // 破棄済み (`forget_*` 後) の request への REQUEST_UPDATE は送信者を特定できないため
        // 従来どおり違反になる。
        let fetch = self
            .fetches
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if locally_terminated {
            return Ok(());
        }
        // draft-ietf-moq-transport-23 §9.19.7 (SUBSCRIBER PRIORITY Parameter): REQUEST_UPDATE に SUBSCRIBER_PRIORITY が含まれる場合は更新
        if let Some(priority) = parameters.subscriber_priority() {
            fetch.subscriber_priority = Some(priority);
        }
        // draft-ietf-moq-transport-23 §9.19.8 (GROUP ORDER Parameter): GROUP_ORDER は確定後変更不可 (draft-ietf-moq-transport-23 §5.1.1 (Definitions)) のため更新しない
        self.events.push_back(SessionEvent::RequestUpdateReceived {
            request_id,
            parameters,
        });
        Ok(())
    }

    /// FETCH の初回成功応答 (REQUEST_OK、別名 FETCH_OK) を受信する (subscriber 側)
    ///
    /// draft-ietf-moq-transport-23 §3.2 (Fetch) / §3.2.2 (Gaps in a Fetch Stream) /
    /// §9.3 (REQUEST_OK) / §9.19.16 (EXPIRES Parameter) / §9.19.17 (LARGEST OBJECT
    /// Parameter):
    ///
    /// - 応答範囲の終端は LARGEST_OBJECT が要求範囲内ならその値、そうでなければ要求で
    ///   明示した End Location になる。要求が End を省略している場合の End は
    ///   Largest Object であるため、LARGEST_OBJECT が無ければ終端を確定できない
    /// - EXPIRES は sender が request を終了するまでの時間であり、値は advisory である。
    ///   期限タイマーは起動せず、[`Fetch::expires_ms`] に値を保持する
    /// - Track Properties を運べるのは初回応答だけであり、REQUEST_UPDATE_OK では
    ///   空でなければならない (§9.3)。そのため本関数は初回応答だけを扱う
    /// - Track の終端は FETCH_OK では運ばない。FETCH 応答ストリームの
    ///   End of Group / End of Track で受け取る (§11.4.2.2 (Prior Object Fields))
    pub(super) fn handle_ok_for_fetch_initial(
        &mut self,
        request_id: u64,
        ok: RequestOk,
    ) -> Result<(), SessionError> {
        let Some(fetch) = self.fetches.get(&request_id) else {
            let err = SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "FETCH_OK received for unknown request id",
            );
            self.fail(err.clone());
            return Err(err);
        };
        if fetch.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_ROLE_VIOLATION,
                "FETCH_OK received on publisher side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // 初回応答かどうかは呼び出し元 (`handle_peer_request_ok`) が `response_received` で
        // 判定して振り分ける。ここに来る時点で未受領であることが契約であり、破れていれば
        // 応答の解釈が変わるため UNEXPECTED_MESSAGE で閉じる。
        if fetch.response_received {
            let err = SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "FETCH_OK received after the initial response",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft-ietf-moq-transport-23 §3.2 (Fetch): FETCH_OK / REQUEST_ERROR は
        // データストリームの FIN 後に届いても受理する。`Pending` は `Established` へ遷移させ、
        // `Terminated` は維持する (`Established` に戻すと `fetch_cleanup_ready` の判定により
        // fetch が回収不能になるため)。
        if fetch.state == FetchState::Established {
            let err = SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "FETCH_OK in unexpected fetch state",
            );
            self.fail(err.clone());
            return Err(err);
        }
        let request_start = fetch.fetch_start;
        let request_end = fetch.request_end;
        // draft-ietf-moq-transport-23 §3.2 (Fetch): Largest Object が要求範囲内なら応答は
        // そこへ切り詰められ、LARGEST_OBJECT が必ず載る。範囲外なら要求の End Location が
        // 応答範囲の終端になる (§3.2.2 (Gaps in a Fetch Stream))。
        let largest = ok
            .parameters
            .largest_object()
            .map(|(group_id, object_id)| Location {
                group_id,
                object_id,
            });
        // draft-ietf-moq-transport-23 §3.2 (Fetch): "If no Objects have been published for the
        // track or Start Location is greater than the Largest Object ... the publisher MUST
        // return FETCH_ERROR with error code INVALID_RANGE." Start より小さい LARGEST_OBJECT を
        // 持つ FETCH_OK はこの MUST に反するため、範囲を表す値が許されないものとして
        // INVALID_FIELD (0x301) で閉じる。
        if let (Some(start), Some(largest)) = (request_start, largest)
            && largest < start
        {
            let err = SessionError::new(
                SESSION_INVALID_FIELD,
                "FETCH_OK LARGEST_OBJECT smaller than start location",
            );
            self.fail(err.clone());
            return Err(err);
        }
        let end_location = match (request_end, largest) {
            // Largest Object が要求範囲内 (要求の End 以下) ならそこへ切り詰める
            (Some(end), Some(largest)) if largest <= end => largest,
            (Some(end), _) => end,
            (None, Some(largest)) => largest,
            (None, None) => {
                // 要求が End を省略している場合の End は Largest Object であり、
                // LARGEST_OBJECT が無ければ応答範囲を確定できない。§9.19.17 は
                // LARGEST_OBJECT の省略を「Track に Object が無い」と定義するため、
                // その状態で FETCH_OK を送ること自体が §3.2 の INVALID_RANGE に反する。
                let err = SessionError::new(
                    SESSION_INVALID_PARAMETER,
                    "FETCH_OK has no LARGEST_OBJECT for an open-ended request",
                );
                self.fail(err.clone());
                return Err(err);
            }
        };
        // ローカル終端済みの request はここまでの検証 (送信者・応答範囲) だけを行い、
        // 状態遷移とイベント発行はしない。draft §3.7 (Mandatory to Understand Track
        // Properties) の cancel は送信方向を reset 済みのため不要である。
        if self.is_locally_terminated_request(request_id) {
            return Ok(());
        }
        // draft §3.7 (Mandatory to Understand Track Properties): 未知の必須プロパティを含む FETCH_OK は fetch をキャンセルする
        // FIN 後の `Terminated` でも一律スキップはしない (アプリが unknown mandatory による
        // キャンセルを検知する手段が失われるため)。bidi request stream 終端 (cancel) 経路で
        // `RequestTerminated` を発行済みの場合は 2 回 push されうるため、アプリは冪等に扱うこと。
        if ok.track_properties.has_unknown_mandatory() {
            // 自側が既に bidi 送信方向へ cancel を発行済みなら再発行しない。
            // `FetchState::Terminated` はデータストリームの終端でも遷移し、その場合は
            // bidi request stream が開いたままなので cancel が必要である (§3.2.4
            // (Fetch State Management): "It MUST send STOP_SENDING for the bidi request
            // stream.")。
            let fetch = self
                .fetches
                .get_mut(&request_id)
                .expect("fetch entry presence checked above");
            let local_cancel_sent = fetch.local_cancel_sent;
            fetch.state = FetchState::Terminated;
            fetch.response_received = true;
            // ローカルで fetch をキャンセルするため、request stream GOAWAY の reset deadline も
            // 解除する
            self.clear_request_stream_goaway_deadline(request_id);
            // draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties) / §3.2.4 (Fetch
            // State Management) / §6.4.2.3 (Request Cancellation and Rejection): cancel は
            // 開いている方向をストリーム終端で打ち切る。§3.2.4 は bidi request stream への
            // STOP_SENDING を MUST とする ("It MUST send STOP_SENDING for the bidi request
            // stream.")。エラーコードは §12.5 の CANCELLED を使う。
            if !local_cancel_sent {
                self.events
                    .push_back(SessionEvent::StopSendingRequestStream {
                        request_id,
                        error_code: STREAM_CANCELLED,
                    });
                self.events.push_back(SessionEvent::ResetRequestStream {
                    request_id,
                    error_code: STREAM_CANCELLED,
                });
            }
            self.events.push_back(SessionEvent::RequestTerminated {
                request_id,
                kind: RequestKind::Fetch,
                reason: TerminationReason::LocalCancel,
            });
            return Ok(());
        }
        // draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1
        // (Malformed Tracks) 条件 8 / 9: この実装が意味を理解する Property をスコープ外で
        // 受信した場合と、定義が複数を許す Property を超過して受信した場合に Track を
        // malformed とする (MUST)。fetch を cancel し、同じ Track の subscription も取消
        // 対象にして、アプリへ `MalformedTrack` を通知する (既存の malformed Track 経路)。
        // 未知 Mandatory Track Property の cancel (§3.7) を先に判定するのは、draft が
        // 両条件の優先順位を定めていないためである。
        // データストリームの FIN 後に FETCH_OK が届いて `Terminated` になっている fetch は
        // 既存の malformed 経路が cancel を再発行しないが、応答自体は受領しているため
        // `response_received` を立てる (アプリが応答待ちのままにならないようにする)。
        if let Err(err) = ok.track_properties.validate() {
            self.terminate_malformed_track(request_id, None, err.reason());
            if let Some(fetch) = self.fetches.get_mut(&request_id) {
                fetch.response_received = true;
            }
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
        fetch.end_location = Some(end_location);
        // draft-ietf-moq-transport-23 §9.19.16 (EXPIRES Parameter): FETCH_OK の EXPIRES は
        // sender が request を終了するまでの時間である。期限タイマーは起動せず、利用側が
        // 期限を判断できるように値だけを保持する。0 (期限なし) と不在は `None` に正規化する。
        fetch.expires_ms = ok.parameters.expires();
        // FETCH_OK 受信をアプリへ能動通知する (FIN 前後どちらの到着もここに至る)。
        // 応答範囲の終端と EXPIRES は `fetch()` ポーリングでも参照できるが、
        // イベント駆動のために載せる。
        self.events.push_back(SessionEvent::FetchOkReceived {
            request_id,
            end_location,
            expires_ms: fetch.expires_ms,
        });
        // 応答より先にデータや FIN が届いていた場合、終端が確定したので保留していた
        // 境界依存の gap 判定を行う (draft-ietf-moq-transport-23 §3.2.2
        // (Gaps in a Fetch Stream))。
        self.resolve_pending_fetch_gaps(request_id)?;
        Ok(())
    }

    /// FETCH の REQUEST_UPDATE に対する REQUEST_UPDATE_OK を検証する
    ///
    /// draft-ietf-moq-transport-23 §9.19.17 (LARGEST OBJECT Parameter) の "If the sending
    /// endpoint has published or received any Objects in the Track, it MUST include this
    /// parameter in the above messages it sends." を満たすため、
    /// publisher が観測した Track 最大 Location を `update_largest_object_in_parameters` で
    /// 補完する (利用者指定値と観測最大の大きい方を載せ、指定値を下げない)。
    /// 観測値は publisher 役 subscription の送信記録 (`send_subgroup_object` /
    /// `send_object_datagram`) に限られ、FETCH 応答で送った Object は含まれない。
    pub(super) fn send_ok_for_fetch(
        &mut self,
        request_id: u64,
        parameters: &mut MessageParameters,
    ) -> Result<(), SessionError> {
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
        // fetch の namespace / name は送受信経路で常に設定される契約である
        // (types.rs の Fetch の doc 参照)
        let track_namespace = fetch
            .track_namespace
            .clone()
            .expect("fetch track_namespace is always set");
        let track_name = fetch
            .track_name
            .clone()
            .expect("fetch track_name is always set");
        if let Some(largest) = self.publisher_track_largest(&track_namespace, &track_name) {
            update_largest_object_in_parameters(parameters, &largest);
        }
        Ok(())
    }

    /// FETCH の REQUEST_UPDATE に対する REQUEST_UPDATE_OK を受信する (subscriber 側)
    ///
    /// draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE): REQUEST_UPDATE は複数回送れるため、
    /// 応答も複数回届きうる。初回応答 (FETCH_OK) は
    /// [`handle_ok_for_fetch_initial`](Self::handle_ok_for_fetch_initial) が扱い、
    /// 本関数は初回応答を受理済みの fetch だけを扱う (呼び出し元が `response_received` で
    /// 振り分ける)。Track Properties は REQUEST_UPDATE_OK では空でなければならない (§9.3)。
    ///
    /// データストリームの FIN 後に REQUEST_UPDATE_OK が届くこともあるため、`Terminated` でも
    /// 受理する (REQUEST_OK は bidi request stream で送られ、データストリームの終端とは独立)。
    /// この節番号・規則は draft 由来であり将来 draft 改版で変わる可能性がある。
    pub(super) fn handle_ok_for_fetch_update(
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
                SESSION_ROLE_VIOLATION,
                "REQUEST_UPDATE_OK (fetch) received on publisher side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        if fetch.state == FetchState::Pending {
            // REQUEST_UPDATE は実装が `Established` でのみ送るため、初回応答を受ける前の
            // REQUEST_UPDATE_OK は peer の違反。§3.2 (Fetch) は FETCH への応答を 1 回の
            // REQUEST_OK / REQUEST_ERROR に限り、§9.3 (REQUEST_OK) の REQUEST_UPDATE_OK は
            // 確立済みの要求への応答だからである。
            let err = SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "REQUEST_UPDATE_OK for fetch requires Established or Terminated state",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // ローカル終端済みの request はここまでの検証 (送信者・state) だけを行い、
        // イベント発行はしない (peer が reset を観測する前に送った正当な応答が遅延して届く)
        if self.is_locally_terminated_request(request_id) {
            return Ok(());
        }
        // draft-ietf-moq-transport-23 §9.19.17 (LARGEST OBJECT Parameter): REQUEST_UPDATE_OK で
        // 新しい LARGEST_OBJECT が通知される
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
    /// draft-ietf-moq-transport-23 §9.5.1: REQUEST_UPDATE 失敗時、publisher は
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
    /// draft-ietf-moq-transport-23 §11 (Data Streams and Datagrams): 1 つの FETCH に対する
    /// 応答は 1 本の data stream で送る ("sends Objects matching a FETCH request on one Data Stream")。
    /// したがって `Established` の fetch に対応する outgoing_fetch エントリは 0 or 1 本のみ。
    ///
    /// `Established` 分岐 (REQUEST_UPDATE 失敗応答) は `Terminated` に遷移する
    /// (draft §3.2.4 の "A FETCH_ERROR indicates that both endpoints can immediately
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
        // 応答 (REQUEST_ERROR) は FIN で送られ、自側の送信方向が閉じる
        fetch.local_cancel_sent = true;
        if fetch.my_role != TrackRole::Publisher {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_error (fetch) can only be sent by publisher-role",
            ));
        }
        let result = match fetch.state {
            FetchState::Pending => {
                fetch.state = FetchState::Terminated;
                Ok(None)
            }
            FetchState::Established => {
                // REQUEST_UPDATE 失敗応答: §3.2.4 の破棄許可を行使して Terminated に遷移する
                // (データストリームは §9.5.1 の MUST でリセット必須。以下で Session から
                // 自動発行する)
                fetch.state = FetchState::Terminated;
                // draft §9.5.1: publisher MUST reset the FETCH data stream. Session から
                // 自動発行するため、対応する outgoing_fetch (§3.2.1 が 1 本の unidirectional
                // stream と定める) を除去する。
                let stream_id = self
                    .data_streams
                    .outgoing_fetch
                    .iter()
                    .find_map(|(&sid, s)| (s.request_id == request_id).then_some(sid));
                if let Some(stream_id) = stream_id {
                    self.data_streams.outgoing_fetch.remove(&stream_id);
                    if let Some(fetch) = self.fetches.get_mut(&request_id) {
                        // 終端済み記録 (draft §3.2.4 "It can remove all FETCH state after
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
        };
        // Terminated への遷移 (Ok) では応答を返す経路が残らないため、REQUEST_UPDATE の
        // クレジット会計を解放する (既に Terminated の request は状態を変えない)
        if result.is_ok() {
            self.remove_request_update_credit_entries(request_id);
        }
        result
    }

    pub(super) fn handle_err_for_fetch(&mut self, request_id: u64) -> Result<(), SessionError> {
        // 借用の都合で可変借用より前に判定する
        let locally_terminated = self.is_locally_terminated_request(request_id);
        let fetch = self
            .fetches
            .get_mut(&request_id)
            .expect("locate_request guarantees key presence");
        if fetch.my_role != TrackRole::Subscriber {
            let err = SessionError::new(
                SESSION_ROLE_VIOLATION,
                "REQUEST_ERROR (fetch) received on publisher side",
            );
            self.fail(err.clone());
            return Err(err);
        }
        if locally_terminated {
            return Ok(());
        }
        // draft-ietf-moq-transport-23 §3.2 (Fetch): REQUEST_ERROR は
        // FETCH データストリームの FIN 前後どちらでも届きうるため、state によらず受理する。
        // REQUEST_UPDATE 失敗応答としての REQUEST_ERROR (`Established` 分岐) と
        // FETCH への応答 (`Pending` 遷移 / `Terminated` 受理) は state ベースでは区別不能であり、
        // フラグ判定すると正当な順序 (FETCH_OK 受信 → REQUEST_UPDATE 送信 → FIN →
        // REQUEST_UPDATE 失敗応答) を誤って二重応答として fail させるため、フラグは判定しない。
        // 帰結として REQUEST_ERROR → REQUEST_ERROR の二重応答は検出されない既知の制約
        // (draft §3.2 (Fetch) の "exactly one" は publisher への MUST であり、
        // 受信側にセッションクローズを明示的に要求する文言ではない。将来の draft 改版で
        // 変わる可能性がある)。
        // `Terminated` 分岐でフラグを立てて安全なのは、REQUEST_UPDATE が `Established` でのみ
        // 送信されるため、`Terminated` かつフラグ未設定の fetch への REQUEST_ERROR は必ず
        // FETCH への応答であることによる (REQUEST_UPDATE 失敗応答は `Established` 分岐で
        // 処理され、フラグは FETCH_OK 受信時に設定済み)。
        let mut tolerate_late_stream = false;
        match fetch.state {
            FetchState::Pending => {
                fetch.state = FetchState::Terminated;
                fetch.response_received = true;
                tolerate_late_stream = true;
            }
            FetchState::Established => {
                // REQUEST_UPDATE 失敗応答: §3.2.4 の "A subscriber keeps FETCH state until
                // ... receives FETCH_ERROR" に従い Terminated に遷移する (REQUEST_ERROR
                // を受信したことに変わりはなく、データストリームも §9.5.1 の MUST で
                // publisher 側がリセット済み。RESET が後から届いても `Terminated` のため
                // `recv_data_stream_closed` の dispatch ガードで吸収される)。
                // フラグは FETCH_OK 受信時に設定済みであり、ここでは立てない
                // (REQUEST_ERROR を FETCH への応答として受け取ったわけではないため)。
                fetch.state = FetchState::Terminated;
                tolerate_late_stream = true;
            }
            FetchState::Terminated => {
                fetch.response_received = true;
            }
        }
        if tolerate_late_stream {
            // FETCH_ERROR 受信後に届く in-flight の FETCH data stream を破棄対象として
            // 受理するための許容集合へ登録する (draft-ietf-moq-transport-23 §3.2.4)
            self.record_fetch_error_late_stream(request_id);
        }
        // 失敗応答では応答範囲の終端 (LARGEST_OBJECT) が来ないため、保留中の gap 判定を
        // 破棄する。
        self.pending_fetch_tails.remove(&request_id);
        // 失敗応答の受信で fetch が終端したため、REQUEST_UPDATE のクレジット会計を解放する
        // (draft-ietf-moq-transport-23 §9.5.1 (Updating Subscriptions): 失敗応答を受けた
        // fetch は Terminated になり、以後の応答は返らない)。
        self.remove_request_update_credit_entries(request_id);
        Ok(())
    }
}
