//! SUBSCRIBE / PUBLISH 状態管理のプロパティテスト
//!
//! - 複数 SUBSCRIBE / PUBLISH ハンドシェイクの確立
//! - Pending ヘルパーの initiator 判定
//! - PUBLISH_DONE の対象限定性
//! - stream_count 受理判定の不変条件
//! - forget_subscription による全索引削除

use pbt::common::test_runner;
use shiguredo_moqt::message::{ControlMessage, common::TrackNamespace};
use shiguredo_moqt::message_parameter::MessageParameters;
use shiguredo_moqt::session::{
    core::PUBLISH_DONE_STREAM_COUNT_UNKNOWN,
    types::{SessionEvent, SubscriptionState},
};
use shiguredo_moqt::track_properties::TrackProperties;
use shiguredo_moqt::{
    session::types::DataStreamId, session::types::RequestStreamEnd,
    stream::subgroup::SubgroupHeader, stream::subgroup::SubgroupIdMode,
};

use super::common::{establish_pair, take_send_on_stream, take_send_request};

// N 個の独立した Track に対する SUBSCRIBE ハンドシェイクが全て Established に至る
#[test]
fn multiple_subscribe_handshakes_all_establish() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let count = noprop::sample_usize_in(ctx, 1..16);
        let (mut client, mut server) = establish_pair();
        let mut rids = Vec::new();
        for i in 0..count {
            let ns = TrackNamespace::new(vec![format!("ns-{i}").into_bytes()])
                .expect("テストフィクスチャの前提条件を満たす");
            let name = format!("track-{i}").into_bytes();
            let rid = client
                .send_subscribe(ns, name, MessageParameters::new())
                .expect("テストフィクスチャの前提条件を満たす");
            rids.push(rid);
            let (ev_rid, sub_msg) = take_send_request(&mut client);
            assert_eq!(ev_rid, rid);
            server
                .recv_request(sub_msg)
                .expect("テストフィクスチャの前提条件を満たす");
            // Server が採番する track alias は Track ごとに一意にする
            server
                .send_subscribe_ok(
                    rid,
                    i as u64 + 1,
                    MessageParameters::new(),
                    TrackProperties::new(),
                )
                .expect("テストフィクスチャの前提条件を満たす");
            let (_, ok_msg) = take_send_on_stream(&mut server);
            client
                .recv_stream_message(rid, ok_msg)
                .expect("テストフィクスチャの前提条件を満たす");
        }
        for rid in rids {
            assert_eq!(
                client
                    .subscription(rid)
                    .expect("テストフィクスチャの前提条件を満たす")
                    .state,
                SubscriptionState::Established
            );
            assert_eq!(
                server
                    .subscription(rid)
                    .expect("テストフィクスチャの前提条件を満たす")
                    .state,
                SubscriptionState::Established
            );
        }
        Ok(())
    })?;
    Ok(())
}

// N 個の Track に対する PUBLISH ハンドシェイク (Client 発 Publisher) も全て Established
#[test]
fn multiple_publish_handshakes_all_establish() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let count = noprop::sample_usize_in(ctx, 1..16);
        let (mut client, mut server) = establish_pair();
        let mut rids = Vec::new();
        for i in 0..count {
            let ns = TrackNamespace::new(vec![format!("pub-{i}").into_bytes()])
                .expect("テストフィクスチャの前提条件を満たす");
            let rid = client
                .send_publish(
                    ns,
                    format!("track-{i}").into_bytes(),
                    i as u64 + 100,
                    MessageParameters::new(),
                    TrackProperties::new(),
                )
                .expect("テストフィクスチャの前提条件を満たす");
            rids.push(rid);
            let (_, pub_msg) = take_send_request(&mut client);
            server
                .recv_request(pub_msg)
                .expect("テストフィクスチャの前提条件を満たす");
            server
                .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
                .expect("テストフィクスチャの前提条件を満たす");
            let (_, ok_msg) = take_send_on_stream(&mut server);
            client
                .recv_stream_message(rid, ok_msg)
                .expect("テストフィクスチャの前提条件を満たす");
        }
        for rid in rids {
            assert_eq!(
                client
                    .subscription(rid)
                    .expect("テストフィクスチャの前提条件を満たす")
                    .state,
                SubscriptionState::Established
            );
            assert_eq!(
                server
                    .subscription(rid)
                    .expect("テストフィクスチャの前提条件を満たす")
                    .state,
                SubscriptionState::Established
            );
        }
        Ok(())
    })?;
    Ok(())
}

