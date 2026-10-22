//! AUTHORIZATION_TOKEN のメッセージ単位 reject のテスト
//!
//! draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression): "The receiver of a
//! message containing a well-formed Token structure that is otherwise invalid MUST reject
//! that message with an MALFORMED_AUTH_TOKEN error."
//!
//! 構造は正しいが内容が不正なトークン (alias 解決後の (Token Type, Token Value) の重複) を
//! 含む request は、セッションを閉じずに REQUEST_ERROR (MALFORMED_AUTH_TOKEN) で拒否する。
//! codec が検出できる重複 (REGISTER / USE_VALUE) は自側の `MessageParameters::encode` が拒否
//! するが、alias 解決後の重複は encode では検出できない。そのため wire で受信したメッセージは
//! テスト側で固定したバイト列または手組みのメッセージで再現して注入する。

use super::*;
use shiguredo_moqt::error::{
    PUBLISH_DONE_UPDATE_FAILED, REQUEST_MALFORMED_AUTH_TOKEN, SESSION_UNKNOWN_AUTH_TOKEN_ALIAS,
    STREAM_CANCELLED,
};
use shiguredo_moqt::message::{Fetch, Publish, RequestUpdate, Subscribe, TrackStatus};
use shiguredo_moqt::message_parameter::PARAM_AUTHORIZATION_TOKEN;

/// 同一 (Token Type, Token Value) の REGISTER 2 件を持つパラメータ
///
/// 構造自体は健全な Token であり、内容 (alias 解決後の Token Type と Token Value の組) が
/// 重複しているため §8.9 の「well-formed Token structure that is otherwise invalid」に該当する。
fn duplicate_token_parameters() -> MessageParameters {
    let mut params = MessageParameters::new();
    for alias in [1, 2] {
        params.push(MessageParameter {
            param_type: PARAM_AUTHORIZATION_TOKEN,
            value: MessageParameterValue::AuthorizationToken(AuthorizationToken::Register {
                alias,
                token_type: 2,
                token_value: b"tok".to_vec(),
            }),
        });
    }
    params
}

/// 同一 (Token Type, Token Value) の USE_VALUE 2 件を持つパラメータ
///
/// alias 登録を伴わないため、REGISTER の MUST (失敗したメッセージでも alias を登録する) や
/// 登録済み alias の重複による session error の影響を受けずに、不正トークンと
/// セッション終了 MUST の優先順位だけを検証できる。
fn duplicate_use_value_parameters() -> MessageParameters {
    let mut params = MessageParameters::new();
    for _ in 0..2 {
        params.push(MessageParameter {
            param_type: PARAM_AUTHORIZATION_TOKEN,
            value: MessageParameterValue::AuthorizationToken(AuthorizationToken::UseValue {
                token_type: 2,
                token_value: b"tok".to_vec(),
            }),
        });
    }
    params
}

/// 送出されたイベントが REQUEST_ERROR (MALFORMED_AUTH_TOKEN) と FIN であることを検証する
fn assert_malformed_auth_token_rejection(server: &mut Session, request_id: u64) {
    let (err_rid, err_msg, fin) = take_send_on_stream_with_fin(server);
    assert_eq!(err_rid, request_id);
    match err_msg {
        ControlMessage::RequestError(err) => {
            assert_eq!(
                err.error_code, REQUEST_MALFORMED_AUTH_TOKEN,
                "不正トークンの reject は MALFORMED_AUTH_TOKEN であること"
            );
        }
        other => {
            panic!("REQUEST_ERROR(MALFORMED_AUTH_TOKEN) を期待したが {other:?} を受け取った")
        }
    }
    assert!(fin, "メッセージ単位の reject は最終応答として FIN すること");
}

/// 失敗したメッセージでも §8.9 の REGISTER MUST により alias が登録されていることを検証する
fn assert_registered_aliases(server: &Session) {
    for alias in [1, 2] {
        assert_eq!(
            server
                .peer_auth_token_cache()
                .resolve(alias)
                .map(|(token_type, value)| (token_type, value.to_vec())),
            Some((2, b"tok".to_vec())),
            "alias={alias} はメッセージの失敗後も登録されていること"
        );
    }
}

/// SUBSCRIBE の不正トークンは REQUEST_ERROR (MALFORMED_AUTH_TOKEN) で拒否し、セッションは閉じない
#[test]
fn subscribe_with_duplicate_tokens_is_rejected_with_malformed_auth_token() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let _ = take_send_request(&mut client);

    let msg = ControlMessage::Subscribe(Subscribe {
        request_id: rid,
        track_namespace: ns(&[b"live"]),
        track_name: b"cam".to_vec(),
        parameters: duplicate_token_parameters(),
    });
    server
        .recv_request(msg)
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert!(
        server.subscription(rid).is_none(),
        "拒否した SUBSCRIBE の subscription 状態を作らないこと"
    );
    assert_malformed_auth_token_rejection(&mut server, rid);
    assert_registered_aliases(&server);
}

/// FETCH の不正トークンは REQUEST_ERROR (MALFORMED_AUTH_TOKEN) で拒否し、セッションは閉じない
#[test]
fn fetch_with_duplicate_tokens_is_rejected_with_malformed_auth_token() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    let rid = client
        .send_fetch(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("FETCH の送信に成功すること");
    let _ = take_send_request(&mut client);

    let msg = ControlMessage::Fetch(Fetch {
        request_id: rid,
        track_namespace: ns(&[b"live"]),
        track_name: b"cam".to_vec(),
        parameters: duplicate_token_parameters(),
    });
    server
        .recv_request(msg)
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert!(
        server.fetch(rid).is_none(),
        "拒否した FETCH の fetch 状態を作らないこと"
    );
    assert_malformed_auth_token_rejection(&mut server, rid);
    assert_registered_aliases(&server);
}

