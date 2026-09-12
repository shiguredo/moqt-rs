//! SUBSCRIBE_NAMESPACE メッセージに関するテスト

use super::*;

/// client から 1 本の SUBSCRIBE_NAMESPACE を確立する
///
/// 返り値は request_id。
fn establish_namespace_subscription(
    client: &mut Session,
    server: &mut Session,
    prefix: TrackNamespace,
) -> u64 {
    let rid = client
        .send_subscribe_namespace(prefix, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sn_msg) = take_send_request(client);
    server
        .recv_request(sn_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    rid
}

/// 任意の namespace で SUBSCRIBE_NAMESPACE を確立する
///
/// 返り値は (subscriber, publisher, request_id)。
fn establish_subscribe_namespace_with(namespace: TrackNamespace) -> (Session, Session, u64) {
    let (mut client, mut server) = establish_pair();
    let rid = establish_namespace_subscription(&mut client, &mut server, namespace);
    (client, server, rid)
}

/// 2 本の SUBSCRIBE_NAMESPACE を確立する
///
/// 返り値は (subscriber, publisher, rid1, rid2)。
fn establish_two_namespace_subscriptions(
    prefix1: TrackNamespace,
    prefix2: TrackNamespace,
) -> (Session, Session, u64, u64) {
    let (mut client, mut server) = establish_pair();
    let rid1 = establish_namespace_subscription(&mut client, &mut server, prefix1);
    let rid2 = establish_namespace_subscription(&mut client, &mut server, prefix2);
    (client, server, rid1, rid2)
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
    let (_client, mut server, rid1, _rid2) =
        establish_two_namespace_subscriptions(ns(&[b"a", b"b"]), ns(&[b"a", b"c"]));

    // rid1 の prefix を ["a"] に更新 → rid2 の ["a", "c"] は ["a"] を prefix に持つので overlap
    // 送信側のローカル検査は別テストで検証するため、ここでは REQUEST_UPDATE を直接
    // 受信させて受信側の overlap 検査を検証する
    let mut upd_params = MessageParameters::new();
    upd_params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b"a"])),
    });
    server
        .recv_stream_message(
            rid1,
            ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
                request_id: rid1,
                parameters: upd_params,
            }),
        )
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

/// 同一 suffix の NAMESPACE を再受信してもセッションを閉じず、イベントも再発行しない
///
/// draft-ietf-moq-transport-21 §9.15 (SUBSCRIBE_NAMESPACE) / §9.16 (NAMESPACE) /
/// §9.17 (NAMESPACE_DONE) に NAMESPACE 重複受信を違反とする規定はない。
#[test]
fn duplicate_namespace_is_ignored() {
    use shiguredo_moqt::session::types::SessionEvent;
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(ns(&[b"example"]));

    // 同一 suffix の NAMESPACE を 2 回配信する
    for _ in 0..2 {
        server
            .send_namespace(rid, ns(&[b"live"]))
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ns_msg) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(rid, ns_msg)
            .expect("重複 NAMESPACE は受理されること");
    }
    assert_eq!(client.state(), SessionState::Established);
    assert!(
        client
            .namespace_subscription(rid)
            .expect("namespace_subscription が存在すること")
            .active_suffixes
            .contains(&ns(&[b"live"])),
        "active_suffixes に当該 suffix が残ること"
    );

    // NAMESPACE_DONE は 1 回で当該 suffix を削除する
    server
        .send_namespace_done(rid, ns(&[b"live"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, done_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("NAMESPACE_DONE は受理されること");
    assert!(
        !client
            .namespace_subscription(rid)
            .expect("namespace_subscription が存在すること")
            .active_suffixes
            .contains(&ns(&[b"live"])),
        "NAMESPACE_DONE で当該 suffix が削除されること"
    );

    // DONE 後の再告知は新規の NAMESPACE として受理する
    server
        .send_namespace(rid, ns(&[b"live"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ns_msg2) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ns_msg2)
        .expect("DONE 後の再告知は受理されること");
    assert!(
        client
            .namespace_subscription(rid)
            .expect("namespace_subscription が存在すること")
            .active_suffixes
            .contains(&ns(&[b"live"])),
        "再告知で active_suffixes に戻ること"
    );

    // NamespaceReceived は重複 1 回 + 再告知 1 回、NamespaceDoneReceived は 1 回
    let mut received = 0;
    let mut done = 0;
    while let Some(e) = client.poll_event() {
        match e {
            SessionEvent::NamespaceReceived { request_id, .. } if request_id == rid => {
                received += 1
            }
            SessionEvent::NamespaceDoneReceived { request_id, .. } if request_id == rid => {
                done += 1
            }
            _ => {}
        }
    }
    assert_eq!(
        received, 2,
        "2 回目の NAMESPACE は再発行せず、DONE 後の再告知は発行すること"
    );
    assert_eq!(done, 1, "NAMESPACE_DONE は 1 回で反映されること");
}

// ─── 送信側 REQUEST_UPDATE の TRACK_NAMESPACE_PREFIX 反映 (draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions)) ─────

/// 送信側は REQUEST_UPDATE の送信直後には prefix を変えず、REQUEST_OK 受信後に
/// 新しい prefix を適用する
///
/// draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions): "If the update is accepted,
/// NAMESPACE and NAMESPACE_DONE messages following the REQUEST_OK will contain Track
/// Namespace suffixes relative to the updated prefix."
#[test]
fn subscribe_namespace_update_prefix_applies_after_request_ok() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let old_prefix = ns(&[b"example"]);
    let new_prefix = ns(&[b"newprefix"]);
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(old_prefix.clone());

    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(new_prefix.clone()),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    // 送信直後の initiator 側 prefix は旧のまま
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        old_prefix,
        "REQUEST_OK 受信前は旧 prefix のままであること"
    );
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // responder 側は REQUEST_UPDATE の受信時点で適用する
    assert_eq!(
        server
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        new_prefix
    );
    // REQUEST_OK 受信で initiator 側にも適用する
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        new_prefix,
        "REQUEST_OK 受信後に新 prefix が適用されること"
    );
}

