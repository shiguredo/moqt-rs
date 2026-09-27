//! CBOR (Concise Binary Object Representation) のコーデック
//!
//! RFC 8949 のデータ項目を `no_std` でエンコード / デコードする。CWT / COSE / CAT
//! (draft-ietf-moq-c4m-01) が必要とする範囲を対象とし、エンコードは RFC 8949 §4.2
//! の決定論的エンコード (Deterministically Encoded CBOR) に従う。
//!
//! - 整数と長さは最小の長さでエンコードする
//! - マップのキーはキーのエンコード済みバイト列の昇順に並べる (§4.2.3)
//! - 浮動小数点数は値を保つ最短の幅 (半精度 / 単精度 / 倍精度) を使い、NaN は
//!   `f9 7e 00` にする
//! - デコードは definite / indefinite の両方の長さ表現を受理する。マップの重複キー、
//!   UTF-8 でないテキスト文字列、ネスト深度の上限超過はエラーにする

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// ネスト深度の上限
///
/// デコード / エンコードの再帰呼び出しによるスタック枯渇を防ぐ。CWT / COSE / CAT が
/// 必要とする構造はこれより十分浅い。
pub const MAX_DEPTH: usize = 64;

/// CBOR のデータ項目 (RFC 8949 §3)
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// 符号なし整数 (major type 0)
    Unsigned(u64),
    /// 負の整数 (major type 1)。実際の値は `-1 - n`
    Negative(u64),
    /// バイト文字列 (major type 2)
    ByteString(Vec<u8>),
    /// テキスト文字列 (major type 3)
    TextString(String),
    /// 配列 (major type 4)
    Array(Vec<Value>),
    /// マップ (major type 5)
    ///
    /// エンコード時はキーの決定論的順序に並べ替える。デコード時は入力の順序を保つ。
    Map(Vec<(Value, Value)>),
    /// タグ付きデータ項目 (major type 6)
    Tag(u64, Box<Value>),
    /// 真偽値 (major type 7 の 20 / 21)
    Bool(bool),
    /// null (major type 7 の 22)
    Null,
    /// undefined (major type 7 の 23)
    Undefined,
    /// 浮動小数点数 (major type 7 の 25 / 26 / 27)
    Float(f64),
    /// 上記以外の単純値 (major type 7 の 0 〜 19 と 32 〜 255)
    Simple(u8),
}

impl Value {
    /// `i64` から整数のデータ項目を作る
    pub fn integer(value: i64) -> Self {
        if value >= 0 {
            Self::Unsigned(value as u64)
        } else {
            Self::Negative((-1 - value) as u64)
        }
    }

    /// マップからキーに対応する値を取り出す
    ///
    /// マップ以外では常に `None` を返す。
    pub fn map_get(&self, key: &Value) -> Option<&Value> {
        match self {
            Self::Map(entries) => entries
                .iter()
                .find(|(entry_key, _)| entry_key == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// 符号なし整数として取り出す
    pub fn as_unsigned(&self) -> Option<u64> {
        match self {
            Self::Unsigned(value) => Some(*value),
            _ => None,
        }
    }

    /// `i64` の範囲に収まる整数として取り出す
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Unsigned(value) => i64::try_from(*value).ok(),
            Self::Negative(value) => {
                let magnitude = i64::try_from(*value).ok()?;
                Some(-1 - magnitude)
            }
            _ => None,
        }
    }

    /// 整数または浮動小数点数の数値として取り出す
    ///
    /// 負の整数は 1 回だけ丸めて `f64` へ変換する (`-1.0 - n as f64` のような
    /// 2 段階の丸めでは 2^53 を超える値がずれる)。
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Unsigned(value) => Some(*value as f64),
            Self::Negative(value) => {
                let exact = -(*value as i128) - 1;
                Some(exact as f64)
            }
            Self::Float(value) => Some(*value),
            _ => None,
        }
    }

    /// バイト文字列として取り出す
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::ByteString(value) => Some(value),
            _ => None,
        }
    }

    /// テキスト文字列として取り出す
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::TextString(value) => Some(value),
            _ => None,
        }
    }

    /// 配列として取り出す
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array(values) => Some(values),
            _ => None,
        }
    }

    /// マップとして取り出す
    pub fn as_map(&self) -> Option<&[(Value, Value)]> {
        match self {
            Self::Map(entries) => Some(entries),
            _ => None,
        }
    }

    /// 真偽値として取り出す
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }
}

