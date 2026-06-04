//! SUBSCRIBE_TRACKS の TRACK_PROPERTY_FILTER による PUBLISH 選別テスト
//!
//! draft-ietf-moq-transport-21 §3.3.2 (Range Filters):
//! "The Track Property Filter can be used in SUBSCRIBE_TRACKS to filter PUBLISH messages with
//! required Track Property types and values. PUBLISH messages which pass the filter will be
//! forwarded while those which do not pass it will not be forwarded nor will any Objects."
//!
//! フィルタは subscriber が SUBSCRIBE_TRACKS で指定し、publisher が PUBLISH 送信前に評価する。
//! 本テストは client(subscriber) → server(publisher) の SUBSCRIBE_TRACKS で条件を作り、
//! server の `send_publish` が選別することを断言する。
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use super::*;
use shiguredo_moqt::error::SESSION_LOCAL_FILTER_MISMATCH;
use shiguredo_moqt::message_parameter::PARAM_TRACK_PROPERTY_FILTER;
use shiguredo_moqt::track_properties::{
    PROP_MAX_CACHE_DURATION, TrackProperty, TrackPropertyValue,
};

/// Property Type 付きの TRACK_PROPERTY_FILTER パラメータを作る
fn track_property_filter(set_id: u8, property_type: u64, start: u64, end: u64) -> MessageParameter {
    let mut bytes = vec![set_id];
    shiguredo_moqt::varint::encode(property_type, &mut bytes);
    shiguredo_moqt::varint::encode(start, &mut bytes);
    shiguredo_moqt::varint::encode(end - start, &mut bytes);
    MessageParameter {
        param_type: PARAM_TRACK_PROPERTY_FILTER,
        value: MessageParameterValue::LengthPrefixed(bytes),
    }
}

/// Length=0 の TRACK_PROPERTY_FILTER (フィルタ削除指示)
fn track_property_filter_removal() -> MessageParameter {
    MessageParameter {
        param_type: PARAM_TRACK_PROPERTY_FILTER,
        value: MessageParameterValue::LengthPrefixed(Vec::new()),
    }
}

/// MAX_CACHE_DURATION を持つ TrackProperties
fn props_with_cache_duration(value: u64) -> TrackProperties {
    let mut props = TrackProperties::new();
    props.push(TrackProperty {
        prop_type: PROP_MAX_CACHE_DURATION,
        value: TrackPropertyValue::VarInt(value),
    });
    props
}

/// SUBSCRIBE_TRACKS を確立して (client, server, request_id) を返す
///
/// Range Filter を送るには peer (server) 側の MAX_FILTER_RANGES 宣言が必要 (§10.3.1.6)。
fn establish_subscribe_tracks(params: MessageParameters) -> (Session, Session, u64) {
    let mut server_opts = SetupOptions::new();
    server_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_FILTER_RANGES, // MAX_FILTER_RANGES
        value: SetupOptionValue::VarInt(8),
    });
    let (mut client, mut server) = establish_pair_with_options(SetupOptions::new(), server_opts);
    let rid = client
        .send_subscribe_tracks(ns(&[b"live"]), params)
        .expect("SUBSCRIBE_TRACKS の送信に成功すること");
    let (_, st_msg) = take_send_request(&mut client);
    server
        .recv_request(st_msg)
        .expect("SUBSCRIBE_TRACKS の受信に成功すること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("REQUEST_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("REQUEST_OK の受信に成功すること");
    (client, server, rid)
}

/// prefix 配下の Track を PUBLISH する
///
/// draft-ietf-moq-transport-21 §3.1.2 (Track Alias) により異なる Track へ同じ alias は使えないので、
/// Track 名から決定的に alias を導出する。
fn try_publish(
    server: &mut Session,
    track_name: &[u8],
    track_properties: TrackProperties,
) -> Result<u64, shiguredo_moqt::session::types::SendRequestError> {
    let track_alias = u64::from(track_name[0]);
    server.send_publish(
        ns(&[b"live", b"room1"]),
        track_name.to_vec(),
        track_alias,
        MessageParameters::new(),
        track_properties,
    )
}

