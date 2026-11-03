//! Property の多重違反 (Track / Object) を Malformed Track として扱う検証
//!
//! draft-ietf-moq-transport-23 §8.4 (Track and Object Properties): "A Track or Object MUST NOT
//! contain more than one instance of the Property." (Property の定義が複数を許す場合を除く) /
//! §12.1 (Malformed Tracks) 条件 9。
//!
//! 未知の Property の複数インスタンスは同節の "MUST allow multiple instances of the Property"
//! と §16.8 (Properties) の "Endpoints MUST ignore unknown Property types" により正当であり、
//! 保持したまま受理する。
//!
//! Track Properties の多重検証は構造 decode ではなく、Request ID と Full Track Name を持つ
//! 受信ハンドラが行う。違反はセッション終了ではなく、同じ Track の subscription / FETCH の
//! cancel (§6.4.2.3) とアプリへの `RequestTerminated { reason: MalformedTrack }` 通知
//! (§12.1 の SHOULD) へ渡す。Object 側は `ObjectProperties::decode_exact` が
//! `MalformedTrack` を保ったまま data plane の同じ取消経路へ渡す。

use super::*;

use shiguredo_moqt::message::common::TrackNamespace;
use shiguredo_moqt::track_properties::{
    PROP_IMMUTABLE_PROPERTIES, PROP_OBJECT_DELIVERY_TIMEOUT, TrackProperty, TrackPropertyValue,
};

// ─── フィクスチャ ─────────────────────────────────────────

/// 単一に限られる既知 Property が重複したときの理由
const DUPLICATE_TRACK: &str = "duplicate track property";

/// 単一に限られる既知 Property が Object scope で重複したときの理由
const DUPLICATE_OBJECT: &str = "duplicate object property";

/// OBJECT_DELIVERY_TIMEOUT (0x02) を 2 インスタンス持つ Track Properties
///
/// 0x02 は Track と Object の両スコープで、定義に複数を許す規則が無いため単一に限られる。
fn track_properties_with_duplicate_delivery_timeout() -> TrackProperties {
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(100),
    });
    tp.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(200),
    });
    tp
}

/// IMMUTABLE_PROPERTIES の内側だけで OBJECT_DELIVERY_TIMEOUT (0x02) を 2 インスタンス持つ
/// Track Properties
///
/// 重複した既知 Property を含む Track Properties は `TrackProperties::encode` がローカル入力の
/// 検証として拒否するため、内側の KVP 列を手組みする。
fn track_properties_with_duplicate_delivery_timeout_in_immutable() -> TrackProperties {
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: PROP_IMMUTABLE_PROPERTIES,
        value: TrackPropertyValue::Bytes(duplicate_even_kvp(
            PROP_OBJECT_DELIVERY_TIMEOUT,
            &[100, 200],
        )),
    });
    tp
}

/// 未知の Type (0x1000) を同値と異値で 2 インスタンス持つ Track Properties
///
/// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) の "MUST allow multiple
/// instances of the Property" により正当であり、ローカル encode も受理する。
fn track_properties_with_unknown_duplicate() -> TrackProperties {
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: 0x1000,
        value: TrackPropertyValue::VarInt(7),
    });
    tp.push(TrackProperty {
        prop_type: 0x1000,
        value: TrackPropertyValue::VarInt(8),
    });
    tp
}

/// 同じ既知 Property を delta=0 で繰り返す偶数型 KVP の生バイト列
fn duplicate_even_kvp(prop_type: u64, values: &[u64]) -> Vec<u8> {
    let mut bytes = Vec::new();
    shiguredo_moqt::varint::encode(prop_type, &mut bytes);
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            // delta=0 で直前と同じ型を表す
            shiguredo_moqt::varint::encode(0, &mut bytes);
        }
        shiguredo_moqt::varint::encode(*value, &mut bytes);
    }
    bytes
}

