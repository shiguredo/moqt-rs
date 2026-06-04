//! WebTransport クライアント実装
//!
//! s2n-quic + shiguredo_http3 (Sans I/O) を使い WebTransport セッションを確立する。
//! 節番号は draft-ietf-webtrans-http3-16 を参照する。依存する shiguredo_http3 が
//! 対応する wire バージョンは draft-15 相当 (`:protocol` 値は共通)。
//!
//! publisher (datagram 送信) と subscriber (datagram 受信 + 受信ストリーム accept) の
//! 双方が利用する union API を提供する。
//!
//! 本モジュールはクライアント例であり、サーバ側専用の要件 (非対応リソースへの 405 応答など)
//! や、未実装の任意要件 (応答前の optimistic capsule 送信、`WT_CLOSE_SESSION` 受信経路) は
//! 対象外とする。対象外の詳細は `WtClient::connect` / `WtSession::close` のコメントを参照。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};

use bytes::Bytes;
use s2n_quic::client::Connect;
use s2n_quic::stream::{ReceiveStream, SendStream};
use shiguredo_http3::event::WebTransportEvent;
use shiguredo_http3::webtransport::capsule::Capsule;
use shiguredo_http3::webtransport::connect::ConnectRequest;
use shiguredo_http3::webtransport::stream::{
    ClassifiedUniStream, StreamHeader, StreamHeaderDecodeError, classify_uni_stream_checked,
};
use shiguredo_http3::{ClientConnection, Event, Settings as H3Settings};
use shiguredo_moqt::session::types::RequestStreamEnd;
use tokio::sync::Notify;
use tokio::sync::mpsc;

use crate::error::{Result, TransportError};

/// WebTransport over HTTP/3 の ALPN プロトコル識別子 (RFC 9114 §3.1)
///
/// s2n-quic の default TLS builder も既定で `h3` を設定するため、証明書検証を
/// スキップする開発用経路でも同じ値を明示する。
const H3_ALPN: &[u8] = b"h3";

// ---------------------------------------------------------------------------
// クライアント設定
// ---------------------------------------------------------------------------

/// WebTransport クライアント設定
pub struct ClientConfig {
    pub remote_addr: SocketAddr,
    /// SNI / 証明書検証に使うサーバー名 (ポートを含まない)
    pub server_name: String,
    /// WebTransport CONNECT の `:authority` に使う値 (ポートを含む)
    pub authority: String,
    pub ca_cert_pem: Option<String>,
    pub disable_cert_validation: bool,
    pub h3_settings: H3Settings,
    /// datagram 継続受信のバックグラウンドタスクを起動するか
    ///
    /// subscriber (datagram 受信側) のみ `true` にする。publisher は datagram を
    /// 送信するだけで受信しないため `false` のままとし、未消費の datagram が
    /// HTTP/3 接続状態に蓄積し続けることや、不要なポーリングを避ける。
    pub receive_datagrams: bool,
}

impl ClientConfig {
    pub fn new(remote_addr: SocketAddr, server_name: impl Into<String>) -> Self {
        let server_name = server_name.into();
        Self {
            remote_addr,
            // 既定では SNI と同じ値を `:authority` に使う (ポート無し)
            authority: server_name.clone(),
            server_name,
            ca_cert_pem: None,
            disable_cert_validation: false,
            h3_settings: H3Settings::default(),
            receive_datagrams: false,
        }
    }

    /// WebTransport CONNECT の `:authority` を設定する
    ///
    /// draft-ietf-webtrans-http3-16 §3.2 は `:authority` に target URI の authority
    /// (非デフォルトポートを含む) を設定する MUST を定める。SNI 用の `server_name` とは
    /// 別に、ポート込みの authority を指定するために使う。
    pub fn authority(mut self, authority: impl Into<String>) -> Self {
        self.authority = authority.into();
        self
    }

    pub fn ca_cert(mut self, pem: impl Into<String>) -> Self {
        self.ca_cert_pem = Some(pem.into());
        self
    }

    /// 証明書検証を無効化する (開発用)
    ///
    /// 呼び出し側は無効化の旨をログ等で明示すること。
    pub fn insecure(mut self) -> Self {
        self.disable_cert_validation = true;
        self
    }

