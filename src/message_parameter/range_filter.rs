//! Range Filter の値の解析と検証 (draft-ietf-moq-transport-23 §3.3.2 (Range Filters))
//!
//! 公開型 [`RangeFilterSet`](super::RangeFilterSet) の定義は親モジュール
//! (`crate::message_parameter`) に置き、本モジュールはその `impl` と private ヘルパだけを持つ。
//! 外部へは公開しない。
use super::{
    PARAM_OBJECT_PROPERTY_FILTER, PARAM_OBJECTID_FILTER, PARAM_PRIORITY_FILTER,
    PARAM_SUBGROUP_FILTER, PARAM_TRACK_PROPERTY_FILTER, RangeFilterSet,
};
use crate::{error::MessageError, varint};
use alloc::vec::Vec;

impl RangeFilterSet {
    /// 値がいずれかの範囲に含まれるか判定する
    ///
    /// 範囲が 1 つも無い場合 (Length=0 や SetID のみのインスタンス) は `false` を返す。
    /// 空の Range Filter は「何も通さない」ではなく「フィルタとして機能しない」ため、
    /// 呼び出し側は範囲を持たないインスタンスを評価対象から外すこと。
    pub fn contains(&self, value: u64) -> bool {
        self.ranges.iter().any(|(start, end)| match end {
            Some(end) => value >= *start && value <= *end,
            None => value >= *start,
        })
    }
}

/// Range Filter のバイト列をデコードして [`RangeFilterSet`] にする
///
/// デルタは §3.3.2 の規則で解決する。最初の Start は 0 から、以降の Start は直前の End から、
/// End は同じ Range の Start からのデルタである。パースできない場合は `None` を返す。
///
/// 親モジュールの [`MessageParameters`](super::MessageParameters) から呼ぶため
/// `pub(super)` にしている。クレート外へは公開しない。
pub(super) fn decode_range_filter_set(param_type: u64, bytes: &[u8]) -> Option<RangeFilterSet> {
    let (set_id, property_type, mut pos) = parse_range_filter_header(param_type, bytes)?;
    let mut ranges = Vec::new();
    let mut prev_end: u64 = 0;
    while pos < bytes.len() {
        let (start_delta, n) = varint::decode(&bytes[pos..]).ok()?;
        pos += n;
        let start = prev_end.checked_add(start_delta)?;
        if pos < bytes.len() {
            let (end_delta, n) = varint::decode(&bytes[pos..]).ok()?;
            pos += n;
            let end = start.checked_add(end_delta)?;
            ranges.push((start, Some(end)));
            prev_end = end;
        } else {
            ranges.push((start, None));
            prev_end = start;
        }
    }
    Some(RangeFilterSet {
        set_id,
        property_type,
        ranges,
    })
}

/// Range Filter パラメータ型 (0x25-0x29) かどうかを返す
pub(super) fn is_range_filter_type(param_type: u64) -> bool {
    matches!(
        param_type,
        PARAM_SUBGROUP_FILTER
            | PARAM_OBJECTID_FILTER
            | PARAM_PRIORITY_FILTER
            | PARAM_OBJECT_PROPERTY_FILTER
            | PARAM_TRACK_PROPERTY_FILTER
    )
}

/// Range Filter のヘッダ (SetID, Property Type) とヘッダ長をパースする
///
/// ワイヤフォーマット: SetID (8 bits) | [Property Type (vi64)] | Range...
/// Property Type は OBJECT_PROPERTY_FILTER (0x28) / TRACK_PROPERTY_FILTER (0x29) のみ。
/// 成功時は (set_id, property_type, header_len) を返す。property_type は 0x28/0x29 では Some、
/// それ以外は None。header_len (SetID + Property Type のバイト長) は範囲走査を開始する位置として
/// `count_ranges_in_filter` / `validate_range_filter_deltas` からも使用される。
/// ヘッダの解釈は本関数に集約する。
///
/// Length=0 (空バイト列) は SetID フィールドを持たないため `None` を返す。
/// 呼び出し側が Length=0 を事前に除外すること。
pub(super) fn parse_range_filter_header(
    param_type: u64,
    bytes: &[u8],
) -> Option<(u8, Option<u64>, usize)> {
    let set_id = *bytes.first()?;
    if matches!(
        param_type,
        PARAM_OBJECT_PROPERTY_FILTER | PARAM_TRACK_PROPERTY_FILTER
    ) {
        let (property_type, n) = varint::decode(&bytes[1..]).ok()?;
        // draft-ietf-moq-transport-23 §9.19.13 / §9.19.14: Property Type MUST be even
        // (KVP の偶数 Type = varint 値のみフィルタ可能。奇数 Type = バイト列は不可)
        if property_type % 2 != 0 {
            return None;
        }
        Some((set_id, Some(property_type), 1 + n))
    } else {
        Some((set_id, None, 1))
    }
}