/// PUBLISH の不正トークンは REQUEST_ERROR (MALFORMED_AUTH_TOKEN) で拒否し、セッションは閉じない
#[test]
fn publish_with_duplicate_tokens_is_rejected_with_malformed_auth_token() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            1,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("PUBLISH の送信に成功すること");
    let _ = take_send_request(&mut client);

    let msg = ControlMessage::Publish(Publish {
        request_id: rid,
        track_namespace: ns(&[b"live"]),
        track_name: b"cam".to_vec(),
        track_alias: 1,
        parameters: duplicate_token_parameters(),
        track_properties: TrackProperties::new(),
    });
    server
        .recv_request(msg)
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert!(
        server.subscription(rid).is_none(),
        "拒否した PUBLISH の subscription 状態を作らないこと"
    );
    assert_malformed_auth_token_rejection(&mut server, rid);
    assert_registered_aliases(&server);
}

/// TRACK_STATUS の不正トークンは REQUEST_ERROR (MALFORMED_AUTH_TOKEN) で拒否する
///
/// draft-ietf-moq-transport-22 §9.13 (TRACK_STATUS): 受信側は SUBSCRIBE と同一に扱うため、
/// 拒否経路も `accept_incoming_track_request` を共有する。
#[test]
fn track_status_with_duplicate_tokens_is_rejected_with_malformed_auth_token() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let _ = take_send_request(&mut client);

    let msg = ControlMessage::TrackStatus(TrackStatus {
        request_id: rid,
        track_namespace: ns(&[b"live"]),
        track_name: b"cam".to_vec(),
        parameters: duplicate_token_parameters(),
    });
    server
        .recv_request(msg)
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert!(
        server.track_status_request(rid).is_none(),
        "拒否した TRACK_STATUS の状態を作らないこと"
    );
    assert_malformed_auth_token_rejection(&mut server, rid);
    assert_registered_aliases(&server);
}

/// REQUEST_UPDATE の不正トークンは REQUEST_ERROR (MALFORMED_AUTH_TOKEN) で拒否する
///
/// REQUEST_UPDATE は既存の request stream 上を流れるため、拒否してもセッションは閉じない。
/// 自側 publisher の Established subscription では draft-ietf-moq-transport-22 §9.5.1
/// (Updating Subscriptions) が「REQUEST_UPDATE が失敗したら PUBLISH_DONE(UPDATE_FAILED) で
/// subscription を終端する」ことを MUST とするため、拒否後は subscription が Terminated に
/// なる (ワイヤ順序は REQUEST_ERROR → PUBLISH_DONE)。
#[test]
fn request_update_with_duplicate_tokens_is_rejected_with_malformed_auth_token() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    let rid = send_and_recv_subscribe_with_parameters(
        &mut client,
        &mut server,
        &[b"live"],
        b"cam",
        MessageParameters::new(),
    );
    complete_subscribe(&mut client, &mut server, rid, 1);

    // REQUEST_UPDATE は自側の Request ID を新規に消費する (送信先は request stream の rid)
    let update = ControlMessage::RequestUpdate(RequestUpdate {
        request_id: rid + 4,
        parameters: duplicate_token_parameters(),
    });
    server
        .recv_stream_message(rid, update)
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert_eq!(
        server
            .subscription(rid)
            .expect("subscription は存在すること")
            .state,
        SubscriptionState::Terminated,
        "自側 publisher の REQUEST_UPDATE 拒否は §9.5.1 により subscription を終端すること"
    );

    let (err_rid, err_msg, err_fin) = take_send_on_stream_with_fin(&mut server);
    assert_eq!(err_rid, rid);
    match err_msg {
        ControlMessage::RequestError(err) => {
            assert_eq!(
                err.error_code, REQUEST_MALFORMED_AUTH_TOKEN,
                "不正トークンの reject は MALFORMED_AUTH_TOKEN であること"
            );
        }
        other => {
            panic!("REQUEST_ERROR(MALFORMED_AUTH_TOKEN) を期待したが {other:?} を受け取った")
        }
    }
    assert!(
        !err_fin,
        "PUBLISH_DONE が続くため REQUEST_ERROR では FIN しないこと"
    );

    let (done_rid, done_msg, done_fin) = take_send_on_stream_with_fin(&mut server);
    assert_eq!(done_rid, rid);
    match done_msg {
        ControlMessage::PublishDone(done) => {
            assert_eq!(
                done.status_code, PUBLISH_DONE_UPDATE_FAILED,
                "§9.5.1 が MUST とする UPDATE_FAILED であること"
            );
        }
        other => {
            panic!("PUBLISH_DONE(UPDATE_FAILED) を期待したが {other:?} を受け取った")
        }
    }
    assert!(done_fin, "PUBLISH_DONE が最終メッセージとして FIN すること");

    assert_registered_aliases(&server);
}

