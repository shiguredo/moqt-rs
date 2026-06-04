//! PUBLISH_NAMESPACE メッセージに関するテスト

use super::*;

/// Server-role が PUBLISH_NAMESPACE を発行する (odd Request ID 採番の role 対称性確認)
///
/// `send_publish_namespace` は client 側でカバー済みだが、
/// Server が odd parity (draft-ietf-moq-transport-21 §6.4.2.1 (Request ID)) で Request ID を採番して発行する経路は未確認だった。
#[test]
fn server_send_publish_namespace_full_cycle() {
    use shiguredo_moqt::session::types::NamespacePublicationState;
    let (mut client, mut server) = establish_pair();
    let rid = server
        .send_publish_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    // Server は odd parity で Request ID を採番する (draft-ietf-moq-transport-21 §6.4.2.1 (Request ID))
    assert_eq!(rid % 2, 1);
    let (_, pn_msg) = take_send_request(&mut server);
    client
        .recv_request(pn_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .namespace_publication(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        NamespacePublicationState::Established
    );
    assert_eq!(
        client
            .namespace_publication(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        NamespacePublicationState::Established
    );
}

/// PUBLISH_NAMESPACE 確立後は REQUEST_UPDATE → REQUEST_OK をやり取りできる
#[test]
fn publish_namespace_request_update_full_cycle() {
    use shiguredo_moqt::session::types::NamespacePublicationState;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

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
        "RequestUpdateReceived event expected for PUBLISH_NAMESPACE"
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
            .namespace_publication(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        NamespacePublicationState::Established
    );
    assert_eq!(
        server
            .namespace_publication(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        NamespacePublicationState::Established
    );
}

/// PUBLISH_NAMESPACE 確立後の REQUEST_ERROR (REQUEST_UPDATE 失敗応答) は
/// 送信・受信の両端で state を Terminated に遷移させ、遷移後の再 REQUEST_UPDATE は
/// PROTOCOL_VIOLATION で拒否される (draft-ietf-moq-transport-21 §9.5.1 (Updating
/// Subscriptions) の "the responder MUST close the bidi stream" 由来)。
#[test]
fn publish_namespace_request_error_in_established_transitions_to_terminated() {
    use shiguredo_moqt::message::ReasonPhrase;
    use shiguredo_moqt::session::types::NamespacePublicationState;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

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
            0x33,
            0,
            ReasonPhrase::new("update failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    // responder (server) 側で Terminated に遷移すること
    assert_eq!(
        server
            .namespace_publication(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        NamespacePublicationState::Terminated
    );
    let (_, err_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, err_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // initiator (client) 側でも Terminated に遷移すること
    assert_eq!(
        client
            .namespace_publication(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        NamespacePublicationState::Terminated
    );
    // 遷移後の再 REQUEST_UPDATE は Established 要求ガードで拒否されること
    let err = client
        .send_request_update(rid, MessageParameters::new())
        .expect_err("Terminated 遷移後の send_request_update は失敗するはず");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// PUBLISH_NAMESPACE の REQUEST_OK (Pending→Established) で
/// RequestOkReceived(request_kind=PublishNamespace) が発火する
#[test]
fn publish_namespace_request_ok_emits_request_ok_received_event_with_kind_publish_namespace() {
    use shiguredo_moqt::session::types::NamespacePublicationState;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pn_msg) = take_send_request(&mut client);
    server
        .recv_request(pn_msg)
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
            .namespace_publication(rid)
            .expect("namespace_publication が存在すること")
            .state,
        NamespacePublicationState::Established
    );

    // Pending→Established で RequestOkReceived(PublishNamespace) が発火する
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestOkReceived {
            request_id,
            request_kind,
            ..
        } = ev
            && request_id == rid
        {
            assert_eq!(request_kind, RequestKind::PublishNamespace);
            got = true;
            break;
        }
    }
    assert!(
        got,
        "PUBLISH_NAMESPACE の REQUEST_OK 受信時に RequestOkReceived(PublishNamespace) が発火すること"
    );
}

/// PUBLISH_NAMESPACE 確立後に REQUEST_UPDATE_OK を受信すると
/// RequestOkReceived(request_kind=PublishNamespace) が再度発火する
#[test]
fn publish_namespace_request_update_ok_emits_request_ok_received_event() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pn_msg) = take_send_request(&mut client);
    server
        .recv_request(pn_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

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
            assert_eq!(request_kind, RequestKind::PublishNamespace);
            consumed_first = true;
            break;
        }
    }
    assert!(
        consumed_first,
        "最初の PUBLISH_NAMESPACE REQUEST_OK (Pending→Established) 用 RequestOkReceived が発火すること"
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
            assert_eq!(request_kind, RequestKind::PublishNamespace);
            got = true;
            break;
        }
    }
    assert!(
        got,
        "PUBLISH_NAMESPACE の REQUEST_UPDATE_OK 受信時に RequestOkReceived(PublishNamespace) が発火すること"
    );
}

/// single period `.` 予約名前空間の PUBLISH_NAMESPACE は DOES_NOT_EXIST で拒否され、publication は作成されない
/// (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces))
#[test]
fn publish_namespace_single_period_rejected() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message::PublishNamespace;
    let (_, mut server) = establish_pair();
    server
        .recv_request(ControlMessage::PublishNamespace(PublishNamespace {
            request_id: 0,
            track_namespace: ns(&[b"."]),
            parameters: MessageParameters::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    // namespace publication は作成されない
    assert!(server.namespace_publication(0).is_none());
    let (_, err_msg) = take_send_on_stream(&mut server);
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_DOES_NOT_EXIST);
        }
        _ => panic!("DOES_NOT_EXIST の RequestError が期待される"),
    }
}

