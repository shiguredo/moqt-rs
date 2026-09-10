use super::*;

// ─── request_streams lifecycle ─────────────────────
//
// draft-ietf-moq-transport-21 §6.3 (Session initialization) が規定する 7 種類の bidi request stream
// それぞれについて、`send_*` / `handle_peer_*` で種別ごとのテーブルに insert
// され、`forget_*` で remove されることを検証する。未登録 request_id に対する
// テーブル参照が `None` を返すことも併せて確認する。

/// SUBSCRIBE request_id が `Subscribe` で追跡され、forget で除去される
#[test]
fn request_stream_tracks_subscribe_lifecycle() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.subscription(rid).is_some(),
        "送信側の subscription テーブルに登録されること"
    );
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        server.subscription(rid).is_some(),
        "受信側の subscription テーブルに登録されること"
    );

    // REQUEST_ERROR で Terminated → forget で subscription テーブルからも除去
    server
        .send_request_error(
            rid,
            shiguredo_moqt::error::REQUEST_INTERNAL_ERROR,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("rejected")
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
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated
    );
    client
        .forget_subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.forget_subscription(rid).is_none(),
        "除去後の再 forget は None を返すこと (追跡から除去済み)"
    );
    server
        .forget_subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        server.forget_subscription(rid).is_none(),
        "除去後の再 forget は None を返すこと (追跡から除去済み)"
    );
}

/// PUBLISH request_id が `Publish` で追跡され、forget で除去される
#[test]
fn request_stream_tracks_publish_lifecycle() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            42,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.subscription(rid).is_some(),
        "送信側の subscription テーブルに登録されること"
    );
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        server.subscription(rid).is_some(),
        "受信側の subscription テーブルに登録されること"
    );

    server
        .send_request_error(
            rid,
            shiguredo_moqt::error::REQUEST_INTERNAL_ERROR,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("no")
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, err_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, err_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .forget_subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .forget_subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.forget_subscription(rid).is_none(),
        "除去後の再 forget は None を返すこと (追跡から除去済み)"
    );
    assert!(
        server.forget_subscription(rid).is_none(),
        "除去後の再 forget は None を返すこと (追跡から除去済み)"
    );
}

/// FETCH request_id が `Fetch` で追跡され、forget で除去される
#[test]
fn request_stream_tracks_fetch_lifecycle() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::session::types::RequestStreamEnd;
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
    assert!(
        client.fetch(rid).is_some(),
        "送信側の fetch テーブルに登録されること"
    );
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        server.fetch(rid).is_some(),
        "受信側の fetch テーブルに登録されること"
    );

    // subscriber 側: FIN 相当で Terminated に遷移させて forget
    client
        .recv_fetch_data_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .forget_fetch(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.forget_fetch(rid).is_none(),
        "除去後の再 forget は None を返すこと (追跡から除去済み)"
    );

    // publisher 側: REQUEST_ERROR を返して Terminated に → forget
    server
        .send_request_error(
            rid,
            shiguredo_moqt::error::REQUEST_INTERNAL_ERROR,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("err")
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_send_on_stream(&mut server);
    server
        .forget_fetch(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        server.forget_fetch(rid).is_none(),
        "除去後の再 forget は None を返すこと (追跡から除去済み)"
    );
}

