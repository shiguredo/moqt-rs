use super::*;
use shiguredo_moqt::error::{
    SESSION_CONTROL_MESSAGE_TIMEOUT, SESSION_DATA_STREAM_TIMEOUT, SESSION_GOAWAY_TIMEOUT,
    STREAM_CANCELLED,
};

/// Established 状態で send_padding_stream を呼ぶと SendPaddingStream が emit される
#[test]
fn send_padding_stream_emits_event_in_established() {
    let mut session = establish_client();
    session
        .send_padding_stream(100)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut got = false;
    while let Some(ev) = session.poll_event() {
        if let SessionEvent::SendPaddingStream { length } = ev {
            assert_eq!(length, 100, "PADDING length が指定値と一致すること");
            got = true;
            break;
        }
    }
    assert!(
        got,
        "Established 状態で SendPaddingStream イベントが発火すること"
    );
}

/// Established 前の状態で send_padding_stream を呼ぶと PROTOCOL_VIOLATION を返す
#[test]
fn send_padding_stream_before_established_returns_protocol_violation() {
    let mut session = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let err = session.send_padding_stream(100).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// Established 状態で send_padding_datagram を呼ぶと SendPaddingDatagram が emit される
#[test]
fn send_padding_datagram_emits_event_in_established() {
    let mut session = establish_client();
    session
        .send_padding_datagram(10)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut got = false;
    while let Some(ev) = session.poll_event() {
        if let SessionEvent::SendPaddingDatagram { length } = ev {
            assert_eq!(length, 10, "PADDING length が指定値と一致すること");
            got = true;
            break;
        }
    }
    assert!(
        got,
        "Established 状態で SendPaddingDatagram イベントが発火すること"
    );
}

/// Established 前の状態で send_padding_datagram を呼ぶと PROTOCOL_VIOLATION を返す
#[test]
fn send_padding_datagram_before_established_returns_protocol_violation() {
    let mut session = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let err = session.send_padding_datagram(10).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// length=0 の PADDING Stream でも正常に emit される
#[test]
fn send_padding_stream_length_zero_accepted() {
    let mut session = establish_client();
    session
        .send_padding_stream(0)
        .expect("length=0 の PADDING Stream は許可される");
    let mut got = false;
    while let Some(ev) = session.poll_event() {
        if matches!(ev, SessionEvent::SendPaddingStream { .. }) {
            got = true;
            break;
        }
    }
    assert!(
        got,
        "length=0 の PADDING Stream でも SendPaddingStream が発火すること"
    );
}

/// length=u64::MAX の PADDING Datagram でも正常に emit される
#[test]
fn send_padding_datagram_length_max_accepted() {
    let mut session = establish_client();
    session
        .send_padding_datagram(u64::MAX)
        .expect("length=u64::MAX の PADDING Datagram は許可される");
    let mut got = false;
    while let Some(ev) = session.poll_event() {
        if matches!(ev, SessionEvent::SendPaddingDatagram { .. }) {
            got = true;
            break;
        }
    }
    assert!(
        got,
        "length=u64::MAX の PADDING Datagram でも SendPaddingDatagram が発火すること"
    );
}

/// recv_control_stream_type に SETUP_STREAM_TYPE (0x2F00) を渡すと Ok(()) を返す
#[test]
fn recv_control_stream_type_setup_stream_type_accepted() {
    use shiguredo_moqt::stream::SETUP_STREAM_TYPE;
    let (mut client, _server) = establish_pair();
    client
        .recv_control_stream_type(SETUP_STREAM_TYPE)
        .expect("SETUP_STREAM_TYPE は受理される");
}

/// 3 種の timeout が同一 tick で満了した場合、control → data_stream → goaway の
/// 優先順位で決定的に 1 つのエラーコードが選ばれる
///
/// DATA_STREAM_TIMEOUT がセッションを閉じるのは FETCH stream が期限到達した場合である
/// (subgroup stream では STOP_SENDING の指示だけを発行する)。優先順位の検証には FETCH stream を使う。
#[test]
fn timeout_priority_control_over_data_over_goaway() {
    // draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) はエラーコードを定義する
    // のみで、複数 timeout の優先順位を規定していないため、実装で
    // control → data_stream → goaway の順に固定する。
    let (mut client, mut server, _data_rid) = establish_subscribe_track(1);
    client.tick(0);
    client.set_control_message_timeout_ms(Some(10));
    client.set_data_stream_timeout_ms(Some(20));

    // control message timeout を開始するため未応答の SUBSCRIBE を送信
    client
        .send_subscribe(ns(&[b"live2"]), b"cam2".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE 送信により control message deadline を開始する");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("server が SUBSCRIBE を受理する");

    // data stream timeout を開始するため FETCH 応答 stream を開く
    let rid = client
        .send_fetch(
            ns(&[b"live3"]),
            b"cam3".to_vec(),
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
        .expect("client が FETCH を送信する");
    let _ = take_send_request(&mut client);
    let fetch_stream_id = DataStreamId(101);
    client
        .recv_data_stream_type(fetch_stream_id, FETCH_HEADER_TYPE)
        .expect("client が fetch stream type を受理する");
    client
        .recv_fetch_header(fetch_stream_id, &FetchHeader { request_id: rid })
        .expect("client が fetch header を受理する");

    // GOAWAY timeout を開始する。 Established の subscription が drain blocker となる。
    client
        .send_goaway(Vec::new(), 100)
        .expect("client が GOAWAY を送信する");

    // control (10ms) / data (20ms) / goaway (100ms) がすべて満了
    client.tick(105);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::CloseSession(err) = ev {
            assert_eq!(
                err.code, SESSION_CONTROL_MESSAGE_TIMEOUT,
                "control message timeout が最優先で選ばれること"
            );
            got = true;
            break;
        }
    }
    assert!(
        got,
        "CONTROL_MESSAGE_TIMEOUT で CloseSession が発火すること"
    );
}

/// control timeout が発火しない場合、data_stream timeout が goaway timeout より優先される
///
/// DATA_STREAM_TIMEOUT がセッションを閉じるのは FETCH stream が期限到達した場合である
/// (subgroup stream では STOP_SENDING の指示だけを発行する)。優先順位の検証には FETCH stream を使う。
#[test]
fn timeout_priority_data_over_goaway() {
    let (mut client, _server, _data_rid) = establish_subscribe_track(1);
    client.tick(0);
    client.set_control_message_timeout_ms(None);
    client.set_data_stream_timeout_ms(Some(20));

    // data stream timeout を開始するため FETCH 応答 stream を開く
    let rid = client
        .send_fetch(
            ns(&[b"live2"]),
            b"cam2".to_vec(),
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
        .expect("client が FETCH を送信する");
    let _ = take_send_request(&mut client);
    let stream_id = DataStreamId(100);
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("client が fetch stream type を受理する");
    client
        .recv_fetch_header(stream_id, &FetchHeader { request_id: rid })
        .expect("client が fetch header を受理する");

    // GOAWAY timeout を開始する。Established の subscription が drain blocker となる。
    client
        .send_goaway(Vec::new(), 100)
        .expect("client が GOAWAY を送信する");

    // data (20ms) / goaway (100ms) が満了
    client.tick(105);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::CloseSession(err) = ev {
            assert_eq!(
                err.code, SESSION_DATA_STREAM_TIMEOUT,
                "data stream timeout が goaway timeout より優先されること"
            );
            got = true;
            break;
        }
    }
    assert!(got, "DATA_STREAM_TIMEOUT で CloseSession が発火すること");
}

/// control / data timeout が発火しない場合、goaway timeout が選ばれる
#[test]
fn timeout_priority_goaway_only() {
    let (mut client, _server, _rid) = establish_subscribe_track(1);
    client.tick(0);
    client.set_control_message_timeout_ms(None);
    client.set_data_stream_timeout_ms(None);

    // GOAWAY timeout を開始する
    client
        .send_goaway(Vec::new(), 100)
        .expect("client が GOAWAY を送信する");

    // goaway (100ms) が満了
    client.tick(105);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::CloseSession(err) = ev {
            assert_eq!(
                err.code, SESSION_GOAWAY_TIMEOUT,
                "goaway timeout が選ばれること"
            );
            got = true;
            break;
        }
    }
    assert!(got, "GOAWAY_TIMEOUT で CloseSession が発火すること");
}

/// recv_control_stream_type に 0x2F00 以外を渡すと PROTOCOL_VIOLATION を返す
#[test]
fn recv_control_stream_type_unknown_stream_type_rejected() {
    let (mut client, _server) = establish_pair();
    let err = client.recv_control_stream_type(0x1234).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// control_message_timeout_ms の set/get ラウンドトリップ
#[test]
fn control_message_timeout_ms_get_set_roundtrip() {
    let mut session = establish_client();
    assert_eq!(session.control_message_timeout_ms(), None);
    session.set_control_message_timeout_ms(Some(5000));
    assert_eq!(session.control_message_timeout_ms(), Some(5000));
    session.set_control_message_timeout_ms(None);
    assert_eq!(session.control_message_timeout_ms(), None);
}

/// data_stream_timeout_ms の set/get ラウンドトリップ
#[test]
fn data_stream_timeout_ms_get_set_roundtrip() {
    let mut session = establish_client();
    assert_eq!(session.data_stream_timeout_ms(), None);
    session.set_data_stream_timeout_ms(Some(3000));
    assert_eq!(session.data_stream_timeout_ms(), Some(3000));
    session.set_data_stream_timeout_ms(None);
    assert_eq!(session.data_stream_timeout_ms(), None);
}

/// CONTROL_MESSAGE_TIMEOUT の deadline 超過時に CloseSession が emit される
#[test]
fn control_message_timeout_tick_emits_close_session() {
    let (mut client, _server) = establish_pair();
    // まず tick で基準時刻を確定する
    client.tick(0);
    client.set_control_message_timeout_ms(Some(10));
    // SUBSCRIBE 送信で control message deadline を開始する
    client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, _sub_msg) = take_send_request(&mut client);
    // 100ms 経過: deadline 10ms を超過している
    client.tick(100);
    // CloseSession が CONTROL_MESSAGE_TIMEOUT で emit される
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::CloseSession(err) = ev {
            assert_eq!(
                err.code, SESSION_CONTROL_MESSAGE_TIMEOUT,
                "CONTROL_MESSAGE_TIMEOUT エラーコードであること"
            );
            got = true;
            break;
        }
    }
    assert!(
        got,
        "CONTROL_MESSAGE_TIMEOUT deadline 超過時に CloseSession が発火すること"
    );
}

// ─── recv_padding_datagram ─────────────────────────
//
// draft-ietf-moq-transport-22 §11.5 (Padding): PADDING datagram の受信側 API。
// Established 状態では受理し、未 Established では PROTOCOL_VIOLATION を返す。

/// Established 状態で recv_padding_datagram は Ok(()) を返す
///
/// draft-ietf-moq-transport-22 §11.5 (Padding): Established セッションは
/// PADDING datagram を受理できなければならない。
#[test]
fn recv_padding_datagram_accepts_in_established() {
    let (mut client, _server) = establish_pair();
    // Established 状態では PADDING datagram 受信は成功する
    client
        .recv_padding_datagram()
        .expect("Established 状態で recv_padding_datagram は Ok(()) を返すこと");
}

/// 未 Established 状態で recv_padding_datagram は PROTOCOL_VIOLATION を返す
///
/// draft-ietf-moq-transport-22 §11.5 (Padding): セッション確立前の PADDING 受信は
/// PROTOCOL_VIOLATION として拒否される。
#[test]
fn recv_padding_datagram_before_established_returns_protocol_violation() {
    let mut session = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    // SETUP 未完了 (LocalSetupSent) の状態では PROTOCOL_VIOLATION
    let err = session.recv_padding_datagram().unwrap_err();
    assert_eq!(
        err.code, SESSION_PROTOCOL_VIOLATION,
        "未 Established 状態では SESSION_PROTOCOL_VIOLATION であること"
    );
}

// ─── data stream activity: object 受信による timestamp 更新 ─────────────────────────
//
// draft-ietf-moq-transport-22 §12.2 (Session Termination Codes): DATA_STREAM_TIMEOUT は
// "an object header within a data stream" も activity に含む。
// Object 受信中のストリームは timeout で閉じられてはならない。

/// subgroup object 受信が data stream activity を更新し、timeout をリセットする
///
/// ヘッダ受信後に object を継続受信しているストリームは、
/// ヘッダからの経過時間が timeout を超えても閉じられない。
#[test]
fn subgroup_object_recv_resets_data_stream_timeout() {
    use shiguredo_moqt::stream::decoder::DecodedSubgroupObject;

    let (mut client, mut server, data_rid) = establish_subscribe_track(1);
    client.tick(0);
    client.set_data_stream_timeout_ms(Some(50));

    // subgroup stream を開く
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(200), data_rid, &header)
        .expect("publisher が subgroup stream を開く");
    client
        .recv_data_stream_type(DataStreamId(200), 0x14)
        .expect("client が subgroup stream type を受理する");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(200), &header)
            .expect("client が subgroup header を受理する"),
        TrackDataAcceptance::Accepted
    );

    // 40ms 経過時点で object を受信 (timeout 50ms 未満なのでまだ閉じない)
    client.tick(40);
    let object = DecodedSubgroupObject {
        object_id: 0,
        payload_length: 10,
        status: None,
        properties_bytes: None,
    };
    client
        .recv_subgroup_object(DataStreamId(200), &object)
        .expect("object 受信が成功すること");

    // ヘッダから 60ms 経過 (timeout 50ms 超過) だが、object 受信から 20ms なので期限に達しない
    client.tick(60);
    let mut timed_out = false;
    while let Some(ev) = client.poll_event() {
        if matches!(
            ev,
            SessionEvent::CloseSession(_) | SessionEvent::StopSendingDataStream { .. }
        ) {
            timed_out = true;
            break;
        }
    }
    assert!(
        !timed_out,
        "object 受信中のストリームは DATA_STREAM_TIMEOUT の発火対象にならないこと"
    );

    // object 受信から 50ms 以上経過 (tick 40 + 50 = 90) すると DATA_STREAM_TIMEOUT の期限に達する。
    // draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) は open な subgroup stream の
    // timeout の措置として STOP_SENDING を挙げるため、セッションは閉じず指示だけを発行する。
    client.tick(91);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::StopSendingDataStream {
            stream_id,
            error_code,
        } = ev
        {
            assert_eq!(
                stream_id,
                DataStreamId(200),
                "timeout した stream の STOP_SENDING が指示されること"
            );
            assert_eq!(
                error_code, STREAM_CANCELLED,
                "STOP_SENDING の error code は CANCELLED (§12.5) であること"
            );
            got = true;
            break;
        }
    }
    assert!(
        got,
        "object 受信が止まると StopSendingDataStream が発行されること"
    );
    assert_eq!(
        client.state(),
        SessionState::Established,
        "subgroup stream の DATA_STREAM_TIMEOUT でセッションが閉じないこと"
    );
}

