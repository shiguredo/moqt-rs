//! MAX_REQUEST_UPDATES / MAX_FILTER_RANGES / GROUP_ORDER 値域のセッション層テスト
//!
//! draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES) /
//! §3.3.2 / §9.1.6 (MAX_FILTER_RANGES)
//!
//! 各テストは「エラーコード」だけでなく、キューに積まれたイベント・
//! subscription 状態など観測可能な副作用まで断言する。

use super::*;
use shiguredo_moqt::error::{REQUEST_INVALID_FILTER, SESSION_TOO_MANY_REQUEST_UPDATES};
use shiguredo_moqt::message_parameter::{
    MessageParameter, MessageParameterValue, PARAM_FORWARD, PARAM_OBJECT_PROPERTY_FILTER,
    PARAM_OBJECTID_FILTER, PARAM_SUBGROUP_FILTER,
};

/// SUBSCRIBE を確立して (client, server, request_id) を返す
fn establish_subscribe_with(
    client_opts: SetupOptions,
    server_opts: SetupOptions,
) -> (Session, Session, u64) {
    let (mut client, mut server) = establish_pair_with_options(client_opts, server_opts);
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");
    (client, server, rid)
}

/// Range 2 個の SUBGROUP_FILTER (Start=0..1 と Start=1..2)
fn two_range_subgroup_filter() -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_SUBGROUP_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x00, 0x00, 0x01, 0x00, 0x01]),
    });
    params
}

/// キューに SendOnStream が残っていないことを確認する
fn assert_no_send_on_stream(s: &mut Session) {
    while let Some(e) = s.poll_event() {
        assert!(
            !matches!(e, SessionEvent::SendOnStream { .. }),
            "SendOnStream が残っていてはいけない: {e:?}"
        );
    }
}

/// INVALID_FILTER 拒否時の副作用を検証する
///
/// - REQUEST_ERROR (INVALID_FILTER) がちょうど 1 件送出される
/// - CloseSession は出ない
/// - RequestUpdateReceived は出ない
/// - pending_update_params は更新されない
fn assert_invalid_filter_rejection(server: &mut Session, rid: u64) {
    let mut saw_invalid_filter = false;
    while let Some(e) = server.poll_event() {
        match e {
            SessionEvent::SendOnStream {
                message: ControlMessage::RequestError(err),
                ..
            } => {
                assert_eq!(err.error_code, REQUEST_INVALID_FILTER);
                assert!(
                    !saw_invalid_filter,
                    "INVALID_FILTER の REQUEST_ERROR が複数回送出されている"
                );
                saw_invalid_filter = true;
            }
            SessionEvent::SendOnStream { message, .. } => {
                panic!("REQUEST_ERROR 以外の SendOnStream は想定外: {message:?}");
            }
            SessionEvent::CloseSession(err) => {
                panic!("INVALID_FILTER でセッションが閉じてはいけない: {err:?}");
            }
            SessionEvent::RequestUpdateReceived { .. } => {
                panic!("INVALID_FILTER では RequestUpdateReceived を発行してはいけない");
            }
            _ => {}
        }
    }
    assert!(
        saw_invalid_filter,
        "INVALID_FILTER の REQUEST_ERROR が送出されること"
    );
    assert!(
        server
            .subscription(rid)
            .expect("subscription が存在する")
            .pending_update_params
            .is_none(),
        "拒否された REQUEST_UPDATE のパラメータが pending に残ってはいけない"
    );
}

// ─── MAX_REQUEST_UPDATES ──────────────────────────────────────────

/// peer が宣言した MAX_REQUEST_UPDATES=1 を超える送信は拒否され、メッセージは積まれない
///
/// draft-ietf-moq-transport-21 §9.1.7
#[test]
fn outgoing_request_update_exceeds_peer_max() {
    // peer (server) だけが上限 1 を宣言する。client 側の送信チェックは peer 宣言を見る。
    let (mut client, mut server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x08, 1));

    client
        .send_request_update(rid, MessageParameters::new())
        .expect("1 通目は peer MAX 以内");
    let (_, upd) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd)
        .expect("1 通目の受信に成功すること");

    let err = client
        .send_request_update(rid, MessageParameters::new())
        .expect_err("2 通目は peer MAX_REQUEST_UPDATES 超過");
    assert_eq!(err.code, SESSION_TOO_MANY_REQUEST_UPDATES);
    assert_no_send_on_stream(&mut client);
}

/// 自側が宣言した MAX_REQUEST_UPDATES=1 を超える受信はセッションを閉じる
///
/// draft-ietf-moq-transport-21 §9.1.7
///
/// 送信側 API は peer 宣言 (こちらでは server=1) で 2 通目を拒否するため、
/// 受信超過を検証するには 2 通目をメッセージ注入する。
#[test]
fn incoming_request_update_exceeds_local_max() {
    let (mut client, mut server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x08, 1));

    client
        .send_request_update(rid, MessageParameters::new())
        .expect("1 通目の送信に成功すること");
    let (_, upd1) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd1)
        .expect("1 通目の受信に成功すること");
    assert!(
        server
            .subscription(rid)
            .expect("subscription が存在する")
            .pending_update_params
            .is_some(),
        "1 通目は pending_update_params に蓄積される"
    );

    let upd2 = inject_request_update(rid, MessageParameters::new());
    let err = server
        .recv_stream_message(rid, upd2)
        .expect_err("2 通目受信は local MAX_REQUEST_UPDATES 超過");
    assert_eq!(err.code, SESSION_TOO_MANY_REQUEST_UPDATES);
    match drain_until_close(&mut server) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_TOO_MANY_REQUEST_UPDATES),
        other => panic!("CloseSession(TOO_MANY_REQUEST_UPDATES) が期待されたが {other:?}"),
    }
}

