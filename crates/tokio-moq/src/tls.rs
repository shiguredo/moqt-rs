//! TLS (rustls) のクライアント設定
//!
//! すべての経路が共有する。TLS の組み立て (CA 証明書の読み込み、システムルートストアの
//! 読み込み、証明書検証の無効化) は共通であり、ALPN だけが異なる。TCP + TLS を使う経路
//! (QMux と WebTransport over HTTP/2) と、QUIC を使う経路 (native QUIC と WebTransport over
//! HTTP/3) のどちらもここで作った設定を使う。

use std::sync::Arc;

use rustls::pki_types::pem::PemObject;

use crate::error::{Result, TransportError};

/// rustls のクライアント設定を構築する
///
/// 証明書の検証方法は次の順で決める。
///
/// - `insecure` が true: 証明書検証を行わない (開発用)。`ca_cert_pem` が指定されていても
///   無視する
/// - `ca_cert_pem` が指定されている: その CA 証明書だけをルートとして検証する
/// - どちらも指定されていない: OS のシステムルートストアで検証する
///
/// `alpn` は TLS の ALPN に載せる 1 要素で、経路ごとに異なる (`h2` / `h3` / `moqt-23`)。
///
/// システムルートストアからルートを 1 つも読み込めない場合はエラーを返す。検証を
/// スキップする経路へはフォールバックしない。
///
/// 検証を無効化したことの警告はこの関数が出す。検証を無効化できるのは `insecure` を
/// 明示した場合だけである。
///
/// # Errors
///
/// CA 証明書の PEM の解析・追加に失敗した場合、システムルートストアからルートを
/// 1 つも読み込めない場合に [`TransportError`] になる。
pub(crate) fn build_client_config(
    ca_cert_pem: Option<&str>,
    insecure: bool,
    alpn: &[u8],
) -> Result<Arc<rustls::ClientConfig>> {
    let mut tls_config = if insecure {
        if ca_cert_pem.is_some() {
            tracing::warn!(
                "TLS certificate verification is disabled (insecure mode); the CA certificate is ignored"
            );
        } else {
            tracing::warn!(
                "TLS certificate verification is disabled (insecure mode); the server certificate is not verified"
            );
        }
        // 開発用: 証明書検証を無効化する
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification::new()))
            .with_no_client_auth()
    } else {
        let roots = match ca_cert_pem {
            Some(pem) => roots_from_pem(pem)?,
            None => system_roots()?,
        };
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth()
    };
    tls_config.alpn_protocols = vec![alpn.to_vec()];
    Ok(Arc::new(tls_config))
}

/// QUIC 用の rustls クライアント設定を構築する
///
/// `cert_path` が指定された場合は、そのファイルを CA 証明書として読み込んで検証を行う。
/// `insecure` が true の場合は証明書検証をスキップする (開発用)。どちらも指定されない
/// 場合は OS のシステムルートストアで検証する。`alpn` は QUIC の ALPN に載せる
/// 識別子 (`moqt-23` / `h3`) である。
#[cfg(any(feature = "quic-tobi", feature = "quic-s2n"))]
pub(crate) fn build_quic_client_config(
    cert_path: Option<&str>,
    insecure: bool,
    alpn: &[u8],
) -> Result<Arc<rustls::ClientConfig>> {
    if insecure {
        return build_client_config(None, true, alpn);
    }
    match cert_path {
        Some(path) => {
            let pem = std::fs::read_to_string(path).map_err(|e| {
                TransportError::Quic(format!("failed to read certificate '{path}': {e}"))
            })?;
            build_client_config(Some(&pem), false, alpn)
        }
        None => build_client_config(None, false, alpn),
    }
}

/// CA 証明書の PEM からルートストアを作る
///
/// PEM に複数の証明書が含まれる場合はすべて追加する。1 つでも解析できない、または
/// ルートとして追加できない証明書があればエラーにする (指定した CA を黙って捨てない)。
fn roots_from_pem(pem: &str) -> Result<rustls::RootCertStore> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls::pki_types::CertificateDer::pem_slice_iter(pem.as_bytes()) {
        let cert = cert.map_err(|e| {
            TransportError::Internal(format!("failed to parse the CA certificate: {e}"))
        })?;
        roots.add(cert).map_err(|e| {
            TransportError::Internal(format!("failed to add the CA certificate: {e}"))
        })?;
    }
    if roots.is_empty() {
        return Err(TransportError::Internal(
            "the provided CA certificate PEM contains no certificate".to_string(),
        ));
    }
    Ok(roots)
}

