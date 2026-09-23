//! `shiguredo_moqt::session::core::Session` を tokio I/O 層と結線する MoQT クライアント
//!
//! sans-I/O な [`Session`] を核に、QUIC / WebTransport の制御ストリームと
//! bidi request stream を駆動する。publisher / subscriber の双方が共有する。
//!
//! # 構造
//!
//! - control stream: uni 送信用 + uni 受信用の 1 ペア (draft-ietf-moq-transport-21 §6.3 (Session initialization))
//! - bidi request stream: PUBLISH / SUBSCRIBE / FETCH ごとに 1 本、`bidi_sends` に保持する
//! - data stream: 各バイナリが [`DataPlaneHandle`] 経由で `Session` に通知する
//! - 受信は `tokio::spawn` した task で行い、`mpsc` 経由でメインループに集約する

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex as StdMutex};

use bytes::Bytes;
use tokio::sync::{Mutex as TokioMutex, mpsc, oneshot};

use shiguredo_moqt::decoder::MessageDecoder;
use shiguredo_moqt::session::types::{
    DatagramAcceptance, FetchState, RecvDataStreamError, SendRequestError, SubscriptionState,
    TrackDataAcceptance,
};
use shiguredo_moqt::stream::encode_control_stream_setup;
use shiguredo_moqt::{
    message::ControlMessage, message::ReasonPhrase, message::common::Location,
    message::common::TrackNamespace, message_parameter::MessageParameter,
    message_parameter::MessageParameterValue, message_parameter::MessageParameters,
    message_parameter::PARAM_SUBSCRIBER_PRIORITY, session::core::Session,
    session::types::DataStreamId, session::types::DataStreamResetReason,
    session::types::RequestStreamEnd, session::types::SessionEvent, session::types::SessionState,
    session::types::Transport as MoqtTransport, stream::decoder::DecodedSubgroupObject,
    stream::fetch::FetchHeader, stream::fetch::FetchPriorContext, stream::fetch::FetchStreamEntry,
    stream::fetch::FetchStreamObject, stream::fetch::FetchSubgroupIdMode,
    stream::subgroup::SubgroupHeader, track_properties::TrackProperties,
};

use crate::error::{Result, TransportError};
use crate::transport::{
    BidiStreamAcceptor, RecvChunk, RecvStream, SendStream, StreamAcceptor, StreamHandle,
};
use crate::webtransport::WtSession;

/// デフォルトの Subscriber Priority
const DEFAULT_SUBSCRIBER_PRIORITY: u8 = 128;

/// オブジェクト送信前のフィルタ評価の結果
///
/// draft-ietf-moq-transport-21 §3.3.3 (Combining Filters): "The publisher MUST forward only
/// objects that pass all filters"。フィルタ不通過のオブジェクトはワイヤに出さずスキップする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "Skip の場合はワイヤ送信を省略する必要がある"]
pub enum ObjectFilterOutcome {
    /// フィルタを通過したため、ワイヤへ送信してよい
    Pass,
    /// フィルタ不通過 (SendRequestError::LocalFilterMismatch)。ワイヤ送信をスキップする
    Skip,
}

/// bidi ストリームからメインループへの通知
///
/// 自側が開始した request stream 上のメッセージ (`Response`) と、peer (MOQT relay) が
/// 開始した request stream の先頭メッセージ (`PeerRequest`) を同じチャネルで運ぶ。
/// 同一チャネルに載せることで、ある request stream の先頭メッセージと後続メッセージの
/// 処理順が入れ替わらないことを保証する (先頭メッセージの登録より前に後続メッセージを
/// 処理すると未知の Request ID として扱われる)。
pub(crate) enum BidiMessage {
    /// 自側が開始した request stream 上のメッセージ (応答または追加メッセージ)
    Response(u64, Result<StreamRead<ControlMessage>>),
    /// peer が開始した request stream の先頭メッセージ
    ///
    /// メインループが `Session::recv_request` で受理した後、`send` を応答用の
    /// 送信半として、`stop_tx` を受信方向の STOP_SENDING 依頼用として登録する。
    PeerRequest {
        /// 開始メッセージに含まれる Request ID
        request_id: u64,
        /// 開始メッセージ (SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS)
        message: ControlMessage,
        /// 応答を書き戻すための送信半
        send: SendStream,
        /// 受信方向の STOP_SENDING を依頼するチャネル
        stop_tx: mpsc::Sender<StopSendingCommand>,
    },
}

/// peer (MOQT relay) から届いた要求
#[derive(Debug)]
pub struct IncomingRequest {
    /// 要求の Request ID
    pub request_id: u64,
    /// 要求メッセージ
    pub message: ControlMessage,
}

/// peer (MOQT relay) から届いた REQUEST_UPDATE
///
/// draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE):
/// 「The receiver of a REQUEST_UPDATE MUST respond with exactly one REQUEST_OK or
///  REQUEST_ERROR message indicating if the update was successful, unless it is
///  coalescing failed updates」
/// 応答は [`MoqtClient::send_request_ok`] / [`MoqtClient::send_request_error`] で行う。
/// パラメータの検証 (scope / Range Filter / FORWARD 値域) は session 層が受信時に
/// 済ませており、ここに来た更新は購読状態へ反映済みである。
#[derive(Debug)]
pub struct IncomingRequestUpdate {
    /// 対象 request の Request ID
    pub request_id: u64,
    /// 受信したパラメータ (購読の累積パラメータ)
    pub parameters: MessageParameters,
}

/// [`MoqtClient::next_event`] が返すイベント
#[derive(Debug)]
pub enum ClientEvent {
    /// session の注目イベント (GOAWAY / close / PUBLISH_DONE など)
    Session(SessionEvent),
    /// peer から届いた要求 (relay が転送した SUBSCRIBE / FETCH など)
    Request(IncomingRequest),
    /// peer から届いた REQUEST_UPDATE (確立済み購読の変更要求)
    RequestUpdate(IncomingRequestUpdate),
}

/// bidi 受信タスクへの STOP_SENDING 指示
pub(crate) struct StopSendingCommand {
    /// Stream Reset Error Code (draft-ietf-moq-transport-21 §12.5 (Stream Reset Error Codes))
    error_code: u64,
    /// 受信半への STOP_SENDING 送出結果の完了通知
    ack: oneshot::Sender<Result<()>>,
}

/// 制御ストリーム受信タスクからメインループへの通知
pub type ControlIncoming = Result<StreamRead<ControlMessage>>;

/// SUBSCRIBE_OK の結果
pub struct SubscribeResult {
    pub request_id: u64,
    pub track_alias: u64,
}

/// FETCH_OK の結果
pub struct FetchResult {
    pub request_id: u64,
}

/// 制御ストリームから読み出した値または終端
#[derive(Debug)]
pub enum StreamRead<T> {
    Value(T),
    Closed(RequestStreamEnd),
}

/// バッファ付き制御ストリームリーダー
pub struct ControlStream {
    stream: RecvStream,
    decoder: MessageDecoder,
}

impl ControlStream {
    pub fn new(stream: RecvStream) -> Self {
        Self {
            stream,
            decoder: MessageDecoder::new(),
        }
    }

    /// ストリーム先頭のストリームタイプ varint を読む (draft-ietf-moq-transport-21 §6.4.1 (Unidirectional Streams))
    pub async fn read_stream_type(&mut self) -> Result<StreamRead<u64>> {
        loop {
            if let Some(val) = self.decoder.try_decode_varint()? {
                return Ok(StreamRead::Value(val));
            }
            match self.stream.receive_chunk().await? {
                RecvChunk::Data(data) => self.decoder.push(&data),
                RecvChunk::End(end) => return Ok(StreamRead::Closed(end)),
            }
        }
    }

    /// 制御メッセージを 1 つ読む。終端なら `Closed` を返す
    pub async fn recv_message(&mut self) -> Result<StreamRead<ControlMessage>> {
        loop {
            if let Some(msg) = self.decoder.try_decode_message()? {
                return Ok(StreamRead::Value(msg));
            }
            match self.stream.receive_chunk().await? {
                RecvChunk::Data(data) => self.decoder.push(&data),
                RecvChunk::End(end) => return Ok(StreamRead::Closed(end)),
            }
        }
    }