/// REQUEST_OK でクレジットが回復すると、再度 REQUEST_UPDATE を送れ SendOnStream に積まれる
///
/// draft-ietf-moq-transport-21 §9.1.7
#[test]
fn request_update_credit_recovers_after_ok() {
    let (mut client, mut server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x08, 1));

    client
        .send_request_update(rid, MessageParameters::new())
        .expect("1 通目の送信に成功すること");
    let (_, upd) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd)
        .expect("1 通目の受信に成功すること");

    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("REQUEST_OK の送信に成功すること");
    let (_, ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok)
        .expect("REQUEST_OK の受信に成功すること");

    client
        .send_request_update(rid, MessageParameters::new())
        .expect("クレジット回復後は再送できる");
    let (stream_rid, second) = take_send_on_stream(&mut client);
    assert_eq!(stream_rid, rid);
    assert!(
        matches!(second, ControlMessage::RequestUpdate(_)),
        "回復後の送信が RequestUpdate として積まれること: {second:?}"
    );
}

/// 検証失敗で REQUEST_UPDATE が送信されない場合、クレジットが消費されないこと
///
/// draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES):
/// "A REQUEST_UPDATE is considered outstanding from when it is sent until the sender
/// receives the corresponding REQUEST_OK or REQUEST_ERROR response."
/// 送信されなかった REQUEST_UPDATE は outstanding にならず、クレジットを消費しない。
/// peer_max=1 の peer に対して Pending 状態 (Established 前) で REQUEST_UPDATE を送ると
/// 検証失敗で Err になるが、その後 Established になってから正しく送信できることを検証する。
#[test]
fn failed_request_update_send_does_not_consume_credit() {
    let (mut client, mut server) =
        establish_pair_with_options(SetupOptions::new(), opts_with(0x08, 1));
    // SUBSCRIBE_OK を送らず、client 側を Pending(Subscriber) のままにする
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");

    // Pending (SUBSCRIBE_OK 前) の subscription に REQUEST_UPDATE を送る → 検証失敗
    let err = client
        .send_request_update(rid, MessageParameters::new())
        .expect_err("Pending 状態では REQUEST_UPDATE は検証失敗になる");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_no_send_on_stream(&mut client);
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .state,
        SubscriptionState::Pending,
        "検証失敗で state が変わらないこと"
    );
    assert!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .is_pending_subscriber(),
        "SUBSCRIBE_OK 前は Pending(Subscriber) のままであること"
    );

    // その後 Established にして REQUEST_UPDATE を送る → クレジットが消費されていないため成功
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("検証失敗後の正しい REQUEST_UPDATE はクレジットが残っていて送信できる");
    let (stream_rid, upd) = take_send_on_stream(&mut client);
    assert_eq!(stream_rid, rid);
    assert!(
        matches!(upd, ControlMessage::RequestUpdate(_)),
        "送信成功時のみクレジットが消費され SendOnStream に積まれること: {upd:?}"
    );
}

/// outstanding 満杯時の TOO_MANY_REQUEST_UPDATES 拒否で `forward_state` が変更されないこと
///
/// `send_update_for_subscription` は FORWARD パラメータで `forward_state` を楽観的に更新する
/// (draft §9.5)。上限チェックが検証の後方にあると、Err を返す前に state が更新済みになる
/// 不整合が生じるため、チェックは検証の前に残している。その回帰防御。
#[test]
fn too_many_request_updates_does_not_mutate_forward_state() {
    let (mut client, mut server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x08, 1));

    // 1 通目: FORWARD=0 で成功し、楽観的に forward_state が 0 になる
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    client
        .send_request_update(rid, params.clone())
        .expect("1 通目は peer MAX 以内");
    let (_, upd) = take_send_on_stream(&mut client);
    assert!(matches!(upd, ControlMessage::RequestUpdate(_)));
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .forward_state,
        0,
        "1 通目の FORWARD=0 で forward_state が 0 に更新されること"
    );

    // 2 通目: FORWARD=1 で TOO_MANY_REQUEST_UPDATES → forward_state は 0 のまま
    let mut params2 = MessageParameters::new();
    params2.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    let err = client
        .send_request_update(rid, params2)
        .expect_err("2 通目は peer MAX_REQUEST_UPDATES 超過");
    assert_eq!(err.code, SESSION_TOO_MANY_REQUEST_UPDATES);
    assert_no_send_on_stream(&mut client);
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在する")
            .forward_state,
        0,
        "TOO_MANY_REQUEST_UPDATES 拒否では forward_state が変更されないこと"
    );

    // 拒否時にクレジットが加算されていないことの間接検証:
    // REQUEST_OK 応答で 1 通目のクレジットが回復した後、再送が成功する
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("REQUEST_OK の送信に成功すること");
    let (_, ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok)
        .expect("REQUEST_OK の受信に成功すること");
    client
        .send_request_update(rid, params.clone())
        .expect("クレジット回復後は再送できる (拒否時に加算されていないこと)");
    let (_, upd2) = take_send_on_stream(&mut client);
    assert!(matches!(upd2, ControlMessage::RequestUpdate(_)));
}

// ─── MAX_FILTER_RANGES ────────────────────────────────────────────

/// peer の MAX_FILTER_RANGES=1 を超える 2 Range の送信は拒否され、メッセージは積まれない
///
/// draft-ietf-moq-transport-21 §3.3.2 / §9.1.6
#[test]
fn outgoing_range_filters_exceed_peer_max() {
    let (mut client, _server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x06, 1));

    let err = client
        .send_request_update(rid, two_range_subgroup_filter())
        .expect_err("Range 2 個は peer MAX_FILTER_RANGES=1 超過");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_no_send_on_stream(&mut client);
}

