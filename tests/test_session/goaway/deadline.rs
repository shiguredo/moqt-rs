//! request stream GOAWAY の deadline 満了による終端と、遅延メッセージ・遅延 stream の吸収
//!
//! draft-ietf-moq-transport-22 §9.2 (GOAWAY) の request stream 版で timeout が満了した場合の
//! 状態遷移と、その後に届く peer メッセージ / data stream の扱いを検証する。

use super::*;

// ─── deadline 満了時の終端 (遅延状態の整理) ─────────────────────────────

/// GOAWAY の deadline 満了で、peer FIN 受信済み (遅延中) の PUBLISH 送信側 subscription が終端する
///
/// draft-ietf-moq-transport-22 §9.2 (GOAWAY): "When sent on a request stream, the sender
/// SHOULD reset the stream with GOING_AWAY after the indicated timeout." の reset 時点を
/// 「PUBLISH_DONE を送れない終端」とみなし、遅延状態を整理して `RequestTerminated` を
/// 1 回だけ発行すること。
#[test]
fn request_stream_goaway_timeout_terminates_deferred_publish_sender() {
    let (mut client, mut server) = establish_pair();
    let rid = establish_publish_sender_as_established(&mut client, &mut server, 801);
    server.tick(1_000);
    server
        .send_goaway_on_request_stream(rid, b"moqt://relay.example/".to_vec(), 100)
        .expect("request stream GOAWAY の送信に成功すること");
    let (_, _) = take_send_on_stream(&mut server);

    // peer (client) の FIN は publisher 役の server を終端しない (PUBLISH_DONE 待ちの遅延)
    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("peer FIN の通知に成功すること");
    assert_eq!(
        server
            .subscription(rid)
            .expect("遅延中の subscription が残ること")
            .state,
        SubscriptionState::Established,
        "PUBLISH_DONE を送るまで Established を維持すること"
    );

    // deadline 満了で reset と終端が 1 回ずつ出る
    server.tick(1_100);
    let mut resets = Vec::new();
    let mut terminations = Vec::new();
    while let Some(e) = server.poll_event() {
        match e {
            SessionEvent::ResetRequestStream {
                request_id,
                error_code,
            } => resets.push((request_id, error_code)),
            SessionEvent::RequestTerminated {
                request_id,
                kind,
                reason,
            } => terminations.push((request_id, kind, reason)),
            _ => {}
        }
    }
    assert_eq!(resets, vec![(rid, STREAM_GOING_AWAY)]);
    assert_eq!(
        terminations,
        vec![(rid, RequestKind::Publish, TerminationReason::GoawayTimeout)],
        "遅延中の subscription が GoawayTimeout で 1 回だけ終端すること"
    );
    assert_eq!(
        server
            .subscription(rid)
            .expect("終端後も subscription は回収まで残ること")
            .state,
        SubscriptionState::Terminated,
        "deadline 満了で subscription が Terminated になること"
    );
    assert!(
        server.goaway_drain_ready(),
        "終端により GOAWAY drain の blocker が解消すること"
    );

    // 以後の tick で再発行しない
    server.tick(2_000);
    while let Some(e) = server.poll_event() {
        assert!(
            !matches!(
                e,
                SessionEvent::ResetRequestStream { .. } | SessionEvent::RequestTerminated { .. }
            ),
            "deadline 満了後は reset も終端通知も再発行しないこと"
        );
    }
}

