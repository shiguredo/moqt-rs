//! RFC 8949 のエンコード動作を検証するテスト

use crate::{bytes_to_hex, hex_to_bytes};

use shiguredo_moqt::cbor::decode::decode;
use shiguredo_moqt::cbor::value::Value;

/// Value とそのエンコード結果 (16 進数) の組を検証する
fn assert_encodings(cases: &[(Value, &str)]) {
    for (value, expected) in cases {
        assert_eq!(
            bytes_to_hex(&value.to_bytes()),
            *expected,
            "{value:?} のエンコード結果が違う"
        );
    }
}

/// Value のエンコード結果の先頭 (ヘッド) が期待どおりかを検証する
fn assert_heads(cases: &[(Value, &str)]) {
    for (value, expected) in cases {
        let bytes = value.to_bytes();
        let head_len = expected.len() / 2;
        assert!(
            bytes.len() >= head_len,
            "{value:?} のエンコード結果が短すぎる: {}",
            bytes_to_hex(&bytes)
        );
        assert_eq!(
            bytes_to_hex(&bytes[..head_len]),
            *expected,
            "{value:?} のヘッドが違う"
        );
    }
}

#[test]
fn encodes_integers_in_shortest_form() {
    // RFC 8949 Section 4.1 のとおり、引数は最短形式で出力する
    assert_encodings(&[
        (Value::Unsigned(0), "00"),
        (Value::Unsigned(23), "17"),
        (Value::Unsigned(24), "1818"),
        (Value::Unsigned(255), "18ff"),
        (Value::Unsigned(256), "190100"),
        (Value::Unsigned(65535), "19ffff"),
        (Value::Unsigned(65536), "1a00010000"),
        (Value::Unsigned(4294967295), "1affffffff"),
        (Value::Unsigned(4294967296), "1b0000000100000000"),
        (Value::Unsigned(u64::MAX), "1bffffffffffffffff"),
        (Value::Negative(0), "20"),
        (Value::Negative(23), "37"),
        (Value::Negative(24), "3818"),
        (Value::Negative(255), "38ff"),
        (Value::Negative(256), "390100"),
        (Value::Negative(65535), "39ffff"),
        (Value::Negative(65536), "3a00010000"),
        (Value::Negative(u64::MAX), "3bffffffffffffffff"),
    ]);
}

#[test]
fn encodes_lengths_in_shortest_form() {
    assert_heads(&[
        (Value::ByteString(vec![0; 0]), "40"),
        (Value::ByteString(vec![0; 23]), "57"),
        (Value::ByteString(vec![0; 24]), "5818"),
        (Value::ByteString(vec![0; 255]), "58ff"),
        (Value::ByteString(vec![0; 256]), "590100"),
        (Value::TextString(String::new()), "60"),
        (Value::TextString(String::from("a")), "6161"),
        // テキスト文字列の長さはバイト数で数える (水 は UTF-8 で 3 バイト)
        (Value::TextString(String::from("水")), "63e6b0b4"),
        (Value::Array(Vec::new()), "80"),
        (Value::Array(vec![Value::Unsigned(1); 24]), "9818"),
        (Value::Map(Vec::new()), "a0"),
        (
            Value::Map(vec![(Value::Unsigned(1), Value::Unsigned(2)); 24]),
            "b818",
        ),
    ]);

    // ヘッドと内容の長さが一致していること
    assert_eq!(Value::ByteString(vec![0; 23]).to_bytes().len(), 1 + 23);
    assert_eq!(Value::ByteString(vec![0; 256]).to_bytes().len(), 3 + 256);
    assert_eq!(
        Value::TextString(String::from("水")).to_bytes().len(),
        1 + 3
    );
    assert_eq!(
        Value::Array(vec![Value::Unsigned(1); 24]).to_bytes().len(),
        2 + 24
    );
}

#[test]
fn encodes_tags_in_shortest_form() {
    assert_encodings(&[
        (Value::Tag(0, Box::new(Value::Unsigned(0))), "c000"),
        (Value::Tag(23, Box::new(Value::Unsigned(0))), "d700"),
        (Value::Tag(24, Box::new(Value::Unsigned(0))), "d81800"),
        (Value::Tag(255, Box::new(Value::Unsigned(0))), "d8ff00"),
        (Value::Tag(256, Box::new(Value::Unsigned(0))), "d9010000"),
        (
            Value::Tag(65536, Box::new(Value::Unsigned(0))),
            "da0001000000",
        ),
        (
            Value::Tag(4294967296, Box::new(Value::Unsigned(0))),
            "db000000010000000000",
        ),
    ]);
}