/// ヘッダ受信後に object が来ない subgroup stream は STOP_SENDING の指示を受ける
///
/// draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) は open な subgroup stream で
/// 新しい object header を待って timeout した場合の措置として "it MAY send a STOP_SENDING on
/// that stream or terminate the subscription" を挙げる。セッションは閉じず、購読は生かしたまま
/// timeout した stream だけを止める。
#[test]
fn subgroup_header_without_object_emits_stop_sending() {
    let (mut client, mut server, data_rid) = establish_subscribe_track(1);
    client.tick(0);
    client.set_data_stream_timeout_ms(Some(30));

    // subgroup stream を開く (header のみ)
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(201), data_rid, &header)
        .expect("publisher が subgroup stream を開く");
    client
        .recv_data_stream_type(DataStreamId(201), 0x14)
        .expect("client が subgroup stream type を受理する");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(201), &header)
            .expect("client が subgroup header を受理する"),
        TrackDataAcceptance::Accepted
    );

    // object を受信せずに timeout 経過
    client.tick(31);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::StopSendingDataStream {
            stream_id,
            error_code,
        } = ev
        {
            assert_eq!(
                stream_id,
                DataStreamId(201),
                "timeout した stream の STOP_SENDING が指示されること"
            );
            assert_eq!(
                error_code, STREAM_CANCELLED,
                "STOP_SENDING の error code は CANCELLED (§12.5) であること"
            );
            got = true;
            break;
        }
    }
    assert!(
        got,
        "ヘッダ受信後に object が来ないと StopSendingDataStream が発行されること"
    );
    assert_eq!(
        client.state(),
        SessionState::Established,
        "subgroup stream の DATA_STREAM_TIMEOUT でセッションが閉じないこと"
    );
}

