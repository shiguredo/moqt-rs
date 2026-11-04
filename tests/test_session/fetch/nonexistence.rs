//! 不存在区間と Object の矛盾 (Malformed Track) のテスト
//!
//! draft-ietf-moq-transport-23 §2.1.2 (Object States) は不存在の通知を恒久的かつ権威的と
//! 定め、§12.1 (Malformed Tracks) 条件 10 は「不存在と記録した後の Object」と「Object を
//! 受信した後の不存在通知」の両方を Malformed Track とする。§3.2.2 (Gaps in a Fetch Stream)
//! は Range Filters を持たない FETCH 応答の暗黙 gap を不存在、Range Filters を持つ場合を
//! Unknown と定める。§11.4.2 (End of Range) の Gone / Unknown / Timed-Out は存在を否定しない。
//!
//! Session はこれらの判定を Full Track Name 単位で共有し、subscription / FETCH / fill を
//! またいで照合する。

use super::*;

use shiguredo_moqt::message_parameter::{
    LocationFilter, MessageParameter, MessageParameterValue, PARAM_FILL_PARAMETERS,
    PARAM_OBJECTID_FILTER,
};
use shiguredo_moqt::stream::FETCH_HEADER_TYPE;

/// テストで使う Track の名前空間
fn track_namespace() -> TrackNamespace {
    ns(&[b"live"])
}

/// テストで使う Track 名
fn track_name() -> Vec<u8> {
    b"cam1".to_vec()
}

/// 同一 Track の subscription を確立し、Largest Object を広告する
///
/// 返り値は subscription の Request ID。
fn establish_subscription(
    client: &mut Session,
    server: &mut Session,
    params: MessageParameters,
    alias: u64,
    largest: Location,
) -> u64 {
    let rid = client
        .send_subscribe(track_namespace(), track_name(), params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut ok_params = MessageParameters::new();
    ok_params.set_largest_object(largest.group_id, largest.object_id);
    server
        .send_subscribe_ok(rid, alias, ok_params, TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // publisher 側にも Largest Object を観測させる。FETCH の受理判定は未公開 Track への
    // 要求を INVALID_RANGE で拒否するため (draft-ietf-moq-transport-23 §3.2 (Fetch))。
    let publish_stream = DataStreamId(10_000 + alias);
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: largest.group_id,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(0),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(publish_stream, rid, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(publish_stream, largest.object_id, None)
        .expect("テストフィクスチャの前提条件を満たす");
    rid
}

/// 同一 Track の FETCH を確立し、応答範囲の終端を Largest Object で確定させる
fn establish_fetch(
    client: &mut Session,
    server: &mut Session,
    params: MessageParameters,
    largest: Location,
) -> u64 {
    let rid = client
        .send_fetch(track_namespace(), track_name(), params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_fetch_ok(rid, fetch_ok_params(Some(largest)), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    rid
}

/// 受信側の FETCH 応答 stream を開く
fn open_fetch_stream(client: &mut Session, request_id: u64, stream_id: DataStreamId) {
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_fetch_header(stream_id, &FetchHeader { request_id })
        .expect("テストフィクスチャの前提条件を満たす");
}

/// 受信側の FETCH 応答 stream を FIN で終端する
fn finish_fetch_stream(client: &mut Session, stream_id: DataStreamId) {
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
}

/// 受信側の subgroup stream を開く
fn open_subgroup_stream(
    client: &mut Session,
    alias: u64,
    group_id: u64,
    subgroup_id: u64,
    stream_id: DataStreamId,
) {
    let header = SubgroupHeader {
        track_alias: alias,
        group_id,
        subgroup_id: SubgroupIdMode::Explicit(subgroup_id),
        publisher_priority: Some(0),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_id, header.encode()[0] as u64)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &header)
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted,
        "テストフィクスチャの subgroup header は受理されること"
    );
}

/// 受信側に Object を 1 つ届ける
fn recv_subgroup_object(client: &mut Session, stream_id: DataStreamId, object_id: u64) {
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &DecodedSubgroupObject {
                    object_id,
                    payload_length: 1,
                    status: None,
                    properties_bytes: None,
                },
            )
            .expect("テストフィクスチャの Object は受理されること"),
        TrackDataAcceptance::Accepted
    );
}

/// 明示的な End of Non-Existent Range の後の Object を拒否する
///
/// draft-ietf-moq-transport-23 §11.4.2 (End of Range) は指示までの Location を不存在と
/// 定める。§12.1 (Malformed Tracks) 条件 10 により、不存在と記録した後の Object 受信は
/// Malformed Track であり、同じ Track の subscription / FETCH を cancel する。
#[test]
fn explicit_missing_range_then_object_cancels_track_requests() {
    let (mut client, mut server) = establish_pair();
    let alias = 700;
    let sub_rid = establish_subscription(
        &mut client,
        &mut server,
        MessageParameters::new(),
        alias,
        Location {
            group_id: 2,
            object_id: 0,
        },
    );
    let fetch_rid = establish_fetch(
        &mut client,
        &mut server,
        MessageParameters::new(),
        Location {
            group_id: 2,
            object_id: 0,
        },
    );
    let fetch_stream = DataStreamId(200);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    client
        .recv_fetch_entry(fetch_stream, &fetch_end_of_non_existent_range_entry(2, 0))
        .expect("先頭の End of Non-Existent Range は受理されること");
    finish_fetch_stream(&mut client, fetch_stream);

    // 不存在と記録した (2, 0) を subscription が届ける
    let subgroup_stream = DataStreamId(201);
    open_subgroup_stream(&mut client, alias, 2, 0, subgroup_stream);
    let err = client
        .recv_subgroup_object(
            subgroup_stream,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("不存在区間内の Object は Malformed Track になる");
    assert_eq!(
        err.reason,
        "malformed track: object received in a range recorded as non-existent"
    );
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
    assert_eq!(
        client.fetch(fetch_rid).expect("fetch が存在する").state,
        FetchState::Terminated
    );
    // FETCH は data stream の FIN で既に Terminated のため cancel 通知は発行されない。
    // 生きている subscription へは MalformedTrack が通知される。
    assert_eq!(
        drain_malformed_reasons(&mut client),
        vec!["malformed track: object received in a range recorded as non-existent"],
        "生きている subscription に MalformedTrack を通知すること"
    );
}

/// 既出 Object を覆う End of Non-Existent Range を拒否する (逆順の矛盾)
///
/// draft-ietf-moq-transport-23 §12.1 (Malformed Tracks) 条件 10 は
/// "is signalled as not existing after it was received" も Malformed Track とする。
#[test]
fn object_then_explicit_missing_range_cancels_track_requests() {
    let (mut client, mut server) = establish_pair();
    let alias = 701;
    let sub_rid = establish_subscription(
        &mut client,
        &mut server,
        MessageParameters::new(),
        alias,
        Location {
            group_id: 0,
            object_id: 1,
        },
    );
    let fetch_rid = establish_fetch(
        &mut client,
        &mut server,
        MessageParameters::new(),
        Location {
            group_id: 0,
            object_id: 1,
        },
    );
    let subgroup_stream = DataStreamId(202);
    open_subgroup_stream(&mut client, alias, 0, 0, subgroup_stream);
    recv_subgroup_object(&mut client, subgroup_stream, 1);

    let fetch_stream = DataStreamId(203);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    let err = client
        .recv_fetch_entry(fetch_stream, &fetch_end_of_non_existent_range_entry(0, 1))
        .expect_err("既出 Object を覆う不存在通知は Malformed Track になる");
    assert_eq!(
        err.reason,
        "malformed track: non-existent range covers a received object"
    );
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
}

/// Range Filters を持たない FETCH 応答の暗黙 gap を不存在として記録する
///
/// draft-ietf-moq-transport-23 §3.2.2 (Gaps in a Fetch Stream): "Unless the request included
/// Range Filters (see Section 3.3.2), any gaps in the Group and Object IDs in the response
/// stream that are not otherwise marked indicate objects that do not exist."
#[test]
fn implicit_gap_between_objects_is_recorded_as_missing() {
    let (mut client, mut server) = establish_pair();
    let alias = 702;
    let sub_rid = establish_subscription(
        &mut client,
        &mut server,
        MessageParameters::new(),
        alias,
        Location {
            group_id: 0,
            object_id: 2,
        },
    );
    let fetch_rid = establish_fetch(
        &mut client,
        &mut server,
        MessageParameters::new(),
        Location {
            group_id: 0,
            object_id: 2,
        },
    );
    let fetch_stream = DataStreamId(204);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(0, 0))
        .expect("先頭 Object は受理されること");
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(0, 2))
        .expect("暗黙 gap を持つ Object は受理されること");
    finish_fetch_stream(&mut client, fetch_stream);

    let subgroup_stream = DataStreamId(205);
    open_subgroup_stream(&mut client, alias, 0, 1, subgroup_stream);
    let err = client
        .recv_subgroup_object(
            subgroup_stream,
            &DecodedSubgroupObject {
                object_id: 1,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("暗黙 gap 内の Object は Malformed Track になる");
    assert_eq!(
        err.reason,
        "malformed track: object received in a range recorded as non-existent"
    );
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
}

/// 既出 Object を覆う暗黙 gap を拒否する (逆順の矛盾)
#[test]
fn implicit_gap_covering_received_object_is_rejected() {
    let (mut client, mut server) = establish_pair();
    let alias = 703;
    let sub_rid = establish_subscription(
        &mut client,
        &mut server,
        MessageParameters::new(),
        alias,
        Location {
            group_id: 0,
            object_id: 2,
        },
    );
    let fetch_rid = establish_fetch(
        &mut client,
        &mut server,
        MessageParameters::new(),
        Location {
            group_id: 0,
            object_id: 2,
        },
    );
    let subgroup_stream = DataStreamId(206);
    open_subgroup_stream(&mut client, alias, 0, 0, subgroup_stream);
    recv_subgroup_object(&mut client, subgroup_stream, 1);

    let fetch_stream = DataStreamId(207);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(0, 0))
        .expect("先頭 Object は受理されること");
    let err = client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(0, 2))
        .expect_err("既出 Object を覆う暗黙 gap は Malformed Track になる");
    assert_eq!(
        err.reason,
        "malformed track: non-existent range covers a received object"
    );
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
}

