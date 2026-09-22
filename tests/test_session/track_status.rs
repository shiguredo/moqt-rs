//! TRACK_STATUS の送信側 (自側 subscriber) と受信側 (自側 publisher) のテスト群
//!
//! draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS): 受信側は SUBSCRIBE と同様に扱うが、
//! subscription state も Track Alias も作らず Objects も送らない。応答 (TRACK_STATUS_OK /
//! REQUEST_ERROR) の送信後に bidi stream を FIN で閉じる。
use super::*;
use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
use shiguredo_moqt::message::{ReasonPhrase, RequestError};
use shiguredo_moqt::message_parameter::{
    MessageParameter, MessageParameterValue, PARAM_INCLUDE_PROPERTIES,
};
use shiguredo_moqt::session::types::{
    RequestKind, RequestStreamEnd, TrackRole, TrackStatusResponse,
};

/// DEFAULT_PUBLISHER_PRIORITY を 1 件持つ空でない Track Properties を作る
fn properties_with_publisher_priority() -> TrackProperties {
    use shiguredo_moqt::track_properties::{
        PROP_DEFAULT_PUBLISHER_PRIORITY, TrackProperty, TrackPropertyValue,
    };
    let mut properties = TrackProperties::new();
    properties.push(TrackProperty {
        prop_type: PROP_DEFAULT_PUBLISHER_PRIORITY,
        value: TrackPropertyValue::VarInt(7),
    });
    properties
}

/// TRACK_STATUS を送信すると request が発行され、entry が応答待ちで登録される
#[test]
fn send_track_status_registers_pending_entry() {
    let (mut client, _server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");

    let entry = client
        .track_status_request(rid)
        .expect("送信した TRACK_STATUS の entry が存在すること");
    assert_eq!(entry.request_id, rid);
    assert_eq!(entry.track_namespace, ns(&[b"live"]));
    assert_eq!(entry.track_name, b"cam".to_vec());
    assert_eq!(
        entry.my_role,
        TrackRole::Subscriber,
        "自側が送った要求は requester (Subscriber) として登録されること"
    );
    assert!(entry.response.is_none(), "応答前は応答待ちであること");

    // SendRequest として wire に載る
    while let Some(e) = client.poll_event() {
        if let SessionEvent::SendRequest {
            request_id,
            message,
        } = e
        {
            assert_eq!(request_id, rid);
            assert!(
                matches!(message, ControlMessage::TrackStatus(_)),
                "TRACK_STATUS が送信されること"
            );
            return;
        }
    }
    panic!("SendRequest イベントが期待されたが発行されなかった");
}

/// REQUEST_OK を受信すると entry が応答済みになり、RequestOkReceived が発行される
///
/// draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS): 受信側は SUBSCRIBE と同様に扱い、
/// TRACK_STATUS_OK を返す。LARGEST_OBJECT は任意で載る。
#[test]
fn track_status_completes_on_request_ok() {
    let (mut client, _server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");

    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("REQUEST_OK の受信に成功すること");

    let entry = client
        .track_status_request(rid)
        .expect("entry が存在すること");
    assert_eq!(
        entry.response,
        Some(TrackStatusResponse::Ok {
            largest_location: None
        }),
        "REQUEST_OK で応答済みになること"
    );

    let mut saw_ok = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::RequestOkReceived { request_id, .. } = e {
            assert_eq!(request_id, rid);
            saw_ok = true;
        }
    }
    assert!(saw_ok, "RequestOkReceived が発行されること");
}

/// REQUEST_ERROR を受信すると entry が失敗応答になり、RequestErrorReceived が発行される
#[test]
fn track_status_completes_on_request_error() {
    let (mut client, _server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");

    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestError(RequestError {
                error_code: REQUEST_DOES_NOT_EXIST,
                retry_interval: 0,
                reason: ReasonPhrase::new("no such track").expect("正当な reason phrase である"),
                redirect: None,
            }),
        )
        .expect("REQUEST_ERROR の受信に成功すること");

    let entry = client
        .track_status_request(rid)
        .expect("entry が存在すること");
    assert_eq!(
        entry.response,
        Some(TrackStatusResponse::Error),
        "REQUEST_ERROR で失敗応答になること"
    );

    let mut saw_error = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::RequestErrorReceived { request_id, .. } = e {
            assert_eq!(request_id, rid);
            saw_error = true;
        }
    }
    assert!(saw_error, "RequestErrorReceived が発行されること");
}

