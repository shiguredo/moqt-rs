//! セッション終了コードの分類 (draft-ietf-moq-transport-23 §12.2 / §16.12.1) のテスト
//!
//! 同じ違反について、wire のバイト列を decode する経路と、decode 済みのメッセージを
//! Session API へ直接渡す経路の双方が、同じセッション終了コードに写ることを固定する。
//! 汎用の `PROTOCOL_VIOLATION` に畳まれると受信側のアプリが原因を失うため、
//! 違反の種類ごとに具体的なコードを検証する。

use super::*;
use shiguredo_moqt::error::{
    SESSION_CLOSED_CONTROL_STREAM, SESSION_DELTA_ENCODING_OVERFLOW, SESSION_FIELD_LENGTH_EXCEEDED,
    SESSION_INVALID_OBJECT_STATUS, SESSION_INVALID_PARAMETER, SESSION_INVALID_STREAM_HEADER,
    SESSION_INVALID_TRACK_NAMESPACE, SESSION_MALFORMED_KEY_VALUE, SESSION_MALFORMED_OBJECT,
    SESSION_ROLE_VIOLATION, SESSION_TRUNCATED_OBJECT, SESSION_UNEXPECTED_MESSAGE,
};
use shiguredo_moqt::message::{
    Goaway, RequestUpdate, Subscribe, SubscribeOk, common::TrackNamespace,
};
use shiguredo_moqt::message_parameter::{
    MessageParameter, MessageParameterValue, MessageParameters, PARAM_FILL_TIMEOUT,
};

/// 制御メッセージのヘッダ (Type + Length) を組み立てる
///
/// `ControlMessage::decode` はメッセージ型と Length を先に読み、Length ぶんの本文を
/// 切り出してから本文を解析する。wire 入力のテストでは、本文の不正だけを切り分けるために
/// ヘッダを自前で組み立てる。
fn frame(type_id: u64, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    shiguredo_moqt::varint::encode(type_id, &mut buf);
    shiguredo_moqt::varint::encode(payload.len() as u64, &mut buf);
    buf.extend_from_slice(payload);
    buf
}

/// SUBSCRIBE の Request ID / Track Namespace / Track Name / Parameters を組み立てる
///
/// `payload` は Parameters ブロックの生バイト列である。
fn subscribe_payload(namespace: &[u8], track_name: &[u8], parameters: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    shiguredo_moqt::varint::encode(0, &mut payload); // Request ID
    payload.extend_from_slice(namespace);
    shiguredo_moqt::varint::encode(track_name.len() as u64, &mut payload);
    payload.extend_from_slice(track_name);
    payload.extend_from_slice(parameters);
    payload
}

/// Track Namespace (1 フィールド) を組み立てる
fn namespace(field: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    shiguredo_moqt::varint::encode(1, &mut buf);
    shiguredo_moqt::varint::encode(field.len() as u64, &mut buf);
    buf.extend_from_slice(field);
    buf
}

// ─── 制御プレーンの wire 入力 ──────────────────────────────

/// KVP の宣言長が残りバイトを超える wire は MALFORMED_KEY_VALUE (0x6) になること
///
/// draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure): 受信側が Type を理解する
/// 場合、Length/Value が定義と一致しないときは MALFORMED_KEY_VALUE で閉じる (MUST)。
#[test]
fn wire_malformed_kvp_maps_to_malformed_key_value() {
    // 奇数型 (AUTHORIZATION_TOKEN 0x03) の Length を 10 と宣言し、実データを 3 バイトにする
    let parameters = [0x01, 0x03, 0x0A, 0x01, 0x02, 0x03];
    let wire = frame(
        0x03,
        &subscribe_payload(&namespace(b"live"), b"cam", &parameters),
    );
    let err = ControlMessage::decode(&wire).expect_err("KVP の宣言長超過は decode に失敗すること");
    assert_eq!(
        err.session_termination_code(),
        Some(SESSION_MALFORMED_KEY_VALUE),
        "KVP の直列化不整合は MALFORMED_KEY_VALUE (0x6) に写ること"
    );
}

