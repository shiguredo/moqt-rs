//! SUBSCRIBE_TRACKS メッセージに関するテスト

use super::*;

/// SUBSCRIBE_TRACKS を両側で Established にする (namespace=example, 空パラメータ)
///
/// 返り値は (subscriber, publisher, request_id)。
fn establish_subscribe_tracks_example() -> (Session, Session, u64) {
    establish_subscribe_tracks_with(ns(&[b"example"]), MessageParameters::new())
}

/// 任意の namespace・パラメータで SUBSCRIBE_TRACKS を確立する
///
/// 返り値は (subscriber, publisher, request_id)。
fn establish_subscribe_tracks_with(
    namespace: TrackNamespace,
    params: MessageParameters,
) -> (Session, Session, u64) {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_tracks(namespace, params)
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
    (client, server, rid)
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
/// "Messages marked \"First\" MUST be the first message on a new request stream."
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
