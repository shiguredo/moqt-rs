//! トランスポート抽象化層
//!
//! QUIC と WebTransport over HTTP/3 の両方を統一的に扱うための型を定義する。
//!
//! publisher (送信側) と subscriber (受信側) の双方が利用する union API を提供し、
//! 各バイナリは必要なメソッドだけを呼び出す。

use std::sync::Arc;

use bytes::Bytes;
use shiguredo_moqt::session::types::RequestStreamEnd;
use tokio::sync::Mutex;

use crate::error::TransportError;
use crate::webtransport::{WtSendStream, WtSession};

/// 受信ストリームから取り出した 1 要素
pub enum RecvChunk {
    Data(Bytes),
    End(RequestStreamEnd),
}

/// 送信ストリーム
pub enum SendStream {
    Quic(s2n_quic::stream::SendStream),
    WebTransport(WtSendStream),
}

impl SendStream {
    /// データを送信する
    pub async fn send(&mut self, data: Bytes) -> Result<(), TransportError> {
        match self {
            SendStream::Quic(s) => s
                .send(data)
                .await
                .map_err(|e| TransportError::Quic(format!("{e}"))),
            SendStream::WebTransport(s) => s.send(&data).await,
        }
    }

    /// ストリーム ID を返す
    pub fn stream_id(&self) -> u64 {
        match self {
            SendStream::Quic(s) => s.id(),
            SendStream::WebTransport(s) => s.stream_id(),
        }
    }

    /// ストリームを終了する
    pub fn finish(&mut self) -> Result<(), TransportError> {
        match self {
            SendStream::Quic(s) => s.finish().map_err(|e| TransportError::Quic(format!("{e}"))),
            SendStream::WebTransport(s) => s.finish(),
        }
    }

    /// ストリームの送信方向を reset する
    ///
    /// Session の `ResetRequestStream` イベントに対応する。QUIC RESET_STREAM を送出する。
    pub fn reset(&mut self, error_code: u64) -> Result<(), TransportError> {
        match self {
            SendStream::Quic(s) => {
                let code = s2n_quic::application::Error::new(error_code)
                    .map_err(|e| TransportError::Quic(format!("{e}")))?;
                s.reset(code)
                    .map_err(|e| TransportError::Quic(format!("{e}")))
            }
            SendStream::WebTransport(s) => s.reset(error_code),
        }
    }
}

/// 受信ストリーム
pub enum RecvStream {
    Quic(s2n_quic::stream::ReceiveStream),
    WebTransport(crate::webtransport::WtRecvStream),
}

impl RecvStream {
    /// データまたは終端を受信する
    pub async fn receive_chunk(&mut self) -> Result<RecvChunk, TransportError> {
        match self {
            RecvStream::Quic(s) => match s.receive().await {
                Ok(Some(data)) => Ok(RecvChunk::Data(data)),
                Ok(None) => Ok(RecvChunk::End(RequestStreamEnd::Fin)),
                Err(s2n_quic::stream::Error::StreamReset { error, .. }) => {
                    Ok(RecvChunk::End(RequestStreamEnd::Reset {
                        error_code: error.into(),
                        reliable_size: None,
                    }))
                }
                Err(e) => Err(TransportError::Quic(format!("{e}"))),
            },
            RecvStream::WebTransport(s) => match s.recv_chunk().await? {
                crate::webtransport::RecvChunk::Data(data) => {
                    Ok(RecvChunk::Data(Bytes::from(data)))
                }
                crate::webtransport::RecvChunk::End(end) => Ok(RecvChunk::End(end)),
            },
        }
    }

    /// ストリーム ID を返す
    pub fn stream_id(&self) -> u64 {
        match self {
            RecvStream::Quic(s) => s.id(),
            RecvStream::WebTransport(s) => s.stream_id(),
        }
    }
}

/// 送信ストリームを開くためのハンドル (Clone 可能)
///
/// 将来の FETCH 等でデータストリームを開く際にも使用する。
///
/// WebTransport では `WtSession` を `Arc<Mutex<...>>` で共有する。`StreamHandle` は
/// 複数の非同期タスクから clone され、各操作は await をまたがず短時間で完結するため、
/// チャネルで単一所有者へ依頼する構成より `Mutex` の方が構成を単純にできる。
#[derive(Clone)]
pub enum StreamHandle {
    Quic(s2n_quic::connection::Handle),
    WebTransport(Arc<Mutex<WtSession>>),
}

impl StreamHandle {
    /// 新しい送信ストリームを開く
    pub async fn open_send_stream(&self) -> Result<SendStream, TransportError> {
        match self {
            StreamHandle::Quic(handle) => {
                let stream = handle
                    .clone()
                    .open_send_stream()
                    .await
                    .map_err(|e| TransportError::Quic(format!("{e}")))?;
                Ok(SendStream::Quic(stream))
            }
            StreamHandle::WebTransport(session) => {
                let mut session = session.lock().await;
                let stream = session.open_uni_stream().await?;
                Ok(SendStream::WebTransport(stream))
            }
        }
    }