/// IMMUTABLE_PROPERTIES の内側に重複した既知 Property を持つ Track Properties の KVP 生バイト列
fn duplicate_in_immutable_kvp() -> Vec<u8> {
    let inner = duplicate_even_kvp(0x02, &[100, 200]);
    let mut bytes = Vec::new();
    shiguredo_moqt::varint::encode(PROP_IMMUTABLE_PROPERTIES, &mut bytes);
    shiguredo_moqt::varint::encode(inner.len() as u64, &mut bytes);
    bytes.extend_from_slice(&inner);
    bytes
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

/// Track Properties の生バイト列を持つ FETCH_OK の wire 表現 (Type 0x07)
///
/// draft-ietf-moq-transport-23 §9.3 (REQUEST_OK): FETCH_OK は REQUEST_OK (Type 0x07) の
/// 別名であり、本文は Number of Parameters + Parameters + Track Properties である。
fn fetch_ok_wire(track_properties: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    shiguredo_moqt::varint::encode(0, &mut payload); // Number of Parameters
    payload.extend_from_slice(track_properties);
    frame(0x07, &payload)
}

/// Track Properties の生バイト列を持つ REQUEST_OK (TRACK_STATUS_OK) の wire 表現 (Type 0x07)
fn track_status_ok_wire(track_properties: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    shiguredo_moqt::varint::encode(0, &mut payload); // Number of Parameters
    payload.extend_from_slice(track_properties);
    frame(0x07, &payload)
}

/// Object Properties の KVP 列を Properties Length 付きのブロックにする
fn object_properties_blob(kvps: &[u8]) -> Vec<u8> {
    let mut block = Vec::new();
    shiguredo_moqt::varint::encode(kvps.len() as u64, &mut block);
    block.extend_from_slice(kvps);
    block
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

/// 受信ハンドラが使う多重検証が多重違反を MalformedTrack にすること
#[test]
fn validate_reports_duplicate_track_property() {
    use shiguredo_moqt::error::MessageError;
    assert_eq!(
        track_properties_with_duplicate_delivery_timeout().validate(),
        Err(MessageError::MalformedTrack(DUPLICATE_TRACK))
    );
}

// ─── PUBLISH (初回) ───────────────────────────────────────

/// 既知 Property を重複して持つ初回 PUBLISH は購読として登録せず取消へ進む
///
/// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1 (Malformed Tracks)
/// 条件 9。受信側は購読を登録する前でも、受信メッセージの Request ID と Full Track Name から
/// Track 文脈を解決して cancel し、アプリへは正常な受信として通知しない。
#[test]
fn publish_with_duplicate_track_property_is_not_registered_and_is_cancelled() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            700,
            MessageParameters::new(),
            track_properties_with_duplicate_delivery_timeout(),
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
}

/// IMMUTABLE_PROPERTIES の内側だけに重複を持つ PUBLISH も同じ経路で取消へ進む
#[test]
fn publish_with_duplicate_track_property_in_immutable_is_cancelled() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            701,
            MessageParameters::new(),
            track_properties_with_duplicate_delivery_timeout_in_immutable(),
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
/// `TrackProperties::decode` / `ControlMessage::decode` は多重違反だけを理由に失敗しない。
/// 違反の判定は Request ID と Full Track Name を得た受信ハンドラの責務である。
#[test]
fn publish_with_duplicate_track_property_on_wire_is_cancelled() {
    let (_client, mut server) = establish_pair();
    let kvps = duplicate_even_kvp(0x02, &[100, 200]);
    let wire = publish_wire(0, &kvps);
    let (message, consumed) =
        ControlMessage::decode(&wire).expect("多重違反は構造 decode を失敗させないこと");
    assert_eq!(consumed, wire.len(), "メッセージ全体を消費すること");
    let ControlMessage::Publish(publish) = &message else {
        panic!("Publish が期待されたが {message:?} だった");
    };
    let rid = publish.request_id;
    assert_eq!(
        publish.track_properties.len(),
        2,
        "重複した Property を保持したまま Session へ渡すこと"
    );
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

/// 未知 Property の複数インスタンスを持つ PUBLISH は購読として登録する
///
/// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) の "MUST allow multiple
/// instances of the Property" / §16.8 (Properties) の "Endpoints MUST ignore unknown Property
/// types"。
#[test]
fn publish_with_unknown_duplicate_property_is_accepted() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            702,
            MessageParameters::new(),
            track_properties_with_unknown_duplicate(),
        )
        .expect("未知 Property の重複は encode できること");
    let (_, publish) = take_send_request(&mut client);
    // wire を経由させ、decode が未知 Property の重複を保持したまま Session へ渡すことを確かめる
    let encoded = publish
        .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
        .expect("未知 Property の重複を含む PUBLISH は encode できること");
    let (decoded, consumed) = ControlMessage::decode(&encoded).expect("decode は成功すること");
    assert_eq!(consumed, encoded.len());
    server
        .recv_request(decoded)
        .expect("未知 Property の重複は拒否しないこと");
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

/// 既知 Property を重複して持つ SUBSCRIBE_OK は購読を cancel する
#[test]
fn subscribe_ok_with_duplicate_track_property_cancels_subscription() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, subscribe) = take_send_request(&mut client);
    server
        .recv_request(subscribe)
        .expect("テストフィクスチャの前提条件を満たす");
    // 送信側の `send_subscribe_ok` はローカル入力の検証 (encode) で重複を拒否する。
    // ここでは受信ハンドラの検証を確かめるため、受信メッセージを直接組み立てる。
    client
        .recv_stream_message(
            rid,
            ControlMessage::SubscribeOk(shiguredo_moqt::message::SubscribeOk {
                track_alias: 705,
                parameters: MessageParameters::new(),
                track_properties: track_properties_with_duplicate_delivery_timeout(),
            }),
        )
        .expect("Malformed Track の検出はセッションを閉じないこと");
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が登録されていること")
            .state,
        SubscriptionState::Terminated,
        "多重違反の SUBSCRIBE_OK で購読をキャンセルすること"
    );
    assert_eq!(client.state(), SessionState::Established);
    assert_malformed_terminations(&mut client, &[(rid, RequestKind::Subscribe)]);
}

