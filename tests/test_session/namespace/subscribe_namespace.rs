//! SUBSCRIBE_NAMESPACE メッセージに関するテスト

use super::*;

/// 任意の namespace で SUBSCRIBE_NAMESPACE を確立する
///
/// 返り値は (subscriber, publisher, request_id)。
fn establish_subscribe_namespace_with(namespace: TrackNamespace) -> (Session, Session, u64) {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(namespace, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sn_msg) = take_send_request(&mut client);
    server
        .recv_request(sn_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server, rid)
}

/// SUBSCRIBE_NAMESPACE の両端ハンドシェイク + NAMESPACE 配信
#[test]
fn subscribe_namespace_with_namespace_delivery() {
    use shiguredo_moqt::message::Namespace;
    use shiguredo_moqt::session::types::NamespaceSubscriptionState;
    use shiguredo_moqt::session::types::SessionEvent;
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(ns(&[b"example"]));
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        NamespaceSubscriptionState::Established
    );
    // Server が NAMESPACE を配信
    server
        .send_namespace(rid, ns(&[b"live"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ns_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ns_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // Client はイベントを受け取る
    let mut got = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::NamespaceReceived { .. } = e {
            got = true;
            break;
        }
    }
    assert!(got);
    let _ = Namespace {
        track_namespace_suffix: ns(&[b"dummy"]),
    };
}

/// SUBSCRIBE_NAMESPACE 確立後は REQUEST_UPDATE → REQUEST_OK をやり取りできる
#[test]
fn subscribe_namespace_request_update_full_cycle() {
    use shiguredo_moqt::session::types::NamespaceSubscriptionState;
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(ns(&[b"example"]));

    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let mut got_update = false;
    while let Some(ev) = server.poll_event() {
        if let SessionEvent::RequestUpdateReceived { request_id, .. } = ev
            && request_id == rid
        {
            got_update = true;
            break;
        }
    }
    assert!(
        got_update,
        "RequestUpdateReceived event expected for SUBSCRIBE_NAMESPACE"
    );

    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, reqok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, reqok)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        NamespaceSubscriptionState::Established
    );
    assert_eq!(
        server
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        NamespaceSubscriptionState::Established
    );
}

/// SUBSCRIBE_NAMESPACE 確立後の REQUEST_ERROR (REQUEST_UPDATE 失敗応答) は
/// 送信・受信の両端で state を Terminated に遷移させ、遷移後の再 REQUEST_UPDATE は
/// PROTOCOL_VIOLATION で拒否される (draft-ietf-moq-transport-21 §9.5.1 (Updating
/// Subscriptions) の "the responder MUST close the bidi stream" 由来)。
#[test]
fn subscribe_namespace_request_error_in_established_transitions_to_terminated() {
    use shiguredo_moqt::message::ReasonPhrase;
    use shiguredo_moqt::session::types::NamespaceSubscriptionState;
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(ns(&[b"example"]));

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
            0x34,
            0,
            ReasonPhrase::new("update failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    // responder (server) 側で Terminated に遷移すること
    assert_eq!(
        server
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        NamespaceSubscriptionState::Terminated
    );
    let (_, err_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, err_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // initiator (client) 側でも Terminated に遷移すること
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        NamespaceSubscriptionState::Terminated
    );
    // 遷移後の再 REQUEST_UPDATE は Established 要求ガードで拒否されること
    let err = client
        .send_request_update(rid, MessageParameters::new())
        .expect_err("Terminated 遷移後の send_request_update は失敗するはず");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

// ─── draft-ietf-moq-transport-21 §9.20.21 (TRACK_NAMESPACE_PREFIX Parameter): TRACK_NAMESPACE_PREFIX ─────

/// SUBSCRIBE_NAMESPACE の REQUEST_UPDATE で prefix を更新できる
#[test]
fn subscribe_namespace_update_prefix() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    use shiguredo_moqt::session::types::SessionEvent;
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(ns(&[b"example"]));

    // client が REQUEST_UPDATE で新しい prefix を送信
    let new_prefix = ns(&[b"newprefix"]);
    let mut upd_params = MessageParameters::new();
    upd_params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(new_prefix.clone()),
    });
    client
        .send_request_update(rid, upd_params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // Server 側で prefix が更新されていることを確認
    let mut got_update = false;
    while let Some(ev) = server.poll_event() {
        if let SessionEvent::RequestUpdateReceived { request_id, .. } = ev
            && request_id == rid
        {
            got_update = true;
            break;
        }
    }
    assert!(got_update);
    assert_eq!(
        server
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        new_prefix
    );
}

