//! ngtcp2 をネイティブ QUIC のバックエンドとして使う実装
//!
//! `shiguredo_ngtcp2_tokio` の接続は 1 本のイベントループで駆動する単一所有者の型であり、
//! s2n-quic / tobi-quic のようにストリームごとの非同期ハンドルを持たない。このため、
//! 接続を専用タスクに所有させ、コマンドチャネルで操作を依頼する構成にする。MOQT 層からは
//! 他のバックエンドと同じストリームハンドルとして見える。
//!
//! 受信したストリームデータは、ストリームごとのチャネルへ流す。チャネルは無制限であり、
//! フロー制御のクレジットは受け取った時点で返す。MOQT 層は受理したストリームを速やかに
//! 読むか、同時処理数を絞って読むため、滞留するのは受理待ちの分だけである。
//! s2n-quic / tobi-quic が読み出しまでライブラリ内に保持するのとは違い、ここでは
//! 接続のイベントループを止めないことを優先している。

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
#[cfg(any(test, feature = "test-support"))]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

#[cfg(any(test, feature = "test-support"))]
use base64ct::{Base64, Encoding};
#[cfg(any(test, feature = "test-support"))]
use ngtcp2::ServerConfig;
use ngtcp2::{
    AcceptedConnection, Client, ClientConfig, ClientConnection, ConnectionErrorKind,
    ConnectionEvent, StreamId, TransportParams,
};
use shiguredo_ngtcp2_tokio as ngtcp2;
use tokio::sync::{mpsc, oneshot};

#[cfg(any(test, feature = "test-support"))]
use super::ServerTls;
use super::{ALPN, CloseReason, QuicError, StopSendingObserved, StopSendingReceiver};
use crate::error::{Result, TransportError};

/// 操作を依頼するチャネルの容量
const COMMAND_CHANNEL_CAPACITY: usize = 256;

/// DATAGRAM の最大サイズ (RFC 9221)
///
/// MOQT は音声やメディアの一部を DATAGRAM で運ぶ。QUIC の DATAGRAM は経路 MTU に収まる
/// 必要があるが、上限の広告自体は大きくても害はない。
const MAX_DATAGRAM_SIZE: u64 = 65535;

/// 接続を確立する前の接続
#[derive(Debug)]
pub struct Connected {
    /// 操作を依頼するチャネル
    tx: mpsc::Sender<Command>,
    /// 接続の終了理由 (ドライバが書き込む)
    close_record: Arc<StdMutex<Option<CloseReason>>>,
    /// 接続先のアドレス
    remote_addr: SocketAddr,
    /// ピア起動の単方向ストリームの待ち行列 (ドライバと受理側で共有する)
    uni_incoming: Arc<StreamQueue<RecvStream>>,
    /// ピア起動の双方向ストリームの待ち行列 (ドライバと受理側で共有する)
    bidi_incoming: Arc<StreamQueue<(SendStream, RecvStream)>>,
    /// 受信済み datagram の待ち行列 (ドライバと受信側で共有する)
    datagram_incoming: Arc<StreamQueue<Vec<u8>>>,
}

impl Connected {
    /// 接続を操作ハンドルとストリーム受理ハンドルに分割する
    pub(crate) fn split(self) -> (Connection, StreamAcceptor) {
        (
            Connection {
                tx: self.tx.clone(),
                close_record: Arc::clone(&self.close_record),
                datagram_incoming: self.datagram_incoming,
            },
            StreamAcceptor {
                _keepalive: self.tx,
                uni_incoming: self.uni_incoming,
                bidi_incoming: self.bidi_incoming,
            },
        )
    }

    /// 接続元のアドレスを返す
    pub async fn remote_addr(&self) -> std::result::Result<SocketAddr, QuicError> {
        Ok(self.remote_addr)
    }
}

/// ストリームを開く側の接続ハンドル
#[derive(Debug, Clone)]
pub struct Connection {
    /// 操作を依頼するチャネル
    tx: mpsc::Sender<Command>,
    /// 接続の終了理由 (ドライバが書き込む)
    close_record: Arc<StdMutex<Option<CloseReason>>>,
    /// 受信済み datagram の待ち行列 (ドライバと共有する)
    datagram_incoming: Arc<StreamQueue<Vec<u8>>>,
}

