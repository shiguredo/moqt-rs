//! TCP + TLS (rustls) のクライアント設定
//!
//! TCP + TLS を使う経路 (QMux と WebTransport over HTTP/2) が共有する。両者は TLS の
//! 組み立て (CA 証明書の読み込み、証明書検証の無効化) が同じで、ALPN だけが異なる。
//! QUIC を使う経路 (native QUIC と WebTransport over HTTP/3) は s2n-quic に TLS を
//! 任せるため、このモジュールを使わない。

use std::sync::Arc;

use rustls::pki_types::pem::PemObject;

use crate::error::{Result, TransportError};

/// rustls のクライアント設定を構築する
///
/// `disable_cert_validation` が true の場合と CA 証明書が指定されていない場合は
/// 証明書検証を行わない (開発用)。両方指定された場合は無効化を優先する。
/// `alpn` は TLS の ALPN に載せる 1 要素で、経路ごとに異なる (`h2` と `moqt-22`)。
///
/// 検証を無効化したことの警告は呼び出し側が出す (経路ごとに理由が異なるため)。
pub(crate) fn build_client_config(
    ca_cert_pem: Option<&str>,
    disable_cert_validation: bool,
    alpn: &[u8],
) -> Result<Arc<rustls::ClientConfig>> {
    let use_insecure = disable_cert_validation || ca_cert_pem.is_none();
    let mut tls_config = if use_insecure {
        if ca_cert_pem.is_some() {
            tracing::warn!(
                "TLS certificate verification is disabled by ClientConfig::insecure; the CA certificate is ignored"
            );
        }
        // 開発用: 証明書検証を無効化する
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification::new()))
            .with_no_client_auth()
    } else {
        // `use_insecure` が false のときは CA 証明書が必ずある
        let pem =
            ca_cert_pem.expect("ca_cert_pem is present when certificate verification is enabled");
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
    tls_config.alpn_protocols = vec![alpn.to_vec()];
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
