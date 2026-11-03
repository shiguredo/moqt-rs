//! 送信経路の Range Filter 検証テスト
//!
//! draft-ietf-moq-transport-22 §3.3.2 (Range Filters) は同一 (Parameter Type, SetID,
//! Property Type) の重複に対して受信側の MUST 拒否 (REQUEST_ERROR / INVALID_FILTER) を
//! 規定する。§9.1.6 は peer が MAX_FILTER_RANGES を宣言していない場合の MUST NOT を
//! 規定する。どちらも「peer が必ず拒否するメッセージ」なので、Range Filter を載せられる
//! 送信 API はすべて送信前に自側で弾かなければならない。
//!
//! PUBLISH は外側の Parameters に Range Filter を持てない (draft-ietf-moq-transport-23
//! §9.8 (PUBLISH) の列挙) が、FILL_PARAMETERS の内側には置ける (§9.19.15 (FILL PARAMETERS
//! Parameter) の Table 7)。§9.1.6 (MAX FILTER RANGES) の総数は "all Range filter ...
//! parameters for a given subscription or fetch" であるため、内側の Range も同じ予算に
//! 含まれる。したがって PUBLISH の送信経路も FILL 内側の Range Filter を送信前に検証する。
//!
//! 拒否はローカルな判断であり、セッションを閉じてはいけない。
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use super::*;
use shiguredo_moqt::message::common::Location;
use shiguredo_moqt::message_parameter::{MessageParameter, PARAM_FILL_PARAMETERS};
use shiguredo_moqt::parameter::SETUP_OPTION_MAX_FILTER_RANGES;
use shiguredo_moqt::session::types::{SendRequestError, SessionError};

/// 同一 (Parameter Type, SetID) が重複した Range Filter (§3.3.2 違反)
fn duplicate_range_filters() -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(one_range_subgroup_filter_with_set_id(0));
    params.push(one_range_subgroup_filter_with_set_id(0));
    params
}

/// SetID が異なる正当な Range Filter 2 本 (Range 総数 2)
fn distinct_set_id_range_filters() -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(one_range_subgroup_filter_with_set_id(0));
    params.push(one_range_subgroup_filter_with_set_id(1));
    params
}

/// FILL_PARAMETERS の内側に Range Filter を置いた PUBLISH 用パラメータを作る
///
/// Range Filter は PUBLISH の外側スコープには現れない (§9.8 (PUBLISH)) ため、
/// 検証対象は FILL の内側だけになる。
fn fill_params_with_inner(filters: MessageParameters) -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FILL_PARAMETERS,
        value: MessageParameterValue::FillParameters(filters),
    });
    params
}

/// PUBLISH の送信拒否を検証する
///
/// - 指定の reason で `SESSION_PROTOCOL_VIOLATION` が返る
/// - `SendRequest` / `OpenFillFetchStream` が積まれず、subscription も登録されない
/// - `CloseSession` が発行されない (送信拒否はローカルな判断である)
fn assert_publish_rejected(session: &mut Session, err: SendRequestError, reason: &str) {
    let SendRequestError::Session(err) = err else {
        panic!("SendRequestError::Session が期待されたが {err:?}");
    };
    assert_eq!(
        err.code, SESSION_PROTOCOL_VIOLATION,
        "PUBLISH の送信拒否で SESSION_PROTOCOL_VIOLATION が返ること"
    );
    assert_eq!(
        err.reason, reason,
        "PUBLISH の送信拒否の理由が期待どおりであること"
    );
    assert_eq!(
        session.subscriptions().count(),
        0,
        "拒否した PUBLISH の subscription を登録しないこと"
    );
    while let Some(e) = session.poll_event() {
        match e {
            SessionEvent::SendRequest { .. } | SessionEvent::OpenFillFetchStream { .. } => {
                panic!("拒否した PUBLISH の送信・開設指示を積まないこと: {e:?}")
            }
            SessionEvent::CloseSession(e) => {
                panic!("送信拒否でセッションが閉じてはいけない: {e:?}")
            }
            _ => {}
        }
    }
}

