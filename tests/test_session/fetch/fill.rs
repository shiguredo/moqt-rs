//! fill fetch stream のテスト (draft-ietf-moq-transport-22 §3.4 (Fill Semantics))
//!
//! FILL_PARAMETERS の送受信、fill fetch stream の開設条件 (PAUSED /
//! fill range / Largest Object)、複数 stream の同時存在、FETCH_HEADER の
//! Request ID、subscription キャンセル時の reset を扱う。

use super::*;
use shiguredo_moqt::error::STREAM_CANCELLED;
use shiguredo_moqt::message::ReasonPhrase;
use shiguredo_moqt::message_parameter::{
    MessageParameterValue, PARAM_FILL_PARAMETERS, PARAM_LOCATION_FILTER, PARAM_PAUSED,
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
        value: MessageParameterValue::LocationFilter(*filter),
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
            SessionEvent::OpenFillFetchStream { request_id, .. } => ids.push(request_id),
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
        value: MessageParameterValue::LocationFilter(*filter),
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

/// REQUEST_UPDATE を client から server へ送り、REQUEST_UPDATE 自身の Request ID を返す
///
/// draft-ietf-moq-transport-22 §6.4.2.1 (Request ID): REQUEST_UPDATE は SUBSCRIBE とは
/// 別に新しい Request ID を消費する。fill fetch stream の FETCH_HEADER はこの ID を
/// 載せる (draft-ietf-moq-transport-22 §3.4 (Fill Semantics))。
fn send_update_to_server(
    client: &mut Session,
    server: &mut Session,
    sub_rid: u64,
    params: MessageParameters,
) -> u64 {
    client
        .send_request_update(sub_rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (stream_rid, upd_msg) = take_send_on_stream(client);
    // SendOnStream の request_id は送信先 bidi stream の識別子 (購読の Request ID) のまま
    assert_eq!(
        stream_rid, sub_rid,
        "REQUEST_UPDATE は購読の bidi stream 上で送ること"
    );
    let update_rid = request_update_request_id(&upd_msg);
    assert_ne!(
        update_rid, sub_rid,
        "REQUEST_UPDATE は購読の Request ID を再利用してはならない"
    );
    server
        .recv_stream_message(sub_rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    update_rid
}

/// Largest Object 未知のまま FILL_PARAMETERS 付き SUBSCRIBE を受けても fill stream は開かない
/// (draft-ietf-moq-transport-22 §3.4: fill range が定まらないため)
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

/// PAUSED=1 の SUBSCRIBE に FILL_PARAMETERS が付いても fill stream は開かない
/// (draft-ietf-moq-transport-22 §3.4.1 / draft-ietf-moq-transport-23 §3.1.1。
/// Largest 確定済みでも paused が優先する)
#[test]
fn subscribe_with_fill_and_paused_opens_nothing() {
    let (mut client, mut server, _sub_rid) = establish_sub_with_object();
    // 同一 track への 2 本目の SUBSCRIBE (PAUSED=1 + FILL)。Largest {0, 0} は確定済み。
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_PAUSED,
        value: MessageParameterValue::Uint8(1),
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
        "paused (PAUSED=1) では fill stream を開いてはならない"
    );
}

/// unpaused (PAUSED=0) の FILL_PARAMETERS 付き SUBSCRIBE 処理で fill stream が開く
/// (draft-ietf-moq-transport-22 §3.4.1。SUBSCRIBE 経路の positive)
#[test]
fn subscribe_with_fill_opens_fill_stream() {
    let (mut client, mut server, _sub_rid) = establish_sub_with_object();
    // 同一 track への 2 本目の SUBSCRIBE (PAUSED 省略 = unpaused + FILL)。
    // Largest {0, 0} は確定済み。
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

/// OpenFillFetchStream イベントの (request_id, subscription_request_id, start, end) を回収する
fn drain_open_fill_stream_events(s: &mut Session) -> Vec<(u64, u64, Location, Location)> {
    let mut events = Vec::new();
    while let Some(e) = s.poll_event() {
        match e {
            SessionEvent::OpenFillFetchStream {
                request_id,
                subscription_request_id,
                start,
                end,
            } => events.push((request_id, subscription_request_id, start, end)),
            SessionEvent::CloseSession(err) => {
                panic!("CloseSession が発行された: {err:?}")
            }
            _ => {}
        }
    }
    events
}

/// fill fetch stream のイベントが解決済みの fill range と購読の Request ID を運ぶこと
///
/// draft-ietf-moq-transport-22 §3.4 (Fill Semantics): fill range は内側の LOCATION_FILTER が
/// 決め、終端は Largest Object を超えない。Relative Start の StartGroup=1 は現在の Group の
/// 先頭 (`{Largest Group, 0}`) を指す。REQUEST_UPDATE 起因の fill では `request_id` が
/// REQUEST_UPDATE 自身の Request ID になり、対象 subscription の Request ID とは一致しない
/// (§6.4.2.1 (Request ID))。
#[test]
fn fill_stream_event_carries_the_resolved_range() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    let mut inner = MessageParameters::new();
    inner.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LocationFilter(LocationFilter::RelativeGroup {
            start_group: 1,
        }),
    });
    let update_rid = send_update_to_server(&mut client, &mut server, sub_rid, fill_params(inner));
    let events = drain_open_fill_stream_events(&mut server);
    assert_eq!(events.len(), 1, "fill stream が 1 本開くこと: {events:?}");
    let (request_id, subscription_request_id, start, end) = events[0];
    assert_eq!(
        request_id, update_rid,
        "FETCH_HEADER には起因メッセージの Request ID を載せること"
    );
    assert_eq!(
        subscription_request_id, sub_rid,
        "fill 対象 subscription の Request ID を別に運ぶこと"
    );
    assert_eq!(
        (start.group_id, start.object_id),
        (0, 0),
        "StartGroup=1 は現在の Group の先頭を指すこと"
    );
    assert!(end >= start, "fill range が空でないこと: {end:?}");
}

/// unpaused (PAUSED=0) で FILL_PARAMETERS 付き REQUEST_UPDATE を処理すると fill stream が開く
/// (draft-ietf-moq-transport-22 §3.4.1)
#[test]
fn request_update_with_fill_opens_fill_stream() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    // 内側パラメータなし = subscription の設定で fill する (track 全体)
    let update_rid = send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(
        drain_open_fill_events(&mut server),
        vec![update_rid],
        "起因 REQUEST_UPDATE の Request ID で fill stream を開くこと"
    );
}

/// PAUSED 1 件を持つ MessageParameters を作る
fn paused_params(value: u8) -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_PAUSED,
        value: MessageParameterValue::Uint8(value),
    });
    params
}

/// FILL_PARAMETERS と PAUSED を併せ持つ MessageParameters を作る
fn fill_params_with_paused(inner: MessageParameters, paused: u8) -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_PAUSED,
        value: MessageParameterValue::Uint8(paused),
    });
    params.push(MessageParameter {
        param_type: PARAM_FILL_PARAMETERS,
        value: MessageParameterValue::FillParameters(inner),
    });
    params
}

/// 2 本目の SUBSCRIBE を PAUSED 値つきで established にする
///
/// 1 本目で Largest Object を確定させた状態を前提とし、track alias は 2 を使う。
fn subscribe_paused_and_ok(client: &mut Session, server: &mut Session, paused: u8) -> u64 {
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), paused_params(paused))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(sub_rid, 2, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    sub_rid
}