/// 異種 Range Filter の Range 合計が peer MAX を超えると送信拒否される
///
/// draft-ietf-moq-transport-21 §9.1.6: 全 Range Filter 内の Range 総数
#[test]
fn outgoing_cross_type_range_filters_exceed_peer_max() {
    let (mut client, _server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x06, 1));

    let mut params = one_range_subgroup_filter();
    params.push(MessageParameter {
        param_type: PARAM_OBJECTID_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x00, 0x00, 0x01]),
    });
    assert_eq!(params.count_range_filters(), 2);

    let err = client
        .send_request_update(rid, params)
        .expect_err("異種合算 2 Range は peer MAX_FILTER_RANGES=1 超過");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_no_send_on_stream(&mut client);
}

/// MAX_FILTER_RANGES 未交渉 (デフォルト 0) では非空 Range Filter の送信が拒否される
///
/// draft-ietf-moq-transport-21 §9.1.6
#[test]
fn outgoing_range_filters_rejected_when_max_is_zero() {
    let (mut client, _server, rid) = establish_subscribe_track(1);

    let err = client
        .send_request_update(rid, one_range_subgroup_filter())
        .expect_err("未交渉では Range Filter 送信不可");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_no_send_on_stream(&mut client);
}

/// MAX_FILTER_RANGES 未交渉 (デフォルト 0) で受信した非空 Range Filter は INVALID_FILTER
///
/// draft-ietf-moq-transport-21 §9.1.6 / §3.3.2
///
/// 送信 API は peer_max=0 で拒否するため、受信経路はメッセージ注入で検証する。
#[test]
fn incoming_range_filters_rejected_when_max_is_zero() {
    let (_client, mut server, rid) = establish_subscribe_track(1);

    let upd = inject_request_update(rid, one_range_subgroup_filter());
    server
        .recv_stream_message(rid, upd)
        .expect("INVALID_FILTER 拒否は Result::Ok (セッションは維持)");
    assert_invalid_filter_rejection(&mut server, rid);
}

/// 自側 MAX_FILTER_RANGES=1 を超える受信は INVALID_FILTER で拒否される
///
/// draft-ietf-moq-transport-21 §3.3.2
#[test]
fn incoming_range_filters_exceed_local_max() {
    // server が local max=1 を宣言。client の peer_max も 1 になるため 2 Range は
    // 送信 API で拒否される → 受信超過はメッセージ注入で検証する。
    let (_client, mut server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x06, 1));

    let upd = inject_request_update(rid, two_range_subgroup_filter());
    server
        .recv_stream_message(rid, upd)
        .expect("INVALID_FILTER 拒否は Result::Ok (セッションは維持)");
    assert_invalid_filter_rejection(&mut server, rid);
}

/// REQUEST_UPDATE 拒否 (INVALID_FILTER) は登録済み request への応答のため
/// `rejected_request_ids` に記録されず、ストリームクローズは従来どおり
/// `request_streams` 経由で処理されること
///
/// `emit_request_error` の拒否 id 記録は `request_streams` 未登録の場合のみ行う。
/// 登録済み request への REQUEST_ERROR (REQUEST_UPDATE 拒否等) を記録すると、
/// クローズ通知時に集合から削除されずに残り続ける (リーク) 上、
/// `forget_*` 後の遅延クローズが no-op で吸収され、2 回目以降のクローズを
/// unknown id として fail させる保護が失われる。
#[test]
fn request_update_rejection_close_goes_through_request_streams() {
    let (_client, mut server, rid) = establish_subscribe_track(1);
    // 登録済み subscription への REQUEST_UPDATE を Range Filter 違反で拒否
    let upd = inject_request_update(rid, one_range_subgroup_filter());
    server
        .recv_stream_message(rid, upd)
        .expect("INVALID_FILTER 拒否は Result::Ok (セッションは維持)");
    assert_invalid_filter_rejection(&mut server, rid);
    // 登録済み request のため no-op ではなく通常経路で処理される (RequestTerminated 発行)
    server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .expect("登録済み request のクローズは通常経路で処理されること");
    let mut terminated = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::RequestTerminated { request_id, .. } = e {
            assert_eq!(request_id, rid);
            terminated = true;
        }
    }
    assert!(
        terminated,
        "登録済み request のクローズで RequestTerminated が発行されること"
    );
    // forget 後の 2 回目のクローズ通知は従来どおり unknown id として fail する
    server
        .forget_subscription(rid)
        .expect("Terminated 状態の subscription は破棄できること");
    let err = server
        .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
        .unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

/// MAX_FILTER_RANGES=1 で Range 1 個の REQUEST_UPDATE は受理され、状態に反映される
///
/// draft-ietf-moq-transport-21 §3.3.2
#[test]
fn incoming_range_filters_within_max_accepted() {
    let (mut client, mut server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x06, 1));

    let params = one_range_subgroup_filter();
    client
        .send_request_update(rid, params.clone())
        .expect("Range 1 個は peer MAX_FILTER_RANGES=1 で送信できる");
    let (_, upd) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd)
        .expect("Range 1 個の受信に成功すること");

    // RequestUpdateReceived が発行され、pending にフィルタが残ること
    let mut saw_update = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::RequestUpdateReceived {
            request_id,
            parameters,
        } = e
        {
            assert_eq!(request_id, rid);
            assert_eq!(parameters.count_range_filters(), 1);
            saw_update = true;
        }
    }
    assert!(saw_update, "RequestUpdateReceived が発行されること");
    let pending = server
        .subscription(rid)
        .expect("subscription が存在する")
        .pending_update_params
        .as_ref()
        .expect("受理された REQUEST_UPDATE は pending_update_params に残る");
    assert_eq!(pending.count_range_filters(), 1);
}