/// REQUEST_OK 受信前に届いた NAMESPACE は旧 prefix のまま解決され、
/// 受信後は新 prefix で解決される
///
/// draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions): "If the update is accepted,
/// NAMESPACE and NAMESPACE_DONE messages following the REQUEST_OK will contain Track
/// Namespace suffixes relative to the updated prefix."
#[test]
fn subscribe_namespace_update_prefix_applies_after_in_flight_namespace() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    use shiguredo_moqt::session::types::SessionEvent;
    let old_prefix = ns(&[b"example"]);
    let new_prefix = ns(&[b"newprefix"]);
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(old_prefix.clone());

    // responder が更新処理より前に旧 prefix 相対の NAMESPACE / NAMESPACE_DONE を送る
    server
        .send_namespace(rid, ns(&[b"live"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ns_before_ok) = take_send_on_stream(&mut server);
    server
        .send_namespace_done(rid, ns(&[b"live"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, nsd_before_ok) = take_send_on_stream(&mut server);
    // initiator が REQUEST_UPDATE を送る (まだ確定しない)
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(new_prefix.clone()),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    // REQUEST_OK より前に届いた NAMESPACE / NAMESPACE_DONE は旧 prefix 基準のまま扱われる
    client
        .recv_stream_message(rid, ns_before_ok)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_stream_message(rid, nsd_before_ok)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        old_prefix,
        "REQUEST_OK 前の NAMESPACE / NAMESPACE_DONE は旧 prefix 基準のまま扱われること"
    );
    // responder が更新を処理して REQUEST_OK を返す
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        new_prefix,
        "REQUEST_OK 後は新 prefix 基準で扱われること"
    );
    // 更新後の NAMESPACE / NAMESPACE_DONE も新 prefix 基準で扱われる
    server
        .send_namespace(rid, ns(&[b"live2"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ns_after_ok) = take_send_on_stream(&mut server);
    server
        .send_namespace_done(rid, ns(&[b"live2"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, nsd_after_ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ns_after_ok)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_stream_message(rid, nsd_after_ok)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        new_prefix,
        "REQUEST_OK 後の NAMESPACE / NAMESPACE_DONE は新 prefix 基準で扱われること"
    );
    // 受信した suffix は prefix に依存せずそのまま届く
    let mut suffixes = Vec::new();
    let mut done_suffixes = Vec::new();
    while let Some(ev) = client.poll_event() {
        match ev {
            SessionEvent::NamespaceReceived { suffix, .. } => suffixes.push(suffix),
            SessionEvent::NamespaceDoneReceived { suffix, .. } => done_suffixes.push(suffix),
            _ => {}
        }
    }
    assert_eq!(suffixes, vec![ns(&[b"live"]), ns(&[b"live2"])]);
    assert_eq!(done_suffixes, vec![ns(&[b"live"]), ns(&[b"live2"])]);
}

/// REQUEST_ERROR を受信した場合は確定待ちを破棄し、prefix は旧のままにする
#[test]
fn subscribe_namespace_update_prefix_request_error_keeps_old_prefix() {
    use shiguredo_moqt::message::ReasonPhrase;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    use shiguredo_moqt::session::types::NamespaceSubscriptionState;
    let old_prefix = ns(&[b"example"]);
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(old_prefix.clone());

    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b"newprefix"])),
    });
    client
        .send_request_update(rid, params)
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
    let (_, err_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, err_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        NamespaceSubscriptionState::Terminated
    );
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        old_prefix,
        "REQUEST_ERROR 受信時は prefix を更新しないこと"
    );
}

