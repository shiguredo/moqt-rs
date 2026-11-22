//! RFC 3986 の URI 検証 (SETUP の PATH / AUTHORITY と MOQT URI)
//!
//! draft-ietf-moq-transport-23 §9.1.1 (AUTHORITY) / §9.1.2 (PATH) は、SETUP の
//! AUTHORITY / PATH が RFC 3986 の URI 構文に従うことを要求する。
//! MSF の MOQT URI 検証も同じ構文規則を使うため、URI 検証だけを本モジュールに集約し、
//! Setup Options の符号化 (`crate::parameter`) とは独立した責務として扱う。
//!
//! 仕様の節番号・規則は draft 由来であり、将来の draft 改定で変わる可能性がある。
use core::str::FromStr;

/// RFC 3986 の URI 構文検証で検出した違反の種別
///
/// 呼び出し側はこの種別に応じて MALFORMED_PATH / MALFORMED_AUTHORITY などの
/// セッション終了コードへ写す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetupUriValidationError {
    /// PATH が RFC 3986 の `path-abempty [ "?" query ]` を満たさない
    Path,
    /// AUTHORITY が RFC 3986 の `authority` を満たさない
    Authority,
}

/// AUTHORITY / PATH が URI 構文で使える ASCII 文字だけからなるか検証する
///
/// RFC 3986 の ABNF は US-ASCII 上で定義されるため、生の非 ASCII 文字は許可しない。
fn validate_uri_ascii(
    value: &[u8],
    kind: SetupUriValidationError,
) -> Result<(), SetupUriValidationError> {
    for &b in value {
        if b < 0x20 || b == 0x7F || b >= 0x80 {
            return Err(kind);
        }
    }
    Ok(())
}

fn is_hex_digit(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

fn is_sub_delim(b: u8) -> bool {
    matches!(
        b,
        b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'='
    )
}

fn validate_pct_encoded(
    value: &[u8],
    pos: &mut usize,
    kind: SetupUriValidationError,
) -> Result<(), SetupUriValidationError> {
    if value.len().saturating_sub(*pos) < 3
        || value[*pos] != b'%'
        || !is_hex_digit(value[*pos + 1])
        || !is_hex_digit(value[*pos + 2])
    {
        return Err(kind);
    }
    *pos += 3;
    Ok(())
}

fn validate_chars<F>(
    value: &[u8],
    kind: SetupUriValidationError,
    allowed: F,
) -> Result<(), SetupUriValidationError>
where
    F: Fn(u8) -> bool,
{
    let mut pos = 0;
    while pos < value.len() {
        if value[pos] == b'%' {
            validate_pct_encoded(value, &mut pos, kind)?;
            continue;
        }
        if !allowed(value[pos]) {
            return Err(kind);
        }
        pos += 1;
    }
    Ok(())
}

fn validate_userinfo(value: &[u8]) -> Result<(), SetupUriValidationError> {
    validate_chars(value, SetupUriValidationError::Authority, |b| {
        is_unreserved(b) || is_sub_delim(b) || b == b':'
    })
}

fn validate_reg_name(value: &[u8]) -> Result<(), SetupUriValidationError> {
    validate_chars(value, SetupUriValidationError::Authority, |b| {
        is_unreserved(b) || is_sub_delim(b)
    })
}

fn is_valid_dec_octet(part: &[u8]) -> bool {
    match part.len() {
        1 => part[0].is_ascii_digit(),
        2 => matches!(part[0], b'1'..=b'9') && part[1].is_ascii_digit(),
        3 if part[0] == b'1' => part[1].is_ascii_digit() && part[2].is_ascii_digit(),
        3 if part[0] == b'2' && matches!(part[1], b'0'..=b'4') => part[2].is_ascii_digit(),
        3 if part[0] == b'2' && part[1] == b'5' => matches!(part[2], b'0'..=b'5'),
        _ => false,
    }
}

fn is_ipv4_address(value: &[u8]) -> bool {
    let mut iter = value.split(|&b| b == b'.');
    matches!(
        (
            iter.next(),
            iter.next(),
            iter.next(),
            iter.next(),
            iter.next()
        ),
        (Some(a), Some(b), Some(c), Some(d), None)
            if is_valid_dec_octet(a)
                && is_valid_dec_octet(b)
                && is_valid_dec_octet(c)
                && is_valid_dec_octet(d)
    )
}

fn validate_ipv_future(value: &[u8]) -> Result<(), SetupUriValidationError> {
    if value.len() < 4 || !matches!(value[0], b'v' | b'V') {
        return Err(SetupUriValidationError::Authority);
    }

    let mut pos = 1;
    let hex_start = pos;
    while pos < value.len() && is_hex_digit(value[pos]) {
        pos += 1;
    }
    if pos == hex_start || pos >= value.len() || value[pos] != b'.' {
        return Err(SetupUriValidationError::Authority);
    }
    pos += 1;
    if pos == value.len() {
        return Err(SetupUriValidationError::Authority);
    }
    while pos < value.len() {
        let b = value[pos];
        if !(is_unreserved(b) || is_sub_delim(b) || b == b':') {
            return Err(SetupUriValidationError::Authority);
        }
        pos += 1;
    }
    Ok(())
}

/// RFC 3986 §3.2.2 の `IPv6address` を検証する
///
/// `IPv6address` の文法は「16 ビット単位の 16 進 8 個」を軸に、`::` による省略と末尾の
/// IPv4 表記 (`ls32 = h16 ":" h16 / IPv4address`) を許す 7 つの代替からなる。
/// `core::net::Ipv6Addr` の `FromStr` はこの文法をそのまま受理するため、手書きの
/// セグメント数え上げではなくパーサに委ねる。手書き実装では `[1:::2]` のような空セグメントを
/// 黙って読み飛ばしていた。
///
/// draft §9.1.1 (AUTHORITY) は AUTHORITY が RFC 3986 の URI formatting rules に従うことを
/// 要求し、違反時は MALFORMED_AUTHORITY でセッションを閉じることを MUST とする。
/// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
fn validate_ipv6addr(s: &str) -> Result<(), SetupUriValidationError> {
    // `Ipv6Addr` のパーサはゾーン ID (`fe80::1%25eth0` の `%25eth0` 等) を受理しないため、
    // RFC 3986 §3.2.2 の `IPv6address` にゾーン ID が含まれないことと整合する。
    core::net::Ipv6Addr::from_str(s)
        .map(|_| ())
        .map_err(|_| SetupUriValidationError::Authority)
}

fn validate_ip_literal(value: &[u8]) -> Result<(), SetupUriValidationError> {
    if matches!(value.first(), Some(b'v' | b'V')) {
        return validate_ipv_future(value);
    }

    let s = core::str::from_utf8(value).map_err(|_| SetupUriValidationError::Authority)?;
    validate_ipv6addr(s)?;
    Ok(())
}

fn validate_port(value: &[u8]) -> Result<(), SetupUriValidationError> {
    if value.iter().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(SetupUriValidationError::Authority)
    }
}