/// FETCH の REQUEST_UPDATE を不正トークンで拒否したとき §9.5.1 の MUST を満たすこと
///
/// draft-ietf-moq-transport-22 §9.5.1 (Updating Subscriptions): "When a REQUEST_UPDATE
/// fails for a FETCH, the publisher MUST reset the FETCH data stream."
/// 自側 publisher の FETCH では `send_request_error` 経由で fetch を Terminated に遷移させ、
/// open 中の FETCH data stream を reset する。
#[test]
fn fetch_request_update_with_duplicate_tokens_resets_fetch_data_stream() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    let start = Location {
        group_id: 0,
        object_id: 0,
    };
    let end = Location {
        group_id: 5,
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
            end,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("FETCH_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("FETCH_OK の受信に成功すること");

    // publisher が FETCH data stream を開く (§3.2.1)。open 中の stream がある状態で
    // REQUEST_UPDATE を拒否し、reset が発行されることを検証する
    let stream_id = DataStreamId(200);
    server
        .send_fetch_header(stream_id, rid)
        .expect("FETCH_HEADER の送信に成功すること");

    let update = ControlMessage::RequestUpdate(RequestUpdate {
        request_id: rid + 4,
        parameters: duplicate_token_parameters(),
    });
    server
        .recv_stream_message(rid, update)
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert_eq!(
        server.fetch(rid).expect("fetch は存在すること").state,
        FetchState::Terminated,
        "§9.5.1 により fetch は Terminated になること"
    );
    assert_malformed_auth_token_rejection(&mut server, rid);

    // §9.5.1 の MUST: FETCH data stream の reset が発行されること
    let mut resets = Vec::new();
    while let Some(event) = server.poll_event() {
        if let SessionEvent::ResetDataStream {
            stream_id,
            error_code,
            ..
        } = event
        {
            resets.push((stream_id, error_code));
        }
    }
    assert_eq!(
        resets,
        vec![(stream_id, STREAM_CANCELLED)],
        "§9.5.1 が MUST とする FETCH data stream の reset が 1 回だけ発行されること"
    );
}

/// GOAWAY 送信後でも不正トークンの reject は GOING_AWAY より優先されること
///
/// draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) のメッセージ拒否は
/// MUST、§9.2 (GOAWAY) の新規 request 拒否は MAY のため、両方が成立する場合は
/// MALFORMED_AUTH_TOKEN を返す (`Session::accept_peer_request` の doc 参照)。
#[test]
fn subscribe_with_duplicate_tokens_after_goaway_is_rejected_with_malformed_auth_token() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    server
        .send_goaway(Vec::new(), 10_000)
        .expect("GOAWAY の送信に成功すること");
    let _ = take_goaway(&mut server);

    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let _ = take_send_request(&mut client);

    let msg = ControlMessage::Subscribe(Subscribe {
        request_id: rid,
        track_namespace: ns(&[b"live"]),
        track_name: b"cam".to_vec(),
        parameters: duplicate_token_parameters(),
    });
    server
        .recv_request(msg)
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert!(
        server.subscription(rid).is_none(),
        "拒否した SUBSCRIBE の subscription 状態を作らないこと"
    );
    let (err_rid, err_msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert_eq!(err_rid, rid);
    match err_msg {
        ControlMessage::RequestError(err) => {
            assert_eq!(
                err.error_code, REQUEST_MALFORMED_AUTH_TOKEN,
                "MUST の不正トークン拒否が MAY の GOING_AWAY 拒否より優先されること"
            );
        }
        other => {
            panic!("REQUEST_ERROR(MALFORMED_AUTH_TOKEN) を期待したが {other:?} を受け取った")
        }
    }
    assert!(fin, "メッセージ単位の reject は最終応答として FIN すること");
    assert_registered_aliases(&server);
}

/// wire バイト列の decode からセッションの reject までを通しで検証すること
///
/// codec が検出できる重複 (REGISTER / USE_VALUE) は自側の `MessageParameters::encode` が拒否する
/// ため、wire で受信したメッセージは固定した手作りバイト列で再現する。decode が重複を受理して
/// Request ID を保持し、セッションが REQUEST_ERROR (MALFORMED_AUTH_TOKEN) で拒否するところまでを
/// 固定する。ペイロードは Request ID (0x00) + Track Namespace ("live") + Track Name ("cam") +
/// Parameters (AUTHORIZATION_TOKEN (0x03) の USE_VALUE 2 件が同一 (Token Type, Token Value))。
#[test]
fn wire_subscribe_with_duplicate_tokens_is_rejected_by_session() {
    let (_client, mut server) = establish_pair();
    let body: Vec<u8> = vec![
        0x00, // Request ID
        0x01, 0x04, b'l', b'i', b'v', b'e', // Track Namespace (1 field)
        0x03, b'c', b'a', b'm', // Track Name
        // Parameters: count=2, delta=0x03 (AUTHORIZATION_TOKEN), len=3,
        // Token=[USE_VALUE (0x03), Token Type 0x00, Token Value 0x78], delta=0x00, 同一 Token
        0x02, 0x03, 0x03, 0x03, 0x00, 0x78, 0x00, 0x03, 0x03, 0x00, 0x78,
    ];
    let mut wire = vec![0x03]; // MSG_SUBSCRIBE
    wire.push((body.len() >> 8) as u8);
    wire.push(body.len() as u8);
    wire.extend_from_slice(&body);

    let (decoded, consumed) =
        ControlMessage::decode(&wire).expect("decode は重複トークンを拒否しないこと");
    assert_eq!(consumed, wire.len(), "メッセージ全体を消費すること");
    let ControlMessage::Subscribe(subscribe) = decoded else {
        panic!("SUBSCRIBE が期待されたが別のメッセージだった");
    };
    assert_eq!(
        subscribe.parameters.authorization_tokens().len(),
        2,
        "decode は重複トークン 2 件をそのまま保持すること"
    );

    server
        .recv_request(ControlMessage::Subscribe(subscribe))
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert!(
        server.subscription(0).is_none(),
        "拒否した SUBSCRIBE の subscription 状態を作らないこと"
    );
    assert_malformed_auth_token_rejection(&mut server, 0);
}

/// Terminated な FETCH への REQUEST_UPDATE は不正トークンより前にセッション終了になること
///
/// draft-ietf-moq-transport-22 §3.2.4 (Fetch State Management) は fetch 状態の保持期間と
/// 終端手順を定めるが、"Established" という状態は §3.1 (Subscriptions) の購読にしか
/// 定義されていない。本実装は Terminated の fetch への REQUEST_UPDATE を状態機械の違反として
/// PROTOCOL_VIOLATION で閉じる
/// (tests/test_session/fetch/request_update.rs の `terminated_fetch_rejects_late_request_update`
/// と同じ判断)。この検証は §8.9 のメッセージ単位 reject より先に行われるため、
/// 重複トークンを含んでいても MALFORMED_AUTH_TOKEN の REQUEST_ERROR にはならない。
/// 1 通目の不正トークン拒否で fetch が Terminated へ遷移した後の 2 通目で再現する。
#[test]
fn request_update_for_terminated_fetch_with_duplicate_tokens_closes_session() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    let start = Location {
        group_id: 0,
        object_id: 0,
    };
    let end = Location {
        group_id: 5,
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
            end,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("FETCH_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("FETCH_OK の受信に成功すること");

    // 1 通目: 不正トークンで拒否され、fetch は Terminated になる
    let first = ControlMessage::RequestUpdate(RequestUpdate {
        request_id: rid + 4,
        parameters: duplicate_token_parameters(),
    });
    server
        .recv_stream_message(rid, first)
        .expect("1 通目はメッセージ単位の reject (Ok)");
    assert_eq!(
        server.fetch(rid).expect("fetch は存在すること").state,
        FetchState::Terminated
    );
    assert_malformed_auth_token_rejection(&mut server, rid);

    // 2 通目: Terminated な fetch への REQUEST_UPDATE は state 検証でセッション終了になる。
    // state 検証は token cache の適用より先に走るため、alias 登録を伴わない USE_VALUE の
    // 重複トークンで検証する (登録済み alias の再登録による session error と切り分ける)
    let second = ControlMessage::RequestUpdate(RequestUpdate {
        request_id: rid + 6,
        parameters: duplicate_use_value_parameters(),
    });
    let err = server
        .recv_stream_message(rid, second)
        .expect_err("Terminated な fetch への REQUEST_UPDATE は session error になること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert!(
        matches!(server.state(), SessionState::Closing | SessionState::Closed),
        "§9.5 の MUST close でセッションが Closing に遷移すること"
    );
}

/// subscriber 起点 subscription への publisher からの REQUEST_UPDATE は §9.5 の MUST close になること
///
/// draft-ietf-moq-transport-22 §9.5 (REQUEST_UPDATE) が REQUEST_UPDATE を送れる側として
/// 挙げるのは元 request の initiator と PUBLISH 起点 subscription の subscriber であり、
/// 自側が SUBSCRIBE を送った subscription へ peer publisher が送るのは該当しない
/// (MUST close)。重複トークンを含んでいてもメッセージ単位の reject にはならない。
#[test]
fn request_update_from_publisher_with_duplicate_tokens_closes_session() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    let rid = send_and_recv_subscribe_with_parameters(
        &mut client,
        &mut server,
        &[b"live"],
        b"cam",
        MessageParameters::new(),
    );
    complete_subscribe(&mut client, &mut server, rid, 1);

    // REQUEST_UPDATE は peer (server) が送るため、server 側の parity (奇数) の Request ID を使う
    let update = ControlMessage::RequestUpdate(RequestUpdate {
        request_id: rid + 5,
        parameters: duplicate_token_parameters(),
    });
    let err = client
        .recv_stream_message(rid, update)
        .expect_err("subscriber 起点 subscription への REQUEST_UPDATE は session error になること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert!(
        matches!(client.state(), SessionState::Closing | SessionState::Closed),
        "§9.5 の MUST close でセッションが Closing に遷移すること"
    );
}

/// ローカル終端済み request への不正トークン付き REQUEST_UPDATE でも §9.5 の検証は行うこと
///
/// 送信方向を reset 済みの request では拒否応答 (REQUEST_ERROR) を送れないため
/// メッセージは吸収するが、送信者・自側の役割の検証は応答の有無に依存しない。
/// 自側が SUBSCRIBE を送った subscription へ peer publisher が REQUEST_UPDATE を送るのは
/// draft-ietf-moq-transport-22 §9.5 (REQUEST_UPDATE) の 2 ケース外であり、重複トークンを
/// 含んでいてもセッションを PROTOCOL_VIOLATION で閉じる。
#[test]
fn locally_terminated_subscription_update_with_duplicate_tokens_closes_session() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    let rid = send_and_recv_subscribe_with_parameters(
        &mut client,
        &mut server,
        &[b"live"],
        b"cam",
        MessageParameters::new(),
    );
    complete_subscribe(&mut client, &mut server, rid, 1);

    // request stream GOAWAY の deadline 満了で client 側の request をローカル終端する
    client.tick(1_000);
    client
        .send_goaway_on_request_stream(rid, Vec::new(), 100)
        .expect("request stream GOAWAY の送信に成功すること");
    let _ = take_send_on_stream(&mut client);
    client.tick(1_100);
    let mut resets = 0;
    while let Some(event) = client.poll_event() {
        if matches!(event, SessionEvent::ResetRequestStream { .. }) {
            resets += 1;
        }
    }
    assert_eq!(
        resets, 1,
        "期限到達で request stream の reset が 1 回発行され、request がローカル終端済みになること"
    );

    // peer (server) の parity (奇数) の Request ID を使う
    let update = ControlMessage::RequestUpdate(RequestUpdate {
        request_id: 1,
        parameters: duplicate_use_value_parameters(),
    });
    let err = client
        .recv_stream_message(rid, update)
        .expect_err("ローカル終端済みでも正当でない送信者の REQUEST_UPDATE は session error");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert!(
        matches!(client.state(), SessionState::Closing | SessionState::Closed),
        "§9.5 の検証でセッションが Closing に遷移すること"
    );
}

