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
use shiguredo_http3::webtransport::ApplicationErrorCode;
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
    /// WebTransport CONNECT の `:authority` に使う値 (URL の authority)
    ///
    /// URL でポートを省略した場合はポートを含まない。
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
    /// draft-ietf-webtrans-http3-16 §3.2 は拡張 CONNECT で `:authority` と `:path` を
    /// 設定する MUST を定める。SNI 用の `server_name` とは別に、target URI の authority を
    /// URL の表記どおり (ポートの有無も含めて) 指定するために使う。
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
                .with_io(crate::local_bind_addr(config.remote_addr))
                .map_err(TransportError::transport)?
                .with_datagram(datagram_endpoint)
                .map_err(TransportError::transport)?
                .start()
                .map_err(TransportError::transport)?
        } else if let Some(ref ca_pem) = config.ca_cert_pem {
            s2n_quic::Client::builder()
                .with_tls(ca_pem.as_str())
                .map_err(TransportError::transport)?
                .with_io(crate::local_bind_addr(config.remote_addr))
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
        let connection = client.connect(connect).await?;

        let state = Arc::new(StdMutex::new(ClientConnectionState::new(
            config.h3_settings,
        )));

        // QUIC transport parameter レベルの前提条件を注入する
        //
        // draft-ietf-webtrans-http3-16 Section 3.1 / 4.6 は WebTransport CONNECT の前に
        // ピアの transport parameter (max_datagram_frame_size > 0 と RESET_STREAM_AT 対応) を
        // 検証することを求める。I/O 層がその結果を注入しないと CONNECT は拒否される。
        //
        // - max_datagram_frame_size: 本 example は s2n-quic の datagram provider を有効にして
        //   接続しており、ピアが DATAGRAM (RFC 9221) を広告しない場合は以降の WebTransport
        //   datagram 送受信が成立しない。s2n-quic の公開 API にピアの
        //   max_datagram_frame_size を取得する手段が無いため、provider を有効にしていることを
        //   根拠に true を渡す (moqt-example-transport は常に datagram を有効にして接続する)。
        // - reset_stream_at: s2n-quic は RESET_STREAM_AT を送出しないため false を渡す。
        //   本 example が接続する draft (draft-15 相当) では必須ではない。
        {
            let mut s = state
                .lock()
                .expect("connection state mutex must not be poisoned");
            s.h3_conn
                .set_webtransport_transport_verified(true, false)
                .map_err(|e| {
                    TransportError::Internal(format!(
                        "failed to set WebTransport transport parameters: {e}"
                    ))
                })?;
        }

        // 接続を分割し、単方向ストリームの受信タスクを先に起動する
        //
        // draft-ietf-webtrans-http3-16 Section 3.1 / 4.6 は WebTransport CONNECT を
        // 送る前に peer の SETTINGS (wt_enabled / enable_connect_protocol / h3_datagram) を
        // 受信していることを要求する。peer の SETTINGS はサーバーの制御ストリーム
        // (単方向) で届くため、CONNECT を送る前に単方向ストリームを受信して
        // HTTP/3 状態へ流し込む必要がある。
        let (mut handle, stream_acceptor) = connection.split();
        let (mut bidi_acceptor, mut uni_acceptor) = stream_acceptor.split();

        let unblock_notify = Arc::new(Notify::new());
        let (uni_tx, uni_rx) = mpsc::channel::<WtRecvStream>(16);
        let (bi_tx, bi_rx) = mpsc::channel::<(WtSendStream, WtRecvStream)>(16);

        // 単方向ストリーム受信タスク
        //
        // H3 の制御 / QPACK ストリームは HTTP/3 状態へ流し、WebTransport の
        // 単方向ストリームは `uni_tx` へ渡す。
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

        // H3 ストリーム (制御 + QPACK encoder/decoder) を初期化する
        let mut control_send = handle.open_send_stream().await?;
        let encoder_send = handle.open_send_stream().await?;
        let decoder_send = handle.open_send_stream().await?;

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

        // peer の SETTINGS を待ってから CONNECT を送る
        wait_for_peer_settings(&state, &unblock_notify).await?;

        // CONNECT リクエスト (双方向ストリーム) を開く
        let connect_stream = handle.open_bidirectional_stream().await?;
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

        // 双方向ストリーム受信タスク
        //
        // サーバー (MOQT relay) が開始する request stream は双方向ストリームで届く。
        // WT の双方向ストリームヘッダー (0x41 + session_id) を読み捨ててから
        // payload 部分を `WtRecvStream` として公開する。session_id の検証には
        // CONNECT stream の id が要るため、CONNECT を開いた後に起動する。
        let state_for_bi = Arc::clone(&state);
        let notify_for_bi = Arc::clone(&unblock_notify);
        tokio::spawn(async move {
            while let Ok(Some(stream)) = bidi_acceptor.accept_bidirectional_stream().await {
                let state = Arc::clone(&state_for_bi);
                let notify = Arc::clone(&notify_for_bi);
                let bi_tx = bi_tx.clone();
                let stream_id: u64 = stream.id();
                tokio::spawn(route_bi_stream(
                    connect_stream_id,
                    stream_id,
                    stream,
                    state,
                    notify,
                    bi_tx,
                ));
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
            uni_rx: Some(uni_rx),
            bi_rx: Some(bi_rx),
            state,
        })
    }
}

