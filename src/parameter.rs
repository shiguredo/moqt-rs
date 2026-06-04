//! Setup Options エンコーディング (draft-ietf-moq-transport-21 §9.1 (SETUP))
//!
//! Setup Options は Key-Value-Pair 形式で、カウントプレフィックスなし。
//! ペイロード末尾まで KVP を読む。
//! 型番号 (type) が偶数の場合は値が varint、奇数の場合は長さ付きバイト列。
use crate::{
    error::MessageError,
    message_parameter::{AuthorizationToken, validate_auth_token_uniqueness},
    varint,
};
use alloc::vec::Vec;

/// Setup Option Type 群 (draft-ietf-moq-transport-21 §16.4 (Setup Options))
///
/// `PARAM_*` (Message Parameter Type) と同水準の公開定数。利用者が draft の
/// 生数値をベタ書きしなくて済むようにする。値は §16.4 のレジストリ表と 1 対 1 に対応する。
/// 節番号・値は draft 由来であり将来 draft 改定で変わる可能性がある。
/// PATH (0x01)
pub const SETUP_OPTION_PATH: u64 = 0x01;
/// AUTHORIZATION_TOKEN (0x03)
pub const SETUP_OPTION_AUTHORIZATION_TOKEN: u64 = 0x03;
/// MAX_AUTH_TOKEN_CACHE_SIZE (0x04)
pub const SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE: u64 = 0x04;
/// AUTHORITY (0x05)
pub const SETUP_OPTION_AUTHORITY: u64 = 0x05;
/// MAX_FILTER_RANGES (0x06)
pub const SETUP_OPTION_MAX_FILTER_RANGES: u64 = 0x06;
/// MOQT_IMPLEMENTATION (0x07)
pub const SETUP_OPTION_MOQT_IMPLEMENTATION: u64 = 0x07;
/// MAX_REQUEST_UPDATES (0x08)
pub const SETUP_OPTION_MAX_REQUEST_UPDATES: u64 = 0x08;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetupUriValidationError {
    Path,
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

fn validate_ipv6addr(s: &str) -> Result<(), SetupUriValidationError> {
    if s.is_empty() || s.len() > 39 {
        return Err(SetupUriValidationError::Authority);
    }

    // IPv4 マップアドレスの末尾部分を検出する
    let (ipv6_part, ipv4_part) = match s.rsplit_once(':') {
        Some((rest, last)) if last.contains('.') => (rest, Some(last)),
        _ => (s, None),
    };

    let has_double_colon = ipv6_part.contains("::");

    // "::" が複数回出現してはならない
    if ipv6_part.matches("::").count() > 1 {
        return Err(SetupUriValidationError::Authority);
    }

    // 各セグメントを抽出し検証する
    let segments: alloc::vec::Vec<&str> =
        ipv6_part.split(':').filter(|seg| !seg.is_empty()).collect();

    for seg in &segments {
        if seg.len() > 4 || !seg.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(SetupUriValidationError::Authority);
        }
    }

    let max_segments = if ipv4_part.is_some() { 6 } else { 8 };

    if segments.len() > max_segments {
        return Err(SetupUriValidationError::Authority);
    }

    if has_double_colon {
        // "::" が存在する場合、セグメント数は max_segments 未満でなければならない
        if segments.len() >= max_segments {
            return Err(SetupUriValidationError::Authority);
        }
    } else {
        // "::" が無い場合、セグメント数は max_segments と一致しなければならない
        if segments.len() != max_segments {
            return Err(SetupUriValidationError::Authority);
        }
    }

    // IPv4 末尾部分の検証
    if let Some(ipv4) = ipv4_part
        && !is_ipv4_address(ipv4.as_bytes())
    {
        return Err(SetupUriValidationError::Authority);
    }

    Ok(())
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

fn validate_path_abempty(value: &[u8]) -> Result<(), SetupUriValidationError> {
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

fn validate_query(value: &[u8]) -> Result<(), SetupUriValidationError> {
    validate_chars(value, SetupUriValidationError::Path, |b| {
        is_unreserved(b) || is_sub_delim(b) || matches!(b, b':' | b'@' | b'/' | b'?')
    })
}

fn validate_path(value: &[u8]) -> Result<(), SetupUriValidationError> {
    validate_uri_ascii(value, SetupUriValidationError::Path)?;
    if let Some(query_start) = value.iter().position(|&b| b == b'?') {
        validate_path_abempty(&value[..query_start])?;
        validate_query(&value[query_start + 1..])?;
        return Ok(());
    }
    validate_path_abempty(value)
}

fn validate_authority(value: &[u8]) -> Result<(), SetupUriValidationError> {
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

/// Setup Option 値
///
/// - `VarInt`: 型番号が偶数の Setup Option
/// - `Bytes`: 型番号が奇数の Setup Option (最大 65535 バイト)
#[derive(Debug, Clone, PartialEq)]
pub enum SetupOptionValue {
    /// 偶数型番号の Setup Option 値 (varint)
    VarInt(u64),
    /// 奇数型番号の Setup Option 値 (バイト列、最大 65535 バイト)
    Bytes(Vec<u8>),
    /// AUTHORIZATION_TOKEN (Option Type 0x03) の Token 構造
    /// (draft-ietf-moq-transport-21 §9.1.4 (AUTHORIZATION TOKEN))
    AuthorizationToken(AuthorizationToken),
}

/// 単一の Setup Option
#[derive(Debug, Clone, PartialEq)]
pub struct SetupOption {
    /// Setup Option の型番号 (draft-ietf-moq-transport-21 §16.4 (Setup Options))
    pub option_type: u64,
    /// Setup Option の値 (型番号が偶数なら varint、奇数ならバイト列)
    pub value: SetupOptionValue,
}

/// Setup Options リスト (draft-ietf-moq-transport-21 §9.1 (SETUP))
///
/// カウントプレフィックスなしで、エンコード時に `option_type` の昇順にソートして
/// デルタエンコードを適用する。デコード時はバッファ末尾まで KVP を読む。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SetupOptions(Vec<SetupOption>);

impl SetupOptions {
    /// 空の SetupOptions を作成する
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// 末尾に Setup Option を追加する
    pub fn push(&mut self, p: SetupOption) {
        self.0.push(p);
    }

    /// Setup Option 数を返す
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Setup Option が 1 件も含まれない場合に true を返す
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Setup Options をバッファにエンコードする (draft-ietf-moq-transport-21 §9.1 (SETUP))
    ///
    /// カウントプレフィックスなし。デルタエンコードされた KVP を直接書き出す。
    ///
    /// draft-ietf-moq-transport-21 §9.1 (SETUP): AUTHORIZATION_TOKEN 以外の同一 Option Type の
    /// 重複送信は禁止されている。重複がある場合は `InvalidParameter` を返す。
    ///
    /// 注意: この関数はワイヤフォーマットのエンコードのみを行う。
    /// draft-ietf-moq-transport-21 §9.1.1 (AUTHORITY) / §9.1.2 (PATH) が要求する以下の検証は上位層で行うこと:
    /// - PATH / AUTHORITY は client のみが送信可能 (server からの送信は MUST NOT)
    /// - WebTransport 使用時は PATH / AUTHORITY を送信してはならない (MUST NOT)
    pub fn encode(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        let mut sorted = self.0.clone();
        sorted.sort_by_key(|p| p.option_type);

        // draft-ietf-moq-transport-21 §9.1 (SETUP): 同一 Option Type の重複送信禁止
        // AUTHORIZATION_TOKEN (0x03) は複数回出現が許可されている
        for window in sorted.windows(2) {
            if window[0].option_type == window[1].option_type
                && window[0].option_type != SETUP_OPTION_AUTHORIZATION_TOKEN
            {
                return Err(MessageError::InvalidParameter);
            }
        }

        // draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter): AUTHORIZATION_TOKEN の
        // (Token Type, Token Value) は alias 解決後に一意でなければならない
        validate_auth_token_uniqueness(sorted.iter().filter_map(|p| {
            if p.option_type == SETUP_OPTION_AUTHORIZATION_TOKEN
                && let SetupOptionValue::AuthorizationToken(ref token) = p.value
            {
                Some(token)
            } else {
                None
            }
        }))?;

        let mut prev_type: u64 = 0;
        for option in &sorted {
            // draft-ietf-moq-transport-21 §9.1 (SETUP): 偶数型は varint、奇数型は長さ付きバイト列
            // 型と値形式の整合性を検証する。
            // AUTHORIZATION_TOKEN (0x03) は Token 構造専用であり、生バイト列の Bytes
            // variant は受理しない (decode が常に AuthorizationToken を返すこととの対称のため)。
            match (&option.value, option.option_type) {
                (SetupOptionValue::VarInt(_), t) if t % 2 == 0 => {}
                (SetupOptionValue::AuthorizationToken(_), SETUP_OPTION_AUTHORIZATION_TOKEN) => {}
                (SetupOptionValue::Bytes(_), t)
                    if t % 2 == 1 && t != SETUP_OPTION_AUTHORIZATION_TOKEN => {}
                _ => return Err(MessageError::InvalidParameter),
            }
            crate::kvp::encode_delta_key(prev_type, option.option_type, buf);
            match &option.value {
                SetupOptionValue::VarInt(v) => {
                    varint::encode(*v, buf);
                }
                SetupOptionValue::Bytes(bytes) => {
                    varint::encode(bytes.len() as u64, buf);
                    buf.extend_from_slice(bytes);
                }
                SetupOptionValue::AuthorizationToken(token) => {
                    // draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter):
                    // SETUP で DELETE / USE_ALIAS は PROTOCOL_VIOLATION
                    token.validate_setup_scope()?;
                    let bytes = token.encode_to_bytes();
                    varint::encode(bytes.len() as u64, buf);
                    buf.extend_from_slice(&bytes);
                }
            }
            prev_type = option.option_type;
        }
        Ok(())
    }

    /// バッファの先頭から末尾まで Setup Options をデコードし `SetupOptions` を返す
    /// (draft-ietf-moq-transport-21 §9.1 (SETUP))
    ///
    /// カウントプレフィックスなし。バッファ末尾まで KVP を読む。
    ///
    /// 注意: この関数はワイヤフォーマットのデコードのみを行う。
    /// draft-ietf-moq-transport-21 §9.1.1 (AUTHORITY) / §9.1.2 (PATH) が要求する以下の検証は上位層で行うこと:
    /// - server から PATH / AUTHORITY を受信した場合は INVALID_PATH / INVALID_AUTHORITY で閉じる
    /// - WebTransport 上で PATH / AUTHORITY を受信した場合は INVALID_PATH / INVALID_AUTHORITY で閉じる
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let mut options = Vec::new();
        let mut prev_type: u64 = 0;

        while pos < buf.len() {
            // delta==0 の重複は parameter では検出しない (AUTHORIZATION_TOKEN の重複が許容され、
            // 重複検出は post-loop で行うため delta は使わない)
            let (option_type, _) = crate::kvp::decode_delta_key(prev_type, buf, &mut pos)?;

            let value = if option_type % 2 == 0 {
                // 偶数型: varint 値
                let (v, n) = varint::decode(&buf[pos..])?;
                pos += n;
                SetupOptionValue::VarInt(v)
            } else {
                // 奇数型: 長さ付きバイト列
                let (len, n) = varint::decode(&buf[pos..])?;
                pos += n;
                if len > 65535 {
                    return Err(MessageError::ProtocolViolation(
                        "setup option value too long",
                    ));
                }
                let len = varint::checked_len(len, buf[pos..].len())?;
                let bytes = buf[pos..pos + len].to_vec();
                pos += len;

                if option_type == SETUP_OPTION_AUTHORIZATION_TOKEN {
                    // draft-ietf-moq-transport-21 §9.1.4 (AUTHORIZATION TOKEN): Message Parameter の AUTHORIZATION_TOKEN と
                    // 機能的に等価。Token 構造としてデコードする。
                    // draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter): デコード失敗時は KEY_VALUE_FORMATTING_ERROR
                    let token = AuthorizationToken::decode(&bytes)?;
                    // draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter):
                    // SETUP で DELETE / USE_ALIAS を受信した場合は PROTOCOL_VIOLATION
                    token.validate_setup_scope()?;
                    SetupOptionValue::AuthorizationToken(token)
                } else {
                    SetupOptionValue::Bytes(bytes)
                }
            };

            options.push(SetupOption { option_type, value });
            prev_type = option_type;
        }

        // draft-ietf-moq-transport-21 §9.1 (SETUP): 同一 Option Type の重複送信禁止
        // AUTHORIZATION_TOKEN (0x03) は複数回出現が許可されている
        // 未知オプションの重複は許容する
        // デルタエンコーディングにより options は type 昇順で並んでいる
        // PATH, MAX_AUTH_TOKEN_CACHE_SIZE, AUTHORITY, MAX_FILTER_RANGES, MOQT_IMPLEMENTATION, MAX_REQUEST_UPDATES
        let known_types: &[u64] = &[
            SETUP_OPTION_PATH,
            SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE,
            SETUP_OPTION_AUTHORITY,
            SETUP_OPTION_MAX_FILTER_RANGES,
            SETUP_OPTION_MOQT_IMPLEMENTATION,
            SETUP_OPTION_MAX_REQUEST_UPDATES,
        ];
        for window in options.windows(2) {
            if window[0].option_type == window[1].option_type
                && known_types.contains(&window[0].option_type)
            {
                return Err(MessageError::ProtocolViolation(
                    "duplicate known setup option type",
                ));
            }
        }

        // draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter): AUTHORIZATION_TOKEN の
        // (Token Type, Token Value) は alias 解決後に一意でなければならない
        validate_auth_token_uniqueness(options.iter().filter_map(|p| {
            if p.option_type == SETUP_OPTION_AUTHORIZATION_TOKEN
                && let SetupOptionValue::AuthorizationToken(ref token) = p.value
            {
                Some(token)
            } else {
                None
            }
        }))?;

        Ok((Self(options), pos))
    }

    /// SETUP Option: PATH (type 0x01, draft-ietf-moq-transport-21 §9.1 (SETUP))
    pub fn path(&self) -> Option<&[u8]> {
        self.find_bytes(SETUP_OPTION_PATH)
    }

    /// すべての AUTHORIZATION_TOKEN (type 0x03) を返す
    ///
    /// draft-ietf-moq-transport-21 §9.1.4 (AUTHORIZATION TOKEN): SETUP で複数の Token を指定できる
    pub fn authorization_tokens(&self) -> Vec<&AuthorizationToken> {
        self.0
            .iter()
            .filter_map(|p| {
                if p.option_type == SETUP_OPTION_AUTHORIZATION_TOKEN
                    && let SetupOptionValue::AuthorizationToken(ref t) = p.value
                {
                    Some(t)
                } else {
                    None
                }
            })
            .collect()
    }

    /// SETUP Option: MAX_AUTH_TOKEN_CACHE_SIZE (type 0x04, draft-ietf-moq-transport-21 §9.1 (SETUP))
    pub fn max_auth_token_cache_size(&self) -> Option<u64> {
        self.find_varint(SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE)
    }

    /// SETUP Option: AUTHORITY (type 0x05, draft-ietf-moq-transport-21 §9.1 (SETUP))
    pub fn authority(&self) -> Option<&[u8]> {
        self.find_bytes(SETUP_OPTION_AUTHORITY)
    }

    /// SETUP Option: MAX_REQUEST_UPDATES (type 0x08, draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES))
    ///
    /// デフォルト値 0 は無制限を意味する。
    pub fn max_request_updates(&self) -> Option<u64> {
        self.find_varint(SETUP_OPTION_MAX_REQUEST_UPDATES)
    }

    /// SETUP Option: MAX_FILTER_RANGES (type 0x06, draft-ietf-moq-transport-21 §9.1.6 (MAX FILTER RANGES))
    ///
    /// デフォルト値 0 は Range Filter の送信を禁止する。
    pub fn max_filter_ranges(&self) -> Option<u64> {
        self.find_varint(SETUP_OPTION_MAX_FILTER_RANGES)
    }

    /// 偶数型 Setup Option から varint 値を取得する
    fn find_varint(&self, option_type: u64) -> Option<u64> {
        for p in &self.0 {
            if p.option_type == option_type
                && let SetupOptionValue::VarInt(v) = p.value
            {
                return Some(v);
            }
        }
        None
    }

    /// 奇数型 Setup Option からバイト列を取得する
    fn find_bytes(&self, option_type: u64) -> Option<&[u8]> {
        for p in &self.0 {
            if p.option_type == option_type
                && let SetupOptionValue::Bytes(ref v) = p.value
            {
                return Some(v);
            }
        }
        None
    }

    pub(crate) fn validate_uri_format(&self) -> Result<(), SetupUriValidationError> {
        if let Some(path) = self.path() {
            validate_path(path)?;
        }
        if let Some(authority) = self.authority() {
            validate_authority(authority)?;
        }
        Ok(())
    }
}
