//! TRACK_STATUS の送信側 (自側 subscriber) と受信側 (自側 publisher) のテスト群
//!
//! draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS): 受信側は SUBSCRIBE と同様に扱うが、
//! subscription state も Track Alias も作らず Objects も送らない。応答 (TRACK_STATUS_OK /
//! REQUEST_ERROR) の送信後に bidi stream を FIN で閉じる。
//!
//! draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties): 未知の
//! Mandatory Track Property を含む TRACK_STATUS_OK は Track を利用可能な成功として扱わず、
//! 専用イベントで未対応の型を通知する。
use super::*;
use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
use shiguredo_moqt::message::{ReasonPhrase, RequestError};
use shiguredo_moqt::message_parameter::{
    AuthorizationToken, MessageParameter, MessageParameterValue, PARAM_AUTHORIZATION_TOKEN,
    PARAM_INCLUDE_PROPERTIES,
};
use shiguredo_moqt::session::types::{
    RequestKind, RequestStreamEnd, TrackRole, TrackStatusResponse,
};
use shiguredo_moqt::track_properties::{
    PROP_DEFAULT_PUBLISHER_PRIORITY, PROP_IMMUTABLE_PROPERTIES, TrackProperty, TrackPropertyValue,
};

/// DEFAULT_PUBLISHER_PRIORITY を 1 件持つ空でない Track Properties を作る
fn properties_with_publisher_priority() -> TrackProperties {
    let mut properties = TrackProperties::new();
    properties.push(TrackProperty {
        prop_type: PROP_DEFAULT_PUBLISHER_PRIORITY,
        value: TrackPropertyValue::VarInt(7),
    });
    properties
}

/// 未知の Mandatory Track Property を含む Track Properties を作る
///
/// draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties) の
/// 0x4000-0x7FFF のうち、この実装が意味を理解しない 0x5000 (mutable リスト) と 0x6000
/// (IMMUTABLE_PROPERTIES の内側) を置く。§16.8 (Properties) の既知型 0x0E と §13 (Grease) の
/// GREASE 値 0x401D は未対応に含めないことを確かめられる構成にしてある。
fn properties_with_unknown_mandatory() -> TrackProperties {
    let mut inner = TrackProperties::new();
    inner.push(TrackProperty {
        prop_type: 0x6000,
        value: TrackPropertyValue::VarInt(1),
    });
    inner.push(TrackProperty {
        prop_type: 0x401D,
        value: TrackPropertyValue::Bytes(vec![0xAB]),
    });
    inner.push(TrackProperty {
        prop_type: PROP_DEFAULT_PUBLISHER_PRIORITY,
        value: TrackPropertyValue::VarInt(128),
    });
    let mut inner_bytes = Vec::new();
    inner
        .encode(&mut inner_bytes)
        .expect("IMMUTABLE_PROPERTIES の内側は encode 可能であること");

    let mut properties = TrackProperties::new();
    properties.push(TrackProperty {
        prop_type: PROP_IMMUTABLE_PROPERTIES,
        value: TrackPropertyValue::Bytes(inner_bytes),
    });
    properties.push(TrackProperty {
        prop_type: 0x5000,
        value: TrackPropertyValue::VarInt(2),
    });
    properties
}

/// Object 専用の既知 Property (PRIOR_GROUP_ID_GAP) を含む Track Properties を作る
///
/// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1 (Malformed Tracks)
/// 条件 8 のスコープ違反であり、Malformed Track としてローカル終端させるフィクスチャに使う。
fn properties_with_object_only_property() -> TrackProperties {
    use shiguredo_moqt::object_properties::PROP_PRIOR_GROUP_ID_GAP;

    let mut properties = TrackProperties::new();
    properties.push(TrackProperty {
        prop_type: PROP_PRIOR_GROUP_ID_GAP,
        value: TrackPropertyValue::VarInt(1),
    });
    properties
}

/// TRACK_STATUS_OK の wire 表現 (Type 0x07) を組み立てる
///
/// Parameters は 0 個、Track Properties は引数の生バイト列をそのまま載せる。
fn track_status_ok_wire(track_properties: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    shiguredo_moqt::varint::encode(0, &mut payload);
    payload.extend_from_slice(track_properties);
    let mut wire = Vec::new();
    shiguredo_moqt::varint::encode(0x07, &mut wire);
    shiguredo_moqt::varint::encode(payload.len() as u64, &mut wire);
    wire.extend_from_slice(&payload);
    wire
}

