//! FETCH のテスト (draft-ietf-moq-transport-23 §9.11)
//!
//! FETCH のフルサイクル、ストリーム終端遷移、役割違反、
//! End Location 検証、LOCATION_FILTER 範囲検証を扱う。

use super::*;

/// draft-ietf-moq-transport-23 §3.2.4 (Fetch State Management): subscriber 側で FETCH データストリームの FIN を受信すると
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
                error_code: Some(0),
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

/// subscriber のアプリ起点 cancel で bidi の両方向が CANCELLED で打ち切られる
///
/// draft-ietf-moq-transport-23 §3.2.4 (Fetch State Management): "It MUST send STOP_SENDING
/// for the bidi request stream." §6.4.2.3 (Request Cancellation and Rejection): cancel は
/// 送信方向を RESET_STREAM、受信方向を STOP_SENDING で打ち切る。発行順は受信方向の
/// StopSendingRequestStream → 送信方向の ResetRequestStream である。
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
    // cancel の 2 イベントを発行順に取り出す。他のイベントは順序判定に影響しない
    let mut cancel_events = Vec::new();
    let mut saw_request_terminated = false;
    while let Some(e) = client.poll_event() {
        match e {
            SessionEvent::StopSendingRequestStream {
                request_id,
                error_code,
            } => {
                assert_eq!(request_id, rid);
                assert_eq!(
                    error_code,
                    shiguredo_moqt::error::STREAM_CANCELLED,
                    "CANCELLED (0x1) で STOP_SENDING されること"
                );
                cancel_events.push("stop_sending");
            }
            SessionEvent::ResetRequestStream {
                request_id,
                error_code,
            } => {
                assert_eq!(request_id, rid);
                assert_eq!(
                    error_code,
                    shiguredo_moqt::error::STREAM_CANCELLED,
                    "CANCELLED (0x1) で reset されること"
                );
                cancel_events.push("reset");
            }
            SessionEvent::RequestTerminated { .. } => saw_request_terminated = true,
            _ => {}
        }
    }
    assert_eq!(
        cancel_events,
        vec!["stop_sending", "reset"],
        "受信方向の STOP_SENDING → 送信方向の RESET_STREAM の順で発行されること"
    );
    assert!(
        !saw_request_terminated,
        "アプリ起点の FETCH cancel は RequestTerminated を発行しないこと"
    );
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
        .fetch_stop_sending_received(rid, None)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::FetchState::Terminated
    );
    // §3.2.4 MUST の bidi reset がイベントで指示されること
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
        "STOP_SENDING 受信では ResetRequestStream が発行されること (§3.2.4 MUST)"
    );
}

/// publisher が受信した STOP_SENDING のエラーコードが Fetch から参照できる
///
/// draft-ietf-webtrans-http3-16 §4.4 (Resetting Data Streams) により、remap できない
/// コードは「アプリケーションエラーコード無し」として配送される。未受信は外側の `None`、
/// コード無しは内側の `None` で表す。
#[test]
fn fetch_stop_sending_received_records_error_code() {
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
    assert_eq!(
        server
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .peer_stop_sending_error_code,
        None,
        "STOP_SENDING を受信する前は None であること"
    );
    server
        .fetch_stop_sending_received(rid, Some(0x1))
        .expect("STOP_SENDING 受信の通知に成功すること");
    assert_eq!(
        server
            .fetch(rid)
            .expect("STOP_SENDING 受信後も fetch が保持されていること")
            .peer_stop_sending_error_code,
        Some(Some(0x1)),
        "受信したエラーコードが保持されること"
    );
}

/// アプリケーションエラーコード無しの STOP_SENDING も区別して保持できる
#[test]
fn fetch_stop_sending_received_records_no_application_error_code() {
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
    server
        .fetch_stop_sending_received(rid, None)
        .expect("STOP_SENDING 受信の通知に成功すること");
    assert_eq!(
        server
            .fetch(rid)
            .expect("STOP_SENDING 受信後も fetch が保持されていること")
            .peer_stop_sending_error_code,
        Some(None),
        "アプリケーションエラーコード無しは内側の None で表すこと"
    );
}

