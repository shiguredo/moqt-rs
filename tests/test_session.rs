//! Session 状態機械の単体テスト
//!
//! 本ファイルには integration test 共有の import / helper を置き、
//! 実際の公開 API 検証は `test_session/` 配下の責務別サブモジュールへ分割する。

use shiguredo_moqt::error::{
    SESSION_DUPLICATE_TRACK_ALIAS, SESSION_INVALID_AUTHORITY, SESSION_INVALID_PATH,
    SESSION_INVALID_REQUEST_ID, SESSION_MALFORMED_AUTHORITY, SESSION_MALFORMED_PATH,
    SESSION_NO_ERROR, SESSION_PROTOCOL_VIOLATION, STREAM_DELIVERY_TIMEOUT, STREAM_INTERNAL_ERROR,
};
use shiguredo_moqt::loc::{LocProperties, LocProperty, LocPropertyValue, PROP_TIMESTAMP};
use shiguredo_moqt::message::{ControlMessage, Goaway, Setup, common::TrackNamespace};
use shiguredo_moqt::message_parameter::{
    AuthorizationToken, MessageParameter, MessageParameterValue, MessageParameters, PARAM_EXPIRES,
    PARAM_OBJECT_DELIVERY_TIMEOUT, PARAM_OBJECT_PROPERTY_FILTER, PARAM_SUBGROUP_DELIVERY_TIMEOUT,
    PARAM_SUBGROUP_FILTER,
};
use shiguredo_moqt::object_properties::{
    ObjectProperties, ObjectProperty, ObjectPropertyValue, PROP_PRIOR_OBJECT_ID_GAP,
};
use shiguredo_moqt::parameter::{SetupOption, SetupOptionValue, SetupOptions};
use shiguredo_moqt::session::{
    core::PUBLISH_DONE_STREAM_COUNT_UNKNOWN,
    types::{
        DataStreamResetReason, DatagramAcceptance, FetchState, RequestKind, SubscriptionState,
        TerminationReason, TrackDataAcceptance, TrackRole,
    },
};
use shiguredo_moqt::stream::{FETCH_HEADER_TYPE, subgroup::SubgroupIdMode};
use shiguredo_moqt::track_properties::{PROP_OBJECT_DELIVERY_TIMEOUT, TrackProperties};
use shiguredo_moqt::{
    message::common::Location, session::core::Session, session::types::DataStreamId,
    session::types::RequestStreamEnd, session::types::SessionEvent, session::types::SessionState,
    session::types::Transport, stream::datagram::ObjectDatagram,
    stream::decoder::DecodedSubgroupObject, stream::fetch::FetchHeader,
    stream::subgroup::SubgroupHeader,
};

/// 発行されたイベント列から最初の SendControl を取り出す
fn take_send_control(s: &mut Session) -> ControlMessage {
    while let Some(e) = s.poll_event() {
        if let SessionEvent::SendControl(msg) = e {
            return msg;
        }
    }
    panic!("SendControl イベントが期待されたが発行されなかった")
}

/// 発行されたイベント列から最初の CloseSession を取り出す (pred を経由することで該当 event を検証可能)
fn drain_until_close(s: &mut Session) -> SessionEvent {
    while let Some(e) = s.poll_event() {
        if matches!(e, SessionEvent::CloseSession(_)) {
            return e;
        }
    }
    panic!("CloseSession イベントが期待されたが発行されなかった")
}

