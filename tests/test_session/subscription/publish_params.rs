//! PUBLISH の subscription パラメータのテスト
//! (draft-ietf-moq-transport-21 §9.8 (PUBLISH) / §9.18.1 / §9.20.3)
//!
//! PUBLISH への subscription パラメータ出現、SUBSCRIBE_TRACKS からの伝播
//! (auth token 除外)、受信時の初期状態への反映を扱う。
//! 伝播の濾過規則自体は `test_message_parameter.rs` の単体テストで固定し、
//! 本ファイルでは送受信の往復と状態反映に焦点を当てる。

use super::*;
use shiguredo_moqt::message::Publish;
use shiguredo_moqt::message_parameter::{
    MessageParameter, MessageParameterValue, PARAM_AUTHORIZATION_TOKEN, PARAM_EXPIRES,
    PARAM_FORWARD, PARAM_GROUP_ORDER, PARAM_LARGEST_OBJECT, PARAM_LOCATION_FILTER,
    PARAM_OBJECT_DELIVERY_TIMEOUT, PARAM_SUBGROUP_DELIVERY_TIMEOUT, PARAM_SUBGROUP_FILTER,
    PARAM_SUBSCRIBER_PRIORITY, PARAM_TRACK_PROPERTY_FILTER,
};
use shiguredo_moqt::track_properties::{
    PROP_OBJECT_DELIVERY_TIMEOUT, PROP_SUBGROUP_DELIVERY_TIMEOUT, TrackProperty, TrackPropertyValue,
};
use shiguredo_moqt::{
    message::common::Location, message_parameter::AuthorizationToken,
    message_parameter::LocationFilter,
};

/// SUBSCRIBE_TRACKS 相当のパラメータ群を作る
/// (FORWARD / GROUP_ORDER / auth token / PUBLISH 非出現型を含む)
///
/// Range Filter を含むため、受信側は MAX_FILTER_RANGES 宣言が必要である。
/// 呼び出し側で `establish_pair_with_options` を使うこと。
fn tracks_params() -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_AUTHORIZATION_TOKEN,
        value: MessageParameterValue::AuthorizationToken(AuthorizationToken::UseValue {
            token_type: 7,
            token_value: b"secret".to_vec(),
        }),
    });
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(1),
    });
    params.push(MessageParameter {
        param_type: PARAM_SUBGROUP_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x00, 0x00, 0x01]),
    });
    params.push(MessageParameter {
        param_type: PARAM_TRACK_PROPERTY_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x00, 0x02, 0x00, 0x01]),
    });
    params
}

/// PUBLISH の subscription パラメータが送受信できる
/// (draft-ietf-moq-transport-21 §9.8 (PUBLISH))
#[test]
fn publish_with_subscription_parameters_round_trip() {
    let (mut client, mut server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_SUBSCRIBER_PRIORITY,
        value: MessageParameterValue::Uint8(64),
    });
    params.push(MessageParameter {
        param_type: PARAM_OBJECT_DELIVERY_TIMEOUT,
        value: MessageParameterValue::VarInt(100),
    });
    params.push(MessageParameter {
        param_type: PARAM_SUBGROUP_DELIVERY_TIMEOUT,
        value: MessageParameterValue::VarInt(200),
    });
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteRange {
                start: Location {
                    group_id: 0,
                    object_id: 0,
                },
                end_group_delta: 0,
            }
            .encode_to_bytes(),
        ),
    });
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            111,
            params,
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    // ワイヤ上には timeout 値が載ったまま届く (状態には保持しない)
    match &msg {
        ControlMessage::Publish(publish) => {
            assert_eq!(
                publish.parameters.object_delivery_timeout(),
                Some(100),
                "timeout 値はワイヤ上に残ること"
            );
            assert_eq!(
                publish.parameters.subgroup_delivery_timeout(),
                Some(200),
                "timeout 値はワイヤ上に残ること"
            );
        }
        other => panic!("Publish が期待されたが {other:?} を受け取った"),
    }
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 送信側も初期パラメータを保持する (受信側と対称)
    let sender_sub = client
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        sender_sub.subscriber_priority,
        Some(64),
        "送信側も priority を保持すること"
    );
    assert!(
        sender_sub.filter.is_some(),
        "送信側も filter を保持すること"
    );
    assert_eq!(
        sender_sub.filter_start,
        Some(Location {
            group_id: 0,
            object_id: 0
        }),
        "送信側も解決済み Start を保持すること"
    );
    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        sub.subscriber_priority,
        Some(64),
        "受信側も priority を保持すること"
    );
    assert_eq!(
        sub.group_order, None,
        "未指定の group_order は None のままであること"
    );
    assert!(sub.filter.is_some(), "受信側も filter を保持すること");
}