/// 終端済み fetch への STOP_SENDING の重複通知はセッションを閉じずに拒否される
///
/// bidi request stream 側の STOP_SENDING が先に fetch を終端した場合、後着のデータ
/// ストリーム側の通知は拒否される。この経路で `fail()` を呼ばないことを固定する。
#[test]
fn fetch_stop_sending_received_twice_does_not_close_session() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::session::types::SessionState;
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
    server
        .fetch_stop_sending_received(rid, Some(0x1))
        .expect("1 回目の通知に成功すること");
    let err = server
        .fetch_stop_sending_received(rid, Some(0x1))
        .expect_err("終端済みの fetch への再通知は拒否されること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        server.state(),
        SessionState::Established,
        "セッションは閉じないこと"
    );
    assert_eq!(
        server
            .fetch(rid)
            .expect("STOP_SENDING 受信後も fetch が保持されていること")
            .peer_stop_sending_error_code,
        Some(Some(0x1)),
        "1 回目に受理したコードが保持されたままであること"
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
            fetch_ok_params(Some(Location {
                group_id: 1,
                object_id: 0,
            })),
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
    let err = client.fetch_stop_sending_received(rid, None).unwrap_err();
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
                error_code: Some(0),
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

/// FETCH の FETCH_OK で LARGEST_OBJECT が Start Location より小さければ INVALID_FIELD
/// (draft-ietf-moq-transport-23 §3.2 (Fetch) / §16.12.1 (Session Termination Error Codes))
#[test]
fn fetch_ok_end_before_start_closes_session() {
    use shiguredo_moqt::message::{RequestOk as WireRequestOk, common::Location};
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
            ControlMessage::RequestOk(WireRequestOk {
                parameters: fetch_ok_params(Some(Location {
                    group_id: 2,
                    object_id: 0,
                })),
                track_properties: TrackProperties::new(),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_INVALID_FIELD);
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
/// INVALID_RANGE で拒否される (draft-ietf-moq-transport-23 §3.2 (Fetch))。
#[test]
fn fetch_start_exceeds_largest_rejected_with_invalid_range() {
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

/// FETCH で Start == 観測 Largest のとき (greater than ではない) 範囲内として受理され、
/// FETCH_OK が返る (draft-ietf-moq-transport-23 §3.2 (Fetch): "greater than" のみ拒否、等値は受理)。
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
/// INVALID_RANGE で拒否される (draft-ietf-moq-transport-23 §3.2 (Fetch) / draft-ietf-moq-transport-23 §3.3.1 (Location Filters))。
#[test]
fn fetch_start_exceeds_received_largest_rejected() {
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
    assert_fetch_rejected_with_invalid_range(
        &mut client,
        &mut server,
        Location {
            group_id: 6,
            object_id: 0,
        },
        Location {
            group_id: 6,
            object_id: 1,
        },
    );
}

/// FETCH で対象トラックの subscription に object が一切公開されていない (largest_location /
/// largest_received_location が共に None) とき、Start>Largest 検証の前段の no-objects 判定で
/// INVALID_RANGE になる (draft-ietf-moq-transport-23 §3.2 (Fetch))。
#[test]
fn fetch_no_published_objects_rejected_with_invalid_range() {
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
    assert_fetch_rejected_with_invalid_range(
        &mut client,
        &mut server,
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 1,
            object_id: 0,
        },
    );
}

// ─── FETCH data stream の Object 送受信 (draft §11.4.1 / §11.4.2) ─────────

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
        .send_fetch_ok(rid, fetch_ok_params(Some(end)), TrackProperties::new())
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
        .send_fetch_object(
            stream_id,
            &shiguredo_moqt::message::common::Location {
                group_id: 0,
                object_id: 0,
            },
        )
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
        .send_fetch_object(
            DataStreamId(121),
            &shiguredo_moqt::message::common::Location {
                group_id: 0,
                object_id: 0,
            },
        )
        .expect_err("FETCH_HEADER 未送信では拒否される");
    assert_eq!(
        err.as_session_error().expect("セッション層のエラー").code,
        SESSION_PROTOCOL_VIOLATION
    );
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
        .recv_fetch_entry(stream_id, &fetch_object_entry(0, 0))
        .expect("FETCH Object の取り込みに成功すること");
}

/// End of Range マーカーを取り込める
///
/// draft-ietf-moq-transport-23 §11.4.2 (End of Range) の種別ごとの
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
        let entry = match label {
            "End of Non-Existent Range" => fetch_end_of_non_existent_range_entry(0, 0),
            _ => fetch_end_of_unknown_range_entry(0, 0),
        };
        client
            .recv_fetch_entry(stream_id, &entry)
            .unwrap_or_else(|e| panic!("{label}: 取り込みに成功すること: {e:?}"));
        // 取り込み成功自体が検証
    }
}

/// End of Unknown Range の後も既知の Object を同じ stream で配送し続けられる
///
/// draft-ietf-moq-transport-23 §3.2.2 (Gaps in a Fetch Stream): "If a Publisher receives a
/// FETCH with a range that includes one or more Objects with Unknown state, it indicates the
/// range of Unknown Objects (End of Unknown Range, see Section 11.4.2) and continues serving
/// other known Objects." したがって publisher は Unknown 範囲を示した後も stream を reset
/// せず既知 Object を送り、subscriber も同じ stream から受け取り続ける。§12.5 (Stream Reset
/// Error Codes) の stream reset では中断しない。
#[test]
fn fetch_continues_known_objects_after_end_of_unknown_range() {
    use shiguredo_moqt::message::common::Location;

    let (mut client, mut server, rid) = establish_fetch();
    let stream_id = DataStreamId(127);
    // publisher 側: FETCH_HEADER → End of Unknown Range → 既知 Object の順に通知する
    server
        .send_fetch_header(stream_id, rid)
        .expect("FETCH_HEADER の送信に成功すること");
    server
        .send_fetch_end_of_range(stream_id)
        .expect("End of Unknown Range の後も stream を継続できること");
    server
        .send_fetch_object(
            stream_id,
            &Location {
                group_id: 5,
                object_id: 0,
            },
        )
        .expect("Unknown 範囲の後も既知 Object を送信できること");
    // publisher 側に reset の指示が積まれていないこと
    while let Some(event) = server.poll_event() {
        assert!(
            !matches!(
                event,
                SessionEvent::ResetDataStream { stream_id: id, .. } if id == stream_id
            ),
            "End of Unknown Range の後に stream を reset してはいけない"
        );
    }

    // subscriber 側: Unknown 範囲の entry と後続の既知 Object の entry を同じ stream で受理する
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("stream type の通知に成功すること");
    client
        .recv_fetch_header(stream_id, &FetchHeader { request_id: rid })
        .expect("FETCH_HEADER の受信に成功すること");
    client
        .recv_fetch_entry(stream_id, &fetch_end_of_unknown_range_entry(0, 0))
        .expect("End of Unknown Range の取り込みに成功すること");
    client
        .recv_fetch_entry(stream_id, &fetch_object_entry(5, 0))
        .expect("Unknown 範囲の後続の既知 Object の取り込みに成功すること");
    // subscriber 側にも reset / STOP_SENDING の指示が積まれていないこと
    while let Some(event) = client.poll_event() {
        assert!(
            !matches!(
                event,
                SessionEvent::ResetDataStream { stream_id: id, .. }
                    | SessionEvent::StopSendingDataStream { stream_id: id, .. }
                    if id == stream_id
            ),
            "End of Unknown Range の後に stream を止めてはいけない"
        );
    }
    // セッションも FETCH も継続していること
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Established
    );
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
        .recv_fetch_entry(stream_id, &fetch_object_entry(0, 0))
        .expect_err("FETCH_HEADER 未受信では拒否される");
    assert_eq!(err.code, SESSION_MALFORMED_OBJECT);
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
        .recv_fetch_entry(stream_id, &fetch_object_entry(0, 0))
        .expect_err("subgroup stream では拒否される");
    assert_eq!(err.reason, "fetch entry received on subgroup stream");
}

