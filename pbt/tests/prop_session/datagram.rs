//! datagram の OBJECT_DELIVERY_TIMEOUT 判定と END_OF_GROUP のプロパティテスト
//!
//! draft-ietf-moq-transport-22 §5.2 (Delivery Timeouts and Data Reliability):
//! "For datagrams, the implementation MUST drop the datagrams if the time elapsed
//! exceeds OBJECT_DELIVERY_TIMEOUT." 起点は object header の最終バイト。
//! object header 提供完了時刻は送信時に tick 済みなら直近の tick 時刻、tick 未到達なら最初の tick 時刻で
//! 確定する (sans-I/O の制約による近似)。経過時間が timeout 以上になったら drop し、
//! drop 後はエントリが保持されるため以後も drop され続ける。
//!
//! draft-ietf-moq-transport-22 §11.2.1 (Object Datagram): END_OF_GROUP bit は
//! "no Object with the same Group ID and an Object ID greater than the Object ID in this datagram
//! exists" を宣言し、受信側は宣言位置の 1 つ先を「存在しない最小の Object ID」として記録する。

use pbt::common::test_runner;
use shiguredo_moqt::message::common::TrackNamespace;
use shiguredo_moqt::message_parameter::MessageParameters;
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::{SessionState, TrackDataAcceptance};
use shiguredo_moqt::stream::datagram::ObjectDatagram;
use shiguredo_moqt::track_properties::{
    PROP_OBJECT_DELIVERY_TIMEOUT, TrackProperties, TrackProperty, TrackPropertyValue,
};

use super::common::{establish_pair, take_send_on_stream, take_send_request};

/// Track Property を指定して publisher subscription を確立する (Track Alias は 1)
fn establish_with_properties(track_properties: TrackProperties) -> (Session, Session, u64) {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(
            TrackNamespace::new(vec![b"live".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            b"cam1".to_vec(),
            MessageParameters::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), track_properties)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server, rid)
}

/// Track Property に OBJECT_DELIVERY_TIMEOUT=timeout_ms を持つ publisher subscription を確立する
fn establish_with_timeout(timeout_ms: u64) -> (Session, Session, u64) {
    let mut track_properties = TrackProperties::new();
    track_properties.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(timeout_ms),
    });
    establish_with_properties(track_properties)
}

/// tick 後の初回送信以降、経過時間 >= timeout なら drop、未満なら送信成功
///
/// object header 提供完了時刻は初回送信時点の直近 tick 時刻で確定する。その後の再送は
/// 確定時刻からの経過時間で判定され、timeout 以上なら drop される。
/// drop 後はエントリが保持されるため、以後の再送も drop され続ける。
#[test]
fn datagram_delivery_timeout_judgment() -> noprop::TestResult {
    // drop と送信成功の両方の分岐の観測をカバレッジゲートで検証する
    let dropped_seen = std::cell::Cell::new(false);
    let sent_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let timeout = noprop::sample_u64_in(ctx, 1..10_000);
        let first_tick = noprop::sample_u64_in(ctx, 0..100_000);
        let resend_delay = noprop::sample_u64_in(ctx, 0..20_000);
        let (_client, mut server, rid) = establish_with_timeout(timeout);
        server.tick(first_tick);
        server
            .send_object_datagram(rid, 0, 0, None, None, false)
            .expect("tick 後の初回送信は必ず成功する");
        server.tick(first_tick + resend_delay);
        let res = server.send_object_datagram(rid, 0, 0, None, None, false);
        if resend_delay >= timeout {
            assert!(
                res.is_err(),
                "経過 {resend_delay}ms >= timeout {timeout}ms なら drop される"
            );
            dropped_seen.set(true);
            // drop 後もエントリが保持されるため、以後の再送も drop され続ける
            let res = server.send_object_datagram(rid, 0, 0, None, None, false);
            assert!(res.is_err(), "drop 後の再送も drop され続けること");
        } else {
            assert!(
                res.is_ok(),
                "経過 {resend_delay}ms < timeout {timeout}ms なら送信される"
            );
            sent_seen.set(true);
        }
        assert_eq!(server.state(), SessionState::Established);
        Ok(())
    })?;
    assert!(
        dropped_seen.get(),
        "timeout により drop されるケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        sent_seen.get(),
        "timeout 前に送信成功するケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// END_OF_GROUP bit 付き datagram は送信でき、受信側で Group 終端が記録される
///
/// draft-ietf-moq-transport-22 §11.2.1 (Object Datagram): END_OF_GROUP bit は
/// 「同じ Group ID で、この Object ID より大きい Object ID の Object は存在しない」ことを宣言する。
/// 受信側は宣言位置の 1 つ先を「存在しない最小の Object ID」として記録する
/// (`saturating_add` のため Object ID が `u64::MAX` のときは 1 つ先へ進められない)。
/// bit が立っていない datagram は Group 終端を記録しない。
#[test]
fn datagram_end_of_group_records_group_end() -> noprop::TestResult {
    // END_OF_GROUP あり / なしの両方の分岐の観測をカバレッジゲートで検証する
    let end_of_group_seen = std::cell::Cell::new(false);
    let no_end_of_group_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let object_id = noprop::sample_u64(ctx);
        let end_of_group = noprop::sample_bool(ctx);
        let (mut client, mut server, rid) = establish_with_properties(TrackProperties::new());
        server
            .send_object_datagram(rid, 0, object_id, None, None, end_of_group)
            .expect("publisher 側の datagram 送信は受理される");
        // wire を経由せず decode 済みの datagram を直接渡す (Session API の境界を検証する)
        let outcome = client
            .recv_object_datagram(&ObjectDatagram {
                track_alias: 1,
                group_id: 0,
                object_id,
                publisher_priority: Some(1),
                properties_data: None,
                end_of_group,
                status: None,
            })
            .expect("subscriber 側の datagram 受信は受理される");
        assert_eq!(outcome, TrackDataAcceptance::Accepted);
        let subscription = client.subscription(rid).expect("subscription が存在する");
        if end_of_group {
            assert_eq!(
                subscription.ended_groups.get(&0),
                Some(&object_id.saturating_add(1)),
                "END_OF_GROUP は宣言位置の 1 つ先を存在しない最小の Object ID として記録する"
            );
            end_of_group_seen.set(true);
        } else {
            assert_eq!(
                subscription.ended_groups.get(&0),
                None,
                "END_OF_GROUP bit が立っていなければ Group 終端を記録しない"
            );
            no_end_of_group_seen.set(true);
        }
        assert_eq!(client.state(), SessionState::Established);
        assert_eq!(server.state(), SessionState::Established);
        Ok(())
    })?;
    assert!(
        end_of_group_seen.get(),
        "END_OF_GROUP ありのケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        no_end_of_group_seen.get(),
        "END_OF_GROUP なしのケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}
