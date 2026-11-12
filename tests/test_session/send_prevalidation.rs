//! 送信 API の事前検証 (sans-I/O の状態整合性)
//!
//! sans-I/O では encode は I/O 層で行われるため、送信 API が状態遷移後に encode で失敗すると
//! 状態だけ進んだ孤児状態が残る。応答 (FETCH_OK / TRACK_STATUS_OK) と PUBLISH の Track
//! Properties、および 4 つの request 送信 API の Full Track Name について、検証を状態遷移前
//! かつ request_id 発行前に揃える。
//!
//! - Track Properties の encode 失敗: `SESSION_PROTOCOL_VIOLATION` (0x3)
//! - Full Track Name の 4096 バイト超過:
//!   draft-ietf-moq-transport-23 §8.7 (Track Namespace Structure) の
//!   FIELD_LENGTH_EXCEEDED (0x401)

use super::*;
use shiguredo_moqt::message::common::TrackNamespace;
use shiguredo_moqt::session::types::SendRequestError;
use shiguredo_moqt::track_properties::{TrackProperty, TrackPropertyValue};

/// ローカル encode で失敗する Track Properties (Object 専用 Property を Track scope に置く)
///
/// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties): PRIOR_GROUP_ID_GAP は
/// Object 専用であり、Track scope の直列化は生成しない。
fn invalid_track_properties() -> TrackProperties {
    let mut tp = TrackProperties::new();
    tp.push(TrackProperty {
        prop_type: shiguredo_moqt::object_properties::PROP_PRIOR_GROUP_ID_GAP,
        value: TrackPropertyValue::VarInt(1),
    });
    tp
}

/// 4097 バイトの Full Track Name になる (namespace, track name) を作る
///
/// §8.7: Full Track Name の長さは Track Namespace Field Length の総和と Track Name Length の
/// 和で計算する。1 フィールド 4096 バイトの namespace に 1 バイトの Track Name を足す。
fn oversized_track_name() -> (TrackNamespace, Vec<u8>) {
    let namespace =
        TrackNamespace::new(vec![vec![b'a'; 4096]]).expect("1 フィールドの namespace を作れること");
    (namespace, vec![b'b'])
}

/// 不正な Track Properties を持つ FETCH_OK は状態を変えずに拒否される
#[test]
fn fetch_ok_with_invalid_track_properties_keeps_pending() {
    // client が FETCH を送り、server (publisher) が FETCH_OK 前の Pending
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
                    group_id: 1,
                    object_id: 0,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server.fetch(rid).expect("fetch entry が存在すること").state,
        FetchState::Pending
    );

    let err = server
        .send_fetch_ok(rid, MessageParameters::new(), invalid_track_properties())
        .expect_err("不正な Track Properties の FETCH_OK は拒否されること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    let fetch = server.fetch(rid).expect("fetch entry が存在すること");
    assert_eq!(fetch.state, FetchState::Pending, "Pending のままであること");
    assert_eq!(fetch.end_location, None, "End Location を設定しないこと");
    // SendOnStream が積まれていないこと
    let mut sent = 0;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::SendOnStream { .. }) {
            sent += 1;
        }
    }
    assert_eq!(sent, 0, "FETCH_OK を送信しないこと");
}

/// 不正な Track Properties を持つ TRACK_STATUS_OK は応答済みにしない
#[test]
fn track_status_ok_with_invalid_track_properties_keeps_unanswered() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_track_status(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let err = server
        .send_request_ok(rid, MessageParameters::new(), invalid_track_properties())
        .expect_err("不正な Track Properties の TRACK_STATUS_OK は拒否されること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    let entry = server
        .track_status_request(rid)
        .expect("entry が存在すること");
    assert_eq!(entry.response, None, "応答済みにしないこと");
    assert!(!entry.terminated, "終端扱いにしないこと");
    // control message deadline も維持される (応答待ちのまま)
    let mut sent = 0;
    while let Some(e) = server.poll_event() {
        if matches!(e, SessionEvent::SendOnStream { .. }) {
            sent += 1;
        }
    }
    assert_eq!(sent, 0, "TRACK_STATUS_OK を送信しないこと");
}

/// 不正な Track Properties を持つ PUBLISH は subscription / alias を登録しない
#[test]
fn publish_with_invalid_track_properties_registers_nothing() {
    let (mut client, _server) = establish_pair();

    let err = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            700,
            MessageParameters::new(),
            invalid_track_properties(),
        )
        .expect_err("不正な Track Properties の PUBLISH は拒否されること");
    match err {
        SendRequestError::Session(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("Session エラーが期待されたが {other:?} になった"),
    }

    // request_id も track_alias も登録されないので、同じ alias で次の PUBLISH が成功し、
    // 最初の request_id (0) が返る
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            700,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("正当な PUBLISH は成功すること");
    assert_eq!(rid, 0, "拒否時に request_id を発行しないこと");
    assert!(client.subscription(rid).is_some());
}

/// 4097 バイトの Full Track Name は 4 つの request 送信 API で拒否される
#[test]
fn oversized_full_track_name_rejected_by_all_request_apis() {
    let (namespace, track_name) = oversized_track_name();
    let (mut client, _server) = establish_pair();

    // send_subscribe
    let err = client
        .send_subscribe(
            namespace.clone(),
            track_name.clone(),
            MessageParameters::new(),
        )
        .expect_err("4097 バイトの SUBSCRIBE は拒否されること");
    assert_field_length_exceeded(err, "send_subscribe");

    // send_publish
    let err = client
        .send_publish(
            namespace.clone(),
            track_name.clone(),
            1,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect_err("4097 バイトの PUBLISH は拒否されること");
    assert_field_length_exceeded(err, "send_publish");

    // send_fetch
    let err = client
        .send_fetch(
            namespace.clone(),
            track_name.clone(),
            fetch_range_params(
                Location {
                    group_id: 0,
                    object_id: 0,
                },
                Location {
                    group_id: 1,
                    object_id: 0,
                },
            ),
        )
        .expect_err("4097 バイトの FETCH は拒否されること");
    assert_field_length_exceeded(err, "send_fetch");

    // send_track_status
    let err = client
        .send_track_status(namespace, track_name, MessageParameters::new())
        .expect_err("4097 バイトの TRACK_STATUS は拒否されること");
    assert_field_length_exceeded(err, "send_track_status");

    // request_id を発行していないことを、次の正当な送信が先頭の id を返すことで確認する
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("正当な SUBSCRIBE は成功すること");
    assert_eq!(rid, 0, "拒否時に request_id を発行しないこと");
    // track_alias も登録されていないので同じ alias で PUBLISH できる
    client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            1,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("同じ track_alias の PUBLISH が成功すること");
}

/// `SendRequestError` が FIELD_LENGTH_EXCEEDED の Session エラーであることを確認する
fn assert_field_length_exceeded(err: SendRequestError, label: &str) {
    match err {
        SendRequestError::Session(e) => assert_eq!(
            e.code, SESSION_FIELD_LENGTH_EXCEEDED,
            "{label} は FIELD_LENGTH_EXCEEDED (0x401) になること"
        ),
        other => panic!("{label}: Session エラーが期待されたが {other:?} になった"),
    }
}
