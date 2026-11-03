//! Property のスコープ違反 (Track / Object) を Malformed Track として扱う検証
//!
//! draft-ietf-moq-transport-23 §8.4 (Track and Object Properties): "An endpoint that supports
//! a Property and receives it outside its scope (e.g. a Track-only Property as an Object
//! Property) MUST treat the Track as malformed" / §12.1 (Malformed Tracks) 条件 8。
//!
//! Track Properties のスコープ検証は構造 decode ではなく、Request ID と Full Track Name を
//! 持つ受信ハンドラが行う。違反はセッション終了ではなく、同じ Track の subscription / FETCH の
//! cancel (§6.4.2.3) とアプリへの `RequestTerminated { reason: MalformedTrack }` 通知
//! (§12.1 の SHOULD) へ渡す。Object 側は `ObjectProperties::decode_exact` が
//! `MalformedTrack` を保ったまま data plane の同じ取消経路へ渡す。

use super::*;

use shiguredo_moqt::message::common::TrackNamespace;
use shiguredo_moqt::object_properties::PROP_PRIOR_GROUP_ID_GAP;
use shiguredo_moqt::track_properties::{
    PROP_IMMUTABLE_PROPERTIES, TrackProperty, TrackPropertyValue,
};

// ─── フィクスチャ ─────────────────────────────────────────

/// Object 専用の既知 Property (PRIOR_GROUP_ID_GAP) 1 つを持つ Track Properties
fn track_properties_with_prior_group_id_gap() -> TrackProperties {
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: PROP_PRIOR_GROUP_ID_GAP,
        value: TrackPropertyValue::VarInt(1),
    });
    tp
}

/// Object 専用の既知 Property を IMMUTABLE_PROPERTIES の内側だけに持つ Track Properties
///
/// mutable リストと IMMUTABLE_PROPERTIES の内側は同じスコープ規則に従う
/// (draft-ietf-moq-transport-23 §10.7 (Immutable Properties) の "Properties MAY appear either
/// in the mutable property list or inside Immutable Properties")。
fn track_properties_with_prior_group_id_gap_in_immutable() -> TrackProperties {
    // 内側の KVP 列を手組みする。Object 専用 Property を含む Track Properties は
    // encode がローカル入力の検証として拒否するため、encode では作れない。
    let mut inner = Vec::new();
    shiguredo_moqt::varint::encode(PROP_PRIOR_GROUP_ID_GAP, &mut inner);
    shiguredo_moqt::varint::encode(1, &mut inner);
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: PROP_IMMUTABLE_PROPERTIES,
        value: TrackPropertyValue::Bytes(inner),
    });
    tp
}

/// PUBLISH の Track Properties ブロックに載せる、PRIOR_GROUP_ID_GAP の KVP 生バイト列
///
/// Object 専用 Property を含む Track Properties は `TrackProperties::encode` が拒否するため、
/// wire 入力のテストでは KVP を手組みする (delta encoding の先頭なので型そのものが delta)。
fn prior_group_id_gap_kvp_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    shiguredo_moqt::varint::encode(PROP_PRIOR_GROUP_ID_GAP, &mut bytes);
    shiguredo_moqt::varint::encode(1, &mut bytes);
    bytes
}

/// Track 専用の既知 Property (MAX_CACHE_DURATION) だけを持つ Object Properties の生バイト列
///
/// Properties Length と実データ長を一致させた完全なブロックである。
/// スコープ違反を含む Object Properties は `ObjectProperties::encode` が拒否するため、
/// wire 入力のテストでは KVP を手組みする。
fn object_properties_with_max_cache_duration() -> Vec<u8> {
    let mut kvps = Vec::new();
    shiguredo_moqt::varint::encode(0x04, &mut kvps); // delta = MAX_CACHE_DURATION (偶数型)
    shiguredo_moqt::varint::encode(1_000, &mut kvps);
    let mut block = Vec::new();
    shiguredo_moqt::varint::encode(kvps.len() as u64, &mut block);
    block.extend_from_slice(&kvps);
    block
}

