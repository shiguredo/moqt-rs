//! GOAWAY / GOAWAY_TIMEOUT のプロパティテスト
//!
//! - GOAWAY URI の制限内ラウンドトリップ
//! - GOAWAY URI の制限超過拒否
//! - blocker 無し GOAWAY の timeout 非発火
//! - blocker 残り GOAWAY の timeout 発火
//! - request stream 上の GOAWAY の deadline 満了で reset と終端が request ごとに 1 回だけ発行される
//! - DATA_STREAM_TIMEOUT で subgroup stream は STOP_SENDING の指示のみ (セッションは閉じない)
//! - DATA_STREAM_TIMEOUT で FETCH stream は 0x12 でセッションを閉じる

use pbt::common::test_runner;
use shiguredo_moqt::error::{SESSION_DATA_STREAM_TIMEOUT, STREAM_CANCELLED};
use shiguredo_moqt::message::common::TrackNamespace;
use shiguredo_moqt::message_parameter::MessageParameters;
use shiguredo_moqt::session::types::DataStreamId;
use shiguredo_moqt::session::types::MAX_NEW_SESSION_URI_LENGTH;
use shiguredo_moqt::session::types::TerminationReason;
use shiguredo_moqt::stream::FETCH_HEADER_TYPE;
use shiguredo_moqt::{session::types::SessionEvent, session::types::SessionState};

use super::common::{establish_pair, take_send_on_stream, take_send_request};

// Server が 0..=MAX_NEW_SESSION_URI_LENGTH の URI で GOAWAY を送ると client が受信できる
#[test]
fn goaway_uri_within_limit_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let uri_len = noprop::sample_usize_in(ctx, 0..=MAX_NEW_SESSION_URI_LENGTH);
        let timeout = noprop::sample_u64(ctx);
        let (mut client, mut server) = establish_pair();
        let uri = vec![b'a'; uri_len];
        server
            .send_goaway(uri.clone(), timeout)
            .expect("テストフィクスチャの前提条件を満たす");
        let msg = loop {
            match server.poll_event() {
                Some(SessionEvent::SendControl(m)) => break m,
                Some(_) => continue,
                None => panic!("制御メッセージが存在しない"),
            }
        };
        client
            .recv_control(msg)
            .expect("テストフィクスチャの前提条件を満たす");
        // GoawayReceived イベントで受信内容を検証する
        let (uri_got, timeout_got) = loop {
            match client.poll_event() {
                Some(SessionEvent::GoawayReceived {
                    new_session_uri,
                    timeout,
                    ..
                }) => break (new_session_uri, timeout),
                Some(_) => continue,
                None => panic!("GoawayReceived イベントが存在しない"),
            }
        };
        assert_eq!(&uri_got, &uri);
        assert_eq!(timeout_got, timeout);
        Ok(())
    })?;
    Ok(())
}

// MAX_NEW_SESSION_URI_LENGTH を超える URI は送信で拒否される
#[test]
fn goaway_uri_over_limit_errors() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let over = noprop::sample_usize_in(ctx, 1..16);
        let (_, mut server) = establish_pair();
        let uri = vec![b'a'; MAX_NEW_SESSION_URI_LENGTH + over];
        let res = server.send_goaway(uri, 0);
        assert!(res.is_err());
        Ok(())
    })?;
    Ok(())
}

// blocker が無い GOAWAY は timeout を超えても自動 close しない
#[test]
fn goaway_timeout_without_blockers_does_not_close() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let base = noprop::sample_u64_in(ctx, 0..1_000_000);
        let timeout = noprop::sample_u64_in(ctx, 0..10_000);
        let extra = noprop::sample_u64_in(ctx, 0..20_000);
        let (_, mut server) = establish_pair();
        server.tick(base);
        server
            .send_goaway(Vec::new(), timeout)
            .expect("テストフィクスチャの前提条件を満たす");
        let check_at = base + extra;
        server.tick(check_at);
        assert_eq!(server.state(), SessionState::Established);
        Ok(())
    })?;
    Ok(())
}

