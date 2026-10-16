//! MOQT peer (サーバー役) の in-process 試験基盤
//!
//! example の I/O 経路 (ハンドシェイク、peer 起点 request stream への送信、client 起点 request
//! stream の受理) を実行で検証するためのテスト専用モジュールである。s2n-quic のサーバーを
//! 127.0.0.1 で立て、sans-I/O な `Session` を `Session::new_server` で駆動する。モック /
//! スタブは使わない。
//!
//! ライブラリと example の公開 API だけを使う (`MoqtClient` の private な内部状態には依存
//! しない)。`#[cfg(test)]` からのみ使う。`#[cfg(test)]` の module は他クレートへ公開されない
//! ため、`examples/moq-pub` / `examples/moq-sub` からは参照できない (検証はこの crate の中で
//! 行う)。
//!
//! 対象は QUIC 直接接続 (`MoqtClient::establish_quic`) と QMux (`MoqtClient::establish_qmux`)
//! である。QMux は TCP + TLS を待ち受ける [`QmuxTestPeerServer`] を使う。WebTransport over
//! HTTP/3 / HTTP/2 のハーネスは必要になった時点で別途追加する。
//!
//! 提供するのは SETUP 交換 (送る SETUP の差し替え、SETUP を送らない control stream の終端を
//! 含む)、peer 起点 request stream へのメッセージ送信、peer 起点 request stream に届く応答の
//! 読み出し、client 起点 request stream の受理と REQUEST_OK による応答、client が送る終了
//! コード (CONNECTION_CLOSE) の観測である。次は提供しない (必要になった時点で拡張する)。
//!
//! - peer が送る STOP_SENDING の観測 (QMux 経路は `QmuxTestPeerServer` と `qmux` モジュールの
//!   テストで観測する)

use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use bytes::Bytes;

use shiguredo_moqt::message::ControlMessage;
use shiguredo_moqt::message::Setup;
use shiguredo_moqt::message::common::Location;
use shiguredo_moqt::message_parameter::MessageParameters;
use shiguredo_moqt::parameter::{SetupOption, SetupOptionValue, SetupOptions};
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::{SessionEvent, SessionState, Transport as MoqtTransport};
use shiguredo_moqt::stream::encode_control_stream_setup;
use shiguredo_moqt::track_properties::TrackProperties;

use crate::error::{Result, TransportError};
use crate::moqt_client::{ControlStream, StreamRead};
use crate::transport::{BidiStreamAcceptor, SendStream, StreamAcceptor, StreamHandle};

// 以下の 3 つは `moqt_client` の対応する private 関数と同じ処理を含む。テスト専用の都合で
// 通常ビルド対象の可視性を広げないため (`shiguredo-rust` の「テストのために private な API を
// 無理矢理公開しないこと」)、あえて重複させている。`request_stream_id` は
// `moqt_client::request_id_of` に加えて `ControlMessage::Unsupported` の Request ID も扱う
// (未対応 request の応答を読むために必要)。

/// 初期イベントとして積まれた SETUP を取り出す
///
/// `Session::new_server` は生成時に SETUP を `SessionEvent::SendControl` として積む
/// (draft-ietf-moq-transport-22 §9.1 (SETUP))。
fn pop_send_control(session: &mut Session) -> Result<ControlMessage> {
    match session.poll_event() {
        Some(SessionEvent::SendControl(message)) => Ok(message),
        other => Err(TransportError::Internal(format!(
            "expected SendControl as initial event, got {other:?}"
        ))),
    }
}

/// 新規制御ストリームの先頭に SETUP を書き出す
///
/// 制御ストリームの先頭には stream type prefix が必要である
/// (draft-ietf-moq-transport-22 §6.4.1 (Unidirectional Streams))。prefix を前置する
/// `encode_control_stream_setup` を使う。ストリームを開くのは呼び出し側である。
async fn send_control_stream_setup(
    control_send: &mut SendStream,
    setup: &ControlMessage,
) -> Result<()> {
    control_send
        .send(Bytes::from(encode_control_stream_setup(setup)?))
        .await?;
    Ok(())
}

/// 先頭メッセージから request stream の Request ID を取り出す
///
/// draft-ietf-moq-transport-22 §6.3 (Session initialization) は request stream の先頭に
/// 7 種類のメッセージを許す。本 example が request として扱う 4 種類 (SUBSCRIBE / PUBLISH /
/// FETCH / TRACK_STATUS) はそれぞれの構造体が Request ID を持ち、本実装が扱わない 3 種類は
/// `ControlMessage::Unsupported` が持つ。応答専用の型は持たないため `None` になる。
fn request_stream_id(message: &ControlMessage) -> Option<u64> {
    match message {
        ControlMessage::Subscribe(m) => Some(m.request_id),
        ControlMessage::Publish(m) => Some(m.request_id),
        ControlMessage::Fetch(m) => Some(m.request_id),
        ControlMessage::TrackStatus(m) => Some(m.request_id),
        ControlMessage::Unsupported { request_id, .. } => *request_id,
        _ => None,
    }
}

/// client が送る終了コードの観測を待つ上限時間
///
/// テスト全体の上限 ([`tests::TEST_TIMEOUT`]) より短くし、終了コードを送らない退行を
/// 「終了コードが届かない」として切り分けられるようにする。
const CLOSE_CODE_TIMEOUT: Duration = Duration::from_secs(5);

/// client が送る CONNECTION_CLOSE の終了コードを記録する購読者
///
/// s2n-quic は application close を `s2n_quic::connection::Error::Application` として
/// `on_connection_closed` に届ける (`initiator` が `Remote` なら peer 起点である)。peer 側で
/// 終了コードを観測する唯一の経路であり、example が終了コード付きで閉じたことをテストが
/// 確かめるために使う。
///
/// 記録は `std::sync::Mutex` で保護する。購読者は s2n-quic の駆動スレッドから同期で呼ばれ、
/// `tokio::sync::Mutex` を await できないため、ロックを保持する区間を「Vec へ 1 件 push する」
/// だけに限って使う。
#[derive(Debug, Default)]
struct CloseCodeRecorder {
    /// 観測した終了コード
    codes: Arc<StdMutex<Vec<u64>>>,
}

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
                .expect("終了コードの記録を壊さない")
                .push(u64::from(*error));
        }
    }
}

/// client の接続を受理する前の in-process peer (s2n-quic サーバー)
pub(crate) struct TestPeerServer {
    /// 接続を受理する s2n-quic サーバー
    server: s2n_quic::Server,
    /// 待ち受けているアドレス
    local_addr: SocketAddr,
    /// client が送る終了コードの記録 (サーバーごとに 1 つで、受理した全接続が共有する)
    close_codes: Arc<StdMutex<Vec<u64>>>,
}

impl TestPeerServer {
    /// s2n-quic サーバーを 127.0.0.1 で起動する
    ///
    /// サーバー証明書は自己署名を rcgen で生成し、ALPN には MOQT のプロトコル識別子を広告する。
    /// client は ALPN で [`crate::MOQT_PROTOCOL`] を要求するため、既定の `h3` のままだと
    /// ハンドシェイクが HANDSHAKE_FAILURE で失敗する。
    pub(crate) async fn start() -> Result<Self> {
        let certified =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).map_err(|e| {
                TransportError::Internal(format!("failed to generate certificate: {e}"))
            })?;
        let server_tls = s2n_quic::provider::tls::rustls::Server::builder()
            .with_certificate(
                vec![certified.cert.der().to_vec()],
                certified.signing_key.serialize_der(),
            )
            .map_err(|e| TransportError::Internal(format!("failed to set certificate: {e}")))?
            .with_application_protocols([crate::MOQT_PROTOCOL].into_iter())
            .map_err(|e| TransportError::Internal(format!("failed to set ALPN: {e}")))?
            .build()
            .map_err(|e| TransportError::Internal(format!("failed to build server TLS: {e}")))?;

        // IPv4 のみを待ち受ける (client は 127.0.0.1 へ接続する)
        let listener = std::net::UdpSocket::bind("127.0.0.1:0")
            .map_err(|e| TransportError::Internal(format!("failed to bind UDP socket: {e}")))?;
        let local_addr = listener
            .local_addr()
            .map_err(|e| TransportError::Internal(format!("failed to get local address: {e}")))?;
        let server_io = s2n_quic::provider::io::tokio::Builder::default()
            .with_rx_socket(listener)
            .map_err(|e| TransportError::Internal(format!("failed to set receive socket: {e}")))?
            .build()
            .map_err(|e| TransportError::Internal(format!("failed to build server I/O: {e}")))?;

        let close_codes = Arc::new(StdMutex::new(Vec::new()));
        let server = s2n_quic::Server::builder()
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

        Ok(Self {
            server,
            local_addr,
            close_codes,
        })
    }

    /// 待ち受けているアドレス
    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// client の接続を 1 つ受理する
    pub(crate) async fn accept(&mut self) -> Result<TestPeer> {
        let connection = self
            .server
            .accept()
            .await
            .ok_or_else(|| TransportError::Quic("server stopped before connection".into()))?;
        TestPeer::new(connection, Arc::clone(&self.close_codes))
    }
}

/// QMux (TCP + TLS) のテスト用サーバー
///
/// [`TestPeerServer`] の QMux 版である。UDP を使わない経路を実行で検証するために置く。
/// TCP を 127.0.0.1 で待ち受け、TLS (ALPN は [`crate::MOQT_PROTOCOL`]) で受理してから
/// `tokio_qmux::Connection` を作る。QMux 自体はハンドシェイクを持たないため、接続が
/// 確立した時点でクライアントの制御ストリームを受理できる。
pub(crate) struct QmuxTestPeerServer {
    /// client の接続を待ち受ける TCP リスナー
    listener: tokio::net::TcpListener,
    /// TLS の受理器 (server 証明書は自己署名)
    tls: tokio_rustls::TlsAcceptor,
    /// 待ち受けているアドレス
    local_addr: SocketAddr,
}

impl QmuxTestPeerServer {
    /// 127.0.0.1 で待ち受けるテスト用の QMux サーバーを立てる
    pub(crate) async fn start() -> Result<Self> {
        let certified =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).map_err(|e| {
                TransportError::Internal(format!("failed to generate certificate: {e}"))
            })?;
        // ALPN は client と同じ MOQT のプロトコル識別子を広告する。QMux は ALPN を定義せず、
        // 下位トランスポートを接続そのもので区別するためである
        let mut tls_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![certified.cert.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(
                    certified.signing_key.serialize_der().into(),
                ),
            )
            .map_err(|e| TransportError::Internal(format!("failed to set certificate: {e}")))?;
        tls_config.alpn_protocols = vec![crate::MOQT_PROTOCOL.as_bytes().to_vec()];

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| TransportError::Internal(format!("failed to bind TCP socket: {e}")))?;
        let local_addr = listener
            .local_addr()
            .map_err(|e| TransportError::Internal(format!("failed to get local address: {e}")))?;

        Ok(Self {
            listener,
            tls: tokio_rustls::TlsAcceptor::from(Arc::new(tls_config)),
            local_addr,
        })
    }

    /// 待ち受けているアドレス
    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// client の接続を 1 つ受理し、QMux 接続を確立する
    pub(crate) async fn accept(&mut self) -> Result<TestPeer> {
        let (tcp, _) = self
            .listener
            .accept()
            .await
            .map_err(|e| TransportError::Internal(format!("failed to accept TCP: {e}")))?;
        tcp.set_nodelay(true)
            .map_err(|e| TransportError::Internal(format!("failed to set nodelay: {e}")))?;
        let tls = self
            .tls
            .accept(tcp)
            .await
            .map_err(|e| TransportError::Internal(format!("failed to accept TLS: {e}")))?;
        let connection = crate::qmux::build_server_connection(tls)
            .map_err(|e| TransportError::Internal(format!("failed to build QMux: {e}")))?;
        TestPeer::new_qmux(crate::qmux::split_connection(
            connection,
            &tokio_metrics::TaskMonitor::new(),
        ))
    }
}

