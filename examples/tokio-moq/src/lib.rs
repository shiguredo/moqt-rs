//! examples 共有トランスポート crate
//!
//! `moq-publisher` / `moq-subscriber` の両バイナリが共通で利用する
//! QUIC / WebTransport over HTTP/3 / WebTransport over HTTP/2 のトランスポート層を提供する。
//! URL の scheme は `moqt://` に統一し、トランスポートは [`Transport`] (`--transport`) で選ぶ。
//!
//! publisher は能動的に送信ストリームを open する側、subscriber は受動的に
//! 受信ストリームを accept する側という非対称があるため、両者の union API を
//! ここに置き、各バイナリは必要なメソッドだけを呼び出す。

use std::net::SocketAddr;

use base64ct::{Base64, Base64Unpadded, Base64Url, Base64UrlUnpadded, Encoding};

use shiguredo_moqt::msf::uri::parse_msf_fragment;

use crate::error::TransportError;

/// draft-ietf-moq-transport-21 §9.1 (SETUP) に基づく基本 Setup Options を構築する
///
/// path (0x01) と authority (0x05) は QUIC 接続時にのみ含まれ、
/// WebTransport 使用時は draft-ietf-moq-transport-21 §9.1.1 (AUTHORITY) /
/// §9.1.2 (PATH) により MUST NOT のため呼び出し側で None を渡す。
/// §9.1.5 (MOQT_IMPLEMENTATION) の `impl_name` (option type 0x07) は常に含まれる。
///
/// `c4m_tokens` は URL の MSF fragment から取り出した C4M 認可トークンで、
/// 各要素を AUTHORIZATION_TOKEN (option type 0x03) として含める。空の場合は
/// AUTHORIZATION_TOKEN を含めない。
pub fn build_setup_options(
    path: Option<&str>,
    authority: Option<&str>,
    impl_name: &str,
    c4m_tokens: &[Vec<u8>],
) -> shiguredo_moqt::parameter::SetupOptions {
    use shiguredo_moqt::{
        c4m::cat::MOQT_AUTH_TOKEN_TYPE_CAT, message_parameter::AuthorizationToken,
        parameter::SETUP_OPTION_AUTHORITY, parameter::SETUP_OPTION_AUTHORIZATION_TOKEN,
        parameter::SETUP_OPTION_MOQT_IMPLEMENTATION, parameter::SETUP_OPTION_PATH,
        parameter::SetupOption, parameter::SetupOptionValue, parameter::SetupOptions,
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
    for token_value in c4m_tokens {
        // draft-ietf-moq-msf-01 §11.1.1 (Reserved fragment parameters) の c4m は
        // C4M の認可トークンであり、SETUP の AUTHORIZATION_TOKEN として送る。
        // Token Type は CAT (0x01) を使う (draft-ietf-moq-c4m-01 §7.1.1)。
        // alias の登録・参照を行わないため USE_VALUE (Alias Type 0x3) を使う
        // (draft-ietf-moq-transport-21 §8.9 (Authorization Token Compression))。
        options.push(SetupOption {
            option_type: SETUP_OPTION_AUTHORIZATION_TOKEN,
            value: SetupOptionValue::AuthorizationToken(AuthorizationToken::UseValue {
                token_type: MOQT_AUTH_TOKEN_TYPE_CAT,
                token_value: token_value.clone(),
            }),
        });
    }
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
pub mod webtransport_h2;
pub mod webtransport_h3;

// 型はサブモジュールパス (`error::...` / `transport::...` / `webtransport_h3::...` / `quic::...`) 経由で
// 参照する。最上位再エクスポートは設けない。

/// トランスポート種別
///
/// URL の scheme は `moqt://` に統一されており、接続経路はこの値で選ぶ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// QUIC 直接接続 (ALPN `moqt-21`)
    Quic,
    /// WebTransport over HTTP/3 (QUIC 上の ALPN `h3`)
    WtH3,
    /// WebTransport over HTTP/2 (TCP+TLS 上の ALPN `h2`)
    WtH2,
}

impl Transport {
    /// `--transport` の値からトランスポート種別を解決する
    ///
    /// # Errors
    ///
    /// `quic` / `wt-h3` / `wt-h2` 以外の値はエラーになる。
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "quic" => Ok(Self::Quic),
            "wt-h3" => Ok(Self::WtH3),
            "wt-h2" => Ok(Self::WtH2),
            other => Err(format!(
                "unknown transport '{other}' (expected quic, wt-h3, or wt-h2)"
            )),
        }
    }
}

