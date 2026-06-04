//! キャンセル済み subscription への Object の破棄と stream 会計のテスト

use super::*;
use shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION;
use shiguredo_moqt::message_parameter::PARAM_OBJECTID_FILTER;

/// キャンセル済み subscription への Object は Discarded として返ること
#[test]
fn cancelled_subscription_object_returns_discarded() {
    const ALIAS: u64 = 3006;
    let (mut client, rid) = establish_filtered_subscription(ALIAS, MessageParameters::new());
    let stream_id = DataStreamId(306);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        group_id: 1,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted,
        "header 受理後にキャンセルすること"
    );
    client
        .stop_sending(rid)
        .expect("Established の subscription は stop_sending できること");

    // 既存 stream への Object はキャンセル由来候補のみ合格のため Discarded が返る
    // (stream は Discarded variant へ移さず、候補評価のたびに破棄判定する)
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(0))
            .expect("キャンセル後の Object 受信は Err にならないこと"),
        TrackDataAcceptance::Discarded
    );

    // キャンセル後の新規 stream も Discarded になり、その Object も Discarded が返る
    let stream_id2 = DataStreamId(307);
    assert_eq!(
        recv_header(&mut client, stream_id2, &header),
        TrackDataAcceptance::Discarded
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_id2, &normal_object(0))
            .expect("Discarded stream への Object 受信は Err にならないこと"),
        TrackDataAcceptance::Discarded
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 合格がキャンセル済み候補のみの Object が Discarded として返ること
///
/// draft-ietf-moq-transport-21 §3.1.2 (Track Alias):
/// "Subscribers SHOULD retain sufficient state to quickly discard these unwanted Objects"
/// 候補ループは datagram の `recv_object_datagram` と同じ規則で、キャンセル由来候補を
/// 帰属対象から除外しつつフィルタ評価だけは実行する。
#[test]
fn object_matching_only_cancelled_candidate_returns_discarded() {
    const ALIAS: u64 = 3007;
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let mut params2 = MessageParameters::new();
    params2.push(range_filter(PARAM_OBJECTID_FILTER, 0, 10, 19));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, params2);
    client
        .stop_sending(rid2)
        .expect("Established の subscription は stop_sending できること");

    // header は Established の rid1 に受理される
    let stream_id = DataStreamId(308);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );

    // Object ID=15 は rid1 が不合格、キャンセル済みの rid2 だけが合格する
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(15))
            .expect("キャンセル後の Object 受信は Err にならないこと"),
        TrackDataAcceptance::Discarded,
        "合格がキャンセル由来候補のみの場合は Discarded になること"
    );
    assert_eq!(
        largest_received(&client, rid1),
        None,
        "Established の rid1 の状態は更新されないこと"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// header 時に紐づいた subscription がキャンセル済みでも、他候補がフィルタ合格すれば
/// その subscription へ再帰属させること
///
/// キャンセル由来 `Terminated` の候補は帰属対象から除外するが、stream 全体を即時破棄せず
/// 候補ループで Established な候補を探す (datagram 経路と同じ規則)。
#[test]
fn cancelled_stream_owner_reattributes_object_to_other_candidate() {
    const ALIAS: u64 = 3011;
    let (mut client, mut server, rid1, rid2) =
        establish_shared_alias_pair(ALIAS, MessageParameters::new(), MessageParameters::new());

    let stream_id = DataStreamId(313);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted,
        "header は最初の候補 rid1 に受理されること"
    );

    // rid1 をキャンセルしても Established な rid2 がフィルタ合格するため、
    // Object は破棄されず rid2 に再帰属する
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(4))
            .expect("キャンセル後の Object 受信は Err にならないこと"),
        TrackDataAcceptance::Accepted,
        "キャンセル済み所有者の stream でも他候補へ再帰属すること"
    );
    assert_eq!(
        largest_received(&client, rid1),
        None,
        "キャンセル済みの rid1 の状態は更新されないこと"
    );
    assert_eq!(
        largest_received(&client, rid2),
        Some((0, 4)),
        "Established な rid2 に帰属して更新されること"
    );

    // 全候補がキャンセル済みになったら Discarded のままとなり、状態は更新されない
    client
        .stop_sending(rid2)
        .expect("Established の subscription は stop_sending できること");
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(5))
            .expect("全候補キャンセル後の Object 受信は Err にならないこと"),
        TrackDataAcceptance::Discarded,
        "全候補キャンセルなら Discarded になること"
    );
    assert_eq!(
        largest_received(&client, rid2),
        Some((0, 4)),
        "Discarded で rid2 の状態は更新されないこと"
    );

    // FIN で open 中の受信 stream 数が戻り、subscription が回収可能になること
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        1,
        "FIN 前は stream 所有者 rid1 の open 数が 1 であること"
    );
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("stream 終端の処理に成功すること");
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "stream 終端で open 中の受信 stream 数が戻ること"
    );
    // キャンセル由来 Terminated の cleanup_ready は常に true になるため、PUBLISH_DONE を
    // 受信して open 数の計上漏れを検出する (漏れがあれば cleanup_ready が false のまま)
    send_publish_done_and_expire_drain(&mut client, &mut server, rid1);
    assert_eq!(
        client.subscription_cleanup_ready(rid1),
        Some(true),
        "open 中の受信 stream 数が漏れず cleanup_ready になること"
    );
    client
        .forget_subscription(rid1)
        .expect("cleanup_ready な subscription は forget できること");
    assert_eq!(client.state(), SessionState::Established);
}

