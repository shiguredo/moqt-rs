//! MOQT peer (サーバー役) の in-process 試験基盤
//!
//! example の I/O 経路 (ハンドシェイク、peer 起点 request stream への送信、client 起点 request
//! stream の受理) を実行で検証するためのテスト専用モジュールである。ビルドで選んだ QUIC
//! バックエンドのサーバーを 127.0.0.1 で立て、sans-I/O な `Session` を `Session::new_server`
//! で駆動する。モック /
//! スタブは使わない。
//!
//! ライブラリと example の公開 API だけを使う (`MoqtClient` の private な内部状態には依存
//! しない)。`test-support` feature を有効にしたビルドでは他クレート (`crates/moq-pub` /
//! `crates/moq-sub`) の `#[cfg(test)] mod tests` から参照できる。feature を有効にしない
//! ビルドでは、この crate 自身のテストからのみ使う。
//!
//! `test-support` feature で他クレートへ公開するのは、example の公開 API だけを使った検証に
//! 必要な API である (接続確立、request stream の受理と応答、peer 起点 request stream への
//! 送信、応答の読み出し、uni data stream の内容と終端の読み出し、request stream GOAWAY /
//! PUBLISH_DONE の送信、終了コードの観測など)。次の能力はこの crate のテストからのみ使う
//! (`#[cfg(test)] pub(crate)`)。
//!
//! - SETUP の差し替え (`send_setup` / `send_control_stream_head` / `finish_control_stream`)
//! - 確立済み control stream への追加 control message の送信 (`send_control_message`)
//! - peer 起点 request stream への任意メッセージの先頭送信 (`send_opened_request_message`)
//! - request stream の FIN (`finish_request_stream`)、FETCH_OK / REQUEST_ERROR の応答書き出し
//!   (`send_fetch_ok` / `send_request_error`)、Session の状態取得 (`session_state`)
//! - datagram の送受信 (`send_datagram` / `recv_datagrams`)
//! - QMux 接続のテストサーバー (`QmuxTestPeerServer`) と QMux の close 観測
//!   (`wait_qmux_closed`)
//!
//! 対象は QUIC 直接接続 (`MoqtClient::establish_quic`) と QMux (`MoqtClient::establish_qmux`)
//! である。QMux は TCP + TLS を待ち受ける `QmuxTestPeerServer` を使う。WebTransport over
//! HTTP/3 / HTTP/2 のハーネスは必要になった時点で別途追加する。
//!
//! 提供するのは SETUP 交換 (送る SETUP の差し替え、SETUP を送らない control stream の終端を
//! 含む)、確立済み control stream への追加 control message の送信、peer 起点 request stream への
//! メッセージ送信、peer 起点 request stream に届く応答の読み出し、peer が開いた request stream への
//! 追加メッセージの送信、client 起点 request stream の受理と REQUEST_OK / SUBSCRIBE_OK /
//! PUBLISH_DONE による応答、client 起点 request stream への request stream GOAWAY の送信、
//! client 起点 request stream の後続メッセージ / 終端の読み出し、client が開いた uni data
//! stream の内容と終端種別 (FIN / RESET_STREAM) の読み出し、client が送る終了コード
//! (CONNECTION_CLOSE) の観測である。次は提供しない (必要になった時点で拡張する)。
//!
//! - peer が送る STOP_SENDING の観測 (QMux 経路は `QmuxTestPeerServer` と `qmux` モジュールの
//!   テストで観測する)

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use shiguredo_moqt::message::ControlMessage;
#[cfg(test)]
use shiguredo_moqt::message::Setup;
#[cfg(test)]
use shiguredo_moqt::message::common::Location;
use shiguredo_moqt::message_parameter::MessageParameters;
use shiguredo_moqt::parameter::{DEFAULT_MAX_CONTROL_MESSAGE_SIZE, SetupOptions};
use shiguredo_moqt::session::core::Session;
#[cfg(test)]
use shiguredo_moqt::session::types::SessionState;
use shiguredo_moqt::session::types::{RequestStreamEnd, SessionEvent, Transport as MoqtTransport};
use shiguredo_moqt::stream::encode_control_stream_setup;
use shiguredo_moqt::track_properties::TrackProperties;

use crate::error::{Result, TransportError};
use crate::moqt_client::{ControlStream, StreamRead};
use crate::transport::{BidiStreamAcceptor, SendStream, StreamAcceptor, StreamHandle};

/// テスト用 peer が使う制御メッセージ長の上限 (バイト)
///
/// 本ハーネスは自側で `MAX_CONTROL_MESSAGE_SIZE` を広告しないため、wire 上の既定値
/// (draft-ietf-moq-transport-23 §9.1.8 (MAX_CONTROL_MESSAGE_SIZE)) を使う。client も同じ
/// 既定値を受信上限に使うため、両方向で一致する。
const MAX_CONTROL_MESSAGE_SIZE: u64 = DEFAULT_MAX_CONTROL_MESSAGE_SIZE;

/// FETCH の初回成功応答に載せるパラメータを組み立てる
///
/// draft-ietf-moq-transport-23 §9.3 (REQUEST_OK): FETCH_OK が運べるのは EXPIRES と
/// LARGEST_OBJECT である。要求が End を省略している場合の応答範囲の終端は Largest Object
/// なので (§3.2 (Fetch) / §9.19.9 (LOCATION FILTER Parameter))、把握している値を
/// LARGEST_OBJECT として渡す。
#[cfg(test)]
fn fetch_ok_parameters(largest_object: Option<Location>) -> MessageParameters {
    let mut parameters = MessageParameters::new();
    if let Some(largest) = largest_object {
        parameters.set_largest_object(largest.group_id, largest.object_id);
    }
    parameters
}

// 以下の 3 つは `moqt_client` の対応する private 関数と同じ処理を含む。テスト専用の都合で
// 通常ビルド対象の可視性を広げないため (`shiguredo-rust` の「テストのために private な API を
// 無理矢理公開しないこと」)、あえて重複させている。`request_stream_id` は Table 5 の
// request stream 先頭に置ける 7 種類すべての Request ID を扱う。

/// 初期イベントとして積まれた SETUP を取り出す
///
/// `Session::new_server` は生成時に SETUP を `SessionEvent::SendControl` として積む
/// (draft-ietf-moq-transport-23 §9.1 (SETUP))。
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
/// (draft-ietf-moq-transport-23 §6.4.1 (Unidirectional Streams))。prefix を前置する
/// `encode_control_stream_setup` を使う。ストリームを開くのは呼び出し側である。
async fn send_control_stream_setup(
    control_send: &mut SendStream,
    setup: &ControlMessage,
) -> Result<()> {
    control_send
        .send(Bytes::from(encode_control_stream_setup(
            setup,
            MAX_CONTROL_MESSAGE_SIZE,
        )?))
        .await?;
    Ok(())
}

/// 先頭メッセージから request stream の Request ID を取り出す
///
/// draft-ietf-moq-transport-23 §6.3 (Session initialization) は request stream の先頭に
/// 7 種類のメッセージを許す。7 種類はすべて Request ID を持ち、応答専用の型は持たないため
/// `None` になる。
fn request_stream_id(message: &ControlMessage) -> Option<u64> {
    match message {
        ControlMessage::Subscribe(m) => Some(m.request_id),
        ControlMessage::Publish(m) => Some(m.request_id),
        ControlMessage::Fetch(m) => Some(m.request_id),
        ControlMessage::TrackStatus(m) => Some(m.request_id),
        ControlMessage::PublishNamespace(m) => Some(m.request_id),
        ControlMessage::SubscribeNamespace(m) => Some(m.request_id),
        ControlMessage::SubscribeTracks(m) => Some(m.request_id),
        _ => None,
    }
}

/// client が送る終了コードの観測を待つ上限時間
///
/// テスト全体の上限 ([`tests::TEST_TIMEOUT`]) より短くし、終了コードを送らない退行を
/// 「終了コードが届かない」として切り分けられるようにする。
const CLOSE_CODE_TIMEOUT: Duration = Duration::from_secs(5);

/// client が開いた request stream から読んだ後続メッセージまたは終端
///
/// [`TestPeer::recv_request_stream_message`] が返す。REQUEST_UPDATE のような request stream の
/// 後続メッセージの受信と、request stream が FIN / RESET_STREAM で終端したことの観測に使う。
#[derive(Debug)]
pub enum RequestStreamRead {
    /// request stream の後続メッセージ
    Message(ControlMessage),
    /// request stream の終端 (FIN / RESET_STREAM)
    Closed(RequestStreamEnd),
}

/// client の接続を受理する前の in-process peer (QUIC サーバー)
pub struct TestPeerServer {
    /// 接続を受理する QUIC サーバー
    server: crate::quic::Server,
    /// 待ち受けているアドレス
    local_addr: SocketAddr,
}

impl TestPeerServer {
    /// QUIC サーバーを 127.0.0.1 で起動する
    ///
    /// サーバー証明書は自己署名を rcgen で生成し、ALPN には MOQT のプロトコル識別子を広告する。
    /// client は ALPN で [`crate::MOQT_PROTOCOL`] を要求するため、既定の `h3` のままだと
    /// ハンドシェイクが HANDSHAKE_FAILURE で失敗する。
    pub async fn start() -> Result<Self> {
        Self::start_with_alpn(&[crate::MOQT_PROTOCOL.as_bytes()]).await
    }

    /// ALPN に載せる識別子を指定して QUIC サーバーを 127.0.0.1 で起動する
    ///
    /// `alpn` が空の場合は ALPN を広告しないサーバーになる。MOQT のプロトコル識別子を
    /// 選択しない相手を client が拒否することを、実接続で確かめるために使う。
    pub async fn start_with_alpn(alpn: &[&[u8]]) -> Result<Self> {
        let certified =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).map_err(|e| {
                TransportError::Internal(format!("failed to generate certificate: {e}"))
            })?;
        // IPv4 のみを待ち受ける (client は 127.0.0.1 へ接続する)
        let tls = crate::quic::ServerTls {
            cert_der: certified.cert.der().to_vec(),
            key_der: certified.signing_key.serialize_der(),
            alpn: alpn.iter().map(|protocol| protocol.to_vec()).collect(),
        };
        let server = crate::quic::start_server(
            tls,
            "127.0.0.1:0"
                .parse()
                .expect("the wildcard address literal must parse"),
        )
        .await?;
        let local_addr = server.local_addr();

        Ok(Self { server, local_addr })
    }

    /// 待ち受けているアドレス
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// client の接続を 1 つ受理する
    pub async fn accept(&mut self) -> Result<TestPeer> {
        let connection = self.server.accept().await?;
        TestPeer::new(connection).await
    }
}

/// QMux (TCP + TLS) のテスト用サーバー
///
/// [`TestPeerServer`] の QMux 版である。UDP を使わない経路を実行で検証するために置く。
/// TCP を 127.0.0.1 で待ち受け、TLS (ALPN は [`crate::MOQT_PROTOCOL`]) で受理してから
/// `shiguredo_tokio_qmux::Connection` を作る。QMux 自体はハンドシェイクを持たないため、接続が
/// 確立した時点でクライアントの制御ストリームを受理できる。
#[cfg(test)]
pub(crate) struct QmuxTestPeerServer {
    /// client の接続を待ち受ける TCP リスナー
    listener: tokio::net::TcpListener,
    /// TLS の受理器 (server 証明書は自己署名)
    tls: tokio_rustls::TlsAcceptor,
    /// 待ち受けているアドレス
    local_addr: SocketAddr,
}

