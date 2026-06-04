//! SUBSCRIBE / PUBLISH ハンドシェイクと確立のテスト

use super::*;

/// Client が subscriber、Server が publisher で SUBSCRIBE ハンドシェイクが完了する
#[test]
fn subscribe_handshake_client_to_server() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (ev_rid, sub_msg) = take_send_request(&mut client);
    assert_eq!(ev_rid, rid);
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

    let c_sub = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    let s_sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(c_sub.state, SubscriptionState::Established);
    assert_eq!(s_sub.state, SubscriptionState::Established);
    assert_eq!(c_sub.track_alias, Some(500));
    assert_eq!(s_sub.track_alias, Some(500));
    assert_eq!(c_sub.my_role, TrackRole::Subscriber);
    assert_eq!(s_sub.my_role, TrackRole::Publisher);
}

/// PUBLISH_OK に FORWARD を含めると送信側で拒否される
///
/// draft-ietf-moq-transport-21 Appendix A.1 #1790 (Subscription parameters appear in
/// REQUEST_UPDATE, not PUBLISH_OK): subscription 更新用パラメータは REQUEST_UPDATE
/// (要求) に出現し、PUBLISH_OK は EXPIRES のみとなる。FORWARD の更新は
/// REQUEST_UPDATE 経路で行う。
#[test]
fn publish_ok_with_forward_is_rejected() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, mut server) = establish_pair();
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
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    let err = server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .expect_err("PUBLISH_OK の FORWARD はスコープ外で拒否される");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // 拒否後も subscription は Pending のままであり、状態は汚染されない
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_pending_publisher()
    );
}

/// PUBLISH_OK で FORWARD を省略しても forward_state は PUBLISH 時の値のまま維持される
///
/// draft-ietf-moq-transport-21 Appendix A.1 #1790: PUBLISH_OK は EXPIRES のみを運び、
/// forward_state の更新は REQUEST_UPDATE (要求) 経路に委譲する。
#[test]
fn publish_ok_without_forward_keeps_publish_forward_state() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, mut server) = establish_pair();
    // PUBLISH 送信時に FORWARD=0 を指定しておく
    let mut pub_params = MessageParameters::new();
    pub_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam1".to_vec(),
            111,
            pub_params,
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        0
    );
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // PUBLISH_OK (空パラメータ) では forward_state は PUBLISH 時の 0 のまま維持される
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
            .forward_state,
        0,
        "PUBLISH_OK では forward_state が上書きされないこと"
    );
}

/// REQUEST_OK (publish context) 受信で `RequestOkReceived` イベントが発行され、EXPIRES
/// parameter を application が取得できる (draft-ietf-moq-transport-21 §9.20.17 (EXPIRES Parameter))
#[test]
fn publish_ok_emits_publish_ok_received_event_with_parameters() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES,
    };
    let (mut client, mut server) = establish_pair();
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
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(5000),
    });
    server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut got = None;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestOkReceived {
            request_id,
            request_kind: _,
            parameters,
        } = ev
            && request_id == rid
        {
            got = Some(parameters);
        }
    }
    let params = got.expect("publish 要求では RequestOkReceived イベントが発行されること");
    assert_eq!(params.expires(), Some(5000));
}

/// PUBLISH_OK に LOCATION_FILTER を含めると送受信ともに拒否される
///
/// draft-ietf-moq-transport-21 Appendix A.1 #1790 (Subscription parameters appear in
/// REQUEST_UPDATE, not PUBLISH_OK): LOCATION_FILTER は REQUEST_UPDATE (要求) に出現し、
/// PUBLISH_OK には出現しない。送信側は状態を変更せずエラーを返し、受信側は
/// PROTOCOL_VIOLATION でセッションを閉じる。
#[test]
fn publish_ok_with_location_filter_rejected() {
    use shiguredo_moqt::message_parameter::{
        LocationFilter, MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
    };
    use shiguredo_moqt::{message::common::Location, session::types::SessionState};
    let filter = LocationFilter::AbsoluteStart {
        start: Location {
            group_id: 2,
            object_id: 0,
        },
    };
    let mut location_params = MessageParameters::new();
    location_params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(filter.encode_to_bytes()),
    });

    // 送信側: スコープ外で拒否され、状態は Pending のまま
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam1".to_vec(),
            112,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = server
        .send_request_ok(rid, location_params.clone(), TrackProperties::default())
        .expect_err("PUBLISH_OK の LOCATION_FILTER はスコープ外で拒否される");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_pending_publisher()
    );

    // 受信側: ワイヤメッセージ直接注入で PROTOCOL_VIOLATION により閉じる
    let ok_msg = ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
        parameters: location_params,
        track_properties: TrackProperties::default(),
    });
    let err = client
        .recv_stream_message(rid, ok_msg)
        .expect_err("LOCATION_FILTER を含む PUBLISH_OK 受信は PROTOCOL_VIOLATION になる");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}

