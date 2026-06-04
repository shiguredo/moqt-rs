use super::*;
use shiguredo_moqt::track_properties::TrackProperties;

// ─── NEW_GROUP_REQUEST / DYNAMIC_GROUPS 検証 (draft-ietf-moq-transport-21 §9.20.20 (NEW GROUP REQUEST Parameter)) ────

/// PUBLISH_OK + NEW_GROUP_REQUEST は送信側で拒否される
///
/// draft-ietf-moq-transport-21 Appendix A.1 #1790 (Subscription parameters appear in
/// REQUEST_UPDATE, not PUBLISH_OK): PUBLISH_OK は EXPIRES のみを運ぶ。
/// NEW_GROUP_REQUEST は SUBSCRIBE / REQUEST_UPDATE にのみ出現する
/// (draft-ietf-moq-transport-21 §9.20.20 (NEW GROUP REQUEST Parameter))。
/// DYNAMIC_GROUPS の有無にかかわらず送信側スコープ検証で拒否される。
/// NEW_GROUP_REQUEST の DYNAMIC_GROUPS 連動検証は REQUEST_UPDATE 経路の
/// `request_update_with_new_group_request_*` で行う。
#[test]
fn publish_ok_with_new_group_request_is_rejected() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_NEW_GROUP_REQUEST,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            13,
            MessageParameters::new(),
            track_properties_with_dynamic_groups(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_NEW_GROUP_REQUEST,
        value: MessageParameterValue::VarInt(5),
    });
    let err = server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .expect_err("PUBLISH_OK の NEW_GROUP_REQUEST はスコープ外で拒否される");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // 拒否後も subscription は Pending のままであり、状態は汚染されない
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .is_pending_publisher()
    );
}

/// DYNAMIC_GROUPS を含まない Track に対する REQUEST_UPDATE + NEW_GROUP_REQUEST は
/// PROTOCOL_VIOLATION
#[test]
fn request_update_with_new_group_request_without_dynamic_groups_rejected() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_NEW_GROUP_REQUEST,
    };
    let (mut client, mut server) = establish_pair();
    // Subscriber-initiated な Subscription を作り、Server が publisher となる
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // server は DYNAMIC_GROUPS なしで SUBSCRIBE_OK
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // client (subscriber) から NEW_GROUP_REQUEST 付き REQUEST_UPDATE を送る
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_NEW_GROUP_REQUEST,
        value: MessageParameterValue::VarInt(2),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    // server 側で受信すると PROTOCOL_VIOLATION
    let err = server.recv_stream_message(rid, upd_msg).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// DYNAMIC_GROUPS=0 (値あり) の Track に対する REQUEST_UPDATE + NEW_GROUP_REQUEST も
/// PROTOCOL_VIOLATION (なし と同等に扱う)
#[test]
fn request_update_with_new_group_request_dynamic_groups_zero_rejected() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_NEW_GROUP_REQUEST,
    };
    use shiguredo_moqt::track_properties::{
        PROP_DYNAMIC_GROUPS, TrackProperty, TrackPropertyValue,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // server は DYNAMIC_GROUPS=0 で SUBSCRIBE_OK
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: PROP_DYNAMIC_GROUPS,
        value: TrackPropertyValue::VarInt(0),
    });
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), tp)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // client (subscriber) から NEW_GROUP_REQUEST 付き REQUEST_UPDATE を送る
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_NEW_GROUP_REQUEST,
        value: MessageParameterValue::VarInt(2),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    // server 側で受信すると PROTOCOL_VIOLATION
    let err = server.recv_stream_message(rid, upd_msg).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// DYNAMIC_GROUPS=1 の Track ならば REQUEST_UPDATE + NEW_GROUP_REQUEST は受理される
#[test]
fn request_update_with_new_group_request_dynamic_groups_one_accepted() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_NEW_GROUP_REQUEST,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            rid,
            1,
            MessageParameters::new(),
            track_properties_with_dynamic_groups(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_NEW_GROUP_REQUEST,
        value: MessageParameterValue::VarInt(7),
    });
    client
        .send_request_update(rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established
    );
}

/// SUBSCRIBE の AUTHORIZATION_TOKEN (REGISTER) を peer_auth_token_cache に登録する
#[test]
fn peer_subscribe_auth_token_register_populates_cache() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            params_with_register(3, 42, b"hello"),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server.peer_auth_token_cache().resolve(3),
        Some((42, &b"hello"[..]))
    );
}

/// PUBLISH の AUTHORIZATION_TOKEN (REGISTER) を peer_auth_token_cache に登録する
#[test]
fn peer_publish_auth_token_register_populates_cache() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            21,
            params_with_register(5, 9, b"world"),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server.peer_auth_token_cache().resolve(5),
        Some((9, &b"world"[..]))
    );
}

/// FETCH の AUTHORIZATION_TOKEN (REGISTER) を peer_auth_token_cache に登録する
#[test]
fn peer_fetch_auth_token_register_populates_cache() {
    use shiguredo_moqt::message::Fetch as WireFetch;
    let (_client, mut server) = establish_pair_with_self_auth_cache(1024);
    server
        .recv_request(ControlMessage::Fetch(WireFetch {
            request_id: 0, // client-originated even request id

            track_namespace: ns(&[b"live"]),
            track_name: b"cam".to_vec(),
            parameters: params_with_register(11, 1, b"ft"),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server.peer_auth_token_cache().resolve(11),
        Some((1, &b"ft"[..]))
    );
}

/// DELETE で登録済み alias が cache から消える
#[test]
fn peer_subscribe_auth_token_delete_removes_from_cache() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            params_with_register(3, 42, b"hello"),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(server.peer_auth_token_cache().resolve(3).is_some());
    client
        .send_subscribe(ns(&[b"live"]), b"mic".to_vec(), params_with_delete(3))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub2_msg) = take_send_request(&mut client);
    server
        .recv_request(sub2_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(server.peer_auth_token_cache().resolve(3).is_none());
}

/// 同一 alias の REGISTER 重複は DUPLICATE_AUTH_TOKEN_ALIAS
#[test]
fn peer_subscribe_auth_token_register_duplicate_alias_closes_session() {
    use shiguredo_moqt::error::SESSION_DUPLICATE_AUTH_TOKEN_ALIAS;
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            params_with_register(3, 42, b"hello"),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .send_subscribe(
            ns(&[b"live"]),
            b"mic".to_vec(),
            params_with_register(3, 43, b"world"),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub2_msg) = take_send_request(&mut client);
    let err = server.recv_request(sub2_msg).unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("テストフィクスチャの前提条件を満たす")
            .code,
        SESSION_DUPLICATE_AUTH_TOKEN_ALIAS
    );
}

/// REGISTER が cache size を超えた場合は AUTH_TOKEN_CACHE_OVERFLOW
#[test]
fn peer_subscribe_auth_token_register_overflow_closes_session() {
    use shiguredo_moqt::error::SESSION_AUTH_TOKEN_CACHE_OVERFLOW;
    // max=16 では 16+1 = 17 バイトの REGISTER を収容できない
    let (mut client, mut server) = establish_pair_with_self_auth_cache(16);
    client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            params_with_register(1, 1, b"x"),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    let err = server.recv_request(sub_msg).unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("テストフィクスチャの前提条件を満たす")
            .code,
        SESSION_AUTH_TOKEN_CACHE_OVERFLOW
    );
}

/// 未登録 alias への USE_ALIAS は UNKNOWN_AUTH_TOKEN_ALIAS
#[test]
fn peer_subscribe_auth_token_use_alias_unknown_closes_session() {
    use shiguredo_moqt::error::SESSION_UNKNOWN_AUTH_TOKEN_ALIAS;
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), params_with_use_alias(99))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    let err = server.recv_request(sub_msg).unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("テストフィクスチャの前提条件を満たす")
            .code,
        SESSION_UNKNOWN_AUTH_TOKEN_ALIAS
    );
}

