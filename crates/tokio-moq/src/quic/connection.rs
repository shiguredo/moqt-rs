//! tobi-quic でネイティブ QUIC 接続を確立する実装

use std::net::SocketAddr;
use std::sync::Arc;

use tobi_quic::{Client, ClientTlsConfig, Connect, Connection as TobiConnection, IoConfig};
#[cfg(any(test, feature = "test-support"))]
use tobi_quic::{Server as TobiServer, ServerTlsConfig};

use super::CloseReason;
#[cfg(any(test, feature = "test-support"))]
use super::ServerTls;
use super::tobi;
pub use super::tobi::RecvStream;
use super::{ALPN, QuicError, StopSendingReceiver};
use crate::error::{Result, TransportError};
use crate::multipath::{MultipathMode, MultipathOptions};

/// 接続を確立する前の接続
///
/// `split` でストリームを開くハンドルと、ストリームを受理するハンドルに分ける。
#[derive(Debug)]
pub struct Connected {
    /// tobi-quic の接続
    inner: TobiConnection,
    /// 接続の所有者がすべて drop されたときに接続を閉じるガード
    close_guard: Arc<CloseGuard>,
}

/// tobi-quic の接続を所有者の drop 時に閉じるガード
///
/// STOP_SENDING の観測タスクは接続ハンドルを保持するため、アプリ側のハンドルを drop
/// するだけでは driver が idle timeout まで残る。最後のガードが drop された時点で
/// close を送り、driver と観測タスクを終了させる。
#[derive(Debug)]
struct CloseGuard {
    connection: TobiConnection,
}

impl CloseGuard {
    fn new(connection: TobiConnection) -> Self {
        Self { connection }
    }
}

impl Drop for CloseGuard {
    fn drop(&mut self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        // Drop の中で close を待てないため、clone したハンドルを別タスクへ渡して
        // close の完了を待つ
        let connection = self.connection.clone();
        runtime.spawn(async move {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                connection.close(0, b"closed"),
            )
            .await;
        });
    }
}

impl Connected {
    /// 接続を操作ハンドルとストリーム受理ハンドルに分割する
    pub(crate) fn split(self) -> (Connection, StreamAcceptor) {
        let Self { inner, close_guard } = self;
        let (handle, acceptor) = inner.split();
        (
            Connection {
                inner: handle,
                _close_guard: Arc::clone(&close_guard),
            },
            StreamAcceptor {
                inner: Arc::new(acceptor),
                _close_guard: close_guard,
            },
        )
    }

    /// 接続元のアドレスを返す
    pub async fn remote_addr(&self) -> std::result::Result<SocketAddr, QuicError> {
        self.inner.remote_addr().await.map_err(tobi::map_error)
    }

    /// 競争に負けた接続を閉じる
    ///
    /// 接続を保持する `CloseGuard` を drop して、close と driver の終了を促す。
    pub(crate) fn close(self) {
        drop(self);
    }
}

/// ストリームを開く側の接続ハンドル
#[derive(Debug, Clone)]
pub struct Connection {
    /// tobi-quic の接続ハンドル
    inner: TobiConnection,
    /// 接続の所有者がすべて drop されたときに接続を閉じるガード
    _close_guard: Arc<CloseGuard>,
}

impl Connection {
    /// 新しい単方向ストリームを開く
    pub(crate) async fn open_send_stream_wait(&self) -> std::result::Result<SendStream, QuicError> {
        let stream = self
            .inner
            .open_send_stream_wait()
            .await
            .map_err(tobi::map_error)?;
        Ok(SendStream { inner: stream })
    }

    /// 新しい双方向ストリームを開く
    pub(crate) async fn open_bidirectional_stream_wait(
        &self,
    ) -> std::result::Result<(SendStream, RecvStream), QuicError> {
        let stream = self
            .inner
            .open_bidirectional_stream_wait()
            .await
            .map_err(tobi::map_error)?;
        let (send, recv) = stream.split();
        Ok((SendStream { inner: send }, RecvStream::new(recv)))
    }

