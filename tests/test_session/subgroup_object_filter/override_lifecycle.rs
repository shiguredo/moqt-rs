//! 先頭 Object の delivery timeout override の登録と削除のテスト

use super::*;
use shiguredo_moqt::message_parameter::PARAM_OBJECTID_FILTER;

/// 先頭 Object の delivery timeout override が帰属先 subscription に登録され、
/// stream 終端でその帰属先から削除されること
///
/// header 時に紐づいた stream 所有者と Object の帰属先が異なる共有 Track Alias でも、
/// override が残留しないことを検証する。
#[test]
fn delivery_timeout_override_is_removed_from_attributed_subscription() {
    const ALIAS: u64 = 3010;
    // rid1 は Object ID [0, 9] のみ通すため header は rid1 に紐づき、Object ID=50 は
    // 2 番目の候補 rid2 (unfiltered) に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(312);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        has_properties: true,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );

    // 先頭 Object ID=50 は rid2 に帰属し、OBJECT_DELIVERY_TIMEOUT の override が
    // rid2 にのみ登録される
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &object_with_properties(50, delivery_timeout_properties(7))
            )
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .delivery_timeouts
            .subgroup_overrides
            .is_empty(),
        "stream 所有者 rid1 には override が登録されないこと"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .delivery_timeouts
            .subgroup_overrides
            .get(&(0, 0)),
        Some(&(None, Some(7))),
        "帰属先 rid2 に override が登録されること"
    );

    // FIN で stream が終端すると override は帰属先 rid2 から削除される
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("stream 終端の処理に成功すること");
    assert!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .delivery_timeouts
            .subgroup_overrides
            .is_empty(),
        "stream 終端で rid1 に override が残留しないこと"
    );
    assert!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .delivery_timeouts
            .subgroup_overrides
            .is_empty(),
        "stream 終端で帰属先 rid2 の override が削除されること"
    );
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid2, 0, 0),
        (None, None),
        "公開 API の参照結果からも override が消えること"
    );
}

/// 移管時に delivery timeout override の所有者が死んだ id のまま残らないこと
///
/// 先頭 Object の帰属先が忘却対象 (rid1) の場合、override は回収された subscription と
/// 一緒に消える。`timeout_override_request_id` を生存判定で正規化しないと、以後の
/// request_id 再利用で無関係な subscription の override を削除してしまう。
#[test]
fn migration_normalizes_dead_timeout_override_owner() {
    const ALIAS: u64 = 3021;
    // rid1 unfiltered / rid2 は Object ID [10, 99] のみ通す
    let mut params2 = MessageParameters::new();
    params2.push(range_filter(PARAM_OBJECTID_FILTER, 0, 10, 99));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, MessageParameters::new(), params2);

    let stream_id = DataStreamId(326);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        has_properties: true,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    // 先頭 Object ID=5 は rid1 に帰属し、OBJECT_DELIVERY_TIMEOUT の override が
    // rid1 に登録される
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &object_with_properties(5, delivery_timeout_properties(7))
            )
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid1, 0, 0),
        (None, Some(7)),
        "帰属先 rid1 に override が登録されること"
    );

    // rid1 をキャンセルして forget する。override は回収された rid1 と一緒に消え、
    // 移管先 rid2 へ引き継がれない (timeout_override_request_id が死んだ id のまま残らない)
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    client
        .forget_subscription(rid1)
        .expect("キャンセル由来 Terminated は cleanup_ready で forget できること");
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid2, 0, 0),
        (None, None),
        "回収済み subscription の override が移管先へ残留しないこと"
    );

    // 移管後も会計と終端が整合する
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(15))
            .expect("移管後の Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("移管後の stream 終端の処理に成功すること");
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid2, 0, 0),
        (None, None),
        "FIN 後も override が残留しないこと"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "FIN で移管先 rid2 の open 中の受信 stream 数が戻ること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 移管後も生きた override 所有者の参照を維持し、FIN で確実に削除すること
///
/// 最後の帰属先と override 所有者が異なる場合でも、override 所有者が現在も生きていれば
/// `timeout_override_request_id` の参照を維持する。移管先の終端で override 削除経路が
/// no-op にならず、生きた所有者から削除できることを検証する。
#[test]
fn migration_keeps_live_timeout_override_owner_until_fin() {
    const ALIAS: u64 = 3022;
    // rid1 は Object ID [10, 19] のみ通し、rid3 (unfiltered) が先頭 Object の帰属先になる
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 10, 19));
    let (mut client, rid1, rid3) = establish_shared_alias(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(327);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        has_properties: true,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    // 先頭 Object ID=5 は rid1 がフィルタ不合格となり、unfiltered の rid3 に帰属する。
    // override は rid3 に登録される
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &object_with_properties(5, delivery_timeout_properties(7))
            )
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid3, 0, 0),
        (None, Some(7)),
        "先頭 Object の帰属先 rid3 に override が登録されること"
    );
    // 2 番目の Object ID=15 は rid1 が合格し、最後の帰属先は rid1 になる
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(15))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(largest_received(&client, rid1), Some((0, 15)));

    // rid1 を forget すると移管先は同じ alias の生きた他候補 rid3 になり、
    // override 所有者 rid3 は生きたままなので参照が維持される
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    client
        .forget_subscription(rid1)
        .expect("キャンセル由来 Terminated は cleanup_ready で forget できること");
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid3, 0, 0),
        (None, Some(7)),
        "移管後も生きた override 所有者の参照が維持されること"
    );

    // 移管後も Object を受信でき、FIN で override が生きた所有者から削除される
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(25))
            .expect("移管後の Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
    );
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("移管後の stream 終端の処理に成功すること");
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid3, 0, 0),
        (None, None),
        "FIN で生きた override 所有者 rid3 から override が削除されること"
    );
    assert_eq!(
        client
            .subscription(rid3)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "FIN で移管先 rid3 の open 中の受信 stream 数が戻ること"
    );
    assert_eq!(client.state(), SessionState::Established);
}