/// STOP_SENDING 受信後 (draft §3.2.4 の MUST reset 実行後) にアプリが終端通知 API を呼ぶと
/// outgoing FETCH stream エントリが除去されること
///
/// draft-ietf-moq-transport-23 §3.2.4: "The Publisher can remove fetch state as soon as it has
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
        .fetch_stop_sending_received(rid, None)
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
        .fetch_stop_sending_received(rid, None)
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
                fetch_ok_params(Some(Location {
                    group_id: 5,
                    object_id: 0,
                })),
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
        .send_fetch_object(
            DataStreamId(143),
            &shiguredo_moqt::message::common::Location {
                group_id: 0,
                object_id: 0,
            },
        )
        .expect("rid_a の stream で送信できること");
    server
        .send_fetch_object(
            DataStreamId(144),
            &shiguredo_moqt::message::common::Location {
                group_id: 0,
                object_id: 0,
            },
        )
        .expect("rid_b の stream で送信できること");
    // rid_a だけを STOP_SENDING 受信 → forget で掃除する
    server
        .fetch_stop_sending_received(rid_a, None)
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
        .send_fetch_object(
            DataStreamId(144),
            &shiguredo_moqt::message::common::Location {
                group_id: 0,
                object_id: 0,
            },
        )
        .expect("他 fetch の stream では送信を継続できること");
}

/// publisher 側で正常完了 (FIN 送信) した fetch を `forget_fetch` で破棄できること
///
/// draft-ietf-moq-transport-23 §3.2.4 (Fetch State Management):
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
/// REQUEST_UPDATE other than in the two cases above MUST close the session with
/// UNEXPECTED_MESSAGE.")。破棄後に bidi request stream 上の制御メッセージが届いた場合は
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
                request_id: rid + 2,
                parameters: MessageParameters::new(),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_UNEXPECTED_MESSAGE);
    assert_eq!(
        server.state(),
        SessionState::Closing,
        "破棄済み fetch への REQUEST_UPDATE でセッションが fail すること"
    );
}

/// 空 range (Start > End) の LOCATION_FILTER を持つ FETCH 送信は送信前に拒否する
/// (draft-ietf-moq-transport-23 §3.2 (Fetch)。peer が必ず INVALID_RANGE で拒否するため)
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
        value: MessageParameterValue::LocationFilter(LocationFilter::AbsoluteRangeWithEnd {
            start: Location {
                group_id: 5,
                object_id: 10,
            },
            end_group_delta: 0,
            end_object: 3,
        }),
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
/// (draft-ietf-moq-transport-23 §3.2 (Fetch) / §3.3.1 (Location Filters))
#[test]
fn fetch_with_empty_filter_range_rejected_with_invalid_range() {
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
        value: MessageParameterValue::LocationFilter(LocationFilter::AbsoluteRangeWithEnd {
            start: Location {
                group_id: 5,
                object_id: 10,
            },
            end_group_delta: 0,
            end_object: 3,
        }),
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
    assert_invalid_range_request_error(&mut server);
}

// ─── 相対 Location Filter の拒否 (draft §3.2) ──────────────────────────────

/// publisher (server) 側で `{group_id, object_id}` を公開し、観測 Largest Object を確定させる
///
/// SUBSCRIBE / SUBSCRIBE_OK を 1 往復してから Object を公開する。返り値は
/// (subscriber, publisher, subscription の Request ID)。
fn establish_track_with_published_object(group_id: u64, object_id: u64) -> (Session, Session, u64) {
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
    publish_subgroup_object(
        &mut server,
        sub_rid,
        1,
        DataStreamId(1),
        group_id,
        object_id,
    );
    (client, server, sub_rid)
}

/// 相対フィルタ拒否の REQUEST_ERROR が INVALID_RANGE と固有の理由を持つことを検証する
///
/// 同じ INVALID_RANGE でも範囲判定による拒否 (空 range / Largest 超過 / 未公開 track) と
/// 区別できるよう、理由句まで確認する。
fn assert_relative_filter_rejected(server: &mut Session) {
    use shiguredo_moqt::error::REQUEST_INVALID_RANGE;
    let (_, err_msg, fin) = take_send_on_stream_with_fin(server);
    assert!(fin, "拒否応答は FIN で送信されること");
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_INVALID_RANGE);
            assert_eq!(
                e.reason.as_str(),
                "relative location filter is not allowed in FETCH",
                "範囲判定ではなく相対フィルタの形式を理由に拒否すること"
            );
        }
        other => panic!("INVALID_RANGE の RequestError が期待されたが {other:?}"),
    }
}

/// 相対 Start (Type 0x01) の単独 FETCH は INVALID_RANGE で拒否される
///
/// draft-ietf-moq-transport-23 §3.2 (Fetch): "If the Location filter is relative
/// (see Section 9.19.9), the publisher MUST return REQUEST_ERROR with error code
/// INVALID_RANGE." 判定は解決後の Location ではなくフィルタの形式で行うため、
/// StartGroup=1 が Largest Group {10, 5} に対して範囲内の {10, 0} に解決される場合でも
/// 拒否される。Fetch state を作らず、セッションは維持する。
#[test]
fn fetch_with_relative_start_filter_rejected_with_invalid_range() {
    use shiguredo_moqt::message::Fetch as WireFetch;
    use shiguredo_moqt::message_parameter::{
        LocationFilter, MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
    };
    use shiguredo_moqt::session::types::FetchState;
    let (mut client, mut server, _sub_rid) = establish_track_with_published_object(10, 5);
    // StartGroup=1 は {Largest Group, 0} = {10, 0} を指し、範囲としては内側になる
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LocationFilter(LocationFilter::RelativeGroup {
            start_group: 1,
        }),
    });
    // 送信 API は相対フィルタを送らないため、受信経路を直接検証する。
    // Request ID は client が使用済みの 0 と続けて使う 2 を避けて 4 にする。
    server
        .recv_request(ControlMessage::Fetch(WireFetch {
            request_id: 4,
            track_namespace: ns(&[b"live"]),
            track_name: b"cam".to_vec(),
            parameters: params,
        }))
        .expect("拒否は Ok で返り、セッションは維持される");
    assert_eq!(
        server.state(),
        SessionState::Established,
        "相対フィルタの拒否でセッションを閉じないこと"
    );
    // Fetch state と request state は作られない
    assert!(server.fetch(4).is_none(), "Fetch state を作らないこと");
    assert_relative_filter_rejected(&mut server);
    // 拒否済み id のストリームクローズが no-op で吸収される = request state が残っていない
    server
        .recv_request_stream_closed(4, RequestStreamEnd::Fin)
        .expect("拒否済み request id のストリームクローズは no-op であること");
    // data state が残っていない = 拒否した FETCH では data stream を開けない
    let err = server
        .send_fetch_header(DataStreamId(9), 4)
        .expect_err("拒否した FETCH で data stream は開けないこと");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // 状態非汚染: 続く絶対範囲の FETCH は従来どおり受理される
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
                    group_id: 10,
                    object_id: 5,
                },
            ),
        )
        .expect("相対フィルタの拒否後も絶対範囲の FETCH を送信できること");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server.fetch(rid).expect("受理されること").state,
        FetchState::Pending,
        "絶対範囲の FETCH は従来どおり受理されること"
    );
}

/// Next Object (Type 0x05) の単独 FETCH は Largest Object が未観測でも INVALID_RANGE で拒否される
///
/// draft-ietf-moq-transport-23 §3.2 (Fetch) の相対フィルタ禁止は Largest Object の
/// 観測有無と独立した MUST である。subscription を作らず Largest Object が未観測の場合、
/// Next Object は解決値としては `{0, 0}` になり範囲判定では受理される条件なので、
/// フィルタの形式で拒否していることをこの条件で確認できる。
#[test]
fn fetch_with_next_object_filter_rejected_without_published_objects() {
    use shiguredo_moqt::message::Fetch as WireFetch;
    use shiguredo_moqt::message_parameter::{
        LocationFilter, MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
    };
    let (_client, mut server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LocationFilter(LocationFilter::NextObject),
    });
    server
        .recv_request(ControlMessage::Fetch(WireFetch {
            request_id: 0,
            track_namespace: ns(&[b"live"]),
            track_name: b"cam".to_vec(),
            parameters: params,
        }))
        .expect("拒否は Ok で返り、セッションは維持される");
    assert_eq!(server.state(), SessionState::Established);
    assert!(server.fetch(0).is_none(), "Fetch state を作らないこと");
    assert_relative_filter_rejected(&mut server);
}

/// 絶対値で範囲が定まる Location Filter と省略は従来どおり範囲判定を受ける
///
/// draft-ietf-moq-transport-23 §3.2 (Fetch) の相対フィルタ禁止はフィルタの形式が相対の
/// 場合だけ適用される。Type 0x00 (None) / Type 0x02 / 0x03 / 0x04 と省略は絶対値で範囲が
/// 定まるため、従来どおり受理 / INVALID_RANGE の範囲判定だけを受ける。
#[test]
fn fetch_with_non_relative_location_filter_keeps_range_judgement() {
    use shiguredo_moqt::message::Fetch as WireFetch;
    use shiguredo_moqt::message_parameter::{
        LocationFilter, MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
    };
    use shiguredo_moqt::session::types::FetchState;
    let (_client, mut server, _sub_rid) = establish_track_with_published_object(10, 5);
    // (表示名, filter (None は省略), 受理されるか)
    let cases: [(&str, Option<LocationFilter>, bool); 8] = [
        ("省略 (unfiltered)", None, true),
        ("Type 0x00 (None)", Some(LocationFilter::NoFilter), true),
        (
            "Type 0x02 (Absolute Start)",
            Some(LocationFilter::AbsoluteStart {
                start: Location {
                    group_id: 5,
                    object_id: 0,
                },
            }),
            true,
        ),
        (
            "Type 0x03 (Absolute Start, Group End)",
            Some(LocationFilter::AbsoluteRange {
                start: Location {
                    group_id: 5,
                    object_id: 0,
                },
                end_group_delta: 0,
            }),
            true,
        ),
        (
            "Type 0x04 (Absolute Range)",
            Some(LocationFilter::AbsoluteRangeWithEnd {
                start: Location {
                    group_id: 5,
                    object_id: 0,
                },
                end_group_delta: 0,
                end_object: 100,
            }),
            true,
        ),
        (
            "Type 0x02 (Absolute Start, Largest より後)",
            Some(LocationFilter::AbsoluteStart {
                start: Location {
                    group_id: 11,
                    object_id: 0,
                },
            }),
            false,
        ),
        (
            "Type 0x04 (Absolute Range, Largest より後の開始)",
            Some(LocationFilter::AbsoluteRangeWithEnd {
                start: Location {
                    group_id: 11,
                    object_id: 0,
                },
                end_group_delta: 0,
                end_object: 100,
            }),
            false,
        ),
        (
            "Type 0x04 (Absolute Range, 空 range)",
            Some(LocationFilter::AbsoluteRangeWithEnd {
                start: Location {
                    group_id: 5,
                    object_id: 100,
                },
                end_group_delta: 0,
                end_object: 3,
            }),
            false,
        ),
    ];
    for (index, (label, filter, accepted)) in cases.into_iter().enumerate() {
        // 各ケースで重複しない even の Request ID を使う
        // (client が SUBSCRIBE で使用済みの 0 と重ならないよう 2 から始める)
        let request_id = (index as u64 + 1) * 2;
        let mut params = MessageParameters::new();
        if let Some(filter) = filter {
            params.push(MessageParameter {
                param_type: PARAM_LOCATION_FILTER,
                value: MessageParameterValue::LocationFilter(filter),
            });
        }
        server
            .recv_request(ControlMessage::Fetch(WireFetch {
                request_id,
                track_namespace: ns(&[b"live"]),
                track_name: b"cam".to_vec(),
                parameters: params,
            }))
            .expect("テストフィクスチャの前提条件を満たす");
        if accepted {
            let fetch = server.fetch(request_id);
            assert!(fetch.is_some(), "{label}: 受理されること");
            assert_eq!(
                fetch.expect("直前に存在を確認した").state,
                FetchState::Pending,
                "{label}: Pending になること"
            );
        } else {
            assert!(
                server.fetch(request_id).is_none(),
                "{label}: INVALID_RANGE で拒否され Fetch state を作らないこと"
            );
            assert_invalid_range_request_error(&mut server);
        }
    }
}

/// 相対 Location Filter の単独 FETCH は送信 API が拒否し、Request ID を消費しない
///
/// draft-ietf-moq-transport-23 §3.2 (Fetch) により peer が必ず INVALID_RANGE で拒否する
/// メッセージを無警告で送出しないよう、送信前に自側で弾く。request_id 発行前に拒否する
/// ため欠番を作らず、セッションも閉じない。
#[test]
fn send_fetch_with_relative_location_filter_rejected_without_consuming_request_id() {
    use shiguredo_moqt::message_parameter::{
        LocationFilter, MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
    };
    use shiguredo_moqt::session::types::SendRequestError;
    let (mut client, _server) = establish_pair();
    for filter in [
        LocationFilter::RelativeGroup { start_group: 0 },
        LocationFilter::RelativeGroup { start_group: 3 },
        LocationFilter::NextObject,
    ] {
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LocationFilter(filter),
        });
        let err = client
            .send_fetch(ns(&[b"live"]), b"cam".to_vec(), params)
            .expect_err("相対 Location Filter の単独 FETCH は送信できないこと");
        let SendRequestError::Session(err) = err else {
            panic!("SendRequestError::Session が期待されたが {err:?}");
        };
        assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
        assert_eq!(
            err.reason, "relative location filter is not allowed in FETCH",
            "拒否理由が相対フィルタであることを示すこと"
        );
    }
    // 拒否した送信で SendRequest / CloseSession は発行されない
    while let Some(e) = client.poll_event() {
        assert!(
            !matches!(
                e,
                SessionEvent::SendRequest { .. } | SessionEvent::CloseSession(_)
            ),
            "拒否した送信で SendRequest / CloseSession は発行されないこと"
        );
    }
    assert_eq!(
        client.state(),
        SessionState::Established,
        "送信側の検証エラーでセッションを閉じないこと"
    );
    // Request ID を消費していない: 最初の送信は 0 のまま
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
        .expect("拒否後も絶対範囲の FETCH を送信できること");
    assert_eq!(
        rid, 0,
        "拒否した送信は Request ID を消費しないこと (欠番を作らない)"
    );
    let (sent_rid, _) = take_send_request(&mut client);
    assert_eq!(sent_rid, rid);
}

/// FETCH で許可されないパラメータの送信は送信前に拒否し副作用を残さない
/// (draft-ietf-moq-transport-23 §9.19.1 (Parameter Scope))
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
/// (draft-ietf-moq-transport-23 §9.11 (FETCH) / §9.19.9)
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

/// Location Filter Type 0x00 (None) の LOCATION_FILTER を持つ FETCH は unfiltered として受理される
/// (draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter): Type 0x00 は no filter)
#[test]
fn fetch_with_no_filter_location_filter_accepted_as_unfiltered() {
    use shiguredo_moqt::message::Fetch as WireFetch;
    use shiguredo_moqt::message_parameter::{
        LocationFilter, MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
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
    // Location Filter Type 0x00 (None) の LOCATION_FILTER 付き FETCH は unfiltered として受理される
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LocationFilter(LocationFilter::NoFilter),
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
            .expect("Type 0x00 (None) の filter の FETCH は受理されること")
            .state,
        shiguredo_moqt::session::types::FetchState::Pending
    );
}

// ─── 複数 publisher 役 subscription の Largest Object 集約 (§9.11 / §3.1.4) ─────

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

/// 直近の request stream 応答が INVALID_RANGE の REQUEST_ERROR であることを検証する
///
/// REQUEST_ERROR の error_code 検証を 1 箇所に集約する。送信 API 経由の FETCH
/// (`assert_fetch_rejected_with_invalid_range`) と、送信 API が空 range を拒否するため
/// `recv_request` へ直接渡す経路の両方から使う。
fn assert_invalid_range_request_error(server: &mut Session) {
    use shiguredo_moqt::error::REQUEST_INVALID_RANGE;
    let (_, err_msg) = take_send_on_stream(server);
    match err_msg {
        ControlMessage::RequestError(e) => assert_eq!(e.error_code, REQUEST_INVALID_RANGE),
        _ => panic!("INVALID_RANGE の RequestError が期待される"),
    }
}

