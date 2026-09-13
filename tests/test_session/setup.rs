use super::*;

/// Client と Server が相互に SETUP を交換して両方 Established に到達する
#[test]
fn client_server_handshake_full_cycle() {
    let mut client = Session::new_client(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let mut server = Session::new_server(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");

    let client_setup = take_send_control(&mut client);
    let server_setup = take_send_control(&mut server);

    server
        .recv_control(client_setup)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(server_setup)
        .expect("テストフィクスチャの前提条件を満たす");

    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(server.state(), SessionState::Established);
    assert_eq!(client.poll_event(), Some(SessionEvent::Established));
    assert_eq!(server.poll_event(), Some(SessionEvent::Established));
}

/// QUIC native で Client のみが AUTHORITY を使えば Server は Established に至る
#[test]
fn quic_client_authority_accepted_by_server() {
    let mut client_opts = SetupOptions::new();
    client_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORITY, // AUTHORITY
        value: SetupOptionValue::Bytes(b"example.com".to_vec()),
    });
    let mut client = Session::new_client(Transport::Quic, client_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut server = Session::new_server(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");

    let client_setup = take_send_control(&mut client);
    server
        .recv_control(client_setup)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.state(), SessionState::Established);
}

/// WebTransport で peer が AUTHORITY を送ってきたら INVALID_AUTHORITY でクローズ
#[test]
fn peer_authority_over_webtransport_rejected() {
    let mut server = Session::new_server(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let mut malformed = SetupOptions::new();
    malformed.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORITY, // AUTHORITY
        value: SetupOptionValue::Bytes(b"example.com".to_vec()),
    });
    let err = server
        .recv_control(ControlMessage::Setup(Setup { options: malformed }))
        .unwrap_err();
    assert_eq!(err.code, SESSION_INVALID_AUTHORITY);
    let e = drain_until_close(&mut server);
    match e {
        SessionEvent::CloseSession(err) => assert_eq!(err.code, SESSION_INVALID_AUTHORITY),
        _ => unreachable!(),
    }
}

/// WebTransport で自側 (Client) が AUTHORITY を送ろうとしたら INVALID_AUTHORITY
///
/// draft-ietf-moq-transport-21 §9.1.1 (AUTHORITY): "It MUST NOT be used by the
/// server, or when WebTransport is used."
/// validate_setup_role_transport 自側経路の単体テスト。
#[test]
fn local_authority_over_webtransport_rejected() {
    let mut options = SetupOptions::new();
    options.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORITY, // AUTHORITY
        value: SetupOptionValue::Bytes(b"example.com".to_vec()),
    });
    let err = Session::new_client(Transport::WebTransport, options).unwrap_err();
    assert_eq!(err.code, SESSION_INVALID_AUTHORITY);
}

/// WebTransport で自側 (Client) が PATH を送ろうとしたら INVALID_PATH
///
/// draft-ietf-moq-transport-21 §9.1.2 (PATH): "It MUST NOT be used by the
/// server, or when WebTransport is used."
/// validate_setup_role_transport 自側経路の単体テスト。
#[test]
fn local_path_over_webtransport_rejected() {
    let mut options = SetupOptions::new();
    options.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_PATH, // PATH
        value: SetupOptionValue::Bytes(b"/catalog".to_vec()),
    });
    let err = Session::new_client(Transport::WebTransport, options).unwrap_err();
    assert_eq!(err.code, SESSION_INVALID_PATH);
}

/// WebTransport で自側 (Server) が AUTHORITY を送ろうとしたら INVALID_AUTHORITY
///
/// draft-ietf-moq-transport-21 §9.1.1 (AUTHORITY): "It MUST NOT be used by the
/// server, or when WebTransport is used."
/// validate_setup_role_transport 自側 Server 経路の単体テスト。
#[test]
fn local_authority_over_webtransport_rejected_server() {
    let mut options = SetupOptions::new();
    options.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORITY, // AUTHORITY
        value: SetupOptionValue::Bytes(b"example.com".to_vec()),
    });
    let err = Session::new_server(Transport::WebTransport, options).unwrap_err();
    assert_eq!(err.code, SESSION_INVALID_AUTHORITY);
}

/// WebTransport で自側 (Server) が PATH を送ろうとしたら INVALID_PATH
///
/// draft-ietf-moq-transport-21 §9.1.2 (PATH): "It MUST NOT be used by the
/// server, or when WebTransport is used."
/// validate_setup_role_transport 自側 Server 経路の単体テスト。
#[test]
fn local_path_over_webtransport_rejected_server() {
    let mut options = SetupOptions::new();
    options.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_PATH, // PATH
        value: SetupOptionValue::Bytes(b"/catalog".to_vec()),
    });
    let err = Session::new_server(Transport::WebTransport, options).unwrap_err();
    assert_eq!(err.code, SESSION_INVALID_PATH);
}