/// キャンセル済み所有者の stream への STOP_SENDING 送信でも open 数が漏れないこと
///
/// header 受理後に所有者をキャンセルした stream は `Subgroup` variant のまま残るため、
/// STOP_SENDING 送信時も `note_incoming_stream_closed` で会計を戻す必要がある。
#[test]
fn cancelled_stream_owner_stop_sending_releases_open_count() {
    const ALIAS: u64 = 3013;
    let (mut client, rid) = establish_filtered_subscription(ALIAS, MessageParameters::new());

    let stream_id = DataStreamId(315);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    client
        .stop_sending(rid)
        .expect("Established の subscription は stop_sending できること");

    client
        .send_data_stream_stop_sending(stream_id)
        .expect("キャンセル後の STOP_SENDING 送信は Err にならないこと");
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "STOP_SENDING 送信で open 中の受信 stream 数が戻ること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// stream 所有者の `forget_subscription` で Object の帰属先が別 subscription の stream は
/// 帰属先へ移管され、以後も帰属先が Object を受信し続けられること
///
/// 共有 Track Alias では Object の帰属先が stream 所有者と異なりうる。所有者の forget で
/// stream ごと除去すると帰属先への受信が黙って止まるため、帰属先へ `request_id` を
/// 付け替えて stream を維持する。移管時は帰属先の `incoming_subgroup_count` /
/// `open_incoming_subgroup_count` に 1 本加算し、override は帰属先に登録済みのため
/// 削除しない。FIN で会計と override が一致して解消されることまで検証する。
#[test]
fn forget_stream_owner_migrates_stream_to_attributed_subscription() {
    const ALIAS: u64 = 3014;
    // rid1 は Object ID [0, 9] のみ通すため header は rid1 に紐づき、Object ID=50 は
    // 2 番目の候補 rid2 (unfiltered) に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, mut server, rid1, rid2) =
        establish_shared_alias_pair(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(316);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        has_properties: true,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &object_with_properties(50, delivery_timeout_properties(7))
            )
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted
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

    // stream 所有者 rid1 をキャンセルして forget する。stream は帰属実績 (rid2) があるため
    // 除去されず、帰属先 rid2 へ移管される
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    client
        .forget_subscription(rid1)
        .expect("キャンセル由来 Terminated は cleanup_ready で forget できること");
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .stream_counts
            .incoming_subgroup_count,
        1,
        "移管した stream が帰属先 rid2 の受信 stream 数に加算されること"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        1,
        "移管した stream が帰属先 rid2 の open 中の受信 stream 数に加算されること"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .delivery_timeouts
            .subgroup_overrides
            .get(&(0, 0)),
        Some(&(None, Some(7))),
        "移管先 rid2 の override は削除されず維持されること"
    );

    // 移管後も帰属先 rid2 が Object を受信し続けられること
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(60))
            .expect("移管後の Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "所有者の forget 後も帰属先 rid2 が Object を受信できること"
    );
    assert_eq!(
        largest_received(&client, rid2),
        Some((0, 60)),
        "移管後も帰属先 rid2 の状態が更新されること"
    );

    // FIN まで会計が一致し、override が帰属先から削除されること
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("移管後の stream 終端の処理に成功すること");
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "FIN で帰属先 rid2 の open 中の受信 stream 数が戻ること"
    );
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid2, 0, 0),
        (None, None),
        "FIN で帰属先 rid2 の override が削除されること"
    );
    // PUBLISH_DONE 受信後の cleanup_ready は open 数に依存するため、会計の漏れが
    // あれば true にならない
    send_publish_done_and_expire_drain(&mut client, &mut server, rid2);
    assert_eq!(
        client.subscription_cleanup_ready(rid2),
        Some(true),
        "移管した stream の open 数が漏れず cleanup_ready になること"
    );
    client
        .forget_subscription(rid2)
        .expect("cleanup_ready な subscription は forget できること");
    assert_eq!(client.state(), SessionState::Established);
}

