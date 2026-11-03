# request stream の GOAWAY を要求受信者だけに許可する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-request-stream-goaway-role
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §9.2](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/session/goaway.rs` の `send_goaway_on_request_stream` は要求の起点を検証せずイベントを送出する。
`handle_peer_goaway_on_request_stream` も重複と URI だけを確認するため、request sender から receiver へ送られた GOAWAY を受理する。

## 設計方針

要求の initiator を SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS ごとに確認し、receiver だけが request stream 上で GOAWAY を送れるようにする。
違反する受信は明示された PROTOCOL_VIOLATION で終了し、送信 API はイベントや期限 state を作る前に拒否する。
client / server role や publisher / subscriber role を request initiator と混同しない。

## 完了条件

- SUBSCRIBE と PUBLISH で sender / receiver が逆になるケース、FETCH / TRACK_STATUS を検証すること。
- 不正な GOAWAY を受理せず、正当な receiver の GOAWAY と session 全体の GOAWAY は従来どおり動くこと。
- 0375 の移行処理とは独立した protocol role 検証として実装すること。

## 依存・関連

- 関連: [0375](0375-draft-22-request-stream-goaway-migration.md)
