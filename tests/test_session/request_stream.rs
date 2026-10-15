use super::*;
use shiguredo_moqt::error::{
    REQUEST_GOING_AWAY, REQUEST_MALFORMED_AUTH_TOKEN, REQUEST_NOT_SUPPORTED,
    SESSION_AUTH_TOKEN_CACHE_OVERFLOW, SESSION_DUPLICATE_AUTH_TOKEN_ALIAS,
    SESSION_KEY_VALUE_FORMATTING_ERROR,
};
use shiguredo_moqt::message_parameter::PARAM_AUTHORIZATION_TOKEN;

// ─── request_streams lifecycle ─────────────────────
//
// draft-ietf-moq-transport-22 §6.3 (Session initialization) が規定する 7 種類の bidi request stream
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

/// draft-ietf-moq-transport-22 §6.3 (Session initialization): SETUP 完了前の request は session を落とさず
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

/// 定義済みパラメータ RENDEZVOUS_TIMEOUT (0x04) を含む SUBSCRIBE を受信しても
/// セッションを閉じない
///
/// draft-ietf-moq-transport-22 §9.20.6 (RENDEZVOUS TIMEOUT Parameter): "The
/// RENDEZVOUS_TIMEOUT parameter (Parameter Type 0x04) MAY appear in a SUBSCRIBE message."
/// 本ライブラリは値を解釈せず `Subscription` にも保持しない。
#[test]
fn recv_subscribe_with_rendezvous_timeout_is_accepted() {
    use shiguredo_moqt::message::{Subscribe, common::TrackNamespace};
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_RENDEZVOUS_TIMEOUT,
    };
    let (_client, mut server) = establish_pair();
    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter {
        param_type: PARAM_RENDEZVOUS_TIMEOUT,
        value: MessageParameterValue::VarInt(500),
    });
    server
        .recv_request(ControlMessage::Subscribe(Subscribe {
            request_id: 0,
            track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                .expect("正当な namespace である"),
            track_name: b"cam".to_vec(),
            parameters,
        }))
        .expect("定義済みパラメータを含む SUBSCRIBE は受理されること");
    assert_eq!(
        server.state(),
        SessionState::Established,
        "RENDEZVOUS_TIMEOUT の受信でセッションを閉じないこと"
    );
    assert!(
        server.subscription(0).is_some(),
        "subscriber 役として登録されること"
    );
}

/// 前縁より先の Request ID を大量に受信してもセッションを閉じない
///
/// draft-ietf-moq-transport-22 §6.4.2.1 (Request ID) が `INVALID_REQUEST_ID` での
/// セッション終了を MUST とするのは parity 違反と重複の 2 条件だけであり、
/// 未到達 Request ID の保持数に上限は設けない。
#[test]
fn recv_many_out_of_order_request_ids_keeps_session_open() {
    use shiguredo_moqt::message::{Subscribe, common::TrackNamespace};
    let (_client, mut server) = establish_pair();
    // Client 役の peer は偶数を採番する。0 を送らずに飛び ID を大量に送る。
    for i in 1..=4096u64 {
        server
            .recv_request(ControlMessage::Subscribe(Subscribe {
                request_id: i * 2,
                track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("正当な namespace である"),
                track_name: b"cam".to_vec(),
                parameters: MessageParameters::new(),
            }))
            .expect("未到達 Request ID の飛び ID は受理されること");
    }
    assert_eq!(
        server.state(),
        SessionState::Established,
        "未到達 Request ID の保持数でセッションを閉じないこと"
    );
    // 保持済み id の再受信は従来どおり重複として拒否される
    let err = server
        .recv_request(ControlMessage::Subscribe(Subscribe {
            request_id: 2,
            track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                .expect("正当な namespace である"),
            track_name: b"cam2".to_vec(),
            parameters: MessageParameters::new(),
        }))
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_INVALID_REQUEST_ID
    );
}

// ─── 未対応 request の本体 (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) ───
//
// draft-ietf-moq-transport-22 §9.14 (PUBLISH_NAMESPACE) / §9.15 (SUBSCRIBE_NAMESPACE) /
// §9.18 (SUBSCRIBE_TRACKS) の 3 種は共通して Request ID (vi64) → Track Namespace Prefix (..) →
// Number of Parameters (vi64) → Parameters (..) の順に直列化する。Track Namespace の encode API は
// crate 内に閉じているため、wire 表現を直接組み立てる。

/// Track Namespace Prefix "live" のワイヤ表現 (フィールド数 1 + フィールド長 4 + "live")
fn live_namespace_prefix() -> Vec<u8> {
    let mut bytes = vec![0x01, 0x04];
    bytes.extend_from_slice(b"live");
    bytes
}

/// 未対応 request の本体を共通構造で組み立てる
fn unsupported_request_body(request_id: u64, parameters: &MessageParameters) -> Vec<u8> {
    let mut encoded = Vec::new();
    parameters
        .encode(&mut encoded)
        .expect("テストフィクスチャの前提条件を満たす");
    unsupported_request_body_with_raw_parameters(request_id, &encoded)
}

/// パラメータブロックの生バイト列から未対応 request の本体を組み立てる
///
/// 重複トークンなど自側の `MessageParameters::encode` が拒否するパラメータは wire 表現を
/// 直接組み立てる必要があるため、生バイト列を受け取る経路を用意する。
fn unsupported_request_body_with_raw_parameters(request_id: u64, parameters: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    shiguredo_moqt::varint::encode(request_id, &mut body);
    body.extend_from_slice(&live_namespace_prefix());
    body.extend_from_slice(parameters);
    body
}

/// 未対応 request の `ControlMessage::Unsupported` を組み立てる
fn unsupported_request(
    type_id: u64,
    request_id: u64,
    parameters: &MessageParameters,
) -> ControlMessage {
    ControlMessage::Unsupported {
        type_id,
        request_id: Some(request_id),
        body: unsupported_request_body(request_id, parameters),
    }
}

/// AUTHORIZATION_TOKEN の REGISTER 1 件 (alias / Token Type 2 / Token Value "tok") を持つパラメータ
fn register_token_parameters(alias: u64) -> MessageParameters {
    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter {
        param_type: PARAM_AUTHORIZATION_TOKEN,
        value: MessageParameterValue::AuthorizationToken(AuthorizationToken::Register {
            alias,
            token_type: 2,
            token_value: b"tok".to_vec(),
        }),
    });
    parameters
}

/// 同一 (Token Type, Token Value) の REGISTER 2 件 (alias 1 / 2) のパラメータブロック
///
/// 構造自体は健全な Token であり、内容 (alias 解決後の Token Type と Token Value の組) が
/// 重複しているため §8.9 の「well-formed Token structure that is otherwise invalid」に該当する。
/// 重複トークンは自側の `MessageParameters::encode` が拒否するため、wire 表現を直接組み立てる。
fn duplicate_register_token_parameters() -> Vec<u8> {
    vec![
        0x02, // Number of Parameters
        0x03, 0x06, 0x01, 0x01, 0x02, b't', b'o', b'k', // REGISTER alias=1 / Type=2 / "tok"
        0x00, 0x06, 0x01, 0x02, 0x02, b't', b'o', b'k', // REGISTER alias=2 / Type=2 / "tok"
    ]
}

