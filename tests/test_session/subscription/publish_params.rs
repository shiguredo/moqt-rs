//! PUBLISH の subscription パラメータのテスト
//! (draft-ietf-moq-transport-21 §9.8 (PUBLISH))
//!
//! PUBLISH への subscription パラメータ出現と、受信時の初期状態への反映を扱う。
//! 本ファイルでは送受信の往復と状態反映に焦点を当てる。

use super::*;
use shiguredo_moqt::message::Publish;
use shiguredo_moqt::message::common::Location;
use shiguredo_moqt::message_parameter::{
    LocationFilter, MessageParameter, MessageParameterValue, PARAM_EXPIRES, PARAM_FORWARD,
    PARAM_GROUP_ORDER, PARAM_LARGEST_OBJECT, PARAM_LOCATION_FILTER, PARAM_OBJECT_DELIVERY_TIMEOUT,
    PARAM_SUBGROUP_DELIVERY_TIMEOUT, PARAM_SUBSCRIBER_PRIORITY,
};
use shiguredo_moqt::track_properties::{
    PROP_OBJECT_DELIVERY_TIMEOUT, PROP_SUBGROUP_DELIVERY_TIMEOUT, TrackProperty, TrackPropertyValue,
};

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
