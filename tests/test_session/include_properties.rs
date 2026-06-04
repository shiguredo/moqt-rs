//! INCLUDE_PROPERTIES パラメータのテスト
//!
//! draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
//! Parameter Type 0x35 (uint8) は SUBSCRIBE / TRACK_STATUS / FETCH /
//! SUBSCRIBE_TRACKS に出現可能。0 を指定すると OK 応答または resulting PUBLISH の
//! Track Properties は存在するが空になる (SHOULD)。許容値は 0 / 1 (default 1) であり、
//! 範囲外受信時は MUST close the session with PROTOCOL_VIOLATION。
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use super::*;
use shiguredo_moqt::message_parameter::PARAM_INCLUDE_PROPERTIES;

/// INCLUDE_PROPERTIES=0 付きのパラメータ群を構築する
fn include_properties_params(value: u8) -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_INCLUDE_PROPERTIES,
        value: MessageParameterValue::Uint8(value),
    });
    params
}

/// 非空の Track Properties を構築する (空化の検証用)
fn non_empty_track_properties() -> TrackProperties {
    track_properties_with_dynamic_groups()
}

/// SUBSCRIBE + INCLUDE_PROPERTIES=0 → SUBSCRIBE_OK の Track Properties が空になる
///
/// draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter)
#[test]
fn subscribe_include_properties_zero_empties_subscribe_ok() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            include_properties_params(0),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // publisher 側は要求値を保持する
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .include_properties,
        Some(0)
    );
    server
        .send_subscribe_ok(
            rid,
            5,
            MessageParameters::new(),
            non_empty_track_properties(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    match ok_msg {
        ControlMessage::SubscribeOk(ref ok) => {
            assert!(
                ok.track_properties.is_empty(),
                "INCLUDE_PROPERTIES=0 では SUBSCRIBE_OK の Track Properties が空になること"
            );
        }
        other => panic!("SUBSCRIBE_OK が期待されたが {other:?} を受け取った"),
    }
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
}

/// SUBSCRIBE の INCLUDE_PROPERTIES 省略時 (default 1) は Track Properties が含まれる
///
/// draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter)
#[test]
fn subscribe_include_properties_omitted_keeps_properties() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .include_properties,
        None
    );
    server
        .send_subscribe_ok(
            rid,
            5,
            MessageParameters::new(),
            non_empty_track_properties(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    match ok_msg {
        ControlMessage::SubscribeOk(ref ok) => {
            assert!(
                !ok.track_properties.is_empty(),
                "省略時 (default 1) は SUBSCRIBE_OK の Track Properties が含まれること"
            );
        }
        other => panic!("SUBSCRIBE_OK が期待されたが {other:?} を受け取った"),
    }
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
}

/// SUBSCRIBE の INCLUDE_PROPERTIES=1 (明示) は Track Properties が含まれる
///
/// draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
/// default 1 と明示 1 は同等に扱う。
#[test]
fn subscribe_include_properties_one_keeps_properties() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            include_properties_params(1),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            rid,
            5,
            MessageParameters::new(),
            non_empty_track_properties(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    match ok_msg {
        ControlMessage::SubscribeOk(ref ok) => {
            assert!(
                !ok.track_properties.is_empty(),
                "明示 1 では SUBSCRIBE_OK の Track Properties が含まれること"
            );
        }
        other => panic!("SUBSCRIBE_OK が期待されたが {other:?} を受け取った"),
    }
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
}

/// FETCH + INCLUDE_PROPERTIES=0 → FETCH_OK の Track Properties が空になる
///
/// draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter)
#[test]
fn fetch_include_properties_zero_empties_fetch_ok() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::message_parameter::LocationFilter;
    use shiguredo_moqt::message_parameter::PARAM_LOCATION_FILTER;
    let (mut client, mut server) = establish_pair();
    // range {0, 0} - {5, 0} を LOCATION_FILTER で指定する
    let mut params = include_properties_params(0);
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteRangeWithEnd {
                start: Location {
                    group_id: 0,
                    object_id: 0,
                },
                end_group_delta: 5,
                end_object: 0,
            }
            .encode_to_bytes(),
        ),
    });
    let rid = client
        .send_fetch(ns(&[b"live"]), b"cam".to_vec(), params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .include_properties,
        Some(0)
    );
    server
        .send_fetch_ok(
            rid,
            0,
            Location {
                group_id: 5,
                object_id: 0,
            },
            MessageParameters::new(),
            non_empty_track_properties(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    match ok_msg {
        ControlMessage::FetchOk(ref ok) => {
            assert!(
                ok.track_properties.is_empty(),
                "INCLUDE_PROPERTIES=0 では FETCH_OK の Track Properties が空になること"
            );
        }
        other => panic!("FETCH_OK が期待されたが {other:?} を受け取った"),
    }
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
}

/// TRACK_STATUS + INCLUDE_PROPERTIES=0 → TRACK_STATUS_OK の Track Properties が空になる
///
/// draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter)
#[test]
fn track_status_include_properties_zero_empties_ok() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(
            ns(&[b"live"]),
            b"cam".to_vec(),
            include_properties_params(0),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ts_msg) = take_send_request(&mut client);
    server
        .recv_request(ts_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .track_status_request(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .include_properties,
        Some(0)
    );
    server
        .send_request_ok(rid, MessageParameters::new(), non_empty_track_properties())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    match ok_msg {
        ControlMessage::RequestOk(ref ok) => {
            assert!(
                ok.track_properties.is_empty(),
                "INCLUDE_PROPERTIES=0 では TRACK_STATUS_OK の Track Properties が空になること"
            );
        }
        other => panic!("REQUEST_OK が期待されたが {other:?} を受け取った"),
    }
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
}

/// SUBSCRIBE_TRACKS + INCLUDE_PROPERTIES=0 → publisher 側に値が保持される
///
/// draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
/// resulting PUBLISH の Track Properties 空化は application 層が
/// `track_subscription()` で保持値を参照して行う (SUBSCRIBE_TRACKS_OK は対象外)。
#[test]
fn subscribe_tracks_include_properties_zero_is_stored() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_tracks(ns(&[b"example"]), include_properties_params(0))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, st_msg) = take_send_request(&mut client);
    server
        .recv_request(st_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .track_subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .include_properties,
        Some(0),
        "publisher 側は SUBSCRIBE_TRACKS の INCLUDE_PROPERTIES=0 を保持すること"
    );
    // SUBSCRIBE_TRACKS_OK は空化の対象外であり、非空 properties でも送れる
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
}