/// FETCH が INVALID_RANGE で拒否され、fetch エントリが作られないことを検証する
fn assert_fetch_rejected_with_invalid_range(
    client: &mut Session,
    server: &mut Session,
    start: Location,
    end: Location,
) {
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
    assert_invalid_range_request_error(server);
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
/// draft-ietf-moq-transport-23 §9.11 (FETCH) / §3.1.4: Largest Object は Track 単位の値。
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
/// draft-ietf-moq-transport-23 §9.11 (FETCH) / §3.1.4: Largest Object は Track 単位の値。
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
/// draft-ietf-moq-transport-23 §9.11 (FETCH) / §3.1.4: Largest Object は Track 単位の値。
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

/// 要求範囲外の Location で FETCH Object を送ると `LocalFilterMismatch` で拒否されること
///
/// draft-ietf-moq-transport-23 §3.3.1 (Location Filters):
/// "A publisher MUST NOT send objects from outside the requested range."
#[test]
fn fetch_object_outside_requested_range_is_rejected() {
    let (_client, mut server, rid) = establish_fetch();
    let stream_id = DataStreamId(150);
    server
        .send_fetch_header(stream_id, rid)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = server
        .send_fetch_object(
            stream_id,
            &shiguredo_moqt::message::common::Location {
                group_id: 6,
                object_id: 0,
            },
        )
        .expect_err("要求範囲外の Location は拒否されること");
    assert_eq!(
        err,
        shiguredo_moqt::session::types::SendRequestError::LocalFilterMismatch
    );
    assert_eq!(
        server.state(),
        SessionState::Established,
        "拒否してもセッションは閉じない"
    );
}

/// 応答範囲の終端を超える Location の Object は拒否されること
#[test]
fn fetch_object_beyond_response_end_is_rejected() {
    use shiguredo_moqt::message::common::Location;
    let (mut client, mut server, rid) = establish_pending_fetch_with_range(
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 5,
            object_id: 0,
        },
    );
    // LARGEST_OBJECT を要求範囲より手前にして応答範囲の終端を切り詰める
    server
        .send_fetch_ok(
            rid,
            fetch_ok_params(Some(Location {
                group_id: 2,
                object_id: 0,
            })),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let stream_id = DataStreamId(151);
    server
        .send_fetch_header(stream_id, rid)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = server
        .send_fetch_object(
            stream_id,
            &Location {
                group_id: 3,
                object_id: 0,
            },
        )
        .expect_err("応答範囲の終端を超える Object は拒否されること");
    assert_eq!(
        err,
        shiguredo_moqt::session::types::SendRequestError::LocalFilterMismatch
    );
}

/// End of Range は Location 検証を通らずに送れること
#[test]
fn fetch_end_of_range_is_accepted_without_location_validation() {
    let (_client, mut server, rid) = establish_fetch();
    let stream_id = DataStreamId(152);
    server
        .send_fetch_header(stream_id, rid)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_fetch_end_of_range(stream_id)
        .expect("End of Range は Location 検証なしで受理されること");
    server
        .send_fetch_data_stream_closed(stream_id)
        .expect("テストフィクスチャの前提条件を満たす");
}

// ─── 初回応答 (REQUEST_OK、別名 FETCH_OK) の応答範囲と dispatch ─────────
//
// draft-ietf-moq-transport-23 §3.2 (Fetch) / §3.2.2 (Gaps in a Fetch Stream) /
// §9.3 (REQUEST_OK): FETCH の初回成功応答は REQUEST_OK (Type 0x07) であり、FETCH の
// REQUEST_UPDATE_OK とはメッセージ型では区別できない。応答範囲の終端は LARGEST_OBJECT が
// 要求範囲内ならその値、そうでなければ要求で明示した End Location になる。

/// 要求が End を省略した FETCH (Absolute Start) のパラメータを作る
///
/// draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter): End を省略した FETCH の
/// End は Largest Object である。
fn fetch_start_params(start: Location) -> MessageParameters {
    use shiguredo_moqt::message_parameter::{
        LocationFilter, MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
    };
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LocationFilter(LocationFilter::AbsoluteStart { start }),
    });
    params
}

/// 要求 range を明示した FETCH を送り、publisher 側の Pending 状態で返す
fn send_pending_fetch_with_params(parameters: MessageParameters) -> (Session, Session, u64) {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_fetch(ns(&[b"live"]), b"cam".to_vec(), parameters)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server, rid)
}

/// 送信された REQUEST_OK の LARGEST_OBJECT を取り出す
fn largest_object_of(msg: &ControlMessage) -> Option<(u64, u64)> {
    match msg {
        ControlMessage::RequestOk(ok) => ok.parameters.largest_object(),
        other => panic!("REQUEST_OK が期待されたが {other:?} だった"),
    }
}

/// LARGEST_OBJECT が要求範囲内なら応答範囲の終端がそこへ切り詰められること
///
/// 要求 range は {0, 0} から {10, 0} までで、Largest Object は {5, 0} である。
/// §3.2 (Fetch) の "If Largest Object is within the requested range, the FETCH response ends
/// at Largest Object" に従い、応答範囲の終端は {5, 0} になる。
#[test]
fn fetch_ok_largest_object_within_range_truncates_end() {
    let (mut client, mut server, rid) = establish_pending_fetch_with_range(
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 10,
            object_id: 0,
        },
    );
    server
        .send_fetch_ok(
            rid,
            fetch_ok_params(Some(Location {
                group_id: 5,
                object_id: 0,
            })),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    assert_eq!(
        largest_object_of(&ok_msg),
        Some((5, 0)),
        "要求範囲内の LARGEST_OBJECT は応答に載ること"
    );
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .end_location,
        Some(Location {
            group_id: 5,
            object_id: 0,
        }),
        "応答範囲の終端が LARGEST_OBJECT へ切り詰められること"
    );
    let mut saw_ok = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::FetchOkReceived {
            request_id,
            end_location,
            ..
        } = ev
        {
            assert_eq!(request_id, rid);
            assert_eq!(
                end_location,
                Location {
                    group_id: 5,
                    object_id: 0,
                },
                "切り詰めた終端がイベントで通知されること"
            );
            saw_ok = true;
        }
    }
    assert!(saw_ok, "FetchOkReceived が発行されること");
}

/// LARGEST_OBJECT が要求範囲外なら応答範囲の終端は要求の End Location になること
///
/// 要求 range は {0, 0} から {5, 0} までで、Largest Object は {10, 0} である。
/// §3.2 (Fetch) の "Otherwise, the response ends at the requested End Location" に従い、
/// 応答範囲の終端は {5, 0} になり、LARGEST_OBJECT は応答に載らない。
#[test]
fn fetch_ok_largest_object_outside_range_uses_requested_end() {
    let (mut client, mut server, rid) = establish_pending_fetch_with_range(
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 5,
            object_id: 0,
        },
    );
    server
        .send_fetch_ok(
            rid,
            fetch_ok_params(Some(Location {
                group_id: 10,
                object_id: 0,
            })),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    assert_eq!(
        largest_object_of(&ok_msg),
        None,
        "要求範囲外の LARGEST_OBJECT は応答に載らないこと"
    );
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .end_location,
        Some(Location {
            group_id: 5,
            object_id: 0,
        }),
        "応答範囲の終端が要求の End Location になること"
    );
}

