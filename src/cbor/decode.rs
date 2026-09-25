//! CBOR のデコードを行うモジュール

use crate::cbor::error::{DecodeError, DecodeErrorKind};
use crate::cbor::half::{f32_to_f64, half_to_f64};
use crate::cbor::value::Value;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::str;

/// [`Decoder`] が使うネストの最大深さのデフォルト値
///
/// RFC 8949 Section 10 の resource exhaustion 対策として深さを制限している。
pub const DEFAULT_MAX_DEPTH: usize = 128;

/// 単一の CBOR データ項目をデコードする
///
/// 入力全体が 1 個のデータ項目でなければならない。余分なバイトが残る場合は
/// [`DecodeErrorKind::TrailingData`] エラーになる。複数のデータ項目
/// (CBOR シーケンス) をデコードする場合は [`decode_all`] または [`Decoder`] を使う。
///
/// ```
/// use shiguredo_moqt::cbor::decode::decode;
/// use shiguredo_moqt::cbor::value::Value;
///
/// let value = decode(&[0x83, 0x01, 0x02, 0x03]).expect("デコードに失敗するはずがない (実装バグ)");
/// assert_eq!(
///     value,
///     Value::Array(vec![Value::Unsigned(1), Value::Unsigned(2), Value::Unsigned(3)])
/// );
/// ```
pub fn decode(input: &[u8]) -> Result<Value, DecodeError> {
    let mut decoder = Decoder::new(input);
    let value = decoder.decode()?;
    if decoder.position() != input.len() {
        return Err(DecodeError::new(
            DecodeErrorKind::TrailingData,
            decoder.position(),
        ));
    }
    Ok(value)
}

/// CBOR シーケンス (RFC 8742) をデコードする
///
/// 空の入力は空の `Vec` になる。途中のデータ項目が不正な場合は、
/// そこまでのデータ項目を捨ててエラーを返す。
///
/// ```
/// use shiguredo_moqt::cbor::decode::decode_all;
/// use shiguredo_moqt::cbor::value::Value;
///
/// let values = decode_all(&[0x01, 0x02]).expect("デコードに失敗するはずがない (実装バグ)");
/// assert_eq!(values, vec![Value::Unsigned(1), Value::Unsigned(2)]);
/// ```
pub fn decode_all(input: &[u8]) -> Result<Vec<Value>, DecodeError> {
    let mut decoder = Decoder::new(input);
    let mut values = Vec::new();
    while !decoder.is_finished() {
        values.push(decoder.decode()?);
    }
    Ok(values)
}

/// 入力バイト列を先頭から順にデコードするデコーダ
///
/// [`Decoder::decode`] を繰り返し呼ぶことで CBOR シーケンス (RFC 8742) を
/// 先頭から順にデコードできる。入力は一括して与えるため、途中までしか
/// 受信していないデータを継続してデコードすることはできない
/// (入力が途中で切れている場合は [`DecodeErrorKind::UnexpectedEnd`] になる)。
///
/// ```
/// use shiguredo_moqt::cbor::decode::Decoder;
/// use shiguredo_moqt::cbor::value::Value;
///
/// let mut decoder = Decoder::new(&[0x01, 0x02]);
/// assert_eq!(decoder.decode().expect("デコードに失敗するはずがない (実装バグ)"), Value::Unsigned(1));
/// assert_eq!(decoder.decode().expect("デコードに失敗するはずがない (実装バグ)"), Value::Unsigned(2));
/// assert!(decoder.is_finished());
/// ```
#[derive(Debug, Clone)]
pub struct Decoder<'a> {
    /// デコード対象の入力全体
    input: &'a [u8],
    /// 次に読む入力中の位置
    offset: usize,
    /// 許容するネストの最大深さ
    max_depth: usize,
}