/// IMMUTABLE_PROPERTIES の内側だけに Track 専用 Property を持つ Object Properties の生バイト列
fn object_properties_with_max_cache_duration_in_immutable() -> Vec<u8> {
    let mut inner = Vec::new();
    shiguredo_moqt::varint::encode(0x04, &mut inner);
    shiguredo_moqt::varint::encode(1_000, &mut inner);
    let mut kvps = Vec::new();
    shiguredo_moqt::varint::encode(PROP_IMMUTABLE_PROPERTIES, &mut kvps);
    shiguredo_moqt::varint::encode(inner.len() as u64, &mut kvps);
    kvps.extend_from_slice(&inner);
    let mut block = Vec::new();
    shiguredo_moqt::varint::encode(kvps.len() as u64, &mut block);
    block.extend_from_slice(&kvps);
    block
}

/// Track Namespace を wire 形式で書き出す
fn encode_namespace(namespace: &TrackNamespace, buf: &mut Vec<u8>) {
    let fields = namespace.fields();
    shiguredo_moqt::varint::encode(fields.len() as u64, buf);
    for field in fields {
        shiguredo_moqt::varint::encode(field.len() as u64, buf);
        buf.extend_from_slice(field);
    }
}

/// 制御メッセージのヘッダ (Type + Length) と本文を組み立てる
fn frame(type_id: u64, payload: &[u8]) -> Vec<u8> {
    let mut wire = Vec::new();
    shiguredo_moqt::varint::encode(type_id, &mut wire);
    shiguredo_moqt::varint::encode(payload.len() as u64, &mut wire);
    wire.extend_from_slice(payload);
    wire
}

/// Track Properties の生バイト列を持つ PUBLISH の wire 表現 (Type 0x1D)
fn publish_wire(request_id: u64, track_properties: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    shiguredo_moqt::varint::encode(request_id, &mut payload);
    encode_namespace(&ns(&[b"live"]), &mut payload);
    shiguredo_moqt::varint::encode(3, &mut payload);
    payload.extend_from_slice(b"cam");
    shiguredo_moqt::varint::encode(700, &mut payload); // Track Alias
    shiguredo_moqt::varint::encode(0, &mut payload); // Number of Parameters
    payload.extend_from_slice(track_properties);
    frame(0x1D, &payload)
}

/// Track Properties の生バイト列を持つ SUBSCRIBE_OK の wire 表現 (Type 0x04)
fn subscribe_ok_wire(track_alias: u64, track_properties: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    shiguredo_moqt::varint::encode(track_alias, &mut payload);
    shiguredo_moqt::varint::encode(0, &mut payload); // Number of Parameters
    payload.extend_from_slice(track_properties);
    frame(0x04, &payload)
}

/// Track Properties の生バイト列を持つ FETCH_OK の wire 表現 (Type 0x18)
fn fetch_ok_wire(track_properties: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.push(0); // End of Track
    shiguredo_moqt::varint::encode(0, &mut payload); // End Location: Group ID
    shiguredo_moqt::varint::encode(0, &mut payload); // End Location: Object ID
    shiguredo_moqt::varint::encode(0, &mut payload); // Number of Parameters
    payload.extend_from_slice(track_properties);
    frame(0x18, &payload)
}

/// Track Properties の生バイト列を持つ REQUEST_OK (TRACK_STATUS_OK) の wire 表現 (Type 0x07)
fn track_status_ok_wire(track_properties: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    shiguredo_moqt::varint::encode(0, &mut payload); // Number of Parameters
    payload.extend_from_slice(track_properties);
    frame(0x07, &payload)
}

