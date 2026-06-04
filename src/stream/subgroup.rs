//! SUBGROUP_HEADER / SUBGROUP_OBJECT (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header))
//!
//! Type Flags の定義済み bit: bit0 (0x01) / bits 1-2 (mask 0x06) / bit3 (0x08) /
//! bit4 (0x10、必須) / bit5 (0x20) / bit6 (0x40)。
//! 128 以上の値は未定義であり、意味未定義の set bit があれば
//! PROTOCOL_VIOLATION でセッションを閉じなければならない (MUST)。
//! - bit0 = PROPERTIES
//! - bit1-2 = SUBGROUP_ID_MODE (0b11 は予約済みで無効)
//! - bit3 = END_OF_GROUP
//! - bit4 = 常に 1
//! - bit5 = DEFAULT_PRIORITY
//! - bit6 = FIRST_OBJECT
//!
//! この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
use super::validate_object_status;
use crate::{error::MessageError, varint};
use alloc::vec::Vec;

/// SUBGROUP_HEADER の定義済み Type Flags bit
/// (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header))
const SUBGROUP_HEADER_KNOWN_MASK: u64 = 0x01 | 0x06 | 0x08 | 0x10 | 0x20 | 0x40;

/// Subgroup ID のエンコードモード
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SubgroupIdMode {
    /// 0b00: Subgroup ID フィールドなし、Subgroup ID = 0
    Zero,
    /// 0b01: Subgroup ID フィールドなし、Subgroup ID = このストリームの最初のオブジェクト ID
    FirstObjectId,
    /// 0b10: Subgroup ID フィールドあり
    Explicit(u64),
}

/// SUBGROUP_HEADER (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header))
#[derive(Debug, Clone, PartialEq)]
pub struct SubgroupHeader {
    /// 対象トラックの Alias
    pub track_alias: u64,
    /// 対象 Group ID
    pub group_id: u64,
    /// Subgroup ID のエンコードモード
    pub subgroup_id: SubgroupIdMode,
    /// `None` = DEFAULT_PRIORITY bit が立っている (サブスクリプションの優先度を継承)
    pub publisher_priority: Option<u8>,
    /// このストリームの Object が Properties を持つかどうか
    pub has_properties: bool,
    /// このヘッダが Group の末尾を含むかどうか
    pub end_of_group: bool,
    /// このサブグループストリームの最初のオブジェクトが original publisher によって
    /// そのサブグループに公開された最初のオブジェクトであるか (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header))
    pub first_object: bool,
}

impl SubgroupHeader {
    /// ヘッダ全体をエンコードする
    pub fn encode(&self) -> Vec<u8> {
        let mut type_byte: u8 = 0x10; // bit4 は常に 1

        if self.has_properties {
            type_byte |= 0x01;
        }
        match &self.subgroup_id {
            SubgroupIdMode::Zero => {}
            SubgroupIdMode::FirstObjectId => {
                type_byte |= 0x02;
            }
            SubgroupIdMode::Explicit(_) => {
                type_byte |= 0x04;
            }
        }
        if self.end_of_group {
            type_byte |= 0x08;
        }
        if self.publisher_priority.is_none() {
            type_byte |= 0x20;
        }
        if self.first_object {
            type_byte |= 0x40;
        }

        let mut buf = Vec::new();
        varint::encode(u64::from(type_byte), &mut buf);
        varint::encode(self.track_alias, &mut buf);
        varint::encode(self.group_id, &mut buf);
        if let SubgroupIdMode::Explicit(id) = &self.subgroup_id {
            varint::encode(*id, &mut buf);
        }
        if let Some(prio) = self.publisher_priority {
            buf.push(prio);
        }
        buf
    }

    /// バッファ先頭から SUBGROUP_HEADER をデコードし `(header, 消費バイト数)` を返す
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (type_id, n) = varint::decode(&buf[pos..])?;
        pos += n;

        // bit4 が必須
        if type_id & 0x10 == 0 {
            return Err(MessageError::ProtocolViolation(
                "subgroup header type must have bit4 set",
            ));
        }
        // 意味未定義の set bit (128 以上) があれば PROTOCOL_VIOLATION
        // (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header))
        if type_id & !SUBGROUP_HEADER_KNOWN_MASK != 0 {
            return Err(MessageError::ProtocolViolation(
                "subgroup header type has undefined bits set",
            ));
        }