/// USE_ALIAS で登録済み alias なら通常処理され、session は Established を維持する
#[test]
fn peer_subscribe_auth_token_use_alias_known_accepted() {
    let (mut client, mut server) = establish_pair_with_self_auth_cache(1024);
    // 先に REGISTER
    client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            params_with_register(7, 2, b"tok"),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, m1) = take_send_request(&mut client);
    server
        .recv_request(m1)
        .expect("テストフィクスチャの前提条件を満たす");
    // 続いて USE_ALIAS
    client
        .send_subscribe(ns(&[b"live"]), b"mic".to_vec(), params_with_use_alias(7))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, m2) = take_send_request(&mut client);
    server
        .recv_request(m2)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.state(), SessionState::Established);
}

// ─── Mandatory Track Properties (draft-ietf-moq-transport-21 §3.6 (Mandatory Track Properties)) ──────

/// 未知の必須トラックプロパティを含む PUBLISH は REQUEST_ERROR (UNSUPPORTED_EXTENSION) で拒否される
#[test]
fn publish_with_unknown_mandatory_property_rejected() {
    use shiguredo_moqt::error::REQUEST_UNSUPPORTED_EXTENSION;
    use shiguredo_moqt::track_properties::{
        MANDATORY_TRACK_PROPERTY_MIN, TrackProperty, TrackPropertyValue,
    };
    let (mut client, mut server) = establish_pair();
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: MANDATORY_TRACK_PROPERTY_MIN,
        value: TrackPropertyValue::VarInt(1),
    });
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            100,
            MessageParameters::new(),
            tp,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let (stream_rid, err_msg) = take_send_on_stream(&mut server);
    assert_eq!(stream_rid, rid);
    assert!(matches!(
        err_msg,
        ControlMessage::RequestError(err) if err.error_code == REQUEST_UNSUPPORTED_EXTENSION
    ));
    assert!(server.subscription(rid).is_none());
    assert_eq!(server.state(), SessionState::Established);
}

// ─── REQUEST_OK の応答 context 別パラメータスコープ検証 (draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope)) ────
//
// REQUEST_OK (Type 0x07) は PUBLISH_OK / REQUEST_UPDATE_OK / TRACK_STATUS_OK /
// namespace 系 OK / SUBSCRIBE_TRACKS_OK が共有する単一ワイヤメッセージ。
// encode/decode 層は全 context の和集合 (REQUEST_OK_ALLOWED_PARAMS) でしか検証
// しないため、context ごとの許可集合外パラメータは受信側 (セッション層) で
// PROTOCOL_VIOLATION として弾く必要がある。以下は意図的エラーパス (PBT では
// 表現できない) のための単体テスト。

/// PUBLISH_OK 応答に LARGEST_OBJECT を載せると送信側で PROTOCOL_VIOLATION
/// (draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter): LARGEST_OBJECT は
/// SUBSCRIBE_OK / PUBLISH / REQUEST_UPDATE_OK / TRACK_STATUS_OK にのみ出現可能で、
/// PUBLISH_OK は列挙されていない。`send_request_ok` が送信側で検出する)
#[test]
fn publish_ok_with_largest_object_rejected() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            21,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 9,
            object: 1,
        },
    });
    // PUBLISH_OK context では LARGEST_OBJECT が許可されていないため送信側で拒否される
    let err = server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// TRACK_STATUS_OK 応答に GROUP_ORDER を載せると PROTOCOL_VIOLATION
/// (draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): GROUP_ORDER の scope に TRACK_STATUS_OK は含まれず、
/// TRACK_STATUS_OK の許可は draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter) LARGEST_OBJECT のみ。draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope) が MUST-close 根拠。
/// 送信側が `send_request_ok` で検出する)
#[test]
fn track_status_ok_with_group_order_rejected() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_GROUP_ORDER,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ts_msg) = take_send_request(&mut client);
    server
        .recv_request(ts_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(1),
    });
    let err = server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// SUBSCRIBE_NAMESPACE_OK 応答に任意のパラメータ (FORWARD) を載せると PROTOCOL_VIOLATION
/// (draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope) 等: namespace 系 OK はパラメータを一切許可しない。
/// 送信側が `send_request_ok` で検出する)
#[test]
fn subscribe_namespace_ok_with_parameter_rejected() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    let err = server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// REQUEST_UPDATE_OK (subscription) 応答に GROUP_ORDER を載せると PROTOCOL_VIOLATION
/// (draft-ietf-moq-transport-21 §9.20.17 (EXPIRES Parameter) / draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter): REQUEST_UPDATE_OK は EXPIRES / LARGEST_OBJECT のみ許可。
/// 送信側が `send_ok_for_subscription` で検出する)
#[test]
fn request_update_ok_subscription_with_group_order_rejected() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_GROUP_ORDER,
    };
    let (mut client, mut server, rid) = establish_subscribe_track(31);
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(1),
    });
    let err = server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// スコープ外パラメータを含む REQUEST_UPDATE_OK は subscription の状態を変更しない
///
/// draft-ietf-moq-transport-21 §9.20.17 (EXPIRES Parameter) / §9.20.18 (LARGEST OBJECT Parameter):
/// REQUEST_UPDATE_OK は EXPIRES / LARGEST_OBJECT のみ許可 (draft §9.20.1 は受信側の
/// PROTOCOL_VIOLATION MUST を規定)。スコープ検証は状態遷移・パラメータ適用より前に
/// 実行されるため、pending_update_params が消費されず、OBJECT_DELIVERY_TIMEOUT や
/// EXPIRES も適用されない。
#[test]
fn request_update_ok_with_out_of_scope_parameter_keeps_subscription_state() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_GROUP_ORDER,
    };
    let (mut client, mut server, rid) = establish_subscribe_track(56);
    // subscriber が OBJECT_DELIVERY_TIMEOUT を含む REQUEST_UPDATE を送る
    // (pending_update_params に積まれる。EXPIRES は REQUEST_UPDATE で非許可のため使わない)
    client
        .send_request_update(rid, delivery_timeout_params(5000))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // スコープ外の GROUP_ORDER とスコープ内の EXPIRES を混在させる
    let mut ok_params = expires_params(60000);
    ok_params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(1),
    });
    let err = server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // pending_update_params が消費されず、OBJECT_DELIVERY_TIMEOUT / EXPIRES も適用されていない
    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        sub.state,
        SubscriptionState::Established,
        "状態は Established のまま"
    );
    assert!(
        sub.pending_update_params.is_some(),
        "pending_update_params が消費されないこと"
    );
    assert_eq!(
        sub.delivery_timeouts.subscriber_object_ms, None,
        "OBJECT_DELIVERY_TIMEOUT が適用されないこと"
    );
    assert_eq!(sub.expires, None, "EXPIRES が適用されないこと");
    // REQUEST_OK が送信イベントとして積まれないこと
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::SendOnStream { .. }) {
            panic!("スコープ検証エラー時に REQUEST_OK は送信されないこと");
        }
    }
}

/// 挙動変更点: REQUEST_UPDATE_OK (subscription) に OBJECT_DELIVERY_TIMEOUT を載せると
/// 新たに PROTOCOL_VIOLATION になる (draft-ietf-moq-transport-21 §9.20.5 (OBJECT_DELIVERY_TIMEOUT Parameter) は REQUEST_UPDATE には許可するが
/// REQUEST_UPDATE_OK には許可しない。送信側が `send_ok_for_subscription` で検出する)
#[test]
fn request_update_ok_subscription_with_object_delivery_timeout_rejected() {
    let (mut client, mut server, rid) = establish_subscribe_track(32);
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = server
        .send_request_ok(
            rid,
            delivery_timeout_params(5000),
            TrackProperties::default(),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// 挙動変更点: REQUEST_UPDATE_OK (subscription) に SUBGROUP_DELIVERY_TIMEOUT を載せると
/// 新たに PROTOCOL_VIOLATION になる (draft-ietf-moq-transport-21 §9.20.4 (SUBGROUP_DELIVERY_TIMEOUT Parameter) は REQUEST_UPDATE には許可するが
/// REQUEST_UPDATE_OK には許可しない。`send_ok_for_subscription` で送信側が先に検出する)
#[test]
fn request_update_ok_subscription_with_subgroup_delivery_timeout_rejected() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_SUBGROUP_DELIVERY_TIMEOUT,
    };
    let (mut client, mut server, rid) = establish_subscribe_track(33);
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_SUBGROUP_DELIVERY_TIMEOUT,
        value: MessageParameterValue::VarInt(5000),
    });
    let err = server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): GROUP_ORDER の