/// Client 視点: Server から PATH 付きの SETUP が来たら INVALID_PATH (Server 送信禁止)
#[test]
fn peer_server_sending_path_rejected() {
    let mut client = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let mut malformed = SetupOptions::new();
    malformed.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_PATH, // PATH
        value: SetupOptionValue::Bytes(b"/evil".to_vec()),
    });
    let err = client
        .recv_control(ControlMessage::Setup(Setup { options: malformed }))
        .unwrap_err();
    assert_eq!(err.code, SESSION_INVALID_PATH);
}

/// Client 視点: Server から AUTHORITY 付きの SETUP が来たら INVALID_AUTHORITY (Server 送信禁止)
///
/// draft-ietf-moq-transport-21 §9.1.1 (AUTHORITY): AUTHORITY は Client のみ送信可能で、Server が送ると INVALID_AUTHORITY。
/// PATH 版 (`peer_server_sending_path_rejected`) と対をなす Server-only 検証経路
/// (`validate_setup_role_transport`) を踏む。
#[test]
fn peer_server_sending_authority_rejected() {
    let mut client = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let mut malformed = SetupOptions::new();
    malformed.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORITY, // AUTHORITY
        value: SetupOptionValue::Bytes(b"example.com".to_vec()),
    });
    let err = client
        .recv_control(ControlMessage::Setup(Setup { options: malformed }))
        .unwrap_err();
    assert_eq!(err.code, SESSION_INVALID_AUTHORITY);
}

/// Client が malformed PATH を送ろうとしたら MALFORMED_PATH
#[test]
fn local_malformed_path_rejected() {
    let mut options = SetupOptions::new();
    options.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_PATH,
        value: SetupOptionValue::Bytes(b"relative/path".to_vec()),
    });

    let err = Session::new_client(Transport::Quic, options).unwrap_err();
    assert_eq!(err.code, SESSION_MALFORMED_PATH);
}

/// peer から malformed PATH 付きの SETUP を受信したら MALFORMED_PATH
#[test]
fn peer_malformed_path_rejected() {
    let mut server = Session::new_server(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let mut malformed = SetupOptions::new();
    malformed.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_PATH,
        value: SetupOptionValue::Bytes(b"relative/path".to_vec()),
    });

    let err = server
        .recv_control(ControlMessage::Setup(Setup { options: malformed }))
        .unwrap_err();
    assert_eq!(err.code, SESSION_MALFORMED_PATH);
    let e = drain_until_close(&mut server);
    match e {
        SessionEvent::CloseSession(err) => assert_eq!(err.code, SESSION_MALFORMED_PATH),
        _ => unreachable!(),
    }
}

/// Client が malformed AUTHORITY を送ろうとしたら MALFORMED_AUTHORITY
#[test]
fn local_malformed_authority_rejected() {
    let mut options = SetupOptions::new();
    options.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORITY,
        value: SetupOptionValue::Bytes(b"bad host".to_vec()),
    });

    let err = Session::new_client(Transport::Quic, options).unwrap_err();
    assert_eq!(err.code, SESSION_MALFORMED_AUTHORITY);
}

/// peer から malformed AUTHORITY 付きの SETUP を受信したら MALFORMED_AUTHORITY
#[test]
fn peer_malformed_authority_rejected() {
    let mut server = Session::new_server(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let mut malformed = SetupOptions::new();
    malformed.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORITY,
        value: SetupOptionValue::Bytes(b"bad host".to_vec()),
    });

    let err = server
        .recv_control(ControlMessage::Setup(Setup { options: malformed }))
        .unwrap_err();
    assert_eq!(err.code, SESSION_MALFORMED_AUTHORITY);
    let e = drain_until_close(&mut server);
    match e {
        SessionEvent::CloseSession(err) => assert_eq!(err.code, SESSION_MALFORMED_AUTHORITY),
        _ => unreachable!(),
    }
}

