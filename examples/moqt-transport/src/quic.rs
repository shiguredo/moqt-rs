//! QUIC クライアント接続の確立
//!
//! TLS / ALPN / datagram エンドポイントを構築し、s2n-quic で接続を確立する。

use std::sync::Arc;

use s2n_quic::Client;
use s2n_quic::client::Connect;
use s2n_quic::connection::Connection;
use s2n_quic::provider::tls::rustls as s2n_rustls;

use crate::error::{Result, TransportError};

/// ALPN プロトコル識別子 (draft-ietf-moq-transport-21 §6.2 (Session establishment))
///
/// WebTransport の `WT-Available-Protocols` と共通の定数を使う。
const ALPN: &[u8] = crate::MOQT_PROTOCOL.as_bytes();

/// QUIC クライアント接続を確立する
pub async fn connect(
    authority: &str,
    server_name: &str,
    cert_path: Option<&str>,
) -> Result<Connection> {
    let tls = build_tls_client(cert_path)?;

    let datagram_endpoint = s2n_quic::provider::datagram::default::Endpoint::builder()
        .with_recv_capacity(64)
        .map_err(|e| TransportError::Quic(format!("datagram endpoint: {e}")))?
        .build()
        .expect("datagram endpoint build must succeed after recv capacity is set");

    let client = Client::builder()
        .with_tls(tls)
        .map_err(|e| TransportError::Quic(format!("TLS configuration failed: {e}")))?
        .with_io("0.0.0.0:0")
        .map_err(|e| TransportError::Quic(format!("I/O binding failed: {e}")))?
        .with_datagram(datagram_endpoint)
        .map_err(|e| TransportError::Quic(format!("datagram provider failed: {e}")))?
        .start()
        .map_err(|e| TransportError::Quic(format!("client start failed: {e}")))?;

    let socket_addr: std::net::SocketAddr = authority
        .parse()
        .map_err(|e| TransportError::Quic(format!("invalid server address '{authority}': {e}")))?;

    let connect = Connect::new(socket_addr).with_server_name(server_name);
    let connection = client
        .connect(connect)
        .await
        .map_err(|e| TransportError::Quic(format!("connection failed: {e}")))?;

    tracing::info!("Connected to {authority}");
    Ok(connection)
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
