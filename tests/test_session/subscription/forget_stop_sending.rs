//! `forget_subscription` が関連する受信 data stream へ STOP_SENDING を指示する
//!
//! draft-ietf-moq-transport-23 §9.9 (PUBLISH_DONE): "The subscriber SHOULD send STOP_SENDING
//! on all streams related to the subscription when it deletes subscription state."
//! 購読状態を削除する時点で、実際に除去する open な受信 data stream に
//! `SessionEvent::StopSendingDataStream` (CANCELLED 0x1) を発行する。共有 Track Alias で
//! 生きた別 subscription へ移管する stream と、既に破棄対象・終端済みの stream は対象外である。

use super::*;
use shiguredo_moqt::error::STREAM_CANCELLED;
use shiguredo_moqt::stream::FETCH_HEADER_TYPE;

/// SUBSCRIBE → SUBSCRIBE_OK で購読を Established にする
fn establish_subscription(client: &mut Session, server: &mut Session, alias: u64) -> u64 {
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(rid, alias, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");
    rid
}

/// 受信 subgroup stream を開く (Accepted まで進める)
fn open_subgroup_stream(client: &mut Session, alias: u64, stream_no: u64, group_id: u64) {
    let stream_id = DataStreamId(stream_no);
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("SUBGROUP_HEADER 用の stream type は受理されること");
    assert_eq!(
        client
            .recv_subgroup_header(
                stream_id,
                &SubgroupHeader {
                    track_alias: alias,
                    group_id,
                    subgroup_id: SubgroupIdMode::Explicit(0),
                    publisher_priority: Some(1),
                    has_properties: false,
                    end_of_group: false,
                    first_object: false,
                },
            )
            .expect("SUBGROUP_HEADER は受理されること"),
        TrackDataAcceptance::Accepted
    );
}

/// 発行された `StopSendingDataStream` の (stream_id, error_code) を集める
fn take_stop_sending(session: &mut Session) -> Vec<(DataStreamId, u64)> {
    let mut events = Vec::new();
    while let Some(e) = session.poll_event() {
        if let SessionEvent::StopSendingDataStream {
            stream_id,
            error_code,
        } = e
        {
            events.push((stream_id, error_code));
        }
    }
    events
}

/// open な受信 subgroup stream を残したまま forget すると STOP_SENDING が 1 回発行される
#[test]
fn forget_subscription_emits_stop_sending_for_open_subgroup_stream() {
    let (mut client, mut server) = establish_pair();
    let rid = establish_subscription(&mut client, &mut server, 500);
    open_subgroup_stream(&mut client, 500, 11, 3);
    client
        .stop_sending(rid)
        .expect("Established の subscription は stop_sending できること");
    while client.poll_event().is_some() {}

    assert!(
        client.forget_subscription(rid).is_some(),
        "キャンセル由来 Terminated は forget できること"
    );
    assert_eq!(
        take_stop_sending(&mut client),
        vec![(DataStreamId(11), STREAM_CANCELLED)],
        "open な受信 subgroup stream に STOP_SENDING が 1 回発行されること"
    );

    // STOP_SENDING 後に peer が送る FIN は吸収される
    client
        .recv_data_stream_closed(DataStreamId(11), RequestStreamEnd::Fin)
        .expect("STOP_SENDING 後の終端通知は no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);
}

/// open な fill fetch stream を残したまま forget すると STOP_SENDING が発行される
#[test]
fn forget_subscription_emits_stop_sending_for_fill_fetch_stream() {
    let (mut client, mut server) = establish_pair();
    let rid = establish_subscription(&mut client, &mut server, 500);
    // 初回 fill は購読の Request ID を載せた FETCH_HEADER で開く (§3.4)
    let stream_id = DataStreamId(22);
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("FETCH stream 用の stream type は受理されること");
    client
        .recv_fetch_header(stream_id, &FetchHeader { request_id: rid })
        .expect("fill fetch stream の FETCH_HEADER は受理されること");

    client
        .stop_sending(rid)
        .expect("Established の subscription は stop_sending できること");
    while client.poll_event().is_some() {}
    assert!(client.forget_subscription(rid).is_some());
    assert_eq!(
        take_stop_sending(&mut client),
        vec![(stream_id, STREAM_CANCELLED)],
        "open な fill fetch stream に STOP_SENDING が発行されること"
    );
    // 除去した fill fetch stream の id は保持集合へ移り、後続の FIN / RESET は no-op
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("FIN は no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);
}

/// 移管先のある subgroup stream には STOP_SENDING を発行しない
///
/// draft-ietf-moq-transport-23 §3.1 (Subscriptions): 共有 Track Alias では Object の帰属先が
/// 別の subscription になりうる。移管先の受信を止めてしまうため対象外である。
#[test]
fn forget_subscription_does_not_emit_stop_sending_for_migrated_stream() {
    let (mut client, mut server) = establish_pair();
    // 同じ Track・同じ alias の購読を 2 本確立する
    let first = establish_subscription(&mut client, &mut server, 500);
    let second = establish_subscription(&mut client, &mut server, 500);
    assert_ne!(first, second);
    open_subgroup_stream(&mut client, 500, 11, 3);
    // 2 本目の購読へ帰属させる (FILTER 無しのため最後の帰属先になる)
    client
        .recv_subgroup_object(
            DataStreamId(11),
            &DecodedSubgroupObject {
                object_id: 1,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect("Object の受信に成功すること");

    // 1 本目を cancel して forget する
    client
        .stop_sending(first)
        .expect("Established の subscription は stop_sending できること");
    while client.poll_event().is_some() {}
    assert!(client.forget_subscription(first).is_some());
    assert!(
        take_stop_sending(&mut client).is_empty(),
        "生きた移管先がある stream には STOP_SENDING を発行しないこと"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 既に破棄対象の stream には STOP_SENDING を発行しない
#[test]
fn forget_subscription_does_not_emit_stop_sending_for_discarded_stream() {
    let (mut client, mut server) = establish_pair();
    let rid = establish_subscription(&mut client, &mut server, 500);
    client
        .stop_sending(rid)
        .expect("Established の subscription は stop_sending できること");
    while client.poll_event().is_some() {}
    // キャンセル後の新規 SUBGROUP_HEADER は Discarded になる (§3.1.3)
    let stream_id = DataStreamId(11);
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(
                stream_id,
                &SubgroupHeader {
                    track_alias: 500,
                    group_id: 3,
                    subgroup_id: SubgroupIdMode::Explicit(0),
                    publisher_priority: Some(1),
                    has_properties: false,
                    end_of_group: false,
                    first_object: false,
                },
            )
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Discarded
    );
    assert!(client.forget_subscription(rid).is_some());
    assert!(
        take_stop_sending(&mut client).is_empty(),
        "既に破棄対象の stream に STOP_SENDING を重ねて発行しないこと"
    );
}

/// FETCH の破棄経路では `StopSendingDataStream` を新たに発行しない
///
/// 購読状態の削除ではないため、`RemovedIncomingStreams` の戻り値を使うのは
/// `forget_subscription` だけである。FETCH の cancel は bidi request stream の
/// STOP_SENDING / RESET_STREAM (`send_fetch_stop_sending` の 2 イベント) で表現し、
/// 受信 data stream への `StopSendingDataStream` は発行しない (§3.2.4 の MAY を新設しない)。
#[test]
fn fetch_cancel_does_not_emit_stop_sending_data_stream() {
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
                    group_id: 1,
                    object_id: 0,
                },
            ),
        )
        .expect("FETCH の送信に成功すること");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("FETCH の受信に成功すること");
    server
        .send_fetch_ok(
            rid,
            fetch_ok_params(Some(Location {
                group_id: 1,
                object_id: 0,
            })),
            TrackProperties::new(),
        )
        .expect("FETCH_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("FETCH_OK の受信に成功すること");
    // FETCH 応答 stream を開く
    let stream_id = DataStreamId(31);
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_fetch_header(stream_id, &FetchHeader { request_id: rid })
        .expect("テストフィクスチャの前提条件を満たす");
    while client.poll_event().is_some() {}

    // FETCH の cancel は bidi の受信方向 (StopSendingRequestStream) と送信方向
    // (ResetRequestStream) だけを発行し、受信 data stream への StopSendingDataStream は
    // 発行しない (§3.2.4 の data stream への STOP_SENDING は MAY であり本実装は新設しない)
    client
        .send_fetch_stop_sending(rid)
        .expect("FETCH の cancel に成功すること");
    assert!(
        take_stop_sending(&mut client).is_empty(),
        "FETCH の cancel では StopSendingDataStream を発行しないこと"
    );
    assert_eq!(client.state(), SessionState::Established);
}
