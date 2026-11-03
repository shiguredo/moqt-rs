//! request stream 上の GOAWAY の protocol role 検証
//!
//! draft-ietf-moq-transport-23 §9.2 (GOAWAY):
//! "Only the receiver of a request can send GOAWAY on its request stream. If the receiver of a
//! request receives a GOAWAY on that request stream, it MUST close the session with a
//! PROTOCOL_VIOLATION."
//!
//! request の sender (initiator) は SUBSCRIBE / FETCH / TRACK_STATUS が subscriber
//! (§9.6 / §9.11 / §9.12)、PUBLISH が publisher (§9.8) である。client / server の role や
//! publisher / subscriber の track role 単独とは一致しないため、4 種類すべてを検証する。

use super::*;

/// request stream 上の GOAWAY (空 URI / timeout 0) を組み立てる
fn goaway_on_request_stream() -> ControlMessage {
    ControlMessage::Goaway(Goaway {
        new_session_uri: Vec::new(),
        timeout: 0,
    })
}

/// request の sender からの GOAWAY 送信が、イベントも期限 state も作らずに拒否されること
fn assert_send_goaway_rejected(s: &mut Session, request_id: u64) {
    let err = s
        .send_goaway_on_request_stream(request_id, Vec::new(), 100)
        .expect_err("request の sender は自身の request stream 上で GOAWAY を送れないこと");
    assert_eq!(
        err.code, SESSION_PROTOCOL_VIOLATION,
        "§9.2 の role 制約違反は PROTOCOL_VIOLATION で拒否すること"
    );
    assert_eq!(
        s.state(),
        SessionState::Established,
        "ローカル API の拒否でセッションを閉じないこと"
    );
    // 拒否時に送信イベントを発行していないこと (確立時に積まれた受信イベントは読み飛ばす)
    while let Some(event) = s.poll_event() {
        assert!(
            !matches!(event, SessionEvent::SendOnStream { .. }),
            "拒否時は SendOnStream を 1 つも発行しないこと"
        );
    }

    // 拒否が期限 state を残していないこと。timeout 100 ms を過ぎても reset を発行しない
    s.tick(1_000);
    s.tick(1_100);
    while let Some(event) = s.poll_event() {
        assert!(
            !matches!(event, SessionEvent::ResetRequestStream { .. }),
            "拒否した GOAWAY の期限で request stream を reset しないこと"
        );
    }
}

/// receiver からの GOAWAY を sender が受理し、`on_request_stream` 付きで通知すること
fn assert_receiver_goaway_accepted(sender: &mut Session, request_id: u64, goaway: ControlMessage) {
    sender
        .recv_stream_message(request_id, goaway)
        .expect("request の sender は receiver からの GOAWAY を受理すること");
    let mut received = false;
    while let Some(event) = sender.poll_event() {
        if let SessionEvent::GoawayReceived {
            on_request_stream, ..
        } = event
        {
            assert_eq!(
                on_request_stream,
                Some(request_id),
                "request stream 上の GOAWAY として通知すること"
            );
            received = true;
        }
    }
    assert!(received, "GoawayReceived が発行されること");
    assert_eq!(
        sender.state(),
        SessionState::Established,
        "正当な GOAWAY でセッションを閉じないこと"
    );
}

// ─── 送信側の role 検証 ─────────────────────────────────────

/// SUBSCRIBE の sender である subscriber は GOAWAY を送信できない
#[test]
fn subscribe_sender_cannot_send_goaway_on_request_stream() {
    // client (subscriber) が SUBSCRIBE の sender、server (publisher) が receiver
    let (mut client, mut server, rid) = establish_subscribe_track(401);
    assert_send_goaway_rejected(&mut client, rid);

    // receiver は従来どおり送信できる
    server
        .send_goaway_on_request_stream(rid, b"moqt://relay.example/".to_vec(), 100)
        .expect("SUBSCRIBE の receiver は GOAWAY を送信できること");
    let (sent_rid, message) = take_send_on_stream(&mut server);
    assert_eq!(sent_rid, rid, "対象 request stream に送ること");
    assert!(
        matches!(message, ControlMessage::Goaway(_)),
        "GOAWAY を送ること"
    );
}