/// GOAWAY の deadline 満了で、peer FIN 未受信の PUBLISH 送信側 subscription も終端する
///
/// peer FIN 未受信 (`Established`) でも reset 後に届く peer FIN は `defers_peer_fin` 経路に
/// 入り終端が確定しないため、deadline 満了時に終端すること。
#[test]
fn request_stream_goaway_timeout_terminates_established_publish_sender_without_peer_fin() {
    let (mut client, mut server) = establish_pair();
    let rid = establish_publish_sender_as_established(&mut client, &mut server, 802);
    server.tick(1_000);
    server
        .send_goaway_on_request_stream(rid, b"moqt://relay.example/".to_vec(), 100)
        .expect("request stream GOAWAY の送信に成功すること");
    let (_, _) = take_send_on_stream(&mut server);

    server.tick(1_100);
    let mut terminations = Vec::new();
    while let Some(e) = server.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind,
            reason,
        } = e
        {
            terminations.push((request_id, kind, reason));
        }
    }
    assert_eq!(
        terminations,
        vec![(rid, RequestKind::Publish, TerminationReason::GoawayTimeout)],
        "peer FIN 未受信の Established も deadline 満了で終端すること"
    );

    // reset 後の peer FIN は拒否済み id の close として no-op で吸収される
    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("終端済み request の close が no-op で吸収されること");
    assert_eq!(
        server.state(),
        SessionState::Established,
        "no-op 吸収でセッションを閉じないこと"
    );
    while let Some(e) = server.poll_event() {
        assert!(
            !matches!(e, SessionEvent::RequestTerminated { .. }),
            "close 通知で RequestTerminated を再発行しないこと"
        );
    }
}

/// GOAWAY の deadline 満了後に peer の RESET_STREAM が届いても二重終端しない
#[test]
fn request_stream_goaway_timeout_absorbs_late_peer_reset() {
    let (_client, mut server, rid) = establish_subscribe_track(803);
    server.tick(1_000);
    server
        .send_goaway_on_request_stream(rid, b"moqt://relay.example/".to_vec(), 100)
        .expect("request stream GOAWAY の送信に成功すること");
    let (_, _) = take_send_on_stream(&mut server);

    server.tick(1_100);
    let mut terminated = 0;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::RequestTerminated { .. }) {
            terminated += 1;
        }
    }
    assert_eq!(terminated, 1, "deadline 満了で終端が 1 回だけ出ること");

    server
        .recv_request_stream_closed(
            rid,
            RequestStreamEnd::Reset {
                error_code: Some(0),
                reliable_size: None,
            },
        )
        .expect("終端済み request の RESET_STREAM が no-op で吸収されること");
    assert_eq!(server.state(), SessionState::Established);
    while let Some(e) = server.poll_event() {
        assert!(
            !matches!(e, SessionEvent::RequestTerminated { .. }),
            "RESET_STREAM で RequestTerminated を再発行しないこと"
        );
    }
}

/// GOAWAY の deadline 満了で FETCH の requester 側も終端する
#[test]
fn request_stream_goaway_timeout_terminates_fetch_requester() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fetch_range_params(
                Location {
                    group_id: 0,
                    object_id: 0,
                },
                Location {
                    group_id: 10,
                    object_id: 0,
                },
            ),
        )
        .expect("FETCH の送信に成功すること");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("FETCH の受信に成功すること");
    server
        .send_fetch_ok(
            rid,
            0,
            Location {
                group_id: 10,
                object_id: 0,
            },
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("FETCH_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("FETCH_OK の受信に成功すること");

    client.tick(1_000);
    client
        .send_goaway_on_request_stream(rid, Vec::new(), 100)
        .expect("request stream GOAWAY の送信に成功すること");
    let (_, _) = take_send_on_stream(&mut client);

    client.tick(1_100);
    let mut terminations = Vec::new();
    while let Some(e) = client.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind,
            reason,
        } = e
        {
            terminations.push((request_id, kind, reason));
        }
    }
    assert_eq!(
        terminations,
        vec![(rid, RequestKind::Fetch, TerminationReason::GoawayTimeout)],
        "fetch の requester も deadline 満了で終端すること"
    );
    let fetch = client.fetch(rid).expect("fetch entry が残ること");
    assert_eq!(fetch.state, FetchState::Terminated);
    assert!(
        fetch.response_received,
        "応答受領済みの状態は終端後も維持されること"
    );
}

