//! RFC 8949 と RFC 8742 のデコード動作を検証するテスト

use crate::hex_to_bytes;

use shiguredo_moqt::cbor::decode::{DEFAULT_MAX_DEPTH, Decoder, decode, decode_all};
use shiguredo_moqt::cbor::error::DecodeErrorKind;
use shiguredo_moqt::cbor::value::Value;

/// RFC 8949 Appendix F の well-formed でない例 (16 進数, 期待するエラー種別)
const INVALID_EXAMPLES: &[(&str, DecodeErrorKind)] = &[
    // 入力不足 (Appendix F well-formedness error kind 2)
    ("18", DecodeErrorKind::UnexpectedEnd),
    ("19", DecodeErrorKind::UnexpectedEnd),
    ("1a", DecodeErrorKind::UnexpectedEnd),
    ("1b", DecodeErrorKind::UnexpectedEnd),
    ("1901", DecodeErrorKind::UnexpectedEnd),
    ("1a0102", DecodeErrorKind::UnexpectedEnd),
    ("1b01020304050607", DecodeErrorKind::UnexpectedEnd),
    ("38", DecodeErrorKind::UnexpectedEnd),
    ("58", DecodeErrorKind::UnexpectedEnd),
    ("78", DecodeErrorKind::UnexpectedEnd),
    ("98", DecodeErrorKind::UnexpectedEnd),
    ("9a01ff00", DecodeErrorKind::UnexpectedEnd),
    ("b8", DecodeErrorKind::UnexpectedEnd),
    ("d8", DecodeErrorKind::UnexpectedEnd),
    ("f8", DecodeErrorKind::UnexpectedEnd),
    ("f900", DecodeErrorKind::UnexpectedEnd),
    ("fa0000", DecodeErrorKind::UnexpectedEnd),
    ("fb000000", DecodeErrorKind::UnexpectedEnd),
    // definite-length 文字列のデータ不足
    ("41", DecodeErrorKind::UnexpectedEnd),
    ("61", DecodeErrorKind::UnexpectedEnd),
    ("5affffffff00", DecodeErrorKind::UnexpectedEnd),
    ("5bffffffffffffffff010203", DecodeErrorKind::UnexpectedEnd),
    ("7affffffff00", DecodeErrorKind::UnexpectedEnd),
    ("7b7fffffffffffffff010203", DecodeErrorKind::UnexpectedEnd),
    // definite-length の配列・マップの要素不足
    ("81", DecodeErrorKind::UnexpectedEnd),
    ("818181818181818181", DecodeErrorKind::UnexpectedEnd),
    ("8200", DecodeErrorKind::UnexpectedEnd),
    ("a1", DecodeErrorKind::UnexpectedEnd),
    ("a20102", DecodeErrorKind::UnexpectedEnd),
    ("a100", DecodeErrorKind::UnexpectedEnd),
    ("a2000000", DecodeErrorKind::UnexpectedEnd),
    // タグ内容の不足
    ("c0", DecodeErrorKind::UnexpectedEnd),
    // indefinite-length 文字列の break 不足
    ("5f4100", DecodeErrorKind::UnexpectedEnd),
    ("7f6100", DecodeErrorKind::UnexpectedEnd),
    // indefinite-length の配列・マップの break 不足
    ("9f", DecodeErrorKind::UnexpectedEnd),
    ("9f0102", DecodeErrorKind::UnexpectedEnd),
    ("bf", DecodeErrorKind::UnexpectedEnd),
    ("bf01020102", DecodeErrorKind::UnexpectedEnd),
    ("819f", DecodeErrorKind::UnexpectedEnd),
    ("9f8000", DecodeErrorKind::UnexpectedEnd),
    // 9f を 5 個並べるが break は 4 個しかなく、最外殻が閉じていない
    (
        concat!("9f9f9f9f9f", "ffffffff"),
        DecodeErrorKind::UnexpectedEnd,
    ),
    // 9f 81 9f 81 9f 9f に対して break が 3 個しかない
    (
        concat!("9f819f819f9f", "ffffff"),
        DecodeErrorKind::UnexpectedEnd,
    ),
    // 予約された additional information (Appendix F subkind 1)
    ("1c", DecodeErrorKind::ReservedAdditionalInformation),
    ("1d", DecodeErrorKind::ReservedAdditionalInformation),
    ("1e", DecodeErrorKind::ReservedAdditionalInformation),
    ("3c", DecodeErrorKind::ReservedAdditionalInformation),
    ("3d", DecodeErrorKind::ReservedAdditionalInformation),
    ("3e", DecodeErrorKind::ReservedAdditionalInformation),
    ("5c", DecodeErrorKind::ReservedAdditionalInformation),
    ("5d", DecodeErrorKind::ReservedAdditionalInformation),
    ("5e", DecodeErrorKind::ReservedAdditionalInformation),
    ("7c", DecodeErrorKind::ReservedAdditionalInformation),
    ("7d", DecodeErrorKind::ReservedAdditionalInformation),
    ("7e", DecodeErrorKind::ReservedAdditionalInformation),
    ("9c", DecodeErrorKind::ReservedAdditionalInformation),
    ("9d", DecodeErrorKind::ReservedAdditionalInformation),
    ("9e", DecodeErrorKind::ReservedAdditionalInformation),
    ("bc", DecodeErrorKind::ReservedAdditionalInformation),
    ("bd", DecodeErrorKind::ReservedAdditionalInformation),
    ("be", DecodeErrorKind::ReservedAdditionalInformation),
    ("dc", DecodeErrorKind::ReservedAdditionalInformation),
    ("dd", DecodeErrorKind::ReservedAdditionalInformation),
    ("de", DecodeErrorKind::ReservedAdditionalInformation),
    ("fc", DecodeErrorKind::ReservedAdditionalInformation),
    ("fd", DecodeErrorKind::ReservedAdditionalInformation),
    ("fe", DecodeErrorKind::ReservedAdditionalInformation),
    // 予約された 2 バイト表現の simple value (Appendix F subkind 2)
    ("f800", DecodeErrorKind::InvalidSimpleValue),
    ("f801", DecodeErrorKind::InvalidSimpleValue),
    ("f818", DecodeErrorKind::InvalidSimpleValue),
    ("f81f", DecodeErrorKind::InvalidSimpleValue),
    // indefinite-length 文字列のチャンク違反 (Appendix F subkind 3)
    ("5f00ff", DecodeErrorKind::InvalidIndefiniteStringChunk),
    ("5f21ff", DecodeErrorKind::InvalidIndefiniteStringChunk),
    ("5f6100ff", DecodeErrorKind::InvalidIndefiniteStringChunk),
    ("5f80ff", DecodeErrorKind::InvalidIndefiniteStringChunk),
    ("5fa0ff", DecodeErrorKind::InvalidIndefiniteStringChunk),
    ("5fc000ff", DecodeErrorKind::InvalidIndefiniteStringChunk),
    ("5fe0ff", DecodeErrorKind::InvalidIndefiniteStringChunk),
    ("7f4100ff", DecodeErrorKind::InvalidIndefiniteStringChunk),
    (
        "5f5f4100ffff",
        DecodeErrorKind::InvalidIndefiniteStringChunk,
    ),
    (
        "7f7f6100ffff",
        DecodeErrorKind::InvalidIndefiniteStringChunk,
    ),
    // break stop code の位置違反 (Appendix F subkind 4)
    ("ff", DecodeErrorKind::UnexpectedBreak),
    ("81ff", DecodeErrorKind::UnexpectedBreak),
    ("8200ff", DecodeErrorKind::UnexpectedBreak),
    ("a1ff", DecodeErrorKind::UnexpectedBreak),
    ("a1ff00", DecodeErrorKind::UnexpectedBreak),
    ("a100ff", DecodeErrorKind::UnexpectedBreak),
    ("a20000ff", DecodeErrorKind::UnexpectedBreak),
    ("9f81ff", DecodeErrorKind::UnexpectedBreak),
    // 9f 82 9f 81 9f 9f のうち内側 3 個を break で閉じた後、82 の 2 個目の要素に break が来る
    (
        concat!("9f829f819f9f", "ffffffff"),
        DecodeErrorKind::UnexpectedBreak,
    ),
    ("bf00ff", DecodeErrorKind::MissingMapValue),
    ("bf000000ff", DecodeErrorKind::MissingMapValue),
    // major type 0, 1, 6 での indefinite-length (Appendix F subkind 5)
    ("1f", DecodeErrorKind::IndefiniteLengthNotAllowed),
    ("3f", DecodeErrorKind::IndefiniteLengthNotAllowed),
    ("df", DecodeErrorKind::IndefiniteLengthNotAllowed),
];

