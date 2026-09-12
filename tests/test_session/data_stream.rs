use super::*;
use shiguredo_moqt::session::types::SendRequestError;

#[test]
fn data_stream_type_before_established_returns_bufferable_error() {
    let mut client = Session::new_client(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let err = client
        .recv_data_stream_type(DataStreamId(1), FETCH_HEADER_TYPE)
        .unwrap_err();
    assert!(matches!(
        err,
        shiguredo_moqt::session::types::RecvDataStreamError::BeforeSessionEstablished
    ));
    assert_eq!(client.state(), SessionState::LocalSetupSent);
    assert!(client.poll_event().is_some());
}

#[test]
fn unknown_data_stream_type_closes_session() {
    let (mut client, _) = establish_pair();
    let err = client
        .recv_data_stream_type(DataStreamId(1), 0x40)
        .unwrap_err();
    match err {
        shiguredo_moqt::session::types::RecvDataStreamError::Session(err) => {
            assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
        }
        other => panic!("予期しないエラー: {other:?}"),
    }
    assert_eq!(client.state(), SessionState::Closing);
    match drain_until_close(&mut client) {
        SessionEvent::CloseSession(err) => assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("予期しないイベント: {other:?}"),
    }
}

#[test]
fn subgroup_header_unknown_alias_can_be_retried_after_alias_established() {
    let (mut client, mut server) = establish_pair();
    let stream_id = DataStreamId(10);
    let header = SubgroupHeader {
        track_alias: 700,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(3),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };

    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    let outcome = client
        .recv_subgroup_header(stream_id, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(outcome, TrackDataAcceptance::UnknownTrackAlias);
    assert_eq!(client.state(), SessionState::Established);

    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 700, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let outcome = client
        .recv_subgroup_header(stream_id, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(outcome, TrackDataAcceptance::Accepted);
    client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
}

/// FirstObjectId モードの空 Subgroup（ヘッダーのみ + FIN、オブジェクト 0 個）が
/// セッションを fail させずに受理される
///
/// 受信側は FIN を「この Subgroup で受信すべき全オブジェクトを受信した」ことの
/// 保証として扱う（draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) の
/// "An MOQT implementation that processes a stream FIN is assured it has received all
/// objects in a subgroup from the start of the subscription."）。送信側がオブジェクトを
/// 1 つも送らない場合の終了方法は、同節後半が RESET_STREAM_AT を MAY で規定するが、
/// 受信側はワイヤから送信側の意図を判定できないため、FIN 終了を正規の完了として
/// 受理する。アプリは `SubgroupStreamDecoder::finish` が `Ok(())` を返したうえで
/// 本 API を呼ぶ。`SubgroupIdMode::FirstObjectId` の空 Subgroup では subgroup_id が
/// 未解決 (`None`) のまま終端が通知されるが、session は tracker 更新をスキップして
/// 終端を受理し、セッションを fail させない。
/// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
#[test]
fn empty_first_object_id_subgroup_fin_does_not_fail_session() {
    let (mut client, mut server, rid) = establish_subscribe_track(700);
    let stream_id = DataStreamId(10);

    // publisher が FirstObjectId モードで subgroup stream を開き、オブジェクトを送らない
    let header = SubgroupHeader {
        track_alias: 700,
        group_id: 1,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(stream_id, rid, &header)
        .expect("テストフィクスチャの前提条件を満たす");

    // subscriber がヘッダーを受理し、オブジェクト 0 個のまま FIN で終端を通知する
    // (FirstObjectId モードの stream type は 0x12)
    client
        .recv_data_stream_type(stream_id, 0x12)
        .expect("テストフィクスチャの前提条件を満たす");
    let outcome = client
        .recv_subgroup_header(stream_id, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(outcome, TrackDataAcceptance::Accepted);
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");

    // セッションは fail しない（CloseSession イベントが発行されない）
    assert_eq!(client.state(), SessionState::Established);
    while let Some(e) = client.poll_event() {
        assert!(
            !matches!(e, SessionEvent::CloseSession(_)),
            "予期しない CloseSession: {e:?}"
        );
    }
    // tracker 状態が汚染されていない (別 Subgroup のヘッダーを正常に受理できる)
    let next_header = SubgroupHeader {
        track_alias: 700,
        group_id: 2,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(DataStreamId(11), 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(11), &next_header)
            .expect("tracker 非汚染時は新規 Subgroup を受理できること"),
        TrackDataAcceptance::Accepted
    );
}

/// END_OF_GROUP 空 Subgroup（オブジェクト 0 個）+ FIN で Group 終端が記録され、
/// 後から届くオブジェクトが Malformed Track として拒否される
///
/// draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header) の END_OF_GROUP bit
/// ("indicates that this subgroup contains the largest Object in the Group") を
/// オブジェクト 0 個に適用すると、この Group には配達すべきオブジェクトが存在しない
/// ことを宣言したことになる。Group 終端は「存在しない最小の Object ID = 0」として
/// 記録し、後から同じ Group に届くオブジェクトは draft §12.1 (Malformed Tracks) の
/// 条件として検出される。
/// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
#[test]
fn end_of_group_empty_subgroup_fin_records_group_end() {
    let (mut client, mut server, rid) = establish_subscribe_track(700);
    let stream_id = DataStreamId(10);

    // publisher が END_OF_GROUP を立てた FirstObjectId モードの空 Subgroup を開く
    let header = SubgroupHeader {
        track_alias: 700,
        group_id: 1,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: true,
        first_object: false,
    };
    server
        .send_subgroup_header(stream_id, rid, &header)
        .expect("テストフィクスチャの前提条件を満たす");

    // subscriber がヘッダーを受理し、オブジェクト 0 個のまま FIN で終端を通知する
    // (FirstObjectId + END_OF_GROUP の stream type は 0x1A)
    client
        .recv_data_stream_type(stream_id, 0x1A)
        .expect("テストフィクスチャの前提条件を満たす");
    let outcome = client
        .recv_subgroup_header(stream_id, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(outcome, TrackDataAcceptance::Accepted);
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");

    // Group 終端が「存在しない最小 Object ID = 0」として記録される
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .ended_groups
            .get(&1),
        Some(&0),
        "空 Group の終端は存在しない最小 Object ID 0 として記録されること"
    );

    // 別 stream で同 Group のオブジェクトを送ると Malformed Track として拒否される
    open_incoming_subgroup(&mut client, DataStreamId(11), 700, 1, 1, false);
    let err = client
        .recv_subgroup_object(DataStreamId(11), &normal_object(0))
        .expect_err("End of Group 以降の object は拒否される");
    assert_eq!(err.reason, "object received after End of Group");
}

/// Zero モードの END_OF_GROUP 空 Subgroup（オブジェクト 0 個）+ FIN で Group 終端が記録される
///
/// FirstObjectId モードと違い subgroup_id が解決済み (Some(0)) のため、
/// `peer_subgroups.mark_fin(…, None)` で final Object が無いことを記録しつつ、
/// ended_groups にも終端が記録される。後から同じ Group に届くオブジェクトは
/// Malformed Track として検出される。
#[test]
fn end_of_group_empty_zero_subgroup_fin_records_group_end() {
    let (mut client, mut server, rid) = establish_subscribe_track(700);
    let stream_id = DataStreamId(10);

    // publisher が END_OF_GROUP を立てた Zero モードの空 Subgroup を開く
    let header = SubgroupHeader {
        track_alias: 700,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: true,
        first_object: false,
    };
    server
        .send_subgroup_header(stream_id, rid, &header)
        .expect("テストフィクスチャの前提条件を満たす");

    // subscriber がヘッダーを受理し、オブジェクト 0 個のまま FIN で終端を通知する
    // (Zero + END_OF_GROUP の stream type は 0x18)
    client
        .recv_data_stream_type(stream_id, 0x18)
        .expect("テストフィクスチャの前提条件を満たす");
    let outcome = client
        .recv_subgroup_header(stream_id, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(outcome, TrackDataAcceptance::Accepted);
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");

    // Group 終端が「存在しない最小 Object ID = 0」として記録される
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .ended_groups
            .get(&1),
        Some(&0),
        "空 Group の終端は存在しない最小 Object ID 0 として記録されること"
    );
}

#[test]
fn object_datagram_unknown_alias_is_reported_without_closing() {
    let (mut client, _) = establish_pair();
    let outcome = client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: 999,
            group_id: 3,
            object_id: 1,
            publisher_priority: Some(7),
            properties_data: None,
            end_of_group: false,
            status: None,
        })
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(outcome, TrackDataAcceptance::UnknownTrackAlias);
    assert_eq!(client.state(), SessionState::Established);
}

/// Malformed Track 検出時はセッションを閉じず該当 subscription だけを終端する
///
/// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks): "When a subscriber detects a
/// Malformed Track, it MUST cancel any corresponding subscription or fetches for that Track
/// from that publisher (see Section 6.4.2.3), and SHOULD deliver an error to the application."
#[test]
fn subgroup_object_malformed_track_terminates_subscription() {
    let (mut client, _, rid) = establish_subscribe_track(500);
    let stream_id = DataStreamId(11);
    let header = SubgroupHeader {
        track_alias: 500,
        group_id: 3,
        subgroup_id: SubgroupIdMode::Explicit(7),
        publisher_priority: Some(1),
        has_properties: true,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_id, 0x15)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &header)
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );
    client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 8,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");

    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_OBJECT_ID_GAP,
        value: ObjectPropertyValue::VarInt(2),
    });
    let mut encoded = Vec::new();
    props
        .encode(&mut encoded)
        .expect("正当なテスト入力の encode は成功する");

    let err = client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 10,
                payload_length: 1,
                status: None,
                properties_bytes: Some(encoded),
            },
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // §12.1 は subscription / fetch 単位の cancel を要求し session error を要求していない
    assert_eq!(
        client.state(),
        SessionState::Established,
        "Malformed Track でセッションを閉じてはいけない"
    );
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated,
        "該当 subscription が Terminated になること"
    );

    let mut saw_reset = false;
    let mut saw_terminated = false;
    while let Some(e) = client.poll_event() {
        match e {
            SessionEvent::ResetDataStream {
                stream_id: reset_id,
                error_code,
                reliable_size,
            } => {
                assert_eq!(reset_id, stream_id);
                assert_eq!(
                    error_code,
                    shiguredo_moqt::error::STREAM_MALFORMED_TRACK,
                    "MALFORMED_TRACK (0x12) で reset されること"
                );
                assert_eq!(
                    reliable_size, None,
                    "自動発火では RESET_STREAM (reliable_size なし) になること"
                );
                saw_reset = true;
            }
            SessionEvent::RequestTerminated {
                request_id,
                reason: TerminationReason::MalformedTrack { .. },
                ..
            } => {
                assert_eq!(request_id, rid);
                saw_terminated = true;
            }
            SessionEvent::CloseSession(err) => {
                panic!("セッションを閉じてはいけない: {err:?}")
            }
            _ => {}
        }
    }
    assert!(
        saw_reset,
        "ResetDataStream(MALFORMED_TRACK) が発行されること"
    );
    assert!(
        saw_terminated,
        "RequestTerminated(MalformedTrack) が発行されること (§12.1 の SHOULD)"
    );

    // malformed 終端後に bidi request stream が close しても二重終端せず no-op で吸収される
    client
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("malformed 後の bidi close は no-op で吸収されること");
    let mut terminated_after_close = 0;
    while let Some(e) = client.poll_event() {
        match e {
            SessionEvent::RequestTerminated { request_id, .. } if request_id == rid => {
                terminated_after_close += 1;
            }
            SessionEvent::CloseSession(err) => {
                panic!("セッションを閉じてはいけない: {err:?}")
            }
            _ => {}
        }
    }
    assert_eq!(
        terminated_after_close, 0,
        "malformed 後の bidi close で RequestTerminated は再発行されないこと"
    );
}

#[test]
fn fetch_header_and_stream_close_are_bound_by_stream_id() {
    let (mut client, _) = establish_pair();
    let rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam1".to_vec(),
            fetch_range_params(
                Location {
                    group_id: 0,
                    object_id: 0,
                },
                Location {
                    group_id: 1,
                    object_id: 0,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_send_request(&mut client);

    let stream_id = DataStreamId(12);
    assert_eq!(
        client
            .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
            .expect("テストフィクスチャの前提条件を満たす"),
        shiguredo_moqt::stream::DataStreamType::Fetch
    );
    client
        .recv_fetch_header(stream_id, &FetchHeader { request_id: rid })
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Terminated
    );
}

#[test]
fn stop_sending_on_subgroup_stream_allows_reopen() {
    let (mut client, _, _) = establish_subscribe_track(500);
    let header = SubgroupHeader {
        track_alias: 500,
        group_id: 9,
        subgroup_id: SubgroupIdMode::Explicit(4),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };

    client
        .recv_data_stream_type(DataStreamId(20), 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(20), &header)
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );
    client
        .send_data_stream_stop_sending(DataStreamId(20))
        .expect("テストフィクスチャの前提条件を満たす");

    // draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams): STOP_SENDING 後の
    // 再オープンは Forward 0→1 の REQUEST_UPDATE 受理後であることが sender 側の SHOULD 条件。
    // 受信側は peer の SHOULD NOT 違反をセッションエラーにせず受理する (本挙動は変更しない)
    client
        .recv_data_stream_type(DataStreamId(21), 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(21), &header)
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(client.state(), SessionState::Established);
}

