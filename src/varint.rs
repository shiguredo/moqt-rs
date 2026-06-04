//! draft-ietf-moq-transport-21 §8.1 (Variable-Length Integers) に準拠した Variable-Length Integer (vi64)
//!
//! Leading-1-bits 方式でエンコード長を決定する:
//!
//! | Leading Bits | バイト長 | 使用可能ビット | 範囲 |
//! |---|---|---|---|
//! | 0 | 1 | 7 | 0-127 |
//! | 10 | 2 | 14 | 0-16,383 |
//! | 110 | 3 | 21 | 0-2,097,151 |
//! | 1110 | 4 | 28 | 0-268,435,455 |
//! | 11110 | 5 | 35 | 0-34,359,738,367 |
//! | 111110 | 6 | 42 | 0-4,398,046,511,103 |
//! | 1111110 | 7 | 49 | 0-562,949,953,421,311 |
//! | 11111110 | 8 | 56 | 0-72,057,594,037,927,935 |
//! | 11111111 | 9 | 64 | 0-18,446,744,073,709,551,615 |
//!
//! draft-ietf-moq-transport-21 Appendix A.3 (Since draft-ietf-moq-transport-17) #1595: 非最小エンコーディングを許容 (decode 側はエラーにしない)。
//! encode 側は引き続き最小バイト数でエンコードする。
//!
//! 注意: この仕様はドラフト段階であり、将来変更される可能性がある。
use crate::error::MessageError;
use alloc::vec::Vec;

// ─── 各段階の閾値 ──────────────────────────────────────────────

/// 1 バイトの最大値 (2^7 - 1)
const THRESHOLD_1: u64 = 127;
/// 2 バイトの最大値 (2^14 - 1)
const THRESHOLD_2: u64 = 16_383;
/// 3 バイトの最大値 (2^21 - 1)
const THRESHOLD_3: u64 = 2_097_151;
/// 4 バイトの最大値 (2^28 - 1)
const THRESHOLD_4: u64 = 268_435_455;
/// 5 バイトの最大値 (2^35 - 1)
const THRESHOLD_5: u64 = 34_359_738_367;
/// 6 バイトの最大値 (2^42 - 1)
const THRESHOLD_6: u64 = 4_398_046_511_103;
/// 7 バイトの最大値 (2^49 - 1) — draft-ietf-moq-transport-21 Appendix A.3 (Since draft-ietf-moq-transport-17) #1595 で追加
const THRESHOLD_7: u64 = 562_949_953_421_311;
/// 8 バイトの最大値 (2^56 - 1)
const THRESHOLD_8: u64 = 72_057_594_037_927_935;

