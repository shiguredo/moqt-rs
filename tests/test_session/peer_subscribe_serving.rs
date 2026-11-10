//! 受信 SUBSCRIBE の購読への送出 API のテスト
//!
//! draft-ietf-moq-transport-23 §3.1 (Subscriptions) は "When an Object matches the filters
//! of multiple subscriptions to the same Track, the publisher MUST send the Object once for
//! each matching subscription" と定める。publisher (Session の my_role=Publisher) は
//! PUBLISH 購読に加えて、受信 SUBSCRIBE で確立した購読に対しても
//! `send_subgroup_header` / `send_subgroup_object` / `send_object_datagram` /
//! `send_publish_done` を使えることを固定する (PUBLISH_DONE は §6.4.2.2 の "the publisher
//! of an Established subscription MUST send PUBLISH_DONE, before sending a FIN" と
//! §9.9 の MUST NOT)。
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use super::*;

/// 受信 SUBSCRIBE の購読 (my_role=Publisher) に subgroup stream を開き Object を送れること
///
/// 複数購読で同一 Object を送る経路 (PUBLISH 購読 + 受信 SUBSCRIBE 購読) の前提である。
#[test]
fn peer_subscription_sends_subgroup_header_and_object() {
    let (_, mut publisher, rid) = establish_subscribe_track(500);
    let s_sub = publisher
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(s_sub.my_role, TrackRole::Publisher);
    let stream_id = DataStreamId(30);

    // per-subscription の subgroup stream を開き、Object を送る
    let header = SubgroupHeader {
        track_alias: 500,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    publisher
        .send_subgroup_header(stream_id, rid, &header)
        .expect("受信 SUBSCRIBE 購読の subgroup stream を開けること");
    publisher
        .send_subgroup_object(stream_id, 0, None)
        .expect("受信 SUBSCRIBE 購読に Object を送れること");

    // 全 outgoing stream を閉じれば PUBLISH_DONE を送れる (§9.9 の MUST NOT)。
    // Stream Count は session が追跡した stream 数 (1) と一致させる
    publisher
        .send_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    publisher
        .send_publish_done(
            rid,
            PUBLISH_DONE_GOING_AWAY,
            1,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("全 stream 閉鎖後の PUBLISH_DONE を送れること");
}

/// 受信 SUBSCRIBE の購読 (my_role=Publisher) に datagram Object を送れること
#[test]
fn peer_subscription_sends_object_datagram() {
    let (_, mut publisher, rid) = establish_subscribe_track(501);
    publisher
        .send_object_datagram(rid, 0, 0, None, None, false)
        .expect("受信 SUBSCRIBE 購読に datagram Object を送れること");
    let s_sub = publisher
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        s_sub.largest_received_location,
        Some(Location {
            group_id: 0,
            object_id: 0,
        })
    );
}

/// open な stream が残ったままだと PUBLISH_DONE が拒否されること (§9.9 の MUST NOT)
#[test]
fn peer_subscription_publish_done_requires_closed_streams() {
    let (_, mut publisher, rid) = establish_subscribe_track(502);
    let header = SubgroupHeader {
        track_alias: 502,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    publisher
        .send_subgroup_header(DataStreamId(40), rid, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = publisher
        .send_publish_done(
            rid,
            PUBLISH_DONE_GOING_AWAY,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect_err("open stream が残ると PUBLISH_DONE は拒否されること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}