#[test]
fn rejects_appendix_f_examples() {
    for (hex, expected_kind) in INVALID_EXAMPLES {
        let bytes = hex_to_bytes(hex);
        match decode(&bytes) {
            Ok(value) => panic!("{hex} のデコードが成功した: {value}"),
            Err(error) => assert_eq!(
                error.kind(),
                *expected_kind,
                "{hex} のエラー種別が違う: {error}"
            ),
        }
    }
}

#[test]
fn reports_error_positions() {
    // 予約された additional information は先頭バイトの位置を報告する
    let error = decode(&hex_to_bytes("1c")).expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::ReservedAdditionalInformation);
    assert_eq!(error.position(), 0);

    // 入力不足は入力の末尾を報告する
    let error = decode(&hex_to_bytes("18")).expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::UnexpectedEnd);
    assert_eq!(error.position(), 1);

    // 余分な入力は残りの先頭を報告する
    let error = decode(&hex_to_bytes("0000")).expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::TrailingData);
    assert_eq!(error.position(), 1);

    // UTF-8 違反は違反したバイトの位置を報告する
    let error = decode(&hex_to_bytes("6241ff")).expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::InvalidUtf8);
    assert_eq!(error.position(), 2);

    // indefinite-length 文字列のチャンク違反はチャンクの先頭を報告する
    let error = decode(&hex_to_bytes("5f00ff")).expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::InvalidIndefiniteStringChunk);
    assert_eq!(error.position(), 1);

    // キーの直後の break は break の位置を報告する
    let error = decode(&hex_to_bytes("bf00ff")).expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::MissingMapValue);
    assert_eq!(error.position(), 2);

    // 深さ制限は制限を超えたデータ項目の先頭を報告する
    let mut bytes = vec![0x81u8; DEFAULT_MAX_DEPTH + 1];
    bytes.push(0x01);
    let error = decode(&bytes).expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::DepthLimitExceeded);
    assert_eq!(error.position(), DEFAULT_MAX_DEPTH + 1);
}