fn establish_client() -> Session {
    let mut client = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_send_control(&mut client);
    client
        .recv_control(ControlMessage::Setup(Setup {
            options: SetupOptions::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    client
}

/// Client / Server 両側を Established にして返す
fn establish_pair() -> (Session, Session) {
    let mut client = Session::new_client(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let mut server = Session::new_server(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let c_setup = take_send_control(&mut client);
    let s_setup = take_send_control(&mut server);
    server
        .recv_control(c_setup)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(s_setup)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server)
}

/// 1 個の SETUP Option (VarInt 値) だけを持つ `SetupOptions` を作る
fn opts_with(option_type: u64, value: u64) -> SetupOptions {
    let mut opts = SetupOptions::new();
    opts.push(SetupOption {
        option_type,
        value: SetupOptionValue::VarInt(value),
    });
    opts
}

/// 全 request テーブルが空であることを検証する
///
/// `request_stream_count() == 0` の公開 API によらない代替。3 種の request テーブル
/// (subscription / fetch / track status) を全走査する。
fn assert_no_tracked_requests(s: &Session) {
    assert!(
        s.subscriptions().next().is_none(),
        "subscription テーブルが空であること"
    );
    assert!(s.fetches().next().is_none(), "fetch テーブルが空であること");
    assert!(
        s.track_status_requests().next().is_none(),
        "track status テーブルが空であること"
    );
}

/// Client / Server 両側を指定の SetupOptions で Established にして返す
fn establish_pair_with_options(
    client_opts: SetupOptions,
    server_opts: SetupOptions,
) -> (Session, Session) {
    let mut client = Session::new_client(Transport::WebTransport, client_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut server = Session::new_server(Transport::WebTransport, server_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    let c_setup = take_send_control(&mut client);
    let s_setup = take_send_control(&mut server);
    server
        .recv_control(c_setup)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(s_setup)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server)
}

/// `peer_auth_token_cache.max_size` を自側 `MAX_AUTH_TOKEN_CACHE_SIZE` で指定して
/// セッションを確立するヘルパ (Client / Server どちらも同じ値を送る)
fn establish_pair_with_self_auth_cache(self_max: u64) -> (Session, Session) {
    let mut client_opts = SetupOptions::new();
    client_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE, // MAX_AUTH_TOKEN_CACHE_SIZE
        value: SetupOptionValue::VarInt(self_max),
    });
    let mut server_opts = SetupOptions::new();
    server_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE,
        value: SetupOptionValue::VarInt(self_max),
    });
    establish_pair_with_options(client_opts, server_opts)
}

fn take_send_request(s: &mut Session) -> (u64, ControlMessage) {
    while let Some(e) = s.poll_event() {
        if let SessionEvent::SendRequest {
            request_id,
            message,
        } = e
        {
            return (request_id, message);
        }
    }
    panic!("SendRequest イベントが期待されたが発行されなかった");
}

fn take_send_on_stream(s: &mut Session) -> (u64, ControlMessage) {
    let (request_id, message, _) = take_send_on_stream_with_fin(s);
    (request_id, message)
}

/// `SendOnStream` イベントの `fin` も含めて取り出す
fn take_send_on_stream_with_fin(s: &mut Session) -> (u64, ControlMessage, bool) {
    while let Some(e) = s.poll_event() {
        if let SessionEvent::SendOnStream {
            request_id,
            message,
            fin,
        } = e
        {
            return (request_id, message, fin);
        }
    }
    panic!("SendOnStream イベントが期待されたが発行されなかった");
}

/// `ControlMessage::RequestUpdate` の Request ID を取り出す
///
/// draft-ietf-moq-transport-21 §6.4.2.1 (Request ID): REQUEST_UPDATE は購読とは別に
/// 新しい Request ID を消費する。この値は `SendOnStream` の request_id (送信先 bidi
/// request stream の識別子) とは異なる。
fn request_update_request_id(message: &ControlMessage) -> u64 {
    match message {
        ControlMessage::RequestUpdate(update) => update.request_id,
        other => panic!("REQUEST_UPDATE が期待されたが {other:?} だった"),
    }
}

/// `send_goaway` が control stream に積む `Goaway` を取り出す
///
/// `take_send_control` は `SendControl` 以外のイベント (受信通知や終端通知) を読み飛ばし、
/// 最初の `SendControl` を返す。GOAWAY は `SendControl(Goaway)` として積まれる。
fn take_goaway(s: &mut Session) -> Goaway {
    match take_send_control(s) {
        ControlMessage::Goaway(goaway) => goaway,
        other => panic!("Goaway が期待されたが {other:?} を受け取った"),
    }
}

fn ns(parts: &[&[u8]]) -> TrackNamespace {
    TrackNamespace::new(parts.iter().map(|p| p.to_vec()).collect())
        .expect("テストフィクスチャの前提条件を満たす")
}

/// FETCH の range 指定 (AbsoluteRangeWithEnd) を持つ MessageParameters を作る
///
/// draft-ietf-moq-transport-21 §9.11 (FETCH): range は LOCATION_FILTER
/// パラメータで指定する。`end.group_id < start.group_id` の場合は delta が
/// 求まらないため panic する (テストフィクスチャの前提)。
fn fetch_range_params(start: Location, end: Location) -> MessageParameters {
    use shiguredo_moqt::message_parameter::LocationFilter;
    use shiguredo_moqt::message_parameter::PARAM_LOCATION_FILTER;
    let end_group_delta = end.group_id - start.group_id;
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteRangeWithEnd {
                start,
                end_group_delta,
                end_object: end.object_id,
            }
            .encode_to_bytes(),
        ),
    });
    params
}

