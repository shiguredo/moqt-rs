//! MSF URI と fragment のパース (draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation))
//!
//! `moqt://` URI の fragment 部 (`msf:...`) をパースし、MSF namespace-name 文字列を
//! [`crate::name::parse_name_with_percent_encoding`] で namespace / track name へ分解する。
//! 予約 fragment パラメータ (§11.1.1 (Reserved fragment parameters)) の値型アクセサを提供する。
//!
//! 本モジュールは URI の構文解析のみを行い、MOQT セッションの確立や SUBSCRIBE / FETCH の
//! 発行は行わない。
//! track-identifier は RFC 3986 §2.1 (Percent-Encoding) の `pct-encoded` (`%XX`) を
//! データバイトとしてデコードする (§11.1 が `?` を `%3F` と書くことを求めるため)。
//! 一方、`&` 区切りのパラメータ列は percent-decode せず生の文字列のまま保持する。
//! この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。

use crate::{error::MessageError, message::common::TrackNamespace, name};
use alloc::{format, string::String, string::ToString, vec::Vec};

/// MSF URI の fragment (draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation))
///
/// `msf:` に続く track-identifier と、`&` 区切りのパラメータ列を保持する。
#[derive(Debug, Clone, PartialEq)]
pub struct MsfFragment {
    /// track-identifier を MSF namespace-name 文字列としてパースした namespace
    pub namespace: TrackNamespace,
    /// track-identifier を MSF namespace-name 文字列としてパースした track name
    pub track_name: Vec<u8>,
    /// fragment パラメータ列 (出現順)
    pub parameters: Vec<MsfFragmentParameter>,
}

/// MSF fragment の 1 パラメータ (draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation))
///
/// 値は token を含み得る (§11.1.1 の `cat`、§11.4.2)。パラメータ名は配備ごとに決まり、名前で
/// 機密かどうかを判別できないため、`Debug` ではすべての値を伏せ、`value_bytes` として長さだけを
/// 表示する。
#[derive(Clone, PartialEq, Eq)]
pub struct MsfFragmentParameter {
    /// パラメータ名 (case-sensitive)
    pub name: String,
    /// パラメータ値 (空文字列を許容する)
    pub value: String,
}

impl core::fmt::Debug for MsfFragmentParameter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // フィールドを追加したときに表示漏れをコンパイラが検出できるよう、`..` を使わずに分解する
        let Self { name, value } = self;
        f.debug_struct("MsfFragmentParameter")
            .field("name", name)
            .field("value_bytes", &value.len())
            .finish()
    }
}

/// connection パラメータが要求する接続種別 (draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters))
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsfConnectionType {
    /// `q`: Native QUIC 接続を必須とする
    Quic,
    /// `wt`: WebTransport 接続を必須とする
    WebTransport,
}

/// wallclock-range / mediatime-range の範囲 (draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters))
///
/// 両端は inclusive。`end_ms` が `None` の場合は open range を表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsfTimeRange {
    /// 範囲の開始値 (wallclock ms または media time ms)
    pub start_ms: u64,
    /// 範囲の終了値。`None` は終端なし (open range)
    pub end_ms: Option<u64>,
}

/// location-range の終端 (draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters))
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsfLocationRangeEnd {
    /// 終端 Group ID
    pub group_id: u64,
    /// 終端 Object ID。`None` は終端 Group 全体を含む
    pub object_id: Option<u64>,
}

/// location-range の範囲 (draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters))
///
/// 両端は inclusive。`end` が `None` の場合は open range を表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsfLocationRange {
    /// 開始 Group ID
    pub start_group_id: u64,
    /// 開始 Object ID。`None` は開始 Group 全体を含む
    pub start_object_id: Option<u64>,
    /// 終端。`None` は終端なし (open range)
    pub end: Option<MsfLocationRangeEnd>,
}

/// パース済みの MSF URI (draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation))
///
/// fragment 以外の要素は接続確立に使う付随情報として保持する。
///
/// connection URI のパラメータは token を含み得て (§11.4.2)、名前で機密かどうかを判別できない
/// ため、`Debug` は `query` 全体を伏せ、バイト長を `Option<usize>` で表示する。`fragment` の
/// パラメータ値は [`MsfFragmentParameter`] の `Debug` で伏せる。
#[derive(Clone, PartialEq)]
pub struct MsfUri {
    /// authority (host + optional port)
    ///
    /// [`parse_msf_uri`] が返す値は userinfo (`@`) を含まない。authority に `@` を
    /// 含む URI は RFC 3986 §3.2.1 (User Information) に基づき拒否する。
    pub authority: String,
    /// path (先頭の `/` を含む。無い場合は空文字列)
    pub path: String,
    /// query (`?` 以降。無い場合は `None`)
    pub query: Option<String>,
    /// `#` 以降の fragment
    pub fragment: MsfFragment,
}

