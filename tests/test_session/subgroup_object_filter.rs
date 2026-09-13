//! 受信 subgroup Object への Object 単位フィルタ再適用のテスト
//!
//! draft-ietf-moq-transport-21 §3.1 (Subscriptions):
//! "Because subscriptions can share a Track Alias, the subscriber re-applies each
//! subscription's filter to determine which subscription a received Object belongs to."
//!
//! §3.3.3 (Combining Filters): "Pass = Forward AND Location Filters AND Range Filters"。
//! header 受理時に評価できるフィルタだけでは Object の帰属先を確定できないため、
//! Object 受信時に候補すべてへフィルタを再適用する。本モジュールはその振り分けと、
//! フィルタ不通過 Object が subscription スコープの状態を更新しないことを検証する。
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use super::*;
use shiguredo_moqt::error::{SESSION_DATA_STREAM_TIMEOUT, SESSION_PROTOCOL_VIOLATION};
use shiguredo_moqt::message_parameter::{
    LocationFilter, PARAM_LOCATION_FILTER, PARAM_OBJECT_PROPERTY_FILTER, PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER, PARAM_SUBGROUP_FILTER,
};
use shiguredo_moqt::stream::OBJECT_STATUS_END_OF_GROUP;
use shiguredo_moqt::track_properties::PROP_OBJECT_DELIVERY_TIMEOUT;

/// MAX_FILTER_RANGES を宣言する server 用 SetupOptions を作る
///
/// Range Filter を送るには peer (server) 側の宣言が必要 (§9.1.6 (MAX FILTER RANGES))。
fn max_filter_options() -> SetupOptions {
    let mut opts = SetupOptions::new();
    opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_FILTER_RANGES,
        value: SetupOptionValue::VarInt(8),
    });
    opts
}

/// SUBSCRIBE を 1 本送って受信側に届け request_id を返す
fn send_and_recv_subscribe(
    client: &mut Session,
    server: &mut Session,
    parameters: MessageParameters,
) -> u64 {
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), parameters)
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    rid
}

/// SUBSCRIBE_OK を返して両側で Established にする
fn complete_subscribe(client: &mut Session, server: &mut Session, rid: u64, alias: u64) {
    server
        .send_subscribe_ok(rid, alias, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");
}

/// 同一 alias を共有する 2 subscription を確立して (client, server, rid1, rid2) を返す
///
/// PUBLISH_DONE の送信を検証に使うテストは server 側の操作が必要になるため、
/// server も返す。candidate の評価順は登録順 (rid1 → rid2) になる。
fn establish_shared_alias_pair(
    alias: u64,
    parameters1: MessageParameters,
    parameters2: MessageParameters,
) -> (Session, Session, u64, u64) {
    let (mut client, mut server) =
        establish_pair_with_options(SetupOptions::new(), max_filter_options());
    let rid1 = send_and_recv_subscribe(&mut client, &mut server, parameters1);
    let rid2 = send_and_recv_subscribe(&mut client, &mut server, parameters2);
    complete_subscribe(&mut client, &mut server, rid1, alias);
    complete_subscribe(&mut client, &mut server, rid2, alias);
    (client, server, rid1, rid2)
}

/// 同一 alias を共有する 2 subscription を確立して (client, rid1, rid2) を返す
///
/// 候補の評価順は登録順 (rid1 → rid2) になる。
fn establish_shared_alias(
    alias: u64,
    parameters1: MessageParameters,
    parameters2: MessageParameters,
) -> (Session, u64, u64) {
    let (client, _server, rid1, rid2) =
        establish_shared_alias_pair(alias, parameters1, parameters2);
    (client, rid1, rid2)
}

/// PUBLISH_DONE (stream_count 0) を送受信し、drain timer を満了させる
///
/// publisher が申告した stream_count 0 より受信 stream 数が多いため
/// `stream_count_overrun` が立ち、open 中の受信 stream が残っていれば
/// `cleanup_ready` は false になる。キャンセル由来 `Terminated` の `cleanup_ready` が
/// 常に true になる性質に依存せず、open 数の計上漏れを検出できる。
fn send_publish_done_and_expire_drain(client: &mut Session, server: &mut Session, rid: u64) {
    server
        .send_publish_done(
            rid,
            0x2,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("PUBLISH_DONE の送信に成功すること");
    let (_, done_msg) = take_send_on_stream(server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("PUBLISH_DONE の受信に成功すること");
    // drain timer を満了させる (delivery timeout 未設定なら 0 で即満了になる)
    client.tick(1_000);
}

/// 同一 alias を共有する 3 subscription を確立して (client, rid1, rid2, rid3) を返す
///
/// 候補の評価順は登録順 (rid1 → rid2 → rid3) になる。
fn establish_shared_alias_three(
    alias: u64,
    parameters1: MessageParameters,
    parameters2: MessageParameters,
    parameters3: MessageParameters,
) -> (Session, u64, u64, u64) {
    let (mut client, mut server) =
        establish_pair_with_options(SetupOptions::new(), max_filter_options());
    let rid1 = send_and_recv_subscribe(&mut client, &mut server, parameters1);
    let rid2 = send_and_recv_subscribe(&mut client, &mut server, parameters2);
    let rid3 = send_and_recv_subscribe(&mut client, &mut server, parameters3);
    complete_subscribe(&mut client, &mut server, rid1, alias);
    complete_subscribe(&mut client, &mut server, rid2, alias);
    complete_subscribe(&mut client, &mut server, rid3, alias);
    (client, rid1, rid2, rid3)
}

/// フィルタ付き SUBSCRIBE を 1 本確立して (client, rid) を返す
fn establish_filtered_subscription(alias: u64, parameters: MessageParameters) -> (Session, u64) {
    let (mut client, mut server) =
        establish_pair_with_options(SetupOptions::new(), max_filter_options());
    let rid = send_and_recv_subscribe(&mut client, &mut server, parameters);
    complete_subscribe(&mut client, &mut server, rid, alias);
    (client, rid)
}

/// client (subscriber) 側で subgroup stream を登録し、header の受理結果を返す
fn recv_header(
    client: &mut Session,
    stream_id: DataStreamId,
    header: &SubgroupHeader,
) -> TrackDataAcceptance {
    client
        .recv_data_stream_type(stream_id, header.encode()[0] as u64)
        .expect("subgroup stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_id, header)
        .expect("subgroup header の受理に成功すること")
}

/// 通常 Object (payload あり / properties なし) を作る
fn normal_object(object_id: u64) -> DecodedSubgroupObject {
    DecodedSubgroupObject {
        object_id,
        payload_length: 1,
        status: None,
        properties_bytes: None,
    }
}

/// Object Properties 付きの通常 Object を作る
fn object_with_properties(object_id: u64, properties_bytes: Vec<u8>) -> DecodedSubgroupObject {
    DecodedSubgroupObject {
        object_id,
        payload_length: 1,
        status: None,
        properties_bytes: Some(properties_bytes),
    }
}

/// Object Status のみ (payload なし) の Object を作る
fn status_object(object_id: u64, status: u64) -> DecodedSubgroupObject {
    DecodedSubgroupObject {
        object_id,
        payload_length: 0,
        status: Some(status),
        properties_bytes: None,
    }
}

/// OBJECT_DELIVERY_TIMEOUT (0x02) を 1 つ持つ Object Properties のバイト列を作る
///
/// `ObjectPropertyTracker` の gap 検証対象外の Property を使い、
/// OBJECT_PROPERTY_FILTER の評価だけを検証できるようにする。
fn delivery_timeout_properties(value: u64) -> Vec<u8> {
    encode_properties(ObjectProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: ObjectPropertyValue::VarInt(value),
    })
}

/// PRIOR_OBJECT_ID_GAP (0x3E) を 1 つ持つ Object Properties のバイト列を作る
fn prior_object_id_gap_properties(value: u64) -> Vec<u8> {
    encode_properties(ObjectProperty {
        prop_type: PROP_PRIOR_OBJECT_ID_GAP,
        value: ObjectPropertyValue::VarInt(value),
    })
}

/// Object Property を 1 つ持つ Object Properties のバイト列を作る
fn encode_properties(property: ObjectProperty) -> Vec<u8> {
    let mut properties = ObjectProperties::new();
    properties.push(property);
    let mut bytes = Vec::new();
    properties
        .encode(&mut bytes)
        .expect("Object Properties の encode に成功すること");
    bytes
}

/// Property Type 付きの OBJECT_PROPERTY_FILTER パラメータを作る
fn property_range_filter(set_id: u8, property_type: u64, start: u64, end: u64) -> MessageParameter {
    let mut bytes = vec![set_id];
    shiguredo_moqt::varint::encode(property_type, &mut bytes);
    shiguredo_moqt::varint::encode(start, &mut bytes);
    shiguredo_moqt::varint::encode(end - start, &mut bytes);
    MessageParameter {
        param_type: PARAM_OBJECT_PROPERTY_FILTER,
        value: MessageParameterValue::LengthPrefixed(bytes),
    }
}

/// LOCATION_FILTER パラメータを作る
fn location_filter_parameter(filter: LocationFilter) -> MessageParameter {
    MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(filter.encode_to_bytes()),
    }
}

/// subscription の largest_received_location を (group_id, object_id) で返す
fn largest_received(session: &Session, request_id: u64) -> Option<(u64, u64)> {
    session
        .subscription(request_id)
        .expect("subscription が存在すること")
        .largest_received_location
        .as_ref()
        .map(|location| (location.group_id, location.object_id))
}

/// 受信 subgroup Object の Object ID が OBJECTID_FILTER で候補ごとに振り分けられること
#[test]
fn objectid_filter_routes_each_received_object() {
    const ALIAS: u64 = 3000;
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let mut params2 = MessageParameters::new();
    params2.push(range_filter(PARAM_OBJECTID_FILTER, 0, 10, 19));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, params2);

    let stream_id = DataStreamId(300);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted,
        "header は最初の候補に受理されること"
    );

    // Object ID=5 は rid1 の [0, 9] に該当する
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(5))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(largest_received(&client, rid1), Some((0, 5)));
    assert_eq!(largest_received(&client, rid2), None);

    // Object ID=15 は rid1 が不合格、rid2 の [10, 19] が合格する
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(15))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "他の subscription に属する Object が破棄されないこと"
    );
    assert_eq!(
        largest_received(&client, rid1),
        Some((0, 5)),
        "rid1 の状態は更新されないこと"
    );
    assert_eq!(
        largest_received(&client, rid2),
        Some((0, 15)),
        "rid2 に帰属されること"
    );

    // Object ID=50 はどちらのフィルタにも該当しない
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(50))
            .expect("フィルタ不通過は Err にならないこと"),
        TrackDataAcceptance::FilteredOut,
        "どの候補も通過しない Object は FilteredOut になること"
    );
    assert_eq!(largest_received(&client, rid1), Some((0, 5)));
    assert_eq!(largest_received(&client, rid2), Some((0, 15)));

    // stream 会計は header 時に記録した rid1 に維持される
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .stream_counts
            .incoming_subgroup_count,
        1,
        "stream 会計は header 時の rid1 に維持されること"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .stream_counts
            .incoming_subgroup_count,
        0,
        "帰属先が変わっても stream 会計は rid2 に移らないこと"
    );

    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("stream 終端の処理に成功すること");
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "FIN で open 中の受信 stream が 0 になること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 受信 subgroup Object の Object Property が OBJECT_PROPERTY_FILTER で振り分けられること
#[test]
fn object_property_filter_routes_each_received_object() {
    const ALIAS: u64 = 3001;
    let mut params1 = MessageParameters::new();
    params1.push(property_range_filter(0, PROP_OBJECT_DELIVERY_TIMEOUT, 1, 3));
    let mut params2 = MessageParameters::new();
    params2.push(property_range_filter(0, PROP_OBJECT_DELIVERY_TIMEOUT, 4, 6));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, params2);

    let stream_id = DataStreamId(301);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: true,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );

    // 値 2 は rid1 の [1, 3] に該当する
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &object_with_properties(0, delivery_timeout_properties(2))
            )
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(largest_received(&client, rid1), Some((0, 0)));
    assert_eq!(largest_received(&client, rid2), None);

    // 値 5 は rid1 が不合格、rid2 の [4, 6] が合格する
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &object_with_properties(1, delivery_timeout_properties(5))
            )
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        largest_received(&client, rid1),
        Some((0, 0)),
        "rid1 の状態は更新されないこと"
    );
    assert_eq!(largest_received(&client, rid2), Some((0, 1)));

    // 値 9 はどちらのフィルタにも該当しない
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &object_with_properties(2, delivery_timeout_properties(9))
            )
            .expect("フィルタ不通過は Err にならないこと"),
        TrackDataAcceptance::FilteredOut,
        "Property が範囲外の Object は FilteredOut になること"
    );
    assert_eq!(largest_received(&client, rid1), Some((0, 0)));
    assert_eq!(largest_received(&client, rid2), Some((0, 1)));

    // Property を持たない Object も OBJECT_PROPERTY_FILTER を満たせない
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(3))
            .expect("フィルタ不通過は Err にならないこと"),
        TrackDataAcceptance::FilteredOut,
        "Property が無い Object は FilteredOut になること"
    );
}