/// peer から受理した request stream
///
/// 送信半と reader の両方を保持する。s2n-quic では送信半の drop で FIN、受信半の drop で
/// STOP_SENDING が送られるため、保持しないと request の終端
/// (draft-ietf-moq-transport-22 §6.4.2.2 (Graceful Request Stream Closure) /
/// §6.4.2.3 (Request Cancellation and Rejection)) が peer に観測される。テストが終わるまで
/// 保持する。
struct AcceptedRequest {
    /// 受理した request の Request ID
    request_id: u64,
    /// 応答を書く送信半
    send: SendStream,
    /// 後続メッセージを読む reader
    ///
    /// フィールドは読み出さないが、この値が drop されるまで受信半を保持する。
    #[expect(
        dead_code,
        reason = "受信半を drop すると STOP_SENDING が peer へ届くため保持する"
    )]
    reader: ControlStream,
}

/// peer が開いた request stream
///
/// 送信半と reader を保持する。drop すると送信半は FIN、受信半は STOP_SENDING として peer に
/// 観測されるため、テストが終わるまで保持する。
struct OpenedRequest {
    /// 先頭メッセージの Request ID (先頭メッセージが request でない場合は `None`)
    request_id: Option<u64>,
    /// 送信半 (drop すると FIN が peer へ届くため保持する)
    send: SendStream,
    /// 応答を読む reader
    reader: ControlStream,
}

/// 受理済みの in-process peer (MOQT のサーバー役)
pub(crate) struct TestPeer {
    /// sans-I/O なサーバー側 session
    session: Session,
    /// ストリームを開く側のハンドル
    handle: StreamHandle,
    /// peer 起動の双方向ストリームを受理する側
    bidi_acceptor: BidiStreamAcceptor,
    /// 単方向ストリームを受理する側
    recv_acceptor: StreamAcceptor,
    /// 自側 control stream (uni) の送信半
    ///
    /// フィールドは読み出さないが、この値が drop されるまで SETUP を送ったストリームを
    /// 保持する (drop すると制御ストリームが終端され、peer がセッション終了として観測する)。
    control_send: Option<SendStream>,
    /// 自側 control stream (uni) の受信半
    ///
    /// フィールドは読み出さないが、この値が drop されるまで受信半を保持する。
    control_recv: Option<ControlStream>,
    /// 自側が開いた request stream
    ///
    /// 送信半の drop は FIN、reader の drop は STOP_SENDING として peer に観測されるため保持する。
    opened_requests: Vec<OpenedRequest>,
    /// peer から受理した request stream
    accepted_requests: Vec<AcceptedRequest>,
    /// client が送る終了コードの記録
    close_codes: Arc<StdMutex<Vec<u64>>>,
}

impl TestPeer {
    /// 確立した QMux 接続から peer を作る
    ///
    /// QUIC 経路と違い、ストリームの受理は接続を共有した別タスクが行い、その結果が
    /// `uni_rx` / `bi_rx` に流れる ([`crate::qmux::split_connection`])。
    fn new_qmux(session: crate::qmux::QmuxSession) -> Result<Self> {
        let crate::qmux::QmuxSession {
            handle,
            uni_rx,
            bi_rx,
            // peer 側は client の STOP_SENDING を観測しない (観測するのは client 側)
            stop_sending_rx: _,
        } = session;
        let session = Session::new_server(MoqtTransport::Qmux, SetupOptions::new())?;
        Ok(Self {
            session,
            handle: StreamHandle::Qmux(handle),
            bidi_acceptor: BidiStreamAcceptor::Qmux { bi_rx },
            recv_acceptor: StreamAcceptor::Qmux { uni_rx },
            control_send: None,
            control_recv: None,
            opened_requests: Vec::new(),
            accepted_requests: Vec::new(),
            // QMux 経路は終了コードの観測タスクを持たない (QUIC の ConnectionObserver に相当する
            // 仕組みが無い)。必要になった時点で `Connection::closed` を待つタスクを足す
            close_codes: Arc::new(StdMutex::new(Vec::new())),
        })
    }

    /// 受理した QUIC 接続から peer を作る
    fn new(
        connection: s2n_quic::connection::Connection,
        close_codes: Arc<StdMutex<Vec<u64>>>,
    ) -> Result<Self> {
        let (handle, acceptor) = connection.split();
        let (bidi_acceptor, recv_acceptor) = acceptor.split();
        let session = Session::new_server(MoqtTransport::Quic, SetupOptions::new())?;
        Ok(Self {
            session,
            handle: StreamHandle::Quic(handle),
            bidi_acceptor: BidiStreamAcceptor::Quic(bidi_acceptor),
            recv_acceptor: StreamAcceptor::Quic(recv_acceptor),
            control_send: None,
            control_recv: None,
            opened_requests: Vec::new(),
            accepted_requests: Vec::new(),
            close_codes,
        })
    }

    /// SETUP を交換してセッションを確立する
    ///
    /// 自側の control stream (uni) を開いて SETUP を送り、client の control stream を受理して
    /// stream type を検証してから SETUP を取り込む。SETUP を送らないと client のハンドシェイクは
    /// 完了しない (`MoqtClient::establish_quic` は peer SETUP を待つ)。client の SETUP を
    /// 取り込まないと session は `Established` にならず、`Session::recv_request` が
    /// `BeforeSessionEstablished` を返す。
    pub(crate) async fn establish(&mut self) -> Result<()> {
        // 新規制御ストリームの先頭には stream type prefix が必要である
        let setup = pop_send_control(&mut self.session)?;
        let mut control_send = self.handle.open_send_stream().await?;
        send_control_stream_setup(&mut control_send, &setup).await?;

        // client の control stream を受理し、stream type を検証してから SETUP を取り込む
        let recv = self
            .recv_acceptor
            .accept_recv_stream()
            .await?
            .ok_or_else(|| {
                TransportError::Quic("connection closed before client control stream".into())
            })?;
        let mut control_recv = ControlStream::new(recv);
        let stream_type = match control_recv.read_stream_type().await? {
            StreamRead::Value(stream_type) => stream_type,
            StreamRead::Closed(end) => {
                let _ = self.session.recv_control_stream_closed(end);
                return Err(TransportError::Internal(
                    "client control stream closed before stream type".into(),
                ));
            }
        };
        self.session.recv_control_stream_type(stream_type)?;
        let peer_setup = match control_recv.recv_message().await? {
            StreamRead::Value(message) => message,
            StreamRead::Closed(end) => {
                let _ = self.session.recv_control_stream_closed(end);
                return Err(TransportError::Internal(
                    "client control stream closed before SETUP".into(),
                ));
            }
        };
        self.session.recv_control(peer_setup)?;

        self.control_send = Some(control_send);
        self.control_recv = Some(control_recv);

        // 自側 SETUP の送信と client SETUP の取り込みで Established へ遷移する
        while let Some(event) = self.session.poll_event() {
            if matches!(event, SessionEvent::Established) {
                return Ok(());
            }
        }
        Err(TransportError::Internal(
            "session did not become established".into(),
        ))
    }

    /// peer 起点の request stream を開き、先頭メッセージとして `message` を送る
    ///
    /// request stream の先頭メッセージは stream type prefix を付けない
    /// (draft-ietf-moq-transport-22 §6.3 (Session initialization))。メッセージの種類は問わない
    /// (request 以外を送って example の種別判定を検証できる)。Request ID を含む request を
    /// 送る場合は、peer 役割の parity に合わせて [`Self::next_request_id`] で採番した値を
    /// 渡すこと。
    pub(crate) async fn send_request_start(&mut self, message: ControlMessage) -> Result<()> {
        let (mut send, recv) = self.handle.open_bidi_stream().await?;
        send.send(Bytes::from(message.encode()?)).await?;
        self.opened_requests.push(OpenedRequest {
            request_id: request_stream_id(&message),
            send,
            reader: ControlStream::new(recv),
        });
        Ok(())
    }

    /// peer 起動の uni stream を 1 つ受理する
    ///
    /// 受理したストリームの受信方向から STOP_SENDING を送るテストが使う
    /// ([`Self::recv_uni_data_stream`] は終端まで読み切るため、ストリームを返さない)。
    pub(crate) async fn accept_recv_stream(&mut self) -> Result<crate::transport::RecvStream> {
        self.recv_acceptor
            .accept_recv_stream()
            .await?
            .ok_or_else(|| TransportError::Internal("data stream acceptor closed".into()))
    }

    /// datagram を 1 つ送る
    ///
    /// 音声の OBJECT_DATAGRAM が QMux 経由で届くことを検証するテストが使う。
    pub(crate) async fn send_datagram(&self, data: &[u8]) -> Result<()> {
        self.handle.send_datagram(data).await
    }

    /// datagram を 1 つ以上受信する
    ///
    /// 1 件届くまで待ち、到着済みをまとめて返す (`StreamHandle::recv_datagrams` と同じ契約)。
    pub(crate) async fn recv_datagrams(&self) -> Result<Vec<Vec<u8>>> {
        self.handle.recv_datagrams().await
    }

    /// peer が開いた request stream に届いたメッセージを 1 つ読む
    ///
    /// `request_id` は [`Self::send_request_start`] に渡したメッセージの Request ID である。
    /// example が request に応答したこと (REQUEST_ERROR など) を確認するために使う。
    /// その Request ID の request stream を開いていない場合と、応答の前に終端された場合は
    /// `TransportError::Internal` を返す。
    pub(crate) async fn recv_response(&mut self, request_id: u64) -> Result<ControlMessage> {
        let request = self
            .opened_requests
            .iter_mut()
            .find(|request| request.request_id == Some(request_id))
            .ok_or_else(|| {
                TransportError::Internal(format!("request stream not found: {request_id}"))
            })?;
        match request.reader.recv_message().await? {
            StreamRead::Value(message) => Ok(message),
            StreamRead::Closed(_) => Err(TransportError::Internal(
                "request stream closed before the response".into(),
            )),
        }
    }