/// timeout した subgroup stream に遅延して届く Object / FIN / RESET は no-op で吸収される
///
/// draft-ietf-moq-transport-22 §11.3.2 (Closing Subgroup Streams) の STOP_SENDING に対して peer が
/// reset を返すまで、in-flight の Object や FIN が届くことがある。破棄対象として登録した stream への
/// 受信でセッションを閉じない。
#[test]
fn timed_out_subgroup_stream_absorbs_late_object_and_close() {
    let (mut client, mut server, data_rid) = establish_subscribe_track(1);
    client.tick(0);
    client.set_data_stream_timeout_ms(Some(30));

    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(202), data_rid, &header)
        .expect("publisher が subgroup stream を開く");
    client
        .recv_data_stream_type(DataStreamId(202), 0x14)
        .expect("client が subgroup stream type を受理する");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(202), &header)
            .expect("client が subgroup header を受理する"),
        TrackDataAcceptance::Accepted
    );

    // timeout させる (StopSendingDataStream が発行される)
    client.tick(31);
    let mut stop_sending = false;
    while let Some(ev) = client.poll_event() {
        if matches!(
            ev,
            SessionEvent::StopSendingDataStream {
                stream_id: DataStreamId(202),
                ..
            }
        ) {
            stop_sending = true;
            break;
        }
    }
    assert!(
        stop_sending,
        "timeout した stream の STOP_SENDING が指示されること"
    );

    // timeout 後に届く Object / FIN は破棄対象として no-op で吸収される
    let object = DecodedSubgroupObject {
        object_id: 0,
        payload_length: 10,
        status: None,
        properties_bytes: None,
    };
    assert_eq!(
        client
            .recv_subgroup_object(DataStreamId(202), &object)
            .expect("破棄対象 stream の Object 受信は no-op で吸収されること"),
        TrackDataAcceptance::Discarded,
        "破棄対象 stream の Object は Discarded として吸収されること"
    );
    client
        .recv_data_stream_closed(DataStreamId(202), RequestStreamEnd::Fin)
        .expect("破棄対象 stream の FIN は no-op で吸収されること");
    // peer が STOP_SENDING に応答して RESET_STREAM を返す経路
    client
        .recv_data_stream_closed(
            DataStreamId(202),
            RequestStreamEnd::Reset {
                error_code: Some(STREAM_CANCELLED),
                reliable_size: None,
            },
        )
        .expect("破棄対象 stream の RESET は no-op で吸収されること");
    assert_eq!(
        client.state(),
        SessionState::Established,
        "遅延到着でセッションが閉じないこと"
    );
}