/// Object の帰属実績が無い（全候補でフィルタ不通過）キャンセル済み所有者の stream では
/// `report_mid_object_fin` が破棄対象として吸収されること
///
/// Object を 1 つも受理していない stream は帰属先 subscription にとって生きた stream では
/// ないため、FIN / STOP_SENDING と同じ後始末 (open 数・delivery timeout override・保持集合)
/// を行って no-op で受理する。後始末漏れは open_incoming_subgroup_count の張り付き
/// (subscription リーク) として現れる。
#[test]
fn report_mid_object_fin_absorbs_stream_without_attribution() {
    const ALIAS: u64 = 3015;
    // rid1 は Object ID [0, 9]、rid2 は [10, 19] のみ通すため、Object ID=50 は
    // どちらのフィルタも通らず帰属実績が付かない (header は最初の候補 rid1 に紐づく)
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let mut params2 = MessageParameters::new();
    params2.push(range_filter(PARAM_OBJECTID_FILTER, 0, 10, 19));
    let (mut client, mut server, rid1, _rid2) =
        establish_shared_alias_pair(ALIAS, params1, params2);

    let stream_id = DataStreamId(317);
    let header = SubgroupHeader {
        track_alias: ALIAS,
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
            .expect("フィルタ不通過は Err にならないこと"),
        TrackDataAcceptance::FilteredOut,
        "Object ID=50 はどの候補も通らず帰属実績が付かないこと"
    );

    // stream 所有者 rid1 をキャンセルしてから mid-object FIN を報告する。stream は
    // Subgroup variant のまま残っているため、破棄対象としての後始末が必要になる
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    client
        .report_mid_object_fin(stream_id)
        .expect("帰属実績の無い破棄対象 stream では no-op で受理されること");

    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "report_mid_object_fin で open 中の受信 stream 数が戻ること"
    );

    // 除去した stream id は保持集合へ移り、以後の終端通知も no-op で吸収される
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("report_mid_object_fin 後の FIN も no-op で吸収されること");

    // PUBLISH_DONE 受信後の cleanup_ready は open 数に依存するため、後始末漏れが
    // あれば true にならない
    send_publish_done_and_expire_drain(&mut client, &mut server, rid1);
    assert_eq!(
        client.subscription_cleanup_ready(rid1),
        Some(true),
        "open 中の受信 stream 数が漏れず cleanup_ready になること"
    );
    client
        .forget_subscription(rid1)
        .expect("cleanup_ready な subscription は forget できること");
    assert_eq!(client.state(), SessionState::Established);
}

/// Object の帰属実績がある stream では `report_mid_object_fin` が破棄対象として
/// 吸収されず、draft §11.3 の SHOULD どおりセッションを `PROTOCOL_VIOLATION` で
/// 閉じること
///
/// 帰属実績がある stream は帰属先 subscription にとって生きた stream であり、
/// 所有者のキャンセルだけを理由に吸収すると帰属先への受信を黙って止めることになる。
#[test]
fn report_mid_object_fin_with_attribution_closes_session() {
    const ALIAS: u64 = 3018;
    // rid1 は Object ID [0, 9] のみ通すため header は rid1 に紐づき、Object ID=50 は
    // 2 番目の候補 rid2 (unfiltered) に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, _server, rid1, rid2) =
        establish_shared_alias_pair(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(322);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        has_properties: true,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted,
        "header は最初の候補 rid1 に受理されること"
    );
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &object_with_properties(50, delivery_timeout_properties(7))
            )
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=50 は rid2 に帰属すること"
    );
    assert_eq!(
        client.subgroup_effective_delivery_timeout(rid2, 0, 0),
        (None, Some(7)),
        "帰属先 rid2 に override が登録されること"
    );

    // 所有者 rid1 をキャンセルしても、帰属実績 (rid2) がある stream は吸収されず、
    // §11.3 の SHOULD に従ってセッションを閉じる
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    let err = client
        .report_mid_object_fin(stream_id)
        .expect_err("帰属実績のある stream の mid-object FIN はセッションを閉じること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert!(
        matches!(client.state(), SessionState::Closing | SessionState::Closed),
        "帰属実績のある mid-object FIN でセッションが閉じること"
    );
}

