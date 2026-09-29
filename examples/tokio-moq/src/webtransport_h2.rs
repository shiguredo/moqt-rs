//! WebTransport over HTTP/2 クライアント実装
//!
//! TCP+TLS (rustls、ALPN `h2`) の上で shiguredo_http2 (Sans I/O) の `Connection` と
//! `webtransport::WtSession` を駆動し WebTransport セッションを確立する。
//! 節番号は draft-ietf-webtrans-http2-15 を参照する。
//!
//! WebTransport over HTTP/2 は、HTTP/2 の 1 本の Extended CONNECT ストリーム上に
//! Capsule Protocol でストリーム (WT_STREAM capsule) と DATAGRAM を多重化する。
//! セッション状態はすべて CONNECT ストリームの 1 本に閉じるため、このモジュールの
//! driver タスクが `Connection` と `WtSession` を所有し、[`WtH2Session`] /
//! [`WtH2SendStream`] / [`WtH2RecvStream`] は mpsc / oneshot で操作を依頼する。
//!
//! publisher (datagram 送信) と subscriber (datagram 受信 + 受信ストリーム accept) の
//! 双方が利用する union API を提供する。
//!
//! 本モジュールはクライアント例であり、サーバ側専用の要件 (Origin の検証や 405 応答など)
//! は対象外とする。

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};

use rustls::pki_types::pem::PemObject;
use shiguredo_http2::settings::DEFAULT_INITIAL_WINDOW_SIZE;
use shiguredo_http2::webtransport::stream::stream_id as wt_stream_id;
use shiguredo_http2::webtransport::{
    WtConfig, WtError, WtErrorKind, WtEvent, WtSession, WtStreamId,
};
use shiguredo_http2::{ErrorCode, Event, HeaderField, Limits, StreamId};
use shiguredo_moqt::session::types::RequestStreamEnd;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_rustls::TlsConnector;

use crate::error::{Result, TransportError};
use crate::webtransport::{
    WtSessionState, session_policy, update_session_state, wait_until_terminated,
};

/// WebTransport over HTTP/2 の ALPN プロトコル識別子 (RFC 9113 §3.3)
const H2_ALPN: &[u8] = b"h2";

/// WebTransport CONNECT の `:protocol` 値 (draft-ietf-webtrans-http2-15 §3.1)
const WEBTRANSPORT_PROTOCOL: &[u8] = b"webtransport";

/// WT 出力を `Connection::send_data` に 1 回で渡せる上限
///
/// shiguredo_http2 のストリーム送信バッファの固定容量 (`DEFAULT_INITIAL_WINDOW_SIZE` =
/// 65535) と同じ値である。1 回の `send_data` にそれ以上を渡すと全量拒否されるため、
/// WT の capsule は capsule 境界と無関係に分割して送る
/// (draft-ietf-webtrans-http2-15 §2 / RFC 9297 §3.1)。
const WT_SEND_CHUNK_SIZE: usize = DEFAULT_INITIAL_WINDOW_SIZE as usize;

/// peer の SETTINGS を待つ上限 (ms)
///
/// サーバーは接続直後に SETTINGS を送るため通常は 1 RTT 以内に届く。届かない場合は
/// 接続が壊れているため、待ち続けずに失敗させる。
const PEER_SETTINGS_TIMEOUT_MS: u64 = 10_000;

// ---------------------------------------------------------------------------
// クライアント設定
// ---------------------------------------------------------------------------

/// WebTransport over HTTP/2 クライアント設定
pub struct ClientConfig {
    /// 接続先アドレス
    pub remote_addr: SocketAddr,
    /// SNI / 証明書検証に使うサーバー名 (ポートを含まない)
    pub server_name: String,
    /// CONNECT の `:authority` に使う値 (URL の authority)
    ///
    /// URL でポートを省略した場合はポートを含まない。
    pub authority: String,
    /// TLS の CA 証明書 (PEM)
    pub ca_cert_pem: Option<String>,
    /// 証明書検証を無効化するかどうか (開発用)
    pub disable_cert_validation: bool,
    /// datagram をアプリへ引き渡すか
    ///
    /// subscriber (datagram 受信側) のみ `true` にする。publisher は datagram を
    /// 送信するだけで受信しないため `false` のままとし、未消費の datagram を
    /// 溜め込まないようにする。
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
            receive_datagrams: false,
        }
    }

    /// CONNECT の `:authority` を設定する
    ///
    /// draft-ietf-webtrans-http2-15 §3.2 は Extended CONNECT で `:authority` と `:path` を
    /// 設定する MUST を定める。SNI 用の `server_name` とは別に、target URI の authority を
    /// URL の表記どおり (ポートの有無も含めて) 指定するために使う。
    pub fn authority(mut self, authority: impl Into<String>) -> Self {
        self.authority = authority.into();
        self
    }

    /// TLS の CA 証明書 (PEM) を設定する
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

    /// datagram をアプリへ引き渡す (subscriber 側で使用)
    pub fn receive_datagrams(mut self) -> Self {
        self.receive_datagrams = true;
        self
    }
}

impl std::fmt::Debug for ClientConfig {
    // CA 証明書 (PEM) は大きいため、有無だけを出す
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientConfig")
            .field("remote_addr", &self.remote_addr)
            .field("server_name", &self.server_name)
            .field("authority", &self.authority)
            .field("ca_cert_pem", &self.ca_cert_pem.as_ref().map(|_| "<PEM>"))
            .field("disable_cert_validation", &self.disable_cert_validation)
            .field("receive_datagrams", &self.receive_datagrams)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// TLS 設定
// ---------------------------------------------------------------------------

/// rustls のクライアント設定を構築する (ALPN `h2` 固定)
///
/// `disable_cert_validation` が true の場合と CA 証明書が指定されていない場合は
/// 証明書検証を行わない (開発用)。両方指定された場合は無効化を優先する。
fn build_tls_client_config(config: &ClientConfig) -> Result<Arc<rustls::ClientConfig>> {
    let use_insecure = config.disable_cert_validation || config.ca_cert_pem.is_none();
    let mut tls_config = if use_insecure {
        if config.ca_cert_pem.is_some() {
            tracing::warn!(
                "TLS certificate verification is disabled by ClientConfig::insecure; the CA certificate is ignored"
            );
        }
        // 開発用: 証明書検証を無効化する (QUIC / HTTP/3 経路と対称の警告は呼び出し側が出す)
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification::new()))
            .with_no_client_auth()
    } else {
        // `use_insecure` が false のときは CA 証明書が必ずある
        let pem = config
            .ca_cert_pem
            .as_ref()
            .expect("ca_cert_pem is present when certificate verification is enabled");
        let mut roots = rustls::RootCertStore::empty();
        for cert in rustls::pki_types::CertificateDer::pem_slice_iter(pem.as_bytes()) {
            let cert = cert.map_err(|e| {
                TransportError::Internal(format!("failed to parse the CA certificate: {e}"))
            })?;
            roots.add(cert).map_err(|e| {
                TransportError::Internal(format!("failed to add the CA certificate: {e}"))
            })?;
        }
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth()
    };
    tls_config.alpn_protocols = vec![H2_ALPN.to_vec()];
    Ok(Arc::new(tls_config))
}

/// 証明書を検証しない `ServerCertVerifier` (開発用)
///
/// 署名検証だけは既定のプロバイダに委ね、証明書チェーンとサーバー名の検証を行わない。
#[derive(Debug)]
struct NoCertificateVerification(Arc<rustls::crypto::CryptoProvider>);

impl NoCertificateVerification {
    fn new() -> Self {
        Self(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
    }
}

impl rustls::client::danger::ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

// ---------------------------------------------------------------------------
// HTTP/2 の非同期 I/O ラッパー
// ---------------------------------------------------------------------------

/// クライアントの TLS ストリーム型
type TlsStream = tokio_rustls::client::TlsStream<TcpStream>;

/// shiguredo_http2 の Sans I/O `Connection` を TCP+TLS ストリーム上で駆動する
///
/// tokio-http2 の `Connection` と同じ構成であり、`poll_output` の書き出し (`flush`) と
/// 受信 (`recv_into_connection`) を提供する。ストリーム型を汎用にするのは、
/// 結合テストでサーバー側の `tokio_rustls::server::TlsStream` も同じ手順で駆動するためである。
struct H2Io<S> {
    stream: S,
    conn: shiguredo_http2::Connection,
    recv_buf: Vec<u8>,
}

impl<S> H2Io<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// クライアントコネクションを作成する
    fn client(stream: S, limits: Limits) -> Self {
        Self {
            stream,
            conn: shiguredo_http2::Connection::client(limits),
            recv_buf: vec![0u8; 16384],
        }
    }

    /// サーバーコネクションを作成する (結合テスト用)
    #[cfg(test)]
    fn server(stream: S, limits: Limits) -> Self {
        Self {
            stream,
            conn: shiguredo_http2::Connection::server(limits),
            recv_buf: vec![0u8; 16384],
        }
    }

    /// 接続プリフェイスと初期 SETTINGS を送信する (クライアント。RFC 9113 §3.4)
    async fn send_preface(&mut self) -> Result<()> {
        self.conn.initiate()?;
        self.flush().await
    }

    /// 送信データをすべて書き出す
    async fn flush(&mut self) -> Result<()> {
        let mut has_data = false;
        while let Some(data) = self.conn.poll_output() {
            self.stream
                .write_all(&data)
                .await
                .map_err(TransportError::transport)?;
            has_data = true;
        }
        if has_data {
            self.stream
                .flush()
                .await
                .map_err(TransportError::transport)?;
        }
        Ok(())
    }

    /// 受信して接続状態へ流し込む
    ///
    /// 戻り値は受信したバイト数である。0 は接続が閉じたことを表す。
    async fn recv_into_connection(&mut self) -> Result<usize> {
        let n = self
            .stream
            .read(&mut self.recv_buf)
            .await
            .map_err(TransportError::transport)?;
        if n == 0 {
            return Err(TransportError::ConnectionClosed);
        }
        self.conn.feed(&self.recv_buf[..n])?;
        self.conn.process()?;
        Ok(n)
    }

    /// イベントが発生するまで読み進め、1 件返す
    async fn next_event(&mut self) -> Result<Event> {
        loop {
            // イベントを返す前に必ず書き出しを済ませる (応答・WINDOW_UPDATE を滞留させない)
            self.flush().await?;
            if let Some(event) = self.conn.poll_event() {
                return Ok(event);
            }
            self.recv_into_connection().await?;
        }
    }

    /// ローカル SETTINGS を返す
    fn local_settings(&self) -> &shiguredo_http2::Settings {
        self.conn.local_settings()
    }