/// parameters が空の REQUEST_OK (publish context) でも `RequestOkReceived` イベントは発行される
#[test]
fn publish_ok_empty_parameters_still_emits_event() {
    let (mut client, mut server) = establish_pair();
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
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut seen = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestOkReceived { request_id, .. } = ev
            && request_id == rid
        {
            seen = true;
        }
    }
    assert!(
        seen,
        "invariant violated: empty REQUEST_OK must still emit RequestOkReceived event"
    );
}

/// Server 側の REQUEST_ERROR で subscriber の subscription が Terminated になる
#[test]
fn request_error_terminates_pending_subscription() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
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
    let (_, err_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, err_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated
    );
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated
    );
}

/// draft-ietf-moq-transport-21 §3.1.1: 同一 Track への複数同時 subscription が許可されたため、
/// 2 件目の SUBSCRIBE も正常に受理される
#[test]
fn duplicate_subscribe_is_accepted_in_draft_20() {
    let (mut client, mut server) = establish_pair();
    client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, m1) = take_send_request(&mut client);
    server
        .recv_request(m1)
        .expect("テストフィクスチャの前提条件を満たす");
    // 2 件目も受理される (draft-20 で複数 subscription 許可)
    client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("同一 Track への複数同時 subscription が受理されること");
}

/// peer publisher が同一 alias を 2 つの Track に使えば DUPLICATE_TRACK_ALIAS でクローズ
#[test]
fn peer_publisher_duplicate_track_alias_closes_session() {
    let (mut client, mut server) = establish_pair();
    server
        .send_publish(
            ns(&[b"a"]),
            b"1".to_vec(),
            77,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, first) = take_send_request(&mut server);
    client
        .recv_request(first)
        .expect("テストフィクスチャの前提条件を満たす");
    // Server が同じ alias を別 Track で再利用しようとすると local 側で失敗
    let err = server
        .send_publish(
            ns(&[b"b"]),
            b"2".to_vec(),
            77,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_DUPLICATE_TRACK_ALIAS
    );
}

/// Established 外での send_subscribe は PROTOCOL_VIOLATION
#[test]
fn send_subscribe_before_established_errors() {
    let mut client = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let err = client
        .send_subscribe(ns(&[b"x"]), b"y".to_vec(), MessageParameters::new())
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    // セッションは閉じない (ローカル API バリデーション)
    assert_eq!(client.state(), SessionState::LocalSetupSent);
}

/// 未知の必須トラックプロパティを含む SUBSCRIBE_OK は購読をキャンセルする (draft-ietf-moq-transport-21 §3.6 (Mandatory Track Properties))
#[test]
fn subscribe_ok_with_unknown_mandatory_property_cancels_subscription() {
    use shiguredo_moqt::track_properties::{
        MANDATORY_TRACK_PROPERTY_MIN, TrackProperty, TrackPropertyValue,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: MANDATORY_TRACK_PROPERTY_MIN,
        value: TrackPropertyValue::VarInt(1),
    });
    server
        .send_subscribe_ok(rid, 500, MessageParameters::new(), tp)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let sub = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(sub.state, SubscriptionState::Terminated);

    let mut found = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind,
            reason,
        } = e
        {
            assert_eq!(request_id, rid);
            assert_eq!(kind, RequestKind::Subscribe);
            assert_eq!(reason, TerminationReason::LocalCancel);
            found = true;
        }
    }
    assert!(
        found,
        "expected RequestTerminated event for cancelled subscription"
    );
}

/// send_subscribe に FORWARD=2 を渡すと SESSION_PROTOCOL_VIOLATION が返る
/// (draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter): 許容値は 0/1 のみ)
#[test]
fn send_subscribe_rejects_invalid_forward_value() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, _server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(2),
    });
    let err = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), params)
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
}

/// send_publish に FORWARD=2 を渡すと SESSION_PROTOCOL_VIOLATION が返る
#[test]
fn send_publish_rejects_invalid_forward_value() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, _server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(2),
    });
    let err = client
        .send_publish(
            ns(&[b"live"]),
            b"cam1".to_vec(),
            100,
            params,
            TrackProperties::new(),
        )
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
}

