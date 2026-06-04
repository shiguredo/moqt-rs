# validate_peer_request と recv_request の併用契約を明確にする

- Created: 2026-09-10
- Completed: 2026-09-12
- Branch: feature/remove-validate-peer-request-recv-request
- Polished: 2026-09-10

## 目的

公開 API の `validate_peer_request` と `recv_request` の二重投入で正当な request が重複拒否される問題をなくす。`validate_peer_request` はリポジトリ内外に利用者がおらず、内部でも `recv_request` の各ハンドラが `accept_peer_request` を直接呼ぶ構造になっているため、`validate_peer_request` を削除して `recv_request` に一本化する。

## 現状

- `src/session/core.rs` の `validate_peer_request` は `Established` チェックの後、内部の `accept_peer_request` を呼ぶだけの薄い公開ラッパーである。
- `accept_peer_request` は `RequestIdTracker` を進めるため、Application が `validate_peer_request` の後に `recv_request` へ同じ request_id を渡すと、`handle_peer_*` の先頭で再度 `accept_peer_request` が走り、重複として `INVALID_REQUEST_ID` でセッションを閉じる。
- `validate_peer_request` の利用箇所は `src/session/core.rs` の定義・module doc と `tests/test_session/setup.rs` の 2 テストのみで、examples / README / docs / SKILL.md / pbt / fuzz には出現しない。内部呼び出し元も存在しない。
- `tests/test_session/setup.rs` は `validate_peer_request` 単体しか検証しておらず、`recv_request` との併用契約が未定義である。両 API の doc にも相互排他の記載がない。
- `validate_peer_request` の doc には「Phase 3 以降で `recv_control` から呼ばれる前提」とあるが、`recv_control` は SETUP / GOAWAY のみを扱い `validate_peer_request` を呼ばない。実装と一致しない記述が残っている。

根拠 (draft-ietf-moq-transport-21 §6.4.2.1):

> "If an endpoint receives a Request ID where the least significant bit is incorrect for the sender, or a duplicate Request ID, it MUST close the session with INVALID_REQUEST_ID."

wire 上の重複 Request ID と parity 違反の検出 MUST は維持する必要がある。

## 設計方針

- `Session::validate_peer_request` を削除する。`pub(crate)` 化ではなく削除とする (crate 内呼び出し元がなく、残すとデッドコードになるため)。検証は `recv_request` の各ハンドラが先頭で呼ぶ `accept_peer_request` に一本化済みで、変更は不要。
- wire 上の重複 Request ID / parity 違反は、1 メッセージにつき `accept_peer_request` が 1 回だけ走る現行構造のまま `INVALID_REQUEST_ID` でセッションを閉じる。回帰テストで固定する。
- `src/session/core.rs` の module doc から `validate_peer_request` の記載を削除する。「Phase 3 以降で `recv_control` から呼ばれる前提」の記述は関数ごと削除される。
- `tests/test_session/setup.rs` の既存 2 テストは公開 API から移行する。
  - `client_server_request_id_cross_validation`: `recv_request` に parity の正しい request_id を持つメッセージを渡して受理されること、parity の誤った request_id で `INVALID_REQUEST_ID` になることを検証する形に書き換える。
  - `duplicate_peer_request_id_closes_session`: 同一 request_id のメッセージを `recv_request` に 2 回渡し、2 回目が `INVALID_REQUEST_ID` で `Closing` になることを検証する形に書き換える。
- 公開 API の削除 (後方互換のない変更) のため、ブランチは `feature/remove-...`、`CHANGES.md` の `## develop` に `[CHANGE]` として記載する (shiguredo-git の削除 prefix 規則と 0066 の前例に従う)。
- `src/session.rs` の module doc や SKILL.md などに `validate_peer_request` の記載があれば追従する。

## 完了条件

- `Session::validate_peer_request` が公開 API と crate 内の両方から削除されていること
- `recv_request` へ parity の正しい request_id を渡すと受理され、誤った request_id では `INVALID_REQUEST_ID` で `Closing` になること
- 同一 request_id を `recv_request` へ 2 回渡すと 2 回目が `INVALID_REQUEST_ID` で `Closing` になること
- `src/session/core.rs` の module doc に `validate_peer_request` の記載が残っていないこと
- `tests/test_session/setup.rs` の既存 2 テストが `recv_request` 経由の検証に置き換わり、`cargo test --workspace` が通ること
- `CHANGES.md` の `## develop` に `[CHANGE]` として記載されていること

## 解決方法

公開 API `Session::validate_peer_request` を削除し、peer Request ID の検証を `recv_request` に一本化した。

- `src/session/core.rs`: `validate_peer_request` を削除した。peer Request ID の parity / 重複検証は各ハンドラ先頭の `accept_peer_request` が 1 メッセージにつき 1 回行う現行構造を維持し、違反時は `INVALID_REQUEST_ID` で `Closing` に遷移する (draft §6.4.2.1)。module doc から `validate_peer_request` の記載を削除し、`recv_request` の doc に parity / 重複 / 保持上限超過の検証内容を追記した。
- `tests/test_session/setup.rs`: 既存 2 テストを `recv_request` 経由に書き換えた。parity の正しい request_id の受理、parity 違反 (未受信 ID) と重複の 2 回目で `INVALID_REQUEST_ID` + `Closing` を検証する。SUBSCRIBE を組み立てるローカルヘルパ `subscribe_message` を追加した。
- `CHANGES.md` の `## develop` に `[CHANGE]` エントリを追加した。
- ブランチ名は shiguredo-git の削除 prefix 規則に合わせて `feature/remove-validate-peer-request-recv-request` とした (issue の Branch フィールドと設計方針も追従)。
