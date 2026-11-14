//! namespace 発見・告知 (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) のテスト
//!
//! draft-ietf-moq-transport-23 §4 (Namespace Discovery) / §9.13-§9.18 の request と
//! 応答 stream 上のメッセージを、client / server の両ロールで往復させる。

use super::*;
use shiguredo_moqt::error::{
    REQUEST_DOES_NOT_EXIST, SESSION_INVALID_PARAMETER, SESSION_PROTOCOL_VIOLATION,
    SESSION_UNEXPECTED_MESSAGE,
};
use shiguredo_moqt::message::{
    Namespace, NamespaceDone, Publish, PublishNamespace, PublishSkipped, SubscribeNamespace,
    SubscribeTracks,
};
use shiguredo_moqt::message_parameter::{
    PARAM_INCLUDE_PROPERTIES, PARAM_PAUSED, PARAM_SUBSCRIBE_TRACKS_ID, PARAM_TRACK_NAMESPACE_PREFIX,
};
use shiguredo_moqt::session::types::{NamespaceRequestState, SendRequestError, SessionEvent};
use shiguredo_moqt::track_properties::{TrackProperty, TrackPropertyValue};

/// パラメータ 1 個を追加した空の `MessageParameters` を作る
fn params_with(param_type: u64, value: MessageParameterValue) -> MessageParameters {
    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter { param_type, value });
    parameters
}

/// SUBSCRIBE_NAMESPACE が NAMESPACE / NAMESPACE_DONE を往復し、prefix と suffix が
/// 結合された完全な namespace がアプリへ届く
#[test]
fn subscribe_namespace_lifecycle() {
    let (mut client, mut server) = establish_pair();

    let rid = client
        .send_subscribe_namespace(ns(&[b"live"]), MessageParameters::new())
        .expect("SUBSCRIBE_NAMESPACE を送信できること");
    let (sent_rid, message) = take_send_request(&mut client);
    assert_eq!(sent_rid, rid);
    assert!(
        matches!(message, ControlMessage::SubscribeNamespace(_)),
        "SUBSCRIBE_NAMESPACE が SendRequest に載ること: {message:?}"
    );

    // publisher 側は要求をアプリへ通知する
    server
        .recv_request(message)
        .expect("SUBSCRIBE_NAMESPACE を受理できること");
    let received = drain_events(&mut server);
    assert!(
        received.iter().any(|event| matches!(
            event,
            SessionEvent::SubscribeNamespaceReceived { request_id, .. } if *request_id == rid
        )),
        "SubscribeNamespaceReceived が発行されること: {received:?}"
    );

    // REQUEST_OK (SUBSCRIBE_NAMESPACE_OK)
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_NAMESPACE_OK を送信できること");
    let (_, ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok)
        .expect("SUBSCRIBE_NAMESPACE_OK を受理できること");
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("購読が登録されていること")
            .state,
        NamespaceRequestState::Established
    );

    // NAMESPACE は prefix と suffix を結合した完全な namespace で通知する
    server
        .send_namespace(rid, ns(&[b"cam"]), MessageParameters::new())
        .expect("NAMESPACE を送信できること");
    let (_, namespace) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, namespace)
        .expect("NAMESPACE を受理できること");
    let received = drain_events(&mut client);
    assert!(
        received.iter().any(|event| matches!(
            event,
            SessionEvent::NamespaceReceived { request_id, track_namespace, .. }
                if *request_id == rid && track_namespace == &ns(&[b"live", b"cam"])
        )),
        "prefix と suffix を結合した NamespaceReceived が発行されること: {received:?}"
    );

    // NAMESPACE_DONE
    server
        .send_namespace_done(rid, ns(&[b"cam"]))
        .expect("NAMESPACE_DONE を送信できること");
    let (_, done) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, done)
        .expect("NAMESPACE_DONE を受理できること");
    let received = drain_events(&mut client);
    assert!(
        received.iter().any(|event| matches!(
            event,
            SessionEvent::NamespaceDoneReceived { request_id, track_namespace }
                if *request_id == rid && track_namespace == &ns(&[b"live", b"cam"])
        )),
        "NamespaceDoneReceived が発行されること: {received:?}"
    );

    client
        .forget_namespace_subscription(rid)
        .expect("forget できること");
    server
        .forget_namespace_subscription(rid)
        .expect("forget できること");
    assert!(client.namespace_subscription(rid).is_none());
    assert!(server.namespace_subscription(rid).is_none());
}

