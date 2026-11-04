//! peer が送った Track Properties を `Subscription::peer_track_properties` から取得できることの検証
//!
//! draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) は Track Properties を
//! それを運ぶメッセージの最終フィールドと定める。同 §10.7 (Immutable Properties) は、
//! プロパティの値を探す処理者に mutable リストと IMMUTABLE_PROPERTIES (0x0B) の内側の
//! 両方を検索することを MUST とする。
//!
//! draft-ietf-moq-loc-05 §3.1.2 (Immutable Properties) は、relay から見える不変の LOC
//! プロパティを IMMUTABLE_PROPERTIES として符号化することを SHOULD とする。
//! draft-ietf-moq-loc-05 §6.1 (MOQ Properties Registry) Table 1 は 0x08 (TIMESCALE) /
//! 0x0D (VIDEO_CONFIG) / 0x0F (AUDIO_CONFIG) / 0x11 (CODEC_STRING) の Scope を
//! "Track, Object" と定める。
//!
//! 本ファイルは、それらの Track スコープ値が peer から届いたときに application が
//! `Session::subscription(request_id)` 経由で取得できることを、公開入口
//! (`Session::recv_request` / `Session::recv_stream_message`) だけを使って固定する。
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use super::*;
use shiguredo_moqt::loc::{
    PROP_AUDIO_CONFIG, PROP_CODEC_STRING, PROP_TIMESCALE, PROP_VIDEO_CONFIG,
};
use shiguredo_moqt::message_parameter::PARAM_INCLUDE_PROPERTIES;
use shiguredo_moqt::session::types::{DynamicGroupsSupport, Subscription};
use shiguredo_moqt::track_properties::{
    PROP_DEFAULT_PUBLISHER_GROUP_ORDER, PROP_DEFAULT_PUBLISHER_PRIORITY, PROP_DYNAMIC_GROUPS,
    PROP_IMMUTABLE_PROPERTIES, PROP_OBJECT_DELIVERY_TIMEOUT, PROP_SUBGROUP_DELIVERY_TIMEOUT,
    TrackProperty, TrackPropertyValue,
};

// ─── フィクスチャ ─────────────────────────────────────────

/// VIDEO_CONFIG (0x0D) として送る H.264 の AVCDecoderConfigurationRecord 風のバイト列
const VIDEO_CONFIG: &[u8] = &[0x01, 0x64, 0x00, 0x1F];

/// AUDIO_CONFIG (0x0F) として送る Opus の identification header 風のバイト列
const AUDIO_CONFIG: &[u8] = b"OpusHead";

/// TIMESCALE (0x08) として送る 1 秒あたりのユニット数 (48 kHz)
const TIMESCALE: u64 = 48_000;

/// CODEC_STRING (0x11) として送る WebCodecs Codec Registry の codec 値 (AV1)
const CODEC_STRING: &str = "av01.0.04M.08";

/// この実装が意味を理解しない未知の奇数型 Property (複数インスタンスが許される)
const UNKNOWN_ODD_TYPE: u64 = 0x1001;

/// 未知の奇数型 Property の値 (加工されずに保持されることの確認用)
const UNKNOWN_ODD_VALUE: &[u8] = &[0xAA, 0xBB];

/// Track スコープの LOC プロパティ 4 つを最上位リストに持つ Track Properties
fn loc_track_properties() -> TrackProperties {
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: PROP_TIMESCALE,
        value: TrackPropertyValue::VarInt(TIMESCALE),
    });
    tp.push(TrackProperty {
        prop_type: PROP_VIDEO_CONFIG,
        value: TrackPropertyValue::Bytes(VIDEO_CONFIG.to_vec()),
    });
    tp.push(TrackProperty {
        prop_type: PROP_AUDIO_CONFIG,
        value: TrackPropertyValue::Bytes(AUDIO_CONFIG.to_vec()),
    });
    tp.push(TrackProperty {
        prop_type: PROP_CODEC_STRING,
        value: TrackPropertyValue::Bytes(CODEC_STRING.as_bytes().to_vec()),
    });
    tp
}

