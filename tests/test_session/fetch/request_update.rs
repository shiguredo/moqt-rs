//! FETCH の REQUEST_UPDATE / REQUEST_ERROR テスト
//!
//! FETCH 確立後の REQUEST_UPDATE → REQUEST_OK サイクル、
//! REQUEST_ERROR 受信時の状態維持、RequestOkReceived イベント発火を扱う。

use super::*;

/// FETCH 確立後は REQUEST_UPDATE → REQUEST_OK をやり取りできる
#[test]
fn fetch_request_update_full_cycle() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_SUBSCRIBER_PRIORITY,
    };
    use shiguredo_moqt::session::types::FetchState;
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

    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_SUBSCRIBER_PRIORITY,
        value: MessageParameterValue::Uint8(7),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let mut got_update = false;
    while let Some(ev) = server.poll_event() {
        if let SessionEvent::RequestUpdateReceived { request_id, .. } = ev
            && request_id == rid
        {
            got_update = true;
            break;
        }
    }
    assert!(
        got_update,
        "FETCH の RequestUpdateReceived イベントが期待された"
    );

    server
        .send_request_ok(
            rid,
            MessageParameters::new(),
            shiguredo_moqt::track_properties::TrackProperties::default(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, reqok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, reqok)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Established
    );
    assert_eq!(
        server
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Established
    );
}

/// FETCH 確立後の REQUEST_ERROR は update 失敗応答として扱い、両側の fetch を
/// `Terminated` に遷移させる
///
/// draft-ietf-moq-transport-21 §3.2.1: "A REQUEST_ERROR indicates that both endpoints can
/// immediately remove state." および "A subscriber keeps FETCH state until it cancels
/// the request (see Section 3.3.3), receives REQUEST_ERROR, or the FETCH data stream
/// receives a FIN or is reset." REQUEST_UPDATE 失敗応答の REQUEST_ERROR も「REQUEST_ERROR
/// を受信した」ことに変わりはない。§9.5.1 の MUST でデータストリームはリセット済みの
/// ため、`Established` のまま維持すると再配信不能な状態と状態機械が矛盾する。
#[test]
fn fetch_request_error_in_established_transitions_to_terminated() {
    use shiguredo_moqt::message::{ReasonPhrase, common::Location};
    use shiguredo_moqt::session::types::FetchState;
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

    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // publisher 側の REQUEST_UPDATE 失敗応答送信で Terminated に遷移する
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
    assert_eq!(
        server
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Terminated,
        "REQUEST_UPDATE 失敗応答の送信で publisher 側が Terminated に遷移すること"
    );
    let (_, err_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, err_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Terminated,
        "REQUEST_UPDATE 失敗応答の受信で subscriber 側が Terminated に遷移すること"
    );
    // Terminated 遷移後の再 REQUEST_UPDATE はエラーになる (Established 要求)
    let err = client
        .send_request_update(rid, MessageParameters::new())
        .expect_err("Terminated 遷移後の REQUEST_UPDATE はエラーになること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // 両側とも forget_fetch が可能になる (fetch_finished が Terminated で true)
    let f = server
        .forget_fetch(rid)
        .expect("Terminated な fetch は破棄できること");
    assert_eq!(f.request_id, rid);
    assert!(
        client.forget_fetch(rid).is_some(),
        "Terminated な fetch は subscriber 側でも破棄できること"
    );
}

/// FETCH 確立後に REQUEST_UPDATE_OK を受信すると RequestOkReceived(request_kind=Fetch) が発火する
#[test]
fn fetch_request_update_ok_emits_request_ok_received_event_with_kind_fetch() {
    use shiguredo_moqt::message::common::Location;
    // FETCH → FETCH_OK で確立し、その後に REQUEST_UPDATE → REQUEST_OK を受信する。
    // FETCH_OK は handle_ok_for_fetch を通らないため、最初のイベントは発火しない。
    // REQUEST_UPDATE_OK 受信時に RequestOkReceived(request_kind=Fetch) が発火することを検証する。
    let (mut client, mut server) = establish_pair();
    let start = Location {
        group_id: 0,
        object_id: 0,
    };
    let end = Location {
        group_id: 10,
        object_id: 0,
    };
    let rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fetch_range_params(start, end),
        )
        .expect("FETCH の送信に成功すること");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("FETCH の受信に成功すること");
    server
        .send_fetch_ok(
            rid,
            0,
            Location {
                group_id: 10,
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
        client.fetch(rid).expect("fetch が存在すること").state,
        FetchState::Established
    );

    // REQUEST_UPDATE → REQUEST_OK サイクル
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

    // FETCH の REQUEST_UPDATE_OK 受信時に RequestOkReceived(request_kind=Fetch) が発火する
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
        "FETCH の REQUEST_UPDATE_OK 受信時に RequestOkReceived(request_kind=Fetch) が発火すること"
    );
}