/// ローカル終端済み request の不正トークンは、応答を送らずに吸収すること
///
/// 送信方向を reset 済みの request では REQUEST_ERROR を送れないため、正当な送信者からの
/// REQUEST_UPDATE は状態遷移せずに捨てる。ただし §8.9 の REGISTER MUST
/// ("does not result in a Session error" が前提) は満たすため、alias は cache に登録する。
#[test]
fn locally_terminated_publish_subscription_absorbs_request_update_with_duplicate_tokens() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    // 自側 publisher の subscription (PUBLISH 起点) を作る。peer subscriber からの
    // REQUEST_UPDATE は §9.5 の 2 ケースの 2 つ目に該当する
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            1,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("PUBLISH の送信に成功すること");
    let (_, publish_msg) = take_send_request(&mut client);
    server
        .recv_request(publish_msg)
        .expect("PUBLISH の受信に成功すること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("REQUEST_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("REQUEST_OK の受信に成功すること");

    // request stream GOAWAY の deadline 満了で client 側の request をローカル終端する
    client.tick(1_000);
    client
        .send_goaway_on_request_stream(rid, Vec::new(), 100)
        .expect("request stream GOAWAY の送信に成功すること");
    let _ = take_send_on_stream(&mut client);
    client.tick(1_100);
    let mut resets = 0;
    while let Some(event) = client.poll_event() {
        if matches!(event, SessionEvent::ResetRequestStream { .. }) {
            resets += 1;
        }
    }
    assert_eq!(resets, 1, "期限到達で request stream が reset されること");

    // peer (server) の parity (奇数) の Request ID で重複トークン付き REQUEST_UPDATE を注入する
    let update = ControlMessage::RequestUpdate(RequestUpdate {
        request_id: 1,
        parameters: duplicate_token_parameters(),
    });
    client
        .recv_stream_message(rid, update)
        .expect("ローカル終端済み request の REQUEST_UPDATE は吸収されること");

    assert_eq!(client.state(), SessionState::Established);
    let subscription = client
        .subscription(rid)
        .expect("subscription は存在すること");
    assert_eq!(
        subscription.state,
        SubscriptionState::Terminated,
        "GOAWAY の deadline 満了によるローカル終端の状態が維持されること"
    );
    assert!(
        subscription.pending_update_params.is_none(),
        "吸収した REQUEST_UPDATE のパラメータを蓄積しないこと"
    );
    assert!(
        client.poll_event().is_none(),
        "吸収時は REQUEST_ERROR を含むイベントを発行しないこと"
    );
    assert_registered_aliases(&client);
}

