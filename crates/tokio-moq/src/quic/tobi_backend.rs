//! tobi-quic をネイティブ QUIC のバックエンドとして使う実装

use std::net::SocketAddr;
use std::sync::Arc;

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
#[derive(Debug)]
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

    /// 接続元のアドレスを返す
    pub async fn remote_addr(&self) -> std::result::Result<SocketAddr, QuicError> {
        self.inner.remote_addr().await.map_err(tobi::map_error)
    }
}

/// ストリームを開く側の接続ハンドル
#[derive(Debug, Clone)]
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
pub(crate) async fn connect(
    socket_addr: SocketAddr,
    server_name: &str,
    cert_path: Option<&str>,
) -> Result<(Connected, StopSendingReceiver)> {
    let tls = crate::tls::build_quic_client_config(cert_path, ALPN)?;

    let client = Client::builder()
        .with_tls(ClientTlsConfig::new(tls))
        .with_io(IoConfig::new(crate::local_bind_addr(socket_addr)))
        // ビルダーに保持した設定は `start()` で固定順に適用されるため、呼ぶ順序に
        // 依存しない
        .with_transport(tobi::build_transport_config())
        .with_datagram(tobi::datagram_config())
        // s2n-quic の endpoint は Server / Client のハンドルとは独立したタスクで常時
        // ソケットを駆動していた。tobi-quic の既定はコマンド駆動であり、アプリが API を
        // 呼んでいない間は受信とタイマーを処理しないため、同じ挙動になるよう接続の生成時
        // からバックグラウンド駆動を有効にする。無効のままだと、MOQT 層が受信待ちに
        // 入っている間に ACK や再送が止まる。
        .with_background_driver(true)
        .start()
        .map_err(|e| TransportError::Quic(format!("client start failed: {e}")))?;

    let connection = client
        .connect(Connect::new(socket_addr, server_name))
        .await
        .map_err(|e| TransportError::Quic(format!("connection failed: {e}")))?;

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
        .with_background_driver(true)
        .start()
        .map_err(|e| TransportError::Internal(format!("failed to start server: {e}")))?;
    Ok(Server { inner: server })
}
