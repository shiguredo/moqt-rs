# FORWARD を PAUSED に置き換えて状態の意味を反転する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-paused-parameter
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.1.1、§3.3.3、§9.19.18](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message_parameter.rs` の `PARAM_FORWARD` は `0x10`、`forward()` は 0 を停止、1 を配送として扱う。
`src/session/types.rs` の `Subscription::forward_state` と `src/session/subscription/validation.rs` の配送判定も同じ意味で、既定値は 1 である。

## 設計方針

`PAUSED` (Type `0x11`、uint8) に置き換え、0 を unpaused、1 を paused、既定値を 0 とする。
codec、送受信 API、REQUEST_UPDATE / PUBLISH_STATE_NOTIFY、fill 開設判定、STOP_SENDING 後の再開判定、publisher / subscriber の構築箇所を一貫して変更する。
旧 `0x10` を新しい意味で受け入れない。

## 完了条件

- 省略・0・1・範囲外の encode / decode と Session の状態遷移を検証すること。
- pause 中は Object を配送せず、再開で STOP_SENDING 後の許可条件が正しく成立すること。
- fill とライブ配送が PAUSED の同じ意味を使い、旧 FORWARD の API・ワイヤ値が残らないこと。
