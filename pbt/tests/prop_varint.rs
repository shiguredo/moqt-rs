use shiguredo_moqt::varint;

use pbt::common::sample_varint;
use pbt::common::test_runner;

/// 任意の u64 のエンコード → デコード ラウンドトリップ
#[test]
fn roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let val = sample_varint(ctx);
        let mut buf = Vec::new();
        varint::encode(val, &mut buf);
        let (decoded, consumed) =
            varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded, val);
        assert_eq!(consumed, buf.len());
        assert_eq!(consumed, varint::encoded_len(val));
        Ok(())
    })?;
    Ok(())
}

/// 先頭バイトの leading-1-bits がエンコード長を正しく示す
#[test]
fn leading_ones_indicate_length() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let val = sample_varint(ctx);
        let mut buf = Vec::new();
        varint::encode(val, &mut buf);
        let expected_len = varint::encoded_len(val);
        let leading_ones = buf[0].leading_ones();
        let expected_leading_ones = match expected_len {
            1 => 0u32,
            2 => 1u32,
            3 => 2u32,
            4 => 3u32,
            5 => 4u32,
            6 => 5u32,
            7 => 6u32, // 1111110x: draft-ietf-moq-transport-21 Appendix A.3 (Since draft-ietf-moq-transport-17) #1595 で追加
            8 => 6u32, // 11111110: leading_ones=7 だが 0xFE は first=0xFE で leading_ones()=7
            9 => 8u32,
            _ => unreachable!("vi64 encoded length is always 1..=9"),
        };
        // 8 バイトの場合は先頭バイトが 0xFE (11111110) で leading_ones=7
        if expected_len == 8 {
            assert_eq!(buf[0], 0xFE);
        } else if expected_len == 7 {
            // 7 バイトの場合は先頭バイトが 1111110x (0xFC-0xFD)
            assert_eq!(buf[0] & 0xFE, 0xFC);
            assert_eq!(leading_ones, expected_leading_ones);
        } else {
            assert_eq!(leading_ones, expected_leading_ones);
        }
        Ok(())
    })?;
    Ok(())
}

/// 値をその最小長より長い varint でエンコードする (非最小形を作るテスト用ヘルパー)
///
/// `len` は `varint::encoded_len(value)` 以上でなければならない。
/// 先頭バイトのプレフィックスは長さごとの値を書き、残りは値のビッグエンディアン表現にする。
fn encode_with_len(value: u64, len: usize) -> Vec<u8> {
    let prefix: u8 = match len {
        1 => 0x00,
        2 => 0x80,
        3 => 0xC0,
        4 => 0xE0,
        5 => 0xF0,
        6 => 0xF8,
        7 => 0xFC,
        8 => 0xFE,
        9 => 0xFF,
        _ => unreachable!("vi64 のエンコード長は 1..=9 である"),
    };
    // 先頭バイトが運ぶ値ビット数は 8 - len (len = 8 と 9 は 0)
    let first_bits = 8usize.saturating_sub(len);
    let mask: u8 = if first_bits == 0 {
        0
    } else {
        (1u8 << first_bits) - 1
    };
    let first_value_bits = if len >= 9 {
        0
    } else {
        ((value >> (8 * (len - 1))) as u8) & mask
    };
    let mut buf = Vec::with_capacity(len);
    buf.push(prefix | first_value_bits);
    for i in (0..len - 1).rev() {
        buf.push((value >> (8 * i)) as u8);
    }
    buf
}

/// 非最小エンコーディングも decode できる
///
/// draft-ietf-moq-transport-21 §8.1: "Variable-length integers do not need to be
/// encoded using the minimum number of bytes; any encoding length that can
/// represent the value is valid."
#[test]
fn non_minimal_encoding_decodes_to_the_same_value() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let val = sample_varint(ctx);
        let minimal_len = varint::encoded_len(val);
        // 最小長から 9 バイトまでの各長で非最小形を作る
        for len in minimal_len..=9usize {
            let mut buf = encode_with_len(val, len);
            assert_eq!(buf.len(), len);
            // 後続フィールドを模したバイトを足し、消費長が len で止まることも見る
            buf.push(0xAA);
            let (decoded, consumed) = varint::decode(&buf).expect("非最小形も decode できる");
            assert_eq!(decoded, val, "minimal_len={minimal_len} len={len}");
            assert_eq!(consumed, len, "minimal_len={minimal_len} len={len}");
        }
        Ok(())
    })?;
    Ok(())
}