/// Track Namespace Field の長さが 0 の wire は INVALID_TRACK_NAMESPACE (0x302) になること
///
/// draft-ietf-moq-transport-23 §8.7 (Track Namespace Structure): 長さ 0 の Track Namespace
/// Field を受信したら INVALID_TRACK_NAMESPACE で閉じる (MUST)。
#[test]
fn wire_invalid_track_namespace_maps_to_invalid_track_namespace() {
    // Track Namespace: count=1 / field length=0
    let namespace_bytes = [0x01, 0x00];
    let wire = frame(0x03, &subscribe_payload(&namespace_bytes, b"cam", &[0x00]));
    let err =
        ControlMessage::decode(&wire).expect_err("空の名前空間フィールドは decode に失敗すること");
    assert_eq!(
        err.session_termination_code(),
        Some(SESSION_INVALID_TRACK_NAMESPACE),
        "Track Namespace の構造違反は INVALID_TRACK_NAMESPACE (0x302) に写ること"
    );
}

/// 4,096 バイトを超える Track Namespace の wire は FIELD_LENGTH_EXCEEDED (0x401) になること
///
/// draft-ietf-moq-transport-23 §8.7 (Track Namespace Structure) の長さ制限の MUST。
/// 構造違反 (フィールド数・空フィールド) が同時に該当しない入力で、長さの分類だけを固定する。
#[test]
fn wire_oversized_track_namespace_maps_to_field_length_exceeded() {
    let mut namespace_bytes = Vec::new();
    shiguredo_moqt::varint::encode(1, &mut namespace_bytes);
    shiguredo_moqt::varint::encode(4096, &mut namespace_bytes);
    namespace_bytes.extend_from_slice(&[b'a'; 4096]);
    let wire = frame(0x03, &subscribe_payload(&namespace_bytes, b"cam", &[0x00]));
    let err = ControlMessage::decode(&wire).expect_err("4,096 バイト超過は decode に失敗すること");
    assert_eq!(
        err.session_termination_code(),
        Some(SESSION_FIELD_LENGTH_EXCEEDED),
        "長さ超過は FIELD_LENGTH_EXCEEDED (0x401) に写ること"
    );
}

/// 未知のメッセージ型は UNEXPECTED_MESSAGE (0x303) になること
///
/// draft-ietf-moq-transport-23 §9 (Control Messages): "An endpoint that receives an unknown
/// message type MUST close the session with UNEXPECTED_MESSAGE." (MUST)
#[test]
fn wire_unknown_message_type_maps_to_unexpected_message() {
    // 0x3F は 1 バイト varint だが有効なメッセージ型 ID ではない
    let err = ControlMessage::decode(&[0x3F, 0x00]).expect_err("未知の型は decode に失敗すること");
    assert_eq!(
        err.session_termination_code(),
        Some(SESSION_UNEXPECTED_MESSAGE),
        "未知のメッセージ型は UNEXPECTED_MESSAGE (0x303) に写ること"
    );
}

/// Parameter の Type Delta が u64 を超える wire は DELTA_ENCODING_OVERFLOW (0x400) になること
///
/// draft-ietf-moq-transport-23 §9.19 (Control Message Parameters): "If the resulting Type
/// would be greater than 2^64 - 1, the endpoint MUST close the session with
/// DELTA_ENCODING_OVERFLOW." (MUST)
#[test]
fn wire_parameter_delta_overflow_maps_to_delta_encoding_overflow() {
    let mut parameters = Vec::new();
    shiguredo_moqt::varint::encode(2, &mut parameters); // Number of Parameters
    shiguredo_moqt::varint::encode(0x20, &mut parameters); // 1 個目: Type 0x20 (偶数型)
    shiguredo_moqt::varint::encode(0, &mut parameters); // 1 個目の varint 値
    shiguredo_moqt::varint::encode(u64::MAX, &mut parameters); // 2 個目: prev + delta が u64 を超える
    let wire = frame(
        0x03,
        &subscribe_payload(&namespace(b"live"), b"cam", &parameters),
    );
    let err = ControlMessage::decode(&wire).expect_err("Type Delta の溢出は decode に失敗すること");
    assert_eq!(
        err.session_termination_code(),
        Some(SESSION_DELTA_ENCODING_OVERFLOW),
        "デルタ溢出は DELTA_ENCODING_OVERFLOW (0x400) に写ること"
    );
}