/// SUBSCRIBE_NAMESPACE の prefix 更新が他購読と overlap すると PREFIX_OVERLAP
#[test]
fn subscribe_namespace_update_prefix_overlap_rejected() {
    use shiguredo_moqt::error::REQUEST_PREFIX_OVERLAP;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let (mut client, mut server) = establish_pair();

    // 1 件目の SUBSCRIBE_NAMESPACE (prefix = ["a", "b"])
    let rid1 = client
        .send_subscribe_namespace(ns(&[b"a", b"b"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg1) = take_send_request(&mut client);
    server
        .recv_request(msg1)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid1, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok1) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid1, ok1)
        .expect("テストフィクスチャの前提条件を満たす");

    // 2 件目の SUBSCRIBE_NAMESPACE (prefix = ["a", b"c"])
    let rid2 = client
        .send_subscribe_namespace(ns(&[b"a", b"c"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg2) = take_send_request(&mut client);
    server
        .recv_request(msg2)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid2, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok2) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid2, ok2)
        .expect("テストフィクスチャの前提条件を満たす");

    // rid1 の prefix を ["a"] に更新 → rid2 の ["a", "c"] は ["a"] を prefix に持つので overlap
    let mut upd_params = MessageParameters::new();
    upd_params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b"a"])),
    });
    client
        .send_request_update(rid1, upd_params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid1, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // Server が REQUEST_ERROR を返したことを確認
    let mut err_code = None;
    while let Some(ev) = server.poll_event() {
        if let SessionEvent::SendOnStream {
            request_id,
            message: ControlMessage::RequestError(e),
            fin,
        } = ev
            && request_id == rid1
        {
            err_code = Some(e.error_code);
            // 初回 request 拒否は最終応答のため FIN 付き (§6.4.2.3)
            assert!(fin, "拒否応答には FIN が付くこと");
            break;
        }
    }
    assert_eq!(err_code, Some(REQUEST_PREFIX_OVERLAP));
}

/// SUBSCRIBE_NAMESPACE の REQUEST_OK (Pending→Established) で
/// RequestOkReceived(request_kind=SubscribeNamespace) が発火する
#[test]
fn subscribe_namespace_request_ok_emits_request_ok_received_event_with_kind_subscribe_namespace() {
    use shiguredo_moqt::session::types::NamespaceSubscriptionState;
    let (mut client, _server, rid) = establish_subscribe_namespace_with(ns(&[b"example"]));
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("namespace_subscription が存在すること")
            .state,
        NamespaceSubscriptionState::Established
    );

    // Pending→Established で RequestOkReceived(SubscribeNamespace) が発火する
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestOkReceived {
            request_id,
            request_kind,
            ..
        } = ev
            && request_id == rid
        {
            assert_eq!(request_kind, RequestKind::SubscribeNamespace);
            got = true;
            break;
        }
    }
    assert!(
        got,
        "SUBSCRIBE_NAMESPACE の REQUEST_OK 受信時に RequestOkReceived(SubscribeNamespace) が発火すること"
    );
}