/// SUBSCRIBE_TRACKS が PUBLISH と PUBLISH_SKIPPED を往復し、PUBLISH の相関情報を保持する
#[test]
fn subscribe_tracks_lifecycle() {
    let (mut client, mut server) = establish_pair();

    let rid = client
        .send_subscribe_tracks(ns(&[b"live"]), MessageParameters::new())
        .expect("SUBSCRIBE_TRACKS を送信できること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("SUBSCRIBE_TRACKS を受理できること");
    let received = drain_events(&mut server);
    assert!(
        received.iter().any(|event| matches!(
            event,
            SessionEvent::SubscribeTracksReceived { request_id, .. } if *request_id == rid
        )),
        "SubscribeTracksReceived が発行されること: {received:?}"
    );

    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_TRACKS_OK を送信できること");
    let (_, ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok)
        .expect("SUBSCRIBE_TRACKS_OK を受理できること");

    // 結果の PUBLISH は SUBSCRIBE_TRACKS_ID を載せる
    let publish_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            10,
            params_with(
                PARAM_SUBSCRIBE_TRACKS_ID,
                MessageParameterValue::VarInt(rid),
            ),
            TrackProperties::new(),
        )
        .expect("SUBSCRIBE_TRACKS の結果として PUBLISH を送信できること");
    let (_, publish) = take_send_request(&mut server);
    client
        .recv_request(publish)
        .expect("PUBLISH を受理できること");
    assert_eq!(
        client
            .subscription(publish_rid)
            .expect("PUBLISH の購読が登録されていること")
            .subscribe_tracks_id,
        Some(rid),
        "SUBSCRIBE_TRACKS_ID が相関情報として保持されること"
    );

    // PUBLISH_SKIPPED は prefix と suffix を結合した完全な namespace で通知する
    // (suffix は要求の prefix の後ろの部分である)
    server
        .send_publish_skipped(rid, ns(&[]), b"mic".to_vec())
        .expect("PUBLISH_SKIPPED を送信できること");
    let (_, message) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, message)
        .expect("PUBLISH_SKIPPED を受理できること");
    let received = drain_events(&mut client);
    assert!(
        received.iter().any(|event| matches!(
            event,
            SessionEvent::PublishSkippedReceived { request_id, track_namespace, track_name }
                if *request_id == rid
                    && track_namespace == &ns(&[b"live"])
                    && track_name == b"mic"
        )),
        "PublishSkippedReceived が発行されること: {received:?}"
    );
}

/// 同じ track の PUBLISH を同じ SUBSCRIBE_TRACKS に対して 2 回送れない
///
/// draft-ietf-moq-transport-23 §9.5.2 (Updating Namespace Subscriptions): "the publisher
/// MUST NOT send another PUBLISH for a Track it already published for that SUBSCRIBE_TRACKS."
#[test]
fn subscribe_tracks_rejects_duplicate_publish() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_tracks(ns(&[b"live"]), MessageParameters::new())
        .expect("SUBSCRIBE_TRACKS を送信できること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("SUBSCRIBE_TRACKS を受理できること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_TRACKS_OK を送信できること");
    let (_, ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok)
        .expect("SUBSCRIBE_TRACKS_OK を受理できること");

    let parameters = params_with(
        PARAM_SUBSCRIBE_TRACKS_ID,
        MessageParameterValue::VarInt(rid),
    );
    server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            10,
            parameters.clone(),
            TrackProperties::new(),
        )
        .expect("1 回目の PUBLISH は送信できること");
    let error = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            11,
            parameters,
            TrackProperties::new(),
        )
        .expect_err("同じ track の 2 回目の PUBLISH は拒否されること");
    assert!(
        matches!(error, SendRequestError::Session(_)),
        "セッション違反として拒否されること: {error:?}"
    );
}