#[test]
fn jump_table_for_initial_byte() {
    // RFC 8949 Appendix B (Table 7) のジャンプテーブルを全 256 通り検証する。
    // 先頭 1 バイトだけを与え、先頭バイトの分類どおりの結果になることを確認する。
    for initial in 0u8..=0xff {
        let major = initial >> 5;
        let additional = initial & 0x1f;
        let expected = expected_jump_table_result(major, additional);
        match (decode(&[initial]), expected) {
            (Ok(actual), Ok(expected)) => {
                assert_eq!(actual, expected, "{initial:#04x} のデコード結果が違う");
            }
            (Err(error), Err(expected_kind)) => {
                assert_eq!(
                    error.kind(),
                    expected_kind,
                    "{initial:#04x} のエラー種別が違う: {error}"
                );
            }
            (actual, expected) => {
                panic!("{initial:#04x} の結果がジャンプテーブルと違う: {actual:?} / {expected:?}");
            }
        }
    }
}

/// RFC 8949 Appendix B (Table 7) に基づく先頭 1 バイトだけの入力の期待結果
///
/// 引数を持つ先頭バイトは続きのバイトが必要なため `UnexpectedEnd` になる。
fn expected_jump_table_result(major: u8, additional: u8) -> Result<Value, DecodeErrorKind> {
    if additional <= 23 {
        return match (major, additional) {
            (0, value) => Ok(Value::Unsigned(u64::from(value))),
            (1, value) => Ok(Value::Negative(u64::from(value))),
            (2, 0) => Ok(Value::ByteString(Vec::new())),
            (3, 0) => Ok(Value::TextString(String::new())),
            (4, 0) => Ok(Value::Array(Vec::new())),
            (5, 0) => Ok(Value::Map(Vec::new())),
            (7, value) if value <= 19 => Ok(Value::Simple(value)),
            (7, 20) => Ok(Value::Bool(false)),
            (7, 21) => Ok(Value::Bool(true)),
            (7, 22) => Ok(Value::Null),
            (7, 23) => Ok(Value::Undefined),
            _ => Err(DecodeErrorKind::UnexpectedEnd),
        };
    }
    match additional {
        24..=27 => Err(DecodeErrorKind::UnexpectedEnd),
        28..=30 => Err(DecodeErrorKind::ReservedAdditionalInformation),
        _ => match major {
            0 | 1 | 6 => Err(DecodeErrorKind::IndefiniteLengthNotAllowed),
            7 => Err(DecodeErrorKind::UnexpectedBreak),
            _ => Err(DecodeErrorKind::UnexpectedEnd),
        },
    }
}