/// prefix 更新先が他購読と overlap する場合は送信前にローカルで拒否する
///
/// draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions) /
/// §9.20.21 (TRACK_NAMESPACE_PREFIX Parameter): 作成時と同じ overlap 条件で検査し、
/// overlap する更新は送信しない。
#[test]
fn subscribe_namespace_update_prefix_overlap_rejected_locally() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let old_prefix = ns(&[b"a", b"b"]);
    let (mut client, _server, rid1, _rid2) =
        establish_two_namespace_subscriptions(old_prefix.clone(), ns(&[b"a", b"c"]));

    // rid1 の prefix を ["a"] に更新 → rid2 の ["a", "c"] と overlap するため送信前に拒否
    let mut upd_params = MessageParameters::new();
    upd_params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b"a"])),
    });
    let err = client
        .send_request_update(rid1, upd_params)
        .expect_err("overlap する prefix 更新はローカルで拒否される");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // 拒否された REQUEST_UPDATE は送信イベントを発行しない
    while let Some(ev) = client.poll_event() {
        assert!(
            !matches!(ev, SessionEvent::SendOnStream { .. }),
            "拒否された REQUEST_UPDATE は送信されないこと"
        );
    }
    assert_eq!(
        client
            .namespace_subscription(rid1)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        old_prefix,
        "拒否時に prefix を更新しないこと"
    );
}

/// prefix 変更を含まない REQUEST_UPDATE と prefix 更新を連続送信しても、
/// それぞれに対応する REQUEST_OK まで prefix を適用しない
#[test]
fn subscribe_namespace_update_prefix_waits_for_corresponding_request_ok() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let old_prefix = ns(&[b"example"]);
    let new_prefix = ns(&[b"newprefix"]);
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(old_prefix.clone());

    // 1 通目: prefix 変更なし、2 通目: prefix 更新
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd1_msg) = take_send_on_stream(&mut client);
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(new_prefix.clone()),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd2_msg) = take_send_on_stream(&mut client);

    // 1 通目の REQUEST_OK では prefix を適用しない
    server
        .recv_stream_message(rid, upd1_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok1_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok1_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        old_prefix,
        "prefix 変更なしの REQUEST_OK では prefix を適用しないこと"
    );

    // 2 通目の REQUEST_OK で prefix を適用する
    server
        .recv_stream_message(rid, upd2_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok2_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok2_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        new_prefix
    );
}