/// 送出された REQUEST_ERROR が指定のコードで FIN されていることを検証する
fn assert_request_error(server: &mut Session, request_id: u64, error_code: u64) {
    let (err_rid, message, fin) = take_send_on_stream_with_fin(server);
    assert_eq!(
        err_rid, request_id,
        "REQUEST_ERROR は request と同じ stream に載ること"
    );
    match message {
        ControlMessage::RequestError(err) => {
            assert_eq!(
                err.error_code, error_code,
                "REQUEST_ERROR のコードが期待どおりであること"
            );
        }
        other => panic!("REQUEST_ERROR を期待したが {other:?} を受け取った"),
    }
    assert!(fin, "拒否は最終応答として FIN すること");
}

/// 登録された alias の解決結果を検証する
fn assert_token_alias(server: &Session, alias: u64) {
    assert_eq!(
        server
            .peer_auth_token_cache()
            .resolve(alias)
            .map(|(token_type, value)| (token_type, value.to_vec())),
        Some((2, b"tok".to_vec())),
        "alias={alias} が token cache に登録されていること"
    );
}

/// 定義済みだが未実装の request は NOT_SUPPORTED で拒否し、セッションを維持する
///
/// draft-ietf-moq-transport-22 §1.6 (Modularity): "Limited endpoints SHOULD respond to any
/// unsupported messages with the appropriate NOT_SUPPORTED error code, rather than ignoring
/// them." §9 の Table 5 で request として届く型 (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE /
/// SUBSCRIBE_TRACKS) が対象である。
#[test]
fn unsupported_request_is_rejected_with_not_supported() {
    let (_client, mut server) = establish_pair();

    for (i, type_id) in [0x06u64, 0x50, 0x51].into_iter().enumerate() {
        let rid = (i as u64) * 2; // Client 役の採番 (偶数)
        let msg = unsupported_request(type_id, rid, &MessageParameters::new());
        let bytes = msg.encode().expect("encode できること");
        let (decoded, _) = ControlMessage::decode(&bytes).expect("decode できること");
        server
            .recv_request(decoded)
            .expect("未対応 request の拒否は Result::Ok であること");
        assert_eq!(
            server.state(),
            SessionState::Established,
            "未対応 request の受信でセッションを閉じないこと: {type_id:#x}"
        );
        // NOT_SUPPORTED (0x3) + FIN で拒否する
        let mut saw_reject = false;
        while let Some(e) = server.poll_event() {
            if let SessionEvent::SendOnStream {
                request_id,
                message: ControlMessage::RequestError(err),
                fin,
            } = e
            {
                assert_eq!(request_id, rid);
                assert_eq!(err.error_code, REQUEST_NOT_SUPPORTED);
                assert!(fin, "拒否は FIN で閉じること");
                saw_reject = true;
            }
        }
        assert!(
            saw_reject,
            "NOT_SUPPORTED の REQUEST_ERROR が発行されること"
        );
        // 後続の終端通知は no-op で吸収される
        server
            .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
            .expect("拒否済み request の終端通知は no-op であること");
        assert_eq!(
            server.state(),
            SessionState::Established,
            "終端通知でセッションを閉じないこと"
        );
    }
}

/// 未対応 request の parity 違反と重複は INVALID_REQUEST_ID でセッションを閉じる
#[test]
fn unsupported_request_parity_and_duplicate_close_session() {
    // parity 違反: Server 役の client に対して奇数 id を送る
    let (_client, mut server) = establish_pair();
    let err = server
        .recv_request(unsupported_request(0x06, 1, &MessageParameters::new()))
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_INVALID_REQUEST_ID
    );
    assert_eq!(server.state(), SessionState::Closing);

    // 重複: 同じ id を 2 回送る
    let (_client, mut server) = establish_pair();
    for _ in 0..2 {
        let result = server.recv_request(unsupported_request(0x50, 0, &MessageParameters::new()));
        if let Err(err) = result {
            assert_eq!(
                err.as_session_error()
                    .expect("Session エラーであること")
                    .code,
                SESSION_INVALID_REQUEST_ID
            );
            assert_eq!(server.state(), SessionState::Closing);
            return;
        }
        while server.poll_event().is_some() {}
    }
    panic!("重複した id が拒否されなかった");
}

/// 自側が control GOAWAY を送信済みなら未対応 request は GOING_AWAY で拒否する
#[test]
fn unsupported_request_after_local_goaway_returns_going_away() {
    let (_client, mut server) = establish_pair();
    // server が control GOAWAY を送信する
    server
        .send_goaway(Vec::new(), 10_000)
        .expect("GOAWAY の送信に成功すること");
    let _ = take_send_control(&mut server);

    let rid = 0u64;
    server
        .recv_request(unsupported_request(0x51, rid, &MessageParameters::new()))
        .expect("GOING_AWAY 拒否は Result::Ok であること");
    assert_request_error(&mut server, rid, REQUEST_GOING_AWAY);
}

/// 未対応 request の本体に載った REGISTER は、拒否後も §8.9 の MUST で alias が登録される
///
/// draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression): "The receiver of a
/// message carrying an Authorization Token with Alias Type REGISTER that does not result in a
/// Session error MUST register the Token Alias in the token cache, even if the message fails
/// for other reasons, including Unauthorized."
#[test]
fn unsupported_request_registers_auth_token_alias() {
    let (_client, mut server) = establish_pair_with_self_auth_cache(1024);
    server
        .recv_request(unsupported_request(0x06, 0, &register_token_parameters(1)))
        .expect("未対応 request の拒否は Result::Ok であること");
    assert_eq!(server.state(), SessionState::Established);
    assert_request_error(&mut server, 0, REQUEST_NOT_SUPPORTED);
    assert_token_alias(&server, 1);
}

/// control GOAWAY 送信済みの GOING_AWAY 拒否でも REGISTER は適用される
///
/// §8.9 の REGISTER MUST は「Session error にならない限り」適用されるため、GOAWAY (MAY) による
/// 拒否でも token cache への登録は行う。
#[test]
fn unsupported_request_after_goaway_registers_auth_token_alias() {
    let (_client, mut server) = establish_pair_with_self_auth_cache(1024);
    server
        .send_goaway(Vec::new(), 10_000)
        .expect("GOAWAY の送信に成功すること");
    let _ = take_send_control(&mut server);

    server
        .recv_request(unsupported_request(0x50, 0, &register_token_parameters(3)))
        .expect("GOING_AWAY 拒否は Result::Ok であること");
    assert_request_error(&mut server, 0, REQUEST_GOING_AWAY);
    assert_token_alias(&server, 3);
}

