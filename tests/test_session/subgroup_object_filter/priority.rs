//! PRIORITY_FILTER を候補ごとの SUBGROUP_HEADER priority で解決するテスト

use super::*;
use shiguredo_moqt::message_parameter::{PARAM_OBJECTID_FILTER, PARAM_PRIORITY_FILTER};

/// PRIORITY_FILTER は wire の SUBGROUP_HEADER priority を候補ごとに解決して評価すること
///
/// 共有 Track Alias では `Subscription::publisher_priority` が別の stream の header で
/// 上書きされうるため、購読単位の直近値ではなく SUBGROUP_HEADER が持つ Publisher Priority を
/// 候補ごとに `resolve_header_publisher_priority` で解決して評価する
/// (header 時の `header_passes_filters` と同じ規則)。
#[test]
fn priority_filter_uses_stream_header_priority_for_each_candidate() {
    const ALIAS: u64 = 3009;
    // rid1 は Object ID [0, 9] のみ通す。header 時点の Object 単位フィルタは評価されない
    // ため、header は最初の候補 rid1 に紐づく
    let mut params1 = MessageParameters::new();
    params1.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 9));
    // rid2 は Publisher Priority [100, 110] のみ通す
    let mut params2 = MessageParameters::new();
    params2.push(range_filter(PARAM_PRIORITY_FILTER, 0, 100, 110));
    let (mut client, rid1, rid2) = establish_shared_alias(ALIAS, params1, params2);

    let stream_id = DataStreamId(311);
    let header = SubgroupHeader {
        track_alias: ALIAS,
        publisher_priority: Some(105),
        ..subgroup_header()
    };
    assert_eq!(
        recv_header(&mut client, stream_id, &header),
        TrackDataAcceptance::Accepted
    );

    // Object ID=50 は rid1 の [0, 9] に該当しない。rid2 には header の priority 105 が
    // 適用されて PRIORITY_FILTER を通過する (購読単位の直近値 128 だと不通過になる)
    assert_eq!(
        client
            .recv_subgroup_object(stream_id, &normal_object(50))
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "header priority 105 で rid2 の PRIORITY_FILTER を通過すること"
    );
    assert_eq!(
        largest_received(&client, rid1),
        None,
        "rid1 は Object 単位フィルタで不合格のため更新されないこと"
    );
    assert_eq!(
        largest_received(&client, rid2),
        Some((0, 50)),
        "rid2 に帰属されること"
    );
}