/// 確定待ちの間に「適用済みと同値」の prefix へ戻す更新を送っても、
/// 送信順の REQUEST_OK ごとに p0 → p1 → p0 と適用される
#[test]
fn subscribe_namespace_update_prefix_reverts_before_first_request_ok() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let old_prefix = ns(&[b"example"]);
    let mid_prefix = ns(&[b"newprefix"]);
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(old_prefix.clone());

    // 1 通目: p0 → p1
    let mut params1 = MessageParameters::new();
    params1.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(mid_prefix.clone()),
    });
    client
        .send_request_update(rid, params1)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd1_msg) = take_send_on_stream(&mut client);
    // 2 通目: p1 → p0 (適用済み p0 と同値だが、確定待ち p1 を経由した後の反映値)
    let mut params2 = MessageParameters::new();
    params2.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(old_prefix.clone()),
    });
    client
        .send_request_update(rid, params2)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd2_msg) = take_send_on_stream(&mut client);

    // responder 側は p1 → p0 の順に処理する
    server
        .recv_stream_message(rid, upd1_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .recv_stream_message(rid, upd2_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        old_prefix
    );
    // 1 通目の REQUEST_OK で p1、2 通目で p0 と適用する
    for (expected, label) in [
        (mid_prefix.clone(), "1 通目"),
        (old_prefix.clone(), "2 通目"),
    ] {
        server
            .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok_msg) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(rid, ok_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            client
                .namespace_subscription(rid)
                .expect("テストフィクスチャの前提条件を満たす")
                .prefix,
            expected,
            "{label} REQUEST_OK の適用結果"
        );
    }
}

/// 他購読の確定待ち prefix と overlap する更新も送信前にローカルで拒否する
#[test]
fn subscribe_namespace_update_prefix_overlapping_pending_update_rejected_locally() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let (mut client, _server, rid1, rid2) =
        establish_two_namespace_subscriptions(ns(&[b"a", b"b"]), ns(&[b"c", b"d"]));

    // rid1 の prefix を ["x"] に更新する (確定待ち)
    let mut params1 = MessageParameters::new();
    params1.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b"x"])),
    });
    client
        .send_request_update(rid1, params1)
        .expect("テストフィクスチャの前提条件を満たす");
    // rid2 の prefix を ["x", "y"] に更新すると、rid1 の確定待ち ["x"] と overlap する
    let mut params2 = MessageParameters::new();
    params2.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b"x", b"y"])),
    });
    let err = client
        .send_request_update(rid2, params2)
        .expect_err("他購読の確定待ち prefix と overlap する更新は拒否される");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        client
            .namespace_subscription(rid2)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        ns(&[b"c", b"d"]),
        "拒否時に prefix を更新しないこと"
    );
}

/// bidi stream 終端では確定待ちを反映せず、prefix は旧のままにする
#[test]
fn subscribe_namespace_update_prefix_bidi_end_keeps_old_prefix() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    use shiguredo_moqt::session::types::{NamespaceSubscriptionState, RequestStreamEnd};
    let old_prefix = ns(&[b"example"]);
    let (mut client, _server, rid) = establish_subscribe_namespace_with(old_prefix.clone());

    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b"newprefix"])),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    // REQUEST_OK を受信しないまま bidi stream が終端する
    client
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        NamespaceSubscriptionState::Terminated
    );
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        old_prefix,
        "bidi 終端時は prefix を更新しないこと (確定待ちキューは内部状態)"
    );
}

/// 対応する REQUEST_UPDATE が無い REQUEST_OK はプロトコル違反でセッションを閉じる
#[test]
fn subscribe_namespace_request_ok_without_update_closes_session() {
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(ns(&[b"example"]));

    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    let err = client
        .recv_stream_message(rid, ok_msg)
        .expect_err("対応する REQUEST_UPDATE が無い REQUEST_OK は拒否される");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    match drain_until_close(&mut client) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("CloseSession(PROTOCOL_VIOLATION) が期待されたが {other:?}"),
    }
}

/// 確定待ち prefix と overlap する新規 SUBSCRIBE_NAMESPACE はローカルで拒否する
#[test]
fn subscribe_namespace_create_overlapping_pending_update_rejected_locally() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let (mut client, _server, rid1) = establish_subscribe_namespace_with(ns(&[b"a", b"b"]));

    // rid1 の prefix を ["a"] に更新する (確定待ち)
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b"a"])),
    });
    client
        .send_request_update(rid1, params)
        .expect("テストフィクスチャの前提条件を満たす");

    // 確定待ち ["a"] と overlap する ["a", "c"] の新規購読は拒否する
    let err = client
        .send_subscribe_namespace(ns(&[b"a", b"c"]), MessageParameters::new())
        .expect_err("確定待ち prefix と overlap する新規購読は拒否される");
    assert_eq!(
        err.as_session_error().map(|e| e.code),
        Some(SESSION_PROTOCOL_VIOLATION)
    );
    // 確定待ちでない ["x"] の新規購読は受理される
    client
        .send_subscribe_namespace(ns(&[b"x"]), MessageParameters::new())
        .expect("確定待ちと overlap しない新規購読は受理される");
}

