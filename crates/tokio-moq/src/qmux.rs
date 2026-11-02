//! QMux (draft-ietf-quic-qmux-02) 経由の接続確立
//!
//! 順序保証された 1 本の双方向バイトストリーム (TLS over TCP) の上で、QMux が提供する
//! QUIC version 1 互換のストリームと DATAGRAM を使う。QMux 自体は暗号化も ALPN の
//! ネゴシエーションも行わないため、両方をこのモジュールが張る。
//!
//! QMux 固有の処理 (TCP + TLS の接続、[`Config`] の組み立て、accept を流すタスク、
//! STOP_SENDING の詰め替え、ピアの `max_datagram_frame_size` の確認) はここに閉じ込め、
//! `transport.rs` の enum と `moqt_client.rs` からは腕で参照する。qmux-rs の API が
//! 変わったときに追随する範囲を局所にするためである。
//!
//! 経路の位置づけ: draft-ietf-moq-transport-22 §6.2 は「TCP など他のトランスポート上で
//! 直接動かす方法は定義しない」と明記しており、QMux 上の MOQT を扱うのは
//! `refs/moq/draft-nandakumar-moq-qmux-moqt-00.txt` (individual draft) である。同 draft は
//! MOQT draft-16 を前提としており、control stream を QMux の双方向 stream 0 とする点
//! (Section 4.2) は draft-ietf-moq-transport-22 §6.4.1 (control stream は単方向ストリーム)
//! と矛盾する。本実装は QMux が QUIC version 1 のストリームを多重化する性質に従い、
//! ストリームを 1 対 1 に対応させる (individual draft Section 4.2 は採らない)。

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use shiguredo_qmux::codec::role::Role;
use shiguredo_qmux::codec::time::Duration;
use shiguredo_qmux::config::Config;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::TlsConnector;

use crate::error::{Result, TransportError};
use crate::quic::{StopSendingObserved, StopSendingReceiver};
use crate::transport::{RecvStream, SendStream};

/// TLS の ALPN に載せる識別子
///
/// QMux 上の MOQT は native QUIC と同じ識別子を使い、新しい ALPN を定義しない
/// (`refs/moq/draft-nandakumar-moq-qmux-moqt-00.txt` Section 3.2。下位トランスポートは
/// ALPN ではなく接続そのもので区別する)。
///
/// 一方 draft-ietf-quic-qmux-02 Section 8.1 は「QMux へのマッピングの識別子は同一
/// プロトコルの QUIC マッピングと異なるものでなければならない (SVCB との互換のため)」と
/// MUST で定めるため、この選択は同節から外れる。MOQT 側に QMux 用の登録が無く
/// (draft-ietf-moq-transport-22 §16 の ALPN registry は未整備)、SVCB 互換レコードを
/// 定義する draft も未整備である (同 §6.1.2 の TODO) ことから interop を優先する。
/// 両 draft が決着したら見直すこと。
const ALPN: &[u8] = crate::MOQT_PROTOCOL.as_bytes();

/// 受信ストリームの読み出しに使うバッファ長
///
/// tokio-qmux の `RecvStream::read` は呼び出し側のバッファへ書くため、読み出しごとの確保と
/// ゼロ埋めを避けてストリームごとに 1 本持つ。収まらない Object は呼び出し側が繰り返し読む。
/// QUIC 直接接続の per-stream の受信窓 (16 MiB) より十分小さいため、多数のストリームを
/// 同時に保持しても `Arc` + 1 本のバッファで収まる。
pub(crate) const READ_BUFFER_SIZE: usize = 64 * 1024;

/// QMux 接続で広告する `max_datagram_frame_size`
///
/// 1 Record の上限 (`max_record_size`) の既定に合わせる。qmux-rs の
/// `Connection::send_datagram` は DATAGRAM フレーム全体 (Type 1 バイト + Length + payload) を
/// ピアの `max_datagram_frame_size` と `max_record_size` の両方と比較するため、Record の
/// 既定と同じ値にすると既定の Record に収まる DATAGRAM を受け入れられる。
/// moq-pub の `--datagram-max-size` の既定 1160 の OBJECT_DATAGRAM はフレーム全体で
/// 1163 バイトになるため、これも満たす。
const MAX_DATAGRAM_FRAME_SIZE: u64 = shiguredo_qmux::record::DEFAULT_MAX_RECORD_SIZE;

/// QMux 接続のハンドル
///
/// `transport.rs` の `StreamHandle::Qmux` が持つ。破棄された受信 DATAGRAM の通知を
/// 1 度だけログへ出すための記録をあわせて持つ (QMux は速やかに配送できない DATAGRAM の
/// 破棄を許すため、取りこぼしを追えるようにする)。
#[derive(Debug)]
pub struct QmuxHandle {
    /// ストリームの open / DATAGRAM の送信 / 接続の close に使う
    connection: Arc<tokio_qmux::Connection>,
    /// 最後にログへ出した破棄数
    logged_dropped_datagrams: AtomicU64,
}

impl QmuxHandle {
    /// 接続を返す
    pub(crate) fn connection(&self) -> &tokio_qmux::Connection {
        &self.connection
    }

    /// 接続が閉じるまで待ち、閉じた理由を返す
    ///
    /// 自側の close、ピアの CONNECTION_CLOSE、下位トランスポートの終了のいずれでも解決する。
    /// close の後にドライバと受理タスクが終了したことを確かめるために使う。
    #[cfg(test)]
    pub(crate) async fn closed(&self) -> shiguredo_qmux::event::CloseReason {
        self.connection.closed().await
    }