/// OS のシステムルートストアからルートストアを作る
///
/// `rustls-native-certs` がプラットフォームの証明書ストア (macOS のキーチェーン、
/// Windows の証明書ストア、Unix の CA バンドル) から読み込んだルートを使う。
/// 読み込めた証明書が 1 つも無い場合、または 1 つもルートとして解析できない場合は
/// エラーにする。空のルートストアでハンドシェイクを続けると、ピアの証明書を検証できない
/// まま接続することになるため、ここで失敗させる。
fn system_roots() -> Result<rustls::RootCertStore> {
    let native = rustls_native_certs::load_native_certs();
    roots_from_native_certs(native.certs, &native.errors)
}

/// `load_native_certs()` の結果からルートストアを作る
fn roots_from_native_certs(
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    errors: &[rustls_native_certs::Error],
) -> Result<rustls::RootCertStore> {
    if certs.is_empty() {
        // 読み込みに失敗した理由があれば添える。環境変数やパスの情報であり機密値ではない
        let reason = if errors.is_empty() {
            "the platform certificate store is empty".to_string()
        } else {
            native_cert_error_text(errors)
        };
        return Err(TransportError::Internal(format!(
            "failed to load system root certificates: {reason}"
        )));
    }
    // 一部のソースだけ読めなかった場合も、黙って捨てずに警告する
    if !errors.is_empty() {
        tracing::warn!(
            "Failed to load some system root certificate sources: {}",
            native_cert_error_text(errors)
        );
    }
    let mut roots = rustls::RootCertStore::empty();
    let (added, ignored) = roots.add_parsable_certificates(certs);
    if ignored > 0 {
        tracing::warn!(
            "Ignored {ignored} system root certificate(s) that could not be parsed as a certificate"
        );
    }
    if added == 0 {
        return Err(TransportError::Internal(
            "failed to load system root certificates: no loaded certificate could be parsed as a root"
                .to_string(),
        ));
    }
    Ok(roots)
}

/// `load_native_certs()` の読み込みエラーを 1 つの文字列にまとめる
fn native_cert_error_text(errors: &[rustls_native_certs::Error]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
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

#[cfg(test)]
mod tests {
    use super::*;

    /// システムルートを 1 つも読み込めない場合はエラーになる
    ///
    /// 空のルートストアのまま接続すると証明書を検証できない。検証をスキップする
    /// 経路へフォールバックせず、接続の前にエラーにすること。
    #[test]
    fn system_roots_rejects_an_empty_store() {
        let error = roots_from_native_certs(Vec::new(), &[])
            .expect_err("ルートが 1 つも無ければエラーになること");
        assert!(
            error.to_string().contains("failed to load system root"),
            "システムルートの読み込み失敗として返ること: {error}"
        );
    }

    /// 読み込めた証明書が解析できない場合もエラーになる
    ///
    /// `load_native_certs()` が壊れた DER を返しても、ルートが 0 件なら検証は成立しない。
    #[test]
    fn system_roots_rejects_unparsable_certificates() {
        let error = roots_from_native_certs(
            vec![rustls::pki_types::CertificateDer::from(vec![
                0x00, 0x01, 0x02,
            ])],
            &[],
        )
        .expect_err("解析できない証明書しか無ければエラーになること");
        assert!(
            error.to_string().contains("failed to load system root"),
            "システムルートの読み込み失敗として返ること: {error}"
        );
    }

    /// 証明書検証を無効化した設定でも ALPN は指定どおりになる
    ///
    /// 開発用の経路でも ALPN を落とすとハンドシェイクの交渉結果が変わってしまう。
    #[test]
    fn insecure_config_keeps_the_alpn() {
        let config =
            build_client_config(None, true, b"moqt-23").expect("検証を無効化した設定を作れること");
        assert_eq!(
            config.alpn_protocols,
            vec![b"moqt-23".to_vec()],
            "ALPN が指定した値になること"
        );
    }
}