/// timeout した subgroup stream があっても購読は生きており、次の Group の stream は受理される
///
/// draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) の STOP_SENDING は
/// "Resets and STOP_SENDING on SUBSCRIBE data streams have no impact on other Subgroups in the
/// Group or the subscription" (§11.3.2 (Closing Subgroup Streams)) とされる。
#[test]
fn subscription_continues_after_subgroup_stream_timeout() {
    let (mut client, mut server, data_rid) = establish_subscribe_track(1);
    client.tick(0);
    client.set_data_stream_timeout_ms(Some(30));

    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(203), data_rid, &header)
        .expect("publisher が subgroup stream を開く");
    client
        .recv_data_stream_type(DataStreamId(203), 0x14)
        .expect("client が subgroup stream type を受理する");
    client
        .recv_subgroup_header(DataStreamId(203), &header)
        .expect("client が subgroup header を受理する");

    // timeout させる
    client.tick(31);
    while client.poll_event().is_some() {}

    // 次の Group の subgroup stream は通常どおり受理される
    let next_header = SubgroupHeader {
        group_id: 2,
        ..header
    };
    server
        .send_subgroup_header(DataStreamId(204), data_rid, &next_header)
        .expect("publisher が次の Group の subgroup stream を開く");
    client
        .recv_data_stream_type(DataStreamId(204), 0x14)
        .expect("client が subgroup stream type を受理する");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(204), &next_header)
            .expect("client が subgroup header を受理する"),
        TrackDataAcceptance::Accepted,
        "timeout 後も購読は生きており次の Group の stream を受理すること"
    );
}