fn establish_subscribe_track(alias: u64) -> (Session, Session, u64) {
    establish_subscribe_track_with_params(alias, MessageParameters::new())
}

/// SUBSCRIBE / SUBSCRIBE_OK までを確立し、subscriber・publisher・request_id を返す
///
/// SUBSCRIBE に載せるパラメータを指定したいテスト (delivery timeout 等) は本関数を使う。
fn establish_subscribe_track_with_params(
    alias: u64,
    params: MessageParameters,
) -> (Session, Session, u64) {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, alias, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server, rid)
}

fn delivery_timeout_params(delivery_timeout_ms: u64) -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_OBJECT_DELIVERY_TIMEOUT,
        value: MessageParameterValue::VarInt(delivery_timeout_ms),
    });
    params
}

fn expires_params(expires_ms: u64) -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(expires_ms),
    });
    params
}

/// DYNAMIC_GROUPS=1 の Track を発行する (draft-ietf-moq-transport-21 §10.6 (DYNAMIC GROUPS))
fn track_properties_with_dynamic_groups() -> TrackProperties {
    use shiguredo_moqt::track_properties::{
        PROP_DYNAMIC_GROUPS, TrackProperty, TrackPropertyValue,
    };
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: PROP_DYNAMIC_GROUPS,
        value: TrackPropertyValue::VarInt(1),
    });
    tp
}

fn track_properties_with_delivery_timeout(delivery_timeout_ms: u64) -> TrackProperties {
    use shiguredo_moqt::track_properties::{
        PROP_OBJECT_DELIVERY_TIMEOUT, TrackProperty, TrackPropertyValue,
    };
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(delivery_timeout_ms),
    });
    tp
}

fn subgroup_delivery_timeout_params(delivery_timeout_ms: u64) -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_SUBGROUP_DELIVERY_TIMEOUT,
        value: MessageParameterValue::VarInt(delivery_timeout_ms),
    });
    params
}

fn track_properties_with_subgroup_delivery_timeout(delivery_timeout_ms: u64) -> TrackProperties {
    use shiguredo_moqt::track_properties::{
        PROP_SUBGROUP_DELIVERY_TIMEOUT, TrackProperty, TrackPropertyValue,
    };
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: PROP_SUBGROUP_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(delivery_timeout_ms),
    });
    tp
}

/// MessageParameters に AUTHORIZATION_TOKEN (REGISTER) を 1 件追加する
fn params_with_register(alias: u64, token_type: u64, token_value: &[u8]) -> MessageParameters {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_AUTHORIZATION_TOKEN,
    };
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_AUTHORIZATION_TOKEN,
        value: MessageParameterValue::AuthorizationToken(AuthorizationToken::Register {
            alias,
            token_type,
            token_value: token_value.to_vec(),
        }),
    });
    params
}

/// MessageParameters に AUTHORIZATION_TOKEN (DELETE) を追加する
fn params_with_delete(alias: u64) -> MessageParameters {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_AUTHORIZATION_TOKEN,
    };
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_AUTHORIZATION_TOKEN,
        value: MessageParameterValue::AuthorizationToken(AuthorizationToken::Delete { alias }),
    });
    params
}

