//! REQUEST_UPDATE / REQUEST_OK / REQUEST_ERROR のテスト

use super::*;

/// Client (subscriber) → Server (publisher) の SUBSCRIBE/OK 確立後に
/// REQUEST_UPDATE → REQUEST_OK のフルサイクルが両端で動作する
#[test]
fn request_update_full_cycle() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    recv_response_helper(&mut client, rid, ok_msg);

    // Client から REQUEST_UPDATE で FORWARD=0
    let mut params = MessageParameters::new();
    params.push(shiguredo_moqt::message_parameter::MessageParameter {
        param_type: shiguredo_moqt::message_parameter::PARAM_FORWARD,
        value: shiguredo_moqt::message_parameter::MessageParameterValue::Uint8(0),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // draft-ietf-moq-transport-21 Appendix A.4 (Since draft-ietf-moq-transport-17) #1540: REQUEST_UPDATE のパラメータは即座に適用されず、
    // pending_update_params に蓄積される。REQUEST_OK 応答後に forward_state が反映される。
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .pending_update_params
            .is_some()
    );

    // Server が REQUEST_OK 応答
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, reqok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, reqok)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established
    );
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        0
    );
}

/// REQUEST_UPDATE への REQUEST_OK で EXPIRES=0 が既存の期限をクリアする
#[test]
fn request_ok_with_expires_zero_clears_existing_expires() {
    // EXPIRES=0 は「期限なし」を意味し、REQUEST_UPDATE_OK 経路でも既存の期限を
    // None にクリアする。draft-ietf-moq-transport-21 §9.20.17
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, expires_params(300), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    recv_response_helper(&mut client, rid, ok_msg);
    assert!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .expires
            .is_some()
    );

    // Client から REQUEST_UPDATE（EXPIRES なし）を送信
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // Server が EXPIRES=0 を含む REQUEST_OK 応答
    server
        .send_request_ok(rid, expires_params(0), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, reqok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, reqok)
        .expect("テストフィクスチャの前提条件を満たす");

    assert!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .expires
            .is_none()
    );
}

/// REQUEST_UPDATE への REQUEST_OK で LARGEST_OBJECT parameter が
/// subscription.largest_location に保存される (draft-ietf-moq-transport-21 §3.1 (Subscriptions) / draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter) / draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions))
#[test]
fn request_ok_largest_object_updates_largest_location() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location
            .is_none()
    );

    // Client から REQUEST_UPDATE を送る
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // Server が REQUEST_OK に LARGEST_OBJECT を含めて応答
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 42,
            object: 7,
        },
    });
    server
        .send_request_ok(rid, params, TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, reqok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, reqok)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location,
        Some(Location {
            group_id: 42,
            object_id: 7,
        })
    );
}

/// REQUEST_OK に LARGEST_OBJECT が含まれない場合、既存の largest_location は変更されない
#[test]
fn request_ok_without_largest_object_leaves_largest_location() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // SUBSCRIBE_OK 側で largest_location をセット
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 5,
            object: 0,
        },
    });
    server
        .send_subscribe_ok(rid, 1, ok_params, TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location,
        Some(Location {
            group_id: 5,
            object_id: 0,
        })
    );
    // REQUEST_UPDATE → REQUEST_OK (parameters 無し) で largest_location は保持される
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, reqok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, reqok)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location,
        Some(Location {
            group_id: 5,
            object_id: 0,
        })
    );
}

/// draft-ietf-moq-transport-21 §9.7 (SUBSCRIBE_OK) / §9.6 (SUBSCRIBE): SUBSCRIBE 初回応答は SUBSCRIBE_OK であり
/// REQUEST_OK ではない。Pending(Subscriber) 状態の subscription に対して peer が REQUEST_OK を送ってきたら
/// PROTOCOL_VIOLATION で session を閉じる。
#[test]
fn peer_request_ok_for_subscription_in_pending_subscriber_closes_session() {
    use shiguredo_moqt::message::RequestOk;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // client はまだ Pending(Subscriber) のまま (SUBSCRIBE_OK 未受信)。
    // SUBSCRIBE_OK ではなく不正に REQUEST_OK が飛んできたと仮定する。
    let reqok = ControlMessage::RequestOk(RequestOk {
        parameters: MessageParameters::new(),
        track_properties: TrackProperties::new(),
    });
    let err = client.recv_stream_message(rid, reqok).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    match drain_until_close(&mut client) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        _ => unreachable!(),
    }
}

/// Pending(Publisher) 状態 (自側 PUBLISH 後、REQUEST_OK (publish context) 未受信) で peer が
/// draft-ietf-moq-transport-21 §9.3 (REQUEST_OK): REQUEST_OK が PUBLISH_OK を統合。Pending(Publisher) で REQUEST_OK を受信すると
/// Established に遷移する。
#[test]
fn peer_request_ok_for_subscription_in_pending_publisher_establishes() {
    use shiguredo_moqt::message::RequestOk;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            100,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let reqok = ControlMessage::RequestOk(RequestOk {
        parameters: MessageParameters::new(),
        track_properties: TrackProperties::default(),
    });
    client
        .recv_stream_message(rid, reqok)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established
    );
}

/// 自側が publisher responder で Pending(Subscriber) 状態のときに
/// REQUEST_OK を送ろうとすると PROTOCOL_VIOLATION。初回 SUBSCRIBE の成功応答は
/// SUBSCRIBE_OK を使うべきで、REQUEST_OK は REQUEST_UPDATE 専用。
#[test]
fn send_request_ok_for_subscription_in_pending_subscriber_errors() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// draft-ietf-moq-transport-21 §9.3 (REQUEST_OK): subscriber responder は Pending(Publisher) 状態で REQUEST_OK を送信できる
/// (旧 PUBLISH_OK 相当)。Pending → Established に遷移する。
#[test]
fn send_request_ok_for_subscription_in_pending_publisher_establishes() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            200,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established
    );
}

/// draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE) / draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions): REQUEST_UPDATE への REQUEST_ERROR は
/// Established から Terminated に遷移し、PUBLISH_DONE(UPDATE_FAILED) が
/// REQUEST_ERROR 直後に自動送信される
#[test]
fn send_request_error_for_subscription_in_established_transitions_to_terminated() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 両端 Established
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established
    );

    // Server が REQUEST_UPDATE 失敗応答として REQUEST_ERROR を送る
    server
        .send_request_error(
            rid,
            0x01,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("update failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    // Terminated に遷移していること
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated
    );
    // REQUEST_ERROR と PUBLISH_DONE(UPDATE_FAILED) の 2 つが SendOnStream に push される
    // REQUEST_ERROR には FIN が付かず、最終の PUBLISH_DONE に FIN が付く (§6.4.2.3 / §9.9)
    let mut events = Vec::new();
    while let Some(e) = server.poll_event() {
        if let SessionEvent::SendOnStream {
            request_id,
            message,
            fin,
        } = e
        {
            assert_eq!(request_id, rid);
            events.push((message, fin));
        }
    }
    assert_eq!(
        events.len(),
        2,
        "REQUEST_ERROR と PUBLISH_DONE が発行されること"
    );
    let (err_msg, err_fin) = &events[0];
    assert!(
        matches!(err_msg, ControlMessage::RequestError(_)),
        "最初の SendOnStream は REQUEST_ERROR であること"
    );
    assert!(
        !err_fin,
        "PUBLISH_DONE が続く REQUEST_ERROR には FIN が付かないこと"
    );
    let (done_msg, done_fin) = &events[1];
    assert!(done_fin, "最終の PUBLISH_DONE には FIN が付くこと");
    match done_msg {
        ControlMessage::PublishDone(done) => {
            // draft §9.5.1: status_code は UPDATE_FAILED (0x8)
            assert_eq!(
                done.status_code,
                shiguredo_moqt::error::PUBLISH_DONE_UPDATE_FAILED
            );
            // stream_count は published_stream_count (このテストでは 0)
            assert_eq!(done.stream_count, 0);
        }
        other => panic!("PUBLISH_DONE が期待されたが {other:?} を受け取った"),
    }
}

