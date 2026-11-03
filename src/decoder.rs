//! バッファ付きインクリメンタルデコーダー (sans I/O)
//!
//! MOQT の制御メッセージは type(vi64) + length(vi64) + payload で構成される
//! (draft-ietf-moq-transport-23 §9 (Control Messages))。
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
/// `max_control_message_size` は自側が SETUP で広告した `MAX_CONTROL_MESSAGE_SIZE`
/// (draft-ietf-moq-transport-23 §9.1.8 (MAX_CONTROL_MESSAGE_SIZE)) であり、peer の SETUP を
/// 受信する前から適用される (自側の受信上限は自側が決めるため交渉を待たない)。
/// 自側が Setup Option を省略した場合の既定値は
/// [`crate::parameter::DEFAULT_MAX_CONTROL_MESSAGE_SIZE`]。
///
/// draft-ietf-moq-transport-23 §15.5 (Resource Exhaustion): "an endpoint that buffers complete
/// control messages commits that much memory per stream. Such an endpoint needs to grant
/// enough flow control for a complete message on each stream, or the endpoints can deadlock."
/// 本デコーダーは宣言長に応じた領域を確保せず、実際に受信したバイト列だけを保持する。
/// I/O 層は transport のストリームごとのフロー制御窓を、ヘッダ (Type と Length の最大 9 + 9
/// バイト) を含む 1 メッセージ以上に設定すること。
///
/// # 使い方 (検証対象外の疑似コード)
///
/// ```text
/// let mut decoder = MessageDecoder::new(max_control_message_size);
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
    max_control_message_size: u64,
}

impl MessageDecoder {
    /// 新しいデコーダーを作成する
    ///
    /// `max_control_message_size` は自側が受信を許容する制御メッセージ本文の最大長 (バイト)。
    /// peer が宣言した Length がこの値を超える場合、本文を待たずに
    /// [`MessageError::FieldLengthExceeded`] を返す。
    pub fn new(max_control_message_size: u64) -> Self {
        Self {
            buf: Vec::new(),
            max_control_message_size,
        }
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
    ///
    /// # Errors
    ///
    /// - 宣言された Length が `max_control_message_size` を超える:
    ///   [`MessageError::FieldLengthExceeded`]
    ///   (draft-ietf-moq-transport-23 §9 (Control Messages) の MUST。本文は待たない)
    /// - 本文が Length と一致しない: [`MessageError::MalformedMessage`]
    /// - 未知のメッセージ型など、メッセージ固有のデコード検証に失敗した: そのエラー
    pub fn try_decode_message(&mut self) -> Result<Option<ControlMessage>, MessageError> {
        if self.buf.is_empty() {
            return Ok(None);
        }
        // Type (vi64) を読む
        let (_, type_len) = match varint::decode(&self.buf) {
            Ok(v) => v,
            Err(MessageError::UnexpectedEof) => return Ok(None),
            Err(e) => return Err(e),
        };
        // Length (vi64) を読む
        let (payload_len, length_len) = match varint::decode(&self.buf[type_len..]) {
            Ok(v) => v,
            Err(MessageError::UnexpectedEof) => return Ok(None),
            Err(e) => return Err(e),
        };
        // 長さ宣言の段階で受信上限を検証する。巨大な本文を待たず、宣言長に応じた領域も
        // 確保しない (draft-ietf-moq-transport-23 §9 (Control Messages) / §15.5
        // (Resource Exhaustion))。
        if payload_len > self.max_control_message_size {
            return Err(MessageError::FieldLengthExceeded(
                "control message exceeds local MAX_CONTROL_MESSAGE_SIZE",
            ));
        }
        // 32bit 環境では usize に収まらない長さを待ち続けない
        if payload_len > usize::MAX as u64 {
            return Err(MessageError::FieldLengthExceeded(
                "control message length does not fit in usize",
            ));
        }
        let header_len = type_len + length_len;
        if payload_len > (self.buf.len() - header_len) as u64 {
            return Ok(None);
        }
        // 外側フレーム (Type + Length + payload) が揃ったので decode する。
        // ここで返るエラー (本文長の不一致など) は追加データでは回復し得ないため、
        // `Ok(None)` に吸収せず呼び出し側へ返す。吸収するとバッファが drain されず
        // 恒久停止する。
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
    /// draft-ietf-moq-transport-22 §6.4.1 (Unidirectional Streams):
    /// 全ての MOQT 単方向ストリームは先頭にストリームタイプ varint を持つ。
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
    /// 既定の受信上限 (`DEFAULT_MAX_CONTROL_MESSAGE_SIZE`) で作成する
    fn default() -> Self {
        Self::new(crate::parameter::DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
    }
}