    /// 破棄された受信 DATAGRAM が増えていればログへ出す
    pub(crate) fn log_dropped_datagrams(&self) {
        let dropped = self.connection.dropped_datagrams();
        let previous = self
            .logged_dropped_datagrams
            .swap(dropped, Ordering::Relaxed);
        if dropped > previous {
            tracing::warn!(
                "Dropped {} received QMux datagram(s) (total {dropped}): the receive buffer overflowed",
                dropped - previous
            );
        }
    }
}

/// 接続確立済みの QMux 接続と、そこから生まれる受け入れ口
///
/// [`connect`] が作り、`MoqtClient::establish_qmux` が制御ストリームを取り出してから
/// 残りを `StreamAcceptor` / `BidiStreamAcceptor` へ渡す。
pub struct QmuxSession {
    /// ストリームの open と DATAGRAM、close に使うハンドル
    pub(crate) handle: Arc<QmuxHandle>,
    /// peer 起動の単方向ストリーム (先頭が制御ストリーム)
    pub(crate) uni_rx: mpsc::UnboundedReceiver<RecvStream>,
    /// peer 起動の双方向ストリーム
    pub(crate) bi_rx: mpsc::UnboundedReceiver<(SendStream, RecvStream)>,
    /// peer の STOP_SENDING の観測
    pub(crate) stop_sending_rx: StopSendingReceiver,
}

/// QMux 接続を確立する
///
/// 名前解決とアドレスのフォールバックは [`crate::connect_with_fallback`] が済ませており、
/// ここは解決済みの 1 つのアドレスへ TCP を張るところから始める。`--cert` を省略した場合は
/// 証明書検証を無効化する (WebTransport over HTTP/2 経路と同じ扱い)。
///
/// # Errors
///
/// TCP 接続、TLS ハンドシェイク、QMux のトランスポートパラメータ検証のいずれかに
/// 失敗した場合に [`TransportError`] になる。
pub(crate) async fn connect(
    socket_addr: SocketAddr,
    server_name: &str,
    cert: Option<&str>,
    task_monitor: &tokio_metrics::TaskMonitor,
) -> Result<QmuxSession> {
    // TCP。QMux は 1 本のバイトストリームを多重化するため、Nagle による遅延が全ストリームに
    // 乗る。音声の datagram を 20 ms ごとに送るため無効化する。
    //
    // tokio-qmux は接続確立前の I/O エラーを表す variant を持たない (接続後の I/O エラーは
    // 下位トランスポートの閉塞として `Connection::closed` の `CloseReason::TransportClosed`
    // で通知する)。接続確立前の失敗は他経路と同じ `TransportError::Transport` に写す。
    let tcp = TcpStream::connect(socket_addr)
        .await
        .map_err(TransportError::transport)?;
    tcp.set_nodelay(true).map_err(TransportError::transport)?;

    // TLS。CA 証明書の読み込みと検証の有無は WebTransport over HTTP/2 経路と同じ扱いにする。
    let ca_cert_pem = match cert {
        Some(path) => Some(std::fs::read_to_string(path).map_err(|e| {
            TransportError::Internal(format!("failed to read certificate '{path}': {e}"))
        })?),
        None => {
            tracing::warn!("TLS certificate verification is disabled (development mode)");
            None
        }
    };
    let tls_config = crate::tls::build_client_config(ca_cert_pem.as_deref(), false, ALPN)?;
    let server_name = rustls::pki_types::ServerName::try_from(server_name.to_string())
        .map_err(|e| TransportError::Internal(format!("invalid server name: {e}")))?;
    let tls = TlsConnector::from(tls_config)
        .connect(server_name, tcp)
        .await
        .map_err(TransportError::transport)?;

    // draft-ietf-quic-qmux-02 Section 8.1 は QMux の client に「ALPN 交渉に失敗した場合は
    // ハンドシェイクを中断する」ことを MUST で求める。rustls が ALPN 未交渉でハンドシェイクを
    // 失敗させるのは QUIC のときだけであり、TCP + TLS では成功してしまうため、ここで確認する。
    // ハンドシェイクは完了しているため alert は送れず、接続を閉じるだけになる
    if tls.get_ref().1.alpn_protocol() != Some(ALPN) {
        return Err(TransportError::Internal(format!(
            "the server did not negotiate the MOQT ALPN ({}); it is not a QMux peer",
            crate::MOQT_PROTOCOL
        )));
    }

    // QMux。`Connection::new` は送信できない設定の場合にエラーを返す。
    let connection = Arc::new(tokio_qmux::Connection::new(
        Role::Client,
        tls,
        build_config(),
    )?);

    Ok(split_connection(connection, task_monitor))
}