    /// 受信方向へ STOP_SENDING を送出する
    ///
    /// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection): 受信方向の
    /// cancel は STOP_SENDING で行う。error code は §12.5 (Stream Reset Error Codes) から選ぶ。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fn stop_sending(&mut self, error_code: u64) -> Result<()> {
        self.stream.stop_sending(error_code)
    }
}

/// data stream から Session の data plane を駆動するためのハンドル
#[derive(Clone)]
pub struct DataPlaneHandle {
    session: Arc<StdMutex<Session>>,
}

impl DataPlaneHandle {
    fn new(session: Arc<StdMutex<Session>>) -> Self {
        Self { session }
    }

    /// subgroup stream open を Session に通知する
    pub fn send_subgroup_header(
        &self,
        stream_id: DataStreamId,
        request_id: u64,
        header: &SubgroupHeader,
    ) -> Result<()> {
        let mut session = lock_session(&self.session);
        session
            .send_subgroup_header(stream_id, request_id, header)
            .map_err(|e| TransportError::Internal(format!("send_subgroup_header: {e}")))
    }

    /// subgroup object を Session に通知し、フィルタ評価の結果を返す
    ///
    /// `SendRequestError::LocalFilterMismatch` (フィルタ不通過) は `Skip` として返し、
    /// 呼び出し側はワイヤ送信をスキップする。その他のエラーは伝播する。
    pub fn send_subgroup_object(
        &self,
        stream_id: DataStreamId,
        object_id: u64,
        properties_bytes: Option<&[u8]>,
    ) -> Result<ObjectFilterOutcome> {
        let mut session = lock_session(&self.session);
        match session.send_subgroup_object(stream_id, object_id, properties_bytes) {
            Ok(()) => Ok(ObjectFilterOutcome::Pass),
            Err(SendRequestError::LocalFilterMismatch) => Ok(ObjectFilterOutcome::Skip),
            Err(e) => Err(TransportError::Internal(format!(
                "send_subgroup_object: {e}"
            ))),
        }
    }

    /// Object Datagram 送信を Session に通知し、フィルタ評価の結果を返す
    ///
    /// `SendRequestError::LocalFilterMismatch` (フィルタ不通過) は `Skip` として返し、
    /// 呼び出し側はワイヤ送信をスキップする。その他のエラーは伝播する。
    pub fn send_object_datagram(
        &self,
        request_id: u64,
        group_id: u64,
        object_id: u64,
        properties_data: Option<Vec<u8>>,
        status: Option<u64>,
    ) -> Result<ObjectFilterOutcome> {
        let mut session = lock_session(&self.session);
        match session.send_object_datagram(request_id, group_id, object_id, properties_data, status)
        {
            Ok(()) => Ok(ObjectFilterOutcome::Pass),
            Err(SendRequestError::LocalFilterMismatch) => Ok(ObjectFilterOutcome::Skip),
            Err(e) => Err(TransportError::Internal(format!(
                "send_object_datagram: {e}"
            ))),
        }
    }

    /// uni data stream close を Session に通知する (publisher 側)
    pub fn send_data_stream_closed(
        &self,
        stream_id: DataStreamId,
        end: RequestStreamEnd,
    ) -> Result<()> {
        let mut session = lock_session(&self.session);
        session
            .send_data_stream_closed(stream_id, end)
            .map_err(|e| TransportError::Internal(format!("send_data_stream_closed: {e}")))
    }

    /// FETCH 応答用 uni stream の open を Session に通知する (publisher 側)
    ///
    /// draft-ietf-moq-transport-21 §11.4.1 (Fetch Header): FETCH_OK を返した publisher は
    /// 要求の Request ID を載せた FETCH_HEADER で応答 stream を開く。
    pub fn send_fetch_header(&self, stream_id: DataStreamId, request_id: u64) -> Result<()> {
        let mut session = lock_session(&self.session);
        session
            .send_fetch_header(stream_id, request_id)
            .map_err(|e| TransportError::Internal(format!("send_fetch_header: {e}")))
    }

    /// FETCH 応答 stream 上の Object 送信を Session に通知する (publisher 側)
    pub fn send_fetch_object(&self, stream_id: DataStreamId) -> Result<()> {
        let mut session = lock_session(&self.session);
        session
            .send_fetch_object(stream_id)
            .map_err(|e| TransportError::Internal(format!("send_fetch_object: {e}")))
    }

    /// FETCH 応答 stream の終端を Session に通知する (publisher 側)
    pub fn send_fetch_data_stream_closed(&self, stream_id: DataStreamId) -> Result<()> {
        let mut session = lock_session(&self.session);
        session
            .send_fetch_data_stream_closed(stream_id)
            .map_err(|e| TransportError::Internal(format!("send_fetch_data_stream_closed: {e}")))
    }

    /// uni data stream の type を Session に通知する (subscriber 側)
    pub fn recv_data_stream_type(&self, stream_id: DataStreamId, stream_type: u64) -> Result<()> {
        let mut session = lock_session(&self.session);
        match session.recv_data_stream_type(stream_id, stream_type) {
            Ok(_) => Ok(()),
            Err(RecvDataStreamError::BeforeSessionEstablished) => Err(TransportError::Internal(
                "data stream arrived before session established".to_string(),
            )),
            Err(RecvDataStreamError::InvalidInput(e) | RecvDataStreamError::Session(e)) => Err(
                TransportError::Internal(format!("recv_data_stream_type: {e}")),
            ),
        }
    }

    /// subgroup header を Session に通知する (subscriber 側)
    pub fn recv_subgroup_header(
        &self,
        stream_id: DataStreamId,
        header: &SubgroupHeader,
    ) -> Result<TrackDataAcceptance> {
        let mut session = lock_session(&self.session);
        session
            .recv_subgroup_header(stream_id, header)
            .map_err(|e| TransportError::Internal(format!("recv_subgroup_header: {e}")))
    }

    /// subgroup object を Session に通知する (subscriber 側)
    ///
    /// 戻り値は帰属判定 (`Accepted` / `FilteredOut` / `Discarded`)。`FilteredOut` /
    /// `Discarded` の object も wire 上は payload を持つため、呼び出し側は payload を
    /// 読み出して消費する。header 受理済み stream では `UnknownTrackAlias` を返さない。
    pub fn recv_subgroup_object(
        &self,
        stream_id: DataStreamId,
        object: &DecodedSubgroupObject,
    ) -> Result<TrackDataAcceptance> {
        let mut session = lock_session(&self.session);
        session
            .recv_subgroup_object(stream_id, object)
            .map_err(|e| TransportError::Internal(format!("recv_subgroup_object: {e}")))
    }

    /// fetch header を Session に通知する (subscriber 側)
    pub fn recv_fetch_header(&self, stream_id: DataStreamId, header: &FetchHeader) -> Result<()> {
        let mut session = lock_session(&self.session);
        session
            .recv_fetch_header(stream_id, header)
            .map_err(|e| TransportError::Internal(format!("recv_fetch_header: {e}")))
    }

    /// fetch entry を Session に通知する (subscriber 側)
    ///
    /// entry 到着の通知であり、内容の検証はデコード時に済んでいる。data stream の
    /// activity 更新 (DATA_STREAM_TIMEOUT の対象) は tick 実行後のみ行われ、
    /// 最初の tick 前 (受信開始直後) に呼んでも更新されない (時刻の起点が無いため)。
    pub fn recv_fetch_entry(&self, stream_id: DataStreamId) -> Result<()> {
        let mut session = lock_session(&self.session);
        session
            .recv_fetch_entry(stream_id)
            .map_err(|e| TransportError::Internal(format!("recv_fetch_entry: {e}")))
    }

    /// uni data stream close を Session に通知する (subscriber 側)
    pub fn recv_data_stream_closed(
        &self,
        stream_id: DataStreamId,
        end: RequestStreamEnd,
    ) -> Result<()> {
        let mut session = lock_session(&self.session);
        session
            .recv_data_stream_closed(stream_id, end)
            .map_err(|e| TransportError::Internal(format!("recv_data_stream_closed: {e}")))
    }