/// AUTHORIZATION_TOKEN REGISTER を含む SETUP を受信すると peer cache に登録される
///
/// draft-ietf-moq-transport-21 §9.1.3 (MAX_AUTH_TOKEN_CACHE_SIZE): `peer_auth_token_cache` の上限は「自側」の MAX_AUTH_TOKEN_CACHE_SIZE。
#[test]
fn peer_authorization_token_register_populates_cache() {
    let mut server_opts = SetupOptions::new();
    server_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE, // MAX_AUTH_TOKEN_CACHE_SIZE (自側 = 保持上限)
        value: SetupOptionValue::VarInt(1024),
    });
    let mut server = Session::new_server(Transport::WebTransport, server_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut client_opts = SetupOptions::new();
    // client 側は MAX を送らない (default 0) 。自側 1024 で受信できることを確認する
    client_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORIZATION_TOKEN, // AUTHORIZATION_TOKEN
        value: SetupOptionValue::AuthorizationToken(AuthorizationToken::Register {
            alias: 7,
            token_type: 42,
            token_value: b"token".to_vec(),
        }),
    });
    server
        .recv_control(ControlMessage::Setup(Setup {
            options: client_opts,
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.state(), SessionState::Established);
    let cache = server.peer_auth_token_cache();
    assert_eq!(cache.max_size(), 1024);
    assert_eq!(cache.resolve(7), Some((42, &b"token"[..])));
}

/// 自側 MAX_AUTH_TOKEN_CACHE_SIZE を超える peer REGISTER は登録されないが、
/// セッションは閉じない (draft-ietf-moq-transport-21 §9.1.4 (AUTHORIZATION TOKEN): SETUP では USE_VALUE として扱う)。
/// peer 側の MAX が大きくても、保持判定は自側の MAX で行う。
#[test]
fn peer_authorization_token_register_over_self_limit_is_ignored() {
    let mut server_opts = SetupOptions::new();
    server_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE, // MAX_AUTH_TOKEN_CACHE_SIZE (自側 = 16)
        value: SetupOptionValue::VarInt(16), // 16 バイト枠 = token_value 0 バイトのみ収まる
    });
    let mut server = Session::new_server(Transport::WebTransport, server_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut client_opts = SetupOptions::new();
    client_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE, // MAX_AUTH_TOKEN_CACHE_SIZE (peer = 1024, 自側判定には無関係)
        value: SetupOptionValue::VarInt(1024),
    });
    client_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORIZATION_TOKEN,
        value: SetupOptionValue::AuthorizationToken(AuthorizationToken::Register {
            alias: 1,
            token_type: 1,
            token_value: b"more-than-zero".to_vec(),
        }),
    });
    server
        .recv_control(ControlMessage::Setup(Setup {
            options: client_opts,
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.state(), SessionState::Established);
    let cache = server.peer_auth_token_cache();
    assert_eq!(cache.max_size(), 16);
    assert!(cache.is_empty());
}

/// 自側 SETUP 内の REGISTER で alias が重複すると new() で拒否される
///
/// draft §8.9 (Authorization Token Compression): alias 重複の事前検出。
/// 自側 cache 自体は保持しないが、重複検証は new() 時に行う。
#[test]
fn local_authorization_token_register_duplicate_alias_rejected() {
    use shiguredo_moqt::error::SESSION_DUPLICATE_AUTH_TOKEN_ALIAS;
    let mut client_opts = SetupOptions::new();
    client_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORIZATION_TOKEN,
        value: SetupOptionValue::AuthorizationToken(AuthorizationToken::Register {
            alias: 5,
            token_type: 9,
            token_value: b"token".to_vec(),
        }),
    });
    client_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORIZATION_TOKEN,
        value: SetupOptionValue::AuthorizationToken(AuthorizationToken::Register {
            alias: 5,
            token_type: 9,
            token_value: b"other".to_vec(),
        }),
    });
    let err = Session::new_client(Transport::WebTransport, client_opts).unwrap_err();
    assert_eq!(err.code, SESSION_DUPLICATE_AUTH_TOKEN_ALIAS);
}

/// 明示的 close 後、さらに recv_control を呼んでも no-op
#[test]
fn recv_control_after_close_is_noop() {
    let mut s = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    s.close(SESSION_NO_ERROR, "done");
    // さらに SETUP を送っても Ok (CloseSession はもう 1 度は発行されない)
    let res = s.recv_control(ControlMessage::Setup(Setup {
        options: SetupOptions::new(),
    }));
    assert!(res.is_ok());
}

/// Established 中に peer control stream が FIN で閉じたら PROTOCOL_VIOLATION
#[test]
fn control_stream_fin_closes_established_session() {
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let (mut client, _) = establish_pair();
    let err = client
        .recv_control_stream_closed(RequestStreamEnd::Fin)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
    match drain_until_close(&mut client) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        _ => unreachable!(),
    }
}

