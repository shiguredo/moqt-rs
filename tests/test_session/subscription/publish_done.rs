//! PUBLISH_DONE / STOP_SENDING / drain / overrun のテスト

use super::*;

/// PUBLISH_DONE 送信 → subscriber 側で Terminated 遷移と PublishDoneReceived イベント発行
#[test]
fn publish_done_full_cycle() {
    let (mut client, mut server) = establish_pair();
    // Server が publisher のペンディングを用意 (Client は subscriber)
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, m) = take_send_request(&mut client);
    server
        .recv_request(m)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 2, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    server
        .send_publish_done(
            rid,
            0x2,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, done_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated
    );
    let mut got_event = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::PublishDoneReceived { status_code, .. } = e {
            assert_eq!(status_code, 0x2);
            got_event = true;
        }
    }
    assert!(got_event);
}

#[test]
fn publish_done_drain_waits_for_timeout_and_open_stream_close() {
    let (mut client, mut server) = establish_pair();
    client.tick(1_000);

    let rid = client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            delivery_timeout_params(100),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            rid,
            9,
            MessageParameters::new(),
            track_properties_with_delivery_timeout(40),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .delivery_timeouts
            .effective_object_ms,
        Some(40)
    );

    let header = SubgroupHeader {
        track_alias: 9,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(DataStreamId(40), 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(40), &header)
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .stream_counts
            .incoming_subgroup_count,
        1
    );

    // publisher 側で 1 本の subgroup stream を開閉し published_stream_count を 1 にする
    // (draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE): announce する stream_count は publisher が実際に開いた本数と一致する必要がある)
    let publisher_stream = DataStreamId(100);
    server
        .send_subgroup_header(publisher_stream, rid, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_data_stream_closed(publisher_stream, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .stream_counts
            .published_count,
        1
    );

    server
        .send_publish_done(
            rid,
            0x2,
            1,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, done_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let subscription = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(subscription.state, SubscriptionState::Terminated);
    let publish_done = subscription
        .publish_done
        .as_ref()
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(publish_done.drain.duration_ms, 40);
    assert_eq!(publish_done.drain.deadline_ms, Some(1_040));
    assert!(!publish_done.drain.expired);
    assert_eq!(client.subscription_cleanup_ready(rid), Some(false));
    assert!(client.forget_subscription(rid).is_none());

    client
        .recv_data_stream_closed(DataStreamId(40), RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    // 終端後は索引から除去される (二重終端は unknown stream id で fail する)
    let err = client
        .recv_data_stream_closed(DataStreamId(40), RequestStreamEnd::Fin)
        .expect_err("終端済み stream の再終端は fail すること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.subscription_cleanup_ready(rid), Some(false));

    client.tick(1_039);
    assert_eq!(client.subscription_cleanup_ready(rid), Some(false));
    client.tick(1_040);
    assert_eq!(client.subscription_cleanup_ready(rid), Some(true));
    assert!(client.forget_subscription(rid).is_some());
}

/// PUBLISH_DONE 受信時点で stream_count を超過していれば、drain timer を待たずに
/// open stream が全て閉じた瞬間に cleanup_ready になる
///
/// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE):
/// "A subscriber MAY discard subscription state earlier, at the cost of potentially not
/// delivering some late objects to the application." (早期破棄の MAY) /
/// "If a subscriber receives more streams for a subscription than specified in Stream Count,
/// it MAY close the session with a PROTOCOL_VIOLATION." (overrun 時の session close MAY)
#[test]
fn publish_done_overrun_allows_early_cleanup_after_open_streams_close() {
    let (mut client, mut server) = establish_pair();
    client.tick(1_000);

    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 9, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // publisher が 1 本の subgroup stream を開く
    let header = SubgroupHeader {
        track_alias: 9,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(DataStreamId(40), 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(40), &header)
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .stream_counts
            .incoming_subgroup_count,
        1
    );

    // PUBLISH_DONE で stream_count=0 を宣言する → 既存の 1 本で overrun が確定
    server
        .send_publish_done(
            rid,
            0x2,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, done_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    assert!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .publish_done
            .as_ref()
            .is_some_and(|p| p.stream_count_overrun)
    );
    assert_eq!(client.subscription_cleanup_ready(rid), Some(false));
    assert!(client.forget_subscription(rid).is_none());

    // open stream が閉じれば drain timer を待たずに回収可能
    client
        .recv_data_stream_closed(DataStreamId(40), RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    // 終端後は索引から除去される (二重終端は unknown stream id で fail する)
    let err = client
        .recv_data_stream_closed(DataStreamId(40), RequestStreamEnd::Fin)
        .expect_err("終端済み stream の再終端は fail すること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.subscription_cleanup_ready(rid), Some(true));
    assert!(client.forget_subscription(rid).is_some());
}

/// stream_count_overrun 確定後に open stream が閉じると GOAWAY drain blocker から外れる
#[test]
fn publish_done_overrun_removes_goaway_drain_blocker() {
    let (mut client, mut server) = establish_pair();
    client.tick(1_000);

    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 9, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // client が GOAWAY を送信して draining を開始
    client
        .send_goaway(Vec::new(), 10_000)
        .expect("テストフィクスチャの前提条件を満たす");
    let snapshot = client.goaway_drain_snapshot();
    assert!(
        snapshot.blocking_subscription_request_ids.contains(&rid),
        "確立済み subscription が GOAWAY drain blocker になること"
    );
    assert!(!snapshot.ready());

    // publisher が 1 本の subgroup stream を開いてから PUBLISH_DONE stream_count=0 を送る
    let header = SubgroupHeader {
        track_alias: 9,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(DataStreamId(40), 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(40), &header)
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );
    server
        .send_publish_done(
            rid,
            0x2,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, done_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // open stream が残っている間はまだ blocker
    let snapshot = client.goaway_drain_snapshot();
    assert!(
        snapshot.blocking_subscription_request_ids.contains(&rid),
        "open stream 残存時は GOAWAY drain blocker が外れないこと"
    );
    assert!(!snapshot.ready());

    // open stream が閉じれば overrun により即座に blocker から外れる
    client
        .recv_data_stream_closed(DataStreamId(40), RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    let snapshot = client.goaway_drain_snapshot();
    assert!(
        !snapshot.blocking_subscription_request_ids.contains(&rid),
        "overrun 確定後に open stream が閉じると GOAWAY drain blocker から外れること"
    );
    assert!(snapshot.ready());
}

#[test]
fn effective_delivery_timeout_uses_min_non_zero_values() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            delivery_timeout_params(300),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .delivery_timeouts
            .effective_object_ms,
        Some(300)
    );
    server
        .send_subscribe_ok(
            rid,
            12,
            MessageParameters::new(),
            track_properties_with_delivery_timeout(100),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .delivery_timeouts
            .publisher_object_ms,
        Some(100)
    );
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .delivery_timeouts
            .effective_object_ms,
        Some(100)
    );
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let subscription = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        subscription.delivery_timeouts.subscriber_object_ms,
        Some(300)
    );
    assert_eq!(
        subscription.delivery_timeouts.publisher_object_ms,
        Some(100)
    );
    assert_eq!(
        subscription.delivery_timeouts.effective_object_ms,
        Some(100)
    );
}

#[test]
fn effective_delivery_timeout_uses_non_zero_value_when_other_side_is_zero() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), delivery_timeout_params(0))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            rid,
            13,
            MessageParameters::new(),
            track_properties_with_delivery_timeout(250),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let subscription = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(subscription.delivery_timeouts.subscriber_object_ms, Some(0));
    assert_eq!(
        subscription.delivery_timeouts.publisher_object_ms,
        Some(250)
    );
    assert_eq!(
        subscription.delivery_timeouts.effective_object_ms,
        Some(250)
    );
}

