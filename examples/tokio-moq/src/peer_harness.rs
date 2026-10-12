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
//! 対象は QUIC 直接接続 (`MoqtClient::establish_quic`) である。WebTransport over HTTP/3 /
//! HTTP/2 のハーネスは必要になった時点で別途追加する。
//!
//! 提供するのは SETUP 交換、peer 起点 request stream へのメッセージ送信、client 起点 request
//! stream の受理と REQUEST_OK による応答である。次は提供しない (必要になった時点で拡張する)。
//!
//! - peer が開いた request stream に届く応答の読み出し
//! - client が送る終了コード (CONNECTION_CLOSE) の観測
//! - SETUP の内容を差し替えて送る経路 (`Session::new_server` が積んだ SETUP を送るだけである)

use std::net::SocketAddr;

use bytes::Bytes;

use shiguredo_moqt::message::ControlMessage;
use shiguredo_moqt::message_parameter::MessageParameters;
use shiguredo_moqt::parameter::SetupOptions;
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::{SessionEvent, SessionState, Transport as MoqtTransport};
use shiguredo_moqt::stream::encode_control_stream_setup;
use shiguredo_moqt::track_properties::TrackProperties;

use crate::error::{Result, TransportError};
use crate::moqt_client::{ControlStream, StreamRead};
use crate::transport::{BidiStreamAcceptor, RecvStream, SendStream, StreamAcceptor, StreamHandle};

// 以下の 3 つは `moqt_client` の同名 private 関数と同じ処理である。テスト専用の都合で
// 通常ビルド対象の可視性を広げないため (`shiguredo-rust` の「テストのために private な API を
// 無理矢理公開しないこと」)、あえて重複させている。

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

/// request の開始メッセージから Request ID を取り出す
///
/// draft-ietf-moq-transport-22 §6.3 (Session initialization) は request stream の先頭に
/// 7 種類のメッセージを許すが、本 example が request として扱うのは SUBSCRIBE / PUBLISH /
/// FETCH / TRACK_STATUS の 4 種類である (残り 3 種類は `ControlMessage::Unsupported`)。
fn request_id_of(message: &ControlMessage) -> Option<u64> {
    match message {
        ControlMessage::Subscribe(m) => Some(m.request_id),
        ControlMessage::Publish(m) => Some(m.request_id),
        ControlMessage::Fetch(m) => Some(m.request_id),
        ControlMessage::TrackStatus(m) => Some(m.request_id),
        _ => None,
    }
}

/// client の接続を受理する前の in-process peer (s2n-quic サーバー)
pub(crate) struct TestPeerServer {
    /// 接続を受理する s2n-quic サーバー
    server: s2n_quic::Server,
    /// 待ち受けているアドレス
    local_addr: SocketAddr,
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

        let server = s2n_quic::Server::builder()
            .with_tls(server_tls)
            .map_err(|e| TransportError::Internal(format!("failed to set TLS: {e}")))?
            .with_io(server_io)
            .map_err(|e| TransportError::Internal(format!("failed to set I/O: {e}")))?
            .start()
            .map_err(|e| TransportError::Internal(format!("failed to start server: {e}")))?;

        Ok(Self { server, local_addr })
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
        TestPeer::new(connection)
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
    /// 送信半の drop は FIN、受信半の drop は STOP_SENDING として peer に観測されるため保持する。
    opened_requests: Vec<(SendStream, RecvStream)>,
    /// peer から受理した request stream
    accepted_requests: Vec<AcceptedRequest>,
}

impl TestPeer {
    /// 受理した QUIC 接続から peer を作る
    fn new(connection: s2n_quic::connection::Connection) -> Result<Self> {
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
        self.opened_requests.push((send, recv));
        Ok(())
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
        let request_id = request_id_of(&message).ok_or_else(|| {
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
            request.send.finish()?;
        }
        Ok(())
    }

    /// 自側の次の Request ID を採番する (server 役は奇数)
    pub(crate) fn next_request_id(&mut self) -> Result<u64> {
        Ok(self.session.next_local_request_id()?)
    }

    /// サーバー側 session の状態
    pub(crate) fn session_state(&self) -> SessionState {
        self.session.state()
    }
}

mod tests {
    use std::future::Future;
    use std::time::Duration;

    use shiguredo_moqt::message::Subscribe;
    use shiguredo_moqt::message::common::TrackNamespace;

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

    /// peer と client を接続し、SETUP 交換まで進める
    ///
    /// client の接続とハンドシェイクは peer のハンドシェイクと並行して進める (どちらも相手の
    /// SETUP を待つため、直列に await すると停止する)。
    async fn connect_pair() -> (TestPeer, MoqtClient) {
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
        let (client, _acceptor) = establish
            .await
            .expect("establish タスクが終了すること")
            .expect("client を確立できること");
        assert_eq!(
            peer.session_state(),
            SessionState::Established,
            "peer 側 session が Established になること"
        );
        (peer, client)
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
                client.publish_track(namespace, b"video".to_vec(), 7).await
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
}
