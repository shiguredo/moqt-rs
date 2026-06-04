//! END_OF_GROUP + FIN による Group 終端確定のテスト

use super::*;
use shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION;
use shiguredo_moqt::message_parameter::PARAM_OBJECTID_FILTER;

/// END_OF_GROUP + FIN の Group 終端確定が stream 所有者ではなく Object の帰属先へ
/// 反映されること
///
/// draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header): END_OF_GROUP bit + FIN は
/// 「その Group の最終 Object」を確定する ("the subscriber can infer the final Object in
/// the Group when the data stream is terminated by a FIN")。共有 Track Alias で Object の
/// 帰属先が stream 所有者と異なる場合、終端を帰属先に記録しないと帰属先で Malformed
/// Track (§12.1 条件 4) を検出できない。
#[test]
fn end_of_group_fin_records_group_end_on_attributed_subscription() {
    const ALIAS: u64 = 3016;
    // rid1 は Object ID [0, 9] のみ通すため header は rid1 に紐づき、Object ID=50 は
    // 2 番目の候補 rid2 (unfiltered) に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(318);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        end_of_group: true,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted,
        "header は最初の候補 rid1 に受理されること"
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(50))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=50 は rid2 に帰属すること"
    );

    // END_OF_GROUP + FIN: Group 0 の最終 Object は 50 なので、存在しない最小 Object ID の
    // 51 が帰属先 rid2 に記録される
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("stream 終端の処理に成功すること");
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .ended_groups
            .get(&0),
        None,
        "stream 所有者 rid1 には Group 終端を記録しないこと"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .ended_groups
            .get(&0),
        Some(&51),
        "帰属先 rid2 に Group 終端 (50 の次 = 51) が記録されること"
    );

    // 同一 Group の別 Subgroup で 51 以上の Object ID を送ると Malformed Track (§12.1 条件 4)
    let stream_id2 = DataStreamId(319);
    let header2 = SubgroupHeader {
        track_alias: ALIAS,
        subgroup_id: SubgroupIdMode::Explicit(1),
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id2, &header2),
        TrackDataAcceptance::Accepted,
        "別 Subgroup の header は受理されること"
    );
    let err = client
        .recv_subgroup_object(stream_id2, &normal_object(60))
        .expect_err("Group 終端後の大きい Object ID は Malformed Track になること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .state,
        SubscriptionState::Terminated,
        "Malformed の終端対象が Object の帰属先 rid2 になること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 所有者をキャンセルした後の END_OF_GROUP + FIN も、帰属実績があれば帰属先へ
/// Group 終端を記録すること
///
/// 帰属実績がある stream は帰属先にとって生きた stream であり、所有者のキャンセルで
/// tracker 更新と Group 終端確定を丸ごと skip してはならない。
#[test]
fn end_of_group_fin_records_group_end_after_owner_cancel() {
    const ALIAS: u64 = 3017;
    // rid1 は Object ID [0, 9] のみ通すため header は rid1 に紐づき、Object ID=50 は
    // 2 番目の候補 rid2 (unfiltered) に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(320);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        end_of_group: true,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted,
        "header は最初の候補 rid1 に受理されること"
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(50))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=50 は rid2 に帰属すること"
    );
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");

    // 所有者 rid1 はキャンセル済みでも、帰属実績 (rid2) があるため通常終端として扱い、
    // tracker を更新して Group 終端を帰属先に記録する
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("帰属実績のある stream の終端は通常どおり処理されること");
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "所有者 rid1 の open 中の受信 stream 数が戻ること"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .ended_groups
            .get(&0),
        Some(&51),
        "帰属先 rid2 に Group 終端 (50 の次 = 51) が記録されること"
    );

    // 同一 Group の別 Subgroup で 51 以上の Object ID を送ると Malformed Track (§12.1 条件 4)
    let stream_id2 = DataStreamId(321);
    let header2 = SubgroupHeader {
        track_alias: ALIAS,
        subgroup_id: SubgroupIdMode::Explicit(1),
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id2, &header2),
        TrackDataAcceptance::Accepted,
        "キャンセル済み所有者が残っていても Established な rid2 に受理されること"
    );
    let err = client
        .recv_subgroup_object(stream_id2, &normal_object(60))
        .expect_err("Group 終端後の大きい Object ID は Malformed Track になること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .state,
        SubscriptionState::Terminated,
        "Malformed の終端対象が Object の帰属先 rid2 になること"
    );
    assert_eq!(client.state(), SessionState::Established);
}