/// 宣言長が受信上限を超える制御メッセージは FIELD_LENGTH_EXCEEDED (0x401) になること
///
/// draft-ietf-moq-transport-23 §9 (Control Messages) / §9.1.8 (MAX_CONTROL_MESSAGE_SIZE):
/// 受信上限を超える Length の宣言は本文を待たずに FIELD_LENGTH_EXCEEDED で閉じる (MUST)。
/// 未知のメッセージ型や本文の不正より長さの判定を優先する (§12.2 の優先規則)。
#[test]
fn wire_control_message_length_exceeded_maps_to_field_length_exceeded() {
    let mut wire = Vec::new();
    shiguredo_moqt::varint::encode(0x03, &mut wire);
    shiguredo_moqt::varint::encode(4, &mut wire); // 受信上限 3 に対して 4 を宣言
    wire.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    let mut decoder = shiguredo_moqt::decoder::MessageDecoder::new(3);
    decoder.push(&wire);
    let err = decoder
        .try_decode_message()
        .expect_err("受信上限を超える Length は decode に失敗すること");
    assert_eq!(
        err.session_termination_code(),
        Some(SESSION_FIELD_LENGTH_EXCEEDED),
        "宣言長の超過は FIELD_LENGTH_EXCEEDED (0x401) に写ること"
    );
}

/// 未知のメッセージ型でも宣言長の超過は FIELD_LENGTH_EXCEEDED が優先されること
///
/// draft-ietf-moq-transport-23 §12.2 (Session Termination Codes): "When more than one code
/// applies, an endpoint uses DELTA_ENCODING_OVERFLOW or FIELD_LENGTH_EXCEEDED in preference to
/// the other codes." 未知型 (UNEXPECTED_MESSAGE) と長さ超過が同時に該当する場合も長さ超過を選ぶ。
#[test]
fn wire_unknown_message_type_with_oversized_length_maps_to_field_length_exceeded() {
    let mut wire = Vec::new();
    shiguredo_moqt::varint::encode(0x7f, &mut wire); // 未知のメッセージ型
    shiguredo_moqt::varint::encode(4, &mut wire); // 受信上限 3 に対して 4 を宣言
    wire.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    let mut decoder = shiguredo_moqt::decoder::MessageDecoder::new(3);
    decoder.push(&wire);
    let err = decoder
        .try_decode_message()
        .expect_err("受信上限を超える Length は decode に失敗すること");
    assert_eq!(
        err.session_termination_code(),
        Some(SESSION_FIELD_LENGTH_EXCEEDED),
        "未知型でも宣言長の超過が優先され FIELD_LENGTH_EXCEEDED (0x401) に写ること"
    );
}

/// デルタ溢出は後続の長さ超過より優先して DELTA_ENCODING_OVERFLOW になること
///
/// draft-ietf-moq-transport-23 §12.2 (Session Termination Codes) の優先規則。1 件目で巨大な
/// 型を確定させ、2 件目で delta を溢出させつつ 2^16-1 を超える Length を宣言する。
#[test]
fn wire_delta_overflow_takes_precedence_over_field_length_exceeded() {
    use shiguredo_moqt::track_properties::TrackProperties;
    let mut wire = Vec::new();
    // 1 件目: 巨大な型 (delta = u64::MAX - 1、偶数型なので値は varint 0)
    shiguredo_moqt::varint::encode(u64::MAX - 1, &mut wire);
    wire.push(0x00);
    // 2 件目: delta 5 で u64 を溢出し、さらに 65,536 の長さを宣言する
    shiguredo_moqt::varint::encode(5, &mut wire);
    shiguredo_moqt::varint::encode(65_536, &mut wire);
    let err = TrackProperties::decode(&wire).expect_err("delta 溢出で decode に失敗すること");
    assert_eq!(
        err.session_termination_code(),
        Some(SESSION_DELTA_ENCODING_OVERFLOW),
        "delta 溢出が優先され DELTA_ENCODING_OVERFLOW (0x400) に写ること"
    );
}