/// PUBLISH_NAMESPACE request_id が `PublishNamespace` で追跡され、forget で除去される
#[test]
fn request_stream_tracks_publish_namespace_lifecycle() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.namespace_publication(rid).is_some(),
        "送信側の namespace publication テーブルに登録されること"
    );
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        server.namespace_publication(rid).is_some(),
        "受信側の namespace publication テーブルに登録されること"
    );

    server
        .send_request_ok(
            rid,
            MessageParameters::new(),
            shiguredo_moqt::track_properties::TrackProperties::default(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // Established 状態でも forget 可能 (Pending 以外なら削除される)
    client
        .forget_namespace_publication(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .forget_namespace_publication(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.forget_namespace_publication(rid).is_none(),
        "除去後の再 forget は None を返すこと (追跡から除去済み)"
    );
    assert!(
        server.forget_namespace_publication(rid).is_none(),
        "除去後の再 forget は None を返すこと (追跡から除去済み)"
    );
}

/// SUBSCRIBE_NAMESPACE request_id が `SubscribeNamespace` で追跡され、forget で除去される
#[test]
fn request_stream_tracks_subscribe_namespace_lifecycle() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.namespace_subscription(rid).is_some(),
        "送信側の namespace subscription テーブルに登録されること"
    );
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        server.namespace_subscription(rid).is_some(),
        "受信側の namespace subscription テーブルに登録されること"
    );

    server
        .send_request_ok(
            rid,
            MessageParameters::new(),
            shiguredo_moqt::track_properties::TrackProperties::default(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .forget_namespace_subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .forget_namespace_subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.forget_namespace_subscription(rid).is_none(),
        "除去後の再 forget は None を返すこと (追跡から除去済み)"
    );
    assert!(
        server.forget_namespace_subscription(rid).is_none(),
        "除去後の再 forget は None を返すこと (追跡から除去済み)"
    );
}

