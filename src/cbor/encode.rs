//! CBOR のエンコードを行うモジュール

use crate::cbor::half::f64_to_half_exact;
use crate::cbor::value::Value;
use alloc::vec::Vec;

/// major type と引数からヘッド (RFC 8949 Section 3) を出力する
///
/// 引数は常に最短形式 (RFC 8949 Section 4.1 の preferred serialization) で出力する。
fn encode_head(out: &mut Vec<u8>, major: u8, argument: u64) {
    if argument < 24 {
        out.push((major << 5) | argument as u8);
    } else if argument <= u64::from(u8::MAX) {
        out.push((major << 5) | 24);
        out.push(argument as u8);
    } else if argument <= u64::from(u16::MAX) {
        out.push((major << 5) | 25);
        out.extend_from_slice(&(argument as u16).to_be_bytes());
    } else if argument <= u64::from(u32::MAX) {
        out.push((major << 5) | 26);
        out.extend_from_slice(&(argument as u32).to_be_bytes());
    } else {
        out.push((major << 5) | 27);
        out.extend_from_slice(&argument.to_be_bytes());
    }
}

impl Value {
    /// CBOR にエンコードしてバイト列を返す
    ///
    /// 引数 (整数・長さ・タグ番号) は最短形式で出力する。浮動小数点値は常に
    /// 倍精度 (0xfb) で出力し、indefinite-length 表現は使わずに definite-length で出力する。
    /// 最短形式の浮動小数点や決定論的エンコードが必要な場合は
    /// [`Value::to_canonical_bytes`] を使う。
    ///
    /// # Panics
    ///
    /// [`Value::Simple`] に 24..=31 を指定した場合はパニックする。
    /// この範囲は予約されており、well-formed な CBOR 表現が存在しない。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    /// CBOR にエンコードして `out` の末尾に追記する
    ///
    /// 出力内容は [`Value::to_bytes`] と同じである。
    ///
    /// # Panics
    ///
    /// [`Value::Simple`] に 24..=31 を指定した場合はパニックする。
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        encode_value(self, out, false);
    }

    /// RFC 8949 Section 4.2.1 の core deterministic encoding でエンコードしてバイト列を返す
    ///
    /// 通常のエンコードとの違いは次のとおり。
    ///
    /// - 浮動小数点値を、値を保つ最短の表現 (半精度 → 単精度 → 倍精度) で出力する
    /// - NaN は常に 0xf97e00 で出力する
    /// - マップのキーを、決定論的エンコードのバイト列の辞書順でソートして出力する
    ///
    /// 引数の最短形式と definite-length の使用は [`Value::to_bytes`] と同じである。
    ///
    /// # Panics
    ///
    /// [`Value::Simple`] に 24..=31 を指定した場合はパニックする。
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_canonical_into(&mut out);
        out
    }

    /// core deterministic encoding でエンコードして `out` の末尾に追記する
    ///
    /// 出力内容は [`Value::to_canonical_bytes`] と同じである。
    ///
    /// # Panics
    ///
    /// [`Value::Simple`] に 24..=31 を指定した場合はパニックする。
    pub fn encode_canonical_into(&self, out: &mut Vec<u8>) {
        encode_value(self, out, true);
    }
}

/// `value` を `out` にエンコードする
///
/// `canonical` が `true` の場合、決定論的エンコードの規則を適用する。
fn encode_value(value: &Value, out: &mut Vec<u8>, canonical: bool) {
    match value {
        Value::Unsigned(value) => encode_head(out, 0, *value),
        Value::Negative(value) => encode_head(out, 1, *value),
        Value::ByteString(value) => {
            encode_head(out, 2, value.len() as u64);
            out.extend_from_slice(value);
        }
        Value::TextString(value) => {
            encode_head(out, 3, value.len() as u64);
            out.extend_from_slice(value.as_bytes());
        }
        Value::Array(items) => {
            encode_head(out, 4, items.len() as u64);
            for item in items {
                encode_value(item, out, canonical);
            }
        }
        Value::Map(pairs) => {
            if canonical {
                encode_canonical_map(pairs, out);
            } else {
                encode_head(out, 5, pairs.len() as u64);
                for (key, value) in pairs {
                    encode_value(key, out, canonical);
                    encode_value(value, out, canonical);
                }
            }
        }
        Value::Tag(tag, content) => {
            encode_head(out, 6, *tag);
            encode_value(content, out, canonical);
        }
        Value::Simple(value) => encode_simple(out, *value),
        Value::Bool(false) => out.push(0xf4),
        Value::Bool(true) => out.push(0xf5),
        Value::Null => out.push(0xf6),
        Value::Undefined => out.push(0xf7),
        Value::Float(value) => encode_float(out, *value, canonical),
    }
}

/// 決定論的エンコードでマップを出力する
///
/// RFC 8949 Section 4.2.1 のとおり、キーを決定論的エンコードのバイト列の
/// 辞書順 (bytewise lexicographic order) でソートする。
fn encode_canonical_map(pairs: &[(Value, Value)], out: &mut Vec<u8>) {
    // キーのエンコード結果はそのまま出力にも使うため、ソート時に一緒に保持する
    let mut entries: Vec<(Vec<u8>, &Value)> = Vec::new();
    for (key, value) in pairs {
        let mut key_bytes = Vec::new();
        encode_value(key, &mut key_bytes, true);
        entries.push((key_bytes, value));
    }
    // 等しいキー (invalid なデータ) の順序は出現順のままにするため stable sort を使う
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    encode_head(out, 5, entries.len() as u64);
    for (key_bytes, value) in entries {
        out.extend_from_slice(&key_bytes);
        encode_value(value, out, true);
    }
}

/// 浮動小数点値を出力する
///
/// 通常は倍精度 (0xfb) で出力し、決定論的エンコードの場合は
/// RFC 8949 Section 4.2.1 のとおり値を保つ最短の表現を使う。
fn encode_float(out: &mut Vec<u8>, value: f64, canonical: bool) {
    if !canonical {
        out.push(0xfb);
        out.extend_from_slice(&value.to_bits().to_be_bytes());
        return;
    }

    if value.is_nan() {
        // RFC 8949 Section 4.2.2 の NaN の扱いに合わせ、決定論的エンコードでは
        // 単一の表現 (半精度の quiet NaN) に固定する
        out.push(0xf9);
        out.extend_from_slice(&0x7e00u16.to_be_bytes());
    } else if let Some(bits) = f64_to_half_exact(value) {
        out.push(0xf9);
        out.extend_from_slice(&bits.to_be_bytes());
    } else {
        let single = value as f32;
        // 単精度に丸めてから倍精度に戻して値が変わらない場合だけ単精度を使う
        if f64::from(single).to_bits() == value.to_bits() {
            out.push(0xfa);
            out.extend_from_slice(&single.to_bits().to_be_bytes());
        } else {
            out.push(0xfb);
            out.extend_from_slice(&value.to_bits().to_be_bytes());
        }
    }
}

/// simple value を出力する
///
/// 0..=23 は 1 バイト表現、32..=255 は 2 バイト表現 (0xf8 の後に値) を使う。
fn encode_simple(out: &mut Vec<u8>, value: u8) {
    if value <= 23 {
        out.push(0xe0 | value);
    } else if value >= 32 {
        out.push(0xf8);
        out.push(value);
    } else {
        // 24..=31 は予約されており、well-formed な表現が存在しない
        panic!("simple values 24..=31 are reserved and cannot be encoded");
    }
}