/// Established 状態で peer から REQUEST_ERROR (REQUEST_UPDATE 失敗応答) を受けると
/// subscription は Terminated に遷移し、RequestErrorReceived イベントが発行される。
///
/// draft-ietf-moq-transport-21 §3.1.1 (Subscription State Management): "A subscriber
/// keeps subscription state until it cancels the request (see Section 6.4.2.3), or until receipt
/// of a PUBLISH_DONE or REQUEST_ERROR." により、REQUEST_UPDATE 失敗応答の REQUEST_ERROR も
/// subscription state を終える条件に該当する。
/// 遷移後の帰結として、`send_request_update` は Established 要求のためエラーを返し
/// (再 REQUEST_UPDATE の窓が閉じる)、`forget_subscription` で cleanup 可能になる。
#[test]
fn peer_request_error_for_subscription_in_established_transitions_to_terminated_and_emits_event() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // Client (initiator) が REQUEST_UPDATE を送る
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // Server が REQUEST_ERROR で失敗応答
    server
        .send_request_error(
            rid,
            0x42,
            100,
            shiguredo_moqt::message::ReasonPhrase::new("update failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, err_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, err_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // Client 側の subscription が Terminated に遷移していること (draft §3.1.1)
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated
    );
    // RequestErrorReceived イベントが発行されていること
    let mut seen = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::RequestErrorReceived {
            request_id,
            error_code,
            retry_interval,
            reason,
            redirect,
        } = e
        {
            assert_eq!(request_id, rid);
            assert_eq!(error_code, 0x42);
            assert_eq!(retry_interval, 100);
            assert_eq!(reason.as_str(), "update failed");
            assert!(redirect.is_none());
            seen = true;
        }
    }
    assert!(seen, "RequestErrorReceived イベントが期待された");

    // 遷移後の再 REQUEST_UPDATE は Established 要求でエラーを返すこと (窓の解消)
    let err = client
        .send_request_update(rid, MessageParameters::new())
        .expect_err("Terminated 遷移後の send_request_update は失敗するはず");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);

    // Terminated かつ publish_done=None なので forget_subscription で cleanup 可能
    assert_eq!(client.subscription_cleanup_ready(rid), Some(true));
    assert!(client.forget_subscription(rid).is_some());
}

/// REQUEST_UPDATE 失敗応答の REQUEST_ERROR で Terminated に遷移した後、publisher が
/// draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions) の MUST に基づいて送る
/// PUBLISH_DONE(UPDATE_FAILED) を subscriber 側が受理し、`PublishDoneReceived` イベントを
/// 発行してセッションを fail しないこと。
#[test]
fn subscription_terminated_by_peer_request_error_accepts_subsequent_publish_done() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // Client → REQUEST_UPDATE → Server が REQUEST_ERROR
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_error(
            rid,
            0x01,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("update failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    // REQUEST_ERROR と PUBLISH_DONE(UPDATE_FAILED) を順に client に配送
    let (_, err_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, err_msg)
        .expect("REQUEST_ERROR 受信は成功するはず");
    // Terminated に遷移済み・ publish_done は未記録
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated
    );

    let (_, done_msg) = take_send_on_stream(&mut server);
    // draft-ietf-moq-transport-21 §9.5.1 の MUST で publisher が送る後続 PUBLISH_DONE を
    // Terminated 状態でも受理し、セッションを fail させないこと
    client
        .recv_stream_message(rid, done_msg)
        .expect("Terminated + publish_done=None の PUBLISH_DONE は受理されるはず");
    // PublishDoneReceived イベントが発行されていること
    let mut seen_publish_done = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::PublishDoneReceived {
            request_id,
            status_code,
            ..
        } = e
        {
            assert_eq!(request_id, rid);
            assert_eq!(
                status_code,
                shiguredo_moqt::error::PUBLISH_DONE_UPDATE_FAILED
            );
            seen_publish_done = true;
        }
    }
    assert!(
        seen_publish_done,
        "PublishDoneReceived イベントが期待された"
    );

    // 重複 PUBLISH_DONE (publish_done が Some になった Terminated 状態) は
    // 従来どおり PROTOCOL_VIOLATION で拒否され、セッションが Closing に遷移すること。
    // これにより、`handle_peer_publish_done` の Terminated 受理条件が
    // 「publish_done が None」に限定されている境界が壊れていないことを検証する。
    let duplicate_done = ControlMessage::PublishDone(shiguredo_moqt::message::PublishDone {
        status_code: shiguredo_moqt::error::PUBLISH_DONE_UPDATE_FAILED,
        stream_count: 0,
        reason: shiguredo_moqt::message::ReasonPhrase::new("")
            .expect("テストフィクスチャの前提条件を満たす"),
    });
    let err = client
        .recv_stream_message(rid, duplicate_done)
        .expect_err("重複 PUBLISH_DONE は PROTOCOL_VIOLATION になること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert!(matches!(
        client.state(),
        SessionState::Closing | SessionState::Closed
    ));
}