/// 確立済みの QMux 接続から、MOQT 層が使う資源を組み立てる
///
/// accept 系と STOP_SENDING の待ちは `&self` を取るため、接続を共有した別タスクで待ち、
/// 結果をチャネルへ流す。client は [`connect`] から呼び、テストの peer は接続を作った側から
/// 呼ぶ (client / server で扱いは同じ)。
pub(crate) fn split_connection(
    connection: Arc<tokio_qmux::Connection>,
    task_monitor: &tokio_metrics::TaskMonitor,
) -> QmuxSession {
    // 受理チャネルは上限を設けない。tokio-qmux の受理チャネルは容量 16 だが、あふれた
    // ストリームはドライバが保持し、空きができ次第配送する (失われない)。ここで上限を
    // 設けると受理タスクが channel の空き待ちで止まり、制御ストリームや data stream の
    // 引き取りが遅れるため、上限なしで即座に流す。受信量は QMux のフロー制御で抑えられる。
    let (uni_tx, uni_rx) = mpsc::unbounded_channel();
    let (bi_tx, bi_rx) = mpsc::unbounded_channel();
    let (stop_sending_tx, stop_sending_rx) = mpsc::unbounded_channel();
    spawn_accept_unidirectional(Arc::clone(&connection), uni_tx, task_monitor);
    spawn_accept_bidirectional(Arc::clone(&connection), bi_tx, task_monitor);
    spawn_stop_sending(Arc::clone(&connection), stop_sending_tx, task_monitor);

    let handle = Arc::new(QmuxHandle {
        connection,
        logged_dropped_datagrams: AtomicU64::new(0),
    });
    QmuxSession {
        handle,
        uni_rx,
        bi_rx,
        stop_sending_rx,
    }
}

/// テスト用に QMux 接続を組み立てる (server 役)
///
/// テストの peer は client と同じ設定を使う。ストリーム ID の parity は `Role` で決まるため、
/// server 役は [`Role::Server`] を渡す。設定を共有するためここに置く。
#[cfg(test)]
pub(crate) fn build_server_connection<T>(io: T) -> tokio_qmux::Result<Arc<tokio_qmux::Connection>>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    Ok(Arc::new(tokio_qmux::Connection::new(
        Role::Server,
        io,
        build_config(),
    )?))
}

/// QMux 接続の設定を組み立てる
///
/// `Config::default()` はフロー制御のクレジットがすべて 0 で DATAGRAM も無効なため、
/// ストリームと DATAGRAM を使うには明示する。同時実行数と窓の値は QUIC 直接接続の
/// `quic::build_limits` に相当する量を写す (moq-sub は同時に多数の subgroup ストリームを
/// 受けるため、QMux の既定である 0 では足りない)。
///
/// `refs/moq/draft-nandakumar-moq-qmux-moqt-00.txt` Section 3.3 の推奨値 (接続 1 MiB、
/// per-stream 256 KiB、stream 数 100) は moq-sub の要求に対して小さく、そのままは使わない。
/// `max_idle_timeout` だけは同節の推奨値 (30000 ms) を使う。
fn build_config() -> Config {
    Config {
        max_idle_timeout: Duration::from_secs(30),
        initial_max_data: 64 * 1024 * 1024,
        initial_max_stream_data_bidi_local: 16 * 1024 * 1024,
        initial_max_stream_data_bidi_remote: 16 * 1024 * 1024,
        initial_max_stream_data_uni: 16 * 1024 * 1024,
        initial_max_streams_bidi: 1_000,
        initial_max_streams_uni: 10_000,
        max_datagram_frame_size: Some(MAX_DATAGRAM_FRAME_SIZE),
        // max_record_size は既定値 (16382) のまま使う
        ..Config::default()
    }
}

/// peer 起動の単方向ストリームを受理して receiver へ流すタスクを起動する
///
/// accept の待機は接続を共有して別タスクで行う。tokio-qmux は受理待ちのストリームを
/// ドライバ内に保持するため、引き取りが遅れてもストリームは失われない。
fn spawn_accept_unidirectional(
    connection: Arc<tokio_qmux::Connection>,
    tx: mpsc::UnboundedSender<RecvStream>,
    task_monitor: &tokio_metrics::TaskMonitor,
) {
    let monitor = task_monitor.clone();
    tokio::spawn(monitor.instrument(async move {
        loop {
            let accepted = tokio::select! {
                biased;
                // アプリがアクセプターを drop したら止める。タスクが接続を握り続けると、
                // close されないままアプリが終了した場合にドライバが動き続ける
                // (tokio-qmux のドライバは close 後は linger の上限つきで終了する)
                _ = tx.closed() => break,
                // 自側 / ピアの close を観測したら止める
                _ = connection.closed() => break,
                accepted = connection.accept_unidirectional() => accepted,
            };
            match accepted {
                Ok(Some(stream)) => {
                    // 受け手が居なくなった (アプリの終了) ら止める
                    if tx.send(RecvStream::new_qmux(stream)).is_err() {
                        break;
                    }
                }
                // 接続が閉じた (channel が閉じるため、待ち手は ConnectionClosed を得る)
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("Failed to accept a QMux unidirectional stream: {e}");
                    break;
                }
            }
        }
    }));
}

/// peer 起動の双方向ストリームを受理して receiver へ流すタスクを起動する
fn spawn_accept_bidirectional(
    connection: Arc<tokio_qmux::Connection>,
    tx: mpsc::UnboundedSender<(SendStream, RecvStream)>,
    task_monitor: &tokio_metrics::TaskMonitor,
) {
    let monitor = task_monitor.clone();
    tokio::spawn(monitor.instrument(async move {
        loop {
            let accepted = tokio::select! {
                biased;
                // アプリがアクセプターを drop したら止める (accept のタスクと同じ理由)
                _ = tx.closed() => break,
                _ = connection.closed() => break,
                accepted = connection.accept_bidirectional() => accepted,
            };
            match accepted {
                Ok(Some((send, recv))) => {
                    let stream = (SendStream::Qmux(send), RecvStream::new_qmux(recv));
                    if tx.send(stream).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("Failed to accept a QMux bidirectional stream: {e}");
                    break;
                }
            }
        }
    }));
}

