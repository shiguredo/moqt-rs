//! キャンセル由来 `Terminated` subscription へのデータ受理のテスト
//!
//! draft-ietf-moq-transport-21 §3.1.2 (Track Alias):
//! "Objects can arrive after a subscription has been cancelled. Subscribers SHOULD retain
//! sufficient state to quickly discard these unwanted Objects, rather than treating them as
//! belonging to an unknown Track Alias."
//!
//! キャンセル由来 `Terminated`（`publish_done` が `None`）の subscription に届いた
//! SUBGROUP_HEADER / OBJECT_DATAGRAM / 既存 stream への object / stream 終端は受理せず、
//! 内部状態（`open_incoming_subgroup_count` / `largest_received_location` /
//! `peer_subgroups` tracker 等）を汚染しないことを検証する。PUBLISH_DONE 受信済み
//! （`publish_done` が `Some`）の drain 期間は従来どおり受理する (draft §9.9)。

use super::*;
use shiguredo_moqt::message_parameter::{
    LocationFilter, MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
};
use shiguredo_moqt::stream::OBJECT_STATUS_END_OF_TRACK;

/// SUBSCRIBE → SUBSCRIBE_OK で両側 Established にし、request_id を返す
fn establish_subscribe(
    client: &mut Session,
    server: &mut Session,
    alias: u64,
    parameters: MessageParameters,
) -> u64 {
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), parameters)
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

fn subgroup_header(alias: u64, group_id: u64, subgroup_id: u64) -> SubgroupHeader {
    SubgroupHeader {
        track_alias: alias,
        group_id,
        subgroup_id: SubgroupIdMode::Explicit(subgroup_id),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    }
}

fn object(object_id: u64) -> DecodedSubgroupObject {
    DecodedSubgroupObject {
        object_id,
        payload_length: 1,
        status: None,
        properties_bytes: None,
    }
}

