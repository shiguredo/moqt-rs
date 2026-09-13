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
            assert_eq!(e.reason.as_str(), "empty track name in .session namespace");
        }
        _ => panic!("DOES_NOT_EXIST の RequestError が期待される"),
    }
}

/// .session 名前空間の非空トラック名に対する TRACK_STATUS は DOES_NOT_EXIST で拒否される
///
/// draft-ietf-moq-transport-21 §6.5 (Session-Level Tracks and Namespaces): 未認識の
/// session-level track へのリクエストは Application へ渡さず REQUEST_ERROR で拒否する。
/// 本ライブラリは session-level track を登録しないため、非空トラック名も拒否対象になる。
#[test]
fn track_status_session_namespace_non_empty_track_rejected() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message::TrackStatus as WireTrackStatus;
    let (_, mut server) = establish_pair();
    server
        .recv_request(ControlMessage::TrackStatus(WireTrackStatus {
            request_id: 0,
            track_namespace: ns(&[b".session"]),
            track_name: b"extension".to_vec(),
            parameters: MessageParameters::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    // リクエスト登録より前に拒否されるため track_status_request は残らない
    assert!(server.track_status_request(0).is_none());
    let (stream_id, err_msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert_eq!(stream_id, 0);
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_DOES_NOT_EXIST);
            assert_eq!(e.reason.as_str(), "session-level track does not exist");
        }
        _ => panic!("DOES_NOT_EXIST の RequestError が期待される"),
    }
    // draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection):
    // Application 処理を行わず拒否する場合は REQUEST_ERROR を送って stream を FIN する
    assert!(fin, "拒否応答は FIN で送信されること");
    // 拒否は REQUEST_ERROR のみで、セッションは閉じない
    assert_eq!(server.state(), SessionState::Established);
    assert_no_tracked_requests(&server);
}

/// `.session` を先頭フィールドに持つ複数フィールド名前空間の TRACK_STATUS も拒否される
///
/// draft-ietf-moq-transport-21 §6.5 の予約は先頭フィールドが `.session` の場合に成立する。
/// 先頭フィールド一致で判定していることを、複数フィールドの名前空間で確認する。
#[test]
fn track_status_session_namespace_multi_field_rejected() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message::TrackStatus as WireTrackStatus;
    let (_, mut server) = establish_pair();
    server
        .recv_request(ControlMessage::TrackStatus(WireTrackStatus {
            request_id: 0,
            track_namespace: ns(&[b".session", b"ext"]),
            track_name: b"extension".to_vec(),
            parameters: MessageParameters::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(server.track_status_request(0).is_none());
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

/// TRACK_STATUS_OK 送信後に bidi stream の送信方向が FIN されること
/// (draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS))
#[test]
fn track_status_ok_is_sent_with_fin() {
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
    let (_, _, fin) = take_send_on_stream_with_fin(&mut server);
    assert!(fin, "TRACK_STATUS_OK は FIN で送信されること");
}

/// 対象 Track の publisher 役 subscription に観測 largest がある場合、
/// TRACK_STATUS_OK に LARGEST_OBJECT が自動注入されること
/// (draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter))
#[test]
fn track_status_ok_injects_publisher_track_largest_object() {
    use shiguredo_moqt::stream::subgroup::{SubgroupHeader, SubgroupIdMode};
    let (mut client, mut server) = establish_pair();
    // 既存 subscription を確立し、object {5, 9} を公開して publisher 側の観測 largest を作る
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(sub_rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let stream_id = DataStreamId(10);
    server
        .send_subgroup_header(
            stream_id,
            sub_rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: 5,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 9, None)
        .expect("テストフィクスチャの前提条件を満たす");

    // 同じ Track へ別 request で TRACK_STATUS を送る
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
    let (_, ok_msg, _) = take_send_on_stream_with_fin(&mut server);
    match ok_msg {
        ControlMessage::RequestOk(ok) => {
            assert_eq!(
                ok.parameters.largest_object(),
                Some((5, 9)),
                "TRACK_STATUS_OK に publisher 観測 largest が注入されること"
            );
        }
        _ => panic!("REQUEST_OK が期待される"),
    }
}

/// 観測 largest が無い Track では TRACK_STATUS_OK に LARGEST_OBJECT を自動注入しないこと
#[test]
fn track_status_ok_does_not_inject_without_publisher_track() {
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
    let (_, ok_msg, _) = take_send_on_stream_with_fin(&mut server);
    match ok_msg {
        ControlMessage::RequestOk(ok) => {
            assert_eq!(
                ok.parameters.largest_object(),
                None,
                "未公開 Track では LARGEST_OBJECT を自動注入しないこと"
            );
        }
        _ => panic!("REQUEST_OK が期待される"),
    }
}

/// アプリが指定した LARGEST_OBJECT は自動注入で下げられないこと
#[test]
fn track_status_ok_app_largest_object_not_lowered() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
    };
    use shiguredo_moqt::stream::subgroup::{SubgroupHeader, SubgroupIdMode};
    let (mut client, mut server) = establish_pair();
    // 既存 subscription を確立し、object {5, 9} を公開して publisher 側の観測 largest を作る
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(sub_rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let stream_id = DataStreamId(10);
    server
        .send_subgroup_header(
            stream_id,
            sub_rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: 5,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 9, None)
        .expect("テストフィクスチャの前提条件を満たす");

    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ts_msg) = take_send_request(&mut client);
    server
        .recv_request(ts_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 観測 largest {5, 9} より大きい {100, 0} をアプリが指定する
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 100,
            object: 0,
        },
    });
    server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg, _) = take_send_on_stream_with_fin(&mut server);
    match ok_msg {
        ControlMessage::RequestOk(ok) => {
            assert_eq!(
                ok.parameters.largest_object(),
                Some((100, 0)),
                "アプリ指定値が自動注入で下げられないこと"
            );
        }
        _ => panic!("REQUEST_OK が期待される"),
    }
}