/// TRACK_STATUS_OK で許可されない AUTHORIZATION_TOKEN を 1 つ持つパラメータ列
///
/// draft-ietf-moq-transport-23 §9.3 (REQUEST_OK): TRACK_STATUS_OK が運べるのは EXPIRES と
/// LARGEST_OBJECT だけである。
fn authorization_token_params() -> MessageParameters {
    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter {
        param_type: PARAM_AUTHORIZATION_TOKEN,
        value: MessageParameterValue::AuthorizationToken(AuthorizationToken::UseValue {
            token_type: 1,
            token_value: vec![0x01],
        }),
    });
    parameters
}

/// TRACK_STATUS を送信し、server に受信させて request id を返す
fn send_track_status_to_server(client: &mut Session, server: &mut Session) -> u64 {
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, message) = take_send_request(client);
    server
        .recv_request(message)
        .expect("TRACK_STATUS の受信に成功すること");
    rid
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
/// draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS): 受信側は SUBSCRIBE と同様に扱い、
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
            largest_location: None,
            expires_ms: None,
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

/// TRACK_STATUS_OK の EXPIRES を仮の購読期限として保持し、参照できる
///
/// draft-ietf-moq-transport-23 §9.3 (REQUEST_OK): "TRACK_STATUS_OK: EXPIRES,
/// LARGEST_OBJECT"。§9.19.16 (EXPIRES Parameter): "EXPIRES in TRACK_STATUS_OK indicates
/// the value the sender would have included in a SUBSCRIBE_OK for the same Track."
/// TRACK_STATUS は subscription state を作らない単発要求であるため (§9.12
/// (TRACK_STATUS))、値だけを保持し、要求を再び生かすこともしない。
#[test]
fn track_status_ok_with_expires_records_tentative_subscription_expiry() {
    let (mut client, mut server) = establish_pair();
    let rid = send_track_status_to_server(&mut client, &mut server);

    server
        .send_request_ok(rid, expires_params(30_000), TrackProperties::new())
        .expect("TRACK_STATUS_OK の EXPIRES は送信できること");
    let (send_rid, ok_msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert_eq!(send_rid, rid);
    assert!(
        matches!(
            &ok_msg,
            ControlMessage::RequestOk(ok) if ok.parameters.expires() == Some(30_000)
        ),
        "EXPIRES が TRACK_STATUS_OK のワイヤ表現に載ること"
    );
    assert!(fin, "TRACK_STATUS_OK の送信後に bidi stream を閉じること");

    client
        .recv_stream_message(rid, ok_msg)
        .expect("TRACK_STATUS_OK の EXPIRES は受信できること");

    let entry = client
        .track_status_request(rid)
        .expect("entry が存在すること");
    assert_eq!(
        entry.response,
        Some(TrackStatusResponse::Ok {
            largest_location: None,
            expires_ms: Some(30_000),
        }),
        "受信した EXPIRES を応答に保持すること"
    );
    assert_eq!(
        entry.expires_ms(),
        Some(30_000),
        "仮の購読期限として参照できること"
    );
    // TRACK_STATUS は subscription state も fetch state も作らない (§9.12)
    assert!(client.subscription(rid).is_none());
    assert!(client.fetch(rid).is_none());
    assert_eq!(client.state(), SessionState::Established);

    let mut saw_ok = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::RequestOkReceived {
            request_id,
            parameters,
            ..
        } = e
        {
            assert_eq!(request_id, rid);
            assert_eq!(
                parameters.expires(),
                Some(30_000),
                "受信イベントからも EXPIRES を参照できること"
            );
            saw_ok = true;
        }
    }
    assert!(saw_ok, "RequestOkReceived が発行されること");
}

