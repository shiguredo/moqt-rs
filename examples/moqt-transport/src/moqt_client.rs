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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex};

use bytes::Bytes;
use tokio::sync::{Mutex as TokioMutex, mpsc};

use shiguredo_moqt::decoder::MessageDecoder;
use shiguredo_moqt::error::SESSION_LOCAL_FILTER_MISMATCH;
use shiguredo_moqt::session::types::{
    DatagramAcceptance, FetchState, RecvDataStreamError, SubscriptionState, TrackDataAcceptance,
};
use shiguredo_moqt::stream::encode_control_stream_setup;
use shiguredo_moqt::{
    message::ControlMessage, message::ReasonPhrase, message::common::TrackNamespace,
    message_parameter::MessageParameter, message_parameter::MessageParameterValue,
    message_parameter::MessageParameters, message_parameter::PARAM_SUBSCRIBER_PRIORITY,
    session::core::Session, session::types::DataStreamId, session::types::RequestStreamEnd,
    session::types::SessionEvent, session::types::SessionState,
    session::types::Transport as MoqtTransport, stream::decoder::DecodedSubgroupObject,
    stream::fetch::FetchHeader, stream::subgroup::SubgroupHeader,
    track_properties::TrackProperties,
};

use crate::error::{Result, TransportError};
use crate::transport::{RecvChunk, RecvStream, SendStream, StreamAcceptor, StreamHandle};
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
    /// フィルタ不通過 (SESSION_LOCAL_FILTER_MISMATCH)。ワイヤ送信をスキップする
    Skip,
}