/// ピアの STOP_SENDING を接続単位で観測し、QUIC 経路と同じチャネルへ流すタスクを起動する
///
/// `SendStream::stopped` は送信と同時に await できない (同じストリームの可変借用が
/// 重なる) ため、接続単位の `Connection::recv_stop_sending` を使う。
/// `crate::quic::StopSendingObserved` へ詰め替えることで、`MoqtClient::handle_stop_sending`
/// が QUIC 経路と同じ処理を行う。
fn spawn_stop_sending(
    connection: Arc<tokio_qmux::Connection>,
    tx: mpsc::UnboundedSender<StopSendingObserved>,
    task_monitor: &tokio_metrics::TaskMonitor,
) {
    let monitor = task_monitor.clone();
    tokio::spawn(monitor.instrument(async move {
        loop {
            let observed = tokio::select! {
                biased;
                // アプリが receiver を drop したら止める (accept のタスクと同じ理由)
                _ = tx.closed() => break,
                _ = connection.closed() => break,
                observed = connection.recv_stop_sending() => observed,
            };
            match observed {
                Ok(Some(observed)) => {
                    let observed = StopSendingObserved {
                        stream_id: observed.id.as_u64(),
                        error_code: observed.error_code,
                    };
                    if tx.send(observed).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("Failed to receive QMux STOP_SENDING: {e}");
                    break;
                }
            }
        }
    }));
}

/// DATAGRAM の大きさがピアの上限に対してどう外れているか
#[derive(Debug, PartialEq, Eq)]
enum DatagramLimit {
    /// ピアが DATAGRAM を広告していない (RFC 9221 Section 3 の「値 0」と同じ)
    Unsupported,
    /// ピアの `max_datagram_frame_size` に収まらない
    FrameSize {
        /// ピアが広告した `max_datagram_frame_size`
        peer_max: u64,
        /// 送ろうとしている DATAGRAM フレーム全体の大きさ
        frame_size: u64,
    },
    /// ピアの `max_record_size` (1 Record の上限) に収まらない
    RecordSize {
        /// ピアが広告した `max_record_size`
        peer_max: u64,
        /// 送ろうとしている DATAGRAM フレーム全体の大きさ
        frame_size: u64,
    },
}

/// ピアが広告した DATAGRAM の上限に対して、送る予定の datagram が収まるかを判定する
///
/// tokio-qmux の `send_datagram` は、ピアが DATAGRAM を広告していない場合やフレームが
/// 上限を超える場合にエラーを返す。音声は 20 ms ごとに送るため、そのエラーを毎回
/// ログへ出すと量大になる。接続確立後 (制御ストリームを開いた後) に 1 度だけ判定して
/// 警告し、原因と対処が分かるようにする。
///
/// qmux-rs の `Connection::send_datagram` は DATAGRAM フレーム全体の大きさを
/// ピアの `max_datagram_frame_size` と `max_record_size` の両方と比較する
/// (`refs/quic/draft-ietf-quic-qmux-02.txt` Section 5.2 の Record の上限)。
/// どちらの上限に当たったかで対処が変わる (前者はピアの DATAGRAM 対応、後者は QMux の
/// Record 構成) ため、判定結果を型で分ける。
///
/// `message_size` は MOQT の OBJECT_DATAGRAM メッセージ全体の長さで、QMux が送る
/// DATAGRAM フレームはこれに Type 1 バイトと Length の可変長整数を足した大きさになる。
fn datagram_limit_violation(
    peer: &shiguredo_qmux::transport_params::TransportParameters,
    message_size: usize,
) -> Option<DatagramLimit> {
    let frame_size = datagram_frame_size(message_size)?;
    // RFC 9221 Section 3: パラメータなしと値 0 は DATAGRAM 非対応を表す
    let Some(peer_max) = peer.max_datagram_frame_size.filter(|max| *max > 0) else {
        return Some(DatagramLimit::Unsupported);
    };
    if frame_size > peer_max {
        return Some(DatagramLimit::FrameSize {
            peer_max,
            frame_size,
        });
    }
    if frame_size > peer.max_record_size {
        return Some(DatagramLimit::RecordSize {
            peer_max: peer.max_record_size,
            frame_size,
        });
    }
    None
}

/// ピアの上限に対して datagram が収まらない場合に警告する
pub(crate) fn warn_if_datagram_too_large(handle: &QmuxHandle, datagram_max_size: Option<usize>) {
    let Some(message_size) = datagram_max_size else {
        return;
    };
    let Some(peer) = handle.connection().peer_transport_parameters() else {
        // トランスポートパラメータ未受信。制御ストリームを開いた後なら通常は起きない
        tracing::warn!(
            "The peer transport parameters are not available yet; skipping the DATAGRAM size check"
        );
        return;
    };
    match datagram_limit_violation(&peer, message_size) {
        Some(DatagramLimit::Unsupported) => tracing::warn!(
            "The peer does not support DATAGRAM frames; audio sent as datagrams will be dropped"
        ),
        Some(DatagramLimit::FrameSize {
            peer_max,
            frame_size,
        }) => tracing::warn!(
            "The peer max_datagram_frame_size ({peer_max}) is smaller than the DATAGRAM frame size ({frame_size}) for --datagram-max-size {message_size}; audio sent as datagrams will be dropped"
        ),
        Some(DatagramLimit::RecordSize {
            peer_max,
            frame_size,
        }) => tracing::warn!(
            "The peer max_record_size ({peer_max}) is smaller than the DATAGRAM frame size ({frame_size}) for --datagram-max-size {message_size}; audio sent as datagrams will be dropped"
        ),
        None => {}
    }
}