/// FETCH stream の DATA_STREAM_TIMEOUT は現行どおり 0x12 でセッションを閉じる
///
/// draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) の MAY は open な subgroup stream を
/// 対象にし、FETCH stream には代替措置を示していない。
#[test]
fn fetch_stream_timeout_still_closes_session() {
    let (mut client, _server) = establish_pair();
    client.tick(0);
    client.set_control_message_timeout_ms(None);
    client.set_data_stream_timeout_ms(Some(20));

    // data stream timeout を開始するため FETCH 応答 stream を開く
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
        .expect("client が FETCH を送信する");
    let _ = take_send_request(&mut client);
    let stream_id = DataStreamId(400);
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("client が fetch stream type を受理する");
    client
        .recv_fetch_header(stream_id, &FetchHeader { request_id: rid })
        .expect("client が fetch header を受理する");

    client.tick(21);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::CloseSession(err) = ev {
            assert_eq!(
                err.code, SESSION_DATA_STREAM_TIMEOUT,
                "FETCH stream は DATA_STREAM_TIMEOUT で閉じること"
            );
            got = true;
            break;
        }
    }
    assert!(
        got,
        "FETCH stream の timeout では CloseSession が発火すること"
    );
}

/// stream type だけ受信した subgroup stream の timeout も STOP_SENDING の指示になる
///
/// draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) の DATA_STREAM_TIMEOUT は
/// "fields of a stream header" も対象にする。SUBGROUP_HEADER を受信していない状態でも
/// セッションを閉じず、遅延して届く SUBGROUP_HEADER は破棄対象として吸収する。
#[test]
fn subgroup_stream_without_header_emits_stop_sending() {
    let (mut client, _server, _data_rid) = establish_subscribe_track(1);
    client.tick(0);
    client.set_data_stream_timeout_ms(Some(30));

    // stream type だけを受信する (SUBGROUP_HEADER は未受信)
    client
        .recv_data_stream_type(DataStreamId(210), 0x14)
        .expect("client が subgroup stream type を受理する");

    client.tick(31);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::StopSendingDataStream { stream_id, .. } = ev {
            assert_eq!(
                stream_id,
                DataStreamId(210),
                "timeout した stream の STOP_SENDING が指示されること"
            );
            got = true;
            break;
        }
    }
    assert!(
        got,
        "SUBGROUP_HEADER 未受信でも StopSendingDataStream が発行されること"
    );
    assert_eq!(
        client.state(),
        SessionState::Established,
        "subgroup stream の DATA_STREAM_TIMEOUT でセッションが閉じないこと"
    );

    // 遅延して届く SUBGROUP_HEADER は破棄対象として吸収される
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(210), &header)
            .expect("破棄対象 stream の SUBGROUP_HEADER は no-op で吸収されること"),
        TrackDataAcceptance::Discarded,
        "timeout 後の遅延 SUBGROUP_HEADER は破棄対象として吸収されること"
    );
    assert_eq!(
        client.state(),
        SessionState::Established,
        "遅延 SUBGROUP_HEADER でセッションが閉じないこと"
    );
}

