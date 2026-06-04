//! DEFAULT_PUBLISHER_PRIORITY / DEFAULT_PUBLISHER_GROUP_ORDER の Session 配線テスト
//!
//! draft-ietf-moq-transport-21 §10.4 (DEFAULT PUBLISHER PRIORITY) /
//! §10.5 (DEFAULT PUBLISHER GROUP ORDER)
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use super::*;
use shiguredo_moqt::session::types::{
    DEFAULT_PUBLISHER_GROUP_ORDER_ASCENDING, PUBLISHER_PRIORITY_DEFAULT,
};
use shiguredo_moqt::track_properties::{
    PROP_DEFAULT_PUBLISHER_GROUP_ORDER, PROP_DEFAULT_PUBLISHER_PRIORITY, TrackProperty,
    TrackPropertyValue,
};

/// DEFAULT_PUBLISHER_* を宣言した `TrackProperties` を作る
fn default_publisher_props(priority: Option<u64>, group_order: Option<u64>) -> TrackProperties {
    let mut props = TrackProperties::new();
    if let Some(priority) = priority {
        props.push(TrackProperty {
            prop_type: PROP_DEFAULT_PUBLISHER_PRIORITY,
            value: TrackPropertyValue::VarInt(priority),
        });
    }
    if let Some(group_order) = group_order {
        props.push(TrackProperty {
            prop_type: PROP_DEFAULT_PUBLISHER_GROUP_ORDER,
            value: TrackPropertyValue::VarInt(group_order),
        });
    }
    props
}

/// SUBSCRIBE_OK で発行した DEFAULT_PUBLISHER_* が両側の subscription に保持される
#[test]
fn subscribe_ok_track_properties_are_stored_on_both_sides() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");

    // SUBSCRIBE 受信直後は publisher (server) 側もまだ未宣言
    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(sub.default_publisher_priority, None);
    assert_eq!(sub.default_publisher_group_order, None);
    assert_eq!(
        sub.effective_publisher_priority(),
        PUBLISHER_PRIORITY_DEFAULT,
        "未宣言なら §10.4 の既定値 128"
    );
    assert_eq!(
        sub.effective_publisher_group_order(),
        DEFAULT_PUBLISHER_GROUP_ORDER_ASCENDING,
        "未宣言なら §10.5 の既定値 Ascending (0x1)"
    );

    server
        .send_subscribe_ok(
            rid,
            1,
            MessageParameters::new(),
            default_publisher_props(Some(42), Some(2)),
        )
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");

    // 発行側 (publisher = server)
    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(sub.default_publisher_priority, Some(42));
    assert_eq!(sub.default_publisher_group_order, Some(2));
    assert_eq!(sub.effective_publisher_priority(), 42);
    assert_eq!(sub.effective_publisher_group_order(), 2);

    // 受信側 (subscriber = client)
    let sub = client.subscription(rid).expect("subscription が存在する");
    assert_eq!(sub.default_publisher_priority, Some(42));
    assert_eq!(sub.default_publisher_group_order, Some(2));
    assert_eq!(sub.effective_publisher_priority(), 42);
    assert_eq!(sub.effective_publisher_group_order(), 2);
}

/// PUBLISH で発行した DEFAULT_PUBLISHER_* が両側の subscription に保持される
#[test]
fn publish_track_properties_are_stored_on_both_sides() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            111,
            MessageParameters::new(),
            default_publisher_props(Some(7), Some(1)),
        )
        .expect("PUBLISH の送信に成功すること");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("PUBLISH の受信に成功すること");

    // 発行側 (publisher = client)
    let sub = client.subscription(rid).expect("subscription が存在する");
    assert_eq!(sub.default_publisher_priority, Some(7));
    assert_eq!(sub.default_publisher_group_order, Some(1));

    // 受信側 (subscriber = server)
    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(sub.default_publisher_priority, Some(7));
    assert_eq!(sub.default_publisher_group_order, Some(1));
    assert_eq!(sub.effective_publisher_priority(), 7);
}

