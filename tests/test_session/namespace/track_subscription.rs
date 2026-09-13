//! SUBSCRIBE_TRACKS メッセージに関するテスト

use super::*;

/// SUBSCRIBE_TRACKS を両側で Established にする (namespace=example, 空パラメータ)
///
/// 返り値は (subscriber, publisher, request_id)。
fn establish_subscribe_tracks_example() -> (Session, Session, u64) {
    establish_subscribe_tracks_with(ns(&[b"example"]), MessageParameters::new())
}

/// client から 1 本の SUBSCRIBE_TRACKS を確立する
///
/// 返り値は request_id。
fn establish_track_subscription(
    client: &mut Session,
    server: &mut Session,
    namespace: TrackNamespace,
    params: MessageParameters,
) -> u64 {
    let rid = client
        .send_subscribe_tracks(namespace, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, st_msg) = take_send_request(client);
    server
        .recv_request(st_msg)
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

/// 任意の namespace・パラメータで SUBSCRIBE_TRACKS を確立する
///
/// 返り値は (subscriber, publisher, request_id)。
fn establish_subscribe_tracks_with(
    namespace: TrackNamespace,
    params: MessageParameters,
) -> (Session, Session, u64) {
    let (mut client, mut server) = establish_pair();
    let rid = establish_track_subscription(&mut client, &mut server, namespace, params);
    (client, server, rid)
}

/// client から 2 本の SUBSCRIBE_TRACKS を確立する
///
/// 返り値は (subscriber, publisher, rid1, rid2)。
fn establish_two_track_subscriptions(
    prefix1: TrackNamespace,
    prefix2: TrackNamespace,
) -> (Session, Session, u64, u64) {
    let (mut client, mut server) = establish_pair();
    let rid1 =
        establish_track_subscription(&mut client, &mut server, prefix1, MessageParameters::new());
    let rid2 =
        establish_track_subscription(&mut client, &mut server, prefix2, MessageParameters::new());
    (client, server, rid1, rid2)
}

/// SUBSCRIBE_TRACKS の REQUEST_OK (Pending→Established) で
/// RequestOkReceived(request_kind=SubscribeTracks) が発火する
#[test]
fn subscribe_tracks_request_ok_emits_request_ok_received_event_with_kind_subscribe_tracks() {
    use shiguredo_moqt::session::types::TrackSubscriptionState;
    let (mut client, _server, rid) = establish_subscribe_tracks_example();
    assert_eq!(
        client
            .track_subscription(rid)
            .expect("track_subscription が存在すること")
            .state,
        TrackSubscriptionState::Established
    );

    // Pending→Established で RequestOkReceived(SubscribeTracks) が発火する
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestOkReceived {
            request_id,
            request_kind,
            ..
        } = ev
            && request_id == rid
        {
            assert_eq!(request_kind, RequestKind::SubscribeTracks);
            got = true;
            break;
        }
    }
    assert!(
        got,
        "SUBSCRIBE_TRACKS の REQUEST_OK 受信時に RequestOkReceived(SubscribeTracks) が発火すること"
    );
}

/// SUBSCRIBE_TRACKS 確立後に REQUEST_UPDATE_OK を受信すると
/// RequestOkReceived(request_kind=SubscribeTracks) が再度発火する
#[test]
fn subscribe_tracks_request_update_ok_emits_request_ok_received_event() {
    let (mut client, mut server, rid) = establish_subscribe_tracks_example();

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
            assert_eq!(request_kind, RequestKind::SubscribeTracks);
            consumed_first = true;
            break;
        }
    }
    assert!(
        consumed_first,
        "最初の SUBSCRIBE_TRACKS REQUEST_OK (Pending→Established) 用 RequestOkReceived が発火すること"
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
            assert_eq!(request_kind, RequestKind::SubscribeTracks);
            got = true;
            break;
        }
    }
    assert!(
        got,
        "SUBSCRIBE_TRACKS の REQUEST_UPDATE_OK 受信時に RequestOkReceived(SubscribeTracks) が発火すること"
    );
}