/// 応答済みの TRACK_STATUS は forget でき、再 forget は None を返す (冪等)
#[test]
fn forget_track_status_is_idempotent_after_response() {
    let (mut client, _server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");

    // 応答前は forget できない (終端通知を先に受ける必要がある)
    assert!(
        client.forget_track_status(rid).is_none(),
        "応答前の forget は None を返すこと"
    );

    client
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("stream 終端の通知に成功すること");
    assert!(
        client.forget_track_status(rid).is_some(),
        "stream 終端後は forget できること"
    );
    assert!(
        client.forget_track_status(rid).is_none(),
        "除去後の再 forget は None を返すこと"
    );
}

/// peer から TRACK_STATUS を受信すると応答待ちの entry が登録され、セッションは閉じない
///
/// draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS): 受信側は SUBSCRIBE と同様に扱うが、
/// "it does not create downstream subscription state or send any Objects" である。
#[test]
fn recv_peer_track_status_registers_pending_entry() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, message) = take_send_request(&mut client);

    server
        .recv_request(message)
        .expect("TRACK_STATUS の受信に成功すること");
    assert_eq!(
        server.state(),
        SessionState::Established,
        "TRACK_STATUS の受信でセッションを閉じないこと"
    );
    let entry = server
        .track_status_request(rid)
        .expect("受信した TRACK_STATUS の entry が登録されること");
    assert_eq!(entry.request_id, rid);
    assert_eq!(
        entry.my_role,
        TrackRole::Publisher,
        "自側が受けた要求は responder (Publisher) として登録されること"
    );
    assert!(entry.response.is_none(), "応答前は応答待ちであること");
    // §9.13: subscription state を作らない
    assert!(
        server.subscription(rid).is_none(),
        "TRACK_STATUS は subscription state を作らないこと"
    );
    assert!(
        server.fetch(rid).is_none(),
        "TRACK_STATUS は fetch state を作らないこと"
    );
}

/// TRACK_STATUS_OK を送信すると entry が応答済みになり、bidi stream が FIN で閉じる
///
/// draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS): "The bidi stream is closed with a FIN
/// after TRACK_STATUS_OK or REQUEST_ERROR are sent."
#[test]
fn track_status_ok_send_closes_stream_with_fin() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("TRACK_STATUS の受信に成功すること");

    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("TRACK_STATUS_OK の送信に成功すること");
    let (send_rid, ok_msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert_eq!(send_rid, rid);
    assert!(
        matches!(ok_msg, ControlMessage::RequestOk(_)),
        "REQUEST_OK (TRACK_STATUS_OK) が送信されること"
    );
    assert!(
        fin,
        "TRACK_STATUS_OK の送信後に bidi stream を FIN で閉じること"
    );
    assert_eq!(
        server
            .track_status_request(rid)
            .expect("entry が存在すること")
            .response,
        Some(TrackStatusResponse::Ok {
            largest_location: None
        }),
        "応答済みとして記録されること"
    );

    // 受信側 (client) は応答を受け取り、応答済みになる
    client
        .recv_stream_message(rid, ok_msg)
        .expect("TRACK_STATUS_OK の受信に成功すること");
    assert_eq!(
        client
            .track_status_request(rid)
            .expect("entry が存在すること")
            .response,
        Some(TrackStatusResponse::Ok {
            largest_location: None
        })
    );
}