/// Malformed Track の cancel と終端イベントを request ごとに検証する
///
/// `expected` は `(request_id, RequestKind)` の列である。cancel (STOP_SENDING →
/// RESET_STREAM、どちらも `STREAM_MALFORMED_TRACK`) と `RequestTerminated { MalformedTrack }`
/// が request ごとに 1 回ずつ発行され、対象外の request には発行されないことを確かめる。
fn assert_malformed_terminations(client: &mut Session, expected: &[(u64, RequestKind)]) {
    let (mut per_request, reset_streams) = drain_malformed_events_by_request(client);
    assert!(
        reset_streams.is_empty(),
        "制御メッセージ由来の malformed で data stream を reset しないこと"
    );
    assert_eq!(
        per_request.len(),
        expected.len(),
        "malformed 終端の対象 request 数が一致すること"
    );
    for (request_id, kind) in expected {
        let (cancels, kinds) = per_request
            .remove(request_id)
            .unwrap_or_else(|| panic!("request {request_id} に malformed イベントが無い"));
        assert_eq!(
            cancels,
            vec!["stop_sending", "reset"],
            "request {request_id} の cancel が STOP_SENDING → RESET_STREAM の順で発行されること"
        );
        assert_eq!(
            kinds,
            vec![*kind],
            "request {request_id} の RequestTerminated が 1 回だけ発行されること"
        );
    }
}

// ─── PUBLISH (初回) ───────────────────────────────────────

/// Object 専用の既知 Property を持つ初回 PUBLISH は購読として登録せず取消へ進む
///
/// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1 (Malformed Tracks)
/// 条件 8。受信側は購読を登録する前でも、受信メッセージの Request ID と Full Track Name から
/// Track 文脈を解決して cancel し、アプリへは正常な受信として通知しない。
#[test]
fn publish_with_object_only_property_is_not_registered_and_is_cancelled() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            700,
            MessageParameters::new(),
            track_properties_with_prior_group_id_gap(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, publish) = take_send_request(&mut client);
    server
        .recv_request(publish)
        .expect("Malformed Track の検出はセッションを閉じないこと");
    assert!(
        server.subscription(rid).is_none(),
        "Malformed Track の PUBLISH は購読として登録しないこと"
    );
    assert_eq!(
        server.state(),
        SessionState::Established,
        "Malformed Track の検出でセッションを閉じないこと"
    );
    assert_malformed_terminations(&mut server, &[(rid, RequestKind::Publish)]);
    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("拒否済み request の stream close は no-op で吸収すること");
    assert_eq!(server.state(), SessionState::Established);
}

/// IMMUTABLE_PROPERTIES の内側だけに Object 専用 Property を持つ PUBLISH も同じ経路で取消へ進む
#[test]
fn publish_with_object_only_property_in_immutable_is_cancelled() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            701,
            MessageParameters::new(),
            track_properties_with_prior_group_id_gap_in_immutable(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, publish) = take_send_request(&mut client);
    server
        .recv_request(publish)
        .expect("Malformed Track の検出はセッションを閉じないこと");
    assert!(server.subscription(rid).is_none());
    assert_eq!(server.state(), SessionState::Established);
    assert_malformed_terminations(&mut server, &[(rid, RequestKind::Publish)]);
}

/// wire 入力の PUBLISH でも Track 文脈を失わず取消へ進む
///
/// `TrackProperties::decode` / `ControlMessage::decode` はスコープ違反だけを理由に失敗しない。
/// 違反の判定は Request ID と Full Track Name を得た受信ハンドラの責務である。
#[test]
fn publish_with_object_only_property_on_wire_is_cancelled() {
    let (_client, mut server) = establish_pair();
    let wire = publish_wire(0, &prior_group_id_gap_kvp_bytes());
    let (message, consumed) =
        ControlMessage::decode(&wire).expect("スコープ違反は構造 decode を失敗させないこと");
    assert_eq!(consumed, wire.len(), "メッセージ全体を消費すること");
    let ControlMessage::Publish(publish) = &message else {
        panic!("Publish が期待されたが {message:?} だった");
    };
    let rid = publish.request_id;
    server
        .recv_request(message)
        .expect("Malformed Track の検出はセッションを閉じないこと");
    assert!(
        server.subscription(rid).is_none(),
        "wire 経由の Malformed Track も購読として登録しないこと"
    );
    assert_eq!(server.state(), SessionState::Established);
    assert_malformed_terminations(&mut server, &[(rid, RequestKind::Publish)]);
}