/// TRACK_STATUS_OK の EXPIRES は期限タイマーを起動せず、FIN と破棄条件にも影響しない
///
/// §9.19.16 (EXPIRES Parameter) の値は同じ Track の SUBSCRIBE_OK で提示される見込みの
/// 購読期限であり、TRACK_STATUS 要求自体の期限ではない。期限を大きく過ぎても要求は
/// 終端せず、応答受領済みの entry は従来どおり FIN の交換後に破棄できる。
#[test]
fn track_status_ok_expires_does_not_start_deadline_timer() {
    let (mut client, mut server) = establish_pair();
    client.tick(0);
    let rid = send_track_status_to_server(&mut client, &mut server);

    server
        .send_request_ok(rid, expires_params(10), TrackProperties::new())
        .expect("TRACK_STATUS_OK の EXPIRES は送信できること");
    let (_, ok_msg, _) = take_send_on_stream_with_fin(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("TRACK_STATUS_OK の EXPIRES は受信できること");
    while client.poll_event().is_some() {}

    // EXPIRES (10ms) を大きく超えて進めても、期限切れによる終端や切断を起こさない
    client.tick(10_000);
    while let Some(e) = client.poll_event() {
        assert!(
            !matches!(
                e,
                SessionEvent::CloseSession(_) | SessionEvent::RequestTerminated { .. }
            ),
            "EXPIRES で期限タイマーを起動しないこと: {e:?}"
        );
    }
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(
        client
            .track_status_request(rid)
            .expect("entry が存在すること")
            .expires_ms(),
        Some(10),
        "期限を過ぎても保持した値を参照できること"
    );

    // 応答受領済みの単発要求は FIN の交換後に破棄できる (EXPIRES の有無で変えない)
    client
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("responder の FIN を受理すること");
    let mut saw_finish = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::FinishRequestStream { request_id } = e {
            assert_eq!(request_id, rid);
            saw_finish = true;
        }
    }
    assert!(
        saw_finish,
        "requester は responder の FIN で送信方向を閉じること"
    );
    assert!(
        client.forget_track_status(rid).is_some(),
        "応答受領済みの entry は EXPIRES があっても破棄できること"
    );
}

/// TRACK_STATUS_OK の EXPIRES=0 は「期限なし」として None に正規化する
///
/// §9.19.16 (EXPIRES Parameter): "If the EXPIRES parameter is 0 or is not present in a
/// message, the request does not expire or expires at an unknown time." パラメータ自体は
/// 存在するため、不在と 0 を区別できることを固定する。
#[test]
fn track_status_ok_with_zero_expires_has_no_expiry() {
    let (mut client, mut server) = establish_pair();
    let rid = send_track_status_to_server(&mut client, &mut server);

    server
        .send_request_ok(rid, expires_params(0), TrackProperties::new())
        .expect("TRACK_STATUS_OK の EXPIRES=0 は送信できること");
    let (_, ok_msg, _) = take_send_on_stream_with_fin(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("TRACK_STATUS_OK の EXPIRES=0 は受信できること");

    assert_eq!(
        client
            .track_status_request(rid)
            .expect("entry が存在すること")
            .expires_ms(),
        None,
        "EXPIRES=0 は期限なし (None) として扱うこと"
    );
    let mut saw_ok = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::RequestOkReceived { parameters, .. } = e {
            assert!(
                parameters.has_expires(),
                "EXPIRES=0 もパラメータとしては存在すること"
            );
            assert_eq!(parameters.expires(), None);
            saw_ok = true;
        }
    }
    assert!(saw_ok, "RequestOkReceived が発行されること");
}

/// TRACK_STATUS_OK の許可集合は EXPIRES と LARGEST_OBJECT に限られる
///
/// draft-ietf-moq-transport-23 §9.3 (REQUEST_OK): TRACK_STATUS_OK が運べるのは EXPIRES と
/// LARGEST_OBJECT だけである。許可集合を広げすぎていないことを送受信の双方で固定する。
#[test]
fn track_status_ok_disallowed_parameter_is_rejected() {
    let (mut client, mut server) = establish_pair();
    let rid = send_track_status_to_server(&mut client, &mut server);

    // 送信側: 許可外のパラメータを含む応答は状態を変えずに拒否する
    let err = server
        .send_request_ok(rid, authorization_token_params(), TrackProperties::new())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert!(
        server
            .track_status_request(rid)
            .expect("entry が存在すること")
            .response
            .is_none(),
        "拒否した応答で entry を応答済みにしないこと"
    );

    // 受信側: decode 済みメッセージを直接渡す API 経路でも同じ許可集合で検証する
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
                parameters: authorization_token_params(),
                track_properties: TrackProperties::new(),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_INVALID_PARAMETER);
    assert_eq!(client.state(), SessionState::Closing);
}

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
                error_details: ErrorDetails::new(),
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
/// draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS): 受信側は SUBSCRIBE と同様に扱うが、
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
    // §9.12: subscription state を作らない
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
/// draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS): "The bidi stream is closed with a FIN
/// after TRACK_STATUS_OK or TRACK_STATUS_ERROR are sent."
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
            largest_location: None,
            expires_ms: None,
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
            largest_location: None,
            expires_ms: None,
        })
    );
}

