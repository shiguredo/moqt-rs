//! RFC 8949 Section 8 の診断記法 (diagnostic notation) を [`Value`] の [`Display`] として実装するモジュール

use crate::cbor::value::Value;
use alloc::format;
use alloc::string::{String, ToString};
use core::fmt::{self, Write};

/// 10 進表記と指数表記の切り替えに使う下限
const DECIMAL_LOWER_BOUND: f64 = 1.0e-5;

/// 10 進表記と指数表記の切り替えに使う上限
const DECIMAL_UPPER_BOUND: f64 = 1.0e17;

/// 16 進数の桁
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

impl fmt::Display for Value {
    /// RFC 8949 Section 8 の診断記法 (diagnostic notation) で出力する
    ///
    /// このライブラリの [`Value`] は indefinite-length 表現や浮動小数点の幅を
    /// 保持しないため、エンコード指示子 (`_` や `_1` など) は出力しない。
    /// タグ付きの bignum (タグ 2, 3) は RFC 8949 Appendix A のような 10 進数
    /// ではなく `2(h'...')` の形式で出力する。非 ASCII 文字はそのままの
    /// UTF-8 で出力する。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Unsigned(value) => write!(f, "{value}"),
            Value::Negative(value) => write!(f, "{}", -1 - i128::from(*value)),
            Value::ByteString(value) => write_byte_string(f, value),
            Value::TextString(value) => write_text_string(f, value),
            Value::Array(items) => {
                f.write_char('[')?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_char(']')
            }
            Value::Map(pairs) => {
                f.write_char('{')?;
                for (index, (key, value)) in pairs.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{key}: {value}")?;
                }
                f.write_char('}')
            }
            Value::Tag(tag, content) => write!(f, "{tag}({content})"),
            Value::Simple(value) => write!(f, "simple({value})"),
            Value::Bool(true) => f.write_str("true"),
            Value::Bool(false) => f.write_str("false"),
            Value::Null => f.write_str("null"),
            Value::Undefined => f.write_str("undefined"),
            Value::Float(value) => f.write_str(&format_float(*value)),
        }
    }
}

/// バイト文字列を `h'...'` 形式で出力する
fn write_byte_string(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    f.write_str("h'")?;
    for byte in bytes {
        f.write_char(HEX_DIGITS[(byte >> 4) as usize] as char)?;
        f.write_char(HEX_DIGITS[(byte & 0x0f) as usize] as char)?;
    }
    f.write_char('\'')
}

/// テキスト文字列を JSON 互換のエスケープで出力する
///
/// RFC 8949 Section 8 のとおり、診断記法は JSON の文字列構文を借用している。
fn write_text_string(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    f.write_char('"')?;
    for character in text.chars() {
        match character {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\u{0008}' => f.write_str("\\b")?,
            '\u{000c}' => f.write_str("\\f")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            _ if (character as u32) < 0x20 => write!(f, "\\u{:04x}", character as u32)?,
            _ => f.write_char(character)?,
        }
    }
    f.write_char('"')
}

/// 浮動小数点値を診断記法の形式で出力する
///
/// 有限値は常に小数点を含む 10 進表記または指数表記 (`1.0e+300` など) で出力し、
/// 非有限値は `Infinity` / `-Infinity` / `NaN` と出力する。
fn format_float(value: f64) -> String {
    if value.is_nan() {
        return String::from("NaN");
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            String::from("-Infinity")
        } else {
            String::from("Infinity")
        };
    }
    if value == 0.0 {
        // 符号付きゼロを区別して出力する
        return if value.is_sign_negative() {
            String::from("-0.0")
        } else {
            String::from("0.0")
        };
    }

    let absolute = value.abs();
    if !(DECIMAL_LOWER_BOUND..DECIMAL_UPPER_BOUND).contains(&absolute) {
        // 指数表記: 仮数部に必ず小数点を付け、指数部には符号を付ける
        let formatted = format!("{value:e}");
        let (mantissa, exponent) = formatted
            .split_once('e')
            .expect("the exponent of a formatted float always contains 'e'");
        let exponent: i32 = exponent
            .parse()
            .expect("the exponent of a formatted float is always an integer");
        let mantissa = if mantissa.contains('.') {
            mantissa.to_string()
        } else {
            format!("{mantissa}.0")
        };
        let sign = if exponent < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{}", exponent.abs());
    }

    // 10 進表記: 整数値には ".0" を付ける
    let formatted = format!("{value}");
    if formatted.contains('.') {
        formatted
    } else {
        format!("{formatted}.0")
    }
}