/// Established 中に peer control stream が RESET_STREAM されたら PROTOCOL_VIOLATION
#[test]
fn control_stream_reset_closes_established_session() {
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let (mut client, _) = establish_pair();
    let err = client
        .recv_control_stream_closed(RequestStreamEnd::Reset {
            error_code: 42,
            reliable_size: None,
        })
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
    match drain_until_close(&mut client) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        _ => unreachable!(),
    }
}

/// SETUP 交換中でも peer control stream の終端は PROTOCOL_VIOLATION
#[test]
fn control_stream_close_in_local_setup_sent_closes_session() {
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let mut client = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let err = client
        .recv_control_stream_closed(RequestStreamEnd::Fin)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}

/// Phase 1 では SETUP 以外の制御メッセージは PROTOCOL_VIOLATION
#[test]
fn non_setup_control_message_is_violation_in_phase_one() {
    use shiguredo_moqt::message::Goaway;
    let mut s = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_send_control(&mut s);
    let err = s
        .recv_control(ControlMessage::Goaway(Goaway {
            new_session_uri: Vec::new(),
            timeout: 1000,
        }))
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

// ─── Phase 2: Request ID 管理 ─────────────────────────────

/// 指定 Request ID の SUBSCRIBE を組み立てる (parity / 重複検証のテスト用)
fn subscribe_message(request_id: u64) -> ControlMessage {
    use shiguredo_moqt::message::Subscribe;
    ControlMessage::Subscribe(Subscribe {
        request_id,
        track_namespace: ns(&[b"live"]),
        track_name: b"cam".to_vec(),
        parameters: MessageParameters::new(),
    })
}

/// 両端ハンドシェイク後、`recv_request` が parity に応じて peer の Request ID を受理し、
/// parity 違反は INVALID_REQUEST_ID で閉じる
#[test]
fn client_server_request_id_cross_validation() {
    let mut client = Session::new_client(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let mut server = Session::new_server(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let c_setup = take_send_control(&mut client);
    let s_setup = take_send_control(&mut server);
    server
        .recv_control(c_setup)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(s_setup)
        .expect("テストフィクスチャの前提条件を満たす");

    // Client は偶数を採番、Server は奇数を採番
    let c0 = client
        .next_local_request_id()
        .expect("テストフィクスチャの前提条件を満たす");
    let c1 = client
        .next_local_request_id()
        .expect("テストフィクスチャの前提条件を満たす");
    let s0 = server
        .next_local_request_id()
        .expect("テストフィクスチャの前提条件を満たす");
    let s1 = server
        .next_local_request_id()
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!((c0, c1), (0, 2));
    assert_eq!((s0, s1), (1, 3));

    // Client が Server の Request ID (奇数) を recv_request で受理する
    client
        .recv_request(subscribe_message(s0))
        .expect("parity の正しい peer request は受理されること");
    client
        .recv_request(subscribe_message(s1))
        .expect("parity の正しい peer request は受理されること");

    // Server が Client の Request ID (偶数) を recv_request で受理する
    server
        .recv_request(subscribe_message(c0))
        .expect("parity の正しい peer request は受理されること");
    server
        .recv_request(subscribe_message(c1))
        .expect("parity の正しい peer request は受理されること");
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(server.state(), SessionState::Established);

    // Client が Server の parity に反する Request ID (偶数) を受けると INVALID_REQUEST_ID
    // (6 は tracker 未受信の ID であり、parity 検証がなければ受理されてしまうため、
    //  検証の欠落を単独で検出できる)
    let err = client.recv_request(subscribe_message(6)).unwrap_err();
    assert_eq!(
        err.as_session_error().map(|e| e.code),
        Some(SESSION_INVALID_REQUEST_ID)
    );
    assert_eq!(client.state(), SessionState::Closing);
}

/// 重複 Request ID 受信は INVALID_REQUEST_ID で閉じる
#[test]
fn duplicate_peer_request_id_closes_session() {
    let mut client = establish_client();
    client
        .recv_request(subscribe_message(1))
        .expect("テストフィクスチャの前提条件を満たす");
    let err = client.recv_request(subscribe_message(1)).unwrap_err();
    assert_eq!(
        err.as_session_error().map(|e| e.code),
        Some(SESSION_INVALID_REQUEST_ID)
    );
    assert_eq!(client.state(), SessionState::Closing);
}

/// Established 状態でのみ next_local_request_id が動作する
#[test]
fn next_local_request_id_requires_established() {
    let mut client = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let err = client.next_local_request_id().unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // セッションは閉じない (純粋な API バリデーション)
    assert_eq!(client.state(), SessionState::LocalSetupSent);
}

// ─── peer_max_auth_token_cache_size (draft-ietf-moq-transport-21 §9.1.3 (MAX_AUTH_TOKEN_CACHE_SIZE)) ─────

/// peer SETUP 未受信時の `peer_max_auth_token_cache_size` はデフォルト 0 を返す
///
/// draft-ietf-moq-transport-21 §9.1.3 (MAX_AUTH_TOKEN_CACHE_SIZE): 未指定は 0。
#[test]
fn peer_max_auth_token_cache_size_defaults_to_zero_before_setup() {
    // 自側が MAX を宣言していても、peer SETUP 未受信なら 0 (自側 MAX へフォールバックしない)
    let mut client_opts = SetupOptions::new();
    client_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE, // MAX_AUTH_TOKEN_CACHE_SIZE (自側 = 1024)
        value: SetupOptionValue::VarInt(1024),
    });
    let client = Session::new_client(Transport::WebTransport, client_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(client.peer_max_auth_token_cache_size(), 0);
    assert_eq!(
        client.peer_auth_token_cache().max_size(),
        0,
        "peer SETUP 未受信時は peer cache の上限も 0 であること"
    );
}

/// peer SETUP の宣言値と未指定時の 0 が `peer_max_auth_token_cache_size` に反映されること
#[test]
fn peer_max_auth_token_cache_size_returns_peer_declared_value() {
    // 自側も別値 (1024) を宣言し、peer 宣言値 512 が返ることを交差検証する
    let mut client_opts = SetupOptions::new();
    client_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE, // MAX_AUTH_TOKEN_CACHE_SIZE (自側 = 1024)
        value: SetupOptionValue::VarInt(1024),
    });
    let mut client = Session::new_client(Transport::WebTransport, client_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut server_opts = SetupOptions::new();
    server_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE, // MAX_AUTH_TOKEN_CACHE_SIZE (peer = 512)
        value: SetupOptionValue::VarInt(512),
    });
    client
        .recv_control(ControlMessage::Setup(Setup {
            options: server_opts,
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(client.peer_max_auth_token_cache_size(), 512);

    // peer が明示的に 0 を宣言した場合も 0
    let mut client = Session::new_client(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let mut server_opts = SetupOptions::new();
    server_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE, // MAX_AUTH_TOKEN_CACHE_SIZE (peer = 0)
        value: SetupOptionValue::VarInt(0),
    });
    client
        .recv_control(ControlMessage::Setup(Setup {
            options: server_opts,
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(client.peer_max_auth_token_cache_size(), 0);

    // peer が option を送らない場合もデフォルト 0
    // (自側が MAX を宣言していても peer 未宣言なら 0 で、自側 MAX へフォールバックしない)
    let mut client_opts = SetupOptions::new();
    client_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE, // MAX_AUTH_TOKEN_CACHE_SIZE (自側 = 1024)
        value: SetupOptionValue::VarInt(1024),
    });
    let mut client = Session::new_client(Transport::WebTransport, client_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(ControlMessage::Setup(Setup {
            options: SetupOptions::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(client.peer_max_auth_token_cache_size(), 0);
}

/// `peer_max_auth_token_cache_size` が自側 MAX ではなく peer 宣言値を返すこと
///
/// `peer_auth_token_cache().max_size()` が自側 `MAX_AUTH_TOKEN_CACHE_SIZE` で、
/// peer の宣言値は `peer_max_auth_token_cache_size()` で取得できることを区別して固定する。
#[test]
fn peer_max_auth_token_cache_size_is_peer_value_not_self_limit() {
    // 自側 (server) MAX=1024 で peer (client) MAX=16 の SETUP を受信する
    let mut server_opts = SetupOptions::new();
    server_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE, // MAX_AUTH_TOKEN_CACHE_SIZE (自側 = 保持上限)
        value: SetupOptionValue::VarInt(1024),
    });
    let mut server = Session::new_server(Transport::WebTransport, server_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut client_opts = SetupOptions::new();
    client_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE, // MAX_AUTH_TOKEN_CACHE_SIZE (peer = 宣言値)
        value: SetupOptionValue::VarInt(16),
    });
    server
        .recv_control(ControlMessage::Setup(Setup {
            options: client_opts,
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server.peer_auth_token_cache().max_size(),
        1024,
        "peer_auth_token_cache の上限は自側 MAX であること"
    );
    assert_eq!(
        server.peer_max_auth_token_cache_size(),
        16,
        "peer の宣言した MAX は新アクセサで取得できること"
    );
}