/// 未対応 request の重複トークンは MALFORMED_AUTH_TOKEN で拒否し、GOING_AWAY より優先する
///
/// draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) のメッセージ拒否は MUST、
/// §9.2 (GOAWAY) の新規 request 拒否は MAY のため、両方が成立する場合は MALFORMED_AUTH_TOKEN を
/// 返す。失敗したメッセージでも REGISTER は適用する (§8.9)。
#[test]
fn unsupported_request_with_duplicate_tokens_is_rejected_with_malformed_auth_token() {
    let (_client, mut server) = establish_pair_with_self_auth_cache(1024);
    server
        .send_goaway(Vec::new(), 10_000)
        .expect("GOAWAY の送信に成功すること");
    let _ = take_send_control(&mut server);

    server
        .recv_request(ControlMessage::Unsupported {
            type_id: 0x51,
            request_id: Some(0),
            body: unsupported_request_body_with_raw_parameters(
                0,
                &duplicate_register_token_parameters(),
            ),
        })
        .expect("メッセージ単位の reject は Ok (セッションは維持)");
    assert_eq!(server.state(), SessionState::Established);
    assert_request_error(&mut server, 0, REQUEST_MALFORMED_AUTH_TOKEN);
    assert_token_alias(&server, 1);
    assert_token_alias(&server, 2);
}

/// 未対応 request で登録済み alias を再登録すると DUPLICATE_AUTH_TOKEN_ALIAS でセッションを閉じる
///
/// draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression): "The receiver of a
/// message attempting to register an Alias which is already registered MUST close the Session
/// with DUPLICATE_AUTH_TOKEN_ALIAS."
#[test]
fn unsupported_request_reregistering_alias_closes_session() {
    let (_client, mut server) = establish_pair_with_self_auth_cache(1024);
    // 1 通目で alias=5 を登録する (未対応 request として NOT_SUPPORTED で拒否される)
    server
        .recv_request(unsupported_request(0x06, 0, &register_token_parameters(5)))
        .expect("1 通目の拒否は Result::Ok であること");
    let _ = take_send_on_stream_with_fin(&mut server);

    // 2 通目で同じ alias を再登録する
    let err = server
        .recv_request(unsupported_request(0x06, 2, &register_token_parameters(5)))
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_DUPLICATE_AUTH_TOKEN_ALIAS
    );
    assert_eq!(server.state(), SessionState::Closing);
}

/// 自側が MAX_AUTH_TOKEN_CACHE_SIZE を宣言していないセッションでは REGISTER で
/// AUTH_TOKEN_CACHE_OVERFLOW となり、REQUEST_ERROR は送らない
///
/// draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression): "If a registration
/// outside of SETUP is attempted that would cause this limit to be exceeded, the receiver MUST
/// terminate the Session with an AUTH_TOKEN_CACHE_OVERFLOW error."
#[test]
fn unsupported_request_register_without_cache_closes_session() {
    // establish_pair は MAX_AUTH_TOKEN_CACHE_SIZE を宣言しない (自側の上限は既定 0)
    let (_client, mut server) = establish_pair();
    let err = server
        .recv_request(unsupported_request(0x06, 0, &register_token_parameters(1)))
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_AUTH_TOKEN_CACHE_OVERFLOW
    );
    assert_eq!(server.state(), SessionState::Closing);
    while let Some(event) = server.poll_event() {
        assert!(
            !matches!(
                event,
                SessionEvent::SendOnStream {
                    message: ControlMessage::RequestError(_),
                    ..
                }
            ),
            "session error では REQUEST_ERROR を送らないこと"
        );
    }
}

/// 未対応 request の本体が共通構造に従わない場合は原因に応じた終了コードでセッションを閉じる
///
/// draft-ietf-moq-transport-22 §9 Table 5 (Length と本体の不一致) は PROTOCOL_VIOLATION、
/// §8.3 (Key-Value-Pair Structure) の Token 直列化の不整合は KEY_VALUE_FORMATTING_ERROR で閉じる。
#[test]
fn unsupported_request_body_decode_errors_close_session() {
    // 本体が Request ID で終わっている (Track Namespace Prefix とパラメータが無い)
    let (_client, mut server) = establish_pair();
    let err = server
        .recv_request(ControlMessage::Unsupported {
            type_id: 0x06,
            request_id: Some(0),
            body: vec![0x00],
        })
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );

    // Track Namespace Prefix の書式不正 (フィールド数 1 に対してフィールドが無い)
    let (_client, mut server) = establish_pair();
    let err = server
        .recv_request(ControlMessage::Unsupported {
            type_id: 0x06,
            request_id: Some(0),
            body: vec![0x00, 0x01],
        })
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );

    // パラメータブロックの後ろに余剰バイトがある (count=0 のあとに 1 バイト)
    let (_client, mut server) = establish_pair();
    let mut body = Vec::new();
    shiguredo_moqt::varint::encode(0, &mut body);
    body.extend_from_slice(&live_namespace_prefix());
    body.push(0x00);
    body.push(0x00);
    let err = server
        .recv_request(ControlMessage::Unsupported {
            type_id: 0x06,
            request_id: Some(0),
            body,
        })
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );

    // AUTHORIZATION_TOKEN の内部直列化が壊れている (DELETE なのに alias が無い)
    let (_client, mut server) = establish_pair();
    let mut body = Vec::new();
    shiguredo_moqt::varint::encode(0, &mut body);
    body.extend_from_slice(&live_namespace_prefix());
    let mut token_parameter = Vec::new();
    shiguredo_moqt::varint::encode(1, &mut token_parameter); // Number of Parameters
    shiguredo_moqt::varint::encode(PARAM_AUTHORIZATION_TOKEN, &mut token_parameter); // Parameter Type
    shiguredo_moqt::varint::encode(1, &mut token_parameter); // Token の Length
    token_parameter.push(0x00); // DELETE なのに alias が無い Token
    body.extend_from_slice(&token_parameter);
    let err = server
        .recv_request(ControlMessage::Unsupported {
            type_id: 0x06,
            request_id: Some(0),
            body,
        })
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_KEY_VALUE_FORMATTING_ERROR
    );

    // 本体の先頭の Request ID が `ControlMessage::Unsupported` の request_id と食い違う
    let (_client, mut server) = establish_pair();
    let err = server
        .recv_request(ControlMessage::Unsupported {
            type_id: 0x06,
            request_id: Some(0),
            body: unsupported_request_body(2, &MessageParameters::new()),
        })
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
}