/// TRACK_STATUS request_id が `TrackStatus` で追跡され、forget で除去される
#[test]
fn request_stream_tracks_track_status_lifecycle() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.track_status_request(rid).is_some(),
        "送信側の track status テーブルに登録されること"
    );
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        server.track_status_request(rid).is_some(),
        "受信側の track status テーブルに登録されること"
    );

    server
        .send_request_ok(
            rid,
            MessageParameters::new(),
            shiguredo_moqt::track_properties::TrackProperties::default(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // TRACK_STATUS_OK は FIN で送られるため、両端で bidi stream 終端を通知してから forget する
    client
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .forget_track_status(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .forget_track_status(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.forget_track_status(rid).is_none(),
        "除去後の再 forget は None を返すこと (追跡から除去済み)"
    );
    assert!(
        server.forget_track_status(rid).is_none(),
        "除去後の再 forget は None を返すこと (追跡から除去済み)"
    );
}

/// 未知 request_id に対して各テーブル参照は `None` を返し、
/// SETUP 直後は全 request テーブルが空である
#[test]
fn unknown_id_tables_return_none() {
    let (client, _) = establish_pair();
    assert!(client.subscription(9999).is_none());
    assert!(client.fetch(9999).is_none());
    assert_no_tracked_requests(&client);
}

// ─── SETUP 前 request の扱い ─────────────────────

/// draft-ietf-moq-transport-21 §6.3 (Session initialization): SETUP 完了前の request は session を落とさず
/// `RecvRequestError::BeforeSessionEstablished` を返す (buffer するか reset するか)
#[test]
fn recv_request_before_setup_returns_deferred_not_closing_session() {
    use shiguredo_moqt::message::{Publish, Subscribe, common::TrackNamespace};
    use shiguredo_moqt::session::types::RecvRequestError;
    let mut server = Session::new_server(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.state(), SessionState::LocalSetupSent);
    let msg = ControlMessage::Subscribe(Subscribe {
        request_id: 0,

        track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
            .expect("テストフィクスチャの前提条件を満たす"),
        track_name: b"cam".to_vec(),
        parameters: MessageParameters::new(),
    });
    let err = server.recv_request(msg).unwrap_err();
    assert!(matches!(err, RecvRequestError::BeforeSessionEstablished));
    // session state は変わらず、closing にも遷移しない
    assert_eq!(server.state(), SessionState::LocalSetupSent);
    // SessionError として取り出そうとすると None
    assert!(err.as_session_error().is_none());

    // PUBLISH でも同じ挙動
    let pub_msg = ControlMessage::Publish(Publish {
        request_id: 0,

        track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
            .expect("テストフィクスチャの前提条件を満たす"),
        track_name: b"cam".to_vec(),
        track_alias: 1,
        parameters: MessageParameters::new(),
        track_properties: TrackProperties::new(),
    });
    let err2 = server.recv_request(pub_msg).unwrap_err();
    assert!(matches!(err2, RecvRequestError::BeforeSessionEstablished));
    assert_eq!(server.state(), SessionState::LocalSetupSent);
}

/// SETUP 完了前に前置到着した request を捨てたあとに SETUP を完了させ、
/// 同内容の request を再投入すると今度は正常に受理される
#[test]
fn recv_request_accepted_after_setup_completes() {
    use shiguredo_moqt::message::{Subscribe, common::TrackNamespace};
    use shiguredo_moqt::session::types::RecvRequestError;
    let mut client = Session::new_client(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let mut server = Session::new_server(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");

    // 前置到着を一度演じる: server が SETUP 前の SUBSCRIBE を受けて Deferred を返す
    let early_msg = ControlMessage::Subscribe(Subscribe {
        request_id: 0,

        track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
            .expect("テストフィクスチャの前提条件を満たす"),
        track_name: b"cam".to_vec(),
        parameters: MessageParameters::new(),
    });
    let err = server.recv_request(early_msg).unwrap_err();
    assert!(matches!(err, RecvRequestError::BeforeSessionEstablished));

    // SETUP を完了させる
    let c_setup = take_send_control(&mut client);
    let s_setup = take_send_control(&mut server);
    server
        .recv_control(c_setup)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(s_setup)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.state(), SessionState::Established);

    // 同じ subscribe を今度は client から正規に送って server で受理する
    let rid = client
        .send_subscribe(
            TrackNamespace::new(vec![b"live".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            b"cam".to_vec(),
            MessageParameters::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        server.subscription(rid).is_some(),
        "受信側の subscription テーブルに登録されること"
    );
}

/// SETUP 完了後に許容されない message type (例: Setup) を recv_request に渡すと
/// PROTOCOL_VIOLATION で session が closing になる (従来挙動の維持)
#[test]
fn recv_request_unsupported_message_closes_session() {
    use shiguredo_moqt::session::types::RecvRequestError;
    let (_, mut server) = establish_pair();
    // SETUP は bidi request stream の開始メッセージとして許容されない
    let err = server
        .recv_request(ControlMessage::Setup(Setup {
            options: SetupOptions::new(),
        }))
        .unwrap_err();
    let RecvRequestError::Session(session_err) = err else {
        panic!("Session のバリアントが期待されたが BeforeSessionEstablished を受け取った");
    };
    assert_eq!(session_err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

// ─── recv_request_stream_closed ──────────────────

/// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection) (request の cancel) + draft-ietf-moq-transport-21 §3.1 (Subscriptions) (STOP_SENDING による終端):
/// SUBSCRIBE の bidi request stream 終端で subscription が Terminated に遷移し、
/// RequestTerminated イベントが発火する
#[test]
fn request_stream_closed_terminates_subscribe() {
    use shiguredo_moqt::session::types::RequestStreamEnd;
    use shiguredo_moqt::session::types::TerminationReason;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // subscriber 側 (自側 SUBSCRIBE を送った client) が FIN で閉じる
    client
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated
    );
    let mut got = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind: RequestKind::Subscribe,
            reason: TerminationReason::PeerStreamFin,
        } = e
        {
            assert_eq!(request_id, rid);
            got = true;
            break;
        }
    }
    assert!(got, "RequestTerminated(PeerStreamFin) イベントが期待された");
}

/// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection) (request の cancel) + draft-ietf-moq-transport-21 §3.1 (Subscriptions) (PUBLISH_DONE/終端):
/// PUBLISH の bidi request stream を RESET_STREAM で閉じると subscription が Terminated、
/// エラーコードも伝わる
#[test]
fn request_stream_closed_reset_terminates_publish() {
    use shiguredo_moqt::session::types::RequestStreamEnd;
    use shiguredo_moqt::session::types::TerminationReason;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            11,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    server
        .recv_request_stream_closed(
            rid,
            RequestStreamEnd::Reset {
                error_code: 42,
                reliable_size: None,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let mut got = None;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind: RequestKind::Publish,
            reason: TerminationReason::PeerStreamReset { error_code },
        } = e
        {
            assert_eq!(request_id, rid);
            got = Some(error_code);
            break;
        }
    }
    assert_eq!(got, Some(42));
}

/// FETCH の bidi request stream 終端で Fetch が Terminated に遷移
#[test]
fn request_stream_closed_terminates_fetch() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::session::types::FetchState;
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let (mut client, _) = establish_pair();
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
    client
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Terminated
    );
}

/// PUBLISH_NAMESPACE の bidi request stream 終端で状態が Terminated に遷移
#[test]
fn request_stream_closed_terminates_publish_namespace() {
    use shiguredo_moqt::session::types::NamespacePublicationState;
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let (mut client, _) = establish_pair();
    let rid = client
        .send_publish_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .namespace_publication(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        NamespacePublicationState::Terminated
    );
}

/// TRACK_STATUS の応答待ち状態で stream を閉じると Error 応答に遷移
#[test]
fn request_stream_closed_track_status_pending_becomes_failed() {
    use shiguredo_moqt::session::types::RequestStreamEnd;
    use shiguredo_moqt::session::types::TrackStatusResponse;
    let (mut client, _) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .track_status_request(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .response,
        Some(TrackStatusResponse::Error)
    );
}

/// 未知 request_id に対する recv_request_stream_closed は PROTOCOL_VIOLATION
#[test]
fn request_stream_closed_unknown_id_closes_session() {
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let (mut client, _) = establish_pair();
    let err = client
        .recv_request_stream_closed(9999, RequestStreamEnd::Fin)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}

// ─── Pending(Subscriber) supersede by PUBLISH ────

/// draft-ietf-moq-transport-21 §3.1 (Subscriptions): 既存 Pending(Subscriber) subscription に対して
/// 同一 track の PUBLISH を受信すると、既存を Terminated に遷移させた上で
/// 新 PUBLISH を受諾する (SUBSCRIBE と PUBLISH が交差したケース)
#[test]
fn publish_supersedes_pending_subscriber_for_same_track() {
    use shiguredo_moqt::session::types::TerminationReason;
    let (mut client, mut server) = establish_pair();

    // Client が SUBSCRIBE を送って Pending(Subscriber) 状態
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    assert!(
        client
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_pending_subscriber()
    );

    // Server は SUBSCRIBE を受信済みだが SUBSCRIBE_OK を返す前に、別の経路で
    // PUBLISH を開始する (実運用では両端がそれぞれ同時に request を出したケース)
    // ここは手動で client 側に相手からの PUBLISH を投入してシミュレートする。
    use shiguredo_moqt::message::Publish;
    let publish_rid = server
        .next_local_request_id()
        .expect("テストフィクスチャの前提条件を満たす");
    let publish_msg = ControlMessage::Publish(Publish {
        request_id: publish_rid,

        track_namespace: ns(&[b"live"]),
        track_name: b"cam".to_vec(),
        track_alias: 123,
        parameters: MessageParameters::new(),
        track_properties: TrackProperties::new(),
    });
    client
        .recv_request(publish_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // 既存 SUBSCRIBE は Terminated、新 PUBLISH は Pending(Publisher)
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated
    );
    assert!(
        client
            .subscription(publish_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_pending_publisher()
    );

    // RequestTerminated(SupersededByPublish) イベントが発火している
    let mut got_superseded = false;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind: RequestKind::Subscribe,
            reason: TerminationReason::SupersededByPublish { new_request_id },
        } = e
        {
            assert_eq!(request_id, sub_rid);
            assert_eq!(new_request_id, publish_rid);
            got_superseded = true;
            break;
        }
    }
    assert!(
        got_superseded,
        "expected RequestTerminated(SupersededByPublish)"
    );

    // 旧 subscribe を forget しても新 PUBLISH の subscriptions_by_track entry は
    // 壊れない
    client
        .forget_subscription(sub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client
            .subscription(publish_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_pending_publisher()
    );

    // 旧 sub_rid は消えているが、新 publish_rid は追跡されている
    assert!(
        client.forget_subscription(sub_rid).is_none(),
        "除去済みの旧 subscribe は追跡されていないこと"
    );
    assert!(
        client.subscription(publish_rid).is_some(),
        "新 PUBLISH は追跡されていること"
    );
    let _ = sub_msg;
}

/// draft-ietf-moq-transport-21 §3.1.1: 同一 Track への複数同時 subscription が許可されたため、
/// Established subscription が存在する Track への PUBLISH も受理される
#[test]
fn publish_on_established_subscription_is_accepted_in_draft_20() {
    use shiguredo_moqt::message::Publish;
    let (mut client, mut server) = establish_pair();

    // Client: SUBSCRIBE → SUBSCRIBE_OK で Established
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
            500,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established
    );

    // draft-20: 同一 Track への PUBLISH も受理される (複数 subscription 許可)
    let publish_rid = server
        .next_local_request_id()
        .expect("テストフィクスチャの前提条件を満たす");
    let publish_msg = ControlMessage::Publish(Publish {
        request_id: publish_rid,

        track_namespace: ns(&[b"live"]),
        track_name: b"cam".to_vec(),
        track_alias: 999,
        parameters: MessageParameters::new(),
        track_properties: TrackProperties::new(),
    });
    client
        .recv_request(publish_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // 既存 SUBSCRIBE は Established のまま、新 PUBLISH も subscriptions に登録される
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established
    );
    assert!(client.subscription(publish_rid).is_some());
}

/// Server が非ゼロ Connect URI の Redirect 付き REQUEST_ERROR を受信したら PROTOCOL_VIOLATION
///
/// draft-ietf-moq-transport-21 §9.4.1 (Redirect Structure):
/// "If a server receives a Redirect with a non-zero Connect URI Length
/// it MUST close the session with a PROTOCOL_VIOLATION."
#[test]
fn server_receives_redirect_nonzero_connect_uri_closes_session() {
    use shiguredo_moqt::message::{Redirect, RequestError};

    let (_client, mut server, rid) = establish_subscribe_track(1);
    let err = server
        .recv_stream_message(
            rid,
            ControlMessage::RequestError(RequestError {
                error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
                retry_interval: 0,
                reason: shiguredo_moqt::message::ReasonPhrase::new("redirect")
                    .expect("正当な reason phrase である"),
                redirect: Some(Redirect {
                    connect_uri: b"moqt://relay.example/".to_vec(),
                    track_namespace: TrackNamespace::new(vec![b"ns".to_vec()])
                        .expect("正当な namespace である"),
                    track_name: b"track".to_vec(),
                }),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// Server が空 Connect URI の Redirect 付き REQUEST_ERROR を受信した場合は通常処理される
///
/// draft-ietf-moq-transport-21 §9.4.1: 非ゼロの場合のみ MUST close であるため、
/// 空 Connect URI は許容される。
#[test]
fn server_receives_redirect_zero_connect_uri_accepted() {
    use shiguredo_moqt::message::{Redirect, RequestError};

    let (_client, mut server, rid) = establish_subscribe_track(2);
    let result = server.recv_stream_message(
        rid,
        ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
            retry_interval: 0,
            reason: shiguredo_moqt::message::ReasonPhrase::new("redirect")
                .expect("正当な reason phrase である"),
            redirect: Some(Redirect {
                connect_uri: Vec::new(),
                track_namespace: TrackNamespace::new(vec![b"ns".to_vec()])
                    .expect("正当な namespace である"),
                track_name: b"track".to_vec(),
            }),
        }),
    );
    // 空 connect_uri は許容されるためエラーにならない
    assert!(result.is_ok());
    assert_eq!(server.state(), SessionState::Established);
}

/// Client が非ゼロ Connect URI の Redirect 付き REQUEST_ERROR を受信した場合は通常処理される
///
/// draft-ietf-moq-transport-21 §9.4.1 の MUST close は Server 受信時のみに適用される。
/// Client が受信する Redirect は正常なリダイレクト指示として扱われる。
#[test]
fn client_receives_redirect_nonzero_connect_uri_accepted() {
    use shiguredo_moqt::message::{Redirect, RequestError};

    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(
            TrackNamespace::new(vec![b"live".to_vec()]).expect("正当な namespace である"),
            b"cam".to_vec(),
            MessageParameters::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let result = client.recv_stream_message(
        rid,
        ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
            retry_interval: 0,
            reason: shiguredo_moqt::message::ReasonPhrase::new("redirect")
                .expect("正当な reason phrase である"),
            redirect: Some(Redirect {
                connect_uri: b"moqt://relay.example/".to_vec(),
                track_namespace: TrackNamespace::new(vec![b"ns".to_vec()])
                    .expect("正当な namespace である"),
                track_name: b"track".to_vec(),
            }),
        }),
    );
    assert!(result.is_ok());
    assert_eq!(client.state(), SessionState::Established);
}

// ─── Redirect non-empty Track Name for namespace-scoped requests ────
//
// draft-ietf-moq-transport-21 §9.4.1 (Redirect Structure):
// "Track Name is not meaningful for namespace-scoped requests
// (SUBSCRIBE_NAMESPACE, PUBLISH_NAMESPACE, SUBSCRIBE_TRACKS) and MUST be empty;
// an endpoint that receives a non-empty Track Name in a Redirect for a
// namespace-scoped request MUST close the session with a PROTOCOL_VIOLATION."

/// SUBSCRIBE_NAMESPACE への REQUEST_ERROR で Redirect の Track Name が non-empty なら PROTOCOL_VIOLATION
#[test]
fn redirect_nonempty_track_name_for_subscribe_namespace_closes_session() {
    use shiguredo_moqt::message::{Redirect, RequestError};

    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // Redirect の Track Name が non-empty → PROTOCOL_VIOLATION でセッションクローズ
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::RequestError(RequestError {
                error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
                retry_interval: 0,
                reason: shiguredo_moqt::message::ReasonPhrase::new("redirect")
                    .expect("正当な reason phrase である"),
                redirect: Some(Redirect {
                    connect_uri: Vec::new(),
                    track_namespace: TrackNamespace::new(vec![b"ns".to_vec()])
                        .expect("正当な namespace である"),
                    track_name: b"track".to_vec(),
                }),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}

/// PUBLISH_NAMESPACE への REQUEST_ERROR で Redirect の Track Name が non-empty なら PROTOCOL_VIOLATION
#[test]
fn redirect_nonempty_track_name_for_publish_namespace_closes_session() {
    use shiguredo_moqt::message::{Redirect, RequestError};

    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // Redirect の Track Name が non-empty → PROTOCOL_VIOLATION でセッションクローズ
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::RequestError(RequestError {
                error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
                retry_interval: 0,
                reason: shiguredo_moqt::message::ReasonPhrase::new("redirect")
                    .expect("正当な reason phrase である"),
                redirect: Some(Redirect {
                    connect_uri: Vec::new(),
                    track_namespace: TrackNamespace::new(vec![b"ns".to_vec()])
                        .expect("正当な namespace である"),
                    track_name: b"track".to_vec(),
                }),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}

/// SUBSCRIBE_TRACKS への REQUEST_ERROR で Redirect の Track Name が non-empty なら PROTOCOL_VIOLATION
#[test]
fn redirect_nonempty_track_name_for_subscribe_tracks_closes_session() {
    use shiguredo_moqt::message::{Redirect, RequestError};

    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_tracks(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // Redirect の Track Name が non-empty → PROTOCOL_VIOLATION でセッションクローズ
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::RequestError(RequestError {
                error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
                retry_interval: 0,
                reason: shiguredo_moqt::message::ReasonPhrase::new("redirect")
                    .expect("正当な reason phrase である"),
                redirect: Some(Redirect {
                    connect_uri: Vec::new(),
                    track_namespace: TrackNamespace::new(vec![b"ns".to_vec()])
                        .expect("正当な namespace である"),
                    track_name: b"track".to_vec(),
                }),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}

/// SUBSCRIBE (track-scoped) への REQUEST_ERROR で Redirect の Track Name が non-empty でもセッションは閉じない
///
/// Track Name の空制約は namespace-scoped request のみに適用される。
/// SUBSCRIBE は track-scoped であるため、non-empty Track Name は許容される。
#[test]
fn redirect_nonempty_track_name_for_subscribe_accepted() {
    use shiguredo_moqt::message::{Redirect, RequestError};

    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // track-scoped request には Track Name 制約がないため通常処理される
    let result = client.recv_stream_message(
        rid,
        ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
            retry_interval: 0,
            reason: shiguredo_moqt::message::ReasonPhrase::new("redirect")
                .expect("正当な reason phrase である"),
            redirect: Some(Redirect {
                connect_uri: Vec::new(),
                track_namespace: TrackNamespace::new(vec![b"ns".to_vec()])
                    .expect("正当な namespace である"),
                track_name: b"track".to_vec(),
            }),
        }),
    );
    assert!(result.is_ok());
    assert_eq!(client.state(), SessionState::Established);
}

/// FETCH (track-scoped) への REQUEST_ERROR で Redirect の Track Name が non-empty でもセッションは閉じない
///
/// Track Name の空制約は namespace-scoped request のみに適用される。
/// FETCH は track-scoped であるため、non-empty Track Name は許容される。
#[test]
fn redirect_nonempty_track_name_for_fetch_accepted() {
    use shiguredo_moqt::message::{Redirect, RequestError};

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
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // track-scoped request には Track Name 制約がないため通常処理される
    let result = client.recv_stream_message(
        rid,
        ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
            retry_interval: 0,
            reason: shiguredo_moqt::message::ReasonPhrase::new("redirect")
                .expect("正当な reason phrase である"),
            redirect: Some(Redirect {
                connect_uri: Vec::new(),
                track_namespace: TrackNamespace::new(vec![b"ns".to_vec()])
                    .expect("正当な namespace である"),
                track_name: b"track".to_vec(),
            }),
        }),
    );
    assert!(result.is_ok());
    assert_eq!(client.state(), SessionState::Established);
}

/// TRACK_STATUS (track-scoped) への REQUEST_ERROR で Redirect の Track Name が non-empty でもセッションは閉じない
///
/// Track Name の空制約は namespace-scoped request のみに適用される。
/// TRACK_STATUS は track-scoped であるため、non-empty Track Name は許容される。
#[test]
fn redirect_nonempty_track_name_for_track_status_accepted() {
    use shiguredo_moqt::message::{Redirect, RequestError};

    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // track-scoped request には Track Name 制約がないため通常処理される
    let result = client.recv_stream_message(
        rid,
        ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
            retry_interval: 0,
            reason: shiguredo_moqt::message::ReasonPhrase::new("redirect")
                .expect("正当な reason phrase である"),
            redirect: Some(Redirect {
                connect_uri: Vec::new(),
                track_namespace: TrackNamespace::new(vec![b"ns".to_vec()])
                    .expect("正当な namespace である"),
                track_name: b"track".to_vec(),
            }),
        }),
    );
    assert!(result.is_ok());
    assert_eq!(client.state(), SessionState::Established);
}

/// SUBSCRIBE_NAMESPACE への REQUEST_ERROR で Redirect なしならセッションは閉じない
///
/// Track Name 制約は Redirect が存在する場合のみ適用される。
/// Redirect なしの REQUEST_ERROR は通常のエラー応答として処理される。
#[test]
fn redirect_absent_for_subscribe_namespace_accepted() {
    use shiguredo_moqt::message::RequestError;

    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // Redirect なし → Track Name 制約は適用されず通常処理される
    let result = client.recv_stream_message(
        rid,
        ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_INTERNAL_ERROR,
            retry_interval: 0,
            reason: shiguredo_moqt::message::ReasonPhrase::new("internal error")
                .expect("正当な reason phrase である"),
            redirect: None,
        }),
    );
    assert!(result.is_ok());
    assert_eq!(client.state(), SessionState::Established);
}

/// SUBSCRIBE_NAMESPACE への REQUEST_ERROR で Redirect の Track Name が空ならセッションは閉じない
///
/// Track Name が空であれば namespace-scoped request への Redirect として正当である。
#[test]
fn redirect_empty_track_name_for_subscribe_namespace_accepted() {
    use shiguredo_moqt::message::{Redirect, RequestError};

    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // Track Name が空 → 制約を満たすため通常処理される
    let result = client.recv_stream_message(
        rid,
        ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
            retry_interval: 0,
            reason: shiguredo_moqt::message::ReasonPhrase::new("redirect")
                .expect("正当な reason phrase である"),
            redirect: Some(Redirect {
                connect_uri: Vec::new(),
                track_namespace: TrackNamespace::new(vec![b"ns".to_vec()])
                    .expect("正当な namespace である"),
                track_name: Vec::new(),
            }),
        }),
    );
    assert!(result.is_ok());
    assert_eq!(client.state(), SessionState::Established);
}

