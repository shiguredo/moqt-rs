//! RFC 8949 Section 8 の診断記法 (Display) を検証するテスト

use shiguredo_moqt::cbor::value::Value;

#[test]
fn escapes_text_strings_like_json() {
    assert_eq!(
        Value::TextString(String::from("a\nb\tc\u{1}d\"e\\f")).to_string(),
        "\"a\\nb\\tc\\u0001d\\\"e\\\\f\""
    );
    assert_eq!(
        Value::TextString(String::from("\u{8}\u{c}\r")).to_string(),
        "\"\\b\\f\\r\""
    );
    // 制御文字以外の非 ASCII はそのまま出力する
    assert_eq!(Value::TextString(String::from("水")).to_string(), "\"水\"");
}

#[test]
fn formats_integers() {
    assert_eq!(
        Value::Unsigned(u64::MAX).to_string(),
        "18446744073709551615"
    );
    assert_eq!(
        Value::Negative(u64::MAX).to_string(),
        "-18446744073709551616"
    );
}

#[test]
fn formats_float_boundaries() {
    for (value, expected) in [
        (0.0, "0.0"),
        (-0.0, "-0.0"),
        (1.0, "1.0"),
        (-4.0, "-4.0"),
        (1.0e-5, "0.00001"),
        (1.0e-6, "1.0e-6"),
        (1.0e16, "10000000000000000.0"),
        (1.0e17, "1.0e+17"),
        (1.0e300, "1.0e+300"),
        (f64::INFINITY, "Infinity"),
        (f64::NEG_INFINITY, "-Infinity"),
        (f64::NAN, "NaN"),
    ] {
        assert_eq!(
            Value::Float(value).to_string(),
            expected,
            "{value} の診断記法が違う"
        );
    }
}

#[test]
fn displays_nested_containers() {
    let value = Value::Map(vec![
        (
            Value::TextString(String::from("a")),
            Value::Array(vec![Value::Unsigned(1), Value::Bool(true)]),
        ),
        (Value::Unsigned(2), Value::Null),
        (
            Value::Tag(0, Box::new(Value::TextString(String::from("x")))),
            Value::Undefined,
        ),
    ]);
    assert_eq!(
        value.to_string(),
        "{\"a\": [1, true], 2: null, 0(\"x\"): undefined}"
    );
}