/// 初回 PUBLISH のスコープ違反で、同じ Track の既存 subscription も取消対象になる
///
/// draft §12.1 (Malformed Tracks): "it MUST cancel any corresponding subscription or fetches
/// for that Track from that publisher"。新規 PUBLISH の subscription は未登録なので、
/// 受信メッセージの Full Track Name から既存要求を解決する。
#[test]
fn publish_with_object_only_property_cancels_existing_subscription_for_same_track() {
    let (mut client, mut server) = establish_pair();
    // 同じ Track の subscription を確立しておく (client が subscriber)
    let subscribed = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, subscribe) = take_send_request(&mut client);
    server
        .recv_request(subscribe)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            subscribed,
            702,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(subscribed, ok)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(subscribed)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established
    );

    // 同じ Track の PUBLISH が Object 専用 Property を含んで届く
    let published = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            703,
            MessageParameters::new(),
            track_properties_with_prior_group_id_gap(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, publish) = take_send_request(&mut server);
    client
        .recv_request(publish)
        .expect("Malformed Track の検出はセッションを閉じないこと");

    assert!(
        client.subscription(published).is_none(),
        "Malformed Track の PUBLISH は購読として登録しないこと"
    );
    assert_eq!(
        client
            .subscription(subscribed)
            .expect("既存 subscription が残っていること")
            .state,
        SubscriptionState::Terminated,
        "同じ Track の既存 subscription を取消対象にすること"
    );
    assert_eq!(client.state(), SessionState::Established);
    assert_malformed_terminations(
        &mut client,
        &[
            (subscribed, RequestKind::Subscribe),
            (published, RequestKind::Publish),
        ],
    );
}

/// 未知 Property と両スコープの既知 Property (IMMUTABLE 内側を含む) は PUBLISH で受理される
///
/// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) は未知 Property の転送を
/// MUST とし、§16.8 (Properties) は "Endpoints MUST ignore unknown Property types" と定める。
/// スコープを推測して拒否しないことを固定する。
#[test]
fn publish_with_unknown_and_both_scope_properties_is_accepted() {
    let (mut client, mut server) = establish_pair();
    // 内側にだけ両スコープ (0x06, TIMESCALE 0x08) と未知 (0x1000) を置く。0x02 は mutable
    // リスト側に置くため、内側には置かない (単一に限られる既知 Property を両方に置くと
    // draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1 (Malformed
    // Tracks) 条件 9 の多重違反になる)。
    let mut inner = TrackProperties::new();
    inner.push(TrackProperty {
        prop_type: 0x06,
        value: TrackPropertyValue::VarInt(100),
    });
    inner.push(TrackProperty {
        prop_type: 0x08,
        value: TrackPropertyValue::VarInt(1_000),
    });
    inner.push(TrackProperty {
        prop_type: 0x1000,
        value: TrackPropertyValue::VarInt(1),
    });
    let mut inner_bytes = Vec::new();
    inner
        .encode(&mut inner_bytes)
        .expect("内側の KVP 列は encode できる");
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: 0x02,
        value: TrackPropertyValue::VarInt(200),
    });
    tp.push(TrackProperty {
        prop_type: 0x1001,
        value: TrackPropertyValue::Bytes(vec![0xCD]),
    });
    tp.push(TrackProperty {
        prop_type: PROP_IMMUTABLE_PROPERTIES,
        value: TrackPropertyValue::Bytes(inner_bytes),
    });
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            704,
            MessageParameters::new(),
            tp,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, publish) = take_send_request(&mut client);
    // wire を経由させ、decode が未知 Property を保持したまま Session へ渡すことを確かめる
    let encoded = publish
        .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
        .expect("スコープ違反の無い PUBLISH は encode できる");
    let (decoded, consumed) = ControlMessage::decode(&encoded).expect("decode は成功すること");
    assert_eq!(consumed, encoded.len());
    server
        .recv_request(decoded)
        .expect("未知 Property と両スコープ Property は拒否しないこと");
    assert_eq!(
        server
            .subscription(rid)
            .expect("購読として登録されること")
            .state,
        SubscriptionState::Pending,
        "正常な PUBLISH は Pending(Publisher) として登録すること"
    );
    assert_eq!(server.state(), SessionState::Established);
    let (per_request, _) = drain_malformed_events_by_request(&mut server);
    assert!(
        per_request.is_empty(),
        "malformed イベントを発行しないこと: {:?}",
        per_request.keys().collect::<Vec<_>>()
    );
}