// ─── forget_track_subscription ─────────────────────
//
// draft-ietf-moq-transport-21 §9.18 (SUBSCRIBE_TRACKS): SUBSCRIBE_TRACKS の
// ライフサイクル追跡と forget による除去を検証する。

/// SUBSCRIBE_TRACKS の Pending / Established / forget 冪等性を検証する
///
/// draft-ietf-moq-transport-21 §9.18 (SUBSCRIBE_TRACKS):
/// - Pending 状態では forget_track_subscription は None を返す (除去拒否)
/// - Established 状態では Some(TrackSubscription) を返して除去する
/// - 除去後の再呼び出しは None を返す (冪等)
#[test]
fn request_stream_tracks_subscribe_tracks_lifecycle() {
    let (mut client, mut server) = establish_pair();

    // SUBSCRIBE_TRACKS を送信して Pending 状態にする
    let rid = client
        .send_subscribe_tracks(ns(&[b"live"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.track_subscription(rid).is_some(),
        "送信側の track subscription テーブルに登録されること"
    );
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // (a) Pending 状態では forget_track_subscription は None を返す
    assert!(
        client.track_subscription(rid).is_some(),
        "track_subscription が登録されていること"
    );
    assert!(
        client.forget_track_subscription(rid).is_none(),
        "Pending 状態では forget_track_subscription は None を返すこと"
    );
    // Pending のまま登録は残っている
    assert!(
        client.track_subscription(rid).is_some(),
        "Pending 拒否後も track_subscription は残っていること"
    );

    // server が REQUEST_OK を返して Established に遷移させる
    server
        .send_request_ok(
            rid,
            MessageParameters::new(),
            shiguredo_moqt::track_properties::TrackProperties::default(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // (b) Established 状態では Some(TrackSubscription) を返して除去する
    let forgotten = client.forget_track_subscription(rid);
    assert!(
        forgotten.is_some(),
        "Established 状態では forget_track_subscription は Some を返すこと"
    );
    let forgotten = forgotten.expect("Some であること");
    assert_eq!(forgotten.request_id, rid);
    // request_streams からも除去されている ((c) の冪等確認で検証)
    assert!(client.track_subscription(rid).is_none());

    // (c) 再度呼ぶと None (冪等)
    assert!(
        client.forget_track_subscription(rid).is_none(),
        "除去後の再呼び出しは None を返すこと (冪等)"
    );
}

// ─── Request ID 採番 ─────────────────────────────
//
// draft-ietf-moq-transport-21 §6.4.2.1 (Request ID):
// Client は偶数 (0, 2, 4, ...)、Server は奇数 (1, 3, 5, ...) を採番する。

/// Client の採番は偶数で、SUBSCRIBE 送信で +2 進む
///
/// draft-ietf-moq-transport-21 §6.4.2.1 (Request ID):
/// Client は 0 から始まり +2 ずつインクリメントする。
#[test]
fn client_request_id_advances_even() {
    let (mut client, _server) = establish_pair();

    // SUBSCRIBE 送信で request_id 0 が消費される
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(rid, 0, "最初の SUBSCRIBE は request_id 0 を使うこと");

    // 2 回目の送信で +2 進む
    let rid2 = client
        .send_subscribe(ns(&[b"live"]), b"cam2".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(rid2, 2, "2 回目の SUBSCRIBE は request_id 2 を使うこと");
}

/// Server の採番は奇数で、PUBLISH 送信で +2 進む
///
/// draft-ietf-moq-transport-21 §6.4.2.1 (Request ID):
/// Server は 1 から始まり +2 ずつインクリメントする。
#[test]
fn server_request_id_advances_odd() {
    let (_client, mut server) = establish_pair();

    // PUBLISH 送信で request_id 1 が消費される
    let rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            42,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(rid, 1, "最初の PUBLISH は request_id 1 を使うこと");

    // 2 回目の送信で +2 進む
    let rid2 = server
        .send_publish(
            ns(&[b"live"]),
            b"cam2".to_vec(),
            43,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(rid2, 3, "2 回目の PUBLISH は request_id 3 を使うこと");
}
