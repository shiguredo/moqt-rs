//! FETCH 応答 Object の Range Filter 評価 (draft-ietf-moq-transport-23 §3.3.2 / §3.3.3)
//!
//! §3.3.3 (Combining Filters): "The publisher MUST forward only objects that pass all
//! filters." FETCH 要求に含まれる Object 系 Range Filter を、応答 Object の
//! Subgroup ID / Object ID / Publisher Priority / Object Properties で評価する。

use super::*;
use shiguredo_moqt::message::common::Location;
use shiguredo_moqt::message_parameter::{
    PARAM_OBJECT_PROPERTY_FILTER, PARAM_OBJECTID_FILTER, PARAM_PRIORITY_FILTER,
    PARAM_SUBGROUP_FILTER,
};
use shiguredo_moqt::object_properties::{ObjectProperties, ObjectProperty, ObjectPropertyValue};
use shiguredo_moqt::parameter::SETUP_OPTION_MAX_FILTER_RANGES;
use shiguredo_moqt::session::types::{DataStreamId, PUBLISHER_PRIORITY_DEFAULT, SendRequestError};
use shiguredo_moqt::varint;

/// 1 個の Range Filter パラメータを組み立てる
///
/// Range Filter のワイヤーフォーマット (draft-ietf-moq-transport-23 §8.6):
/// SetID (8 bits) | [Property Type (vi64)] | Start (vi64) | End (vi64) ...
/// End を省略できるのは最後の Range だけである。
fn filter_param(
    param_type: u64,
    set_id: u8,
    property_type: Option<u64>,
    ranges: &[(u64, Option<u64>)],
) -> MessageParameter {
    let mut bytes = vec![set_id];
    if let Some(property_type) = property_type {
        varint::encode(property_type, &mut bytes);
    }
    // draft-ietf-moq-transport-23 §8.6 (Range Filter Structure): Start は直前の End からの
    // delta、End は Start からの delta で符号化する。
    let mut prev_end = 0u64;
    for (index, (start, end)) in ranges.iter().enumerate() {
        assert!(*start >= prev_end, "Start は直前の End 以上であること");
        varint::encode(start - prev_end, &mut bytes);
        match end {
            Some(end) => {
                assert!(*end >= *start, "End は Start 以上であること");
                varint::encode(end - start, &mut bytes);
                prev_end = *end;
            }
            None if index + 1 == ranges.len() => {
                prev_end = *start;
            }
            None => panic!("End を省略できるのは最後の Range だけである"),
        }
    }
    MessageParameter {
        param_type,
        value: MessageParameterValue::LengthPrefixed(bytes),
    }
}

/// Range Filter を 1 個持つ `MessageParameters` を作る
fn parameters_with(param: MessageParameter) -> MessageParameters {
    let mut parameters = MessageParameters::new();
    parameters.push(param);
    parameters
}

/// publisher 側で FETCH を確立し、応答用のデータストリームを開く
///
/// 戻り値の `publisher` が応答 Object を送る側である。応答範囲の終端は
/// LARGEST_OBJECT (group 1, object 10) で確定する。
fn establish_publisher_fetch(parameters: MessageParameters) -> (Session, Session, u64) {
    // Range Filter を送るには、送信側から見た peer (publisher) の MAX_FILTER_RANGES 宣言が必要
    // (draft-ietf-moq-transport-23 §9.1.6 (MAX FILTER RANGES))。
    let (mut publisher, mut subscriber) = establish_pair_with_options(
        opts_with(SETUP_OPTION_MAX_FILTER_RANGES, 8),
        SetupOptions::new(),
    );
    let rid = subscriber
        .send_fetch(ns(&[b"live"]), b"cam".to_vec(), parameters)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut subscriber);
    publisher
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    publisher
        .send_fetch_ok(
            rid,
            fetch_ok_params(Some(Location {
                group_id: 1,
                object_id: 10,
            })),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut publisher);
    subscriber
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    publisher
        .send_fetch_header(DataStreamId(70), rid)
        .expect("テストフィクスチャの前提条件を満たす");
    (publisher, subscriber, rid)
}

/// OBJECTID_FILTER の範囲外 Object を送信できない
#[test]
fn fetch_object_rejects_object_id_out_of_range() {
    let parameters = parameters_with(filter_param(
        PARAM_OBJECTID_FILTER,
        0,
        None,
        &[(1, Some(2))],
    ));
    let (mut publisher, _subscriber, _rid) = establish_publisher_fetch(parameters);
    let stream = DataStreamId(70);

    publisher
        .send_fetch_object_with_context(
            stream,
            &Location {
                group_id: 1,
                object_id: 1,
            },
            None,
            PUBLISHER_PRIORITY_DEFAULT,
            None,
        )
        .expect("範囲内の Object は送信できること");

    let error = publisher
        .send_fetch_object_with_context(
            stream,
            &Location {
                group_id: 1,
                object_id: 3,
            },
            None,
            PUBLISHER_PRIORITY_DEFAULT,
            None,
        )
        .expect_err("範囲外の Object は拒否されること");
    assert!(
        matches!(error, SendRequestError::LocalFilterMismatch),
        "LocalFilterMismatch で拒否されること: {error:?}"
    );

    // 文脈を渡さない従来 API も同じ判定を使う
    let error = publisher
        .send_fetch_object(
            stream,
            &Location {
                group_id: 1,
                object_id: 3,
            },
        )
        .expect_err("従来 API でも範囲外の Object は拒否されること");
    assert!(matches!(error, SendRequestError::LocalFilterMismatch));
}

