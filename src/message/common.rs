//! Control Message 共通のワイヤ型 (draft-ietf-moq-transport-23 §8.2 (Location Structure) / §8.7 (Track Namespace Structure))
//!
//! `TrackNamespace` / `Location` は `message.rs` と `message_parameter.rs` の両方から使用される。
//! 従来 `message.rs` に定義し `message_parameter.rs` から import する構成だったため、
//! 両モジュールの間で循環依存が発生していた。本モジュールへ分離することで
//! 依存方向を一方向 (`message_parameter` → `common`) に統一する。
//!
//! ワイヤフォーマットの節番号・規則は draft 由来であり、将来の draft 改版で変わる可能性がある。

use crate::{error::MessageError, varint};
use alloc::vec::Vec;

/// Full Track Name / Track Namespace の最大バイト長 (draft-ietf-moq-transport-23 §8.7 (Track Namespace Structure))
pub(crate) const MAX_TRACK_NAME_LENGTH: usize = 4096;

/// トラック名前空間 (0–32 フィールド、各フィールドは 1 バイト以上)
///
/// draft-ietf-moq-transport-22 §2.4.1 (Track Naming):
/// "Track Namespace is an ordered set of between 0 and 32 Track Namespace Fields"
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackNamespace(Vec<Vec<u8>>);

impl TrackNamespace {
    /// フィールドリストから TrackNamespace を作成する
    ///
    /// フィールド数の下限は 0、上限は 32 (draft-ietf-moq-transport-22 §2.4.1 (Track Naming))。
    /// 各フィールドは 1 バイト以上であり、合計 4,096 バイト以下でなければならない
    /// (draft-ietf-moq-transport-23 §8.7 (Track Namespace Structure))。
    ///
    /// # Errors
    ///
    /// - 合計バイト長が 4,096 を超える: `FieldLengthExceeded`
    /// - フィールド数が 32 超、または 0 バイトのフィールドが含まれる: `InvalidTrackNamespace`
    ///
    /// 長さ超過と構造違反が同時に該当する場合は、draft-ietf-moq-transport-23 §12.2
    /// (Session Termination Codes) の優先規則に従い `FieldLengthExceeded` を優先する。
    pub fn new(fields: Vec<Vec<u8>>) -> Result<Self, MessageError> {
        let ns = Self(fields);
        if ns.byte_length() > MAX_TRACK_NAME_LENGTH {
            return Err(MessageError::FieldLengthExceeded(
                "track namespace exceeds 4096 bytes",
            ));
        }
        if ns.0.len() > 32 {
            return Err(MessageError::InvalidTrackNamespace(
                "track namespace field count exceeds 32",
            ));
        }
        if ns.0.iter().any(|f| f.is_empty()) {
            return Err(MessageError::InvalidTrackNamespace(
                "track namespace field length is 0",
            ));
        }
        Ok(ns)
    }

    /// 名前空間フィールド一覧の参照を返す
    pub fn fields(&self) -> &[Vec<u8>] {
        &self.0
    }

    /// セッションレベルトラック用の予約名前空間 `.session` かを返す (draft-ietf-moq-transport-22 §6.5 (Session-Level Tracks and Namespaces))
    ///
    /// 最初の名前空間フィールドが b".session" (8 バイト) と一致する場合に true
    pub fn is_session_level(&self) -> bool {
        self.0.first().is_some_and(|f| f == b".session")
    }

    /// 予約名前空間の single period `.` かを返す (draft-ietf-moq-transport-22 §2.4.3 (Reserved Namespaces))
    ///
    /// 最初の名前空間フィールドが b"." (1 バイトの single period, 0x2e) と一致する場合に true。
    /// draft-ietf-moq-transport-22 §2.4.3: "A Track Namespace whose first field is exactly . [...] is reserved
    /// and MUST NOT be used for any purpose"
    pub fn is_single_period(&self) -> bool {
        self.0.first().is_some_and(|f| f == b".")
    }