/// ローカル拒否された更新は確定待ちに残らず、後続の更新だけが REQUEST_OK で適用される
#[test]
fn subscribe_namespace_locally_rejected_update_is_not_queued() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let (mut client, mut server, rid1, _rid2) =
        establish_two_namespace_subscriptions(ns(&[b"a", b"b"]), ns(&[b"a", b"c"]));

    // rid1 の ["a"] 更新は rid2 と overlap するためローカル拒否される
    let mut rejected = MessageParameters::new();
    rejected.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b"a"])),
    });
    client
        .send_request_update(rid1, rejected)
        .expect_err("overlap する prefix 更新はローカルで拒否される");

    // 後続の正当な更新 ["x"] を送り、REQUEST_OK で ["x"] が適用される
    let mut accepted = MessageParameters::new();
    accepted.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b"x"])),
    });
    client
        .send_request_update(rid1, accepted)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid1, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid1, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid1, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .namespace_subscription(rid1)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        ns(&[b"x"]),
        "拒否された更新が確定待ちに残らないこと"
    );
}

/// Terminated 購読は overlap 検査の対象外である (作成時・更新時の両方)
#[test]
fn subscribe_namespace_terminated_subscription_does_not_block_overlap() {
    use shiguredo_moqt::message::ReasonPhrase;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    use shiguredo_moqt::session::types::NamespaceSubscriptionState;
    let (mut client, mut server, rid1) = establish_subscribe_namespace_with(ns(&[b"a", b"b"]));

    // rid1 の prefix 更新を REQUEST_ERROR で Terminated にする (prefix は ["a", "b"] のまま)
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b"x"])),
    });
    client
        .send_request_update(rid1, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid1, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_error(
            rid1,
            0x34,
            0,
            ReasonPhrase::new("update failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, err_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid1, err_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .namespace_subscription(rid1)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        NamespaceSubscriptionState::Terminated
    );

    // Terminated の ["a", "b"] と overlap する新規購読は受理される
    let rid2 = client
        .send_subscribe_namespace(ns(&[b"a", b"c"]), MessageParameters::new())
        .expect("Terminated 購読は overlap 検査の対象外");
    let (_, sn_msg) = take_send_request(&mut client);
    server
        .recv_request(sn_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid2, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid2, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // rid2 の更新 ["a"] も Terminated の ["a", "b"] と overlap するが受理される
    let mut params2 = MessageParameters::new();
    params2.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b"a"])),
    });
    client
        .send_request_update(rid2, params2)
        .expect("Terminated 購読は overlap 検査の対象外");
    let (_, upd2_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid2, upd2_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid2, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok2_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid2, ok2_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .namespace_subscription(rid2)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        ns(&[b"a"]),
        "Terminated 購読を無視して新 prefix が適用されること"
    );
}

