//! FETCH 状態管理のプロパティテスト
//!
//! - 複数 FETCH の確立
//! - publisher 側の largest_location 非保存 (draft-20 で MUST 削除)
//! - FETCH の INVALID_RANGE 判定
//! - 相対 Location Filter を持つ単独 FETCH の拒否
//! - REQUEST_ERROR 後の Terminated と forget_fetch
//!
//! (draft-20 で Joining FETCH は廃止された)

use pbt::common::test_runner;
use shiguredo_moqt::message::{
    ControlMessage,
    common::{Location, TrackNamespace},
};
use shiguredo_moqt::message_parameter::LocationFilter;
use shiguredo_moqt::message_parameter::{
    MessageParameter, MessageParameterValue, MessageParameters, PARAM_LOCATION_FILTER,
};
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::DataStreamId;
use shiguredo_moqt::session::types::FetchState;
use shiguredo_moqt::stream::subgroup::{SubgroupHeader, SubgroupIdMode};
use shiguredo_moqt::track_properties::TrackProperties;

use super::common::{establish_pair, fetch_ok_params, take_send_on_stream, take_send_request};

/// FETCH の range 指定 (AbsoluteRangeWithEnd) を持つ MessageParameters を作る
///
/// draft-ietf-moq-transport-23 §9.11 (FETCH): range は LOCATION_FILTER で指定する。
/// `end.group_id < start.group_id` の場合は delta が求まらないため panic する
/// (テストフィクスチャの前提)。
fn fetch_range_params(start: Location, end: Location) -> MessageParameters {
    let end_group_delta = end.group_id - start.group_id;
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LocationFilter(LocationFilter::AbsoluteRangeWithEnd {
            start,
            end_group_delta,
            end_object: end.object_id,
        }),
    });
    params
}

