# FETCH の cancel で bidi request stream の受信方向に STOP_SENDING を指示する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-fetch-cancel-stop-sending
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §3.2.4 (Fetch State Management) は "The subscriber wishing to cancel the FETCH MAY send STOP_SENDING for the data stream as well as the bidi request stream. It MUST send STOP_SENDING for the bidi request stream." と MUST を課す。現状の FETCH cancel は bidi
request stream の受信方向に STOP_SENDING を発行しない。

## 現状

- `src/session/fetch.rs` の `send_fetch_stop_sending` は `SessionEvent::ResetRequestStream` (自側送信方向の RESET_STREAM) だけを発行し、`SessionEvent::StopSendingRequestStream` を発行しない。
- `src/session/data.rs` の `send_data_stream_stop_sending` は FETCH 応答 stream への STOP_SENDING を `send_fetch_stop_sending` に dispatch するため、example がデータストリームを cancel する経路でも bidi の STOP_SENDING が欠ける。
- `tests/test_session/fetch/unified.rs` の `send_fetch_stop_sending` テストは `ResetRequestStream` だけを検証しており、欠落を固定していない。

## 設計方針

- `send_fetch_stop_sending` で `StopSendingRequestStream { error_code: STREAM_CANCELLED }` を `ResetRequestStream` より先に発行する。
- `SessionEvent` の順序契約 (cancel は受信方向の STOP_SENDING → 送信方向の RESET_STREAM) を doc に明記する。
- テストを両イベント検証に更新する。

## 完了条件

- FETCH cancel で `StopSendingRequestStream` と `ResetRequestStream` の両方が発行されること。
- 順序が契約どおりであることをテストで固定すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
