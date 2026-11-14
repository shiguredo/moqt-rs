//! PAUSED / GROUP_ORDER / INCLUDE_PROPERTIES の検証と抽出
//! (draft-ietf-moq-transport-23 §9.19.18 / draft-ietf-moq-transport-23 §9.19.8 /
//! draft-ietf-moq-transport-23 §9.19.22)
//!
//! 将来 draft 側で変更される可能性がある。

use super::super::types::{Subscription, SubscriptionRangeFilters};
use crate::error::SESSION_INVALID_PARAMETER;
use crate::message::common::Location;
use crate::message_parameter::{
    LocationFilter, LocationFilterContext, MessageParameters, PARAM_OBJECT_PROPERTY_FILTER,
    PARAM_OBJECTID_FILTER, PARAM_PRIORITY_FILTER, PARAM_SUBGROUP_FILTER,
};
use crate::object_properties::ObjectProperties;

use super::super::types::SessionError;

/// GROUP_ORDER の値域を検証する (draft-ietf-moq-transport-23 §9.19.8 (GROUP ORDER Parameter))
///
/// 許容値は Ascending (0x1) または Descending (0x2) のみ。それ以外は INVALID_PARAMETER。
/// ワイヤデコード層でも値域検証を行うが、API 経路 (MessageParameters を直接構築して
/// セッション層 API に渡す経路) からの不正値投入を防ぐため、セッション層でも検証する。
/// 受信経路で値域外を検出した場合は、MUST でセッションを INVALID_PARAMETER により
/// 閉じる (self.fail() を呼ぶ)。
/// この仕様は将来変更される可能性がある。
pub fn validate_group_order(order: u8) -> Result<(), SessionError> {
    if order != 0x1 && order != 0x2 {
        return Err(SessionError::new(
            SESSION_INVALID_PARAMETER,
            "invalid group order value",
        ));
    }
    Ok(())
}

/// PAUSED の値を検証し、paused 状態へ変換する
/// (draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter))
///
/// 許容値は 0 (unpaused) と 1 (paused) のみ。それ以外は INVALID_PARAMETER。
/// 値域内なら `1` を `true` (paused)、`0` を `false` (unpaused) として返す。
/// デコード層 (`message_parameter.rs`) でもワイヤフォーマット上の検証を行うが、
/// API 経路からの不正値投入を防ぐためセッション層でも検証する。
///
/// SUBSCRIBE / PUBLISH / REQUEST_UPDATE / PUBLISH_STATE_NOTIFY の受信経路
/// (`handle_peer_subscribe` / `handle_peer_publish` / `handle_peer_request_update` /
/// `handle_peer_publish_state_notify`) では、値域外の受信は同節の MUST (INVALID_PARAMETER)
/// を、本実装では既存の写像に合わせて INVALID_PARAMETER として扱う。呼び出し側が
/// `self.fail()` を呼んでセッションを閉じる (`?` 伝播のみではセッションが開いたまま残る)。
pub fn validate_paused(value: u8) -> Result<bool, SessionError> {
    if value > 1 {
        return Err(SessionError::new(
            SESSION_INVALID_PARAMETER,
            "invalid paused value",
        ));
    }
    Ok(value == 1)
}

/// INCLUDE_PROPERTIES の値を検証する (draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter))
///
/// 許容値は 0 (送らない) と 1 (送る) のみ。それ以外は INVALID_PARAMETER。
/// デコード層 (`message_parameter.rs`) でもワイヤフォーマット上の検証を行うが、
/// API 経路からの不正値投入を防ぐためセッション層でも検証する。
///
/// SUBSCRIBE / FETCH / TRACK_STATUS 受信経路では、値域外の受信は
/// draft §9.19.22 の MUST に従い呼び出し側が `self.fail()` を呼んでセッションを
/// INVALID_PARAMETER により閉じる (`?` 伝播のみではセッションが開いたまま残る)。
pub fn validate_include_properties(value: u8) -> Result<(), SessionError> {
    if value > 1 {
        return Err(SessionError::new(
            SESSION_INVALID_PARAMETER,
            "invalid include_properties value",
        ));
    }
    Ok(())
}