/// 受信 subgroup Object の Location が Location Filter の Object 部分で振り分けられること
#[test]
fn location_filter_object_part_routes_each_received_object() {
    const ALIAS: u64 = 3002;
    // rid1: {0, 0} から {0, 4} まで
    let mut params1 = MessageParameters::new();
    params1.push(location_filter_parameter(
        LocationFilter::AbsoluteRangeWithEnd {
            start: Location {
                group_id: 0,
                object_id: 0,
            },
            end_group_delta: 0,
            end_object: 4,
        },
    ));
    // rid2: {0, 5} から {0, 9} まで
    let mut params2 = MessageParameters::new();
    params2.push(location_filter_parameter(
        LocationFilter::AbsoluteRangeWithEnd {
            start: Location {
                group_id: 0,
                object_id: 5,
            },
            end_group_delta: 0,
            end_object: 9,
        },
    ));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, params2);

    let stream_id = DataStreamId(302);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted,
        "header は Group 部分のみの評価で受理されること"
    );

    // Object ID=3 は rid1 の [0, 4] に該当する
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(3))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(largest_received(&client, rid1), Some((0, 3)));
    assert_eq!(largest_received(&client, rid2), None);

    // Object ID=7 は rid1 が不合格、rid2 の [5, 9] が合格する
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(7))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        largest_received(&client, rid1),
        Some((0, 3)),
        "rid1 の状態は更新されないこと"
    );
    assert_eq!(largest_received(&client, rid2), Some((0, 7)));

    // Object ID=12 はどちらの End よりも後ろ
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(12))
            .expect("フィルタ不通過は Err にならないこと"),
        TrackDataAcceptance::FilteredOut,
        "End を超えた Object は FilteredOut になること"
    );
    assert_eq!(largest_received(&client, rid1), Some((0, 3)));
    assert_eq!(largest_received(&client, rid2), Some((0, 7)));
}