/// LARGEST_OBJECT を省略した応答は要求の End Location を終端にすること
///
/// §9.19.17 (LARGEST OBJECT Parameter) は Largest Object が要求範囲内のときだけ
/// FETCH_OK への同梱を必須とする。要求 range が {0, 0} から {5, 0} までで Largest Object が
/// 範囲外なら省略できるため、終端は要求の End Location になる。
#[test]
fn fetch_ok_without_largest_object_uses_requested_end() {
    let (mut client, mut server, rid) = establish_pending_fetch_with_range(
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 5,
            object_id: 0,
        },
    );
    server
        .send_fetch_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    assert_eq!(
        largest_object_of(&ok_msg),
        None,
        "LARGEST_OBJECT を省略した応答には載らないこと"
    );
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .end_location,
        Some(Location {
            group_id: 5,
            object_id: 0,
        }),
        "省略時は要求の End Location が終端になること"
    );
}

/// End を省略した要求への LARGEST_OBJECT 付き応答は終端を Largest Object にすること
///
/// draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter): End を省略した FETCH の
/// End は Largest Object である。応答範囲の終端は LARGEST_OBJECT になり、要求の End は
/// 存在しないため、LARGEST_OBJECT は必須である (§9.19.17)。
#[test]
fn fetch_ok_largest_object_resolves_open_ended_request() {
    let (mut client, mut server, rid) =
        send_pending_fetch_with_params(fetch_start_params(Location {
            group_id: 0,
            object_id: 0,
        }));
    server
        .send_fetch_ok(
            rid,
            fetch_ok_params(Some(Location {
                group_id: 3,
                object_id: 4,
            })),
            TrackProperties::new(),
        )
        .expect("End を省略した要求には LARGEST_OBJECT が必要であること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .end_location,
        Some(Location {
            group_id: 3,
            object_id: 4,
        }),
        "Largest Object が応答範囲の終端になること"
    );
}

/// End を省略した要求への LARGEST_OBJECT 無し応答は拒否されること
///
/// §3.2 (Fetch) は "the FETCH_OK MUST NOT be sent until the publisher knows whether Largest
/// Object is within the requested range." とし、End を省略した要求の End は Largest Object
/// である (§9.19.9)。LARGEST_OBJECT の省略は §9.19.17 により「Track に Object が無い」を
/// 意味するため、その状態の FETCH_OK は応答範囲を確定できない。
#[test]
fn fetch_ok_without_largest_object_for_open_ended_request_closes_session() {
    let (mut client, mut server, rid) =
        send_pending_fetch_with_params(fetch_start_params(Location {
            group_id: 0,
            object_id: 0,
        }));
    // publisher 側も応答範囲を確定できないため送信 API が拒否する
    let err = server
        .send_fetch_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect_err("End を省略した要求への LARGEST_OBJECT 無し応答は拒否されること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // ワイヤで届いた場合も受信側が INVALID_PARAMETER で閉じる
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect_err("LARGEST_OBJECT 無しの FETCH_OK は拒否されること");
    assert_eq!(err.code, SESSION_INVALID_PARAMETER);
}

/// 初回応答の Track Properties は受理され、2 回目以降 (REQUEST_UPDATE_OK) は拒否されること
///
/// draft-ietf-moq-transport-23 §9.3 (REQUEST_OK): Track Properties は TRACK_STATUS_OK と
/// FETCH_OK でのみ populate され、REQUEST_UPDATE_OK では空でなければならない
/// ("If an endpoint receives Track Properties in one of these messages it MUST close the
/// session with INVALID_FIELD.")。
#[test]
fn track_properties_are_accepted_only_in_the_initial_fetch_ok() {
    use shiguredo_moqt::track_properties::{
        PROP_DEFAULT_PUBLISHER_PRIORITY, TrackProperty, TrackPropertyValue,
    };
    let (mut client, mut server, rid) = establish_pending_fetch_with_range(
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 5,
            object_id: 0,
        },
    );
    let mut track_properties = TrackProperties::new();
    track_properties.push(TrackProperty {
        prop_type: PROP_DEFAULT_PUBLISHER_PRIORITY,
        value: TrackPropertyValue::VarInt(200),
    });
    // 初回応答では Track Properties を運べる
    server
        .send_fetch_ok(rid, fetch_ok_params(None), track_properties.clone())
        .expect("初回応答の Track Properties は受理されること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("初回応答の Track Properties は受理されること");
    // 2 回目の REQUEST_OK は REQUEST_UPDATE_OK として扱われ、Track Properties を持つと
    // INVALID_FIELD でセッションを閉じる
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
                parameters: MessageParameters::new(),
                track_properties,
            }),
        )
        .expect_err("REQUEST_UPDATE_OK の Track Properties は拒否されること");
    assert_eq!(err.code, SESSION_INVALID_FIELD);
}