/// Range Filter 内の Range 個数を数える
///
/// ワイヤフォーマット: SetID (8 bits) | [Property Type (vi64)] | Range...
/// Range { Start (vi64), [End (vi64)] } — 最終 Range の End は省略可能。
pub(super) fn count_ranges_in_filter(param_type: u64, bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return Some(0);
    }
    let (_, _, mut pos) = parse_range_filter_header(param_type, bytes)?;
    let mut count: u64 = 0;
    while pos < bytes.len() {
        // Start (vi64)
        let (_, n) = varint::decode(&bytes[pos..]).ok()?;
        pos += n;
        count += 1;
        // End (vi64) — 省略可能 (最終 Range)
        if pos < bytes.len() {
            let (_, n) = varint::decode(&bytes[pos..]).ok()?;
            pos += n;
        }
    }
    Some(count)
}

/// Range Filter のデルタエンコーディング溢出と値域を検証する
///
/// draft-ietf-moq-transport-23 §8.6 (Range Filter Structure): "If adding the delta would exceed
/// 2^64-1, the request MUST be rejected with INVALID_FILTER."
/// draft-ietf-moq-transport-23 §9.19.12: "If a decoded value exceeds 255, the endpoint MUST reject
/// this with REQUEST_ERROR with error code INVALID_FILTER since Publisher Priority is an 8-bit
/// field." (PRIORITY_FILTER の値 > 255)
///
/// 本 codec 層は `MessageError::ProtocolViolation` を返し、session 層 (`validate_range_filters` in
/// `src/session/core.rs`) がそれを wire code `REQUEST_INVALID_FILTER` にラップして REQUEST_ERROR
/// を送出する 2 層構造である。`MessageParameters::decode` 単体テストからは `PROTOCOL_VIOLATION`
/// として観測されるが、ワイヤ上の最終 code は draft の規定どおり `INVALID_FILTER`。
pub(super) fn validate_range_filter_deltas(
    param_type: u64,
    bytes: &[u8],
) -> Result<(), MessageError> {
    if bytes.is_empty() {
        return Ok(());
    }
    let (_, _, mut pos) = parse_range_filter_header(param_type, bytes).ok_or(
        MessageError::ProtocolViolation("malformed Range Filter header"),
    )?;
    let is_priority_filter = param_type == PARAM_PRIORITY_FILTER;
    // デルタ基底: 最初の Start は 0 から、以降の Start は直前 End から、End は同 Start から
    let mut prev_end: u64 = 0;
    while pos < bytes.len() {
        let (start_delta, n) = varint::decode(&bytes[pos..])
            .map_err(|_| MessageError::ProtocolViolation("malformed Range Filter Start delta"))?;
        pos += n;
        let start = prev_end
            .checked_add(start_delta)
            .ok_or(MessageError::ProtocolViolation(
                "Range Filter Start delta overflows u64",
            ))?;
        // draft-ietf-moq-transport-23 §9.19.12: Publisher Priority は 8 bit フィールド
        if is_priority_filter && start > 255 {
            return Err(MessageError::ProtocolViolation(
                "PRIORITY_FILTER value exceeds 255",
            ));
        }
        if pos < bytes.len() {
            let (end_delta, n) = varint::decode(&bytes[pos..])
                .map_err(|_| MessageError::ProtocolViolation("malformed Range Filter End delta"))?;
            pos += n;
            let end = start
                .checked_add(end_delta)
                .ok_or(MessageError::ProtocolViolation(
                    "Range Filter End delta overflows u64",
                ))?;
            // draft-ietf-moq-transport-23 §9.19.12: Publisher Priority は 8 bit フィールド
            if is_priority_filter && end > 255 {
                return Err(MessageError::ProtocolViolation(
                    "PRIORITY_FILTER value exceeds 255",
                ));
            }
            prev_end = end;
        } else {
            // 最終 Range が End なし (open-ended)
            prev_end = start;
        }
    }
    Ok(())
}