#[test]
fn publish_done_requires_closed_outgoing_subgroup_streams() {
    let (mut client, mut server, rid) = establish_subscribe_track(700);
    let stream_id = DataStreamId(30);
    let header = SubgroupHeader {
        track_alias: 700,
        group_id: 2,
        subgroup_id: SubgroupIdMode::Explicit(5),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };

    server
        .send_subgroup_header(stream_id, rid, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .stream_counts
            .published_count,
        1
    );

    let err = server
        .send_publish_done(
            rid,
            0x2,
            1,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established
    );

    server
        .send_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = server
        .send_publish_done(
            rid,
            0x2,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);

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
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated
    );
}

#[test]
fn outgoing_subgroup_reset_tracks_delivery_timeout_reason() {
    let (_, mut server, rid) = establish_subscribe_track(702);
    let header = SubgroupHeader {
        track_alias: 702,
        group_id: 4,
        subgroup_id: SubgroupIdMode::Explicit(1),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(32), rid, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_data_stream_closed(
            DataStreamId(32),
            RequestStreamEnd::Reset {
                error_code: STREAM_DELIVERY_TIMEOUT,
                reliable_size: None,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    // reset 終端後は旧 stream id が除去される
    let err = server
        .send_subgroup_object(DataStreamId(32), 0, None)
        .expect_err("終端済み stream への送信は拒否されること");
    assert_eq!(
        err.as_session_error().map(|e| e.code),
        Some(SESSION_PROTOCOL_VIOLATION)
    );
    // reset 終端した Subgroup は再オープンできる (Reset は再オープン可)
    server
        .send_subgroup_header(DataStreamId(34), rid, &header)
        .expect("reset 済み Subgroup は再オープンできること");
}

#[test]
fn incoming_subgroup_reset_tracks_delivery_timeout_reason() {
    let (mut client, _, _rid) = establish_subscribe_track(703);
    let header = SubgroupHeader {
        track_alias: 703,
        group_id: 5,
        subgroup_id: SubgroupIdMode::Explicit(2),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(DataStreamId(33), 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(33), &header)
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );
    client
        .recv_data_stream_closed(
            DataStreamId(33),
            RequestStreamEnd::Reset {
                error_code: STREAM_DELIVERY_TIMEOUT,
                reliable_size: None,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    // reset 終端後は旧 stream id が除去される
    let old_obj = DecodedSubgroupObject {
        object_id: 0,
        payload_length: 1,
        status: None,
        properties_bytes: None,
    };
    client
        .recv_subgroup_object(DataStreamId(33), &old_obj)
        .expect_err("終端済み stream への受信は拒否されること");
    // reset 終端した Subgroup は再オープンできる (Reset は再オープン可)
    client
        .recv_data_stream_type(DataStreamId(35), 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(35), &header)
            .expect("reset 済み Subgroup は再オープンできること"),
        TrackDataAcceptance::Accepted
    );
}

#[test]
fn outgoing_first_object_id_stream_is_resolved_on_first_object() {
    let (_, mut server, rid) = establish_subscribe_track(701);
    server
        .send_subgroup_header(
            DataStreamId(31),
            rid,
            &SubgroupHeader {
                track_alias: 701,
                group_id: 4,
                subgroup_id: SubgroupIdMode::FirstObjectId,
                publisher_priority: Some(1),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(DataStreamId(31), 9, None)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .recv_data_stream_stop_sending(DataStreamId(31))
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_data_stream_closed(DataStreamId(31), RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");

    let err = server
        .send_subgroup_header(
            DataStreamId(32),
            rid,
            &SubgroupHeader {
                track_alias: 701,
                group_id: 4,
                subgroup_id: SubgroupIdMode::Explicit(9),
                publisher_priority: Some(1),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// Server (publisher) が送信した Object Datagram を Client (subscriber) が受信できる
///
/// `send_object_datagram` は client / server いずれの
/// レシーバでも統合テスト未カバーだった。relay の datagram 転送で Server が踏むため、
/// Server 送信 -> Client 受信の往復を検証する。
#[test]
fn server_send_object_datagram_received_by_client() {
    let alias = 500;
    let (mut client, mut server, rid) = establish_subscribe_track(alias);
    // Server (publisher) が datagram 送信を Session に通知する
    server
        .send_object_datagram(rid, 3, 1, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    // Client (subscriber) が同じ track alias の datagram を受信する
    let outcome = client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: alias,
            group_id: 3,
            object_id: 1,
            publisher_priority: Some(7),
            properties_data: None,
            end_of_group: false,
            status: None,
        })
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(outcome, TrackDataAcceptance::Accepted);
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(server.state(), SessionState::Established);
}

/// datagram 送信後、publisher subscription の最大位置が更新される
#[test]
fn send_object_datagram_updates_largest_received_location() {
    let (_, mut server, rid) = establish_subscribe_track(501);
    server
        .send_object_datagram(rid, 3, 1, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(
        sub.largest_received_location,
        Some(Location {
            group_id: 3,
            object_id: 1,
        })
    );
}

/// 同じ位置を再度送信しても最大位置は変わらない
#[test]
fn send_object_datagram_does_not_change_location_on_duplicate() {
    let (_, mut server, rid) = establish_subscribe_track(502);
    server
        .send_object_datagram(rid, 3, 1, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_object_datagram(rid, 3, 1, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(
        sub.largest_received_location,
        Some(Location {
            group_id: 3,
            object_id: 1,
        })
    );
}

/// 小さい位置から大きい位置へ最大位置が更新される
#[test]
fn send_object_datagram_max_location_from_small_to_large() {
    let (_, mut server, rid) = establish_subscribe_track(503);
    server
        .send_object_datagram(rid, 1, 1, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_object_datagram(rid, 2, 1, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(
        sub.largest_received_location,
        Some(Location {
            group_id: 2,
            object_id: 1,
        })
    );
}

/// 大きい位置の後に小さい位置を送信しても最大位置は維持される
#[test]
fn send_object_datagram_max_location_keeps_large_after_small() {
    let (_, mut server, rid) = establish_subscribe_track(504);
    server
        .send_object_datagram(rid, 2, 1, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_object_datagram(rid, 1, 1, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(
        sub.largest_received_location,
        Some(Location {
            group_id: 2,
            object_id: 1,
        })
    );
}

/// Location の比較は group_id → object_id の辞書順で行われる
#[test]
fn send_object_datagram_max_location_uses_lexicographic_order() {
    let (_, mut server, rid) = establish_subscribe_track(505);
    server
        .send_object_datagram(rid, 2, 10, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_object_datagram(rid, 3, 1, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(
        sub.largest_received_location,
        Some(Location {
            group_id: 3,
            object_id: 1,
        })
    );
}

/// 非 Normal status (EndOfGroup) の datagram でも最大位置は更新される
#[test]
fn send_object_datagram_updates_location_with_non_normal_status() {
    let (_, mut server, rid) = establish_subscribe_track(506);
    // EndOfGroup = 0x3
    server
        .send_object_datagram(rid, 3, 1, None, Some(3))
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(
        sub.largest_received_location,
        Some(Location {
            group_id: 3,
            object_id: 1,
        })
    );
}

/// 検証に失敗した場合は最大位置が更新されない
#[test]
fn send_object_datagram_failure_does_not_update_location() {
    let (_, mut server, rid) = establish_subscribe_track(507);
    // 空スライス (Properties Length varint すら含まない) は契約違反として拒否される
    let err = server
        .send_object_datagram(rid, 3, 1, Some(Vec::new()), None)
        .unwrap_err();
    assert_eq!(
        err.as_session_error().map(|e| e.code),
        Some(SESSION_PROTOCOL_VIOLATION)
    );
    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(sub.largest_received_location, None);
}

/// Properties Length = 0 と宣言長不一致の properties も送信前に拒否される
///
/// draft-ietf-moq-transport-21 §11.2.1 (Object Datagram): Datagram では Properties Length = 0 は
/// プロトコル違反。公開 API から不正なワイヤを生成しないよう Session 段でも早期に拒否する。
#[test]
fn send_object_datagram_rejects_invalid_properties_length() {
    for (label, properties_data) in [
        ("Length=0", Some(vec![0x00u8])),
        ("宣言長不一致", Some(vec![0x02u8, 0xAA])),
    ] {
        let (_, mut server, rid) = establish_subscribe_track(513);
        let err = server
            .send_object_datagram(rid, 3, 1, properties_data, None)
            .unwrap_err();
        assert_eq!(
            err.as_session_error().map(|e| e.code),
            Some(SESSION_PROTOCOL_VIOLATION),
            "{label}"
        );
        assert_eq!(
            server
                .subscription(rid)
                .expect("subscription が存在する")
                .largest_received_location,
            None,
            "{label}: 拒否時に最大位置を更新しないこと"
        );
        assert_eq!(server.state(), SessionState::Established, "{label}");
    }
}

/// subgroup stream 送信と datagram 送信を混在させた場合も最大位置が正しく合流する
#[test]
fn send_object_datagram_mixed_with_subgroup_stream_updates_location() {
    let (_, mut server, rid) = establish_subscribe_track(508);
    server
        .send_object_datagram(rid, 3, 1, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_header(
            DataStreamId(31),
            rid,
            &SubgroupHeader {
                track_alias: 508,
                group_id: 4,
                subgroup_id: SubgroupIdMode::Explicit(1),
                publisher_priority: Some(1),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(DataStreamId(31), 2, None)
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(
        sub.largest_received_location,
        Some(Location {
            group_id: 4,
            object_id: 2,
        })
    );
}

/// publisher role でない subscription では datagram 送信が拒否され最大位置は更新されない
#[test]
fn send_object_datagram_rejected_for_non_publisher_role() {
    let (mut client, mut server) = establish_pair();
    let rid = server
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut server);
    client
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .send_subscribe_ok(rid, 600, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // server は subscriber role なので send_object_datagram は拒否される
    let err = server
        .send_object_datagram(rid, 3, 1, None, None)
        .unwrap_err();
    assert_eq!(
        err.as_session_error().map(|e| e.code),
        Some(SESSION_PROTOCOL_VIOLATION)
    );
    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(sub.largest_received_location, None);
}

/// Established 以外の状態では datagram 送信が拒否され最大位置は更新されない
#[test]
fn send_object_datagram_rejected_before_established() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // SUBSCRIBE_OK 送信前は Pending 状態
    let err = server
        .send_object_datagram(rid, 3, 1, None, None)
        .unwrap_err();
    assert_eq!(
        err.as_session_error().map(|e| e.code),
        Some(SESSION_PROTOCOL_VIOLATION)
    );
    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(sub.largest_received_location, None);
}

/// 非 Normal status に properties を伴う datagram は拒否され最大位置は更新されない
#[test]
fn send_object_datagram_rejected_for_properties_with_non_normal_status() {
    let (_, mut server, rid) = establish_subscribe_track(510);
    let mut props = LocProperties::new();
    props.push(LocProperty {
        prop_id: PROP_TIMESTAMP,
        value: LocPropertyValue::VarInt(42),
    });
    let properties_data = Some(props.encode().expect("正当な LOC properties である"));
    let err = server
        .send_object_datagram(rid, 3, 1, properties_data, Some(0x3))
        .unwrap_err();
    assert_eq!(
        err.as_session_error().map(|e| e.code),
        Some(SESSION_PROTOCOL_VIOLATION)
    );
    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(sub.largest_received_location, None);
}

/// EndOfTrack (0x4) の datagram でも最大位置は更新される
#[test]
fn send_object_datagram_updates_location_with_end_of_track_status() {
    let (_, mut server, rid) = establish_subscribe_track(511);
    server
        .send_object_datagram(rid, 3, 1, None, Some(0x4))
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(
        sub.largest_received_location,
        Some(Location {
            group_id: 3,
            object_id: 1,
        })
    );
}

/// 非空の properties を伴う Normal status の datagram でも最大位置は更新される
#[test]
fn send_object_datagram_updates_location_with_non_empty_properties() {
    let (_, mut server, rid) = establish_subscribe_track(512);
    let mut props = LocProperties::new();
    props.push(LocProperty {
        prop_id: PROP_TIMESTAMP,
        value: LocPropertyValue::VarInt(42),
    });
    let properties_data = Some(props.encode().expect("正当な LOC properties である"));
    server
        .send_object_datagram(rid, 3, 1, properties_data, None)
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = server.subscription(rid).expect("subscription が存在する");
    assert_eq!(
        sub.largest_received_location,
        Some(Location {
            group_id: 3,
            object_id: 1,
        })
    );
}

/// datagram 送信後の REQUEST_UPDATE_OK に LARGEST_OBJECT パラメータが反映される
#[test]
fn send_object_datagram_reflected_in_request_update_ok() {
    let alias = 509;
    let (mut client, mut server, rid) = establish_subscribe_track(alias);

    // publisher 側が datagram を送信
    server
        .send_object_datagram(rid, 3, 1, None, None)
        .expect("テストフィクスチャの前提条件を満たす");

    // subscriber 側が REQUEST_UPDATE を送信
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, update_msg) = take_send_on_stream(&mut client);

    // publisher 側が REQUEST_UPDATE を受信
    server
        .recv_stream_message(rid, update_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(matches!(
        server.poll_event(),
        Some(SessionEvent::RequestUpdateReceived { .. })
    ));

    // publisher 側が REQUEST_UPDATE_OK を送信
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);

    // REQUEST_UPDATE_OK の LARGEST_OBJECT パラメータに datagram の位置が含まれる
    let request_ok = match ok_msg {
        ControlMessage::RequestOk(ok) => ok,
        other => panic!("RequestOk が期待されたが {other:?} を受け取った"),
    };
    assert_eq!(request_ok.parameters.largest_object(), Some((3, 1)));
}

/// draft-ietf-moq-transport-21 §10.1-12.2: per-subgroup delivery timeout オーバーライド
mod per_subgroup_delivery_timeout_override {
    use super::*;
    use shiguredo_moqt::track_properties::{
        PROP_OBJECT_DELIVERY_TIMEOUT, PROP_SUBGROUP_DELIVERY_TIMEOUT,
    };

    /// SUBGROUP_DELIVERY_TIMEOUT の Object Property を含むバイト列を構築する
    fn encode_subgroup_timeout_property(timeout_ms: u64) -> Vec<u8> {
        let mut props = ObjectProperties::new();
        props.push(ObjectProperty {
            prop_type: PROP_SUBGROUP_DELIVERY_TIMEOUT,
            value: ObjectPropertyValue::VarInt(timeout_ms),
        });
        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");
        buf
    }

    /// OBJECT_DELIVERY_TIMEOUT の Object Property を含むバイト列を構築する
    fn encode_object_timeout_property(timeout_ms: u64) -> Vec<u8> {
        let mut props = ObjectProperties::new();
        props.push(ObjectProperty {
            prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
            value: ObjectPropertyValue::VarInt(timeout_ms),
        });
        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");
        buf
    }

    /// subgroup stream を開いて先頭 object を受信するヘルパー
    fn open_subgroup_and_recv_first_object(
        client: &mut Session,
        stream_id: DataStreamId,
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
        properties_bytes: Option<Vec<u8>>,
    ) {
        let header = SubgroupHeader {
            track_alias,
            group_id,
            subgroup_id: SubgroupIdMode::Explicit(subgroup_id),
            publisher_priority: Some(1),
            has_properties: properties_bytes.is_some(),
            end_of_group: false,
            first_object: true,
        };
        client
            .recv_data_stream_type(stream_id, 0x14)
            .expect("テストフィクスチャの前提条件を満たす");
        let outcome = client
            .recv_subgroup_header(stream_id, &header)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(outcome, TrackDataAcceptance::Accepted);
        client
            .recv_subgroup_object(
                stream_id,
                &DecodedSubgroupObject {
                    object_id: 0,
                    payload_length: 1,
                    status: None,
                    properties_bytes,
                },
            )
            .expect("テストフィクスチャの前提条件を満たす");
    }

    #[test]
    fn first_object_subgroup_timeout_overrides_track_level() {
        // 先頭 object に SUBGROUP_DELIVERY_TIMEOUT=100 を付与すると
        // Track-level の 500 を上書きして effective subgroup timeout が 100 になる
        let alias = 600;
        let (mut client, _, rid) = establish_with_track_delivery_timeout(alias);

        open_subgroup_and_recv_first_object(
            &mut client,
            DataStreamId(20),
            alias,
            1,
            0,
            Some(encode_subgroup_timeout_property(100)),
        );

        let (subgroup_timeout, _object_timeout) =
            client.subgroup_effective_delivery_timeout(rid, 1, 0);
        assert_eq!(
            subgroup_timeout,
            Some(100),
            "先頭 object の SUBGROUP_DELIVERY_TIMEOUT が Track-level 値を上書きしなければいけない"
        );
    }

    #[test]
    fn first_object_object_timeout_overrides_track_level() {
        // 先頭 object に OBJECT_DELIVERY_TIMEOUT=200 を付与すると
        // Track-level の 500 を上書きして effective object timeout が 200 になる
        let alias = 601;
        let (mut client, _, rid) = establish_with_track_delivery_timeout(alias);

        open_subgroup_and_recv_first_object(
            &mut client,
            DataStreamId(21),
            alias,
            1,
            0,
            Some(encode_object_timeout_property(200)),
        );

        let (_subgroup_timeout, object_timeout) =
            client.subgroup_effective_delivery_timeout(rid, 1, 0);
        assert_eq!(
            object_timeout,
            Some(200),
            "先頭 object の OBJECT_DELIVERY_TIMEOUT が Track-level 値を上書きしなければいけない"
        );
    }

    #[test]
    fn non_first_object_timeout_is_ignored() {
        // 先頭以外の object に付与された delivery timeout は無視される
        let alias = 602;
        let (mut client, _, rid) = establish_with_track_delivery_timeout(alias);

        // 先頭 object は Property なし
        open_subgroup_and_recv_first_object(&mut client, DataStreamId(22), alias, 1, 0, None);

        // 2 番目の object に OBJECT_DELIVERY_TIMEOUT=100 を付与しても無視される
        client
            .recv_subgroup_object(
                DataStreamId(22),
                &DecodedSubgroupObject {
                    object_id: 1,
                    payload_length: 1,
                    status: None,
                    properties_bytes: Some(encode_object_timeout_property(100)),
                },
            )
            .expect("テストフィクスチャの前提条件を満たす");

        // Track-level の OBJECT_DELIVERY_TIMEOUT=500 が維持され、100 に上書きされていないこと
        let (_subgroup_timeout, object_timeout) =
            client.subgroup_effective_delivery_timeout(rid, 1, 0);
        assert_eq!(
            object_timeout,
            Some(500),
            "先頭以外の object の delivery timeout は無視され Track-level 値が維持されなければいけない"
        );
    }

    #[test]
    fn no_object_property_uses_track_level() {
        // Object Property がない場合は Track Property の値が使用される (回帰)
        let alias = 603;
        let (mut client, _, rid) = establish_with_track_delivery_timeout(alias);

        open_subgroup_and_recv_first_object(&mut client, DataStreamId(23), alias, 1, 0, None);

        let (subgroup_timeout, object_timeout) =
            client.subgroup_effective_delivery_timeout(rid, 1, 0);
        assert_eq!(
            object_timeout,
            Some(500),
            "Object Property なしで Track Property の値が使用されなければいけない"
        );
        assert_eq!(
            subgroup_timeout, None,
            "SUBGROUP_DELIVERY_TIMEOUT の Track Property は設定していないため None"
        );
    }

    #[test]
    fn multiple_subgroups_have_independent_timeouts() {
        // 同一 subscription 内で複数 subgroup が異なる timeout を持つ場合の独立性
        // 注意: 同一 (group_id, object_id) を異なる subgroup で受信すると §7.1 により
        // Malformed Track になるため、group_id を分ける
        let alias = 604;
        let (mut client, _, rid) = establish_with_track_delivery_timeout(alias);

        // subgroup (1, 0): SUBGROUP_DELIVERY_TIMEOUT=100
        open_subgroup_and_recv_first_object(
            &mut client,
            DataStreamId(24),
            alias,
            1,
            0,
            Some(encode_subgroup_timeout_property(100)),
        );

        // subgroup (2, 1): SUBGROUP_DELIVERY_TIMEOUT=300
        open_subgroup_and_recv_first_object(
            &mut client,
            DataStreamId(25),
            alias,
            2,
            1,
            Some(encode_subgroup_timeout_property(300)),
        );

        // 各 subgroup が独立した値を返すこと
        let (timeout_a, _) = client.subgroup_effective_delivery_timeout(rid, 1, 0);
        let (timeout_b, _) = client.subgroup_effective_delivery_timeout(rid, 2, 1);
        assert_eq!(
            timeout_a,
            Some(100),
            "subgroup (1, 0) は自身の override 値 100 を返さなければいけない"
        );
        assert_eq!(
            timeout_b,
            Some(300),
            "subgroup (2, 1) は自身の override 値 300 を返さなければいけない"
        );
    }

    #[test]
    fn stream_close_removes_override_entry() {
        // subgroup stream 終端後にエントリが削除され Track-level にフォールバックする
        let alias = 605;
        let (mut client, _, rid) = establish_with_track_delivery_timeout(alias);

        open_subgroup_and_recv_first_object(
            &mut client,
            DataStreamId(26),
            alias,
            1,
            0,
            Some(encode_object_timeout_property(100)),
        );

        // 終端前は override が有効
        let (_, object_timeout) = client.subgroup_effective_delivery_timeout(rid, 1, 0);
        assert_eq!(object_timeout, Some(100));

        // stream を閉じる
        client
            .recv_data_stream_closed(DataStreamId(26), RequestStreamEnd::Fin)
            .expect("テストフィクスチャの前提条件を満たす");

        // 終端後はエントリが削除され Track-level にフォールバック
        let (_, object_timeout_after) = client.subgroup_effective_delivery_timeout(rid, 1, 0);
        assert_eq!(
            object_timeout_after,
            Some(500),
            "stream 終端後は Track-level 値にフォールバックしなければいけない"
        );
    }
}

// ─── recv_datagram の type 分岐 (draft-ietf-moq-transport-21 §11) ────────────

/// Padding Datagram (draft-ietf-moq-transport-21 §11.5.2 (Padding Datagrams)) の raw バイト列
///
/// 先頭に type varint、続けて `length` バイトの 0x00 を並べる。
fn padding_datagram_bytes(length: usize) -> Vec<u8> {
    let mut raw = Vec::new();
    shiguredo_moqt::varint::encode(shiguredo_moqt::stream::PADDING_DATAGRAM_TYPE, &mut raw);
    raw.extend(core::iter::repeat_n(0x00, length));
    raw
}

/// 未知の datagram type を受信するとセッションを閉じる
///
/// draft-ietf-moq-transport-21 §11: "An endpoint that receives an unknown datagram type
/// MUST close the session."
///
/// `0x10` は bit4 が立っており §11.2.1 の `0b00X0XXXX` の形に合わない。
/// §11.2.1 は無効 Type 値に対して PROTOCOL_VIOLATION を規定する。
#[test]
fn recv_datagram_unknown_type_closes_session() {
    let (mut client, _server) = establish_pair();
    // type=0x10 (bit4 が立っている) + 適当な後続バイト
    let err = client
        .recv_datagram(&[0x10, 0x00, 0x00, 0x00])
        .expect_err("未知 datagram type はセッションを閉じる");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        err.reason, "unknown datagram type",
        "ヘッダ本体の不正ではなく type の不正として区別されること"
    );
    match drain_until_close(&mut client) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("CloseSession(PROTOCOL_VIOLATION) が期待されたが {other:?}"),
    }
}

/// STATUS と END_OF_GROUP を同時指定した type もセッションを閉じる
///
/// draft-ietf-moq-transport-21 §11.2.1: "Values with both the STATUS bit (0x20) and
/// END_OF_GROUP bit (0x02) set." 具体値 (0x22、0x23、0x26、0x27、0x2A、0x2B、0x2E、0x2F) は上記からの導出。
#[test]
fn recv_datagram_status_with_end_of_group_closes_session() {
    for type_id in [0x22u8, 0x23, 0x26, 0x27, 0x2A, 0x2B, 0x2E, 0x2F] {
        let (mut client, _server) = establish_pair();
        let err = client
            .recv_datagram(&[type_id, 0x00, 0x00, 0x00])
            .expect_err("STATUS + END_OF_GROUP の type は拒否される");
        assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION, "type={type_id:#x}");
        assert_eq!(
            err.reason, "unknown datagram type",
            "type={type_id:#x}: 無効 type として扱われること"
        );
        assert!(
            matches!(
                drain_until_close(&mut client),
                SessionEvent::CloseSession(_)
            ),
            "type={type_id:#x}: セッションが閉じること"
        );
    }
}

/// Padding Datagram は既知 type として破棄される
///
/// draft-ietf-moq-transport-21 §11.5.2: "The receiver MUST discard all data received in a
/// padding datagram."
#[test]
fn recv_datagram_padding_is_discarded() {
    let (mut client, _server) = establish_pair();
    let outcome = client
        .recv_datagram(&padding_datagram_bytes(16))
        .expect("Padding Datagram は受理される");
    assert_eq!(outcome, DatagramAcceptance::Padding);
    assert_eq!(
        client.state(),
        SessionState::Established,
        "Padding でセッションが閉じてはいけない"
    );
}

/// Object Datagram は decode して既存経路へ委譲される
#[test]
fn recv_datagram_object_delegates_to_object_path() {
    let alias = 777;
    let (mut client, _server, _rid) = establish_subscribe_track(alias);
    let raw = ObjectDatagram {
        track_alias: alias,
        group_id: 3,
        object_id: 1,
        publisher_priority: Some(7),
        properties_data: None,
        end_of_group: false,
        status: None,
    }
    .encode()
    .expect("ObjectDatagram の encode に成功すること");
    // Normal object は payload 必須 (§11.2.1: zero-length は Normal status を明示する形しか許されない)
    let raw = [raw, vec![0xAA]].concat();

    let outcome = client
        .recv_datagram(&raw)
        .expect("既知 Track Alias の Object Datagram は受理される");
    assert_eq!(
        outcome,
        DatagramAcceptance::Object(TrackDataAcceptance::Accepted)
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// datagram_writer と同じ組み立て (LocProperties::encode() → ObjectDatagram → encode →
/// recv_datagram) で受信が受理され、Malformed Track にならないこと
///
/// draft-ietf-moq-transport-21 §11.1.3 (Object Properties): Properties は
/// Properties Length + Key-Value-Pairs。`LocProperties::encode()` の Length 込み出力を
/// そのまま ObjectDatagram に渡す。
#[test]
fn datagram_writer_style_object_datagram_is_accepted() {
    let alias = 611;
    let (mut client, _server, _rid) = establish_subscribe_track(alias);
    // writer 側 (publisher) の組み立て
    let mut props = LocProperties::new();
    props.push(LocProperty {
        prop_id: PROP_TIMESTAMP,
        value: LocPropertyValue::VarInt(1234),
    });
    let encoded_props = props.encode().expect("正当な LOC properties である");
    let raw = ObjectDatagram {
        track_alias: alias,
        group_id: 5,
        object_id: 1,
        publisher_priority: Some(7),
        properties_data: Some(encoded_props),
        end_of_group: false,
        status: None,
    }
    .encode()
    .expect("ObjectDatagram の encode に成功すること");
    // Normal object は payload 必須 (§11.2.1: zero-length は Normal status を明示する形しか許されない)
    let raw = [raw, vec![0xAA]].concat();

    let outcome = client
        .recv_datagram(&raw)
        .expect("Length 込み Properties の datagram は受理される");
    assert_eq!(
        outcome,
        DatagramAcceptance::Object(TrackDataAcceptance::Accepted)
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 未知 Track Alias は §11.2 に従いセッションを閉じずに報告される
///
/// type は既知なので §11 の MUST の対象外であり、`recv_object_datagram` と同じ扱いになる。
#[test]
fn recv_datagram_unknown_alias_is_reported_without_closing() {
    let (mut client, _server) = establish_pair();
    let raw = ObjectDatagram {
        track_alias: 999,
        group_id: 3,
        object_id: 1,
        publisher_priority: Some(7),
        properties_data: None,
        end_of_group: false,
        status: None,
    }
    .encode()
    .expect("ObjectDatagram の encode に成功すること");
    // Normal object は payload 必須 (§11.2.1: zero-length は Normal status を明示する形しか許されない)
    let raw = [raw, vec![0xAA]].concat();

    let outcome = client
        .recv_datagram(&raw)
        .expect("未知 Track Alias はセッションを閉じない");
    assert_eq!(
        outcome,
        DatagramAcceptance::Object(TrackDataAcceptance::UnknownTrackAlias)
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// type は既知だがヘッダ本体が壊れている場合は別 reason でセッションを閉じる
#[test]
fn recv_datagram_malformed_object_header_closes_session() {
    let (mut client, _server) = establish_pair();
    // type=0x00 (有効) だが Track Alias 以降が足りない
    let err = client
        .recv_datagram(&[0x00])
        .expect_err("ヘッダ本体が不足していればセッションを閉じる");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        err.reason, "malformed OBJECT_DATAGRAM header",
        "unknown type と区別されること"
    );
}

/// 空の datagram は type varint を読めないためセッションを閉じる
#[test]
fn recv_datagram_empty_closes_session() {
    let (mut client, _server) = establish_pair();
    let err = client
        .recv_datagram(&[])
        .expect_err("type varint が読めなければセッションを閉じる");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(err.reason, "datagram type varint could not be decoded");
}

// ─── mid-object FIN (draft-ietf-moq-transport-21 §11.3 (Subgroup Streams)) ────────────

/// subgroup stream の raw バイト列を組み立てる
///
/// SUBGROUP_HEADER + Object 1 件 (payload 4 バイト) を返す。
fn subgroup_stream_bytes(track_alias: u64) -> (SubgroupHeader, Vec<u8>) {
    let header = SubgroupHeader {
        track_alias,
        group_id: 3,
        subgroup_id: SubgroupIdMode::Explicit(7),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let mut raw = header.encode();
    shiguredo_moqt::stream::subgroup::SubgroupObject {
        object_id_delta: 0,
        payload_length: 4,
        status: None,
    }
    .encode(false, None, &mut raw)
    .expect("SubgroupObject の encode に成功すること");
    raw.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    (header, raw)
}

/// Object 境界での FIN は従来どおり受理される
///
/// 実 decoder の `finish()` が `Ok(())` を返すことを確認したうえで
/// `recv_data_stream_closed` を呼ぶ (アプリケーションの正常系フロー)。
#[test]
fn object_boundary_fin_is_accepted() {
    let alias = 820;
    let (mut client, _server, _rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(31);
    let (header, raw) = subgroup_stream_bytes(alias);

    client
        .recv_data_stream_type(stream_id, raw[0] as u64)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("subgroup header の通知に成功すること");

    // 実 decoder に stream 全体を流し込み、Object 境界で終わっていることを確認する
    let mut decoder = shiguredo_moqt::stream::decoder::SubgroupStreamDecoder::new();
    decoder.push(&raw);
    decoder
        .try_decode_header()
        .expect("header の decode に成功すること")
        .expect("header が得られること");
    let object = decoder
        .try_decode_object()
        .expect("object の decode に成功すること")
        .expect("object が得られること");
    let payload = decoder
        .try_read_payload()
        .expect("payload が揃っているので取り出せること");
    assert_eq!(payload.len(), object.payload_length as usize);
    decoder
        .finish()
        .expect("Object 境界で終わっているので finish は成功する");

    client
        .recv_subgroup_object(stream_id, &object)
        .expect("object の通知に成功すること");
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("Object 境界での FIN は受理される");
    assert_eq!(
        client.state(),
        SessionState::Established,
        "Object 境界での FIN でセッションが閉じてはいけない"
    );
}

/// payload 途中での graceful FIN は PROTOCOL_VIOLATION でセッションを閉じる
///
/// draft-ietf-moq-transport-21 §11.3: "If a stream ends gracefully (i.e., the stream
/// terminates with a FIN) in the middle of a serialized Object, the session SHOULD be
/// closed with a PROTOCOL_VIOLATION."
#[test]
fn mid_payload_fin_closes_session() {
    let alias = 821;
    let (mut client, _server, _rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(32);
    let (header, raw) = subgroup_stream_bytes(alias);

    client
        .recv_data_stream_type(stream_id, raw[0] as u64)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("subgroup header の通知に成功すること");

    // payload を 1 バイト欠いた状態で stream が終わったことにする
    let mut decoder = shiguredo_moqt::stream::decoder::SubgroupStreamDecoder::new();
    decoder.push(&raw[..raw.len() - 1]);
    decoder
        .try_decode_header()
        .expect("header の decode に成功すること")
        .expect("header が得られること");
    decoder
        .try_decode_object()
        .expect("object の decode に成功すること")
        .expect("object が得られること");
    assert!(
        decoder.try_read_payload().is_none(),
        "payload が 1 バイト足りないので読み出せない"
    );
    decoder
        .finish()
        .expect_err("payload が足りないので finish は UnexpectedEof になる");

    let err = client
        .report_mid_object_fin(stream_id)
        .expect_err("mid-object FIN はセッションを閉じる");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        err.reason,
        "data stream finished in the middle of a serialized object"
    );
    match drain_until_close(&mut client) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("CloseSession(PROTOCOL_VIOLATION) が期待されたが {other:?}"),
    }
}

/// Object ヘッダ途中での graceful FIN も PROTOCOL_VIOLATION でセッションを閉じる
#[test]
fn mid_object_header_fin_closes_session() {
    let alias = 822;
    let (mut client, _server, _rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(33);
    let (header, raw) = subgroup_stream_bytes(alias);

    client
        .recv_data_stream_type(stream_id, raw[0] as u64)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("subgroup header の通知に成功すること");

    // Object ヘッダの手前までしか届いていない状態を作る
    let header_len = header.encode().len();
    let mut decoder = shiguredo_moqt::stream::decoder::SubgroupStreamDecoder::new();
    decoder.push(&raw[..header_len + 1]);
    decoder
        .try_decode_header()
        .expect("header の decode に成功すること")
        .expect("header が得られること");
    decoder
        .finish()
        .expect_err("Object ヘッダ途中なので finish は UnexpectedEof になる");

    let err = client
        .report_mid_object_fin(stream_id)
        .expect_err("mid-object FIN はセッションを閉じる");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert!(matches!(
        drain_until_close(&mut client),
        SessionEvent::CloseSession(_)
    ));
}

/// recv_data_stream_closed を先に呼んでいても report_mid_object_fin はセッションを閉じる
///
/// アプリケーションが FIN 通知と decoder 検査のどちらを先に行うかで結果が変わらないこと。
#[test]
fn report_mid_object_fin_after_stream_closed_still_closes_session() {
    let alias = 823;
    let (mut client, _server, _rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(34);
    let (header, raw) = subgroup_stream_bytes(alias);

    client
        .recv_data_stream_type(stream_id, raw[0] as u64)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("subgroup header の通知に成功すること");
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("FIN 通知が先に来ても受理される");

    let err = client
        .report_mid_object_fin(stream_id)
        .expect_err("登録解除済みでもセッションを閉じる");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert!(matches!(
        drain_until_close(&mut client),
        SessionEvent::CloseSession(_)
    ));
}

/// header 未受信 (AwaitingHeader) での FIN は mid-object ではないので受理される
///
/// draft-ietf-moq-transport-21 §11.3 の mid-object は「シリアライズ途中の Object」であり、
/// ヘッダを 1 バイトも受け取らずに終わった stream は該当しない。境界条件の確認。
#[test]
fn awaiting_header_fin_is_accepted() {
    let (mut client, _server, _rid) = establish_subscribe_track(824);
    let stream_id = DataStreamId(35);
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("stream type の通知に成功すること");
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("header 未受信での FIN は受理される");
    assert_eq!(client.state(), SessionState::Established);
}

// ─── Malformed Track の request 単位終端 (draft §12.1) ─────────────────

/// Malformed Track を検出しても他の subscription は生き残る
///
/// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) は「該当 Track の subscription /
/// fetch を cancel する」ことだけを要求しており、セッションや他 request には影響しない。
#[test]
fn malformed_track_keeps_other_subscriptions_alive() {
    let (mut client, mut server) = establish_pair();
    // 2 本の subscription を別 Track / 別 alias で確立する
    let mut rids = Vec::new();
    for (name, alias) in [(b"cam".as_slice(), 600u64), (b"mic".as_slice(), 601)] {
        let rid = client
            .send_subscribe(ns(&[b"live"]), name.to_vec(), MessageParameters::new())
            .expect("SUBSCRIBE の送信に成功すること");
        let (_, sub_msg) = take_send_request(&mut client);
        server
            .recv_request(sub_msg)
            .expect("SUBSCRIBE の受信に成功すること");
        server
            .send_subscribe_ok(rid, alias, MessageParameters::new(), TrackProperties::new())
            .expect("SUBSCRIBE_OK の送信に成功すること");
        let (_, ok_msg) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(rid, ok_msg)
            .expect("SUBSCRIBE_OK の受信に成功すること");
        rids.push(rid);
    }

    // 1 本目 (alias 600) で Malformed Track を起こす
    let stream_id = DataStreamId(21);
    let header = SubgroupHeader {
        track_alias: 600,
        group_id: 3,
        subgroup_id: SubgroupIdMode::Explicit(7),
        publisher_priority: Some(1),
        has_properties: true,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_id, 0x15)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("subgroup header の通知に成功すること");
    client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 8,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect("1 つ目の object は受理される");
    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_OBJECT_ID_GAP,
        value: ObjectPropertyValue::VarInt(2),
    });
    let mut encoded = Vec::new();
    props.encode(&mut encoded).expect("encode に成功すること");
    client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 10,
                payload_length: 1,
                status: None,
                properties_bytes: Some(encoded),
            },
        )
        .expect_err("Malformed Track として拒否される");

    assert_eq!(
        client
            .subscription(rids[0])
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated,
        "該当 subscription は Terminated"
    );
    assert_eq!(
        client
            .subscription(rids[1])
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Established,
        "他の subscription は Established のまま生き残ること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// datagram 経路の Malformed Track も subscription 単位で終端する
///
/// datagram は stream を持たないので `ResetDataStream` は発行されない。
#[test]
fn malformed_track_via_datagram_terminates_subscription() {
    let alias = 610;
    let (mut client, _server, rid) = establish_subscribe_track(alias);

    client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: alias,
            group_id: 3,
            object_id: 8,
            publisher_priority: Some(7),
            properties_data: None,
            end_of_group: false,
            status: None,
        })
        .expect("1 つ目の datagram は受理される");

    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_OBJECT_ID_GAP,
        value: ObjectPropertyValue::VarInt(2),
    });
    let mut encoded = Vec::new();
    props.encode(&mut encoded).expect("encode に成功すること");
    let err = client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: alias,
            group_id: 3,
            object_id: 10,
            publisher_priority: Some(7),
            properties_data: Some(encoded),
            end_of_group: false,
            status: None,
        })
        .expect_err("Malformed Track として拒否される");
    assert_eq!(
        err.reason, "malformed track: PRIOR_OBJECT_ID_GAP covers a previously received object",
        "二重 Length による decode 失敗ではなく PRIOR_OBJECT_ID_GAP 経路で拒否されること"
    );

    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
    assert_eq!(client.state(), SessionState::Established);

    let mut saw_terminated = false;
    while let Some(e) = client.poll_event() {
        match e {
            SessionEvent::ResetDataStream { .. } => {
                panic!("datagram には reset 対象の stream が無い")
            }
            SessionEvent::RequestTerminated {
                reason: TerminationReason::MalformedTrack { reason },
                ..
            } => {
                assert_eq!(
                    reason,
                    "malformed track: PRIOR_OBJECT_ID_GAP covers a previously received object"
                );
                saw_terminated = true;
            }
            SessionEvent::CloseSession(err) => panic!("セッションを閉じてはいけない: {err:?}"),
            _ => {}
        }
    }
    assert!(
        saw_terminated,
        "RequestTerminated(MalformedTrack) が発行されること"
    );
}

// ─── EndOfGroup / EndOfTrack の意味論 (draft §11.1.2 / §11.3.1 / §11.2.1) ─────

/// subgroup stream を開いて object を 1 件流すヘルパー
fn open_incoming_subgroup(
    client: &mut Session,
    stream_id: DataStreamId,
    track_alias: u64,
    group_id: u64,
    subgroup_id: u64,
    end_of_group: bool,
) {
    let header = SubgroupHeader {
        track_alias,
        group_id,
        subgroup_id: SubgroupIdMode::Explicit(subgroup_id),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_id, header.encode()[0] as u64)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("subgroup header の通知に成功すること");
}

/// status のみ (payload なし) の subgroup object を作る
fn status_object(object_id: u64, status: u64) -> DecodedSubgroupObject {
    DecodedSubgroupObject {
        object_id,
        payload_length: 0,
        status: Some(status),
        properties_bytes: None,
    }
}

/// 通常 object を作る
fn normal_object(object_id: u64) -> DecodedSubgroupObject {
    DecodedSubgroupObject {
        object_id,
        payload_length: 1,
        status: None,
        properties_bytes: None,
    }
}

/// Object Status 0x3 (End of Group) 以降の同 Group Object は拒否される
///
/// draft-ietf-moq-transport-21 §11.1.2 (Object Status): "Indicates that no objects with the
/// specified Group ID and the Object ID that is greater than or equal to the one specified exist
/// in the group identified by the Group ID."
#[test]
fn object_after_end_of_group_status_is_rejected() {
    let alias = 700;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    open_incoming_subgroup(&mut client, DataStreamId(80), alias, 3, 0, false);

    client
        .recv_subgroup_object(DataStreamId(80), &normal_object(1))
        .expect("終端前の object は受理される");
    client
        .recv_subgroup_object(DataStreamId(80), &status_object(5, 0x3))
        .expect("End of Group マーカーは受理される");
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .ended_groups
            .get(&3),
        Some(&5),
        "存在しない最小 Object ID として 5 が記録されること"
    );

    // 別 stream で同 Group の object_id >= 5 を送る
    open_incoming_subgroup(&mut client, DataStreamId(81), alias, 3, 1, false);
    let err = client
        .recv_subgroup_object(DataStreamId(81), &normal_object(5))
        .expect_err("End of Group 以降の object は拒否される");
    assert_eq!(err.reason, "object received after End of Group");
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated,
        "§12.1 の Malformed Track 経路で subscription が終端されること"
    );
    assert_eq!(
        client.state(),
        SessionState::Established,
        "セッションは閉じない"
    );
}

/// End of Group より小さい Object ID は引き続き受理される (境界)
#[test]
fn object_before_end_of_group_is_accepted() {
    let alias = 701;
    let (mut client, _server, _rid) = establish_subscribe_track(alias);
    open_incoming_subgroup(&mut client, DataStreamId(82), alias, 3, 0, false);
    client
        .recv_subgroup_object(DataStreamId(82), &status_object(5, 0x3))
        .expect("End of Group マーカーは受理される");

    open_incoming_subgroup(&mut client, DataStreamId(83), alias, 3, 1, false);
    client
        .recv_subgroup_object(DataStreamId(83), &normal_object(4))
        .expect("End of Group より小さい Object ID は受理される");
}

/// Object Status 0x4 (End of Track) 以降の Object は拒否される
///
/// draft-ietf-moq-transport-21 §11.1.2 (Object Status): "Indicates that no objects with the
/// location that is equal to or greater than the one specified exist."
#[test]
fn object_after_end_of_track_status_is_rejected() {
    let alias = 702;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    open_incoming_subgroup(&mut client, DataStreamId(84), alias, 3, 0, false);
    client
        .recv_subgroup_object(DataStreamId(84), &status_object(7, 0x4))
        .expect("End of Track マーカーは受理される");
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .end_of_track,
        Some(Location {
            group_id: 3,
            object_id: 7
        })
    );

    // より後ろの Group の object も拒否される
    open_incoming_subgroup(&mut client, DataStreamId(85), alias, 4, 0, false);
    let err = client
        .recv_subgroup_object(DataStreamId(85), &normal_object(0))
        .expect_err("End of Track 以降の object は拒否される");
    assert_eq!(err.reason, "object received after End of Track");
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
}

/// End of Track より前の Location は受理される (境界)
#[test]
fn object_before_end_of_track_is_accepted() {
    let alias = 703;
    let (mut client, _server, _rid) = establish_subscribe_track(alias);
    open_incoming_subgroup(&mut client, DataStreamId(86), alias, 5, 0, false);
    client
        .recv_subgroup_object(DataStreamId(86), &status_object(7, 0x4))
        .expect("End of Track マーカーは受理される");

    open_incoming_subgroup(&mut client, DataStreamId(87), alias, 4, 0, false);
    client
        .recv_subgroup_object(DataStreamId(87), &normal_object(999))
        .expect("End of Track より前の Group は受理される");
}

/// SUBGROUP_HEADER の END_OF_GROUP bit + FIN で Group 終端が確定する
///
/// draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header): "indicates that this subgroup contains
/// the largest Object in the Group. When set to 1, the subscriber can infer the final Object in
/// the Group when the data stream is terminated by a FIN."
#[test]
fn end_of_group_header_bit_with_fin_sets_group_end() {
    let alias = 704;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    open_incoming_subgroup(&mut client, DataStreamId(88), alias, 3, 0, true);
    client
        .recv_subgroup_object(DataStreamId(88), &normal_object(2))
        .expect("object は受理される");

    // FIN 前は終端が確定していない
    assert!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .ended_groups
            .is_empty(),
        "FIN 前は Group 終端が確定しない"
    );

    client
        .recv_data_stream_closed(DataStreamId(88), RequestStreamEnd::Fin)
        .expect("FIN の通知に成功すること");
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .ended_groups
            .get(&3),
        Some(&3),
        "最終 Object ID 2 の 1 つ後ろ (3) が存在しない最小 ID になること"
    );

    // 以降その Group に object_id >= 3 は来られない
    open_incoming_subgroup(&mut client, DataStreamId(89), alias, 3, 1, false);
    client
        .recv_subgroup_object(DataStreamId(89), &normal_object(3))
        .expect_err("Group 終端後の object は拒否される");
}

/// END_OF_GROUP bit が立っていても RESET なら Group 終端は確定しない
///
/// §11.3.1 は "when the data stream is terminated by a FIN" と FIN に限定している。
#[test]
fn end_of_group_header_bit_with_reset_does_not_set_group_end() {
    let alias = 705;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    open_incoming_subgroup(&mut client, DataStreamId(90), alias, 3, 0, true);
    client
        .recv_subgroup_object(DataStreamId(90), &normal_object(2))
        .expect("object は受理される");
    client
        .recv_data_stream_closed(
            DataStreamId(90),
            RequestStreamEnd::Reset {
                error_code: 0,
                reliable_size: None,
            },
        )
        .expect("RESET の通知に成功すること");

    assert!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .ended_groups
            .is_empty(),
        "RESET では Group 終端が確定しないこと"
    );
}

/// datagram の END_OF_GROUP bit が状態に反映される
///
/// draft-ietf-moq-transport-21 §11.2.1 (Object Datagram): "no Object with the same Group ID and
/// an Object ID greater than the Object ID in this datagram exists."
#[test]
fn datagram_end_of_group_bit_sets_group_end() {
    let alias = 706;
    let (mut client, _server, rid) = establish_subscribe_track(alias);

    client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: alias,
            group_id: 3,
            object_id: 4,
            publisher_priority: Some(7),
            properties_data: None,
            end_of_group: true,
            status: None,
        })
        .expect("END_OF_GROUP bit 付き datagram は受理される");
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .ended_groups
            .get(&3),
        Some(&5),
        "宣言位置 4 の 1 つ後ろ (5) が存在しない最小 ID になること"
    );

    // object_id 4 はまだ有効、5 は拒否される
    client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: alias,
            group_id: 3,
            object_id: 4,
            publisher_priority: Some(7),
            properties_data: None,
            end_of_group: false,
            status: None,
        })
        .expect("宣言位置自身は存在しうる");
    client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: alias,
            group_id: 3,
            object_id: 5,
            publisher_priority: Some(7),
            properties_data: None,
            end_of_group: false,
            status: None,
        })
        .expect_err("宣言位置より大きい Object ID は拒否される");
}

// ─── Stream Reset Error Codes (draft §12.5 / §11.3.2) ────────────

/// 理由からエラーコードへの対応表が draft §12.5 と一致する
#[test]
fn data_stream_reset_reason_maps_to_spec_codes() {
    use shiguredo_moqt::error::{
        STREAM_CANCELLED, STREAM_DELIVERY_TIMEOUT, STREAM_EXCESSIVE_LOAD,
        STREAM_EXPIRED_AUTH_TOKEN, STREAM_GOING_AWAY, STREAM_INTERNAL_ERROR,
        STREAM_MALFORMED_TRACK, STREAM_SESSION_CLOSED, STREAM_TOO_FAR_BEHIND,
        STREAM_UNKNOWN_OBJECT_STATUS,
    };
    use shiguredo_moqt::session::types::DataStreamResetReason as R;
    assert_eq!(R::InternalError.error_code(), STREAM_INTERNAL_ERROR);
    assert_eq!(R::Cancelled.error_code(), STREAM_CANCELLED);
    assert_eq!(R::DeliveryTimeout.error_code(), STREAM_DELIVERY_TIMEOUT);
    assert_eq!(R::SessionClosed.error_code(), STREAM_SESSION_CLOSED);
    assert_eq!(R::GoingAway.error_code(), STREAM_GOING_AWAY);
    assert_eq!(R::TooFarBehind.error_code(), STREAM_TOO_FAR_BEHIND);
    assert_eq!(
        R::UnknownObjectStatus.error_code(),
        STREAM_UNKNOWN_OBJECT_STATUS
    );
    assert_eq!(R::ExpiredAuthToken.error_code(), STREAM_EXPIRED_AUTH_TOKEN);
    assert_eq!(R::ExcessiveLoad.error_code(), STREAM_EXCESSIVE_LOAD);
    assert_eq!(R::MalformedTrack.error_code(), STREAM_MALFORMED_TRACK);
}

/// publisher 側の outgoing subgroup stream を開く
fn open_outgoing_subgroup(
    server: &mut Session,
    rid: u64,
    stream_id: DataStreamId,
    alias: u64,
    group_id: u64,
    subgroup_id: u64,
) {
    server
        .send_subgroup_header(
            stream_id,
            rid,
            &SubgroupHeader {
                track_alias: alias,
                group_id,
                subgroup_id: SubgroupIdMode::Explicit(subgroup_id),
                publisher_priority: Some(1),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("subgroup header の送信に成功すること");
}

/// アプリ起点の cancel は `STREAM_CANCELLED` (0x1) で reset される
///
/// draft-ietf-moq-transport-21 §12.5 (Stream Reset Error Codes): "CANCELLED (0x1): The stream
/// was cancelled by either endpoint."
#[test]
fn cancel_uses_stream_cancelled_code() {
    let alias = 800;
    let (_client, mut server, rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(95);
    open_outgoing_subgroup(&mut server, rid, stream_id, alias, 3, 0);
    while server.poll_event().is_some() {}

    server
        .reset_outgoing_data_stream(stream_id, DataStreamResetReason::Cancelled)
        .expect("cancel に成功すること");

    let mut saw_reset = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::ResetDataStream {
            stream_id: reset_id,
            error_code,
            reliable_size,
        } = e
        {
            assert_eq!(reset_id, stream_id);
            assert_eq!(
                error_code,
                shiguredo_moqt::error::STREAM_CANCELLED,
                "CANCELLED (0x1) が使われること"
            );
            assert_eq!(
                reliable_size, None,
                "旧 API では RESET_STREAM (reliable_size なし) になること"
            );
            saw_reset = true;
        }
    }
    assert!(saw_reset, "ResetDataStream が発行されること");
}

/// RESET_STREAM_AT の reliable_size はイベントに反映され、追跡状態も終端する
///
/// draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams)
#[test]
fn reset_at_propagates_reliable_size_to_event_and_tracker() {
    let alias = 802;
    let (_client, mut server, rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(96);
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 3,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(stream_id, rid, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    while server.poll_event().is_some() {}

    server
        .reset_outgoing_data_stream_at(stream_id, DataStreamResetReason::Cancelled, Some(42))
        .expect("reset_at に成功すること");

    let mut saw_reset = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::ResetDataStream {
            stream_id: reset_id,
            error_code,
            reliable_size,
        } = e
        {
            assert_eq!(reset_id, stream_id);
            assert_eq!(
                error_code,
                shiguredo_moqt::error::STREAM_CANCELLED,
                "CANCELLED (0x1) が使われること"
            );
            assert_eq!(
                reliable_size,
                Some(42),
                "指定した reliable_size がイベントに反映されること"
            );
            saw_reset = true;
        }
    }
    assert!(
        saw_reset,
        "RESET_STREAM_AT では reliable_size 付きの ResetDataStream が発行されること"
    );
    // 追跡状態も終端している (旧 stream id が除去され、同一 Subgroup を再オープンできる)
    let err = server
        .send_subgroup_object(stream_id, 0, None)
        .expect_err("終端済み stream への送信は拒否されること");
    assert_eq!(
        err.as_session_error().map(|e| e.code),
        Some(SESSION_PROTOCOL_VIOLATION)
    );
    server
        .send_subgroup_header(DataStreamId(97), rid, &header)
        .expect("reset 済み Subgroup は再オープンできること");
}

/// reset だけでは subscription 状態が壊れない
///
/// uni stream の早期終了は MOQT application state に影響しない
/// (draft-ietf-moq-transport-21 §6.4.1 (Unidirectional Streams)。旧 §11.4.1 (Stream Cancellation)
/// は -21 で削除された)
#[test]
fn reset_does_not_affect_subscription_state() {
    let alias = 801;
    let (_client, mut server, rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(96);
    open_outgoing_subgroup(&mut server, rid, stream_id, alias, 3, 0);

    server
        .reset_outgoing_data_stream(stream_id, DataStreamResetReason::Cancelled)
        .expect("cancel に成功すること");

    assert_eq!(
        server
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Established,
        "reset で subscription が終了してはいけない"
    );
    assert_eq!(server.state(), SessionState::Established);

    // 同一 subscription で別 subgroup stream を開き直して送信できること
    let next_stream = DataStreamId(97);
    open_outgoing_subgroup(&mut server, rid, next_stream, alias, 3, 1);
    server
        .send_subgroup_object(next_stream, 0, None)
        .expect("reset 後も同一 subscription で送信できること");
}

/// FETCH 応答で次の Object の status が判定できない場合は UNKNOWN_OBJECT_STATUS を使える
///
/// draft-ietf-moq-transport-21 §12.5 (Stream Reset Error Codes): "UNKNOWN_OBJECT_STATUS (0x6):
/// In response to a FETCH, the publisher is unable to determine the status of the next Object in
/// the requested range."
#[test]
fn unknown_object_status_code_is_available() {
    let alias = 802;
    let (_client, mut server, rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(98);
    open_outgoing_subgroup(&mut server, rid, stream_id, alias, 3, 0);
    while server.poll_event().is_some() {}

    server
        .reset_outgoing_data_stream(stream_id, DataStreamResetReason::UnknownObjectStatus)
        .expect("reset に成功すること");

    let mut codes = Vec::new();
    while let Some(e) = server.poll_event() {
        if let SessionEvent::ResetDataStream { error_code, .. } = e {
            codes.push(error_code);
        }
    }
    assert_eq!(
        codes,
        vec![shiguredo_moqt::error::STREAM_UNKNOWN_OBJECT_STATUS]
    );
}

/// 生のコード指定もできる
#[test]
fn reset_with_raw_code_is_supported() {
    let alias = 803;
    let (_client, mut server, rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(99);
    open_outgoing_subgroup(&mut server, rid, stream_id, alias, 3, 0);
    while server.poll_event().is_some() {}

    server
        .reset_outgoing_data_stream_with_code(stream_id, 0xABCD)
        .expect("生コード指定の reset に成功すること");

    let mut codes = Vec::new();
    while let Some(e) = server.poll_event() {
        if let SessionEvent::ResetDataStream { error_code, .. } = e {
            codes.push(error_code);
        }
    }
    assert_eq!(codes, vec![0xABCD]);
}

/// 未知の stream id を reset しようとするとエラーになる
#[test]
fn reset_unknown_stream_is_rejected() {
    let (_client, mut server, _rid) = establish_subscribe_track(804);
    let err = server
        .reset_outgoing_data_stream(DataStreamId(9999), DataStreamResetReason::Cancelled)
        .expect_err("未知の stream id は拒否される");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// Malformed Track の cancel / 終端イベントを回収して検証する
///
/// 返り値は (cancel イベント列, ResetDataStream 件数, RequestTerminated(MalformedTrack) 件数)。
/// cancel は `request_id` と `STREAM_MALFORMED_TRACK` も検証する。`expected_reset_stream_id` は
/// `ResetDataStream` の対象 stream (datagram 経路は `None`)。CloseSession はテストの前提に
/// 反するため panic する。
fn drain_malformed_events(
    session: &mut Session,
    request_id: u64,
    expected_reset_stream_id: Option<DataStreamId>,
) -> (Vec<&'static str>, usize, usize) {
    use shiguredo_moqt::error::STREAM_MALFORMED_TRACK;
    let mut cancels = Vec::new();
    let mut reset_data_streams = 0;
    let mut malformed_terminations = 0;
    while let Some(e) = session.poll_event() {
        match e {
            SessionEvent::StopSendingRequestStream {
                request_id: rid,
                error_code,
            } => {
                assert_eq!(rid, request_id, "cancel の対象 request id が一致すること");
                assert_eq!(
                    error_code, STREAM_MALFORMED_TRACK,
                    "cancel の error code が STREAM_MALFORMED_TRACK (0x12) であること"
                );
                cancels.push("stop_sending");
            }
            SessionEvent::ResetRequestStream {
                request_id: rid,
                error_code,
            } => {
                assert_eq!(rid, request_id, "cancel の対象 request id が一致すること");
                assert_eq!(
                    error_code, STREAM_MALFORMED_TRACK,
                    "cancel の error code が STREAM_MALFORMED_TRACK (0x12) であること"
                );
                cancels.push("reset");
            }
            SessionEvent::ResetDataStream { stream_id, .. } => {
                assert_eq!(
                    Some(stream_id),
                    expected_reset_stream_id,
                    "ResetDataStream の対象 stream_id が期待値と一致すること"
                );
                reset_data_streams += 1;
            }
            SessionEvent::RequestTerminated {
                request_id: rid,
                reason: TerminationReason::MalformedTrack { .. },
                ..
            } => {
                assert_eq!(rid, request_id, "終端対象 request id が一致すること");
                malformed_terminations += 1;
            }
            SessionEvent::CloseSession(err) => {
                panic!("CloseSession が発行された: {err:?}");
            }
            _ => {}
        }
    }
    (cancels, reset_data_streams, malformed_terminations)
}

// ─── 条件 1: 同一 Subgroup ID の Publisher Priority 不一致 ─────────────────

/// 同一 Subgroup キーの 2 本目の stream で Publisher Priority を変えると Malformed Track
///
/// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 1:
/// "An Object with a particular Subgroup ID is received, but its Publisher Priority is
/// different from that of the previous Object with the same Subgroup ID."
#[test]
fn condition1_priority_mismatch_terminates_subscription() {
    let alias = 810u64;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(rid, alias, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");

    // 1 本目: priority = 10
    let stream1 = DataStreamId(30);
    let header1 = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(5),
        publisher_priority: Some(10),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream1, 0x14)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream1, &header1)
        .expect("1 本目の header は受理される");
    client
        .recv_subgroup_object(
            stream1,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect("object の受信に成功すること");
    // STOP_SENDING で停止 → StoppedByPeer になり再オープン可能 (peer publisher 側は
    // Forward 0→1 まで再オープンを抑止すべきだが、受信側 tracker は SHOULD NOT 違反を
    // セッションエラーにせず StoppedByPeer からの再オープンを受理する)
    client
        .send_data_stream_stop_sending(stream1)
        .expect("STOP_SENDING に成功すること");

    // 2 本目: 同一 Subgroup キーだが priority = 99 (不一致)
    let stream2 = DataStreamId(31);
    let header2 = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(5),
        publisher_priority: Some(99),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream2, 0x14)
        .expect("stream type の通知に成功すること");
    let err = client
        .recv_subgroup_header(stream2, &header2)
        .expect_err("条件 1: priority 不一致は Malformed Track");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        client.state(),
        SessionState::Established,
        "セッションは閉じないこと"
    );
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated,
        "該当 subscription は Terminated になること"
    );
    // draft §12.1 MUST: subscription に対応する bidi request stream を cancel する
    let (cancels, reset_data_streams, malformed_terminations) =
        drain_malformed_events(&mut client, rid, Some(stream2));
    assert_eq!(
        cancels,
        vec!["stop_sending", "reset"],
        "受信方向 → 送信方向の順で STREAM_MALFORMED_TRACK の cancel を発行すること"
    );
    assert_eq!(
        reset_data_streams, 1,
        "malformed を運んだ stream を reset すること"
    );
    assert_eq!(
        malformed_terminations, 1,
        "RequestTerminated(MalformedTrack) を 1 件発行すること"
    );
}

// ─── 条件 6/7: 重複 Object のフィールド不一致 ─────────────────

/// 同一 (Group ID, Object ID) を subgroup stream と datagram の両方で受信すると
/// Forwarding Preference 不一致で Malformed Track (条件 7)
///
/// draft-ietf-moq-transport-21 §7.1 (Caching Relays):
/// "An endpoint that receives a duplicate Object with a different Forwarding Preference,
/// Subgroup ID, Priority or Payload MUST treat the track as Malformed."
#[test]
fn condition7_forwarding_preference_mismatch_terminates_subscription() {
    let alias = 820u64;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(rid, alias, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");

    // subgroup stream で (group=0, object=0) を受信
    let stream_id = DataStreamId(40);
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("header の受信に成功すること");
    client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect("object の受信に成功すること");

    // datagram で同一 (group=0, object=0) を受信 → Forwarding Preference 不一致
    let err = client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: alias,
            group_id: 0,
            object_id: 0,
            publisher_priority: Some(128),
            properties_data: None,
            end_of_group: false,
            status: None,
        })
        .expect_err("条件 7: Forwarding Preference 不一致は Malformed Track");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        client.state(),
        SessionState::Established,
        "セッションは閉じないこと"
    );
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated,
        "該当 subscription は Terminated になること"
    );
}

/// 同一 Track / 同一 Group 内で別々の Object を subgroup と datagram に分けても Malformed にならない
///
/// draft-ietf-moq-transport-21 §11.1.1 (Object Header):
/// "Object Forwarding Preference is a property of an individual Object and can vary among
/// Objects in the same Track."
/// draft-ietf-moq-transport-21 §11 (Data Streams and Datagrams):
/// "the Original Publisher MAY use both Subgroups and Datagrams within a Group or Track."
#[test]
fn different_objects_mixed_subgroup_and_datagram_is_not_malformed() {
    let alias = 821u64;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(rid, alias, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");

    // subgroup stream で (group=0, object=0) を受信
    let stream_id = DataStreamId(41);
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("header の受信に成功すること");
    client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect("object 0 の受信に成功すること");

    // datagram で (group=0, object=1) を受信 → 別 Object なので Malformed にならない
    let outcome = client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: alias,
            group_id: 0,
            object_id: 1,
            publisher_priority: Some(128),
            properties_data: None,
            end_of_group: false,
            status: None,
        })
        .expect("別 Object の datagram は受理されること");
    assert_eq!(outcome, TrackDataAcceptance::Accepted);
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Established,
        "subscription は Established のまま"
    );
}

/// 同一 (Group ID, Object ID) を同一経路 (datagram) で 2 回受信し Priority が異なると
/// Malformed Track (条件 6)
#[test]
fn condition6_priority_mismatch_on_duplicate_datagram_terminates_subscription() {
    let alias = 822u64;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(rid, alias, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");

    // 1 回目: priority = 100
    let outcome = client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: alias,
            group_id: 1,
            object_id: 5,
            publisher_priority: Some(100),
            properties_data: None,
            end_of_group: false,
            status: None,
        })
        .expect("1 回目の datagram は受理されること");
    assert_eq!(outcome, TrackDataAcceptance::Accepted);

    // 2 回目: 同一 (group=1, object=5) だが priority = 200 (不一致)
    let err = client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: alias,
            group_id: 1,
            object_id: 5,
            publisher_priority: Some(200),
            properties_data: None,
            end_of_group: false,
            status: None,
        })
        .expect_err("条件 6: Priority 不一致は Malformed Track");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        client.state(),
        SessionState::Established,
        "セッションは閉じないこと"
    );
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated,
        "該当 subscription は Terminated になること"
    );
    // draft §12.1 MUST: datagram 経路 (stream_id なし) でも bidi request stream を cancel する
    let (cancels, reset_data_streams, malformed_terminations) =
        drain_malformed_events(&mut client, rid, None);
    assert_eq!(
        cancels,
        vec!["stop_sending", "reset"],
        "受信方向 → 送信方向の順で STREAM_MALFORMED_TRACK の cancel を発行すること"
    );
    assert_eq!(
        reset_data_streams, 0,
        "datagram 経路では ResetDataStream を発行しないこと"
    );
    assert_eq!(
        malformed_terminations, 1,
        "RequestTerminated(MalformedTrack) を 1 件発行すること"
    );
}

