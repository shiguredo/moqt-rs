# PUBLISH の SUBSCRIBE_TRACKS_ID パラメータを扱う

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/add-moqt-draft-23-subscribe-tracks-id-parameter
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.6、§9.8、§9.19.21](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message_parameter.rs` の定数・`value_encoding` に SUBSCRIBE_TRACKS_ID がなく、`src/message.rs` の `PUBLISH_ALLOWED_PARAMS` にもない。
draft 23 の正当な PUBLISH にこの値が含まれると、未知パラメータとして codec で失敗する。

## 設計方針

Type `0x36` の varint パラメータと型付きアクセサを追加し、PUBLISH でのみ受理・送信する。
元の SUBSCRIBE_TRACKS の Request ID を保持・利用側に通知できるようにする。
ライブラリは SUBSCRIBE_TRACKS 自体を実装しないため、その要求との照合 state は追加しない。受信時は ID を関連付け情報として保持・通知し、対応する要求がないことだけを理由に拒否やセッション終了を行わず、通常の PUBLISH の検証とアプリケーションによる受理・拒否判断へ進める。
送信 API は型と PUBLISH のスコープを検証し、対応する namespace 要求の存在検証は追加しない。SUBSCRIBE_TRACKS 起因の PUBLISH にのみ指定する契約を API の説明へ明記する。

## 完了条件

- Type `0x36` の golden バイト列と PUBLISH の往復を検証すること。
- SUBSCRIBE / FETCH / REQUEST_UPDATE など許可されない文脈では INVALID_PARAMETER になること。
- 対応する namespace 要求がない場合も ID を保持・通知し、通常の PUBLISH の検証・受理判断へ進むこと。ID の照合 state や namespace 機能を追加しないこと。

## 依存・関連

- 関連: [0389](0389-moqt-draft-23-subscribe-tracks-parameter-scope.md)
