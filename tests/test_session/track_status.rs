//! TRACK_STATUS の送信側 (自側 subscriber) のテスト群
//!
//! 本ライブラリは TRACK_STATUS の受信側 (自側 publisher) を実装しないため、
//! 送信と応答受信、および peer から受信したときの拒否を検証する。
use super::*;
use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
use shiguredo_moqt::message::{ReasonPhrase, RequestError};
use shiguredo_moqt::session::types::{RecvRequestError, TrackStatusResponse};

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

/// peer から TRACK_STATUS を受信すると未対応 request としてセッションを閉じる
///
/// 本ライブラリは TRACK_STATUS の受信側 (自側 publisher) を実装しないため、
/// `recv_request` の未対応経路で `SESSION_PROTOCOL_VIOLATION` になる。
#[test]
fn recv_peer_track_status_closes_session() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, message) = take_send_request(&mut client);

    let err = server
        .recv_request(message)
        .expect_err("TRACK_STATUS の受信は未対応として拒否されること");
    let session_err = match err {
        RecvRequestError::Session(err) => err,
        other => panic!("Session エラーが期待されたが {other:?} を受け取った"),
    };
    assert_eq!(
        session_err.code, SESSION_PROTOCOL_VIOLATION,
        "エラーコードが PROTOCOL_VIOLATION であること"
    );
    assert_eq!(
        server.state(),
        SessionState::Closing,
        "セッションが Closing に遷移すること"
    );

    // drain snapshot には自側で送信した TRACK_STATUS のみが現れる
    let snapshot = client.goaway_drain_snapshot();
    assert!(
        snapshot.blocking_track_status_request_ids.contains(&rid),
        "応答待ちの TRACK_STATUS が drain blocker になること"
    );
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
