# 別タスクからの flush と request 回収が競合して PUBLISH_DONE を失う

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-moqt-client-concurrent-flush-cleanup-race
- Polished: {YYYY-MM-DD}

## 目的

0201 で request の回収条件に保留中の PUBLISH_DONE を加えたが、保証は「Session のイベントキューが
空になってから回収する」ところまでである。`send_data_stream_closed` を別タスクから呼ぶ配線では、
キューが空になった直後に flush が `SendOnStream` を積む競合が残る。回収が先に走ると subscription が
破棄され、積まれた PUBLISH_DONE は送信半が無いため捨てられる (§9.5.1 (Updating Subscriptions) の MUST
を果たせない)。配線を増やしたときに黙って壊れない状態にする。

## 現状

- `examples/tokio-moq/src/moqt_client.rs` の `MoqtClient::cleanup_closed_requests` は `drain_events` の
  末尾 (と `CloseSession` の早期 return 前) に呼ばれ、`Subscription::pending_publish_done` が `None` なら
  subscription を破棄する。
- `src/session/data.rs` の `Session::maybe_flush_pending_publish_done` は `send_data_stream_closed` /
  `reset_outgoing_data_stream` / fill fetch stream への STOP_SENDING を契機に `SendOnStream { fin: true }`
  を積み、`pending_publish_done` を `take` する。
- これらの送信 API は `DataPlaneHandle` (Clone 可能で Session を共有する) から呼べるため、別タスクから
  呼ぶ配線では「drain がキュー空を観測 → 別タスクが flush → 回収が subscription を破棄」の順になりうる。
- Session にはイベントキューに未処理のイベントがあるかを問い合わせる API が無いため、example 側だけでは
  競合を閉じられない。
- 現在の examples は data plane を main ループからのみ使うため到達しない (コードにも制限として明記済み)。

## 設計方針

- 案を比較して決める。
  - Session に「該当 request のイベントがキューに残っているか」を問い合わせる API を足し、回収の判定と
    キュー確認を同一ロックで行う
  - flush の完了を Session に記録させ、回収がそれを観測してから破棄する
  - `forget_subscription` の前提として「保留 PUBLISH_DONE を破棄しない」ことを Session 側の責務にする
- 回収の判定は引き続き純関数に保ち、同一ロックで完結させる。
- 「回収は Session のイベントを I/O へ変換し終えた後にだけ行う」契約と、`tick` から回収しない方針は維持する。

## 完了条件

- 別タスクから `send_data_stream_closed` を呼ぶ配線でも、flush された PUBLISH_DONE が request stream へ
  書き出されること。競合を閉じられない場合は、その理由と到達条件を doc と issue に記録し、到達条件を
  再現するテストを追加したうえで、恒久対策を別 issue として残すこと。
- 追加した挙動を単体テストで固定すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること
