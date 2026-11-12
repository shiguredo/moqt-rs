//! Track 単位の不存在区間 (Location の区間代数) のプロパティテスト
//!
//! `PeerTrackHistory` の区間演算は非公開のため、`Session` の公開 API (FETCH_OK / FETCH
//! 応答の Object と End of Non-Existent Range / FIN) を通して次の性質を検証する。
//!
//! - 要求範囲への切り詰め: 要求範囲外の Location を不存在として記録しない
//! - 2 つの entry の間の gap: 両端を含まず、間にある Location の集合と一致する
//! - 昇順と降順: 同じ要求範囲と同じ配信 Object 集合に対して同じ Location 集合を被覆する
//! - 参照カウント: 最後の request を破棄するまで履歴を保持し、破棄後に解放する
//!
//! 根拠は draft-ietf-moq-transport-23 §3.2.2 (Gaps in a Fetch Stream) / §11.4.2
//! (End of Range) / §2.1.2 (Object States) / §12.1 (Malformed Tracks) 条件 10。
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use super::common::{establish_pair, fetch_ok_params, take_send_on_stream, take_send_request};
use pbt::common::test_runner;
use shiguredo_moqt::message::common::{Location, TrackNamespace};
use shiguredo_moqt::message_parameter::{
    LocationFilter, MessageParameter, MessageParameterValue, MessageParameters, PARAM_GROUP_ORDER,
    PARAM_LOCATION_FILTER,
};
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::{DataStreamId, RequestStreamEnd, SessionError};
use shiguredo_moqt::stream::FETCH_HEADER_TYPE;
use shiguredo_moqt::stream::decoder::{DecodedFetchEntry, DecodedFetchObject};
use shiguredo_moqt::stream::fetch::FetchHeader;
use shiguredo_moqt::track_properties::TrackProperties;

/// テストで使う Location の格子の最大 Group ID
const MAX_GROUP_ID: u64 = 4;
/// テストで使う Location の格子の最大 Object ID
const MAX_OBJECT_ID: u64 = 4;

/// テストで使う Track の名前空間
fn track_namespace() -> TrackNamespace {
    TrackNamespace::new(vec![b"live".to_vec()]).expect("Track Namespace を作れること")
}

/// テストで使う Track 名
fn track_name() -> Vec<u8> {
    b"cam1".to_vec()
}

/// Location を作る
fn loc(group_id: u64, object_id: u64) -> Location {
    Location {
        group_id,
        object_id,
    }
}

/// 指定 Location の Object entry を作る
fn object_entry(location: Location) -> DecodedFetchEntry {
    DecodedFetchEntry::Object(DecodedFetchObject {
        group_id: location.group_id,
        subgroup_id: 0,
        object_id: location.object_id,
        publisher_priority: 128,
        is_datagram_origin: false,
        payload_length: 0,
        properties_bytes: None,
    })
}

/// End of Non-Existent Range の entry を作る
fn end_of_non_existent_range_entry(location: Location) -> DecodedFetchEntry {
    DecodedFetchEntry::EndOfNonExistentRange {
        group_id: location.group_id,
        object_id: location.object_id,
    }
}

/// FETCH の Location Filter と Group Order を組み立てる
///
/// draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter) の Type 0x04
/// (Absolute Range) を使う。`group_order` は 0x1 (Ascending) / 0x2 (Descending) であり、
/// Ascending は省略時の既定値なのでパラメータを載せない (§9.19.8)。
fn range_params(start: Location, end: Location, group_order: u8) -> MessageParameters {
    let end_group_delta = end
        .group_id
        .checked_sub(start.group_id)
        .expect("テスト入力の Start は End 以下であること");
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LocationFilter(LocationFilter::AbsoluteRangeWithEnd {
            start,
            end_group_delta,
            end_object: end.object_id,
        }),
    });
    if group_order == 2 {
        params.push(MessageParameter {
            param_type: PARAM_GROUP_ORDER,
            value: MessageParameterValue::Uint8(group_order),
        });
    }
    params
}

/// FETCH を送信し、publisher 側の Session に受理させる
fn send_fetch(client: &mut Session, server: &mut Session, params: MessageParameters) -> u64 {
    let request_id = client
        .send_fetch(track_namespace(), track_name(), params)
        .expect("FETCH を送信できること");
    let (_, message) = take_send_request(client);
    server
        .recv_request(message)
        .expect("FETCH を受理できること");
    request_id
}

/// FETCH_OK を publisher から subscriber へ届ける
fn send_fetch_ok(client: &mut Session, server: &mut Session, request_id: u64, largest: Location) {
    server
        .send_fetch_ok(
            request_id,
            fetch_ok_params(Some(largest)),
            TrackProperties::new(),
        )
        .expect("FETCH_OK を送信できること");
    let (_, message) = take_send_on_stream(server);
    client
        .recv_stream_message(request_id, message)
        .expect("FETCH_OK を受理できること");
}

/// 受信側の FETCH 応答 stream を開く
fn open_fetch_stream(client: &mut Session, request_id: u64, stream_id: DataStreamId) {
    client
        .recv_data_stream_type(stream_id, FETCH_HEADER_TYPE)
        .expect("FETCH_HEADER の stream type を受理できること");
    client
        .recv_fetch_header(stream_id, &FetchHeader { request_id })
        .expect("FETCH_HEADER を受理できること");
}

/// 受信側の FETCH 応答 stream を FIN で終端する
fn finish_fetch_stream(client: &mut Session, stream_id: DataStreamId) {
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("FIN を受理できること");
}

/// 応答順に entry を並べる
///
/// draft-ietf-moq-transport-23 §3.2.1 (Fetch Object Delivery): Group 内の Object ID は
/// 常に昇順である。Group Order が Descending で Start と End の Group が異なる場合だけ
/// Group ID が降順になる (§3.2.2)。
fn sort_response_order(
    entries: &mut [Location],
    start: Location,
    largest: Location,
    group_order: u8,
) {
    if group_order == 2 && start.group_id != largest.group_id {
        entries.sort_by_key(|location| (core::cmp::Reverse(location.group_id), location.object_id));
    } else {
        entries.sort_by_key(|location| (location.group_id, location.object_id));
    }
}

/// 1 回の FETCH 応答シナリオを実行する
///
/// `entries` は配信する Object の集合 (応答順は内部で整列する)。応答範囲の終端は
/// `largest` (FETCH_OK の LARGEST_OBJECT) で確定させ、最後に FIN で終端する。
fn run_scenario(
    start: Location,
    request_end: Location,
    largest: Location,
    group_order: u8,
    entries: &[Location],
    stream_id: DataStreamId,
) -> (Session, Session) {
    let (mut client, mut server) = establish_pair();
    let request_id = send_fetch(
        &mut client,
        &mut server,
        range_params(start, request_end, group_order),
    );
    send_fetch_ok(&mut client, &mut server, request_id, largest);
    open_fetch_stream(&mut client, request_id, stream_id);
    let mut ordered = entries.to_vec();
    sort_response_order(&mut ordered, start, largest, group_order);
    for location in ordered {
        client
            .recv_fetch_entry(stream_id, &object_entry(location))
            .expect("配信された Object は受理されること");
    }
    finish_fetch_stream(&mut client, stream_id);
    (client, server)
}

/// 同じ Track に新しい FETCH を作り、指定 Location の Object を受信する
///
/// 履歴に不存在として記録されていれば `Err`、そうでなければ `Ok` を返す。FETCH の要求範囲は
/// 探査する Location だけに絞り、この FETCH 自身が新しい gap を記録しないようにする。
/// 戻り値の request ID は、探査用 FETCH の参照を明示的に解放するために使う。
fn probe_location(
    client: &mut Session,
    server: &mut Session,
    stream_id: DataStreamId,
    location: Location,
) -> (u64, Result<(), SessionError>) {
    let request_id = send_fetch(client, server, range_params(location, location, 1));
    send_fetch_ok(client, server, request_id, location);
    open_fetch_stream(client, request_id, stream_id);
    (
        request_id,
        client.recv_fetch_entry(stream_id, &object_entry(location)),
    )
}

/// 格子から任意の Location を生成する
fn sample_location(ctx: &mut noprop::TestCaseContext) -> Location {
    loc(
        noprop::sample_u64_in(ctx, 0..=MAX_GROUP_ID),
        noprop::sample_u64_in(ctx, 0..=MAX_OBJECT_ID),
    )
}

/// 格子から Start <= End を満たす要求範囲を生成する
fn sample_range(ctx: &mut noprop::TestCaseContext) -> (Location, Location) {
    let mut start_group = noprop::sample_u64_in(ctx, 0..=MAX_GROUP_ID);
    let mut end_group = noprop::sample_u64_in(ctx, 0..=MAX_GROUP_ID);
    if start_group > end_group {
        core::mem::swap(&mut start_group, &mut end_group);
    }
    let mut start_object = noprop::sample_u64_in(ctx, 0..=MAX_OBJECT_ID);
    let mut end_object = noprop::sample_u64_in(ctx, 0..=MAX_OBJECT_ID);
    if start_group == end_group && start_object > end_object {
        core::mem::swap(&mut start_object, &mut end_object);
    }
    (loc(start_group, start_object), loc(end_group, end_object))
}

/// `[start, end]` の内側の Location を生成する
fn sample_in_range(ctx: &mut noprop::TestCaseContext, start: Location, end: Location) -> Location {
    let group_id = noprop::sample_u64_in(ctx, start.group_id..=end.group_id);
    let object_id = if group_id == start.group_id && group_id == end.group_id {
        noprop::sample_u64_in(ctx, start.object_id..=end.object_id)
    } else if group_id == start.group_id {
        noprop::sample_u64_in(ctx, start.object_id..=MAX_OBJECT_ID)
    } else if group_id == end.group_id {
        noprop::sample_u64_in(ctx, 0..=end.object_id)
    } else {
        noprop::sample_u64_in(ctx, 0..=MAX_OBJECT_ID)
    };
    loc(group_id, object_id)
}

/// `[start, largest]` の内側で重複のない Object 集合を生成する
fn sample_entries(
    ctx: &mut noprop::TestCaseContext,
    start: Location,
    largest: Location,
) -> Vec<Location> {
    let count = noprop::sample_usize_in(ctx, 0..=6);
    let mut entries = Vec::new();
    for _ in 0..count {
        let location = sample_in_range(ctx, start, largest);
        if !entries.contains(&location) {
            entries.push(location);
        }
    }
    entries
}

/// `[start, largest]` の内側で未配信の Location かを返す (不存在の期待値)
fn is_missing(
    location: Location,
    start: Location,
    largest: Location,
    entries: &[Location],
) -> bool {
    start <= location && location <= largest && !entries.contains(&location)
}

/// 不存在として記録された Object の受信エラーか
fn is_non_existent_object_error(err: &SessionError) -> bool {
    // `MessageError::MalformedTrack` の Display が "malformed track: " を付けるため、
    // reason 自体は接頭辞を含まない
    err.reason == "object received in a range recorded as non-existent"
}

/// 要求範囲外の Location は不存在として記録されない
///
/// draft-ietf-moq-transport-23 §3.2.2 (Gaps in a Fetch Stream) の gap は要求範囲の内側だけを
/// 対象とする。応答終端 (LARGEST_OBJECT) から要求 End までの未配信 Location も、応答が
/// そこまで届かない以上、不存在として記録してはならない。
#[test]
fn locations_outside_the_request_are_not_recorded_as_missing() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let (start, request_end) = sample_range(ctx);
        let largest = sample_in_range(ctx, start, request_end);
        let group_order = if noprop::sample_bool(ctx) { 1 } else { 2 };
        let entries = sample_entries(ctx, start, largest);
        let (mut client, mut server) = run_scenario(
            start,
            request_end,
            largest,
            group_order,
            &entries,
            DataStreamId(1),
        );

        // 要求範囲の外側 (Start より前または LARGEST_OBJECT より後ろ) を探す
        let mut outside = None;
        for group_id in 0..=MAX_GROUP_ID {
            for object_id in 0..=MAX_OBJECT_ID {
                let candidate = loc(group_id, object_id);
                if candidate < start || largest < candidate {
                    outside = Some(candidate);
                    break;
                }
            }
            if outside.is_some() {
                break;
            }
        }
        let Some(outside) = outside else {
            // 格子全体が要求範囲に収まる場合は検証対象が無い
            return Ok(());
        };
        probe_location(&mut client, &mut server, DataStreamId(2), outside)
            .1
            .expect("要求範囲外の Location は誤って不存在にされないこと");
        Ok(())
    })?;
    Ok(())
}