/// PAUSED パラメータから初期 paused 状態を抽出する
/// (draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter))
///
/// 1 → `true` (paused)、0 → `false` (unpaused)、省略 → `false` (既定値 0)。
/// 本関数は送信 API (`send_subscribe` / `send_publish`) 専用であり、送信経路では前段で
/// `validate_paused` が実行済みのため 2 以上の値は到達しない。受信経路は
/// `handle_peer_subscribe` / `handle_peer_publish` が値域検証込みで直接解釈する
/// (本関数は使わない)。
pub fn extract_paused_state(parameters: &MessageParameters) -> bool {
    matches!(parameters.paused(), Some(1))
}

/// パラメータから Object 系 Range Filter を取り出して型付き状態にする
///
/// draft-ietf-moq-transport-23 §3.3.2 (Range Filters): 0x25-0x28 が Object 判定に使われる。
/// TRACK_PROPERTY_FILTER (0x29) は PUBLISH 選別用なので含めない。
pub fn extract_range_filters(parameters: &MessageParameters) -> SubscriptionRangeFilters {
    SubscriptionRangeFilters {
        subgroup: parameters.range_filter_sets(PARAM_SUBGROUP_FILTER),
        object_id: parameters.range_filter_sets(PARAM_OBJECTID_FILTER),
        priority: parameters.range_filter_sets(PARAM_PRIORITY_FILTER),
        object_property: parameters.range_filter_sets(PARAM_OBJECT_PROPERTY_FILTER),
    }
}

/// 適用済みの Range Filter に、パラメータに現れた型だけを反映する
///
/// draft-ietf-moq-transport-23 §3.3.2 (Range Filters): "In REQUEST_UPDATE, Length of 0
/// removes the filter; non-zero replaces it entirely. If a filter parameter is omitted
/// from REQUEST_UPDATE, it is unchanged."
///
/// FILL_PARAMETERS を含む更新は合体できない (§9.19.15 (FILL PARAMETERS Parameter)) ため、
/// 未応答の更新はその前後でグループに分かれる。先行グループで設定した型は後続グループの
/// パラメータに現れないことがあり、[`extract_range_filters`] で 4 型を丸ごと置き換えると
/// 「省略された型は不変」という規則に反して先行グループの設定が消える。
///
/// そこで型ごとに、その型のパラメータが現れたときだけ置き換える (現れた型の
/// [`SubscriptionRangeFilters`] は空 = Length=0 の削除になる)。Subscription に累積
/// パラメータを別途保持すると subscription 状態と二重管理になるため、適用済みの状態である
/// `applied` を直接更新する。
///
/// 型の有無は [`MessageParameters::range_filters`] の生インスタンスで判定する。
/// [`MessageParameters::range_filter_sets`] は Length=0 を除外するため、削除指示を
/// 「型が現れていない」と誤認する。
pub fn apply_range_filter_update(
    applied: &mut SubscriptionRangeFilters,
    parameters: &MessageParameters,
) {
    if !parameters.range_filters(PARAM_SUBGROUP_FILTER).is_empty() {
        applied.subgroup = parameters.range_filter_sets(PARAM_SUBGROUP_FILTER);
    }
    if !parameters.range_filters(PARAM_OBJECTID_FILTER).is_empty() {
        applied.object_id = parameters.range_filter_sets(PARAM_OBJECTID_FILTER);
    }
    if !parameters.range_filters(PARAM_PRIORITY_FILTER).is_empty() {
        applied.priority = parameters.range_filter_sets(PARAM_PRIORITY_FILTER);
    }
    if !parameters
        .range_filters(PARAM_OBJECT_PROPERTY_FILTER)
        .is_empty()
    {
        applied.object_property = parameters.range_filter_sets(PARAM_OBJECT_PROPERTY_FILTER);
    }
}

