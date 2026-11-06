//! トランスポート抽象化層
//!
//! QUIC / WebTransport over HTTP/3 / WebTransport over HTTP/2 / QMux を統一的に扱うための
//! 型を定義する。
//!
//! publisher (送信側) と subscriber (受信側) の双方が利用する union API を提供し、
//! 各バイナリは必要なメソッドだけを呼び出す。

use std::sync::Arc;

use crate::quic::QuicError;
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
    Quic(crate::quic::SendStream),
    WtH3(WtSendStream),
    WtH2(WtH2SendStream),
    /// QMux (draft-ietf-quic-qmux-02)
    Qmux(tokio_qmux::SendStream),
}

impl SendStream {
    /// データを送信する
    pub async fn send(&mut self, data: Bytes) -> Result<(), TransportError> {
        match self {
            SendStream::Quic(s) => s.send_all(&data).await.map_err(|e| {
                TransportError::from_send_error(e, |e| TransportError::Quic(format!("{e}")))
            }),
            SendStream::WtH3(s) => s.send(&data).await,
            SendStream::WtH2(s) => s.send(&data).await,
            SendStream::Qmux(s) => {
                // tokio-qmux の `write` は渡したデータをすべて受け付けるまで待つ
                // (フロー制御の調整はドライバが行う)。戻り値は常に `data.len()` になる
                s.write(&data)
                    .await
                    .map(|_| ())
                    .map_err(TransportError::from_qmux)
            }
        }
    }

    /// ストリーム ID を返す
    pub fn stream_id(&self) -> u64 {
        match self {
            SendStream::Quic(s) => s.id(),
            SendStream::WtH3(s) => s.stream_id(),
            SendStream::WtH2(s) => s.stream_id(),
            SendStream::Qmux(s) => s.id().as_u64(),
        }
    }

    /// ストリームを終了する
    ///
    /// peer が cancel 済みのストリームでは I/O 層が拒否を返す (wt-h2 の driver の reply を待つ。
    /// [`TransportError::StreamReset`] / [`TransportError::StreamClosed`])。
    pub async fn finish(&mut self) -> Result<(), TransportError> {
        match self {
            SendStream::Quic(s) => s.finish().await.map_err(|e| {
                TransportError::from_send_error(e, |e| TransportError::Quic(format!("{e}")))
            }),
            SendStream::WtH3(s) => s.finish().await,
            SendStream::WtH2(s) => s.finish().await,
            SendStream::Qmux(s) => s.finish().await.map_err(TransportError::from_qmux),
        }
    }

    /// ストリームの送信方向を reset する
    ///
    /// Session の `ResetRequestStream` イベントに対応する。QUIC RESET_STREAM を送出する。
    /// peer が cancel 済みのストリームでは I/O 層が拒否を返す (`finish` と同じ)。
    pub async fn reset(&mut self, error_code: u64) -> Result<(), TransportError> {
        match self {
            SendStream::Quic(s) => s.reset(error_code).await.map_err(|e| {
                TransportError::from_send_error(e, |e| TransportError::Quic(format!("{e}")))
            }),
            SendStream::WtH3(s) => s.reset(error_code).await,
            SendStream::WtH2(s) => s.reset(error_code).await,
            SendStream::Qmux(s) => s.reset(error_code).await.map_err(TransportError::from_qmux),
        }
    }
}

/// 受信ストリーム
pub enum RecvStream {
    /// QUIC 直接接続の受信ストリーム
    Quic(crate::quic::RecvStream),
    WtH3(WtRecvStream),
    WtH2(WtH2RecvStream),
    /// QMux (draft-ietf-quic-qmux-02)
    ///
    /// `tokio_qmux::RecvStream::read` は呼び出し側のバッファへ書くため、読み出しごとの
    /// 確保を避けてストリームごとに 1 本持つ ([`RecvStream::new_qmux`])。
    Qmux {
        /// QMux の受信ストリーム
        stream: tokio_qmux::RecvStream,
        /// 読み出し用のバッファ (初回の読み出しまで確保しない)
        buf: Option<Box<[u8]>>,
    },
}

