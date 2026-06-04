use shiguredo_moqt::{error::MessageError, varint};

mod boundary_values {
    use super::*;

    #[test]
    fn zero() {
        let mut buf = Vec::new();
        varint::encode(0, &mut buf);
        assert_eq!(buf, vec![0x00]);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 0);
        assert_eq!(consumed, 1);
        assert_eq!(varint::encoded_len(0), 1);
    }

    #[test]
    fn one_byte_max() {
        let mut buf = Vec::new();
        varint::encode(127, &mut buf);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0], 0x7F);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 127);
        assert_eq!(consumed, 1);
        assert_eq!(varint::encoded_len(127), 1);
    }

    #[test]
    fn two_byte_min() {
        let mut buf = Vec::new();
        varint::encode(128, &mut buf);
        assert_eq!(buf.len(), 2);
        // 先頭バイトの leading bits = 10
        assert_eq!(buf[0] & 0xC0, 0x80);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 128);
        assert_eq!(consumed, 2);
        assert_eq!(varint::encoded_len(128), 2);
    }

    #[test]
    fn two_byte_max() {
        let mut buf = Vec::new();
        varint::encode(16383, &mut buf);
        assert_eq!(buf.len(), 2);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 16383);
        assert_eq!(consumed, 2);
        assert_eq!(varint::encoded_len(16383), 2);
    }

    #[test]
    fn three_byte_min() {
        let mut buf = Vec::new();
        varint::encode(16384, &mut buf);
        assert_eq!(buf.len(), 3);
        assert_eq!(buf[0] & 0xE0, 0xC0);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 16384);
        assert_eq!(consumed, 3);
        assert_eq!(varint::encoded_len(16384), 3);
    }

    #[test]
    fn three_byte_max() {
        let mut buf = Vec::new();
        varint::encode(2_097_151, &mut buf);
        assert_eq!(buf.len(), 3);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 2_097_151);
        assert_eq!(consumed, 3);
        assert_eq!(varint::encoded_len(2_097_151), 3);
    }

    #[test]
    fn four_byte_min() {
        let mut buf = Vec::new();
        varint::encode(2_097_152, &mut buf);
        assert_eq!(buf.len(), 4);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 2_097_152);
        assert_eq!(consumed, 4);
        assert_eq!(varint::encoded_len(2_097_152), 4);
    }

    #[test]
    fn four_byte_max() {
        let mut buf = Vec::new();
        varint::encode(268_435_455, &mut buf);
        assert_eq!(buf.len(), 4);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 268_435_455);
        assert_eq!(consumed, 4);
        assert_eq!(varint::encoded_len(268_435_455), 4);
    }

    #[test]
    fn five_byte_min() {
        let mut buf = Vec::new();
        varint::encode(268_435_456, &mut buf);
        assert_eq!(buf.len(), 5);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 268_435_456);
        assert_eq!(consumed, 5);
        assert_eq!(varint::encoded_len(268_435_456), 5);
    }

    #[test]
    fn five_byte_max() {
        let mut buf = Vec::new();
        varint::encode(34_359_738_367, &mut buf);
        assert_eq!(buf.len(), 5);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 34_359_738_367);
        assert_eq!(consumed, 5);
        assert_eq!(varint::encoded_len(34_359_738_367), 5);
    }

    #[test]
    fn six_byte_min() {
        let mut buf = Vec::new();
        varint::encode(34_359_738_368, &mut buf);
        assert_eq!(buf.len(), 6);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 34_359_738_368);
        assert_eq!(consumed, 6);
        assert_eq!(varint::encoded_len(34_359_738_368), 6);
    }

    #[test]
    fn six_byte_max() {
        let mut buf = Vec::new();
        varint::encode(4_398_046_511_103, &mut buf);
        assert_eq!(buf.len(), 6);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 4_398_046_511_103);
        assert_eq!(consumed, 6);
        assert_eq!(varint::encoded_len(4_398_046_511_103), 6);
    }

    #[test]
    fn seven_byte_min() {
        let mut buf = Vec::new();
        varint::encode(4_398_046_511_104, &mut buf);
        assert_eq!(buf.len(), 7);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 4_398_046_511_104);
        assert_eq!(consumed, 7);
        assert_eq!(varint::encoded_len(4_398_046_511_104), 7);
    }

    #[test]
    fn eight_byte_min() {
        let mut buf = Vec::new();
        varint::encode(562_949_953_421_312, &mut buf);
        assert_eq!(buf.len(), 8);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 562_949_953_421_312);
        assert_eq!(consumed, 8);
        assert_eq!(varint::encoded_len(562_949_953_421_312), 8);
    }

    #[test]
    fn eight_byte_max() {
        let mut buf = Vec::new();
        varint::encode(72_057_594_037_927_935, &mut buf);
        assert_eq!(buf.len(), 8);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 72_057_594_037_927_935);
        assert_eq!(consumed, 8);
        assert_eq!(varint::encoded_len(72_057_594_037_927_935), 8);
    }

    #[test]
    fn nine_byte_min() {
        let mut buf = Vec::new();
        varint::encode(72_057_594_037_927_936, &mut buf);
        assert_eq!(buf.len(), 9);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 72_057_594_037_927_936);
        assert_eq!(consumed, 9);
        assert_eq!(varint::encoded_len(72_057_594_037_927_936), 9);
    }

    #[test]
    fn max_varint() {
        let mut buf = Vec::new();
        varint::encode(u64::MAX, &mut buf);
        assert_eq!(buf.len(), 9);
        let (val, consumed) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, u64::MAX);
        assert_eq!(consumed, 9);
        assert_eq!(varint::encoded_len(u64::MAX), 9);
    }
}

