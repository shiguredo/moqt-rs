//! OBJECT_DELIVERY_TIMEOUT / SUBGROUP_DELIVERY_TIMEOUT のテスト

use super::*;

/// OBJECT_DELIVERY_TIMEOUT > 0 の subscription で send_subgroup_object 呼び出し時に
/// 最初の object の object header 提供完了時刻が追跡されることを確認する
#[test]
fn object_delivery_timeout_tracks_first_object_header_complete_time() {
    let (mut client, mut server) = establish_pair();
    // 時間を進めておく
    client.tick(1_000);
    server.tick(1_000);

    // subscriber 側が OBJECT_DELIVERY_TIMEOUT=100 を指定して SUBSCRIBE
    let rid = client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam1".to_vec(),
            delivery_timeout_params(100),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // publisher 側も OBJECT_DELIVERY_TIMEOUT=200 を TrackProperty で返す
    server
        .send_subscribe_ok(
            rid,
            100,
            MessageParameters::new(),
            track_properties_with_delivery_timeout(200),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // effective は subscriber(100) と publisher(200) の最小値 = 100 になる
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .delivery_timeouts
            .effective_object_ms,
        Some(100)
    );

    // publisher 側で subgroup stream を開いて object を送信する
    let stream_id = DataStreamId(10);
    server
        .send_subgroup_header(
            stream_id,
            rid,
            &SubgroupHeader {
                track_alias: 100,
                group_id: 1,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 0, None)
        .expect("テストフィクスチャの前提条件を満たす");

    // tick すると delivery timeout が追跡され、経過時間が timeout を超えていなければ
    // ResetDataStream は発行されない
    server.tick(1_050); // 経過 50ms < timeout 100ms
    let mut got_reset = false;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::ResetDataStream { .. }) {
            got_reset = true;
        }
    }
    assert!(
        !got_reset,
        "timeout 未到達では ResetDataStream が発行されない"
    );

    // timeout を超える時間に進める
    server.tick(1_100); // 経過 100ms >= timeout 100ms
    let mut reset_stream_id = None;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::ResetDataStream {
            stream_id: sid,
            error_code,
            reliable_size,
        } = e
        {
            assert_eq!(error_code, STREAM_DELIVERY_TIMEOUT);
            assert_eq!(
                reliable_size, None,
                "自動発火では RESET_STREAM (reliable_size なし) になること"
            );
            reset_stream_id = Some(sid);
        }
    }
    assert_eq!(
        reset_stream_id,
        Some(stream_id),
        "OBJECT_DELIVERY_TIMEOUT 超過で ResetDataStream が発行される"
    );
}

/// OBJECT_DELIVERY_TIMEOUT = 0 の subscription では delivery timeout 追跡が行われないことを確認する
#[test]
fn object_delivery_timeout_zero_does_not_track() {
    let (mut client, mut server) = establish_pair();
    client.tick(1_000);
    server.tick(1_000);

    // subscriber が OBJECT_DELIVERY_TIMEOUT=0 を指定 (無効)
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), delivery_timeout_params(0))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 100, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // timeout=0 または None の場合は effective も None
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .delivery_timeouts
            .effective_object_ms,
        None
    );

    let stream_id = DataStreamId(10);
    server
        .send_subgroup_header(
            stream_id,
            rid,
            &SubgroupHeader {
                track_alias: 100,
                group_id: 1,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 0, None)
        .expect("テストフィクスチャの前提条件を満たす");

    // OBJECT_DELIVERY_TIMEOUT が無効なので timeout は発生しない
    server.tick(2_000);
    let mut got_reset = false;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::ResetDataStream { .. }) {
            got_reset = true;
        }
    }
    assert!(
        !got_reset,
        "OBJECT_DELIVERY_TIMEOUT 無効では ResetDataStream は発行されない"
    );
}