    /// 全フィールドの値バイト数の合計を返す
    ///
    /// draft-ietf-moq-transport-23 §8.7 (Track Namespace Structure): "The length of a Track
    /// Namespace is the sum of the Track Namespace Field Length fields."
    pub fn byte_length(&self) -> usize {
        self.0.iter().map(|f| f.len()).sum()
    }

    /// バッファにエンコードする
    ///
    /// draft-ietf-moq-transport-23 §8.7 (Track Namespace Structure): Track Namespace が
    /// 4096 バイトを超えたら FIELD_LENGTH_EXCEEDED (0x401) でセッションを閉じる (MUST)。
    pub(crate) fn encode_to(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        if self.byte_length() > MAX_TRACK_NAME_LENGTH {
            return Err(MessageError::FieldLengthExceeded(
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
    ///
    /// draft-ietf-moq-transport-23 §8.7 (Track Namespace Structure): 長さ 0 の Track
    /// Namespace Field と 32 を超えるフィールド数は INVALID_TRACK_NAMESPACE (0x302)、
    /// 4,096 バイトを超える Track Namespace は FIELD_LENGTH_EXCEEDED (0x401) で
    /// セッションを閉じる (MUST)。
    ///
    /// 長さ超過と構造違反が同時に該当する場合は、§12.2 (Session Termination Codes) の
    /// 優先規則に従い、フィールドを最後まで読んで総バイト長を確定させてから長さを先に
    /// 判定する (フィールド数の早期拒否は優先規則を破るため行わない)。
    pub(crate) fn decode_from(buf: &[u8], pos: &mut usize) -> Result<Self, MessageError> {
        let (count, n) = varint::decode(&buf[*pos..])?;
        *pos += n;
        let mut fields = Vec::new();
        for _ in 0..count {
            let (len, n) = varint::decode(&buf[*pos..])?;
            *pos += n;
            // 長さ 0 のフィールドは構造違反だが、ここでは読むだけに留める。
            // 総バイト長の判定を先に行うため、早期拒否はしない。
            let len = varint::checked_len(len, buf[*pos..].len())?;
            fields.push(buf[*pos..*pos + len].to_vec());
            *pos += len;
        }
        let ns = Self(fields);
        if ns.byte_length() > MAX_TRACK_NAME_LENGTH {
            return Err(MessageError::FieldLengthExceeded(
                "track namespace exceeds 4096 bytes",
            ));
        }
        if ns.0.len() > 32 {
            return Err(MessageError::InvalidTrackNamespace(
                "track namespace field count exceeds 32",
            ));
        }
        if ns.0.iter().any(|f| f.is_empty()) {
            return Err(MessageError::InvalidTrackNamespace(
                "track namespace field length is 0",
            ));
        }
        Ok(ns)
    }
}

/// Full Track Name (Track Namespace + Track Name) の長さ制限を検証する
///
/// draft-ietf-moq-transport-23 §8.7 (Track Namespace Structure): Full Track Name の合計
/// バイト長は 4,096 バイトまでであり、超過は FIELD_LENGTH_EXCEEDED (0x401) で閉じる (MUST)。
pub(crate) fn validate_full_track_name(
    ns: &TrackNamespace,
    track_name: &[u8],
) -> Result<(), MessageError> {
    if ns.byte_length() + track_name.len() > MAX_TRACK_NAME_LENGTH {
        return Err(MessageError::FieldLengthExceeded(
            "full track name exceeds 4096 bytes",
        ));
    }
    Ok(())
}

/// length-prefixed な Track Name (vi64 length + bytes) をバッファにエンコードする
///
/// Track Name は Track Namespace の後に Length (vi64) + バイト列として現れる
/// (draft-ietf-moq-transport-23 §8.7 (Track Namespace Structure))。
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

/// グループ内のオブジェクト位置 (draft-ietf-moq-transport-23 §8.2 (Location Structure))
///
/// 比較は (group_id, object_id) の辞書順で行う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Location {
    /// グループ ID (draft-ietf-moq-transport-23 §8.2 (Location Structure))
    pub group_id: u64,
    /// オブジェクト ID (draft-ietf-moq-transport-23 §8.2 (Location Structure))
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