impl Connection {
    /// 新しい単方向ストリームを開く
    ///
    /// ピアが許可した枠が空くまで待つ。
    pub(crate) async fn open_send_stream_wait(&self) -> std::result::Result<SendStream, QuicError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(Command::OpenSend { reply })
            .await
            .map_err(|_| QuicError::ConnectionClosed)?;
        response.await.map_err(|_| QuicError::ConnectionClosed)?
    }

    /// 新しい双方向ストリームを開く
    pub(crate) async fn open_bidirectional_stream_wait(
        &self,
    ) -> std::result::Result<(SendStream, RecvStream), QuicError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(Command::OpenBidi { reply })
            .await
            .map_err(|_| QuicError::ConnectionClosed)?;
        response.await.map_err(|_| QuicError::ConnectionClosed)?
    }

    /// datagram を送信する
    pub(crate) async fn send_datagram_wait(
        &self,
        data: Vec<u8>,
    ) -> std::result::Result<(), QuicError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(Command::SendDatagram { data, reply })
            .await
            .map_err(|_| QuicError::ConnectionClosed)?;
        response.await.map_err(|_| QuicError::ConnectionClosed)?
    }

    /// datagram を 1 つ受信する
    ///
    /// `select!` でこの future が破棄されても、受信済みの datagram は失われない。
    pub(crate) async fn receive_datagram(&self) -> std::result::Result<Vec<u8>, QuicError> {
        self.datagram_incoming
            .recv()
            .await
            .ok_or(QuicError::ConnectionClosed)
    }

    /// 接続が終了するまで待ち、終了理由を返す
    pub async fn closed(&self) -> Option<CloseReason> {
        loop {
            if let Some(reason) = self
                .close_record
                .lock()
                .expect("the close record must not be poisoned")
                .clone()
            {
                return Some(reason);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// 接続をアプリケーションエラーで閉じる
pub(crate) async fn close_application(connection: &Connection, code: u64) -> Result<()> {
    let (reply, response) = oneshot::channel();
    connection
        .tx
        .send(Command::Close { code, reply })
        .await
        .map_err(|_| TransportError::Quic("connection closed".into()))?;
    response
        .await
        .map_err(|_| TransportError::Quic("connection closed".into()))?
        .map_err(|e| TransportError::Quic(e.to_string()))?;
    Ok(())
}

/// 送信ストリーム
#[derive(Debug)]
pub struct SendStream {
    /// ストリーム ID
    id: StreamId,
    /// 操作を依頼するチャネル
    tx: mpsc::Sender<Command>,
}

impl SendStream {
    /// データをすべて送信する
    ///
    /// ngtcp2 の `write_stream` は渡したデータをすべて送信待ちに積むため、部分送信の
    /// 繰り返しは不要である。
    pub(crate) async fn send_all(&mut self, data: &[u8]) -> std::result::Result<(), QuicError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(Command::Send {
                id: self.id,
                data: data.to_vec(),
                reply,
            })
            .await
            .map_err(|_| QuicError::ConnectionClosed)?;
        response.await.map_err(|_| QuicError::ConnectionClosed)?
    }

    /// 送信方向を終了する (FIN)
    pub(crate) async fn finish(&mut self) -> std::result::Result<(), QuicError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(Command::Finish { id: self.id, reply })
            .await
            .map_err(|_| QuicError::ConnectionClosed)?;
        response.await.map_err(|_| QuicError::ConnectionClosed)?
    }

    /// 送信方向を reset する (RESET_STREAM)
    pub(crate) async fn reset(&mut self, error_code: u64) -> std::result::Result<(), QuicError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(Command::Reset {
                id: self.id,
                error_code,
                reply,
            })
            .await
            .map_err(|_| QuicError::ConnectionClosed)?;
        response.await.map_err(|_| QuicError::ConnectionClosed)?
    }

    /// ストリーム ID を返す
    pub(crate) fn id(&self) -> u64 {
        self.id as u64
    }
}

/// 受信ストリームから届くイベント
enum RecvEvent {
    /// 受信データ
    Data(Vec<u8>),
    /// FIN
    Fin,
    /// peer が RESET_STREAM で終端した
    Reset {
        /// peer が載せたアプリケーションエラーコード
        error_code: u64,
    },
}

/// 受信ストリーム
#[derive(Debug)]
pub struct RecvStream {
    /// ストリーム ID
    id: StreamId,
    /// 操作を依頼するチャネル
    tx: mpsc::Sender<Command>,
    /// ドライバが流すイベント
    rx: mpsc::UnboundedReceiver<RecvEvent>,
    /// 終端を観測したか
    ended: bool,
}

impl RecvStream {
    /// ストリーム ID を返す
    pub(crate) fn id(&self) -> u64 {
        self.id as u64
    }

    /// データまたは FIN を受信する
    pub(crate) async fn receive(&mut self) -> std::result::Result<Option<Vec<u8>>, QuicError> {
        if self.ended {
            return Ok(None);
        }
        match self.rx.recv().await {
            Some(RecvEvent::Data(data)) => Ok(Some(data)),
            Some(RecvEvent::Fin) => {
                self.ended = true;
                Ok(None)
            }
            Some(RecvEvent::Reset { error_code }) => {
                self.ended = true;
                Err(QuicError::Reset {
                    error_code,
                    reliable_size: None,
                })
            }
            // ドライバが終了した = 接続が終了した
            None => Err(QuicError::ConnectionClosed),
        }
    }

    /// 受信方向へ STOP_SENDING を送出する
    pub(crate) async fn stop_sending(
        &mut self,
        error_code: u64,
    ) -> std::result::Result<(), QuicError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(Command::StopSending {
                id: self.id,
                error_code,
                reply,
            })
            .await
            .map_err(|_| QuicError::ConnectionClosed)?;
        response.await.map_err(|_| QuicError::ConnectionClosed)?
    }
}

/// ストリームを受理する側のハンドル
#[derive(Debug)]
pub struct StreamAcceptor {
    /// 接続のドライバを生かしておくための送信側
    ///
    /// 受理だけを行う利用者が接続ハンドルを drop しても、ピア起動ストリームの
    /// 受理を続けられるようにする (ドライバは送信側がすべて落ちると終了する)。
    _keepalive: mpsc::Sender<Command>,
    /// ピア起動の単方向ストリームを受け取る待ち行列
    uni_incoming: Arc<StreamQueue<RecvStream>>,
    /// ピア起動の双方向ストリームを受け取る待ち行列
    bidi_incoming: Arc<StreamQueue<(SendStream, RecvStream)>>,
}

impl StreamAcceptor {
    /// ピア起動の単方向ストリームを 1 つ受理する
    ///
    /// `select!` でこの future が破棄されても、受理済みのストリームは失われない。
    pub(crate) async fn accept_receive_stream(&self) -> std::result::Result<RecvStream, QuicError> {
        self.uni_incoming
            .recv()
            .await
            .ok_or(QuicError::ConnectionClosed)
    }

    /// ピア起動の双方向ストリームを 1 つ受理する
    ///
    /// `accept_receive_stream` と同じく、future が破棄されてもストリームは失われない。
    pub(crate) async fn accept_bidirectional_stream(
        &self,
    ) -> std::result::Result<(SendStream, RecvStream), QuicError> {
        self.bidi_incoming
            .recv()
            .await
            .ok_or(QuicError::ConnectionClosed)
    }
}