/// Location Filter を解決して (Start Location, End Location) を返す
///
/// draft-ietf-moq-transport-23 §3.3.1 (Location Filters): RelativeGroup / NextObject は
/// 相対フィルタなので、確立時 / REQUEST_UPDATE 適用時の `largest` で 1 度だけ解決する。
/// 送信のたびに再計算すると Start が前方にずれ続けるため、解決結果を状態として固定する。
/// End フィールド省略時の扱いはコンテキストで異なり、subscription は open-ended (`None`)、
/// Fetch は End = Largest Object になる。
pub fn resolve_location_filter(
    filter: Option<&LocationFilter>,
    largest: Option<&Location>,
    context: LocationFilterContext,
) -> (Option<Location>, Option<Location>) {
    match filter {
        Some(filter) => (
            filter.effective_start_location(largest),
            filter.effective_end_location(largest, context),
        ),
        None => (None, None),
    }
}

/// Object の Pass 評価に必要な値
///
/// draft-ietf-moq-transport-23 §3.3.2 (Range Filters) が対象とする
/// "Subgroup ID, Object ID, and Publisher Priority" と Object Properties に対応する。
/// `subgroup_id` が `None` の場合は datagram (Delivery Mode = Datagram) で、
/// §11.2.1 のワイヤ構造に Subgroup ID フィールドが存在しないため SUBGROUP_FILTER は
/// 評価対象外になる。
#[derive(Debug, Clone)]
pub struct ObjectFilterInput<'a> {
    pub location: Location,
    /// datagram では `None`
    pub subgroup_id: Option<u64>,
    pub publisher_priority: u8,
    /// Object Properties の生バイト列 (Properties Length varint + データ)。
    /// `None` は Object Properties なしを表す。
    pub properties_bytes: Option<&'a [u8]>,
}

/// SetID ごとの AND / SetID 間の OR で Range Filter を評価する
///
/// draft-ietf-moq-transport-23 §3.3.2 (Range Filters): "Filter parameters with the same SetID
/// are AND'd; distinct SetIDs are OR'd. The final result is SetID=0 OR SetID=1 OR ... SetID=255,
/// where each SetID=i is the AND of all filter parameters carrying that SetID."
pub(crate) fn range_filters_pass(
    filters: &SubscriptionRangeFilters,
    input: &ObjectFilterInput<'_>,
    object_properties: Option<&ObjectProperties>,
) -> bool {
    if filters.is_empty() {
        return true;
    }
    filters.set_ids().into_iter().any(|set_id| {
        // SUBGROUP_FILTER: datagram には Subgroup ID フィールドが無いので評価しない
        let subgroup_ok = filters
            .subgroup
            .iter()
            .filter(|set| set.set_id == set_id)
            .all(|set| match input.subgroup_id {
                Some(subgroup_id) => set.contains(subgroup_id),
                None => true,
            });
        let object_id_ok = filters
            .object_id
            .iter()
            .filter(|set| set.set_id == set_id)
            .all(|set| set.contains(input.location.object_id));
        let priority_ok = filters
            .priority
            .iter()
            .filter(|set| set.set_id == set_id)
            .all(|set| set.contains(u64::from(input.publisher_priority)));
        // OBJECT_PROPERTY_FILTER: 指定 Property Type の値が範囲に入っていること。
        // Property が付いていない Object はその条件を満たせないので AND が落ちる。
        let property_ok = filters
            .object_property
            .iter()
            .filter(|set| set.set_id == set_id)
            .all(|set| match (set.property_type, object_properties) {
                (Some(prop_type), Some(props)) => props
                    .find_varint(prop_type)
                    .is_some_and(|v| set.contains(v)),
                _ => false,
            });
        subgroup_ok && object_id_ok && priority_ok && property_ok
    })
}