/// FirstObjectId モードでは subgroup_id を解決してから SUBGROUP_FILTER を評価すること
#[test]
fn first_object_id_resolves_subgroup_id_before_subgroup_filter() {
    const ALIAS: u64 = 3003;
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_SUBGROUP_FILTER, 0, 0, 4));
    let mut params2 = MessageParameters::new();
    params2.push(range_filter(PARAM_SUBGROUP_FILTER, 0, 5, 9));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, params2);

    // FirstObjectId モード: header 時点では subgroup_id が未解決のため SUBGROUP_FILTER は
    // 両候補を素通りし、最初の candidate である rid1 に紐づく
    let stream_id = DataStreamId(303);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted,
        "header は最初の候補に受理されること"
    );

    // 最初の Object ID=7 が subgroup_id に解決される。rid1 は不合格、rid2 が合格する
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(7))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "解決済み subgroup_id で SUBGROUP_FILTER が評価されること"
    );
    assert_eq!(
        largest_received(&client, rid1),
        None,
        "rid1 (subgroup [0, 4]) には帰属しないこと"
    );
    assert_eq!(
        largest_received(&client, rid2),
        Some((0, 7)),
        "rid2 (subgroup [5, 9]) に帰属すること"
    );

    // 2 番目の Object ID=10 も解決済み subgroup_id=7 で評価され、rid2 が引き続き帰属先になる
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(10))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(largest_received(&client, rid2), Some((0, 10)));

    // stream 会計は header 時の rid1 に維持される
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .stream_counts
            .incoming_subgroup_count,
        1
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .stream_counts
            .incoming_subgroup_count,
        0
    );
}

/// FirstObjectId の最初の Object がフィルタ不通過でも subgroup_id 解決と priority 記録を
/// 行うこと
///
/// draft-ietf-moq-transport-21 §3.1 (Subscriptions) のフィルタ再適用で Object は破棄されるが、
/// wire 構造の subgroup_id 解決と §12.1 (Malformed Tracks) 条件 1 の priority 記録は
/// フィルタ判定に依存させない。
#[test]
fn filtered_out_first_object_id_still_resolves_subgroup_id() {
    const ALIAS: u64 = 3008;
    // Object ID 0..=4 のみ通す。最初の Object ID=7 はフィルタ不通過になる
    let mut params = MessageParameters::new();
    params.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 4));
    let (mut client, rid) = establish_filtered_subscription(ALIAS, params);

    let stream_id = DataStreamId(309);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(10),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    // 最初の Object ID=7 はフィルタ不通過。それでも subgroup_id=7 に解決され、
    // priority 10 が記録される
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(7))
            .expect("フィルタ不通過は Err にならないこと"),
        TrackDataAcceptance::FilteredOut
    );

    // STOP_SENDING で終端し、同一 Subgroup の再オープンを可能にする
    client
        .send_data_stream_stop_sending(stream_id)
        .expect("STOP_SENDING の送信に成功すること");

    // 同一 Subgroup を priority 99 で再オープンすると、記録済み priority 10 と不一致になり
    // Malformed Track (条件 1) になる。priority が記録されていなければ受理されてしまう
    let stream_id2 = DataStreamId(310);
    let header2 = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(7),
        publisher_priority: Some(99),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_id2, header2.encode()[0] as u64)
        .expect("subgroup stream type の通知に成功すること");
    let err = client
        .recv_subgroup_header(stream_id2, &header2)
        .expect_err("同一 Subgroup の priority 不一致は Malformed Track になること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        client.state(),
        SessionState::Established,
        "セッションは閉じないこと"
    );
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在すること")
            .state,
        SubscriptionState::Terminated,
        "該当 subscription は Terminated になること"
    );
}

/// フィルタ不通過 Object が subscription スコープの状態を更新しないこと
///
/// `largest_received_location` / 先頭 Object の delivery timeout override /
/// Object Status 由来の終端 / `peer_object_properties` の gap 追跡のいずれも、
/// 帰属先が確定しない限り更新しない。フィルタ不通過の先頭 Object に delivery timeout
/// Property が付いていても override を登録せず、後続の受理 Object を「先頭扱い」しない
/// (`first_object_received` はフィルタ判定に依存せず更新する)。
#[test]
fn filtered_out_object_does_not_update_subscription_state() {
    const ALIAS: u64 = 3004;
    // OBJECT_DELIVERY_TIMEOUT (0x02) が 1..=3 のときだけ通す
    let mut params = MessageParameters::new();
    params.push(property_range_filter(0, PROP_OBJECT_DELIVERY_TIMEOUT, 1, 3));
    let (mut client, rid) = establish_filtered_subscription(ALIAS, params);

    let stream_id = DataStreamId(304);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: true,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );

    // 先頭 Object は timeout Property の値 9 が [1, 3] の外で FilteredOut になる。
    // 帰属先が無いため largest_received_location も delivery timeout override も更新されない
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &object_with_properties(0, delivery_timeout_properties(9))
            )
            .expect("フィルタ不通過は Err にならないこと"),
        TrackDataAcceptance::FilteredOut
    );

    // End of Group status のみの Object も Property が無いため FilteredOut になり、
    // 終端が記録されない。記録されていれば後続の Object ID=3 が Malformed Track になる
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &status_object(2, OBJECT_STATUS_END_OF_GROUP))
            .expect("フィルタ不通過は Err にならないこと"),
        TrackDataAcceptance::FilteredOut
    );
    assert!(
        client
            .subscription(rid)
            .expect("subscription が存在すること")
            .ended_groups
            .is_empty(),
        "FilteredOut の End of Group が ended_groups に記録されないこと"
    );

    // PRIOR_OBJECT_ID_GAP 付きの FilteredOut Object が peer_object_properties に
    // 記録されない。記録されていれば gap [0, 4] に入る Object ID=3 が Malformed Track になる
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &object_with_properties(5, prior_object_id_gap_properties(5))
            )
            .expect("フィルタ不通過は Err にならないこと"),
        TrackDataAcceptance::FilteredOut
    );

    // フィルタを通過する Object は受理され、状態が更新される。先頭 Object は既に
    // FilteredOut として消費済みのため、timeout Property 付きでも override は登録されない
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &object_with_properties(3, delivery_timeout_properties(2))
            )
            .expect("フィルタ通過 Object は受理されること"),
        TrackDataAcceptance::Accepted
    );
    let sub = client
        .subscription(rid)
        .expect("subscription が存在すること");
    assert_eq!(
        sub.largest_received_location
            .as_ref()
            .map(|l| (l.group_id, l.object_id)),
        Some((1, 3)),
        "フィルタ通過後に largest_received_location が更新されること"
    );
    assert!(
        sub.delivery_timeouts.subgroup_overrides.is_empty(),
        "先頭 Object がフィルタ不通過なら後続の受理 Object を先頭扱いしないこと"
    );
    assert!(
        sub.ended_groups.is_empty(),
        "後続の Object が Malformed Track にならず受理されること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// フィルタ不通過 Object でも data stream activity が更新されること
///
/// draft-ietf-moq-transport-21 §12.2 (Session Termination Codes) の
/// DATA_STREAM_TIMEOUT (0x12) は "fields of a stream header or an object header within
/// a data stream" も activity に含む。
/// フィルタ不通過で破棄する Object でも activity を更新し、timeout を誤発火させない。
#[test]
fn filtered_out_object_updates_data_stream_activity() {
    const ALIAS: u64 = 3005;
    let mut params = MessageParameters::new();
    params.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 4));
    let (mut client, _rid) = establish_filtered_subscription(ALIAS, params);
    client.tick(0);
    client.set_data_stream_timeout_ms(Some(50));

    let stream_id = DataStreamId(305);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );

    // Object ID=9 は FilteredOut だが activity を更新する
    client.tick(40);
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(9))
            .expect("フィルタ不通過は Err にならないこと"),
        TrackDataAcceptance::FilteredOut
    );

    // header から 60ms (timeout 50ms 超過) でも Object 受信から 20ms なので閉じない
    client.tick(60);
    let mut closed = false;
    while let Some(event) = client.poll_event() {
        if matches!(event, SessionEvent::CloseSession(_)) {
            closed = true;
            break;
        }
    }
    assert!(
        !closed,
        "FilteredOut Object の受信で DATA_STREAM_TIMEOUT が誤発火しないこと"
    );

    // Object 受信から 50ms 以上経過すると timeout で閉じる
    client.tick(91);
    let mut got = false;
    while let Some(event) = client.poll_event() {
        if let SessionEvent::CloseSession(err) = event {
            assert_eq!(
                err.code, SESSION_DATA_STREAM_TIMEOUT,
                "FilteredOut 後も activity が止まれば DATA_STREAM_TIMEOUT で閉じること"
            );
            got = true;
            break;
        }
    }
    assert!(
        got,
        "Object 受信が止まると DATA_STREAM_TIMEOUT で CloseSession が発火すること"
    );
}

