//! GOAWAY / GOAWAY_TIMEOUT のプロパティテスト
//!
//! - GOAWAY URI の制限内ラウンドトリップ
//! - GOAWAY URI の制限超過拒否
//! - blocker 無し GOAWAY の timeout 非発火
//! - blocker 残り GOAWAY の timeout 発火
//! - request stream 上の GOAWAY の deadline 満了で reset と終端が request ごとに 1 回だけ発行される

use pbt::common::test_runner;
use shiguredo_moqt::message::common::TrackNamespace;
use shiguredo_moqt::message_parameter::MessageParameters;
use shiguredo_moqt::session::types::MAX_NEW_SESSION_URI_LENGTH;
use shiguredo_moqt::session::types::TerminationReason;
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