/// Range Filters を持つ FETCH 応答の gap は Unknown であり不存在にしない
///
/// draft-ietf-moq-transport-23 §3.2.2: "When one or more Range Filters are present, unmarked
/// gaps indicate objects in the Unknown state." したがって後から届く Object を誤拒否しない。
#[test]
fn range_filter_gap_does_not_reject_later_object() {
    let (mut client, mut server) =
        establish_pair_with_options(SetupOptions::new(), max_filter_options());
    let alias = 704;
    let sub_rid = establish_subscription(
        &mut client,
        &mut server,
        MessageParameters::new(),
        alias,
        Location {
            group_id: 0,
            object_id: 2,
        },
    );
    let mut params = MessageParameters::new();
    params.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 10));
    let fetch_rid = establish_fetch(
        &mut client,
        &mut server,
        params,
        Location {
            group_id: 0,
            object_id: 2,
        },
    );
    let fetch_stream = DataStreamId(208);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(0, 0))
        .expect("先頭 Object は受理されること");
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(0, 2))
        .expect("Range Filters 下の gap は Unknown として受理されること");
    finish_fetch_stream(&mut client, fetch_stream);

    // Range Filters が作った gap (0, 1) の Object は誤拒否されない
    let subgroup_stream = DataStreamId(209);
    open_subgroup_stream(&mut client, alias, 0, 0, subgroup_stream);
    recv_subgroup_object(&mut client, subgroup_stream, 1);
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Established,
        "Unknown の gap は不存在ではないため購読を終端しないこと"
    );
}

