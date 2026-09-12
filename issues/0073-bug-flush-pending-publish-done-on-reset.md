# send_data_stream_closed の Reset でも保留 PUBLISH_DONE を flush する

- Created: 2026-09-13
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-flush-pending-publish-done-on-reset
- Polished: {YYYY-MM-DD}

## 目的

REQUEST_UPDATE の失敗応答で保留された PUBLISH_DONE (UPDATE_FAILED) が、outgoing data stream を Reset で閉じた場合にも送信され、draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions) の MUST (REQUEST_UPDATE が失敗したら publisher は PUBLISH_DONE で subscription を終端する) を満たすようにする。

## 現状

`src/session/data.rs` の `Session::send_data_stream_closed` は、`RequestStreamEnd::Fin` のときだけ `Session::maybe_flush_pending_publish_done` を呼ぶ。`RequestStreamEnd::Reset` のときは呼ばないため、I/O 層が先に stream を reset して `send_data_stream_closed(Reset)` を直接呼ぶ経路では `Subscription::pending_publish_done` が残ったままになり、
PUBLISH_DONE が送られない。

- `reset_outgoing_data_stream_at_with_code` 経由では、`send_data_stream_closed(Reset)` の後に `ResetDataStream` イベントを push し、その後に `maybe_flush_pending_publish_done` を呼ぶため flush される。
- 一方、`examples/moqt-publisher/src/stream_writer.rs` の `finish` のように、アプリが `reset` した後に `send_data_stream_closed(Reset)` を直接呼ぶ経路では flush されない。
- 一時テストで確認した挙動: Established publisher の subscription で REQUEST_UPDATE 失敗応答 (`send_request_error`) の後に outgoing subgroup stream を `send_data_stream_closed(Reset)` で閉じても、`pending_publish_done` は `Some(1)` のまま残り、`SessionEvent::SendOnStream` の PUBLISH_DONE は発行されない (`Fin` で閉じた場合は発行される)。
- `send_data_stream_closed` の中で無条件に flush すると、`reset_outgoing_data_stream_at_with_code` 経由のときに `ResetDataStream` イベントより先に PUBLISH_DONE が push され、ワイヤ順序が PUBLISH_DONE → RESET_STREAM になって §9.9 (PUBLISH_DONE) の MUST NOT ("A sender MUST NOT send PUBLISH_DONE until it has closed all streams it will
  ever open") に反する。

## 設計方針

- `send_data_stream_closed` の Reset 経路でも、その stream の終端確定後に `maybe_flush_pending_publish_done` を呼ぶ。
- `reset_outgoing_data_stream_at_with_code` 経由では RESET_STREAM → PUBLISH_DONE のワイヤ順序を維持する。内部呼び出しと I/O 直接通知を区別できるようにする (private helper への分離など)。flush の条件 (全 outgoing stream 終端 + Terminated + 保留あり) は既存の `maybe_flush_pending_publish_done` のまま変えない。
- FIN 経路の既存挙動は変えない。

## 完了条件

- `send_data_stream_closed(Reset)` で最後の outgoing stream を閉じた後、保留 PUBLISH_DONE が 1 回だけ自動送信されること
- `reset_outgoing_data_stream` / `reset_outgoing_data_stream_at` 経由のイベント順が `ResetDataStream` → PUBLISH_DONE のままであること
- open 中の outgoing stream が残っている間は PUBLISH_DONE を送らないこと (§9.9 の MUST NOT) がテストで固定されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