/// Range Filter 群に FETCH range ({0, 0}-{5, 0}) の LOCATION_FILTER を足す
///
/// draft-ietf-moq-transport-22 §9.11 (FETCH): range は LOCATION_FILTER で指定する。
/// range 構築は共有ヘルパー `fetch_range_params` に委譲する。
fn with_fetch_range(mut params: MessageParameters) -> MessageParameters {
    for p in fetch_range_params(
        Location {
            group_id: 0,
            object_id: 0,
        },
        Location {
            group_id: 5,
            object_id: 0,
        },
    )
    .as_slice()
    {
        params.push(p.clone());
    }
    params
}

/// 送信拒否の副作用を検証する
///
/// - `SESSION_PROTOCOL_VIOLATION` が返る
/// - reason が重複検証由来である (上限超過や宣言なしと区別する)
/// - `SendOnStream` / `SendRequest` が積まれない
/// - `CloseSession` が発行されない
fn assert_rejected_without_side_effects(session: &mut Session, err: SessionError, label: &str) {
    assert_eq!(
        err.code, SESSION_PROTOCOL_VIOLATION,
        "{label} で SESSION_PROTOCOL_VIOLATION が返ること"
    );
    assert_eq!(
        err.reason, "outgoing Range Filters are invalid",
        "{label}: 重複検証で拒否されなければならない"
    );
    while let Some(e) = session.poll_event() {
        match e {
            SessionEvent::SendOnStream { message, .. } => {
                panic!("{label}: 拒否したのに SendOnStream が積まれている: {message:?}")
            }
            SessionEvent::SendRequest { message, .. } => {
                panic!("{label}: 拒否したのに SendRequest が積まれている: {message:?}")
            }
            SessionEvent::CloseSession(e) => {
                panic!("{label}: 送信拒否でセッションが閉じてはいけない: {e:?}")
            }
            _ => {}
        }
    }
}

/// client から見た peer (server) が MAX_FILTER_RANGES を宣言した状態のペアを返す
///
/// 送信側の検証は **peer** の宣言を見るため、client が送る経路では server 側に宣言させる。
fn pair_for_client_send(max: u64) -> (Session, Session) {
    establish_pair_with_options(SetupOptions::new(), opts_with(0x06, max))
}

/// server から見た peer (client) が MAX_FILTER_RANGES を宣言した状態のペアを返す
///
/// PUBLISH_OK は responder である server が送るため、宣言側が client になる。
fn pair_for_server_send(max: u64) -> (Session, Session) {
    establish_pair_with_options(opts_with(0x06, max), SetupOptions::new())
}

/// FETCH は重複 Range Filter を送信前に拒否する
#[test]
fn fetch_rejects_duplicate_range_filters() {
    let (mut client, _server) = pair_for_client_send(2);
    let err = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            with_fetch_range(duplicate_range_filters()),
        )
        .expect_err("重複 Range Filter は送信前に拒否される");
    let SendRequestError::Session(err) = err else {
        panic!("SendRequestError::Session が期待されたが {err:?}");
    };
    assert_rejected_without_side_effects(&mut client, err, "send_fetch 経路");
}

/// PUBLISH_OK context の REQUEST_OK は重複 Range Filter を送信前に拒否する
///
/// なお draft-ietf-moq-transport-22 Appendix A.3 #1790 以降、PUBLISH_OK は EXPIRES のみを
/// 運ぶため、正当な Range Filter であっても後段のスコープ検証で拒否される。
/// 本テストは前段の outgoing 検証 (重複検出) が先に発火することを確認する。
#[test]
fn publish_ok_rejects_duplicate_range_filters() {
    let (mut client, mut server) = pair_for_server_send(2);
    // publisher (client) の PUBLISH に subscriber (server) が PUBLISH_OK で応答する形を作る
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
    while server.poll_event().is_some() {}

    let err = server
        .send_request_ok(rid, duplicate_range_filters(), TrackProperties::default())
        .expect_err("重複 Range Filter は送信前に拒否される");
    assert_rejected_without_side_effects(&mut server, err, "send_request_ok (publish_ok) 経路");
}

