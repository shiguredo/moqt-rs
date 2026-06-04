//! REQUEST_UPDATE における Range Filter 内容検証とスコープ検証の順序テスト
//!
//! draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope) は、許可されない context に
//! 現れたパラメータに対して PROTOCOL_VIOLATION でのセッションクローズを MUST とする。
//! §3.3.2 (Range Filters) の内容検証を先に走らせると INVALID_FILTER の REQUEST_ERROR で
//! 継続してしまい MUST に到達しないため、スコープ検証が先であることを固定する。
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use super::*;
use shiguredo_moqt::message::common::Location;

/// スコープ違反が PROTOCOL_VIOLATION になり REQUEST_ERROR に格下げされないことを断言する
fn assert_scope_violation_closes_session(server: &mut Session, rid: u64) {
    let err = server
        .recv_stream_message(
            rid,
            inject_request_update(rid + 2, one_range_subgroup_filter()),
        )
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

/// FORWARD 値域外と Range Filter 違反が同一 REQUEST_UPDATE にある場合は
/// FORWARD の MUST close (§9.20.19) が優先され、INVALID_FILTER の REQUEST_ERROR に
/// 格下げされない
#[test]
fn invalid_forward_takes_precedence_over_range_filter_rejection() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    recv_response_helper(&mut client, rid, ok_msg);

    // FORWARD=2 (値域外) と Range Filter を同時に載せた REQUEST_UPDATE を注入する
    let mut params = one_range_subgroup_filter();
    params.push(shiguredo_moqt::message_parameter::MessageParameter {
        param_type: shiguredo_moqt::message_parameter::PARAM_FORWARD,
        value: shiguredo_moqt::message_parameter::MessageParameterValue::Uint8(2),
    });

    let err = server
        .recv_stream_message(rid, inject_request_update(rid + 2, params))
        .expect_err("FORWARD 値域外は PROTOCOL_VIOLATION になる");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);

    let mut saw_close = false;
    while let Some(e) = server.poll_event() {
        match e {
            SessionEvent::SendOnStream {
                message: ControlMessage::RequestError(err),
                ..
            } => {
                panic!(
                    "FORWARD 値域外を REQUEST_ERROR に格下げしてはいけない: error_code={:#x}",
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