/// Gone / Unknown / Timed-Out の範囲の後の Object は誤拒否しない
///
/// draft-ietf-moq-transport-23 §2.1.2 (Object States): "Because another source can still hold
/// a Gone Object, receiving an Object that was recorded as Gone or Unknown is not an error."
#[test]
fn end_of_range_other_than_non_existent_does_not_reject_object() {
    let cases = [
        (
            "Unknown",
            DecodedFetchEntry::EndOfUnknownRange {
                group_id: 0,
                object_id: 2,
            },
        ),
        (
            "Timed-Out",
            DecodedFetchEntry::EndOfTimedOutRange {
                group_id: 0,
                object_id: 2,
            },
        ),
        (
            "Gone",
            DecodedFetchEntry::EndOfGoneRange {
                group_id: 0,
                object_id: 2,
            },
        ),
    ];
    for (label, entry) in cases {
        let (mut client, mut server) = establish_pair();
        let alias = 705;
        let sub_rid = establish_subscription(
            &mut client,
            &mut server,
            MessageParameters::new(),
            alias,
            Location {
                group_id: 0,
                object_id: 2,
            },
        );
        let fetch_rid = establish_fetch(
            &mut client,
            &mut server,
            MessageParameters::new(),
            Location {
                group_id: 0,
                object_id: 2,
            },
        );
        let fetch_stream = DataStreamId(210);
        open_fetch_stream(&mut client, fetch_rid, fetch_stream);
        client
            .recv_fetch_entry(fetch_stream, &entry)
            .unwrap_or_else(|e| panic!("{label}: End of Range は受理されること: {e:?}"));
        finish_fetch_stream(&mut client, fetch_stream);

        let subgroup_stream = DataStreamId(211);
        open_subgroup_stream(&mut client, alias, 0, 0, subgroup_stream);
        recv_subgroup_object(&mut client, subgroup_stream, 1);
        assert_eq!(
            client
                .subscription(sub_rid)
                .expect("subscription が存在する")
                .state,
            SubscriptionState::Established,
            "{label}: 存在を否定しない範囲の後の Object で購読を終端しないこと"
        );
    }
}

/// Prior Object ID Gap の検証が別 request をまたいで共有される
///
/// draft-ietf-moq-transport-23 §2.1.2 (Object States) は Prior Object ID Gap も不存在の通知と
/// 定める。Track 単位の履歴を共有するため、ある FETCH で通知された Gap 内の Object を
/// 別 request が受信した場合も Malformed Track になる。
#[test]
fn prior_gap_is_shared_across_requests() {
    let (mut client, mut server) = establish_pair();
    let alias = 706;
    let sub_rid = establish_subscription(
        &mut client,
        &mut server,
        MessageParameters::new(),
        alias,
        Location {
            group_id: 0,
            object_id: 5,
        },
    );
    let fetch_rid = establish_fetch(
        &mut client,
        &mut server,
        fetch_range_params(
            Location {
                group_id: 0,
                object_id: 5,
            },
            Location {
                group_id: 0,
                object_id: 5,
            },
        ),
        Location {
            group_id: 0,
            object_id: 5,
        },
    );
    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_OBJECT_ID_GAP,
        value: ObjectPropertyValue::VarInt(2),
    });
    let mut properties_bytes = Vec::new();
    props
        .encode(&mut properties_bytes)
        .expect("テストフィクスチャの Properties はエンコードできること");
    let fetch_stream = DataStreamId(212);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    client
        .recv_fetch_entry(
            fetch_stream,
            &DecodedFetchEntry::Object(DecodedFetchObject {
                group_id: 0,
                subgroup_id: 0,
                object_id: 5,
                publisher_priority: 128,
                is_datagram_origin: false,
                payload_length: 0,
                properties_bytes: Some(properties_bytes),
            }),
        )
        .expect("Prior Object ID Gap を持つ Object は受理されること");
    finish_fetch_stream(&mut client, fetch_stream);

    // 別 request (subscription) が Gap 内の Object を受信する
    let subgroup_stream = DataStreamId(213);
    open_subgroup_stream(&mut client, alias, 0, 1, subgroup_stream);
    let err = client
        .recv_subgroup_object(
            subgroup_stream,
            &DecodedSubgroupObject {
                object_id: 4,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("Prior Gap 内の Object は Malformed Track になる");
    assert_eq!(
        err.reason,
        "malformed track: object ID falls within a previously communicated gap"
    );
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
    assert_eq!(
        client.fetch(fetch_rid).expect("fetch が存在する").state,
        FetchState::Terminated
    );
}

/// 過去 Group の既出 Object が新しい FETCH の不存在通知と照合される
#[test]
fn past_group_object_is_shared_with_new_fetch() {
    let (mut client, mut server) = establish_pair();
    let alias = 707;
    let sub_rid = establish_subscription(
        &mut client,
        &mut server,
        MessageParameters::new(),
        alias,
        Location {
            group_id: 5,
            object_id: 0,
        },
    );
    let subgroup_stream = DataStreamId(214);
    open_subgroup_stream(&mut client, alias, 5, 0, subgroup_stream);
    recv_subgroup_object(&mut client, subgroup_stream, 0);

    let fetch_rid = establish_fetch(
        &mut client,
        &mut server,
        fetch_range_params(
            Location {
                group_id: 5,
                object_id: 0,
            },
            Location {
                group_id: 5,
                object_id: 1,
            },
        ),
        Location {
            group_id: 5,
            object_id: 1,
        },
    );
    let fetch_stream = DataStreamId(215);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    let err = client
        .recv_fetch_entry(fetch_stream, &fetch_end_of_non_existent_range_entry(5, 1))
        .expect_err("過去 Group の既出 Object を覆う不存在通知は Malformed Track になる");
    assert_eq!(
        err.reason,
        "malformed track: non-existent range covers a received object"
    );
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
}

/// 最後の request を forget するまで Track 履歴を保持し、その後は破棄する
///
/// 1 件の request の破棄では、他の request が参照している共有情報を失わない。
#[test]
fn track_history_survives_one_forget_and_is_released_after_last() {
    let (mut client, mut server) = establish_pair();
    let largest = Location {
        group_id: 0,
        object_id: 2,
    };
    // 1 本目の FETCH が要求範囲全体を不存在として通知する
    let fetch_a = establish_fetch(&mut client, &mut server, MessageParameters::new(), largest);
    let stream_a = DataStreamId(216);
    open_fetch_stream(&mut client, fetch_a, stream_a);
    client
        .recv_fetch_entry(stream_a, &fetch_end_of_non_existent_range_entry(0, 2))
        .expect("End of Non-Existent Range は受理されること");
    finish_fetch_stream(&mut client, stream_a);
    // 2 本目の FETCH を先に確立してから 1 本目を破棄する。他の参照が残っている間は
    // 履歴を保持することを確認するためである。
    let fetch_b = establish_fetch(&mut client, &mut server, MessageParameters::new(), largest);
    client
        .forget_fetch(fetch_a)
        .expect("終端済み FETCH は破棄できること");
    let stream_b = DataStreamId(217);
    open_fetch_stream(&mut client, fetch_b, stream_b);
    let err = client
        .recv_fetch_entry(stream_b, &fetch_object_entry(0, 1))
        .expect_err("他の request が共有する不存在区間との矛盾を検出すること");
    assert_eq!(
        err.reason,
        "malformed track: object received in a range recorded as non-existent"
    );
    client
        .forget_fetch(fetch_b)
        .expect("malformed 終端済み FETCH も破棄できること");

    // 最後の参照が外れたため、新しい FETCH は履歴を引き継がない
    let fetch_c = establish_fetch(&mut client, &mut server, MessageParameters::new(), largest);
    let stream_c = DataStreamId(218);
    open_fetch_stream(&mut client, fetch_c, stream_c);
    client
        .recv_fetch_entry(stream_c, &fetch_object_entry(0, 1))
        .expect("最後の request の破棄後に履歴が解放されること");
    assert_eq!(client.state(), SessionState::Established);
}

/// 先頭と末尾の暗黙 gap を不存在として記録する
///
/// draft-ietf-moq-transport-23 §3.2.2 (Gaps in a Fetch Stream): 昇順では Start から最初の
/// Object まで、最後の Object から End までが gap になる。
#[test]
fn implicit_leading_and_trailing_gaps_are_recorded() {
    let (mut client, mut server) = establish_pair();
    let alias = 708;
    let start = Location {
        group_id: 1,
        object_id: 0,
    };
    let end = Location {
        group_id: 1,
        object_id: 5,
    };
    let sub_rid = establish_subscription(
        &mut client,
        &mut server,
        MessageParameters::new(),
        alias,
        end,
    );
    let fetch_rid = establish_fetch(
        &mut client,
        &mut server,
        fetch_range_params(start, end),
        end,
    );
    let fetch_stream = DataStreamId(219);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(1, 2))
        .expect("先頭 Object は受理されること");
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(1, 3))
        .expect("2 つ目の Object は受理されること");
    finish_fetch_stream(&mut client, fetch_stream);

    // 先頭 gap (1, 0) の Object
    let subgroup_stream = DataStreamId(220);
    open_subgroup_stream(&mut client, alias, 1, 0, subgroup_stream);
    let err = client
        .recv_subgroup_object(
            subgroup_stream,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("先頭 gap 内の Object は Malformed Track になる");
    assert_eq!(
        err.reason,
        "malformed track: object received in a range recorded as non-existent"
    );
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
}