/// 同一 (Group ID, Object ID) を同一 priority で 2 回受信しても Malformed にならない (正当な重複)
#[test]
fn duplicate_object_with_same_fields_is_not_malformed() {
    let alias = 823u64;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(rid, alias, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");

    // 同一フィールドで 2 回受信 → 正当な重複 (relay の再送等)
    for _ in 0..2 {
        let outcome = client
            .recv_object_datagram(&ObjectDatagram {
                track_alias: alias,
                group_id: 2,
                object_id: 3,
                publisher_priority: Some(50),
                properties_data: None,
                end_of_group: false,
                status: None,
            })
            .expect("同一フィールドの重複は受理されること");
        assert_eq!(outcome, TrackDataAcceptance::Accepted);
    }
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Established,
        "subscription は Established のまま"
    );
}

// ─── datagram の OBJECT_DELIVERY_TIMEOUT 判定 (draft §5.2) ────────────────────

/// Track Property に OBJECT_DELIVERY_TIMEOUT=500 を持つ publisher subscription を確立する
///
/// SUBSCRIBE_OK の Track Properties に OBJECT_DELIVERY_TIMEOUT=500 を載せることで、
/// publisher (server) 側の effective_object_ms を 500 に設定する。
fn establish_with_track_delivery_timeout(alias: u64) -> (Session, Session, u64) {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            rid,
            alias,
            MessageParameters::new(),
            track_properties_with_delivery_timeout(500),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server, rid)
}