// SubscriptionState::Pending + initiator の組み合わせで
// is_pending_subscriber / is_pending_publisher が正しく分岐する
#[test]
fn pending_helper_identifies_initiator_for_send_subscribe() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let count = noprop::sample_usize_in(ctx, 1..16);
        let (mut client, _server) = establish_pair();
        let mut rids = Vec::new();
        for i in 0..count {
            let ns = TrackNamespace::new(vec![format!("s{i}").into_bytes()])
                .expect("テストフィクスチャの前提条件を満たす");
            let rid = client
                .send_subscribe(ns, format!("t{i}").into_bytes(), MessageParameters::new())
                .expect("テストフィクスチャの前提条件を満たす");
            rids.push(rid);
        }
        for rid in rids {
            let sub = client
                .subscription(rid)
                .expect("テストフィクスチャの前提条件を満たす");
            assert_eq!(sub.state, SubscriptionState::Pending);
            assert!(sub.is_pending_subscriber());
            assert!(!sub.is_pending_publisher());
        }
        Ok(())
    })?;
    Ok(())
}

#[test]
fn pending_helper_identifies_initiator_for_send_publish() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let count = noprop::sample_usize_in(ctx, 1..16);
        let (mut client, _server) = establish_pair();
        let mut rids = Vec::new();
        for i in 0..count {
            let ns = TrackNamespace::new(vec![format!("p{i}").into_bytes()])
                .expect("テストフィクスチャの前提条件を満たす");
            let rid = client
                .send_publish(
                    ns,
                    format!("t{i}").into_bytes(),
                    i as u64 + 100,
                    MessageParameters::new(),
                    TrackProperties::new(),
                )
                .expect("テストフィクスチャの前提条件を満たす");
            rids.push(rid);
        }
        for rid in rids {
            let sub = client
                .subscription(rid)
                .expect("テストフィクスチャの前提条件を満たす");
            assert_eq!(sub.state, SubscriptionState::Pending);
            assert!(sub.is_pending_publisher());
            assert!(!sub.is_pending_subscriber());
        }
        Ok(())
    })?;
    Ok(())
}

// Established / Terminated では pending helper は両方 false
#[test]
fn pending_helpers_false_outside_pending() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let count = noprop::sample_usize_in(ctx, 1..8);
        let (mut client, mut server) = establish_pair();
        let mut rids = Vec::new();
        for i in 0..count {
            let ns = TrackNamespace::new(vec![format!("e{i}").into_bytes()])
                .expect("テストフィクスチャの前提条件を満たす");
            let rid = client
                .send_subscribe(ns, format!("t{i}").into_bytes(), MessageParameters::new())
                .expect("テストフィクスチャの前提条件を満たす");
            rids.push(rid);
            let (_, m) = take_send_request(&mut client);
            server
                .recv_request(m)
                .expect("テストフィクスチャの前提条件を満たす");
            server
                .send_subscribe_ok(
                    rid,
                    i as u64 + 1,
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
            let sub = client
                .subscription(rid)
                .expect("テストフィクスチャの前提条件を満たす");
            assert_eq!(sub.state, SubscriptionState::Established);
            assert!(!sub.is_pending_subscriber());
            assert!(!sub.is_pending_publisher());
        }
        Ok(())
    })?;
    Ok(())
}

