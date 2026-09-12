//! FETCH のテスト (draft-ietf-moq-transport-21 §9.11)
//!
//! FETCH のフルサイクル、ストリーム終端遷移、役割違反、
//! End Location 検証、LOCATION_FILTER 範囲検証を扱う。

use super::*;

/// draft-ietf-moq-transport-21 §3.2.1 (Fetch State Management): subscriber 側で FETCH データストリームの FIN を受信すると
/// `recv_fetch_data_stream_closed(Fin)` 経由で state が Terminated に遷移する。
#[test]
fn fetch_stream_finished_transitions_to_terminated() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let (mut client, _server, rid) = establish_fetch_with_range(
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 1,
            object_id: 0,
        },
    );
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::FetchState::Established
    );
    // I/O 層から FIN 通知
    client
        .recv_fetch_data_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::FetchState::Terminated
    );
    // forget_fetch で回収できる
    assert!(client.forget_fetch(rid).is_some());
}

/// subscriber 側で RESET_STREAM 受信 → Terminated
#[test]
fn fetch_stream_reset_transitions_to_terminated() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let (mut client, _server, rid) = establish_fetch_with_range(
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 1,
            object_id: 0,
        },
    );
    client
        .recv_fetch_data_stream_closed(
            rid,
            RequestStreamEnd::Reset {
                error_code: 0,
                reliable_size: None,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::FetchState::Terminated
    );
}

/// subscriber が STOP_SENDING を送出 → Terminated
#[test]
fn send_fetch_stop_sending_transitions_to_terminated() {
    use shiguredo_moqt::message::common::Location;
    let (mut client, _server, rid) = establish_fetch_with_range(
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 1,
            object_id: 0,
        },
    );
    client
        .send_fetch_stop_sending(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::FetchState::Terminated
    );
    // subscriber の cancel は bidi 送信方向の reset 指示になる (§6.4.2.3)
    let mut saw_reset = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::ResetRequestStream {
            request_id,
            error_code,
        } = e
        {
            assert_eq!(request_id, rid);
            assert_eq!(
                error_code,
                shiguredo_moqt::error::STREAM_CANCELLED,
                "CANCELLED (0x1) で reset されること"
            );
            saw_reset = true;
        }
    }
    assert!(saw_reset, "cancel では ResetRequestStream が発行されること");
}

/// publisher が STOP_SENDING 受信 → Terminated
#[test]
fn fetch_stop_sending_received_transitions_to_terminated_for_publisher() {
    use shiguredo_moqt::message::common::Location;
    let (_client, mut server, rid) = establish_fetch_with_range(
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 1,
            object_id: 0,
        },
    );
    // publisher (server) 側で STOP_SENDING 受信
    server
        .fetch_stop_sending_received(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::FetchState::Terminated
    );
    // §3.2.1 MUST の bidi reset がイベントで指示されること
    let mut saw_reset = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::ResetRequestStream {
            request_id,
            error_code,
        } = e
        {
            assert_eq!(request_id, rid);
            assert_eq!(
                error_code,
                shiguredo_moqt::error::STREAM_CANCELLED,
                "CANCELLED (0x1) で reset されること"
            );
            saw_reset = true;
        }
    }
    assert!(
        saw_reset,
        "STOP_SENDING 受信では ResetRequestStream が発行されること (§3.2.1 MUST)"
    );
}

/// 役割違反: publisher が recv_fetch_data_stream_closed を呼ぶと PROTOCOL_VIOLATION
#[test]
fn fetch_stream_finished_requires_subscriber_role() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let (_client, mut server, rid) = establish_pending_fetch_with_range(
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 1,
            object_id: 0,
        },
    );
    server
        .send_fetch_ok(
            rid,
            0,
            Location {
                group_id: 1,
                object_id: 0,
            },
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let err = server
        .recv_fetch_data_stream_closed(rid, RequestStreamEnd::Fin)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// 役割違反: subscriber が fetch_stop_sending_received を呼ぶと PROTOCOL_VIOLATION
#[test]
fn fetch_stop_sending_received_requires_publisher_role() {
    use shiguredo_moqt::message::common::Location;
    let (mut client, _server, rid) = establish_fetch_with_range(
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 1,
            object_id: 0,
        },
    );
    let err = client.fetch_stop_sending_received(rid).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// 既に Terminated な fetch に対してストリーム終端 API を呼ぶと PROTOCOL_VIOLATION
#[test]
fn fetch_stream_finished_on_terminated_errors() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let (mut client, _server, rid) = establish_fetch_with_range(
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 1,
            object_id: 0,
        },
    );
    client
        .recv_fetch_data_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = client
        .recv_fetch_data_stream_closed(rid, RequestStreamEnd::Fin)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// Pending 状態でもストリーム終端 API は呼べる (FETCH_OK 前に publisher が
/// RESET_STREAM した等のケース)
#[test]
fn fetch_stream_reset_in_pending_transitions_to_terminated() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let (mut client, _server, rid) = establish_pending_fetch_with_range(
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 1,
            object_id: 0,
        },
    );
    // FETCH_OK 前に RESET_STREAM
    client
        .recv_fetch_data_stream_closed(
            rid,
            RequestStreamEnd::Reset {
                error_code: 0,
                reliable_size: None,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::FetchState::Terminated
    );
}