    /// 新しい双方向ストリームを開く (リクエスト-レスポンス用)
    ///
    /// draft-ietf-moq-transport-21 §6.3 (Session initialization): PUBLISH / SUBSCRIBE 等の
    /// リクエストメッセージは双方向ストリームで送受信する。
    pub async fn open_bidi_stream(&self) -> Result<(SendStream, RecvStream), TransportError> {
        match self {
            StreamHandle::Quic(handle) => {
                let stream = handle
                    .clone()
                    .open_bidirectional_stream()
                    .await
                    .map_err(|e| TransportError::Quic(format!("{e}")))?;
                let (recv, send) = stream.split();
                Ok((SendStream::Quic(send), RecvStream::Quic(recv)))
            }
            StreamHandle::WebTransport(session) => {
                let mut session = session.lock().await;
                let wt_bi = session.open_bi_stream().await?;
                let (send, recv) = wt_bi.into_parts();
                Ok((
                    SendStream::WebTransport(send),
                    RecvStream::WebTransport(recv),
                ))
            }
        }
    }

    /// 接続をクローズする
    ///
    /// QUIC では application error code を、WebTransport では CLOSE_SESSION capsule を送信する。
    pub async fn close(&self, code: u64, reason: &str) -> Result<(), TransportError> {
        match self {
            StreamHandle::Quic(handle) => {
                let err = s2n_quic::application::Error::new(code)
                    .unwrap_or(s2n_quic::application::Error::UNKNOWN);
                handle.close(err);
                Ok(())
            }
            StreamHandle::WebTransport(session) => {
                let mut session = session.lock().await;
                session.close(code as u32, reason).await
            }
        }
    }

    /// datagram を送信する (publisher 側で使用)
    ///
    /// QUIC では s2n-quic の datagram sender を使い、WebTransport では HTTP Datagram 経由で送信する。
    pub async fn send_datagram(&self, data: &[u8]) -> Result<(), TransportError> {
        match self {
            StreamHandle::Quic(handle) => {
                let bytes = Bytes::copy_from_slice(data);
                handle
                    .datagram_mut(
                        |sender: &mut s2n_quic::provider::datagram::default::Sender| {
                            sender.send_datagram(bytes)
                        },
                    )
                    .map_err(|e| TransportError::Quic(format!("datagram send error: {e}")))?
                    .map_err(|e| TransportError::Quic(format!("datagram send error: {e}")))?;
                Ok(())
            }
            StreamHandle::WebTransport(session) => {
                let session = session.lock().await;
                session.send_datagram(data).await
            }
        }
    }

    /// datagram を受信する (subscriber 側で使用)
    ///
    /// QUIC では `s2n-quic` の datagram プロバイダーから、WebTransport では
    /// `WtSession::take_buffered_datagrams` から取得する。
    pub async fn recv_datagrams(&self) -> Result<Vec<Vec<u8>>, TransportError> {
        match self {
            StreamHandle::Quic(handle) => {
                let mut datagrams = Vec::new();
                let result = handle.datagram_mut(
                    |receiver: &mut s2n_quic::provider::datagram::default::Receiver| {
                        while let Some(bytes) = receiver.recv_datagram() {
                            datagrams.push(bytes.to_vec());
                        }
                    },
                );
                result.map_err(|e| TransportError::Quic(format!("datagram query error: {e}")))?;
                Ok(datagrams)
            }
            StreamHandle::WebTransport(session) => {
                let session = session.lock().await;
                session.take_buffered_datagrams()
            }
        }
    }
}

/// 受信ストリームを受け入れるためのアクセプター (subscriber 側で使用)
pub enum StreamAcceptor {
    Quic(s2n_quic::connection::ReceiveStreamAcceptor),
    WebTransport(Arc<Mutex<WtSession>>),
}

impl StreamAcceptor {
    /// 受信ストリームを 1 つ受け入れる
    ///
    /// 接続が閉じた場合は `Ok(None)` を返す。
    pub async fn accept_recv_stream(&mut self) -> Result<Option<RecvStream>, TransportError> {
        match self {
            StreamAcceptor::Quic(acceptor) => {
                let stream = acceptor
                    .accept_receive_stream()
                    .await
                    .map_err(|e| TransportError::Quic(format!("{e}")))?;
                Ok(stream.map(RecvStream::Quic))
            }
            StreamAcceptor::WebTransport(session) => {
                let mut session = session.lock().await;
                match session.accept_uni_stream().await {
                    Ok(s) => Ok(Some(RecvStream::WebTransport(s))),
                    Err(TransportError::StreamClosed) => Ok(None),
                    Err(e) => Err(e),
                }
            }
        }
    }
}