/// REQUEST_UPDATE 失敗応答で Terminated に遷移した subscription に、
/// (a) 遷移前に開かれた既存 subgroup stream があり、その後に (b) publisher が §9.5.1 の
/// MUST に基づいて送る PUBLISH_DONE(UPDATE_FAILED) を受理する経路で、既存 stream の
/// FIN 受信後に `forget_subscription` が可能になることを検証する。
///
/// Terminated + publish_done=None の窓に既存 subgroup stream が残っているとき、
/// stream 終端での計数デクリメント漏れがあると
/// `cleanup_ready` が永久に false のまま張り付き subscription がリークする。
#[test]
fn subscription_terminated_by_peer_request_error_forgets_after_stream_fin_and_publish_done() {
    let (mut client, mut server) = establish_pair();
    // client (subscriber) → server (publisher)
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let track_alias = 700;
    server
        .send_subscribe_ok(
            rid,
            track_alias,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // client 側で既存 subgroup stream を 1 本開いておく
    let stream_id = DataStreamId(31);
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    let header = SubgroupHeader {
        track_alias,
        group_id: 3,
        subgroup_id: SubgroupIdMode::Explicit(7),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &header)
            .expect("SUBGROUP_HEADER の受信に成功すること"),
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

    // client → REQUEST_UPDATE → server が REQUEST_ERROR
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_error(
            rid,
            0x01,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("update failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, err_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, err_msg)
        .expect("REQUEST_ERROR 受信は成功するはず");
    // Terminated + publish_done=None の窓では REQUEST_ERROR 受信で subscription が残る
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が残ること")
            .state,
        SubscriptionState::Terminated
    );

    // 既存 stream にキャンセル由来 Terminated の subscription へ届いた object は、
    // stream を Subgroup variant のまま候補評価で Discarded として吸収する
    // (Discarded variant への置き換えは行わず、subscription スコープの状態も更新しない)。
    // open_incoming_subgroup_count は object 受信では変更せず、stream 終端で戻す。
    let obj = DecodedSubgroupObject {
        object_id: 0,
        payload_length: 1,
        status: None,
        properties_bytes: None,
    };
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &obj)
            .expect("キャンセル由来 Terminated への object は no-op で吸収されること"),
        TrackDataAcceptance::Discarded,
        "キャンセル由来 Terminated への object は候補評価で Discarded として吸収されること"
    );

    // 既存 stream の FIN。キャンセル由来 Terminated の所有者の stream は帰属実績が
    // 無いため no-op 吸収経路で open_incoming_subgroup_count を戻す。
    // デクリメント漏れがあると cleanup_ready が永久に false になる。
    // 最終の cleanup_ready 成立自体が計数の裏付けになるためここでは検証しない
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("キャンセル由来 Terminated の FIN は no-op で吸収されること");

    // publisher が続けて送る PUBLISH_DONE(UPDATE_FAILED)
    let (_, done_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("Terminated + publish_done=None の PUBLISH_DONE は受理されるはず");

    // drain timer を 0 に張ってから tick して満了させ、cleanup_ready を true にする
    // (delivery timeout が未設定なら drain_timeout_ms=0 になり即満了)
    client.tick(1);
    // stream 終端で計数が 0、PUBLISH_DONE 受信で drain 開始・満了で cleanup_ready
    assert_eq!(client.subscription_cleanup_ready(rid), Some(true));
    assert!(client.forget_subscription(rid).is_some());
}

/// Established で REQUEST_ERROR を送ると PUBLISH_DONE(UPDATE_FAILED) が自動送信され、
/// 後続の手動 send_publish_done は Terminated 状態のため失敗する
/// (draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions))。
#[test]
fn publish_done_auto_sent_after_request_error_in_established() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // Client → REQUEST_UPDATE → Server が REQUEST_ERROR
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_error(
            rid,
            0x01,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("update failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // send_request_error により Terminated に遷移していること
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated
    );
    // REQUEST_ERROR と PUBLISH_DONE(UPDATE_FAILED) が自動送信されていること
    let (_, err_msg) = take_send_on_stream(&mut server);
    assert!(
        matches!(err_msg, ControlMessage::RequestError(_)),
        "最初の SendOnStream は REQUEST_ERROR であること"
    );
    let (_, done_msg) = take_send_on_stream(&mut server);
    match done_msg {
        ControlMessage::PublishDone(done) => {
            assert_eq!(
                done.status_code,
                shiguredo_moqt::error::PUBLISH_DONE_UPDATE_FAILED
            );
        }
        other => panic!("PUBLISH_DONE が期待されたが {other:?} を受け取った"),
    }
    // 後続の手動 send_publish_done は Terminated 状態のため失敗する
    let err = server
        .send_publish_done(
            rid,
            0x06,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("update_failed")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// `Terminated` 状態の subscription への `send_subgroup_object` が
/// `Err(SESSION_PROTOCOL_VIOLATION)` を返し、内部状態を汚染しないこと
///
/// draft-ietf-moq-transport-21 §3.1.1 (Subscription State Management):
/// "Objects MUST NOT be sent for requests that end with an error." REQUEST_UPDATE 失敗応答として
/// `send_request_error` を送ると subscription は `Terminated` に遷移するが、outgoing stream は
/// `forget_subscription` まで `data_streams.outgoing` に残るため、`Terminated` 後の送信を
/// `send_subgroup_header` と同じエラー種別・メッセージの Established 検証で拒否する。
/// `SubgroupIdMode::FirstObjectId` で stream を開いてから拒否された送信を行い、
/// REQUEST_UPDATE 失敗応答で open 中の outgoing subgroup stream がある場合、PUBLISH_DONE
/// が保留され、全 stream 終端後に自動送信されること
///
/// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE): "A sender MUST NOT send PUBLISH_DONE
/// until it has closed all streams it will ever open, and has no further datagrams to send,
/// for a subscription." (§3.1.1 も同旨)。§9.5.1 の MUST (REQUEST_UPDATE 失敗時、publisher
/// は PUBLISH_DONE (UPDATE_FAILED) を送る) と両立させるため、open 中の outgoing subgroup
/// stream がある間は push を保留し (`pending_publish_done`)、全 stream 終端後に自動送信
/// する。Terminated 後の `send_subgroup_object` 拒否 + 内部状態非汚染 (検証位置が
/// FirstObjectId 解決より後へ後退した場合の回帰防御) も併せて検証する。
/// PUBLISH_DONE の stream_count 検証は、stream が「閉じた後 (push 時)」のケースであり、
/// 「open のまま (保留)」のケースを扱う `pending_publish_done_discarded_on_forget_subscription`
/// とは対象が異なる (published_count はどちらのケースでも同じ 1)。
#[test]
fn send_subgroup_object_to_terminated_subscription_rejected() {
    use shiguredo_moqt::message::ReasonPhrase;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // FirstObjectId モードで outgoing stream を開く
    let stream_id = DataStreamId(160);
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 0,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(128),
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

    // Client → REQUEST_UPDATE → Server が REQUEST_ERROR (失敗応答) → Terminated
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_error(
            rid,
            0x42,
            0,
            ReasonPhrase::new("update failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated,
        "REQUEST_UPDATE 失敗応答で subscription が Terminated に遷移すること"
    );
    // 保留確認: REQUEST_ERROR のみ push され、PUBLISH_DONE は保留される (open stream があるため)
    let (_, err_msg) = take_send_on_stream(&mut server);
    assert!(matches!(err_msg, ControlMessage::RequestError(_)));
    let mut done_pushed = false;
    while let Some(ev) = server.poll_event() {
        if matches!(
            ev,
            SessionEvent::SendOnStream {
                message: ControlMessage::PublishDone(_),
                ..
            }
        ) {
            done_pushed = true;
        }
    }
    assert!(
        !done_pushed,
        "open stream がある間は PUBLISH_DONE が保留されること (§9.9 の MUST NOT)"
    );
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .pending_publish_done,
        Some(1),
        "保留情報に stream_count (published_count=1) が記録されること"
    );

    // 拒否確認 (stream が残っているうちに行う。forget_subscription を呼ぶと stream が
    // 除去され偽陽性になるため)
    let err = server
        .send_subgroup_object(stream_id, 5, None)
        .expect_err("Terminated 状態の subscription への送信は拒否されること");
    let err = err.as_session_error().expect("SessionError が得られること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        err.reason, "outgoing subgroup stream requires Established subscription",
        "send_subgroup_header と同じエラー文言であること"
    );
    // FirstObjectId 解決が実行されていないこと (検証位置の回帰防御)
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .stream_counts
            .published_count,
        1,
        "拒否された送信で published_count が増えないこと"
    );
    // largest_received_location が更新されていないこと
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_received_location,
        None,
        "拒否された送信で largest_received_location が更新されないこと"
    );
    // stream はまだ残っている (forget 前)
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .stream_counts
            .published_count,
        1,
        "拒否後も stream は残っていること"
    );
    // ローカル API の検証拒否なのでセッションは closing しない
    assert_ne!(
        server.state(),
        SessionState::Closing,
        "ローカル検証の拒否でセッションが閉じないこと"
    );

    // stream 終端 (FIN) → 保留していた PUBLISH_DONE (UPDATE_FAILED) が自動 push される
    server
        .send_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, done_msg) = take_send_on_stream(&mut server);
    match done_msg {
        ControlMessage::PublishDone(done) => {
            assert_eq!(
                done.status_code,
                shiguredo_moqt::error::PUBLISH_DONE_UPDATE_FAILED,
                "全 stream 終端後に PUBLISH_DONE(UPDATE_FAILED) が自動送信されること"
            );
            assert_eq!(
                done.stream_count, 1,
                "この subscription のために開いた subgroup stream が 1 本 (published_count=1) であること"
            );
        }
        other => panic!("PUBLISH_DONE が期待されたが {other:?} を受け取った"),
    }
    // 保留情報が push と同時にクリアされること (二重 push の回帰ガード)
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .pending_publish_done,
        None,
        "push と同時に保留情報がクリアされること"
    );
    // 追加の PublishDone push がないこと
    let mut extra_push = false;
    while let Some(ev) = server.poll_event() {
        if matches!(
            ev,
            SessionEvent::SendOnStream {
                message: ControlMessage::PublishDone(_),
                ..
            }
        ) {
            extra_push = true;
        }
    }
    assert!(
        !extra_push,
        "保留情報クリア後は追加の PUBLISH_DONE が push されないこと"
    );
}

