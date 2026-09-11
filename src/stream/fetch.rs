//! FETCH_HEADER / FETCH_STREAM_OBJECT (draft-ietf-moq-transport-21 §11.4.1 (Fetch Header))
//!
//! FETCH_HEADER の型 ID は 0x05。
//! FETCH_STREAM_OBJECT の Serialization Flags は以下の通り:
//! - [Group ID Delta (vi64)]    — bit3 が立っている場合
//! - [Subgroup ID (vi64)]       — bits 0-1 が 0b11 の場合
//! - [Object ID Delta (vi64)]   — bit2 が立っている場合
//! - [Publisher Priority (8)]   — bit4 が立っている場合
//! - [Properties (..)]          — bit5 が立っている場合
//! - Object Payload Length (vi64)
//! - [Object Payload (..)]
use super::{FETCH_HEADER_TYPE, validate_properties_blob};
use crate::{error::MessageError, varint};
use alloc::vec::Vec;

/// FETCH_HEADER (draft-ietf-moq-transport-21 §11.4.1 (Fetch Header))
#[derive(Debug, Clone, PartialEq)]
pub struct FetchHeader {
    /// 対応する FETCH 要求の Request ID
    pub request_id: u64,
}

impl FetchHeader {
    /// ヘッダ全体をエンコードする
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        varint::encode(FETCH_HEADER_TYPE, &mut buf);
        varint::encode(self.request_id, &mut buf);
        buf
    }

    /// バッファ先頭から FETCH_HEADER をデコードし `(header, 消費バイト数)` を返す
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (type_id, n) = varint::decode(&buf[pos..])?;
        pos += n;
        if type_id != FETCH_HEADER_TYPE {
            return Err(MessageError::ProtocolViolation(
                "fetch header type must be 0x05",
            ));
        }
        let (request_id, n) = varint::decode(&buf[pos..])?;
        pos += n;
        Ok((Self { request_id }, pos))
    }
}

/// Serialization Flags の特殊値: End of Non-Existent Range
pub(crate) const FETCH_END_OF_NON_EXISTENT_RANGE: u64 = 0x8C;
/// Serialization Flags の特殊値: End of Unknown Range
pub(crate) const FETCH_END_OF_UNKNOWN_RANGE: u64 = 0x10C;
/// Serialization Flags の特殊値: End of Timed-Out Range (draft-ietf-moq-transport-21 §11.4.1 (FETCH stream) Table 7)
pub(crate) const FETCH_END_OF_TIMED_OUT_RANGE: u64 = 0x20C;

/// Fetch ストリームの prior 参照文脈 (draft-ietf-moq-transport-21 §11.4.1.1 (Flags) / §11.4.1.2 (End of Range))
///
/// encode/decode 時に、prior object を参照するフラグの妥当性を検証するために使用する。
/// 呼び出し側がストリーム内のオブジェクト出現状況に応じて適切な値を渡す。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FetchPriorContext {
    /// ストリームの最初のオブジェクト: すべての prior 参照が禁止
    /// (draft-ietf-moq-transport-21 §11.4.1.1 (Flags))
    First,
    /// End of Range の後で actual object が未出現:
    /// prior Subgroup ID / Priority の参照が禁止
    /// (draft-ietf-moq-transport-21 §11.4.1.2 (End of Range): prior Group ID / Object ID は End of Range の値を使う)
    NoPriorActualObject,
    /// prior 参照がすべて有効
    HasPriorObject,
}

/// Subgroup ID のエンコードモード (Fetch ストリーム内)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FetchSubgroupIdMode {
    /// 0b00: Subgroup ID は 0
    Zero,
    /// 0b01: 前のオブジェクトと同じ Subgroup ID
    PreviousSame,
    /// 0b10: 前の Subgroup ID + 1
    PreviousPlusOne,
    /// 0b11: Subgroup ID フィールドあり
    Explicit(u64),
}

/// Fetch ストリーム内のオブジェクトの種別
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FetchStreamEntry {
    /// 通常のオブジェクト
    Object(FetchStreamObject),
    /// End of Non-Existent Range (0x8C)
    /// draft-ietf-moq-transport-21 §11.4.1.2 (End of Range): Group ID と Object ID が必須
    EndOfNonExistentRange {
        /// Group ID
        group_id: u64,
        /// Object ID
        object_id: u64,
    },
    /// End of Unknown Range (0x10C)
    /// draft-ietf-moq-transport-21 §11.4.1.2 (End of Range): Group ID と Object ID が必須
    EndOfUnknownRange {
        /// Group ID
        group_id: u64,
        /// Object ID
        object_id: u64,
    },
    /// End of Timed-Out Range (0x20C)
    /// draft-ietf-moq-transport-21 §11.4.1 (FETCH stream) Table 7: Group ID と Object ID が必須
    EndOfTimedOutRange {
        /// Group ID
        group_id: u64,
        /// Object ID
        object_id: u64,
    },
}

/// Fetch ストリーム内のオブジェクトヘッダ (draft-ietf-moq-transport-21 §11.4.1 (Fetch Header))
///
/// ペイロードデータ自体は含まない。呼び出し元が `payload_length` バイト分のペイロードを
/// 後続のバッファから読み書きする。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FetchStreamObject {
    /// Group ID Delta (None の場合は前のオブジェクトの Group ID を継承)
    ///
    /// draft-ietf-moq-transport-21 §11.4.1.1 (Flags): 値はデルタエンコードされる。
    /// 最初のオブジェクトでは絶対値として解釈される。
    pub group_id: Option<u64>,
    /// Subgroup ID のエンコードモード (変更なし)
    pub subgroup_id: FetchSubgroupIdMode,
    /// Object ID (None の場合は前の Object ID + 1)
    ///
    /// draft-ietf-moq-transport-21 §11.4.1.1 (Flags): Group が変更された場合は絶対値、
    /// 同じ Group 内ではデルタエンコードされる。
    pub object_id: Option<u64>,
    /// Publisher Priority (None の場合は前のオブジェクトの Priority を継承)
    pub publisher_priority: Option<u8>,
    /// Properties フィールドが存在するか
    pub has_properties: bool,
    /// Datagram 起源のオブジェクトか (draft-ietf-moq-transport-21 §11.4.1.1 (Flags): bit 0x40)
    ///
    /// true の場合、元の Forwarding Preference が Datagram であり Subgroup ID を持たない。
    pub is_datagram_origin: bool,
    /// ペイロード長
    pub payload_length: u64,
}

impl FetchStreamEntry {
    /// エントリをエンコードする
    ///
    /// `prior_context` で prior object 参照の妥当性を検証する。
    /// `properties_data` は `has_properties` が true の場合に渡す。
    pub fn encode(
        &self,
        properties_data: Option<&[u8]>,
        prior_context: FetchPriorContext,
        buf: &mut Vec<u8>,
    ) -> Result<(), MessageError> {
        match self {
            Self::EndOfNonExistentRange {
                group_id,
                object_id,
            } => {
                varint::encode(FETCH_END_OF_NON_EXISTENT_RANGE, buf);
                varint::encode(*group_id, buf);
                varint::encode(*object_id, buf);
                Ok(())
            }
            Self::EndOfUnknownRange {
                group_id,
                object_id,
            } => {
                varint::encode(FETCH_END_OF_UNKNOWN_RANGE, buf);
                varint::encode(*group_id, buf);
                varint::encode(*object_id, buf);
                Ok(())
            }
            Self::EndOfTimedOutRange {
                group_id,
                object_id,
            } => {
                varint::encode(FETCH_END_OF_TIMED_OUT_RANGE, buf);
                varint::encode(*group_id, buf);
                varint::encode(*object_id, buf);
                Ok(())
            }
            Self::Object(obj) => obj.encode(properties_data, prior_context, buf),
        }
    }

    /// バッファ先頭からエントリをデコードし `(entry, 消費バイト数)` を返す
    ///
    /// `prior_context` で prior object 参照の妥当性を検証する。
    /// - `First`: すべての prior 参照を禁止 (draft-ietf-moq-transport-21 §11.4.1.1 (Flags))
    /// - `NoPriorActualObject`: prior Subgroup ID / Priority を禁止 (draft-ietf-moq-transport-21 §11.4.1.2 (End of Range))
    /// - `HasPriorObject`: すべての prior 参照を許可
    pub fn decode(
        buf: &[u8],
        prior_context: FetchPriorContext,
    ) -> Result<(Self, usize), MessageError> {
        let (entry, _, consumed) = Self::decode_with_properties(buf, prior_context)?;
        Ok((entry, consumed))
    }