/// PUBLISH_SKIPPED を送った track には PUBLISH を送れない
///
/// draft-ietf-moq-transport-23 §9.18 (PUBLISH_SKIPPED) / §3.6.3 (Skipped Tracks): "The
/// Publisher MUST NOT send a PUBLISH for a Track for a given SUBSCRIBE_TRACKS after
/// PUBLISH_SKIPPED has been sent."
#[test]
fn subscribe_tracks_rejects_publish_after_skipped() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_tracks(ns(&[b"live"]), MessageParameters::new())
        .expect("SUBSCRIBE_TRACKS を送信できること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("SUBSCRIBE_TRACKS を受理できること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_TRACKS_OK を送信できること");
    let (_, ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok)
        .expect("SUBSCRIBE_TRACKS_OK を受理できること");

    server
        .send_publish_skipped(rid, ns(&[]), b"mic".to_vec())
        .expect("PUBLISH_SKIPPED を送信できること");
    let error = server
        .send_publish(
            ns(&[b"live"]),
            b"mic".to_vec(),
            12,
            params_with(
                PARAM_SUBSCRIBE_TRACKS_ID,
                MessageParameterValue::VarInt(rid),
            ),
            TrackProperties::new(),
        )
        .expect_err("PUBLISH_SKIPPED 後の PUBLISH は拒否されること");
    assert!(
        matches!(error, SendRequestError::Session(_)),
        "セッション違反として拒否されること: {error:?}"
    );
}

/// PUBLISH_NAMESPACE が REQUEST_OK で Established になる
#[test]
fn publish_namespace_lifecycle() {
    let (mut client, mut server) = establish_pair();
    let rid = server
        .send_publish_namespace(ns(&[b"live"]), MessageParameters::new())
        .expect("PUBLISH_NAMESPACE を送信できること");
    let (_, message) = take_send_request(&mut server);
    client
        .recv_request(message)
        .expect("PUBLISH_NAMESPACE を受理できること");
    let received = drain_events(&mut client);
    assert!(
        received.iter().any(|event| matches!(
            event,
            SessionEvent::PublishNamespaceReceived { request_id, .. } if *request_id == rid
        )),
        "PublishNamespaceReceived が発行されること: {received:?}"
    );

    client
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("PUBLISH_NAMESPACE_OK を送信できること");
    let (_, ok) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, ok)
        .expect("PUBLISH_NAMESPACE_OK を受理できること");
    assert_eq!(
        server
            .publish_namespace_request(rid)
            .expect("告知が登録されていること")
            .state,
        NamespaceRequestState::Established
    );
}

/// NAMESPACE より先の NAMESPACE_DONE は UNEXPECTED_MESSAGE でセッションを閉じる
///
/// draft-ietf-moq-transport-23 §4.2: "If a subscriber receives a NAMESPACE_DONE before the
/// corresponding NAMESPACE, it MUST close the session with UNEXPECTED_MESSAGE."
#[test]
fn namespace_done_before_namespace_closes_session() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"live"]), MessageParameters::new())
        .expect("SUBSCRIBE_NAMESPACE を送信できること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("SUBSCRIBE_NAMESPACE を受理できること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_NAMESPACE_OK を送信できること");
    let (_, ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok)
        .expect("SUBSCRIBE_NAMESPACE_OK を受理できること");

    let error = client
        .recv_stream_message(
            rid,
            ControlMessage::NamespaceDone(NamespaceDone {
                track_namespace_suffix: ns(&[b"cam"]),
            }),
        )
        .expect_err("対応する NAMESPACE が無い NAMESPACE_DONE は拒否されること");
    assert_eq!(
        error.code, SESSION_UNEXPECTED_MESSAGE,
        "UNEXPECTED_MESSAGE で閉じること"
    );
    assert_eq!(client.state(), SessionState::Closing);
}

