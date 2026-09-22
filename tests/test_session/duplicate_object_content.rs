//! 重複 Object の内容 (payload / immutable properties) 不一致の検出テスト
//!
//! draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 6: "The same Object is
//! received more than once with different Payload or other immutable properties." と、
//! draft-ietf-moq-transport-21 §10.7 (Immutable Properties) の比較範囲を検証する。

use super::*;
use shiguredo_moqt::track_properties::PROP_IMMUTABLE_PROPERTIES;

/// IMMUTABLE_PROPERTIES (0x0B) の内側に GREASE の偶数型プロパティ 1 つを載せた
/// Object Properties のバイト列を作る
///
/// 内側の値は「入れ子の Key-Value-Pair 列」で、`ObjectProperties::encode` の出力
/// (`Properties Length | Key-Value-Pairs`) から先頭の Properties Length varint を除いたものである。
/// 副作用のない GREASE 値 (draft-ietf-moq-transport-21 §13 (Grease)) を使うことで、
/// PRIOR_GROUP_ID_GAP 等の他条件と干渉させずに「immutable properties の生バイト列」だけを
/// 変化させられる。
fn properties_with_immutable_grease(value: u64) -> Vec<u8> {
    // GREASE の Property Type は `0x7f * N + 0x9D`。N = 129 の偶数型が 0x409C
    // (draft-ietf-moq-transport-21 §16.8 (Properties) Table 14)
    const GREASE_TYPE: u64 = 0x409C;
    assert!(
        shiguredo_moqt::grease::is_grease(GREASE_TYPE),
        "テスト前提として GREASE 値であること"
    );
    let mut inner_properties = ObjectProperties::new();
    inner_properties.push(ObjectProperty {
        prop_type: GREASE_TYPE,
        value: ObjectPropertyValue::VarInt(value),
    });
    let mut inner_buf = Vec::new();
    inner_properties
        .encode(&mut inner_buf)
        .expect("正当なテスト入力の encode は成功する");
    let (_, length_varint) = shiguredo_moqt::varint::decode(&inner_buf)
        .expect("encode の出力は Properties Length varint で始まる");
    let inner = inner_buf[length_varint..].to_vec();

    encode_properties([ObjectProperty {
        prop_type: PROP_IMMUTABLE_PROPERTIES,
        value: ObjectPropertyValue::Bytes(inner),
    }])
}

/// IMMUTABLE_PROPERTIES (0x0B) と mutable な GREASE プロパティを 1 つずつ持つ
/// Object Properties のバイト列を作る
///
/// immutables の内側は `immutable_value`、mutable 側は `mutable_value` で変化させる。
/// どちらも副作用のない GREASE 値 (draft-ietf-moq-transport-21 §13 (Grease)) である。
fn properties_with_immutable_and_mutable_grease(
    immutable_value: u64,
    mutable_value: u64,
) -> Vec<u8> {
    // GREASE の Property Type は `0x7f * N + 0x9D`。偶数型は 0x409C (N = 129)、
    // 奇数型は 0x401D (N = 128)。mutable 側は奇数型 (Bytes) にして immutables と型を分ける
    const MUTABLE_GREASE_TYPE: u64 = 0x401D;
    assert!(
        shiguredo_moqt::grease::is_grease(MUTABLE_GREASE_TYPE),
        "テスト前提として GREASE 値であること"
    );
    let mut inner_properties = ObjectProperties::new();
    inner_properties.push(ObjectProperty {
        prop_type: 0x409C,
        value: ObjectPropertyValue::VarInt(immutable_value),
    });
    let mut inner_buf = Vec::new();
    inner_properties
        .encode(&mut inner_buf)
        .expect("正当なテスト入力の encode は成功する");
    let (_, length_varint) = shiguredo_moqt::varint::decode(&inner_buf)
        .expect("encode の出力は Properties Length varint で始まる");

    encode_properties([
        ObjectProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            value: ObjectPropertyValue::Bytes(inner_buf[length_varint..].to_vec()),
        },
        ObjectProperty {
            prop_type: MUTABLE_GREASE_TYPE,
            value: ObjectPropertyValue::Bytes(vec![mutable_value as u8]),
        },
    ])
}

