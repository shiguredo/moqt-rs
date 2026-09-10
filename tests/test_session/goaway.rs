use super::*;
use shiguredo_moqt::error::REQUEST_GOING_AWAY;
use shiguredo_moqt::message::Goaway;
use shiguredo_moqt::message_parameter::MessageParameters;
use shiguredo_moqt::session::types::RecvRequestError;
use shiguredo_moqt::{
    session::types::RequestStreamEnd, session::types::SendRequestError,
    session::types::SessionError, session::types::SessionEvent, session::types::SessionState,
};

/// server 起点の GOAWAY を送信して client に配送する (drain 前提作り)
///
/// timeout / URI はテストごとに意味を持つため引数で受け取る。
fn deliver_goaway_from_server(
    server: &mut Session,
    client: &mut Session,
    uri: Vec<u8>,
    timeout: u64,
) {
    server
        .send_goaway(uri, timeout)
        .expect("テストフィクスチャの前提条件を満たす");
    let go_msg = take_send_control(server);
    client
        .recv_control(go_msg)
        .expect("テストフィクスチャの前提条件を満たす");
}

/// Server → Client の request stream GOAWAY 送受信
#[test]
fn goaway_on_request_stream_server_to_client() {
    let (mut client, mut server) = establish_pair();
    // client (subscriber) が SUBSCRIBE を送る
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (sent_rid, sub_msg) = take_send_request(&mut client);
    assert_eq!(sent_rid, sub_rid);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // server → client に SUBSCRIBE_OK
    server
        .send_subscribe_ok(
            sub_rid,
            1,
            MessageParameters::new(),
            TrackProperties::default(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // server が request stream 上で GOAWAY を送信
    server
        .send_goaway_on_request_stream(sub_rid, b"moqt://relay.example/".to_vec(), 10000)
        .expect("テストフィクスチャの前提条件を満たす");
    let (go_rid, go_msg) = take_send_on_stream(&mut server);
    assert_eq!(go_rid, sub_rid);
    match &go_msg {
        ControlMessage::Goaway(g) => {
            assert_eq!(g.new_session_uri, b"moqt://relay.example/");
            assert_eq!(g.timeout, 10000);
        }
        _ => panic!("Goaway イベントが期待された"),
    }
    // client が request stream 上で GOAWAY を受信
    client
        .recv_stream_message(sub_rid, go_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // client 側で GoawayReceived イベントが発行される
    let mut received = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::GoawayReceived {
            new_session_uri,
            timeout,
            on_request_stream,
        } = e
        {
            assert_eq!(new_session_uri, b"moqt://relay.example/");
            assert_eq!(timeout, 10000);
            assert_eq!(on_request_stream, Some(sub_rid));
            received = true;
            break;
        }
    }
    assert!(received, "GoawayReceived イベントが期待された");
}

/// Client → Server の空 URI request stream GOAWAY 送受信
#[test]
fn goaway_on_request_stream_client_to_server_zero_uri() {
    let (mut client, mut server) = establish_pair();
    // server (publisher) が PUBLISH を送る
    let pub_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (sent_rid, pub_msg) = take_send_request(&mut server);
    assert_eq!(sent_rid, pub_rid);
    client
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // client → server に REQUEST_OK (旧 PUBLISH_OK)
    client
        .send_request_ok(pub_rid, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(pub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // client が request stream 上で GOAWAY を送信 (URI 空)
    client
        .send_goaway_on_request_stream(pub_rid, Vec::new(), 5000)
        .expect("テストフィクスチャの前提条件を満たす");
    let (go_rid, go_msg) = take_send_on_stream(&mut client);
    assert_eq!(go_rid, pub_rid);
    match &go_msg {
        ControlMessage::Goaway(g) => {
            assert!(g.new_session_uri.is_empty());
            assert_eq!(g.timeout, 5000);
        }
        _ => panic!("Goaway イベントが期待された"),
    }
    // server が受信
    server
        .recv_stream_message(pub_rid, go_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut received = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::GoawayReceived {
            new_session_uri,
            timeout,
            on_request_stream,
            ..
        } = e
        {
            assert!(new_session_uri.is_empty());
            assert_eq!(timeout, 5000);
            assert_eq!(on_request_stream, Some(pub_rid));
            received = true;
            break;
        }
    }
    assert!(received, "GoawayReceived イベントが期待された");
}

/// 同一 request stream への二重 GOAWAY 送信は拒否される
#[test]
fn goaway_on_request_stream_duplicate_send_rejected() {
    let (mut client, mut server) = establish_pair();
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            sub_rid,
            1,
            MessageParameters::new(),
            TrackProperties::default(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 1 回目: 成功
    server
        .send_goaway_on_request_stream(sub_rid, Vec::new(), 0)
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_send_on_stream(&mut server);
    // 2 回目: 拒否
    let err = server
        .send_goaway_on_request_stream(sub_rid, Vec::new(), 0)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// 同一 request stream への二重 GOAWAY 受信は session を閉じる
#[test]
fn goaway_on_request_stream_duplicate_receive_closes_session() {
    let (mut client, mut server) = establish_pair();
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            sub_rid,
            1,
            MessageParameters::new(),
            TrackProperties::default(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 1 回目: 成功
    server
        .send_goaway_on_request_stream(sub_rid, Vec::new(), 0)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, go_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, go_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 2 回目: PROTOCOL_VIOLATION で session が閉じられる
    let err = client
        .recv_stream_message(
            sub_rid,
            ControlMessage::Goaway(Goaway {
                new_session_uri: Vec::new(),
                timeout: 0,
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}

/// control stream GOAWAY は `on_request_stream: None`
#[test]
fn goaway_on_control_stream_has_none_on_request_stream() {
    let (mut client, mut server) = establish_pair();
    server
        .send_goaway(b"moqt://relay.example/".to_vec(), 10000)
        .expect("テストフィクスチャの前提条件を満たす");
    let go_msg = loop {
        match server.poll_event() {
            Some(SessionEvent::SendControl(m)) => break m,
            Some(_) => continue,
            None => panic!("SendControl イベントが期待された"),
        }
    };
    client
        .recv_control(go_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut received = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::GoawayReceived {
            on_request_stream, ..
        } = e
        {
            assert_eq!(on_request_stream, None);
            received = true;
            break;
        }
    }
    assert!(received, "GoawayReceived イベントが期待された");
}

/// Client が non-zero URI の request stream GOAWAY を送信しようとすると拒否される
#[test]
fn client_send_request_stream_goaway_with_non_zero_uri_rejected() {
    let (mut client, mut server) = establish_pair();
    // server (publisher) が PUBLISH を送る
    let pub_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut server);
    client
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .send_request_ok(pub_rid, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(pub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // client が non-zero URI で GOAWAY 送信 → 拒否
    let err = client
        .send_goaway_on_request_stream(pub_rid, b"moqt://example.com/".to_vec(), 5000)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// 確立前のリクエストストリームに対する GOAWAY 受信は session を閉じる
#[test]
fn goaway_on_request_stream_before_established_closes_session() {
    let mut client = Session::new_client(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_send_control(&mut client);
    // SETUP 未完了の状態で GOAWAY を受信
    let err = client
        .recv_stream_message(
            0,
            ControlMessage::Goaway(Goaway {
                new_session_uri: Vec::new(),
                timeout: 0,
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}

/// goaway_drain_snapshot が namespace_subscription / namespace_publication / track_status を
/// drain blocker に含めること
#[test]
fn goaway_drain_snapshot_includes_all_request_types() {
    let (mut client, mut server) = establish_pair();
    let ns_rid = client
        .send_subscribe_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ns_msg) = take_send_request(&mut client);
    server
        .recv_request(ns_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let pn_rid = client
        .send_publish_namespace(ns(&[b"pub"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pn_msg) = take_send_request(&mut client);
    server
        .recv_request(pn_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let ts_rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ts_msg) = take_send_request(&mut client);
    server
        .recv_request(ts_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let snapshot = server.goaway_drain_snapshot();
    assert!(
        snapshot
            .blocking_namespace_subscription_request_ids
            .contains(&ns_rid)
    );
    assert!(
        snapshot
            .blocking_namespace_publication_request_ids
            .contains(&pn_rid)
    );
    assert!(snapshot.blocking_track_status_request_ids.contains(&ts_rid));
    assert!(!snapshot.ready());
}

/// Server が control stream で非空 new_session_uri の GOAWAY を受信したら PROTOCOL_VIOLATION
///
/// draft-ietf-moq-transport-21 §9.2 (GOAWAY): Server (受信側) は Client (peer) からの
/// non-zero new_session_uri を拒否する。送信側検証
/// (`client_send_request_stream_goaway_with_non_zero_uri_rejected`) は正常な Client が
/// 送れないことの確認に留まり、Server 受信側の分岐 (`handle_peer_goaway`) は未カバーだった。
#[test]
fn server_recv_control_goaway_with_non_zero_uri_rejected() {
    let (_client, mut server) = establish_pair();
    let err = server
        .recv_control(ControlMessage::Goaway(Goaway {
            new_session_uri: b"moqt://relay.example/".to_vec(),
            timeout: 10000,
        }))
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// Server が request stream で非空 new_session_uri の GOAWAY を受信したら PROTOCOL_VIOLATION
///
/// draft-ietf-moq-transport-21 §9.2 (GOAWAY): request stream 上の GOAWAY でも Server (受信側) は Client からの
/// non-zero new_session_uri を拒否する (`handle_peer_goaway_on_request_stream`)。control stream 版と対をなす。
#[test]
fn server_recv_request_stream_goaway_with_non_zero_uri_rejected() {
    let (_client, mut server, rid) = establish_subscribe_track(1);
    let err = server
        .recv_stream_message(
            rid,
            ControlMessage::Goaway(Goaway {
                new_session_uri: b"moqt://relay.example/".to_vec(),
                timeout: 5000,
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

// ─── GOAWAY 送信後の到着後拒否 (GOING_AWAY) ────────────────────────────────

/// L が control GOAWAY を送信した後、P からの新規 SUBSCRIBE は GOING_AWAY で拒否される
///
/// draft-ietf-moq-transport-21 §9.2 (GOAWAY): control GOAWAY 送信側は GOAWAY 後に到着する新規 request を
/// GOING_AWAY で MAY 拒否。
/// 注: peer GOAWAY 受信後は自側の send_subscribe も抑制されるため、
/// ここでは recv_request に直接メッセージを渡して server 側の拒否動作を検証する。
#[test]
fn local_goaway_rejects_peer_new_subscribe() {
    let (mut client, mut server) = establish_pair();
    // server (L) が control GOAWAY を送信
    deliver_goaway_from_server(&mut server, &mut client, Vec::new(), 10000);

    // peer (client) が L に新規 SUBSCRIBE を送信 (send_subscribe は抑制されるため直接 recv_request に渡す)
    let rid = 0u64; // Client 役の最初の request_id (偶数)
    server
        .recv_request(ControlMessage::Subscribe(
            shiguredo_moqt::message::Subscribe {
                track_namespace: ns(&[b"live"]),
                track_name: b"cam".to_vec(),
                request_id: rid,
                parameters: MessageParameters::new(),
            },
        ))
        .expect("テストフィクスチャの前提条件を満たす");

    // L は GOING_AWAY で拒否
    let (rejected_rid, reject_msg) = take_send_on_stream(&mut server);
    assert_eq!(rejected_rid, rid);
    if let ControlMessage::RequestError(e) = &reject_msg {
        assert_eq!(
            e.error_code, REQUEST_GOING_AWAY,
            "GOING_AWAY (0x6) で拒否される"
        );
    } else {
        panic!("REQUEST_ERROR が期待されたが {reject_msg:?} を受け取った");
    }
    assert_eq!(
        server.state(),
        SessionState::Established,
        "L は Established を維持"
    );
}

/// GOAWAY 送信後に拒否した request のストリームクローズはセッションを fail させない
///
/// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection): 拒否する側は
/// REQUEST_ERROR を送信して自側の送信方向を FIN で閉じる (SHOULD)。bidi request stream は
/// 各方向が独立に閉じるため (draft §6.4.2.2)、peer も自分の送信方向を FIN / RESET_STREAM で
/// 閉じる。拒否済み request id のストリームクローズは no-op で吸収され、セッションは fail しない。
///
/// 注: 本テストは GOAWAY を受信した peer が新規 request を送るシナリオを検証するが、
/// これは draft §9.2 の "SHOULD NOT initiate new requests to the peer" に反する
/// 非準拠動作のシミュレーションであり、拒否側 (server) の実装検証が目的。
/// また server が発行した REQUEST_ERROR は client に配送しない。fixture の client は
/// send_subscribe を通していないため request 未登録で、配送すると client 側の
/// handle_peer_request_error が unknown id で fail するため (実プロトコルでは
/// client は自側登録済みなので正常処理できる)。
#[test]
fn goaway_rejected_request_stream_close_does_not_fail_session() {
    let (mut client, mut server) = establish_pair();
    // server (L) が control GOAWAY を送信
    server
        .send_goaway(Vec::new(), 10000)
        .expect("テストフィクスチャの前提条件を満たす");
    let go_msg = take_send_control(&mut server);
    client
        .recv_control(go_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // peer (client) が L に新規 SUBSCRIBE を送信 (send_subscribe は抑制されるため直接 recv_request に渡す)
    // 2 つの request id で拒否し、FIN と RESET_STREAM の両方のクローズを検証する。
    // id は Client 役の偶数採番 (0, 2) を使う (奇数は Server 役のローカル採番域で、
    // peer が奇数 id を送ると parity 不正で fail するため)。
    let closes: [(u64, RequestStreamEnd); 2] = [
        (0, RequestStreamEnd::Fin),
        (
            2,
            RequestStreamEnd::Reset {
                error_code: 0, // RESET_STREAM のアプリケーションエラーコード (値は検証対象外)
                reliable_size: None,
            },
        ),
    ];
    for (rid, end) in closes {
        server
            .recv_request(ControlMessage::Subscribe(
                shiguredo_moqt::message::Subscribe {
                    track_namespace: ns(&[b"live"]),
                    track_name: b"cam".to_vec(),
                    request_id: rid,
                    parameters: MessageParameters::new(),
                },
            ))
            .expect("テストフィクスチャの前提条件を満たす");
        // L は GOING_AWAY で拒否 (draft §16.11.2 (REQUEST_ERROR Codes): GOING_AWAY = 0x6)
        let (rejected_rid, reject_msg) = take_send_on_stream(&mut server);
        assert_eq!(rejected_rid, rid);
        if let ControlMessage::RequestError(e) = &reject_msg {
            assert_eq!(
                e.error_code, REQUEST_GOING_AWAY,
                "GOING_AWAY (0x6) で拒否される"
            );
        } else {
            panic!("REQUEST_ERROR が期待されたが {reject_msg:?} を受け取った");
        }
        // peer が自分の送信方向を閉じる → no-op で吸収されセッションは生存
        server
            .recv_request_stream_closed(rid, end)
            .expect("拒否済み request id のストリームクローズは no-op であること");
        assert_eq!(
            server.state(),
            SessionState::Established,
            "拒否済み id のクローズで L は Established を維持"
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
}

/// 拒否済み request id の 2 回目のストリームクローズは PROTOCOL_VIOLATION になる
///
/// no-op 吸収は `rejected_request_ids` から削除することで成立する。クローズ通知が
/// 届くたびに削除されるため、2 回目の通知は unknown id として fail する。
/// 実プロトコル上 QUIC は同一ストリームへの二重 FIN / 二重 RESET_STREAM を
/// 保証しないため、本テストは内部ライフサイクル (クローズ時削除) の回帰防御を
/// 意図する。
#[test]
fn goaway_rejected_request_double_close_fails_as_unknown_id() {
    let (mut client, mut server) = establish_pair();
    deliver_goaway_from_server(&mut server, &mut client, Vec::new(), 10000);
    let rid = 0u64;
    server
        .recv_request(ControlMessage::Subscribe(
            shiguredo_moqt::message::Subscribe {
                track_namespace: ns(&[b"live"]),
                track_name: b"cam".to_vec(),
                request_id: rid,
                parameters: MessageParameters::new(),
            },
        ))
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_send_on_stream(&mut server);
    // 1 回目: no-op で吸収され集合から削除される
    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    // 2 回目: 集合から既に削除済みのため unknown id として fail
    let err = server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// GOAWAY 拒否済み request id の再送は INVALID_REQUEST_ID でセッションが閉じる
///
/// draft-ietf-moq-transport-21 §6.4.2.1 (Request ID): duplicate Request ID は
/// INVALID_REQUEST_ID でセッションを閉じる (MUST)。拒否済み id は
/// `accept_peer_request` で `peer_tracker.accept` 済みのため、再送は重複として検出される。
#[test]
fn goaway_rejected_request_id_resend_closes_with_invalid_request_id() {
    let (mut client, mut server) = establish_pair();
    deliver_goaway_from_server(&mut server, &mut client, Vec::new(), 10000);
    let rid = 0u64;
    server
        .recv_request(ControlMessage::Subscribe(
            shiguredo_moqt::message::Subscribe {
                track_namespace: ns(&[b"live"]),
                track_name: b"cam".to_vec(),
                request_id: rid,
                parameters: MessageParameters::new(),
            },
        ))
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_send_on_stream(&mut server);
    // 同じ id の再送は重複として INVALID_REQUEST_ID で fail
    let err = server
        .recv_request(ControlMessage::Subscribe(
            shiguredo_moqt::message::Subscribe {
                track_namespace: ns(&[b"live"]),
                track_name: b"cam".to_vec(),
                request_id: rid,
                parameters: MessageParameters::new(),
            },
        ))
        .unwrap_err();
    assert!(
        matches!(err, RecvRequestError::Session(ref e) if e.code == SESSION_INVALID_REQUEST_ID),
        "拒否済み id の再送は INVALID_REQUEST_ID で fail: got {err:?}"
    );
    assert_eq!(server.state(), SessionState::Closing);
}

/// P が GOAWAY を送信しただけでは L は新規 peer request を拒否しない
///
/// 現状バグでは L=Server 時に P の id=0 GOAWAY で誤拒否される。
/// 修正前 RED ・修正後 GREEN の回帰テスト。
#[test]
fn peer_goaway_does_not_trigger_local_going_away_reject() {
    let (mut client, mut server) = establish_pair();
    // peer (client) が control GOAWAY を送信
    client
        .send_goaway(Vec::new(), 10000)
        .expect("テストフィクスチャの前提条件を満たす");
    let go_msg = take_send_control(&mut client);
    server
        .recv_control(go_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // peer が L に新規 SUBSCRIBE を送信
    let _rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // L は REQUEST_ERROR を積まず、受理される
    while let Some(e) = server.poll_event() {
        if let SessionEvent::SendOnStream { message, .. } = &e
            && matches!(message, ControlMessage::RequestError(_))
        {
            panic!("peer GOAWAY だけでは GOING_AWAY 拒否されるべきではない: {e:?}");
        }
    }
    // recv_request の戻り値が Ok(true) であることは recv_request の戻り値型から確認できないため、
    // 少なくとも REQUEST_ERROR が発行されないことを検証する
}

/// local_sent が true でも parity 不正な request_id は INVALID_REQUEST_ID で Closing になる
///
/// draft §6.4.2.1 の parity/重複検証は §9.2 の GOING_AWAY 拒否より優先される。
#[test]
fn goaway_local_sent_parity_invalid_closes_with_invalid_request_id() {
    let (mut client, mut server) = establish_pair();
    // server (L) が control GOAWAY を送信
    server
        .send_goaway(Vec::new(), 10000)
        .expect("テストフィクスチャの前提条件を満たす");
    let go_msg = take_send_control(&mut server);
    client
        .recv_control(go_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // client の役割は Client → 採番は偶数 (0, 2, 4, ...)
    // server 側は peer=Client なので偶数を受け付ける
    // ここで client 役の peer が奇数の request_id を持つ SUBSCRIBE メッセージを
    // 直接 server に recv_request する
    let err = server
        .recv_request(ControlMessage::Subscribe(
            shiguredo_moqt::message::Subscribe {
                track_namespace: ns(&[b"live"]),
                track_name: b"cam".to_vec(),
                request_id: 3, // 奇数 = Client 役として不正な parity
                parameters: MessageParameters::new(),
            },
        ))
        .unwrap_err();
    assert!(
        matches!(err, RecvRequestError::Session(ref e) if e.code == SESSION_INVALID_REQUEST_ID),
        "parity 不正は INVALID_REQUEST_ID で Closing: got {err:?}"
    );
    assert_eq!(server.state(), SessionState::Closing);
}

/// send_goaway_on_request_stream は local_sent を立てないため peer の新規 request は拒否されない
#[test]
fn goaway_on_request_stream_does_not_reject_new_peer_requests() {
    let (mut client, mut server, sub_rid) = establish_subscribe_track(1);
    // server が request stream 上で GOAWAY を送信
    server
        .send_goaway_on_request_stream(sub_rid, Vec::new(), 10000)
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_send_on_stream(&mut server);

    // local_sent は立たないため、この後の peer 新規 request は拒否されない。
    // 新規 SUBSCRIBE を受理できることで検証する
    let new_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam2".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, new_msg) = take_send_request(&mut client);
    server
        .recv_request(new_msg)
        .expect("request stream 上の GOAWAY では新規 request は拒否されないこと");
    assert!(
        server.subscription(new_rid).is_some(),
        "新規 SUBSCRIBE が受理されること"
    );

    // この時点で peer からの新規 request は拒否されない
    // (accept_peer_request の local_sent 判定は false になるため)
}

// ─── peer GOAWAY 受信後の新規リクエスト送信抑制 (draft §9.2 SHOULD NOT) ────

/// peer GOAWAY 受信後に send_subscribe が PeerGoawayReceived を返す
#[test]
fn peer_goaway_suppresses_send_subscribe() {
    let (mut client, mut server) = establish_pair();
    // server (peer) が control GOAWAY を送信し、client が受信
    server
        .send_goaway(b"moqt://relay.example/".to_vec(), 10000)
        .expect("テストフィクスチャの前提条件を満たす");
    let go_msg = take_send_control(&mut server);
    client
        .recv_control(go_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // client 側で新規 SUBSCRIBE は抑制される
    let err = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .unwrap_err();
    assert_eq!(err, SendRequestError::PeerGoawayReceived);
}

/// peer GOAWAY 受信後に send_publish が PeerGoawayReceived を返す
#[test]
fn peer_goaway_suppresses_send_publish() {
    let (mut client, mut server) = establish_pair();
    deliver_goaway_from_server(
        &mut server,
        &mut client,
        b"moqt://relay.example/".to_vec(),
        10000,
    );
    let err = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            100,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .unwrap_err();
    assert_eq!(err, SendRequestError::PeerGoawayReceived);
}

/// peer GOAWAY 受信後に send_fetch が PeerGoawayReceived を返す
#[test]
fn peer_goaway_suppresses_send_fetch() {
    let (mut client, mut server) = establish_pair();
    deliver_goaway_from_server(
        &mut server,
        &mut client,
        b"moqt://relay.example/".to_vec(),
        10000,
    );
    let err = client
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
        .unwrap_err();
    assert_eq!(err, SendRequestError::PeerGoawayReceived);
}

/// peer GOAWAY 受信後に send_publish_namespace が PeerGoawayReceived を返す
#[test]
fn peer_goaway_suppresses_send_publish_namespace() {
    let (mut client, mut server) = establish_pair();
    deliver_goaway_from_server(
        &mut server,
        &mut client,
        b"moqt://relay.example/".to_vec(),
        10000,
    );
    let err = client
        .send_publish_namespace(ns(&[b"live"]), MessageParameters::new())
        .unwrap_err();
    assert_eq!(err, SendRequestError::PeerGoawayReceived);
}

/// peer GOAWAY 受信後に send_subscribe_namespace が PeerGoawayReceived を返す
#[test]
fn peer_goaway_suppresses_send_subscribe_namespace() {
    let (mut client, mut server) = establish_pair();
    deliver_goaway_from_server(
        &mut server,
        &mut client,
        b"moqt://relay.example/".to_vec(),
        10000,
    );
    let err = client
        .send_subscribe_namespace(ns(&[b"live"]), MessageParameters::new())
        .unwrap_err();
    assert_eq!(err, SendRequestError::PeerGoawayReceived);
}

/// peer GOAWAY 受信後に send_track_status が PeerGoawayReceived を返す
#[test]
fn peer_goaway_suppresses_send_track_status() {
    let (mut client, mut server) = establish_pair();
    deliver_goaway_from_server(
        &mut server,
        &mut client,
        b"moqt://relay.example/".to_vec(),
        10000,
    );
    let err = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .unwrap_err();
    assert_eq!(err, SendRequestError::PeerGoawayReceived);
}

/// peer GOAWAY 受信後に send_subscribe_tracks が PeerGoawayReceived を返す
#[test]
fn peer_goaway_suppresses_send_subscribe_tracks() {
    let (mut client, mut server) = establish_pair();
    deliver_goaway_from_server(
        &mut server,
        &mut client,
        b"moqt://relay.example/".to_vec(),
        10000,
    );
    let err = client
        .send_subscribe_tracks(ns(&[b"live"]), MessageParameters::new())
        .unwrap_err();
    assert_eq!(err, SendRequestError::PeerGoawayReceived);
}

// ─── 非回帰: peer GOAWAY 受信後も応答・更新系 API は抑制されない ────

/// peer GOAWAY 受信後でも send_request_update はエラーを返さない
#[test]
fn peer_goaway_does_not_suppress_send_request_update() {
    let (mut client, mut server) = establish_pair();
    // 先に SUBSCRIBE を確立しておく (REQUEST_UPDATE の前提)
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
    // server (peer) が control GOAWAY を送信し、client が受信
    server
        .send_goaway(b"moqt://relay.example/".to_vec(), 10000)
        .expect("テストフィクスチャの前提条件を満たす");
    let go_msg = take_send_control(&mut server);
    client
        .recv_control(go_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 既存 subscription への REQUEST_UPDATE は抑制されない
    client
        .send_request_update(sub_rid, MessageParameters::new())
        .expect("peer GOAWAY 受信後も REQUEST_UPDATE は送信可能であること");
}

/// peer GOAWAY 受信後でも send_request_ok はエラーを返さない
#[test]
fn peer_goaway_does_not_suppress_send_request_ok() {
    let (mut client, mut server) = establish_pair();
    // server (publisher) が PUBLISH を送り、client が受信
    let pub_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut server);
    client
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // server (peer) が control GOAWAY を送信し、client が受信
    server
        .send_goaway(b"moqt://relay.example/".to_vec(), 10000)
        .expect("テストフィクスチャの前提条件を満たす");
    let go_msg = take_send_control(&mut server);
    client
        .recv_control(go_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 既存 request への REQUEST_OK は抑制されない
    client
        .send_request_ok(pub_rid, MessageParameters::new(), TrackProperties::new())
        .expect("peer GOAWAY 受信後も REQUEST_OK は送信可能であること");
}

/// peer GOAWAY 受信後でも send_request_error はエラーを返さない
#[test]
fn peer_goaway_does_not_suppress_send_request_error() {
    let (mut client, mut server) = establish_pair();
    // server (publisher) が PUBLISH を送り、client が受信
    let pub_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut server);
    client
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // server (peer) が control GOAWAY を送信し、client が受信
    server
        .send_goaway(b"moqt://relay.example/".to_vec(), 10000)
        .expect("テストフィクスチャの前提条件を満たす");
    let go_msg = take_send_control(&mut server);
    client
        .recv_control(go_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 既存 request への REQUEST_ERROR は抑制されない
    client
        .send_request_error(
            pub_rid,
            0,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("rejected")
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("peer GOAWAY 受信後も REQUEST_ERROR は送信可能であること");
}

/// peer GOAWAY 受信後でも send_fetch_ok はエラーを返さない
#[test]
fn peer_goaway_does_not_suppress_send_fetch_ok() {
    let (mut client, mut server) = establish_pair();
    // client (subscriber) が FETCH を送り、server (publisher) が受信
    let fetch_rid = client
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
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // client (peer) が control GOAWAY を送信し、server が受信
    client
        .send_goaway(Vec::new(), 10000)
        .expect("テストフィクスチャの前提条件を満たす");
    let go_msg = take_send_control(&mut client);
    server
        .recv_control(go_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 既存 fetch への FETCH_OK は抑制されない
    server
        .send_fetch_ok(
            fetch_rid,
            0,
            Location {
                group_id: 0,
                object_id: 0,
            },
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("peer GOAWAY 受信後も FETCH_OK は送信可能であること");
}

/// peer GOAWAY 受信後でも send_publish_done はエラーを返さない
#[test]
fn peer_goaway_does_not_suppress_send_publish_done() {
    let (mut client, mut server) = establish_pair();
    // server (publisher) が PUBLISH を送り、client が REQUEST_OK で確立
    let pub_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut server);
    client
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .send_request_ok(pub_rid, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(pub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // client (peer) が control GOAWAY を送信し、server が受信
    client
        .send_goaway(Vec::new(), 10000)
        .expect("テストフィクスチャの前提条件を満たす");
    let go_msg = take_send_control(&mut client);
    server
        .recv_control(go_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // publisher 側で 1 本 stream を open→close し、published_stream_count > 0 にする
    // (0 stream では draft §9.9 の MUST により sentinel を送れない)
    let pre_stream_id = DataStreamId(69);
    server
        .send_subgroup_header(
            pre_stream_id,
            pub_rid,
            &SubgroupHeader {
                track_alias: 999,
                group_id: 0,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(1),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_data_stream_closed(pre_stream_id, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    // 既存 subscription への PUBLISH_DONE は抑制されない
    server
        .send_publish_done(
            pub_rid,
            0,
            PUBLISH_DONE_STREAM_COUNT_UNKNOWN,
            shiguredo_moqt::message::ReasonPhrase::new("done")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("peer GOAWAY 受信後も PUBLISH_DONE は送信可能であること");
}

// ─── 非回帰: 自側 GOAWAY 送信後は新規リクエスト送信可能 ────

/// 自側 GOAWAY 送信後 (local_sent のみ、peer = None) は send_subscribe がエラーを返さない
///
/// draft-ietf-moq-transport-21 §9.2 (GOAWAY): "Sending a GOAWAY does not prevent
/// the sender from initiating new requests [...]."
#[test]
fn goaway_on_request_stream_does_not_suppress_send_subscribe() {
    let (mut client, _server) = establish_pair();
    // client 自身が GOAWAY を送信 (peer からは未受信)
    client
        .send_goaway(Vec::new(), 10000)
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_send_control(&mut client);
    // 自側 GOAWAY 送信後は新規 SUBSCRIBE が抑制されない
    client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("自側 GOAWAY 送信後は新規 SUBSCRIBE が送信可能であること");
}

// ─── From 変換 ────

/// SendRequestError::from(SessionError) が SendRequestError::Session(_) を生成する
#[test]
fn send_request_error_from_session_error() {
    let session_err = SessionError::new(0x1, "test error");
    let send_err: SendRequestError = session_err.clone().into();
    assert_eq!(send_err, SendRequestError::Session(session_err));
    // as_session_error で内部エラーを参照できる
    assert_eq!(
        send_err
            .as_session_error()
            .expect("Session エラーであること")
            .code,
        0x1
    );
    // PeerGoawayReceived の場合は as_session_error は None
    let goaway_err = SendRequestError::PeerGoawayReceived;
    assert!(goaway_err.as_session_error().is_none());
}

// ─── GOAWAY deadline の自動 CloseSession ─────────────────────
//
// draft-ietf-moq-transport-21 §9.2 (GOAWAY): GOAWAY の timeout フィールドと
// sans-I/O tick から導出される deadline で drain が終わらなければ
// GOAWAY_TIMEOUT で自動的に閉じる。内部の deadline 値ではなく
// CloseSession イベントの有無で検証する。

/// GOAWAY 未送信時は tick を進めても GOAWAY_TIMEOUT で閉じない
///
/// draft-ietf-moq-transport-21 §9.2 (GOAWAY): GOAWAY を送信していなければ
/// deadline は存在しない。
#[test]
fn no_goaway_timeout_close_without_goaway_sent() {
    let (mut client, _server, _rid) = establish_subscribe_track(1);
    client.set_control_message_timeout_ms(None);
    client.set_data_stream_timeout_ms(None);
    client.tick(1_000_000);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::CloseSession(_)) {
            got = true;
            break;
        }
    }
    assert!(!got, "GOAWAY 未送信時は GOAWAY_TIMEOUT で閉じないこと");
}

/// send_goaway(uri, 0) では timeout=0 のため自動で閉じない
///
/// draft-ietf-moq-transport-21 §9.2 (GOAWAY): timeout==0 は specific timeout なし。
#[test]
fn no_goaway_timeout_close_when_timeout_zero() {
    let (mut client, _server, _rid) = establish_subscribe_track(1);
    client.set_control_message_timeout_ms(None);
    client.set_data_stream_timeout_ms(None);
    client
        .send_goaway(Vec::new(), 0)
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_goaway(&mut client);
    client.tick(1_000_000);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::CloseSession(_)) {
            got = true;
            break;
        }
    }
    assert!(!got, "timeout=0 の GOAWAY では自動で閉じないこと");
}

/// tick 未実施で send_goaway(uri, T) を呼ぶと deadline は最初の tick 基準で確定する
///
/// draft-ietf-moq-transport-21 §9.2 (GOAWAY): sans-I/O 制約のため、
/// tick を受け取るまで deadline を確定できない。
#[test]
fn goaway_timeout_uses_first_tick_as_base() {
    let (mut client, _server, _rid) = establish_subscribe_track(1);
    client.set_control_message_timeout_ms(None);
    client.set_data_stream_timeout_ms(None);
    // tick を呼ばずに GOAWAY を送信 (timeout=5000)
    client
        .send_goaway(Vec::new(), 5000)
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_goaway(&mut client);
    // 最初の tick で deadline が確定する (now_ms=100 → 100 + 5000 = 5100)
    client.tick(100);
    client.tick(5099);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::CloseSession(_)) {
            got = true;
            break;
        }
    }
    assert!(!got, "deadline 前は閉じないこと");
    client.tick(5100);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::CloseSession(err) = ev {
            assert_eq!(
                err.code,
                shiguredo_moqt::error::SESSION_GOAWAY_TIMEOUT,
                "GOAWAY_TIMEOUT で閉じること"
            );
            got = true;
            break;
        }
    }
    assert!(
        got,
        "deadline 到達で GOAWAY_TIMEOUT の CloseSession が発火すること"
    );
}

/// tick 後に send_goaway(uri, T) を呼ぶと deadline はその時刻基準で確定する
///
/// draft-ietf-moq-transport-21 §9.2 (GOAWAY): 基準時刻が既知の場合、
/// deadline は送信後の tick で確定する。
#[test]
fn goaway_timeout_uses_known_base_time() {
    let (mut client, _server, _rid) = establish_subscribe_track(1);
    client.set_control_message_timeout_ms(None);
    client.set_data_stream_timeout_ms(None);
    // 先に tick で基準時刻を確定する
    client.tick(200);
    // GOAWAY を送信 (timeout=3000 → deadline は 200 + 3000 = 3200)
    client
        .send_goaway(Vec::new(), 3000)
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_goaway(&mut client);
    client.tick(3199);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::CloseSession(_)) {
            got = true;
            break;
        }
    }
    assert!(!got, "deadline 前は閉じないこと");
    client.tick(3200);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::CloseSession(err) = ev {
            assert_eq!(
                err.code,
                shiguredo_moqt::error::SESSION_GOAWAY_TIMEOUT,
                "GOAWAY_TIMEOUT で閉じること"
            );
            got = true;
            break;
        }
    }
    assert!(
        got,
        "deadline 到達で GOAWAY_TIMEOUT の CloseSession が発火すること"
    );
}

/// 自側 GOAWAY 送信後に peer から届いた request に含まれる AUTHORIZATION_TOKEN REGISTER が
/// `peer_token_cache` に反映されること (draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION
/// TOKEN Parameter): "The receiver of a message carrying an AUTHORIZATION TOKEN with Alias
/// Type REGISTER that does not result in a Session error MUST register the Token Alias in
/// the token cache, even if the message fails for other reasons, including Unauthorized.")。
///
/// `REQUEST_GOING_AWAY` は request error であって session error ではないため MUST の対象。
/// REGISTER が反映されなかった旧実装では、後続の別 request で同じ alias に USE_ALIAS すると
/// UNKNOWN_AUTH_TOKEN_ALIAS でセッション終了に至っていた。
#[test]
fn goaway_sent_side_registers_auth_token_from_going_away_rejected_subscribe() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    // server が control GOAWAY を送信 (client への配送は本テストの検証対象外)
    server
        .send_goaway(Vec::new(), 10_000)
        .expect("server は GOAWAY を送信できる");
    let _ = take_goaway(&mut server);

    // client → server: SUBSCRIBE (REGISTER alias=7)
    let rid1 = client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            params_with_register(7, 2, b"tok"),
        )
        .expect("client の SUBSCRIBE 送信は成功する");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("GOAWAY 拒否経路でも recv_request は Err にならない");
    assert_eq!(server.state(), SessionState::Established);

    // server が REQUEST_ERROR(GOING_AWAY) を発行していること
    let (err_rid, err_msg) = take_send_on_stream(&mut server);
    assert_eq!(err_rid, rid1);
    match err_msg {
        ControlMessage::RequestError(err) => {
            assert_eq!(err.error_code, REQUEST_GOING_AWAY);
        }
        other => panic!("REQUEST_ERROR(GOING_AWAY) を期待したが {other:?} を受け取った"),
    }

    // server の peer_token_cache に alias=7 が REGISTER されていること (draft §9.20.3 MUST)
    assert!(
        server.peer_auth_token_cache().resolve(7).is_some(),
        "GOAWAY 拒否経路でも AUTHORIZATION_TOKEN REGISTER が peer_token_cache に反映されること"
    );

    // 続けて client → server: 別 SUBSCRIBE (USE_ALIAS alias=7)
    let _rid2 = client
        .send_subscribe(ns(&[b"live"]), b"mic".to_vec(), params_with_use_alias(7))
        .expect("client の 2 本目 SUBSCRIBE 送信は成功する");
    let (_, sub_msg2) = take_send_request(&mut client);
    // REGISTER が反映されているため USE_ALIAS は UNKNOWN_AUTH_TOKEN_ALIAS にならず、
    // GOAWAY 拒否として REQUEST_ERROR(GOING_AWAY) のみ発行される
    server
        .recv_request(sub_msg2)
        .expect("REGISTER 済みの USE_ALIAS で session error にならないこと");
    assert_eq!(server.state(), SessionState::Established);
}

/// GOAWAY 拒否経路で REGISTER が session error (AUTH_TOKEN_CACHE_OVERFLOW) になった場合、
/// session error が REQUEST_GOING_AWAY より優先され、セッションが Closing に遷移すること
/// (draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter) の MUST は
/// "does not result in a Session error" が前提)。
///
/// MAX_AUTH_TOKEN_CACHE_SIZE=20 バイト (Token 1 個分 = 16 + Token Value 4 バイト) で
/// 1 件登録済みの状態にした後、GOAWAY 送信 → 2 件目の REGISTER で overflow を再現する
/// (通常運用に近い overflow ケース)。
#[test]
fn goaway_sent_side_register_overflow_takes_priority_over_going_away() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(20);
    // 事前に 1 件登録して cache を満杯にする (GOAWAY 前の通常経路で REGISTER)
    client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            params_with_register(1, 2, b"tok0"),
        )
        .expect("client の初回 SUBSCRIBE 送信は成功する");
    let (_, sub_msg0) = take_send_request(&mut client);
    server
        .recv_request(sub_msg0)
        .expect("初回 REGISTER は cache に収まる");
    assert_eq!(server.state(), SessionState::Established);
    assert!(
        server.peer_auth_token_cache().resolve(1).is_some(),
        "初回 REGISTER が反映されていること"
    );

    // server が GOAWAY を送信
    server
        .send_goaway(Vec::new(), 10_000)
        .expect("server は GOAWAY を送信できる");
    let _ = take_goaway(&mut server);

    // 2 件目の REGISTER は cache 満杯で overflow (session error)
    let _rid = client
        .send_subscribe(
            ns(&[b"live"]),
            b"mic".to_vec(),
            params_with_register(2, 2, b"tok1"),
        )
        .expect("client の 2 本目 SUBSCRIBE 送信は成功する");
    let (_, sub_msg1) = take_send_request(&mut client);
    let err = server
        .recv_request(sub_msg1)
        .expect_err("REGISTER overflow は session error として扱われるはず");
    assert_eq!(
        err.as_session_error()
            .expect("SessionError が期待された")
            .code,
        shiguredo_moqt::error::SESSION_AUTH_TOKEN_CACHE_OVERFLOW
    );
    assert!(matches!(
        server.state(),
        SessionState::Closing | SessionState::Closed
    ));
}

/// GOAWAY 拒否経路では USE_ALIAS / DELETE / USE_VALUE を触らないこと
/// (draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter) の MUST は
/// "with Alias Type REGISTER" 限定で、DELETE / USE_ALIAS / USE_VALUE は「失敗した request の
/// パラメータ」として peer 側で not-applied 扱いされる可能性があるため触らない)。
///
/// 未登録 alias に USE_ALIAS を含む request が GOAWAY 送信後に届いた場合、旧実装なら
/// GOING_AWAY で benign 拒否だったが、`accept_peer_request` で `apply_peer_message_auth_tokens`
/// を無条件に呼ぶと `UNKNOWN_AUTH_TOKEN_ALIAS` で session を kill する回帰になる。
#[test]
fn goaway_sent_side_use_alias_after_goaway_does_not_kill_session() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    server
        .send_goaway(Vec::new(), 10_000)
        .expect("server は GOAWAY を送信できる");
    let _ = take_goaway(&mut server);

    // 未登録 alias に対して USE_ALIAS を送る (peer_token_cache には未登録)
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), params_with_use_alias(42))
        .expect("client の SUBSCRIBE 送信は成功する");
    let (_, sub_msg) = take_send_request(&mut client);
    // GOAWAY 拒否では USE_ALIAS を触らないため session error にならない
    server
        .recv_request(sub_msg)
        .expect("USE_ALIAS は GOAWAY 拒否経路では触られず session error にならないこと");
    assert_eq!(server.state(), SessionState::Established);

    // 発行されるのは REQUEST_ERROR(GOING_AWAY) のみ
    let (err_rid, err_msg) = take_send_on_stream(&mut server);
    assert_eq!(err_rid, rid);
    match err_msg {
        ControlMessage::RequestError(err) => {
            assert_eq!(err.error_code, REQUEST_GOING_AWAY);
        }
        other => panic!("REQUEST_ERROR(GOING_AWAY) を期待したが {other:?} を受け取った"),
    }

    // peer_token_cache に alias=42 が入っていないこと (USE_ALIAS は not-applied)
    assert!(
        server.peer_auth_token_cache().resolve(42).is_none(),
        "USE_ALIAS は GOAWAY 拒否経路で cache を触らないこと"
    );
}