/// IMMUTABLE_PROPERTIES (0x0B) の内側だけに LOC プロパティ 4 つを持つ Track Properties
///
/// draft-ietf-moq-loc-05 §3.1.2 (Immutable Properties) が SHOULD とする符号化を再現する。
/// 内側は `TrackProperties::encode` で組み立てることで、実装が生成するのと同じ
/// デルタ符号化された KVP 列にする (専用の手組みパーサーを持ち込まない)。
fn loc_track_properties_in_immutable() -> TrackProperties {
    let mut inner = Vec::new();
    loc_track_properties()
        .encode(&mut inner)
        .expect("IMMUTABLE_PROPERTIES の内側の KVP 列の encode に成功すること");
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: PROP_IMMUTABLE_PROPERTIES,
        value: TrackPropertyValue::Bytes(inner),
    });
    tp
}

/// 既知の MOQT 値 5 つ・LOC プロパティ 4 つ・未知の奇数型 1 つを併せ持つ Track Properties
///
/// 既知 MOQT 値の取り込みが LOC 値や未知の Property の追加で変わらないことを確かめる
/// ための入力である。
fn track_properties_with_known_moq_and_loc_values() -> TrackProperties {
    let mut tp = loc_track_properties();
    tp.push(TrackProperty {
        prop_type: PROP_DYNAMIC_GROUPS,
        value: TrackPropertyValue::VarInt(1),
    });
    tp.push(TrackProperty {
        prop_type: PROP_DEFAULT_PUBLISHER_PRIORITY,
        value: TrackPropertyValue::VarInt(42),
    });
    tp.push(TrackProperty {
        prop_type: PROP_DEFAULT_PUBLISHER_GROUP_ORDER,
        value: TrackPropertyValue::VarInt(2),
    });
    tp.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(3_000),
    });
    tp.push(TrackProperty {
        prop_type: PROP_SUBGROUP_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(4_000),
    });
    tp.push(TrackProperty {
        prop_type: UNKNOWN_ODD_TYPE,
        value: TrackPropertyValue::Bytes(UNKNOWN_ODD_VALUE.to_vec()),
    });
    tp
}

/// 受信側 subscription の `peer_track_properties` から LOC の Track スコープ値を検証する
///
/// 0x0D / 0x0F / 0x11 は奇数型なので `find_bytes` で生バイト列として、0x08 は偶数型なので
/// `find_varint` で varint として取得できなければならない。0x11 は `codec_string` で
/// UTF-8 文字列として取得できること、型を取り違えた探索が値を返さないことも併せて確認する。
fn assert_loc_track_properties(sub: &Subscription) {
    assert_eq!(
        sub.peer_track_properties.find_bytes(PROP_VIDEO_CONFIG),
        Some(VIDEO_CONFIG),
        "VIDEO_CONFIG (0x0D) の生バイト列が取得できること"
    );
    assert_eq!(
        sub.peer_track_properties.find_bytes(PROP_AUDIO_CONFIG),
        Some(AUDIO_CONFIG),
        "AUDIO_CONFIG (0x0F) の生バイト列が取得できること"
    );
    assert_eq!(
        sub.peer_track_properties.find_bytes(PROP_CODEC_STRING),
        Some(CODEC_STRING.as_bytes()),
        "CODEC_STRING (0x11) の生バイト列が取得できること"
    );
    assert_eq!(
        sub.peer_track_properties.codec_string(),
        Ok(Some(CODEC_STRING)),
        "CODEC_STRING (0x11) を UTF-8 文字列として取得できること"
    );
    assert_eq!(
        sub.peer_track_properties.find_varint(PROP_TIMESCALE),
        Some(TIMESCALE),
        "TIMESCALE (0x08) の varint 値が取得できること"
    );
    // 奇数型の値は varint として、偶数型の値はバイト列として取り出せないこと。
    // 「たまたま別の探索経路で値が返る」実装になっていないことの裏付けになる。
    assert_eq!(
        sub.peer_track_properties.find_varint(PROP_VIDEO_CONFIG),
        None,
        "VIDEO_CONFIG (0x0D) は varint 値を持たないこと"
    );
    assert_eq!(
        sub.peer_track_properties.find_varint(PROP_AUDIO_CONFIG),
        None,
        "AUDIO_CONFIG (0x0F) は varint 値を持たないこと"
    );
    assert_eq!(
        sub.peer_track_properties.find_bytes(PROP_TIMESCALE),
        None,
        "TIMESCALE (0x08) は生バイト列を持たないこと"
    );
}

