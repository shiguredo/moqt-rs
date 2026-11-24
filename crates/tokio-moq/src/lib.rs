//! crates 配下で共有するトランスポート crate
//!
//! `moq-pub` / `moq-sub` の両バイナリが共通で利用する QUIC / WebTransport over HTTP/3 /
//! WebTransport over HTTP/2 / QMux のトランスポート層を提供する。`--transport auto` では
//! 複数の経路を Happy Eyeballs v3 (draft-ietf-happy-happyeyeballs-v3-05) の手順で競争させる。
//! URL の scheme は `moqt://` に統一し、トランスポートは [`Transport`] (`--transport`) で選ぶ。
//! `--transport` を省略した場合は `--url` の MSF fragment の `connection` で決める
//! ([`resolve_transport`])。
//! ネイティブ QUIC と WebTransport over HTTP/3 は tobi-quic を使う。
//! `--url` の `moqt://` URI は [`parse_url`] でパースし、`--namespace` と `--url` の MSF fragment
//! からの Track Namespace の解決は [`resolve_namespace`] が担う。
//!
//! QMux 経路 ([`Transport::Qmux`]) も experimental である。QMux は順序保証された 1 本の
//! バイトストリーム (TLS over TCP) の上で QUIC version 1 のストリームと DATAGRAM を多重化する
//! ため (draft-ietf-quic-qmux-02)、経路での損失は下位の TCP が再送するが、TCP の再送で
//! 遅延が伸びる。
//! `RESET_STREAM_AT` を実装しないため、reset の `reliable_size` は常に `None` になる。
//! QMux 上の MOQT を扱う individual draft
//! (`refs/moq/draft-nandakumar-moq-qmux-moqt-00.txt`) は MOQT draft-16 を前提としており、
//! 本 crate は QUIC version 1 との 1 対 1 の対応を採る。
//!
//! publisher は能動的に送信ストリームを open する側、subscriber は受動的に
//! 受信ストリームを accept する側という非対称があるため、両者の union API を
//! ここに置き、各バイナリは必要なメソッドだけを呼び出す。

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64ct::{Base64, Base64Unpadded, Base64Url, Base64UrlUnpadded, Encoding};

use shiguredo_moqt::c4m::cat::MOQT_AUTH_TOKEN_TYPE_CAT;
use shiguredo_moqt::message::common::TrackNamespace;
use shiguredo_moqt::message_parameter::AuthorizationToken;
use shiguredo_moqt::msf::uri::{MsfConnectionType, MsfFragment, parse_msf_fragment};

use crate::error::TransportError;
use crate::multipath::MultipathOptions;

/// peer が内容を制御できる文字列をログ出力用にサニタイズする
///
/// draft-ietf-moq-transport-23 §15.9 (Logging of Untrusted String Fields): Reason Phrase や
/// MOQT_IMPLEMENTATION は sender 制御のテキストであり、ログや描画の前にサニタイズすることを
/// SHOULD とする ("an endpoint that logs or renders them SHOULD sanitize them first (for example,
/// by escaping bytes outside the printable ASCII range)")。ANSI エスケープシーケンスによる
/// 端末制御・ログ注入を防ぐため、printable ASCII (0x20-0x7E) 以外の文字を `\xNN` /
/// `\u{...}` へエスケープする。タブ (0x09) も printable ASCII ではないためエスケープする。
pub fn sanitize_for_log(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    for c in value.chars() {
        if c.is_ascii() && (0x20..=0x7E).contains(&(c as u32)) {
            sanitized.push(c);
        } else if (c as u32) <= 0xFF {
            use core::fmt::Write;
            write!(sanitized, "\\x{:02X}", c as u32).expect("writing to String cannot fail");
        } else {
            use core::fmt::Write;
            write!(sanitized, "\\u{{{:X}}}", c as u32).expect("writing to String cannot fail");
        }
    }
    sanitized
}

/// draft-ietf-moq-transport-23 §9.1 (SETUP) に基づく基本 Setup Options を構築する
///
/// path (0x01) と authority (0x05) は QUIC 接続時にのみ含まれ、
/// WebTransport 使用時は draft-ietf-moq-transport-23 §9.1.1 (AUTHORITY) /
/// §9.1.2 (PATH) により MUST NOT のため呼び出し側で None を渡す。
/// §9.1.5 (MOQT IMPLEMENTATION) の `impl_name` (option type 0x07) は常に含まれる。
///
/// `cat_tokens` は URL の MSF fragment から取り出した C4M 認可トークンで、
/// 各要素を AUTHORIZATION_TOKEN (option type 0x03) として含める。空の場合は
/// AUTHORIZATION_TOKEN を含めない。この関数はトークンのバイト列を運ぶだけで、
/// デコード・署名検証・認可は行わない。
pub fn build_setup_options(
    path: Option<&str>,
    authority: Option<&str>,
    impl_name: &str,
    cat_tokens: &[Vec<u8>],
) -> shiguredo_moqt::parameter::SetupOptions {
    use shiguredo_moqt::{
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
        // §9.1.5 (MOQT IMPLEMENTATION): 将来 draft 改訂で変更される可能性がある
        option_type: SETUP_OPTION_MOQT_IMPLEMENTATION,
        value: SetupOptionValue::Bytes(impl_name.as_bytes().to_vec()),
    });
    for token_value in cat_tokens {
        // draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters) の cat は
        // C4M の認可トークンであり、SETUP の AUTHORIZATION_TOKEN として送る。
        options.push(SetupOption {
            option_type: SETUP_OPTION_AUTHORIZATION_TOKEN,
            value: SetupOptionValue::AuthorizationToken(c4m_authorization_token(token_value)),
        });
    }
    options
}

/// C4M トークンのバイト列を AUTHORIZATION TOKEN の Token 構造へ包む
///
/// SETUP の AUTHORIZATION_TOKEN (option type 0x03) と、制御メッセージの
/// AUTHORIZATION_TOKEN (param type 0x03) の双方で同じ表現を使う。
/// Token Type は CAT (0x01) を使い (draft-ietf-moq-c4m-02 §8.2.1)、alias の登録・参照を
/// 行わないため USE_VALUE (Alias Type 0x3) を使う
/// (draft-ietf-moq-transport-23 §8.9 (Authorization Token Compression))。
///
/// この関数はトークンのバイト列を運ぶだけで、トークンのデコード、署名 / MAC の検証、
/// クレームや scope の認可、DPoP proof の検証を行わない。これらはトークンを受理する
/// 側 (リレー) またはアプリケーションの責務である。
pub(crate) fn c4m_authorization_token(token_value: &[u8]) -> AuthorizationToken {
    AuthorizationToken::UseValue {
        token_type: MOQT_AUTH_TOKEN_TYPE_CAT,
        token_value: token_value.to_vec(),
    }
}

/// MOQT のプロトコル識別子 (draft-ietf-moq-transport-23 §6.2 (Session establishment))
///
/// QUIC の ALPN と WebTransport の `WT-Available-Protocols` の双方で使う。draft の ALPN は
/// `moqt-` に draft 番号を付けた値であり、draft-23 では `moqt-23` である。識別子が一致しない
/// 相手とは交渉が成立しないため、draft-22 の識別子 (`moqt-22`) だけを提示する相手を
/// draft-23 として受け入れない。
pub const MOQT_PROTOCOL: &str = "moqt-23";

mod connection_race;

pub mod error;
pub mod metrics;
pub mod moqt_client;
pub mod multipath;
pub mod qmux;
pub mod quic;
pub mod transport;
pub mod webtransport;
pub mod webtransport_h2;
pub mod webtransport_h3;

// TCP + TLS のクライアント設定 (QMux と WebTransport over HTTP/2 が共有する)
pub(crate) mod tls;

// in-process の MOQT peer 試験基盤 (テスト専用)
//
// この crate 自身のテストに加え、`test-support` feature を有効にした他クレートの
// `#[cfg(test)] mod tests` からも参照する。
#[cfg(any(test, feature = "test-support"))]
pub mod peer_harness;

// 型はサブモジュールパス (`error::...` / `transport::...` / `webtransport_h3::...` / `quic::...`) 経由で
// 参照する。最上位再エクスポートは設けない。

/// Multipath QUIC を提供しないプラットフォームで接続を試みたときのエラー
///
/// tobi-quic は Windows では Multipath QUIC を実装していないため、Windows ビルドだけが
/// 利用できない。
#[cfg(windows)]
const MULTIPATH_UNAVAILABLE: &str = "multipath QUIC is not supported on Windows: \
     tobi-quic does not implement Multipath QUIC on Windows";

/// Multipath QUIC (RFC 10069) がこのビルドで利用できない理由
///
/// 利用できる場合は `None` を返す。tobi-quic は Windows 以外のビルドで Multipath QUIC を
/// 提供する。
pub fn multipath_unavailable_reason() -> Option<&'static str> {
    #[cfg(windows)]
    {
        Some(MULTIPATH_UNAVAILABLE)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Multipath QUIC をこのビルドで利用できるか
///
/// 利用できない理由は [`multipath_unavailable_reason`] で取得できる。
pub fn is_multipath_available() -> bool {
    multipath_unavailable_reason().is_none()
}

/// トランスポート種別
///
/// URL の scheme は `moqt://` に統一されており、接続経路はこの値で選ぶ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// QUIC 直接接続 (ALPN `moqt-23`)
    Quic,
    /// WebTransport over HTTP/3 (QUIC 上の ALPN `h3`)
    ///
    /// draft-17 の `SETTINGS_WT_ENABLED` (`0x20b8933b`) だけを広告する。旧 codepoint
    /// (`0x2c7cf000`) だけを広告するサーバーとは共通版が無いため、
    /// `TransportError::InvalidState` で接続に失敗する (draft-ietf-webtrans-http3-17 §7.1)。
    WtH3,
    /// WebTransport over HTTP/2 (TCP+TLS 上の ALPN `h2`)
    ///
    /// reset は `WT_RESET_STREAM` capsule で行うため、relay の capsule 対応に依存する。
    WtH2,
    /// QMux (TCP+TLS 上の ALPN `moqt-23`)
    ///
    /// experimental。QMux は順序保証された 1 本の双方向バイトストリーム (TLS over TCP) の上で
    /// QUIC version 1 のストリーム操作と DATAGRAM 操作を多重化する
    /// (draft-ietf-quic-qmux-02)。UDP が使えない環境でも MOQT を試せるようにするための経路で
    /// あり、QMux 上の MOQT は individual draft
    /// (`refs/moq/draft-nandakumar-moq-qmux-moqt-00.txt`) の範囲である。
    ///
    /// native QUIC との差: 経路での損失は下位の TCP が再送するため起きないが、TCP の再送で
    /// 遅延が伸び、輻輳時は後続のストリームもまとめて遅れる。加えて QMux はアプリへ速やかに
    /// 配送できない DATAGRAM の破棄を許すため (draft-ietf-quic-qmux-02 Section 6)、受信側の
    /// バッファが溢れると欠落しうる。破棄は `dropped_datagrams` として警告ログに出る。
    Qmux,
    /// 複数の経路を Happy Eyeballs v3 で競争させ、最初に確立した経路を使う
    ///
    /// draft-ietf-happy-happyeyeballs-v3-05 の Connection Attempts (§6) に従い、候補を
    /// Connection Attempt Delay でずらして開始し、最初にトランスポートが確立した候補を
    /// 採用する。候補は URL の `msf` fragment の `connection` とビルドで利用できる経路から
    /// 決める。
    ///
    /// - `connection` が無い場合: `quic`、`wt-h3`、`qmux`、`wt-h2` の順に競争する。
    ///   QUIC を TCP より優先し
    ///   (draft-ietf-happy-happyeyeballs-v3-05 §3)、同一の QUIC 系では層の少ない
    ///   native QUIC を先にする
    /// - `connection=q` の場合: `quic` だけを使う。draft-ietf-moq-msf-02 §11.1.1 は `q` に
    ///   Native QUIC を MUST で対応づけるため、QMux (TCP+TLS 上の QUIC 多重化) は候補に
    ///   含めない
    /// - `connection=wt` の場合: `wt-h3`、`wt-h2` の順に競争する
    ///
    /// MOQT の SETUP は最初に確立した候補だけで交換する。SETUP の失敗は別の候補への
    /// フォールバックを起こさない (`connect` の doc を参照する)。
    Auto,
}

impl Transport {
    /// `--transport` の値からトランスポート種別を解決する
    ///
    /// # Errors
    ///
    /// `quic` / `wt-h3` / `wt-h2` / `qmux` / `auto` 以外の値はエラーになる。
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "quic" => Ok(Self::Quic),
            "wt-h3" => Ok(Self::WtH3),
            "wt-h2" => Ok(Self::WtH2),
            "qmux" => Ok(Self::Qmux),
            "auto" => Ok(Self::Auto),
            other => Err(format!(
                "unknown transport '{other}' (expected quic, wt-h3, wt-h2, qmux, or auto)"
            )),
        }
    }

    /// `--transport` の値としての表現を返す ([`Self::parse`] の逆写像)
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quic => "quic",
            Self::WtH3 => "wt-h3",
            Self::WtH2 => "wt-h2",
            Self::Qmux => "qmux",
            Self::Auto => "auto",
        }
    }
}

/// moqt URI の fragment (`<type>:<value>`)
///
/// draft-ietf-moq-transport-23 §6.1.1 (Fragment Identifiers) は fragment をサーバーへ
/// 送信せず、クライアントが MOQT セッション確立後にローカルで処理すると定める。
/// `:path` / PATH option / SNI には渡さない。
///
/// `type` は [`parse_url`] が §6.1.1 の文字種の MUST に一致するかを検証する。
/// §16.2 (Media Type Registration) が application/moqt の fragment を §6.1.1 に従わせるため、
/// `moqt://` の fragment を同じ規則で検証する。§16.3 の "MOQT URI Fragment Types" registry は
/// 初期状態で空で、登録済みの type かどうかは検証しない。
/// `type` が `msf` のときだけ、[`parse_url`] が value を
/// `shiguredo_moqt::msf::uri::parse_msf_fragment` で MSF fragment として検証し、
/// 予約パラメータ `cat` の認可トークンを取り出す ([`ServerUrl::cat_tokens`])。
/// 他の `type` の value は moq-pub / moq-sub が解釈しないため、構文 (§6.1.1) の検証だけを行う。
///
/// value は認可トークンなどの機密値を含み得るため、`Debug` は実値を表示せずバイト長を
/// `value_bytes` として表示する ([`ServerUrl`] の `Debug` も同じ方針である)。
#[derive(Clone, PartialEq, Eq)]
pub struct MoqtFragment {
    /// fragment type (`<type>` 部分)。ASCII 小文字 / 数字 / ハイフンに限る
    pub fragment_type: String,
    /// fragment value (`<value>` 部分)。意味は fragment type を登録した仕様が定める
    ///
    /// percent-encoding された生の文字列を保持する。`#` は RFC 3986 §3.5 の
    /// `fragment = *( pchar / "/" / "?" )` に含まれないため `%23` でなければならない。
    pub value: String,
}

impl core::fmt::Debug for MoqtFragment {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // フィールドを追加したときに表示漏れをコンパイラが検出できるよう、`..` を使わずに分解する
        let Self {
            fragment_type,
            value,
        } = self;
        // value は認可トークンなどの機密値を含み得るため、文字列ではなくバイト長を表示する
        f.debug_struct("MoqtFragment")
            .field("fragment_type", fragment_type)
            .field("value_bytes", &value.len())
            .finish()
    }
}

/// パース済みの接続先情報
///
/// 接続経路 (QUIC / WebTransport over HTTP/3 / WebTransport over HTTP/2 / QMux) は
/// [`Transport`] で選ぶため、ここには持たない。`--transport` が指定されていればそれを使い、無ければ `msf` fragment
/// の `connection`、どちらも無ければ native QUIC になる ([`resolve_transport`])。
///
/// query と fragment は認可トークンなどを含み得るため、`Debug` は実値を表示しない。
/// path (`?` より前) を表示し、`query: Option<usize>` に `?` 以降のバイト長、
/// [`MoqtFragment`] の `value_bytes` に fragment の value のバイト長、`cat_token_count` に
/// `cat` の件数、`cat_token_bytes` に各トークンのバイト長の配列を表示する。`msf` の
/// namespace は [`TrackNamespace`] の `Debug` 表現、track name は十進のバイト列として表示する
/// (いずれも機密値でない)。
///
/// authority の userinfo (`user:password@host` の最後の `@` より前) も実値を表示しない。
/// `authority` には `@` より後ろの host / port を表示し、userinfo がある場合だけ
/// `userinfo_bytes` にそのバイト長を表示する。
#[derive(Clone)]
pub struct ServerUrl {
    /// authority (userinfo、ホスト名または IP アドレス、ポートを含み得る)
    ///
    /// URL でポートを省略した場合はポートを含まない。接続先の決定では
    /// 既定ポート 443 を補って解釈する。
    ///
    /// [`parse_url`] は authority に userinfo (`user:password@host`) を含む URL を拒否しない
    /// ため、この値は userinfo を含み得る。接続時は userinfo を拒否する。
    /// `Debug` は userinfo を実値で表示しない。
    pub authority: String,
    /// パス部分 (query を含み、fragment を含まない)
    pub path: String,
    /// MOQT の fragment (`#` 以降)
    ///
    /// draft-ietf-moq-transport-23 §6.1.1 (Fragment Identifiers) によりサーバーへは
    /// 送信しない。接続経路 (`:path` / PATH option / SNI) では参照しない。
    /// `msf` fragment の track-identifier とパラメータの解釈結果は [`ServerUrl::msf`]、
    /// 予約パラメータ `cat` の認可トークンは [`ServerUrl::cat_tokens`] に取り出している。
    pub fragment: Option<MoqtFragment>,
    /// MSF fragment (`#msf:...`) の解釈結果
    ///
    /// draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation) の
    /// `msf-fragment-value = track-identifier [ "&" parameter-list ]` をパースした結果で、
    /// track-identifier が示す namespace と track name、パラメータ列を保持する。
    ///
    /// fragment が無い場合と fragment type が `msf` 以外の場合は `None`。
    /// `msf` fragment は moq-pub / moq-sub が解釈すると宣言した fragment type であり、形式が
    /// §11.1 の ABNF に一致しない場合は [`parse_url`] がエラーにする。
    pub msf: Option<MsfFragment>,
    /// MSF fragment の `cat` パラメータから取り出した C4M 認可トークン (出現順)
    ///
    /// draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters) は `cat` を
    /// "A base64 encoded C4M token" と定め、draft-ietf-moq-c4m-02 §2 / §5 は
    /// トークンを URL に埋め込むときの Base64 エンコードを定める。
    /// Base64 をデコードしたバイト列が MOQT の AUTHORIZATION_TOKEN
    /// (Token Type CAT) の Token Value になる (draft-ietf-moq-c4m-02 §8.2.1)。
    ///
    /// `msf` 以外の fragment と `cat` パラメータ無しの `msf` fragment では空。
    /// v01 の予約パラメータ `c4m` は v02 で `cat` に改名された (Appendix A.1.7)。本実装は
    /// 旧 `c4m` を予約パラメータとして扱わず、認可トークンの供給元にしない (fail-closed
    /// の方針)。
    /// 同一のトークンが複数回現れた場合は 1 つに畳む
    /// (draft-ietf-moq-transport-23 §8.9 (Authorization Token Compression) は
    /// alias 解決後の (Token Type, Token Value) の重複を拒否するため)。
    pub cat_tokens: Vec<Vec<u8>>,
}

/// `Debug` に出す authority と userinfo のバイト長を決める
///
/// authority は userinfo (`user:password@host`) を含み得る。RFC 3986 §3.2.1 は `user:password`
/// 形式の userinfo を deprecated とし、最初の `:` より後ろのデータを平文で表示しないこと
/// (SHOULD、`:` より後ろが空の場合を除く) を推奨する。本実装は安全側に倒し、userinfo 全体を
/// 実値で表示しない。最後の `@` より後ろの host / port だけを返し、`@` を含まない場合は
/// そのまま返してバイト長を `None` にする。
fn authority_debug_fields(authority: &str) -> (Option<usize>, &str) {
    // userinfo の区切りは最後の `@` とする (userinfo に `@` は現れないが、安全側に倒す)
    match authority.rsplit_once('@') {
        Some((userinfo, host)) => (Some(userinfo.len()), host),
        None => (None, authority),
    }
}