/// キャンセル由来 `Terminated` 後の新規 SUBGROUP_HEADER が `Discarded` になること
#[test]
fn cancelled_subscription_discards_new_subgroup_header() {
    let (mut client, mut server) = establish_pair();
    let rid = establish_subscribe(&mut client, &mut server, 500, MessageParameters::new());
    // キャンセル (publish_done 未設定のまま stop_sending で Terminated に遷移)
    client
        .stop_sending(rid)
        .expect("Established の subscription は stop_sending できること");

    let stream_id = DataStreamId(11);
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &subgroup_header(500, 3, 7))
            .expect("キャンセル後のデータ受理は Err にならないこと"),
        TrackDataAcceptance::Discarded,
        "キャンセル由来 Terminated への新規 SUBGROUP_HEADER は Discarded になること"
    );
    // 内部状態が汚染されないこと (incoming_subgroup_count で検証)
    assert_eq!(
        client
            .subscription(rid)
            .expect("キャンセル後もアプリの forget まで subscription が残ること")
            .stream_counts
            .incoming_subgroup_count,
        0,
        "キャンセル後の SUBGROUP_HEADER で incoming_subgroup_count が増えないこと"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// キャンセル由来 `Terminated` 後の新規 OBJECT_DATAGRAM が `Discarded` になること
#[test]
fn cancelled_subscription_discards_object_datagram() {
    let (mut client, mut server) = establish_pair();
    let rid = establish_subscribe(&mut client, &mut server, 500, MessageParameters::new());
    client
        .stop_sending(rid)
        .expect("Established の subscription は stop_sending できること");

    assert_eq!(
        client
            .recv_object_datagram(&ObjectDatagram {
                track_alias: 500,
                group_id: 3,
                object_id: 1,
                publisher_priority: Some(7),
                properties_data: None,
                end_of_group: false,
                status: None,
            })
            .expect("キャンセル後のデータ受理は Err にならないこと"),
        TrackDataAcceptance::Discarded,
        "キャンセル由来 Terminated への OBJECT_DATAGRAM は Discarded になること"
    );
    assert_eq!(
        client
            .subscription(rid)
            .expect("キャンセル後もアプリの forget まで subscription が残ること")
            .largest_received_location,
        None,
        "キャンセル後の OBJECT_DATAGRAM で largest_received_location が更新されないこと"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// キャンセル由来 `Terminated` 後の既存 stream への object 受信と終端が no-op で吸収されること
#[test]
fn cancelled_subscription_absorbs_object_and_fin_on_existing_stream() {
    let (mut client, mut server) = establish_pair();
    let rid = establish_subscribe(&mut client, &mut server, 500, MessageParameters::new());

    let stream_id = DataStreamId(11);
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &subgroup_header(500, 3, 7))
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );
    client
        .recv_subgroup_object(stream_id, &object(0))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在すること")
            .largest_received_location,
        Some(Location {
            group_id: 3,
            object_id: 0
        })
    );

    // キャンセル (publish_done 未設定のまま stop_sending で Terminated に遷移)
    client
        .stop_sending(rid)
        .expect("Established の subscription は stop_sending できること");
    let stream_count_before = client
        .subscription(rid)
        .expect("キャンセル後もアプリの forget まで subscription が残ること")
        .stream_counts
        .incoming_subgroup_count;

    // 既存 stream への object 受信は no-op で吸収され、内部状態が汚染されないこと
    client
        .recv_subgroup_object(stream_id, &object(1))
        .expect("キャンセル後の既存 stream への object 受信は Err にならないこと");
    assert_eq!(
        client
            .subscription(rid)
            .expect("キャンセル後もアプリの forget まで subscription が残ること")
            .largest_received_location,
        Some(Location {
            group_id: 3,
            object_id: 0
        }),
        "キャンセル後の object 受信で largest_received_location が更新されないこと"
    );
    assert_eq!(
        client
            .subscription(rid)
            .expect("キャンセル後もアプリの forget まで subscription が残ること")
            .stream_counts
            .incoming_subgroup_count,
        stream_count_before,
        "キャンセル後の object 受信で incoming_subgroup_count が変わらないこと"
    );

    // 既存 stream の終端も no-op で吸収され、セッションが fail しないこと
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("キャンセル後の stream 終端は Err にならないこと");
    assert_eq!(client.state(), SessionState::Established);
    // キャンセル由来 Terminated でも Object の帰属実績がある stream の FIN は通常終端として
    // 扱われ、open 中の受信 stream 数も戻る
    assert_eq!(
        client
            .subscription(rid)
            .expect("キャンセル後もアプリの forget まで subscription が残ること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "stream 終端で open 中の受信 stream 数が戻ること"
    );
    assert_eq!(
        client.subscription_cleanup_ready(rid),
        Some(true),
        "open stream が無くなれば cleanup_ready になり forget_subscription 可能になること"
    );
}

/// キャンセル由来 `Terminated` 後の既存 stream への再 SUBGROUP_HEADER が no-op で吸収されること
#[test]
fn cancelled_subscription_absorbs_re_subgroup_header() {
    let (mut client, mut server) = establish_pair();
    let rid = establish_subscribe(&mut client, &mut server, 500, MessageParameters::new());

    let stream_id = DataStreamId(11);
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &subgroup_header(500, 3, 7))
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );
    client
        .stop_sending(rid)
        .expect("Established の subscription は stop_sending できること");

    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &subgroup_header(500, 3, 7))
            .expect("キャンセル後の再 SUBGROUP_HEADER は Err にならないこと"),
        TrackDataAcceptance::Discarded,
        "キャンセル由来 Terminated の既存 stream への再 SUBGROUP_HEADER は no-op で吸収されること"
    );
    // Discarded 化後の再 SUBGROUP_HEADER も no-op で吸収される
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &subgroup_header(500, 3, 7))
            .expect("Discarded 化後の再 SUBGROUP_HEADER は Err にならないこと"),
        TrackDataAcceptance::Discarded,
        "Discarded 化後の再 SUBGROUP_HEADER も no-op で吸収されること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 自側 STOP_SENDING 送信後に届く peer の FIN / RESET_STREAM が no-op で吸収されること