/// `val` を vi64 としてエンコードし `buf` に追記する
pub fn encode(val: u64, buf: &mut Vec<u8>) {
    if val <= THRESHOLD_1 {
        // 0xxxxxxx (1 バイト)
        buf.push(val as u8);
    } else if val <= THRESHOLD_2 {
        // 10xxxxxx xxxxxxxx (2 バイト)
        buf.push(0x80 | (val >> 8) as u8);
        buf.push(val as u8);
    } else if val <= THRESHOLD_3 {
        // 110xxxxx xxxxxxxx xxxxxxxx (3 バイト)
        buf.push(0xC0 | (val >> 16) as u8);
        buf.push((val >> 8) as u8);
        buf.push(val as u8);
    } else if val <= THRESHOLD_4 {
        // 1110xxxx xxxxxxxx xxxxxxxx xxxxxxxx (4 バイト)
        buf.push(0xE0 | (val >> 24) as u8);
        buf.push((val >> 16) as u8);
        buf.push((val >> 8) as u8);
        buf.push(val as u8);
    } else if val <= THRESHOLD_5 {
        // 11110xxx xxxxxxxx xxxxxxxx xxxxxxxx xxxxxxxx (5 バイト)
        buf.push(0xF0 | (val >> 32) as u8);
        buf.push((val >> 24) as u8);
        buf.push((val >> 16) as u8);
        buf.push((val >> 8) as u8);
        buf.push(val as u8);
    } else if val <= THRESHOLD_6 {
        // 111110xx xxxxxxxx xxxxxxxx xxxxxxxx xxxxxxxx xxxxxxxx (6 バイト)
        buf.push(0xF8 | (val >> 40) as u8);
        buf.push((val >> 32) as u8);
        buf.push((val >> 24) as u8);
        buf.push((val >> 16) as u8);
        buf.push((val >> 8) as u8);
        buf.push(val as u8);
    } else if val <= THRESHOLD_7 {
        // 1111110x xxxxxxxx xxxxxxxx xxxxxxxx xxxxxxxx xxxxxxxx xxxxxxxx (7 バイト, draft-ietf-moq-transport-21 Appendix A.3 (Since draft-ietf-moq-transport-17) #1595)
        buf.push(0xFC | (val >> 48) as u8);
        buf.push((val >> 40) as u8);
        buf.push((val >> 32) as u8);
        buf.push((val >> 24) as u8);
        buf.push((val >> 16) as u8);
        buf.push((val >> 8) as u8);
        buf.push(val as u8);
    } else if val <= THRESHOLD_8 {
        // 11111110 xxxxxxxx xxxxxxxx xxxxxxxx xxxxxxxx xxxxxxxx xxxxxxxx xxxxxxxx (8 バイト)
        buf.push(0xFE);
        buf.push((val >> 48) as u8);
        buf.push((val >> 40) as u8);
        buf.push((val >> 32) as u8);
        buf.push((val >> 24) as u8);
        buf.push((val >> 16) as u8);
        buf.push((val >> 8) as u8);
        buf.push(val as u8);
    } else {
        // 11111111 xxxxxxxx xxxxxxxx xxxxxxxx xxxxxxxx xxxxxxxx xxxxxxxx xxxxxxxx xxxxxxxx (9 バイト)
        buf.push(0xFF);
        buf.push((val >> 56) as u8);
        buf.push((val >> 48) as u8);
        buf.push((val >> 40) as u8);
        buf.push((val >> 32) as u8);
        buf.push((val >> 24) as u8);
        buf.push((val >> 16) as u8);
        buf.push((val >> 8) as u8);
        buf.push(val as u8);
    }
}