/// bidi 受信タスクからメインループへの通知
pub type BidiMessage = (u64, Result<StreamRead<ControlMessage>>);

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
    /// `SESSION_LOCAL_FILTER_MISMATCH` (フィルタ不通過) は `Skip` として返し、
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
            Err(e) if e.code == SESSION_LOCAL_FILTER_MISMATCH => Ok(ObjectFilterOutcome::Skip),
            Err(e) => Err(TransportError::Internal(format!(
                "send_subgroup_object: {e}"
            ))),
        }
    }

    /// Object Datagram 送信を Session に通知し、フィルタ評価の結果を返す
    ///
    /// `SESSION_LOCAL_FILTER_MISMATCH` (フィルタ不通過) は `Skip` として返し、
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
            Err(e) if e.code == SESSION_LOCAL_FILTER_MISMATCH => Ok(ObjectFilterOutcome::Skip),
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
    pub fn recv_subgroup_object(
        &self,
        stream_id: DataStreamId,
        object: &DecodedSubgroupObject,
    ) -> Result<()> {
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
    closed_request_streams: HashSet<u64>,
    control_rx: mpsc::Receiver<ControlIncoming>,
    bidi_tx: mpsc::Sender<BidiMessage>,
    bidi_rx: mpsc::Receiver<BidiMessage>,
    task_monitor: tokio_metrics::TaskMonitor,
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
fn spawn_bidi_recv_task(
    request_id: u64,
    mut stream: ControlStream,
    tx: mpsc::Sender<BidiMessage>,
    task_monitor: &tokio_metrics::TaskMonitor,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(task_monitor.clone().instrument(async move {
        loop {
            match stream.recv_message().await {
                Ok(message) => {
                    let is_closed = matches!(message, StreamRead::Closed(_));
                    if tx.send((request_id, Ok(message))).await.is_err() || is_closed {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send((request_id, Err(e))).await;
                    break;
                }
            }
        }
    }))
}

impl MoqtClient {
    /// QUIC 直接接続で MoQT client を確立する
    /// (draft-ietf-moq-transport-21 §6.2 (Session establishment) / §6.3 (Session initialization))
    ///
    /// 戻り値の [`StreamAcceptor`] は data stream を受信する側 (subscriber) が使う。
    /// publisher は受信しないため破棄してよい。
    pub async fn establish_quic(
        connection: s2n_quic::connection::Connection,
        path: &str,
        authority: &str,
        impl_name: &str,
        task_monitor: &tokio_metrics::TaskMonitor,
    ) -> Result<(Self, StreamAcceptor)> {
        let (handle, acceptor) = connection.split();
        let (_bidi_acceptor, mut recv_acceptor) = acceptor.split();
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

        let client =
            Self::finish_handshake(session, control_send, handle, control_recv, task_monitor)
                .await?;
        Ok((client, StreamAcceptor::Quic(recv_acceptor)))
    }

    /// WebTransport 接続で MoQT client を確立する
    ///
    /// draft-ietf-moq-transport-21 §9.1.1 (AUTHORITY) / §9.1.2 (PATH) により
    /// WebTransport 使用時は PATH (0x01) と AUTHORITY (0x05) を送信してはならない。
    ///
    /// 戻り値の [`StreamAcceptor`] は data stream を受信する側 (subscriber) が使う。
    /// publisher は受信しないため破棄してよい。
    pub async fn establish_wt(
        wt_session: WtSession,
        impl_name: &str,
        task_monitor: &tokio_metrics::TaskMonitor,
    ) -> Result<(Self, StreamAcceptor)> {
        let shared = Arc::new(TokioMutex::new(wt_session));
        let handle = StreamHandle::WebTransport(shared.clone());

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

        let client =
            Self::finish_handshake(session, control_send, handle, control_recv, task_monitor)
                .await?;
        Ok((client, StreamAcceptor::WebTransport(shared)))
    }

    async fn finish_handshake(
        mut session: Session,
        control_send: SendStream,
        handle: StreamHandle,
        mut control_recv: ControlStream,
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

        let task_monitor = task_monitor.clone();

        Ok(Self {
            session,
            control_send,
            handle,
            bidi_sends: HashMap::new(),
            closed_request_streams: HashSet::new(),
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
        // bidi 送信側を finish してリソースを解放
        if let Some(mut send) = self.bidi_sends.remove(&request_id) {
            send.finish()?;
        }
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

    /// 次の注目イベント (GoawayReceived / CloseSession / PublishDoneReceived) を返す
    pub async fn next_event(&mut self) -> Result<Option<SessionEvent>> {
        if let Some(ev) = self.take_notable_event() {
            return Ok(Some(ev));
        }
        self.pump_once().await?;
        Ok(self.take_notable_event())
    }

    /// Session::tick を呼び出す (GOAWAY_TIMEOUT の deadline 判定)
    pub fn tick(&mut self, now_ms: u64) {
        let mut session = lock_session(&self.session);
        session.tick(now_ms);
        drop(session);
        self.cleanup_closed_requests();
    }

    /// 指定した subscription に対して STOP_SENDING を送信する (subscriber 側)
    pub async fn stop_sending(&mut self, request_id: u64) -> Result<()> {
        {
            let mut session = lock_session(&self.session);
            session
                .stop_sending(request_id)
                .map_err(|e| TransportError::Internal(format!("stop_sending: {e}")))?;
        }
        self.drain_events().await?;
        tracing::info!("Sent STOP_SENDING (request_id={request_id})");
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
        tracing::info!("Session closed gracefully");
        Ok(())
    }

    /// アプリ側に通知すべき SessionEvent を取り出す (GoawayReceived など)
    fn take_notable_event(&mut self) -> Option<SessionEvent> {
        loop {
            let event = {
                let mut session = lock_session(&self.session);
                session.poll_event()
            };
            match event {
                Some(
                    ev @ (SessionEvent::GoawayReceived { .. }
                    | SessionEvent::CloseSession(_)
                    | SessionEvent::PublishDoneReceived { .. }),
                ) => return Some(ev),
                Some(_) => continue,
                None => return None,
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
                let Some((rid, res)) = maybe else {
                    return Err(TransportError::Internal("bidi channel closed".into()));
                };
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
                        let recv_result = {
                            let mut session = lock_session(&self.session);
                            session.recv_request_stream_closed(rid, end)
                        };
                        if recv_result.is_ok() {
                            self.closed_request_streams.insert(rid);
                        }
                        tracing::debug!("bidi stream {rid} closed by peer");
                        self.drain_events().await?;
                        self.cleanup_closed_requests();
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
                    let reader = ControlStream::new(recv);
                    spawn_bidi_recv_task(
                        request_id,
                        reader,
                        self.bidi_tx.clone(),
                        &self.task_monitor,
                    );
                }
                SessionEvent::SendOnStream {
                    request_id,
                    message,
                    fin,
                } => {
                    let Some(mut send) = self.bidi_sends.remove(&request_id) else {
                        return Err(TransportError::Internal(format!(
                            "no bidi stream for request_id {request_id}"
                        )));
                    };
                    send.send(Bytes::from(message.encode()?)).await?;
                    // 最終メッセージの場合は送信後に FIN する (§6.4.2.3 / §9.9)。
                    // 以後の送信は Session が発行しないためエントリは戻さない。
                    if fin {
                        send.finish()?;
                    } else {
                        self.bidi_sends.insert(request_id, send);
                    }
                }
                SessionEvent::ResetRequestStream {
                    request_id,
                    error_code,
                } => {
                    let Some(mut send) = self.bidi_sends.remove(&request_id) else {
                        return Err(TransportError::Internal(format!(
                            "no bidi stream for request_id {request_id}"
                        )));
                    };
                    send.reset(error_code)?;
                }
                SessionEvent::CloseSession(err) => {
                    self.handle.close(err.code, err.reason).await?;
                    return Ok(());
                }
                SessionEvent::Established
                | SessionEvent::GoawayReceived { .. }
                | SessionEvent::PublishDoneReceived { .. }
                | SessionEvent::RequestErrorReceived { .. }
                | SessionEvent::NamespaceReceived { .. }
                | SessionEvent::NamespaceDoneReceived { .. }
                | SessionEvent::RequestTerminated { .. }
                | SessionEvent::PublishSkippedReceived { .. }
                | SessionEvent::RequestOkReceived { .. }
                | SessionEvent::RequestUpdateReceived { .. }
                | SessionEvent::PublishStateNotifyReceived { .. }
                | SessionEvent::ResetDataStream { .. }
                | SessionEvent::SubscribeTracksReceived { .. }
                | SessionEvent::StopSendingRequestStream { .. }
                | SessionEvent::FetchOkReceived { .. }
                | SessionEvent::SendPaddingStream { .. }
                | SessionEvent::OpenFillFetchStream { .. }
                | SessionEvent::SendPaddingDatagram { .. } => {
                    // GoawayReceived / PublishDoneReceived はメインループが take_notable_event で拾う。
                    // ResetDataStream は OBJECT_DELIVERY_TIMEOUT / SUBGROUP_DELIVERY_TIMEOUT を
                    // 設定した subscription でのみ発火する (draft-ietf-moq-transport-21 §5.2 (Delivery Timeouts and Data Reliability))。
                    // PublishStateNotifyReceived は peer publisher の通知であり、
                    // 本 example では特別な処理を行わない
                    // (draft-ietf-moq-transport-21 §9.10 (PUBLISH_STATE_NOTIFY))。
                    // OpenFillFetchStream は fill 配信を要求された場合に発火する
                    // (draft-ietf-moq-transport-21 §3.4 (Fill Semantics))。本 example は fill 配信を行わないため無視する。
                    // StopSendingRequestStream は受信方向の cancel 指示だが、
                    // 受信半は受信タスクが所有し停止手段がないため無視する。
                    // Session の自動発火経路もなく到達しない
                    // (draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection))。
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