    pub fn enable_webtransport(mut self, wt: shiguredo_http3::webtransport::Settings) -> Self {
        self.h3_settings = self.h3_settings.enable_webtransport_client(wt);
        self
    }

    /// datagram 継続受信タスクを有効化する (subscriber 側で使用)
    pub fn receive_datagrams(mut self) -> Self {
        self.receive_datagrams = true;
        self
    }
}

// ---------------------------------------------------------------------------
// HTTP/3 接続状態 (Sans I/O ラッパー)
// ---------------------------------------------------------------------------

struct ClientConnectionState {
    h3_conn: ClientConnection,
}

impl ClientConnectionState {
    fn new(settings: H3Settings) -> Self {
        Self {
            h3_conn: ClientConnection::new(settings),
        }
    }

    fn process_stream_data(&mut self, id: u64, data: &[u8], fin: bool) -> Result<Vec<Event>> {
        self.h3_conn.feed_stream(id, data, fin)?;
        Ok(self.h3_conn.drain_events()?)
    }

    fn feed_stream_only(&mut self, id: u64, data: &[u8], fin: bool) -> Result<()> {
        self.h3_conn.feed_stream(id, data, fin)?;
        Ok(())
    }

    fn drain_events(&mut self) -> Result<Vec<Event>> {
        Ok(self.h3_conn.drain_events()?)
    }

    fn feed_datagram(&mut self, data: &[u8]) -> Result<Vec<Event>> {
        self.h3_conn.feed_datagram(data)?;
        Ok(self.h3_conn.drain_events()?)
    }
}

// ---------------------------------------------------------------------------
// CONNECT レスポンス判定
// ---------------------------------------------------------------------------

/// CONNECT レスポンスのイベント列を処理した結果
#[derive(Debug, Clone, Copy)]
enum ConnectOutcome {
    /// HeadersEnd 未受信。判定はまだ確定していない
    Pending,
    /// 2xx を受信しセッション確立
    Established,
    /// HeadersEnd を受信したが :status が 2xx でない、または :status 不在
    Failed { status: Option<u16> },
}

/// WT_CLOSE_SESSION の Application Error Message 最大長 (バイト)
///
/// draft-ietf-webtrans-http3-16 §6: length MUST NOT exceed 1024 bytes。
/// 将来 draft 改定で上限が変わる可能性がある。
const CLOSE_SESSION_MESSAGE_MAX_BYTES: usize = 1024;

/// Application Error Message を UTF-8 境界で最大長に切り詰める
///
/// draft-ietf-webtrans-http3-16 §6:
/// Senders that truncate an application-supplied message MUST do so at a UTF-8 character boundary.
fn truncate_close_session_message(message: &str) -> &str {
    if message.len() <= CLOSE_SESSION_MESSAGE_MAX_BYTES {
        return message;
    }
    let mut end = CLOSE_SESSION_MESSAGE_MAX_BYTES;
    while end > 0 && !message.is_char_boundary(end) {
        end -= 1;
    }
    &message[..end]
}

/// CONNECT レスポンスの `:status` 擬似ヘッダー (ASCII 数字列) を u16 にパースする
fn parse_status(value: &[u8]) -> Option<u16> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

/// CONNECT レスポンスのイベント列を走査する
///
/// `:status` ヘッダーを受信したら `status` に蓄積し (HeadersEnd と別バッチで届く場合に備える)、
/// HeadersEnd を受信した時点で蓄積した status が 2xx かどうかで結果を確定する。
/// draft-ietf-webtrans-http3-16 §3.2: クライアントは 2xx 応答受信時のみセッション確立とみなす。
fn connect_outcome(events: &[Event], status: &mut Option<u16>) -> ConnectOutcome {
    for event in events {
        match event {
            Event::Header { name, value, .. } if name == b":status" => {
                *status = parse_status(value);
            }
            Event::HeadersEnd { .. } => {
                return match *status {
                    Some(s) if (200..=299).contains(&s) => ConnectOutcome::Established,
                    other => ConnectOutcome::Failed { status: other },
                };
            }
            _ => {}
        }
    }
    ConnectOutcome::Pending
}

// ---------------------------------------------------------------------------
// WebTransport クライアント
// ---------------------------------------------------------------------------