#[test]
fn reads_arguments_in_network_byte_order() {
    // RFC 8949 Appendix B のとおり、符号なし整数はネットワークバイトオーダーで読む
    for (bytes, expected) in [
        (&[0x18, 0x80][..], Value::Unsigned(0x80)),
        (&[0x19, 0x01, 0x00][..], Value::Unsigned(0x0100)),
        (
            &[0x1a, 0x00, 0x01, 0x00, 0x00][..],
            Value::Unsigned(0x0001_0000),
        ),
        (
            &[0x1b, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef][..],
            Value::Unsigned(0x0123_4567_89ab_cdef),
        ),
    ] {
        assert_eq!(decode(bytes).expect("デコードに失敗した"), expected);
    }

    // 負の整数も同様
    for (bytes, expected) in [
        (&[0x38, 0x80][..], Value::Negative(0x80)),
        (&[0x39, 0x01, 0x00][..], Value::Negative(0x0100)),
        (
            &[0x3a, 0x00, 0x01, 0x00, 0x00][..],
            Value::Negative(0x0001_0000),
        ),
        (
            &[0x3b, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef][..],
            Value::Negative(0x0123_4567_89ab_cdef),
        ),
    ] {
        assert_eq!(decode(bytes).expect("デコードに失敗した"), expected);
    }

    // タグ番号
    assert_eq!(
        decode(&[0xd8, 0x18, 0x01]).expect("デコードに失敗した"),
        Value::Tag(24, Box::new(Value::Unsigned(1)))
    );
    assert_eq!(
        decode(&[0xd9, 0x01, 0x00, 0x01]).expect("デコードに失敗した"),
        Value::Tag(256, Box::new(Value::Unsigned(1)))
    );

    // 浮動小数点のビット列もビッグエンディアン
    assert_eq!(
        decode(&[0xf9, 0x3c, 0x00]).expect("デコードに失敗した"),
        Value::Float(1.0)
    );
    assert_eq!(
        decode(&[0xfa, 0x3f, 0x80, 0x00, 0x00]).expect("デコードに失敗した"),
        Value::Float(1.0)
    );
    assert_eq!(
        decode(&[0xfb, 0x3f, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
            .expect("デコードに失敗した"),
        Value::Float(1.0)
    );
}

#[test]
fn reports_errors_inside_indefinite_length_items() {
    // チャンクの長さ表現が途中で終わっている
    let error = decode(&hex_to_bytes("5f58")).expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::UnexpectedEnd);
    assert_eq!(error.position(), 2);

    // テキスト文字列チャンクの長さが入力より大きい
    let error = decode(&hex_to_bytes("7f7affffffff00ff")).expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::UnexpectedEnd);
    assert_eq!(error.position(), 8);

    // バイト文字列チャンクの長さが入力より大きい
    let error = decode(&hex_to_bytes("5f5bffffffffffffffff41ff")).expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::UnexpectedEnd);
    assert_eq!(error.position(), 12);

    // indefinite-length 配列の途中で入力が終わる
    let error = decode(&hex_to_bytes("9f01")).expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::UnexpectedEnd);
    assert_eq!(error.position(), 2);

    // indefinite-length マップの途中で入力が終わる
    let error = decode(&hex_to_bytes("bf0102")).expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::UnexpectedEnd);
    assert_eq!(error.position(), 3);
}

#[test]
fn formats_decode_errors() {
    for (hex, expected) in [
        ("18", "unexpected end of input at offset 1"),
        ("0000", "trailing data after the data item at offset 1"),
        (
            "1c",
            "reserved additional information (28..=30) at offset 0",
        ),
        (
            "1f",
            "indefinite length is not allowed for this major type at offset 0",
        ),
        ("ff", "unexpected break stop code at offset 0"),
        ("f800", "invalid simple value encoding at offset 0"),
        (
            "5f00ff",
            "invalid indefinite-length string chunk at offset 1",
        ),
        ("bf00ff", "missing map value at offset 2"),
        ("6241ff", "invalid UTF-8 text string at offset 2"),
    ] {
        let error = decode(&hex_to_bytes(hex)).expect_err("デコードが成功した");
        assert_eq!(
            error.to_string(),
            expected,
            "{hex} のエラーメッセージが違う"
        );
    }

    // 深さ制限のエラーメッセージ
    let mut bytes = vec![0x81u8; DEFAULT_MAX_DEPTH + 1];
    bytes.push(0x01);
    let error = decode(&bytes).expect_err("デコードが成功した");
    assert_eq!(
        error.to_string(),
        format!(
            "maximum nesting depth exceeded at offset {}",
            DEFAULT_MAX_DEPTH + 1
        )
    );
}