/// アプリ指定の LARGEST_OBJECT が観測値より小さい場合、観測値まで引き上げられ、
/// ローカル状態の largest_location も wire 値と一致すること
#[test]
fn track_status_ok_raises_app_largest_object_to_observed() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
    };
    use shiguredo_moqt::session::types::TrackStatusResponse;
    use shiguredo_moqt::stream::subgroup::{SubgroupHeader, SubgroupIdMode};
    let (mut client, mut server) = establish_pair();
    // 既存 subscription を確立し、object {5, 9} を公開する
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(sub_rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let stream_id = DataStreamId(10);
    server
        .send_subgroup_header(
            stream_id,
            sub_rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: 5,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 9, None)
        .expect("テストフィクスチャの前提条件を満たす");

    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ts_msg) = take_send_request(&mut client);
    server
        .recv_request(ts_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 観測 largest {5, 9} より小さい {1, 0} をアプリが指定する
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 1,
            object: 0,
        },
    });
    server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg, _) = take_send_on_stream_with_fin(&mut server);
    match ok_msg {
        ControlMessage::RequestOk(ok) => {
            assert_eq!(
                ok.parameters.largest_object(),
                Some((5, 9)),
                "アプリ指定値は観測値まで引き上げられること"
            );
        }
        _ => panic!("REQUEST_OK が期待される"),
    }
    assert_eq!(
        server
            .track_status_request(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .response,
        Some(TrackStatusResponse::Ok {
            largest_location: Some(Location {
                group_id: 5,
                object_id: 9,
            }),
        }),
        "ローカル状態の largest_location が wire 値と一致すること"
    );
}

/// TRACK_STATUS 以外の context (SUBSCRIBE_OK) は FIN せずに送信されること
#[test]
fn subscribe_ok_is_sent_without_fin() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, _, fin) = take_send_on_stream_with_fin(&mut server);
    assert!(!fin, "SUBSCRIBE_OK は FIN せずに送信されること");
}