/// CBOR のエンコード / デコードエラー
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CborError {
    /// 入力が途中で終わっている
    UnexpectedEof,
    /// additional information が予約値 (28 〜 30) または不正な値である
    InvalidAdditionalInformation(u8),
    /// 単純値が予約値 (24 〜 31) である
    InvalidSimpleValue(u8),
    /// テキスト文字列が UTF-8 として不正である
    InvalidUtf8,
    /// indefinite 長の外で break が現れた
    BreakOutsideIndefinite,
    /// indefinite 長のチャンクの型が本体と一致しない
    InvalidIndefiniteChunk,
    /// マップのキーが重複している (RFC 8949 §5.6 は一意性を要求する)
    DuplicateMapKey,
    /// ネスト深度が [`MAX_DEPTH`] を超えた
    DepthLimitExceeded,
    /// 長さがプラットフォームの `usize` に収まらない
    LengthOverflow,
    /// データ項目の後に余分なバイトがある
    TrailingBytes,
}

impl fmt::Display for CborError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof => write!(f, "unexpected end of input"),
            Self::InvalidAdditionalInformation(ai) => {
                write!(f, "invalid additional information: {ai:#x}")
            }
            Self::InvalidSimpleValue(value) => write!(f, "invalid simple value: {value:#x}"),
            Self::InvalidUtf8 => write!(f, "text string is not valid UTF-8"),
            Self::BreakOutsideIndefinite => {
                write!(f, "break code outside of an indefinite-length item")
            }
            Self::InvalidIndefiniteChunk => {
                write!(f, "indefinite-length chunk has a different major type")
            }
            Self::DuplicateMapKey => write!(f, "duplicate map key"),
            Self::DepthLimitExceeded => write!(f, "nesting depth exceeds {MAX_DEPTH}"),
            Self::LengthOverflow => write!(f, "length does not fit in usize"),
            Self::TrailingBytes => write!(f, "trailing bytes after the data item"),
        }
    }
}

impl core::error::Error for CborError {}

/// CBOR のデータ項目をエンコードする
///
/// RFC 8949 §4.2 の決定論的エンコードに従う。マップに重複するキーがある場合は
/// [`CborError::DuplicateMapKey`] を返す。
pub fn encode(value: &Value) -> Result<Vec<u8>, CborError> {
    let mut out = Vec::new();
    encode_into(value, &mut out)?;
    Ok(out)
}

/// CBOR のデータ項目を既存のバッファへ追記する
///
/// 失敗した場合、バッファには途中まで書かれたバイト列が残る。
pub fn encode_into(value: &Value, out: &mut Vec<u8>) -> Result<(), CborError> {
    encode_value(value, 0, out)
}

/// CBOR のデータ項目をデコードする
///
/// 入力の全バイトをデータ項目として消費する。末尾に余分なバイトがある場合は
/// [`CborError::TrailingBytes`] を返す。
pub fn decode(bytes: &[u8]) -> Result<Value, CborError> {
    let (value, consumed) = decode_partial(bytes)?;
    if consumed != bytes.len() {
        return Err(CborError::TrailingBytes);
    }
    Ok(value)
}

/// CBOR のデータ項目をデコードし、消費したバイト数を返す
pub fn decode_partial(bytes: &[u8]) -> Result<(Value, usize), CborError> {
    let mut decoder = Decoder::new(bytes);
    let value = decoder.decode_value(0)?;
    Ok((value, decoder.position))
}

struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, CborError> {
        let value = *self
            .bytes
            .get(self.position)
            .ok_or(CborError::UnexpectedEof)?;
        self.position += 1;
        Ok(value)
    }

    fn read_exact(&mut self, length: usize) -> Result<&'a [u8], CborError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(CborError::LengthOverflow)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(CborError::UnexpectedEof)?;
        self.position = end;
        Ok(value)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    /// additional information を読み、長さ / 値の引数を返す
    ///
    /// indefinite 長 (additional information 31) の場合は `None` を返す。
    fn read_argument(&mut self, additional: u8) -> Result<Option<u64>, CborError> {
        match additional {
            0..=23 => Ok(Some(u64::from(additional))),
            24 => Ok(Some(u64::from(self.read_u8()?))),
            25 => {
                let bytes = self.read_exact(2)?;
                Ok(Some(u64::from(u16::from_be_bytes([bytes[0], bytes[1]]))))
            }
            26 => {
                let bytes = self.read_exact(4)?;
                Ok(Some(u64::from(u32::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3],
                ]))))
            }
            27 => {
                let bytes = self.read_exact(8)?;
                Ok(Some(u64::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ])))
            }
            28..=30 => Err(CborError::InvalidAdditionalInformation(additional)),
            31 => Ok(None),
            _ => Err(CborError::InvalidAdditionalInformation(additional)),
        }
    }

    /// 引数を必須とするデータ項目 (整数 / タグ) の値を返す
    fn read_argument_value(&mut self, additional: u8) -> Result<u64, CborError> {
        self.read_argument(additional)?
            .ok_or(CborError::InvalidAdditionalInformation(additional))
    }

    fn decode_value(&mut self, depth: usize) -> Result<Value, CborError> {
        if depth > MAX_DEPTH {
            return Err(CborError::DepthLimitExceeded);
        }
        let initial = self.read_u8()?;
        let major = initial >> 5;
        let additional = initial & 0x1f;
        match major {
            0 => Ok(Value::Unsigned(self.read_argument_value(additional)?)),
            1 => Ok(Value::Negative(self.read_argument_value(additional)?)),
            2 => Ok(Value::ByteString(self.read_byte_string(additional, 2)?)),
            3 => {
                let bytes = self.read_byte_string(additional, 3)?;
                let text = core::str::from_utf8(&bytes).map_err(|_| CborError::InvalidUtf8)?;
                Ok(Value::TextString(String::from(text)))
            }
            4 => Ok(Value::Array(self.read_array(additional, depth)?)),
            5 => Ok(Value::Map(self.read_map(additional, depth)?)),
            6 => {
                let tag = self.read_argument_value(additional)?;
                let inner = self.decode_value(depth + 1)?;
                Ok(Value::Tag(tag, Box::new(inner)))
            }
            _ => match additional {
                0..=19 => Ok(Value::Simple(additional)),
                20 => Ok(Value::Bool(false)),
                21 => Ok(Value::Bool(true)),
                22 => Ok(Value::Null),
                23 => Ok(Value::Undefined),
                24 => {
                    // RFC 8949 §3.3: 24 〜 31 は予約されており、単純値として使えない
                    let value = self.read_u8()?;
                    if value < 32 {
                        Err(CborError::InvalidSimpleValue(value))
                    } else {
                        Ok(Value::Simple(value))
                    }
                }
                25 => {
                    let bytes = self.read_exact(2)?;
                    let bits = u16::from_be_bytes([bytes[0], bytes[1]]);
                    Ok(Value::Float(f16_to_f64(bits)))
                }
                26 => {
                    let bytes = self.read_exact(4)?;
                    let bits = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                    Ok(Value::Float(f64::from(f32::from_bits(bits))))
                }
                27 => {
                    let bytes = self.read_exact(8)?;
                    let bits = u64::from_be_bytes([
                        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                        bytes[7],
                    ]);
                    Ok(Value::Float(f64::from_bits(bits)))
                }
                28..=30 => Err(CborError::InvalidAdditionalInformation(additional)),
                31 => Err(CborError::BreakOutsideIndefinite),
                _ => Err(CborError::InvalidAdditionalInformation(additional)),
            },
        }
    }

    /// バイト文字列 / テキスト文字列を読む
    ///
    /// `expected_major` は本体の major type。indefinite 長のチャンクは同じ major type
    /// でなければならない (RFC 8949 §3.2.3)。
    fn read_byte_string(
        &mut self,
        additional: u8,
        expected_major: u8,
    ) -> Result<Vec<u8>, CborError> {
        if let Some(length) = self.read_argument(additional)? {
            let length = usize::try_from(length).map_err(|_| CborError::LengthOverflow)?;
            return Ok(self.read_exact(length)?.to_vec());
        }
        let mut out = Vec::new();
        loop {
            if self.peek() == Some(0xff) {
                self.position += 1;
                return Ok(out);
            }
            let initial = self.read_u8()?;
            let major = initial >> 5;
            let chunk_additional = initial & 0x1f;
            if major != expected_major {
                return Err(CborError::InvalidIndefiniteChunk);
            }
            let length = self.read_argument_value(chunk_additional)?;
            let length = usize::try_from(length).map_err(|_| CborError::LengthOverflow)?;
            out.extend_from_slice(self.read_exact(length)?);
        }
    }

    fn read_array(&mut self, additional: u8, depth: usize) -> Result<Vec<Value>, CborError> {
        let mut out = Vec::new();
        if let Some(length) = self.read_argument(additional)? {
            let length = usize::try_from(length).map_err(|_| CborError::LengthOverflow)?;
            // 入力サイズから大きく乖離した確保を避けるため、count による事前確保はしない
            for _ in 0..length {
                out.push(self.decode_value(depth + 1)?);
            }
            return Ok(out);
        }
        loop {
            if self.peek() == Some(0xff) {
                self.position += 1;
                return Ok(out);
            }
            out.push(self.decode_value(depth + 1)?);
        }
    }

    fn read_map(&mut self, additional: u8, depth: usize) -> Result<Vec<(Value, Value)>, CborError> {
        let mut out = Vec::new();
        let definite = self.read_argument(additional)?;
        if let Some(length) = definite {
            let length = usize::try_from(length).map_err(|_| CborError::LengthOverflow)?;
            for _ in 0..length {
                let key = self.decode_value(depth + 1)?;
                let value = self.decode_value(depth + 1)?;
                out.push((key, value));
            }
        } else {
            loop {
                if self.peek() == Some(0xff) {
                    self.position += 1;
                    break;
                }
                let key = self.decode_value(depth + 1)?;
                let value = self.decode_value(depth + 1)?;
                out.push((key, value));
            }
        }
        if has_duplicate_keys(&out) {
            return Err(CborError::DuplicateMapKey);
        }
        Ok(out)
    }
}

