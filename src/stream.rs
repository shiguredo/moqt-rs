//! QUIC ストリームおよびデータグラムのヘッダ型 (draft-ietf-moq-transport-23 §11 (Data Streams and Datagrams))
//!
//! ストリーム型 ID はビットフラグを含む varint で表される。
//!
//! 制御ストリーム (単方向) の先頭 varint は SETUP メッセージの Type (0x2F00) であり、
//! stream type を兼ねる (draft-ietf-moq-transport-23 §6.4.1 (Unidirectional Streams) Table 2 /
//! §9.1 (SETUP) Figure 7)。このため制御ストリームでは stream type を別途前置せず、
//! SETUP メッセージの Type を 1 回だけ書く。
//!
//! サブモジュール構成:
//! - `subgroup`: SUBGROUP_HEADER / SUBGROUP_OBJECT (draft-ietf-moq-transport-23 §11.3.1 (Subgroup Header))
//! - `fetch`: FETCH_HEADER / FETCH_STREAM_OBJECT (draft-ietf-moq-transport-23 §11.4.1 (Fetch Header))
//! - `datagram`: OBJECT_DATAGRAM (draft-ietf-moq-transport-23 §11.2.1 (Object Datagram))
//! - `decoder`: Subgroup / Fetch ストリームのインクリメンタルデコーダ (sans I/O)
//! - `encoder`: Fetch ストリームのデルタ圧縮エンコーダ (sans I/O)
use crate::error::MessageError;
use crate::message::ControlMessage;
use crate::varint;
use alloc::vec::Vec;

pub mod datagram;
pub mod decoder;
pub mod encoder;
pub mod fetch;
pub mod subgroup;

/// 受信 uni data stream の種別
///
/// control stream (`SETUP`) は含まない。caller は control stream を別途扱い、
/// それ以外の uni stream について本種別を使って Session の data plane API に接続する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataStreamType {
    /// Fetch レスポンスストリーム (draft-ietf-moq-transport-23 §11.4.1)
    Fetch,
    /// Subgroup ストリーム (draft-ietf-moq-transport-23 §11.3.1)
    Subgroup,
    /// 帯域幅プロービング用パディングストリーム (draft-ietf-moq-transport-23 §11.5.1 (Padding Streams))
    ///
    /// §11.5.1 は "The receiver MUST discard all data received on a padding stream to
    /// prevent exhausting flow control." と無条件の破棄を要求する。したがって
    /// パディングストリームのバイト列を Session へ渡す API は用意しない。I/O 層は
    /// `recv_data_stream_type` でこの種別を得たあと、後続バイトを読み捨てるだけでよい。
    ///
    /// Session が種別だけを保持するのは、同一ストリームに subgroup / fetch header が
    /// 来た場合を `INVALID_STREAM_HEADER` として弾くためである。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    Padding,
}

/// FETCH_HEADER の type 値 (draft-ietf-moq-transport-23 §11.4.1 (Fetch Header))
pub const FETCH_HEADER_TYPE: u64 = 0x05;

/// パディングストリームの type 値 (draft-ietf-moq-transport-23 §11.5.1 (Padding Streams))
pub const PADDING_STREAM_TYPE: u64 = 0x132B_3E28;

/// パディングデータグラムの type 値 (draft-ietf-moq-transport-23 §11.5.2 (Padding Datagrams))
pub const PADDING_DATAGRAM_TYPE: u64 = 0x132B_3E29;

/// SETUP 制御ストリームの stream type (draft-ietf-moq-transport-23 §6.4.1 (Unidirectional Streams) Table 2)
///
/// この値は draft-ietf-moq-transport-23 §9.1 (SETUP) Figure 7 の SETUP メッセージの Type と
/// 同じ 0x2F00 であり、制御ストリームでは SETUP の Type が stream type を兼ねる。したがって
/// 制御ストリームの先頭 varint は 1 回だけ書き、stream type を前置したうえに SETUP の Type を
/// 書く、という 2 回の表現にはしない。
pub const SETUP_STREAM_TYPE: u64 = 0x2F00;

/// uni data stream の先頭 varint から種別を判定する
///
/// - `0x132B_3E28` → [`DataStreamType::Padding`]
/// - `0x05` → [`DataStreamType::Fetch`]
/// - `0x10..=0x1F` / `0x30..=0x3F` / `0x50..=0x5F` / `0x70..=0x7F` かつ bit4=1 → [`DataStreamType::Subgroup`]
/// - それ以外 → `None`
///
/// 予約済みの `SUBGROUP_ID_MODE = 0b11` を含む type 値も `Subgroup` として返す。
/// それらは後続の [`crate::stream::subgroup::SubgroupHeader`] デコードが
/// `INVALID_STREAM_HEADER` として reject する。
pub fn classify_data_stream_type(type_id: u64) -> Option<DataStreamType> {
    // 制御ストリームの先頭 varint は SETUP メッセージの Type (SETUP_STREAM_TYPE = 0x2F00) が
    // stream type を兼ねるため、data stream として扱わない
    // (draft-ietf-moq-transport-23 §6.4.1 (Unidirectional Streams) Table 2 で制御ストリームは別扱い)
    if type_id == SETUP_STREAM_TYPE {
        return None;
    }
    if type_id == FETCH_HEADER_TYPE {
        return Some(DataStreamType::Fetch);
    }
    // draft-ietf-moq-transport-23 §11.5.1 (Padding Streams): パディングストリーム
    if type_id == PADDING_STREAM_TYPE {
        return Some(DataStreamType::Padding);
    }
    if type_id <= 0x7F && type_id & 0x10 != 0 {
        return Some(DataStreamType::Subgroup);
    }
    None
}