/// stream type だけ受信した FETCH stream の timeout は 0x12 でセッションを閉じる
///
/// draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) の MAY は open な subgroup stream を
/// 対象にする。stream header の受信待ちであっても FETCH stream には代替措置が無い。
#[test]
fn fetch_stream_without_header_times_out_and_closes_session() {
    let (mut client, _server) = establish_pair();
    client.tick(0);
    client.set_control_message_timeout_ms(None);
    client.set_data_stream_timeout_ms(Some(20));

    // stream type だけを受信する (FETCH_HEADER は未受信)
    client
        .recv_data_stream_type(DataStreamId(410), FETCH_HEADER_TYPE)
        .expect("client が fetch stream type を受理する");

    client.tick(21);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::CloseSession(err) = ev {
            assert_eq!(
                err.code, SESSION_DATA_STREAM_TIMEOUT,
                "FETCH stream は DATA_STREAM_TIMEOUT で閉じること"
            );
            got = true;
            break;
        }
    }
    assert!(
        got,
        "FETCH stream の timeout では CloseSession が発火すること"
    );
}

/// timeout した subgroup stream は監視から外れ、再 tick で再発火しない
///
/// 破棄対象として会計から外れるため、同じ tick を繰り返しても `StopSendingDataStream` を
/// 再発行せず、セッションも閉じない。
#[test]
fn timed_out_subgroup_stream_is_excluded_from_monitoring() {
    let (mut client, mut server, data_rid) = establish_subscribe_track(1);
    client.tick(0);
    client.set_data_stream_timeout_ms(Some(30));

    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(220), data_rid, &header)
        .expect("publisher が subgroup stream を開く");
    client
        .recv_data_stream_type(DataStreamId(220), 0x14)
        .expect("client が subgroup stream type を受理する");
    client
        .recv_subgroup_header(DataStreamId(220), &header)
        .expect("client が subgroup header を受理する");

    // 1 回目の timeout で STOP_SENDING が指示される
    client.tick(31);
    let mut first = false;
    while let Some(ev) = client.poll_event() {
        if matches!(
            ev,
            SessionEvent::StopSendingDataStream {
                stream_id: DataStreamId(220),
                ..
            }
        ) {
            first = true;
            break;
        }
    }
    assert!(first, "1 回目の timeout で指示されること");

    // 2 回目以降の tick では再発火しない
    client.tick(100);
    let mut second = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::StopSendingDataStream { .. }) {
            second = true;
            break;
        }
    }
    assert!(!second, "監視から外れた stream は再発火しないこと");
    assert_eq!(
        client.state(),
        SessionState::Established,
        "再 tick でもセッションが閉じないこと"
    );
}

/// 同一 tick で subgroup stream と FETCH stream が期限到達した場合は 0x12 で閉じる
///
/// FETCH stream には draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) の代替措置が
/// 無いため、subgroup stream の STOP_SENDING より先にセッションクローズが決まる。
#[test]
fn fetch_stream_timeout_wins_over_subgroup_stream_in_same_tick() {
    let (mut client, mut server, data_rid) = establish_subscribe_track(1);
    client.tick(0);
    client.set_control_message_timeout_ms(None);
    client.set_data_stream_timeout_ms(Some(20));

    // subgroup stream (header のみ)
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(230), data_rid, &header)
        .expect("publisher が subgroup stream を開く");
    client
        .recv_data_stream_type(DataStreamId(230), 0x14)
        .expect("client が subgroup stream type を受理する");
    client
        .recv_subgroup_header(DataStreamId(230), &header)
        .expect("client が subgroup header を受理する");

    // FETCH 応答 stream
    let rid = client
        .send_fetch(
            ns(&[b"live4"]),
            b"cam4".to_vec(),
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
        .expect("client が FETCH を送信する");
    let _ = take_send_request(&mut client);
    client
        .recv_data_stream_type(DataStreamId(420), FETCH_HEADER_TYPE)
        .expect("client が fetch stream type を受理する");
    client
        .recv_fetch_header(DataStreamId(420), &FetchHeader { request_id: rid })
        .expect("client が fetch header を受理する");

    // 同一 tick で両方が期限到達する
    client.tick(21);
    let mut closed = false;
    let mut stop_sending = false;
    while let Some(ev) = client.poll_event() {
        match ev {
            SessionEvent::CloseSession(err) => {
                assert_eq!(
                    err.code, SESSION_DATA_STREAM_TIMEOUT,
                    "FETCH stream があれば 0x12 で閉じること"
                );
                closed = true;
            }
            SessionEvent::StopSendingDataStream { .. } => stop_sending = true,
            _ => {}
        }
    }
    assert!(closed, "FETCH stream が期限到達した tick で閉じること");
    assert!(
        !stop_sending,
        "セッションを閉じる tick では STOP_SENDING を指示しないこと"
    );
}

/// padding stream は DATA_STREAM_TIMEOUT を誘発しない
///
/// draft-ietf-moq-transport-22 §11.5.1 (Padding Streams): padding stream は
/// 後続データを持たないため、DATA_STREAM_TIMEOUT の監視対象外である。
/// peer が padding stream を開いたままにしても session は閉じられない。
#[test]
fn padding_stream_does_not_trigger_data_stream_timeout() {
    use shiguredo_moqt::stream::PADDING_STREAM_TYPE;

    let (mut client, _server) = establish_pair();
    client.tick(0);
    client.set_data_stream_timeout_ms(Some(20));

    // padding stream を開く
    client
        .recv_data_stream_type(DataStreamId(300), PADDING_STREAM_TYPE)
        .expect("padding stream type は受理される");

    // timeout (20ms) を十分に超える時間を経過させる
    client.tick(100);

    // DATA_STREAM_TIMEOUT で閉じられないこと
    let mut closed = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::CloseSession(err) = ev
            && err.code == SESSION_DATA_STREAM_TIMEOUT
        {
            closed = true;
            break;
        }
    }
    assert!(
        !closed,
        "padding stream は DATA_STREAM_TIMEOUT を誘発しないこと"
    );
}