/// キャンセル済み subscription への Object は Discarded として返ること
#[test]
fn cancelled_subscription_object_returns_discarded() {
    const ALIAS: u64 = 3006;
    let (mut client, rid) = establish_filtered_subscription(ALIAS, MessageParameters::new());
    let stream_id = DataStreamId(306);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted,
        "header 受理後にキャンセルすること"
    );
    client
        .stop_sending(rid)
        .expect("Established の subscription は stop_sending できること");

    // 既存 stream への Object はキャンセル由来候補のみ合格のため Discarded が返る
    // (stream は Discarded variant へ移さず、候補評価のたびに破棄判定する)
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(0))
            .expect("キャンセル後の Object 受信は Err にならないこと"),
        TrackDataAcceptance::Discarded
    );

    // キャンセル後の新規 stream も Discarded になり、その Object も Discarded が返る
    let stream_id2 = DataStreamId(307);
    assert_eq!(
        recv_header(&mut client, stream_id2, &header),
        TrackDataAcceptance::Discarded
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_id2, &normal_object(0))
            .expect("Discarded stream への Object 受信は Err にならないこと"),
        TrackDataAcceptance::Discarded
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 合格がキャンセル済み候補のみの Object が Discarded として返ること
///
/// draft-ietf-moq-transport-21 §3.1.2 (Track Alias):
/// "Subscribers SHOULD retain sufficient state to quickly discard these unwanted Objects"
/// 候補ループは datagram の `recv_object_datagram` と同じ規則で、キャンセル由来候補を
/// 帰属対象から除外しつつフィルタ評価だけは実行する。
#[test]
fn object_matching_only_cancelled_candidate_returns_discarded() {
    const ALIAS: u64 = 3007;
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let mut params2 = MessageParameters::new();
    params2.push(range_filter(PARAM_OBJECTID_FILTER, 0, 10, 19));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, params2);
    client
        .stop_sending(rid2)
        .expect("Established の subscription は stop_sending できること");

    // header は Established の rid1 に受理される
    let stream_id = DataStreamId(308);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );

    // Object ID=15 は rid1 が不合格、キャンセル済みの rid2 だけが合格する
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(15))
            .expect("キャンセル後の Object 受信は Err にならないこと"),
        TrackDataAcceptance::Discarded,
        "合格がキャンセル由来候補のみの場合は Discarded になること"
    );
    assert_eq!(
        largest_received(&client, rid1),
        None,
        "Established の rid1 の状態は更新されないこと"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// PRIORITY_FILTER は wire の SUBGROUP_HEADER priority を候補ごとに解決して評価すること
///
/// 共有 Track Alias では `Subscription::publisher_priority` が別の stream の header で
/// 上書きされうるため、購読単位の直近値ではなく SUBGROUP_HEADER が持つ Publisher Priority を
/// 候補ごとに `resolve_header_publisher_priority` で解決して評価する
/// (header 時の `header_passes_filters` と同じ規則)。
#[test]
fn priority_filter_uses_stream_header_priority_for_each_candidate() {
    const ALIAS: u64 = 3009;
    // rid1 は Object ID [0, 9] のみ通す。header 時点の Object 単位フィルタは評価されない
    // ため、header は最初の候補 rid1 に紐づく
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    // rid2 は Publisher Priority [100, 110] のみ通す
    let mut params2 = MessageParameters::new();
    params2.push(range_filter(PARAM_PRIORITY_FILTER, 0, 100, 110));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, params2);

    let stream_id = DataStreamId(311);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(105),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );

    // Object ID=50 は rid1 の [0, 9] に該当しない。rid2 には header の priority 105 が
    // 適用されて PRIORITY_FILTER を通過する (購読単位の直近値 128 だと不通過になる)
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(50))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "header priority 105 で rid2 の PRIORITY_FILTER を通過すること"
    );
    assert_eq!(
        largest_received(&client, rid1),
        None,
        "rid1 は Object 単位フィルタで不合格のため更新されないこと"
    );
    assert_eq!(
        largest_received(&client, rid2),
        Some((0, 50)),
        "rid2 に帰属されること"
    );
}

/// 先頭 Object の delivery timeout override が帰属先 subscription に登録され、
/// stream 終端でその帰属先から削除されること
///
/// header 時に紐づいた stream 所有者と Object の帰属先が異なる共有 Track Alias でも、
/// override が残留しないことを検証する。
#[test]
fn delivery_timeout_override_is_removed_from_attributed_subscription() {
    const ALIAS: u64 = 3010;
    // rid1 は Object ID [0, 9] のみ通すため header は rid1 に紐づき、Object ID=50 は
    // 2 番目の候補 rid2 (unfiltered) に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(312);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: true,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );

    // 先頭 Object ID=50 は rid2 に帰属し、OBJECT_DELIVERY_TIMEOUT の override が
    // rid2 にのみ登録される
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &object_with_properties(50, delivery_timeout_properties(7))
            )
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .delivery_timeouts
            .subgroup_overrides
            .is_empty(),
        "stream 所有者 rid1 には override が登録されないこと"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .delivery_timeouts
            .subgroup_overrides
            .get(&(0, 0)),
        Some(&(None, Some(7))),
        "帰属先 rid2 に override が登録されること"
    );

    // FIN で stream が終端すると override は帰属先 rid2 から削除される
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("stream 終端の処理に成功すること");
    assert!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .delivery_timeouts
            .subgroup_overrides
            .is_empty(),
        "stream 終端で rid1 に override が残留しないこと"
    );
    assert!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .delivery_timeouts
            .subgroup_overrides
            .is_empty(),
        "stream 終端で帰属先 rid2 の override が削除されること"
    );
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid2, 0, 0),
        (None, None),
        "公開 API の参照結果からも override が消えること"
    );
}

