//! Namespace / Track Name のシリアライズ表現とパース (draft-ietf-moq-transport-21 §8.8 (Representing Namespace and Track Names) / draft-ietf-moq-transport-21 §8.8.1 (Parsing Serialized Names))
//!
//! binary な namespace タプルと track name を、ファイル名・ URL セーフな文字列へ変換する
//! (draft-ietf-moq-transport-21 §8.8 (Representing Namespace and Track Names)、RECOMMENDED) ユーティリティ。ログ・ファイル名・一部の認可検証で使う想定で、protocol
//! state machine の動作には影響しない。
//!
//! # シリアライズ (draft-ietf-moq-transport-21 §8.8 (Representing Namespace and Track Names))
//!
//! - 各 namespace タプルをハイフン (`-`) 区切りで並べ、最後の namespace と track name の間は二重
//!   ハイフン (`--`) で区切る。
//! - `a-z` / `A-Z` / `0-9` / `_` (0x5f) はそのまま、それ以外のバイトはピリオド (`.`) + 小文字 hex 2 桁。
//!
//! # パース (draft-ietf-moq-transport-21 §8.8.1 (Parsing Serialized Names)、MUST)
//!
//! - ピリオド後の hex は小文字必須 (大文字は invalid)。
//! - リテラル表現可能バイトの hex 化 (例 `.61`) は冗長として拒否。
//! - ピリオド直後は必ず hex 2 桁 (末尾ピリオド・ 1 桁・非 hex は invalid)。
//!
//! これにより encoding は bijective (draft-ietf-moq-transport-21 §8.8.1 (Parsing Serialized Names))。なお `-` (0x2d) / `.` (0x2e) はリテラル範囲外
//! なので必ず `.2d` / `.2e` にエンコードされ、シリアライズ文字列中の裸の `-` は区切りとしてしか現れない。
//! 本ライブラリの `TrackNamespace` は空フィールドを表現できないため、bijective 性は `TrackNamespace::new`
//! を通せる binary 値に限定して成立する。
//!
//! この節番号・規則は draft-ietf-moq-transport-21 由来であり、将来の draft 改版で変わる可能性がある。

use crate::message::{common::MAX_TRACK_NAME_LENGTH, common::TrackNamespace};
use alloc::string::String;
use alloc::vec::Vec;

/// `parse_name` のエラー (draft-ietf-moq-transport-21 §8.8.1 (Parsing Serialized Names))
///
/// draft-ietf-moq-transport-21 §8.8.1 (Parsing Serialized Names) のパース失敗は application-defined であり protocol violation ではない。呼び出し側が
/// セッションを閉じるかどうかを判断する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameParseError {
    /// ピリオド後の hex が大文字 (draft-ietf-moq-transport-21 §8.8.1 (Parsing Serialized Names): 小文字必須)
    UppercaseHex,
    /// リテラル表現可能バイトの hex 化 (例 `.61`、draft-ietf-moq-transport-21 §8.8.1 (Parsing Serialized Names): 冗長エンコード禁止)
    RedundantEncoding,
    /// 末尾ピリオド / hex 1 桁 / 非 hex 文字、または想定外のバイト (裸の `-` 等)
    InvalidEscape,
    /// 連続 / 先頭 / 末尾ハイフンによる空の namespace フィールド
    EmptyNamespaceField,
    /// namespace と track name の境界 `--` が見つからない
    MissingSeparator,
    /// 境界 `--` が 2 回以上、またはハイフンの連続ラン長が 3 以上
    MultipleSeparators,
    /// namespace + track name の合計が 4096 バイトを超える (draft-ietf-moq-transport-21 §2.4.1 (Track Naming))
    FullNameTooLong,
    /// `TrackNamespace::new` のフィールド数 (32 超) 制約違反 (draft-ietf-moq-transport-21 §2.4.1 (Track Naming))
    InvalidNamespace,
}

/// バイトがリテラル出力可能か (`a-z` / `A-Z` / `0-9` / `_`)
fn is_literal(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 0..=15 の nibble を小文字 hex 文字へ変換する
fn hex_digit(nibble: u8) -> char {
    char::from(b"0123456789abcdef"[nibble as usize])
}

/// 小文字 hex 文字を 0..=15 へ変換する。大文字は `UppercaseHex`、非 hex は `InvalidEscape`。
fn decode_hex(b: u8) -> Result<u8, NameParseError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Err(NameParseError::UppercaseHex),
        _ => Err(NameParseError::InvalidEscape),
    }
}

/// 単一フィールド (namespace フィールド 1 つ、または track name) を draft-ietf-moq-transport-21 §8.8 (Representing Namespace and Track Names) 表現で `out` に追記する
fn serialize_field(bytes: &[u8], out: &mut String) {
    for &b in bytes {
        if is_literal(b) {
            out.push(char::from(b));
        } else {
            out.push('.');
            out.push(hex_digit(b >> 4));
            out.push(hex_digit(b & 0x0f));
        }
    }
}