/// paused な購読への REQUEST_UPDATE は FILL を持っても、PAUSED 省略なら fill を開かない
///
/// draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter): REQUEST_UPDATE で PAUSED を
/// 省略した場合は現在の状態を維持する。paused のままなので §3.4.1 (Fill Semantics) の
/// "FILL_PARAMETERS carried while the subscription is paused opens no fill fetch stream."
/// に従い開設しない。
#[test]
fn request_update_with_fill_and_omitted_paused_stays_paused() {
    let (mut client, mut server, _sub_rid) = establish_sub_with_object();
    let paused_rid = subscribe_paused_and_ok(&mut client, &mut server, 1);
    send_update_to_server(
        &mut client,
        &mut server,
        paused_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(
        drain_open_fill_events(&mut server),
        Vec::<u64>::new(),
        "PAUSED 省略で paused を維持する場合、FILL 付き UPDATE でも開設しないこと"
    );
}

/// paused な購読への FILL + PAUSED=0 の REQUEST_UPDATE は再開して fill を開く
///
/// draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter): PAUSED=0 は unpaused を明示する。
/// 再開後の購読は配送対象になり、§3.4.1 (Fill Semantics) に従って fill fetch stream を開く。
#[test]
fn request_update_with_fill_and_unpaused_resumes_and_opens_fill_stream() {
    let (mut client, mut server, _sub_rid) = establish_sub_with_object();
    let paused_rid = subscribe_paused_and_ok(&mut client, &mut server, 1);
    let update_rid = send_update_to_server(
        &mut client,
        &mut server,
        paused_rid,
        fill_params_with_paused(MessageParameters::new(), 0),
    );
    assert_eq!(
        drain_open_fill_events(&mut server),
        vec![update_rid],
        "PAUSED=0 で再開した場合は fill stream を開くこと"
    );
}

/// unpaused な購読への FILL + PAUSED=1 の REQUEST_UPDATE は fill を開かない
///
/// draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter): 同じ REQUEST_UPDATE で
/// paused にする場合、fill を開設してはならない (§3.4.1 (Fill Semantics))。
#[test]
fn request_update_with_fill_and_pause_opens_nothing() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params_with_paused(MessageParameters::new(), 1),
    );
    assert_eq!(
        drain_open_fill_events(&mut server),
        Vec::<u64>::new(),
        "同じ UPDATE で paused にする場合は fill stream を開設しないこと"
    );
}

/// FILL 内側の Location Filter Type 0x00 (None) は subscription filter を継承せず
/// track 全体を指す (draft-ietf-moq-transport-22 §3.4 (Fill Semantics))
///
/// subscription filter を Next Group (`RelativeGroup { start_group: 0 }`) にすると
/// Largest `{0, 0}` に対して fill range が空になり開設されないため、内側 Type 0x00 が
/// subscription filter の継承 (`Unchanged`) ではなく track 全体 (`Removed`) として
/// 扱われたことをこの対比で判別できる。
#[test]
fn request_update_with_fill_no_filter_overrides_subscription_filter() {
    let (mut client, mut server, sub_rid) =
        establish_filtered_sub_with_object(&LocationFilter::RelativeGroup { start_group: 0 });
    let update_rid = send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(inner_with_location_filter(&LocationFilter::NoFilter)),
    );
    assert_eq!(
        drain_open_fill_events(&mut server),
        vec![update_rid],
        "内側 Type 0x00 は track 全体として fill stream を開くこと"
    );
}

/// FILL 内側を省略した場合は subscription filter を継承する
/// (draft-ietf-moq-transport-22 §3.4 (Fill Semantics))
///
/// 直前のテストと同じ subscription filter で内側を省略すると、継承した Next Group が
/// Largest `{0, 0}` より後ろになり開設されない。内側省略と Type 0x00 が別扱いであることを固定する。
#[test]
fn request_update_with_fill_omitted_inner_inherits_subscription_filter() {
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
        "内側省略時は subscription filter を継承し fill range が空なら開設しないこと"
    );
}

/// fill range が Largest Object より後に始まる場合は fill stream を開かない
/// (draft-ietf-moq-transport-22 §3.4)
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
/// (draft-ietf-moq-transport-22 §3.4.1)
#[test]
fn request_update_without_fill_opens_nothing() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_PAUSED,
        value: MessageParameterValue::Uint8(0),
    });
    send_update_to_server(&mut client, &mut server, sub_rid, params);
    assert_eq!(
        drain_open_fill_events(&mut server),
        Vec::<u64>::new(),
        "FILL なし REQUEST_UPDATE では新規 fill stream を開いてはならない"
    );
}

/// FILL_PARAMETERS 付き REQUEST_UPDATE ごとに新規 fill stream が開き、既存は残る
/// (draft-ietf-moq-transport-22 §3.4: 暗黙キャンセルしない)
#[test]
fn second_request_update_with_fill_opens_another_stream() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    let update_rid1 = send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![update_rid1]);
    // 1 本目を実際に開設する
    server
        .send_fill_fetch_header(DataStreamId(100), update_rid1)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.open_outgoing_fill_stream_count(sub_rid), 1);
    // 2 回目の FILL 付き UPDATE でも新規 fill stream が開き、1 本目は残る
    let update_rid2 = send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(
        drain_open_fill_events(&mut server),
        vec![update_rid2],
        "2 回目の FILL 付き UPDATE でも新規 fill stream が開くこと"
    );
    server
        .send_fill_fetch_header(DataStreamId(101), update_rid2)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server.open_outgoing_fill_stream_count(sub_rid),
        2,
        "fill stream は複数本が同時に存在できること"
    );
}

/// fill stream が open 中は PUBLISH_DONE を送れない
/// (draft-ietf-moq-transport-22 §9.9 の MUST NOT が fill を含むことの裏付け)
#[test]
fn publish_done_rejected_while_fill_stream_open() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    let update_rid = send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![update_rid]);
    server
        .send_fill_fetch_header(DataStreamId(100), update_rid)
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
/// (draft-ietf-moq-transport-22 §9.20.15)
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
        param_type: PARAM_PAUSED,
        value: MessageParameterValue::Uint8(0),
    });
    send_update_to_server(&mut client, &mut server, sub_rid, params);
    let merged = take_request_update_params(&mut server);
    assert!(
        merged.fill_parameters().is_none(),
        "FILL は後続 UPDATE に引き継がれてはならない"
    );
}

/// fill fetch stream の header 登録・ Object 通知・終端と Stream Count の計上
/// (draft-ietf-moq-transport-22 §3.4 / §9.9 / §11.4.1)
#[test]
fn fill_fetch_header_object_close_flow() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    let update_rid = send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![update_rid]);

    // 未知の Request ID では登録できない
    let err = server
        .send_fill_fetch_header(DataStreamId(100), 999)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);

    // fill stream を開く (FETCH_HEADER に載せた起因 REQUEST_UPDATE の Request ID で登録する)
    let stream_id = DataStreamId(100);
    server
        .send_fill_fetch_header(stream_id, update_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.open_outgoing_fill_stream_count(sub_rid), 1);
    server
        .send_fetch_object(
            stream_id,
            &shiguredo_moqt::message::common::Location {
                group_id: 0,
                object_id: 0,
            },
        )
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

/// subscriber は起因 REQUEST_UPDATE の Request ID を載せた fill 用 FETCH_HEADER を受理する
/// (draft-ietf-moq-transport-22 §3.4)。複数本の同時存在も許す。
#[test]
fn subscriber_accepts_fill_fetch_headers() {
    use shiguredo_moqt::stream::FETCH_HEADER_TYPE;

    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    let update_rid = send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![update_rid]);
    assert_ne!(
        update_rid, sub_rid,
        "fill 起因の REQUEST_UPDATE は購読とは別の Request ID を持つこと"
    );

    // fill stream 1 本目も 2 本目も受理する (同時存在可)。
    // FETCH_HEADER には購読の Request ID ではなく起因 REQUEST_UPDATE の Request ID を載せる。
    for stream_no in [200u64, 201u64] {
        let stream_id = DataStreamId(stream_no);
        client
            .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
            .expect("テストフィクスチャの前提条件を満たす");
        client
            .recv_fetch_header(
                stream_id,
                &FetchHeader {
                    request_id: update_rid,
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
    assert_eq!(err.code, SESSION_ROLE_VIOLATION);
    // キャンセル由来 (publish_done なし) の Terminated subscription を指す FETCH_HEADER は拒否する。
    // 起因 REQUEST_UPDATE の Request ID からも購読へ解決できることの確認を兼ねる。
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
                request_id: update_rid,
            },
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_UNEXPECTED_MESSAGE);
}

