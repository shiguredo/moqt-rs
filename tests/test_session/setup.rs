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
/// draft-ietf-moq-transport-23 §9.1.1 (AUTHORITY): "It MUST NOT be used by the
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
/// draft-ietf-moq-transport-23 §9.1.2 (PATH): "It MUST NOT be used by the
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
/// draft-ietf-moq-transport-23 §9.1.1 (AUTHORITY): "It MUST NOT be used by the
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
/// draft-ietf-moq-transport-23 §9.1.2 (PATH): "It MUST NOT be used by the
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
/// draft-ietf-moq-transport-23 §9.1.1 (AUTHORITY): AUTHORITY は Client のみ送信可能で、Server が送ると INVALID_AUTHORITY。
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
/// draft-ietf-moq-transport-23 §9.1.3 (MAX_AUTH_TOKEN_CACHE_SIZE): `peer_auth_token_cache` の上限は「自側」の MAX_AUTH_TOKEN_CACHE_SIZE。
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
/// セッションは閉じない (draft-ietf-moq-transport-23 §9.1.4 (AUTHORIZATION TOKEN): SETUP では USE_VALUE として扱う)。
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

/// 手組み SETUP の (Token Type, Token Value) の重複は MALFORMED_AUTH_TOKEN でセッションを閉じる
///
/// draft-ietf-moq-transport-23 §8.9 (Authorization Token Compression): "An Authorization Token
/// MAY be repeated within a message as long as the combination of Token Type and Token Value are
/// unique after resolving any aliases." 違反は "MUST reject that message with an
/// MALFORMED_AUTH_TOKEN error" であり、Request ID を持たない SETUP ではセッション終了
/// (§12.2 の MALFORMED_AUTH_TOKEN) になる。`Session::recv_control` に渡す手組みの `Setup` は
/// `SetupOptions::decode` の検証を経由しないため、Session 側で検証する。
/// 拒否したときは、登録しかけた alias も peer の SETUP も保持せず、Established にしない。
#[test]
fn peer_setup_duplicate_authorization_token_closes_with_malformed_auth_token() {
    use shiguredo_moqt::error::SESSION_MALFORMED_AUTH_TOKEN;

    let use_value = || AuthorizationToken::UseValue {
        token_type: 1,
        token_value: b"token".to_vec(),
    };
    let register = |alias| AuthorizationToken::Register {
        alias,
        token_type: 1,
        token_value: b"token".to_vec(),
    };
    let cases = [
        ("USE_VALUE の重複", vec![use_value(), use_value()]),
        ("値の同じ REGISTER 2 件", vec![register(1), register(2)]),
        (
            "値の同じ REGISTER と USE_VALUE",
            vec![register(1), use_value()],
        ),
    ];
    for (name, tokens) in cases {
        // REGISTER が登録され得るよう、自側の MAX_AUTH_TOKEN_CACHE_SIZE を十分に取る
        let mut server_opts = SetupOptions::new();
        server_opts.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE,
            value: SetupOptionValue::VarInt(1024),
        });
        let mut server = Session::new_server(Transport::WebTransport, server_opts)
            .expect("テストフィクスチャの前提条件を満たす");
        let mut client_opts = SetupOptions::new();
        for token in tokens {
            client_opts.push(SetupOption {
                option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORIZATION_TOKEN,
                value: SetupOptionValue::AuthorizationToken(token),
            });
        }

        let err = server
            .recv_control(ControlMessage::Setup(Setup {
                options: client_opts,
            }))
            .expect_err("重複トークンを含む SETUP は拒否すること");
        assert_eq!(
            err.code, SESSION_MALFORMED_AUTH_TOKEN,
            "{name}: MALFORMED_AUTH_TOKEN で閉じること"
        );
        assert_eq!(
            server.state(),
            SessionState::Closing,
            "{name}: セッションを閉じること"
        );
        assert!(
            server.peer_auth_token_cache().is_empty(),
            "{name}: 拒否した SETUP の alias を登録しないこと"
        );
        assert_eq!(
            server.peer_control_message_size_limit(),
            PeerControlMessageSizeLimit::BeforeSetup,
            "{name}: 拒否した SETUP を peer の SETUP として保持しないこと"
        );
        let events = drain_events(&mut server);
        assert!(
            events.iter().any(|e| matches!(
                e,
                SessionEvent::CloseSession(close) if close.code == SESSION_MALFORMED_AUTH_TOKEN
            )),
            "{name}: MALFORMED_AUTH_TOKEN の CloseSession を発行すること: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SessionEvent::Established)),
            "{name}: Established を発行しないこと: {events:?}"
        );
    }
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