/// PUBLISH の sender である publisher は GOAWAY を送信できない
///
/// SUBSCRIBE とは sender / receiver が逆になるケースである。
#[test]
fn publish_sender_cannot_send_goaway_on_request_stream() {
    // server (publisher) が PUBLISH の sender、client (subscriber) が receiver
    let (mut client, mut server) = establish_pair();
    let rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            402,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("PUBLISH の送信に成功すること");
    let (_, publish_msg) = take_send_request(&mut server);
    client
        .recv_request(publish_msg)
        .expect("PUBLISH の受信に成功すること");

    assert_send_goaway_rejected(&mut server, rid);

    // receiver は従来どおり送信できる
    client
        .send_goaway_on_request_stream(rid, Vec::new(), 100)
        .expect("PUBLISH の receiver は GOAWAY を送信できること");
    let (sent_rid, message) = take_send_on_stream(&mut client);
    assert_eq!(sent_rid, rid);
    assert!(matches!(message, ControlMessage::Goaway(_)));
}

/// client が PUBLISH の sender、server が receiver のケースでも sender は GOAWAY を送信できない
///
/// client / server の `Role` ではなく request の initiator で判定していることを、
/// PUBLISH の sender / receiver を server / client から入れ替えて固定する。
#[test]
fn client_publish_sender_cannot_send_goaway_on_request_stream() {
    // client (publisher) が PUBLISH の sender、server (subscriber) が receiver
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            405,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("PUBLISH の送信に成功すること");
    let (_, publish_msg) = take_send_request(&mut client);
    server
        .recv_request(publish_msg)
        .expect("PUBLISH の受信に成功すること");

    assert_send_goaway_rejected(&mut client, rid);

    // receiver である server は送信できる
    server
        .send_goaway_on_request_stream(rid, b"moqt://relay.example/".to_vec(), 100)
        .expect("PUBLISH の receiver は GOAWAY を送信できること");
    let (sent_rid, message) = take_send_on_stream(&mut server);
    assert_eq!(sent_rid, rid);
    assert!(matches!(message, ControlMessage::Goaway(_)));
}

/// FETCH の sender である subscriber は GOAWAY を送信できない
#[test]
fn fetch_sender_cannot_send_goaway_on_request_stream() {
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
                    group_id: 10,
                    object_id: 0,
                },
            ),
        )
        .expect("FETCH の送信に成功すること");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("FETCH の受信に成功すること");

    assert_send_goaway_rejected(&mut client, rid);

    // receiver は従来どおり送信できる
    server
        .send_goaway_on_request_stream(rid, b"moqt://relay.example/".to_vec(), 100)
        .expect("FETCH の receiver は GOAWAY を送信できること");
    let (sent_rid, message) = take_send_on_stream(&mut server);
    assert_eq!(sent_rid, rid);
    assert!(matches!(message, ControlMessage::Goaway(_)));
}

/// TRACK_STATUS の sender である subscriber は GOAWAY を送信できない
#[test]
fn track_status_sender_cannot_send_goaway_on_request_stream() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, status_msg) = take_send_request(&mut client);
    server
        .recv_request(status_msg)
        .expect("TRACK_STATUS の受信に成功すること");

    assert_send_goaway_rejected(&mut client, rid);

    // receiver は従来どおり送信できる
    server
        .send_goaway_on_request_stream(rid, b"moqt://relay.example/".to_vec(), 100)
        .expect("TRACK_STATUS の receiver は GOAWAY を送信できること");
    let (sent_rid, message) = take_send_on_stream(&mut server);
    assert_eq!(sent_rid, rid);
    assert!(matches!(message, ControlMessage::Goaway(_)));
}

/// 自側 state に無い request の GOAWAY 送信は拒否される
#[test]
fn unknown_request_send_goaway_rejected() {
    let (_client, mut server) = establish_pair();
    assert_send_goaway_rejected(&mut server, 0);
}

// ─── 受信側の role 検証 ─────────────────────────────────────