/// 同一 Track に publisher 役 subscription が複数ある場合、最大の largest が使われること
#[test]
fn track_status_ok_injects_max_largest_across_publisher_subscriptions() {
    use shiguredo_moqt::stream::subgroup::{SubgroupHeader, SubgroupIdMode};
    let (mut client, mut server) = establish_pair();
    // 1 本目の subscription を確立し object {1, 0} を公開する
    let sub1 = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, m1) = take_send_request(&mut client);
    server
        .recv_request(m1)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(sub1, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok1) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub1, ok1)
        .expect("テストフィクスチャの前提条件を満たす");
    let s1 = DataStreamId(10);
    server
        .send_subgroup_header(
            s1,
            sub1,
            &SubgroupHeader {
                track_alias: 1,
                group_id: 1,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(s1, 0, None)
        .expect("テストフィクスチャの前提条件を満たす");

    // 2 本目の subscription を確立し object {5, 9} を公開する
    let sub2 = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, m2) = take_send_request(&mut client);
    server
        .recv_request(m2)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(sub2, 2, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok2) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub2, ok2)
        .expect("テストフィクスチャの前提条件を満たす");
    let s2 = DataStreamId(20);
    server
        .send_subgroup_header(
            s2,
            sub2,
            &SubgroupHeader {
                track_alias: 2,
                group_id: 5,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(s2, 9, None)
        .expect("テストフィクスチャの前提条件を満たす");

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
    let (_, ok_msg, _) = take_send_on_stream_with_fin(&mut server);
    match ok_msg {
        ControlMessage::RequestOk(ok) => {
            assert_eq!(
                ok.parameters.largest_object(),
                Some((5, 9)),
                "複数 subscription の最大値が使われること"
            );
        }
        _ => panic!("REQUEST_OK が期待される"),
    }
}

/// 観測 largest が無い Track ではアプリ指定の LARGEST_OBJECT がそのまま残ること
#[test]
fn track_status_ok_keeps_app_largest_object_without_publisher_track() {
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
            group: 7,
            object: 3,
        },
    });
    server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg, _) = take_send_on_stream_with_fin(&mut server);
    match ok_msg {
        ControlMessage::RequestOk(ok) => {
            assert_eq!(
                ok.parameters.largest_object(),
                Some((7, 3)),
                "アプリ指定値が改変されないこと"
            );
        }
        _ => panic!("REQUEST_OK が期待される"),
    }
}

/// 予約名前空間の拒否は値域外パラメータの MUST close より優先される (TRACK_STATUS)
///
/// draft-ietf-moq-transport-21 は予約名前空間拒否 (§2.4.2 / §6.5) と値域 MUST (§9.20.22) の
/// 優先順位を規定しない。宛先自体が存在しない予約名前空間の拒否を優先する意図した選択である。
#[test]
fn track_status_reserved_namespace_precedes_parameter_range() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message::TrackStatus;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_INCLUDE_PROPERTIES,
    };
    for reserved in [ns(&[b"."]), ns(&[b".session"])] {
        let (_, mut server) = establish_pair();
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_INCLUDE_PROPERTIES,
            value: MessageParameterValue::Uint8(2), // 値域外 (0 / 1 以外)
        });
        server
            .recv_request(ControlMessage::TrackStatus(TrackStatus {
                request_id: 0,
                track_namespace: reserved,
                track_name: b"cam".to_vec(),
                parameters: params,
            }))
            .expect("拒否は Ok で返る");
        assert_eq!(
            server.state(),
            SessionState::Established,
            "予約名前空間の拒否でセッションを閉じないこと"
        );
        assert!(server.track_status_request(0).is_none());
        let (_, err_msg, fin) = take_send_on_stream_with_fin(&mut server);
        assert!(fin, "拒否応答には FIN が付くこと");
        match err_msg {
            ControlMessage::RequestError(e) => assert_eq!(e.error_code, REQUEST_DOES_NOT_EXIST),
            _ => panic!("DOES_NOT_EXIST の RequestError が期待される"),
        }
    }
}
