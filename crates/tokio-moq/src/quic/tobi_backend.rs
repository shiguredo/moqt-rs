//! tobi-quic をネイティブ QUIC のバックエンドとして使う実装

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tobi_quic::{Client, ClientTlsConfig, Connect, Connection as TobiConnection, IoConfig};
#[cfg(test)]
use tobi_quic::{Server as TobiServer, ServerTlsConfig};

use super::CloseReason;
#[cfg(test)]
use super::ServerTls;
use super::tobi;
pub use super::tobi::RecvStream;
use super::{ALPN, QuicError, StopSendingReceiver};
use crate::error::{Result, TransportError};

/// 接続を確立する前の接続
///
/// `split` でストリームを開くハンドルと、ストリームを受理するハンドルに分ける。
pub struct Connected {
    /// tobi-quic の接続
    inner: TobiConnection,
}

impl Connected {
    /// 接続を操作ハンドルとストリーム受理ハンドルに分割する
    pub(crate) fn split(self) -> (Connection, StreamAcceptor) {
        let (handle, acceptor) = self.inner.split();
        (
            Connection { inner: handle },
            StreamAcceptor {
                inner: Arc::new(acceptor),
            },
        )
    }

    /// アプリケーションが API を呼んでいない間も受信とタイマーを処理する
    pub async fn enable_background_driver(&self) -> std::result::Result<(), QuicError> {
        self.inner
            .enable_background_driver()
            .await
            .map_err(tobi::map_error)
    }

    /// 接続元のアドレスを返す
    pub async fn remote_addr(&self) -> std::result::Result<SocketAddr, QuicError> {
        self.inner.remote_addr().await.map_err(tobi::map_error)
    }
}

/// ストリームを開く側の接続ハンドル
#[derive(Clone)]
pub struct Connection {
    /// tobi-quic の接続ハンドル
    inner: TobiConnection,
}

impl Connection {
    /// 新しい単方向ストリームを開く
    pub(crate) async fn open_send_stream_wait(&self) -> std::result::Result<SendStream, QuicError> {
        let stream = self
            .inner
            .open_send_stream_wait()
            .await
            .map_err(tobi::map_error)?;
        Ok(SendStream {
            inner: Mutex::new(stream),
        })
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
        Ok((
            SendStream {
                inner: Mutex::new(send),
            },
            RecvStream::new(recv),
        ))
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
pub struct SendStream {
    /// tobi-quic の送信ストリーム
    ///
    /// `tobi_quic::SendStream` はキャンセル安全性のために進行中の書き込み future を保持し、
    /// `Sync` ではない。`MoqtClient` を `Sync` に保つため `Mutex` で包む。操作はすべて
    /// `&mut self` なので、`get_mut` でロックを取らずに使う。
    inner: Mutex<tobi_quic::SendStream>,
}

impl SendStream {
    /// データをすべて送信する
    pub(crate) async fn send_all(&mut self, data: &[u8]) -> std::result::Result<(), QuicError> {
        let inner = self
            .inner
            .get_mut()
            .expect("the send stream mutex must not be poisoned");
        tobi::send_all(inner, data).await.map_err(tobi::map_error)
    }

    /// 送信方向を終了する (FIN)
    pub(crate) async fn finish(&mut self) -> std::result::Result<(), QuicError> {
        self.inner
            .get_mut()
            .expect("the send stream mutex must not be poisoned")
            .finish()
            .await
            .map_err(tobi::map_error)
    }

    /// 送信方向を reset する (RESET_STREAM)
    pub(crate) async fn reset(&mut self, error_code: u64) -> std::result::Result<(), QuicError> {
        self.inner
            .get_mut()
            .expect("the send stream mutex must not be poisoned")
            .reset_stream(error_code)
            .await
            .map_err(tobi::map_error)
    }

    /// ストリーム ID を返す
    pub(crate) fn id(&self) -> u64 {
        self.inner
            .lock()
            .expect("the send stream mutex must not be poisoned")
            .id()
    }
}

/// ストリームを受理する側のハンドル
///
/// tobi-quic の `StreamAcceptor` は単方向と双方向の両方を受け付けられ、受理は `&self` を
/// 取る。単方向と双方向を別のタスクから同時に待つため `Arc` で共有する。
pub struct StreamAcceptor {
    /// tobi-quic のストリーム受理
    inner: Arc<tobi_quic::StreamAcceptor>,
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
        Ok((
            SendStream {
                inner: Mutex::new(send),
            },
            RecvStream::new(recv),
        ))
    }
}

/// QUIC クライアント接続を確立する
pub(crate) async fn connect(
    socket_addr: SocketAddr,
    server_name: &str,
    cert_path: Option<&str>,
) -> Result<(Connected, StopSendingReceiver)> {
    let tls = crate::tls::build_quic_client_config(cert_path, ALPN)?;

    let client = Client::builder()
        .with_tls(ClientTlsConfig::new(tls))
        .with_io(IoConfig::new(crate::local_bind_addr(socket_addr)))
        // tobi-quic の `with_datagram` は現在の transport 設定へ書き込むため、
        // `with_transport` で置き換えた後に呼ぶ (逆順だと DATAGRAM の広告が消える)
        .with_transport(tobi::build_transport_config())
        .with_datagram(tobi::datagram_config())
        .start()
        .map_err(|e| TransportError::Quic(format!("client start failed: {e}")))?;

    let connection = client
        .connect(Connect::new(socket_addr, server_name))
        .await
        .map_err(|e| TransportError::Quic(format!("connection failed: {e}")))?;

    // s2n-quic の endpoint は Server / Client のハンドルとは独立したタスクで常時ソケットを
    // 駆動していた。tobi-quic の既定はコマンド駆動であり、アプリケーションが API を呼んで
    // いない間は受信とタイマーを処理しないため、同じ挙動になるようバックグラウンド駆動を
    // 有効にする。無効のままだと、MOQT 層が受信待ちに入っている間に ACK や再送が止まる。
    connection.enable_background_driver().await.map_err(|e| {
        TransportError::Quic(format!("failed to enable the background driver: {e}"))
    })?;

    let selected = connection.application_protocol().await;
    super::verify_alpn(selected.as_deref())?;

    tobi::spawn_packet_diagnostics(&connection);
    let stop_sending_rx = tobi::spawn_connection_observer(&connection);

    tracing::info!("Connected to {socket_addr}");
    Ok((Connected { inner: connection }, stop_sending_rx))
}

/// テスト用の QUIC サーバー
#[cfg(test)]
pub(crate) struct Server {
    /// tobi-quic のサーバー
    inner: TobiServer,
}

#[cfg(test)]
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
        Ok(Connected { inner: connection })
    }
}

/// テスト用の QUIC サーバーを起動する
#[cfg(test)]
pub(crate) async fn start_server(tls: ServerTls, bind_addr: SocketAddr) -> Result<Server> {
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(tls.cert_der)],
            rustls::pki_types::PrivateKeyDer::Pkcs8(tls.key_der.into()),
        )
        .map_err(|e| TransportError::Internal(format!("failed to set certificate: {e}")))?;
    config.alpn_protocols = tls.alpn;

    let server = TobiServer::builder()
        .with_tls(ServerTlsConfig::new(Arc::new(config)))
        .with_io(IoConfig::new(bind_addr))
        .with_transport(tobi::build_transport_config())
        .with_datagram(tobi::datagram_config())
        .start()
        .map_err(|e| TransportError::Internal(format!("failed to start server: {e}")))?;
    Ok(Server { inner: server })
}