/// マップのキーが重複しているかどうかを判定する
///
/// キー数はトークンサイズに収まるため、ソートせず線形に比較する。NaN をキーにした
/// 浮動小数点数は自身と等しくならないため重複として検出できないが、RFC 8949 §5.6 の
/// 一意性要求はキーが有限個の通常の値であることを前提とする。
fn has_duplicate_keys(entries: &[(Value, Value)]) -> bool {
    for (index, (key, _)) in entries.iter().enumerate() {
        if entries[index + 1..].iter().any(|(other, _)| other == key) {
            return true;
        }
    }
    false
}

fn encode_value(value: &Value, depth: usize, out: &mut Vec<u8>) -> Result<(), CborError> {
    if depth > MAX_DEPTH {
        return Err(CborError::DepthLimitExceeded);
    }
    match value {
        Value::Unsigned(number) => write_head(0, *number, out),
        Value::Negative(number) => write_head(1, *number, out),
        Value::ByteString(bytes) => {
            write_head(2, bytes.len() as u64, out);
            out.extend_from_slice(bytes);
        }
        Value::TextString(text) => {
            write_head(3, text.len() as u64, out);
            out.extend_from_slice(text.as_bytes());
        }
        Value::Array(values) => {
            write_head(4, values.len() as u64, out);
            for item in values {
                encode_value(item, depth + 1, out)?;
            }
        }
        Value::Map(entries) => {
            // 決定論的エンコード: キーのエンコード済みバイト列の昇順に並べる
            let mut encoded_keys = Vec::new();
            for (key, _) in entries {
                let mut key_bytes = Vec::new();
                encode_value(key, depth + 1, &mut key_bytes)?;
                encoded_keys.push(key_bytes);
            }
            let mut order: Vec<usize> = (0..entries.len()).collect();
            order.sort_by(|left, right| encoded_keys[*left].cmp(&encoded_keys[*right]));
            for pair in order.windows(2) {
                if encoded_keys[pair[0]] == encoded_keys[pair[1]] {
                    return Err(CborError::DuplicateMapKey);
                }
            }
            write_head(5, entries.len() as u64, out);
            for index in order {
                out.extend_from_slice(&encoded_keys[index]);
                encode_value(&entries[index].1, depth + 1, out)?;
            }
        }
        Value::Tag(tag, inner) => {
            write_head(6, *tag, out);
            encode_value(inner, depth + 1, out)?;
        }
        Value::Bool(false) => out.push(0xf4),
        Value::Bool(true) => out.push(0xf5),
        Value::Null => out.push(0xf6),
        Value::Undefined => out.push(0xf7),
        Value::Float(number) => encode_float(*number, out),
        Value::Simple(simple) => match simple {
            0..=19 => out.push(0xe0 | simple),
            20..=31 => return Err(CborError::InvalidSimpleValue(*simple)),
            _ => {
                out.push(0xf8);
                out.push(*simple);
            }
        },
    }
    Ok(())
}