/// 帰属先の `forget_subscription` 後も、死んだ帰属実績を根拠にセッションを閉じたり
/// 誤った subscription を終端したりしないこと
///
/// 共有 Track Alias では Object の帰属先が stream 所有者と異なりうる。帰属先を forget すると
/// `last_attributed_request_id` が死んだ request_id を指したまま残るため、生存判定を行わずに
/// 「帰属実績あり」と扱うと、mid-object FIN の吸収判定や FIN 時の Group 終端確定が誤動作する
/// (draft §3.1.2 (Track Alias) の「不要 Object の即時破棄」に反する)。
/// 所有者 (rid1)・帰属先 (rid2) をキャンセルして rid2 を forget した後に
/// `report_mid_object_fin` が no-op で吸収されること、END_OF_GROUP + FIN が所有者側の記録や
/// Malformed 終端で panic / 誤終端しないことを検証する。
#[test]
fn stale_attribution_after_forget_is_absorbed() {
    const ALIAS: u64 = 3019;
    // rid1 (stream 所有者) は Object ID [0, 9] のみ通し、Object ID=50 は unfiltered の
    // 2 番目の候補 rid2 に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, MessageParameters::new());

    // mid-object FIN を報告する stream
    let stream_mid = DataStreamId(323);
    let header_mid = SubgroupHeader {
        track_alias: ALIAS,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_mid, &header_mid),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_mid, &normal_object(50))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=50 は rid2 に帰属すること"
    );

    // END_OF_GROUP + FIN を報告する stream
    let stream_fin = DataStreamId(324);
    let header_fin = SubgroupHeader {
        track_alias: ALIAS,
        subgroup_id: SubgroupIdMode::Explicit(1),
        end_of_group: true,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_fin, &header_fin),
        TrackDataAcceptance::Accepted
    );
    // 同一 (group, object) の重複不一致 (§12.1 条件 7) を避けるため別の Object ID を使う
    assert_eq!(
        client
            .recv_subgroup_object(stream_fin, &normal_object(60))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=60 は rid2 に帰属すること"
    );

    // 所有者 rid1 と帰属先 rid2 をキャンセルし、帰属先だけを forget する。
    // 以後 last_attributed_request_id は死んだ rid2 を指したままになる
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    client
        .stop_sending(rid2)
        .expect("Established の subscription は stop_sending できること");
    client
        .forget_subscription(rid2)
        .expect("キャンセル由来 Terminated は cleanup_ready で forget できること");

    // 死んだ帰属先しかない stream の mid-object FIN は破棄対象として吸収され、
    // セッションを閉じない
    client
        .report_mid_object_fin(stream_mid)
        .expect("死んだ帰属先しかない stream の mid-object FIN は no-op で受理されること");
    assert_eq!(client.state(), SessionState::Established);
    // 除去した stream id は保持集合へ移り、以後の終端通知も no-op で吸収される
    client
        .recv_data_stream_closed(stream_mid, RequestStreamEnd::Fin)
        .expect("report_mid_object_fin 後の FIN も no-op で吸収されること");
    assert_eq!(
        client
            .subscription(rid1)
            .expect("所有者 rid1 はキャンセル後も forget まで残ること")
            .stream_counts
            .open_incoming_subgroup_count,
        1,
        "stream_mid の破棄後は stream_fin の 1 本だけが open であること"
    );

    // END_OF_GROUP + FIN は死んだ帰属先ではなく所有者へフォールバックする。所有者は
    // キャンセル由来のため subscription スコープの Group 終端は記録せず、wire 構造の
    // 終端だけを処理して panic / 誤終端しない
    client
        .recv_data_stream_closed(stream_fin, RequestStreamEnd::Fin)
        .expect("死んだ帰属先を指す stream の END_OF_GROUP + FIN も正常終端すること");
    assert!(
        client
            .subscription(rid1)
            .expect("所有者 rid1 はキャンセル後も forget まで残ること")
            .ended_groups
            .is_empty(),
        "キャンセル済み所有者に Group 終端を記録しないこと"
    );
    assert_eq!(
        client
            .subscription(rid1)
            .expect("所有者 rid1 はキャンセル後も forget まで残ること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "FIN で open 中の受信 stream 数が戻ること"
    );
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(
        client.subscription_cleanup_ready(rid1),
        Some(true),
        "open 中の受信 stream 数が漏れず cleanup_ready になること"
    );
}