/// 同一 (Group ID, Object ID) の重複 Object で mutable な Object Property だけが異なっても
/// Malformed にならない (subgroup 経路)
///
/// draft-ietf-moq-transport-21 §7.1 (Caching Relays): "Object Properties can be added, removed or
/// updated, subject to the constraints of the specific property." 比較対象は §12.1 (Malformed
/// Tracks) 条件 6 の "different Payload or other immutable properties" に限られるため、mutable な
/// Object Property の更新は Malformed ではない。
#[test]
fn duplicate_object_with_different_mutable_properties_is_not_malformed_on_subgroup() {
    let alias = 847u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(74);
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(10),
        has_properties: true,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_id, 0x15)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("テストフィクスチャの前提条件を満たす");

    // IMMUTABLE_PROPERTIES は同一で、mutable な GREASE プロパティの値だけが異なる 2 通
    for value in [1u64, 2] {
        let outcome = client
            .recv_subgroup_object(
                stream_id,
                &DecodedSubgroupObject {
                    object_id: 0,
                    payload_length: 4,
                    status: None,
                    properties_bytes: Some(properties_with_immutable_and_mutable_grease(1, value)),
                },
            )
            .unwrap_or_else(|e| {
                panic!("mutable な Object Property の差異は Malformed にならないこと: {e:?}")
            });
        assert_eq!(
            outcome,
            TrackDataAcceptance::Accepted,
            "mutable な Object Property の差異は受理されること (value {value})"
        );
    }
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Established,
        "subscription は Established のまま"
    );
}

/// 同一 (Group ID, Object ID) の重複 datagram で mutable な Object Property だけが異なっても
/// Malformed にならない
#[test]
fn duplicate_object_with_different_mutable_properties_is_not_malformed_on_datagram() {
    let alias = 848u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    for value in [1u64, 2] {
        let outcome = client
            .recv_object_datagram(&ObjectDatagram {
                track_alias: alias,
                group_id: 0,
                object_id: 0,
                publisher_priority: Some(50),
                properties_data: Some(properties_with_immutable_and_mutable_grease(1, value)),
                end_of_group: false,
                status: None,
            })
            .unwrap_or_else(|e| {
                panic!("mutable な Object Property の差異は Malformed にならないこと: {e:?}")
            });
        assert_eq!(
            outcome,
            TrackDataAcceptance::Accepted,
            "mutable な Object Property の差異は受理されること (value {value})"
        );
    }
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Established,
        "subscription は Established のまま"
    );
}

/// 同一 (Group ID, Object ID) の重複 Object で payload 長が異なると Malformed Track (条件 6)
///
/// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 6 の "different Payload" の検出。
/// Session は payload のバイト列を保持しないため、payload 長の差異で検出する。
#[test]
fn condition6_subgroup_duplicate_with_different_payload_length_terminates_subscription() {
    let alias = 836u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(70);
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(10),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 4,
                status: None,
                properties_bytes: None,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // 同じ Object を payload 長だけ変えて再受信する
    let err = client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 5,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("条件 6: payload 長の不一致は Malformed Track");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        err.reason, "malformed track: duplicate Object with different Payload",
        "payload の不一致理由を返すこと"
    );
    assert_eq!(
        client.state(),
        SessionState::Established,
        "セッションは閉じないこと"
    );
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated,
        "該当 subscription は Terminated になること"
    );

    // draft §12.1 MUST: bidi request stream と malformed を運んだ data stream を cancel する
    let (cancels, reset_data_streams, malformed_terminations) =
        drain_malformed_events(&mut client, rid, Some(stream_id));
    assert_eq!(
        cancels,
        vec!["stop_sending", "reset"],
        "受信方向 → 送信方向の順で STREAM_MALFORMED_TRACK の cancel を発行すること"
    );
    assert_eq!(
        reset_data_streams, 1,
        "malformed を運んだ stream を reset すること"
    );
    assert_eq!(
        malformed_terminations, 1,
        "RequestTerminated(MalformedTrack) を 1 件発行すること"
    );
}

