//! トランスポート抽象化層
//!
//! QUIC / WebTransport over HTTP/3 / WebTransport over HTTP/2 を統一的に扱うための型を
//! 定義する。
//!
//! publisher (送信側) と subscriber (受信側) の双方が利用する union API を提供し、
//! 各バイナリは必要なメソッドだけを呼び出す。

use std::sync::Arc;

use bytes::Bytes;
use shiguredo_moqt::session::types::RequestStreamEnd;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::watch;

use crate::error::TransportError;
use crate::webtransport::{MoqtCloseCode, WtSessionState, moqt_close_code, wait_until_terminated};
use crate::webtransport_h2::{WtH2RecvStream, WtH2SendStream, WtH2Session};
use crate::webtransport_h3::{WtRecvStream, WtSendStream, WtSession};

/// 受信ストリームから取り出した 1 要素
pub enum RecvChunk {
    Data(Bytes),
    End(RequestStreamEnd),
}

/// 送信ストリーム
pub enum SendStream {
    Quic(s2n_quic::stream::SendStream),
    WtH3(WtSendStream),
    WtH2(WtH2SendStream),
}

impl SendStream {
    /// データを送信する
    pub async fn send(&mut self, data: Bytes) -> Result<(), TransportError> {
        match self {
            SendStream::Quic(s) => s
                .send(data)
                .await
                .map_err(|e| TransportError::Quic(format!("{e}"))),
            SendStream::WtH3(s) => s.send(&data).await,
            SendStream::WtH2(s) => s.send(&data).await,
        }
    }

    /// ストリーム ID を返す
    pub fn stream_id(&self) -> u64 {
        match self {
            SendStream::Quic(s) => s.id(),
            SendStream::WtH3(s) => s.stream_id(),
            SendStream::WtH2(s) => s.stream_id(),
        }
    }

    /// ストリームを終了する
    pub fn finish(&mut self) -> Result<(), TransportError> {
        match self {
            SendStream::Quic(s) => s.finish().map_err(|e| TransportError::Quic(format!("{e}"))),
            SendStream::WtH3(s) => s.finish(),
            SendStream::WtH2(s) => s.finish(),
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
            SendStream::WtH3(s) => s.reset(error_code),
            SendStream::WtH2(s) => s.reset(error_code),
        }
    }
}

/// 受信ストリーム
pub enum RecvStream {
    Quic(s2n_quic::stream::ReceiveStream),
    WtH3(WtRecvStream),
    WtH2(WtH2RecvStream),
}

impl RecvStream {
    /// データまたは終端を受信する
    pub async fn receive_chunk(&mut self) -> Result<RecvChunk, TransportError> {
        match self {
            RecvStream::Quic(s) => match s.receive().await {
                Ok(Some(data)) => Ok(RecvChunk::Data(data)),
                Ok(None) => Ok(RecvChunk::End(RequestStreamEnd::Fin)),
                Err(s2n_quic::stream::Error::StreamReset { error, .. }) => {
                    // QUIC 経路は QUIC のコード空間であり常にコードがあるため Some にする
                    Ok(RecvChunk::End(RequestStreamEnd::Reset {
                        error_code: Some(error.into()),
                        reliable_size: None,
                    }))
                }
                Err(e) => Err(TransportError::Quic(format!("{e}"))),
            },
            RecvStream::WtH3(s) => match s.recv_chunk().await? {
                crate::webtransport_h3::RecvChunk::Data(data) => {
                    Ok(RecvChunk::Data(Bytes::from(data)))
                }
                crate::webtransport_h3::RecvChunk::End(end) => Ok(RecvChunk::End(end)),
            },
            RecvStream::WtH2(s) => match s.recv_chunk().await? {
                crate::webtransport_h2::RecvChunk::Data(data) => {
                    Ok(RecvChunk::Data(Bytes::from(data)))
                }
                crate::webtransport_h2::RecvChunk::End(end) => Ok(RecvChunk::End(end)),
            },
        }
    }

