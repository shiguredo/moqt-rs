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
