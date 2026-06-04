//! Fetch データストリーム用エンコーダー (sans I/O)
//!
//! 呼び出し側が絶対値の group_id / subgroup_id / object_id を渡し、
//! エンコーダが内部でデルタ圧縮の判断と FetchPriorContext の状態遷移を行う。
//!
//! このモジュールは I/O を持たず、バイト列生成とプロトコル状態管理のみを提供する。
//! ペイロードの書き込みは呼び出し側が行う。
use super::fetch::{
    FetchHeader, FetchPriorContext, FetchStreamEntry, FetchStreamObject, FetchSubgroupIdMode,
};
use crate::error::MessageError;
use alloc::vec::Vec;

/// FetchStreamEncoder に渡すオブジェクト情報
///
/// 呼び出し側は絶対値の group_id / subgroup_id / object_id を指定する。
/// デルタ圧縮の判断はエンコーダが行う。
#[derive(Debug, Clone)]
pub struct FetchObjectInput {
    /// 絶対 Group ID
    pub group_id: u64,
    /// 絶対 Subgroup ID
    pub subgroup_id: u64,
    /// 絶対 Object ID
    pub object_id: u64,
    /// Publisher Priority
    pub publisher_priority: u8,
    /// Properties フィールドが存在するか
    pub has_properties: bool,
    /// Datagram 起源のオブジェクトか
    pub is_datagram_origin: bool,
    /// ペイロード長
    pub payload_length: u64,
}

/// エンコード時の前回のオブジェクト情報
#[derive(Debug, Clone, Copy)]
struct PriorEncodeState {
    group_id: u64,
    subgroup_id: u64,
    object_id: u64,
    publisher_priority: u8,
    is_datagram_origin: bool,
}

/// Fetch レスポンスストリーム用エンコーダー (sans I/O)
///
/// `FetchPriorContext` の状態遷移を内部で自動管理し、
/// 絶対値の group_id / subgroup_id / object_id からデルタ圧縮を判断する。
///
/// ペイロードはエンコーダの責務外。エンコード結果のバイト列の後に
/// 呼び出し側がペイロードを結合する。
///
/// # 使い方 (検証対象外の疑似コード)
///
/// ```text
/// let mut encoder = FetchStreamEncoder::new(request_id);
/// let header_bytes = encoder.encode_header();
/// // header_bytes を送信する
///
/// for obj in objects {
///     let mut buf = Vec::new();
///     // input.has_properties が true の場合のみ properties_data を渡す
///     let properties = obj.has_properties.then_some(properties_data);
///     encoder.encode_object(&obj, properties, &mut buf)?;
///     buf.extend_from_slice(&payload);
///     // buf を送信する
/// }
/// ```
pub struct FetchStreamEncoder {
    request_id: u64,
    group_order: u8,
    prior_context: FetchPriorContext,
    prior_state: Option<PriorEncodeState>,
}

/// FETCH response の Group Order: Ascending (draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter))
const FETCH_GROUP_ORDER_ASCENDING: u8 = 0x01;
/// FETCH response の Group Order: Descending (draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter))
const FETCH_GROUP_ORDER_DESCENDING: u8 = 0x02;

impl FetchStreamEncoder {
    /// 新しいエンコーダーを作成する (group_order は Ascending がデフォルト)
    pub fn new(request_id: u64) -> Self {
        Self::with_group_order(request_id, FETCH_GROUP_ORDER_ASCENDING)
    }

    /// Group Order を指定して新しいエンコーダーを作成する
    ///
    /// `group_order` は Ascending (0x01) または Descending (0x02) のいずれか
    /// (draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter))。
    /// それ以外の値は `ProtocolViolation` を返す。
    /// `FetchStreamDecoder::new_with_group_order` と対称の公開 API である。
    pub fn new_with_group_order(request_id: u64, group_order: u8) -> Result<Self, MessageError> {
        validate_fetch_group_order(group_order)?;
        Ok(Self::with_group_order(request_id, group_order))
    }

    fn with_group_order(request_id: u64, group_order: u8) -> Self {
        Self {
            request_id,
            group_order,
            prior_context: FetchPriorContext::First,
            prior_state: None,
        }
    }

    /// FetchHeader をエンコードする
    pub fn encode_header(&self) -> Vec<u8> {
        let header = FetchHeader {
            request_id: self.request_id,
        };
        header.encode()
    }