/// timeout した subgroup stream は会計と tracker の停止記録まで後始末される
///
/// `discard_timed_out_subgroup_stream` は購読の `open_incoming_subgroup_count` を戻し
/// (戻さないと `cleanup_ready()` が false のままで `forget_subscription` が不能になる)、
/// `peer_subgroups` に STOP_SENDING を記録する。記録しないと tracker が `Open` のまま残り、
/// 同じ subgroup の再オープンが open 衝突の `PROTOCOL_VIOLATION` になる。
#[test]
fn timed_out_subgroup_stream_releases_accounting_and_stop_record() {
    let (mut client, mut server, data_rid) = establish_subscribe_track(1);
    client.tick(0);
    client.set_data_stream_timeout_ms(Some(30));

    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(270), data_rid, &header)
        .expect("publisher が subgroup stream を開く");
    client
        .recv_data_stream_type(DataStreamId(270), 0x14)
        .expect("client が subgroup stream type を受理する");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(270), &header)
            .expect("client が subgroup header を受理する"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .subscription(data_rid)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        1,
        "header 受理で open 中の受信 stream 数が 1 になること"
    );

    // 先頭 Object の Object Property で per-subgroup delivery timeout override を登録する
    assert_eq!(
        client
            .recv_subgroup_object(
                DataStreamId(270),
                &object_with_properties(0, delivery_timeout_properties(1_000)),
            )
            .expect("先頭 Object を受理すること"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client.subgroup_effective_delivery_timeout(data_rid, 1, 0),
        (None, Some(1_000)),
        "先頭 Object の Property で override が登録されること"
    );

    // timeout させる
    client.tick(31);
    while client.poll_event().is_some() {}

    // 会計が戻っている (戻らないと cleanup_ready が false のままで forget できない)
    assert_eq!(
        client
            .subscription(data_rid)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "timeout した stream の会計が戻ること"
    );
    // per-subgroup delivery timeout override も削除されている
    assert_eq!(
        client.subgroup_effective_delivery_timeout(data_rid, 1, 0),
        (None, None),
        "timeout した stream の override が削除されること"
    );

    // 同じ subgroup を新しい stream で開ける (tracker への停止記録が open 衝突を起こさない)
    client
        .recv_data_stream_type(DataStreamId(271), 0x14)
        .expect("client が subgroup stream type を受理する");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(271), &header)
            .expect("同じ subgroup の再オープンが PROTOCOL_VIOLATION にならないこと"),
        TrackDataAcceptance::Accepted,
        "timeout した subgroup の再オープンを受理すること"
    );
    assert_eq!(
        client.state(),
        SessionState::Established,
        "再オープンでセッションが閉じないこと"
    );
}

/// STOP_SENDING を送った stream は data stream timeout の対象から外れる
///
/// カタログの読み取りに失敗した stream を example が `send_data_stream_stop_sending` で Session の
/// 会計から外すため、その stream では timeout が発火しない (STOP_SENDING の指示も出ない)。
#[test]
fn stopped_subgroup_stream_is_excluded_from_data_stream_timeout() {
    let (mut client, mut server, data_rid) = establish_subscribe_track(1);
    client.tick(0);
    client.set_data_stream_timeout_ms(Some(30));

    // ヘッダだけ受信した subgroup stream を登録する
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(201), data_rid, &header)
        .expect("publisher が subgroup stream を開く");
    client
        .recv_data_stream_type(DataStreamId(201), 0x14)
        .expect("client が subgroup stream type を受理する");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(201), &header)
            .expect("client が subgroup header を受理する"),
        TrackDataAcceptance::Accepted
    );

    // 読み切れなかった stream を会計から外す (example の失敗経路と同じ)
    client
        .send_data_stream_stop_sending(DataStreamId(201))
        .expect("STOP_SENDING の送信は Err にならないこと");

    // object を受信しないまま timeout を経過させる。
    // 会計から外れているため DATA_STREAM_TIMEOUT の対象にならず、STOP_SENDING の指示も出ない
    // (会計から外す処理が壊れると、この stream が timeout して StopSendingDataStream が出る)。
    client.tick(31);
    let mut stop_sending = false;
    while let Some(ev) = client.poll_event() {
        match ev {
            SessionEvent::StopSendingDataStream { .. } => stop_sending = true,
            SessionEvent::CloseSession(err) => {
                assert_ne!(
                    err.code, SESSION_DATA_STREAM_TIMEOUT,
                    "STOP_SENDING を送った stream では DATA_STREAM_TIMEOUT が発火しないこと"
                );
            }
            _ => {}
        }
    }
    assert!(
        !stop_sending,
        "会計から外した stream へ STOP_SENDING を再指示しないこと"
    );
    assert_eq!(
        client.state(),
        SessionState::Established,
        "STOP_SENDING を送った stream のせいでセッションが閉じないこと"
    );
}