/// FIN を先に処理してから mid-object FIN を報告しても、死んだ帰属先しかない stream は
/// 破棄対象として吸収されること
///
/// `report_mid_object_fin` は `recv_data_stream_closed` との呼び出し順序を問わない契約のため、
/// FIN → report の順でも no-op で吸収される必要がある。破棄分岐に入らず保持集合へ移らないと、
/// 後続の `report_mid_object_fin` が未知 stream として PROTOCOL_VIOLATION でセッションを閉じる。
#[test]
fn stale_attribution_fin_before_report_mid_object_is_absorbed() {
    const ALIAS: u64 = 3021;
    // rid1 (stream 所有者) は Object ID [0, 9] のみ通し、Object ID=50 は unfiltered の
    // 2 番目の候補 rid2 に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(326);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(50))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=50 は rid2 に帰属すること"
    );

    // 所有者 rid1 と帰属先 rid2 をキャンセルし、帰属先だけを forget する。
    // 以後 last_attributed_request_id は死んだ rid2 を指したままになる
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    client
        .stop_sending(rid2)
        .expect("Established の subscription は stop_sending できること");
    client
        .forget_subscription(rid2)
        .expect("キャンセル由来 Terminated は cleanup_ready で forget できること");

    // FIN を先に処理し、その後に mid-object FIN を報告しても順序に依存せず吸収される
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("死んだ帰属先しかない stream の FIN は no-op で吸収されること");
    client
        .report_mid_object_fin(stream_id)
        .expect("FIN 後の mid-object FIN も no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(
        client
            .subscription(rid1)
            .expect("所有者 rid1 はキャンセル後も forget まで残ること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "FIN で open 中の受信 stream 数が戻ること"
    );
}

/// 最後の帰属先が忘却対象自身でも、同じ Track Alias の生きた他候補へ stream を移管し、
/// 以後の Object をその候補で受信し続けられること
///
/// `remove_incoming_data_streams_for_request` の移管先は、`last_attributed_request_id` が
/// 使えない (忘却対象自身 / 回収済み / キャンセル済み) 場合に同じ alias の生きた他候補へ
/// フォールバックする。移管しないと所有者の forget で帰属先への受信が黙って止まる。
#[test]
fn forget_stream_owner_migrates_to_live_alias_candidate() {
    const ALIAS: u64 = 3020;
    // rid1 (stream 所有者) は unfiltered で最初の候補、rid2 は Object ID [10, 99] のみ通す。
    // 先頭 Object ID=5 は候補順で rid1 自身に帰属する (rid2 のフィルタは不合格)
    let mut params2 = MessageParameters::new();
    params2.push(range_filter(PARAM_OBJECTID_FILTER, 0, 10, 99));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, MessageParameters::new(), params2);

    let stream_id = DataStreamId(325);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(5))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=5 は最初の候補 rid1 自身に帰属すること"
    );
    assert_eq!(largest_received(&client, rid1), Some((0, 5)));
    assert_eq!(largest_received(&client, rid2), None);

    // rid1 をキャンセルして forget する。最後の帰属先が忘却対象自身のため、
    // 従来は stream ごと除去されていたが、同じ alias の生きた他候補 rid2 へ移管される
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    client
        .forget_subscription(rid1)
        .expect("キャンセル由来 Terminated は cleanup_ready で forget できること");

    // 会計: 移管した stream が rid2 の受信数 / open 数へ加算される
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .stream_counts
            .incoming_subgroup_count,
        1,
        "移管した stream が rid2 の受信 stream 数に加算されること"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        1,
        "移管した stream が rid2 の open 中の受信 stream 数に加算されること"
    );

    // 移管後も Object が rid2 へ Accepted で届く
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(15))
            .expect("移管後の Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "移管先 rid2 が Object を受信し続けられること"
    );
    assert_eq!(
        largest_received(&client, rid2),
        Some((0, 15)),
        "移管先 rid2 の状態が更新されること"
    );

    // FIN まで会計が一致する
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("移管後の stream 終端の処理に成功すること");
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

