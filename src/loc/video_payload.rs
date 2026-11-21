//! LOC の Video Payload Format における NAL Unit の境界処理
//!
//! draft-ietf-moq-loc-05 §2.1.3 (Length Prefixes in Payload) / §2.1.4 (Start Code Prefixes in
//! Payload) は、LOC payload 中の NAL Unit を 4 バイト length prefix または 4 バイト
//! start code で区切ると定める。4 バイト未満の length prefix は Video Config
//! (`lengthSizeMinusOne`) で指定でき、3 バイト start code は length prefix と Video Config を
//! 使わない track で使える。
//!
//! このモジュールはコーデック非依存のコンテナ層として、payload の境界だけを扱う。H.264 /
//! H.265 の parameter set の内容や、payload 内の parameter set の有無は解釈しない。
//! 節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。

use alloc::vec::Vec;

/// NAL Unit の区切り方
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NalUnitKind {
    /// ネットワークバイトオーダーの length prefix
    LengthPrefix,
    /// 3 または 4 バイトの start code
    StartCode,
}

/// NAL Unit の区切り形式
///
/// 不正な幅を持つ値を作れないよう、フィールドは非公開でコンストラクタだけを公開する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NalUnitFormat {
    kind: NalUnitKind,
    width: u8,
}

impl NalUnitFormat {
    /// length prefix 形式を作る
    ///
    /// `width` は 1〜4 バイト。4 バイト未満は Video Config の `lengthSizeMinusOne` に対応する
    /// (draft-ietf-moq-loc-05 §2.1.3)。
    pub fn length_prefix(width: u8) -> Result<Self, VideoPayloadError> {
        if !(1..=4).contains(&width) {
            return Err(VideoPayloadError::InvalidLengthPrefixWidth(width));
        }
        Ok(Self {
            kind: NalUnitKind::LengthPrefix,
            width,
        })
    }

    /// start code 形式を作る
    ///
    /// `width` は 3 または 4 バイト。3 バイト start code は length prefix と Video Config を
    /// 使わない track だけで使える (draft-ietf-moq-loc-05 §2.1.4)。
    pub fn start_code(width: u8) -> Result<Self, VideoPayloadError> {
        if width != 3 && width != 4 {
            return Err(VideoPayloadError::InvalidStartCodeWidth(width));
        }
        Ok(Self {
            kind: NalUnitKind::StartCode,
            width,
        })
    }

    /// Video Config の `lengthSizeMinusOne` (0〜3) から length prefix 形式を作る
    ///
    /// `lengthSizeMinusOne = 0` は 1 バイト、`3` は 4 バイトの length prefix を表す。
    pub fn from_length_size_minus_one(value: u8) -> Result<Self, VideoPayloadError> {
        if value > 3 {
            return Err(VideoPayloadError::InvalidLengthSizeMinusOne(value));
        }
        Self::length_prefix(value + 1)
    }

    /// 区切り方
    pub fn kind(self) -> NalUnitKind {
        self.kind
    }

    /// 区切りのバイト幅
    pub fn width(self) -> u8 {
        self.width
    }
}

/// LOC payload の NAL Unit 境界処理で発生するエラー
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoPayloadError {
    /// length prefix の幅が 1〜4 バイトでない
    InvalidLengthPrefixWidth(u8),
    /// start code の幅が 3 または 4 バイトでない
    InvalidStartCodeWidth(u8),
    /// `lengthSizeMinusOne` が 0〜3 でない
    InvalidLengthSizeMinusOne(u8),
    /// length prefix を読む途中で payload が終わった
    TruncatedLengthPrefix,
    /// 長さ 0 の NAL Unit
    ZeroLengthNalUnit,
    /// length prefix が payload の残りより大きい
    LengthExceedsPayload,
    /// start code が期待する位置に無い
    MissingStartCode,
    /// 連続する start code の間に NAL Unit が無い
    EmptyNalUnit,
    /// 1〜2 バイトの length prefix で値 1 が現れた
    ///
    /// draft-ietf-moq-loc-05 §2.1.3 は「長さ 1 は start code と解釈する」とするが、
    /// 1〜2 バイトの prefix は 3 / 4 バイト start code を表現できない。
    AmbiguousLengthOne(u8),
    /// NAL Unit の長さが length prefix の幅で表現できない
    LengthTooLargeForPrefix,
}