/// MAY appear 列挙 (SUBSCRIBE / SUBSCRIBE_TRACKS / FETCH) に PUBLISH_OK は含まれないため、
/// PUBLISH_OK に GROUP_ORDER を載せると拒否される (送信側で PROTOCOL_VIOLATION)
#[test]
fn publish_ok_with_group_order_rejected_in_draft_20() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_GROUP_ORDER,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            41,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(1),
    });
    // draft-20: GROUP_ORDER は PUBLISH_OK context では非許可
    let err = server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// GROUP_ORDER を含む PUBLISH の送信が許可され、アプリが渡した値と同一の GROUP_ORDER が
/// 送信メッセージに含まれ、ワイヤエンコードできること
///
/// draft-ietf-moq-transport-21 §9.18.1 (Parameters on SUBSCRIBE_TRACKS): "Any Parameter that can be
/// specified on a Subscription (ie: in SUBSCRIBE) is valid in SUBSCRIBE_TRACKS, unless otherwise
/// specified." GROUP_ORDER は §9.20.9 (GROUP ORDER Parameter) に定義される。
/// SUBSCRIBE_TRACKS 由来の PUBLISH に GROUP_ORDER を載せる伝播を実現するため、
/// `PUBLISH_ALLOWED_PARAMS` に GROUP_ORDER を追加した。
#[test]
fn publish_with_group_order_encodes_and_keeps_value() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_GROUP_ORDER,
    };
    let (mut client, _server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(1),
    });
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            99,
            params,
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (sent_rid, pub_msg) = take_send_request(&mut client);
    assert_eq!(sent_rid, rid);
    // アプリが渡した値と同一の GROUP_ORDER が送信メッセージに含まれること
    match &pub_msg {
        ControlMessage::Publish(p) => {
            assert_eq!(
                p.parameters.group_order(),
                Some(1),
                "アプリが渡した GROUP_ORDER が送信メッセージに含まれること"
            );
        }
        other => panic!("Publish が期待されたが {other:?} を受け取った"),
    }
    // ワイヤエンコードが成功すること
    pub_msg
        .encode()
        .expect("GROUP_ORDER を含む PUBLISH はエンコードできること");
}

/// GROUP_ORDER を含む PUBLISH の受信がデコード層で拒否されないこと (roundtrip)
///
/// 受信側の `Publish::decode_message_body` の `validate_scope` が、§9.18.1 に従って
/// GROUP_ORDER を載せた peer の PUBLISH を拒否しないことを確認する。
/// encode → decode の roundtrip で検証する (recv_request への直接渡しでは
/// デコード層が走らないため判別力がない)。
#[test]
fn publish_with_group_order_roundtrips() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_GROUP_ORDER,
    };
    let (mut client, _server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(2),
    });
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            100,
            params,
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (sent_rid, pub_msg) = take_send_request(&mut client);
    assert_eq!(sent_rid, rid);
    let encoded = pub_msg
        .encode()
        .expect("GROUP_ORDER を含む PUBLISH はエンコードできること");
    let (decoded, _) =
        ControlMessage::decode(&encoded).expect("GROUP_ORDER を含む PUBLISH はデコードできること");
    match &decoded {
        ControlMessage::Publish(p) => {
            assert_eq!(
                p.parameters.group_order(),
                Some(2),
                "roundtrip で GROUP_ORDER が保持されること"
            );
        }
        other => panic!("Publish が期待されたが {other:?} を受け取った"),
    }
}

/// 値域外 GROUP_ORDER (0x1 / 0x2 以外) を含む PUBLISH の送信が API 呼び出し時に拒否されること
///
/// draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): "The allowed values are
/// Ascending (0x1) or Descending (0x2)." 検証がないと、request 登録・ SendRequest の push
/// まで完了した後に I/O 層のエンコード時 (validate_param_encoding) で非同期に失敗し、
/// アプリは送信成功と誤認したまま peer に届かない。検証は request_id 発行より前に置き、
/// エラー時に欠番を作らない。
#[test]
fn publish_with_invalid_group_order_rejected_on_send() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_GROUP_ORDER,
    };
    use shiguredo_moqt::session::types::SendRequestError;
    let (mut client, _server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(3), // 値域外
    });
    let err = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            101,
            params,
            TrackProperties::new(),
        )
        .unwrap_err();
    match err {
        SendRequestError::Session(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("SendRequestError::Session が期待される: {other:?}"),
    }
    // SendRequest イベントが積まれないこと
    let mut sent = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::SendRequest { .. }) {
            sent = true;
        }
    }
    assert!(!sent, "検証エラー時に SendRequest イベントが積まれないこと");
    // 検証エラー後に正常な送信が継続できること (フレッシュセッションでは最初の
    // 正常送信の request_id が 0 になる = 検証エラーで request_id が欠番にならない)
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            102,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("検証エラー後の正常送信は成功すること");
    assert_eq!(rid, 0, "検証エラーで request_id が欠番にならないこと");
}

/// 値域外 GROUP_ORDER を含む `send_publish` で control deadline が開始されないこと
///
/// 検証エラー時に deadline が開始されると、アプリがエンコードエラーを無視した場合に
/// `SESSION_CONTROL_MESSAGE_TIMEOUT` という後続の誤作動につながる。
/// 手順: (a) timeout 設定は検証エラーとなる送信より前に呼ぶ (b) tick は送信前に 1 回打って
/// `last_tick_ms` を確定させたうえで、期限超過となる絶対時刻で tick する
/// (c) 正常送信を行うテストとは分けて実施する (正常送信後は deadline が開始されるため)。
/// 本テストは負の検証のみのため、deadline 機構が丸ごと壊れた場合も pass する
/// (false negative)。正の経路 (deadline 開始 → CloseSession) は timeout_api.rs の
/// `control_message_timeout_*` テスト群が検証している。
#[test]
fn publish_invalid_group_order_does_not_start_control_deadline() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_GROUP_ORDER,
    };
    let (mut client, _server) = establish_pair();
    client.tick(0);
    client.set_control_message_timeout_ms(Some(10));
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(3), // 値域外
    });
    let _ = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            103,
            params,
            TrackProperties::new(),
        )
        .expect_err("値域外 GROUP_ORDER は API 呼び出し時にエラーになること");
    // 期限超過となる絶対時刻で tick しても CloseSession が出ない (deadline が開始されていない)
    client.tick(100);
    let mut closed = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::CloseSession(_)) {
            closed = true;
        }
    }
    assert!(
        !closed,
        "検証エラー時に control deadline が開始されないこと"
    );
}

/// FORWARD=0 を含む PUBLISH の送信が許可され、アプリが渡した値と同一の FORWARD が
/// 送信メッセージに含まれ、ワイヤエンコードできること
///
/// draft-ietf-moq-transport-21 §9.18.1 (Parameters on SUBSCRIBE_TRACKS): "Any Parameter that can be
/// specified on a Subscription (ie: in SUBSCRIBE) is valid in SUBSCRIBE_TRACKS, unless otherwise
/// specified." FORWARD は §9.20.19 (FORWARD Parameter) に定義される。
/// SUBSCRIBE_TRACKS 由来の PUBLISH に FORWARD を載せる伝播を実現するため、
/// `PUBLISH_ALLOWED_PARAMS` に FORWARD が含まれており、`send_publish` は FORWARD=0 を
/// 受容する。
#[test]
fn publish_with_forward_encodes_and_keeps_value() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, _server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            106,
            params,
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (sent_rid, pub_msg) = take_send_request(&mut client);
    assert_eq!(sent_rid, rid);
    // アプリが渡した値と同一の FORWARD が送信メッセージに含まれること
    match &pub_msg {
        ControlMessage::Publish(p) => {
            assert_eq!(
                p.parameters.forward(),
                Some(0),
                "アプリが渡した FORWARD が送信メッセージに含まれること"
            );
        }
        other => panic!("Publish が期待されたが {other:?} を受け取った"),
    }
    // ワイヤエンコードが成功すること
    pub_msg
        .encode()
        .expect("FORWARD=0 を含む PUBLISH はエンコードできること");
}