/// prefix 更新の REQUEST_UPDATE で新しい prefix に一致しない namespace は暗黙に終了する
///
/// draft-ietf-moq-transport-23 §9.5.2 (Updating Namespace Subscriptions): "For a
/// SUBSCRIBE_NAMESPACE, the REQUEST_OK implies a NAMESPACE_DONE for each advertised
/// namespace that no longer matches the prefix."
#[test]
fn request_update_prefix_ends_non_matching_namespace() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"live"]), MessageParameters::new())
        .expect("SUBSCRIBE_NAMESPACE を送信できること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("SUBSCRIBE_NAMESPACE を受理できること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_NAMESPACE_OK を送信できること");
    let (_, ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok)
        .expect("SUBSCRIBE_NAMESPACE_OK を受理できること");
    server
        .send_namespace(rid, ns(&[b"cam"]), MessageParameters::new())
        .expect("NAMESPACE を送信できること");
    let (_, namespace) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, namespace)
        .expect("NAMESPACE を受理できること");

    // 新しい prefix に一致しない namespace は暗黙の NAMESPACE_DONE になる
    client
        .send_request_update(
            rid,
            params_with(
                PARAM_TRACK_NAMESPACE_PREFIX,
                MessageParameterValue::TrackNamespacePrefix(ns(&[b"live", b"mic"])),
            ),
        )
        .expect("REQUEST_UPDATE を送信できること");
    let events = drain_events(&mut client);
    assert!(
        events.iter().any(|event| matches!(
            event,
            SessionEvent::NamespaceDoneReceived { request_id, track_namespace }
                if *request_id == rid && track_namespace == &ns(&[b"live", b"cam"])
        )),
        "新しい prefix に一致しない namespace の NamespaceDoneReceived: {events:?}"
    );

    // peer は prefix を更新する
    let update = events
        .iter()
        .find_map(|event| match event {
            SessionEvent::SendOnStream {
                request_id,
                message: ControlMessage::RequestUpdate(update),
                ..
            } if *request_id == rid => Some(update.clone()),
            _ => None,
        })
        .expect("REQUEST_UPDATE が送信されること");
    server
        .recv_stream_message(rid, ControlMessage::RequestUpdate(update))
        .expect("REQUEST_UPDATE を受理できること");
    assert_eq!(
        server
            .namespace_subscription(rid)
            .expect("購読が登録されていること")
            .track_namespace,
        ns(&[b"live", b"mic"]),
        "publisher 側の prefix が更新されること"
    );
}

/// SUBSCRIBE_NAMESPACE に許可されないパラメータを載せると INVALID_PARAMETER で閉じる
///
/// draft-ietf-moq-transport-23 §9.14 (SUBSCRIBE_NAMESPACE): "The only parameter that can
/// appear ... is AUTHORIZATION_TOKEN."
#[test]
fn subscribe_namespace_rejects_out_of_scope_parameter() {
    let (_client, mut server) = establish_pair();
    let message = ControlMessage::SubscribeNamespace(SubscribeNamespace {
        request_id: 0,
        track_namespace: ns(&[b"live"]),
        parameters: params_with(PARAM_PAUSED, MessageParameterValue::Uint8(1)),
    });
    let error = server
        .recv_request(message)
        .expect_err("許可されないパラメータは拒否されること");
    match error {
        shiguredo_moqt::session::types::RecvRequestError::Session(err) => {
            assert_eq!(err.code, SESSION_INVALID_PARAMETER);
        }
        other => panic!("Session エラーであること: {other:?}"),
    }
    assert_eq!(server.state(), SessionState::Closing);
}

/// 予約名前空間 `.` への namespace request は DOES_NOT_EXIST で拒否する
///
/// draft-ietf-moq-transport-23 §2.4.3 (Reserved Namespaces)
#[test]
fn namespace_request_rejects_reserved_namespace() {
    let (_client, mut server) = establish_pair();
    let message = ControlMessage::SubscribeTracks(SubscribeTracks {
        request_id: 0,
        track_namespace: ns(&[b"."]),
        parameters: MessageParameters::new(),
    });
    server
        .recv_request(message)
        .expect("拒否は Result::Ok であること");
    let (rid, response, fin) = take_send_on_stream_with_fin(&mut server);
    assert_eq!(rid, 0);
    match response {
        ControlMessage::RequestError(err) => {
            assert_eq!(err.error_code, REQUEST_DOES_NOT_EXIST);
        }
        other => panic!("REQUEST_ERROR が期待された: {other:?}"),
    }
    assert!(fin, "拒否は FIN で閉じること");
    assert_eq!(
        server.state(),
        SessionState::Established,
        "セッションは維持されること"
    );
}

/// 送信側の予約名前空間は API で拒否する
#[test]
fn outgoing_namespace_request_rejects_reserved_namespace() {
    let (mut client, _server) = establish_pair();
    assert!(
        client
            .send_subscribe_namespace(ns(&[b"."]), MessageParameters::new())
            .is_err(),
        "`.` への SUBSCRIBE_NAMESPACE は送信できないこと"
    );
    assert!(
        client
            .send_subscribe_tracks(ns(&[b".session"]), MessageParameters::new())
            .is_err(),
        "`.session` への SUBSCRIBE_TRACKS は送信できないこと"
    );
    assert!(
        client
            .send_publish_namespace(ns(&[b"."]), MessageParameters::new())
            .is_err(),
        "`.` への PUBLISH_NAMESPACE は送信できないこと"
    );
}