    /// ピア SETTINGS を返す
    fn remote_settings(&self) -> &shiguredo_http2::Settings {
        self.conn.remote_settings()
    }

    /// 指定ストリームに未送信の送信データまたは保留中の END_STREAM があるかを返す
    fn has_pending_send_data(&self, stream_id: StreamId) -> bool {
        self.conn.has_pending_send_data(stream_id)
    }
}

// ---------------------------------------------------------------------------
// 接続
// ---------------------------------------------------------------------------

/// WebTransport over HTTP/2 クライアント
pub struct WtH2Client;

impl WtH2Client {
    /// WebTransport セッションを確立する
    ///
    /// 1. TCP 接続と TLS ハンドシェイク (ALPN `h2`)
    /// 2. HTTP/2 の接続プリフェイスと SETTINGS (WebTransport の初期フロー制御値を広告する)
    /// 3. peer SETTINGS の待機 (`SETTINGS_ENABLE_CONNECT_PROTOCOL` と `SETTINGS_WT_ENABLED`
    ///    の確認。draft-ietf-webtrans-http2-15 §3.1)
    /// 4. Extended CONNECT (`:protocol=webtransport`) の送信と 2xx 応答の待機 (§3.2)
    /// 5. driver タスクの起動
    ///
    /// # Errors
    ///
    /// - TCP / TLS の失敗、peer SETTINGS が WebTransport に対応しない場合 (§3.1)
    /// - CONNECT が 2xx 以外で拒否された場合 (§3.2)
    /// - 2xx 応答の `WT-Protocol` が無い、または送った候補に一致しない場合 (§3.3)
    pub async fn connect(config: ClientConfig, path: &str) -> Result<WtH2Session> {
        // 1. TCP + TLS
        let tls_config = build_tls_client_config(&config)?;
        let tcp = TcpStream::connect(config.remote_addr)
            .await
            .map_err(TransportError::transport)?;
        let server_name = rustls::pki_types::ServerName::try_from(config.server_name.clone())
            .map_err(|e| {
                TransportError::Internal(format!(
                    "invalid server name '{}': {e}",
                    config.server_name
                ))
            })?;
        let tls_stream = TlsConnector::from(tls_config)
            .connect(server_name, tcp)
            .await
            .map_err(TransportError::transport)?;
        tracing::info!(
            "Connected to {} over TLS (HTTP/2) as {}",
            config.remote_addr,
            config.authority
        );

        // 2. HTTP/2 の初期化
        //
        // WebTransport の初期フロー制御値は SETTINGS で広告する (§4.3.1)。
        // ローカル設定 (`WtConfig::default`) と広告値が一致するよう同じ値を使う。
        let wt_config = WtConfig::default();
        let limits = build_limits(&wt_config)?;
        let mut io = H2Io::client(tls_stream, limits);
        io.send_preface().await?;

        // 3. peer SETTINGS の待機
        wait_for_peer_settings(&mut io).await?;

        // 4. CONNECT リクエスト
        let headers = build_connect_headers(&config.authority, path)?;
        let connect_stream = io.conn.start_stream(headers, false)?;
        io.flush().await?;
        wait_for_connect_response(&mut io, connect_stream).await?;
        tracing::info!(
            "WebTransport session established: connect_stream={}",
            connect_stream.as_u32()
        );

        // 5. WtSession を構築する
        //
        // セッション確立時に適用する初期値は、ACK 済みの自広告 SETTINGS と
        // ピアの SETTINGS である (§4.3.1)。未広告項目の Initial Value は 0 のため、
        // ピア設定は `peer_default` から始める (§11.2)。
        let mut local_config = WtConfig::default();
        local_config.overlay_settings(io.local_settings());
        let mut peer_config = WtConfig::peer_default();
        peer_config.overlay_settings(io.remote_settings());
        let mut wt_session = WtSession::client(local_config, peer_config);
        wt_session.initiate()?;

        // 6. driver タスクの起動
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (uni_tx, uni_rx) = mpsc::unbounded_channel();
        let (bi_tx, bi_rx) = mpsc::unbounded_channel();
        let (session_state_tx, _) = watch::channel(WtSessionState::Active);
        let datagrams = Arc::new(StdMutex::new(VecDeque::new()));
        let session_consumed = Arc::new(AtomicU64::new(0));

        let driver_session_state = session_state_tx.clone();
        let driver_datagrams = Arc::clone(&datagrams);
        let driver_consumed = Arc::clone(&session_consumed);
        let driver_cmd_tx = cmd_tx.clone();
        let receive_datagrams = config.receive_datagrams;
        let driver = tokio::spawn(async move {
            let mut state = DriverState {
                io,
                connect_stream,
                wt_session,
                cmd_tx: driver_cmd_tx,
                cmd_rx,
                uni_tx,
                bi_tx,
                datagrams: driver_datagrams,
                receive_datagrams,
                session_state: driver_session_state,
                streams: HashMap::new(),
                pending_sends: HashMap::new(),
                wt_out_pending: Vec::new(),
                stop_sending_sent: HashSet::new(),
                stop_sending_received: HashSet::new(),
                session_received: 0,
                session_consumed: driver_consumed,
            };
            state.run().await;
        });

        Ok(WtH2Session {
            cmd_tx,
            uni_rx: Some(uni_rx),
            bi_rx: Some(bi_rx),
            session_state: session_state_tx,
            datagrams,
            _driver: AbortOnDrop(driver),
        })
    }
}

/// WebTransport の初期フロー制御値を含む `Limits` を構築する
///
/// draft-ietf-webtrans-http2-15 §4.3.1: 初期フロー制御値は SETTINGS で広告する。
/// §3.1: WebTransport を使うには `SETTINGS_ENABLE_CONNECT_PROTOCOL` と
/// `SETTINGS_WT_ENABLED` の両方が必要である。
/// 初期値を SETTINGS で広告するため、任意の `WebTransport-Init` ヘッダーは送らない
/// (§4.3.2 のヘッダーは SETTINGS と併用する場合に大きい方が採用される)。
fn build_limits(wt_config: &WtConfig) -> Result<Limits> {
    let setting = |value: u64| -> Result<u32> {
        u32::try_from(value).map_err(|_| {
            TransportError::Internal(format!(
                "WebTransport flow control limit {value} does not fit in a SETTINGS value (u32)"
            ))
        })
    };
    Limits::builder()
        .enable_connect_protocol(true)
        .wt_enabled(true)
        .webtransport(
            Some(setting(wt_config.initial_max_data)?),
            Some(setting(wt_config.initial_max_stream_data_uni)?),
            Some(setting(wt_config.initial_max_stream_data_bidi_local)?),
            Some(setting(wt_config.initial_max_streams_uni)?),
            Some(setting(wt_config.initial_max_streams_bidi)?),
            Some(setting(wt_config.initial_max_stream_data_bidi_remote)?),
        )
        .build()
        .map_err(|e| TransportError::Internal(format!("invalid HTTP/2 limits: {e}")))
}

/// WebTransport CONNECT のリクエストヘッダーを構築する
///
/// draft-ietf-webtrans-http2-15 §3.1 / §3.2: `:method=CONNECT`、`:protocol=webtransport`、
/// `:scheme=https`、`:authority`、`:path` を設定する。MOQT は
/// `WT-Available-Protocols` でプロトコル識別子を通知する
/// (draft-ietf-moq-transport-21 §6.2 (Session establishment))。
fn build_connect_headers(authority: &str, path: &str) -> Result<Vec<HeaderField>> {
    let internal = |e: shiguredo_http2::HeaderFieldError| {
        TransportError::Internal(format!("failed to build CONNECT headers: {e}"))
    };
    let mut headers = vec![
        HeaderField::from_static(b":method", b"CONNECT"),
        HeaderField::from_static(b":protocol", WEBTRANSPORT_PROTOCOL),
        HeaderField::from_static(b":scheme", b"https"),
        HeaderField::new(b":authority", authority).map_err(internal)?,
        HeaderField::new(b":path", path).map_err(internal)?,
    ];
    // RFC 8941 §4.1.6 の sf-string としてシリアライズした値を 1 要素の List として送る
    let available =
        shiguredo_http2::webtransport::serialize_wt_protocol(crate::MOQT_PROTOCOL.as_bytes())
            .map_err(|e| {
                TransportError::Internal(format!("failed to build WT-Available-Protocols: {e}"))
            })?;
    headers.push(HeaderField::new(b"wt-available-protocols", available).map_err(internal)?);
    Ok(headers)
}