impl core::fmt::Debug for ServerUrl {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // フィールドを追加したときに表示漏れをコンパイラが検出できるよう、`..` を使わずに分解する
        let Self {
            authority,
            path,
            fragment,
            msf,
            cat_tokens,
        } = self;
        // path は query を含み、query は認可トークンを含み得るため、`?` より前だけを表示する
        let (path_without_query, query) = match path.split_once('?') {
            Some((path, query)) => (path, Some(query.len())),
            None => (path.as_str(), None),
        };
        // authority は userinfo (`user:password@host`) を含み得るため、最後の `@` より前を
        // 実値で表示しない。`@` より後ろの host / port は機密値でない
        let (userinfo_bytes, authority) = authority_debug_fields(authority);
        let mut debug = f.debug_struct("ServerUrl");
        debug.field("authority", &authority);
        // userinfo が無い場合の表示を変えないよう、存在するときだけフィールドを足す
        if let Some(userinfo_bytes) = userinfo_bytes {
            debug.field("userinfo_bytes", &userinfo_bytes);
        }
        debug
            .field("path", &path_without_query)
            .field("query", &query)
            .field("fragment", fragment)
            .field("msf", msf)
            .field("cat_token_count", &cat_tokens.len())
            .field(
                "cat_token_bytes",
                &cat_tokens
                    .iter()
                    .map(|token| token.len())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// URL をパースして authority ・ path ・ fragment ・ C4M 認可トークンを取得する
///
/// scheme は `moqt://` だけを受け付ける (`MOQT://` も受理する。RFC 3986 §3.1 に従い
/// scheme は大文字小文字を区別しない)。接続経路は [`Transport`] で選び、`--transport` が指定されて
/// いればそれを使い、無ければ `msf` fragment の `connection`、どちらも無ければ native QUIC になる
/// ([`resolve_transport`])。
/// scheme は比較のみを大文字小文字非区別に行い、authority / path / query / fragment は入力の
/// 文字列をそのまま保持する (RFC 3986 §6.2.2.1 は scheme と host を大文字小文字非区別とするが、
/// host を入力のまま使うのは DNS / SNI の非区別性に依存する設計判断である)。
///
/// fragment は `:path` と PATH option には含めない。draft-ietf-moq-transport-23 §6.1.1
/// (Fragment Identifiers) の `<type>:<value>` として解釈し、同節の文字種の MUST に
/// 一致しない場合はエラーにする。同 draft §16.2 (Media Type Registration) は
/// "Fragment identifiers for application/moqt follow the syntax defined in Section 6.1.1."
/// と定める。この仕様は将来 draft 改訂で変更される可能性がある。
///
/// `type` が `msf` の場合は value を MSF fragment (`track-identifier [ "&" parameter-list ]`)
/// として検証し、track-identifier の解釈結果を [`ServerUrl::msf`] に、予約パラメータ `cat` を
/// Base64 デコードした結果を [`ServerUrl::cat_tokens`] に入れる。MSF fragment の形式が
/// 不正な場合はエラーにする。
///
/// # Errors
///
/// - `moqt://` 以外の scheme、または `:` を含まない URL: `unsupported URL scheme`
/// - authority の欠落 (`//` が無い、または `//` の直後の authority が空): `requires authority`
/// - authority または path に空白 / 制御文字を含む: `invalid URL`
/// - fragment が `<type>:<value>` でない / type の文字種が §6.1.1 に一致しない: `invalid moqt URI fragment`
/// - `msf` fragment が MSF §11.1 の ABNF に一致しない: `invalid MSF fragment`
/// - `cat` の値が Base64 でない / 空: `invalid cat parameter`
///
/// query と fragment は認可トークンを含み得て (connection URI のパラメータは
/// draft-ietf-moq-msf-02 §11.5.2、fragment の `cat` は §11.1.1)、解析エラーはログや CLI の
/// 標準エラーに出るため、エラーメッセージには入力 URL 全体とパラメータ値を含めない。
/// scheme、`authority` / `path` などの成分名、空白のバイト位置、MSF fragment の
/// track-identifier、パラメータ名、パラメータ位置 (いずれも機密値でない) だけを示し、
/// fragment type の値は示さない。
pub fn parse_url(url: &str) -> Result<ServerUrl, String> {
    // RFC 3986 §3 (`URI = scheme ":" hier-part`) が scheme を `:` の前に置くことを定め、
    // §3.1 (Scheme) が比較の大文字小文字非区別を定める。エラーメッセージの生成は 1 箇所に集約する。
    let Some((scheme, rest)) = url.split_once(':') else {
        return Err("unsupported URL scheme (use moqt://)".to_string());
    };
    // `:` より前は、URL でない入力では任意の文字列になり得る。RFC 3986 §3.1 の scheme の
    // 文字種に一致するときだけ示し、query / fragment の認可トークンを反射させない
    if !is_uri_scheme(scheme) {
        return Err("unsupported URL scheme (use moqt://)".to_string());
    }
    if !scheme.eq_ignore_ascii_case("moqt") {
        return Err(format!("unsupported URL scheme '{scheme}' (use moqt://)"));
    }
    // RFC 3986 §3 は `//` の有無で authority の有無が決まり、draft-ietf-moq-transport-23
    // §6.1 も `moqt-URI = "moqt" "://" authority ...` と `//` を必須にする。未対応 scheme と
    // 区別できるよう、`//` の欠落は authority の欠落として報告する。
    // 同じ文言を `//` の欠落と authority の空の 2 箇所で返すため定数にする
    const REQUIRES_AUTHORITY: &str = "moqt:// URL requires authority (e.g. moqt://localhost:4443)";
    let Some(rest) = rest.strip_prefix("//") else {
        return Err(REQUIRES_AUTHORITY.to_string());
    };
    let (before_fragment, raw_fragment) = split_fragment(rest);
    let (authority, path) = split_authority_path(before_fragment);
    if authority.is_empty() {
        return Err(REQUIRES_AUTHORITY.to_string());
    }
    // RFC 3986 §2 (Characters) の `pchar` は `unreserved` / `pct-encoded` / sub-delims /
    // ":" / "@" であり、空白 (SP / HTAB) と制御文字を含まない。path に残った空白は SETUP の
    // PATH option で `PATH does not conform to RFC 3986` になり、authority に残った空白は
    // 名前解決の失敗になる。原因が分かるように、接続の前に URL として拒否する。
    // fragment は解釈する仕様が意味を定めるため対象外にし、percent-encoding された
    // `%20` は RFC 3986 §2.1 の `pct-encoded` として正当なので拒否しない。
    if let Some((component, index)) = first_url_whitespace(&authority, &path) {
        // 成分名とバイト位置は機密値でないため示し、URL 全体は示さない
        return Err(format!(
            "invalid URL: '{component}' contains a whitespace or control character at byte {index}"
        ));
    }
    let fragment = match raw_fragment {
        Some(fragment) => Some(parse_moqt_fragment(fragment)?),
        None => None,
    };
    // draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation): fragment type
    // "msf" の value は MSF の track-identifier とパラメータ列である。moq-pub / moq-sub が解釈するのは
    // track-identifier が示す namespace、予約パラメータ cat の認可トークン、予約パラメータ
    // connection の接続種別 ([`resolve_transport`]) である。
    // 同じ value を 2 度パースしないよう、解釈結果を 1 度だけ作って共有する。
    let msf = match &fragment {
        Some(f) if f.fragment_type == "msf" => Some(parse_msf_value(&f.value)?),
        _ => None,
    };
    let cat_tokens = match &msf {
        Some(msf) => parse_cat_tokens(msf)?,
        None => Vec::new(),
    };
    Ok(ServerUrl {
        authority,
        path,
        fragment,
        msf,
        cat_tokens,
    })
}

impl ServerUrl {
    /// MSF fragment の track-identifier が示す Track Namespace を返す
    ///
    /// draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation) の
    /// track-identifier を §11.1.2 (MSF Namespace-Name String Encoding) としてパースした
    /// namespace である。`msf` fragment が無い場合と `msf` 以外の fragment の場合は `None`。
    pub fn msf_namespace(&self) -> Option<&TrackNamespace> {
        self.msf.as_ref().map(|msf| &msf.namespace)
    }
}

/// `--namespace` の指定と `--url` の MSF fragment から Track Namespace を解決する
///
/// `explicit` が `Some` のときは draft-ietf-moq-transport-23 §8.8 (Representing Namespace and
/// Track Names) の表現としてパースする。`None` のときは `--url` の `msf` fragment
/// (draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation)) の track-identifier が
/// 示す namespace を使う。moq-pub / moq-sub が共通で使う。
///
/// 解決した namespace は 1 つ以上のフィールドを持つ必要がある。moq-pub / moq-sub が扱う catalog と
/// トラックは 1 つ以上のフィールドを持つ namespace に置くため、0 フィールドは
/// `--namespace` の空文字と同じく CLI の時点でエラーにする。
///
/// # Errors
///
/// - `explicit` が空文字 / §8.8 として不正: namespace の解釈エラー
/// - `explicit` が無く、`--url` に `msf` fragment も無い: `--namespace` を要求するエラー
/// - 解決した namespace が 0 フィールド: 1 つ以上のフィールドを要求するエラー
pub fn resolve_namespace(
    explicit: Option<&str>,
    url: &ServerUrl,
) -> Result<TrackNamespace, String> {
    let namespace = match explicit {
        Some(value) => {
            // 空文字は §8.8 では 0 フィールドの namespace になるが、moq-pub / moq-sub は 1 つ以上の
            // フィールドを持つ namespace を扱うため、指定の欠如と同じく CLI の時点で拒否する
            if value.is_empty() {
                return Err("--namespace must not be empty".to_string());
            }
            // §8.8 の正規形として解釈する。`-` は namespace フィールドの区切りであり、
            // フィールド内のリテラルな `-` は `.2d` として書く
            shiguredo_moqt::name::parse_namespace(value).map_err(|e| {
                format!(
                    "{e} ('-' separates namespace fields; use the draft-ietf-moq-transport-23 §8.8 form)"
                )
            })?
        }
        None => url.msf_namespace().cloned().ok_or_else(|| {
            "--namespace is required when --url has no 'msf' fragment (e.g. --namespace moq-example, or --url 'moqt://host:4443#msf:moq-example--video')".to_string()
        })?,
    };
    if namespace.fields().is_empty() {
        return Err("the namespace must have at least one field (e.g. 'moq-example')".to_string());
    }
    Ok(namespace)
}

/// URL の `connection` と `--transport` から接続種別を解決する
///
/// draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters) の予約パラメータ `connection`
/// は、client に接続種別を MUST で指定する (`q` は native QUIC、`wt` は WebTransport)。`wt` は
/// WebTransport の実装 (HTTP/3 / HTTP/2) を指定しないため、本実装は `wt-h3` (HTTP/3 上の
/// WebTransport) を既定にし、`wt-h2` は `--transport wt-h2` を明示したときだけ使う。WebTransport の
/// 主経路は HTTP/3 であり、`wt-h2` は relay の capsule 対応に依存する経路である
/// (draft-ietf-moq-transport-23 §6.2 (Session establishment) は HTTP/2 上の WebTransport も
/// 許容する)。tobi-quic はすべての対応プラットフォームで `wt-h3` を提供する。
///
/// 解決の規則は次のとおりである。
///
/// - fragment に `connection` が無ければ `explicit` (`--transport` の指定) を使い、省略時は
///   native QUIC にする
/// - `connection=q` と native QUIC、`connection=wt` と WebTransport (`wt-h3` / `wt-h2`) は
///   整合するため `explicit` をそのまま使う
/// - `connection` と `explicit` が矛盾する場合はエラーにする。`connection` は接続種別を MUST で
///   指定するため、`explicit` で上書きできない (`explicit` を優先して fragment を無視することは
///   許されない)
/// - `connection=q` と `connection=wt` が同時に指定された場合はエラーにする。§11.1.1 は `q` に
///   Native QUIC、`wt` に WebTransport を MUST で対応づけており、同じ `connection` が両方を
///   要求すると 2 つの MUST が両立しない。§11.1.1 の union (`If multiple ranges are specified
///   within the same URL for the same parameter, the client MUST process the union of those
///   ranges.`) は値域 (range) を対象とした規則であり `connection` には適用しないため、
///   [`MsfFragment::connection_types`] は `connection` の値を出現順にすべて返すだけである。
///   moq-pub / moq-sub はその結果を見て両立しない組み合わせを拒否する
/// - `--transport auto` は [`Transport::Auto`] を返す。候補の展開と競争は [`connect`] が行う。
///   `connection=q` と組み合わせた場合は native QUIC だけを、`connection=wt` と組み合わせた
///   場合は `wt-h3` / `wt-h2` を競争する。`connection` が無い場合は `quic` / `wt-h3` /
///   `qmux` / `wt-h2` を競争する ([`Transport::Auto`] の doc を参照する)
///
/// 値域の検証 (`q` / `wt` 以外の拒否) は [`MsfFragment::connection_types`] が行う。
///
/// # Errors
///
/// - `connection` に `q` / `wt` 以外の値がある: fragment の解釈エラー
/// - `connection` と `--transport` が矛盾する: どちらを優先するかを決められないためエラー
/// - `connection=q` と `connection=wt` が同時に指定されている: 両立しないためエラー
pub fn resolve_transport(
    explicit: Option<Transport>,
    url: &ServerUrl,
) -> Result<Transport, String> {
    let mut requires_quic = false;
    let mut requires_webtransport = false;
    if let Some(msf) = url.msf.as_ref() {
        for connection in msf.connection_types().map_err(|e| e.to_string())? {
            match connection {
                MsfConnectionType::Quic => requires_quic = true,
                MsfConnectionType::WebTransport => requires_webtransport = true,
            }
        }
    }
    if requires_quic && requires_webtransport {
        return Err(
            "'connection=q' and 'connection=wt' cannot be used together: draft-ietf-moq-msf-02 \
             §11.1.1 mandates a native QUIC connection for 'q' and a WebTransport connection for \
             'wt' (remove one of them from --url)"
                .to_string(),
        );
    }
    // fragment が要求する接続種別。`wt` は WebTransport の実装を指定しないため WebTransport と
    // して扱い、実装は `explicit` (省略時は `wt-h3`) で選ぶ
    let required = if requires_quic {
        Some(MsfConnectionType::Quic)
    } else if requires_webtransport {
        Some(MsfConnectionType::WebTransport)
    } else {
        None
    };
    match (required, explicit) {
        (None, explicit) => Ok(explicit.unwrap_or(Transport::Quic)),
        (Some(MsfConnectionType::Quic), None) => Ok(Transport::Quic),
        (Some(MsfConnectionType::Quic), Some(explicit @ (Transport::Quic | Transport::Auto))) => {
            Ok(explicit)
        }
        (Some(MsfConnectionType::Quic), Some(explicit)) => Err(format!(
            "--transport {} conflicts with 'connection=q' in --url: draft-ietf-moq-msf-02 \
             §11.1.1 mandates a native QUIC connection (remove --transport or the 'connection' \
             parameter)",
            explicit.as_str()
        )),
        (Some(MsfConnectionType::WebTransport), None) => Ok(Transport::WtH3),
        (
            Some(MsfConnectionType::WebTransport),
            Some(explicit @ (Transport::WtH3 | Transport::WtH2 | Transport::Auto)),
        ) => Ok(explicit),
        (Some(MsfConnectionType::WebTransport), Some(explicit)) => Err(format!(
            "--transport {} conflicts with 'connection=wt' in --url: draft-ietf-moq-msf-02 \
             §11.1.1 mandates a WebTransport connection (use --transport wt-h3 or wt-h2)",
            explicit.as_str()
        )),
    }
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

/// authority と path のうち、空白または制御文字を最初に含む成分を返す
///
/// 戻り値は成分名 (`authority` / `path`) と、その成分の先頭からのバイト位置。
/// ASCII の空白 (SP / HTAB / LF / CR 等) と制御文字 (0x00-0x1F / 0x7F) を対象にする。
/// RFC 3986 §2 (Characters) は `host` / `path` にこれらを含めない。
/// 非 ASCII の空白 (全角スペース等) は `pct-encoded` ではないが、UTF-8 のままでも
/// 接続層で意味を持たないため、ここでは ASCII の空白と制御文字だけを拒否する。
fn first_url_whitespace<'a>(authority: &'a str, path: &'a str) -> Option<(&'a str, usize)> {
    for (name, component) in [("authority", authority), ("path", path)] {
        if let Some(index) = component
            .bytes()
            .position(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        {
            return Some((name, index));
        }
    }
    None
}

/// moqt URI の fragment を `<type>:<value>` として解釈する
///
/// draft-ietf-moq-transport-23 §6.1.1 (Fragment Identifiers) は moqt URI の fragment を
/// 登録済みの fragment type と `:` で始めると MUST で定め、type の文字種を
/// "Fragment type identifiers MUST consist of ASCII lowercase letters, digits, and hyphens (a-z, 0-9, -)."
/// と定める (§6.1.1 に ABNF は無く、本文の MUST で示される)。`:` が無い fragment と
/// 空の type と文字種に一致しない type は拒否する。
/// value は type を登録した仕様が意味を定めるため、最初の `:` 以降をそのまま保持する。
/// `#` は fragment の中に現れ得ない (RFC 3986 §3.5) ため、2 個目以降の `#` は拒否する。
///
/// この仕様は将来 draft 改訂で変更される可能性がある。
///
/// エラーメッセージには fragment の実値を含めない (認可トークンなどの機密値を含み得るため)。
fn parse_moqt_fragment(fragment: &str) -> Result<MoqtFragment, String> {
    if fragment.contains('#') {
        return Err(
            "invalid moqt URI fragment ('#' inside a fragment must be percent-encoded)".to_string(),
        );
    }
    let Some((fragment_type, value)) = fragment.split_once(':') else {
        return Err("invalid moqt URI fragment (must be '<type>:<value>')".to_string());
    };
    if !is_fragment_type(fragment_type) {
        return Err(
            "invalid moqt URI fragment type (must be non-empty and consist of ASCII lowercase letters, digits, and hyphens)"
                .to_string(),
        );
    }
    Ok(MoqtFragment {
        fragment_type: fragment_type.to_string(),
        value: value.to_string(),
    })
}

/// fragment type が draft-ietf-moq-transport-23 §6.1.1 の文字種の MUST に一致するかを返す
fn is_fragment_type(fragment_type: &str) -> bool {
    !fragment_type.is_empty()
        && fragment_type
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// RFC 3986 §3.1 の `scheme = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )` に一致するかを返す
///
/// URL でない入力の `:` より前は任意の文字列になり得るため、エラーメッセージに示してよい
/// scheme かどうかの判定に使う。
fn is_uri_scheme(scheme: &str) -> bool {
    let mut bytes = scheme.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

/// `msf` fragment の value を MSF fragment としてパースする
///
/// draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation) の
/// `msf-fragment-value = track-identifier [ "&" parameter-list ]` に従い、value 全体を
/// [`parse_msf_fragment`] で検証する。MSF fragment の形式が不正な場合は [`parse_url`] の
/// エラーとして扱う (moq-pub / moq-sub が解釈すると宣言した fragment type の値は、黙って無視せず
/// 仕様どおりに扱う)。
///
/// # Errors
///
/// - MSF fragment が §11.1 の ABNF に一致しない: `invalid MSF fragment`
///
/// エラーメッセージにはパラメータ値を含めない (認可トークンなどの機密値を含み得るため)。
/// track-identifier とパラメータ名は機密値でないため、内側の [`parse_msf_fragment`] の
/// メッセージ経由で含まれ得る。
fn parse_msf_value(fragment_value: &str) -> Result<MsfFragment, String> {
    // parse_msf_fragment は `msf:` 前置を含む fragment 全体を受け取る
    parse_msf_fragment(&format!("msf:{fragment_value}"))
        .map_err(|e| format!("invalid MSF fragment: {e}"))
}

/// パース済みの `msf` fragment から `cat` パラメータの認可トークンを取り出す
///
/// 予約パラメータ `cat` を読む (draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters))。
///
/// 各 `cat` の値は RFC 3986 §2.1 (Percent-Encoding) の `%XX` をデコードしてから
/// Base64 としてデコードする。得られたバイト列が MOQT の AUTHORIZATION_TOKEN
/// (Token Type CAT) の Token Value になる (draft-ietf-moq-c4m-02 §8.2.1)。
/// この関数はバイト列を取り出すだけで、トークンの検証と認可は行わない。
///
/// # Errors
///
/// - `cat` の値が Base64 でない / 空: `invalid cat parameter`
///
/// エラーメッセージには `cat` の実値を含めない (認可トークンそのものであるため)。
fn parse_cat_tokens(msf: &MsfFragment) -> Result<Vec<Vec<u8>>, String> {
    let mut tokens: Vec<Vec<u8>> = Vec::new();
    for text in msf.cat_tokens() {
        let token = decode_cat_token(text)?;
        // draft-ietf-moq-transport-23 §8.9 (Authorization Token Compression):
        // alias 解決後の (Token Type, Token Value) の重複は送れないため、同一トークンは畳む
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    Ok(tokens)
}

/// `cat` パラメータの値を認可トークンのバイト列へデコードする
///
/// draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters) は `cat` を
/// "A base64 encoded C4M token" と定める。RFC 4648 は §4 の Base64 と §5 の
/// base64url を定義しており、draft-ietf-moq-c4m-02 §2 / §5 はどちらを使うかを指定しないため、
/// 両方とパディングの有無を受理する (library の `CatToken::decode` が URL 埋め込みの
/// Base64 を扱う規則と同じである)。
///
/// # Errors
///
/// - Base64 としてデコードできない / デコード結果が空: `invalid cat parameter`
///
/// エラーメッセージには入力の実値を含めない (認可トークンそのものであるため)。
fn decode_cat_token(text: &str) -> Result<Vec<u8>, String> {
    let invalid = || "invalid cat parameter: not a non-empty Base64 token".to_string();
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
/// path には query (`?` 以降) を含める。draft-ietf-moq-transport-23 §9.1.2 (PATH) は
/// query が存在する場合に `?` と query を PATH option へ連結することを MUST とするため。
///
/// RFC 3986 §3.2 は authority が `/` / `?` / `#` で終端すると定めるため、`#` 以降
/// (fragment) は path に含めない。呼び出し元の [`parse_url`] は先に fragment を分離するが、
/// この関数を単独で呼んでも fragment が path に混入しない。
/// 例: `split_authority_path("example.com/path#type:value")` は
/// `("example.com", "/path")`、`split_authority_path("example.com#type:value")` は
/// `("example.com", "/")` を返す。
/// path が空の場合は `/` を補う。RFC 3986 §6.2.3 は
/// "a URI that uses the generic syntax for authority with an empty path should be normalized to
/// a path of \"/\"" と定めるため、query のみの場合も `/` を補う。
fn split_authority_path(rest: &str) -> (String, String) {
    // authority は最初の `/` / `?` / `#` まで (RFC 3986 §3.2)
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest[..end].to_string();
    // authority の終端が `/` または `?` のときは `#` がここに残るため切り落とす
    // (RFC 3986 §3.2 の終端文字のうち `#` だけは authority の後ろに現れる)
    let path_and_query = &rest[end..];
    let path_and_query = match path_and_query.find('#') {
        Some(fragment_start) => &path_and_query[..fragment_start],
        None => path_and_query,
    };
    let path = if path_and_query.is_empty() {
        "/".to_string()
    } else if path_and_query.starts_with('?') {
        // path が空で query のみの場合は `/` を補う
        format!("/{path_and_query}")
    } else {
        path_and_query.to_string()
    };
    (authority, path)
}

/// authority から host 部 (ポートを除く) を取り出す
///
/// TLS の SNI / 証明書検証に使う server_name はポートを含まないため、ここで分離する。
/// IPv6 リテラルは `[` から `]` までを host とし、`]` が無い場合は authority 全体を返す。
/// IPv4 / ホスト名は最後の `:` より前を host とし、ポートが無い場合は authority 全体を返す。
fn host_from_authority(authority: &str) -> &str {
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
/// draft-ietf-moq-transport-23 §6.1.2 (Dereferencing a MOQT URI) は
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
/// (draft-ietf-moq-transport-23 §6.1.2)。
///
/// 受理する host は RFC 3986 §3.2.2 (Host) の `IP-literal / IPv4address / reg-name` のうち
/// IPv6 リテラル、IPv4 リテラル、reg-name である。IPvFuture は受け付けない。
/// RFC 3986 §3.2.2 は未知の version flag を持つ IP-literal を dereference する
/// アプリケーションに 'address mechanism not supported' のエラーを返すことを推奨する。
/// RFC 6874 の zone id 付き IPv6 リテラル (`[fe80::1%25en0]`) も受け付けない。
/// RFC 3986 §3.2.2 は "This syntax does not support IPv6 scoped addressing zone identifiers."
/// と定め、[`std::net::SocketAddr`] も zone id を解釈できない。
///
/// 次の authority はエラーにする。draft-ietf-moq-transport-23 §6.1 (MOQT URI Scheme) は
/// "The authority portion MUST NOT contain an empty host portion." と定める
/// (この仕様は将来 draft 改訂で変更される可能性がある)。
///
/// IPv6 リテラルを `[` `]` で囲む規則は RFC 3986 §3.2.2 (Host)、ポートを数字にする
/// 規則は RFC 3986 §3.2.3 (Port) による。ポート 0 は RFC 3986 §3.2.3 の `port = *DIGIT`
/// としては妥当だが接続先として使えないため受け付けない。
///
/// userinfo も受け付けない。RFC 3986 §3.2 の authority 構文は userinfo を許すが、
/// draft-ietf-moq-transport-23 §6.1 は userinfo に言及せず、RFC 3986 §3.2.1 は受け取った
/// reference 中の userinfo を reject する選択を許す
/// ("Applications may choose to ignore or reject such data when it is received as part of a reference")。
///
/// - host が空 (`""` / `":4443"` / `"[]"`)
/// - `[` `]` の中身が IPv6 アドレスでない (`[example.com]` / `[v1.fe80::]` / `[fe80::1%25en0]`)
/// - IPv6 リテラルの `]` が無い、または `]` の直後がポート区切りでない (`[::1` / `[::1]x`)
/// - ブラケット無しの host に `:` が残る (裸の IPv6 リテラル `2001:db8::1`)
/// - ポートの区切りがあるのに数字が続かない (`127.0.0.1:` / `127.0.0.1:abc`)
/// - ポートが 0 (`127.0.0.1:0`)
/// - userinfo を含む (`user@127.0.0.1`)
fn authority_parts(authority: &str) -> Result<AuthorityParts<'_>, TransportError> {
    let host = host_from_authority(authority);
    // `@` を含む authority は userinfo として拒否し、host として名前解決に回さない。
    // port の解釈より先に判定し、`user:pass@host` を port のエラーとして報告しない
    if authority.contains('@') {
        return Err(invalid_authority(authority, "userinfo is not supported"));
    }

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
    // `[` `]` の中身は IPv6 アドレスに限る (RFC 3986 §3.2.2 の IP-literal)
    if authority.starts_with('[') && host.parse::<std::net::Ipv6Addr>().is_err() {
        return Err(invalid_authority(
            authority,
            "the host in '[' and ']' must be an IPv6 address",
        ));
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
        Some(text) => {
            let port = text
                .parse::<u16>()
                .map_err(|e| invalid_authority(authority, &e.to_string()))?;
            if port == 0 {
                return Err(invalid_authority(authority, "port 0 cannot be used"));
            }
            port
        }
    };

    Ok(AuthorityParts { host, port })
}

/// authority の解釈に失敗したことを表すエラーを作る
///
/// userinfo を含む authority はパスワードなどの機密値を含み得るため、その場合は authority を
/// メッセージに含めない。この判定をここに集約し、呼び出し側が authority を渡すだけで安全に
/// なるようにする。
fn invalid_authority(authority: &str, reason: &str) -> TransportError {
    if authority.contains('@') {
        return TransportError::InvalidAuthority(format!("invalid server address: {reason}"));
    }
    TransportError::InvalidAuthority(format!("invalid server address '{authority}': {reason}"))
}

/// 名前解決 (`getaddrinfo`) の待ち時間の上限
///
/// `tokio::net::lookup_host` は OS のリゾルバに委譲するため、リゾルバが応答しないと
/// 数十秒から数分ブロックする。セッションのタイムアウトより前に解決を打ち切る。
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// authority を解決して接続先の `SocketAddr` の列を得る
///
/// host が IP リテラルなら `SocketAddr` に直接パースして 1 件の列にし、ホスト名なら
/// `tokio::net::lookup_host` で解決した結果を解決順のまま列にする。ポートが省略された
/// authority は既定ポート 443 を使う (draft-ietf-moq-transport-23 §6.1.2)。
///
/// 名前解決は `RESOLVE_TIMEOUT` で打ち切る。打ち切った場合は
/// [`TransportError::ResolutionFailed`] になり、接続を試みずに終わる。
/// IP リテラルは OS のリゾルバを通さないため、このタイムアウトの影響を受けない。
///
/// # Errors
///
/// authority を解釈できない場合は [`TransportError::InvalidAuthority`]、
/// 名前解決が失敗した場合・`RESOLVE_TIMEOUT` を超えた場合・解決結果が
/// 0 件の場合は [`TransportError::ResolutionFailed`] になる。
// テスト方針: IP リテラルと `localhost` の経路は単体テストで固定し、タイムアウトの
// 行使は resolver を差し替える単体テスト (応答しない resolver を future で置き換える) と、
// タイムアウト値を一時的に縮めた実機確認で確認する。
pub async fn resolve_socket_addrs(authority: &str) -> Result<Vec<SocketAddr>, TransportError> {
    resolve_socket_addrs_within(authority, RESOLVE_TIMEOUT).await
}

/// authority をタイムアウト付きで解決する (実 DNS を注入する薄いラッパー)
///
/// 実 DNS ([`tokio::net::lookup_host`]) を [`resolve_socket_addrs_with`] に渡して解決する。
/// resolver を差し替えてタイムアウト経路を決定的に固定する単体テストは
/// [`resolve_socket_addrs_with`] を直接使うため、この関数は既存の呼び出し点を
/// 変えないためのラッパーである。
/// 公開 API は `RESOLVE_TIMEOUT` を渡す [`resolve_socket_addrs`] だけにする。
async fn resolve_socket_addrs_within(
    authority: &str,
    timeout: Duration,
) -> Result<Vec<SocketAddr>, TransportError> {
    // 本体は resolver を差し替えられる core 関数に委ねる。実 DNS の経路は
    // `tokio::net::lookup_host` を注入する形で変わらない
    resolve_socket_addrs_with(authority, timeout, |addr_text| {
        tokio::net::lookup_host(addr_text)
    })
    .await
}

/// authority をタイムアウト付きで解決する (resolver を注入する本体)
///
/// `lookup` は socket addr 文字列を受け取り、解決済みアドレスの列を返す future を作る。
/// resolver を差し替えられるのは、応答しないリゾルバを実際の OS リゾルバでは用意できない
/// 環境でも、タイムアウトの行使の経路を単体テストで決的に固定できるようにするためである
/// ([`connect_with_fallback`] の注入クロージャと同じ方式)。実 DNS による解決は
/// [`resolve_socket_addrs_within`] 経由でのみ行う。
async fn resolve_socket_addrs_with<F, Fut, I>(
    authority: &str,
    timeout: Duration,
    lookup: F,
) -> Result<Vec<SocketAddr>, TransportError>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = std::io::Result<I>>,
    I: Iterator<Item = SocketAddr>,
{
    let parts = authority_parts(authority)?;
    let addr_text = parts.socket_addr_string();

    let addrs = if let Ok(addr) = addr_text.parse::<SocketAddr>() {
        vec![addr]
    } else {
        let resolved = tokio::time::timeout(timeout, lookup(addr_text))
            .await
            .map_err(|_| {
                resolve_failed(
                    authority,
                    &format!("name resolution timed out after {timeout:?}"),
                )
            })?
            .map_err(|e| resolve_failed(authority, &e.to_string()))?;
        let addrs: Vec<SocketAddr> = resolved.collect();
        if addrs.is_empty() {
            return Err(resolve_failed(authority, "no address"));
        }
        addrs
    };

    tracing::info!("Resolved {authority} to {addrs:?}");
    Ok(addrs)
}

/// 解決した接続先の列を順に試して接続を確立する
///
/// 名前解決の結果は環境依存の順序で返るため、先頭のアドレスへの接続が失敗したら
/// 次のアドレスを試す。`localhost` が IPv6 と IPv4 の両方に解決される環境では、
/// IPv4 のみ待ち受ける relay へ接続するために必要になる。
///
/// 並行接続 (Happy Eyeballs) と再解決は行わず、解決結果の順序どおりに順次試行する。
/// `connect` は各試行で 1 回呼ばれ、試行したアドレスは常に info ログに残す。
/// すべての試行が失敗した場合は最後の失敗を返す。
///
/// [`connect`] はこの関数ではなく、アドレスとトランスポートを Happy Eyeballs v3 で
/// 競争させる。この関数は並行競争を行わない汎用ヘルパーとして残している。
///
/// # Errors
///
/// 名前解決に失敗した場合と、`connect` がすべてのアドレスで失敗した場合はエラーになる。
pub async fn connect_with_fallback<T, E, F, Fut>(authority: &str, connect: F) -> Result<T, E>
where
    E: From<TransportError> + std::fmt::Display,
    F: FnMut(SocketAddr) -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let addrs = resolve_socket_addrs(authority).await?;
    try_resolved_addrs(authority, &addrs, connect).await
}

/// 解決済みの空でないアドレス列を順に試す
async fn try_resolved_addrs<T, E, F, Fut>(
    authority: &str,
    addrs: &[SocketAddr],
    mut connect: F,
) -> Result<T, E>
where
    E: std::fmt::Display,
    F: FnMut(SocketAddr) -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let mut last: Option<E> = None;
    for &addr in addrs {
        tracing::info!("Connecting to {addr} (authority {authority})");
        match connect(addr).await {
            Ok(connected) => return Ok(connected),
            Err(error) => {
                // 失敗した試行もログに残し、次に試すアドレスがあることを分かるようにする
                tracing::warn!("Failed to connect to {addr}: {error}");
                last = Some(error);
            }
        }
    }
    // 解決結果が 0 件の場合は resolve_socket_addrs が失敗するため last は必ず埋まる
    Err(last.expect("resolve_socket_addrs must return at least one address"))
}

/// 2 つの URL が同じ session を指すか
///
/// GOAWAY の New Session URI が現在の URL と同じかどうかの判定に使う。`ServerUrl` は
/// 全体の `PartialEq` を持たないため、session を決める authority / path / fragment を比較する。
/// fragment には cat の認可トークンも含まれるため、トークンだけが異なる URL は同一 session
/// とみなさない (別 origin / 別トークンは [`migration_target`] が移行先として解決する)。
pub fn same_session_url(current: &ServerUrl, target: &ServerUrl) -> bool {
    current.authority == target.authority
        && current.path == target.path
        && current.fragment == target.fragment
}

/// GOAWAY の New Session URI から移行先を決める
///
/// draft-ietf-moq-transport-23 §9.2 (GOAWAY) / §6.6.1 (Graceful Session Migration) に従う。
///
/// - control stream の GOAWAY (`on_request_stream` が `None`) は session 全体を移行する
///   ([`MigrationScope::Session`])。
/// - request stream の GOAWAY (`on_request_stream` が `Some`) は当該 request だけを移行する
///   対象として返す ([`MigrationScope::Request`])。request 単位の移行は呼び出し側が
///   side session を確立して行う。URI が不正な場合は session を維持できるよう
///   `Ok(None)` にする。
/// - `new_session_uri` が空のときは現在の URL を再利用する (空 URI は request stream の
///   GOAWAY でも「現在の session で再発行する」ことを意味する)。
/// - 非空のときは [`parse_url`] と同じ規則で解釈し、そのあと authority に userinfo
///   (`user@host`) を含まないことを確認する。userinfo はパスワードなどの機密値を含み得て、
///   移行先としてログに表示し得るため、request 単位では `Ok(None)`、session 単位では
///   エラーにする。認可トークン (`cat`) は
///   **現在の URL と同じ origin (host / port) のときだけ** 引き継ぐ。別 origin へ
///   credential を送らないためである。移行先が自前の `cat` を持つ場合はそちらを優先する。
///
/// 移行先 URI の `connection` (draft-ietf-moq-msf-02 §11.1.1) と msf の track-identifier は
/// ここでは解釈しない。接続経路は呼び出し側が [`resolve_transport`] で現在の解決結果と
/// 矛盾しないことを確認し、track 名は購読 / 配信の張り直しで使う既存の指定をそのまま使う。
///
/// # Errors
///
/// control stream の GOAWAY で `new_session_uri` が UTF-8 でない場合、[`parse_url`] が
/// 解釈できない場合、authority に userinfo を含む場合に [`TransportError`] になる。
/// request stream の GOAWAY では同じ入力で `Ok(None)` になり、session を維持する。
pub fn migration_target(
    new_session_uri: &[u8],
    on_request_stream: Option<u64>,
    current: &ServerUrl,
) -> Result<Option<MigrationTarget>, TransportError> {
    let scope = match on_request_stream {
        Some(request_id) => MigrationScope::Request(request_id),
        None => MigrationScope::Session,
    };
    if new_session_uri.is_empty() {
        return Ok(Some(MigrationTarget {
            scope,
            url: current.clone(),
        }));
    }
    let uri = match std::str::from_utf8(new_session_uri) {
        Ok(uri) => uri,
        Err(e) => {
            if on_request_stream.is_some() {
                // request 単位移行では移行先を決められない。URI が壊れていても session を
                // 維持し、該当 request の終端は peer / Session の deadline に任せる。
                return Ok(None);
            }
            return Err(TransportError::Internal(format!(
                "new session URI is not UTF-8: {e}"
            )));
        }
    };
    let mut target = match parse_url(uri) {
        Ok(target) => target,
        Err(e) => {
            if on_request_stream.is_some() {
                return Ok(None);
            }
            // parse_url のエラーは URL の実値を含まないため、URI を付け直さない。
            // New Session URI は peer が供給し、query / fragment に認可トークンを
            // 含み得る (draft-ietf-moq-msf-02 §11.5.2)
            return Err(TransportError::Internal(format!(
                "invalid new session URI: {e}"
            )));
        }
    };
    // authority に userinfo を含む New Session URI は受け付けない。パスワードなどの機密値を
    // 保持し、移行先としてログに表示し得るため (RFC 3986 §3.2.1)。接続時の authority_parts でも
    // 拒否するが、移行の判断とログ出力より前にここで落とす
    if target.authority.contains('@') {
        if on_request_stream.is_some() {
            return Ok(None);
        }
        return Err(TransportError::Internal(
            "invalid new session URI: userinfo is not supported".to_string(),
        ));
    }
    // 認可トークンは同一 origin のときだけ引き継ぐ。別 origin へ送ると credential の
    // 漏洩になるため、移行先ではトークンを再取得する (再取得は
    // draft-ietf-moq-msf-02 §11.5.4 の対象)。
    if target.cat_tokens.is_empty() && same_origin(&target, current) {
        target.cat_tokens = current.cat_tokens.clone();
    }
    Ok(Some(MigrationTarget { scope, url: target }))
}

/// 2 つの [`ServerUrl`] が同じ origin を指すか
///
/// draft-ietf-moq-transport-23 §9.2 (GOAWAY) は New Session URI に同じ scheme を使うことを
/// SHOULD とする。scheme は [`parse_url`] が `moqt://` に固定しているため、ここでは host と
/// port を比較する。host は RFC 3986 §3.2.2 により大文字小文字を区別せず、port の省略時は
/// 既定ポート 443 を使う (draft-ietf-moq-transport-23 §6.1.2)。
fn same_origin(a: &ServerUrl, b: &ServerUrl) -> bool {
    let (Ok(a), Ok(b)) = (authority_parts(&a.authority), authority_parts(&b.authority)) else {
        return false;
    };
    a.port == b.port && a.host.eq_ignore_ascii_case(b.host)
}

/// 移行先の適用範囲
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationScope {
    /// control stream の GOAWAY。session 全体を移行する
    Session,
    /// request stream の GOAWAY。指定した request だけを移行する
    Request(u64),
}

/// GOAWAY が示す移行先
#[derive(Debug, Clone)]
pub struct MigrationTarget {
    /// 移行の適用範囲
    pub scope: MigrationScope,
    /// 移行先の URL
    pub url: ServerUrl,
}

/// GOAWAY の Timeout を移行期限として管理する
///
/// draft-ietf-moq-transport-23 §9.2 (GOAWAY) の Timeout は、GOAWAY を送った側が
/// セッションを閉じるまで待つ時間 (ミリ秒) である。受信側は「GOAWAY を受信した時刻 +
/// Timeout」を移行期限として扱い、期限を過ぎたら移行を打ち切る。
///
/// Timeout が 0 のときは送信側に specific timeout が無いことを意味する
/// ("the recipient SHOULD migrate as quickly as possible")。期限なしで無限に再試行しないよう、
/// ローカルの既定値 ([`Self::DEFAULT_TIMEOUT`]) を使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationDeadline {
    /// 移行を打ち切る時刻
    deadline: Instant,
    /// Timeout 0 のためローカル既定値を用いたか
    local_default: bool,
}

impl MigrationDeadline {
    /// Timeout 0 のときに使うローカル既定の移行期限
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

    /// Timeout の上限
    ///
    /// 仕様上は任意のミリ秒を受け取れるが、移行期限として 24 時間を超える値は扱わない。
    /// `Instant` の加算でオーバーフローさせないための防御でもある。
    pub const MAX_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

    /// GOAWAY の受信時刻と Timeout から期限を作る
    pub fn after_goaway(received_at: Instant, timeout_ms: u64) -> Self {
        let (timeout, local_default) = if timeout_ms == 0 {
            (Self::DEFAULT_TIMEOUT, true)
        } else {
            (
                Duration::from_millis(timeout_ms).min(Self::MAX_TIMEOUT),
                false,
            )
        };
        Self {
            deadline: received_at + timeout,
            local_default,
        }
    }

    /// 指定時刻から見た残り時間
    pub fn remaining_at(&self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }

    /// 指定時刻で期限切れか
    pub fn expired_at(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    /// 現在時刻から見た残り時間
    pub fn remaining(&self) -> Duration {
        self.remaining_at(Instant::now())
    }

    /// 現在時刻で期限切れか
    pub fn expired(&self) -> bool {
        self.expired_at(Instant::now())
    }

    /// Timeout 0 のためローカル既定値を使ったか
    pub fn uses_local_default(&self) -> bool {
        self.local_default
    }
}

/// 移行先への接続を再試行するときの待ち時間
///
/// 移行は GOAWAY の Timeout 内に完了させる必要がある。初回は短い待ち時間で試し、
/// 失敗するたびに倍にする。上限を設けて、期限までに複数回試せるようにする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// 初回の待ち時間
    pub initial_delay: Duration,
    /// 待ち時間の上限
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_millis(200),
            max_delay: Duration::from_secs(2),
        }
    }
}

impl RetryPolicy {
    /// `attempt` 回目 (0 始まり) の待ち時間を返す
    ///
    /// 同期した再試行を避けるため、指数バックオフの範囲内で 0 から 1/4 の jitter を加える。
    /// jitter は暗号用途ではなく、同時に移行するクライアントの再試行をずらすためだけに使う。
    pub fn delay(&self, attempt: u32) -> Duration {
        let factor = 1u32 << attempt.min(16);
        let base = self
            .initial_delay
            .saturating_mul(factor)
            .min(self.max_delay);
        base + jitter(base / 4)
    }
}

/// `max` 以下の擬似乱数を作る
fn jitter(max: Duration) -> Duration {
    if max.is_zero() {
        return Duration::ZERO;
    }
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    hasher.write_u64(nanos);
    Duration::from_millis(hasher.finish() % (max.as_millis() as u64 + 1))
}

/// 移行先への接続エラーが再試行可能か
///
/// URI / authority の解釈エラー、プロトコル交渉の失敗、認可に起因する 4xx 応答は
/// 同じ設定で再試行しても成功しないため除外する。ネットワーク到達不能、名前解決の一時失敗、
/// 5xx / 429 などは再試行する。
pub fn is_retryable_connect_error(error: &TransportError) -> bool {
    match error {
        TransportError::InvalidAuthority(_) => false,
        TransportError::ProtocolNegotiationFailed { .. } => false,
        TransportError::Moqt(_)
        | TransportError::InvalidState(_)
        | TransportError::Unsupported(_)
        | TransportError::Internal(_) => false,
        TransportError::ConnectFailed { status } => {
            matches!(status, None | Some(408 | 425 | 429) | Some(500..=599))
        }
        TransportError::Quic(_)
        | TransportError::ResolutionFailed(_)
        | TransportError::Transport(_)
        | TransportError::Http3(_)
        | TransportError::Http2(_)
        | TransportError::WtH2(_)
        | TransportError::Qmux(_)
        | TransportError::ConnectionClosed
        | TransportError::PeerSessionClosed { .. }
        | TransportError::StreamReset { .. }
        | TransportError::StreamClosed => true,
    }
}

/// 移行期限まで接続を再試行する
///
/// 1 回の試行が移行期限を超えて待ち続けないよう、[`tokio::time::timeout`] で残り時間を
/// 上限にする。再試行不能なエラーはそのまま返し、期限切れの場合は最後のエラーを返す。
///
/// # Errors
///
/// 再試行不能なエラー、または移行期限までに接続できなかった場合に [`TransportError`] になる。
pub async fn connect_with_retry(
    params: &ConnectParams<'_>,
    deadline: MigrationDeadline,
    policy: RetryPolicy,
    task_monitor: &tokio_metrics::TaskMonitor,
) -> Result<
    (
        crate::moqt_client::MoqtClient,
        crate::transport::StreamAcceptor,
    ),
    TransportError,
> {
    let mut attempt = 0u32;
    let mut last_error = None;
    loop {
        let remaining = deadline.remaining();
        if remaining.is_zero() {
            return Err(last_error.unwrap_or_else(|| {
                TransportError::Internal("migration deadline expired".to_string())
            }));
        }
        match tokio::time::timeout(remaining, connect(params, task_monitor)).await {
            Ok(Ok(connected)) => return Ok(connected),
            Ok(Err(error)) if !is_retryable_connect_error(&error) => return Err(error),
            Ok(Err(error)) => {
                tracing::warn!("Migration connection attempt failed: {error}");
                last_error = Some(error);
            }
            Err(_) => {
                tracing::warn!("Migration connection attempt timed out");
                last_error = Some(TransportError::Internal(
                    "connection attempt timed out".to_string(),
                ));
            }
        }
        let delay = policy.delay(attempt).min(deadline.remaining());
        if delay.is_zero() {
            return Err(last_error.unwrap_or_else(|| {
                TransportError::Internal("migration deadline expired".to_string())
            }));
        }
        tokio::time::sleep(delay).await;
        attempt = attempt.saturating_add(1);
    }
}

/// 短期間に連続する GOAWAY 移行を制限する
///
/// 移行先がまた GOAWAY を返す構成では、移行を無制限に繰り返すと再接続ストームになる。
/// 直近 `window` の間に `max_migrations` 回を超える移行を拒否する。
#[derive(Debug)]
pub struct MigrationLimiter {
    /// 制限を数える時間窓
    window: Duration,
    /// 時間窓内で許す移行回数
    max_migrations: usize,
    /// 移行した時刻 (古い順)
    occurred: VecDeque<Instant>,
}

impl MigrationLimiter {
    /// 制限を設定する
    ///
    /// `max_migrations` が 0 の場合は常に拒否する。呼び出し側が移行を無効化したい場合に使う。
    pub fn new(window: Duration, max_migrations: usize) -> Self {
        Self {
            window,
            max_migrations,
            occurred: VecDeque::new(),
        }
    }

    /// 指定時刻で移行してよいかを判定し、よい場合は記録する
    ///
    /// 時間窓の外に出た記録は捨てる。判定と記録を 1 回で行うため、呼び出し側は
    /// 戻り値が `true` のときだけ移行を開始する。
    pub fn check_and_record(&mut self, now: Instant) -> bool {
        while let Some(front) = self.occurred.front() {
            if now.saturating_duration_since(*front) >= self.window {
                self.occurred.pop_front();
            } else {
                break;
            }
        }
        if self.occurred.len() >= self.max_migrations {
            return false;
        }
        self.occurred.push_back(now);
        true
    }
}

/// 接続の確立に必要な設定
///
/// 接続経路ごとの分岐 (native QUIC / WebTransport over HTTP/3 / WebTransport over HTTP/2 /
/// QMux) は [`connect`] が担う。moq-pub / moq-sub は初回接続と GOAWAY 後の移行の双方で
/// 同じ設定を使う。
#[derive(Debug, Clone)]
pub struct ConnectParams<'a> {
    /// 接続先 (authority / path / MSF fragment / C4M 認可トークン)
    pub url: &'a ServerUrl,
    /// 接続経路 (`--transport` と `--url` の `msf` fragment の `connection` から解決済みの値)
    ///
    /// [`Transport::Auto`] の場合は、`connect` が URL とビルドに応じて候補を展開し、
    /// Happy Eyeballs v3 で競争させる。
    pub transport: Transport,
    /// TLS の CA 証明書パス
    ///
    /// 指定すると、その証明書だけをルートとしてサーバー証明書を検証する。省略した場合は
    /// OS のシステムルートストアで検証する。[`Self::insecure`] と同時には指定できない。
    pub cert: Option<&'a str>,
    /// 証明書検証をスキップする (開発用)
    ///
    /// true の場合はサーバー証明書を検証しない。接続時に警告ログを出す。
    /// [`Self::cert`] と同時には指定できない。
    pub insecure: bool,
    /// SETUP の MOQT IMPLEMENTATION (option type 0x07) に載せる実装名
    pub impl_name: &'a str,
    /// WebTransport 経路で datagram の継続受信を有効にする (subscriber が使う)
    pub receive_datagrams: bool,
    /// QMux 経路で送る予定の datagram の上限 (MOQT メッセージ全体の長さ。publisher が使う)
    ///
    /// QMux は DATAGRAM の送信失敗を呼び出し元へ返さないため、ピアが広告した
    /// `max_datagram_frame_size` に収まるかを接続確立時に確認して警告する。`None` は
    /// datagram を送らない経路 (subscriber) を表す。
    pub datagram_max_size: Option<usize>,
    /// Multipath QUIC (RFC 10069) の設定
    ///
    /// [`MultipathOptions::mode`] が [`MultipathMode::Off`](crate::multipath::MultipathMode::Off)
    /// の場合は Multipath QUIC を広告しない。有効にできるのは tobi-quic のネイティブ QUIC
    /// 経路だけであり、WebTransport / QMux を選ぶと [`TransportError::Unsupported`] になる。
    /// Windows では tobi-quic が Multipath QUIC を実装していないため使えない。
    pub multipath: MultipathOptions,
    /// Happy Eyeballs v3 の設定
    ///
    /// [`Transport::Auto`] と、アドレスが複数ある場合の競争に使う。既定値は
    /// draft-ietf-happy-happyeyeballs-v3-05 §10 の推奨値である。
    pub happy_eyeballs: happy_eyeballs::Config,
}

/// URL と接続設定から MOQT セッションを確立する
///
/// 名前解決したアドレスと、`params.transport` で指定した経路の候補を Happy Eyeballs v3 の
/// 手順で試し、最初にトランスポートが確立した候補を採用する。そこで MOQT の SETUP を交換する。
/// SETUP が失敗した場合は、別の候補を試さずエラーを返す。
/// 成功時は [`MoqtClient`](crate::moqt_client::MoqtClient) と data stream を受信する
/// [`StreamAcceptor`](crate::transport::StreamAcceptor) を返す。
///
/// `params.transport` が [`Transport::Auto`] の場合は、URL の `msf` fragment の
/// `connection` とビルドで利用できる経路から候補を展開し、Happy Eyeballs v3
/// (draft-ietf-happy-happyeyeballs-v3-05) の手順で競争させる。候補の並びと成功判定は
/// [`Transport::Auto`] の doc を参照する。競争の勝者だけで SETUP を交換するため、
/// SETUP の失敗は別の候補へのフォールバックを起こさない (draft-ietf-happy-happyeyeballs-v3-05
/// §6.1 は成功判定をアプリケーションに委ねる)。
///
/// 証明書の検証方法は [`ConnectParams`] で決める。`cert` も `insecure` も指定されない
/// 場合は OS のシステムルートストアで検証する。`insecure` を指定したときだけ検証を
/// スキップし、その旨を
/// 試行ごとに警告する。`cert` と `insecure` を同時に指定すると、名前解決より前に
/// エラーを返す。
///
/// # Errors
///
/// 名前解決に失敗した場合、接続の確立に失敗した場合、SETUP の交換に失敗した場合、
/// 証明書の読み込みに失敗した場合、システムルートストアからルートを読み込めない場合、
/// `cert` と `insecure` を同時に指定した場合は [`TransportError`] になる。
pub async fn connect(
    params: &ConnectParams<'_>,
    task_monitor: &tokio_metrics::TaskMonitor,
) -> Result<
    (
        crate::moqt_client::MoqtClient,
        crate::transport::StreamAcceptor,
    ),
    TransportError,
> {
    // CA 証明書での検証と検証のスキップは同時に指定できない。どちらを優先するかを
    // 決められないため、接続を試みる前にエラーにする。
    if params.cert.is_some() && params.insecure {
        return Err(TransportError::InvalidState(
            "cert and insecure cannot be used together; specify either a CA certificate or insecure mode"
                .to_string(),
        ));
    }
    // Multipath QUIC は tobi-quic のネイティブ QUIC 経路だけが使う。設定の誤りを接続の
    // 試行より前に返し、候補の競争で無駄に接続しないようにする。mode が Off の場合も、
    // Multipath 固有の設定が残っていないことを検証する。
    params
        .multipath
        .validate()
        .map_err(TransportError::InvalidState)?;
    if params.multipath.mode.is_enabled() {
        if let Some(reason) = multipath_unavailable_reason() {
            return Err(TransportError::Unsupported(reason.to_string()));
        }
        if params.transport != Transport::Quic {
            return Err(TransportError::Unsupported(
                "multipath QUIC is only available for native QUIC (--transport quic)".to_string(),
            ));
        }
    }
    let addrs = resolve_socket_addrs(&params.url.authority).await?;
    connect_resolved(params, task_monitor, &addrs).await
}

/// アドレス試行で確立した、MOQT SETUP 未交換のトランスポート
enum ConnectedTransport {
    Quic(crate::quic::Connected, crate::quic::StopSendingReceiver),
    WtH3(crate::webtransport_h3::WtSession),
    WtH2(crate::webtransport_h2::WtH2Session),
    Qmux(crate::qmux::QmuxSession),
}

impl ConnectedTransport {
    /// 競争に負けた確立済みの接続を閉じる
    ///
    /// 競争の勝者が決まったとき、`JoinSet` にはすでに確立まで完了した負け候補が残って
    /// いることがある。それを drop するだけでは、経路によっては接続を保持する
    /// タスクが残り、idle timeout までソケットとドライバが残る。ここで明示的に閉じる。
    fn close(self) {
        match self {
            Self::Quic(connection, _) => connection.close(),
            Self::WtH3(session) => session.discard(),
            Self::WtH2(session) => session.discard(),
            Self::Qmux(session) => session.discard(),
        }
    }
}

/// 接続競争の各試行が所有する接続設定
///
/// [`ConnectParams`] は参照を持つため、競争の各タスクへそのまま渡せない。試行に必要な値
/// だけを所有権付きで複製する。認可トークン (`ServerUrl::cat_tokens`) は URL の一部として
/// 複製するが、ログには出さない。
struct AttemptContext {
    /// 接続先 (authority / path / MSF fragment / C4M 認可トークン)
    url: ServerUrl,
    /// TLS の CA 証明書パス
    cert: Option<String>,
    /// 証明書検証をスキップするか
    insecure: bool,
    /// WebTransport 経路で datagram の継続受信を有効にするか
    receive_datagrams: bool,
    /// Multipath QUIC の設定
    multipath: MultipathOptions,
}

/// URL の `msf` fragment の `connection` から q / wt の要求を取り出す
///
/// 戻り値は (`connection=q` があるか, `connection=wt` があるか)。`connection` が無い場合は
/// `(false, false)`。両方が指定されている場合と値が不正な場合はエラーになる
/// (draft-ietf-moq-msf-02 §11.1.1)。
fn connection_requirements(url: &ServerUrl) -> Result<(bool, bool), TransportError> {
    let Some(msf) = url.msf.as_ref() else {
        return Ok((false, false));
    };
    let mut requires_quic = false;
    let mut requires_webtransport = false;
    for connection in msf
        .connection_types()
        .map_err(|e| TransportError::InvalidState(format!("invalid 'connection' parameter: {e}")))?
    {
        match connection {
            MsfConnectionType::Quic => requires_quic = true,
            MsfConnectionType::WebTransport => requires_webtransport = true,
        }
    }
    if requires_quic && requires_webtransport {
        return Err(TransportError::InvalidState(
            "'connection=q' and 'connection=wt' cannot be used together: \
             draft-ietf-moq-msf-02 §11.1.1 mandates a native QUIC connection for 'q' and a \
             WebTransport connection for 'wt'"
                .to_string(),
        ));
    }
    Ok((requires_quic, requires_webtransport))
}

/// [`Transport::Auto`] を具体的なトランスポートの優先順へ展開する
///
/// `connection` が無い場合は QUIC を TCP より優先し (draft-ietf-happy-happyeyeballs-v3-05 §3)、
/// 同一の QUIC 系では層の少ない native QUIC を先にする。`connection=q` は
/// draft-ietf-moq-msf-02 §11.1.1 が Native QUIC を MUST で要求するため `quic` だけにする
/// (QMux は TCP+TLS 上で QUIC のストリーム操作を多重化する経路であり、Native QUIC ではない)。
/// `connection=wt` は `wt-h3` / `wt-h2` を HTTP/3 優先で競争する。
fn candidate_transports(
    requested: Transport,
    url: &ServerUrl,
) -> Result<Vec<Transport>, TransportError> {
    if requested != Transport::Auto {
        return Ok(vec![requested]);
    }

    let (requires_quic, requires_webtransport) = connection_requirements(url)?;
    let mut transports = Vec::new();
    if requires_quic {
        transports.push(Transport::Quic);
    } else if requires_webtransport {
        transports.push(Transport::WtH3);
        transports.push(Transport::WtH2);
    } else {
        transports.push(Transport::Quic);
        transports.push(Transport::WtH3);
        transports.push(Transport::Qmux);
        transports.push(Transport::WtH2);
    }
    Ok(transports)
}

/// トランスポートが確立するまで候補を競争させ、採用した接続でだけ MOQT SETUP を交換する
async fn connect_resolved(
    params: &ConnectParams<'_>,
    task_monitor: &tokio_metrics::TaskMonitor,
    addrs: &[SocketAddr],
) -> Result<
    (
        crate::moqt_client::MoqtClient,
        crate::transport::StreamAcceptor,
    ),
    TransportError,
> {
    use crate::moqt_client::MoqtClient;

    let url = params.url;
    let authority = &url.authority;
    // 自動選択では候補ごとに別の接続を作る (§6)。試行に必要な値は所有権付きで複製し、
    // 各タスクが参照を跨がないようにする
    let context = std::sync::Arc::new(AttemptContext {
        url: url.clone(),
        cert: params.cert.map(str::to_string),
        insecure: params.insecure,
        receive_datagrams: params.receive_datagrams,
        multipath: params.multipath.clone(),
    });
    let config = params.happy_eyeballs.clone().sanitized();
    let transports = candidate_transports(params.transport, url)?;
    let groups: Vec<happy_eyeballs::CandidateGroup<Transport>> = transports
        .iter()
        .map(|&transport| happy_eyeballs::CandidateGroup {
            transport,
            addrs: addrs.to_vec(),
        })
        .collect();
    tracing::debug!(
        config = ?config,
        transports = ?transports,
        "Happy Eyeballs: connection candidates"
    );
    let connection = crate::connection_race::race(
        groups,
        config,
        task_monitor,
        move |candidate| {
            let context = std::sync::Arc::clone(&context);
            let task_monitor = task_monitor.clone();
            async move {
                connect_transport(&context, candidate.transport, candidate.addr, &task_monitor)
                    .await
            }
        },
        |connected| connected.close(),
    )
    .await
    .map_err(|failure| match failure {
        crate::connection_race::RaceFailure::NoCandidates => TransportError::Unsupported(
            "no connection candidate is available for this URL".to_string(),
        ),
        crate::connection_race::RaceFailure::Attempt(error) => error,
        crate::connection_race::RaceFailure::Task(error) => TransportError::Internal(format!(
            "Happy Eyeballs connection attempt task failed: {error}"
        )),
        crate::connection_race::RaceFailure::Internal(reason) => {
            TransportError::Internal(reason.to_string())
        }
    })?;

    // SETUP 失敗をアドレス試行に戻すと、同じ SETUP を別の接続へ送り直してしまう。
    match connection {
        ConnectedTransport::Quic(connection, stop_sending_rx) => {
            MoqtClient::establish_quic(
                connection,
                stop_sending_rx,
                &url.path,
                authority,
                params.impl_name,
                &url.cat_tokens,
                task_monitor,
            )
            .await
        }
        ConnectedTransport::WtH3(session) => {
            MoqtClient::establish_wt(session, params.impl_name, &url.cat_tokens, task_monitor).await
        }
        ConnectedTransport::WtH2(session) => {
            MoqtClient::establish_wt_h2(session, params.impl_name, &url.cat_tokens, task_monitor)
                .await
        }
        ConnectedTransport::Qmux(session) => {
            // 制御ストリームを開いた後はピアのトランスポートパラメータが届いている
            // ため、確立直後に datagram の上限を確認できる。
            let handle = std::sync::Arc::clone(&session.handle);
            let connected = MoqtClient::establish_qmux(
                session,
                &url.path,
                authority,
                params.impl_name,
                &url.cat_tokens,
                task_monitor,
            )
            .await?;
            crate::qmux::warn_if_datagram_too_large(&handle, params.datagram_max_size);
            Ok(connected)
        }
    }
}

/// 指定アドレスの指定トランスポートだけを確立する。MOQT のストリームは開かない
///
/// 競争の各タスクから呼ばれるため、`context` は所有権付きの設定だけを持つ。MOQT 層の
/// 確立 (`establish_*`) は競争の勝者だけで行う。
async fn connect_transport(
    context: &AttemptContext,
    transport: Transport,
    socket_addr: SocketAddr,
    task_monitor: &tokio_metrics::TaskMonitor,
) -> Result<ConnectedTransport, TransportError> {
    let url = &context.url;
    let server_name = host_from_authority(&url.authority);
    match transport {
        Transport::Quic => {
            let (connection, stop_sending_rx) = crate::quic::connect(
                socket_addr,
                server_name,
                context.cert.as_deref(),
                context.insecure,
                &context.multipath,
            )
            .await?;
            Ok(ConnectedTransport::Quic(connection, stop_sending_rx))
        }
        Transport::WtH3 => {
            // draft-ietf-webtrans-http3-17 §3.1 / §9.2: draft-17 の codepoint だけを
            // 広告する。旧 codepoint だけの peer は wait_for_peer_settings が
            // InvalidState で拒否する。
            let mut client_config =
                crate::webtransport_h3::ClientConfig::new(socket_addr, server_name)
                    // :authority は target URI の表記どおり渡す
                    // (draft-ietf-webtrans-http3-16 §3.2)。
                    .authority(&url.authority)
                    .enable_webtransport(crate::webtransport_h3::wt_h3_settings());
            if context.receive_datagrams {
                // subscriber は datagram を継続受信するためバックグラウンドタスクを起動する。
                client_config = client_config.receive_datagrams();
            }
            client_config =
                apply_tls_config_h3(client_config, context.cert.as_deref(), context.insecure)?;
            let session =
                crate::webtransport_h3::WtClient::connect(client_config, &url.path).await?;
            Ok(ConnectedTransport::WtH3(session))
        }
        Transport::WtH2 => {
            let mut client_config =
                crate::webtransport_h2::ClientConfig::new(socket_addr, server_name)
                    // :authority は target URI の表記どおり渡す
                    // (draft-ietf-webtrans-http2-16 §3.2)。
                    .authority(&url.authority);
            if context.receive_datagrams {
                // subscriber は datagram を継続受信するためバックグラウンドタスクを起動する。
                client_config = client_config.receive_datagrams();
            }
            client_config =
                apply_tls_config_h2(client_config, context.cert.as_deref(), context.insecure)?;
            let session =
                crate::webtransport_h2::WtH2Client::connect(client_config, &url.path).await?;
            Ok(ConnectedTransport::WtH2(session))
        }
        Transport::Qmux => {
            let session = crate::qmux::connect(
                socket_addr,
                server_name,
                context.cert.as_deref(),
                context.insecure,
                task_monitor,
            )
            .await?;
            Ok(ConnectedTransport::Qmux(session))
        }
        Transport::Auto => Err(TransportError::Internal(
            "Transport::Auto must be expanded into concrete candidates before connecting"
                .to_string(),
        )),
    }
}

/// WebTransport over HTTP/3 の TLS 設定に CA 証明書または検証のスキップを適用する
///
/// `cert` が指定された場合はその CA 証明書で検証する。`insecure` が true の場合は
/// 証明書検証をスキップする (開発用)。どちらも指定されない場合は WebTransport over
/// HTTP/3 の connect が OS のシステムルートストアで検証する。両方が指定された場合は
/// [`connect`] が事前にエラーにするため、ここでは `insecure` を優先する。
fn apply_tls_config_h3(
    client_config: crate::webtransport_h3::ClientConfig,
    cert: Option<&str>,
    insecure: bool,
) -> Result<crate::webtransport_h3::ClientConfig, TransportError> {
    if insecure {
        // 検証をスキップした旨の警告は、TLS の設定を組み立てるときに一箇所で出す
        return Ok(client_config.insecure());
    }
    match cert {
        Some(cert) => {
            let pem = std::fs::read_to_string(cert).map_err(|e| {
                TransportError::Internal(format!("failed to read certificate '{cert}': {e}"))
            })?;
            Ok(client_config.ca_cert(pem))
        }
        // CA 証明書が無い場合はシステムルートストアで検証する (connect が行う)
        None => Ok(client_config),
    }
}

/// WebTransport over HTTP/2 の TLS 設定に CA 証明書または検証のスキップを適用する
///
/// `cert` が指定された場合はその CA 証明書で検証する。`insecure` が true の場合は
/// 証明書検証をスキップする (開発用)。どちらも指定されない場合は OS のシステムルート
/// ストアで検証する。両方が指定された場合は [`connect`] が事前にエラーにするため、
/// ここでは `insecure` を優先する。
fn apply_tls_config_h2(
    client_config: crate::webtransport_h2::ClientConfig,
    cert: Option<&str>,
    insecure: bool,
) -> Result<crate::webtransport_h2::ClientConfig, TransportError> {
    if insecure {
        // 検証をスキップした旨の警告は、TLS の設定を組み立てるときに一箇所で出す
        return Ok(client_config.insecure());
    }
    match cert {
        Some(cert) => {
            let pem = std::fs::read_to_string(cert).map_err(|e| {
                TransportError::Internal(format!("failed to read certificate '{cert}': {e}"))
            })?;
            Ok(client_config.ca_cert(pem))
        }
        // CA 証明書が無い場合はシステムルートストアで検証する (WtH2Client::connect が行う)
        None => Ok(client_config),
    }
}

/// 名前解決に失敗したことを表すエラーを作る
fn resolve_failed(authority: &str, reason: &str) -> TransportError {
    TransportError::ResolutionFailed(format!("failed to resolve '{authority}': {reason}"))
}

/// 接続先と同じアドレスファミリでローカルソケットを bind するためのアドレス
///
/// `0.0.0.0:0` 固定だと IPv4 ソケットから IPv6 宛に送信できず、IPv6 に解決された
/// 接続先へパケットを送れない。接続先のファミリに合わせる。
pub(crate) fn local_bind_addr(remote: SocketAddr) -> SocketAddr {
    let wildcard = if remote.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    };
    wildcard
        .parse()
        .expect("ワイルドカードアドレスのリテラルを解析できること")
}