/// moqt URI の fragment (`<type>:<value>`)
///
/// draft-ietf-moq-transport-21 §6.1.1 (Fragment Identifiers) は fragment をサーバーへ
/// 送信せず、クライアントが MOQT セッション確立後にローカルで処理すると定める。
/// `:path` / PATH option / SNI には渡さない。
///
/// `type` は [`parse_url`] が §6.1.1 の文字種の MUST に一致するかを検証する。
/// §16.2 (Media Type Registration) が application/moqt の fragment を §6.1.1 に従わせるため、
/// `moqt://` の fragment を同じ規則で検証する。§16.3 の "MOQT URI Fragment Types" registry は
/// 初期状態で空で、登録済みの type かどうかは検証しない。
/// `type` が `msf` のときだけ、[`parse_url`] が value を
/// `shiguredo_moqt::msf::uri::parse_msf_fragment` で MSF fragment として検証し、
/// 予約パラメータ `c4m` の認可トークンを取り出す ([`ServerUrl::c4m_tokens`])。
/// 他の `type` の value は example が解釈しないため、構文 (§6.1.1) の検証だけを行う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoqtFragment {
    /// fragment type (`<type>` 部分)。ASCII 小文字 / 数字 / ハイフンに限る
    pub fragment_type: String,
    /// fragment value (`<value>` 部分)。意味は fragment type を登録した仕様が定める
    ///
    /// percent-encoding された生の文字列を保持する。`#` は RFC 3986 §3.5 の
    /// `fragment = *( pchar / "/" / "?" )` に含まれないため `%23` でなければならない。
    pub value: String,
}

/// パース済みの接続先情報
///
/// 接続経路 (QUIC / WebTransport over HTTP/3 / WebTransport over HTTP/2) は URL ではなく
/// [`Transport`] (`--transport`) で選ぶため、ここには持たない。
#[derive(Debug, Clone)]
pub struct ServerUrl {
    /// ホスト名または IP アドレス (ポートを含む場合がある)
    ///
    /// URL でポートを省略した場合はポートを含まない。接続先の決定では
    /// 既定ポート 443 を補って解釈する。
    pub authority: String,
    /// パス部分 (query を含み、fragment を含まない)
    pub path: String,
    /// MOQT の fragment (`#` 以降)
    ///
    /// draft-ietf-moq-transport-21 §6.1.1 (Fragment Identifiers) によりサーバーへは
    /// 送信しない。接続経路 (`:path` / PATH option / SNI) では参照しない。
    /// `msf` fragment の解釈結果は [`ServerUrl::c4m_tokens`] に取り出している。
    pub fragment: Option<MoqtFragment>,
    /// MSF fragment の `c4m` パラメータから取り出した C4M 認可トークン (出現順)
    ///
    /// draft-ietf-moq-msf-01 §11.1.1 (Reserved fragment parameters) は `c4m` を
    /// "A base64 encoded C4M token" と定め、draft-ietf-moq-c4m-01 §2 / §4 は
    /// トークンを URL に埋め込むときの Base64 エンコードを定める。
    /// Base64 をデコードしたバイト列が MOQT の AUTHORIZATION_TOKEN
    /// (Token Type CAT) の Token Value になる (draft-ietf-moq-c4m-01 §7.1.1)。
    ///
    /// `msf` 以外の fragment と `c4m` パラメータ無しの `msf` fragment では空。
    /// 同一のトークンが複数回現れた場合は 1 つに畳む
    /// (draft-ietf-moq-transport-21 §8.9 (Authorization Token Compression) は
    /// alias 解決後の (Token Type, Token Value) の重複を拒否するため)。
    pub c4m_tokens: Vec<Vec<u8>>,
}

