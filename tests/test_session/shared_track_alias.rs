//! 同一 Track への Track Alias 共有テスト
//!
//! draft-ietf-moq-transport-21 §3.1 (Subscriptions):
//! "An endpoint MAY have multiple concurrent subscriptions to the same Track, each identified
//! by a unique Request ID. A publisher MAY assign the same or different Track Aliases to
//! these subscriptions."
//!
//! §3.1.2 (Track Alias):
//! "The same Track Alias MUST NOT be used by a publisher to refer to two different Tracks
//! simultaneously in the same session. If a subscriber receives a PUBLISH or SUBSCRIBE_OK that
//! uses the same Track Alias as a different Track with an Established subscription, it MUST
//! close the session with error DUPLICATE_TRACK_ALIAS."
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use super::*;
use shiguredo_moqt::message_parameter::{PARAM_OBJECTID_FILTER, PARAM_SUBGROUP_FILTER};
use shiguredo_moqt::session::types::SendRequestError;

/// SUBSCRIBE を 1 本送って受信側に届けるところまでを行い request_id を返す
fn send_and_recv_subscribe(
    client: &mut Session,
    server: &mut Session,
    namespace: &[&[u8]],
    track_name: &[u8],
) -> u64 {
    let rid = client
        .send_subscribe(ns(namespace), track_name.to_vec(), MessageParameters::new())
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

/// 同一 Track への 2 subscription が同一 Track Alias で Established できる (SUBSCRIBE_OK 経路)
#[test]
fn same_track_two_subscriptions_share_alias_via_subscribe_ok() {
    const ALIAS: u64 = 1000;
    let (mut client, mut server) = establish_pair();
    let rid1 = send_and_recv_subscribe(&mut client, &mut server, &[b"live"], b"cam");
    let rid2 = send_and_recv_subscribe(&mut client, &mut server, &[b"live"], b"cam");
    assert_ne!(rid1, rid2, "Request ID は subscription ごとに異なる");

    complete_subscribe(&mut client, &mut server, rid1, ALIAS);
    complete_subscribe(&mut client, &mut server, rid2, ALIAS);

    for (label, session) in [("publisher", &server), ("subscriber", &client)] {
        for rid in [rid1, rid2] {
            let sub = session
                .subscription(rid)
                .unwrap_or_else(|| panic!("{label}: subscription {rid} が存在すること"));
            assert_eq!(
                sub.state,
                SubscriptionState::Established,
                "{label}: subscription {rid} が Established になること"
            );
            assert_eq!(
                sub.track_alias,
                Some(ALIAS),
                "{label}: subscription {rid} が同じ alias を持つこと"
            );
        }
    }
}

/// 同一 Track への 2 subscription が同一 Track Alias で Established できる (PUBLISH 経路)
#[test]
fn same_track_two_subscriptions_share_alias_via_publish() {
    const ALIAS: u64 = 1001;
    let (mut client, mut server) = establish_pair();
    let mut rids = Vec::new();
    for _ in 0..2 {
        let rid = client
            .send_publish(
                ns(&[b"live"]),
                b"cam".to_vec(),
                ALIAS,
                MessageParameters::new(),
                TrackProperties::new(),
            )
            .expect("同一 Track への PUBLISH は同じ alias を共有できる");
        let (_, pub_msg) = take_send_request(&mut client);
        server
            .recv_request(pub_msg)
            .expect("同一 Track の共有 alias PUBLISH は受理される");
        rids.push(rid);
    }
    assert_ne!(rids[0], rids[1]);

    for (label, session) in [("publisher", &client), ("subscriber", &server)] {
        for &rid in &rids {
            let sub = session
                .subscription(rid)
                .unwrap_or_else(|| panic!("{label}: subscription {rid} が存在すること"));
            assert_eq!(sub.track_alias, Some(ALIAS), "{label}: {rid}");
        }
    }
    assert_eq!(
        server.state(),
        SessionState::Established,
        "同一 Track の共有 alias でセッションが閉じてはいけない"
    );
}

/// 異なる Track への同一 Alias 再利用は送信 API で拒否される (PUBLISH)
///
/// draft-ietf-moq-transport-21 §3.1.2 の 1 文目 (送信側 MUST NOT)。
#[test]
fn different_track_same_alias_rejected_on_send_publish() {
    const ALIAS: u64 = 1002;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            ALIAS,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("1 本目の PUBLISH は成功する");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("1 本目の受信に成功すること");
    let _ = rid;

    let err = client
        .send_publish(
            ns(&[b"live"]),
            b"mic".to_vec(),
            ALIAS,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect_err("異なる Track への同一 alias は拒否される");
    let SendRequestError::Session(err) = err else {
        panic!("SendRequestError::Session が期待されたが {err:?}");
    };
    assert_eq!(err.code, SESSION_DUPLICATE_TRACK_ALIAS);
}

/// 異なる Track への同一 Alias 再利用は SUBSCRIBE_OK 送信でも拒否される
#[test]
fn different_track_same_alias_rejected_on_send_subscribe_ok() {
    const ALIAS: u64 = 1003;
    let (mut client, mut server) = establish_pair();
    let rid1 = send_and_recv_subscribe(&mut client, &mut server, &[b"live"], b"cam");
    let rid2 = send_and_recv_subscribe(&mut client, &mut server, &[b"live"], b"mic");
    complete_subscribe(&mut client, &mut server, rid1, ALIAS);

    let err = server
        .send_subscribe_ok(
            rid2,
            ALIAS,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect_err("異なる Track への同一 alias は拒否される");
    assert_eq!(err.code, SESSION_DUPLICATE_TRACK_ALIAS);
}

/// 異なる Track の Established subscription と衝突する PUBLISH 受信はセッションを閉じる
///
/// draft-ietf-moq-transport-21 §3.1.2 の 2 文目 (受信側 MUST)。
#[test]
fn different_track_same_alias_closes_session_on_recv_publish() {
    const ALIAS: u64 = 1004;
    let (mut client, mut server) = establish_pair();
    // client (publisher) → server (subscriber) の PUBLISH で alias を Established させる
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            ALIAS,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("PUBLISH の送信に成功すること");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("PUBLISH の受信に成功すること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("PUBLISH_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("PUBLISH_OK の受信に成功すること");
    assert_eq!(
        server
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Established
    );

    // 送信側は自側で弾くため、異なる Track の同一 alias PUBLISH はメッセージ注入で作る
    let injected = ControlMessage::Publish(shiguredo_moqt::message::Publish {
        request_id: rid + 2,
        track_namespace: ns(&[b"live"]),
        track_name: b"mic".to_vec(),
        track_alias: ALIAS,
        parameters: MessageParameters::new(),
        track_properties: TrackProperties::new(),
    });
    let err = server
        .recv_request(injected)
        .expect_err("異なる Track の Established subscription と衝突したらセッションを閉じる");
    let err = err.as_session_error().expect("SessionError が得られること");
    assert_eq!(err.code, SESSION_DUPLICATE_TRACK_ALIAS);
    match drain_until_close(&mut server) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_DUPLICATE_TRACK_ALIAS),
        other => panic!("CloseSession(DUPLICATE_TRACK_ALIAS) が期待されたが {other:?}"),
    }
}

/// 共有 alias の 1 subscription を forget しても他方は data plane で解決できる
#[test]
fn forget_one_shared_alias_subscription_keeps_the_other() {
    const ALIAS: u64 = 1005;
    let (mut client, mut server) = establish_pair();
    let rid1 = send_and_recv_subscribe(&mut client, &mut server, &[b"live"], b"cam");
    let rid2 = send_and_recv_subscribe(&mut client, &mut server, &[b"live"], b"cam");
    complete_subscribe(&mut client, &mut server, rid1, ALIAS);
    complete_subscribe(&mut client, &mut server, rid2, ALIAS);

    // rid1 を Terminated にして forget する
    client
        .stop_sending(rid1)
        .expect("subscriber からの STOP_SENDING に成功すること");
    client
        .forget_subscription(rid1)
        .expect("Terminated な subscription は forget できる");
    assert!(
        client.subscription(rid1).is_none(),
        "forget した subscription は消えること"
    );

    // 残った rid2 が共有 alias の datagram を引き続き解決できること
    let raw = ObjectDatagram {
        track_alias: ALIAS,
        group_id: 3,
        object_id: 1,
        publisher_priority: Some(7),
        properties_data: None,
        end_of_group: false,
        status: None,
    };
    let outcome = client
        .recv_object_datagram(&raw)
        .expect("共有 alias の解決に成功すること");
    assert_eq!(
        outcome,
        TrackDataAcceptance::Accepted,
        "共有相手が残っているので未知 alias にはならない"
    );
    assert!(
        client
            .subscription(rid2)
            .expect("subscription が存在する")
            .largest_received_location
            .is_some(),
        "残った subscription の状態が更新されること"
    );
}

/// 共有 alias の全 subscription を forget したら alias 解決先が無くなる
///
/// 直後は draft-ietf-moq-transport-21 §3.1.2 (Track Alias) の discard 用 tombstone が効くため
/// `Discarded` になり、保持期間が切れると `UnknownTrackAlias` に戻る。
#[test]
fn forget_all_shared_alias_subscriptions_makes_alias_unknown() {
    const ALIAS: u64 = 1006;
    let (mut client, mut server) = establish_pair();
    let rid1 = send_and_recv_subscribe(&mut client, &mut server, &[b"live"], b"cam");
    let rid2 = send_and_recv_subscribe(&mut client, &mut server, &[b"live"], b"cam");
    complete_subscribe(&mut client, &mut server, rid1, ALIAS);
    complete_subscribe(&mut client, &mut server, rid2, ALIAS);

    for rid in [rid1, rid2] {
        client
            .stop_sending(rid)
            .expect("STOP_SENDING に成功すること");
        client
            .forget_subscription(rid)
            .expect("Terminated な subscription は forget できる");
    }

    let raw = ObjectDatagram {
        track_alias: ALIAS,
        group_id: 3,
        object_id: 1,
        publisher_priority: Some(7),
        properties_data: None,
        end_of_group: false,
        status: None,
    };
    // 保持期間中は tombstone が効くので Discarded
    let outcome = client
        .recv_object_datagram(&raw)
        .expect("tombstone 期間中もセッションは閉じない");
    assert_eq!(
        outcome,
        TrackDataAcceptance::Discarded,
        "全 subscription を forget した直後は discard 経路に入ること"
    );

    // 保持期間を過ぎたら従来どおり未知 alias
    client.tick(0);
    client.tick(client.peer_alias_retention_ms() + 1);
    let outcome = client
        .recv_object_datagram(&raw)
        .expect("未知 alias はセッションを閉じない");
    assert_eq!(
        outcome,
        TrackDataAcceptance::UnknownTrackAlias,
        "保持期間を過ぎたら未知 alias に戻ること"
    );
}

/// 共有 alias では publisher 側の largest 更新が subscription ごとに独立する
///
/// 送信 stream は 1 subscription に属するため、alias 索引ではなく stream の request_id を
/// 使って更新しなければならない。
#[test]
fn shared_alias_publisher_updates_are_per_subscription() {
    const ALIAS: u64 = 1007;
    let (mut client, mut server) = establish_pair();
    let rid1 = send_and_recv_subscribe(&mut client, &mut server, &[b"live"], b"cam");
    let rid2 = send_and_recv_subscribe(&mut client, &mut server, &[b"live"], b"cam");
    complete_subscribe(&mut client, &mut server, rid1, ALIAS);
    complete_subscribe(&mut client, &mut server, rid2, ALIAS);

    // publisher (server) が rid2 の stream にだけ object を送る
    let stream_id = DataStreamId(51);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 9,
        subgroup_id: SubgroupIdMode::Explicit(1),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(stream_id, rid2, &header)
        .expect("subgroup header の送信に成功すること");
    server
        .send_subgroup_object(stream_id, 5, None)
        .expect("subgroup object の送信に成功すること");

    assert_eq!(
        server
            .subscription(rid2)
            .expect("subscription が存在する")
            .largest_received_location
            .as_ref()
            .map(|l| (l.group_id, l.object_id)),
        Some((9, 5)),
        "stream が属する subscription が更新されること"
    );
    assert!(
        server
            .subscription(rid1)
            .expect("subscription が存在する")
            .largest_received_location
            .is_none(),
        "stream が属さない subscription は更新されないこと"
    );
}

// ─── フィルタ再適用による振り分け (draft §3.1) ─────────────────

/// MAX_FILTER_RANGES を宣言した server と、フィルタ付き SUBSCRIBE で確立した
/// 同一 Track ・同一 alias の 2 subscription を返す。
///
/// rid1 には OBJECTID_FILTER [0, 9]、rid2 には OBJECTID_FILTER [10, 19] を設定する。
fn establish_shared_alias_with_objectid_filters() -> (Session, Session, u64, u64) {
    const ALIAS: u64 = 2000;
    let (mut client, mut server) = establish_pair_with_options(SetupOptions::new(), {
        let mut opts = SetupOptions::new();
        opts.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_FILTER_RANGES, // MAX_FILTER_RANGES
            value: SetupOptionValue::VarInt(8),
        });
        opts
    });

    // rid1: OBJECTID_FILTER [0, 9]
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let rid1 = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), params1)
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg1) = take_send_request(&mut client);
    server
        .recv_request(sub_msg1)
        .expect("SUBSCRIBE の受信に成功すること");

    // rid2: OBJECTID_FILTER [10, 19]
    let mut params2 = MessageParameters::new();
    params2.push(range_filter(PARAM_OBJECTID_FILTER, 0, 10, 19));
    let rid2 = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), params2)
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg2) = take_send_request(&mut client);
    server
        .recv_request(sub_msg2)
        .expect("SUBSCRIBE の受信に成功すること");

    // 両方に同一 alias を割り当てる
    server
        .send_subscribe_ok(
            rid1,
            ALIAS,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg1) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid1, ok_msg1)
        .expect("SUBSCRIBE_OK の受信に成功すること");

    server
        .send_subscribe_ok(
            rid2,
            ALIAS,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg2) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid2, ok_msg2)
        .expect("SUBSCRIBE_OK の受信に成功すること");

    (client, server, rid1, rid2)
}