/// send_subgroup_object の 2 回目以降も最初の object の object header 提供完了時刻が基準であることを確認する
#[test]
fn object_delivery_timeout_uses_first_object_header_complete_time_as_baseline() {
    let (mut client, mut server) = establish_pair();
    // 最初の時刻を大きく設定
    client.tick(10_000);
    server.tick(10_000);

    let rid = client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam1".to_vec(),
            delivery_timeout_params(100),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 100, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let stream_id = DataStreamId(10);
    server
        .send_subgroup_header(
            stream_id,
            rid,
            &SubgroupHeader {
                track_alias: 100,
                group_id: 1,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    // 1 回目の object: 時刻 10_000 で記録される
    server
        .send_subgroup_object(stream_id, 0, None)
        .expect("テストフィクスチャの前提条件を満たす");

    // 時間を 50ms 進めてから 2 回目の object を送信
    server.tick(10_050);
    server
        .send_subgroup_object(stream_id, 1, None)
        .expect("テストフィクスチャの前提条件を満たす");

    // timeout 100ms: 最初の object 基準で 10_100 が期限
    // 時刻 10_090 ではまだ timeout しない
    server.tick(10_090);
    let mut got_reset = false;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::ResetDataStream { .. }) {
            got_reset = true;
        }
    }
    assert!(
        !got_reset,
        "timeout 未到達では ResetDataStream が発行されない"
    );

    // 時刻 10_101 で timeout 超過
    server.tick(10_101);
    let mut reset_stream_id = None;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::ResetDataStream {
            stream_id: sid,
            error_code,
            reliable_size,
        } = e
        {
            assert_eq!(error_code, STREAM_DELIVERY_TIMEOUT);
            assert_eq!(
                reliable_size, None,
                "自動発火では RESET_STREAM (reliable_size なし) になること"
            );
            reset_stream_id = Some(sid);
        }
    }
    assert_eq!(
        reset_stream_id,
        Some(stream_id),
        "2 回目の object 以降も最初の object の object header 提供完了時刻を基準に timeout が判定される"
    );
}

/// send_data_stream_closed(Fin) 後に OBJECT_DELIVERY_TIMEOUT 追跡が除去されることを確認する
#[test]
fn object_delivery_timeout_tracking_removed_on_stream_close() {
    let (mut client, mut server) = establish_pair();
    client.tick(1_000);
    server.tick(1_000);

    let rid = client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam1".to_vec(),
            delivery_timeout_params(100),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 100, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let stream_id = DataStreamId(10);
    server
        .send_subgroup_header(
            stream_id,
            rid,
            &SubgroupHeader {
                track_alias: 100,
                group_id: 1,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 0, None)
        .expect("テストフィクスチャの前提条件を満たす");

    // stream を FIN で閉じる
    server
        .send_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");

    // stream が閉じられたので timeout 追跡が除去されており、
    // timeout を超えても ResetDataStream は発行されない
    server.tick(2_000);
    let mut got_reset = false;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::ResetDataStream { .. }) {
            got_reset = true;
        }
    }
    assert!(
        !got_reset,
        "stream 閉鎖後は OBJECT_DELIVERY_TIMEOUT 追跡が除去され ResetDataStream は発行されない"
    );
}