#[test]
fn rendezvous_timeout_is_preserved_for_both_subscribe_sides() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            rendezvous_timeout_params(2500),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .subscriber_rendezvous_timeout_ms,
        Some(2500)
    );

    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .subscriber_rendezvous_timeout_ms,
        Some(2500)
    );
}

#[test]
fn rendezvous_timeout_zero_is_not_normalized_away() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            rendezvous_timeout_params(0),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .subscriber_rendezvous_timeout_ms,
        Some(0)
    );
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .subscriber_rendezvous_timeout_ms,
        Some(0)
    );
}

#[test]
fn publish_done_tracks_stream_count_overrun() {
    let (mut client, mut server, rid) = establish_subscribe_track(10);

    // publisher 側で 1 本の subgroup stream を開閉し published_stream_count を 1 にする
    // (draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE): announce する stream_count は publisher が実際に開いた本数と一致する必要がある)
    let publisher_stream = DataStreamId(100);
    let publisher_header = SubgroupHeader {
        track_alias: 10,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(publisher_stream, rid, &publisher_header)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_data_stream_closed(publisher_stream, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .stream_counts
            .published_count,
        1
    );

    server
        .send_publish_done(
            rid,
            0x2,
            1,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, done_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        !client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .publish_done
            .as_ref()
            .is_some_and(|p| p.stream_count_overrun)
    );

    let first_header = SubgroupHeader {
        track_alias: 10,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(DataStreamId(41), 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(41), &first_header)
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );
    assert!(
        !client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .publish_done
            .as_ref()
            .is_some_and(|p| p.stream_count_overrun)
    );

    let second_header = SubgroupHeader {
        track_alias: 10,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(1),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(DataStreamId(42), 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(42), &second_header)
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );
    assert!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .publish_done
            .as_ref()
            .is_some_and(|p| p.stream_count_overrun)
    );
}

/// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE):
/// subscriber が sentinel を受信した場合、後続で subgroup stream を多数受信しても
/// `publish_done.stream_count_overrun` は `false` を維持する
/// (sentinel = publisher が exact 数を表明していないため比較対象が存在しない)。
#[test]
fn peer_publish_done_with_sentinel_does_not_set_overrun() {
    let (mut client, mut server, rid) = establish_subscribe_track(804);

    server
        .send_publish_done(
            rid,
            0x2,
            PUBLISH_DONE_STREAM_COUNT_UNKNOWN,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, done_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        !client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .publish_done
            .as_ref()
            .is_some_and(|p| p.stream_count_overrun)
    );

    // sentinel 受信後に subgroup stream を 2 本受信しても overrun フラグは立たない
    for (sg_id, stream_id_value) in [(0u64, 70u64), (1u64, 71u64)] {
        let header = SubgroupHeader {
            track_alias: 804,
            group_id: 1,
            subgroup_id: SubgroupIdMode::Explicit(sg_id),
            publisher_priority: Some(1),
            has_properties: false,
            end_of_group: false,
            first_object: false,
        };
        client
            .recv_data_stream_type(DataStreamId(stream_id_value), 0x14)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            client
                .recv_subgroup_header(DataStreamId(stream_id_value), &header)
                .expect("テストフィクスチャの前提条件を満たす"),
            TrackDataAcceptance::Accepted
        );
    }
    assert!(
        !client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .publish_done
            .as_ref()
            .is_some_and(|p| p.stream_count_overrun)
    );
}

/// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE):
/// exact 数不明時の sentinel は `2^64 - 1` (`u64::MAX`) である。
#[test]
fn publish_done_stream_count_unknown_is_u64_max() {
    assert_eq!(
        PUBLISH_DONE_STREAM_COUNT_UNKNOWN,
        u64::MAX,
        "sentinel は draft-ietf-moq-transport-21 §9.9 の 2^64 - 1 であること"
    );
}

/// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE):
/// `stream_count = u64::MAX` の PUBLISH_DONE を送受信でき、subscriber 側で
/// stream 数が `u64::MAX` として届き overrun は立たない。
#[test]
fn peer_publish_done_with_u64_max_does_not_set_overrun() {
    let (mut client, mut server, rid) = establish_subscribe_track(805);

    server
        .send_publish_done(
            rid,
            0x2,
            u64::MAX,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("u64::MAX の stream_count は常に受理されること");
    let (_, done_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let subscription = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        subscription
            .publish_done
            .as_ref()
            .expect("テストフィクスチャの前提条件を満たす")
            .stream_count,
        u64::MAX,
        "u64::MAX が Session 間で round-trip すること"
    );
    assert!(
        !subscription
            .publish_done
            .as_ref()
            .is_some_and(|p| p.stream_count_overrun),
        "sentinel 受信時は overrun を立てないこと"
    );

    // sentinel 受信後に subgroup stream を受信しても overrun フラグは立たない
    let header = SubgroupHeader {
        track_alias: 805,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(DataStreamId(72), 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(72), &header)
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );
    assert!(
        !client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .publish_done
            .as_ref()
            .is_some_and(|p| p.stream_count_overrun),
        "後発 stream 受信後も sentinel の overrun は立たないこと"
    );
}

/// subscriber 側の STOP_SENDING 通知で subscription が Terminated になる
/// (I/O 層が QUIC STOP_SENDING を発行したことを session に通知)
#[test]
fn stop_sending_terminates_subscription() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, m) = take_send_request(&mut client);
    server
        .recv_request(m)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 3, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    client
        .stop_sending(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated
    );
    // draft-ietf-moq-transport-21 §3.1.1: REQUEST_ERROR → 即時破棄可能
    assert_eq!(client.subscription_cleanup_ready(rid), Some(true));
}