/// PUBLISH の delivery timeout パラメータは Track Properties の値を用い、
/// Parameters の値は状態に保持しない
/// (draft-ietf-moq-transport-21 §5.2: publisher は Track Property で伝える)
#[test]
fn publish_timeout_parameters_not_retained() {
    let (mut client, mut server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_OBJECT_DELIVERY_TIMEOUT,
        value: MessageParameterValue::VarInt(100),
    });
    params.push(MessageParameter {
        param_type: PARAM_SUBGROUP_DELIVERY_TIMEOUT,
        value: MessageParameterValue::VarInt(200),
    });
    // Track Properties 側の値を 300 / 400 にして区別する
    let mut track_properties = TrackProperties::new();
    track_properties.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(300),
    });
    track_properties.push(TrackProperty {
        prop_type: PROP_SUBGROUP_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(400),
    });
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            111,
            params,
            track_properties,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 送受信とも effective timeout は Track Properties 由来の 300 / 400 になる
    for session in [&client, &server] {
        let sub = session
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            sub.delivery_timeouts.effective_object_ms,
            Some(300),
            "PUBLISH パラメータの timeout は effective に寄与しないこと"
        );
        assert_eq!(
            sub.delivery_timeouts.effective_subgroup_ms,
            Some(400),
            "PUBLISH パラメータの timeout は effective に寄与しないこと"
        );
    }
}

/// SUBSCRIBE_TRACKS の Parameters が auth token 除外で PUBLISH に伝播する
/// (draft-ietf-moq-transport-21 §9.18.1 / §9.20.3)
#[test]
fn subscribe_tracks_params_propagate_to_publish_without_auth_token() {
    // Range Filter を含むため server 側に MAX_FILTER_RANGES を宣言させる
    let (mut client, mut server) =
        establish_pair_with_options(SetupOptions::new(), opts_with(0x06, 10));
    // client が SUBSCRIBE_TRACKS を送る
    client
        .send_subscribe_tracks(ns(&[b"example"]), tracks_params())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, tracks_msg) = take_send_request(&mut client);
    server
        .recv_request(tracks_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // server が SubscribeTracksReceived を受け、helper で PUBLISH パラメータを導出する
    let received = take_subscribe_tracks_params(&mut server);
    assert_eq!(
        received.authorization_tokens().len(),
        1,
        "受信した SUBSCRIBE_TRACKS には auth token が含まれること"
    );
    let derived = received.resulting_publish_parameters();
    assert!(
        derived.authorization_tokens().is_empty(),
        "導出パラメータに auth token が含まれないこと"
    );
    assert_eq!(derived.forward(), Some(0), "FORWARD が複写されること");
    assert_eq!(
        derived.group_order(),
        Some(1),
        "GROUP_ORDER が複写されること"
    );
    // server が resulting PUBLISH を送る。
    // TRACK_PROPERTY_FILTER (SetID 0 / Property Type 2 / range 0-1) を通る
    // Track Properties (Property Type 2 = テスト用の任意値) を添える。
    // filter 自体は PUBLISH に載らない。
    let mut track_properties = TrackProperties::new();
    track_properties.push(TrackProperty {
        prop_type: 2,
        value: TrackPropertyValue::VarInt(0),
    });
    let pub_rid = server
        .send_publish(
            ns(&[b"example", b"live"]),
            b"cam".to_vec(),
            777,
            derived.clone(),
            track_properties,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut server);
    // ワイヤ equality (auth token 除外): 受信パラメータが導出値と一致する
    let published = match &pub_msg {
        ControlMessage::Publish(publish) => publish.parameters.clone(),
        other => panic!("Publish が期待されたが {other:?} を受け取った"),
    };
    assert_eq!(published.forward(), Some(0));
    assert_eq!(published.group_order(), Some(1));
    assert!(published.authorization_tokens().is_empty());
    // 非出現型 (Range Filter / TRACK_PROPERTY_FILTER) はワイヤに載らない
    assert!(!published.has_range_filters());
    assert!(published.track_namespace_prefix().is_none());
    let published_types: Vec<u64> = published.as_slice().iter().map(|p| p.param_type).collect();
    assert_eq!(
        published_types,
        derived
            .as_slice()
            .iter()
            .map(|p| p.param_type)
            .collect::<Vec<u64>>(),
        "受信パラメータは導出値と一致すること"
    );
    client
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 受信側は起因判別で拒否せず、初期状態に反映する
    let sub = client
        .subscription(pub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(sub.forward_state, 0, "伝播した FORWARD が反映されること");
    assert_eq!(
        sub.group_order,
        Some(1),
        "伝播した GROUP_ORDER が反映されること"
    );
}

/// PUBLISH 受信で初期 subscription パラメータが状態に反映される
/// (draft-ietf-moq-transport-21 §9.8 (PUBLISH))
#[test]
fn publish_recv_reflects_initial_parameters() {
    let (mut client, mut server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(2),
    });
    params.push(MessageParameter {
        param_type: PARAM_SUBSCRIBER_PRIORITY,
        value: MessageParameterValue::Uint8(64),
    });
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteStart {
                start: Location {
                    group_id: 3,
                    object_id: 0,
                },
            }
            .encode_to_bytes(),
        ),
    });
    params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 3,
            object: 0,
        },
    });
    params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(5000),
    });
    let pub_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            params,
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut server);
    client
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = client
        .subscription(pub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(sub.forward_state, 0, "PUBLISH の FORWARD が反映されること");
    assert_eq!(sub.group_order, Some(2), "GROUP_ORDER が反映されること");
    assert_eq!(
        sub.subscriber_priority,
        Some(64),
        "SUBSCRIBER_PRIORITY が反映されること"
    );
    assert!(sub.filter.is_some(), "LOCATION_FILTER が反映されること");
    assert!(sub.expires.is_some(), "PUBLISH の EXPIRES が反映されること");
    // 送信側は forward=0 の確立では広告値を保存しない (受信側と非対称。
    // publisher 側の保存は廃止され、観測値は largest_received_location で追跡する)
    assert!(
        server
            .subscription(pub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location
            .is_none(),
        "forward=0 確立では送信側 largest_location は保存されないこと"
    );
    assert_eq!(
        sub.filter_start,
        Some(Location {
            group_id: 3,
            object_id: 0
        })
    );
    assert_eq!(
        sub.largest_location,
        Some(Location {
            group_id: 3,
            object_id: 0
        })
    );
}

/// 値域外 GROUP_ORDER の PUBLISH 受信は PROTOCOL_VIOLATION でセッションを閉じる
/// (draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter))
#[test]
fn publish_recv_with_invalid_group_order_closes_session() {
    let (mut _client, mut server) = establish_pair();
    // decode 層を迂回して不正値を直接受信させる (decode 層も値域検証するため)
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(3),
    });
    let err = server
        .recv_request(ControlMessage::Publish(Publish {
            request_id: 2,
            track_namespace: ns(&[b"live"]),
            track_name: b"cam".to_vec(),
            track_alias: 999,
            parameters: params,
            track_properties: TrackProperties::new(),
        }))
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("セッションエラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    assert_eq!(server.state(), SessionState::Closing);
    assert_eq!(
        server.subscriptions().count(),
        0,
        "拒否した PUBLISH は登録されないこと"
    );
}

/// 不正形式 filter の PUBLISH 受信は PROTOCOL_VIOLATION でセッションを閉じる
#[test]
fn publish_recv_with_malformed_filter_closes_session() {
    let (mut _client, mut server) = establish_pair();
    // EndGroupDelta 溢出の LOCATION_FILTER を直接受信させる
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![
            0x01, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ]),
    });
    let err = server
        .recv_request(ControlMessage::Publish(Publish {
            request_id: 2,
            track_namespace: ns(&[b"live"]),
            track_name: b"cam".to_vec(),
            track_alias: 999,
            parameters: params,
            track_properties: TrackProperties::new(),
        }))
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("セッションエラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    assert_eq!(server.state(), SessionState::Closing);
    assert_eq!(
        server.subscriptions().count(),
        0,
        "拒否した PUBLISH は登録されないこと"
    );
}

/// 不正形式 filter の PUBLISH 送信は送信前に拒否し副作用を残さない
#[test]
fn send_publish_with_malformed_filter_rejected_without_side_effects() {
    let (mut client, _server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![
            0x01, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ]),
    });
    let err = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            111,
            params,
            TrackProperties::new(),
        )
        .expect_err("不正形式 filter の PUBLISH は送信前に拒否されること");
    assert_eq!(
        err.as_session_error()
            .expect("セッションエラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    assert_eq!(client.subscriptions().count(), 0);
    while let Some(e) = client.poll_event() {
        assert!(
            !matches!(
                e,
                SessionEvent::SendRequest { .. } | SessionEvent::CloseSession(_)
            ),
            "拒否した送信で SendRequest / CloseSession は発行されないこと"
        );
    }
    // 欠番なし: 次の送信は先頭 ID で発行される
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            111,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("拒否後の正常な送信ができること");
    let (sent_rid, _) = take_send_request(&mut client);
    assert_eq!(sent_rid, rid);
    assert_eq!(rid, 0, "拒否で request ID を消費しないこと");
}

/// SubscribeTracksReceived イベントのパラメータを取り出す
fn take_subscribe_tracks_params(server: &mut Session) -> MessageParameters {
    while let Some(e) = server.poll_event() {
        if let SessionEvent::SubscribeTracksReceived { parameters, .. } = e {
            return parameters;
        }
    }
    panic!("SubscribeTracksReceived イベントが期待されたが発行されなかった");
}