/// REQUEST_UPDATE 失敗応答で fetch が `Terminated` に遷移し、
/// draft-ietf-moq-transport-21 §9.5.1 の "When a REQUEST_UPDATE fails for a FETCH, the
/// publisher MUST reset the FETCH data stream." (MUST) が Session から自動発火されること。
///
/// ワイヤ順序は REQUEST_ERROR (bidi) → RESET_STREAM (uni data) で、SessionEvent の
/// 発行順序でも同じになる (subscription 側の PUBLISH_DONE 遅延 push と対称)。
/// error code は §12.5 (Stream Reset Error Codes) の `CANCELLED` (0x1)。
/// 自動 reset により outgoing_fetch から stream エントリが除去され、
/// `data_stream_finished` が立つため終端通知なしでも `forget_fetch` が可能になる (draft §3.2.1)。
#[test]
fn request_error_update_failure_transitions_to_terminated_and_auto_resets_data_stream() {
    use shiguredo_moqt::error::STREAM_CANCELLED;
    use shiguredo_moqt::message::{ReasonPhrase, common::Location};
    use shiguredo_moqt::session::types::FetchState;
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
    let stream_id = DataStreamId(150);
    server
        .send_fetch_header(stream_id, rid)
        .expect("FETCH_HEADER の送信に成功すること");
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // REQUEST_UPDATE を REQUEST_ERROR で拒否する (Terminated に遷移する)
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
    assert_eq!(
        server
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Terminated,
        "REQUEST_UPDATE 失敗応答では fetch が Terminated に遷移すること"
    );

    // Session から REQUEST_ERROR → ResetDataStream の順に発行されること (ワイヤ順序保持)
    // REQUEST_ERROR 単独が最終のため FIN 付き (§6.4.2.3)
    let mut err_rid = None;
    let mut err_fin = None;
    let mut err_msg = None;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::SendOnStream {
            request_id,
            message,
            fin,
        } = e
        {
            err_rid = Some(request_id);
            err_fin = Some(fin);
            err_msg = Some(message);
            break;
        }
    }
    assert_eq!(err_rid, Some(rid));
    assert_eq!(err_fin, Some(true), "最終応答には FIN が付くこと");
    assert!(
        matches!(err_msg, Some(ControlMessage::RequestError(_))),
        "最初は REQUEST_ERROR であること"
    );
    let err_msg = err_msg.expect("REQUEST_ERROR が発行されること");
    let mut saw_reset = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::ResetDataStream {
            stream_id: sid,
            error_code,
            reliable_size,
        } = e
        {
            assert_eq!(sid, stream_id);
            assert_eq!(error_code, STREAM_CANCELLED, "draft §12.5: CANCELLED (0x1)");
            assert_eq!(
                reliable_size, None,
                "自動発火では RESET_STREAM (reliable_size なし) になること"
            );
            saw_reset = true;
            break;
        }
    }
    assert!(
        saw_reset,
        "Session から ResetDataStream イベントが自動発行されること (draft §9.5.1 MUST)"
    );

    // client (subscriber) に REQUEST_ERROR を配送
    client
        .recv_stream_message(rid, err_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // 自動 reset により outgoing FETCH stream は既に掃除されている
    // (掃除済み stream の終端通知は unknown stream id で fail する)
    let err = server
        .send_fetch_data_stream_closed(stream_id)
        .expect_err("自動 reset で除去済みの stream は終端通知できないこと");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // Terminated + data_stream_finished により終端通知なしでも破棄可能 (draft §3.2.1)
    assert!(
        server.fetch_cleanup_ready(rid) == Some(true),
        "自動 reset 後の fetch は終端通知なしでも破棄可能になること"
    );
    let f = server
        .forget_fetch(rid)
        .expect("終端通知なしでも破棄できること");
    assert_eq!(f.request_id, rid);
}

