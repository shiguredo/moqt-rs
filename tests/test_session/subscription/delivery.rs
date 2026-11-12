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
            tracking,
        } = e
        {
            assert_eq!(error_code, STREAM_DELIVERY_TIMEOUT);
            assert_eq!(
                reliable_size, None,
                "自動発火では RESET_STREAM (reliable_size なし) になること"
            );
            assert_eq!(
                tracking,
                shiguredo_moqt::session::types::ResetTracking::Retained,
                "配送 timeout では PUBLISH_DONE の flush まで追跡を残すこと"
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
            tracking: _,
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
            tracking: _,
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
                error_code: Some(STREAM_INTERNAL_ERROR),
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

// ─── 送信側 Object Property override と送信前チェック ────

/// 先頭 Object の Object Property で delivery timeout を override する
///
/// draft-ietf-moq-transport-23 §5.2 (Delivery Timeouts and Data Reliability): "Either timeout
/// value can also be set as an Object Property on the first object in a subgroup, overriding
/// the Track-level value for that subgroup." 送信側も受信側と同じく override を反映する。
mod sender_override {
    use super::*;
    use shiguredo_moqt::error::STREAM_CANCELLED;
    use shiguredo_moqt::object_properties::{
        ObjectProperties, ObjectProperty, ObjectPropertyValue,
    };
    use shiguredo_moqt::session::types::{ResetTracking, SendRequestError};
    use shiguredo_moqt::track_properties::{
        PROP_OBJECT_DELIVERY_TIMEOUT, PROP_SUBGROUP_DELIVERY_TIMEOUT,
    };

    /// 送信する Object Properties の生バイト列を作る
    fn properties_bytes(subgroup_ms: Option<u64>, object_ms: Option<u64>) -> Vec<u8> {
        let mut properties = ObjectProperties::new();
        if let Some(value) = subgroup_ms {
            properties.push(ObjectProperty {
                prop_type: PROP_SUBGROUP_DELIVERY_TIMEOUT,
                value: ObjectPropertyValue::VarInt(value),
            });
        }
        if let Some(value) = object_ms {
            properties.push(ObjectProperty {
                prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
                value: ObjectPropertyValue::VarInt(value),
            });
        }
        let mut bytes = Vec::new();
        properties
            .encode(&mut bytes)
            .expect("Properties の encode は成功すること");
        bytes
    }

    /// subscriber 指定の OBJECT_DELIVERY_TIMEOUT 付きで購読を Established にする
    ///
    /// publisher の Track Property は `track_object_ms` とする。
    fn establish_with_timeouts(
        client: &mut Session,
        server: &mut Session,
        subscriber_object_ms: u64,
        track_object_ms: u64,
    ) -> u64 {
        let rid = client
            .send_subscribe(
                ns(&[b"live"]),
                b"cam-delivery".to_vec(),
                delivery_timeout_params(subscriber_object_ms),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, sub_msg) = take_send_request(client);
        server
            .recv_request(sub_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        server
            .send_subscribe_ok(
                rid,
                300,
                MessageParameters::new(),
                track_properties_with_delivery_timeout(track_object_ms),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok_msg) = take_send_on_stream(server);
        client
            .recv_stream_message(rid, ok_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        rid
    }

    /// pub 側で subgroup stream を開く
    fn open_stream(server: &mut Session, rid: u64, stream_id: DataStreamId) {
        server
            .send_subgroup_header(
                stream_id,
                rid,
                &SubgroupHeader {
                    track_alias: 300,
                    group_id: 1,
                    subgroup_id: SubgroupIdMode::Explicit(0),
                    publisher_priority: Some(128),
                    has_properties: true,
                    end_of_group: false,
                    first_object: false,
                },
            )
            .expect("SUBGROUP_HEADER の送信に成功すること");
    }

    /// 先頭 Object の override が送信側の SUBGROUP_DELIVERY_TIMEOUT に反映される
    ///
    /// Track Property が 50ms、override が 500ms の場合、50ms 経過しても reset されない。
    #[test]
    fn sender_override_extends_subgroup_delivery_timeout() {
        let (mut client, mut server) = establish_pair();
        client.tick(1_000);
        server.tick(1_000);
        let rid = establish_with_timeouts(&mut client, &mut server, 1_000, 50);
        let stream_id = DataStreamId(10);
        open_stream(&mut server, rid, stream_id);
        server
            .send_subgroup_object(stream_id, 0, Some(&properties_bytes(Some(500), None)))
            .expect("Object の送信に成功すること");
        server
            .send_data_stream_closed(stream_id, RequestStreamEnd::Fin)
            .expect("FIN の通知に成功すること");
        // override が登録されていること
        assert_eq!(
            server
                .subscription(rid)
                .expect("購読が存在すること")
                .delivery_timeouts
                .subgroup_overrides
                .get(&(1, 0))
                .copied(),
            Some((Some(500), None))
        );

        // Track Property だけなら 50ms で timeout するが、override の 500ms では未到達
        server.tick(1_400);
        let mut got_reset = false;
        while let Some(e) = server.poll_event() {
            if matches!(e, SessionEvent::ResetDataStream { .. }) {
                got_reset = true;
            }
        }
        assert!(
            !got_reset,
            "override が Track Property より長い場合は早く reset されないこと"
        );

        // override の 500ms を超えると reset される
        server.tick(1_500);
        let mut resets = Vec::new();
        while let Some(e) = server.poll_event() {
            if let SessionEvent::ResetDataStream {
                stream_id: sid,
                error_code,
                ..
            } = e
            {
                resets.push((sid, error_code));
            }
        }
        assert_eq!(
            resets,
            vec![(stream_id, STREAM_DELIVERY_TIMEOUT)],
            "override の期限を超えると DELIVERY_TIMEOUT で reset されること"
        );
    }

    /// 先頭 Object の override が送信側の OBJECT_DELIVERY_TIMEOUT に反映される
    ///
    /// Track Property が 50ms、override が 500ms の場合、tick で 50ms を超えても reset されない。
    #[test]
    fn sender_override_extends_object_delivery_timeout() {
        let (mut client, mut server) = establish_pair();
        client.tick(1_000);
        server.tick(1_000);
        let rid = establish_with_timeouts(&mut client, &mut server, 1_000, 50);
        let stream_id = DataStreamId(10);
        open_stream(&mut server, rid, stream_id);
        server
            .send_subgroup_object(stream_id, 0, Some(&properties_bytes(None, Some(500))))
            .expect("Object の送信に成功すること");
        assert_eq!(
            server
                .subscription(rid)
                .expect("購読が存在すること")
                .delivery_timeouts
                .subgroup_overrides
                .get(&(1, 0))
                .copied(),
            Some((None, Some(500)))
        );

        // Track Property の 50ms を超えても override の 500ms 以内なら reset されない
        server.tick(1_400);
        let mut got_reset = false;
        while let Some(e) = server.poll_event() {
            if matches!(e, SessionEvent::ResetDataStream { .. }) {
                got_reset = true;
            }
        }
        assert!(
            !got_reset,
            "override が Track Property より長い場合は早く reset されないこと"
        );

        // override の 500ms を超えると reset される
        server.tick(1_500);
        let mut resets = Vec::new();
        while let Some(e) = server.poll_event() {
            if let SessionEvent::ResetDataStream {
                stream_id: sid,
                error_code,
                ..
            } = e
            {
                resets.push((sid, error_code));
            }
        }
        assert_eq!(
            resets,
            vec![(stream_id, STREAM_DELIVERY_TIMEOUT)],
            "override の期限を超えると DELIVERY_TIMEOUT で reset されること"
        );
    }

    /// 再送が delivery timeout を超過すると送信せず LocalSubgroupTimeout を返す
    ///
    /// draft-ietf-moq-transport-23 §5.2: "The implementation SHOULD check object delivery
    /// timeouts before retransmitting object data if the underlying transport implementation
    /// allows."
    #[test]
    fn resend_over_object_delivery_timeout_returns_local_subgroup_timeout() {
        let (mut client, mut server) = establish_pair();
        client.tick(1_000);
        server.tick(1_000);
        let rid = establish_with_timeouts(&mut client, &mut server, 1_000, 100);
        let stream_id = DataStreamId(10);
        open_stream(&mut server, rid, stream_id);
        server
            .send_subgroup_object(stream_id, 0, None)
            .expect("初回送信に成功すること");
        while server.poll_event().is_some() {}

        // 100ms 経過後に tick が DELIVERY_TIMEOUT で stream を reset する
        server.tick(1_100);
        let mut resets = Vec::new();
        while let Some(e) = server.poll_event() {
            if let SessionEvent::ResetDataStream {
                stream_id: sid,
                error_code,
                reliable_size,
                tracking,
            } = e
            {
                resets.push((sid, error_code, reliable_size, tracking));
            }
        }
        assert_eq!(
            resets,
            vec![(
                stream_id,
                STREAM_DELIVERY_TIMEOUT,
                None,
                ResetTracking::Retained
            )],
            "DELIVERY_TIMEOUT で reset され、追跡は保持されること"
        );
        // reset 済みの stream への再送は transport に渡さない (重複 reset は発行しない)
        let err = server
            .send_subgroup_object(stream_id, 0, None)
            .expect_err("期限超過の再送は拒否されること");
        assert_eq!(err, SendRequestError::LocalSubgroupTimeout);
        assert!(
            !server
                .poll_event()
                .is_some_and(|e| matches!(e, SessionEvent::ResetDataStream { .. })),
            "再送の拒否で ResetDataStream を重複発行しないこと"
        );
        // 追跡が保持されているため、I/O 層の終端通知を受理できる
        server
            .send_data_stream_closed(stream_id, RequestStreamEnd::Fin)
            .expect("reset 後の終端通知は受理されること");
        // 最大位置も更新されていない (Object 0 のまま)
        assert_eq!(
            server
                .subscription(rid)
                .expect("購読が存在すること")
                .largest_received_location,
            Some(Location {
                group_id: 1,
                object_id: 0,
            })
        );
    }

    /// Track Property のみでも再送前チェックが働く
    #[test]
    fn resend_without_override_checks_track_property() {
        let (mut client, mut server) = establish_pair();
        client.tick(1_000);
        server.tick(1_000);
        let rid = establish_with_timeouts(&mut client, &mut server, 1_000, 100);
        let stream_id = DataStreamId(10);
        open_stream(&mut server, rid, stream_id);
        server
            .send_subgroup_object(stream_id, 0, None)
            .expect("初回送信に成功すること");
        while server.poll_event().is_some() {}

        server.tick(1_100);
        let err = server
            .send_subgroup_object(stream_id, 0, None)
            .expect_err("期限超過の再送は拒否されること");
        assert_eq!(err, SendRequestError::LocalSubgroupTimeout);
    }

    /// 期限前の再送は通る
    #[test]
    fn resend_before_timeout_passes() {
        let (mut client, mut server) = establish_pair();
        client.tick(1_000);
        server.tick(1_000);
        let rid = establish_with_timeouts(&mut client, &mut server, 1_000, 100);
        let stream_id = DataStreamId(10);
        open_stream(&mut server, rid, stream_id);
        server
            .send_subgroup_object(stream_id, 0, None)
            .expect("初回送信に成功すること");
        while server.poll_event().is_some() {}

        server.tick(1_050);
        server
            .send_subgroup_object(stream_id, 0, None)
            .expect("期限内の再送は通ること");
    }

    /// RESET 終端では override が削除される
    #[test]
    fn reset_termination_removes_override() {
        let (mut client, mut server) = establish_pair();
        client.tick(1_000);
        server.tick(1_000);
        let rid = establish_with_timeouts(&mut client, &mut server, 1_000, 50);
        let stream_id = DataStreamId(10);
        open_stream(&mut server, rid, stream_id);
        server
            .send_subgroup_object(stream_id, 0, Some(&properties_bytes(Some(500), None)))
            .expect("Object の送信に成功すること");
        assert!(
            server
                .subscription(rid)
                .expect("購読が存在すること")
                .delivery_timeouts
                .subgroup_overrides
                .contains_key(&(1, 0))
        );
        server
            .send_data_stream_closed(
                stream_id,
                RequestStreamEnd::Reset {
                    error_code: Some(STREAM_CANCELLED),
                    reliable_size: None,
                },
            )
            .expect("RESET 終端の通知に成功すること");
        assert!(
            !server
                .subscription(rid)
                .expect("購読が存在すること")
                .delivery_timeouts
                .subgroup_overrides
                .contains_key(&(1, 0)),
            "RESET 終端では override が削除されること"
        );
    }

    /// datagram は §5.2 の override 対象外であり、既存の LocalDatagramTimeout を維持する
    #[test]
    fn datagram_keeps_local_datagram_timeout() {
        let (mut client, mut server) = establish_pair();
        client.tick(1_000);
        server.tick(1_000);
        let _rid = establish_with_timeouts(&mut client, &mut server, 100, 100);
        // datagram 経路は override を見ないため、Track Property の 100ms で drop される
        server
            .send_object_datagram(_rid, 1, 0, None, None, false)
            .expect("初回 datagram は送信できること");
        // 送信直後は提供完了時刻が未確定のため、同じ時刻の tick で確定させる
        server.tick(1_000);
        server.tick(1_100);
        // 同じ Object の再送は delivery timeout を超えているため drop される
        let err = server
            .send_object_datagram(_rid, 1, 0, None, None, false)
            .expect_err("期限超過の datagram は drop されること");
        assert_eq!(err, SendRequestError::LocalDatagramTimeout);
    }
}