/// 期限到達の判定は `経過 >= timeout` である (境界値)
///
/// draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) の DATA_STREAM_TIMEOUT は
/// 期限そのものの時刻で到達する (`>=`)。`>` との取り違えを境界値で固定する。
#[test]
fn data_stream_timeout_fires_exactly_at_deadline() {
    let (mut client, mut server, data_rid) = establish_subscribe_track(1);
    client.tick(0);
    client.set_data_stream_timeout_ms(Some(30));

    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(240), data_rid, &header)
        .expect("publisher が subgroup stream を開く");
    client
        .recv_data_stream_type(DataStreamId(240), 0x14)
        .expect("client が subgroup stream type を受理する");
    client
        .recv_subgroup_header(DataStreamId(240), &header)
        .expect("client が subgroup header を受理する");

    // 期限の 1ms 前では発火しない
    client.tick(29);
    while let Some(ev) = client.poll_event() {
        assert!(
            !matches!(ev, SessionEvent::StopSendingDataStream { .. }),
            "期限前では STOP_SENDING を指示しないこと"
        );
    }

    // 期限ちょうど (経過 == timeout) で発火する
    client.tick(30);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if matches!(
            ev,
            SessionEvent::StopSendingDataStream {
                stream_id: DataStreamId(240),
                ..
            }
        ) {
            got = true;
            break;
        }
    }
    assert!(got, "経過 == timeout で STOP_SENDING が指示されること");
}

/// 同一 tick で複数の subgroup stream が期限到達した場合、stream_id 昇順に指示する
///
/// 複数 stream の同時 timeout でもイベント順を決定的にする (`Session::tick` の
/// `expired.sort_unstable()`)。
#[test]
fn multiple_subgroup_stream_timeouts_are_reported_in_stream_id_order() {
    let (mut client, mut server, data_rid) = establish_subscribe_track(1);
    client.tick(0);
    client.set_data_stream_timeout_ms(Some(30));

    // 登録順と stream_id の昇順を意図的に逆にする
    for (stream_id, group_id) in [(252u64, 2u64), (251, 1)] {
        let header = SubgroupHeader {
            track_alias: 1,
            group_id,
            subgroup_id: SubgroupIdMode::Explicit(0),
            publisher_priority: Some(1),
            has_properties: false,
            end_of_group: false,
            first_object: false,
        };
        server
            .send_subgroup_header(DataStreamId(stream_id), data_rid, &header)
            .expect("publisher が subgroup stream を開く");
        client
            .recv_data_stream_type(DataStreamId(stream_id), 0x14)
            .expect("client が subgroup stream type を受理する");
        client
            .recv_subgroup_header(DataStreamId(stream_id), &header)
            .expect("client が subgroup header を受理する");
    }

    client.tick(31);
    let mut order = Vec::new();
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::StopSendingDataStream { stream_id, .. } = ev {
            order.push(stream_id);
        }
    }
    assert_eq!(
        order,
        vec![DataStreamId(251), DataStreamId(252)],
        "期限到達した subgroup stream は stream_id 昇順に指示されること"
    );
}

/// timeout 後に同じ stream id の stream type を再通知しても監視に戻らない
///
/// 破棄対象の保持集合に入った id は `recv_data_stream_type` の再通知で再登録されず、
/// 再 tick でも `StopSendingDataStream` を再発行しない。
#[test]
fn timed_out_stream_id_is_not_monitored_again() {
    let (mut client, mut server, data_rid) = establish_subscribe_track(1);
    client.tick(0);
    client.set_data_stream_timeout_ms(Some(30));

    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(DataStreamId(260), data_rid, &header)
        .expect("publisher が subgroup stream を開く");
    client
        .recv_data_stream_type(DataStreamId(260), 0x14)
        .expect("client が subgroup stream type を受理する");
    client
        .recv_subgroup_header(DataStreamId(260), &header)
        .expect("client が subgroup header を受理する");

    client.tick(31);
    while client.poll_event().is_some() {}

    // 同じ stream id の stream type を再通知しても監視に戻らない
    client
        .recv_data_stream_type(DataStreamId(260), 0x14)
        .expect("破棄対象 stream の再通知は受理される");
    client.tick(100);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::StopSendingDataStream { .. }) {
            got = true;
            break;
        }
    }
    assert!(!got, "再通知した破棄対象 stream を監視に戻さないこと");
    assert_eq!(
        client.state(),
        SessionState::Established,
        "再通知と再 tick でセッションが閉じないこと"
    );
}