/// 受信した TRACK_STATUS は REQUEST_ERROR で拒否でき、bidi stream が FIN で閉じる
///
/// draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS): "A publisher responds to a failed
/// TRACK_STATUS with an appropriate TRACK_STATUS_ERROR message. The bidi stream is closed
/// with a FIN after TRACK_STATUS_OK or TRACK_STATUS_ERROR are sent."
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
            ErrorDetails::new(),
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
            ErrorDetails::new(),
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
/// draft-ietf-moq-transport-23 §6.4.2.3 (Request Cancellation and Rejection): "Once a request
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
                error_code: Some(shiguredo_moqt::error::STREAM_CANCELLED),
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
/// draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS): TRACK_STATUS_OK を送るのは
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
    assert_eq!(err.code, SESSION_ROLE_VIOLATION);
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
                error_details: ErrorDetails::new(),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_ROLE_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// 予約名前空間と `.session` 名前空間の TRACK_STATUS は REQUEST_ERROR で拒否される
///
/// draft-ietf-moq-transport-23 §2.4.3 (Reserved Namespaces) / §6.5 (Session-Level Tracks and
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

/// 空の Track Name を持つ TRACK_STATUS は PROTOCOL_VIOLATION でセッションを閉じる
///
/// draft-ietf-moq-transport-23 §2.4.1 (Track Naming): 長さ 0 の Track Name を受信した
/// endpoint は PROTOCOL_VIOLATION でセッションを閉じる (MUST)。TRACK_STATUS は SUBSCRIBE と
/// 同じ受信前検証を通るため、通常名前空間と `.session` 名前空間の両方で同じ分類になる。
#[test]
fn recv_track_status_empty_track_name_closes_session() {
    use shiguredo_moqt::message::TrackStatus as WireTrackStatus;

    for namespace in [ns(&[b"live"]), ns(&[b".session"])] {
        let (_client, mut server) = establish_pair();
        let err = server
            .recv_request(ControlMessage::TrackStatus(WireTrackStatus {
                request_id: 0,
                track_namespace: namespace,
                track_name: Vec::new(),
                parameters: MessageParameters::new(),
            }))
            .expect_err("空の Track Name は PROTOCOL_VIOLATION になる");
        assert_eq!(
            err.as_session_error()
                .expect("Session エラーであること")
                .code,
            SESSION_PROTOCOL_VIOLATION
        );
        assert_eq!(server.state(), SessionState::Closing);
        // entry を作らず REQUEST_ERROR も送らない
        assert!(server.track_status_request(0).is_none());
        while let Some(e) = server.poll_event() {
            assert!(
                !matches!(e, SessionEvent::SendOnStream { .. }),
                "空の Track Name で REQUEST_ERROR を送ってはならない"
            );
        }
    }
}

/// INCLUDE_PROPERTIES=0 の TRACK_STATUS への応答では Track Properties が空になる
///
/// draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter): "If
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

/// responder 側のイベント列で、TRACK_STATUS_OK の最終メッセージが RequestTerminated より先に
/// 出ることを確認し、TRACK_STATUS_OK を返す
///
/// 自側の最終メッセージで request の終端が確定する場合、`RequestTerminated` はその API 呼び出しで
/// 当該 request に積む最後のイベントでなければならない。逆順だと、`RequestTerminated` を受けて
/// 送信半を捨てる I/O 層が TRACK_STATUS_OK と FIN を落とす (draft-ietf-moq-transport-23 §9.12
/// (TRACK_STATUS) の "The bidi stream is closed with a FIN after TRACK_STATUS_OK ... are sent")。
fn assert_track_status_ok_before_terminated(server: &mut Session, rid: u64) -> ControlMessage {
    let events = drain_events(server);
    let ok_index = event_position(&events, "FIN 付きの TRACK_STATUS_OK", |e| {
        matches!(
            e,
            SessionEvent::SendOnStream {
                request_id,
                message: ControlMessage::RequestOk(_),
                fin: true,
            } if *request_id == rid
        )
    });
    let terminated_index = event_position(&events, "RequestTerminated(PeerStreamFin)", |e| {
        matches!(
            e,
            SessionEvent::RequestTerminated {
                request_id,
                kind: RequestKind::TrackStatus,
                reason: TerminationReason::PeerStreamFin,
            } if *request_id == rid
        )
    });
    assert!(
        ok_index < terminated_index,
        "最終メッセージの SendOnStream は RequestTerminated より先に出ること: {events:?}"
    );
    assert_eq!(
        terminated_index,
        events.len() - 1,
        "RequestTerminated は当該 request の最後のイベントであること: {events:?}"
    );
    match &events[ok_index] {
        SessionEvent::SendOnStream { message, .. } => message.clone(),
        _ => unreachable!("位置は SendOnStream を指している"),
    }
}