/// peer が MAX_FILTER_RANGES を宣言していなければ 2 経路すべてが送信を拒否する
///
/// draft-ietf-moq-transport-22 §9.1.6 の MUST NOT (peer が MAX_FILTER_RANGES を宣言しない
/// (default 0) 場合、送信側は Range Filter を含む request を送出してはならない)。この判定を
/// 集約する `validate_outgoing_range_filters` の呼び出しが 2 経路 (FETCH / PUBLISH_OK)
/// すべてに繋がっていることを確認する。
/// (draft-20 で Joining FETCH は廃止された)
///
/// なお PUBLISH_OK (send_request_ok) への到達確認は本テストの対象外である。
/// draft-ietf-moq-transport-22 Appendix A.3 #1790 以降、PUBLISH_OK は EXPIRES のみを運ぶため、
/// Range Filter を含む PUBLISH_OK は後段のスコープ検証でも拒否される。
/// 本テストは前段の outgoing 検証 (MAX 未宣言検出) が先に発火することを確認する。
#[test]
fn all_send_paths_reject_when_peer_max_is_zero() {
    const REASON: &str = "peer did not declare MAX_FILTER_RANGES";

    let (mut client, _server) = establish_pair();
    let err = client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            with_fetch_range(distinct_set_id_range_filters()),
        )
        .expect_err("peer MAX 未宣言では送信できない");
    let SendRequestError::Session(err) = err else {
        panic!("SendRequestError::Session が期待されたが {err:?}");
    };
    assert_eq!(err.reason, REASON, "send_fetch の拒否理由が一致すること");

    let (mut client, mut server) = establish_pair();
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
    let err = server
        .send_request_ok(
            rid,
            distinct_set_id_range_filters(),
            TrackProperties::default(),
        )
        .expect_err("peer MAX 未宣言では送信できない");
    assert_eq!(
        err.reason, REASON,
        "send_request_ok (publish_ok) の拒否理由が一致すること"
    );
}

/// 正当な複数 SetID の Range Filter は FETCH で送信でき、PUBLISH_OK では拒否される
///
/// 検証追加で正当なメッセージまで弾いていないことを確認する。
/// PUBLISH_OK は draft-20 で EXPIRES のみとなり Range Filter を運べないため、
/// 送信側で拒否されることも併せて検証する。
#[test]
fn send_paths_accept_distinct_set_id_range_filters_and_publish_ok_rejects() {
    let (mut client, _server) = pair_for_client_send(2);
    client
        .send_fetch(
            ns(&[b"live"]),
            b"cam".to_vec(),
            with_fetch_range(distinct_set_id_range_filters()),
        )
        .expect("send_fetch: 正当な Range Filter は送信できる");

    let (mut client, mut server) = pair_for_server_send(2);
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
    // draft-ietf-moq-transport-22 Appendix A.3 #1790: PUBLISH_OK は EXPIRES のみを
    // 運ぶため、Range Filter を含む PUBLISH_OK は送信側で拒否される
    let err = server
        .send_request_ok(
            rid,
            distinct_set_id_range_filters(),
            TrackProperties::default(),
        )
        .expect_err("send_request_ok (publish_ok): Range Filter は送信できない");
    assert_eq!(
        err.code, SESSION_PROTOCOL_VIOLATION,
        "send_request_ok (publish_ok) で Range Filter が拒否されること"
    );
}

