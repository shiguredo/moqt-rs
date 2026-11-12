# 送信 API の検証を状態遷移前に行い、encode 失敗による孤児状態を作らない

- Created: 2026-10-19
- Completed: 2026-11-11
- Branch: develop (CODEBASE.md の規約により develop に直接コミットする)
- Polished: 2026-10-19
- Refreshed: 2026-11-04

## 目的

sans-I/O では encode は I/O 層で行われるため、Session の送信 API が状態遷移後に encode で失敗すると、状態だけ進んだ孤児状態が残る。また
draft-ietf-moq-transport-23 §8.7 (Track Namespace Structure) の Full Track Name 4096 バイト上限 (超過は FIELD_LENGTH_EXCEEDED) は encode 時にしか検証されず、
Session に request が登録されたまま送信されない状態を作り得る。応答 (FETCH_OK / TRACK_STATUS_OK) と PUBLISH の Track Properties、および 4 つの
request 送信 API の Full Track Name について、検証を状態遷移前かつ request_id 発行前に揃える。

## 現状

- `src/session/fetch.rs` の `send_fetch_ok` は `FetchState::Established` に遷移した後に `SendOnStream` を積む。
  Track Properties の encode 検証は I/O 層の `RequestOk::encode_message_body` (`src/message.rs`)
  で行われ、失敗しても fetch は Established のまま応答を再送できない。draft-23 で FETCH_OK から End Location と
  End Of Track が削除されたため、`send_fetch_ok` の事前検証対象は Track Properties だけである。
- `src/session/track_status.rs` の `send_ok_for_track_status` は
  `entry.response = Some(TrackStatusResponse::Ok { .. })` を設定して control message deadline を解除した後に
  track_properties を返す。Track Properties の encode 検証は I/O 層の `RequestOk::encode_message_body`
  (`src/message.rs`) で行われ、失敗しても `entry.response` は `Some(Ok)` のまま応答を再送できない。
- `src/session/subscription/send.rs` の `send_subscribe_ok` だけが `track_properties.encode()` を状態遷移前に実行している。`send_fetch_ok` / `send_ok_for_track_status` / `send_publish` は実行していない。
- `src/session/subscription/send.rs` の `send_publish` は `track_properties` を `Publish` に渡し、
  `register_subscription` と `request_streams` / control message deadline / track_alias の登録後に `SendRequest`
  を積む。`Publish::encode_message_body` (`src/message.rs`) の `track_properties.encode()` が失敗すると、subscription は
  Pending のまま残る。Pending の subscription は `Subscription::cleanup_ready` が Terminated を要求するため
  `forget_subscription` で回収できず、同じ request_id で PUBLISH を再送できない。
- `send_subscribe` / `send_publish` / `send_fetch` / `send_track_status` は Full Track Name の合計長を検証しない。`src/message/common.rs` の `validate_full_track_name` が encode 時に初めて失敗する。
- パラメータスコープは `send_subscribe_ok` / `send_fetch_ok` / `send_publish` / `send_request_ok` (TRACK_STATUS) で既に状態遷移前に検証されている (`SUBSCRIBE_OK_ALLOWED_PARAMS` / `FETCH_OK_ALLOWED_PARAMS` / `PUBLISH_ALLOWED_PARAMS` / `TRACK_STATUS_OK_ALLOWED_PARAMS`)。draft-ietf-moq-transport-23 §9.19.1 (Parameter Scope)。
- 影響: 不正入力の API 呼び出しで、応答を送れない request が Pending / Established のまま、または TRACK_STATUS の `response` が設定済みのまま残る。PUBLISH の subscription も Pending のまま回収できない。

## 設計方針

- `src/session/core.rs` の `Session` に Track Properties の encode 可否を検証する private ヘルパを追加する。
  `TrackProperties::encode` を一時バッファに対して実行し、エラーを `SESSION_PROTOCOL_VIOLATION` の `SessionError` に変換する。
  `send_subscribe_ok` の既存のインライン検証をこのヘルパに置き換え、`send_fetch_ok` / `send_ok_for_track_status` / `send_publish`
  から状態遷移前かつ request 登録前に呼ぶ。`INCLUDE_PROPERTIES=0` による空化は検証後に適用し、`send_subscribe_ok` と同じく入力の Track Properties
  を検証する。
- `send_fetch_ok` は Track Properties の encode 可否を `fetch.state = FetchState::Established` の代入より前に検証する。違反時は `SESSION_PROTOCOL_VIOLATION` を返し、`fetch.state` / `end_location` / `SendOnStream` を変更しない。
- `send_ok_for_track_status` は Track Properties の検証を `entry.response = Some(..)` と `clear_control_message_deadline` より前に行う。違反時は `entry.response` / `terminated` / control message deadline / `SendOnStream` を変更しない。
- `send_publish` は Track Properties の検証を `request_id` 発行 / `register_subscription` / `request_streams` / control message deadline / track_alias の登録より前に行う。違反時は subscription も track_alias も登録しない。
- 4 つの request 送信 API (`send_subscribe` / `send_publish` / `send_fetch` / `send_track_status`) で、`request_id`
  発行前に `validate_full_track_name(&track_namespace, &track_name)` を呼ぶ。既存の 4096 バイト判定を再実装せず、
  `src/message/common.rs` の `validate_full_track_name` を再利用する。違反時は draft-ietf-moq-transport-23 §8.7 の
  FIELD_LENGTH_EXCEEDED に対応する `SendRequestError::Session` (`SESSION_FIELD_LENGTH_EXCEEDED`) を返し、request を登録しない。