// blocker が残る GOAWAY は timeout 値と経過時間に応じて CloseSession(GOAWAY_TIMEOUT) が発行される
#[test]
fn goaway_timeout_expires_with_pending_subscription_blocker() -> noprop::TestResult {
    // timeout 発火 (Closing 遷移) と未発火 (Established 維持) の両方の分岐の観測を
    // カバレッジゲートで検証する
    let closed_seen = std::cell::Cell::new(false);
    let open_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        use shiguredo_moqt::error::SESSION_GOAWAY_TIMEOUT;
        let base = noprop::sample_u64_in(ctx, 0..1_000_000);
        let timeout = noprop::sample_u64_in(ctx, 0..10_000);
        let extra = noprop::sample_u64_in(ctx, 0..20_000);
        let (_, mut server) = establish_pair();
        server.tick(base);
        server
            .send_subscribe(
                TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                b"video".to_vec(),
                MessageParameters::new(),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        server
            .send_goaway(Vec::new(), timeout)
            .expect("テストフィクスチャの前提条件を満たす");
        let check_at = base + extra;
        server.tick(check_at);
        let should_close = timeout > 0 && check_at >= base + timeout;
        if should_close {
            assert_eq!(server.state(), SessionState::Closing);
            closed_seen.set(true);
            let mut found = false;
            while let Some(e) = server.poll_event() {
                if let SessionEvent::CloseSession(err) = e {
                    assert_eq!(err.code, SESSION_GOAWAY_TIMEOUT);
                    found = true;
                    break;
                }
            }
            assert!(found);
        } else {
            assert_eq!(server.state(), SessionState::Established);
            open_seen.set(true);
        }
        Ok(())
    })?;
    assert!(
        closed_seen.get(),
        "GOAWAY timeout により CloseSession が発火するケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        open_seen.get(),
        "GOAWAY timeout が発火しないケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

// request stream 上の GOAWAY の deadline 満了で、reset と終端が request ごとに 1 回だけ発行され、
// 期限到達後の tick で再発行されないこと
//
// draft-ietf-moq-transport-22 §9.2 (GOAWAY): "When sent on a request stream, the sender SHOULD
// reset the stream with GOING_AWAY after the indicated timeout." の reset と、それに伴う
// `TerminationReason::GoawayTimeout` の終端は request ごとに 1 回だけであり、セッションは
// 閉じない。時刻と timeout の任意の組合せで成立することを確認する。
#[test]
fn request_stream_goaway_timeout_fires_exactly_once() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let base = noprop::sample_u64_in(ctx, 0..1_000_000);
        let timeout = noprop::sample_u64_in(ctx, 1..10_000);
        let extra = noprop::sample_u64_in(ctx, 0..20_000);
        let (mut client, _server) = establish_pair();
        let rid = client
            .send_subscribe(
                TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                b"video".to_vec(),
                MessageParameters::new(),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (sent_rid, _) = take_send_request(&mut client);
        assert_eq!(sent_rid, rid);
        client.tick(base);
        client
            .send_goaway_on_request_stream(rid, Vec::new(), timeout)
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, _) = take_send_on_stream(&mut client);

        // 期限前後の任意の時刻で tick し、その後で期限超過を確定させる。
        // `tick(now_ms)` の契約は単調増加ミリ秒時刻なので、2 回目は必ず 1 回目以降にする。
        let deadline = base.saturating_add(timeout);
        let first_check = base.saturating_add(extra);
        let second_check = first_check.max(deadline.saturating_add(1));
        let mut resets = 0;
        let mut terminated = 0;
        for check_at in [first_check, second_check] {
            client.tick(check_at);
            while let Some(e) = client.poll_event() {
                match e {
                    SessionEvent::ResetRequestStream { request_id, .. } => {
                        assert_eq!(
                            request_id, rid,
                            "対象 request の reset のみが発行されること"
                        );
                        resets += 1;
                    }
                    SessionEvent::RequestTerminated {
                        request_id, reason, ..
                    } => {
                        assert_eq!(request_id, rid, "対象 request の終端のみが発行されること");
                        assert_eq!(reason, TerminationReason::GoawayTimeout);
                        terminated += 1;
                    }
                    SessionEvent::CloseSession(err) => {
                        panic!(
                            "request stream の deadline 満了でセッションを閉じてはいけない: {err:?}"
                        )
                    }
                    _ => {}
                }
            }
            // 期限前は発火せず、期限到達後は 1 回だけ発行される
            if check_at < deadline {
                assert_eq!(resets, 0, "期限未到達で reset を発行しないこと");
                assert_eq!(terminated, 0, "期限未到達で終端を発行しないこと");
            }
        }
        // 期限超過を確定させた後は reset が 1 回、終端も 1 回である
        assert_eq!(resets, 1, "期限到達で reset が 1 回だけ出ること");
        assert_eq!(terminated, 1, "期限到達で終端が 1 回だけ出ること");

        // 期限到達後に再度 tick しても再発行しない
        // (`tick(now_ms)` の契約は単調増加ミリ秒時刻なので、直前の tick 以降にする)
        client.tick(second_check.max(deadline.saturating_add(1)));
        while let Some(e) = client.poll_event() {
            assert!(
                !matches!(
                    e,
                    SessionEvent::ResetRequestStream { .. }
                        | SessionEvent::RequestTerminated { .. }
                ),
                "期限到達後に reset / 終端を再発行しないこと"
            );
        }
        Ok(())
    })?;
    Ok(())
}