/// GOAWAY の deadline 満了で、応答未受領 (Pending) の FETCH も終端する
///
/// FETCH_OK / REQUEST_ERROR を受信していない requester 側 fetch も drain の blocker に
/// 残らないよう終端し、`RequestTerminated` を 1 回だけ発行すること。
#[test]
fn request_stream_goaway_timeout_terminates_pending_fetch_requester() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            fetch_range_params(
                Location {
                    group_id: 0,
                    object_id: 0,
                },
                Location {
                    group_id: 10,
                    object_id: 0,
                },
            ),
        )
        .expect("FETCH の送信に成功すること");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("FETCH の受信に成功すること");

    client.tick(1_000);
    client
        .send_goaway_on_request_stream(rid, Vec::new(), 100)
        .expect("request stream GOAWAY の送信に成功すること");
    let (_, _) = take_send_on_stream(&mut client);

    client.tick(1_100);
    let mut terminations = Vec::new();
    while let Some(e) = client.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind,
            reason,
        } = e
        {
            terminations.push((request_id, kind, reason));
        }
    }
    assert_eq!(
        terminations,
        vec![(rid, RequestKind::Fetch, TerminationReason::GoawayTimeout)],
        "応答未受領の fetch も deadline 満了で終端すること"
    );
    let fetch = client.fetch(rid).expect("fetch entry が残ること");
    assert_eq!(fetch.state, FetchState::Terminated);
    assert!(!fetch.response_received, "応答未受領のまま終端すること");
    assert!(
        client.goaway_drain_ready(),
        "drain の blocker が解消すること"
    );
}

/// GOAWAY の deadline 満了で TRACK_STATUS の requester 側も終端する
#[test]
fn request_stream_goaway_timeout_terminates_track_status_requester() {
    let (mut client, _server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, _) = take_send_request(&mut client);

    client.tick(1_000);
    client
        .send_goaway_on_request_stream(rid, Vec::new(), 100)
        .expect("request stream GOAWAY の送信に成功すること");
    let (_, _) = take_send_on_stream(&mut client);

    client.tick(1_100);
    let mut terminations = Vec::new();
    while let Some(e) = client.poll_event() {
        if let SessionEvent::RequestTerminated {
            request_id,
            kind,
            reason,
        } = e
        {
            terminations.push((request_id, kind, reason));
        }
    }
    assert_eq!(
        terminations,
        vec![(
            rid,
            RequestKind::TrackStatus,
            TerminationReason::GoawayTimeout
        )],
        "TRACK_STATUS の requester も deadline 満了で終端すること"
    );
    let entry = client
        .track_status_request(rid)
        .expect("track status entry が残ること");
    assert!(entry.terminated, "送信方向が閉じたこと");
    assert!(
        entry.response.is_none(),
        "応答を合成せず、peer の遅延応答を吸収できる状態のまま残ること"
    );
    assert!(
        client.goaway_drain_ready(),
        "終端により GOAWAY drain の blocker が解消すること"
    );
}

/// TRACK_STATUS の responder は応答前に peer の RESET_STREAM を受けると drain を妨げなくなる
///
/// `goaway_drain_snapshot` の blocker 条件は「応答が `None` かつ `terminated` でない」である。
/// responder 側は応答を送れなくなった (`terminated`) 時点で `forget_track_status` により
/// 破棄できるため、応答未受領でも drain を妨げない。
#[test]
fn track_status_responder_terminated_by_peer_reset_does_not_block_drain() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("TRACK_STATUS の送信に成功すること");
    let (_, ts_msg) = take_send_request(&mut client);
    server
        .recv_request(ts_msg)
        .expect("TRACK_STATUS の受信に成功すること");

    // peer (requester) の RESET_STREAM で responder 側は応答を送れなくなる
    server
        .recv_request_stream_closed(
            rid,
            RequestStreamEnd::Reset {
                error_code: Some(0),
                reliable_size: None,
            },
        )
        .expect("RESET_STREAM の通知に成功すること");
    let entry = server
        .track_status_request(rid)
        .expect("track status entry が残ること");
    assert!(entry.terminated, "送信方向が閉じたこと");
    assert!(entry.response.is_none(), "応答は未送信のままであること");

    let snapshot = server.goaway_drain_snapshot();
    assert!(
        snapshot.blocking_track_status_request_ids.is_empty(),
        "応答を送れない TRACK_STATUS は drain の blocker にしないこと"
    );
    assert!(server.goaway_drain_ready(), "drain 完了と判定されること");
    assert!(
        server.forget_track_status(rid).is_some(),
        "終端済み TRACK_STATUS を破棄できること"
    );
}
