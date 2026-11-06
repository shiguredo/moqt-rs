//! s2n-quic をネイティブ QUIC のバックエンドとして使う実装

use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use bytes::Bytes;
use s2n_quic::Client;
#[cfg(test)]
use s2n_quic::Server as S2nServer;
use s2n_quic::client::Connect;
use s2n_quic::provider::event::events::{Frame, FrameReceived};
use s2n_quic::provider::tls::rustls as s2n_rustls;
use tokio::sync::{Mutex, mpsc};

use super::CloseReason;
#[cfg(test)]
use super::ServerTls;
use super::{ALPN, QuicError, StopSendingObserved, StopSendingReceiver};
use crate::error::{Result, TransportError};

/// peer の STOP_SENDING と受信パケットの内訳を観測する購読者
///
/// 2 つの役割を持つ。
///
/// - 受信パケットの内訳 (受信 / 破棄 / 復号失敗) を数える診断。`MOQT_PACKET_DIAG=1` の
///   ときだけ 1 秒ごとのログ出力を有効化する。受信が止まったときに「パケットが来ていない」
///   のか「来ているが復号に失敗している」のかを切り分けるために使う
/// - peer の STOP_SENDING の観測。診断ではなく MOQT 層へ渡す必要のある情報であるため、
///   `MOQT_PACKET_DIAG` の有無に関わらず常に行う
///
/// s2n-quic の `with_event` は 1 接続につき 1 購読者しか差し込めないため、診断と観測を
/// 同じ購読者に同居させている。
#[derive(Debug, Clone, Default)]
struct ConnectionObserver {
    /// 受信したパケット数
    received: Arc<std::sync::atomic::AtomicU64>,
    /// 破棄したパケット数
    dropped: Arc<std::sync::atomic::AtomicU64>,
    /// 復号に失敗したパケット数
    decryption_failed: Arc<std::sync::atomic::AtomicU64>,
    /// peer の STOP_SENDING を流す先。観測が不要な場合は `None`
    stop_sending_tx: Option<mpsc::UnboundedSender<StopSendingObserved>>,
}

impl ConnectionObserver {
    /// カウンタだけを持つ購読者を作る
    fn counters(&self) -> PacketCounters {
        PacketCounters {
            received: Arc::clone(&self.received),
            dropped: Arc::clone(&self.dropped),
            decryption_failed: Arc::clone(&self.decryption_failed),
        }
    }
}

/// 診断用のカウンタ
#[derive(Debug, Clone)]
struct PacketCounters {
    /// 受信したパケット数
    received: Arc<std::sync::atomic::AtomicU64>,
    /// 破棄したパケット数
    dropped: Arc<std::sync::atomic::AtomicU64>,
    /// 復号に失敗したパケット数
    decryption_failed: Arc<std::sync::atomic::AtomicU64>,
}

impl PacketCounters {}

impl s2n_quic::provider::event::Subscriber for ConnectionObserver {
    type ConnectionContext = ();

    fn create_connection_context(
        &mut self,
        _meta: &s2n_quic::provider::event::ConnectionMeta,
        _info: &s2n_quic::provider::event::ConnectionInfo<'_>,
    ) -> Self::ConnectionContext {
    }

