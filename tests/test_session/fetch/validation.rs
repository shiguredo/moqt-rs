//! FETCH のバリデーションテスト
//!
//! 名前空間検証 (.session / single period `.`)、FETCH_OK の未知必須プロパティチェック、
//! 必須プロパティ非含有時の非回帰、GROUP_ORDER 値域検証を扱う。

use super::*;

/// .session 名前空間の空トラック名に対する FETCH は DOES_NOT_EXIST で拒否される
/// (draft-ietf-moq-transport-21 §6.5 (Session-Level Tracks and Namespaces))
#[test]
fn fetch_session_namespace_empty_track_rejected() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message::Fetch as WireFetch;
    let (_, mut server) = establish_pair();
    server
        .recv_request(ControlMessage::Fetch(WireFetch {
            request_id: 0,
            track_namespace: ns(&[b".session"]),
            track_name: vec![],
            parameters: MessageParameters::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(server.fetch(0).is_none());
    let (_, err_msg) = take_send_on_stream(&mut server);
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_DOES_NOT_EXIST);
        }
        _ => panic!("DOES_NOT_EXIST の RequestError が期待される"),
    }
}

/// single period `.` 予約名前空間の FETCH は track_name 非空でも DOES_NOT_EXIST で拒否される
/// (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces))
#[test]
fn fetch_single_period_namespace_rejected() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message::Fetch as WireFetch;
    let (_, mut server) = establish_pair();
    server
        .recv_request(ControlMessage::Fetch(WireFetch {
            request_id: 0,
            track_namespace: ns(&[b"."]),
            track_name: b"cam".to_vec(),
            parameters: MessageParameters::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(server.fetch(0).is_none());
    let (_, err_msg) = take_send_on_stream(&mut server);
    match err_msg {
        ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_DOES_NOT_EXIST);
        }
        _ => panic!("DOES_NOT_EXIST の RequestError が期待される"),
    }
}

