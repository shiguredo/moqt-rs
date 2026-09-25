//! Value 型の等価性とアクセサを検証するテスト

use shiguredo_moqt::cbor::value::Value;

#[test]
fn compares_floats_like_cbor_key_equivalence() {
    // RFC 8949 Section 5.6.1 に合わせ、-0.0 と 0.0 は等しい
    assert_eq!(Value::Float(-0.0), Value::Float(0.0));
    // NaN 同士は等しい (反射律を満たす)
    assert_eq!(Value::Float(f64::NAN), Value::Float(f64::NAN));
    // 異なる値は等しくない
    assert_ne!(Value::Float(1.0), Value::Float(1.5));
}

#[test]
fn compares_values_structurally() {
    // 表現が違えば値も違う
    assert_ne!(Value::Unsigned(1), Value::Simple(1));
    assert_ne!(Value::Unsigned(1), Value::Negative(1));
    assert_ne!(Value::Null, Value::Undefined);
    assert_ne!(
        Value::ByteString(vec![1]),
        Value::TextString(String::from("\u{1}"))
    );
    // タグは番号と内容の両方で比較する
    assert_eq!(
        Value::Tag(1, Box::new(Value::Unsigned(2))),
        Value::Tag(1, Box::new(Value::Unsigned(2)))
    );
    assert_ne!(
        Value::Tag(1, Box::new(Value::Unsigned(2))),
        Value::Tag(1, Box::new(Value::Unsigned(3)))
    );
    assert_ne!(
        Value::Tag(1, Box::new(Value::Unsigned(2))),
        Value::Tag(2, Box::new(Value::Unsigned(2)))
    );
    // マップは構造的な等価性で比較するため、順序が違えば等しくない
    let a = Value::Map(vec![
        (Value::Unsigned(1), Value::Unsigned(2)),
        (Value::Unsigned(3), Value::Unsigned(4)),
    ]);
    let b = Value::Map(vec![
        (Value::Unsigned(3), Value::Unsigned(4)),
        (Value::Unsigned(1), Value::Unsigned(2)),
    ]);
    assert_ne!(a, b);
}

#[test]
fn provides_accessors() {
    assert_eq!(Value::Unsigned(1).as_unsigned(), Some(1));
    assert_eq!(Value::Unsigned(1).as_negative(), None);
    assert_eq!(Value::Negative(1).as_negative(), Some(1));
    assert_eq!(Value::Negative(1).as_unsigned(), None);
    assert_eq!(
        Value::Unsigned(u64::MAX).as_integer(),
        Some(i128::from(u64::MAX))
    );
    assert_eq!(
        Value::Negative(u64::MAX).as_integer(),
        Some(-18446744073709551616)
    );
    assert_eq!(Value::Float(1.5).as_f64(), Some(1.5));
    assert_eq!(
        Value::ByteString(vec![1, 2]).as_bytes(),
        Some([1, 2].as_slice())
    );
    assert_eq!(Value::TextString(String::from("a")).as_text(), Some("a"));
    assert_eq!(
        Value::Array(vec![Value::Unsigned(1)]).as_array(),
        Some([Value::Unsigned(1)].as_slice())
    );
    assert_eq!(
        Value::Map(vec![(Value::Unsigned(1), Value::Unsigned(2))]).as_map(),
        Some([(Value::Unsigned(1), Value::Unsigned(2))].as_slice())
    );
    assert!(Value::Null.is_null());
    assert!(Value::Undefined.is_undefined());
    assert!(!Value::Unsigned(0).is_null());
    assert!(!Value::Unsigned(0).is_undefined());

    // 異なる variant では None になる
    assert_eq!(Value::Unsigned(1).as_text(), None);
    assert_eq!(Value::TextString(String::from("a")).as_integer(), None);
    assert_eq!(Value::Null.as_f64(), None);
    assert_eq!(Value::Null.as_bytes(), None);
    assert_eq!(Value::Null.as_array(), None);
    assert_eq!(Value::Null.as_map(), None);
}