/// 未知の Object Status を持つ OBJECT_DATAGRAM の wire は INVALID_OBJECT_STATUS (0x202) になること
///
/// draft-ietf-moq-transport-23 §11.1.1 (Object Status): 定義値以外の Status を受信したら
/// INVALID_OBJECT_STATUS で閉じる (SHOULD)。STATUS bit (0x20) と ZERO_OBJECT_ID /
/// DEFAULT_PRIORITY を立て、Status だけを不正値にする。
#[test]
fn wire_object_datagram_invalid_status_maps_to_invalid_object_status() {
    // Type Flags: STATUS (0x20) + ZERO_OBJECT_ID (0x04) + DEFAULT_PRIORITY (0x08)
    let wire = [0x2C, 0x01, 0x02, 0x07];
    let err = shiguredo_moqt::stream::datagram::ObjectDatagram::decode(&wire)
        .expect_err("未知の Object Status は decode に失敗すること");
    assert_eq!(
        err.session_termination_code(),
        Some(SESSION_INVALID_OBJECT_STATUS),
        "Object Status の不正は INVALID_OBJECT_STATUS (0x202) に写ること"
    );
}

/// 不正な Type Flags を持つ SUBGROUP_HEADER の wire は INVALID_STREAM_HEADER (0x200) になること
///
/// draft-ietf-moq-transport-23 §11.3.1 (Subgroup Header): bit4 が立っていない値は
/// INVALID_STREAM_HEADER で閉じる (MUST)。
#[test]
fn wire_subgroup_header_invalid_flags_map_to_invalid_stream_header() {
    let err = shiguredo_moqt::stream::subgroup::SubgroupHeader::decode(&[0x00])
        .expect_err("bit4 の立っていない Type Flags は decode に失敗すること");
    assert_eq!(
        err.session_termination_code(),
        Some(SESSION_INVALID_STREAM_HEADER),
        "Type Flags の不正は INVALID_STREAM_HEADER (0x200) に写ること"
    );
}

/// 未知の FETCH Serialization Flags の wire は MALFORMED_OBJECT (0x201) になること
///
/// draft-ietf-moq-transport-23 §11.4.1 (Fetch Header): Table 8 に無い 128 以上の値は
/// MALFORMED_OBJECT で閉じる (MUST)。
#[test]
fn wire_fetch_unknown_flags_map_to_malformed_object() {
    let mut wire = Vec::new();
    shiguredo_moqt::varint::encode(0x8D, &mut wire);
    let err = shiguredo_moqt::stream::fetch::FetchStreamEntry::decode(
        &wire,
        shiguredo_moqt::stream::fetch::FetchPriorContext::First,
    )
    .expect_err("未知の Serialization Flags は decode に失敗すること");
    assert_eq!(
        err.session_termination_code(),
        Some(SESSION_MALFORMED_OBJECT),
        "Object の復号失敗は MALFORMED_OBJECT (0x201) に写ること"
    );
}

// ─── 制御プレーンの Session API 入力 ──────────────────────