/// peer の SETTINGS を受信し、WebTransport に対応していることを確認する
///
/// draft-ietf-webtrans-http2-15 §3.1: クライアントはサーバーが
/// `SETTINGS_ENABLE_CONNECT_PROTOCOL=1` と `SETTINGS_WT_ENABLED=1` を送るまで
/// WebTransport の CONNECT を送ってはならない。最初の SETTINGS で両方が無い場合は
/// この接続では WebTransport を使えないため、待ち続けずに失敗させる。
async fn wait_for_peer_settings(io: &mut H2Io<TlsStream>) -> Result<()> {
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_millis(PEER_SETTINGS_TIMEOUT_MS);
    loop {
        if io.remote_settings().enable_connect_protocol() && io.remote_settings().wt_enabled() {
            return Ok(());
        }
        tokio::select! {
            event = io.next_event() => {
                // 最初の SETTINGS で WebTransport の対応が宣言されなければ、
                // この接続で WebTransport は使えない (§3.1)。SETTINGS は
                // `next_event` の中で `remote_settings` へ反映済みである。
                // それ以外のイベントは接続開始前の処理なので読み飛ばす
                if matches!(event?, Event::SettingsReceived { ack: false })
                    && !(io.remote_settings().enable_connect_protocol()
                        && io.remote_settings().wt_enabled())
                {
                    return Err(TransportError::InvalidState(format!(
                        "peer does not support WebTransport over HTTP/2 (enable_connect_protocol={}, wt_enabled={})",
                        io.remote_settings().enable_connect_protocol(),
                        io.remote_settings().wt_enabled()
                    )));
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Err(TransportError::Internal(
                    "peer SETTINGS not received before WebTransport CONNECT".to_string(),
                ));
            }
        }
    }
}

/// CONNECT の 2xx 応答を待ち、`WT-Protocol` を検証する
///
/// draft-ietf-webtrans-http2-15 §3.2: 2xx でセッションが確立する。§3.3: クライアントが
/// `WT-Available-Protocols` を送った場合、サーバーが `WT-Protocol` を含めるなら
/// 候補の中から 1 つを選ばなければならない。MOQT はプロトコル識別子の交渉に使うため、
/// 2xx に `WT-Protocol` が無い場合も失敗させる (§3.3 の MUST)。
async fn wait_for_connect_response(
    io: &mut H2Io<TlsStream>,
    connect_stream: StreamId,
) -> Result<()> {
    loop {
        match io.next_event().await? {
            Event::HeadersReceived {
                stream_id,
                headers,
                end_stream,
                ..
            } if stream_id == connect_stream => {
                let status = headers
                    .iter()
                    .find(|h| h.name() == b":status")
                    .and_then(|h| std::str::from_utf8(h.value()).ok())
                    .and_then(|s| s.parse::<u16>().ok());
                let Some(status) = status else {
                    return Err(TransportError::ConnectFailed { status: None });
                };
                if !(200..300).contains(&status) {
                    return Err(TransportError::ConnectFailed {
                        status: Some(status),
                    });
                }
                if end_stream {
                    // 2xx + END_STREAM はセッション確立直後の終了であり、使えるセッションにならない
                    return Err(TransportError::InvalidState(
                        "WebTransport CONNECT response ended the stream".to_string(),
                    ));
                }
                verify_wt_protocol(&headers)?;
                return Ok(());
            }
            Event::StreamReset {
                stream_id,
                error_code,
                ..
            } if stream_id == connect_stream => {
                return Err(TransportError::Internal(format!(
                    "WebTransport CONNECT stream reset before the response: {error_code:?}"
                )));
            }
            Event::StreamClosed { stream_id } if stream_id == connect_stream => {
                return Err(TransportError::Internal(
                    "WebTransport CONNECT stream closed before the response".to_string(),
                ));
            }
            Event::ConnectionError { error_code, reason } => {
                return Err(TransportError::from(
                    shiguredo_http2::Error::connection_error(error_code, reason),
                ));
            }
            _ => {}
        }
    }
}

/// 応答の `WT-Protocol` が送った候補に一致するかを検証する
///
/// draft-ietf-webtrans-http2-15 §3.3: 2xx 応答の `WT-Protocol` は
/// `WT-Available-Protocols` に含めた値でなければならない。String 以外の型は無視され、
/// パラメータは読み飛ばされる (同節)。パラメータの読み飛ばしは
/// `WtAvailableProtocols::parse` の List パーサーに任せ、要素数を 1 に限定して
/// Item of String として扱う。
fn verify_wt_protocol(headers: &[HeaderField]) -> Result<()> {
    let negotiation_failed = || {
        TransportError::InvalidState(
            "WebTransport protocol negotiation failed: the response does not select moqt-21"
                .to_string(),
        )
    };
    let Some(value) = headers
        .iter()
        .find(|h| h.name() == b"wt-protocol")
        .map(HeaderField::value)
    else {
        return Err(negotiation_failed());
    };
    let parsed = shiguredo_http2::webtransport::WtAvailableProtocols::parse(value)
        .map_err(|_| negotiation_failed())?;
    if parsed.protocols.len() != 1 || parsed.protocols[0] != crate::MOQT_PROTOCOL {
        return Err(negotiation_failed());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// セッション
// ---------------------------------------------------------------------------

/// WebTransport over HTTP/2 セッション
///
/// driver タスクと mpsc / oneshot で通信する。driver は `WtH2Session` が drop されて
/// `cmd_tx` が閉じると終了する。
pub struct WtH2Session {
    cmd_tx: mpsc::UnboundedSender<Command>,
    /// 単方向受信ストリームの receiver。`take_uni_receiver` で取り出すと None になる
    uni_rx: Option<mpsc::UnboundedReceiver<WtH2RecvStream>>,
    /// 双方向受信ストリームの receiver。`take_bi_receiver` で取り出すと None になる
    bi_rx: Option<mpsc::UnboundedReceiver<(WtH2SendStream, WtH2RecvStream)>>,
    /// セッション状態 (§6.12 の終了 / §6.13 の drain / 接続エラー) を配る sender
    session_state: watch::Sender<WtSessionState>,
    /// 受信した datagram のバッファ
    datagrams: Arc<StdMutex<VecDeque<Vec<u8>>>>,
    /// driver タスクの JoinHandle (drop で abort する)。
    /// 読み出さないが、セッションより長生きしないための RAII ガードとして保持する
    _driver: AbortOnDrop,
}

impl WtH2Session {
    /// セッション状態の変更を観測する receiver を作る
    pub(crate) fn session_state_receiver(&self) -> watch::Receiver<WtSessionState> {
        self.session_state.subscribe()
    }

    /// 単方向送信ストリームを開く
    ///
    /// drain / 終了を観測している場合は開かずに `ConnectionClosed` を返す
    /// (`session_policy` の `reject_new_streams`。受信ループを待たせない)。
    pub async fn open_uni_stream(&mut self) -> Result<WtH2SendStream> {
        self.ensure_new_stream_allowed()?;
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::OpenUni { reply })
            .map_err(|_| TransportError::ConnectionClosed)?;
        rx.await.map_err(|_| TransportError::ConnectionClosed)?
    }

    /// 双方向ストリームを開く
    ///
    /// drain / 終了を観測している場合は開かずに `ConnectionClosed` を返す
    /// (`open_uni_stream` と同じ)。
    pub async fn open_bi_stream(&mut self) -> Result<(WtH2SendStream, WtH2RecvStream)> {
        self.ensure_new_stream_allowed()?;
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::OpenBi { reply })
            .map_err(|_| TransportError::ConnectionClosed)?;
        rx.await.map_err(|_| TransportError::ConnectionClosed)?
    }

    /// 新しいストリームを開ける状態かを確認する
    ///
    /// draft-ietf-webtrans-http2-15 §6.12 はセッション終了の検知後に新しいストリームを
    /// 開くことを禁じ、§6.13 の drain 後もセッションの利用は許す (MAY) が、本 example は
    /// drain を「できるだけ早く終了する」合図として扱い新規の作業を始めない。
    /// どちらの状態でも MOQT 層へは `ConnectionClosed` を返す。
    fn ensure_new_stream_allowed(&self) -> Result<()> {
        let state = *self.session_state.borrow();
        if session_policy(state).reject_new_streams {
            tracing::debug!(
                "Refusing to open a new WebTransport stream: session state is {state:?}"
            );
            return Err(TransportError::ConnectionClosed);
        }
        Ok(())
    }

    /// 単方向受信ストリームを受け付ける
    ///
    /// `take_uni_receiver` で receiver を取り出した後は `StreamClosed` を返す。
    pub async fn accept_uni_stream(&mut self) -> Result<WtH2RecvStream> {
        let rx = self.uni_rx.as_mut().ok_or(TransportError::StreamClosed)?;
        rx.recv().await.ok_or(TransportError::StreamClosed)
    }

    /// 単方向受信ストリームの receiver を取り出す
    ///
    /// 受信ループのように長く待つ呼び出しは receiver を取り出してロック外で `recv()` する。
    /// 取り出しは 1 回だけで、2 回目以降は None を返す。
    pub fn take_uni_receiver(&mut self) -> Option<mpsc::UnboundedReceiver<WtH2RecvStream>> {
        self.uni_rx.take()
    }

    /// 双方向受信ストリームの receiver を取り出す
    ///
    /// 詳細は `take_uni_receiver` を参照する。
    pub fn take_bi_receiver(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<(WtH2SendStream, WtH2RecvStream)>> {
        self.bi_rx.take()
    }

    /// セッションをクローズする (draft-ietf-webtrans-http2-15 §6.12)
    ///
    /// `WT_CLOSE_SESSION` を送り、直後に CONNECT ストリームを END_STREAM で half-close する
    /// (MUST)。`code` は 32 ビットの Application Error Code である
    /// (`moqt_close_code` で変換する)。
    pub async fn close(&mut self, code: u32, reason: &str) -> Result<()> {
        update_session_state(&self.session_state, WtSessionState::ClosedLocally);
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Close {
                error_code: code,
                reason: reason.to_string(),
                reply,
            })
            .map_err(|_| TransportError::ConnectionClosed)?;
        rx.await.map_err(|_| TransportError::ConnectionClosed)?
    }

    /// datagram を送信する (publisher 側で使用)
    pub async fn send_datagram(&self, payload: &[u8]) -> Result<()> {
        let state = *self.session_state.borrow();
        if session_policy(state).reject_datagrams {
            tracing::debug!("Refusing to send a WebTransport datagram: session state is {state:?}");
            return Err(TransportError::ConnectionClosed);
        }
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::SendDatagram {
                data: payload.to_vec(),
                reply,
            })
            .map_err(|_| TransportError::ConnectionClosed)?;
        rx.await.map_err(|_| TransportError::ConnectionClosed)?
    }

    /// バッファリングされた datagram を取り出す (subscriber 側で使用)
    ///
    /// セッション終了後に呼ぶと `ConnectionClosed` を返し、MOQT 層の受信ループを
    /// 待たせない。終了直前までに受信した datagram は捨てない。
    pub fn take_buffered_datagrams(&self) -> Result<Vec<Vec<u8>>> {
        let datagrams = self
            .datagrams
            .lock()
            .expect("datagram buffer mutex must not be poisoned")
            .drain(..)
            .collect::<Vec<_>>();
        let state = *self.session_state.borrow();
        if datagrams.is_empty() && session_policy(state).abort_streams {
            return Err(TransportError::ConnectionClosed);
        }
        Ok(datagrams)
    }
}

/// JoinHandle を drop で abort するラッパー
///
/// `WtH2Session` の drop で driver タスクを確実に止めるために使う。
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// ---------------------------------------------------------------------------
// ストリーム
// ---------------------------------------------------------------------------

/// WebTransport over HTTP/2 の送信ストリーム
///
/// `session_state` はセッション終了 / drain / 接続エラーの観測用である。
pub struct WtH2SendStream {
    stream_id: WtStreamId,
    cmd_tx: mpsc::UnboundedSender<Command>,
    session_state: watch::Receiver<WtSessionState>,
}