    fn on_packet_received(
        &mut self,
        _context: &mut Self::ConnectionContext,
        _meta: &s2n_quic::provider::event::ConnectionMeta,
        _event: &s2n_quic::provider::event::events::PacketReceived,
    ) {
        self.received
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn on_frame_received(
        &mut self,
        _context: &mut Self::ConnectionContext,
        _meta: &s2n_quic::provider::event::ConnectionMeta,
        event: &FrameReceived<'_>,
    ) {
        // peer が送出した STOP_SENDING を観測して MOQT 層へ渡す。送信 API の失敗からは
        // 検知できない場合がある (FIN ack 済みのストリームでは s2n-quic が RESET を
        // 組み立てず、STOP_SENDING に由来するエラーが `send` / `finish` に現れない) ため、
        // フレームの観測を検知の経路の 1 つにする。
        // 自側が送出した STOP_SENDING はこのフックに現れないため、peer 由来に限られる。
        if let Frame::StopSending { id, error_code, .. } = &event.frame
            && let Some(tx) = &self.stop_sending_tx
        {
            let _ = tx.send(StopSendingObserved {
                stream_id: *id,
                error_code: *error_code,
            });
        }
    }

    fn on_packet_dropped(
        &mut self,
        _context: &mut Self::ConnectionContext,
        _meta: &s2n_quic::provider::event::ConnectionMeta,
        event: &s2n_quic::provider::event::events::PacketDropped,
    ) {
        self.dropped
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let s2n_quic::provider::event::events::PacketDropReason::DecryptionFailed { .. } =
            &event.reason
        {
            self.decryption_failed
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// client が送る CONNECTION_CLOSE の終了コードを記録する購読者
///
/// サーバー側で client の終了コードを観測する唯一の経路である。記録は
/// `std::sync::Mutex` で保護する。購読者は s2n-quic の駆動スレッドから同期で呼ばれ、
/// `tokio::sync::Mutex` を await できないため、ロックを保持する区間を「Vec へ 1 件
/// push する」だけに限って使う。
#[cfg(test)]
#[derive(Debug, Default)]
struct CloseCodeRecorder {
    /// 観測した終了コード
    codes: Arc<StdMutex<Vec<u64>>>,
}

#[cfg(test)]
impl s2n_quic::provider::event::Subscriber for CloseCodeRecorder {
    type ConnectionContext = ();

    fn create_connection_context(
        &mut self,
        _meta: &s2n_quic::provider::event::ConnectionMeta,
        _info: &s2n_quic::provider::event::ConnectionInfo<'_>,
    ) -> Self::ConnectionContext {
    }

    fn on_connection_closed(
        &mut self,
        _context: &mut Self::ConnectionContext,
        _meta: &s2n_quic::provider::event::ConnectionMeta,
        event: &s2n_quic::provider::event::events::ConnectionClosed,
    ) {
        // `Application` は自側の close でも作られるため、peer 起点 (`Remote`) に限る
        if let s2n_quic::connection::Error::Application {
            error,
            initiator: s2n_quic::provider::event::Location::Remote,
            ..
        } = &event.error
        {
            self.codes
                .lock()
                .expect("the close code record must not be poisoned")
                .push(u64::from(*error));
        }
    }
}

/// 接続を確立する前の接続
pub struct Connected {
    /// s2n-quic の接続
    inner: s2n_quic::connection::Connection,
    /// peer が送った終了コードの記録 (server 接続のみ)
    close_codes: Option<Arc<StdMutex<Vec<u64>>>>,
}

impl Connected {
    /// 接続を操作ハンドルとストリーム受理ハンドルに分割する
    pub(crate) fn split(self) -> (Connection, StreamAcceptor) {
        let (handle, acceptor) = self.inner.split();
        let (bidi, uni) = acceptor.split();
        (
            Connection {
                inner: handle,
                close_codes: self.close_codes,
            },
            StreamAcceptor {
                bidi: Mutex::new(bidi),
                uni: Mutex::new(uni),
            },
        )
    }

    /// s2n-quic の endpoint は接続とは独立に駆動されるため、何もしない
    pub async fn enable_background_driver(&self) -> std::result::Result<(), QuicError> {
        Ok(())
    }

    /// 接続元のアドレスを返す
    pub async fn remote_addr(&self) -> std::result::Result<SocketAddr, QuicError> {
        self.inner
            .remote_addr()
            .map_err(|e| QuicError::Other(format!("failed to read the remote address: {e}")))
    }
}

/// ストリームを開く側の接続ハンドル
#[derive(Clone)]
pub struct Connection {
    /// s2n-quic の接続ハンドル
    inner: s2n_quic::connection::Handle,
    /// peer が送った終了コードの記録 (server 接続のみ)
    close_codes: Option<Arc<StdMutex<Vec<u64>>>>,
}

impl Connection {
    /// 新しい単方向ストリームを開く
    ///
    /// s2n-quic の `open_send_stream` は枠が空くまで待つ。
    pub(crate) async fn open_send_stream_wait(&self) -> std::result::Result<SendStream, QuicError> {
        let stream = self
            .inner
            .clone()
            .open_send_stream()
            .await
            .map_err(map_connection_error)?;
        Ok(SendStream { inner: stream })
    }

    /// 新しい双方向ストリームを開く
    pub(crate) async fn open_bidirectional_stream_wait(
        &self,
    ) -> std::result::Result<(SendStream, RecvStream), QuicError> {
        let stream = self
            .inner
            .clone()
            .open_bidirectional_stream()
            .await
            .map_err(map_connection_error)?;
        // s2n-quic の `split` は (受信, 送信) の順で返す
        let (recv, send) = stream.split();
        Ok((SendStream { inner: send }, RecvStream { inner: recv }))
    }

    /// datagram を送信する
    ///
    /// 送信キューの空きができるまで待つ。待たずに捨てると、再送されない datagram が
    /// 失われる。
    pub(crate) async fn send_datagram_wait(
        &self,
        data: Vec<u8>,
    ) -> std::result::Result<(), QuicError> {
        let mut bytes = Bytes::from(data);
        std::future::poll_fn(|cx| {
            let sent = self.inner.datagram_mut(
                |sender: &mut s2n_quic::provider::datagram::default::Sender| {
                    sender.poll_send_datagram(&mut bytes, cx)
                },
            );
            match sent {
                Ok(sent) => sent.map_err(|e| QuicError::Other(format!("datagram send error: {e}"))),
                Err(_) => std::task::Poll::Ready(Err(QuicError::ConnectionClosed)),
            }
        })
        .await
    }

    /// datagram を 1 つ受信する
    pub(crate) async fn receive_datagram(&self) -> std::result::Result<Vec<u8>, QuicError> {
        std::future::poll_fn(|cx| {
            let received = self.inner.datagram_mut(
                |receiver: &mut s2n_quic::provider::datagram::default::Receiver| {
                    receiver.poll_recv_datagram(cx)
                },
            );
            match received {
                Ok(received) => received.map(|result| {
                    result
                        .map(|bytes| bytes.to_vec())
                        .map_err(|e| QuicError::Other(format!("datagram receive error: {e}")))
                }),
                // `datagram_mut` 自体の失敗は接続が閉じていることを意味する
                Err(_) => std::task::Poll::Ready(Err(QuicError::ConnectionClosed)),
            }
        })
        .await
    }

    /// 接続が終了するまで待ち、終了理由を返す
    ///
    /// s2n-quic の公開 API には接続の終了理由を待つ手段が無い。サーバー側で購読者が
    /// 記録した終了コードを、記録されるまで待つ。
    pub async fn closed(&self) -> Option<CloseReason> {
        let codes = self.close_codes.as_ref()?;
        loop {
            if let Some(code) = codes
                .lock()
                .expect("the close code record must not be poisoned")
                .first()
                .copied()
            {
                return Some(CloseReason::Application { code });
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// 接続をアプリケーションエラーで閉じる
pub(crate) async fn close_application(connection: &Connection, code: u64) -> Result<()> {
    // 可変長整数に収まらないコードは UNKNOWN で閉じる
    let error =
        s2n_quic::application::Error::new(code).unwrap_or(s2n_quic::application::Error::UNKNOWN);
    connection.inner.close(error);
    Ok(())
}

/// 送信ストリーム
pub struct SendStream {
    /// s2n-quic の送信ストリーム
    inner: s2n_quic::stream::SendStream,
}

impl SendStream {
    /// データをすべて送信する
    ///
    /// s2n-quic の `send` は渡したデータをすべてキューに積むまで待つ。
    pub(crate) async fn send_all(&mut self, data: &[u8]) -> std::result::Result<(), QuicError> {
        self.inner
            .send(Bytes::copy_from_slice(data))
            .await
            .map_err(map_send_error)
    }

    /// 送信方向を終了する (FIN)
    pub(crate) async fn finish(&mut self) -> std::result::Result<(), QuicError> {
        self.inner.finish().map_err(map_send_error)
    }

    /// 送信方向を reset する (RESET_STREAM)
    pub(crate) async fn reset(&mut self, error_code: u64) -> std::result::Result<(), QuicError> {
        let error = s2n_quic::application::Error::new(error_code)
            .map_err(|e| QuicError::Other(format!("failed to build an application error: {e}")))?;
        self.inner.reset(error).map_err(map_send_error)
    }

    /// ストリーム ID を返す
    pub(crate) fn id(&self) -> u64 {
        self.inner.id()
    }
}

/// 受信ストリーム
pub struct RecvStream {
    /// s2n-quic の受信ストリーム
    inner: s2n_quic::stream::ReceiveStream,
}

impl RecvStream {
    /// ストリーム ID を返す
    pub(crate) fn id(&self) -> u64 {
        self.inner.id()
    }

    /// データまたは FIN を受信する
    pub(crate) async fn receive(&mut self) -> std::result::Result<Option<Vec<u8>>, QuicError> {
        match self.inner.receive().await {
            Ok(Some(data)) => Ok(Some(data.to_vec())),
            Ok(None) => Ok(None),
            Err(s2n_quic::stream::Error::StreamReset { error, .. }) => Err(QuicError::Reset {
                error_code: u64::from(error),
            }),
            Err(e) => Err(QuicError::Other(format!("{e}"))),
        }
    }

    /// 受信方向へ STOP_SENDING を送出する
    pub(crate) async fn stop_sending(
        &mut self,
        error_code: u64,
    ) -> std::result::Result<(), QuicError> {
        let code = s2n_quic::application::Error::new(error_code)
            .map_err(|e| QuicError::Other(format!("failed to build an application error: {e}")))?;
        self.inner
            .stop_sending(code)
            .map_err(|e| QuicError::Other(format!("{e}")))
    }
}

/// ストリームを受理する側のハンドル
///
/// s2n-quic の受理 API は `&mut self` を取るため、`&self` の受理を提供するために
/// 方向ごとに `tokio::sync::Mutex` で包む。単方向と双方向は別のタスクが同時に待つため、
/// Mutex を分けて互いに待たせない。各方向の利用者は 1 つなので、ロックの取り合いは
/// 起きない。
pub struct StreamAcceptor {
    /// 双方向ストリームの受理
    bidi: Mutex<s2n_quic::connection::BidirectionalStreamAcceptor>,
    /// 単方向ストリームの受理
    uni: Mutex<s2n_quic::connection::ReceiveStreamAcceptor>,
}

impl StreamAcceptor {
    /// ピア起動の単方向ストリームを 1 つ受理する
    pub(crate) async fn accept_receive_stream(&self) -> std::result::Result<RecvStream, QuicError> {
        let mut uni = self.uni.lock().await;
        match uni.accept_receive_stream().await {
            Ok(Some(stream)) => Ok(RecvStream { inner: stream }),
            // 接続が閉じた
            Ok(None) => Err(QuicError::ConnectionClosed),
            Err(e) => Err(map_connection_error(e)),
        }
    }

    /// ピア起動の双方向ストリームを 1 つ受理する
    pub(crate) async fn accept_bidirectional_stream(
        &self,
    ) -> std::result::Result<(SendStream, RecvStream), QuicError> {
        let mut bidi = self.bidi.lock().await;
        match bidi.accept_bidirectional_stream().await {
            Ok(Some(stream)) => {
                // s2n-quic の `split` は (受信, 送信) の順で返す
                let (recv, send) = stream.split();
                Ok((SendStream { inner: send }, RecvStream { inner: recv }))
            }
            // 接続が閉じた
            Ok(None) => Err(QuicError::ConnectionClosed),
            Err(e) => Err(map_connection_error(e)),
        }
    }
}

/// 接続エラーを共通の QUIC エラーへ写す
fn map_connection_error(error: s2n_quic::connection::Error) -> QuicError {
    QuicError::Other(format!("{error}"))
}

/// 送信 API のエラーを共通の QUIC エラーへ写す
///
/// peer の STOP_SENDING は s2n-quic では `StreamReset` として返る。
fn map_send_error(error: s2n_quic::stream::Error) -> QuicError {
    match error {
        s2n_quic::stream::Error::StreamReset { error, .. } => QuicError::Stopped {
            error_code: u64::from(error),
        },
        e => QuicError::Other(format!("{e}")),
    }
}

/// QUIC クライアント接続を確立する
pub(crate) async fn connect(
    socket_addr: SocketAddr,
    server_name: &str,
    cert_path: Option<&str>,
) -> Result<(Connected, StopSendingReceiver)> {
    let tls = crate::tls::build_quic_client_config(cert_path, ALPN)?;
    let tls = s2n_rustls::Client::from(tls);

    let (stop_sending_tx, stop_sending_rx) = mpsc::unbounded_channel();
    let observer = ConnectionObserver {
        stop_sending_tx: Some(stop_sending_tx),
        ..ConnectionObserver::default()
    };
    let counters = observer.counters();

    let client = Client::builder()
        .with_tls(tls)
        .map_err(|e| TransportError::Quic(format!("TLS configuration failed: {e}")))?
        .with_io(crate::local_bind_addr(socket_addr))
        .map_err(|e| TransportError::Quic(format!("I/O binding failed: {e}")))?
        .with_datagram(datagram_endpoint()?)
        .map_err(|e| TransportError::Quic(format!("datagram provider failed: {e}")))?
        .with_event(observer)
        .map_err(|e| TransportError::Quic(format!("event provider failed: {e}")))?
        .start()
        .map_err(|e| TransportError::Quic(format!("client start failed: {e}")))?;

    let connection = client
        .connect(Connect::new(socket_addr).with_server_name(server_name))
        .await
        .map_err(|e| TransportError::Quic(format!("connection failed: {e}")))?;

    let selected = connection
        .application_protocol()
        .map_err(|e| TransportError::Quic(format!("failed to read the negotiated ALPN: {e}")))?;
    super::verify_alpn(Some(&selected))?;

    spawn_packet_diagnostics(&counters);

    tracing::info!("Connected to {socket_addr}");
    Ok((
        Connected {
            inner: connection,
            close_codes: None,
        },
        stop_sending_rx,
    ))
}

/// DATAGRAM の送受信キューの容量を決めた endpoint を作る
///
/// s2n-quic の既定はどちらも 0 であり、送信の容量が 0 のままだと `poll_send_datagram` が
/// 常に `Poll::Pending` を返すため、datagram を 1 つも送れない。
fn datagram_endpoint() -> Result<s2n_quic::provider::datagram::default::Endpoint> {
    Ok(s2n_quic::provider::datagram::default::Endpoint::builder()
        .with_send_capacity(64)
        .map_err(|e| TransportError::Quic(format!("datagram endpoint: {e}")))?
        .with_recv_capacity(64)
        .map_err(|e| TransportError::Quic(format!("datagram endpoint: {e}")))?
        .build()
        .expect("datagram endpoint build must succeed after capacities are set"))
}

/// 受信パケットの内訳を 1 秒ごとにログ出力する診断タスクを起動する
fn spawn_packet_diagnostics(counters: &PacketCounters) {
    if !super::packet_diag_enabled() {
        return;
    }
    tracing::warn!("packet diagnostics are enabled (MOQT_PACKET_DIAG=1)");
    let counters = counters.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            use std::sync::atomic::Ordering::Relaxed;
            tracing::warn!(
                "PKTDIAG received={} dropped={} decryption_failed={}",
                counters.received.load(Relaxed),
                counters.dropped.load(Relaxed),
                counters.decryption_failed.load(Relaxed),
            );
        }
    });
}

/// テスト用の QUIC サーバー
#[cfg(test)]
pub(crate) struct Server {
    /// s2n-quic のサーバー
    inner: S2nServer,
    /// 受理した全接続が共有する終了コードの記録
    close_codes: Arc<StdMutex<Vec<u64>>>,
}

#[cfg(test)]
impl Server {
    /// 待ち受けているアドレスを返す
    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.inner
            .local_addr()
            .expect("the local address must be available")
    }

    /// クライアントの接続を 1 つ受理する
    pub(crate) async fn accept(&mut self) -> Result<Connected> {
        let connection = self
            .inner
            .accept()
            .await
            .ok_or_else(|| TransportError::Quic("server stopped before connection".into()))?;
        Ok(Connected {
            inner: connection,
            close_codes: Some(Arc::clone(&self.close_codes)),
        })
    }
}

impl Default for PacketCounters {
    fn default() -> Self {
        Self {
            received: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            dropped: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            decryption_failed: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }
}

/// テスト用の QUIC サーバーを起動する
#[cfg(test)]
pub(crate) async fn start_server(tls: ServerTls, bind_addr: SocketAddr) -> Result<Server> {
    let server_tls = s2n_quic::provider::tls::rustls::Server::builder()
        .with_certificate(vec![tls.cert_der], tls.key_der)
        .map_err(|e| TransportError::Internal(format!("failed to set certificate: {e}")))?
        .with_application_protocols(tls.alpn.iter().map(|protocol| protocol.as_slice()))
        .map_err(|e| TransportError::Internal(format!("failed to set ALPN: {e}")))?
        .build()
        .map_err(|e| TransportError::Internal(format!("failed to build server TLS: {e}")))?;

    // IPv4 のみを待ち受ける (client は 127.0.0.1 へ接続する)
    let listener = std::net::UdpSocket::bind(bind_addr)
        .map_err(|e| TransportError::Internal(format!("failed to bind UDP socket: {e}")))?;
    let server_io = s2n_quic::provider::io::tokio::Builder::default()
        .with_rx_socket(listener)
        .map_err(|e| TransportError::Internal(format!("failed to set receive socket: {e}")))?
        .build()
        .map_err(|e| TransportError::Internal(format!("failed to build server I/O: {e}")))?;

    let close_codes = Arc::new(StdMutex::new(Vec::new()));
    let server = S2nServer::builder()
        .with_tls(server_tls)
        .map_err(|e| TransportError::Internal(format!("failed to set TLS: {e}")))?
        .with_io(server_io)
        .map_err(|e| TransportError::Internal(format!("failed to set I/O: {e}")))?
        .with_event(CloseCodeRecorder {
            codes: Arc::clone(&close_codes),
        })
        .map_err(|e| TransportError::Internal(format!("failed to set event: {e}")))?
        .start()
        .map_err(|e| TransportError::Internal(format!("failed to start server: {e}")))?;

    Ok(Server {
        inner: server,
        close_codes,
    })
}