/// 接続のドライバへ送るコマンド
enum Command {
    /// 単方向ストリームを開く
    OpenSend {
        /// 結果を返す
        reply: oneshot::Sender<std::result::Result<SendStream, QuicError>>,
    },
    /// 双方向ストリームを開く
    OpenBidi {
        /// 結果を返す
        reply: oneshot::Sender<std::result::Result<(SendStream, RecvStream), QuicError>>,
    },
    /// ストリームへデータを積む
    Send {
        /// ストリーム ID
        id: StreamId,
        /// 送信するデータ
        data: Vec<u8>,
        /// 結果を返す
        reply: oneshot::Sender<std::result::Result<(), QuicError>>,
    },
    /// ストリームを FIN で終端する
    Finish {
        /// ストリーム ID
        id: StreamId,
        /// 結果を返す
        reply: oneshot::Sender<std::result::Result<(), QuicError>>,
    },
    /// ストリームを RESET_STREAM で終端する
    Reset {
        /// ストリーム ID
        id: StreamId,
        /// アプリケーションエラーコード
        error_code: u64,
        /// 結果を返す
        reply: oneshot::Sender<std::result::Result<(), QuicError>>,
    },
    /// 受信方向へ STOP_SENDING を送出する
    StopSending {
        /// ストリーム ID
        id: StreamId,
        /// アプリケーションエラーコード
        error_code: u64,
        /// 結果を返す
        reply: oneshot::Sender<std::result::Result<(), QuicError>>,
    },
    /// datagram を送信する
    SendDatagram {
        /// 送信するデータ
        data: Vec<u8>,
        /// 結果を返す
        reply: oneshot::Sender<std::result::Result<(), QuicError>>,
    },
    /// 接続をアプリケーションエラーで閉じる
    Close {
        /// アプリケーションエラーコード
        code: u64,
        /// 結果を返す
        reply: oneshot::Sender<std::result::Result<(), QuicError>>,
    },
}

/// ピアが開いたストリームをドライバからアプリへ引き渡す待ち行列
///
/// oneshot で 1 つの待ち手へ直接渡すと、受け取り側の future が `select!` で捨てられた
/// ときに受理済みのストリームが失われる。`oneshot::Sender::send` は receiver が生きて
/// いれば成功するが、その直後に receiver が drop されると値も捨てられるためである。
/// ストリームはキューに残したまま、アプリが取り出した時点で初めてキューから消すことで、
/// future のキャンセルで失わない。
///
/// 通知は起床の合図に過ぎず、ストリームの有無はキューが持つ。通知を取りこぼしても、
/// 次の確認でキューから取り出せる。
#[derive(Debug)]
struct StreamQueue<T> {
    /// キューと閉鎖状態
    state: StdMutex<StreamQueueState<T>>,
    /// ストリームが積まれたこと、または接続が閉じたことを知らせる
    notify: tokio::sync::Notify,
}

/// 待ち行列の中身
#[derive(Debug)]
struct StreamQueueState<T> {
    /// 未取り出しのストリーム
    values: VecDeque<T>,
    /// 接続が閉じたか
    closed: bool,
}

impl<T> StreamQueue<T> {
    /// 空の待ち行列を作る
    fn new() -> Self {
        Self {
            state: StdMutex::new(StreamQueueState {
                values: VecDeque::new(),
                closed: false,
            }),
            notify: tokio::sync::Notify::new(),
        }
    }

    /// ストリームを積んで待ち手を起こす
    fn push(&self, value: T) {
        self.state
            .lock()
            .expect("the stream queue lock is not poisoned")
            .values
            .push_back(value);
        self.notify.notify_waiters();
    }

    /// 接続の終了を記録して待ち手を起こす
    fn close(&self) {
        self.state
            .lock()
            .expect("the stream queue lock is not poisoned")
            .closed = true;
        self.notify.notify_waiters();
    }

    /// 積まれているストリームを取り出す
    fn pop(&self) -> Option<T> {
        self.state
            .lock()
            .expect("the stream queue lock is not poisoned")
            .values
            .pop_front()
    }

    /// 接続が閉じたかを返す
    fn is_closed(&self) -> bool {
        self.state
            .lock()
            .expect("the stream queue lock is not poisoned")
            .closed
    }

    /// ストリームが積まれるか接続が閉じるまで待つ
    ///
    /// cancel safe である。この future を破棄してもキューからは何も取り出さない。
    async fn recv(&self) -> Option<T> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            // キューを確認する前に待ち手として登録する。確認した直後に push されても
            // `notify_waiters` で起こされる
            notified.as_mut().enable();
            if let Some(value) = self.pop() {
                return Some(value);
            }
            if self.is_closed() {
                return None;
            }
            notified.await;
        }
    }
}

/// ドライバが待つ入力
enum Wake {
    /// 接続のイベント
    Event(ngtcp2::Result<ConnectionEvent>),
    /// アプリケーションからのコマンド
    Command(Option<Command>),
}

/// 接続を所有する側の型
enum Inner {
    /// クライアント接続
    Client(ClientConnection),
    /// サーバーが受理した接続 (テスト専用の QUIC サーバーだけが作る)
    ///
    /// `test-support` feature で `peer_harness` を他クレートへ公開するビルドでも
    /// `Server::accept` が使うため、そのビルドでは dead_code にならない。
    #[cfg_attr(not(any(test, feature = "test-support")), expect(dead_code))]
    Server(AcceptedConnection),
}

impl Inner {
    /// イベントを 1 つ待つ
    async fn recv_event(&mut self) -> ngtcp2::Result<ConnectionEvent> {
        match self {
            Self::Client(conn) => conn.recv_event().await,
            Self::Server(conn) => conn.recv_event().await,
        }
    }