/// tick 前に送信した datagram が最初の tick 後の再送で誤 drop されない
///
/// draft-ietf-moq-transport-21 §5.2 (Delivery Timeouts and Data Reliability): "the MOQT
/// implementation MUST retain the time at which the last header byte of every object
/// has been either received from the upstream subscription, or provided by the original
/// publisher application."
/// sans-I/O の Session は tick まで時刻を知らないため、object header 提供完了時刻は最初の tick で
/// 確定する (提供時刻の近似)。tick 未到達のエントリを 0 として記録すると、最初の tick が
/// timeout 以上 (例: 500ms に対し 60,000ms) の場合に経過時間を過大評価して誤 drop する。
/// この近似は経過時間を過小評価する方向 (誤 drop は起きない代わりに、真の timeout 超過
/// datagram の drop 判定が最大 1 tick 遅れる)。
#[test]
fn datagram_sent_before_first_tick_is_not_dropped_after_tick() {
    let (_client, mut server, rid) = establish_with_track_delivery_timeout(800);
    // tick を一切呼ばずに datagram を送信する (object header 提供完了時刻は未確定)
    server
        .send_object_datagram(rid, 0, 0, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    // 最初の tick が timeout (500ms) を大きく超える 60,000ms で到達する
    server.tick(60_000);
    // 再送は経過時間 0 と判定され、誤 drop されない
    server
        .send_object_datagram(rid, 0, 0, None, None)
        .expect("tick 前送信の datagram が最初の tick 後に誤 drop されないこと");
    assert_eq!(server.state(), SessionState::Established);
    // object header 提供完了時刻は最初の tick で確定したまま継続して経過時間が評価される
    // (成功送信でも object header 提供完了時刻はリセットされない)
    server.tick(60_500);
    let err = server
        .send_object_datagram(rid, 0, 0, None, None)
        .expect_err("確定後の経過 500ms >= 500ms で drop されること");
    assert!(matches!(err, SendRequestError::LocalDatagramTimeout));
}

/// tick 後に送信した datagram は timeout 超過で drop され、drop 後の再送も再度 drop される
///
/// draft-ietf-moq-transport-21 §5.2 (Delivery Timeouts and Data Reliability): "For datagrams,
/// the implementation MUST drop the datagrams if the time elapsed
/// exceeds OBJECT_DELIVERY_TIMEOUT." 起点は object header の最終バイト。
/// drop 時はエントリを削除しないため、同じオブジェクトの
/// 再送は subscription の forget まで再び drop され続ける。drop 時は送信されないため、
/// publisher 側の最大位置も更新されない。
#[test]
fn datagram_sent_after_tick_is_dropped_after_timeout() {
    let (_client, mut server, rid) = establish_with_track_delivery_timeout(801);
    server.tick(100);
    server
        .send_object_datagram(rid, 0, 0, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    // 経過 500ms >= 500ms (境界): 再送は drop される
    server.tick(600);
    let err = server
        .send_object_datagram(rid, 0, 0, None, None)
        .unwrap_err();
    assert!(matches!(err, SendRequestError::LocalDatagramTimeout));
    // drop 後もエントリが残るため、再送は再度 drop される
    let err = server
        .send_object_datagram(rid, 0, 0, None, None)
        .unwrap_err();
    assert!(matches!(err, SendRequestError::LocalDatagramTimeout));
    // drop 時は送信されないため、最大位置は初回送信時のまま更新されない
    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        sub.largest_received_location,
        Some(Location {
            group_id: 0,
            object_id: 0,
        }),
        "drop 時は最大位置が更新されないこと"
    );
    assert_eq!(server.state(), SessionState::Established);
}

// ─── Subgroup 単位 priority と重複 Object 検証 (draft-ietf-moq-transport-21 §12.1 条件 1 / §7.1 (Caching Relays)) ─────

/// FirstObjectId の先頭 Object 解決時、Subgroup 単位で記録された priority と同じ priority で
/// subgroup を再オープンしても Malformed にならない
///
/// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 1 は Subgroup 単位の
/// Publisher Priority の一致を求める。購読単位の直近値は並行 Subgroup の header で上書きされる。
#[test]
fn first_object_id_subgroup_reopen_with_same_priority_is_not_malformed() {
    let alias = 830u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);

    // stream A: FirstObjectId / priority 10 (先頭 Object はまだ処理しない)
    let stream_a = DataStreamId(40);
    let header_a = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(10),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_a, 0x12)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_a, &header_a)
        .expect("1 本目の header は受理される");

    // stream B: FirstObjectId / priority 99 → 購読単位の直近値が 99 に上書きされる
    let stream_b = DataStreamId(41);
    let header_b = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(99),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_b, 0x12)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_b, &header_b)
        .expect("2 本目の header は受理される");

    // stream A の先頭 Object (ID 5) → subgroup 5 の priority は stream A の 10 で記録される
    client
        .recv_subgroup_object(
            stream_a,
            &DecodedSubgroupObject {
                object_id: 5,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect("先頭 Object の受信に成功すること");
    // STOP_SENDING で停止 → StoppedByPeer になり subgroup 5 を再オープン可能にする
    client
        .send_data_stream_stop_sending(stream_a)
        .expect("STOP_SENDING に成功すること");

    // subgroup 5 を priority 10 で再オープンしても Malformed にならない
    let stream_c = DataStreamId(42);
    let header_c = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(10),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_c, 0x12)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_c, &header_c)
        .expect("再オープンの header は受理される");
    client
        .recv_subgroup_object(
            stream_c,
            &DecodedSubgroupObject {
                object_id: 5,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect("Subgroup 単位の priority が一致するため Malformed にならない");
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Established
    );
}