/// URL をパースして authority ・ path ・ fragment ・ C4M 認可トークンを取得する
///
/// scheme は `moqt://` だけを受け付ける (`MOQT://` も受理する。RFC 3986 §3.1 に従い
/// scheme は大文字小文字を区別しない)。接続経路は URL では選ばず [`Transport`] で選ぶ。
/// 正規化するのは scheme だけで、authority / path / query / fragment は入力の文字列を
/// そのまま保持する (RFC 3986 §6.2.2.1 は scheme と host を大文字小文字非区別とするが、
/// host を入力のまま使うのは DNS / SNI の非区別性に依存する設計判断である)。
///
/// fragment は `:path` と PATH option には含めない。draft-ietf-moq-transport-21 §6.1.1
/// (Fragment Identifiers) の `<type>:<value>` として解釈し、同節の文字種の MUST に
/// 一致しない場合はエラーにする。同 draft §16.2 (Media Type Registration) は
/// "Fragment identifiers for application/moqt follow the syntax defined in Section 6.1.1."
/// と定める。この仕様は将来 draft 改訂で変更される可能性がある。
///
/// `type` が `msf` の場合は value を MSF fragment (`track-identifier [ "&" parameter-list ]`)
/// として検証し、予約パラメータ `c4m` を Base64 デコードして [`ServerUrl::c4m_tokens`] に
/// 入れる。MSF fragment の形式が不正な場合はエラーにする。
///
/// # Errors
///
/// - `moqt://` 以外の scheme / authority の欠落: `unsupported URL scheme` / `requires authority`
/// - fragment が `<type>:<value>` でない / type の文字種が §6.1.1 に一致しない: `invalid moqt URI fragment`
/// - `msf` fragment が MSF §11.1 の ABNF に一致しない: `invalid MSF fragment`
/// - `c4m` の値が Base64 でない / 空: `invalid c4m parameter`
pub fn parse_url(url: &str) -> Result<ServerUrl, String> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(format!("unsupported URL scheme: {url} (use moqt://)"));
    };
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "moqt" {
        return Err(format!("unsupported URL scheme: {url} (use moqt://)"));
    }
    let (before_fragment, raw_fragment) = split_fragment(rest);
    let (authority, path) = split_authority_path(before_fragment);
    if authority.is_empty() {
        return Err(format!(
            "moqt:// URL requires authority: {url} (e.g. moqt://localhost:4443)"
        ));
    }
    let fragment = match raw_fragment {
        Some(fragment) => Some(parse_moqt_fragment(url, fragment)?),
        None => None,
    };
    // draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation): fragment type
    // "msf" の value は MSF の track-identifier とパラメータ列である。example が
    // 解釈するのは予約パラメータ c4m の認可トークンだけである。
    let c4m_tokens = match &fragment {
        Some(f) if f.fragment_type == "msf" => parse_c4m_tokens(url, &f.value)?,
        _ => Vec::new(),
    };
    Ok(ServerUrl {
        authority,
        path,
        fragment,
        c4m_tokens,
    })
}

/// URL の `#` 以降を fragment として分離する
///
/// 戻り値は fragment を除いた URI と、`#` の直後から末尾までの fragment の生文字列。
/// fragment の中身の解釈は scheme ごとに異なるため [`parse_moqt_fragment`] が行う。
///
/// RFC 3986 §3.5 は `fragment` が URI の末尾までであると定めるため、2 個目以降の `#` も
/// fragment の一部として切り出す。この関数は文字列を分離するだけで検証しない。
fn split_fragment(rest: &str) -> (&str, Option<&str>) {
    match rest.find('#') {
        Some(hash) => (&rest[..hash], Some(&rest[hash + 1..])),
        None => (rest, None),
    }
}

/// moqt URI の fragment を `<type>:<value>` として解釈する
///
/// draft-ietf-moq-transport-21 §6.1.1 (Fragment Identifiers) は moqt URI の fragment を
/// 登録済みの fragment type と `:` で始めると MUST で定め、type の文字種を
/// "Fragment type identifiers MUST consist of ASCII lowercase letters, digits, and hyphens (a-z, 0-9, -)."
/// と定める (§6.1.1 に ABNF は無く、本文の MUST で示される)。`:` が無い fragment と
/// 空の type と文字種に一致しない type は拒否する。
/// value は type を登録した仕様が意味を定めるため、最初の `:` 以降をそのまま保持する。
/// `#` は fragment の中に現れ得ない (RFC 3986 §3.5) ため、2 個目以降の `#` は拒否する。
///
/// この仕様は将来 draft 改訂で変更される可能性がある。
fn parse_moqt_fragment(url: &str, fragment: &str) -> Result<MoqtFragment, String> {
    if fragment.contains('#') {
        return Err(format!(
            "invalid moqt URI fragment: {url} ('#' inside a fragment must be percent-encoded)"
        ));
    }
    let Some((fragment_type, value)) = fragment.split_once(':') else {
        return Err(format!(
            "invalid moqt URI fragment: {url} (must be '<type>:<value>')"
        ));
    };
    if !is_fragment_type(fragment_type) {
        return Err(format!(
            "invalid moqt URI fragment type: {url} (must be non-empty and consist of ASCII lowercase letters, digits, and hyphens)"
        ));
    }
    Ok(MoqtFragment {
        fragment_type: fragment_type.to_string(),
        value: value.to_string(),
    })
}