/// 許可されない Message Parameter を直接渡すと INVALID_PARAMETER (0x305) になること
///
/// draft-ietf-moq-transport-23 §9.19.1 (Parameter Scope): メッセージ型に許可されない
/// パラメータの受信は INVALID_PARAMETER で閉じる (MUST)。wire の decode は scope を
/// 検証しないため、Session API 側の検証経路を直接固定する。
#[test]
fn api_parameter_scope_violation_maps_to_invalid_parameter() {
    let (_, mut server) = establish_pair();
    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter {
        param_type: PARAM_FILL_TIMEOUT,
        value: MessageParameterValue::VarInt(1),
    });
    let message = ControlMessage::Subscribe(Subscribe {
        request_id: 0,
        track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
            .expect("正当な Track Namespace である"),
        track_name: b"cam".to_vec(),
        parameters,
    });
    match server.recv_request(message) {
        Err(shiguredo_moqt::session::types::RecvRequestError::Session(err)) => {
            assert_eq!(
                err.code, SESSION_INVALID_PARAMETER,
                "scope 違反は INVALID_PARAMETER (0x305) であること"
            );
        }
        other => panic!("想定外の結果: {other:?}"),
    }
    match drain_until_close(&mut server) {
        SessionEvent::CloseSession(err) => assert_eq!(err.code, SESSION_INVALID_PARAMETER),
        other => panic!("想定外のイベント: {other:?}"),
    }
}

/// 同じ Request ID へ 2 回目の応答を直接渡すと UNEXPECTED_MESSAGE (0x303) になること
///
/// draft-ietf-moq-transport-23 §12.2 (Session Termination Codes) の UNEXPECTED_MESSAGE は
/// "more times than allowed" を含む。§3.1 (Subscriptions) は SUBSCRIBE に対して
/// ちょうど 1 個の応答を要求する。
#[test]
fn api_duplicate_response_maps_to_unexpected_message() {
    let (mut client, _server, rid) = establish_subscribe_track(710);
    let duplicate = ControlMessage::SubscribeOk(SubscribeOk {
        track_alias: 710,
        parameters: MessageParameters::new(),
        track_properties: TrackProperties::new(),
    });
    let err = client
        .recv_stream_message(rid, duplicate)
        .expect_err("確立済み subscription への再応答は拒否されること");
    assert_eq!(
        err.code, SESSION_UNEXPECTED_MESSAGE,
        "重複応答は UNEXPECTED_MESSAGE (0x303) であること"
    );
}

/// publisher 側の購読に SUBSCRIBE_OK を直接渡すと ROLE_VIOLATION (0x304) になること
///
/// draft-ietf-moq-transport-23 §12.2 (Session Termination Codes): 応答は要求の受信者だけが
/// 送れる。PUBLISH の送信側 (publisher) に SUBSCRIBE_OK が届くのは role 違反であり、
/// 重複応答 (UNEXPECTED_MESSAGE) と区別する。
#[test]
fn api_subscribe_ok_for_publisher_side_subscription_maps_to_role_violation() {
    let (mut client, mut server) = establish_pair();
    let rid = establish_publish_sender_as_established(&mut client, &mut server, 1);
    let ok = ControlMessage::SubscribeOk(SubscribeOk {
        track_alias: 2,
        parameters: MessageParameters::new(),
        track_properties: TrackProperties::new(),
    });
    let err = server
        .recv_stream_message(rid, ok)
        .expect_err("publisher 側の購読への SUBSCRIBE_OK は拒否されること");
    assert_eq!(
        err.code, SESSION_ROLE_VIOLATION,
        "role 違反は ROLE_VIOLATION (0x304) であること"
    );
}

/// publisher から REQUEST_UPDATE を直接渡すと ROLE_VIOLATION (0x304) になること
///
/// draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE) は REQUEST_UPDATE を subscriber が
/// 送るものとし、publisher からの受信は role 違反になる。
#[test]
fn api_request_update_from_publisher_maps_to_role_violation() {
    let (mut client, _server, rid) = establish_subscribe_track(711);
    // REQUEST_UPDATE は sender の Request ID を消費する (§6.4.2.1) ため、
    // server が送る場合は奇数 ID になる。ここでは role だけを不正にする。
    let update = ControlMessage::RequestUpdate(RequestUpdate {
        request_id: 1,
        parameters: MessageParameters::new(),
    });
    let err = client
        .recv_stream_message(rid, update)
        .expect_err("publisher からの REQUEST_UPDATE は拒否されること");
    assert_eq!(
        err.code, SESSION_ROLE_VIOLATION,
        "role 違反は ROLE_VIOLATION (0x304) であること"
    );
}

