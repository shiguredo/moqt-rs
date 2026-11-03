# 空の Track Name を PROTOCOL_VIOLATION で拒否する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-nonempty-track-name
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §2.4.1、§6.5、§9.4.2.1](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message/common.rs` の `decode_track_name` / `encode_track_name` と `validate_full_track_name` は空名を許す。
SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS の各経路は長さの上限だけを検証し、session-level track の空名を旧 DOES_NOT_EXIST の扱いにする経路もある。

## 設計方針

Track を名指す要求と REDIRECT target で最低 1 バイトを要求し、受信の違反は PROTOCOL_VIOLATION、送信は状態登録前に拒否する。
namespace-scoped REDIRECT の Track Name は空でなければならないため、共有 decode 関数で一律に空を禁止せず文脈別に検証する。
Track Namespace が 0 フィールドであることは引き続き許す。

## 完了条件

- 通常 namespace と `.session` の空 Track Name を、wire と直接 Session API の双方で拒否すること。
- 1 バイトの名前と空 Track Namespace が正しく使えること。
- namespace-scoped REDIRECT の空名を維持し、非空名は INVALID_FIELD になること。