    /// datagram を送信する
    ///
    /// 送信キューの空きができるまで待つ。待たずに捨てると、再送されない datagram が
    /// 失われる。
    pub(crate) async fn send_datagram_wait(
        &self,
        data: Vec<u8>,
    ) -> std::result::Result<(), QuicError> {
        self.inner
            .send_datagram_wait(data)
            .await
            .map_err(tobi::map_error)
    }

    /// datagram を 1 つ受信する
    pub(crate) async fn receive_datagram(&self) -> std::result::Result<Vec<u8>, QuicError> {
        self.inner.receive_datagram().await.map_err(tobi::map_error)
    }

    /// peer 起点のセッション終了コード (取得できた場合のみ)
    ///
    /// tobi-quic の接続ハンドルからは peer の application error code を同期的に取得できない。
    /// tobi-quic ではストリーム受信 / 受理 API のエラー (`QuicError::PeerClosed`) が
    /// コードを運ぶため、ここでは保持しない。
    pub(crate) fn peer_session_close(&self) -> Option<u64> {
        None
    }

    /// 接続が終了するまで待ち、終了理由を返す
    pub async fn closed(&self) -> Option<CloseReason> {
        self.inner.closed().await.map(|reason| match reason {
            tobi_quic::ConnectionError::Application { code, .. } => {
                CloseReason::Application { code }
            }
            tobi_quic::ConnectionError::Closed => CloseReason::Normal,
            other => CloseReason::Other(other.to_string()),
        })
    }
}

/// 接続をアプリケーションエラーで閉じる
pub(crate) async fn close_application(connection: &Connection, code: u64) -> Result<()> {
    tobi::close_application(&connection.inner, code).await?;
    Ok(())
}

/// 送信ストリーム
#[derive(Debug)]
pub struct SendStream {
    /// tobi-quic の送信ストリーム
    ///
    /// `tobi_quic::SendStream` は `Send + Sync` であり、`&SendStream` を async ブロック
    /// 越しに保持しても `tokio::spawn` できる。
    inner: tobi_quic::SendStream,
}

impl SendStream {
    /// データをすべて送信する
    pub(crate) async fn send_all(&mut self, data: &[u8]) -> std::result::Result<(), QuicError> {
        self.inner.send_all(data).await.map_err(tobi::map_error)
    }

    /// 送信方向を終了する (FIN)
    pub(crate) async fn finish(&mut self) -> std::result::Result<(), QuicError> {
        self.inner.finish().await.map_err(tobi::map_error)
    }

    /// 送信方向を reset する (RESET_STREAM)
    pub(crate) async fn reset(&mut self, error_code: u64) -> std::result::Result<(), QuicError> {
        self.inner
            .reset_stream(error_code)
            .await
            .map_err(tobi::map_error)
    }

    /// ストリーム ID を返す
    pub(crate) fn id(&self) -> u64 {
        self.inner.id()
    }
}

/// ストリームを受理する側のハンドル
///
/// tobi-quic の `StreamAcceptor` は単方向と双方向の両方を受け付けられ、受理は `&self` を
/// 取る。単方向と双方向を別のタスクから同時に待つため `Arc` で共有する。
#[derive(Debug)]
pub struct StreamAcceptor {
    /// tobi-quic のストリーム受理
    inner: Arc<tobi_quic::StreamAcceptor>,
    /// 接続の所有者がすべて drop されたときに接続を閉じるガード
    _close_guard: Arc<CloseGuard>,
}

impl StreamAcceptor {
    /// ピア起動の単方向ストリームを 1 つ受理する
    pub(crate) async fn accept_receive_stream(&self) -> std::result::Result<RecvStream, QuicError> {
        let stream = self
            .inner
            .accept_receive_stream()
            .await
            .map_err(tobi::map_error)?;
        Ok(RecvStream::new(stream))
    }

    /// ピア起動の双方向ストリームを 1 つ受理する
    pub(crate) async fn accept_bidirectional_stream(
        &self,
    ) -> std::result::Result<(SendStream, RecvStream), QuicError> {
        let stream = self
            .inner
            .accept_bidirectional_stream()
            .await
            .map_err(tobi::map_error)?;
        let (send, recv) = stream.split();
        Ok((SendStream { inner: send }, RecvStream::new(recv)))
    }
}