/// SUBSCRIBE_NAMESPACE 確立後に REQUEST_UPDATE_OK を受信すると
/// RequestOkReceived(request_kind=SubscribeNamespace) が再度発火する
#[test]
fn subscribe_namespace_request_update_ok_emits_request_ok_received_event() {
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(ns(&[b"example"]));

    // 最初の Pending→Established イベントを消費する
    let mut consumed_first = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestOkReceived {
            request_id,
            request_kind,
            ..
        } = ev
            && request_id == rid
        {
            assert_eq!(request_kind, RequestKind::SubscribeNamespace);
            consumed_first = true;
            break;
        }
    }
    assert!(
        consumed_first,
        "最初の SUBSCRIBE_NAMESPACE REQUEST_OK (Pending→Established) 用 RequestOkReceived が発火すること"
    );

    // REQUEST_UPDATE → REQUEST_OK: 二つ目のイベントが発火する
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
            assert_eq!(request_kind, RequestKind::SubscribeNamespace);
            got = true;
            break;
        }
    }
    assert!(
        got,
        "SUBSCRIBE_NAMESPACE の REQUEST_UPDATE_OK 受信時に RequestOkReceived(SubscribeNamespace) が発火すること"
    );
}

/// single period `.` 予約名前空間の SUBSCRIBE_NAMESPACE は DOES_NOT_EXIST で拒否され、subscription は作成されない
/// (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces))
#[test]
fn subscribe_namespace_single_period_rejected() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message::SubscribeNamespace;
    let (_, mut server) = establish_pair();
    server
        .recv_request(ControlMessage::SubscribeNamespace(SubscribeNamespace {
            request_id: 0,
            track_namespace_prefix: ns(&[b"."]),
            parameters: MessageParameters::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    // namespace subscription は作成されない
    assert!(server.namespace_subscription(0).is_none());
    let (_, err_msg) = take_send_on_stream(&mut server);
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_DOES_NOT_EXIST);
        }
        _ => panic!("DOES_NOT_EXIST の RequestError が期待される"),
    }
}

/// send_subscribe_namespace に single period `.` 予約名前空間を渡すと SESSION_PROTOCOL_VIOLATION が返る
/// (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces))
#[test]
fn send_subscribe_namespace_single_period_rejected() {
    let (mut client, _server) = establish_pair();
    let err = client
        .send_subscribe_namespace(ns(&[b"."]), MessageParameters::new())
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
}