/// namespace request は stop_sending で終端できる
#[test]
fn namespace_request_can_be_cancelled() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"live"]), MessageParameters::new())
        .expect("SUBSCRIBE_NAMESPACE を送信できること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("SUBSCRIBE_NAMESPACE を受理できること");

    client
        .stop_sending(rid)
        .expect("SUBSCRIBE_NAMESPACE を cancel できること");
    assert_eq!(
        client
            .namespace_subscription(rid)
            .expect("購読が登録されていること")
            .state,
        NamespaceRequestState::Terminated
    );
}

/// response-only のメッセージが Session API へ来ると UNEXPECTED_MESSAGE で閉じる
///
/// NAMESPACE / NAMESPACE_DONE / PUBLISH_SKIPPED は request stream の先頭に置けない。
#[test]
fn response_only_message_as_request_closes_session() {
    let (_client, mut server) = establish_pair();
    let error = server
        .recv_request(ControlMessage::Namespace(Namespace {
            track_namespace_suffix: ns(&[b"cam"]),
            parameters: MessageParameters::new(),
        }))
        .expect_err("NAMESPACE を request の先頭としては受理しないこと");
    match error {
        shiguredo_moqt::session::types::RecvRequestError::Session(err) => {
            assert_eq!(err.code, SESSION_UNEXPECTED_MESSAGE);
        }
        other => panic!("Session エラーであること: {other:?}"),
    }
    assert_eq!(server.state(), SessionState::Closing);
}