/// 応答前に requester の FIN を受けても responder は終端せず TRACK_STATUS_OK を送れる
///
/// draft-ietf-moq-transport-23 §6.4.2.2 (Graceful Request Stream Closure) の FIN は
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
    // TRACK_STATUS_OK を FIN 付きで送り、その後に両方向の終端で RequestTerminated が出る
    assert_track_status_ok_before_terminated(&mut server, rid);
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
    // TRACK_STATUS_OK を FIN 付きで送り、その後に responder 側の RequestTerminated が出る
    let ok_msg = assert_track_status_ok_before_terminated(&mut server, rid);
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

    // draft-ietf-moq-transport-23 §2.4.3 (Reserved Namespaces): single period `.` は送信不可
    let err = client
        .send_track_status(ns(&[b"."]), b"cam".to_vec(), MessageParameters::new())
        .expect_err("予約名前空間への TRACK_STATUS は拒否されること");
    let SendRequestError::Session(err) = err else {
        panic!("SendRequestError::Session が期待されたが {err:?}");
    };
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);

    // draft-ietf-moq-transport-23 §9.19.1 (Parameter Scope): TRACK_STATUS で許可されない
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

/// send_track_status に空の Track Name を渡すと SESSION_PROTOCOL_VIOLATION が返る
///
/// draft-ietf-moq-transport-23 §2.4.1 (Track Naming): 空の Track Name は peer が
/// PROTOCOL_VIOLATION でセッションを閉じる MUST の対象であり、送出してはならない。
/// 送信 API は状態登録と request_id 採番より前に拒否し、request_id を消費しない。
#[test]
fn send_track_status_empty_track_name_rejected() {
    use shiguredo_moqt::session::types::SendRequestError;

    let (mut client, _server) = establish_pair();
    let err = client
        .send_track_status(ns(&[b"live"]), Vec::new(), MessageParameters::new())
        .expect_err("空の Track Name の TRACK_STATUS は拒否されること");
    let SendRequestError::Session(err) = err else {
        panic!("SendRequestError::Session が期待されたが {err:?}");
    };
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_no_tracked_requests(&client);
    // 拒否後に送信できる最初の TRACK_STATUS が request id 0 を使うこと
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("拒否後も正常な TRACK_STATUS を送信できること");
    assert_eq!(rid, 0);
}

// ─── 未知の Mandatory Track Property (draft-ietf-moq-transport-23 §3.7) ───