    /// 単方向ストリームを開く
    fn open_uni_stream(&mut self) -> ngtcp2::Result<StreamId> {
        match self {
            Self::Client(conn) => conn.open_uni_stream(),
            Self::Server(conn) => conn.open_uni_stream(),
        }
    }

    /// 双方向ストリームを開く
    fn open_bidi_stream(&mut self) -> ngtcp2::Result<StreamId> {
        match self {
            Self::Client(conn) => conn.open_bidi_stream(),
            Self::Server(conn) => conn.open_bidi_stream(),
        }
    }

    /// ピアが許可した単方向ストリームの残枠を返す
    fn streams_uni_left(&self) -> u64 {
        match self {
            Self::Client(conn) => conn.streams_uni_left(),
            Self::Server(conn) => conn.streams_uni_left(),
        }
    }

    /// ピアが許可した双方向ストリームの残枠を返す
    fn streams_bidi_left(&self) -> u64 {
        match self {
            Self::Client(conn) => conn.streams_bidi_left(),
            Self::Server(conn) => conn.streams_bidi_left(),
        }
    }

    /// ストリームへデータを積む
    fn write_stream(
        &mut self,
        stream_id: StreamId,
        data: &[u8],
        fin: bool,
    ) -> ngtcp2::Result<usize> {
        match self {
            Self::Client(conn) => conn.write_stream(stream_id, data, fin),
            Self::Server(conn) => conn.write_stream(stream_id, data, fin),
        }
    }

    /// ストリームを RESET_STREAM で終端する
    fn reset_stream(&mut self, stream_id: StreamId, error_code: u64) -> ngtcp2::Result<()> {
        match self {
            Self::Client(conn) => conn.reset_stream(stream_id, error_code),
            Self::Server(conn) => conn.reset_stream(stream_id, error_code),
        }
    }

    /// 受信方向へ STOP_SENDING を送出する
    fn stop_sending(&mut self, stream_id: StreamId, error_code: u64) -> ngtcp2::Result<()> {
        match self {
            Self::Client(conn) => conn.stop_sending(stream_id, error_code),
            Self::Server(conn) => conn.stop_sending(stream_id, error_code),
        }
    }

    /// ストリームのフロー制御クレジットを返す
    fn extend_max_stream_offset(
        &mut self,
        stream_id: StreamId,
        consumed: u64,
    ) -> ngtcp2::Result<()> {
        match self {
            Self::Client(conn) => conn.extend_max_stream_offset(stream_id, consumed),
            Self::Server(conn) => conn.extend_max_stream_offset(stream_id, consumed),
        }
    }

    /// datagram を送信する
    async fn send_datagram(&mut self, data: &[u8]) -> ngtcp2::Result<()> {
        match self {
            Self::Client(conn) => conn.send_datagram(data).await,
            Self::Server(conn) => conn.send_datagram(data).await,
        }
    }

    /// 接続をアプリケーションエラーで閉じる
    async fn close(&mut self, error_code: u64, reason: &[u8]) -> ngtcp2::Result<()> {
        match self {
            Self::Client(conn) => conn.close(error_code, reason).await,
            Self::Server(conn) => conn.close(error_code, reason).await,
        }
    }

    /// 接続の統計を返す
    fn stats(&self) -> ngtcp2::ConnStats {
        match self {
            Self::Client(conn) => conn.stats(),
            Self::Server(conn) => conn.stats(),
        }
    }
}

/// ドライバが保持する状態
struct DriverState {
    /// ハンドルを作るための送信側 (すべてのハンドルが落ちたことを知るため弱参照)
    tx: mpsc::WeakSender<Command>,
    /// ストリームごとの受信イベントの送信側
    recv_txs: HashMap<StreamId, mpsc::UnboundedSender<RecvEvent>>,
    /// 受理待ちの単方向ストリーム (アプリと共有する)
    uni_incoming: Arc<StreamQueue<RecvStream>>,
    /// 受理待ちの双方向ストリーム (アプリと共有する)
    bidi_incoming: Arc<StreamQueue<(SendStream, RecvStream)>>,
    /// 単方向ストリームの open 待ち (枠が空くのを待つ)
    open_send_waiters: VecDeque<oneshot::Sender<std::result::Result<SendStream, QuicError>>>,
    /// 双方向ストリームの open 待ち
    open_bidi_waiters:
        VecDeque<oneshot::Sender<std::result::Result<(SendStream, RecvStream), QuicError>>>,
    /// 受信済み datagram の待ち行列 (アプリと共有する)
    datagram_incoming: Arc<StreamQueue<Vec<u8>>>,
    /// ピアが STOP_SENDING で載せたコード (送信 API のエラーに付ける)
    stop_codes: HashMap<StreamId, u64>,
    /// peer の STOP_SENDING を流す先
    stop_tx: mpsc::UnboundedSender<StopSendingObserved>,
    /// 接続の終了理由 (ハンドルが観測する)
    close_record: Arc<StdMutex<Option<CloseReason>>>,
    /// 接続が終了したか
    closed: bool,
}

impl DriverState {
    /// 状態を作る
    fn new(
        tx: mpsc::WeakSender<Command>,
        stop_tx: mpsc::UnboundedSender<StopSendingObserved>,
        close_record: Arc<StdMutex<Option<CloseReason>>>,
        uni_incoming: Arc<StreamQueue<RecvStream>>,
        bidi_incoming: Arc<StreamQueue<(SendStream, RecvStream)>>,
        datagram_incoming: Arc<StreamQueue<Vec<u8>>>,
    ) -> Self {
        Self {
            tx,
            recv_txs: HashMap::new(),
            uni_incoming,
            bidi_incoming,
            open_send_waiters: VecDeque::new(),
            open_bidi_waiters: VecDeque::new(),
            datagram_incoming,
            stop_codes: HashMap::new(),
            stop_tx,
            close_record,
            closed: false,
        }
    }