/// namespace 系 request の受信テーブルを forget で除去できる
#[test]
fn namespace_request_forget_lifecycle() {
    let (mut client, mut server) = establish_pair();

    // PUBLISH_NAMESPACE
    let rid = client
        .send_publish_namespace(ns(&[b"live"]), MessageParameters::new())
        .expect("PUBLISH_NAMESPACE を送信できること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("PUBLISH_NAMESPACE を受理できること");
    assert!(client.publish_namespace_request(rid).is_some());
    assert!(server.publish_namespace_request(rid).is_some());
    client
        .forget_publish_namespace(rid)
        .expect("forget できること");
    server
        .forget_publish_namespace(rid)
        .expect("forget できること");
    assert!(client.publish_namespace_request(rid).is_none());
    assert!(server.publish_namespace_request(rid).is_none());

    // SUBSCRIBE_TRACKS
    let rid = client
        .send_subscribe_tracks(ns(&[b"live"]), MessageParameters::new())
        .expect("SUBSCRIBE_TRACKS を送信できること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("SUBSCRIBE_TRACKS を受理できること");
    assert!(client.track_subscription(rid).is_some());
    assert!(server.track_subscription(rid).is_some());
    client
        .forget_track_subscription(rid)
        .expect("forget できること");
    server
        .forget_track_subscription(rid)
        .expect("forget できること");
    assert!(client.track_subscription(rid).is_none());
    assert!(server.track_subscription(rid).is_none());
}

/// 受信側の PUBLISH が SUBSCRIBE_TRACKS の prefix と一致しないと PROTOCOL_VIOLATION になる
#[test]
fn subscribe_tracks_publish_with_mismatched_prefix_closes_session() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_tracks(ns(&[b"live"]), MessageParameters::new())
        .expect("SUBSCRIBE_TRACKS を送信できること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("SUBSCRIBE_TRACKS を受理できること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_TRACKS_OK を送信できること");
    let (_, ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok)
        .expect("SUBSCRIBE_TRACKS_OK を受理できること");

    // 要求の prefix と一致しない namespace の PUBLISH を直接渡す
    let publish = ControlMessage::Publish(Publish {
        request_id: 1,
        track_namespace: ns(&[b"other"]),
        track_name: b"cam".to_vec(),
        track_alias: 5,
        parameters: params_with(
            PARAM_SUBSCRIBE_TRACKS_ID,
            MessageParameterValue::VarInt(rid),
        ),
        track_properties: TrackProperties::new(),
    });
    let error = client
        .recv_request(publish)
        .expect_err("prefix と一致しない PUBLISH は拒否されること");
    match error {
        shiguredo_moqt::session::types::RecvRequestError::Session(err) => {
            assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
        }
        other => panic!("Session エラーであること: {other:?}"),
    }
}

/// 受信側の PUBLISH_SKIPPED の重複は冪等に扱う (仕様違反ではない)
#[test]
fn duplicate_publish_skipped_is_idempotent() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_tracks(ns(&[b"live"]), MessageParameters::new())
        .expect("SUBSCRIBE_TRACKS を送信できること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("SUBSCRIBE_TRACKS を受理できること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_TRACKS_OK を送信できること");
    let (_, ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok)
        .expect("SUBSCRIBE_TRACKS_OK を受理できること");

    let skipped = ControlMessage::PublishSkipped(PublishSkipped {
        track_namespace_suffix: ns(&[]),
        track_name: b"mic".to_vec(),
    });
    client
        .recv_stream_message(rid, skipped.clone())
        .expect("1 回目の PUBLISH_SKIPPED は受理できること");
    let received = drain_events(&mut client);
    assert!(
        received.iter().any(|event| matches!(
            event,
            SessionEvent::PublishSkippedReceived { request_id, .. } if *request_id == rid
        )),
        "1 回目の PublishSkippedReceived が発行されること: {received:?}"
    );
    client
        .recv_stream_message(rid, skipped)
        .expect("同じ track の PUBLISH_SKIPPED の重複は受理されること");
    assert_eq!(
        client.state(),
        SessionState::Established,
        "重複でセッションを閉じないこと"
    );
    let received = drain_events(&mut client);
    assert!(
        !received
            .iter()
            .any(|event| matches!(event, SessionEvent::PublishSkippedReceived { .. })),
        "重複の PublishSkippedReceived は発行しないこと: {received:?}"
    );
}

/// PUBLISH_NAMESPACE の受信も予約名前空間を DOES_NOT_EXIST で拒否する
#[test]
fn publish_namespace_rejects_reserved_namespace() {
    let (_client, mut server) = establish_pair();
    let message = ControlMessage::PublishNamespace(PublishNamespace {
        request_id: 0,
        track_namespace: ns(&[b"."]),
        parameters: MessageParameters::new(),
    });
    server
        .recv_request(message)
        .expect("拒否は Result::Ok であること");
    let (_, response, fin) = take_send_on_stream_with_fin(&mut server);
    match response {
        ControlMessage::RequestError(err) => assert_eq!(err.error_code, REQUEST_DOES_NOT_EXIST),
        other => panic!("REQUEST_ERROR が期待された: {other:?}"),
    }
    assert!(fin);
}

/// NAMESPACE の送受信で suffix の上限超過は INVALID_TRACK_NAMESPACE になる
#[test]
fn namespace_suffix_overflow_closes_session() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"live"]), MessageParameters::new())
        .expect("SUBSCRIBE_NAMESPACE を送信できること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("SUBSCRIBE_NAMESPACE を受理できること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_NAMESPACE_OK を送信できること");
    let (_, ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok)
        .expect("SUBSCRIBE_NAMESPACE_OK を受理できること");

    // 32 フィールドの prefix に 1 フィールドの suffix を足すと 33 フィールドになる
    let prefix: Vec<Vec<u8>> = (0..32).map(|i| format!("f{i}").into_bytes()).collect();
    let long_rid = client
        .send_subscribe_namespace(
            TrackNamespace::new(prefix).expect("32 フィールドの prefix は正当である"),
            MessageParameters::new(),
        )
        .expect("SUBSCRIBE_NAMESPACE を送信できること");
    let (_, long_message) = take_send_request(&mut client);
    server
        .recv_request(long_message)
        .expect("SUBSCRIBE_NAMESPACE を受理できること");
    server
        .send_request_ok(long_rid, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_NAMESPACE_OK を送信できること");
    let (_, long_ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(long_rid, long_ok)
        .expect("SUBSCRIBE_NAMESPACE_OK を受理できること");

    let error = client
        .recv_stream_message(
            long_rid,
            ControlMessage::Namespace(Namespace {
                track_namespace_suffix: ns(&[b"x"]),
                parameters: MessageParameters::new(),
            }),
        )
        .expect_err("33 フィールドになる suffix は拒否されること");
    assert_eq!(
        error.code,
        shiguredo_moqt::error::SESSION_INVALID_TRACK_NAMESPACE
    );
}

/// 応答 stream の RequestOk は EXPIRES だけを許可する
#[test]
fn namespace_ok_rejects_out_of_scope_parameter() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"live"]), MessageParameters::new())
        .expect("SUBSCRIBE_NAMESPACE を送信できること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("SUBSCRIBE_NAMESPACE を受理できること");

    // PAUSED は namespace の OK では許可されない
    let error = server
        .send_request_ok(
            rid,
            params_with(PARAM_PAUSED, MessageParameterValue::Uint8(1)),
            TrackProperties::new(),
        )
        .expect_err("許可されないパラメータを持つ OK は送信できないこと");
    assert_eq!(error.code, SESSION_PROTOCOL_VIOLATION);
}