/// 末尾 gap は FIN でだけ確定する
///
/// draft-ietf-moq-transport-23 §3.2.2: "Gaps at the end of a stream are only detectable when
/// the stream is terminated by a FIN."
#[test]
fn implicit_trailing_gap_is_recorded_at_fin() {
    let (mut client, mut server) = establish_pair();
    let alias = 709;
    let start = Location {
        group_id: 1,
        object_id: 0,
    };
    let end = Location {
        group_id: 1,
        object_id: 5,
    };
    let sub_rid = establish_subscription(
        &mut client,
        &mut server,
        MessageParameters::new(),
        alias,
        end,
    );
    let fetch_rid = establish_fetch(
        &mut client,
        &mut server,
        fetch_range_params(start, end),
        end,
    );
    let fetch_stream = DataStreamId(221);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(1, 0))
        .expect("Object は受理されること");
    finish_fetch_stream(&mut client, fetch_stream);

    let subgroup_stream = DataStreamId(222);
    open_subgroup_stream(&mut client, alias, 1, 1, subgroup_stream);
    let err = client
        .recv_subgroup_object(
            subgroup_stream,
            &DecodedSubgroupObject {
                object_id: 5,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("FIN で確定した末尾 gap 内の Object は Malformed Track になる");
    assert_eq!(
        err.reason,
        "malformed track: object received in a range recorded as non-existent"
    );
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
}

/// 応答より先に FIN が届いた場合、末尾 gap の判定を FETCH_OK まで保留する
///
/// draft-ietf-moq-transport-23 §3.2 (Fetch) は FETCH_OK を Object 配信より後に送ることを
/// 許す。応答範囲の終端が未確定の間に末尾 gap を確定すると、LARGEST_OBJECT による切り詰めを
/// 誤って不存在として記録するため、判定を保留する。
#[test]
fn trailing_gap_is_deferred_until_fetch_ok_arrives() {
    let (mut client, mut server) = establish_pair();
    let alias = 710;
    let start = Location {
        group_id: 0,
        object_id: 0,
    };
    let end = Location {
        group_id: 0,
        object_id: 3,
    };
    let sub_rid = establish_subscription(
        &mut client,
        &mut server,
        MessageParameters::new(),
        alias,
        end,
    );
    // FETCH_OK を送らないまま FETCH を登録する
    let fetch_rid = client
        .send_fetch(
            track_namespace(),
            track_name(),
            fetch_range_params(start, end),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let fetch_stream = DataStreamId(223);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(0, 0))
        .expect("応答前の Object は受理されること");
    finish_fetch_stream(&mut client, fetch_stream);

    // 応答前は末尾 gap を確定しない。FETCH_OK で終端 (0, 3) が確定して初めて記録される
    server
        .send_fetch_ok(
            fetch_rid,
            fetch_ok_params(Some(Location {
                group_id: 0,
                object_id: 3,
            })),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(fetch_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let subgroup_stream = DataStreamId(224);
    open_subgroup_stream(&mut client, alias, 0, 0, subgroup_stream);
    let err = client
        .recv_subgroup_object(
            subgroup_stream,
            &DecodedSubgroupObject {
                object_id: 3,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("FETCH_OK で確定した末尾 gap 内の Object は Malformed Track になる");
    assert_eq!(
        err.reason,
        "malformed track: object received in a range recorded as non-existent"
    );
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
}

/// 降順で Group をまたぐ gap を要求範囲に切り詰めて記録する
///
/// draft-ietf-moq-transport-23 §3.2.2 (Gaps in a Fetch Stream): 降順で Start と End の Group が
/// 異なる場合、連続する Object が異なる Group にあるときの gap は先行 Group の残り・経由
/// Group・後続 Group の先頭になる。要求範囲外の Location を不存在として記録してはならない。
#[test]
fn descending_multi_group_gap_is_clamped_to_request() {
    let mut params = fetch_range_params(
        Location {
            group_id: 2,
            object_id: 0,
        },
        Location {
            group_id: 5,
            object_id: 2,
        },
    );
    params.push(MessageParameter {
        param_type: shiguredo_moqt::message_parameter::PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(2),
    });
    let (mut client, mut server) = establish_pair();
    let alias = 711;
    let sub_rid = establish_subscription(
        &mut client,
        &mut server,
        MessageParameters::new(),
        alias,
        Location {
            group_id: 5,
            object_id: 2,
        },
    );
    let fetch_rid = establish_fetch(
        &mut client,
        &mut server,
        params,
        Location {
            group_id: 5,
            object_id: 2,
        },
    );
    let fetch_stream = DataStreamId(225);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(5, 1))
        .expect("Object は受理されること");
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(4, 3))
        .expect("Group をまたぐ Object は受理されること");
    finish_fetch_stream(&mut client, fetch_stream);

    // 要求範囲の外 (End より後ろ) の Location は不存在として記録されていない
    let subgroup_stream = DataStreamId(226);
    open_subgroup_stream(&mut client, alias, 5, 0, subgroup_stream);
    recv_subgroup_object(&mut client, subgroup_stream, 3);
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Established,
        "要求範囲外の Object を誤拒否しないこと"
    );

    // 要求範囲の内側にある gap は不存在として記録されている
    let subgroup_stream = DataStreamId(229);
    open_subgroup_stream(&mut client, alias, 5, 1, subgroup_stream);
    let err = client
        .recv_subgroup_object(
            subgroup_stream,
            &DecodedSubgroupObject {
                object_id: 2,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("降順応答の gap 内の Object は Malformed Track になる");
    assert_eq!(
        err.reason,
        "malformed track: object received in a range recorded as non-existent"
    );
}

/// FETCH_OK より先に届いた降順 multi-group のデータで応答後半を誤拒否しない
///
/// draft-ietf-moq-transport-23 §3.2 (Fetch) は FETCH_OK がオブジェクト配信より後に
/// 届くことを許す。§3.2.2 (Gaps in a Fetch Stream) の降順 multi-group では先頭の期待
/// Location が {End Location.Group, 0} であるため、応答終端が未確定のうちに要求 Start を
/// 基準に先頭 gap を計算すると、後から届く経由 Group の正当な Object を不存在として
/// 記録してしまう。
#[test]
fn descending_data_before_fetch_ok_keeps_later_objects() {
    let start = Location {
        group_id: 2,
        object_id: 0,
    };
    let end = Location {
        group_id: 5,
        object_id: 2,
    };
    let mut params = fetch_range_params(start, end);
    params.push(MessageParameter {
        param_type: shiguredo_moqt::message_parameter::PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(2),
    });
    let (mut client, mut server) = establish_pair();
    let alias = 720;
    let sub_rid = establish_subscription(
        &mut client,
        &mut server,
        MessageParameters::new(),
        alias,
        end,
    );
    // FETCH_OK を送らないまま FETCH を登録する
    let fetch_rid = client
        .send_fetch(track_namespace(), track_name(), params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let fetch_stream = DataStreamId(240);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(5, 1))
        .expect("応答順の先頭 Object は受理されること");

    // 応答の終端を後から知らせる。ここで先頭 gap が要求 Start 基準で確定すると、
    // 経由 Group 全体が不存在として記録される
    server
        .send_fetch_ok(
            fetch_rid,
            fetch_ok_params(Some(end)),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(fetch_rid, ok_msg)
        .expect("FETCH_OK を受理すること");

    // 応答後半の経由 Group の Object は、要求 Start ではなく {End.Group, 0} を基準に
    // 判定されなければならない
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(4, 3))
        .expect("FETCH_OK 後の応答後半の正当な Object を誤拒否しないこと");
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(3, 0))
        .expect("経由 Group の正当な Object を誤拒否しないこと");
    finish_fetch_stream(&mut client, fetch_stream);

    // 先頭 gap は {End.Group, 0} から最初の Object までになる
    let subgroup_stream = DataStreamId(241);
    open_subgroup_stream(&mut client, alias, 5, 0, subgroup_stream);
    let err = client
        .recv_subgroup_object(
            subgroup_stream,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("先頭 gap 内の Object は Malformed Track になる");
    assert_eq!(
        err.reason,
        "malformed track: object received in a range recorded as non-existent"
    );
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
}

/// FETCH_OK より先に届いた昇順のデータでも応答後半を誤拒否しない
///
/// 昇順では要求 Start が先頭の期待 Location になるが、応答終端が未確定の間は末尾 gap を
/// 確定できない。FETCH_OK で確定した LARGEST_OBJECT より後ろの Location を誤って
/// 不存在にしないことを固定する。
#[test]
fn ascending_data_before_fetch_ok_keeps_later_objects() {
    let start = Location {
        group_id: 1,
        object_id: 0,
    };
    let end = Location {
        group_id: 1,
        object_id: 5,
    };
    let largest = Location {
        group_id: 1,
        object_id: 3,
    };
    let (mut client, mut server) = establish_pair();
    let alias = 721;
    // 応答を LARGEST_OBJECT (1, 3) で終わらせるため、購読の Largest Object も同じ値にする
    // (publisher 側の観測値が FETCH_OK の LARGEST_OBJECT を上書きするため)
    let sub_rid = establish_subscription(
        &mut client,
        &mut server,
        MessageParameters::new(),
        alias,
        largest,
    );
    // FETCH_OK を送らないまま FETCH を登録する
    let fetch_rid = client
        .send_fetch(
            track_namespace(),
            track_name(),
            fetch_range_params(start, end),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let fetch_stream = DataStreamId(242);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(1, 1))
        .expect("先頭 Object は受理されること");

    // 応答の終端を後から知らせ、その後に応答後半の Object を届ける
    server
        .send_fetch_ok(
            fetch_rid,
            fetch_ok_params(Some(largest)),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(fetch_rid, ok_msg)
        .expect("FETCH_OK を受理すること");
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(1, 2))
        .expect("FETCH_OK 後の応答後半の正当な Object を誤拒否しないこと");
    finish_fetch_stream(&mut client, fetch_stream);

    // 応答は LARGEST_OBJECT (1, 3) で終わるため、要求 End (1, 5) 側は存在を否定しない
    let outside_stream = DataStreamId(243);
    open_subgroup_stream(&mut client, alias, 1, 4, outside_stream);
    recv_subgroup_object(&mut client, outside_stream, 4);
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Established,
        "LARGEST_OBJECT より後ろの Object を誤拒否しないこと"
    );

    // 先頭 gap は要求 Start から最初の Object までになる
    let gap_stream = DataStreamId(244);
    open_subgroup_stream(&mut client, alias, 1, 0, gap_stream);
    let err = client
        .recv_subgroup_object(
            gap_stream,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("先頭 gap 内の Object は Malformed Track になる");
    assert_eq!(
        err.reason,
        "malformed track: object received in a range recorded as non-existent"
    );
}

/// FETCH_OK より先に届いた先頭 End of Non-Existent Range を応答終端で解決する
///
/// 降順 multi-group で応答終端が未確定の間は先頭 marker の基準点も決まらない。要求 Start を
/// 基準にすると経由 Group の正当な Object を不存在として記録してしまう。
#[test]
fn descending_leading_marker_before_fetch_ok_is_resolved() {
    let start = Location {
        group_id: 2,
        object_id: 0,
    };
    let end = Location {
        group_id: 5,
        object_id: 2,
    };
    let mut params = fetch_range_params(start, end);
    params.push(MessageParameter {
        param_type: shiguredo_moqt::message_parameter::PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(2),
    });
    let (mut client, mut server) = establish_pair();
    let alias = 722;
    let sub_rid = establish_subscription(
        &mut client,
        &mut server,
        MessageParameters::new(),
        alias,
        end,
    );
    // FETCH_OK を送らないまま FETCH を登録する
    let fetch_rid = client
        .send_fetch(track_namespace(), track_name(), params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let fetch_stream = DataStreamId(245);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    client
        .recv_fetch_entry(fetch_stream, &fetch_end_of_non_existent_range_entry(5, 0))
        .expect("先頭の End of Non-Existent Range は受理されること");

    // 応答終端を確定させてから、経由 Group の正当な Object を届ける
    server
        .send_fetch_ok(
            fetch_rid,
            fetch_ok_params(Some(end)),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(fetch_rid, ok_msg)
        .expect("FETCH_OK を受理すること");
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(4, 3))
        .expect("経由 Group の正当な Object を誤拒否しないこと");
    finish_fetch_stream(&mut client, fetch_stream);

    // marker (5, 0) と次の Object (4, 3) の間は不存在になる
    let subgroup_stream = DataStreamId(246);
    open_subgroup_stream(&mut client, alias, 5, 1, subgroup_stream);
    let err = client
        .recv_subgroup_object(
            subgroup_stream,
            &DecodedSubgroupObject {
                object_id: 1,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("先頭 marker から次の Object までの Object は Malformed Track になる");
    assert_eq!(
        err.reason,
        "malformed track: object received in a range recorded as non-existent"
    );
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
}

/// FIN でしか確定できない末尾 gap が、FETCH_OK の後に記録される
///
/// draft-ietf-moq-transport-23 §3.2.2 (Gaps in a Fetch Stream): "Gaps at the end of a
/// stream are only detectable when the stream is terminated by a FIN." 応答終端が未確定の
/// まま FIN した場合も、FETCH_OK の到着時に保留していた末尾 gap を確定しなければならない。
#[test]
fn fin_before_fetch_ok_records_trailing_gap() {
    let start = Location {
        group_id: 0,
        object_id: 0,
    };
    let end = Location {
        group_id: 0,
        object_id: 5,
    };
    let largest = Location {
        group_id: 0,
        object_id: 4,
    };
    let (mut client, mut server) = establish_pair();
    let alias = 723;
    let sub_rid = establish_subscription(
        &mut client,
        &mut server,
        MessageParameters::new(),
        alias,
        end,
    );
    // FETCH_OK を送らないまま FETCH を登録する
    let fetch_rid = client
        .send_fetch(
            track_namespace(),
            track_name(),
            fetch_range_params(start, end),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let fetch_stream = DataStreamId(247);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(0, 2))
        .expect("Object は受理されること");
    // 応答終端が未確定のまま FIN する。末尾 gap の判定は FETCH_OK まで保留される
    finish_fetch_stream(&mut client, fetch_stream);
    server
        .send_fetch_ok(
            fetch_rid,
            fetch_ok_params(Some(largest)),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(fetch_rid, ok_msg)
        .expect("FIN 後の FETCH_OK を受理すること");

    // 最後の Object (0, 2) から応答終端 (0, 4) までが不存在になる
    let subgroup_stream = DataStreamId(248);
    open_subgroup_stream(&mut client, alias, 0, 3, subgroup_stream);
    let err = client
        .recv_subgroup_object(
            subgroup_stream,
            &DecodedSubgroupObject {
                object_id: 3,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("FIN と FETCH_OK で確定した末尾 gap 内の Object は Malformed Track になる");
    assert_eq!(
        err.reason,
        "malformed track: object received in a range recorded as non-existent"
    );
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
}

/// data stream の id を使わない終端通知でも FIN 時点の末尾 gap を記録する
///
/// `recv_fetch_data_stream_closed` は request ID だけを受け取る。受理済みの FETCH 応答
/// stream が残っている場合は、stream id 経由の終端通知と同じく末尾 gap を確定する。
#[test]
fn direct_fetch_data_stream_fin_records_trailing_gap() {
    let start = Location {
        group_id: 0,
        object_id: 0,
    };
    let end = Location {
        group_id: 0,
        object_id: 4,
    };
    let (mut client, mut server) = establish_pair();
    let alias = 724;
    let sub_rid = establish_subscription(
        &mut client,
        &mut server,
        MessageParameters::new(),
        alias,
        end,
    );
    let fetch_rid = establish_fetch(
        &mut client,
        &mut server,
        fetch_range_params(start, end),
        end,
    );
    let fetch_stream = DataStreamId(249);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    client
        .recv_fetch_entry(fetch_stream, &fetch_object_entry(0, 1))
        .expect("Object は受理されること");
    // stream id ではなく request ID だけで FIN を通知する
    client
        .recv_fetch_data_stream_closed(fetch_rid, RequestStreamEnd::Fin)
        .expect("request ID だけの FIN 通知を受理すること");

    // 最後の Object (0, 1) から応答終端 (0, 4) までが不存在になる
    let subgroup_stream = DataStreamId(250);
    open_subgroup_stream(&mut client, alias, 0, 3, subgroup_stream);
    let err = client
        .recv_subgroup_object(
            subgroup_stream,
            &DecodedSubgroupObject {
                object_id: 3,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("request ID だけの FIN でも末尾 gap 内の Object は Malformed Track になる");
    assert_eq!(
        err.reason,
        "malformed track: object received in a range recorded as non-existent"
    );
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
}

/// fill fetch stream も同じ Track 履歴を共有する
///
/// draft-ietf-moq-transport-23 §3.4 (Fill Semantics) の fill fetch stream は FETCH 応答と
/// 同じデータストリーム形式であり、不存在の通知も Track 単位で共有される。
#[test]
fn fill_stream_shares_track_history() {
    let (mut client, mut server) = establish_pair();
    let alias = 712;
    let largest = Location {
        group_id: 5,
        object_id: 0,
    };
    let mut inner = MessageParameters::new();
    inner.push(MessageParameter {
        param_type: shiguredo_moqt::message_parameter::PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LocationFilter(LocationFilter::AbsoluteStart {
            start: Location {
                group_id: 0,
                object_id: 0,
            },
        }),
    });
    let mut fill_params = MessageParameters::new();
    fill_params.push(MessageParameter {
        param_type: PARAM_FILL_PARAMETERS,
        value: MessageParameterValue::FillParameters(inner),
    });
    let sub_rid = establish_subscription(&mut client, &mut server, fill_params, alias, largest);
    // publisher が開く初回 fill fetch stream を受信する
    let fill_stream = DataStreamId(227);
    open_fetch_stream(&mut client, sub_rid, fill_stream);
    client
        .recv_fetch_entry(fill_stream, &fetch_object_entry(0, 0))
        .expect("fill の Object は受理されること");
    client
        .recv_fetch_entry(fill_stream, &fetch_end_of_non_existent_range_entry(5, 0))
        .expect("fill の End of Non-Existent Range は受理されること");
    finish_fetch_stream(&mut client, fill_stream);

    let subgroup_stream = DataStreamId(228);
    open_subgroup_stream(&mut client, alias, 3, 0, subgroup_stream);
    let err = client
        .recv_subgroup_object(
            subgroup_stream,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("fill が通知した不存在区間内の Object は Malformed Track になる");
    assert_eq!(
        err.reason,
        "malformed track: object received in a range recorded as non-existent"
    );
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
}

/// fill の Range Filters が作る gap は Unknown であり不存在にしない
///
/// draft-ietf-moq-transport-23 §9.19.15 (FILL PARAMETERS Parameter) の内側の Range Filters も
/// §3.2.2 (Gaps in a Fetch Stream) の判定に使う。Range Filters がある場合の gap は Unknown で
/// あり、後から届く Object を誤拒否しない。
#[test]
fn fill_range_filter_gap_does_not_reject_later_object() {
    let (mut client, mut server) =
        establish_pair_with_options(SetupOptions::new(), max_filter_options());
    let alias = 713;
    let largest = Location {
        group_id: 5,
        object_id: 0,
    };
    let mut inner = MessageParameters::new();
    inner.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 10));
    let mut fill_params = MessageParameters::new();
    fill_params.push(MessageParameter {
        param_type: PARAM_FILL_PARAMETERS,
        value: MessageParameterValue::FillParameters(inner),
    });
    let sub_rid = establish_subscription(&mut client, &mut server, fill_params, alias, largest);
    let fill_stream = DataStreamId(230);
    open_fetch_stream(&mut client, sub_rid, fill_stream);
    client
        .recv_fetch_entry(fill_stream, &fetch_object_entry(0, 0))
        .expect("fill の Object は受理されること");
    client
        .recv_fetch_entry(fill_stream, &fetch_object_entry(5, 0))
        .expect("fill の Object は受理されること");
    finish_fetch_stream(&mut client, fill_stream);

    // fill の Range Filters が作った gap の Object は誤拒否されない
    let subgroup_stream = DataStreamId(231);
    open_subgroup_stream(&mut client, alias, 3, 0, subgroup_stream);
    recv_subgroup_object(&mut client, subgroup_stream, 0);
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Established,
        "fill の Range Filters が作る Unknown の gap で購読を終端しないこと"
    );
}

/// 受信した MalformedTrack イベントの理由を集める
fn drain_malformed_reasons(session: &mut Session) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    while let Some(event) = session.poll_event() {
        if let SessionEvent::RequestTerminated {
            reason: TerminationReason::MalformedTrack { reason },
            ..
        } = event
        {
            reasons.push(reason);
        }
    }
    reasons
}

/// FETCH 応答の Object Properties が不正な CODEC_STRING (0x11) を持つ場合に
/// MALFORMED_KEY_VALUE (0x6) を返すこと
///
/// draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure): 受信側が Type を理解する
/// 場合、Value が定義された serialization と一致しなければ MALFORMED_KEY_VALUE (0x6) で
/// セッションを閉じる (MUST)。CODEC_STRING の値は UTF-8 文字列
/// (draft-ietf-moq-loc-05 §2.3.1.3 (Codec String)) であるため、不正な UTF-8 は 0x6 に写る。
/// data plane の API はセッションを直接閉じず、戻り値のコードで通知する (閉じるのは呼び出し側)。
#[test]
fn fetch_object_with_invalid_codec_string_reports_malformed_key_value() {
    use shiguredo_moqt::error::SESSION_MALFORMED_KEY_VALUE;
    use shiguredo_moqt::loc::PROP_CODEC_STRING;

    let (mut client, mut server) = establish_pair();
    let fetch_rid = establish_fetch(
        &mut client,
        &mut server,
        fetch_range_params(
            Location {
                group_id: 0,
                object_id: 5,
            },
            Location {
                group_id: 0,
                object_id: 5,
            },
        ),
        Location {
            group_id: 0,
            object_id: 5,
        },
    );

    // CODEC_STRING (0x11) に不正な UTF-8 ([0xFF]) を載せた Object Properties を組み立てる。
    let mut properties_kvp = Vec::new();
    shiguredo_moqt::varint::encode(PROP_CODEC_STRING, &mut properties_kvp);
    shiguredo_moqt::varint::encode(1, &mut properties_kvp);
    properties_kvp.push(0xFF);
    let mut properties_bytes = Vec::new();
    shiguredo_moqt::varint::encode(properties_kvp.len() as u64, &mut properties_bytes);
    properties_bytes.extend_from_slice(&properties_kvp);

    let fetch_stream = DataStreamId(212);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    let err = client
        .recv_fetch_entry(
            fetch_stream,
            &DecodedFetchEntry::Object(DecodedFetchObject {
                group_id: 0,
                subgroup_id: 0,
                object_id: 5,
                publisher_priority: 128,
                is_datagram_origin: false,
                payload_length: 0,
                properties_bytes: Some(properties_bytes),
            }),
        )
        .expect_err("不正な UTF-8 の CODEC_STRING は MALFORMED_KEY_VALUE になること");
    assert_eq!(
        err.code, SESSION_MALFORMED_KEY_VALUE,
        "FETCH 応答の Object Properties も MALFORMED_KEY_VALUE (0x6) を返すこと"
    );
    assert_eq!(
        client.state(),
        SessionState::Established,
        "data plane のエラーは戻り値で通知し、セッション終了は呼び出し側が行うこと"
    );
}

/// FETCH 応答の Object Properties の宣言長と実データ長が一致しない場合に
/// MALFORMED_OBJECT (0x201) を返すこと
///
/// `ObjectProperties::decode_exact` は Properties Length と実データ長の不一致を
/// `MalformedObject` (MALFORMED_OBJECT 0x201) に写す
/// (draft-ietf-moq-transport-23 §11.1.2 (Object Properties) / §12.2
/// (Session Termination Codes))。FETCH 応答の Object Properties も同じ分類で通知される。
#[test]
fn fetch_object_with_inconsistent_properties_length_reports_malformed_object() {
    use shiguredo_moqt::error::SESSION_MALFORMED_OBJECT;

    let (mut client, mut server) = establish_pair();
    let fetch_rid = establish_fetch(
        &mut client,
        &mut server,
        fetch_range_params(
            Location {
                group_id: 0,
                object_id: 5,
            },
            Location {
                group_id: 0,
                object_id: 5,
            },
        ),
        Location {
            group_id: 0,
            object_id: 5,
        },
    );

    // Properties Length = 5 だが実データは 3 バイトしかない。
    let properties_bytes = vec![0x05, 0x11, 0x01, 0xFF];

    let fetch_stream = DataStreamId(213);
    open_fetch_stream(&mut client, fetch_rid, fetch_stream);
    let err = client
        .recv_fetch_entry(
            fetch_stream,
            &DecodedFetchEntry::Object(DecodedFetchObject {
                group_id: 0,
                subgroup_id: 0,
                object_id: 5,
                publisher_priority: 128,
                is_datagram_origin: false,
                payload_length: 0,
                properties_bytes: Some(properties_bytes),
            }),
        )
        .expect_err("Properties Length と実データ長の不一致は MALFORMED_OBJECT になること");
    assert_eq!(
        err.code, SESSION_MALFORMED_OBJECT,
        "FETCH 応答の Object Properties の長さ不一致も MALFORMED_OBJECT (0x201) を返すこと"
    );
}