// ─── alias 解決後まで含めた (Token Type, Token Value) の一意性検証 ─────────────

/// USE_ALIAS の列を持つパラメータを組み立てる
fn use_alias_parameters(aliases: &[u64]) -> MessageParameters {
    let mut params = MessageParameters::new();
    for &alias in aliases {
        params.push(MessageParameter {
            param_type: PARAM_AUTHORIZATION_TOKEN,
            value: MessageParameterValue::AuthorizationToken(AuthorizationToken::UseAlias {
                alias,
            }),
        });
    }
    params
}

/// USE_VALUE 1 件を持つパラメータを組み立てる
fn use_value_parameters(token_type: u64, token_value: &[u8]) -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_AUTHORIZATION_TOKEN,
        value: MessageParameterValue::AuthorizationToken(AuthorizationToken::UseValue {
            token_type,
            token_value: token_value.to_vec(),
        }),
    });
    params
}

/// REGISTER 1 件を持つパラメータを組み立てる
fn register_parameters(alias: u64, token_type: u64, token_value: &[u8]) -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_AUTHORIZATION_TOKEN,
        value: MessageParameterValue::AuthorizationToken(AuthorizationToken::Register {
            alias,
            token_type,
            token_value: token_value.to_vec(),
        }),
    });
    params
}