#[cfg(test)]
mod tests {
    use super::*;

    use shiguredo_moqt::c4m::cat::MOQT_AUTH_TOKEN_TYPE_CAT;
    use shiguredo_moqt::message_parameter::AuthorizationToken;
    use shiguredo_moqt::name::serialize_namespace;
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

    /// query は path に含める (draft-ietf-moq-transport-23 §9.1.2 PATH)
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

    /// fragment (`#` 以降) は path に含めない (RFC 3986 §3.2)
    #[test]
    fn split_authority_path_stops_at_fragment() {
        assert_eq!(
            split_authority_path("example.com/path#type:value"),
            ("example.com".to_string(), "/path".to_string()),
            "path 付き URL でも fragment を path に含めないこと"
        );
        assert_eq!(
            split_authority_path("example.com#type:value"),
            ("example.com".to_string(), "/".to_string()),
            "authority 直後の fragment は path を `/` にすること"
        );
        assert_eq!(
            split_authority_path("example.com?x=1#type:value"),
            ("example.com".to_string(), "/?x=1".to_string()),
            "query の後の fragment も path に含めないこと"
        );
        assert_eq!(
            split_authority_path("#type:value"),
            (String::new(), "/".to_string()),
            "`#` のみの入力は authority を空文字列、path を `/` にすること"
        );
        assert_eq!(
            split_authority_path("example.com/path#type:value#extra"),
            ("example.com".to_string(), "/path".to_string()),
            "2 個目以降の `#` も fragment として path に含めないこと"
        );
    }

