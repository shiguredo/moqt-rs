//! STOP_SENDING を受けた購読と FETCH の open な送信 stream を Session が reset する
//!
//! - draft-ietf-moq-transport-23 §3.1.2 (Subscription State Management): "The Publisher can
//!   remove subscription state as soon as it has received STOP_SENDING. It MUST reset any open
//!   streams associated with the SUBSCRIBE."
//! - draft-ietf-moq-transport-23 §3.2.4 (Fetch State Management): "The Publisher can remove
//!   fetch state as soon as it has received a STOP_SENDING. It MUST reset the bidi request
//!   stream and unidirectional data stream associated with the FETCH."
//!
//! I/O 層は bidi request stream の STOP_SENDING を `RequestStreamEnd::Reset` として渡す。
//! Session は open な送信 stream に `ResetDataStream` を発行して追跡から除去し、自動 reset
//! した stream id への終端通知を no-op で吸収する。FIN は cancel ではないため対象外である。

use super::*;
use shiguredo_moqt::error::STREAM_CANCELLED;

/// client (subscriber) から server (publisher) への購読を Established にする
fn establish_publisher_subscription(client: &mut Session, server: &mut Session, alias: u64) -> u64 {
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(rid, alias, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");
    rid
}

/// publisher 側で subgroup stream を 2 本開く
fn open_two_outgoing_subgroup_streams(
    server: &mut Session,
    rid: u64,
    alias: u64,
) -> [DataStreamId; 2] {
    let streams = [DataStreamId(11), DataStreamId(12)];
    for (index, stream_id) in streams.iter().enumerate() {
        server
            .send_subgroup_header(
                *stream_id,
                rid,
                &SubgroupHeader {
                    track_alias: alias,
                    group_id: index as u64,
                    subgroup_id: SubgroupIdMode::Explicit(0),
                    publisher_priority: Some(128),
                    has_properties: false,
                    end_of_group: false,
                    first_object: false,
                },
            )
            .expect("SUBGROUP_HEADER の送信に成功すること");
        server
            .send_subgroup_object(*stream_id, 0, None)
            .expect("Object の送信に成功すること");
    }
    streams
}

/// 発行された `ResetDataStream` の (stream_id, error_code, tracking) を集める
fn take_data_stream_resets(session: &mut Session) -> Vec<(DataStreamId, u64)> {
    let mut events = Vec::new();
    while let Some(e) = session.poll_event() {
        if let SessionEvent::ResetDataStream {
            stream_id,
            error_code,
            ..
        } = e
        {
            events.push((stream_id, error_code));
        }
    }
    events
}

/// publisher 役 subscription の cancel で open な subgroup stream が reset される
#[test]
fn subscription_cancel_resets_open_outgoing_subgroup_streams() {
    let (mut client, mut server) = establish_pair();
    let rid = establish_publisher_subscription(&mut client, &mut server, 500);
    let streams = open_two_outgoing_subgroup_streams(&mut server, rid, 500);
    while server.poll_event().is_some() {}

    // bidi request stream の RESET_STREAM (STOP_SENDING の写像) で cancel する
    server
        .recv_request_stream_closed(
            rid,
            RequestStreamEnd::Reset {
                error_code: Some(STREAM_CANCELLED),
                reliable_size: None,
            },
        )
        .expect("peer の cancel を受理すること");

    assert_eq!(
        take_data_stream_resets(&mut server),
        vec![
            (streams[0], STREAM_CANCELLED),
            (streams[1], STREAM_CANCELLED)
        ],
        "open な subgroup stream が stream_id の昇順で CANCELLED により reset されること"
    );
    // 追跡から除去され、アプリの終端通知は no-op になる
    for stream_id in streams {
        server
            .send_data_stream_closed(
                stream_id,
                RequestStreamEnd::Reset {
                    error_code: Some(0),
                    reliable_size: None,
                },
            )
            .expect("自動 reset 済み stream の終端通知は Ok になること");
    }
    // 未知の stream id は従来どおり拒否する
    let err = server
        .send_data_stream_closed(DataStreamId(99), RequestStreamEnd::Fin)
        .expect_err("未知の stream id は拒否されること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// FIN 終端では open な subgroup stream を自動 reset しない (cancel と FIN の区別)
#[test]
fn subscription_fin_does_not_reset_open_outgoing_subgroup_streams() {
    let (mut client, mut server) = establish_pair();
    let rid = establish_publisher_subscription(&mut client, &mut server, 500);
    let streams = open_two_outgoing_subgroup_streams(&mut server, rid, 500);
    while server.poll_event().is_some() {}

    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("peer の FIN を受理すること");

    assert!(
        take_data_stream_resets(&mut server).is_empty(),
        "FIN は cancel ではないため自動 reset しないこと"
    );
    // アプリの終端通知は従来どおり必要
    server
        .send_data_stream_closed(streams[0], RequestStreamEnd::Fin)
        .expect("アプリの終端通知は受理されること");
}

/// publisher 役 FETCH の cancel で open な outgoing FETCH data stream が reset される
#[test]
fn fetch_cancel_resets_open_outgoing_fetch_stream() {
    let (mut client, mut server) = establish_pair();
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
        .expect("FETCH の送信に成功すること");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("FETCH の受信に成功すること");
    server
        .send_fetch_ok(
            rid,
            fetch_ok_params(Some(Location {
                group_id: 1,
                object_id: 0,
            })),
            TrackProperties::new(),
        )
        .expect("FETCH_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("FETCH_OK の受信に成功すること");
    // publisher 側で FETCH 応答 stream を開く
    let stream_id = DataStreamId(31);
    server
        .send_fetch_header(stream_id, rid)
        .expect("FETCH_HEADER の送信に成功すること");
    server
        .send_fetch_object(
            stream_id,
            &Location {
                group_id: 0,
                object_id: 0,
            },
        )
        .expect("FETCH Object の送信に成功すること");
    while server.poll_event().is_some() {}

    // bidi request stream の RESET_STREAM で cancel する
    server
        .recv_request_stream_closed(
            rid,
            RequestStreamEnd::Reset {
                error_code: Some(STREAM_CANCELLED),
                reliable_size: None,
            },
        )
        .expect("peer の cancel を受理すること");

    assert_eq!(
        take_data_stream_resets(&mut server),
        vec![(stream_id, STREAM_CANCELLED)],
        "open な outgoing FETCH data stream が reset されること"
    );
    assert!(
        server
            .fetch(rid)
            .expect("fetch が存在すること")
            .data_stream_finished,
        "data_stream_finished が true になること"
    );
    // 自動 reset 済み stream への終端通知は no-op
    server
        .send_fetch_data_stream_closed(stream_id)
        .expect("自動 reset 済み stream の終端通知は Ok になること");
    assert_eq!(server.state(), SessionState::Established);
}

/// uni data stream の STOP_SENDING 受信でも outgoing FETCH data stream が reset される
#[test]
fn fetch_stop_sending_received_resets_open_outgoing_fetch_stream() {
    let (mut client, mut server) = establish_pair();
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
        .expect("FETCH の送信に成功すること");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("FETCH の受信に成功すること");
    server
        .send_fetch_ok(
            rid,
            fetch_ok_params(Some(Location {
                group_id: 1,
                object_id: 0,
            })),
            TrackProperties::new(),
        )
        .expect("FETCH_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("FETCH_OK の受信に成功すること");
    let stream_id = DataStreamId(32);
    server
        .send_fetch_header(stream_id, rid)
        .expect("FETCH_HEADER の送信に成功すること");
    while server.poll_event().is_some() {}

    server
        .fetch_stop_sending_received(rid, Some(STREAM_CANCELLED))
        .expect("STOP_SENDING の受信に成功すること");

    // bidi の reset が先、data stream の reset が後
    let mut order = Vec::new();
    while let Some(e) = server.poll_event() {
        match e {
            SessionEvent::ResetRequestStream { request_id, .. } => {
                assert_eq!(request_id, rid);
                order.push("request");
            }
            SessionEvent::ResetDataStream {
                stream_id: id,
                error_code,
                ..
            } => {
                assert_eq!(id, stream_id);
                assert_eq!(error_code, STREAM_CANCELLED);
                order.push("data");
            }
            _ => {}
        }
    }
    assert_eq!(
        order,
        vec!["request", "data"],
        "bidi request stream の reset が先、data stream の reset が後であること"
    );
    assert!(
        server
            .fetch(rid)
            .expect("fetch が存在すること")
            .data_stream_finished
    );
}

/// FETCH の FIN 終端では outgoing FETCH data stream を自動 reset しない
#[test]
fn fetch_fin_does_not_reset_outgoing_fetch_stream() {
    let (mut client, mut server) = establish_pair();
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
        .expect("FETCH の送信に成功すること");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("FETCH の受信に成功すること");
    server
        .send_fetch_ok(
            rid,
            fetch_ok_params(Some(Location {
                group_id: 1,
                object_id: 0,
            })),
            TrackProperties::new(),
        )
        .expect("FETCH_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("FETCH_OK の受信に成功すること");
    let stream_id = DataStreamId(33);
    server
        .send_fetch_header(stream_id, rid)
        .expect("FETCH_HEADER の送信に成功すること");
    while server.poll_event().is_some() {}

    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("peer の FIN を受理すること");
    assert!(
        take_data_stream_resets(&mut server).is_empty(),
        "FIN は cancel ではないため自動 reset しないこと"
    );
}
