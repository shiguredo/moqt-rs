//! CBOR のデータモデルを表す [`Value`] 型を定義するモジュール

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

/// CBOR のデータ項目 (RFC 8949 Section 2 の基本ジェネリックデータモデル) を表す
///
/// major type ごとの表現を variant として持つ。indefinite-length 表現は
/// デコード時に definite-length 表現と同じ値に実体化されるため、この型では区別しない。
/// また浮動小数点値は精度を失わずに表現できる [`f64`] に統一する
/// (半精度・単精度はデコード時に正確に [`f64`] へ拡張される)。
///
/// # 等価性
///
/// [`PartialEq`] は variant と内容の構造的な等価性で判定する。
/// [`Value::Map`] はエントリの順序も比較対象になる。
/// [`Value::Float`] だけは数値としての等価性で判定し、`-0.0 == 0.0`、
/// `NaN == NaN` とする (RFC 8949 Section 5.6.1 のキー等価性に合わせている)。
#[derive(Debug, Clone)]
pub enum Value {
    /// major type 0: 符号なし整数 (0..=2^64-1)
    Unsigned(u64),
    /// major type 1: 負の整数。値は `-1 - n` で表される (-2^64..=-1)
    Negative(u64),
    /// major type 2: バイト文字列
    ByteString(Vec<u8>),
    /// major type 3: テキスト文字列 (UTF-8)
    TextString(String),
    /// major type 4: 配列
    Array(Vec<Value>),
    /// major type 5: マップ
    ///
    /// キーと値の組を出現順に保持する。重複キーもそのまま保持する
    /// (RFC 8949 Section 5.6 の「全てのエントリをアプリケーションに渡す」方式)。
    Map(Vec<(Value, Value)>),
    /// major type 6: タグ付きデータ項目 (タグ番号, タグ内容)
    ///
    /// タグ内容の妥当性 (RFC 8949 Section 5.3.2) は検証しない。
    Tag(u64, Box<Value>),
    /// major type 7: 割り当てられていない simple value (0..=19, 32..=255)
    ///
    /// 20..=23 は [`Value::Bool`], [`Value::Null`], [`Value::Undefined`] が
    /// 対応するため、この variant には使わないこと。24..=31 は予約されており、
    /// エンコード時の表現が存在しないためパニックする
    /// ([`Value::to_bytes`](crate::cbor::value::Value::to_bytes) を参照)。
    Simple(u8),
    /// major type 7: 真偽値 (simple value 20, 21)
    Bool(bool),
    /// major type 7: null (simple value 22)
    Null,
    /// major type 7: undefined (simple value 23)
    Undefined,
    /// major type 7: 浮動小数点値
    ///
    /// 半精度・単精度でエンコードされた値もデコード時にこの variant へ正確に拡張される。
    /// エンコード時は常に倍精度 (0xfb) で出力し、最短形式で出力したい場合は
    /// [`Value::to_canonical_bytes`](crate::cbor::value::Value::to_canonical_bytes) を使う。
    Float(f64),
}

impl Value {
    /// 符号なし整数の値を返す
    pub fn as_unsigned(&self) -> Option<u64> {
        match self {
            Value::Unsigned(value) => Some(*value),
            _ => None,
        }
    }

    /// 負の整数の引数 `n` を返す (値は `-1 - n`)
    pub fn as_negative(&self) -> Option<u64> {
        match self {
            Value::Negative(value) => Some(*value),
            _ => None,
        }
    }

    /// 整数値を [`i128`] として返す
    ///
    /// [`Value::Unsigned`] と [`Value::Negative`] の両方を受け付ける。
    /// `i128` は CBOR の整数の全範囲 (-2^64..=2^64-1) を表現できる。
    pub fn as_integer(&self) -> Option<i128> {
        match self {
            Value::Unsigned(value) => Some(i128::from(*value)),
            Value::Negative(value) => Some(-1 - i128::from(*value)),
            _ => None,
        }
    }

    /// 浮動小数点値を返す
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(value) => Some(*value),
            _ => None,
        }
    }

    /// バイト文字列を返す
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::ByteString(value) => Some(value),
            _ => None,
        }
    }

    /// テキスト文字列を返す
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::TextString(value) => Some(value),
            _ => None,
        }
    }

    /// 配列を返す
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(value) => Some(value),
            _ => None,
        }
    }

    /// マップを返す
    pub fn as_map(&self) -> Option<&[(Value, Value)]> {
        match self {
            Value::Map(value) => Some(value),
            _ => None,
        }
    }

    /// null かどうかを返す
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// undefined かどうかを返す
    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Undefined)
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Unsigned(a), Value::Unsigned(b)) => a == b,
            (Value::Negative(a), Value::Negative(b)) => a == b,
            (Value::ByteString(a), Value::ByteString(b)) => a == b,
            (Value::TextString(a), Value::TextString(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => a == b,
            (Value::Map(a), Value::Map(b)) => a == b,
            (Value::Tag(a_tag, a_content), Value::Tag(b_tag, b_content)) => {
                a_tag == b_tag && a_content == b_content
            }
            (Value::Simple(a), Value::Simple(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Null, Value::Null) => true,
            (Value::Undefined, Value::Undefined) => true,
            // RFC 8949 Section 5.6.1 のキー等価性に合わせ、-0.0 と 0.0 は等しく、
            // NaN 同士は等しいものとして扱う
            (Value::Float(a), Value::Float(b)) => (*a == *b) || (a.is_nan() && b.is_nan()),
            _ => false,
        }
    }
}

impl Eq for Value {}