/// TRACK_PROPERTY_FILTER が TrackSubscription 状態に保持される
#[test]
fn track_property_filter_is_stored_on_both_sides() {
    let mut params = MessageParameters::new();
    params.push(track_property_filter(0, PROP_MAX_CACHE_DURATION, 100, 200));
    let (client, server, rid) = establish_subscribe_tracks(params);

    for (label, session) in [("subscriber", &client), ("publisher", &server)] {
        let ts = session
            .track_subscription(rid)
            .unwrap_or_else(|| panic!("{label}: track subscription が存在すること"));
        assert_eq!(
            ts.track_property_filters.len(),
            1,
            "{label}: フィルタが保持されること"
        );
        let set = &ts.track_property_filters[0];
        assert_eq!(set.set_id, 0, "{label}");
        assert_eq!(set.property_type, Some(PROP_MAX_CACHE_DURATION), "{label}");
        assert_eq!(set.ranges, vec![(100, Some(200))], "{label}");
    }
}

/// フィルタを通る Track Properties の PUBLISH は送れる
#[test]
fn passing_track_properties_can_be_published() {
    let mut params = MessageParameters::new();
    params.push(track_property_filter(0, PROP_MAX_CACHE_DURATION, 100, 200));
    let (_client, mut server, _rid) = establish_subscribe_tracks(params);

    try_publish(&mut server, b"cam", props_with_cache_duration(150))
        .expect("範囲内の Track Property なら PUBLISH できる");
}

/// フィルタを通らない Track Properties の PUBLISH は抑止される
#[test]
fn failing_track_properties_are_suppressed() {
    let mut params = MessageParameters::new();
    params.push(track_property_filter(0, PROP_MAX_CACHE_DURATION, 100, 200));
    let (_client, mut server, _rid) = establish_subscribe_tracks(params);

    let err = try_publish(&mut server, b"cam", props_with_cache_duration(500))
        .expect_err("範囲外の Track Property は抑止される");
    let err = err.as_session_error().expect("SessionError が得られること");
    assert_eq!(err.code, SESSION_LOCAL_FILTER_MISMATCH);
    assert_eq!(
        server.state(),
        SessionState::Established,
        "抑止でセッションが閉じてはいけない"
    );
}

/// 指定 Property Type を持たない Track も抑止される
///
/// フィルタは「required Track Property types and values」なので、Property が無い Track は
/// 条件を満たせない。
#[test]
fn track_without_required_property_is_suppressed() {
    let mut params = MessageParameters::new();
    params.push(track_property_filter(0, PROP_MAX_CACHE_DURATION, 100, 200));
    let (_client, mut server, _rid) = establish_subscribe_tracks(params);

    let err = try_publish(&mut server, b"cam", TrackProperties::new())
        .expect_err("必須 Property が無い Track は抑止される");
    assert_eq!(
        err.as_session_error()
            .expect("SessionError が得られること")
            .code,
        SESSION_LOCAL_FILTER_MISMATCH
    );
}

/// フィルタ省略時は従来どおり PUBLISH できる
#[test]
fn no_filter_allows_any_track() {
    let (_client, mut server, _rid) = establish_subscribe_tracks(MessageParameters::new());

    try_publish(&mut server, b"cam", TrackProperties::new())
        .expect("フィルタが無ければ任意の Track を PUBLISH できる");
    try_publish(&mut server, b"mic", props_with_cache_duration(9999))
        .expect("フィルタが無ければ任意の Track を PUBLISH できる");
}