impl core::fmt::Debug for MsfUri {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // フィールドを追加したときに表示漏れをコンパイラが検出できるよう、`..` を使わずに分解する
        let Self {
            authority,
            path,
            query,
            fragment,
        } = self;
        f.debug_struct("MsfUri")
            .field("authority", authority)
            .field("path", path)
            .field("query", &query.as_ref().map(String::len))
            .field("fragment", fragment)
            .finish()
    }
}

impl MsfFragment {
    /// 指定名のパラメータ値を出現順に返す
    ///
    /// 未知パラメータや予約パラメータも含めて参照できる。
    pub fn parameter_values(&self, name: &str) -> Vec<&str> {
        self.parameters
            .iter()
            .filter(|p| p.name == name)
            .map(|p| p.value.as_str())
            .collect()
    }

    /// connection パラメータを接続種別へ変換する
    ///
    /// 複数指定された場合は出現順にすべて返す。§11.1.1 (Reserved fragment parameters) の union 規則は
    /// 値域 (range) を対象としたもので `connection` には適用しないため、`q` と `wt` のように両立しない
    /// 組み合わせの扱いは呼び出し側が決める。
    ///
    /// # Errors
    ///
    /// `q` / `wt` 以外の値: `InvalidCatalog`
    pub fn connection_types(&self) -> Result<Vec<MsfConnectionType>, MessageError> {
        let mut types = Vec::new();
        for value in self.parameter_values("connection") {
            let connection = match value {
                "q" => MsfConnectionType::Quic,
                "wt" => MsfConnectionType::WebTransport,
                other => {
                    return Err(MessageError::InvalidCatalog(format!(
                        "invalid connection parameter value '{other}' (expected 'q' or 'wt')"
                    )));
                }
            };
            types.push(connection);
        }
        Ok(types)
    }

    /// wallclock-range パラメータを範囲へ変換する
    ///
    /// # Errors
    ///
    /// 値が数値でない / 終端が開始より前 / u64 を超える: `InvalidCatalog`
    pub fn wallclock_ranges(&self) -> Result<Vec<MsfTimeRange>, MessageError> {
        self.parameter_values("wallclock-range")
            .into_iter()
            .map(parse_time_range)
            .collect()
    }

    /// mediatime-range パラメータを範囲へ変換する
    ///
    /// # Errors
    ///
    /// 値が数値でない / 終端が開始より前 / u64 を超える: `InvalidCatalog`
    pub fn mediatime_ranges(&self) -> Result<Vec<MsfTimeRange>, MessageError> {
        self.parameter_values("mediatime-range")
            .into_iter()
            .map(parse_time_range)
            .collect()
    }

    /// location-range パラメータを範囲へ変換する
    ///
    /// # Errors
    ///
    /// 値が `GroupID` / `GroupID.ObjectID` 形式でない / u64 を超える /
    /// 終端が開始より前 / 末尾ダッシュ (`5-`) を含む: `InvalidCatalog`
    pub fn location_ranges(&self) -> Result<Vec<MsfLocationRange>, MessageError> {
        self.parameter_values("location-range")
            .into_iter()
            .map(parse_location_range)
            .collect()
    }

    /// wallclock-range パラメータを union した範囲を返す
    ///
    /// draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters) は、同じ URL に同じ
    /// パラメータが複数ある場合、その値を union すると定める。`wallclock_ranges` が返す
    /// 出現順の範囲を開始値でソートし、重なる・隣接する範囲を併合する。
    ///
    /// # Errors
    ///
    /// [`Self::wallclock_ranges`] と同じ。
    pub fn wallclock_range_union(&self) -> Result<Vec<MsfTimeRange>, MessageError> {
        Ok(union_time_ranges(self.wallclock_ranges()?))
    }