/// NAMESPACE → NAMESPACE_DONE のシーケンスで active_suffixes が正しく遷移
#[test]
fn namespace_active_suffixes_track_and_clear() {
    use shiguredo_moqt::message::{Namespace, NamespaceDone};
    use shiguredo_moqt::session::types::NamespaceSubscriptionState;
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(ns(&[b"a"]));
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        NamespaceSubscriptionState::Established
    );

    // NAMESPACE 2 件配信
    server
        .send_namespace(rid, ns(&[b"live"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, m1) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, m1)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_namespace(rid, ns(&[b"vod"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, m2) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, m2)
        .expect("テストフィクスチャの前提条件を満たす");

    let entry = client
        .namespace_subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(entry.active_suffixes.len(), 2);
    assert!(entry.active_suffixes.contains(&ns(&[b"live"])));
    assert!(entry.active_suffixes.contains(&ns(&[b"vod"])));

    // NAMESPACE_DONE で 1 件除去
    server
        .send_namespace_done(rid, ns(&[b"live"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, d1) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, d1)
        .expect("テストフィクスチャの前提条件を満たす");
    let entry = client
        .namespace_subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(entry.active_suffixes.len(), 1);
    assert!(entry.active_suffixes.contains(&ns(&[b"vod"])));

    // 未参照: 型名を使う
    let _ = Namespace {
        track_namespace_suffix: ns(&[b"x"]),
    };
    let _ = NamespaceDone {
        track_namespace_suffix: ns(&[b"x"]),
    };
}

/// 対応する NAMESPACE を受けていない NAMESPACE_DONE は PROTOCOL_VIOLATION
#[test]
fn namespace_done_without_namespace_is_violation() {
    use shiguredo_moqt::message::ControlMessage;
    use shiguredo_moqt::message::NamespaceDone;
    let (mut client, _server, rid) = establish_subscribe_namespace_with(ns(&[b"a"]));

    // server からの NAMESPACE なしで直接 NAMESPACE_DONE を注入する
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::NamespaceDone(NamespaceDone {
                track_namespace_suffix: ns(&[b"live"]),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// draft-ietf-moq-transport-21 §4.1 (Subscribing to Namespaces): publisher は REQUEST_OK / REQUEST_ERROR を bidi stream の最初の
/// メッセージとして送るので、REQUEST_OK 送信前 (Pending 状態) に NAMESPACE を送ることはできない。
#[test]
fn send_namespace_before_request_ok_errors() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"a"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sn_msg) = take_send_request(&mut client);
    server
        .recv_request(sn_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // REQUEST_OK は送信していない。server は Pending 状態のまま。
    let err = server.send_namespace(rid, ns(&[b"live"])).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    let _ = rid;
}

/// 同じく NAMESPACE_DONE を REQUEST_OK 送信前に送ることはできない。
#[test]
fn send_namespace_done_before_request_ok_errors() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"a"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sn_msg) = take_send_request(&mut client);
    server
        .recv_request(sn_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = server.send_namespace_done(rid, ns(&[b"live"])).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// 同じく PUBLISH_SKIPPED を REQUEST_OK 送信前に送ることはできない。
#[test]
fn send_publish_skipped_before_request_ok_errors() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"a"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sn_msg) = take_send_request(&mut client);
    server
        .recv_request(sn_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = server
        .send_publish_skipped(rid, ns(&[b"live"]), b"t".to_vec())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// subscriber 側: REQUEST_OK 受信前に NAMESPACE が届いたら session を閉じる。
#[test]
fn peer_namespace_before_request_ok_closes_session() {
    use shiguredo_moqt::message::Namespace;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"a"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sn_msg) = take_send_request(&mut client);
    server
        .recv_request(sn_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // client は Pending のまま。NAMESPACE を注入する。
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::Namespace(Namespace {
                track_namespace_suffix: ns(&[b"live"]),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}

/// subscriber 側: REQUEST_OK 受信前に NAMESPACE_DONE が届いたら session を閉じる。
#[test]
fn peer_namespace_done_before_request_ok_closes_session() {
    use shiguredo_moqt::message::NamespaceDone;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"a"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sn_msg) = take_send_request(&mut client);
    server
        .recv_request(sn_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::NamespaceDone(NamespaceDone {
                track_namespace_suffix: ns(&[b"live"]),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}

/// subscriber 側: REQUEST_OK 受信前に PUBLISH_SKIPPED が届いたら session を閉じる。
#[test]
fn peer_publish_skipped_before_request_ok_closes_session() {
    use shiguredo_moqt::message::PublishSkipped;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"a"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sn_msg) = take_send_request(&mut client);
    server
        .recv_request(sn_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::PublishSkipped(PublishSkipped {
                track_namespace_suffix: ns(&[b"live"]),
                track_name: b"t".to_vec(),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}

/// ストリームリセット通知で active な全 suffix が implicit_done として吐き出される
#[test]
fn stream_reset_emits_implicit_done_for_all_active() {
    use shiguredo_moqt::session::types::{NamespaceSubscriptionState, TerminationReason};
    use shiguredo_moqt::{session::types::RequestStreamEnd, session::types::SessionEvent};
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(ns(&[b"a"]));

    // 2 件の NAMESPACE を配信
    for suffix in &[b"live".as_slice(), b"vod".as_slice()] {
        server
            .send_namespace(rid, ns(&[suffix]))
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, m) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(rid, m)
            .expect("テストフィクスチャの前提条件を満たす");
    }

    // stream reset
    client
        .recv_request_stream_closed(
            rid,
            RequestStreamEnd::Reset {
                error_code: 0,
                reliable_size: None,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // イベントを確認
    let mut reset_seen = None;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind: RequestKind::SubscribeNamespace,
            reason: TerminationReason::NamespaceImplicitDone { suffixes },
        } = e
        {
            assert_eq!(request_id, rid);
            reset_seen = Some(suffixes);
            break;
        }
    }
    let implicit = reset_seen.expect("NamespaceImplicitDone イベントを期待する");
    assert_eq!(implicit.len(), 2);
    assert!(implicit.contains(&ns(&[b"live"])));
    assert!(implicit.contains(&ns(&[b"vod"])));

    // state は Terminated に遷移し active_suffixes は空
    let entry = client
        .namespace_subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(entry.state, NamespaceSubscriptionState::Terminated);
    assert!(entry.active_suffixes.is_empty());
}

/// スコープ外パラメータを含む `send_subscribe_namespace` は API 呼び出し時に
/// `Err(SendRequestError::Session(...))` を返し、状態・イベント・ request_id を汚染しないこと
///
/// draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope): 検証がないと、
/// NamespaceSubscription の登録・ control deadline の開始・ SendRequest の push まで
/// 完了した後に I/O 層のエンコード時 (validate_scope) で非同期に失敗する。
/// 検証は request_id 発行より前に置き、エラー時に欠番を作らない。
#[test]
fn send_subscribe_namespace_with_out_of_scope_parameter_rejected_without_state_change() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES,
    };
    use shiguredo_moqt::session::types::SendRequestError;
    let (mut client, _server) = establish_pair();
    let before = client.namespace_subscriptions().count();
    // SUBSCRIBE_NAMESPACE で許可されない EXPIRES を載せる
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(1000),
    });
    let err = client
        .send_subscribe_namespace(ns(&[b"example"]), params)
        .unwrap_err();
    match err {
        SendRequestError::Session(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("SendRequestError::Session が期待される: {other:?}"),
    }
    // NamespaceSubscription が登録されないこと
    assert_eq!(
        client.namespace_subscriptions().count(),
        before,
        "検証エラー時に NamespaceSubscription が登録されないこと"
    );
    // SendRequest イベントが積まれないこと
    let mut sent = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::SendRequest { .. }) {
            sent = true;
        }
    }
    assert!(!sent, "検証エラー時に SendRequest イベントが積まれないこと");
    // 検証エラー後に正常な送信が継続できること (request_id が欠番にならない)
    let rid = client
        .send_subscribe_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("検証エラー後の正常送信は成功すること");
    assert_eq!(rid, 0, "検証エラーで request_id が欠番にならないこと");
}

/// SUBSCRIBE_NAMESPACE の REQUEST_OK に含まれる EXPIRES が
/// RequestOkReceived の parameters に伝播すること
///
/// draft-ietf-moq-transport-21 §9.3 (REQUEST_OK): NAMESPACE_OK_ALLOWED_PARAMS は
/// EXPIRES のみを許可する。受信側がパラメータを捨てると application が期限を扱えない。
#[test]
fn subscribe_namespace_request_ok_propagates_expires_parameter() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sn_msg) = take_send_request(&mut client);
    server
        .recv_request(sn_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(5678),
    });
    server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .expect("EXPIRES 付き REQUEST_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestOkReceived {
            request_id,
            request_kind,
            parameters,
        } = ev
            && request_id == rid
        {
            assert_eq!(request_kind, RequestKind::SubscribeNamespace);
            assert_eq!(
                parameters.expires(),
                Some(5678),
                "REQUEST_OK の EXPIRES が RequestOkReceived に伝播すること"
            );
            got = true;
            break;
        }
    }
    assert!(
        got,
        "SUBSCRIBE_NAMESPACE の REQUEST_OK 受信イベントが発火すること"
    );
}
