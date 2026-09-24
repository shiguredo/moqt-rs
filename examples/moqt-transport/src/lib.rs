//! examples 共有トランスポート crate
//!
//! `moqt-publisher` / `moqt-subscriber` の両バイナリが共通で利用する
//! QUIC / WebTransport over HTTP/3 のトランスポート層を提供する。
//!
//! publisher は能動的に送信ストリームを open する側、subscriber は受動的に
//! 受信ストリームを accept する側という非対称があるため、両者の union API を
//! ここに置き、各バイナリは必要なメソッドだけを呼び出す。

use std::net::SocketAddr;

use crate::error::TransportError;

/// draft-ietf-moq-transport-21 §9.1 (SETUP) に基づく基本 Setup Options を構築する
///
/// path (0x01) と authority (0x05) は QUIC 接続時にのみ含まれ、
/// WebTransport 使用時は draft-ietf-moq-transport-21 §9.1.1 (AUTHORITY) /
/// §9.1.2 (PATH) により MUST NOT のため呼び出し側で None を渡す。
/// §9.1.5 (MOQT_IMPLEMENTATION) の `impl_name` (option type 0x07) は常に含まれる。
pub fn build_setup_options(
    path: Option<&str>,
    authority: Option<&str>,
    impl_name: &str,
) -> shiguredo_moqt::parameter::SetupOptions {
    use shiguredo_moqt::{
        parameter::SETUP_OPTION_AUTHORITY, parameter::SETUP_OPTION_MOQT_IMPLEMENTATION,
        parameter::SETUP_OPTION_PATH, parameter::SetupOption, parameter::SetupOptionValue,
        parameter::SetupOptions,
    };
    let mut options = SetupOptions::new();
    if let Some(p) = path {
        options.push(SetupOption {
            // §9.1.2 (PATH): 将来 draft 改訂で変更される可能性がある
            option_type: SETUP_OPTION_PATH,
            value: SetupOptionValue::Bytes(p.as_bytes().to_vec()),
        });
    }
    if let Some(a) = authority {
        options.push(SetupOption {
            // §9.1.1 (AUTHORITY): 将来 draft 改訂で変更される可能性がある
            option_type: SETUP_OPTION_AUTHORITY,
            value: SetupOptionValue::Bytes(a.as_bytes().to_vec()),
        });
    }
    options.push(SetupOption {
        // §9.1.5 (MOQT_IMPLEMENTATION): 将来 draft 改訂で変更される可能性がある
        option_type: SETUP_OPTION_MOQT_IMPLEMENTATION,
        value: SetupOptionValue::Bytes(impl_name.as_bytes().to_vec()),
    });
    options
}

/// MOQT のプロトコル識別子 (draft-ietf-moq-transport-21 §6.2 (Session establishment))
///
/// QUIC の ALPN と WebTransport の `WT-Available-Protocols` の双方で使う。draft-21 では `moqt-21`。
pub const MOQT_PROTOCOL: &str = "moqt-21";

pub mod error;
pub mod metrics;
pub mod moqt_client;
pub mod quic;
pub mod transport;
pub mod webtransport;

// 型はサブモジュールパス (`error::...` / `transport::...` / `webtransport::...` / `quic::...`) 経由で
// 参照する。最上位再エクスポートは設けない。

/// トランスポート種別
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Transport {
    /// QUIC 直接接続 (moqt:// スキーム)
    Quic,
    /// WebTransport over HTTP/3 (https:// スキーム)
    WebTransport,
}

/// パース済みの接続先情報
#[derive(Debug, Clone)]
pub struct ServerUrl {
    /// トランスポート種別
    pub transport: Transport,
    /// ホスト名または IP アドレス (ポートを含む場合がある)
    ///
    /// URL でポートを省略した場合はポートを含まない。接続先の決定では
    /// 既定ポート 443 を補って解釈する。
    pub authority: String,
    /// パス部分 (query を含む)
    pub path: String,
}