// ─── 同一 Parameter Type の複数出現 (draft-ietf-moq-transport-21 §3.3.2) ──────

/// SetID=0 のみで Range を 1 つも持たない SUBGROUP_FILTER を作る
///
/// Range 総数は 0 と数えられるため、Range 総数で検証を門番すると
/// §3.3.2 の重複拒否がすり抜ける。その門番の回帰を検出するための入力。
fn zero_range_subgroup_filter() -> MessageParameter {
    MessageParameter {
        param_type: PARAM_SUBGROUP_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x00]),
    }
}

/// Range 0 個の同一 (Parameter Type, SetID) が重複したら INVALID_FILTER で拒否される
///
/// draft-ietf-moq-transport-21 §3.3.2: "If the same combination of Parameter Type, SetID,
/// and Property Type ... repeat in any message, an endpoint MUST reject this with
/// REQUEST_ERROR with error code INVALID_FILTER."
///
/// Range を 1 つも持たないインスタンスは Range 総数 0 と数えられる。検証を Range 総数で
/// 門番していると重複拒否に到達できないため、パラメータの有無で門番していることを確認する。
#[test]
fn incoming_zero_range_duplicate_filters_are_rejected() {
    let (_client, mut server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x06, 1));

    let mut params = MessageParameters::new();
    params.push(zero_range_subgroup_filter());
    params.push(zero_range_subgroup_filter());
    assert_eq!(
        params.count_range_filters(),
        0,
        "Range を持たないので Range 総数は 0 になる"
    );

    let upd = inject_request_update(rid, params);
    server
        .recv_stream_message(rid, upd)
        .expect("INVALID_FILTER 拒否は Result::Ok (セッションは維持)");
    assert_invalid_filter_rejection(&mut server, rid);
}

/// SetID が異なる同一型 Range Filter 2 本は送信 API から受信状態反映まで通る
///
/// draft-ietf-moq-transport-21 §3.3.2: 同一 Parameter Type は複数回出現でき、
/// SetID ごとの結果は OR 結合される。1 インスタンスは 1 SetID しか持てないため、
/// 複数 SetID を表現するには複数出現が必須になる。
#[test]
fn incoming_distinct_set_id_range_filters_are_accepted() {
    let (mut client, mut server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x06, 2));

    let mut params = MessageParameters::new();
    params.push(one_range_subgroup_filter_with_set_id(0));
    params.push(one_range_subgroup_filter_with_set_id(1));

    client
        .send_request_update(rid, params)
        .expect("SetID が異なる同一型 2 本は peer MAX_FILTER_RANGES=2 で送信できる");
    let (_, upd) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd)
        .expect("同一型 2 本の受信に成功すること");

    let mut saw_update = false;
    while let Some(e) = server.poll_event() {
        if let SessionEvent::RequestUpdateReceived {
            request_id,
            parameters,
        } = e
        {
            assert_eq!(request_id, rid);
            assert_eq!(
                parameters.count_range_filters(),
                2,
                "2 本の Range が合算されること"
            );
            let filters = parameters.range_filters(PARAM_SUBGROUP_FILTER);
            assert_eq!(
                filters,
                vec![[0x00, 0x00, 0x01].as_slice(), [0x01, 0x00, 0x01].as_slice()],
                "同一型 2 インスタンスが出現順で保たれること"
            );
            saw_update = true;
        }
    }
    assert!(saw_update, "RequestUpdateReceived が発行されること");
}

/// PUBLISH_OK に Range Filter が来たらスコープ検証でセッションを閉じる
///
/// draft-ietf-moq-transport-21 Appendix A.1 #1790 (Subscription parameters appear in
/// REQUEST_UPDATE, not PUBLISH_OK): PUBLISH_OK は EXPIRES のみを運ぶ。
/// Range Filter を含む PUBLISH_OK はスコープ外として PROTOCOL_VIOLATION で
/// セッションを閉じる。
///
/// 送信経路もスコープ外パラメータを弾くため、send_request_ok では作れない。
/// 検証対象は受信側の扱いなので、ワイヤメッセージを直接構築して注入する。
#[test]
fn publish_ok_with_range_filter_closes_session() {
    let (mut client, mut server) =
        establish_pair_with_options(opts_with(0x06, 1), SetupOptions::new());

    // publisher (client) から PUBLISH を送り、subscriber (server) が PUBLISH_OK で応答する
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            111,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("PUBLISH の送信に成功すること");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("PUBLISH の受信に成功すること");

    let mut ok_params = MessageParameters::new();
    ok_params.push(zero_range_subgroup_filter());
    // 送信経路もスコープ外パラメータを弾くため、send_request_ok では作れない。
    // 検証対象は受信側の扱いなので、ワイヤメッセージを直接構築して注入する。
    let ok_msg = ControlMessage::RequestOk(shiguredo_moqt::message::RequestOk {
        parameters: ok_params,
        track_properties: TrackProperties::default(),
    });

    let err = client
        .recv_stream_message(rid, ok_msg)
        .expect_err("Range Filter を含む PUBLISH_OK は PROTOCOL_VIOLATION になる");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        err.reason, "REQUEST_OK (publish) parameter not allowed in this context",
        "スコープ検証で拒否されなければならない"
    );
    match drain_until_close(&mut client) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("CloseSession(PROTOCOL_VIOLATION) が期待されたが {other:?}"),
    }
}