/// 未対応 request でもメッセージ型ごとの許可パラメータ集合を検証する
///
/// draft-ietf-moq-transport-22 §9.20.1 (Parameter Scope): "If a parameter appears in some other
/// type of message, the receiving endpoint MUST close the connection with a PROTOCOL_VIOLATION."
#[test]
fn unsupported_request_rejects_out_of_scope_parameter() {
    use shiguredo_moqt::message_parameter::{
        PARAM_FILL_TIMEOUT, PARAM_GROUP_ORDER, PARAM_SUBSCRIBER_PRIORITY,
    };

    // PUBLISH_NAMESPACE が許可するのは AUTHORIZATION_TOKEN だけ (§9.14)
    let (_client, mut server) = establish_pair();
    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter {
        param_type: PARAM_SUBSCRIBER_PRIORITY,
        value: MessageParameterValue::Uint8(128),
    });
    let err = server
        .recv_request(unsupported_request(0x06, 0, &parameters))
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    assert_eq!(server.state(), SessionState::Closing);

    // SUBSCRIBE_TRACKS は GROUP_ORDER を許可する (§9.18) ため、同じ種類のパラメータでも拒否だけになる
    let (_client, mut server) = establish_pair();
    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(1),
    });
    server
        .recv_request(unsupported_request(0x51, 0, &parameters))
        .expect("許可されたパラメータを持つ未対応 request は Result::Ok であること");
    assert_eq!(server.state(), SessionState::Established);
    assert_request_error(&mut server, 0, REQUEST_NOT_SUPPORTED);

    // SUBSCRIBE_TRACKS でも FILL_TIMEOUT は許可されない
    // (draft-ietf-moq-transport-22 §9.20.5 (FILL TIMEOUT Parameter) は FETCH と
    // FILL_PARAMETERS 内だけに出現を認める)
    let (_client, mut server) = establish_pair();
    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter {
        param_type: PARAM_FILL_TIMEOUT,
        value: MessageParameterValue::VarInt(1000),
    });
    let err = server
        .recv_request(unsupported_request(0x51, 0, &parameters))
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    assert_eq!(server.state(), SessionState::Closing);
}

/// 未対応 request の重複トークンは NOT_SUPPORTED よりも MALFORMED_AUTH_TOKEN を優先する
///
/// draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) のメッセージ拒否は MUST、
/// §1.6 (Modularity) の NOT_SUPPORTED は SHOULD のため、両方が成立する場合は
/// MALFORMED_AUTH_TOKEN を返す。失敗したメッセージでも REGISTER は適用する (§8.9)。
#[test]
fn unsupported_request_with_duplicate_tokens_prefers_malformed_over_not_supported() {
    let (_client, mut server) = establish_pair_with_self_auth_cache(1024);
    server
        .recv_request(ControlMessage::Unsupported {
            type_id: 0x51,
            request_id: Some(0),
            body: unsupported_request_body_with_raw_parameters(
                0,
                &duplicate_register_token_parameters(),
            ),
        })
        .expect("メッセージ単位の reject は Ok (セッションは維持)");
    assert_eq!(server.state(), SessionState::Established);
    assert_request_error(&mut server, 0, REQUEST_MALFORMED_AUTH_TOKEN);
    assert_token_alias(&server, 1);
    assert_token_alias(&server, 2);
}

/// SUBSCRIBE_TRACKS は §3.6.2 の包括許可により SUBSCRIBE のパラメータも載せられる
///
/// draft-ietf-moq-transport-22 §3.6.2 (Parameters on SUBSCRIBE_TRACKS): "Any Parameter that can
/// be specified on a Subscription (ie: in SUBSCRIBE) is valid in SUBSCRIBE_TRACKS, unless
/// otherwise specified." 同節は LOCATION_FILTER と FILL_PARAMETERS を SUBSCRIBE_TRACKS に
/// 載せられると明記しており (§9.18 の列挙には無い)、スコープ違反としてセッションを閉じない。
#[test]
fn subscribe_tracks_with_subscribe_parameters_is_rejected_with_not_supported() {
    use shiguredo_moqt::message_parameter::{LocationFilter, PARAM_LOCATION_FILTER};
    let (_client, mut server) = establish_pair();
    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LocationFilter(LocationFilter::NoFilter),
    });
    server
        .recv_request(unsupported_request(0x51, 0, &parameters))
        .expect("§3.6.2 の包括許可に入るパラメータでセッションを閉じないこと");
    assert_eq!(server.state(), SessionState::Established);
    assert_request_error(&mut server, 0, REQUEST_NOT_SUPPORTED);
}

/// SUBSCRIBE_TRACKS は TRACK_PROPERTY_FILTER と他の許可パラメータを同時に載せられる
///
/// draft-ietf-moq-transport-22 §9.18 (SUBSCRIBE_TRACKS) は TRACK_PROPERTY_FILTER と
/// GROUP_ORDER 等を同じ列挙で許可し、§3.6.1 (Filtering SUBSCRIBE_TRACKS) は同メッセージの
/// Track Property Filter による絞り込みを定める。許可集合は §3.6.2 の包括許可との和集合なので、
/// 複数の型を組み合わせてもスコープ違反にしない。
#[test]
fn subscribe_tracks_allows_track_property_filter_with_other_parameters() {
    use shiguredo_moqt::message_parameter::{
        LocationFilter, PARAM_GROUP_ORDER, PARAM_LOCATION_FILTER, PARAM_TRACK_PROPERTY_FILTER,
    };

    // TRACK_PROPERTY_FILTER 単独 (§9.18 の列挙のうち SUBSCRIBE に無い型)
    let mut alone = MessageParameters::new();
    alone.push(MessageParameter {
        param_type: PARAM_TRACK_PROPERTY_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x00, 0x00, 0x01]),
    });
    // TRACK_PROPERTY_FILTER + LOCATION_FILTER + GROUP_ORDER (§9.18 と §3.6.2 の両方にまたがる)
    let mut combined = MessageParameters::new();
    combined.push(MessageParameter {
        param_type: PARAM_TRACK_PROPERTY_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x00, 0x00, 0x01]),
    });
    combined.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LocationFilter(LocationFilter::NoFilter),
    });
    combined.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(1),
    });

    for parameters in [alone, combined] {
        let (_client, mut server) = establish_pair();
        server
            .recv_request(unsupported_request(0x51, 0, &parameters))
            .expect("許可集合の和集合に入るパラメータでセッションを閉じないこと");
        assert_eq!(server.state(), SessionState::Established);
        assert_request_error(&mut server, 0, REQUEST_NOT_SUPPORTED);
    }
}

/// SUBSCRIBE_NAMESPACE の許可集合は AUTHORIZATION_TOKEN だけである
///
/// draft-ietf-moq-transport-22 §9.15 (SUBSCRIBE_NAMESPACE): "The only parameter that can appear
/// in a SUBSCRIBE_NAMESPACE is AUTHORIZATION_TOKEN." 許可されないパラメータの受信は §9.20.1
/// (Parameter Scope) の MUST により PROTOCOL_VIOLATION でセッションを閉じる。
#[test]
fn subscribe_namespace_parameter_scope_is_validated() {
    use shiguredo_moqt::message_parameter::PARAM_SUBSCRIBER_PRIORITY;

    // AUTHORIZATION_TOKEN は許可される (値は USE_VALUE で alias 登録を伴わない)
    let (_client, mut server) = establish_pair();
    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter {
        param_type: PARAM_AUTHORIZATION_TOKEN,
        value: MessageParameterValue::AuthorizationToken(AuthorizationToken::UseValue {
            token_type: 2,
            token_value: b"tok".to_vec(),
        }),
    });
    server
        .recv_request(unsupported_request(0x50, 0, &parameters))
        .expect("許可されたパラメータを持つ未対応 request は Result::Ok であること");
    assert_eq!(server.state(), SessionState::Established);
    assert_request_error(&mut server, 0, REQUEST_NOT_SUPPORTED);

    // SUBSCRIBER_PRIORITY は許可されない
    let (_client, mut server) = establish_pair();
    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter {
        param_type: PARAM_SUBSCRIBER_PRIORITY,
        value: MessageParameterValue::Uint8(128),
    });
    let err = server
        .recv_request(unsupported_request(0x50, 0, &parameters))
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    assert_eq!(server.state(), SessionState::Closing);
}