/// MessageParameters に AUTHORIZATION_TOKEN (USE_ALIAS) を追加する
fn params_with_use_alias(alias: u64) -> MessageParameters {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_AUTHORIZATION_TOKEN,
    };
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_AUTHORIZATION_TOKEN,
        value: MessageParameterValue::AuthorizationToken(AuthorizationToken::UseAlias { alias }),
    });
    params
}

/// 1 つの Range を持つ Range Filter パラメータを作る (SetID, Start, End)
fn range_filter(param_type: u64, set_id: u8, start: u64, end: u64) -> MessageParameter {
    let mut bytes = vec![set_id];
    shiguredo_moqt::varint::encode(start, &mut bytes);
    shiguredo_moqt::varint::encode(end - start, &mut bytes);
    MessageParameter {
        param_type,
        value: MessageParameterValue::LengthPrefixed(bytes),
    }
}

/// Range 1 個の SUBGROUP_FILTER を SetID 指定で作る (SetID, Start=0, End_delta=1)
fn one_range_subgroup_filter_with_set_id(set_id: u8) -> MessageParameter {
    MessageParameter {
        param_type: PARAM_SUBGROUP_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![set_id, 0x00, 0x01]),
    }
}

/// MAX_FILTER_RANGES を宣言する server 用 SetupOptions を作る
///
/// Range Filter を送るには peer (server) 側の宣言が必要
/// (draft-ietf-moq-transport-21 §9.1.6 (MAX FILTER RANGES))。
fn max_filter_options() -> SetupOptions {
    let mut opts = SetupOptions::new();
    opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_FILTER_RANGES,
        value: SetupOptionValue::VarInt(8),
    });
    opts
}

/// SUBSCRIBE を 1 本送って受信側に届け request_id を返す (SUBSCRIBE のパラメータなし)
fn send_and_recv_subscribe(
    client: &mut Session,
    server: &mut Session,
    namespace: &[&[u8]],
    track_name: &[u8],
) -> u64 {
    send_and_recv_subscribe_with_parameters(
        client,
        server,
        namespace,
        track_name,
        MessageParameters::new(),
    )
}

/// SUBSCRIBE を 1 本送って受信側に届け request_id を返す
///
/// namespace / track name と SUBSCRIBE に載せるパラメータを指定できる。
fn send_and_recv_subscribe_with_parameters(
    client: &mut Session,
    server: &mut Session,
    namespace: &[&[u8]],
    track_name: &[u8],
    parameters: MessageParameters,
) -> u64 {
    let rid = client
        .send_subscribe(ns(namespace), track_name.to_vec(), parameters)
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    rid
}