/// peer の SETTINGS 受信を待つ
///
/// draft-ietf-webtrans-http3-16 Section 3.1 / 4.6 は WebTransport CONNECT の送信前に
/// peer の SETTINGS を受信していることを要求する。待たずに送ると
/// `WtSetupError::PeerSettingsNotReceived` で失敗する。
/// peer の SETTINGS はサーバーの制御ストリーム (単方向) で届き、受信タスクが
/// HTTP/3 状態へ流し込むたびに `notify` で通知する。
async fn wait_for_peer_settings(
    state: &Arc<StdMutex<ClientConnectionState>>,
    notify: &Notify,
) -> Result<()> {
    /// peer SETTINGS を待つ上限 (ms)
    ///
    /// サーバーは接続直後に制御ストリームを開くため通常は 1 RTT 以内に届く。
    /// 届かない場合は接続が壊れているため、待ち続けずに失敗させる。
    const PEER_SETTINGS_TIMEOUT_MS: u64 = 10_000;

    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_millis(PEER_SETTINGS_TIMEOUT_MS);
    loop {
        // 条件確認より先に通知の受信登録を行う (取りこぼしを避ける)
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        {
            let s = state
                .lock()
                .expect("connection state mutex must not be poisoned");
            if s.h3_conn.peer_settings().is_some() {
                return Ok(());
            }
        }
        tokio::select! {
            _ = &mut notified => {}
            _ = tokio::time::sleep_until(deadline) => {
                return Err(TransportError::Internal(
                    "peer SETTINGS not received before WebTransport CONNECT".to_string(),
                ));
            }
        }
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
    /// 単方向受信ストリームの receiver。`take_uni_receiver` で取り出すと None になる
    uni_rx: Option<mpsc::Receiver<WtRecvStream>>,
    /// 双方向受信ストリームの receiver。`take_bi_receiver` で取り出すと None になる
    bi_rx: Option<mpsc::Receiver<(WtSendStream, WtRecvStream)>>,
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
    ///
    /// `take_uni_receiver` で receiver を取り出した後は `StreamClosed` を返す。
    pub async fn accept_uni_stream(&mut self) -> Result<WtRecvStream> {
        let rx = self.uni_rx.as_mut().ok_or(TransportError::StreamClosed)?;
        rx.recv().await.ok_or(TransportError::StreamClosed)
    }

    /// サーバーが開始した双方向ストリームを受け付ける
    ///
    /// WT の双方向ストリームヘッダーは受信タスクが読み捨て済みである。
    /// `take_bi_receiver` で receiver を取り出した後は `StreamClosed` を返す。
    pub async fn accept_bi_stream(&mut self) -> Result<(WtSendStream, WtRecvStream)> {
        let rx = self.bi_rx.as_mut().ok_or(TransportError::StreamClosed)?;
        rx.recv().await.ok_or(TransportError::StreamClosed)
    }

    /// 単方向受信ストリームの receiver を取り出す
    ///
    /// `accept_uni_stream` は `&mut self` の await であり、`Arc<Mutex<WtSession>>` を
    /// 共有していると待機中ロックを保持してしまう。受信ループのように長く待つ呼び出しは
    /// receiver を取り出してロック外で `recv()` する。
    /// 取り出しは 1 回だけで、2 回目以降は None を返す。
    pub fn take_uni_receiver(&mut self) -> Option<mpsc::Receiver<WtRecvStream>> {
        self.uni_rx.take()
    }

    /// 双方向受信ストリームの receiver を取り出す
    ///
    /// 詳細は `take_uni_receiver` を参照する。
    pub fn take_bi_receiver(&mut self) -> Option<mpsc::Receiver<(WtSendStream, WtRecvStream)>> {
        self.bi_rx.take()
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
// WebTransport のエラーコード変換
// ---------------------------------------------------------------------------

/// MOQT のエラーコードを WebTransport の HTTP/3 エラーコードへ remap する
///
/// draft-ietf-webtrans-http3-16 §4.4 (Resetting Data Streams) は、WebTransport の
/// アプリケーションエラーコード (0x00000000-0xffffffff) を WT_APPLICATION_ERROR の範囲へ
/// remap する MUST を定める。0x00000000 が 0x52e4a40fa8db、0xffffffff が 0x52e5ac983162 に
/// 対応し、予約コードポイント (0x1f * N + 0x21) はスキップする。
/// 変換自体は `shiguredo_http3::webtransport::ApplicationErrorCode` に実装済みのものを使う。
///
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
///
/// MOQT のエラーコードは §12.5 (Stream Reset Error Codes) の登録値だけでなく、
/// §13 (Grease) の greasing 値 (`0x7f * N + 0x9D`。`0x9D, 0x11C, ..., 0x3fffffffffffffde` を取り得る) も含む。
/// §16.11.4 の Stream Reset Error Codes registry も同じ greasing 予約を持つ。
/// §4.4 は WebTransport のアプリケーションエラーコードを 32 ビットに限るため、
/// 32 ビットを超える greasing 値は WebTransport 経路では remap できずリセットを送れない。
/// 黙って切り捨てずエラーにする (2^32 以上 2^62 未満は従来は remap せず wire に載っていたため、
/// この防御は挙動変更になる)。公開 API (`Session::reset_outgoing_data_stream_with_code` など) は
/// 任意の `u64` を受け付けるため、32 ビットを超える値が到達し得る。
/// この制限は §4.4 の u32 制限に由来し、`RequestStreamEnd` の型や greasing の扱いとは別に
/// WebTransport 経路の既知の制限として扱う。
fn moqt_to_wt_code(code: u64) -> Result<u64> {
    let app_code = u32::try_from(code).map_err(|_| {
        TransportError::Internal(format!(
            "MOQT error code {code:#x} does not fit in a WebTransport application error code (0x00000000-0xffffffff)"
        ))
    })?;
    Ok(ApplicationErrorCode::to_http3_code(app_code))
}

/// HTTP/3 のエラーコードを WebTransport のアプリケーションエラーコードへ戻す
///
/// WT_APPLICATION_ERROR の範囲外のコード (WT_SESSION_GONE = 0x170d7b68 などのプロトコルコード) と
/// 予約コードポイント (0x1f * N + 0x21) は `None` を返す (draft-ietf-webtrans-http3-16 §4.4)。
///
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
fn wt_to_moqt_code(http3_code: u64) -> Option<u32> {
    ApplicationErrorCode::from_http3_code(http3_code)
}

/// RESET_STREAM で受信した HTTP/3 のエラーコードを `RequestStreamEnd::Reset` へ入れる値に変換する
///
/// remap できれば MOQT のエラーコードを返す。remap できない場合について §4.4 は
/// "the stream is still considered reset, but the error code is not mapped to a WebTransport
/// application error code." と定めるが、`RequestStreamEnd::Reset` の `error_code` は必須の `u64` で
/// 「アプリケーションエラーコード無し」を表す値を持たない (`Option<u64>` への型変更は
/// `SessionEvent` / `TerminationReason` の公開 API とその構築サイトに波及するため行わない)。
/// そのため wire の HTTP/3 コードをそのまま返し、生値を `tracing::warn!` でログに残す。
/// 呼び出し元では「WT_APPLICATION_ERROR の範囲だったものを remap した MOQT コード」と
/// 「範囲外の HTTP/3 コードをそのまま入れた値」の 2 種が区別されずに渡る。MOQT §12.5 のコードは
/// 小さな値 (現行は 0x0-0x12) なので前者とは区別できるが、後者には MOQT のコードと
/// 区別できない値もある。この扱いは `RequestStreamEnd::Reset` の型を変更するまでの暫定である。
///
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
fn wt_reset_error_code(http3_code: u64) -> u64 {
    match wt_to_moqt_code(http3_code) {
        Some(code) => u64::from(code),
        None => {
            if ApplicationErrorCode::is_application_error(http3_code) {
                // 数値上は WT_APPLICATION_ERROR の範囲内だが予約コードポイント (0x1f * N + 0x21)。
                // 依存に予約コードポイントの判定 API が無いため、`from_http3_code` が None を返す条件と
                // `is_application_error` が範囲だけを見ることの組み合わせで判定している。
                // shiguredo_http3 を更新したらこの 2 つの条件が変わっていないか確認すること
                tracing::warn!(
                    "RESET_STREAM received with a reserved HTTP/3 error code point: {http3_code:#x}"
                );
            } else {
                // 範囲外 (WT_SESSION_GONE などのプロトコルコード)
                tracing::warn!(
                    "RESET_STREAM received with an HTTP/3 error code outside the WebTransport application error range: {http3_code:#x}"
                );
            }
            http3_code
        }
    }
}

/// MOQT のエラーコードを s2n-quic のアプリケーションエラーへ変換する
///
/// remap した値だけを `s2n_quic::application::Error::new` に渡す経路をこの関数に閉じ、
/// `WtSendStream::reset` / `WtRecvStream::stop_sending` から呼ぶ。
fn moqt_application_error(error_code: u64) -> Result<s2n_quic::application::Error> {
    s2n_quic::application::Error::new(moqt_to_wt_code(error_code)?)
        .map_err(TransportError::transport)
}

/// wire の HTTP/3 エラーコードから `RequestStreamEnd::Reset` を組み立てる
///
/// `reliable_size` は `RESET_STREAM_AT` を受信したときに埋まる値だが、s2n-quic は
/// `RESET_STREAM_AT` に対応していないため常に `None` にする。
fn wt_reset_stream_end(http3_code: u64) -> RequestStreamEnd {
    RequestStreamEnd::Reset {
        error_code: wt_reset_error_code(http3_code),
        reliable_size: None,
    }
}

/// `WtRecvStream::recv_chunk` のエラー分岐を `RecvChunk` へ変換する
///
/// `RESET_STREAM` を受信したときは wire の HTTP/3 コードを MOQT のコードへ戻して
/// `RequestStreamEnd::Reset` にし、それ以外のストリームエラーはそのままエラーにする。
/// I/O ハンドルを持たないため `recv_chunk` から切り出して単体テストできるようにしている。
fn wt_recv_end(error: s2n_quic::stream::Error) -> Result<RecvChunk> {
    match error {
        s2n_quic::stream::Error::StreamReset { error, .. } => {
            Ok(RecvChunk::End(wt_reset_stream_end(error.into())))
        }
        e => Err(TransportError::transport(e)),
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
    ///
    /// MOQT のエラーコードは WebTransport のアプリケーションエラーコードとして
    /// WT_APPLICATION_ERROR の範囲へ remap してから送る (draft-ietf-webtrans-http3-16 §4.4)。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fn reset(&mut self, error_code: u64) -> Result<()> {
        self.send
            .reset(moqt_application_error(error_code)?)
            .map_err(TransportError::transport)
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
#[derive(PartialEq, Eq)]
pub enum RecvChunk {
    /// 受信データ
    Data(Vec<u8>),
    /// ストリーム終端
    End(RequestStreamEnd),
}

impl std::fmt::Debug for RecvChunk {
    // 受信データは長さだけを出す。`#[derive(Debug)]` だとメディアの payload 全体が
    // ログに載るため手書きする。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Data(data) => write!(f, "Data({} bytes)", data.len()),
            Self::End(end) => write!(f, "End({end:?})"),
        }
    }
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
            Err(e) => wt_recv_end(e),
        }
    }

    /// 受信方向へ STOP_SENDING を送出する (QUIC STOP_SENDING)
    ///
    /// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection): 受信方向の
    /// cancel は STOP_SENDING で行う。error code は §12.5 (Stream Reset Error Codes) から選ぶ。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fn stop_sending(&mut self, error_code: u64) -> Result<()> {
        self.recv
            .stop_sending(moqt_application_error(error_code)?)
            .map_err(TransportError::transport)
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

/// 双方向ストリームをルーティングする
///
/// サーバーが開始する双方向ストリームには WT の双方向ストリームヘッダー
/// (0x41 + session_id) が先頭に付く (draft-ietf-webtrans-http3-16 Section 4.3)。
/// ヘッダーを読み捨てた残りを payload として公開する。
/// 自 session 以外の session_id を持つストリームは MOQT の request stream ではないため
/// HTTP/3 層へ渡す。
async fn route_bi_stream(
    session_id: u64,
    stream_id: u64,
    stream: s2n_quic::stream::BidirectionalStream,
    state: Arc<StdMutex<ClientConnectionState>>,
    notify: Arc<Notify>,
    bi_tx: mpsc::Sender<(WtSendStream, WtRecvStream)>,
) {
    let (mut recv_stream, send_stream) = stream.split();
    let mut header_buf: Vec<u8> = Vec::new();

    loop {
        match recv_stream.receive().await {
            Ok(Some(data)) => {
                header_buf.extend_from_slice(&data);
                match StreamHeader::decode_bidirectional_checked(&header_buf) {
                    Ok((header, data_offset)) if header.session_id() == session_id => {
                        let pending = header_buf[data_offset..].to_vec();
                        let wt_send = WtSendStream {
                            stream_id,
                            send: send_stream,
                        };
                        let wt_recv = WtRecvStream::new(stream_id, recv_stream, pending);
                        let _ = bi_tx.send((wt_send, wt_recv)).await;
                        return;
                    }
                    Ok(_) => {
                        // 他 session の双方向ストリームは HTTP/3 層の管轄である
                        let _ = state
                            .lock()
                            .expect("connection state mutex must not be poisoned")
                            .feed_stream_only(stream_id, &header_buf, false);
                        notify.notify_one();
                        return;
                    }
                    Err(StreamHeaderDecodeError::BufferTooShort) => continue,
                    Err(_) => return,
                }
            }
            Ok(None) => return,
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// WT_APPLICATION_ERROR 範囲外のプロトコルエラーコード (draft-ietf-webtrans-http3-16 §9.5)
    const WT_SESSION_GONE: u64 = shiguredo_http3::webtransport::ErrorCode::SessionGone as u64;

    /// 予約コードポイント (0x1f * N + 0x21) のうち WT_APPLICATION_ERROR 範囲に入る最初の値
    ///
    /// draft-ietf-webtrans-http3-16 §4.4 は予約コードポイントをアプリケーションエラーコードの
    /// remap 対象外とする。`ApplicationErrorCode::to_http3_code` は `n / 0x1e` の加算で
    /// 予約コードポイントを飛ばすため、変換結果がこの値になることはない。
    fn first_reserved_code_point_in_range() -> u64 {
        let base = ApplicationErrorCode::FIRST - 0x21;
        base.next_multiple_of(0x1f) + 0x21
    }

    /// `tracing::warn!` の回数とメッセージを記録するテスト用サブスクライバ
    ///
    /// 観測専用であり、変換関数の処理や戻り値には関与しない (テスト対象を置き換えない)。
    struct WarnRecorder {
        count: Arc<AtomicUsize>,
        messages: Arc<StdMutex<Vec<String>>>,
    }

    impl tracing::Subscriber for WarnRecorder {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            if *event.metadata().level() == tracing::Level::WARN {
                self.count.fetch_add(1, Ordering::SeqCst);
                let mut collector = MessageCollector::default();
                event.record(&mut collector);
                self.messages
                    .lock()
                    .expect("warn のメッセージを記録できること")
                    .push(collector.message);
            }
        }

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    /// `tracing` のイベントから `message` フィールドを取り出す
    #[derive(Default)]
    struct MessageCollector {
        message: String,
    }

    impl tracing::field::Visit for MessageCollector {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.message = format!("{value:?}");
            }
        }
    }

    /// warn を記録するサブスクライバを設定して関数を実行し、結果と記録内容を返す
    fn with_warn_recorder<T>(f: impl FnOnce() -> T) -> (T, usize, Vec<String>) {
        let count = Arc::new(AtomicUsize::new(0));
        let messages = Arc::new(StdMutex::new(Vec::new()));
        let recorder = WarnRecorder {
            count: Arc::clone(&count),
            messages: Arc::clone(&messages),
        };
        let returned = tracing::subscriber::with_default(recorder, f);
        let recorded = messages
            .lock()
            .expect("warn のメッセージを取得できること")
            .clone();
        (returned, count.load(Ordering::SeqCst), recorded)
    }

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

    /// MOQT の登録コードを remap すると往復して元の値に戻る (draft-ietf-webtrans-http3-16 §4.4)
    #[test]
    fn moqt_to_wt_code_round_trips_registered_codes() {
        for moqt_code in [0x0, 0x1, 0x12] {
            let http3_code = moqt_to_wt_code(moqt_code).expect("remap に成功すること");
            assert!(
                ApplicationErrorCode::is_application_error(http3_code),
                "{moqt_code:#x} の remap 結果 {http3_code:#x} が WT_APPLICATION_ERROR の範囲に入ること"
            );
            assert_eq!(
                wt_to_moqt_code(http3_code),
                Some(moqt_code as u32),
                "{moqt_code:#x} が往復して戻ること"
            );
        }
    }

    /// §4.4 のアンカー (0x00000000 / 0xffffffff の対応) をリテラルで固定する
    #[test]
    fn code_conversion_matches_specification_anchors() {
        assert_eq!(
            moqt_to_wt_code(0x00000000).expect("remap に成功すること"),
            0x52e4a40fa8db,
            "0x00000000 が WT_APPLICATION_ERROR の先頭に対応すること"
        );
        assert_eq!(
            moqt_to_wt_code(0xffffffff).expect("remap に成功すること"),
            0x52e5ac983162,
            "0xffffffff が WT_APPLICATION_ERROR の末尾に対応すること"
        );
        assert_eq!(
            wt_to_moqt_code(0x52e4a40fa8db),
            Some(0x00000000),
            "WT_APPLICATION_ERROR の先頭が 0x00000000 に戻ること"
        );
        assert_eq!(
            wt_to_moqt_code(0x52e5ac983162),
            Some(0xffffffff),
            "WT_APPLICATION_ERROR の末尾が 0xffffffff に戻ること"
        );
    }

    /// §4.4 の変換の境界値を remap しても往復して元の値に戻る
    #[test]
    fn moqt_to_wt_code_round_trips_boundary_codes() {
        for moqt_code in [0x1e_u64, 0x1f, u64::from(u32::MAX)] {
            let http3_code = moqt_to_wt_code(moqt_code).expect("remap に成功すること");
            assert!(
                ApplicationErrorCode::is_application_error(http3_code),
                "{moqt_code:#x} の remap 結果 {http3_code:#x} が WT_APPLICATION_ERROR の範囲に入ること"
            );
            assert_ne!(
                (http3_code - 0x21) % 0x1f,
                0,
                "{moqt_code:#x} の remap 結果が予約コードポイントにならないこと"
            );
            assert_eq!(
                wt_to_moqt_code(http3_code),
                Some(moqt_code as u32),
                "{moqt_code:#x} が往復して戻ること"
            );
        }
    }

    /// remap の結果は予約コードポイントにならない
    #[test]
    fn moqt_to_wt_code_never_produces_reserved_code_points() {
        let reserved = first_reserved_code_point_in_range();
        assert_eq!(
            (reserved - 0x21) % 0x1f,
            0,
            "{reserved:#x} が予約コードポイントの式 (0x1f * N + 0x21) を満たすこと"
        );
        assert_eq!(
            wt_to_moqt_code(reserved),
            None,
            "予約コードポイント {reserved:#x} は remap できないこと"
        );
        // 予約コードポイントは 0x1f 個ごとに 1 点現れるため、写像の 1 周期より広い範囲を全数確認する
        for moqt_code in 0..=0x1000_u64 {
            let http3_code = moqt_to_wt_code(moqt_code).expect("remap に成功すること");
            let nearest_reserved = ((http3_code - 0x21) / 0x1f) * 0x1f + 0x21;
            assert_ne!(
                http3_code, nearest_reserved,
                "{moqt_code:#x} の remap 結果 {http3_code:#x} が予約コードポイントでないこと"
            );
            assert!(
                ApplicationErrorCode::is_application_error(http3_code),
                "{moqt_code:#x} の remap 結果 {http3_code:#x} が範囲内であること"
            );
        }
        assert!(
            reserved < moqt_to_wt_code(0x1000).expect("remap に成功すること"),
            "確認範囲が最初の予約コードポイント {reserved:#x} を跨いでいること"
        );
    }

    /// WT_APPLICATION_ERROR の範囲外のコードは remap できない (draft-ietf-webtrans-http3-16 §4.4)
    #[test]
    fn wt_to_moqt_code_rejects_codes_outside_application_range() {
        for http3_code in [
            // セッション終了などのプロトコルエラーコード
            WT_SESSION_GONE,
            // WebTransport 拡張ではない任意の HTTP/3 コード
            0x12345678,
            // 範囲の直前と直後
            ApplicationErrorCode::FIRST - 1,
            ApplicationErrorCode::LAST + 1,
        ] {
            assert_eq!(
                wt_to_moqt_code(http3_code),
                None,
                "{http3_code:#x} は remap できないこと"
            );
        }
    }

    /// 範囲内のコードは MOQT のコードに戻り、範囲外は warn を出して生値のまま返る
    #[test]
    fn wt_reset_error_code_returns_moqt_code_or_passes_through() {
        let http3_code = moqt_to_wt_code(0x1).expect("remap に成功すること");
        let (returned, count, messages) = with_warn_recorder(|| {
            [
                wt_reset_error_code(http3_code),
                wt_reset_error_code(WT_SESSION_GONE),
                wt_reset_error_code(0x12345678),
            ]
        });
        assert_eq!(
            returned,
            [0x1, WT_SESSION_GONE, 0x12345678],
            "remap できたコードは MOQT のコードになり、できないコードは wire の値のまま返ること"
        );
        assert_eq!(
            count, 2,
            "remap できないコードごとに warn が 1 回だけ出ること (remap できたコードでは出ないこと)"
        );
        assert_eq!(
            messages,
            [WT_SESSION_GONE, 0x12345678].map(|http3_code| format!(
                "RESET_STREAM received with an HTTP/3 error code outside the WebTransport application error range: {http3_code:#x}"
            )),
            "範囲外の warn が生値を含み、予約コードポイントの文言と異なること"
        );
    }

    /// 予約コードポイントは範囲外と区別できる warn になる
    #[test]
    fn wt_reset_error_code_warns_reserved_code_points_separately() {
        let reserved = first_reserved_code_point_in_range();
        let (returned, count, messages) = with_warn_recorder(|| wt_reset_error_code(reserved));
        assert_eq!(returned, reserved, "予約コードポイントが生値のまま返ること");
        assert_eq!(count, 1, "予約コードポイントで warn が 1 回出ること");
        assert_eq!(
            messages,
            [format!(
                "RESET_STREAM received with a reserved HTTP/3 error code point: {reserved:#x}"
            )],
            "予約コードポイントを範囲外と表現しないこと"
        );
    }

    /// `recv_chunk` が組み立てる `RequestStreamEnd::Reset` に remap 後の値が入る
    #[test]
    fn wt_reset_stream_end_uses_remapped_code() {
        let http3_code = moqt_to_wt_code(0x12).expect("remap に成功すること");
        assert_eq!(
            wt_reset_stream_end(http3_code),
            RequestStreamEnd::Reset {
                error_code: 0x12,
                reliable_size: None,
            },
            "remap できたコードが MOQT のコードとして入ること"
        );
        assert_eq!(
            wt_reset_stream_end(WT_SESSION_GONE),
            RequestStreamEnd::Reset {
                error_code: WT_SESSION_GONE,
                reliable_size: None,
            },
            "remap できないコードが生値のまま入ること"
        );
    }

    /// remap した値で s2n-quic のアプリケーションエラーを構築する
    #[test]
    fn moqt_application_error_remaps_before_creating_s2n_error() {
        for (moqt_code, expected) in [
            (0x0_u64, 0x52e4a40fa8db_u64),
            (0x1, 0x52e4a40fa8dc),
            (0x12, 0x52e4a40fa8ed),
            (u64::from(u32::MAX), 0x52e5ac983162),
        ] {
            let error = moqt_application_error(moqt_code).expect("remap に成功すること");
            assert_eq!(
                u64::from(error),
                expected,
                "{moqt_code:#x} の wire の値が remap 後の値になること"
            );
        }
        assert!(
            moqt_application_error(u64::MAX).is_err(),
            "32 ビットを超える値は wire に載せないこと"
        );
    }

    /// `RESET_STREAM` の受信エラーを remap した `RequestStreamEnd::Reset` に変換する
    ///
    /// `StreamError` の variant は `#[non_exhaustive]` で、接続なしに `RESET_STREAM` の値を組み立てるには
    /// s2n-quic が公開している `stream_reset` を使うしかない (ドキュメント非公開の補助 API)。
    #[test]
    fn wt_recv_end_maps_stream_reset_to_remapped_request_stream_end() {
        for (http3_code, expected) in [
            (
                moqt_to_wt_code(0x12).expect("remap に成功すること"),
                0x12_u64,
            ),
            (WT_SESSION_GONE, WT_SESSION_GONE),
        ] {
            let application_error =
                s2n_quic::application::Error::new(http3_code).expect("varint に収まること");
            let end = wt_recv_end(s2n_quic::stream::Error::stream_reset(application_error))
                .expect("End に変換できること");
            assert_eq!(
                end,
                RecvChunk::End(RequestStreamEnd::Reset {
                    error_code: expected,
                    reliable_size: None,
                }),
                "wire の {http3_code:#x} が {expected:#x} として入ること"
            );
        }
    }

    /// `RESET_STREAM` 以外のストリームエラーはエラーのまま返る
    #[test]
    fn wt_recv_end_keeps_other_stream_errors() {
        let error = wt_recv_end(s2n_quic::stream::Error::send_after_finish())
            .expect_err("エラーになること");
        assert!(
            matches!(error, TransportError::Transport(_)),
            "ストリームエラーとして返ること: {error:?}"
        );
    }

    /// 32 ビットに収まらない MOQT のコードは切り捨てずエラーにする
    #[test]
    fn moqt_to_wt_code_rejects_values_beyond_32_bits() {
        for moqt_code in [u64::from(u32::MAX) + 1, u64::MAX] {
            let error = moqt_to_wt_code(moqt_code).expect_err("エラーになること");
            assert_eq!(
                error.to_string(),
                format!(
                    "internal error: MOQT error code {moqt_code:#x} does not fit in a WebTransport application error code (0x00000000-0xffffffff)"
                ),
                "{moqt_code:#x} のエラーメッセージ"
            );
        }
    }
}
