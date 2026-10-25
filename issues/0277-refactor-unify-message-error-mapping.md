# メッセージエラーからセッション終了コードへの写像を共通化する

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-unify-message-error-mapping
- Polished: 2026-10-25

## 目的

同じ分類の写像が 2 箇所にあり、片方だけを変更すると data plane と control plane でセッション終了コードが食い違う。写像を 1 箇所に寄せ、変更が両方に効くようにする。

## 現状

- `src/session/data.rs` の `session_error_from_data_message` は `MessageError` を `ProtocolViolation` / `MalformedTrack` → `SESSION_PROTOCOL_VIOLATION`、`KeyValueFormattingError` → `SESSION_KEY_VALUE_FORMATTING_ERROR`、その他 → `SESSION_PROTOCOL_VIOLATION` に写す。
- `src/session/core.rs` の `session_error_from_request_body` は同じ分類を独自に持つ。差は catch-all の reason phrase だけで、写像そのものは同一である。
- `examples/tokio-moq/src/error.rs` の `session_error_code` は `MalformedAuthToken` を `SESSION_MALFORMED_AUTH_TOKEN` に写す点で 2 つと異なる。
- library の 2 経路には `MalformedAuthToken` が届かないため library 内の統一には影響しない。
- example では SETUP の decode 失敗で `MalformedAuthToken` が実際に届く。
- `examples/tokio-moq/src/moqt_client.rs` の `close_handshake_session_on_message_error` と `examples/tokio-moq/src/peer_harness.rs` の `initial_setup_with_duplicate_tokens_closes_with_malformed_auth_token` が 0x16 を固定している。
- `data_stream_termination_code` は終了対象の分類に `session_error_code` を使うため、この扱いに追随する。
- 同じ分類の大部分が 3 箇所に分散しているため、`MessageError` に variant を追加したときに写像の更新漏れが起きうる。

## 設計方針

- `MessageError` から `SessionError` への写像を `session` モジュール内の 1 箇所に置き、data plane と control plane の両方から呼ぶ。`subscription` の送受信 API がローカル検証エラーを `SessionError` に変換する箇所 (`location_filter_update` の失敗など) は、現在の 2 関数が担う分類とは異なるため対象外とする。
- catch-all の reason phrase は、共通の文言にするか、呼び出し側が文脈を渡せるようにするかを比較して決める。既存のログ・テストの期待値を変えないことを優先する。呼び出し側が文脈を渡す場合は、既存の catch-all 文言 (`data plane decode error` / `unsupported request body decode error`) をそのまま渡す。
- 挙動は変えない。既存テストがそのまま通ることを確認する。
- 共通写像の doc には、現在の `session_error_from_request_body` の §8.3 / §8.7 / §9.20 の根拠と、data plane 側の §11.1.1 / §11.1.2 / §11.2.1 / §11.3.1 / §12.1 の根拠を集約し、節番号・規則が draft 由来で将来変わる可能性があることを書く。
- `examples` 側の写像を共通化するかは、共通写像を library の公開 API にするか `pub(crate)` に留めるかで判断する。`pub(crate)` に留める場合は example の `session_error_code` を共通化せず、`MalformedAuthToken` を 0x16 に写す理由を `examples/tokio-moq/src/error.rs` の doc に残す。

## 完了条件

- data plane と control plane で使う `MessageError` から `SessionError` への写像 (現在の `session_error_from_data_message` と `session_error_from_request_body` が担うもの) が `session` モジュール内の 1 箇所に統合されていること。
- 既存の終了コードの対応が変わっていないことがテストで確認できること。少なくとも `ProtocolViolation` / `MalformedTrack` → `SESSION_PROTOCOL_VIOLATION`、`KeyValueFormattingError` → `SESSION_KEY_VALUE_FORMATTING_ERROR`、catch-all → `SESSION_PROTOCOL_VIOLATION` を共通写像に対して固定するテストを追加し、既存テストもそのまま通ること。
- `examples` 側の写像を共通化する場合としない場合のどちらでも、`examples/tokio-moq/src/error.rs` の doc が実際の写像と一致していること（共通化しない場合は `MalformedAuthToken` を 0x16 に写す理由と library の共通写像との差、共通化する場合は共通写像を使う旨）。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