/// 数値を最小の長さでエンコードする
fn write_head(major: u8, value: u64, out: &mut Vec<u8>) {
    let prefix = major << 5;
    if value < 24 {
        out.push(prefix | value as u8);
    } else if value <= u64::from(u8::MAX) {
        out.push(prefix | 24);
        out.push(value as u8);
    } else if value <= u64::from(u16::MAX) {
        out.push(prefix | 25);
        out.extend_from_slice(&(value as u16).to_be_bytes());
    } else if value <= u64::from(u32::MAX) {
        out.push(prefix | 26);
        out.extend_from_slice(&(value as u32).to_be_bytes());
    } else {
        out.push(prefix | 27);
        out.extend_from_slice(&value.to_be_bytes());
    }
}

/// 浮動小数点数を値を保つ最短の幅でエンコードする (RFC 8949 §4.2.2)
fn encode_float(number: f64, out: &mut Vec<u8>) {
    if number.is_nan() {
        // NaN は符号とペイロードによらず正規の表現に揃える
        out.extend_from_slice(&[0xf9, 0x7e, 0x00]);
        return;
    }
    let half_bits = f64_to_f16_bits(number);
    if f16_to_f64(half_bits) == number {
        out.push(0xf9);
        out.extend_from_slice(&half_bits.to_be_bytes());
        return;
    }
    let single = number as f32;
    if f64::from(single) == number {
        out.push(0xfa);
        out.extend_from_slice(&single.to_bits().to_be_bytes());
        return;
    }
    out.push(0xfb);
    out.extend_from_slice(&number.to_bits().to_be_bytes());
}

/// 2^-24 〜 2^15 の厳密な値 (添字 `0` が 2^-24)
///
/// `core` には浮動小数点数の累乗関数が無いため、半精度 (binary16) の指数部を
/// 厳密に展開できる表を持つ。
const F16_POWERS: [f64; 40] = {
    let mut values = [1.0f64; 40];
    let mut index = 0;
    while index < 40 {
        let exponent = index as i32 - 24;
        let mut product = 1.0f64;
        let mut count = 0;
        // 2 の整数乗は乗除算の繰り返しで厳密に作れる
        while count < exponent.abs() {
            if exponent >= 0 {
                product *= 2.0;
            } else {
                product *= 0.5;
            }
            count += 1;
        }
        values[index] = product;
        index += 1;
    }
    values
};

/// IEEE 754 半精度 (binary16) のビット列を `f64` に変換する
fn f16_to_f64(bits: u16) -> f64 {
    let sign = (bits >> 15) & 1;
    let exponent = (bits >> 10) & 0x1f;
    let fraction = bits & 0x3ff;
    let magnitude = if exponent == 0 {
        // 非正規数 (および ±0)
        f64::from(fraction) * F16_POWERS[0]
    } else if exponent == 31 {
        if fraction == 0 {
            f64::INFINITY
        } else {
            f64::NAN
        }
    } else {
        // 正規数: (1 + fraction/1024) * 2^(exponent-15)
        (1024.0 + f64::from(fraction)) * F16_POWERS[(exponent as usize) + 9] / 1024.0
    };
    if sign == 1 { -magnitude } else { magnitude }
}

/// `f64` を IEEE 754 半精度 (binary16) のビット列に変換する
///
/// 丸めは最近接への近似で行う。呼び出し側は [`f16_to_f64`] との往復で値が厳密に
/// 保たれることを確認してから使う (決定論的エンコードは値を変える幅を使ってはならない)。
fn f64_to_f16_bits(number: f64) -> u16 {
    let bits = number.to_bits();
    let sign = ((bits >> 63) as u16) << 15;
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & 0x000f_ffff_ffff_ffff;
    if exponent == 0x7ff {
        // 無限大と NaN。NaN は呼び出し側で正規化するためペイロードは問わない
        return if fraction == 0 { sign | 0x7c00 } else { 0x7e00 };
    }
    let unbiased = exponent - 1023;
    if unbiased > 15 {
        // 半精度で表現できない大きさ。呼び出し側の往復確認で不一致になる
        return sign | 0x7c00;
    }
    if unbiased >= -14 {
        let mantissa = ((fraction >> 42) as u16) & 0x03ff;
        let exponent_bits = ((unbiased + 15) as u16) << 10;
        return sign | exponent_bits | mantissa;
    }
    if unbiased < -24 {
        // 半精度の最小非正規数より小さい。呼び出し側の往復確認で不一致になる
        return sign;
    }
    // 非正規数: 仮数部を 2^-24 単位に落とす
    let shift = (28 - unbiased) as u32;
    let mantissa = ((fraction | (1 << 52)) >> shift) as u16;
    sign | mantissa
}