/// MOQT メッセージ長から DATAGRAM フレーム全体の大きさを求める
///
/// QMux は DATAGRAM フレームを常に Length 付き (Type 0x31) で送るため、
/// Type 1 バイト + Length の可変長整数 + payload になる。可変長整数で表せない長さは
/// `None` になる (MOQT のメッセージ長では起きない)。
fn datagram_frame_size(payload_len: usize) -> Option<u64> {
    let payload = u64::try_from(payload_len).ok()?;
    let length_size = shiguredo_qmux::codec::varint::VarInt::try_from(payload)
        .ok()?
        .encoded_size();
    Some(1 + length_size as u64 + payload)
}

#[cfg(test)]
mod tests {
    use shiguredo_moqt::error::STREAM_CANCELLED;
    use shiguredo_moqt::message::ControlMessage;
    use shiguredo_moqt::message::common::TrackNamespace;
    use shiguredo_moqt::session::types::SessionState;
    use shiguredo_moqt::track_properties::TrackProperties;

    use bytes::Bytes;

    use super::*;
    use crate::moqt_client::MoqtClient;
    use crate::peer_harness::{QmuxTestPeerServer, TestPeer};
    use crate::transport::{RecvChunk, StreamAcceptor, StreamHandle};

    /// moq-pub の `--datagram-max-size` の既定値 (テストで往復させる大きさ)
    const DEFAULT_DATAGRAM_MAX_SIZE: usize = 1160;

    /// QMux のテスト peer と client を接続し、SETUP 交換まで進める
    ///
    /// TCP + TLS + QMux のテスト peer を立て、[`connect`] と
    /// [`MoqtClient::establish_qmux`] でクライアントを確立する。戻り値は peer、client、
    /// data stream のアクセプター、client の QMux ハンドルである。
    async fn connect_pair() -> (TestPeer, MoqtClient, StreamAcceptor, Arc<QmuxHandle>) {
        let mut server = QmuxTestPeerServer::start()
            .await
            .expect("サーバーを起動できること");
        let addr = server.local_addr();

        let connect = tokio::spawn(async move {
            let task_monitor = tokio_metrics::TaskMonitor::new();
            let session = connect(addr, "localhost", None, &task_monitor).await?;
            let handle = Arc::clone(&session.handle);
            let (client, acceptor) = MoqtClient::establish_qmux(
                session,
                "/",
                "localhost",
                "test-peer",
                &[],
                &task_monitor,
            )
            .await?;
            Ok::<_, TransportError>((client, acceptor, handle))
        });
        // 接続を先に待つ。TCP の接続は待ち受けより先に完了してよい
        let mut peer = server.accept().await.expect("接続を受理できること");
        peer.establish().await.expect("SETUP を交換できること");

        let (client, acceptor, handle) = connect
            .await
            .expect("接続タスクが終了すること")
            .expect("client を確立できること");
        assert_eq!(
            peer.session_state(),
            SessionState::Established,
            "peer 側 session が Established になること"
        );
        (peer, client, acceptor, handle)
    }

    /// DATAGRAM フレームの大きさが Type と Length を含む
    ///
    /// qmux-rs の `Connection::send_datagram` は `1 + length_size + payload_len` を
    /// ピアの `max_datagram_frame_size` と `max_record_size` の両方と比較する。
    /// moq-pub の `--datagram-max-size` の既定 1160 は Length が 2 バイトになるため、
    /// フレーム全体は 1163 バイトになる。Length の符号化長は RFC 9000 Section 16 の
    /// 可変長整数であり、境界は qmux-rs の `VarInt::encoded_size` に従う。
    #[test]
    fn datagram_frame_size_includes_header() {
        assert_eq!(
            datagram_frame_size(1160),
            Some(1163),
            "既定の 1160 は 1163 になること"
        );
        assert_eq!(
            datagram_frame_size(64),
            Some(67),
            "Length が 2 バイトになる下限"
        );
        assert_eq!(
            datagram_frame_size(63),
            Some(65),
            "Length が 1 バイトの上限"
        );
        assert_eq!(
            datagram_frame_size(16379),
            Some(16382),
            "Record に収まる最大"
        );
    }