/// 既知の MOQT 値が個別フィールドへ取り込まれていることを検証する
fn assert_known_moq_values(sub: &Subscription) {
    assert_eq!(
        sub.dynamic_groups,
        DynamicGroupsSupport::Supported,
        "DYNAMIC_GROUPS=1 が対応済みとして取り込まれること"
    );
    assert_eq!(
        sub.default_publisher_priority,
        Some(42),
        "DEFAULT_PUBLISHER_PRIORITY が取り込まれること"
    );
    assert_eq!(
        sub.default_publisher_group_order,
        Some(2),
        "DEFAULT_PUBLISHER_GROUP_ORDER が取り込まれること"
    );
    assert_eq!(
        sub.delivery_timeouts.publisher_object_ms,
        Some(3_000),
        "OBJECT_DELIVERY_TIMEOUT が取り込まれること"
    );
    assert_eq!(
        sub.delivery_timeouts.publisher_subgroup_ms,
        Some(4_000),
        "SUBGROUP_DELIVERY_TIMEOUT が取り込まれること"
    );
    // 生の Track Properties 側にも既知値が残っていること (個別フィールドへの取り込みが
    // 元の値を捨てる置き換えになっていないこと)。
    assert_eq!(
        sub.peer_track_properties.dynamic_groups(),
        Some(1),
        "peer_track_properties に DYNAMIC_GROUPS が残っていること"
    );
    assert_eq!(
        sub.peer_track_properties.default_publisher_priority(),
        Some(42),
        "peer_track_properties に DEFAULT_PUBLISHER_PRIORITY が残っていること"
    );
    // 未知の Property も加工せずに保持されること (draft-ietf-moq-transport-23 §16.8
    // (Properties) の "Endpoints MUST ignore unknown Property types")。find_bytes は
    // 未知の奇数型も生バイト列として返す。
    assert!(
        sub.peer_track_properties
            .as_slice()
            .iter()
            .any(|p| p.prop_type == UNKNOWN_ODD_TYPE),
        "peer_track_properties に未知の Property が残っていること"
    );
    assert_eq!(
        sub.peer_track_properties.find_bytes(UNKNOWN_ODD_TYPE),
        Some(UNKNOWN_ODD_VALUE),
        "未知の奇数型 Property の生バイト列が取得できること"
    );
}

/// 送信メッセージを wire の encode / decode に通してから受信側へ渡す
///
/// 実 I/O 層と同じく `ControlMessage::encode` / `ControlMessage::decode` を経由させることで、
/// `TrackProperties` の wire 直列化から `Session` の購読状態までを通しで検証する。
fn roundtrip_wire(message: ControlMessage) -> ControlMessage {
    let encoded = message
        .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
        .expect("テストフィクスチャのメッセージは encode できる");
    let (decoded, consumed) =
        ControlMessage::decode(&encoded).expect("encode した wire を decode できる");
    assert_eq!(consumed, encoded.len(), "メッセージ全体を消費すること");
    decoded
}

/// peer publisher からの PUBLISH を wire 経由で公開入口 `Session::recv_request` に渡し、
/// 受信側 (subscriber 役) の Session と Request ID を返す
fn recv_peer_publish(properties: TrackProperties) -> (Session, u64) {
    let (mut publisher, mut subscriber) = establish_pair();
    let rid = publisher
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            700,
            MessageParameters::new(),
            properties,
        )
        .expect("PUBLISH の送信に成功すること");
    let (_, publish_msg) = take_send_request(&mut publisher);
    subscriber
        .recv_request(roundtrip_wire(publish_msg))
        .expect("PUBLISH の受信に成功すること");
    (subscriber, rid)
}

/// peer publisher からの SUBSCRIBE_OK を wire 経由で公開入口
/// `Session::recv_stream_message` に渡し、受信側 (subscriber 役) の Session と Request ID を
/// 返す
fn recv_peer_subscribe_ok(properties: TrackProperties) -> (Session, u64) {
    let (mut subscriber, mut publisher) = establish_pair();
    let rid = subscriber
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, subscribe_msg) = take_send_request(&mut subscriber);
    publisher
        .recv_request(subscribe_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    publisher
        .send_subscribe_ok(rid, 700, MessageParameters::new(), properties)
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, subscribe_ok_msg) = take_send_on_stream(&mut publisher);
    subscriber
        .recv_stream_message(rid, roundtrip_wire(subscribe_ok_msg))
        .expect("SUBSCRIBE_OK の受信に成功すること");
    (subscriber, rid)
}

// ─── PUBLISH 経路 ─────────────────────────────────────────