/// PRIORITY_FILTER は文脈で渡した Publisher Priority で評価する
#[test]
fn fetch_object_priority_filter_uses_context_priority() {
    let parameters = parameters_with(filter_param(
        PARAM_PRIORITY_FILTER,
        0,
        None,
        &[(0, Some(10))],
    ));
    let (mut publisher, _subscriber, _rid) = establish_publisher_fetch(parameters);
    let stream = DataStreamId(70);
    let location = Location {
        group_id: 1,
        object_id: 1,
    };

    publisher
        .send_fetch_object_with_context(stream, &location, None, 5, None)
        .expect("範囲内の Priority は通ること");
    let error = publisher
        .send_fetch_object_with_context(stream, &location, None, 11, None)
        .expect_err("範囲外の Priority は拒否されること");
    assert!(matches!(error, SendRequestError::LocalFilterMismatch));
}

/// SUBGROUP_FILTER は文脈で渡した Subgroup ID で評価する
#[test]
fn fetch_object_subgroup_filter_uses_context_subgroup() {
    let parameters = parameters_with(filter_param(
        PARAM_SUBGROUP_FILTER,
        0,
        None,
        &[(0, Some(2))],
    ));
    let (mut publisher, _subscriber, _rid) = establish_publisher_fetch(parameters);
    let stream = DataStreamId(70);
    let location = Location {
        group_id: 1,
        object_id: 1,
    };

    publisher
        .send_fetch_object_with_context(
            stream,
            &location,
            Some(1),
            PUBLISHER_PRIORITY_DEFAULT,
            None,
        )
        .expect("範囲内の Subgroup は通ること");
    let error = publisher
        .send_fetch_object_with_context(
            stream,
            &location,
            Some(3),
            PUBLISHER_PRIORITY_DEFAULT,
            None,
        )
        .expect_err("範囲外の Subgroup は拒否されること");
    assert!(matches!(error, SendRequestError::LocalFilterMismatch));
    // Subgroup ID 省略時は Subgroup に属さないため評価しない (datagram と同じ扱い)
    publisher
        .send_fetch_object_with_context(stream, &location, None, PUBLISHER_PRIORITY_DEFAULT, None)
        .expect("Subgroup を省略した Object は通ること");
}

/// OBJECT_PROPERTY_FILTER は文脈で渡した Object Properties で評価する
#[test]
fn fetch_object_property_filter_uses_context_properties() {
    // Property Type 0x02 (OBJECT_DELIVERY_TIMEOUT) を 100 以上に制限する
    let parameters = parameters_with(filter_param(
        PARAM_OBJECT_PROPERTY_FILTER,
        0,
        Some(0x02),
        &[(100, None)],
    ));
    let (mut publisher, _subscriber, _rid) = establish_publisher_fetch(parameters);
    let stream = DataStreamId(70);
    let location = Location {
        group_id: 1,
        object_id: 1,
    };

    let mut properties = ObjectProperties::new();
    properties.push(ObjectProperty {
        prop_type: 0x02,
        value: ObjectPropertyValue::VarInt(150),
    });
    let mut encoded = Vec::new();
    properties
        .encode(&mut encoded)
        .expect("テストフィクスチャの前提条件を満たす");
    publisher
        .send_fetch_object_with_context(
            stream,
            &location,
            None,
            PUBLISHER_PRIORITY_DEFAULT,
            Some(&encoded),
        )
        .expect("条件を満たす Object Properties は通ること");

    let error = publisher
        .send_fetch_object_with_context(stream, &location, None, PUBLISHER_PRIORITY_DEFAULT, None)
        .expect_err("Property を持たない Object は拒否されること");
    assert!(matches!(error, SendRequestError::LocalFilterMismatch));

    let mut out_of_range = ObjectProperties::new();
    out_of_range.push(ObjectProperty {
        prop_type: 0x02,
        value: ObjectPropertyValue::VarInt(99),
    });
    let mut out_of_range_bytes = Vec::new();
    out_of_range
        .encode(&mut out_of_range_bytes)
        .expect("テストフィクスチャの前提条件を満たす");
    let error = publisher
        .send_fetch_object_with_context(
            stream,
            &location,
            None,
            PUBLISHER_PRIORITY_DEFAULT,
            Some(&out_of_range_bytes),
        )
        .expect_err("範囲外の Object Properties は拒否されること");
    assert!(matches!(error, SendRequestError::LocalFilterMismatch));
}