/// FORWARD を含む PUBLISH の受信がデコード層で拒否されないこと (roundtrip)
///
/// 受信側の `Publish::decode_message_body` の `validate_scope` が、FORWARD を載せた
/// peer の PUBLISH を拒否しないことを確認する。encode → decode の roundtrip で検証する
/// (recv_request への直接渡しではデコード層が走らないため判別力がない)。
#[test]
fn publish_with_forward_roundtrips() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD,
    };
    let (mut client, _server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            107,
            params,
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (sent_rid, pub_msg) = take_send_request(&mut client);
    assert_eq!(sent_rid, rid);
    let encoded = pub_msg
        .encode()
        .expect("FORWARD を含む PUBLISH はエンコードできること");
    let (decoded, _) =
        ControlMessage::decode(&encoded).expect("FORWARD を含む PUBLISH はデコードできること");
    match &decoded {
        ControlMessage::Publish(p) => {
            assert_eq!(
                p.parameters.forward(),
                Some(0),
                "roundtrip で FORWARD が保持されること"
            );
        }
        other => panic!("Publish が期待されたが {other:?} を受け取った"),
    }
}

/// FORWARD 未指定の PUBLISH にライブラリが FORWARD を自動注入しないこと
///
/// 伝播はアプリの責務であり、ライブラリは暗黙の注入を行わない
/// (アプリの明示的なパラメータ指定を尊重する API 哲学)。FORWARD=1 は省略で示してよい
/// (draft §9.18.1) ため、自動注入の必要性はない。将来の自動注入機能追加への回帰ガード。
#[test]
fn publish_without_forward_no_auto_inject() {
    let (mut client, _server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            108,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (sent_rid, pub_msg) = take_send_request(&mut client);
    assert_eq!(sent_rid, rid);
    match &pub_msg {
        ControlMessage::Publish(p) => {
            assert_eq!(
                p.parameters.forward(),
                None,
                "FORWARD 未指定の PUBLISH に自動注入されないこと"
            );
        }
        other => panic!("Publish が期待されたが {other:?} を受け取った"),
    }
}

/// スコープ外パラメータを含む SUBSCRIBE の送信が API 呼び出し時に拒否されること
///
/// draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope): SUBSCRIBE で許可されない
/// パラメータを含む送信は、検証がないと request 登録・ SendRequest の push まで完了した
/// 後に I/O 層のエンコード時で非同期に失敗する。検証は request_id 発行より前に置き、
/// エラー時に欠番を作らない。
#[test]
fn subscribe_with_out_of_scope_parameter_rejected_without_state_change() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES, PARAM_SUBSCRIBER_PRIORITY,
    };
    use shiguredo_moqt::session::types::SendRequestError;
    let (mut client, _server) = establish_pair();
    // スコープ外の EXPIRES と、スコープ内の SUBSCRIBER_PRIORITY を混在させる
    // (スコープ内パラメータを含んでも検証エラーになることを確認する)
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(1000),
    });
    params.push(MessageParameter {
        param_type: PARAM_SUBSCRIBER_PRIORITY,
        value: MessageParameterValue::Uint8(5),
    });
    let err = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), params)
        .unwrap_err();
    match err {
        SendRequestError::Session(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("SendRequestError::Session が期待される: {other:?}"),
    }
    // subscription が登録されないこと
    assert_eq!(
        client.subscriptions().count(),
        0,
        "検証エラー時に subscription が登録されないこと"
    );
    // SendRequest イベントが積まれないこと
    let mut sent = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::SendRequest { .. }) {
            sent = true;
        }
    }
    assert!(!sent, "検証エラー時に SendRequest イベントが積まれないこと");
    // 検証エラー後に正常な送信が継続できること (フレッシュセッションでは最初の
    // 正常送信の request_id が 0 になる = 検証エラーで request_id が欠番にならない)
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("検証エラー後の正常送信は成功すること");
    assert_eq!(rid, 0, "検証エラーで request_id が欠番にならないこと");
}

/// スコープ外パラメータを含む PUBLISH の送信が API 呼び出し時に拒否されること
///
/// draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope): PUBLISH で許可されない
/// パラメータを含む送信は、検証がないと request 登録・ SendRequest の push まで完了した
/// 後に I/O 層のエンコード時で非同期に失敗する。検証は request_id 発行より前に置き、
/// エラー時に欠番を作らない。
#[test]
fn publish_with_out_of_scope_parameter_rejected_without_state_change() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES, PARAM_RENDEZVOUS_TIMEOUT,
    };
    use shiguredo_moqt::session::types::SendRequestError;
    let (mut client, _server) = establish_pair();
    // スコープ外の RENDEZVOUS_TIMEOUT (SUBSCRIBE のみ) と、スコープ内の EXPIRES を混在させる
    // (スコープ内パラメータを含んでも検証エラーになることを確認する。
    // SUBSCRIBER_PRIORITY は draft-20 で PUBLISH に出現可能なため使わない)
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_RENDEZVOUS_TIMEOUT,
        value: MessageParameterValue::VarInt(5),
    });
    params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(1000),
    });
    let err = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            104,
            params,
            TrackProperties::new(),
        )
        .unwrap_err();
    match err {
        SendRequestError::Session(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("SendRequestError::Session が期待される: {other:?}"),
    }
    // subscription が登録されないこと
    assert_eq!(
        client.subscriptions().count(),
        0,
        "検証エラー時に subscription が登録されないこと"
    );
    // SendRequest イベントが積まれないこと
    let mut sent = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::SendRequest { .. }) {
            sent = true;
        }
    }
    assert!(!sent, "検証エラー時に SendRequest イベントが積まれないこと");
    // 検証エラー後に正常な送信が継続できること (フレッシュセッションでは最初の
    // 正常送信の request_id が 0 になる = 検証エラーで request_id が欠番にならない)
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            105,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("検証エラー後の正常送信は成功すること");
    assert_eq!(rid, 0, "検証エラーで request_id が欠番にならないこと");
}

/// 値域外 GROUP_ORDER を含む PUBLISH の受信が `MessageParameters::decode` の値域検証で
/// 拒否されること
///
/// draft-ietf-moq-transport-21 §9.20.9: "The allowed values are Ascending (0x1) or
/// Descending (0x2)." 送信側は encode が先に失敗するため roundtrip では受信側に
/// 到達できないため、手作りバイト列で `MessageParameters::decode` を直接呼んで検証する。
/// 先頭バイトはパラメータ数 (count)、続いて delta-key (GROUP_ORDER = 0x22) と
/// 値 (Uint8(3)) のワイヤフォーマットに従う。
#[test]
fn publish_group_order_out_of_range_rejected_on_decode() {
    let bytes = [0x01u8, 0x22u8, 0x03u8]; // count=1, GROUP_ORDER=Uint8(3)
    assert!(
        MessageParameters::decode(&bytes).is_err(),
        "値域外 GROUP_ORDER はデコード時に拒否されること"
    );
}