/// SUBGROUP_DELIVERY_TIMEOUT > 0 の subscription で subgroup stream FIN 時に
/// タイマーが開始され、timeout 超過時に ResetDataStream が発行されることを確認する
#[test]
fn subgroup_delivery_timeout_tracks_fin_and_emits_reset() {
    let (mut client, mut server) = establish_pair();
    client.tick(1_000);
    server.tick(1_000);

    // subscriber 側が SUBGROUP_DELIVERY_TIMEOUT=100 を指定
    let rid = client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam1".to_vec(),
            subgroup_delivery_timeout_params(100),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            rid,
            100,
            MessageParameters::new(),
            track_properties_with_subgroup_delivery_timeout(50),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // effective は subscriber(100) と publisher(50) の最小値 = 50
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .delivery_timeouts
            .effective_subgroup_ms,
        Some(50)
    );

    let stream_id = DataStreamId(10);
    server
        .send_subgroup_header(
            stream_id,
            rid,
            &SubgroupHeader {
                track_alias: 100,
                group_id: 1,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // subgroup stream を FIN で閉じる → SUBGROUP_DELIVERY_TIMEOUT タイマー開始
    server
        .send_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");

    // tick で時間を timeout 内に進める → ResetDataStream は発行されない
    server.tick(1_030);
    let mut got_reset = false;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::ResetDataStream { .. }) {
            got_reset = true;
        }
    }
    assert!(
        !got_reset,
        "timeout 未到達では ResetDataStream が発行されない"
    );

    // timeout を超える時間に進める
    server.tick(1_050); // FIN 時刻 1_000 + timeout 50ms = 1_050
    let mut reset_stream_id = None;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::ResetDataStream {
            stream_id: sid,
            error_code,
            reliable_size,
        } = e
        {
            assert_eq!(error_code, STREAM_DELIVERY_TIMEOUT);
            assert_eq!(
                reliable_size, None,
                "自動発火では RESET_STREAM (reliable_size なし) になること"
            );
            reset_stream_id = Some(sid);
        }
    }
    assert_eq!(
        reset_stream_id,
        Some(stream_id),
        "SUBGROUP_DELIVERY_TIMEOUT 超過で ResetDataStream が発行される"
    );
}

/// SUBGROUP_DELIVERY_TIMEOUT が無効な場合 (None) は FIN 時にタイマーが開始されないことを確認する
#[test]
fn subgroup_delivery_timeout_none_does_not_track_fin() {
    let (mut client, mut server) = establish_pair();
    client.tick(1_000);
    server.tick(1_000);

    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam1".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 100, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .delivery_timeouts
            .effective_subgroup_ms,
        None
    );

    let stream_id = DataStreamId(10);
    server
        .send_subgroup_header(
            stream_id,
            rid,
            &SubgroupHeader {
                track_alias: 100,
                group_id: 1,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");

    // SUBGROUP_DELIVERY_TIMEOUT が無効なので timeout は発生しない
    server.tick(2_000);
    let mut got_reset = false;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::ResetDataStream { .. }) {
            got_reset = true;
        }
    }
    assert!(
        !got_reset,
        "SUBGROUP_DELIVERY_TIMEOUT 無効では ResetDataStream は発行されない"
    );
}

/// Reset 終端の stream では SUBGROUP_DELIVERY_TIMEOUT が開始されないことを確認する
#[test]
fn subgroup_delivery_timeout_not_tracked_on_reset() {
    let (mut client, mut server) = establish_pair();
    client.tick(1_000);
    server.tick(1_000);

    let rid = client
        .send_subscribe(
            ns(&[b"live"]),
            b"cam1".to_vec(),
            subgroup_delivery_timeout_params(100),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            rid,
            100,
            MessageParameters::new(),
            track_properties_with_subgroup_delivery_timeout(50),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let stream_id = DataStreamId(10);
    server
        .send_subgroup_header(
            stream_id,
            rid,
            &SubgroupHeader {
                track_alias: 100,
                group_id: 1,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // Reset で stream を閉じる → SUBGROUP_DELIVERY_TIMEOUT は開始されない
    server
        .send_data_stream_closed(
            stream_id,
            RequestStreamEnd::Reset {
                error_code: STREAM_INTERNAL_ERROR,
                reliable_size: None,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // timeout 時間を超えても ResetDataStream は発行されない
    server.tick(2_000);
    let mut got_reset = false;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::ResetDataStream { .. }) {
            got_reset = true;
        }
    }
    assert!(
        !got_reset,
        "Reset 終端の stream では SUBGROUP_DELIVERY_TIMEOUT は開始されない"
    );
}
