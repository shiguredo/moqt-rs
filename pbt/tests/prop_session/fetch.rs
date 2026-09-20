//! FETCH 状態管理のプロパティテスト
//!
//! - 複数 FETCH の確立
//! - publisher 側の largest_location 非保存 (draft-20 で MUST 削除)
//! - FETCH の INVALID_RANGE 判定
//! - REQUEST_ERROR 後の Terminated と forget_fetch
//!
//! (draft-ietf-moq-transport-21 で Joining FETCH は廃止された)

use pbt::common::test_runner;
use shiguredo_moqt::message::{
    ControlMessage,
    common::{Location, TrackNamespace},
};
use shiguredo_moqt::message_parameter::LocationFilter;
use shiguredo_moqt::message_parameter::{
    MessageParameter, MessageParameterValue, MessageParameters, PARAM_LOCATION_FILTER,
};
use shiguredo_moqt::session::types::FetchState;
use shiguredo_moqt::track_properties::TrackProperties;

use super::common::{establish_pair, take_send_on_stream, take_send_request};

/// FETCH の range 指定 (AbsoluteRangeWithEnd) を持つ MessageParameters を作る
///
/// draft-ietf-moq-transport-21 §9.11 (FETCH): range は LOCATION_FILTER で指定する。
/// `end.group_id < start.group_id` の場合は delta が求まらないため panic する
/// (テストフィクスチャの前提)。
fn fetch_range_params(start: Location, end: Location) -> MessageParameters {
    let end_group_delta = end.group_id - start.group_id;
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteRangeWithEnd {
                start,
                end_group_delta,
                end_object: end.object_id,
            }
            .encode_to_bytes(),
        ),
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
                    0,
                    Location {
                        group_id: u64::from(i) + 1,
                        object_id: 0,
                    },
                    MessageParameters::new(),
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

/// draft-ietf-moq-transport-21 で publisher 側の Largest Location 保持 MUST 要件は
/// 削除された。publisher 側の `largest_location` は REQUEST_UPDATE_OK の
/// LARGEST_OBJECT でも保存されない (自側の観測値は `largest_received_location` で追跡する)。
/// 確立経路 (SUBSCRIBE_OK / PUBLISH) の非保存は単体テストで担保する。
///
/// (初期 forward, REQUEST_UPDATE での forward) の組を生成し、0→1 遷移時も含めて
/// 保存されないことを検証する。
#[test]
fn publisher_largest_location_never_saved_on_request_update_ok() -> noprop::TestResult {
    // 0→1 遷移時とそれ以外の両方の分岐の観測をカバレッジゲートで検証する
    let transition_seen = std::cell::Cell::new(false);
    let other_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        use shiguredo_moqt::message_parameter::{
            MessageParameter, MessageParameterValue, PARAM_FORWARD, PARAM_LARGEST_OBJECT,
        };
        let initial_forward = noprop::sample_choice(ctx, &[0u8, 1u8]);
        let update_forward = noprop::sample_choice(ctx, &[0u8, 1u8]);
        let g = noprop::sample_u64_in(ctx, 0..1000);
        let o = noprop::sample_u64_in(ctx, 0..1000);
        let (mut client, mut server) = establish_pair();
        // initial_forward で SUBSCRIBE → publisher (server) が forward=initial_forward で確立する
        let mut sub_params = MessageParameters::new();
        sub_params.push(MessageParameter {
            param_type: PARAM_FORWARD,
            value: MessageParameterValue::Uint8(initial_forward),
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
        // REQUEST_UPDATE で FORWARD=update_forward を送る
        let mut upd_params = MessageParameters::new();
        upd_params.push(MessageParameter {
            param_type: PARAM_FORWARD,
            value: MessageParameterValue::Uint8(update_forward),
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
        // publisher 側は 0→1 遷移時も含めて largest_location を保存しない
        if initial_forward == 0 && update_forward == 1 {
            transition_seen.set(true);
        } else {
            other_seen.set(true);
        }
        assert_eq!(
            server
                .subscription(rid)
                .expect("テストフィクスチャの前提条件を満たす")
                .forward_state,
            update_forward
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
        "Forward State 0→1 のケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        other_seen.get(),
        "Forward State が 0→1 以外のケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// draft-ietf-moq-transport-21 §9.11 (FETCH): FETCH は Start Location が対象トラックの観測
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
                    redirect: None,
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