/// SUBSCRIBE_NAMESPACE の応答 stream が終端したら広告中の namespace を暗黙に終了する
///
/// draft-ietf-moq-transport-23 §4.2.2 (Namespace Subscription State Management): "When a
/// subscriber receives a stream reset or FIN on a SUBSCRIBE_NAMESPACE response stream, it
/// SHOULD treat this as though each active namespace received a NAMESPACE_DONE."
#[test]
fn subscribe_namespace_stream_end_implies_namespace_done() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_namespace(ns(&[b"live"]), MessageParameters::new())
        .expect("SUBSCRIBE_NAMESPACE を送信できること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("SUBSCRIBE_NAMESPACE を受理できること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_NAMESPACE_OK を送信できること");
    let (_, ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok)
        .expect("SUBSCRIBE_NAMESPACE_OK を受理できること");
    server
        .send_namespace(rid, ns(&[b"cam"]), MessageParameters::new())
        .expect("NAMESPACE を送信できること");
    let (_, namespace) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, namespace)
        .expect("NAMESPACE を受理できること");
    drain_events(&mut client);

    client
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("stream 終端を受理できること");
    let events = drain_events(&mut client);
    assert!(
        events.iter().any(|event| matches!(
            event,
            SessionEvent::NamespaceDoneReceived { request_id, track_namespace }
                if *request_id == rid && track_namespace == &ns(&[b"live", b"cam"])
        )),
        "stream 終端で暗黙の NamespaceDoneReceived が発行されること: {events:?}"
    );
}

/// SUBSCRIBE_TRACKS の INCLUDE_PROPERTIES=0 は resulting PUBLISH の Track Properties を空にする
///
/// draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter): SUBSCRIBE_TRACKS の
/// INCLUDE_PROPERTIES=0 は "whether the resulting PUBLISH messages include Track Properties" を
/// 指定し、Track Properties は存在するが空にする SHOULD。
#[test]
fn subscribe_tracks_include_properties_zero_empties_publish_properties() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe_tracks(
            ns(&[b"live"]),
            params_with(PARAM_INCLUDE_PROPERTIES, MessageParameterValue::Uint8(0)),
        )
        .expect("SUBSCRIBE_TRACKS を送信できること");
    let (_, message) = take_send_request(&mut client);
    server
        .recv_request(message)
        .expect("SUBSCRIBE_TRACKS を受理できること");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_TRACKS_OK を送信できること");
    let (_, ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok)
        .expect("SUBSCRIBE_TRACKS_OK を受理できること");

    let mut properties = TrackProperties::new();
    properties.push(TrackProperty {
        prop_type: 0x02, // OBJECT_DELIVERY_TIMEOUT
        value: TrackPropertyValue::VarInt(1000),
    });
    let publish_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            10,
            params_with(
                PARAM_SUBSCRIBE_TRACKS_ID,
                MessageParameterValue::VarInt(rid),
            ),
            properties,
        )
        .expect("PUBLISH を送信できること");
    let (_, publish) = take_send_request(&mut server);
    client
        .recv_request(publish)
        .expect("PUBLISH を受理できること");
    assert!(
        client
            .subscription(publish_rid)
            .expect("購読が登録されていること")
            .peer_track_properties
            .as_slice()
            .is_empty(),
        "INCLUDE_PROPERTIES=0 の resulting PUBLISH は Track Properties が空であること"
    );
}