/// REGISTER 付き SUBSCRIBE を受理させて alias を peer (server) の token cache に登録する
///
/// 受理済みの subscription は残るが、以降の検証には影響しない。
fn register_token_on_peer(
    client: &mut Session,
    server: &mut Session,
    alias: u64,
    token_type: u64,
    token_value: &[u8],
) {
    let rid = client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            register_parameters(alias, token_type, token_value),
        )
        .expect("REGISTER 付き SUBSCRIBE の送信に成功すること");
    let (_, msg) = take_send_request(client);
    server
        .recv_request(msg)
        .expect("REGISTER 付き SUBSCRIBE は受理されること");
    assert_eq!(
        server
            .peer_auth_token_cache()
            .resolve(alias)
            .map(|(token_type, value)| (token_type, value.to_vec())),
        Some((token_type, token_value.to_vec())),
        "alias={alias} が peer cache に登録されること"
    );
    let _ = rid;
}

/// 任意のパラメータを載せた SUBSCRIBE を client に新規 Request ID で送らせて返す
fn subscribe_with_parameters(
    client: &mut Session,
    parameters: MessageParameters,
) -> ControlMessage {
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"mic".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let _ = take_send_request(client);
    ControlMessage::Subscribe(Subscribe {
        request_id: rid,
        track_namespace: ns(&[b"live"]),
        track_name: b"mic".to_vec(),
        parameters,
    })
}

/// SUBSCRIBE メッセージから Request ID を取り出す
fn subscribe_request_id(message: &ControlMessage) -> u64 {
    match message {
        ControlMessage::Subscribe(subscribe) => subscribe.request_id,
        other => panic!("SUBSCRIBE が期待されたが {other:?} だった"),
    }
}

/// 同一 alias を 2 回参照する SUBSCRIBE は alias 解決後の重複として拒否されること
///
/// draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression): "An Authorization
/// Token MAY be repeated within a message as long as the combination of Token Type and
/// Token Value are unique after resolving any aliases." に反する。
#[test]
fn subscribe_with_duplicate_resolved_alias_is_rejected_with_malformed_auth_token() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    register_token_on_peer(&mut client, &mut server, 1, 2, b"tok");

    let message = subscribe_with_parameters(&mut client, use_alias_parameters(&[1, 1]));
    let rid = subscribe_request_id(&message);
    server
        .recv_request(message)
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert!(
        server.subscription(rid).is_none(),
        "拒否した SUBSCRIBE の subscription 状態を作らないこと"
    );
    assert_malformed_auth_token_rejection(&mut server, rid);
}

/// PUBLISH でも alias 解決後の重複は拒否されること
#[test]
fn publish_with_duplicate_resolved_alias_is_rejected_with_malformed_auth_token() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    register_token_on_peer(&mut client, &mut server, 1, 2, b"tok");

    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            1,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("PUBLISH の送信に成功すること");
    let _ = take_send_request(&mut client);
    let message = ControlMessage::Publish(Publish {
        request_id: rid,
        track_namespace: ns(&[b"live"]),
        track_name: b"cam".to_vec(),
        track_alias: 1,
        parameters: use_alias_parameters(&[1, 1]),
        track_properties: TrackProperties::new(),
    });
    server
        .recv_request(message)
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert!(
        server.subscription(rid).is_none(),
        "拒否した PUBLISH の subscription 状態を作らないこと"
    );
    assert_malformed_auth_token_rejection(&mut server, rid);
}

/// FETCH でも alias 解決後の重複は拒否されること
#[test]
fn fetch_with_duplicate_resolved_alias_is_rejected_with_malformed_auth_token() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    register_token_on_peer(&mut client, &mut server, 1, 2, b"tok");

    let rid = client
        .send_fetch(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("FETCH の送信に成功すること");
    let _ = take_send_request(&mut client);
    let message = ControlMessage::Fetch(Fetch {
        request_id: rid,
        track_namespace: ns(&[b"live"]),
        track_name: b"cam".to_vec(),
        parameters: use_alias_parameters(&[1, 1]),
    });
    server
        .recv_request(message)
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert!(
        server.fetch(rid).is_none(),
        "拒否した FETCH の fetch 状態を作らないこと"
    );
    assert_malformed_auth_token_rejection(&mut server, rid);
}

/// TRACK_STATUS でも alias 解決後の重複は拒否されること
#[test]
fn track_status_with_duplicate_resolved_alias_is_rejected_with_malformed_auth_token() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    register_token_on_peer(&mut client, &mut server, 1, 2, b"tok");

    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let _ = take_send_request(&mut client);
    let message = ControlMessage::TrackStatus(TrackStatus {
        request_id: rid,
        track_namespace: ns(&[b"live"]),
        track_name: b"cam".to_vec(),
        parameters: use_alias_parameters(&[1, 1]),
    });
    server
        .recv_request(message)
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert!(
        server.track_status_request(rid).is_none(),
        "拒否した TRACK_STATUS の状態を作らないこと"
    );
    assert_malformed_auth_token_rejection(&mut server, rid);
}

/// 同一メッセージ内の先行 REGISTER を参照する USE_ALIAS も重複として拒否されること
///
/// draft は USE_ALIAS を "previously registered" と書くが、本実装の受理経路はメッセージ内の
/// Token をワイヤ順に適用するため、一意性判定も同じ解決結果を使う。REGISTER の MUST
/// (拒否したメッセージでも alias を登録する) もあわせて確認する。
#[test]
fn subscribe_with_register_and_use_alias_in_same_message_is_rejected() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);

    let mut parameters = register_parameters(5, 2, b"tok");
    parameters.push(MessageParameter {
        param_type: PARAM_AUTHORIZATION_TOKEN,
        value: MessageParameterValue::AuthorizationToken(AuthorizationToken::UseAlias { alias: 5 }),
    });
    let message = subscribe_with_parameters(&mut client, parameters);
    let rid = subscribe_request_id(&message);
    server
        .recv_request(message)
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert_malformed_auth_token_rejection(&mut server, rid);
    assert_eq!(
        server
            .peer_auth_token_cache()
            .resolve(5)
            .map(|(token_type, value)| (token_type, value.to_vec())),
        Some((2, b"tok".to_vec())),
        "拒否したメッセージの REGISTER も token cache に登録されること"
    );
}