/// 新規制御ストリームの先頭メッセージ (SETUP) をエンコードする
///
/// 制御ストリームの先頭 varint は SETUP メッセージの Type (0x2F00) が stream type を兼ねる
/// (draft-ietf-moq-transport-23 §6.4.1 (Unidirectional Streams) / §9.1 (SETUP))。
/// 本関数は [`ControlMessage::encode`] の結果をそのまま返す。stream type を別途前置しては
/// ならない (前置すると 0x2F00 がワイヤに 2 回現れ、1 回だけ送る実装と相互運用できない)。
///
/// `msg` には [`ControlMessage::Setup`] を渡すこと。
///
/// `max_control_message_size` は peer が広告した `MAX_CONTROL_MESSAGE_SIZE`
/// (draft-ietf-moq-transport-23 §9.1.8 (MAX_CONTROL_MESSAGE_SIZE))。peer の SETUP を
/// 受信する前に送る自側の SETUP には、既定値
/// [`crate::parameter::DEFAULT_MAX_CONTROL_MESSAGE_SIZE`] を渡す。
pub fn encode_control_stream_setup(
    msg: &ControlMessage,
    max_control_message_size: u64,
) -> Result<Vec<u8>, MessageError> {
    msg.encode(max_control_message_size)
}

/// Object Status: Normal (通常の Object) (draft-ietf-moq-transport-23 §11.1.1 (Object Status))
pub(crate) const OBJECT_STATUS_NORMAL: u64 = 0x0;
/// Object Status: End of Group (Group の終端) (draft-ietf-moq-transport-23 §11.1.1 (Object Status))
///
/// §11.1.1: "Indicates that no objects with the specified Group ID and the Object ID that is
/// greater than or equal to the one specified exist in the group identified by the Group ID."
pub const OBJECT_STATUS_END_OF_GROUP: u64 = 0x3;
/// Object Status: End of Track (Track の終端) (draft-ietf-moq-transport-23 §11.1.1 (Object Status))
///
/// §11.1.1: "Indicates that no objects with the Location that is equal to or greater than the
/// one specified exist."
pub const OBJECT_STATUS_END_OF_TRACK: u64 = 0x4;

/// draft-ietf-moq-transport-23 §11.1.1 (Object Status): Object Status の値域を検証する
///
/// 有効値は IANA Object Status registry に登録された値 (0x0 Normal, 0x3 End of Group,
/// 0x4 End of Track)。未知の status code は draft-ietf-moq-transport-23 §11.1.1
/// (Object Status) に従い INVALID_OBJECT_STATUS (0x202) でセッションを閉じる。
pub(crate) fn validate_object_status(status: u64) -> Result<(), MessageError> {
    match status {
        OBJECT_STATUS_NORMAL | OBJECT_STATUS_END_OF_GROUP | OBJECT_STATUS_END_OF_TRACK => Ok(()),
        // draft-ietf-moq-transport-23 §11.1.1 (Object Status) / §12.2
        // (Session Termination Codes): 定義値以外の Status は INVALID_OBJECT_STATUS (0x202)。
        _ => Err(MessageError::InvalidObjectStatus(
            "unknown object status value",
        )),
    }
}

/// Properties 生バイト列の Properties Length と実データ長の一致を検証し、Length を返す
///
/// draft-ietf-moq-transport-23 §11.1.2 (Object Properties): Properties は
/// Properties Length (vi64) + Properties の構造で、Length は後続バイト数を表す。
/// 空スライス、Properties Length varint が途中で切れた blob、長さの一致しない blob は
/// `MalformedObject` とする。
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
fn validate_properties_blob(data: &[u8]) -> Result<u64, MessageError> {
    // draft-ietf-moq-transport-23 §11.1.2 (Object Properties) / §12.2
    // (Session Termination Codes): Properties Length と実データ長が一致しない Object は
    // 直列化を復号できないため MALFORMED_OBJECT (0x201) で閉じる。
    let (prop_len, n) = varint::decode(data)
        .map_err(|_| MessageError::MalformedObject("malformed properties length"))?;
    // varint::decode が Ok のとき n <= data.len() が保証されるため減算で桁あふれしない
    if prop_len != (data.len() - n) as u64 {
        return Err(MessageError::MalformedObject(
            "properties length does not match the actual data length",
        ));
    }
    Ok(prop_len)
}