    /// mediatime-range パラメータを union した範囲を返す
    ///
    /// # Errors
    ///
    /// [`Self::mediatime_ranges`] と同じ。
    pub fn mediatime_range_union(&self) -> Result<Vec<MsfTimeRange>, MessageError> {
        Ok(union_time_ranges(self.mediatime_ranges()?))
    }

    /// location-range パラメータを union した範囲を返す
    ///
    /// draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters) は、同じ URL に同じ
    /// パラメータが複数ある場合、その値を union すると定める。Group ID / Object ID の
    /// 辞書順で範囲をソートし、重なる・隣接する範囲を併合する。
    ///
    /// # Errors
    ///
    /// [`Self::location_ranges`] と同じ。
    pub fn location_range_union(&self) -> Result<Vec<MsfLocationRange>, MessageError> {
        Ok(union_location_ranges(self.location_ranges()?))
    }

    /// cat パラメータの base64 文字列を出現順に返す
    ///
    /// base64 のデコードや妥当性検証は行わない (利用側の責務)。
    /// draft-ietf-moq-msf-02 Appendix A.1.7 が v01 の予約パラメータ `c4m` の `cat` への改名を
    /// 記録し、§11.1.1 (Reserved fragment parameters) の Table 12 が `cat` を予約パラメータと
    /// して定義する。本実装は旧 `c4m` を予約パラメータとして扱わず、認可トークンの供給元に
    /// しない (予約でないパラメータを認可に使わない fail-closed の方針)。
    pub fn cat_tokens(&self) -> Vec<&str> {
        self.parameter_values("cat")
    }
}

/// MOQT URI の共通成分 (scheme を除く)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MoqtUriComponents<'a> {
    authority: &'a str,
    path: &'a str,
    query: Option<&'a str>,
    fragment: Option<&'a str>,
}

/// `moqt://` URI の scheme を確認し、authority / path / query / fragment に分解する
///
/// draft-ietf-moq-transport-23 §6.1 (MOQT URI Scheme) の
/// `moqt-URI = "moqt" "://" authority path-abempty [ "?" query ]` に従う。
/// fragment は §6.1.1 (Fragment Identifiers) のとおり optional として返す。
fn split_moqt_uri(uri: &str) -> Result<MoqtUriComponents<'_>, MessageError> {
    // バイト境界を跨ぐ slice で panic しないよう get で前方一致を確認する。
    const SCHEME: &str = "moqt://";
    let rest = match uri.get(..SCHEME.len()) {
        Some(prefix) if prefix.eq_ignore_ascii_case(SCHEME) => &uri[SCHEME.len()..],
        _ => {
            return Err(MessageError::InvalidCatalog(
                "MOQT URI must use the 'moqt://' scheme".to_string(),
            ));
        }
    };
    let (before_fragment, fragment) = match rest.split_once('#') {
        Some((before, fragment)) => {
            if fragment.contains('#') {
                return Err(MessageError::InvalidCatalog(
                    "MOQT URI must contain at most one '#' separator".to_string(),
                ));
            }
            (before, Some(fragment))
        }
        None => (rest, None),
    };
    let (before_query, query) = match before_fragment.split_once('?') {
        Some((before, query)) => (before, Some(query)),
        None => (before_fragment, None),
    };
    let (authority, path) = match before_query.find('/') {
        Some(idx) => (&before_query[..idx], &before_query[idx..]),
        None => (before_query, ""),
    };
    if authority.is_empty() {
        return Err(MessageError::InvalidCatalog(
            "MOQT URI authority is empty".to_string(),
        ));
    }
    // RFC 3986 §3.2.1 (User Information) は "user:password" 形式を deprecated とし、
    // 受け取った reference 中の userinfo を拒否する選択を許す。MOQT §6.1 の authority は
    // RFC 3986 の authority であり、本実装は資格情報を URI に載せない。
    // エラーメッセージに authority の実値を含めない (パスワードを残さない)。
    if authority.contains('@') {
        return Err(MessageError::InvalidCatalog(
            "MOQT URI authority MUST NOT contain userinfo".to_string(),
        ));
    }
    crate::parameter::validate_authority(authority.as_bytes()).map_err(|_| {
        MessageError::InvalidCatalog(
            "MOQT URI authority is not a valid RFC 3986 authority".to_string(),
        )
    })?;
    crate::parameter::validate_path_abempty(path.as_bytes()).map_err(|_| {
        MessageError::InvalidCatalog("MOQT URI path is not a valid RFC 3986 path".to_string())
    })?;
    if let Some(query) = query {
        crate::parameter::validate_query(query.as_bytes()).map_err(|_| {
            MessageError::InvalidCatalog("MOQT URI query is not a valid RFC 3986 query".to_string())
        })?;
    }
    Ok(MoqtUriComponents {
        authority,
        path,
        query,
        fragment,
    })
}