    /// moqt:// URL は query が path に残る
    #[test]
    fn parse_url_parses_moqt_scheme_with_query() {
        let url = parse_url("moqt://127.0.0.1:4443/path?x=1").expect("URL のパースに成功すること");
        assert_eq!(url.authority, "127.0.0.1:4443");
        assert_eq!(url.path, "/path?x=1");
    }

    /// authority と path に含まれる空白と制御文字は拒否する
    ///
    /// RFC 3986 §2 (Characters) は `host` と `path` に空白を含めない。受理すると
    /// 名前解決の失敗や SETUP の `PATH does not conform to RFC 3986` という
    /// 原因の分かりにくいエラーになるため、URL の検証で拒否する。
    #[test]
    fn parse_url_rejects_whitespace_in_authority_and_path() {
        for url in [
            "moqt://exa mple.com/app",
            "moqt://127.0.0.1:4443/a b",
            "moqt://127.0.0.1:4443/a\tb",
            "moqt://example.com/a\nb",
            "moqt://example.com/a\u{0}b",
        ] {
            let err = parse_url(url).expect_err("空白 / 制御文字はエラーになること");
            assert!(
                err.contains("invalid URL"),
                "URL の検証エラーとして返ること: {err}"
            );
            assert!(
                !err.contains(url),
                "入力の URL 全体はエラーに含めないこと: {err}"
            );
        }
    }

