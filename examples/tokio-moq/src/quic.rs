//! QUIC クライアント接続の確立
//!
//! TLS / ALPN / datagram エンドポイントを構築し、s2n-quic で接続を確立する。

use std::sync::Arc;

use s2n_quic::Client;
use s2n_quic::client::Connect;
use s2n_quic::connection::Connection;
use s2n_quic::provider::event::events::{Frame, FrameReceived};
use s2n_quic::provider::tls::rustls as s2n_rustls;
use tokio::sync::mpsc;

use crate::error::{Result, TransportError};

/// ALPN プロトコル識別子 (draft-ietf-moq-transport-22 §6.2 (Session establishment))
///
/// WebTransport の `WT-Available-Protocols` と共通の定数を使う。
const ALPN: &[u8] = crate::MOQT_PROTOCOL.as_bytes();

/// peer から受信した STOP_SENDING の観測結果
///
/// RFC 9000 §19.5 の STOP_SENDING を、送信ストリームを所有する MOQT 層へ渡すための値である。
/// `error_code` は wire のコードであり、WebTransport over HTTP/3 は HTTP/3 の code space を
/// 共有するため `wt_stop_sending_error_code` で MOQT のコードへ戻す
/// (draft-ietf-webtrans-http3-16 §4.4 (Resetting Data Streams))。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopSendingObserved {
    /// STOP_SENDING の対象となったストリーム ID
    pub stream_id: u64,
    /// peer が載せた wire のエラーコード
    pub error_code: u64,
}

/// peer の STOP_SENDING の観測を受け取る receiver
///
/// 接続終了で channel が閉じると `recv` は `None` を返す。呼び出し側は receiver を落として
/// 観測を止めること (閉じた receiver を poll し続けると待たずに `None` が返り続ける)。
pub type StopSendingReceiver = mpsc::UnboundedReceiver<StopSendingObserved>;

/// 接続イベントの購読者
///
/// 2 つの役割を持つ。
///
/// - 受信パケットの内訳 (受信 / 破棄 / 復号失敗) を数える診断。`MOQT_PACKET_DIAG=1` の
///   ときだけ 1 秒ごとのログ出力を有効化する。受信が止まったときに「パケットが来ていない」
///   のか「来ているが復号に失敗している」のかを切り分けるために使う。常時出力すると
///   計測そのものを歪めるため、環境変数で明示的に有効化する
/// - peer の STOP_SENDING の観測。診断ではなく MOQT 層へ渡す必要のある情報であるため、
///   `MOQT_PACKET_DIAG` の有無に関わらず常に行う
///
/// s2n-quic の `with_event` は 1 接続につき 1 購読者しか差し込めないため、診断と観測を
/// 同じ購読者に同居させている。
#[derive(Debug, Clone, Default)]
pub(crate) struct ConnectionObserver {
    received: Arc<std::sync::atomic::AtomicU64>,
    dropped: Arc<std::sync::atomic::AtomicU64>,
    decryption_failed: Arc<std::sync::atomic::AtomicU64>,
    /// peer の STOP_SENDING を流す先。観測が不要な場合は `None`
    stop_sending_tx: Option<mpsc::UnboundedSender<StopSendingObserved>>,
}

impl ConnectionObserver {
    /// peer の STOP_SENDING を `tx` へ流す購読者を作る
    pub(crate) fn with_stop_sending(tx: mpsc::UnboundedSender<StopSendingObserved>) -> Self {
        Self {
            stop_sending_tx: Some(tx),
            ..Self::default()
        }
    }

    /// 1 秒ごとに内訳を出すタスクを起動する
    pub(crate) fn spawn_logger(&self) {
        let received = Arc::clone(&self.received);
        let dropped = Arc::clone(&self.dropped);
        let decryption_failed = Arc::clone(&self.decryption_failed);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                tracing::warn!(
                    "PKTDIAG received={} dropped={} decryption_failed={}",
                    received.load(std::sync::atomic::Ordering::Relaxed),
                    dropped.load(std::sync::atomic::Ordering::Relaxed),
                    decryption_failed.load(std::sync::atomic::Ordering::Relaxed),
                );
            }
        });
    }
}

impl s2n_quic::provider::event::Subscriber for ConnectionObserver {
    type ConnectionContext = ();

    fn create_connection_context(
        &mut self,
        _meta: &s2n_quic::provider::event::ConnectionMeta,
        _info: &s2n_quic::provider::event::ConnectionInfo<'_>,
    ) -> Self::ConnectionContext {
    }