/// MOQT の endpoint URI を検証する
///
/// MSF §5.2.37 (Connection URI) が要求する "valid MOQT endpoint URI" の検証に使う。
/// `moqt://` scheme、空でない host、RFC 3986 の path / query、optional fragment の
/// `type:value` 構文を確認する。fragment の値の意味は fragment type ごとに異なるため、
/// この関数では検証しない。
///
/// # Errors
///
/// - scheme が `moqt://` でない / authority が空 / userinfo を含む
/// - authority / path / query が RFC 3986 に違反する
/// - fragment が `type:value` の構文を満たさない
pub fn validate_moqt_endpoint_uri(uri: &str) -> Result<(), MessageError> {
    let components = split_moqt_uri(uri)?;
    if let Some(fragment) = components.fragment {
        validate_moqt_fragment_syntax(fragment)?;
    }
    Ok(())
}

/// MOQT URI の fragment が `type:value` の構文を満たすか検証する
///
/// draft-ietf-moq-transport-23 §6.1.1 (Fragment Identifiers): fragment type identifier は
/// ASCII 小文字・数字・ハイフンだけで構成し、`:` の後に type-specific value を続ける。
/// value の構文は fragment type ごとに定義されるため、ここでは空でないことだけを確認する。
fn validate_moqt_fragment_syntax(fragment: &str) -> Result<(), MessageError> {
    let (kind, value) = fragment.split_once(':').ok_or_else(|| {
        MessageError::InvalidCatalog("MOQT URI fragment must be 'type:value'".to_string())
    })?;
    if kind.is_empty() || value.is_empty() {
        return Err(MessageError::InvalidCatalog(
            "MOQT URI fragment type and value must be non-empty".to_string(),
        ));
    }
    if !kind
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(MessageError::InvalidCatalog(
            "MOQT URI fragment type must contain only lower-case letters, digits, and '-'"
                .to_string(),
        ));
    }
    Ok(())
}

/// MSF URI 全体をパースする
///
/// draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation) の
/// `msf-uri = "moqt://" authority path-abempty [ "?" query ] "#" msf-fragment` に従う。
/// 同節は scheme を case-insensitive と規定するため、`MOQT://` なども受理する。
///
/// # Errors
///
/// - `moqt://` scheme でない / `#` が無い / authority が空: `InvalidCatalog`
/// - authority に userinfo (`@`) を含む: `InvalidCatalog`
///   (RFC 3986 §3.2.1 (User Information) は `user:password` 形式を deprecated とし、
///   受け取った reference 中の userinfo を拒否する選択を許す)
/// - fragment が `msf:` で始まらない / 形式不正: `InvalidCatalog`
pub fn parse_msf_uri(uri: &str) -> Result<MsfUri, MessageError> {
    let components = split_moqt_uri(uri)?;
    let fragment_str = components
        .fragment
        .ok_or_else(|| MessageError::InvalidCatalog("MSF URI is missing a fragment".to_string()))?;
    let fragment = parse_msf_fragment(fragment_str)?;
    Ok(MsfUri {
        authority: components.authority.to_string(),
        path: components.path.to_string(),
        query: components.query.map(ToString::to_string),
        fragment,
    })
}