    /// 空白のエラーは原因の成分を報告する
    #[test]
    fn parse_url_reports_whitespace_component() {
        let err = parse_url("moqt://exa mple.com/app").expect_err("エラーになること");
        assert!(
            err.contains("'authority' contains a whitespace or control character at byte 3"),
            "authority の位置を報告すること: {err}"
        );

        let err = parse_url("moqt://127.0.0.1:4443/a b").expect_err("エラーになること");
        assert!(
            err.contains("'path' contains a whitespace or control character at byte 2"),
            "path の位置を報告すること: {err}"
        );

        // userinfo を含む authority でも、userinfo の値を反射しない
        let err = parse_url("moqt://alice:s3cr3t@exa mple.com/app").expect_err("エラーになること");
        assert!(
            !err.contains("alice") && !err.contains("s3cr3t"),
            "userinfo を含めないこと: {err}"
        );

        // query を含む path のエラーでも、query の値を反射しない
        let err =
            parse_url("moqt://example.com/app?token=SECRETVALUE x").expect_err("エラーになること");
        assert!(!err.contains("SECRETVALUE"), "query を含めないこと: {err}");
    }

    /// percent-encoding された空白は path として受理する
    ///
    /// RFC 3986 §2.1 の `pct-encoded` は正当な表現であり、空白そのものではない。
    #[test]
    fn parse_url_accepts_percent_encoded_space_in_path() {
        let url = parse_url("moqt://example.com/a%20b").expect("URL のパースに成功すること");
        assert_eq!(url.authority, "example.com");
        assert_eq!(url.path, "/a%20b");
    }

    /// fragment の空白は URL の検証では拒否しない
    ///
    /// fragment は解釈する仕様が意味を定めるため、authority / path と同じ規則を課さない。
    #[test]
    fn parse_url_keeps_whitespace_in_fragment() {
        let url = parse_url("moqt://example.com/app#type:a b")
            .expect("fragment の空白は URL の検証で拒否しないこと");
        assert_eq!(url.authority, "example.com");
        assert_eq!(url.path, "/app");
        assert_eq!(
            url.fragment.as_ref().map(|f| f.value.as_str()),
            Some("a b"),
            "fragment の値はそのまま保持すること"
        );
    }

    /// `moqt://` 以外の scheme はエラーになる (接続経路は --transport で選ぶ)
    #[test]
    fn parse_url_rejects_unknown_scheme() {
        for (url, scheme) in [
            ("ftp://127.0.0.1:4443", Some("ftp")),
            ("https://example.com:443/foo?bar=baz", Some("https")),
            // `:` より前が RFC 3986 §3.1 の scheme の文字種に一致しないため、値を示さない
            ("://host/path", None),
            ("cat=AQID#msf:ns--track", None),
        ] {
            let err = parse_url(url).expect_err("未対応 scheme がエラーになること");
            let expected = match scheme {
                Some(scheme) => format!("unsupported URL scheme '{scheme}' (use moqt://)"),
                None => "unsupported URL scheme (use moqt://)".to_string(),
            };
            assert_eq!(err, expected, "scheme だけを示すこと: {err}");
            assert!(!err.contains("bar=baz"), "query を含めないこと: {err}");
            assert!(!err.contains("AQID"), "token を反射しないこと: {err}");
        }
    }

    /// `//` が無い、または `//` の直後の authority が空の URL は authority の欠落として報告する
    ///
    /// RFC 3986 §3 は `//` の有無で authority の有無が決まり、draft-ietf-moq-transport-23 §6.1 も
    /// `moqt-URI = "moqt" "://" authority path-abempty [ "?" query ]` と `//` を必須にする。
    /// scheme の比較は大文字小文字を区別しない (RFC 3986 §3.1)。
    #[test]
    fn parse_url_rejects_url_without_authority_prefix() {
        for url in [
            "moqt:/app",
            "moqt:example.com/app",
            "MOQT:/app",
            "MOQT:foo",
            "moqt:///app",
        ] {
            let err = parse_url(url).expect_err("`//` が無い URL がエラーになること");
            assert_eq!(
                err,
                "moqt:// URL requires authority (e.g. moqt://localhost:4443)"
            );
            assert!(!err.contains(url), "入力の URL を含めないこと: {err}");
        }
    }

