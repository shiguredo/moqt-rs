//! TRACK_STATUS メッセージに関するテスト

use super::*;

/// TRACK_STATUS の両端ハンドシェイク
#[test]
fn track_status_full_cycle() {
    use shiguredo_moqt::session::types::TrackStatusResponse;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ts_msg) = take_send_request(&mut client);
    server
        .recv_request(ts_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(matches!(
        client
            .track_status_request(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .response,
        Some(TrackStatusResponse::Ok {
            largest_location: None
        })
    ));
    assert!(matches!(
        server
            .track_status_request(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .response,
        Some(TrackStatusResponse::Ok {
            largest_location: None
        })
    ));
}

/// TRACK_STATUS への REQUEST_OK が LARGEST_OBJECT を含むと `largest_location`
/// が保存される (draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter): LARGEST_OBJECT は TRACK_STATUS_OK にも乗る)
#[test]
fn track_status_ok_largest_object_saved() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ts_msg) = take_send_request(&mut client);
    server
        .recv_request(ts_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 99,
            object: 17,
        },
    });
    server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    use shiguredo_moqt::session::types::TrackStatusResponse;
    assert_eq!(
        client
            .track_status_request(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .response,
        Some(TrackStatusResponse::Ok {
            largest_location: Some(Location {
                group_id: 99,
                object_id: 17,
            }),
        })
    );
}

/// .session 名前空間の空トラック名に対する TRACK_STATUS は DOES_NOT_EXIST で拒否される
/// (draft-ietf-moq-transport-21 §6.5 (Session-Level Tracks and Namespaces))
#[test]
fn track_status_session_namespace_empty_track_rejected() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message::TrackStatus as WireTrackStatus;
    let (_, mut server) = establish_pair();
    server
        .recv_request(ControlMessage::TrackStatus(WireTrackStatus {
            request_id: 0,
            track_namespace: ns(&[b".session"]),
            track_name: vec![],
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
}

/// TRACK_STATUS の REQUEST_OK で RequestOkReceived(request_kind=TrackStatus) が発火する
#[test]
fn track_status_request_ok_emits_request_ok_received_event_with_kind_track_status() {
    use shiguredo_moqt::session::types::TrackStatusResponse;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ts_msg) = take_send_request(&mut client);
    server
        .recv_request(ts_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // TRACK_STATUS の REQUEST_OK 受信時に response が Ok で設定される
    assert!(matches!(
        client
            .track_status_request(rid)
            .expect("track_status_request が存在すること")
            .response,
        Some(TrackStatusResponse::Ok { .. })
    ));

    // TRACK_STATUS の REQUEST_OK 受信時に RequestOkReceived(TrackStatus) が発火する
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestOkReceived {
            request_id,
            request_kind,
            ..
        } = ev
            && request_id == rid
        {
            assert_eq!(request_kind, RequestKind::TrackStatus);
            got = true;
            break;
        }
    }
    assert!(
        got,
        "TRACK_STATUS の REQUEST_OK 受信時に RequestOkReceived(TrackStatus) が発火すること"
    );
}

/// single period `.` 予約名前空間の TRACK_STATUS は track_name 非空でも DOES_NOT_EXIST で拒否される
/// (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces))
#[test]
fn track_status_single_period_namespace_rejected() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message::TrackStatus as WireTrackStatus;
    let (_, mut server) = establish_pair();
    server
        .recv_request(ControlMessage::TrackStatus(WireTrackStatus {
            request_id: 0,
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
}

/// send_track_status に single period `.` 予約名前空間を渡すと SESSION_PROTOCOL_VIOLATION が返る
/// (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces))
#[test]
fn send_track_status_single_period_namespace_rejected() {
    let (mut client, _server) = establish_pair();
    let err = client
        .send_track_status(ns(&[b"."]), b"cam".to_vec(), MessageParameters::new())
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
}

/// スコープ外パラメータを含む `send_track_status` は API 呼び出し時に
/// `Err(SendRequestError::Session(...))` を返し、状態・イベント・ request_id を汚染しないこと
///
/// draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope): 検証がないと、
/// TrackStatusEntry の登録・ control deadline の開始・ SendRequest の push まで
/// 完了した後に I/O 層のエンコード時 (validate_scope) で非同期に失敗する。
/// 検証は request_id 発行より前に置き、エラー時に欠番を作らない。
#[test]
fn send_track_status_with_out_of_scope_parameter_rejected_without_state_change() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES,
    };
    use shiguredo_moqt::session::types::SendRequestError;
    let (mut client, _server) = establish_pair();
    let before = client.track_status_requests().count();
    // TRACK_STATUS で許可されない EXPIRES を載せる
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(1000),
    });
    let err = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), params)
        .unwrap_err();
    match err {
        SendRequestError::Session(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("SendRequestError::Session が期待される: {other:?}"),
    }
    // TrackStatusEntry が登録されないこと
    assert_eq!(
        client.track_status_requests().count(),
        before,
        "検証エラー時に TrackStatusEntry が登録されないこと"
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
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("検証エラー後の正常送信は成功すること");
    assert_eq!(rid, 0, "検証エラーで request_id が欠番にならないこと");
}