/// `#` 以降の fragment (`msf:...`) をパースする
///
/// draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation) の ABNF に従う。
/// track-identifier は §11.1 (URL construction and interpretation) の `pchar-no-amp / "/"` を
/// §11.1.2 (MSF Namespace-Name String Encoding) の表現として
/// [`crate::name::parse_name_with_percent_encoding`] で分解する。URI 層の `%XX`
/// (RFC 3986 §2.1 (Percent-Encoding)) はデータバイトとしてデコードする。
///
/// `&` 区切りのパラメータ列は percent-decode せず生の文字列のまま保持する。
///
/// # Errors
///
/// - `msf:` で始まらない / track-identifier が空 / パラメータ形式不正: `InvalidCatalog`
/// - track-identifier の文字が ABNF 外、または `%XX` が hex 2 桁でない: `InvalidCatalog`
/// - track-identifier が namespace-name 表現として不正: `InvalidCatalog`
///
/// パラメータ値は認可トークンなどの機密値を含み得るため、パラメータ値の検証エラーには実値を
/// 含めず、パラメータ名と 1 始まりの位置を示す。`=` の無い要素は名前と値を区別できないため、
/// 要素全体を示さず位置だけを示す。track-identifier とパラメータ名は機密値でないため実値を示す。
pub fn parse_msf_fragment(fragment: &str) -> Result<MsfFragment, MessageError> {
    let value = fragment.strip_prefix("msf:").ok_or_else(|| {
        MessageError::InvalidCatalog("MSF fragment must start with 'msf:'".to_string())
    })?;
    if value.is_empty() {
        return Err(MessageError::InvalidCatalog(
            "MSF fragment is empty".to_string(),
        ));
    }
    let mut parts = value.split('&');
    let track_identifier = parts
        .next()
        .expect("split always yields at least one element");
    if track_identifier.is_empty() {
        return Err(MessageError::InvalidCatalog(
            "MSF track identifier is empty".to_string(),
        ));
    }
    validate_pchar_no_amp(track_identifier, true, FragmentComponent::TrackIdentifier)?;
    // draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation): track-identifier は
    // `pchar-no-amp / "/"` であり、RFC 3986 §2.1 (Percent-Encoding) の `pct-encoded` を含む。
    // `%XX` をデータバイトとしてデコードする (`?` は `%3F` として現れる)。
    let (namespace, track_name) = name::parse_name_with_percent_encoding(track_identifier)
        .map_err(|e| {
            MessageError::InvalidCatalog(format!("invalid MSF track identifier: {e:?}"))
        })?;
    // draft-ietf-moq-transport-23 §2.4.1 (Track Naming): "Each Track Name MUST contain at
    // least one byte." §8.8 の表現としては空の Track Name も表せるが、MSF fragment は
    // SUBSCRIBE / FETCH に使う Full Track Name を作るため、ここで拒否する。
    if track_name.is_empty() {
        return Err(MessageError::InvalidCatalog(
            "MSF track identifier has an empty Track Name".to_string(),
        ));
    }

    let mut parameters = Vec::new();
    for (index, element) in parts.enumerate() {
        if element.is_empty() {
            return Err(MessageError::InvalidCatalog(
                "MSF fragment contains an empty parameter".to_string(),
            ));
        }
        let (name, value) = element.split_once('=').ok_or_else(|| {
            // 要素全体はパラメータ値 (認可トークンなど) を含み得るため、位置だけを示す
            MessageError::InvalidCatalog(format!(
                "MSF fragment parameter #{} is missing '='",
                index + 1
            ))
        })?;
        if name.is_empty() {
            return Err(MessageError::InvalidCatalog(
                "MSF fragment parameter name is empty".to_string(),
            ));
        }
        validate_pchar_no_amp(name, true, FragmentComponent::ParameterName)?;
        validate_pchar_no_amp(
            value,
            true,
            FragmentComponent::ParameterValue {
                name,
                position: index + 1,
            },
        )?;
        parameters.push(MsfFragmentParameter {
            name: name.to_string(),
            value: value.to_string(),
        });
    }
    Ok(MsfFragment {
        namespace,
        track_name,
        parameters,
    })
}

/// `pchar-no-amp` (任意で `/` を含む) の文字種を検証する成分
///
/// エラーメッセージに実値を含めてよいかを成分ごとに決める。track-identifier と
/// パラメータ名は機密値でないため実値を示す。パラメータ値は認可トークンなどの機密値を
/// 含み得るため、実値を示さずパラメータ名と位置を示す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FragmentComponent<'a> {
    /// fragment の track-identifier
    TrackIdentifier,
    /// fragment のパラメータ名
    ParameterName,
    /// fragment のパラメータ値
    ParameterValue {
        /// パラメータ名 (機密値でないためメッセージに示す)
        name: &'a str,
        /// パラメータ列 (track-identifier を除く) の 1 始まりの位置
        position: usize,
    },
}

impl FragmentComponent<'_> {
    /// エラーメッセージに埋め込む成分の説明を返す
    fn describe(self, value: &str) -> String {
        match self {
            Self::TrackIdentifier => format!("MSF track identifier '{value}'"),
            Self::ParameterName => format!("MSF fragment parameter name '{value}'"),
            // パラメータ値は認可トークンなどの機密値を含み得るため実値を示さない。
            // 名前と位置は機密値でないため、該当するパラメータを特定できるように示す
            Self::ParameterValue { name, position } => {
                format!("MSF fragment parameter '{name}' value (#{position})")
            }
        }
    }
}