/// SUBSCRIBE_TRACKS 確立後、新規 bidi stream の先頭 PUBLISH (recv_request 経由) は
/// active_track_aliases に alias を追加する
#[test]
fn subscribe_tracks_publish_accepted_adds_active_track_alias() {
    use shiguredo_moqt::message::Publish;
    use shiguredo_moqt::session::types::TrackSubscriptionState;
    let (mut client, _server, rid) = establish_subscribe_tracks_example();
    assert_eq!(
        client
            .track_subscription(rid)
            .expect("track_subscription が存在すること")
            .state,
        TrackSubscriptionState::Established
    );

    // server 側から新規 bidi stream の先頭メッセージとして PUBLISH を送信 (request_id=1: server 起点)
    let alias = 42;
    let publish_msg = ControlMessage::Publish(Publish {
        request_id: 1,
        track_namespace: ns(&[b"example", b"live"]),
        track_name: b"cam".to_vec(),
        track_alias: alias,
        parameters: MessageParameters::new(),
        track_properties: TrackProperties::new(),
    });
    client
        .recv_request(publish_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let entry = client
        .track_subscription(rid)
        .expect("track_subscription が存在すること");
    assert!(entry.active_track_aliases.contains(&alias));
}

/// GOAWAY 送信後に新規 bidi stream の先頭 PUBLISH (recv_request 経由) を受信しても
/// active_track_aliases に alias を追加しない
#[test]
fn subscribe_tracks_publish_rejected_after_goaway_does_not_add_alias() {
    use shiguredo_moqt::message::Publish;
    use shiguredo_moqt::session::types::TrackSubscriptionState;
    let (mut client, mut server, rid) = establish_subscribe_tracks_example();
    assert_eq!(
        client
            .track_subscription(rid)
            .expect("track_subscription が存在すること")
            .state,
        TrackSubscriptionState::Established
    );

    // client が GOAWAY を送信し、server がそれを受信する
    client
        .send_goaway(Vec::new(), 10000)
        .expect("テストフィクスチャの前提条件を満たす");
    let go_msg = take_send_control(&mut client);
    server
        .recv_control(go_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // GOAWAY 送信後の新規 PUBLISH は GOING_AWAY で拒否され alias は追加されない
    let alias = 43;
    let publish_msg = ControlMessage::Publish(Publish {
        request_id: 1,
        track_namespace: ns(&[b"example", b"live"]),
        track_name: b"cam".to_vec(),
        track_alias: alias,
        parameters: MessageParameters::new(),
        track_properties: TrackProperties::new(),
    });
    client
        .recv_request(publish_msg)
        .expect("recv_request for publish (GOING_AWAY 拒否は Ok)");
    let entry = client
        .track_subscription(rid)
        .expect("track_subscription が存在すること");
    assert!(!entry.active_track_aliases.contains(&alias));
}

/// session-level track への PUBLISH (recv_request 経由) は拒否され、active_track_aliases に alias を追加しない
#[test]
fn subscribe_tracks_publish_rejected_session_level_track_does_not_add_alias() {
    use shiguredo_moqt::message::Publish;
    use shiguredo_moqt::session::types::TrackSubscriptionState;
    let (mut client, _server, rid) = establish_subscribe_tracks_example();
    assert_eq!(
        client
            .track_subscription(rid)
            .expect("track_subscription が存在すること")
            .state,
        TrackSubscriptionState::Established
    );

    // .session 名前空間の PUBLISH は DOES_NOT_EXIST で拒否され alias は追加されない
    let alias = 44;
    let publish_msg = ControlMessage::Publish(Publish {
        request_id: 1,
        track_namespace: ns(&[b".session"]),
        track_name: b"cam".to_vec(),
        track_alias: alias,
        parameters: MessageParameters::new(),
        track_properties: TrackProperties::new(),
    });
    client
        .recv_request(publish_msg)
        .expect("recv_request for publish (.session 拒否は Ok)");
    let entry = client
        .track_subscription(rid)
        .expect("track_subscription が存在すること");
    assert!(!entry.active_track_aliases.contains(&alias));
}

/// 未知の必須 track property を含む PUBLISH (recv_request 経由) は拒否され、
/// active_track_aliases に alias を追加しない
#[test]
fn subscribe_tracks_publish_rejected_unknown_mandatory_property_does_not_add_alias() {
    use shiguredo_moqt::message::Publish;
    use shiguredo_moqt::session::types::TrackSubscriptionState;
    use shiguredo_moqt::track_properties::{TrackProperty, TrackPropertyValue};
    let (mut client, _server, rid) = establish_subscribe_tracks_example();
    assert_eq!(
        client
            .track_subscription(rid)
            .expect("track_subscription が存在すること")
            .state,
        TrackSubscriptionState::Established
    );

    // 未知の必須 track property を含む PUBLISH は UNSUPPORTED_EXTENSION で拒否され alias は追加されない
    let mut track_properties = TrackProperties::new();
    track_properties.push(TrackProperty {
        prop_type: 0x4000,
        value: TrackPropertyValue::VarInt(1),
    });
    let alias = 45;
    let publish_msg = ControlMessage::Publish(Publish {
        request_id: 1,
        track_namespace: ns(&[b"example", b"live"]),
        track_name: b"cam".to_vec(),
        track_alias: alias,
        parameters: MessageParameters::new(),
        track_properties,
    });
    client
        .recv_request(publish_msg)
        .expect("recv_request for publish (unknown mandatory 拒否は Ok)");
    let entry = client
        .track_subscription(rid)
        .expect("track_subscription が存在すること");
    assert!(!entry.active_track_aliases.contains(&alias));
}

/// 既存購読がある Track への PUBLISH (recv_request 経由) も draft-20 では受理され alias が追加される
/// draft-ietf-moq-transport-21 §3.1.1: 同一 Track への複数同時 subscription が許可されたため、
/// 既存 subscription がある Track への PUBLISH も受理され alias が追加される
#[test]
fn subscribe_tracks_publish_accepted_with_existing_subscription_in_draft_20() {
    use shiguredo_moqt::message::Publish;
    use shiguredo_moqt::session::types::TrackSubscriptionState;
    let (mut client, mut server) = establish_pair();

    // 同じ track に対する subscriber 側の購読を先に Established にする
    let sub_rid = client
        .send_subscribe(
            ns(&[b"example", b"live"]),
            b"cam".to_vec(),
            MessageParameters::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            sub_rid,
            100,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let (mut client, _server, rid) = establish_subscribe_tracks_example();
    assert_eq!(
        client
            .track_subscription(rid)
            .expect("track_subscription が存在すること")
            .state,
        TrackSubscriptionState::Established
    );

    // draft-20: 既存 subscription があっても PUBLISH は受理され alias が追加される
    let alias = 46;
    let publish_msg = ControlMessage::Publish(Publish {
        request_id: 1,
        track_namespace: ns(&[b"example", b"live"]),
        track_name: b"cam".to_vec(),
        track_alias: alias,
        parameters: MessageParameters::new(),
        track_properties: TrackProperties::new(),
    });
    client
        .recv_request(publish_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let entry = client
        .track_subscription(rid)
        .expect("track_subscription が存在すること");
    assert!(entry.active_track_aliases.contains(&alias));
}

// ─── SUBSCRIBE_TRACKS の .session / single period 予約 namespace 拒否 ──────────────────

/// .session で始まる track_namespace_prefix の SUBSCRIBE_TRACKS は DOES_NOT_EXIST で拒否され、
/// track_subscriptions に entry は登録されず SubscribeTracksReceived イベントも発生しない
/// (draft-ietf-moq-transport-21 §6.5 (Session-Level Tracks and Namespaces))
#[test]
fn subscribe_tracks_session_namespace_rejected() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message::SubscribeTracks;
    let (_, mut server) = establish_pair();
    // .session 予約名前空間の SUBSCRIBE_TRACKS を raw ワイヤメッセージとして注入する
    server
        .recv_request(ControlMessage::SubscribeTracks(SubscribeTracks {
            request_id: 0,
            track_namespace_prefix: ns(&[b".session"]),
            parameters: MessageParameters::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    // track_subscriptions に entry が登録されないことを確認する
    assert!(server.track_subscription(0).is_none());
    // REQUEST_DOES_NOT_EXIST の RequestError が送信されることを確認する
    let (_, err_msg) = take_send_on_stream(&mut server);
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_DOES_NOT_EXIST);
        }
        _ => panic!("DOES_NOT_EXIST の RequestError が期待される"),
    }
    // SubscribeTracksReceived イベントが発生しないことを確認する
    while let Some(ev) = server.poll_event() {
        assert!(
            !matches!(ev, SessionEvent::SubscribeTracksReceived { .. }),
            "SubscribeTracksReceived イベントは発生してはならない"
        );
    }
}

/// single period `.` の track_namespace_prefix の SUBSCRIBE_TRACKS は DOES_NOT_EXIST で拒否され、
/// track_subscriptions に entry は登録されず SubscribeTracksReceived イベントも発生しない
/// (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces))
#[test]
fn subscribe_tracks_single_period_namespace_rejected() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message::SubscribeTracks;
    let (_, mut server) = establish_pair();
    // single period `.` 予約名前空間の SUBSCRIBE_TRACKS を raw ワイヤメッセージとして注入する
    server
        .recv_request(ControlMessage::SubscribeTracks(SubscribeTracks {
            request_id: 0,
            track_namespace_prefix: ns(&[b"."]),
            parameters: MessageParameters::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    // track_subscriptions に entry が登録されないことを確認する
    assert!(server.track_subscription(0).is_none());
    // REQUEST_DOES_NOT_EXIST の RequestError が送信されることを確認する
    let (_, err_msg) = take_send_on_stream(&mut server);
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_DOES_NOT_EXIST);
        }
        _ => panic!("DOES_NOT_EXIST の RequestError が期待される"),
    }
    // SubscribeTracksReceived イベントが発生しないことを確認する
    while let Some(ev) = server.poll_event() {
        assert!(
            !matches!(ev, SessionEvent::SubscribeTracksReceived { .. }),
            "SubscribeTracksReceived イベントは発生してはならない"
        );
    }
}

/// send_subscribe_tracks に single period `.` 予約名前空間を渡すと SESSION_PROTOCOL_VIOLATION が返る
/// (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces))
#[test]
fn send_subscribe_tracks_single_period_rejected() {
    let (mut client, _server) = establish_pair();
    let err = client
        .send_subscribe_tracks(ns(&[b"."]), MessageParameters::new())
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
}

/// 既存 bidi request stream 上で PUBLISH を recv_stream_message に渡すと PROTOCOL_VIOLATION
/// draft-ietf-moq-transport-21 §9 Table 5: PUBLISH (0x1D) は Request, First。
/// Messages marked "First" MUST be the first message on a new request stream.
#[test]
fn publish_on_existing_bidi_stream_is_protocol_violation() {
    use shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION;
    use shiguredo_moqt::message::Publish;
    use shiguredo_moqt::session::types::TrackSubscriptionState;
    let (mut client, _server, rid) = establish_subscribe_tracks_example();
    assert_eq!(
        client
            .track_subscription(rid)
            .expect("track_subscription が存在すること")
            .state,
        TrackSubscriptionState::Established
    );

    // 既存 bidi stream 上の PUBLISH は PROTOCOL_VIOLATION
    let publish_msg = ControlMessage::Publish(Publish {
        request_id: rid,
        track_namespace: ns(&[b"example", b"live"]),
        track_name: b"cam".to_vec(),
        track_alias: 99,
        parameters: MessageParameters::new(),
        track_properties: TrackProperties::new(),
    });
    let err = client
        .recv_stream_message(rid, publish_msg)
        .expect_err("既存 bidi stream 上の PUBLISH は PROTOCOL_VIOLATION でなければならない");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// 既存 bidi request stream 上で SUBSCRIBE を recv_stream_message に渡すと PROTOCOL_VIOLATION (非回帰)
/// draft-ietf-moq-transport-21 §9 Table 5: SUBSCRIBE (0x03) も Request, First。
#[test]
fn subscribe_on_existing_bidi_stream_is_protocol_violation() {
    use shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION;
    use shiguredo_moqt::message::Subscribe;
    use shiguredo_moqt::session::types::TrackSubscriptionState;
    let (mut client, _server, rid) = establish_subscribe_tracks_example();
    assert_eq!(
        client
            .track_subscription(rid)
            .expect("track_subscription が存在すること")
            .state,
        TrackSubscriptionState::Established
    );

    // 既存 bidi stream 上の SUBSCRIBE は PROTOCOL_VIOLATION (First メッセージの非回帰確認)
    let subscribe_msg = ControlMessage::Subscribe(Subscribe {
        request_id: rid,
        track_namespace: ns(&[b"example", b"live"]),
        track_name: b"cam".to_vec(),
        parameters: MessageParameters::new(),
    });
    let err = client
        .recv_stream_message(rid, subscribe_msg)
        .expect_err("既存 bidi stream 上の SUBSCRIBE は PROTOCOL_VIOLATION でなければならない");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

// ─── GROUP_ORDER 値域検証 (draft §9.20.9) ──────────────────────────────────

/// GROUP_ORDER 値域外の SUBSCRIBE_TRACKS 受信は PROTOCOL_VIOLATION でセッションを閉じる
///
/// draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): SUBSCRIBE_TRACKS は
/// GROUP_ORDER の出現を許可する (MAY)。値域外の値は
/// "MUST close the session with PROTOCOL_VIOLATION"。ワイヤデコード層は値域外を
/// 弾くため、セッション層に到達するのはアプリが MessageParameters を直接構築して
/// 渡した場合のみ。
#[test]
fn subscribe_tracks_with_invalid_group_order_closes_session() {
    use shiguredo_moqt::message::SubscribeTracks;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_GROUP_ORDER,
    };
    let (_client, mut server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(3), // 値域外 (Ascending=0x1 / Descending=0x2)
    });
    let err = server
        .recv_request(ControlMessage::SubscribeTracks(SubscribeTracks {
            request_id: 0,
            track_namespace_prefix: ns(&[b"live"]),
            parameters: params,
        }))
        .expect_err("値域外 GROUP_ORDER は PROTOCOL_VIOLATION になる");
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    match drain_until_close(&mut server) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("CloseSession(PROTOCOL_VIOLATION) が期待されたが {other:?}"),
    }
}

/// 値域外 GROUP_ORDER を含む `send_subscribe_tracks` は API 呼び出し時に
/// `Err(SendRequestError::Session(...))` を返し、状態・イベント・ request_id を汚染しないこと
///
/// draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): "The allowed values are
/// Ascending (0x1) or Descending (0x2)." 検証がないと、TrackSubscription の登録・
/// control deadline の開始・ SendRequest の push まで完了した後に I/O 層のエンコード時
/// (validate_param_encoding) で非同期に失敗する。検証は request_id 発行より前に置き、
/// エラー時に欠番を作らない。
#[test]
fn send_subscribe_tracks_with_invalid_group_order_returns_error_without_state_change() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_GROUP_ORDER,
    };
    use shiguredo_moqt::session::types::SendRequestError;
    let (mut client, _server) = establish_pair();
    let before = client.track_subscriptions().count();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(3), // 値域外 (Ascending=0x1 / Descending=0x2)
    });
    let err = client
        .send_subscribe_tracks(ns(&[b"live"]), params)
        .unwrap_err();
    match err {
        SendRequestError::Session(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("SendRequestError::Session が期待される: {other:?}"),
    }
    // TrackSubscription が登録されないこと
    assert_eq!(
        client.track_subscriptions().count(),
        before,
        "検証エラー時に TrackSubscription が登録されないこと"
    );
    // SendRequest イベントが積まれないこと
    let mut sent = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::SendRequest { .. }) {
            sent = true;
        }
    }
    assert!(!sent, "検証エラー時に SendRequest イベントが積まれないこと");
    // 検証エラー後に正常な送信が継続できること (フレッシュセッションでは最初の
    // 正常送信の request_id が 0 になる = 検証エラーで request_id が欠番にならない)
    let rid = client
        .send_subscribe_tracks(ns(&[b"live"]), MessageParameters::new())
        .expect("検証エラー後の正常送信は成功すること");
    assert_eq!(rid, 0, "検証エラーで request_id が欠番にならないこと");
}

