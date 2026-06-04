//! datagram の OBJECT_DELIVERY_TIMEOUT 判定のプロパティテスト
//!
//! draft-ietf-moq-transport-21 §5.2 (Delivery Timeouts and Data Reliability):
//! "For datagrams, the implementation MUST drop the datagrams if the time elapsed
//! exceeds OBJECT_DELIVERY_TIMEOUT." 起点は object header の最終バイト。
//! object header 提供完了時刻は送信時に tick 済みなら直近の tick 時刻、tick 未到達なら最初の tick 時刻で
//! 確定する (sans-I/O の制約による近似)。経過時間が timeout 以上になったら drop し、
//! drop 後はエントリが保持されるため以後も drop され続ける。

use pbt::common::test_runner;
use shiguredo_moqt::message::common::TrackNamespace;
use shiguredo_moqt::message_parameter::MessageParameters;
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::SessionState;
use shiguredo_moqt::track_properties::{
    PROP_OBJECT_DELIVERY_TIMEOUT, TrackProperties, TrackProperty, TrackPropertyValue,
};

use super::common::{establish_pair, take_send_on_stream, take_send_request};

/// Track Property に OBJECT_DELIVERY_TIMEOUT=timeout_ms を持つ publisher subscription を確立する
fn establish_with_timeout(timeout_ms: u64) -> (Session, Session, u64) {
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
    let mut track_properties = TrackProperties::new();
    track_properties.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(timeout_ms),
    });
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), track_properties)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server, rid)
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
            .send_object_datagram(rid, 0, 0, None, None)
            .expect("tick 後の初回送信は必ず成功する");
        server.tick(first_tick + resend_delay);
        let res = server.send_object_datagram(rid, 0, 0, None, None);
        if resend_delay >= timeout {
            assert!(
                res.is_err(),
                "経過 {resend_delay}ms >= timeout {timeout}ms なら drop される"
            );
            dropped_seen.set(true);
            // drop 後もエントリが保持されるため、以後の再送も drop され続ける
            let res = server.send_object_datagram(rid, 0, 0, None, None);
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