/// PUBLISH の最上位リストに置かれた Track スコープの LOC プロパティを subscriber が取得できる
#[test]
fn publish_exposes_track_scope_loc_properties() {
    let (subscriber, rid) = recv_peer_publish(loc_track_properties());
    let sub = subscriber
        .subscription(rid)
        .expect("PUBLISH を受信した subscription が存在すること");

    // application は Session::subscription の peer_track_properties から値を取得できる。
    assert_loc_track_properties(sub);
    // 最上位リストに置かれた値であることを確認する (IMMUTABLE_PROPERTIES に包まれて
    // いない入力を、内側探索の経路ではなく mutable リストの探索で取得している)。
    assert!(
        sub.peer_track_properties
            .as_slice()
            .iter()
            .any(|p| p.prop_type == PROP_VIDEO_CONFIG),
        "VIDEO_CONFIG が最上位リストに存在すること"
    );
    assert!(
        sub.peer_track_properties
            .as_slice()
            .iter()
            .any(|p| p.prop_type == PROP_AUDIO_CONFIG),
        "AUDIO_CONFIG が最上位リストに存在すること"
    );
    assert!(
        sub.peer_track_properties
            .as_slice()
            .iter()
            .any(|p| p.prop_type == PROP_TIMESCALE),
        "TIMESCALE が最上位リストに存在すること"
    );
}

/// PUBLISH の IMMUTABLE_PROPERTIES の内側に置かれた LOC プロパティを subscriber が取得できる
#[test]
fn publish_exposes_track_scope_loc_properties_in_immutable() {
    let (subscriber, rid) = recv_peer_publish(loc_track_properties_in_immutable());
    let sub = subscriber
        .subscription(rid)
        .expect("PUBLISH を受信した subscription が存在すること");

    // draft-ietf-moq-transport-23 §10.7 (Immutable Properties) の "MUST search both" に
    // 従い、内側だけに置かれた値も取得できること。
    assert_loc_track_properties(sub);
    // 最上位リストには IMMUTABLE_PROPERTIES (0x0B) だけが入っていることを確認する。
    // これにより、上の検証が mutable リストではなく内側の探索で通っていることが分かる。
    assert_eq!(
        sub.peer_track_properties.as_slice().len(),
        1,
        "最上位リストには IMMUTABLE_PROPERTIES だけが入っていること"
    );
    assert_eq!(
        sub.peer_track_properties.as_slice()[0].prop_type,
        PROP_IMMUTABLE_PROPERTIES,
        "最上位リストの唯一の要素が IMMUTABLE_PROPERTIES であること"
    );
}

/// peer が Track Properties を送らない PUBLISH では空の Track Properties として取得できる
#[test]
fn publish_without_track_properties_yields_empty_peer_track_properties() {
    let (subscriber, rid) = recv_peer_publish(TrackProperties::new());
    let sub = subscriber
        .subscription(rid)
        .expect("PUBLISH を受信した subscription が存在すること");

    // 空でも panic せず、空として観測できること。
    assert!(
        sub.peer_track_properties.is_empty(),
        "Track Properties を送らない PUBLISH では空になること"
    );
    assert_eq!(
        sub.peer_track_properties.find_bytes(PROP_VIDEO_CONFIG),
        None,
        "空の Track Properties から VIDEO_CONFIG は取得できないこと"
    );
    assert_eq!(
        sub.peer_track_properties.find_varint(PROP_TIMESCALE),
        None,
        "空の Track Properties から TIMESCALE は取得できないこと"
    );
}

// ─── SUBSCRIBE_OK 経路 ────────────────────────────────────

/// SUBSCRIBE_OK の最上位リストに置かれた Track スコープの LOC プロパティを subscriber が取得できる
#[test]
fn subscribe_ok_exposes_track_scope_loc_properties() {
    let (subscriber, rid) = recv_peer_subscribe_ok(loc_track_properties());
    let sub = subscriber
        .subscription(rid)
        .expect("SUBSCRIBE_OK を受信した subscription が存在すること");

    assert_loc_track_properties(sub);
    assert!(
        sub.peer_track_properties
            .as_slice()
            .iter()
            .any(|p| p.prop_type == PROP_VIDEO_CONFIG),
        "VIDEO_CONFIG が最上位リストに存在すること"
    );
    assert!(
        sub.peer_track_properties
            .as_slice()
            .iter()
            .any(|p| p.prop_type == PROP_TIMESCALE),
        "TIMESCALE が最上位リストに存在すること"
    );
}