/// datagram 経路: OBJECTID_FILTER で異なる subscription に振り分けられる
///
/// draft-ietf-moq-transport-21 §3.1 (Subscriptions):
/// "the subscriber re-applies each subscription's filter to determine which subscription
/// a received Object belongs to."
#[test]
fn datagram_routed_by_objectid_filter() {
    let (mut client, _server, rid1, rid2) = establish_shared_alias_with_objectid_filters();
    const ALIAS: u64 = 2000;

    // Object ID = 5 → rid1 のフィルタ [0, 9] に該当
    let datagram_for_rid1 = ObjectDatagram {
        track_alias: ALIAS,
        group_id: 0,
        object_id: 5,
        publisher_priority: Some(128),
        properties_data: None,
        end_of_group: false,
        status: None,
    };
    let outcome = client
        .recv_object_datagram(&datagram_for_rid1)
        .expect("datagram 受信に失敗しないこと");
    assert_eq!(outcome, TrackDataAcceptance::Accepted);
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在する")
            .largest_received_location
            .as_ref()
            .map(|l| (l.group_id, l.object_id)),
        Some((0, 5)),
        "Object ID=5 は rid1 (フィルタ [0, 9]) に振り分けられること"
    );
    assert!(
        client
            .subscription(rid2)
            .expect("subscription が存在する")
            .largest_received_location
            .is_none(),
        "rid2 には振り分けられないこと"
    );

    // Object ID = 15 → rid2 のフィルタ [10, 19] に該当
    let datagram_for_rid2 = ObjectDatagram {
        track_alias: ALIAS,
        group_id: 0,
        object_id: 15,
        publisher_priority: Some(128),
        properties_data: None,
        end_of_group: false,
        status: None,
    };
    let outcome = client
        .recv_object_datagram(&datagram_for_rid2)
        .expect("datagram 受信に失敗しないこと");
    assert_eq!(outcome, TrackDataAcceptance::Accepted);
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在する")
            .largest_received_location
            .as_ref()
            .map(|l| (l.group_id, l.object_id)),
        Some((0, 15)),
        "Object ID=15 は rid2 (フィルタ [10, 19]) に振り分けられること"
    );
}