    /// client から request stream の先頭メッセージを受理する
    ///
    /// 受理したメッセージは `Session::recv_request` へ渡す。返り値はテストが内容を確認する
    /// ために返す。
    pub(crate) async fn recv_request_start(&mut self) -> Result<ControlMessage> {
        let (send, recv) = self
            .bidi_acceptor
            .accept_bidi_stream()
            .await?
            .ok_or_else(|| {
                TransportError::Quic("connection closed before peer request stream".into())
            })?;
        let mut reader = ControlStream::new(recv);
        let message = match reader.recv_message().await? {
            StreamRead::Value(message) => message,
            StreamRead::Closed(_) => {
                return Err(TransportError::Internal(
                    "peer request stream closed before first message".into(),
                ));
            }
        };
        let request_id = request_stream_id(&message).ok_or_else(|| {
            TransportError::Internal(format!(
                "peer request start message is not a request: {message:?}"
            ))
        })?;
        self.session
            .recv_request(message.clone())
            .map_err(|e| TransportError::Internal(format!("failed to accept request: {e}")))?;
        self.accepted_requests.push(AcceptedRequest {
            request_id,
            send,
            reader,
        });
        Ok(message)
    }

    /// 受理した request に REQUEST_OK で応答する
    ///
    /// `Session::send_request_ok` は応答を `SessionEvent::SendOnStream` として積むため、該当する
    /// request stream へ書き出す。応答を返さないと client の API (`MoqtClient::publish_track`
    /// など) は応答待ちのまま解決しない。
    pub(crate) async fn send_request_ok(&mut self, request_id: u64) -> Result<()> {
        self.session.send_request_ok(
            request_id,
            MessageParameters::new(),
            TrackProperties::new(),
        )?;
        self.flush_queued_response(request_id).await
    }

    /// Session が積んだ応答 (`SessionEvent::SendOnStream`) を該当する request stream へ書く
    ///
    /// 応答を返さないと client の API (`MoqtClient::publish_track` など) は応答待ちのまま
    /// 解決しない。他の request への送信イベントは待ち合わせに影響しないため読み飛ばす
    /// (複数 request へ連続して応答する場合は保持が必要になる)。
    async fn flush_queued_response(&mut self, request_id: u64) -> Result<()> {
        let (message, fin) = loop {
            match self.session.poll_event() {
                Some(SessionEvent::SendOnStream {
                    request_id: id,
                    message,
                    fin,
                }) if id == request_id => break (message, fin),
                // セッション終了は応答を送れない原因として返す (無視すると、以後の送信の失敗
                // だけが残って原因が分からなくなる)
                Some(SessionEvent::CloseSession(error)) => {
                    return Err(TransportError::Internal(format!(
                        "session closed before responding to request {request_id}: {error}"
                    )));
                }
                // 他の request への送信イベントは応答の待ち合わせに影響しないため読み飛ばす
                // (複数 request へ連続して応答する場合は保持が必要になる)
                Some(_) => continue,
                None => {
                    return Err(TransportError::Internal(format!(
                        "no response queued for request {request_id}"
                    )));
                }
            }
        };
        let request = self
            .accepted_requests
            .iter_mut()
            .find(|request| request.request_id == request_id)
            .ok_or_else(|| {
                TransportError::Internal(format!("request stream not found: {request_id}"))
            })?;
        request.send.send(Bytes::from(message.encode()?)).await?;
        if fin {
            request.send.finish().await?;
        }
        Ok(())
    }

    /// 受理した SUBSCRIBE に SUBSCRIBE_OK で応答する
    ///
    /// [`TestPeer::send_request_ok`] と同じ手順で応答を書き出す。SUBSCRIBE_OK は
    /// track alias を運ぶため、REQUEST_OK とは別の API を使う。
    pub(crate) async fn send_subscribe_ok(
        &mut self,
        request_id: u64,
        track_alias: u64,
    ) -> Result<()> {
        self.session.send_subscribe_ok(
            request_id,
            track_alias,
            MessageParameters::new(),
            TrackProperties::new(),
        )?;
        self.flush_queued_response(request_id).await
    }

    /// 受理した FETCH に FETCH_OK で応答する
    ///
    /// draft-ietf-moq-transport-22 §9.12 (FETCH_OK)。End Location は publisher が返す範囲の
    /// 終端で、End of Track は track の終端まで返し切ったことを示す (0 は未達)。
    pub(crate) async fn send_fetch_ok(
        &mut self,
        request_id: u64,
        end_location: Location,
        end_of_track: u8,
    ) -> Result<()> {
        self.session
            .send_fetch_ok(
                request_id,
                end_of_track,
                end_location,
                MessageParameters::new(),
                TrackProperties::new(),
            )
            .map_err(|e| TransportError::Internal(format!("send_fetch_ok: {e}")))?;
        self.flush_queued_response(request_id).await
    }

    /// 自側が開く uni stream を開き、送信ハンドルを返す
    ///
    /// FETCH 応答 (draft-ietf-moq-transport-22 §11.4.1) のように、peer が自ら uni stream を
    /// 開いて書く経路をテストで再現するために使う。
    pub(crate) async fn open_uni_send_stream(&mut self) -> Result<crate::transport::SendStream> {
        self.handle.open_send_stream().await
    }

    /// 自側 control stream に GOAWAY を書く
    ///
    /// draft-ietf-moq-transport-22 §9.2 (GOAWAY)。`new_session_uri` が空でない場合は、
    /// 受け取った client がその URI へ移行する (server 役だけが非空 URI を送れる)。
    /// `timeout_ms` は sender が閉じるまでの時間で、0 は期限なしを意味する。
    pub(crate) async fn send_goaway(
        &mut self,
        new_session_uri: Vec<u8>,
        timeout_ms: u64,
    ) -> Result<()> {
        self.session
            .send_goaway(new_session_uri, timeout_ms)
            .map_err(|e| TransportError::Internal(format!("send_goaway: {e}")))?;
        // 制御メッセージは SendControl として積まれるため、取り出して control stream へ書く
        let message = loop {
            match self.session.poll_event() {
                Some(SessionEvent::SendControl(message)) => break message,
                Some(SessionEvent::CloseSession(error)) => {
                    return Err(TransportError::Internal(format!(
                        "session closed before sending GOAWAY: {error}"
                    )));
                }
                // 他のイベントは GOAWAY の書き出しに影響しないため読み飛ばす
                Some(_) => continue,
                None => {
                    return Err(TransportError::Internal(
                        "no GOAWAY queued for the control stream".into(),
                    ));
                }
            }
        };
        let control_send = self
            .control_send
            .as_mut()
            .ok_or_else(|| TransportError::Internal("control stream is not opened".into()))?;
        control_send.send(Bytes::from(message.encode()?)).await?;
        Ok(())
    }

    /// peer が開いた request stream の送信方向を FIN で閉じる
    ///
    /// example が拒否した request のストリーム終端を通知する経路を確かめるために使う。
    pub(crate) async fn finish_request_stream(&mut self, request_id: u64) -> Result<()> {
        let request = self
            .opened_requests
            .iter_mut()
            .find(|request| request.request_id == Some(request_id))
            .ok_or_else(|| {
                TransportError::Internal(format!("request stream not found: {request_id}"))
            })?;
        request.send.finish().await?;
        Ok(())
    }

    /// client が開いた uni data stream を受理し、FIN まで読む
    ///
    /// FETCH 応答や subgroup stream の内容を、client が書いたバイト列のまま取り出す。
    pub(crate) async fn recv_uni_data_stream(&mut self) -> Result<Vec<u8>> {
        let mut stream = self
            .recv_acceptor
            .accept_recv_stream()
            .await?
            .ok_or_else(|| TransportError::Internal("data stream acceptor closed".into()))?;
        let mut buf = Vec::new();
        while let crate::transport::RecvChunk::Data(data) = stream.receive_chunk().await? {
            buf.extend_from_slice(&data);
        }
        Ok(buf)
    }

    /// 自側の次の Request ID を採番する (server 役は奇数)
    pub(crate) fn next_request_id(&mut self) -> Result<u64> {
        Ok(self.session.next_local_request_id()?)
    }

    /// サーバー側 session の状態
    pub(crate) fn session_state(&self) -> SessionState {
        self.session.state()
    }

    /// 自側 control stream の先頭に書く完全なバイト列を送る
    ///
    /// `ControlMessage::encode` は stream type prefix を含まないため、SETUP を差し替える
    /// テストは prefix を含むバイト列
    /// (`shiguredo_moqt::stream::encode_control_stream_setup`) を作って渡す。prefix を誤らせて
    /// `Session::recv_control_stream_type` の失敗を再現することもできる。
    ///
    /// 受け取った側は先頭の varint を stream type として読むため、空のバイト列を送ると
    /// stream type を読む前に終端する (FIN は [`Self::finish_control_stream`] で送る)。
    pub(crate) async fn send_control_stream_head(&mut self, head: Vec<u8>) -> Result<()> {
        let mut control_send = self.handle.open_send_stream().await?;
        if !head.is_empty() {
            control_send.send(Bytes::from(head)).await?;
        }
        self.control_send = Some(control_send);
        Ok(())
    }

    /// 自側 control stream の送信方向を FIN で終端する
    pub(crate) async fn finish_control_stream(&mut self) -> Result<()> {
        let mut control_send = self
            .control_send
            .take()
            .ok_or_else(|| TransportError::Internal("control stream is not opened".into()))?;
        control_send.finish().await?;
        Ok(())
    }

    /// 自側の SETUP を指定した `SetupOptions` で送る
    ///
    /// ハーネスの session は `SetupOptions::new()` のままにする (server 役の SETUP に PATH を
    /// 渡すと `Session::new_server` が `INVALID_PATH` で拒否するため)。送る SETUP だけを
    /// 差し替えて、client の検証失敗を再現する。
    pub(crate) async fn send_setup(&mut self, options: &SetupOptions) -> Result<()> {
        let setup = ControlMessage::Setup(Setup {
            options: options.clone(),
        });
        let head = shiguredo_moqt::stream::encode_control_stream_setup(&setup)?;
        self.send_control_stream_head(head).await
    }

