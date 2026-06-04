//! QUIC ストリームおよびデータグラムのヘッダ型 (draft-ietf-moq-transport-21 §11 (Data Streams and Datagrams))
//!
//! ストリーム型 ID はビットフラグを含む varint で表される。
//!
//! サブモジュール構成:
//! - `subgroup`: SUBGROUP_HEADER / SUBGROUP_OBJECT (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header))
//! - `fetch`: FETCH_HEADER / FETCH_STREAM_OBJECT (draft-ietf-moq-transport-21 §11.4.1 (Fetch Header))
//! - `datagram`: OBJECT_DATAGRAM (draft-ietf-moq-transport-21 §11.2.1 (Object Datagram))
//! - `decoder`: Subgroup / Fetch ストリームのインクリメンタルデコーダ (sans I/O)
//! - `encoder`: Fetch ストリームのデルタ圧縮エンコーダ (sans I/O)
use crate::error::MessageError;
use crate::message::ControlMessage;
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
    /// Fetch レスポンスストリーム (draft-ietf-moq-transport-21 §11.4.1)
    Fetch,
    /// Subgroup ストリーム (draft-ietf-moq-transport-21 §11.3.1)
    Subgroup,
    /// 帯域幅プロービング用パディングストリーム (draft-ietf-moq-transport-21 §11.5.1 (Padding Streams))
    ///
    /// §11.5.1 は "The receiver MUST discard all data received on a padding stream to
    /// prevent exhausting flow control." と無条件の破棄を要求する。したがって
    /// パディングストリームのバイト列を Session へ渡す API は用意しない。I/O 層は
    /// `recv_data_stream_type` でこの種別を得たあと、後続バイトを読み捨てるだけでよい。
    ///
    /// Session が種別だけを保持するのは、同一ストリームに subgroup / fetch header が
    /// 来た場合を `PROTOCOL_VIOLATION` として弾くためである。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    Padding,
}

/// FETCH_HEADER の type 値 (draft-ietf-moq-transport-21 §11.4.1 (Fetch Header))
pub const FETCH_HEADER_TYPE: u64 = 0x05;

/// パディングストリームの type 値 (draft-ietf-moq-transport-21 §11.5.1 (Padding Streams))
pub const PADDING_STREAM_TYPE: u64 = 0x132B_3E28;

/// パディングデータグラムの type 値 (draft-ietf-moq-transport-21 §11.5.2 (Padding Datagrams))
pub const PADDING_DATAGRAM_TYPE: u64 = 0x132B_3E29;

/// SETUP 制御ストリームの stream type (draft-ietf-moq-transport-21 §6.4.1 (Unidirectional Streams) Table 3)
///
/// draft-ietf-moq-transport-21 §9.1 (SETUP) Figure 6 の SETUP メッセージ type と同じ値だが、
/// stream type と message type は異なる名前空間である。
pub const SETUP_STREAM_TYPE: u64 = 0x2F00;

/// uni data stream の先頭 varint から種別を判定する
///
/// - `0x132B_3E28` → [`DataStreamType::Padding`]
/// - `0x05` → [`DataStreamType::Fetch`]
/// - `0x10..=0x1F` / `0x30..=0x3F` / `0x50..=0x5F` / `0x70..=0x7F` かつ bit4=1 → [`DataStreamType::Subgroup`]
/// - それ以外 → `None`
///
/// 予約済みの `SUBGROUP_ID_MODE = 0b11` を含む type 値も `Subgroup` として返す。
/// それらは後続の [`SubgroupHeader`] デコードが `PROTOCOL_VIOLATION` として reject する。
pub fn classify_data_stream_type(type_id: u64) -> Option<DataStreamType> {
    // control stream (SETUP_STREAM_TYPE = 0x2F00) は data stream ではないため除外する
    // (draft-ietf-moq-transport-21 §6.4.1 (Unidirectional Streams) Table 3 で制御ストリームは別扱い)
    if type_id == SETUP_STREAM_TYPE {
        return None;
    }
    if type_id == FETCH_HEADER_TYPE {
        return Some(DataStreamType::Fetch);
    }
    // draft-ietf-moq-transport-21 §11.5.1 (Padding Streams): パディングストリーム
    if type_id == PADDING_STREAM_TYPE {
        return Some(DataStreamType::Padding);
    }
    if type_id <= 0x7F && type_id & 0x10 != 0 {
        return Some(DataStreamType::Subgroup);
    }
    None
}

/// 制御ストリーム用の stream type prefix を varint エンコードして返す
///
/// 新規制御ストリームを開設して SETUP を送信する際は
/// [`encode_control_stream_setup`] を使うこと。
fn encode_control_stream_prefix() -> Vec<u8> {
    let mut buf = Vec::new();
    crate::varint::encode(SETUP_STREAM_TYPE, &mut buf);
    buf
}

/// 新規制御ストリームの先頭メッセージを stream type prefix 込みでエンコードする
///
/// [`SubgroupHeader::encode`] や [`FetchHeader::encode`] が stream type を
/// 内包するのと同様に、制御ストリーム開設時の初回メッセージ (SETUP) も
/// stream type prefix を自動で前置する。
pub fn encode_control_stream_setup(msg: &ControlMessage) -> Result<Vec<u8>, MessageError> {
    let mut buf = encode_control_stream_prefix();
    buf.extend_from_slice(&msg.encode()?);
    Ok(buf)
}

/// Object Status: Normal (draft-ietf-moq-transport-21 §11.1.2 (Object Status))
pub(crate) const OBJECT_STATUS_NORMAL: u64 = 0x0;
/// Object Status: End of Group (draft-ietf-moq-transport-21 §11.1.2 (Object Status))
///
/// §11.1.2: "Indicates that no objects with the specified Group ID and the Object ID that is
/// greater than or equal to the one specified exist in the group identified by the Group ID."
pub const OBJECT_STATUS_END_OF_GROUP: u64 = 0x3;
/// Object Status: End of Track (draft-ietf-moq-transport-21 §11.1.2 (Object Status))
///
/// §11.1.2: "Indicates that no objects with the location that is equal to or greater than the
/// one specified exist."
pub const OBJECT_STATUS_END_OF_TRACK: u64 = 0x4;

/// draft-ietf-moq-transport-21 §11.1.2 (Object Status): Object Status の値域を検証する
///
/// 有効値は IANA Object Status registry に登録された値 (0x0 Normal, 0x3 End of Group,
/// 0x4 End of Track)。未知の status code は draft-ietf-moq-transport-21 §11.1.2 に従い
/// SHOULD be treated as a protocol error (PROTOCOL_VIOLATION でセッションを閉じる)。
fn validate_object_status(status: u64) -> Result<(), MessageError> {
    match status {
        OBJECT_STATUS_NORMAL | OBJECT_STATUS_END_OF_GROUP | OBJECT_STATUS_END_OF_TRACK => Ok(()),
        _ => Err(MessageError::ProtocolViolation(
            "unknown object status value",
        )),
    }
}