    /// 送信ストリームのハンドルを作る
    fn send_stream(&self, id: StreamId) -> std::result::Result<SendStream, QuicError> {
        let tx = self.tx.upgrade().ok_or(QuicError::ConnectionClosed)?;
        Ok(SendStream { id, tx })
    }

    /// 受信ストリームのハンドルを作る
    fn recv_stream(&mut self, id: StreamId) -> std::result::Result<RecvStream, QuicError> {
        let tx = self.tx.upgrade().ok_or(QuicError::ConnectionClosed)?;
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        self.recv_txs.insert(id, event_tx);
        Ok(RecvStream {
            id,
            tx,
            rx: event_rx,
            ended: false,
        })
    }

    /// 双方向ストリームのハンドルを作る
    fn bidi_stream(
        &mut self,
        id: StreamId,
    ) -> std::result::Result<(SendStream, RecvStream), QuicError> {
        let send = self.send_stream(id)?;
        let recv = self.recv_stream(id)?;
        Ok((send, recv))
    }

    /// 受信ストリームの終端を通知する
    fn end_recv(&mut self, id: StreamId, event: RecvEvent) {
        if let Some(tx) = self.recv_txs.remove(&id) {
            let _ = tx.send(event);
        }
    }

    /// 枠が空いたストリームの open を再試行する
    ///
    /// 開いたストリームを待ち手へ渡せない場合 (呼び出し側の future が既に破棄されている
    /// 場合) は、その場で閉じる。`SendStream` は drop で終端しないため、閉じずに捨てると
    /// ピアが広告した `MAX_STREAMS` の枠を消費したまま残る。
    fn retry_open_waiters(&mut self, conn: &mut Inner) {
        while !self.open_send_waiters.is_empty() && conn.streams_uni_left() > 0 {
            let Some(waiter) = self.open_send_waiters.pop_front() else {
                break;
            };
            let result = match conn.open_uni_stream() {
                Ok(id) => self.send_stream(id),
                Err(e) => Err(map_stream_error(e, None)),
            };
            if let Err(Ok(stream)) = waiter.send(result) {
                // アプリは 1 バイトも書いていないため NO_ERROR で閉じる
                if let Err(e) = conn.reset_stream(stream.id, 0) {
                    tracing::warn!("Failed to reset an unused outgoing stream: {e}");
                }
            }
        }
        while !self.open_bidi_waiters.is_empty() && conn.streams_bidi_left() > 0 {
            let Some(waiter) = self.open_bidi_waiters.pop_front() else {
                break;
            };
            let result = match conn.open_bidi_stream() {
                Ok(id) => self.bidi_stream(id),
                Err(e) => Err(map_stream_error(e, None)),
            };
            if let Err(Ok((send, recv))) = waiter.send(result) {
                // 単方向と同じ理由で閉じる。受信方向も止めないとピアが送り続ける
                if let Err(e) = conn.reset_stream(send.id, 0) {
                    tracing::warn!("Failed to reset an unused outgoing stream: {e}");
                }
                if let Err(e) = conn.stop_sending(recv.id, 0) {
                    tracing::warn!("Failed to stop an unused incoming stream: {e}");
                }
            }
        }
    }

    /// 接続のイベントを処理する
    fn handle_event(&mut self, conn: &mut Inner, event: ConnectionEvent) {
        match event {
            ConnectionEvent::HandshakeCompleted
            | ConnectionEvent::HandshakeConfirmed
            | ConnectionEvent::EarlyDataRejected
            | ConnectionEvent::PathValidated { .. }
            | ConnectionEvent::StatelessResetReceived
            | ConnectionEvent::StreamMaxData { .. }
            | ConnectionEvent::MaxStreamsBidi { .. }
            | ConnectionEvent::MaxStreamsUni { .. } => {}
            ConnectionEvent::StreamOpened { stream_id } => {
                // ハンドルを作れないのは接続が閉じた場合だけである。その場合も
                // 以後の `recv_event` が終了を返すため、ここでは積むだけにする
                if is_bidirectional(stream_id) {
                    if let Ok(stream) = self.bidi_stream(stream_id) {
                        self.bidi_incoming.push(stream);
                    }
                } else if let Ok(stream) = self.recv_stream(stream_id) {
                    self.uni_incoming.push(stream);
                }
            }
            ConnectionEvent::StreamData {
                stream_id,
                data,
                fin,
            } => {
                let len = data.len() as u64;
                if let Some(tx) = self.recv_txs.get(&stream_id) {
                    let _ = tx.send(RecvEvent::Data(data));
                    if fin {
                        self.end_recv(stream_id, RecvEvent::Fin);
                    } else {
                        // 受け取った時点でクレジットを返す。MOQT 層は速やかに読むため、
                        // 受理待ちのストリームだけが窓を埋める
                        let _ = conn.extend_max_stream_offset(stream_id, len);
                    }
                }
            }
            ConnectionEvent::StreamReset {
                stream_id,
                app_error_code,
                ..
            } => {
                self.end_recv(
                    stream_id,
                    RecvEvent::Reset {
                        error_code: app_error_code,
                    },
                );
            }
            ConnectionEvent::StreamStopSending {
                stream_id,
                app_error_code,
            } => {
                self.stop_codes.insert(stream_id, app_error_code);
                let _ = self.stop_tx.send(StopSendingObserved {
                    stream_id: stream_id as u64,
                    error_code: app_error_code,
                });
            }
            ConnectionEvent::StreamClosed { stream_id, .. } => {
                // FIN / RESET_STREAM の観測で送信側は既に閉じている
                self.recv_txs.remove(&stream_id);
            }
            ConnectionEvent::Datagram { data } => {
                self.datagram_incoming.push(data);
            }
            ConnectionEvent::ConnectionClosed {
                error_code, kind, ..
            } => {
                self.mark_closed(close_reason(kind, error_code));
            }
        }
    }