// 複数の subscribe の一部に PUBLISH_DONE を適用しても残りは Established のまま
#[test]
fn publish_done_only_terminates_targeted_subscriptions() -> noprop::TestResult {
    // 対象に選ばれるケースと選ばれないケースの両方の観測をカバレッジゲートで検証する
    let terminated_seen = std::cell::Cell::new(false);
    let established_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let total = noprop::sample_usize_in(ctx, 2..10);
        // done_index は total 未満から valid-by-construction で選ぶ
        let done_index = noprop::sample_usize_in(ctx, 0..total);
        let (mut client, mut server) = establish_pair();
        let mut rids = Vec::new();
        for i in 0..total {
            let ns = TrackNamespace::new(vec![format!("n{i}").into_bytes()])
                .expect("テストフィクスチャの前提条件を満たす");
            let rid = client
                .send_subscribe(ns, format!("t{i}").into_bytes(), MessageParameters::new())
                .expect("テストフィクスチャの前提条件を満たす");
            rids.push(rid);
            let (_, m) = take_send_request(&mut client);
            server
                .recv_request(m)
                .expect("テストフィクスチャの前提条件を満たす");
            server
                .send_subscribe_ok(
                    rid,
                    i as u64 + 1,
                    MessageParameters::new(),
                    TrackProperties::new(),
                )
                .expect("テストフィクスチャの前提条件を満たす");
            let (_, ok) = take_send_on_stream(&mut server);
            client
                .recv_stream_message(rid, ok)
                .expect("テストフィクスチャの前提条件を満たす");
        }
        // done_index の subscription のみ PUBLISH_DONE
        let target = rids[done_index];
        server
            .send_publish_done(
                target,
                0x2,
                0,
                shiguredo_moqt::message::ReasonPhrase::new(String::new())
                    .expect("テストフィクスチャの前提条件を満たす"),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, done) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(target, done)
            .expect("テストフィクスチャの前提条件を満たす");
        for (i, rid) in rids.iter().enumerate() {
            let expected = if i == done_index {
                terminated_seen.set(true);
                SubscriptionState::Terminated
            } else {
                established_seen.set(true);
                SubscriptionState::Established
            };
            assert_eq!(
                client
                    .subscription(*rid)
                    .expect("テストフィクスチャの前提条件を満たす")
                    .state,
                expected
            );
        }
        Ok(())
    })?;
    assert!(
        terminated_seen.get(),
        "PUBLISH_DONE で終端される subscription のケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        established_seen.get(),
        "PUBLISH_DONE の対象外が Established のまま残るケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

// draft-ietf-moq-transport-22 §9.9 (PUBLISH_DONE):
// `send_publish_done` の `stream_count` 受理判定。
//
// 検証する不変条件 (draft-ietf-moq-transport-22 §9.9 (PUBLISH_DONE)):
//   `published_stream_count == 0` のときは `stream_count == 0` のときだけ `Ok`。
//   `published_stream_count > 0` のときは `stream_count == published_stream_count`
//   または `stream_count == PUBLISH_DONE_STREAM_COUNT_UNKNOWN` のとき `Ok`。
//   それ以外は `Err(SESSION_PROTOCOL_VIOLATION)` を返す。
//   (draft-ietf-moq-transport-22 §9.9 の「stream を開かなかった場合は Stream Count を 0 にする」を送信側で強制する)
#[test]
fn send_publish_done_stream_count_invariant() -> noprop::TestResult {
    // 受理 (Ok) と拒否 (PROTOCOL_VIOLATION) の両方の分岐の観測を
    // カバレッジゲートで検証する
    let accepted_seen = std::cell::Cell::new(false);
    let rejected_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let opened_streams = noprop::sample_u64_in(ctx, 0..6);
        // 4 ブランチを同確率に振り分ける
        let branch = noprop::sample_usize_in(ctx, 0..4);
        let (_, mut server) = establish_pair();
        // server を publisher にする SUBSCRIBE ハンドシェイク (peer 側送信を模擬)
        server
            .recv_request(ControlMessage::Subscribe(
                shiguredo_moqt::message::Subscribe {
                    request_id: 0,
                    track_namespace: TrackNamespace::new(vec![b"ns".to_vec()])
                        .expect("テストフィクスチャの前提条件を満たす"),
                    track_name: b"t".to_vec(),
                    parameters: MessageParameters::new(),
                },
            ))
            .expect("テストフィクスチャの前提条件を満たす");
        server
            .send_subscribe_ok(0, 1, MessageParameters::new(), TrackProperties::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let _ = take_send_on_stream(&mut server);

        // opened_streams 本の subgroup stream を open → close する
        for i in 0..opened_streams {
            let stream_id = DataStreamId(i + 1);
            let header = SubgroupHeader {
                track_alias: 1,
                group_id: 1,
                subgroup_id: SubgroupIdMode::Explicit(i),
                publisher_priority: Some(1),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            };
            server
                .send_subgroup_header(stream_id, 0, &header)
                .expect("テストフィクスチャの前提条件を満たす");
            server
                .send_data_stream_closed(stream_id, RequestStreamEnd::Fin)
                .expect("テストフィクスチャの前提条件を満たす");
        }
        let published = server
            .subscription(0)
            .expect("テストフィクスチャの前提条件を満たす")
            .stream_counts
            .published_count;
        assert_eq!(published, opened_streams);

        // 4 ブランチで stream_count を生成
        let stream_count = match branch {
            0 => 0u64,
            1 => PUBLISH_DONE_STREAM_COUNT_UNKNOWN,
            2 => published,
            // vi64 値域 (`0..2^62`) を網羅できるようにする。`0` と `published` に重なる
            // ケースは rejection で除外する (出現確率はほぼ 0)。
            _ => noprop::sample_with_rejection(ctx, 100, |ctx| {
                let v = noprop::sample_u64_in(ctx, 0..(1u64 << 62));
                if v != 0 && v != published {
                    Some(v)
                } else {
                    None
                }
            }),
        };

        let result = server.send_publish_done(
            0,
            0,
            stream_count,
            shiguredo_moqt::message::ReasonPhrase::new(String::new())
                .expect("テストフィクスチャの前提条件を満たす"),
        );

        let should_accept = if published == 0 {
            stream_count == 0
        } else {
            stream_count == PUBLISH_DONE_STREAM_COUNT_UNKNOWN || stream_count == published
        };
        if should_accept {
            assert!(
                result.is_ok(),
                "({published}, {stream_count}) では Ok を期待する"
            );
            accepted_seen.set(true);
            assert_eq!(
                server
                    .subscription(0)
                    .expect("テストフィクスチャの前提条件を満たす")
                    .state,
                SubscriptionState::Terminated
            );
        } else {
            let err = result.unwrap_err();
            rejected_seen.set(true);
            assert_eq!(err.code, shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION);
            assert_eq!(
                server
                    .subscription(0)
                    .expect("テストフィクスチャの前提条件を満たす")
                    .state,
                SubscriptionState::Established
            );
        }
        Ok(())
    })?;
    assert!(
        accepted_seen.get(),
        "stream_count が受理されるケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        rejected_seen.get(),
        "stream_count が PROTOCOL_VIOLATION で拒否されるケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

// forget_subscription 後、track alias / subscription / subscriptions_by_track から消える
#[test]
fn forget_subscription_removes_all_indices() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let alias = noprop::sample_u64_in(ctx, 1..1024);
        let (_, mut server) = establish_pair();
        server
            .recv_request(ControlMessage::Subscribe(
                shiguredo_moqt::message::Subscribe {
                    request_id: 0,

                    track_namespace: TrackNamespace::new(vec![b"a".to_vec()])
                        .expect("テストフィクスチャの前提条件を満たす"),
                    track_name: b"t".to_vec(),
                    parameters: MessageParameters::new(),
                },
            ))
            .expect("テストフィクスチャの前提条件を満たす");
        server
            .send_subscribe_ok(0, alias, MessageParameters::new(), TrackProperties::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let _ = take_send_on_stream(&mut server);
        server
            .send_publish_done(
                0,
                0,
                0,
                shiguredo_moqt::message::ReasonPhrase::new(String::new())
                    .expect("テストフィクスチャの前提条件を満たす"),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let _ = take_send_on_stream(&mut server);
        server
            .forget_subscription(0)
            .expect("テストフィクスチャの前提条件を満たす");
        assert!(server.subscription(0).is_none());
        // 同じ alias を再利用できる = my_publisher_aliases から消えている
        let rid2 = server
            .send_publish(
                TrackNamespace::new(vec![b"b".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                b"t2".to_vec(),
                alias,
                MessageParameters::new(),
                TrackProperties::new(),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            server
                .subscription(rid2)
                .expect("テストフィクスチャの前提条件を満たす")
                .track_alias,
            Some(alias)
        );
        Ok(())
    })?;
    Ok(())
}

/// REQUEST_UPDATE は受信順のグループ単位で処理され、応答も受信順に返る
///
/// draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE): "REQUEST_UPDATE messages MUST be
/// processed in order" / "The receiver MUST still send a REQUEST_UPDATE_OK for each
/// successful update" / "REQUEST_UPDATE_OKs MUST be returned in the order the
/// REQUEST_UPDATEs were received". 通常の更新は合体 (coalesce) して累積結果のみを適用
/// できるが、FILL_PARAMETERS を含む更新は合体できない (§9.19.15)。そのため受信順の
/// グループ単位で状態が適用され、応答の個数は受信数と一致する。
///
/// 本性質は任意の更新列 (PAUSED / LOCATION_FILTER / FILL の組合せ) について次を検証する。
///
/// - FILL の前後がグループ境界になり、グループ数が境界規則と一致する
/// - 応答 k 回目の直後の paused / filter が、先頭から k グループ目までを適用した値になり、
///   未応答の後続グループの値が先に適用されない
/// - 応答の個数が受信数と一致し、最後にキューが空になる
#[test]
fn request_update_groups_apply_in_receive_order() -> noprop::TestResult {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::message_parameter::{
        LocationFilter, MessageParameter, MessageParameterValue, PARAM_FILL_PARAMETERS,
        PARAM_LOCATION_FILTER, PARAM_PAUSED,
    };

    /// サンプルした更新 1 通分
    struct SampledUpdate {
        has_fill: bool,
        paused: Option<u8>,
        filter: Option<LocationFilter>,
    }

    /// 更新列から受信順の coalescing グループを求める
    ///
    /// FILL_PARAMETERS を含む更新は単独グループになり、その直後の通常更新も
    /// 新しいグループを開始する。それ以外は直前のグループへ合体する。
    fn group_indices(updates: &[SampledUpdate]) -> Vec<Vec<usize>> {
        let mut groups: Vec<Vec<usize>> = Vec::new();
        let mut tail_is_fill = false;
        for (index, update) in updates.iter().enumerate() {
            if update.has_fill || groups.is_empty() || tail_is_fill {
                groups.push(vec![index]);
                tail_is_fill = update.has_fill;
            } else {
                groups
                    .last_mut()
                    .expect("グループは 1 件以上ある")
                    .push(index);
            }
        }
        groups
    }

    /// 先頭から `answered` グループまでを適用した (paused, filter) を返す
    ///
    /// PAUSED は省略時に現在値を維持し、LOCATION_FILTER は後の値が前を上書きする
    /// (draft-ietf-moq-transport-23 §9.19.18 / §9.19.9)。
    fn expected_state_after(
        updates: &[SampledUpdate],
        groups: &[Vec<usize>],
        answered: usize,
    ) -> (bool, Option<LocationFilter>) {
        let mut paused = false;
        let mut filter = None;
        for group in &groups[..answered] {
            for &index in group {
                if let Some(value) = updates[index].paused {
                    paused = value == 1;
                }
                if let Some(value) = updates[index].filter {
                    filter = Some(value);
                }
            }
        }
        (paused, filter)
    }

    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let count = noprop::sample_usize_in(ctx, 1..=5);
        let mut updates = Vec::new();
        for _ in 0..count {
            let has_fill = noprop::sample_bool(ctx);
            let paused = if noprop::sample_bool(ctx) {
                Some(noprop::sample_u8(ctx) % 2)
            } else {
                None
            };
            let filter = if noprop::sample_bool(ctx) {
                Some(LocationFilter::AbsoluteRangeWithEnd {
                    start: Location {
                        group_id: 0,
                        object_id: 0,
                    },
                    end_group_delta: noprop::sample_u64_in(ctx, 0..=4),
                    end_object: noprop::sample_u64_in(ctx, 0..=4),
                })
            } else {
                None
            };
            updates.push(SampledUpdate {
                has_fill,
                paused,
                filter,
            });
        }
        let groups = group_indices(&updates);

        // SUBSCRIBE を確立する。Largest Object を公開しないため fill fetch stream は開かない
        // (fill の開設条件だけを本性質の対象外にする)。
        let (mut client, mut server) = establish_pair();
        let rid = client
            .send_subscribe(
                TrackNamespace::new(vec![b"prop".to_vec()]).expect("正当な namespace である"),
                b"update".to_vec(),
                MessageParameters::new(),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, subscribe) = take_send_request(&mut client);
        server
            .recv_request(subscribe)
            .expect("テストフィクスチャの前提条件を満たす");
        server
            .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(rid, ok)
            .expect("テストフィクスチャの前提条件を満たす");

        // 更新を応答を待たずに送受信する
        for update in &updates {
            let mut parameters = MessageParameters::new();
            if update.has_fill {
                parameters.push(MessageParameter {
                    param_type: PARAM_FILL_PARAMETERS,
                    value: MessageParameterValue::FillParameters(MessageParameters::new()),
                });
            }
            if let Some(paused) = update.paused {
                parameters.push(MessageParameter {
                    param_type: PARAM_PAUSED,
                    value: MessageParameterValue::Uint8(paused),
                });
            }
            if let Some(filter) = update.filter {
                parameters.push(MessageParameter {
                    param_type: PARAM_LOCATION_FILTER,
                    value: MessageParameterValue::LocationFilter(filter),
                });
            }
            client
                .send_request_update(rid, parameters)
                .expect("テストフィクスチャの前提条件を満たす");
            let (stream_rid, message) = take_send_on_stream(&mut client);
            assert_eq!(stream_rid, rid, "購読の bidi stream に送ること");
            server
                .recv_stream_message(rid, message)
                .expect("テストフィクスチャの前提条件を満たす");
        }

        // 受信イベントは受信数と一致し、fill fetch stream は開かない
        let mut received = 0_usize;
        while let Some(event) = server.poll_event() {
            match event {
                SessionEvent::RequestUpdateReceived { .. } => received += 1,
                SessionEvent::OpenFillFetchStream { .. } => {
                    panic!("Largest Object 未知では fill fetch stream を開かない")
                }
                SessionEvent::CloseSession(err) => {
                    panic!("REQUEST_UPDATE の受信でセッションが閉じてはいけない: {err:?}")
                }
                _ => {}
            }
        }
        assert_eq!(received, count, "REQUEST_UPDATE ごとに通知されること");

        // グループ構造が境界規則と一致する
        let subscription = server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            subscription.pending_update_params.len(),
            groups.len(),
            "FILL の前後を境界としたグループ数になること"
        );
        assert_eq!(
            subscription
                .pending_update_params
                .iter()
                .map(|group| group.has_fill_parameters)
                .collect::<Vec<_>>(),
            groups
                .iter()
                .map(|group| group.len() == 1 && updates[group[0]].has_fill)
                .collect::<Vec<_>>(),
            "fill を含むグループだけが境界として印されること"
        );

        // 応答ごとに先頭グループだけが適用される
        for answered in 1..=groups.len() {
            server
                .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
                .expect("REQUEST_UPDATE_OK の送信に成功すること");
            let (stream_rid, message) = take_send_on_stream(&mut server);
            assert_eq!(stream_rid, rid);
            assert!(
                matches!(message, ControlMessage::RequestOk(_)),
                "REQUEST_UPDATE_OK で応答すること: {message:?}"
            );
            let subscription = server
                .subscription(rid)
                .expect("テストフィクスチャの前提条件を満たす");
            let (expected_paused, expected_filter) =
                expected_state_after(&updates, &groups, answered);
            assert_eq!(
                subscription.paused, expected_paused,
                "応答したグループまでの PAUSED が適用されること"
            );
            assert_eq!(
                subscription.filter, expected_filter,
                "応答したグループまでの LOCATION_FILTER が適用されること"
            );
            assert_eq!(
                subscription.pending_update_params.len(),
                groups.len() - answered,
                "応答したグループだけがキューから取り除かれること"
            );
        }
        assert_eq!(
            server
                .subscription(rid)
                .expect("テストフィクスチャの前提条件を満たす")
                .state,
            SubscriptionState::Established,
            "応答で購読が終端しないこと"
        );
        Ok(())
    })?;
    Ok(())
}