    /// オブジェクトをエンコードする (draft-ietf-moq-transport-21 §11.4.1.1 (Flags): delta encoding)
    ///
    /// 絶対値の group_id / subgroup_id / object_id から
    /// デルタ圧縮を判断し、FetchStreamObject をエンコードする。
    /// `properties_data` は `input.has_properties` が true の場合に渡す。
    pub fn encode_object(
        &mut self,
        input: &FetchObjectInput,
        properties_data: Option<&[u8]>,
        buf: &mut Vec<u8>,
    ) -> Result<(), MessageError> {
        let (group_id, subgroup_id, object_id, publisher_priority) = match self.prior_state {
            None => {
                // 最初のオブジェクト: 全フィールド絶対値で明示必須
                (
                    Some(input.group_id),
                    FetchSubgroupIdMode::Explicit(input.subgroup_id),
                    Some(input.object_id),
                    Some(input.publisher_priority),
                )
            }
            Some(prior) => {
                if input.group_id == prior.group_id && input.object_id <= prior.object_id {
                    return Err(MessageError::ProtocolViolation(
                        "FETCH response objects within the same group must be strictly increasing by object ID",
                    ));
                }
                if input.group_id == prior.group_id
                    && input.subgroup_id == prior.subgroup_id
                    && matches!(self.prior_context, FetchPriorContext::HasPriorObject)
                    && !input.is_datagram_origin
                    && !prior.is_datagram_origin
                    && input.publisher_priority != prior.publisher_priority
                {
                    return Err(MessageError::ProtocolViolation(
                        "publisher priority must not change within the same subgroup in a FETCH response",
                    ));
                }

                let can_use_subgroup_prior =
                    matches!(self.prior_context, FetchPriorContext::HasPriorObject);
                let can_use_priority_prior =
                    matches!(self.prior_context, FetchPriorContext::HasPriorObject);

                let group_changed = input.group_id != prior.group_id;
                let subgroup_changed = group_changed || input.subgroup_id != prior.subgroup_id;

                // group_id: 変更時にデルタ値を計算 (draft-ietf-moq-transport-21 §11.4.1.1 (Flags))
                let group_id = if group_changed {
                    let delta = match self.group_order {
                        FETCH_GROUP_ORDER_ASCENDING => input
                            .group_id
                            .checked_sub(prior.group_id)
                            .and_then(|v| v.checked_sub(1))
                            .ok_or({
                                MessageError::ProtocolViolation(
                                    "fetch group ID delta underflow in ascending order",
                                )
                            })?,
                        FETCH_GROUP_ORDER_DESCENDING => prior
                            .group_id
                            .checked_sub(input.group_id)
                            .and_then(|v| v.checked_sub(1))
                            .ok_or({
                                MessageError::ProtocolViolation(
                                    "fetch group ID delta underflow in descending order",
                                )
                            })?,
                        _ => unreachable!(
                            "group_order is ASCENDING or DESCENDING (draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter))"
                        ),
                    };
                    Some(delta)
                } else {
                    None
                };

                let subgroup_id = if input.is_datagram_origin {
                    FetchSubgroupIdMode::Zero
                } else if !subgroup_changed && can_use_subgroup_prior {
                    FetchSubgroupIdMode::PreviousSame
                } else {
                    FetchSubgroupIdMode::Explicit(input.subgroup_id)
                };

                // object_id: Group 変更時は絶対値、同 Group ならデルタ値 (draft-ietf-moq-transport-21 §11.4.1.1 (Flags))
                // subgroup 変更時は Group ID Delta が absent のため delta 解釈になる。
                // 絶対値を乗せると decoder 側で delta として誤解釈される (draft-21 §11.4.1.1:
                // "When the Group ID Delta field is not present, the Object ID is the prior
                // Object's ID plus the Object ID Delta if present")。
                // 注: Subgroup (§11.3.1) と異なり Fetch の Object ID Delta に +1/-1 は付かない
                let object_id = if group_changed {
                    Some(input.object_id)
                } else if prior.object_id.checked_add(1) != Some(input.object_id) {
                    // delta = object_id - prior (Subgroup と異なり -1 しない)
                    let delta = input.object_id.checked_sub(prior.object_id).ok_or({
                        MessageError::ProtocolViolation("fetch object ID delta underflow")
                    })?;
                    Some(delta)
                } else {
                    // prior + 1 は absent で表現 (draft-21 §11.4.1.1: "If Object ID Delta
                    // is not present, the Object ID is the prior Object's ID plus one")
                    None
                };

                let publisher_priority = if !can_use_priority_prior
                    || input.publisher_priority != prior.publisher_priority
                {
                    Some(input.publisher_priority)
                } else {
                    None
                };

                (group_id, subgroup_id, object_id, publisher_priority)
            }
        };

        let fetch_obj = FetchStreamObject {
            group_id,
            subgroup_id,
            object_id,
            publisher_priority,
            has_properties: input.has_properties,
            is_datagram_origin: input.is_datagram_origin,
            payload_length: input.payload_length,
        };

        let entry = FetchStreamEntry::Object(fetch_obj);
        entry.encode(properties_data, self.prior_context, buf)?;

        // 状態を更新する
        self.prior_context = FetchPriorContext::HasPriorObject;
        self.prior_state = Some(PriorEncodeState {
            group_id: input.group_id,
            subgroup_id: input.subgroup_id,
            object_id: input.object_id,
            publisher_priority: input.publisher_priority,
            is_datagram_origin: input.is_datagram_origin,
        });

        Ok(())
    }