/// 2 つの entry の間の gap は両端を含まず、間の Location 集合と一致する
///
/// draft-ietf-moq-transport-23 §3.2.2 (Gaps in a Fetch Stream): 応答内の連続しない Object の
/// 間は、Range Filters が無ければ不存在である。§11.4.2 (End of Range) の End of Range は
/// 「直前の Object から指示の Location まで (両端を含む)」であり、その境界の扱いと区別する。
#[test]
fn gap_between_entries_excludes_both_endpoints() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let (start, end) = sample_range(ctx);
        if start == end {
            return Ok(());
        }
        let group_order = if noprop::sample_bool(ctx) { 1 } else { 2 };
        // 両端を Object として配信し、その間だけを gap にする
        let entries = [start, end];
        let between: Vec<Location> = {
            let mut locations = Vec::new();
            for group_id in 0..=MAX_GROUP_ID {
                for object_id in 0..=MAX_OBJECT_ID {
                    let candidate = loc(group_id, object_id);
                    if start < candidate && candidate < end {
                        locations.push(candidate);
                    }
                }
            }
            locations
        };
        if between.is_empty() {
            return Ok(());
        }
        let probe = between[noprop::sample_usize_in(ctx, 0..between.len())];
        let (mut client, mut server) =
            run_scenario(start, end, end, group_order, &entries, DataStreamId(1));
        let err = probe_location(&mut client, &mut server, DataStreamId(2), probe)
            .1
            .expect_err("2 つの entry の間の Location は不存在として拒否されること");
        assert!(
            is_non_existent_object_error(&err),
            "不存在区間の Object として Malformed Track になること: {err:?}"
        );

        // 端点は配信済み Object であり、再受信しても不存在としては拒否されない
        let (mut client, mut server) =
            run_scenario(start, end, end, group_order, &entries, DataStreamId(1));
        probe_location(&mut client, &mut server, DataStreamId(2), start)
            .1
            .expect("先頭の entry は不存在として拒否されないこと");
        let (mut client, mut server) =
            run_scenario(start, end, end, group_order, &entries, DataStreamId(1));
        probe_location(&mut client, &mut server, DataStreamId(2), end)
            .1
            .expect("末尾の entry は不存在として拒否されないこと");
        Ok(())
    })?;
    Ok(())
}