/// `buf` の先頭から vi64 をデコードし `(値, 消費バイト数)` を返す
///
/// draft-ietf-moq-transport-21 Appendix A.3 (Since draft-ietf-moq-transport-17) #1595: 非最小エンコーディングを許容する。
pub fn decode(buf: &[u8]) -> Result<(u64, usize), MessageError> {
    if buf.is_empty() {
        return Err(MessageError::UnexpectedEof);
    }
    let first = buf[0];

    // 先頭バイトの leading-1-bits をカウントしてエンコード長を決定する
    let leading_ones = first.leading_ones() as usize;

    match leading_ones {
        0 => {
            // 0xxxxxxx → 1 バイト (7 bit)
            Ok((u64::from(first & 0x7F), 1))
        }
        1 => {
            // 10xxxxxx → 2 バイト (14 bit)
            if buf.len() < 2 {
                return Err(MessageError::UnexpectedEof);
            }
            let val = (u64::from(first & 0x3F) << 8) | u64::from(buf[1]);
            Ok((val, 2))
        }
        2 => {
            // 110xxxxx → 3 バイト (21 bit)
            if buf.len() < 3 {
                return Err(MessageError::UnexpectedEof);
            }
            let val =
                (u64::from(first & 0x1F) << 16) | (u64::from(buf[1]) << 8) | u64::from(buf[2]);
            Ok((val, 3))
        }
        3 => {
            // 1110xxxx → 4 バイト (28 bit)
            if buf.len() < 4 {
                return Err(MessageError::UnexpectedEof);
            }
            let val = (u64::from(first & 0x0F) << 24)
                | (u64::from(buf[1]) << 16)
                | (u64::from(buf[2]) << 8)
                | u64::from(buf[3]);
            Ok((val, 4))
        }
        4 => {
            // 11110xxx → 5 バイト (35 bit)
            if buf.len() < 5 {
                return Err(MessageError::UnexpectedEof);
            }
            let val = (u64::from(first & 0x07) << 32)
                | (u64::from(buf[1]) << 24)
                | (u64::from(buf[2]) << 16)
                | (u64::from(buf[3]) << 8)
                | u64::from(buf[4]);
            Ok((val, 5))
        }
        5 => {
            // 111110xx → 6 バイト (42 bit)
            if buf.len() < 6 {
                return Err(MessageError::UnexpectedEof);
            }
            let val = (u64::from(first & 0x03) << 40)
                | (u64::from(buf[1]) << 32)
                | (u64::from(buf[2]) << 24)
                | (u64::from(buf[3]) << 16)
                | (u64::from(buf[4]) << 8)
                | u64::from(buf[5]);
            Ok((val, 6))
        }
        6 => {
            // leading_ones=6: first = 1111110x → 7 バイト (49 bit, draft-ietf-moq-transport-21 Appendix A.3 (Since draft-ietf-moq-transport-17) #1595)
            // 非最小エンコーディング許容のため先頭バイトのプレフィックス不要ビットも受理する
            if buf.len() < 7 {
                return Err(MessageError::UnexpectedEof);
            }
            let val = (u64::from(first & 0x01) << 48)
                | (u64::from(buf[1]) << 40)
                | (u64::from(buf[2]) << 32)
                | (u64::from(buf[3]) << 24)
                | (u64::from(buf[4]) << 16)
                | (u64::from(buf[5]) << 8)
                | u64::from(buf[6]);
            Ok((val, 7))
        }
        7 => {
            // leading_ones=7: first = 11111110 (0xFE) → 8 バイト (56 bit)
            if buf.len() < 8 {
                return Err(MessageError::UnexpectedEof);
            }
            let val = (u64::from(buf[1]) << 48)
                | (u64::from(buf[2]) << 40)
                | (u64::from(buf[3]) << 32)
                | (u64::from(buf[4]) << 24)
                | (u64::from(buf[5]) << 16)
                | (u64::from(buf[6]) << 8)
                | u64::from(buf[7]);
            Ok((val, 8))
        }
        8 => {
            // 11111111 → 9 バイト (64 bit)
            if buf.len() < 9 {
                return Err(MessageError::UnexpectedEof);
            }
            let val = (u64::from(buf[1]) << 56)
                | (u64::from(buf[2]) << 48)
                | (u64::from(buf[3]) << 40)
                | (u64::from(buf[4]) << 32)
                | (u64::from(buf[5]) << 24)
                | (u64::from(buf[6]) << 16)
                | (u64::from(buf[7]) << 8)
                | u64::from(buf[8]);
            Ok((val, 9))
        }
        _ => unreachable!("leading_ones of a u8 is always 0..=8"),
    }
}

/// 入力由来の長さ (u64) を、残りバッファ長と u64 空間で比較してから usize に変換する。
///
/// 32bit 環境 (usize == u32) では `len as usize` で u32::MAX を超える長さが silent に
/// 切り捨てられ、後続のバッファ長チェックをすり抜けて誤パースやスライスアクセスの
/// パニックを引き起こす。そのため必ず u64 空間で `len <= remaining` を検証してから変換する。
///
/// `remaining as u64` は widening なので情報欠落しない。チェック通過後は
/// `len <= remaining <= usize::MAX` のため `len as usize` も切り捨てなく安全である。
pub fn checked_len(len: u64, remaining: usize) -> Result<usize, MessageError> {
    if len > remaining as u64 {
        return Err(MessageError::UnexpectedEof);
    }
    Ok(len as usize)
}

/// `val` を vi64 としてエンコードしたとき何バイトになるかを返す
pub fn encoded_len(val: u64) -> usize {
    if val <= THRESHOLD_1 {
        1
    } else if val <= THRESHOLD_2 {
        2
    } else if val <= THRESHOLD_3 {
        3
    } else if val <= THRESHOLD_4 {
        4
    } else if val <= THRESHOLD_5 {
        5
    } else if val <= THRESHOLD_6 {
        6
    } else if val <= THRESHOLD_7 {
        7
    } else if val <= THRESHOLD_8 {
        8
    } else {
        9
    }
}