impl<'a> Decoder<'a> {
    /// デコーダを作成する
    ///
    /// ネストの最大深さは [`DEFAULT_MAX_DEPTH`] になる。
    pub fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            offset: 0,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }

    /// ネストの最大深さを指定してデコーダを作成する
    ///
    /// トップレベルのデータ項目を深さ 0 とし、配列・マップ・タグの内容の深さが
    /// 1 増える。`max_depth` を超える深さのデータ項目をデコードしようとすると
    /// [`DecodeErrorKind::DepthLimitExceeded`] エラーになる。
    /// 極端に大きな値を指定すると、深くネストした入力でスタックが枯渇するおそれがある。
    pub fn with_max_depth(input: &'a [u8], max_depth: usize) -> Self {
        Self {
            input,
            offset: 0,
            max_depth,
        }
    }

    /// ネストの最大深さを変更する
    ///
    /// 極端に大きな値を指定すると、深くネストした入力でスタックが枯渇するおそれがある。
    pub fn set_max_depth(&mut self, max_depth: usize) {
        self.max_depth = max_depth;
    }

    /// ネストの最大深さを返す
    pub fn max_depth(&self) -> usize {
        self.max_depth
    }

    /// 次に読む入力中の位置 (0 始まり) を返す
    pub fn position(&self) -> usize {
        self.offset
    }

    /// まだ読んでいない入力の残りを返す
    pub fn remaining(&self) -> &'a [u8] {
        &self.input[self.offset..]
    }

    /// 入力を使い切ったかどうかを返す
    pub fn is_finished(&self) -> bool {
        self.offset == self.input.len()
    }

    /// 次の CBOR データ項目を 1 個デコードする
    ///
    /// 入力が途中で終了している場合は [`DecodeErrorKind::UnexpectedEnd`] エラーになる。
    /// エラーが発生した場合、`self` の位置はエラーを検出した位置で止まる。
    pub fn decode(&mut self) -> Result<Value, DecodeError> {
        self.decode_item(0)
    }

    /// データ項目を 1 個デコードする
    ///
    /// `depth` は現在のネストの深さで、トップレベルが 0 になる。
    fn decode_item(&mut self, depth: usize) -> Result<Value, DecodeError> {
        if depth > self.max_depth {
            return Err(DecodeError::new(
                DecodeErrorKind::DepthLimitExceeded,
                self.offset,
            ));
        }

        let position = self.offset;
        let initial = self.read_u8()?;
        let major = initial >> 5;
        let additional = initial & 0x1f;
        match major {
            0 => Ok(Value::Unsigned(self.read_argument(additional, position)?)),
            1 => Ok(Value::Negative(self.read_argument(additional, position)?)),
            2 => self.decode_byte_string(additional, position),
            3 => self.decode_text_string(additional, position),
            4 => self.decode_array(additional, position, depth),
            5 => self.decode_map(additional, position, depth),
            6 => {
                let tag = self.read_argument(additional, position)?;
                let content = self.decode_item(depth + 1)?;
                Ok(Value::Tag(tag, Box::new(content)))
            }
            7 => self.decode_major_7(additional, position),
            _ => unreachable!("major type は 3 ビットなので 0..=7 に収まる"),
        }
    }

    /// additional information から引数を読み取る
    ///
    /// 28..=30 は予約された値なのでエラー、31 は引数を持たないためエラーにする。
    fn read_argument(&mut self, additional: u8, position: usize) -> Result<u64, DecodeError> {
        match additional {
            0..=23 => Ok(u64::from(additional)),
            24 => Ok(u64::from(self.read_u8()?)),
            25 => self.read_uint(2),
            26 => self.read_uint(4),
            27 => self.read_uint(8),
            28..=30 => Err(DecodeError::new(
                DecodeErrorKind::ReservedAdditionalInformation,
                position,
            )),
            31 => Err(DecodeError::new(
                DecodeErrorKind::IndefiniteLengthNotAllowed,
                position,
            )),
            _ => unreachable!("additional information は 5 ビットなので 0..=31 に収まる"),
        }
    }

    /// ビッグエンディアンの `n` バイト符号なし整数を読み取る
    fn read_uint(&mut self, n: usize) -> Result<u64, DecodeError> {
        let bytes = self.read_bytes(n as u64)?;
        let mut value = 0u64;
        for byte in bytes {
            value = (value << 8) | u64::from(*byte);
        }
        Ok(value)
    }

    /// 1 バイト読み取る
    fn read_u8(&mut self) -> Result<u8, DecodeError> {
        let byte =
            self.input.get(self.offset).copied().ok_or_else(|| {
                DecodeError::new(DecodeErrorKind::UnexpectedEnd, self.input.len())
            })?;
        self.offset += 1;
        Ok(byte)
    }

    /// 次に読む 1 バイトを消費せずに参照する
    fn peek_u8(&self) -> Result<u8, DecodeError> {
        self.input
            .get(self.offset)
            .copied()
            .ok_or_else(|| DecodeError::new(DecodeErrorKind::UnexpectedEnd, self.input.len()))
    }

    /// `len` バイト読み取り、入力の一部を返す
    ///
    /// 入力が不足している場合は `len` の値にかかわらず入力を最後まで読まずに
    /// エラーを返すため、巨大な `len` でメモリを消費することはない。
    fn read_bytes(&mut self, len: u64) -> Result<&'a [u8], DecodeError> {
        let remaining = self.input.len() - self.offset;
        if len > remaining as u64 {
            return Err(DecodeError::new(
                DecodeErrorKind::UnexpectedEnd,
                self.input.len(),
            ));
        }
        let start = self.offset;
        let end = start + len as usize;
        self.offset = end;
        Ok(&self.input[start..end])
    }

    /// major type 2 (バイト文字列) をデコードする
    fn decode_byte_string(
        &mut self,
        additional: u8,
        position: usize,
    ) -> Result<Value, DecodeError> {
        if additional == 31 {
            // indefinite-length: definite-length のチャンクを連結する
            let mut bytes = Vec::new();
            loop {
                let chunk_position = self.offset;
                let initial = self.read_u8()?;
                if initial == 0xff {
                    break;
                }
                let chunk_major = initial >> 5;
                let chunk_additional = initial & 0x1f;
                if chunk_major != 2 || chunk_additional == 31 {
                    return Err(DecodeError::new(
                        DecodeErrorKind::InvalidIndefiniteStringChunk,
                        chunk_position,
                    ));
                }
                let len = self.read_argument(chunk_additional, chunk_position)?;
                bytes.extend_from_slice(self.read_bytes(len)?);
            }
            Ok(Value::ByteString(bytes))
        } else {
            let len = self.read_argument(additional, position)?;
            Ok(Value::ByteString(self.read_bytes(len)?.to_vec()))
        }
    }

    /// major type 3 (テキスト文字列) をデコードする
    fn decode_text_string(
        &mut self,
        additional: u8,
        position: usize,
    ) -> Result<Value, DecodeError> {
        if additional == 31 {
            // indefinite-length: definite-length のチャンクを連結する
            // RFC 8949 Section 3.2.3 のとおり、チャンクごとに UTF-8 として
            // 妥当でなければならず、コードポイントをチャンク間にまたがせられない
            let mut text = String::new();
            loop {
                let chunk_position = self.offset;
                let initial = self.read_u8()?;
                if initial == 0xff {
                    break;
                }
                let chunk_major = initial >> 5;
                let chunk_additional = initial & 0x1f;
                if chunk_major != 3 || chunk_additional == 31 {
                    return Err(DecodeError::new(
                        DecodeErrorKind::InvalidIndefiniteStringChunk,
                        chunk_position,
                    ));
                }
                let len = self.read_argument(chunk_additional, chunk_position)?;
                let content_position = self.offset;
                let bytes = self.read_bytes(len)?;
                let chunk = str::from_utf8(bytes).map_err(|error| {
                    DecodeError::new(
                        DecodeErrorKind::InvalidUtf8,
                        content_position + error.valid_up_to(),
                    )
                })?;
                text.push_str(chunk);
            }
            Ok(Value::TextString(text))
        } else {
            let len = self.read_argument(additional, position)?;
            let content_position = self.offset;
            let bytes = self.read_bytes(len)?;
            let text = str::from_utf8(bytes).map_err(|error| {
                DecodeError::new(
                    DecodeErrorKind::InvalidUtf8,
                    content_position + error.valid_up_to(),
                )
            })?;
            Ok(Value::TextString(String::from(text)))
        }
    }

    /// major type 4 (配列) をデコードする
    fn decode_array(
        &mut self,
        additional: u8,
        position: usize,
        depth: usize,
    ) -> Result<Value, DecodeError> {
        let mut items = Vec::new();
        if additional == 31 {
            loop {
                if self.peek_u8()? == 0xff {
                    self.offset += 1;
                    break;
                }
                items.push(self.decode_item(depth + 1)?);
            }
        } else {
            let count = self.read_argument(additional, position)?;
            // count は最大 2^64-1 になりうるが、各要素は必ず 1 バイト以上を消費するため
            // 入力が尽きた時点で UnexpectedEnd で終わる
            for _ in 0..count {
                items.push(self.decode_item(depth + 1)?);
            }
        }
        Ok(Value::Array(items))
    }

    /// major type 5 (マップ) をデコードする
    fn decode_map(
        &mut self,
        additional: u8,
        position: usize,
        depth: usize,
    ) -> Result<Value, DecodeError> {
        let mut pairs = Vec::new();
        if additional == 31 {
            loop {
                if self.peek_u8()? == 0xff {
                    self.offset += 1;
                    break;
                }
                let key = self.decode_item(depth + 1)?;
                if self.peek_u8()? == 0xff {
                    // キーの直後に break があり、値が存在しない
                    return Err(DecodeError::new(
                        DecodeErrorKind::MissingMapValue,
                        self.offset,
                    ));
                }
                let value = self.decode_item(depth + 1)?;
                pairs.push((key, value));
            }
        } else {
            let count = self.read_argument(additional, position)?;
            for _ in 0..count {
                let key = self.decode_item(depth + 1)?;
                let value = self.decode_item(depth + 1)?;
                pairs.push((key, value));
            }
        }
        Ok(Value::Map(pairs))
    }

    /// major type 7 (浮動小数点値と simple value) をデコードする
    fn decode_major_7(&mut self, additional: u8, position: usize) -> Result<Value, DecodeError> {
        match additional {
            0..=19 => Ok(Value::Simple(additional)),
            20 => Ok(Value::Bool(false)),
            21 => Ok(Value::Bool(true)),
            22 => Ok(Value::Null),
            23 => Ok(Value::Undefined),
            24 => {
                let value = self.read_u8()?;
                if value < 32 {
                    // RFC 8949 Section 3.3 のとおり、この 2 バイト表現は well-formed ではない
                    Err(DecodeError::new(
                        DecodeErrorKind::InvalidSimpleValue,
                        position,
                    ))
                } else {
                    Ok(Value::Simple(value))
                }
            }
            25 => {
                let bits = self.read_uint(2)? as u16;
                Ok(Value::Float(half_to_f64(bits)))
            }
            26 => {
                let bits = self.read_uint(4)? as u32;
                Ok(Value::Float(f32_to_f64(bits)))
            }
            27 => {
                let bits = self.read_uint(8)?;
                Ok(Value::Float(f64::from_bits(bits)))
            }
            28..=30 => Err(DecodeError::new(
                DecodeErrorKind::ReservedAdditionalInformation,
                position,
            )),
            31 => Err(DecodeError::new(DecodeErrorKind::UnexpectedBreak, position)),
            _ => unreachable!("additional information は 5 ビットなので 0..=31 に収まる"),
        }
    }
}