/// Established 中に peer control stream が FIN で閉じたら CLOSED_CONTROL_STREAM
#[test]
fn control_stream_fin_closes_established_session() {
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let (mut client, _) = establish_pair();
    let err = client
        .recv_control_stream_closed(RequestStreamEnd::Fin)
        .unwrap_err();
    assert_eq!(err.code, SESSION_CLOSED_CONTROL_STREAM);
    assert_eq!(client.state(), SessionState::Closing);
    match drain_until_close(&mut client) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_CLOSED_CONTROL_STREAM),
        _ => unreachable!(),
    }
}

/// Established 中に peer control stream が RESET_STREAM されたら CLOSED_CONTROL_STREAM
#[test]
fn control_stream_reset_closes_established_session() {
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let (mut client, _) = establish_pair();
    let err = client
        .recv_control_stream_closed(RequestStreamEnd::Reset {
            error_code: Some(42),
            reliable_size: None,
        })
        .unwrap_err();
    assert_eq!(err.code, SESSION_CLOSED_CONTROL_STREAM);
    assert_eq!(client.state(), SessionState::Closing);
    match drain_until_close(&mut client) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_CLOSED_CONTROL_STREAM),
        _ => unreachable!(),
    }
}

/// SETUP 交換中でも peer control stream の終端は CLOSED_CONTROL_STREAM
#[test]
fn control_stream_close_in_local_setup_sent_closes_session() {
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let mut client = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let err = client
        .recv_control_stream_closed(RequestStreamEnd::Fin)
        .unwrap_err();
    assert_eq!(err.code, SESSION_CLOSED_CONTROL_STREAM);
    assert_eq!(client.state(), SessionState::Closing);
}

/// Phase 1 では SETUP 以外の制御メッセージは UNEXPECTED_MESSAGE
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
    assert_eq!(err.code, SESSION_UNEXPECTED_MESSAGE);
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

// ─── peer_max_auth_token_cache_size (draft-ietf-moq-transport-23 §9.1.3 (MAX_AUTH_TOKEN_CACHE_SIZE)) ─────

/// peer SETUP 未受信時の `peer_max_auth_token_cache_size` はデフォルト 0 を返す
///
/// draft-ietf-moq-transport-23 §9.1.3 (MAX_AUTH_TOKEN_CACHE_SIZE): 未指定は 0。
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

// ─── MAX_CONTROL_MESSAGE_SIZE (draft-ietf-moq-transport-23 §9.1.8) ─────

/// MAX_CONTROL_MESSAGE_SIZE を持つ SetupOptions を作る
fn options_with_max_control_message_size(value: u64) -> SetupOptions {
    let mut options = SetupOptions::new();
    options.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_CONTROL_MESSAGE_SIZE,
        value: SetupOptionValue::VarInt(value),
    });
    options
}

/// 自側が MAX_CONTROL_MESSAGE_SIZE を省略した場合の受信上限は既定値 65535
///
/// draft-ietf-moq-transport-23 §9.1.8 (MAX_CONTROL_MESSAGE_SIZE): 省略時の既定値は 65535。
#[test]
fn local_max_control_message_size_defaults_to_65535() {
    use shiguredo_moqt::parameter::DEFAULT_MAX_CONTROL_MESSAGE_SIZE;

    let client = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client.local_max_control_message_size(),
        DEFAULT_MAX_CONTROL_MESSAGE_SIZE
    );
    assert_eq!(client.local_max_control_message_size(), 65_535);
}

/// 自側が広告した MAX_CONTROL_MESSAGE_SIZE が受信上限になる
///
/// peer SETUP の受信前でも、自側の受信上限は自側の設定で確定する
/// (draft-ietf-moq-transport-23 §9.1.8 (MAX_CONTROL_MESSAGE_SIZE))。
#[test]
fn local_max_control_message_size_returns_advertised_value() {
    let client = Session::new_client(
        Transport::WebTransport,
        options_with_max_control_message_size(1_000_000),
    )
    .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(client.local_max_control_message_size(), 1_000_000);
}