/// `Terminated` 遷移後の fetch への REQUEST_UPDATE 受信が `handle_update_for_fetch` で
/// 拒否され、セッションが PROTOCOL_VIOLATION で閉じること
///
/// draft-ietf-moq-transport-21 §9.5: "The sender of a request (SUBSCRIBE, PUBLISH, FETCH,
/// ...) can later send a REQUEST_UPDATE on the same bidi stream as the request to modify
/// it." の two cases の 1 つ目に FETCH が含まれるため、`Terminated` の fetch への再
/// REQUEST_UPDATE は MUST close の対象外であり、非 `Established` での拒否は仕様が規定
/// しないケースへの実装判断 (self.fail によるセッションクローズ)。
#[test]
fn terminated_fetch_rejects_late_request_update() {
    use shiguredo_moqt::message::{ReasonPhrase, common::Location};
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
    // REQUEST_UPDATE → REQUEST_ERROR で Terminated に遷移させる
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
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
        .expect("テストフィクスチャの前提条件を満たす");
    // Terminated の fetch にワイヤ REQUEST_UPDATE を直接注入する (handle_update_for_fetch
    // が Established 要求で拒否し、self.fail でセッションを閉じる)
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
                request_id: rid,
                parameters: MessageParameters::new(),
            }),
        )
        .expect_err("Terminated の fetch への REQUEST_UPDATE は拒否されること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    match drain_until_close(&mut client) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("CloseSession(PROTOCOL_VIOLATION) が期待されたが {other:?}"),
    }
}

/// subscriber 側で REQUEST_ERROR (REQUEST_UPDATE 失敗応答) 受信後に届くデータストリームの
/// RESET が、`recv_data_stream_closed` の dispatch ガード (fetch が `Terminated` なら無視)
/// により吸収されること
///
/// draft-ietf-moq-transport-21 §9.5.1: "When a REQUEST_UPDATE fails for a FETCH, the
/// publisher MUST reset the FETCH data stream." 失敗応答の受信で fetch は `Terminated` に
/// 遷移済みのため、後から届く RESET は `recv_data_stream_closed` の Fetch 分岐の dispatch
/// ガード (`src/session/data.rs`。`Terminated` なら `Ok(())` で無視) により吸収される。
#[test]
fn terminated_fetch_absorbs_reset_via_recv_data_stream_closed() {
    use shiguredo_moqt::message::{ReasonPhrase, common::Location};
    use shiguredo_moqt::session::types::FetchState;
    use shiguredo_moqt::stream::fetch::FetchHeader;
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
    // subscriber (client) 側に FETCH データストリームを配送する
    // (IncomingDataStream::Fetch エントリを作る。RESET 吸収の検証に必要)
    let stream_id = DataStreamId(131);
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_fetch_header(stream_id, &FetchHeader { request_id: rid })
        .expect("テストフィクスチャの前提条件を満たす");
    // REQUEST_UPDATE → REQUEST_ERROR で Terminated に遷移させる
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
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
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Terminated,
        "REQUEST_UPDATE 失敗応答の受信で subscriber 側が Terminated に遷移すること"
    );
    // 後から届く RESET は recv_data_stream_closed 経由で吸収される
    client
        .recv_data_stream_closed(
            stream_id,
            shiguredo_moqt::session::types::RequestStreamEnd::Reset {
                error_code: 0x0d,
                reliable_size: None,
            },
        )
        .expect("Terminated の fetch への RESET は吸収されること");
    // セッションが fail していないこと
    assert_eq!(client.state(), SessionState::Established);
}