    /// `:` を含まない URL は scheme を取り出せないため未対応 scheme としてエラーになる
    ///
    /// `moqt:` のように scheme が取れて `//` が無い場合は authority の欠落として報告する
    /// (テスト `parse_url_rejects_url_without_authority_prefix` を参照)。
    #[test]
    fn parse_url_rejects_url_without_scheme_delimiter() {
        assert_eq!(
            parse_url("example.com/app").expect_err("scheme が無い URL がエラーになること"),
            "unsupported URL scheme (use moqt://)"
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
        assert_eq!(
            Transport::parse("qmux").expect("qmux は受理されること"),
            Transport::Qmux
        );
        assert_eq!(
            Transport::parse("auto").expect("auto は受理されること"),
            Transport::Auto
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

    /// fragment は path に含めない (draft-ietf-moq-transport-23 §6.1.1)
    #[test]
    fn parse_url_excludes_fragment_from_path() {
        let url =
            parse_url("moqt://example.com/app#type:value").expect("URL のパースに成功すること");
        assert_eq!(url.authority, "example.com");
        assert_eq!(url.path, "/app");
    }

    /// query の後ろの fragment も path に含めない (draft-ietf-moq-transport-23 §9.1.2 PATH)
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
            "moqt:// URL requires authority (e.g. moqt://localhost:4443)"
        );
        // fragment の検証より前に authority の有無を判定する。
        // fragment 自体が不正でも authority のエラーになることを固定する
        for url in ["moqt://?x=1#t:v", "moqt://?x=1#bad", "moqt://#bad"] {
            let err = parse_url(url).expect_err("authority が無い moqt URL がエラーになること");
            assert_eq!(
                err,
                "moqt:// URL requires authority (e.g. moqt://localhost:4443)"
            );
            assert!(!err.contains("x=1"), "query を含めないこと: {err}");
        }
    }

    /// `:` を含まない fragment はエラーになる (draft-ietf-moq-transport-23 §6.1.1)
    #[test]
    fn parse_url_rejects_fragment_without_colon() {
        assert_eq!(
            parse_url("moqt://example.com/app#typevalue")
                .expect_err("fragment に `:` が無い URL がエラーになること"),
            "invalid moqt URI fragment (must be '<type>:<value>')"
        );
    }

    /// fragment のみ (`#` だけ) は type が無いためエラーになる (draft-ietf-moq-transport-23 §6.1.1)
    #[test]
    fn parse_url_rejects_empty_fragment() {
        assert_eq!(
            parse_url("moqt://example.com/app#")
                .expect_err("fragment のみの URL がエラーになること"),
            "invalid moqt URI fragment (must be '<type>:<value>')"
        );
    }

    /// §6.1.1 の文字種の MUST に一致しない fragment type はエラーになる
    #[test]
    fn parse_url_rejects_fragment_type_outside_charset() {
        assert_eq!(
            parse_url("moqt://example.com/app#Type:value")
                .expect_err("大文字の fragment type がエラーになること"),
            "invalid moqt URI fragment type (must be non-empty and consist of ASCII lowercase letters, digits, and hyphens)"
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
            "invalid moqt URI fragment ('#' inside a fragment must be percent-encoded)"
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

    /// SETUP の PATH option に fragment を含めない (draft-ietf-moq-transport-23 §9.1.2)
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

    /// `msf` fragment の cat パラメータを Base64 デコードして保持する
    #[test]
    fn parse_url_extracts_cat_token_from_msf_fragment() {
        let token = Base64::encode_string(&[0x01, 0x02, 0x03]);
        let url = parse_url(&format!(
            "moqt://example.com/app#msf:moq-example--video&cat={token}"
        ))
        .expect("URL のパースに成功すること");
        assert_eq!(url.cat_tokens, vec![vec![0x01, 0x02, 0x03]]);
        // track-identifier の解釈結果も msf に保持される
        let fragment = url.fragment.expect("fragment が保持されること");
        assert_eq!(fragment.fragment_type, "msf");
        assert_eq!(fragment.value, format!("moq-example--video&cat={token}"));
        let msf = url.msf.as_ref().expect("MSF fragment が保持されること");
        assert_eq!(
            serialize_namespace(&msf.namespace),
            "moq-example",
            "track-identifier の namespace が取り出されること"
        );
        assert_eq!(msf.track_name, b"video", "track name が取り出されること");
    }

    /// msf fragment の track-identifier から namespace と track name を取り出す
    ///
    /// MSF URI の track-identifier は §11.1.2 の namespace-name 表現であり、`-` は
    /// namespace フィールドの区切り、`--` が namespace と track name の境界になる。
    /// URI 層の `%XX` はデータバイトとしてデコードする (`%2D` は区切りではない)。
    #[test]
    fn parse_url_extracts_msf_track_identifier() {
        let url = parse_url("moqt://example.com/app#msf:spam-egg--video")
            .expect("URL のパースに成功すること");
        let msf = url.msf.as_ref().expect("MSF fragment が保持されること");
        assert_eq!(serialize_namespace(&msf.namespace), "spam-egg");
        assert_eq!(msf.namespace.fields().len(), 2, "'-' が区切りになること");
        assert_eq!(msf.track_name, b"video");

        // `%XX` はデータバイトへデコードされるため、`%2D` は区切りではなく 1 フィールドの `-` になる
        let url = parse_url("moqt://example.com/app#msf:spam%2Degg--video")
            .expect("URL のパースに成功すること");
        let msf = url.msf.as_ref().expect("MSF fragment が保持されること");
        assert_eq!(
            msf.namespace.fields().to_vec(),
            vec![b"spam-egg".to_vec()],
            "'%2D' がリテラルの '-' として 1 フィールドになること"
        );
    }

    /// `msf_namespace` が fragment の有無と type に応じて namespace を返す
    #[test]
    fn msf_namespace_returns_namespace_only_for_msf_fragment() {
        let url = parse_url("moqt://example.com/app#msf:moq-example--video")
            .expect("URL のパースに成功すること");
        assert_eq!(
            url.msf_namespace().map(serialize_namespace),
            Some("moq-example".to_string())
        );

        // msf 以外の fragment type は track-identifier を解釈しない
        let url = parse_url("moqt://example.com/app#type:moq-example--video")
            .expect("URL のパースに成功すること");
        assert!(url.msf.is_none(), "msf 以外は MSF fragment を持たないこと");
        assert!(url.msf_namespace().is_none());

        // fragment が無い場合も持たない
        let url = parse_url("moqt://example.com/app").expect("URL のパースに成功すること");
        assert!(url.msf.is_none());
        assert!(url.msf_namespace().is_none());
    }

    /// `Transport::as_str` は `Transport::parse` の逆写像である
    #[test]
    fn transport_as_str_is_the_inverse_of_parse() {
        for transport in [
            Transport::Quic,
            Transport::WtH2,
            Transport::Qmux,
            Transport::Auto,
        ] {
            assert_eq!(
                Transport::parse(transport.as_str()).expect("解釈できること"),
                transport,
                "as_str の結果を parse すると元の値に戻ること: {}",
                transport.as_str()
            );
        }

        assert_eq!(
            Transport::parse(Transport::WtH3.as_str()).expect("解釈できること"),
            Transport::WtH3,
            "as_str の結果を parse すると元の値に戻ること: wt-h3"
        );
    }

    /// `connection=q` / `connection=wt` と `--transport qmux` は矛盾する
    ///
    /// draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters) の `q` は native QUIC、
    /// `wt` は WebTransport を MUST で要求する。QMux はどちらでもないため、fragment が
    /// 接続種別を指定しているときは `--transport qmux` を受け付けない
    /// (どちらを優先するかを決められないため)。
    #[test]
    fn resolve_transport_rejects_qmux_with_connection_parameter() {
        for connection in ["q", "wt"] {
            let url = parse_url(&format!(
                "moqt://example.com/app#msf:moq-example--video&connection={connection}"
            ))
            .expect("URL のパースに成功すること");
            let err = resolve_transport(Some(Transport::Qmux), &url)
                .expect_err("矛盾するためエラーになること");
            assert!(
                err.contains("--transport qmux conflicts"),
                "エラーに理由が含まれること: {err}"
            );
        }
    }

    /// `connection` が無ければ `--transport qmux` はそのまま使われる
    #[test]
    fn resolve_transport_accepts_qmux_without_connection_parameter() {
        let url = parse_url("moqt://example.com/app").expect("URL のパースに成功すること");
        assert_eq!(
            resolve_transport(Some(Transport::Qmux), &url).expect("受理されること"),
            Transport::Qmux
        );
    }

    /// `connection=q` は native QUIC を要求する
    ///
    /// draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters) は `connection` で接続種別を
    /// MUST で指定する。`q` は native QUIC である。
    #[test]
    fn resolve_transport_requires_native_quic() {
        let url = parse_url("moqt://example.com/app#msf:moq-example--video&connection=q")
            .expect("URL を解釈できること");
        assert_eq!(
            resolve_transport(None, &url).expect("接続種別を解決できること"),
            Transport::Quic,
            "connection=q は native QUIC になること"
        );
        // `--transport quic` は fragment と整合するためそのまま使う
        assert_eq!(
            resolve_transport(Some(Transport::Quic), &url).expect("接続種別を解決できること"),
            Transport::Quic,
            "整合する明示はそのまま使われること"
        );
        // WebTransport を明示すると 2 つの MUST が両立しない
        let error = resolve_transport(Some(Transport::WtH3), &url)
            .expect_err("connection=q と --transport wt-h3 はエラーになること");

        assert!(
            error.contains("connection=q"),
            "fragment が要求する接続種別が理由として分かること: {error}"
        );
    }

    /// `connection=wt` は WebTransport を要求し、既定が `wt-h3` になる
    ///
    /// `wt` は WebTransport の実装 (HTTP/3 / HTTP/2) を指定しないため、本実装は `wt-h3` を
    /// 既定にする。
    #[test]
    fn resolve_transport_requires_webtransport_and_defaults_to_wt_h3() {
        let url = parse_url("moqt://example.com/app#msf:moq-example--video&connection=wt")
            .expect("URL を解釈できること");

        assert_eq!(
            resolve_transport(None, &url).expect("接続種別を解決できること"),
            Transport::WtH3,
            "connection=wt の既定は wt-h3 であること"
        );

        // WebTransport の実装は `--transport` で選べる
        assert_eq!(
            resolve_transport(Some(Transport::WtH2), &url).expect("接続種別を解決できること"),
            Transport::WtH2,
            "connection=wt と --transport wt-h2 は整合すること"
        );

        assert_eq!(
            resolve_transport(Some(Transport::WtH3), &url).expect("接続種別を解決できること"),
            Transport::WtH3,
            "connection=wt と --transport wt-h3 は整合すること"
        );

        // native QUIC を明示すると 2 つの MUST が両立しない
        let error = resolve_transport(Some(Transport::Quic), &url)
            .expect_err("connection=wt と --transport quic はエラーになること");
        assert!(
            error.contains("connection=wt"),
            "fragment が要求する接続種別が理由として分かること: {error}"
        );
    }

    /// `connection` が無ければ `--transport` の指定を使い、省略時は native QUIC になる
    #[test]
    fn resolve_transport_defaults_to_quic_without_connection() {
        let url = parse_url("moqt://example.com/app#msf:moq-example--video")
            .expect("URL を解釈できること");
        assert_eq!(
            resolve_transport(None, &url).expect("接続種別を解決できること"),
            Transport::Quic,
            "connection が無ければ既定は native QUIC であること"
        );
        assert_eq!(
            resolve_transport(Some(Transport::WtH2), &url).expect("接続種別を解決できること"),
            Transport::WtH2,
            "connection が無ければ明示がそのまま使われること"
        );
        // fragment が無い URL でも同じ
        let url = parse_url("moqt://example.com/app").expect("URL を解釈できること");
        assert_eq!(
            resolve_transport(None, &url).expect("接続種別を解決できること"),
            Transport::Quic,
            "fragment が無ければ既定は native QUIC であること"
        );

        assert_eq!(
            resolve_transport(Some(Transport::WtH3), &url).expect("接続種別を解決できること"),
            Transport::WtH3,
            "fragment が無ければ明示がそのまま使われること"
        );
    }

    /// `--transport auto` は `connection` の要求と両立する
    ///
    /// `connection=q` は native QUIC を MUST で要求し、`connection=wt` は WebTransport を
    /// MUST で要求する (draft-ietf-moq-msf-02 §11.1.1)。どちらも `auto` の候補集合を
    /// その要求の範囲に狭めるだけで、`auto` 自体は拒否しない。
    #[test]
    fn resolve_transport_accepts_auto_with_connection_requirements() {
        let url = parse_url("moqt://example.com/app#msf:moq-example--video&connection=q")
            .expect("URL を解釈できること");
        assert_eq!(
            resolve_transport(Some(Transport::Auto), &url).expect("auto を解決できること"),
            Transport::Auto,
            "connection=q でも auto を受け付けること"
        );

        let url = parse_url("moqt://example.com/app#msf:moq-example--video&connection=wt")
            .expect("URL を解釈できること");
        assert_eq!(
            resolve_transport(Some(Transport::Auto), &url).expect("auto を解決できること"),
            Transport::Auto,
            "connection=wt でも auto を受け付けること"
        );

        let url = parse_url("moqt://example.com/app").expect("URL を解釈できること");
        assert_eq!(
            resolve_transport(Some(Transport::Auto), &url).expect("auto を解決できること"),
            Transport::Auto,
            "connection が無い場合も auto をそのまま返すこと"
        );
    }

    /// `auto` の候補集合は `connection` で決まる
    ///
    /// `connection` が無い場合は QUIC を TCP より優先し、`connection=q` は native QUIC だけ、
    /// `connection=wt` は `wt-h3` / `wt-h2` にする。
    #[test]
    fn candidate_transports_expands_auto() {
        let url = parse_url("moqt://example.com/app").expect("URL を解釈できること");
        let mut expected = vec![Transport::Quic];

        expected.push(Transport::WtH3);

        expected.push(Transport::Qmux);
        expected.push(Transport::WtH2);
        assert_eq!(
            candidate_transports(Transport::Auto, &url).expect("候補を展開できること"),
            expected,
            "connection が無い場合は QUIC、WebTransport over HTTP/3、QMux、\
             WebTransport over HTTP/2 の順に競争すること"
        );

        let url = parse_url("moqt://example.com/app#msf:moq-example--video&connection=q")
            .expect("URL を解釈できること");
        assert_eq!(
            candidate_transports(Transport::Auto, &url).expect("候補を展開できること"),
            vec![Transport::Quic],
            "connection=q は Native QUIC だけを候補にすること"
        );

        let url = parse_url("moqt://example.com/app#msf:moq-example--video&connection=wt")
            .expect("URL を解釈できること");
        assert_eq!(
            candidate_transports(Transport::Auto, &url).expect("候補を展開できること"),
            vec![Transport::WtH3, Transport::WtH2],
            "connection=wt は wt-h3 / wt-h2 を候補にすること"
        );

        // 具体的なトランスポートは展開しない
        let url = parse_url("moqt://example.com/app").expect("URL を解釈できること");
        assert_eq!(
            candidate_transports(Transport::Qmux, &url).expect("候補を展開できること"),
            vec![Transport::Qmux],
            "具体的なトランスポートはそのまま 1 候補になること"
        );
    }

    /// `auto` でも `connection=q` と `connection=wt` の同時指定はエラーにする
    #[test]
    fn candidate_transports_rejects_conflicting_connections() {
        let url =
            parse_url("moqt://example.com/app#msf:moq-example--video&connection=q&connection=wt")
                .expect("URL を解釈できること");
        let error = candidate_transports(Transport::Auto, &url)
            .expect_err("q と wt の同時指定はエラーになること");
        assert!(
            error.to_string().contains("cannot be used together"),
            "両立しないことが理由として分かること: {error}"
        );
    }

    /// `connection=q` と `connection=wt` の同時指定はエラーにする
    ///
    /// 2 つの MUST が両立しないため、どちらかを選ぶことはできない。§11.1.1 の「複数指定は
    /// union を処理する」は値域を対象とした規則であり、単一値の `connection` には当てはまらない。
    #[test]
    fn resolve_transport_rejects_conflicting_connections() {
        let url =
            parse_url("moqt://example.com/app#msf:moq-example--video&connection=q&connection=wt")
                .expect("URL を解釈できること");
        let error =
            resolve_transport(None, &url).expect_err("q と wt の同時指定はエラーになること");
        assert!(
            error.contains("cannot be used together"),
            "両立しないことが理由として分かること: {error}"
        );
        assert!(
            resolve_transport(Some(Transport::Quic), &url).is_err(),
            "明示があっても q と wt の同時指定はエラーになること"
        );
    }

    /// 同じ `connection` の重複はエラーにせず、その接続種別を使う
    ///
    /// 重複は同じ要求の反復であり、`q` と `wt` の併記 (両立しない) とは異なる。
    #[test]
    fn resolve_transport_accepts_duplicated_connection_values() {
        let url =
            parse_url("moqt://example.com/app#msf:moq-example--video&connection=q&connection=q")
                .expect("URL を解釈できること");
        assert_eq!(
            resolve_transport(None, &url).expect("接続種別を解決できること"),
            Transport::Quic,
            "connection=q の重複は native QUIC になること"
        );

        let url =
            parse_url("moqt://example.com/app#msf:moq-example--video&connection=wt&connection=wt")
                .expect("URL を解釈できること");

        assert_eq!(
            resolve_transport(None, &url).expect("接続種別を解決できること"),
            Transport::WtH3,
            "connection=wt の重複は WebTransport (既定 wt-h3) になること"
        );

        assert_eq!(
            resolve_transport(Some(Transport::WtH2), &url).expect("接続種別を解決できること"),
            Transport::WtH2,
            "重複していても整合する明示はそのまま使われること"
        );
    }

    /// `connection` の不正値はエラーにする
    ///
    /// 値の検証は [`MsfFragment::connection_types`] が行う。
    #[test]
    fn resolve_transport_rejects_invalid_connection_value() {
        let url = parse_url("moqt://example.com/app#msf:moq-example--video&connection=xx")
            .expect("URL を解釈できること");
        let error =
            resolve_transport(None, &url).expect_err("connection の不正値はエラーになること");
        assert!(
            error.contains("invalid connection parameter value"),
            "connection の値の検証エラーとして分かること: {error}"
        );
    }

    /// `--namespace` を指定した場合は §8.8 表現として解釈する
    #[test]
    fn resolve_namespace_uses_explicit_option() {
        let url = parse_url("moqt://example.com/app#msf:spam-egg--video")
            .expect("URL のパースに成功すること");
        let namespace = resolve_namespace(Some("moq-example"), &url).expect("解決に成功すること");
        assert_eq!(
            serialize_namespace(&namespace),
            "moq-example",
            "指定した namespace が msf fragment より優先されること"
        );
    }

    /// `--namespace` が無い場合は `--url` の msf fragment の namespace を使う
    #[test]
    fn resolve_namespace_falls_back_to_msf_fragment() {
        let url = parse_url("moqt://example.com/app#msf:spam-egg--video&cat=AQID")
            .expect("URL のパースに成功すること");
        let namespace = resolve_namespace(None, &url).expect("解決に成功すること");
        assert_eq!(serialize_namespace(&namespace), "spam-egg");
        assert_eq!(namespace.fields().len(), 2);
    }

    /// `--namespace` も msf fragment も無い場合はエラーになる
    #[test]
    fn resolve_namespace_requires_namespace_or_msf_fragment() {
        for text in [
            // fragment が無い
            "moqt://example.com/app",
            // msf 以外の fragment type
            "moqt://example.com/app#type:moq-example--video",
        ] {
            let url = parse_url(text).expect("URL のパースに成功すること");
            let err = resolve_namespace(None, &url).expect_err("エラーになること");
            assert!(
                err.contains("--namespace is required"),
                "ケース {text}: {err}"
            );
        }
    }

    /// msf fragment の namespace が 0 フィールドの場合はエラーになる
    ///
    /// 0 フィールドの namespace は `--namespace` の空文字と同じく moq-pub / moq-sub が扱わない。
    #[test]
    fn resolve_namespace_rejects_msf_fragment_without_namespace_fields() {
        let url =
            parse_url("moqt://example.com/app#msf:--video").expect("URL のパースに成功すること");
        let err = resolve_namespace(None, &url).expect_err("エラーになること");
        assert!(err.contains("at least one field"), "エラー: {err}");
    }

    /// `--namespace` の空文字と §8.8 として不正な値はエラーになる
    #[test]
    fn resolve_namespace_rejects_invalid_explicit_option() {
        let url = parse_url("moqt://example.com/app").expect("URL のパースに成功すること");
        let err = resolve_namespace(Some(""), &url).expect_err("空文字はエラーになること");
        assert!(err.contains("must not be empty"), "エラー: {err}");
        let err = resolve_namespace(Some("moq/example"), &url)
            .expect_err("§8.8 として不正な値はエラーになること");
        assert!(err.contains("§8.8"), "エラー: {err}");
    }

    /// パディング無しの標準 Base64 と base64url も cat として受理する
    #[test]
    fn parse_url_accepts_unpadded_and_url_safe_base64_for_cat() {
        let bytes = [0xfb, 0xff];
        for text in [
            Base64Unpadded::encode_string(&bytes),
            Base64UrlUnpadded::encode_string(&bytes),
        ] {
            let url = parse_url(&format!(
                "moqt://example.com/app#msf:moq-example--video&cat={text}"
            ))
            .unwrap_or_else(|e| panic!("{text} のパースに失敗した: {e}"));
            assert_eq!(url.cat_tokens, vec![bytes.to_vec()], "ケース {text}");
        }
    }

    /// percent-encoding された cat パラメータもデコードする (RFC 3986 §2.1)
    #[test]
    fn parse_url_decodes_percent_encoded_cat_parameter() {
        // `%2B%2F8` は標準 Base64 の `+/8` (パディング無し) の percent-encoding
        let url = parse_url("moqt://example.com/app#msf:moq-example--video&cat=%2B%2F8")
            .expect("URL のパースに成功すること");
        assert_eq!(url.cat_tokens, vec![vec![0xfb, 0xff]]);
    }

    /// cat パラメータは出現順を保ち、同一トークンは 1 つに畳む
    #[test]
    fn parse_url_keeps_cat_tokens_in_order_and_deduplicates() {
        let first = Base64UrlUnpadded::encode_string(&[0x01]);
        let second = Base64UrlUnpadded::encode_string(&[0x02]);
        let url = parse_url(&format!(
            "moqt://example.com/app#msf:moq-example--video&cat={first}&cat={second}&cat={first}"
        ))
        .expect("URL のパースに成功すること");
        assert_eq!(url.cat_tokens, vec![vec![0x01], vec![0x02]]);
    }

    /// cat パラメータが無い msf fragment でもパースできる
    #[test]
    fn parse_url_accepts_msf_fragment_without_cat() {
        let url = parse_url("moqt://example.com/app#msf:moq-example--video&connection=q")
            .expect("URL のパースに成功すること");
        assert!(
            url.cat_tokens.is_empty(),
            "cat 無しではトークンを保持しないこと"
        );
        let fragment = url.fragment.expect("fragment が保持されること");
        assert_eq!(fragment.fragment_type, "msf");
    }

    /// msf 以外の fragment type は cat らしき値があっても解釈しない
    #[test]
    fn parse_url_ignores_cat_outside_msf_fragment() {
        let url =
            parse_url("moqt://example.com/app#type:cat=AQID").expect("URL のパースに成功すること");
        assert!(url.cat_tokens.is_empty());
    }

    /// 旧予約パラメータ `c4m` は認可トークンとして取り出さないこと
    ///
    /// draft-ietf-moq-msf-02 Appendix A.1.7 が `c4m` の `cat` への改名を記録した。予約でない
    /// パラメータを認可トークンの供給元にしない fail-closed の方針を取る。
    #[test]
    fn parse_url_does_not_extract_legacy_c4m_parameter() {
        let url = parse_url("moqt://example.com/app#msf:moq-example--video&c4m=AQID")
            .expect("URL のパースに成功すること");
        assert!(
            url.cat_tokens.is_empty(),
            "旧 c4m パラメータを認可トークンとして扱わないこと"
        );
    }

    /// msf fragment が MSF §11.1 の ABNF に一致しない場合はエラーになる
    #[test]
    fn parse_url_rejects_invalid_msf_fragment() {
        for value in [
            // track-identifier が無い
            "",
            // パラメータに `=` が無い
            "moq-example--video&cat",
            // `&` を含む値はパラメータ区切りとして解釈され、`=` の無いパラメータになる
            "moq-example--video&cat=AQ&ID",
        ] {
            let url = format!("moqt://example.com/app#msf:{value}");
            let err = parse_url(&url).expect_err("エラーになること");
            assert!(
                err.contains("invalid MSF fragment"),
                "URL {url} の検証に失敗した: {err}"
            );
        }
    }

    /// cat の値が Base64 でない、または空の場合はエラーになる
    #[test]
    fn parse_url_rejects_invalid_cat_token() {
        for text in ["!!!", ""] {
            let url = format!("moqt://example.com/app#msf:moq-example--video&cat={text}");
            let err = parse_url(&url).expect_err("エラーになること");
            assert!(
                err.contains("invalid cat parameter"),
                "URL {url} の検証に失敗した: {err}"
            );
        }
    }

    /// URL の解析エラーは query / fragment の実値を含まない
    ///
    /// query と fragment は認可トークン (connection URI のパラメータは
    /// draft-ietf-moq-msf-02 §11.5.2、fragment の `cat` は §11.1.1) を含み得て、解析エラーは
    /// ログや CLI の標準エラーに出るため、実値をエラーメッセージに埋め込まない。
    #[test]
    fn parse_url_errors_do_not_leak_query_or_fragment_values() {
        for (url, secrets) in [
            (
                "moqt://exa mple.com/app?token=SECRETVALUE",
                &["SECRETVALUE"][..],
            ),
            (
                "moqt://example.com/app?token=SECRETVALUE#badvalue",
                &["SECRETVALUE", "badvalue"][..],
            ),
            (
                "moqt://example.com/app?token=SECRETVALUE#",
                &["SECRETVALUE"][..],
            ),
            (
                "moqt://example.com/app?token=SECRETVALUE#Type:value",
                &["SECRETVALUE"][..],
            ),
            (
                "moqt://example.com/app?token=SECRETVALUE#a:b#c:d",
                &["SECRETVALUE"][..],
            ),
            (
                "moqt://example.com/app?token=SECRETVALUE#msf:",
                &["SECRETVALUE"][..],
            ),
            (
                "moqt://example.com/app?token=SECRETVALUE#msf:ns--catalog&cat",
                &["SECRETVALUE"][..],
            ),
            (
                "moqt://example.com/app#msf:ns--catalog&cat=SECRETVALUE!",
                &["SECRETVALUE"][..],
            ),
        ] {
            let err = parse_url(url).expect_err("エラーになること");
            for secret in secrets {
                assert!(!err.contains(secret), "{url}: 実値を含まないこと: {err}");
            }
        }
    }

    /// Debug 出力に実値が文字列としても十進のバイト列としても含まれないことを確認する
    ///
    /// 失敗時に Debug 出力をそのまま出すと、漏えいした実値を CI のログへ書いてしまうため、
    /// どの値が漏れたかだけを示す。空または 1 バイトの `secret` は無関係な長さ表示などと
    /// 衝突するため検査しない。呼び出し側は 2 バイト以上の値を渡すこと。
    fn assert_hidden(debug: &str, what: &str, secret: &[u8]) {
        if secret.len() <= 1 {
            return;
        }
        // 認可トークンは UTF-8 でないバイト列でもよいため、文字列として扱える場合だけ比較する
        if let Ok(text) = core::str::from_utf8(secret) {
            assert!(
                !debug.contains(text),
                "{what} の文字列が Debug 出力に含まれている"
            );
        }
        let list = format!("{secret:?}");
        let inner = list
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .expect("バイト列の Debug 表現は [...] の形になること");
        assert!(
            !debug.contains(inner),
            "{what} の十進のバイト列が Debug 出力に含まれている"
        );
    }

    /// ServerUrl の Debug は authority の userinfo を表示しない
    ///
    /// `parse_url` は userinfo を含む authority を拒否しないため、`Debug` から漏れないように
    /// `@` より前を実値で表示せず、バイト長だけを表示する (RFC 3986 §3.2.1)。
    #[test]
    fn server_url_debug_hides_userinfo() {
        let url = parse_url("moqt://alice:s3cr3t@relay.example.com:443/live")
            .expect("URL のパースに成功すること");
        let debug = format!("{url:?}");

        // 実値は文字列でも十進のバイト列でも現れない。ユーザ名とパスワードも
        // 個別に検査し、片方だけが漏れる退行も検出する
        assert_hidden(&debug, "userinfo", b"alice:s3cr3t");
        assert_hidden(&debug, "userinfo のユーザ名", b"alice");
        assert_hidden(&debug, "userinfo のパスワード", b"s3cr3t");
        // host と port は機密値でないため表示する
        assert!(
            debug.contains("authority: \"relay.example.com:443\""),
            "userinfo より後ろの host と port は表示すること"
        );
        assert!(
            debug.contains("userinfo_bytes: 12"),
            "userinfo のバイト長を表示すること"
        );

        // バイト長は文字数ではなくバイト数であること
        let url = parse_url("moqt://利用者:秘密@relay.example.com:443/live")
            .expect("URL のパースに成功すること");
        let debug = format!("{url:?}");
        assert_hidden(&debug, "userinfo のユーザ名", "利用者".as_bytes());
        assert_hidden(&debug, "userinfo のパスワード", "秘密".as_bytes());
        // "利用者:秘密" は 9 + 1 + 6 = 16 バイト (文字数は 6)
        assert!(
            debug.contains("userinfo_bytes: 16"),
            "userinfo のバイト長を表示すること"
        );
        assert!(
            debug.contains("path: \"/live\""),
            "path は従来どおり表示すること"
        );
    }

    /// userinfo を含まない authority の表示は変わらないこと
    #[test]
    fn server_url_debug_keeps_authority_without_userinfo() {
        let url =
            parse_url("moqt://relay.example.com:443/live").expect("URL のパースに成功すること");
        let debug = format!("{url:?}");
        assert!(
            debug.contains("authority: \"relay.example.com:443\""),
            "userinfo が無ければ authority をそのまま表示すること"
        );
        assert!(
            !debug.contains("userinfo_bytes"),
            "userinfo が無い場合はフィールドを表示しないこと"
        );
    }

    /// authority に複数の `@` がある場合は最後の `@` より前を隠すこと
    ///
    /// userinfo の区切りは最後の `@` とする。最初の `@` で区切るとパスワードの後半が
    /// 表示されてしまう。
    #[test]
    fn server_url_debug_hides_userinfo_before_the_last_at_sign() {
        let url = parse_url("moqt://alice:p@ss@relay.example.com:443/live")
            .expect("URL のパースに成功すること");
        let debug = format!("{url:?}");
        assert_hidden(&debug, "userinfo", b"alice:p@ss");
        assert_hidden(&debug, "パスワードの一部", b"p@ss");
        assert!(
            debug.contains("authority: \"relay.example.com:443\""),
            "最後の `@` より後ろの host と port は表示すること"
        );
        assert!(
            debug.contains("userinfo_bytes: 10"),
            "userinfo 全体のバイト長を表示すること"
        );
    }

    /// `@` が先頭・末尾にある場合も実値を表示しないこと
    #[test]
    fn server_url_debug_hides_userinfo_at_edges() {
        // `@` が先頭: userinfo は空、host はそのまま
        let url =
            parse_url("moqt://@relay.example.com:443/live").expect("URL のパースに成功すること");
        let debug = format!("{url:?}");
        assert!(
            debug.contains("authority: \"relay.example.com:443\""),
            "先頭の `@` より後ろを host として表示すること"
        );
        assert!(
            debug.contains("userinfo_bytes: 0"),
            "空の userinfo のバイト長 (0) を表示すること"
        );
        // `@` が末尾: authority は userinfo だけになる
        let url = parse_url("moqt://relay.example.com@/live").expect("URL のパースに成功すること");
        let debug = format!("{url:?}");
        assert_hidden(&debug, "userinfo", b"relay.example.com");
        assert!(
            debug.contains("authority: \"\""),
            "`@` より後ろが空なら authority も空にすること"
        );
        assert!(
            debug.contains("userinfo_bytes: 17"),
            "userinfo のバイト長を表示すること"
        );
    }

    /// ServerUrl の Debug は query / fragment / cat トークンの実値を表示しない
    ///
    /// ログ出力で `{:?}` を使っても認可トークンが漏れないよう、伏せたフィールドはバイト長を
    /// 表示する。userinfo を含まない authority と query より前の path は非機密のため
    /// 従来どおり表示する。
    #[test]
    fn server_url_debug_hides_token_values() {
        let token = [222u8, 173, 190, 239];
        let text = Base64::encode_string(&token);
        let fragment_value = format!("moq-example--video&cat={text}");
        // query は `?` 以降 (`token=SECRETQUERY` の 17 バイト)
        let url = parse_url(&format!(
            "moqt://example.com/app?token=SECRETQUERY#msf:{fragment_value}"
        ))
        .expect("URL のパースに成功すること");
        let debug = format!("{url:?}");

        // 実値は文字列でも十進のバイト列でも現れない。query と fragment は「その中の
        // 秘匿対象の値」と「全体の文字列」を別々に検査する
        assert_hidden(&debug, "query の値", b"SECRETQUERY");
        assert_hidden(&debug, "query 全体", b"token=SECRETQUERY");
        assert_hidden(&debug, "fragment", fragment_value.as_bytes());
        assert_hidden(&debug, "cat トークンの Base64 表現", text.as_bytes());
        assert_hidden(&debug, "cat トークン", &token);

        // 伏せたフィールドはバイト長を表示する
        assert!(
            debug.contains("query: Some(17)"),
            "query のバイト長を表示すること: {debug}"
        );
        assert!(
            debug.contains("fragment: Some("),
            "fragment を表示すること: {debug}"
        );
        assert!(debug.contains("msf: Some("), "msf を表示すること: {debug}");
        assert!(
            debug.contains("value_bytes: 31"),
            "fragment の value のバイト長を表示すること: {debug}"
        );
        assert!(
            debug.contains("cat_token_count: 1"),
            "cat の件数を表示すること: {debug}"
        );
        assert!(
            debug.contains("cat_token_bytes: [4]"),
            "cat の各バイト長を表示すること: {debug}"
        );
        // 非機密のフィールドは従来どおり表示する
        assert!(
            debug.contains("authority: \"example.com\""),
            "authority は表示すること: {debug}"
        );
        assert!(
            debug.contains("path: \"/app\""),
            "path は query より前だけを表示すること: {debug}"
        );
        // msf fragment の namespace と track name は非機密のため、十進のバイト列として表示する
        assert!(
            debug.contains("track_name: [118, 105, 100, 101, 111]"),
            "track name は非機密として表示すること: {debug}"
        );
    }

    /// query と fragment が無い URL の Debug は None と空の件数を表示する
    #[test]
    fn server_url_debug_reports_absent_query_and_fragment() {
        let url = parse_url("moqt://example.com/app").expect("URL のパースに成功すること");
        let debug = format!("{url:?}");
        assert!(
            debug.contains("query: None"),
            "query は None になること: {debug}"
        );
        assert!(
            debug.contains("fragment: None"),
            "fragment は None になること: {debug}"
        );
        assert!(
            debug.contains("msf: None"),
            "msf は None になること: {debug}"
        );
        assert!(
            debug.contains("cat_token_count: 0"),
            "cat の件数は 0 になること: {debug}"
        );
        assert!(
            debug.contains("cat_token_bytes: []"),
            "cat の各バイト長は空になること: {debug}"
        );
        assert!(
            debug.contains("path: \"/app\""),
            "path は表示すること: {debug}"
        );
    }

    /// query に複数の `?` がある URL は最初の `?` 以降をまとめて隠す
    #[test]
    fn server_url_debug_hides_query_after_the_first_question_mark() {
        let url = parse_url("moqt://example.com/app?token=SECRET1?token=SECRET2")
            .expect("URL のパースに成功すること");
        let debug = format!("{url:?}");
        // 漏えい検出を長さのアサートより先に行い、失敗時に secret を印字しない
        assert_hidden(&debug, "query", b"SECRET1");
        assert_hidden(&debug, "query", b"SECRET2");
        // `token=SECRET1?token=SECRET2` の 27 バイト
        assert!(
            debug.contains("query: Some(27)"),
            "query 全体のバイト長を表示すること: {debug}"
        );
        assert!(
            debug.contains("path: \"/app\""),
            "path は最初の `?` より前だけを表示すること: {debug}"
        );
    }

    /// cat が複数ある場合も件数と各トークンのバイト長を表示する
    #[test]
    fn server_url_debug_reports_each_cat_token_length() {
        let url = parse_url("moqt://example.com/app#msf:moq-example--video&cat=AQID&cat=BAUG")
            .expect("URL のパースに成功すること");
        assert_eq!(url.cat_tokens.len(), 2, "トークンは 2 個になること");
        let debug = format!("{url:?}");
        assert_hidden(&debug, "1 個目の cat トークンの Base64 表現", b"AQID");
        assert_hidden(&debug, "2 個目の cat トークンの Base64 表現", b"BAUG");
        assert_hidden(&debug, "1 個目の cat トークン", &[1u8, 2, 3]);
        assert_hidden(&debug, "2 個目の cat トークン", &[4u8, 5, 6]);
        assert!(
            debug.contains("cat_token_count: 2"),
            "cat の件数を表示すること: {debug}"
        );
        assert!(
            debug.contains("cat_token_bytes: [3, 3]"),
            "cat の各トークンのバイト長を表示すること: {debug}"
        );
    }

    /// msf 以外の fragment は type を表示し、value の実値を表示しない
    #[test]
    fn server_url_debug_hides_non_msf_fragment_value() {
        let url = parse_url("moqt://example.com/app#other:SECRETVALUE")
            .expect("URL のパースに成功すること");
        let debug = format!("{url:?}");
        assert_hidden(&debug, "fragment の value", b"SECRETVALUE");
        assert!(
            debug.contains("fragment_type: \"other\""),
            "fragment type は表示すること: {debug}"
        );
        assert!(
            debug.contains("msf: None"),
            "msf は None になること: {debug}"
        );
    }

    /// MoqtFragment の Debug は value の実値を表示しない
    #[test]
    fn moqt_fragment_debug_hides_value() {
        let fragment = MoqtFragment {
            fragment_type: "msf".to_string(),
            value: "SECRETFRAGMENT".to_string(),
        };
        let debug = format!("{fragment:?}");
        // 漏えい検出を長さのアサートより先に行い、失敗時に secret を印字しない
        assert_hidden(&debug, "fragment の value", b"SECRETFRAGMENT");
        assert!(
            debug.contains("fragment_type: \"msf\""),
            "fragment type は表示すること: {debug}"
        );
        assert!(
            debug.contains("value_bytes: 14"),
            "value のバイト長を表示すること: {debug}"
        );
    }

    /// MigrationTarget と ConnectParams の Debug も URL の秘匿を通ること
    #[test]
    fn migration_and_connect_params_debug_hide_url_secrets() {
        let token = [222u8, 173, 190, 239];
        let text = Base64::encode_string(&token);
        let fragment_value = format!("moq-example--video&cat={text}");
        let url = parse_url(&format!(
            "moqt://alice:s3cr3t@example.com/app?token=SECRETQUERY#msf:{fragment_value}"
        ))
        .expect("URL のパースに成功すること");
        let target = MigrationTarget {
            scope: MigrationScope::Session,
            url: url.clone(),
        };
        let debug = format!("{target:?}");
        assert_hidden(&debug, "userinfo", b"alice:s3cr3t");
        assert_hidden(&debug, "query の値", b"SECRETQUERY");
        assert_hidden(&debug, "fragment", fragment_value.as_bytes());
        assert_hidden(&debug, "cat トークンの Base64 表現", text.as_bytes());
        assert_hidden(&debug, "cat トークン", &token);

        let params = ConnectParams {
            url: &url,
            transport: Transport::Quic,
            cert: None,
            // 接続しないため検証の有無は結果に影響しない
            insecure: false,
            impl_name: "impl",
            receive_datagrams: false,
            datagram_max_size: None,
            multipath: MultipathOptions::default(),
            happy_eyeballs: happy_eyeballs::Config::default(),
        };
        let debug = format!("{params:?}");
        assert_hidden(&debug, "userinfo", b"alice:s3cr3t");
        assert_hidden(&debug, "query の値", b"SECRETQUERY");
        assert_hidden(&debug, "fragment", fragment_value.as_bytes());
        assert_hidden(&debug, "cat トークンの Base64 表現", text.as_bytes());
        assert_hidden(&debug, "cat トークン", &token);
        assert!(
            debug.contains("authority: \"example.com\""),
            "authority は表示すること: {debug}"
        );
    }

    /// fragment と cat の解析エラーは入力を含まない
    ///
    /// これらは公開 API のエラーとして伝播するため、実値を含めないことを直接固定する。
    #[test]
    fn fragment_parser_errors_do_not_include_input() {
        let err =
            parse_moqt_fragment("badvalue").expect_err("`:` の無い fragment はエラーになること");
        assert!(!err.contains("badvalue"), "fragment を含めないこと: {err}");

        let err =
            parse_moqt_fragment("bad#value:x").expect_err("`#` を含む fragment はエラーになること");
        assert!(!err.contains("bad#value"), "fragment を含めないこと: {err}");

        let err = parse_msf_value("ns--catalog&catSECRET")
            .expect_err("`=` の無いパラメータはエラーになること");
        assert!(!err.contains("catSECRET"), "要素を含めないこと: {err}");

        let err = decode_cat_token("SECRET!").expect_err("Base64 でない値はエラーになること");
        assert!(!err.contains("SECRET"), "トークンを含めないこと: {err}");
    }

    /// SETUP に C4M トークンを AUTHORIZATION_TOKEN (Token Type CAT) として含める
    #[test]
    fn setup_options_include_c4m_authorization_tokens() {
        let url = parse_url("moqt://example.com/app#msf:moq-example--video&cat=AQID")
            .expect("URL のパースに成功すること");
        let options = build_setup_options(
            Some(&url.path),
            Some(&url.authority),
            "impl",
            &url.cat_tokens,
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

    /// C4M トークン無しでは AUTHORIZATION_TOKEN を含めない
    #[test]
    fn setup_options_omit_authorization_token_without_tokens() {
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

    /// ポート省略の authority は既定ポート 443 を使う (draft-ietf-moq-transport-23 §6.1.2)
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

    /// `[` `]` の中身が IPv6 アドレスでない authority はエラーになる
    ///
    /// RFC 3986 §3.2.2 の IP-literal は IPv6address または IPvFuture に限られる。
    /// `[example.com]` を reg-name として受理すると、無関係な外部ホストへの接続になる。
    /// IPvFuture は受け付けない (既知の version flag を持たないアドレス機構は非対応)。
    /// エラーメッセージに理由が含まれることも固定する。
    #[test]
    fn authority_parts_rejects_non_ipv6_ip_literal() {
        for authority in [
            "[example.com]",
            "[example.com]:4443",
            "[v1.fe80::]",
            "[::1x]",
        ] {
            let err = authority_parts(authority).expect_err("エラーになること");
            assert!(
                matches!(err, TransportError::InvalidAuthority(_)),
                "authority の解釈失敗として返ること: {err}"
            );
            assert!(
                err.to_string().contains("must be an IPv6 address"),
                "理由が含まれること: {err}"
            );
        }
    }

    /// ポート 0 の authority はエラーになる
    ///
    /// RFC 3986 §3.2.3 の `port = *DIGIT` は 0 も許すが、接続先として使えない。
    #[test]
    fn authority_parts_rejects_port_zero() {
        for authority in ["127.0.0.1:0", "[::1]:0", "relay.example.com:0"] {
            let err = authority_parts(authority).expect_err("エラーになること");
            assert!(
                err.to_string().contains("port 0 cannot be used"),
                "理由が含まれること: {err}"
            );
        }
    }

    /// userinfo を含む authority は名前解決に回さずエラーになる
    ///
    /// draft-ietf-moq-transport-23 §6.1 は userinfo に言及せず、RFC 3986 §3.2.1 は
    /// reference 中の userinfo を reject する選択を許す。
    #[test]
    fn authority_parts_rejects_userinfo() {
        for (authority, user, password) in [
            ("alice:s3cr3t@127.0.0.1:4433", "alice", "s3cr3t"),
            ("alice:s3cr3t@relay.example.com", "alice", "s3cr3t"),
            ("alice:s3cr3t@[::1]:4433", "alice", "s3cr3t"),
        ] {
            let err = authority_parts(authority).expect_err("エラーになること");
            assert!(
                matches!(err, TransportError::InvalidAuthority(_)),
                "authority の解釈失敗として返ること: {err}"
            );
            let message = err.to_string();
            assert!(
                message.contains("userinfo is not supported"),
                "理由が含まれること: {message}"
            );
            assert!(
                !message.contains(authority)
                    && !message.contains(user)
                    && !message.contains(password),
                "userinfo (ユーザー名・パスワード) を含めないこと: {message}"
            );
        }
    }

    /// zone id 付き IPv6 リテラルは非対応として拒否する
    ///
    /// RFC 3986 §3.2.2 は "This syntax does not support IPv6 scoped addressing zone
    /// identifiers." と定め、`std::net::SocketAddr` も zone id を解釈できない。
    #[test]
    fn authority_parts_rejects_ipv6_zone_id() {
        for authority in ["[fe80::1%25en0]:4433", "[fe80::1%en0]"] {
            let err = authority_parts(authority).expect_err("エラーになること");
            assert!(
                err.to_string().contains("must be an IPv6 address"),
                "理由が含まれること: {err}"
            );
        }
    }

    /// 正当な authority は従来どおり受理される
    ///
    /// 拒否の追加で接続可能な入力が減っていないことを固定する。
    #[test]
    fn authority_parts_accepts_valid_authorities() {
        for (authority, host, port) in [
            ("127.0.0.1", "127.0.0.1", 443),
            ("127.0.0.1:4443", "127.0.0.1", 4443),
            ("[::1]", "::1", 443),
            ("[2001:db8::1]:4443", "2001:db8::1", 4443),
            ("relay.example.com", "relay.example.com", 443),
            ("relay.example.com:443", "relay.example.com", 443),
        ] {
            let parts = authority_parts(authority).expect("authority の解釈に成功すること");
            assert_eq!(parts.host, host, "ホスト: {authority}");
            assert_eq!(parts.port, port, "ポート: {authority}");
        }
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

    /// host が空の authority はエラーになる (draft-ietf-moq-transport-23 §6.1 は MUST NOT)
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
        assert_eq!(
            local_bind_addr(remote),
            "0.0.0.0:0"
                .parse::<std::net::SocketAddr>()
                .expect("アドレスの解釈に成功すること")
        );
    }

    /// IPv6 の接続先には IPv6 のローカルソケットを使う
    ///
    /// IPv4 ソケットから IPv6 宛に送信できないため、ファミリを合わせる。
    #[test]
    fn local_bind_addr_uses_ipv6_for_ipv6_remote() {
        let remote: std::net::SocketAddr = "[2001:db8::1]:4443"
            .parse()
            .expect("アドレスの解釈に成功すること");
        assert_eq!(
            local_bind_addr(remote),
            "[::]:0"
                .parse::<std::net::SocketAddr>()
                .expect("アドレスの解釈に成功すること")
        );
    }

    /// IP リテラルの authority は 1 件の列に解決され、既定ポートを補う
    #[tokio::test]
    async fn resolve_socket_addrs_defaults_ipv4_port_to_443() {
        let addrs = resolve_socket_addrs("127.0.0.1")
            .await
            .expect("解決に成功すること");
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].to_string(), "127.0.0.1:443");
    }

    /// IP リテラルの authority は明示ポートを維持して 1 件の列に解決される
    #[tokio::test]
    async fn resolve_socket_addrs_keeps_explicit_port() {
        let addrs = resolve_socket_addrs("127.0.0.1:4443")
            .await
            .expect("解決に成功すること");
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].to_string(), "127.0.0.1:4443");
    }

    /// IPv6 リテラルの authority は既定ポートを補って 1 件の列に解決される
    #[tokio::test]
    async fn resolve_socket_addrs_defaults_ipv6_port_to_443() {
        let addrs = resolve_socket_addrs("[::1]")
            .await
            .expect("解決に成功すること");
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].to_string(), "[::1]:443");
    }

