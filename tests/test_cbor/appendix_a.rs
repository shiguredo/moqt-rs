//! RFC 8949 Appendix A (Table 6) のデータ項目の例を検証するテスト
//!
//! デコード・診断記法・決定論的エンコードを 1 箇所でまとめて検証する。

use crate::{bytes_to_hex, hex_to_bytes};

use shiguredo_moqt::cbor::decode::decode;

/// RFC 8949 Appendix A のデータ項目の例
#[derive(Debug, Clone, Copy)]
struct Example {
    /// Appendix A の 16 進エンコード (`0x` を除く)
    hex: &'static str,
    /// 期待する診断記法
    ///
    /// RFC の表記を本ライブラリの出力に合わせて読み替えたもの。
    /// bignum はタグ付きの形式、非 ASCII 文字は生の UTF-8、
    /// indefinite-length は実体化した definite-length の形式になる。
    diagnostic: &'static str,
    /// 決定論的エンコードの期待値
    ///
    /// 浮動小数点値は値を保つ最短の幅に、map はキーの決定論的エンコードの
    /// 辞書順に、indefinite-length は definite-length に正規化される。
    canonical_hex: &'static str,
}

/// RFC 8949 Appendix A (Table 6) のデータ項目の例
const APPENDIX_A: &[Example] = &[
    // 符号なし整数
    Example {
        hex: "00",
        diagnostic: "0",
        canonical_hex: "00",
    },
    Example {
        hex: "01",
        diagnostic: "1",
        canonical_hex: "01",
    },
    Example {
        hex: "0a",
        diagnostic: "10",
        canonical_hex: "0a",
    },
    Example {
        hex: "17",
        diagnostic: "23",
        canonical_hex: "17",
    },
    Example {
        hex: "1818",
        diagnostic: "24",
        canonical_hex: "1818",
    },
    Example {
        hex: "1819",
        diagnostic: "25",
        canonical_hex: "1819",
    },
    Example {
        hex: "1864",
        diagnostic: "100",
        canonical_hex: "1864",
    },
    Example {
        hex: "1903e8",
        diagnostic: "1000",
        canonical_hex: "1903e8",
    },
    Example {
        hex: "1a000f4240",
        diagnostic: "1000000",
        canonical_hex: "1a000f4240",
    },
    Example {
        hex: "1b000000e8d4a51000",
        diagnostic: "1000000000000",
        canonical_hex: "1b000000e8d4a51000",
    },
    Example {
        hex: "1bffffffffffffffff",
        diagnostic: "18446744073709551615",
        canonical_hex: "1bffffffffffffffff",
    },
    // 負の整数
    Example {
        hex: "20",
        diagnostic: "-1",
        canonical_hex: "20",
    },
    Example {
        hex: "29",
        diagnostic: "-10",
        canonical_hex: "29",
    },
    Example {
        hex: "3863",
        diagnostic: "-100",
        canonical_hex: "3863",
    },
    Example {
        hex: "3903e7",
        diagnostic: "-1000",
        canonical_hex: "3903e7",
    },
    Example {
        hex: "3bffffffffffffffff",
        diagnostic: "-18446744073709551616",
        canonical_hex: "3bffffffffffffffff",
    },
    // bignum (タグ 2, 3)
    Example {
        hex: "c249010000000000000000",
        diagnostic: "2(h'010000000000000000')",
        canonical_hex: "c249010000000000000000",
    },
    Example {
        hex: "c349010000000000000000",
        diagnostic: "3(h'010000000000000000')",
        canonical_hex: "c349010000000000000000",
    },
    // 浮動小数点 (半精度・単精度は f64 に正確に拡張される)
    Example {
        hex: "f90000",
        diagnostic: "0.0",
        canonical_hex: "f90000",
    },
    Example {
        hex: "f98000",
        diagnostic: "-0.0",
        canonical_hex: "f98000",
    },
    Example {
        hex: "f93c00",
        diagnostic: "1.0",
        canonical_hex: "f93c00",
    },
    Example {
        hex: "fb3ff199999999999a",
        diagnostic: "1.1",
        canonical_hex: "fb3ff199999999999a",
    },
    Example {
        hex: "f93e00",
        diagnostic: "1.5",
        canonical_hex: "f93e00",
    },
    Example {
        hex: "f97bff",
        diagnostic: "65504.0",
        canonical_hex: "f97bff",
    },
    Example {
        hex: "fa47c35000",
        diagnostic: "100000.0",
        canonical_hex: "fa47c35000",
    },
    Example {
        hex: "fa7f7fffff",
        diagnostic: "3.4028234663852886e+38",
        canonical_hex: "fa7f7fffff",
    },
    Example {
        hex: "fb7e37e43c8800759c",
        diagnostic: "1.0e+300",
        canonical_hex: "fb7e37e43c8800759c",
    },
    Example {
        hex: "f90001",
        diagnostic: "5.960464477539063e-8",
        canonical_hex: "f90001",
    },
    Example {
        hex: "f90400",
        diagnostic: "0.00006103515625",
        canonical_hex: "f90400",
    },
    Example {
        hex: "f9c400",
        diagnostic: "-4.0",
        canonical_hex: "f9c400",
    },
    Example {
        hex: "fbc010666666666666",
        diagnostic: "-4.1",
        canonical_hex: "fbc010666666666666",
    },
    Example {
        hex: "f97c00",
        diagnostic: "Infinity",
        canonical_hex: "f97c00",
    },
    Example {
        hex: "f97e00",
        diagnostic: "NaN",
        canonical_hex: "f97e00",
    },
    Example {
        hex: "f9fc00",
        diagnostic: "-Infinity",
        canonical_hex: "f9fc00",
    },
    // 単精度・倍精度の無限大・NaN は決定論的エンコードで半精度に短縮される
    Example {
        hex: "fa7f800000",
        diagnostic: "Infinity",
        canonical_hex: "f97c00",
    },
    Example {
        hex: "fa7fc00000",
        diagnostic: "NaN",
        canonical_hex: "f97e00",
    },
    Example {
        hex: "faff800000",
        diagnostic: "-Infinity",
        canonical_hex: "f9fc00",
    },
    Example {
        hex: "fb7ff0000000000000",
        diagnostic: "Infinity",
        canonical_hex: "f97c00",
    },
    Example {
        hex: "fb7ff8000000000000",
        diagnostic: "NaN",
        canonical_hex: "f97e00",
    },
    Example {
        hex: "fbfff0000000000000",
        diagnostic: "-Infinity",
        canonical_hex: "f9fc00",
    },
    // simple value
    Example {
        hex: "f0",
        diagnostic: "simple(16)",
        canonical_hex: "f0",
    },
    Example {
        hex: "f8ff",
        diagnostic: "simple(255)",
        canonical_hex: "f8ff",
    },
    Example {
        hex: "f4",
        diagnostic: "false",
        canonical_hex: "f4",
    },
    Example {
        hex: "f5",
        diagnostic: "true",
        canonical_hex: "f5",
    },
    Example {
        hex: "f6",
        diagnostic: "null",
        canonical_hex: "f6",
    },
    Example {
        hex: "f7",
        diagnostic: "undefined",
        canonical_hex: "f7",
    },
    // タグ
    Example {
        hex: "c074323031332d30332d32315432303a30343a30305a",
        diagnostic: "0(\"2013-03-21T20:04:00Z\")",
        canonical_hex: "c074323031332d30332d32315432303a30343a30305a",
    },
    Example {
        hex: "c11a514b67b0",
        diagnostic: "1(1363896240)",
        canonical_hex: "c11a514b67b0",
    },
    Example {
        hex: "c1fb41d452d9ec200000",
        diagnostic: "1(1363896240.5)",
        canonical_hex: "c1fb41d452d9ec200000",
    },
    Example {
        hex: "d74401020304",
        diagnostic: "23(h'01020304')",
        canonical_hex: "d74401020304",
    },
    Example {
        hex: "d818456449455446",
        diagnostic: "24(h'6449455446')",
        canonical_hex: "d818456449455446",
    },
    Example {
        hex: "d82076687474703a2f2f7777772e6578616d706c652e636f6d",
        diagnostic: "32(\"http://www.example.com\")",
        canonical_hex: "d82076687474703a2f2f7777772e6578616d706c652e636f6d",
    },
    // バイト文字列
    Example {
        hex: "40",
        diagnostic: "h''",
        canonical_hex: "40",
    },
    Example {
        hex: "4401020304",
        diagnostic: "h'01020304'",
        canonical_hex: "4401020304",
    },
    // テキスト文字列 (非 ASCII は生の UTF-8 で出力する)
    Example {
        hex: "60",
        diagnostic: "\"\"",
        canonical_hex: "60",
    },
    Example {
        hex: "6161",
        diagnostic: "\"a\"",
        canonical_hex: "6161",
    },
    Example {
        hex: "6449455446",
        diagnostic: "\"IETF\"",
        canonical_hex: "6449455446",
    },
    Example {
        hex: "62225c",
        diagnostic: r#""\"\\""#,
        canonical_hex: "62225c",
    },
    Example {
        hex: "62c3bc",
        diagnostic: "\"ü\"",
        canonical_hex: "62c3bc",
    },
    Example {
        hex: "63e6b0b4",
        diagnostic: "\"水\"",
        canonical_hex: "63e6b0b4",
    },
    Example {
        hex: "64f0908591",
        diagnostic: "\"𐅑\"",
        canonical_hex: "64f0908591",
    },
    // 配列とマップ
    Example {
        hex: "80",
        diagnostic: "[]",
        canonical_hex: "80",
    },
    Example {
        hex: "83010203",
        diagnostic: "[1, 2, 3]",
        canonical_hex: "83010203",
    },
    Example {
        hex: "8301820203820405",
        diagnostic: "[1, [2, 3], [4, 5]]",
        canonical_hex: "8301820203820405",
    },
    Example {
        hex: "98190102030405060708090a0b0c0d0e0f101112131415161718181819",
        diagnostic: "[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25]",
        canonical_hex: "98190102030405060708090a0b0c0d0e0f101112131415161718181819",
    },
    Example {
        hex: "a0",
        diagnostic: "{}",
        canonical_hex: "a0",
    },
    Example {
        hex: "a201020304",
        diagnostic: "{1: 2, 3: 4}",
        canonical_hex: "a201020304",
    },
    Example {
        hex: "a26161016162820203",
        diagnostic: "{\"a\": 1, \"b\": [2, 3]}",
        canonical_hex: "a26161016162820203",
    },
    Example {
        hex: "826161a161626163",
        diagnostic: "[\"a\", {\"b\": \"c\"}]",
        canonical_hex: "826161a161626163",
    },
    Example {
        hex: "a56161614161626142616361436164614461656145",
        diagnostic: "{\"a\": \"A\", \"b\": \"B\", \"c\": \"C\", \"d\": \"D\", \"e\": \"E\"}",
        canonical_hex: "a56161614161626142616361436164614461656145",
    },
    // indefinite-length (実体化した definite-length の形式で出力する)
    Example {
        hex: "5f42010243030405ff",
        diagnostic: "h'0102030405'",
        canonical_hex: "450102030405",
    },
    Example {
        hex: "7f657374726561646d696e67ff",
        diagnostic: "\"streaming\"",
        canonical_hex: "6973747265616d696e67",
    },
    Example {
        hex: "9fff",
        diagnostic: "[]",
        canonical_hex: "80",
    },
    Example {
        hex: "9f018202039f0405ffff",
        diagnostic: "[1, [2, 3], [4, 5]]",
        canonical_hex: "8301820203820405",
    },
    Example {
        hex: "9f01820203820405ff",
        diagnostic: "[1, [2, 3], [4, 5]]",
        canonical_hex: "8301820203820405",
    },
    Example {
        hex: "83018202039f0405ff",
        diagnostic: "[1, [2, 3], [4, 5]]",
        canonical_hex: "8301820203820405",
    },
    Example {
        hex: "83019f0203ff820405",
        diagnostic: "[1, [2, 3], [4, 5]]",
        canonical_hex: "8301820203820405",
    },
    Example {
        hex: "9f0102030405060708090a0b0c0d0e0f101112131415161718181819ff",
        diagnostic: "[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25]",
        canonical_hex: "98190102030405060708090a0b0c0d0e0f101112131415161718181819",
    },
    Example {
        hex: "bf61610161629f0203ffff",
        diagnostic: "{\"a\": 1, \"b\": [2, 3]}",
        canonical_hex: "a26161016162820203",
    },
    Example {
        hex: "826161bf61626163ff",
        diagnostic: "[\"a\", {\"b\": \"c\"}]",
        canonical_hex: "826161a161626163",
    },
    // 決定論的エンコードではキーがソートされる
    Example {
        hex: "bf6346756ef563416d7421ff",
        diagnostic: "{\"Fun\": true, \"Amt\": -2}",
        canonical_hex: "a263416d74216346756ef5",
    },
];

