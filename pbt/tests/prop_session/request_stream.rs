//! bidi request stream 終端の役割分岐のプロパティテスト
//!
//! draft-ietf-moq-transport-21 §6.4.2.2 (Graceful Request Stream Closure) は FIN を方向ごとの
//! 終端として定義し、cancel (§6.4.2.3 (Request Cancellation and Rejection)) と区別する。
//!
//! - 自側が requester のとき peer FIN で request が終端する
//! - 自側が SUBSCRIBE / FETCH の responder のとき peer FIN では終端しない
//! - peer RESET_STREAM (cancel) は役割にかかわらず終端する
//! - PUBLISH 起点の responder が受ける peer FIN は従来どおり終端する
//! - responder の終端は両方向が閉じた時点で確定し、FIN の到着順に依存しない
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use noprop::TestResult;
use pbt::common::test_runner;
use shiguredo_moqt::message::ReasonPhrase;
use shiguredo_moqt::message::common::{Location, TrackNamespace};
use shiguredo_moqt::message_parameter::{
    LocationFilter, MessageParameter, MessageParameterValue, MessageParameters,
    PARAM_LOCATION_FILTER,
};
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::{
    FetchState, RequestStreamEnd, SessionEvent, SubscriptionState, TerminationReason,
};
use shiguredo_moqt::track_properties::TrackProperties;

use super::common::{establish_pair, take_send_on_stream, take_send_request};

/// peer の終端種別のサンプル (FIN と RESET_STREAM)
fn sample_end(ctx: &mut noprop::TestCaseContext) -> RequestStreamEnd {
    if noprop::sample_bool(ctx) {
        RequestStreamEnd::Fin
    } else {
        RequestStreamEnd::Reset {
            error_code: noprop::sample_u64_in(ctx, 0..64),
            reliable_size: None,
        }
    }
}

/// 自側の最終メッセージ送信と peer FIN のどちらが先かを表す
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinOrder {
    /// peer の FIN が先に到着する
    PeerFirst,
    /// 自側が最終メッセージを先に送る
    LocalFirst,
}

/// FIN の到着順のサンプル
fn sample_fin_order(ctx: &mut noprop::TestCaseContext) -> FinOrder {
    if noprop::sample_bool(ctx) {
        FinOrder::PeerFirst
    } else {
        FinOrder::LocalFirst
    }
}

/// `RequestTerminated` の理由を取り出す (発行されなければ `None`)
///
/// `SessionEvent::CloseSession` が発行された場合と、同じ drain で `RequestTerminated` が
/// 二重に発行された場合は panic する (テストが失敗を握り潰さないため)。
fn take_termination_reason(s: &mut Session) -> Option<TerminationReason> {
    let mut reason = None;
    while let Some(e) = s.poll_event() {
        match e {
            SessionEvent::RequestTerminated {
                reason: r,
                kind,
                request_id,
            } => {
                assert!(
                    reason.is_none(),
                    "同一 request の終端で RequestTerminated が二重発行された: kind={kind:?}, request_id={request_id}"
                );
                reason = Some(r);
            }
            SessionEvent::CloseSession(err) => {
                panic!("request stream の終端でセッションが閉じてはいけない: {err:?}")
            }
            _ => {}
        }
    }
    reason
}