        let type_byte = type_id as u8;
        let has_properties = type_byte & 0x01 != 0;
        let subgroup_id_mode = (type_byte & 0x06) >> 1;
        let end_of_group = type_byte & 0x08 != 0;
        let default_priority = type_byte & 0x20 != 0;
        let first_object = type_byte & 0x40 != 0;

        // SUBGROUP_ID_MODE = 0b11 は予約済み
        if subgroup_id_mode == 0b11 {
            return Err(MessageError::ProtocolViolation(
                "subgroup_id_mode 0b11 is reserved",
            ));
        }

        let (track_alias, n) = varint::decode(&buf[pos..])?;
        pos += n;
        let (group_id, n) = varint::decode(&buf[pos..])?;
        pos += n;

        let subgroup_id = match subgroup_id_mode {
            0b00 => SubgroupIdMode::Zero,
            0b01 => SubgroupIdMode::FirstObjectId,
            0b10 => {
                let (id, n) = varint::decode(&buf[pos..])?;
                pos += n;
                SubgroupIdMode::Explicit(id)
            }
            _ => {
                unreachable!("subgroup_id_mode 0b11 is rejected above; only 0b00/0b01/0b10 remain")
            }
        };

        let publisher_priority = if default_priority {
            None
        } else {
            if buf[pos..].is_empty() {
                return Err(MessageError::UnexpectedEof);
            }
            let prio = buf[pos];
            pos += 1;
            Some(prio)
        };

        Ok((
            Self {
                track_alias,
                group_id,
                subgroup_id,
                publisher_priority,
                has_properties,
                end_of_group,
                first_object,
            },
            pos,
        ))
    }
}

/// サブグループストリーム内のオブジェクトヘッダ (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header))
///
/// 構造:
/// - Object ID Delta (vi64)
/// - [Properties (..)]          — SUBGROUP_HEADER の PROPERTIES ビットに依存
/// - Object Payload Length (vi64)
/// - [Object Status (vi64)]     — Payload Length が 0 の場合のみ
/// - [Object Payload (..)]      — Payload Length が > 0 の場合のみ
///
/// `has_properties` は SUBGROUP_HEADER の PROPERTIES ビットから決まるため、
/// encode/decode 時に外部から渡す。
///
/// ペイロードデータ自体は含まない。呼び出し元が `payload_length` バイト分のペイロードを
/// 後続のバッファから読み書きする。
#[derive(Debug, Clone, PartialEq)]
pub struct SubgroupObject {
    /// Object ID Delta:
    /// - 最初のオブジェクト: Object ID そのもの
    /// - 以降: `(今回の Object ID) - (前回の Object ID) - 1`
    pub object_id_delta: u64,
    /// ペイロード長 (0 の場合は status が必須)
    pub payload_length: u64,
    /// Object Status (payload_length == 0 の場合のみ)
    pub status: Option<u64>,
}