/// draft-ietf-moq-transport-21 §3.1 (Subscriptions): STOP_SENDING は Pending (Subscriber) or Established から。
/// Pending(Publisher) (自側 my_role=Subscriber で peer initiated via PUBLISH) からは不可。
#[test]
fn stop_sending_in_pending_publisher_errors() {
    let (mut client, mut server) = establish_pair();
    // server が PUBLISH 送信、client は Pending(Publisher) な subscriber responder
    let rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            1,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut server);
    client
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_pending_publisher()
    );
    let err = client.stop_sending(rid).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // 状態は遷移していない (Terminated になっていない)
    assert!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_pending_publisher()
    );
}

/// draft-ietf-moq-transport-21 §3.1 (Subscriptions): PUBLISH_DONE は Pending (Publisher) or Established から。
/// Pending(Subscriber) (peer が SUBSCRIBE の initiator で自側 my_role=Publisher responder)
/// からは送信不可。
#[test]
fn send_publish_done_in_pending_subscriber_errors() {
    let (mut client, mut server) = establish_pair();
    // client が SUBSCRIBE 送信、server は Pending(Subscriber) な publisher responder
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_pending_subscriber()
    );
    let err = server
        .send_publish_done(
            rid,
            0,
            0,
            shiguredo_moqt::message::ReasonPhrase::new(String::new())
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_pending_subscriber()
    );
}

/// 受信側: Pending(Subscriber) 状態 (自側 my_role=Subscriber, 自側が initiator) で
/// peer から PUBLISH_DONE を受信することは state machine 上ありえない
/// (publisher は Pending(Publisher)/Established からしか送れない)。受信時は
/// PROTOCOL_VIOLATION でセッションを閉じる。
#[test]
fn peer_publish_done_in_pending_subscriber_closes_session() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // client 側は Pending(Subscriber) のまま SUBSCRIBE_OK を受信していない
    assert!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_pending_subscriber()
    );
    let done = ControlMessage::PublishDone(shiguredo_moqt::message::PublishDone {
        status_code: 0,
        stream_count: 0,
        reason: shiguredo_moqt::message::ReasonPhrase::new(String::new())
            .expect("テストフィクスチャの前提条件を満たす"),
    });
    let err = client.recv_stream_message(rid, done).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}

/// forget_subscription で Track Alias が再利用できる
#[test]
fn forget_subscription_allows_track_alias_reuse() {
    let (_, mut server) = establish_pair();
    server
        .recv_request(ControlMessage::Subscribe(
            shiguredo_moqt::message::Subscribe {
                request_id: 0,

                track_namespace: ns(&[b"live"]),
                track_name: b"cam".to_vec(),
                parameters: MessageParameters::new(),
            },
        ))
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(0, 42, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_send_on_stream(&mut server);
    server
        .send_publish_done(
            0,
            0,
            0,
            shiguredo_moqt::message::ReasonPhrase::new(String::new())
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_send_on_stream(&mut server);
    server
        .forget_subscription(0)
        .expect("テストフィクスチャの前提条件を満たす");

    // 同じ alias=42 を別 Track で再利用
    let rid2 = server
        .send_publish(
            ns(&[b"other"]),
            b"cam2".to_vec(),
            42,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid2)
            .expect("テストフィクスチャの前提条件を満たす")
            .track_alias,
        Some(42)
    );
}
