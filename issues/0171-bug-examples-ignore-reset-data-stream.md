# examples が ResetDataStream を無視して保留 PUBLISH_DONE が flush されない

- Created: 2026-09-26
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-examples-ignore-reset-data-stream
- Polished: {YYYY-MM-DD}

## 目的

`Session` は outgoing data stream の reset を `SessionEvent::ResetDataStream` で I/O 層へ指示するが、examples がこのイベントを無視している。そのため Session が「開いている」とみなす stream が閉じず、`Subscription::pending_publish_done` の flush 条件 (全 outgoing stream の終端) が満たされずに PUBLISH_DONE が送られない経路がありうる。

## 現状

- `SessionEvent::ResetDataStream` は `examples/moqt-publisher` / `examples/moqt-subscriber` のイベント処理で無視されている (要確認: 各 example の `poll_event` ループ)
- 0141 の検証で、OBJECT_DELIVERY_TIMEOUT / SUBGROUP_DELIVERY_TIMEOUT を設定した場合に保留 PUBLISH_DONE が flush されないケースが指摘された
- `Session` 側の flush 判定は `Subscription::pending_publish_done` と「open 中の outgoing stream 数」に依存する (`Session::maybe_flush_pending_publish_done`)

## 設計方針

- 各 example で `ResetDataStream` を処理し、対象 stream を reset したうえで Session へ stream の終端を通知する (既存の `send_data_stream_reset` 相当の公開 API を使う。無い場合は API の追加を検討する)
- 0141 で `send_fetch_data_stream_closed` を flush の契機にしなかった判断 (fill fetch stream の終端では flush しない) を壊さないこと
- 実機確認は delivery timeout を設定した publication で PUBLISH_DONE が届くことを `RUST_LOG=debug` で確認する

## 完了条件

- examples が `ResetDataStream` を処理し、保留中の PUBLISH_DONE が flush されることを確認できること (ログまたはテスト)
- `Session` 側の flush 条件に変更が必要な場合は、その条件を固定するテストが追加されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