/// 値域外 GROUP_ORDER を含む `send_subscribe_tracks` で control deadline が開始されないこと
///
/// 検証エラー時に deadline が開始されると、アプリがエンコードエラーを無視した場合に
/// `SESSION_CONTROL_MESSAGE_TIMEOUT` という後続の誤作動につながる。
/// 手順: (a) timeout 設定は検証エラーとなる送信より前に呼ぶ (b) tick は送信前に 1 回打って
/// `last_tick_ms` を確定させたうえで、期限超過となる絶対時刻で tick する
/// (c) 正常送信を行うテストとは分けて実施する (正常送信後は deadline が開始されるため)。
/// 本テストは負の検証のみのため、deadline 機構が丸ごと壊れた場合も pass する
/// (false negative)。正の経路 (deadline 開始 → CloseSession) は timeout_api.rs の
/// `control_message_timeout_*` テスト群が検証している。
#[test]
fn send_subscribe_tracks_invalid_group_order_does_not_start_control_deadline() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_GROUP_ORDER,
    };
    let (mut client, _server) = establish_pair();
    client.tick(0);
    client.set_control_message_timeout_ms(Some(10));
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(3), // 値域外
    });
    let _ = client
        .send_subscribe_tracks(ns(&[b"live"]), params)
        .expect_err("値域外 GROUP_ORDER は API 呼び出し時にエラーになること");
    // 期限超過となる絶対時刻で tick しても CloseSession が出ない (deadline が開始されていない)
    client.tick(100);
    let mut closed = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::CloseSession(_)) {
            closed = true;
        }
    }
    assert!(
        !closed,
        "検証エラー時に control deadline が開始されないこと"
    );
}

/// 値域外 FORWARD を含む `send_subscribe_tracks` は API 呼び出し時に
/// `Err(SendRequestError::Session(...))` を返し、状態・イベント・ request_id を汚染しないこと
///
/// draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter): "The allowed values are 0
/// (don't forward) or 1 (forward)." 検証がないと、TrackSubscription の登録・ control
/// deadline の開始・ SendRequest の push まで完了した後に I/O 層のエンコード時
/// (validate_param_encoding) で非同期に失敗する。検証は request_id 発行より前に置き、
/// エラー時に欠番を作らない。
#[test]
fn send_subscribe_tracks_with_invalid_forward_returns_error_without_state_change() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    use shiguredo_moqt::session::types::SendRequestError;
    let (mut client, _server) = establish_pair();
    let before = client.track_subscriptions().count();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(2), // 値域外 (0 / 1)
    });
    let err = client
        .send_subscribe_tracks(ns(&[b"live"]), params)
        .unwrap_err();
    match err {
        SendRequestError::Session(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("SendRequestError::Session が期待される: {other:?}"),
    }
    // TrackSubscription が登録されないこと
    assert_eq!(
        client.track_subscriptions().count(),
        before,
        "検証エラー時に TrackSubscription が登録されないこと"
    );
    // SendRequest イベントが積まれないこと
    let mut sent = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::SendRequest { .. }) {
            sent = true;
        }
    }
    assert!(!sent, "検証エラー時に SendRequest イベントが積まれないこと");
    // 検証エラー後に正常な送信が継続できること (フレッシュセッションでは最初の
    // 正常送信の request_id が 0 になる = 検証エラーで request_id が欠番にならない)
    let rid = client
        .send_subscribe_tracks(ns(&[b"live"]), MessageParameters::new())
        .expect("検証エラー後の正常送信は成功すること");
    assert_eq!(rid, 0, "検証エラーで request_id が欠番にならないこと");
}

/// 値域外 FORWARD を含む `send_subscribe_tracks` で control deadline が開始されないこと
///
/// 検証エラー時に deadline が開始されると、アプリがエンコードエラーを無視した場合に
/// `SESSION_CONTROL_MESSAGE_TIMEOUT` という後続の誤作動につながる。
/// 手順: (a) timeout 設定は検証エラーとなる送信より前に呼ぶ (b) tick は送信前に 1 回打って
/// `last_tick_ms` を確定させたうえで、期限超過となる絶対時刻で tick する
/// (c) 正常送信を行うテストとは分けて実施する (正常送信後は deadline が開始されるため)。
/// 本テストは負の検証のみのため、deadline 機構が丸ごと壊れた場合も pass する
/// (false negative)。正の経路 (deadline 開始 → CloseSession) は timeout_api.rs の
/// `control_message_timeout_*` テスト群が検証している。
#[test]
fn send_subscribe_tracks_invalid_forward_does_not_start_control_deadline() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, _server) = establish_pair();
    client.tick(0);
    client.set_control_message_timeout_ms(Some(10));
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(2), // 値域外
    });
    let _ = client
        .send_subscribe_tracks(ns(&[b"live"]), params)
        .expect_err("値域外 FORWARD は API 呼び出し時にエラーになること");
    // 期限超過となる絶対時刻で tick しても CloseSession が出ない (deadline が開始されていない)
    client.tick(100);
    let mut closed = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::CloseSession(_)) {
            closed = true;
        }
    }
    assert!(
        !closed,
        "検証エラー時に control deadline が開始されないこと"
    );
}

/// 正当な FORWARD (0 / 1) を含む `send_subscribe_tracks` が成功し、送信メッセージに
/// アプリが渡した値と同一の FORWARD が含まれること
///
/// 値域検証が 0 / 1 を誤って拒否しないことの正のケース。
#[test]
fn send_subscribe_tracks_with_valid_forward_accepted() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, _server) = establish_pair();
    // 同一 prefix への 2 回目の SUBSCRIBE_TRACKS は overlap で拒否されるため、
    // 値ごとに prefix を変える
    for (idx, valid) in [0u8, 1u8].iter().enumerate() {
        let prefix = if idx == 0 {
            ns(&[b"live"])
        } else {
            ns(&[b"archive"])
        };
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_FORWARD,
            value: MessageParameterValue::Uint8(*valid),
        });
        let rid = client
            .send_subscribe_tracks(prefix, params.clone())
            .expect("正当な FORWARD を含む送信は成功すること");
        let (sent_rid, msg) = take_send_request(&mut client);
        assert_eq!(sent_rid, rid);
        match &msg {
            ControlMessage::SubscribeTracks(st) => {
                assert_eq!(
                    st.parameters.forward(),
                    Some(*valid),
                    "送信メッセージにアプリが渡した FORWARD と同一の値が含まれること"
                );
            }
            other => panic!("SubscribeTracks が期待されたが {other:?} を受け取った"),
        }
    }
}

