# 公開受信 API で応答・通知メッセージのパラメータスコープを検証する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-response-parameter-scope
- Polished: 2026-10-19

## 目的

draft-ietf-moq-transport-22 §9.20.1 (Parameter Scope) は "If a parameter appears in some other type of message, the receiving endpoint MUST close the connection with a PROTOCOL_VIOLATION." と MUST を課す。wire 経路では `ControlMessage::decode` が検証するが、公開 API `recv_stream_message` は decode
済みメッセージを直接受け取るため、session 層でも検証しなければ MUST を満たせない。

## 現状

- `src/session/subscription/recv.rs` の `handle_peer_subscribe_ok` は SUBSCRIBE_OK のパラメータスコープを検証しない。
- `src/session/fetch.rs` の `handle_peer_fetch_ok` は FETCH_OK (許可パラメータ 0 個) のスコープを検証しない。
- `src/session/subscription/recv.rs` の `handle_peer_publish_state_notify` は PUBLISH_STATE_NOTIFY (LARGEST_OBJECT / FORWARD / LOCATION_FILTER のみ) のスコープを検証しない。
- `src/session/core.rs` の `handle_peer_request_ok` / `src/session/track_status.rs` の `handle_peer_track_status` / `src/session/subscription/recv.rs` の `handle_peer_request_update` は context 別スコープを検証しており、非対称である。
- `src/session/data.rs` の `recv_fetch_header` が受け取る `src/stream/fetch.rs` の `FetchHeader` は Request ID
  のみでパラメータを持たない。FILL_PARAMETERS は SUBSCRIBE / REQUEST_UPDATE のパラメータであり、本 issue が対象とする応答・通知メッセージのスコープ検証とは別である。
  wire 経路の FILL_PARAMETERS 内側スコープは decode 層 (`src/message_parameter.rs` の `decode_fill_parameters`) で検証され、送信側は
  `src/session/subscription/fill.rs` の `validate_outgoing_fill_parameters` で検証される。
- 影響: 手組みメッセージを渡す利用者が §9.20.1 の MUST 違反メッセージを受理させられる。

## 設計方針

- 上記 3 ハンドラの冒頭 (ローカル終端済みの早期 return より前) で `SUBSCRIBE_OK_ALLOWED_PARAMS` / `FETCH_OK_ALLOWED_PARAMS` / `PUBLISH_STATE_NOTIFY_ALLOWED_PARAMS` を検証し、違反時は `self.fail` して `PROTOCOL_VIOLATION` とする。
- `src/session/core.rs` の `recv_stream_message` の doc に「decode 済みメッセージを渡すが、session 層でもスコープを検証する」と明記する。
- `tests/test_session/parameter_rules.rs` に手組みメッセージの拒否テストを追加する。

## 完了条件

- SUBSCRIBE_OK / FETCH_OK / PUBLISH_STATE_NOTIFY のそれぞれについて、許可外パラメータを含む手組みメッセージでセッションが `PROTOCOL_VIOLATION` で閉じること。ローカル終端済みの request へ遅延して届いた場合も、早期 return で素通りせず閉じること。
- `src/session/core.rs` の `recv_stream_message` の doc が実態と一致すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
