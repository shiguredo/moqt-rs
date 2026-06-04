//! PUBLISH_STATE_NOTIFY のテスト (draft-ietf-moq-transport-21 §9.10)
//!
//! 送受信の往復、subscription 状態への反映、LARGEST_OBJECT の条件付き必須、
//! スコープ違反の拒否、subscriber 送信・非 subscription 宛の拒否、
//! MAX_REQUEST_UPDATES クレジット非消費を扱う.

use super::*;
use shiguredo_moqt::error::SESSION_TOO_MANY_REQUEST_UPDATES;
use shiguredo_moqt::message::PublishStateNotify;
use shiguredo_moqt::message_parameter::{
    MessageParameter, MessageParameterValue, PARAM_EXPIRES, PARAM_FORWARD, PARAM_LARGEST_OBJECT,
    PARAM_LOCATION_FILTER,
};
use shiguredo_moqt::{
    message::common::Location, message_parameter::LocationFilter, stream::subgroup::SubgroupHeader,
    stream::subgroup::SubgroupIdMode,
};

/// FORWARD 1 件を持つパラメータ群を作る
fn forward_params(value: u8) -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(value),
    });
    params
}

/// SUBSCRIBE を Established にし、server 側で {group, object} を公開する
fn establish_sub_with_object(group: u64, object: u64) -> (Session, Session, u64) {
    let (mut client, mut server) = establish_pair();
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(sub_rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // server (publisher) が指定位置を公開し、観測 Largest を確定させる
    let stream_id = DataStreamId(1);
    server
        .send_subgroup_header(
            stream_id,
            sub_rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: group,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, object, None)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server, sub_rid)
}

/// OpenFillFetchStream ではなく PublishStateNotifyReceived を取り出す
///
/// 他イベントは読み飛ばす (既存の take 系ヘルパーと同様の前提)。
/// FILL なし fixture では通知以外のイベントは積まれない。
fn take_notify(session: &mut Session) -> (u64, MessageParameters) {
    while let Some(e) = session.poll_event() {
        if let SessionEvent::PublishStateNotifyReceived {
            request_id,
            parameters,
        } = e
        {
            return (request_id, parameters);
        }
    }
    panic!("PublishStateNotifyReceived イベントが期待されたが発行されなかった");
}

/// FORWARD 通知の往復で subscriber 側 forward_state が更新される
/// (draft-ietf-moq-transport-21 §9.10)
#[test]
fn notify_forward_round_trip_updates_forward_state() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object(0, 0);
    server
        .send_publish_state_notify(sub_rid, forward_params(0))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        0,
        "通知された FORWARD が subscriber 側に反映されること"
    );
    let (rid, params) = take_notify(&mut client);
    assert_eq!(
        rid, sub_rid,
        "通知は同一 subscription の Request ID で届くこと"
    );
    assert_eq!(
        params.forward(),
        Some(0),
        "通知パラメータがアプリに渡ること"
    );
}

/// LOCATION_FILTER と LARGEST_OBJECT が subscriber 側状態に反映される
/// (draft-ietf-moq-transport-21 §9.10 / §9.20.10 / §9.20.18)
#[test]
fn notify_filter_and_largest_reflected() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object(0, 0);
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteRange {
                start: Location {
                    group_id: 0,
                    object_id: 0,
                },
                end_group_delta: 0,
            }
            .encode_to_bytes(),
        ),
    });
    params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 0,
            object: 0,
        },
    });
    server
        .send_publish_state_notify(sub_rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = client
        .subscription(sub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        sub.filter.is_some(),
        "通知された LOCATION_FILTER が反映されること"
    );
    assert_eq!(
        sub.largest_location,
        Some(Location {
            group_id: 0,
            object_id: 0
        }),
        "通知された LARGEST_OBJECT が反映されること"
    );
}

/// 観測 Largest がある場合、LARGEST_OBJECT なしの通知に自動で補われる
/// (draft-ietf-moq-transport-21 §9.10 / §9.20.18 の条件付き必須)
#[test]
fn notify_injects_largest_object_when_observed() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object(2, 0);
    server
        .send_publish_state_notify(sub_rid, forward_params(1))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_on_stream(&mut server);
    let params = match msg {
        ControlMessage::PublishStateNotify(notify) => notify.parameters,
        other => panic!("PublishStateNotify が期待されたが {other:?} を受け取った"),
    };
    assert_eq!(
        params.largest_object(),
        Some((2, 0)),
        "観測 Largest がある場合は LARGEST_OBJECT が補われること"
    );
    // subscriber 側にも反映される
    client
        .recv_stream_message(
            sub_rid,
            ControlMessage::PublishStateNotify(PublishStateNotify { parameters: params }),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location,
        Some(Location {
            group_id: 2,
            object_id: 0
        }),
        "補完された LARGEST_OBJECT が subscriber 側に反映されること"
    );
}