/// peer の制御ストリームが FIN で閉じた通知は CLOSED_CONTROL_STREAM (0x306) になること
///
/// draft-ietf-moq-transport-23 §6.3 (Session initialization): "If the control stream is
/// closed, the peer MUST close the session with CLOSED_CONTROL_STREAM." (MUST)
#[test]
fn api_closed_control_stream_maps_to_closed_control_stream() {
    let (mut client, _server) = establish_pair();
    let err = client
        .recv_control_stream_closed(RequestStreamEnd::Fin)
        .expect_err("制御ストリームの閉鎖はセッション終了になること");
    assert_eq!(
        err.code, SESSION_CLOSED_CONTROL_STREAM,
        "制御ストリームの閉鎖は CLOSED_CONTROL_STREAM (0x306) であること"
    );
}

/// client からの非空 New Session URI を持つ GOAWAY を直接渡すと ROLE_VIOLATION (0x304) になること
///
/// draft-ietf-moq-transport-23 §9.2 (GOAWAY): "A client MUST send a zero-length New Session
/// URI in any GOAWAY." §12.2 (Session Termination Codes) の ROLE_VIOLATION が
/// "a server receiving a non-empty New Session URI" を例示しているため ROLE_VIOLATION で閉じる。
#[test]
fn api_client_goaway_with_non_zero_uri_maps_to_role_violation() {
    let (_, mut server) = establish_pair();
    let goaway = ControlMessage::Goaway(Goaway {
        new_session_uri: b"https://relay.example/".to_vec(),
        timeout: 0,
    });
    // client が送った GOAWAY を server が受信する経路を作る
    let err = server
        .recv_control(goaway)
        .expect_err("client からの非空 URI は拒否されること");
    assert_eq!(
        err.code, SESSION_ROLE_VIOLATION,
        "role 違反は ROLE_VIOLATION (0x304) であること"
    );
}

// ─── データプレーンの Session API 入力 ────────────────────

/// 未知の Object Status を持つ datagram を直接渡すと INVALID_OBJECT_STATUS (0x202) になること
#[test]
fn api_object_datagram_invalid_status_maps_to_invalid_object_status() {
    let (mut client, _server, _rid) = establish_subscribe_track(712);
    let err = client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: 712,
            group_id: 0,
            object_id: 1,
            publisher_priority: Some(1),
            properties_data: None,
            end_of_group: false,
            status: Some(0x07),
        })
        .expect_err("未知の Object Status は拒否されること");
    assert_eq!(
        err.code, SESSION_INVALID_OBJECT_STATUS,
        "Object Status の不正は INVALID_OBJECT_STATUS (0x202) であること"
    );
}

/// 非 Normal の Status に Properties を付けた datagram は INVALID_OBJECT_STATUS (0x202) になること
///
/// draft-ietf-moq-transport-23 §11.1.2 (Object Properties): 非 Normal の Object に Properties を
/// 付けた場合は INVALID_OBJECT_STATUS で閉じる (MUST)。
#[test]
fn api_object_datagram_properties_on_non_normal_status_maps_to_invalid_object_status() {
    let (mut client, _server, _rid) = establish_subscribe_track(713);
    let err = client
        .recv_object_datagram(&ObjectDatagram {
            track_alias: 713,
            group_id: 0,
            object_id: 1,
            publisher_priority: Some(1),
            // Properties Length = 1 + 偶数型 0x20 の varint 値
            properties_data: Some(vec![0x02, 0x20, 0x01]),
            end_of_group: false,
            status: Some(0x03),
        })
        .expect_err("非 Normal status への Properties は拒否されること");
    assert_eq!(
        err.code, SESSION_INVALID_OBJECT_STATUS,
        "非 Normal status への Properties は INVALID_OBJECT_STATUS (0x202) であること"
    );
}