/// 未登録 alias を参照する USE_ALIAS を含む未対応 request は NOT_SUPPORTED で拒否し、
/// セッションを維持する
///
/// draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) の REGISTER 適用 MUST は
/// REGISTER だけを対象とし、DELETE / USE_ALIAS / USE_VALUE は失敗するメッセージのパラメータと
/// して適用しない (`apply_peer_message_auth_token_registers_only` の doc 参照)。GOAWAY 拒否の
/// 経路と同じ扱いであり、未登録 alias を理由にした UNKNOWN_AUTH_TOKEN_ALIAS でのセッション終了は
/// 起こさない。
#[test]
fn unsupported_request_with_unknown_alias_is_rejected_with_not_supported() {
    let (_client, mut server) = establish_pair_with_self_auth_cache(1024);
    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter {
        param_type: PARAM_AUTHORIZATION_TOKEN,
        value: MessageParameterValue::AuthorizationToken(AuthorizationToken::UseAlias { alias: 7 }),
    });
    server
        .recv_request(unsupported_request(0x06, 0, &parameters))
        .expect("未対応 request の拒否は Result::Ok であること");
    assert_eq!(server.state(), SessionState::Established);
    assert_request_error(&mut server, 0, REQUEST_NOT_SUPPORTED);
}

/// 応答専用の型を request stream の先頭で受けるとセッションを閉じる
///
/// draft-ietf-moq-transport-22 §9 Table 5 で NAMESPACE / NAMESPACE_DONE / PUBLISH_SKIPPED は
/// "First" を持たず、応答先の request も存在しない。`ControlMessage::decode` は応答専用の型に
/// Request ID を付けないが、公開 enum を手組みして付けた場合も §6.3 (Session initialization)
/// 違反として扱う。
#[test]
fn response_only_message_as_request_start_closes_session() {
    for type_id in [0x08u64, 0x0E, 0x0F] {
        let (_client, mut server) = establish_pair();
        let err = server
            .recv_request(ControlMessage::Unsupported {
                type_id,
                request_id: None,
                body: vec![0x01],
            })
            .unwrap_err();
        assert_eq!(
            err.as_session_error()
                .expect("Session エラーであること")
                .code,
            SESSION_PROTOCOL_VIOLATION,
            "応答専用の型は PROTOCOL_VIOLATION になること: {type_id:#x}"
        );
        assert_eq!(server.state(), SessionState::Closing);
    }

    // 公開 enum を手組みして Request ID を付けた場合も同じ扱いにする
    for type_id in [0x08u64, 0x0E, 0x0F] {
        let (_client, mut server) = establish_pair();
        let err = server
            .recv_request(ControlMessage::Unsupported {
                type_id,
                request_id: Some(0),
                body: unsupported_request_body(0, &MessageParameters::new()),
            })
            .unwrap_err();
        assert_eq!(
            err.as_session_error()
                .expect("Session エラーであること")
                .code,
            SESSION_PROTOCOL_VIOLATION,
            "応答専用の型に Request ID を付けても PROTOCOL_VIOLATION になること: {type_id:#x}"
        );
        assert_eq!(server.state(), SessionState::Closing);
    }
}

// ─── recv_request_stream_closed ──────────────────

/// 自側が requester のとき responder の FIN で subscription が Terminated に遷移し、
/// RequestTerminated(PeerStreamFin) が発火する
///
/// draft-ietf-moq-transport-22 §6.4.2.2 (Graceful Request Stream Closure): responder の FIN は
/// 「応答とそれに続くメッセージを送り終えた」通知であり、requester は送信方向を閉じる
/// (SHOULD)。cancel を定める §6.4.2.3 (Request Cancellation and Rejection) は RESET_STREAM /
/// STOP_SENDING であり、FIN は含まない。
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

    // responder (server) が送信方向を FIN で閉じたことを requester (client) が受ける
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

/// draft-ietf-moq-transport-22 §6.4.2.3 (Request Cancellation and Rejection) (request の cancel) + draft-ietf-moq-transport-22 §3.1 (Subscriptions) (PUBLISH_DONE/終端):
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
                error_code: Some(42),
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
    assert_eq!(got, Some(Some(42)));
}