/// 初回応答の後に届いた REQUEST_OK は REQUEST_UPDATE_OK として dispatch されること
///
/// FETCH の初回応答 (FETCH_OK) と FETCH の REQUEST_UPDATE_OK は同じ REQUEST_OK (Type 0x07)
/// で届く (§9.3 (REQUEST_OK))。受信側は初回応答の受領有無 (`response_received`) で
/// 応答 context を決め、2 回目以降は REQUEST_UPDATE_OK としてアプリへ通知する。
#[test]
fn second_request_ok_is_dispatched_as_request_update_ok() {
    let (mut client, _server, rid) = establish_fetch();
    // 1 回目 (初回応答) は FetchOkReceived
    let mut saw_fetch_ok = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::FetchOkReceived { request_id, .. } = ev {
            assert_eq!(request_id, rid);
            saw_fetch_ok = true;
        }
    }
    assert!(saw_fetch_ok, "初回応答で FetchOkReceived が発行されること");
    // 2 回目は REQUEST_UPDATE_OK として RequestOkReceived (request_kind = Fetch)
    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("2 回目の REQUEST_OK は REQUEST_UPDATE_OK として受理されること");
    let mut saw_update_ok = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestOkReceived {
            request_id,
            request_kind,
            ..
        } = ev
        {
            assert_eq!(request_id, rid);
            assert_eq!(request_kind, RequestKind::Fetch);
            saw_update_ok = true;
        }
    }
    assert!(
        saw_update_ok,
        "REQUEST_UPDATE_OK で RequestOkReceived が発行されること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 応答前に publish 済みの Object があれば FETCH_OK の送信時点で Largest Object が決まること
///
/// draft-ietf-moq-transport-23 §3.2 (Fetch): "the FETCH_OK MUST NOT be sent until the publisher
/// knows whether Largest Object is within the requested range." 要求が End を省略している場合の
/// End は Largest Object であるため、応答前に publish した Object の最大 Location が応答範囲の
/// 終端になり、LARGEST_OBJECT が応答に載る (§9.19.17)。
#[test]
fn fetch_ok_uses_objects_published_before_the_response() {
    let alias = 700;
    let (mut client, mut server, sub_rid) = establish_subscribe_track(alias);
    // 応答前に publisher が Object を publish する (datagram は送信記録だけを残す)
    server
        .send_object_datagram(sub_rid, 7, 3, None, None, false)
        .expect("テストフィクスチャの前提条件を満たす");
    // 同じ track を End 省略で FETCH する
    let rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam1".to_vec(),
            fetch_start_params(Location {
                group_id: 0,
                object_id: 0,
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_fetch_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("観測済みの Largest Object から応答範囲を確定できること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    assert_eq!(
        largest_object_of(&ok_msg),
        Some((7, 3)),
        "publish 済みの最大 Location が LARGEST_OBJECT として載ること"
    );
    assert_eq!(
        server
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .end_location,
        Some(Location {
            group_id: 7,
            object_id: 3,
        }),
        "応答範囲の終端が観測済みの最大 Location になること"
    );
}

/// 空ストリーム応答でも応答範囲の終端が確定し、subscriber が終端を解決できること
///
/// draft-ietf-moq-transport-23 §3.2.1 (Fetch Object Delivery): 要求範囲に Object が無い場合、
/// publisher は FETCH_HEADER だけの stream を FIN で閉じる。FETCH_OK は要求範囲の終端を
/// 示し、Track / Group の終端はデータストリームの marker で通知する (§11.4.2.2)。
#[test]
fn empty_fetch_response_still_resolves_the_end() {
    let (mut client, mut server, rid) = establish_pending_fetch_with_range(
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 5,
            object_id: 0,
        },
    );
    server
        .send_fetch_ok(rid, fetch_ok_params(None), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // FETCH_HEADER だけの stream を開いて FIN する
    let stream_id = DataStreamId(160);
    server
        .send_fetch_header(stream_id, rid)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_fetch_data_stream_closed(stream_id)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_fetch_data_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    let fetch = client
        .fetch(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fetch.end_location,
        Some(Location {
            group_id: 5,
            object_id: 0,
        }),
        "空ストリームでも応答範囲の終端が確定すること"
    );
    assert_eq!(
        fetch.state,
        FetchState::Terminated,
        "空ストリームの FIN で state が Terminated になること"
    );
}

/// 降順の要求でも LARGEST_OBJECT による切り詰めが応答範囲の終端に効くこと
///
/// draft-ietf-moq-transport-23 §3.2.2 (Gaps in a Fetch Stream) は降順 (Descending Group
/// Order) でも End Location を応答範囲の上端と定める。Largest Object が要求範囲内なら
/// 応答範囲の終端はそこへ切り詰められ、降順の最初の Group がその Group になる。
#[test]
fn descending_fetch_truncates_the_end_to_largest_object() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_GROUP_ORDER,
    };
    let mut parameters = fetch_range_params(
        Location {
            group_id: 5,
            object_id: 0,
        },
        Location {
            group_id: 10,
            object_id: 0,
        },
    );
    parameters.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(2),
    });
    let (mut client, mut server, rid) = send_pending_fetch_with_params(parameters);
    server
        .send_fetch_ok(
            rid,
            fetch_ok_params(Some(Location {
                group_id: 8,
                object_id: 2,
            })),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let fetch = client
        .fetch(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fetch.end_location,
        Some(Location {
            group_id: 8,
            object_id: 2,
        }),
        "降順でも LARGEST_OBJECT が応答範囲の終端になること"
    );
    assert_eq!(
        fetch.group_order,
        Some(2),
        "要求した Group Order が保持されること"
    );
}

/// 通常の FETCH 応答 stream への STOP_SENDING でも bidi の両方向が打ち切られる
///
/// `Session::send_data_stream_stop_sending` は通常の FETCH 応答 stream の cancel を
/// `Session::send_fetch_stop_sending` へ dispatch するため、bidi request stream の
/// 受信方向 (STOP_SENDING) と送信方向 (RESET_STREAM) が同じ順序で発行される。
/// draft-ietf-moq-transport-23 §3.2.4 (Fetch State Management) /
/// §6.4.2.3 (Request Cancellation and Rejection)。
#[test]
fn send_data_stream_stop_sending_for_fetch_response_stops_bidi() {
    let (mut client, _server, rid) = establish_fetch();
    let stream_id = DataStreamId(121);
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_fetch_header(stream_id, &FetchHeader { request_id: rid })
        .expect("テストフィクスチャの前提条件を満たす");
    // ここまでのイベントは判定に影響しない
    while client.poll_event().is_some() {}

    client
        .send_data_stream_stop_sending(stream_id)
        .expect("通常の FETCH 応答 stream の STOP_SENDING は受理されること");

    let mut cancel_events = Vec::new();
    while let Some(e) = client.poll_event() {
        match e {
            SessionEvent::StopSendingRequestStream {
                request_id,
                error_code,
            } => {
                assert_eq!(request_id, rid);
                assert_eq!(error_code, shiguredo_moqt::error::STREAM_CANCELLED);
                cancel_events.push("stop_sending");
            }
            SessionEvent::ResetRequestStream {
                request_id,
                error_code,
            } => {
                assert_eq!(request_id, rid);
                assert_eq!(error_code, shiguredo_moqt::error::STREAM_CANCELLED);
                cancel_events.push("reset");
            }
            _ => {}
        }
    }
    assert_eq!(
        cancel_events,
        vec!["stop_sending", "reset"],
        "data stream 経由の cancel でも受信方向 → 送信方向の順で発行されること"
    );
}