/// single period `.` 予約名前空間の SUBSCRIBE は DOES_NOT_EXIST で拒否され、subscription は作成されない
/// (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces))
#[test]
fn subscribe_single_period_namespace_rejected() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message::Subscribe;
    let (_, mut server) = establish_pair();
    server
        .recv_request(ControlMessage::Subscribe(Subscribe {
            request_id: 0,
            track_namespace: ns(&[b"."]),
            track_name: b"cam".to_vec(),
            parameters: MessageParameters::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    // subscription は作成されない
    assert!(server.subscription(0).is_none());
    let (_, err_msg) = take_send_on_stream(&mut server);
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_DOES_NOT_EXIST);
        }
        _ => panic!("DOES_NOT_EXIST の RequestError が期待される"),
    }
}

/// REQUEST_ERROR で拒否した request のストリームクローズは no-op で吸収され、
/// セッションが fail しないこと
///
/// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection):
/// "When an endpoint rejects a request without performing any application processing,
/// it SHOULD send a REQUEST_ERROR and FIN the stream." 拒否された request は
/// `request_streams` に未登録のため、peer が自分の送信方向を閉じても
/// `recv_request_stream_closed` が unknown id として fail しないよう
/// `rejected_request_ids` で no-op 吸収する (GOAWAY 拒否以外の拒否パスの共通経路)。
#[test]
fn rejected_subscribe_stream_close_does_not_fail_session() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message::Subscribe;
    let (_, mut server) = establish_pair();
    // 2 つの request id で single-period 名前空間の SUBSCRIBE を拒否し、
    // FIN と RESET_STREAM の両方のクローズを検証する (id は Client 役の偶数採番。
    // 奇数は Server 役のローカル採番域で、peer が奇数 id を送ると parity 不正で fail するため)
    let closes: [(u64, RequestStreamEnd); 2] = [
        (0, RequestStreamEnd::Fin),
        (
            2,
            RequestStreamEnd::Reset {
                error_code: 0,
                reliable_size: None,
            },
        ),
    ];
    for (rid, end) in closes {
        server
            .recv_request(ControlMessage::Subscribe(Subscribe {
                request_id: rid,
                track_namespace: ns(&[b"."]),
                track_name: b"cam".to_vec(),
                parameters: MessageParameters::new(),
            }))
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, err_msg) = take_send_on_stream(&mut server);
        match err_msg {
            ControlMessage::RequestError(e) => {
                assert_eq!(e.error_code, REQUEST_DOES_NOT_EXIST);
            }
            _ => panic!("DOES_NOT_EXIST の RequestError が期待される"),
        }
        // peer が自分の送信方向を閉じる → no-op で吸収されセッションは生存
        server
            .recv_request_stream_closed(rid, end)
            .expect("拒否済み request id のストリームクローズは no-op であること");
        assert_eq!(
            server.state(),
            SessionState::Established,
            "拒否済み id のクローズでセッションは Established を維持"
        );
        // 拒否済み id はテーブルに登録されないため参照は None のまま
        assert!(server.subscription(rid).is_none());
    }
    // 拒否 id がテーブルに一切登録されないこと
    assert_no_tracked_requests(&server);
    // RequestTerminated イベントが 1 件も発行されないこと (no-op)
    let mut terminated = false;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::RequestTerminated { .. }) {
            terminated = true;
            break;
        }
    }
    assert!(
        !terminated,
        "拒否済み id のクローズで RequestTerminated は発行されないこと"
    );
    // 拒否済み id の 2 回目のストリームクローズは unknown id として fail する
    // (no-op 吸収は集合から削除することで成立するため)
    let err = server
        .recv_request_stream_closed(0, RequestStreamEnd::Fin)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// single period `.` 予約名前空間の PUBLISH は DOES_NOT_EXIST で拒否され、subscription は作成されない
/// (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces))
#[test]
fn publish_single_period_namespace_rejected() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message::Publish;
    let (_, mut server) = establish_pair();
    server
        .recv_request(ControlMessage::Publish(Publish {
            request_id: 0,
            track_namespace: ns(&[b"."]),
            track_name: b"cam".to_vec(),
            track_alias: 100,
            parameters: MessageParameters::new(),
            track_properties: TrackProperties::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    // subscription は作成されない
    assert!(server.subscription(0).is_none());
    let (_, err_msg) = take_send_on_stream(&mut server);
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_DOES_NOT_EXIST);
        }
        _ => panic!("DOES_NOT_EXIST の RequestError が期待される"),
    }
}

/// send_subscribe に single period `.` 予約名前空間を渡すと SESSION_PROTOCOL_VIOLATION が返る
/// (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces))
#[test]
fn send_subscribe_single_period_namespace_rejected() {
    let (mut client, _server) = establish_pair();
    let err = client
        .send_subscribe(ns(&[b"."]), b"cam".to_vec(), MessageParameters::new())
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
}