/// `pchar-no-amp` (任意で `/` を含む) の文字種を検証する
///
/// draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation) の ABNF に従う。
/// パラメータ値の検証では実値をエラーメッセージに埋め込まない ([`FragmentComponent`])。
fn validate_pchar_no_amp(
    value: &str,
    allow_slash: bool,
    component: FragmentComponent,
) -> Result<(), MessageError> {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if is_pchar_no_amp_byte(b) || (allow_slash && b == b'/') {
            i += 1;
        } else if b == b'%' {
            // pct-encoded = "%" HEXDIG HEXDIG
            if i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit()
            {
                return Err(MessageError::InvalidCatalog(format!(
                    "invalid percent-encoding in {}",
                    component.describe(value)
                )));
            }
            i += 3;
        } else {
            return Err(MessageError::InvalidCatalog(format!(
                "invalid character in {}",
                component.describe(value)
            )));
        }
    }
    Ok(())
}

/// `pchar-no-amp` の 1 バイトを判定する
///
/// `unreserved` / `sub-delims-no-amp` / `:` / `@` を許可する (`&` と `?` は除外)。
/// `src/msf.rs` の変数名検証からも使うため `pub(crate)` にする。
pub(crate) fn is_pchar_no_amp_byte(b: u8) -> bool {
    is_unreserved(b) || is_sub_delim_no_amp(b) || b == b':' || b == b'@'
}

/// RFC 3986 の `unreserved` を判定する
fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

/// RFC 3986 の `sub-delims` から `&` を除いたものを判定する
fn is_sub_delim_no_amp(b: u8) -> bool {
    matches!(
        b,
        b'!' | b'$' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'='
    )
}

/// wallclock-range / mediatime-range の値を範囲へ変換する
fn parse_time_range(value: &str) -> Result<MsfTimeRange, MessageError> {
    let (start, end) = match value.split_once('-') {
        Some((s, e)) => (s, if e.is_empty() { None } else { Some(e) }),
        None => (value, None),
    };
    let start_ms = parse_u64_decimal(start, "time range start")?;
    let end_ms = match end {
        Some(e) => Some(parse_u64_decimal(e, "time range end")?),
        None => None,
    };
    if let Some(end_ms) = end_ms
        && end_ms < start_ms
    {
        return Err(MessageError::InvalidCatalog(format!(
            "time range end {end_ms} is before start {start_ms}"
        )));
    }
    Ok(MsfTimeRange { start_ms, end_ms })
}

/// ロケーション上の点 (Group ID, Object ID)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct LocationPoint {
    group_id: u64,
    object_id: u64,
}

impl LocationPoint {
    fn new(group_id: u64, object_id: u64) -> Self {
        Self {
            group_id,
            object_id,
        }
    }

    /// 辞書順で次の点を返す
    fn successor(self) -> Self {
        if self.object_id < u64::MAX {
            Self::new(self.group_id, self.object_id + 1)
        } else if self.group_id < u64::MAX {
            Self::new(self.group_id + 1, 0)
        } else {
            self
        }
    }
}

/// 範囲の内側で使う点の組
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LocationSpan {
    start: LocationPoint,
    end: Option<LocationPoint>,
}

/// Group ID / Object ID の範囲を union する
///
/// 開始点でソートし、重なる・隣接する範囲を 1 つに併合する。`end` が `None` の範囲は
/// 終端なしとして、それ以降の範囲をすべて含む。Group ID だけの端点は、開始なら Object 0、
/// 終端なら Object `u64::MAX` として扱う (draft-ietf-moq-msf-02 §11.1.1)。
fn union_location_ranges(ranges: Vec<MsfLocationRange>) -> Vec<MsfLocationRange> {
    let mut spans: Vec<LocationSpan> = ranges
        .into_iter()
        .map(|range| LocationSpan {
            start: LocationPoint::new(range.start_group_id, range.start_object_id.unwrap_or(0)),
            end: range
                .end
                .map(|end| LocationPoint::new(end.group_id, end.object_id.unwrap_or(u64::MAX))),
        })
        .collect();
    spans.sort_by_key(|span| span.start);

    let mut merged: Vec<LocationSpan> = Vec::new();
    for span in spans {
        match merged.last_mut() {
            Some(current) => {
                let overlaps = match current.end {
                    None => true,
                    Some(end) => span.start <= end.successor(),
                };
                if overlaps {
                    current.end = match (current.end, span.end) {
                        (Some(left), Some(right)) => Some(left.max(right)),
                        _ => None,
                    };
                } else {
                    merged.push(span);
                }
            }
            None => merged.push(span),
        }
    }

    merged
        .into_iter()
        .map(|span| MsfLocationRange {
            start_group_id: span.start.group_id,
            start_object_id: (span.start.object_id != 0).then_some(span.start.object_id),
            end: span.end.map(|end| MsfLocationRangeEnd {
                group_id: end.group_id,
                object_id: (end.object_id != u64::MAX).then_some(end.object_id),
            }),
        })
        .collect()
}