    /// コマンドを処理する
    ///
    /// 戻り値はドライバを続けるかどうか。
    async fn handle_command(&mut self, conn: &mut Inner, command: Command) -> bool {
        match command {
            Command::OpenSend { reply } => {
                self.open_send_waiters.push_back(reply);
                self.retry_open_waiters(conn);
            }
            Command::OpenBidi { reply } => {
                self.open_bidi_waiters.push_back(reply);
                self.retry_open_waiters(conn);
            }
            Command::Send { id, data, reply } => {
                let result = conn
                    .write_stream(id, &data, false)
                    .map(|_| ())
                    .map_err(|e| map_stream_error(e, self.stop_codes.get(&id).copied()));
                let _ = reply.send(result);
            }
            Command::Finish { id, reply } => {
                let result = conn
                    .write_stream(id, &[], true)
                    .map(|_| ())
                    .map_err(|e| map_stream_error(e, self.stop_codes.get(&id).copied()));
                let _ = reply.send(result);
            }
            Command::Reset {
                id,
                error_code,
                reply,
            } => {
                let result = conn
                    .reset_stream(id, error_code)
                    .map_err(|e| map_stream_error(e, self.stop_codes.get(&id).copied()));
                let _ = reply.send(result);
            }
            Command::StopSending {
                id,
                error_code,
                reply,
            } => {
                let result = conn
                    .stop_sending(id, error_code)
                    .map_err(|e| map_stream_error(e, self.stop_codes.get(&id).copied()));
                if result.is_ok() {
                    self.end_recv(id, RecvEvent::Fin);
                }
                let _ = reply.send(result);
            }
            Command::SendDatagram { data, reply } => {
                let result = conn.send_datagram(&data).await.map_err(map_error);
                let _ = reply.send(result);
            }
            Command::Close { code, reply } => {
                let result = conn.close(code, &[]).await.map_err(map_error);
                self.mark_closed(Some(CloseReason::Application { code }));
                let _ = reply.send(result);
                return false;
            }
        }
        true
    }

    /// 接続が終了したことを記録する
    fn mark_closed(&mut self, reason: Option<CloseReason>) {
        self.closed = true;
        let mut record = self
            .close_record
            .lock()
            .expect("the close record must not be poisoned");
        if record.is_none() {
            *record =
                Some(reason.unwrap_or_else(|| CloseReason::Other("connection closed".into())));
        }
    }

    /// 待機中の呼び出し元をすべて接続終了で終わらせる
    fn finish(&mut self) {
        let error = QuicError::ConnectionClosed;
        // 受理待ちのアプリへ接続終了を知らせる。積まれたストリームはアプリが
        // 取り出せるが、取り出したあとの読み出しは接続終了で失敗する
        self.uni_incoming.close();
        self.bidi_incoming.close();
        for waiter in self.open_send_waiters.drain(..) {
            let _ = waiter.send(Err(error.clone()));
        }
        for waiter in self.open_bidi_waiters.drain(..) {
            let _ = waiter.send(Err(error.clone()));
        }
        self.datagram_incoming.close();
        self.recv_txs.clear();

        let mut record = self
            .close_record
            .lock()
            .expect("the close record must not be poisoned");
        if record.is_none() {
            *record = Some(CloseReason::Other("connection closed".into()));
        }
    }
}

/// 接続のドライバを起動する
fn spawn_driver(
    conn: Inner,
    rx: mpsc::Receiver<Command>,
    tx: mpsc::WeakSender<Command>,
    stop_tx: mpsc::UnboundedSender<StopSendingObserved>,
    close_record: Arc<StdMutex<Option<CloseReason>>>,
    uni_incoming: Arc<StreamQueue<RecvStream>>,
    bidi_incoming: Arc<StreamQueue<(SendStream, RecvStream)>>,
    datagram_incoming: Arc<StreamQueue<Vec<u8>>>,
) {
    tokio::spawn(async move {
        let mut conn = conn;
        let mut rx = rx;
        let mut state = DriverState::new(
            tx,
            stop_tx,
            close_record,
            uni_incoming,
            bidi_incoming,
            datagram_incoming,
        );
        let mut last_diag = Instant::now();
        loop {
            state.retry_open_waiters(&mut conn);
            // コマンドと接続イベントのどちらかを待つ。コマンドが届いたときに
            // `recv_event` の future を破棄するが、受信済みのパケットは処理済みであり、
            // 送信途中のパケットは損失回復が再送する
            let wake = tokio::select! {
                result = conn.recv_event(), if !state.closed => Wake::Event(result),
                command = rx.recv() => Wake::Command(command),
            };
            match wake {
                Wake::Event(Ok(event)) => state.handle_event(&mut conn, event),
                Wake::Event(Err(error)) => {
                    state.mark_closed(Some(CloseReason::Other(error.to_string())));
                    break;
                }
                Wake::Command(Some(command)) => {
                    if !state.handle_command(&mut conn, command).await {
                        break;
                    }
                }
                // アプリケーション側のハンドルがすべて落ちた
                Wake::Command(None) => break,
            }
            if state.closed {
                break;
            }
            if crate::quic::packet_diag_enabled() && last_diag.elapsed() >= Duration::from_secs(1) {
                last_diag = Instant::now();
                let stats = conn.stats();
                tracing::warn!(
                    "PKTDIAG received={} lost={}",
                    stats.packets_received,
                    stats.packets_lost,
                );
            }
        }
        state.finish();
    });
}

/// ストリーム ID が双方向ストリームのものかを返す (RFC 9000 §2.1)
fn is_bidirectional(stream_id: StreamId) -> bool {
    (stream_id as u64) & 0x2 == 0
}

