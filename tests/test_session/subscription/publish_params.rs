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

/// publisher 役購読を確立し、そのうえで Object を publish した状態にする
///
/// `send_subgroup_object` は publisher 側の `largest_received_location` を更新するため、
/// これ以降の `publisher_track_largest` は (5, 9) を返す。
fn publish_objects_on_publisher_subscription(server: &mut Session, client: &mut Session) {
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");
    let stream_id = DataStreamId(10);
    server
        .send_subgroup_header(
            stream_id,
            rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: 5,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("subgroup header の送信に成功すること");
    server
        .send_subgroup_object(stream_id, 9, None)
        .expect("object の送信に成功すること");
}

/// 既に Object を publish した Track を PUBLISH で再告知する場合、LARGEST_OBJECT を補完する
///
/// draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter) は LARGEST_OBJECT を
/// PUBLISH に出現可能なパラメータとして列挙し、"If Objects have been published on this
/// Track the Publisher MUST include this parameter." と規定する。
#[test]
fn send_publish_includes_largest_object_of_published_objects() {
    let (mut client, mut server) = establish_pair();
    publish_objects_on_publisher_subscription(&mut server, &mut client);
    let rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            222,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("PUBLISH の送信に成功すること");
    let (sent_rid, msg) = take_send_request(&mut server);
    assert_eq!(sent_rid, rid);
    let ControlMessage::Publish(publish) = msg else {
        panic!("PUBLISH が送信されること");
    };
    assert_eq!(
        publish.parameters.largest_object(),
        Some((5, 9)),
        "publish 済み Object の最大 Location が補完されること"
    );
}

/// Object の観測値が無い Track への PUBLISH には LARGEST_OBJECT を付与しない
///
/// draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter) の MUST は
/// "If Objects have been published on this Track" の場合に限られる。
#[test]
fn send_publish_omits_largest_object_without_published_objects() {
    let (_client, mut server) = establish_pair();
    let rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            111,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("PUBLISH の送信に成功すること");
    let (sent_rid, msg) = take_send_request(&mut server);
    assert_eq!(sent_rid, rid);
    let ControlMessage::Publish(publish) = msg else {
        panic!("PUBLISH が送信されること");
    };
    assert_eq!(
        publish.parameters.largest_object(),
        None,
        "観測値が無ければ LARGEST_OBJECT を付与しないこと"
    );
}

/// 観測値より大きいアプリ指定の LARGEST_OBJECT は上書きされない
///
/// `update_largest_object_in_parameters` はアプリ指定値との max を取るため、
/// 指定値が観測値以上であればそのまま残る。
#[test]
fn send_publish_keeps_larger_explicit_largest_object() {
    let (mut client, mut server) = establish_pair();
    publish_objects_on_publisher_subscription(&mut server, &mut client);
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 7,
            object: 3,
        },
    });
    let rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            222,
            params,
            TrackProperties::new(),
        )
        .expect("PUBLISH の送信に成功すること");
    let (sent_rid, msg) = take_send_request(&mut server);
    assert_eq!(sent_rid, rid);
    let ControlMessage::Publish(publish) = msg else {
        panic!("PUBLISH が送信されること");
    };
    assert_eq!(
        publish.parameters.largest_object(),
        Some((7, 3)),
        "観測値 (5, 9) より大きいアプリ指定値が上書きされないこと"
    );
}

/// 観測値より小さいアプリ指定の LARGEST_OBJECT は観測値まで引き上げられる
///
/// draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter) は LARGEST_OBJECT が
/// 実際に publish 済みの最大 Location 以上であることを求めるため、指定値が小さい場合は
/// `update_largest_object_in_parameters` の max 合流で観測値に揃える。
#[test]
fn send_publish_raises_smaller_explicit_largest_object() {
    let (mut client, mut server) = establish_pair();
    publish_objects_on_publisher_subscription(&mut server, &mut client);
    // 観測値 (5, 9) より小さい (3, 0) を明示する
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 3,
            object: 0,
        },
    });
    let rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            222,
            params,
            TrackProperties::new(),
        )
        .expect("PUBLISH の送信に成功すること");
    let (sent_rid, msg) = take_send_request(&mut server);
    assert_eq!(sent_rid, rid);
    let ControlMessage::Publish(publish) = msg else {
        panic!("PUBLISH が送信されること");
    };
    assert_eq!(
        publish.parameters.largest_object(),
        Some((5, 9)),
        "指定値が観測値より小さければ観測値に引き上げられること"
    );
}