impl WtH2SendStream {
    /// データを送信する
    ///
    /// driver が `WT_STREAM` capsule へ変換して CONNECT ストリームに載せるまで待つ
    /// (WT の送信ウィンドウが枯渇している間は待つ)。セッション終了を観測した場合は
    /// `ConnectionClosed` を返す。
    pub async fn send(&mut self, data: &[u8]) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        let cmd_tx = self.cmd_tx.clone();
        let stream_id = self.stream_id;
        let send = async move {
            cmd_tx
                .send(Command::Send {
                    stream_id,
                    data: data.to_vec(),
                    fin: false,
                    reply: Some(reply),
                })
                .map_err(|_| TransportError::ConnectionClosed)?;
            rx.await.map_err(|_| TransportError::ConnectionClosed)?
        };
        tokio::select! {
            sent = send => sent,
            _ = wait_until_terminated(&mut self.session_state) => {
                Err(TransportError::ConnectionClosed)
            }
        }
    }

    /// ストリームの送信方向を終了する (FIN)
    ///
    /// FIN は capsule として送られるため、完了を待たずに戻る。
    pub fn finish(&mut self) -> Result<()> {
        self.cmd_tx
            .send(Command::Send {
                stream_id: self.stream_id,
                data: Vec::new(),
                fin: true,
                reply: None,
            })
            .map_err(|_| TransportError::ConnectionClosed)
    }

    /// ストリームの送信方向を reset する (`WT_RESET_STREAM` capsule)
    ///
    /// MOQT のエラーコードは WebTransport の Application Error Code としてそのまま送る
    /// (over HTTP/2 の capsule は 32 ビットのコードを運ぶ)。
    pub fn reset(&mut self, error_code: u64) -> Result<()> {
        self.cmd_tx
            .send(Command::Reset {
                stream_id: self.stream_id,
                error_code,
            })
            .map_err(|_| TransportError::ConnectionClosed)
    }

    /// ストリーム ID を返す
    pub fn stream_id(&self) -> u64 {
        self.stream_id
    }
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

/// WebTransport over HTTP/2 の受信ストリーム
pub struct WtH2RecvStream {
    stream_id: WtStreamId,
    cmd_tx: mpsc::UnboundedSender<Command>,
    data_rx: mpsc::UnboundedReceiver<StreamPacket>,
    session_state: watch::Receiver<WtSessionState>,
    /// ストリーム単位で消費したバイト数 (受信ウィンドウの拡張に使う)
    consumed: Arc<AtomicU64>,
    /// セッション単位で消費したバイト数 (受信ウィンドウの拡張に使う)
    session_consumed: Arc<AtomicU64>,
    /// 直近のデータに付いていた FIN をまだアプリへ返していない
    pending_fin: bool,
    /// FIN / reset を返し終えた終端 (以降は同じ値を返す)
    terminal: Option<RequestStreamEnd>,
}

impl WtH2RecvStream {
    /// データまたは終端を 1 つ受信する
    ///
    /// セッション終了を観測した場合は受信を待たずに `ConnectionClosed` を返す
    /// (受信ループを永久に待たせない)。
    pub async fn recv_chunk(&mut self) -> Result<RecvChunk> {
        loop {
            if self.pending_fin {
                self.pending_fin = false;
                self.terminal = Some(RequestStreamEnd::Fin);
                return Ok(RecvChunk::End(RequestStreamEnd::Fin));
            }
            if let Some(end) = self.terminal {
                return Ok(RecvChunk::End(end));
            }
            tokio::select! {
                packet = self.data_rx.recv() => match packet {
                    Some(StreamPacket::Data { data, fin }) => {
                        if fin {
                            self.pending_fin = true;
                        }
                        if !data.is_empty() {
                            let len = data.len() as u64;
                            self.consumed.fetch_add(len, Ordering::Relaxed);
                            self.session_consumed.fetch_add(len, Ordering::Relaxed);
                            return Ok(RecvChunk::Data(data));
                        }
                        // 空のデータ capsule (FIN なし) は次を待つ
                    }
                    Some(StreamPacket::Reset { error_code }) => {
                        // over HTTP/2 の WT_RESET_STREAM は Reliable Size を運ぶが、
                        // `WtEvent::StreamReset` には現れないため `None` にする
                        // over HTTP/2 の WT_RESET_STREAM は MOQT §12.5 のコードをそのまま運ぶため
                        // 常にコードがある (HTTP/3 のような code space の remap は不要)
                        let end = RequestStreamEnd::Reset {
                            error_code: Some(error_code),
                            reliable_size: None,
                        };
                        self.terminal = Some(end);
                        return Ok(RecvChunk::End(end));
                    }
                    // driver が終了した = セッション終了 (チャネルが閉じた)
                    None => {
                        return Err(TransportError::ConnectionClosed);
                    }
                },
                _ = wait_until_terminated(&mut self.session_state) => {
                    return Err(TransportError::ConnectionClosed);
                }
            }
        }
    }

    /// 受信方向へ `WT_STOP_SENDING` capsule を送る
    ///
    /// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection): 受信方向の
    /// cancel は STOP_SENDING で行う。
    pub fn stop_sending(&mut self, error_code: u64) -> Result<()> {
        self.cmd_tx
            .send(Command::StopSending {
                stream_id: self.stream_id,
                error_code,
            })
            .map_err(|_| TransportError::ConnectionClosed)
    }

    /// ストリーム ID を返す
    pub fn stream_id(&self) -> u64 {
        self.stream_id
    }
}

// ---------------------------------------------------------------------------
// ドライバ
// ---------------------------------------------------------------------------

/// driver へのコマンド
enum Command {
    /// 単方向送信ストリームを開く
    OpenUni {
        reply: oneshot::Sender<Result<WtH2SendStream>>,
    },
    /// 双方向ストリームを開く
    OpenBi {
        reply: oneshot::Sender<Result<(WtH2SendStream, WtH2RecvStream)>>,
    },
    /// ストリームへデータ (または FIN) を送る
    Send {
        stream_id: WtStreamId,
        data: Vec<u8>,
        fin: bool,
        reply: Option<oneshot::Sender<Result<()>>>,
    },
    /// ストリームの送信方向を reset する
    Reset {
        stream_id: WtStreamId,
        error_code: u64,
    },
    /// ストリームの受信方向へ STOP_SENDING を送る
    StopSending {
        stream_id: WtStreamId,
        error_code: u64,
    },
    /// datagram を送る
    SendDatagram {
        data: Vec<u8>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// セッションをクローズする
    Close {
        error_code: u32,
        reason: String,
        reply: oneshot::Sender<Result<()>>,
    },
}

/// ストリームのチャネルへ流すメッセージ
enum StreamPacket {
    /// データ (FIN 付きの場合がある)
    Data { data: Vec<u8>, fin: bool },
    /// peer からの reset
    Reset { error_code: u64 },
}

/// 送信待ちのデータ
struct PendingSend {
    /// 未送信のデータ
    data: Vec<u8>,
    /// このデータを送り終えたら FIN を送るか
    fin: bool,
    /// 送信完了 (capsule への変換完了) を知らせる reply
    reply: Option<oneshot::Sender<Result<()>>>,
}

/// ストリームごとの受信状態
struct StreamState {
    /// アプリへデータを渡すチャネル
    tx: mpsc::UnboundedSender<StreamPacket>,
    /// アプリが消費したバイト数 (受信ウィンドウの拡張に使う)
    consumed: Arc<AtomicU64>,
    /// 受信したバイト数
    received: u64,
}

/// WebTransport over HTTP/2 の driver
///
/// `Connection` と `WtSession` を所有し、H2 のイベントとアプリからのコマンドを
/// 単一のタスクで直列に処理する。
struct DriverState {
    io: H2Io<TlsStream>,
    connect_stream: StreamId,
    wt_session: WtSession,
    cmd_tx: mpsc::UnboundedSender<Command>,
    cmd_rx: mpsc::UnboundedReceiver<Command>,
    /// peer 起動の単方向受信ストリームを渡すチャネル
    uni_tx: mpsc::UnboundedSender<WtH2RecvStream>,
    /// peer 起動の双方向ストリームを渡すチャネル
    bi_tx: mpsc::UnboundedSender<(WtH2SendStream, WtH2RecvStream)>,
    datagrams: Arc<StdMutex<VecDeque<Vec<u8>>>>,
    receive_datagrams: bool,
    session_state: watch::Sender<WtSessionState>,
    streams: HashMap<WtStreamId, StreamState>,
    /// 送信待ちのデータ (ストリームごと、FIFO)
    pending_sends: HashMap<WtStreamId, VecDeque<PendingSend>>,
    /// `poll_output` から取り出してまだ HTTP/2 へ流していない WT 出力
    wt_out_pending: Vec<u8>,
    /// `WT_STOP_SENDING` を送ったストリーム (以降の受信データを破棄する)
    stop_sending_sent: HashSet<WtStreamId>,
    /// peer から `WT_STOP_SENDING` を受けたストリーム (以降の送信を拒否する)
    stop_sending_received: HashSet<WtStreamId>,
    /// セッション全体で受信したバイト数
    session_received: u64,
    /// アプリが消費したバイト数 (セッション全体)
    session_consumed: Arc<AtomicU64>,
}

impl DriverState {
    /// driver のメインループ
    ///
    /// 終了時は必ずセッション状態を終了方向へ進める。ストリームを待っているタスクは
    /// この状態の変化かチャネルの閉鎖 (`ConnectionClosed`) で終了を観測する。
    async fn run(&mut self) {
        loop {
            tokio::select! {
                biased;
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Some(cmd) => {
                            if !self.handle_command(cmd).await {
                                break;
                            }
                        }
                        // セッションが drop された
                        None => break,
                    }
                }
                event = self.io.next_event() => {
                    match event {
                        Ok(event) => {
                            match self.handle_event(event).await {
                                Ok(true) => {}
                                Ok(false) => break,
                                Err(e) => {
                                    tracing::warn!("WebTransport over HTTP/2 session failed: {e}");
                                    update_session_state(
                                        &self.session_state,
                                        WtSessionState::ConnectionFailed,
                                    );
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!("HTTP/2 connection ended: {e}");
                            update_session_state(
                                &self.session_state,
                                WtSessionState::ConnectionFailed,
                            );
                            break;
                        }
                    }
                }
            }
        }
    }

