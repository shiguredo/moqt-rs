# SUBSCRIBE / PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE の .session 拒否テストを追加する

- Created: 2026-09-11
- Completed: {YYYY-MM-DD}
- Branch: feature/test-session-namespace-reject-coverage
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §6.5 の `.session` 拒否を 7 種の受信ハンドラすべてでテストに固定する。実装は全種で拒否しているが、一部のメッセージ種別に回帰テストがない。

## 現状

受信ハンドラの実装は 7 種すべてで `.session` をトラック名 / prefix 不問で拒否している (`handle_peer_subscribe` / `handle_peer_publish` / `handle_peer_fetch` / `handle_peer_track_status` / `handle_peer_publish_namespace` / `handle_peer_subscribe_namespace` / `handle_peer_subscribe_tracks`)。

`.session` 拒否の回帰テストの有無:

- SUBSCRIBE: なし (single period の拒否テストのみ)
- PUBLISH: あり
- FETCH: あり (空・非空・複数フィールド)
- TRACK_STATUS: あり (空・非空・複数フィールド)
- PUBLISH_NAMESPACE: なし (single period の拒否テストのみ)
- SUBSCRIBE_NAMESPACE: なし (single period の拒否テストのみ)
- SUBSCRIBE_TRACKS: あり

根拠 (draft-ietf-moq-transport-21 §6.5 (Session-Level Tracks and Namespaces)):

> "An endpoint that receives a request for an unrecognized session-level track or namespace MUST reject it with REQUEST_ERROR using error code DOES_NOT_EXIST rather than passing it to the Application."

## 設計方針

既存の single period 拒否テストと同じ手順で、track_name / prefix が非空の `.session` リクエストを注入し、次を検証する。

- `DOES_NOT_EXIST` の REQUEST_ERROR が送られること
- request テーブルに登録されないこと (`assert_no_tracked_requests` など)
- セッションが `Established` のままであること

追加対象は `tests/test_session/subscription/handshake.rs` (SUBSCRIBE)、`tests/test_session/namespace/publish_namespace.rs` (PUBLISH_NAMESPACE)、`tests/test_session/namespace/subscribe_namespace.rs` (SUBSCRIBE_NAMESPACE)。

## 完了条件

- 上記 3 種の `.session` 拒否テストが追加され、`cargo test --workspace` が通ること
- 拒否時の状態非汚染 (request テーブル・セッション状態) がテストで確認されていること
