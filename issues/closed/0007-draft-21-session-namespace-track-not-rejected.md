# `.session` 予約名前空間の非空トラック名を FETCH / TRACK_STATUS で拒否する

- Created: 2026-09-10
- Completed: 2026-09-11
- Branch: feature/fix-session-namespace-track-not-rejected
- Polished: 2026-09-10

## 目的

draft-ietf-moq-transport-21 §6.5 (Session-Level Tracks and Namespaces) の MUST を満たす。未認識の session-level track へのリクエストは Application へ流さず `DOES_NOT_EXIST` で拒否しなければならない。

## 現状

`src/session/fetch.rs` の `handle_peer_fetch` と `src/session/namespace/track_status.rs` の `handle_peer_track_status` は、`.session` 名前空間の判定に空トラック名の条件を付けている。

- `handle_peer_fetch`: `if track_ns.is_session_level() && track_name.is_empty()`
- `handle_peer_track_status`: `if msg.track_namespace.is_session_level() && msg.track_name.is_empty()`

非空トラック名はリクエストとして登録され、Application の応答に委ねられる。一方、SUBSCRIBE / PUBLISH (`src/session/subscription/recv.rs`)、PUBLISH_NAMESPACE (`src/session/namespace/publish_namespace.rs`)、SUBSCRIBE_NAMESPACE (`src/session/namespace/subscribe_namespace.rs`)、SUBSCRIBE_TRACKS
(`src/session/namespace/track_subscription.rs`) はトラック名を問わず `.session` を拒否しており、非対称になっている。

本ライブラリは session-level track を 1 つも登録していないため、非空の `.session` トラックは「unrecognized session-level track」にあたる。

根拠 (draft-ietf-moq-transport-21 §6.5):

> "An endpoint that receives a request for an unrecognized session-level track or namespace MUST reject it with REQUEST_ERROR using error code DOES_NOT_EXIST rather than passing it to the Application."

テストは空トラック名のみを検証しており、非空の拒否は未検証 (`tests/test_session/fetch/validation.rs`、`tests/test_session/namespace/track_status.rs`)。

## 設計方針

FETCH / TRACK_STATUS の `.session` 判定を他メッセージ種別と同じくトラック名不問の拒否に統一する。拡張で将来 session-level track を認識する場合に備え、認識済みレジストリの有無で分岐できる構造にするかは、拡張を実装する時点で判断する。

## 完了条件

- `.session` 名前空間の非空トラック名に対する FETCH / TRACK_STATUS が `DOES_NOT_EXIST` の `REQUEST_ERROR` になること
- 空トラック名の既存挙動が維持されること
- 非空トラック名の拒否テストが `tests/` に追加されていること

## 解決方法

`.session` 名前空間への FETCH / TRACK_STATUS をトラック名不問で拒否するようにした。

- `src/session/fetch.rs` の `handle_peer_fetch` と `src/session/namespace/track_status.rs` の `handle_peer_track_status` の `.session` 判定から空トラック名の条件を外し、非空トラック名も未認識のセッションレベルトラックとして `DOES_NOT_EXIST` の REQUEST_ERROR で拒否するようにした。空トラック名と非空トラック名で reason 文字列を分け、SUBSCRIBE の既存実装と揃えた。
- 空トラック名の既存挙動は維持し、reason 文字列を既存テストで固定した。
- 非空トラック名の拒否テストに加えて、`.session` を先頭に持つ複数フィールド名前空間の拒否と、先頭以外の `.session` を受理する境界テストを追加した。拒否応答が FIN で送られ、request テーブルを汚染しないことも検証した。
- `CHANGES.md` の `## develop` に `[FIX]` を追記した。