/// FirstObjectId の先頭 Object 解決時、Subgroup 単位で記録された priority と異なる priority で
/// subgroup を再オープンすると §12.1 条件 1 の Malformed として subscription が終端される
#[test]
fn first_object_id_subgroup_reopen_with_different_priority_terminates_subscription() {
    let alias = 831u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);

    // stream A: FirstObjectId / priority 10 (先頭 Object はまだ処理しない)
    let stream_a = DataStreamId(50);
    let header_a = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(10),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_a, 0x12)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_a, &header_a)
        .expect("1 本目の header は受理される");

    // stream B: FirstObjectId / priority 99 → 購読単位の直近値が 99 に上書きされる
    let stream_b = DataStreamId(51);
    let header_b = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(99),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_b, 0x12)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_b, &header_b)
        .expect("2 本目の header は受理される");

    // stream A の先頭 Object (ID 5) → subgroup 5 の priority は stream A の 10 で記録される
    client
        .recv_subgroup_object(
            stream_a,
            &DecodedSubgroupObject {
                object_id: 5,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect("先頭 Object の受信に成功すること");
    // STOP_SENDING で停止 → StoppedByPeer になり subgroup 5 を再オープン可能にする
    client
        .send_data_stream_stop_sending(stream_a)
        .expect("STOP_SENDING に成功すること");

    // subgroup 5 を priority 99 で再オープンすると §12.1 条件 1 の Malformed
    // (購読単位の直近値 99 を記録する旧実装では一致して検出漏れになる)
    let stream_c = DataStreamId(52);
    let header_c = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(99),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_c, 0x12)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_c, &header_c)
        .expect("再オープンの header は受理される");
    let err = client
        .recv_subgroup_object(
            stream_c,
            &DecodedSubgroupObject {
                object_id: 5,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("Subgroup 単位の priority 不一致は Malformed Track");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        client.state(),
        SessionState::Established,
        "セッションは閉じないこと"
    );
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated,
        "該当 subscription は Terminated になること"
    );
    // draft §12.1 MUST: 先頭 Object 解決経路でも bidi request stream を cancel する
    let (cancels, reset_data_streams, malformed_terminations) =
        drain_malformed_events(&mut client, rid, Some(stream_c));
    assert_eq!(
        cancels,
        vec!["stop_sending", "reset"],
        "受信方向 → 送信方向の順で STREAM_MALFORMED_TRACK の cancel を発行すること"
    );
    assert_eq!(
        reset_data_streams, 1,
        "malformed を運んだ stream を reset すること"
    );
    assert_eq!(
        malformed_terminations, 1,
        "RequestTerminated(MalformedTrack) を 1 件発行すること"
    );
}