// 複数の FETCH を発行 → 応答で全て Established に至る
#[test]
fn multiple_fetches_all_establish() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let count = noprop::sample_usize_in(ctx, 1..8) as u32;
        let (mut client, mut server) = establish_pair();
        let mut rids = Vec::new();
        for i in 0..count {
            let rid = client
                .send_fetch(
                    TrackNamespace::new(vec![format!("ns{i}").into_bytes()])
                        .expect("テストフィクスチャの前提条件を満たす"),
                    format!("t{i}").into_bytes(),
                    fetch_range_params(
                        Location {
                            group_id: 0,
                            object_id: 0,
                        },
                        Location {
                            group_id: u64::from(i) + 1,
                            object_id: 0,
                        },
                    ),
                )
                .expect("テストフィクスチャの前提条件を満たす");
            rids.push(rid);
            let (_, m) = take_send_request(&mut client);
            server
                .recv_request(m)
                .expect("テストフィクスチャの前提条件を満たす");
            server
                .send_fetch_ok(
                    rid,
                    fetch_ok_params(Some(Location {
                        group_id: u64::from(i) + 1,
                        object_id: 0,
                    })),
                    TrackProperties::new(),
                )
                .expect("テストフィクスチャの前提条件を満たす");
            let (_, ok) = take_send_on_stream(&mut server);
            client
                .recv_stream_message(rid, ok)
                .expect("テストフィクスチャの前提条件を満たす");
        }
        for rid in rids {
            assert_eq!(
                client
                    .fetch(rid)
                    .expect("テストフィクスチャの前提条件を満たす")
                    .state,
                FetchState::Established
            );
            assert_eq!(
                server
                    .fetch(rid)
                    .expect("テストフィクスチャの前提条件を満たす")
                    .state,
                FetchState::Established
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// draft-20 で publisher 側の Largest Location 保持 MUST 要件は
/// 削除された。publisher 側の `largest_location` は REQUEST_UPDATE_OK の
/// LARGEST_OBJECT でも保存されない (自側の観測値は `largest_received_location` で追跡する)。
/// 確立経路 (SUBSCRIBE_OK / PUBLISH) の非保存は単体テストで担保する。
///
/// (初期 PAUSED, REQUEST_UPDATE での PAUSED) の組を生成し、購読の再開 (PAUSED 1→0)
/// を含めても保存されないことを検証する。
#[test]
fn publisher_largest_location_never_saved_on_request_update_ok() -> noprop::TestResult {
    // 再開 (PAUSED 1→0) とそれ以外の両方の分岐の観測をカバレッジゲートで検証する
    let transition_seen = std::cell::Cell::new(false);
    let other_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        use shiguredo_moqt::message_parameter::{
            MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT, PARAM_PAUSED,
        };
        let initial_paused = noprop::sample_choice(ctx, &[0u8, 1u8]);
        let update_paused = noprop::sample_choice(ctx, &[0u8, 1u8]);
        let g = noprop::sample_u64_in(ctx, 0..1000);
        let o = noprop::sample_u64_in(ctx, 0..1000);
        let (mut client, mut server) = establish_pair();
        // initial_paused で SUBSCRIBE → publisher (server) が同じ PAUSED で確立する
        let mut sub_params = MessageParameters::new();
        sub_params.push(MessageParameter {
            param_type: PARAM_PAUSED,
            value: MessageParameterValue::Uint8(initial_paused),
        });
        let rid = client
            .send_subscribe(
                TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                b"cam".to_vec(),
                sub_params,
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, sub_msg) = take_send_request(&mut client);
        server
            .recv_request(sub_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        // SUBSCRIBE_OK では LARGEST_OBJECT を載せない → establishment 契機の save を排除し、
        // REQUEST_UPDATE_OK 経路の gate のみを観測する。確立直後は largest_location == None。
        server
            .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok_msg) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(rid, ok_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        assert!(
            server
                .subscription(rid)
                .expect("テストフィクスチャの前提条件を満たす")
                .largest_location
                .is_none()
        );
        // REQUEST_UPDATE で PAUSED=update_paused を送る
        let mut upd_params = MessageParameters::new();
        upd_params.push(MessageParameter {
            param_type: PARAM_PAUSED,
            value: MessageParameterValue::Uint8(update_paused),
        });
        client
            .send_request_update(rid, upd_params)
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, upd_msg) = take_send_on_stream(&mut client);
        server
            .recv_stream_message(rid, upd_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        // REQUEST_OK に LARGEST_OBJECT {g, o} を載せて応答する
        let mut req_ok_params = MessageParameters::new();
        req_ok_params.push(MessageParameter {
            param_type: PARAM_LARGEST_OBJECT,
            value: MessageParameterValue::Location {
                group: g,
                object: o,
            },
        });
        server
            .send_request_ok(rid, req_ok_params, TrackProperties::default())
            .expect("テストフィクスチャの前提条件を満たす");
        // publisher 側は再開 (PAUSED 1→0) を含めても largest_location を保存しない
        if initial_paused == 1 && update_paused == 0 {
            transition_seen.set(true);
        } else {
            other_seen.set(true);
        }
        assert_eq!(
            server
                .subscription(rid)
                .expect("テストフィクスチャの前提条件を満たす")
                .paused,
            update_paused == 1,
            "REQUEST_UPDATE の PAUSED が publisher 側の状態へ反映されること"
        );
        assert!(
            server
                .subscription(rid)
                .expect("テストフィクスチャの前提条件を満たす")
                .largest_location
                .is_none(),
            "publisher 側は largest_location を保存しないこと"
        );
        Ok(())
    })?;
    assert!(
        transition_seen.get(),
        "購読の再開 (PAUSED 1→0) のケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        other_seen.get(),
        "購読の再開以外のケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// draft-ietf-moq-transport-23 §3.2 (Fetch): FETCH は Start Location が対象トラックの観測
/// Largest (effective_largest_object) を上回る (start > largest) ときに限り INVALID_RANGE で
/// 拒否される。範囲内 (start <= largest) では受理される。比較は Location の (group, object) 辞書式。
/// 確立経路 (SUBSCRIBE_OK / REQUEST_UPDATE_OK) の publisher 側保存は廃止されたため、
/// 観測値は公開 Object で確定させる。
#[test]
fn fetch_invalid_range_iff_start_exceeds_largest() -> noprop::TestResult {
    // INVALID_RANGE で拒否される分岐と受理される分岐の両方の観測を
    // カバレッジゲートで検証する
    let rejected_seen = std::cell::Cell::new(false);
    let accepted_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        use shiguredo_moqt::error::REQUEST_INVALID_RANGE;
        let lg = noprop::sample_u64_in(ctx, 0..1000);
        let lo = noprop::sample_u64_in(ctx, 0..1000);
        let sg = noprop::sample_u64_in(ctx, 0..1000);
        let so = noprop::sample_u64_in(ctx, 0..1000);
        let (mut client, mut server) = establish_pair();
        let sub_rid = client
            .send_subscribe(
                TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                b"cam".to_vec(),
                MessageParameters::new(),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, sub_msg) = take_send_request(&mut client);
        server
            .recv_request(sub_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        // SUBSCRIBE_OK で確立し、{lg, lo} を公開して server の観測 Largest を確定させる
        // (draft-20 で publisher 側の LARGEST_OBJECT 保存は廃止されたため、
        // 公開 Object で観測値を確定させる)
        server
            .send_subscribe_ok(sub_rid, 1, MessageParameters::new(), TrackProperties::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok_msg) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(sub_rid, ok_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        let stream_id = shiguredo_moqt::session::types::DataStreamId(1);
        server
            .send_subgroup_header(
                stream_id,
                sub_rid,
                &shiguredo_moqt::stream::subgroup::SubgroupHeader {
                    track_alias: 1,
                    group_id: lg,
                    subgroup_id: shiguredo_moqt::stream::subgroup::SubgroupIdMode::Explicit(0),
                    publisher_priority: Some(128),
                    has_properties: false,
                    end_of_group: false,
                    first_object: false,
                },
            )
            .expect("テストフィクスチャの前提条件を満たす");
        server
            .send_subgroup_object(stream_id, lo, None)
            .expect("テストフィクスチャの前提条件を満たす");
        // FETCH: Start = End = {sg, so} (End == Start は wire 上受理される)
        let fetch_rid = client
            .send_fetch(
                TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                b"cam".to_vec(),
                fetch_range_params(
                    Location {
                        group_id: sg,
                        object_id: so,
                    },
                    Location {
                        group_id: sg,
                        object_id: so,
                    },
                ),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, fetch_msg) = take_send_request(&mut client);
        server
            .recv_request(fetch_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        if (sg, so) > (lg, lo) {
            // 範囲外: Fetch は作られず INVALID_RANGE で拒否される
            assert!(server.fetch(fetch_rid).is_none());
            rejected_seen.set(true);
            let (_, err_msg) = take_send_on_stream(&mut server);
            match err_msg {
                ControlMessage::RequestError(e) => {
                    assert_eq!(e.error_code, REQUEST_INVALID_RANGE);
                }
                _ => panic!("RequestError(INVALID_RANGE) が期待された"),
            }
        } else {
            // 範囲内: Fetch は受理される
            assert!(server.fetch(fetch_rid).is_some());
            accepted_seen.set(true);
        }
        Ok(())
    })?;
    assert!(
        rejected_seen.get(),
        "FETCH が INVALID_RANGE で拒否されるケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        accepted_seen.get(),
        "FETCH が受理されるケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

// REQUEST_ERROR で fetch は Terminated、forget_fetch で完全削除
#[test]
fn fetch_request_error_then_forget() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let start_group = noprop::sample_u64_in(ctx, 0..100);
        let (mut client, _) = establish_pair();
        let rid = client
            .send_fetch(
                TrackNamespace::new(vec![b"n".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                b"t".to_vec(),
                fetch_range_params(
                    Location {
                        group_id: start_group,
                        object_id: 0,
                    },
                    Location {
                        group_id: start_group + 1,
                        object_id: 0,
                    },
                ),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let _ = take_send_request(&mut client);
        client
            .recv_stream_message(
                rid,
                ControlMessage::RequestError(shiguredo_moqt::message::RequestError {
                    error_code: 0,
                    retry_interval: 0,
                    reason: shiguredo_moqt::message::ReasonPhrase::new(String::new())
                        .expect("テストフィクスチャの前提条件を満たす"),
                    error_details: shiguredo_moqt::message::ErrorDetails::new(),
                }),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            client
                .fetch(rid)
                .expect("テストフィクスチャの前提条件を満たす")
                .state,
            FetchState::Terminated
        );
        let removed = client
            .forget_fetch(rid)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(removed.request_id, rid);
        assert!(client.fetch(rid).is_none());
        Ok(())
    })?;
    Ok(())
}

/// LOCATION_FILTER 1 件だけを持つ FETCH 用パラメータを作る
fn params_with_location_filter(filter: LocationFilter) -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LocationFilter(filter),
    });
    params
}

/// SUBSCRIBE / SUBSCRIBE_OK を確立し、publisher (server) 側で Object を 1 件公開する
///
/// 公開した Object の Location が publisher の観測 Largest Object になる。返り値は
/// (subscriber, publisher, subscription の Request ID)。
fn establish_published_track(group_id: u64, object_id: u64) -> (Session, Session, u64) {
    let (mut client, mut server) = establish_pair();
    let sub_rid = client
        .send_subscribe(
            TrackNamespace::new(vec![b"live".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            b"cam".to_vec(),
            MessageParameters::new(),
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
    let stream_id = DataStreamId(1);
    server
        .send_subgroup_header(
            stream_id,
            sub_rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, object_id, None)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server, sub_rid)
}

/// draft-ietf-moq-transport-23 §3.2 (Fetch): 相対 Location Filter (Type 0x01 / 0x05) を
/// 持つ単独 FETCH は、Largest Object の観測有無にかかわらず REQUEST_ERROR
/// (INVALID_RANGE) で拒否される。拒否時は Fetch state を作らず、セッションも維持する。
///
/// 判定は解決後の Location ではなくフィルタの形式で行う。相対 Start は Largest Object を
/// 基準に範囲内へ解決でき、Next Object は Largest Object 未観測だと `{0, 0}` に解決される
/// ため、どちらの形式も「解決値が範囲内になりうる」条件で拒否を確認できる。
#[test]
fn fetch_relative_filter_always_rejected_with_invalid_range() -> noprop::TestResult {
    let relative_start_seen = std::cell::Cell::new(false);
    let next_object_seen = std::cell::Cell::new(false);
    let published_seen = std::cell::Cell::new(false);
    let unpublished_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        use shiguredo_moqt::error::REQUEST_INVALID_RANGE;
        use shiguredo_moqt::message::Fetch as WireFetch;
        use shiguredo_moqt::session::types::SessionState;
        let group_id = noprop::sample_u64_in(ctx, 0..1000);
        let object_id = noprop::sample_u64_in(ctx, 0..1000);
        let use_next_object = noprop::sample_bool(ctx);
        // Largest Object を観測済みにするか、subscription 自体を作らず未観測のままにするか
        let published = noprop::sample_bool(ctx);
        let (_client, mut server, request_id) = if published {
            let (client, server, _sub_rid) = establish_published_track(group_id, object_id);
            // client は SUBSCRIBE で 0 を使うため、重複しない even の 2 を使う
            (client, server, 2)
        } else {
            let (client, server) = establish_pair();
            (client, server, 0)
        };
        let params = if use_next_object {
            params_with_location_filter(LocationFilter::NextObject)
        } else {
            params_with_location_filter(LocationFilter::RelativeGroup {
                start_group: noprop::sample_u64_in(ctx, 0..=group_id + 1),
            })
        };
        server
            .recv_request(ControlMessage::Fetch(WireFetch {
                request_id,
                track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                track_name: b"cam".to_vec(),
                parameters: params,
            }))
            .expect("相対フィルタの拒否は Ok で返り、セッションは維持される");
        assert_eq!(
            server.state(),
            SessionState::Established,
            "相対フィルタの拒否でセッションを閉じないこと"
        );
        assert!(
            server.fetch(request_id).is_none(),
            "拒否時に Fetch state を作らないこと"
        );
        let (_, message) = take_send_on_stream(&mut server);
        match message {
            ControlMessage::RequestError(error) => {
                assert_eq!(error.error_code, REQUEST_INVALID_RANGE);
                assert_eq!(
                    error.reason.as_str(),
                    "relative location filter is not allowed in FETCH",
                    "範囲判定ではなく相対フィルタの形式を理由に拒否すること"
                );
            }
            other => panic!("INVALID_RANGE の RequestError が期待されたが {other:?}"),
        }
        if use_next_object {
            next_object_seen.set(true);
        } else {
            relative_start_seen.set(true);
        }
        if published {
            published_seen.set(true);
        } else {
            unpublished_seen.set(true);
        }
        Ok(())
    })?;
    assert!(
        relative_start_seen.get(),
        "相対 Start の拒否ケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        next_object_seen.get(),
        "Next Object の拒否ケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        published_seen.get(),
        "Largest Object 観測済みの拒否ケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        unpublished_seen.get(),
        "Largest Object 未観測の拒否ケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// draft-ietf-moq-transport-23 §3.2 (Fetch) の相対フィルタ禁止はフィルタの形式が相対の
/// 場合だけ適用される。絶対値で範囲が定まる形式 (省略 / Type 0x00 / 0x02 / 0x03 / 0x04) は
/// 従来どおり範囲判定を受け、Largest Object 以下の範囲は受理される。
#[test]
fn fetch_non_relative_filter_accepted_within_largest() -> noprop::TestResult {
    let unfiltered_seen = std::cell::Cell::new(false);
    let absolute_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        use shiguredo_moqt::session::types::SessionState;
        let group_id = noprop::sample_u64_in(ctx, 0..1000);
        let object_id = noprop::sample_u64_in(ctx, 0..1000);
        let (_client, mut server, _sub_rid) = establish_published_track(group_id, object_id);
        // Largest Object 以下に収まる開始 Location を選ぶ (範囲判定で受理される条件)
        let start_group = noprop::sample_u64_in(ctx, 0..=group_id);
        let start_object = if start_group == group_id {
            noprop::sample_u64_in(ctx, 0..=object_id)
        } else {
            noprop::sample_u64_in(ctx, 0..1000)
        };
        let start = Location {
            group_id: start_group,
            object_id: start_object,
        };
        let form = noprop::sample_usize_in(ctx, 0..5);
        let params = match form {
            // 省略は unfiltered として受理される
            0 => MessageParameters::new(),
            // Type 0x00 (None) は no filter として受理される
            1 => params_with_location_filter(LocationFilter::NoFilter),
            // Type 0x02 (Absolute Start): End は Largest Object
            2 => params_with_location_filter(LocationFilter::AbsoluteStart { start }),
            // Type 0x03 (Absolute Start, Group End): End は End Group の末尾
            3 => params_with_location_filter(LocationFilter::AbsoluteRange {
                start,
                end_group_delta: noprop::sample_u64_in(ctx, 0..=3),
            }),
            // Type 0x04 (Absolute Range): End Group を Start Group より後ろにして
            // 空 range を避ける
            _ => params_with_location_filter(LocationFilter::AbsoluteRangeWithEnd {
                start,
                end_group_delta: 1 + noprop::sample_u64_in(ctx, 0..=3),
                end_object: noprop::sample_u64_in(ctx, 0..1000),
            }),
        };
        // client は SUBSCRIBE で 0 を使うため、重複しない even の 2 を使う
        server
            .recv_request(ControlMessage::Fetch(shiguredo_moqt::message::Fetch {
                request_id: 2,
                track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                track_name: b"cam".to_vec(),
                parameters: params,
            }))
            .expect("絶対値で範囲が定まる形式は受理されること");
        assert_eq!(
            server.state(),
            SessionState::Established,
            "受理時にセッションを閉じないこと"
        );
        let fetch = server.fetch(2);
        assert!(fetch.is_some(), "絶対値で範囲が定まる形式は受理されること");
        assert_eq!(
            fetch.expect("直前に存在を確認した").state,
            FetchState::Pending,
            "受理時は Pending になること"
        );
        if form <= 1 {
            unfiltered_seen.set(true);
        } else {
            absolute_seen.set(true);
        }
        Ok(())
    })?;
    assert!(
        unfiltered_seen.get(),
        "省略 / Type 0x00 の受理ケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        absolute_seen.get(),
        "絶対値フィルタの受理ケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// 相対 Location Filter を持つ単独 FETCH は送信 API が拒否し、Request ID を消費しない
///
/// draft-ietf-moq-transport-23 §3.2 (Fetch) により peer が必ず INVALID_RANGE で拒否する
/// メッセージを無警告で送出しないよう、送信前に自側で弾く。request_id 発行前に拒否する
/// ため欠番を作らず、セッションも閉じない。
#[test]
fn send_fetch_relative_filter_rejected_without_side_effects() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        use shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION;
        use shiguredo_moqt::session::types::SendRequestError;
        use shiguredo_moqt::session::types::SessionEvent;
        use shiguredo_moqt::session::types::SessionState;
        let (mut client, _server) = establish_pair();
        let filter = if noprop::sample_bool(ctx) {
            LocationFilter::RelativeGroup {
                start_group: noprop::sample_u64_in(ctx, 0..1000),
            }
        } else {
            LocationFilter::NextObject
        };
        let err = client
            .send_fetch(
                TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                b"cam".to_vec(),
                params_with_location_filter(filter),
            )
            .expect_err("相対 Location Filter の単独 FETCH は送信できないこと");
        let SendRequestError::Session(err) = err else {
            panic!("SendRequestError::Session が期待されたが {err:?}");
        };
        assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
        assert_eq!(
            err.reason, "relative location filter is not allowed in FETCH",
            "拒否理由が相対フィルタであることを示すこと"
        );
        // 拒否した送信で SendRequest / CloseSession は発行されない
        while let Some(e) = client.poll_event() {
            assert!(
                !matches!(
                    e,
                    SessionEvent::SendRequest { .. } | SessionEvent::CloseSession(_)
                ),
                "拒否した送信で SendRequest / CloseSession は発行されないこと"
            );
        }
        assert_eq!(
            client.state(),
            SessionState::Established,
            "送信側の検証エラーでセッションを閉じないこと"
        );
        let group_id = noprop::sample_u64_in(ctx, 0..1000);
        // Request ID を消費していない: 拒否後の最初の送信は 0 のまま
        let rid = client
            .send_fetch(
                TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                b"cam".to_vec(),
                fetch_range_params(
                    Location {
                        group_id,
                        object_id: 0,
                    },
                    Location {
                        group_id: group_id + 1,
                        object_id: 0,
                    },
                ),
            )
            .expect("拒否後も絶対範囲の FETCH を送信できること");
        assert_eq!(
            rid, 0,
            "拒否した送信は Request ID を消費しないこと (欠番を作らない)"
        );
        let (sent_rid, _) = take_send_request(&mut client);
        assert_eq!(sent_rid, rid);
        Ok(())
    })?;
    Ok(())
}

/// FETCH_OK の応答範囲の終端は Largest Object が要求範囲内ならそこへ切り詰められること
///
/// draft-ietf-moq-transport-23 §3.2 (Fetch): "If Largest Object is within the requested range,
/// the FETCH response ends at Largest Object and the publisher MUST include the LARGEST_OBJECT
/// parameter in FETCH_OK. Otherwise, the response ends at the requested End Location."
/// 任意の要求範囲と Largest Object について、切り詰めの判定がこの規則と一致し、
/// 範囲内のときだけ LARGEST_OBJECT が応答に載ることを検証する。
#[test]
fn fetch_ok_end_follows_largest_object_within_range() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let start_group = noprop::sample_u64_in(ctx, 0..1000);
        let start_object = noprop::sample_u64_in(ctx, 0..100);
        let start = Location {
            group_id: start_group,
            object_id: start_object,
        };
        // 空 range は send_fetch が拒否するため、End は Start 以上になるように作る
        let end_group_delta = noprop::sample_u64_in(ctx, 0..10);
        let end = Location {
            group_id: start_group + end_group_delta,
            object_id: if end_group_delta == 0 {
                start_object + noprop::sample_u64_in(ctx, 0..100)
            } else {
                noprop::sample_u64_in(ctx, 0..100)
            },
        };
        let largest = Location {
            group_id: start_group + noprop::sample_u64_in(ctx, 0..12),
            object_id: noprop::sample_u64_in(ctx, 0..100),
        };
        let (mut client, mut server) = establish_pair();
        let rid = client
            .send_fetch(
                TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                b"cam".to_vec(),
                fetch_range_params(start, end),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, m) = take_send_request(&mut client);
        server
            .recv_request(m)
            .expect("テストフィクスチャの前提条件を満たす");
        let largest_in_range = largest >= start && largest <= end;
        server
            .send_fetch_ok(rid, fetch_ok_params(Some(largest)), TrackProperties::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok) = take_send_on_stream(&mut server);
        let ControlMessage::RequestOk(request_ok) = &ok else {
            panic!("REQUEST_OK が期待されたが {ok:?} だった");
        };
        assert_eq!(
            request_ok.parameters.largest_object(),
            largest_in_range.then_some((largest.group_id, largest.object_id)),
            "要求範囲内のときだけ LARGEST_OBJECT が応答に載ること"
        );
        client
            .recv_stream_message(rid, ok)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            client
                .fetch(rid)
                .expect("テストフィクスチャの前提条件を満たす")
                .end_location,
            Some(if largest_in_range { largest } else { end }),
            "応答範囲の終端が Largest Object の範囲判定に一致すること"
        );
        Ok(())
    })?;
    Ok(())
}
