//! Fetch データストリーム用エンコーダー (sans I/O)
//!
//! 呼び出し側が絶対値の group_id / subgroup_id / object_id を渡し、
//! エンコーダが内部でデルタ圧縮の判断と FetchPriorContext の状態遷移を行う。
//!
//! End of Range (draft-ietf-moq-transport-23 §11.4.2 (End of Range)) も絶対 Location を
//! 受け取り、内部で Group ID Delta / Object ID Delta に変換する (§11.4.2.1 (ID Decoding))。
//! 低水準 codec が wire の生値を扱うのに対し、このモジュールは常に絶対 Location を扱う。
//!
//! このモジュールは I/O を持たず、バイト列生成とプロトコル状態管理のみを提供する。
//! ペイロードの書き込みは呼び出し側が行う。
use super::fetch::{
    FetchHeader, FetchPriorContext, FetchStreamEntry, FetchStreamObject, FetchSubgroupIdMode,
};
use crate::error::MessageError;
use alloc::vec::Vec;
use hashbrown::HashMap;

/// FetchStreamEncoder に渡すオブジェクト情報
///
/// 呼び出し側は絶対値の group_id / subgroup_id / object_id を指定する。
/// デルタ圧縮の判断はエンコーダが行う。
#[derive(Debug, Clone)]
pub struct FetchObjectInput {
    /// 絶対 Group ID
    pub group_id: u64,
    /// 絶対 Subgroup ID
    ///
    /// `is_datagram_origin` が true のときは wire に載らず 0 として扱われる
    /// (draft-ietf-moq-transport-22 §11.4.1.1 (Flags): Datagram 起源の Object は Subgroup ID を持たない)。
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

/// エンコード時の直前の actual Object のフィールド
///
/// End of Range は Group ID / Object ID だけを更新し、この値を保持する
/// (draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields))。
/// Datagram 起源の Object は Subgroup ID を運ばずデコーダでも 0 に解決されるため、
/// ここでも 0 を保持する。
#[derive(Debug, Clone, Copy)]
struct PriorActualObject {
    subgroup_id: u64,
    publisher_priority: u8,
    is_datagram_origin: bool,
}

/// エンコード時の直前状態
///
/// `group_id` / `object_id` は直前の Object または End of Range の Location であり、
/// デルタ圧縮の基準になる。`actual_object` は直前の actual Object のフィールドであり、
/// End of Range では更新しない。End of Range だけが先頭に現れた場合 `None` のままになり、
/// その状態で prior Subgroup ID / Priority を参照する Object は低水準 codec が拒否する。
#[derive(Debug, Clone, Copy)]
struct PriorEncodeState {
    group_id: u64,
    object_id: u64,
    actual_object: Option<PriorActualObject>,
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
    /// Subgroup ごとの最後の Publisher Priority
    ///
    /// draft-ietf-moq-transport-22 §12.1 (Malformed Tracks) 条件 1:
    /// "An Object with a particular Subgroup ID is received, but its Publisher
    ///  Priority is different from that of the previous Object with the same
    ///  Subgroup ID."
    /// 判定は「同一 Subgroup の直前の Object」との比較であり、間に別 Subgroup や
    /// datagram 起源 Object が挟まっても成立する。`FetchStreamDecoder` と同じ粒度で
    /// 検証するため `(group_id, subgroup_id)` ごとの最後の Priority を保持する。
    /// datagram 起源 Object は Subgroup ID を持たないため記録しない。
    subgroup_priorities: HashMap<(u64, u64), u8>,
}

/// FETCH response の Group Order: Ascending (draft-ietf-moq-transport-22 §9.20.8 (GROUP ORDER Parameter))
const FETCH_GROUP_ORDER_ASCENDING: u8 = 0x01;
/// FETCH response の Group Order: Descending (draft-ietf-moq-transport-22 §9.20.8 (GROUP ORDER Parameter))
const FETCH_GROUP_ORDER_DESCENDING: u8 = 0x02;

impl FetchStreamEncoder {
    /// 新しいエンコーダーを作成する (group_order は Ascending がデフォルト)
    pub fn new(request_id: u64) -> Self {
        Self::with_group_order(request_id, FETCH_GROUP_ORDER_ASCENDING)
    }