// ─── SUBSCRIBE_OK ────────────────────────────────────────

/// Object 専用の既知 Property を持つ SUBSCRIBE_OK は購読を cancel する
#[test]
fn subscribe_ok_with_object_only_property_cancels_subscription() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, subscribe) = take_send_request(&mut client);
    server
        .recv_request(subscribe)
        .expect("テストフィクスチャの前提条件を満たす");
    // 送信側の `send_subscribe_ok` はローカル入力の検証 (encode) でスコープ違反を拒否する。
    // ここでは受信ハンドラの検証を確かめるため、受信メッセージを直接組み立てる。
    client
        .recv_stream_message(
            rid,
            ControlMessage::SubscribeOk(shiguredo_moqt::message::SubscribeOk {
                track_alias: 705,
                parameters: MessageParameters::new(),
                track_properties: track_properties_with_prior_group_id_gap(),
            }),
        )
        .expect("Malformed Track の検出はセッションを閉じないこと");
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が登録されていること")
            .state,
        SubscriptionState::Terminated,
        "スコープ違反の SUBSCRIBE_OK で購読をキャンセルすること"
    );
    assert_eq!(client.state(), SessionState::Established);
    assert_malformed_terminations(&mut client, &[(rid, RequestKind::Subscribe)]);
}

/// wire 入力の SUBSCRIBE_OK でも Track 文脈を失わず取消へ進む
#[test]
fn subscribe_ok_with_object_only_property_on_wire_cancels_subscription() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, subscribe) = take_send_request(&mut client);
    server
        .recv_request(subscribe)
        .expect("テストフィクスチャの前提条件を満たす");
    let wire = subscribe_ok_wire(706, &prior_group_id_gap_kvp_bytes());
    let (message, consumed) =
        ControlMessage::decode(&wire).expect("スコープ違反は構造 decode を失敗させないこと");
    assert_eq!(consumed, wire.len());
    client
        .recv_stream_message(rid, message)
        .expect("Malformed Track の検出はセッションを閉じないこと");
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が登録されていること")
            .state,
        SubscriptionState::Terminated
    );
    assert_eq!(client.state(), SessionState::Established);
    assert_malformed_terminations(&mut client, &[(rid, RequestKind::Subscribe)]);
}

// ─── FETCH_OK ────────────────────────────────────────────

/// Object 専用の既知 Property を持つ FETCH_OK は fetch を cancel する
#[test]
fn fetch_ok_with_object_only_property_cancels_fetch() {
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
                    group_id: 5,
                    object_id: 0,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch) = take_send_request(&mut client);
    server
        .recv_request(fetch)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_stream_message(
            rid,
            ControlMessage::FetchOk(shiguredo_moqt::message::FetchOk {
                end_of_track: 0,
                end_location: Location {
                    group_id: 5,
                    object_id: 0,
                },
                parameters: MessageParameters::new(),
                track_properties: track_properties_with_prior_group_id_gap(),
            }),
        )
        .expect("Malformed Track の検出はセッションを閉じないこと");
    let fetch = client.fetch(rid).expect("fetch が登録されていること");
    assert_eq!(fetch.state, FetchState::Terminated);
    assert!(
        fetch.response_received,
        "FETCH_OK を受領した fetch は応答受領フラグが立つこと"
    );
    assert_eq!(client.state(), SessionState::Established);
    assert_malformed_terminations(&mut client, &[(rid, RequestKind::Fetch)]);
}