/// GROUP_ORDER 未指定の PUBLISH にライブラリが GROUP_ORDER を自動注入しないこと
///
/// 伝播はアプリの責務であり、ライブラリは暗黙の注入を行わない
/// (アプリの明示的なパラメータ指定を尊重する API 哲学)。
#[test]
fn publish_without_group_order_no_auto_inject() {
    let (mut client, _server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            102,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (sent_rid, pub_msg) = take_send_request(&mut client);
    assert_eq!(sent_rid, rid);
    match &pub_msg {
        ControlMessage::Publish(p) => {
            assert_eq!(
                p.parameters.group_order(),
                None,
                "GROUP_ORDER 未指定の PUBLISH に自動注入されないこと"
            );
        }
        other => panic!("Publish が期待されたが {other:?} を受け取った"),
    }
}

/// スコープ外パラメータを含む PUBLISH_OK は subscription の状態を変更しない
///
/// draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope): スコープ外パラメータを含む
/// REQUEST_OK は、受信側が PROTOCOL_VIOLATION で接続を閉じる (MUST) ため、
/// 送信側でも防御的に検証して PROTOCOL_VIOLATION を返す。スコープ検証は状態遷移・
/// パラメータ適用より前に実行されるため、subscription は Pending(Publisher) のままで、
/// スコープ内パラメータ (EXPIRES) も適用されない。
#[test]
fn publish_ok_with_out_of_scope_parameter_keeps_subscription_pending() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES, PARAM_GROUP_ORDER,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            55,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // スコープ外の GROUP_ORDER とスコープ内の EXPIRES を混在させる
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(1),
    });
    ok_params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(5000),
    });
    let err = server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // subscription は Pending(Publisher) のまま (状態不変)
    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(sub.state, SubscriptionState::Pending);
    assert!(sub.is_pending_publisher());
    // スコープ内パラメータ (EXPIRES) も適用されていない
    assert_eq!(sub.expires, None);
    // REQUEST_OK が送信イベントとして積まれないこと
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::SendOnStream { .. }) {
            panic!("スコープ検証エラー時に REQUEST_OK は送信されないこと");
        }
    }
}

/// FORWARD を含む REQUEST_OK (Pending(Publisher) 分岐) で、subscription の状態が
/// 遷移せず・パラメータも適用されないこと
///
/// draft-ietf-moq-transport-21 Appendix A.1 #1790: FORWARD は PUBLISH_OK に出現しない
/// ため、スコープ検証で拒否される (値検証には到達しない)。スコープ検証は状態遷移・
/// パラメータ適用より前に実行されるため、subscription は Pending のまま維持される。
#[test]
fn request_ok_publish_with_forward_keeps_subscription_pending() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD, PARAM_SUBSCRIBER_PRIORITY,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            77,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // FORWARD=2 と SUBSCRIBER_PRIORITY を混在させる。draft-20 ではいずれも
    // PUBLISH_OK スコープ外のため、スコープ検証で拒否される
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(2),
    });
    ok_params.push(MessageParameter {
        param_type: PARAM_SUBSCRIBER_PRIORITY,
        value: MessageParameterValue::Uint8(5),
    });
    let err = server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // subscription は Pending(Publisher) のまま (状態不変)
    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(sub.state, SubscriptionState::Pending);
    assert!(sub.is_pending_publisher());
    // スコープ外パラメータ (SUBSCRIBER_PRIORITY) も適用されていない
    assert_eq!(sub.subscriber_priority, None);
    // REQUEST_OK が送信イベントとして積まれないこと
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::SendOnStream { .. }) {
            panic!("スコープ検証エラー時に REQUEST_OK は送信されないこと");
        }
    }
    // 検証失敗後も subscription は Pending(Publisher) のまま生きているため、
    // 正当なパラメータで再呼び出しすると Established に遷移できる (孤児化の解消)
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("スコープ検証エラー後の再送信は成功すること");
    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(sub.state, SubscriptionState::Established);
    assert!(!sub.is_pending_publisher());
}

/// LOCATION_FILTER を含む REQUEST_OK (Pending(Publisher) 分岐) で、
/// subscription の状態が遷移せず・パラメータも適用されないこと
///
/// draft-ietf-moq-transport-21 Appendix A.1 #1790: LOCATION_FILTER は PUBLISH_OK に
/// 出現しないため、スコープ検証で拒否される (値検証には到達しない)。
#[test]
fn request_ok_publish_with_location_filter_keeps_subscription_pending() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER, PARAM_SUBSCRIBER_PRIORITY,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            78,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 未知の Filter Type (0x5) を含む REQUEST_OK。draft-20 では LOCATION_FILTER 自体が
    // PUBLISH_OK スコープ外のため、スコープ検証で拒否される
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x05]),
    });
    ok_params.push(MessageParameter {
        param_type: PARAM_SUBSCRIBER_PRIORITY,
        value: MessageParameterValue::Uint8(5),
    });
    let err = server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // subscription は Pending(Publisher) のまま (状態不変)
    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(sub.state, SubscriptionState::Pending);
    assert!(sub.is_pending_publisher());
    // スコープ外パラメータ (SUBSCRIBER_PRIORITY) も適用されていない
    assert_eq!(sub.subscriber_priority, None);
    // REQUEST_OK が送信イベントとして積まれないこと
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::SendOnStream { .. }) {
            panic!("スコープ検証エラー時に REQUEST_OK は送信されないこと");
        }
    }
}

/// スコープ外パラメータを含む SUBSCRIBE_OK は送信が拒否され、subscription が Pending のまま・
/// track_alias も設定されないこと
///
/// draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope): SUBSCRIBE_OK で許可されるパラメータは
/// EXPIRES / LARGEST_OBJECT のみ (§9.20.17 / §9.20.18 の MAY appear 列挙)。受信側は
/// ワイヤ層で検証済みのため、ここは送信側の状態整合性のための検証。スコープ検証は
/// 状態遷移より前に実行されるため、subscription は Pending のまま維持される
/// (検証が遷移後に走ると、subscription が Established に遷移済みのまま後段のエンコード
/// 失敗で peer に SUBSCRIBE_OK が送られない孤児状態が残る)。
#[test]
fn subscribe_ok_with_out_of_scope_parameter_keeps_subscription_pending() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES, PARAM_SUBSCRIBER_PRIORITY,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // スコープ外の SUBSCRIBER_PRIORITY と、スコープ内の EXPIRES を混在させる
    // (EXPIRES の未適用が「パラメータが一切適用されない」ことの判別力を持つ)
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_SUBSCRIBER_PRIORITY,
        value: MessageParameterValue::Uint8(5),
    });
    ok_params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(1000),
    });
    let err = server
        .send_subscribe_ok(rid, 1, ok_params, TrackProperties::new())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // subscription は Pending(Subscriber) のまま (状態不変)
    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(sub.state, SubscriptionState::Pending);
    assert!(sub.is_pending_subscriber());
    // track_alias が設定されていない
    assert_eq!(sub.track_alias, None);
    // スコープ内パラメータ (EXPIRES) も適用されていない
    assert!(
        sub.expires.is_none(),
        "スコープ検証エラー時に EXPIRES は適用されないこと"
    );
    // SUBSCRIBE_OK が送信イベントとして積まれないこと
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::SendOnStream { .. }) {
            panic!("スコープ検証エラー時に SUBSCRIBE_OK は送信されないこと");
        }
    }
    // 検証失敗後も subscription は Pending(Subscriber) のまま生きているため、
    // 正当なパラメータで再呼び出しすると Established に遷移できる (孤児化の解消)
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("スコープ検証エラー後の再送信は成功すること");
    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(sub.state, SubscriptionState::Established);
    assert!(!sub.is_pending_subscriber());
    // 再送信成功時に SUBSCRIBE_OK が SendOnStream として積まれること
    let (stream_rid, ok_msg) = take_send_on_stream(&mut server);
    assert_eq!(stream_rid, rid);
    assert!(
        matches!(ok_msg, ControlMessage::SubscribeOk(_)),
        "再送信の SUBSCRIBE_OK が SendOnStream として積まれること"
    );
}