/// SUBSCRIBE_TRACKS の FORWARD 値域外 (0 / 1 以外) 受信で、`Err(RecvRequestError::Session(...))` と
/// `CloseSession(PROTOCOL_VIOLATION)` の両方が発生し、セッションが閉じること
///
/// draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter): "If an endpoint receives a
/// value outside this range, it MUST close the session with PROTOCOL_VIOLATION."
/// `?` 伝播のみだと `RecvRequestError::Session` の契約 (session state は Closing に遷移済み) が
/// 破れ、セッションが開いたまま残る。GROUP_ORDER の値域検証と同じパターンで
/// `self.fail()` に接続した。
#[test]
fn subscribe_tracks_with_invalid_forward_closes_session() {
    use shiguredo_moqt::message::SubscribeTracks;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (_client, mut server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(2), // 値域外 (0 / 1)
    });
    let err = server
        .recv_request(ControlMessage::SubscribeTracks(SubscribeTracks {
            request_id: 0,
            track_namespace_prefix: ns(&[b"live"]),
            parameters: params,
        }))
        .expect_err("値域外 FORWARD は PROTOCOL_VIOLATION になる");
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    // 検証失敗時に TrackSubscription が登録されないこと (検証は insert より前)
    assert!(
        server.track_subscription(0).is_none(),
        "検証エラー時に TrackSubscription が登録されないこと"
    );
    match drain_until_close(&mut server) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("CloseSession(PROTOCOL_VIOLATION) が期待されたが {other:?}"),
    }
}

/// FORWARD=0 / 1 を含む SUBSCRIBE_TRACKS の受信で `SubscribeTracksReceived` が発火し、
/// `forward_state` が設定されること
///
/// 検証の `Ok` 値が `forward_state` の設定に使われる (検証と設定の分離)。
/// FORWARD 不在 (デフォルト 1) のケースも含める。
#[test]
fn subscribe_tracks_with_forward_sets_forward_state() {
    use shiguredo_moqt::message::SubscribeTracks;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let cases: [(Option<u8>, u8); 3] = [
        (None, 1), // FORWARD 不在 → デフォルト 1
        (Some(0), 0),
        (Some(1), 1),
    ];
    for (idx, (forward, expected)) in cases.iter().enumerate() {
        let (_client, mut server) = establish_pair();
        let mut params = MessageParameters::new();
        if let Some(f) = forward {
            params.push(MessageParameter {
                param_type: PARAM_FORWARD,
                value: MessageParameterValue::Uint8(*f),
            });
        }
        // request id は Client 役の偶数採番 (0, 2, 4) を使う (奇数は parity 不正で fail する)
        let rid = (idx as u64) * 2;
        server
            .recv_request(ControlMessage::SubscribeTracks(SubscribeTracks {
                request_id: rid,
                track_namespace_prefix: ns(&[b"live"]),
                parameters: params.clone(),
            }))
            .expect("正当な FORWARD の SUBSCRIBE_TRACKS は受理されること");
        // forward_state が設定されること
        assert_eq!(
            server
                .track_subscription(rid)
                .expect("track subscription が存在する")
                .forward_state,
            *expected,
            "FORWARD={forward:?} で forward_state が {expected} になること"
        );
        // SubscribeTracksReceived が発火すること
        let mut got = false;
        while let Some(ev) = server.poll_event() {
            if let SessionEvent::SubscribeTracksReceived {
                request_id,
                parameters,
                ..
            } = ev
                && request_id == rid
            {
                assert_eq!(parameters.forward(), *forward);
                got = true;
                break;
            }
        }
        assert!(
            got,
            "FORWARD={forward:?} で SubscribeTracksReceived が発火すること"
        );
    }
}

/// スコープ外パラメータを含む `send_subscribe_tracks` は API 呼び出し時に
/// `Err(SendRequestError::Session(...))` を返し、状態・イベント・ request_id を汚染しないこと
///
/// draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope): 検証がないと、TrackSubscription
/// の登録・ control deadline の開始・ SendRequest の push まで完了した後に I/O 層の
/// エンコード時 (validate_scope) で非同期に失敗する。検証は request_id 発行より前に
/// 置き、エラー時に欠番を作らない。
#[test]
fn send_subscribe_tracks_with_out_of_scope_parameter_rejected_without_state_change() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES, PARAM_FORWARD,
    };
    use shiguredo_moqt::session::types::SendRequestError;
    let (mut client, _server) = establish_pair();
    let before = client.track_subscriptions().count();
    // スコープ外の EXPIRES と、スコープ内の FORWARD を混在させる
    // (スコープ内パラメータを含んでも検証エラーになることを確認する)
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(1000),
    });
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    let err = client
        .send_subscribe_tracks(ns(&[b"live"]), params)
        .unwrap_err();
    match err {
        SendRequestError::Session(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("SendRequestError::Session が期待される: {other:?}"),
    }
    // TrackSubscription が登録されないこと
    assert_eq!(
        client.track_subscriptions().count(),
        before,
        "検証エラー時に TrackSubscription が登録されないこと"
    );
    // SendRequest イベントが積まれないこと
    let mut sent = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::SendRequest { .. }) {
            sent = true;
        }
    }
    assert!(!sent, "検証エラー時に SendRequest イベントが積まれないこと");
    // 検証エラー後に正常な送信が継続できること (フレッシュセッションでは最初の
    // 正常送信の request_id が 0 になる = 検証エラーで request_id が欠番にならない)
    let rid = client
        .send_subscribe_tracks(ns(&[b"live"]), MessageParameters::new())
        .expect("検証エラー後の正常送信は成功すること");
    assert_eq!(rid, 0, "検証エラーで request_id が欠番にならないこと");
}

/// スコープ外パラメータを含む `send_subscribe_tracks` で control deadline が開始されないこと
///
/// 検証エラー時に deadline が開始されると、アプリがエンコードエラーを無視した場合に
/// `SESSION_CONTROL_MESSAGE_TIMEOUT` という後続の誤作動につながる。
/// 手順: (a) timeout 設定は検証エラーとなる送信より前に呼ぶ (b) tick は送信前に 1 回打って
/// `last_tick_ms` を確定させたうえで、期限超過となる絶対時刻で tick する
/// (c) 正常送信を行うテストとは分けて実施する (正常送信後は deadline が開始されるため)。
/// 本テストは負の検証のみのため、deadline 機構が丸ごと壊れた場合も pass する
/// (false negative)。正の経路 (deadline 開始 → CloseSession) は timeout_api.rs の
/// `control_message_timeout_*` テスト群が検証している。
#[test]
fn send_subscribe_tracks_out_of_scope_parameter_does_not_start_control_deadline() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES,
    };
    let (mut client, _server) = establish_pair();
    client.tick(0);
    client.set_control_message_timeout_ms(Some(10));
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(1000),
    });
    let _ = client
        .send_subscribe_tracks(ns(&[b"live"]), params)
        .expect_err("スコープ外パラメータは API 呼び出し時にエラーになること");
    // 期限超過となる絶対時刻で tick しても CloseSession が出ない (deadline が開始されていない)
    client.tick(100);
    let mut closed = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::CloseSession(_)) {
            closed = true;
        }
    }
    assert!(
        !closed,
        "検証エラー時に control deadline が開始されないこと"
    );
}

/// スコープ内パラメータ (FORWARD + GROUP_ORDER) を含む `send_subscribe_tracks` が成功し、
/// 送信メッセージにアプリが渡した値と同一のパラメータが含まれること
///
/// スコープ検証が許可パラメータを誤って拒否しないことの正のケース。
#[test]
fn send_subscribe_tracks_with_in_scope_parameters_accepted() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD, PARAM_GROUP_ORDER,
    };
    let (mut client, _server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(2),
    });
    let rid = client
        .send_subscribe_tracks(ns(&[b"live"]), params.clone())
        .expect("スコープ内パラメータを含む送信は成功すること");
    let (sent_rid, msg) = take_send_request(&mut client);
    assert_eq!(sent_rid, rid);
    match &msg {
        ControlMessage::SubscribeTracks(st) => {
            assert_eq!(
                st.parameters.forward(),
                Some(1),
                "送信メッセージにアプリが渡した FORWARD と同一の値が含まれること"
            );
            assert_eq!(
                st.parameters.group_order(),
                Some(2),
                "送信メッセージにアプリが渡した GROUP_ORDER と同一の値が含まれること"
            );
        }
        other => panic!("SubscribeTracks が期待されたが {other:?} を受け取った"),
    }
}