/// 同一 (Parameter Type, SetID) が重複した Range Filter は送信 API で拒否される
///
/// draft-ietf-moq-transport-21 §3.3.2 により peer は必ず INVALID_FILTER で拒否するため、
/// 自側で送信前に弾く。セッションは閉じず Err を返すだけに留まる。
#[test]
fn outgoing_duplicate_range_filters_are_rejected() {
    let (mut client, _server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x06, 2));

    let mut params = MessageParameters::new();
    params.push(one_range_subgroup_filter_with_set_id(0));
    params.push(one_range_subgroup_filter_with_set_id(0));

    let err = client
        .send_request_update(rid, params)
        .expect_err("同一 (Type, SetID) の重複は送信前に拒否される");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    // 上限超過 (peer MAX=2 に対し Range 2 個なので発火しない) と同じエラーコードなので、
    // 重複検証で弾かれたことを reason で区別する
    assert_eq!(
        err.reason, "outgoing Range Filters are invalid",
        "上限超過ではなく重複検証で拒否されなければならない"
    );
    assert_no_send_on_stream(&mut client);
    // 送信拒否はローカルなエラーであり、セッションを閉じてはいけない
    while let Some(e) = client.poll_event() {
        assert!(
            !matches!(e, SessionEvent::CloseSession(_)),
            "送信拒否でセッションが閉じてはいけない: {e:?}"
        );
    }
}

// ─── プロトコル層 REQUEST_ERROR のクレジット回復 (§9.1.7) ────────────

/// プロトコル層が自動送出する REQUEST_ERROR でもクレジットが回復する
///
/// draft-ietf-moq-transport-21 §9.1.7: "Each REQUEST_OK or REQUEST_ERROR response
/// restores one credit on that stream"。アプリ層 API を経由しない自動応答も
/// REQUEST_ERROR であり、回復対象に含まれる。
///
/// 自側 MAX_REQUEST_UPDATES=1 / MAX_FILTER_RANGES=0 (未宣言) の状態で、Range Filter を
/// 載せた REQUEST_UPDATE を INVALID_FILTER で拒否したあと、正当な REQUEST_UPDATE が
/// TOO_MANY_REQUEST_UPDATES にならずに受理されることを確認する。
#[test]
fn request_update_credit_recovers_after_protocol_level_error() {
    let (mut client, mut server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x08, 1));

    // client の送信 API は peer MAX_FILTER_RANGES=0 で弾くため、拒否対象は注入で作る
    server
        .recv_stream_message(rid, inject_request_update(rid, one_range_subgroup_filter()))
        .expect("INVALID_FILTER 拒否は Result::Ok (セッションは維持)");
    assert_invalid_filter_rejection(&mut server, rid);

    // 拒否でクレジットが回復していなければ、ここで上限超過になりセッションが閉じる
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("client 側の outstanding は 0 のままなので送信できる");
    let (_, upd) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd)
        .expect("拒否でクレジットが回復しているため受理されること");

    while let Some(e) = server.poll_event() {
        assert!(
            !matches!(e, SessionEvent::CloseSession(_)),
            "クレジット回復後の REQUEST_UPDATE でセッションが閉じてはいけない: {e:?}"
        );
    }
    assert!(
        server
            .subscription(rid)
            .expect("subscription が存在する")
            .pending_update_params
            .is_some(),
        "受理された REQUEST_UPDATE が pending_update_params に反映されること"
    );
}

/// 拒否を繰り返しても上限がちょうど 1 のまま保たれる
///
/// draft-ietf-moq-transport-21 §9.1.7 の回復は「加算した分を戻す」ものであり、
/// 拒否のたびに +1 / -1 が釣り合う。釣り合いが崩れると上限判定が緩むか、
/// 逆に正当な REQUEST_UPDATE を拒否してしまう。
#[test]
fn repeated_protocol_level_errors_keep_update_limit() {
    let (_client, mut server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x08, 1));

    // 加算 (+1) → 拒否による回復 (-1) を 2 巡させる
    for _ in 0..2 {
        server
            .recv_stream_message(rid, inject_request_update(rid, one_range_subgroup_filter()))
            .expect("INVALID_FILTER 拒否は Result::Ok (セッションは維持)");
        assert_invalid_filter_rejection(&mut server, rid);
    }

    // 上限 1 が保たれていれば 1 通目だけが受理される
    server
        .recv_stream_message(rid, inject_request_update(rid, MessageParameters::new()))
        .expect("1 通目は上限内");
    let err = server
        .recv_stream_message(rid, inject_request_update(rid, MessageParameters::new()))
        .expect_err("応答を返していないので 2 通目は上限超過");
    assert_eq!(err.code, SESSION_TOO_MANY_REQUEST_UPDATES);
}

/// 1 つの REQUEST_UPDATE に応答を 2 回送っても outstanding が 0 を割り込まない
///
/// draft-ietf-moq-transport-21 §9.1.7 の回復は加算分を戻すものなので、
/// outstanding 0 の状態でさらに減算してはならない。outstanding のエントリは
/// 応答後も 0 の値で残るため、同一 request_id に REQUEST_OK と REQUEST_ERROR を
/// 続けて送ると 2 回目の回復が 0 に対して走る。減算が符号なし整数を回り込むと
/// 上限判定が事実上無効になる。
#[test]
fn credit_restore_does_not_underflow_on_second_response() {
    let (mut client, mut server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x08, 1));

    client
        .send_request_update(rid, MessageParameters::new())
        .expect("REQUEST_UPDATE の送信に成功すること");
    let (_, upd) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd)
        .expect("REQUEST_UPDATE の受信に成功すること");

    // 1 回目の応答で outstanding が 1 → 0 になる
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("REQUEST_OK の送信に成功すること");
    // 2 回目の応答。回復は 0 に対して走るため、床を守っていないと減算が回り込む
    server
        .send_request_error(
            rid,
            shiguredo_moqt::error::REQUEST_INTERNAL_ERROR,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("rejected")
                .expect("1024 バイト以下の reason は常に有効"),
            None,
        )
        .expect("REQUEST_ERROR の送信に成功すること");

    // 回り込んでいれば上限判定が無効化され、注入した 2 通が両方受理されてしまう。
    // subscription は Terminated なので受理自体が起きないことも同時に確認する。
    let err = server
        .recv_stream_message(rid, inject_request_update(rid, MessageParameters::new()))
        .expect_err("Terminated な subscription への REQUEST_UPDATE は拒否される");
    assert_ne!(
        err.code, SESSION_TOO_MANY_REQUEST_UPDATES,
        "上限超過ではなく状態違反で拒否されること"
    );
}

