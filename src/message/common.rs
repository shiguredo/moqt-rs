//! Control Message 共通のワイヤ型 (draft-ietf-moq-transport-21 §8.2 (Location Structure) / §8.7 (Track Namespace Structure))
//!
//! `TrackNamespace` / `Location` は `message.rs` と `message_parameter.rs` の両方から使用される。
//! 従来 `message.rs` に定義し `message_parameter.rs` から import する構成だったため、
//! 両モジュールの間で循環依存が発生していた。本モジュールへ分離することで
//! 依存方向を一方向 (`message_parameter` → `common`) に統一する。
//!
//! ワイヤフォーマットの節番号・規則は draft 由来であり、将来の draft 改版で変わる可能性がある。

use crate::{error::MessageError, varint};
use alloc::vec::Vec;

/// Full Track Name / Track Namespace の最大バイト長 (draft-ietf-moq-transport-21 §8.7 (Track Namespace Structure))
pub(crate) const MAX_TRACK_NAME_LENGTH: usize = 4096;

/// トラック名前空間 (0–32 フィールド、各フィールドは 1 バイト以上)
///
/// draft-ietf-moq-transport-21 §2.4.1 (Track Naming):
/// "Track Namespace is an ordered set of between 0 and 32 Track Namespace Fields"
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackNamespace(Vec<Vec<u8>>);

impl TrackNamespace {
    /// フィールドリストから TrackNamespace を作成する
    ///
    /// フィールド数の下限は 0、上限は 32 (draft-ietf-moq-transport-21 §2.4.1 (Track Naming))。
    ///
    /// # Errors
    ///
    /// フィールド数が 32 超、0 バイトのフィールドが含まれる、または合計バイト長が 4096 を超える場合
    pub fn new(fields: Vec<Vec<u8>>) -> Result<Self, MessageError> {
        if fields.len() > 32 {
            return Err(MessageError::ProtocolViolation(
                "track namespace field count exceeds 32",
            ));
        }
        for f in &fields {
            if f.is_empty() {
                return Err(MessageError::ProtocolViolation(
                    "track namespace field length is 0",
                ));
            }
        }
        let ns = Self(fields);
        // draft-ietf-moq-transport-21 §8.7 (Track Namespace Structure): Track Namespace が 4096 バイトを超えたら
        // PROTOCOL_VIOLATION
        if ns.byte_length() > MAX_TRACK_NAME_LENGTH {
            return Err(MessageError::ProtocolViolation(
                "track namespace exceeds 4096 bytes",
            ));
        }
        Ok(ns)
    }

    /// 名前空間フィールド一覧の参照を返す
    pub fn fields(&self) -> &[Vec<u8>] {
        &self.0
    }

    /// セッションレベルトラック用の予約名前空間 `.session` かを返す (draft-ietf-moq-transport-21 §6.5 (Session-Level Tracks and Namespaces))
    ///
    /// 最初の名前空間フィールドが b".session" (8 バイト) と一致する場合に true
    pub fn is_session_level(&self) -> bool {
        self.0.first().is_some_and(|f| f == b".session")
    }

    /// 予約名前空間の single period `.` かを返す (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces))
    ///
    /// 最初の名前空間フィールドが b"." (1 バイトの single period, 0x2e) と一致する場合に true。
    /// draft-21 §2.4.2: "A Track Namespace whose first field is exactly . [...] is reserved
    /// and MUST NOT be used for any purpose"
    pub fn is_single_period(&self) -> bool {
        self.0.first().is_some_and(|f| f == b".")
    }

    /// 全フィールドの値バイト数の合計を返す
    ///
    /// draft-ietf-moq-transport-21 §8.7 (Track Namespace Structure): "The length of a Track
    /// Namespace is the sum of the Track Namespace Field Length fields."
    pub fn byte_length(&self) -> usize {
        self.0.iter().map(|f| f.len()).sum()
    }

    /// バッファにエンコードする
    ///
    /// draft-ietf-moq-transport-21 §8.7 (Track Namespace Structure): Track Namespace が 4096 バイトを超えたら
    /// PROTOCOL_VIOLATION
    pub(crate) fn encode_to(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        if self.byte_length() > MAX_TRACK_NAME_LENGTH {
            return Err(MessageError::ProtocolViolation(
                "track namespace exceeds 4096 bytes",
            ));
        }
        varint::encode(self.0.len() as u64, buf);
        for field in &self.0 {
            varint::encode(field.len() as u64, buf);
            buf.extend_from_slice(field);
        }
        Ok(())
    }

