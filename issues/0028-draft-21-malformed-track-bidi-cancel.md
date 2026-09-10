# Malformed Track 検出時に bidi request stream を cancel する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-malformed-track-bidi-cancel

## 目的

draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) の MUST を満たす。Malformed Track を検出した subscriber が、対応する subscription / fetch の bidi request stream を cancel する。

## 現状

`src/session/data.rs` の `terminate_malformed_track` は `ResetDataStream` と `RequestTerminated { reason: MalformedTrack }` を発行するが、`ResetRequestStream` / `StopSendingRequestStream` を発行しない。`src/session/types.rs` の doc は「Application が §6.4.2.3 に従い閉じる」としている。

根拠 (draft-ietf-moq-transport-21 §12.1):

> "When a subscriber detects a Malformed Track, it MUST cancel any corresponding subscription or fetches for that Track from that publisher"

## 設計方針

`terminate_malformed_track` で bidi request stream の cancel 指示 (`ResetRequestStream` 相当) を発行する。relay 実装向けに届いた Object を forward しない既存挙動は維持する。

## 完了条件

- Malformed Track 検出時に対象 request の bidi request stream が cancel されること
- データストリームの reset と `RequestTerminated` の既存挙動が維持されること
- 回帰テストが `tests/test_session/` に追加されていること