/// アプリが stream を閉じないまま `forget_subscription` を呼んだ場合、保留中の
/// PUBLISH_DONE が push されずに破棄されること
///
/// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE): "A sender MUST NOT send PUBLISH_DONE
/// until it has closed all streams it will ever open..." の MUST NOT により、stream が
/// 閉じるまで PUBLISH_DONE を送れない。stream を閉じないまま subscription を破棄した
/// 場合は、§9.5.1 の MUST (REQUEST_UPDATE 失敗時、publisher は PUBLISH_DONE (UPDATE_FAILED)
/// を送る) が果たせない帰結として、保留中の PUBLISH_DONE も push されずに破棄される。
#[test]
fn pending_publish_done_discarded_on_forget_subscription() {
    use shiguredo_moqt::message::ReasonPhrase;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // FirstObjectId モードで outgoing stream を開く
    let stream_id = DataStreamId(161);
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 0,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(stream_id, rid, &header)
        .expect("テストフィクスチャの前提条件を満たす");

    // Client → REQUEST_UPDATE → Server が REQUEST_ERROR (失敗応答) → Terminated (保留)
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_error(
            rid,
            0x42,
            0,
            ReasonPhrase::new("update failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, err_msg) = take_send_on_stream(&mut server);
    assert!(matches!(err_msg, ControlMessage::RequestError(_)));
    // 保留が記録されていること
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .pending_publish_done,
        Some(1),
        "REQUEST_UPDATE 失敗応答で PUBLISH_DONE が保留されること"
    );
    // stream を閉じないまま forget_subscription を呼ぶと、保留中の PUBLISH_DONE は
    // push されずに破棄される
    let sub = server
        .forget_subscription(rid)
        .expect("Terminated な subscription は forget できること");
    assert_eq!(
        sub.pending_publish_done, None,
        "forget した subscription の保留情報は破棄されること"
    );
    // PublishDone が push されていないこと
    let mut done_pushed = false;
    while let Some(ev) = server.poll_event() {
        if matches!(
            ev,
            SessionEvent::SendOnStream {
                message: ControlMessage::PublishDone(_),
                ..
            }
        ) {
            done_pushed = true;
        }
    }
    assert!(
        !done_pushed,
        "stream 未終端のままの forget では保留 PUBLISH_DONE が push されないこと"
    );
}

/// PUBLISH 経路: publisher (client) が REQUEST_UPDATE 失敗応答として REQUEST_ERROR を送ると、
/// PUBLISH_DONE(UPDATE_FAILED) が REQUEST_ERROR 直後に自動送信される
/// (draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions))
#[test]
fn send_request_error_for_publish_path_in_established_sends_publish_done() {
    let (mut client, mut server) = establish_pair();
    // Client が publisher として PUBLISH 送信
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam1".to_vec(),
            111,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // Server (subscriber) が REQUEST_OK で確立
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established
    );

    // Server (subscriber) が REQUEST_UPDATE を送信
    server
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // Client (publisher) が REQUEST_ERROR で失敗応答
    client
        .send_request_error(
            rid,
            0x01,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("update failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // Terminated に遷移していること
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated
    );
    // ワイヤ順序: REQUEST_ERROR → PUBLISH_DONE(UPDATE_FAILED)
    let (_, err_msg) = take_send_on_stream(&mut client);
    assert!(
        matches!(err_msg, ControlMessage::RequestError(_)),
        "最初の SendOnStream は REQUEST_ERROR であること"
    );
    let (_, done_msg) = take_send_on_stream(&mut client);
    match done_msg {
        ControlMessage::PublishDone(done) => {
            assert_eq!(
                done.status_code,
                shiguredo_moqt::error::PUBLISH_DONE_UPDATE_FAILED
            );
            // subgroup stream を開いていないので stream_count は 0
            assert_eq!(done.stream_count, 0);
        }
        other => panic!("PUBLISH_DONE が期待されたが {other:?} を受け取った"),
    }
}

/// published_stream_count > 0 の場合、PUBLISH_DONE(UPDATE_FAILED) の stream_count に
/// publisher が実際に開いた stream 数が反映される
/// (draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions) / §9.9 (PUBLISH_DONE))
#[test]
fn send_request_error_in_established_with_open_streams_uses_published_stream_count() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // publisher (server) が subgroup stream を 1 本開いて閉じる
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let stream_id = DataStreamId(100);
    server
        .send_subgroup_header(stream_id, rid, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .stream_counts
            .published_count,
        1
    );

    // Client → REQUEST_UPDATE → Server が REQUEST_ERROR
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_error(
            rid,
            0x01,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("update failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // Terminated に遷移していること
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated
    );
    // REQUEST_ERROR → PUBLISH_DONE(UPDATE_FAILED, stream_count=1) の順
    let (_, err_msg) = take_send_on_stream(&mut server);
    assert!(
        matches!(err_msg, ControlMessage::RequestError(_)),
        "最初の SendOnStream は REQUEST_ERROR であること"
    );
    let (_, done_msg) = take_send_on_stream(&mut server);
    match done_msg {
        ControlMessage::PublishDone(done) => {
            assert_eq!(
                done.status_code,
                shiguredo_moqt::error::PUBLISH_DONE_UPDATE_FAILED
            );
            // 開いた subgroup stream は 1 本なので stream_count は 1
            assert_eq!(done.stream_count, 1);
        }
        other => panic!("PUBLISH_DONE が期待されたが {other:?} を受け取った"),
    }
}

/// 非回帰: Pending 状態での REQUEST_ERROR 送信は Terminated に遷移するが
/// PUBLISH_DONE は送信されない (初回 request 失敗応答のため)
#[test]
fn send_request_error_for_subscription_in_pending_does_not_send_publish_done() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // Server はまだ Pending (SUBSCRIBE_OK 未送信)

    // Server が初回 request 失敗応答として REQUEST_ERROR を送る
    server
        .send_request_error(
            rid,
            0x10,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("nope".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // Terminated に遷移していること
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated
    );
    // SendOnStream は REQUEST_ERROR の 1 件のみ (PUBLISH_DONE は送信されない)
    let (_, err_msg) = take_send_on_stream(&mut server);
    assert!(
        matches!(err_msg, ControlMessage::RequestError(_)),
        "SendOnStream は REQUEST_ERROR であること"
    );
    // 2 件目の SendOnStream がないこと (PUBLISH_DONE なし)
    let mut extra = false;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::SendOnStream { .. }) {
            extra = true;
        }
    }
    assert!(
        !extra,
        "Pending からの REQUEST_ERROR では PUBLISH_DONE は送信されない"
    );
}

/// 非回帰: Terminated 状態での REQUEST_ERROR 送信は SESSION_PROTOCOL_VIOLATION
#[test]
fn send_request_error_for_subscription_in_terminated_errors() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // REQUEST_ERROR で Pending → Terminated に遷移
    server
        .send_request_error(
            rid,
            0x10,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("nope".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated
    );
    // 2 回目の REQUEST_ERROR 送信は PROTOCOL_VIOLATION
    let err = server
        .send_request_error(
            rid,
            0x10,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("again".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// Pending* での REQUEST_ERROR 受信でも RequestErrorReceived イベントが発行される
#[test]
fn request_error_received_event_emitted_for_pending_subscription() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_error(
            rid,
            0x05,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("nope".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, err_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, err_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut seen = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::RequestErrorReceived { request_id, .. } = e
            && request_id == rid
        {
            seen = true;
        }
    }
    assert!(
        seen,
        "Pending* で RequestErrorReceived イベントが期待された"
    );
}

/// draft-ietf-moq-transport-21 §3.1 (Subscriptions):
/// REQUEST_UPDATE は Established 状態の self loop として定義されており、
/// Pending(Subscriber) (自側 SUBSCRIBE 送信後 SUBSCRIBE_OK 未受信) からは送信できない。
#[test]
fn send_request_update_in_pending_subscriber_errors() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, m) = take_send_request(&mut client);
    server
        .recv_request(m)
        .expect("テストフィクスチャの前提条件を満たす");
    // SUBSCRIBE_OK は返さない。client は Pending(Subscriber) 状態のまま。
    assert!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_pending_subscriber()
    );
    let err = client
        .send_request_update(rid, MessageParameters::new())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // ローカル API バリデーションなので session は closing しない
    assert_ne!(client.state(), SessionState::Closing);
}

/// Pending(Publisher) (自側 PUBLISH 送信後 REQUEST_OK (publish context) 未受信) からも REQUEST_UPDATE は送れない。
#[test]
fn send_request_update_in_pending_publisher_errors() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            1,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, m) = take_send_request(&mut client);
    server
        .recv_request(m)
        .expect("テストフィクスチャの前提条件を満たす");
    // REQUEST_OK (publish context) は返さない。client は Pending(Publisher) 状態のまま。
    assert!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_pending_publisher()
    );
    let err = client
        .send_request_update(rid, MessageParameters::new())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_ne!(client.state(), SessionState::Closing);
}

/// 受信側: Pending(Subscriber) 状態の subscription に peer から REQUEST_UPDATE が届いた場合
/// PROTOCOL_VIOLATION でセッションを閉じる。
#[test]
fn peer_request_update_in_pending_subscriber_closes_session() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, m) = take_send_request(&mut client);
    server
        .recv_request(m)
        .expect("テストフィクスチャの前提条件を満たす");
    // server は state=Pending(Subscriber) のまま SUBSCRIBE_OK を返さない。
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_pending_subscriber()
    );
    // client 側で REQUEST_UPDATE を自前構築し、server に直接流し込む。
    let update = ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
        request_id: rid,

        parameters: MessageParameters::new(),
    });
    let err = server.recv_stream_message(rid, update).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// 受信側: Pending(Publisher) 状態の subscription に peer から REQUEST_UPDATE が届いた場合も