/// SUBSCRIBE_TRACKS の REQUEST_UPDATE + FORWARD=0 で track subscription の forward_state が更新される
///
/// draft-ietf-moq-transport-21 Appendix A.1 #1812 / §9.20.19 (FORWARD Parameter):
/// FORWARD は SUBSCRIBE_TRACKS の REQUEST_UPDATE に出現可能であり、
/// 将来マッチする subscription の Forwarding State を指定する。
#[test]
fn subscribe_tracks_request_update_forward_updates_forward_state() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    use shiguredo_moqt::session::types::TrackSubscriptionState;
    let (mut client, mut server, rid) = establish_subscribe_tracks_example();
    assert_eq!(
        server
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1,
        "初期値は default 1 であること"
    );

    // FORWARD=0 の REQUEST_UPDATE で更新する
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let entry = server
        .track_subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        entry.forward_state, 0,
        "REQUEST_UPDATE の FORWARD=0 が forward_state に反映されること"
    );
    assert_eq!(
        entry.state,
        TrackSubscriptionState::Established,
        "更新後も Established のままであること"
    );
}

/// FORWARD 更新値は application 層が新規マッチ Track への PUBLISH に明示載せする
///
/// draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter):
/// 更新値は将来マッチする subscription の Forwarding State を指定する。
/// PUBLISH への載せ方は application 層が `track_subscription()` の保持値を
/// 参照して明示的に行う (Session は自動載せしない。明示値を優先する)。
#[test]
fn subscribe_tracks_request_update_forward_explicit_in_new_publish_by_app() {
    use shiguredo_moqt::message::Publish;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, mut server, rid) = establish_subscribe_tracks_example();

    // FORWARD=0 に更新する
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // application 層は保持値を参照して新規 PUBLISH の FORWARD に載せる
    let saved = server
        .track_subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす")
        .forward_state;
    assert_eq!(saved, 0, "保存値が更新値を反映すること");
    let mut pub_params = MessageParameters::new();
    pub_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(saved),
    });
    let pub_rid = server
        .send_publish(
            ns(&[b"example"]),
            b"new".to_vec(),
            21,
            pub_params,
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (sent_rid, pub_msg) = take_send_request(&mut server);
    assert_eq!(sent_rid, pub_rid);
    match &pub_msg {
        ControlMessage::Publish(Publish { parameters, .. }) => {
            assert_eq!(
                parameters.forward(),
                Some(0),
                "新規 PUBLISH が更新 Forward State を反映すること"
            );
        }
        other => panic!("Publish が期待されたが {other:?} を受け取った"),
    }
    client
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(pub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        0,
        "受信側 subscription の forward_state が更新値を反映すること"
    );
}

/// SUBSCRIBE_TRACKS の FORWARD 更新は既存 Established subscription に影響しない
///
/// draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter)
#[test]
fn subscribe_tracks_request_update_forward_does_not_affect_existing_subscription() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, mut server) = establish_pair();
    // 既存 Established subscription を作る (SUBSCRIBE 経路)
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(sub_rid, 7, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

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

    // FORWARD=0 に更新する
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // 既存 subscription の forward_state は変わらない
    assert_eq!(
        server
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1,
        "既存 subscription の forward_state は変わらないこと"
    );
    assert_eq!(
        server
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        0,
        "track subscription の forward_state は更新されること"
    );
}

/// SUBSCRIBE_TRACKS の REQUEST_UPDATE + FORWARD=2 (値域外) はセッションを閉じる
///
/// draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter):
/// 値域外受信時は MUST close the session with PROTOCOL_VIOLATION。
#[test]
fn subscribe_tracks_request_update_forward_out_of_range_closes_session() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    use shiguredo_moqt::session::types::SessionState;
    let (mut client, mut server, rid) = establish_subscribe_tracks_example();

    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(2),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    let err = server
        .recv_stream_message(rid, upd_msg)
        .expect_err("値域外 FORWARD は PROTOCOL_VIOLATION になる");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
    // forward_state は初期値 1 のまま維持される
    assert_eq!(
        server
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1,
        "値域外受信でも forward_state は変更されないこと"
    );
    // RequestUpdateReceived は発火せず、CloseSession(PROTOCOL_VIOLATION) が発行される
    let mut saw_update = false;
    loop {
        match server.poll_event() {
            Some(SessionEvent::RequestUpdateReceived { .. }) => {
                saw_update = true;
            }
            Some(SessionEvent::CloseSession(e)) => {
                assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION);
                break;
            }
            Some(_) => {}
            None => panic!("CloseSession(PROTOCOL_VIOLATION) が期待されたが発行されなかった"),
        }
    }
    assert!(
        !saw_update,
        "値域外受信で RequestUpdateReceived は発火しないこと"
    );
}

/// SUBSCRIBE_TRACKS の REQUEST_UPDATE で FORWARD を省略すると値は維持される
///
/// draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter):
/// "If the parameter is omitted from REQUEST_UPDATE ..., the value ... remains unchanged."
#[test]
fn subscribe_tracks_request_update_forward_omitted_keeps_value() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, mut server, rid) = establish_subscribe_tracks_example();

    // FORWARD=0 に更新する
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // FORWARD 省略の空 REQUEST_UPDATE では値は維持される
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        0,
        "FORWARD 省略時は値が維持されること"
    );
}

/// SUBSCRIBE_TRACKS の REQUEST_OK に含まれる EXPIRES が
/// RequestOkReceived の parameters に伝播すること
///
/// draft-ietf-moq-transport-21 §9.3 (REQUEST_OK): NAMESPACE_OK_ALLOWED_PARAMS は
/// EXPIRES のみを許可する。受信側がパラメータを捨てると application が期限を扱えない。
#[test]
fn subscribe_tracks_request_ok_propagates_expires_parameter() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_tracks(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, st_msg) = take_send_request(&mut client);
    server
        .recv_request(st_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(9101),
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
            assert_eq!(request_kind, RequestKind::SubscribeTracks);
            assert_eq!(
                parameters.expires(),
                Some(9101),
                "REQUEST_OK の EXPIRES が RequestOkReceived に伝播すること"
            );
            got = true;
            break;
        }
    }
    assert!(
        got,
        "SUBSCRIBE_TRACKS の REQUEST_OK 受信イベントが発火すること"
    );
}

/// SUBSCRIBE_TRACKS の bidi stream 終端で暗黙終端した PUBLISH subscription に
/// RequestTerminated が 1 回だけ発行され、後続の PUBLISH stream 終端が no-op で
/// 吸収されること
///
/// draft-ietf-moq-transport-21 §6.4.2.2 (Graceful Request Stream Closure):
/// FIN はその方向に送るメッセージが終わったことだけを示し、request の cancel ではない。
/// SUBSCRIBE_TRACKS の終端で PUBLISH 由来 subscription の request_streams entry を
/// 削除したあと、後続の PUBLISH stream 終端が unknown id として PROTOCOL_VIOLATION に
/// ならないよう、終端時に rejected_request_ids へ登録する。派生 subscription の
/// RequestTerminated.reason は SUBSCRIBE_TRACKS の終端種別 (FIN / RESET) に追従する。
#[test]
fn subscribe_tracks_stream_end_then_publish_stream_close_does_not_close_session() {
    use shiguredo_moqt::message::Publish;
    use shiguredo_moqt::session::types::{RequestStreamEnd, SessionState};
    let track_end_cases = [
        (RequestStreamEnd::Fin, TerminationReason::PeerStreamFin),
        (
            RequestStreamEnd::Reset {
                error_code: 0,
                reliable_size: None,
            },
            TerminationReason::PeerStreamReset { error_code: 0 },
        ),
    ];
    for (idx, (track_end, expected_reason)) in track_end_cases.into_iter().enumerate() {
        let (mut client, _server, rid) = establish_subscribe_tracks_example();

        // server 側から PUBLISH を受信して alias を active_track_aliases に登録する
        // (server 起点なので request_id は奇数)
        let alias = 42;
        let publish_rid = 1;
        client
            .recv_request(ControlMessage::Publish(Publish {
                request_id: publish_rid,
                track_namespace: ns(&[b"example", b"live"]),
                track_name: b"cam".to_vec(),
                track_alias: alias,
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }))
            .expect("テストフィクスチャの前提条件を満たす");
        assert!(
            client
                .track_subscription(rid)
                .expect("track_subscription が存在すること")
                .active_track_aliases
                .contains(&alias),
            "PUBLISH 受信で active_track_aliases に alias が登録されること"
        );

        // SUBSCRIBE_TRACKS の bidi stream を peer が track_end で終端する
        client
            .recv_request_stream_closed(rid, track_end)
            .expect("SUBSCRIBE_TRACKS stream 終端は受理されること");

        // 後から PUBLISH の bidi stream を peer が FIN で終端する。
        // rejected_request_ids に登録済みのため no-op で吸収される
        client
            .recv_request_stream_closed(publish_rid, RequestStreamEnd::Fin)
            .expect("後続の PUBLISH stream 終端は no-op で吸収されること");

        assert_eq!(
            client.state(),
            SessionState::Established,
            "case {idx}: セッションが閉じないこと"
        );
        // PUBLISH subscription の RequestTerminated は SUBSCRIBE_TRACKS 終端で 1 回だけ発行され、
        // 後続の PUBLISH stream 終端では再発行されない。reason は SUBSCRIBE_TRACKS の終端種別に追従する
        let mut terminated = 0;
        while let Some(ev) = client.poll_event() {
            match ev {
                SessionEvent::CloseSession(_) => panic!("CloseSession は発行されてはならない"),
                SessionEvent::RequestTerminated {
                    request_id,
                    kind,
                    reason,
                } if request_id == publish_rid => {
                    assert_eq!(kind, RequestKind::Publish);
                    assert_eq!(reason, expected_reason.clone());
                    terminated += 1;
                }
                _ => {}
            }
        }
        assert_eq!(
            terminated, 1,
            "case {idx}: PUBLISH subscription の RequestTerminated は 1 回だけ発行されること"
        );
    }
}

