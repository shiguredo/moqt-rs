//! OBJECT_DATAGRAM (draft-ietf-moq-transport-21 §11.2.1 (Object Datagram))
//!
//! Type Flags の定義済み bit: 0x01 / 0x02 / 0x04 / 0x08 / 0x20。
//! 0x10 は予約、0x40 は未定義であり、意味未定義の set bit があれば
//! PROTOCOL_VIOLATION でセッションを閉じなければならない (MUST)。
//! - bit0 = PROPERTIES
//! - bit1 = END_OF_GROUP
//! - bit2 = ZERO_OBJECT_ID (1 のとき Object ID フィールド省略、値は 0)
//! - bit3 = DEFAULT_PRIORITY
//! - bit4 = 常に 0 (予約)
//! - bit5 = STATUS (1 のとき Object Status あり、Payload なし)
//!
//! 無効な組み合わせ: STATUS (0x20) と END_OF_GROUP (0x02) の同時指定
//! この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
use super::{validate_object_status, validate_properties_blob};
use crate::{error::MessageError, varint};
use alloc::vec::Vec;

/// Datagram の Object Properties 生バイト列 (Properties Length + Properties) を検証する
///
/// draft-ietf-moq-transport-21 §11.2.1 (Object Datagram): Datagram の properties_data は
/// Properties Length varint + Properties の生バイト列であり、SubgroupObject /
/// FetchStreamObject と同じく Length を含む規約である。Datagram では Properties Length = 0 は
/// プロトコル違反 (受信側は §11.2.1 で PROTOCOL_VIOLATION)。
/// 空スライス (Length varint すら含まない)・途中で切れた Length varint・
/// 宣言 Length と実データ長の不一致も拒否する。
///
/// `ObjectDatagram::encode` と `send_object_datagram` の双方から使い、公開 API から
/// 不正なワイヤを生成しないようにする。
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
pub(crate) fn validate_datagram_properties_blob(data: &[u8]) -> Result<(), MessageError> {
    if validate_properties_blob(data)? == 0 {
        return Err(MessageError::ProtocolViolation(
            "datagram properties length 0 is invalid when PROPERTIES bit is set",
        ));
    }
    Ok(())
}

/// OBJECT_DATAGRAM の定義済み Type Flags bit
/// (draft-ietf-moq-transport-21 §11.2.1 (Object Datagram))
const OBJECT_DATAGRAM_KNOWN_MASK: u64 = 0x01 | 0x02 | 0x04 | 0x08 | 0x20;

/// Object Datagram Type として有効か検証する (draft-ietf-moq-transport-21 §11.2.1 (Object Datagram))
///
/// §11.2.1 は無効な Type 値を列挙し "If an endpoint receives a datagram with any of these
/// Type values, it MUST close the session with a PROTOCOL_VIOLATION" と規定する。
///
/// - 意味未定義の set bit がある値 (0x10 予約・ 0x40 未定義・ 128 以上を含む)
/// - STATUS (0x20) と END_OF_GROUP (0x02) の両方が立っている値
///   (0x2F 以下の例: 0x22, 0x23, 0x26, 0x27, 0x2A, 0x2B, 0x2E, 0x2F)
///
/// `ObjectDatagram::decode` と、Session の datagram type 分岐が同じ判定を使うために
/// 切り出している。節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
pub fn validate_object_datagram_type(type_id: u64) -> Result<(), MessageError> {
    // 意味未定義の set bit があれば PROTOCOL_VIOLATION
    if type_id & !OBJECT_DATAGRAM_KNOWN_MASK != 0 {
        return Err(MessageError::ProtocolViolation(
            "object datagram type has undefined bits set",
        ));
    }
    // STATUS と END_OF_GROUP の同時指定は無効
    if type_id & 0x20 != 0 && type_id & 0x02 != 0 {
        return Err(MessageError::ProtocolViolation(
            "STATUS and END_OF_GROUP cannot both be set",
        ));
    }
    Ok(())
}

