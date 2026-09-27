//! CBOR (RFC 8949) のコーデックのテスト

use shiguredo_moqt::c4m::cbor::{
    CborError, MAX_DEPTH, Value, decode, decode_partial, encode, encode_into,
};

use super::helpers::{decode_hex, encode_hex};
use super::vectors::CLAIM_VECTORS;

#[test]
fn claim_vectors_decode_and_encode_back_to_same_bytes() {
    for vector in CLAIM_VECTORS {
        let bytes = decode_hex(vector.payload_hex);
        let value = decode(&bytes)
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        let encoded = encode(&value)
            .unwrap_or_else(|error| panic!("ベクタ {} のエンコードに失敗した: {error}", vector.id));
        assert_eq!(
            encode_hex(&encoded),
            vector.payload_hex,
            "ベクタ {} の再エンコードが一致しない",
            vector.id
        );
    }
}

#[test]
fn issuer_only_vector_has_expected_structure() {
    let bytes = decode_hex(CLAIM_VECTORS[0].payload_hex);
    let value = decode(&bytes).expect("デコードできる");
    assert_eq!(
        value,
        Value::Map(vec![(
            Value::Unsigned(1),
            Value::TextString(String::from("https://auth.example.com"))
        )])
    );
}

#[test]
fn decode_partial_returns_consumed_length() {
    let mut bytes = decode_hex(CLAIM_VECTORS[0].payload_hex);
    bytes.push(0xff);
    let (value, consumed) =
        decode_partial(&bytes).expect("余分なバイトがあっても先頭のデータ項目を読める");
    assert!(matches!(value, Value::Map(_)));
    assert_eq!(consumed, bytes.len() - 1);
    // 全体をデコードする API は余分なバイトを拒否する
    assert_eq!(decode(&bytes), Err(CborError::TrailingBytes));
}