/// send_publish_namespace に single period `.` 予約名前空間を渡すと SESSION_PROTOCOL_VIOLATION が返る
/// (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces))
#[test]
fn send_publish_namespace_single_period_rejected() {
    let (mut client, _server) = establish_pair();
    let err = client
        .send_publish_namespace(ns(&[b"."]), MessageParameters::new())
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
}

/// スコープ外パラメータを含む `send_publish_namespace` は API 呼び出し時に
/// `Err(SendRequestError::Session(...))` を返し、状態・イベント・ request_id を汚染しないこと
///
/// draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope): 検証がないと、
/// NamespacePublication の登録・ control deadline の開始・ SendRequest の push まで
/// 完了した後に I/O 層のエンコード時 (validate_scope) で非同期に失敗する。
/// 検証は request_id 発行より前に置き、エラー時に欠番を作らない。
#[test]
fn send_publish_namespace_with_out_of_scope_parameter_rejected_without_state_change() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES,
    };
    use shiguredo_moqt::session::types::SendRequestError;
    let (mut client, _server) = establish_pair();
    let before = client.namespace_publications().count();
    // PUBLISH_NAMESPACE で許可されない EXPIRES を載せる
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(1000),
    });
    let err = client
        .send_publish_namespace(ns(&[b"example"]), params)
        .unwrap_err();
    match err {
        SendRequestError::Session(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("SendRequestError::Session が期待される: {other:?}"),
    }
    // NamespacePublication が登録されないこと
    assert_eq!(
        client.namespace_publications().count(),
        before,
        "検証エラー時に NamespacePublication が登録されないこと"
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
        .send_publish_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("検証エラー後の正常送信は成功すること");
    assert_eq!(rid, 0, "検証エラーで request_id が欠番にならないこと");
}

/// PUBLISH_NAMESPACE の REQUEST_OK に含まれる EXPIRES が
/// RequestOkReceived の parameters に伝播すること
///
/// draft-ietf-moq-transport-21 §9.3 (REQUEST_OK): NAMESPACE_OK_ALLOWED_PARAMS は
/// EXPIRES のみを許可する。受信側がパラメータを捨てると application が期限を扱えない。
#[test]
fn publish_namespace_request_ok_propagates_expires_parameter() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pn_msg) = take_send_request(&mut client);
    server
        .recv_request(pn_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(1234),
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
            assert_eq!(request_kind, RequestKind::PublishNamespace);
            assert_eq!(
                parameters.expires(),
                Some(1234),
                "REQUEST_OK の EXPIRES が RequestOkReceived に伝播すること"
            );
            got = true;
            break;
        }
    }
    assert!(
        got,
        "PUBLISH_NAMESPACE の REQUEST_OK 受信イベントが発火すること"
    );
}