/// header 時に紐づいた subscription がキャンセル済みでも、他候補がフィルタ合格すれば
/// その subscription へ再帰属させること
///
/// キャンセル由来 `Terminated` の候補は帰属対象から除外するが、stream 全体を即時破棄せず
/// 候補ループで Established な候補を探す (datagram 経路と同じ規則)。
#[test]
fn cancelled_stream_owner_reattributes_object_to_other_candidate() {
    const ALIAS: u64 = 3011;
    let (mut client, mut server, rid1, rid2) =
        establish_shared_alias_pair(ALIAS, MessageParameters::new(), MessageParameters::new());

    let stream_id = DataStreamId(313);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted,
        "header は最初の候補 rid1 に受理されること"
    );

    // rid1 をキャンセルしても Established な rid2 がフィルタ合格するため、
    // Object は破棄されず rid2 に再帰属する
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(4))
            .expect("キャンセル後の Object 受信は Err にならないこと"),
        TrackDataAcceptance::Accepted,
        "キャンセル済み所有者の stream でも他候補へ再帰属すること"
    );
    assert_eq!(
        largest_received(&client, rid1),
        None,
        "キャンセル済みの rid1 の状態は更新されないこと"
    );
    assert_eq!(
        largest_received(&client, rid2),
        Some((0, 4)),
        "Established な rid2 に帰属して更新されること"
    );

    // 全候補がキャンセル済みになったら Discarded のままとなり、状態は更新されない
    client
        .stop_sending(rid2)
        .expect("Established の subscription は stop_sending できること");
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(5))
            .expect("全候補キャンセル後の Object 受信は Err にならないこと"),
        TrackDataAcceptance::Discarded,
        "全候補キャンセルなら Discarded になること"
    );
    assert_eq!(
        largest_received(&client, rid2),
        Some((0, 4)),
        "Discarded で rid2 の状態は更新されないこと"
    );

    // FIN で open 中の受信 stream 数が戻り、subscription が回収可能になること
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        1,
        "FIN 前は stream 所有者 rid1 の open 数が 1 であること"
    );
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("stream 終端の処理に成功すること");
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "stream 終端で open 中の受信 stream 数が戻ること"
    );
    // キャンセル由来 Terminated の cleanup_ready は常に true になるため、PUBLISH_DONE を
    // 受信して open 数の計上漏れを検出する (漏れがあれば cleanup_ready が false のまま)
    send_publish_done_and_expire_drain(&mut client, &mut server, rid1);
    assert_eq!(
        client.subscription_cleanup_ready(rid1),
        Some(true),
        "open 中の受信 stream 数が漏れず cleanup_ready になること"
    );
    client
        .forget_subscription(rid1)
        .expect("cleanup_ready な subscription は forget できること");
    assert_eq!(client.state(), SessionState::Established);
}

/// キャンセル済み所有者の stream への STOP_SENDING 送信でも open 数が漏れないこと
///
/// header 受理後に所有者をキャンセルした stream は `Subgroup` variant のまま残るため、
/// STOP_SENDING 送信時も `note_incoming_stream_closed` で会計を戻す必要がある。
#[test]
fn cancelled_stream_owner_stop_sending_releases_open_count() {
    const ALIAS: u64 = 3013;
    let (mut client, rid) = establish_filtered_subscription(ALIAS, MessageParameters::new());

    let stream_id = DataStreamId(315);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    client
        .stop_sending(rid)
        .expect("Established の subscription は stop_sending できること");

    client
        .send_data_stream_stop_sending(stream_id)
        .expect("キャンセル後の STOP_SENDING 送信は Err にならないこと");
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "STOP_SENDING 送信で open 中の受信 stream 数が戻ること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// stream 所有者の `forget_subscription` で Object の帰属先が別 subscription の stream は
/// 帰属先へ移管され、以後も帰属先が Object を受信し続けられること
///
/// 共有 Track Alias では Object の帰属先が stream 所有者と異なりうる。所有者の forget で
/// stream ごと除去すると帰属先への受信が黙って止まるため、帰属先へ `request_id` を
/// 付け替えて stream を維持する。移管時は帰属先の `incoming_subgroup_count` /
/// `open_incoming_subgroup_count` に 1 本加算し、override は帰属先に登録済みのため
/// 削除しない。FIN で会計と override が一致して解消されることまで検証する。
#[test]
fn forget_stream_owner_migrates_stream_to_attributed_subscription() {
    const ALIAS: u64 = 3014;
    // rid1 は Object ID [0, 9] のみ通すため header は rid1 に紐づき、Object ID=50 は
    // 2 番目の候補 rid2 (unfiltered) に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, mut server, rid1, rid2) =
        establish_shared_alias_pair(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(316);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: true,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &object_with_properties(50, delivery_timeout_properties(7))
            )
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .delivery_timeouts
            .subgroup_overrides
            .get(&(0, 0)),
        Some(&(None, Some(7))),
        "帰属先 rid2 に override が登録されること"
    );

    // stream 所有者 rid1 をキャンセルして forget する。stream は帰属実績 (rid2) があるため
    // 除去されず、帰属先 rid2 へ移管される
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    client
        .forget_subscription(rid1)
        .expect("キャンセル由来 Terminated は cleanup_ready で forget できること");
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .stream_counts
            .incoming_subgroup_count,
        1,
        "移管した stream が帰属先 rid2 の受信 stream 数に加算されること"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        1,
        "移管した stream が帰属先 rid2 の open 中の受信 stream 数に加算されること"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .delivery_timeouts
            .subgroup_overrides
            .get(&(0, 0)),
        Some(&(None, Some(7))),
        "移管先 rid2 の override は削除されず維持されること"
    );

    // 移管後も帰属先 rid2 が Object を受信し続けられること
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(60))
            .expect("移管後の Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "所有者の forget 後も帰属先 rid2 が Object を受信できること"
    );
    assert_eq!(
        largest_received(&client, rid2),
        Some((0, 60)),
        "移管後も帰属先 rid2 の状態が更新されること"
    );

    // FIN まで会計が一致し、override が帰属先から削除されること
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("移管後の stream 終端の処理に成功すること");
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "FIN で帰属先 rid2 の open 中の受信 stream 数が戻ること"
    );
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid2, 0, 0),
        (None, None),
        "FIN で帰属先 rid2 の override が削除されること"
    );
    // PUBLISH_DONE 受信後の cleanup_ready は open 数に依存するため、会計の漏れが
    // あれば true にならない
    send_publish_done_and_expire_drain(&mut client, &mut server, rid2);
    assert_eq!(
        client.subscription_cleanup_ready(rid2),
        Some(true),
        "移管した stream の open 数が漏れず cleanup_ready になること"
    );
    client
        .forget_subscription(rid2)
        .expect("cleanup_ready な subscription は forget できること");
    assert_eq!(client.state(), SessionState::Established);
}

/// Established な候補が複数同時に合格する場合は最初の候補へ帰属すること
///
/// 候補ループは datagram の `recv_object_datagram` と同じく、最初に通過した候補で
/// break して以降の候補を評価対象にしない (候補順)。
#[test]
fn first_passing_established_candidate_wins() {
    const ALIAS: u64 = 3012;
    // rid1 は Object ID [0, 9] のみ通し、Object ID=50 では不合格になる
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    // rid2 / rid3 は unfiltered で両方合格する
    let (mut client, rid1, rid2, rid3) = establish_shared_alias_three(
        ALIAS,
        params1,
        MessageParameters::new(),
        MessageParameters::new(),
    );

    let stream_id = DataStreamId(314);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );

    // Object ID=50 は rid1 が不合格、rid2 と rid3 が合格する。候補順で先の rid2 に帰属する
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(50))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        largest_received(&client, rid1),
        None,
        "フィルタ不合格の rid1 は更新されないこと"
    );
    assert_eq!(
        largest_received(&client, rid2),
        Some((0, 50)),
        "最初に合格した候補 rid2 に帰属すること"
    );
    assert_eq!(
        largest_received(&client, rid3),
        None,
        "後続の合格候補 rid3 には帰属しないこと"
    );
}