// ─── MAX_FILTER_RANGES=0 と Range を持たない Range Filter (§9.1.6 / §3.3.2) ────

/// Range を 1 つも持たない Range Filter の 3 形態を返す
///
/// `count_range_filters()` はいずれも 0 を返すため、上限判定を Range 総数だけで
/// 行うと MAX_FILTER_RANGES=0 でも通過してしまう入力群。
///
/// - Length=0: REQUEST_UPDATE でのフィルタ削除指示
/// - SetID のみ: 0x25-0x27 の最小ペイロード
/// - SetID + Property Type のみ: 0x28 / 0x29 の最小ペイロード (Property Type は偶数でなければならない)
fn zero_range_filter_forms() -> [(&'static str, MessageParameters); 3] {
    let mut length_zero = MessageParameters::new();
    length_zero.push(MessageParameter {
        param_type: PARAM_SUBGROUP_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![]),
    });
    let mut set_id_only = MessageParameters::new();
    set_id_only.push(MessageParameter {
        param_type: PARAM_SUBGROUP_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x00]),
    });
    let mut set_id_and_property = MessageParameters::new();
    set_id_and_property.push(MessageParameter {
        param_type: PARAM_OBJECT_PROPERTY_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x00, 0x02]),
    });
    [
        ("Length=0", length_zero),
        ("SetID のみ", set_id_only),
        ("SetID + Property Type のみ", set_id_and_property),
    ]
}

/// peer が MAX_FILTER_RANGES を宣言していなければ Range を持たない Range Filter も送出できない
///
/// draft-ietf-moq-transport-21 §9.1.6: "The default value is 0, so if not specified,
/// the peer MUST NOT send any such filter parameters."
/// 条件は "such filter parameters" であり Range 数ではないため、Range 総数 0 でも送れない。
#[test]
fn outgoing_zero_range_filters_rejected_when_peer_max_is_zero() {
    for (label, params) in zero_range_filter_forms() {
        let (mut client, _server, rid) =
            establish_subscribe_with(SetupOptions::new(), SetupOptions::new());
        assert_eq!(
            params.count_range_filters(),
            0,
            "{label}: Range 総数は 0 でなければならない"
        );

        let err = client
            .send_request_update(rid, params)
            .expect_err("peer MAX 未宣言では Range Filter を送出できない");
        assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION, "{label}");
        // 上限超過 (Range 総数 0 なので発火しない) や重複検証と区別する
        assert_eq!(
            err.reason, "peer did not declare MAX_FILTER_RANGES",
            "{label}: 宣言なしによる拒否でなければならない"
        );
        assert_no_send_on_stream(&mut client);
        // 送信拒否はローカルな判断であり、セッションを閉じてはいけない
        while let Some(e) = client.poll_event() {
            assert!(
                !matches!(e, SessionEvent::CloseSession(_)),
                "{label}: 送信拒否でセッションが閉じてはいけない: {e:?}"
            );
        }
    }
}

/// peer が MAX_FILTER_RANGES=1 を宣言していれば Range を持たない Range Filter は送出できる
///
/// §9.1.6 の MUST NOT は「宣言が無い場合」の規定なので、宣言があれば Range 総数 0 は
/// 上限判定 (0 > 1) を通過して送出できる。境界の確認。
#[test]
fn outgoing_zero_range_filters_allowed_when_peer_max_declared() {
    for (label, params) in zero_range_filter_forms() {
        let (mut client, _server, rid) =
            establish_subscribe_with(SetupOptions::new(), opts_with(0x06, 1));

        client
            .send_request_update(rid, params)
            .unwrap_or_else(|e| panic!("{label}: peer MAX=1 なら送出できること: {e:?}"));
        let (stream_rid, msg) = take_send_on_stream(&mut client);
        assert_eq!(stream_rid, rid, "{label}");
        assert!(
            matches!(msg, ControlMessage::RequestUpdate(_)),
            "{label}: RequestUpdate として積まれること: {msg:?}"
        );
    }
}

/// 受信側は MAX_FILTER_RANGES=0 でも Range を持たない Range Filter を受理する
///
/// draft-ietf-moq-transport-21 §3.3.2 の MUST reject は「上限超過」しか条件にしていないため、
/// Range 総数 0 を拒否する規範が無い。MUST で要求されていない拒否で peer のリクエストを
/// 失敗させない、という送信保守・受信寛容の判断を固定する。
#[test]
fn incoming_zero_range_filters_accepted_when_local_max_is_zero() {
    for (label, params) in zero_range_filter_forms() {
        let (_client, mut server, rid) =
            establish_subscribe_with(SetupOptions::new(), SetupOptions::new());

        server
            .recv_stream_message(rid, inject_request_update(rid, params))
            .unwrap_or_else(|e| panic!("{label}: 受理されること: {e:?}"));

        while let Some(e) = server.poll_event() {
            match e {
                SessionEvent::SendOnStream {
                    message: ControlMessage::RequestError(err),
                    ..
                } => panic!(
                    "{label}: REQUEST_ERROR を返してはいけない: error_code={:#x}",
                    err.error_code
                ),
                SessionEvent::CloseSession(err) => {
                    panic!("{label}: セッションを閉じてはいけない: {err:?}")
                }
                _ => {}
            }
        }
    }
}

