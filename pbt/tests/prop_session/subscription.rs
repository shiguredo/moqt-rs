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
use shiguredo_moqt::session::{core::PUBLISH_DONE_STREAM_COUNT_UNKNOWN, types::SubscriptionState};
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

// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE):
// `send_publish_done` の `stream_count` 受理判定。
//
// 検証する不変条件 (sentinel `PUBLISH_DONE_STREAM_COUNT_UNKNOWN` 受容後):
//   `stream_count == PUBLISH_DONE_STREAM_COUNT_UNKNOWN`
//   または `stream_count == published_stream_count`
//   のとき `Ok` を返し、それ以外は `Err(SESSION_PROTOCOL_VIOLATION)` を返す。
//   (published_stream_count == 0 のとき stream_count == 0 のみ許可。draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE) の
//    「stream を開かなかった場合は Stream Count を 0 にする」を送信側で強制する)
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
            // vi64 値域 (`0..2^62`) を網羅できるようにする。sentinel (2^62-1) と published に
            // 重なるケースは rejection で除外する (ともに出現確率はほぼ 0)。
            _ => noprop::sample_with_rejection(ctx, 100, |ctx| {
                let v = noprop::sample_u64_in(ctx, 0..(1u64 << 62));
                if v != 0 && v != PUBLISH_DONE_STREAM_COUNT_UNKNOWN && v != published {
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

        let should_accept =
            stream_count == PUBLISH_DONE_STREAM_COUNT_UNKNOWN || stream_count == published;
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