/// SUBSCRIBE の receiver が sender からの GOAWAY を受信するとセッションを閉じる
#[test]
fn subscribe_receiver_recv_goaway_from_sender_closes_session() {
    // client (subscriber) が sender、server (publisher) が receiver
    let (_client, mut server, rid) = establish_subscribe_track(403);
    let err = server
        .recv_stream_message(rid, goaway_on_request_stream())
        .expect_err("request の receiver への GOAWAY は受理されないこと");
    assert_eq!(
        err.code, SESSION_PROTOCOL_VIOLATION,
        "§9.2 の MUST により PROTOCOL_VIOLATION で閉じること"
    );
    assert_eq!(server.state(), SessionState::Closing);
}

/// PUBLISH の receiver が sender からの GOAWAY を受信するとセッションを閉じる
///
/// SUBSCRIBE とは sender / receiver が逆になるケースである。
#[test]
fn publish_receiver_recv_goaway_from_sender_closes_session() {
    // server (publisher) が sender、client (subscriber) が receiver
    let (mut client, mut server) = establish_pair();
    let rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            404,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("PUBLISH の送信に成功すること");
    let (_, publish_msg) = take_send_request(&mut server);
    client
        .recv_request(publish_msg)
        .expect("PUBLISH の受信に成功すること");

    let err = client
        .recv_stream_message(rid, goaway_on_request_stream())
        .expect_err("request の receiver への GOAWAY は受理されないこと");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}

/// client が PUBLISH の sender、server が receiver のケースでも受信側は GOAWAY で閉じる
///
/// 上の PUBLISH テストと sender / receiver を入れ替えたケースである。
/// request の initiator は publisher であり、どちらの `Role` が publisher でも同じ結果になる。
#[test]
fn client_publish_receiver_recv_goaway_from_sender_closes_session() {
    // client (publisher) が sender、server (subscriber) が receiver
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            406,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("PUBLISH の送信に成功すること");
    let (_, publish_msg) = take_send_request(&mut client);
    server
        .recv_request(publish_msg)
        .expect("PUBLISH の受信に成功すること");

    let err = server
        .recv_stream_message(rid, goaway_on_request_stream())
        .expect_err("request の receiver への GOAWAY は受理されないこと");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// FETCH の receiver が sender からの GOAWAY を受信するとセッションを閉じる
#[test]
fn fetch_receiver_recv_goaway_from_sender_closes_session() {
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
                    group_id: 10,
                    object_id: 0,
                },
            ),
        )
        .expect("FETCH の送信に成功すること");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("FETCH の受信に成功すること");

    let err = server
        .recv_stream_message(rid, goaway_on_request_stream())
        .expect_err("request の receiver への GOAWAY は受理されないこと");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// TRACK_STATUS の receiver が sender からの GOAWAY を受信するとセッションを閉じる
#[test]
fn track_status_receiver_recv_goaway_from_sender_closes_session() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, status_msg) = take_send_request(&mut client);
    server
        .recv_request(status_msg)
        .expect("TRACK_STATUS の受信に成功すること");

    let err = server
        .recv_stream_message(rid, goaway_on_request_stream())
        .expect_err("request の receiver への GOAWAY は受理されないこと");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// 自側 state に無い request stream 上の GOAWAY 受信は受理してイベントを発行する
///
/// draft-ietf-moq-transport-23 §9.2 (GOAWAY) の MUST は「request の受信側が受け取った場合」に
/// だけ掛かる条件である。自側 state に request が無い場合は受信側かどうかを判定できないため、
/// 条件不成立として受理する (判定できないことを理由に閉じると、正当な GOAWAY でセッションを
/// 落とすことになる)。
#[test]
fn unknown_request_recv_goaway_is_accepted() {
    let (_client, mut server) = establish_pair();
    server
        .recv_stream_message(0, goaway_on_request_stream())
        .expect("request を特定できない GOAWAY も受理すること");
    assert_eq!(
        server.state(),
        SessionState::Established,
        "判定できない GOAWAY でセッションを閉じないこと"
    );
    let mut received = false;
    while let Some(event) = server.poll_event() {
        if let SessionEvent::GoawayReceived {
            on_request_stream, ..
        } = event
        {
            assert_eq!(
                on_request_stream,
                Some(0),
                "request stream 上の GOAWAY として通知すること"
            );
            received = true;
        }
    }
    assert!(received, "GoawayReceived が発行されること");

    // 同じ request stream への 2 回目の GOAWAY は UNEXPECTED_MESSAGE (§9.2)
    let err = server
        .recv_stream_message(0, goaway_on_request_stream())
        .expect_err("同一 request stream への 2 回目の GOAWAY は拒否されること");
    assert_eq!(err.code, SESSION_UNEXPECTED_MESSAGE);
    assert_eq!(server.state(), SessionState::Closing);
}

/// request を forget した sender が receiver からの正当な GOAWAY を受信できる
///
/// requester は cancel 後に request を破棄できる。破棄後も peer (receiver) が request stream 上で
/// GOAWAY を送ることは正当であり (§9.2 は GOAWAY を送れるのは receiver だけと定める)、
/// 自側 state に request が無いことを理由にセッションを閉じてはならない。
#[test]
fn forgotten_request_accepts_receiver_goaway() {
    // client (subscriber) が SUBSCRIBE の sender、server (publisher) が receiver
    let (mut client, _server, rid) = establish_subscribe_track(405);
    client
        .stop_sending(rid)
        .expect("stop_sending に成功すること");
    client
        .forget_subscription(rid)
        .expect("forget に成功すること");
    assert!(
        client.subscription(rid).is_none(),
        "forget 後に subscription が残らないこと"
    );

    client
        .recv_stream_message(rid, goaway_on_request_stream())
        .expect("forget 済み request への receiver からの GOAWAY が受理されること");
    assert_eq!(
        client.state(),
        SessionState::Established,
        "破棄済み request への正当な GOAWAY でセッションを閉じないこと"
    );
    let mut received = false;
    while let Some(event) = client.poll_event() {
        if let SessionEvent::GoawayReceived {
            on_request_stream, ..
        } = event
        {
            assert_eq!(on_request_stream, Some(rid));
            received = true;
        }
    }
    assert!(received, "GoawayReceived が発行されること");
}

// ─── 正当な receiver からの GOAWAY ─────────────────────────

/// FETCH の receiver (publisher) からの GOAWAY は sender (subscriber) に受理される
#[test]
fn fetch_receiver_goaway_is_accepted_by_sender() {
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
                    group_id: 10,
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
        .send_goaway_on_request_stream(rid, b"moqt://relay.example/".to_vec(), 100)
        .expect("FETCH の receiver は GOAWAY を送信できること");
    let (sent_rid, message) = take_send_on_stream(&mut server);
    assert_eq!(sent_rid, rid);
    assert_receiver_goaway_accepted(&mut client, rid, message);
}

/// TRACK_STATUS の receiver (publisher) からの GOAWAY は sender (subscriber) に受理される
#[test]
fn track_status_receiver_goaway_is_accepted_by_sender() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, status_msg) = take_send_request(&mut client);
    server
        .recv_request(status_msg)
        .expect("TRACK_STATUS の受信に成功すること");

    server
        .send_goaway_on_request_stream(rid, b"moqt://relay.example/".to_vec(), 100)
        .expect("TRACK_STATUS の receiver は GOAWAY を送信できること");
    let (sent_rid, message) = take_send_on_stream(&mut server);
    assert_eq!(sent_rid, rid);
    assert_receiver_goaway_accepted(&mut client, rid, message);
}

/// client が PUBLISH の sender のケースでも receiver (server) からの GOAWAY は受理される
///
/// SUBSCRIBE / FETCH / TRACK_STATUS は client が sender、PUBLISH は server が sender になる
/// 既存テストばかりであるため、PUBLISH の sender を client に入れ替えた向きも固定する。
#[test]
fn client_publish_sender_accepts_receiver_goaway() {
    // client (publisher) が sender、server (subscriber) が receiver
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            407,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("PUBLISH の送信に成功すること");
    let (_, publish_msg) = take_send_request(&mut client);
    server
        .recv_request(publish_msg)
        .expect("PUBLISH の受信に成功すること");

    server
        .send_goaway_on_request_stream(rid, b"moqt://relay.example/".to_vec(), 100)
        .expect("PUBLISH の receiver は GOAWAY を送信できること");
    let (sent_rid, message) = take_send_on_stream(&mut server);
    assert_eq!(sent_rid, rid);
    assert_receiver_goaway_accepted(&mut client, rid, message);
}