/// 内部構造がパース不能な Range Filter は INVALID_FILTER で拒否される
///
/// `0xC0` は leading-ones 方式で 3 バイト varint の先頭バイトなので、後続 2 バイトが
/// 足りず decode に失敗する。この入力は Range 総数を数えられないため 0 に畳まれる。
/// 上限判定を Range 総数だけで門番していると検証に到達できず、不正バイト列がそのまま
/// アプリケーションへ渡ってしまう。門番がパラメータの有無であることを固定する。
#[test]
fn incoming_unparsable_range_filter_is_rejected() {
    let (_client, mut server) =
        establish_pair_with_options(SetupOptions::new(), SetupOptions::new());

    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_SUBGROUP_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x00, 0xC0]),
    });
    assert_eq!(
        params.count_range_filters(),
        0,
        "パース不能な入力は Range 総数を数えられず 0 に畳まれる"
    );

    let subscribe = ControlMessage::Subscribe(shiguredo_moqt::message::Subscribe {
        request_id: 0,
        track_namespace: ns(&[b"live"]),
        track_name: b"cam".to_vec(),
        parameters: params,
    });
    server
        .recv_request(subscribe)
        .expect("INVALID_FILTER 拒否は Result::Ok (セッションは維持)");

    let mut request_errors = 0;
    while let Some(e) = server.poll_event() {
        match e {
            SessionEvent::SendOnStream {
                message: ControlMessage::RequestError(err),
                ..
            } => {
                assert_eq!(err.error_code, REQUEST_INVALID_FILTER);
                request_errors += 1;
            }
            SessionEvent::CloseSession(err) => {
                panic!("INVALID_FILTER でセッションが閉じてはいけない: {err:?}")
            }
            _ => {}
        }
    }
    assert_eq!(
        request_errors, 1,
        "INVALID_FILTER の REQUEST_ERROR がちょうど 1 件送出されること"
    );
}

// ─── MAX_FILTER_RANGES の subscription 単位累積 (§3.3.2 / §9.1.6) ─────────

/// Range 1 個の OBJECTID_FILTER (SetID=0, Start=0, End_delta=1)
fn one_range_objectid_filter() -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_OBJECTID_FILTER,
        value: MessageParameterValue::LengthPrefixed(vec![0x00, 0x00, 0x01]),
    });
    params
}

/// 型をまたぐ Range Filter の累積が自側 MAX_FILTER_RANGES を超えたら拒否される
///
/// draft-ietf-moq-transport-21 §9.1.6: 上限は "the peer's total number of Ranges
/// (Start/End pairs) allowed concurrently in all Range filter parameters for a given
/// subscription or fetch" であり、1 メッセージ単位ではなく subscription 単位の同時保持数。
///
/// `merge_from` は Range Filter を型単位で全置換するため同一型では累積しないが、
/// 型をまたぐと `pending_update_params` に累積する。1 通ずつ見れば上限内なので、
/// マージ後の再検証が無いと宣言した上限を超えたフィルタ状態を保持させられる。
#[test]
fn cumulative_cross_type_range_filters_exceed_local_max() {
    let (_client, mut server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x06, 1));

    // 1 通目: SUBGROUP_FILTER 1 Range → 単体でも累積でも上限内
    server
        .recv_stream_message(rid, inject_request_update(rid, one_range_subgroup_filter()))
        .expect("1 通目は上限内");
    let pending = server
        .subscription(rid)
        .expect("subscription が存在する")
        .pending_update_params
        .as_ref()
        .expect("1 通目が累積されること");
    assert_eq!(pending.count_range_filters(), 1);
    while server.poll_event().is_some() {}

    // 2 通目: OBJECTID_FILTER 1 Range → 単体では上限内だが累積 2 で超過
    server
        .recv_stream_message(rid, inject_request_update(rid, one_range_objectid_filter()))
        .expect("累積超過は INVALID_FILTER 拒否なので Result::Ok (セッションは維持)");

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
            SessionEvent::CloseSession(err) => {
                panic!("累積超過でセッションを閉じてはいけない: {err:?}")
            }
            SessionEvent::RequestUpdateReceived { .. } => {
                panic!("累積超過では RequestUpdateReceived を発行してはいけない")
            }
            _ => {}
        }
    }
    assert!(
        saw_invalid_filter,
        "累積超過は INVALID_FILTER の REQUEST_ERROR で拒否されること"
    );
}

/// 累積超過で拒否した場合、pending_update_params が 1 通目の状態のまま巻き戻ること
///
/// 上限違反で拒否したのに累積側が更新されていると、以降の正当な REQUEST_UPDATE も
/// 超過状態のまま評価され続けてしまう。
#[test]
fn cumulative_overflow_does_not_mutate_pending_params() {
    let (_client, mut server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x06, 1));

    server
        .recv_stream_message(rid, inject_request_update(rid, one_range_subgroup_filter()))
        .expect("1 通目は上限内");
    while server.poll_event().is_some() {}

    server
        .recv_stream_message(rid, inject_request_update(rid, one_range_objectid_filter()))
        .expect("累積超過は INVALID_FILTER 拒否なので Result::Ok");
    while server.poll_event().is_some() {}

    let pending = server
        .subscription(rid)
        .expect("subscription が存在する")
        .pending_update_params
        .as_ref()
        .expect("1 通目の累積は残ること");
    assert_eq!(
        pending.count_range_filters(),
        1,
        "拒否した 2 通目の Range が累積に残ってはいけない"
    );
    assert!(
        pending.range_filters(PARAM_OBJECTID_FILTER).is_empty(),
        "拒否した OBJECTID_FILTER が累積に混ざってはいけない"
    );
    assert_eq!(
        pending.range_filters(PARAM_SUBGROUP_FILTER).len(),
        1,
        "1 通目の SUBGROUP_FILTER はそのまま残ること"
    );

    // 巻き戻っているので、上限内の正当な更新は引き続き受理される
    server
        .recv_stream_message(rid, inject_request_update(rid, one_range_subgroup_filter()))
        .expect("巻き戻っていれば同一型の置換は引き続き受理される");
}