/// PROTOCOL_VIOLATION でセッションを閉じる。
#[test]
fn peer_request_update_in_pending_publisher_closes_session() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            1,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, m) = take_send_request(&mut client);
    server
        .recv_request(m)
        .expect("テストフィクスチャの前提条件を満たす");
    // server は state=Pending(Publisher) のまま REQUEST_OK (publish context) を返さない。
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_pending_publisher()
    );
    let update = ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
        request_id: rid,

        parameters: MessageParameters::new(),
    });
    let err = server.recv_stream_message(rid, update).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE): "A subscriber can also send REQUEST_UPDATE to
/// modify parameters of a subscription established with PUBLISH."
/// PUBLISH で確立された subscription に対して、subscriber 側 (non-initiator) から
/// REQUEST_UPDATE を送れる。
#[test]
fn send_request_update_by_subscriber_on_publish_established_subscription() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, mut server) = establish_pair();
    // client=publisher が PUBLISH を送信、server=subscriber が REQUEST_OK (publish context) を返す
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            11,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // server は my_role=Subscriber, initiator=Publisher (non-initiator) で Established
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established
    );
    assert!(
        !server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_initiator_self()
    );

    // server (subscriber, non-initiator) から REQUEST_UPDATE を送る
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    server
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut server);
    // client (publisher, initiator) 側で受信できる
    client
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // draft-ietf-moq-transport-21 Appendix A.4 (Since draft-ietf-moq-transport-17) #1540: REQUEST_UPDATE のパラメータは pending_update_params に蓄積される
    let pending = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす")
        .pending_update_params
        .as_ref();
    assert!(pending.is_some());
    assert_eq!(
        pending
            .expect("テストフィクスチャの前提条件を満たす")
            .forward(),
        Some(0)
    );
    // forward_state は REQUEST_OK 応答まで更新されない
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1
    );
}

/// SUBSCRIBE で確立された subscription に対して、publisher responder (non-initiator) からの
/// REQUEST_OK (REQUEST_UPDATE 応答) は Established 状態で受理される。
#[test]
fn send_request_ok_by_publisher_on_subscribe_established_subscription_succeeds() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        !server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_initiator_self()
    );
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established
    );
}

/// draft-ietf-moq-transport-21 Appendix A.4 (Since draft-ietf-moq-transport-17) #1540, draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions): 同一サブスクリプション上の複数 REQUEST_UPDATE が
/// 合体 (coalesce) される。後の値が前を上書きし、累積結果のみが適用される。
#[test]
fn multiple_request_updates_are_coalesced() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD, PARAM_SUBSCRIBER_PRIORITY,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    recv_response_helper(&mut client, rid, ok_msg);

    // 1 回目の REQUEST_UPDATE: FORWARD=0
    let mut params1 = MessageParameters::new();
    params1.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    client
        .send_request_update(rid, params1)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd1) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd1)
        .expect("テストフィクスチャの前提条件を満たす");

    // pending_update_params に FORWARD=0 が蓄積されている
    let pending = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす")
        .pending_update_params
        .as_ref();
    assert!(pending.is_some());
    assert_eq!(
        pending
            .expect("テストフィクスチャの前提条件を満たす")
            .forward(),
        Some(0)
    );
    // まだ forward_state は変更されていない
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1
    );

    // 2 回目の REQUEST_UPDATE: SUBSCRIBER_PRIORITY=100 (FORWARD を含まない)
    let mut params2 = MessageParameters::new();
    params2.push(MessageParameter {
        param_type: PARAM_SUBSCRIBER_PRIORITY,
        value: MessageParameterValue::Uint8(100),
    });
    client
        .send_request_update(rid, params2)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd2) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd2)
        .expect("テストフィクスチャの前提条件を満たす");

    // 合体後: FORWARD=0 は保持され、SUBSCRIBER_PRIORITY=100 が追加されている
    let pending = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす")
        .pending_update_params
        .as_ref();
    assert!(pending.is_some());
    assert_eq!(
        pending
            .expect("テストフィクスチャの前提条件を満たす")
            .forward(),
        Some(0)
    );
    assert_eq!(
        pending
            .expect("テストフィクスチャの前提条件を満たす")
            .subscriber_priority(),
        Some(100)
    );

    // 3 回目の REQUEST_UPDATE: FORWARD=1 (前の FORWARD=0 を上書き)
    let mut params3 = MessageParameters::new();
    params3.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    client
        .send_request_update(rid, params3)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd3) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd3)
        .expect("テストフィクスチャの前提条件を満たす");

    // 合体後: FORWARD=1 に上書き、SUBSCRIBER_PRIORITY=100 は保持
    let pending = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす")
        .pending_update_params
        .as_ref();
    assert!(pending.is_some());
    assert_eq!(
        pending
            .expect("テストフィクスチャの前提条件を満たす")
            .forward(),
        Some(1)
    );
    assert_eq!(
        pending
            .expect("テストフィクスチャの前提条件を満たす")
            .subscriber_priority(),
        Some(100)
    );

    // send_request_ok で累積パラメータが subscription に適用される
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, reqok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, reqok)
        .expect("テストフィクスチャの前提条件を満たす");

    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(sub.forward_state, 1);
    assert_eq!(sub.subscriber_priority, Some(100));
    // pending_update_params はクリアされている
    assert!(sub.pending_update_params.is_none());
}

/// draft-ietf-moq-transport-21 Appendix A.4 (Since draft-ietf-moq-transport-17) #1540: REQUEST_UPDATE 合体後に REQUEST_ERROR で応答すると
/// pending_update_params が破棄される。
#[test]
fn request_error_discards_coalesced_pending_params() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    recv_response_helper(&mut client, rid, ok_msg);

    // REQUEST_UPDATE で FORWARD=0 を送信
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .pending_update_params
            .is_some()
    );

    // REQUEST_ERROR で応答 → pending_update_params が破棄される
    server
        .send_request_error(
            rid,
            0x01,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("update rejected".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .pending_update_params
            .is_none()
    );
    // forward_state は元のまま
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1
    );
}

/// draft-ietf-moq-transport-21 Appendix A.4 (Since draft-ietf-moq-transport-17) #1583: REQUEST_UPDATE で forward 0→1 に変更後、
/// STOP_SENDING で停止されたサブグループを publisher が再オープンできる
#[test]
fn forward_0_to_1_allows_reopen_of_stopped_by_peer_subgroup() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, mut server) = establish_pair();
    // SUBSCRIBE に FORWARD=0 を指定して購読を確立
    let mut sub_params = MessageParameters::new();
    sub_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), sub_params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 100, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        0
    );

    // publisher (server) がサブグループを開く
    let stream_id = DataStreamId(60);
    let header = SubgroupHeader {
        track_alias: 100,
        group_id: 5,
        subgroup_id: SubgroupIdMode::Explicit(3),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(stream_id, rid, &header)
        .expect("テストフィクスチャの前提条件を満たす");

    // subscriber (client) が STOP_SENDING を送信 → publisher は StoppedByPeer を記録する。
    // 記録自体は後段の再オープン成功で検証する (StoppedByPeer からのみ再オープン可)
    server
        .recv_data_stream_stop_sending(stream_id)
        .expect("テストフィクスチャの前提条件を満たす");

    // client が REQUEST_UPDATE で forward=1 に変更
    let mut upd_params = MessageParameters::new();
    upd_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    client
        .send_request_update(rid, upd_params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // server が REQUEST_OK で合体パラメータを適用 → forward_state が 0→1 になる
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, reqok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, reqok)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1
    );

    // publisher (server) が同じサブグループを再オープンできる (draft-ietf-moq-transport-21 Appendix A.4 (Since draft-ietf-moq-transport-17) #1583)
    // 再オープン成功自体が StoppedByPeer 記録の裏付けになる (他状態からは再オープン不可)
    let stream_id2 = DataStreamId(61);
    server
        .send_subgroup_header(stream_id2, rid, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    // 再オープンした stream でオブジェクト送信できる (Open 状態であることの裏付け)
    server
        .send_subgroup_object(stream_id2, 0, None)
        .expect("再オープンした stream ではオブジェクト送信できること");
}

/// 受信側対称性: SUBSCRIBE 経由 (peer=publisher, self=subscriber) で peer が
/// REQUEST_UPDATE を送ってきた場合、publisher は draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE) の送信資格がないので
/// PROTOCOL_VIOLATION でセッションを閉じる。
#[test]
fn peer_request_update_from_publisher_on_subscribe_closes_session() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // client は my_role=Subscriber, initiator=Subscriber (is_initiator_self=true)。
    // peer (publisher) から REQUEST_UPDATE が届いたケースを模擬する。
    let update = ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
        request_id: rid,

        parameters: MessageParameters::new(),
    });
    let err = client.recv_stream_message(rid, update).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}