/// 受信した TRACK_STATUS は REQUEST_ERROR で拒否でき、bidi stream が FIN で閉じる
///
/// draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS): "A publisher responds to a failed
/// TRACK_STATUS with an appropriate REQUEST_ERROR message. The bidi stream is closed with a
/// FIN after TRACK_STATUS_OK or REQUEST_ERROR are sent."
#[test]
fn track_status_request_error_send_closes_stream_with_fin() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("TRACK_STATUS の受信に成功すること");

    server
        .send_request_error(
            rid,
            REQUEST_DOES_NOT_EXIST,
            0,
            ReasonPhrase::new("no such track").expect("正当な reason phrase である"),
            None,
        )
        .expect("受信した TRACK_STATUS を REQUEST_ERROR で拒否できること");
    let (send_rid, err_msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert_eq!(send_rid, rid);
    match err_msg {
        ControlMessage::RequestError(err) => {
            assert_eq!(err.error_code, REQUEST_DOES_NOT_EXIST);
        }
        other => panic!("REQUEST_ERROR が期待されたが {other:?} だった"),
    }
    assert!(
        fin,
        "REQUEST_ERROR の送信後に bidi stream を FIN で閉じること"
    );
    assert_eq!(
        server
            .track_status_request(rid)
            .expect("entry が存在すること")
            .response,
        Some(TrackStatusResponse::Error),
        "失敗応答として確定すること"
    );
}

/// 応答済みの TRACK_STATUS への再応答は拒否される
#[test]
fn track_status_double_response_is_rejected() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("TRACK_STATUS の受信に成功すること");

    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("1 回目の応答に成功すること");
    let _ = take_send_on_stream_with_fin(&mut server);
    let err = server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    let err = server
        .send_request_error(
            rid,
            REQUEST_DOES_NOT_EXIST,
            0,
            ReasonPhrase::new("no").expect("正当な reason phrase である"),
            None,
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        server.state(),
        SessionState::Established,
        "二重応答の拒否でセッションを閉じないこと"
    );
}

/// requester の RESET_STREAM (cancel) 後は responder も応答できず、entry を破棄できる
///
/// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection): "Once a request
/// stream has been opened, the request MAY be cancelled by either endpoint." cancel 後は
/// 状態を破棄してよい。
#[test]
fn responder_cannot_respond_after_requester_reset() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("TRACK_STATUS の受信に成功すること");

    server
        .recv_request_stream_closed(
            rid,
            RequestStreamEnd::Reset {
                error_code: shiguredo_moqt::error::STREAM_CANCELLED,
                reliable_size: None,
            },
        )
        .expect("requester の RESET_STREAM を受理すること");
    let err = server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .unwrap_err();
    assert_eq!(
        err.code, SESSION_PROTOCOL_VIOLATION,
        "cancel 後に応答を送ってはならないこと"
    );
    assert!(
        server.forget_track_status(rid).is_some(),
        "cancel 後は entry を破棄できること"
    );
    assert_eq!(
        server.state(),
        SessionState::Established,
        "cancel の処理でセッションを閉じないこと"
    );
}