/// 未知 Request ID の FETCH_HEADER は再試行可能な結果を返し、セッションを閉じない
///
/// draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams): FILL_PARAMETERS
/// 付き PUBLISH を送った publisher は初回 fill の fill fetch stream を開く。PUBLISH の bidi
/// stream と fill の uni stream の到着順序は保証されないため、PUBLISH の処理前に届いた初回
/// fill は「要求が未登録」として保留・再通知できなければならない。§11.4.1 (Fetch Header) は
/// 未知の Request ID の扱いを MUST で定めていないため、セッションを閉じる必要はない。
#[test]
fn fill_fetch_header_with_unknown_request_id_is_retryable() {
    use shiguredo_moqt::session::types::FetchHeaderAcceptance;
    use shiguredo_moqt::stream::FETCH_HEADER_TYPE;

    let (mut client, _server) = establish_pair();
    let stream_id = DataStreamId(202);
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    let acceptance = client
        .recv_fetch_header(stream_id, &FetchHeader { request_id: 999 })
        .expect("未知の Request ID でもセッションを閉じないこと");
    assert_eq!(
        acceptance,
        FetchHeaderAcceptance::RequestNotRegistered,
        "未知の Request ID は再試行可能な結果として返すこと"
    );
    assert_eq!(
        client.state(),
        SessionState::Established,
        "先着した初回 fill でセッションを終了しないこと"
    );
    // 受理しなかったため、stream の登録 (受信 data stream) は残ったままである。
    // 終端通知が通ることで、caller が保留して再通知できる状態を保っていることを確認する
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("再試行待ちの stream の終端通知が通ること");
}

/// subscription のキャンセル時は open 中の fill fetch stream を reset する
/// (draft-ietf-moq-transport-22 §3.4.1)
///
/// draft-ietf-moq-transport-22 §6.4.2.3 (Request Cancellation and Rejection) は cancel を
/// RESET_STREAM / STOP_SENDING と定める。FIN は「その方向にもうメッセージを送らない」
/// ことだけを示し cancel ではないため、キャンセルは RESET_STREAM で表現する。
#[test]
fn cancel_subscription_resets_open_fill_streams() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    let update_rid = send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![update_rid]);
    server
        .send_fill_fetch_header(DataStreamId(100), update_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.open_outgoing_fill_stream_count(sub_rid), 1);

    // bidi request stream の RESET_STREAM で subscription をキャンセルする
    server
        .recv_request_stream_closed(
            sub_rid,
            RequestStreamEnd::Reset {
                error_code: Some(shiguredo_moqt::error::STREAM_CANCELLED),
                reliable_size: None,
            },
        )
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

/// PUBLISH 起点で自側が publisher の subscription は、peer の終端 (FIN) で終端せず
/// open 中の fill fetch stream も reset しない
///
/// draft-ietf-moq-transport-22 §6.4.2.2 (Graceful Request Stream Closure): "A FIN only indicates
/// that an endpoint will send no further messages in that direction; it is not a request
/// cancellation." §3.4.1 (Opening and Closing Fill Fetch Streams) の reset の MUST は cancel
/// (STOP_SENDING / RESET_STREAM) に対するものであり、購読が継続する限り fill の配送も継続する。
/// open 中の outgoing stream が残っている間は §9.9 (PUBLISH_DONE) の MUST NOT により
/// PUBLISH_DONE を送れないため、stream を閉じた後に PUBLISH_DONE を送る。
#[test]
fn publish_origin_peer_fin_keeps_open_fill_streams() {
    let (mut client, mut server) = establish_pair();
    let pub_rid = establish_publish_sender_as_established(&mut client, &mut server, 910);
    let (subgroup_stream, fill_stream) = open_fill_for_publish_sender(&mut server, pub_rid, 910);

    // peer (subscriber) の FIN では購読を終端せず、open 中の fill stream も reset しない
    server
        .recv_request_stream_closed(pub_rid, RequestStreamEnd::Fin)
        .expect("peer の終端通知を受理すること");
    assert_eq!(
        server
            .subscription(pub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::SubscriptionState::Established,
        "PUBLISH を送った側は peer FIN で Terminated にならないこと"
    );
    assert_eq!(
        server.open_outgoing_fill_stream_count(pub_rid),
        1,
        "peer FIN では open 中の fill stream を reset しないこと"
    );
    let mut saw_reset = false;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::ResetDataStream { .. }) {
            saw_reset = true;
        }
    }
    assert!(!saw_reset, "peer FIN では ResetDataStream を発行しないこと");

    // open 中の outgoing stream が残っている間は PUBLISH_DONE を送れない (§9.9 の MUST NOT)
    let err = server
        .send_publish_done(
            pub_rid,
            0x2,
            2,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("正当な reason phrase である"),
        )
        .expect_err("open 中の outgoing stream がある間は PUBLISH_DONE を送れないこと");
    assert_eq!(
        err.code, SESSION_PROTOCOL_VIOLATION,
        "拒否理由は SESSION_PROTOCOL_VIOLATION であること"
    );
    assert_eq!(
        server
            .subscription(pub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::SubscriptionState::Established,
        "拒否された PUBLISH_DONE の送信で subscription の状態を変えないこと"
    );

    // subgroup stream と fill fetch stream を閉じた後に PUBLISH_DONE を送れる
    server
        .send_data_stream_closed(subgroup_stream, RequestStreamEnd::Fin)
        .expect("subgroup stream を終端できること");
    server
        .send_fetch_data_stream_closed(fill_stream)
        .expect("fill fetch stream を終端できること");
    assert_eq!(server.open_outgoing_fill_stream_count(pub_rid), 0);
    // published_count は subgroup と fill fetch の 2 本である
    server
        .send_publish_done(
            pub_rid,
            0x2,
            2,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("正当な reason phrase である"),
        )
        .expect("全 outgoing stream の終端後は PUBLISH_DONE を送れること");
    let (_, done_msg, fin) = take_send_on_stream_with_fin(&mut server);
    assert!(matches!(done_msg, ControlMessage::PublishDone(_)));
    assert!(fin, "PUBLISH_DONE は FIN で送られること");
    let mut got_terminated = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind: RequestKind::Publish,
            reason: TerminationReason::PeerStreamFin,
        } = e
        {
            assert_eq!(request_id, pub_rid);
            got_terminated = true;
        }
    }
    assert!(
        got_terminated,
        "PUBLISH_DONE の送信時点で RequestTerminated(PeerStreamFin) が発行されること"
    );
}

/// PUBLISH 起点 publisher 役の subscription で fill fetch stream を 1 本開く
///
/// 戻り値は (subgroup stream の DataStreamId, fill fetch stream の DataStreamId)。
/// fill の開設判定には観測済み Largest Object が要るため、publisher が 1 件公開してから
/// REQUEST_UPDATE (FILL_PARAMETERS) を注入する。購読は `Established` であること。
fn open_fill_for_publish_sender(
    server: &mut Session,
    pub_rid: u64,
    track_alias: u64,
) -> (DataStreamId, DataStreamId) {
    let subgroup_stream = DataStreamId(401);
    let fill_stream = DataStreamId(400);
    server
        .send_subgroup_header(
            subgroup_stream,
            pub_rid,
            &SubgroupHeader {
                track_alias,
                group_id: 0,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(subgroup_stream, 0, None)
        .expect("テストフィクスチャの前提条件を満たす");
    // PUBLISH 起点の REQUEST_UPDATE は subscriber 側の採番で届く (§6.4.2.1 (Request ID))
    let update_rid = pub_rid + 1;
    server
        .recv_stream_message(
            pub_rid,
            ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
                request_id: update_rid,
                parameters: fill_params(MessageParameters::new()),
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(drain_open_fill_events(server), vec![update_rid]);
    server
        .send_fill_fetch_header(fill_stream, update_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server.open_outgoing_fill_stream_count(pub_rid),
        1,
        "fill fetch stream が 1 本開いていること"
    );
    (subgroup_stream, fill_stream)
}

/// PUBLISH 起点 publisher 役で自側 REQUEST_UPDATE の失敗応答を受けると open 中の
/// fill fetch stream を reset し、残る subgroup stream の終端で PUBLISH_DONE を送る
///
/// draft-ietf-moq-transport-22 §3.4.1 (Opening and Closing Fill Fetch Streams) は
/// subscription の終了時に open 中の fill fetch stream の reset を MUST とする。
/// §9.9 (PUBLISH_DONE) の MUST NOT (全 stream を閉じるまで送ってはならない) により
/// PUBLISH_DONE は保留され、最後の outgoing stream の終端で送られる (§9.5.1 の MUST)。
#[test]
fn publish_origin_update_failed_resets_open_fill_streams_and_flushes_publish_done() {
    use shiguredo_moqt::error::{PUBLISH_DONE_UPDATE_FAILED, REQUEST_INVALID_FILTER};
    use shiguredo_moqt::message::RequestError;

    let (mut client, mut server) = establish_pair();
    let pub_rid = establish_publish_sender_as_established(&mut client, &mut server, 404);
    let (subgroup_stream, fill_stream) = open_fill_for_publish_sender(&mut server, pub_rid, 404);

    // 自側 REQUEST_UPDATE を送り、その失敗応答 (REQUEST_ERROR) で購読が終端する。
    // open 中の fill fetch stream は §3.4.1 の MUST により reset される
    server
        .send_request_update(pub_rid, MessageParameters::new())
        .expect("REQUEST_UPDATE を送れること");
    let (_, update_msg, _) = take_send_on_stream_with_fin(&mut server);
    assert!(matches!(update_msg, ControlMessage::RequestUpdate(_)));
    server
        .recv_stream_message(
            pub_rid,
            ControlMessage::RequestError(RequestError {
                error_code: REQUEST_INVALID_FILTER,
                retry_interval: 0,
                reason: ReasonPhrase::new("rejected").expect("正当な reason phrase である"),
                error_details: ErrorDetails::new(),
            }),
        )
        .expect("REQUEST_ERROR を受理すること");
    assert_eq!(
        server
            .subscription(pub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::SubscriptionState::Terminated,
        "REQUEST_ERROR の受信で Terminated になること"
    );
    assert_eq!(
        server.open_outgoing_fill_stream_count(pub_rid),
        0,
        "open 中の fill fetch stream を reset すること"
    );
    let mut saw_fill_reset = false;
    let mut flushed_early = false;
    while let Some(e) = server.poll_event() {
        match e {
            SessionEvent::ResetDataStream { stream_id, .. } if stream_id == fill_stream => {
                saw_fill_reset = true;
            }
            SessionEvent::SendOnStream {
                request_id,
                message: ControlMessage::PublishDone(_),
                ..
            } if request_id == pub_rid => flushed_early = true,
            _ => {}
        }
    }
    assert!(
        saw_fill_reset,
        "fill fetch stream の ResetDataStream が発行されること"
    );
    // subgroup stream が open のため PUBLISH_DONE は保留される (§9.9 の MUST NOT)
    assert!(
        !flushed_early,
        "subgroup stream が open の間は PUBLISH_DONE を送らないこと"
    );
    assert!(
        server
            .subscription(pub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .pending_publish_done
            .is_some(),
        "PUBLISH_DONE が保留されていること"
    );

    // peer (subscriber) の FIN では終端しない (自側の PUBLISH_DONE が未送信のため)
    server
        .recv_request_stream_closed(pub_rid, RequestStreamEnd::Fin)
        .expect("peer の FIN を受理すること");
    let mut terminated_before_publish_done = false;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::RequestTerminated { .. }) {
            terminated_before_publish_done = true;
        }
    }
    assert!(
        !terminated_before_publish_done,
        "保留 PUBLISH_DONE が残っている間は peer FIN で終端しないこと"
    );

    // 最後の outgoing stream を閉じた時点で保留 PUBLISH_DONE が flush され、
    // peer FIN と揃って request の終端が確定する
    server
        .send_data_stream_closed(subgroup_stream, RequestStreamEnd::Fin)
        .expect("subgroup stream を終端できること");
    let (_, msg, fin) = take_send_on_stream_with_fin(&mut server);
    let ControlMessage::PublishDone(done) = msg else {
        panic!("PUBLISH_DONE が送られること");
    };
    assert_eq!(done.status_code, PUBLISH_DONE_UPDATE_FAILED);
    assert!(fin, "PUBLISH_DONE は FIN で送られること");
    let mut got_terminated = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind: RequestKind::Publish,
            reason: TerminationReason::PeerStreamFin,
        } = e
        {
            assert_eq!(request_id, pub_rid);
            got_terminated = true;
        }
    }
    assert!(
        got_terminated,
        "保留 PUBLISH_DONE の flush で RequestTerminated(PeerStreamFin) が発行されること"
    );
}

/// PUBLISH 起点 publisher 役でも peer の RESET_STREAM (cancel) では open 中の
/// fill fetch stream を reset する
///
/// draft-ietf-moq-transport-22 §3.4.1 (Opening and Closing Fill Fetch Streams): "When the
/// subscription is cancelled, the publisher MUST reset any open fill fetch streams."
#[test]
fn publish_origin_peer_reset_resets_open_fill_streams() {
    let (mut client, mut server) = establish_pair();
    let pub_rid = establish_publish_sender_as_established(&mut client, &mut server, 403);
    let (_subgroup_stream, fill_stream) = open_fill_for_publish_sender(&mut server, pub_rid, 403);

    server
        .recv_request_stream_closed(
            pub_rid,
            RequestStreamEnd::Reset {
                error_code: Some(42),
                reliable_size: None,
            },
        )
        .expect("peer の cancel を受理すること");
    assert_eq!(
        server
            .subscription(pub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        shiguredo_moqt::session::types::SubscriptionState::Terminated,
        "cancel では subscription を終端すること"
    );
    assert_eq!(
        server.open_outgoing_fill_stream_count(pub_rid),
        0,
        "cancel では open 中の fill stream を reset すること"
    );
    let mut saw_reset = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::ResetDataStream {
            stream_id,
            error_code,
            ..
        } = e
        {
            assert_eq!(stream_id, fill_stream);
            assert_eq!(error_code, STREAM_CANCELLED);
            saw_reset = true;
        }
    }
    assert!(
        saw_reset,
        "cancel では open 中の fill stream を reset すること"
    );
}

/// FILL 内側の Range Filter 不正は INVALID_FILTER で拒否し fill を開かない
/// (draft-ietf-moq-transport-22 §9.20.15 / §3.3.2)
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
                request_id: sub_rid + 2,
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

/// FILL 内側の LOCATION_FILTER が Location Filter 形式でない場合は fill stream を開かない
///
/// API 経由で手組みしたメッセージのみ到達する防御である (wire 経路では decode 層が
/// PROTOCOL_VIOLATION で拒否する)。不正値を fill range として解釈せず安全側に倒すことと、
/// その場合もセッションを閉じないことを固定する。
#[test]
fn request_update_with_non_location_filter_inner_opens_nothing() {
    use shiguredo_moqt::message::RequestUpdate;

    let (_client, mut server, sub_rid) = establish_sub_with_object();
    let mut inner = MessageParameters::new();
    inner.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x00]),
    });
    server
        .recv_stream_message(
            sub_rid,
            ControlMessage::RequestUpdate(RequestUpdate {
                request_id: sub_rid + 2,
                parameters: fill_params(inner),
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        drain_open_fill_events(&mut server),
        Vec::<u64>::new(),
        "Location Filter 形式でない内側 filter では fill stream を開かないこと"
    );
    assert_eq!(
        server.state(),
        SessionState::Established,
        "API 経由の不正 filter でセッションは閉じないこと"
    );
}

/// Table 7 外の内側パラメータを持つ FILL の送信は送信前に拒否し副作用を残さない
/// (draft-ietf-moq-transport-22 §9.20.15)
#[test]
fn send_with_out_of_scope_inner_fill_rejected_without_side_effects() {
    use shiguredo_moqt::message_parameter::PARAM_PAUSED as INNER_PAUSED;

    // 内側に PAUSED (Table 7 外) を持つ FILL を作る
    let mut inner = MessageParameters::new();
    inner.push(MessageParameter {
        param_type: INNER_PAUSED,
        value: MessageParameterValue::Uint8(1),
    });
    let params = fill_params(inner);

    // SUBSCRIBE 送信は request_id 発行・状態登録より前に拒否する
    let (mut client, _server) = establish_pair();
    let err = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), params.clone())
        .expect_err("Table 7 外の内側パラメータは送信前に拒否されること");
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
        .expect_err("Table 7 外の内側パラメータは送信前に拒否されること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert!(
        !client
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .paused,
        "拒否した UPDATE で paused は変わらないこと"
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
/// (draft-ietf-moq-transport-22 §3.4.1)
#[test]
fn peer_stop_sending_on_fill_absorbed() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    let update_rid = send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![update_rid]);
    let stream_id = DataStreamId(100);
    server
        .send_fill_fetch_header(stream_id, update_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .recv_data_stream_stop_sending(stream_id, None)
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
/// (draft-ietf-moq-transport-22 §3.4.1 の失敗時即 reset 経路)
#[test]
fn reset_single_fill_stream_emits_reset_event() {
    use shiguredo_moqt::session::types::DataStreamResetReason;

    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    let update_rid = send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![update_rid]);
    let stream_id = DataStreamId(100);
    server
        .send_fill_fetch_header(stream_id, update_rid)
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
/// (draft-ietf-moq-transport-22 §3.4)
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
    let update_rid = send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(
        drain_open_fill_events(&mut server),
        vec![update_rid],
        "subscription filter が範囲内なら内側省略の fill が開くこと"
    );
}

/// 同一 UPDATE での filter 変更は fill 評価に反映される (処理後観点)
/// (draft-ietf-moq-transport-22 §3.4)
#[test]
fn fill_evaluated_with_updated_filter_in_same_update() {
    // unfiltered で確立 → 同一 UPDATE で範囲外 filter + FILL → 開かない
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LocationFilter(LocationFilter::AbsoluteStart {
            start: Location {
                group_id: 5,
                object_id: 0,
            },
        }),
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
/// (draft-ietf-moq-transport-22 §3.4)
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
    let update_rid = send_update_to_server(&mut client, &mut server, sub_rid, fill_params(inner));
    assert_eq!(
        drain_open_fill_events(&mut server),
        vec![update_rid],
        "飽和した開始位置の fill は開設されること"
    );
}

/// 複数本の fill stream はキャンセル時にまとめて reset される
/// (draft-ietf-moq-transport-22 §3.4.1)
///
/// キャンセルは draft-ietf-moq-transport-22 §6.4.2.3 (Request Cancellation and Rejection) に
/// 従い RESET_STREAM で表現する (FIN は cancel ではない)。
#[test]
fn cancel_subscription_resets_all_open_fill_streams() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    let mut update_rids = Vec::new();
    for _ in 0..2 {
        let update_rid = send_update_to_server(
            &mut client,
            &mut server,
            sub_rid,
            fill_params(MessageParameters::new()),
        );
        assert_eq!(drain_open_fill_events(&mut server), vec![update_rid]);
        update_rids.push(update_rid);
    }
    server
        .send_fill_fetch_header(DataStreamId(100), update_rids[0])
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_fill_fetch_header(DataStreamId(101), update_rids[1])
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(server.open_outgoing_fill_stream_count(sub_rid), 2);
    server
        .recv_request_stream_closed(
            sub_rid,
            RequestStreamEnd::Reset {
                error_code: Some(shiguredo_moqt::error::STREAM_CANCELLED),
                reliable_size: None,
            },
        )
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
/// (draft-ietf-moq-transport-22 §3.4.1 / §9.5.1)
#[test]
fn request_error_resets_fills_and_sends_publish_done() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    let update_rid = send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![update_rid]);
    server
        .send_fill_fetch_header(DataStreamId(100), update_rid)
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
            ErrorDetails::new(),
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

/// PUBLISH 起点 subscription を Established にし、server 側で {0, 0} を公開して
/// Largest {0, 0} を確定させる
///
/// request stream 上の GOAWAY は request の receiver だけが送れる
/// (draft-ietf-moq-transport-23 §9.2 (GOAWAY)) ため、PUBLISH の receiver である subscriber
/// (client) 側から GOAWAY を送るテストの前提を作る。戻り値は PUBLISH の Request ID。
fn establish_publish_with_object() -> (Session, Session, u64) {
    let (mut client, mut server) = establish_pair();
    let sub_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            1,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, publish_msg) = take_send_request(&mut server);
    client
        .recv_request(publish_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .send_request_ok(sub_rid, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut client);
    server
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
                group_id: 0,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 0, None)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server, sub_rid)
}

/// GOAWAY の deadline 満了で終端した subscription への遅延 fill fetch stream は受理する
///
/// request stream は方向ごとに独立に閉じるため (draft-ietf-moq-transport-22 §6.4.2.2
/// (Graceful Request Stream Closure))、自側が送信方向を reset しても peer の送信方向は
/// 開いたままである。peer が GOAWAY / reset を観測する前に開いた fill fetch stream は
/// 正当に届くため、session を `PROTOCOL_VIOLATION` で閉じてはならない。
/// GOAWAY を送るのは request の receiver である subscriber (client) であり、
/// PUBLISH 起点 subscription (§9.8) で fill stream の受信側でもある。
#[test]
fn subscriber_accepts_late_fill_fetch_header_after_goaway_timeout() {
    use shiguredo_moqt::stream::FETCH_HEADER_TYPE;

    let (mut client, mut server, sub_rid) = establish_publish_with_object();
    let update_rid = send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![update_rid]);

    // subscriber (client) が request stream GOAWAY を送り、deadline 満了で終端する
    client.tick(1_000);
    client
        .send_goaway_on_request_stream(sub_rid, Vec::new(), 100)
        .expect("request stream GOAWAY の送信に成功すること");
    client.tick(1_100);
    let mut terminations = 0;
    while let Some(e) = client.poll_event() {
        if matches!(e, SessionEvent::RequestTerminated { .. }) {
            terminations += 1;
        }
    }
    assert_eq!(terminations, 1, "期限到達で subscription が終端すること");

    // peer が reset を観測する前に開いた fill stream は遅延して届く
    let stream_id = DataStreamId(210);
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_fetch_header(
            stream_id,
            &FetchHeader {
                request_id: update_rid,
            },
        )
        .expect("終端済み subscription への遅延 fill FETCH_HEADER が受理されること");
    assert_eq!(
        client.state(),
        SessionState::Established,
        "遅延 fill stream でセッションを閉じないこと"
    );
    // FIN での終端は subscription に影響せず吸収する
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("遅延 fill stream の終端通知が吸収されること");
    assert_eq!(client.state(), SessionState::Established);

    // アプリが先に subscription を破棄した場合も、遅延 fill stream の終端通知は
    // unknown stream id のエラーにしない (破棄対象 stream として吸収する)。
    // peer の close 通知後に受理した stream でも同じであることは、
    // 後続の `subscriber_accepts_late_fill_fetch_header_after_peer_close` で確認する
    let another = DataStreamId(211);
    client
        .recv_data_stream_type(another, FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_fetch_header(
            another,
            &FetchHeader {
                request_id: update_rid,
            },
        )
        .expect("遅延 fill FETCH_HEADER が受理されること");
    assert!(
        client.forget_subscription(sub_rid).is_some(),
        "終端済み subscription を破棄できること"
    );
    client
        .recv_data_stream_closed(another, RequestStreamEnd::Fin)
        .expect("破棄後の遅延 fill stream の終端通知が吸収されること");
    assert_eq!(client.state(), SessionState::Established);
}

/// peer の bidi close 通知が先に届いても、遅延した fill fetch stream は受理する
///
/// fill fetch stream は uni stream であり、bidi request stream の close 通知との到着順に
/// 保証がない。close 通知で `rejected_request_ids` から id が消えても、自側が request stream
/// の GOAWAY を送った request への遅延 fill stream は受理する。
/// GOAWAY を送るのは request の receiver である subscriber (client) であり、
/// PUBLISH 起点 subscription (§9.8) で fill stream の受信側でもある。
#[test]
fn subscriber_accepts_late_fill_fetch_header_after_peer_close() {
    use shiguredo_moqt::stream::FETCH_HEADER_TYPE;

    let (mut client, mut server, sub_rid) = establish_publish_with_object();
    let update_rid = send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![update_rid]);

    client.tick(1_000);
    client
        .send_goaway_on_request_stream(sub_rid, Vec::new(), 100)
        .expect("request stream GOAWAY の送信に成功すること");
    client.tick(1_100);
    while client.poll_event().is_some() {}
    // peer の bidi close 通知を先に処理する
    client
        .recv_request_stream_closed(sub_rid, RequestStreamEnd::Fin)
        .expect("終端済み request の FIN が no-op で吸収されること");

    let stream_id = DataStreamId(212);
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_fetch_header(
            stream_id,
            &FetchHeader {
                request_id: update_rid,
            },
        )
        .expect("close 通知後に届いた遅延 fill FETCH_HEADER が受理されること");
    assert_eq!(
        client.state(),
        SessionState::Established,
        "遅延 fill stream でセッションを閉じないこと"
    );

    // 破棄後の遅延 fill stream の終端通知も吸収する
    assert!(
        client.forget_subscription(sub_rid).is_some(),
        "終端済み subscription を破棄できること"
    );
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("破棄後の遅延 fill stream の終端通知が吸収されること");
    assert_eq!(client.state(), SessionState::Established);
}

/// fill fetch stream の Object は effective fill range の外側なら拒否されること
///
/// draft-ietf-moq-transport-22 §3.4 (Fill Semantics) の fill range は
/// FILL_PARAMETERS の LOCATION_FILTER と継承 filter から解決する。既定 (FILL なし) は
/// Track 全体 (先頭から Largest Object まで) であり、Largest を超える Location は拒否する。
#[test]
fn fill_object_outside_effective_range_is_rejected() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object();
    let update_rid = send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![update_rid]);
    let stream_id = DataStreamId(102);
    server
        .send_fill_fetch_header(stream_id, update_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = server
        .send_fetch_object(
            stream_id,
            &shiguredo_moqt::message::common::Location {
                group_id: 1,
                object_id: 0,
            },
        )
        .expect_err("fill range 外の Location は拒否されること");
    assert_eq!(
        err,
        shiguredo_moqt::session::types::SendRequestError::LocalFilterMismatch
    );
    assert_eq!(
        server.state(),
        SessionState::Established,
        "拒否してもセッションは閉じない"
    );
}

// ─── PUBLISH の FILL_PARAMETERS からの初回 fill ─────────────────
//
// draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams) は、publisher が
// FILL_PARAMETERS 付きの PUBLISH を送るときも初回 fill の fill fetch stream を開くと定める。
// 開設するのは publisher だけであり、PUBLISH を受けて PUBLISH_OK を返す subscriber は開かない。
// 初回 fill の FETCH_HEADER は subscription の Request ID (PUBLISH の Request ID) を載せる
// (§3.4 (Fill Semantics) / §6.4.2.1 (Request ID))。

/// publisher が FILL_PARAMETERS 付き PUBLISH を送ると初回 fill を 1 本開く
///
/// PUBLISH 自体は bidi request として送られ、初回 fill の開設指示はその後に積まれる
/// (I/O 層が PUBLISH を書いてから fill stream を開く順序を保ち、PUBLISH の処理前に fill が
/// 届く競合を減らすため)。fill range は Largest Object までにクランプされ (§3.4)、
/// 開いた fill fetch stream は PUBLISH の Request ID で登録できる。
#[test]
fn publish_with_fill_opens_initial_fill_stream() {
    let (_client, mut server, sub_rid) = establish_sub_with_object();
    // sub_rid の購読で server が {0, 0} を公開済みであり、Largest Object は {0, 0} である
    assert!(server.subscription(sub_rid).is_some());
    let pub_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            fill_params(MessageParameters::new()),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");

    let mut events = Vec::new();
    while let Some(e) = server.poll_event() {
        events.push(e);
    }
    let mut publish_index = None;
    let mut fill_indices = Vec::new();
    let mut fills = Vec::new();
    for (index, event) in events.iter().enumerate() {
        match event {
            SessionEvent::SendRequest {
                request_id,
                message,
            } => {
                assert_eq!(*request_id, pub_rid, "PUBLISH の Request ID で送ること");
                assert!(
                    matches!(message, ControlMessage::Publish(_)),
                    "PUBLISH が送信されること"
                );
                publish_index = Some(index);
            }
            SessionEvent::OpenFillFetchStream {
                request_id,
                subscription_request_id,
                start,
                end,
            } => {
                fill_indices.push(index);
                fills.push((*request_id, *subscription_request_id, *start, *end));
            }
            SessionEvent::CloseSession(err) => panic!("CloseSession が発行された: {err:?}"),
            _ => {}
        }
    }
    let publish_index = publish_index.expect("PUBLISH が送信されること");
    assert_eq!(
        fills.len(),
        1,
        "初回 fill は 1 本だけ開くこと (重複して開くと fill が二重に配信される)"
    );
    let fill_index = fill_indices[0];
    assert!(
        publish_index < fill_index,
        "PUBLISH を送ってから初回 fill を開くこと"
    );
    let (request_id, subscription_request_id, start, end) = fills[0];
    assert_eq!(
        request_id, pub_rid,
        "初回 fill の FETCH_HEADER には PUBLISH の Request ID を載せること"
    );
    assert_eq!(
        subscription_request_id, pub_rid,
        "fill 対象 subscription は PUBLISH が作る subscription であること"
    );
    assert_eq!(
        (start.group_id, start.object_id),
        (0, 0),
        "開始は Track の先頭であること"
    );
    assert_eq!(
        (end.group_id, end.object_id),
        (0, 0),
        "終端は Largest Object でクランプされること"
    );

    // 開いた fill fetch stream は PUBLISH の Request ID で登録でき、range 内の Object だけを
    // 送れる (§3.4 の fill range は Object 送出の上限でもある)
    let stream_id = DataStreamId(310);
    server
        .send_fill_fetch_header(stream_id, pub_rid)
        .expect("PUBLISH 起因の初回 fill stream を登録できること");
    server
        .send_fetch_object(
            stream_id,
            &shiguredo_moqt::message::common::Location {
                group_id: 0,
                object_id: 0,
            },
        )
        .expect("fill range 内の Object は送信できること");
    let err = server
        .send_fetch_object(
            stream_id,
            &shiguredo_moqt::message::common::Location {
                group_id: 1,
                object_id: 0,
            },
        )
        .expect_err("fill range 外の Object は拒否されること");
    assert_eq!(
        err,
        shiguredo_moqt::session::types::SendRequestError::LocalFilterMismatch
    );
    server
        .send_fetch_data_stream_closed(stream_id)
        .expect("fill stream の終端を通知できること");
    assert_eq!(server.state(), SessionState::Established);
}

/// PAUSED=1 の PUBLISH に FILL_PARAMETERS が付いても fill は開かない
///
/// draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams): "FILL_PARAMETERS
/// carried while the subscription is paused opens no fill fetch stream." paused な subscription
/// では live の Object も送らないため、fill だけを開く理由がない (§3.1.1)。
#[test]
fn publish_with_fill_and_paused_opens_nothing() {
    let (_client, mut server, _sub_rid) = establish_sub_with_object();
    let pub_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            fill_params_with_paused(MessageParameters::new(), 1),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    // PUBLISH は paused の初期状態を伝えるために送られる
    assert_eq!(take_send_request(&mut server).0, pub_rid);
    assert_eq!(
        drain_open_fill_events(&mut server),
        Vec::<u64>::new(),
        "paused な PUBLISH では fill stream を開いてはならない"
    );
}

/// FILL_PARAMETERS を持たない PUBLISH では fill を開かない
///
/// draft-ietf-moq-transport-23 §9.19.15 (FILL PARAMETERS Parameter): "A fill fetch stream is
/// opened only when FILL_PARAMETERS is present." PUBLISH のパラメータに FILL が無ければ、
/// Largest Object が既知でも開設しない。
#[test]
fn publish_without_fill_opens_nothing() {
    let (_client, mut server, _sub_rid) = establish_sub_with_object();
    server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        drain_open_fill_events(&mut server),
        Vec::<u64>::new(),
        "FILL_PARAMETERS が無ければ fill stream を開かないこと"
    );
}

/// Largest Object が未知のまま FILL_PARAMETERS 付き PUBLISH を送っても fill は開かない
///
/// draft-ietf-moq-transport-23 §3.4 (Fill Semantics): fill range は Largest Object を超えられず、
/// 送れる Object が確定しないため開設しない。PUBLISH 自体は初期パラメータを伝えるために送る。
#[test]
fn publish_with_fill_and_no_largest_object_opens_nothing() {
    let (_client, mut server) = establish_pair();
    let pub_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            fill_params(MessageParameters::new()),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        server.subscription(pub_rid).is_some(),
        "PUBLISH は受理されること"
    );
    assert_eq!(take_send_request(&mut server).0, pub_rid);
    assert_eq!(
        drain_open_fill_events(&mut server),
        Vec::<u64>::new(),
        "Largest Object が未知なら fill stream を開いてはならない"
    );
}

/// 観測最大が無くても PUBLISH の LARGEST_OBJECT を Largest Object として fill を開く
///
/// draft-ietf-moq-transport-23 §3.4 (Fill Semantics): "The subscriber learns the Largest
/// Object from the LARGEST_OBJECT parameter in SUBSCRIBE_OK, PUBLISH or REQUEST_UPDATE_OK."
/// そのため publisher がまだ Object を観測していなくても (自側の観測最大が無くても)、
/// PUBLISH に LARGEST_OBJECT を載せれば subscriber は Largest Object を知り、fill range が
/// 定まる。終端は PUBLISH で通知した値であり、自側の観測値で置き換えてはならない。
#[test]
fn publish_with_fill_uses_publish_largest_object_as_fill_end() {
    let (_client, mut server) = establish_pair();
    // 観測済みの Object は無い。アプリが LARGEST_OBJECT だけを指定する
    let mut params = fill_params(MessageParameters::new());
    params.set_largest_object(2, 3);
    let pub_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            params,
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");

    let mut fills = Vec::new();
    while let Some(event) = server.poll_event() {
        match event {
            SessionEvent::OpenFillFetchStream {
                request_id,
                subscription_request_id,
                start,
                end,
            } => fills.push((request_id, subscription_request_id, start, end)),
            SessionEvent::CloseSession(err) => panic!("CloseSession が発行された: {err:?}"),
            _ => {}
        }
    }
    assert_eq!(
        fills.len(),
        1,
        "アプリ指定の LARGEST_OBJECT だけで初回 fill を 1 本開くこと"
    );
    let (request_id, subscription_request_id, start, end) = fills[0];
    assert_eq!(
        request_id, pub_rid,
        "初回 fill の FETCH_HEADER には PUBLISH の Request ID を載せること"
    );
    assert_eq!(
        subscription_request_id, pub_rid,
        "fill 対象 subscription は PUBLISH が作る subscription であること"
    );
    assert_eq!(
        (start.group_id, start.object_id),
        (0, 0),
        "開始は Track の先頭であること"
    );
    assert_eq!(
        (end.group_id, end.object_id),
        (2, 3),
        "終端は PUBLISH で通知した LARGEST_OBJECT であること"
    );
}

/// Table 7 外の内側パラメータを持つ FILL 付き PUBLISH は送信前に拒否し副作用を残さない
///
/// draft-ietf-moq-transport-23 §9.19.15 (FILL PARAMETERS Parameter) の Table 7 は内側に
/// 置けるパラメータを列挙し、それ以外は INVALID_PARAMETER でセッションを閉じる MUST。
/// PUBLISH 送信側は request_id 発行・状態登録より前に拒否し、孤児状態を作らない。
#[test]
fn publish_with_out_of_scope_inner_fill_rejected_without_side_effects() {
    let mut inner = MessageParameters::new();
    inner.push(MessageParameter {
        param_type: PARAM_PAUSED,
        value: MessageParameterValue::Uint8(1),
    });
    let (_client, mut server) = establish_pair();
    let err = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            fill_params(inner),
            TrackProperties::new(),
        )
        .expect_err("Table 7 外の内側パラメータは送信前に拒否されること");
    assert_eq!(
        err.as_session_error()
            .expect("セッションエラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    assert_eq!(
        server.subscriptions().count(),
        0,
        "拒否した PUBLISH は登録されてはならない"
    );
    while let Some(e) = server.poll_event() {
        assert!(
            !matches!(
                e,
                SessionEvent::SendRequest { .. }
                    | SessionEvent::OpenFillFetchStream { .. }
                    | SessionEvent::CloseSession(_)
            ),
            "拒否した送信で SendRequest / OpenFillFetchStream / CloseSession は発行されないこと"
        );
    }
}

/// INCLUDE_PROPERTIES は PUBLISH のパラメータではないため、送信前に拒否し fill も開かない
///
/// draft-ietf-moq-transport-23 §9.8 (PUBLISH) の Parameters の列挙に INCLUDE_PROPERTIES は無い。
/// fill のプロパティ条件は subscription 側の INCLUDE_PROPERTIES が担い、PUBLISH の
/// FILL_PARAMETERS からは制御しない (§9.19.15 の Table 7 にも含まれない)。
#[test]
fn publish_with_include_properties_is_rejected_without_side_effects() {
    use shiguredo_moqt::message_parameter::PARAM_INCLUDE_PROPERTIES;

    let mut params = fill_params(MessageParameters::new());
    params.push(MessageParameter {
        param_type: PARAM_INCLUDE_PROPERTIES,
        value: MessageParameterValue::Uint8(0),
    });
    let (_client, mut server) = establish_pair();
    let err = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            params,
            TrackProperties::new(),
        )
        .expect_err("PUBLISH で許可されないパラメータは送信前に拒否されること");
    assert_eq!(
        err.as_session_error()
            .expect("セッションエラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    assert_eq!(server.subscriptions().count(), 0);
    assert_eq!(
        drain_open_fill_events(&mut server),
        Vec::<u64>::new(),
        "拒否した PUBLISH で fill stream を開いてはならない"
    );
}

/// PUBLISH を受けた subscriber は fill を開かず、publisher が開いた初回 fill を受理する
///
/// draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams): fill fetch stream
/// を開くのは publisher である。PUBLISH_OK を返す subscriber が開設すると、同じ fill を
/// 両端が開くことになる。初回 fill の FETCH_HEADER は PUBLISH の Request ID を載せる
/// (§3.4) ため、subscriber は PUBLISH の Request ID で受理する。
#[test]
fn subscriber_receives_publish_with_fill_and_opens_nothing() {
    use shiguredo_moqt::session::types::FetchHeaderAcceptance;
    use shiguredo_moqt::stream::FETCH_HEADER_TYPE;

    let (mut client, mut server, _sub_rid) = establish_sub_with_object();
    let pub_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            fill_params(MessageParameters::new()),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, publish_msg) = take_send_request(&mut server);
    assert_eq!(
        drain_open_fill_events(&mut server),
        vec![pub_rid],
        "publisher である server だけが初回 fill を開くこと"
    );

    client
        .recv_request(publish_msg)
        .expect("FILL_PARAMETERS 付き PUBLISH を受理できること");
    assert!(
        client.subscription(pub_rid).is_some(),
        "PUBLISH が subscription を作ること"
    );
    assert_eq!(
        drain_open_fill_events(&mut client),
        Vec::<u64>::new(),
        "PUBLISH を受けた subscriber は fill stream を開いてはならない"
    );

    // publisher が開いた初回 fill は PUBLISH の Request ID を載せて届き、subscriber は受理する
    let stream_id = DataStreamId(311);
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_fetch_header(
                stream_id,
                &FetchHeader {
                    request_id: pub_rid
                }
            )
            .expect("初回 fill の FETCH_HEADER が受理されること"),
        FetchHeaderAcceptance::Accepted
    );
    client
        .recv_fetch_entry(stream_id, &fetch_object_entry(0, 0))
        .expect("受理した fill stream の Object 通知が通ること");
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("fill stream の終端通知が通ること");
    assert_eq!(client.state(), SessionState::Established);
}

/// PUBLISH の処理前に届いた初回 fill は、PUBLISH の受理後に同じ stream を再通知して受理できる
///
/// draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams): 初回 fill は
/// PUBLISH を送った直後に別の uni stream で開かれるため、PUBLISH の処理前に到着しうる。
/// `Session` は未知の Request ID でセッションを閉じず、状態も変えずに
/// `FetchHeaderAcceptance::RequestNotRegistered` を返す。caller は stream と decode 済み
/// ヘッダを保留し、PUBLISH の受理後に再通知する。
#[test]
fn fetch_header_is_accepted_after_publish_arrives() {
    use shiguredo_moqt::message::Publish;
    use shiguredo_moqt::session::types::FetchHeaderAcceptance;
    use shiguredo_moqt::stream::FETCH_HEADER_TYPE;

    let (mut client, mut server) = establish_pair();
    // PUBLISH は server (publisher) が発行するため、Request ID は server 側の採番である
    let pub_rid = server
        .next_local_request_id()
        .expect("テストフィクスチャの前提条件を満たす");
    let stream_id = DataStreamId(312);
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_fetch_header(
                stream_id,
                &FetchHeader {
                    request_id: pub_rid
                }
            )
            .expect("先着した初回 fill でセッションを閉じないこと"),
        FetchHeaderAcceptance::RequestNotRegistered,
        "PUBLISH が未登録であることを再試行可能な結果で返すこと"
    );
    assert_eq!(client.state(), SessionState::Established);

    // PUBLISH を処理すると、同じ stream の再通知が受理される
    client
        .recv_request(ControlMessage::Publish(Publish {
            request_id: pub_rid,
            track_namespace: ns(&[b"live"]),
            track_name: b"cam".to_vec(),
            track_alias: 7,
            parameters: fill_params(MessageParameters::new()),
            track_properties: TrackProperties::new(),
        }))
        .expect("FILL_PARAMETERS 付き PUBLISH を受理できること");
    assert_eq!(
        client
            .recv_fetch_header(
                stream_id,
                &FetchHeader {
                    request_id: pub_rid
                }
            )
            .expect("PUBLISH の受理後の再通知が受理されること"),
        FetchHeaderAcceptance::Accepted
    );
    client
        .recv_fetch_entry(stream_id, &fetch_object_entry(0, 0))
        .expect("受理した fill stream の Object 通知が通ること");
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("fill stream の終端通知が通ること");
    assert_eq!(client.state(), SessionState::Established);
}

/// 初回 fill (PUBLISH 起因) と REQUEST_UPDATE 起因の fill は別の Request ID を持つ
///
/// draft-ietf-moq-transport-23 §6.4.2.1 (Request ID): REQUEST_UPDATE は PUBLISH / SUBSCRIBE とは
/// 別の Request ID を消費する。§3.4 (Fill Semantics) により初回 fill の FETCH_HEADER は
/// subscription の Request ID、REQUEST_UPDATE 起因の fill は REQUEST_UPDATE 自身の
/// Request ID を載せる。subscriber はどちらの fill も subscription へ解決して受理する。
#[test]
fn initial_and_update_fill_use_distinct_request_ids() {
    use shiguredo_moqt::stream::FETCH_HEADER_TYPE;

    // subscriber (client) が SUBSCRIBE を送り、publisher (server) が SUBSCRIBE_OK を返す
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
    server
        .send_subscribe_ok(sub_rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // Largest Object が未知のため SUBSCRIBE では fill を開かない。公開してから
    // REQUEST_UPDATE で fill を開かせる
    let sg_stream = DataStreamId(401);
    server
        .send_subgroup_header(
            sg_stream,
            sub_rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: 0,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(sg_stream, 0, None)
        .expect("テストフィクスチャの前提条件を満たす");
    let update_rid = send_update_to_server(
        &mut client,
        &mut server,
        sub_rid,
        fill_params(MessageParameters::new()),
    );
    assert_eq!(drain_open_fill_events(&mut server), vec![update_rid]);
    assert_ne!(
        update_rid, sub_rid,
        "REQUEST_UPDATE は購読とは別の Request ID を消費すること"
    );

    // subscriber は REQUEST_UPDATE 起因の fill を REQUEST_UPDATE の Request ID で受理する
    let update_stream = DataStreamId(402);
    client
        .recv_data_stream_type(update_stream, FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_fetch_header(
            update_stream,
            &FetchHeader {
                request_id: update_rid,
            },
        )
        .expect("REQUEST_UPDATE 起因の fill FETCH_HEADER が受理されること");
    client
        .recv_data_stream_closed(update_stream, RequestStreamEnd::Fin)
        .expect("fill stream の終端通知が通ること");

    // 初回 fill は購読の Request ID で受理する (REQUEST_UPDATE の Request ID とは別)
    let initial_stream = DataStreamId(403);
    client
        .recv_data_stream_type(initial_stream, FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_fetch_header(
            initial_stream,
            &FetchHeader {
                request_id: sub_rid,
            },
        )
        .expect("初回 fill の FETCH_HEADER が受理されること");
    client
        .recv_data_stream_closed(initial_stream, RequestStreamEnd::Fin)
        .expect("fill stream の終端通知が通ること");
    assert_eq!(client.state(), SessionState::Established);
}