/// WebTransport セッションを確立するクライアント
pub struct WtClient;

impl WtClient {
    /// WebTransport セッションを確立する
    pub async fn connect(config: ClientConfig, path: &str) -> Result<WtSession> {
        let datagram_endpoint = s2n_quic::provider::datagram::default::Endpoint::builder()
            .with_recv_capacity(64)
            .map_err(|e| TransportError::Internal(format!("datagram endpoint: {e}")))?
            .build()
            .expect("datagram endpoint build must succeed after recv capacity is set");

        let client = if config.disable_cert_validation {
            // 開発用: 証明書検証をスキップする。WebTransport over HTTP/3 の ALPN は
            // `h3` (RFC 9114 §3.1) を使う。s2n-quic の default TLS は feature
            // `provider-tls-rustls` で rustls に解決されるため、cert_store が空の
            // builder では `missing trusted root certificate(s)` で必ず失敗する。
            let tls = crate::quic::build_insecure_tls_client(&[H3_ALPN])?;
            s2n_quic::Client::builder()
                .with_tls(tls)
                .map_err(TransportError::transport)?
                .with_io("0.0.0.0:0")
                .map_err(TransportError::transport)?
                .with_datagram(datagram_endpoint)
                .map_err(TransportError::transport)?
                .start()
                .map_err(TransportError::transport)?
        } else if let Some(ref ca_pem) = config.ca_cert_pem {
            s2n_quic::Client::builder()
                .with_tls(ca_pem.as_str())
                .map_err(TransportError::transport)?
                .with_io("0.0.0.0:0")
                .map_err(TransportError::transport)?
                .with_datagram(datagram_endpoint)
                .map_err(TransportError::transport)?
                .start()
                .map_err(TransportError::transport)?
        } else {
            return Err(TransportError::InvalidState(
                "ca_cert_pem or disable_cert_validation is required".to_string(),
            ));
        };

        let connect = Connect::new(config.remote_addr).with_server_name(&*config.server_name);
        let mut connection = client.connect(connect).await?;

        let state = Arc::new(StdMutex::new(ClientConnectionState::new(
            config.h3_settings,
        )));

        // H3 ストリーム (制御 + QPACK encoder/decoder) を初期化する
        let mut control_send = connection.open_send_stream().await?;
        let encoder_send = connection.open_send_stream().await?;
        let decoder_send = connection.open_send_stream().await?;

        let init_data = {
            let mut s = state
                .lock()
                .expect("connection state mutex must not be poisoned");
            s.h3_conn
                .init_h3_streams(control_send.id(), encoder_send.id(), decoder_send.id())?
        };

        control_send
            .send(Bytes::from(init_data.control_data))
            .await?;
        // encoder/decoder は初期データだけ送れば良い (ストリーム自体は保持不要)

        // CONNECT リクエスト (双方向ストリーム) を開く
        let connect_stream = connection.open_bidirectional_stream().await?;
        let connect_stream_id: u64 = connect_stream.id();
        let (mut recv_stream, mut send_stream) = connect_stream.split();

        // Sans I/O で CONNECT リクエストをエンコードする
        let request_data = {
            let mut s = state
                .lock()
                .expect("connection state mutex must not be poisoned");
            let headers = ConnectRequest::new("https", &config.authority, path)
                // draft-ietf-moq-transport-21 §6.2 / draft-ietf-webtrans-http3-16 §3.3:
                // MOQT のプロトコル識別子を WT-Available-Protocols で通知する
                .available_protocols(vec![crate::MOQT_PROTOCOL.to_string()])
                .to_headers()
                .map_err(|e| {
                    TransportError::Internal(format!("failed to build CONNECT headers: {e}"))
                })?;
            let h3_id = s.h3_conn.send_request(&headers, false)?;
            if h3_id != connect_stream_id {
                return Err(TransportError::Internal(format!(
                    "unexpected CONNECT stream id: expected {connect_stream_id}, got {h3_id}"
                )));
            }
            let mut data = Vec::new();
            while let Some((chunk, _fin)) = s.h3_conn.take_stream_data(h3_id) {
                data.extend_from_slice(&chunk);
            }
            data
        };

        // CONNECT リクエストを送信する (fin=false)
        // draft-ietf-webtrans-http3-16 §3.2: クライアントは応答前に capsule を楽観送信してよい (MAY)。
        // 本実装は 2xx 受信まで capsule を送らない (非対応)。
        send_stream
            .send(Bytes::from(request_data))
            .await
            .map_err(TransportError::transport)?;

        let (handle, stream_acceptor) = connection.split();
        let (_bidi_acceptor, mut uni_acceptor) = stream_acceptor.split();

        let unblock_notify = Arc::new(Notify::new());
        let (uni_tx, uni_rx) = mpsc::channel::<WtRecvStream>(16);

        // 単方向ストリーム受信タスク
        let state_for_uni = Arc::clone(&state);
        let notify_for_uni = Arc::clone(&unblock_notify);
        tokio::spawn(async move {
            while let Ok(Some(recv)) = uni_acceptor.accept_receive_stream().await {
                let state = Arc::clone(&state_for_uni);
                let notify = Arc::clone(&notify_for_uni);
                let uni_tx = uni_tx.clone();
                let stream_id: u64 = recv.id();
                tokio::spawn(route_uni_stream(stream_id, recv, state, notify, uni_tx));
            }
        });

        // datagram 受信タスク (subscriber 側のみ起動する)
        if config.receive_datagrams {
            let state_for_datagram = Arc::clone(&state);
            let handle_for_datagram = handle.clone();
            tokio::spawn(async move {
                loop {
                    let result = handle_for_datagram.datagram_mut(
                        |receiver: &mut s2n_quic::provider::datagram::default::Receiver| {
                            receiver.recv_datagram()
                        },
                    );
                    match result {
                        Ok(Some(bytes)) => {
                            let mut s = state_for_datagram
                                .lock()
                                .expect("connection state mutex must not be poisoned");
                            match s.feed_datagram(&bytes) {
                                Ok(events) => {
                                    for event in events {
                                        tracing::debug!("Datagram event: {event:?}");
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to feed datagram: {e}");
                                    break;
                                }
                            }
                        }
                        Ok(None) => {
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                        Err(e) => {
                            tracing::warn!("Datagram query error: {e}");
                            break;
                        }
                    }
                }
            });
        }

        // 制御ストリームと QPACK ストリームを接続中保持する
        // (クローズすると H3_CLOSED_CRITICAL_STREAM エラーになる)
        tokio::spawn(async move {
            let _control_send = control_send;
            let _encoder_send = encoder_send;
            let _decoder_send = decoder_send;
            std::future::pending::<()>().await;
        });

        // CONNECT レスポンスを待つ
        // draft-ietf-webtrans-http3-16 §3.2: クライアントは 2xx 応答受信時のみセッション確立とみなす。
        // :status を見ず HeadersEnd だけで判定すると 4xx/5xx エラーレスポンスを成功と誤認するため、
        // connect_outcome で :status を確認する。
        // サーバが非対応リソースに返す推奨ステータスは -16 で 404 から 405 に変わったが、
        // クライアントは 2xx 以外を一律失敗として扱うため追加の分岐は不要。
        let mut connect_status: Option<u16> = None;
        let mut session_established = false;
        while !session_established {
            tokio::select! {
                received = recv_stream.receive() => {
                    let (data, fin) = match received {
                        Ok(Some(data)) => (data.to_vec(), false),
                        Ok(None) => (vec![], true),
                        Err(e) => return Err(TransportError::transport(e)),
                    };
                    let events = {
                        let mut s = state.lock().expect("connection state mutex must not be poisoned");
                        s.process_stream_data(connect_stream_id, &data, fin)?
                    };
                    match connect_outcome(&events, &mut connect_status) {
                        ConnectOutcome::Established => session_established = true,
                        ConnectOutcome::Failed { status } => {
                            return Err(TransportError::ConnectFailed { status });
                        }
                        ConnectOutcome::Pending => {}
                    }
                    if fin { break; }
                }
                _ = unblock_notify.notified() => {
                    let events = state.lock().expect("connection state mutex must not be poisoned").drain_events()?;
                    match connect_outcome(&events, &mut connect_status) {
                        ConnectOutcome::Established => session_established = true,
                        ConnectOutcome::Failed { status } => {
                            return Err(TransportError::ConnectFailed { status });
                        }
                        ConnectOutcome::Pending => {}
                    }
                }
            }
        }

        if !session_established {
            return Err(TransportError::ConnectionClosed);
        }

        // CONNECT の受信側はセッション確立判定にのみ使い、以降は保持しない。
        // そのため WT_CLOSE_SESSION 受信や、受信後の STOP_SENDING (WT_SESSION_GONE) /
        // H3_MESSAGE_ERROR reset (§6) は非対応。
        drop(recv_stream);

        Ok(WtSession {
            session_id: connect_stream_id,
            handle,
            connect_send: send_stream,
            uni_rx,
            state,
        })
    }
}

// ---------------------------------------------------------------------------
// WebTransport セッション
// ---------------------------------------------------------------------------

/// WebTransport セッション
///
/// `state` は `ClientConnectionState` (HTTP/3 の Sans I/O 状態) を `Arc<StdMutex<...>>` で
/// 共有する。datagram 受信タスクと各ストリーム操作が並行して状態を更新するため
/// `Mutex` で保護するが、各操作は await をまたがず短時間で完結する。
pub struct WtSession {
    session_id: u64,
    handle: s2n_quic::connection::Handle,
    connect_send: SendStream,
    uni_rx: mpsc::Receiver<WtRecvStream>,
    state: Arc<StdMutex<ClientConnectionState>>,
}

impl WtSession {
    /// 単方向送信ストリームを開く
    pub async fn open_uni_stream(&mut self) -> Result<WtSendStream> {
        let stream = self
            .handle
            .open_send_stream()
            .await
            .map_err(TransportError::transport)?;
        let stream_id: u64 = stream.id();
        let mut send = stream;

        // WT 単方向ストリームヘッダー (0x54 + session_id) を送信する
        let mut header = Vec::new();
        // session_id は CONNECT stream ID である (client 起動の双方向、session_id % 4 == 0)
        StreamHeader::new(self.session_id)
            .expect("session_id must be a client-initiated bidirectional stream ID")
            .encode_unidirectional(&mut header);
        send.send(Bytes::from(header))
            .await
            .map_err(TransportError::transport)?;

        Ok(WtSendStream { stream_id, send })
    }

    /// 単方向受信ストリームを受け付ける
    pub async fn accept_uni_stream(&mut self) -> Result<WtRecvStream> {
        self.uni_rx.recv().await.ok_or(TransportError::StreamClosed)
    }

    /// 双方向ストリームを開く (draft-ietf-webtrans-http3-16 Section 4.3)
    pub async fn open_bi_stream(&mut self) -> Result<WtBiStream> {
        let stream = self
            .handle
            .open_bidirectional_stream()
            .await
            .map_err(TransportError::transport)?;
        let stream_id: u64 = stream.id();
        let (recv, mut send) = stream.split();

        let mut header = Vec::new();
        // session_id は CONNECT stream ID である (client 起動の双方向、session_id % 4 == 0)
        StreamHeader::new(self.session_id)
            .expect("session_id must be a client-initiated bidirectional stream ID")
            .encode_bidirectional(&mut header);
        send.send(Bytes::from(header))
            .await
            .map_err(TransportError::transport)?;

        Ok(WtBiStream {
            stream_id,
            recv,
            send,
            pending: Vec::new(),
        })
    }

    /// セッションをクローズする (draft-ietf-webtrans-http3-16 Section 6)
    ///
    /// `WT_CLOSE_SESSION` を送り、直後に CONNECT stream へ FIN を送る (MUST)。
    /// Application Error Message が 1024 バイトを超える場合は UTF-8 境界で truncate する (MUST)。
    ///
    /// 非対応:
    /// - 送信後の `STOP_SENDING` with `WT_SESSION_GONE` (§6 MAY)
    /// - `WT_CLOSE_SESSION` 受信経路、および受信側の `STOP_SENDING` / `H3_MESSAGE_ERROR` (§6)
    pub async fn close(&mut self, code: u32, reason: &str) -> Result<()> {
        let capsule = Capsule::CloseSession {
            error_code: code,
            message: truncate_close_session_message(reason).to_string(),
        };
        let mut buf = Vec::new();
        capsule.encode(&mut buf);
        self.connect_send
            .send(Bytes::from(buf))
            .await
            .map_err(TransportError::transport)?;
        // draft-ietf-webtrans-http3-16 §6 (Session Termination):
        // WT_CLOSE_SESSION 送信後は CONNECT stream に即座に FIN を送る (MUST)
        self.connect_send
            .finish()
            .map_err(TransportError::transport)
    }

    /// WebTransport セッションにバッファリングされた datagram を取り出す (subscriber 側で使用)
    pub fn take_buffered_datagrams(&self) -> Result<Vec<Vec<u8>>> {
        let mut s = self
            .state
            .lock()
            .expect("connection state mutex must not be poisoned");
        let events = s.drain_events()?;
        let mut datagrams = Vec::new();
        for event in events {
            if let Event::WebTransport(WebTransportEvent::Datagram { payload, .. }) = event {
                datagrams.push(payload);
            }
        }
        Ok(datagrams)
    }

    /// datagram を送信する (publisher 側で使用)
    ///
    /// HTTP Datagram フォーマットでエンコードし、s2n-quic の datagram sender で送信する。
    /// `state` のロックは HTTP Datagram 生成後に解放してから `handle.datagram_mut` を呼び、
    /// デッドロックを回避する。
    pub async fn send_datagram(&self, payload: &[u8]) -> Result<()> {
        let datagram_bytes = {
            let s = self
                .state
                .lock()
                .expect("connection state mutex must not be poisoned");
            s.h3_conn.send_datagram(self.session_id, payload)?
        };
        let bytes = Bytes::from(datagram_bytes);
        self.handle
            .datagram_mut(
                |sender: &mut s2n_quic::provider::datagram::default::Sender| {
                    sender.send_datagram(bytes)
                },
            )
            .map_err(TransportError::transport)?
            .map_err(|e| TransportError::Internal(format!("datagram send: {e}")))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ストリーム型
// ---------------------------------------------------------------------------

/// WebTransport の単方向送信ストリーム
pub struct WtSendStream {
    stream_id: u64,
    send: SendStream,
}

impl WtSendStream {
    /// データを送信する
    pub async fn send(&mut self, data: &[u8]) -> Result<()> {
        self.send
            .send(Bytes::copy_from_slice(data))
            .await
            .map_err(TransportError::transport)
    }

    /// ストリームの送信方向を終了する (FIN)
    pub fn finish(&mut self) -> Result<()> {
        self.send.finish().map_err(TransportError::transport)
    }

    /// ストリームの送信方向を reset する (QUIC RESET_STREAM)
    pub fn reset(&mut self, error_code: u64) -> Result<()> {
        let code =
            s2n_quic::application::Error::new(error_code).map_err(TransportError::transport)?;
        self.send.reset(code).map_err(TransportError::transport)
    }

    /// ストリーム ID を返す (publisher 側で使用)
    pub fn stream_id(&self) -> u64 {
        self.stream_id
    }
}

/// WebTransport の受信ストリーム
///
/// ストリームタイプ判定で読みすぎたバイトを `pending` に保持し、
/// `recv_chunk` の初回呼び出しで返す。
pub struct WtRecvStream {
    stream_id: u64,
    recv: ReceiveStream,
    pending: Vec<u8>,
}

/// WebTransport ストリームから取り出した 1 要素
pub enum RecvChunk {
    /// 受信データ
    Data(Vec<u8>),
    /// ストリーム終端
    End(RequestStreamEnd),
}

impl WtRecvStream {
    fn new(stream_id: u64, recv: ReceiveStream, pending: Vec<u8>) -> Self {
        Self {
            stream_id,
            recv,
            pending,
        }
    }

    /// データまたは終端を 1 つ受信する
    pub async fn recv_chunk(&mut self) -> Result<RecvChunk> {
        if !self.pending.is_empty() {
            return Ok(RecvChunk::Data(std::mem::take(&mut self.pending)));
        }
        match self.recv.receive().await {
            Ok(Some(data)) => Ok(RecvChunk::Data(data.to_vec())),
            Ok(None) => Ok(RecvChunk::End(RequestStreamEnd::Fin)),
            Err(s2n_quic::stream::Error::StreamReset { error, .. }) => {
                Ok(RecvChunk::End(RequestStreamEnd::Reset {
                    error_code: error.into(),
                    reliable_size: None,
                }))
            }
            Err(e) => Err(TransportError::transport(e)),
        }
    }

    /// ストリーム ID を返す (subscriber 側で使用)
    pub fn stream_id(&self) -> u64 {
        self.stream_id
    }
}

/// WebTransport の双方向ストリーム
pub struct WtBiStream {
    stream_id: u64,
    recv: ReceiveStream,
    send: SendStream,
    pending: Vec<u8>,
}

impl WtBiStream {
    /// 双方向ストリームを送信側と受信側に分解する
    pub fn into_parts(self) -> (WtSendStream, WtRecvStream) {
        (
            WtSendStream {
                stream_id: self.stream_id,
                send: self.send,
            },
            WtRecvStream::new(self.stream_id, self.recv, self.pending),
        )
    }
}

// ---------------------------------------------------------------------------
// 単方向ストリームルーティング
// ---------------------------------------------------------------------------

/// 単方向ストリームをタイプで判別してルーティングする
async fn route_uni_stream(
    stream_id: u64,
    mut recv_stream: ReceiveStream,
    state: Arc<StdMutex<ClientConnectionState>>,
    notify: Arc<Notify>,
    uni_tx: mpsc::Sender<WtRecvStream>,
) {
    let mut type_buf: Vec<u8> = Vec::new();

    let classified = loop {
        match recv_stream.receive().await {
            Ok(Some(data)) => {
                type_buf.extend_from_slice(&data);
                match classify_uni_stream_checked(&type_buf) {
                    Ok(result) => break Some(result),
                    Err(StreamHeaderDecodeError::BufferTooShort) => continue,
                    Err(_) => return,
                }
            }
            Ok(None) => {
                let _ = state
                    .lock()
                    .expect("connection state mutex must not be poisoned")
                    .feed_stream_only(stream_id, &[], true);
                notify.notify_one();
                return;
            }
            Err(_) => return,
        }
    };

    match classified {
        Some(ClassifiedUniStream::WebTransport { data_offset, .. }) => {
            let pending = type_buf[data_offset..].to_vec();
            let wt_recv = WtRecvStream::new(stream_id, recv_stream, pending);
            let _ = uni_tx.send(wt_recv).await;
        }
        _ => {
            let _ = state
                .lock()
                .expect("connection state mutex must not be poisoned")
                .feed_stream_only(stream_id, &type_buf, false);
            notify.notify_one();
            while let Ok(Some(data)) = recv_stream.receive().await {
                let _ = state
                    .lock()
                    .expect("connection state mutex must not be poisoned")
                    .feed_stream_only(stream_id, &data, false);
                notify.notify_one();
            }
            let _ = state
                .lock()
                .expect("connection state mutex must not be poisoned")
                .feed_stream_only(stream_id, &[], true);
            notify.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1024 バイト以下はそのまま返す
    #[test]
    fn truncate_close_session_message_keeps_short_message() {
        assert_eq!(truncate_close_session_message("ok"), "ok");
        assert_eq!(
            truncate_close_session_message(&"a".repeat(1024)).len(),
            1024
        );
    }

    /// ASCII のみで 1024 を超える場合はちょうど 1024 バイトに切る
    #[test]
    fn truncate_close_session_message_cuts_ascii_at_limit() {
        let message = "a".repeat(1025);
        let truncated = truncate_close_session_message(&message);
        assert_eq!(truncated.len(), 1024);
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    /// 1024 バイト境界がマルチバイト文字の途中になる場合は文字境界まで戻す
    ///
    /// draft-ietf-webtrans-http3-16 §6: truncate は UTF-8 character boundary で行う MUST。
    #[test]
    fn truncate_close_session_message_respects_utf8_boundary() {
        // 'あ' は 3 バイト。1022 バイトの ASCII の直後に置くと 1024 が文字の途中になる
        let mut message = "a".repeat(1022);
        message.push('あ'); // bytes 1022..1025
        message.push('あ');
        let truncated = truncate_close_session_message(&message);
        assert_eq!(truncated.len(), 1022);
        assert!(truncated.is_char_boundary(truncated.len()));
        assert!(!truncated.contains('あ'));
    }
}