/// 同一 (Group ID, Object ID) の重複 Object で IMMUTABLE_PROPERTIES が異なると Malformed Track (条件 6)
///
/// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 6:
/// "The same Object is received more than once with different Payload or other immutable
/// properties." の "other immutable properties" の差異を検出する。draft §10.7 (Immutable
/// Properties) は "This Property MUST NOT be modified or removed and the serialization (e.g.
/// variable-length integer encodings) of the Key-Value-Pairs MUST NOT change." と定める。
#[test]
fn condition6_subgroup_duplicate_with_different_immutable_properties_terminates_subscription() {
    let alias = 837u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(71);
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(10),
        has_properties: true,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_id, 0x15)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 4,
                status: None,
                properties_bytes: Some(properties_with_immutable_grease(1)),
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // 同じ Object を IMMUTABLE_PROPERTIES の値だけ変えて再受信する
    let err = client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 4,
                status: None,
                properties_bytes: Some(properties_with_immutable_grease(2)),
            },
        )
        .expect_err("条件 6: immutable properties の不一致は Malformed Track");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        err.reason, "malformed track: duplicate Object with different immutable properties",
        "immutable properties の不一致理由を返すこと"
    );
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated,
        "該当 subscription は Terminated になること"
    );

    // draft §12.1 MUST: bidi request stream と malformed を運んだ data stream を cancel する
    let (cancels, reset_data_streams, malformed_terminations) =
        drain_malformed_events(&mut client, rid, Some(stream_id));
    assert_eq!(
        cancels,
        vec!["stop_sending", "reset"],
        "受信方向 → 送信方向の順で STREAM_MALFORMED_TRACK の cancel を発行すること"
    );
    assert_eq!(
        reset_data_streams, 1,
        "malformed を運んだ stream を reset すること"
    );
    assert_eq!(
        malformed_terminations, 1,
        "RequestTerminated(MalformedTrack) を 1 件発行すること"
    );
}

/// 同一 (Group ID, Object ID) の重複 datagram で status が異なると Malformed Track (条件 6)
///
/// draft-ietf-moq-transport-21 §11.1.2 (Object Status): status 付き Object は payload を持たない
/// ("0x0 := Normal object. This status is implicit for any non-zero length object.") ため、
/// status の差異は §12.1 条件 6 の Payload の差異にあたる。
///
/// datagram 経路は payload 長を比較キーに含めないので、`Some(0x0)` (Normal) との比較は
/// **status の presence バイトだけ**で差が出る。`Some(0x0)` を「未提供」と同じ符号化にすると
/// この差異を見逃す。
#[test]
fn condition6_datagram_duplicate_with_different_payload_status_terminates_subscription() {
    for (alias, second_status) in [(838u64, 0x3u64), (839, 0x0)] {
        let (mut client, _server, rid) = establish_subscribe_track(alias);
        client
            .recv_object_datagram(&ObjectDatagram {
                track_alias: alias,
                group_id: 0,
                object_id: 0,
                publisher_priority: Some(50),
                properties_data: None,
                end_of_group: false,
                status: None,
            })
            .expect("テストフィクスチャの前提条件を満たす");

        // 同じ Object を status 付き (payload 無し) で再受信する
        let err = match client.recv_object_datagram(&ObjectDatagram {
            track_alias: alias,
            group_id: 0,
            object_id: 0,
            publisher_priority: Some(50),
            properties_data: None,
            end_of_group: false,
            status: Some(second_status),
        }) {
            Ok(acceptance) => panic!(
                "条件 6: status {second_status:#x} の差異が Malformed Track にならなかった: \
                 {acceptance:?}"
            ),
            Err(err) => err,
        };
        assert_eq!(
            err.code, SESSION_PROTOCOL_VIOLATION,
            "status {second_status:#x}: session error code が一致すること"
        );
        assert_eq!(
            err.reason, "malformed track: duplicate Object with different Payload",
            "payload の不一致理由を返すこと (status {second_status:#x})"
        );
        assert_eq!(
            client.state(),
            SessionState::Established,
            "status {second_status:#x}: セッションは閉じないこと"
        );
        assert_eq!(
            client
                .subscription(rid)
                .expect("subscription が存在する")
                .state,
            SubscriptionState::Terminated,
            "該当 subscription は Terminated になること (status {second_status:#x})"
        );

        // draft §12.1 MUST: bidi request stream を STREAM_MALFORMED_TRACK で cancel する
        let (cancels, reset_data_streams, malformed_terminations) =
            drain_malformed_events(&mut client, rid, None);
        assert_eq!(
            cancels,
            vec!["stop_sending", "reset"],
            "status {second_status:#x}: 受信方向 → 送信方向の順で cancel を発行すること"
        );
        assert_eq!(
            reset_data_streams, 0,
            "status {second_status:#x}: datagram 経路では ResetDataStream を発行しないこと"
        );
        assert_eq!(
            malformed_terminations, 1,
            "status {second_status:#x}: RequestTerminated(MalformedTrack) を 1 件発行すること"
        );
    }
}

