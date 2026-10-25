# 公開受信 API で request メッセージのパラメータスコープを検証する

- Created: 2026-10-25
- Completed: 2026-10-25
- Branch: develop
- Polished: 2026-10-25

## 目的

draft-ietf-moq-transport-22 §9.20.1 (Parameter Scope) は "If a parameter appears in some other type of message, the receiving
endpoint MUST close the connection with a PROTOCOL_VIOLATION." と MUST を課す。wire 経路では `ControlMessage::decode` が検証するが、
公開受信 API `Session::recv_request` は decode 済みの `ControlMessage` を直接受け取るため、decode を通らない手組みメッセージでは
session 層で検証しなければ MUST を満たせない。本 issue は `Session::recv_request` が扱う初回 request メッセージ
(SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS) のトップレベルパラメータと、FILL_PARAMETERS (0x23) の内側パラメータ
(§9.20.15 (FILL PARAMETERS Parameter)) を対象とする。FILL_PARAMETERS は §9.20.15 により SUBSCRIBE と
REQUEST_UPDATE (subscription 用) にのみ出現できるため、内側検証は SUBSCRIBE と REQUEST_UPDATE の受信経路を対象とし、
PUBLISH / FETCH / TRACK_STATUS に載った場合はトップレベルスコープ違反として扱う。

## 現状

- `Session::recv_request` は Established を確認したうえで SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS / 未対応 request を振り分ける。
  未対応 request は `Session::handle_unsupported_request` が Request ID → 本体解析 → `validate_scope` → AUTH_TOKEN 一意性 →
  Range Filter → 予約名前空間の順で検証している。
- SUBSCRIBE は `Session::accept_incoming_track_request` 経由で、`Session::accept_peer_request` (Request ID の parity / 重複 →
  AUTH_TOKEN 一意性 → GOAWAY) → `apply_peer_message_auth_tokens` → `check_incoming_range_filters` (INVALID_FILTER の
  REQUEST_ERROR) → 予約名前空間 (`.` / `.session` は DOES_NOT_EXIST の REQUEST_ERROR) の順に処理する。`validate_scope` は呼ばない。
- PUBLISH は `accept_peer_request(..., false)` (GOAWAY 拒否なし) → AUTH_TOKEN 適用 → Range Filter → 予約名前空間 → … の順に処理し、
  `validate_scope` を呼ばない。
- FETCH は `accept_peer_request(..., true)` → AUTH_TOKEN 適用 → Range Filter → 予約名前空間 → … の順に処理し、`validate_scope` を呼ばない。
- TRACK_STATUS は `accept_incoming_track_request` の後に `validate_scope(TRACK_STATUS_ALLOWED_PARAMS)` を呼ぶが、
  `accept_incoming_track_request` が `Ok(false)` を返した場合 (AUTH_TOKEN 重複、GOAWAY、INVALID_FILTER、DOES_NOT_EXIST) は
  早期 return するため、許可外パラメータとの複合違反では §9.20.1 の MUST close が REQUEST_ERROR に隠れる。
- wire 経路は各 `decode_message_body` が `validate_scope` を呼ぶ。decode 済みの `ControlMessage` を `Session::recv_request` に
  直接渡すと、その検証を受けられない。
- FILL_PARAMETERS の内側は §9.20.15 が `FILL_PARAMETERS_ALLOWED_PARAMS` 外のパラメータ受信に PROTOCOL_VIOLATION を MUST とする。
  wire は `decode_fill_parameters`、送信は `validate_outgoing_fill_parameters` が検証するが、`Session::recv_request` の受信経路は
  内側を検証しない。
- 応答・通知メッセージ (SUBSCRIBE_OK / FETCH_OK / PUBLISH_STATE_NOTIFY 等) の公開受信 API 側は issue 0305 で扱う。
- REQUEST_UPDATE は `Session::recv_stream_message` 経由で `Session::handle_peer_request_update` が context 別に
  `validate_scope` を呼ぶが、FILL_PARAMETERS の内側は検証しない。`SUBSCRIPTION_UPDATE_ALLOWED_PARAMS` は
  FILL_PARAMETERS を含むため、内側の許可外パラメータは §9.20.15 の MUST close を満たさず受理される。

## 設計方針

- 検証位置をハンドラに散らさず `Session::accept_peer_request` に集約する。引数に `allowed_params: &[u64]` を追加し、
  `validate_peer_request_id` の直後、AUTH_TOKEN 一意性検証と GOAWAY 拒否より前に `validate_scope` を置く。
  順序の根拠: Request ID の MUST、§9.20.1 の MUST close、§9.20.15 の MUST close は、メッセージ単位の reject
  (MALFORMED_AUTH_TOKEN / GOING_AWAY / INVALID_FILTER / DOES_NOT_EXIST) より先に処理する。未対応 request と同じ順序になる。