#[test]
fn stop_sending_then_peer_stream_close_is_absorbed() {
    let (mut client, mut server) = establish_pair();
    let rid = establish_subscribe(&mut client, &mut server, 500, MessageParameters::new());

    let stream_id = DataStreamId(11);
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &subgroup_header(500, 3, 7))
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );
    client
        .stop_sending(rid)
        .expect("Established の subscription は stop_sending できること");

    // 自側 STOP_SENDING 送信 (終端済みの破棄対象 stream id は保持集合へ移る)
    client
        .send_data_stream_stop_sending(stream_id)
        .expect("キャンセル後の STOP_SENDING 送信は Err にならないこと");

    // 保持集合に含まれる id への peer の FIN / RESET_STREAM は no-op で吸収される
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("STOP_SENDING 送信後の peer FIN は no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);

    let stream_id2 = DataStreamId(12);
    client
        .recv_data_stream_type(stream_id2, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id2, &subgroup_header(500, 3, 8))
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Discarded
    );
    client
        .send_data_stream_stop_sending(stream_id2)
        .expect("Discarded stream への STOP_SENDING 送信は Err にならないこと");
    client
        .recv_data_stream_closed(
            stream_id2,
            RequestStreamEnd::Reset {
                error_code: 0x6,
                reliable_size: Some(0),
            },
        )
        .expect("STOP_SENDING 送信後の peer RESET_STREAM は no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);
}

