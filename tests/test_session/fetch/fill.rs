//! fill fetch stream のテスト (draft-ietf-moq-transport-21 §3.4 (Fill Semantics))
//!
//! FILL_PARAMETERS の送受信、fill fetch stream の開設条件 (Forward State /
//! fill range / Largest Object)、複数 stream の同時存在、FETCH_HEADER の
//! Request ID、subscription キャンセル時の reset を扱う。

use super::*;
use shiguredo_moqt::error::STREAM_CANCELLED;
use shiguredo_moqt::message::ReasonPhrase;
use shiguredo_moqt::message_parameter::{
    MessageParameterValue, PARAM_FILL_PARAMETERS, PARAM_FORWARD, PARAM_LOCATION_FILTER,
};
use shiguredo_moqt::{message_parameter::LocationFilter, stream::subgroup::SubgroupHeader};

/// FILL_PARAMETERS 1 件を持つ MessageParameters を作る
fn fill_params(inner: MessageParameters) -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FILL_PARAMETERS,
        value: MessageParameterValue::FillParameters(inner),
    });
    params
}

/// LOCATION_FILTER 1 件を持つ内側パラメータ群を作る
fn inner_with_location_filter(filter: &LocationFilter) -> MessageParameters {
    let mut inner = MessageParameters::new();
    inner.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(filter.encode_to_bytes()),
    });
    inner
}

/// OpenFillFetchStream イベントの Request ID 列をすべて回収する
///
/// CloseSession が混入したら panic する (成功系テストでセッション死を隠さないため)。
fn drain_open_fill_events(s: &mut Session) -> Vec<u64> {
    let mut ids = Vec::new();
    while let Some(e) = s.poll_event() {
        match e {
            SessionEvent::OpenFillFetchStream { request_id } => ids.push(request_id),
            SessionEvent::CloseSession(err) => {
                panic!("CloseSession が発行された: {err:?}")
            }
            _ => {}
        }
    }
    ids
}

/// 最初の RequestUpdateReceived イベントのパラメータを取り出す
fn take_request_update_params(s: &mut Session) -> MessageParameters {
    while let Some(e) = s.poll_event() {
        if let SessionEvent::RequestUpdateReceived { parameters, .. } = e {
            return parameters;
        }
    }
    panic!("RequestUpdateReceived イベントが期待されたが発行されなかった");
}

/// SUBSCRIBE を Established にし、server 側で {0, 0} を公開して Largest {0, 0} を確定させる
fn establish_sub_with_object() -> (Session, Session, u64) {
    establish_sub_with_params_and_object(MessageParameters::new(), 0, 0)
}

/// 指定 filter の SUBSCRIBE を Established にし、server 側で {0, 0} を公開する
fn establish_filtered_sub_with_object(filter: &LocationFilter) -> (Session, Session, u64) {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(filter.encode_to_bytes()),
    });
    establish_sub_with_params_and_object(params, 0, 0)
}

/// SUBSCRIBE を Established にし、server 側で {group, object} を公開して Largest を確定させる
fn establish_sub_with_group_object(group: u64, object: u64) -> (Session, Session, u64) {
    establish_sub_with_params_and_object(MessageParameters::new(), group, object)
}

/// SUBSCRIBE (指定パラメータ) を Established にし、server 側で指定位置を公開する
fn establish_sub_with_params_and_object(
    params: MessageParameters,
    group: u64,
    object: u64,
) -> (Session, Session, u64) {
    let (mut client, mut server) = establish_pair();
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(sub_rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // server (publisher) が指定位置を公開し、観測 Largest を確定させる
    let stream_id = DataStreamId(1);
    server
        .send_subgroup_header(
            stream_id,
            sub_rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: group,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, object, None)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server, sub_rid)
}

/// server が PUBLISH を送り publisher 役の subscription を持つ状態を作る
fn establish_pub_side() -> (Session, Session, u64) {
    let (client, mut server) = establish_pair();
    let pub_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server, pub_rid)
}