/// URL をパースしてトランスポート種別・ authority ・ path を取得する
pub fn parse_url(url: &str) -> Result<ServerUrl, String> {
    if let Some(rest) = url.strip_prefix("moqt://") {
        let (authority, path) = split_authority_path(rest);
        if authority.is_empty() {
            return Err("moqt:// URL requires authority (e.g. moqt://localhost:4443)".to_string());
        }
        Ok(ServerUrl {
            transport: Transport::Quic,
            authority,
            path,
        })
    } else if let Some(rest) = url.strip_prefix("https://") {
        let (authority, path) = split_authority_path(rest);
        if authority.is_empty() {
            return Err(
                "https:// URL requires authority (e.g. https://localhost:4443)".to_string(),
            );
        }
        Ok(ServerUrl {
            transport: Transport::WebTransport,
            authority,
            path,
        })
    } else {
        Err(format!(
            "unsupported URL scheme: {url} (use moqt:// or https://)"
        ))
    }
}

/// authority と path を分離する
///
/// path には query (`?` 以降) を含める。draft-ietf-moq-transport-21 §9.1.2 (PATH) は
/// query が存在する場合に `?` と query を PATH option へ連結することを MUST とするため。
pub fn split_authority_path(rest: &str) -> (String, String) {
    // authority は最初の `/` または `?` まで
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = rest[..end].to_string();
    let path = if end == rest.len() {
        "/".to_string()
    } else if rest.as_bytes()[end] == b'?' {
        // path が空で query のみの場合は `/` を補う
        format!("/{}", &rest[end..])
    } else {
        rest[end..].to_string()
    };
    (authority, path)
}

/// authority から host 部 (ポートを除く) を取り出す
///
/// TLS の SNI / 証明書検証に使う server_name はポートを含まないため、ここで分離する。
/// IPv6 リテラルは `[` から `]` までを host とし、`]` が無い場合は authority 全体を返す。
/// IPv4 / ホスト名は最後の `:` より前を host とし、ポートが無い場合は authority 全体を返す。
pub fn host_from_authority(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return match rest.find(']') {
            Some(end) => &rest[..end],
            // 閉じ括弧が無い不正な形式はそのまま返す (authority_parts が解決段階で拒否する)
            None => authority,
        };
    }
    match authority.rfind(':') {
        Some(pos) => &authority[..pos],
        None => authority,
    }
}

/// URI でポートを省略したときに使う既定ポート
///
/// draft-ietf-moq-transport-21 §6.1.2 (Dereferencing a MOQT URI) は
/// "If the port is omitted in the URI, a default port of 443 is used." と定める。
/// `https://` (WebTransport) も同じ 443 を既定値として使う。
/// この仕様は将来 draft 改訂で変更される可能性がある。
const DEFAULT_MOQT_PORT: u16 = 443;

/// authority から取り出した接続先の host と port
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AuthorityParts<'a> {
    /// ホスト名または IP アドレス (ポートを含まない)
    host: &'a str,
    /// ポート (authority で省略されている場合は [`DEFAULT_MOQT_PORT`])
    port: u16,
}

