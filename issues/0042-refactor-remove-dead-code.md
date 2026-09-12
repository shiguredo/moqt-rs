# 到達しない公開コードと過剰公開を整理する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-remove-dead-code

## 目的

死にコードと過剰公開を除去し、公開 API と内部状態の境界を明確にする。

## 現状

- `src/session/subscription/delivery.rs` の `refresh_effective_object_delivery_timeout` / `refresh_effective_subgroup_delivery_timeout` / `set_subscription_subscriber_object_delivery_timeout` / `set_subscription_subscriber_subgroup_delivery_timeout` は同ファイル内からのみ呼ばれる。
- `src/session/types.rs` の `TrackSubscription::forward_state` は受信側で書き込まれるがライブラリ内部で読まれない (`src/session/namespace/track_subscription.rs` のコメントが自己申告)。
- `examples/moqt-transport/src/moqt_client.rs` の `send_publish_done` 後の `bidi_sends.remove(&request_id)` は、直前の `drain_events` 内 `fin: true` 処理で既に remove 済みのため常に `None`。`finish()` は到達しない。

## 設計方針

- `delivery.rs` の 4 関数: private 化する。
- `TrackSubscription::forward_state`: `send_publish` の初期 FORWARD 決定に使うか、削除する。
- example の no-op `remove`: 削除する。

## 完了条件

- 到達しない公開コードが削除または統合されていること
- 内部利用のみの関数が private になっていること
- 削除による公開 API 変更が `CHANGES.md` に記載されていること
- `cargo clippy --workspace --all-targets -- -D warnings` が通ること