/// prefix 更新後は新 prefix 相対で同一 suffix 文字列の NAMESPACE が新規として届く
///
/// draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions): prefix 更新後の
/// NAMESPACE は新 prefix 相対になるため、旧 prefix 相対で保持していた `active_suffixes` は
/// 破棄される。
#[test]
fn namespace_after_prefix_update_is_not_treated_as_duplicate() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    use shiguredo_moqt::session::types::SessionEvent;
    let old_prefix = ns(&[b"example"]);
    let new_prefix = ns(&[b"newprefix"]);
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(old_prefix);

    // 旧 prefix 相対で suffix "live" を広告する
    server
        .send_namespace(rid, ns(&[b"live"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ns_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ns_msg)
        .expect("NAMESPACE は受理されること");
    // `take_send_on_stream` は他のイベントを破棄するため、ここで 1 回目のイベントを数える
    let mut first_received = 0;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::NamespaceReceived { request_id, .. } = e
            && request_id == rid
        {
            first_received += 1;
        }
    }
    assert_eq!(first_received, 1, "旧 prefix 基準の NAMESPACE が届くこと");

    // client が prefix 更新を送り、REQUEST_OK で適用する
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(new_prefix.clone()),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("namespace_subscription が存在すること")
            .prefix,
        new_prefix
    );
    assert!(
        client
            .namespace_subscription(rid)
            .expect("namespace_subscription が存在すること")
            .active_suffixes
            .is_empty(),
        "旧 prefix 相対の suffix は破棄されること"
    );

    // 新 prefix 相対で同一 suffix 文字列 "live" を 2 回広告する (1 回目は新規、2 回目は抑止)
    for _ in 0..2 {
        server
            .send_namespace(rid, ns(&[b"live"]))
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ns_msg2) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(rid, ns_msg2)
            .expect("新基準の NAMESPACE は受理されること");
    }
    assert!(
        client
            .namespace_subscription(rid)
            .expect("namespace_subscription が存在すること")
            .active_suffixes
            .contains(&ns(&[b"live"])),
        "新 prefix 基準の suffix が active_suffixes に残ること"
    );

    let mut received = 0;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::NamespaceReceived { request_id, .. } = e
            && request_id == rid
        {
            received += 1;
        }
    }
    assert_eq!(
        received, 1,
        "prefix 更新後の NAMESPACE は新規 1 回だけ発行されること"
    );
}