impl RecvStream {
    /// QUIC の受信ストリームを作る
    pub(crate) fn new_quic(stream: crate::quic::RecvStream) -> Self {
        Self::Quic(stream)
    }

    /// QMux の受信ストリームを作る
    ///
    /// 読み出し用のバッファは初回の読み出しで確保する。受理したまま読まれないストリームが
    /// 積まれても、ストリームごとの 64 KiB を先行確保しないためである。
    pub(crate) fn new_qmux(stream: tokio_qmux::RecvStream) -> Self {
        Self::Qmux { stream, buf: None }
    }
    /// データまたは終端を受信する
    pub async fn receive_chunk(&mut self) -> Result<RecvChunk, TransportError> {
        match self {
            RecvStream::Quic(stream) => match stream.receive().await {
                Ok(Some(data)) => Ok(RecvChunk::Data(Bytes::from(data))),
                Ok(None) => Ok(RecvChunk::End(RequestStreamEnd::Fin)),
                Err(QuicError::Reset { error_code }) => {
                    // QUIC 経路は QUIC のコード空間であり常にコードがあるため Some にする
                    Ok(RecvChunk::End(RequestStreamEnd::Reset {
                        error_code: Some(error_code),
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
            // `tokio_qmux::RecvStream::read` は await 点の前で内部状態を変えないため
            // cancel safe である (qmux-rs の doc には明記されていないが、呼び出し側が
            // 破棄しても読み出し済みのバイトは失われない)。moq-sub は `select!` で
            // この future を破棄しうる
            RecvStream::Qmux { stream, buf } => {
                let buf = buf.get_or_insert_with(|| {
                    vec![0u8; crate::qmux::READ_BUFFER_SIZE].into_boxed_slice()
                });
                match stream.read(buf).await {
                    Ok(Some(len)) => Ok(RecvChunk::Data(Bytes::copy_from_slice(&buf[..len]))),
                    Ok(None) => Ok(RecvChunk::End(RequestStreamEnd::Fin)),
                    // QMux は RESET_STREAM_AT 拡張を実装しないため reliable_size は常に None
                    Err(tokio_qmux::Error::Reset(error_code)) => {
                        Ok(RecvChunk::End(RequestStreamEnd::Reset {
                            error_code: Some(error_code),
                            reliable_size: None,
                        }))
                    }
                    Err(e) => Err(TransportError::from_qmux(e)),
                }
            }
        }
    }

    /// 受信方向へ STOP_SENDING を送出する
    ///
    /// draft-ietf-moq-transport-23 §6.4.2.3 (Request Cancellation and Rejection): 受信方向の
    /// cancel は STOP_SENDING で行う。error code は §12.5 (Stream Reset Error Codes) から選ぶ。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub async fn stop_sending(&mut self, error_code: u64) -> Result<(), TransportError> {
        match self {
            RecvStream::Quic(stream) => stream
                .stop_sending(error_code)
                .await
                .map_err(|e| TransportError::Quic(format!("{e}"))),
            RecvStream::WtH3(s) => s.stop_sending(error_code).await,
            RecvStream::WtH2(s) => s.stop_sending(error_code),
            RecvStream::Qmux { stream, .. } => stream
                .stop_sending(error_code)
                .await
                .map_err(TransportError::from_qmux),
        }
    }

    /// ストリーム ID を返す
    pub fn stream_id(&self) -> u64 {
        match self {
            RecvStream::Quic(stream) => stream.id(),
            RecvStream::WtH3(s) => s.stream_id(),
            RecvStream::WtH2(s) => s.stream_id(),
            RecvStream::Qmux { stream, .. } => stream.id().as_u64(),
        }
    }
}

/// STOP_SENDING のエラーコード変換に使う transport 種別
///
/// wire のエラーコードの code space は transport ごとに異なるため、変換則を型で分ける。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopSendingTransport {
    /// QUIC 直接接続 (`moqt://`)。QUIC の code space を共有する
    Quic,
    /// WebTransport over HTTP/3。HTTP/3 の code space を共有する
    WtH3,
    /// WebTransport over HTTP/2。capsule がアプリケーションエラーコードを直接運ぶため
    /// remap しない (draft-ietf-webtrans-http2-15 §6.3)
    WtH2,
    /// QMux。QUIC version 1 のフレームをそのまま多重化するため QUIC の code space を
    /// 共有し、remap しない (draft-ietf-quic-qmux-02 Section 4)
    Qmux,
}

/// peer から受信した STOP_SENDING の wire コードを MOQT のコードへ写す
///
/// QUIC 直接接続は QUIC の code space を共有するため remap しない。
/// QMux は QUIC version 1 のフレーム (STOP_SENDING / RESET_STREAM) をそのまま多重化するため
/// remap しない (draft-ietf-quic-qmux-02 Section 4)。
/// WebTransport over HTTP/2 は capsule がアプリケーションエラーコードをそのまま運ぶため
/// remap しない (draft-ietf-webtrans-http2-15 §6.3)。
/// WebTransport over HTTP/3 は HTTP/3 の code space を共有するため §4.4 の remap を行い、
/// remap できない値は「アプリケーションエラーコード無し」を表す `None` にする
/// (draft-ietf-webtrans-http3-16 §4.4 (Resetting Data Streams))。
///
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
pub(crate) fn remap_stop_sending_error_code(
    transport: StopSendingTransport,
    wire_code: u64,
) -> Option<u64> {
    match transport {
        StopSendingTransport::Quic | StopSendingTransport::WtH2 | StopSendingTransport::Qmux => {
            Some(wire_code)
        }
        StopSendingTransport::WtH3 => crate::webtransport_h3::wt_stop_sending_error_code(wire_code),
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
    /// QUIC 直接接続の接続ハンドル
    ///
    /// いずれのバックエンドのハンドルも Clone 可能であり、複数タスクから同じ接続を
    /// 操作できる。
    Quic(crate::quic::Connection),
    WtH3(Arc<Mutex<WtSession>>),
    WtH2(Arc<Mutex<WtH2Session>>),
    /// QMux (draft-ietf-quic-qmux-02)
    ///
    /// `tokio_qmux::Connection` は Clone できないが、接続を必要とするメソッドはすべて
    /// `&self` を取るため `Arc` で共有できる。accept の待機は接続を共有した別タスクが行い、
    /// 結果を receiver へ流す (`StreamAcceptor::Qmux` / `BidiStreamAcceptor::Qmux`)。
    Qmux(Arc<crate::qmux::QmuxHandle>),
}

impl StreamHandle {
    /// 新しい送信ストリームを開く
    pub async fn open_send_stream(&self) -> Result<SendStream, TransportError> {
        match self {
            StreamHandle::Quic(handle) => {
                // いずれのバックエンドも、ピアが許可した枠が空くまで待つ
                let stream = handle
                    .open_send_stream_wait()
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
            StreamHandle::Qmux(handle) => {
                let stream = handle
                    .connection()
                    .open_unidirectional()
                    .await
                    .map_err(TransportError::from_qmux)?;
                Ok(SendStream::Qmux(stream))
            }
        }
    }

    /// peer から受信した STOP_SENDING を WebTransport (h3) 層へ通知する
    ///
    /// WebTransport over HTTP/3 は h3 層がストリーム状態を持つため、Sans I/O の h3 層へ
    /// 受信を通知する必要がある (draft-ietf-webtrans-http3-16 §4.4 (Resetting Data Streams))。
    /// QUIC 直接接続は h3 層を持たず、WebTransport over HTTP/2 は driver が capsule を
    /// 受信して自ら状態を更新するため、どちらも何もしない
    /// (draft-ietf-webtrans-http2-15 §6.3)。QMux も h3 層を持たないため何もしない
    /// (QMux 自身がストリーム状態を管理する)。
    pub(crate) async fn notify_received_stop_sending(
        &self,
        stream_id: u64,
        error_code: u64,
    ) -> Result<(), TransportError> {
        match self {
            StreamHandle::Quic(_) | StreamHandle::WtH2(_) | StreamHandle::Qmux(_) => Ok(()),
            StreamHandle::WtH3(session) => {
                let mut session = session.lock().await;
                session.notify_received_stop_sending(stream_id, error_code)
            }
        }
    }

    /// peer から受信した STOP_SENDING の wire コードを MOQT のコードへ戻す
    ///
    /// `None` は「アプリケーションエラーコード無し」を表す (draft-ietf-webtrans-http3-16 §4.4)。
    /// 変換則は [`remap_stop_sending_error_code`] を参照する。
    pub(crate) fn remap_stop_sending_error_code(&self, wire_code: u64) -> Option<u64> {
        remap_stop_sending_error_code(self.stop_sending_transport(), wire_code)
    }

    /// STOP_SENDING のコード変換に使う transport 種別
    fn stop_sending_transport(&self) -> StopSendingTransport {
        match self {
            StreamHandle::Quic(_) => StopSendingTransport::Quic,
            StreamHandle::WtH3(_) => StopSendingTransport::WtH3,
            StreamHandle::WtH2(_) => StopSendingTransport::WtH2,
            StreamHandle::Qmux(_) => StopSendingTransport::Qmux,
        }
    }

    /// 新しい双方向ストリームを開く (リクエスト-レスポンス用)
    ///
    /// draft-ietf-moq-transport-23 §6.3 (Session initialization): PUBLISH / SUBSCRIBE 等の
    /// リクエストメッセージは双方向ストリームで送受信する。
    pub async fn open_bidi_stream(&self) -> Result<(SendStream, RecvStream), TransportError> {
        match self {
            StreamHandle::Quic(handle) => {
                // `open_send_stream` と同じ理由で、枠が空くまで待つ variant を使う
                let (send, recv) = handle
                    .open_bidirectional_stream_wait()
                    .await
                    .map_err(|e| TransportError::Quic(format!("{e}")))?;
                Ok((SendStream::Quic(send), RecvStream::new_quic(recv)))
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
            StreamHandle::Qmux(handle) => {
                let (send, recv) = handle
                    .connection()
                    .open_bidirectional()
                    .await
                    .map_err(TransportError::from_qmux)?;
                Ok((SendStream::Qmux(send), RecvStream::new_qmux(recv)))
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
                // 可変長整数に収まらないコードは s2n-quic と同じく UNKNOWN で閉じる
                crate::quic::close_application(handle, code).await?;
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
            StreamHandle::Qmux(handle) => {
                // QMux の CONNECTION_CLOSE のエラーコードは QUIC version 1 の code space を
                // 共有するため、MOQT のコードをそのまま載せる (draft-ietf-quic-qmux-02 Section 4)。
                // tokio-qmux の `close` は可変長整数で表せないコードだけをエラーにし、要求は
                // 送らない (RFC 9000 Section 19.19)。黙って閉じないよう、その失敗を返す。
                // close 要求を送った後のドライバの終了は `closed()` で観測する
                // (ピアの shutdown を待つ上限は tokio-qmux の既定で 3 秒)
                handle
                    .connection()
                    .close(code, reason)
                    .map_err(TransportError::from_qmux)?;
                Ok(())
            }
        }
    }

    /// datagram を送信する (publisher 側で使用)
    ///
    /// QUIC では DATAGRAM (RFC 9221) を、WebTransport では HTTP Datagram 経由で送信する。
    ///
    /// QUIC の送信のキューが満杯のときは、空きができるまで待つ。datagram は再送されないため、
    /// ここで捨てると音声が飛ぶ。満杯になるのは回線が詰まっているときであり、待てば送れる。
    /// 待たずにエラーを返すと、購読側は音声を一切受け取れないまま publisher が終了する。
    pub async fn send_datagram(&self, data: &[u8]) -> Result<(), TransportError> {
        match self {
            StreamHandle::Quic(handle) => {
                // 送信キューの空きができるまで待つ。待たずに捨てると音声が飛ぶ
                handle
                    .send_datagram_wait(data.to_vec())
                    .await
                    .map_err(|e| TransportError::Quic(format!("datagram send error: {e}")))
            }
            StreamHandle::WtH3(session) => {
                let session = session.lock().await;
                session.send_datagram(data).await
            }
            StreamHandle::WtH2(session) => {
                let session = session.lock().await;
                session.send_datagram(data).await
            }
            StreamHandle::Qmux(handle) => {
                // tokio-qmux の `send_datagram` は、ピアが DATAGRAM を広告していない場合や
                // フレームが `max_datagram_frame_size` / `max_record_size` を超える場合に
                // エラーを返す。黙って捨てる経路は無いため、そのまま伝播する。上限の不足は
                // 接続確立時に `qmux::warn_if_datagram_too_large` が 1 度だけ警告する
                handle
                    .connection()
                    .send_datagram(data)
                    .await
                    .map_err(TransportError::from_qmux)
            }
        }
    }

    /// datagram を受信する (subscriber 側で使用)
    ///
    /// QUIC では接続の DATAGRAM 受信キューから、WebTransport では
    /// `WtSession::take_buffered_datagrams` / `WtH2Session::take_buffered_datagrams`
    /// から取得する。
    /// WebTransport ではセッション終了を検知した場合に `ConnectionClosed` を返し、
    /// MOQT 層の受信ループを待たせない。
    pub async fn recv_datagrams(&self) -> Result<Vec<Vec<u8>>, TransportError> {
        match self {
            StreamHandle::Quic(handle) => {
                // 1 つ届くまで待つ。空のまま返すと呼び出し側の受信ループが空回りして
                // CPU を 1 コア使い切り、同じ runtime で動く接続の I/O を飢えさせる
                match handle.receive_datagram().await {
                    Ok(datagram) => Ok(vec![datagram]),
                    // 接続が終了した。MOQT 層の受信ループを待たせないようセッション終了として返す
                    Err(QuicError::ConnectionClosed) => Err(TransportError::ConnectionClosed),
                    Err(e) => Err(TransportError::Quic(format!("datagram receive error: {e}"))),
                }
            }
            StreamHandle::WtH3(session) => {
                let session = session.lock().await;
                session.take_buffered_datagrams()
            }
            StreamHandle::WtH2(session) => {
                let session = session.lock().await;
                session.take_buffered_datagrams()
            }
            StreamHandle::Qmux(handle) => {
                // 破棄された datagram が増えていれば記録する (QMux は速やかに配送できない
                // DATAGRAM の破棄を許すため、取りこぼしを追えるようにする)
                handle.log_dropped_datagrams();
                match handle.connection().recv_datagrams().await {
                    // 1 件以上届くまで待ち、到着済みをまとめて返す。取り出しと返却の間に
                    // await が無いため、呼び出し側が `select!` で future を捨てても
                    // 取り出し済みの datagram は失われない (cancel safe)
                    Ok(Some(datagrams)) => Ok(datagrams),
                    Ok(None) => Err(TransportError::ConnectionClosed),
                    Err(e) => Err(TransportError::from_qmux(e)),
                }
            }
        }
    }
}

/// 単方向ストリーム (data stream) を受け入れるためのアクセプター (subscriber 側で使用)
pub enum StreamAcceptor {
    /// QUIC 直接接続のストリーム受理
    ///
    /// 単方向と双方向の受理を別のタスクから同時に待つため `Arc` で共有する。
    /// 各バックエンドは `&self` で受理できるよう内部で排他する。
    Quic(Arc<crate::quic::StreamAcceptor>),
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
    /// QMux (draft-ietf-quic-qmux-02)
    ///
    /// accept の待機は接続を共有した別タスクが行い、結果を `uni_rx` へ流す
    /// (`qmux::spawn_accept_unidirectional`)。接続が閉じると channel も閉じるため、
    /// 待ち手は [`TransportError::ConnectionClosed`] を得る。
    Qmux {
        /// 単方向受信ストリームの receiver
        uni_rx: mpsc::UnboundedReceiver<RecvStream>,
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
            StreamAcceptor::Quic(acceptor) => match acceptor.accept_receive_stream().await {
                Ok(stream) => Ok(Some(RecvStream::new_quic(stream))),
                // 接続終了は「これ以上ストリームが来ない」として扱う
                Err(e) if crate::quic::is_connection_closed(&e) => Ok(None),
                Err(e) => Err(TransportError::Quic(format!("{e}"))),
            },
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
            StreamAcceptor::Qmux { uni_rx } => match uni_rx.recv().await {
                Some(stream) => Ok(Some(stream)),
                // accept タスクが終了した = QMux 接続が閉じた
                None => Err(TransportError::ConnectionClosed),
            },
        }
    }
}

/// peer が開始する双方向ストリーム (request stream) を受け入れるためのアクセプター
///
/// MOQT relay は subscriber の SUBSCRIBE / FETCH を publisher 側 session の新しい
/// request stream として転送する (draft-ietf-moq-transport-23 §6.3 (Session initialization))。
/// publisher はこのストリームを受理しなければ要求に応答できない。
pub enum BidiStreamAcceptor {
    /// QUIC 直接接続の双方向ストリーム受理 (`StreamAcceptor` と実体を共有する)
    Quic(Arc<crate::quic::StreamAcceptor>),
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
    /// QMux (draft-ietf-quic-qmux-02)
    ///
    /// `StreamAcceptor::Qmux` と同じく、accept の待機は接続を共有した別タスクが行う
    /// (`qmux::spawn_accept_bidirectional`)。
    Qmux {
        /// 双方向受信ストリームの receiver
        bi_rx: mpsc::UnboundedReceiver<(SendStream, RecvStream)>,
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
                match acceptor.accept_bidirectional_stream().await {
                    Ok((send, recv)) => {
                        Ok(Some((SendStream::Quic(send), RecvStream::new_quic(recv))))
                    }
                    // 接続終了は「これ以上ストリームが来ない」として扱う
                    Err(e) if crate::quic::is_connection_closed(&e) => Ok(None),
                    Err(e) => Err(TransportError::Quic(format!("{e}"))),
                }
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
            BidiStreamAcceptor::Qmux { bi_rx } => match bi_rx.recv().await {
                Some(stream) => Ok(Some(stream)),
                // accept タスクが終了した = QMux 接続が閉じた
                None => Err(TransportError::ConnectionClosed),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HTTP/3 以外の transport は wire のコードをそのまま MOQT のコードとして使う
    ///
    /// QUIC 直接接続は QUIC の code space を MOQT と共有するため remap しない。
    /// QMux も QUIC version 1 のフレーム (STOP_SENDING / RESET_STREAM) をそのまま多重化するため
    /// remap しない (draft-ietf-quic-qmux-02 Section 4)。
    /// WebTransport over HTTP/2 は capsule がアプリケーションエラーコードを直接運ぶため
    /// remap しない (wt-h2 の driver が capsule で受けたコードを MOQT のコードとして使う)。
    /// 32 ビットを超える値も切り詰めない。
    #[test]
    fn remap_stop_sending_error_code_keeps_non_http3_codes() {
        for wire_code in [0, 0x1, 0x1234_5678_9abc] {
            assert_eq!(
                remap_stop_sending_error_code(StopSendingTransport::Quic, wire_code),
                Some(wire_code),
                "QUIC のコードは remap せずそのまま使うこと: {wire_code:#x}"
            );
            assert_eq!(
                remap_stop_sending_error_code(StopSendingTransport::Qmux, wire_code),
                Some(wire_code),
                "QMux のコードは remap せずそのまま使うこと: {wire_code:#x}"
            );
            assert_eq!(
                remap_stop_sending_error_code(StopSendingTransport::WtH2, wire_code),
                Some(wire_code),
                "HTTP/2 の capsule のコードは remap せずそのまま使うこと: {wire_code:#x}"
            );
        }
    }

    /// WebTransport over HTTP/3 は §4.4 の remap を行い、できない値はコード無しになる
    #[test]
    fn remap_stop_sending_error_code_remaps_webtransport_codes() {
        use shiguredo_http3::webtransport::ApplicationErrorCode;
        let http3_code = ApplicationErrorCode::to_http3_code(0x1);
        assert_eq!(
            remap_stop_sending_error_code(StopSendingTransport::WtH3, http3_code),
            Some(0x1),
            "HTTP/3 のコードは MOQT のコードへ戻すこと"
        );
        assert_eq!(
            remap_stop_sending_error_code(StopSendingTransport::WtH3, 0x170d7b68),
            None,
            "WT_APPLICATION_ERROR の範囲外はコード無し (None) になること"
        );
        assert_eq!(
            remap_stop_sending_error_code(StopSendingTransport::WtH3, u64::MAX),
            None,
            "範囲外の上限値もコード無し (None) になること"
        );
    }
}
