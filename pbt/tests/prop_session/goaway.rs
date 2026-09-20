//! GOAWAY / GOAWAY_TIMEOUT のプロパティテスト
//!
//! - GOAWAY URI の制限内ラウンドトリップ
//! - GOAWAY URI の制限超過拒否
//! - blocker 無し GOAWAY の timeout 非発火
//! - blocker 残り GOAWAY の timeout 発火

use pbt::common::test_runner;
use shiguredo_moqt::message::common::TrackNamespace;
use shiguredo_moqt::message_parameter::MessageParameters;
use shiguredo_moqt::session::types::MAX_NEW_SESSION_URI_LENGTH;
use shiguredo_moqt::{session::types::SessionEvent, session::types::SessionState};

use super::common::establish_pair;

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