    /// バッファの `pos` 位置からデコードする (0–32 フィールド)
    pub(crate) fn decode_from(buf: &[u8], pos: &mut usize) -> Result<Self, MessageError> {
        let (count, n) = varint::decode(&buf[*pos..])?;
        *pos += n;
        if count > 32 {
            return Err(MessageError::ProtocolViolation(
                "track namespace field count exceeds 32",
            ));
        }
        let mut fields = Vec::new();
        for _ in 0..count {
            let (len, n) = varint::decode(&buf[*pos..])?;
            *pos += n;
            if len == 0 {
                return Err(MessageError::ProtocolViolation(
                    "track namespace field length is 0",
                ));
            }
            let len = varint::checked_len(len, buf[*pos..].len())?;
            fields.push(buf[*pos..*pos + len].to_vec());
            *pos += len;
        }
        let ns = Self(fields);
        // draft-ietf-moq-transport-21 §8.7 (Track Namespace Structure): Track Namespace が 4096 バイトを超えたら
        // PROTOCOL_VIOLATION
        if ns.byte_length() > MAX_TRACK_NAME_LENGTH {
            return Err(MessageError::ProtocolViolation(
                "track namespace exceeds 4096 bytes",
            ));
        }
        Ok(ns)
    }
}

/// Full Track Name (Track Namespace + Track Name) の長さ制限を検証する
///
/// draft-ietf-moq-transport-21 §8.7 (Track Namespace Structure): Full Track Name の合計バイト長は 4096 バイトまで
pub(crate) fn validate_full_track_name(
    ns: &TrackNamespace,
    track_name: &[u8],
) -> Result<(), MessageError> {
    if ns.byte_length() + track_name.len() > MAX_TRACK_NAME_LENGTH {
        return Err(MessageError::ProtocolViolation(
            "full track name exceeds 4096 bytes",
        ));
    }
    Ok(())
}

/// length-prefixed な Track Name (vi64 length + bytes) をバッファにエンコードする
///
/// Track Name は Track Namespace の後に Length (vi64) + バイト列として現れる
/// (draft-ietf-moq-transport-21 §8.7 (Track Namespace Structure))。
pub(crate) fn encode_track_name(track_name: &[u8], buf: &mut Vec<u8>) {
    varint::encode(track_name.len() as u64, buf);
    buf.extend_from_slice(track_name);
}

/// length-prefixed な Track Name (vi64 length + bytes) をバッファの `pos` 位置からデコードする
pub(crate) fn decode_track_name(buf: &[u8], pos: &mut usize) -> Result<Vec<u8>, MessageError> {
    let (len, n) = varint::decode(&buf[*pos..])?;
    *pos += n;
    let len = varint::checked_len(len, buf[*pos..].len())?;
    let track_name = buf[*pos..*pos + len].to_vec();
    *pos += len;
    Ok(track_name)
}

/// グループ内のオブジェクト位置 (draft-ietf-moq-transport-21 §8.2 (Location Structure))
///
/// 比較は (group_id, object_id) の辞書順で行う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Location {
    /// グループ ID (draft-ietf-moq-transport-21 §8.2 (Location Structure))
    pub group_id: u64,
    /// オブジェクト ID (draft-ietf-moq-transport-21 §8.2 (Location Structure))
    pub object_id: u64,
}

impl Location {
    /// バッファにエンコードする (Group (vi64) + Object (vi64))
    pub(crate) fn encode_to(&self, buf: &mut Vec<u8>) {
        varint::encode(self.group_id, buf);
        varint::encode(self.object_id, buf);
    }

    /// バッファの `pos` 位置からデコードする (Group (vi64) + Object (vi64))
    pub(crate) fn decode_from(buf: &[u8], pos: &mut usize) -> Result<Self, MessageError> {
        let (group_id, n) = varint::decode(&buf[*pos..])?;
        *pos += n;
        let (object_id, n) = varint::decode(&buf[*pos..])?;
        *pos += n;
        Ok(Self {
            group_id,
            object_id,
        })
    }
}