/// 同一 (Group ID, Object ID) の重複 datagram で IMMUTABLE_PROPERTIES が異なると
/// Malformed Track (条件 6)
///
/// subgroup 経路と同じく、datagram 経路でも `properties_data` から
/// IMMUTABLE_PROPERTIES (0x0B) の生バイト列を取り出して比較する。
#[test]
fn condition6_datagram_duplicate_with_different_immutable_properties_terminates_subscription() {
    let alias = 845u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: alias,
            group_id: 0,
            object_id: 0,
            publisher_priority: Some(50),
            properties_data: Some(properties_with_immutable_grease(1)),
            end_of_group: false,
            status: None,
        })
        .expect("テストフィクスチャの前提条件を満たす");

    // 同じ Object を IMMUTABLE_PROPERTIES の値だけ変えて再受信する
    let err = client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: alias,
            group_id: 0,
            object_id: 0,
            publisher_priority: Some(50),
            properties_data: Some(properties_with_immutable_grease(2)),
            end_of_group: false,
            status: None,
        })
        .expect_err("条件 6: immutable properties の不一致は Malformed Track");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        err.reason, "malformed track: duplicate Object with different immutable properties",
        "immutable properties の不一致理由を返すこと"
    );
    assert_eq!(
        client.state(),
        SessionState::Established,
        "セッションは閉じないこと"
    );
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated,
        "該当 subscription は Terminated になること"
    );
    // draft §12.1 MUST: bidi request stream を STREAM_MALFORMED_TRACK で cancel する
    let (cancels, reset_data_streams, malformed_terminations) =
        drain_malformed_events(&mut client, rid, None);
    assert_eq!(
        cancels,
        vec!["stop_sending", "reset"],
        "受信方向 → 送信方向の順で cancel を発行すること"
    );
    assert_eq!(
        reset_data_streams, 0,
        "datagram 経路では ResetDataStream を発行しないこと"
    );
    assert_eq!(
        malformed_terminations, 1,
        "RequestTerminated(MalformedTrack) を 1 件発行すること"
    );
}

/// 同一内容の重複 Object は Malformed にならない (正当な再送)
///
/// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 6 は Payload や immutable
/// properties が「異なる」場合だけを Malformed とする。relay の再送などで同一内容が
/// 再受信されることは正当である。
#[test]
fn condition6_duplicate_object_with_same_content_is_not_malformed() {
    let alias = 840u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(72);
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(10),
        has_properties: true,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_id, 0x15)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_subgroup_header(stream_id, &header)
        .expect("テストフィクスチャの前提条件を満たす");

    // 同一内容 (payload 長・properties とも同じ) を 2 回受信する
    for _ in 0..2 {
        let outcome = client
            .recv_subgroup_object(
                stream_id,
                &DecodedSubgroupObject {
                    object_id: 0,
                    payload_length: 4,
                    status: None,
                    properties_bytes: Some(properties_with_immutable_grease(1)),
                },
            )
            .expect("同一内容の重複は受理されること");
        assert_eq!(outcome, TrackDataAcceptance::Accepted);
    }
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Established,
        "subscription は Established のまま"
    );
}