/// wire 入力の FETCH_OK でも Track 文脈を失わず取消へ進む
#[test]
fn fetch_ok_with_object_only_property_on_wire_cancels_fetch() {
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
                    group_id: 5,
                    object_id: 0,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch) = take_send_request(&mut client);
    server
        .recv_request(fetch)
        .expect("テストフィクスチャの前提条件を満たす");
    let wire = fetch_ok_wire(&prior_group_id_gap_kvp_bytes());
    let (message, consumed) =
        ControlMessage::decode(&wire).expect("スコープ違反は構造 decode を失敗させないこと");
    assert_eq!(consumed, wire.len());
    client
        .recv_stream_message(rid, message)
        .expect("Malformed Track の検出はセッションを閉じないこと");
    assert_eq!(
        client.fetch(rid).expect("fetch が登録されていること").state,
        FetchState::Terminated
    );
    assert_eq!(client.state(), SessionState::Established);
    assert_malformed_terminations(&mut client, &[(rid, RequestKind::Fetch)]);
}

// ─── TRACK_STATUS_OK ─────────────────────────────────────

/// Object 専用の既知 Property を持つ TRACK_STATUS_OK は TRACK_STATUS と関連要求を cancel する
///
/// draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties) は
/// TRACK_STATUS_OK も未対応 Property の検査対象として列挙する。スコープ違反は §12.1
/// (Malformed Tracks) の Malformed Track であり、同じ Track の subscription も取消対象になる。
#[test]
fn track_status_ok_with_object_only_property_cancels_related_requests() {
    let (mut client, mut server) = establish_pair();
    // 同じ Track の subscription を確立しておく
    let subscribed = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, subscribe) = take_send_request(&mut client);
    server
        .recv_request(subscribe)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            subscribed,
            707,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(subscribed, ok)
        .expect("テストフィクスチャの前提条件を満たす");

    let status = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, track_status) = take_send_request(&mut client);
    server
        .recv_request(track_status)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(
            status,
            MessageParameters::new(),
            track_properties_with_prior_group_id_gap(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(status, ok)
        .expect("Malformed Track の検出はセッションを閉じないこと");

    // TRACK_STATUS は応答済みの失敗として確定し、成功イベントを発行しない
    assert!(
        matches!(
            client
                .track_status_request(status)
                .expect("TRACK_STATUS が登録されていること")
                .response,
            Some(shiguredo_moqt::session::types::TrackStatusResponse::Error)
        ),
        "TRACK_STATUS_OK を成功として確定しないこと"
    );
    assert_eq!(
        client
            .subscription(subscribed)
            .expect("subscription が登録されていること")
            .state,
        SubscriptionState::Terminated,
        "同じ Track の subscription を取消対象にすること"
    );
    // `RequestOkReceived` を発行する経路は `TrackStatusResponse::Ok` を記録するため、
    // 応答が `Error` であることが成功通知を発行していないことの根拠になる。
    assert_eq!(client.state(), SessionState::Established);
    assert_malformed_terminations(
        &mut client,
        &[
            (subscribed, RequestKind::Subscribe),
            (status, RequestKind::TrackStatus),
        ],
    );
}

/// wire 入力の TRACK_STATUS_OK でも Track 文脈を失わず取消へ進む
#[test]
fn track_status_ok_with_object_only_property_on_wire_cancels_track_status() {
    let (mut client, mut server) = establish_pair();
    let status = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, track_status) = take_send_request(&mut client);
    server
        .recv_request(track_status)
        .expect("テストフィクスチャの前提条件を満たす");
    let wire = track_status_ok_wire(&prior_group_id_gap_kvp_bytes());
    let (message, consumed) =
        ControlMessage::decode(&wire).expect("スコープ違反は構造 decode を失敗させないこと");
    assert_eq!(consumed, wire.len());
    client
        .recv_stream_message(status, message)
        .expect("Malformed Track の検出はセッションを閉じないこと");
    assert!(
        matches!(
            client
                .track_status_request(status)
                .expect("TRACK_STATUS が登録されていること")
                .response,
            Some(shiguredo_moqt::session::types::TrackStatusResponse::Error)
        ),
        "TRACK_STATUS_OK を成功として確定しないこと"
    );
    assert_eq!(client.state(), SessionState::Established);
    assert_malformed_terminations(&mut client, &[(status, RequestKind::TrackStatus)]);
}

