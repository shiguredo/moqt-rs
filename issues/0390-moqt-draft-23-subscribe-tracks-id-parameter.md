# PUBLISH の SUBSCRIBE_TRACKS_ID パラメータを扱う

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/add-moqt-draft-23-subscribe-tracks-id-parameter
- Polished: {YYYY-MM-DD}

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.6、§9.8、§9.19.21](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message_parameter.rs` の定数・`value_encoding` に SUBSCRIBE_TRACKS_ID がなく、`src/message.rs` の `PUBLISH_ALLOWED_PARAMS` にもない。
draft 23 の正当な PUBLISH にこの値が含まれると、未知パラメータとして codec で失敗する。

## 設計方針

Type `0x36` の varint パラメータと型付きアクセサを追加し、PUBLISH でのみ受理・送信する。
元の SUBSCRIBE_TRACKS の Request ID を保持・利用側に通知できるようにする。
ライブラリは SUBSCRIBE_TRACKS 自体を実装しないため、対応する要求を持たない場合の扱いを規定と現行の限定 endpoint 方針に沿って明記し、namespace 機能を追加しない。

## 完了条件

- Type `0x36` の golden バイト列と PUBLISH の往復を検証すること。
- SUBSCRIBE / FETCH / REQUEST_UPDATE など許可されない文脈では INVALID_PARAMETER になること。
- 対応する namespace 要求を送らない現行 endpoint の受信方針が明記されること。

## 依存・関連

- 関連: [0389](0389-moqt-draft-23-subscribe-tracks-parameter-scope.md)
