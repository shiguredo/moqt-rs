//! Request ID 管理のプロパティテスト
//!
//! - RequestIdGenerator の parity と +2 増分
//! - RequestIdTracker の parity 受理と重複拒否

use pbt::common::test_runner;
use shiguredo_moqt::session::request_id::{RequestIdGenerator, RequestIdTracker};
use shiguredo_moqt::session::types::Role;

// RequestIdGenerator: 任意回数の next_id で role に合致する parity と +2 増分
#[test]
fn request_id_generator_parity_and_increment() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let is_client = noprop::sample_bool(ctx);
        let count = noprop::sample_usize_in(ctx, 0..64);
        let role = if is_client {
            Role::Client
        } else {
            Role::Server
        };
        let expected_parity = if is_client { 0 } else { 1 };
        let mut g = RequestIdGenerator::new(role);
        let mut prev: Option<u64> = None;
        for _ in 0..count {
            let id = g.next_id();
            assert_eq!(id % 2, expected_parity);
            if let Some(p) = prev {
                assert_eq!(id, p + 2);
            }
            prev = Some(id);
        }
        Ok(())
    })?;
    Ok(())
}

// RequestIdTracker: role に合致する parity の新規 Request ID は accept、
// 重複は必ず INVALID_REQUEST_ID
#[test]
fn request_id_tracker_parity_and_duplicate() -> noprop::TestResult {
    // 重複 (エラー) と新規 (受理) の両方の分岐の観測をカバレッジゲートで検証する
    let duplicate_seen = std::cell::Cell::new(false);
    let accepted_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let peer_is_client = noprop::sample_bool(ctx);
        let id_count = noprop::sample_usize_in(ctx, 0..32);
        let raw_ids = (0..id_count)
            .map(|_| noprop::sample_u64_in(ctx, 0..1024))
            .collect::<Vec<_>>();
        let peer_role = if peer_is_client {
            Role::Client
        } else {
            Role::Server
        };
        let expected_parity: u64 = if peer_is_client { 0 } else { 1 };
        let mut t = RequestIdTracker::new(peer_role);
        let mut accepted: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for id in raw_ids {
            // 正しい parity に正規化して検証対象を作る
            let id = (id / 2) * 2 + expected_parity;
            let res = t.accept(id);
            if accepted.contains(&id) {
                assert!(res.is_err());
                duplicate_seen.set(true);
            } else {
                assert!(res.is_ok());
                accepted_seen.set(true);
                accepted.insert(id);
            }
        }
        assert_eq!(t.seen_count(), accepted.len());
        Ok(())
    })?;
    assert!(
        duplicate_seen.get(),
        "重複 Request ID のケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        accepted_seen.get(),
        "新規 Request ID の受理ケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}