/// PUBLISH は FILL 内側の Range Filter を peer の MAX 未宣言で送信しない
///
/// draft-ietf-moq-transport-23 §9.1.6 (MAX FILTER RANGES): "The default value is 0, so if
/// not specified, the peer MUST NOT send any such filter parameters." PUBLISH も
/// §9.19.15 (FILL PARAMETERS Parameter) で FILL 内側に Range Filter を運べるため、
/// SUBSCRIBE / REQUEST_UPDATE と同じく request_id 発行前に拒否する。
#[test]
fn publish_fill_rejects_range_filters_when_peer_max_is_zero() {
    const REASON: &str = "peer did not declare MAX_FILTER_RANGES";

    let (_client, mut server) = establish_pair();
    let err = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            111,
            fill_params_with_inner(duplicate_range_filters()),
            TrackProperties::new(),
        )
        .expect_err("peer が MAX_FILTER_RANGES を宣言していなければ送信できない");
    assert_publish_rejected(&mut server, err, REASON);
}

/// PUBLISH は FILL 内側の Range 総数が peer の MAX を超えると送信しない
///
/// draft-ietf-moq-transport-23 §9.1.6 (MAX FILTER RANGES) の総数は "all Range filter ...
/// parameters for a given subscription or fetch" であり、FILL 内側も含む。SETUP で
/// 宣言された上限を超える送出は peer に INVALID_FILTER の拒否 MUST を強いるため、
/// 送信前に自側で弾く。
#[test]
fn publish_fill_rejects_range_filters_exceeding_peer_max() {
    const REASON: &str = "outgoing Range Filters exceed peer MAX_FILTER_RANGES";

    // server から見た peer (client) が MAX_FILTER_RANGES=1 を宣言する
    let (_client, mut server) = establish_pair_with_options(
        opts_with(SETUP_OPTION_MAX_FILTER_RANGES, 1),
        SetupOptions::new(),
    );
    let err = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            111,
            fill_params_with_inner(distinct_set_id_range_filters()),
            TrackProperties::new(),
        )
        .expect_err("Range 総数 2 は peer の上限 1 を超えるため送信できない");
    assert_publish_rejected(&mut server, err, REASON);
}

/// PUBLISH は FILL 内側の重複 Range Filter を送信しない
///
/// draft-ietf-moq-transport-23 §3.3.2 (Range Filters) は同一 (Parameter Type, SetID,
/// Property Type) の重複に対して受信側の MUST 拒否 (INVALID_FILTER) を課す。上限判定に
/// 落ちないよう peer の MAX_FILTER_RANGES=2 に宣言し、重複検出そのものを固定する。
#[test]
fn publish_fill_rejects_duplicate_range_filters() {
    const REASON: &str = "outgoing Range Filters are invalid inside FILL_PARAMETERS";

    let (_client, mut server) = establish_pair_with_options(
        opts_with(SETUP_OPTION_MAX_FILTER_RANGES, 2),
        SetupOptions::new(),
    );
    let err = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            111,
            fill_params_with_inner(duplicate_range_filters()),
            TrackProperties::new(),
        )
        .expect_err("重複した Range Filter は送信前に拒否される");
    assert_publish_rejected(&mut server, err, REASON);
}

/// 上限内で重複の無い FILL 内側 Range Filter は PUBLISH で送信できる
///
/// 送信前検証の追加で正当なメッセージまで弾いていないことを確認する。
#[test]
fn publish_fill_accepts_valid_range_filters() {
    let (_client, mut server) = establish_pair_with_options(
        opts_with(SETUP_OPTION_MAX_FILTER_RANGES, 2),
        SetupOptions::new(),
    );
    let pub_rid = server
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            111,
            fill_params_with_inner(distinct_set_id_range_filters()),
            TrackProperties::new(),
        )
        .expect("上限内で重複の無い Range Filter は送信できる");
    assert!(
        server.subscription(pub_rid).is_some(),
        "送信した PUBLISH の subscription が登録されること"
    );
    let (sent_rid, message) = take_send_request(&mut server);
    assert_eq!(sent_rid, pub_rid, "PUBLISH の Request ID で送ること");
    let ControlMessage::Publish(publish) = message else {
        panic!("PUBLISH が送信されること: {message:?}");
    };
    assert!(
        publish.parameters.fill_parameters().is_some(),
        "FILL_PARAMETERS がそのまま送信されること"
    );
}
