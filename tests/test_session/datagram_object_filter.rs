//! 受信 datagram Object への Object 単位フィルタ再適用のテスト
//!
//! draft-ietf-moq-transport-21 §3.1 (Subscriptions):
//! "Because subscriptions can share a Track Alias, the subscriber re-applies each
//! subscription's filter to determine which subscription a received Object belongs to."
//!
//! §3.3.3 (Combining Filters): "Pass = Forward AND Location Filters AND Range Filters"。
//! datagram は Subgroup ID を持たないため SUBGROUP_FILTER は評価対象外だが、Object Properties を
//! 持てるため OBJECT_PROPERTY_FILTER は評価対象になる。本モジュールは
//! `Session::recv_object_datagram` の振り分けと、フィルタ不通過 datagram が subscription
//! スコープの状態を更新しないことを検証する。subgroup 経路は
//! `super::subgroup_object_filter` が同じ規則を検証する。
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use super::*;
use shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION;
use shiguredo_moqt::track_properties::PROP_OBJECT_DELIVERY_TIMEOUT;

/// OBJECT_DELIVERY_TIMEOUT (0x02) と PRIOR_OBJECT_ID_GAP (0x3E) を持つバイト列を作る
///
/// フィルタの評価に使う Property と、フィルタ不通過でも状態を更新してはならない
/// gap 追跡の Property を 1 つの datagram に同時に載せるために使う。
fn delivery_timeout_with_gap_properties(timeout: u64, gap: u64) -> Vec<u8> {
    encode_properties([
        ObjectProperty {
            prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
            value: ObjectPropertyValue::VarInt(timeout),
        },
        ObjectProperty {
            prop_type: PROP_PRIOR_OBJECT_ID_GAP,
            value: ObjectPropertyValue::VarInt(gap),
        },
    ])
}

/// 受信側へ注入する Object Datagram を作る
///
/// `properties_data` は Properties Length varint 込みの生バイト列
/// (draft-ietf-moq-transport-21 §11.1.3 (Object Properties))。
/// Object Datagram は Subgroup ID を持たないため、フィルタ評価に渡るのは
/// Group ID / Object ID / Publisher Priority / Object Properties である。
fn datagram(
    alias: u64,
    group_id: u64,
    object_id: u64,
    properties_data: Option<Vec<u8>>,
) -> ObjectDatagram {
    ObjectDatagram {
        track_alias: alias,
        group_id,
        object_id,
        publisher_priority: Some(128),
        properties_data,
        end_of_group: false,
        status: None,
    }
}

/// Property がフィルタに一致する datagram が当該 subscription に紐づくこと
#[test]
fn object_property_filter_accepts_matching_datagram() {
    const ALIAS: u64 = 4200;
    // OBJECT_DELIVERY_TIMEOUT (0x02) が 1..=3 のときだけ通す
    let mut params = MessageParameters::new();
    params.push(property_range_filter(0, PROP_OBJECT_DELIVERY_TIMEOUT, 1, 3));
    let (mut client, rid) = establish_filtered_subscription(ALIAS, params);

    // 値 2 は [1, 3] に該当する
    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS, 0, 0, Some(delivery_timeout_properties(2))))
            .expect("datagram 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "フィルタに一致する Property を持つ datagram は受理されること"
    );
    assert_eq!(
        largest_received(&client, rid),
        Some((0, 0)),
        "受理した datagram の位置で largest_received_location が更新されること"
    );

    // 2 通目も同様に帰属し、位置が前進する
    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS, 0, 1, Some(delivery_timeout_properties(3))))
            .expect("datagram 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(largest_received(&client, rid), Some((0, 1)));
    assert_eq!(client.state(), SessionState::Established);
}

/// フィルタに一致しない datagram が FilteredOut になり subscription スコープの状態を
/// 更新しないこと
///
/// draft-ietf-moq-transport-21 §3.3.3 (Combining Filters): "The publisher MUST forward only
/// objects that pass all filters." 受信側の破棄は §3.1 (Subscriptions) のフィルタ再適用に
/// よるものであり、セッションを閉じない。
#[test]
fn object_property_filter_filters_out_unmatched_datagram_without_state_update() {
    const ALIAS: u64 = 4201;
    // OBJECT_DELIVERY_TIMEOUT (0x02) が 1..=3 のときだけ通す
    let mut params = MessageParameters::new();
    params.push(property_range_filter(0, PROP_OBJECT_DELIVERY_TIMEOUT, 1, 3));
    let (mut client, rid) = establish_filtered_subscription(ALIAS, params);

    // 先にフィルタを通過する datagram を受理させ、以降の比較の基準を作る
    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS, 0, 0, Some(delivery_timeout_properties(2))))
            .expect("datagram 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(largest_received(&client, rid), Some((0, 0)));

    // 値 9 は [1, 3] の外で FilteredOut になる
    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS, 0, 1, Some(delivery_timeout_properties(9))))
            .expect("フィルタ不通過は Err にならないこと"),
        TrackDataAcceptance::FilteredOut,
        "Property が範囲外の datagram は FilteredOut になること"
    );
    assert_eq!(
        largest_received(&client, rid),
        Some((0, 0)),
        "FilteredOut の datagram は largest_received_location を更新しないこと"
    );

    // Property を持たない datagram も OBJECT_PROPERTY_FILTER を満たせない
    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS, 0, 2, None))
            .expect("フィルタ不通過は Err にならないこと"),
        TrackDataAcceptance::FilteredOut,
        "Property が無い datagram は FilteredOut になること"
    );
    assert_eq!(
        largest_received(&client, rid),
        Some((0, 0)),
        "Property を持たない datagram も状態を更新しないこと"
    );

    // PRIOR_OBJECT_ID_GAP を持つ datagram も、フィルタ不通過なら gap 追跡に記録されない。
    // OBJECT_DELIVERY_TIMEOUT が範囲外なので FilteredOut になり、gap [5, 5] は記録されない。
    // 記録されていれば、その範囲に入る Object ID=5 の datagram が Malformed Track になる
    assert_eq!(
        client
            .recv_object_datagram(&datagram(
                ALIAS,
                0,
                6,
                Some(delivery_timeout_with_gap_properties(9, 1))
            ))
            .expect("フィルタ不通過は Err にならないこと"),
        TrackDataAcceptance::FilteredOut
    );
    assert_eq!(
        largest_received(&client, rid),
        Some((0, 0)),
        "gap 付きの FilteredOut でも状態を更新しないこと"
    );
    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS, 0, 5, Some(delivery_timeout_properties(2))))
            .expect("FilteredOut の gap が記録されていなければ Err にならないこと"),
        TrackDataAcceptance::Accepted,
        "FilteredOut の datagram が gap 追跡に記録されていないこと"
    );
    assert_eq!(largest_received(&client, rid), Some((0, 5)));
    assert_eq!(client.state(), SessionState::Established);
}

