//! FETCH データストリームの FIN / RESET_STREAM 後に FETCH_OK / REQUEST_ERROR / REQUEST_OK が
//! 届くケースのテスト
//!
//! draft-ietf-moq-transport-21 §9.11 (FETCH) は応答の到着タイミングを
//! オブジェクト配信に対して制約しないため、データストリームの終端後に
//! FETCH_OK / REQUEST_ERROR / REQUEST_OK が届いても (二重応答でなければ) セッションを
//! fail させず、終端情報の保存やイベント発行だけを行う (unknown mandatory によるキャンセル
//! 受理時は終端情報を保存せずイベント発行のみ)。FIN 通知は
//! `recv_fetch_data_stream_closed` の直接呼び出しで表現する (`recv_data_stream_closed` 経由だと
//! `Terminated` 済み fetch の終端は dispatch ガードで無視されるため)。

use super::*;

/// FETCH を送信して subscriber 側を Pending 状態にする
///
/// 返り値は (subscriber, publisher, request_id)。
fn send_fetch_pending() -> (Session, Session, u64) {
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
    (client, server, rid)
}

/// subscriber 側の FETCH を Established にし、REQUEST_UPDATE を 1 回送った状態にする
///
/// 返り値は (subscriber, publisher, request_id)。
fn establish_fetch_and_send_request_update() -> (Session, Session, u64) {
    use shiguredo_moqt::message::common::Location;
    let (mut client, mut server, rid) = send_fetch_pending();
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
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server, rid)
}

/// データストリームの FIN 後に FETCH_OK が届いても fail せず、state が `Terminated` のまま
/// 終端情報が保存され、応答受領フラグが立つこと
///
/// draft-ietf-moq-transport-21 §9.11 (FETCH): "The FETCH_OK or REQUEST_ERROR can
/// come at any time relative to object delivery." FETCH データストリーム (uni) と
/// FETCH_OK (bidi request stream) は別々の QUIC ストリームに流れるため、どちらの順序も
/// 起こりうる。
#[test]
fn fetch_ok_after_fin_keeps_terminated_and_saves_end_info() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::session::types::FetchState;
    let (mut client, mut server, rid) = send_fetch_pending();
    client
        .recv_fetch_data_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Terminated
    );
    server
        .send_fetch_ok(
            rid,
            1,
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
        .expect("FIN 後に FETCH_OK を受信しても fail しないこと");
    let fetch = client
        .fetch(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fetch.state,
        FetchState::Terminated,
        "FIN 後の FETCH_OK でも state は Terminated のまま維持されること"
    );
    assert_eq!(
        fetch.end_location,
        Some(Location {
            group_id: 5,
            object_id: 0,
        }),
        "FIN 後の FETCH_OK で終端情報が保存されること"
    );
    assert!(
        fetch.end_of_track,
        "FIN 後の FETCH_OK で end_of_track フラグが保存されること"
    );
    assert!(
        fetch.response_received,
        "FIN 後の FETCH_OK で応答受領フラグが立つこと"
    );
    // FIN 後の到着でも能動通知されること
    let mut saw_ok = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::FetchOkReceived {
            request_id,
            end_location,
            end_of_track,
        } = ev
        {
            assert_eq!(request_id, rid);
            assert_eq!(
                end_location,
                Location {
                    group_id: 5,
                    object_id: 0,
                },
                "FIN 後の FETCH_OK でも終端情報が通知されること"
            );
            assert!(
                end_of_track,
                "FIN 後の FETCH_OK でも end_of_track が通知されること"
            );
            saw_ok = true;
        }
    }
    assert!(
        saw_ok,
        "FIN 後の FETCH_OK で FetchOkReceived が発行されること"
    );
}

/// データストリームの FIN 前に FETCH_OK が届くと `FetchOkReceived` が push されること
///
/// draft-ietf-moq-transport-21 §9.12 (FETCH_OK): 通常フロー (FIN 前到着) でも
/// 終端情報つきの能動通知が行われ、アプリはポーリングなしに再生終了を確定できる。
#[test]
fn fetch_ok_before_fin_emits_fetch_ok_received() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::session::types::FetchState;
    let (mut client, mut server, rid) = send_fetch_pending();
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
        .expect("FIN 前に FETCH_OK を受信できること");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Established
    );
    let mut saw_ok = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::FetchOkReceived {
            request_id,
            end_location,
            end_of_track,
        } = ev
        {
            assert_eq!(request_id, rid);
            assert_eq!(
                end_location,
                Location {
                    group_id: 5,
                    object_id: 0,
                },
                "FIN 前の FETCH_OK でも終端情報が通知されること"
            );
            assert!(!end_of_track, "end_of_track = 0 がそのまま通知されること");
            saw_ok = true;
        }
    }
    assert!(
        saw_ok,
        "FIN 前の FETCH_OK で FetchOkReceived が発行されること"
    );
}