/// SUBSCRIBE_TRACKS の終端で発行する RequestTerminated の kind が
/// request_streams に記録された種別と一致すること
///
/// 共有 alias では SUBSCRIBE_OK 由来 subscription が peer_publisher_aliases に混在しうる。
/// `RequestKind::Publish` 固定では Subscribe 由来 subscription の kind を誤る。
/// 本テストは、共有 alias の SUBSCRIBE_OK 由来 subscription まで SUBSCRIBE_TRACKS 終端で
/// 終端する現状の実装挙動を固定する（過剰終端は `peer_publisher_aliases` が確立経路を
/// 区別しない設計に起因する）。
#[test]
fn subscribe_tracks_stream_end_reports_registered_kind_for_shared_alias() {
    use shiguredo_moqt::message::Publish;
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let (mut client, mut server, track_rid) = establish_subscribe_tracks_example();

    // client が SUBSCRIBE を送り、alias=42 の SUBSCRIBE_OK を受ける (Subscribe 由来)
    let subscribe_rid = client
        .send_subscribe(
            ns(&[b"example", b"live"]),
            b"cam".to_vec(),
            MessageParameters::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            subscribe_rid,
            42,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(subscribe_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // server が同じ alias=42 で PUBLISH を送る (Publish 由来)
    let publish_rid = 1;
    client
        .recv_request(ControlMessage::Publish(Publish {
            request_id: publish_rid,
            track_namespace: ns(&[b"example", b"live"]),
            track_name: b"cam".to_vec(),
            track_alias: 42,
            parameters: MessageParameters::new(),
            track_properties: TrackProperties::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");

    // SUBSCRIBE_TRACKS stream 終端で両 subscription が終端される。
    // kind は request_streams の登録値 (Subscribe / Publish) が使われる
    client
        .recv_request_stream_closed(track_rid, RequestStreamEnd::Fin)
        .expect("SUBSCRIBE_TRACKS stream 終端は受理されること");

    let mut subscribe_kind = None;
    let mut publish_kind = None;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id, kind, ..
        } = ev
        {
            if request_id == subscribe_rid {
                subscribe_kind = Some(kind);
            } else if request_id == publish_rid {
                publish_kind = Some(kind);
            }
        }
    }
    assert_eq!(
        subscribe_kind,
        Some(RequestKind::Subscribe),
        "Subscribe 由来 subscription の kind は Subscribe であること"
    );
    assert_eq!(
        publish_kind,
        Some(RequestKind::Publish),
        "Publish 由来 subscription の kind は Publish であること"
    );
}

/// PUBLISH の bidi stream を先に終端し、その後に SUBSCRIBE_TRACKS stream を終端しても
/// PUBLISH subscription の RequestTerminated が二重発行されず、close 済み id が
/// rejected_request_ids に登録されないこと
///
/// `close_subscription_on_stream_end` が close 受信時に request_streams を除去するため、
/// 後続の SUBSCRIBE_TRACKS 終端では同じ request id を再終端しない。
#[test]
fn publish_stream_close_then_subscribe_tracks_stream_end_terminates_once() {
    use shiguredo_moqt::message::Publish;
    use shiguredo_moqt::session::types::{RequestStreamEnd, SessionState};
    let (mut client, _server, rid) = establish_subscribe_tracks_example();

    let alias = 42;
    let publish_rid = 1;
    client
        .recv_request(ControlMessage::Publish(Publish {
            request_id: publish_rid,
            track_namespace: ns(&[b"example", b"live"]),
            track_name: b"cam".to_vec(),
            track_alias: alias,
            parameters: MessageParameters::new(),
            track_properties: TrackProperties::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");

    // 先に PUBLISH の bidi stream を終端する
    client
        .recv_request_stream_closed(publish_rid, RequestStreamEnd::Fin)
        .expect("PUBLISH stream 終端は受理されること");

    // 後から SUBSCRIBE_TRACKS の bidi stream を終端する。
    // PUBLISH subscription は既に close 済みのため再終端されない
    client
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("SUBSCRIBE_TRACKS stream 終端は受理されること");

    assert_eq!(
        client.state(),
        SessionState::Established,
        "セッションが閉じないこと"
    );
    let mut terminated = 0;
    while let Some(ev) = client.poll_event() {
        match ev {
            SessionEvent::CloseSession(_) => panic!("CloseSession は発行されてはならない"),
            SessionEvent::RequestTerminated { request_id, .. } if request_id == publish_rid => {
                terminated += 1;
            }
            _ => {}
        }
    }
    assert_eq!(
        terminated, 1,
        "PUBLISH subscription の RequestTerminated は 1 回だけ発行されること"
    );

    // close 済み id は rejected_request_ids に登録されない。再度 PUBLISH stream 終端通知が
    // 来ると unknown id として PROTOCOL_VIOLATION になる
    let err = client
        .recv_request_stream_closed(publish_rid, RequestStreamEnd::Fin)
        .expect_err("close 済み id は rejected_request_ids に残らず unknown id になること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

// ─── 送信側 REQUEST_UPDATE の TRACK_NAMESPACE_PREFIX 反映 (draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions)) ─────

/// 送信側は REQUEST_UPDATE の送信直後には prefix を変えず、REQUEST_OK 受信後に
/// 新しい prefix を適用する
///
/// draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions): "If the update is accepted,
/// NAMESPACE and NAMESPACE_DONE messages following the REQUEST_OK will contain Track
/// Namespace suffixes relative to the updated prefix."
#[test]
fn subscribe_tracks_update_prefix_applies_after_request_ok() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let old_prefix = ns(&[b"example"]);
    let new_prefix = ns(&[b"newprefix"]);
    let (mut client, mut server, rid) =
        establish_subscribe_tracks_with(old_prefix.clone(), MessageParameters::new());

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
            .track_subscription(rid)
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
            .track_subscription(rid)
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
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        new_prefix,
        "REQUEST_OK 受信後に新 prefix が適用されること"
    );
}

/// REQUEST_OK 待ちの間に届いた新 prefix 相対の PUBLISH も active_track_aliases に登録される
///
/// SUBSCRIBE_TRACKS の PUBLISH は REQUEST_UPDATE とは別 bidi stream で順序保証がない
/// ため、確定待ちの prefix でもマッチさせる (draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions))。
#[test]
fn subscribe_tracks_update_prefix_pending_publish_registers_alias() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let old_prefix = ns(&[b"example"]);
    let new_prefix = ns(&[b"new"]);
    let (mut client, mut server, rid) =
        establish_subscribe_tracks_with(old_prefix.clone(), MessageParameters::new());

    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(new_prefix.clone()),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    // responder が更新を処理して新 prefix に切り替える (REQUEST_OK はまだ送らない)
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // REQUEST_OK より前に新 prefix 相対の PUBLISH が届く
    let alias = 21;
    let pub_rid = server
        .send_publish(
            ns(&[b"new", b"live"]),
            b"cam".to_vec(),
            alias,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut server);
    client
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.subscription(pub_rid).is_some(),
        "PUBLISH の subscription が作成されること"
    );
    assert!(
        client
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .active_track_aliases
            .contains(&alias),
        "確定待ち prefix にマッチする PUBLISH が active_track_aliases に登録されること"
    );
    assert_eq!(
        client
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        old_prefix,
        "REQUEST_OK までは旧 prefix のままであること"
    );

    // REQUEST_OK 受信で新 prefix を適用する
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        new_prefix
    );
}

/// bidi stream 終端で確定待ちを破棄し、prefix は旧のままにする
#[test]
fn subscribe_tracks_update_prefix_bidi_end_keeps_old_prefix() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    use shiguredo_moqt::session::types::{RequestStreamEnd, TrackSubscriptionState};
    let old_prefix = ns(&[b"example"]);
    let (mut client, _server, rid) =
        establish_subscribe_tracks_with(old_prefix.clone(), MessageParameters::new());

    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b"new"])),
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
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        TrackSubscriptionState::Terminated
    );
    assert_eq!(
        client
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        old_prefix,
        "bidi 終端時は prefix を更新しないこと"
    );
}

