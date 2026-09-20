# REQUEST_UPDATE の Request ID を §6.4.2.1 に従って検証する

- Created: 2026-09-15
- Completed: 2026-09-15
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

## 解決方法

`handle_peer_request_update` の先頭で wire の Request ID を検証し、parity 違反と重複を
`INVALID_REQUEST_ID` でのセッションクローズにつなげた。

- `src/session/core.rs` の `accept_peer_request` が持っていた parity と重複の検証を
  `validate_peer_request_id` として切り出した。`accept_peer_request` はこれを呼ぶ。
- `src/session/subscription/recv.rs` の `handle_peer_request_update` は先頭で
  `validate_peer_request_id(update.request_id)` を 1 回だけ呼び、subscription / fetch の
  分岐より前に検証を済ませる。wire の Request ID は購読の解決には使わず、従来どおり
  stream context の Request ID で解決する。
- GOAWAY 送信済みでも REQUEST_UPDATE は拒否しない。§9.2 (GOAWAY) の "The GOAWAY message does
  not impact subscription state." と、§9.5 (REQUEST_UPDATE) の「REQUEST_OK / REQUEST_ERROR の
  いずれか 1 つで応答する」MUST を根拠に、`accept_peer_request` の GOING_AWAY 拒否経路は
  適用せず `validate_peer_request_id` だけを呼ぶ。
- 検証失敗時は §6.4.2.1 の MUST (即時 close) を優先し、REQUEST_ERROR は送らない。
- `CHANGES.md` の `## develop` に `[FIX]` エントリを追加した。

回帰テストは `tests/test_session/subscription/request_update.rs` に 3 件追加した。
`request_update_with_wrong_parity_closes_session` (parity 違反)、
`request_update_with_duplicate_request_id_closes_session` (重複)、
`request_update_after_goaway_is_accepted` (GOAWAY 後も受理し REQUEST_OK で応答できる) である。
新規採番された正しい parity の Request ID が受理されることは、0087 で追加した
`request_update_consumes_new_request_id` が固定している。

REQUEST_UPDATE の wire Request ID に購読の Request ID と同じ値を注入していた既存テストは、
peer が採番する Request ID を載せる形に更新した。注入は 1 通につき Request ID を 1 つ消費する
ため、同じ session へ複数回注入するテストは 2 ずつ進める値に直した。

検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo fmt --all -- --check` がすべて通ることを確認した。