#[cfg(test)]
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
/// 送信半と reader の両方を保持する。QUIC では送信半の drop で RESET_STREAM、受信半の
/// drop で STOP_SENDING が送られるため、保持しないと request の終端
/// (draft-ietf-moq-transport-23 §6.4.2.2 (Graceful Request Stream Closure) /
/// §6.4.2.3 (Request Cancellation and Rejection)) が peer に観測される。テストが終わるまで
/// 保持する。
struct AcceptedRequest {
    /// 受理した request の Request ID
    request_id: u64,
    /// 応答を書く送信半
    send: SendStream,
    /// 後続メッセージを読む reader
    ///
    /// [`TestPeer::recv_request_stream_message`] が読む。drop するまで受信半も保持する。
    reader: ControlStream,
}

/// peer が開いた request stream
///
/// 送信半と reader を保持する。QUIC では drop すると送信半は RESET_STREAM、受信半は
/// STOP_SENDING として peer に観測されるため、テストが終わるまで保持する。
struct OpenedRequest {
    /// 先頭メッセージの Request ID (先頭メッセージが request でない場合は `None`)
    request_id: Option<u64>,
    /// 送信半 (drop すると RESET_STREAM が peer へ届くため保持する)
    ///
    /// 送信半を読む API (`send_opened_request_message` / `finish_request_stream`) は
    /// `cfg(test)` にだけ現れるため、`test-support` を有効にした非テストビルドでは
    /// 読み出しだけが無くなる。フィールド自体は寿命のために保持する。
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "送信半の drop で RESET_STREAM を送らないため保持する"
        )
    )]
    send: SendStream,
    /// 応答を読む reader
    reader: ControlStream,
}

/// 受理済みの in-process peer (MOQT のサーバー役)
pub struct TestPeer {
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
    /// SETUP の送信後に保持し、`send_goaway` / `send_control_message` による追加 control
    /// message の送信と `finish_control_stream` の FIN に使う。drop すると制御ストリームが
    /// 終端され、peer がセッション終了として観測するため保持する。
    control_send: Option<SendStream>,
    /// 自側 control stream (uni) の受信半
    ///
    /// フィールドは読み出さないが、この値が drop されるまで受信半を保持する。
    control_recv: Option<ControlStream>,
    /// 自側が開いた request stream
    ///
    /// 送信半の drop は RESET_STREAM、reader の drop は STOP_SENDING として peer に観測されるため保持する。
    opened_requests: Vec<OpenedRequest>,
    /// peer から受理した request stream
    accepted_requests: Vec<AcceptedRequest>,
}

