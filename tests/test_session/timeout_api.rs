use super::*;
use shiguredo_moqt::error::{
    SESSION_CONTROL_MESSAGE_TIMEOUT, SESSION_DATA_STREAM_TIMEOUT, SESSION_GOAWAY_TIMEOUT,
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
#[test]
fn timeout_priority_control_over_data_over_goaway() {
    // draft-ietf-moq-transport-21 §6.6 (Termination) はエラーコードを定義するのみで、
    // 複数 timeout の優先順位を規定していないため、実装で
    // control → data_stream → goaway の順に固定する。
    let (mut client, mut server, data_rid) = establish_subscribe_track(1);
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

    // data stream timeout を開始するため確立済み subscription の subgroup stream を開く
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
        .send_subgroup_header(DataStreamId(100), data_rid, &header)
        .expect("publisher が subgroup stream を開く");
    client
        .recv_data_stream_type(DataStreamId(100), 0x14)
        .expect("client が subgroup stream type を受理する");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(100), &header)
            .expect("client が subgroup header を受理する"),
        TrackDataAcceptance::Accepted
    );

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
#[test]
fn timeout_priority_data_over_goaway() {
    let (mut client, mut server, data_rid) = establish_subscribe_track(1);
    client.tick(0);
    client.set_control_message_timeout_ms(None);
    client.set_data_stream_timeout_ms(Some(20));

    // data stream timeout を開始するため確立済み subscription の subgroup stream を開く
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
        .send_subgroup_header(DataStreamId(100), data_rid, &header)
        .expect("publisher が subgroup stream を開く");
    client
        .recv_data_stream_type(DataStreamId(100), 0x14)
        .expect("client が subgroup stream type を受理する");
    assert_eq!(
        client
            .recv_subgroup_header(DataStreamId(100), &header)
            .expect("client が subgroup header を受理する"),
        TrackDataAcceptance::Accepted
    );

    // GOAWAY timeout を開始する
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
// draft-ietf-moq-transport-21 §11.5 (Padding): PADDING datagram の受信側 API。
// Established 状態では受理し、未 Established では PROTOCOL_VIOLATION を返す。

/// Established 状態で recv_padding_datagram は Ok(()) を返す
///
/// draft-ietf-moq-transport-21 §11.5 (Padding): Established セッションは
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
/// draft-ietf-moq-transport-21 §11.5 (Padding): セッション確立前の PADDING 受信は
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
// draft-ietf-moq-transport-21 §6.6 (Termination): DATA_STREAM_TIMEOUT は
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

    // ヘッダから 60ms 経過 (timeout 50ms 超過) だが、object 受信から 20ms なので閉じない
    client.tick(60);
    let mut closed = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::CloseSession(_) = ev {
            closed = true;
            break;
        }
    }
    assert!(
        !closed,
        "object 受信中のストリームは DATA_STREAM_TIMEOUT で閉じられないこと"
    );

    // object 受信から 50ms 以上経過 (tick 40 + 50 = 90) すると timeout で閉じる
    client.tick(91);
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::CloseSession(err) = ev {
            assert_eq!(
                err.code, SESSION_DATA_STREAM_TIMEOUT,
                "object 受信が止まると DATA_STREAM_TIMEOUT で閉じること"
            );
            got = true;
            break;
        }
    }
    assert!(
        got,
        "object 受信が止まると DATA_STREAM_TIMEOUT で CloseSession が発火すること"
    );
}

/// ヘッダ受信後に object が来ないストリームは timeout で閉じられる (既存動作の維持)
#[test]
fn subgroup_header_without_object_still_times_out() {
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
        if let SessionEvent::CloseSession(err) = ev {
            assert_eq!(
                err.code, SESSION_DATA_STREAM_TIMEOUT,
                "object が来ないストリームは DATA_STREAM_TIMEOUT で閉じること"
            );
            got = true;
            break;
        }
    }
    assert!(
        got,
        "ヘッダ受信後に object が来ないと DATA_STREAM_TIMEOUT で CloseSession が発火すること"
    );
}

/// padding stream は DATA_STREAM_TIMEOUT を誘発しない
///
/// draft-ietf-moq-transport-21 §11.5.1 (Padding Streams): padding stream は
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