    /// End of Non-Existent Range をエンコードする
    pub fn encode_end_of_non_existent_range(
        &mut self,
        group_id: u64,
        object_id: u64,
        buf: &mut Vec<u8>,
    ) -> Result<(), MessageError> {
        let entry = FetchStreamEntry::EndOfNonExistentRange {
            group_id,
            object_id,
        };
        entry.encode(None, self.prior_context, buf)?;

        self.update_prior_for_end_of_range(group_id, object_id);
        if matches!(self.prior_context, FetchPriorContext::First) {
            self.prior_context = FetchPriorContext::NoPriorActualObject;
        }

        Ok(())
    }

    /// End of Unknown Range をエンコードする
    pub fn encode_end_of_unknown_range(
        &mut self,
        group_id: u64,
        object_id: u64,
        buf: &mut Vec<u8>,
    ) -> Result<(), MessageError> {
        let entry = FetchStreamEntry::EndOfUnknownRange {
            group_id,
            object_id,
        };
        entry.encode(None, self.prior_context, buf)?;

        self.update_prior_for_end_of_range(group_id, object_id);
        if matches!(self.prior_context, FetchPriorContext::First) {
            self.prior_context = FetchPriorContext::NoPriorActualObject;
        }

        Ok(())
    }

    /// End of Timed-Out Range をエンコードする (draft-ietf-moq-transport-21 §11.4.1 Table 7)
    pub fn encode_end_of_timed_out_range(
        &mut self,
        group_id: u64,
        object_id: u64,
        buf: &mut Vec<u8>,
    ) -> Result<(), MessageError> {
        let entry = FetchStreamEntry::EndOfTimedOutRange {
            group_id,
            object_id,
        };
        entry.encode(None, self.prior_context, buf)?;

        self.update_prior_for_end_of_range(group_id, object_id);
        if matches!(self.prior_context, FetchPriorContext::First) {
            self.prior_context = FetchPriorContext::NoPriorActualObject;
        }

        Ok(())
    }

    /// End of Range エントリ後に prior_state を更新する
    fn update_prior_for_end_of_range(&mut self, group_id: u64, object_id: u64) {
        match &mut self.prior_state {
            Some(prior) => {
                prior.group_id = group_id;
                prior.object_id = object_id;
            }
            None => {
                self.prior_state = Some(PriorEncodeState {
                    group_id,
                    subgroup_id: 0,
                    object_id,
                    publisher_priority: 0,
                    is_datagram_origin: false,
                });
            }
        }
    }
}

/// FETCH response の Group Order が Ascending (0x01) または Descending (0x02) か検証する
///
/// draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter):
/// 値域外は `ProtocolViolation` とする。
fn validate_fetch_group_order(group_order: u8) -> Result<(), MessageError> {
    match group_order {
        FETCH_GROUP_ORDER_ASCENDING | FETCH_GROUP_ORDER_DESCENDING => Ok(()),
        _ => Err(MessageError::ProtocolViolation(
            "FETCH group order must be 1 (Ascending) or 2 (Descending)",
        )),
    }
}