    fn on_packet_received(
        &mut self,
        _context: &mut Self::ConnectionContext,
        _meta: &s2n_quic::provider::event::ConnectionMeta,
        _event: &s2n_quic::provider::event::events::PacketReceived,
    ) {
        self.received
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn on_frame_received(
        &mut self,
        _context: &mut Self::ConnectionContext,
        _meta: &s2n_quic::provider::event::ConnectionMeta,
        event: &FrameReceived<'_>,
    ) {
        // peer が送出した STOP_SENDING を観測して MOQT 層へ渡す。送信 API の失敗からは
        // 検知できない場合がある (FIN ack 済みのストリームでは s2n-quic が RESET を
        // 組み立てず、STOP_SENDING に由来するエラーが `send` / `finish` に現れない) ため、
        // フレームの観測を検知の唯一の経路にする。
        // 自側が送出した STOP_SENDING はこのフックに現れないため、peer 由来に限られる。
        if let Frame::StopSending { id, error_code, .. } = &event.frame
            && let Some(tx) = &self.stop_sending_tx
        {
            let _ = tx.send(StopSendingObserved {
                stream_id: *id,
                error_code: *error_code,
            });
        }
    }

    fn on_packet_dropped(
        &mut self,
        _context: &mut Self::ConnectionContext,
        _meta: &s2n_quic::provider::event::ConnectionMeta,
        event: &s2n_quic::provider::event::events::PacketDropped,
    ) {
        self.dropped
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let s2n_quic::provider::event::events::PacketDropReason::DecryptionFailed { .. } =
            &event.reason
        {
            self.decryption_failed
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// パケットの診断を有効にするか (環境変数 `MOQT_PACKET_DIAG=1` で明示的に有効化)
pub(crate) fn packet_diag_enabled() -> bool {
    std::env::var("MOQT_PACKET_DIAG").is_ok_and(|v| v == "1")
}

/// QUIC クライアント接続を確立する
///
/// `socket_addr` は呼び出し側が [`crate::resolve_socket_addrs`] で解決した接続先の
/// 1 件を使う。複数に解決された場合の試行順は [`crate::connect_with_fallback`] が決める。
///
/// 戻り値の [`StopSendingReceiver`] は peer の STOP_SENDING の観測であり、QUIC 直接接続
/// (`moqt://`) では QUIC の code space のコードがそのまま流れる。MOQT 層が送信ストリームの
/// 終端として扱う。
pub async fn connect(
    socket_addr: std::net::SocketAddr,
    server_name: &str,
    cert_path: Option<&str>,
) -> Result<(Connection, StopSendingReceiver)> {
    let tls = build_tls_client(cert_path)?;

    let datagram_endpoint = s2n_quic::provider::datagram::default::Endpoint::builder()
        .with_recv_capacity(64)
        .map_err(|e| TransportError::Quic(format!("datagram endpoint: {e}")))?
        .build()
        .expect("datagram endpoint build must succeed after recv capacity is set");

    // 受信パケットの内訳を見るときだけ記録する。
    //
    // `with_event` は builder の型を変えるため if で分岐できない。購読者は常に
    // 差し込み、カウンタの加算とログ出力を環境変数で切り替える。無効時は
    // atomic の加算だけが残るが、分岐のための型の複雑さを避けるほうを取る。
    // peer の STOP_SENDING の観測は診断ではなく MOQT 層へ渡す必要のある情報であるため、
    // `MOQT_PACKET_DIAG` の有無に関わらず常に行う。
    let (stop_sending_tx, stop_sending_rx) = mpsc::unbounded_channel();
    let diag = ConnectionObserver::with_stop_sending(stop_sending_tx);
    if packet_diag_enabled() {
        diag.spawn_logger();
        tracing::warn!("packet diagnostics are enabled (MOQT_PACKET_DIAG=1)");
    }

    let client = Client::builder()
        .with_tls(tls)
        .map_err(|e| TransportError::Quic(format!("TLS configuration failed: {e}")))?
        .with_io(crate::local_bind_addr(socket_addr))
        .map_err(|e| TransportError::Quic(format!("I/O binding failed: {e}")))?
        .with_datagram(datagram_endpoint)
        .map_err(|e| TransportError::Quic(format!("datagram provider failed: {e}")))?
        .with_limits(build_limits()?)
        .map_err(|e| TransportError::Quic(format!("limits provider failed: {e}")))?
        .with_event(diag)
        .map_err(|e| TransportError::Quic(format!("event provider failed: {e}")))?
        .start()
        .map_err(|e| TransportError::Quic(format!("client start failed: {e}")))?;

    let connect = Connect::new(socket_addr).with_server_name(server_name);
    let connection = client
        .connect(connect)
        .await
        .map_err(|e| TransportError::Quic(format!("connection failed: {e}")))?;

    tracing::info!("Connected to {socket_addr}");
    Ok((connection, stop_sending_rx))
}

/// クライアントの接続 limits を構築する。
///
/// 既定のままだと、ピアが開ける単方向 stream の累積上限が 100 と小さい。
/// この example は audio を 20ms ごと、video を 1 frame ごとに 1 本の
/// subgroup stream で受けるため、stream の消費速度が上限の回復速度を上回り、
/// 購読が数秒から数十秒で止まる。上限とウィンドウを明示して余裕を持たせる。
///
/// 値は s2n-quic の既定 (100 stream / 3.75MB) に対して、この example が
/// 必要とする同時実行数 (MAX_CONCURRENT_STREAMS = 4) と 1 group あたりの
/// サイズから決めている。過大にしても受信側が accept しなければ flow control
/// で止まるだけなので、上限そのものは大きくて困らない。
fn build_limits() -> Result<s2n_quic::provider::limits::Limits> {
    s2n_quic::provider::limits::Limits::new()
        // ピア (relay) が開ける単方向 stream の累積上限
        .with_max_open_remote_unidirectional_streams(10_000)
        .map_err(|e| TransportError::Quic(format!("remote uni stream limit: {e}")))?
        // ピア (relay) が開ける双方向 stream の累積上限
        .with_max_open_remote_bidirectional_streams(1_000)
        .map_err(|e| TransportError::Quic(format!("remote bidi stream limit: {e}")))?
        // 接続レベルで受け入れを許す総バイト数
        .with_data_window(64 * 1024 * 1024)
        .map_err(|e| TransportError::Quic(format!("data window: {e}")))?
        // 単方向 stream (relay -> subscriber の data stream) 1 本あたりの窓
        .with_unidirectional_data_window(16 * 1024 * 1024)
        .map_err(|e| TransportError::Quic(format!("uni stream window: {e}")))?
        .with_bidirectional_local_data_window(16 * 1024 * 1024)
        .map_err(|e| TransportError::Quic(format!("bidi local stream window: {e}")))?
        .with_bidirectional_remote_data_window(16 * 1024 * 1024)
        .map_err(|e| TransportError::Quic(format!("bidi remote stream window: {e}")))
}

/// rustls TLS クライアントを構築する
fn build_tls_client(cert_path: Option<&str>) -> Result<s2n_rustls::Client> {
    match cert_path {
        Some(path) => {
            // CA 証明書を指定して証明書検証を行う
            let client = s2n_rustls::Client::builder()
                .with_certificate(std::path::Path::new(path))
                .map_err(|e| {
                    TransportError::Quic(format!("failed to load certificate '{path}': {e}"))
                })?
                .with_application_protocols([ALPN].iter())
                .map_err(|e| TransportError::Quic(format!("failed to set ALPN: {e}")))?
                .build()
                .map_err(|e| TransportError::Quic(format!("failed to build TLS client: {e}")))?;
            Ok(client)
        }
        None => {
            // 開発用: 証明書検証をスキップする
            tracing::warn!("TLS certificate verification is disabled (development mode)");
            build_insecure_tls_client(&[ALPN])
        }
    }
}

/// 証明書検証をスキップする開発用 TLS クライアントを構築する
///
/// `alpn_protocols` には接続方式に応じた ALPN を渡す。QUIC 直接接続は MoQT の
/// プロトコル識別子、WebTransport over HTTP/3 は `h3` を使う。
/// s2n-quic の default TLS は feature `provider-tls-rustls` で rustls に解決されるため、
/// rustls の `ClientConfig` を直接構築して渡す。
pub(crate) fn build_insecure_tls_client(alpn_protocols: &[&[u8]]) -> Result<s2n_rustls::Client> {
    let provider = rustls::crypto::aws_lc_rs::default_provider();

    let config = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| TransportError::Quic(format!("failed to set TLS versions: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerifier))
        .with_no_client_auth();

    let mut config = config;
    config.alpn_protocols = alpn_protocols.iter().map(|p| p.to_vec()).collect();

    Ok(s2n_rustls::Client::from(config))
}

/// 証明書検証をスキップする Verifier (開発用)
#[derive(Debug)]
struct NoVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
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
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