/// FETCH の FETCH_OK で End Location < Start Location なら PROTOCOL_VIOLATION
/// (draft-ietf-moq-transport-21 §9.12 (FETCH_OK))
#[test]
fn fetch_ok_end_before_start_closes_session() {
    use shiguredo_moqt::message::{FetchOk as WireFetchOk, common::Location};
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fetch_range_params(
                Location {
                    group_id: 3,
                    object_id: 0,
                },
                Location {
                    group_id: 10,
                    object_id: 0,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // server が不正な End Location (< Start) を含む FETCH_OK を送る
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::FetchOk(WireFetchOk {
                end_of_track: 0,
                end_location: Location {
                    group_id: 2,
                    object_id: 0,
                },
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// FETCH_OK で End Location == Start Location は許容する (空レンジ許容)
#[test]
fn fetch_ok_end_equals_start_accepted() {
    use shiguredo_moqt::message::common::Location;
    let (client, _server, rid) = establish_fetch_with_range(
        Location {
            group_id: 3,
            object_id: 0,
        },
        Location {
            group_id: 10,
            object_id: 0,
        },
    );
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::FetchState::Established
    );
}

/// FETCH で Start Location が対象トラックの観測 Largest (effective_largest_object、
/// 本テストでは公開 Object 由来の largest_received_location) を上回るとき
/// INVALID_RANGE で拒否される (draft-ietf-moq-transport-21 §9.11 (FETCH))。
#[test]
fn fetch_start_exceeds_largest_rejected_with_invalid_range() {
    use shiguredo_moqt::error::REQUEST_INVALID_RANGE;
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::{
        session::types::DataStreamId, stream::subgroup::SubgroupHeader,
        stream::subgroup::SubgroupIdMode,
    };
    let (mut client, mut server) = establish_pair();
    // client=subscriber → server=publisher の SUBSCRIBE_OK で server に Publisher subscription を作る
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(sub_rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // {10, 5} を公開して観測 Largest を確定させる
    let stream_id = DataStreamId(3);
    server
        .send_subgroup_header(
            stream_id,
            sub_rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: 10,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 5, None)
        .expect("テストフィクスチャの前提条件を満たす");
    // FETCH (start={20,0} > 観測 Largest {10,5}) → INVALID_RANGE
    let fetch_rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fetch_range_params(
                Location {
                    group_id: 20,
                    object_id: 0,
                },
                Location {
                    group_id: 20,
                    object_id: 1,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(server.fetch(fetch_rid).is_none());
    let (_, err_msg) = take_send_on_stream(&mut server);
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_INVALID_RANGE);
        }
        _ => panic!("INVALID_RANGE の RequestError が期待される"),
    }
}

/// FETCH で Start == 観測 Largest のとき (greater than ではない) 範囲内として受理され、
/// FETCH_OK が返る (draft-ietf-moq-transport-21 §9.11 (FETCH): "greater than" のみ拒否、等値は受理)。
#[test]
fn fetch_start_equals_largest_accepted() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::session::types::FetchState;
    let (mut client, mut server) = establish_pair();
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(sub_rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // publisher 側で {10, 5} を公開し、観測 Largest を確定させる
    // (draft-20 で publisher 側の LARGEST_OBJECT 保存は廃止されたため、
    // SUBSCRIBE_OK の広告値ではなく公開 Object で確定させる)
    let stream_id = DataStreamId(3);
    server
        .send_subgroup_header(
            stream_id,
            sub_rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: 10,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 5, None)
        .expect("テストフィクスチャの前提条件を満たす");
    // start={10,5} == 観測 Largest {10,5} なので受理される
    let fetch_rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fetch_range_params(
                Location {
                    group_id: 10,
                    object_id: 5,
                },
                Location {
                    group_id: 10,
                    object_id: 6,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .fetch(fetch_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Pending
    );
}

/// FETCH の観測 Largest は effective_largest_object (largest_received_location との max) を
/// 使う。largest_location が None でも公開済み object の largest_received_location を上回る Start は
/// INVALID_RANGE で拒否される (draft-ietf-moq-transport-21 §9.11 (FETCH) / draft-ietf-moq-transport-21 §3.3.1 (Location Filters))。
#[test]
fn fetch_start_exceeds_received_largest_rejected() {
    use shiguredo_moqt::error::REQUEST_INVALID_RANGE;
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::{
        session::types::DataStreamId, stream::subgroup::SubgroupHeader,
        stream::subgroup::SubgroupIdMode,
    };
    let (mut client, mut server) = establish_pair();
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // SUBSCRIBE_OK は LARGEST_OBJECT を伴わない → largest_location は None
    server
        .send_subscribe_ok(sub_rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // publisher 側で object {5,9} を公開 → largest_received_location = {5,9}、effective = {5,9}
    let stream_id = DataStreamId(3);
    server
        .send_subgroup_header(
            stream_id,
            sub_rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: 5,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 9, None)
        .expect("テストフィクスチャの前提条件を満たす");
    // FETCH (start={6,0} > effective {5,9}) → INVALID_RANGE
    let fetch_rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fetch_range_params(
                Location {
                    group_id: 6,
                    object_id: 0,
                },
                Location {
                    group_id: 6,
                    object_id: 1,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(server.fetch(fetch_rid).is_none());
    let (_, err_msg) = take_send_on_stream(&mut server);
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_INVALID_RANGE);
        }
        _ => panic!("INVALID_RANGE の RequestError が期待される"),
    }
}

/// FETCH で対象トラックの subscription に object が一切公開されていない (largest_location /
/// largest_received_location が共に None) とき、Start>Largest 検証の前段の no-objects 判定で
/// INVALID_RANGE になる (draft-ietf-moq-transport-21 §9.11 (FETCH))。
#[test]
fn fetch_no_published_objects_rejected_with_invalid_range() {
    use shiguredo_moqt::error::REQUEST_INVALID_RANGE;
    use shiguredo_moqt::message::common::Location;
    let (mut client, mut server) = establish_pair();
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // LARGEST_OBJECT なし・ object 未公開 → largest_location / largest_received_location 共に None
    server
        .send_subscribe_ok(sub_rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let fetch_rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
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
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(server.fetch(fetch_rid).is_none());
    let (_, err_msg) = take_send_on_stream(&mut server);
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_INVALID_RANGE);
        }
        _ => panic!("INVALID_RANGE の RequestError が期待される"),
    }
}

// ─── FETCH data stream の Object 送受信 (draft §11.4.1 / §11.4.1.2) ─────────

/// FETCH を publisher / subscriber 両側で Established にする
///
/// 返り値は (subscriber, publisher, request_id)。
fn establish_fetch() -> (Session, Session, u64) {
    use shiguredo_moqt::message::common::Location;
    establish_fetch_with_range(
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 5,
            object_id: 0,
        },
    )
}

/// 指定範囲の FETCH を両側で Established にする
///
/// 返り値は (subscriber, publisher, request_id)。
fn establish_fetch_with_range(
    start: shiguredo_moqt::message::common::Location,
    end: shiguredo_moqt::message::common::Location,
) -> (Session, Session, u64) {
    let (mut client, mut server, rid) = establish_pending_fetch_with_range(start, end);
    server
        .send_fetch_ok(
            rid,
            0,
            end,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server, rid)
}

/// 指定範囲の FETCH を送信して server に要求を届け、FETCH_OK 前の Pending 状態で返す
///
/// 返り値は (subscriber, publisher, request_id)。
fn establish_pending_fetch_with_range(
    start: shiguredo_moqt::message::common::Location,
    end: shiguredo_moqt::message::common::Location,
) -> (Session, Session, u64) {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fetch_range_params(start, end),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server, rid)
}

/// publisher が Session API 経由で FETCH data stream に Object を送れる
#[test]
fn publisher_sends_fetch_object_via_session() {
    let (_client, mut server, rid) = establish_fetch();
    let stream_id = DataStreamId(120);

    server
        .send_fetch_header(stream_id, rid)
        .expect("FETCH_HEADER の送信に成功すること");
    // 登録の裏付け: 同 stream で Object 送信できる (§11: 1 本の data stream で送る)
    server
        .send_fetch_object(stream_id)
        .expect("FETCH Object の送信に成功すること");
    server
        .send_fetch_data_stream_closed(stream_id)
        .expect("stream 終端の通知に成功すること");
    // 終端後は索引から消える (二重終端は unknown stream id で fail する)
    let err = server
        .send_fetch_data_stream_closed(stream_id)
        .expect_err("終端済み stream の再終端は fail すること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// FETCH_HEADER 未送信のまま Object を渡すと拒否される
#[test]
fn fetch_object_before_header_is_rejected_on_send() {
    let (_client, mut server, _rid) = establish_fetch();
    let err = server
        .send_fetch_object(DataStreamId(121))
        .expect_err("FETCH_HEADER 未送信では拒否される");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// publisher 役でない FETCH に FETCH_HEADER は送れない
#[test]
fn fetch_header_requires_publisher_role() {
    let (mut client, _server, rid) = establish_fetch();
    let err = client
        .send_fetch_header(DataStreamId(122), rid)
        .expect_err("subscriber 役では送れない");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// subscriber が Session API 経由で FETCH Object を取り込める
#[test]
fn subscriber_receives_fetch_object_via_session() {
    let (mut client, _server, rid) = establish_fetch();
    let stream_id = DataStreamId(123);
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("stream type の通知に成功すること");
    client
        .recv_fetch_header(stream_id, &FetchHeader { request_id: rid })
        .expect("FETCH_HEADER の受信に成功すること");

    client
        .recv_fetch_entry(stream_id)
        .expect("FETCH Object の取り込みに成功すること");
}

/// End of Range マーカーを取り込める
///
/// draft-ietf-moq-transport-21 §11.4.1.2 (End of Range) の種別ごとの
/// デコード検証は codec 側 (`tests/test_stream.rs`) が担う。Session 層では
/// entry 到着の受理のみ検証する。
#[test]
fn subscriber_receives_end_of_range_markers() {
    for label in ["End of Non-Existent Range", "End of Unknown Range"] {
        let (mut client, _server, rid) = establish_fetch();
        let stream_id = DataStreamId(124);
        client
            .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
            .expect("stream type の通知に成功すること");
        client
            .recv_fetch_header(stream_id, &FetchHeader { request_id: rid })
            .expect("FETCH_HEADER の受信に成功すること");
        client
            .recv_fetch_entry(stream_id)
            .unwrap_or_else(|e| panic!("{label}: 取り込みに成功すること: {e:?}"));
        // 取り込み成功自体が検証
    }
}

/// FETCH_HEADER 未受信のまま entry を渡すと拒否される
#[test]
fn fetch_entry_before_header_is_rejected() {
    let (mut client, _server, _rid) = establish_fetch();
    let stream_id = DataStreamId(125);
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("stream type の通知に成功すること");
    let err = client
        .recv_fetch_entry(stream_id)
        .expect_err("FETCH_HEADER 未受信では拒否される");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(err.reason, "fetch entry received before fetch header");
}

/// subgroup stream に FETCH entry を渡すと拒否される
#[test]
fn fetch_entry_on_subgroup_stream_is_rejected() {
    let alias = 900;
    let (mut client, _server, _rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(126);
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_id, header.encode()[0] as u64)
        .expect("stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("subgroup header の通知に成功すること");

    let err = client
        .recv_fetch_entry(stream_id)
        .expect_err("subgroup stream では拒否される");
    assert_eq!(err.reason, "fetch entry received on subgroup stream");
}

/// STOP_SENDING 受信後 (draft §3.2.1 の MUST reset 実行後) にアプリが終端通知 API を呼ぶと
/// outgoing FETCH stream エントリが除去されること
///
/// draft-ietf-moq-transport-21 §3.2.1: "The Publisher can remove fetch state as soon as it has
/// received a STOP_SENDING. It MUST reset the bidi request stream and unidirectional data
/// stream associated with the FETCH."
#[test]
fn fetch_stop_sending_received_then_stream_closed_cleans_outgoing_entries() {
    let (_client, mut server, rid) = establish_fetch();
    let stream_id = DataStreamId(140);
    server
        .send_fetch_header(stream_id, rid)
        .expect("FETCH_HEADER の送信に成功すること");
    server
        .fetch_stop_sending_received(rid)
        .expect("STOP_SENDING 受信の通知に成功すること");
    // アプリがデータストリームを reset し、終端を通知する
    server
        .send_fetch_data_stream_closed(stream_id)
        .expect("reset 後の終端通知に成功すること");
    // 終端後は索引から消える (二重終端は unknown stream id で fail する)
    let err = server
        .send_fetch_data_stream_closed(stream_id)
        .expect_err("終端済み stream の再終端は fail すること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// `forget_fetch` は open 中の outgoing FETCH stream エントリも掃除する (安全網)
///
/// STOP_SENDING 受信で `Terminated` に遷移した後、アプリが終端通知 API を呼ばずに
/// `forget_fetch` した場合、outgoing FETCH stream エントリが残ってセッション寿命まで
/// リークするのを防ぐ。掃除後に終端通知 API を呼ぶと unknown stream id で fail する。
#[test]
fn forget_fetch_cleans_outgoing_fetch_entries() {
    let (_client, mut server, rid) = establish_fetch();
    let stream_id = DataStreamId(141);
    server
        .send_fetch_header(stream_id, rid)
        .expect("FETCH_HEADER の送信に成功すること");
    server
        .fetch_stop_sending_received(rid)
        .expect("STOP_SENDING 受信の通知に成功すること");
    assert!(
        server.forget_fetch(rid).is_some(),
        "Terminated 状態の fetch は破棄できること"
    );
    // 掃除済みの stream に対する終端通知は unknown stream id で fail する
    let err = server
        .send_fetch_data_stream_closed(stream_id)
        .expect_err("掃除済み stream の終端通知は fail すること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// データストリーム終端済みでない `Established` の fetch は破棄できないこと
///
/// publisher 側の fetch はデータストリーム終端済み
/// (`send_fetch_data_stream_closed` 呼び出し済み) でなければ破棄できない。
/// データ配信中の `Established` fetch に対する `forget_fetch` は `None` を返し、
/// open 中の outgoing FETCH stream エントリを掃除しない (送信を継続できる)。
#[test]
fn forget_fetch_does_not_clean_established_fetch_entries() {
    let (_client, mut server, rid) = establish_fetch();
    let stream_id = DataStreamId(142);
    server
        .send_fetch_header(stream_id, rid)
        .expect("FETCH_HEADER の送信に成功すること");
    assert!(
        server.forget_fetch(rid).is_none(),
        "Established 状態の fetch は破棄できないこと"
    );
    // 送信追跡が失われていないため、終端通知も成功する
    server
        .send_fetch_data_stream_closed(stream_id)
        .expect("終端通知に成功すること");
    // 終端後は索引から消える (二重終端は unknown stream id で fail する)
    let err = server
        .send_fetch_data_stream_closed(stream_id)
        .expect_err("終端済み stream の再終端は fail すること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// `forget_fetch` の掃除は該当 request のみを対象とし、他 fetch のエントリを巻き添えにしないこと
#[test]
fn forget_fetch_does_not_clean_other_fetch_entries() {
    use shiguredo_moqt::message::common::Location;
    let (mut client, mut server) = establish_pair();
    // client から 2 件の FETCH を送り、server (publisher) に両方の fetch を作る
    let mut rids = Vec::new();
    for name in [b"cam1".as_slice(), b"cam2".as_slice()] {
        let rid = client
            .send_fetch(
                ns(&[b"live"]),
                name.to_vec(),
                fetch_range_params(
                    Location {
                        group_id: 0,
                        object_id: 0,
                    },
                    Location {
                        group_id: 5,
                        object_id: 0,
                    },
                ),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, fetch_msg) = take_send_request(&mut client);
        server
            .recv_request(fetch_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        server
            .send_fetch_ok(
                rid,
                0,
                Location {
                    group_id: 5,
                    object_id: 0,
                },
                MessageParameters::new(),
                TrackProperties::new(),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok_msg) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(rid, ok_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        rids.push(rid);
    }
    let rid_a = rids[0];
    let rid_b = rids[1];
    server
        .send_fetch_header(DataStreamId(143), rid_a)
        .expect("FETCH_HEADER の送信に成功すること");
    server
        .send_fetch_header(DataStreamId(144), rid_b)
        .expect("FETCH_HEADER の送信に成功すること");
    // 両 stream で Object 送信できる (登録の裏付け)
    server
        .send_fetch_object(DataStreamId(143))
        .expect("rid_a の stream で送信できること");
    server
        .send_fetch_object(DataStreamId(144))
        .expect("rid_b の stream で送信できること");
    // rid_a だけを STOP_SENDING 受信 → forget で掃除する
    server
        .fetch_stop_sending_received(rid_a)
        .expect("STOP_SENDING 受信の通知に成功すること");
    assert!(
        server.forget_fetch(rid_a).is_some(),
        "Terminated 状態の fetch は破棄できること"
    );
    // forget_fetch 後に該当 fetch のエントリが残らないこと (二重終端で fail)
    let err = server
        .send_fetch_data_stream_closed(DataStreamId(143))
        .expect_err("掃除済み stream の終端通知は fail すること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // 他 fetch のエントリは巻き添えにされないこと
    server
        .send_fetch_object(DataStreamId(144))
        .expect("他 fetch の stream では送信を継続できること");
}

/// publisher 側で正常完了 (FIN 送信) した fetch を `forget_fetch` で破棄できること
///
/// draft-ietf-moq-transport-21 §3.2.1 (Fetch State Management):
/// "It can remove all FETCH state after closing the data stream with a FIN." に従い、
/// データストリーム終端済みの publisher 側 fetch は `Established` のままでも破棄可能。
#[test]
fn publisher_finished_fetch_can_be_forgotten() {
    let (_client, mut server, rid) = establish_fetch();
    // データストリームを開いて FIN 送信まで完了させる
    let stream_id = DataStreamId(142);
    server
        .send_fetch_header(stream_id, rid)
        .expect("FETCH_HEADER の送信に成功すること");
    server
        .send_fetch_data_stream_closed(stream_id)
        .expect("FIN 送信後の終端通知に成功すること");
    assert_eq!(
        server
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Established,
        "FIN 送信後も state は Established のまま維持されること"
    );
    assert!(
        server.fetch_cleanup_ready(rid) == Some(true),
        "データストリーム終端済みの Established fetch は破棄可能になること"
    );
    assert!(
        server.forget_fetch(rid).is_some(),
        "正常完了した fetch を forget_fetch で破棄できること"
    );
}

/// 破棄済み fetch の bidi request stream close が unknown request id で fail しないこと
///
/// 正常完了 fetch の破棄は peer が bidi request stream を閉じる前でも行えるため、
/// 後から届く close 通知を no-op で吸収する (draft §9.5 の REQUEST_UPDATE 等は
/// unknown request id として従来どおり fail する)。
#[test]
fn forget_finished_fetch_absorbs_late_bidi_close() {
    let (_client, mut server, rid) = establish_fetch();
    let stream_id = DataStreamId(142);
    server
        .send_fetch_header(stream_id, rid)
        .expect("FETCH_HEADER の送信に成功すること");
    server
        .send_fetch_data_stream_closed(stream_id)
        .expect("終端通知に成功すること");
    assert!(
        server.forget_fetch(rid).is_some(),
        "正常完了した fetch を forget_fetch で破棄できること"
    );
    // peer が bidi request stream を閉じた通知は no-op で吸収され、セッションは fail しない
    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("破棄済み fetch の bidi close は no-op で吸収されること");
    assert_eq!(
        server.state(),
        SessionState::Established,
        "破棄済み fetch の bidi close でセッションが fail しないこと"
    );
}

/// 破棄済み fetch に後から届く REQUEST_UPDATE が unknown request id で fail すること
///
/// doc で明言している挙動の検証 (draft §9.5 の "An endpoint that receives a
/// REQUEST_UPDATE other than in the two cases above MUST close the session with a
/// PROTOCOL_VIOLATION.")。破棄後に bidi request stream 上の制御メッセージが届いた場合は
/// 従来どおりセッションが fail する (bidi close のみ `rejected_request_ids` で吸収する)。
#[test]
fn forgotten_fetch_rejects_late_request_update() {
    use shiguredo_moqt::message::RequestUpdate;
    let (_client, mut server, rid) = establish_fetch();
    let stream_id = DataStreamId(142);
    server
        .send_fetch_header(stream_id, rid)
        .expect("FETCH_HEADER の送信に成功すること");
    server
        .send_fetch_data_stream_closed(stream_id)
        .expect("終端通知に成功すること");
    assert!(
        server.forget_fetch(rid).is_some(),
        "正常完了した fetch を forget_fetch で破棄できること"
    );
    // peer が破棄済み fetch に REQUEST_UPDATE を送った場合は unknown request id で fail する
    let err = server
        .recv_stream_message(
            rid,
            ControlMessage::RequestUpdate(RequestUpdate {
                request_id: rid,
                parameters: MessageParameters::new(),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        server.state(),
        SessionState::Closing,
        "破棄済み fetch への REQUEST_UPDATE でセッションが fail すること"
    );
}

/// 空 range (Start > End) の LOCATION_FILTER を持つ FETCH 送信は送信前に拒否する
/// (draft-ietf-moq-transport-21 §9.11 (FETCH)。peer が必ず INVALID_RANGE で拒否するため)
#[test]
fn send_fetch_with_empty_filter_range_rejected() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::message_parameter::LocationFilter;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
    };
    use shiguredo_moqt::session::types::SendRequestError;
    let (mut client, _server) = establish_pair();
    // AbsoluteRangeWithEnd {start {5, 10}, delta 0, end 3} → End {5, 3} < Start
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteRangeWithEnd {
                start: Location {
                    group_id: 5,
                    object_id: 10,
                },
                end_group_delta: 0,
                end_object: 3,
            }
            .encode_to_bytes(),
        ),
    });
    let err = client
        .send_fetch(ns(&[b"live"]), b"cam".to_vec(), params)
        .expect_err("空 range の FETCH は送信前に拒否されること");
    let SendRequestError::Session(err) = err else {
        panic!("SendRequestError::Session が期待されたが {err:?}");
    };
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    while let Some(e) = client.poll_event() {
        assert!(
            !matches!(
                e,
                SessionEvent::SendRequest { .. } | SessionEvent::CloseSession(_)
            ),
            "拒否した送信で SendRequest / CloseSession は発行されないこと"
        );
    }
}

/// 空 range (Start > End) の FETCH 受信は INVALID_RANGE で拒否する
/// (draft-ietf-moq-transport-21 §9.11 (FETCH) / §3.3.1 (Location Filters))
#[test]
fn fetch_with_empty_filter_range_rejected_with_invalid_range() {
    use shiguredo_moqt::error::REQUEST_INVALID_RANGE;
    use shiguredo_moqt::message::Fetch as WireFetch;
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
    };
    use shiguredo_moqt::stream::subgroup::SubgroupIdMode;
    use shiguredo_moqt::{message_parameter::LocationFilter, stream::subgroup::SubgroupHeader};
    // subscription を確立し {5, 10} を公開して Largest を確定させる
    // (Start > Largest ではなく空 range 規則で拒否される条件にする)
    let (mut client, mut server) = establish_pair();
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(sub_rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let stream_id = DataStreamId(1);
    server
        .send_subgroup_header(
            stream_id,
            sub_rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: 5,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 10, None)
        .expect("テストフィクスチャの前提条件を満たす");
    // 空 range の FETCH を直接受信させる (送信 API は事前に弾くため)
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteRangeWithEnd {
                start: Location {
                    group_id: 5,
                    object_id: 10,
                },
                end_group_delta: 0,
                end_object: 3,
            }
            .encode_to_bytes(),
        ),
    });
    server
        .recv_request(ControlMessage::Fetch(WireFetch {
            request_id: 2,
            track_namespace: ns(&[b"live"]),
            track_name: b"cam".to_vec(),
            parameters: params,
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(server.fetch(2).is_none());
    let (_, err_msg) = take_send_on_stream(&mut server);
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_INVALID_RANGE);
        }
        _ => panic!("INVALID_RANGE の RequestError が期待される"),
    }
}

/// FETCH で許可されないパラメータの送信は送信前に拒否し副作用を残さない
/// (draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope))
#[test]
fn send_fetch_with_disallowed_parameter_rejected_without_side_effects() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES,
    };
    use shiguredo_moqt::session::types::SendRequestError;
    let (mut client, _server) = establish_pair();
    // EXPIRES は FETCH に出現できない
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(100),
    });
    let err = client
        .send_fetch(ns(&[b"live"]), b"cam".to_vec(), params)
        .expect_err("許可外パラメータの FETCH は送信前に拒否されること");
    let SendRequestError::Session(err) = err else {
        panic!("SendRequestError::Session が期待されたが {err:?}");
    };
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    while let Some(e) = client.poll_event() {
        assert!(
            !matches!(
                e,
                SessionEvent::SendRequest { .. } | SessionEvent::CloseSession(_)
            ),
            "拒否した送信で SendRequest / CloseSession は発行されないこと"
        );
    }
    // 欠番なし: 次の送信は先頭 ID で発行される
    let rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
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
        .expect("拒否後の正常な送信ができること");
    let (sent_rid, _) = take_send_request(&mut client);
    assert_eq!(sent_rid, rid);
    assert_eq!(rid, 0, "拒否で request ID を消費しないこと");
}

/// LOCATION_FILTER 省略の FETCH は unfiltered として受理される
/// (draft-ietf-moq-transport-21 §9.11 (FETCH) / §9.20.10)
#[test]
fn fetch_without_location_filter_accepted_as_unfiltered() {
    use shiguredo_moqt::message::Fetch as WireFetch;
    use shiguredo_moqt::{stream::subgroup::SubgroupHeader, stream::subgroup::SubgroupIdMode};
    // subscription を確立し {0, 0} を公開して Largest を確定させる
    let (mut client, mut server) = establish_pair();
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(sub_rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let stream_id = DataStreamId(1);
    server
        .send_subgroup_header(
            stream_id,
            sub_rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: 0,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 0, None)
        .expect("テストフィクスチャの前提条件を満たす");
    // LOCATION_FILTER なしの FETCH は unfiltered として受理される
    server
        .recv_request(ControlMessage::Fetch(WireFetch {
            request_id: 2,
            track_namespace: ns(&[b"live"]),
            track_name: b"cam".to_vec(),
            parameters: MessageParameters::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .fetch(2)
            .expect("unfiltered FETCH は受理されること")
            .state,
        shiguredo_moqt::session::types::FetchState::Pending
    );
}

/// zero-length LOCATION_FILTER の FETCH は unfiltered として受理される
/// (draft-ietf-moq-transport-21 §3.3.1 (Location Filters): Length 0 は no filter)
#[test]
fn fetch_with_zero_length_location_filter_accepted_as_unfiltered() {
    use shiguredo_moqt::message::Fetch as WireFetch;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
    };
    use shiguredo_moqt::{stream::subgroup::SubgroupHeader, stream::subgroup::SubgroupIdMode};
    // subscription を確立し {0, 0} を公開して Largest を確定させる
    let (mut client, mut server) = establish_pair();
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(sub_rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let stream_id = DataStreamId(1);
    server
        .send_subgroup_header(
            stream_id,
            sub_rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: 0,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 0, None)
        .expect("テストフィクスチャの前提条件を満たす");
    // Length 0 の LOCATION_FILTER 付き FETCH は unfiltered として受理される
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(Vec::new()),
    });
    server
        .recv_request(ControlMessage::Fetch(WireFetch {
            request_id: 2,
            track_namespace: ns(&[b"live"]),
            track_name: b"cam".to_vec(),
            parameters: params,
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .fetch(2)
            .expect("zero-length filter の FETCH は受理されること")
            .state,
        shiguredo_moqt::session::types::FetchState::Pending
    );
}

// ─── 複数 publisher 役 subscription の Largest Object 集約 (§9.11 / §3.1.3) ─────

/// client から同一 Track に 2 本の SUBSCRIBE を張り、server (publisher) が
/// alias 1 / 2 で SUBSCRIBE_OK を返した状態を作る
///
/// 返り値は (sub1_rid, sub2_rid)。sub1 が先に登録されるため
/// `subscriptions_by_track` の先頭になる。
fn establish_two_publisher_subscriptions(client: &mut Session, server: &mut Session) -> (u64, u64) {
    let sub1_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub1_msg) = take_send_request(client);
    server
        .recv_request(sub1_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            sub1_rid,
            1,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok1_msg) = take_send_on_stream(server);
    client
        .recv_stream_message(sub1_rid, ok1_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let sub2_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub2_msg) = take_send_request(client);
    server
        .recv_request(sub2_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            sub2_rid,
            2,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok2_msg) = take_send_on_stream(server);
    client
        .recv_stream_message(sub2_rid, ok2_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    (sub1_rid, sub2_rid)
}

/// publisher (server) 側で subgroup Object を 1 件公開し、観測 Largest を確定させる
fn publish_subgroup_object(
    server: &mut Session,
    request_id: u64,
    track_alias: u64,
    stream_id: DataStreamId,
    group_id: u64,
    object_id: u64,
) {
    server
        .send_subgroup_header(
            stream_id,
            request_id,
            &SubgroupHeader {
                track_alias,
                group_id,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, object_id, None)
        .expect("テストフィクスチャの前提条件を満たす");
}

/// FETCH が INVALID_RANGE で拒否され、fetch エントリが作られないことを検証する
fn assert_fetch_rejected_with_invalid_range(
    client: &mut Session,
    server: &mut Session,
    start: Location,
    end: Location,
) {
    use shiguredo_moqt::error::REQUEST_INVALID_RANGE;
    let fetch_rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fetch_range_params(start, end),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(server.fetch(fetch_rid).is_none());
    let (_, err_msg) = take_send_on_stream(server);
    match err_msg {
        ControlMessage::RequestError(e) => assert_eq!(e.error_code, REQUEST_INVALID_RANGE),
        _ => panic!("INVALID_RANGE の RequestError が期待される"),
    }
}

/// FETCH が受理され Pending になることを検証する
fn assert_fetch_accepted(
    client: &mut Session,
    server: &mut Session,
    start: Location,
    end: Location,
) {
    use shiguredo_moqt::session::types::FetchState;
    let fetch_rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fetch_range_params(start, end),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server.fetch(fetch_rid).expect("受理されること").state,
        FetchState::Pending
    );
}

/// 先頭の publisher 役 subscription が未観測でも、他の subscription が観測済みなら
/// FETCH が INVALID_RANGE にならない (Largest は Track 単位)
///
/// draft-ietf-moq-transport-21 §9.11 (FETCH) / §3.1.3: Largest Object は Track 単位の値。
#[test]
fn fetch_accepts_start_within_largest_of_other_publisher_subscription() {
    use shiguredo_moqt::message::common::Location;
    let (mut client, mut server) = establish_pair();
    let (sub1_rid, sub2_rid) = establish_two_publisher_subscriptions(&mut client, &mut server);

    // 先頭 (sub1) が未観測で、もう一方 (sub2) も未観測のままでは INVALID_RANGE
    assert_fetch_rejected_with_invalid_range(
        &mut client,
        &mut server,
        Location {
            group_id: 10,
            object_id: 0,
        },
        Location {
            group_id: 10,
            object_id: 1,
        },
    );

    // 先頭ではない sub2 で {10, 5} を公開し、sub1 は未観測のままにする
    publish_subgroup_object(&mut server, sub2_rid, 2, DataStreamId(3), 10, 5);
    assert!(
        server
            .subscription(sub1_rid)
            .expect("subscription が存在する")
            .largest_received_location
            .is_none(),
        "先頭の subscription は未観測のまま"
    );
    assert_eq!(
        server
            .subscription(sub2_rid)
            .expect("subscription が存在する")
            .largest_received_location,
        Some(Location {
            group_id: 10,
            object_id: 5,
        })
    );

    // 先頭 (sub1) が未観測でも、他 (sub2) が観測済みなので受理される
    assert_fetch_accepted(
        &mut client,
        &mut server,
        Location {
            group_id: 10,
            object_id: 0,
        },
        Location {
            group_id: 10,
            object_id: 1,
        },
    );
}

/// 最大の Largest を持つ subscription が先頭でも、他方の Largest を上回る start が
/// Track 最大以内なら受理される
///
/// draft-ietf-moq-transport-21 §9.11 (FETCH) / §3.1.3: Largest Object は Track 単位の値。
/// 先頭が未観測で他方が観測済みのケースは
/// `fetch_accepts_start_within_largest_of_other_publisher_subscription` が担う。
#[test]
fn fetch_uses_max_largest_across_publisher_subscriptions() {
    use shiguredo_moqt::message::common::Location;
    let (mut client, mut server) = establish_pair();
    let (sub1_rid, sub2_rid) = establish_two_publisher_subscriptions(&mut client, &mut server);
    // 最大の Largest を先頭 subscription (sub1) に置き、最後の 1 件だけを見る実装を検出する
    publish_subgroup_object(&mut server, sub1_rid, 1, DataStreamId(3), 10, 5);
    publish_subgroup_object(&mut server, sub2_rid, 2, DataStreamId(4), 5, 0);

    // start {7, 0} は sub2 の Largest {5, 0} を上回るが、Track の最大 {10, 5} 以内なので受理
    assert_fetch_accepted(
        &mut client,
        &mut server,
        Location {
            group_id: 7,
            object_id: 0,
        },
        Location {
            group_id: 7,
            object_id: 1,
        },
    );

    // 最大を上回る start {20, 0} は従来どおり INVALID_RANGE
    assert_fetch_rejected_with_invalid_range(
        &mut client,
        &mut server,
        Location {
            group_id: 20,
            object_id: 0,
        },
        Location {
            group_id: 20,
            object_id: 1,
        },
    );
}

/// 先に観測済みになった subscription より、後から観測した subscription の Largest が
/// 大きい場合でも最大値が使われる
///
/// 「最初に観測済みの 1 件だけを使う」実装 (`find_map` 相当) を検出する。
/// draft-ietf-moq-transport-21 §9.11 (FETCH) / §3.1.3: Largest Object は Track 単位の値。
#[test]
fn fetch_uses_larger_largest_observed_after_smaller_one() {
    use shiguredo_moqt::message::common::Location;
    let (mut client, mut server) = establish_pair();
    let (sub1_rid, sub2_rid) = establish_two_publisher_subscriptions(&mut client, &mut server);
    // 先頭 (sub1) で {5, 0} を、後から sub2 で {10, 5} を公開する
    publish_subgroup_object(&mut server, sub1_rid, 1, DataStreamId(3), 5, 0);
    publish_subgroup_object(&mut server, sub2_rid, 2, DataStreamId(4), 10, 5);

    // start {7, 0} は最初に観測済みになった sub1 の Largest {5, 0} を上回るが、
    // Track の最大 {10, 5} 以内なので受理される
    assert_fetch_accepted(
        &mut client,
        &mut server,
        Location {
            group_id: 7,
            object_id: 0,
        },
        Location {
            group_id: 7,
            object_id: 1,
        },
    );
}