#[test]
fn decodes_appendix_a_examples() {
    for example in APPENDIX_A {
        let bytes = hex_to_bytes(example.hex);
        let value = decode(&bytes).unwrap_or_else(|error| {
            panic!("{} のデコードに失敗した: {error}", example.hex);
        });

        // 通常のエンコード結果を再デコードしても同じ値になること
        let reencoded = value.to_bytes();
        let decoded_again = decode(&reencoded).unwrap_or_else(|error| {
            panic!(
                "{} の再エンコード結果のデコードに失敗した: {error}",
                example.hex
            );
        });
        assert_eq!(decoded_again, value, "{} の往復で値が変わった", example.hex);
    }
}

#[test]
fn displays_appendix_a_examples() {
    for example in APPENDIX_A {
        let value = decode(&hex_to_bytes(example.hex)).unwrap_or_else(|error| {
            panic!("{} のデコードに失敗した: {error}", example.hex);
        });
        assert_eq!(
            value.to_string(),
            example.diagnostic,
            "{} の診断記法が違う",
            example.hex
        );
    }
}

#[test]
fn canonically_encodes_appendix_a_examples() {
    for example in APPENDIX_A {
        let value = decode(&hex_to_bytes(example.hex)).unwrap_or_else(|error| {
            panic!("{} のデコードに失敗した: {error}", example.hex);
        });

        // 決定論的エンコードが期待値と一致すること
        assert_eq!(
            bytes_to_hex(&value.to_canonical_bytes()),
            example.canonical_hex,
            "{} の決定論的エンコード結果が違う",
            example.hex
        );

        // 決定論的エンコードを再デコードしてエンコードし直しても同じ結果になること
        let canonical = decode(&hex_to_bytes(example.canonical_hex)).unwrap_or_else(|error| {
            panic!("{} のデコードに失敗した: {error}", example.canonical_hex);
        });
        assert_eq!(
            bytes_to_hex(&canonical.to_canonical_bytes()),
            example.canonical_hex,
            "{} の決定論的エンコードが安定しない",
            example.canonical_hex
        );
    }
}