impl SubgroupObject {
    /// オブジェクトヘッダをエンコードする
    ///
    /// `has_properties` は SUBGROUP_HEADER の PROPERTIES ビットに対応する。
    /// true の場合、呼び出し元が LocProperties でエンコードした結果を `properties_data` として渡す。
    /// プロパティが空でも `has_properties` が true なら Properties Length = 0 を含むデータを渡す必要がある。
    pub fn encode(
        &self,
        has_properties: bool,
        properties_data: Option<&[u8]>,
        buf: &mut Vec<u8>,
    ) -> Result<(), MessageError> {
        // payload_length == 0 なのに status がない場合はエラー
        if self.payload_length == 0 && self.status.is_none() {
            return Err(MessageError::ProtocolViolation(
                "object status is required when payload length is 0",
            ));
        }
        // payload_length > 0 なのに status がある場合はエラー
        if self.payload_length > 0 && self.status.is_some() {
            return Err(MessageError::ProtocolViolation(
                "object status must not be present when payload length > 0",
            ));
        }
        // draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header): PROPERTIES ビットが立っている場合は
        // Object Properties (draft-ietf-moq-transport-21 §11.1.3 (Object Properties)) が必須。
        // 空でも Properties Length = 0 を含む (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header))
        if has_properties && properties_data.is_none() {
            return Err(MessageError::ProtocolViolation(
                "properties data is required when has_properties is true",
            ));
        }
        if !has_properties && properties_data.is_some() {
            return Err(MessageError::ProtocolViolation(
                "properties data must not be present when has_properties is false",
            ));
        }
        // draft-ietf-moq-transport-21 §11.1.3 (Object Properties): status が Normal (0x0) 以外のオブジェクトに
        // 実際のプロパティデータ (Properties Length > 0) が付いている場合は PROTOCOL_VIOLATION。
        // PROPERTIES bit はストリーム全体のフラグであり、non-Normal オブジェクトは
        // Properties Length = 0 で「プロパティなし」を表現する (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header))。
        if let Some(props) = properties_data
            && matches!(self.status, Some(s) if s != 0)
        {
            // Properties Length varint をデコードして実際のプロパティ有無を判定する
            let (prop_len, _) = varint::decode(props).map_err(|_| {
                MessageError::ProtocolViolation("malformed properties length in subgroup object")
            })?;
            if prop_len > 0 {
                return Err(MessageError::ProtocolViolation(
                    "properties on non-Normal status object is not allowed",
                ));
            }
        }

        varint::encode(self.object_id_delta, buf);

        // Properties: 呼び出し元が渡したデータをそのまま書き込む
        if let Some(props) = properties_data {
            buf.extend_from_slice(props);
        }

        varint::encode(self.payload_length, buf);

        if let Some(status) = self.status {
            // draft-ietf-moq-transport-21 §11.1.2 (Object Status): 値域検証
            validate_object_status(status)?;
            varint::encode(status, buf);
        }

        Ok(())
    }

    /// バッファ先頭からオブジェクトヘッダをデコードし `(object, 消費バイト数)` を返す
    ///
    /// `has_properties` は SUBGROUP_HEADER の PROPERTIES ビットから決まる。
    /// Properties の生バイト (Properties Length varint + Properties データ) も返す。
    /// ペイロードデータはデコードしない。
    ///
    /// 戻り値: `(Self, Option<properties_bytes>, consumed_bytes)`
    pub fn decode(
        buf: &[u8],
        has_properties: bool,
    ) -> Result<(Self, Option<Vec<u8>>, usize), MessageError> {
        let mut pos = 0;

        let (object_id_delta, n) = varint::decode(&buf[pos..])?;
        pos += n;

        // Properties: SUBGROUP_HEADER の PROPERTIES ビットが立っている場合は存在する
        // draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header): "Objects with no properties set Properties Length to 0."
        // Subgroup object では Properties Length = 0 は合法 (Datagram とは異なる)
        let properties_bytes = if has_properties {
            let prop_start = pos;
            let (prop_len, n) = varint::decode(&buf[pos..])?;
            pos += n;
            let prop_len = varint::checked_len(prop_len, buf[pos..].len())?;
            pos += prop_len;
            Some(buf[prop_start..pos].to_vec())
        } else {
            None
        };

        let (payload_length, n) = varint::decode(&buf[pos..])?;
        pos += n;

        let status = if payload_length == 0 {
            let (s, n) = varint::decode(&buf[pos..])?;
            pos += n;
            validate_object_status(s)?;
            Some(s)
        } else {
            None
        };

        // draft-ietf-moq-transport-21 §11.1.3 (Object Properties): status が Normal (0x0) 以外のオブジェクトに
        // 実際のプロパティデータ (Properties Length > 0) が付いている場合は PROTOCOL_VIOLATION。
        // PROPERTIES bit はストリーム全体のフラグであり、non-Normal オブジェクトは
        // Properties Length = 0 で「プロパティなし」を表現する (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header))。
        if let Some(ref props) = properties_bytes
            && matches!(status, Some(s) if s != 0)
        {
            // Properties Length varint をデコードして実際のプロパティ有無を判定する
            let (prop_len, _) = varint::decode(props).map_err(|_| {
                MessageError::ProtocolViolation("malformed properties length in subgroup object")
            })?;
            if prop_len > 0 {
                return Err(MessageError::ProtocolViolation(
                    "properties on non-Normal status object is not allowed",
                ));
            }
        }

        Ok((
            Self {
                object_id_delta,
                payload_length,
                status,
            },
            properties_bytes,
            pos,
        ))
    }
}