impl AuthorityParts<'_> {
    /// `ToSocketAddrs` (`SocketAddr` または `lookup_host`) として解釈できる形の接続先文字列
    ///
    /// IPv6 リテラルは `[` `]` で囲む。[`host_from_authority`] はブラケットを外して
    /// host を返すため、そのまま連結すると `2001:db8::1:443` になって解釈できない。
    fn socket_addr_string(self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// authority から host と port を取り出す
///
/// host は [`host_from_authority`] と同じ規則で取り出す (IPv6 リテラルは `[` から `]`
/// まで)。port が省略されている場合は [`DEFAULT_MOQT_PORT`] にする
/// (draft-ietf-moq-transport-21 §6.1.2)。
///
/// 次の authority はエラーにする。draft-ietf-moq-transport-21 §6.1 (MOQT URI Scheme) は
/// "The authority portion MUST NOT contain an empty host portion." と定める
/// (この仕様は将来 draft 改訂で変更される可能性がある)。
///
/// IPv6 リテラルを `[` `]` で囲む規則は RFC 3986 §3.2.2 (Host)、ポートを数字にする
/// 規則は RFC 3986 §3.2.3 (Port) による。
///
/// - host が空 (`""` / `":4443"`)
/// - IPv6 リテラルの `]` が無い、または `]` の直後がポート区切りでない (`[::1` / `[::1]x`)
/// - ブラケット無しの host に `:` が残る (裸の IPv6 リテラル `2001:db8::1`)
/// - ポートの区切りがあるのに数字が続かない (`127.0.0.1:` / `127.0.0.1:abc`)
fn authority_parts(authority: &str) -> Result<AuthorityParts<'_>, TransportError> {
    let host = host_from_authority(authority);
    let port_text = if authority.starts_with('[') {
        // IPv6 リテラルは `]` の後ろだけがポート部分になる
        let end = authority
            .find(']')
            .ok_or_else(|| invalid_authority(authority, "missing ']'"))?;
        let after = &authority[end + 1..];
        if after.is_empty() {
            None
        } else if let Some(text) = after.strip_prefix(':') {
            Some(text)
        } else {
            return Err(invalid_authority(authority, "unexpected text after ']'"));
        }
    } else {
        authority.rfind(':').map(|pos| &authority[pos + 1..])
    };

    if host.is_empty() {
        return Err(invalid_authority(authority, "empty host"));
    }
    if !authority.starts_with('[') && host.contains(':') {
        return Err(invalid_authority(
            authority,
            "IPv6 literal must be enclosed in '[' and ']'",
        ));
    }

    let port = match port_text {
        None => DEFAULT_MOQT_PORT,
        Some("") => return Err(invalid_authority(authority, "empty port")),
        Some(text) => text
            .parse::<u16>()
            .map_err(|e| invalid_authority(authority, &e.to_string()))?,
    };

    Ok(AuthorityParts { host, port })
}

/// authority の解釈に失敗したことを表すエラーを作る
fn invalid_authority(authority: &str, reason: &str) -> TransportError {
    TransportError::InvalidAuthority(format!("invalid server address '{authority}': {reason}"))
}

/// authority を解決して接続先の `SocketAddr` を得る
///
/// host が IP リテラルなら `SocketAddr` に直接パースし、ホスト名なら
/// `tokio::net::lookup_host` で解決して最初の結果を使う。ポートが省略された
/// authority は既定ポート 443 を使う
/// (draft-ietf-moq-transport-21 §6.1.2)。解決したアドレスは接続前に info ログに出す。
// テスト方針: IP リテラルの経路は単体テストで固定し、ホスト名の経路は名前解決に
// 依存するため `localhost` の解決テストと実機確認で確認する。
pub async fn resolve_socket_addr(authority: &str) -> Result<SocketAddr, TransportError> {
    let parts = authority_parts(authority)?;
    let addr_text = parts.socket_addr_string();

    let addr = if let Ok(addr) = addr_text.parse::<SocketAddr>() {
        addr
    } else {
        let mut resolved = tokio::net::lookup_host(addr_text.as_str())
            .await
            .map_err(|e| resolve_failed(authority, &e.to_string()))?;
        resolved
            .next()
            .ok_or_else(|| resolve_failed(authority, "no address"))?
    };

    tracing::info!("Resolved {authority} to {addr}");
    Ok(addr)
}

/// 名前解決に失敗したことを表すエラーを作る
fn resolve_failed(authority: &str, reason: &str) -> TransportError {
    TransportError::ResolutionFailed(format!("failed to resolve '{authority}': {reason}"))
}