/// SUBSCRIBE の responder は peer FIN で subscription を終端しない
///
/// 自側が responder のとき、requester の FIN は「もうメッセージを送らない」ことだけを示す
/// (§6.4.2.2)。RESET_STREAM は cancel なので終端する (§6.4.2.3)。
#[test]
fn subscribe_responder_terminates_only_on_reset() -> TestResult {
    let mut runner = test_runner()?;
    let fin_seen = std::cell::Cell::new(false);
    let reset_seen = std::cell::Cell::new(false);
    let peer_first_seen = std::cell::Cell::new(false);
    let local_first_seen = std::cell::Cell::new(false);
    runner.run(256, |ctx| {
        let end = sample_end(ctx);
        match end {
            RequestStreamEnd::Fin => fin_seen.set(true),
            RequestStreamEnd::Reset { .. } => reset_seen.set(true),
        }
        // FIN の到着順も入れ替える (RESET は即時終端のため順序の次元は無い)
        let order = sample_fin_order(ctx);
        let local_first = order == FinOrder::LocalFirst && matches!(end, RequestStreamEnd::Fin);
        if matches!(end, RequestStreamEnd::Fin) {
            if local_first {
                local_first_seen.set(true);
            } else {
                peer_first_seen.set(true);
            }
        }
        let (mut client, mut server) = establish_pair();
        // client (subscriber) が SUBSCRIBE を送り、server が responder になる
        let rid = client
            .send_subscribe(
                TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                b"cam".to_vec(),
                MessageParameters::new(),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, sub_msg) = take_send_request(&mut client);
        server
            .recv_request(sub_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        // 確定した状態を作る (SUBSCRIBE_OK まで進める)
        server
            .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok_msg) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(rid, ok_msg)
            .expect("テストフィクスチャの前提条件を満たす");

        // 自側の最終メッセージ送信と peer FIN の到着順を入れ替えても終端が確定すること
        if local_first {
            server
                .send_publish_done(
                    rid,
                    0x2,
                    0,
                    ReasonPhrase::new("ended").expect("正当な reason phrase である"),
                )
                .expect("PUBLISH_DONE を送れること");
            assert!(
                take_termination_reason(&mut server).is_none(),
                "自側の送信方向を閉じただけでは終端しない"
            );
        }
        server
            .recv_request_stream_closed(rid, end)
            .expect("requester の終端通知を受理すること");
        let reason = take_termination_reason(&mut server);
        let state = server
            .subscription(rid)
            .expect("responder 側の subscription は追跡され続けること")
            .state;
        if matches!(end, RequestStreamEnd::Fin) {
            // 自側の PUBLISH_DONE は subscription の終端そのものなので、送信済みなら
            // Terminated になる。requester の FIN だけでは終端しないことが要点である。
            let expected_state = if local_first {
                SubscriptionState::Terminated
            } else {
                SubscriptionState::Established
            };
            assert_eq!(
                state, expected_state,
                "requester の FIN は subscription の状態を変えない (終端は PUBLISH_DONE が担う)"
            );
            if local_first {
                assert_eq!(
                    reason,
                    Some(TerminationReason::PeerStreamFin),
                    "自側の FIN が先でも peer FIN の到着で終端が確定する"
                );
            } else {
                assert!(
                    reason.is_none(),
                    "responder は requester の FIN だけでは RequestTerminated を発行しない"
                );
                // 応答経路が塞がれていないことを、最終メッセージの送信で確認する
                server
                    .send_publish_done(
                        rid,
                        0x2,
                        0,
                        ReasonPhrase::new("ended").expect("正当な reason phrase である"),
                    )
                    .expect("requester の FIN 後でも PUBLISH_DONE を送れること");
                assert_eq!(
                    take_termination_reason(&mut server),
                    Some(TerminationReason::PeerStreamFin),
                    "PUBLISH_DONE の送信で peer FIN による終端が確定する"
                );
            }
        } else {
            assert_eq!(
                state,
                SubscriptionState::Terminated,
                "responder も peer の RESET_STREAM (cancel) では終端する"
            );
            assert!(
                matches!(reason, Some(TerminationReason::PeerStreamReset { .. })),
                "RESET_STREAM の終端理由は PeerStreamReset になる"
            );
        }
        Ok(())
    })?;
    assert!(
        fin_seen.get(),
        "FIN 分岐が 1 度も実行されなかった\n{runner}"
    );
    assert!(
        reset_seen.get(),
        "RESET_STREAM 分岐が 1 度も実行されなかった\n{runner}"
    );
    assert!(
        peer_first_seen.get(),
        "peer FIN が先の順序が 1 度も実行されなかった\n{runner}"
    );
    assert!(
        local_first_seen.get(),
        "自側の FIN が先の順序が 1 度も実行されなかった\n{runner}"
    );
    Ok(())
}

/// SUBSCRIBE の requester は peer FIN で request を終端する
///
/// §6.4.2.2: responder の FIN は「応答とそれに続くメッセージを送り終えた」通知であり、
/// requester は送信方向を閉じる SHOULD がある。
#[test]
fn subscribe_requester_terminates_on_peer_fin() -> TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let end = sample_end(ctx);
        let (mut client, mut server) = establish_pair();
        let rid = client
            .send_subscribe(
                TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                b"cam".to_vec(),
                MessageParameters::new(),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, sub_msg) = take_send_request(&mut client);
        server
            .recv_request(sub_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        server
            .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok_msg) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(rid, ok_msg)
            .expect("テストフィクスチャの前提条件を満たす");

        client
            .recv_request_stream_closed(rid, end)
            .expect("responder の終端通知を受理すること");
        assert_eq!(
            client
                .subscription(rid)
                .expect("requester 側の subscription は追跡され続けること")
                .state,
            SubscriptionState::Terminated,
            "requester は peer の終端で subscription を終端する"
        );
        let expected = match end {
            RequestStreamEnd::Fin => TerminationReason::PeerStreamFin,
            RequestStreamEnd::Reset { error_code, .. } => {
                TerminationReason::PeerStreamReset { error_code }
            }
        };
        assert_eq!(
            take_termination_reason(&mut client),
            Some(expected),
            "終端理由が peer の終端種別に対応すること"
        );
        Ok(())
    })?;
    Ok(())
}