    /// data stream へ STOP_SENDING を送信する (subscriber 側)
    pub fn send_data_stream_stop_sending(&self, stream_id: DataStreamId) -> Result<()> {
        let mut session = lock_session(&self.session);
        session
            .send_data_stream_stop_sending(stream_id)
            .map_err(|e| TransportError::Internal(format!("send_data_stream_stop_sending: {e}")))
    }

    /// FETCH データストリームの終端を Session に通知する (subscriber 側)
    pub fn recv_fetch_data_stream_closed(
        &self,
        request_id: u64,
        end: RequestStreamEnd,
    ) -> Result<()> {
        let mut session = lock_session(&self.session);
        session
            .recv_fetch_data_stream_closed(request_id, end)
            .map_err(|e| TransportError::Internal(format!("recv_fetch_data_stream_closed: {e}")))
    }

    /// 受信 datagram を Session に渡す (subscriber 側)
    ///
    /// type 分岐は Session が行う。未知 type は draft-ietf-moq-transport-21 §11 (Data Streams and Datagrams) に従い
    /// セッションクローズになるため、I/O 層は decode せずそのまま渡す。
    pub fn recv_datagram(&self, raw: &[u8]) -> Result<DatagramAcceptance> {
        let mut session = lock_session(&self.session);
        session
            .recv_datagram(raw)
            .map_err(|e| TransportError::Internal(format!("recv_datagram: {e}")))
    }
}

/// publisher / subscriber 共通の MoQT クライアント (Session ドライバ)
pub struct MoqtClient {
    session: Arc<StdMutex<Session>>,
    control_send: SendStream,
    handle: StreamHandle,
    bidi_sends: HashMap<u64, SendStream>,
    /// bidi 受信タスクへ STOP_SENDING の送出を依頼するチャネル (request_id 単位、値は error code と完了通知)
    bidi_stop_txs: HashMap<u64, mpsc::Sender<StopSendingCommand>>,
    closed_request_streams: HashSet<u64>,
    /// peer から届いた未処理の要求 (relay が転送した SUBSCRIBE / FETCH など)
    incoming_requests: VecDeque<IncomingRequest>,
    /// peer から届いた未処理の REQUEST_UPDATE
    incoming_updates: VecDeque<IncomingRequestUpdate>,
    /// drain_events が Session から取り出した notable イベント
    ///
    /// `drain_events` は受信メッセージを契機に生成されたイベントも一緒に poll するため、
    /// アプリが観測するイベントをその場で捨てないようここへ移す。
    notable_events: VecDeque<SessionEvent>,
    control_rx: mpsc::Receiver<ControlIncoming>,
    bidi_tx: mpsc::Sender<BidiMessage>,
    bidi_rx: mpsc::Receiver<BidiMessage>,
    task_monitor: tokio_metrics::TaskMonitor,
}

/// アプリ (`next_event`) が観測する notable イベントか
///
/// `drain_events` はアプリより先に `Session` のイベントを poll する。ここに挙げた
/// イベントを `drain_events` が捨てると、`next_event` は二度と取り出せない。
fn is_notable_event(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::GoawayReceived { .. }
            | SessionEvent::CloseSession(_)
            | SessionEvent::PublishDoneReceived { .. }
    )
}

/// `Session` を排他ロックして取り出す
///
/// `Session` は sans-I/O な状態機械で、control / bidi / data plane の複数の非同期タスクから
/// 共有される。各操作は await をまたがず短時間で完結するため、チャネルで単一所有者へ
/// 操作を依頼する構成より `Mutex` の方が状態の一貫性を保ったまま構成を単純にできる。
fn lock_session(session: &Arc<StdMutex<Session>>) -> std::sync::MutexGuard<'_, Session> {
    session.lock().expect("session mutex poisoned")
}