/// responder が peer から REQUEST_OK を受けると PROTOCOL_VIOLATION になる
///
/// draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS): TRACK_STATUS_OK を送るのは
/// TRACK_STATUS を受けた側であり、その逆方向に REQUEST_OK が届くのは peer の違反である。
#[test]
fn responder_receiving_request_ok_closes_session() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("TRACK_STATUS の受信に成功すること");

    let err = server
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// responder が peer から REQUEST_ERROR を受けると PROTOCOL_VIOLATION になる
#[test]
fn responder_receiving_request_error_closes_session() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("TRACK_STATUS の受信に成功すること");

    let err = server
        .recv_stream_message(
            rid,
            ControlMessage::RequestError(RequestError {
                error_code: REQUEST_DOES_NOT_EXIST,
                retry_interval: 0,
                reason: ReasonPhrase::new("no").expect("正当な reason phrase である"),
                redirect: None,
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// 予約名前空間と `.session` 名前空間の TRACK_STATUS は REQUEST_ERROR で拒否される
///
/// draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces) / §6.5 (Session-Level Tracks and
/// Namespaces): SUBSCRIBE と共通の受信前検証が適用される。
#[test]
fn recv_track_status_reserved_namespaces_are_rejected() {
    use shiguredo_moqt::message::TrackStatus as WireTrackStatus;

    for (namespace, reason) in [
        (ns(&[b"."]), "single-period"),
        (ns(&[b".session"]), "session-level"),
    ] {
        let (_client, mut server) = establish_pair();
        let rid = 0u64;
        server
            .recv_request(ControlMessage::TrackStatus(WireTrackStatus {
                request_id: rid,
                track_namespace: namespace,
                track_name: b"cam".to_vec(),
                parameters: MessageParameters::new(),
            }))
            .expect("予約名前空間の TRACK_STATUS はセッションを閉じずに拒否すること");
        assert_eq!(
            server.state(),
            SessionState::Established,
            "{reason} 名前空間の拒否でセッションを閉じないこと"
        );
        assert!(
            server.track_status_request(rid).is_none(),
            "{reason} 名前空間の TRACK_STATUS は entry を作らないこと"
        );
        let mut saw_error = false;
        while let Some(e) = server.poll_event() {
            if let SessionEvent::SendOnStream {
                message: ControlMessage::RequestError(err),
                fin,
                ..
            } = e
            {
                assert_eq!(err.error_code, REQUEST_DOES_NOT_EXIST);
                assert!(fin, "拒否は FIN で閉じること");
                saw_error = true;
            }
        }
        assert!(
            saw_error,
            "{reason} 名前空間で REQUEST_ERROR が発行されること"
        );
    }
}

/// INCLUDE_PROPERTIES=0 の TRACK_STATUS への応答では Track Properties が空になる
///
/// draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter): "If
/// INCLUDE_PROPERTIES is 0, the Track Properties are still present in the message, but they
/// SHOULD be empty."
#[test]
fn track_status_ok_with_include_properties_zero_empties_track_properties() {
    let (mut client, mut server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_INCLUDE_PROPERTIES,
        value: MessageParameterValue::Uint8(0),
    });
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), params)
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("TRACK_STATUS の受信に成功すること");

    let properties = properties_with_publisher_priority();
    server
        .send_request_ok(rid, MessageParameters::new(), properties)
        .expect("TRACK_STATUS_OK の送信に成功すること");
    let (_, ok_msg, _) = take_send_on_stream_with_fin(&mut server);
    let ControlMessage::RequestOk(ok) = ok_msg else {
        panic!("REQUEST_OK が期待されたが {ok_msg:?} だった");
    };
    assert!(
        ok.track_properties.is_empty(),
        "INCLUDE_PROPERTIES=0 では Track Properties を空にすること"
    );
}

/// INCLUDE_PROPERTIES=1 の TRACK_STATUS への応答では Track Properties が保持される
#[test]
fn track_status_ok_with_include_properties_one_keeps_track_properties() {
    use shiguredo_moqt::message_parameter::PARAM_INCLUDE_PROPERTIES;
    use shiguredo_moqt::message_parameter::{MessageParameter, MessageParameterValue};

    let (mut client, mut server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_INCLUDE_PROPERTIES,
        value: MessageParameterValue::Uint8(1),
    });
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), params)
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("TRACK_STATUS の受信に成功すること");

    let properties = properties_with_publisher_priority();
    server
        .send_request_ok(rid, MessageParameters::new(), properties.clone())
        .expect("TRACK_STATUS_OK の送信に成功すること");
    let (_, ok_msg, _) = take_send_on_stream_with_fin(&mut server);
    let ControlMessage::RequestOk(ok) = ok_msg else {
        panic!("REQUEST_OK が期待されたが {ok_msg:?} だった");
    };
    assert_eq!(
        ok.track_properties, properties,
        "INCLUDE_PROPERTIES=1 では渡した Track Properties をそのまま載せること"
    );
}