/// SUBSCRIBE_OK の IMMUTABLE_PROPERTIES の内側に置かれた LOC プロパティを subscriber が
/// 取得できる
#[test]
fn subscribe_ok_exposes_track_scope_loc_properties_in_immutable() {
    let (subscriber, rid) = recv_peer_subscribe_ok(loc_track_properties_in_immutable());
    let sub = subscriber
        .subscription(rid)
        .expect("SUBSCRIBE_OK を受信した subscription が存在すること");

    assert_loc_track_properties(sub);
    assert_eq!(
        sub.peer_track_properties.as_slice().len(),
        1,
        "最上位リストには IMMUTABLE_PROPERTIES だけが入っていること"
    );
    assert_eq!(
        sub.peer_track_properties.as_slice()[0].prop_type,
        PROP_IMMUTABLE_PROPERTIES,
        "最上位リストの唯一の要素が IMMUTABLE_PROPERTIES であること"
    );
}

/// peer が Track Properties を送らない SUBSCRIBE_OK では空の Track Properties として
/// 取得できる
#[test]
fn subscribe_ok_without_track_properties_yields_empty_peer_track_properties() {
    let (subscriber, rid) = recv_peer_subscribe_ok(TrackProperties::new());
    let sub = subscriber
        .subscription(rid)
        .expect("SUBSCRIBE_OK を受信した subscription が存在すること");

    // 空でも panic せず、空として観測できること。
    assert!(
        sub.peer_track_properties.is_empty(),
        "Track Properties を送らない SUBSCRIBE_OK では空になること"
    );
    assert_eq!(
        sub.peer_track_properties.find_bytes(PROP_AUDIO_CONFIG),
        None,
        "空の Track Properties から AUDIO_CONFIG は取得できないこと"
    );
    assert_eq!(
        sub.peer_track_properties.find_varint(PROP_TIMESCALE),
        None,
        "空の Track Properties から TIMESCALE は取得できないこと"
    );
}

/// subscriber が INCLUDE_PROPERTIES=0 を指定すると、空化された SUBSCRIBE_OK が空の
/// Track Properties として取得できる
///
/// draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter): 0 を指定すると
/// OK 応答の Track Properties は存在するが空になる (SHOULD)。
#[test]
fn subscribe_ok_with_include_properties_zero_yields_empty_peer_track_properties() {
    let (mut subscriber, mut publisher) = establish_pair();
    // subscriber が INCLUDE_PROPERTIES=0 を要求する
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_INCLUDE_PROPERTIES,
        value: MessageParameterValue::Uint8(0),
    });
    let rid = subscriber
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), params)
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, subscribe_msg) = take_send_request(&mut subscriber);
    publisher
        .recv_request(subscribe_msg)
        .expect("SUBSCRIBE の受信に成功すること");

    // publisher は LOC プロパティを渡すが、send_subscribe_ok が wire 上で空化する
    // (wire での空化そのものは tests/test_session/include_properties.rs が検証する)。
    publisher
        .send_subscribe_ok(rid, 700, MessageParameters::new(), loc_track_properties())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, subscribe_ok_msg) = take_send_on_stream(&mut publisher);
    subscriber
        .recv_stream_message(rid, roundtrip_wire(subscribe_ok_msg))
        .expect("SUBSCRIBE_OK の受信に成功すること");

    // peer が送らなかった場合と空化された場合のどちらも空の TrackProperties として観測され、
    // panic しないこと。application は include_properties が Some(0) かどうかで空化要求の
    // 有無を判断できる。
    let sub = subscriber
        .subscription(rid)
        .expect("SUBSCRIBE_OK を受信した subscription が存在すること");
    assert_eq!(
        sub.include_properties,
        Some(0),
        "INCLUDE_PROPERTIES=0 の要求値が保持されていること"
    );
    assert!(
        sub.peer_track_properties.is_empty(),
        "INCLUDE_PROPERTIES=0 で空化された Track Properties は空のまま保持されること"
    );
    assert_eq!(
        sub.peer_track_properties.find_bytes(PROP_VIDEO_CONFIG),
        None,
        "空化された Track Properties から VIDEO_CONFIG は取得できないこと"
    );
    assert_eq!(
        sub.peer_track_properties.find_varint(PROP_TIMESCALE),
        None,
        "空化された Track Properties から TIMESCALE は取得できないこと"
    );
}

// ─── 自側が送る側・受信前の subscription ──────────────────