/// ストリーム API のエラーを共通の QUIC エラーへ写す
fn map_stream_error(error: ngtcp2::Error, stop_code: Option<u64>) -> QuicError {
    match error {
        ngtcp2::Error::ConnectionClosed => QuicError::ConnectionClosed,
        // ピアの STOP_SENDING で送信できなくなった。コードは観測済みの値を使う
        ngtcp2::Error::StreamShutWr(_) => QuicError::Stopped {
            error_code: stop_code.unwrap_or(0),
        },
        e => QuicError::Other(e.to_string()),
    }
}

/// 接続のエラーを共通の QUIC エラーへ写す
fn map_error(error: ngtcp2::Error) -> QuicError {
    match error {
        ngtcp2::Error::ConnectionClosed => QuicError::ConnectionClosed,
        e => QuicError::Other(e.to_string()),
    }
}

/// 接続終了イベントを終了理由へ写す
fn close_reason(kind: ConnectionErrorKind, error_code: u64) -> Option<CloseReason> {
    match kind {
        ConnectionErrorKind::ApplicationClose => {
            if error_code == 0 {
                Some(CloseReason::Normal)
            } else {
                Some(CloseReason::Application { code: error_code })
            }
        }
        ConnectionErrorKind::TransportClose => {
            if error_code == 0 {
                Some(CloseReason::Normal)
            } else {
                Some(CloseReason::Other(format!(
                    "transport error: code={error_code:#x}"
                )))
            }
        }
        ConnectionErrorKind::Terminal => Some(CloseReason::Other("connection terminated".into())),
        ConnectionErrorKind::SilentDrop | ConnectionErrorKind::Internal => {
            Some(CloseReason::Other("connection dropped".into()))
        }
        ConnectionErrorKind::Ignore => None,
    }
}

/// 接続で広告する transport parameter を構築する
fn build_transport_params() -> TransportParams {
    TransportParams::new()
        // ピア (relay) が開ける単方向 stream の累積上限
        .with_max_streams_uni(10_000)
        // ピア (relay) が開ける双方向 stream の累積上限
        .with_max_streams_bidi(1_000)
        // 接続レベルで受け入れを許す総バイト数
        .with_initial_max_data(64 * 1024 * 1024)
        // 単方向 stream (relay -> subscriber の data stream) 1 本あたりの窓
        .with_initial_max_stream_data_uni(16 * 1024 * 1024)
        .with_initial_max_stream_data_bidi_local(16 * 1024 * 1024)
        .with_initial_max_stream_data_bidi_remote(16 * 1024 * 1024)
        // s2n-quic の既定 (30 秒) に合わせる
        .with_max_idle_timeout(Duration::from_secs(30))
}

/// DATAGRAM の設定を作る
fn datagram_config() -> ngtcp2::DatagramConfig {
    ngtcp2::DatagramConfig {
        max_datagram_frame_size: MAX_DATAGRAM_SIZE,
        max_tx_datagram_size: MAX_DATAGRAM_SIZE as usize,
    }
}