/// 自側の MAX_CONTROL_MESSAGE_SIZE が 65535 未満なら Session を作れない
///
/// draft-ietf-moq-transport-23 §9.1.8 (MAX_CONTROL_MESSAGE_SIZE): "MUST be at least 65535."
/// 値が許容範囲外のフィールドに該当するため INVALID_FIELD (§12.2) を返す。
#[test]
fn local_max_control_message_size_below_minimum_is_rejected() {
    let err = Session::new_client(
        Transport::WebTransport,
        options_with_max_control_message_size(65_534),
    )
    .expect_err("65535 未満の自側設定は拒否されること");
    assert_eq!(err.code, SESSION_INVALID_FIELD);
}

/// 自側の MAX_CONTROL_MESSAGE_SIZE が最小値ちょうどなら受理される
#[test]
fn local_max_control_message_size_at_minimum_is_accepted() {
    let client = Session::new_client(
        Transport::WebTransport,
        options_with_max_control_message_size(65_535),
    )
    .expect("65535 ちょうどは受理されること");
    assert_eq!(client.local_max_control_message_size(), 65_535);
}

/// 自側の MAX_CONTROL_MESSAGE_SIZE を Bytes 値で設定すると拒否される
///
/// 型番号 0x09 の値は varint として解釈するため、生のバイト列では広告として成立しない。
/// 値の形式違反は INVALID_FIELD (§12.2) として扱う。
#[test]
fn local_max_control_message_size_with_bytes_value_is_rejected() {
    let mut options = SetupOptions::new();
    options.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_CONTROL_MESSAGE_SIZE,
        value: SetupOptionValue::Bytes(vec![0x01, 0x00, 0x00]),
    });
    let err = Session::new_client(Transport::WebTransport, options)
        .expect_err("Bytes 値の 0x09 は拒否されること");
    assert_eq!(err.code, SESSION_INVALID_FIELD);
}

/// 自側の MAX_CONTROL_MESSAGE_SIZE を重複して設定すると拒否される
///
/// draft-ietf-moq-transport-23 §9.1 (SETUP): "Senders MUST NOT repeat the same Option Type
/// in a message unless the option definition explicitly allows multiple instances."
#[test]
fn local_max_control_message_size_duplicate_is_rejected() {
    let mut options = options_with_max_control_message_size(65_535);
    options.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_CONTROL_MESSAGE_SIZE,
        value: SetupOptionValue::VarInt(1_000_000),
    });
    let err = Session::new_client(Transport::WebTransport, options)
        .expect_err("0x09 の重複は拒否されること");
    assert_eq!(err.code, SESSION_INVALID_FIELD);
}