    /// ピアの上限に対する判定が両方の上限を見る
    ///
    /// qmux-rs は DATAGRAM フレーム全体の大きさをピアの `max_datagram_frame_size` と
    /// `max_record_size` の両方と比較し、超過時は送信を拒否する。どちらの上限に当たったかで
    /// 対処が変わるため、警告の判定も両方を見る必要がある。
    #[test]
    fn datagram_limit_violation_checks_both_limits() {
        let mut peer = shiguredo_qmux::transport_params::TransportParameters::default();

        // 既定 (max_datagram_frame_size なし) は DATAGRAM 非対応として扱う
        assert_eq!(
            datagram_limit_violation(&peer, 1160),
            Some(DatagramLimit::Unsupported),
            "DATAGRAM 非対応のピアは警告すること"
        );
        // 値 0 も DATAGRAM 非対応である (RFC 9221 Section 3)
        peer.max_datagram_frame_size = Some(0);
        assert_eq!(
            datagram_limit_violation(&peer, 1160),
            Some(DatagramLimit::Unsupported),
            "値 0 のピアは警告すること"
        );

        // max_datagram_frame_size に収まらない場合はその上限を報告する
        peer.max_datagram_frame_size = Some(1162);
        assert_eq!(
            datagram_limit_violation(&peer, 1160),
            Some(DatagramLimit::FrameSize {
                peer_max: 1162,
                frame_size: 1163,
            }),
            "1 バイト足りない場合も警告すること"
        );

        // max_datagram_frame_size に収まっていても max_record_size に収まらない場合は
        // その上限を報告する (既定の Record は 16382)。20000 は Length が 4 バイトに
        // なるためフレーム全体は 20005 バイトである
        peer.max_datagram_frame_size = Some(65535);
        peer.max_record_size = shiguredo_qmux::record::DEFAULT_MAX_RECORD_SIZE;
        assert_eq!(
            datagram_limit_violation(&peer, 20000),
            Some(DatagramLimit::RecordSize {
                peer_max: shiguredo_qmux::record::DEFAULT_MAX_RECORD_SIZE,
                frame_size: 20005,
            }),
            "Record の上限に当たる場合も警告すること"
        );

        // 両方の上限に収まる場合は警告しない
        peer.max_datagram_frame_size = Some(65535);
        peer.max_record_size = 65535;
        assert_eq!(
            datagram_limit_violation(&peer, 20000),
            None,
            "収まる場合は警告しないこと"
        );
        assert_eq!(
            datagram_limit_violation(&peer, 1160),
            None,
            "既定の 1160 は収まること"
        );
    }

    /// 広告する DATAGRAM の上限が既定の Record に収まる
    ///
    /// `max_datagram_frame_size` を `max_record_size` の既定 (16382) に合わせているため、
    /// 上限いっぱいの DATAGRAM は payload 16379 バイトになる。
    #[test]
    fn advertised_datagram_limit_fits_in_a_record() {
        assert_eq!(
            datagram_frame_size(16379),
            Some(MAX_DATAGRAM_FRAME_SIZE),
            "Record の上限と同じフレームは payload 16379 になること"
        );
    }