/// OBJECT_DATAGRAM (draft-ietf-moq-transport-21 §11.2.1 (Object Datagram))
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectDatagram {
    /// 対象トラックの Alias
    pub track_alias: u64,
    /// 対象 Group ID
    pub group_id: u64,
    /// ZERO_OBJECT_ID bit が立っている場合は 0 (draft-ietf-moq-transport-21 §11.2.1 (Object Datagram))
    pub object_id: u64,
    /// `None` = DEFAULT_PRIORITY bit が立っている (サブスクリプションの優先度を継承)
    pub publisher_priority: Option<u8>,
    /// Object Properties の生バイト列 (Properties Length varint + Properties)
    /// (draft-ietf-moq-transport-21 §11.1.3 (Object Properties))
    ///
    /// `Some(data)` の場合は PROPERTIES bit を立て、data をそのまま書き込む
    /// (SubgroupObject / FetchStreamObject と同じく Properties Length を含む)。
    /// `None` の場合は PROPERTIES bit を立てない。
    /// draft-ietf-moq-transport-21 §11.2.1 (Object Datagram): Datagram では Properties Length = 0 はプロトコル違反。
    pub properties_data: Option<Vec<u8>>,
    /// この Datagram が Group の末尾かどうか
    pub end_of_group: bool,
    /// STATUS bit が立っている場合のみ Some
    pub status: Option<u64>,
}

impl ObjectDatagram {
    /// ヘッダをエンコードする
    ///
    /// `properties_data` には Properties Length + Properties の生バイト列を指定すると
    /// PROPERTIES bit を立て、そのままエンコードする。
    /// このメソッドは datagram header だけを返すため、`status == None` の場合は
    /// caller が後続に 1 byte 以上の payload を付けなければならない。
    ///
    /// # Errors
    ///
    /// - STATUS と END_OF_GROUP の同時指定: `ProtocolViolation`
    /// - `properties_data` が空スライス / Properties Length varint が途中で切れている /
    ///   Properties Length = 0 / 宣言長と実データ長の不一致: `ProtocolViolation`
    /// - 非 Normal status に Properties を付けた場合: `ProtocolViolation`
    /// - 未知の Object Status: `ProtocolViolation`
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        // STATUS と END_OF_GROUP の同時指定は無効
        if self.status.is_some() && self.end_of_group {
            return Err(MessageError::ProtocolViolation(
                "STATUS and END_OF_GROUP cannot both be set",
            ));
        }
        // draft-ietf-moq-transport-21 §11.2.1 (Object Datagram): Datagram では Properties Length = 0 はプロトコル違反
        if let Some(ref data) = self.properties_data {
            validate_datagram_properties_blob(data)?;
        }
        // draft-ietf-moq-transport-21 §11.1.3 (Object Properties): status が Normal 以外のオブジェクトに Properties は不可
        if self.properties_data.is_some() && matches!(self.status, Some(s) if s != 0) {
            return Err(MessageError::ProtocolViolation(
                "properties on non-Normal status object is not allowed",
            ));
        }

        let has_properties = self.properties_data.is_some();
        let mut type_byte: u8 = 0x00;
        // draft-ietf-moq-transport-21 §11.2.1 (Object Datagram): ZERO_OBJECT_ID bit は Object ID == 0 のとき立てる
        let zero_object_id = self.object_id == 0;

        if has_properties {
            type_byte |= 0x01;
        }
        if self.end_of_group {
            type_byte |= 0x02;
        }
        if zero_object_id {
            type_byte |= 0x04;
        }
        if self.publisher_priority.is_none() {
            type_byte |= 0x08;
        }
        if self.status.is_some() {
            type_byte |= 0x20;
        }