// ─── Object scope (data plane) ───────────────────────────

/// Object Properties の Track 専用既知 Property は subgroup 経路で Malformed Track になる
///
/// `ObjectProperties::decode_exact` は `MalformedTrack` を `ProtocolViolation` へ畳まず
/// 透過し、data plane の既存取消経路へ渡す。
#[test]
fn subgroup_object_with_track_only_property_terminates_subscription() {
    let alias = 861u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(80);
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
    let err = client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 4,
                status: None,
                properties_bytes: Some(object_properties_with_max_cache_duration()),
            },
        )
        .expect_err("Track 専用 Property を Object scope で受けたら Malformed Track");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        err.reason, "Track-only property in Object scope",
        "Object scope のスコープ違反として Malformed Track にすること"
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
        SubscriptionState::Terminated
    );
    let (cancels, reset_streams, terminated) =
        drain_malformed_events(&mut client, rid, Some(stream_id));
    assert_eq!(cancels, vec!["stop_sending", "reset"]);
    assert_eq!(
        reset_streams, 1,
        "malformed を運んだ stream を reset すること"
    );
    assert_eq!(terminated, 1);
}

/// IMMUTABLE_PROPERTIES の内側の Track 専用既知 Property も Object scope で malformed になる
#[test]
fn subgroup_object_with_track_only_property_in_immutable_terminates_subscription() {
    let alias = 862u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(81);
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
    let err = client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 4,
                status: None,
                properties_bytes: Some(object_properties_with_max_cache_duration_in_immutable()),
            },
        )
        .expect_err("IMMUTABLE 内側の Track 専用 Property も Malformed Track");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(err.reason, "Track-only property in Object scope");
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
}

/// Object Properties の Track 専用既知 Property は datagram 経路でも Malformed Track になる
#[test]
fn datagram_with_track_only_property_terminates_subscription() {
    let alias = 863u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    let err = client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: alias,
            group_id: 0,
            object_id: 0,
            publisher_priority: Some(7),
            properties_data: Some(object_properties_with_max_cache_duration()),
            end_of_group: false,
            status: None,
        })
        .expect_err("Track 専用 Property を Object scope で受けたら Malformed Track");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(err.reason, "Track-only property in Object scope");
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
    let (cancels, reset_streams, terminated) = drain_malformed_events(&mut client, rid, None);
    assert_eq!(cancels, vec!["stop_sending", "reset"]);
    assert_eq!(
        reset_streams, 0,
        "datagram 経路は data stream を持たないこと"
    );
    assert_eq!(terminated, 1);
}

/// Track 専用 Property を持たない Object Properties は従来どおり受理される (非回帰)
#[test]
fn subgroup_object_with_unknown_property_is_accepted() {
    let alias = 864u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(82);
    let header = SubgroupHeader {
        track_alias: alias,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(10),
        has_properties: true,
        end_of_group: false,
        first_object: false,
    };
    let mut kvps = Vec::new();
    shiguredo_moqt::varint::encode(0x1000, &mut kvps); // 未知 (偶数型)
    shiguredo_moqt::varint::encode(1, &mut kvps);
    let mut properties = Vec::new();
    shiguredo_moqt::varint::encode(kvps.len() as u64, &mut properties);
    properties.extend_from_slice(&kvps);
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
                properties_bytes: Some(properties),
            },
        )
        .expect("未知 Property はスコープを推測して拒否しないこと");
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Established
    );
    let (per_request, _) = drain_malformed_events_by_request(&mut client);
    assert!(per_request.is_empty());
}
