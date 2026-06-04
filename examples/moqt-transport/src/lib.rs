//! examples 共有トランスポート crate
//!
//! `moqt-publisher` / `moqt-subscriber` の両バイナリが共通で利用する
//! QUIC / WebTransport over HTTP/3 のトランスポート層を提供する。
//!
//! publisher は能動的に送信ストリームを open する側、subscriber は受動的に
//! 受信ストリームを accept する側という非対称があるため、両者の union API を
//! ここに置き、各バイナリは必要なメソッドだけを呼び出す。

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
    /// ホスト名または IP アドレス (ポートを含む)
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
            // 閉じ括弧が無い不正な形式はそのまま返す (呼び出し側で接続時にエラーになる)
            None => authority,
        };
    }
    match authority.rfind(':') {
        Some(pos) => &authority[..pos],
        None => authority,
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
}
