//! バッファ付きインクリメンタルデコーダー (sans I/O)
//!
//! MoQT の制御メッセージは type(varint) + length(u16) + payload で構成される。
//! QUIC/WebTransport のフレーム境界とメッセージ境界は一致しないため、
//! 受信バッファに蓄積しながらインクリメンタルにデコードする必要がある。
//!
//! このモジュールは I/O を持たず、バッファ管理とデコード試行のみを提供する。
//! ストリームからのデータ受信は呼び出し側が行う。
use crate::error::MessageError;
use crate::message::ControlMessage;
use crate::varint;
use alloc::vec::Vec;

/// バッファ付きメッセージデコーダー
///
/// # 使い方 (検証対象外の疑似コード)
///
/// ```text
/// let mut decoder = MessageDecoder::new();
/// loop {
///     if let Some(msg) = decoder.try_decode_message()? {
///         return Ok(msg);
///     }
///     match stream.receive().await? {
///         Some(data) => decoder.push(&data),
///         None => return Err("stream closed"),
///     }
/// }
/// ```
pub struct MessageDecoder {
    buf: Vec<u8>,
}

impl MessageDecoder {
    /// 新しいデコーダーを作成する
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// バッファにデータを追加する
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// バッファから制御メッセージのデコードを試みる
    ///
    /// - `Ok(Some(msg))` — デコード成功、バッファから消費済み
    /// - `Ok(None)` — データ不足、追加データが必要
    /// - `Err(e)` — デコードエラー
    pub fn try_decode_message(&mut self) -> Result<Option<ControlMessage>, MessageError> {
        if self.buf.is_empty() {
            return Ok(None);
        }
        // 外側フレーム (Type + Length + payload) が揃っているかを先に判定する。
        // 揃っているのに ControlMessage::decode が UnexpectedEof を返す場合は、
        // ペイロード長は確定済みで追加データでは回復し得ない。これを
        // 「データ不足 (Ok(None))」と扱うとバッファが drain されず恒久停止するため、
        // エラーとして呼び出し側に返す。
        let (_, type_len) = match varint::decode(&self.buf) {
            Ok(v) => v,
            Err(MessageError::UnexpectedEof) => return Ok(None),
            Err(e) => return Err(e),
        };
        if self.buf.len() < type_len + 2 {
            return Ok(None);
        }
        let payload_len = ((self.buf[type_len] as usize) << 8) | self.buf[type_len + 1] as usize;
        if self.buf.len() < type_len + 2 + payload_len {
            return Ok(None);
        }
        match ControlMessage::decode(&self.buf) {
            Ok((msg, consumed)) => {
                self.buf.drain(..consumed);
                Ok(Some(msg))
            }
            Err(e) => Err(e),
        }
    }

    /// バッファから varint のデコードを試みる
    ///
    /// ストリームタイプの読み取りに使用する。
    /// draft-ietf-moq-transport-21 §6.4.1 (Unidirectional Streams):
    /// 全ての MoQT 単方向ストリームは先頭にストリームタイプ varint を持つ。
    ///
    /// - `Ok(Some(val))` — デコード成功、バッファから消費済み
    /// - `Ok(None)` — データ不足、追加データが必要
    /// - `Err(e)` — デコードエラー
    pub fn try_decode_varint(&mut self) -> Result<Option<u64>, MessageError> {
        if self.buf.is_empty() {
            return Ok(None);
        }
        match varint::decode(&self.buf) {
            Ok((val, consumed)) => {
                self.buf.drain(..consumed);
                Ok(Some(val))
            }
            Err(MessageError::UnexpectedEof) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

impl Default for MessageDecoder {
    fn default() -> Self {
        Self::new()
    }
}