/// SUBSCRIBE_OK を返して両側で Established にする
fn complete_subscribe(client: &mut Session, server: &mut Session, rid: u64, alias: u64) {
    server
        .send_subscribe_ok(rid, alias, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");
}

/// 同一 alias を共有する 2 subscription を確立して (client, server, rid1, rid2) を返す
///
/// PUBLISH_DONE の送信を検証に使うテストは server 側の操作が必要になるため server も返す。
/// candidate の評価順は登録順 (rid1 → rid2) になる。
fn establish_shared_alias_pair(
    alias: u64,
    parameters1: MessageParameters,
    parameters2: MessageParameters,
) -> (Session, Session, u64, u64) {
    let (mut client, mut server) =
        establish_pair_with_options(SetupOptions::new(), max_filter_options());
    let rid1 = send_and_recv_subscribe_with_parameters(
        &mut client,
        &mut server,
        &[b"live"],
        b"cam",
        parameters1,
    );
    let rid2 = send_and_recv_subscribe_with_parameters(
        &mut client,
        &mut server,
        &[b"live"],
        b"cam",
        parameters2,
    );
    complete_subscribe(&mut client, &mut server, rid1, alias);
    complete_subscribe(&mut client, &mut server, rid2, alias);
    (client, server, rid1, rid2)
}

/// 同一 alias を共有する 2 subscription を確立して (client, rid1, rid2) を返す
///
/// 候補の評価順は登録順 (rid1 → rid2) になる。
fn establish_shared_alias(
    alias: u64,
    parameters1: MessageParameters,
    parameters2: MessageParameters,
) -> (Session, u64, u64) {
    let (client, _server, rid1, rid2) =
        establish_shared_alias_pair(alias, parameters1, parameters2);
    (client, rid1, rid2)
}

/// フィルタ付き SUBSCRIBE を 1 本確立して (client, rid) を返す
fn establish_filtered_subscription(alias: u64, parameters: MessageParameters) -> (Session, u64) {
    let (mut client, mut server) =
        establish_pair_with_options(SetupOptions::new(), max_filter_options());
    let rid = send_and_recv_subscribe_with_parameters(
        &mut client,
        &mut server,
        &[b"live"],
        b"cam",
        parameters,
    );
    complete_subscribe(&mut client, &mut server, rid, alias);
    (client, rid)
}

/// 通常 Object (payload あり / properties なし) を作る
fn normal_object(object_id: u64) -> DecodedSubgroupObject {
    DecodedSubgroupObject {
        object_id,
        payload_length: 1,
        status: None,
        properties_bytes: None,
    }
}

/// Object Properties 付きの通常 Object を作る
fn object_with_properties(object_id: u64, properties_bytes: Vec<u8>) -> DecodedSubgroupObject {
    DecodedSubgroupObject {
        object_id,
        payload_length: 1,
        status: None,
        properties_bytes: Some(properties_bytes),
    }
}

/// Object Status のみ (payload なし) の Object を作る
fn status_object(object_id: u64, status: u64) -> DecodedSubgroupObject {
    DecodedSubgroupObject {
        object_id,
        payload_length: 0,
        status: Some(status),
        properties_bytes: None,
    }
}

/// Malformed Track の cancel / 終端イベントを回収して検証する
///
/// 返り値は (cancel イベント列, ResetDataStream 件数, RequestTerminated(MalformedTrack) 件数)。
/// cancel は `request_id` と `STREAM_MALFORMED_TRACK` も検証する。`expected_reset_stream_id` は
/// `ResetDataStream` の対象 stream (datagram 経路は `None`)。CloseSession はテストの前提に
/// 反するため panic する。
fn drain_malformed_events(
    session: &mut Session,
    request_id: u64,
    expected_reset_stream_id: Option<DataStreamId>,
) -> (Vec<&'static str>, usize, usize) {
    use shiguredo_moqt::error::STREAM_MALFORMED_TRACK;
    let mut cancels = Vec::new();
    let mut reset_data_streams = 0;
    let mut malformed_terminations = 0;
    while let Some(e) = session.poll_event() {
        match e {
            SessionEvent::StopSendingRequestStream {
                request_id: rid,
                error_code,
            } => {
                assert_eq!(rid, request_id, "cancel の対象 request id が一致すること");
                assert_eq!(
                    error_code, STREAM_MALFORMED_TRACK,
                    "cancel の error code が STREAM_MALFORMED_TRACK (0x12) であること"
                );
                cancels.push("stop_sending");
            }
            SessionEvent::ResetRequestStream {
                request_id: rid,
                error_code,
            } => {
                assert_eq!(rid, request_id, "cancel の対象 request id が一致すること");
                assert_eq!(
                    error_code, STREAM_MALFORMED_TRACK,
                    "cancel の error code が STREAM_MALFORMED_TRACK (0x12) であること"
                );
                cancels.push("reset");
            }
            SessionEvent::ResetDataStream { stream_id, .. } => {
                assert_eq!(
                    Some(stream_id),
                    expected_reset_stream_id,
                    "ResetDataStream の対象 stream_id が期待値と一致すること"
                );
                reset_data_streams += 1;
            }
            SessionEvent::RequestTerminated {
                request_id: rid,
                reason: TerminationReason::MalformedTrack { .. },
                ..
            } => {
                assert_eq!(rid, request_id, "終端対象 request id が一致すること");
                malformed_terminations += 1;
            }
            SessionEvent::CloseSession(err) => {
                panic!("CloseSession が発行された: {err:?}");
            }
            _ => {}
        }
    }
    (cancels, reset_data_streams, malformed_terminations)
}

/// OBJECT_DELIVERY_TIMEOUT (0x02) を 1 つ持つ Object Properties のバイト列を作る
///
/// `ObjectPropertyTracker` の gap 検証対象外の Property を使い、
/// OBJECT_PROPERTY_FILTER の評価だけを検証できるようにする。
fn delivery_timeout_properties(value: u64) -> Vec<u8> {
    encode_properties([ObjectProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: ObjectPropertyValue::VarInt(value),
    }])
}