/// Object の帰属実績が無い（全候補でフィルタ不通過）キャンセル済み所有者の stream では
/// `report_mid_object_fin` が破棄対象として吸収されること
///
/// Object を 1 つも受理していない stream は帰属先 subscription にとって生きた stream では
/// ないため、FIN / STOP_SENDING と同じ後始末 (open 数・delivery timeout override・保持集合)
/// を行って no-op で受理する。後始末漏れは open_incoming_subgroup_count の張り付き
/// (subscription リーク) として現れる。
#[test]
fn report_mid_object_fin_absorbs_stream_without_attribution() {
    const ALIAS: u64 = 3015;
    // rid1 は Object ID [0, 9]、rid2 は [10, 19] のみ通すため、Object ID=50 は
    // どちらのフィルタも通らず帰属実績が付かない (header は最初の候補 rid1 に紐づく)
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let mut params2 = MessageParameters::new();
    params2.push(range_filter(PARAM_OBJECTID_FILTER, 0, 10, 19));
    let (mut client, mut server, rid1, _rid2) =
        establish_shared_alias_pair(ALIAS, params1, params2);

    let stream_id = DataStreamId(317);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted,
        "header は最初の候補 rid1 に受理されること"
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(50))
            .expect("フィルタ不通過は Err にならないこと"),
        TrackDataAcceptance::FilteredOut,
        "Object ID=50 はどの候補も通らず帰属実績が付かないこと"
    );

    // stream 所有者 rid1 をキャンセルしてから mid-object FIN を報告する。stream は
    // Subgroup variant のまま残っているため、破棄対象としての後始末が必要になる
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    client
        .report_mid_object_fin(stream_id)
        .expect("帰属実績の無い破棄対象 stream では no-op で受理されること");

    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "report_mid_object_fin で open 中の受信 stream 数が戻ること"
    );

    // 除去した stream id は保持集合へ移り、以後の終端通知も no-op で吸収される
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("report_mid_object_fin 後の FIN も no-op で吸収されること");

    // PUBLISH_DONE 受信後の cleanup_ready は open 数に依存するため、後始末漏れが
    // あれば true にならない
    send_publish_done_and_expire_drain(&mut client, &mut server, rid1);
    assert_eq!(
        client.subscription_cleanup_ready(rid1),
        Some(true),
        "open 中の受信 stream 数が漏れず cleanup_ready になること"
    );
    client
        .forget_subscription(rid1)
        .expect("cleanup_ready な subscription は forget できること");
    assert_eq!(client.state(), SessionState::Established);
}

/// Object の帰属実績がある stream では `report_mid_object_fin` が破棄対象として
/// 吸収されず、draft §11.3 の SHOULD どおりセッションを `PROTOCOL_VIOLATION` で
/// 閉じること
///
/// 帰属実績がある stream は帰属先 subscription にとって生きた stream であり、
/// 所有者のキャンセルだけを理由に吸収すると帰属先への受信を黙って止めることになる。
#[test]
fn report_mid_object_fin_with_attribution_closes_session() {
    const ALIAS: u64 = 3018;
    // rid1 は Object ID [0, 9] のみ通すため header は rid1 に紐づき、Object ID=50 は
    // 2 番目の候補 rid2 (unfiltered) に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, _server, rid1, rid2) =
        establish_shared_alias_pair(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(322);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: true,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted,
        "header は最初の候補 rid1 に受理されること"
    );
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &object_with_properties(50, delivery_timeout_properties(7))
            )
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=50 は rid2 に帰属すること"
    );
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid2, 0, 0),
        (None, Some(7)),
        "帰属先 rid2 に override が登録されること"
    );

    // 所有者 rid1 をキャンセルしても、帰属実績 (rid2) がある stream は吸収されず、
    // §11.3 の SHOULD に従ってセッションを閉じる
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    let err = client
        .report_mid_object_fin(stream_id)
        .expect_err("帰属実績のある stream の mid-object FIN はセッションを閉じること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert!(
        matches!(client.state(), SessionState::Closing | SessionState::Closed),
        "帰属実績のある mid-object FIN でセッションが閉じること"
    );
}

/// END_OF_GROUP + FIN の Group 終端確定が stream 所有者ではなく Object の帰属先へ
/// 反映されること
///
/// draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header): END_OF_GROUP bit + FIN は
/// 「その Group の最終 Object」を確定する ("the subscriber can infer the final Object in
/// the Group when the data stream is terminated by a FIN")。共有 Track Alias で Object の
/// 帰属先が stream 所有者と異なる場合、終端を帰属先に記録しないと帰属先で Malformed
/// Track (§12.1 条件 4) を検出できない。
#[test]
fn end_of_group_fin_records_group_end_on_attributed_subscription() {
    const ALIAS: u64 = 3016;
    // rid1 は Object ID [0, 9] のみ通すため header は rid1 に紐づき、Object ID=50 は
    // 2 番目の候補 rid2 (unfiltered) に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(318);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: true,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted,
        "header は最初の候補 rid1 に受理されること"
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(50))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=50 は rid2 に帰属すること"
    );

    // END_OF_GROUP + FIN: Group 0 の最終 Object は 50 なので、存在しない最小 Object ID の
    // 51 が帰属先 rid2 に記録される
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("stream 終端の処理に成功すること");
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .ended_groups
            .get(&0),
        None,
        "stream 所有者 rid1 には Group 終端を記録しないこと"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .ended_groups
            .get(&0),
        Some(&51),
        "帰属先 rid2 に Group 終端 (50 の次 = 51) が記録されること"
    );

    // 同一 Group の別 Subgroup で 51 以上の Object ID を送ると Malformed Track (§12.1 条件 4)
    let stream_id2 = DataStreamId(319);
    let header2 = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(1),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id2, &header2),
        TrackDataAcceptance::Accepted,
        "別 Subgroup の header は受理されること"
    );
    let err = client
        .recv_subgroup_object(stream_id2, &normal_object(60))
        .expect_err("Group 終端後の大きい Object ID は Malformed Track になること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .state,
        SubscriptionState::Terminated,
        "Malformed の終端対象が Object の帰属先 rid2 になること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 所有者をキャンセルした後の END_OF_GROUP + FIN も、帰属実績があれば帰属先へ
/// Group 終端を記録すること
///
/// 帰属実績がある stream は帰属先にとって生きた stream であり、所有者のキャンセルで
/// tracker 更新と Group 終端確定を丸ごと skip してはならない。
#[test]
fn end_of_group_fin_records_group_end_after_owner_cancel() {
    const ALIAS: u64 = 3017;
    // rid1 は Object ID [0, 9] のみ通すため header は rid1 に紐づき、Object ID=50 は
    // 2 番目の候補 rid2 (unfiltered) に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(320);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: true,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted,
        "header は最初の候補 rid1 に受理されること"
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(50))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=50 は rid2 に帰属すること"
    );
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");

    // 所有者 rid1 はキャンセル済みでも、帰属実績 (rid2) があるため通常終端として扱い、
    // tracker を更新して Group 終端を帰属先に記録する
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("帰属実績のある stream の終端は通常どおり処理されること");
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "所有者 rid1 の open 中の受信 stream 数が戻ること"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .ended_groups
            .get(&0),
        Some(&51),
        "帰属先 rid2 に Group 終端 (50 の次 = 51) が記録されること"
    );

    // 同一 Group の別 Subgroup で 51 以上の Object ID を送ると Malformed Track (§12.1 条件 4)
    let stream_id2 = DataStreamId(321);
    let header2 = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(1),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id2, &header2),
        TrackDataAcceptance::Accepted,
        "キャンセル済み所有者が残っていても Established な rid2 に受理されること"
    );
    let err = client
        .recv_subgroup_object(stream_id2, &normal_object(60))
        .expect_err("Group 終端後の大きい Object ID は Malformed Track になること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .state,
        SubscriptionState::Terminated,
        "Malformed の終端対象が Object の帰属先 rid2 になること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 帰属先の `forget_subscription` 後も、死んだ帰属実績を根拠にセッションを閉じたり
/// 誤った subscription を終端したりしないこと
///
/// 共有 Track Alias では Object の帰属先が stream 所有者と異なりうる。帰属先を forget すると
/// `last_attributed_request_id` が死んだ request_id を指したまま残るため、生存判定を行わずに
/// 「帰属実績あり」と扱うと、mid-object FIN の吸収判定や FIN 時の Group 終端確定が誤動作する
/// (draft §3.1.2 (Track Alias) の「不要 Object の即時破棄」に反する)。
/// 所有者 (rid1)・帰属先 (rid2) をキャンセルして rid2 を forget した後に
/// `report_mid_object_fin` が no-op で吸収されること、END_OF_GROUP + FIN が所有者側の記録や
/// Malformed 終端で panic / 誤終端しないことを検証する。
#[test]
fn stale_attribution_after_forget_is_absorbed() {
    const ALIAS: u64 = 3019;
    // rid1 (stream 所有者) は Object ID [0, 9] のみ通し、Object ID=50 は unfiltered の
    // 2 番目の候補 rid2 に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, MessageParameters::new());

    // mid-object FIN を報告する stream
    let stream_mid = DataStreamId(323);
    let header_mid = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_mid, &header_mid),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_mid, &normal_object(50))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=50 は rid2 に帰属すること"
    );

    // END_OF_GROUP + FIN を報告する stream
    let stream_fin = DataStreamId(324);
    let header_fin = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(1),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: true,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_fin, &header_fin),
        TrackDataAcceptance::Accepted
    );
    // 同一 (group, object) の重複不一致 (§12.1 条件 7) を避けるため別の Object ID を使う
    assert_eq!(
        client
            .recv_subgroup_object(stream_fin, &normal_object(60))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=60 は rid2 に帰属すること"
    );

    // 所有者 rid1 と帰属先 rid2 をキャンセルし、帰属先だけを forget する。
    // 以後 last_attributed_request_id は死んだ rid2 を指したままになる
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    client
        .stop_sending(rid2)
        .expect("Established の subscription は stop_sending できること");
    client
        .forget_subscription(rid2)
        .expect("キャンセル由来 Terminated は cleanup_ready で forget できること");

    // 死んだ帰属先しかない stream の mid-object FIN は破棄対象として吸収され、
    // セッションを閉じない
    client
        .report_mid_object_fin(stream_mid)
        .expect("死んだ帰属先しかない stream の mid-object FIN は no-op で受理されること");
    assert_eq!(client.state(), SessionState::Established);
    // 除去した stream id は保持集合へ移り、以後の終端通知も no-op で吸収される
    client
        .recv_data_stream_closed(stream_mid, RequestStreamEnd::Fin)
        .expect("report_mid_object_fin 後の FIN も no-op で吸収されること");
    assert_eq!(
        client
            .subscription(rid1)
            .expect("所有者 rid1 はキャンセル後も forget まで残ること")
            .stream_counts
            .open_incoming_subgroup_count,
        1,
        "stream_mid の破棄後は stream_fin の 1 本だけが open であること"
    );

    // END_OF_GROUP + FIN は死んだ帰属先ではなく所有者へフォールバックする。所有者は
    // キャンセル由来のため subscription スコープの Group 終端は記録せず、wire 構造の
    // 終端だけを処理して panic / 誤終端しない
    client
        .recv_data_stream_closed(stream_fin, RequestStreamEnd::Fin)
        .expect("死んだ帰属先を指す stream の END_OF_GROUP + FIN も正常終端すること");
    assert!(
        client
            .subscription(rid1)
            .expect("所有者 rid1 はキャンセル後も forget まで残ること")
            .ended_groups
            .is_empty(),
        "キャンセル済み所有者に Group 終端を記録しないこと"
    );
    assert_eq!(
        client
            .subscription(rid1)
            .expect("所有者 rid1 はキャンセル後も forget まで残ること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "FIN で open 中の受信 stream 数が戻ること"
    );
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(
        client.subscription_cleanup_ready(rid1),
        Some(true),
        "open 中の受信 stream 数が漏れず cleanup_ready になること"
    );
}

