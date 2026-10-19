# 送信 API の検証を状態遷移前に行い、孤児状態と encode 失敗を作らない

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-send-response-prevalidation
- Polished: {YYYY-MM-DD}

## 目的

sans-I/O では encode は I/O 層で行われるため、Session の送信 API が状態遷移後に encode で失敗すると、状態だけ進んだ孤児状態が残る。また draft-ietf-moq-transport-22 §8.7 (Track Namespace Structure) の Full Track Name 4096 バイト上限は encode 時にしか検証されず、Session に request が登録されたまま送信されない状態を作り得る。送信 API の検証を状態遷移前に揃える。

## 現状

- `src/session/fetch.rs` の `send_fetch_ok` は `FetchState::Established` に遷移した後で `end_of_track > 1` と Track Properties の encode 検証を行う。失敗しても fetch は Established のまま応答を再送できない。
- `src/session/track_status.rs` の `send_ok_for_track_status` は `entry.response = Some(...)` の後で Track Properties の encode 検証を行う。
- `src/session/subscription/send.rs` の `send_subscribe_ok` だけが `track_properties.encode()` を状態遷移前に実行している。
- `send_subscribe` / `send_publish` / `send_fetch` / `send_track_status` は Full Track Name の合計長を検証しない。`src/message/common.rs` の `validate_full_track_name` が encode 時に初めて失敗する。
- 影響: 不正入力の API 呼び出しで、応答を送れない request が Pending / Established のまま残る。

## 設計方針

- FETCH_OK / TRACK_STATUS_OK について、`end_of_track` の値域、パラメータスコープ、Track Properties の encode 可否を状態遷移前に検証する。共通ヘルパに集約する。
- 4 つの request 送信 API で `track_namespace.byte_length() + track_name.len() <= MAX_TRACK_NAME_LENGTH` を request_id 発行前に検証する。
- 失敗時に状態が変化しないことをテストで固定する。

## 完了条件

- 不正な FETCH_OK / TRACK_STATUS_OK の送信が状態遷移前にエラーになり、状態が変化しないこと。
- 4097 バイトの Full Track Name の送信が request_id 発行前にエラーになること。
- 上記を `tests/test_session` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
