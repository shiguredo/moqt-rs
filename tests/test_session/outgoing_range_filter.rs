//! 送信経路の Range Filter 検証テスト
//!
//! draft-ietf-moq-transport-21 §3.3.2 (Range Filters) は同一 (Parameter Type, SetID,
//! Property Type) の重複に対して受信側の MUST 拒否 (REQUEST_ERROR / INVALID_FILTER) を
//! 規定する。§9.1.6 は peer が MAX_FILTER_RANGES を宣言していない場合の MUST NOT を
//! 規定する。どちらも「peer が必ず拒否するメッセージ」なので、Range Filter を載せられる
//! 送信 API はすべて送信前に自側で弾かなければならない。
//!
//! 拒否はローカルな判断であり、セッションを閉じてはいけない。
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use super::*;
use shiguredo_moqt::message::common::Location;
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

/// Range Filter 群に FETCH range ({0, 0}-{5, 0}) の LOCATION_FILTER を足す
///
/// draft-ietf-moq-transport-21 §9.11 (FETCH): range は LOCATION_FILTER で指定する。
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

/// SUBSCRIBE_TRACKS は重複 Range Filter を送信前に拒否する
#[test]
fn subscribe_tracks_rejects_duplicate_range_filters() {
    let (mut client, _server) = pair_for_client_send(2);
    let err = client
        .send_subscribe_tracks(ns(&[b"example"]), duplicate_range_filters())
        .expect_err("重複 Range Filter は送信前に拒否される");
    let SendRequestError::Session(err) = err else {
        panic!("SendRequestError::Session が期待されたが {err:?}");
    };
    assert_rejected_without_side_effects(&mut client, err, "send_subscribe_tracks 経路");
}

/// PUBLISH_OK context の REQUEST_OK は重複 Range Filter を送信前に拒否する
///
/// なお draft-ietf-moq-transport-21 Appendix A.1 #1790 以降、PUBLISH_OK は EXPIRES のみを
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

/// peer が MAX_FILTER_RANGES を宣言していなければ 3 経路すべてが送信を拒否する
///
/// draft-ietf-moq-transport-21 §9.1.6 の MUST NOT (peer が MAX_FILTER_RANGES を宣言しない
/// (default 0) 場合、送信側は Range Filter を含む request を送出してはならない)。この判定を
/// 集約する `validate_outgoing_range_filters` の呼び出しが 3 経路 (SUBSCRIBE_TRACKS /
/// FETCH / PUBLISH_OK) すべてに繋がっていることを確認する。
/// (draft-ietf-moq-transport-21 で Joining FETCH は廃止された)
///
/// なお PUBLISH_OK (send_request_ok) への到達確認は本テストの対象外である。
/// draft-ietf-moq-transport-21 Appendix A.1 #1790 以降、PUBLISH_OK は EXPIRES のみを運ぶため、
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

    let (mut client, _server) = establish_pair();
    let err = client
        .send_subscribe_tracks(ns(&[b"example"]), distinct_set_id_range_filters())
        .expect_err("peer MAX 未宣言では送信できない");
    let SendRequestError::Session(err) = err else {
        panic!("SendRequestError::Session が期待されたが {err:?}");
    };
    assert_eq!(
        err.reason, REASON,
        "send_subscribe_tracks の拒否理由が一致すること"
    );

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

/// 正当な複数 SetID の Range Filter は 2 経路で送信でき、PUBLISH_OK では拒否される
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

    let (mut client, _server) = pair_for_client_send(2);
    client
        .send_subscribe_tracks(ns(&[b"example"]), distinct_set_id_range_filters())
        .expect("send_subscribe_tracks: 正当な Range Filter は送信できる");

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
    // draft-ietf-moq-transport-21 Appendix A.1 #1790: PUBLISH_OK は EXPIRES のみを
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