// ============================================================
// DATA_STREAM_TIMEOUT (draft-ietf-moq-transport-22 §12.2 (Session Termination Codes))
// ============================================================
//
// §12.2 は open な subgroup stream で新しい object header を待って timeout した場合の措置を
// "it MAY send a STOP_SENDING on that stream or terminate the subscription" と定め、
// セッション全体のクローズを挙げていない。本実装は subgroup stream ではセッションを閉じずに
// `StopSendingDataStream` を発行し、FETCH stream では §12.2 に代替措置が無いため 0x12 で閉じる。

/// 期限到達した subgroup stream は STOP_SENDING の指示だけを発行し、セッションを閉じない
///
/// 任意の timeout と経過時刻について、期限未満では何も発行せず、期限到達で対象 stream の
/// `StopSendingDataStream` が 1 回だけ発行され、再 tick でも再発行されないことを検証する。
#[test]
fn subgroup_data_stream_timeout_emits_stop_sending_without_close() -> noprop::TestResult {
    let mut runner = test_runner()?;
    // 境界 (timeout == 1 と 経過 == timeout) を確実に踏むため、サンプラーに境界値を明示する
    let boundary_seen = std::sync::atomic::AtomicBool::new(false);
    runner.run(256, |ctx| {
        let timeout =
            noprop::sample_with_boundaries(ctx, &[1u64, 2], noprop::Ratio::one_nth(4), |ctx| {
                noprop::sample_u64_in(ctx, 1..10_000)
            });
        let before = noprop::sample_u64_in(ctx, 0..timeout);
        let after = noprop::sample_with_boundaries(
            ctx,
            &[timeout, timeout.saturating_add(1)],
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_u64_in(ctx, timeout..=timeout + 10_000),
        );
        if after == timeout {
            // 期限ちょうど (`経過 >= timeout` の境界) を踏んだ
            boundary_seen.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let (mut client, _server) = establish_pair();
        client.tick(0);
        client.set_control_message_timeout_ms(None);
        client.set_data_stream_timeout_ms(Some(timeout));
        // stream type だけを受信した状態でも DATA_STREAM_TIMEOUT の監視対象になる
        // 0x14 は SUBGROUP_HEADER の stream type (draft-ietf-moq-transport-22 §11.3.1 (Subgroup Header))
        client
            .recv_data_stream_type(DataStreamId(1), 0x14)
            .expect("テストフィクスチャの前提条件を満たす");

        // 期限未満では何も発行しない
        client.tick(before);
        while let Some(e) = client.poll_event() {
            assert!(
                !matches!(e, SessionEvent::StopSendingDataStream { .. }),
                "期限未到達では STOP_SENDING を指示しないこと"
            );
        }
        assert_eq!(
            client.state(),
            SessionState::Established,
            "期限未到達ではセッションを閉じないこと"
        );

        // 期限到達で STOP_SENDING を 1 回だけ指示する
        client.tick(after);
        let mut stop_sending = 0;
        while let Some(e) = client.poll_event() {
            match e {
                SessionEvent::StopSendingDataStream {
                    stream_id,
                    error_code,
                } => {
                    assert_eq!(
                        stream_id,
                        DataStreamId(1),
                        "timeout した stream のみを対象にすること"
                    );
                    assert_eq!(
                        error_code, STREAM_CANCELLED,
                        "STOP_SENDING の error code は CANCELLED (§12.5) であること"
                    );
                    stop_sending += 1;
                }
                SessionEvent::CloseSession(err) => panic!(
                    "subgroup stream の DATA_STREAM_TIMEOUT でセッションを閉じてはいけない: {err:?}"
                ),
                _ => {}
            }
        }
        assert_eq!(
            stop_sending, 1,
            "期限到達で STOP_SENDING が 1 回だけ出ること"
        );

        // 監視から外れているため再 tick でも再発行されない
        client.tick(after.saturating_add(1));
        while let Some(e) = client.poll_event() {
            assert!(
                !matches!(e, SessionEvent::StopSendingDataStream { .. }),
                "期限到達後に STOP_SENDING を再発行しないこと"
            );
        }
        assert_eq!(
            client.state(),
            SessionState::Established,
            "timeout 後もセッションは Established のままであること"
        );
        Ok(())
    })?;
    // 境界 (経過 == timeout) のケースが実際に生成されたことを確認する
    assert!(
        boundary_seen.load(std::sync::atomic::Ordering::Relaxed),
        "経過 == timeout のケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// 期限到達した FETCH stream は 0x12 でセッションを閉じ、STOP_SENDING は指示しない
///
/// draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) の MAY は open な subgroup stream を
/// 対象にし、FETCH stream には代替措置を示していない。
#[test]
fn fetch_data_stream_timeout_closes_session() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let timeout = noprop::sample_u64_in(ctx, 1..10_000);
        let after = noprop::sample_u64_in(ctx, timeout..=timeout + 10_000);
        let (mut client, _server) = establish_pair();
        client.tick(0);
        client.set_control_message_timeout_ms(None);
        client.set_data_stream_timeout_ms(Some(timeout));
        client
            .recv_data_stream_type(DataStreamId(2), FETCH_HEADER_TYPE)
            .expect("テストフィクスチャの前提条件を満たす");

        client.tick(after);
        let mut closed = 0;
        while let Some(e) = client.poll_event() {
            match e {
                SessionEvent::CloseSession(err) => {
                    assert_eq!(
                        err.code, SESSION_DATA_STREAM_TIMEOUT,
                        "FETCH stream は 0x12 で閉じること"
                    );
                    closed += 1;
                }
                SessionEvent::StopSendingDataStream { .. } => {
                    panic!("FETCH stream では STOP_SENDING を指示しないこと")
                }
                _ => {}
            }
        }
        assert_eq!(
            closed, 1,
            "FETCH stream の期限到達で CloseSession が 1 回出ること"
        );
        // CloseSession の取り出しで Closing → Closed に遷移する
        assert_eq!(
            client.state(),
            SessionState::Closed,
            "0x12 でセッションが閉じること"
        );
        Ok(())
    })?;
    Ok(())
}