    /// コマンドを処理し、ループを継続するかどうかを返す
    async fn handle_command(&mut self, cmd: Command) -> bool {
        match cmd {
            Command::OpenUni { reply } => {
                let result = self
                    .wt_session
                    .open_uni_stream()
                    .map_err(TransportError::from)
                    .map(|stream_id| self.create_send_stream(stream_id));
                let _ = reply.send(result);
                true
            }
            Command::OpenBi { reply } => {
                let result = self
                    .wt_session
                    .open_bidi_stream()
                    .map_err(TransportError::from)
                    .map(|stream_id| {
                        let (send, recv) = self.create_stream(stream_id, true);
                        (send.expect("bidirectional stream has a send part"), recv)
                    });
                let _ = reply.send(result);
                true
            }
            Command::Send {
                stream_id,
                data,
                fin,
                reply,
            } => {
                // FIN / reset 済みのストリームへの送信は受け付けない
                if self.stop_sending_received.contains(&stream_id)
                    || self.wt_session.stream(stream_id).is_none()
                {
                    if let Some(reply) = reply {
                        let _ = reply.send(Err(TransportError::StreamClosed));
                    }
                    return true;
                }
                self.pending_sends
                    .entry(stream_id)
                    .or_default()
                    .push_back(PendingSend { data, fin, reply });
                self.flush_pending_sends();
                if !self.pump_and_flush().await {
                    return false;
                }
                true
            }
            Command::Reset {
                stream_id,
                error_code,
            } => {
                // 未送信のデータを破棄し、待っている reply を失敗させる
                self.fail_pending_sends(stream_id);
                match self.wt_session.reset_stream(stream_id, error_code) {
                    Ok(()) => {
                        if !self.pump_and_flush().await {
                            return false;
                        }
                    }
                    Err(e) => {
                        // 終端済みストリームへの reset など、アプリの後始末で起きる
                        tracing::debug!(
                            "Failed to send WT_RESET_STREAM for stream {stream_id}: {e}"
                        );
                    }
                }
                true
            }
            Command::StopSending {
                stream_id,
                error_code,
            } => {
                match self.wt_session.stop_sending(stream_id, error_code) {
                    Ok(()) => {
                        self.stop_sending_sent.insert(stream_id);
                        if !self.pump_and_flush().await {
                            return false;
                        }
                    }
                    Err(e) => {
                        tracing::debug!(
                            "Failed to send WT_STOP_SENDING for stream {stream_id}: {e}"
                        );
                    }
                }
                true
            }
            Command::SendDatagram { data, reply } => {
                let result = match self.wt_session.send_datagram(&data) {
                    Ok(()) => {
                        if self.pump_and_flush().await {
                            Ok(())
                        } else {
                            Err(TransportError::ConnectionClosed)
                        }
                    }
                    Err(e) => Err(TransportError::from(e)),
                };
                let failed = result.is_err();
                let _ = reply.send(result);
                if failed {
                    update_session_state(&self.session_state, WtSessionState::ConnectionFailed);
                }
                !failed
            }
            Command::Close {
                error_code,
                reason,
                reply,
            } => {
                let result = self.handle_close(error_code, &reason).await;
                let _ = reply.send(result);
                false
            }
        }
    }

    /// セッションをクローズする (draft-ietf-webtrans-http2-15 §6.12)
    ///
    /// `WT_CLOSE_SESSION` を送ったあと、CONNECT ストリームを END_STREAM で half-close する
    /// (MUST)。送信ウィンドウの枯渇で capsule が送り切れなかった場合は、呼び出し側が
    /// 「実際には送信されていない」ことを認識できるようエラーを返す。
    async fn handle_close(&mut self, error_code: u32, reason: &str) -> Result<()> {
        self.wt_session.close(error_code, reason)?;
        self.pump_wt_output()?;
        self.io.flush().await?;
        // RFC 9113 §6.9.1: 空 DATA + END_STREAM はフロー制御ウィンドウの空きが
        // 無くても送信できる
        self.io
            .conn
            .send_data(self.connect_stream, Vec::new(), true)?;
        self.io.flush().await?;
        if self.io.has_pending_send_data(self.connect_stream) {
            return Err(TransportError::ConnectionClosed);
        }
        Ok(())
    }

    /// H2 のイベントを処理し、ループを継続するかどうかを返す
    async fn handle_event(&mut self, event: Event) -> Result<bool> {
        match event {
            Event::DataReceived {
                stream_id,
                data,
                end_stream,
            } if stream_id == self.connect_stream => {
                let len = u32::try_from(data.len())
                    .expect("HTTP/2 DATA payload always fits in u32 per RFC 9113");
                if let Err(e) = self.wt_session.feed(&data) {
                    return Err(self.abort_session_with_wt_error(e).await);
                }
                if let Err(e) = self.wt_session.process() {
                    return Err(self.abort_session_with_wt_error(e).await);
                }
                while let Some(event) = self.wt_session.poll_event() {
                    if !self.dispatch_wt_event(event).await? {
                        return Ok(false);
                    }
                }

                // RFC 9113 §6.9.1: 受信した分の WINDOW_UPDATE を即座に返し、
                // 単一の CONNECT ストリーム上でピアが送り続けられるようにする
                if len > 0 {
                    self.io.conn.send_window_update(StreamId::Connection, len)?;
                    if !end_stream {
                        self.io.conn.send_window_update(self.connect_stream, len)?;
                    }
                }

                if !self.pump_and_flush().await {
                    return Ok(false);
                }

                if end_stream {
                    // CONNECT ストリームの終了はセッション終了である (§3.4)
                    tracing::info!("WebTransport session closed by peer: CONNECT stream ended");
                    update_session_state(&self.session_state, WtSessionState::ClosedByPeer);
                    return Ok(false);
                }
                Ok(true)
            }
            Event::StreamReset {
                stream_id,
                error_code,
                ..
            } if stream_id == self.connect_stream => {
                // draft-ietf-webtrans-http2-15 §3.4: CONNECT ストリームの reset は
                // アプリケーション層の通知を伴わないセッション終了である
                tracing::warn!(
                    "WebTransport session terminated by CONNECT stream reset: {error_code:?}"
                );
                update_session_state(&self.session_state, WtSessionState::ConnectionFailed);
                Ok(false)
            }
            Event::StreamClosed { stream_id } if stream_id == self.connect_stream => {
                tracing::info!("WebTransport session closed: CONNECT stream closed");
                update_session_state(&self.session_state, WtSessionState::ClosedByPeer);
                Ok(false)
            }
            Event::ConnectionError { error_code, reason } => {
                tracing::warn!("HTTP/2 connection error: {error_code:?}: {reason}");
                update_session_state(&self.session_state, WtSessionState::ConnectionFailed);
                Err(TransportError::from(
                    shiguredo_http2::Error::connection_error(error_code, reason),
                ))
            }
            Event::GoawayReceived { error_code, .. } => {
                // draft-ietf-webtrans-http2-15 §6.13: GOAWAY は新しいセッションを
                // 作れなくする。既存セッションは継続できるため drain と同じ扱いにする
                tracing::info!("Received GOAWAY: {error_code:?}");
                update_session_state(&self.session_state, WtSessionState::Draining);
                Ok(true)
            }
            Event::WindowUpdateReceived { .. } | Event::SettingsReceived { .. } => {
                // 送信ウィンドウが開いた可能性があるため、待っていた出力を流す
                if !self.pump_and_flush().await {
                    return Ok(false);
                }
                Ok(true)
            }
            Event::DataDiscarded {
                stream_id,
                connection_window_consumed,
            } if stream_id == self.connect_stream && connection_window_consumed > 0 => {
                // RFC 9113 §6.9.1: 破棄した DATA の分も接続ウィンドウを補充する
                let increment = u32::try_from(connection_window_consumed)
                    .expect("discarded DATA always fits in u32 per RFC 9113");
                self.io
                    .conn
                    .send_window_update(StreamId::Connection, increment)?;
                self.io.flush().await?;
                Ok(true)
            }
            other => {
                tracing::debug!("Ignoring HTTP/2 event: {other:?}");
                Ok(true)
            }
        }
    }

    /// WT イベントを処理し、ループを継続するかどうかを返す
    async fn dispatch_wt_event(&mut self, event: WtEvent) -> Result<bool> {
        match event {
            WtEvent::StreamOpened {
                stream_id,
                bidirectional,
            } => {
                let (send, recv) = self.create_stream(stream_id, bidirectional);
                if bidirectional {
                    let send = send.expect("bidirectional stream has a send part");
                    let _ = self.bi_tx.send((send, recv));
                } else {
                    let _ = self.uni_tx.send(recv);
                }
                Ok(true)
            }
            WtEvent::StreamData {
                stream_id,
                data,
                fin,
            } => {
                if self.stop_sending_sent.contains(&stream_id) {
                    // draft-ietf-moq-transport-21 §6.4.2.3: STOP_SENDING 後のデータは破棄する。
                    // ストリームウィンドウも拡張しない (draft-ietf-webtrans-http2-15 §6.6)
                    if fin {
                        self.streams.remove(&stream_id);
                        self.stop_sending_sent.remove(&stream_id);
                    }
                    return Ok(true);
                }
                let len = data.len() as u64;
                let Some(state) = self.streams.get_mut(&stream_id) else {
                    tracing::debug!(
                        "Received data for an unknown WebTransport stream: {stream_id}"
                    );
                    return Ok(true);
                };
                state.received = state.received.saturating_add(len);
                let _ = state.tx.send(StreamPacket::Data { data, fin });
                if fin {
                    self.streams.remove(&stream_id);
                }
                self.session_received = self.session_received.saturating_add(len);
                self.grow_windows(stream_id)?;
                Ok(true)
            }
            WtEvent::StreamReset {
                stream_id,
                error_code,
            } => {
                if let Some(state) = self.streams.remove(&stream_id) {
                    let _ = state.tx.send(StreamPacket::Reset { error_code });
                }
                self.stop_sending_sent.remove(&stream_id);
                Ok(true)
            }
            WtEvent::StopSending {
                stream_id,
                error_code,
            } => {
                // 送信側のストリームに対して peer が停止を要求した。未送信データを
                // 破棄し、以降の送信を拒否する
                tracing::debug!(
                    "Received WT_STOP_SENDING for stream {stream_id}: error_code={error_code:#x}"
                );
                self.stop_sending_received.insert(stream_id);
                self.fail_pending_sends(stream_id);
                Ok(true)
            }
            WtEvent::DatagramReceived { data } => {
                if self.receive_datagrams {
                    self.datagrams
                        .lock()
                        .expect("datagram buffer mutex must not be poisoned")
                        .push_back(data);
                }
                Ok(true)
            }
            WtEvent::SessionDraining => {
                tracing::info!("WebTransport session draining");
                update_session_state(&self.session_state, WtSessionState::Draining);
                Ok(true)
            }
            WtEvent::SessionClosed { error_code, reason } => {
                // draft-ietf-webtrans-http2-15 §6.12: WT_CLOSE_SESSION を受信したら
                // CONNECT ストリームを END_STREAM で閉じる (MUST)
                tracing::info!(
                    "WebTransport session closed by peer: error_code={error_code:#x}, reason={reason:?}"
                );
                update_session_state(&self.session_state, WtSessionState::ClosedByPeer);
                let _ = self
                    .io
                    .conn
                    .send_data(self.connect_stream, Vec::new(), true);
                let _ = self.io.flush().await;
                Ok(false)
            }
        }
    }