/// namespace タプル + track name を draft-ietf-moq-transport-21 §8.8 (Representing Namespace and Track Names) のシリアライズ文字列へ変換する
///
/// 0 フィールドの namespace は先頭が `--` の文字列になる。任意のバイト列を表現できるため infallible。
/// `TrackNamespace` は構築時に `new` で検証済みのため、ここでは再検証しない。
pub fn serialize_name(namespace: &TrackNamespace, track_name: &[u8]) -> String {
    let mut out = String::new();
    for (i, field) in namespace.fields().iter().enumerate() {
        if i > 0 {
            out.push('-');
        }
        serialize_field(field, &mut out);
    }
    // namespace と track name の境界 (0 フィールドでも `--` を出力する)
    out.push('-');
    out.push('-');
    serialize_field(track_name, &mut out);
    out
}

/// draft-ietf-moq-transport-21 §8.8 (Representing Namespace and Track Names) 表現の単一フィールドを binary バイト列へデコードする (draft-ietf-moq-transport-21 §8.8.1 (Parsing Serialized Names) の MUST を厳格に適用)
fn decode_field(bytes: &[u8]) -> Result<Vec<u8>, NameParseError> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if is_literal(b) {
            out.push(b);
            i += 1;
        } else if b == b'.' {
            // ピリオドの直後は必ず小文字 hex 2 桁
            if i + 2 >= bytes.len() {
                return Err(NameParseError::InvalidEscape);
            }
            let decoded = (decode_hex(bytes[i + 1])? << 4) | decode_hex(bytes[i + 2])?;
            // リテラル表現可能バイトの hex 化は冗長 (例 `.61`)
            if is_literal(decoded) {
                return Err(NameParseError::RedundantEncoding);
            }
            out.push(decoded);
            i += 3;
        } else {
            // 裸の `-` を含む想定外バイト (リテラルでも `.` でもない)
            return Err(NameParseError::InvalidEscape);
        }
    }
    Ok(out)
}

/// draft-ietf-moq-transport-21 §8.8.1 (Parsing Serialized Names) の MUST を厳格に適用してシリアライズ文字列を namespace タプル + track name へパースする
pub fn parse_name(s: &str) -> Result<(TrackNamespace, Vec<u8>), NameParseError> {
    let bytes = s.as_bytes();

    // 境界 `--` (ハイフンの極大連続ラン長 2) を一意に特定する。
    // ラン長 1 = namespace 区切り、ラン長 2 = 境界、ラン長 3 以上 = 不正 (draft-ietf-moq-transport-21 §8.8 (Representing Namespace and Track Names) 不変条件)。
    let mut boundary: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'-' {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i] == b'-' {
            i += 1;
        }
        match i - start {
            1 => {} // namespace 区切り。namespace 部の分割で処理する
            2 => {
                if boundary.is_some() {
                    return Err(NameParseError::MultipleSeparators);
                }
                boundary = Some(start);
            }
            _ => return Err(NameParseError::MultipleSeparators),
        }
    }
    let boundary = boundary.ok_or(NameParseError::MissingSeparator)?;

    let ns_part = &bytes[..boundary];
    let track_part = &bytes[boundary + 2..];

    // track name 部をデコードする (裸の `-` を含む想定外バイトは decode_field が InvalidEscape で弾く)
    let track_name = decode_field(track_part)?;

    // namespace 部を `-` で分割して各フィールドをデコードする。
    // ns_part が空文字列なら 0 フィールド (空フィールド 1 個と混同しない)。
    let mut fields = Vec::new();
    if !ns_part.is_empty() {
        for field_bytes in ns_part.split(|&b| b == b'-') {
            if field_bytes.is_empty() {
                return Err(NameParseError::EmptyNamespaceField);
            }
            fields.push(decode_field(field_bytes)?);
        }
    }

    // draft-ietf-moq-transport-21 §2.4.1 (Track Naming): Full Track Name (namespace + track name) の合計は 4096 バイト以下。
    // parse_name は `TrackNamespace` を返すため (型不変条件で namespace 単独の 4096 / 32 フィールド制約は
    // 必ず満たす)、Full Track Name 制約の範囲内で動作する。合計長は `TrackNamespace::new` が見ない
    // (namespace 単独しか見ない) ため、ここで合計を検証する。`new` の namespace 単独 4096 検査は、この
    // 合計検査を通過した時点で namespace 単独も 4096 以下が保証されるため発火しない。
    let ns_len: usize = fields.iter().map(Vec::len).sum();
    if ns_len + track_name.len() > MAX_TRACK_NAME_LENGTH {
        return Err(NameParseError::FullNameTooLong);
    }

    // フィールド数 (<=32) を TrackNamespace::new で検証する。空フィールドと合計長は上で検証済みのため、
    // ここで返り得るエラーはフィールド数超過のみ。draft-ietf-moq-transport-21 §2.4.1 (Track Naming) の wire 制約 (PROTOCOL_VIOLATION) の意味は
    // 持ち込まず NameParseError::InvalidNamespace へ写像する。
    let namespace = TrackNamespace::new(fields).map_err(|_| NameParseError::InvalidNamespace)?;

    Ok((namespace, track_name))
}