/// 観測 Largest がない場合、LARGEST_OBJECT なしの通知は省略のまま送れる
/// (draft-ietf-moq-transport-21 §9.20.18 の省略可)
#[test]
fn notify_without_largest_stays_omitted_when_nothing_published() {
    let (mut client, mut server) = establish_pair();
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(sub_rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_send_on_stream(&mut server);
    server
        .send_publish_state_notify(sub_rid, forward_params(1))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_on_stream(&mut server);
    match msg {
        ControlMessage::PublishStateNotify(notify) => {
            assert!(
                notify.parameters.largest_object().is_none(),
                "未公開なら LARGEST_OBJECT は省略できること"
            );
        }
        other => panic!("PublishStateNotify が期待されたが {other:?} を受け取った"),
    }
}

/// 観測より小さい LARGEST_OBJECT は観測値に引き上げられる
#[test]
fn notify_upgrades_smaller_largest_object() {
    let (mut _client, mut server, sub_rid) = establish_sub_with_object(2, 0);
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 0,
            object: 0,
        },
    });
    server
        .send_publish_state_notify(sub_rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_on_stream(&mut server);
    match msg {
        ControlMessage::PublishStateNotify(notify) => {
            assert_eq!(
                notify.parameters.largest_object(),
                Some((2, 0)),
                "観測より小さい LARGEST_OBJECT は引き上げられること"
            );
        }
        other => panic!("PublishStateNotify が期待されたが {other:?} を受け取った"),
    }
}

/// 許可外パラメータの通知送信は送信前に拒否し副作用を残さない
/// (draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope))
#[test]
fn notify_with_disallowed_parameter_rejected_without_side_effects() {
    let (_client, mut server, sub_rid) = establish_sub_with_object(0, 0);
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(100),
    });
    let err = server
        .send_publish_state_notify(sub_rid, params)
        .expect_err("許可外パラメータの通知は送信前に拒否されること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // forward_state は変わらず、イベントも発行されない
    assert_eq!(
        server
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1
    );
    while let Some(e) = server.poll_event() {
        assert!(
            !matches!(
                e,
                SessionEvent::SendOnStream { .. } | SessionEvent::CloseSession(_)
            ),
            "拒否した送信で SendOnStream / CloseSession は発行されないこと"
        );
    }
}