/// Object Property 列を Object Properties のバイト列に encode する
fn encode_properties(properties: impl IntoIterator<Item = ObjectProperty>) -> Vec<u8> {
    let mut encoded = ObjectProperties::new();
    for property in properties {
        encoded.push(property);
    }
    let mut bytes = Vec::new();
    encoded
        .encode(&mut bytes)
        .expect("Object Properties の encode に成功すること");
    bytes
}

/// Property Type 付きの OBJECT_PROPERTY_FILTER パラメータを作る
fn property_range_filter(set_id: u8, property_type: u64, start: u64, end: u64) -> MessageParameter {
    let mut bytes = vec![set_id];
    shiguredo_moqt::varint::encode(property_type, &mut bytes);
    shiguredo_moqt::varint::encode(start, &mut bytes);
    shiguredo_moqt::varint::encode(end - start, &mut bytes);
    MessageParameter {
        param_type: PARAM_OBJECT_PROPERTY_FILTER,
        value: MessageParameterValue::LengthPrefixed(bytes),
    }
}

/// subscription の largest_received_location を (group_id, object_id) で返す
fn largest_received(session: &Session, request_id: u64) -> Option<(u64, u64)> {
    session
        .subscription(request_id)
        .expect("subscription が存在すること")
        .largest_received_location
        .as_ref()
        .map(|location| (location.group_id, location.object_id))
}

/// テスト用 SUBGROUP_HEADER の既定値を作る
///
/// 個々のテストは `SubgroupHeader { track_alias: ..., ..subgroup_header() }` の形で
/// header ごとに異なるフィールドだけを指定する。既定値は group_id 0 /
/// subgroup_id Explicit(0) / publisher_priority Some(128) / properties なし /
/// Group 終端なし / 最初の Object ではない。
fn subgroup_header() -> SubgroupHeader {
    SubgroupHeader {
        track_alias: 0,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    }
}

#[path = "test_session/alias_tombstone.rs"]
mod alias_tombstone;
#[path = "test_session/data_stream.rs"]
mod data_stream;
#[path = "test_session/datagram_object_filter.rs"]
mod datagram_object_filter;

#[path = "test_session/default_publisher_properties.rs"]
mod default_publisher_properties;
#[path = "test_session/duplicate_object_content.rs"]
mod duplicate_object_content;
#[path = "test_session/fetch.rs"]
mod fetch;
#[path = "test_session/goaway.rs"]
mod goaway;
#[path = "test_session/include_properties.rs"]
mod include_properties;
#[path = "test_session/multi_track.rs"]
mod multi_track;
#[path = "test_session/object_filter_pass.rs"]
mod object_filter_pass;
#[path = "test_session/outgoing_range_filter.rs"]
mod outgoing_range_filter;
#[path = "test_session/parameter_rules.rs"]
mod parameter_rules;
#[path = "test_session/request_stream.rs"]
mod request_stream;
#[path = "test_session/setup.rs"]
mod setup;
#[path = "test_session/shared_track_alias.rs"]
mod shared_track_alias;
#[path = "test_session/subgroup_object_filter.rs"]
mod subgroup_object_filter;
#[path = "test_session/subscription.rs"]
mod subscription;
#[path = "test_session/timeout_api.rs"]
mod timeout_api;
#[path = "test_session/track_status.rs"]
mod track_status;