/// 同一型の置換は累積扱いにならず、上限内なら何度でも更新できる
///
/// `merge_from` の型単位全置換により、同じ型の Range Filter は上書きされる。
#[test]
fn cumulative_same_type_replacement_does_not_accumulate() {
    let (_client, mut server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x06, 1));

    for i in 0..3 {
        server
            .recv_stream_message(rid, inject_request_update(rid, one_range_subgroup_filter()))
            .unwrap_or_else(|e| panic!("{i} 回目の同一型置換は受理されること: {e:?}"));
        let pending = server
            .subscription(rid)
            .expect("subscription が存在する")
            .pending_update_params
            .as_ref()
            .expect("累積が存在すること");
        assert_eq!(
            pending.count_range_filters(),
            1,
            "{i} 回目: 同一型は置換されるので Range 総数は 1 のまま"
        );
        while server.poll_event().is_some() {}
    }
}

/// 累積がちょうど上限に一致するケースは受理される (境界)
#[test]
fn cumulative_range_filters_at_local_max_accepted() {
    let (_client, mut server, rid) =
        establish_subscribe_with(SetupOptions::new(), opts_with(0x06, 2));

    server
        .recv_stream_message(rid, inject_request_update(rid, one_range_subgroup_filter()))
        .expect("1 通目は上限内");
    while server.poll_event().is_some() {}
    server
        .recv_stream_message(rid, inject_request_update(rid, one_range_objectid_filter()))
        .expect("累積 2 は MAX_FILTER_RANGES=2 と一致するので受理される");

    let mut saw_update = false;
    while let Some(e) = server.poll_event() {
        match e {
            SessionEvent::SendOnStream {
                message: ControlMessage::RequestError(err),
                ..
            } => panic!(
                "境界一致で拒否してはいけない: error_code={:#x}",
                err.error_code
            ),
            SessionEvent::RequestUpdateReceived { parameters, .. } => {
                assert_eq!(parameters.count_range_filters(), 2, "累積 2 が渡されること");
                saw_update = true;
            }
            _ => {}
        }
    }
    assert!(saw_update, "RequestUpdateReceived が発行されること");

    let pending = server
        .subscription(rid)
        .expect("subscription が存在する")
        .pending_update_params
        .as_ref()
        .expect("累積が存在すること");
    assert_eq!(pending.count_range_filters(), 2);
}

// ─── GROUP_ORDER 値域検証 (draft §9.20.9) ──────────────────────────────────

/// GROUP_ORDER 値域外の SUBSCRIBE 受信は PROTOCOL_VIOLATION でセッションを閉じる
///
/// draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): 値域外の値は
/// "MUST close the session with PROTOCOL_VIOLATION"。ワイヤデコード層は値域外を
/// 弾くため、セッション層に到達するのはアプリが MessageParameters を直接構築して
/// 渡した場合のみ。
#[test]
fn subscribe_with_invalid_group_order_closes_session() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_GROUP_ORDER,
    };
    let (_client, mut server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(3), // 値域外 (Ascending=0x1 / Descending=0x2)
    });
    let err = server
        .recv_request(ControlMessage::Subscribe(
            shiguredo_moqt::message::Subscribe {
                track_namespace: ns(&[b"live"]),
                track_name: b"cam".to_vec(),
                request_id: 0,
                parameters: params,
            },
        ))
        .expect_err("値域外 GROUP_ORDER は PROTOCOL_VIOLATION になる");
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    match drain_until_close(&mut server) {
        SessionEvent::CloseSession(e) => assert_eq!(e.code, SESSION_PROTOCOL_VIOLATION),
        other => panic!("CloseSession(PROTOCOL_VIOLATION) が期待されたが {other:?}"),
    }
}

/// 値域外 GROUP_ORDER を含む send_subscribe はエラーを返し、セッションは閉じない
///
/// draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): MUST クローズは
/// 受信側の義務であり、送信側の検証はエラーを返すだけ。検証失敗後も正常な送信が
/// 継続できること (状態非汚染) を確認する。
#[test]
fn send_subscribe_with_invalid_group_order_returns_error_without_closing() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_GROUP_ORDER,
    };
    let (mut client, _server) = establish_pair();
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_GROUP_ORDER,
        value: MessageParameterValue::Uint8(3), // 値域外 (Ascending=0x1 / Descending=0x2)
    });
    let err = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), params)
        .unwrap_err();
    assert_eq!(
        err.as_session_error()
            .expect("Session エラーであること")
            .code,
        SESSION_PROTOCOL_VIOLATION
    );
    // セッションは閉じない (CloseSession イベントが発行されない)
    while let Some(e) = client.poll_event() {
        assert!(
            !matches!(e, SessionEvent::CloseSession(_)),
            "送信側の検証エラーでセッションは閉じないこと"
        );
    }
    // 状態非汚染: 検証エラー後も正常な SUBSCRIBE が送信できる
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("検証エラー後も正常な送信ができること");
    let (sent_rid, _) = take_send_request(&mut client);
    assert_eq!(sent_rid, rid);
}