#[test]
fn decodes_utf8_text_strings() {
    // 非 ASCII のテキスト文字列
    assert_eq!(
        decode(&hex_to_bytes("62c3bc")).expect("デコードに失敗した"),
        Value::TextString(String::from("ü"))
    );
    // 4 バイトの UTF-8 (U+10151)
    assert_eq!(
        decode(&hex_to_bytes("64f0908591")).expect("デコードに失敗した"),
        Value::TextString(String::from("𐅑"))
    );
    // indefinite-length のテキスト文字列はチャンクごとに UTF-8 を検証する
    assert_eq!(
        decode(&hex_to_bytes("7f62c3bc62c3bcff")).expect("デコードに失敗した"),
        Value::TextString(String::from("üü"))
    );
    // コードポイントをチャンク間にまたがせることはできない
    let error = decode(&hex_to_bytes("7f61c362bcff")).expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::InvalidUtf8);
    assert_eq!(error.position(), 2);
}

#[test]
fn decodes_simple_values_and_floats() {
    // 半精度・単精度・倍精度はすべて f64 に正確に拡張される
    assert_eq!(
        decode(&hex_to_bytes("f97bff")).expect("デコードに失敗した"),
        Value::Float(65504.0)
    );
    assert_eq!(
        decode(&hex_to_bytes("fa7f7fffff")).expect("デコードに失敗した"),
        Value::Float(f64::from(f32::MAX))
    );
    assert_eq!(
        decode(&hex_to_bytes("fb3ff199999999999a")).expect("デコードに失敗した"),
        Value::Float(1.1)
    );
    // 割り当てられていない simple value
    assert_eq!(
        decode(&hex_to_bytes("f0")).expect("デコードに失敗した"),
        Value::Simple(16)
    );
    assert_eq!(
        decode(&hex_to_bytes("f8ff")).expect("デコードに失敗した"),
        Value::Simple(255)
    );
    // false / true / null / undefined
    assert_eq!(
        decode(&hex_to_bytes("f4")).expect("デコードに失敗した"),
        Value::Bool(false)
    );
    assert_eq!(
        decode(&hex_to_bytes("f5")).expect("デコードに失敗した"),
        Value::Bool(true)
    );
    assert_eq!(
        decode(&hex_to_bytes("f6")).expect("デコードに失敗した"),
        Value::Null
    );
    assert_eq!(
        decode(&hex_to_bytes("f7")).expect("デコードに失敗した"),
        Value::Undefined
    );
}

#[test]
fn decodes_non_preferred_encodings() {
    // RFC 8949 Section 4.1 のとおり、デコーダは非最短形式も受け付ける
    assert_eq!(
        decode(&hex_to_bytes("1800")).expect("デコードに失敗した"),
        Value::Unsigned(0)
    );
    assert_eq!(
        decode(&hex_to_bytes("3800")).expect("デコードに失敗した"),
        Value::Negative(0)
    );
    // 超長い長さ表現の文字列
    assert_eq!(
        decode(&hex_to_bytes("5b000000000000000141")).expect("デコードに失敗した"),
        Value::ByteString(vec![0x41])
    );
    assert_eq!(
        decode(&hex_to_bytes("7b000000000000000161")).expect("デコードに失敗した"),
        Value::TextString(String::from("a"))
    );
}

#[test]
fn decodes_indefinite_length_items() {
    // 空の indefinite-length バイト文字列
    assert_eq!(
        decode(&hex_to_bytes("5fff")).expect("デコードに失敗した"),
        Value::ByteString(Vec::new())
    );
    // 空チャンクだけの indefinite-length テキスト文字列
    assert_eq!(
        decode(&hex_to_bytes("7f60ff")).expect("デコードに失敗した"),
        Value::TextString(String::new())
    );
    // 0 バイトのチャンクが混ざっていても連結される
    assert_eq!(
        decode(&hex_to_bytes("5f404141ff")).expect("デコードに失敗した"),
        Value::ByteString(vec![0x41])
    );
    // indefinite-length のマップ
    assert_eq!(
        decode(&hex_to_bytes("bf616101616202ff")).expect("デコードに失敗した"),
        Value::Map(vec![
            (Value::TextString(String::from("a")), Value::Unsigned(1)),
            (Value::TextString(String::from("b")), Value::Unsigned(2)),
        ])
    );
}