#[test]
fn encodes_simple_values_and_floats() {
    assert_encodings(&[
        (Value::Simple(0), "e0"),
        (Value::Simple(19), "f3"),
        (Value::Simple(32), "f820"),
        (Value::Simple(255), "f8ff"),
        (Value::Bool(false), "f4"),
        (Value::Bool(true), "f5"),
        (Value::Null, "f6"),
        (Value::Undefined, "f7"),
        // 通常のエンコードでは浮動小数点値は常に倍精度になる
        (Value::Float(0.0), "fb0000000000000000"),
        (Value::Float(-0.0), "fb8000000000000000"),
        (Value::Float(1.5), "fb3ff8000000000000"),
        (Value::Float(1.1), "fb3ff199999999999a"),
        (Value::Float(f64::INFINITY), "fb7ff0000000000000"),
    ]);
}

#[test]
fn encodes_non_preferred_examples_back_to_preferred_encoding() {
    // 非最短形式の入力も、エンコードし直すと最短形式になる
    for (input, expected) in [
        ("1800", "00"),
        ("1817", "17"),
        ("190018", "1818"),
        ("3800", "20"),
        ("580141", "4141"),
        ("780161", "6161"),
        ("980101", "8101"),
        ("b8010101", "a10101"),
        ("d80000", "c000"),
        ("5f41414142ff", "424142"),
        ("7f61616162ff", "626162"),
        ("9f01ff", "8101"),
        ("bf0102ff", "a10102"),
    ] {
        let value = decode(&hex_to_bytes(input)).unwrap_or_else(|error| {
            panic!("{input} のデコードに失敗した: {error}");
        });
        assert_eq!(
            bytes_to_hex(&value.to_bytes()),
            expected,
            "{input} のエンコード結果が違う"
        );
    }
}

#[test]
fn encodes_into_existing_buffer() {
    let mut out = vec![0xff];
    Value::Unsigned(1).encode_into(&mut out);
    assert_eq!(out, vec![0xff, 0x01]);

    let mut out = vec![0xff];
    Value::Unsigned(1).encode_canonical_into(&mut out);
    assert_eq!(out, vec![0xff, 0x01]);
}

#[test]
fn canonical_encoding_sorts_map_keys() {
    // RFC 8949 Section 4.2.1 のキーのソート例を、値が識別できるように
    // 入力順の番号を値として付けて検証する
    let keys = [
        Value::Unsigned(100),
        Value::TextString(String::from("aa")),
        Value::Bool(false),
        Value::Unsigned(10),
        Value::Array(vec![Value::Negative(0)]),
        Value::Negative(0),
        Value::Array(vec![Value::Unsigned(100)]),
        Value::TextString(String::from("z")),
    ];
    let pairs: Vec<(Value, Value)> = keys
        .into_iter()
        .enumerate()
        .map(|(index, key)| (key, Value::Unsigned(index as u64 + 1)))
        .collect();
    let value = Value::Map(pairs);
    assert_eq!(
        bytes_to_hex(&value.to_canonical_bytes()),
        "a80a041864012006617a086261610281186407812005f403"
    );

    // 入力順が違っても決定論的エンコードは同じバイト列になる
    let reversed = Value::Map(
        value
            .as_map()
            .expect("マップのはず")
            .iter()
            .rev()
            .cloned()
            .collect(),
    );
    assert_eq!(
        reversed.to_canonical_bytes(),
        value.to_canonical_bytes(),
        "入力順によって決定論的エンコード結果が変わった"
    );
}

#[test]
fn canonical_encoding_uses_shortest_float_form() {
    for (value, expected) in [
        (0.0, "f90000"),
        (-0.0, "f98000"),
        (1.0, "f93c00"),
        (1.5, "f93e00"),
        // RFC 8949 Section 4.1 の例
        (5.5, "f94580"),
        (5555.5, "fa45ad9c00"),
        (65504.0, "f97bff"),
        // RFC 8949 Section 4.2.1 の例
        (1000000.5, "fa49742408"),
        (1.0e300, "fb7e37e43c8800759c"),
        (f64::INFINITY, "f97c00"),
        (f64::NEG_INFINITY, "f9fc00"),
        // RFC 8949 Section 4.2.2 のとおり NaN は単一の表現に固定する
        (f64::NAN, "f97e00"),
    ] {
        assert_eq!(
            bytes_to_hex(&Value::Float(value).to_canonical_bytes()),
            expected,
            "{value} の決定論的エンコード結果が違う"
        );
    }
}

#[test]
#[should_panic(expected = "simple values 24..=31 are reserved and cannot be encoded")]
fn encoding_reserved_simple_value_panics() {
    let _ = Value::Simple(24).to_bytes();
}

#[test]
#[should_panic(expected = "simple values 24..=31 are reserved and cannot be encoded")]
fn canonical_encoding_reserved_simple_value_panics() {
    let _ = Value::Simple(31).to_canonical_bytes();
}