/// 時間範囲の列を union する
///
/// 開始値でソートし、重なる・隣接する範囲を 1 つに併合する。`end_ms` が `None` の範囲は
/// 終端なしとして、それ以降の範囲をすべて含む。
fn union_time_ranges(mut ranges: Vec<MsfTimeRange>) -> Vec<MsfTimeRange> {
    ranges.sort_by_key(|range| range.start_ms);
    let mut merged: Vec<MsfTimeRange> = Vec::new();
    for range in ranges {
        match merged.last_mut() {
            Some(current) => {
                let overlaps = match current.end_ms {
                    None => true,
                    Some(end) => range.start_ms <= end.saturating_add(1),
                };
                if overlaps {
                    current.end_ms = match (current.end_ms, range.end_ms) {
                        (Some(left), Some(right)) => Some(left.max(right)),
                        _ => None,
                    };
                } else {
                    merged.push(range);
                }
            }
            None => merged.push(range),
        }
    }
    merged
}

/// location-range の値を範囲へ変換する
///
/// draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters): location-range は
/// "The '.' dot and '-' dash separators MUST be omitted when the second value is omitted."
/// のため、末尾ダッシュ (`5-`) は open range として受理しない。
fn parse_location_range(value: &str) -> Result<MsfLocationRange, MessageError> {
    let (start, end) = match value.split_once('-') {
        Some((s, e)) => {
            if e.is_empty() {
                return Err(MessageError::InvalidCatalog(format!(
                    "location range '{value}' must omit the trailing dash for an open range"
                )));
            }
            (s, Some(e))
        }
        None => (value, None),
    };
    let (start_group_id, start_object_id) = parse_location(start, "location range start")?;
    let end = match end {
        Some(e) => {
            let (group_id, object_id) = parse_location(e, "location range end")?;
            Some(MsfLocationRangeEnd {
                group_id,
                object_id,
            })
        }
        None => None,
    };
    // time range と同様、終端が開始より前なら reject する。
    // Object ID は Group ID が同じときだけ比較する。片方でも省略 (None = Group 全体) の
    // 場合は開始位置がその Group 全体を含むため「前」とは判定しない。
    if let Some(ref end) = end {
        let end_before_start = end.group_id < start_group_id
            || (end.group_id == start_group_id
                && matches!(
                    (start_object_id, end.object_id),
                    (Some(start), Some(end)) if end < start
                ));
        if end_before_start {
            return Err(MessageError::InvalidCatalog(format!(
                "location range end is before start in '{value}'"
            )));
        }
    }
    Ok(MsfLocationRange {
        start_group_id,
        start_object_id,
        end,
    })
}

/// `GroupID` / `GroupID.ObjectID` 形式の Location を `(group_id, object_id)` へ変換する
fn parse_location(value: &str, what: &str) -> Result<(u64, Option<u64>), MessageError> {
    match value.split_once('.') {
        Some((group, object)) => {
            let group_id = parse_u64_decimal(group, what)?;
            let object_id = parse_u64_decimal(object, what)?;
            Ok((group_id, Some(object_id)))
        }
        None => {
            let group_id = parse_u64_decimal(value, what)?;
            Ok((group_id, None))
        }
    }
}

/// 10 進数の u64 をパースする
fn parse_u64_decimal(value: &str, what: &str) -> Result<u64, MessageError> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(MessageError::InvalidCatalog(format!(
            "invalid {what} '{value}'"
        )));
    }
    value
        .parse::<u64>()
        .map_err(|_| MessageError::InvalidCatalog(format!("{what} '{value}' exceeds u64")))
}