    /// ストリームの受信チャネルとハンドルを作る
    ///
    /// `bidirectional` が false の場合は受信専用の uni ストリームであり、
    /// `send` は `None` になる。受信ハンドルは常に作る (ローカルから開く送信専用の
    /// uni ストリームではこのメソッドを呼ばない)。
    fn create_stream(
        &mut self,
        stream_id: WtStreamId,
        bidirectional: bool,
    ) -> (Option<WtH2SendStream>, WtH2RecvStream) {
        let (tx, rx) = mpsc::unbounded_channel();
        let consumed = Arc::new(AtomicU64::new(0));
        self.streams.insert(
            stream_id,
            StreamState {
                tx,
                consumed: Arc::clone(&consumed),
                received: 0,
            },
        );
        let recv = WtH2RecvStream {
            stream_id,
            cmd_tx: self.cmd_tx.clone(),
            data_rx: rx,
            session_state: self.session_state.subscribe(),
            consumed,
            session_consumed: Arc::clone(&self.session_consumed),
            pending_fin: false,
            terminal: None,
        };
        let send = if bidirectional {
            Some(self.create_send_stream(stream_id))
        } else {
            None
        };
        (send, recv)
    }

    /// 送信ストリームのハンドルを作る
    fn create_send_stream(&self, stream_id: WtStreamId) -> WtH2SendStream {
        WtH2SendStream {
            stream_id,
            cmd_tx: self.cmd_tx.clone(),
            session_state: self.session_state.subscribe(),
        }
    }

    /// 受信ウィンドウを消費の進み具合に応じて拡張する
    ///
    /// draft-ietf-webtrans-http2-15 §6.5 / §6.6: アプリが読んだ分だけ
    /// `WT_MAX_DATA` / `WT_MAX_STREAM_DATA` を送る。読んでいない分を超えて
    /// ウィンドウを広げないため、アプリの停滞がメモリの増加にならない。
    fn grow_windows(&mut self, stream_id: WtStreamId) -> Result<()> {
        // セッションウィンドウ
        let initial = self.wt_session.config().initial_max_data;
        if initial > 0 {
            let consumed = self.session_consumed.load(Ordering::Relaxed);
            let outstanding = self.session_received.saturating_sub(consumed);
            if outstanding < initial.div_ceil(2) {
                self.wt_session.grow_recv_window(initial)?;
            }
        }
        // ストリームウィンドウ
        let Some(state) = self.streams.get(&stream_id) else {
            return Ok(());
        };
        let Some(stream) = self.wt_session.stream(stream_id) else {
            return Ok(());
        };
        if !stream.can_recv() {
            return Ok(());
        }
        let initial = if stream.is_bidirectional() {
            if wt_stream_id::is_client_initiated(stream_id) {
                self.wt_session.config().initial_max_stream_data_bidi_local
            } else {
                self.wt_session.config().initial_max_stream_data_bidi_remote
            }
        } else {
            self.wt_session.config().initial_max_stream_data_uni
        };
        if initial == 0 {
            return Ok(());
        }
        let consumed = state.consumed.load(Ordering::Relaxed);
        let outstanding = state.received.saturating_sub(consumed);
        if outstanding < initial.div_ceil(2) {
            self.wt_session
                .grow_stream_recv_window(stream_id, initial)?;
        }
        Ok(())
    }

    /// 送信待ちのデータを送信ウィンドウの空きに応じて送る
    fn flush_pending_sends(&mut self) {
        let stream_ids: Vec<WtStreamId> = self.pending_sends.keys().copied().collect();
        for stream_id in stream_ids {
            if self.wt_session.stream(stream_id).is_none() {
                // FIN / reset 済みのストリームに残ったデータは送れない
                self.fail_pending_sends(stream_id);
                continue;
            }
            while let Some(front) = self
                .pending_sends
                .get(&stream_id)
                .and_then(|queue| queue.front())
            {
                let session_available = self.wt_session.flow_control().send_available();
                let stream_available = self
                    .wt_session
                    .stream(stream_id)
                    .map_or(0, |stream| stream.send_available());
                let available = session_available.min(stream_available);
                let empty_fin = front.data.is_empty() && front.fin;
                if available == 0 && !empty_fin {
                    break;
                }
                let take = (available as usize).min(front.data.len());
                let (chunk, is_last, send_fin) = {
                    let front = self
                        .pending_sends
                        .get_mut(&stream_id)
                        .and_then(|queue| queue.front_mut())
                        .expect("front was checked above");
                    let chunk: Vec<u8> = front.data.drain(..take).collect();
                    let is_last = front.data.is_empty();
                    (chunk, is_last, is_last && front.fin)
                };
                match self
                    .wt_session
                    .send_stream_data(stream_id, &chunk, send_fin)
                {
                    Ok(()) => {
                        if is_last {
                            let pending = self
                                .pending_sends
                                .get_mut(&stream_id)
                                .and_then(|queue| queue.pop_front())
                                .expect("front was checked above");
                            if let Some(reply) = pending.reply {
                                let _ = reply.send(Ok(()));
                            }
                        }
                    }
                    Err(e) => {
                        let pending = self
                            .pending_sends
                            .get_mut(&stream_id)
                            .and_then(|queue| queue.pop_front())
                            .expect("front was checked above");
                        if let Some(reply) = pending.reply {
                            let _ = reply.send(Err(TransportError::from(e)));
                        }
                    }
                }
                if self
                    .pending_sends
                    .get(&stream_id)
                    .is_some_and(VecDeque::is_empty)
                {
                    self.pending_sends.remove(&stream_id);
                    break;
                }
            }
        }
    }

    /// `pending_sends` の未送信データを失敗させる
    fn fail_pending_sends(&mut self, stream_id: WtStreamId) {
        if let Some(queue) = self.pending_sends.remove(&stream_id) {
            for pending in queue {
                if let Some(reply) = pending.reply {
                    let _ = reply.send(Err(TransportError::StreamClosed));
                }
            }
        }
    }

    /// WT の出力を HTTP/2 の DATA として送信可能な分だけ流す
    fn pump_wt_output(&mut self) -> Result<()> {
        while let Some(out) = self.wt_session.poll_output() {
            self.wt_out_pending.extend(out);
        }
        if self.wt_out_pending.is_empty() {
            return Ok(());
        }
        // 送信バッファに滞留データがある間は追加しない (固定容量 65535 の超過を避ける)
        if self.io.has_pending_send_data(self.connect_stream) {
            return Ok(());
        }
        let mut sent = 0;
        while sent < self.wt_out_pending.len() {
            let end = (sent + WT_SEND_CHUNK_SIZE).min(self.wt_out_pending.len());
            let chunk = self.wt_out_pending[sent..end].to_vec();
            self.io.conn.send_data(self.connect_stream, chunk, false)?;
            sent = end;
            if self.io.has_pending_send_data(self.connect_stream) {
                break;
            }
        }
        self.wt_out_pending.drain(..sent);
        Ok(())
    }

    /// WT の出力を流して TCP まで書き出す
    ///
    /// 失敗した場合はセッションを終了として扱い、false を返す。
    async fn pump_and_flush(&mut self) -> bool {
        if let Err(e) = self.pump_wt_output() {
            tracing::warn!("Failed to serialize WebTransport output: {e}");
            update_session_state(&self.session_state, WtSessionState::ConnectionFailed);
            return false;
        }
        if let Err(e) = self.io.flush().await {
            tracing::debug!("Failed to write HTTP/2 output: {e}");
            update_session_state(&self.session_state, WtSessionState::ConnectionFailed);
            return false;
        }
        true
    }