/// 通知送信は publisher 役・ Established の subscription に限る
#[test]
fn notify_send_requires_publisher_established() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object(0, 0);
    // subscriber 役からは送れない
    let err = client
        .send_publish_state_notify(sub_rid, forward_params(0))
        .expect_err("subscriber 役は通知を送れないこと");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // Pending の subscription からは送れない
    let pending_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pending_msg) = take_send_request(&mut client);
    server
        .recv_request(pending_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = server
        .send_publish_state_notify(pending_rid, forward_params(0))
        .expect_err("Pending の subscription からは送れないこと");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // 未知 ID からは送れない
    let err = server
        .send_publish_state_notify(999, forward_params(0))
        .expect_err("未知 ID からは送れないこと");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // Terminated からは送れない (setup 用 subgroup stream を先に終端する)
    server
        .send_data_stream_closed(DataStreamId(1), RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_publish_done(
            sub_rid,
            0x2,
            1,
            shiguredo_moqt::message::ReasonPhrase::new("done")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let err = server
        .send_publish_state_notify(sub_rid, forward_params(0))
        .expect_err("Terminated からは送れないこと");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// publisher 側で通知を受信したら PROTOCOL_VIOLATION でセッションを閉じる
/// (draft-ietf-moq-transport-21 §9.10: subscriber からの受信は MUST close)
#[test]
fn notify_received_on_publisher_side_closes_session() {
    let (mut client, mut server) = establish_pair();
    // server が PUBLISH を送り publisher 役の subscription を持つ
    let pub_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut server);
    client
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // publisher 役の server が通知を受け取ったら拒否する
    let err = server
        .recv_stream_message(
            pub_rid,
            ControlMessage::PublishStateNotify(PublishStateNotify {
                parameters: forward_params(0),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// subscription 以外への通知受信は PROTOCOL_VIOLATION でセッションを閉じる
/// (draft-ietf-moq-transport-21 §9.10)
#[test]
fn notify_for_non_subscription_closes_session() {
    let (mut client, mut server, _sub_rid) = establish_sub_with_object(0, 0);
    // FETCH の request_id 宛てに通知を送る
    let fetch_rid = client
        .send_fetch(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, fetch_msg) = take_send_request(&mut client);
    server
        .recv_request(fetch_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = server
        .recv_stream_message(
            fetch_rid,
            ControlMessage::PublishStateNotify(PublishStateNotify {
                parameters: forward_params(0),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// Pending (subscriber 役) の subscription への通知受信は拒否する
/// (state ゲートが発火すること。responder 側 Pending との対比用)
#[test]
fn notify_to_pending_subscriber_state_closes_session() {
    let (mut client, mut server) = establish_pair();
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // subscriber 役の client は SUBSCRIBE_OK 前 (Pending) で通知を受け取ったら拒否する
    let err = client
        .recv_stream_message(
            sub_rid,
            ControlMessage::PublishStateNotify(PublishStateNotify {
                parameters: forward_params(0),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(client.state(), SessionState::Closing);
}

/// Pending (publisher 役の responder 側) の subscription への通知受信は拒否する
/// (role ゲートが state ゲートより先に発火すること)
#[test]
fn notify_to_pending_publisher_responder_closes_session() {
    let (mut client, mut server) = establish_pair();
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = server
        .recv_stream_message(
            sub_rid,
            ControlMessage::PublishStateNotify(PublishStateNotify {
                parameters: forward_params(0),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// 値域外 FORWARD の通知受信は PROTOCOL_VIOLATION でセッションを閉じる
/// (draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter))
#[test]
fn notify_with_invalid_forward_closes_session() {
    let (mut _client, mut server, sub_rid) = establish_sub_with_object(0, 0);
    // decode 層を迂回して不正値を直接受信させる (decode 層も値域検証するため)
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(2),
    });
    let err = server
        .recv_stream_message(
            sub_rid,
            ControlMessage::PublishStateNotify(PublishStateNotify { parameters: params }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}

/// zero-length LOCATION_FILTER の通知で filter が削除される
/// (draft-ietf-moq-transport-21 §3.3.1 (Location Filters))
#[test]
fn notify_with_removed_filter_clears_filter() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object(0, 0);
    // 先に filter を設定する (REQUEST_UPDATE 経由)
    let mut upd = MessageParameters::new();
    upd.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteRange {
                start: Location {
                    group_id: 0,
                    object_id: 0,
                },
                end_group_delta: 0,
            }
            .encode_to_bytes(),
        ),
    });
    client
        .send_request_update(sub_rid, upd)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(sub_rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(
            sub_rid,
            MessageParameters::new(),
            TrackProperties::default(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let _ = take_send_on_stream(&mut server);
    assert!(
        server
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .filter
            .is_some()
    );
    // zero-length 通知で削除する
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(Vec::new()),
    });
    server
        .send_publish_state_notify(sub_rid, params)
        .expect("テストフィクスチャの前提条件を満たす");
    // 送信側の解決済み値も含めて 3 フィールドがクリアされる
    let server_sub = server
        .subscription(sub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(server_sub.filter.is_none());
    assert!(server_sub.filter_start.is_none());
    assert!(server_sub.filter_end.is_none());
    let (_, msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .filter
            .is_none(),
        "zero-length 通知で filter が削除されること"
    );
}

/// 省略されたフィールドは通知で変更されない
/// (draft-ietf-moq-transport-21 §9.10: 省略時は unchanged)
#[test]
fn notify_omitted_fields_stay_unchanged() {
    let (mut client, mut server, sub_rid) = establish_sub_with_object(0, 0);
    // filter を設定する (REQUEST_UPDATE 経由)
    let mut upd = MessageParameters::new();
    upd.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteRange {
                start: Location {
                    group_id: 0,
                    object_id: 0,
                },
                end_group_delta: 0,
            }
            .encode_to_bytes(),
        ),
    });
    client
        .send_request_update(sub_rid, upd)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(sub_rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(
            sub_rid,
            MessageParameters::new(),
            TrackProperties::default(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // FORWARD のみの通知では filter 系 3 フィールドが維持される
    server
        .send_publish_state_notify(sub_rid, forward_params(0))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = client
        .subscription(sub_rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(sub.forward_state, 0);
    assert!(sub.filter.is_some(), "省略された filter は維持されること");
    assert!(
        sub.filter_start.is_some() && sub.filter_end.is_some(),
        "省略された解決済み値も維持されること"
    );
    // filter のみの通知では forward_state が維持される
    let mut filter_only = MessageParameters::new();
    filter_only.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteRange {
                start: Location {
                    group_id: 0,
                    object_id: 0,
                },
                end_group_delta: 0,
            }
            .encode_to_bytes(),
        ),
    });
    server
        .send_publish_state_notify(sub_rid, filter_only)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        0,
        "省略された FORWARD は維持されること"
    );
}

/// PUBLISH_STATE_NOTIFY は MAX_REQUEST_UPDATES のクレジットを消費しない
/// (draft-ietf-moq-transport-21 §9.10)
///
/// PUBLISH 起点の subscription で publisher (initiator) が REQUEST_UPDATE を送り、
/// 通知を挟んで再度送ると上限超過になること (通知が回復も消費もしない証拠) で検証する。
#[test]
fn notify_does_not_consume_request_update_credit() {
    use shiguredo_moqt::parameter::{SetupOption, SetupOptionValue, SetupOptions};
    // client 側の MAX_REQUEST_UPDATES=1 (server の REQUEST_UPDATE 送出が 1 件で上限に達する)
    let mut client_opts = SetupOptions::new();
    client_opts.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_REQUEST_UPDATES,
        value: SetupOptionValue::VarInt(1),
    });
    let (mut client, mut server) = establish_pair_with_options(client_opts, SetupOptions::new());
    // server が PUBLISH し、client が REQUEST_OK で確立させる
    let pub_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            999,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut server);
    client
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .send_request_ok(
            pub_rid,
            MessageParameters::new(),
            TrackProperties::default(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(pub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 1 件目の REQUEST_UPDATE で outgoing クレジットを使い切る
    server
        .send_request_update(pub_rid, forward_params(1))
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(pub_rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 通知の送信はクレジット制限を受けない
    server
        .send_publish_state_notify(pub_rid, forward_params(1))
        .expect("通知送信はクレジット制限を受けないこと");
    let (_, notify_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(pub_rid, notify_msg)
        .expect("通知受信はクレジット制限を受けないこと");
    // 2 件目の REQUEST_UPDATE は上限超過で拒否される (通知が回復させていない証拠)
    let err = server
        .send_request_update(pub_rid, forward_params(1))
        .expect_err("上限超過の REQUEST_UPDATE は拒否されること");
    assert_eq!(err.code, SESSION_TOO_MANY_REQUEST_UPDATES);
}

/// 値域外 FORWARD の通知送信は送信前に拒否し副作用を残さない
/// (draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter))
#[test]
fn notify_send_with_invalid_forward_rejected_without_side_effects() {
    let (_client, mut server, sub_rid) = establish_sub_with_object(0, 0);
    let err = server
        .send_publish_state_notify(sub_rid, forward_params(2))
        .expect_err("値域外 FORWARD の通知は送信前に拒否されること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // forward_state は変わらず、イベントも発行されない
    assert_eq!(
        server
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1
    );
    while let Some(e) = server.poll_event() {
        assert!(
            !matches!(
                e,
                SessionEvent::SendOnStream { .. } | SessionEvent::CloseSession(_)
            ),
            "拒否した送信で SendOnStream / CloseSession は発行されないこと"
        );
    }
}

/// 不正形式 filter の通知送信は送信前に拒否し副作用を残さない
#[test]
fn notify_send_with_malformed_filter_rejected_without_side_effects() {
    let (_client, mut server, sub_rid) = establish_sub_with_object(0, 0);
    // EndGroupDelta 溢出の LOCATION_FILTER (Start {1, 0} + Delta MAX は溢出する)
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![
            0x01, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ]),
    });
    let err = server
        .send_publish_state_notify(sub_rid, params)
        .expect_err("不正形式 filter の通知は送信前に拒否されること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert!(
        server
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .filter
            .is_none(),
        "拒否した送信で filter は変わらないこと"
    );
}

/// 不正形式 filter の通知受信は PROTOCOL_VIOLATION でセッションを閉じる
#[test]
fn notify_with_malformed_filter_closes_session() {
    let (mut _client, mut server, sub_rid) = establish_sub_with_object(0, 0);
    // decode 層を迂回して不正形式 filter を直接受信させる
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![
            0x01, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ]),
    });
    let err = server
        .recv_stream_message(
            sub_rid,
            ControlMessage::PublishStateNotify(PublishStateNotify { parameters: params }),
        )
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(server.state(), SessionState::Closing);
}