        let mut buf = Vec::new();
        varint::encode(u64::from(type_byte), &mut buf);
        varint::encode(self.track_alias, &mut buf);
        varint::encode(self.group_id, &mut buf);
        if !zero_object_id {
            varint::encode(self.object_id, &mut buf);
        }
        if let Some(prio) = self.publisher_priority {
            buf.push(prio);
        }
        if let Some(ref data) = self.properties_data {
            buf.extend_from_slice(data);
        }
        if let Some(status) = self.status {
            // draft-ietf-moq-transport-21 §11.1.2 (Object Status): 値域検証
            validate_object_status(status)?;
            varint::encode(status, &mut buf);
        }
        Ok(buf)
    }

    /// バッファ先頭から OBJECT_DATAGRAM ヘッダをデコードし `(header, 消費バイト数)` を返す
    ///
    /// プロパティヘッダが存在する場合は Properties Length varint を含む生バイト列を保持する
    /// (encode 側の「生バイト列をそのまま書く」規約と対称)。
    /// ステータスが存在する場合は読み込んで `status` に設定する。
    /// ペイロードはデコードしない。
    ///
    /// datagram 全体の長さを使い、zero-length Normal object と status object の
    /// payload 禁止も合わせて検証する。
    ///
    /// `buf` は厳密に 1 つの datagram 全体でなければならない。datagram は
    /// 1 メッセージ = 1 データグラムであり、残りバイト数 (`buf.len() - pos`) を
    /// そのまま payload 長とみなして検証するため、複数 datagram を連結した
    /// バッファを渡すと payload 長検証が誤動作する。
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (type_id, n) = varint::decode(&buf[pos..])?;
        pos += n;

        validate_object_datagram_type(type_id)?;

        let type_byte = type_id as u8;
        let has_properties = type_byte & 0x01 != 0;
        let end_of_group = type_byte & 0x02 != 0;
        let zero_object_id = type_byte & 0x04 != 0;
        let default_priority = type_byte & 0x08 != 0;
        let has_status = type_byte & 0x20 != 0;

        let (track_alias, n) = varint::decode(&buf[pos..])?;
        pos += n;
        let (group_id, n) = varint::decode(&buf[pos..])?;
        pos += n;

        // draft-ietf-moq-transport-21 §11.2.1 (Object Datagram): ZERO_OBJECT_ID bit が立っている場合、Object ID は 0
        let object_id = if zero_object_id {
            0
        } else {
            let (id, n) = varint::decode(&buf[pos..])?;
            pos += n;
            id
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

        // プロパティヘッダを読み取る。Properties Length varint を含む生バイト列を保持し、
        // encode 側の「受け取った生バイト列をそのまま書く」規約と対称にする。
        let properties_data = if has_properties {
            let prop_start = pos;
            let (ext_len, n) = varint::decode(&buf[pos..])?;
            pos += n;
            if ext_len == 0 {
                return Err(MessageError::ProtocolViolation(
                    "datagram properties length 0 is invalid when PROPERTIES bit is set",
                ));
            }
            // sibling の subgroup.rs と同じく checked_len で u64 空間のまま境界比較してから
            // usize 変換する。32bit ターゲットで ext_len > u32::MAX のとき素キャストでは
            // 切り詰められて誤読するため、checked_len で UnexpectedEof にする。
            let ext_len = varint::checked_len(ext_len, buf[pos..].len())?;
            pos += ext_len;
            Some(buf[prop_start..pos].to_vec())
        } else {
            None
        };

        let status = if has_status {
            let (s, n) = varint::decode(&buf[pos..])?;
            pos += n;
            validate_object_status(s)?;
            Some(s)
        } else {
            None
        };

        // draft-ietf-moq-transport-21 §11.1.3 (Object Properties): status が Normal (0x0) 以外のオブジェクトに
        // Properties が付いている場合は PROTOCOL_VIOLATION
        if properties_data.is_some() && matches!(status, Some(s) if s != 0) {
            return Err(MessageError::ProtocolViolation(
                "properties on non-Normal status object is not allowed",
            ));
        }

        let remaining_payload_len = buf.len() - pos;
        if status.is_some() {
            if remaining_payload_len != 0 {
                return Err(MessageError::ProtocolViolation(
                    "status object must not carry payload",
                ));
            }
        } else if remaining_payload_len == 0 {
            return Err(MessageError::ProtocolViolation(
                "zero-length object must explicitly encode Normal status",
            ));
        }

        Ok((
            Self {
                track_alias,
                group_id,
                object_id,
                publisher_priority,
                properties_data,
                end_of_group,
                status,
            },
            pos,
        ))
    }
}