/// 共有 alias で候補が複数ある場合に最初に通過した subscription に紐づくこと
///
/// draft-ietf-moq-transport-21 §3.1 (Subscriptions):
/// "the subscriber re-applies each subscription's filter to determine which subscription
/// a received Object belongs to."
/// 候補の評価順は登録順で、1 通目の datagram は 2 番目の候補だけが合格する。
#[test]
fn object_property_filter_routes_datagram_to_first_passing_candidate() {
    const ALIAS: u64 = 4202;
    let mut params1 = MessageParameters::new();
    params1.push(property_range_filter(0, PROP_OBJECT_DELIVERY_TIMEOUT, 1, 3));
    let mut params2 = MessageParameters::new();
    params2.push(property_range_filter(0, PROP_OBJECT_DELIVERY_TIMEOUT, 4, 6));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, params2);

    // 値 5 は rid1 が不合格、rid2 の [4, 6] が合格する
    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS, 0, 0, Some(delivery_timeout_properties(5))))
            .expect("datagram 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        largest_received(&client, rid1),
        None,
        "rid1 の状態は更新されないこと"
    );
    assert_eq!(
        largest_received(&client, rid2),
        Some((0, 0)),
        "2 番目の候補 rid2 に紐づくこと"
    );

    // 値 9 はどちらのフィルタにも該当しない
    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS, 0, 1, Some(delivery_timeout_properties(9))))
            .expect("フィルタ不通過は Err にならないこと"),
        TrackDataAcceptance::FilteredOut,
        "どの候補も通過しない datagram は FilteredOut になること"
    );
    assert_eq!(largest_received(&client, rid1), None);
    assert_eq!(largest_received(&client, rid2), Some((0, 0)));

    // 両方のフィルタが合格する場合は登録順で先の rid1 に紐づく
    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS, 0, 2, Some(delivery_timeout_properties(3))))
            .expect("datagram 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        largest_received(&client, rid1),
        Some((0, 2)),
        "最初に通過した rid1 に紐づくこと"
    );
    assert_eq!(
        largest_received(&client, rid2),
        Some((0, 0)),
        "後続の候補 rid2 は更新されないこと"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// datagram が複数の Object Property を持つ場合もフィルタ対象の Property で判定すること
///
/// OBJECT_PROPERTY_FILTER は Property Type を指定して値を評価するため、
/// datagram に他の Property が混在していても対象 Property の値だけで合否が決まる。
#[test]
fn object_property_filter_evaluates_target_property_in_datagram() {
    const ALIAS: u64 = 4203;
    // OBJECT_DELIVERY_TIMEOUT (0x02) が 1..=3 のときだけ通す
    let mut params = MessageParameters::new();
    params.push(property_range_filter(0, PROP_OBJECT_DELIVERY_TIMEOUT, 1, 3));
    let (mut client, rid) = establish_filtered_subscription(ALIAS, params);

    // 対象 Property (先頭) が範囲外 (9) なので FilteredOut になる。2 番目に別の Property が
    // あっても合否は対象 Property の値だけで決まる
    assert_eq!(
        client
            .recv_object_datagram(&datagram(
                ALIAS,
                0,
                0,
                Some(delivery_timeout_with_gap_properties(9, 1))
            ))
            .expect("フィルタ不通過は Err にならないこと"),
        TrackDataAcceptance::FilteredOut,
        "複数 Property を持つ datagram でもフィルタが評価されること"
    );
    assert_eq!(largest_received(&client, rid), None);

    // 対象 Property が範囲内 (2) なら受理され、同じ datagram の gap が追跡に記録される
    assert_eq!(
        client
            .recv_object_datagram(&datagram(
                ALIAS,
                0,
                5,
                Some(delivery_timeout_with_gap_properties(2, 1))
            ))
            .expect("datagram 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(largest_received(&client, rid), Some((0, 5)));

    // 受理した datagram の gap [4, 4] に入る Object ID=4 は Malformed Track として拒否される
    // (FilteredOut の datagram の gap は記録されないため、この範囲に入る Object が
    //  拒否されるのは受理された datagram の gap が記録されている証拠になる)
    let err = client
        .recv_object_datagram(&datagram(ALIAS, 0, 4, Some(delivery_timeout_properties(2))))
        .expect_err("受理済み gap に入る Object は Malformed Track になること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在すること")
            .state,
        SubscriptionState::Terminated,
        "該当 subscription だけが Terminated になること"
    );
    assert_eq!(
        client.state(),
        SessionState::Established,
        "セッションは閉じないこと"
    );
}
