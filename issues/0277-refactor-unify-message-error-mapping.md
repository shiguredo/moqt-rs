# メッセージエラーからセッション終了コードへの写像を共通化する

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-unify-message-error-mapping
- Polished: {YYYY-MM-DD}

## 目的

同じ分類の写像が 2 箇所にあり、片方だけを変更すると data plane と control plane でセッション終了コードが食い違う。写像を 1 箇所に寄せ、変更が両方に効くようにする。

## 現状

- `src/session/data.rs` の `session_error_from_data_message` は `MessageError` を `ProtocolViolation` / `MalformedTrack` → `SESSION_PROTOCOL_VIOLATION`、`KeyValueFormattingError` → `SESSION_KEY_VALUE_FORMATTING_ERROR`、その他 → `PROTOCOL_VIOLATION` に写す。
- `src/session/core.rs` の `session_error_from_request_body` は同じ分類を独自に持つ。差は catch-all の reason phrase だけで、写像そのものは同一である。
- `examples/tokio-moq/src/error.rs` の `session_error_code` は `MalformedAuthToken` を `SESSION_MALFORMED_AUTH_TOKEN` に写す点で 2 つと異なる (現状この経路では `MalformedAuthToken` は発生しないため実害はない)。
- 同じ分類が 3 箇所に分散しているため、`MessageError` に variant を追加したときに写像の更新漏れが起きうる。

## 設計方針

- `MessageError` から `SessionError` への写像を `session` モジュール内の 1 箇所に置き、data plane と control plane の両方から呼ぶ。
- catch-all の reason phrase は、共通の文言にするか、呼び出し側が文脈を渡せるようにするかを比較して決める。既存のログ・テストの期待値を変えないことを優先する。
- 挙動は変えない。既存テストがそのまま通ることを確認する。
- `examples` 側の写像を共通化するかは、example が library の内部型に依存しない方針を壊さない範囲で判断し、共通化しない場合は理由を doc に残す。

## 完了条件

- `MessageError` から `SessionError` への写像が 1 箇所になっていること。
- 既存の終了コードの対応が変わっていないことがテストで確認できること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