/// 値域違反の track_properties を含む SUBSCRIBE_OK は送信が拒否され、subscription が
/// Pending のまま・ track_alias も設定されないこと
///
/// draft-ietf-moq-transport-21 §10.4 (DEFAULT PUBLISHER PRIORITY): 0-255 のみ。
/// 検証がないと、状態遷移・ track_alias 代入・パラメータ適用の後に I/O 層のエンコード
/// (エンコードは sans-I/O のため I/O 層で行われる) で失敗し、subscription が Established
/// に遷移済みのまま peer に SUBSCRIBE_OK が送られない孤児状態が残る。
/// 値域違反の DEFAULT_PUBLISHER_PRIORITY と、正当な DYNAMIC_GROUPS (=1)・ EXPIRES を
/// 混在させる (両者の未適用が「track_properties 由来と MessageParameters 由来の両方の
/// 適用が行われない」ことの判別力を持つ)。
#[test]
fn subscribe_ok_with_invalid_track_properties_keeps_subscription_pending() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES,
    };
    use shiguredo_moqt::track_properties::{
        PROP_DEFAULT_PUBLISHER_PRIORITY, PROP_DYNAMIC_GROUPS, TrackProperty, TrackPropertyValue,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 値域違反 (300 > 255) の DEFAULT_PUBLISHER_PRIORITY と、正当な DYNAMIC_GROUPS=1 を混在させる
    let mut invalid_props = TrackProperties::new();
    invalid_props.push(TrackProperty {
        prop_type: PROP_DEFAULT_PUBLISHER_PRIORITY,
        value: TrackPropertyValue::VarInt(300),
    });
    invalid_props.push(TrackProperty {
        prop_type: PROP_DYNAMIC_GROUPS,
        value: TrackPropertyValue::VarInt(1),
    });
    // スコープ内の EXPIRES も混在させる (パラメータ適用の未実施の判別力)
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(1000),
    });
    let err = server
        .send_subscribe_ok(rid, 1, ok_params, invalid_props)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // subscription は Pending(Subscriber) のまま (状態不変)
    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(sub.state, SubscriptionState::Pending);
    assert!(sub.is_pending_subscriber());
    // track_alias が設定されていない
    assert_eq!(sub.track_alias, None);
    // track_properties 由来の適用 (DYNAMIC_GROUPS) も行われていない
    assert!(
        !sub.dynamic_groups,
        "track_properties 検証エラー時に DYNAMIC_GROUPS は適用されないこと"
    );
    // MessageParameters 由来の適用 (EXPIRES) も行われていない
    assert!(
        sub.expires.is_none(),
        "track_properties 検証エラー時に EXPIRES は適用されないこと"
    );
    // SUBSCRIBE_OK が送信イベントとして積まれないこと
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::SendOnStream { .. }) {
            panic!("track_properties 検証エラー時に SUBSCRIBE_OK は送信されないこと");
        }
    }
    // 検証失敗後も subscription は Pending(Subscriber) のまま生きているため、
    // 正当な track_properties で再呼び出しすると Established に遷移できる (孤児化の解消)
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("track_properties 検証エラー後の再送信は成功すること");
    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(sub.state, SubscriptionState::Established);
    // 再送信成功時に SUBSCRIBE_OK が SendOnStream として積まれること
    let (stream_rid, ok_msg) = take_send_on_stream(&mut server);
    assert_eq!(stream_rid, rid);
    assert!(
        matches!(ok_msg, ControlMessage::SubscribeOk(_)),
        "再送信の SUBSCRIBE_OK が SendOnStream として積まれること"
    );
}

/// 値域違反以外の不正 track_properties を含む SUBSCRIBE_OK も送信が拒否され、
/// subscription が Pending のまま維持されること
///
/// `TrackProperties::encode` の事前検証が、値域違反 (GROUP_ORDER / DYNAMIC_GROUPS) に加えて
/// 型不整合・同一 prop_type の重複・ IMMUTABLE_PROPERTIES 内側の値域違反も弾くことを
/// 確認する (受信側ワイヤ層の検証と同じ内容)。各ケースとも subscription は Pending の
/// まま維持される。
#[test]
fn subscribe_ok_with_invalid_track_properties_variants_rejected() {
    use shiguredo_moqt::track_properties::{
        PROP_DEFAULT_PUBLISHER_GROUP_ORDER, PROP_DEFAULT_PUBLISHER_PRIORITY, PROP_DYNAMIC_GROUPS,
        PROP_IMMUTABLE_PROPERTIES, TrackProperty, TrackPropertyValue,
    };
    // 各不正 track_properties のフィクスチャ (値域違反 2 種・型不整合・重複・ IMMUTABLE 内側)
    let cases: Vec<Vec<TrackProperty>> = vec![
        // DEFAULT_PUBLISHER_GROUP_ORDER が 1 / 2 以外 (draft-ietf-moq-transport-21 §10.5)
        vec![TrackProperty {
            prop_type: PROP_DEFAULT_PUBLISHER_GROUP_ORDER,
            value: TrackPropertyValue::VarInt(3),
        }],
        // DYNAMIC_GROUPS が 0 / 1 以外 (draft-ietf-moq-transport-21 §10.6)
        vec![TrackProperty {
            prop_type: PROP_DYNAMIC_GROUPS,
            value: TrackPropertyValue::VarInt(2),
        }],
        // 型不整合: 偶数型 (0x30) に Bytes 値
        vec![TrackProperty {
            prop_type: PROP_DYNAMIC_GROUPS,
            value: TrackPropertyValue::Bytes(vec![]),
        }],
        // 同一 prop_type の重複
        vec![
            TrackProperty {
                prop_type: PROP_DEFAULT_PUBLISHER_PRIORITY,
                value: TrackPropertyValue::VarInt(1),
            },
            TrackProperty {
                prop_type: PROP_DEFAULT_PUBLISHER_PRIORITY,
                value: TrackPropertyValue::VarInt(2),
            },
        ],
        // IMMUTABLE_PROPERTIES 内側の値域違反 (内側 KVP 列に DYNAMIC_GROUPS=2)
        vec![TrackProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            value: TrackPropertyValue::Bytes(vec![0x0B, 0x02, 0x30, 0x02]),
        }],
    ];
    for props in cases {
        let (mut client, mut server) = establish_pair();
        let rid = client
            .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, sub_msg) = take_send_request(&mut client);
        server
            .recv_request(sub_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        let mut invalid_props = TrackProperties::new();
        for p in props {
            invalid_props.push(p);
        }
        let err = server
            .send_subscribe_ok(rid, 1, MessageParameters::new(), invalid_props)
            .unwrap_err();
        assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
        // subscription は Pending(Subscriber) のまま (状態不変)
        let sub = server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            sub.state,
            SubscriptionState::Pending,
            "不正な track_properties では subscription が Pending のまま維持されること"
        );
        assert_eq!(
            sub.track_alias, None,
            "不正な track_properties では track_alias が設定されないこと"
        );
    }
}

/// 正常系: REQUEST_UPDATE_OK (subscription) に EXPIRES (許可集合内) を載せると受理される
#[test]
fn request_update_ok_subscription_with_expires_accepted() {
    let (mut client, mut server, rid) = establish_subscribe_track(52);
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, expires_params(60000), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, reqok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, reqok)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established
    );
}

/// 正常系: REQUEST_UPDATE_OK (fetch) に EXPIRES / LARGEST_OBJECT (許可集合内) を
/// 載せると受理される
#[test]
fn request_update_ok_fetch_with_expires_and_largest_object_accepted() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES, PARAM_LARGEST_OBJECT,
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
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(1000),
    });
    ok_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 5,
            object: 3,
        },
    });
    server
        .send_request_ok(rid, ok_params, TrackProperties::default())
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
}

// ─── send_request_ok 送信側検証 ─────────────────

/// TRACK_STATUS_OK に LARGEST_OBJECT を載せて送信すると成功する
/// (draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter): LARGEST_OBJECT のみ許可)
#[test]
fn send_track_status_ok_with_largest_object_accepted() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ts_msg) = take_send_request(&mut client);
    server
        .recv_request(ts_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 1,
            object: 2,
        },
    });
    server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
}