/// QUIC クライアント接続を確立する
///
/// `cert_path` が指定された場合はその CA 証明書で検証し、`insecure` が true の場合は
/// 検証をスキップする (開発用)。どちらも指定されない場合は OS のシステムルートストアで
/// 検証する。
///
/// `multipath.mode` が [`MultipathMode::Off`](crate::multipath::MultipathMode::Off) 以外の
/// 場合は Multipath QUIC を広告し、パスの使用方針とネットワーク監視を設定する。
pub(crate) async fn connect(
    socket_addr: SocketAddr,
    server_name: &str,
    cert_path: Option<&str>,
    insecure: bool,
    multipath: &MultipathOptions,
) -> Result<(Connected, StopSendingReceiver)> {
    let tls = crate::tls::build_quic_client_config(cert_path, insecure, ALPN)?;

    // qlog は QLOGDIR が設定されているときだけ有効にする (診断用)
    let qlog_factory = crate::quic::qlog::DirFactory::from_env();

    // Multipath を有効にする場合だけ、Path ID の上限と keep-alive を設定した
    // transport parameter を使う。パスの使い方とインターフェースのフィルタは
    // `with_multipath` の `MultipathConfig` が上書きする。
    let transport = if multipath.mode.is_enabled() {
        tobi::build_multipath_transport_config(multipath)
    } else {
        tobi::build_transport_config()
    };
    let scheduler = tobi::client_scheduler_config(multipath, transport.scheduler);

    // ビルダーに保持した設定は `start()` で固定順に適用されるため、呼ぶ順序に
    // 依存しない
    let mut builder = Client::builder()
        .with_tls(ClientTlsConfig::new(tls))
        .with_io(IoConfig::new(crate::local_bind_addr(socket_addr)))
        .with_transport(transport)
        .with_datagram(tobi::datagram_config())
        // tobi-quic の既定はコマンド駆動であり、アプリが API を呼んでいない間は受信と
        // タイマーを処理しない。そのままでは MOQT 層が受信待ちに入っている間に ACK や
        // 再送が止まるため、接続の生成時からバックグラウンド駆動を有効にする。
        .with_background_driver(true);
    if let Some(config) = tobi::build_multipath_config(multipath) {
        builder = builder.with_multipath(config);
    }
    if let Some(scheduler) = scheduler {
        builder = builder.with_scheduler(scheduler);
    }
    if multipath.mode == MultipathMode::Aggregate {
        // 輻輳制御はデータスケジューラーを明示した場合も従来どおりにする。
        builder = builder.with_congestion_control(tobi_quic::CongestionControlAlgorithm::BbrV3);
    }
    if multipath.mode.is_enabled()
        && let Some(interval) = multipath.network_monitor_interval
    {
        builder = builder.with_network_monitor(interval);
    }
    let client = match qlog_factory {
        Some(factory) => builder.with_qlog_writer_factory(factory),
        None => builder,
    };
    let client = client
        .start()
        .map_err(|e| TransportError::Quic(format!("client start failed: {e}")))?;

    let connection = client
        .connect(Connect::new(socket_addr, server_name))
        .await
        .map_err(|e| TransportError::Quic(format!("connection failed: {e}")))?;

    // tobi-quic は「ALPN 未確定 (None)」と「ドライバータスク停止などで取得できない
    // (Err)」を区別して返す。取得できない場合は接続が使えない状態なので、
    // 交渉失敗として扱わず取得失敗として返す
    let selected = connection
        .application_protocol()
        .await
        .map_err(|e| TransportError::Quic(format!("failed to read the negotiated ALPN: {e}")))?;
    super::verify_alpn(selected.as_deref())?;
    let close_guard = Arc::new(CloseGuard::new(connection.clone()));

    if multipath.has_scheduler_overrides() || multipath.loss_detection.is_some() {
        // RFC 10069 Section 2.1: Multipath 専用の方針を明示したときは、単一パスへ
        // フォールバックしないようにピアの Multipath 広告も確かめる。
        let peer = connection
            .peer_transport_params()
            .await
            .ok_or(TransportError::ConnectionClosed)?;
        if peer.initial_max_path_id.is_none() {
            return Err(TransportError::Unsupported(
                "the peer did not negotiate Multipath QUIC required by the explicit transmission policy"
                    .to_string(),
            ));
        }
    }
    if let Some(scheduler) = scheduler
        && multipath.has_scheduler_overrides()
    {
        tracing::info!(
            data_scheduler = scheduler.data.name(),
            ack_path = scheduler.ack.name(),
            retransmission = scheduler.retx.name(),
            "Configured Multipath QUIC scheduling"
        );
    }
    if let Some(loss_detection) = multipath.loss_detection {
        tracing::info!(?loss_detection, "Configured Multipath QUIC loss detection");
    }

    tobi::spawn_packet_diagnostics(&connection);
    let stop_sending_rx = tobi::spawn_connection_observer(&connection);

    if multipath.mode.is_enabled() {
        // 待機パスを middlebox のタイムアウトで失わないよう、すべてのパスに
        // keep-alive を設定する (RFC 10069 Section 5.9)。間隔を指定しない場合は
        // transport parameter の max_idle_timeout から間隔を決める。
        connection
            .keep_alive(true)
            .await
            .map_err(|e| TransportError::Quic(format!("failed to enable keep-alive: {e}")))?;
        // パスの開設・検証・切り替えは診断ログに出す。MOQT 層はパスを意識しない
        tobi::spawn_path_observer(&connection);
    }

    tracing::info!("Connected to {socket_addr}");
    Ok((
        Connected {
            inner: connection,
            close_guard,
        },
        stop_sending_rx,
    ))
}