/// 未知の datagram type は INVALID_STREAM_HEADER (0x200) になること
///
/// draft-ietf-moq-transport-23 §11 (Data Streams and Datagrams): 未知の datagram type は
/// INVALID_STREAM_HEADER で閉じる (MUST)。
#[test]
fn api_unknown_datagram_type_maps_to_invalid_stream_header() {
    let (mut client, _server) = establish_pair();
    let err = client
        .recv_datagram(&[0x10, 0x00, 0x00, 0x00])
        .expect_err("未知の datagram type は拒否されること");
    assert_eq!(
        err.code, SESSION_INVALID_STREAM_HEADER,
        "未知の datagram type は INVALID_STREAM_HEADER (0x200) であること"
    );
}

/// ヘッダが途中で切れた datagram は MALFORMED_OBJECT (0x201) になること
///
/// Object の直列化を復号できないため、§12.2 (Session Termination Codes) の
/// MALFORMED_OBJECT で閉じる。
#[test]
fn api_truncated_datagram_maps_to_malformed_object() {
    let (mut client, _server) = establish_pair();
    let err = client
        .recv_datagram(&[0x00])
        .expect_err("ヘッダが不足する datagram は拒否されること");
    assert_eq!(
        err.code, SESSION_MALFORMED_OBJECT,
        "途中で切れた datagram は MALFORMED_OBJECT (0x201) であること"
    );
}

/// 未知の Request ID を持つ FETCH_HEADER は再試行可能な結果になり、セッションを閉じないこと
///
/// draft-ietf-moq-transport-23 §11.4.1 (Fetch Header) は未知の Request ID の扱いを MUST で
/// 定めていない。§3.4.1 (Opening and Closing Fill Fetch Streams) により PUBLISH の初回 fill は
/// PUBLISH の処理前に届きうるため、未知の Request ID を理由にセッションを閉じない。
/// caller は stream と decode 済みヘッダを保留し、PUBLISH の受理後に再通知する。
#[test]
fn api_fetch_header_for_unknown_request_returns_retryable_result() {
    use shiguredo_moqt::session::types::FetchHeaderAcceptance;

    let (mut client, _server) = establish_pair();
    let stream_id = DataStreamId(91);
    client
        .recv_data_stream_type(stream_id, shiguredo_moqt::stream::FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    let acceptance = client
        .recv_fetch_header(stream_id, &FetchHeader { request_id: 4 })
        .expect("未知の Request ID でもセッションを閉じないこと");
    assert_eq!(
        acceptance,
        FetchHeaderAcceptance::RequestNotRegistered,
        "未知の Request ID は再試行可能な結果として返すこと"
    );
    assert_eq!(
        client.state(),
        SessionState::Established,
        "未知の Request ID の FETCH_HEADER でセッションを閉じないこと"
    );
}

/// シリアライズ途中の FIN は TRUNCATED_OBJECT (0x203) になること
///
/// draft-ietf-moq-transport-23 §11.3 (Subgroup Streams): "If a stream ends gracefully (i.e.,
/// the stream terminates with a FIN) in the middle of a serialized Object, the receiver
/// SHOULD close the session with TRUNCATED_OBJECT."
#[test]
fn api_mid_object_fin_maps_to_truncated_object() {
    let (mut client, _server, _rid) = establish_subscribe_track(714);
    let stream_id = DataStreamId(92);
    client
        .recv_data_stream_type(stream_id, 0x15)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_subgroup_header(
            stream_id,
            &SubgroupHeader {
                track_alias: 714,
                group_id: 0,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(1),
                has_properties: false,
                end_of_group: false,
                first_object: true,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let err = client
        .report_mid_object_fin(stream_id)
        .expect_err("シリアライズ途中の FIN はセッション終了になること");
    assert_eq!(
        err.code, SESSION_TRUNCATED_OBJECT,
        "途中終端は TRUNCATED_OBJECT (0x203) であること"
    );
}