    /// 受信方向へ STOP_SENDING を送出する
    ///
    /// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection): 受信方向の
    /// cancel は STOP_SENDING で行う。error code は §12.5 (Stream Reset Error Codes) から選ぶ。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fn stop_sending(&mut self, error_code: u64) -> Result<(), TransportError> {
        match self {
            RecvStream::Quic(s) => {
                let code = s2n_quic::application::Error::new(error_code)
                    .map_err(|e| TransportError::Quic(format!("{e}")))?;
                s.stop_sending(code)
                    .map_err(|e| TransportError::Quic(format!("{e}")))
            }
            RecvStream::WtH3(s) => s.stop_sending(error_code),
            RecvStream::WtH2(s) => s.stop_sending(error_code),
        }
    }

    /// ストリーム ID を返す
    pub fn stream_id(&self) -> u64 {
        match self {
            RecvStream::Quic(s) => s.id(),
            RecvStream::WtH3(s) => s.stream_id(),
            RecvStream::WtH2(s) => s.stream_id(),
        }
    }
}

/// 送信ストリームを開くためのハンドル (Clone 可能)
///
/// 将来の FETCH 等でデータストリームを開く際にも使用する。
///
/// WebTransport では session を `Arc<Mutex<...>>` で共有する。`StreamHandle` は
/// 複数の非同期タスクから clone され、各操作は送信ストリームの open と 1 回の
/// send のように await をまたいでも短時間で終わるため、チャネルで単一所有者へ
/// 依頼する構成より `Mutex` の方が構成を単純にできる。
///
/// 一方、受信ループ (peer 起動ストリームの accept) は次の 1 件が届くまで待ち続ける。
/// その待機をロック内で行うと `open_send_stream` / `send_datagram` / `close` が
/// 待たされるため、受信側は receiver を acceptor が持ち、ロック外で待つ
/// (`WtSession::take_uni_receiver` / `WtH2Session::take_uni_receiver`)。
#[derive(Clone)]
pub enum StreamHandle {
    Quic(s2n_quic::connection::Handle),
    WtH3(Arc<Mutex<WtSession>>),
    WtH2(Arc<Mutex<WtH2Session>>),
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
            StreamHandle::WtH3(session) => {
                let mut session = session.lock().await;
                let stream = session.open_uni_stream().await?;
                Ok(SendStream::WtH3(stream))
            }
            StreamHandle::WtH2(session) => {
                let mut session = session.lock().await;
                let stream = session.open_uni_stream().await?;
                Ok(SendStream::WtH2(stream))
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
            StreamHandle::WtH3(session) => {
                let mut session = session.lock().await;
                let wt_bi = session.open_bi_stream().await?;
                let (send, recv) = wt_bi.into_parts();
                Ok((SendStream::WtH3(send), RecvStream::WtH3(recv)))
            }
            StreamHandle::WtH2(session) => {
                let mut session = session.lock().await;
                let (send, recv) = session.open_bi_stream().await?;
                Ok((SendStream::WtH2(send), RecvStream::WtH2(recv)))
            }
        }
    }

    /// 接続をクローズする
    ///
    /// QUIC では application error code を、WebTransport では CLOSE_SESSION capsule を送信する。
    /// WebTransport の Application Error Code は 32 ビットのため、収まらない MOQT のコードは
    /// 切り捨てず、接続レベルの close に切り替える (`moqt_close_code` の判定)。
    /// セッション状態を終了へ移すため、以降は新しいストリームの open と datagram の送信が
    /// 拒否される (§6)。
    pub async fn close(&self, code: u64, reason: &str) -> Result<(), TransportError> {
        match self {
            StreamHandle::Quic(handle) => {
                let err = s2n_quic::application::Error::new(code)
                    .unwrap_or(s2n_quic::application::Error::UNKNOWN);
                handle.close(err);
                Ok(())
            }
            StreamHandle::WtH3(session) => {
                let code = moqt_close_code(code);
                let mut session = session.lock().await;
                session.close(code, reason).await
            }
            StreamHandle::WtH2(session) => {
                // over HTTP/2 の `WT_CLOSE_SESSION` の Application Error Code も実装は 32 ビットで
                // 扱う (`WtH2Session::close` の引数)。32 ビットを超える値は capsule で運べないため、
                // ここでは 32 ビットに収まらないことを利用者へ明示して失敗させる。
                // 接続レベルの close へのフォールバックは WebTransport over HTTP/3 のみ実装する。
                let original = code;
                let code = match moqt_close_code(code) {
                    MoqtCloseCode::Capsule(code) => code,
                    MoqtCloseCode::ConnectionClose(_) | MoqtCloseCode::ConnectionCloseUnknown => {
                        return Err(TransportError::Internal(format!(
                            "MOQT close code {original:#x} does not fit in the WT_CLOSE_SESSION application error code over HTTP/2 (0x00000000-0xffffffff)"
                        )));
                    }
                };
                let mut session = session.lock().await;
                session.close(code, reason).await
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
            StreamHandle::WtH3(session) => {
                let session = session.lock().await;
                session.send_datagram(data).await
            }
            StreamHandle::WtH2(session) => {
                let session = session.lock().await;
                session.send_datagram(data).await
            }
        }
    }

    /// datagram を受信する (subscriber 側で使用)
    ///
    /// QUIC では `s2n-quic` の datagram プロバイダーから、WebTransport では
    /// `WtSession::take_buffered_datagrams` / `WtH2Session::take_buffered_datagrams`
    /// から取得する。
    /// WebTransport ではセッション終了を検知した場合に `ConnectionClosed` を返し、
    /// MOQT 層の受信ループを待たせない。
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
            StreamHandle::WtH3(session) => {
                let session = session.lock().await;
                session.take_buffered_datagrams()
            }
            StreamHandle::WtH2(session) => {
                let session = session.lock().await;
                session.take_buffered_datagrams()
            }
        }
    }
}