- パラメータスコープの既存検証は状態遷移前にあり、変更しない。
- 失敗時に状態が変化しないことをテストで固定する。

## 完了条件

- 不正な Track Properties を含む FETCH_OK / TRACK_STATUS_OK / PUBLISH の送信が `SESSION_PROTOCOL_VIOLATION` で拒否され、それぞれ
  fetch が Pending のまま (`end_location` も未設定) / TRACK_STATUS の `response` が `None` のまま /
  PUBLISH では subscription / track_alias / control message deadline / `SendRequest` が登録されず request_id
  も発行されないこと。
- `send_subscribe_ok` の既存の Track Properties 事前検証が共通ヘルパ経由でも維持され、不正な Track Properties で subscription が Pending のまま、track_alias 未設定のままであること。
- `send_subscribe` / `send_publish` / `send_fetch` / `send_track_status` のそれぞれで、4097 バイトの Full Track Name の送信が
  `SESSION_FIELD_LENGTH_EXCEEDED` で拒否され、request_id が発行されないこと。request_id が発行されていないことは、失敗後の次の正当な送信が同じ request_id
  を返すことで確認する。`send_publish` では track_alias も登録されないこと (同じ track_alias で次の PUBLISH が成功すること)。いずれの API でも
  `SendRequest` が積まれないこと。
- 上記を `tests/test_session` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `src/session/core.rs` の `Session` に private ヘルパを 2 つ追加した。
  - `validate_outgoing_track_properties(&TrackProperties, what)`: `TrackProperties::encode` を一時バッファに対して実行し、失敗を `SESSION_PROTOCOL_VIOLATION` の `SessionError` に変換する。検証内容は encode と同一 (同一 prop_type の重複・型整合性・値域・ IMMUTABLE_PROPERTIES 内側の KVP 列) である。
  - `validate_outgoing_full_track_name(&TrackNamespace, &[u8])`: `src/message/common.rs` の `validate_full_track_name` を再利用し、4096 バイト超過を `SESSION_FIELD_LENGTH_EXCEEDED` の `SessionError` に変換する。
- `src/session/subscription/send.rs`:
  - `send_subscribe_ok` のインライン encode 検証を共通ヘルパ呼び出しに置き換えた (挙動は不変)。
  - `send_publish` で request_id 発行と状態登録より前に Track Properties の検証と Full Track Name の検証を行うようにした。拒否時は subscription / track_alias / control message deadline / `SendRequest` / request_id のいずれも登録しない。
  - `send_subscribe` で Full Track Name の検証を追加した。
- `src/session/fetch.rs`:
  - `send_fetch_ok` で `fetch.state = FetchState::Established` の代入前に Track Properties を検証するようにした。拒否時は `fetch.state` / `end_location` / `SendOnStream` を変更しない。`INCLUDE_PROPERTIES=0` による空化は検証後に適用するため、検証対象はアプリが渡した入力の Track Properties である。
  - `send_fetch` で Full Track Name の検証を追加した。
- `src/session/track_status.rs`:
  - `send_ok_for_track_status` の冒頭で Track Properties を検証するようにした。拒否時は `entry.response` / `terminated` / control message deadline / `SendOnStream` を変更しない。
  - `send_track_status` で Full Track Name の検証を追加した。
- 送信 API が不正な Track Properties を拒否するようになったため、受信側の Malformed Track 処理を固定していた既存テストのフィクスチャを、wire 上の不正メッセージを直接組み立てて `recv_request` / `recv_stream_message` に渡す形へ変更した (`tests/test_session/property_duplicates.rs` / `tests/test_session/property_scope.rs`)。`tests/test_session/parameter_rules.rs` の `client_receives_unsupported_property_details_for_rejected_publish` は奇数型 0x4005 に `VarInt` を入れていた誤りを `Bytes` に直した。
- テスト: `tests/test_session/send_prevalidation.rs` を追加した。
  - `fetch_ok_with_invalid_track_properties_keeps_pending`: 不正な Track Properties の FETCH_OK が `SESSION_PROTOCOL_VIOLATION` で拒否され、fetch が Pending のまま、`end_location` 未設定、`SendOnStream` 未発行であること。
  - `track_status_ok_with_invalid_track_properties_keeps_unanswered`: 同じく TRACK_STATUS_OK が拒否され、`response` が `None`、`terminated` が false、`SendOnStream` 未発行であること。
  - `publish_with_invalid_track_properties_registers_nothing`: 不正な PUBLISH が拒否され、次の同じ track_alias の PUBLISH が成功して request_id が 0 のままであること。
  - `oversized_full_track_name_rejected_by_all_request_apis`: 4097 バイトの Full Track Name が `send_subscribe` / `send_publish` / `send_fetch` / `send_track_status` の 4 API で `SESSION_FIELD_LENGTH_EXCEEDED` になり、拒否後の正当な送信が request_id 0 を返し、同じ track_alias の PUBLISH も成功すること。
- 検証: `cargo test -p shiguredo_moqt --test test_session` (1115 件) / `cargo test -p shiguredo_moqt --lib` (133 件) / `cargo clippy -p shiguredo_moqt --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通る。