/// アプリケーションエラーコード無しの RESET_STREAM は `None` として通知される
///
/// draft-ietf-webtrans-http3-16 §4.4 (Resetting Data Streams) は WT_APPLICATION_ERROR の
/// 範囲外のコードで reset された場合に「アプリケーションエラーコード無し」として
/// application へ届ける SHOULD を定める。WebTransport 経路でコードを remap できない場合に
/// `None` が伝わることを固定する。
#[test]
fn request_stream_reset_without_error_code_is_terminated_with_none() {
    use shiguredo_moqt::session::types::TerminationReason;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            12,
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
                error_code: None,
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
    assert_eq!(
        got,
        Some(None),
        "アプリケーションエラーコード無しが None として伝わること"
    );
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

// ─── requester の FIN と responder の応答 (draft-ietf-moq-transport-22 §6.4.2.2) ────
//
// §6.4.2.2 (Graceful Request Stream Closure): "A FIN only indicates that an endpoint will send
// no further messages in that direction; it is not a request cancellation." FIN を cancel と
// 定める §6.4.2.3 (Request Cancellation and Rejection) は cancel を RESET_STREAM /
// STOP_SENDING に限る。requester の FIN 自体は §6.4.2.2 の "A requester, with the exception of the
// sender of PUBLISH, MAY FIN immediately after sending a message if it will not send a
// REQUEST_UPDATE." で許容される。したがって requester が SUBSCRIBE / FETCH の送信直後に
// FIN しても request は失敗せず、responder は応答の MUST (§3.1 (Subscriptions) /
// §3.2 (Fetch)) を果たせなければならない。
// PUBLISH の送信者が例外である理由は、publisher が購読の終了時に PUBLISH_DONE を送ってから
// FIN する必要があるためである (§6.4.2.2 "the publisher of an Established subscription MUST send
// PUBLISH_DONE, before sending a FIN")。

/// requester が SUBSCRIBE の送信直後に FIN しても responder は SUBSCRIBE_OK を送れる
#[test]
fn responder_sends_subscribe_ok_after_requester_fin() {
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // requester (subscriber) が送信方向を FIN で閉じる
    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("requester の FIN は responder の request を終端しないこと");
    assert_eq!(
        server
            .subscription(rid)
            .expect("requester の FIN 後も responder 側の subscription は追跡され続けること")
            .state,
        SubscriptionState::Pending,
        "requester の FIN 後も responder 側の subscription は Pending を維持すること"
    );

    // responder の応答は FIN で塞がれない
    server
        .send_subscribe_ok(rid, 42, MessageParameters::new(), TrackProperties::new())
        .expect("requester の FIN 後でも SUBSCRIBE_OK を送れること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("responder からの SUBSCRIBE_OK を受信できること");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established
    );
}

/// requester が FETCH の送信直後に FIN しても responder は FETCH_OK を送れる
#[test]
fn responder_sends_fetch_ok_after_requester_fin() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::session::types::{FetchState, RequestStreamEnd};
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
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("requester の FIN は responder の request を終端しないこと");
    assert_eq!(
        server
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Pending,
        "requester の FIN で responder 側 fetch は終端しないこと"
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
        .expect("requester の FIN 後でも FETCH_OK を送れること");
    let (_, ok_msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert!(
        matches!(ok_msg, ControlMessage::FetchOk(_)),
        "FETCH_OK が SendOnStream として発行されること"
    );
    assert!(
        !fin,
        "FETCH_OK の送信では FIN しない (データストリームが続く)"
    );
    assert_eq!(
        server
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Established,
        "FETCH_OK の送信で fetch が Established になること"
    );
}

/// requester の FIN 後でも responder は REQUEST_ERROR を送れる
#[test]
fn responder_sends_request_error_after_requester_fin() {
    use shiguredo_moqt::message::ReasonPhrase;
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("requester の FIN は responder の request を終端しないこと");

    // 応答を送る前の FIN でも、request を拒否する REQUEST_ERROR の送信経路は塞がれない
    server
        .send_request_error(
            rid,
            shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST,
            0,
            ReasonPhrase::new("no such track").expect("正当な reason phrase である"),
            None,
        )
        .expect("requester の FIN 後でも REQUEST_ERROR を送れること");
    let (_, err_msg, fin) = take_send_on_stream_with_fin(&mut server);
    match err_msg {
        ControlMessage::RequestError(err) => {
            assert_eq!(
                err.error_code,
                shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST
            );
        }
        other => panic!("REQUEST_ERROR が期待されたが {other:?} だった"),
    }
    assert!(fin, "単独の REQUEST_ERROR は最終応答のため FIN されること");
    // REQUEST_ERROR の送信が自側の送信方向を閉じるため、peer FIN の記録はここで確定する
    let mut got = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind: RequestKind::Subscribe,
            reason: TerminationReason::PeerStreamFin,
        } = e
        {
            assert_eq!(request_id, rid);
            got = true;
        }
    }
    assert!(
        got,
        "REQUEST_ERROR の FIN で RequestTerminated(PeerStreamFin) が発行されること"
    );
    assert_eq!(
        server
            .subscription(rid)
            .expect("Terminated になった subscription は追跡されていること")
            .state,
        SubscriptionState::Terminated,
        "REQUEST_ERROR の送信で subscription が Terminated になること"
    );
}

/// Established の subscription で responder が requester の FIN を受けても Terminated に
/// ならず、publisher が PUBLISH_DONE を送れる
#[test]
fn established_subscription_survives_requester_fin() {
    use shiguredo_moqt::message::ReasonPhrase;
    use shiguredo_moqt::session::types::RequestStreamEnd;
    let (_client, mut server, rid) = establish_subscribe_track(1);

    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("requester の FIN は responder の subscription を終端しないこと");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established,
        "確立済み subscription は requester の FIN で Terminated にならないこと"
    );

    server
        .send_publish_done(
            rid,
            0x2,
            0,
            ReasonPhrase::new("ended").expect("正当な reason phrase である"),
        )
        .expect("requester の FIN 後でも PUBLISH_DONE を送れること");
    let (_, done_msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert!(matches!(done_msg, ControlMessage::PublishDone(_)));
    assert!(
        fin,
        "PUBLISH_DONE は subscription の最終メッセージのため FIN されること"
    );
}

/// peer FIN を受信済みのまま PUBLISH_DONE を送った時点で
/// RequestTerminated(PeerStreamFin) が発行される
#[test]
fn publish_done_after_requester_fin_terminates_request() {
    use shiguredo_moqt::message::ReasonPhrase;
    use shiguredo_moqt::session::types::RequestStreamEnd;
    use shiguredo_moqt::session::types::TerminationReason;
    let (_client, mut server, rid) = establish_subscribe_track(1);
    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("requester の FIN は responder の subscription を終端しないこと");

    // FIN を受けただけでは RequestTerminated を発行しない (応答を送る余地を残すため)
    let mut terminated_on_fin = false;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::RequestTerminated { .. }) {
            terminated_on_fin = true;
        }
    }
    assert!(
        !terminated_on_fin,
        "responder は requester の FIN だけでは RequestTerminated を発行しないこと"
    );

    // 自側の送信方向も閉じる PUBLISH_DONE の送信で request の終端が確定する
    server
        .send_publish_done(
            rid,
            0x2,
            0,
            ReasonPhrase::new("ended").expect("正当な reason phrase である"),
        )
        .expect("PUBLISH_DONE を送れること");
    let (_, msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert!(matches!(msg, ControlMessage::PublishDone(_)));
    assert!(fin, "PUBLISH_DONE は FIN で送られること");
    let mut got = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind: RequestKind::Subscribe,
            reason: TerminationReason::PeerStreamFin,
        } = e
        {
            assert_eq!(request_id, rid);
            got = true;
        }
    }
    assert!(
        got,
        "PUBLISH_DONE の FIN で RequestTerminated(PeerStreamFin) が発行されること"
    );
}

/// responder が先に PUBLISH_DONE を送り、後から requester の FIN を受けても終端する
///
/// draft-ietf-moq-transport-22 §6.4.2.2 (Graceful Request Stream Closure) の FIN は方向ごとの
/// 終端であり、bidi stream は方向ごとに独立に閉じる。したがってどちらの方向が先に閉じても、
/// 両方向が閉じた時点で request の終端が確定しなければならない。
#[test]
fn publish_done_before_requester_fin_terminates_request() {
    use shiguredo_moqt::message::ReasonPhrase;
    use shiguredo_moqt::session::types::RequestStreamEnd;
    use shiguredo_moqt::session::types::TerminationReason;
    let (_client, mut server, rid) = establish_subscribe_track(1);

    // 自側 (responder) が先に最終メッセージを送る
    server
        .send_publish_done(
            rid,
            0x2,
            0,
            ReasonPhrase::new("ended").expect("正当な reason phrase である"),
        )
        .expect("PUBLISH_DONE を送れること");
    let (_, done_msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert!(matches!(done_msg, ControlMessage::PublishDone(_)));
    assert!(fin, "PUBLISH_DONE は FIN で送られること");
    let mut terminated_on_send = false;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::RequestTerminated { .. }) {
            terminated_on_send = true;
        }
    }
    assert!(
        !terminated_on_send,
        "自側の送信方向を閉じただけでは RequestTerminated を発行しないこと"
    );

    // 後から requester の FIN が届く
    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("requester の FIN を受理すること");
    let mut got = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind: RequestKind::Subscribe,
            reason: TerminationReason::PeerStreamFin,
        } = e
        {
            assert_eq!(request_id, rid);
            got = true;
        }
    }
    assert!(
        got,
        "自側の FIN が先でも peer FIN の到着で RequestTerminated(PeerStreamFin) が発行されること"
    );
}