    /// ホスト名の authority は名前解決し、解決順のまま列にしてポートを補う
    ///
    /// `localhost` は名前解決を必要とするため、`lookup_host` を通ることを固定する。
    /// 解決するアドレスとその順序は環境に依存するため (IPv4 のみ / IPv6 のみ / 両方)、
    /// 件数と順序は検証せず、解決した全アドレスがループバックであることだけを検証する。
    #[tokio::test]
    async fn resolve_socket_addrs_resolves_hostname() {
        let addrs = resolve_socket_addrs("localhost")
            .await
            .expect("解決に成功すること");
        assert!(!addrs.is_empty(), "localhost は 1 件以上に解決されること");
        for addr in &addrs {
            assert_eq!(addr.port(), 443);
            assert!(
                addr.ip().is_loopback(),
                "localhost はループバックに解決されること: {addr}"
            );
        }
    }

    /// 不正な authority は解決せず、QUIC 由来でないエラーになる
    ///
    /// 利用者向けの表示に `QUIC:` を付けない設計方針を固定する。
    #[tokio::test]
    async fn resolve_socket_addrs_rejects_invalid_authority() {
        for authority in ["::1", "127.0.0.1:abc"] {
            let err = resolve_socket_addrs(authority)
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

    /// 名前解決がタイムアウトで打ち切られる
    ///
    /// resolver を「応答しない」future で置き換えてタイムアウト経路を決定的に検証する。
    /// 本物の DNS (`example.invalid` など) は OS リゾルバの挙動 (即時の否定応答や
    /// キャッシュ) に依存し、環境によってはタイムアウトより先に future が完成するため
    /// タイムアウト経路が観測できず、テストが環境依存になってしまう。
    /// `std::future::pending` はモックやスタブではなく、resolver が応答しないという
    /// テストの前提をそのまま表す fixture である (応答を偽装しない)。
    /// hanging future は 100ns のタイムアウトより早く完成しないため、タイムアウト経路が
    /// 全環境で決定的に勝つ。
    /// タイムアウト (`RESOLVE_TIMEOUT`) を超える前に解決が失敗すれば従来どおり
    /// 解決に失敗することも [`resolve_socket_addrs_reports_resolution_failure`]
    /// (`example.invalid` を使う) で固定する。
    #[tokio::test]
    async fn resolve_socket_addrs_times_out_when_resolver_does_not_answer() {
        let started = std::time::Instant::now();
        let authority = "example.invalid";
        // resolver が永遠に応答しない前提をそのまま表す fixture (応答の偽装はしない)
        let err = resolve_socket_addrs_with(authority, Duration::from_nanos(100), |_addr_text| {
            std::future::pending::<std::io::Result<std::vec::IntoIter<SocketAddr>>>()
        })
        .await
        .expect_err("タイムアウトでエラーになること");
        assert!(
            matches!(err, TransportError::ResolutionFailed(_)),
            "名前解決の失敗として返ること: {err}"
        );
        assert!(
            err.to_string().contains("name resolution timed out"),
            "打ち切りが理由として分かること: {err}"
        );
        assert!(
            err.to_string().contains("100ns"),
            "使ったタイムアウトが理由に含まれること: {err}"
        );
        assert!(
            err.to_string().contains(authority),
            "入力の authority が含まれること: {err}"
        );
        assert!(
            !err.to_string().contains("QUIC"),
            "QUIC 由来の表示にならないこと: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "タイムアウトで打ち切られること: {:?}",
            started.elapsed()
        );
    }

    /// タイムアウトを超えなければ名前解決の失敗はそのまま伝わる
    ///
    /// 打ち切り以外の失敗の扱いがタイムアウトを包んでも変わらないことを固定する。
    #[tokio::test]
    async fn resolve_socket_addrs_reports_resolution_failure() {
        let err = resolve_socket_addrs_within("example.invalid", RESOLVE_TIMEOUT)
            .await
            .expect_err("名前解決に失敗すること");
        assert!(
            matches!(err, TransportError::ResolutionFailed(_)),
            "名前解決の失敗として返ること: {err}"
        );
        assert!(
            !err.to_string().contains("timed out"),
            "打ち切りではないことが理由に現れないこと: {err}"
        );
        assert!(
            err.to_string().contains("example.invalid"),
            "入力の authority が含まれること: {err}"
        );
    }

    /// 名前解決の結果は接続に成功するまで解決順に試される
    ///
    /// I/O ハンドルを必要としない QUIC クライアントの接続で確かめる。IPv4 のみを
    /// 待ち受ける QUIC サーバーを立て、解決に IPv6 が含まれていれば先頭の試行は失敗し、
    /// 2 回目の試行で接続が確立する。IPv6 のアドレスが含まれない環境では 1 回目で
    /// 確立するため、試行回数の期待値を環境に依存させない。
    #[tokio::test]
    async fn connect_with_fallback_tries_resolved_addrs_in_order() {
        // IPv4 のみを待ち受ける QUIC サーバーを立てる。`127.0.0.1` で bind し、
        // IPv6 では待ち受けないようにする。
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("自己署名証明書を生成できること");
        // クライアントは ALPN で MOQT_PROTOCOL を広告する。既定の `h3` のままだと
        // ハンドシェイクが HANDSHAKE_FAILURE で失敗する
        let tls = crate::quic::ServerTls {
            cert_der: certified.cert.der().to_vec(),
            key_der: certified.signing_key.serialize_der(),
            alpn: vec![MOQT_PROTOCOL.as_bytes().to_vec()],
        };
        let mut server = crate::quic::start_server(
            tls,
            "127.0.0.1:0"
                .parse()
                .expect("ワイルドカードアドレスのリテラルを解析できること"),
        )
        .await
        .expect("サーバーを起動できること");
        let port = server.local_addr().port();

        // 解決に IPv6 が含まれる環境では先頭の試行が失敗する。含まれない環境では 1 回で成功する
        let addrs = resolve_socket_addrs_within("localhost", RESOLVE_TIMEOUT)
            .await
            .expect("localhost を解決できること");
        let first_is_ipv6 = addrs.first().is_some_and(std::net::SocketAddr::is_ipv6);

        let server_task = tokio::spawn(async move {
            let connection = server.accept().await.expect("接続を受け付けられること");
            connection
                .remote_addr()
                .await
                .expect("接続元アドレスを取得できること")
                .ip()
        });
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&attempts);
        let authority = format!("localhost:{port}");
        let _connection = connect_with_fallback(&authority, move |addr| {
            let counted = std::sync::Arc::clone(&counted);
            async move {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // テスト peer は自己署名証明書を使うため、証明書検証をスキップする
                crate::quic::connect(addr, "localhost", None, true, &MultipathOptions::default())
                    .await
            }
        })
        .await
        .expect("IPv4 の待ち受けへ接続できること");
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::SeqCst),
            if first_is_ipv6 { 2 } else { 1 },
            "最初のアドレスが IPv6 のときだけ 2 回目の試行が行われること"
        );