/// prefix 変更を含まない REQUEST_UPDATE と prefix 更新を連続送信しても、
/// それぞれに対応する REQUEST_OK まで prefix を適用しない
#[test]
fn subscribe_tracks_update_prefix_waits_for_corresponding_request_ok() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let old_prefix = ns(&[b"example"]);
    let new_prefix = ns(&[b"newprefix"]);
    let (mut client, mut server, rid) =
        establish_subscribe_tracks_with(old_prefix.clone(), MessageParameters::new());

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
            .track_subscription(rid)
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
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        new_prefix
    );
}

/// 確定待ちの間に「適用済みと同値」の prefix へ戻す更新を送っても、
/// 送信順の REQUEST_OK ごとに p0 → p1 → p0 と適用される
#[test]
fn subscribe_tracks_update_prefix_reverts_before_first_request_ok() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let old_prefix = ns(&[b"example"]);
    let mid_prefix = ns(&[b"newprefix"]);
    let (mut client, mut server, rid) =
        establish_subscribe_tracks_with(old_prefix.clone(), MessageParameters::new());

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
            .track_subscription(rid)
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
                .track_subscription(rid)
                .expect("テストフィクスチャの前提条件を満たす")
                .prefix,
            expected,
            "{label} REQUEST_OK の適用結果"
        );
    }
}

/// prefix 更新先が他購読と overlap する場合は送信前にローカルで拒否する
#[test]
fn subscribe_tracks_update_prefix_overlap_rejected_locally() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let old_prefix = ns(&[b"a", b"b"]);
    let (mut client, _server, rid1, _rid2) =
        establish_two_track_subscriptions(old_prefix.clone(), ns(&[b"a", b"c"]));

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
            .track_subscription(rid1)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        old_prefix,
        "拒否時に prefix を更新しないこと"
    );
}

/// REQUEST_ERROR を受信した場合は確定待ちを反映せず、prefix は旧のままにする
#[test]
fn subscribe_tracks_update_prefix_request_error_keeps_old_prefix() {
    use shiguredo_moqt::message::ReasonPhrase;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    use shiguredo_moqt::session::types::TrackSubscriptionState;
    let old_prefix = ns(&[b"example"]);
    let (mut client, mut server, rid) =
        establish_subscribe_tracks_with(old_prefix.clone(), MessageParameters::new());

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
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        TrackSubscriptionState::Terminated
    );
    assert_eq!(
        client
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        old_prefix,
        "REQUEST_ERROR 受信時は prefix を更新しないこと (確定待ちキューは内部状態)"
    );
}

/// 対応する REQUEST_UPDATE が無い REQUEST_OK はプロトコル違反でセッションを閉じる
#[test]
fn subscribe_tracks_request_ok_without_update_closes_session() {
    let (mut client, mut server, rid) = establish_subscribe_tracks_example();

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

/// 確定待ち prefix と overlap する新規 SUBSCRIBE_TRACKS はローカルで拒否する
#[test]
fn subscribe_tracks_create_overlapping_pending_update_rejected_locally() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let (mut client, _server, rid1) =
        establish_subscribe_tracks_with(ns(&[b"a", b"b"]), MessageParameters::new());

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
        .send_subscribe_tracks(ns(&[b"a", b"c"]), MessageParameters::new())
        .expect_err("確定待ち prefix と overlap する新規購読は拒否される");
    assert_eq!(
        err.as_session_error().map(|e| e.code),
        Some(SESSION_PROTOCOL_VIOLATION)
    );
    // 確定待ちでない ["x"] の新規購読は受理される
    client
        .send_subscribe_tracks(ns(&[b"x"]), MessageParameters::new())
        .expect("確定待ちと overlap しない新規購読は受理される");
}

/// 他購読の確定待ち prefix と overlap する更新も送信前にローカルで拒否する
#[test]
fn subscribe_tracks_update_prefix_overlapping_pending_update_rejected_locally() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let (mut client, _server, rid1, rid2) =
        establish_two_track_subscriptions(ns(&[b"a", b"b"]), ns(&[b"c", b"d"]));

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
            .track_subscription(rid2)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        ns(&[b"c", b"d"]),
        "拒否時に prefix を更新しないこと"
    );
}

/// ローカル拒否された更新は確定待ちに残らず、後続の更新だけが REQUEST_OK で適用される
#[test]
fn subscribe_tracks_locally_rejected_update_is_not_queued() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let (mut client, mut server, rid1, _rid2) =
        establish_two_track_subscriptions(ns(&[b"a", b"b"]), ns(&[b"a", b"c"]));

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
            .track_subscription(rid1)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        ns(&[b"x"]),
        "拒否された更新が確定待ちに残らないこと"
    );
}

/// 確定待ちの間に旧 prefix 相対の PUBLISH が届いても active_track_aliases に登録される
#[test]
fn subscribe_tracks_update_prefix_pending_publish_with_old_prefix_registers_alias() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let old_prefix = ns(&[b"example"]);
    let new_prefix = ns(&[b"new"]);
    let (mut client, mut server, rid) =
        establish_subscribe_tracks_with(old_prefix.clone(), MessageParameters::new());

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

    // REQUEST_OK より前に旧 prefix 相対の PUBLISH が届く (peer が更新を処理する前の送信)
    let alias = 22;
    server
        .send_publish(
            ns(&[b"example", b"live"]),
            b"cam".to_vec(),
            alias,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut server);
    client
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .active_track_aliases
            .contains(&alias),
        "旧 prefix にマッチする PUBLISH が active_track_aliases に登録されること"
    );
    assert_eq!(
        client
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        old_prefix,
        "REQUEST_OK までは旧 prefix のままであること"
    );
}

/// Terminated 購読は overlap 検査の対象外である (作成時・更新時の両方)
#[test]
fn subscribe_tracks_terminated_subscription_does_not_block_overlap() {
    use shiguredo_moqt::message::ReasonPhrase;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    use shiguredo_moqt::session::types::TrackSubscriptionState;
    let (mut client, mut server, rid1) =
        establish_subscribe_tracks_with(ns(&[b"a", b"b"]), MessageParameters::new());

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
            .track_subscription(rid1)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        TrackSubscriptionState::Terminated
    );

    // Terminated の ["a", "b"] と overlap する新規購読は受理される
    let rid2 = client
        .send_subscribe_tracks(ns(&[b"a", b"c"]), MessageParameters::new())
        .expect("Terminated 購読は overlap 検査の対象外");
    let (_, st_msg) = take_send_request(&mut client);
    server
        .recv_request(st_msg)
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
            .track_subscription(rid2)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        ns(&[b"a"]),
        "Terminated 購読を無視して新 prefix が適用されること"
    );
}

// ─── REQUEST_UPDATE の予約名前空間の再検証とローカル拒否 (§2.4.2 (Reserved Namespaces) / §6.5 (Session-Level Tracks and Namespaces)) ─────

/// 確立後の REQUEST_UPDATE で single period `.` 予約名前空間へ prefix を変更できない
///
/// 初回の SUBSCRIBE_TRACKS と同じ条件を更新経路でも検証し、DOES_NOT_EXIST の
/// REQUEST_ERROR を FIN 付きで返す。prefix と subscription state は変わらず、
/// セッションも閉じない。
#[test]
fn subscribe_tracks_update_prefix_single_period_rejected() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    use shiguredo_moqt::session::types::TrackSubscriptionState;
    let old_prefix = ns(&[b"example"]);
    let (_client, mut server, rid) =
        establish_subscribe_tracks_with(old_prefix.clone(), MessageParameters::new());

    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b"."])),
    });
    // 送信 API は予約名前空間をローカル拒否するため、受信側の検証は wire を経由せず
    // メッセージを直接組み立てて注入する
    server
        .recv_stream_message(
            rid,
            ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
                request_id: rid,
                parameters: params,
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions): 失敗した REQUEST_UPDATE への
    // 応答は FIN 付きで送る
    let (_, err_msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert!(fin, "拒否応答には FIN が付くこと");
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_DOES_NOT_EXIST);
            assert_eq!(e.reason.as_str(), "reserved single-period namespace");
        }
        _ => panic!("DOES_NOT_EXIST の RequestError が期待される"),
    }
    assert_eq!(
        server
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        old_prefix,
        "拒否時に prefix を更新しないこと"
    );
    assert_eq!(
        server
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        TrackSubscriptionState::Established,
        "拒否時に subscription state を変えないこと"
    );
    assert_eq!(
        server.state(),
        SessionState::Established,
        "拒否してもセッションを閉じないこと"
    );
    while let Some(ev) = server.poll_event() {
        assert!(
            !matches!(ev, SessionEvent::RequestUpdateReceived { .. }),
            "拒否した REQUEST_UPDATE を Application へ渡さないこと"
        );
    }
}