    /// Group Order を指定して新しいエンコーダーを作成する
    ///
    /// `group_order` は Ascending (0x01) または Descending (0x02) のいずれか
    /// (draft-ietf-moq-transport-22 §9.20.8 (GROUP ORDER Parameter))。
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
            subgroup_priorities: HashMap::new(),
        }
    }

    /// FetchHeader をエンコードする
    pub fn encode_header(&self) -> Vec<u8> {
        let header = FetchHeader {
            request_id: self.request_id,
        };
        header.encode()
    }

    /// オブジェクトをエンコードする (draft-ietf-moq-transport-22 §11.4.1.1 (Flags): delta encoding)
    ///
    /// 絶対値の group_id / subgroup_id / object_id から
    /// デルタ圧縮を判断し、FetchStreamObject をエンコードする。
    ///
    /// `properties_data` は Properties Length varint を含む生バイト列である
    /// (draft-ietf-moq-transport-22 §11.1.2 (Object Properties))。
    /// `input.has_properties` が true の場合に渡し、プロパティが空でも
    /// Properties Length = 0 を含むデータ (`&[0x00]` など) を渡す。
    /// 空スライス (`Some(&[])`) は Properties Length varint を含まない契約違反入力である。
    /// `input.has_properties` が false の場合は `None` を渡す。
    ///
    /// # Errors
    ///
    /// - 直前のオブジェクトと同じ Group ID で Object ID が増加していない: `ProtocolViolation`
    /// - Group Order (Ascending / Descending) と逆方向に Group ID が変化し、
    ///   Group ID Delta がアンダーフローする: `ProtocolViolation`
    /// - 直前と同じ (Group ID, Subgroup ID) で Publisher Priority が変化した: `ProtocolViolation`
    ///   (draft-ietf-moq-transport-22 §12.1 (Malformed Tracks) 条件 1)
    /// - その他 [`FetchStreamObject::encode`] が返す `ProtocolViolation`
    ///   (Datagram 起源なのに Subgroup ID を持つ、`has_properties` と `properties_data` の
    ///   組み合わせ不正、Properties Length varint 不正・長さ不一致、prior 参照不正)
    ///
    /// エンコードに成功した場合、この呼び出しは内部の prior 状態
    /// (直前の Group ID / Subgroup ID / Object ID / Priority と prior 参照文脈) を更新する。
    pub fn encode_object(
        &mut self,
        input: &FetchObjectInput,
        properties_data: Option<&[u8]>,
        buf: &mut Vec<u8>,
    ) -> Result<(), MessageError> {
        // draft-ietf-moq-transport-22 §11.4.1.1 (Flags): Datagram 起源 (0x40) の Object は
        // Subgroup ID を運ばず、デコーダも下位 2 bit を無視して 0 に解決する。
        // エンコード側も入力値ではなく 0 を正として扱い、prior 状態にも 0 を保存する。
        let effective_subgroup_id = if input.is_datagram_origin {
            0
        } else {
            input.subgroup_id
        };
        let (group_id, subgroup_id, object_id, publisher_priority) = match self.prior_state {
            None => {
                // 最初のオブジェクト: Group ID / Object ID / Publisher Priority は絶対値で明示必須
                // (Datagram 起源の Subgroup ID は wire に載らないため Zero を選ぶ)
                (
                    Some(input.group_id),
                    if input.is_datagram_origin {
                        FetchSubgroupIdMode::Zero
                    } else {
                        FetchSubgroupIdMode::Explicit(effective_subgroup_id)
                    },
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
                // group が前進したら過去 group の per-group エントリを破棄する
                // (グループは要求順に送られるため再出現しない)。
                // 記録量が Object 数に比例して増えないようにする。
                if input.group_id != prior.group_id {
                    let ascending = self.group_order == FETCH_GROUP_ORDER_ASCENDING;
                    if ascending {
                        self.subgroup_priorities
                            .retain(|&(g, _), _| g >= input.group_id);
                    } else {
                        self.subgroup_priorities
                            .retain(|&(g, _), _| g <= input.group_id);
                    }
                }

                let prior_actual = prior.actual_object;
                let can_use_subgroup_prior =
                    matches!(self.prior_context, FetchPriorContext::HasPriorObject);
                let can_use_priority_prior =
                    matches!(self.prior_context, FetchPriorContext::HasPriorObject);

                let group_changed = input.group_id != prior.group_id;
                let subgroup_changed = group_changed
                    || prior_actual.map(|actual| actual.subgroup_id) != Some(effective_subgroup_id);

                // group_id: 変更時にデルタ値を計算 (draft-ietf-moq-transport-23 §11.4.1.1 (Flags))
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
                            "group_order is ASCENDING or DESCENDING (draft-ietf-moq-transport-23 §9.19.8 (GROUP ORDER Parameter))"
                        ),
                    };
                    Some(delta)
                } else {
                    None
                };

                let subgroup_id = if input.is_datagram_origin {
                    FetchSubgroupIdMode::Zero
                } else if prior_actual.is_some_and(|actual| actual.is_datagram_origin) {
                    // draft-ietf-moq-transport-23 §11.4.1.1 (Flags): Datagram 起源の prior Object には
                    // Subgroup ID が無いため、Table 9 の "prior Object's Subgroup ID" を参照できない。
                    // prior 参照を避けて送る (0 は Zero、それ以外は Explicit)
                    if effective_subgroup_id == 0 {
                        FetchSubgroupIdMode::Zero
                    } else {
                        FetchSubgroupIdMode::Explicit(effective_subgroup_id)
                    }
                } else if !subgroup_changed && can_use_subgroup_prior {
                    FetchSubgroupIdMode::PreviousSame
                } else {
                    FetchSubgroupIdMode::Explicit(effective_subgroup_id)
                };

                // object_id: Group 変更時は Group ID Delta が present になるため絶対値、
                // 同一 Group では Group ID Delta が absent になるため
                // prior Object ID + delta + 2 の関係で送る
                // (draft-ietf-moq-transport-23 §11.4.1.1 (Flags):
                //  "When the Group ID Delta field is not present, the Object ID is the prior
                //   Object's ID plus the Object ID Delta + 2 if present.")
                // Subgroup を変更しても Group ID Delta は absent のままなので、同じ +2 の
                // 規則でデルタ値を送る。絶対値を乗せると decoder 側でデルタとして誤解釈される。
                // 連続する +1 は Object ID Delta の省略で表現する
                // (同節: "If Object ID Delta is not present, the Object ID is the prior
                //  Object's ID plus one, regardless of which group it belongs to.")。
                // End of Range の同一 Group の +1 (§11.4.2.1 (ID Decoding)) とは別規則である。
                let object_id = if group_changed {
                    Some(input.object_id)
                } else if prior.object_id.checked_add(1) == Some(input.object_id) {
                    // prior + 1 は Object ID Delta の省略で表現する
                    None
                } else {
                    // 同一 Group の Object ID Delta は実際の差から 2 を引いた値である
                    let delta = input
                        .object_id
                        .checked_sub(prior.object_id)
                        .and_then(|v| v.checked_sub(2))
                        .ok_or(MessageError::ProtocolViolation(
                            "fetch object ID delta underflow",
                        ))?;
                    Some(delta)
                };

                let publisher_priority = if !can_use_priority_prior
                    || prior_actual.map(|actual| actual.publisher_priority)
                        != Some(input.publisher_priority)
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

        // draft-ietf-moq-transport-22 §12.1 (Malformed Tracks) 条件 1:
        // 同一 Subgroup 内で Publisher Priority が変わってはならない。判定は
        // (group_id, subgroup_id) ごとの最後の Priority との比較で行い、間に別 Subgroup や
        // datagram 起源 Object が挟まっても検出する (decoder と対称)。
        // datagram 起源 Object は Subgroup ID を持たないため比較にも記録にも使わない。
        if !input.is_datagram_origin {
            let key = (input.group_id, effective_subgroup_id);
            if let Some(previous_priority) = self.subgroup_priorities.get(&key)
                && *previous_priority != input.publisher_priority
            {
                return Err(MessageError::ProtocolViolation(
                    "publisher priority must not change within the same subgroup in a FETCH response",
                ));
            }
            self.subgroup_priorities
                .insert(key, input.publisher_priority);
        }

        // 状態を更新する
        self.prior_context = FetchPriorContext::HasPriorObject;
        self.prior_state = Some(PriorEncodeState {
            group_id: input.group_id,
            object_id: input.object_id,
            actual_object: Some(PriorActualObject {
                // Datagram 起源は wire 上で Subgroup ID を運ばずデコーダも 0 に解決するため、
                // 実装状態としてもデコーダの解決値 (0) を保持して両者の意味を揃える
                subgroup_id: effective_subgroup_id,
                publisher_priority: input.publisher_priority,
                is_datagram_origin: input.is_datagram_origin,
            }),
        });

        Ok(())
    }

    /// End of Non-Existent Range をエンコードする
    ///
    /// `group_id` / `object_id` は絶対 Location であり、直前の Location とのデルタへの
    /// 変換は内部で行う (draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding))。
    ///
    /// # Errors
    ///
    /// - Group Order と逆方向の Group ID を指定した (デルタが負になる): `ProtocolViolation`
    /// - 同一 Group で直前の Location 以前の Object ID を指定した: `ProtocolViolation`
    pub fn encode_end_of_non_existent_range(
        &mut self,
        group_id: u64,
        object_id: u64,
        buf: &mut Vec<u8>,
    ) -> Result<(), MessageError> {
        self.encode_end_of_range(
            group_id,
            object_id,
            |group_id_delta, object_id_delta| FetchStreamEntry::EndOfNonExistentRange {
                group_id_delta,
                object_id_delta,
            },
            buf,
        )
    }

    /// End of Unknown Range をエンコードする
    ///
    /// `group_id` / `object_id` は絶対 Location であり、直前の Location とのデルタへの
    /// 変換は内部で行う (draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding))。
    ///
    /// # Errors
    ///
    /// [`FetchStreamEncoder::encode_end_of_non_existent_range`] と同じ条件で
    /// `ProtocolViolation` を返す。
    pub fn encode_end_of_unknown_range(
        &mut self,
        group_id: u64,
        object_id: u64,
        buf: &mut Vec<u8>,
    ) -> Result<(), MessageError> {
        self.encode_end_of_range(
            group_id,
            object_id,
            |group_id_delta, object_id_delta| FetchStreamEntry::EndOfUnknownRange {
                group_id_delta,
                object_id_delta,
            },
            buf,
        )
    }

    /// End of Timed-Out Range をエンコードする (draft-ietf-moq-transport-23 §11.4.1 Table 8)
    ///
    /// `group_id` / `object_id` は絶対 Location であり、直前の Location とのデルタへの
    /// 変換は内部で行う (draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding))。
    ///
    /// # Errors
    ///
    /// [`FetchStreamEncoder::encode_end_of_non_existent_range`] と同じ条件で
    /// `ProtocolViolation` を返す。
    pub fn encode_end_of_timed_out_range(
        &mut self,
        group_id: u64,
        object_id: u64,
        buf: &mut Vec<u8>,
    ) -> Result<(), MessageError> {
        self.encode_end_of_range(
            group_id,
            object_id,
            |group_id_delta, object_id_delta| FetchStreamEntry::EndOfTimedOutRange {
                group_id_delta,
                object_id_delta,
            },
            buf,
        )
    }

    /// End of Range の絶対 Location をエンコードする
    ///
    /// `build_entry` は生の Group ID Delta / Object ID Delta を受け取って variant を組み立てる。
    /// 3 種の End of Range はフィールドの構造が同じで variant だけが異なるため、
    /// デルタ計算・状態更新・prior 文脈の遷移を共通化する。
    ///
    /// # Errors
    ///
    /// 直前の Location との差分が表現できない場合に `ProtocolViolation` を返す。
    fn encode_end_of_range(
        &mut self,
        group_id: u64,
        object_id: u64,
        build_entry: fn(u64, u64) -> FetchStreamEntry,
        buf: &mut Vec<u8>,
    ) -> Result<(), MessageError> {
        let (group_id_delta, object_id_delta) = self.end_of_range_deltas(group_id, object_id)?;
        let entry = build_entry(group_id_delta, object_id_delta);
        entry.encode(None, self.prior_context, buf)?;

        self.update_prior_for_end_of_range(group_id, object_id);
        if matches!(self.prior_context, FetchPriorContext::First) {
            self.prior_context = FetchPriorContext::NoPriorActualObject;
        }

        Ok(())
    }

    /// End of Range の絶対 Location を生のデルタに変換する
    ///
    /// draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding):
    /// - FETCH_HEADER 直後の最初の entry は両フィールドが絶対 ID
    /// - それ以外は Group ID を昇順なら加算、降順なら減算する
    /// - Group ID Delta が 0 なら Object ID は prior + delta + 1
    /// - Group ID Delta が 0 でなければ Object ID は絶対値
    ///
    /// Object (Table 10 の 0x04 / 0x08) の Group ID Delta には +1 が付くが、
    /// End of Range の Group ID Delta には付かない (§11.4.2.1)。この差を混同しない。
    fn end_of_range_deltas(
        &self,
        group_id: u64,
        object_id: u64,
    ) -> Result<(u64, u64), MessageError> {
        let Some(prior) = self.prior_state else {
            // 最初の entry は絶対 ID
            return Ok((group_id, object_id));
        };

        let group_id_delta = match self.group_order {
            FETCH_GROUP_ORDER_ASCENDING => {
                group_id
                    .checked_sub(prior.group_id)
                    .ok_or(MessageError::ProtocolViolation(
                        "fetch end of range group ID must not decrease in ascending order",
                    ))?
            }
            FETCH_GROUP_ORDER_DESCENDING => {
                prior
                    .group_id
                    .checked_sub(group_id)
                    .ok_or(MessageError::ProtocolViolation(
                        "fetch end of range group ID must not increase in descending order",
                    ))?
            }
            _ => unreachable!(
                "group_order is ASCENDING or DESCENDING (draft-ietf-moq-transport-23 §9.19.8 (GROUP ORDER PARAMETER))"
            ),
        };

        let object_id_delta = if group_id_delta == 0 {
            // 同一 Group では prior + delta + 1 で表現するため、delta 0 は prior + 1 になる。
            // 直前の Location と同じかそれ以前の Object ID は表現できない。
            object_id
                .checked_sub(prior.object_id)
                .and_then(|v| v.checked_sub(1))
                .ok_or(MessageError::ProtocolViolation(
                    "fetch end of range object ID must be after the prior location within the same group",
                ))?
        } else {
            // Group が変わる場合、Object ID Delta は新しい Group の絶対 Object ID
            object_id
        };

        Ok((group_id_delta, object_id_delta))
    }

    /// End of Range エントリ後に prior_state を更新する
    ///
    /// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): prior Group ID /
    /// Object ID は End of Range の Location に更新するが、prior Subgroup ID / Priority は
    /// End of Range より前の最後の actual Object の値を保持する。
    fn update_prior_for_end_of_range(&mut self, group_id: u64, object_id: u64) {
        match &mut self.prior_state {
            Some(prior) => {
                prior.group_id = group_id;
                prior.object_id = object_id;
            }
            None => {
                self.prior_state = Some(PriorEncodeState {
                    group_id,
                    object_id,
                    actual_object: None,
                });
            }
        }
    }
}

/// FETCH response の Group Order が Ascending (0x01) または Descending (0x02) か検証する
///
/// draft-ietf-moq-transport-23 §9.19.8 (GROUP ORDER Parameter):
/// 値域外は `ProtocolViolation` とする。
fn validate_fetch_group_order(group_order: u8) -> Result<(), MessageError> {
    match group_order {
        FETCH_GROUP_ORDER_ASCENDING | FETCH_GROUP_ORDER_DESCENDING => Ok(()),
        _ => Err(MessageError::ProtocolViolation(
            "FETCH group order must be 1 (Ascending) or 2 (Descending)",
        )),
    }
}