/// DEFAULT_PRIORITY header では Track Property の値が publisher_priority に解決される
///
/// draft-ietf-moq-transport-21 §10.4: "Subgroups and Datagrams for this subscription inherit
/// this priority, unless they specifically override it."
#[test]
fn default_priority_header_resolves_from_track_property() {
    let alias = 910;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(
            rid,
            alias,
            MessageParameters::new(),
            default_publisher_props(Some(42), None),
        )
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");

    // publisher_priority を None にした header (= DEFAULT_PRIORITY bit が立っている)
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 3,
        subgroup_id: SubgroupIdMode::Explicit(7),
        publisher_priority: None,
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let stream_id = DataStreamId(41);
    client
        .recv_data_stream_type(stream_id, header.encode()[0] as u64)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("subgroup header の通知に成功すること");

    let sub = client.subscription(rid).expect("subscription が存在する");
    assert_eq!(
        sub.publisher_priority,
        Some(42),
        "DEFAULT_PRIORITY header は Track Property の値を継承する"
    );
    assert_eq!(sub.effective_publisher_priority(), 42);
}

/// Track Property も無ければ DEFAULT_PRIORITY header は既定値 128 に解決される
///
/// draft-ietf-moq-transport-21 §10.4: "If omitted, the Default Publisher Priority is 128."
#[test]
fn default_priority_header_falls_back_to_128() {
    let alias = 911;
    let (mut client, _server, rid) = establish_subscribe_track(alias);

    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 3,
        subgroup_id: SubgroupIdMode::Explicit(7),
        publisher_priority: None,
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let stream_id = DataStreamId(42);
    client
        .recv_data_stream_type(stream_id, header.encode()[0] as u64)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("subgroup header の通知に成功すること");

    let sub = client.subscription(rid).expect("subscription が存在する");
    assert_eq!(
        sub.publisher_priority,
        Some(PUBLISHER_PRIORITY_DEFAULT),
        "Track Property も無ければ既定値 128"
    );
}

/// header が優先度を明示している場合は Track Property を上書きする
///
/// draft-ietf-moq-transport-21 §10.4 の "unless they specifically override it" に対応する。
#[test]
fn explicit_header_priority_overrides_track_property() {
    let alias = 912;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(
            rid,
            alias,
            MessageParameters::new(),
            default_publisher_props(Some(42), None),
        )
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");

    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 3,
        subgroup_id: SubgroupIdMode::Explicit(7),
        publisher_priority: Some(200),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let stream_id = DataStreamId(43);
    client
        .recv_data_stream_type(stream_id, header.encode()[0] as u64)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("subgroup header の通知に成功すること");

    let sub = client.subscription(rid).expect("subscription が存在する");
    assert_eq!(
        sub.publisher_priority,
        Some(200),
        "header の明示値が Track Property を上書きすること"
    );
    assert_eq!(
        sub.default_publisher_priority,
        Some(42),
        "Track Property 側の宣言値は保持されたままであること"
    );
}

/// DEFAULT_PUBLISHER_GROUP_ORDER は GROUP ORDER Parameter とは別に保持される
///
/// draft-ietf-moq-transport-21 §10.5 は publisher の選好、§9.20.9 は subscriber からの要求で
/// 別の値である。片方が他方を上書きしてはいけない。
#[test]
fn publisher_group_order_is_independent_from_group_order_parameter() {
    let (mut client, mut server) = establish_pair();
    // subscriber は GROUP_ORDER Parameter で Descending (0x2) を要求する
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: shiguredo_moqt::message_parameter::PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(2),
    });
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), params)
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    // publisher は DEFAULT_PUBLISHER_GROUP_ORDER で Ascending (0x1) を宣言する
    server
        .send_subscribe_ok(
            rid,
            1,
            MessageParameters::new(),
            default_publisher_props(None, Some(1)),
        )
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");

    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(
        sub.group_order,
        Some(2),
        "subscriber の要求 (§9.20.9) は Descending のまま保たれること"
    );
    assert_eq!(
        sub.default_publisher_group_order,
        Some(1),
        "publisher の選好 (§10.5) は Ascending として別に保持されること"
    );
    assert_eq!(sub.effective_publisher_group_order(), 1);
}
