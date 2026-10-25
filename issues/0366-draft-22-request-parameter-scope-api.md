# 公開受信 API で request メッセージのパラメータスコープを検証する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §9.20.1 (Parameter Scope) は "If a parameter appears in some other type of message, the receiving
endpoint MUST close the connection with a PROTOCOL_VIOLATION." と MUST を課す。wire 経路では `ControlMessage::decode` が検証するが、公開受信 API
`Session::recv_request` は decode 済みメッセージを直接受け取るため、session 層でも検証しなければ MUST を満たせない。応答・通知メッセージ側は別 issue で対応対象になっているが、request
メッセージ側は未対応のままである。

## 現状

- `src/session/core.rs` の `Session::recv_request` は SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS / 未対応 request を振り分ける。TRACK_STATUS
  と未対応 request は session 層でスコープを検証している。
- `src/session/subscription/recv.rs` の `handle_peer_subscribe` と `handle_peer_publish`、`src/session/fetch.rs` の
  `handle_peer_fetch` は、`Session::accept_peer_request` / `Session::accept_incoming_track_request` で Request ID・AUTH_TOKEN
  の一意性・GOAWAY・予約名前空間・Range Filter の総数を検証するが、§9.20.1 のパラメータスコープを検証しない。
- 同じ §9.20.1 の MUST は、応答・通知メッセージ (SUBSCRIBE_OK / FETCH_OK / PUBLISH_STATE_NOTIFY) の公開受信 API 経路でも未対応であり、別 issue として扱われている。
- wire 経路で `ControlMessage::decode` が検証するため、この欠落が観測されるのは decode 済みメッセージを手組みして `Session::recv_request` に渡す利用者だけである。

## 設計方針

- `handle_peer_subscribe` / `handle_peer_publish` / `handle_peer_fetch` の冒頭で、それぞれ `SUBSCRIBE_ALLOWED_PARAMS` /
  `PUBLISH_ALLOWED_PARAMS` / `FETCH_ALLOWED_PARAMS` を使って `MessageParameters::validate_scope` を呼び、違反時は
  `SESSION_PROTOCOL_VIOLATION` で `fail` する。
- 検証は AUTH TOKEN の適用と状態遷移より前に行う。スコープ違反はセッション終了を伴うため、§8.9 (Authorization Token Compression) の「セッションエラーにならないメッセージの失敗でも REGISTER
  を適用する」MUST の対象外である。
- `Session::recv_request` の doc に、decode 済みメッセージを渡す場合も session 層でスコープを検証することを明記する。
- `tests/test_session/parameter_rules.rs` に、手組みの SUBSCRIBE (`EXPIRES` など SUBSCRIBE で許可されないパラメータ) / PUBLISH (`NEW_GROUP_REQUEST`
  など) / FETCH (`LARGEST_OBJECT` など) を `recv_request` に渡すとセッションが `PROTOCOL_VIOLATION` で閉じること、許可パラメータのみなら従来どおり受理されることを追加する。

## 完了条件

- 手組みの SUBSCRIBE / PUBLISH / FETCH のそれぞれについて、許可されないパラメータを含むメッセージを `Session::recv_request` に渡すと、セッションが
  `SESSION_PROTOCOL_VIOLATION` (0x3) で閉じること。
- 許可パラメータのみを含む手組みメッセージは従来どおり受理され、セッションが閉じないこと。
- wire 経路 (`ControlMessage::decode` 経由) の挙動が変わらないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