#[test]
fn unsigned_integers_use_minimum_length() {
    assert_eq!(
        encode(&Value::Unsigned(0)).expect("エンコードできる"),
        [0x00]
    );
    assert_eq!(
        encode(&Value::Unsigned(23)).expect("エンコードできる"),
        [0x17]
    );
    assert_eq!(
        encode(&Value::Unsigned(24)).expect("エンコードできる"),
        [0x18, 0x18]
    );
    assert_eq!(
        encode(&Value::Unsigned(255)).expect("エンコードできる"),
        [0x18, 0xff]
    );
    assert_eq!(
        encode(&Value::Unsigned(256)).expect("エンコードできる"),
        [0x19, 0x01, 0x00]
    );
    assert_eq!(
        encode(&Value::Unsigned(65536)).expect("エンコードできる"),
        [0x1a, 0x00, 0x01, 0x00, 0x00]
    );
    assert_eq!(
        encode(&Value::Unsigned(u64::MAX)).expect("エンコードできる"),
        [0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
    );
}

#[test]
fn negative_integers_use_minimum_length() {
    assert_eq!(
        encode(&Value::integer(-1)).expect("エンコードできる"),
        [0x20]
    );
    assert_eq!(
        encode(&Value::integer(-24)).expect("エンコードできる"),
        [0x37]
    );
    assert_eq!(
        encode(&Value::integer(-25)).expect("エンコードできる"),
        [0x38, 0x18]
    );
    assert_eq!(
        encode(&Value::integer(i64::MIN)).expect("エンコードできる"),
        [0x3b, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
    );
    assert_eq!(Value::integer(-1), Value::Negative(0));
    assert_eq!(Value::integer(-1).as_int(), Some(-1));
}

#[test]
fn floats_use_preferred_shortest_encoding() {
    // 付録 A.2 / A.5 のベクタに出てくる値
    assert_eq!(
        encode(&Value::Float(100.0)).expect("エンコードできる"),
        decode_hex("f95640")
    );
    assert_eq!(
        encode(&Value::Float(300.0)).expect("エンコードできる"),
        decode_hex("f95cb0")
    );
    assert_eq!(
        encode(&Value::Float(37.7749)).expect("エンコードできる"),
        decode_hex("fb4042e32fec56d5d0")
    );
    assert_eq!(
        encode(&Value::Float(-122.4194)).expect("エンコードできる"),
        decode_hex("fbc05e9ad77318fc50")
    );
    // 半精度の境界と特殊値
    assert_eq!(
        encode(&Value::Float(65504.0)).expect("エンコードできる"),
        decode_hex("f97bff")
    );
    assert_eq!(
        encode(&Value::Float(1.5)).expect("エンコードできる"),
        decode_hex("f93e00")
    );
    assert_eq!(
        encode(&Value::Float(-0.0)).expect("エンコードできる"),
        decode_hex("f98000")
    );
    assert_eq!(
        encode(&Value::Float(f64::INFINITY)).expect("エンコードできる"),
        decode_hex("f97c00")
    );
    assert_eq!(
        encode(&Value::Float(f64::NEG_INFINITY)).expect("エンコードできる"),
        decode_hex("f9fc00")
    );
    assert_eq!(
        encode(&Value::Float(f64::NAN)).expect("エンコードできる"),
        decode_hex("f97e00")
    );
    // 半精度で表現できない値は単精度 / 倍精度になる
    assert_eq!(
        encode(&Value::Float(100000.0)).expect("エンコードできる"),
        decode_hex("fa47c35000")
    );
    assert_eq!(
        encode(&Value::Float(0.1)).expect("エンコードできる"),
        decode_hex("fb3fb999999999999a")
    );
}

#[test]
fn floats_decode_from_every_width() {
    // 付録 A.2 の catgeocoord の accuracy は f9 5640 (100.0) で書かれている
    assert_eq!(decode(&decode_hex("f95640")), Ok(Value::Float(100.0)));
    // 半精度の別値 (0x5664 は 102.25) も正しく読める
    assert_eq!(decode(&decode_hex("f95664")), Ok(Value::Float(102.25)));
    assert_eq!(
        decode(&decode_hex("fa47c35000")),
        Ok(Value::Float(100000.0))
    );
    assert_eq!(
        decode(&decode_hex("fb3fb999999999999a")),
        Ok(Value::Float(0.1))
    );
    assert_eq!(
        decode(&decode_hex("f97c00")),
        Ok(Value::Float(f64::INFINITY))
    );
    assert_eq!(
        decode(&decode_hex("f9fc00")),
        Ok(Value::Float(f64::NEG_INFINITY))
    );
    assert!(matches!(
        decode(&decode_hex("f97e00")),
        Ok(Value::Float(value)) if value.is_nan()
    ));
    // -0.0 は符号を保つ
    assert!(matches!(
        decode(&decode_hex("f98000")),
        Ok(Value::Float(value)) if value == 0.0 && value.is_sign_negative()
    ));
}

#[test]
fn byte_and_text_strings_round_trip() {
    let value = Value::Map(vec![
        (Value::Unsigned(1), Value::ByteString(vec![0x00, 0xff])),
        (
            Value::Unsigned(2),
            Value::TextString(String::from("こんにちは")),
        ),
    ]);
    let encoded = encode(&value).expect("エンコードできる");
    assert_eq!(decode(&encoded), Ok(value));
}

#[test]
fn indefinite_length_items_decode() {
    // indefinite 長のバイト文字列
    let value = decode(&decode_hex("5f42010243030405ff")).expect("デコードできる");
    assert_eq!(value, Value::ByteString(vec![1, 2, 3, 4, 5]));
    // indefinite 長のテキスト文字列
    let value = decode(&decode_hex("7f616161626163ff")).expect("デコードできる");
    assert_eq!(value, Value::TextString(String::from("abc")));
    // indefinite 長の配列
    let value = decode(&decode_hex("9f0102ff")).expect("デコードできる");
    assert_eq!(
        value,
        Value::Array(vec![Value::Unsigned(1), Value::Unsigned(2)])
    );
    // indefinite 長のマップ
    let value = decode(&decode_hex("bf01020304ff")).expect("デコードできる");
    assert_eq!(
        value,
        Value::Map(vec![
            (Value::Unsigned(1), Value::Unsigned(2)),
            (Value::Unsigned(3), Value::Unsigned(4)),
        ])
    );
}

#[test]
fn indefinite_length_errors() {
    // break が単独で現れる
    assert_eq!(decode(&[0xff]), Err(CborError::BreakOutsideIndefinite));
    // indefinite チャンクの型が違う
    assert_eq!(
        decode(&decode_hex("5f6101ff")),
        Err(CborError::InvalidIndefiniteChunk)
    );
    // indefinite チャンクに indefinite は書けない
    assert_eq!(
        decode(&decode_hex("5f5f4101ffff")),
        Err(CborError::InvalidAdditionalInformation(31))
    );
}

#[test]
fn map_keys_are_sorted_by_encoded_bytes() {
    let value = Value::Map(vec![
        (Value::TextString(String::from("aa")), Value::Unsigned(3)),
        (Value::Unsigned(10), Value::Unsigned(2)),
        (Value::Unsigned(2), Value::Unsigned(1)),
    ]);
    assert_eq!(
        encode_hex(&encode(&value).expect("エンコードできる")),
        "a302010a0262616103"
    );
}

#[test]
fn duplicate_map_keys_are_rejected_on_decode_and_encode() {
    assert_eq!(
        decode(&decode_hex("a201010102")),
        Err(CborError::DuplicateMapKey)
    );
    let value = Value::Map(vec![
        (Value::Unsigned(1), Value::Unsigned(1)),
        (Value::Unsigned(1), Value::Unsigned(2)),
    ]);
    assert_eq!(encode(&value), Err(CborError::DuplicateMapKey));
}

#[test]
fn invalid_inputs_are_rejected() {
    assert_eq!(decode(&[]), Err(CborError::UnexpectedEof));
    assert_eq!(decode(&[0x18]), Err(CborError::UnexpectedEof));
    assert_eq!(
        decode(&[0x1c]),
        Err(CborError::InvalidAdditionalInformation(28))
    );
    assert_eq!(
        decode(&[0xf8, 0x1f]),
        Err(CborError::InvalidSimpleValue(31))
    );
    assert_eq!(decode(&decode_hex("61ff")), Err(CborError::InvalidUtf8));
}

#[test]
fn simple_and_special_values_round_trip() {
    assert_eq!(decode(&[0xf4]), Ok(Value::Bool(false)));
    assert_eq!(decode(&[0xf5]), Ok(Value::Bool(true)));
    assert_eq!(decode(&[0xf6]), Ok(Value::Null));
    assert_eq!(decode(&[0xf7]), Ok(Value::Undefined));
    assert_eq!(decode(&[0xf8, 0x20]), Ok(Value::Simple(32)));
    assert_eq!(
        encode(&Value::Bool(true)).expect("エンコードできる"),
        [0xf5]
    );
    assert_eq!(encode(&Value::Null).expect("エンコードできる"), [0xf6]);
    assert_eq!(encode(&Value::Undefined).expect("エンコードできる"), [0xf7]);
    assert_eq!(
        encode(&Value::Simple(32)).expect("エンコードできる"),
        [0xf8, 0x20]
    );
    assert_eq!(encode(&Value::Simple(5)).expect("エンコードできる"), [0xe5]);
    assert_eq!(
        encode(&Value::Simple(25)),
        Err(CborError::InvalidSimpleValue(25))
    );
}

#[test]
fn tags_are_preserved() {
    let value = decode(&decode_hex("d83dd8184101")).expect("デコードできる");
    assert_eq!(
        value,
        Value::Tag(
            61,
            Box::new(Value::Tag(24, Box::new(Value::ByteString(vec![1]))))
        )
    );
    let encoded = encode(&value).expect("エンコードできる");
    assert_eq!(encode_hex(&encoded), "d83dd8184101");
}

#[test]
fn nesting_depth_is_limited_on_decode_and_encode() {
    // 上限ちょうどは通る
    let mut bytes = vec![0x81; MAX_DEPTH]; // 1 要素の配列
    bytes.push(0x01);
    assert_eq!(
        decode(&bytes),
        Ok({
            let mut value = Value::Unsigned(1);
            for _ in 0..MAX_DEPTH {
                value = Value::Array(vec![value]);
            }
            value
        })
    );
    // 上限を超えるとエラー
    let mut bytes = vec![0x81; MAX_DEPTH + 1];
    bytes.push(0x01);
    assert_eq!(decode(&bytes), Err(CborError::DepthLimitExceeded));

    let mut value = Value::Unsigned(1);
    for _ in 0..MAX_DEPTH + 1 {
        value = Value::Array(vec![value]);
    }
    assert_eq!(encode(&value), Err(CborError::DepthLimitExceeded));
}

#[test]
fn accessors_read_values() {
    let map = Value::Map(vec![
        (Value::Unsigned(1), Value::TextString(String::from("a"))),
        (Value::integer(-1), Value::Float(1.5)),
        (Value::Unsigned(2), Value::ByteString(vec![7])),
        (Value::Unsigned(3), Value::Bool(true)),
    ]);
    assert_eq!(
        map.map_get(&Value::Unsigned(1)).and_then(Value::as_text),
        Some("a")
    );
    assert_eq!(
        map.map_get(&Value::integer(-1)).and_then(Value::as_number),
        Some(1.5)
    );
    assert_eq!(
        map.map_get(&Value::Unsigned(2)).and_then(Value::as_bytes),
        Some(&[7u8][..])
    );
    assert_eq!(
        map.map_get(&Value::Unsigned(3)).and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(Value::Unsigned(1).as_unsigned(), Some(1));
    assert_eq!(Value::Negative(0).as_unsigned(), None);
    assert!(matches!(Value::Array(vec![]).as_array(), Some(values) if values.is_empty()));
    assert_eq!(Value::Unsigned(1).as_map(), None);
}

#[test]
fn encode_into_appends() {
    let mut buffer = vec![0x00];
    encode_into(&Value::Unsigned(1), &mut buffer).expect("エンコードできる");
    assert_eq!(buffer, [0x00, 0x01]);
}

#[test]
fn negative_numbers_convert_with_single_rounding() {
    // 2^53 を超える負の整数も 1 回だけ丸めて f64 へ変換する
    let value = Value::integer(-9412508413557840);
    assert_eq!(value.as_number(), Some(-9412508413557840.0));
    assert_eq!(
        Value::Negative(9412508413557839).as_number(),
        Some(-9412508413557840.0)
    );
    // u64 の最大値は丸めた結果を返す (i128 経由で 1 回だけ丸める)
    assert_eq!(
        Value::Negative(u64::MAX).as_number(),
        Some((-(u64::MAX as i128) - 1) as f64)
    );
}

#[test]
fn indefinite_text_chunks_must_each_be_valid_utf8() {
    // チャンクを連結すると "€" (e2 82 ac) になるが、RFC 8949 §3.2.3 は
    // 各チャンクが個別に正しい UTF-8 であることを要求する
    assert_eq!(
        decode(&decode_hex("7f62e28261acff")),
        Err(CborError::InvalidUtf8)
    );
    // 1 チャンクに収まっていれば通る
    assert_eq!(
        decode(&decode_hex("7f63e282acff")),
        Ok(Value::TextString(String::from("€")))
    );
}

#[test]
fn duplicate_nan_keys_are_rejected() {
    // RFC 8949 §5.6.1 は同じ内容の NaN のキーを同一視する
    let value = Value::Map(vec![
        (Value::Float(f64::NAN), Value::Unsigned(1)),
        (Value::Float(f64::NAN), Value::Unsigned(2)),
    ]);
    assert_eq!(encode(&value), Err(CborError::DuplicateMapKey));
    assert_eq!(
        decode(&decode_hex("a2f97e0001f97e0002")),
        Err(CborError::DuplicateMapKey)
    );
}

#[test]
fn large_maps_are_checked_for_duplicates() {
    // 重複検査が二次関数的な時間を使わないことを、大きなマップで確認する
    let mut entries = Vec::new();
    for index in 0..2000u64 {
        entries.push((Value::Unsigned(index), Value::Unsigned(index)));
    }
    entries.push((Value::Unsigned(1000), Value::Unsigned(1000)));
    assert_eq!(
        encode(&Value::Map(entries)),
        Err(CborError::DuplicateMapKey)
    );
}

#[test]
fn zero_and_negative_zero_keys_are_equivalent() {
    // RFC 8949 §5.6.1: -0.0 と 0.0 は数値として等しいため同一のキー
    let value = Value::Map(vec![
        (Value::Float(0.0), Value::Unsigned(1)),
        (Value::Float(-0.0), Value::Unsigned(2)),
    ]);
    assert_eq!(encode(&value), Err(CborError::DuplicateMapKey));
    assert_eq!(
        decode(&decode_hex("a2f9000001f9800002")),
        Err(CborError::DuplicateMapKey)
    );
    // 片方だけなら通る
    assert!(encode(&Value::Map(vec![(Value::Float(-0.0), Value::Unsigned(1))])).is_ok());
}

#[test]
fn nested_keys_are_compared_after_normalization() {
    // 入れ子のキーでも -0.0 と 0.0 は同一視する
    let value = Value::Map(vec![
        (Value::Array(vec![Value::Float(0.0)]), Value::Unsigned(1)),
        (Value::Array(vec![Value::Float(-0.0)]), Value::Unsigned(2)),
    ]);
    assert_eq!(encode(&value), Err(CborError::DuplicateMapKey));

    // 深すぎるキーは stack を壊さず DepthLimitExceeded になる
    let mut key = Value::Unsigned(1);
    for _ in 0..MAX_DEPTH + 2 {
        key = Value::Array(vec![key]);
    }
    let value = Value::Map(vec![(key, Value::Unsigned(1))]);
    assert_eq!(encode(&value), Err(CborError::DepthLimitExceeded));
}