/// fragment type が draft-ietf-moq-transport-21 §6.1.1 の文字種の MUST に一致するかを返す
fn is_fragment_type(fragment_type: &str) -> bool {
    !fragment_type.is_empty()
        && fragment_type
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `msf` fragment の value から `c4m` パラメータの認可トークンを取り出す
///
/// draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation) の
/// `msf-fragment-value = track-identifier [ "&" parameter-list ]` に従い、value 全体を
/// [`parse_msf_fragment`] で検証してから予約パラメータ `c4m` を読む。MSF fragment の
/// 形式が不正な場合は [`parse_url`] のエラーとして扱う (example が解釈すると宣言した
/// fragment type の値は、黙って無視せず仕様どおりに扱う)。
///
/// 各 `c4m` の値は RFC 3986 §2.1 (Percent-Encoding) の `%XX` をデコードしてから
/// Base64 としてデコードする。得られたバイト列が MOQT の AUTHORIZATION_TOKEN
/// (Token Type CAT) の Token Value になる (draft-ietf-moq-c4m-01 §7.1.1)。
///
/// # Errors
///
/// - MSF fragment が §11.1 の ABNF に一致しない: `invalid MSF fragment`
/// - `c4m` の値が Base64 でない / 空: `invalid c4m parameter`
fn parse_c4m_tokens(url: &str, fragment_value: &str) -> Result<Vec<Vec<u8>>, String> {
    // parse_msf_fragment は `msf:` 前置を含む fragment 全体を受け取る
    let fragment = parse_msf_fragment(&format!("msf:{fragment_value}"))
        .map_err(|e| format!("invalid MSF fragment in {url}: {e}"))?;
    let mut tokens: Vec<Vec<u8>> = Vec::new();
    for text in fragment.c4m_tokens() {
        let token = decode_c4m_token(url, text)?;
        // draft-ietf-moq-transport-21 §8.9 (Authorization Token Compression):
        // alias 解決後の (Token Type, Token Value) の重複は送れないため、同一トークンは畳む
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    Ok(tokens)
}

/// `c4m` パラメータの値を認可トークンのバイト列へデコードする
///
/// draft-ietf-moq-msf-01 §11.1.1 (Reserved fragment parameters) は `c4m` を
/// "A base64 encoded C4M token" と定める。RFC 4648 は §4 の Base64 と §5 の
/// base64url を定義しており、draft-ietf-moq-c4m-01 §4 はどちらを使うかを指定しないため、
/// 両方とパディングの有無を受理する (library の `CatToken::decode` が URL 埋め込みの
/// Base64 を扱う規則と同じである)。
///
/// # Errors
///
/// - Base64 としてデコードできない / デコード結果が空: `invalid c4m parameter`
fn decode_c4m_token(url: &str, text: &str) -> Result<Vec<u8>, String> {
    let invalid = || format!("invalid c4m parameter in {url}: not a non-empty Base64 token");
    // RFC 3986 §2.1 (Percent-Encoding): MSF のパラメータ列は library が生の文字列で
    // 返すため、Base64 アルファベット外の `%XX` をここでデータバイトへ戻す。
    // Base64 のテキストは ASCII のみのため、UTF-8 でなければ Base64 として不正である。
    let decoded_text = String::from_utf8(percent_decode(text)).map_err(|_| invalid())?;
    for bytes in [
        Base64UrlUnpadded::decode_vec(&decoded_text),
        Base64Url::decode_vec(&decoded_text),
        Base64::decode_vec(&decoded_text),
        Base64Unpadded::decode_vec(&decoded_text),
    ] {
        if let Ok(token) = bytes
            && !token.is_empty()
        {
            return Ok(token);
        }
    }
    Err(invalid())
}

/// RFC 3986 §2.1 (Percent-Encoding) の `%XX` をデータバイトへデコードする
///
/// 入力は [`parse_msf_fragment`] が `pct-encoded` (`%` HEXDIG HEXDIG) として検証済みで
/// あるため、`%` の後は必ず hex 2 桁である。hex は大文字小文字を区別しない。
fn percent_decode(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            decoded.push((hex_digit(bytes[i + 1]) << 4) | hex_digit(bytes[i + 2]));
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    decoded
}

/// 1 文字の hex を値へ変換する
fn hex_digit(byte: u8) -> u8 {
    // parse_msf_fragment が `pct-encoded` の hex 2 桁を検証済みのため必ず成功する
    (byte as char)
        .to_digit(16)
        .expect("hex digit validated by parse_msf_fragment") as u8
}

/// authority と path を分離する
///
/// path には query (`?` 以降) を含める。draft-ietf-moq-transport-21 §9.1.2 (PATH) は
/// query が存在する場合に `?` と query を PATH option へ連結することを MUST とするため。
///
/// RFC 3986 §3.2 は authority が `#` でも終端すると定めるが、この関数は `#` を区切りに
/// 含めない。呼び出し元の [`parse_url`] が先に fragment を分離してから渡す。
/// path が空の場合は `/` を補う。RFC 3986 §6.2.3 は
/// "a URI that uses the generic syntax for authority with an empty path should be normalized to
/// a path of \"/\"" と定めるため、query のみの場合も `/` を補う。
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
/// QUIC 直結と WebTransport (HTTP/3 / HTTP/2) のどの経路でも同じ 443 を既定値として使う。
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

    use shiguredo_moqt::c4m::cat::MOQT_AUTH_TOKEN_TYPE_CAT;
    use shiguredo_moqt::message_parameter::AuthorizationToken;
    use shiguredo_moqt::parameter::SetupOptions;

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

    /// moqt:// URL は query が path に残る
    #[test]
    fn parse_url_parses_moqt_scheme_with_query() {
        let url = parse_url("moqt://127.0.0.1:4443/path?x=1").expect("URL のパースに成功すること");
        assert_eq!(url.authority, "127.0.0.1:4443");
        assert_eq!(url.path, "/path?x=1");
    }

    /// `moqt://` 以外の scheme はエラーになる (接続経路は --transport で選ぶ)
    #[test]
    fn parse_url_rejects_unknown_scheme() {
        for url in [
            "ftp://127.0.0.1:4443",
            "https://example.com:443/foo?bar=baz",
        ] {
            assert_eq!(
                parse_url(url).expect_err("未対応 scheme がエラーになること"),
                format!("unsupported URL scheme: {url} (use moqt://)")
            );
        }
    }

    /// `://` が無い URL も未対応 scheme としてエラーになる
    #[test]
    fn parse_url_rejects_url_without_scheme_delimiter() {
        assert_eq!(
            parse_url("example.com/app").expect_err("scheme が無い URL がエラーになること"),
            "unsupported URL scheme: example.com/app (use moqt://)"
        );
    }

    /// `--transport` の値からトランスポート種別を解決する
    #[test]
    fn transport_parses_supported_values() {
        assert_eq!(
            Transport::parse("quic").expect("quic は受理されること"),
            Transport::Quic
        );
        assert_eq!(
            Transport::parse("wt-h3").expect("wt-h3 は受理されること"),
            Transport::WtH3
        );
        assert_eq!(
            Transport::parse("wt-h2").expect("wt-h2 は受理されること"),
            Transport::WtH2
        );
    }

    /// 未対応の transport 名・表記ゆれ・空文字はエラーにする
    #[test]
    fn transport_rejects_unsupported_values() {
        for value in ["wt_h3", "WT-H3", "h3", ""] {
            let err = Transport::parse(value).expect_err("受理しないこと");
            assert!(
                err.contains("unknown transport"),
                "エラーに理由が含まれること: {err}"
            );
        }
    }

    /// fragment は path に含めない (draft-21 §6.1.1)
    #[test]
    fn parse_url_excludes_fragment_from_path() {
        let url =
            parse_url("moqt://example.com/app#type:value").expect("URL のパースに成功すること");
        assert_eq!(url.authority, "example.com");
        assert_eq!(url.path, "/app");
    }

    /// query の後ろの fragment も path に含めない (draft-21 §9.1.2 PATH)
    #[test]
    fn parse_url_excludes_fragment_from_path_with_query() {
        let url =
            parse_url("moqt://example.com/app?x=1#type:value").expect("URL のパースに成功すること");
        assert_eq!(url.authority, "example.com");
        assert_eq!(url.path, "/app?x=1");
    }

    /// fragment が付いても authority から SNI 用の host を取り出せる
    #[test]
    fn parse_url_keeps_fragment_free_authority_for_sni() {
        let url = parse_url("moqt://example.com:4443/app#type:value")
            .expect("URL のパースに成功すること");
        assert_eq!(url.authority, "example.com:4443");
        assert_eq!(host_from_authority(&url.authority), "example.com");
    }

    /// IPv6 リテラルの authority と fragment を同時に扱える
    #[test]
    fn parse_url_handles_ipv6_literal_with_fragment() {
        let url =
            parse_url("moqt://[::1]:4443/path#type:value").expect("URL のパースに成功すること");
        assert_eq!(url.authority, "[::1]:4443");
        assert_eq!(url.path, "/path");
        assert_eq!(host_from_authority(&url.authority), "::1");
        let fragment = url.fragment.expect("fragment が保持されること");
        assert_eq!(fragment.value, "value");
    }

    /// 大文字 scheme でも fragment を分離できる (RFC 3986 §3.1)
    #[test]
    fn parse_url_splits_fragment_with_uppercase_scheme() {
        let url =
            parse_url("MOQT://example.com/app#type:value").expect("URL のパースに成功すること");
        assert_eq!(url.path, "/app");
        let fragment = url.fragment.expect("fragment が保持されること");
        assert_eq!(fragment.fragment_type, "type");
    }

    /// authority 直後の fragment は authority を汚染せず path は `/` になる
    #[test]
    fn parse_url_keeps_authority_without_fragment() {
        let url = parse_url("moqt://example.com#type:value").expect("URL のパースに成功すること");
        assert_eq!(url.authority, "example.com");
        assert_eq!(url.path, "/");
    }

    /// fragment の type と value を分離して保持する
    #[test]
    fn parse_url_splits_fragment_type_and_value() {
        let url =
            parse_url("moqt://example.com/app#type:value").expect("URL のパースに成功すること");
        let fragment = url.fragment.expect("fragment が保持されること");
        assert_eq!(fragment.fragment_type, "type");
        assert_eq!(fragment.value, "value");
    }

    /// fragment の value は最初の `:` 以降をすべて含む
    #[test]
    fn parse_url_keeps_colons_in_fragment_value() {
        let url =
            parse_url("moqt://example.com/app#type:va:lue").expect("URL のパースに成功すること");
        let fragment = url.fragment.expect("fragment が保持されること");
        assert_eq!(fragment.fragment_type, "type");
        assert_eq!(fragment.value, "va:lue");
    }

    /// fragment が無い URL は fragment を保持しない
    #[test]
    fn parse_url_has_no_fragment_without_hash() {
        let url = parse_url("moqt://example.com/app").expect("URL のパースに成功すること");
        assert_eq!(url.fragment, None);
    }

    /// authority が空の URL はエラーになり、入力 URL がメッセージに含まれる
    #[test]
    fn parse_url_rejects_empty_authority() {
        assert_eq!(
            parse_url("moqt://").expect_err("authority が無い moqt URL がエラーになること"),
            "moqt:// URL requires authority: moqt:// (e.g. moqt://localhost:4443)"
        );
        // fragment の検証より前に authority の有無を判定する。
        // fragment 自体が不正でも authority のエラーになることを固定する
        for url in ["moqt://?x=1#t:v", "moqt://?x=1#bad", "moqt://#bad"] {
            assert_eq!(
                parse_url(url).expect_err("authority が無い moqt URL がエラーになること"),
                format!("moqt:// URL requires authority: {url} (e.g. moqt://localhost:4443)")
            );
        }
    }

    /// `:` を含まない fragment はエラーになる (draft-21 §6.1.1)
    #[test]
    fn parse_url_rejects_fragment_without_colon() {
        assert_eq!(
            parse_url("moqt://example.com/app#typevalue")
                .expect_err("fragment に `:` が無い URL がエラーになること"),
            "invalid moqt URI fragment: moqt://example.com/app#typevalue (must be '<type>:<value>')"
        );
    }

    /// fragment のみ (`#` だけ) は type が無いためエラーになる (draft-21 §6.1.1)
    #[test]
    fn parse_url_rejects_empty_fragment() {
        assert_eq!(
            parse_url("moqt://example.com/app#")
                .expect_err("fragment のみの URL がエラーになること"),
            "invalid moqt URI fragment: moqt://example.com/app# (must be '<type>:<value>')"
        );
    }

    /// §6.1.1 の文字種の MUST に一致しない fragment type はエラーになる
    #[test]
    fn parse_url_rejects_fragment_type_outside_charset() {
        assert_eq!(
            parse_url("moqt://example.com/app#Type:value")
                .expect_err("大文字の fragment type がエラーになること"),
            "invalid moqt URI fragment type: moqt://example.com/app#Type:value (must be non-empty and consist of ASCII lowercase letters, digits, and hyphens)"
        );
        for url in [
            // 空の type は registered identifier にならない
            "moqt://example.com/app#:value",
            // アンダースコアは文字種 (a-z / 0-9 / -) に一致しない
            "moqt://example.com/app#ty_pe:value",
            // `.` は scheme には使えるが fragment type には使えない
            "moqt://example.com/app#ty.pe:value",
        ] {
            assert!(parse_url(url).is_err(), "{url} がエラーになること");
        }
    }

    /// §6.1.1 の文字種の MUST が許す数字とハイフンの type を受理する
    #[test]
    fn parse_url_accepts_digits_and_hyphens_in_fragment_type() {
        for (url, expected_type) in [
            ("moqt://example.com/app#a-b:value", "a-b"),
            ("moqt://example.com/app#1:value", "1"),
            ("moqt://example.com/app#a1-b2:value", "a1-b2"),
        ] {
            let parsed = parse_url(url).unwrap_or_else(|e| panic!("{url} のパースに失敗した: {e}"));
            let fragment = parsed.fragment.expect("fragment が保持されること");
            assert_eq!(fragment.fragment_type, expected_type, "{url} の type");
        }
    }

    /// fragment 内の 2 個目の `#` はエラーになる (RFC 3986 §3.5)
    #[test]
    fn parse_url_rejects_second_hash_in_fragment() {
        assert_eq!(
            parse_url("moqt://example.com/app#a:b#c:d")
                .expect_err("fragment 内の 2 個目の `#` がエラーになること"),
            "invalid moqt URI fragment: moqt://example.com/app#a:b#c:d ('#' inside a fragment must be percent-encoded)"
        );
        assert!(parse_url("moqt://example.com/app#t:v#").is_err());
    }

    /// value が空でも type があれば fragment として受理する
    #[test]
    fn parse_url_accepts_empty_fragment_value() {
        let url = parse_url("moqt://example.com/app#type:").expect("URL のパースに成功すること");
        let fragment = url.fragment.expect("fragment が保持されること");
        assert_eq!(fragment.fragment_type, "type");
        assert_eq!(fragment.value, "");
    }

    /// 大文字 scheme を小文字 scheme と同じに解釈する (RFC 3986 §3.1)
    #[test]
    fn parse_url_accepts_uppercase_scheme() {
        let url = parse_url("MOQT://127.0.0.1:4443/path").expect("URL のパースに成功すること");
        assert_eq!(url.authority, "127.0.0.1:4443");
        assert_eq!(url.path, "/path");
    }

    /// scheme 以外の成分は大文字小文字を保持する (RFC 3986 §6.2.2.1 は host も非区別とするが入力を保持する)
    #[test]
    fn parse_url_keeps_case_of_non_scheme_components() {
        let url = parse_url("MOQT://EXAMPLE.com:4443/Path?X=Y#type:VALue")
            .expect("URL のパースに成功すること");
        assert_eq!(url.authority, "EXAMPLE.com:4443");
        assert_eq!(url.path, "/Path?X=Y");
        let fragment = url.fragment.expect("fragment が保持されること");
        assert_eq!(fragment.value, "VALue");
    }

    /// scheme の区切りは最初の `://` で判定し、query 内の `://` は scheme とみなさない
    #[test]
    fn parse_url_keeps_scheme_delimiter_in_query() {
        let url = parse_url("moqt://example.com/app?x=a://b").expect("URL のパースに成功すること");
        assert_eq!(url.authority, "example.com");
        assert_eq!(url.path, "/app?x=a://b");
    }

    /// fragment の有無で authority と path が変わらない (fragment は経路に影響しない)
    #[test]
    fn parse_url_fragment_does_not_change_authority_and_path() {
        let with_fragment =
            parse_url("moqt://example.com/app?x=1#type:value").expect("URL のパースに成功すること");
        let without_fragment =
            parse_url("moqt://example.com/app?x=1").expect("URL のパースに成功すること");
        assert_eq!(with_fragment.authority, without_fragment.authority);
        assert_eq!(with_fragment.path, without_fragment.path);
    }

    /// SETUP の PATH option に fragment を含めない (draft-21 §9.1.2)
    #[test]
    fn setup_options_path_excludes_fragment() {
        let url =
            parse_url("moqt://example.com/app#type:value").expect("URL のパースに成功すること");
        let options = build_setup_options(Some(&url.path), Some(&url.authority), "impl", &[]);
        // PATH option の値そのものを確認する
        assert_eq!(options.path(), Some(b"/app".as_slice()));
        // エンコード全体にも `#` が現れないことを確認する
        let mut encoded = Vec::new();
        options
            .encode(&mut encoded)
            .expect("SETUP option のエンコードに成功すること");
        assert!(
            !encoded.contains(&b'#'),
            "SETUP のどこにも fragment を含めないこと: {encoded:?}"
        );
    }

    /// `msf` fragment の c4m パラメータを Base64 デコードして保持する
    #[test]
    fn parse_url_extracts_c4m_token_from_msf_fragment() {
        let token = Base64::encode_string(&[0x01, 0x02, 0x03]);
        let url = parse_url(&format!(
            "moqt://example.com/app#msf:kaki--video&c4m={token}"
        ))
        .expect("URL のパースに成功すること");
        assert_eq!(url.c4m_tokens, vec![vec![0x01, 0x02, 0x03]]);
        // track-identifier は example が使わないが、MSF fragment として保持される
        let fragment = url.fragment.expect("fragment が保持されること");
        assert_eq!(fragment.fragment_type, "msf");
        assert_eq!(fragment.value, format!("kaki--video&c4m={token}"));
    }

    /// パディング無しの標準 Base64 と base64url も c4m として受理する
    #[test]
    fn parse_url_accepts_unpadded_and_url_safe_base64_for_c4m() {
        let bytes = [0xfb, 0xff];
        for text in [
            Base64Unpadded::encode_string(&bytes),
            Base64UrlUnpadded::encode_string(&bytes),
        ] {
            let url = parse_url(&format!(
                "moqt://example.com/app#msf:kaki--video&c4m={text}"
            ))
            .unwrap_or_else(|e| panic!("{text} のパースに失敗した: {e}"));
            assert_eq!(url.c4m_tokens, vec![bytes.to_vec()], "{text}");
        }
    }

    /// percent-encoding された c4m パラメータもデコードする (RFC 3986 §2.1)
    #[test]
    fn parse_url_decodes_percent_encoded_c4m_parameter() {
        // `%2B%2F8` は標準 Base64 の `+/8` (パディング無し) の percent-encoding
        let url = parse_url("moqt://example.com/app#msf:kaki--video&c4m=%2B%2F8")
            .expect("URL のパースに成功すること");
        assert_eq!(url.c4m_tokens, vec![vec![0xfb, 0xff]]);
    }

    /// c4m パラメータは出現順を保ち、同一トークンは 1 つに畳む
    #[test]
    fn parse_url_keeps_c4m_tokens_in_order_and_deduplicates() {
        let first = Base64UrlUnpadded::encode_string(&[0x01]);
        let second = Base64UrlUnpadded::encode_string(&[0x02]);
        let url = parse_url(&format!(
            "moqt://example.com/app#msf:kaki--video&c4m={first}&c4m={second}&c4m={first}"
        ))
        .expect("URL のパースに成功すること");
        assert_eq!(url.c4m_tokens, vec![vec![0x01], vec![0x02]]);
    }

    /// c4m パラメータが無い msf fragment でもパースできる
    #[test]
    fn parse_url_accepts_msf_fragment_without_c4m() {
        let url = parse_url("moqt://example.com/app#msf:kaki--video&connection=q")
            .expect("URL のパースに成功すること");
        assert!(
            url.c4m_tokens.is_empty(),
            "c4m 無しではトークンを保持しないこと"
        );
        let fragment = url.fragment.expect("fragment が保持されること");
        assert_eq!(fragment.fragment_type, "msf");
    }

    /// msf 以外の fragment type は c4m らしき値があっても解釈しない
    #[test]
    fn parse_url_ignores_c4m_outside_msf_fragment() {
        let url =
            parse_url("moqt://example.com/app#type:c4m=AQID").expect("URL のパースに成功すること");
        assert!(url.c4m_tokens.is_empty());
    }

    /// msf fragment が MSF §11.1 の ABNF に一致しない場合はエラーになる
    #[test]
    fn parse_url_rejects_invalid_msf_fragment() {
        for value in [
            // track-identifier が無い
            "",
            // パラメータに `=` が無い
            "kaki--video&c4m",
            // `&` を含む値はパラメータ区切りとして解釈され、`=` の無いパラメータになる
            "kaki--video&c4m=AQ&ID",
        ] {
            let url = format!("moqt://example.com/app#msf:{value}");
            let err = parse_url(&url).expect_err("エラーになること");
            assert!(err.contains("invalid MSF fragment"), "{url}: {err}");
        }
    }

    /// c4m の値が Base64 でない、または空の場合はエラーになる
    #[test]
    fn parse_url_rejects_invalid_c4m_token() {
        for text in ["!!!", ""] {
            let url = format!("moqt://example.com/app#msf:kaki--video&c4m={text}");
            let err = parse_url(&url).expect_err("エラーになること");
            assert!(err.contains("invalid c4m parameter"), "{url}: {err}");
        }
    }

    /// SETUP に c4m トークンを AUTHORIZATION_TOKEN (Token Type CAT) として含める
    #[test]
    fn setup_options_include_c4m_authorization_tokens() {
        let url = parse_url("moqt://example.com/app#msf:kaki--video&c4m=AQID")
            .expect("URL のパースに成功すること");
        let options = build_setup_options(
            Some(&url.path),
            Some(&url.authority),
            "impl",
            &url.c4m_tokens,
        );
        assert_eq!(
            options.authorization_tokens(),
            vec![&AuthorizationToken::UseValue {
                token_type: MOQT_AUTH_TOKEN_TYPE_CAT,
                token_value: vec![0x01, 0x02, 0x03],
            }]
        );
        // PATH option の値そのものを確認する
        assert_eq!(options.path(), Some(b"/app".as_slice()));

        // ワイヤ形式でも USE_VALUE / CAT として往復することを確認する
        let mut encoded = Vec::new();
        options
            .encode(&mut encoded)
            .expect("SETUP option のエンコードに成功すること");
        let (decoded, _) =
            SetupOptions::decode(&encoded).expect("SETUP option のデコードに成功すること");
        assert_eq!(
            decoded.authorization_tokens(),
            vec![&AuthorizationToken::UseValue {
                token_type: MOQT_AUTH_TOKEN_TYPE_CAT,
                token_value: vec![0x01, 0x02, 0x03],
            }]
        );
    }

    /// c4m トークン無しでは AUTHORIZATION_TOKEN を含めない
    #[test]
    fn setup_options_omit_authorization_token_without_c4m() {
        let options = build_setup_options(Some("/app"), Some("example.com"), "impl", &[]);
        assert!(options.authorization_tokens().is_empty());
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
        // `moqt://[2001:db8::1]/app` の authority
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

        let url = parse_url("moqt://[2001:db8::1]/app").expect("URL のパースに成功すること");
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