/// TRACK_STATUS_OK に OBJECT_DELIVERY_TIMEOUT を載せて送信すると PROTOCOL_VIOLATION
/// (draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter): TRACK_STATUS_OK は LARGEST_OBJECT のみ許可。
/// 送信側が `send_request_ok` で検出する)
#[test]
fn send_track_status_ok_with_object_delivery_timeout_rejected() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ts_msg) = take_send_request(&mut client);
    server
        .recv_request(ts_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = server
        .send_request_ok(
            rid,
            delivery_timeout_params(5000),
            TrackProperties::default(),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// TRACK_STATUS_OK に非空 TrackProperties を載せて送信すると成功する
/// (TRACK_STATUS_OK は TrackProperties を許容する唯一の REQUEST_OK context)
#[test]
fn send_track_status_ok_with_non_empty_track_properties_accepted() {
    use shiguredo_moqt::track_properties::{
        PROP_OBJECT_DELIVERY_TIMEOUT, TrackProperty, TrackPropertyValue,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ts_msg) = take_send_request(&mut client);
    server
        .recv_request(ts_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(5000),
    });
    server
        .send_request_ok(rid, MessageParameters::new(), tp)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
}

/// REQUEST_UPDATE_OK (fetch) に OBJECT_DELIVERY_TIMEOUT を載せると
/// send_request_ok で PROTOCOL_VIOLATION (fetch の REQUEST_UPDATE_OK は EXPIRES / LARGEST_OBJECT のみ許可)
#[test]
fn send_request_ok_for_fetch_with_object_delivery_timeout_rejected() {
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
                    group_id: 2,
                    object_id: 0,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // FETCH_OK で fetch を確立する
    server
        .send_fetch_ok(
            rid,
            0,
            Location {
                group_id: 1,
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
    // REQUEST_UPDATE_OK 相当で send_request_ok を呼ぶ
    let err = server
        .send_request_ok(
            rid,
            delivery_timeout_params(5000),
            TrackProperties::default(),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// スコープ外パラメータを含む FETCH_OK は送信が拒否され、fetch が Pending のまま・
/// SendOnStream イベントが積まれないこと
///
/// draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope): FETCH_OK はパラメータを運べない
/// (FETCH_OK_ALLOWED_PARAMS は空集合。§9.12 (FETCH_OK) の Parameters フィールドに
/// 許可されるパラメータは 0 個)。受信側はワイヤ層で検証済みのため、ここは送信側の
/// 状態整合性のための検証。スコープ検証は状態遷移より前に実行されるため、
/// fetch は Pending のまま維持される (検証が遷移後に走ると、fetch が Established に
/// 遷移済みのまま後段のエンコード失敗で peer に FETCH_OK が送られない孤児状態が残る)。
#[test]
fn fetch_ok_with_out_of_scope_parameter_keeps_fetch_pending() {
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
    // FETCH リクエストでは許可されるが FETCH_OK では許可されない SUBSCRIBER_PRIORITY を
    // 使う (FETCH_ALLOWED_PARAMS と FETCH_OK_ALLOWED_PARAMS を誤って共用するバグを検出できる)
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_SUBSCRIBER_PRIORITY,
        value: MessageParameterValue::Uint8(5),
    });
    let err = server
        .send_fetch_ok(
            rid,
            0,
            Location {
                group_id: 5,
                object_id: 0,
            },
            ok_params,
            TrackProperties::new(),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // fetch は Pending のまま (状態不変)
    assert_eq!(
        server.fetch(rid).expect("fetch が存在する").state,
        FetchState::Pending,
        "スコープ検証エラー時に fetch は Pending のままであること"
    );
    // FETCH_OK が送信イベントとして積まれないこと
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::SendOnStream { .. }) {
            panic!("スコープ検証エラー時に FETCH_OK は送信されないこと");
        }
    }
    // 検証失敗後も fetch は Pending のまま生きているため、
    // 正当なパラメータで再呼び出しすると Established に遷移できる (孤児化の解消)
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
        .expect("スコープ検証エラー後の再送信は成功すること");
    assert_eq!(
        server.fetch(rid).expect("fetch が存在する").state,
        FetchState::Established,
        "再送信で Established に遷移できること"
    );
    // 再送信成功時に FETCH_OK が SendOnStream として積まれること
    let (stream_rid, ok_msg) = take_send_on_stream(&mut server);
    assert_eq!(stream_rid, rid);
    assert!(
        matches!(ok_msg, ControlMessage::FetchOk(_)),
        "再送信の FETCH_OK が SendOnStream として積まれること"
    );
}

/// REQUEST_UPDATE_OK (fetch) に非空 TrackProperties を渡すと
/// send_request_ok で PROTOCOL_VIOLATION (draft §9.3 (REQUEST_OK): fetch は TrackProperties 空必須)
#[test]
fn send_request_ok_for_fetch_with_non_empty_track_properties_rejected() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::track_properties::{
        PROP_OBJECT_DELIVERY_TIMEOUT, TrackProperty, TrackPropertyValue,
    };
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
                    group_id: 2,
                    object_id: 0,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // FETCH_OK で fetch を確立する
    server
        .send_fetch_ok(
            rid,
            0,
            Location {
                group_id: 1,
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
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(5000),
    });
    let err = server
        .send_request_ok(rid, MessageParameters::new(), tp)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// PUBLISH_NAMESPACE_OK に SUBSCRIBER_PRIORITY を含む parameters を渡すと
/// send_request_ok で PROTOCOL_VIOLATION (namespace 系 OK はパラメータを許可しない)
#[test]
fn send_request_ok_for_namespace_publication_with_subscriber_priority_rejected() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_SUBSCRIBER_PRIORITY,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_SUBSCRIBER_PRIORITY,
        value: MessageParameterValue::Uint8(1),
    });
    let err = server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// PUBLISH_NAMESPACE_OK に非空 TrackProperties を渡すと send_request_ok で PROTOCOL_VIOLATION
/// (draft §9.3 (REQUEST_OK): TRACK_STATUS_OK 以外は TrackProperties 空必須)
#[test]
fn send_request_ok_for_namespace_publication_with_non_empty_track_properties_rejected() {
    use shiguredo_moqt::track_properties::{
        PROP_OBJECT_DELIVERY_TIMEOUT, TrackProperty, TrackPropertyValue,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(5000),
    });
    let err = server
        .send_request_ok(rid, MessageParameters::new(), tp)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// SUBSCRIBE_NAMESPACE_OK に非空 TrackProperties を渡すと send_request_ok で PROTOCOL_VIOLATION
#[test]
fn send_request_ok_for_namespace_subscription_with_non_empty_track_properties_rejected() {
    use shiguredo_moqt::track_properties::{
        PROP_OBJECT_DELIVERY_TIMEOUT, TrackProperty, TrackPropertyValue,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(5000),
    });
    let err = server
        .send_request_ok(rid, MessageParameters::new(), tp)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// SUBSCRIBE_TRACKS_OK に非空 TrackProperties を渡すと send_request_ok で PROTOCOL_VIOLATION
#[test]
fn send_request_ok_for_track_subscription_with_non_empty_track_properties_rejected() {
    use shiguredo_moqt::track_properties::{
        PROP_OBJECT_DELIVERY_TIMEOUT, TrackProperty, TrackPropertyValue,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_tracks(ns(&[b"example"]), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(5000),
    });
    let err = server
        .send_request_ok(rid, MessageParameters::new(), tp)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// PUBLISH_OK (Subscription context) に非空 TrackProperties を渡すと
/// send_request_ok で PROTOCOL_VIOLATION
///
/// draft-ietf-moq-transport-21 §9.3 (REQUEST_OK): PUBLISH_OK を含む全非 TRACK_STATUS_OK context で
/// TrackProperties は空でなければならない。
#[test]
fn send_request_ok_for_subscription_with_non_empty_track_properties_rejected() {
    use shiguredo_moqt::track_properties::{
        PROP_OBJECT_DELIVERY_TIMEOUT, TrackProperty, TrackPropertyValue,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            1,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(5000),
    });
    let err = server
        .send_request_ok(rid, MessageParameters::new(), tp)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

// ─── LARGEST_OBJECT の PUBLISH_OK 非包含にともなう非回帰テスト ─────────────────

/// 送信側非回帰: PUBLISH_OK 送信時に LARGEST_OBJECT が自動注入されない
///
/// subscription に largest_location が設定されていても、PUBLISH_OK context では
/// LARGEST_OBJECT が許可されていないため `update_largest_object_in_parameters`
/// による注入が行われないことを確認する。
#[test]
fn publish_ok_does_not_contain_largest_object() {
    use shiguredo_moqt::message::ControlMessage;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
    };
    let (mut client, mut server) = establish_pair();
    // PUBLISH に LARGEST_OBJECT を含めて送信し、server 側 subscription に largest_location を設定する
    let mut pub_params = MessageParameters::new();
    pub_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 5,
            object: 3,
        },
    });
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            61,
            pub_params,
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // server 側 subscription に largest_location が保存されている
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location,
        Some(Location {
            group_id: 5,
            object_id: 3,
        }),
    );
    // 空パラメータで PUBLISH_OK を送信する
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    // 送信された REQUEST_OK に LARGEST_OBJECT が含まれていないことを確認する
    if let ControlMessage::RequestOk(ok) = &ok_msg {
        assert!(
            ok.parameters.largest_object().is_none(),
            "PUBLISH_OK に LARGEST_OBJECT が自動注入されてはならない"
        );
    } else {
        panic!("RequestOk が期待されたが {ok_msg:?} を受け取った");
    }
    // client 側でも正常に受信できる
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established
    );
}