/// FIN を先に処理してから mid-object FIN を報告しても、死んだ帰属先しかない stream は
/// 破棄対象として吸収されること
///
/// `report_mid_object_fin` は `recv_data_stream_closed` との呼び出し順序を問わない契約のため、
/// FIN → report の順でも no-op で吸収される必要がある。破棄分岐に入らず保持集合へ移らないと、
/// 後続の `report_mid_object_fin` が未知 stream として PROTOCOL_VIOLATION でセッションを閉じる。
#[test]
fn stale_attribution_fin_before_report_mid_object_is_absorbed() {
    const ALIAS: u64 = 3021;
    // rid1 (stream 所有者) は Object ID [0, 9] のみ通し、Object ID=50 は unfiltered の
    // 2 番目の候補 rid2 に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(326);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(50))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=50 は rid2 に帰属すること"
    );

    // 所有者 rid1 と帰属先 rid2 をキャンセルし、帰属先だけを forget する。
    // 以後 last_attributed_request_id は死んだ rid2 を指したままになる
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    client
        .stop_sending(rid2)
        .expect("Established の subscription は stop_sending できること");
    client
        .forget_subscription(rid2)
        .expect("キャンセル由来 Terminated は cleanup_ready で forget できること");

    // FIN を先に処理し、その後に mid-object FIN を報告しても順序に依存せず吸収される
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("死んだ帰属先しかない stream の FIN は no-op で吸収されること");
    client
        .report_mid_object_fin(stream_id)
        .expect("FIN 後の mid-object FIN も no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(
        client
            .subscription(rid1)
            .expect("所有者 rid1 はキャンセル後も forget まで残ること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "FIN で open 中の受信 stream 数が戻ること"
    );
}

/// 最後の帰属先が忘却対象自身でも、同じ Track Alias の生きた他候補へ stream を移管し、
/// 以後の Object をその候補で受信し続けられること
///
/// `remove_incoming_data_streams_for_request` の移管先は、`last_attributed_request_id` が
/// 使えない (忘却対象自身 / 回収済み / キャンセル済み) 場合に同じ alias の生きた他候補へ
/// フォールバックする。移管しないと所有者の forget で帰属先への受信が黙って止まる。
#[test]
fn forget_stream_owner_migrates_to_live_alias_candidate() {
    const ALIAS: u64 = 3020;
    // rid1 (stream 所有者) は unfiltered で最初の候補、rid2 は Object ID [10, 99] のみ通す。
    // 先頭 Object ID=5 は候補順で rid1 自身に帰属する (rid2 のフィルタは不合格)
    let mut params2 = MessageParameters::new();
    params2.push(range_filter(PARAM_OBJECTID_FILTER, 0, 10, 99));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, MessageParameters::new(), params2);

    let stream_id = DataStreamId(325);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(5))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=5 は最初の候補 rid1 自身に帰属すること"
    );
    assert_eq!(largest_received(&client, rid1), Some((0, 5)));
    assert_eq!(largest_received(&client, rid2), None);

    // rid1 をキャンセルして forget する。最後の帰属先が忘却対象自身のため、
    // 従来は stream ごと除去されていたが、同じ alias の生きた他候補 rid2 へ移管される
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    client
        .forget_subscription(rid1)
        .expect("キャンセル由来 Terminated は cleanup_ready で forget できること");

    // 会計: 移管した stream が rid2 の受信数 / open 数へ加算される
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .stream_counts
            .incoming_subgroup_count,
        1,
        "移管した stream が rid2 の受信 stream 数に加算されること"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        1,
        "移管した stream が rid2 の open 中の受信 stream 数に加算されること"
    );

    // 移管後も Object が rid2 へ Accepted で届く
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(15))
            .expect("移管後の Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "移管先 rid2 が Object を受信し続けられること"
    );
    assert_eq!(
        largest_received(&client, rid2),
        Some((0, 15)),
        "移管先 rid2 の状態が更新されること"
    );

    // FIN まで会計が一致する
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("移管後の stream 終端の処理に成功すること");
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "FIN で移管先 rid2 の open 中の受信 stream 数が戻ること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 移管時に delivery timeout override の所有者が死んだ id のまま残らないこと
///
/// 先頭 Object の帰属先が忘却対象 (rid1) の場合、override は回収された subscription と
/// 一緒に消える。`timeout_override_request_id` を生存判定で正規化しないと、以後の
/// request_id 再利用で無関係な subscription の override を削除してしまう。
#[test]
fn migration_normalizes_dead_timeout_override_owner() {
    const ALIAS: u64 = 3021;
    // rid1 unfiltered / rid2 は Object ID [10, 99] のみ通す
    let mut params2 = MessageParameters::new();
    params2.push(range_filter(PARAM_OBJECTID_FILTER, 0, 10, 99));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, MessageParameters::new(), params2);

    let stream_id = DataStreamId(326);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: true,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    // 先頭 Object ID=5 は rid1 に帰属し、OBJECT_DELIVERY_TIMEOUT の override が
    // rid1 に登録される
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &object_with_properties(5, delivery_timeout_properties(7))
            )
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid1, 0, 0),
        (None, Some(7)),
        "帰属先 rid1 に override が登録されること"
    );

    // rid1 をキャンセルして forget する。override は回収された rid1 と一緒に消え、
    // 移管先 rid2 へ引き継がれない (timeout_override_request_id が死んだ id のまま残らない)
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    client
        .forget_subscription(rid1)
        .expect("キャンセル由来 Terminated は cleanup_ready で forget できること");
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid2, 0, 0),
        (None, None),
        "回収済み subscription の override が移管先へ残留しないこと"
    );

    // 移管後も会計と終端が整合する
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(15))
            .expect("移管後の Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("移管後の stream 終端の処理に成功すること");
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid2, 0, 0),
        (None, None),
        "FIN 後も override が残留しないこと"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "FIN で移管先 rid2 の open 中の受信 stream 数が戻ること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 移管後も生きた override 所有者の参照を維持し、FIN で確実に削除すること