/// 接続先と同じアドレスファミリでローカルソケットを bind するためのアドレス
///
/// `0.0.0.0:0` 固定だと IPv4 ソケットから IPv6 宛に送信できず、IPv6 に解決された
/// 接続先へパケットを送れない。接続先のファミリに合わせる。
pub(crate) fn local_bind_addr(remote: SocketAddr) -> &'static str {
    if remote.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// path 付き URL は authority と path に分離される
    #[test]
    fn split_authority_path_separates_authority_and_path() {
        assert_eq!(
            split_authority_path("127.0.0.1:4443/path"),
            ("127.0.0.1:4443".to_string(), "/path".to_string())
        );
    }

    /// path が無い場合は path を `/` にする
    #[test]
    fn split_authority_path_defaults_to_root() {
        assert_eq!(
            split_authority_path("127.0.0.1:4443"),
            ("127.0.0.1:4443".to_string(), "/".to_string())
        );
    }

    /// query は path に含める (draft-21 §9.1.2 PATH)
    #[test]
    fn split_authority_path_keeps_query_in_path() {
        assert_eq!(
            split_authority_path("127.0.0.1:4443/path?x=1&y=2"),
            ("127.0.0.1:4443".to_string(), "/path?x=1&y=2".to_string())
        );
    }

    /// path が空で query のみの場合も `/` を補って query を保持する
    #[test]
    fn split_authority_path_keeps_query_without_path() {
        assert_eq!(
            split_authority_path("127.0.0.1:4443?x=1"),
            ("127.0.0.1:4443".to_string(), "/?x=1".to_string())
        );
    }

    /// moqt:// URL は QUIC としてパースされ query が path に残る
    #[test]
    fn parse_url_parses_moqt_scheme_with_query() {
        let url = parse_url("moqt://127.0.0.1:4443/path?x=1").expect("URL のパースに成功すること");
        assert_eq!(url.transport, Transport::Quic);
        assert_eq!(url.authority, "127.0.0.1:4443");
        assert_eq!(url.path, "/path?x=1");
    }

    /// https:// URL は WebTransport としてパースされ query が path に残る
    #[test]
    fn parse_url_parses_https_scheme_with_query() {
        let url =
            parse_url("https://example.com:443/foo?bar=baz").expect("URL のパースに成功すること");
        assert_eq!(url.transport, Transport::WebTransport);
        assert_eq!(url.authority, "example.com:443");
        assert_eq!(url.path, "/foo?bar=baz");
    }

    /// 未対応スキームはエラーになる
    #[test]
    fn parse_url_rejects_unknown_scheme() {
        assert!(parse_url("ftp://127.0.0.1:4443").is_err());
    }

    /// IPv4 の authority からポートを除いた host を取り出せる
    #[test]
    fn host_from_authority_extracts_ipv4_host() {
        assert_eq!(host_from_authority("127.0.0.1:4443"), "127.0.0.1");
    }

    /// IPv6 リテラルの authority からブラケットを除いた host を取り出せる
    #[test]
    fn host_from_authority_extracts_ipv6_host() {
        assert_eq!(host_from_authority("[::1]:4443"), "::1");
    }

    /// ポートが無い IPv6 リテラルでも host を取り出せる
    #[test]
    fn host_from_authority_extracts_ipv6_host_without_port() {
        assert_eq!(host_from_authority("[2001:db8::1]"), "2001:db8::1");
    }

    /// ポートが無いホスト名は authority 全体を host として返す
    #[test]
    fn host_from_authority_keeps_host_without_port() {
        assert_eq!(host_from_authority("example.com"), "example.com");
    }

    /// ポート省略の authority は既定ポート 443 を使う (draft-21 §6.1.2)
    #[test]
    fn authority_parts_defaults_to_443_for_ipv4() {
        // `moqt://127.0.0.1/app` の authority
        let parts = authority_parts("127.0.0.1").expect("authority の解釈に成功すること");
        assert_eq!(parts.host, "127.0.0.1");
        assert_eq!(parts.port, 443);
        assert_eq!(parts.socket_addr_string(), "127.0.0.1:443");
    }

    /// ポート省略の IPv6 リテラルは既定ポート 443 を使い、接続先文字列はブラケットで囲む
    #[test]
    fn authority_parts_defaults_to_443_for_ipv6() {
        // `https://[2001:db8::1]/app` の authority
        let parts = authority_parts("[2001:db8::1]").expect("authority の解釈に成功すること");
        assert_eq!(parts.host, "2001:db8::1");
        assert_eq!(parts.port, 443);
        assert_eq!(parts.socket_addr_string(), "[2001:db8::1]:443");
    }

    /// ポート付きの IPv6 リテラルは指定したポートを使う
    #[test]
    fn authority_parts_keeps_explicit_port_for_ipv6() {
        let parts = authority_parts("[::1]:4443").expect("authority の解釈に成功すること");
        assert_eq!(parts.host, "::1");
        assert_eq!(parts.port, 4443);
        assert_eq!(parts.socket_addr_string(), "[::1]:4443");
    }

    /// ポート付きの authority は指定したポートを使う
    #[test]
    fn authority_parts_keeps_explicit_port() {
        let parts = authority_parts("127.0.0.1:4443").expect("authority の解釈に成功すること");
        assert_eq!(parts.host, "127.0.0.1");
        assert_eq!(parts.port, 4443);
        assert_eq!(parts.socket_addr_string(), "127.0.0.1:4443");
    }

    /// ポート省略のホスト名は既定ポート 443 を使う
    #[test]
    fn authority_parts_defaults_to_443_for_hostname() {
        let parts = authority_parts("relay.example.com").expect("authority の解釈に成功すること");
        assert_eq!(parts.host, "relay.example.com");
        assert_eq!(parts.port, 443);
        assert_eq!(parts.socket_addr_string(), "relay.example.com:443");
    }

    /// 数字でないポートはエラーになる
    #[test]
    fn authority_parts_rejects_non_numeric_port() {
        assert!(authority_parts("127.0.0.1:abc").is_err());
        assert!(authority_parts("[::1]:abc").is_err());
    }

    /// 範囲外のポートはエラーになる
    #[test]
    fn authority_parts_rejects_out_of_range_port() {
        assert!(authority_parts("127.0.0.1:65536").is_err());
    }

    /// ポートの区切りだけがある authority はエラーになる
    #[test]
    fn authority_parts_rejects_empty_port() {
        assert!(authority_parts("127.0.0.1:").is_err());
        assert!(authority_parts("[::1]:").is_err());
    }

    /// 閉じ括弧の後ろにポート以外が続く authority はエラーになる
    #[test]
    fn authority_parts_rejects_text_after_ipv6_literal() {
        assert!(authority_parts("[::1]x").is_err());
    }

    /// 閉じ括弧が無い IPv6 リテラルはエラーになる
    #[test]
    fn authority_parts_rejects_missing_ipv6_bracket() {
        assert!(authority_parts("[::1").is_err());
    }

    /// ブラケット無しの IPv6 リテラルはエラーになる
    ///
    /// 最終の `:` で host と port に分けると host `2001:db8:` / port `1` になり、
    /// 不正な接続先を組み立ててしまう。RFC 3986 どおりブラケットを要求する。
    #[test]
    fn authority_parts_rejects_bare_ipv6_literal() {
        assert!(authority_parts("::1").is_err());
        assert!(authority_parts("2001:db8::1").is_err());
    }

    /// host が空の authority はエラーになる (draft-21 §6.1 は MUST NOT)
    #[test]
    fn authority_parts_rejects_empty_host() {
        assert!(authority_parts("").is_err());
        assert!(authority_parts(":4443").is_err());
        assert!(authority_parts("[]").is_err());
    }

    /// URL から取り出した authority でもポートが補完される
    #[test]
    fn authority_parts_supplements_port_from_parsed_url() {
        let url = parse_url("moqt://127.0.0.1/app").expect("URL のパースに成功すること");
        let parts = authority_parts(&url.authority).expect("authority の解釈に成功すること");
        assert_eq!(parts.host, "127.0.0.1");
        assert_eq!(parts.port, 443);
        assert_eq!(parts.socket_addr_string(), "127.0.0.1:443");

        let url = parse_url("https://[2001:db8::1]/app").expect("URL のパースに成功すること");
        let parts = authority_parts(&url.authority).expect("authority の解釈に成功すること");
        assert_eq!(parts.host, "2001:db8::1");
        assert_eq!(parts.port, 443);
        assert_eq!(parts.socket_addr_string(), "[2001:db8::1]:443");
    }

    /// IPv4 の接続先には IPv4 のローカルソケットを使う
    #[test]
    fn local_bind_addr_uses_ipv4_for_ipv4_remote() {
        let remote: std::net::SocketAddr = "127.0.0.1:4443"
            .parse()
            .expect("アドレスの解釈に成功すること");
        assert_eq!(local_bind_addr(remote), "0.0.0.0:0");
    }

    /// IPv6 の接続先には IPv6 のローカルソケットを使う
    ///
    /// IPv4 ソケットから IPv6 宛に送信できないため、ファミリを合わせる。
    #[test]
    fn local_bind_addr_uses_ipv6_for_ipv6_remote() {
        let remote: std::net::SocketAddr = "[2001:db8::1]:4443"
            .parse()
            .expect("アドレスの解釈に成功すること");
        assert_eq!(local_bind_addr(remote), "[::]:0");
    }

    /// IP リテラルの authority は既定ポートを補って解決される
    #[tokio::test]
    async fn resolve_socket_addr_defaults_ipv4_port_to_443() {
        let addr = resolve_socket_addr("127.0.0.1")
            .await
            .expect("解決に成功すること");
        assert_eq!(addr.to_string(), "127.0.0.1:443");
    }

    /// IP リテラルの authority は明示ポートを維持して解決される
    #[tokio::test]
    async fn resolve_socket_addr_keeps_explicit_port() {
        let addr = resolve_socket_addr("127.0.0.1:4443")
            .await
            .expect("解決に成功すること");
        assert_eq!(addr.to_string(), "127.0.0.1:4443");
    }

    /// IPv6 リテラルの authority は既定ポートを補って解決される
    #[tokio::test]
    async fn resolve_socket_addr_defaults_ipv6_port_to_443() {
        let addr = resolve_socket_addr("[::1]")
            .await
            .expect("解決に成功すること");
        assert_eq!(addr.to_string(), "[::1]:443");
    }

    /// ホスト名の authority は名前解決してポートを補う
    ///
    /// `localhost` は名前解決を必要とするため、`lookup_host` を通ることを固定する。
    /// 解決先は環境によって IPv4 / IPv6 のどちらかになるため、ファミリは検証しない。
    #[tokio::test]
    async fn resolve_socket_addr_resolves_hostname() {
        let addr = resolve_socket_addr("localhost")
            .await
            .expect("解決に成功すること");
        assert_eq!(addr.port(), 443);
        assert!(
            addr.ip().is_loopback(),
            "localhost はループバックに解決されること"
        );
    }

    /// 不正な authority は解決せず、QUIC 由来でないエラーになる
    ///
    /// 利用者向けの表示に `QUIC:` を付けない設計方針を固定する。
    #[tokio::test]
    async fn resolve_socket_addr_rejects_invalid_authority() {
        for authority in ["::1", "127.0.0.1:abc"] {
            let err = resolve_socket_addr(authority)
                .await
                .expect_err("エラーになること");
            assert!(
                matches!(err, TransportError::InvalidAuthority(_)),
                "authority の解釈失敗として返ること: {err}"
            );
            assert!(
                !err.to_string().contains("QUIC"),
                "QUIC 由来の表示にならないこと: {err}"
            );
        }
    }

    /// トランスポート種別に依存しない失敗の表示に `QUIC:` を付けない
    #[test]
    fn transport_error_display_omits_quic_prefix() {
        // 実際に authority_parts / resolve_failed が生成するメッセージで確認する
        let invalid = authority_parts(":4443").expect_err("エラーになること");
        assert_eq!(
            invalid.to_string(),
            "invalid server address ':4443': empty host"
        );

        let resolve = resolve_failed("relay.invalid", "no address");
        assert!(
            matches!(resolve, TransportError::ResolutionFailed(_)),
            "名前解決の失敗として返ること: {resolve}"
        );
        assert_eq!(
            resolve.to_string(),
            "failed to resolve 'relay.invalid': no address"
        );
    }
}