/// prefix が一致しない SUBSCRIBE_TRACKS のフィルタは適用されない
#[test]
fn filter_does_not_apply_outside_prefix() {
    let mut params = MessageParameters::new();
    params.push(track_property_filter(0, PROP_MAX_CACHE_DURATION, 100, 200));
    let (_client, mut server, _rid) = establish_subscribe_tracks(params);

    // prefix (live) の外にある Track は選別対象外
    server
        .send_publish(
            ns(&[b"vod", b"room1"]),
            b"cam".to_vec(),
            2,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("prefix 外の Track にはフィルタが適用されない");
}

/// REQUEST_UPDATE でフィルタが更新される
#[test]
fn request_update_replaces_filter() {
    let mut params = MessageParameters::new();
    params.push(track_property_filter(0, PROP_MAX_CACHE_DURATION, 100, 200));
    let (mut client, mut server, rid) = establish_subscribe_tracks(params);

    // 更新前は 500 が通らない
    try_publish(&mut server, b"cam", props_with_cache_duration(500)).expect_err("更新前は範囲外");

    // 400..=600 に置き換える
    let mut upd = MessageParameters::new();
    upd.push(track_property_filter(0, PROP_MAX_CACHE_DURATION, 400, 600));
    client
        .send_request_update(rid, upd)
        .expect("REQUEST_UPDATE の送信に成功すること");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("REQUEST_UPDATE の受信に成功すること");

    let ts = server
        .track_subscription(rid)
        .expect("track subscription が存在する");
    assert_eq!(
        ts.track_property_filters[0].ranges,
        vec![(400, Some(600))],
        "フィルタが全置換されること"
    );
    try_publish(&mut server, b"cam", props_with_cache_duration(500))
        .expect("更新後は範囲内なので PUBLISH できる");
    try_publish(&mut server, b"mic", props_with_cache_duration(150))
        .expect_err("更新後は旧範囲が通らない");
}

/// REQUEST_UPDATE の Length=0 でフィルタが削除され PUBLISH が再び通る
///
/// draft-ietf-moq-transport-21 §3.3.2 (Range Filters): "In REQUEST_UPDATE, Length of 0
/// removes the filter"
#[test]
fn request_update_length_zero_removes_filter() {
    let mut params = MessageParameters::new();
    params.push(track_property_filter(0, PROP_MAX_CACHE_DURATION, 100, 200));
    let (mut client, mut server, rid) = establish_subscribe_tracks(params);

    try_publish(&mut server, b"cam", TrackProperties::new())
        .expect_err("削除前は必須 Property が無いので抑止される");

    let mut upd = MessageParameters::new();
    upd.push(track_property_filter_removal());
    client
        .send_request_update(rid, upd)
        .expect("REQUEST_UPDATE の送信に成功すること");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("REQUEST_UPDATE の受信に成功すること");

    assert!(
        server
            .track_subscription(rid)
            .expect("track subscription が存在する")
            .track_property_filters
            .is_empty(),
        "Length=0 でフィルタが削除されること"
    );
    try_publish(&mut server, b"cam", TrackProperties::new())
        .expect("削除後は任意の Track を PUBLISH できる");
}

/// REQUEST_UPDATE にフィルタが含まれなければ既存フィルタは維持される
///
/// draft-ietf-moq-transport-21 §3.3.2 (Range Filters): "If a filter parameter is omitted from
/// REQUEST_UPDATE, it is unchanged."
#[test]
fn request_update_without_filter_keeps_existing() {
    let mut params = MessageParameters::new();
    params.push(track_property_filter(0, PROP_MAX_CACHE_DURATION, 100, 200));
    let (mut client, mut server, rid) = establish_subscribe_tracks(params);

    client
        .send_request_update(rid, MessageParameters::new())
        .expect("空の REQUEST_UPDATE の送信に成功すること");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("REQUEST_UPDATE の受信に成功すること");

    assert_eq!(
        server
            .track_subscription(rid)
            .expect("track subscription が存在する")
            .track_property_filters[0]
            .ranges,
        vec![(100, Some(200))],
        "省略時はフィルタが変わらないこと"
    );
    try_publish(&mut server, b"cam", props_with_cache_duration(150))
        .expect("既存フィルタのまま範囲内は通る");
}

/// SetID ごとの AND と SetID 間の OR が効く
#[test]
fn track_property_filter_and_or_semantics() {
    // SetID=0: MAX_CACHE_DURATION 100..=200
    // SetID=1: MAX_CACHE_DURATION 900..=1000
    let mut params = MessageParameters::new();
    params.push(track_property_filter(0, PROP_MAX_CACHE_DURATION, 100, 200));
    params.push(track_property_filter(1, PROP_MAX_CACHE_DURATION, 900, 1000));
    let (_client, mut server, _rid) = establish_subscribe_tracks(params);

    try_publish(&mut server, b"a", props_with_cache_duration(150))
        .expect("SetID=0 を満たすので通る");
    try_publish(&mut server, b"b", props_with_cache_duration(950))
        .expect("SetID=1 を満たすので通る");
    try_publish(&mut server, b"c", props_with_cache_duration(500))
        .expect_err("どの SetID も満たさないので抑止される");
}