/// キャンセル済み所有者の stream を RESET で破棄終端した後、同一 Subgroup の正当な
/// 再オープンが session close せず受理されること
///
/// 破棄分岐でも `SubgroupTracker` を終端状態 (`Reset`) にしないとエントリが `Open` の
/// まま残り、draft §2.2 (Subgroups) の premature reset 後の再オープンが open 衝突として
/// `PROTOCOL_VIOLATION` になる。
#[test]
fn cancelled_stream_reset_allows_subgroup_reopen() {
    let (mut client, mut server) = establish_pair();
    // rid1 が stream 所有者 (最初の候補)、rid2 が再オープン時の受理先になる
    let rid1 = establish_subscribe(&mut client, &mut server, 500, MessageParameters::new());
    let _rid2 = establish_subscribe(&mut client, &mut server, 500, MessageParameters::new());

    let stream_a = DataStreamId(11);
    client
        .recv_data_stream_type(stream_a, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_a, &subgroup_header(500, 3, 7))
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");

    // RESET で破棄終端する (Object の帰属実績が無いため破棄分岐に入る)
    client
        .recv_data_stream_closed(
            stream_a,
            RequestStreamEnd::Reset {
                error_code: 0x1,
                reliable_size: None,
            },
        )
        .expect("キャンセル由来 Terminated の RESET は Err にならないこと");

    // 同一 Subgroup の再オープン: rid1 はキャンセル済みのため Established な rid2 が
    // 受理する。tracker が `Open` のままなら open 衝突で session close になる
    let stream_b = DataStreamId(12);
    client
        .recv_data_stream_type(stream_b, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_b, &subgroup_header(500, 3, 7))
            .expect("破棄終端後の同一 Subgroup の再オープンは session close せず受理されること"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// キャンセル済み所有者の stream への STOP_SENDING 送信で破棄終端した後、同一
/// Subgroup の正当な再オープンが session close せず受理されること
///
/// 破棄分岐でも `SubgroupTracker` を `StoppedByPeer` にしないとエントリが `Open` の
/// まま残り、draft §11.3.2 (Closing Subgroup Streams) / Appendix A.3
/// (REQUEST_UPDATE の Forward State 0→1) による再オープンが open 衝突として
/// `PROTOCOL_VIOLATION` になる。
#[test]
fn cancelled_stream_stop_sending_allows_subgroup_reopen() {
    let (mut client, mut server) = establish_pair();
    // rid1 が stream 所有者 (最初の候補)、rid2 が再オープン時の受理先になる
    let rid1 = establish_subscribe(&mut client, &mut server, 500, MessageParameters::new());
    let _rid2 = establish_subscribe(&mut client, &mut server, 500, MessageParameters::new());

    let stream_a = DataStreamId(11);
    client
        .recv_data_stream_type(stream_a, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_a, &subgroup_header(500, 3, 7))
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");

    // 破棄対象 stream への STOP_SENDING 送信で破棄終端する
    client
        .send_data_stream_stop_sending(stream_a)
        .expect("キャンセル後の STOP_SENDING 送信は Err にならないこと");

    // 同一 Subgroup の再オープン: rid1 はキャンセル済みのため Established な rid2 が
    // 受理する。tracker が `Open` のままなら open 衝突で session close になる
    let stream_b = DataStreamId(12);
    client
        .recv_data_stream_type(stream_b, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_b, &subgroup_header(500, 3, 7))
            .expect("破棄終端後の同一 Subgroup の再オープンは session close せず受理されること"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// `forget_subscription` 後に届く破棄対象 stream の受信・終端が保持期間中は no-op で吸収され、
/// 保持期間満了で掃除されること
#[test]
fn forget_subscription_absorbs_late_streams_until_retention_expiry() {
    let (mut client, mut server) = establish_pair();
    client.tick(1_000);
    let rid = establish_subscribe(&mut client, &mut server, 500, MessageParameters::new());
    client
        .stop_sending(rid)
        .expect("Established の subscription は stop_sending できること");
    assert!(
        client
            .subscription_cleanup_ready(rid)
            .expect("subscription が存在すること"),
        "キャンセル由来 Terminated は cleanup_ready が即 true になること"
    );
    client
        .forget_subscription(rid)
        .expect("cleanup_ready な subscription は forget できること");

    // forget 後に届く新規 stream は alias tombstone 由来で Discarded になり、
    // stream id 単位の保持集合に登録される (request_id は不明のため None)
    let stream_id = DataStreamId(11);
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &subgroup_header(500, 3, 7))
            .expect("forget 後の SUBGROUP_HEADER は Err にならないこと"),
        TrackDataAcceptance::Discarded,
        "forget 後の SUBGROUP_HEADER は alias tombstone 由来で Discarded になること"
    );
    client
        .recv_subgroup_object(stream_id, &object(0))
        .expect("forget 後の object 受信は no-op で吸収されること");
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("forget 後の stream 終端は no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);

    // 保持期間 (peer_alias_retention_ms) 満了で保持集合から掃除される
    client.tick(6_100);
    let err = client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .unwrap_err();
    assert_eq!(
        err.code, SESSION_PROTOCOL_VIOLATION,
        "保持期間満了後は破棄対象 stream id が掃除され unknown stream id として扱われること"
    );
}

/// PUBLISH_DONE 受信済み `Terminated` の drain 中の既存 stream への object が
/// 従来どおり取り込まれること
///
/// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE): "A subscriber that receives PUBLISH_DONE
/// SHOULD set a timer ... in case some objects are still inbound due to prioritization or
/// packet loss" の drain 期間中は、キャンセル由来とは異なり遅延データを受理する。
#[test]
fn publish_done_drain_keeps_accepting_existing_stream_object() {
    let (mut client, mut server) = establish_pair();
    let rid = establish_subscribe(&mut client, &mut server, 500, MessageParameters::new());

    let stream_id = DataStreamId(11);
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &subgroup_header(500, 3, 7))
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );

    // PUBLISH_DONE 受信で Terminated に遷移 (publish_done が設定される)
    server
        .send_publish_done(
            rid,
            0x2,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("PUBLISH_DONE の送信に成功すること");
    let (_, done_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("PUBLISH_DONE の受信に成功すること");
    let sub = client
        .subscription(rid)
        .expect("drain 期間中は subscription が残ること");
    assert_eq!(sub.state, SubscriptionState::Terminated);
    assert!(
        sub.publish_done.is_some(),
        "PUBLISH_DONE 受信で publish_done が設定されること"
    );

    // drain 中の既存 stream への object は従来どおり取り込まれる
    client
        .recv_subgroup_object(stream_id, &object(4))
        .expect("drain 中の既存 stream への object は従来どおり受理されること");
    assert_eq!(
        client
            .subscription(rid)
            .expect("drain 期間中は subscription が残ること")
            .largest_received_location,
        Some(Location {
            group_id: 3,
            object_id: 4
        }),
        "drain 中の object で largest_received_location が更新されること"
    );
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("drain 中の stream 終端は従来どおり処理されること");
    assert_eq!(client.state(), SessionState::Established);
}

/// PUBLISH_DONE 受信済み `Terminated` での malformed 再検出時は `RequestTerminated` を
/// 二重発行せず、`ResetDataStream` のみ発行すること
///
/// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) の MUST cancel に従い、
/// malformed 検出後のデータは破棄対象になる。drain モードは解除され (`publish_done` が
/// `None` に戻る)、以後のデータはキャンセル由来と同様に破棄される。
#[test]
fn malformed_after_publish_done_sends_reset_without_request_terminated() {
    let (mut client, mut server) = establish_pair();
    let rid = establish_subscribe(&mut client, &mut server, 500, MessageParameters::new());

    let stream_id = DataStreamId(11);
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &subgroup_header(500, 3, 7))
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );

    server
        .send_publish_done(
            rid,
            0x2,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("PUBLISH_DONE の送信に成功すること");
    let (_, done_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("PUBLISH_DONE の受信に成功すること");

    // End of Track 宣言 (drain 中は受理される)
    client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 4,
                payload_length: 0,
                status: Some(OBJECT_STATUS_END_OF_TRACK),
                properties_bytes: None,
            },
        )
        .expect("drain 中の End of Track 宣言は受理されること");
    // 終端宣言後の Object は Malformed Track にあたる (draft §12.1 条件 4/5)
    let err = client
        .recv_subgroup_object(stream_id, &object(4))
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);

    // RequestTerminated は発行されず (二重発行の抑止)、ResetDataStream のみ発行される
    let mut reset_count = 0;
    let mut terminated_count = 0;
    while let Some(e) = client.poll_event() {
        if let SessionEvent::ResetDataStream { stream_id: sid, .. } = e {
            assert_eq!(sid, stream_id);
            reset_count += 1;
        }
        if matches!(e, SessionEvent::RequestTerminated { .. }) {
            terminated_count += 1;
        }
    }
    assert_eq!(
        reset_count, 1,
        "malformed を運んだ stream は ResetDataStream で reset されること"
    );
    assert_eq!(
        terminated_count, 0,
        "PUBLISH_DONE 受信済み Terminated での再検出は RequestTerminated を発行しないこと"
    );
    // drain モードが解除され、以後のデータは破棄対象になる
    assert!(
        client
            .subscription(rid)
            .expect("subscription が存在すること")
            .publish_done
            .is_none(),
        "malformed 検出で publish_done が None にリセットされること"
    );
    client
        .recv_subgroup_object(stream_id, &object(5))
        .expect("drain 解除後の object 受信は no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);
}

/// 破棄対象 stream での `report_mid_object_fin` が no-op で受理され、
/// 終端系 API の呼び出し順序によらず後続の終端通知も no-op で吸収されること
///
/// アプリが decoder を回して mid-object FIN を検出した場合でも、破棄対象 stream で
/// セッションを fail させない。`recv_data_stream_closed` との呼び出し順序で挙動が
/// 変わらないこと (doc コメントの契約) も検証する。キャンセル済み所有者の
/// `Subgroup` variant は FIN / STOP_SENDING と同じ後始末 (open 数・delivery timeout
/// override) を行う。
#[test]
fn discarded_stream_report_mid_object_fin_is_absorbed_in_any_order() {
    let (mut client, mut server) = establish_pair();
    let rid = establish_subscribe(&mut client, &mut server, 500, MessageParameters::new());

    // 順序 1: report_mid_object_fin が先 → 後続の FIN も吸収される
    let stream_a = DataStreamId(11);
    client
        .recv_data_stream_type(stream_a, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_a, &subgroup_header(500, 3, 7))
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted
    );
    client
        .stop_sending(rid)
        .expect("Established の subscription は stop_sending できること");
    client
        .report_mid_object_fin(stream_a)
        .expect("破棄対象 stream での report_mid_object_fin は no-op で受理されること");
    // Discarded variant への置き換えを行わず Subgroup variant のまま終端するため、
    // ここで open 数を戻す (FIN / STOP_SENDING のキャンセル分岐と同じ後始末)
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "report_mid_object_fin で open 中の受信 stream 数が戻ること"
    );
    client
        .recv_data_stream_closed(stream_a, RequestStreamEnd::Fin)
        .expect("report_mid_object_fin の後の FIN も no-op で吸収されること");

    // 順序 2: recv_data_stream_closed が先 → 後続の report_mid_object_fin も吸収される
    let stream_b = DataStreamId(12);
    client
        .recv_data_stream_type(stream_b, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_b, &subgroup_header(500, 3, 8))
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Discarded
    );
    client
        .recv_data_stream_closed(stream_b, RequestStreamEnd::Fin)
        .expect("Discarded stream の FIN は no-op で吸収されること");
    client
        .report_mid_object_fin(stream_b)
        .expect("FIN の後の report_mid_object_fin も no-op で受理されること");

    // PUBLISH_DONE 受信後の cleanup_ready は open 数に依存するため、open 数の
    // 計上漏れがあれば true にならない (キャンセル由来 Terminated の cleanup_ready が
    // 常に true になる性質に依存しない検証)
    server
        .send_publish_done(
            rid,
            0x2,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("PUBLISH_DONE の送信に成功すること");
    let (_, done_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("PUBLISH_DONE の受信に成功すること");
    client.tick(1);
    assert_eq!(
        client.subscription_cleanup_ready(rid),
        Some(true),
        "open 中の受信 stream 数が漏れず cleanup_ready になること"
    );
    assert!(
        client.forget_subscription(rid).is_some(),
        "cleanup_ready な subscription は forget できること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// キャンセル由来候補が複数フィルタ合格した場合、最初に合格した候補の request_id で
/// 紐づけられ、先の候補の `forget_subscription` で除去されること
#[test]
fn multiple_cancelled_candidates_use_first_matched_request_id() {
    let (mut client, mut server) = establish_pair();
    // 同じ alias を共有する 2 subscription を両方キャンセルする
    let rid1 = establish_subscribe(&mut client, &mut server, 500, MessageParameters::new());
    let rid2 = establish_subscribe(&mut client, &mut server, 500, MessageParameters::new());
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    client
        .stop_sending(rid2)
        .expect("Established の subscription は stop_sending できること");

    // 両方のキャンセル候補がフィルタ合格 → 最初に合格した rid1 に紐づけられる
    let stream_id = DataStreamId(11);
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &subgroup_header(500, 3, 7))
            .expect("キャンセル後のデータ受理は Err にならないこと"),
        TrackDataAcceptance::Discarded
    );
    // 最初に合格した候補 (rid1) の forget で破棄対象 stream が除去されること
    client
        .forget_subscription(rid1)
        .expect("cleanup_ready な subscription は forget できること");
    client
        .recv_subgroup_object(stream_id, &object(0))
        .expect("forget 後の object 受信は no-op で吸収されること");
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("forget 後の stream 終端は no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);
}

/// 共有 alias でキャンセル候補と Established 候補が両方フィルタ不合格の場合は
/// 従来どおり `FilteredOut` になること
#[test]
fn shared_alias_all_candidates_filtered_out_keeps_filtered_out() {
    let (mut client, mut server) = establish_pair();
    // rid1 (Established) は group 1 以降のみ通す Location Filter
    let mut filtered_params = MessageParameters::new();
    filtered_params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteStart {
                start: Location {
                    group_id: 1,
                    object_id: 0,
                },
            }
            .encode_to_bytes(),
        ),
    });
    let _rid1 = establish_subscribe(&mut client, &mut server, 500, filtered_params);
    // rid2 は group 2 以降のみ通す Location Filter (キャンセル後は候補から除外される)
    let mut filtered_params2 = MessageParameters::new();
    filtered_params2.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteStart {
                start: Location {
                    group_id: 2,
                    object_id: 0,
                },
            }
            .encode_to_bytes(),
        ),
    });
    let rid2 = establish_subscribe(&mut client, &mut server, 500, filtered_params2);
    client
        .stop_sending(rid2)
        .expect("Established の subscription は stop_sending できること");

    // group 0 のデータ: Established の rid1 は不合格、キャンセル候補の rid2 も不合格
    // (フィルタ評価自体は実行される) → FilteredOut
    let stream_id = DataStreamId(11);
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &subgroup_header(500, 0, 0))
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::FilteredOut,
        "全候補がフィルタ不合格の場合は従来どおり FilteredOut になること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 共有 alias でキャンセル済み subscription に紐づいたデータが受理されず、