///
/// 最後の帰属先と override 所有者が異なる場合でも、override 所有者が現在も生きていれば
/// `timeout_override_request_id` の参照を維持する。移管先の終端で override 削除経路が
/// no-op にならず、生きた所有者から削除できることを検証する。
#[test]
fn migration_keeps_live_timeout_override_owner_until_fin() {
    const ALIAS: u64 = 3022;
    // rid1 は Object ID [10, 19] のみ通し、rid3 (unfiltered) が先頭 Object の帰属先になる
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 10, 19));
    let (mut client, rid1, rid3) = establish_shared_alias(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(327);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: true,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    // 先頭 Object ID=5 は rid1 がフィルタ不合格となり、unfiltered の rid3 に帰属する。
    // override は rid3 に登録される
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &object_with_properties(5, delivery_timeout_properties(7))
            )
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid3, 0, 0),
        (None, Some(7)),
        "先頭 Object の帰属先 rid3 に override が登録されること"
    );
    // 2 番目の Object ID=15 は rid1 が合格し、最後の帰属先は rid1 になる
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(15))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(largest_received(&client, rid1), Some((0, 15)));

    // rid1 を forget すると移管先は同じ alias の生きた他候補 rid3 になり、
    // override 所有者 rid3 は生きたままなので参照が維持される
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    client
        .forget_subscription(rid1)
        .expect("キャンセル由来 Terminated は cleanup_ready で forget できること");
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid3, 0, 0),
        (None, Some(7)),
        "移管後も生きた override 所有者の参照が維持されること"
    );

    // 移管後も Object を受信でき、FIN で override が生きた所有者から削除される
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(25))
            .expect("移管後の Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("移管後の stream 終端の処理に成功すること");
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid3, 0, 0),
        (None, None),
        "FIN で生きた override 所有者 rid3 から override が削除されること"
    );
    assert_eq!(
        client
            .subscription(rid3)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "FIN で移管先 rid3 の open 中の受信 stream 数が戻ること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 生きた帰属先がある stream への再 SUBGROUP_HEADER は破棄吸収せず拒否すること
///
/// キャンセル由来 `Terminated` の所有者でも、Object の帰属実績の参照先が現在も生きた
/// subscription である stream は破棄対象ではない。再 SUBGROUP_HEADER を no-op で吸収すると
/// 帰属先への受信を黙って止めるため、通常どおり protocol violation として拒否する。
#[test]
fn re_subgroup_header_with_live_attribution_is_rejected() {
    const ALIAS: u64 = 3023;
    // rid1 (stream 所有者) は Object ID [0, 9] のみ通し、Object ID=50 は unfiltered の
    // 2 番目の候補 rid2 に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(328);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(50))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=50 は rid2 に帰属すること"
    );

    // 所有者 rid1 をキャンセルしても生きた帰属先 rid2 があるため吸収せず、再
    // SUBGROUP_HEADER を protocol violation として拒否する
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    let err = client
        .recv_subgroup_header(stream_id, &header)
        .expect_err("生きた帰属先がある stream への再 SUBGROUP_HEADER は拒否されること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Established);

    // 拒否後も stream は帰属先 rid2 が受信し続けられる
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(60))
            .expect("拒否後の Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "帰属先 rid2 が Object を受信し続けられること"
    );
    assert_eq!(largest_received(&client, rid2), Some((0, 60)));
}

/// 生きた帰属先がある stream への STOP_SENDING 送信は破棄分岐ではなく通常分岐で
/// tracker と会計を処理すること
///
/// キャンセル由来 `Terminated` の所有者でも、帰属先にとって生きた stream は
/// `mark_stop_sending` で終端状態にして再オープンを可能にし、所有者の会計を戻す。
#[test]
fn stop_sending_with_live_attribution_takes_normal_path() {
    const ALIAS: u64 = 3024;
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(329);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(50))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=50 は rid2 に帰属すること"
    );

    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    client
        .send_data_stream_stop_sending(stream_id)
        .expect("生きた帰属先がある stream への STOP_SENDING は通常分岐で処理されること");
    assert_eq!(
        client
            .subscription(rid1)
            .expect("所有者 rid1 はキャンセル後も forget まで残ること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "通常分岐で所有者 rid1 の open 中の受信 stream 数が戻ること"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .state,
        SubscriptionState::Established,
        "帰属先 rid2 は Established のままであること"
    );
    assert_eq!(client.state(), SessionState::Established);

    // tracker が StoppedByPeer として終端されるため、同一 Subgroup の再オープンが受理される
    let stream_id2 = DataStreamId(330);
    assert_eq!(
        recv_header(&mut client, stream_id2, &header),
        TrackDataAcceptance::Accepted,
        "STOP_SENDING 後の同一 Subgroup 再オープンが受理されること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 移管先の生きた候補が無い場合は従来どおり stream を除去し、保持集合で遅延 Object と
/// 終端を吸収すること
#[test]
fn forget_stream_owner_without_live_candidate_removes_stream() {
    const ALIAS: u64 = 3025;
    let (mut client, rid) = establish_filtered_subscription(ALIAS, MessageParameters::new());

    let stream_id = DataStreamId(331);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(5))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=5 は唯一の候補 rid に帰属すること"
    );

    // rid をキャンセルして forget する。他に生きた候補が無いため stream は除去され、
    // id は破棄対象の保持集合へ移る
    client
        .stop_sending(rid)
        .expect("Established の subscription は stop_sending できること");
    client
        .forget_subscription(rid)
        .expect("キャンセル由来 Terminated は cleanup_ready で forget できること");

    // 保持期間中は遅延 Object / FIN を no-op で吸収する
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(6))
            .expect("forget 後の Object 受信は Err にならないこと"),
        TrackDataAcceptance::Discarded,
        "移管先が無い stream の遅延 Object は Discarded として吸収されること"
    );
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("forget 後の stream 終端は no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);
}