/// send_request_update に FORWARD=2 を渡すと SESSION_PROTOCOL_VIOLATION が返り、
/// 値の検証失敗時に delivery timeout が適用されないこと
#[test]
fn send_request_update_rejects_invalid_forward_value() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 500, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // delivery timeout と値域違反の FORWARD=2 を混在させる
    // (修正前は delivery timeout が先に適用され、FORWARD 検証失敗後に残留していた)
    let mut params = delivery_timeout_params(100);
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(2),
    });
    let err = client.send_request_update(rid, params).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // 検証エラー時に delivery timeout が適用されていないこと (部分適用の残留を防ぐ)
    let sub = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        sub.delivery_timeouts.effective_object_ms, None,
        "値の検証失敗時に delivery timeout が適用されないこと"
    );
    assert_eq!(
        sub.forward_state, 1,
        "値の検証失敗時に forward_state が更新されないこと (SUBSCRIBE 時のデフォルト 1 のまま)"
    );
    // REQUEST_UPDATE が送信イベントとして積まれないこと
    while let Some(e) = client.poll_event() {
        if matches!(e, SessionEvent::SendOnStream { .. }) {
            panic!("値検証エラー時に REQUEST_UPDATE は送信されないこと");
        }
    }
}

/// send_request_update に不正な Location Filter を渡すと SESSION_PROTOCOL_VIOLATION が返り、
/// 値の検証失敗時に delivery timeout と forward_state が適用されないこと
///
/// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): 定義された serialization と
/// 一致しない LOCATION_FILTER は受信側の MUST で session close になるが、ローカル API の
/// 送信検証では同じ `SESSION_PROTOCOL_VIOLATION` コードの `Err` を返す (peer に未送信のため fail しない)。
#[test]
fn send_request_update_rejects_invalid_location_filter() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD, PARAM_LOCATION_FILTER,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 500, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // delivery timeout + FORWARD=0 (正常値) + 5 フィールドの壊れた LOCATION_FILTER を混在させる
    // (修正前は delivery timeout と forward_state=0 が先に適用され、
    // Location Filter 検証失敗後に両方が残留していた。forward_state は SUBSCRIBE 時点で
    // デフォルト 1 のため、適用されなければ 1 のまま残ることで判別できる)
    let mut params = delivery_timeout_params(100);
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x01, 0x02, 0x03, 0x04, 0x05]),
    });
    let err = client.send_request_update(rid, params).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // 検証エラー時に delivery timeout と forward_state の両方が適用されていないこと
    // (forward_state は SUBSCRIBE 時のデフォルト 1 のまま更新されない)
    let sub = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        sub.delivery_timeouts.effective_object_ms, None,
        "値の検証失敗時に delivery timeout が適用されないこと"
    );
    assert_eq!(
        sub.forward_state, 1,
        "値の検証失敗時に forward_state が更新されないこと"
    );
    // REQUEST_UPDATE が送信イベントとして積まれないこと
    while let Some(e) = client.poll_event() {
        if matches!(e, SessionEvent::SendOnStream { .. }) {
            panic!("値検証エラー時に REQUEST_UPDATE は送信されないこと");
        }
    }
    // 検証失敗後も subscription は正常に使い続けられ、正当なパラメータで再送信できること
    client
        .send_request_update(rid, delivery_timeout_params(200))
        .expect("検証失敗後に正当な REQUEST_UPDATE は送信できること");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .delivery_timeouts
            .effective_object_ms,
        Some(200),
        "正当な REQUEST_UPDATE で delivery timeout が適用されること"
    );
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1,
        "正当な REQUEST_UPDATE でも forward_state は更新されないこと (FORWARD 未指定)"
    );
}

/// send_request_update に正常な Location Filter を含めると成功し、適用されること
#[test]
fn send_request_update_applies_valid_location_filter() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD, PARAM_LOCATION_FILTER,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 500, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // delivery timeout + FORWARD=0 + NextObject ([0x00, 0x00]) の正常 filter を混在させる
    let mut params = delivery_timeout_params(100);
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x00, 0x00]),
    });
    client
        .send_request_update(rid, params)
        .expect("正常な REQUEST_UPDATE は送信できること");
    let sub = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        sub.delivery_timeouts.effective_object_ms,
        Some(100),
        "正常な REQUEST_UPDATE で delivery timeout が適用されること"
    );
    assert_eq!(
        sub.forward_state, 0,
        "正常な REQUEST_UPDATE で FORWARD=0 が適用されること"
    );
    assert_eq!(
        sub.filter,
        Some(shiguredo_moqt::message_parameter::LocationFilter::NextObject),
        "正常な REQUEST_UPDATE で指定した Location Filter が適用されること"
    );
}

/// REQUEST_UPDATE の Length 0 (no filter) で Location Filter が削除されること
///
/// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): Length 0 は no filter であり、
/// REQUEST_UPDATE ではフィルタ削除になる。パラメータ省略時は値 unchanged のため、
/// フィルタなしの更新では既存フィルタが維持される。
#[test]
fn send_request_update_empty_location_filter_removes_filter() {
    use shiguredo_moqt::message_parameter::{
        LocationFilter, MessageParameter, MessageParameterValue, PARAM_FORWARD,
        PARAM_LOCATION_FILTER,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 500, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // AbsoluteStart {5, 0} を適用する
    let start = Location {
        group_id: 5,
        object_id: 0,
    };
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteStart { start }.encode_to_bytes(),
        ),
    });
    client
        .send_request_update(rid, params)
        .expect("正常な REQUEST_UPDATE は送信できること");
    let sub = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        sub.filter,
        Some(LocationFilter::AbsoluteStart { start }),
        "REQUEST_UPDATE で指定した Location Filter が適用されること"
    );
    assert_eq!(
        sub.filter_start,
        Some(start),
        "解決済みの Start Location が追随すること"
    );
    assert_eq!(
        sub.filter_end, None,
        "AbsoluteStart は subscription open-ended のため End なしであること"
    );

    // フィルタなしの更新では既存フィルタが維持される (unchanged)
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    client
        .send_request_update(rid, params)
        .expect("フィルタなしの REQUEST_UPDATE は送信できること");
    assert!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .filter
            .is_some(),
        "LOCATION_FILTER 省略時は既存フィルタが維持されること"
    );

    // Length 0 の更新でフィルタが削除される
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(Vec::new()),
    });
    client
        .send_request_update(rid, params)
        .expect("Length 0 の REQUEST_UPDATE は送信できること");
    let sub = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        sub.filter, None,
        "Length 0 で Location Filter が削除されること"
    );
    assert_eq!(
        sub.filter_start, None,
        "Length 0 で解決済みの Start Location が戻ること"
    );
    assert_eq!(
        sub.filter_end, None,
        "Length 0 で解決済みの End Location が戻ること"
    );
}

/// peer からの REQUEST_UPDATE (Set → Length 0) を REQUEST_OK で適用するとフィルタが削除されること
///
/// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): Length 0 は no filter であり、
/// REQUEST_UPDATE ではフィルタ削除になる。受信側は pending_update_params に合体し、
/// REQUEST_OK 応答時に適用する。
#[test]
fn recv_request_update_empty_filter_clears_on_request_ok() {
    use shiguredo_moqt::message_parameter::{
        LocationFilter, MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 500, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // AbsoluteStart {5, 0} を送り、Length 0 で削除する
    let start = Location {
        group_id: 5,
        object_id: 0,
    };
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteStart { start }.encode_to_bytes(),
        ),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // REQUEST_OK 応答前は合体のみで適用されない (defer semantics)
    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        sub.pending_update_params.is_some(),
        "REQUEST_OK 応答前はパラメータが pending 蓄積されること"
    );
    assert_eq!(
        sub.filter, None,
        "REQUEST_OK 応答前はフィルタが適用されないこと"
    );
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(Vec::new()),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // REQUEST_OK 応答で合体済みパラメータが適用され、フィルタが削除される
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        sub.filter, None,
        "Set 後の Length 0 でフィルタが削除されること"
    );
    assert_eq!(
        sub.filter_start, None,
        "Set 後の Length 0 で解決済み Start が戻ること"
    );
    assert_eq!(
        sub.filter_end, None,
        "Set 後の Length 0 で解決済み End が戻ること"
    );

    // LOCATION_FILTER 省略の追随更新では削除状態が維持される (unchanged)
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        sub.filter, None,
        "省略の追随更新では削除状態が維持されること"
    );
    assert_eq!(
        sub.filter_start, None,
        "省略の追随更新では解決済み Start が維持されること"
    );
    assert_eq!(
        sub.filter_end, None,
        "省略の追随更新では解決済み End が維持されること"
    );
}