/// REQUEST_UPDATE を client から server へ送る
fn send_update_to_server(
    client: &mut Session,
    server: &mut Session,
    sub_rid: u64,
    params: MessageParameters,
) {
    client
        .send_request_update(sub_rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(client);
    server
        .recv_stream_message(sub_rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
}

/// Largest Object 未知のまま FILL_PARAMETERS 付き SUBSCRIBE を受けても fill stream は開かない
/// (draft-ietf-moq-transport-21 §3.4: fill range が定まらないため)
#[test]
fn subscribe_with_fill_but_no_largest_object_opens_nothing() {
    let (mut client, mut server) = establish_pair();
    let sub_rid = client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fill_params(MessageParameters::new()),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // subscription 自体は受理される
    assert!(server.subscription(sub_rid).is_some());
    assert_eq!(
        drain_open_fill_events(&mut server),
        Vec::<u64>::new(),
        "Largest 不明では fill stream を開いてはならない"
    );
}

/// FORWARD=0 の SUBSCRIBE に FILL_PARAMETERS が付いても fill stream は開かない
/// (draft-ietf-moq-transport-21 §3.4.1。Largest 確定済みでも Forward が優先する)
#[test]
fn subscribe_with_fill_and_forward_zero_opens_nothing() {
    let (mut client, mut server, _sub_rid) = establish_sub_with_object();
    // 同一 track への 2 本目の SUBSCRIBE (FORWARD=0 + FILL)。Largest {0, 0} は確定済み。
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    params.push(MessageParameter {
        param_type: PARAM_FILL_PARAMETERS,
        value: MessageParameterValue::FillParameters(MessageParameters::new()),
    });
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(server.subscription(sub_rid).is_some());
    assert_eq!(
        drain_open_fill_events(&mut server),
        Vec::<u64>::new(),
        "Forward State 0 では fill stream を開いてはならない"
    );
}

/// Forward State 1 の FILL_PARAMETERS 付き SUBSCRIBE 処理で fill stream が開く
/// (draft-ietf-moq-transport-21 §3.4.1。SUBSCRIBE 経路の positive)
#[test]
fn subscribe_with_fill_opens_fill_stream() {
    let (mut client, mut server, _sub_rid) = establish_sub_with_object();
    // 同一 track への 2 本目の SUBSCRIBE (FORWARD=1 + FILL)。Largest {0, 0} は確定済み。
    let sub_rid = client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fill_params(MessageParameters::new()),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(server.subscription(sub_rid).is_some());
    assert_eq!(
        drain_open_fill_events(&mut server),
        vec![sub_rid],
        "SUBSCRIBE 処理時にも fill stream が開くこと"
    );
}

/// Forward State 1 で FILL_PARAMETERS 付き REQUEST_UPDATE を処理すると fill stream が開く
/// (draft-ietf-moq-transport-21 §3.4.1)
#[test]
fn request_update_with_fill_opens_fill_stream() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    // 内側パラメータなし = subscription の設定で fill する (track 全体)
    send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(
        drain_open_fill_events(&mut server),
        vec![sub_rid],
        "起因 SUBSCRIBE / REQUEST_UPDATE の Request ID で fill stream を開くこと"
    );
}

/// FILL 内側の zero-length LOCATION_FILTER は track 全体を指し fill stream が開く
/// (draft-ietf-moq-transport-21 §3.4)
#[test]
fn request_update_with_fill_zero_length_inner_filter_opens_fill_stream() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    let mut inner = MessageParameters::new();
    inner.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(Vec::new()),
    });
    send_update_to_server(&mut client, &mut server, sub_rid, fill_params(inner));
    assert_eq!(
        drain_open_fill_events(&mut server),
        vec![sub_rid],
        "zero-length 内側 filter は track 全体として fill stream を開くこと"
    );
}