    pub(crate) fn decode_with_properties(
        buf: &[u8],
        prior_context: FetchPriorContext,
    ) -> Result<(Self, Option<Vec<u8>>, usize), MessageError> {
        let mut pos = 0;
        let (flags, n) = varint::decode(&buf[pos..])?;
        pos += n;

        match flags {
            FETCH_END_OF_NON_EXISTENT_RANGE => {
                // draft-ietf-moq-transport-21 §11.4.1.2 (End of Range): Group ID と Object ID が必須
                let (group_id, n) = varint::decode(&buf[pos..])?;
                pos += n;
                let (object_id, n) = varint::decode(&buf[pos..])?;
                pos += n;
                Ok((
                    Self::EndOfNonExistentRange {
                        group_id,
                        object_id,
                    },
                    None,
                    pos,
                ))
            }
            FETCH_END_OF_UNKNOWN_RANGE => {
                // draft-ietf-moq-transport-21 §11.4.1.2 (End of Range): Group ID と Object ID が必須
                let (group_id, n) = varint::decode(&buf[pos..])?;
                pos += n;
                let (object_id, n) = varint::decode(&buf[pos..])?;
                pos += n;
                Ok((
                    Self::EndOfUnknownRange {
                        group_id,
                        object_id,
                    },
                    None,
                    pos,
                ))
            }
            FETCH_END_OF_TIMED_OUT_RANGE => {
                // draft-ietf-moq-transport-21 §11.4.1 (FETCH stream) Table 7: Group ID と Object ID が必須
                let (group_id, n) = varint::decode(&buf[pos..])?;
                pos += n;
                let (object_id, n) = varint::decode(&buf[pos..])?;
                pos += n;
                Ok((
                    Self::EndOfTimedOutRange {
                        group_id,
                        object_id,
                    },
                    None,
                    pos,
                ))
            }
            _ => {
                // Table 7 の 3 値以外の 128 以上は未知値として先に拒否する。
                // prior 文脈違反より未知 flags 自体を優先して報告するのは
                // 実装の診断選択である (いずれも PROTOCOL_VIOLATION であり、
                // draft-ietf-moq-transport-21 §11.4.1 (FETCH stream) は
                // 優先順位を規定しない)。
                if flags >= 128 {
                    return Err(MessageError::ProtocolViolation(
                        "invalid fetch serialization flags",
                    ));
                }
                Self::validate_prior_context(flags, prior_context)?;
                let (obj, properties_bytes, consumed) =
                    FetchStreamObject::decode_after_flags(flags, &buf[pos..])?;
                pos += consumed;
                Ok((Self::Object(obj), properties_bytes, pos))
            }
        }
    }

    /// prior 参照文脈に基づいてフラグの妥当性を検証する
    fn validate_prior_context(
        flags: u64,
        prior_context: FetchPriorContext,
    ) -> Result<(), MessageError> {
        if matches!(prior_context, FetchPriorContext::HasPriorObject) {
            return Ok(());
        }

        let is_datagram_origin = flags & 0x40 != 0;
        // bit 0x40 が立っている場合、下位 2 bit は無視する
        let subgroup_id_mode = if is_datagram_origin {
            0u8
        } else {
            (flags & 0x03) as u8
        };
        let has_priority = flags & 0x10 != 0;

        // prior Subgroup ID 参照の検証 (First / NoPriorActualObject 共通)
        // subgroup_id mode 1 (PreviousSame) または 2 (PreviousPlusOne) は prior 参照
        if subgroup_id_mode == 0x01 || subgroup_id_mode == 0x02 {
            return Err(MessageError::ProtocolViolation(
                "fetch object must not use prior-relative subgroup ID mode without a prior actual object",
            ));
        }
        // prior Priority 参照の検証 (First / NoPriorActualObject 共通)
        // priority 未指定は prior Priority を意味する
        if !has_priority {
            return Err(MessageError::ProtocolViolation(
                "fetch object must include explicit Publisher Priority without a prior actual object",
            ));
        }

        // First ではさらに Group ID / Object ID の prior 参照も禁止
        if matches!(prior_context, FetchPriorContext::First) {
            let has_object_id = flags & 0x04 != 0;
            let has_group_id = flags & 0x08 != 0;

            // object_id 未指定は prior Object ID + 1 を意味する
            if !has_object_id {
                return Err(MessageError::ProtocolViolation(
                    "first fetch object must include explicit Object ID",
                ));
            }
            // group_id 未指定は prior Group ID を意味する
            if !has_group_id {
                return Err(MessageError::ProtocolViolation(
                    "first fetch object must include explicit Group ID",
                ));
            }
        }

        Ok(())
    }
}