/// PUBLISH を送った側 (publisher 役) は peer subscriber の FIN を受けても購読を終端せず、
/// PUBLISH_DONE を送れる
///
/// draft-ietf-moq-transport-22 §3.1 (Subscriptions) は購読の終端を publisher の PUBLISH_DONE と
/// subscriber の STOP_SENDING に限る。§6.4.2.2 (Graceful Request Stream Closure) の FIN は
/// 方向ごとの終端であり cancel ではないため、subscriber の FIN で購読を終端すると publisher は
/// "the publisher of an Established subscription MUST send PUBLISH_DONE, before sending a FIN."
/// の MUST を果たせなくなる。
#[test]
fn publish_sender_survives_peer_fin_and_sends_publish_done() {
    use shiguredo_moqt::message::ReasonPhrase;
    use shiguredo_moqt::session::types::RequestStreamEnd;

    let (mut client, mut server) = establish_pair();
    let rid = establish_publish_sender_as_established(&mut client, &mut server, 11);

    // peer (subscriber) が REQUEST_UPDATE を送らない意思を示して FIN する
    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("peer の FIN を受理すること");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established,
        "PUBLISH を送った側は peer FIN で Terminated にならないこと"
    );

    // FIN だけでは終端イベントを発行しない (PUBLISH_DONE を送る余地を残す)
    let mut terminated = false;
    let mut finish_request_stream = false;
    while let Some(e) = server.poll_event() {
        match e {
            SessionEvent::RequestTerminated { .. } => terminated = true,
            SessionEvent::FinishRequestStream { .. } => finish_request_stream = true,
            _ => {}
        }
    }
    assert!(
        !terminated,
        "peer FIN だけでは RequestTerminated を発行しないこと"
    );
    assert!(
        !finish_request_stream,
        "PUBLISH の送信側は FIN を返す対象ではないこと"
    );

    // peer は FIN で「今後の REQUEST_UPDATE に応答しない」ことを示しているため、
    // 応答が来ないまま CONTROL_MESSAGE_TIMEOUT でセッションを閉じないよう送信を拒否する
    let err = server
        .send_request_update(rid, MessageParameters::new())
        .expect_err("peer FIN 後の REQUEST_UPDATE は拒否されること");
    assert_eq!(
        err.code,
        shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION,
        "拒否理由は SESSION_PROTOCOL_VIOLATION であること"
    );

    // PUBLISH_DONE の送信で両方向が閉じ、request の終端が確定する
    server
        .send_publish_done(
            rid,
            0x2,
            0,
            ReasonPhrase::new("ended").expect("正当な reason phrase である"),
        )
        .expect("peer FIN の後でも PUBLISH_DONE を送れること");
    let (_, done_msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert!(matches!(done_msg, ControlMessage::PublishDone(_)));
    assert!(fin, "PUBLISH_DONE は FIN で送られること");
    let mut got = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind: RequestKind::Publish,
            reason: TerminationReason::PeerStreamFin,
        } = e
        {
            assert_eq!(request_id, rid);
            got = true;
        }
    }
    assert!(
        got,
        "PUBLISH_DONE の送信時点で RequestTerminated(PeerStreamFin) が発行されること"
    );
}

/// PUBLISH を送った側が先に PUBLISH_DONE を送り、後から peer FIN を受けても終端する
///
/// draft-ietf-moq-transport-22 §6.4.2.2 (Graceful Request Stream Closure) の FIN は方向ごとの
/// 終端であるため、どちらの方向が先に閉じても両方向が閉じた時点で終端が確定しなければならない。
#[test]
fn publish_sender_publish_done_before_peer_fin_terminates_request() {
    use shiguredo_moqt::message::ReasonPhrase;
    use shiguredo_moqt::session::types::RequestStreamEnd;

    let (mut client, mut server) = establish_pair();
    let rid = establish_publish_sender_as_established(&mut client, &mut server, 12);

    // 自側 (publisher) が先に最終メッセージを送る
    server
        .send_publish_done(
            rid,
            0x2,
            0,
            ReasonPhrase::new("ended").expect("正当な reason phrase である"),
        )
        .expect("PUBLISH_DONE を送れること");
    let (_, done_msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert!(matches!(done_msg, ControlMessage::PublishDone(_)));
    assert!(fin, "PUBLISH_DONE は FIN で送られること");
    let mut terminated_on_send = false;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::RequestTerminated { .. }) {
            terminated_on_send = true;
        }
    }
    assert!(
        !terminated_on_send,
        "自側の送信方向を閉じただけでは RequestTerminated を発行しないこと"
    );

    // 後から peer (subscriber) の FIN が届く
    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("peer の FIN を受理すること");
    let mut got = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind: RequestKind::Publish,
            reason: TerminationReason::PeerStreamFin,
        } = e
        {
            assert_eq!(request_id, rid);
            got = true;
        }
    }
    assert!(
        got,
        "自側の FIN が先でも peer FIN の到着で RequestTerminated(PeerStreamFin) が発行されること"
    );
}

/// peer FIN で遅延している間に peer の RESET_STREAM (cancel) が届いたら即時に終端する
///
/// draft-ietf-moq-transport-22 §6.4.2.2 (Graceful Request Stream Closure) の FIN は方向ごとの
/// 終端であり cancel ではないため遅延するが、§6.4.2.3 (Request Cancellation and Rejection) の
/// cancel (RESET_STREAM / STOP_SENDING) は即時に終端する。
#[test]
fn publish_sender_peer_reset_during_deferral_terminates_request() {
    use shiguredo_moqt::session::types::RequestStreamEnd;

    let (mut client, mut server) = establish_pair();
    let rid = establish_publish_sender_as_established(&mut client, &mut server, 14);

    // peer FIN では終端しない
    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("peer の FIN を受理すること");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established,
        "peer FIN では終端しないこと"
    );

    // 遅延中に cancel (RESET_STREAM) が届いたら即時に終端する
    server
        .recv_request_stream_closed(
            rid,
            RequestStreamEnd::Reset {
                error_code: Some(42),
                reliable_size: None,
            },
        )
        .expect("peer の cancel を受理すること");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated,
        "cancel では遅延せず即時に終端すること"
    );
    let mut got = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind: RequestKind::Publish,
            reason: TerminationReason::PeerStreamReset { error_code },
        } = e
        {
            assert_eq!(request_id, rid);
            assert_eq!(error_code, Some(42));
            got = true;
        }
    }
    assert!(got, "RequestTerminated(PeerStreamReset) が発行されること");
}