/// 確立後の REQUEST_UPDATE で `.session` 予約名前空間へ prefix を変更できない
///
/// 初回の SUBSCRIBE_TRACKS と同じ条件を更新経路でも検証し、DOES_NOT_EXIST の
/// REQUEST_ERROR を FIN 付きで返す。prefix と subscription state は変わらず、
/// セッションも閉じない。
#[test]
fn subscribe_tracks_update_prefix_session_level_rejected() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    use shiguredo_moqt::session::types::TrackSubscriptionState;
    let old_prefix = ns(&[b"example"]);
    let (_client, mut server, rid) =
        establish_subscribe_tracks_with(old_prefix.clone(), MessageParameters::new());

    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b".session"])),
    });
    // 送信 API は予約名前空間をローカル拒否するため、受信側の検証は wire を経由せず
    // メッセージを直接組み立てて注入する
    server
        .recv_stream_message(
            rid,
            ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
                request_id: rid,
                parameters: params,
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions): 失敗した REQUEST_UPDATE への
    // 応答は FIN 付きで送る
    let (_, err_msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert!(fin, "拒否応答には FIN が付くこと");
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_DOES_NOT_EXIST);
            assert_eq!(e.reason.as_str(), "session-level track does not exist");
        }
        _ => panic!("DOES_NOT_EXIST の RequestError が期待される"),
    }
    assert_eq!(
        server
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        old_prefix,
        "拒否時に prefix を更新しないこと"
    );
    assert_eq!(
        server
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        TrackSubscriptionState::Established,
        "拒否時に subscription state を変えないこと"
    );
    assert_eq!(
        server.state(),
        SessionState::Established,
        "拒否してもセッションを閉じないこと"
    );
    while let Some(ev) = server.poll_event() {
        assert!(
            !matches!(ev, SessionEvent::RequestUpdateReceived { .. }),
            "拒否した REQUEST_UPDATE を Application へ渡さないこと"
        );
    }
}

/// 予約名前空間への prefix 更新は FORWARD の値域違反 (MUST close) より優先して拒否される
///
/// 同一 REQUEST_UPDATE が「予約名前空間の拒否 (REQUEST_ERROR)」と「FORWARD 値域外
/// (MUST close)」の両方に該当する場合、予約名前空間の検証を先に行い、セッションを
/// 閉じずに DOES_NOT_EXIST の REQUEST_ERROR を返す。
#[test]
fn subscribe_tracks_update_reserved_prefix_precedes_forward_validation() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let old_prefix = ns(&[b"example"]);
    let (_client, mut server, rid) =
        establish_subscribe_tracks_with(old_prefix.clone(), MessageParameters::new());

    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b"."])),
    });
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(2),
    });
    // 送信 API は予約名前空間をローカル拒否するため、受信側の検証は wire を経由せず
    // メッセージを直接組み立てて注入する
    server
        .recv_stream_message(
            rid,
            ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
                request_id: rid,
                parameters: params,
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");

    let (_, err_msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert!(fin, "拒否応答には FIN が付くこと");
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_DOES_NOT_EXIST);
            assert_eq!(e.reason.as_str(), "reserved single-period namespace");
        }
        _ => panic!("予約名前空間の拒否が FORWARD の値域検証より優先されること"),
    }
    assert_eq!(
        server.state(),
        SessionState::Established,
        "予約名前空間の拒否ではセッションを閉じないこと"
    );
    assert_eq!(
        server
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        old_prefix,
        "拒否時に prefix を更新しないこと"
    );
}

/// 送信側は `.` / `.session` への prefix 更新をローカルで拒否する
///
/// 受信側の初回検証と同じ条件 (`.` / `.session`) を更新経路でも検証し、SESSION_PROTOCOL_VIOLATION で
/// 送信せずに拒否し、理由文字列も固定する。REQUEST_UPDATE は送信されず prefix も変わらない。
#[test]
fn subscribe_tracks_update_prefix_reserved_rejected_locally() {
    use shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_TRACK_NAMESPACE_PREFIX,
    };
    let old_prefix = ns(&[b"example"]);
    let (mut client, _server, rid) =
        establish_subscribe_tracks_with(old_prefix.clone(), MessageParameters::new());

    // 判定は「最初のフィールド」基準なので、後続フィールドの有無で結果が変わらないことも見る
    for (reserved, reason) in [
        (
            ns(&[b"."]),
            "application cannot use single-period reserved namespace",
        ),
        (
            ns(&[b".", b"x"]),
            "application cannot use single-period reserved namespace",
        ),
        (
            ns(&[b".session"]),
            "application cannot use .session reserved namespace",
        ),
        (
            ns(&[b".session", b"ext"]),
            "application cannot use .session reserved namespace",
        ),
    ] {
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_TRACK_NAMESPACE_PREFIX,
            value: MessageParameterValue::TrackNamespacePrefix(reserved),
        });
        let err = client
            .send_request_update(rid, params)
            .expect_err("予約名前空間への prefix 更新はローカルで拒否される");
        assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
        assert_eq!(err.reason, reason);
    }
    while let Some(ev) = client.poll_event() {
        assert!(
            !matches!(ev, SessionEvent::SendOnStream { .. }),
            "拒否された REQUEST_UPDATE は送信されないこと"
        );
    }
    assert_eq!(
        client
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .prefix,
        old_prefix,
        "拒否時に prefix を更新しないこと"
    );

    // 拒否は確定待ちキューを汚染しないため、その後の有効な prefix 更新は送信できる
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_TRACK_NAMESPACE_PREFIX,
        value: MessageParameterValue::TrackNamespacePrefix(ns(&[b"ok"])),
    });
    client
        .send_request_update(rid, ok_params)
        .expect("拒否後も有効な prefix 更新は送信できること");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    assert!(
        matches!(upd_msg, ControlMessage::RequestUpdate(_)),
        "有効な prefix 更新が REQUEST_UPDATE として送信されること"
    );
}

// ─── 空 prefix の購読で予約名前空間を広告できない (§6.5 (Session-Level Tracks and Namespaces) / §2.4.2 (Reserved Namespaces)) ─────

/// 空 prefix の SUBSCRIBE_TRACKS では suffix の先頭が `.session` / `.` の PUBLISH_SKIPPED を送れない
#[test]
fn send_publish_skipped_reserved_suffix_with_empty_prefix_rejected() {
    use shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION;
    let (_, mut server, rid) = establish_subscribe_tracks_with(ns(&[]), MessageParameters::new());

    for reserved in [
        ns(&[b".session"]),
        ns(&[b".session", b"ext"]),
        ns(&[b"."]),
        ns(&[b".", b"x"]),
    ] {
        let err = server
            .send_publish_skipped(rid, reserved, b"track".to_vec())
            .expect_err("空 prefix では予約名前空間を広告できない");
        assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
        assert_eq!(
            err.reason,
            "application cannot advertise reserved namespace with empty subscription prefix"
        );
    }
    while let Some(ev) = server.poll_event() {
        assert!(
            !matches!(ev, SessionEvent::SendOnStream { .. }),
            "拒否された PUBLISH_SKIPPED は送信されないこと"
        );
    }
    // 拒否した PUBLISH_SKIPPED を送信済みとして記録すると、後続の正当な PUBLISH を
    // ローカルで拒否してしまう (draft-ietf-moq-transport-21 §4.1 (Subscribing to Namespaces) の
    // "The Publisher MUST NOT send a PUBLISH for a Track for a given SUBSCRIBE_TRACKS after
    // PUBLISH_SKIPPED has been sent" を、送っていない PUBLISH_SKIPPED に適用する形になる)
    assert!(
        server
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .skipped_tracks
            .is_empty(),
        "拒否された PUBLISH_SKIPPED は skipped_tracks に登録されないこと"
    );
    // 記録されていないので、同じ Track への PUBLISH はローカル拒否されない
    server
        .send_publish(
            ns(&[b"live"]),
            b"track".to_vec(),
            7,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("拒否された PUBLISH_SKIPPED は正当な PUBLISH を妨げないこと");
    let _ = take_send_request(&mut server);

    server
        .send_publish_skipped(rid, ns(&[b"live"]), b"track".to_vec())
        .expect("予約名前空間でない suffix は送信できること");
    let (_, msg) = take_send_on_stream(&mut server);
    assert!(matches!(msg, ControlMessage::PublishSkipped(_)));
    assert_eq!(
        server
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .skipped_tracks
            .len(),
        1,
        "送信した PUBLISH_SKIPPED だけが記録されること"
    );
}

/// prefix が非空なら suffix の先頭が予約名前空間でも PUBLISH_SKIPPED を送信できる
#[test]
fn send_publish_skipped_reserved_suffix_with_non_empty_prefix_accepted() {
    let (_, mut server, rid) =
        establish_subscribe_tracks_with(ns(&[b"example"]), MessageParameters::new());

    server
        .send_publish_skipped(rid, ns(&[b".session"]), b"track".to_vec())
        .expect("prefix が非空なら full namespace は予約名前空間にならない");
    let (_, msg) = take_send_on_stream(&mut server);
    assert!(matches!(msg, ControlMessage::PublishSkipped(_)));
}