/// 応答前に requester の FIN を受けても responder は終端せず TRACK_STATUS_OK を送れる
///
/// draft-ietf-moq-transport-21 §6.4.2.2 (Graceful Request Stream Closure) の FIN は
/// 方向ごとの終端であり cancel ではない。
#[test]
fn responder_sends_track_status_ok_after_requester_fin() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("TRACK_STATUS の受信に成功すること");

    // requester が送信方向を FIN で閉じる
    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("requester の FIN を受理すること");
    assert!(
        server
            .track_status_request(rid)
            .expect("entry が存在すること")
            .response
            .is_none(),
        "responder は peer FIN で応答待ち状態を変更しないこと"
    );
    // 応答経路が塞がれていない
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("requester の FIN 後でも TRACK_STATUS_OK を送れること");
    let (_, _, fin) = take_send_on_stream_with_fin(&mut server);
    assert!(fin, "TRACK_STATUS_OK の送信後に FIN すること");
}

/// requester が TRACK_STATUS を送った直後に FIN しても responder は応答を送れる
#[test]
fn requester_fin_then_responder_sends_track_status_ok() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("TRACK_STATUS の受信に成功すること");

    // requester (client) が FIN したことを responder が受け取る
    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("requester の FIN を受理すること");
    // responder が応答する
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("TRACK_STATUS_OK の送信に成功すること");
    let (_, ok_msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert!(fin, "TRACK_STATUS_OK の送信後に FIN すること");
    // requester は応答を受け取り、その後 responder の FIN を受ける
    client
        .recv_stream_message(rid, ok_msg)
        .expect("TRACK_STATUS_OK の受信に成功すること");
    client
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("responder の FIN を受理すること");
    let mut saw_finish = false;
    let mut saw_terminated = false;
    while let Some(e) = client.poll_event() {
        match e {
            SessionEvent::FinishRequestStream { request_id } => {
                assert_eq!(request_id, rid);
                saw_finish = true;
            }
            SessionEvent::RequestTerminated {
                request_id,
                kind: RequestKind::TrackStatus,
                ..
            } => {
                assert_eq!(request_id, rid);
                saw_terminated = true;
            }
            _ => {}
        }
    }
    assert!(
        saw_finish,
        "requester は responder の FIN で送信方向を閉じること"
    );
    assert!(saw_terminated, "requester 側は request を終端すること");
}

/// 受信側では peer の FIN で FinishRequestStream を発行しない
#[test]
fn responder_does_not_get_finish_request_stream_on_peer_fin() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("TRACK_STATUS の受信に成功すること");

    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("requester の FIN を受理すること");
    while let Some(e) = server.poll_event() {
        assert!(
            !matches!(e, SessionEvent::FinishRequestStream { .. }),
            "responder は peer FIN で送信方向を閉じないこと"
        );
    }
}

/// 予約名前空間と許可外パラメータは送信前に拒否される
#[test]
fn send_track_status_rejects_reserved_namespace_and_invalid_parameters() {
    use shiguredo_moqt::message_parameter::{MessageParameter, MessageParameterValue};
    use shiguredo_moqt::session::types::SendRequestError;

    let (mut client, _server) = establish_pair();

    // draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces): single period `.` は送信不可
    let err = client
        .send_track_status(ns(&[b"."]), b"cam".to_vec(), MessageParameters::new())
        .expect_err("予約名前空間への TRACK_STATUS は拒否されること");
    let SendRequestError::Session(err) = err else {
        panic!("SendRequestError::Session が期待されたが {err:?}");
    };
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);

    // draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope): TRACK_STATUS で許可されない
    // パラメータは送信前に拒否する
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: shiguredo_moqt::message_parameter::PARAM_OBJECT_DELIVERY_TIMEOUT,
        value: MessageParameterValue::VarInt(1000),
    });
    let err = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), params)
        .expect_err("許可外パラメータ付きの TRACK_STATUS は拒否されること");
    let SendRequestError::Session(err) = err else {
        panic!("SendRequestError::Session が期待されたが {err:?}");
    };
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}