/// datagram 経路: どの候補のフィルタも通らない Object は FilteredOut になりセッションは閉じない
///
/// draft-ietf-moq-transport-21 §3.1 / §11.2: 受信側の破棄を要求していないため
/// セッションを閉じてはならない。
#[test]
fn datagram_no_matching_filter_returns_filtered_out() {
    let (mut client, _server, rid1, rid2) = establish_shared_alias_with_objectid_filters();
    const ALIAS: u64 = 2000;

    // Object ID = 50 → どちらのフィルタ ([0, 9] / [10, 19]) にも該当しない
    let datagram = ObjectDatagram {
        track_alias: ALIAS,
        group_id: 0,
        object_id: 50,
        publisher_priority: Some(128),
        properties_data: None,
        end_of_group: false,
        status: None,
    };
    let outcome = client
        .recv_object_datagram(&datagram)
        .expect("セッションは閉じないこと");
    assert_eq!(
        outcome,
        TrackDataAcceptance::FilteredOut,
        "どのフィルタも通らない Object は FilteredOut になること"
    );
    // どちらの subscription も更新されていないこと
    assert!(
        client
            .subscription(rid1)
            .expect("subscription が存在する")
            .largest_received_location
            .is_none(),
        "rid1 は更新されないこと"
    );
    assert!(
        client
            .subscription(rid2)
            .expect("subscription が存在する")
            .largest_received_location
            .is_none(),
        "rid2 は更新されないこと"
    );
}