/// 単方向ストリーム (data stream) を受け入れるためのアクセプター (subscriber 側で使用)
pub enum StreamAcceptor {
    Quic(s2n_quic::connection::ReceiveStreamAcceptor),
    WtH3 {
        /// ストリームを開く側で使う session。accept ではロックを取らない
        session: Arc<Mutex<WtSession>>,
        /// 単方向受信ストリームの receiver (ロック外で待つ)
        uni_rx: mpsc::Receiver<WtRecvStream>,
        /// セッション状態 (§6 の終了 / h3 層が返した接続エラー) の観測用 (ロック外で待つ)
        session_state: watch::Receiver<WtSessionState>,
    },
    WtH2 {
        /// ストリームを開く側で使う session。accept ではロックを取らない
        session: Arc<Mutex<WtH2Session>>,
        /// 単方向受信ストリームの receiver (ロック外で待つ)
        uni_rx: mpsc::UnboundedReceiver<WtH2RecvStream>,
        /// セッション状態 (§6.12 の終了 / 接続エラー) の観測用 (ロック外で待つ)
        session_state: watch::Receiver<WtSessionState>,
    },
}

impl StreamAcceptor {
    /// 単方向ストリーム (data stream) を 1 つ受け入れる
    ///
    /// QUIC では接続が閉じた場合に `Ok(None)` を返す。WebTransport では接続が閉じた場合も
    /// セッションが終了した場合も、接続層が接続エラーを返して接続を閉じた場合も
    /// `ConnectionClosed` を返す (MOQT 層の受信ループを待たせないため)。
    pub async fn accept_recv_stream(&mut self) -> Result<Option<RecvStream>, TransportError> {
        match self {
            StreamAcceptor::Quic(acceptor) => {
                let stream = acceptor
                    .accept_receive_stream()
                    .await
                    .map_err(|e| TransportError::Quic(format!("{e}")))?;
                Ok(stream.map(RecvStream::Quic))
            }
            StreamAcceptor::WtH3 {
                session: _session,
                uni_rx,
                session_state,
            } => {
                // ロックを保持せずに待つ。解放後も session は
                // `open_send_stream` / `send_datagram` / `close` で使える
                tokio::select! {
                    received = uni_rx.recv() => match received {
                        Some(s) => Ok(Some(RecvStream::WtH3(s))),
                        // ルーティングタスクが終了した = QUIC 接続が閉じた (h3 層の接続エラーを
                        // 検知して閉じた場合も含む)
                        None => Err(TransportError::ConnectionClosed),
                    },
                    // セッション終了 (§6) や h3 層の接続エラーを検知したら accept を待ち続けない
                    _ = wait_until_terminated(session_state) => {
                        Err(TransportError::ConnectionClosed)
                    }
                }
            }
            StreamAcceptor::WtH2 {
                session: _session,
                uni_rx,
                session_state,
            } => {
                // ロックを保持せずに待つ。解放後も session は
                // `open_send_stream` / `send_datagram` / `close` で使える
                tokio::select! {
                    received = uni_rx.recv() => match received {
                        Some(s) => Ok(Some(RecvStream::WtH2(s))),
                        // driver タスクが終了した = 接続が閉じた (接続エラーを
                        // 検知して閉じた場合も含む)
                        None => Err(TransportError::ConnectionClosed),
                    },
                    // セッション終了 (§6.12) や接続エラーを検知したら accept を待ち続けない
                    _ = wait_until_terminated(session_state) => {
                        Err(TransportError::ConnectionClosed)
                    }
                }
            }
        }
    }
}