    /// QMux 経由で SETUP を交換し、PUBLISH と SUBSCRIBE が成立する
    ///
    /// TCP + TLS + QMux のテスト peer (`QmuxTestPeerServer`) を立て、実 relay なしで
    /// client のハンドシェイクと request stream (双方向) の往復を検証する。
    /// QMux は QUIC version 1 のストリームを多重化するため、request stream は双方向になる。
    #[tokio::test(flavor = "multi_thread")]
    async fn qmux_client_establishes_publish_and_subscribe() {
        let (mut peer, client, _acceptor, _handle) = connect_pair().await;
        let mut client = client;

        let namespace = TrackNamespace::new(vec![b"live".to_vec()]).expect("名前空間を作れること");
        let subscribe_namespace = namespace.clone();
        // PUBLISH と SUBSCRIBE は peer の応答と並行して進める
        let client_task = tokio::spawn(async move {
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
            subscribed.track_alias, 11,
            "SUBSCRIBE_OK の track alias が一致すること"
        );
    }

    /// 映像 (subgroup stream) と音声 (datagram) の双方が QMux 経由で届く
    ///
    /// QMux はストリームと DATAGRAM を 1 本のバイトストリームへ多重化するため、両方が
    /// 同じ接続で往復できることを実行で固定する。
    #[tokio::test(flavor = "multi_thread")]
    async fn qmux_carries_subgroup_stream_and_datagram() {
        let (mut peer, _client, mut acceptor, handle) = connect_pair().await;
        let handle = StreamHandle::Qmux(handle);

        // 映像: peer が uni stream を開いて subgroup のバイト列を送る
        let mut send = peer.open_uni_send_stream().await.expect("開けること");
        send.send(Bytes::from_static(b"subgroup"))
            .await
            .expect("送信できること");
        send.finish().await.expect("終端できること");
        let mut recv = acceptor
            .accept_recv_stream()
            .await
            .expect("受理できること")
            .expect("ストリームがあること");
        match recv.receive_chunk().await.expect("受信できること") {
            RecvChunk::Data(data) => {
                assert_eq!(&data[..], b"subgroup", "subgroup のバイト列が届くこと")
            }
            RecvChunk::End(end) => panic!("データとして受信されること: {end:?}"),
        }

        // 音声: peer から client へ datagram が届く
        peer.send_datagram(b"audio")
            .await
            .expect("datagram を送れること");
        let datagrams = handle
            .recv_datagrams()
            .await
            .expect("datagram を受信できること");
        assert_eq!(
            datagrams,
            vec![b"audio".to_vec()],
            "送った datagram がそのまま届くこと"
        );

        // 音声: client から peer へ datagram が届く
        handle
            .send_datagram(b"audio-up")
            .await
            .expect("datagram を送れること");
        let datagrams = peer
            .recv_datagrams()
            .await
            .expect("peer が datagram を受信できること");
        assert_eq!(
            datagrams,
            vec![b"audio-up".to_vec()],
            "逆向きの datagram も届くこと"
        );

        // 音声: moq-pub の `--datagram-max-size` の既定 1160 の OBJECT_DATAGRAM が往復する。
        // DATAGRAM フレーム全体は 1163 バイトになり、広告した `max_datagram_frame_size`
        // (Record の上限と同じ 16382) に収まる
        let audio_object = vec![0x5a_u8; DEFAULT_DATAGRAM_MAX_SIZE];
        handle
            .send_datagram(&audio_object)
            .await
            .expect("既定の上限の datagram を送れること");
        let datagrams = peer
            .recv_datagrams()
            .await
            .expect("peer が datagram を受信できること");
        assert_eq!(
            datagrams.first().map(Vec::len),
            Some(DEFAULT_DATAGRAM_MAX_SIZE),
            "既定の上限の datagram が欠落せず届くこと"
        );
    }

    /// ピアの STOP_SENDING が接続単位の観測と送信ストリームの終端の双方に現れる
    ///
    /// QMux の STOP_SENDING は接続単位の `Connection::recv_stop_sending` で通知される
    /// (`SendStream::stopped` は送信と同時に await できない)。設計はこの通知を
    /// `spawn_stop_sending` が [`StopSendingObserved`] へ詰め替えて `StopSendingReceiver` へ
    /// 流し、`MoqtClient::handle_stop_sending` が QUIC 経路と同じ処理を行う経路を採る。
    /// ここでは MoqtClient を介さず QMux 接続を直接使って、その経路と、通知済みのストリームの
    /// 送信 API が終端を返すことの両方を確認する。
    #[tokio::test(flavor = "multi_thread")]
    async fn qmux_peer_stop_sending_ends_the_send_stream() {
        let mut server = QmuxTestPeerServer::start()
            .await
            .expect("サーバーを起動できること");
        let addr = server.local_addr();
        let connect = tokio::spawn(async move {
            let task_monitor = tokio_metrics::TaskMonitor::new();
            connect(addr, "localhost", None, &task_monitor).await
        });
        let mut peer = server.accept().await.expect("接続を受理できること");
        let session = connect
            .await
            .expect("接続タスクが終了すること")
            .expect("接続できること");
        let QmuxSession {
            handle,
            mut stop_sending_rx,
            ..
        } = session;
        let handle = StreamHandle::Qmux(handle);

        // client が uni stream を開いて送り、peer が受理して STOP_SENDING を送る
        let mut send = handle.open_send_stream().await.expect("開けること");
        let stream_id = send.stream_id();
        send.send(Bytes::from_static(b"first"))
            .await
            .expect("送信できること");
        let mut recv = peer.accept_recv_stream().await.expect("受理できること");
        recv.stop_sending(STREAM_CANCELLED)
            .await
            .expect("STOP_SENDING を送れること");

        // 接続単位の通知が観測タスクから届く
        let observed = stop_sending_rx.recv().await.expect("観測できること");
        assert_eq!(
            observed.stream_id, stream_id,
            "送信したストリームの ID が届くこと"
        );
        assert_eq!(
            observed.error_code, STREAM_CANCELLED,
            "peer が載せたコードがそのまま届くこと"
        );

        // 通知済みのストリームの送信 API は終端を返す (QUIC 経路の StreamReset と同じ扱い)
        let err = send
            .send(Bytes::from_static(b"second"))
            .await
            .expect_err("終端として観測されること");
        match err {
            TransportError::StreamReset { error_code } => assert_eq!(
                error_code, STREAM_CANCELLED,
                "peer が載せたコードがそのまま観測されること"
            ),
            other => panic!("StreamReset として観測されること: {other}"),
        }
    }

    /// 自側の close でドライバと受理タスクが終了し、下位トランスポートが解放される
    ///
    /// tokio-qmux のドライバは、自側の `close` で CONNECTION_CLOSE を送って下位トランスポートの
    /// 送信側を shutdown し、ピアの shutdown を linger (既定 3 秒) の上限つきで待って終了する。
    /// ドライバが終了すれば [`split_connection`] が起こした受理タスクも `closed()` の観測で
    /// 終了し、channel が閉じる (接続と TCP ソケットを握り続けない)。
    #[tokio::test(flavor = "multi_thread")]
    async fn qmux_close_releases_the_driver_and_accept_tasks() {
        let mut server = QmuxTestPeerServer::start()
            .await
            .expect("サーバーを起動できること");
        let addr = server.local_addr();
        let connect = tokio::spawn(async move {
            let task_monitor = tokio_metrics::TaskMonitor::new();
            connect(addr, "localhost", None, &task_monitor).await
        });
        let peer = server.accept().await.expect("接続を受理できること");
        let session = connect
            .await
            .expect("接続タスクが終了すること")
            .expect("接続できること");
        let QmuxSession {
            handle,
            mut uni_rx,
            mut bi_rx,
            mut stop_sending_rx,
        } = session;

        // 自側から close する。ドライバは CONNECTION_CLOSE を送り、下位トランスポートの
        // 送信側を shutdown する
        handle
            .connection()
            .close(0, "test close")
            .expect("close 要求を送れること");

        // ピアは CONNECTION_CLOSE を観測して下位トランスポートを閉じるため、client の
        // ドライバは linger の上限を待たずに終了する
        let reason = tokio::time::timeout(std::time::Duration::from_secs(10), handle.closed())
            .await
            .expect("ドライバが終了すること");
        assert!(
            matches!(reason, shiguredo_qmux::event::CloseReason::Local { .. }),
            "自側の close として観測されること: {reason:?}"
        );

        // 受理タスクと STOP_SENDING の観測タスクは接続の終了で抜け、channel が閉じる
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), uni_rx.recv())
                .await
                .expect("単方向ストリームの受理タスクが終了すること")
                .is_none(),
            "単方向ストリームの受理タスクが終了すること"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), bi_rx.recv())
                .await
                .expect("双方向ストリームの受理タスクが終了すること")
                .is_none(),
            "双方向ストリームの受理タスクが終了すること"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), stop_sending_rx.recv())
                .await
                .expect("STOP_SENDING の観測タスクが終了すること")
                .is_none(),
            "STOP_SENDING の観測タスクが終了すること"
        );

        // ピアは client の CONNECTION_CLOSE を観測する
        match peer.wait_qmux_closed().await.expect("QMux 接続であること") {
            shiguredo_qmux::event::CloseReason::Peer {
                application,
                error_code,
                reason,
            } => {
                assert!(application, "アプリケーションエラーとして観測されること");
                assert_eq!(error_code, 0, "client が載せたコードがそのまま届くこと");
                assert_eq!(
                    reason, "test close",
                    "client が載せた理由がそのまま届くこと"
                );
            }
            other => panic!("Peer として観測されること: {other:?}"),
        }
    }

    /// ピアが shutdown しない場合でも close 後にドライバが終了する
    ///
    /// tokio-qmux のドライバは、自側の `close` の後に下位トランスポートの送信側を shutdown し、
    /// ピアの shutdown を linger (既定 3 秒) の上限つきで待つ。ピアが CONNECTION_CLOSE を
    /// 読まずに shutdown もしない場合、linger が無いとドライバは終了せず、接続と TCP
    /// ソケットが残る。ここでは QMux を話さず読み取りもしない peer を立て、linger の上限で
    /// ドライバが終了することを固定する。協力的な peer を使う
    /// [`qmux_close_releases_the_driver_and_accept_tasks`] だけでは、旧 rev でも通るため
    /// この経路を固定できない。
    #[tokio::test(flavor = "multi_thread")]
    async fn qmux_close_terminates_the_driver_without_peer_shutdown() {
        // QMux を話さない peer。TLS のハンドシェイクだけ応答し、その後の読み取りと
        // shutdown を行わない
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("証明書を生成できること");
        let mut tls_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![certified.cert.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(
                    certified.signing_key.serialize_der().into(),
                ),
            )
            .expect("証明書を設定できること");
        tls_config.alpn_protocols = vec![crate::MOQT_PROTOCOL.as_bytes().to_vec()];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("TCP を待ち受けられること");
        let addr = listener
            .local_addr()
            .expect("待ち受けアドレスを取得できること");
        let peer = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("接続を受理できること");
            tcp.set_nodelay(true).expect("Nagle を無効化できること");
            // ハンドシェイクだけ応答し、以後は読まずに shutdown もしない
            let tls = acceptor.accept(tcp).await.expect("TLS を受理できること");
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            drop(tls);
        });

        let task_monitor = tokio_metrics::TaskMonitor::new();
        let session = connect(addr, "localhost", None, &task_monitor)
            .await
            .expect("接続できること");
        let QmuxSession {
            handle,
            mut uni_rx,
            mut bi_rx,
            mut stop_sending_rx,
        } = session;

        // 自側から close する。ピアは shutdown しないため、ドライバは linger の上限まで待つ
        handle
            .connection()
            .close(0, "peer does not shut down")
            .expect("close 要求を送れること");
        let started = std::time::Instant::now();
        let reason = tokio::time::timeout(std::time::Duration::from_secs(6), handle.closed())
            .await
            .expect("linger の上限でドライバが終了すること");
        assert!(
            matches!(reason, shiguredo_qmux::event::CloseReason::Local { .. }),
            "自側の close として観測されること: {reason:?}"
        );
        assert!(
            started.elapsed() >= tokio_qmux::DEFAULT_LINGER,
            "ピアが shutdown しない場合は linger の上限まで待つこと: {:?}",
            started.elapsed()
        );

        // 受理タスクと STOP_SENDING の観測タスクもドライバの終了で抜ける
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), uni_rx.recv())
                .await
                .expect("単方向ストリームの受理タスクが終了すること")
                .is_none(),
            "単方向ストリームの受理タスクが終了すること"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), bi_rx.recv())
                .await
                .expect("双方向ストリームの受理タスクが終了すること")
                .is_none(),
            "双方向ストリームの受理タスクが終了すること"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), stop_sending_rx.recv())
                .await
                .expect("STOP_SENDING の観測タスクが終了すること")
                .is_none(),
            "STOP_SENDING の観測タスクが終了すること"
        );

        peer.abort();
    }

    /// 設定が QUIC 直接接続の上限に相当する量を持つ
    #[test]
    fn config_has_flow_control_credits() {
        let config = build_config();
        assert_eq!(
            config.initial_max_streams_uni, 10_000,
            "単方向 stream の上限"
        );
        assert_eq!(
            config.initial_max_streams_bidi, 1_000,
            "双方向 stream の上限"
        );
        assert_eq!(config.initial_max_data, 64 * 1024 * 1024, "接続の窓");
        assert_eq!(
            config.max_datagram_frame_size,
            Some(MAX_DATAGRAM_FRAME_SIZE),
            "DATAGRAM を有効にすること"
        );
        // 既定のまま使う値 (max_record_size など) が 0 でないこと
        assert_eq!(
            config.max_record_size,
            shiguredo_qmux::record::DEFAULT_MAX_RECORD_SIZE,
            "Record の上限は既定値のまま使うこと"
        );
    }
}