/// 値域外 INCLUDE_PROPERTIES の SUBSCRIBE 受信は PROTOCOL_VIOLATION でセッションを閉じる
///
/// draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
/// 値域外の値は "MUST close the session with PROTOCOL_VIOLATION"。
/// ワイヤデコード層は値域外を弾くため、セッション層に到達するのはアプリが
/// MessageParameters を直接構築して渡した場合のみ。
#[test]
fn subscribe_with_invalid_include_properties_closes_session() {
    let (_client, mut server) = establish_pair();
    let err = server
        .recv_request(ControlMessage::Subscribe(
            shiguredo_moqt::message::Subscribe {
                track_namespace: ns(&[b"live"]),
                track_name: b"cam".to_vec(),
                request_id: 0,
                parameters: include_properties_params(2),
            },
        ))
        .expect_err("値域外 INCLUDE_PROPERTIES は PROTOCOL_VIOLATION になる");
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

/// 値域外 INCLUDE_PROPERTIES を含む send_subscribe はエラーを返し、セッションは閉じない
///
/// draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
/// MUST クローズは受信側の義務であり、送信側の検証はエラーを返すだけ。
/// 検証失敗後も正常な送信が継続できること (状態非汚染) を確認する。
#[test]
fn send_subscribe_with_invalid_include_properties_returns_error_without_closing() {
    let (mut client, _server) = establish_pair();
    let err = client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            include_properties_params(2),
        )
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
    // 状態非汚染: 検証エラー後も正常な SUBSCRIBE が送信できる
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("検証エラー後も正常な送信ができること");
    let (sent_rid, _) = take_send_request(&mut client);
    assert_eq!(sent_rid, rid);
}

/// 値域外 INCLUDE_PROPERTIES の FETCH / TRACK_STATUS / SUBSCRIBE_TRACKS 受信は
/// PROTOCOL_VIOLATION でセッションを閉じる
///
/// draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
/// 4 受信経路は独立配線のため、SUBSCRIBE 代表に加えて残り 3 種も検証する。
#[test]
fn invalid_include_properties_closes_session_for_fetch_track_status_subscribe_tracks() {
    use shiguredo_moqt::message::{Fetch, SubscribeTracks, TrackStatus};
    use shiguredo_moqt::session::types::SessionState;
    let cases: Vec<(&str, ControlMessage)> = vec![
        (
            "FETCH",
            ControlMessage::Fetch(Fetch {
                request_id: 0,
                track_namespace: ns(&[b"live"]),
                track_name: b"cam".to_vec(),
                parameters: include_properties_params(2),
            }),
        ),
        (
            "TRACK_STATUS",
            ControlMessage::TrackStatus(TrackStatus {
                request_id: 0,
                track_namespace: ns(&[b"live"]),
                track_name: b"cam".to_vec(),
                parameters: include_properties_params(2),
            }),
        ),
        (
            "SUBSCRIBE_TRACKS",
            ControlMessage::SubscribeTracks(SubscribeTracks {
                request_id: 0,
                track_namespace_prefix: ns(&[b"example"]),
                parameters: include_properties_params(2),
            }),
        ),
    ];
    for (label, msg) in cases {
        let (_client, mut server) = establish_pair();
        let err = server.recv_request(msg).expect_err(&format!(
            "{label} の値域外 INCLUDE_PROPERTIES は PROTOCOL_VIOLATION になる"
        ));
        assert_eq!(
            err.as_session_error()
                .expect("Session エラーであること")
                .code,
            SESSION_PROTOCOL_VIOLATION,
            "{label} のエラーコードが PROTOCOL_VIOLATION であること"
        );
        assert_eq!(
            server.state(),
            SessionState::Closing,
            "{label} の受信後にセッションが閉じること"
        );
        match drain_until_close(&mut server) {
            SessionEvent::CloseSession(e) => assert_eq!(
                e.code, SESSION_PROTOCOL_VIOLATION,
                "{label} の CloseSession コードが PROTOCOL_VIOLATION であること"
            ),
            other => panic!("CloseSession(PROTOCOL_VIOLATION) が期待されたが {other:?}"),
        }
    }
}