/// 自側が publisher / subscriber として構築した subscription では peer の
/// Track Properties が空であること
///
/// `send_publish` / `send_subscribe_ok` が持つ自側の発行値と、peer が送った値を混同しない
/// ことを固定する。
#[test]
fn local_side_subscriptions_have_empty_peer_track_properties() {
    let (mut client, mut server) = establish_pair();

    // 自側が publisher として PUBLISH を送った直後。引数の Track Properties は自側の
    // 発行値であり、peer が送った値ではないため空のままである。
    let publish_rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            700,
            MessageParameters::new(),
            loc_track_properties(),
        )
        .expect("PUBLISH の送信に成功すること");
    assert!(
        client
            .subscription(publish_rid)
            .expect("送信した PUBLISH の subscription が存在すること")
            .peer_track_properties
            .is_empty(),
        "send_publish 直後の peer_track_properties は空であること"
    );
    let (_, publish_msg) = take_send_request(&mut client);

    // 対照: その PUBLISH を受けた subscriber 役の subscription は値を保持する。
    server
        .recv_request(publish_msg)
        .expect("PUBLISH の受信に成功すること");
    assert!(
        !server
            .subscription(publish_rid)
            .expect("受信した PUBLISH の subscription が存在すること")
            .peer_track_properties
            .is_empty(),
        "PUBLISH を受けた側は peer の Track Properties を保持すること"
    );

    // 自側が subscriber として SUBSCRIBE を送った直後。SUBSCRIBE_OK をまだ受け取って
    // いないため空である。
    let subscribe_rid = client
        .send_subscribe(ns(&[b"mic"]), b"talk".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    assert!(
        client
            .subscription(subscribe_rid)
            .expect("送信した SUBSCRIBE の subscription が存在すること")
            .peer_track_properties
            .is_empty(),
        "send_subscribe 直後の peer_track_properties は空であること"
    );
    let (_, subscribe_msg) = take_send_request(&mut client);

    // peer の SUBSCRIBE を受けて publisher 役になった直後も、peer から Track Properties を
    // 受け取っていないため空である。
    server
        .recv_request(subscribe_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    assert!(
        server
            .subscription(subscribe_rid)
            .expect("受信した SUBSCRIBE の subscription が存在すること")
            .peer_track_properties
            .is_empty(),
        "SUBSCRIBE を受けた publisher 役の peer_track_properties は空であること"
    );

    // 自側が publisher として SUBSCRIBE_OK を送った直後。引数の Track Properties は自側の
    // 発行値であり、peer が送った値ではないため空のままである。
    server
        .send_subscribe_ok(
            subscribe_rid,
            701,
            MessageParameters::new(),
            loc_track_properties(),
        )
        .expect("SUBSCRIBE_OK の送信に成功すること");
    assert!(
        server
            .subscription(subscribe_rid)
            .expect("SUBSCRIBE_OK を送った subscription が存在すること")
            .peer_track_properties
            .is_empty(),
        "send_subscribe_ok 直後の peer_track_properties は空であること"
    );
    let (_, subscribe_ok_msg) = take_send_on_stream(&mut server);

    // 対照: その SUBSCRIBE_OK を受けた subscriber 役の subscription は値を保持する。
    client
        .recv_stream_message(subscribe_rid, subscribe_ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");
    assert!(
        !client
            .subscription(subscribe_rid)
            .expect("受信した SUBSCRIBE_OK の subscription が存在すること")
            .peer_track_properties
            .is_empty(),
        "SUBSCRIBE_OK を受けた側は peer の Track Properties を保持すること"
    );
}

// ─── 既知 MOQT 値の回帰 ───────────────────────────────────

/// LOC プロパティと併せて PUBLISH で送られた既知 MOQT 値の取り込みが変わらないこと
#[test]
fn publish_keeps_known_moq_values_with_loc_properties() {
    let (subscriber, rid) = recv_peer_publish(track_properties_with_known_moq_and_loc_values());
    let sub = subscriber
        .subscription(rid)
        .expect("PUBLISH を受信した subscription が存在すること");

    assert_known_moq_values(sub);
    // LOC プロパティの取得と既知 MOQT 値の取り込みが両立すること。
    assert_loc_track_properties(sub);
}

/// LOC プロパティと併せて SUBSCRIBE_OK で送られた既知 MOQT 値の取り込みが変わらないこと
#[test]
fn subscribe_ok_keeps_known_moq_values_with_loc_properties() {
    let (subscriber, rid) =
        recv_peer_subscribe_ok(track_properties_with_known_moq_and_loc_values());
    let sub = subscriber
        .subscription(rid)
        .expect("SUBSCRIBE_OK を受信した subscription が存在すること");

    assert_known_moq_values(sub);
    assert_loc_track_properties(sub);
}