/// send_publish に single period `.` 予約名前空間を渡すと SESSION_PROTOCOL_VIOLATION が返る
/// (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces))
#[test]
fn send_publish_single_period_namespace_rejected() {
    let (mut client, _server) = establish_pair();
    let err = client
        .send_publish(
            ns(&[b"."]),
            b"cam".to_vec(),
            100,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
}

/// SUBSCRIBE 受信の FORWARD 値域外 (0 / 1 以外) で、`Err(RecvRequestError::Session(...))` と
/// `CloseSession(PROTOCOL_VIOLATION)` の両方が発生し、セッションが閉じること
///
/// draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter): "If an endpoint receives a
/// value outside this range, it MUST close the session with PROTOCOL_VIOLATION."
/// `?` 伝播のみだと `RecvRequestError::Session` の契約 (session state は Closing に遷移済み) が
/// 破れ、セッションが開いたまま残る。GROUP_ORDER の値域検証と同じパターンで
/// `self.fail()` に接続した。
#[test]
fn subscribe_with_invalid_forward_closes_session() {
    use shiguredo_moqt::message::Subscribe;
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
        .recv_request(ControlMessage::Subscribe(Subscribe {
            request_id: 0,
            track_namespace: ns(&[b"live"]),
            track_name: b"cam".to_vec(),
            parameters: params,
        }))
        .expect_err("値域外 FORWARD は PROTOCOL_VIOLATION になる");
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    // 検証失敗時に Subscription が登録されないこと (検証は insert より前)
    assert!(
        server.subscription(0).is_none(),
        "検証エラー時に Subscription が登録されないこと"
    );
    match drain_until_close(&mut server) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("CloseSession(PROTOCOL_VIOLATION) が期待されたが {other:?}"),
    }
}

/// PUBLISH 受信の FORWARD 値域外 (0 / 1 以外) で、`Err(RecvRequestError::Session(...))` と
/// `CloseSession(PROTOCOL_VIOLATION)` の両方が発生し、セッションが閉じること
///
/// draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter): "If an endpoint receives a
/// value outside this range, it MUST close the session with PROTOCOL_VIOLATION."
#[test]
fn publish_with_invalid_forward_closes_session() {
    use shiguredo_moqt::message::Publish;
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
        .recv_request(ControlMessage::Publish(Publish {
            request_id: 0,
            track_namespace: ns(&[b"live"]),
            track_name: b"cam".to_vec(),
            track_alias: 100,
            parameters: params,
            track_properties: TrackProperties::new(),
        }))
        .expect_err("値域外 FORWARD は PROTOCOL_VIOLATION になる");
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    // 検証失敗時に Subscription が登録されないこと (検証は insert より前)
    assert!(
        server.subscription(0).is_none(),
        "検証エラー時に Subscription が登録されないこと"
    );
    match drain_until_close(&mut server) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("CloseSession(PROTOCOL_VIOLATION) が期待されたが {other:?}"),
    }
}

/// FORWARD=0 / 1 を含む SUBSCRIBE の受信で `forward_state` が設定されること
///
/// 検証の `Ok` 値が `forward_state` の設定に使われる (検証と設定の分離)。
/// FORWARD 不在 (デフォルト 1) のケースも含める。
#[test]
fn subscribe_with_forward_sets_forward_state() {
    use shiguredo_moqt::message::Subscribe;
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
            .recv_request(ControlMessage::Subscribe(Subscribe {
                request_id: rid,
                track_namespace: ns(&[b"live"]),
                track_name: format!("cam{idx}").into_bytes(),
                parameters: params,
            }))
            .expect("正当な FORWARD の SUBSCRIBE は受理されること");
        // forward_state が設定されること
        assert_eq!(
            server
                .subscription(rid)
                .expect("subscription が存在する")
                .forward_state,
            *expected,
            "FORWARD={forward:?} で forward_state が {expected} になること"
        );
    }
}

/// FORWARD=0 / 1 を含む PUBLISH の受信で `forward_state` が設定されること
///
/// 検証の `Ok` 値が `forward_state` の設定に使われる (検証と設定の分離)。
/// FORWARD 不在 (デフォルト 1) のケースも含める。
#[test]
fn publish_with_forward_sets_forward_state() {
    use shiguredo_moqt::message::Publish;
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
            .recv_request(ControlMessage::Publish(Publish {
                request_id: rid,
                track_namespace: ns(&[b"live"]),
                track_name: format!("cam{idx}").into_bytes(),
                track_alias: 100 + idx as u64,
                parameters: params,
                track_properties: TrackProperties::new(),
            }))
            .expect("正当な FORWARD の PUBLISH は受理されること");
        // forward_state が設定されること
        assert_eq!(
            server
                .subscription(rid)
                .expect("subscription が存在する")
                .forward_state,
            *expected,
            "FORWARD={forward:?} で forward_state が {expected} になること"
        );
    }
}