/// fill range が Largest Object より後に始まる場合は fill stream を開かない
/// (draft-ietf-moq-transport-21 §3.4)
#[test]
fn request_update_with_fill_start_after_largest_opens_nothing() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    // Largest {0, 0} に対して Start {5, 0} は範囲外
    let inner = inner_with_location_filter(&LocationFilter::AbsoluteStart {
        start: Location {
            group_id: 5,
            object_id: 0,
        },
    });
    send_update_to_server(&mut client, &mut server, sub_rid, fill_params(inner));
    assert_eq!(
        drain_open_fill_events(&mut server),
        Vec::<u64>::new(),
        "fill range が Largest Object より後に始まる場合は開設しないこと"
    );
}

/// FILL_PARAMETERS のない REQUEST_UPDATE では fill stream を開かない
/// (draft-ietf-moq-transport-21 §3.4.1)
#[test]
fn request_update_without_fill_opens_nothing() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    send_update_to_server(&mut client, &mut server, sub_rid, params);
    assert_eq!(
        drain_open_fill_events(&mut server),
        Vec::<u64>::new(),
        "FILL なし REQUEST_UPDATE では新規 fill stream を開いてはならない"
    );
}

/// FILL_PARAMETERS 付き REQUEST_UPDATE ごとに新規 fill stream が開き、既存は残る
/// (draft-ietf-moq-transport-21 §3.4: 暗黙キャンセルしない)
#[test]
fn second_request_update_with_fill_opens_another_stream() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![sub_rid]);
    // 1 本目を実際に開設する
    server
        .send_fill_fetch_header(DataStreamId(100), sub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.open_outgoing_fill_stream_count(sub_rid), 1);
    // 2 回目の FILL 付き UPDATE でも新規 fill stream が開き、1 本目は残る
    send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(
        drain_open_fill_events(&mut server),
        vec![sub_rid],
        "2 回目の FILL 付き UPDATE でも新規 fill stream が開くこと"
    );
    server
        .send_fill_fetch_header(DataStreamId(101), sub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server.open_outgoing_fill_stream_count(sub_rid),
        2,
        "fill stream は複数本が同時に存在できること"
    );
}