/// REQUEST_ERROR で拒否した FETCH のストリームクローズは no-op で吸収され、
/// セッションが fail しないこと
///
/// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection):
/// "When an endpoint rejects a request without performing any application processing,
/// it SHOULD send a REQUEST_ERROR and FIN the stream." FETCH 検証拒否
/// (ここでは single-period 名前空間) も `request_streams` 未登録のまま拒否するため、
/// peer のストリームクローズを `rejected_request_ids` で no-op 吸収する。
#[test]
fn rejected_fetch_stream_close_does_not_fail_session() {
    use shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST;
    use shiguredo_moqt::message::Fetch as WireFetch;
    let (_, mut server) = establish_pair();
    // 2 つの request id で拒否し、FIN と RESET_STREAM の両方のクローズを検証する
    // (id は Client 役の偶数採番 (0, 2) を使う。奇数は Server 役のローカル採番域で、
    // peer が奇数 id を送ると parity 不正で fail するため)
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
            .recv_request(ControlMessage::Fetch(WireFetch {
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
        assert!(server.fetch(rid).is_none());
    }
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
    let err = server
        .recv_request_stream_closed(0, RequestStreamEnd::Fin)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// send_fetch に single period `.` 予約名前空間を渡すと SESSION_PROTOCOL_VIOLATION が返る
/// (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces))
#[test]
fn send_fetch_single_period_namespace_rejected() {
    use shiguredo_moqt::message::common::Location;
    let (mut client, _server) = establish_pair();
    let err = client
        .send_fetch(
            ns(&[b"."]),
            b"cam".to_vec(),
            fetch_range_params(
                Location {
                    group_id: 0,
                    object_id: 0,
                },
                Location {
                    group_id: 1,
                    object_id: 0,
                },
            ),
        )
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
}

/// FETCH への FETCH_OK で未知の必須 track property を受信すると
/// fetch が Terminated に遷移し RequestTerminated イベントが発火する
/// (draft-ietf-moq-transport-21 §3.6 (Mandatory Track Properties))
#[test]
fn fetch_ok_with_unknown_mandatory_property_cancels_fetch() {
    use shiguredo_moqt::message::{FetchOk as WireFetchOk, common::Location};
    use shiguredo_moqt::track_properties::{
        MANDATORY_TRACK_PROPERTY_MIN, TrackProperty, TrackPropertyValue,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fetch_range_params(
                Location {
                    group_id: 0,
                    object_id: 0,
                },
                Location {
                    group_id: 5,
                    object_id: 0,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 未知の必須プロパティを含む FETCH_OK を直接構築して subscriber に渡す
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: MANDATORY_TRACK_PROPERTY_MIN,
        value: TrackPropertyValue::VarInt(1),
    });
    client
        .recv_stream_message(
            rid,
            ControlMessage::FetchOk(WireFetchOk {
                end_of_track: 0,
                end_location: Location {
                    group_id: 5,
                    object_id: 0,
                },
                parameters: MessageParameters::new(),
                track_properties: tp,
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    // fetch は Terminated に遷移する
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::FetchState::Terminated
    );
    // RequestTerminated イベントが Fetch / LocalCancel で発火する
    let mut found = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind,
            reason,
        } = e
        {
            assert_eq!(request_id, rid);
            assert_eq!(kind, RequestKind::Fetch);
            assert_eq!(reason, TerminationReason::LocalCancel);
            found = true;
        }
    }
    assert!(
        found,
        "FETCH_OK の未知必須プロパティで RequestTerminated イベントが期待される"
    );
}

/// 必須範囲外の track property のみを含む FETCH_OK は通常どおり Established に遷移する
/// (draft-ietf-moq-transport-21 §3.6 (Mandatory Track Properties) の非回帰)
#[test]
fn fetch_ok_with_non_mandatory_property_establishes_fetch() {
    use shiguredo_moqt::message::{FetchOk as WireFetchOk, common::Location};
    use shiguredo_moqt::track_properties::{TrackProperty, TrackPropertyValue};
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fetch_range_params(
                Location {
                    group_id: 0,
                    object_id: 0,
                },
                Location {
                    group_id: 5,
                    object_id: 0,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 必須範囲外 (0x1000) のプロパティはキャンセルを引き起こさない
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: 0x1000,
        value: TrackPropertyValue::VarInt(42),
    });
    client
        .recv_stream_message(
            rid,
            ControlMessage::FetchOk(WireFetchOk {
                end_of_track: 0,
                end_location: Location {
                    group_id: 5,
                    object_id: 0,
                },
                parameters: MessageParameters::new(),
                track_properties: tp,
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::FetchState::Established
    );
}

/// track property なしの FETCH_OK は通常どおり Established に遷移する
/// (draft-ietf-moq-transport-21 §3.6 (Mandatory Track Properties) の非回帰)
#[test]
fn fetch_ok_without_track_properties_establishes_fetch() {
    use shiguredo_moqt::message::common::Location;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fetch_range_params(
                Location {
                    group_id: 0,
                    object_id: 0,
                },
                Location {
                    group_id: 5,
                    object_id: 0,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_fetch_ok(
            rid,
            0,
            Location {
                group_id: 5,
                object_id: 0,
            },
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::FetchState::Established
    );
}

/// End Location < Start Location かつ未知の必須プロパティを含む FETCH_OK は
/// mandatory チェックより先に End Location 検証が評価され SESSION_PROTOCOL_VIOLATION になる
/// (draft-ietf-moq-transport-21 §9.12 (FETCH_OK) / §3.6 (Mandatory Track Properties) の評価順序)
#[test]
fn fetch_ok_end_before_start_with_unknown_mandatory_closes_session() {
    use shiguredo_moqt::message::{FetchOk as WireFetchOk, common::Location};
    use shiguredo_moqt::track_properties::{
        MANDATORY_TRACK_PROPERTY_MIN, TrackProperty, TrackPropertyValue,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fetch_range_params(
                Location {
                    group_id: 3,
                    object_id: 0,
                },
                Location {
                    group_id: 10,
                    object_id: 0,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // End {2, 0} < Start {3, 0} かつ未知の必須プロパティを含む FETCH_OK
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: MANDATORY_TRACK_PROPERTY_MIN,
        value: TrackPropertyValue::VarInt(1),
    });
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::FetchOk(WireFetchOk {
                end_of_track: 0,
                end_location: Location {
                    group_id: 2,
                    object_id: 0,
                },
                parameters: MessageParameters::new(),
                track_properties: tp,
            }),
        )
        .unwrap_err();
    // End Location 検証が先に評価されセッションレベルのエラーになる
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

// ─── GROUP_ORDER 値域検証 (draft §9.20.9) ──────────────────────────────────

/// GROUP_ORDER 値域外の FETCH 受信は PROTOCOL_VIOLATION でセッションを閉じる
///
/// draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): 値域外の値は
/// "MUST close the session with PROTOCOL_VIOLATION"。ワイヤデコード層は値域外を
/// 弾くため、セッション層に到達するのはアプリが MessageParameters を直接構築して
/// 渡した場合のみ。
#[test]
fn fetch_with_invalid_group_order_closes_session() {
    use shiguredo_moqt::message::Fetch as WireFetch;
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
        .recv_request(ControlMessage::Fetch(WireFetch {
            request_id: 0,
            track_namespace: ns(&[b"live"]),
            track_name: b"cam".to_vec(),
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

/// 値域外 GROUP_ORDER を含む send_fetch はエラーを返し、セッションは閉じない
///
/// draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): MUST クローズは
/// 受信側の義務であり、送信側の検証はエラーを返すだけ。検証失敗後も正常な送信が
/// 継続できること (状態非汚染) を確認する。
#[test]
fn send_fetch_with_invalid_group_order_returns_error_without_closing() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_GROUP_ORDER,
    };
    let (mut client, _server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(3), // 値域外 (Ascending=0x1 / Descending=0x2)
    });
    params.push(MessageParameter {
        param_type: shiguredo_moqt::message_parameter::PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            shiguredo_moqt::message_parameter::LocationFilter::AbsoluteRangeWithEnd {
                start: Location {
                    group_id: 0,
                    object_id: 0,
                },
                end_group_delta: 1,
                end_object: 0,
            }
            .encode_to_bytes(),
        ),
    });
    let err = client
        .send_fetch(ns(&[b"live"]), b"cam".to_vec(), params)
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    // セッションは閉じない (CloseSession イベントが発行されない)
    while let Some(e) = client.poll_event() {
        assert!(
            !matches!(e, SessionEvent::CloseSession(_)),
            "送信側の検証エラーでセッションは閉じないこと"
        );
    }
    // 状態非汚染: 検証エラー後も正常な FETCH が送信できる
    let rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fetch_range_params(
                Location {
                    group_id: 0,
                    object_id: 0,
                },
                Location {
                    group_id: 1,
                    object_id: 0,
                },
            ),
        )
        .expect("検証エラー後も正常な送信ができること");
    let (sent_rid, _) = take_send_request(&mut client);
    assert_eq!(sent_rid, rid);
}