/// 送信側非回帰: REQUEST_UPDATE_OK には LARGEST_OBJECT が自動注入される
///
/// subscription に largest_location が設定されている場合、REQUEST_UPDATE_OK context では
/// LARGEST_OBJECT が許可されているため `update_largest_object_in_parameters`
/// による注入が行われることを確認する。
#[test]
fn request_update_ok_contains_largest_object() {
    use shiguredo_moqt::message::ControlMessage;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
    };
    let (mut client, mut server) = establish_pair();
    // PUBLISH に LARGEST_OBJECT を含めて送信し、server 側 subscription に largest_location を設定する
    let mut pub_params = MessageParameters::new();
    pub_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 7,
            object: 2,
        },
    });
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            62,
            pub_params,
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // PUBLISH_OK で subscription を Established にする
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // client から REQUEST_UPDATE を送る
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 空パラメータで REQUEST_UPDATE_OK を送信する
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, reqok_msg) = take_send_on_stream(&mut server);
    // 送信された REQUEST_OK に LARGEST_OBJECT が自動注入されていることを確認する
    if let ControlMessage::RequestOk(ok) = &reqok_msg {
        assert_eq!(
            ok.parameters.largest_object(),
            Some((7, 2)),
            "REQUEST_UPDATE_OK に LARGEST_OBJECT が自動注入されなければならない"
        );
    } else {
        panic!("RequestOk が期待されたが {reqok_msg:?} を受け取った");
    }
    // client 側でも正常に受信できる
    client
        .recv_stream_message(rid, reqok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
}

/// 受信側非回帰: PUBLISH_OK に LARGEST_OBJECT 以外の許可パラメータのみ含まれれば通常処理される
///
/// PUBLISH_OK から LARGEST_OBJECT が除外されても、他の許可パラメータ
/// (ここでは EXPIRES) の受信・処理に影響がないことを確認する。
#[test]
fn publish_ok_with_allowed_params_other_than_largest_object_accepted() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            63,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // EXPIRES は PUBLISH_OK context で許可されている
    server
        .send_request_ok(rid, expires_params(30000), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established
    );
}

/// 広告値保存経路の非回帰: PUBLISH 受信で保存された largest_location が
/// PUBLISH_OK 送信後も保持される
///
/// PUBLISH の LARGEST_OBJECT から保存された広告値は、PUBLISH_OK に
/// LARGEST_OBJECT が含まれなくても上書きされず保持される。
#[test]
fn publish_advertised_largest_preserved_after_publish_ok() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
    };
    let (mut client, mut server) = establish_pair();
    // PUBLISH に LARGEST_OBJECT を含めて送信する
    let mut pub_params = MessageParameters::new();
    pub_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 10,
            object: 4,
        },
    });
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            64,
            pub_params,
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // PUBLISH 受信時点で largest_location が保存されている
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location,
        Some(Location {
            group_id: 10,
            object_id: 4,
        }),
    );
    // PUBLISH_OK を送信する (LARGEST_OBJECT は含まれない)
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // PUBLISH_OK 送信後も server 側 subscription の largest_location が保持されている
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location,
        Some(Location {
            group_id: 10,
            object_id: 4,
        }),
        "PUBLISH の LARGEST_OBJECT から保存された広告値が PUBLISH_OK 送信後も保持されなければならない"
    );
}

/// FORWARD を含む REQUEST_OK (Pending(Publisher) 分岐) の受信で、subscription が
/// Established に遷移せず・パラメータも適用されないこと
///
/// draft-ietf-moq-transport-21 Appendix A.1 #1790: FORWARD は PUBLISH_OK に出現しない
/// ため、スコープ検証で PROTOCOL_VIOLATION により閉じる。スコープ検証は状態遷移・
/// パラメータ適用より前に実行されるため、部分適用は残留しない。
/// セッションを PROTOCOL_VIOLATION で閉じる (MUST) ため、CloseSession の発生も検証する。
#[test]
fn request_ok_publish_with_forward_keeps_subscription_pending_on_recv() {
    use shiguredo_moqt::message::RequestOk;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD, PARAM_OBJECT_DELIVERY_TIMEOUT,
    };
    let (mut client, mut server) = establish_pair();
    // client=publisher が PUBLISH を送信し、Pending(Publisher) を作る
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            100,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 有効値の FORWARD=0 と OBJECT_DELIVERY_TIMEOUT を混在させる。
    // いずれも PUBLISH_OK スコープ外のため、スコープ検証で拒否される
    // (delivery timeout の未適用が判別力を持つ)
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    ok_params.push(MessageParameter {
        param_type: PARAM_OBJECT_DELIVERY_TIMEOUT,
        value: MessageParameterValue::VarInt(500),
    });
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(RequestOk {
                parameters: ok_params,
                track_properties: TrackProperties::new(),
            }),
        )
        .expect_err("値域外 FORWARD は PROTOCOL_VIOLATION になる");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // subscription は Pending(Publisher) のまま (状態不変)
    let sub = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(sub.state, SubscriptionState::Pending);
    assert!(sub.is_pending_publisher());
    // delivery timeout が適用されていないこと (部分適用の残留がない)
    assert_eq!(
        sub.delivery_timeouts.subscriber_object_ms, None,
        "検証エラー時に OBJECT_DELIVERY_TIMEOUT は適用されないこと"
    );
    // セッションが PROTOCOL_VIOLATION で閉じること (FORWARD 値域違反の MUST)
    match drain_until_close(&mut client) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("CloseSession(PROTOCOL_VIOLATION) が期待されたが {other:?}"),
    }
    // RequestOkReceived イベントが積まれないこと
    // (CloseSession を先に消費してから確認する。イベントは CloseSession が最後に積まれる)
    let mut got = false;
    while let Some(ev) = client.poll_event() {
        if matches!(ev, SessionEvent::RequestOkReceived { .. }) {
            got = true;
        }
    }
    assert!(
        !got,
        "スコープ検証エラー時に RequestOkReceived が積まれないこと"
    );
}

/// LOCATION_FILTER を含む REQUEST_OK (Pending(Publisher) 分岐) の受信で、
/// subscription が Established に遷移せず・パラメータも適用されないこと
///
/// draft-ietf-moq-transport-21 Appendix A.1 #1790: LOCATION_FILTER は PUBLISH_OK に
/// 出現しないため、スコープ検証で PROTOCOL_VIOLATION により閉じる (値検証には
/// 到達しないことを意図どおりとする)。
#[test]
fn request_ok_publish_with_location_filter_keeps_subscription_pending_on_recv() {
    use shiguredo_moqt::message::RequestOk;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            101,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // LOCATION_FILTER を含む REQUEST_OK を client (publisher 役) に直接流し込む
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x05]),
    });
    let err = client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(RequestOk {
                parameters: ok_params,
                track_properties: TrackProperties::new(),
            }),
        )
        .expect_err("LOCATION_FILTER を含む PUBLISH_OK は PROTOCOL_VIOLATION になる");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // subscription は Pending(Publisher) のまま (状態不変)
    let sub = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(sub.state, SubscriptionState::Pending);
    assert!(sub.is_pending_publisher());
    // セッションが PROTOCOL_VIOLATION で閉じること (Location Filter は fail + Err)
    match drain_until_close(&mut client) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("CloseSession(PROTOCOL_VIOLATION) が期待されたが {other:?}"),
    }
}