impl FetchStreamObject {
    /// オブジェクトヘッダをエンコードする
    ///
    /// `prior_context` で prior object 参照の妥当性を検証する。
    /// `properties_data` は `has_properties` が true の場合に渡す。
    /// プロパティが空でも `has_properties` が true なら Properties Length = 0 を含むデータを渡す必要がある。
    ///
    /// # Errors
    ///
    /// - Datagram 起源なのに Subgroup ID を持つ: `ProtocolViolation`
    /// - `has_properties` と `properties_data` の組み合わせが不正 (true なのに `None` / 空スライス、
    ///   または false なのに `Some`): `ProtocolViolation`
    /// - Properties Length varint が不正、または Properties Length と実データ長が一致しない: `ProtocolViolation`
    /// - `prior_context` に対して prior 参照が不正: `ProtocolViolation`
    pub fn encode(
        &self,
        properties_data: Option<&[u8]>,
        prior_context: FetchPriorContext,
        buf: &mut Vec<u8>,
    ) -> Result<(), MessageError> {
        // draft-ietf-moq-transport-21 §11.4.1.1 (Flags): Datagram 起源のオブジェクトは Subgroup ID を持たない
        if self.is_datagram_origin && !matches!(self.subgroup_id, FetchSubgroupIdMode::Zero) {
            return Err(MessageError::ProtocolViolation(
                "datagram origin object must not have a Subgroup ID",
            ));
        }

        // draft-ietf-moq-transport-21 §11.4.1.1 (Flags): has_properties と properties_data の整合性を検証する
        // 0x20 bit が立っているときだけ Properties フィールドが present であるべき
        if self.has_properties && properties_data.is_none() {
            return Err(MessageError::ProtocolViolation(
                "has_properties is set but properties_data is not provided",
            ));
        }
        // draft-ietf-moq-transport-21 §11.1.3 (Object Properties): Properties は
        // Properties Length (vi64) + Properties の構造であり、空スライスは Length varint を
        // 含まない契約違反入力。呼び出し側は Length = 0 を含むデータ (`&[0x00]` 等) を渡すこと。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        if self.has_properties && properties_data.is_some_and(|data| data.is_empty()) {
            return Err(MessageError::ProtocolViolation(
                "properties data must include Properties Length (empty slice is not allowed)",
            ));
        }
        if !self.has_properties && properties_data.is_some() {
            return Err(MessageError::ProtocolViolation(
                "properties_data is provided but has_properties is not set",
            ));
        }
        // draft-ietf-moq-transport-21 §11.1.3 (Object Properties): Properties Length と
        // 実データ長の一致を書き込み前に検証する
        if let Some(props) = properties_data {
            validate_properties_blob(props)?;
        }

        // prior 参照文脈の検証
        self.validate_encode_prior_context(prior_context)?;

        let mut flags: u64 = 0;

        match &self.subgroup_id {
            FetchSubgroupIdMode::Zero => {}
            FetchSubgroupIdMode::PreviousSame => {
                flags |= 0x01;
            }
            FetchSubgroupIdMode::PreviousPlusOne => {
                flags |= 0x02;
            }
            FetchSubgroupIdMode::Explicit(_) => {
                flags |= 0x03;
            }
        }
        if self.object_id.is_some() {
            flags |= 0x04;
        }
        if self.group_id.is_some() {
            flags |= 0x08;
        }
        if self.publisher_priority.is_some() {
            flags |= 0x10;
        }
        if self.has_properties {
            flags |= 0x20;
        }
        // draft-ietf-moq-transport-21 §11.4.1.1 (Flags): Datagram 起源のオブジェクトは bit 0x40 を設定し、
        // 下位 2 bit を 0 にする
        if self.is_datagram_origin {
            flags |= 0x40;
            flags &= !0x03;
        }

        varint::encode(flags, buf);

        if let Some(group_id) = self.group_id {
            varint::encode(group_id, buf);
        }
        if let FetchSubgroupIdMode::Explicit(id) = &self.subgroup_id {
            varint::encode(*id, buf);
        }
        if let Some(object_id) = self.object_id {
            varint::encode(object_id, buf);
        }
        if let Some(prio) = self.publisher_priority {
            buf.push(prio);
        }
        if let Some(props) = properties_data {
            buf.extend_from_slice(props);
        }

        varint::encode(self.payload_length, buf);