    /// peer が観測した client の終了コード (CONNECTION_CLOSE) を待つ
    ///
    /// example が終了コードを送らずに接続を切った場合と、上限時間までに届かなかった場合は
    /// `TransportError::Internal` を返す。
    pub(crate) async fn recv_close_code(&mut self) -> Result<u64> {
        let deadline = Instant::now() + CLOSE_CODE_TIMEOUT;
        loop {
            if let Some(code) = self
                .close_codes
                .lock()
                .expect("終了コードの記録を壊さない")
                .first()
                .copied()
            {
                return Ok(code);
            }
            if Instant::now() >= deadline {
                return Err(TransportError::Internal(
                    "client did not send CONNECTION_CLOSE".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

mod tests {
    use std::future::Future;
    use std::time::Duration;

    use shiguredo_moqt::error::{
        REQUEST_NOT_SUPPORTED, SESSION_INVALID_PATH, SESSION_INVALID_REQUEST_ID,
        SESSION_KEY_VALUE_FORMATTING_ERROR, SESSION_MALFORMED_AUTH_TOKEN,
        SESSION_PROTOCOL_VIOLATION,
    };
    use shiguredo_moqt::message::RequestOk;
    use shiguredo_moqt::message::Subscribe;
    use shiguredo_moqt::message::common::TrackNamespace;
    use shiguredo_moqt::track_properties::{
        PROP_DYNAMIC_GROUPS, TrackProperty, TrackPropertyValue,
    };

    use super::*;
    use crate::moqt_client::{ClientEvent, MoqtClient};

    /// PUBLISH_NAMESPACE の制御メッセージ型 ID (draft-ietf-moq-transport-22 §9 Table 5)
    const PUBLISH_NAMESPACE_TYPE_ID: u64 = 0x6;

    /// 基盤を使うテストの上限時間 (ハングを失敗として観測する)
    const TEST_TIMEOUT: Duration = Duration::from_secs(10);

    /// テスト本体を上限時間で実行する
    ///
    /// 実 relay を必要としないため `#[ignore]` は付けず、`cargo test --workspace` で実行する。
    async fn run_with_timeout(future: impl Future<Output = ()>) {
        tokio::time::timeout(TEST_TIMEOUT, future)
            .await
            .expect("上限時間以内に完了すること");
    }

    /// peer と client を接続し、SETUP 交換まで進める
    ///
    /// client の接続とハンドシェイクは peer のハンドシェイクと並行して進める (どちらも相手の
    /// SETUP を待つため、直列に await すると停止する)。
    async fn connect_pair() -> (TestPeer, MoqtClient) {
        let (peer, client, _acceptor) = connect_pair_with_acceptor().await;
        (peer, client)
    }

    /// peer と client を接続し、SETUP 交換まで進める (client の StreamAcceptor も返す)
    ///
    /// peer が開く uni stream (FETCH 応答など) を client 側で読むテストが使う。
    async fn connect_pair_with_acceptor() -> (TestPeer, MoqtClient, crate::transport::StreamAcceptor)
    {
        // server はこの関数を抜けるまで保持する。s2n-quic の endpoint は Server ハンドルとは
        // 独立に駆動されるが、待ち受けを意図せず閉じないことをコードで示しておく
        let mut server = TestPeerServer::start()
            .await
            .expect("サーバーを起動できること");
        let addr = server.local_addr();

        let connect =
            tokio::spawn(async move { crate::quic::connect(addr, "localhost", None).await });
        // 接続を先に待つ。接続に失敗した場合 accept は完了しないため、この順序でないと
        // 失敗が上限時間のタイムアウトとしてしか観測されない。s2n-quic の endpoint は
        // Server ハンドルとは独立に駆動されるので、accept より先にハンドシェイクが
        // 完了してよい
        let (connection, stop_sending_rx) = connect
            .await
            .expect("接続タスクが終了すること")
            .expect("接続できること");
        let mut peer = server.accept().await.expect("接続を受理できること");

        let task_monitor = tokio_metrics::TaskMonitor::new();
        let establish = tokio::spawn(async move {
            MoqtClient::establish_quic(
                connection,
                stop_sending_rx,
                "/",
                "localhost",
                "test-peer",
                &[],
                &task_monitor,
            )
            .await
        });

        peer.establish().await.expect("SETUP を交換できること");
        let (client, acceptor) = establish
            .await
            .expect("establish タスクが終了すること")
            .expect("client を確立できること");
        assert_eq!(
            peer.session_state(),
            SessionState::Established,
            "peer 側 session が Established になること"
        );
        (peer, client, acceptor)
    }

    /// peer が SETUP を送ってハンドシェイクが完了し、peer 起点 request stream の先頭に
    /// 送ったメッセージを example が受理する
    ///
    /// 実行ファイル (moq-pub / moq-sub) を起動せず、example の I/O 層 (`MoqtClient`) を
    /// in-process の peer と接続して検証する。実 relay は使わない。
    #[tokio::test(flavor = "multi_thread")]
    async fn peer_request_stream_start_message_is_accepted() {
        run_with_timeout(async {
            let (mut peer, mut client) = connect_pair().await;

            // peer 起点の request stream の先頭に SUBSCRIBE を送る
            let request_id = peer.next_request_id().expect("Request ID を採番できること");
            let message = ControlMessage::Subscribe(Subscribe {
                request_id,
                track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("名前空間を作れること"),
                track_name: b"video".to_vec(),
                parameters: MessageParameters::new(),
            });
            peer.send_request_start(message)
                .await
                .expect("request stream を開けること");

            // example が request として受理する
            let event = client.next_event().await.expect("イベントを取得できること");
            match event {
                Some(ClientEvent::Request(request)) => {
                    assert_eq!(request.request_id, request_id, "Request ID が一致すること");
                    match request.message {
                        ControlMessage::Subscribe(subscribe) => {
                            assert_eq!(
                                subscribe.track_namespace,
                                TrackNamespace::new(vec![b"live".to_vec()])
                                    .expect("名前空間を作れること"),
                                "名前空間が一致すること"
                            );
                            assert_eq!(
                                subscribe.track_name,
                                b"video".to_vec(),
                                "トラック名が一致すること"
                            );
                        }
                        other => panic!("SUBSCRIBE として受理されること: {other:?}"),
                    }
                }
                other => panic!("peer 起点の request が受理されること: {other:?}"),
            }
        })
        .await;
    }

    /// client が開いた request stream の先頭メッセージを peer が受理し、応答で要求が完了する
    ///
    /// example から送った PUBLISH が request stream の先頭メッセージとして届き、
    /// `Session::recv_request` へ渡せることを確認する。peer の REQUEST_OK で
    /// `MoqtClient::publish_track` が解決することも併せて確認する。
    #[tokio::test(flavor = "multi_thread")]
    async fn client_request_stream_start_message_is_accepted_by_peer() {
        run_with_timeout(async {
            let (mut peer, client) = connect_pair().await;

            let namespace =
                TrackNamespace::new(vec![b"live".to_vec()]).expect("名前空間を作れること");
            let expected_namespace = namespace.clone();
            // client の PUBLISH 送信と peer の受理は並行して進める
            let publish = tokio::spawn(async move {
                let mut client = client;
                client
                    .publish_track(namespace, b"video".to_vec(), 7, TrackProperties::new())
                    .await
            });

            // peer が request stream の先頭メッセージとして受理する
            let message = peer
                .recv_request_start()
                .await
                .expect("request stream を受理できること");
            let request_id = match message {
                ControlMessage::Publish(publish) => {
                    assert_eq!(
                        publish.track_namespace, expected_namespace,
                        "名前空間が一致すること"
                    );
                    assert_eq!(
                        publish.track_name,
                        b"video".to_vec(),
                        "トラック名が一致すること"
                    );
                    assert_eq!(publish.track_alias, 7, "Track Alias が一致すること");
                    publish.request_id
                }
                other => panic!("PUBLISH として受理されること: {other:?}"),
            };

            // 応答を返すと client の publish_track が解決する
            peer.send_request_ok(request_id)
                .await
                .expect("応答を送れること");
            let published = publish
                .await
                .expect("publish タスクが終了すること")
                .expect("PUBLISH を送れること");
            assert_eq!(
                published, request_id,
                "publish_track が返した Request ID が一致すること"
            );
        })
        .await;
    }

    /// PUBLISH に載せた Track Properties が peer へ届くこと
    ///
    /// moq-pub は映像トラックの PUBLISH で `DYNAMIC_GROUPS=1` を告知し、購読者からの
    /// `NEW_GROUP_REQUEST` (draft-ietf-moq-transport-22 §9.20.19) に応じて新しい Group を
    /// 開始する。告知は PUBLISH の Track Properties (§8.4 / §10.6) で行う。
    #[tokio::test(flavor = "multi_thread")]
    async fn client_publish_track_properties_are_sent_to_peer() {
        run_with_timeout(async {
            let (mut peer, client) = connect_pair().await;

            let namespace =
                TrackNamespace::new(vec![b"live".to_vec()]).expect("名前空間を作れること");
            let mut properties = TrackProperties::new();
            properties.push(TrackProperty {
                prop_type: PROP_DYNAMIC_GROUPS,
                value: TrackPropertyValue::VarInt(1),
            });
            let publish = tokio::spawn(async move {
                let mut client = client;
                client
                    .publish_track(namespace, b"video".to_vec(), 7, properties)
                    .await
            });

            let message = peer
                .recv_request_start()
                .await
                .expect("request stream を受理できること");
            let request_id = match message {
                ControlMessage::Publish(publish) => {
                    assert_eq!(
                        publish.track_properties.dynamic_groups(),
                        Some(1),
                        "PUBLISH に DYNAMIC_GROUPS=1 が載ること"
                    );
                    publish.request_id
                }
                other => panic!("PUBLISH として受理されること: {other:?}"),
            };

            peer.send_request_ok(request_id)
                .await
                .expect("応答を送れること");
            publish
                .await
                .expect("publish タスクが終了すること")
                .expect("PUBLISH を送れること");
        })
        .await;
    }

    /// peer が §6.3 の 7 種類でないメッセージを先頭に送るとセッションが終了する
    ///
    /// 未知の type ID は decode 失敗の経路 (`BidiMessage::PeerRequestError`) を通るため、
    /// decode できる型を送って example の種別判定を確かめる。
    #[tokio::test(flavor = "multi_thread")]
    async fn start_message_outside_allowed_types_closes_session() {
        run_with_timeout(async {
            let (mut peer, mut client) = connect_pair().await;

            // REQUEST_OK は §6.3 の 7 種類に含まれない (request stream の先頭に置けない)
            let message = ControlMessage::RequestOk(RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            });
            peer.send_request_start(message)
                .await
                .expect("request stream を開けること");

            // example は PROTOCOL_VIOLATION でセッションを閉じる
            let event = client.next_event().await.expect("イベントを取得できること");
            match event {
                Some(ClientEvent::Session(SessionEvent::CloseSession(error))) => {
                    assert_eq!(
                        error.code, SESSION_PROTOCOL_VIOLATION,
                        "PROTOCOL_VIOLATION で閉じること"
                    );
                }
                other => panic!("CloseSession が届くこと: {other:?}"),
            }
        })
        .await;
    }

    /// peer が本実装の扱わない request を先頭に送ると拒否され、セッションが維持される
    ///
    /// アプリへ要求が渡らないこと (二重応答を避けること) も確認する。
    #[tokio::test(flavor = "multi_thread")]
    async fn unsupported_request_start_message_is_rejected_and_session_is_kept() {
        run_with_timeout(async {
            let (mut peer, client) = connect_pair().await;

            // PUBLISH_NAMESPACE を先頭に送る。body は 3 種類の共通構造
            // (Request ID → Track Namespace Prefix → Number of Parameters) に従う
            // (draft-ietf-moq-transport-22 §9.14 (PUBLISH_NAMESPACE) /
            // §8.7 (Track Namespace Structure))
            let request_id = peer.next_request_id().expect("Request ID を採番できること");
            let mut body = Vec::new();
            shiguredo_moqt::varint::encode(request_id, &mut body);
            // 空の Track Namespace Prefix (フィールド数 0)
            shiguredo_moqt::varint::encode(0, &mut body);
            // パラメータ数 0
            shiguredo_moqt::varint::encode(0, &mut body);
            let message = ControlMessage::Unsupported {
                type_id: PUBLISH_NAMESPACE_TYPE_ID,
                request_id: Some(request_id),
                body,
            };
            peer.send_request_start(message)
                .await
                .expect("request stream を開けること");

            // client の main ループを回す。peer が開いた stream の先頭メッセージは accept task
            // から main ループへ渡されるため、駆動しないと Session が処理しない。
            // アプリへ要求が渡っていれば、この next_event が直ちに Request を返す
            let drive = tokio::spawn(async move {
                let mut client = client;
                client.next_event().await
            });

            // peer は REQUEST_ERROR (NOT_SUPPORTED) を受け取る
            let response = peer
                .recv_response(request_id)
                .await
                .expect("応答を読めること");
            match response {
                ControlMessage::RequestError(error) => {
                    assert_eq!(
                        error.error_code, REQUEST_NOT_SUPPORTED,
                        "NOT_SUPPORTED で拒否されること"
                    );
                }
                other => panic!("REQUEST_ERROR が届くこと: {other:?}"),
            }

            // アプリへ要求は渡らない (渡っていれば next_event が先に完了している)
            assert!(!drive.is_finished(), "アプリへ request が渡らないこと");

            // 拒否したストリームを終端しても session は維持される
            peer.finish_request_stream(request_id)
                .await
                .expect("request stream を終端できること");

            // 拒否のあとも通常の request が受理されることで session の維持を確かめる
            let next_request_id = peer.next_request_id().expect("Request ID を採番できること");
            let next_message = ControlMessage::Subscribe(Subscribe {
                request_id: next_request_id,
                track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("名前空間を作れること"),
                track_name: b"video".to_vec(),
                parameters: MessageParameters::new(),
            });
            peer.send_request_start(next_message)
                .await
                .expect("request stream を開けること");
            match drive.await.expect("駆動タスクが終了すること") {
                Ok(Some(ClientEvent::Request(request))) => {
                    assert_eq!(
                        request.request_id, next_request_id,
                        "拒否のあとも request が受理されること"
                    );
                }
                other => panic!("拒否のあとも request が受理されること: {other:?}"),
            }
        })
        .await;
    }

    /// peer が parity 違反の request を先頭に送るとセッションが終了する
    ///
    /// 本実装が扱う 4 種類の request でも Session がエラーを返す場合は、終了コード付きで
    /// セッションを閉じることを確かめる (draft-ietf-moq-transport-22 §6.4.2.1 (Request ID))。
    #[tokio::test(flavor = "multi_thread")]
    async fn request_start_message_with_invalid_parity_closes_session() {
        run_with_timeout(async {
            let (mut peer, mut client) = connect_pair().await;

            // server 役の peer が client の parity (偶数) を使うと §6.4.2.1 違反になる
            let message = ControlMessage::Subscribe(Subscribe {
                request_id: 0,
                track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("名前空間を作れること"),
                track_name: b"video".to_vec(),
                parameters: MessageParameters::new(),
            });
            peer.send_request_start(message)
                .await
                .expect("request stream を開けること");

            let event = client.next_event().await.expect("イベントを取得できること");
            match event {
                Some(ClientEvent::Session(SessionEvent::CloseSession(error))) => {
                    assert_eq!(
                        error.code, SESSION_INVALID_REQUEST_ID,
                        "INVALID_REQUEST_ID で閉じること"
                    );
                }
                other => panic!("CloseSession が届くこと: {other:?}"),
            }
        })
        .await;
    }

    /// 生の Setup Options を載せた SETUP を control stream の先頭に書くバイト列
    ///
    /// `ControlMessage::Setup` は `SetupOptions` の型付き API しか受け付けないため、型では
    /// 表現できない不正な Setup Options は `ControlMessage::Unsupported` で書き出す (encode は
    /// type varint + u16 長 + body であり SETUP と同じ書式になる)。body は
    /// draft-ietf-moq-transport-22 §9.1 (SETUP) の「カウントプレフィックスなしのデルタ
    /// エンコードされた KVP」である。
    fn setup_head(body: Vec<u8>) -> Vec<u8> {
        let message = ControlMessage::Unsupported {
            type_id: shiguredo_moqt::stream::SETUP_STREAM_TYPE,
            request_id: None,
            body,
        };
        shiguredo_moqt::stream::encode_control_stream_setup(&message)
            .expect("SETUP のバイト列を組み立てられること")
    }

    /// peer と client を接続し、client の SETUP 交換を開始した状態にする
    ///
    /// [`connect_pair`] と違い peer 側の SETUP 送信はテストが決める。client の establish は
    /// 相手の SETUP を待つため、タスクとして起動して返す。
    async fn connect_handshake() -> (TestPeer, tokio::task::JoinHandle<Result<MoqtClient>>) {
        let mut server = TestPeerServer::start()
            .await
            .expect("サーバーを起動できること");
        let addr = server.local_addr();

        let connect =
            tokio::spawn(async move { crate::quic::connect(addr, "localhost", None).await });
        let (connection, stop_sending_rx) = connect
            .await
            .expect("接続タスクが終了すること")
            .expect("接続できること");
        let peer = server.accept().await.expect("接続を受理できること");

        let task_monitor = tokio_metrics::TaskMonitor::new();
        let establish = tokio::spawn(async move {
            MoqtClient::establish_quic(
                connection,
                stop_sending_rx,
                "/",
                "localhost",
                "test-peer",
                &[],
                &task_monitor,
            )
            .await
            .map(|(client, _acceptor)| client)
        });
        (peer, establish)
    }

    /// 初回 SETUP のトークン構造の decode 失敗で KEY_VALUE_FORMATTING_ERROR を送ること
    ///
    /// draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) は Token 構造を
    /// decode できない場合に KEY_VALUE_FORMATTING_ERROR で閉じることを MUST で要求する。
    /// AUTHORIZATION_TOKEN (0x03) の値に未知の Alias Type (0x04) を置くと
    /// `AuthorizationToken::decode` が `KeyValueFormattingError` を返す。
    #[tokio::test(flavor = "multi_thread")]
    async fn initial_setup_with_unknown_token_alias_closes_with_key_value_formatting_error() {
        run_with_timeout(async {
            let (mut peer, establish) = connect_handshake().await;

            // Setup Options: option type 0x03 (delta 3)、値長 1、値 = 未知の Alias Type (0x04)
            peer.send_control_stream_head(setup_head(vec![0x03, 0x01, 0x04]))
                .await
                .expect("control stream を書けること");

            let error = establish
                .await
                .expect("establish タスクが終了すること")
                .err()
                .expect("SETUP の decode 失敗で確立が失敗すること");
            assert!(
                matches!(error, TransportError::Moqt(_)),
                "decode 失敗として返ること: {error}"
            );
            assert_eq!(
                peer.recv_close_code()
                    .await
                    .expect("終了コードを観測できること"),
                SESSION_KEY_VALUE_FORMATTING_ERROR,
                "書式違反は KEY_VALUE_FORMATTING_ERROR (0x6) であること"
            );
        })
        .await;
    }

    /// 初回 SETUP のトークン重複で MALFORMED_AUTH_TOKEN を送ること
    ///
    /// draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) は Authorization Token の
    /// 繰り返しを "(Token Type, Token Value) が alias 解決後に一意である限り" 許し、整形式だが
    /// 不正な Token を "MUST reject that message with an MALFORMED_AUTH_TOKEN error" とする。
    /// SETUP は Request ID を持たずメッセージ単位の reject ができないため、§12.2
    /// (Session Termination Codes) の MALFORMED_AUTH_TOKEN (0x16) でセッションを閉じる。
    /// `SetupOptions::encode` は重複を拒否するため、生の Setup Options で 2 つ目を書く。
    #[tokio::test(flavor = "multi_thread")]
    async fn initial_setup_with_duplicate_tokens_closes_with_malformed_auth_token() {
        run_with_timeout(async {
            let (mut peer, establish) = connect_handshake().await;

            // REGISTER (0x01) の AUTHORIZATION_TOKEN を 2 つ書き、(Token Type, Token Value) を
            // 重複させる (alias だけが異なる)。§12.2 の MALFORMED_AUTH_TOKEN は
            // "registration 中の不正な直列化" を指すため、REGISTER で組む
            let token_a: Vec<u8> = [&[0x01, 0x01, 0x00][..], b"test"].concat();
            let token_b: Vec<u8> = [&[0x01, 0x02, 0x00][..], b"test"].concat();
            let mut body = vec![0x03, token_a.len() as u8];
            body.extend_from_slice(&token_a);
            // 2 つ目は option type のデルタ 0
            body.push(0x00);
            body.push(token_b.len() as u8);
            body.extend_from_slice(&token_b);
            peer.send_control_stream_head(setup_head(body))
                .await
                .expect("control stream を書けること");

            let error = establish
                .await
                .expect("establish タスクが終了すること")
                .err()
                .expect("SETUP の decode 失敗で確立が失敗すること");
            assert!(
                matches!(
                    error,
                    TransportError::Moqt(shiguredo_moqt::error::MessageError::MalformedAuthToken(
                        _
                    ))
                ),
                "トークン重複として返ること: {error}"
            );
            assert_eq!(
                peer.recv_close_code()
                    .await
                    .expect("終了コードを観測できること"),
                SESSION_MALFORMED_AUTH_TOKEN,
                "トークン重複は MALFORMED_AUTH_TOKEN (0x16) であること"
            );
        })
        .await;
    }

    /// server 役の SETUP が PATH を持つと INVALID_PATH を送ること
    ///
    /// draft-ietf-moq-transport-22 §9.1.2 (PATH) は server が PATH を送ることを MUST NOT と
    /// する。session 層の検証失敗であるため、`Session` が積んだ終了コードを送る。
    #[tokio::test(flavor = "multi_thread")]
    async fn initial_setup_with_path_from_server_closes_with_invalid_path() {
        run_with_timeout(async {
            let (mut peer, establish) = connect_handshake().await;

            let mut options = SetupOptions::new();
            options.push(SetupOption {
                option_type: shiguredo_moqt::parameter::SETUP_OPTION_PATH,
                value: SetupOptionValue::Bytes(b"/evil".to_vec()),
            });
            peer.send_setup(&options).await.expect("SETUP を送れること");

            let error = establish
                .await
                .expect("establish タスクが終了すること")
                .err()
                .expect("session 層の検証失敗で確立が失敗すること");
            assert!(
                matches!(error, TransportError::Internal(_)),
                "session 層の失敗として返ること: {error}"
            );
            assert_eq!(
                peer.recv_close_code()
                    .await
                    .expect("終了コードを観測できること"),
                SESSION_INVALID_PATH,
                "server の PATH は INVALID_PATH (0x8) であること"
            );
        })
        .await;
    }

    /// control stream の stream type が不一致だと PROTOCOL_VIOLATION を送ること
    ///
    /// draft-ietf-moq-transport-22 §6.4.1 (Unidirectional Streams) は control stream の先頭を
    /// `SETUP_STREAM_TYPE` (0x2F00) に限り、未知の stream type の受信でセッションを閉じる。
    /// 終了コードは §12.2 (Session Termination Codes) の PROTOCOL_VIOLATION (0x3) である。
    #[tokio::test(flavor = "multi_thread")]
    async fn control_stream_with_wrong_stream_type_closes_with_protocol_violation() {
        run_with_timeout(async {
            let (mut peer, establish) = connect_handshake().await;

            // 誤った stream type prefix (control stream は 0x2F00)
            let mut head = Vec::new();
            shiguredo_moqt::varint::encode(0x2F01, &mut head);
            peer.send_control_stream_head(head)
                .await
                .expect("control stream を書けること");

            establish
                .await
                .expect("establish タスクが終了すること")
                .err()
                .expect("stream type の不一致で確立が失敗すること");
            assert_eq!(
                peer.recv_close_code()
                    .await
                    .expect("終了コードを観測できること"),
                SESSION_PROTOCOL_VIOLATION,
                "stream type の不一致は PROTOCOL_VIOLATION (0x3) であること"
            );
        })
        .await;
    }

    /// SETUP の前に control stream が終端すると PROTOCOL_VIOLATION を送ること
    ///
    /// draft-ietf-moq-transport-22 §6.3 (Session initialization) は control stream を
    /// セッション中に閉じることを禁じる。
    #[tokio::test(flavor = "multi_thread")]
    async fn control_stream_finished_before_setup_closes_with_protocol_violation() {
        run_with_timeout(async {
            let (mut peer, establish) = connect_handshake().await;

            // 何も書かずに control stream を終端する (client は stream type を読めない)
            peer.send_control_stream_head(Vec::new())
                .await
                .expect("control stream を開けること");
            peer.finish_control_stream()
                .await
                .expect("control stream を終端できること");

            establish
                .await
                .expect("establish タスクが終了すること")
                .err()
                .expect("SETUP 前の終端で確立が失敗すること");
            assert_eq!(
                peer.recv_close_code()
                    .await
                    .expect("終了コードを観測できること"),
                SESSION_PROTOCOL_VIOLATION,
                "SETUP 前の終端は PROTOCOL_VIOLATION (0x3) であること"
            );
        })
        .await;
    }

    /// 初回 SETUP の decode 失敗で写像が PROTOCOL_VIOLATION を選ぶこと
    ///
    /// draft-ietf-moq-transport-22 §9.1.4 (AUTHORIZATION TOKEN) は SETUP の Token に DELETE /
    /// USE_ALIAS を許さず、受信側は PROTOCOL_VIOLATION でセッションを閉じる (原文の MUST は
    /// server が受信する場合について述べているが、library は役割によらず同じ検証をする)。
    /// SETUP の decode 失敗なので写像 (`session_error_code`) の 3 つ目の分岐を通り、
    /// §12.2 (Session Termination Codes) の PROTOCOL_VIOLATION (0x3) で閉じる。
    #[tokio::test(flavor = "multi_thread")]
    async fn initial_setup_with_delete_token_closes_with_protocol_violation() {
        run_with_timeout(async {
            let (mut peer, establish) = connect_handshake().await;

            // Setup Options: option type 0x03 (delta 3)、値長 2、値 = DELETE (0x00) + alias 1
            peer.send_control_stream_head(setup_head(vec![0x03, 0x02, 0x00, 0x01]))
                .await
                .expect("control stream を書けること");

            let error = establish
                .await
                .expect("establish タスクが終了すること")
                .err()
                .expect("SETUP の decode 失敗で確立が失敗すること");
            assert!(
                matches!(error, TransportError::Moqt(_)),
                "decode 失敗として返ること: {error}"
            );
            assert_eq!(
                peer.recv_close_code()
                    .await
                    .expect("終了コードを観測できること"),
                SESSION_PROTOCOL_VIOLATION,
                "DELETE トークンは PROTOCOL_VIOLATION (0x3) であること"
            );
        })
        .await;
    }

    /// stream type の直後に control stream が終端すると PROTOCOL_VIOLATION を送ること
    ///
    /// stream type は正当だが SETUP を送らずに終端する場合であり、`finish_handshake` の
    /// 「control stream closed before SETUP」経路を通る (stream type を読む前に終端する
    /// [`control_stream_finished_before_setup_closes_with_protocol_violation`] とは別の経路)。
    /// draft-ietf-moq-transport-22 §6.3 (Session initialization) は control stream を
    /// セッション中に閉じることを禁じ、§12.2 (Session Termination Codes) の
    /// PROTOCOL_VIOLATION (0x3) で閉じる。
    #[tokio::test(flavor = "multi_thread")]
    async fn control_stream_finished_after_stream_type_closes_with_protocol_violation() {
        run_with_timeout(async {
            let (mut peer, establish) = connect_handshake().await;

            // 正当な stream type prefix だけを書いて SETUP を送らずに終端する
            let mut head = Vec::new();
            shiguredo_moqt::varint::encode(shiguredo_moqt::stream::SETUP_STREAM_TYPE, &mut head);
            peer.send_control_stream_head(head)
                .await
                .expect("control stream を書けること");
            peer.finish_control_stream()
                .await
                .expect("control stream を終端できること");

            let error = establish
                .await
                .expect("establish タスクが終了すること")
                .err()
                .expect("SETUP 前の終端で確立が失敗すること");
            assert!(
                error.to_string().contains("closed before SETUP"),
                "SETUP 前の終端として返ること: {error}"
            );
            assert_eq!(
                peer.recv_close_code()
                    .await
                    .expect("終了コードを観測できること"),
                SESSION_PROTOCOL_VIOLATION,
                "SETUP 前の終端は PROTOCOL_VIOLATION (0x3) であること"
            );
        })
        .await;
    }
    /// 共通の接続経路で確立した session で PUBLISH と SUBSCRIBE が成立すること
    ///
    /// GOAWAY の移行先もこの経路 ([`crate::connect`]) で接続するため、経路そのものが
    /// 動くことを固定する。PUBLISH は PUBLISH_OK、SUBSCRIBE は track alias を運ぶ
    /// SUBSCRIBE_OK で解決する。
    #[tokio::test(flavor = "multi_thread")]
    async fn shared_connect_path_establishes_publish_and_subscribe() {
        run_with_timeout(async {
            let mut server = TestPeerServer::start()
                .await
                .expect("サーバーを起動できること");
            let addr = server.local_addr();
            let url = crate::parse_url(&format!("moqt://{addr}/")).expect("URL をパースできること");

            let task_monitor = tokio_metrics::TaskMonitor::new();
            let connect_url = url.clone();
            let connect_monitor = task_monitor.clone();
            let connect = tokio::spawn(async move {
                crate::connect(
                    &crate::ConnectParams {
                        url: &connect_url,
                        transport: crate::Transport::Quic,
                        cert: None,
                        impl_name: "test-peer",
                        receive_datagrams: false,
                        // QMux 経路は使わない (QUIC 直接接続の試験基盤)
                        datagram_max_size: None,
                    },
                    &connect_monitor,
                )
                .await
            });

            let mut peer = server.accept().await.expect("接続を受理できること");
            peer.establish().await.expect("SETUP を交換できること");
            let (client, _acceptor) = connect
                .await
                .expect("接続タスクが終了すること")
                .expect("共通の接続経路で接続できること");

            let namespace =
                TrackNamespace::new(vec![b"live".to_vec()]).expect("名前空間を作れること");
            let subscribe_namespace = namespace.clone();
            // PUBLISH と SUBSCRIBE は peer の応答と並行して進める
            let client_task = tokio::spawn(async move {
                let mut client = client;
                let publish_request_id = client
                    .publish_track(namespace, b"video".to_vec(), 7, TrackProperties::new())
                    .await?;
                let subscribe = client
                    .subscribe_track(subscribe_namespace, b"video".to_vec())
                    .await?;
                Ok::<_, TransportError>((publish_request_id, subscribe))
            });

            let publish = peer
                .recv_request_start()
                .await
                .expect("PUBLISH を受理できること");
            let publish_request_id = match publish {
                ControlMessage::Publish(publish) => publish.request_id,
                other => panic!("PUBLISH として受理されること: {other:?}"),
            };
            peer.send_request_ok(publish_request_id)
                .await
                .expect("PUBLISH_OK を返せること");

            let subscribe = peer
                .recv_request_start()
                .await
                .expect("SUBSCRIBE を受理できること");
            let subscribe_request_id = match subscribe {
                ControlMessage::Subscribe(subscribe) => subscribe.request_id,
                other => panic!("SUBSCRIBE として受理されること: {other:?}"),
            };
            // peer が決める track alias で SUBSCRIBE_OK を返す
            peer.send_subscribe_ok(subscribe_request_id, 11)
                .await
                .expect("SUBSCRIBE_OK を返せること");

            let (published, subscribed) = client_task
                .await
                .expect("client タスクが終了すること")
                .expect("PUBLISH と SUBSCRIBE が成立すること");
            assert_eq!(
                published, publish_request_id,
                "PUBLISH の Request ID が一致すること"
            );
            assert_eq!(
                subscribed.request_id, subscribe_request_id,
                "SUBSCRIBE の Request ID が一致すること"
            );
            assert_eq!(subscribed.track_alias, 11, "track alias が一致すること");
        })
        .await;
    }
    /// 移行先の New Session URI から決めた URL へ共通の接続経路で接続し、PUBLISH が成立すること
    ///
    /// GOAWAY の New Session URI を [`crate::migration_target`] で解釈した結果を、そのまま
    /// [`crate::connect`] に渡せることを固定する。実際の移行では example がこの 2 つを順に呼ぶ。
    #[tokio::test(flavor = "multi_thread")]
    async fn migration_target_url_is_connectable() {
        run_with_timeout(async {
            let mut server = TestPeerServer::start()
                .await
                .expect("サーバーを起動できること");
            let addr = server.local_addr();

            // GOAWAY が運ぶ New Session URI と同じ形の URI から移行先を決める
            let current = crate::parse_url("moqt://relay.example:443/live")
                .expect("現在の URL をパースできること");
            let uri = format!("moqt://{addr}/live");
            let target = crate::migration_target(uri.as_bytes(), None, &current)
                .expect("判断に失敗しないこと")
                .expect("移行先があること");

            let task_monitor = tokio_metrics::TaskMonitor::new();
            let connect_monitor = task_monitor.clone();
            let connect = tokio::spawn(async move {
                crate::connect(
                    &crate::ConnectParams {
                        url: &target,
                        transport: crate::Transport::Quic,
                        cert: None,
                        impl_name: "test-peer",
                        receive_datagrams: false,
                        // QMux 経路は使わない (QUIC 直接接続の試験基盤)
                        datagram_max_size: None,
                    },
                    &connect_monitor,
                )
                .await
            });

            let mut peer = server.accept().await.expect("接続を受理できること");
            peer.establish().await.expect("SETUP を交換できること");
            let (mut client, _acceptor) = connect
                .await
                .expect("接続タスクが終了すること")
                .expect("移行先へ接続できること");

            let namespace =
                TrackNamespace::new(vec![b"live".to_vec()]).expect("名前空間を作れること");
            let publish_task = tokio::spawn(async move {
                client
                    .publish_track(namespace, b"video".to_vec(), 3, TrackProperties::new())
                    .await
            });
            let publish = peer
                .recv_request_start()
                .await
                .expect("PUBLISH を受理できること");
            let publish_request_id = match publish {
                ControlMessage::Publish(publish) => publish.request_id,
                other => panic!("PUBLISH として受理されること: {other:?}"),
            };
            peer.send_request_ok(publish_request_id)
                .await
                .expect("PUBLISH_OK を返せること");
            assert_eq!(
                publish_task
                    .await
                    .expect("タスクが終了すること")
                    .expect("PUBLISH が成立すること"),
                publish_request_id,
                "移行先の session で PUBLISH が成立すること"
            );
        })
        .await;
    }
    /// GOAWAY の New Session URI へ移行する流れを、実 relay なしで固定する
    ///
    /// example が実装する手順 (GOAWAY 受信 → [`crate::migration_target`] で移行先を決定 →
    /// [`crate::connect`] で新 session を確立 → 購読を張り直す → 旧 session を `close(0, "")`)
    /// を、同じ API を同じ順に呼んで通す。旧 session が NO_ERROR で閉じられることも確認する。
    #[tokio::test(flavor = "multi_thread")]
    async fn goaway_migration_switches_to_the_new_session() {
        run_with_timeout(async {
            let task_monitor = tokio_metrics::TaskMonitor::new();

            // 移行元 (server A) へ接続する
            let mut server_a = TestPeerServer::start()
                .await
                .expect("サーバーを起動できること");
            let addr_a = server_a.local_addr();
            let url_a = crate::parse_url(&format!("moqt://{addr_a}/"))
                .expect("移行元の URL をパースできること");
            let connect_url = url_a.clone();
            let connect_monitor = task_monitor.clone();
            let connect_a = tokio::spawn(async move {
                crate::connect(
                    &crate::ConnectParams {
                        url: &connect_url,
                        transport: crate::Transport::Quic,
                        cert: None,
                        impl_name: "test-peer",
                        receive_datagrams: false,
                        // QMux 経路は使わない (QUIC 直接接続の試験基盤)
                        datagram_max_size: None,
                    },
                    &connect_monitor,
                )
                .await
            });
            let mut peer_a = server_a.accept().await.expect("接続を受理できること");
            peer_a.establish().await.expect("SETUP を交換できること");
            let (mut client, _acceptor) = connect_a
                .await
                .expect("接続タスクが終了すること")
                .expect("移行元へ接続できること");

            // 移行元で購読を確立する
            let namespace =
                TrackNamespace::new(vec![b"live".to_vec()]).expect("名前空間を作れること");
            // 購読の送信と peer の応答は並行して進める (client は移行の判定でも使うため
            // タスクへ move せず、join で同時に進める)
            let subscribe_namespace = namespace.clone();
            let (subscribed, _request_id) = tokio::join!(
                client.subscribe_track(subscribe_namespace, b"video".to_vec()),
                async {
                    let message = peer_a
                        .recv_request_start()
                        .await
                        .expect("SUBSCRIBE を受理できること");
                    let request_id = match message {
                        ControlMessage::Subscribe(subscribe) => subscribe.request_id,
                        other => panic!("SUBSCRIBE として受理されること: {other:?}"),
                    };
                    peer_a
                        .send_subscribe_ok(request_id, 5)
                        .await
                        .expect("SUBSCRIBE_OK を返せること");
                    request_id
                }
            );
            let subscribed = subscribed.expect("移行元で購読が成立すること");
            assert_eq!(subscribed.track_alias, 5, "移行元の alias が一致すること");

            // 移行先 (server B) の URI を載せた GOAWAY を server A から送る
            let mut server_b = TestPeerServer::start()
                .await
                .expect("移行先のサーバーを起動できること");
            let addr_b = server_b.local_addr();
            let migration_uri = format!("moqt://{addr_b}/");
            peer_a
                .send_goaway(migration_uri.clone().into_bytes(), 0)
                .await
                .expect("GOAWAY を送れること");

            // client が GOAWAY を受信し、移行先を決める
            let event = client.next_event().await.expect("イベントを取得できること");
            let (new_session_uri, on_request_stream) = match event {
                Some(ClientEvent::Session(SessionEvent::GoawayReceived {
                    new_session_uri,
                    on_request_stream,
                    ..
                })) => (new_session_uri, on_request_stream),
                other => panic!("GOAWAY として受理されること: {other:?}"),
            };
            assert!(
                on_request_stream.is_none(),
                "control stream の GOAWAY であること"
            );
            let target = crate::migration_target(&new_session_uri, on_request_stream, &url_a)
                .expect("移行先を決められること")
                .expect("移行先があること");
            assert_eq!(
                target.authority,
                format!("127.0.0.1:{}", addr_b.port()),
                "GOAWAY の URI が移行先になること"
            );

            // 移行先へ接続して購読を張り直す
            let target_monitor = task_monitor.clone();
            let connect_b = tokio::spawn(async move {
                crate::connect(
                    &crate::ConnectParams {
                        url: &target,
                        transport: crate::Transport::Quic,
                        cert: None,
                        impl_name: "test-peer",
                        receive_datagrams: false,
                        // QMux 経路は使わない (QUIC 直接接続の試験基盤)
                        datagram_max_size: None,
                    },
                    &target_monitor,
                )
                .await
            });
            let mut peer_b = server_b.accept().await.expect("移行先で受理できること");
            peer_b
                .establish()
                .await
                .expect("移行先で SETUP を交換できること");
            let (mut migrated, _acceptor) = connect_b
                .await
                .expect("接続タスクが終了すること")
                .expect("移行先へ接続できること");
            let resubscribe = tokio::spawn(async move {
                migrated.subscribe_track(namespace, b"video".to_vec()).await
            });
            let message = peer_b
                .recv_request_start()
                .await
                .expect("移行先で SUBSCRIBE を受理できること");
            let request_id = match message {
                ControlMessage::Subscribe(subscribe) => subscribe.request_id,
                other => panic!("SUBSCRIBE として受理されること: {other:?}"),
            };
            peer_b
                .send_subscribe_ok(request_id, 9)
                .await
                .expect("移行先で SUBSCRIBE_OK を返せること");
            let resubscribed = resubscribe
                .await
                .expect("タスクが終了すること")
                .expect("移行先で購読が成立すること");
            assert_eq!(resubscribed.track_alias, 9, "移行先の alias が一致すること");

            // 旧 session は移行対象を終了させてから NO_ERROR で閉じる
            client.set_control_message_timeout_ms(Some(30_000));
            client
                .stop_sending(subscribed.request_id)
                .await
                .expect("旧 session の購読を終了できること");
            client
                .close(0, "")
                .await
                .expect("旧 session を閉じられること");
            assert_eq!(
                peer_a
                    .recv_close_code()
                    .await
                    .expect("終了コードを観測できること"),
                0,
                "旧 session は NO_ERROR (0x0) で閉じられること"
            );
        })
        .await;
    }
    /// peer から届いた FETCH に、複数 Object と End of Unknown Range を 1 本の応答で返す
    ///
    /// publisher 側の応答経路 (`send_fetch_ok` → `open_fetch_response` → writer) を固定する。
    /// 保持した複数の Object と End of Range を 1 本の uni stream に並べて書けること、
    /// peer 側の [`FetchStreamDecoder`] が同じ内容を読めることを確認する。
    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_response_writer_writes_objects_and_end_of_range() {
        use shiguredo_moqt::message::Fetch;
        use shiguredo_moqt::message::common::Location;
        use shiguredo_moqt::message_parameter::MessageParameters;
        use shiguredo_moqt::stream::decoder::{DecodedFetchEntry, FetchStreamDecoder};
        use shiguredo_moqt::stream::encoder::FetchObjectInput;

        run_with_timeout(async {
            let (mut peer, mut client) = connect_pair().await;

            // peer (relay) が publisher へ FETCH を送る
            let request_id = peer.next_request_id().expect("Request ID を採番できること");
            let namespace =
                TrackNamespace::new(vec![b"live".to_vec()]).expect("名前空間を作れること");
            peer.send_request_start(ControlMessage::Fetch(Fetch {
                request_id,
                track_namespace: namespace,
                track_name: b"video".to_vec(),
                parameters: MessageParameters::new(),
            }))
            .await
            .expect("FETCH を送れること");

            // client が request として受理する
            let event = client.next_event().await.expect("イベントを取得できること");
            let delivered_request_id = match event {
                Some(ClientEvent::Request(request)) => {
                    assert!(
                        matches!(request.message, ControlMessage::Fetch(_)),
                        "FETCH として受理されること"
                    );
                    request.request_id
                }
                other => panic!("FETCH がアプリへ届くこと: {other:?}"),
            };
            assert_eq!(
                delivered_request_id, request_id,
                "Request ID が一致すること"
            );

            // FETCH_OK を返し、保持している Object と End of Unknown Range を 1 本に書く
            client
                .send_fetch_ok(
                    request_id,
                    0,
                    Location {
                        group_id: 3,
                        object_id: 1,
                    },
                    MessageParameters::new(),
                    TrackProperties::new(),
                )
                .await
                .expect("FETCH_OK を返せること");
            let mut writer = client
                .open_fetch_response(request_id)
                .await
                .expect("FETCH 応答 stream を開けること");
            // Properties Length = 0 を含む生バイト列 (draft-ietf-moq-transport-22 §11.1.2)
            let properties: Vec<u8> = vec![0x00];
            writer
                .write_object(
                    &FetchObjectInput {
                        group_id: 1,
                        subgroup_id: 0,
                        object_id: 0,
                        publisher_priority: 128,
                        has_properties: true,
                        is_datagram_origin: false,
                        payload_length: 4,
                    },
                    b"abcd",
                    Some(&properties),
                )
                .await
                .expect("1 件目の Object を書けること");
            // 保持していない範囲を End of Unknown Range で示す
            writer
                .write_end_of_unknown_range(Location {
                    group_id: 2,
                    object_id: 5,
                })
                .await
                .expect("End of Unknown Range を書けること");
            writer
                .write_object(
                    &FetchObjectInput {
                        group_id: 3,
                        subgroup_id: 0,
                        object_id: 1,
                        publisher_priority: 128,
                        has_properties: true,
                        is_datagram_origin: false,
                        payload_length: 2,
                    },
                    b"xy",
                    Some(&properties),
                )
                .await
                .expect("2 件目の Object を書けること");
            writer.finish().await.expect("応答を FIN で閉じられること");

            // peer が応答 stream を読み、FETCH_HEADER と entry を decode する
            let bytes = peer
                .recv_uni_data_stream()
                .await
                .expect("FETCH 応答 stream を読めること");
            let mut decoder = FetchStreamDecoder::new();
            decoder.push(&bytes);
            let header = decoder
                .try_decode_header()
                .expect("FETCH_HEADER を decode できること")
                .expect("FETCH_HEADER があること");
            assert_eq!(header.request_id, request_id, "FETCH_HEADER の Request ID");

            let mut decoded = Vec::new();
            while let Some(entry) = decoder
                .try_decode_entry()
                .expect("entry を decode できること")
            {
                let payload = match &entry {
                    DecodedFetchEntry::Object(object) if object.payload_length > 0 => {
                        decoder.try_read_payload().expect("payload を読めること")
                    }
                    _ => Vec::new(),
                };
                decoded.push((entry, payload));
            }
            assert_eq!(decoded.len(), 3, "Object / End of Range / Object の 3 件");
            match (&decoded[0].0, &decoded[0].1) {
                (DecodedFetchEntry::Object(object), payload) => {
                    assert_eq!(object.group_id, 1);
                    assert_eq!(object.object_id, 0);
                    assert_eq!(payload.as_slice(), b"abcd", "1 件目の payload");
                }
                other => panic!("1 件目が Object であること: {other:?}"),
            }
            match &decoded[1].0 {
                DecodedFetchEntry::EndOfUnknownRange {
                    group_id,
                    object_id,
                } => {
                    assert_eq!(*group_id, 2);
                    assert_eq!(*object_id, 5);
                }
                other => panic!("2 件目が End of Unknown Range であること: {other:?}"),
            }
            match (&decoded[2].0, &decoded[2].1) {
                (DecodedFetchEntry::Object(object), payload) => {
                    assert_eq!(object.group_id, 3);
                    assert_eq!(object.object_id, 1);
                    assert_eq!(payload.as_slice(), b"xy", "2 件目の payload");
                }
                other => panic!("3 件目が Object であること: {other:?}"),
            }
        })
        .await;
    }

    /// client が FETCH を発行し、FETCH_OK と応答 stream の Object / End of Range を受け取る
    ///
    /// subscriber 側の取得経路を固定する。`MoqtClient::fetch` → `FetchOkReceived` の配送 →
    /// peer が開いた uni stream の `FetchStreamDecoder` での decode までを通す。
    #[tokio::test(flavor = "multi_thread")]
    async fn client_fetch_receives_fetch_ok_and_response_entries() {
        use shiguredo_moqt::message_parameter::MessageParameters;
        use shiguredo_moqt::stream::decoder::{DecodedFetchEntry, FetchStreamDecoder};
        use shiguredo_moqt::stream::encoder::{FetchObjectInput, FetchStreamEncoder};

        run_with_timeout(async {
            let (mut peer, mut client, mut acceptor) = connect_pair_with_acceptor().await;

            // client が FETCH を発行する
            let namespace =
                TrackNamespace::new(vec![b"live".to_vec()]).expect("名前空間を作れること");
            let request_id = client
                .fetch(namespace, b"video".to_vec(), MessageParameters::new())
                .await
                .expect("FETCH を発行できること");

            // peer が受理し、FETCH_OK を返す
            let message = peer
                .recv_request_start()
                .await
                .expect("FETCH を受理できること");
            let received_request_id = match message {
                ControlMessage::Fetch(fetch) => fetch.request_id,
                other => panic!("FETCH として受理されること: {other:?}"),
            };
            assert_eq!(received_request_id, request_id, "Request ID が一致すること");
            let end_location = Location {
                group_id: 3,
                object_id: 1,
            };
            peer.send_fetch_ok(request_id, end_location, 0)
                .await
                .expect("FETCH_OK を返せること");

            // peer が応答 stream (uni) に Object と End of Unknown Range を書く
            let mut encoder = FetchStreamEncoder::new(request_id);
            let mut bytes = encoder.encode_header();
            let properties: Vec<u8> = vec![0x00];
            encoder
                .encode_object(
                    &FetchObjectInput {
                        group_id: 1,
                        subgroup_id: 0,
                        object_id: 0,
                        publisher_priority: 128,
                        has_properties: true,
                        is_datagram_origin: false,
                        payload_length: 4,
                    },
                    Some(&properties),
                    &mut bytes,
                )
                .expect("1 件目の Object を encode できること");
            bytes.extend_from_slice(b"abcd");
            encoder
                .encode_end_of_unknown_range(2, 5, &mut bytes)
                .expect("End of Unknown Range を encode できること");
            encoder
                .encode_object(
                    &FetchObjectInput {
                        group_id: 3,
                        subgroup_id: 0,
                        object_id: 1,
                        publisher_priority: 128,
                        has_properties: true,
                        is_datagram_origin: false,
                        payload_length: 2,
                    },
                    Some(&properties),
                    &mut bytes,
                )
                .expect("2 件目の Object を encode できること");
            bytes.extend_from_slice(b"xy");
            let mut stream = peer
                .open_uni_send_stream()
                .await
                .expect("uni stream を開けること");
            stream
                .send(Bytes::from(bytes))
                .await
                .expect("応答 stream を書けること");
            stream.finish().await.expect("応答 stream を終端できること");

            // client が FETCH_OK を受信する
            let event = client.next_event().await.expect("イベントを取得できること");
            match event {
                Some(ClientEvent::Session(SessionEvent::FetchOkReceived {
                    request_id: rid,
                    end_location: location,
                    end_of_track,
                })) => {
                    assert_eq!(rid, request_id, "FETCH_OK の Request ID");
                    assert_eq!(location, end_location, "FETCH_OK の End Location");
                    assert!(!end_of_track, "FETCH_OK の End of Track は未達であること");
                }
                other => panic!("FETCH_OK がアプリへ届くこと: {other:?}"),
            }

            // client が応答 stream を読んで decode する
            let mut stream = acceptor
                .accept_recv_stream()
                .await
                .expect("応答 stream を受理できること")
                .expect("応答 stream があること");
            let mut buf = Vec::new();
            while let crate::transport::RecvChunk::Data(data) = stream
                .receive_chunk()
                .await
                .expect("応答 stream を読めること")
            {
                buf.extend_from_slice(&data);
            }
            let mut decoder = FetchStreamDecoder::new();
            decoder.push(&buf);
            let header = decoder
                .try_decode_header()
                .expect("FETCH_HEADER を decode できること")
                .expect("FETCH_HEADER があること");
            assert_eq!(header.request_id, request_id, "FETCH_HEADER の Request ID");
            let mut decoded = Vec::new();
            while let Some(entry) = decoder
                .try_decode_entry()
                .expect("entry を decode できること")
            {
                let payload = match &entry {
                    DecodedFetchEntry::Object(object) if object.payload_length > 0 => {
                        decoder.try_read_payload().expect("payload を読めること")
                    }
                    _ => Vec::new(),
                };
                decoded.push((entry, payload));
            }
            assert_eq!(decoded.len(), 3, "Object / End of Range / Object の 3 件");
            match (&decoded[0].0, &decoded[0].1) {
                (DecodedFetchEntry::Object(object), payload) => {
                    assert_eq!(object.group_id, 1);
                    assert_eq!(object.object_id, 0);
                    assert_eq!(payload.as_slice(), b"abcd", "1 件目の payload");
                }
                other => panic!("1 件目が Object であること: {other:?}"),
            }
            match &decoded[1].0 {
                DecodedFetchEntry::EndOfUnknownRange {
                    group_id,
                    object_id,
                } => {
                    assert_eq!(*group_id, 2);
                    assert_eq!(*object_id, 5);
                }
                other => panic!("2 件目が End of Unknown Range であること: {other:?}"),
            }
            match (&decoded[2].0, &decoded[2].1) {
                (DecodedFetchEntry::Object(object), payload) => {
                    assert_eq!(object.group_id, 3);
                    assert_eq!(object.object_id, 1);
                    assert_eq!(payload.as_slice(), b"xy", "2 件目の payload");
                }
                other => panic!("3 件目が Object であること: {other:?}"),
            }
        })
        .await;
    }

    /// Session が INVALID_RANGE で拒否した FETCH はアプリへ渡らない
    ///
    /// draft-ietf-moq-transport-22 §3.2 (Fetch) の MUST 2 条件 (Object が publish されていない /
    /// Start Location が Largest Object より大きい) は Session が判定して REQUEST_ERROR を送る。
    /// アプリが重ねて応答すると `send_fetch_ok` が未登録の Request ID を拒否して致命終了するため、
    /// 拒否済みの FETCH を配送しないことを固定する。
    #[tokio::test(flavor = "multi_thread")]
    async fn rejected_fetch_is_not_delivered_to_the_application() {
        use shiguredo_moqt::error::REQUEST_INVALID_RANGE;
        use shiguredo_moqt::message::Fetch;
        use shiguredo_moqt::message_parameter::MessageParameters;

        run_with_timeout(async {
            let (mut peer, mut client) = connect_pair().await;
            let namespace =
                TrackNamespace::new(vec![b"live".to_vec()]).expect("名前空間を作れること");

            // まだ Object を publish していない track を PUBLISH する
            // (publish の送信と peer の応答は並行して進める)
            let publish_namespace = namespace.clone();
            let (published, _) = tokio::join!(
                client.publish_track(
                    publish_namespace,
                    b"video".to_vec(),
                    7,
                    TrackProperties::new()
                ),
                async {
                    let message = peer
                        .recv_request_start()
                        .await
                        .expect("PUBLISH を受理できること");
                    let request_id = match message {
                        ControlMessage::Publish(publish) => publish.request_id,
                        other => panic!("PUBLISH として受理されること: {other:?}"),
                    };
                    peer.send_request_ok(request_id)
                        .await
                        .expect("PUBLISH_OK を返せること");
                    request_id
                }
            );
            published.expect("PUBLISH が成立すること");

            // peer が、まだ Object が無い track へ FETCH を送る
            let fetch_request_id = peer.next_request_id().expect("Request ID を採番できること");
            peer.send_request_start(ControlMessage::Fetch(Fetch {
                request_id: fetch_request_id,
                track_namespace: namespace,
                track_name: b"video".to_vec(),
                parameters: MessageParameters::new(),
            }))
            .await
            .expect("FETCH を送れること");

            // peer 起点 request の受理と自動応答はアプリのイベントループ
            // (`MoqtClient::next_event`) から駆動されるため、待つ間も回しておく
            let drive = tokio::spawn(async move { client.next_event().await });
            // peer は INVALID_RANGE の REQUEST_ERROR を受け取る
            let response = peer
                .recv_response(fetch_request_id)
                .await
                .expect("応答を読めること");
            match response {
                ControlMessage::RequestError(error) => {
                    assert_eq!(
                        error.error_code, REQUEST_INVALID_RANGE,
                        "INVALID_RANGE で拒否されること"
                    );
                }
                other => panic!("REQUEST_ERROR が届くこと: {other:?}"),
            }

            // アプリへは要求が渡らない (渡っていれば next_event が先に完了している)
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(
                !drive.is_finished(),
                "拒否された FETCH がアプリへ渡らないこと"
            );
            drive.abort();
        })
        .await;
    }
}
