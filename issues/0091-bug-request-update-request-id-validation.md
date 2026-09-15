# REQUEST_UPDATE の Request ID を §6.4.2.1 に従って検証する

- Created: 2026-09-15
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-request-update-request-id-validation
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §6.4.2.1 (Request ID) は REQUEST_UPDATE も Request ID を消費する
メッセージとして列挙し、受信した Request ID の parity 違反と重複に対して MUST を課している。
受信側は REQUEST_UPDATE が運ぶ Request ID を検証していないため MUST 違反になる。

根拠 (draft-ietf-moq-transport-21 §6.4.2.1 (Request ID)):

> Each SUBSCRIBE, PUBLISH, FETCH, SUBSCRIBE_NAMESPACE, SUBSCRIBE_TRACKS, PUBLISH_NAMESPACE,
> REQUEST_UPDATE, and TRACK_STATUS message consumes a Request ID.

> If an endpoint receives a Request ID where the least significant bit is incorrect for the
> sender, or a duplicate Request ID, it MUST close the session with INVALID_REQUEST_ID.

## 現状

- `src/session/subscription/recv.rs` の `handle_peer_request_update` は wire の
  `RequestUpdate.request_id` を stream context の request_id と比較するだけで、parity と重複の
  検証を行わない。この一致比較は issue 0087 の作業で削除されるため、削除後は REQUEST_UPDATE の
  Request ID に関する受信側の検証が一切無くなる。
- SUBSCRIBE / PUBLISH / FETCH は各ハンドラ先頭で `accept_peer_request` を呼び、
  `RequestIdTracker::accept` が parity 違反と重複を検出して `INVALID_REQUEST_ID` で
  セッションを Closing に遷移させる (`src/session/core.rs` の `accept_peer_request`)。
- 送信側は REQUEST_UPDATE ごとに新しい Request ID を採番する (issue 0087)。

## 設計方針

- `handle_peer_request_update` で wire の `update.request_id` を `accept_peer_request` に渡し、
  parity と重複を検証する。stream context の request_id は購読の同定に引き続き使う。
- `accept_peer_request` は GOAWAY 送信後の新規 request を `REQUEST_ERROR` で拒否する経路を持つ。
  REQUEST_UPDATE は既存の bidi stream 上を流れるため、この経路をそのまま適用してよいかを
  実装前に確認する (§9.5 (REQUEST_UPDATE) の応答規定と矛盾しないか)。
- 検証は `handle_peer_request_update` の先頭で 1 回だけ行い、`handle_update_for_subscription` /
  `handle_update_for_fetch` の分岐より前に済ませる。
- REQUEST_UPDATE の REQUEST_ERROR は購読の終端を伴うため、検証失敗時の応答を送るかどうかは
  §6.4.2.1 の MUST (即時 close) を優先して決める。

## 完了条件

- parity 違反の Request ID を持つ REQUEST_UPDATE の受信で、`INVALID_REQUEST_ID` により
  セッションが Closing に遷移すること
- 過去に使用済みの Request ID を持つ REQUEST_UPDATE の受信でも同じく Closing に遷移すること
- 新規採番された正しい parity の Request ID を持つ REQUEST_UPDATE は従来どおり受理されること
- 回帰テストが `tests/test_session/` に追加され、`cargo test --workspace` が通ること
- `cargo clippy --workspace --all-targets -- -D warnings` と `cargo fmt --all -- --check` が通ること
- `CHANGES.md` の `## develop` に `[FIX]` エントリが追加されていること