fn validate_authority_host_port(value: &[u8]) -> Result<(), SetupUriValidationError> {
    if value.is_empty() {
        return Err(SetupUriValidationError::Authority);
    }

    if value[0] == b'[' {
        let closing = value
            .iter()
            .position(|&b| b == b']')
            .ok_or(SetupUriValidationError::Authority)?;
        if closing == 1 {
            return Err(SetupUriValidationError::Authority);
        }
        validate_ip_literal(&value[1..closing])?;
        let rest = &value[closing + 1..];
        if rest.is_empty() {
            return Ok(());
        }
        if rest[0] != b':' {
            return Err(SetupUriValidationError::Authority);
        }
        return validate_port(&rest[1..]);
    }

    let (host, port) = if let Some(idx) = value.iter().rposition(|&b| b == b':') {
        let host = &value[..idx];
        if host.contains(&b':') {
            return Err(SetupUriValidationError::Authority);
        }
        (host, Some(&value[idx + 1..]))
    } else {
        (value, None)
    };

    if host.is_empty() {
        return Err(SetupUriValidationError::Authority);
    }
    if let Some(port) = port {
        validate_port(port)?;
    }
    if is_ipv4_address(host) {
        return Ok(());
    }
    validate_reg_name(host)
}

/// RFC 3986 §3.3 の `path-abempty` を検証する
///
/// 空文字列、または `/` で始まり各セグメントが pchar (`unreserved` / `pct-encoded` /
/// `sub-delims` / `:` / `@`) だけからなるパスを受理する。query は含めない。
pub(crate) fn validate_path_abempty(value: &[u8]) -> Result<(), SetupUriValidationError> {
    if value.is_empty() {
        return Ok(());
    }
    if value[0] != b'/' {
        return Err(SetupUriValidationError::Path);
    }

    let mut pos = 0;
    while pos < value.len() {
        if value[pos] != b'/' {
            return Err(SetupUriValidationError::Path);
        }
        pos += 1;
        let segment_start = pos;
        while pos < value.len() && value[pos] != b'/' {
            pos += 1;
        }
        validate_chars(
            &value[segment_start..pos],
            SetupUriValidationError::Path,
            |b| is_unreserved(b) || is_sub_delim(b) || matches!(b, b':' | b'@'),
        )?;
    }
    Ok(())
}