- `Session::accept_incoming_track_request` にも `allowed_params` を追加し、`accept_peer_request` へ委譲する。
  呼び出し側は SUBSCRIBE が `SUBSCRIBE_ALLOWED_PARAMS`、TRACK_STATUS が `TRACK_STATUS_ALLOWED_PARAMS`、PUBLISH が
  `PUBLISH_ALLOWED_PARAMS`、FETCH が `FETCH_ALLOWED_PARAMS` を渡す。
- TRACK_STATUS が `accept_incoming_track_request` の後に呼んでいる `validate_scope` は削除する
  (`accept_peer_request` に集約されるため)。
- FILL_PARAMETERS の内側は、`MessageParameters::validate_fill_parameters_scope` を追加して
  `fill_parameters()` が `Some(inner)` なら `inner.validate_scope(FILL_PARAMETERS_ALLOWED_PARAMS)` を呼ぶ形に集約する。
  `accept_peer_request` で外側スコープ検証の直後に呼び、違反は PROTOCOL_VIOLATION で `fail` する。
  `Session::handle_peer_request_update` でも既存の context 別 `validate_scope` の直後に同じ検証を呼ぶ
  (§9.20.15 の MUST close を未達のまま残さない)。
- 違反時は `self.fail(SessionError::new(SESSION_PROTOCOL_VIOLATION, ...))` で session を Closing にし、REQUEST_ERROR は送らない。
- `Session::recv_request` の doc に、decode 済みメッセージを渡した場合も session 層で §9.20.1 / §9.20.15 を検証することを明記する。
- テストは `tests/test_session/parameter_rules.rs` に追加する。違反の優先順位 (scope 違反が INVALID_FILTER / MALFORMED_AUTH_TOKEN /
  DOES_NOT_EXIST / GOAWAY 拒否より先に PROTOCOL_VIOLATION になること) は `tests/test_session/subscription/range_filter_scope_order.rs`
  の既存パターンに倣って固定する。TRACK_STATUS と未対応 request の既存テストが退行しないことも確認する。

## 完了条件

- 手組みの SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS のそれぞれについて、許可されないトップレベルパラメータを含むメッセージを
  `Session::recv_request` に渡すと、セッションが `SESSION_PROTOCOL_VIOLATION` (0x3) で閉じること
  (例: SUBSCRIBE + EXPIRES、PUBLISH + NEW_GROUP_REQUEST、FETCH + LARGEST_OBJECT、TRACK_STATUS + EXPIRES)。
- 許可パラメータのみを含む手組みメッセージは従来どおり受理され、セッションが閉じないこと。
- 複合違反 (許可外パラメータ + `.` 予約名前空間 / INVALID_FILTER / 重複 AUTH_TOKEN / GOAWAY 拒否対象) で、scope 違反の
  PROTOCOL_VIOLATION が優先され、REQUEST_ERROR が送信されないこと。
- PUBLISH に Range Filter (SUBGROUP_FILTER 等) を載せた手組みメッセージが、従来の INVALID_FILTER の REQUEST_ERROR ではなく、
  §9.20.1 の PROTOCOL_VIOLATION で閉じること。`tests/test_session/subscription/subscription_limits.rs` の
  `initial_requests_with_range_filter_violation_send_request_error_only` を更新する。このテストは PUBLISH と FETCH を
  同一ループで検証しており、PUBLISH のみ PROTOCOL_VIOLATION に変更し、FETCH の SUBGROUP_FILTER は
  `FETCH_ALLOWED_PARAMS` に含まれるため従来どおり INVALID_FILTER を維持する (回帰ガードを残す)。
- TRACK_STATUS の scope 検証が `accept_peer_request` 経由に移り、早期 return による MUST close の取りこぼしが無いこと
  (既存テストの非退行を含む)。
- FILL_PARAMETERS の内側に許可外パラメータを含む手組み SUBSCRIBE と REQUEST_UPDATE が `SESSION_PROTOCOL_VIOLATION` で
  閉じること。内側が許可パラメータのみなら従来どおり受理されること。
- PUBLISH / FETCH / TRACK_STATUS に FILL_PARAMETERS を載せた手組みメッセージが、外側スコープ違反として
  `SESSION_PROTOCOL_VIOLATION` で閉じること (内側検証の対象外であることの固定)。
- `Session::recv_request` の doc に session 層検証の記載があること。
- wire 経路 (`ControlMessage::decode` 経由) の挙動が変わらないこと。
- 未対応 request (PUBLISH_NAMESPACE 等) の既存 scope 検証とエラーコードが退行しないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 対象外