/// 未知の Mandatory Track Property を含む TRACK_STATUS_OK は未対応として記録する
///
/// draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties): 未知の
/// Mandatory Track Property を含む TRACK_STATUS_OK を受信した endpoint は、その Track を
/// 処理してはならない (MUST)。応答自体は受領済みであり、成功イベントではなく専用イベントで
/// 未対応の型を通知する。
#[test]
fn track_status_ok_with_unknown_mandatory_property_is_unsupported() {
    let (mut client, mut server) = establish_pair();
    let rid = send_track_status_to_server(&mut client, &mut server);

    server
        .send_request_ok(
            rid,
            MessageParameters::new(),
            properties_with_unknown_mandatory(),
        )
        .expect("未知の Mandatory Track Property の送信でローカル検証に失敗しないこと");
    let (_, ok_msg, _) = take_send_on_stream_with_fin(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("未対応の検出でセッションを閉じないこと");

    assert_eq!(
        client
            .track_status_request(rid)
            .expect("entry が存在すること")
            .response,
        Some(TrackStatusResponse::Unsupported),
        "応答を受領済みの未対応結果として記録すること"
    );
    assert_eq!(
        client.state(),
        SessionState::Established,
        "未対応の検出でセッションを閉じないこと"
    );

    let mut unsupported_types = None;
    let mut saw_ok = false;
    let mut saw_request_error = false;
    while let Some(e) = client.poll_event() {
        match e {
            SessionEvent::TrackStatusUnsupported {
                request_id,
                unsupported_property_types,
            } => {
                assert_eq!(request_id, rid);
                unsupported_types = Some(unsupported_property_types);
            }
            SessionEvent::RequestOkReceived { .. } => saw_ok = true,
            // 既に届いた TRACK_STATUS_OK に対して逆向きに REQUEST_ERROR を返さない
            SessionEvent::SendOnStream {
                message: ControlMessage::RequestError(_),
                ..
            } => saw_request_error = true,
            _ => {}
        }
    }
    assert_eq!(
        unsupported_types,
        Some(vec![0x5000, 0x6000]),
        "mutable と IMMUTABLE 内側の未知の型を昇順で通知し、GREASE と既知型を含めないこと"
    );
    assert!(!saw_ok, "通常の成功通知を発行しないこと");
    assert!(
        !saw_request_error,
        "応答受信済みの要求に REQUEST_ERROR を返さないこと"
    );
}

/// GREASE 値だけを含む TRACK_STATUS_OK は通常の成功として扱う
///
/// draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties) は GREASE 値を
/// Mandatory Track Property から除き、§13 (Grease) は未知値の受信だけでセッションを閉じる
/// ことを禁じる。GREASE を未対応と誤認しないことを固定する。
#[test]
fn track_status_ok_with_grease_property_is_success() {
    let (mut client, mut server) = establish_pair();
    let rid = send_track_status_to_server(&mut client, &mut server);

    let mut properties = TrackProperties::new();
    properties.push(TrackProperty {
        prop_type: 0x401D,
        value: TrackPropertyValue::Bytes(vec![0xAB]),
    });
    server
        .send_request_ok(rid, MessageParameters::new(), properties)
        .expect("GREASE 値の送信に成功すること");
    let (_, ok_msg, _) = take_send_on_stream_with_fin(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("GREASE 値の受信でセッションを閉じないこと");

    assert_eq!(
        client
            .track_status_request(rid)
            .expect("entry が存在すること")
            .response,
        Some(TrackStatusResponse::Ok {
            largest_location: None,
            expires_ms: None,
        }),
        "GREASE 値を未対応と誤認せず成功として扱うこと"
    );
    let mut saw_ok = false;
    while let Some(e) = client.poll_event() {
        match e {
            SessionEvent::RequestOkReceived { request_id, .. } => {
                assert_eq!(request_id, rid);
                saw_ok = true;
            }
            SessionEvent::TrackStatusUnsupported { .. } => {
                panic!("GREASE 値を未対応として通知してはならない")
            }
            _ => {}
        }
    }
    assert!(saw_ok, "通常の成功通知が発行されること");
}

/// wire 入力の TRACK_STATUS_OK でも未知の Mandatory Track Property を未対応として扱う
///
/// 構造 decode は未知 Mandatory の判定を行わず Properties を保持し、Request ID を持つ
/// 受信ハンドラが §3.7 の判定を行う (構造と意味の分離)。
#[test]
fn track_status_ok_with_unknown_mandatory_property_on_wire_is_unsupported() {
    let (mut client, mut server) = establish_pair();
    let rid = send_track_status_to_server(&mut client, &mut server);

    let mut properties_bytes = Vec::new();
    properties_with_unknown_mandatory()
        .encode(&mut properties_bytes)
        .expect("未知の Mandatory Track Property は encode 可能であること");
    let wire = track_status_ok_wire(&properties_bytes);
    let (message, consumed) =
        ControlMessage::decode(&wire).expect("未知 Mandatory は構造 decode を失敗させないこと");
    assert_eq!(consumed, wire.len());
    client
        .recv_stream_message(rid, message)
        .expect("未対応の検出でセッションを閉じないこと");

    assert_eq!(
        client
            .track_status_request(rid)
            .expect("entry が存在すること")
            .response,
        Some(TrackStatusResponse::Unsupported)
    );
    let mut saw_unsupported = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::TrackStatusUnsupported {
            unsupported_property_types,
            ..
        } = e
        {
            assert_eq!(
                unsupported_property_types,
                vec![0x5000, 0x6000],
                "wire 経由でも IMMUTABLE 内側の型を列挙すること"
            );
            saw_unsupported = true;
        }
    }
    assert!(saw_unsupported, "TrackStatusUnsupported が発行されること");
}

/// 未対応結果でも応答タイマー (CONTROL_MESSAGE_TIMEOUT) を解除する
#[test]
fn track_status_unsupported_clears_control_message_deadline() {
    let (mut client, mut server) = establish_pair();
    client.tick(0);
    client.set_control_message_timeout_ms(Some(10));
    let rid = send_track_status_to_server(&mut client, &mut server);

    server
        .send_request_ok(
            rid,
            MessageParameters::new(),
            properties_with_unknown_mandatory(),
        )
        .expect("未知の Mandatory Track Property の送信に成功すること");
    let (_, ok_msg, _) = take_send_on_stream_with_fin(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("未対応の検出でセッションを閉じないこと");
    while client.poll_event().is_some() {}

    // deadline は 10ms であり、100ms 経過しても応答待ちが残っていなければ発火しない
    client.tick(100);
    while let Some(e) = client.poll_event() {
        assert!(
            !matches!(e, SessionEvent::CloseSession(_)),
            "未対応結果で応答タイマーを解除すること"
        );
    }
    assert_eq!(
        client.state(),
        SessionState::Established,
        "応答タイマーの解除後もセッションが確立したままであること"
    );
}

/// 未対応結果でも FIN の交換と forget は応答受領済みの単発要求と同じに扱う
///
/// draft-ietf-moq-transport-23 §6.4.2.2 (Graceful Request Stream Closure): responder の FIN を
/// 受けた requester は送信方向を FIN で閉じる。TRACK_STATUS_OK を受領済みの entry は
/// `forget_track_status` で破棄できる。
#[test]
fn track_status_unsupported_allows_fin_exchange_and_forget() {
    let (mut client, mut server) = establish_pair();
    let rid = send_track_status_to_server(&mut client, &mut server);

    server
        .send_request_ok(
            rid,
            MessageParameters::new(),
            properties_with_unknown_mandatory(),
        )
        .expect("未知の Mandatory Track Property の送信に成功すること");
    let (_, ok_msg, _) = take_send_on_stream_with_fin(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("未対応の検出でセッションを閉じないこと");

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
    assert!(
        client.forget_track_status(rid).is_some(),
        "応答受領済みの entry は破棄できること"
    );
    assert!(
        client.forget_track_status(rid).is_none(),
        "除去後の再 forget は None を返すこと"
    );
}

/// ローカル cancel 済みの TRACK_STATUS に遅延した OK が届いても未対応通知を発行しない
///
/// 同じ Track の Malformed Track 検出で TRACK_STATUS がローカル終端されると、自側は request
/// stream の送信方向を reset する。peer が reset を観測する前に送った TRACK_STATUS_OK は
/// 正当に届くため吸収し、利用しないと決めた Track の未対応型を改めて通知しない。
#[test]
fn late_request_ok_for_locally_cancelled_track_status_is_absorbed() {
    let (mut client, mut server) = establish_pair();
    let rid = send_track_status_to_server(&mut client, &mut server);

    // 1 通目は Object 専用 Property を含み、Malformed Track としてローカル終端される
    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
                parameters: MessageParameters::new(),
                track_properties: properties_with_object_only_property(),
            }),
        )
        .expect("Malformed Track の検出でセッションを閉じないこと");
    assert_eq!(
        client
            .track_status_request(rid)
            .expect("entry が存在すること")
            .response,
        Some(TrackStatusResponse::Error),
        "Malformed Track は応答済みの失敗として確定すること"
    );
    while client.poll_event().is_some() {}

    // 遅延して届いた 2 通目 (未知の Mandatory Track Property を含む)
    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
                parameters: MessageParameters::new(),
                track_properties: properties_with_unknown_mandatory(),
            }),
        )
        .expect("ローカル終端済みの request への遅延応答は吸収すること");
    assert_eq!(
        client.state(),
        SessionState::Established,
        "遅延応答の吸収でセッションを閉じないこと"
    );
    assert_eq!(
        client
            .track_status_request(rid)
            .expect("entry が存在すること")
            .response,
        Some(TrackStatusResponse::Error),
        "既存の失敗結果を変更しないこと"
    );
    while let Some(e) = client.poll_event() {
        assert!(
            !matches!(e, SessionEvent::TrackStatusUnsupported { .. }),
            "ローカル終端済みの entry に新たな未対応通知を発行しないこと"
        );
    }
}
