# moqt-subscriber が終了済み subscription へ STOP_SENDING を送って警告を出す

- Created: 2026-09-17
- Completed: 2026-09-17
- Branch: feature/fix-subscriber-stop-sending-after-end
- Polished: {YYYY-MM-DD}

## 目的

`moqt-subscriber` が PUBLISH_DONE / GOAWAY / session close で受信ループを抜けたあと、終了済みの subscription へ STOP_SENDING を送ろうとして警告を出す。正常終了時に警告を出さないようにする。

## 現状

- `examples/moqt-subscriber/src/pipeline.rs` の受信ループを抜けたあと、`video_request_id` / `audio_request_id` へ `client.stop_sending(rid)` を送る
- ループを抜ける経路は 6 つある
  - `recv_acceptor.accept_recv_stream()` が `Ok(None)` (data stream がもう来ない)
  - 同 `Err` (data stream 受信失敗)
  - `SessionEvent::GoawayReceived`
  - `SessionEvent::PublishDoneReceived`
  - `SessionEvent::CloseSession`
  - graceful shutdown のシグナル
- 前 5 つは peer が subscription / session を終了させた経路であり、`Session` の subscription は `Terminated` に遷移済みである。この状態の `stop_sending` は
  `{error, ... requires Pending(Subscriber) or Established state}` になり、example が警告を出す
- 実測 (moqt-rs の publisher / subscriber と relay の E2E、publisher の graceful shutdown 直後):
  - `Failed to send STOP_SENDING for video: internal error: stop_sending: session error 0x3: stop_sending requires Pending(Subscriber) or Established state`
  - `Failed to send STOP_SENDING: bidi receive task already finished (request_id=6)`

## 設計方針

- 受信ループを peer 由来の理由で抜けたかを保持し、その場合は STOP_SENDING の後始末を行わない
- graceful shutdown のシグナルで抜けた場合は、自側の都合で止めるため従来どおり STOP_SENDING を送る
- `Session` の公開 API と状態機械は変更しない (終了済み subscription への `stop_sending` がエラーになるのは正しい挙動)

## 完了条件

- publisher の PUBLISH_DONE で `moqt-subscriber` が終了するとき、STOP_SENDING 由来の警告が出ないこと
- graceful shutdown のシグナルで終了するときは従来どおり STOP_SENDING を送ること
- 既存の終了処理 (GOAWAY / close) の挙動が変わらないこと

## 解決方法

`examples/moqt-subscriber/src/pipeline.rs` の受信ループに `peer_ended` を追加し、
peer 由来の理由でループを抜けた場合は STOP_SENDING の後始末を行わないようにした。

- `peer_ended = true` にする経路: `accept_recv_stream()` の `Ok(None)` と `Err`、
  `GoawayReceived`、`PublishDoneReceived`、`CloseSession`
- graceful shutdown のシグナルで抜けた場合は従来どおり STOP_SENDING を送る
- `Session` の公開 API と状態機械は変更していない。
  終了済み subscription への `stop_sending` がエラーになるのは draft-ietf-moq-transport-21
  §6.4.2.3 に沿った正しい挙動である

実測 (moqt-rs の publisher / subscriber と relay の E2E) では、publisher の
graceful shutdown で `Received PUBLISH_DONE (status_code=4, stream_count=18446744073709551615)`
を出力したあと、STOP_SENDING 由来の警告が 1 件も出ずに終了するようになった。

検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo fmt --all -- --check` の通過で確認した。