/// 残った Established subscription への紐づけとデータ受理が壊れないこと
///
/// draft-ietf-moq-transport-21 §3.1 (Subscriptions): 共有 alias の候補をフィルタ再適用で
/// 振り分ける。キャンセル由来 `Terminated` の候補は受理対象から除外され、データが
/// `peer_subgroups` tracker 等を汚染しないことを検証する。
#[test]
fn shared_alias_cancelled_subscription_does_not_poison_tracking() {
    let (mut client, mut server) = establish_pair();
    // rid2 を先に登録し、共有 alias の候補評価で先頭になるようにする
    // (キャンセル候補が先頭でも Established 候補が正しく選ばれることの検証)
    let rid2 = establish_subscribe(&mut client, &mut server, 500, MessageParameters::new());
    // rid1 は group 2 以降のみ通す Location Filter を付ける
    let mut filtered_params = MessageParameters::new();
    filtered_params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteStart {
                start: Location {
                    group_id: 2,
                    object_id: 0,
                },
            }
            .encode_to_bytes(),
        ),
    });
    let rid1 = establish_subscribe(&mut client, &mut server, 500, filtered_params);
    client
        .stop_sending(rid2)
        .expect("Established の subscription は stop_sending できること");

    // (500, 3, 0) の SUBGROUP_HEADER: キャンセル由来の rid2 は除外され、rid1 に紐づく
    let stream_id = DataStreamId(11);
    client
        .recv_data_stream_type(stream_id, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &subgroup_header(500, 3, 0))
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted,
        "共有 alias でキャンセル候補が先頭でも Established 候補に受理されること"
    );
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .stream_counts
            .incoming_subgroup_count,
        1,
        "データが Established の rid1 に紐づくこと"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("キャンセル後もアプリの forget まで subscription が残ること")
            .stream_counts
            .incoming_subgroup_count,
        0,
        "キャンセル済みの rid2 に紐づかないこと"
    );
    client
        .recv_subgroup_object(stream_id, &object(5))
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");

    // (500, 1, 0) の SUBGROUP_HEADER: rid1 はフィルタ不合格、rid2 はキャンセル由来で合格 →
    // 合格した候補がキャンセル由来のみのため Discarded になり tracker を汚染しない
    let discarded_stream = DataStreamId(12);
    client
        .recv_data_stream_type(discarded_stream, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(discarded_stream, &subgroup_header(500, 1, 0))
            .expect("キャンセル後のデータ受理は Err にならないこと"),
        TrackDataAcceptance::Discarded,
        "共有 alias でキャンセル由来候補のみ合格の場合は Discarded になること"
    );
    assert_eq!(
        client
            .recv_subgroup_object(discarded_stream, &object(0))
            .expect("Discarded stream への object 受信は no-op で吸収されること"),
        TrackDataAcceptance::Discarded,
        "Discarded stream への object 受信は Discarded として吸収されること"
    );
    client
        .recv_data_stream_closed(discarded_stream, RequestStreamEnd::Fin)
        .expect("Discarded stream の終端は no-op で吸収されること");

    // キャンセル済み rid2 のデータが peer_subgroups を汚染していないこと:
    // rid1 の (500, 3, 0) と同じ位置を再び受信できる (FIN 済みの再オープンは
    // プロトコル違反だが、同じ位置への open 記録が残っていなければ別 group の
    // 受信は従来どおり Accepted になる)
    let stream_id2 = DataStreamId(13);
    client
        .recv_data_stream_type(stream_id2, 0x14)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id2, &subgroup_header(500, 4, 0))
            .expect("テストフィクスチャの前提条件を満たす"),
        TrackDataAcceptance::Accepted,
        "汚染がない場合、rid1 の別 group の受信は従来どおり Accepted になること"
    );
    assert_eq!(client.state(), SessionState::Established);
}