/// peer からの REQUEST_UPDATE (Length 0 → Set) を REQUEST_OK で適用すると Set が勝つこと
#[test]
fn recv_request_update_set_after_removed_wins() {
    use shiguredo_moqt::message_parameter::{
        LocationFilter, MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 500, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // Length 0 を送り、AbsoluteStart {5, 0} で上書きする
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(Vec::new()),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let start = Location {
        group_id: 5,
        object_id: 0,
    };
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteStart { start }.encode_to_bytes(),
        ),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // REQUEST_OK 応答で後の Set が適用される
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        sub.filter,
        Some(LocationFilter::AbsoluteStart { start }),
        "Length 0 の後の Set が適用されること"
    );
    assert_eq!(
        sub.filter_start,
        Some(start),
        "解決済みの Start Location が追随すること"
    );
    assert_eq!(
        sub.filter_end, None,
        "AbsoluteStart は subscription open-ended のため End なしであること"
    );
}

/// FORWARD と Location Filter の両方が不正な場合は、検証順序 (forward → filter) に従って
/// forward 検証が先に失敗すること
#[test]
fn send_request_update_rejects_invalid_forward_before_invalid_filter() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD, PARAM_LOCATION_FILTER,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 500, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // FORWARD=2 (値域外) + 5 フィールドの壊れた LOCATION_FILTER の両方を混在させる
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(2),
    });
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x01, 0x02, 0x03, 0x04, 0x05]),
    });
    let err = client.send_request_update(rid, params).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // forward 検証が先に失敗するため、filter 検証のエラーメッセージにはならないこと
    assert_eq!(
        err.reason, "invalid forward value",
        "検証順序 (forward → filter) が維持されること"
    );
}

#[test]
fn subscribe_ok_with_expires_zero_clears_expires() {
    // EXPIRES=0 は「期限なし」を意味し、subscription.expires が None のままとなる。
    // draft-ietf-moq-transport-21 §9.20.17
    let (mut client, mut server) = establish_pair();
    client.tick(2_000);

    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 11, expires_params(0), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let subscription = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(subscription.expires.is_none());
}

#[test]
fn subscribe_ok_expires_arms_and_expires_on_tick() {
    let (mut client, mut server) = establish_pair();
    client.tick(2_000);

    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 11, expires_params(300), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let subscription = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    let expires = subscription
        .expires
        .as_ref()
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(expires.duration_ms, 300);
    assert_eq!(expires.deadline_ms, Some(2_300));
    assert!(!subscription.expires_elapsed());

    client.tick(2_299);
    assert!(
        !client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .expires_elapsed()
    );
    client.tick(2_300);
    assert!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .expires_elapsed()
    );
}

/// SUBSCRIBE 確立後に REQUEST_UPDATE_OK を受信すると RequestOkReceived(request_kind=Subscribe) が発火する
#[test]
fn subscribe_request_update_ok_emits_request_ok_received_event_with_kind_subscribe() {
    // SUBSCRIBE → SUBSCRIBE_OK で確立し、その後に REQUEST_UPDATE → REQUEST_OK を受信する。
    // SUBSCRIBE_OK は handle_ok_for_subscription を通らないため、最初のイベントは発火しない。
    // REQUEST_UPDATE_OK 受信時に Established 分岐で RequestOkReceived(request_kind=Subscribe) が発火することを検証する。
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 500, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在すること")
            .state,
        SubscriptionState::Established
    );

    // REQUEST_UPDATE → REQUEST_OK サイクル
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, req_ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, req_ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // SUBSCRIBE 由来の REQUEST_UPDATE_OK 受信時に RequestOkReceived(request_kind=Subscribe) が発火する
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestOkReceived {
            request_id,
            request_kind,
            ..
        } = ev
            && request_id == rid
        {
            assert_eq!(request_kind, RequestKind::Subscribe);
            got = true;
            break;
        }
    }
    assert!(
        got,
        "SUBSCRIBE 由来の REQUEST_UPDATE_OK 受信時に RequestOkReceived(request_kind=Subscribe) が発火すること"
    );
}

/// PUBLISH 確立後に REQUEST_UPDATE_OK を受信すると RequestOkReceived(request_kind=Publish) が発火する
#[test]
fn publish_request_update_ok_emits_request_ok_received_event_with_kind_publish() {
    // PUBLISH → REQUEST_OK (Pending→Established) で最初の RequestOkReceived が発火する。
    // それを消費したあと、REQUEST_UPDATE → REQUEST_OK で二つ目の
    // RequestOkReceived(request_kind=Publish) が発火することを検証する。
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam1".to_vec(),
            0u64,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // Pending→Established で発火した最初の RequestOkReceived を消費する
    let mut consumed_first = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestOkReceived {
            request_id,
            request_kind,
            ..
        } = ev
            && request_id == rid
        {
            assert_eq!(request_kind, RequestKind::Publish);
            consumed_first = true;
            break;
        }
    }
    assert!(
        consumed_first,
        "最初の REQUEST_OK (Pending→Established) 用 RequestOkReceived が発火すること"
    );

    // REQUEST_UPDATE → REQUEST_OK サイクル: 二つ目の RequestOkReceived が発火する
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, req_ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, req_ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestOkReceived {
            request_id,
            request_kind,
            ..
        } = ev
            && request_id == rid
        {
            assert_eq!(request_kind, RequestKind::Publish);
            got = true;
            break;
        }
    }
    assert!(
        got,
        "PUBLISH 由来の REQUEST_UPDATE_OK 受信時に RequestOkReceived(request_kind=Publish) が発火すること"
    );
}

// ─── Range Filter パラメータスコープ検証 (draft-ietf-moq-transport-21 §3.3.2 (Range Filters)) ────

/// SUBSCRIBE_TRACKS 確立後に TRACK_PROPERTY_FILTER 付き REQUEST_UPDATE が受理されること
/// (draft-ietf-moq-transport-21 §3.3.2 (Range Filters): TRACK_PROPERTY_FILTER は
/// SUBSCRIBE_TRACKS の REQUEST_UPDATE に出現可能)
#[test]
fn subscribe_tracks_request_update_with_track_property_filter_accepted() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_PROPERTY_FILTER,
    };
    // 送信側は peer が MAX_FILTER_RANGES を宣言していないと Range Filter を送出できない
    // (draft-ietf-moq-transport-21 §9.1.6)。本テストの主題はパラメータスコープ検証なので、
    // peer (server) 側に上限を宣言させて送出できる状態を作る。
    let (mut client, mut server) =
        establish_pair_with_options(SetupOptions::new(), opts_with(0x06, 1));
    // SUBSCRIBE_TRACKS を確立する
    let rid = client
        .send_subscribe_tracks(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, st_msg) = take_send_request(&mut client);
    server
        .recv_request(st_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // TRACK_PROPERTY_FILTER 付き REQUEST_UPDATE を送信する
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_PROPERTY_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![]),
    });
    client
        .send_request_update(rid, params)
        .expect("TRACK_PROPERTY_FILTER は SUBSCRIBE_TRACKS の REQUEST_UPDATE で許可される");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    // 受信側でも受理されること
    server
        .recv_stream_message(rid, upd_msg)
        .expect("TRACK_PROPERTY_FILTER は SUBSCRIBE_TRACKS の REQUEST_UPDATE で許可される");
}

