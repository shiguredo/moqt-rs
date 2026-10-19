# 購読 cancel 後に届く SUBSCRIBE_OK / REQUEST_ERROR でセッションを閉じない

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-subscription-cancel-late-response
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §3.1 (Subscriptions) は "The subscriber terminates a subscription in the Pending (Subscriber) or Established states by sending STOP_SENDING." と定める。購読者が Pending(Subscriber) で cancel した直後に、peer が停止を観測する前に送った SUBSCRIBE_OK / REQUEST_ERROR
が届くのは正当な競合である。現状はこれを不正状態として `PROTOCOL_VIOLATION` でセッション全体を閉じる誤動作になっている。

## 現状

- `src/session/subscription/recv.rs` の `stop_sending` は subscription を `Terminated` にするだけで、`rejected_request_ids` に記録しない。
- `src/session/subscription.rs` の `supersede_pending_subscriber` も同様に `Terminated` にするだけで記録しない。
- `src/session/subscription/recv.rs` の `handle_peer_subscribe_ok` は「Pending(Subscriber) でも拒否済みでもない」SUBSCRIBE_OK を `PROTOCOL_VIOLATION` で閉じる。`src/session/subscription/dispatch.rs` の `handle_err_for_subscription` も `Terminated` の REQUEST_ERROR を同様に閉じる。
- FETCH 側は `src/session/fetch.rs` の `handle_peer_fetch_ok` が `Terminated` を許容して吸収しており、subscription だけが非対称である。
- 遅延応答を注入するテストは無い (`tests/test_session/request_stream.rs` の supersede テストは旧 request id の forget までしか見ていない)。
- 影響: cancel と応答の競合で全購読が落ちる。example の STOP_SENDING 経路でも起こり得る。

## 設計方針

- `Terminated` かつ `publish_done` 未設定 (`is_cancelled_terminated`) の subscription への SUBSCRIBE_OK / REQUEST_ERROR は、状態遷移もイベント発行もせず no-op で吸収する。
- `stop_sending` / `supersede_pending_subscriber` で当該 request id を拒否済みとして扱う方法も検討する。ただし Track Alias 衝突などの検証は残す。
- cancel と応答の到着順を入れ替えたテストを追加する。

## 完了条件

- Pending(Subscriber) で `stop_sending` した直後に SUBSCRIBE_OK / REQUEST_ERROR を `recv_stream_message` へ渡しても、セッションが閉じず購読だけが終端していること。
- `supersede_pending_subscriber` 経由でも同じであること。
- 上記を `tests/test_session` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