/// USE_ALIAS の解決結果と USE_VALUE が一致する場合は重複として拒否されること
#[test]
fn subscribe_with_use_value_matching_resolved_alias_is_rejected() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    register_token_on_peer(&mut client, &mut server, 1, 2, b"tok");

    let mut parameters = use_value_parameters(2, b"tok");
    parameters.push(MessageParameter {
        param_type: PARAM_AUTHORIZATION_TOKEN,
        value: MessageParameterValue::AuthorizationToken(AuthorizationToken::UseAlias { alias: 1 }),
    });
    let message = subscribe_with_parameters(&mut client, parameters);
    let rid = subscribe_request_id(&message);
    server
        .recv_request(message)
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert_malformed_auth_token_rejection(&mut server, rid);
}

/// 一意な USE_ALIAS だけを持つ SUBSCRIBE は従来どおり受理されること
#[test]
fn subscribe_with_unique_use_aliases_is_accepted() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    register_token_on_peer(&mut client, &mut server, 1, 2, b"tok");
    register_token_on_peer(&mut client, &mut server, 2, 3, b"tok2");

    let message = subscribe_with_parameters(&mut client, use_alias_parameters(&[1, 2]));
    let rid = subscribe_request_id(&message);
    server
        .recv_request(message)
        .expect("alias 解決後に一意な SUBSCRIBE は受理されること");

    assert_eq!(server.state(), SessionState::Established);
    assert!(
        server.subscription(rid).is_some(),
        "受理した SUBSCRIBE の subscription 状態を作ること"
    );
    while let Some(event) = server.poll_event() {
        assert!(
            !matches!(
                event,
                SessionEvent::SendOnStream { .. } | SessionEvent::SendControl(_)
            ),
            "受理した SUBSCRIBE では拒否応答を発行しないこと: {event:?}"
        );
    }
}

/// 異なる alias が同じ (Token Type, Token Value) に解決する場合も重複として拒否されること
///
/// alias が異なれば同じ (Token Type, Token Value) を登録できる。解決結果が一致する 2 つの
/// USE_ALIAS は draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) の
/// "unique after resolving any aliases" に反する。
#[test]
fn subscribe_with_different_aliases_resolving_to_same_value_is_rejected() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    register_token_on_peer(&mut client, &mut server, 1, 2, b"tok");
    register_token_on_peer(&mut client, &mut server, 2, 2, b"tok");

    let message = subscribe_with_parameters(&mut client, use_alias_parameters(&[1, 2]));
    let rid = subscribe_request_id(&message);
    server
        .recv_request(message)
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert!(
        server.subscription(rid).is_none(),
        "拒否した SUBSCRIBE の subscription 状態を作らないこと"
    );
    assert_malformed_auth_token_rejection(&mut server, rid);
}

/// 未登録 alias を参照する SUBSCRIBE は従来どおり UNKNOWN_AUTH_TOKEN_ALIAS でセッション終了すること
///
/// 一意性検証は解決できない alias を判定対象から外すだけで、未登録 alias の扱いは
/// 適用経路 (`apply_peer_message_auth_tokens`) のまま変わらない。
#[test]
fn subscribe_with_unknown_alias_closes_session_with_unknown_auth_token_alias() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);

    let message = subscribe_with_parameters(&mut client, use_alias_parameters(&[9]));
    let err = server
        .recv_request(message)
        .expect_err("未登録 alias は session error になること");
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_UNKNOWN_AUTH_TOKEN_ALIAS
    );
    assert!(
        matches!(server.state(), SessionState::Closing | SessionState::Closed),
        "未登録 alias の適用でセッションが Closing に遷移すること"
    );
}

/// 未登録 alias と重複が同居する場合は MALFORMED_AUTH_TOKEN のメッセージ単位 reject が優先されること
///
/// 解決できない alias は一意性の判定対象から外し、重複は §8.9 の MUST に従ってメッセージ単位に
/// 拒否する。未登録 alias の適用は行わないため、セッションは維持される。
#[test]
fn subscribe_with_duplicate_and_unknown_alias_prefers_malformed_auth_token() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    register_token_on_peer(&mut client, &mut server, 1, 2, b"tok");

    // 未登録 alias を先頭に置き、その後ろに解決可能な alias の重複を置く
    let message = subscribe_with_parameters(&mut client, use_alias_parameters(&[9, 1, 1]));
    let rid = subscribe_request_id(&message);
    server
        .recv_request(message)
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert_malformed_auth_token_rejection(&mut server, rid);
}

