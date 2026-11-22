//! バッファ付きインクリメンタルデコーダー (sans I/O)
//!
//! MOQT の制御メッセージは type(vi64) + length(vi64) + payload で構成される
//! (draft-ietf-moq-transport-23 §9 (Control Messages))。
//! QUIC/WebTransport のフレーム境界とメッセージ境界は一致しないため、
//! 受信バッファに蓄積しながらインクリメンタルにデコードする必要がある。
//!
//! このモジュールは I/O を持たず、バッファ管理とデコード試行のみを提供する。
//! ストリームからのデータ受信は呼び出し側が行う。
use crate::decode_buffer::DecodeBuffer;
use crate::error::MessageError;
use crate::message::{ControlMessage, decode_control_message_header};
use crate::varint;

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
    /// 受信済みで未デコードのバイト列
    ///
    /// `Vec::drain(..n)` をメッセージごとに呼ばず読み出し位置だけを進めることで、
    /// 小さなメッセージを大量に含むチャンクでも二乗のコピーを避ける。
    buffer: DecodeBuffer,
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
            buffer: DecodeBuffer::new(),
            max_control_message_size,
        }
    }

    /// バッファにデータを追加する
    pub fn push(&mut self, data: &[u8]) {
        self.buffer.push(data);
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
        // Type (vi64) と Length (vi64) の解析は `ControlMessage::decode` と共通のヘルパに
        // 委ねる。ヘッダ未完成 (`Ok(None)`) は追加データ待ちとして扱う。
        let Some(header) = decode_control_message_header(self.buffer.available())? else {
            return Ok(None);
        };
        // 長さ宣言の段階で受信上限を検証する。巨大な本文を待たず、宣言長に応じた領域も
        // 確保しない (draft-ietf-moq-transport-23 §9 (Control Messages) / §15.5
        // (Resource Exhaustion))。
        if header.payload_len > self.max_control_message_size {
            return Err(MessageError::FieldLengthExceeded(
                "control message exceeds local MAX_CONTROL_MESSAGE_SIZE",
            ));
        }
        // 32bit 環境では usize に収まらない長さを待ち続けない
        if header.payload_len > usize::MAX as u64 {
            return Err(MessageError::FieldLengthExceeded(
                "control message length does not fit in usize",
            ));
        }
        // payload の完成判定は本関数が行う。宣言長が揃うまでは Ok(None) を返す。
        let buffered = self.buffer.len();
        if header.payload_len > (buffered - header.header_len) as u64 {
            return Ok(None);
        }
        // 外側フレーム (Type + Length + payload) が揃ったので decode する。
        // ここで返るエラー (本文長の不一致など) は追加データでは回復し得ないため、
        // `Ok(None)` に吸収せず呼び出し側へ返す。吸収するとバッファが drain されず
        // 恒久停止する。
        match ControlMessage::decode(self.buffer.available()) {
            Ok((msg, consumed)) => {
                self.buffer.advance(consumed);
                Ok(Some(msg))
            }
            Err(e) => Err(e),
        }
    }

    /// バッファから varint のデコードを試みる
    ///
    /// ストリームタイプの読み取りに使用する。
    /// draft-ietf-moq-transport-23 §6.4.1 (Unidirectional Streams):
    /// 全ての MOQT 単方向ストリームは先頭にストリームタイプ varint を持つ。
    ///
    /// - `Ok(Some(val))` — デコード成功、バッファから消費済み
    /// - `Ok(None)` — データ不足、追加データが必要
    /// - `Err(e)` — デコードエラー
    pub fn try_decode_varint(&mut self) -> Result<Option<u64>, MessageError> {
        if self.buffer.is_empty() {
            return Ok(None);
        }
        match varint::decode(self.buffer.available()) {
            Ok((val, consumed)) => {
                self.buffer.advance(consumed);
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