/// RFC 3986 §3.4 の `query` を検証する
///
/// `query` は pchar と `/` / `?` からなる。空文字列も正当である。
pub(crate) fn validate_query(value: &[u8]) -> Result<(), SetupUriValidationError> {
    validate_chars(value, SetupUriValidationError::Path, |b| {
        is_unreserved(b) || is_sub_delim(b) || matches!(b, b':' | b'@' | b'/' | b'?')
    })
}

/// SETUP の PATH (RFC 3986 の `path-abempty [ "?" query ]`) を検証する
///
/// draft-ietf-moq-transport-23 §9.1.2 (PATH) が要求する URI 構文の検証であり、
/// 違反時は呼び出し側が MALFORMED_PATH でセッションを閉じる。
pub(crate) fn validate_path(value: &[u8]) -> Result<(), SetupUriValidationError> {
    validate_uri_ascii(value, SetupUriValidationError::Path)?;
    if let Some(query_start) = value.iter().position(|&b| b == b'?') {
        validate_path_abempty(&value[..query_start])?;
        validate_query(&value[query_start + 1..])?;
        return Ok(());
    }
    validate_path_abempty(value)
}

/// RFC 3986 §3.2 の `authority` を検証する
///
/// `[ userinfo "@" ] host [ ":" port ]` の構文を検証する。host は reg-name / IPv4address /
/// `"[" ( IPv6address / IPvFuture ) "]"` のいずれかである。
/// draft-ietf-moq-transport-23 §9.1.1 (AUTHORITY) が要求する URI 構文の検証であり、
/// 違反時は呼び出し側が MALFORMED_AUTHORITY でセッションを閉じる。
pub(crate) fn validate_authority(value: &[u8]) -> Result<(), SetupUriValidationError> {
    validate_uri_ascii(value, SetupUriValidationError::Authority)?;
    if value.is_empty() {
        return Err(SetupUriValidationError::Authority);
    }

    let host_port = if let Some(idx) = value.iter().rposition(|&b| b == b'@') {
        validate_userinfo(&value[..idx])?;
        &value[idx + 1..]
    } else {
        value
    };
    validate_authority_host_port(host_port)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 3986 §3.2.2 の `IPv6address` として不正なリテラルを拒否すること
    ///
    /// 空セグメントを読み飛ばす手書き実装では `1:::2` や先頭・末尾の余分な `:` が
    /// 受理されていた。`IPv6address` の ABNF は空の 16 ビット単位を許さない。
    #[test]
    fn rejects_invalid_ipv6_literal() {
        for literal in [
            // "::" の前後に空セグメントが付く
            "1:::2",
            "1:2:3:4:5:6:7:8:",
            ":1:2:3:4:5:6:7:8",
            // 16 ビット単位が 9 個
            "1:2:3:4:5:6:7:8:9",
            // 16 進でない文字
            "gggg::1",
            // 空文字
            "",
        ] {
            assert!(
                validate_ipv6addr(literal).is_err(),
                "{literal:?} は IPv6address として拒否されること"
            );
        }
    }

    /// RFC 3986 §3.2.2 の `IPv6address` として正当なリテラルを受理すること
    ///
    /// IPv4-mapped 表記は 45 文字になり、旧実装の長さ上限 39 では拒否されていた。
    /// `"::" ls32` 形式 (`::1.2.3.4`) も ABNF の 5 番目の代替として正当である。
    #[test]
    fn accepts_valid_ipv6_literal() {
        for literal in [
            "::",
            "::1",
            "2001:db8::1",
            "::1.2.3.4",
            "::ffff:192.0.2.1",
            "1:2:3:4:5:6:1.2.3.4",
            // IPv4-mapped 表記の最大長 (45 文字)
            "0000:0000:0000:0000:0000:ffff:255.255.255.255",
            "1:2:3:4:5:6:7:8",
        ] {
            assert!(
                validate_ipv6addr(literal).is_ok(),
                "{literal:?} は IPv6address として受理されること"
            );
        }
    }

    /// IPvFuture の検証を維持すること
    ///
    /// `validate_ip_literal` の `v` / `V` 分岐は IPvFuture であり、IPv6 パーサへ
    /// 委ねてはならない (ABNF の `IPvFuture` は `v` で始まる)。
    #[test]
    fn ipv_future_is_still_accepted() {
        assert!(validate_ip_literal(b"v1.fe80::").is_ok());
        assert!(validate_ip_literal(b"V1.fe80::").is_ok());
        assert!(validate_ip_literal(b"v1").is_err());
    }
}