/// 制御ストリームの受信 task を起動する
fn spawn_control_recv_task(
    mut stream: ControlStream,
    task_monitor: &tokio_metrics::TaskMonitor,
) -> (mpsc::Receiver<ControlIncoming>, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel::<ControlIncoming>(16);
    let handle = tokio::spawn(task_monitor.clone().instrument(async move {
        loop {
            match stream.recv_message().await {
                Ok(message) => {
                    let is_closed = matches!(message, StreamRead::Closed(_));
                    if tx.send(Ok(message)).await.is_err() || is_closed {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    break;
                }
            }
        }
    }));
    (rx, handle)
}

/// bidi request stream の受信 task を起動する
///
/// メインループから停止指示 (`mpsc`) を受けると、受信半に STOP_SENDING を送出し、
/// その結果を `ack` で返して終了する。
fn spawn_bidi_recv_task(
    request_id: u64,
    mut stream: ControlStream,
    tx: mpsc::Sender<BidiMessage>,
    mut stop_rx: mpsc::Receiver<StopSendingCommand>,
    task_monitor: &tokio_metrics::TaskMonitor,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(task_monitor.clone().instrument(async move {
        loop {
            tokio::select! {
                biased;
                command = stop_rx.recv() => {
                    // 停止指示を受けたら受信半に STOP_SENDING を送出し、結果を依頼元へ返す
                    if let Some(command) = command {
                        let result = stream.stop_sending(command.error_code);
                        let _ = command.ack.send(result);
                    }
                    break;
                }
                message = stream.recv_message() => {
                    match message {
                        Ok(message) => {
                            let is_closed = matches!(message, StreamRead::Closed(_));
                            if tx.send(BidiMessage::Response(request_id, Ok(message))).await.is_err() || is_closed {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(BidiMessage::Response(request_id, Err(e))).await;
                            break;
                        }
                    }
                }
            }
        }
    }))
}

/// peer (MOQT relay) が開始する bidi request stream を受け入れる task を起動する
///
/// relay は subscriber の SUBSCRIBE / FETCH を publisher 側 session の新しい
/// request stream として転送する (draft-ietf-moq-transport-21 §6.3 (Session initialization))。
/// ストリームごとに reader task を起こし、先頭メッセージを
/// [`BidiMessage::PeerRequest`] としてメインループへ渡す。以降のメッセージは
/// 通常の bidi メッセージと同じ経路で流す。
fn spawn_peer_bidi_accept_task(
    mut acceptor: BidiStreamAcceptor,
    tx: mpsc::Sender<BidiMessage>,
    task_monitor: &tokio_metrics::TaskMonitor,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(task_monitor.clone().instrument(async move {
        loop {
            let pair = match acceptor.accept_bidi_stream().await {
                Ok(Some(pair)) => pair,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("Failed to accept peer bidi stream: {e}");
                    break;
                }
            };
            let (send, recv) = pair;
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut stream = ControlStream::new(recv);
                // 先頭メッセージが request の開始メッセージである
                let first = match stream.recv_message().await {
                    Ok(StreamRead::Value(message)) => message,
                    Ok(StreamRead::Closed(_)) => return,
                    Err(e) => {
                        tracing::warn!("Failed to read peer request start message: {e}");
                        return;
                    }
                };
                let Some(request_id) = request_id_of(&first) else {
                    tracing::warn!("Peer request start message is not a request: {first:?}");
                    return;
                };
                let (stop_tx, mut stop_rx) = mpsc::channel::<StopSendingCommand>(1);
                if tx
                    .send(BidiMessage::PeerRequest {
                        request_id,
                        message: first,
                        send,
                        stop_tx,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                loop {
                    tokio::select! {
                        biased;
                        command = stop_rx.recv() => {
                            if let Some(command) = command {
                                let result = stream.stop_sending(command.error_code);
                                let _ = command.ack.send(result);
                            }
                            break;
                        }
                        message = stream.recv_message() => {
                            match message {
                                Ok(message) => {
                                    let is_closed = matches!(message, StreamRead::Closed(_));
                                    if tx
                                        .send(BidiMessage::Response(request_id, Ok(message)))
                                        .await
                                        .is_err()
                                        || is_closed
                                    {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    let _ = tx
                                        .send(BidiMessage::Response(request_id, Err(e)))
                                        .await;
                                    break;
                                }
                            }
                        }
                    }
                }
            });
        }
    }))
}

/// request の開始メッセージから Request ID を取り出す
///
/// bidi request stream の先頭メッセージとして許されるのは SUBSCRIBE / PUBLISH /
/// FETCH / TRACK_STATUS の 4 種類である
/// (draft-ietf-moq-transport-21 §6.3 (Session initialization))。
fn request_id_of(message: &ControlMessage) -> Option<u64> {
    match message {
        ControlMessage::Subscribe(m) => Some(m.request_id),
        ControlMessage::Publish(m) => Some(m.request_id),
        ControlMessage::Fetch(m) => Some(m.request_id),
        ControlMessage::TrackStatus(m) => Some(m.request_id),
        _ => None,
    }
}

impl MoqtClient {
    /// QUIC 直接接続で MoQT client を確立する
    /// (draft-ietf-moq-transport-21 §6.2 (Session establishment) / §6.3 (Session initialization))
    ///
    /// 戻り値の [`StreamAcceptor`] は data stream を受信する側 (subscriber) が使う。
    /// peer 起動の request stream は MoqtClient 内部の受理タスクが受け取るため、
    /// publisher も本 API を使える。
    pub async fn establish_quic(
        connection: s2n_quic::connection::Connection,
        path: &str,
        authority: &str,
        impl_name: &str,
        task_monitor: &tokio_metrics::TaskMonitor,
    ) -> Result<(Self, StreamAcceptor)> {
        let (handle, acceptor) = connection.split();
        let (bidi_acceptor, mut recv_acceptor) = acceptor.split();
        let handle = StreamHandle::Quic(handle);

        // 自側 uni stream (control 送信側) を開く
        let mut control_send = handle.open_send_stream().await?;

        // Session を生成 (初期の SendControl(Setup) イベントが積まれる)
        let options = crate::build_setup_options(Some(path), Some(authority), impl_name);
        let mut session = Session::new_client(MoqtTransport::Quic, options)?;

        // SETUP を取り出して送信する
        let setup_msg = pop_send_control(&mut session)?;
        send_control_stream_setup(&mut control_send, &setup_msg).await?;
        tracing::info!("Sent SETUP message");

        // peer uni stream を受信して stream type を検証
        let recv_stream = recv_acceptor
            .accept_receive_stream()
            .await
            .map_err(|e| TransportError::Quic(format!("failed to accept control stream: {e}")))?
            .ok_or_else(|| {
                TransportError::Quic("connection closed before control stream".into())
            })?;
        let mut control_recv = ControlStream::new(RecvStream::Quic(recv_stream));
        let stream_type = match control_recv.read_stream_type().await? {
            StreamRead::Value(stream_type) => stream_type,
            StreamRead::Closed(end) => {
                let _ = session.recv_control_stream_closed(end);
                return Err(TransportError::Internal(
                    "control stream closed before stream type".into(),
                ));
            }
        };
        session.recv_control_stream_type(stream_type)?;

        let client = Self::finish_handshake(
            session,
            control_send,
            handle,
            control_recv,
            BidiStreamAcceptor::Quic(bidi_acceptor),
            task_monitor,
        )
        .await?;
        Ok((client, StreamAcceptor::Quic(recv_acceptor)))
    }

    /// WebTransport 接続で MoQT client を確立する
    ///
    /// draft-ietf-moq-transport-21 §9.1.1 (AUTHORITY) / §9.1.2 (PATH) により
    /// WebTransport 使用時は PATH (0x01) と AUTHORITY (0x05) を送信してはならない。
    ///
    /// 戻り値の [`StreamAcceptor`] は data stream を受信する側 (subscriber) が使う。
    /// peer 起動の request stream は MoqtClient 内部の受理タスクが受け取るため、
    /// publisher も本 API を使える。
    pub async fn establish_wt(
        wt_session: WtSession,
        impl_name: &str,
        task_monitor: &tokio_metrics::TaskMonitor,
    ) -> Result<(Self, StreamAcceptor)> {
        let shared = Arc::new(TokioMutex::new(wt_session));
        let handle = StreamHandle::WebTransport(shared.clone());
        // 受信ストリームの receiver は acceptor が持ち、accept の待機中に
        // session のロックを保持しないようにする (`WtSession::take_uni_receiver`)。
        let (wt_uni_rx, wt_bi_rx) = {
            let mut s = shared.lock().await;
            let uni_rx = s.take_uni_receiver().ok_or_else(|| {
                TransportError::Internal("uni stream receiver already taken".into())
            })?;
            let bi_rx = s.take_bi_receiver().ok_or_else(|| {
                TransportError::Internal("bidi stream receiver already taken".into())
            })?;
            (uni_rx, bi_rx)
        };

        let wt_send = {
            let mut s = shared.lock().await;
            s.open_uni_stream().await?
        };
        let mut control_send = SendStream::WebTransport(wt_send);

        let options = crate::build_setup_options(None, None, impl_name);
        let mut session = Session::new_client(MoqtTransport::WebTransport, options)?;

        let setup_msg = pop_send_control(&mut session)?;
        send_control_stream_setup(&mut control_send, &setup_msg).await?;
        tracing::info!("Sent SETUP message");

        let wt_recv = {
            let mut s = shared.lock().await;
            s.accept_uni_stream().await?
        };
        let mut control_recv = ControlStream::new(RecvStream::WebTransport(wt_recv));
        let stream_type = match control_recv.read_stream_type().await? {
            StreamRead::Value(stream_type) => stream_type,
            StreamRead::Closed(end) => {
                let _ = session.recv_control_stream_closed(end);
                return Err(TransportError::Internal(
                    "control stream closed before stream type".into(),
                ));
            }
        };
        session.recv_control_stream_type(stream_type)?;

        let client = Self::finish_handshake(
            session,
            control_send,
            handle,
            control_recv,
            BidiStreamAcceptor::WebTransport {
                session: shared.clone(),
                bi_rx: wt_bi_rx,
            },
            task_monitor,
        )
        .await?;
        Ok((
            client,
            StreamAcceptor::WebTransport {
                session: shared,
                uni_rx: wt_uni_rx,
            },
        ))
    }

    async fn finish_handshake(
        mut session: Session,
        control_send: SendStream,
        handle: StreamHandle,
        mut control_recv: ControlStream,
        bidi_acceptor: BidiStreamAcceptor,
        task_monitor: &tokio_metrics::TaskMonitor,
    ) -> Result<Self> {
        // peer SETUP 受信
        let peer_setup = match control_recv.recv_message().await? {
            StreamRead::Value(message) => message,
            StreamRead::Closed(end) => {
                let _ = session.recv_control_stream_closed(end);
                return Err(TransportError::Internal(
                    "control stream closed before SETUP".into(),
                ));
            }
        };
        session.recv_control(peer_setup)?;
        if session.state() != SessionState::Established {
            return Err(TransportError::Internal(
                "session not established after SETUP exchange".into(),
            ));
        }
        // Established イベントは drain しておく
        while let Some(ev) = session.poll_event() {
            if matches!(ev, SessionEvent::Established) {
                tracing::info!("Session established");
                break;
            }
        }

        let session = Arc::new(StdMutex::new(session));

        // 制御 stream の受信を task 化
        let (control_rx, _) = spawn_control_recv_task(control_recv, task_monitor);
        let (bidi_tx, bidi_rx) = mpsc::channel::<BidiMessage>(64);

        // peer (relay) 起動の request stream の受理を task 化
        let _peer_bidi_task =
            spawn_peer_bidi_accept_task(bidi_acceptor, bidi_tx.clone(), task_monitor);

        let task_monitor = task_monitor.clone();

        Ok(Self {
            session,
            control_send,
            handle,
            bidi_sends: HashMap::new(),
            bidi_stop_txs: HashMap::new(),
            closed_request_streams: HashSet::new(),
            incoming_requests: VecDeque::new(),
            incoming_updates: VecDeque::new(),
            notable_events: VecDeque::new(),
            control_rx,
            bidi_tx,
            bidi_rx,
            task_monitor,
        })
    }

    /// ストリーム開設用ハンドルのクローン
    pub fn handle(&self) -> StreamHandle {
        self.handle.clone()
    }

    /// data plane 通知用ハンドルのクローン
    pub fn data_plane(&self) -> DataPlaneHandle {
        DataPlaneHandle::new(Arc::clone(&self.session))
    }

    /// PUBLISH を発行し、REQUEST_OK または REQUEST_ERROR が返るまで待つ (publisher 側)
    pub async fn publish_track(
        &mut self,
        namespace: TrackNamespace,
        track_name: Vec<u8>,
        track_alias: u64,
    ) -> Result<u64> {
        let request_id = {
            let mut session = lock_session(&self.session);
            session
                .send_publish(
                    namespace,
                    track_name,
                    track_alias,
                    MessageParameters::new(),
                    TrackProperties::new(),
                )
                .map_err(|e| TransportError::Internal(format!("send_publish: {e}")))?
        };
        self.drain_events().await?;
        tracing::info!(
            "Sent PUBLISH (request_id={}, alias={})",
            request_id,
            track_alias
        );
        self.wait_subscription_resolved(request_id).await?;
        Ok(request_id)
    }

    /// PUBLISH_DONE を送信する (publisher 側)
    /// (draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE))
    pub async fn send_publish_done(
        &mut self,
        request_id: u64,
        status_code: u64,
        reason: &str,
    ) -> Result<()> {
        let reason = ReasonPhrase::new(reason)?;
        let stream_count = {
            let session = lock_session(&self.session);
            session
                .subscription(request_id)
                .map(|sub| sub.stream_counts.published_count)
                .ok_or_else(|| {
                    TransportError::Internal(format!("subscription not found: {request_id}"))
                })?
        };
        {
            let mut session = lock_session(&self.session);
            session
                .send_publish_done(request_id, status_code, stream_count, reason)
                .map_err(|e| TransportError::Internal(format!("send_publish_done: {e}")))?;
        }
        self.drain_events().await?;
        // bidi 送信側の後始末は不要である。PUBLISH_DONE は subscription の最終メッセージであり、
        // 直前の drain_events が SendOnStream { fin: true } を処理する時点で送信半は FIN 済みで
        // 台帳 (bidi_sends) から外れている。ここで取り出しても常に None になる。
        tracing::info!("Sent PUBLISH_DONE (request_id={request_id}, stream_count={stream_count})");
        Ok(())
    }

    /// トラックを SUBSCRIBE する (subscriber 側)
    pub async fn subscribe_track(
        &mut self,
        namespace: TrackNamespace,
        track_name: Vec<u8>,
    ) -> Result<SubscribeResult> {
        let mut parameters = MessageParameters::new();
        parameters.push(MessageParameter {
            param_type: PARAM_SUBSCRIBER_PRIORITY,
            value: MessageParameterValue::Uint8(DEFAULT_SUBSCRIBER_PRIORITY),
        });
        let request_id = {
            let mut session = lock_session(&self.session);
            session
                .send_subscribe(namespace, track_name, parameters)
                .map_err(|e| TransportError::Internal(format!("send_subscribe: {e}")))?
        };
        self.drain_events().await?;
        self.wait_subscription_resolved(request_id).await?;
        let (request_id, track_alias) = {
            let session = lock_session(&self.session);
            let sub = session
                .subscription(request_id)
                .ok_or_else(|| TransportError::Internal("subscription disappeared".into()))?;
            let track_alias = sub.track_alias.ok_or_else(|| {
                TransportError::Internal("SUBSCRIBE_OK without track_alias".into())
            })?;
            (sub.request_id, track_alias)
        };
        tracing::info!("SUBSCRIBE_OK: request_id={request_id}, alias={track_alias}");
        Ok(SubscribeResult {
            request_id,
            track_alias,
        })
    }

    /// FETCH を発行する (subscriber 側)
    /// (draft-ietf-moq-transport-21 §9.11 (FETCH))
    ///
    /// range は LOCATION_FILTER パラメータで指定する。
    pub async fn fetch(
        &mut self,
        namespace: TrackNamespace,
        track_name: Vec<u8>,
        parameters: MessageParameters,
    ) -> Result<FetchResult> {
        let request_id = {
            let mut session = lock_session(&self.session);
            session
                .send_fetch(namespace, track_name, parameters)
                .map_err(|e| TransportError::Internal(format!("send_fetch: {e}")))?
        };
        self.drain_events().await?;
        self.wait_fetch_resolved(request_id).await?;
        self.build_fetch_result(request_id)
    }

    fn build_fetch_result(&self, request_id: u64) -> Result<FetchResult> {
        let session = lock_session(&self.session);
        let _fetch = session
            .fetch(request_id)
            .ok_or_else(|| TransportError::Internal("fetch entry missing".into()))?;
        Ok(FetchResult { request_id })
    }

    async fn wait_subscription_resolved(&mut self, request_id: u64) -> Result<()> {
        loop {
            let state = {
                let session = lock_session(&self.session);
                session.subscription(request_id).map(|sub| sub.state)
            };
            match state {
                Some(SubscriptionState::Established) => return Ok(()),
                Some(SubscriptionState::Terminated) => {
                    return Err(TransportError::Internal(format!(
                        "request {request_id} rejected"
                    )));
                }
                _ => {}
            }
            self.pump_once().await?;
        }
    }

    async fn wait_fetch_resolved(&mut self, request_id: u64) -> Result<()> {
        loop {
            let state = {
                let session = lock_session(&self.session);
                session.fetch(request_id).map(|fetch| fetch.state)
            };
            match state {
                Some(FetchState::Established) => return Ok(()),
                Some(FetchState::Terminated) => {
                    return Err(TransportError::Internal(format!(
                        "FETCH rejected (request_id={request_id})"
                    )));
                }
                _ => {}
            }
            self.pump_once().await?;
        }
    }

    /// 次のイベントを返す
    ///
    /// session の注目イベント (GoawayReceived / CloseSession / PublishDoneReceived) に加えて、
    /// peer (MOQT relay) から届いた要求を [`ClientEvent::Request`]、
    /// REQUEST_UPDATE を [`ClientEvent::RequestUpdate`] として返す。
    /// 未処理の要求が溜まっている場合はそちらを優先して返す。
    /// REQUEST_UPDATE は応答を待たせると peer の control message timeout を招くため、
    /// 要求より先に返す。
    pub async fn next_event(&mut self) -> Result<Option<ClientEvent>> {
        loop {
            if let Some(update) = self.incoming_updates.pop_front() {
                return Ok(Some(ClientEvent::RequestUpdate(update)));
            }
            if let Some(request) = self.incoming_requests.pop_front() {
                return Ok(Some(ClientEvent::Request(request)));
            }
            if let Some(ev) = self.take_notable_event() {
                return Ok(Some(ClientEvent::Session(ev)));
            }
            self.pump_once().await?;
        }
    }

    /// peer から届いた要求に SUBSCRIBE_OK で応答する (publisher 側)
    ///
    /// draft-ietf-moq-transport-21 §9.3 (REQUEST_OK): SUBSCRIBE_OK は REQUEST_OK の別名であり、
    /// 応答は要求と同じ bidi request stream に載る。
    pub async fn send_subscribe_ok(
        &mut self,
        request_id: u64,
        track_alias: u64,
        parameters: MessageParameters,
        track_properties: TrackProperties,
    ) -> Result<()> {
        {
            let mut session = lock_session(&self.session);
            session
                .send_subscribe_ok(request_id, track_alias, parameters, track_properties)
                .map_err(|e| TransportError::Internal(format!("send_subscribe_ok: {e}")))?;
        }
        self.drain_events().await
    }

    /// peer から届いた FETCH に FETCH_OK で応答する (publisher 側)
    ///
    /// draft-ietf-moq-transport-21 §9.12 (FETCH_OK): FETCH_OK を送った publisher は
    /// 続けて FETCH 応答用の uni stream を開き、FETCH_HEADER (要求の Request ID) に
    /// 続けて Object を書く。
    pub async fn send_fetch_ok(
        &mut self,
        request_id: u64,
        end_of_track: u8,
        end_location: Location,
        parameters: MessageParameters,
        track_properties: TrackProperties,
    ) -> Result<()> {
        {
            let mut session = lock_session(&self.session);
            session
                .send_fetch_ok(
                    request_id,
                    end_of_track,
                    end_location,
                    parameters,
                    track_properties,
                )
                .map_err(|e| TransportError::Internal(format!("send_fetch_ok: {e}")))?;
        }
        self.drain_events().await
    }

    /// peer から届いた REQUEST_UPDATE に REQUEST_OK で応答する
    ///
    /// draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE): 受信側は必ず 1 通の
    /// REQUEST_OK または REQUEST_ERROR を返す。REQUEST_UPDATE_OK に載せる
    /// パラメータは無いため空で送る (LARGEST_OBJECT / EXPIRES は publisher が
    /// 状態を通知したい場合に載せるもので、本 example は購読状態を session 層に
    /// 持たせているため送らない)。
    pub async fn send_request_ok(&mut self, request_id: u64) -> Result<()> {
        {
            let mut session = lock_session(&self.session);
            session
                .send_request_ok(request_id, MessageParameters::new(), TrackProperties::new())
                .map_err(|e| TransportError::Internal(format!("send_request_ok: {e}")))?;
        }
        self.drain_events().await
    }

    /// peer から届いた要求を REQUEST_ERROR で拒否する (publisher 側)
    pub async fn send_request_error(
        &mut self,
        request_id: u64,
        error_code: u64,
        reason: &str,
    ) -> Result<()> {
        {
            let mut session = lock_session(&self.session);
            let phrase = ReasonPhrase::new(reason)
                .map_err(|e| TransportError::Internal(format!("reason phrase: {e}")))?;
            session
                .send_request_error(request_id, error_code, 0, phrase, None)
                .map_err(|e| TransportError::Internal(format!("send_request_error: {e}")))?;
        }
        self.drain_events().await
    }

    /// FETCH 応答ストリームを開き、Object を 1 件書いて FIN する (publisher 側)
    ///
    /// draft-ietf-moq-transport-21 §11.4.1 (Fetch Header): FETCH 応答は uni stream を開き、
    /// FETCH_HEADER (要求の Request ID) に続けて FETCH_STREAM_OBJECT を書く。
    /// 位置は最初の Object のため Group ID / Object ID を絶対値で載せる。
    pub async fn send_fetch_response(
        &mut self,
        request_id: u64,
        group_id: u64,
        object_id: u64,
        payload: &[u8],
        properties_data: Option<&[u8]>,
    ) -> Result<()> {
        let data_plane = self.data_plane();
        let mut stream = self.handle.open_send_stream().await?;
        let stream_id = DataStreamId(stream.stream_id());

        // Session に FETCH 応答 stream の open を通知する
        data_plane.send_fetch_header(stream_id, request_id)?;

        let mut buf = Vec::new();
        buf.extend_from_slice(&FetchHeader { request_id }.encode());
        let object = FetchStreamEntry::Object(FetchStreamObject {
            group_id: Some(group_id),
            subgroup_id: FetchSubgroupIdMode::Zero,
            object_id: Some(object_id),
            // 最初の Object は prior Object を持たないため Publisher Priority を明示する
            // (draft-ietf-moq-transport-21 §11.4.1.1 (Flags))
            publisher_priority: Some(128),
            has_properties: properties_data.is_some(),
            is_datagram_origin: false,
            payload_length: payload.len() as u64,
        });
        object
            .encode(properties_data, FetchPriorContext::First, &mut buf)
            .map_err(|e| TransportError::Internal(format!("fetch object encode: {e}")))?;
        buf.extend_from_slice(payload);

        data_plane.send_fetch_object(stream_id)?;
        stream.send(Bytes::from(buf)).await?;
        stream.finish()?;
        data_plane.send_fetch_data_stream_closed(stream_id)?;
        Ok(())
    }

    /// Session::tick を呼び出す (GOAWAY_TIMEOUT の deadline 判定)
    pub fn tick(&mut self, now_ms: u64) {
        let mut session = lock_session(&self.session);
        session.tick(now_ms);
        drop(session);
        self.cleanup_closed_requests();
    }

    /// bidi 受信タスクへ STOP_SENDING を指示する (失敗は warn のみ)
    ///
    /// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection):
    /// 受信方向の cancel は STOP_SENDING で行う。
    async fn request_stop_sending(&mut self, request_id: u64, error_code: u64) {
        let Some(stop_tx) = self.bidi_stop_txs.remove(&request_id) else {
            tracing::warn!(
                "Failed to send STOP_SENDING: no bidi receive task (request_id={request_id})"
            );
            return;
        };
        let (ack_tx, ack_rx) = oneshot::channel();
        let command = StopSendingCommand {
            error_code,
            ack: ack_tx,
        };
        if stop_tx.send(command).await.is_err() {
            tracing::warn!(
                "Failed to send STOP_SENDING: bidi receive task already finished (request_id={request_id})"
            );
            return;
        }
        // 受信タスクが bidi メッセージ送信でブロックしている場合に備え、ack 待ちに上限を設ける
        match tokio::time::timeout(std::time::Duration::from_secs(1), ack_rx).await {
            Ok(Ok(Ok(()))) => tracing::info!("Sent STOP_SENDING (request_id={request_id})"),
            Ok(Ok(Err(e))) => {
                tracing::warn!("Failed to send STOP_SENDING (request_id={request_id}): {e}");
            }
            Ok(Err(_)) => {
                tracing::warn!(
                    "Failed to send STOP_SENDING: bidi receive task dropped the command (request_id={request_id})"
                );
            }
            Err(_) => {
                tracing::warn!(
                    "Failed to send STOP_SENDING: timed out waiting for the bidi receive task (request_id={request_id})"
                );
            }
        }
    }

    /// 指定した subscription に対して STOP_SENDING を送信する (subscriber 側)
    ///
    /// `Session::stop_sending` で subscription を Terminated にした後、bidi 受信タスクへ
    /// 停止指示を渡し、request stream の受信半に実際の STOP_SENDING を送出する。
    /// 受信半への送出 API 呼び出しが成功した後にのみ `Sent STOP_SENDING` をログし、
    /// 送出できなかった場合は未送出であることと理由を警告ログに残す。
    /// I/O 送出に失敗しても戻り値は `Ok(())` とし、呼び出し側はログで未送出を判断する。
    ///
    /// 手動確認 (開発者向け): `examples/moqt-subscriber/src/pipeline.rs` の `client.close(0, "")` の直前に
    /// `tokio::time::sleep(std::time::Duration::from_millis(100)).await` を一時的に入れて、
    /// GOAWAY / close の前に STOP_SENDING を flush させ、peer (publisher / relay) 側で
    /// 当該 bidi request stream が RESET_STREAM で停止することを `RUST_LOG=debug` のログで観測する。
    pub async fn stop_sending(&mut self, request_id: u64) -> Result<()> {
        {
            let mut session = lock_session(&self.session);
            session
                .stop_sending(request_id)
                .map_err(|e| TransportError::Internal(format!("stop_sending: {e}")))?;
        }
        self.drain_events().await?;
        // 購読停止は subscriber 主導の cancel のため CANCELLED を使う
        // (draft-ietf-moq-transport-21 §12.5 (Stream Reset Error Codes))
        let error_code = DataStreamResetReason::Cancelled.error_code();
        // 送信方向が開いたままなら RESET_STREAM で打ち切る
        // (draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection): 送信方向は
        // RESET_STREAM、受信方向は STOP_SENDING)
        if let Some(mut send) = self.bidi_sends.remove(&request_id)
            && let Err(e) = send.reset(error_code)
        {
            tracing::warn!("Failed to reset bidi request stream (request_id={request_id}): {e}");
        }
        // 受信タスク終了後は peer からの close を検知できないため、回収対象として登録する。
        // 実際の回収は通常の pump_once / tick に任せる (キュー済みの Closed を先に処理させるため、
        // ここで即時 cleanup すると unknown request id になる余地がある)
        self.closed_request_streams.insert(request_id);
        self.request_stop_sending(request_id, error_code).await;
        Ok(())
    }

    /// 指定した request に対して REQUEST_UPDATE を送信する (subscriber 側)
    pub async fn send_request_update(
        &mut self,
        request_id: u64,
        parameters: MessageParameters,
    ) -> Result<()> {
        {
            let mut session = lock_session(&self.session);
            session
                .send_request_update(request_id, parameters)
                .map_err(|e| TransportError::Internal(format!("send_request_update: {e}")))?;
        }
        self.drain_events().await?;
        tracing::info!("Sent REQUEST_UPDATE (request_id={request_id})");
        Ok(())
    }

    /// GOAWAY を送信する (draft-ietf-moq-transport-21 §9.2 (GOAWAY))
    ///
    /// graceful shutdown の開始を peer に通知する。
    pub async fn send_goaway(&mut self, new_session_uri: Vec<u8>, timeout_ms: u64) -> Result<()> {
        {
            let mut session = lock_session(&self.session);
            session
                .send_goaway(new_session_uri, timeout_ms)
                .map_err(|e| TransportError::Internal(format!("send_goaway: {e}")))?;
        }
        self.drain_events().await?;
        tracing::info!("Sent GOAWAY (timeout={timeout_ms}ms)");
        Ok(())
    }

    /// 制御メッセージ応答待ちタイムアウト (ms) を取得する
    pub fn control_message_timeout_ms(&self) -> Option<u64> {
        let session = lock_session(&self.session);
        session.control_message_timeout_ms()
    }

    /// 制御メッセージ応答待ちタイムアウト (ms) を設定する
    pub fn set_control_message_timeout_ms(&mut self, timeout_ms: Option<u64>) {
        let mut session = lock_session(&self.session);
        session.set_control_message_timeout_ms(timeout_ms);
    }

    /// 指定 Request ID の subscription の Start Location を取得する
    ///
    /// 解決済みの Location Filter の Start Location を返す。Location Filter が無い (unfiltered)
    /// 場合と該当 subscription が無い場合はどちらも `None` を返す。
    /// この値は REQUEST_UPDATE で更新されうるため、Subgroup の終端方法を決める側
    /// (draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams)) が時点を決めて読む。
    pub fn subscription_filter_start(&self, request_id: u64) -> Option<Location> {
        let session = lock_session(&self.session);
        session.subscription(request_id)?.filter_start
    }

    /// Data Stream タイムアウト (ms) を取得する
    pub fn data_stream_timeout_ms(&self) -> Option<u64> {
        let session = lock_session(&self.session);
        session.data_stream_timeout_ms()
    }

    /// Data Stream タイムアウト (ms) を設定する
    pub fn set_data_stream_timeout_ms(&mut self, timeout_ms: Option<u64>) {
        let mut session = lock_session(&self.session);
        session.set_data_stream_timeout_ms(timeout_ms);
    }

    /// セッションを明示的にクローズする
    ///
    /// `Session::close` を呼び出し、`CloseSession` イベントを drain して
    /// transport 層に接続クローズを伝える。
    pub async fn close(&mut self, code: u64, reason: &'static str) -> Result<()> {
        {
            let mut session = lock_session(&self.session);
            session.close(code, reason);
        }
        self.drain_events().await?;
        // コード 0 以外はエラー終了なので "gracefully" とは出さない。
        // アプリ側が `Session closed: {:#x} {}` を警告として記録するため、
        // ここでは通常終了と同じ info を出さず debug に留める
        if code == 0 {
            tracing::info!("Session closed gracefully");
        } else {
            tracing::debug!("Session close sent: {code:#x} {reason}");
        }
        Ok(())
    }

    /// アプリ側に通知すべき SessionEvent を取り出す (GoawayReceived など)
    fn take_notable_event(&mut self) -> Option<SessionEvent> {
        if let Some(event) = self.notable_events.pop_front() {
            return Some(event);
        }
        loop {
            let event = {
                let mut session = lock_session(&self.session);
                session.poll_event()
            };
            let ev = event?;
            if is_notable_event(&ev) {
                return Some(ev);
            }
        }
    }

    /// control / bidi channel のいずれか 1 件を受信して session に流し、events を drain
    async fn pump_once(&mut self) -> Result<()> {
        tokio::select! {
            maybe = self.control_rx.recv() => {
                let Some(res) = maybe else {
                    return Err(TransportError::Internal("control channel closed".into()));
                };
                match res? {
                    StreamRead::Value(msg) => {
                        let mut session = lock_session(&self.session);
                        session
                            .recv_control(msg)
                            .map_err(|e| TransportError::Internal(format!("recv_control: {e}")))?;
                    }
                    StreamRead::Closed(end) => {
                        let mut session = lock_session(&self.session);
                        let _ = session.recv_control_stream_closed(end);
                    }
                }
                self.drain_events().await?;
            }
            maybe = self.bidi_rx.recv() => {
                let Some(message) = maybe else {
                    return Err(TransportError::Internal("bidi channel closed".into()));
                };
                match message {
                    BidiMessage::Response(rid, res) => {
                        match res? {
                            StreamRead::Value(msg) => {
                                let recv_result = {
                                    let mut session = lock_session(&self.session);
                                    session
                                    .recv_stream_message(rid, msg)
                                        .map_err(|e| TransportError::Internal(format!("recv_stream_message: {e}")))
                                };
                                recv_result?;
                                self.drain_events().await?;
                            }
                            StreamRead::Closed(end) => {
                                // 回収済み request (malformed 終端後など) への遅延 close は Session が
                                // no-op 吸収するため、回収対象として登録しない (登録すると誰も除去できない)
                                let register_for_cleanup = {
                                    let mut session = lock_session(&self.session);
                                    let recv_result = session.recv_request_stream_closed(rid, end);
                                    recv_result.is_ok()
                                        && (session.subscription(rid).is_some()
                                            || session.fetch(rid).is_some())
                                };
                                if register_for_cleanup {
                                    self.closed_request_streams.insert(rid);
                                }
                                tracing::debug!("bidi stream {rid} closed by peer");
                                self.drain_events().await?;
                                self.cleanup_closed_requests();
                            }
                        }
                    }
                    BidiMessage::PeerRequest { request_id, message, send, stop_tx } => {
                        // peer (relay) が開始した request stream の受理。
                        // 先頭メッセージを Session に通知して状態を作り、応答用の送信半を
                        // 登録したうえでアプリへ引き渡す。
                        {
                            let mut session = lock_session(&self.session);
                            session
                                .recv_request(message.clone())
                                .map_err(|e| TransportError::Internal(format!("recv_request: {e}")))?;
                        }
                        self.bidi_sends.insert(request_id, send);
                        self.bidi_stop_txs.insert(request_id, stop_tx);
                        tracing::info!("Received peer request: request_id={request_id}");
                        self.incoming_requests.push_back(IncomingRequest {
                            request_id,
                            message,
                        });
                        // 受理時に Session が自動応答 (REQUEST_ERROR 等) を積むことがある
                        self.drain_events().await?;
                    }
                }
            }
        }
        Ok(())
    }

    /// session の events キューを drain し、I/O 操作に変換する
    async fn drain_events(&mut self) -> Result<()> {
        loop {
            let event = {
                let mut session = lock_session(&self.session);
                session.poll_event()
            };
            let Some(event) = event else {
                break;
            };
            match event {
                SessionEvent::SendControl(msg) => {
                    self.control_send.send(Bytes::from(msg.encode()?)).await?;
                }
                SessionEvent::SendRequest {
                    request_id,
                    message,
                } => {
                    let (mut send, recv) = self.handle.open_bidi_stream().await?;
                    send.send(Bytes::from(message.encode()?)).await?;
                    self.bidi_sends.insert(request_id, send);
                    let (stop_tx, stop_rx) = mpsc::channel(1);
                    self.bidi_stop_txs.insert(request_id, stop_tx);
                    let reader = ControlStream::new(recv);
                    spawn_bidi_recv_task(
                        request_id,
                        reader,
                        self.bidi_tx.clone(),
                        stop_rx,
                        &self.task_monitor,
                    );
                }
                SessionEvent::SendOnStream {
                    request_id,
                    message,
                    fin,
                } => {
                    // 送信方向を FIN / RESET 済みの request では送信半が台帳に無い。
                    // Session が同一 request へ 2 度目の fin 付き送信を発行し得るため
                    // (例: 拒否した REQUEST_UPDATE がパイプラインで届き、同じ request へ
                    // 2 度目の REQUEST_ERROR + fin が発行される)、
                    // ResetRequestStream と同じく no-op にして example を停止させない。
                    if let Some(mut send) = self.bidi_sends.remove(&request_id) {
                        send.send(Bytes::from(message.encode()?)).await?;
                        // 最終メッセージの場合は送信後に FIN する (§6.4.2.3 / §9.9)。
                        // 以後の送信は Session が発行しないためエントリは戻さない。
                        if fin {
                            send.finish()?;
                        } else {
                            self.bidi_sends.insert(request_id, send);
                        }
                    } else {
                        // 送信方向を FIN / RESET 済みの request では送信半が台帳に無い。
                        // Session は同一 request へ 2 度目の fin 付き送信を発行し得るため
                        // (例: 拒否した REQUEST_UPDATE がパイプラインで届き、同じ request へ
                        // 2 度目の REQUEST_ERROR + fin が発行される)、
                        // ResetRequestStream と同じく no-op にして example を停止させない。
                        tracing::warn!(
                            "Dropping send on already closed request stream: request_id={request_id}, fin={fin}, message={message:?}"
                        );
                    }
                }
                SessionEvent::FinishRequestStream { request_id } => {
                    // draft-ietf-moq-transport-21 §6.4.2.2 (Graceful Request Stream Closure):
                    // responder の FIN で request が完了したため、requester である自側も
                    // 送信方向を FIN で閉じる (SHOULD)。既に FIN 済みなら送信半は台帳に無い。
                    if let Some(mut send) = self.bidi_sends.remove(&request_id)
                        && let Err(e) = send.finish()
                    {
                        tracing::warn!(
                            "Failed to finish bidi request stream (request_id={request_id}): {e}"
                        );
                    }
                }
                SessionEvent::ResetRequestStream {
                    request_id,
                    error_code,
                } => {
                    // 送信方向が既に FIN / RESET 済みの request では no-op にする
                    // (GOING_AWAY timeout reset と malformed cancel が重複して届きうる。
                    // `ResetRequestStream` の doc 参照)
                    if let Some(mut send) = self.bidi_sends.remove(&request_id)
                        && let Err(e) = send.reset(error_code)
                    {
                        tracing::warn!(
                            "Failed to reset bidi request stream (request_id={request_id}): {e}"
                        );
                    }
                }
                SessionEvent::StopSendingRequestStream {
                    request_id,
                    error_code,
                } => {
                    // Session が Malformed Track 検出 (§12.1) または未知の Mandatory Track
                    // Property を含む SUBSCRIBE_OK / FETCH_OK の受信 (§3.6) で発行する受信方向の
                    // cancel (draft-ietf-moq-transport-21 §6.4.2.3)。bidi 受信タスクへ
                    // STOP_SENDING を指示する。
                    // 受信タスク終了後は peer の close を検知できないため、回収対象として登録する
                    self.closed_request_streams.insert(request_id);
                    self.request_stop_sending(request_id, error_code).await;
                }
                SessionEvent::CloseSession(err) => {
                    self.handle.close(err.code, err.reason).await?;
                    return Ok(());
                }
                SessionEvent::RequestUpdateReceived {
                    request_id,
                    parameters,
                } => {
                    // draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE):
                    // 受信側は必ず 1 通の REQUEST_OK または REQUEST_ERROR で応答する MUST。
                    // 黙殺すると peer の control message 応答待ち (CONTROL_MESSAGE_TIMEOUT)
                    // を招くため、応答の判断をアプリへ引き渡す。
                    tracing::info!("Received REQUEST_UPDATE: request_id={request_id}");
                    self.incoming_updates.push_back(IncomingRequestUpdate {
                        request_id,
                        parameters,
                    });
                }
                ev @ (SessionEvent::GoawayReceived { .. }
                | SessionEvent::PublishDoneReceived { .. }) => {
                    // アプリ (next_event) が観測する notable イベント。
                    // ここで捨てると take_notable_event が取り出せなくなる
                    // (受信メッセージを契機に生成されたイベントは、この drain が
                    //  next_event の次の take_notable_event より先に poll する)。
                    self.notable_events.push_back(ev);
                }
                SessionEvent::Established
                | SessionEvent::RequestErrorReceived { .. }
                | SessionEvent::RequestTerminated { .. }
                | SessionEvent::RequestOkReceived { .. }
                | SessionEvent::PublishStateNotifyReceived { .. }
                | SessionEvent::ResetDataStream { .. }
                | SessionEvent::FetchOkReceived { .. }
                | SessionEvent::SendPaddingStream { .. }
                | SessionEvent::OpenFillFetchStream { .. }
                | SessionEvent::SendPaddingDatagram { .. } => {
                    // GoawayReceived / PublishDoneReceived はメインループが take_notable_event で拾う。
                    // ResetDataStream は OBJECT_DELIVERY_TIMEOUT / SUBGROUP_DELIVERY_TIMEOUT を
                    // 設定した subscription や malformed 検出時に発火する (draft-ietf-moq-transport-21
                    // §5.2 (Delivery Timeouts and Data Reliability) / §12.1 (Malformed Tracks))。
                    // 本 example は受信 data stream の reset を行わないため無視する。
                    // PublishStateNotifyReceived は peer publisher の通知であり、
                    // 本 example では特別な処理を行わない
                    // (draft-ietf-moq-transport-21 §9.10 (PUBLISH_STATE_NOTIFY))。
                    // OpenFillFetchStream は fill 配信を要求された場合に発火する
                    // (draft-ietf-moq-transport-21 §3.4 (Fill Semantics))。本 example は fill 配信を行わないため無視する。
                    // FetchOkReceived は subscriber 役でのみ発火し、終端情報は Session::fetch の
                    // ポーリングで参照するためここでは特別な処理を行わない
                    // (draft-ietf-moq-transport-21 §9.12 (FETCH_OK))。
                    // SendPaddingStream / SendPaddingDatagram も padding を要求していないため到達しない。
                    // その他の Received 系 / Established / RequestTerminated は本 example では特別な処理を行わない。
                }
            }
        }
        self.cleanup_closed_requests();
        Ok(())
    }

    fn cleanup_closed_requests(&mut self) {
        let request_ids: Vec<u64> = self.closed_request_streams.iter().copied().collect();
        let mut forgotten = Vec::new();
        {
            let mut session = lock_session(&self.session);
            for request_id in request_ids {
                if session.subscription_cleanup_ready(request_id) == Some(true)
                    && session.forget_subscription(request_id).is_some()
                {
                    forgotten.push(request_id);
                    continue;
                }
                if session.fetch_cleanup_ready(request_id) == Some(true)
                    && session.forget_fetch(request_id).is_some()
                {
                    forgotten.push(request_id);
                }
            }
        }
        for request_id in forgotten {
            self.closed_request_streams.remove(&request_id);
            self.bidi_stop_txs.remove(&request_id);
            if let Some(mut send) = self.bidi_sends.remove(&request_id) {
                let _ = send.finish();
            }
        }
    }
}

fn pop_send_control(session: &mut Session) -> Result<ControlMessage> {
    match session.poll_event() {
        Some(SessionEvent::SendControl(msg)) => Ok(msg),
        other => Err(TransportError::Internal(format!(
            "expected SendControl as initial event, got {other:?}"
        ))),
    }
}

async fn send_control_stream_setup(
    control_send: &mut SendStream,
    setup: &ControlMessage,
) -> Result<()> {
    control_send
        .send(Bytes::from(encode_control_stream_setup(setup)?))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PUBLISH_DONE はアプリが観測する notable イベントである
    #[test]
    fn publish_done_is_notable_event() {
        let event = SessionEvent::PublishDoneReceived {
            request_id: 0,
            status_code: 0x2,
            stream_count: u64::MAX,
            reason: ReasonPhrase::new("").expect("空の理由句は有効である"),
        };
        assert!(is_notable_event(&event));
    }

    /// GOAWAY はアプリが観測する notable イベントである
    #[test]
    fn goaway_is_notable_event() {
        let event = SessionEvent::GoawayReceived {
            new_session_uri: Vec::new(),
            timeout: 5000,
            on_request_stream: None,
        };
        assert!(is_notable_event(&event));
    }

    /// アプリが観測しないイベントは notable ではない
    #[test]
    fn established_is_not_notable_event() {
        assert!(!is_notable_event(&SessionEvent::Established));
    }
}