/// Location Filter を評価する
///
/// draft-ietf-moq-transport-23 §3.3.1 (Location Filters): "Only objects with Locations
/// within the inclusive range pass the filter"。End は Location 単位の比較で inclusive。
pub(crate) fn location_filter_pass(
    start: Option<&Location>,
    end: Option<&Location>,
    location: &Location,
) -> bool {
    if let Some(start) = start
        && location < start
    {
        return false;
    }
    if let Some(end) = end
        && location > end
    {
        return false;
    }
    true
}

/// draft-ietf-moq-transport-23 §3.3.3 (Combining Filters) の Pass 評価
///
/// > Pass = (NOT Paused) AND Location Filters AND Range Filters
///
/// §3.3.3 は "which can be evaluated in any order" なので評価順は結果に影響しない。
/// `true` を返した Object だけを publisher は送信してよい。
///
/// Object Properties のデコードに失敗した場合は OBJECT_PROPERTY_FILTER を満たせないものとして
/// 扱う (フィルタが設定されていなければ影響しない)。
pub fn object_passes_filters(subscription: &Subscription, input: &ObjectFilterInput<'_>) -> bool {
    // draft-ietf-moq-transport-23 §3.1.1 (Pausing Subscriptions):
    // "The publisher does not send Objects on a paused subscription"
    if subscription.paused {
        return false;
    }
    if !location_filter_pass(
        subscription.filter_start.as_ref(),
        subscription.filter_end.as_ref(),
        &input.location,
    ) {
        return false;
    }
    if subscription.range_filters.is_empty() {
        return true;
    }
    let object_properties = input
        .properties_bytes
        .and_then(|bytes| ObjectProperties::decode(bytes).ok().map(|(props, _)| props));
    range_filters_pass(
        &subscription.range_filters,
        input,
        object_properties.as_ref(),
    )
}

/// Subgroup header 受信時点で評価可能なフィルタだけで Pass 判定する
///
/// draft-ietf-moq-transport-23 §3.1 (Subscriptions): "the subscriber re-applies each
/// subscription's filter to determine which subscription a received Object belongs to."
///
/// header 時点では Object ID と Object Properties が未知のため、OBJECTID_FILTER と
/// OBJECT_PROPERTY_FILTER は評価対象外とする。Location Filter は Group 部分のみ評価し、
/// Object 部分は評価しない。SUBGROUP_FILTER と PRIORITY_FILTER は header から値が揃うため
/// 評価する。
///
/// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
pub fn header_passes_filters(
    subscription: &Subscription,
    group_id: u64,
    subgroup_id: Option<u64>,
    publisher_priority: u8,
) -> bool {
    // draft-ietf-moq-transport-23 §3.1.1 (Pausing Subscriptions):
    // "The publisher does not send Objects on a paused subscription"
    if subscription.paused {
        return false;
    }
    // Location Filter の Group 部分のみ評価する (Object 部分は header 時点では未知)
    if let Some(start) = subscription.filter_start.as_ref()
        && group_id < start.group_id
    {
        return false;
    }
    if let Some(end) = subscription.filter_end.as_ref()
        && group_id > end.group_id
    {
        return false;
    }
    // Range Filters のうち SUBGROUP_FILTER と PRIORITY_FILTER のみ評価する
    if subscription.range_filters.is_empty() {
        return true;
    }
    subscription
        .range_filters
        .set_ids()
        .into_iter()
        .any(|set_id| {
            let subgroup_ok = subscription
                .range_filters
                .subgroup
                .iter()
                .filter(|set| set.set_id == set_id)
                .all(|set| match subgroup_id {
                    Some(subgroup_id) => set.contains(subgroup_id),
                    None => true,
                });
            let priority_ok = subscription
                .range_filters
                .priority
                .iter()
                .filter(|set| set.set_id == set_id)
                .all(|set| set.contains(u64::from(publisher_priority)));
            subgroup_ok && priority_ok
        })
}