/// SUBSCRIBE 確立後に TRACK_PROPERTY_FILTER 付き REQUEST_UPDATE が
/// PROTOCOL_VIOLATION で拒否されること
/// (draft-ietf-moq-transport-21 §3.3.2 (Range Filters): TRACK_PROPERTY_FILTER は
/// SUBSCRIBE_TRACKS 専用であり、SUBSCRIBE の REQUEST_UPDATE には出現不可)
#[test]
fn subscribe_request_update_with_track_property_filter_rejected() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_PROPERTY_FILTER,
    };
    let (mut client, mut server) = establish_pair();
    // SUBSCRIBE を確立する
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // peer が TRACK_PROPERTY_FILTER 付き REQUEST_UPDATE を送ってきたと仮定する。
    // 送信側 API はスコープ検証で拒否するため、ワイヤメッセージを直接構築して受信側に流し込む。
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_PROPERTY_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![]),
    });
    let update = ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
        request_id: rid,
        parameters: params,
    });
    let err = server.recv_stream_message(rid, update).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // セッションがクローズされること
    match drain_until_close(&mut server) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        _ => unreachable!(),
    }
}

/// SUBSCRIBE 確立後に SUBGROUP_FILTER 付き REQUEST_UPDATE が受理されること (回帰)
/// (draft-ietf-moq-transport-21 §3.3.2 (Range Filters): Range Filter 0x25-0x28 は
/// subscription の REQUEST_UPDATE に出現可能)
#[test]
fn subscribe_request_update_with_subgroup_filter_accepted() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_SUBGROUP_FILTER,
    };
    // 送信側は peer が MAX_FILTER_RANGES を宣言していないと Range Filter を送出できない
    // (draft-ietf-moq-transport-21 §9.1.6)。本テストの主題はパラメータスコープ検証なので、
    // peer (server) 側に上限を宣言させて送出できる状態を作る。
    let (mut client, mut server) =
        establish_pair_with_options(SetupOptions::new(), opts_with(0x06, 1));
    // SUBSCRIBE を確立する
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // SUBGROUP_FILTER 付き REQUEST_UPDATE を送信する
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_SUBGROUP_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![]),
    });
    client
        .send_request_update(rid, params)
        .expect("SUBGROUP_FILTER は SUBSCRIBE の REQUEST_UPDATE で許可される");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    // 受信側でも受理されること
    server
        .recv_stream_message(rid, upd_msg)
        .expect("SUBGROUP_FILTER は SUBSCRIBE の REQUEST_UPDATE で許可される");
}

/// SUBSCRIBE_TRACKS 確立後に SUBGROUP_FILTER 付き REQUEST_UPDATE が受理されること (回帰)
/// (draft-ietf-moq-transport-21 §3.3.2 (Range Filters): Range Filter 0x25-0x28 は
/// SUBSCRIBE_TRACKS の REQUEST_UPDATE にも出現可能)
#[test]
fn subscribe_tracks_request_update_with_subgroup_filter_accepted() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_SUBGROUP_FILTER,
    };
    // 送信側は peer が MAX_FILTER_RANGES を宣言していないと Range Filter を送出できない
    // (draft-ietf-moq-transport-21 §9.1.6)。本テストの主題はパラメータスコープ検証なので、
    // peer (server) 側に上限を宣言させて送出できる状態を作る。
    let (mut client, mut server) =
        establish_pair_with_options(SetupOptions::new(), opts_with(0x06, 1));
    // SUBSCRIBE_TRACKS を確立する
    let rid = client
        .send_subscribe_tracks(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, st_msg) = take_send_request(&mut client);
    server
        .recv_request(st_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // SUBGROUP_FILTER 付き REQUEST_UPDATE を送信する
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_SUBGROUP_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![]),
    });
    client
        .send_request_update(rid, params)
        .expect("SUBGROUP_FILTER は SUBSCRIBE_TRACKS の REQUEST_UPDATE で許可される");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    // 受信側でも受理されること
    server
        .recv_stream_message(rid, upd_msg)
        .expect("SUBGROUP_FILTER は SUBSCRIBE_TRACKS の REQUEST_UPDATE で許可される");
}

/// FORWARD 値域外 (2) を含む REQUEST_UPDATE の受信で、`Err` と
/// `CloseSession(PROTOCOL_VIOLATION)` の両方が発生し、`pending_update_params` に保存されず
/// `RequestUpdateReceived` も発火しないこと
///
/// draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter): "If an endpoint receives a
/// value outside this range, it MUST close the session with PROTOCOL_VIOLATION."
/// 受信時に値域検証するため、`pending_update_params` への保存 (遅延検証化) が行われない。
#[test]
fn peer_request_update_with_invalid_forward_closes_session() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, mut server) = establish_pair();
    // client=publisher が PUBLISH を送信、server=subscriber が REQUEST_OK を返して Established
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            11,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // FORWARD=2 (値域外) を含む REQUEST_UPDATE を client (publisher 役) に直接流し込む
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(2),
    });
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
                request_id: rid,
                parameters: params,
            }),
        )
        .expect_err("値域外 FORWARD は PROTOCOL_VIOLATION になる");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // pending_update_params に保存されないこと (状態非汚染)
    assert!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .pending_update_params
            .is_none(),
        "値域違反時に pending_update_params に保存されないこと"
    );
    match drain_until_close(&mut client) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("CloseSession(PROTOCOL_VIOLATION) が期待されたが {other:?}"),
    }
    // RequestUpdateReceived イベントが発火しないこと
    // (CloseSession を先に消費してから確認する。イベントは CloseSession が最後に積まれる)
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::RequestUpdateReceived { .. }) {
            got = true;
        }
    }
    assert!(!got, "値域違反時に RequestUpdateReceived が発火しないこと");
}

/// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE) は "A publisher sends a PUBLISH_DONE message"
/// と規定するため、subscriber 側 (my_role != Publisher) からの Established subscription に対する
/// send_request_error は role 逆転で PUBLISH_DONE を誤送出する経路となる。release ビルドでも捕捉して
/// SESSION_PROTOCOL_VIOLATION でセッションを閉じることを検証する。
#[test]
fn send_request_error_from_subscriber_side_on_established_closes_session() {
    let (mut client, mut server) = establish_pair();
    // client=publisher が PUBLISH を送信、server=subscriber が REQUEST_OK を返して Established
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            11,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // Established 確認、server 側 my_role == Subscriber であること
    let s_sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(s_sub.state, SubscriptionState::Established);
    assert_eq!(s_sub.my_role, TrackRole::Subscriber);

    // server (Subscriber 役) から Established subscription に対して send_request_error を呼ぶ
    let err = server
        .send_request_error(
            rid,
            0x01,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("update failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect_err("subscriber 側の Established REQUEST_ERROR は PROTOCOL_VIOLATION になる");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // セッションが閉じていること (fail() 経由で Closing 遷移)
    assert_eq!(server.state(), SessionState::Closing);
    // PUBLISH_DONE が誤送出されていないこと
    let mut saw_publish_done = false;
    while let Some(ev) = server.poll_event() {
        if let SessionEvent::SendOnStream { message, .. } = ev
            && matches!(message, ControlMessage::PublishDone(_))
        {
            saw_publish_done = true;
        }
    }
    assert!(
        !saw_publish_done,
        "role 逆転検出時に PUBLISH_DONE を誤送出してはいけない"
    );
}

/// REQUEST_UPDATE の相対フィルタが更新時点の largest 基準で解決される
///
/// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): 相対フィルタは解決時点の
/// largest で 1 度だけ解決する。{0, 2} まで受信後に RelativeGroup{0} (Next Group) へ
/// 更新すると filter_start は {1, 0} になる (先頭 {0, 0} ではない)。
#[test]
fn request_update_relative_filter_resolves_with_current_largest() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::message_parameter::{
        LocationFilter, MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
    };
    use shiguredo_moqt::stream::subgroup::SubgroupIdMode;
    let (mut client, mut server, rid) = establish_subscribe_track(600);
    // {0, 0} / {0, 1} / {0, 2} を受信して largest を進める
    let stream_id = DataStreamId(210);
    let header = SubgroupHeader {
        track_alias: 600,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(stream_id, rid, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    for object_id in 0..=2 {
        let obj = DecodedSubgroupObject {
            object_id,
            payload_length: 1,
            status: None,
            properties_bytes: None,
        };
        client
            .recv_subgroup_object(stream_id, &obj)
            .expect("テストフィクスチャの前提条件を満たす");
    }
    // RelativeGroup{0} (Next Group) へ更新する
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::RelativeGroup { start_group: 0 }.encode_to_bytes(),
        ),
    });
    client
        .send_request_update(rid, params)
        .expect("正常な REQUEST_UPDATE は送信できること");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .filter_start,
        Some(Location {
            group_id: 1,
            object_id: 0,
        }),
        "更新時点の largest 基準で Next Group に解決されること"
    );
}