mod error_cases {
    use super::*;

    #[test]
    fn empty_buffer() {
        assert_eq!(varint::decode(&[]), Err(MessageError::UnexpectedEof));
    }

    #[test]
    fn two_byte_truncated() {
        assert_eq!(varint::decode(&[0x80]), Err(MessageError::UnexpectedEof));
    }

    #[test]
    fn three_byte_truncated() {
        assert_eq!(
            varint::decode(&[0xC0, 0x00]),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn four_byte_truncated() {
        assert_eq!(
            varint::decode(&[0xE0, 0x00, 0x00]),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn five_byte_truncated() {
        assert_eq!(
            varint::decode(&[0xF0, 0x00, 0x00, 0x00]),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn six_byte_truncated() {
        assert_eq!(
            varint::decode(&[0xF8, 0x00, 0x00, 0x00, 0x00]),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn eight_byte_truncated() {
        assert_eq!(
            varint::decode(&[0xFE, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn nine_byte_truncated() {
        assert_eq!(
            varint::decode(&[0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
            Err(MessageError::UnexpectedEof)
        );
    }

    /// draft-ietf-moq-transport-21 §8.1 (Variable-Length Integers): 0xFC (11111100) は 7 バイトエンコーディングの先頭として有効
    #[test]
    fn valid_code_point_0xfc() {
        let (val, consumed) = varint::decode(&[0xFC, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(val, 0);
        assert_eq!(consumed, 7);
    }
}

/// draft-ietf-moq-transport-21 §8.1 (Variable-Length Integers) Table 4 (Example Integer Encodings) の
/// テストベクトル
mod spec_test_vectors {
    use super::*;

    #[test]
    fn value_37_one_byte() {
        assert_eq!(
            varint::decode(&[0x25]).expect("テストフィクスチャの前提条件を満たす"),
            (37, 1)
        );
    }

    #[test]
    fn value_37_two_bytes() {
        // 最短エンコードではないが有効
        assert_eq!(
            varint::decode(&[0x80, 0x25]).expect("テストフィクスチャの前提条件を満たす"),
            (37, 2)
        );
    }

    #[test]
    fn value_15293() {
        assert_eq!(
            varint::decode(&[0xBB, 0xBD]).expect("テストフィクスチャの前提条件を満たす"),
            (15293, 2)
        );
    }

    /// 494,878,333 は 4 バイト形式の上限 2^28 - 1 を超えるため、最短エンコードは
    /// 5 バイト形式の 0xf01d7f3e7d になる
    #[test]
    fn value_494878333() {
        assert_eq!(
            varint::decode(&[0xF0, 0x1D, 0x7F, 0x3E, 0x7D])
                .expect("テストフィクスチャの前提条件を満たす"),
            (494_878_333, 5)
        );
    }

    #[test]
    fn value_2893212287960() {
        assert_eq!(
            varint::decode(&[0xFA, 0xA1, 0xA0, 0xE4, 0x03, 0xD8])
                .expect("テストフィクスチャの前提条件を満たす"),
            (2_893_212_287_960, 6)
        );
    }

    #[test]
    fn value_70423237261249041() {
        assert_eq!(
            varint::decode(&[0xFE, 0xFA, 0x31, 0x8F, 0xA8, 0xE3, 0xCA, 0x11])
                .expect("テストフィクスチャの前提条件を満たす"),
            (70_423_237_261_249_041, 8)
        );
    }

    #[test]
    fn value_u64_max() {
        assert_eq!(
            varint::decode(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])
                .expect("テストフィクスチャの前提条件を満たす"),
            (u64::MAX, 9)
        );
    }
}

mod multi_decode {
    use super::*;

    #[test]
    fn consecutive_varints_in_buffer() {
        let mut buf = Vec::new();
        varint::encode(1, &mut buf);
        varint::encode(1000, &mut buf);
        varint::encode(1_000_000, &mut buf);

        let (v1, n1) = varint::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        let (v2, n2) = varint::decode(&buf[n1..]).expect("テストフィクスチャの前提条件を満たす");
        let (v3, n3) =
            varint::decode(&buf[n1 + n2..]).expect("テストフィクスチャの前提条件を満たす");

        assert_eq!(v1, 1);
        assert_eq!(v2, 1000);
        assert_eq!(v3, 1_000_000);
        assert_eq!(n1 + n2 + n3, buf.len());
    }
}

mod checked_len {
    use super::*;

    #[test]
    fn exact_match_length_is_accepted() {
        // len == remaining の境界は Ok(remaining) を返す
        let remaining = 16usize;
        assert_eq!(
            varint::checked_len(remaining as u64, remaining),
            Ok(remaining)
        );
    }

    #[test]
    fn one_over_remaining_is_eof() {
        // len == remaining + 1 は UnexpectedEof を返す
        let remaining = 16usize;
        assert_eq!(
            varint::checked_len(remaining as u64 + 1, remaining),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn u64_max_is_eof_without_32bit_truncation() {
        // u64::MAX は 32bit 環境で as usize 切り捨てると小さい値になり得るが、
        // u64 空間で remaining と比較するため切り捨てなく UnexpectedEof を返す
        assert_eq!(
            varint::checked_len(u64::MAX, 10),
            Err(MessageError::UnexpectedEof)
        );
    }
}

mod non_minimal {
    use super::*;

    /// 値を指定バイト長の varint でエンコードする (非最小形を作るテスト用ヘルパー)
    ///
    /// `len` は値が収まる長さでなければならない。先頭バイトのプレフィックスは
    /// 長さごとの値を書き、残りは値のビッグエンディアン表現にする。
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
            _ => panic!("vi64 のエンコード長は 1..=9 である"),
        };
        let mut buf = Vec::with_capacity(len);
        // 先頭バイトが運ぶ値ビット数は 7 * len - 8 * (len - 1) = 8 - len
        // (len = 8 は 0 ビット、len = 9 は 0xFF で 0 ビット)
        let first_bits = 8usize.saturating_sub(len);
        let mask: u8 = if first_bits == 0 {
            0
        } else {
            (1u8 << first_bits) - 1
        };
        // len = 9 は 0xFF + 8 バイトで、先頭バイトに値ビットを持たない
        let first_value_bits = if len >= 9 {
            0
        } else {
            ((value >> (8 * (len - 1))) as u8) & mask
        };
        buf.push(prefix | first_value_bits);
        for i in (0..len - 1).rev() {
            buf.push((value >> (8 * i)) as u8);
        }
        buf
    }

    #[test]
    fn two_byte_zero_is_accepted() {
        // draft-ietf-moq-transport-21 §8.1: 最小バイト数でエンコードする必要はない。
        // 0 を 2 バイトで表した 0x8000 も有効である
        let buf = [0x80, 0x00];
        let (val, consumed) = varint::decode(&buf).expect("非最小形も受理される");
        assert_eq!(val, 0);
        assert_eq!(consumed, 2);
    }

    #[test]
    fn two_byte_37_is_accepted() {
        // 0x8025 = 37 (最小形は 0x25 の 1 バイト)
        let buf = [0x80, 0x25];
        let (val, consumed) = varint::decode(&buf).expect("非最小形も受理される");
        assert_eq!(val, 37);
        assert_eq!(consumed, 2);
    }

    #[test]
    fn each_length_decodes_37() {
        // 37 を 2〜9 バイトの各長でエンコードしたものを decode する
        for len in 2..=9usize {
            let buf = encode_with_len(37, len);
            assert_eq!(buf.len(), len, "len={len}");
            let (val, consumed) = varint::decode(&buf).expect("非最小形も受理される");
            assert_eq!(val, 37, "len={len}");
            assert_eq!(consumed, len, "len={len}");
        }
    }

    #[test]
    fn each_length_decodes_the_maximum_of_the_shorter_length() {
        // 各長の最大値を 1 バイト長い形でエンコードしても同じ値に decode できる
        let maxima = [
            (1usize, 0x7Fu64),
            (2, 0x3FFF),
            (3, 0x1F_FFFF),
            (4, 0x0FFF_FFFF),
            (5, 0x07_FFFF_FFFF),
            (6, 0x03FF_FFFF_FFFF),
            (7, 0x01_FFFF_FFFF_FFFF),
            (8, 0x00FF_FFFF_FFFF_FFFF),
        ];
        for (minimal_len, value) in maxima {
            let len = minimal_len + 1;
            let buf = encode_with_len(value, len);
            assert_eq!(buf.len(), len);
            let (val, consumed) = varint::decode(&buf).expect("非最小形も受理される");
            assert_eq!(val, value, "minimal_len={minimal_len}");
            assert_eq!(consumed, len, "minimal_len={minimal_len}");
        }
    }

    #[test]
    fn trailing_bytes_are_not_consumed() {
        // 非最小形の後ろに続くバイトは消費しない (呼び出し側が次のフィールドを読む)
        for len in 2..=9usize {
            let mut buf = encode_with_len(37, len);
            buf.push(0xAA);
            let (val, consumed) = varint::decode(&buf).expect("非最小形も受理される");
            assert_eq!(val, 37, "len={len}");
            assert_eq!(consumed, len, "len={len}");
        }
    }
}