/// wire 入力の SUBSCRIBE_OK でも Track 文脈を失わず取消へ進む
#[test]
fn subscribe_ok_with_duplicate_track_property_on_wire_cancels_subscription() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, subscribe) = take_send_request(&mut client);
    server
        .recv_request(subscribe)
        .expect("テストフィクスチャの前提条件を満たす");
    let kvps = duplicate_even_kvp(0x02, &[100, 200]);
    let wire = subscribe_ok_wire(706, &kvps);
    let (message, consumed) =
        ControlMessage::decode(&wire).expect("多重違反は構造 decode を失敗させないこと");
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

/// 既知 Property を重複して持つ FETCH_OK は fetch を cancel する
#[test]
fn fetch_ok_with_duplicate_track_property_cancels_fetch() {
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
            ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
                parameters: fetch_ok_params(Some(Location {
                    group_id: 5,
                    object_id: 0,
                })),
                track_properties: track_properties_with_duplicate_delivery_timeout(),
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
fn fetch_ok_with_duplicate_track_property_on_wire_cancels_fetch() {
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
    let wire = fetch_ok_wire(&duplicate_even_kvp(0x02, &[100, 200]));
    let (message, consumed) =
        ControlMessage::decode(&wire).expect("多重違反は構造 decode を失敗させないこと");
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

/// 既知 Property を重複して持つ TRACK_STATUS_OK は TRACK_STATUS と関連要求を cancel する
#[test]
fn track_status_ok_with_duplicate_track_property_cancels_related_requests() {
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
    let wire = track_status_ok_wire(&duplicate_even_kvp(0x02, &[100, 200]));
    let (message, consumed) =
        ControlMessage::decode(&wire).expect("多重違反は構造 decode を失敗させないこと");
    assert_eq!(consumed, wire.len());
    client
        .recv_stream_message(status, message)
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
    assert_eq!(client.state(), SessionState::Established);
    assert_malformed_terminations(
        &mut client,
        &[
            (subscribed, RequestKind::Subscribe),
            (status, RequestKind::TrackStatus),
        ],
    );
}

/// 直接 Session API で組み立てた TRACK_STATUS_OK でも TRACK_STATUS を cancel する
///
/// 送信側の `send_request_ok` はローカル入力の検証 (encode) で重複を拒否するため、
/// 受信メッセージを直接組み立てて受信ハンドラの検証を確かめる。
#[test]
fn track_status_ok_with_duplicate_track_property_cancels_track_status() {
    let (mut client, mut server) = establish_pair();
    let status = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, track_status) = take_send_request(&mut client);
    server
        .recv_request(track_status)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_stream_message(
            status,
            ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
                parameters: MessageParameters::new(),
                track_properties: track_properties_with_duplicate_delivery_timeout(),
            }),
        )
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

/// Object Properties の既知 Property の重複は subgroup 経路で Malformed Track になる
///
/// `ObjectProperties::decode_exact` は `MalformedTrack` を `ProtocolViolation` へ畳まず
/// 透過し、data plane の既存取消経路へ渡す。
#[test]
fn subgroup_object_with_duplicate_property_terminates_subscription() {
    let alias = 871u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(90);
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
    let properties = object_properties_blob(&duplicate_even_kvp(0x02, &[100, 200]));
    let err = client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 4,
                status: None,
                properties_bytes: Some(properties),
            },
        )
        .expect_err("既知 Property の重複を Object scope で受けたら Malformed Track");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        err.reason, DUPLICATE_OBJECT,
        "多重違反として Malformed Track にすること"
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

/// IMMUTABLE_PROPERTIES の内側の既知 Property の重複も Object scope で malformed になる
#[test]
fn subgroup_object_with_duplicate_property_in_immutable_terminates_subscription() {
    let alias = 872u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(91);
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
    let kvps = duplicate_in_immutable_kvp();
    let err = client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 0,
                payload_length: 4,
                status: None,
                properties_bytes: Some(object_properties_blob(&kvps)),
            },
        )
        .expect_err("IMMUTABLE 内側の既知 Property の重複も Malformed Track");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(err.reason, DUPLICATE_OBJECT);
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Terminated
    );
}

/// Object Properties の既知 Property の重複は datagram 経路でも Malformed Track になる
#[test]
fn datagram_with_duplicate_property_terminates_subscription() {
    let alias = 873u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    let properties = object_properties_blob(&duplicate_even_kvp(0x02, &[100, 200]));
    let err = client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: alias,
            group_id: 0,
            object_id: 0,
            publisher_priority: Some(7),
            properties_data: Some(properties),
            end_of_group: false,
            status: None,
        })
        .expect_err("既知 Property の重複を Object scope で受けたら Malformed Track");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(err.reason, DUPLICATE_OBJECT);
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

/// 未知 Property の複数インスタンスを持つ Object Properties は受理される
#[test]
fn subgroup_object_with_unknown_duplicate_property_is_accepted() {
    let alias = 874u64;
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    let stream_id = DataStreamId(92);
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
    let properties = object_properties_blob(&duplicate_even_kvp(0x1000, &[1, 2]));
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
        .expect("未知 Property の重複は多重違反にしないこと");
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