        Ok(())
    }

    /// encode 時の prior 参照文脈を検証する
    fn validate_encode_prior_context(
        &self,
        prior_context: FetchPriorContext,
    ) -> Result<(), MessageError> {
        if matches!(prior_context, FetchPriorContext::HasPriorObject) {
            return Ok(());
        }

        // prior Subgroup ID 参照の検証 (First / NoPriorActualObject 共通)
        if matches!(
            self.subgroup_id,
            FetchSubgroupIdMode::PreviousSame | FetchSubgroupIdMode::PreviousPlusOne
        ) {
            return Err(MessageError::ProtocolViolation(
                "fetch object must not use prior-relative subgroup ID mode without a prior actual object",
            ));
        }
        // prior Priority 参照の検証 (First / NoPriorActualObject 共通)
        if self.publisher_priority.is_none() {
            return Err(MessageError::ProtocolViolation(
                "fetch object must include explicit Publisher Priority without a prior actual object",
            ));
        }

        // First ではさらに Group ID / Object ID の prior 参照も禁止
        if matches!(prior_context, FetchPriorContext::First) {
            if self.object_id.is_none() {
                return Err(MessageError::ProtocolViolation(
                    "first fetch object must include explicit Object ID",
                ));
            }
            if self.group_id.is_none() {
                return Err(MessageError::ProtocolViolation(
                    "first fetch object must include explicit Group ID",
                ));
            }
        }

        Ok(())
    }

    /// Serialization Flags を既にデコード済みの状態からフィールドをデコードする
    ///
    /// draft-ietf-moq-transport-21 §11.4.1 (FETCH stream) Table 7 / §11.4.1.1 (Flags):
    /// 128 未満の全 bit は定義済み (Table 8 の 0x03 / Table 9 の
    /// 0x04 / 0x08 / 0x10 / 0x20 / 0x40) のため mask 検証は no-op であり、
    /// Table 7 の 3 値 (0x8C / 0x10C / 0x20C) 以外の 128 以上は
    /// PROTOCOL_VIOLATION とする ("Any other value is a PROTOCOL_VIOLATION")。
    /// 128 以上の検査は呼び出し側 (`decode_with_properties`) でも行う二重検査で
    /// あり、ここは将来の単独利用に備えた防御である。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    fn decode_after_flags(
        flags: u64,
        buf: &[u8],
    ) -> Result<(Self, Option<Vec<u8>>, usize), MessageError> {
        // flags が 128 以上は通常のフラグではない (特殊値はこのメソッドに来ない)
        if flags >= 128 {
            return Err(MessageError::ProtocolViolation(
                "invalid fetch serialization flags",
            ));
        }

        let mut pos = 0;

        // draft-ietf-moq-transport-21 §11.4.1.1 (Flags): bit 0x40 は Datagram 起源を示す
        // bit 0x40 が立っている場合、下位 2 bit は無視する (MUST)
        let is_datagram_origin = flags & 0x40 != 0;
        let subgroup_id_mode = if is_datagram_origin {
            0u8 // 下位 2 bit を無視し、Subgroup ID は Zero として扱う
        } else {
            (flags & 0x03) as u8
        };
        let has_object_id = flags & 0x04 != 0;
        let has_group_id = flags & 0x08 != 0;
        let has_priority = flags & 0x10 != 0;
        let has_properties = flags & 0x20 != 0;

        let group_id = if has_group_id {
            let (id, n) = varint::decode(&buf[pos..])?;
            pos += n;
            Some(id)
        } else {
            None
        };

        let subgroup_id = match subgroup_id_mode {
            0b00 => FetchSubgroupIdMode::Zero,
            0b01 => FetchSubgroupIdMode::PreviousSame,
            0b10 => FetchSubgroupIdMode::PreviousPlusOne,
            0b11 => {
                let (id, n) = varint::decode(&buf[pos..])?;
                pos += n;
                FetchSubgroupIdMode::Explicit(id)
            }
            _ => {
                unreachable!("subgroup_id_mode is a 2-bit value and all four patterns are covered")
            }
        };

        let object_id = if has_object_id {
            let (id, n) = varint::decode(&buf[pos..])?;
            pos += n;
            Some(id)
        } else {
            None
        };

        let publisher_priority = if has_priority {
            if buf[pos..].is_empty() {
                return Err(MessageError::UnexpectedEof);
            }
            let prio = buf[pos];
            pos += 1;
            Some(prio)
        } else {
            None
        };

        // Properties をスキップする
        // draft-ietf-moq-transport-21 §11.4.1 (Fetch Header): Fetch object でも Properties Length = 0 は合法
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

        Ok((
            Self {
                group_id,
                subgroup_id,
                object_id,
                publisher_priority,
                has_properties,
                is_datagram_origin,
                payload_length,
            },
            properties_bytes,
            pos,
        ))
    }
}
