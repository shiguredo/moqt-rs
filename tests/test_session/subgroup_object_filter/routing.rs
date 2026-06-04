//! フィルタによる Object の振り分けとフィルタ不通過 Object の扱いのテスト

use super::*;
use shiguredo_moqt::error::{SESSION_DATA_STREAM_TIMEOUT, SESSION_PROTOCOL_VIOLATION};
use shiguredo_moqt::message_parameter::{PARAM_OBJECTID_FILTER, PARAM_SUBGROUP_FILTER};
use shiguredo_moqt::stream::OBJECT_STATUS_END_OF_GROUP;
use shiguredo_moqt::track_properties::PROP_OBJECT_DELIVERY_TIMEOUT;

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
        ..subgroup_header()
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
        has_properties: true,
        ..subgroup_header()
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
        ..subgroup_header()
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
        subgroup_id: SubgroupIdMode::FirstObjectId,
        ..subgroup_header()
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
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(10),
        ..subgroup_header()
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
        subgroup_id: SubgroupIdMode::Explicit(7),
        publisher_priority: Some(99),
        ..subgroup_header()
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
        has_properties: true,
        ..subgroup_header()
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
        ..subgroup_header()
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
        ..subgroup_header()
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