impl core::fmt::Display for VideoPayloadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidLengthPrefixWidth(width) => {
                write!(f, "length prefix width must be 1..=4, got {width}")
            }
            Self::InvalidStartCodeWidth(width) => {
                write!(f, "start code width must be 3 or 4, got {width}")
            }
            Self::InvalidLengthSizeMinusOne(value) => {
                write!(f, "lengthSizeMinusOne must be 0..=3, got {value}")
            }
            Self::TruncatedLengthPrefix => write!(f, "truncated length prefix"),
            Self::ZeroLengthNalUnit => write!(f, "NAL unit length is zero"),
            Self::LengthExceedsPayload => write!(f, "NAL unit length exceeds payload"),
            Self::MissingStartCode => write!(f, "missing NAL unit start code"),
            Self::EmptyNalUnit => write!(f, "empty NAL unit"),
            Self::AmbiguousLengthOne(width) => {
                write!(
                    f,
                    "length prefix value 1 with {width}-byte prefix is ambiguous"
                )
            }
            Self::LengthTooLargeForPrefix => {
                write!(
                    f,
                    "NAL unit length cannot be represented by the prefix width"
                )
            }
        }
    }
}

impl core::error::Error for VideoPayloadError {}

/// 3 バイト start code
const START_CODE_3: [u8; 3] = [0x00, 0x00, 0x01];
/// 4 バイト start code
const START_CODE_4: [u8; 4] = [0x00, 0x00, 0x00, 0x01];

/// 幅に対応する start code を返す
fn start_code_bytes(width: u8) -> &'static [u8] {
    if width == 3 {
        &START_CODE_3
    } else {
        &START_CODE_4
    }
}

/// `from` 以降で最初に現れる start code の位置を返す
fn find_start_code(payload: &[u8], from: usize, pattern: &[u8]) -> Option<usize> {
    let tail = payload.get(from..)?;
    tail.windows(pattern.len())
        .position(|window| window == pattern)
        .map(|offset| from + offset)
}

/// LOC payload を NAL Unit に分割する増分デコーダ
///
/// `next_unit` は payload の借用スライスを返す。入力は 1 つの Object payload 全体であり、
/// 複数の NAL Unit を含み得る。payload が空の場合は `Ok(None)` を返す。
#[derive(Debug, Clone)]
pub struct NalUnitDecoder<'a> {
    payload: &'a [u8],
    format: NalUnitFormat,
    pos: usize,
    done: bool,
}

impl<'a> NalUnitDecoder<'a> {
    /// payload と区切り形式からデコーダを作る
    pub fn new(payload: &'a [u8], format: NalUnitFormat) -> Self {
        Self {
            payload,
            format,
            pos: 0,
            done: false,
        }
    }

    /// 次の NAL Unit を返す
    ///
    /// payload の終端に達したら `Ok(None)` を返す。途中で不正な長さや start code を検出した
    /// 場合はエラーを返す。エラー後の状態は保証しないため、呼び出し側はエラーで分割を
    /// 中断すること。
    pub fn next_unit(&mut self) -> Result<Option<&'a [u8]>, VideoPayloadError> {
        if self.done || self.pos >= self.payload.len() {
            return Ok(None);
        }
        match self.format.kind {
            NalUnitKind::LengthPrefix => self.next_length_prefix_unit(),
            NalUnitKind::StartCode => self.next_start_code_unit(),
        }
    }

    /// まだ読み取っていない payload の残り
    pub fn remaining(&self) -> &'a [u8] {
        self.payload.get(self.pos..).unwrap_or(&[])
    }

    fn next_length_prefix_unit(&mut self) -> Result<Option<&'a [u8]>, VideoPayloadError> {
        let width = usize::from(self.format.width);
        if self.payload.len() - self.pos < width {
            return Err(VideoPayloadError::TruncatedLengthPrefix);
        }
        let mut length = 0u64;
        for &byte in &self.payload[self.pos..self.pos + width] {
            length = (length << 8) | u64::from(byte);
        }
        if length == 1 {
            // draft-ietf-moq-loc-05 §2.1.3: 長さ 1 は start code と解釈する。
            let start_code_width = match self.format.width {
                3 | 4 => self.format.width,
                width => return Err(VideoPayloadError::AmbiguousLengthOne(width)),
            };
            self.format = NalUnitFormat {
                kind: NalUnitKind::StartCode,
                width: start_code_width,
            };
            return self.next_start_code_unit();
        }
        if length == 0 {
            return Err(VideoPayloadError::ZeroLengthNalUnit);
        }
        let length =
            usize::try_from(length).map_err(|_| VideoPayloadError::LengthExceedsPayload)?;
        // 長さが payload の残りを超える場合は prefix の位置を進めない
        if length > self.payload.len() - self.pos - width {
            return Err(VideoPayloadError::LengthExceedsPayload);
        }
        self.pos += width;
        let unit = &self.payload[self.pos..self.pos + length];
        self.pos += length;
        Ok(Some(unit))
    }

    fn next_start_code_unit(&mut self) -> Result<Option<&'a [u8]>, VideoPayloadError> {
        let pattern = start_code_bytes(self.format.width);
        if self.payload.len() - self.pos < pattern.len() {
            return Err(VideoPayloadError::MissingStartCode);
        }
        if &self.payload[self.pos..self.pos + pattern.len()] != pattern {
            return Err(VideoPayloadError::MissingStartCode);
        }
        let nal_start = self.pos + pattern.len();
        match find_start_code(self.payload, nal_start, pattern) {
            Some(next) => {
                if next == nal_start {
                    return Err(VideoPayloadError::EmptyNalUnit);
                }
                let unit = &self.payload[nal_start..next];
                self.pos = next;
                Ok(Some(unit))
            }
            None => {
                self.pos = self.payload.len();
                self.done = true;
                if nal_start == self.payload.len() {
                    return Err(VideoPayloadError::EmptyNalUnit);
                }
                Ok(Some(&self.payload[nal_start..]))
            }
        }
    }
}