/// 昇順と降順は同じ要求範囲を同じ Location 集合で被覆する
///
/// draft-ietf-moq-transport-23 §3.2.2 (Gaps in a Fetch Stream) は、降順 multi-group でも
/// 「先行 Group の残り・飛ばした Group・後続 Group の先頭」を gap と定める。したがって
/// 同じ要求範囲と同じ配信 Object 集合に対して、順序によらず不存在の集合は一致しなければ
/// ならない。
#[test]
fn ascending_and_descending_cover_the_same_locations() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let (start, request_end) = sample_range(ctx);
        let largest = sample_in_range(ctx, start, request_end);
        let entries = sample_entries(ctx, start, largest);
        let probe = sample_location(ctx);
        let expected = is_missing(probe, start, largest, &entries);

        let (mut asc_client, mut asc_server) =
            run_scenario(start, request_end, largest, 1, &entries, DataStreamId(1));
        let (_, asc) = probe_location(&mut asc_client, &mut asc_server, DataStreamId(2), probe);
        let (mut desc_client, mut desc_server) =
            run_scenario(start, request_end, largest, 2, &entries, DataStreamId(1));
        let (_, desc) = probe_location(&mut desc_client, &mut desc_server, DataStreamId(2), probe);

        assert_eq!(
            asc.is_err(),
            expected,
            "昇順の不存在判定が期待値と一致すること: probe={probe:?} start={start:?} \
             largest={largest:?} entries={entries:?} result={asc:?}"
        );
        assert_eq!(
            desc.is_err(),
            expected,
            "降順の不存在判定が期待値と一致すること: probe={probe:?} start={start:?} \
             largest={largest:?} entries={entries:?} result={desc:?}"
        );
        if let Err(err) = asc {
            assert!(
                is_non_existent_object_error(&err),
                "昇順でも不存在区間の Object として Malformed Track になること: {err:?}"
            );
        }
        if let Err(err) = desc {
            assert!(
                is_non_existent_object_error(&err),
                "降順でも不存在区間の Object として Malformed Track になること: {err:?}"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// Track 履歴は最後の request を破棄するまで保持され、破棄後に解放される
///
/// 同じ Track の履歴は別の FETCH が参照している間は保持され、全ての参照が外れた後に
/// 新しい FETCH が引き継いではならない。どちらの場合も公開 API の観測結果で検証する。
#[test]
fn track_history_lifetime_follows_request_references() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(96, |ctx| {
        let fetch_count = noprop::sample_usize_in(ctx, 2..=5);
        let location = sample_location(ctx);
        let (mut client, mut server) = establish_pair();
        let mut request_ids = Vec::new();
        for _ in 0..fetch_count {
            let request_id = send_fetch(
                &mut client,
                &mut server,
                range_params(location, location, 1),
            );
            send_fetch_ok(&mut client, &mut server, request_id, location);
            request_ids.push(request_id);
        }
        // 先頭の FETCH だけで不存在を記録する
        let first = request_ids[0];
        open_fetch_stream(&mut client, first, DataStreamId(1));
        client
            .recv_fetch_entry(DataStreamId(1), &end_of_non_existent_range_entry(location))
            .expect("End of Non-Existent Range を受理できること");
        finish_fetch_stream(&mut client, DataStreamId(1));

        // 残りの FETCH は request ID だけの終端通知で破棄可能な状態にする
        for &request_id in &request_ids[1..] {
            client
                .recv_fetch_data_stream_closed(request_id, RequestStreamEnd::Fin)
                .expect("request ID だけの FIN を受理できること");
        }
        // 最後の 1 件だけを残して破棄する。共有履歴はまだ保持されていなければならない
        for &request_id in &request_ids[..fetch_count - 1] {
            client
                .forget_fetch(request_id)
                .expect("終端済み FETCH は破棄できること");
        }
        let (probe_request_id, shared_result) =
            probe_location(&mut client, &mut server, DataStreamId(2), location);
        let shared_error =
            shared_result.expect_err("他の request が参照している間は不存在を検出し続けること");
        assert!(
            is_non_existent_object_error(&shared_error),
            "共有履歴の不存在区間として Malformed Track になること: {shared_error:?}"
        );

        // 最後の参照を外すと履歴は解放される。この時点で残りの FETCH は malformed 終端で
        // Terminated になっているため、そのまま破棄できる
        let remaining = request_ids[fetch_count - 1];
        client
            .forget_fetch(remaining)
            .expect("終端済み FETCH は破棄できること");
        client
            .forget_fetch(probe_request_id)
            .expect("探査用の終端済み FETCH も破棄できること");
        let fresh = send_fetch(
            &mut client,
            &mut server,
            range_params(location, location, 1),
        );
        send_fetch_ok(&mut client, &mut server, fresh, location);
        open_fetch_stream(&mut client, fresh, DataStreamId(3));
        client
            .recv_fetch_entry(DataStreamId(3), &object_entry(location))
            .expect("最後の参照の破棄後に履歴が解放されること");
        Ok(())
    })?;
    Ok(())
}