impl TestPeer {
    /// 確立した QMux 接続から peer を作る
    ///
    /// QUIC 経路と違い、ストリームの受理は接続を共有した別タスクが行い、その結果が
    /// `uni_rx` / `bi_rx` に流れる ([`crate::qmux::split_connection`])。
    #[cfg(test)]
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
        })
    }

    /// 受理した QUIC 接続から peer を作る
    async fn new(connection: crate::quic::Connected) -> Result<Self> {
        // バックグラウンド駆動はテスト用サーバーの生成時に有効化済みである
        // (`start_server` が `with_background_driver(true)` を渡す)。テストは peer の
        // 受信ループを常に poll するわけではないため、これが無いと接続終了の観測
        // (`recv_close_code`) が止まる
        let (handle, acceptor) = connection.split();
        // tobi-quic の `StreamAcceptor` は単方向と双方向の両方を受け付けるため `Arc` で共有する
        let acceptor = Arc::new(acceptor);
        let session = Session::new_server(MoqtTransport::Quic, SetupOptions::new())?;
        Ok(Self {
            session,
            handle: StreamHandle::Quic(handle),
            bidi_acceptor: BidiStreamAcceptor::Quic(Arc::clone(&acceptor)),
            recv_acceptor: StreamAcceptor::Quic(acceptor),
            control_send: None,
            control_recv: None,
            opened_requests: Vec::new(),
            accepted_requests: Vec::new(),
        })
    }

    /// SETUP を交換してセッションを確立する
    ///
    /// 自側の control stream (uni) を開いて SETUP を送り、client の control stream を受理して
    /// stream type を検証してから SETUP を取り込む。SETUP を送らないと client のハンドシェイクは
    /// 完了しない (`MoqtClient::establish_quic` は peer SETUP を待つ)。client の SETUP を
    /// 取り込まないと session は `Established` にならず、`Session::recv_request` が
    /// `BeforeSessionEstablished` を返す。
    pub async fn establish(&mut self) -> Result<()> {
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
        let mut control_recv = ControlStream::new(recv, MAX_CONTROL_MESSAGE_SIZE);
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
    /// (draft-ietf-moq-transport-23 §6.3 (Session initialization))。メッセージの種類は問わない
    /// (request 以外を送って example の種別判定を検証できる)。Request ID を含む request を
    /// 送る場合は、peer 役割の parity に合わせて [`Self::next_request_id`] で採番した値を
    /// 渡すこと。
    pub async fn send_request_start(&mut self, message: ControlMessage) -> Result<()> {
        let (mut send, recv) = self.handle.open_bidi_stream().await?;
        send.send(Bytes::from(message.encode(MAX_CONTROL_MESSAGE_SIZE)?))
            .await?;
        self.opened_requests.push(OpenedRequest {
            request_id: request_stream_id(&message),
            send,
            reader: ControlStream::new(recv, MAX_CONTROL_MESSAGE_SIZE),
        });
        Ok(())
    }

    /// peer 起動の uni stream を 1 つ受理する
    ///
    /// 受理したストリームの受信方向から STOP_SENDING を送るテストが使う
    /// ([`Self::recv_uni_data_stream`] は終端まで読み切るため、ストリームを返さない)。
    pub async fn accept_recv_stream(&mut self) -> Result<crate::transport::RecvStream> {
        self.recv_acceptor
            .accept_recv_stream()
            .await?
            .ok_or_else(|| TransportError::Internal("data stream acceptor closed".into()))
    }

    /// datagram を 1 つ送る
    ///
    /// 音声の OBJECT_DATAGRAM が QMux 経由で届くことを検証するテストが使う。
    #[cfg(test)]
    pub(crate) async fn send_datagram(&self, data: &[u8]) -> Result<()> {
        self.handle.send_datagram(data).await
    }

    /// datagram を 1 つ以上受信する
    ///
    /// 1 件届くまで待ち、到着済みをまとめて返す (`StreamHandle::recv_datagrams` と同じ契約)。
    #[cfg(test)]
    pub(crate) async fn recv_datagrams(&self) -> Result<Vec<Vec<u8>>> {
        self.handle.recv_datagrams().await
    }

    /// 自側 (publisher) が track に Object を 1 つ publish したことを Session に記録する
    ///
    /// wire には送らない。FETCH を受ける publisher は、publisher 役の subscription が
    /// 存在する track について「Object を publish していない」場合に FETCH を INVALID_RANGE で
    /// 拒否する (draft-ietf-moq-transport-23 §3.2 (Fetch))。したがって FETCH を受理させる
    /// テストは、購読の確立後にこのメソッドで publish 済みの Location を記録してから
    /// [`Self::recv_request_start`] を呼ぶ。publisher 役の subscription が存在しない track の
    /// FETCH はこの判定を通らずアプリへ渡る。
    ///
    /// 記録は `Session::send_object_datagram` が行う。このハーネスは Session が積む
    /// datagram 送信イベントを transport へ書き出さないため、wire には何も送られない
    /// (バックエンドが datagram に対応しているかどうかには依存しない)。
    ///
    /// # Errors
    ///
    /// Session が確立していない、受理していない request、publisher 役でない購読、
    /// 確立前の購読を指定した場合は [`TransportError`] になる。
    pub fn note_published_object(
        &mut self,
        request_id: u64,
        group_id: u64,
        object_id: u64,
    ) -> Result<()> {
        self.session
            .send_object_datagram(request_id, group_id, object_id, None, None, false)
            .map_err(|e| TransportError::Internal(format!("send_object_datagram: {e}")))
    }

    /// peer が開いた request stream に届いたメッセージを 1 つ読む
    ///
    /// `request_id` は [`Self::send_request_start`] に渡したメッセージの Request ID である。
    /// example が request に応答したこと (REQUEST_ERROR など) を確認するために使う。
    /// その Request ID の request stream を開いていない場合と、応答の前に終端された場合は
    /// `TransportError::Internal` を返す。
    pub async fn recv_response(&mut self, request_id: u64) -> Result<ControlMessage> {
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
    ///
    /// Session がメッセージ単位の拒否をした場合 (REQUEST_ERROR をイベントとして積み、
    /// `recv_request` 自体は `Ok` を返す場合) は `TransportError::Internal` を返す。
    /// 拒否された request は Session が応答を送るため、テスト側が応答してはならない。
    pub async fn recv_request_start(&mut self) -> Result<ControlMessage> {
        let (send, recv) = self
            .bidi_acceptor
            .accept_bidi_stream()
            .await?
            .ok_or_else(|| {
                TransportError::Quic("connection closed before peer request stream".into())
            })?;
        let mut reader = ControlStream::new(recv, MAX_CONTROL_MESSAGE_SIZE);
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
        // メッセージ単位の拒否 (REQUEST_ERROR) は Session が積んで `recv_request` は Ok を
        // 返す。拒否された request は Session が応答を送るため、アプリ (テスト) が応答しては
        // ならない。追跡表に載っていなければ拒否とみなす
        let tracked = match &message {
            ControlMessage::Subscribe(subscribe) => {
                self.session.subscription(subscribe.request_id).is_some()
            }
            // PUBLISH も subscriber 役の subscription として追跡される
            ControlMessage::Publish(publish) => {
                self.session.subscription(publish.request_id).is_some()
            }
            ControlMessage::Fetch(fetch) => self.session.fetch(fetch.request_id).is_some(),
            // 他の request 種別は本ハーネスの利用側で使っていないため、従来どおり受理として扱う
            _ => true,
        };
        if !tracked {
            return Err(TransportError::Internal(format!(
                "request stream start was rejected by the session: {message:?}"
            )));
        }
        self.accepted_requests.push(AcceptedRequest {
            request_id,
            send,
            reader,
        });
        Ok(message)
    }

    /// client が開いた request stream の後続メッセージを 1 つ読む
    ///
    /// `request_id` は [`TestPeer::recv_request_start`] で受理した request の Request ID である。
    /// request stream に届く REQUEST_UPDATE などの後続メッセージを [`RequestStreamRead::Message`]
    /// として返し、FIN / RESET_STREAM による終端は [`RequestStreamRead::Closed`] として返す。
    /// request stream を受理していない場合は [`TransportError`] になる。
    pub async fn recv_request_stream_message(
        &mut self,
        request_id: u64,
    ) -> Result<RequestStreamRead> {
        let request = self
            .accepted_requests
            .iter_mut()
            .find(|request| request.request_id == request_id)
            .ok_or_else(|| {
                TransportError::Internal(format!("request stream not found: {request_id}"))
            })?;
        match request.reader.recv_message().await? {
            StreamRead::Value(message) => Ok(RequestStreamRead::Message(message)),
            StreamRead::Closed(end) => Ok(RequestStreamRead::Closed(end)),
        }
    }

    /// 受理した request に REQUEST_OK で応答する
    ///
    /// `Session::send_request_ok` は応答を `SessionEvent::SendOnStream` として積むため、該当する
    /// request stream へ書き出す。応答を返さないと client の API (`MoqtClient::publish_track`
    /// など) は応答待ちのまま解決しない。
    pub async fn send_request_ok(&mut self, request_id: u64) -> Result<()> {
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
        request
            .send
            .send(Bytes::from(message.encode(MAX_CONTROL_MESSAGE_SIZE)?))
            .await?;
        if fin {
            request.send.finish().await?;
        }
        Ok(())
    }

    /// 受理した FETCH に FETCH_OK で応答する (応答パラメータを指定する版)
    ///
    /// [`TestPeer::send_request_ok`] は REQUEST_UPDATE_OK などの汎用の OK を送るため、
    /// FETCH の初回応答 (`FETCH_OK`) には使えない (Session は `send_fetch_ok` を要求する)。
    /// 本メソッドは [`TestPeer::send_request_ok`] と同じ手順で、`Session::send_fetch_ok` が
    /// 積んだ応答を該当する request stream へ書き出す。
    ///
    /// # Errors
    ///
    /// Session が確立していない、受理していない FETCH、publisher 役でない、応答範囲の
    /// 終端を確定できない場合は [`TransportError`] になる。
    pub async fn send_fetch_ok_with_parameters(
        &mut self,
        request_id: u64,
        parameters: MessageParameters,
        track_properties: TrackProperties,
    ) -> Result<()> {
        self.session
            .send_fetch_ok(request_id, parameters, track_properties)
            .map_err(|e| TransportError::Internal(format!("send_fetch_ok: {e}")))?;
        self.flush_queued_response(request_id).await
    }

    /// 受理した SUBSCRIBE に SUBSCRIBE_OK で応答する
    ///
    /// [`TestPeer::send_request_ok`] と同じ手順で応答を書き出す。SUBSCRIBE_OK は
    /// track alias に加えて応答パラメータ (`LARGEST_OBJECT` / `EXPIRES` など) と
    /// Track Properties (`DYNAMIC_GROUPS` など) を運ぶため、`parameters` と
    /// `track_properties` をテストから指定できる。
    pub async fn send_subscribe_ok(
        &mut self,
        request_id: u64,
        track_alias: u64,
        parameters: MessageParameters,
        track_properties: TrackProperties,
    ) -> Result<()> {
        self.session
            .send_subscribe_ok(request_id, track_alias, parameters, track_properties)
            .map_err(|e| TransportError::Internal(format!("send_subscribe_ok: {e}")))?;
        self.flush_queued_response(request_id).await
    }

    /// 受理した SUBSCRIBE に PUBLISH_DONE で応答する
    ///
    /// draft-ietf-moq-transport-23 §9.9 (PUBLISH_DONE)。publisher 役の peer が subscription を
    /// 終端する最終メッセージであり、`stream_count` は subscription で開いた data stream の数に
    /// 一致させる (0 本なら 0)。`status_code` は PUBLISH_DONE のレジストリのコードを使うこと
    /// (LOCAL 系のコードは Session が `PUBLISH_DONE_INTERNAL_ERROR` に置き換える)。
    /// reason phrase は空文字列にする。
    ///
    /// `Session::send_publish_done` は応答を `SessionEvent::SendOnStream` として FIN 付きで積む
    /// ため、既存の応答書き出し (`flush_queued_response`) と同じ手順で該当する request stream
    /// へ書き出す。
    ///
    /// [`Self::open_uni_send_stream`] で直接開いた uni stream は Session の
    /// `published_count` に入らない。そのため、そのような stream を送った track では
    /// Session が追跡する `stream_count` と実際に開いた数が一致せず、正しい `stream_count` を
    /// このメソッドへ渡せない (Session は追跡値との一致、または 2^64 - 1 (unknown) を要求する)。
    /// stream 数を一致させたい場合は `DataPlaneHandle` 経由で Object を送る。
    ///
    /// # Errors
    ///
    /// 次の場合は [`TransportError`] になる。
    ///
    /// - Session が確立していない
    /// - 受理していない request (subscription が見つからない)
    /// - publisher 役でない subscription、または PUBLISH_DONE を送れない状態
    /// - 未終端の outgoing data stream が残っている
    /// - `stream_count` が Session の追跡値と一致しない (0 本のときは 0 に限る。
    ///   1 本以上のときは正確な値か 2^64 - 1 (unknown) のいずれか)
    pub async fn send_publish_done(
        &mut self,
        request_id: u64,
        status_code: u64,
        stream_count: u64,
    ) -> Result<()> {
        let reason = shiguredo_moqt::message::ReasonPhrase::new("")
            .map_err(|e| TransportError::Internal(format!("invalid reason phrase: {e}")))?;
        self.session
            .send_publish_done(request_id, status_code, stream_count, reason)
            .map_err(|e| TransportError::Internal(format!("send_publish_done: {e}")))?;
        self.flush_queued_response(request_id).await
    }

    /// 受理した FETCH に初回成功応答 (REQUEST_OK、別名 FETCH_OK) で応答する
    ///
    /// draft-ietf-moq-transport-23 §3.2 (Fetch): 応答範囲の終端は LARGEST_OBJECT が
    /// 要求範囲内ならその値、そうでなければ要求で明示した
    /// End Location になる。テストの要求は End を省略することが多いため、把握している
    /// Largest Object を LARGEST_OBJECT として渡せるようにする。
    #[cfg(test)]
    pub(crate) async fn send_fetch_ok(
        &mut self,
        request_id: u64,
        largest_object: Option<Location>,
    ) -> Result<()> {
        self.session
            .send_fetch_ok(
                request_id,
                fetch_ok_parameters(largest_object),
                TrackProperties::new(),
            )
            .map_err(|e| TransportError::Internal(format!("send_fetch_ok: {e}")))?;
        self.flush_queued_response(request_id).await
    }

    /// 受理した request に REQUEST_ERROR で応答する
    ///
    /// `Session::send_request_error` は応答を `SessionEvent::SendOnStream` として積むため、該当する
    /// request stream へ書き出す。`error_code` は wire に載るコードを使うこと (LOCAL 系のコードは
    /// Session が INTERNAL_ERROR に置き換える)。
    #[cfg(test)]
    pub(crate) async fn send_request_error(
        &mut self,
        request_id: u64,
        error_code: u64,
        retry_interval: u64,
        reason: &str,
    ) -> Result<()> {
        let reason = shiguredo_moqt::message::ReasonPhrase::new(reason)
            .map_err(|e| TransportError::Internal(format!("invalid reason phrase: {e}")))?;
        self.session
            .send_request_error(
                request_id,
                error_code,
                retry_interval,
                reason,
                shiguredo_moqt::message::ErrorDetails::new(),
            )
            .map_err(|e| TransportError::Internal(format!("send_request_error: {e}")))?;
        self.flush_queued_response(request_id).await
    }

    /// 自側が開く uni stream を開き、送信ハンドルを返す
    ///
    /// FETCH 応答 (draft-ietf-moq-transport-23 §11.4.1) のように、peer が自ら uni stream を
    /// 開いて書く経路をテストで再現するために使う。
    pub async fn open_uni_send_stream(&mut self) -> Result<crate::transport::SendStream> {
        self.handle.open_send_stream().await
    }

    /// 自側 control stream に GOAWAY を書く
    ///
    /// draft-ietf-moq-transport-23 §9.2 (GOAWAY)。`new_session_uri` が空でない場合は、
    /// 受け取った client がその URI へ移行する (server 役だけが非空 URI を送れる)。
    /// `timeout_ms` は sender が閉じるまでの時間で、0 は期限なしを意味する。
    pub async fn send_goaway(&mut self, new_session_uri: Vec<u8>, timeout_ms: u64) -> Result<()> {
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
        control_send
            .send(Bytes::from(message.encode(MAX_CONTROL_MESSAGE_SIZE)?))
            .await?;
        Ok(())
    }

    /// client が開いた request stream に request stream GOAWAY を書く
    ///
    /// draft-ietf-moq-transport-23 §9.2 (GOAWAY)。個別の request を移行させる GOAWAY であり、
    /// control stream の GOAWAY ([`TestPeer::send_goaway`]) のようにセッション全体は閉じない。
    /// request の受信側 (relay 役の peer) だけが、受理済みの request stream へ送れる。
    ///
    /// `new_session_uri` が空でない場合は、受け取った client がその URI で request を再発行する。
    /// `timeout_ms` は sender が request stream を reset するまでの時間で、0 は期限なしを意味する。
    ///
    /// `Session::send_goaway_on_request_stream` は `SessionEvent::SendOnStream` を積むため、
    /// 既存の応答書き出し (`flush_queued_response`) と同じ手順で該当する request stream へ
    /// 書き出す。
    ///
    /// # Errors
    ///
    /// 次の場合は [`TransportError`] になる。
    ///
    /// - Session が確立していない
    /// - 受理していない (Session が追跡していない) request への送信
    /// - request の sender (requester) としての送信
    /// - 同じ request stream への GOAWAY の 2 回目の送信
    /// - `new_session_uri` が 8192 バイトを超える場合
    ///
    /// `new_session_uri` の形式 (scheme や UTF-8) は Session では検証されない。移行先として
    /// 解釈するのは受信側 (`tokio_moq::migration_target`) の責務である。
    pub async fn send_goaway_on_request_stream(
        &mut self,
        request_id: u64,
        new_session_uri: Vec<u8>,
        timeout_ms: u64,
    ) -> Result<()> {
        self.session
            .send_goaway_on_request_stream(request_id, new_session_uri, timeout_ms)
            .map_err(|e| TransportError::Internal(format!("send_goaway_on_request_stream: {e}")))?;
        self.flush_queued_response(request_id).await
    }

    /// 確立済みの control stream に control message を書く
    ///
    /// stream type prefix は付けない (control stream の先頭で送信済みのため)。確立後に届く
    /// 2 通目の SETUP など、本来送られない control message を再現するために使う。
    /// 新しい uni stream を開く [`TestPeer::send_setup`] / [`TestPeer::send_control_stream_head`]
    /// は既存の control stream を FIN するため、この用途には使えない。
    #[cfg(test)]
    pub(crate) async fn send_control_message(&mut self, message: ControlMessage) -> Result<()> {
        let control_send = self
            .control_send
            .as_mut()
            .ok_or_else(|| TransportError::Internal("control stream is not opened".into()))?;
        control_send
            .send(Bytes::from(message.encode(MAX_CONTROL_MESSAGE_SIZE)?))
            .await?;
        Ok(())
    }

    /// 自側が開いた request stream に 2 通目以降の control message を書く
    ///
    /// `Session` の状態機械を経由せず wire を直接書く。確立後の request stream に届く
    /// 不正なメッセージ (Table 5 で "First" を持つ型の 2 通目など) を再現するために使う。
    #[cfg(test)]
    pub(crate) async fn send_opened_request_message(
        &mut self,
        request_id: u64,
        message: ControlMessage,
    ) -> Result<()> {
        let request = self
            .opened_requests
            .iter_mut()
            .find(|request| request.request_id == Some(request_id))
            .ok_or_else(|| {
                TransportError::Internal(format!("opened request not found: {request_id}"))
            })?;
        request
            .send
            .send(Bytes::from(message.encode(MAX_CONTROL_MESSAGE_SIZE)?))
            .await?;
        Ok(())
    }

    /// peer が開いた request stream の送信方向を FIN で閉じる
    ///
    /// example が拒否した request のストリーム終端を通知する経路を確かめるために使う。
    #[cfg(test)]
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

    /// client が開いた uni data stream を受理し、終端まで読む
    ///
    /// FETCH 応答や subgroup stream の内容を、client が書いたバイト列のまま取り出す。
    /// 終端種別 (FIN / RESET_STREAM) も確認する場合は
    /// [`Self::recv_uni_data_stream_end`] を使う。
    pub async fn recv_uni_data_stream(&mut self) -> Result<Vec<u8>> {
        Ok(self.recv_uni_data_stream_end().await?.0)
    }

    /// client が開いた uni data stream を受理し、終端まで読んで内容と終端種別を返す
    ///
    /// FETCH 応答や subgroup stream の内容を client が書いたバイト列のまま取り出し、
    /// 終端が FIN だったか RESET_STREAM だったかを [`RequestStreamEnd`] で返す。
    /// FIN を確認するテスト (FETCH 応答を最後まで書いて閉じたことの検証など) はこちらを使う。
    /// 終端種別を区別する必要が無い場合は [`Self::recv_uni_data_stream`] を使える。
    ///
    /// RESET_STREAM はエラーではなく [`RequestStreamEnd::Reset`] として返る。この場合、
    /// `RESET_STREAM` を受けるまでに読めたバイト列 (空のこともある) が第 1 要素になる。
    ///
    /// # Errors
    ///
    /// - 接続が終了している、または test peer の data stream acceptor が閉じている場合
    /// - 受信した stream の読み取りに失敗した場合 (RESET_STREAM 以外の transport エラー)
    ///
    /// Session の状態 (確立前など) や subscription の `stream_count` は、この読み出し API
    /// では検証しない。`stream_count` の不一致は PUBLISH_DONE を送る
    /// [`Self::send_publish_done`] の呼び出し時点で Session が検出する。
    pub async fn recv_uni_data_stream_end(&mut self) -> Result<(Vec<u8>, RequestStreamEnd)> {
        let mut stream = self
            .recv_acceptor
            .accept_recv_stream()
            .await?
            .ok_or_else(|| TransportError::Internal("data stream acceptor closed".into()))?;
        let mut buf = Vec::new();
        loop {
            match stream.receive_chunk().await? {
                crate::transport::RecvChunk::Data(data) => buf.extend_from_slice(&data),
                crate::transport::RecvChunk::End(end) => return Ok((buf, end)),
            }
        }
    }

    /// 自側の次の Request ID を採番する (server 役は奇数)
    pub fn next_request_id(&mut self) -> Result<u64> {
        Ok(self.session.next_local_request_id()?)
    }

    /// サーバー側 session の状態
    #[cfg(test)]
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
    #[cfg(test)]
    pub(crate) async fn send_control_stream_head(&mut self, head: Vec<u8>) -> Result<()> {
        let mut control_send = self.handle.open_send_stream().await?;
        if !head.is_empty() {
            control_send.send(Bytes::from(head)).await?;
        }
        self.control_send = Some(control_send);
        Ok(())
    }

    /// 自側 control stream の送信方向を FIN で終端する
    #[cfg(test)]
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
    #[cfg(test)]
    pub(crate) async fn send_setup(&mut self, options: &SetupOptions) -> Result<()> {
        let setup = ControlMessage::Setup(Setup {
            options: options.clone(),
        });
        let head =
            shiguredo_moqt::stream::encode_control_stream_setup(&setup, MAX_CONTROL_MESSAGE_SIZE)?;
        self.send_control_stream_head(head).await
    }

    /// QMux 接続が閉じた理由を待つ
    ///
    /// QMux 経路以外では `None` を返す。QMux の接続終了は `Connection::closed` で観測でき、
    /// ピアが CONNECTION_CLOSE を受けた場合は `CloseReason::Peer`、下位トランスポートの
    /// 終了の場合は `CloseReason::PeerShutdown` などになる。
    #[cfg(test)]
    pub(crate) async fn wait_qmux_closed(&self) -> Option<shiguredo_qmux::event::CloseReason> {
        match &self.handle {
            StreamHandle::Qmux(handle) => Some(handle.closed().await),
            _ => None,
        }
    }

    /// peer が観測した client の終了コード (CONNECTION_CLOSE) を待つ
    ///
    /// example が終了コードを送らずに接続を切った場合と、上限時間までに届かなかった場合は
    /// `TransportError::Internal` を返す。
    pub async fn recv_close_code(&mut self) -> Result<u64> {
        let connection = match &self.handle {
            StreamHandle::Quic(connection) => connection,
            // WebTransport (h3 / h2) と QMux は接続の終了コードをこの API では観測しない。
            // Windows ビルドでは WtH3 variant 自体が無いため、ワイルドカードで受ける
            _ => {
                return Err(TransportError::Internal(
                    "recv_close_code is only available for the QUIC transport".into(),
                ));
            }
        };
        let closed = tokio::time::timeout(CLOSE_CODE_TIMEOUT, connection.closed())
            .await
            .map_err(|_| TransportError::Internal("client did not send CONNECTION_CLOSE".into()))?;
        // 正常終了 (終了コード 0) はコード 0 として観測する
        match closed.as_ref().and_then(|reason| reason.application_code()) {
            Some(code) => Ok(code),
            None => Err(TransportError::Internal(format!(
                "client did not send an application CONNECTION_CLOSE: {closed:?}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::time::{Duration, Instant};

    use shiguredo_moqt::error::{
        SESSION_CLOSED_CONTROL_STREAM, SESSION_FIELD_LENGTH_EXCEEDED, SESSION_INVALID_FIELD,
        SESSION_INVALID_PATH, SESSION_INVALID_REQUEST_ID, SESSION_INVALID_STREAM_HEADER,
        SESSION_MALFORMED_AUTH_TOKEN, SESSION_MALFORMED_KEY_VALUE, SESSION_ROLE_VIOLATION,
        SESSION_UNEXPECTED_MESSAGE,
    };
    use shiguredo_moqt::message::RequestOk;
    use shiguredo_moqt::message::Subscribe;
    use shiguredo_moqt::message::SubscribeNamespace;
    use shiguredo_moqt::message::common::TrackNamespace;
    use shiguredo_moqt::parameter::{SetupOption, SetupOptionValue};
    use shiguredo_moqt::track_properties::{
        PROP_DYNAMIC_GROUPS, TrackProperty, TrackPropertyValue,
    };

    use super::*;
    use crate::moqt_client::{ClientEvent, MoqtClient};

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

    /// future を 1 回だけ poll する
    ///
    /// 待ち手の登録だけを行い、起こされても再 poll しない状態を作るために使う。
    /// 起こされないよう noop waker を渡す。
    fn poll_once<F: std::future::Future>(
        future: std::pin::Pin<&mut F>,
    ) -> std::task::Poll<F::Output> {
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        future.poll(&mut context)
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
        // server はこの関数を抜けるまで保持する。待ち受けを意図せず閉じないことを
        // コードで示しておく
        let mut server = TestPeerServer::start()
            .await
            .expect("サーバーを起動できること");
        let addr = server.local_addr();

        // 接続と受理は並行に進める。tobi-quic のサーバーは `accept()` の poll で
        // ハンドシェイクを進めるため、client の `connect()` を先に待つと停止する
        let (connection, peer) = tokio::join!(
            crate::quic::connect(addr, "localhost", None),
            server.accept(),
        );
        let (connection, stop_sending_rx) = connection.expect("接続できること");
        let mut peer = peer.expect("接続を受理できること");

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

    /// 未知の request id は `subscription_cleanup_ready` で `None` を返すこと
    ///
    /// アプリが終端済み subscription の cleanup 状態を [`MoqtClient::subscription_cleanup_ready`]
    /// で問うた後、Session が回収済み (`Session::forget_subscription` 済み) の request id と
    /// 存在しない request id のどちらも `None` になる受け取り方を固定する。
    #[tokio::test(flavor = "multi_thread")]
    async fn subscription_cleanup_ready_returns_none_for_unknown_request() {
        run_with_timeout(async {
            let (_peer, client) = connect_pair().await;
            // 購読していない session なので、どの request id も未知である。Session 側の
            // request id は 0 始まりの採番のため、測定には想定外の大きな値も含める
            assert_eq!(
                client.subscription_cleanup_ready(u64::MAX),
                None,
                "未知の request id は None を返すこと"
            );
        })
        .await;
    }

    /// 受理待ちをキャンセルしても、その後に届いた uni stream を失わないこと
    ///
    /// example の main loop は `tokio::select!` で受理待ちの future を頻繁に破棄する。
    /// バックエンドの受理待ちがキャンセル安全でないと、キャンセル直後に到着した stream が
    /// 失われ、次の accept が返らない。キャンセル → 到着 → 再 accept の順で固定する。
    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_uni_stream_accept_does_not_lose_the_stream() {
        run_with_timeout(async {
            let (mut peer, _client, mut acceptor) = connect_pair_with_acceptor().await;

            // ドライバへ受理待ちを登録させたうえで future を破棄する。この時点では
            // stream が無いため、accept は時間切れになる
            let cancelled =
                tokio::time::timeout(Duration::from_millis(100), acceptor.accept_recv_stream())
                    .await;
            assert!(
                cancelled.is_err(),
                "stream が無い間は accept が時間切れになること"
            );

            // キャンセル後に peer が uni stream を開く
            let mut send = peer
                .open_uni_send_stream()
                .await
                .expect("uni stream を開けること");
            send.send(Bytes::from_static(b"cancelled-accept"))
                .await
                .expect("stream にデータを書けること");
            send.finish().await.expect("stream を FIN で閉じられること");

            // キャンセル後に到着した stream を次の accept で取得できる
            let received =
                tokio::time::timeout(Duration::from_secs(5), acceptor.accept_recv_stream())
                    .await
                    .expect("キャンセル後に届いた uni stream を受理できること")
                    .expect("accept がエラーにならないこと");
            assert!(
                received.is_some(),
                "キャンセル後に届いた uni stream が失われないこと"
            );
        })
        .await;
    }

    /// 受理待ちが保留のまま破棄されても、引き渡し済みの stream を失わないこと
    ///
    /// 受理待ちの future を 1 回だけ poll して待ち手として登録し、ドライバが stream を
    /// 引き渡したあとに値を受け取らず破棄する。oneshot で直接渡していると、oneshot に
    /// 格納されたストリームごと破棄され、以降のデータも読めなくなる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_a_pending_accept_does_not_lose_the_delivered_stream() {
        run_with_timeout(async {
            let (mut peer, _client, mut acceptor) = connect_pair_with_acceptor().await;

            // 受理待ちを 1 回だけ poll して待ち手として登録し、保留のまま保持する
            let mut accept = Box::pin(acceptor.accept_recv_stream());
            assert!(
                poll_once(accept.as_mut()).is_pending(),
                "stream が無い間は受理待ちが完了しないこと"
            );

            // peer が uni stream を開く。ドライバは登録済みの待ち手へ引き渡す
            let mut send = peer
                .open_uni_send_stream()
                .await
                .expect("uni stream を開けること");
            send.send(Bytes::from_static(b"pending-accept"))
                .await
                .expect("stream にデータを書けること");
            send.finish().await.expect("stream を FIN で閉じられること");

            // 引き渡しが起きるまで待ってから、値を受け取らずに受理待ちを破棄する
            tokio::time::sleep(Duration::from_millis(200)).await;
            drop(accept);

            // 失われていなければ、次の受理で取り出せる
            let received =
                tokio::time::timeout(Duration::from_secs(5), acceptor.accept_recv_stream())
                    .await
                    .expect("引き渡し済みの stream が失われていないこと")
                    .expect("受理がエラーにならないこと");
            assert!(received.is_some(), "受理した stream を取り出せること");
        })
        .await;
    }

    /// open 待ちを破棄しても、開いたストリームが閉じられること
    ///
    /// open の待ち手へ結果を渡せない場合、開いたストリームを閉じないとピアが広告した
    /// `MAX_STREAMS` の枠を消費したまま残る。ピア側でストリームが RESET_STREAM として
    /// 観測できることを確認する。
    ///
    /// この閉じる処理は ngtcp2 バックエンドのドライバが行うため、ngtcp2 ビルドでだけ検証する。
    /// 単一スレッドの runtime を使い、1 回目の poll ではドライバが動かないことを保証する。
    #[cfg(feature = "quic-ngtcp2")]
    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_open_closes_the_stream() {
        run_with_timeout(async {
            let (peer, _client, mut acceptor) = connect_pair_with_acceptor().await;

            // open 待ちを 1 回だけ poll してコマンドを送らせ、結果を受け取らずに破棄する。
            // ドライバは待ち手へ渡せないため、開いたストリームを閉じる
            let mut open = Box::pin(peer.handle.open_send_stream());
            assert!(
                poll_once(open.as_mut()).is_pending(),
                "結果を受け取る前の open 待ちは完了しないこと"
            );
            drop(open);

            // 相手側ではストリームが開かれ、すぐ終端される
            let mut stream =
                tokio::time::timeout(Duration::from_secs(5), acceptor.accept_recv_stream())
                    .await
                    .expect("開かれたストリームを受理できること")
                    .expect("受理がエラーにならないこと")
                    .expect("接続が閉じていないこと");
            loop {
                match tokio::time::timeout(Duration::from_secs(5), stream.receive_chunk())
                    .await
                    .expect("ストリームの終端を観測できること")
                    .expect("ストリームを読み出せること")
                {
                    // データは書いていないため通常は来ない
                    crate::transport::RecvChunk::Data(_) => continue,
                    crate::transport::RecvChunk::End(RequestStreamEnd::Reset {
                        error_code,
                        ..
                    }) => {
                        assert_eq!(
                            error_code,
                            Some(0),
                            "使わないまま閉じたストリームは NO_ERROR で reset されること"
                        );
                        return;
                    }
                    crate::transport::RecvChunk::End(end) => {
                        panic!("RESET_STREAM で閉じること: {end:?}")
                    }
                }
            }
        })
        .await;
    }

    /// 受信待ちが保留のまま破棄されても、受信済みの datagram を失わないこと
    ///
    /// datagram の受信待ちを 1 回だけ poll して待ち手として登録し、ドライバが datagram を
    /// 引き渡したあとに値を受け取らず破棄する。oneshot で直接渡していると、oneshot に
    /// 格納された datagram ごと破棄される。
    ///
    /// 待ち行列で受け渡すのは ngtcp2 バックエンドのため、ngtcp2 ビルドでだけ検証する。
    /// 単一スレッドの runtime を使い、1 回目の poll ではドライバが動かないことを保証する。
    #[cfg(feature = "quic-ngtcp2")]
    #[tokio::test(flavor = "current_thread")]
    async fn dropping_a_pending_datagram_receive_does_not_lose_the_datagram() {
        run_with_timeout(async {
            let (peer, client, _acceptor) = connect_pair_with_acceptor().await;
            let handle = client.handle();

            // 受信待ちを 1 回だけ poll して待ち手として登録し、保留のまま保持する
            let mut receive = Box::pin(handle.recv_datagrams());
            assert!(
                poll_once(receive.as_mut()).is_pending(),
                "datagram が無い間は受信待ちが完了しないこと"
            );

            // peer が datagram を送る。ドライバは登録済みの待ち手へ引き渡す
            peer.send_datagram(b"pending-datagram")
                .await
                .expect("datagram を送れること");

            // 引き渡しが起きるまで待ってから、値を受け取らずに受信待ちを破棄する
            tokio::time::sleep(Duration::from_millis(200)).await;
            drop(receive);

            // 失われていなければ、次の受信で取り出せる
            let datagrams = tokio::time::timeout(Duration::from_secs(5), handle.recv_datagrams())
                .await
                .expect("引き渡し済みの datagram が失われていないこと")
                .expect("datagram を受信できること");
            assert_eq!(
                datagrams,
                vec![b"pending-datagram".to_vec()],
                "引き渡し済みの datagram が取り出せること"
            );
        })
        .await;
    }

    /// データ受信側が積んだセッション終了イベントは、待機中の `next_event` を起こす
    #[tokio::test(flavor = "current_thread")]
    async fn data_stream_error_wakes_pending_next_event() {
        use std::future::poll_fn;
        use std::pin::pin;

        use shiguredo_moqt::session::types::DataStreamId;

        use crate::transport::RecvChunk;

        run_with_timeout(async {
            let (mut peer, mut client, mut acceptor) = connect_pair_with_acceptor().await;
            let data_plane = client.data_plane();

            // `next_event` が実際に Pending を返したあとでデータを送る。同じ Future を独立したタスクで
            // 待ち続けるため、peer やデータ受信側の I/O を待つタスクの起床では再 poll されない。
            let (waiting, wait_until_pending) = tokio::sync::oneshot::channel();
            let event_task = tokio::spawn(async move {
                let event = {
                    let mut event = pin!(client.next_event());
                    let mut waiting = Some(waiting);
                    poll_fn(|cx| {
                        let progress = event.as_mut().poll(cx);
                        if let Some(waiting) = waiting.take() {
                            assert!(progress.is_pending(), "最初は制御イベントがないこと");
                            waiting.send(()).expect("待機開始を通知できること");
                        }
                        progress
                    })
                    .await
                };
                // peer が終了コードを受け取る前に client を破棄しない。
                (event, client)
            });
            wait_until_pending.await.expect("待機開始を観測できること");

            // 未知の stream type (0x00) を実際の QUIC uni stream で受信し、データ側から Session に通知する。
            // 制御メッセージを追加したり、tick で待機中の処理を進めたりはしない。
            let mut send = peer
                .open_uni_send_stream()
                .await
                .expect("peer が単方向ストリームを開けること");
            send.send(Bytes::from_static(&[0x00]))
                .await
                .expect("peer が stream type を送れること");
            send.finish()
                .await
                .expect("peer がストリームを閉じられること");
            let mut recv = acceptor
                .accept_recv_stream()
                .await
                .expect("単方向ストリームを受理できること")
                .expect("単方向ストリームが届くこと");
            let RecvChunk::Data(data) = recv.receive_chunk().await.expect("先頭データを読めること")
            else {
                panic!("stream type がデータとして届くこと");
            };
            let (stream_type, _) = shiguredo_moqt::varint::decode(&data)
                .expect("stream type の可変長整数を読めること");
            data_plane
                .recv_data_stream_type(DataStreamId(recv.stream_id()), stream_type)
                .expect_err("未知の stream type を拒否すること");

            // 待機中の同じ `next_event` が起きてアプリへ通知し、peer にも終了コードが届く。
            let (event, _client) = event_task.await.expect("イベント待機タスクが終了すること");
            let event = event.expect("イベントを取得できること");
            let Some(ClientEvent::Session(SessionEvent::CloseSession(error))) = event else {
                panic!("データ側の違反によるセッション終了が届くこと: {event:?}");
            };
            assert_eq!(
                error.code, SESSION_INVALID_STREAM_HEADER,
                "終了コードが一致すること"
            );
            assert_eq!(
                peer.recv_close_code()
                    .await
                    .expect("peer が終了コードを受信できること"),
                SESSION_INVALID_STREAM_HEADER,
                "ワイヤ上の終了コードも一致すること",
            );
        })
        .await;
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

    /// client の main ループを error まで回す
    ///
    /// `pump_once` は Session の失敗を drain したうえで `TransportError::Internal` を返すため、
    /// その error を受け取るまで `next_event` を呼び続ける。
    async fn next_event_until_error(client: &mut MoqtClient) -> TransportError {
        loop {
            match client.next_event().await {
                Ok(_) => continue,
                Err(e) => return e,
            }
        }
    }

    /// 確立後に届いた 2 通目の SETUP で UNEXPECTED_MESSAGE の終了コードを送ること
    ///
    /// draft-ietf-moq-transport-23 §6.3 (Session initialization) は「各 peer は SETUP で始まる
    /// control stream を 1 本だけ開く」とし、§9.1 (SETUP) も SETUP を control stream の最初の
    /// メッセージとする。確立後に届いた 2 通目の SETUP は Session が UNEXPECTED_MESSAGE で
    /// 閉じる (§12.2 (Session Termination Codes))。`pump_once` は Session が積んだ
    /// `CloseSession` を drain してから error を返す。
    #[tokio::test(flavor = "multi_thread")]
    async fn established_duplicate_setup_closes_with_unexpected_message() {
        run_with_timeout(async {
            let (mut peer, mut client) = connect_pair().await;

            // 既存の control stream に 2 通目の SETUP を書く (stream type prefix は付けない)
            peer.send_control_message(ControlMessage::Setup(Setup {
                options: SetupOptions::new(),
            }))
            .await
            .expect("control stream を書けること");

            let error = next_event_until_error(&mut client).await;
            let message = match &error {
                TransportError::Internal(message) => message.clone(),
                other => panic!("TransportError::Internal を期待したが {other:?} だった"),
            };
            assert!(
                message.contains("recv_control"),
                "recv_control 経路の error であること: {message}"
            );
            assert!(
                message.contains("duplicate SETUP"),
                "duplicate SETUP の error であること: {message}"
            );
            assert_eq!(
                peer.recv_close_code()
                    .await
                    .expect("終了コードを観測できること"),
                SESSION_UNEXPECTED_MESSAGE,
                "duplicate SETUP は UNEXPECTED_MESSAGE (0x303) であること"
            );
        })
        .await;
    }

    /// 確立後の request stream で Session が失敗したときも終了コードを送ること
    ///
    /// draft-ietf-moq-transport-23 §9 Table 5 は "Messages marked \"First\" MUST be the first
    /// message on a new request stream." と定める。確立後の request stream に 2 通目の
    /// SUBSCRIBE が届くと Session は UNEXPECTED_MESSAGE で閉じる。`pump_once` は
    /// `recv_stream_message` の失敗で `CloseSession` を drain してから error を返す。
    #[tokio::test(flavor = "multi_thread")]
    async fn established_request_stream_error_closes_with_unexpected_message() {
        run_with_timeout(async {
            let (mut peer, mut client) = connect_pair().await;

            // peer が request stream を開き、SUBSCRIBE を先頭メッセージとして送る
            let request_id = peer.next_request_id().expect("Request ID を採番できること");
            peer.send_request_start(ControlMessage::Subscribe(Subscribe {
                request_id,
                track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("名前空間を作れること"),
                track_name: b"video".to_vec(),
                parameters: MessageParameters::new(),
            }))
            .await
            .expect("request stream を開けること");

            // アプリへ要求が渡るまで main ループを回す
            loop {
                match client.next_event().await.expect("イベントを取得できること") {
                    Some(ClientEvent::Request(request)) => {
                        assert_eq!(request.request_id, request_id, "Request ID が一致すること");
                        break;
                    }
                    Some(_) => continue,
                    None => panic!("request が配送されること"),
                }
            }

            // 同じ request stream に 2 通目の SUBSCRIBE を書く (Table 5 の "First" 違反)
            let second_request_id = peer.next_request_id().expect("Request ID を採番できること");
            peer.send_opened_request_message(
                request_id,
                ControlMessage::Subscribe(Subscribe {
                    request_id: second_request_id,
                    track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                        .expect("名前空間を作れること"),
                    track_name: b"video".to_vec(),
                    parameters: MessageParameters::new(),
                }),
            )
            .await
            .expect("request stream に書けること");

            let error = next_event_until_error(&mut client).await;
            let message = match &error {
                TransportError::Internal(message) => message.clone(),
                other => panic!("TransportError::Internal を期待したが {other:?} だった"),
            };
            assert!(
                message.contains("recv_stream_message"),
                "recv_stream_message 経路の error であること: {message}"
            );
            assert_eq!(
                peer.recv_close_code()
                    .await
                    .expect("終了コードを観測できること"),
                SESSION_UNEXPECTED_MESSAGE,
                "request stream の \"First\" 違反は UNEXPECTED_MESSAGE (0x303) であること"
            );
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
    /// `NEW_GROUP_REQUEST` (draft-ietf-moq-transport-23 §9.19.19) に応じて新しい Group を
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

            // example は UNEXPECTED_MESSAGE でセッションを閉じる
            let event = client.next_event().await.expect("イベントを取得できること");
            match event {
                Some(ClientEvent::Session(SessionEvent::CloseSession(error))) => {
                    assert_eq!(
                        error.code, SESSION_UNEXPECTED_MESSAGE,
                        "UNEXPECTED_MESSAGE で閉じること"
                    );
                }
                other => panic!("CloseSession が届くこと: {other:?}"),
            }
        })
        .await;
    }

    /// peer が送った namespace 系 request はアプリへ渡される
    ///
    /// draft-ietf-moq-transport-23 §6.3 (Session initialization): SUBSCRIBE_NAMESPACE などの
    /// namespace 系 request も bidi request stream の先頭に置ける。I/O 層が拒否せず、
    /// アプリが応答できるように `ClientEvent::Request` で配送することを確認する。
    #[tokio::test(flavor = "multi_thread")]
    async fn namespace_request_start_message_is_delivered_to_application() {
        run_with_timeout(async {
            let (mut peer, client) = connect_pair().await;

            let request_id = peer.next_request_id().expect("Request ID を採番できること");
            let message = ControlMessage::SubscribeNamespace(SubscribeNamespace {
                request_id,
                track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("名前空間を作れること"),
                parameters: MessageParameters::new(),
            });
            peer.send_request_start(message)
                .await
                .expect("request stream を開けること");

            // アプリへ要求が渡る
            let mut client = client;
            match client.next_event().await.expect("イベントを取得できること") {
                Some(ClientEvent::Request(request)) => {
                    assert_eq!(request.request_id, request_id, "request がアプリへ渡ること");
                    assert_eq!(
                        request.message,
                        ControlMessage::SubscribeNamespace(SubscribeNamespace {
                            request_id,
                            track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                                .expect("名前空間を作れること"),
                            parameters: MessageParameters::new(),
                        }),
                        "受信したメッセージが元の SUBSCRIBE_NAMESPACE と一致すること"
                    );
                }
                other => panic!("Request が届くこと: {other:?}"),
            }

            peer.finish_request_stream(request_id)
                .await
                .expect("request stream を終端できること");
        })
        .await;
    }

    /// peer が parity 違反の request を先頭に送るとセッションが終了する
    ///
    /// 本実装が扱う 4 種類の request でも Session がエラーを返す場合は、終了コード付きで
    /// セッションを閉じることを確かめる (draft-ietf-moq-transport-23 §6.4.2.1 (Request ID))。
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
    /// 表現できない不正な Setup Options は、control stream の prefix と SETUP の message type
    /// (どちらも 0x2F00) + vi64 長 + body を生バイト列で書き出す。body は
    /// draft-ietf-moq-transport-23 §9.1 (SETUP) の「カウントプレフィックスなしのデルタ
    /// エンコードされた KVP」である。
    fn setup_head(body: Vec<u8>) -> Vec<u8> {
        let mut buf = Vec::new();
        // control stream の stream type prefix
        shiguredo_moqt::varint::encode(shiguredo_moqt::stream::SETUP_STREAM_TYPE, &mut buf);
        // 先頭メッセージ (SETUP) の message type
        shiguredo_moqt::varint::encode(shiguredo_moqt::stream::SETUP_STREAM_TYPE, &mut buf);
        shiguredo_moqt::varint::encode(body.len() as u64, &mut buf);
        buf.extend_from_slice(&body);
        buf
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

        // 接続と受理は並行に進める。tobi-quic のサーバーは `accept()` の poll で
        // ハンドシェイクを進めるため、client の `connect()` を先に待つと停止する
        let (connection, peer) = tokio::join!(
            crate::quic::connect(addr, "localhost", None),
            server.accept(),
        );
        let (connection, stop_sending_rx) = connection.expect("接続できること");
        let peer = peer.expect("接続を受理できること");

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

    /// 初回 SETUP のトークン構造の decode 失敗で MALFORMED_AUTH_TOKEN を送ること
    ///
    /// draft-ietf-moq-transport-23 §8.9 (Authorization Token Compression) は Token 構造を
    /// decode できない場合に MALFORMED_AUTH_TOKEN で閉じることを MUST で要求する。
    /// AUTHORIZATION_TOKEN (0x03) の値に未知の Alias Type (0x04) を置くと
    /// `AuthorizationToken::decode` が `MalformedAuthToken` を返す。
    #[tokio::test(flavor = "multi_thread")]
    async fn initial_setup_with_unknown_token_alias_closes_with_malformed_auth_token() {
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
                SESSION_MALFORMED_AUTH_TOKEN,
                "トークン構造の不正は MALFORMED_AUTH_TOKEN (0x16) であること"
            );
        })
        .await;
    }

    /// 初回 SETUP の Setup Options で KVP の宣言長が残りを超えると
    /// MALFORMED_KEY_VALUE を送ること
    ///
    /// draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure): "If a receiver understands a
    /// Type, and the following Value or Length/Value does not match the serialization defined by
    /// that Type, the receiver MUST close the session with error code
    /// MALFORMED_KEY_VALUE." Type を理解している (option type 0x03) が、宣言長 10 に対して
    /// 値が 3 バイトしかない。
    #[tokio::test(flavor = "multi_thread")]
    async fn initial_setup_with_kvp_length_exceeding_options_closes_with_key_value_formatting_error()
     {
        run_with_timeout(async {
            let (mut peer, establish) = connect_handshake().await;

            // Setup Options: option type 0x03 (delta 3)、値長 10、値は 3 バイトしかない
            peer.send_control_stream_head(setup_head(vec![0x03, 0x0A, 0x01, 0x02, 0x03]))
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
                SESSION_MALFORMED_KEY_VALUE,
                "KVP の宣言長超過は MALFORMED_KEY_VALUE (0x6) であること"
            );
        })
        .await;
    }

    /// 初回 SETUP のトークン重複で MALFORMED_AUTH_TOKEN を送ること
    ///
    /// draft-ietf-moq-transport-23 §8.9 (Authorization Token Compression) は Authorization Token の
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
    /// draft-ietf-moq-transport-23 §9.1.2 (PATH) は server が PATH を送ることを MUST NOT と
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

    /// peer の MAX_CONTROL_MESSAGE_SIZE が 65535 以上なら SETUP 交換が成立すること
    ///
    /// draft-ietf-moq-transport-23 §9.1.8 (MAX_CONTROL_MESSAGE_SIZE): 値は varint で、
    /// 65535 以上でなければならない。wire 上の Setup Option を client が decode して受理し、
    /// ハンドシェイクを完了することを通しで固定する。
    #[tokio::test(flavor = "multi_thread")]
    async fn initial_setup_with_max_control_message_size_establishes() {
        run_with_timeout(async {
            let (mut peer, establish) = connect_handshake().await;

            let mut options = SetupOptions::new();
            options.push(SetupOption {
                option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_CONTROL_MESSAGE_SIZE,
                value: SetupOptionValue::VarInt(1_000_000),
            });
            peer.send_setup(&options).await.expect("SETUP を送れること");

            let _client = establish
                .await
                .expect("establish タスクが終了すること")
                .expect("MAX_CONTROL_MESSAGE_SIZE を持つ SETUP で確立できること");
        })
        .await;
    }

    /// peer の MAX_CONTROL_MESSAGE_SIZE が 65535 未満なら INVALID_FIELD を送ること
    ///
    /// draft-ietf-moq-transport-23 §9.1.8 (MAX_CONTROL_MESSAGE_SIZE) は 65535 以上を MUST と
    /// する。値が許容範囲外の制御メッセージフィールドに該当するため、§12.2 (Session
    /// Termination Codes) の INVALID_FIELD (0x301) で閉じる。`SetupOptions::encode` は
    /// 最小値を検証するため、生の Setup Options を書いて再現する。
    #[tokio::test(flavor = "multi_thread")]
    async fn initial_setup_with_small_max_control_message_size_closes_with_invalid_field() {
        run_with_timeout(async {
            let (mut peer, establish) = connect_handshake().await;

            // delta type = 0x09、Length = 3、値 = 65534 (最小値の 1 バイト下)
            let mut body =
                vec![shiguredo_moqt::parameter::SETUP_OPTION_MAX_CONTROL_MESSAGE_SIZE as u8];
            body.push(0x03); // 65534 の vi64 は 3 バイト
            shiguredo_moqt::varint::encode(65_534, &mut body);
            peer.send_control_stream_head(setup_head(body))
                .await
                .expect("control stream を書けること");

            establish
                .await
                .expect("establish タスクが終了すること")
                .err()
                .expect("最小値未満の MAX_CONTROL_MESSAGE_SIZE で確立が失敗すること");
            assert_eq!(
                peer.recv_close_code()
                    .await
                    .expect("終了コードを観測できること"),
                SESSION_INVALID_FIELD,
                "最小値未満は INVALID_FIELD (0x301) であること"
            );
        })
        .await;
    }

    /// 受信上限を超える制御メッセージの宣言は、本文を待たず FIELD_LENGTH_EXCEEDED になること
    ///
    /// draft-ietf-moq-transport-23 §9 (Control Messages): 自側の MAX_CONTROL_MESSAGE_SIZE を
    /// 超える Length の宣言は FIELD_LENGTH_EXCEEDED で閉じる (MUST)。client は既定値 65535 を
    /// 受信上限に使うため、65536 を宣言した SETUP のヘッダだけを送り、本文を送らなくても
    /// セッションが閉じることを固定する。
    #[tokio::test(flavor = "multi_thread")]
    async fn oversized_control_message_declaration_closes_with_field_length_exceeded() {
        run_with_timeout(async {
            let (mut peer, establish) = connect_handshake().await;

            // stream type prefix (control stream) と SETUP の Type / Length のみを書く
            let mut head = Vec::new();
            shiguredo_moqt::varint::encode(shiguredo_moqt::stream::SETUP_STREAM_TYPE, &mut head);
            shiguredo_moqt::varint::encode(0x2F00, &mut head);
            shiguredo_moqt::varint::encode(65_536, &mut head);
            peer.send_control_stream_head(head)
                .await
                .expect("control stream を書けること");

            let error = establish
                .await
                .expect("establish タスクが終了すること")
                .err()
                .expect("上限超過の宣言で確立が失敗すること");
            assert!(
                matches!(
                    error,
                    TransportError::Moqt(shiguredo_moqt::error::MessageError::FieldLengthExceeded(
                        _
                    ))
                ),
                "FieldLengthExceeded として返ること: {error}"
            );
            assert_eq!(
                peer.recv_close_code()
                    .await
                    .expect("終了コードを観測できること"),
                SESSION_FIELD_LENGTH_EXCEEDED,
                "上限超過は FIELD_LENGTH_EXCEEDED (0x401) であること"
            );
        })
        .await;
    }

    /// control stream の stream type が不一致だと INVALID_STREAM_HEADER を送ること
    ///
    /// draft-ietf-moq-transport-23 §6.4.1 (Unidirectional Streams) は control stream の先頭を
    /// `SETUP_STREAM_TYPE` (0x2F00) に限り、未知の stream type の受信でセッションを閉じる。
    /// 終了コードは §12.2 (Session Termination Codes) の INVALID_STREAM_HEADER (0x200) である。
    #[tokio::test(flavor = "multi_thread")]
    async fn control_stream_with_wrong_stream_type_closes_with_invalid_stream_header() {
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
                SESSION_INVALID_STREAM_HEADER,
                "stream type の不一致は INVALID_STREAM_HEADER (0x200) であること"
            );
        })
        .await;
    }

    /// SETUP の前に control stream が終端すると CLOSED_CONTROL_STREAM を送ること
    ///
    /// draft-ietf-moq-transport-23 §6.3 (Session initialization) は control stream を
    /// セッション中に閉じることを禁じる。
    #[tokio::test(flavor = "multi_thread")]
    async fn control_stream_finished_before_setup_closes_with_closed_control_stream() {
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
                SESSION_CLOSED_CONTROL_STREAM,
                "SETUP 前の終端は CLOSED_CONTROL_STREAM (0x306) であること"
            );
        })
        .await;
    }

    /// 初回 SETUP の decode 失敗で写像が ROLE_VIOLATION を選ぶこと
    ///
    /// draft-ietf-moq-transport-23 §9.1.4 (AUTHORIZATION TOKEN) は SETUP の Token に DELETE /
    /// USE_ALIAS を許さず、受信側は ROLE_VIOLATION でセッションを閉じる (原文の MUST は
    /// server が受信する場合について述べているが、library は役割によらず同じ検証をする)。
    /// SETUP の decode 失敗なので写像 (`session_error_code`) を通り、
    /// §12.2 (Session Termination Codes) の ROLE_VIOLATION (0x304) で閉じる。
    #[tokio::test(flavor = "multi_thread")]
    async fn initial_setup_with_delete_token_closes_with_role_violation() {
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
                SESSION_ROLE_VIOLATION,
                "DELETE トークンは ROLE_VIOLATION (0x304) であること"
            );
        })
        .await;
    }

    /// stream type の直後に control stream が終端すると CLOSED_CONTROL_STREAM を送ること
    ///
    /// stream type は正当だが SETUP を送らずに終端する場合であり、`finish_handshake` の
    /// 「control stream closed before SETUP」経路を通る (stream type を読む前に終端する
    /// [`control_stream_finished_before_setup_closes_with_protocol_violation`] とは別の経路)。
    /// draft-ietf-moq-transport-23 §6.3 (Session initialization) は control stream を
    /// セッション中に閉じることを禁じ、§12.2 (Session Termination Codes) の
    /// CLOSED_CONTROL_STREAM (0x306) で閉じる。
    #[tokio::test(flavor = "multi_thread")]
    async fn control_stream_finished_after_stream_type_closes_with_closed_control_stream() {
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
                SESSION_CLOSED_CONTROL_STREAM,
                "SETUP 前の終端は CLOSED_CONTROL_STREAM (0x306) であること"
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
                        happy_eyeballs: happy_eyeballs::Config::default(),
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
            peer.send_subscribe_ok(
                subscribe_request_id,
                11,
                MessageParameters::new(),
                TrackProperties::new(),
            )
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
                        url: &target.url,
                        transport: crate::Transport::Quic,
                        cert: None,
                        impl_name: "test-peer",
                        receive_datagrams: false,
                        // QMux 経路は使わない (QUIC 直接接続の試験基盤)
                        datagram_max_size: None,
                        happy_eyeballs: happy_eyeballs::Config::default(),
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
                        happy_eyeballs: happy_eyeballs::Config::default(),
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
                        .send_subscribe_ok(
                            request_id,
                            5,
                            MessageParameters::new(),
                            TrackProperties::new(),
                        )
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
            assert_eq!(target.scope, crate::MigrationScope::Session);
            assert_eq!(
                target.url.authority,
                format!("127.0.0.1:{}", addr_b.port()),
                "GOAWAY の URI が移行先になること"
            );
            let target = target.url;

            // 移行先へ接続して購読を張り直す。実際の example は期限付きの再試行経路で
            // 接続するため、テストでも同じ connect_with_retry を使う
            let target_monitor = task_monitor.clone();
            let deadline = crate::MigrationDeadline::after_goaway(Instant::now(), 5000);
            let connect_b = tokio::spawn(async move {
                crate::connect_with_retry(
                    &crate::ConnectParams {
                        url: &target,
                        transport: crate::Transport::Quic,
                        cert: None,
                        impl_name: "test-peer",
                        receive_datagrams: false,
                        // QMux 経路は使わない (QUIC 直接接続の試験基盤)
                        datagram_max_size: None,
                        happy_eyeballs: happy_eyeballs::Config::default(),
                    },
                    deadline,
                    crate::RetryPolicy::default(),
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
                .send_subscribe_ok(
                    request_id,
                    9,
                    MessageParameters::new(),
                    TrackProperties::new(),
                )
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
                    fetch_ok_parameters(Some(Location {
                        group_id: 3,
                        object_id: 1,
                    })),
                    TrackProperties::new(),
                )
                .await
                .expect("FETCH_OK を返せること");
            let mut writer = client
                .open_fetch_response(request_id)
                .await
                .expect("FETCH 応答 stream を開けること");
            // Properties Length = 0 を含む生バイト列 (draft-ietf-moq-transport-23 §11.1.2)
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

    /// peer から届いた FETCH に、End of Group / End of Track を書いた応答を返す
    ///
    /// publisher 側の writer が marker を送出できること
    /// (draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields)) と、peer 側の
    /// [`FetchStreamDecoder`] が marker の前後の Group を解決することを、実際の uni stream で
    /// 固定する。marker が後続 ID フィールドを書かないため、次の entry が marker の直後から
    /// 読めることも確認する。
    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_response_writer_writes_end_of_group_and_track() {
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
            match client.next_event().await.expect("イベントを取得できること") {
                Some(ClientEvent::Request(request)) => assert!(
                    matches!(request.message, ControlMessage::Fetch(_)),
                    "FETCH として受理されること"
                ),
                other => panic!("FETCH がアプリへ届くこと: {other:?}"),
            }

            // LARGEST_OBJECT は marker が示す最終 Object に合わせる
            client
                .send_fetch_ok(
                    request_id,
                    fetch_ok_parameters(Some(Location {
                        group_id: 2,
                        object_id: 0,
                    })),
                    TrackProperties::new(),
                )
                .await
                .expect("FETCH_OK を返せること");
            let mut writer = client
                .open_fetch_response(request_id)
                .await
                .expect("FETCH 応答 stream を開けること");
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
                        payload_length: 2,
                    },
                    b"ab",
                    Some(&properties),
                )
                .await
                .expect("1 件目の Object を書けること");
            // 1 件目の Object が Group 1 の最終 Object であることを示す
            writer
                .write_end_of_group()
                .await
                .expect("End of Group を書けること");
            writer
                .write_object(
                    &FetchObjectInput {
                        group_id: 2,
                        subgroup_id: 0,
                        object_id: 0,
                        publisher_priority: 128,
                        has_properties: true,
                        is_datagram_origin: false,
                        payload_length: 2,
                    },
                    b"cd",
                    Some(&properties),
                )
                .await
                .expect("2 件目の Object を書けること");
            // 2 件目の Object が Track の最終 Object であることを示す
            writer
                .write_end_of_track()
                .await
                .expect("End of Track を書けること");
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
            assert_eq!(
                decoded.len(),
                4,
                "Object / End of Group / Object / End of Track の 4 件"
            );
            match (&decoded[0].0, &decoded[0].1) {
                (DecodedFetchEntry::Object(object), payload) => {
                    assert_eq!(object.group_id, 1);
                    assert_eq!(object.object_id, 0);
                    assert_eq!(payload.as_slice(), b"ab", "1 件目の payload");
                }
                other => panic!("1 件目が Object であること: {other:?}"),
            }
            match &decoded[1].0 {
                DecodedFetchEntry::EndOfGroup { group_id } => {
                    assert_eq!(*group_id, 1, "Group 1 の終端であること");
                }
                other => panic!("2 件目が End of Group であること: {other:?}"),
            }
            match (&decoded[2].0, &decoded[2].1) {
                (DecodedFetchEntry::Object(object), payload) => {
                    assert_eq!(object.group_id, 2, "marker の次の Group が復元されること");
                    assert_eq!(object.object_id, 0);
                    assert_eq!(payload.as_slice(), b"cd", "2 件目の payload");
                }
                other => panic!("3 件目が Object であること: {other:?}"),
            }
            match &decoded[3].0 {
                DecodedFetchEntry::EndOfTrack { group_id } => {
                    assert_eq!(*group_id, 2, "Group 2 で Track が終端されること");
                }
                other => panic!("4 件目が End of Track であること: {other:?}"),
            }
            decoder
                .finish()
                .expect("entry 境界で終端した応答は finish で受容されること");
        })
        .await;
    }

    /// peer から届いた FETCH に、Object と End of Gone Range を 1 本の応答で返す
    ///
    /// publisher 側の writer が End of Gone Range (draft-ietf-moq-transport-23 §11.4.1
    /// Table 8 の 0x40C) を送出できることと、peer 側の [`FetchStreamDecoder`] が Gone を
    /// 通常の Object と同じストリームで読めることを、実際の uni stream で固定する。
    /// Gone は存在しないと確定した Non-Existent とは別の状態であるため
    /// (同 §2.1.2 (Object States))、復号結果が
    /// [`DecodedFetchEntry::EndOfGoneRange`] になることを確認する。
    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_response_writer_writes_end_of_gone_range() {
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
            match client.next_event().await.expect("イベントを取得できること") {
                Some(ClientEvent::Request(request)) => assert!(
                    matches!(request.message, ControlMessage::Fetch(_)),
                    "FETCH として受理されること"
                ),
                other => panic!("FETCH がアプリへ届くこと: {other:?}"),
            }

            client
                .send_fetch_ok(
                    request_id,
                    fetch_ok_parameters(Some(Location {
                        group_id: 3,
                        object_id: 6,
                    })),
                    TrackProperties::new(),
                )
                .await
                .expect("FETCH_OK を返せること");
            let mut writer = client
                .open_fetch_response(request_id)
                .await
                .expect("FETCH 応答 stream を開けること");
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
                        payload_length: 2,
                    },
                    b"ab",
                    Some(&properties),
                )
                .await
                .expect("1 件目の Object を書けること");
            // 元の publisher が再配送できない範囲を End of Gone Range で示す
            writer
                .write_end_of_gone_range(Location {
                    group_id: 3,
                    object_id: 5,
                })
                .await
                .expect("End of Gone Range を書けること");
            writer
                .write_object(
                    &FetchObjectInput {
                        group_id: 3,
                        subgroup_id: 0,
                        object_id: 6,
                        publisher_priority: 128,
                        has_properties: true,
                        is_datagram_origin: false,
                        payload_length: 2,
                    },
                    b"cd",
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
            // End of Gone Range の Serialization Flags (0x40C) が wire に載ること
            assert!(
                bytes.windows(2).any(|window| window == [0x84, 0x0C]),
                "End of Gone Range の Serialization Flags (0x40C) が送出されること: {bytes:?}"
            );
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
            assert_eq!(
                decoded.len(),
                3,
                "Object / End of Gone Range / Object の 3 件"
            );
            match (&decoded[0].0, &decoded[0].1) {
                (DecodedFetchEntry::Object(object), payload) => {
                    assert_eq!(object.group_id, 1);
                    assert_eq!(object.object_id, 0);
                    assert_eq!(payload.as_slice(), b"ab", "1 件目の payload");
                }
                other => panic!("1 件目が Object であること: {other:?}"),
            }
            match &decoded[1].0 {
                DecodedFetchEntry::EndOfGoneRange {
                    group_id,
                    object_id,
                } => {
                    assert_eq!(*group_id, 3, "Gone の終端 Group が解決されること");
                    assert_eq!(*object_id, 5, "Gone の終端 Object が解決されること");
                }
                other => panic!("2 件目が End of Gone Range であること: {other:?}"),
            }
            match (&decoded[2].0, &decoded[2].1) {
                (DecodedFetchEntry::Object(object), payload) => {
                    assert_eq!(object.group_id, 3);
                    assert_eq!(object.object_id, 6, "Gone の直後の Object が受理されること");
                    assert_eq!(payload.as_slice(), b"cd", "2 件目の payload");
                }
                other => panic!("3 件目が Object であること: {other:?}"),
            }
            decoder
                .finish()
                .expect("entry 境界で終端した応答は finish で受容されること");
        })
        .await;
    }

    /// client が発行した FETCH の REQUEST_ERROR がアプリへ届く
    ///
    /// `MoqtClient::fetch` の拒否は `next_event` の `SessionEvent::RequestErrorReceived` でしか
    /// 観測できない。Retry Interval と Error Details (REDIRECT を含む) もこのイベントにだけ載り、
    /// アプリの再試行や代替の判断材料になる (draft-ietf-moq-transport-23 §9.4 (REQUEST_ERROR))。
    #[tokio::test(flavor = "multi_thread")]
    async fn client_fetch_request_error_is_delivered_to_the_application() {
        use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
        use shiguredo_moqt::message_parameter::MessageParameters;

        run_with_timeout(async {
            let (mut peer, mut client, _acceptor) = connect_pair_with_acceptor().await;

            let namespace =
                TrackNamespace::new(vec![b"live".to_vec()]).expect("名前空間を作れること");
            let request_id = client
                .fetch(namespace, b"video".to_vec(), MessageParameters::new())
                .await
                .expect("FETCH を発行できること");
            peer.recv_request_start()
                .await
                .expect("FETCH を受理できること");

            // peer が DOES_NOT_EXIST と Retry Interval 付きで拒否する
            peer.send_request_error(request_id, REQUEST_DOES_NOT_EXIST, 1500, "unknown track")
                .await
                .expect("REQUEST_ERROR を返せること");

            let event = client.next_event().await.expect("イベントを取得できること");
            match event {
                Some(ClientEvent::Session(SessionEvent::RequestErrorReceived {
                    request_id: received_request_id,
                    error_code,
                    retry_interval,
                    reason,
                    error_details,
                })) => {
                    assert_eq!(received_request_id, request_id, "Request ID が一致すること");
                    assert_eq!(
                        error_code, REQUEST_DOES_NOT_EXIST,
                        "peer が送ったエラーコードが届くこと"
                    );
                    assert_eq!(
                        retry_interval, 1500,
                        "peer が送った Retry Interval が届くこと"
                    );
                    assert_eq!(
                        reason.as_str(),
                        "unknown track",
                        "peer が送った理由句が届くこと"
                    );
                    assert!(
                        error_details.redirect().is_none(),
                        "REDIRECT でない拒否は移動先を持たないこと"
                    );
                }
                other => panic!("FETCH の REQUEST_ERROR が届くこと: {other:?}"),
            }
        })
        .await;
    }

    /// SUBSCRIBE の API とイベント配送が、同じ拒否理由を失わずに返すことを確認する。
    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_rejection_is_returned_and_remains_queued() {
        use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;

        run_with_timeout(async {
            let (mut peer, mut client) = connect_pair().await;
            let namespace =
                TrackNamespace::new(vec![b"live".to_vec()]).expect("名前空間を作れること");
            let reason = "unknown requested track";
            let (result, request_id) = tokio::join!(
                client.subscribe_track(namespace, b"video".to_vec()),
                async {
                    let message = peer
                        .recv_request_start()
                        .await
                        .expect("SUBSCRIBE を受理できること");
                    let ControlMessage::Subscribe(request) = message else {
                        panic!("受信した要求が SUBSCRIBE であること: {message:?}");
                    };
                    let request_id = request.request_id;
                    peer.send_request_error(request_id, REQUEST_DOES_NOT_EXIST, 1500, reason)
                        .await
                        .expect("要求を拒否できること");
                    request_id
                },
            );
            let Err(error) = result else {
                panic!("購読失敗を API が返すこと");
            };
            assert!(error.to_string().contains(reason), "拒否理由を API が返すこと: {error}");
            let event = client.next_event().await.expect("拒否イベントを読めること");
            assert!(
                matches!(event, Some(ClientEvent::Session(SessionEvent::RequestErrorReceived {
                    request_id: rejected_id, error_code: REQUEST_DOES_NOT_EXIST, retry_interval: 1500,
                    reason: details, ..
                })) if rejected_id == request_id && details.as_str() == reason),
                "同じ拒否イベントを理由と再試行間隔付きで取得できること",
            );
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
            peer.send_fetch_ok(request_id, Some(end_location))
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
                    ..
                })) => {
                    assert_eq!(rid, request_id, "FETCH_OK の Request ID");
                    assert_eq!(
                        location, end_location,
                        "FETCH_OK の応答範囲の終端が LARGEST_OBJECT と一致すること"
                    );
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
    /// draft-ietf-moq-transport-23 §3.2 (Fetch) の MUST 2 条件 (Object が publish されていない /
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

    /// 応答を返さない peer でも control message timeout が発火して購読待ちが戻ること
    ///
    /// 受信待ちで `Session::tick` が止まると、期限が来ても購読待ちが返らない。待ち経路でも
    /// 時刻を進めることを固定する。
    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_wait_times_out_without_peer_response() {
        run_with_timeout(async {
            let (_peer, mut client) = connect_pair().await;
            client.set_control_message_timeout_ms(Some(100));
            let namespace =
                TrackNamespace::new(vec![b"live".to_vec()]).expect("名前空間を作れること");
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                client.subscribe_track(namespace, b"video".to_vec()),
            )
            .await
            .expect("control message timeout で 2 秒以内に購読待ちが戻ること");
            assert!(result.is_err(), "応答の無い購読は失敗すること");
        })
        .await;
    }

    /// 移行先への接続再試行が期限で打ち切られること
    ///
    /// 到達不能な移行先に対して無限に再試行しないことを固定する。
    #[tokio::test(flavor = "multi_thread")]
    async fn connect_with_retry_stops_at_the_migration_deadline() {
        run_with_timeout(async {
            // 未使用の UDP ポートを選ぶ (QUIC の接続先として到達不能にする)
            let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("UDP を bind できること");
            let port = socket
                .local_addr()
                .expect("ローカルアドレスを取得できること")
                .port();
            drop(socket);

            let url = crate::parse_url(&format!("moqt://127.0.0.1:{port}/"))
                .expect("URL をパースできること");
            let deadline = crate::MigrationDeadline::after_goaway(Instant::now(), 300);
            let policy = crate::RetryPolicy {
                initial_delay: Duration::from_millis(20),
                max_delay: Duration::from_millis(50),
            };
            let task_monitor = tokio_metrics::TaskMonitor::new();
            let started = Instant::now();
            let result = crate::connect_with_retry(
                &crate::ConnectParams {
                    url: &url,
                    transport: crate::Transport::Quic,
                    cert: None,
                    impl_name: "test-peer",
                    receive_datagrams: false,
                    datagram_max_size: None,
                    happy_eyeballs: happy_eyeballs::Config::default(),
                },
                deadline,
                policy,
                &task_monitor,
            )
            .await;
            assert!(result.is_err(), "到達不能な移行先への接続は失敗すること");
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "期限で打ち切ること: {:?}",
                started.elapsed()
            );
        })
        .await;
    }
}