- 応答・通知メッセージの受信 API (issue 0305 で扱う)。
- REQUEST_UPDATE / REQUEST_OK などのトップレベルスコープ検証 (既存の context 別検証に従う。本 issue は
  REQUEST_UPDATE の FILL_PARAMETERS 内側のみを追加する)。

## 解決方法

- `Session::accept_peer_request` に `allowed_params: &[u64]` と `scope_error_reason: &'static str` を追加し、
  `validate_peer_request_id` の直後・AUTH_TOKEN 一意性検証と GOAWAY 拒否より前に `MessageParameters::validate_scope`
  を置いた。SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS の全初回 request が同じ順序で検証される。
- `Session::accept_incoming_track_request` も `allowed_params` / `scope_error_reason` を受け取り、`accept_peer_request`
  へ委譲するようにした。SUBSCRIBE は `SUBSCRIBE_ALLOWED_PARAMS`、TRACK_STATUS は `TRACK_STATUS_ALLOWED_PARAMS`、
  PUBLISH は `PUBLISH_ALLOWED_PARAMS`、FETCH は `FETCH_ALLOWED_PARAMS` を渡す。
- TRACK_STATUS が `accept_incoming_track_request` の後に呼んでいた `validate_scope` を削除した。早期 return 経路でも
  §9.20.1 の MUST close がメッセージ単位の reject に隠れない。
- `MessageParameters::validate_fill_parameters_scope` を追加した。FILL_PARAMETERS の全個を走査して内側を
  `FILL_PARAMETERS_ALLOWED_PARAMS` で検証し、重複は §9.20 (Control Message Parameters) の受信側 SHOULD として
  PROTOCOL_VIOLATION、値が `FillParameters` でない手組みは不正形として PROTOCOL_VIOLATION にする。
  `accept_peer_request` と `Session::handle_peer_request_update` の両方から呼ぶ。
- `Session::recv_request` の doc に、decode 済みメッセージを渡した場合も session 層で §9.20.1 / §9.20.15 を検証する
  ことを明記した。`accept_peer_request` / `accept_incoming_track_request` / `*_ALLOWED_PARAMS` の doc も更新した。
- `tests/test_session/parameter_rules.rs` に、SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS の許可外パラメータ、
  許可パラメータのみの受理、予約名前空間・GOAWAY・重複 AUTH_TOKEN との複合違反、FILL_PARAMETERS の内側違反と
  許可内受理、重複 FILL、`FillParameters` 以外の値、REQUEST_UPDATE の FILL 内側違反と許可内受理を検証するテストを
  追加した。scope 違反では `CloseSession(PROTOCOL_VIOLATION)` が 1 回だけ発行され REQUEST_ERROR を送らないことを固定した。
- `tests/test_session/subscription/subscription_limits.rs` の
  `initial_requests_with_range_filter_violation_send_request_error_only` を分割し、PUBLISH + Range Filter は
  §9.20.1 の PROTOCOL_VIOLATION、FETCH + Range Filter は `FETCH_ALLOWED_PARAMS` に含まれるため従来どおり
  INVALID_FILTER の REQUEST_ERROR になることを固定した。

## 完了条件の検証

- 許可外トップレベルパラメータ: `recv_request_with_disallowed_parameter_closes_session`
- 許可パラメータのみの受理: `recv_request_with_allowed_parameters_is_accepted`
- 複合違反の優先順位: `recv_request_scope_violation_precedes_reserved_namespace_rejection` /
  `recv_request_scope_violation_precedes_going_away_rejection` /
  `recv_request_scope_violation_precedes_auth_token_rejection`
- TRACK_STATUS の早期 return 取りこぼし: 上記の TRACK_STATUS + `.` の複合違反テスト
- FILL_PARAMETERS の内側: `recv_request_fill_parameters_inner_scope_violation_closes_session` /
  `recv_request_fill_parameters_with_allowed_inner_is_accepted` /
  `recv_request_update_fill_parameters_inner_scope_violation_closes_session` /
  `recv_request_update_fill_parameters_with_allowed_inner_is_accepted` /
  `recv_request_duplicate_fill_parameters_closes_session` /
  `recv_request_fill_parameters_with_invalid_value_encoding_closes_session`
- FILL_PARAMETERS を許可しないメッセージでの外側スコープ違反: `recv_request_fill_parameters_outside_subscribe_closes_session`
- PUBLISH / FETCH の Range Filter: `initial_publish_with_range_filter_closes_session_with_protocol_violation` /
  `initial_fetch_with_range_filter_violation_sends_request_error_only`
- wire 経路と未対応 request の非退行: 既存テストがすべて成功
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` 成功
