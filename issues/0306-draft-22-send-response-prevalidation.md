# 送信 API の検証を状態遷移前に行い、encode 失敗による孤児状態を作らない

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-send-response-prevalidation
- Polished: 2026-10-19

## 目的

sans-I/O では encode は I/O 層で行われるため、Session の送信 API が状態遷移後に encode で失敗すると、状態だけ進んだ孤児状態が残る。また draft-ietf-moq-transport-22 §8.7 (Track Namespace Structure) の Full Track Name 4096 バイト上限は encode 時にしか検証されず、Session に request が登録されたまま送信されない状態を作り得る。応答 (FETCH_OK / TRACK_STATUS_OK) と PUBLISH の Track Properties、および 4 つの request 送信 API の Full Track Name について、検証を状態遷移前かつ request_id 発行前に揃える。

## 現状

- `src/session/fetch.rs` の `send_fetch_ok` は `FetchState::Established` に遷移した後に `SendOnStream` を積む。`end_of_track > 1` と Track Properties の encode 検証は I/O 層の `FetchOk::encode_message_body` (`src/message.rs`) で行われ、失敗しても fetch は Established のまま応答を再送できない。`send_fetch_ok` 自身のコメントにも「`end_of_track > 1` や不正な `track_properties` はエンコード時検証のまま残る (同型の孤児化経路)」とある。
- `src/session/track_status.rs` の `send_ok_for_track_status` は `entry.response = Some(TrackStatusResponse::Ok { .. })` を設定して control message deadline を解除した後に track_properties を返す。Track Properties の encode 検証は I/O 層の `RequestOk::encode_message_body` (`src/message.rs`) で行われ、失敗しても `entry.response` は `Some(Ok)` のまま応答を再送できない。
- `src/session/subscription/send.rs` の `send_subscribe_ok` だけが `track_properties.encode()` を状態遷移前に実行している。`send_fetch_ok` / `send_ok_for_track_status` / `send_publish` は実行していない。
- `src/session/subscription/send.rs` の `send_publish` は `track_properties` を `Publish` に渡し、`register_subscription` と `request_streams` / control message deadline / track_alias の登録後に `SendRequest` を積む。`Publish::encode_message_body` (`src/message.rs`) の `track_properties.encode()` が失敗すると、subscription は Pending のまま残る。Pending の subscription は `Subscription::cleanup_ready` が Terminated を要求するため `forget_subscription` で回収できず、同じ request_id で PUBLISH を再送できない。
- `send_subscribe` / `send_publish` / `send_fetch` / `send_track_status` は Full Track Name の合計長を検証しない。`src/message/common.rs` の `validate_full_track_name` が encode 時に初めて失敗する。
- パラメータスコープは `send_subscribe_ok` / `send_fetch_ok` / `send_publish` / `send_request_ok` (TRACK_STATUS) で既に状態遷移前に検証されている (`SUBSCRIBE_OK_ALLOWED_PARAMS` / `FETCH_OK_ALLOWED_PARAMS` / `PUBLISH_ALLOWED_PARAMS` / `TRACK_STATUS_OK_ALLOWED_PARAMS`)。draft-ietf-moq-transport-22 §9.20.1 (Parameter Scope)。
- 影響: 不正入力の API 呼び出しで、応答を送れない request が Pending / Established のまま、または TRACK_STATUS の `response` が設定済みのまま残る。PUBLISH の subscription も Pending のまま回収できない。

## 設計方針

- `src/session/core.rs` の `Session` に Track Properties の encode 可否を検証する private ヘルパを追加する。`TrackProperties::encode` を一時バッファに対して実行し、エラーを `SESSION_PROTOCOL_VIOLATION` の `SessionError` に変換する。`send_subscribe_ok` の既存のインライン検証をこのヘルパに置き換え、`send_fetch_ok` / `send_ok_for_track_status` / `send_publish` から状態遷移前かつ request 登録前に呼ぶ。`INCLUDE_PROPERTIES=0` による空化は検証後に適用し、`send_subscribe_ok` と同じく入力の Track Properties を検証する。
- `send_fetch_ok` は `end_of_track` が 0 または 1 であることを、`fetch.state = FetchState::Established` の代入より前に検証する (draft-ietf-moq-transport-22 §9.12 (FETCH_OK))。違反時は `SESSION_PROTOCOL_VIOLATION` を返し、`fetch.state` / `end_of_track` / `end_location` / `SendOnStream` を変更しない。
- `send_ok_for_track_status` は Track Properties の検証を `entry.response = Some(..)` と `clear_control_message_deadline` より前に行う。違反時は `entry.response` / `terminated` / control message deadline / `SendOnStream` を変更しない。
- `send_publish` は Track Properties の検証を `request_id` 発行 / `register_subscription` / `request_streams` / control message deadline / track_alias の登録より前に行う。違反時は subscription も track_alias も登録しない。
- 4 つの request 送信 API (`send_subscribe` / `send_publish` / `send_fetch` / `send_track_status`) で、`request_id` 発行前に `validate_full_track_name(&track_namespace, &track_name)` を呼ぶ。既存の 4096 バイト判定を再実装せず、`src/message/common.rs` の `validate_full_track_name` を再利用する。違反時は `SendRequestError::Session` (`SESSION_PROTOCOL_VIOLATION`) を返し、request を登録しない。
- パラメータスコープの既存検証は状態遷移前にあり、変更しない。
- 失敗時に状態が変化しないことをテストで固定する。

## 完了条件

- `end_of_track` が 2 以上の FETCH_OK の送信が `SESSION_PROTOCOL_VIOLATION` で拒否され、fetch が Pending のまま (`end_of_track` / `end_location` も未設定)、`SendOnStream` が積まれないこと。
- 不正な Track Properties を含む FETCH_OK / TRACK_STATUS_OK / PUBLISH の送信が `SESSION_PROTOCOL_VIOLATION` で拒否され、それぞれ fetch が Pending のまま (`end_of_track` / `end_location` も未設定) / TRACK_STATUS の `response` が `None` のまま / PUBLISH では subscription / track_alias / control message deadline / `SendRequest` が登録されず request_id も発行されないこと。
- `send_subscribe_ok` の既存の Track Properties 事前検証が共通ヘルパ経由でも維持され、不正な Track Properties で subscription が Pending のまま、track_alias 未設定のままであること。
- `send_subscribe` / `send_publish` / `send_fetch` / `send_track_status` のそれぞれで、4097 バイトの Full Track Name の送信が `SESSION_PROTOCOL_VIOLATION` で拒否され、request_id が発行されないこと。request_id が発行されていないことは、失敗後の次の正当な送信が同じ request_id を返すことで確認する。`send_publish` では track_alias も登録されないこと (同じ track_alias で次の PUBLISH が成功すること)。いずれの API でも `SendRequest` が積まれないこと。
- 上記を `tests/test_session` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