/// prefix を変更しない REQUEST_UPDATE の REQUEST_OK では active_suffixes を破棄しない
#[test]
fn namespace_suffixes_are_kept_on_update_without_prefix() {
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(ns(&[b"example"]));

    server
        .send_namespace(rid, ns(&[b"live"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ns_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ns_msg)
        .expect("NAMESPACE は受理されること");

    // TRACK_NAMESPACE_PREFIX を含まない REQUEST_UPDATE → REQUEST_OK
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
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("namespace_subscription が存在すること")
            .prefix,
        ns(&[b"example"])
    );
    assert!(
        client
            .namespace_subscription(rid)
            .expect("namespace_subscription が存在すること")
            .active_suffixes
            .contains(&ns(&[b"live"])),
        "prefix 変更なしの REQUEST_OK では active_suffixes を破棄しないこと"
    );
}

/// prefix を P0 → P1 → P0 と更新した場合でも、旧基準の NAMESPACE_DONE を受理する
///
/// draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions): receiver は複数 REQUEST_UPDATE を
/// 累積結果のみ適用してよい (coalescing)。その場合 P1 は一度も適用されないため、P0 基準の
/// NAMESPACE_DONE は対応する NAMESPACE が先行しており §9.15 違反ではない。
#[test]
fn coalesced_prefix_revert_keeps_namespace_for_done() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    use shiguredo_moqt::session::types::SessionEvent;
    let old_prefix = ns(&[b"example"]);
    let new_prefix = ns(&[b"newprefix"]);
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(old_prefix.clone());

    // 旧 prefix 基準で suffix "live" を広告する
    server
        .send_namespace(rid, ns(&[b"live"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ns_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ns_msg)
        .expect("NAMESPACE は受理されること");

    // P0 → P1 と P1 → P0 を REQUEST_OK 前に連続送信する
    for prefix in [new_prefix.clone(), old_prefix.clone()] {
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_TRACK_NAMESPACE_PREFIX,
            value: MessageParameterValue::TrackNamespacePrefix(prefix),
        });
        client
            .send_request_update(rid, params)
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, upd_msg) = take_send_on_stream(&mut client);
        server
            .recv_stream_message(rid, upd_msg)
            .expect("テストフィクスチャの前提条件を満たす");
    }

    // responder が累積結果のみ適用した場合と同様に中間状態を広告せず REQUEST_OK を 2 件返す
    for _ in 0..2 {
        server
            .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok_msg) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(rid, ok_msg)
            .expect("テストフィクスチャの前提条件を満たす");
    }
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("namespace_subscription が存在すること")
            .prefix,
        old_prefix
    );

    // P0 基準の NAMESPACE_DONE は対応する NAMESPACE が先行しているため受理される
    server
        .send_namespace_done(rid, ns(&[b"live"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, done_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("旧 prefix 基準の NAMESPACE_DONE が受理されること");
    assert_eq!(client.state(), SessionState::Established);
    assert!(
        client
            .namespace_subscription(rid)
            .expect("namespace_subscription が存在すること")
            .active_suffixes
            .is_empty()
    );

    let mut done = 0;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::NamespaceDoneReceived { request_id, .. } = e
            && request_id == rid
        {
            done += 1;
        }
    }
    assert_eq!(done, 1);
}

/// prefix を短縮すると full namespace は新 prefix 配下の suffix に投影し直される
#[test]
fn namespace_suffix_is_reprojected_on_prefix_shortening() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let old_prefix = ns(&[b"a", b"b"]);
    let new_prefix = ns(&[b"a"]);
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(old_prefix);

    server
        .send_namespace(rid, ns(&[b"c"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ns_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ns_msg)
        .expect("NAMESPACE は受理されること");

    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(new_prefix.clone()),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("namespace_subscription が存在すること")
            .prefix,
        new_prefix
    );
    assert!(
        client
            .namespace_subscription(rid)
            .expect("namespace_subscription が存在すること")
            .active_suffixes
            .contains(&ns(&[b"b", b"c"])),
        "新 prefix 基準の suffix [b, c] に投影されること"
    );

    // 新基準の NAMESPACE_DONE が受理される
    server
        .send_namespace_done(rid, ns(&[b"b", b"c"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, done_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("新基準の NAMESPACE_DONE が受理されること");
    assert!(
        client
            .namespace_subscription(rid)
            .expect("namespace_subscription が存在すること")
            .active_suffixes
            .is_empty()
    );
}

/// coalescing で REQUEST_OK の間に届いた累積結果 prefix 基準の NAMESPACE_DONE も受理する
#[test]
fn coalesced_prefix_revert_accepts_done_between_request_oks() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let old_prefix = ns(&[b"example"]);
    let new_prefix = ns(&[b"newprefix"]);
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(old_prefix.clone());

    server
        .send_namespace(rid, ns(&[b"live"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ns_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ns_msg)
        .expect("NAMESPACE は受理されること");

    // P0 → P1 と P1 → P0 を REQUEST_OK 前に連続送信する
    for prefix in [new_prefix.clone(), old_prefix.clone()] {
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_TRACK_NAMESPACE_PREFIX,
            value: MessageParameterValue::TrackNamespacePrefix(prefix),
        });
        client
            .send_request_update(rid, params)
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, upd_msg) = take_send_on_stream(&mut client);
        server
            .recv_stream_message(rid, upd_msg)
            .expect("テストフィクスチャの前提条件を満たす");
    }

    // responder が累積結果 P0 のみ適用したまま OK1 を送り、その直後に P0 基準の DONE を送る
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok1_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok1_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_namespace_done(rid, ns(&[b"live"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, done_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("確定待ち prefix 基準の NAMESPACE_DONE が受理されること");
    assert_eq!(client.state(), SessionState::Established);

    // OK2 で累積結果 P0 が確定する
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok2_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok2_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("namespace_subscription が存在すること")
            .prefix,
        old_prefix
    );
    assert!(
        client
            .namespace_subscription(rid)
            .expect("namespace_subscription が存在すること")
            .active_suffixes
            .is_empty()
    );
}

/// prefix 更新の完了後に旧 prefix 基準の NAMESPACE_DONE が届くと §9.15 違反で閉じる
#[test]
fn stale_prefix_namespace_done_closes_session() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let old_prefix = ns(&[b"example"]);
    let new_prefix = ns(&[b"newprefix"]);
    let (mut client, mut server, rid) = establish_subscribe_namespace_with(old_prefix);

    server
        .send_namespace(rid, ns(&[b"live"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ns_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ns_msg)
        .expect("NAMESPACE は受理されること");

    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(new_prefix.clone()),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // 現在 prefix は ["newprefix"]。suffix "live" は ["newprefix", "live"] を意味し、
    // active な ["example", "live"] とは一致しないため §9.15 違反になる
    server
        .send_namespace_done(rid, ns(&[b"live"]))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, done_msg) = take_send_on_stream(&mut server);
    let err = client
        .recv_stream_message(rid, done_msg)
        .expect_err("旧 prefix 基準の NAMESPACE_DONE は §9.15 違反");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}
