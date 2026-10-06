//! GOAWAY の deadline 満了で終端した request へ遅延して届くメッセージ・data stream の吸収
//!
//! 終端と drain の状態遷移は `goaway::deadline` を参照。ここでは reset 後に届く
//! peer の応答・REQUEST_UPDATE・PUBLISH_STATE_NOTIFY・FIN / RESET_STREAM と、
//! 遅延 data stream の扱いを検証する。

use super::*;

/// deadline 満了で終端した request へ peer の正当な応答が遅延して届いても、セッションを閉じない
///
/// request stream は方向ごとに独立に閉じるため (draft-ietf-moq-transport-22 §6.4.2.2
/// (Graceful Request Stream Closure))、送信方向の reset 後も peer の送信方向は開いたままである。
/// peer が GOAWAY / reset を観測する前に送った応答は正当な traffic であり、
/// `SESSION_PROTOCOL_VIOLATION` でセッションを閉じてはならない。吸収した応答は状態を作らない。
#[test]
fn request_stream_goaway_timeout_absorbs_late_response() {
    // subscription (Pending(Subscriber)) の遅延 SUBSCRIBE_OK
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    terminate_request_by_goaway_timeout(&mut client, rid, Vec::new());

    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::default())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("終端済み request への遅延 SUBSCRIBE_OK が no-op で吸収されること");
    assert_eq!(
        client.state(),
        SessionState::Established,
        "遅延応答でセッションを閉じないこと"
    );

    // track_status (Pending) の遅延 REQUEST_OK (TRACK_STATUS_OK)
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, ts_msg) = take_send_request(&mut client);
    server
        .recv_request(ts_msg)
        .expect("TRACK_STATUS の受信に成功すること");
    terminate_request_by_goaway_timeout(&mut client, rid, Vec::new());

    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("終端済み request への遅延 TRACK_STATUS_OK が no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);
    assert!(
        client
            .track_status_request(rid)
            .expect("track status entry が残ること")
            .response
            .is_none(),
        "吸収した応答で応答状態を確定しないこと"
    );

    // track_status (Pending) の遅延 REQUEST_ERROR
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, ts_msg) = take_send_request(&mut client);
    server
        .recv_request(ts_msg)
        .expect("TRACK_STATUS の受信に成功すること");
    terminate_request_by_goaway_timeout(&mut client, rid, Vec::new());

    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestError(shiguredo_moqt::message::RequestError {
                error_code: shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST,
                retry_interval: 0,
                reason: shiguredo_moqt::message::ReasonPhrase::new("failed")
                    .expect("テストフィクスチャの前提条件を満たす"),
                redirect: None,
            }),
        )
        .expect("終端済み request への遅延 REQUEST_ERROR が no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);
    assert!(
        client
            .track_status_request(rid)
            .expect("track status entry が残ること")
            .response
            .is_none(),
        "吸収した応答で応答状態を確定しないこと"
    );

    // fetch (Pending) の遅延 FETCH_OK
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
    terminate_request_by_goaway_timeout(&mut client, rid, Vec::new());

    client
        .recv_stream_message(
            rid,
            ControlMessage::FetchOk(shiguredo_moqt::message::FetchOk {
                end_of_track: 0,
                end_location: Location {
                    group_id: 10,
                    object_id: 0,
                },
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("終端済み request への遅延 FETCH_OK が no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);
    while let Some(e) = client.poll_event() {
        assert!(
            !matches!(e, SessionEvent::FetchOkReceived { .. }),
            "吸収した FETCH_OK でイベントを発行しないこと"
        );
    }

    // subscription (Established) の遅延 PUBLISH_DONE
    let (mut client, _server, rid) = establish_subscribe_track(820);
    terminate_request_by_goaway_timeout(&mut client, rid, Vec::new());

    client
        .recv_stream_message(
            rid,
            ControlMessage::PublishDone(shiguredo_moqt::message::PublishDone {
                status_code: 0,
                stream_count: 0,
                reason: shiguredo_moqt::message::ReasonPhrase::new("done")
                    .expect("テストフィクスチャの前提条件を満たす"),
            }),
        )
        .expect("終端済み request への遅延 PUBLISH_DONE が no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);
    assert!(
        client
            .subscription(rid)
            .expect("subscription が残ること")
            .publish_done
            .is_none(),
        "吸収した PUBLISH_DONE で drain 状態を作らないこと"
    );
}

/// peer が正当な送信者である REQUEST_UPDATE / PUBLISH_STATE_NOTIFY は終端後も吸収する
///
/// draft-ietf-moq-transport-22 §9.5 (REQUEST_UPDATE) の 2 ケースは「request の送信側」と
/// 「PUBLISH 起点 subscription の subscriber」であり、§9.10 (PUBLISH_STATE_NOTIFY) は
/// publisher だけが送れる。いずれも peer が GOAWAY / reset を観測する前に送ったものであり、
/// 終端済み request へ届いてもセッションを閉じない。
#[test]
fn request_stream_goaway_timeout_absorbs_late_message_from_expected_sender() {
    // 自側が SUBSCRIBE の responder のとき、request の送信者 (peer) からの REQUEST_UPDATE
    let (_client, mut server, rid) = establish_subscribe_track(821);
    terminate_request_by_goaway_timeout(&mut server, rid, b"moqt://relay.example/".to_vec());
    server
        .recv_stream_message(
            rid,
            ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
                request_id: rid + 2,
                parameters: MessageParameters::new(),
            }),
        )
        .expect("request の送信者からの REQUEST_UPDATE が no-op で吸収されること");
    assert_eq!(server.state(), SessionState::Established);

    // 自側が subscriber の subscription へ publisher (peer) からの PUBLISH_STATE_NOTIFY
    let (mut client, _server, rid) = establish_subscribe_track(822);
    terminate_request_by_goaway_timeout(&mut client, rid, Vec::new());
    client
        .recv_stream_message(
            rid,
            ControlMessage::PublishStateNotify(shiguredo_moqt::message::PublishStateNotify {
                parameters: MessageParameters::new(),
            }),
        )
        .expect("publisher からの PUBLISH_STATE_NOTIFY が no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);
}

/// peer が正当な送信者でないメッセージは終端後も違反として扱う
///
/// 吸収は「ローカル終端済み request へ届いた、peer が正当に送り得るメッセージ」に限る。
/// 仕様が MUST close を定める組み合わせは従来どおりセッションを閉じる。
#[test]
fn request_stream_goaway_timeout_rejects_late_message_from_unexpected_sender() {
    // 自側が SUBSCRIBE の requester のとき、publisher (peer) からの REQUEST_UPDATE は
    // draft §9.5 の 2 ケース外
    let (mut client, _server, rid) = establish_subscribe_track(823);
    terminate_request_by_goaway_timeout(&mut client, rid, Vec::new());
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
                // peer (Server) が採番する Request ID は奇数 (draft-ietf-moq-transport-22
                // §6.4.2.1 (Request ID))。REQUEST_UPDATE 自身も Request ID を消費するため、
                // 検証を通る id を渡す
                request_id: 1,
                parameters: MessageParameters::new(),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);

    // 自側が PUBLISH を送った側のとき、subscriber (peer) からの PUBLISH_STATE_NOTIFY は
    // draft §9.10 の MUST に反する
    let (mut client, mut server) = establish_pair();
    let rid = establish_publish_sender_as_established(&mut client, &mut server, 824);
    terminate_request_by_goaway_timeout(&mut server, rid, b"moqt://relay.example/".to_vec());
    let err = server
        .recv_stream_message(
            rid,
            ControlMessage::PublishStateNotify(shiguredo_moqt::message::PublishStateNotify {
                parameters: MessageParameters::new(),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// deadline 満了で終端した request の 2 回目の close 通知は PROTOCOL_VIOLATION になる
///
/// no-op 吸収は `rejected_request_ids` から削除することで成立する。QUIC では FIN 送信後も
/// RESET_STREAM を送れるため (RFC 9000 §3.2) 同一 request stream への 2 回目の close 通知を
/// 抑止するのは I/O 層の責務であり、本テストは Session 側ライフサイクル (クローズ時削除) の
/// 回帰防御を意図する。
#[test]
fn request_stream_goaway_timeout_second_close_fails_as_unknown_id() {
    let (_client, mut server, rid) = establish_subscribe_track(805);
    server.tick(1_000);
    server
        .send_goaway_on_request_stream(rid, b"moqt://relay.example/".to_vec(), 100)
        .expect("request stream GOAWAY の送信に成功すること");
    let (_, _) = take_send_on_stream(&mut server);

    server.tick(1_100);
    while server.poll_event().is_some() {}
    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("1 回目の close は no-op で吸収されること");
    let err = server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// deadline が解除済みの request には満了時刻を過ぎても reset も終端通知も発行しない
///
/// requester が peer (responder) の FIN を受けた時点で request は終端し、reset deadline も
/// 解除される (`Session::recv_request_stream_closed`)。したがって期限到達後も
/// `terminate_request_on_goaway_timeout` は呼ばれず、二重の終端通知は発行されない。
#[test]
fn request_stream_goaway_timeout_is_not_reprocessed_after_deadline_cleared() {
    let (mut client, _server, rid) = establish_subscribe_track(806);
    client.tick(1_000);
    client
        .send_goaway_on_request_stream(rid, Vec::new(), 100)
        .expect("request stream GOAWAY の送信に成功すること");
    let (_, _) = take_send_on_stream(&mut client);
    // requester が peer (responder) の FIN を受けると request は即時に終端する
    client
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("responder の FIN の通知に成功すること");
    let mut terminated = 0;
    while let Some(e) = client.poll_event() {
        if matches!(e, SessionEvent::RequestTerminated { .. }) {
            terminated += 1;
        }
    }
    assert_eq!(terminated, 1, "requester の終端が 1 回だけ出ること");

    // FIN の終端経路で deadline は解除済みのため、期限到達後も reset も終端も出ない
    client.tick(1_100);
    while let Some(e) = client.poll_event() {
        assert!(
            !matches!(
                e,
                SessionEvent::ResetRequestStream { .. } | SessionEvent::RequestTerminated { .. }
            ),
            "既に終端した request へ reset も終端通知も発行しないこと"
        );
    }
}

/// REQUEST_UPDATE の吸収条件が draft §9.5 の 2 ケースと一致すること
///
/// 2 ケースは「request の送信側」と「PUBLISH 起点 subscription の subscriber」であり、
/// 自側から見ると「peer が initiator」または「自側が publisher」である。とくに PUBLISH を
/// 受信した側 (自側 subscriber、peer が PUBLISH の送信側) からの REQUEST_UPDATE は正当である。
#[test]
fn request_stream_goaway_timeout_absorbs_request_update_from_legitimate_sender() {
    // PUBLISH を受信した subscription (自側 subscriber、peer が request の送信側)
    let (mut client, mut server) = establish_pair();
    let rid = establish_publish_sender_as_established(&mut client, &mut server, 830);
    terminate_request_by_goaway_timeout(&mut client, rid, Vec::new());
    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
                // peer (Server) の次の Request ID は奇数
                request_id: rid + 2,
                parameters: MessageParameters::new(),
            }),
        )
        .expect("PUBLISH の送信側からの REQUEST_UPDATE が no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);

    // FETCH を受けた側 (自側 publisher、peer が request の送信側)
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
    terminate_request_by_goaway_timeout(&mut server, rid, b"moqt://relay.example/".to_vec());
    server
        .recv_stream_message(
            rid,
            ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
                // peer (Client) の次の Request ID は偶数
                request_id: rid + 2,
                parameters: MessageParameters::new(),
            }),
        )
        .expect("FETCH の送信側からの REQUEST_UPDATE が no-op で吸収されること");
    assert_eq!(server.state(), SessionState::Established);

    // TRACK_STATUS は draft §9.5 の 2 ケースに含まれないため従来どおり違反
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, ts_msg) = take_send_request(&mut client);
    server
        .recv_request(ts_msg)
        .expect("TRACK_STATUS の受信に成功すること");
    terminate_request_by_goaway_timeout(&mut server, rid, b"moqt://relay.example/".to_vec());
    let err = server
        .recv_stream_message(
            rid,
            ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
                request_id: rid + 2,
                parameters: MessageParameters::new(),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// 応答の内容に対する MUST close は吸収より優先される
///
/// 吸収は「ローカル終端済み request へ届いた応答」を対象にするが、メッセージ内容そのものの
/// 違反 (draft-ietf-moq-transport-22 §9.3 (REQUEST_OK) の Track Properties、
/// §9.4.1 (Redirect Structure) の Connect URI) は終端後も検出してセッションを閉じる。
#[test]
fn request_stream_goaway_timeout_does_not_absorb_invalid_response_content() {
    use shiguredo_moqt::track_properties::{TrackProperty, TrackPropertyValue};

    // TRACK_STATUS 以外の context へ Track Properties 付き REQUEST_OK
    let (_client, mut server, rid) = establish_subscribe_track(831);
    terminate_request_by_goaway_timeout(&mut server, rid, b"moqt://relay.example/".to_vec());
    let mut properties = TrackProperties::new();
    properties.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(1),
    });
    let err = server
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
                parameters: MessageParameters::new(),
                track_properties: properties,
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);

    // server が非 0 の Connect URI を持つ Redirect を受信
    let (_client, mut server, rid) = establish_subscribe_track(832);
    terminate_request_by_goaway_timeout(&mut server, rid, b"moqt://relay.example/".to_vec());
    let err = server
        .recv_stream_message(
            rid,
            ControlMessage::RequestError(shiguredo_moqt::message::RequestError {
                error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
                retry_interval: 0,
                reason: shiguredo_moqt::message::ReasonPhrase::new("redirect")
                    .expect("テストフィクスチャの前提条件を満たす"),
                redirect: Some(shiguredo_moqt::message::Redirect {
                    connect_uri: b"moqt://relay.example/".to_vec(),
                    track_namespace: ns(&[b"live"]),
                    track_name: b"cam".to_vec(),
                }),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);

    // 破棄済み request (state テーブルから除去済み) でも server の Redirect MUST は検証する
    let (_client, mut server, rid) = establish_subscribe_track(834);
    terminate_request_by_goaway_timeout(&mut server, rid, b"moqt://relay.example/".to_vec());
    assert!(
        server.forget_subscription(rid).is_some(),
        "終端済み subscription を破棄できること"
    );
    let err = server
        .recv_stream_message(
            rid,
            ControlMessage::RequestError(shiguredo_moqt::message::RequestError {
                error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
                retry_interval: 0,
                reason: shiguredo_moqt::message::ReasonPhrase::new("redirect")
                    .expect("テストフィクスチャの前提条件を満たす"),
                redirect: Some(shiguredo_moqt::message::Redirect {
                    connect_uri: b"moqt://relay.example/".to_vec(),
                    track_namespace: ns(&[b"live"]),
                    track_name: b"cam".to_vec(),
                }),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// 終端済み subscription への遅延 SUBSCRIBE_OK でも Track Alias 衝突は検出する
///
/// draft-ietf-moq-transport-22 §3.1.3 (Track Alias): 異なる Track の Established
/// subscription と同じ Track Alias を使う SUBSCRIBE_OK は DUPLICATE_TRACK_ALIAS で
/// セッションを閉じる MUST。終端済み request への遅延応答でも検証を省かない。
#[test]
fn request_stream_goaway_timeout_rejects_late_subscribe_ok_with_duplicate_track_alias() {
    let (mut client, mut server) = establish_pair();
    // 1 本目: "cam" を alias 77 で確立する
    let rid1 = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("1 本目の SUBSCRIBE の送信に成功すること");
    let (_, sub1) = take_send_request(&mut client);
    server
        .recv_request(sub1)
        .expect("1 本目の SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(
            rid1,
            77,
            MessageParameters::new(),
            TrackProperties::default(),
        )
        .expect("1 本目の SUBSCRIBE_OK の送信に成功すること");
    let (_, ok1) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid1, ok1)
        .expect("1 本目の SUBSCRIBE_OK の受信に成功すること");

    // 2 本目: "mic" を GOAWAY の deadline 満了で終端してから同じ alias の SUBSCRIBE_OK を届ける
    let rid2 = client
        .send_subscribe(ns(&[b"live"]), b"mic".to_vec(), MessageParameters::new())
        .expect("2 本目の SUBSCRIBE の送信に成功すること");
    let (_, sub2) = take_send_request(&mut client);
    server
        .recv_request(sub2)
        .expect("2 本目の SUBSCRIBE の受信に成功すること");
    terminate_request_by_goaway_timeout(&mut client, rid2, Vec::new());
    // 自側の送信 API は alias 衝突を送信前に拒否するため、受信経路の検証として
    // peer が送ってきた SUBSCRIBE_OK を直接渡す
    let err = client
        .recv_stream_message(
            rid2,
            ControlMessage::SubscribeOk(shiguredo_moqt::message::SubscribeOk {
                track_alias: 77,
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect_err("終端済み request でも Track Alias 衝突は拒否されること");
    assert_eq!(err.code, SESSION_DUPLICATE_TRACK_ALIAS);
}

/// 終端済み TRACK_STATUS への遅延 REQUEST_OK でも context 別スコープを検証する
///
/// draft-ietf-moq-transport-22 §9.20.1 (Parameter Scope): TRACK_STATUS_OK で許可されるのは
/// LARGEST_OBJECT のみであり、EXPIRES を含む REQUEST_OK は MUST でセッションを閉じる。
#[test]
fn request_stream_goaway_timeout_rejects_late_track_status_ok_with_invalid_scope() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, ts_msg) = take_send_request(&mut client);
    server
        .recv_request(ts_msg)
        .expect("TRACK_STATUS の受信に成功すること");
    terminate_request_by_goaway_timeout(&mut client, rid, Vec::new());

    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
                parameters: expires_params(1_000),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect_err("TRACK_STATUS_OK で許可されない EXPIRES は拒否されること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// PUBLISH 起点 subscription の subscriber 側でも遅延 REQUEST_UPDATE_OK でセッションを閉じない
///
/// draft-ietf-moq-transport-22 §9.5 (REQUEST_UPDATE): PUBLISH 起点の subscription では
/// subscriber も REQUEST_UPDATE を送れる。その応答 (REQUEST_UPDATE_OK) が self の reset を
/// peer が観測する前に届く場合、終端済みでも MUST 検証だけを行って状態遷移せず受理する。
#[test]
fn request_stream_goaway_timeout_absorbs_late_request_ok_for_publish_origin_subscription() {
    let (mut client, mut server) = establish_pair();
    // server = publisher が PUBLISH を送り、client = subscriber が PUBLISH_OK を返して確立する
    let rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            836,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("PUBLISH の送信に成功すること");
    let (_, pub_msg) = take_send_request(&mut server);
    client
        .recv_request(pub_msg)
        .expect("PUBLISH の受信に成功すること");
    client
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("PUBLISH を受けた subscriber は PUBLISH_OK を返せること");
    let (_, pubok_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, pubok_msg)
        .expect("PUBLISH_OK の受信に成功すること");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established,
        "PUBLISH_OK の送信で subscription が確立すること"
    );
    // PUBLISH 起点 subscription の subscriber も REQUEST_UPDATE を送れる (draft §9.5)
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("PUBLISH 起点 subscription の subscriber は REQUEST_UPDATE を送れること");
    while client.poll_event().is_some() {}

    terminate_request_by_goaway_timeout(&mut client, rid, Vec::new());
    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("遅延した REQUEST_UPDATE_OK が no-op で吸収されること");
    assert_eq!(
        client.state(),
        SessionState::Established,
        "遅延応答でセッションを閉じないこと"
    );
}

/// 終端済みでも自側 publisher への遅延 REQUEST_OK は拒否する
///
/// draft-ietf-moq-transport-22 §9.5 (REQUEST_UPDATE): REQUEST_UPDATE を送れるのは request の
/// 送信側と PUBLISH 起点 subscription の subscriber だけである。自側が SUBSCRIBE を受けた
/// publisher である場合、peer からの REQUEST_OK は正当な応答になり得ないため、
/// 終端済みでも従来どおり PROTOCOL_VIOLATION で閉じる。
#[test]
fn request_stream_goaway_timeout_rejects_late_request_ok_for_responder_publisher() {
    let (_client, mut server, rid) = establish_subscribe_track(837);
    terminate_request_by_goaway_timeout(&mut server, rid, b"moqt://relay.example/".to_vec());
    let err = server
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect_err("自側 publisher への遅延 REQUEST_OK は拒否されること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// 終端済みでも自側 publisher の遅延 REQUEST_OK を受理する (context 復元不能による残差)
///
/// 自側が PUBLISH を送った (publisher 役の initiator) subscription を GOAWAY の deadline 満了で
/// 終端すると、draft-ietf-moq-transport-22 §9.20.1 (Parameter Scope) の context
/// (PUBLISH_OK / REQUEST_UPDATE_OK) が state から失われる。どちらの context でも許可されない
/// パラメータだけを違反とするため、PUBLISH_OK context では許可されない LARGEST_OBJECT でも
/// 受理する (残差。詳細は `Session::handle_ok_for_subscription` のコメントを参照)。
#[test]
fn request_stream_goaway_timeout_absorbs_late_publish_ok_with_largest_object() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
    };

    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            900,
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
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established,
        "PUBLISH_OK で subscription が確立すること"
    );

    terminate_request_by_goaway_timeout(&mut client, rid, Vec::new());
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 1,
            object: 2,
        },
    });
    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
                parameters: params,
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("終端済み request への遅延 REQUEST_OK が no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);
}
