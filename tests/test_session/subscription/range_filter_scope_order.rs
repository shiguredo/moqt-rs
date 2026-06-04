//! REQUEST_UPDATE における Range Filter 内容検証とスコープ検証の順序テスト
//!
//! draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope) は、許可されない context に
//! 現れたパラメータに対して PROTOCOL_VIOLATION でのセッションクローズを MUST とする。
//! §3.3.2 (Range Filters) の内容検証を先に走らせると INVALID_FILTER の REQUEST_ERROR で
//! 継続してしまい MUST に到達しないため、スコープ検証が先であることを固定する。
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use super::*;
use shiguredo_moqt::error::REQUEST_INVALID_FILTER;
use shiguredo_moqt::message::common::Location;

/// スコープ違反が PROTOCOL_VIOLATION になり REQUEST_ERROR に格下げされないことを断言する
fn assert_scope_violation_closes_session(server: &mut Session, rid: u64) {
    let err = server
        .recv_stream_message(rid, inject_request_update(rid, one_range_subgroup_filter()))
        .expect_err("スコープ違反は PROTOCOL_VIOLATION になる");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    let mut saw_close = false;
    while let Some(e) = server.poll_event() {
        match e {
            SessionEvent::SendOnStream {
                message: ControlMessage::RequestError(err),
                ..
            } => {
                panic!(
                    "スコープ違反を REQUEST_ERROR に格下げしてはいけない: error_code={:#x}",
                    err.error_code
                );
            }
            SessionEvent::CloseSession(e) => {
                assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION);
                saw_close = true;
            }
            _ => {}
        }
    }
    assert!(
        saw_close,
        "CloseSession(PROTOCOL_VIOLATION) が発行されること"
    );
}

/// fetch context の REQUEST_UPDATE に Range Filter が来たらセッションを閉じる
///
/// `FETCH_UPDATE_ALLOWED_PARAMS` は AUTHORIZATION_TOKEN と SUBSCRIBER_PRIORITY のみで
/// 0x25-0x29 を含まない。
#[test]
fn fetch_context_range_filter_closes_session() {
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
                    group_id: 5,
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
                group_id: 5,
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

    assert_scope_violation_closes_session(&mut server, rid);
}

/// namespace publication context の REQUEST_UPDATE に Range Filter が来たらセッションを閉じる
///
/// `NAMESPACE_PUBLICATION_UPDATE_ALLOWED_PARAMS` は AUTHORIZATION_TOKEN のみ。
#[test]
fn namespace_publication_context_range_filter_closes_session() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_publish_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("PUBLISH_NAMESPACE の送信に成功すること");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("PUBLISH_NAMESPACE の受信に成功すること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("REQUEST_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("REQUEST_OK の受信に成功すること");

    assert_scope_violation_closes_session(&mut server, rid);
}

/// namespace subscription context の REQUEST_UPDATE に Range Filter が来たらセッションを閉じる
///
/// `NAMESPACE_SUBSCRIPTION_UPDATE_ALLOWED_PARAMS` は AUTHORIZATION_TOKEN と
/// TRACK_NAMESPACE_PREFIX のみ。
#[test]
fn namespace_subscription_context_range_filter_closes_session() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"example"]), MessageParameters::new())
        .expect("SUBSCRIBE_NAMESPACE の送信に成功すること");
    let (_, msg) = take_send_request(&mut client);
    server
        .recv_request(msg)
        .expect("SUBSCRIBE_NAMESPACE の受信に成功すること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("REQUEST_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("REQUEST_OK の受信に成功すること");

    assert_scope_violation_closes_session(&mut server, rid);
}

/// track subscription context ではスコープ上正当なので INVALID_FILTER に留まり、セッションは閉じない
///
/// スコープ検証を先に移したことで正当な context の扱いまで PROTOCOL_VIOLATION に
/// 格上げしてしまう逆方向の回帰を防ぐ。`TRACK_SUBSCRIPTION_UPDATE_ALLOWED_PARAMS` は
/// 0x25-0x29 を含むため、§3.3.2 の内容検証まで進んで REQUEST_ERROR になる。
#[test]
fn track_subscription_context_range_filter_stays_request_error() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_tracks(ns(&[b"example"]), MessageParameters::new())
        .expect("SUBSCRIBE_TRACKS の送信に成功すること");
    let (_, st_msg) = take_send_request(&mut client);
    server
        .recv_request(st_msg)
        .expect("SUBSCRIBE_TRACKS の受信に成功すること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("REQUEST_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("REQUEST_OK の受信に成功すること");

    server
        .recv_stream_message(rid, inject_request_update(rid, one_range_subgroup_filter()))
        .expect("INVALID_FILTER 拒否は Result::Ok (セッションは維持)");

    let mut saw_invalid_filter = false;
    while let Some(e) = server.poll_event() {
        match e {
            SessionEvent::SendOnStream {
                message: ControlMessage::RequestError(err),
                ..
            } => {
                assert_eq!(err.error_code, REQUEST_INVALID_FILTER);
                saw_invalid_filter = true;
            }
            SessionEvent::CloseSession(e) => {
                panic!("スコープ上正当な Range Filter でセッションを閉じてはいけない: {e:?}");
            }
            _ => {}
        }
    }
    assert!(
        saw_invalid_filter,
        "INVALID_FILTER の REQUEST_ERROR が送出されること"
    );
}