/// alias 解決後の重複を REQUEST_UPDATE でも拒否すること
#[test]
fn request_update_with_duplicate_resolved_alias_is_rejected_with_malformed_auth_token() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    // 自側 publisher の Established subscription を作る。SUBSCRIBE に REGISTER を載せ、
    // server 側の peer cache に alias 1 を登録する
    let rid = send_and_recv_subscribe_with_parameters(
        &mut client,
        &mut server,
        &[b"live"],
        b"cam",
        register_parameters(1, 2, b"tok"),
    );
    complete_subscribe(&mut client, &mut server, rid, 1);

    let update = ControlMessage::RequestUpdate(RequestUpdate {
        request_id: rid + 4,
        parameters: use_alias_parameters(&[1, 1]),
    });
    server
        .recv_stream_message(rid, update)
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert_eq!(
        server
            .subscription(rid)
            .expect("subscription は存在すること")
            .state,
        SubscriptionState::Terminated,
        "自側 publisher の REQUEST_UPDATE 拒否は §9.5.1 により subscription を終端すること"
    );
    let (err_rid, err_msg, err_fin) = take_send_on_stream_with_fin(&mut server);
    assert_eq!(err_rid, rid);
    match err_msg {
        ControlMessage::RequestError(err) => {
            assert_eq!(
                err.error_code, REQUEST_MALFORMED_AUTH_TOKEN,
                "alias 解決後の重複も MALFORMED_AUTH_TOKEN で拒否されること"
            );
        }
        other => {
            panic!("REQUEST_ERROR(MALFORMED_AUTH_TOKEN) を期待したが {other:?} を受け取った")
        }
    }
    assert!(
        !err_fin,
        "PUBLISH_DONE が続くため REQUEST_ERROR では FIN しないこと"
    );
}

/// wire バイト列の decode から alias 解決後の重複の reject までを通しで検証すること
///
/// 固定した wire バイト列で decode と session の reject を通しで固定する。ペイロードは
/// Request ID (4) + Track Namespace ("live") + Track Name ("cam") + Parameters
/// (AUTHORIZATION_TOKEN (0x03) の USE_ALIAS 2 件が同じ alias 1)。
#[test]
fn wire_subscribe_with_duplicate_resolved_alias_is_rejected_by_session() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    register_token_on_peer(&mut client, &mut server, 1, 2, b"tok");

    let body: Vec<u8> = vec![
        0x04, // Request ID
        0x01, 0x04, b'l', b'i', b'v', b'e', // Track Namespace (1 field)
        0x03, b'c', b'a', b'm', // Track Name
        // Parameters: count=2, delta=0x03 (AUTHORIZATION_TOKEN), len=2,
        // Token=[USE_ALIAS (0x02), Token Alias 1], delta=0x00, 同一 Token
        0x02, 0x03, 0x02, 0x02, 0x01, 0x00, 0x02, 0x02, 0x01,
    ];
    let mut wire = vec![0x03]; // MSG_SUBSCRIBE
    wire.push((body.len() >> 8) as u8);
    wire.push(body.len() as u8);
    wire.extend_from_slice(&body);

    let (decoded, consumed) =
        ControlMessage::decode(&wire).expect("decode は alias 解決後の重複を拒否しないこと");
    assert_eq!(consumed, wire.len(), "メッセージ全体を消費すること");
    let ControlMessage::Subscribe(subscribe) = decoded else {
        panic!("SUBSCRIBE が期待されたが別のメッセージだった");
    };
    assert_eq!(
        subscribe.parameters.authorization_tokens().len(),
        2,
        "decode は USE_ALIAS 2 件をそのまま保持すること"
    );

    server
        .recv_request(ControlMessage::Subscribe(subscribe))
        .expect("メッセージ単位の reject は Ok (セッションは維持)");

    assert_eq!(server.state(), SessionState::Established);
    assert!(
        server.subscription(4).is_none(),
        "拒否した SUBSCRIBE の subscription 状態を作らないこと"
    );
    assert_malformed_auth_token_rejection(&mut server, 4);
}

/// メッセージ内の DELETE を alias 解決に反映すること
///
/// 事前登録した alias を同じメッセージ内で DELETE してから USE_ALIAS で参照した場合、
/// DELETE を反映しなければ実キャッシュの値に解決されて重複と誤判定され、MALFORMED_AUTH_TOKEN の
/// メッセージ単位 reject (セッション維持) になる。反映していれば未登録 alias となり、一意性検証は
/// 重複を検出せず、適用経路が UNKNOWN_AUTH_TOKEN_ALIAS でセッションを閉じる。
#[test]
fn subscribe_with_deleted_alias_is_not_treated_as_duplicate() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    register_token_on_peer(&mut client, &mut server, 1, 2, b"tok");

    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter {
        param_type: PARAM_AUTHORIZATION_TOKEN,
        value: MessageParameterValue::AuthorizationToken(AuthorizationToken::Delete { alias: 1 }),
    });
    for _ in 0..2 {
        parameters.push(MessageParameter {
            param_type: PARAM_AUTHORIZATION_TOKEN,
            value: MessageParameterValue::AuthorizationToken(AuthorizationToken::UseAlias {
                alias: 1,
            }),
        });
    }
    let message = subscribe_with_parameters(&mut client, parameters);
    let err = server
        .recv_request(message)
        .expect_err("DELETE 済み alias の参照は session error になること");
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_UNKNOWN_AUTH_TOKEN_ALIAS,
        "DELETE を反映した結果、重複ではなく未登録 alias として扱われること"
    );
    assert!(
        matches!(server.state(), SessionState::Closing | SessionState::Closed),
        "未登録 alias の適用でセッションが Closing に遷移すること"
    );
}