/// 同一 (group, object) を異なる Subgroup ID で受信すると §7.1 (Caching Relays) の
/// Malformed として subscription が終端される
///
/// `observe_object_fields` は Forwarding Preference → Subgroup ID → Priority の順に比較する。
#[test]
fn duplicate_object_with_different_subgroup_id_terminates_subscription() {
    let alias = 832u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);

    // stream A: FirstObjectId / priority 10 (先頭 Object はまだ処理しない)
    let stream_a = DataStreamId(60);
    let header_a = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(10),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_a, 0x12)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_a, &header_a)
        .expect("1 本目の header は受理される");

    // stream B: FirstObjectId / priority 99 → 購読単位の直近値が 99 に上書きされる
    let stream_b = DataStreamId(61);
    let header_b = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(99),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_b, 0x12)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_b, &header_b)
        .expect("2 本目の header は受理される");

    // stream A の先頭 Object (ID 0) → (group 0, object 0) は priority 10 で記録される
    client
        .recv_subgroup_object(
            stream_a,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect("先頭 Object の受信に成功すること");

    // stream C: Explicit(1) / priority 99 で同一 Object (ID 0) を受信 → Subgroup ID 不一致で
    // §7.1 の Malformed
    let stream_c = DataStreamId(62);
    let header_c = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(1),
        publisher_priority: Some(99),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_c, 0x14)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_c, &header_c)
        .expect("3 本目の header は受理される");
    let err = client
        .recv_subgroup_object(
            stream_c,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("重複 Object の Subgroup ID 不一致は Malformed Track");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        err.reason,
        "malformed track: duplicate Object with different Subgroup ID"
    );
    assert_eq!(
        client.state(),
        SessionState::Established,
        "セッションは閉じないこと"
    );
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated,
        "該当 subscription は Terminated になること"
    );
}