/// テスト用の QUIC サーバー
#[cfg(any(test, feature = "test-support"))]
pub(crate) struct Server {
    /// tobi-quic のサーバー
    inner: TobiServer,
}

#[cfg(any(test, feature = "test-support"))]
impl Server {
    /// 待ち受けているアドレスを返す
    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.inner.local_addr()
    }

    /// クライアントの接続を 1 つ受理する
    pub(crate) async fn accept(&mut self) -> Result<Connected> {
        let connection = self.inner.accept().await.map_err(|e| {
            TransportError::Quic(format!("server failed to accept a connection: {e}"))
        })?;
        let close_guard = Arc::new(CloseGuard::new(connection.clone()));
        Ok(Connected {
            inner: connection,
            close_guard,
        })
    }
}

/// テスト用の QUIC サーバーを起動する
#[cfg(any(test, feature = "test-support"))]
pub(crate) async fn start_server(tls: ServerTls, bind_addr: SocketAddr) -> Result<Server> {
    start_server_with_multipath(tls, bind_addr, &MultipathOptions::default()).await
}

/// テスト用の QUIC サーバーを Multipath の設定付きで起動する
///
/// `multipath.mode` が `Off` 以外の場合は、クライアントから追加パスを受け付ける。
/// テストで Multipath のネゴシエーションと追加パスの検証を確かめるために使う。
#[cfg(any(test, feature = "test-support"))]
pub(crate) async fn start_server_with_multipath(
    tls: ServerTls,
    bind_addr: SocketAddr,
    multipath: &MultipathOptions,
) -> Result<Server> {
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(tls.cert_der)],
            rustls::pki_types::PrivateKeyDer::Pkcs8(tls.key_der.into()),
        )
        .map_err(|e| TransportError::Internal(format!("failed to set certificate: {e}")))?;
    config.alpn_protocols = tls.alpn;

    let transport = if multipath.mode.is_enabled() {
        tobi::build_multipath_transport_config(multipath)
    } else {
        tobi::build_transport_config()
    };
    let mut builder = TobiServer::builder()
        .with_tls(ServerTlsConfig::new(Arc::new(config)))
        .with_io(IoConfig::new(bind_addr))
        .with_transport(transport)
        .with_datagram(tobi::datagram_config())
        .with_background_driver(true);
    if let Some(config) = tobi::build_multipath_config(multipath) {
        builder = builder.with_multipath(config);
    }
    if multipath.mode == MultipathMode::Aggregate {
        // クライアントと同じく、Aggregate モードでは複数パスへ振り分けるスケジューラーと
        // BBR 系の輻輳制御を使う。
        builder = builder
            .with_scheduler(tobi::aggregate_scheduler_config())
            .with_congestion_control(tobi_quic::CongestionControlAlgorithm::BbrV3);
    }
    if multipath.mode.is_enabled()
        && let Some(interval) = multipath.network_monitor_interval
    {
        builder = builder.with_network_monitor(interval);
    }
    let server = builder
        .start()
        .map_err(|e| TransportError::Internal(format!("failed to start server: {e}")))?;
    Ok(Server { inner: server })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(windows))]
    use crate::multipath::MultipathMode;
    use crate::multipath::MultipathOptions;
    use std::time::Duration;
    #[cfg(not(windows))]
    use tobi_quic::{NewPath, PathState};

    /// テスト用のサーバー証明書を作る
    fn test_server_tls() -> ServerTls {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("自己署名証明書を生成できること");
        ServerTls {
            cert_der: certified.cert.der().to_vec(),
            key_der: certified.signing_key.serialize_der(),
            alpn: vec![ALPN.to_vec()],
        }
    }

    /// Multipath を有効にした接続を確立する
    async fn connect_multipath_pair(options: &MultipathOptions) -> (Connected, Connected) {
        let mut server = start_server_with_multipath(
            test_server_tls(),
            "127.0.0.1:0"
                .parse()
                .expect("ループバックアドレスのリテラルを解析できること"),
            options,
        )
        .await
        .expect("Multipath のサーバーを起動できること");
        let addr = server.local_addr();
        let (client, accepted) = tokio::join!(
            connect(addr, "localhost", None, true, options),
            server.accept(),
        );
        let (client, _stop_sending_rx) = client.expect("クライアントが接続できること");
        let accepted = accepted.expect("サーバーが接続を受理できること");
        (client, accepted)
    }

    /// Multipath を有効にすると Path ID の上限を広告し、追加パスが検証されること
    ///
    /// パスの使い方 (Handover / Aggregate) によらず、追加パスの検証とデータ転送が
    /// 成立することを確かめる。
    #[cfg(not(windows))]
    async fn assert_multipath_round_trip(options: MultipathOptions) {
        let (client, server) = connect_multipath_pair(&options).await;

        let params = client
            .inner
            .peer_transport_params()
            .await
            .expect("ハンドシェイクが完了していること");
        assert!(
            params.initial_max_path_id.is_some(),
            "Multipath を有効にしたサーバーが initial_max_path_id を広告すること"
        );

        // 別のループバックポートから追加パスを開く。サーバーは同じ接続で受理する
        let path = client
            .inner
            .open_path(NewPath::with_local_addr(
                "127.0.0.1:0"
                    .parse()
                    .expect("ループバックアドレスのリテラルを解析できること"),
            ))
            .await
            .expect("追加パスを開けること");

        // パスイベントは接続の観測タスクが消費するため、状態の遷移を paths() で待つ
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let paths = client.inner.paths().await;
            if paths
                .iter()
                .any(|snapshot| snapshot.handle == path && snapshot.state == PathState::Active)
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "追加パスが検証されて Active になること"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // パスを追加した後もストリームでデータを送受信できること
        let mut send = client
            .inner
            .open_send_stream()
            .await
            .expect("単方向ストリームを開けること");
        send.send_all(b"multipath")
            .await
            .expect("追加パスを経由してデータを送れること");
        send.finish().await.expect("ストリームを FIN できること");

        let mut recv = server
            .inner
            .accept_receive_stream()
            .await
            .expect("サーバーが単方向ストリームを受理できること");
        let chunk = recv
            .receive_vec(1024)
            .await
            .expect("Multipath 接続でデータを受信できること");
        assert_eq!(
            chunk.data, b"multipath",
            "追加パスの検証後もデータが欠けずに届くこと"
        );
    }

    /// Handover モードで追加パスを検証し、データを送受信できること
    #[cfg(not(windows))]
    #[tokio::test(flavor = "multi_thread")]
    async fn multipath_handover_validates_an_additional_path() {
        tokio::time::timeout(Duration::from_secs(10), async {
            assert_multipath_round_trip(MultipathOptions {
                mode: MultipathMode::Handover,
                initial_max_path_id: 4,
                keep_alive_interval: Some(Duration::from_secs(10)),
                ..MultipathOptions::default()
            })
            .await;
        })
        .await
        .expect("Handover の Multipath が制限時間内に完了すること");
    }

    /// Aggregate モードでも追加パスを検証し、データを送受信できること
    ///
    /// Aggregate モードでは複数パスへ振り分けるスケジューラーと BBR 系の輻輳制御を
    /// 使う。設定が接続の確立とデータ転送を壊さないことを確かめる。
    #[cfg(not(windows))]
    #[tokio::test(flavor = "multi_thread")]
    async fn multipath_aggregate_validates_an_additional_path() {
        tokio::time::timeout(Duration::from_secs(10), async {
            assert_multipath_round_trip(MultipathOptions {
                mode: MultipathMode::Aggregate,
                initial_max_path_id: 4,
                keep_alive_interval: Some(Duration::from_secs(10)),
                ..MultipathOptions::default()
            })
            .await;
        })
        .await
        .expect("Aggregate の Multipath が制限時間内に完了すること");
    }

    /// 送信と再送の方針を両方指定した状態でも、実 QUIC 接続で追加パスとデータ転送が成立すること
    #[cfg(not(windows))]
    #[tokio::test(flavor = "multi_thread")]
    async fn multipath_overrides_allow_additional_path() {
        use crate::multipath::{DataScheduler, RetransmissionPolicy};

        tokio::time::timeout(
            Duration::from_secs(10),
            assert_multipath_round_trip(MultipathOptions {
                mode: MultipathMode::Aggregate,
                data_scheduler: Some(DataScheduler::Redundant),
                retransmission: Some(RetransmissionPolicy::EarliestRecoveryPath),
                ..MultipathOptions::default()
            }),
        )
        .await
        .expect("方針を指定した Multipath 接続の転送が制限時間内に完了すること");
    }

    /// ピアが Multipath を広告しない接続は、方針を明示した場合に限り拒否すること
    #[cfg(not(windows))]
    #[tokio::test(flavor = "multi_thread")]
    async fn multipath_overrides_require_peer_negotiation() {
        use crate::multipath::{LossDetection, RetransmissionPolicy};

        tokio::time::timeout(Duration::from_secs(10), async {
            let mut server = start_server(test_server_tls(), SocketAddr::from(([127, 0, 0, 1], 0)))
                .await
                .expect("Multipath を無効にしたサーバーを起動できること");
            let addr = server.local_addr();
            let defaults = MultipathOptions {
                mode: MultipathMode::Aggregate,
                ..MultipathOptions::default()
            };
            // モードだけを指定した接続は、従来どおり単一パスにフォールバックできる。
            let (client, accepted) = tokio::join!(
                connect(addr, "localhost", None, true, &defaults),
                server.accept(),
            );
            let (client, _stop_sending) = client.expect("従来の接続は確立できること");
            let _accepted = accepted.expect("従来の接続を受理できること");
            client.close();

            // 再送ポリシーまたは損失検出の単独指定でも、Multipath を広告しないピアは拒否する。
            for options in [
                MultipathOptions {
                    retransmission: Some(RetransmissionPolicy::CrossPathImmediate),
                    ..defaults.clone()
                },
                MultipathOptions {
                    loss_detection: Some(LossDetection::Rfc9002),
                    ..defaults
                },
            ] {
                let (client, _accepted) = tokio::join!(
                    connect(addr, "localhost", None, true, &options),
                    server.accept(),
                );
                let error = client.expect_err("方針の指定時は単一パス接続を拒否すること");
                assert!(
                    matches!(error, TransportError::Unsupported(ref message)
                        if message.contains("did not negotiate Multipath QUIC")),
                    "Multipath のネゴシエーションが必要であることを示すこと: {error}"
                );
            }
        })
        .await
        .expect("接続の可否が制限時間内に判明すること");
    }

    /// Multipath を無効にすると Path ID の上限を広告しないこと
    #[tokio::test(flavor = "multi_thread")]
    async fn multipath_off_does_not_advertise_path_ids() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let options = MultipathOptions::default();
            let (client, _server) = connect_multipath_pair(&options).await;
            let params = client
                .inner
                .peer_transport_params()
                .await
                .expect("ハンドシェイクが完了していること");
            assert!(
                params.initial_max_path_id.is_none(),
                "Multipath を無効にした接続では initial_max_path_id を広告しないこと"
            );
        })
        .await
        .expect("Multipath を無効にした接続が制限時間内に確立すること");
    }
}