/// QUIC クライアント接続を確立する
pub(crate) async fn connect(
    socket_addr: SocketAddr,
    server_name: &str,
    cert_path: Option<&str>,
) -> Result<(Connected, StopSendingReceiver)> {
    let mut config = ClientConfig::new(&[ALPN])
        .with_transport_params(build_transport_params())
        .with_datagram(datagram_config());
    match cert_path {
        Some(path) => {
            let pem = std::fs::read_to_string(path).map_err(|e| {
                TransportError::Quic(format!("failed to read certificate '{path}': {e}"))
            })?;
            config = config.with_verify_peer(true).with_ca_cert_pem(pem);
        }
        None => {
            // 開発用: 証明書検証をスキップする
            tracing::warn!("TLS certificate verification is disabled (development mode)");
            config = config.with_verify_peer(false);
        }
    }

    let local_addr = crate::local_bind_addr(socket_addr);
    let connection = Client::connect_with_config(socket_addr, local_addr, server_name, &config)
        .await
        .map_err(|e| TransportError::Quic(format!("connection failed: {e}")))?;

    let selected = connection.selected_alpn_protocol();
    super::verify_alpn(selected.as_deref())?;

    let remote_addr = connection.remote_addr();
    let (tx, rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    let (stop_tx, stop_rx) = mpsc::unbounded_channel();
    let close_record = Arc::new(StdMutex::new(None));
    let uni_incoming = Arc::new(StreamQueue::new());
    let bidi_incoming = Arc::new(StreamQueue::new());
    let datagram_incoming = Arc::new(StreamQueue::new());
    spawn_driver(
        Inner::Client(connection),
        rx,
        tx.downgrade(),
        stop_tx,
        Arc::clone(&close_record),
        Arc::clone(&uni_incoming),
        Arc::clone(&bidi_incoming),
        Arc::clone(&datagram_incoming),
    );

    tracing::info!("Connected to {socket_addr}");
    Ok((
        Connected {
            tx,
            close_record,
            remote_addr,
            uni_incoming,
            bidi_incoming,
            datagram_incoming,
        },
        stop_rx,
    ))
}

/// テスト用の QUIC サーバー
#[cfg(any(test, feature = "test-support"))]
pub struct Server {
    /// ngtcp2 のサーバー
    inner: ngtcp2::Server,
    /// 一時的に書き出した証明書のパス
    cert_path: std::path::PathBuf,
    /// 一時的に書き出した秘密鍵のパス
    key_path: std::path::PathBuf,
}

#[cfg(any(test, feature = "test-support"))]
impl Server {
    /// 待ち受けているアドレスを返す
    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.inner.local_addr()
    }

    /// クライアントの接続を 1 つ受理する
    pub(crate) async fn accept(&mut self) -> Result<Connected> {
        let connection = self
            .inner
            .accept()
            .await
            .map_err(|e| {
                TransportError::Quic(format!("server failed to accept a connection: {e}"))
            })?
            .ok_or_else(|| TransportError::Quic("server stopped before connection".into()))?;
        #[cfg(any(test, feature = "test-support"))]
        let remote_addr = connection.remote_addr();
        let (tx, rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        let (stop_tx, _stop_rx) = mpsc::unbounded_channel();
        let close_record = Arc::new(StdMutex::new(None));
        let uni_incoming = Arc::new(StreamQueue::new());
        let bidi_incoming = Arc::new(StreamQueue::new());
        let datagram_incoming = Arc::new(StreamQueue::new());
        spawn_driver(
            Inner::Server(connection),
            rx,
            tx.downgrade(),
            stop_tx,
            Arc::clone(&close_record),
            Arc::clone(&uni_incoming),
            Arc::clone(&bidi_incoming),
            Arc::clone(&datagram_incoming),
        );
        Ok(Connected {
            tx,
            close_record,
            remote_addr,
            uni_incoming,
            bidi_incoming,
            datagram_incoming,
        })
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for Server {
    fn drop(&mut self) {
        // 一時ファイルを残さない
        let _ = std::fs::remove_file(&self.cert_path);
        let _ = std::fs::remove_file(&self.key_path);
    }
}

/// テスト用の QUIC サーバーを起動する
#[cfg(any(test, feature = "test-support"))]
pub(crate) async fn start_server(tls: ServerTls, bind_addr: SocketAddr) -> Result<Server> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let unique = format!(
        "moqt-ngtcp2-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let dir = std::env::temp_dir();
    let cert_path = dir.join(format!("{unique}-cert.pem"));
    let key_path = dir.join(format!("{unique}-key.pem"));
    std::fs::write(&cert_path, der_to_pem("CERTIFICATE", &tls.cert_der)).map_err(|e| {
        TransportError::Internal(format!("failed to write the server certificate: {e}"))
    })?;
    std::fs::write(&key_path, der_to_pem("PRIVATE KEY", &tls.key_der))
        .map_err(|e| TransportError::Internal(format!("failed to write the server key: {e}")))?;

    let alpn: Vec<&[u8]> = tls
        .alpn
        .iter()
        .map(|protocol| protocol.as_slice())
        .collect();
    let config = ServerConfig::new(&alpn)
        .with_transport_params(build_transport_params())
        .with_datagram(datagram_config());
    let server = ngtcp2::Server::bind(bind_addr, &cert_path, &key_path, Some(config))
        .await
        .map_err(|e| TransportError::Internal(format!("failed to start server: {e}")))?;

    Ok(Server {
        inner: server,
        cert_path,
        key_path,
    })
}

/// DER を PEM 形式へ変換する
#[cfg(any(test, feature = "test-support"))]
fn der_to_pem(label: &str, der: &[u8]) -> String {
    let encoded = Base64::encode_string(der);
    let mut pem = String::with_capacity(encoded.len() + 64);
    pem.push_str("-----BEGIN ");
    pem.push_str(label);
    pem.push_str("-----\n");
    for chunk in encoded.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).expect("base64 output is ASCII"));
        pem.push('\n');
    }
    pem.push_str("-----END ");
    pem.push_str(label);
    pem.push_str("-----\n");
    pem
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// 待ち手が破棄されても、その後に積まれたストリームが失われないこと
    ///
    /// 受理待ちの future は `select!` / `timeout` で頻繁に破棄される。破棄のたびに
    /// キューから何か取り出していれば、そのストリームは永久に失われる。
    #[tokio::test]
    async fn cancelled_waiter_does_not_consume_a_stream() {
        let queue: StreamQueue<u64> = StreamQueue::new();

        // 待ち手を作って破棄する
        let cancelled = tokio::time::timeout(Duration::from_millis(10), queue.recv()).await;
        assert!(
            cancelled.is_err(),
            "積まれる前の受理待ちは時間切れになること"
        );

        // 破棄後に積まれた値は取り出せる
        queue.push(1);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), queue.recv())
                .await
                .expect("破棄後に積まれた値を取り出せること"),
            Some(1),
            "破棄された待ち手が値を消費していないこと"
        );
    }

    /// 積まれた順に取り出せること
    #[tokio::test]
    async fn queue_preserves_order() {
        let queue: StreamQueue<u64> = StreamQueue::new();
        queue.push(1);
        queue.push(2);
        queue.push(3);
        for expected in [1, 2, 3] {
            assert_eq!(
                queue.recv().await,
                Some(expected),
                "積まれた順に取り出せること"
            );
        }
    }

    /// 接続が閉じたら、待ち手は接続終了として終わること
    #[tokio::test]
    async fn close_wakes_the_waiter() {
        let queue: StreamQueue<u64> = StreamQueue::new();
        let recv = queue.recv();
        tokio::pin!(recv);

        // 待ち手を登録してから閉じる
        assert!(
            tokio::time::timeout(Duration::from_millis(10), recv.as_mut())
                .await
                .is_err(),
            "閉じる前の受理待ちは時間切れになること"
        );
        queue.close();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), queue.recv())
                .await
                .expect("閉鎖で受理待ちが終わること"),
            None,
            "接続終了として None を返すこと"
        );
    }

    /// 閉鎖後に積まれていた値は取り出してから終わること
    ///
    /// 閉鎖の直前に受理した stream を捨てると、ピアが送ったデータを読む機会を失う。
    #[tokio::test]
    async fn close_keeps_queued_streams() {
        let queue: StreamQueue<u64> = StreamQueue::new();
        queue.push(7);
        queue.close();
        assert_eq!(
            queue.recv().await,
            Some(7),
            "閉鎖後も積まれていた値を取り出せること"
        );
        assert_eq!(queue.recv().await, None, "取り出した後は接続終了になること");
    }
}