    /// WT のセッションエラーでセッションを中断する
    ///
    /// draft-ietf-webtrans-http2-15 §3.4 / §11.3: セッションエラーは CONNECT ストリームの
    /// RST_STREAM で伝える。エラーコードが特定できる場合は RST_STREAM を送ってから返す。
    async fn abort_session_with_wt_error(&mut self, error: WtError) -> TransportError {
        if let Some(code) = wt_http2_error_code(error.kind())
            && self.io.conn.reset_stream(self.connect_stream, code).is_ok()
        {
            let _ = self.io.flush().await;
        }
        update_session_state(&self.session_state, WtSessionState::ConnectionFailed);
        TransportError::from(error)
    }
}

/// セッション終了系の `WtErrorKind` を HTTP/2 のエラーコードへ対応付ける
///
/// draft-ietf-webtrans-http2-15 §3.4 / §11.3: セッションエラーは CONNECT ストリームの
/// RST_STREAM で伝える。対応するコードが無い場合は `None` を返し、RST_STREAM を送らない。
fn wt_http2_error_code(kind: WtErrorKind) -> Option<ErrorCode> {
    match kind {
        WtErrorKind::StreamStateError => Some(ErrorCode::WtStreamStateError),
        WtErrorKind::FlowControlError => Some(ErrorCode::WtFlowControlError),
        WtErrorKind::SessionStateError => Some(ErrorCode::WtError),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extended CONNECT の必須フィールドが設定される
    #[test]
    fn build_connect_headers_sets_extended_connect_fields() {
        let headers = build_connect_headers("relay.example.com:4443", "/path?x=1")
            .expect("構築に成功すること");
        let value = |name: &[u8]| {
            headers
                .iter()
                .find(|h| h.name() == name)
                .map(HeaderField::value)
        };
        assert_eq!(value(b":method"), Some(b"CONNECT".as_slice()));
        assert_eq!(value(b":protocol"), Some(WEBTRANSPORT_PROTOCOL));
        assert_eq!(value(b":scheme"), Some(b"https".as_slice()));
        assert_eq!(
            value(b":authority"),
            Some(b"relay.example.com:4443".as_slice())
        );
        assert_eq!(value(b":path"), Some(b"/path?x=1".as_slice()));
    }

    /// MOQT のプロトコル識別子を WT-Available-Protocols で通知する
    #[test]
    fn build_connect_headers_offers_moqt_protocol() {
        let headers = build_connect_headers("example.com", "/").expect("構築に成功すること");
        let available = headers
            .iter()
            .find(|h| h.name() == b"wt-available-protocols")
            .map(HeaderField::value)
            .expect("WT-Available-Protocols が設定されること");
        // RFC 8941 の sf-string を 1 要素の List として送る
        assert_eq!(available, br#""moqt-21""#);
    }

    /// 2xx 応答の WT-Protocol が moqt-21 なら受理する
    #[test]
    fn verify_wt_protocol_accepts_moqt_21() {
        let headers = vec![HeaderField::from_static(b"wt-protocol", b"\"moqt-21\"")];
        verify_wt_protocol(&headers).expect("moqt-21 は受理されること");
    }

    /// WT-Protocol のパラメータは無視する (RFC 8941 / draft §3.3)
    #[test]
    fn verify_wt_protocol_ignores_parameters() {
        let headers = vec![HeaderField::from_static(b"wt-protocol", b"\"moqt-21\";x=1")];
        verify_wt_protocol(&headers).expect("パラメータ付きでも受理されること");
    }

    /// WT-Protocol が無い / 別の値 / List / 非 String は交渉失敗にする
    #[test]
    fn verify_wt_protocol_rejects_invalid_selection() {
        for value in [
            // 別のプロトコル
            &b"\"moqt-20\""[..],
            // List (Item of String ではない)
            &b"\"moqt-21\", \"moqt-20\""[..],
            // 非 String
            &b"token"[..],
        ] {
            let headers =
                vec![HeaderField::new(b"wt-protocol", value).expect("構築に成功すること")];
            let error = verify_wt_protocol(&headers).expect_err("拒否されること");
            assert!(
                matches!(error, TransportError::InvalidState(_)),
                "{error:?}"
            );
        }
        let error = verify_wt_protocol(&[]).expect_err("WT-Protocol 無しは拒否されること");
        assert!(
            matches!(error, TransportError::InvalidState(_)),
            "{error:?}"
        );
    }

    /// SETTINGS に WebTransport の初期フロー制御値を設定する
    #[test]
    fn build_limits_advertises_webtransport_settings() {
        let limits = build_limits(&WtConfig::default()).expect("構築に成功すること");
        assert!(limits.enable_connect_protocol());
        assert!(limits.wt_enabled());
        let config = WtConfig::default();
        assert_eq!(
            limits.wt_initial_max_data(),
            Some(config.initial_max_data as u32)
        );
        assert_eq!(
            limits.wt_initial_max_streams_uni(),
            Some(config.initial_max_streams_uni as u32)
        );
    }

    /// ClientConfig のビルダーが値を設定する
    #[test]
    fn client_config_builders_set_fields() {
        let addr: SocketAddr = "127.0.0.1:4443".parse().expect("解釈に成功すること");
        let config = ClientConfig::new(addr, "relay.example.com")
            .authority("relay.example.com:4443")
            .ca_cert("PEM")
            .insecure()
            .receive_datagrams();
        assert_eq!(config.remote_addr, addr);
        assert_eq!(config.server_name, "relay.example.com");
        assert_eq!(config.authority, "relay.example.com:4443");
        assert_eq!(config.ca_cert_pem.as_deref(), Some("PEM"));
        assert!(config.disable_cert_validation);
        assert!(config.receive_datagrams);
        // Debug は PEM の中身を出さない
        let debug = format!("{config:?}");
        assert!(debug.contains("<PEM>"), "{debug}");
        assert!(!debug.contains("PEM,"), "{debug}");
    }

    /// 既定では authority を server_name と同じ値にする
    #[test]
    fn client_config_defaults_authority_to_server_name() {
        let addr: SocketAddr = "127.0.0.1:4443".parse().expect("解釈に成功すること");
        let config = ClientConfig::new(addr, "relay.example.com");
        assert_eq!(config.authority, "relay.example.com");
        assert!(!config.disable_cert_validation);
        assert!(!config.receive_datagrams);
    }

    /// セッションエラーの種類が HTTP/2 のエラーコードへ対応付く
    #[test]
    fn wt_http2_error_code_maps_session_errors() {
        assert_eq!(
            wt_http2_error_code(WtErrorKind::StreamStateError),
            Some(ErrorCode::WtStreamStateError)
        );
        assert_eq!(
            wt_http2_error_code(WtErrorKind::FlowControlError),
            Some(ErrorCode::WtFlowControlError)
        );
        assert_eq!(
            wt_http2_error_code(WtErrorKind::SessionStateError),
            Some(ErrorCode::WtError)
        );
    }

    /// RecvChunk の Debug は payload を出さない
    #[test]
    fn recv_chunk_debug_shows_length_only() {
        let chunk = RecvChunk::Data(vec![0x01, 0x02, 0x03]);
        assert_eq!(format!("{chunk:?}"), "Data(3 bytes)");
        let chunk = RecvChunk::End(RequestStreamEnd::Fin);
        assert_eq!(format!("{chunk:?}"), "End(Fin)");
    }

    // -----------------------------------------------------------------------
    // 実サーバーとの結合テスト
    // -----------------------------------------------------------------------

    /// テストサーバーの TLS 設定 (localhost の自己署名証明書)
    fn test_server_tls() -> tokio_rustls::TlsAcceptor {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("自己署名証明書の生成に成功すること");
        let cert = certified.cert.der().clone();
        let key = rustls::pki_types::PrivateKeyDer::try_from(certified.signing_key.serialize_der())
            .expect("秘密鍵の変換に成功すること");
        let mut config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .expect("TLS サーバー設定の構築に成功すること");
        config.alpn_protocols = vec![H2_ALPN.to_vec()];
        tokio_rustls::TlsAcceptor::from(Arc::new(config))
    }

    /// サーバー側の WT 出力を HTTP/2 の DATA として送る
    async fn pump_server_output<S>(
        io: &mut H2Io<S>,
        wt: &mut WtSession,
        connect_stream: StreamId,
    ) -> std::result::Result<(), String>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        while let Some(out) = wt.poll_output() {
            for chunk in out.chunks(WT_SEND_CHUNK_SIZE) {
                io.conn
                    .send_data(connect_stream, chunk.to_vec(), false)
                    .map_err(|e| format!("failed to send server DATA: {e}"))?;
            }
        }
        io.flush()
            .await
            .map_err(|e| format!("failed to flush server output: {e}"))
    }

    /// テスト用の WebTransport over HTTP/2 サーバー
    ///
    /// CONNECT に 200 + WT-Protocol で応答し、client の uni stream の "hello" を受信したら
    /// 自分の uni stream で "world"、双方向ストリームで "bidi-from-server"、datagram を送る。
    /// client の双方向ストリームの "req" には "res" を返す。最後に WT_CLOSE_SESSION を待つ。
    async fn run_test_server(
        listener: tokio::net::TcpListener,
        tls: tokio_rustls::TlsAcceptor,
    ) -> std::result::Result<(), String> {
        let (tcp, _) = listener
            .accept()
            .await
            .map_err(|e| format!("accept failed: {e}"))?;
        let stream = tls
            .accept(tcp)
            .await
            .map_err(|e| format!("TLS accept failed: {e}"))?;

        let wt_config = WtConfig::default();
        let limits = build_limits(&wt_config).map_err(|e| format!("limits failed: {e}"))?;
        let mut io = H2Io::server(stream, limits);
        io.send_preface()
            .await
            .map_err(|e| format!("failed to send server SETTINGS: {e}"))?;

        let mut wt: Option<WtSession> = None;
        let mut connect_stream: Option<StreamId> = None;
        let mut received = Vec::new();
        let mut sent_world = false;
        let mut saw_close = false;
        // client が開始した双方向ストリームの ID (受信した "req" に "res" を返す)
        let mut client_bidi: Option<WtStreamId> = None;
        let mut saw_stream_reset = false;
        let mut saw_stop_sending = false;

        loop {
            let event = io
                .next_event()
                .await
                .map_err(|e| format!("server next_event failed: {e}"))?;
            match event {
                Event::HeadersReceived {
                    stream_id, headers, ..
                } if connect_stream.is_none() => {
                    let protocol = headers
                        .iter()
                        .find(|h| h.name() == b":protocol")
                        .map(HeaderField::value);
                    if protocol != Some(WEBTRANSPORT_PROTOCOL) {
                        return Err(format!("unexpected :protocol: {protocol:?}"));
                    }
                    let wt_protocol = shiguredo_http2::webtransport::serialize_wt_protocol(
                        crate::MOQT_PROTOCOL.as_bytes(),
                    )
                    .map_err(|e| format!("failed to serialize wt-protocol: {e}"))?;
                    let response = vec![
                        HeaderField::from_static(b":status", b"200"),
                        HeaderField::new(b"wt-protocol", wt_protocol)
                            .map_err(|e| format!("failed to build wt-protocol: {e}"))?,
                    ];
                    io.conn
                        .send_response(stream_id, response, false)
                        .map_err(|e| format!("failed to send 200 response: {e}"))?;
                    io.flush()
                        .await
                        .map_err(|e| format!("failed to flush the response: {e}"))?;

                    let mut local = WtConfig::default();
                    local.overlay_settings(io.local_settings());
                    let mut peer = WtConfig::peer_default();
                    peer.overlay_settings(io.remote_settings());
                    let mut session = WtSession::server(local, peer);
                    session
                        .initiate()
                        .map_err(|e| format!("WtSession::initiate failed: {e}"))?;
                    wt = Some(session);
                    connect_stream = Some(stream_id);
                }
                Event::DataReceived {
                    stream_id,
                    data,
                    end_stream,
                } if Some(stream_id) == connect_stream => {
                    let session = wt.as_mut().ok_or("session is not established")?;
                    session
                        .feed(&data)
                        .map_err(|e| format!("wt feed failed: {e}"))?;
                    session
                        .process()
                        .map_err(|e| format!("wt process failed: {e}"))?;
                    while let Some(event) = session.poll_event() {
                        match event {
                            WtEvent::StreamOpened {
                                stream_id,
                                bidirectional: true,
                            } => {
                                client_bidi = Some(stream_id);
                            }
                            WtEvent::StreamData {
                                stream_id,
                                data,
                                fin: _,
                            } => {
                                received.extend_from_slice(&data);
                                // FIN はデータとは別の capsule で届くため、データだけで判定する
                                if Some(stream_id) == client_bidi && data == b"req" {
                                    session
                                        .send_stream_data(stream_id, b"res", true)
                                        .map_err(|e| format!("echo failed: {e}"))?;
                                }
                            }
                            WtEvent::StreamReset { .. } => saw_stream_reset = true,
                            WtEvent::StopSending { .. } => saw_stop_sending = true,
                            WtEvent::SessionClosed { .. } => saw_close = true,
                            _ => {}
                        }
                    }
                    if !sent_world {
                        let uni = session
                            .open_uni_stream()
                            .map_err(|e| format!("open_uni_stream failed: {e}"))?;
                        session
                            .send_stream_data(uni, b"world", true)
                            .map_err(|e| format!("send_stream_data failed: {e}"))?;
                        // STOP_SENDING を受けるまで開いたままにする uni stream
                        let extra = session
                            .open_uni_stream()
                            .map_err(|e| format!("open_uni_stream failed: {e}"))?;
                        session
                            .send_stream_data(extra, b"extra", false)
                            .map_err(|e| format!("send_stream_data failed: {e}"))?;
                        let bidi = session
                            .open_bidi_stream()
                            .map_err(|e| format!("open_bidi_stream failed: {e}"))?;
                        session
                            .send_stream_data(bidi, b"bidi-from-server", true)
                            .map_err(|e| format!("send_stream_data failed: {e}"))?;
                        session
                            .send_datagram(b"datagram")
                            .map_err(|e| format!("send_datagram failed: {e}"))?;
                        sent_world = true;
                    }
                    pump_server_output(&mut io, session, stream_id).await?;
                    if end_stream {
                        break;
                    }
                }
                Event::StreamReset { stream_id, .. } | Event::StreamClosed { stream_id }
                    if Some(stream_id) == connect_stream =>
                {
                    saw_close = true;
                    break;
                }
                Event::ConnectionError { .. } => break,
                _ => {}
            }
        }

        assert_eq!(
            received, b"helloreqreset-me",
            "client のデータを受信すること"
        );
        assert!(saw_close, "WT_CLOSE_SESSION を受信すること");
        assert!(saw_stream_reset, "WT_RESET_STREAM を受信すること");
        assert!(saw_stop_sending, "WT_STOP_SENDING を受信すること");
        Ok(())
    }

    /// TCP+TLS+HTTP/2 の実サーバーと WebTransport セッションを確立し、
    /// ストリームと datagram を往復してクローズする
    #[tokio::test(flavor = "multi_thread")]
    async fn wt_h2_session_roundtrip_over_real_tls() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind に成功すること");
        let addr = listener
            .local_addr()
            .expect("local_addr の取得に成功すること");
        let server = tokio::spawn(run_test_server(listener, test_server_tls()));

        let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let config = ClientConfig::new(addr, "localhost")
                .authority(format!("localhost:{}", addr.port()))
                .insecure()
                .receive_datagrams();
            let mut session = WtH2Client::connect(config, "/test")
                .await
                .expect("WebTransport セッションの確立に成功すること");

            // client → server (uni stream)
            let mut send = session
                .open_uni_stream()
                .await
                .expect("open_uni_stream に成功すること");
            send.send(b"hello").await.expect("send に成功すること");
            send.finish().expect("finish に成功すること");

            // client → server (双方向ストリーム) と server からの応答
            let (mut req_send, mut req_recv) = session
                .open_bi_stream()
                .await
                .expect("open_bi_stream に成功すること");
            req_send.send(b"req").await.expect("send に成功すること");
            req_send.finish().expect("finish に成功すること");
            let mut response = Vec::new();
            loop {
                match req_recv
                    .recv_chunk()
                    .await
                    .expect("recv_chunk に成功すること")
                {
                    RecvChunk::Data(data) => response.extend_from_slice(&data),
                    RecvChunk::End(RequestStreamEnd::Fin) => break,
                    RecvChunk::End(end) => panic!("unexpected end: {end:?}"),
                }
            }
            assert_eq!(response, b"res", "双方向ストリームの応答を受信すること");

            // server → client (uni stream)
            let mut recv = session
                .accept_uni_stream()
                .await
                .expect("accept_uni_stream に成功すること");
            let mut received = Vec::new();
            loop {
                match recv.recv_chunk().await.expect("recv_chunk に成功すること") {
                    RecvChunk::Data(data) => received.extend_from_slice(&data),
                    RecvChunk::End(RequestStreamEnd::Fin) => break,
                    RecvChunk::End(end) => panic!("unexpected end: {end:?}"),
                }
            }
            assert_eq!(received, b"world", "server のデータを受信すること");

            // server → client (双方向ストリーム)
            let mut bi_rx = session
                .take_bi_receiver()
                .expect("bi receiver を取り出せること");
            let (_bi_send, mut bi_recv) = bi_rx
                .recv()
                .await
                .expect("server 起動の双方向ストリームを受信できること");
            let mut received = Vec::new();
            loop {
                match bi_recv
                    .recv_chunk()
                    .await
                    .expect("recv_chunk に成功すること")
                {
                    RecvChunk::Data(data) => received.extend_from_slice(&data),
                    RecvChunk::End(RequestStreamEnd::Fin) => break,
                    RecvChunk::End(end) => panic!("unexpected end: {end:?}"),
                }
            }
            assert_eq!(
                received, b"bidi-from-server",
                "server 起動の双方向ストリームのデータを受信すること"
            );

            // client → server (stream reset)
            let mut reset_send = session
                .open_uni_stream()
                .await
                .expect("open_uni_stream に成功すること");
            reset_send
                .send(b"reset-me")
                .await
                .expect("send に成功すること");
            reset_send.reset(0x1).expect("reset に成功すること");

            // server → client (uni stream) への STOP_SENDING
            let mut extra = session
                .accept_uni_stream()
                .await
                .expect("accept_uni_stream に成功すること");
            match extra.recv_chunk().await.expect("recv_chunk に成功すること") {
                RecvChunk::Data(data) => assert_eq!(data, b"extra", "extra のデータを受信すること"),
                RecvChunk::End(end) => panic!("unexpected end: {end:?}"),
            }
            extra
                .stop_sending(0x2)
                .expect("stop_sending に成功すること");

            // server → client (datagram)
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let datagrams = session
                    .take_buffered_datagrams()
                    .expect("take_buffered_datagrams に成功すること");
                if !datagrams.is_empty() {
                    assert_eq!(
                        datagrams[0], b"datagram",
                        "server の datagram を受信すること"
                    );
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "datagram が届くこと"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }

            session
                .close(0, "done")
                .await
                .expect("close に成功すること");
        })
        .await;
        assert!(result.is_ok(), "テストがタイムアウトしないこと: {result:?}");