/// peer が開始する双方向ストリーム (request stream) を受け入れるためのアクセプター
///
/// MOQT relay は subscriber の SUBSCRIBE / FETCH を publisher 側 session の新しい
/// request stream として転送する (draft-ietf-moq-transport-21 §6.3 (Session initialization))。
/// publisher はこのストリームを受理しなければ要求に応答できない。
pub enum BidiStreamAcceptor {
    Quic(s2n_quic::connection::BidirectionalStreamAcceptor),
    WtH3 {
        /// ストリームを開く側で使う session。accept ではロックを取らない
        session: Arc<Mutex<WtSession>>,
        /// 双方向受信ストリームの receiver (ロック外で待つ)
        bi_rx: mpsc::Receiver<(WtSendStream, WtRecvStream)>,
        /// セッション状態 (§6 の終了 / h3 層が返した接続エラー) の観測用 (ロック外で待つ)
        session_state: watch::Receiver<WtSessionState>,
    },
    WtH2 {
        /// ストリームを開く側で使う session。accept ではロックを取らない
        session: Arc<Mutex<WtH2Session>>,
        /// 双方向受信ストリームの receiver (ロック外で待つ)
        bi_rx: mpsc::UnboundedReceiver<(WtH2SendStream, WtH2RecvStream)>,
        /// セッション状態 (§6.12 の終了 / 接続エラー) の観測用 (ロック外で待つ)
        session_state: watch::Receiver<WtSessionState>,
    },
}

impl BidiStreamAcceptor {
    /// 双方向ストリーム (peer 起動 request stream) を 1 つ受け入れる
    ///
    /// QUIC では接続が閉じた場合に `Ok(None)` を返す。WebTransport では接続が閉じた場合も
    /// セッションが終了した場合も、接続層が接続エラーを返して接続を閉じた場合も
    /// `ConnectionClosed` を返す (`accept_recv_stream` と同じ)。
    /// WebTransport over HTTP/3 では WT の双方向ストリームヘッダーを読み捨てた後の
    /// payload 部分が返る (`BidiStreamAcceptor::WtH3` が受け取る `WtSendStream` /
    /// `WtRecvStream` は `route_bi_stream` が作る)。WebTransport over HTTP/2 では
    /// ストリームの区別が capsule にあるため、読み捨てるヘッダーは無い。
    pub async fn accept_bidi_stream(
        &mut self,
    ) -> Result<Option<(SendStream, RecvStream)>, TransportError> {
        match self {
            BidiStreamAcceptor::Quic(acceptor) => {
                let stream = acceptor
                    .accept_bidirectional_stream()
                    .await
                    .map_err(|e| TransportError::Quic(format!("{e}")))?;
                Ok(stream.map(|stream| {
                    let (recv, send) = stream.split();
                    (SendStream::Quic(send), RecvStream::Quic(recv))
                }))
            }
            BidiStreamAcceptor::WtH3 {
                session: _session,
                bi_rx,
                session_state,
            } => {
                // ロックを保持せずに待つ
                tokio::select! {
                    received = bi_rx.recv() => match received {
                        Some((send, recv)) => Ok(Some((
                            SendStream::WtH3(send),
                            RecvStream::WtH3(recv),
                        ))),
                        // ルーティングタスクが終了した = QUIC 接続が閉じた
                        // (`accept_recv_stream` と同じ扱い)
                        None => Err(TransportError::ConnectionClosed),
                    },
                    // セッション終了 (§6) や h3 層の接続エラーを検知したら accept を待ち続けない
                    _ = wait_until_terminated(session_state) => {
                        Err(TransportError::ConnectionClosed)
                    }
                }
            }
            BidiStreamAcceptor::WtH2 {
                session: _session,
                bi_rx,
                session_state,
            } => {
                // ロックを保持せずに待つ
                tokio::select! {
                    received = bi_rx.recv() => match received {
                        Some((send, recv)) => Ok(Some((
                            SendStream::WtH2(send),
                            RecvStream::WtH2(recv),
                        ))),
                        // driver タスクが終了した = 接続が閉じた
                        // (`accept_recv_stream` と同じ扱い)
                        None => Err(TransportError::ConnectionClosed),
                    },
                    // セッション終了 (§6.12) や接続エラーを検知したら accept を待ち続けない
                    _ = wait_until_terminated(session_state) => {
                        Err(TransportError::ConnectionClosed)
                    }
                }
            }
        }
    }
}