        let accepted_ip = tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("サーバーが接続を受け付けること")
            .expect("サーバータスクがパニックしないこと");
        assert_eq!(
            accepted_ip,
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            "IPv4 の待ち受けで接続を受け付けること"
        );
    }

    /// `cert` と `insecure` の同時指定は接続の前にエラーになる
    ///
    /// どちらを優先するかを決められないため、名前解決や接続を試みずに拒否する。
    /// `cert` のパスは読み取りまで進まないことを、存在しないパスでも同じエラーになる
    /// ことで確かめる。
    #[tokio::test]
    async fn connect_rejects_cert_and_insecure_together() {
        let url = parse_url("moqt://127.0.0.1:4443/").expect("URL を解釈できること");
        let params = ConnectParams {
            url: &url,
            transport: Transport::Quic,
            cert: Some("/nonexistent/ca.pem"),
            insecure: true,
            impl_name: "test-client",
            receive_datagrams: false,
            datagram_max_size: None,
            multipath: MultipathOptions::default(),
            happy_eyeballs: happy_eyeballs::Config::default(),
        };
        let monitor = tokio_metrics::TaskMonitor::new();
        let error = match connect(&params, &monitor).await {
            Ok(_) => panic!("cert と insecure の同時指定はエラーになること"),
            Err(error) => error,
        };
        assert!(
            matches!(error, TransportError::InvalidState(_)),
            "設定の矛盾として返ること: {error:?}"
        );
        let message = error.to_string();
        assert!(
            message.contains("cert") && message.contains("insecure"),
            "どちらも指定できないことを示すこと: {message}"
        );
    }

    /// Multipath を無効にしたまま Multipath 固有の設定を渡すと接続前にエラーになること
    ///
    /// 設定が無視されたと誤解しないよう、mode が Off のときは既定以外の値を拒否する。
    #[tokio::test]
    async fn connect_rejects_multipath_settings_with_off_mode() {
        let url = parse_url("moqt://127.0.0.1:4443/").expect("URL を解釈できること");
        let params = ConnectParams {
            url: &url,
            transport: Transport::Quic,
            cert: None,
            insecure: false,
            impl_name: "test-client",
            receive_datagrams: false,
            datagram_max_size: None,
            multipath: MultipathOptions {
                keep_alive_interval: Some(Duration::from_secs(10)),
                ..MultipathOptions::default()
            },
            happy_eyeballs: happy_eyeballs::Config::default(),
        };
        let monitor = tokio_metrics::TaskMonitor::new();
        let error = match connect(&params, &monitor).await {
            Ok(_) => panic!("mode が Off の Multipath 設定はエラーになること"),
            Err(error) => error,
        };
        assert!(
            matches!(error, TransportError::InvalidState(_)),
            "設定の矛盾として返ること: {error:?}"
        );
        let message = error.to_string();
        assert!(
            message.contains("multipath") && message.contains("off"),
            "mode が必要であることを示すこと: {message}"
        );
    }

    /// Windows では Multipath を提供しないため接続前にエラーになること
    #[cfg(windows)]
    #[tokio::test]
    async fn connect_rejects_multipath_when_unavailable() {
        let url = parse_url("moqt://127.0.0.1:4443/").expect("URL を解釈できること");
        let params = ConnectParams {
            url: &url,
            transport: Transport::Quic,
            cert: None,
            insecure: false,
            impl_name: "test-client",
            receive_datagrams: false,
            datagram_max_size: None,
            multipath: MultipathOptions {
                mode: crate::multipath::MultipathMode::Handover,
                ..MultipathOptions::default()
            },
            happy_eyeballs: happy_eyeballs::Config::default(),
        };
        let monitor = tokio_metrics::TaskMonitor::new();
        let error = match connect(&params, &monitor).await {
            Ok(_) => panic!("Windows では Multipath がエラーになること"),
            Err(error) => error,
        };
        assert!(
            matches!(error, TransportError::Unsupported(_)),
            "未対応として返ること: {error:?}"
        );
        let message = error.to_string();
        assert!(
            message.contains("multipath QUIC is not supported on Windows"),
            "Windows では Multipath を利用できないことを示すこと: {message}"
        );
    }

    /// Multipath は native QUIC 以外の経路では指定できないこと
    #[tokio::test]
    async fn connect_rejects_multipath_for_non_quic_transport() {
        let url = parse_url("moqt://127.0.0.1:4443/").expect("URL を解釈できること");
        let params = ConnectParams {
            url: &url,
            transport: Transport::WtH2,
            cert: None,
            insecure: false,
            impl_name: "test-client",
            receive_datagrams: false,
            datagram_max_size: None,
            multipath: MultipathOptions {
                mode: crate::multipath::MultipathMode::Handover,
                ..MultipathOptions::default()
            },
            happy_eyeballs: happy_eyeballs::Config::default(),
        };
        let monitor = tokio_metrics::TaskMonitor::new();
        let error = match connect(&params, &monitor).await {
            Ok(_) => panic!("WebTransport 経路の Multipath はエラーになること"),
            Err(error) => error,
        };
        assert!(
            matches!(error, TransportError::Unsupported(_)),
            "未対応として返ること: {error:?}"
        );
        let message = error.to_string();
        assert!(
            message.contains("--transport quic"),
            "native QUIC が必要であることを示すこと: {message}"
        );
    }

    /// 範囲外の Multipath 設定は接続前にエラーになること
    #[tokio::test]
    async fn connect_rejects_invalid_multipath_options() {
        let url = parse_url("moqt://127.0.0.1:4443/").expect("URL を解釈できること");
        let params = ConnectParams {
            url: &url,
            transport: Transport::Quic,
            cert: None,
            insecure: false,
            impl_name: "test-client",
            receive_datagrams: false,
            datagram_max_size: None,
            multipath: MultipathOptions {
                mode: crate::multipath::MultipathMode::Handover,
                max_paths: 0,
                ..MultipathOptions::default()
            },
            happy_eyeballs: happy_eyeballs::Config::default(),
        };
        let monitor = tokio_metrics::TaskMonitor::new();
        let error = match connect(&params, &monitor).await {
            Ok(_) => panic!("範囲外の Multipath 設定はエラーになること"),
            Err(error) => error,
        };
        assert!(
            matches!(error, TransportError::InvalidState(_)),
            "設定の誤りとして返ること: {error:?}"
        );
        assert!(
            error.to_string().contains("max_paths"),
            "誤りのある設定を示すこと: {error}"
        );
    }

    /// QUIC が確立した後に MOQT SETUP が失敗しても、次のアドレスには接続しない
    #[tokio::test]
    async fn connect_does_not_fall_back_after_setup_fails() {
        use crate::peer_harness::TestPeerServer;

        tokio::time::timeout(Duration::from_secs(10), async {
            let mut first = TestPeerServer::start()
                .await
                .expect("最初の QUIC サーバーを起動できること");
            let mut fallback = TestPeerServer::start()
                .await
                .expect("次の QUIC サーバーを起動できること");
            // DNS や IPv6 の有無に依存せず、実際に応答できる 2 つの接続先を順序付きで用意する。
            let addrs = [first.local_addr(), fallback.local_addr()];
            let url = parse_url(&format!("moqt://localhost:{}", addrs[0].port()))
                .expect("接続先 URL を解釈できること");
            let params = ConnectParams {
                url: &url,
                transport: Transport::Quic,
                cert: None,
                // テスト peer は自己署名証明書を使うため、証明書検証をスキップする
                insecure: true,
                impl_name: "test-client",
                receive_datagrams: false,
                datagram_max_size: None,
                multipath: MultipathOptions::default(),
                happy_eyeballs: happy_eyeballs::Config::default(),
            };
            let monitor = tokio_metrics::TaskMonitor::new();

            tokio::join!(
                async {
                    let mut peer = first
                        .accept()
                        .await
                        .expect("最初の QUIC 接続を受理できること");
                    // SETUP の Type (0x2F00) だけを送り、Length と本文を欠いたまま FIN で
                    // 終端する。先頭 varint は SETUP の Type が stream type を兼ねる。
                    let mut setup_type = Vec::new();
                    shiguredo_moqt::varint::encode(
                        shiguredo_moqt::stream::SETUP_STREAM_TYPE,
                        &mut setup_type,
                    );
                    peer.send_control_stream_head(setup_type)
                        .await
                        .expect("SETUP の Type を送信できること");
                    peer.finish_control_stream()
                        .await
                        .expect("SETUP の続きを送らずに制御ストリームを終端できること");
                    // クライアントが MOQT の終了コードを送るまでサーバーを生かしておく。
                    assert_eq!(
                        peer.recv_close_code()
                            .await
                            .expect("終了コードを観測できること"),
                        shiguredo_moqt::error::SESSION_CLOSED_CONTROL_STREAM,
                        "SETUP を受け取る前の制御ストリーム終了として扱うこと"
                    );
                },
                async {
                    // 次のアドレスへの接続を受け付けながらエラーを待つ。誤って試行すると、
                    // SETUP の応答を待つ前に QUIC 接続を受理できるため、不在確認用の sleep は不要。
                    tokio::select! {
                        result = connect_resolved(&params, &monitor, &addrs) => {
                            let error = result.err().expect("最初の SETUP の失敗を返すこと");
                            assert!(
                                error.to_string().contains("control stream closed before SETUP"),
                                "最初の SETUP のエラーを保持すること: {error}"
                            );
                        }
                        _ = fallback.accept() => panic!("次の QUIC サーバーには接続しないこと"),
                    }
                },
            );
        })
        .await
        .expect("SETUP の失敗と切断の観測が制限時間内に完了すること");
    }

    /// `Transport::Auto` は QUIC が確立できないとき QMux にフォールバックする
    ///
    /// QUIC の待ち受けを置かず、TCP + TLS + QMux のサーバーだけを立てる。Auto の候補は
    /// native QUIC を先に試すため、QUIC の試行が確立しない間に QMux の試行が始まり、
    /// QMux が勝者になる。SETUP まで交換できたことを実接続で確認する。
    #[tokio::test]
    async fn connect_auto_falls_back_to_qmux() {
        use crate::peer_harness::QmuxTestPeerServer;

        tokio::time::timeout(Duration::from_secs(10), async {
            let mut server = QmuxTestPeerServer::start()
                .await
                .expect("QMux のテストサーバーを起動できること");
            let addr = server.local_addr();
            let url = parse_url(&format!("moqt://127.0.0.1:{}", addr.port()))
                .expect("接続先 URL を解釈できること");
            let params = ConnectParams {
                url: &url,
                transport: Transport::Auto,
                cert: None,
                // テスト peer は自己署名証明書を使うため、証明書検証をスキップする
                insecure: true,
                impl_name: "test-client",
                receive_datagrams: false,
                datagram_max_size: None,
                multipath: MultipathOptions::default(),
                happy_eyeballs: happy_eyeballs::Config::default(),
            };
            let monitor = tokio_metrics::TaskMonitor::new();

            // QUIC の待ち受けは無い。QMux のサーバーは TCP で待ち受け、Auto の QMux 試行を
            // 受理して SETUP を交換する
            let (connected, ()) = tokio::join!(connect(&params, &monitor), async {
                let mut peer = server.accept().await.expect("QMux 接続を受理できること");
                peer.establish().await.expect("SETUP を交換できること");
            },);
            connected.expect("QMux 経由で接続できること");
        })
        .await
        .expect("Auto のフォールバックが制限時間内に完了すること");
    }

    /// すべての接続先が失敗した場合は最後の失敗を返す
    ///
    /// どのアドレスでも接続が確立しない場合に、試行を繰り返したうえで最後の失敗が
    /// 呼び出し元へ伝わることを固定する。`127.0.0.1:9` (discard) は待ち受けが無い前提とする。
    #[tokio::test]
    async fn connect_with_fallback_returns_last_failure() {
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&attempts);
        let error: TransportError = connect_with_fallback("127.0.0.1:9", move |_addr| {
            let counted = std::sync::Arc::clone(&counted);
            async move {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err::<(), TransportError>(TransportError::Quic("connection failed".to_string()))
            }
        })
        .await
        .expect_err("すべて失敗した場合はエラーになること");
        assert!(
            matches!(error, TransportError::Quic(_)),
            "最後の失敗が返ること: {error}"
        );
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "IP リテラルは 1 件に解決されるため 1 回だけ試行すること"
        );
    }

    /// タイムアウトが十分長ければ IP リテラルとホスト名の解決は従来どおり成功する
    ///
    /// タイムアウトを包んでも成功経路が変わらないことを固定する。
    #[tokio::test]
    async fn resolve_socket_addrs_succeeds_within_timeout() {
        let ipv4 = resolve_socket_addrs_within("127.0.0.1", RESOLVE_TIMEOUT)
            .await
            .expect("IP リテラルの解決に成功すること");
        assert_eq!(ipv4.len(), 1);
        assert_eq!(ipv4[0].to_string(), "127.0.0.1:443");

        let hostname = resolve_socket_addrs_within("localhost", RESOLVE_TIMEOUT)
            .await
            .expect("ホスト名の解決に成功すること");
        assert!(
            !hostname.is_empty(),
            "localhost は 1 件以上に解決されること"
        );
        assert!(
            hostname.iter().all(|addr| addr.ip().is_loopback()),
            "localhost はループバックに解決されること: {hostname:?}"
        );
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

    /// origin の比較は host の大文字小文字とポート省略を正規化すること
    #[test]
    fn same_origin_normalizes_host_case_and_default_port() {
        let a = parse_url("moqt://relay.example/live").expect("URL をパースできること");
        let b = parse_url("moqt://RELAY.example:443/other").expect("URL をパースできること");
        assert!(same_origin(&a, &b), "同じ origin と判定すること");

        let other_port =
            parse_url("moqt://relay.example:4443/live").expect("URL をパースできること");
        assert!(!same_origin(&a, &other_port), "ポートが違えば別 origin");

        let other_host = parse_url("moqt://other.example/live").expect("URL をパースできること");
        assert!(!same_origin(&a, &other_host), "host が違えば別 origin");
    }

    /// Timeout 0 はローカル既定値を使い、通常の Timeout はそのまま期限にすること
    #[test]
    fn migration_deadline_uses_timeout_and_local_default() {
        let base = Instant::now();
        let deadline = MigrationDeadline::after_goaway(base, 5000);
        assert!(!deadline.uses_local_default());
        assert_eq!(
            deadline.remaining_at(base + Duration::from_secs(1)),
            Duration::from_secs(4)
        );
        assert!(deadline.expired_at(base + Duration::from_secs(5)));
        assert!(!deadline.expired_at(base + Duration::from_millis(4999)));

        let no_timeout = MigrationDeadline::after_goaway(base, 0);
        assert!(no_timeout.uses_local_default());
        assert_eq!(
            no_timeout.remaining_at(base),
            MigrationDeadline::DEFAULT_TIMEOUT
        );
    }

    /// 再試行の待ち時間は指数で増え、上限と jitter の範囲に収まること
    #[test]
    fn retry_policy_grows_and_caps() {
        let policy = RetryPolicy {
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(800),
        };
        let first = policy.delay(0);
        assert!(
            first >= Duration::from_millis(100) && first <= Duration::from_millis(125),
            "初回は 100ms から jitter の範囲に収まること: {first:?}"
        );
        let late = policy.delay(20);
        assert!(
            late >= Duration::from_millis(800) && late <= Duration::from_millis(1000),
            "上限と jitter の範囲に収まること: {late:?}"
        );
    }

    /// 時間窓内の移行回数を制限すること
    #[test]
    fn migration_limiter_enforces_the_window() {
        let base = Instant::now();
        let mut limiter = MigrationLimiter::new(Duration::from_secs(10), 2);
        assert!(limiter.check_and_record(base));
        assert!(limiter.check_and_record(base + Duration::from_secs(1)));
        assert!(
            !limiter.check_and_record(base + Duration::from_secs(2)),
            "窓内の上限を超えたら拒否すること"
        );
        assert!(
            limiter.check_and_record(base + Duration::from_secs(11)),
            "窓の外に出た記録は捨てること"
        );
    }

    /// request stream の GOAWAY でも URI を解釈し、scope を区別すること
    #[test]
    fn migration_target_reports_request_scope() {
        let current = parse_url("moqt://relay.example/live").expect("URL をパースできること");
        let target = migration_target(b"moqt://other.example/live", Some(7), &current)
            .expect("判断に成功すること")
            .expect("移行先があること");
        assert_eq!(target.scope, MigrationScope::Request(7));
        assert_eq!(target.url.authority, "other.example");
        assert!(
            target.url.cat_tokens.is_empty(),
            "別 origin へトークンを引き継がないこと"
        );
    }

    /// New Session URI が空のときは現在の URL を再利用すること
    ///
    /// draft-ietf-moq-transport-23 §9.2 (GOAWAY) は「URI が 0 バイトなら現在の URI を
    /// 再利用する」と定める。
    #[test]
    fn migration_target_empty_uri_reuses_current_url() {
        let current =
            parse_url("moqt://relay.example:443/live?token=abc#msf:moq-example--video&cat=QUJD")
                .expect("現在の URL をパースできること");
        let target = migration_target(&[], None, &current)
            .expect("判断に失敗しないこと")
            .expect("移行先があること");
        assert_eq!(target.scope, MigrationScope::Session);
        assert_eq!(target.url.authority, current.authority);
        assert_eq!(target.url.path, current.path);
        assert_eq!(target.url.cat_tokens, current.cat_tokens);
    }

    /// 同一 origin の New Session URI で cat が無いときは元のトークンを引き継ぐこと
    ///
    /// draft-ietf-moq-msf-02 §11.5.3 は track に紐づくトークンを同じ track の制御
    /// メッセージへ載せる MUST を課すため、移行先でも載せ続ける必要がある。
    #[test]
    fn migration_target_same_origin_uri_without_cat_carries_tokens() {
        let current =
            parse_url("moqt://relay.example:443/live?token=abc#msf:moq-example--video&cat=QUJD")
                .expect("現在の URL をパースできること");
        let target = migration_target(
            b"moqt://relay.example:443/other#msf:moq-example--video",
            None,
            &current,
        )
        .expect("判断に失敗しないこと")
        .expect("移行先があること");
        assert_eq!(target.url.authority, "relay.example:443");
        assert_eq!(target.url.path, "/other");
        assert_eq!(
            target.url.cat_tokens, current.cat_tokens,
            "同一 origin なら移行先に cat が無くても元のトークンを引き継ぐこと"
        );
    }

    /// 別 origin の New Session URI には cat を引き継がないこと
    ///
    /// 移行先が別ホストのときは認可トークンを送らない。移行先で別途取得する
    /// (draft-ietf-moq-msf-02 §11.5.4)。
    #[test]
    fn migration_target_different_origin_does_not_carry_tokens() {
        let current =
            parse_url("moqt://relay.example:443/live?token=abc#msf:moq-example--video&cat=QUJD")
                .expect("現在の URL をパースできること");
        let target = migration_target(
            b"moqt://other.example:443/live#msf:moq-example--video",
            None,
            &current,
        )
        .expect("判断に失敗しないこと")
        .expect("移行先があること");
        assert_eq!(target.url.authority, "other.example:443");
        assert!(
            target.url.cat_tokens.is_empty(),
            "別 origin へ認可トークンを送らないこと"
        );
    }

    /// New Session URI の fragment に cat があるときはそちらを使うこと
    #[test]
    fn migration_target_uri_with_cat_uses_its_own_tokens() {
        let current =
            parse_url("moqt://relay.example:443/live?token=abc#msf:moq-example--video&cat=QUJD")
                .expect("現在の URL をパースできること");
        let target = migration_target(
            b"moqt://other.example:443/live#msf:moq-example--video&cat=WFla",
            None,
            &current,
        )
        .expect("判断に失敗しないこと")
        .expect("移行先があること");
        assert_eq!(target.url.cat_tokens, vec![b"XYZ".to_vec()]);
    }

    /// UTF-8 でない New Session URI はエラーにすること
    #[test]
    fn migration_target_non_utf8_uri_is_rejected() {
        let current = parse_url("moqt://relay.example/live").expect("URL をパースできること");
        let err = migration_target(&[0xff, 0xfe], None, &current).expect_err("エラーになること");
        assert!(
            err.to_string().contains("not UTF-8"),
            "UTF-8 でない URI として報告すること: {err}"
        );
    }

    /// request stream の GOAWAY で URI が不正なら session を維持できるよう None にすること
    #[test]
    fn migration_target_keeps_session_for_invalid_request_uri() {
        let current = parse_url("moqt://relay.example/live").expect("URL をパースできること");
        let target = migration_target(b"https://other.example/live", Some(7), &current)
            .expect("request 単位移行ではエラーにしないこと");
        assert!(target.is_none(), "session を維持すること");
    }

    /// New Session URI の解析エラーは URI の実値を含まない
    ///
    /// New Session URI は peer が供給し、query / fragment に認可トークンを含み得る
    /// (draft-ietf-moq-msf-02 §11.5.2)。エラーはログに出るため実値を含めない。
    #[test]
    fn migration_target_errors_do_not_include_uri() {
        let current = parse_url("moqt://relay.example/live").expect("URL をパースできること");
        for uri in [
            "moqt://other.example/live?token=SECRETVALUE#badvalue",
            "moqt://other.example/live#msf:ns--track&cat=SECRETVALUE!",
        ] {
            let err = migration_target(uri.as_bytes(), None, &current)
                .expect_err("session 単位の移行ではエラーにすること");
            let message = err.to_string();
            assert!(
                !message.contains("SECRETVALUE"),
                "実値を含めないこと: {message}"
            );
        }

        // userinfo を含む移行先は拒否し、ユーザー名とパスワードを保持・表示しない
        let err = migration_target(b"moqt://alice:s3cr3t@other.example/live", None, &current)
            .expect_err("userinfo を含む移行先は拒否すること");
        let message = err.to_string();
        assert!(
            !message.contains("alice") && !message.contains("s3cr3t"),
            "userinfo を含めないこと: {message}"
        );
        // request 単位移行では session を維持する
        let target = migration_target(b"moqt://alice:s3cr3t@other.example/live", Some(7), &current)
            .expect("request 単位移行ではエラーにしないこと");
        assert!(target.is_none(), "session を維持すること");
    }

    /// 再試行可能な接続エラーの分類が仕様どおりであること
    #[test]
    fn retryable_connect_errors_are_classified() {
        assert!(!is_retryable_connect_error(
            &TransportError::InvalidAuthority("invalid".to_string())
        ));
        assert!(is_retryable_connect_error(
            &TransportError::ResolutionFailed("temporary".to_string())
        ));
        assert!(is_retryable_connect_error(
            &TransportError::ConnectionClosed
        ));
        assert!(!is_retryable_connect_error(
            &TransportError::ConnectFailed { status: Some(401) }
        ));
        assert!(is_retryable_connect_error(&TransportError::ConnectFailed {
            status: Some(503)
        }));
        assert!(!is_retryable_connect_error(
            &TransportError::ProtocolNegotiationFailed { error_code: 0 }
        ));
        assert!(!is_retryable_connect_error(&TransportError::InvalidState(
            "no common WebTransport draft version".to_string()
        )));
    }

    /// ログ用サニタイズが printable ASCII 以外をエスケープし、通常の ASCII を残すこと
    #[test]
    fn sanitize_for_log_escapes_untrusted_characters() {
        assert_eq!(super::sanitize_for_log("hello world"), "hello world");
        // ESC (0x1B) / タブ (0x09) / DEL (0x7F)
        assert_eq!(
            super::sanitize_for_log("\u{1b}[31m\t\u{7f}"),
            "\\x1B[31m\\x09\\x7F"
        );
        // Latin-1 は \xNN、それ以外の Unicode は \u{...}
        assert_eq!(super::sanitize_for_log("é"), "\\xE9");
        assert_eq!(super::sanitize_for_log("あ"), "\\u{3042}");
    }
}
