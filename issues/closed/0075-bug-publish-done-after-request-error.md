# peer REQUEST_ERROR 受信後も publisher が PUBLISH_DONE を送れるようにする

- Created: 2026-09-13
- Completed: 2026-09-17
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

## 解決方法

`src/session/subscription/dispatch.rs` の `handle_err_for_subscription` の Established 分岐で、自側が PUBLISH 起点 subscription の publisher
(`my_role == TrackRole::Publisher` かつ `is_initiator_self()`) の場合に `publish_done_stream_count` を立て、関数末尾で
PUBLISH_DONE (UPDATE_FAILED) を発行するようにした。

- open 中の outgoing data stream (subgroup / fill fetch) がある場合は `Subscription::pending_publish_done` に保留し、
  `send_data_stream_closed` / `reset_outgoing_data_stream` からの `maybe_flush_pending_publish_done` で全 stream 終端後に 1 回だけ送る
  (responder 側の `send_err_for_subscription` と同じ扱い)
- open 中の stream が無い場合は REQUEST_ERROR の直後に PUBLISH_DONE を送る。PUBLISH_DONE が最終メッセージのため FIN を付け、
  request stream GOAWAY の reset deadline も解除する
- stream count は `Subscription::stream_counts.published_count` を使う
- `send_publish_done` の state ガード (`Pending(Publisher)` / `Established` のみ) は変更していない。
  Terminated 遷移後に呼ぶ必要があるのは自動送信経路だけであり、そちらは state ガードを通らずにイベントを発行する
- subscriber 側 (SUBSCRIBE 起点の subscriber、受信 PUBLISH の subscriber) は従来どおり Terminated へ遷移するだけで、
  PUBLISH_DONE は peer publisher が送る

テストは `src/session/tests.rs` に 2 本追加した。

- `publisher_request_update_error_sends_publish_done_update_failed`: PUBLISH 起点の Established で REQUEST_UPDATE を送り、
  peer から REQUEST_ERROR を受信すると PUBLISH_DONE (UPDATE_FAILED、stream count 0、FIN) が 1 回だけ発行される
- `publisher_request_update_error_defers_publish_done_until_streams_close`: outgoing subgroup stream が open の間は発行されず、
  `send_data_stream_closed` で終端すると stream count 1 で 1 回だけ発行される

検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` の通過で確認した。