/// PUBLISH を送った直後 (Pending(Publisher)) の peer FIN は要求未完として終端する
///
/// draft-ietf-moq-transport-22 §6.4.2.2 (Graceful Request Stream Closure): "An endpoint that
/// receives a FIN before all required messages have arrived treats the request as failed."
#[test]
fn publish_sender_pending_peer_fin_terminates_subscription() {
    use shiguredo_moqt::session::types::RequestStreamEnd;

    let (mut client, mut server) = establish_pair();
    let rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            13,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut server);
    client
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Pending,
        "PUBLISH_OK 未受信では Pending であること"
    );

    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("peer の FIN を受理すること");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated,
        "PUBLISH_OK 未受信の peer FIN は要求未完として終端すること"
    );
    let mut got = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind: RequestKind::Publish,
            reason: TerminationReason::PeerStreamFin,
        } = e
        {
            assert_eq!(request_id, rid);
            got = true;
        }
    }
    assert!(got, "RequestTerminated(PeerStreamFin) が発行されること");
}

/// FETCH の responder が peer FIN を受信済みのまま REQUEST_ERROR を送ると終端する
///
/// `Session::send_fetch_ok` は送信方向を閉じない (`fin: false`) ため、FETCH の responder が
/// 終端するのは単独の REQUEST_ERROR を最終メッセージとして送ったときである
/// (draft-ietf-moq-transport-22 §9.11 (FETCH) / §6.4.2.2 (Graceful Request Stream Closure))。
#[test]
fn fetch_responder_terminates_on_request_error_after_requester_fin() {
    use shiguredo_moqt::message::ReasonPhrase;
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::session::types::TerminationReason;
    use shiguredo_moqt::session::types::{FetchState, RequestStreamEnd};
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
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("requester の FIN は responder の request を終端しないこと");
    assert_eq!(
        server
            .fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        FetchState::Pending
    );

    server
        .send_request_error(
            rid,
            shiguredo_moqt::error::REQUEST_DOES_NOT_EXIST,
            0,
            ReasonPhrase::new("not found").expect("正当な reason phrase である"),
            None,
        )
        .expect("requester の FIN 後でも REQUEST_ERROR を送れること");
    let (_, _, fin) = take_send_on_stream_with_fin(&mut server);
    assert!(fin, "単独の REQUEST_ERROR は FIN されること");
    let mut got = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind: RequestKind::Fetch,
            reason: TerminationReason::PeerStreamFin,
        } = e
        {
            assert_eq!(request_id, rid);
            got = true;
        }
    }
    assert!(
        got,
        "FETCH responder も REQUEST_ERROR の FIN で RequestTerminated(PeerStreamFin) を発行すること"
    );
}

/// peer の RESET_STREAM では responder 側でも request が即時に終端する
///
/// RESET_STREAM は cancel である (draft-ietf-moq-transport-22 §6.4.2.3) ため、
/// FIN と異なり自側の役割にかかわらず request を終端する。
#[test]
fn responder_terminates_subscription_on_peer_reset() {
    use shiguredo_moqt::session::types::RequestStreamEnd;
    use shiguredo_moqt::session::types::TerminationReason;
    let (_client, mut server, rid) = establish_subscribe_track(1);
    server
        .recv_request_stream_closed(
            rid,
            RequestStreamEnd::Reset {
                error_code: Some(7),
                reliable_size: None,
            },
        )
        .expect("RESET_STREAM の通知に成功すること");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated,
        "peer の RESET_STREAM は responder 側でも Terminated にすること"
    );
    let mut got = None;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind: RequestKind::Subscribe,
            reason: TerminationReason::PeerStreamReset { error_code },
        } = e
        {
            assert_eq!(request_id, rid);
            got = Some(error_code);
        }
    }
    assert_eq!(got, Some(Some(7)));
}

/// 自側が requester のとき responder の FIN で FinishRequestStream が発行される
#[test]
fn requester_receives_responder_fin_and_finishes_request_stream() {
    use shiguredo_moqt::message::ReasonPhrase;
    use shiguredo_moqt::session::types::RequestStreamEnd;
    use shiguredo_moqt::session::types::TerminationReason;
    let (mut client, mut server, rid) = establish_subscribe_track(1);

    // responder (publisher) の FIN を requester が受ける
    server
        .send_publish_done(
            rid,
            0x2,
            0,
            ReasonPhrase::new("ended").expect("正当な reason phrase である"),
        )
        .expect("PUBLISH_DONE を送れること");
    let (_, done_msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert!(matches!(done_msg, ControlMessage::PublishDone(_)));
    assert!(fin, "PUBLISH_DONE は FIN で送られること");
    client
        .recv_stream_message(rid, done_msg)
        .expect("PUBLISH_DONE の受信に成功すること");
    client
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("responder の FIN の通知に成功すること");

    let mut finish_request_stream = false;
    let mut terminated = false;
    while let Some(e) = client.poll_event() {
        match e {
            SessionEvent::FinishRequestStream { request_id } => {
                assert_eq!(request_id, rid);
                finish_request_stream = true;
            }
            SessionEvent::RequestTerminated {
                request_id,
                kind: RequestKind::Subscribe,
                reason: TerminationReason::PeerStreamFin,
            } => {
                assert_eq!(request_id, rid);
                terminated = true;
            }
            _ => {}
        }
    }
    assert!(
        finish_request_stream,
        "requester は responder の FIN で送信方向を閉じること"
    );
    assert!(
        terminated,
        "requester 側は responder の FIN で request を終端すること"
    );
}

// ─── Pending(Subscriber) supersede by PUBLISH ────

/// draft-ietf-moq-transport-22 §3.1 (Subscriptions): 既存 Pending(Subscriber) subscription に対して
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

/// draft-ietf-moq-transport-22 §3.1: 同一 Track への複数同時 subscription が許可されたため、
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

    // 同一 Track への PUBLISH も受理される (複数 subscription 許可)
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
/// draft-ietf-moq-transport-22 §9.4.1 (Redirect Structure):
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
/// draft-ietf-moq-transport-22 §9.4.1: 非ゼロの場合のみ MUST close であるため、
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
/// draft-ietf-moq-transport-22 §9.4.1 の MUST close は Server 受信時のみに適用される。
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

/// Client の採番は偶数で、SUBSCRIBE 送信で +2 進む
///
/// draft-ietf-moq-transport-22 §6.4.2.1 (Request ID):
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
/// draft-ietf-moq-transport-22 §6.4.2.1 (Request ID):
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
