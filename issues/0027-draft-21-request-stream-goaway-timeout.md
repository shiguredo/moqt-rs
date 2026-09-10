# request stream GOAWAY の timeout を実装する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-request-stream-goaway-timeout

## 目的

draft-ietf-moq-transport-21 §9.2 (GOAWAY) の SHOULD に従い、request stream 上の GOAWAY で timeout 経過後に `GOING_AWAY` で stream を reset する。

## 現状

`src/session/goaway.rs` の `send_goaway_on_request_stream` は deadline を設定しない。control stream 上の GOAWAY は `local_deadline_ms` で `GOAWAY_TIMEOUT` を扱うが、request stream 上の GOAWAY は timeout を記録しない。

根拠 (draft-ietf-moq-transport-21 §9.2):

> "When sent on a request stream, the sender SHOULD reset the stream with GOING_AWAY after the indicated timeout."

## 設計方針

request_id ごとの deadline を保持し、`tick` で期限到達時に `ResetRequestStream` (`GOING_AWAY`) を発火する。control GOAWAY の drain と同じ deadline 管理に載せる。sans I/O のため、期限判定は `tick(now_ms)` 駆動とする。

## 完了条件

- request stream GOAWAY の timeout が `tick` で判定されること
- 期限到達で対象 request stream が `GOING_AWAY` で reset されること
- 回帰テストが `tests/test_session/goaway.rs` に追加されていること