/// 重複 Object の priority 検証にも stream の Subgroup 単位 priority が使われる
///
/// 購読単位の直近値 (stream T の 99) を重複 Object 検証に使うと、stream S の同一 Object を
/// 再受信したときに 10 と 99 の不一致で Malformed を誤検出する
/// (draft-ietf-moq-transport-21 §7.1 (Caching Relays))。
#[test]
fn duplicate_object_priority_check_uses_stream_subgroup_priority() {
    let alias = 833u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);

    // stream S: Explicit(0) / priority 10 で (0, 0) を受理する
    let stream_s = DataStreamId(70);
    let header_s = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(10),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_s, 0x14)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_s, &header_s)
        .expect("header の受信に成功すること");
    client
        .recv_subgroup_object(
            stream_s,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect("object の受信に成功すること");

    // stream T: Explicit(1) / priority 99 → 購読単位の直近値が 99 に上書きされる
    let stream_t = DataStreamId(71);
    let header_t = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(1),
        publisher_priority: Some(99),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_t, 0x14)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_t, &header_t)
        .expect("header の受信に成功すること");

    // stream S の同一 Object を再受信しても、Subgroup 単位の priority が一致するため受理される
    client
        .recv_subgroup_object(
            stream_s,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect("Subgroup 単位の priority が一致するため Malformed にならない");
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Established
    );
}

// ─── STOP_SENDING 後の Subgroup 再オープン (draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams)) ─────

/// FORWARD=0 の SUBSCRIBE を確立し `(client, server, rid)` を返す
fn establish_forward_0_subscription(alias: u64) -> (Session, Session, u64) {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, mut server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), params)
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(rid, alias, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");
    (client, server, rid)
}