/// データストリームの FIN 後に REQUEST_ERROR が届いても fail せず、応答受領フラグが立ち、
/// `RequestErrorReceived` が push されること
///
/// draft §3.2.1 (Fetch State Management): "A REQUEST_ERROR indicates that both endpoints can
/// immediately remove state." データストリームの終端とは独立に bidi request stream で
/// 送られるため、FIN 後到着も正規フローである。
#[test]
fn request_error_after_fin_accepted() {
    use shiguredo_moqt::message::ReasonPhrase;
    let (mut client, mut server, rid) = send_fetch_pending();
    client
        .recv_fetch_data_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_error(
            rid,
            0x42,
            0,
            ReasonPhrase::new("fetch failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, err_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, err_msg)
        .expect("FIN 後に REQUEST_ERROR を受信しても fail しないこと");
    let fetch = client
        .fetch(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fetch.state,
        FetchState::Terminated,
        "REQUEST_ERROR 受理後も state は Terminated のまま維持されること"
    );
    assert!(
        fetch.response_received,
        "REQUEST_ERROR 受理で応答受領フラグが立つこと"
    );
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestErrorReceived {
            request_id,
            error_code,
            ..
        } = ev
            && request_id == rid
        {
            assert_eq!(error_code, 0x42);
            got = true;
            break;
        }
    }
    assert!(
        got,
        "FIN 後の REQUEST_ERROR 受理で RequestErrorReceived が push されること"
    );
}

/// 二重応答: FIN → REQUEST_ERROR → FETCH_OK で ProtocolViolation になりセッションが fail すること
///
/// draft §3.2.1 (Fetch State Management): "The publisher MUST send exactly one FETCH_OK or
/// REQUEST_ERROR in response to a FETCH."
#[test]
fn fetch_ok_after_request_error_after_fin_fails_session() {
    use shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION;
    use shiguredo_moqt::message::{FetchOk as WireFetchOk, ReasonPhrase, common::Location};
    let (mut client, mut server, rid) = send_fetch_pending();
    client
        .recv_fetch_data_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_error(
            rid,
            0x42,
            0,
            ReasonPhrase::new("fetch failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, err_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, err_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // REQUEST_ERROR 受理後の FETCH_OK は二重応答として ProtocolViolation
    // (server 側の `send_fetch_ok` は Pending 状態を要求するため、ワイヤメッセージを直接注入する)
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::FetchOk(WireFetchOk {
                end_of_track: 0,
                end_location: Location {
                    group_id: 5,
                    object_id: 0,
                },
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect_err("二重応答の FETCH_OK は ProtocolViolation で fail すること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// 二重応答: FIN → FETCH_OK → FETCH_OK で ProtocolViolation になりセッションが fail すること
///
/// draft §3.2.1 (Fetch State Management): "The publisher MUST send exactly one FETCH_OK or
/// REQUEST_ERROR in response to a FETCH."
#[test]
fn fetch_ok_twice_after_fin_fails_session() {
    use shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION;
    use shiguredo_moqt::message::{FetchOk as WireFetchOk, common::Location};
    let (mut client, _server, rid) = send_fetch_pending();
    client
        .recv_fetch_data_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    // server 側の `send_fetch_ok` は Pending 状態を要求するため、ワイヤメッセージを直接注入する
    let fetch_ok = |end: Location| {
        ControlMessage::FetchOk(WireFetchOk {
            end_of_track: 0,
            end_location: end,
            parameters: MessageParameters::new(),
            track_properties: TrackProperties::new(),
        })
    };
    client
        .recv_stream_message(
            rid,
            fetch_ok(Location {
                group_id: 5,
                object_id: 0,
            }),
        )
        .expect("FIN 後の 1 回目の FETCH_OK は受理されること");
    let err = client
        .recv_stream_message(
            rid,
            fetch_ok(Location {
                group_id: 5,
                object_id: 0,
            }),
        )
        .expect_err("2 回目の FETCH_OK は二重応答として ProtocolViolation で fail すること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// 二重応答: FIN 前 (Established) の FETCH_OK → FETCH_OK でも ProtocolViolation になり
/// セッションが fail すること
///
/// データストリームの FIN 前後で応答受領フラグによる二重応答検出は同一経路である。
/// FIN 前に FETCH_OK を受信して `Established` に遷移した後は `response_received` が
/// true のため、2 回目の FETCH_OK はフラグ判定で fail する
/// (draft §3.2.1 (Fetch State Management): "The publisher MUST send exactly one FETCH_OK or
/// REQUEST_ERROR in response to a FETCH.")。
#[test]
fn fetch_ok_twice_before_fin_fails_session() {
    use shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION;
    use shiguredo_moqt::message::{FetchOk as WireFetchOk, common::Location};
    use shiguredo_moqt::session::types::FetchState;
    let (mut client, mut server, rid) = send_fetch_pending();
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
        .expect("FIN 前の 1 回目の FETCH_OK は受理されること");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Established
    );
    // server 側の `send_fetch_ok` は Pending 状態を要求するため、ワイヤメッセージを直接注入する
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::FetchOk(WireFetchOk {
                end_of_track: 0,
                end_location: Location {
                    group_id: 5,
                    object_id: 0,
                },
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect_err("Established での 2 回目の FETCH_OK は ProtocolViolation で fail すること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// FIN → FETCH_OK → REQUEST_ERROR の順で届いても fail しないこと
///
/// REQUEST_ERROR は REQUEST_UPDATE 失敗応答と区別不能であり、state ベースで判定すると
/// 正当な順序 (FIN → FETCH_OK → REQUEST_UPDATE 失敗応答) を誤って fail させるため、
/// REQUEST_ERROR は state によらず受理する。REQUEST_UPDATE を送っていない本テストの
/// REQUEST_ERROR は draft §3.2.1 の "exactly one" に反する二重応答だが、受信側にセッション
/// クローズを要求する MUST は無く、かつ REQUEST_UPDATE 送信履歴を持たない実装では
/// 区別不能のため許容する設計である (REQUEST_UPDATE 送信済みの正当ケースは
/// `request_error_for_failed_update_after_fin_accepted` が検証する)。
#[test]
fn request_error_after_fetch_ok_after_fin_accepted() {
    use shiguredo_moqt::message::{ReasonPhrase, common::Location};
    let (mut client, mut server, rid) = send_fetch_pending();
    client
        .recv_fetch_data_stream_closed(rid, RequestStreamEnd::Fin)
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
        .expect("FIN 後の FETCH_OK は受理されること");
    server
        .send_request_error(
            rid,
            0x42,
            0,
            ReasonPhrase::new("update failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, err_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, err_msg)
        .expect("FETCH_OK 受理後の REQUEST_ERROR は fail させないこと");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Terminated,
        "REQUEST_ERROR 受理後も state は Terminated のまま維持されること"
    );
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestErrorReceived {
            request_id,
            error_code,
            ..
        } = ev
            && request_id == rid
        {
            assert_eq!(error_code, 0x42);
            got = true;
            break;
        }
    }
    assert!(
        got,
        "FIN 後の REQUEST_ERROR 受理で RequestErrorReceived が push されること"
    );
}

/// Pending 状態 (FIN 前) での REQUEST_ERROR 受理でも fail せず、state が `Terminated` に
/// 遷移し応答受領フラグが立つこと
///
/// draft §3.2.1 (Fetch State Management): "A REQUEST_ERROR indicates that both endpoints can
/// immediately remove state." publisher が FETCH を拒否する一般的な経路
/// (INVALID_RANGE / DOES_NOT_EXIST 等) では subscriber は Pending のまま REQUEST_ERROR を
/// 受けるため、この遷移が正規フローとして成立する。
#[test]
fn request_error_in_pending_transitions_to_terminated() {
    use shiguredo_moqt::message::ReasonPhrase;
    let (mut client, mut server, rid) = send_fetch_pending();
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Pending
    );
    server
        .send_request_error(
            rid,
            0x42,
            0,
            ReasonPhrase::new("fetch failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, err_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, err_msg)
        .expect("Pending 状態の REQUEST_ERROR 受理で fail しないこと");
    let fetch = client
        .fetch(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fetch.state,
        FetchState::Terminated,
        "Pending での REQUEST_ERROR 受理で Terminated に遷移すること"
    );
    assert!(
        fetch.response_received,
        "Pending での REQUEST_ERROR 受理で応答受領フラグが立つこと"
    );
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestErrorReceived {
            request_id,
            error_code,
            ..
        } = ev
            && request_id == rid
        {
            assert_eq!(error_code, 0x42);
            got = true;
            break;
        }
    }
    assert!(
        got,
        "Pending での REQUEST_ERROR 受理で RequestErrorReceived が push されること"
    );
}

/// RESET_STREAM による `Terminated` 後も FETCH_OK を受理し、state が `Terminated` のまま
/// 終端情報が保存されること
#[test]
fn fetch_ok_after_reset_stream_accepted() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::session::types::FetchState;
    let (mut client, mut server, rid) = send_fetch_pending();
    client
        .recv_fetch_data_stream_closed(
            rid,
            RequestStreamEnd::Reset {
                error_code: 0,
                reliable_size: None,
            },
        )
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
        .expect("RESET_STREAM 後の FETCH_OK を受信しても fail しないこと");
    let fetch = client
        .fetch(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fetch.state,
        FetchState::Terminated,
        "RESET_STREAM 後の FETCH_OK でも state は Terminated のまま維持されること"
    );
    assert_eq!(
        fetch.end_location,
        Some(Location {
            group_id: 5,
            object_id: 0,
        }),
        "RESET_STREAM 後の FETCH_OK で終端情報が保存されること"
    );
    assert!(
        fetch.response_received,
        "RESET_STREAM 後の FETCH_OK で応答受領フラグが立つこと"
    );
}

/// `Terminated` 状態での FETCH_OK でも End Location < Start Location は ProtocolViolation で
/// fail すること (draft-ietf-moq-transport-21 §9.12 (FETCH_OK))
#[test]
fn fetch_ok_end_before_start_after_fin_fails_session() {
    use shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION;
    use shiguredo_moqt::message::{FetchOk as WireFetchOk, common::Location};
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
    client
        .recv_fetch_data_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
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
                track_properties: TrackProperties::new(),
            }),
        )
        .expect_err(
            "End Location < Start Location の FETCH_OK は ProtocolViolation で fail すること",
        );
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// `Terminated` 状態で unknown mandatory を含む FETCH_OK が届いた場合、`RequestTerminated` が
/// push され応答受領フラグが立つこと
///
/// draft §3.6 (Mandatory Track Properties): 未知の必須プロパティを含む FETCH_OK は fetch を
/// キャンセルする。FIN 後の `Terminated` でも一律スキップせず、アプリが unknown mandatory
/// によるキャンセルを検知できるようにする。
#[test]
fn unknown_mandatory_fetch_ok_after_fin_terminates_fetch() {
    use shiguredo_moqt::message::{FetchOk as WireFetchOk, common::Location};
    use shiguredo_moqt::session::types::FetchState;
    use shiguredo_moqt::track_properties::{
        MANDATORY_TRACK_PROPERTY_MIN, TrackProperty, TrackPropertyValue,
    };
    let (mut client, _server, rid) = send_fetch_pending();
    client
        .recv_fetch_data_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut track_properties = TrackProperties::new();
    track_properties.push(TrackProperty {
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
                track_properties,
            }),
        )
        .expect("unknown mandatory を含む FETCH_OK の受信自体は fail しないこと");
    let fetch = client
        .fetch(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fetch.state,
        FetchState::Terminated,
        "unknown mandatory によるキャンセルで state が Terminated になること"
    );
    assert!(
        fetch.response_received,
        "unknown mandatory 受理でも応答受領フラグが立つこと"
    );
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind,
            reason,
        } = ev
            && request_id == rid
        {
            assert_eq!(kind, RequestKind::Fetch);
            assert_eq!(reason, TerminationReason::LocalCancel);
            got = true;
            break;
        }
    }
    assert!(
        got,
        "unknown mandatory を含む FETCH_OK で RequestTerminated が push されること"
    );
}

/// REQUEST_UPDATE 失敗応答の REQUEST_ERROR がデータストリームの FIN 後 (`Terminated`) に
/// 届いても fail しないこと
///
/// draft-ietf-moq-transport-21 §9.11 (FETCH): FETCH_OK / REQUEST_ERROR は
/// オブジェクト配信に対して任意のタイミングで届く。REQUEST_UPDATE 失敗応答も
/// bidi request stream で送られるため、データストリームの終端と独立に到着しうる。
#[test]
fn request_error_for_failed_update_after_fin_accepted() {
    use shiguredo_moqt::message::ReasonPhrase;
    let (mut client, mut server, rid) = establish_fetch_and_send_request_update();
    client
        .recv_fetch_data_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Terminated
    );
    server
        .send_request_error(
            rid,
            0x42,
            0,
            ReasonPhrase::new("update failed".to_string())
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, err_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, err_msg)
        .expect("REQUEST_UPDATE 失敗応答の REQUEST_ERROR が FIN 後に届いても fail しないこと");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Terminated,
        "REQUEST_ERROR 受理後も state は Terminated のまま維持されること"
    );
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestErrorReceived {
            request_id,
            error_code,
            ..
        } = ev
            && request_id == rid
        {
            assert_eq!(error_code, 0x42);
            got = true;
            break;
        }
    }
    assert!(
        got,
        "FIN 後の REQUEST_ERROR 受理で RequestErrorReceived が push されること"
    );
}

/// FETCH_OK 受信 → REQUEST_UPDATE 送信 → データストリーム FIN → REQUEST_OK 受信の順序で
/// fail せず、`RequestOkReceived` が push されること
///
/// REQUEST_OK は REQUEST_UPDATE への応答 (REQUEST_UPDATE_OK) のみであり、データストリームの
/// 終端とは独立に bidi request stream で送られる (REQUEST_OK の FIN 後到着は bidi stream が
/// データストリームと独立であることからの推論。draft-ietf-moq-transport-21 §9.11
/// (FETCH) の "can come at any time" は FETCH_OK / REQUEST_ERROR のタイミングのみを
/// 述べたものである)。REQUEST_UPDATE は複数回送れるため二重応答フラグの対象外とする。
#[test]
fn request_ok_after_fin_accepted() {
    let (mut client, mut server, rid) = establish_fetch_and_send_request_update();
    client
        .recv_fetch_data_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Terminated
    );
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("FIN 後に REQUEST_OK を受信しても fail しないこと");
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::RequestOkReceived {
            request_id,
            request_kind,
            ..
        } = ev
            && request_id == rid
        {
            assert_eq!(request_kind, RequestKind::Fetch);
            got = true;
            break;
        }
    }
    assert!(
        got,
        "FIN 後の REQUEST_OK 受理で RequestOkReceived が push されること"
    );
}

/// 空の FETCH 応答 (FETCH_HEADER + FIN、エントリ 0 個) の後の FETCH_OK で fail せず、
/// 終端情報が保存されること
///
/// draft-ietf-moq-transport-21 §9.11 (FETCH): "If no Objects exist in the
/// requested range, the publisher opens the unidirectional stream, sends the FETCH_HEADER
/// (see Section 11.4.4) and closes the stream with a FIN."
#[test]
fn empty_fetch_response_then_fetch_ok_accepted() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::session::types::FetchState;
    let (mut client, mut server, rid) = send_fetch_pending();
    let stream_id = DataStreamId(130);
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_fetch_header(stream_id, &FetchHeader { request_id: rid })
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_fetch_data_stream_closed(rid, RequestStreamEnd::Fin)
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
        .expect("空 FETCH 応答後の FETCH_OK を受信しても fail しないこと");
    let fetch = client
        .fetch(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fetch.state,
        FetchState::Terminated,
        "空 FETCH 応答後の FETCH_OK でも state は Terminated のまま維持されること"
    );
    assert_eq!(
        fetch.end_location,
        Some(Location {
            group_id: 5,
            object_id: 0,
        }),
        "空 FETCH 応答後の FETCH_OK で終端情報が保存されること"
    );
    assert!(
        fetch.response_received,
        "空 FETCH 応答後の FETCH_OK で応答受領フラグが立つこと"
    );
}

/// `Pending` 状態での REQUEST_OK は従来どおり ProtocolViolation で fail すること
///
/// REQUEST_UPDATE は実装が `Established` でのみ送るため、`Pending` での REQUEST_OK は
/// peer の違反である。
#[test]
fn request_ok_in_pending_fails_session() {
    use shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION;
    use shiguredo_moqt::message::RequestOk as WireRequestOk;
    let (mut client, _server, rid) = send_fetch_pending();
    // server 側の `send_request_ok` は Established 状態を要求するため、ワイヤメッセージを直接注入する
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(WireRequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::default(),
            }),
        )
        .expect_err("Pending 状態の REQUEST_OK は ProtocolViolation で fail すること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}