/// subgroup stream 経路: SUBGROUP_FILTER で異なる subscription に振り分けられる
///
/// draft-ietf-moq-transport-21 §3.1 (Subscriptions):
/// "the subscriber re-applies each subscription's filter to determine which subscription
/// a received Object belongs to."
///
/// header 時点で評価できる SUBGROUP_FILTER で振り分けを行う。
#[test]
fn subgroup_stream_routed_by_subgroup_filter() {
    const ALIAS: u64 = 2001;
    let (mut client, mut server) = establish_pair_with_options(SetupOptions::new(), {
        let mut opts = SetupOptions::new();
        opts.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_FILTER_RANGES, // MAX_FILTER_RANGES
            value: SetupOptionValue::VarInt(8),
        });
        opts
    });

    // rid1: SUBGROUP_FILTER [0, 4]
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_SUBGROUP_FILTER, 0, 0, 4));
    let rid1 = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), params1)
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg1) = take_send_request(&mut client);
    server
        .recv_request(sub_msg1)
        .expect("SUBSCRIBE の受信に成功すること");

    // rid2: SUBGROUP_FILTER [5, 9]
    let mut params2 = MessageParameters::new();
    params2.push(range_filter(PARAM_SUBGROUP_FILTER, 0, 5, 9));
    let rid2 = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), params2)
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg2) = take_send_request(&mut client);
    server
        .recv_request(sub_msg2)
        .expect("SUBSCRIBE の受信に成功すること");

    // 両方に同一 alias を割り当てる
    server
        .send_subscribe_ok(
            rid1,
            ALIAS,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg1) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid1, ok_msg1)
        .expect("SUBSCRIBE_OK の受信に成功すること");

    server
        .send_subscribe_ok(
            rid2,
            ALIAS,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg2) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid2, ok_msg2)
        .expect("SUBSCRIBE_OK の受信に成功すること");

    // publisher (server) 側で subgroup stream を開いて client に送る
    // Subgroup ID = 7 → rid2 のフィルタ [5, 9] に該当
    let stream_id = DataStreamId(80);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(7),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(stream_id, rid2, &header)
        .expect("subgroup header の送信に成功すること");

    // client 側で stream を登録してから header を受信する
    client
        .recv_data_stream_type(DataStreamId(80), 0x14)
        .expect("data stream type の登録に失敗しないこと");
    let outcome = client
        .recv_subgroup_header(DataStreamId(80), &header)
        .expect("subgroup header の受信に失敗しないこと");
    assert_eq!(outcome, TrackDataAcceptance::Accepted);

    // rid2 の stream_counts が更新されていること
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在する")
            .stream_counts
            .incoming_subgroup_count,
        1,
        "Subgroup ID=7 は rid2 (フィルタ [5, 9]) に振り分けられること"
    );
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在する")
            .stream_counts
            .incoming_subgroup_count,
        0,
        "rid1 には振り分けられないこと"
    );
}

/// alias を共有しない単一 subscription では従来どおり Accepted になる (回帰)
#[test]
fn single_subscription_without_shared_alias_still_accepted() {
    const ALIAS: u64 = 2002;
    let (mut client, mut server) = establish_pair();
    let rid = send_and_recv_subscribe(&mut client, &mut server, &[b"live"], b"cam");
    complete_subscribe(&mut client, &mut server, rid, ALIAS);

    let datagram = ObjectDatagram {
        track_alias: ALIAS,
        group_id: 1,
        object_id: 0,
        publisher_priority: Some(128),
        properties_data: None,
        end_of_group: false,
        status: None,
    };
    let outcome = client
        .recv_object_datagram(&datagram)
        .expect("datagram 受信に失敗しないこと");
    assert_eq!(
        outcome,
        TrackDataAcceptance::Accepted,
        "単一 subscription では従来どおり Accepted になること"
    );
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .largest_received_location
            .as_ref()
            .map(|l| (l.group_id, l.object_id)),
        Some((1, 0)),
        "subscription の状態が更新されること"
    );
}