/// peer SETUP 受信前の送信上限は BeforeSetup で既定値 65535
///
/// draft-ietf-moq-transport-23 §9.1.8 (MAX_CONTROL_MESSAGE_SIZE): "Until an endpoint receives
/// the peer's SETUP, it uses the default value, including for SETUP itself."
/// 交渉前と省略は同じ 65535 だが、区別できることを固定する。
#[test]
fn peer_control_message_size_limit_before_setup_is_default() {
    let client = Session::new_client(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let limit = client.peer_control_message_size_limit();
    assert_eq!(limit, PeerControlMessageSizeLimit::BeforeSetup);
    assert_eq!(limit.bytes(), 65_535);
}

/// peer SETUP が MAX_CONTROL_MESSAGE_SIZE を省略した場合は Omitted、広告した場合は Advertised
#[test]
fn peer_control_message_size_limit_tracks_peer_setup() {
    // 省略: 既定値 65535 (交渉前の BeforeSetup とは区別する)
    let mut client = Session::new_client(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(ControlMessage::Setup(Setup {
            options: SetupOptions::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    let limit = client.peer_control_message_size_limit();
    assert_eq!(limit, PeerControlMessageSizeLimit::Omitted);
    assert_eq!(limit.bytes(), 65_535);

    // 広告: peer の値がそのまま送信上限になる
    let mut client = Session::new_client(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(ControlMessage::Setup(Setup {
            options: options_with_max_control_message_size(2_000_000),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    let limit = client.peer_control_message_size_limit();
    assert_eq!(limit, PeerControlMessageSizeLimit::Advertised(2_000_000));
    assert_eq!(limit.bytes(), 2_000_000);
}

/// peer SETUP の MAX_CONTROL_MESSAGE_SIZE が 65535 未満なら INVALID_FIELD で閉じる
///
/// SetupOptions を直接組み立てた場合 (decode を通らない API 入力) も Session が検証する。
#[test]
fn peer_max_control_message_size_below_minimum_is_rejected() {
    let mut client = Session::new_client(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let err = client
        .recv_control(ControlMessage::Setup(Setup {
            options: options_with_max_control_message_size(65_534),
        }))
        .expect_err("65535 未満の peer 設定は拒否されること");
    assert_eq!(err.code, SESSION_INVALID_FIELD);
    // セッションは閉じる (PROTOCOL_VIOLATION ではなく原因を伝えるコード)
    match drain_until_close(&mut client) {
        SessionEvent::CloseSession(err) => assert_eq!(err.code, SESSION_INVALID_FIELD),
        other => panic!("CloseSession が期待されたが {other:?}"),
    }
}

/// peer SETUP の MAX_CONTROL_MESSAGE_SIZE が最小値ちょうどなら受理される
#[test]
fn peer_max_control_message_size_at_minimum_is_accepted() {
    let mut client = Session::new_client(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(ControlMessage::Setup(Setup {
            options: options_with_max_control_message_size(65_535),
        }))
        .expect("65535 ちょうどの peer 設定は受理されること");
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(
        client.peer_control_message_size_limit(),
        PeerControlMessageSizeLimit::Advertised(65_535)
    );
}

/// AUTHORITY の IPv6 リテラルを RFC 3986 §3.2.2 の `IPv6address` として検証する
///
/// draft-ietf-moq-transport-23 §9.1.1 (AUTHORITY): "If an AUTHORITY option does not
/// conform to these rules, the session MUST be closed with MALFORMED_AUTHORITY."
/// 自側 (new_client) と peer (recv_control) の両経路で同じ判定になることを固定する。
#[test]
fn malformed_ipv6_authority_rejected() {
    for literal in [
        // "::" の前後に空セグメントが付く
        b"[1:::2]".as_slice(),
        b"[1:2:3:4:5:6:7:8:]".as_slice(),
        b"[:1:2:3:4:5:6:7:8]".as_slice(),
    ] {
        // 自側: URI 検証に失敗するのでセッションを作る前に拒否する
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORITY,
            value: SetupOptionValue::Bytes(literal.to_vec()),
        });
        let err = Session::new_client(Transport::Quic, options)
            .expect_err("不正な IPv6 リテラルは new_client で拒否されること");
        assert_eq!(
            err.code, SESSION_MALFORMED_AUTHORITY,
            "{literal:?} は MALFORMED_AUTHORITY であること"
        );

        // peer: SETUP 受信時に同じ判定でセッションを閉じる
        let mut server = Session::new_server(Transport::Quic, SetupOptions::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let mut malformed = SetupOptions::new();
        malformed.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORITY,
            value: SetupOptionValue::Bytes(literal.to_vec()),
        });
        let err = server
            .recv_control(ControlMessage::Setup(Setup { options: malformed }))
            .expect_err("不正な IPv6 リテラルは recv_control で拒否されること");
        assert_eq!(err.code, SESSION_MALFORMED_AUTHORITY);
        match drain_until_close(&mut server) {
            SessionEvent::CloseSession(err) => assert_eq!(err.code, SESSION_MALFORMED_AUTHORITY),
            _ => unreachable!(),
        }
    }
}

/// 正当な IPv6 リテラルを含む AUTHORITY は受理される
///
/// IPv4-mapped 表記は 45 文字になり、`"::" ls32` 形式 (`::1.2.3.4`) も ABNF の
/// 正当な代替である。どちらも旧実装では誤って拒否していた。
#[test]
fn valid_ipv6_authority_accepted() {
    for literal in [
        b"[0000:0000:0000:0000:0000:ffff:255.255.255.255]".as_slice(),
        b"[::1.2.3.4]".as_slice(),
        b"[2001:db8::1]:4433".as_slice(),
    ] {
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORITY,
            value: SetupOptionValue::Bytes(literal.to_vec()),
        });
        let mut client = Session::new_client(Transport::Quic, options)
            .expect("正当な IPv6 リテラルは new_client で受理されること");
        let mut server = Session::new_server(Transport::Quic, SetupOptions::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let client_setup = take_send_control(&mut client);
        server
            .recv_control(client_setup)
            .expect("正当な IPv6 リテラルは recv_control で受理されること");
        assert_eq!(server.state(), SessionState::Established);
    }
}
