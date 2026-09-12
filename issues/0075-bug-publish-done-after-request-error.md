# peer REQUEST_ERROR 受信後も publisher が PUBLISH_DONE を送れるようにする

- Created: 2026-09-13
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publish-done-after-request-error
- Polished: {YYYY-MM-DD}

## 目的

PUBLISH で確立した subscription の publisher が REQUEST_UPDATE を送り、peer subscriber から REQUEST_ERROR (失敗応答) を受信した場合でも、draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions) の MUST (publisher は PUBLISH_DONE with UPDATE_FAILED で subscription を終端する) を果たせるようにする。

## 現状

`src/session/core.rs` の `Session::handle_peer_request_error` は subscription の request を `Session::handle_err_for_subscription` に渡す。`src/session/subscription/dispatch.rs` の `handle_err_for_subscription` は Established を無条件に `Terminated` へ遷移させ、`publish_done` を `None` にする。

`src/session/subscription/send.rs` の `Session::send_publish_done` は publisher 側であっても `is_pending_publisher()` または `state == Established` を要求するため、Terminated 遷移後は `SESSION_PROTOCOL_VIOLATION` ("publish_done requires Pending(Publisher) or Established state") を返して PUBLISH_DONE を送れない。

一時テストで確認した流れ:

- client (publisher, PUBLISH の initiator) が `send_publish` → peer subscriber が REQUEST_OK → publisher が `send_request_update` (initiator のため送信可)
- peer subscriber 発の REQUEST_ERROR を `recv_stream_message` に注入すると subscription が `SubscriptionState::Terminated` になる
- この状態で `send_publish_done` を呼ぶと state ガードで拒否される

draft-ietf-moq-transport-21 §9.5.1: "When a REQUEST_UPDATE is unsuccessful, the publisher MUST also terminate the subscription by sending a PUBLISH_DONE with error code UPDATE_FAILED." この MUST は REQUEST_UPDATE の送信者である publisher (PUBLISH の initiator) 側にも係る。§9.9 (PUBLISH_DONE) の MUST NOT により、
open 中の outgoing data stream がある間は PUBLISH_DONE を送れない。

## 設計方針

- `handle_err_for_subscription` の Established 分岐で role を見て、publisher には PUBLISH_DONE(UPDATE_FAILED) の送信経路を残す (自動送信にするか state ガードを緩めるかは実装時に決める)。
- open 中の outgoing data stream の扱いは既存の `Subscription::pending_publish_done` と `maybe_flush_pending_publish_done` の規則 (§9.9 の MUST NOT と `send_data_stream_closed` / `reset_outgoing_data_stream` での flush) を再利用する。
- subscriber 側の Established 分岐 (Terminated へ遷移し、peer の PUBLISH_DONE を `handle_peer_publish_done` が受理する) の既存挙動は変えない。

## 完了条件

- PUBLISH 確立後の REQUEST_UPDATE 失敗応答 (REQUEST_ERROR 受信) で publisher が PUBLISH_DONE(UPDATE_FAILED) を送れること (自動送信・API 経由のどちらでもよい)
- open 中の outgoing data stream が残っている間は PUBLISH_DONE が送られず、全 stream 終端後に 1 回だけ送られること
- subscriber 側の REQUEST_ERROR 受信挙動と PUBLISH_DONE 受信挙動の既存テストが変わらず通ること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