/// REQUEST_UPDATE を送受信し、publisher に REQUEST_OK を返させて Forward State を 1 にする
fn update_forward_to_1(client: &mut Session, server: &mut Session, rid: u64) {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    client
        .send_request_update(rid, params)
        .expect("REQUEST_UPDATE の送信に成功すること");
    let (_, upd_msg) = take_send_on_stream(client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("REQUEST_UPDATE の受信に成功すること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("REQUEST_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("REQUEST_OK の受信に成功すること");
    assert_eq!(
        server
            .subscription(rid)
            .expect("subscription が存在する")
            .forward_state,
        1
    );
}

/// STOP_SENDING を受けた Subgroup の再オープンは Forward 0→1 の REQUEST_UPDATE 受理後のみ可能
#[test]
fn stop_sending_requires_forward_0_to_1_before_reopen() {
    let alias = 840u64;
    let (mut client, mut server, rid) = establish_forward_0_subscription(alias);
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 5,
        subgroup_id: SubgroupIdMode::Explicit(3),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(60), rid, &header)
        .expect("header の送信に成功すること");
    server
        .recv_data_stream_stop_sending(DataStreamId(60))
        .expect("STOP_SENDING の通知に成功すること");

    // Forward 0→1 前の再オープンは拒否され、セッションは閉じない
    let err = server
        .send_subgroup_header(DataStreamId(61), rid, &header)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        server.state(),
        SessionState::Established,
        "セッションは閉じないこと"
    );

    // FORWARD を含まない REQUEST_UPDATE の受理では解除されない
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("REQUEST_UPDATE の送信に成功すること");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("REQUEST_UPDATE の受信に成功すること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("REQUEST_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("REQUEST_OK の受信に成功すること");
    let err = server
        .send_subgroup_header(DataStreamId(61), rid, &header)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);

    // 停止エントリは Forward 0→1 の REQUEST_OK 適用まで残る。解除は responder 専用 API の
    // `send_ok_for_subscription` が行うため、ここでは受信のみで OK を送らない段階では
    // 再オープンできないことも確認する
    let mut forward_params = MessageParameters::new();
    forward_params.push({
        use shiguredo_moqt::message_parameter::{
            MessageParameter, MessageParameterValue, PARAM_FORWARD,
        };
        MessageParameter {
            param_type: PARAM_FORWARD,
            value: MessageParameterValue::Uint8(1),
        }
    });
    client
        .send_request_update(rid, forward_params)
        .expect("REQUEST_UPDATE の送信に成功すること");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("REQUEST_UPDATE の受信に成功すること");
    let err = server
        .send_subgroup_header(DataStreamId(61), rid, &header)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);

    // Forward 0→1 の REQUEST_OK を適用すると再オープンできる
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("REQUEST_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("REQUEST_OK の受信に成功すること");
    server
        .send_subgroup_header(DataStreamId(61), rid, &header)
        .expect("Forward 0→1 後は再オープンできること");
}

/// 停止時の Forward State が 1 の場合は 0→1 遷移がないため再オープンできない
#[test]
fn stop_sending_with_forward_1_blocks_reopen_without_transition() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let alias = 841u64;
    let (mut client, mut server, rid) = establish_subscribe_track(alias);
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 5,
        subgroup_id: SubgroupIdMode::Explicit(3),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(62), rid, &header)
        .expect("header の送信に成功すること");
    server
        .recv_data_stream_stop_sending(DataStreamId(62))
        .expect("STOP_SENDING の通知に成功すること");

    // 既定の Forward State 1 のままで 0→1 遷移がないため再オープン不可
    let err = server
        .send_subgroup_header(DataStreamId(63), rid, &header)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);

    // Forward 1 のまま FORWARD=1 の REQUEST_UPDATE を受理しても解除されない (0→1 遷移なし)
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    client
        .send_request_update(rid, params)
        .expect("REQUEST_UPDATE の送信に成功すること");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("REQUEST_UPDATE の受信に成功すること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("REQUEST_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("REQUEST_OK の受信に成功すること");
    let err = server
        .send_subgroup_header(DataStreamId(63), rid, &header)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Established);

    // 停止していない別 Subgroup は影響を受けない (キー精度の確認)
    let other_header = SubgroupHeader {
        track_alias: alias,
        group_id: 5,
        subgroup_id: SubgroupIdMode::Explicit(4),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(63), rid, &other_header)
        .expect("停止していない Subgroup は開けること");

    // 別 Group の同一 Subgroup ID も開ける (キーの group_id 次元の確認)
    let other_group_header = SubgroupHeader {
        track_alias: alias,
        group_id: 6,
        subgroup_id: SubgroupIdMode::Explicit(3),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(64), rid, &other_group_header)
        .expect("停止していない Group は開けること");
}

/// STOP_SENDING 後に reset で終端しても再オープン禁止は残り、Forward 0→1 で解除される
#[test]
fn stop_sending_then_reset_still_blocks_reopen_until_forward_0_to_1() {
    let alias = 842u64;
    let (mut client, mut server, rid) = establish_forward_0_subscription(alias);
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 5,
        subgroup_id: SubgroupIdMode::Explicit(3),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(64), rid, &header)
        .expect("header の送信に成功すること");
    server
        .recv_data_stream_stop_sending(DataStreamId(64))
        .expect("STOP_SENDING の通知に成功すること");
    // §11.3.2 の SHOULD に従って reset しても STOP_SENDING の再オープン禁止は残る
    server
        .send_data_stream_closed(
            DataStreamId(64),
            RequestStreamEnd::Reset {
                error_code: 0,
                reliable_size: None,
            },
        )
        .expect("reset に成功すること");
    let err = server
        .send_subgroup_header(DataStreamId(65), rid, &header)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);

    update_forward_to_1(&mut client, &mut server, rid);
    server
        .send_subgroup_header(DataStreamId(65), rid, &header)
        .expect("Forward 0→1 後は再オープンできること");
}

/// FirstObjectId の subgroup_id 解決経路でも Forward 0→1 まで再オープンできない
#[test]
fn first_object_id_stop_sending_blocks_resolution_until_forward_0_to_1() {
    let alias = 843u64;
    let (mut client, mut server, rid) = establish_forward_0_subscription(alias);
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 5,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(66), rid, &header)
        .expect("header の送信に成功すること");
    server
        .recv_data_stream_stop_sending(DataStreamId(66))
        .expect("STOP_SENDING の通知に成功すること");

    // 先頭 Object による subgroup_id 解決経路が拒否され、セッションは閉じない
    let err = server
        .send_subgroup_object(DataStreamId(66), 0, None)
        .unwrap_err();
    assert_eq!(
        err.as_session_error().map(|e| e.code),
        Some(SESSION_PROTOCOL_VIOLATION)
    );
    assert_eq!(server.state(), SessionState::Established);

    update_forward_to_1(&mut client, &mut server, rid);
    server
        .send_subgroup_object(DataStreamId(66), 0, None)
        .expect("Forward 0→1 後は subgroup_id を解決できること");
}

/// alias を共有する別 subscription の Forward 0→1 では再オープン禁止が解除されない
#[test]
fn shared_alias_forward_0_to_1_does_not_unlock_other_subscription() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    const ALIAS: u64 = 844;
    let (mut client, mut server) = establish_pair();
    let mut sub_params = MessageParameters::new();
    sub_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    let rid1 = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), sub_params.clone())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg1) = take_send_request(&mut client);
    server
        .recv_request(sub_msg1)
        .expect("SUBSCRIBE の受信に成功すること");
    let rid2 = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), sub_params)
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg2) = take_send_request(&mut client);
    server
        .recv_request(sub_msg2)
        .expect("SUBSCRIBE の受信に成功すること");
    // 同一 Track の 2 subscription に同じ alias を割り当てる
    for sub_rid in [rid1, rid2] {
        server
            .send_subscribe_ok(
                sub_rid,
                ALIAS,
                MessageParameters::new(),
                TrackProperties::new(),
            )
            .expect("SUBSCRIBE_OK の送信に成功すること");
        let (_, ok_msg) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(sub_rid, ok_msg)
            .expect("SUBSCRIBE_OK の受信に成功すること");
    }

    // rid1 の Subgroup を開いて STOP_SENDING を受ける
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 5,
        subgroup_id: SubgroupIdMode::Explicit(3),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(80), rid1, &header)
        .expect("header の送信に成功すること");
    server
        .recv_data_stream_stop_sending(DataStreamId(80))
        .expect("STOP_SENDING の通知に成功すること");

    // rid2 の Forward 0→1 を受理しても rid1 の禁止は解除されない
    update_forward_to_1(&mut client, &mut server, rid2);
    let err = server
        .send_subgroup_header(DataStreamId(81), rid1, &header)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);

    // alias を共有する rid2 経由でも同一 Subgroup は再オープンできない
    let err = server
        .send_subgroup_header(DataStreamId(82), rid2, &header)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);

    // rid1 の Forward 0→1 で解除される
    update_forward_to_1(&mut client, &mut server, rid1);
    server
        .send_subgroup_header(DataStreamId(81), rid1, &header)
        .expect("rid1 の Forward 0→1 後は再オープンできること");
}

/// 解決済み Subgroup の STOP_SENDING は別の未解決 FirstObjectId stream の同一 ID 解決も拒否する
#[test]
fn first_object_id_resolved_stopped_subgroup_blocks_other_stream_resolution() {
    let alias = 846u64;
    let (_client, mut server, rid) = establish_subscribe_track(alias);
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 5,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    // stream A で Object ID 3 を送り、subgroup 3 を解決する
    server
        .send_subgroup_header(DataStreamId(67), rid, &header)
        .expect("header の送信に成功すること");
    server
        .send_subgroup_object(DataStreamId(67), 3, None)
        .expect("subgroup_id の解決に成功すること");
    server
        .recv_data_stream_stop_sending(DataStreamId(67))
        .expect("STOP_SENDING の通知に成功すること");

    // stream B (未解決 FirstObjectId) で同じ Object ID 3 を送ると subgroup 3 の停止に当たる
    server
        .send_subgroup_header(DataStreamId(68), rid, &header)
        .expect("header の送信に成功すること");
    let err = server
        .send_subgroup_object(DataStreamId(68), 3, None)
        .unwrap_err();
    assert_eq!(
        err.as_session_error().map(|e| e.code),
        Some(SESSION_PROTOCOL_VIOLATION)
    );
    assert_eq!(server.state(), SessionState::Established);

    // 停止していない Object ID 4 は解決できる
    server
        .send_subgroup_object(DataStreamId(68), 4, None)
        .expect("停止していない Subgroup は解決できること");
}

/// 停止の再オープン禁止は track_alias 単位であり、別 alias の同一 Subgroup には影響しない
#[test]
fn stop_sending_does_not_block_same_subgroup_on_different_alias() {
    let (mut client, mut server) = establish_pair();
    let rid1 = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg1) = take_send_request(&mut client);
    server
        .recv_request(sub_msg1)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(rid1, 910, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg1) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid1, ok_msg1)
        .expect("SUBSCRIBE_OK の受信に成功すること");
    let rid2 = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg2) = take_send_request(&mut client);
    server
        .recv_request(sub_msg2)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(rid2, 911, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg2) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid2, ok_msg2)
        .expect("SUBSCRIBE_OK の受信に成功すること");

    // alias 910 の (group 5, subgroup 3) を開いて STOP_SENDING を受ける
    let header1 = SubgroupHeader {
        track_alias: 910,
        group_id: 5,
        subgroup_id: SubgroupIdMode::Explicit(3),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(100), rid1, &header1)
        .expect("header の送信に成功すること");
    server
        .recv_data_stream_stop_sending(DataStreamId(100))
        .expect("STOP_SENDING の通知に成功すること");
    let err = server
        .send_subgroup_header(DataStreamId(101), rid1, &header1)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);

    // 別 alias 911 の同一 (group 5, subgroup 3) は停止の影響を受けない
    let header2 = SubgroupHeader {
        track_alias: 911,
        group_id: 5,
        subgroup_id: SubgroupIdMode::Explicit(3),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(102), rid2, &header2)
        .expect("別 alias の同一 Subgroup は開けること");
}

/// forget_subscription で停止エントリが破棄され、alias 再利用後の同一 Subgroup を開ける
#[test]
fn forget_subscription_discards_stopped_subgroups() {
    let alias = 920u64;
    let (mut client, mut server, rid) = establish_subscribe_track(alias);
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 5,
        subgroup_id: SubgroupIdMode::Explicit(3),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(110), rid, &header)
        .expect("header の送信に成功すること");
    server
        .recv_data_stream_stop_sending(DataStreamId(110))
        .expect("STOP_SENDING の通知に成功すること");

    // subscriber (client) が cancel し、publisher (server) が stream 終端で Terminated → forget する
    client
        .stop_sending(rid)
        .expect("stop_sending に成功すること");
    server
        .recv_request_stream_closed(
            rid,
            RequestStreamEnd::Reset {
                error_code: 0,
                reliable_size: None,
            },
        )
        .expect("request stream 終端の通知に成功すること");
    server
        .forget_subscription(rid)
        .expect("cleanup_ready な subscription を破棄できること");

    // 新規 SUBSCRIBE に同じ alias を割り当て、同一 Subgroup を開ける
    let rid2 = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg2) = take_send_request(&mut client);
    server
        .recv_request(sub_msg2)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(
            rid2,
            alias,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg2) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid2, ok_msg2)
        .expect("SUBSCRIBE_OK の受信に成功すること");
    server
        .send_subgroup_header(DataStreamId(111), rid2, &header)
        .expect("forget 済みの停止エントリに妨げられないこと");
}