#[test]
fn keeps_duplicate_map_keys() {
    // RFC 8949 Section 5.6 のとおり、重複キーは許容して全てのエントリを保持する
    let value = decode(&hex_to_bytes("a201010102")).expect("デコードに失敗した");
    assert_eq!(
        value,
        Value::Map(vec![
            (Value::Unsigned(1), Value::Unsigned(1)),
            (Value::Unsigned(1), Value::Unsigned(2)),
        ])
    );
}

#[test]
fn keeps_tag_contents_without_validation() {
    // タグ内容の妥当性は検証しない (RFC 8949 Section 5.3.2)
    assert_eq!(
        decode(&hex_to_bytes("c001")).expect("デコードに失敗した"),
        Value::Tag(0, Box::new(Value::Unsigned(1)))
    );
    // 既知のタグも解釈しない
    assert_eq!(
        decode(&hex_to_bytes("c249010000000000000000")).expect("デコードに失敗した"),
        Value::Tag(
            2,
            Box::new(Value::ByteString(vec![1, 0, 0, 0, 0, 0, 0, 0, 0]))
        )
    );
}

#[test]
fn decoder_decodes_sequence_incrementally() {
    let bytes = hex_to_bytes("01626869f5");
    let mut decoder = Decoder::new(&bytes);
    assert_eq!(decoder.position(), 0);
    assert_eq!(decoder.remaining(), bytes.as_slice());

    assert_eq!(
        decoder.decode().expect("デコードに失敗した"),
        Value::Unsigned(1)
    );
    assert_eq!(
        decoder.decode().expect("デコードに失敗した"),
        Value::TextString(String::from("hi"))
    );
    assert_eq!(
        decoder.decode().expect("デコードに失敗した"),
        Value::Bool(true)
    );

    assert!(decoder.is_finished());
    assert_eq!(decoder.position(), bytes.len());
    assert!(decoder.remaining().is_empty());
    // 使い切った後のデコードは入力不足になる
    let error = decoder.decode().expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::UnexpectedEnd);
}

#[test]
fn decode_all_decodes_cbor_sequence() {
    // 空の CBOR シーケンスは空の Vec になる (RFC 8742 Section 2)
    assert_eq!(
        decode_all(&[]).expect("デコードに失敗した"),
        Vec::<Value>::new()
    );
    assert_eq!(
        decode_all(&hex_to_bytes("0102f6")).expect("デコードに失敗した"),
        vec![Value::Unsigned(1), Value::Unsigned(2), Value::Null]
    );
    // 途中のデータ項目が不正な場合はエラーを返す
    let error = decode_all(&hex_to_bytes("011c")).expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::ReservedAdditionalInformation);
    assert_eq!(error.position(), 1);
}

#[test]
fn enforces_depth_limit() {
    // デフォルトの上限ちょうど (128 段の入れ子) は成功する
    let mut ok_bytes = vec![0x81u8; DEFAULT_MAX_DEPTH];
    ok_bytes.push(0x01);
    decode(&ok_bytes).expect("上限ちょうどのデコードに失敗した");

    // 上限を超えると DepthLimitExceeded になる
    let mut too_deep_bytes = vec![0x81u8; DEFAULT_MAX_DEPTH + 1];
    too_deep_bytes.push(0x01);
    let error = decode(&too_deep_bytes).expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::DepthLimitExceeded);

    // タグも深さに数える
    let mut tag_bytes = vec![0xc0u8; DEFAULT_MAX_DEPTH + 1];
    tag_bytes.push(0x01);
    let error = decode(&tag_bytes).expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::DepthLimitExceeded);
}

#[test]
fn supports_custom_depth_limit() {
    // 上限 0 では入れ子の要素をデコードできない
    let error = Decoder::with_max_depth(&hex_to_bytes("8101"), 0)
        .decode()
        .expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::DepthLimitExceeded);
    assert_eq!(error.position(), 1);

    // 上限 1 なら 1 段の入れ子をデコードできる
    assert_eq!(
        Decoder::with_max_depth(&hex_to_bytes("8101"), 1)
            .decode()
            .expect("デコードに失敗した"),
        Value::Array(vec![Value::Unsigned(1)])
    );

    // set_max_depth で変更できる
    let bytes = hex_to_bytes("8101");
    let mut decoder = Decoder::new(&bytes);
    assert_eq!(decoder.max_depth(), DEFAULT_MAX_DEPTH);
    decoder.set_max_depth(0);
    assert_eq!(decoder.max_depth(), 0);
    let error = decoder.decode().expect_err("デコードが成功した");
    assert_eq!(error.kind(), DecodeErrorKind::DepthLimitExceeded);
}
