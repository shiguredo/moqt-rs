//! MSF URI と fragment のパース (draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation))
//!
//! `moqt://` URI の fragment 部 (`msf:...`) をパースし、MSF namespace-name 文字列を
//! [`crate::name::parse_name`] で namespace / track name へ分解する。
//! 予約 fragment パラメータ (§11.1.1 (Reserved fragment parameters)) の値型アクセサを提供する。
//!
//! 本モジュールは URI の構文解析のみを行い、MOQT セッションの確立や SUBSCRIBE / FETCH の
//! 発行は行わない。percent-decode は行わず、値をそのまま保持する。
//! この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。

use crate::{error::MessageError, message::common::TrackNamespace, name};
use alloc::{format, string::String, string::ToString, vec::Vec};

/// MSF URI の fragment (draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation))
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

/// MSF fragment の 1 パラメータ (draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation))
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsfFragmentParameter {
    /// パラメータ名 (case-sensitive)
    pub name: String,
    /// パラメータ値 (空文字列を許容する)
    pub value: String,
}

/// connection パラメータが要求する接続種別 (draft-ietf-moq-msf-01 §11.1.1 (Reserved fragment parameters))
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsfConnectionType {
    /// `q`: Native QUIC 接続を必須とする
    Quic,
    /// `wt`: WebTransport 接続を必須とする
    WebTransport,
}

/// wallclock-range / mediatime-range の範囲 (draft-ietf-moq-msf-01 §11.1.1 (Reserved fragment parameters))
///
/// 両端は inclusive。`end_ms` が `None` の場合は open range を表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsfTimeRange {
    /// 範囲の開始値 (wallclock ms または media time ms)
    pub start_ms: u64,
    /// 範囲の終了値。`None` は終端なし (open range)
    pub end_ms: Option<u64>,
}

/// location-range の終端 (draft-ietf-moq-msf-01 §11.1.1 (Reserved fragment parameters))
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsfLocationRangeEnd {
    /// 終端 Group ID
    pub group_id: u64,
    /// 終端 Object ID。`None` は終端 Group 全体を含む
    pub object_id: Option<u64>,
}

/// location-range の範囲 (draft-ietf-moq-msf-01 §11.1.1 (Reserved fragment parameters))
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

/// パース済みの MSF URI (draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation))
///
/// fragment 以外の要素は接続確立に使う付随情報として保持する。
#[derive(Debug, Clone, PartialEq)]
pub struct MsfUri {
    /// authority (host + optional port)
    pub authority: String,
    /// path (先頭の `/` を含む。無い場合は空文字列)
    pub path: String,
    /// query (`?` 以降。無い場合は `None`)
    pub query: Option<String>,
    /// `#` 以降の fragment
    pub fragment: MsfFragment,
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
    /// 複数指定された場合は出現順に返す (§11.1.1 (Reserved fragment parameters) の union 規則)。
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

    /// c4m パラメータの base64 文字列を出現順に返す
    ///
    /// base64 のデコードや妥当性検証は行わない (利用側の責務)。
    pub fn c4m_tokens(&self) -> Vec<&str> {
        self.parameter_values("c4m")
    }
}

/// MSF URI 全体をパースする
///
/// draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation) の
/// `msf-uri = "moqt://" authority path-abempty [ "?" query ] "#" msf-fragment` に従う。
/// 同節は scheme を case-insensitive と規定するため、`MOQT://` なども受理する。
///
/// # Errors
///
/// - `moqt://` scheme でない / `#` が無い / authority が空: `InvalidCatalog`
/// - fragment が `msf:` で始まらない / 形式不正: `InvalidCatalog`
pub fn parse_msf_uri(uri: &str) -> Result<MsfUri, MessageError> {
    // draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation):
    // "Scheme: This case-insensitive scheme defines the underlying transport."
    // バイト境界を跨ぐ slice で panic しないよう get で前方一致を確認する。
    const SCHEME: &str = "moqt://";
    let rest = match uri.get(..SCHEME.len()) {
        Some(prefix) if prefix.eq_ignore_ascii_case(SCHEME) => &uri[SCHEME.len()..],
        _ => {
            return Err(MessageError::InvalidCatalog(
                "MSF URI must use the 'moqt://' scheme".to_string(),
            ));
        }
    };
    let (before_fragment, fragment_str) = rest
        .split_once('#')
        .ok_or_else(|| MessageError::InvalidCatalog("MSF URI is missing a fragment".to_string()))?;
    let (before_query, query) = match before_fragment.split_once('?') {
        Some((b, q)) => (b, Some(q.to_string())),
        None => (before_fragment, None),
    };
    let (authority, path) = match before_query.find('/') {
        Some(idx) => (&before_query[..idx], &before_query[idx..]),
        None => (before_query, ""),
    };
    if authority.is_empty() {
        return Err(MessageError::InvalidCatalog(
            "MSF URI authority is empty".to_string(),
        ));
    }
    let fragment = parse_msf_fragment(fragment_str)?;
    Ok(MsfUri {
        authority: authority.to_string(),
        path: path.to_string(),
        query,
        fragment,
    })
}

/// `#` 以降の fragment (`msf:...`) をパースする
///
/// draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation) の ABNF に従う。
/// track-identifier は §11.1.2 (MSF Namespace-Name String Encoding) の表現として
/// [`crate::name::parse_name`] で分解する。
///
/// # Errors
///
/// - `msf:` で始まらない / track-identifier が空 / パラメータ形式不正: `InvalidCatalog`
/// - track-identifier が namespace-name 表現として不正: `InvalidCatalog`
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
    validate_pchar_no_amp(track_identifier, true)?;
    let (namespace, track_name) = name::parse_name(track_identifier).map_err(|e| {
        MessageError::InvalidCatalog(format!("invalid MSF track identifier: {e:?}"))
    })?;

    let mut parameters = Vec::new();
    for element in parts {
        if element.is_empty() {
            return Err(MessageError::InvalidCatalog(
                "MSF fragment contains an empty parameter".to_string(),
            ));
        }
        let (name, value) = element.split_once('=').ok_or_else(|| {
            MessageError::InvalidCatalog(format!(
                "MSF fragment parameter '{element}' is missing '='"
            ))
        })?;
        if name.is_empty() {
            return Err(MessageError::InvalidCatalog(
                "MSF fragment parameter name is empty".to_string(),
            ));
        }
        validate_pchar_no_amp(name, true)?;
        validate_pchar_no_amp(value, true)?;
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

/// `pchar-no-amp` (任意で `/` を含む) の文字種を検証する
///
/// draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation) の ABNF に従う。
fn validate_pchar_no_amp(value: &str, allow_slash: bool) -> Result<(), MessageError> {
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
                    "invalid percent-encoding in MSF fragment component '{value}'"
                )));
            }
            i += 3;
        } else {
            return Err(MessageError::InvalidCatalog(format!(
                "invalid character in MSF fragment component '{value}'"
            )));
        }
    }
    Ok(())
}

/// `pchar-no-amp` の 1 バイトを判定する
///
/// `unreserved` / `sub-delims-no-amp` / `:` / `@` を許可する (`&` と `?` は除外)。
fn is_pchar_no_amp_byte(b: u8) -> bool {
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

/// location-range の値を範囲へ変換する
///
/// draft-ietf-moq-msf-01 §11.1.1 (Reserved fragment parameters): location-range は
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