/// fill stream が open 中は PUBLISH_DONE を送れない
/// (draft-ietf-moq-transport-21 §9.9 の MUST NOT が fill を含むことの裏付け)
#[test]
fn publish_done_rejected_while_fill_stream_open() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![sub_rid]);
    server
        .send_fill_fetch_header(DataStreamId(100), sub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = server
        .send_publish_done(
            sub_rid,
            0x2,
            1,
            ReasonPhrase::new("done").expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect_err("open 中の fill stream がある間は PUBLISH_DONE を送れないこと");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// FILL_PARAMETERS は累積パラメータに保持されない (sticky 対象外)
/// (draft-ietf-moq-transport-21 §9.20.16)
#[test]
fn fill_not_retained_in_pending_update_params() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    let merged = take_request_update_params(&mut server);
    assert!(
        merged.fill_parameters().is_some(),
        "運んできたメッセージ自体の FILL はアプリに通知されること"
    );
    // FILL なしの UPDATE を送る
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    send_update_to_server(&mut client, &mut server, sub_rid, params);
    let merged = take_request_update_params(&mut server);
    assert!(
        merged.fill_parameters().is_none(),
        "FILL は後続 UPDATE に引き継がれてはならない"
    );
}

/// fill fetch stream の header 登録・ Object 通知・終端と Stream Count の計上
/// (draft-ietf-moq-transport-21 §3.4 / §9.9 / §11.4.1)
#[test]
fn fill_fetch_header_object_close_flow() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![sub_rid]);

    // 未知 subscription では登録できない
    let err = server
        .send_fill_fetch_header(DataStreamId(100), 999)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);

    // fill stream を開く
    let stream_id = DataStreamId(100);
    server
        .send_fill_fetch_header(stream_id, sub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.open_outgoing_fill_stream_count(sub_rid), 1);
    server
        .send_fetch_object(stream_id)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_fetch_data_stream_closed(stream_id)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.open_outgoing_fill_stream_count(sub_rid), 0);

    // Stream Count には fill stream が含まれる (subgroup 1 本 + fill 1 本 = 2)。
    // 先に setup 用 subgroup stream を終端する (PUBLISH_DONE の MUST NOT)。
    server
        .send_data_stream_closed(DataStreamId(1), RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_publish_done(
            sub_rid,
            0x2,
            2,
            ReasonPhrase::new("done").expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("fill stream 数を含む Stream Count は受理されること");
}

/// subscriber は subscription の Request ID を載せた fill 用 FETCH_HEADER を受理する
/// (draft-ietf-moq-transport-21 §3.4)。複数本の同時存在も許す。
#[test]
fn subscriber_accepts_fill_fetch_headers() {
    use shiguredo_moqt::stream::FETCH_HEADER_TYPE;

    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![sub_rid]);

    // fill stream 1 本目も 2 本目も受理する (同時存在可)
    for stream_no in [200u64, 201u64] {
        let stream_id = DataStreamId(stream_no);
        client
            .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
            .expect("テストフィクスチャの前提条件を満たす");
        client
            .recv_fetch_header(
                stream_id,
                &FetchHeader {
                    request_id: sub_rid,
                },
            )
            .expect("fill 用 FETCH_HEADER は受理されること");
    }
    // FIN での終端は subscription に影響なく吸収する
    client
        .recv_data_stream_closed(DataStreamId(200), RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::SubscriptionState::Established,
        "fill stream の終端で subscription は終わらないこと"
    );
    // publisher 役の subscription を指す FETCH_HEADER は拒否する (別ペアで検証する)
    let (_peer, mut pub_server, pub_rid) = establish_pub_side();
    pub_server
        .recv_data_stream_type(DataStreamId(300), FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = pub_server
        .recv_fetch_header(
            DataStreamId(300),
            &FetchHeader {
                request_id: pub_rid,
            },
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // Terminated 済みの subscription を指す FETCH_HEADER は拒否する
    client
        .stop_sending(sub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_data_stream_type(DataStreamId(203), FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = client
        .recv_fetch_header(
            DataStreamId(203),
            &FetchHeader {
                request_id: sub_rid,
            },
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// 未知 Request ID の FETCH_HEADER は拒否しセッションを閉じる
#[test]
fn fill_fetch_header_with_unknown_request_id_closes_session() {
    use shiguredo_moqt::stream::FETCH_HEADER_TYPE;

    let (mut client, _server) = establish_pair();
    client
        .recv_data_stream_type(DataStreamId(202), FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = client
        .recv_fetch_header(DataStreamId(202), &FetchHeader { request_id: 999 })
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}

/// subscription のキャンセル時は open 中の fill fetch stream を reset する
/// (draft-ietf-moq-transport-21 §3.4.1)
#[test]
fn cancel_subscription_resets_open_fill_streams() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![sub_rid]);
    server
        .send_fill_fetch_header(DataStreamId(100), sub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.open_outgoing_fill_stream_count(sub_rid), 1);

    // bidi request stream の FIN で subscription をキャンセルする
    server
        .recv_request_stream_closed(sub_rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.open_outgoing_fill_stream_count(sub_rid), 0);
    let mut saw_reset = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::ResetDataStream {
            stream_id,
            error_code,
            reliable_size,
        } = e
        {
            assert_eq!(stream_id, DataStreamId(100));
            assert_eq!(error_code, STREAM_CANCELLED);
            assert_eq!(
                reliable_size, None,
                "自動発火では RESET_STREAM (reliable_size なし) になること"
            );
            saw_reset = true;
        }
    }
    assert!(saw_reset, "open 中の fill stream は reset されること");
}

/// FILL 内側の Range Filter 不正は INVALID_FILTER で拒否し fill を開かない
/// (draft-ietf-moq-transport-21 §9.20.16 / §3.3.2)
#[test]
fn fill_inner_range_filter_violation_rejected_with_invalid_filter() {
    use shiguredo_moqt::error::REQUEST_INVALID_FILTER;
    use shiguredo_moqt::message::RequestUpdate;
    use shiguredo_moqt::message_parameter::PARAM_SUBGROUP_FILTER;

    let (_client, mut server, sub_rid) = establish_sub_with_object();
    // 内側に重複 (SetID, Property Type) の SUBGROUP_FILTER を持つ FILL を直接構築する
    // (client 送信 API は送信前に弾くため、ワイヤメッセージとして受信させる)
    let mut inner = MessageParameters::new();
    for _ in 0..2 {
        inner.push(MessageParameter {
            param_type: PARAM_SUBGROUP_FILTER,
            value: MessageParameterValue::LengthPrefixed(vec![0x00, 0x00, 0x01]),
        });
    }
    server
        .recv_stream_message(
            sub_rid,
            ControlMessage::RequestUpdate(RequestUpdate {
                request_id: sub_rid,
                parameters: fill_params(inner),
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let mut saw_invalid_filter = false;
    let mut saw_fill = false;
    while let Some(e) = server.poll_event() {
        match e {
            SessionEvent::SendOnStream {
                message: ControlMessage::RequestError(e),
                ..
            } if e.error_code == REQUEST_INVALID_FILTER => {
                saw_invalid_filter = true;
            }
            SessionEvent::OpenFillFetchStream { .. } => {
                saw_fill = true;
            }
            _ => {}
        }
    }
    assert!(
        saw_invalid_filter,
        "内側 Range Filter 不正は INVALID_FILTER で拒否されること"
    );
    assert!(
        !saw_fill,
        "拒否された FILL で fill stream を開いてはならない"
    );
    assert_eq!(
        server.state(),
        SessionState::Established,
        "INVALID_FILTER 拒否でセッションは閉じないこと"
    );
}

/// Table 6 外の内側パラメータを持つ FILL の送信は送信前に拒否し副作用を残さない
/// (draft-ietf-moq-transport-21 §9.20.16)
#[test]
fn send_with_out_of_scope_inner_fill_rejected_without_side_effects() {
    use shiguredo_moqt::message_parameter::PARAM_FORWARD as INNER_FORWARD;

    // 内側に FORWARD (Table 6 外) を持つ FILL を作る
    let mut inner = MessageParameters::new();
    inner.push(MessageParameter {
        param_type: INNER_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    let params = fill_params(inner);

    // SUBSCRIBE 送信は request_id 発行・状態登録より前に拒否する
    let (mut client, _server) = establish_pair();
    let err = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), params.clone())
        .expect_err("Table 6 外の内側パラメータは送信前に拒否されること");
    assert_eq!(
        err.as_session_error()
            .expect("セッションエラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    assert_eq!(
        client.subscriptions().count(),
        0,
        "拒否した SUBSCRIBE は登録されてはならない"
    );
    while let Some(e) = client.poll_event() {
        assert!(
            !matches!(
                e,
                SessionEvent::SendRequest { .. } | SessionEvent::CloseSession(_)
            ),
            "拒否した送信で SendRequest / CloseSession は発行されないこと"
        );
    }

    // REQUEST_UPDATE 送信も楽観的状態更新より前に拒否する
    let (mut client, _server, sub_rid) = establish_sub_with_object();
    let err = client
        .send_request_update(sub_rid, params)
        .expect_err("Table 6 外の内側パラメータは送信前に拒否されること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1,
        "拒否した UPDATE で forward_state は変わらないこと"
    );
    while let Some(e) = client.poll_event() {
        assert!(
            !matches!(
                e,
                SessionEvent::SendOnStream { .. } | SessionEvent::CloseSession(_)
            ),
            "拒否した送信で SendOnStream / CloseSession は発行されないこと"
        );
    }
}

/// peer の STOP_SENDING による fill 単体の cancel は吸収し subscription に影響しない
/// (draft-ietf-moq-transport-21 §3.4.1)
#[test]
fn peer_stop_sending_on_fill_absorbed() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![sub_rid]);
    let stream_id = DataStreamId(100);
    server
        .send_fill_fetch_header(stream_id, sub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .recv_data_stream_stop_sending(stream_id)
        .expect("fill への STOP_SENDING は吸収されること");
    assert_eq!(server.open_outgoing_fill_stream_count(sub_rid), 0);
    assert_eq!(
        server
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::SubscriptionState::Established,
        "fill の cancel で subscription は終わらないこと"
    );
    assert_eq!(server.state(), SessionState::Established);
}

/// fill stream の単体 reset は ResetDataStream イベントを発行する
/// (draft-ietf-moq-transport-21 §3.4.1 の失敗時即 reset 経路)
#[test]
fn reset_single_fill_stream_emits_reset_event() {
    use shiguredo_moqt::session::types::DataStreamResetReason;

    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![sub_rid]);
    let stream_id = DataStreamId(100);
    server
        .send_fill_fetch_header(stream_id, sub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .reset_outgoing_data_stream(stream_id, DataStreamResetReason::Cancelled)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.open_outgoing_fill_stream_count(sub_rid), 0);
    let mut saw_reset = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::ResetDataStream {
            stream_id: rid,
            error_code,
            reliable_size,
        } = e
        {
            assert_eq!(rid, stream_id);
            assert_eq!(error_code, STREAM_CANCELLED);
            assert_eq!(
                reliable_size, None,
                "旧 API では RESET_STREAM (reliable_size なし) になること"
            );
            saw_reset = true;
        }
    }
    assert!(
        saw_reset,
        "fill の単体 reset は ResetDataStream を発行すること"
    );
}

/// 内側省略時は subscription の filter が fill range になる
/// (draft-ietf-moq-transport-21 §3.4)
#[test]
fn fill_without_inner_filter_inherits_subscription_filter() {
    // subscription filter RelativeGroup{0} (= Next Group): Largest {0, 0} に対して
    // 開始位置 {1, 0} は範囲外。内側省略の fill も開かない。
    // (発行 {0, 0} 自体は確立時解決の filter_start {0, 0} を通る)
    let (mut client, mut server, sub_rid) =
        establish_filtered_sub_with_object(&LocationFilter::RelativeGroup { start_group: 0 });
    send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(
        drain_open_fill_events(&mut server),
        Vec::<u64>::new(),
        "subscription filter が範囲外なら内側省略の fill も開かないこと"
    );

    // subscription filter AbsoluteRange {0, 0} - {0, MAX} (範囲内) + 内側省略 → 開く
    let (mut client, mut server, sub_rid) =
        establish_filtered_sub_with_object(&LocationFilter::AbsoluteRange {
            start: Location {
                group_id: 0,
                object_id: 0,
            },
            end_group_delta: 0,
        });
    send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(
        drain_open_fill_events(&mut server),
        vec![sub_rid],
        "subscription filter が範囲内なら内側省略の fill が開くこと"
    );
}

/// 同一 UPDATE での filter 変更は fill 評価に反映される (処理後観点)
/// (draft-ietf-moq-transport-21 §3.4)
#[test]
fn fill_evaluated_with_updated_filter_in_same_update() {
    // unfiltered で確立 → 同一 UPDATE で範囲外 filter + FILL → 開かない
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteStart {
                start: Location {
                    group_id: 5,
                    object_id: 0,
                },
            }
            .encode_to_bytes(),
        ),
    });
    params.push(MessageParameter {
        param_type: PARAM_FILL_PARAMETERS,
        value: MessageParameterValue::FillParameters(MessageParameters::new()),
    });
    send_update_to_server(&mut client, &mut server, sub_rid, params);
    assert_eq!(
        drain_open_fill_events(&mut server),
        Vec::<u64>::new(),
        "同一 UPDATE の filter 変更後の観点で評価すること"
    );
}

/// fill range が empty (Start > End) の場合は fill stream を開かない
/// (draft-ietf-moq-transport-21 §3.4)
#[test]
fn fill_with_empty_range_opens_nothing() {
    let (mut client, mut server, sub_rid) = establish_sub_with_group_object(5, 10);
    // AbsoluteRangeWithEnd {start {5, 10}, delta 0, end_object 3} → End {5, 3} < Start
    let inner = inner_with_location_filter(&LocationFilter::AbsoluteRangeWithEnd {
        start: Location {
            group_id: 5,
            object_id: 10,
        },
        end_group_delta: 0,
        end_object: 3,
    });
    send_update_to_server(&mut client, &mut server, sub_rid, fill_params(inner));
    assert_eq!(
        drain_open_fill_events(&mut server),
        Vec::<u64>::new(),
        "fill range が empty なら開設しないこと"
    );
}

/// Next Object 開始位置の溢出時は Largest 自体に飽和した範囲で fill stream を開く
///
/// 開始位置は `{0, MAX}` に飽和して定まるため、空範囲ではなく飽和点の範囲として
/// 開設する。下限なしの全通し (旧 fail-open) にはならない。
#[test]
fn fill_with_next_object_overflow_opens_saturated_range() {
    let (mut client, mut server, sub_rid) = establish_sub_with_group_object(0, u64::MAX);
    // NextObject: Start {0, MAX + 1} は {0, MAX} に飽和する
    let inner = inner_with_location_filter(&LocationFilter::NextObject);
    send_update_to_server(&mut client, &mut server, sub_rid, fill_params(inner));
    assert_eq!(
        drain_open_fill_events(&mut server),
        vec![sub_rid],
        "飽和した開始位置の fill は開設されること"
    );
}

/// 複数本の fill stream はキャンセル時にまとめて reset される
/// (draft-ietf-moq-transport-21 §3.4.1)
#[test]
fn cancel_subscription_resets_all_open_fill_streams() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    for _ in 0..2 {
        send_update_to_server(
            &mut client,
            &mut server,
            sub_rid,
            fill_params(MessageParameters::new()),
        );
        assert_eq!(drain_open_fill_events(&mut server), vec![sub_rid]);
    }
    server
        .send_fill_fetch_header(DataStreamId(100), sub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_fill_fetch_header(DataStreamId(101), sub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.open_outgoing_fill_stream_count(sub_rid), 2);
    server
        .recv_request_stream_closed(sub_rid, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.open_outgoing_fill_stream_count(sub_rid), 0);
    let mut resets = Vec::new();
    while let Some(e) = server.poll_event() {
        if let SessionEvent::ResetDataStream {
            stream_id,
            error_code,
            reliable_size,
        } = e
        {
            assert_eq!(error_code, STREAM_CANCELLED);
            assert_eq!(
                reliable_size, None,
                "自動発火では RESET_STREAM (reliable_size なし) になること"
            );
            resets.push(stream_id);
        }
    }
    resets.sort_by_key(|id| id.0);
    assert_eq!(
        resets,
        vec![DataStreamId(100), DataStreamId(101)],
        "open 中の全 fill stream が reset されること"
    );
}

/// REQUEST_UPDATE 失敗応答時は open 中の fill を reset して PUBLISH_DONE を送る
/// (draft-ietf-moq-transport-21 §3.4.1 / §9.5.1)
#[test]
fn request_error_resets_fills_and_sends_publish_done() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![sub_rid]);
    server
        .send_fill_fetch_header(DataStreamId(100), sub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    // setup 用 subgroup stream を終端する (残っていると PUBLISH_DONE が保留される)
    server
        .send_data_stream_closed(DataStreamId(1), RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    // REQUEST_UPDATE 失敗応答 → subscription 終了 + fill reset + PUBLISH_DONE
    server
        .send_request_error(
            sub_rid,
            0x01,
            0,
            ReasonPhrase::new("fail").expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.open_outgoing_fill_stream_count(sub_rid), 0);
    let mut saw_reset = false;
    let mut saw_publish_done = false;
    while let Some(e) = server.poll_event() {
        match e {
            SessionEvent::ResetDataStream {
                stream_id: DataStreamId(100),
                ..
            } => {
                saw_reset = true;
            }
            SessionEvent::SendOnStream {
                message: ControlMessage::PublishDone(_),
                ..
            } => {
                saw_publish_done = true;
            }
            _ => {}
        }
    }
    assert!(saw_reset, "失敗応答で fill stream が reset されること");
    assert!(saw_publish_done, "失敗応答で PUBLISH_DONE が送られること");
}