/// FETCH の responder は peer FIN で fetch を終端しない
///
/// §3.2.1 (Fetch State Management): "The publisher MUST send exactly one FETCH_OK or
/// REQUEST_ERROR in response to a FETCH." 応答を送る前に終端すると MUST を果たせない。
#[test]
fn fetch_responder_terminates_only_on_reset() -> TestResult {
    let mut runner = test_runner()?;
    let fin_seen = std::cell::Cell::new(false);
    let reset_seen = std::cell::Cell::new(false);
    runner.run(256, |ctx| {
        let end = sample_end(ctx);
        match end {
            RequestStreamEnd::Fin => fin_seen.set(true),
            RequestStreamEnd::Reset { .. } => reset_seen.set(true),
        }
        let (mut client, mut server) = establish_pair();
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LengthPrefixed(
                LocationFilter::AbsoluteRange {
                    start: Location {
                        group_id: 0,
                        object_id: 0,
                    },
                    end_group_delta: 5,
                }
                .encode_to_bytes(),
            ),
        });
        let rid = client
            .send_fetch(
                TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                b"cam".to_vec(),
                params,
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, fetch_msg) = take_send_request(&mut client);
        server
            .recv_request(fetch_msg)
            .expect("テストフィクスチャの前提条件を満たす");

        server
            .recv_request_stream_closed(rid, end)
            .expect("requester の終端通知を受理すること");
        let state = server
            .fetch(rid)
            .expect("responder 側の fetch は追跡され続けること")
            .state;
        if matches!(end, RequestStreamEnd::Fin) {
            assert_eq!(
                state,
                FetchState::Pending,
                "responder は requester の FIN で fetch を終端しない"
            );
            // 応答経路が塞がれていないことを FETCH_OK の送信で確認する
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
                .expect("requester の FIN 後でも FETCH_OK を送れること");
        } else {
            assert_eq!(
                state,
                FetchState::Terminated,
                "responder も peer の RESET_STREAM (cancel) では終端する"
            );
        }
        Ok(())
    })?;
    assert!(
        fin_seen.get(),
        "FIN 分岐が 1 度も実行されなかった\n{runner}"
    );
    assert!(
        reset_seen.get(),
        "RESET_STREAM 分岐が 1 度も実行されなかった\n{runner}"
    );
    Ok(())
}

/// PUBLISH 起点の subscription では peer FIN の扱いが従来どおりである
///
/// §6.4.2.2 は PUBLISH の送信者を「メッセージ送信直後の FIN」の例外とする。したがって
/// PUBLISH を受けた側 (responder) が受け取る peer FIN は PUBLISH_DONE 受信後の完了通知で
/// あり、従来どおり終端する (requester 側の組合せは本 API の未対応範囲)。
#[test]
fn publish_responder_terminates_on_peer_fin() -> TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let end = sample_end(ctx);
        let (mut client, mut server) = establish_pair();
        // server が PUBLISH を送り、client が responder (subscriber 役) になる
        let rid = server
            .send_publish(
                TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                b"cam".to_vec(),
                7,
                MessageParameters::new(),
                TrackProperties::new(),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, pub_msg) = take_send_request(&mut server);
        client
            .recv_request(pub_msg)
            .expect("テストフィクスチャの前提条件を満たす");

        client
            .recv_request_stream_closed(rid, end)
            .expect("requester の終端通知を受理すること");
        assert_eq!(
            client
                .subscription(rid)
                .expect("responder 側の subscription は追跡され続けること")
                .state,
            SubscriptionState::Terminated,
            "PUBLISH 起点の responder は peer の終端で subscription を終端する"
        );
        let expected = match end {
            RequestStreamEnd::Fin => TerminationReason::PeerStreamFin,
            RequestStreamEnd::Reset { error_code, .. } => {
                TerminationReason::PeerStreamReset { error_code }
            }
        };
        assert_eq!(
            take_termination_reason(&mut client),
            Some(expected),
            "終端理由が peer の終端種別に対応すること"
        );
        Ok(())
    })?;
    Ok(())
}