        server
            .await
            .expect("サーバータスクの join に成功すること")
            .expect("サーバーがエラーなく終了すること");
    }

    /// CONNECT への応答だけを行うテストサーバー
    ///
    /// `enable_webtransport` が false のときは SETTINGS で WebTransport の対応を宣言しない。
    /// `status` が 200 以外のときはその status を END_STREAM 付きで返す。
    /// `include_wt_protocol` が false のときは 200 でも WT-Protocol を付けない。
    async fn run_connect_only_server(
        listener: tokio::net::TcpListener,
        tls: tokio_rustls::TlsAcceptor,
        enable_webtransport: bool,
        status: u16,
        include_wt_protocol: bool,
    ) -> std::result::Result<(), String> {
        let (tcp, _) = listener
            .accept()
            .await
            .map_err(|e| format!("accept failed: {e}"))?;
        let stream = tls
            .accept(tcp)
            .await
            .map_err(|e| format!("TLS accept failed: {e}"))?;
        let limits = if enable_webtransport {
            build_limits(&WtConfig::default()).map_err(|e| format!("limits failed: {e}"))?
        } else {
            Limits::builder()
                .enable_connect_protocol(true)
                .build()
                .map_err(|e| format!("limits failed: {e}"))?
        };
        let mut io = H2Io::server(stream, limits);
        io.send_preface()
            .await
            .map_err(|e| format!("failed to send server SETTINGS: {e}"))?;

        loop {
            match io.next_event().await {
                Ok(Event::HeadersReceived { stream_id, .. }) => {
                    let mut response = vec![
                        HeaderField::new(b":status", status.to_string())
                            .map_err(|e| format!("failed to build :status: {e}"))?,
                    ];
                    if status == 200 && include_wt_protocol {
                        let wt_protocol = shiguredo_http2::webtransport::serialize_wt_protocol(
                            crate::MOQT_PROTOCOL.as_bytes(),
                        )
                        .map_err(|e| format!("failed to serialize wt-protocol: {e}"))?;
                        response.push(
                            HeaderField::new(b"wt-protocol", wt_protocol)
                                .map_err(|e| format!("failed to build wt-protocol: {e}"))?,
                        );
                    }
                    io.conn
                        .send_response(stream_id, response, status != 200)
                        .map_err(|e| format!("failed to send the response: {e}"))?;
                    io.flush()
                        .await
                        .map_err(|e| format!("failed to flush the response: {e}"))?;
                    return Ok(());
                }
                Ok(_) => {}
                // client がエラーで切断した場合は正常終了として扱う
                Err(_) => return Ok(()),
            }
        }
    }

    /// テストサーバーを起動して待ち受けアドレスを返す
    async fn spawn_connect_only_server(
        enable_webtransport: bool,
        status: u16,
        include_wt_protocol: bool,
    ) -> (
        SocketAddr,
        tokio::task::JoinHandle<std::result::Result<(), String>>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind に成功すること");
        let addr = listener
            .local_addr()
            .expect("local_addr の取得に成功すること");
        let server = tokio::spawn(run_connect_only_server(
            listener,
            test_server_tls(),
            enable_webtransport,
            status,
            include_wt_protocol,
        ));
        (addr, server)
    }

    /// connect のエラーを取り出す (WtH2Session は Debug ではないため expect_err を使わない)
    fn expect_connect_error(
        result: std::result::Result<WtH2Session, TransportError>,
    ) -> TransportError {
        match result {
            Ok(_) => panic!("接続が拒否されること"),
            Err(e) => e,
        }
    }

    /// 2xx 以外の応答は ConnectFailed に status を保持して返す
    #[tokio::test(flavor = "multi_thread")]
    async fn wt_h2_connect_reports_rejection_status() {
        let (addr, server) = spawn_connect_only_server(true, 403, false).await;
        let config = ClientConfig::new(addr, "localhost").insecure();
        let error = expect_connect_error(
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                WtH2Client::connect(config, "/test"),
            )
            .await
            .expect("タイムアウトしないこと"),
        );
        assert!(
            matches!(error, TransportError::ConnectFailed { status: Some(403) }),
            "{error:?}"
        );
        server
            .await
            .expect("サーバータスクの join に成功すること")
            .expect("サーバーがエラーなく終了すること");
    }

    /// SETTINGS で WebTransport の対応を宣言しないサーバーとは接続できない
    #[tokio::test(flavor = "multi_thread")]
    async fn wt_h2_requires_webtransport_settings() {
        let (addr, server) = spawn_connect_only_server(false, 200, true).await;
        let config = ClientConfig::new(addr, "localhost").insecure();
        let error = expect_connect_error(
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                WtH2Client::connect(config, "/test"),
            )
            .await
            .expect("タイムアウトしないこと"),
        );
        assert!(
            matches!(error, TransportError::InvalidState(_)),
            "{error:?}"
        );
        server
            .await
            .expect("サーバータスクの join に成功すること")
            .expect("サーバーがエラーなく終了すること");
    }

    /// 2xx でも WT-Protocol が無い応答はプロトコル交渉の失敗にする (draft §3.3)
    #[tokio::test(flavor = "multi_thread")]
    async fn wt_h2_requires_wt_protocol_selection() {
        let (addr, server) = spawn_connect_only_server(true, 200, false).await;
        let config = ClientConfig::new(addr, "localhost").insecure();
        let error = expect_connect_error(
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                WtH2Client::connect(config, "/test"),
            )
            .await
            .expect("タイムアウトしないこと"),
        );
        assert!(
            matches!(error, TransportError::InvalidState(_)),
            "{error:?}"
        );
        server
            .await
            .expect("サーバータスクの join に成功すること")
            .expect("サーバーがエラーなく終了すること");
    }
}