/// 1 個の NAL Unit を区切り形式に従って `out` へ追記する
///
/// length prefix 形式では長さをネットワークバイトオーダーで書き、start code 形式では
/// start code を書いてから NAL Unit を書く。空の NAL Unit と、prefix の幅で表現できない
/// 長さはエラーにする。
pub fn encode_nal_unit(
    unit: &[u8],
    format: NalUnitFormat,
    out: &mut Vec<u8>,
) -> Result<(), VideoPayloadError> {
    if unit.is_empty() {
        return Err(VideoPayloadError::ZeroLengthNalUnit);
    }
    match format.kind {
        NalUnitKind::LengthPrefix => {
            let width = format.width;
            // draft-ietf-moq-loc-05 §2.1.3: 長さ 1 は start code と解釈されるため、
            // 1 バイトの NAL Unit を length prefix で表すと復号時に start code と衝突する。
            if unit.len() == 1 {
                return Err(VideoPayloadError::AmbiguousLengthOne(width));
            }
            let max = if width == 4 {
                u64::from(u32::MAX)
            } else {
                (1u64 << (8 * u32::from(width))) - 1
            };
            if unit.len() as u64 > max {
                return Err(VideoPayloadError::LengthTooLargeForPrefix);
            }
            let bytes = (unit.len() as u64).to_be_bytes();
            out.extend_from_slice(&bytes[8 - usize::from(width)..]);
        }
        NalUnitKind::StartCode => out.extend_from_slice(start_code_bytes(format.width)),
    }
    out.extend_from_slice(unit);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn collect(payload: &[u8], format: NalUnitFormat) -> Result<Vec<&[u8]>, VideoPayloadError> {
        let mut decoder = NalUnitDecoder::new(payload, format);
        let mut units = Vec::new();
        while let Some(unit) = decoder.next_unit()? {
            units.push(unit);
        }
        Ok(units)
    }

    #[test]
    fn length_prefix_roundtrip() {
        let format = NalUnitFormat::length_prefix(4).expect("4 バイト prefix は有効");
        let mut payload = Vec::new();
        encode_nal_unit(&[0x65, 0x11], format, &mut payload).expect("encode できること");
        encode_nal_unit(&[0x41, 0x01], format, &mut payload).expect("encode できること");
        let units = collect(&payload, format).expect("decode できること");
        assert_eq!(units, vec![&[0x65, 0x11][..], &[0x41, 0x01][..]]);
    }

    #[test]
    fn three_byte_start_code_roundtrip() {
        let format = NalUnitFormat::start_code(3).expect("3 バイト start code は有効");
        let mut payload = Vec::new();
        encode_nal_unit(&[0x67, 0x42], format, &mut payload).expect("encode できること");
        encode_nal_unit(&[0x68], format, &mut payload).expect("encode できること");
        let units = collect(&payload, format).expect("decode できること");
        assert_eq!(units, vec![&[0x67, 0x42][..], &[0x68][..]]);
    }

    #[test]
    fn four_byte_start_code_roundtrip() {
        let format = NalUnitFormat::start_code(4).expect("4 バイト start code は有効");
        let mut payload = Vec::new();
        encode_nal_unit(&[0x67], format, &mut payload).expect("encode できること");
        encode_nal_unit(&[0x68, 0xEE], format, &mut payload).expect("encode できること");
        let units = collect(&payload, format).expect("decode できること");
        assert_eq!(units, vec![&[0x67][..], &[0x68, 0xEE][..]]);
    }

    #[test]
    fn four_byte_length_one_switches_to_start_code() {
        // 00 00 00 01 は 4 バイト length prefix の値 1 であり、同時に 4 バイト start code でもある
        let payload = [0x00, 0x00, 0x00, 0x01, 0x65, 0x00, 0x00, 0x00, 0x01, 0x41];
        let format = NalUnitFormat::length_prefix(4).expect("4 バイト prefix は有効");
        let units = collect(&payload, format).expect("start code として解釈できること");
        assert_eq!(units, vec![&[0x65][..], &[0x41][..]]);
    }

    #[test]
    fn three_byte_length_one_switches_to_start_code() {
        let payload = [0x00, 0x00, 0x01, 0x65, 0x00, 0x00, 0x01, 0x41];
        let format = NalUnitFormat::length_prefix(3).expect("3 バイト prefix は有効");
        let units = collect(&payload, format).expect("start code として解釈できること");
        assert_eq!(units, vec![&[0x65][..], &[0x41][..]]);
    }

    #[test]
    fn truncated_length_prefix_is_rejected() {
        let format = NalUnitFormat::length_prefix(4).expect("4 バイト prefix は有効");
        let mut decoder = NalUnitDecoder::new(&[0x00, 0x00], format);
        assert_eq!(
            decoder.next_unit(),
            Err(VideoPayloadError::TruncatedLengthPrefix)
        );
    }

    #[test]
    fn zero_length_nal_unit_is_rejected() {
        let format = NalUnitFormat::length_prefix(4).expect("4 バイト prefix は有効");
        let mut decoder = NalUnitDecoder::new(&[0x00, 0x00, 0x00, 0x00], format);
        assert_eq!(
            decoder.next_unit(),
            Err(VideoPayloadError::ZeroLengthNalUnit)
        );
    }

    #[test]
    fn length_beyond_payload_is_rejected() {
        let format = NalUnitFormat::length_prefix(4).expect("4 バイト prefix は有効");
        let mut decoder = NalUnitDecoder::new(&[0x00, 0x00, 0x00, 0x05, 0x65], format);
        assert_eq!(
            decoder.next_unit(),
            Err(VideoPayloadError::LengthExceedsPayload)
        );
    }

    #[test]
    fn missing_start_code_is_rejected() {
        let format = NalUnitFormat::start_code(4).expect("4 バイト start code は有効");
        let mut decoder = NalUnitDecoder::new(&[0x65, 0x41], format);
        assert_eq!(
            decoder.next_unit(),
            Err(VideoPayloadError::MissingStartCode)
        );
    }

    #[test]
    fn empty_nal_unit_between_start_codes_is_rejected() {
        let format = NalUnitFormat::start_code(4).expect("4 バイト start code は有効");
        let payload = [0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01];
        let mut decoder = NalUnitDecoder::new(&payload, format);
        assert_eq!(decoder.next_unit(), Err(VideoPayloadError::EmptyNalUnit));
    }

    #[test]
    fn one_byte_nal_with_length_prefix_is_rejected() {
        let format = NalUnitFormat::length_prefix(4).expect("4 バイト prefix は有効");
        let mut out = Vec::new();
        assert_eq!(
            encode_nal_unit(&[0x65], format, &mut out),
            Err(VideoPayloadError::AmbiguousLengthOne(4))
        );
    }

    #[test]
    fn length_too_large_for_one_byte_prefix_is_rejected() {
        let format = NalUnitFormat::length_prefix(1).expect("1 バイト prefix は有効");
        let mut out = Vec::new();
        let unit = [0u8; 256];
        assert_eq!(
            encode_nal_unit(&unit, format, &mut out),
            Err(VideoPayloadError::LengthTooLargeForPrefix)
        );
    }

    #[test]
    fn length_one_with_one_byte_prefix_is_ambiguous() {
        let format = NalUnitFormat::length_prefix(1).expect("1 バイト prefix は有効");
        let mut decoder = NalUnitDecoder::new(&[0x01, 0x65], format);
        assert_eq!(
            decoder.next_unit(),
            Err(VideoPayloadError::AmbiguousLengthOne(1))
        );
    }

    #[test]
    fn invalid_widths_are_rejected() {
        assert_eq!(
            NalUnitFormat::length_prefix(0),
            Err(VideoPayloadError::InvalidLengthPrefixWidth(0))
        );
        assert_eq!(
            NalUnitFormat::length_prefix(5),
            Err(VideoPayloadError::InvalidLengthPrefixWidth(5))
        );
        assert_eq!(
            NalUnitFormat::start_code(2),
            Err(VideoPayloadError::InvalidStartCodeWidth(2))
        );
        assert_eq!(
            NalUnitFormat::from_length_size_minus_one(4),
            Err(VideoPayloadError::InvalidLengthSizeMinusOne(4))
        );
    }
}