/// 生きた帰属先がある stream への再 SUBGROUP_HEADER は破棄吸収せず拒否すること
///
/// キャンセル由来 `Terminated` の所有者でも、Object の帰属実績の参照先が現在も生きた
/// subscription である stream は破棄対象ではない。再 SUBGROUP_HEADER を no-op で吸収すると
/// 帰属先への受信を黙って止めるため、通常どおり protocol violation として拒否する。
#[test]
fn re_subgroup_header_with_live_attribution_is_rejected() {
    const ALIAS: u64 = 3023;
    // rid1 (stream 所有者) は Object ID [0, 9] のみ通し、Object ID=50 は unfiltered の
    // 2 番目の候補 rid2 に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(328);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(50))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=50 は rid2 に帰属すること"
    );

    // 所有者 rid1 をキャンセルしても生きた帰属先 rid2 があるため吸収せず、再
    // SUBGROUP_HEADER を protocol violation として拒否する
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");
    let err = client
        .recv_subgroup_header(stream_id, &header)
        .expect_err("生きた帰属先がある stream への再 SUBGROUP_HEADER は拒否されること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Established);

    // 拒否後も stream は帰属先 rid2 が受信し続けられる
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(60))
            .expect("拒否後の Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "帰属先 rid2 が Object を受信し続けられること"
    );
    assert_eq!(largest_received(&client, rid2), Some((0, 60)));
}

/// 生きた帰属先がある stream への STOP_SENDING 送信は破棄分岐ではなく通常分岐で
/// tracker と会計を処理すること
///
/// キャンセル由来 `Terminated` の所有者でも、帰属先にとって生きた stream は
/// `mark_stop_sending` で終端状態にして再オープンを可能にし、所有者の会計を戻す。
#[test]
fn stop_sending_with_live_attribution_takes_normal_path() {
    const ALIAS: u64 = 3024;
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, MessageParameters::new());

    let stream_id = DataStreamId(329);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
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
    client
        .send_data_stream_stop_sending(stream_id)
        .expect("生きた帰属先がある stream への STOP_SENDING は通常分岐で処理されること");
    assert_eq!(
        client
            .subscription(rid1)
            .expect("所有者 rid1 はキャンセル後も forget まで残ること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "通常分岐で所有者 rid1 の open 中の受信 stream 数が戻ること"
    );
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .state,
        SubscriptionState::Established,
        "帰属先 rid2 は Established のままであること"
    );
    assert_eq!(client.state(), SessionState::Established);

    // tracker が StoppedByPeer として終端されるため、同一 Subgroup の再オープンが受理される
    let stream_id2 = DataStreamId(330);
    assert_eq!(
        recv_header(&mut client, stream_id2, &header),
        TrackDataAcceptance::Accepted,
        "STOP_SENDING 後の同一 Subgroup 再オープンが受理されること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// 移管先の生きた候補が無い場合は従来どおり stream を除去し、保持集合で遅延 Object と
/// 終端を吸収すること
#[test]
fn forget_stream_owner_without_live_candidate_removes_stream() {
    const ALIAS: u64 = 3025;
    let (mut client, rid) = establish_filtered_subscription(ALIAS, MessageParameters::new());

    let stream_id = DataStreamId(331);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(5))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=5 は唯一の候補 rid に帰属すること"
    );

    // rid をキャンセルして forget する。他に生きた候補が無いため stream は除去され、
    // id は破棄対象の保持集合へ移る
    client
        .stop_sending(rid)
        .expect("Established の subscription は stop_sending できること");
    client
        .forget_subscription(rid)
        .expect("キャンセル由来 Terminated は cleanup_ready で forget できること");

    // 保持期間中は遅延 Object / FIN を no-op で吸収する
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(6))
            .expect("forget 後の Object 受信は Err にならないこと"),
        TrackDataAcceptance::Discarded,
        "移管先が無い stream の遅延 Object は Discarded として吸収されること"
    );
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("forget 後の stream 終端は no-op で吸収されること");
    assert_eq!(client.state(), SessionState::Established);
}
