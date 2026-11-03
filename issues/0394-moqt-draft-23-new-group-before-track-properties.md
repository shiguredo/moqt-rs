# Track Properties が未判明の NEW_GROUP_REQUEST を許可する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-new-group-before-track-properties
- Polished: {YYYY-MM-DD}

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §9.19.19](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/session/types.rs` の `Subscription::dynamic_groups` は bool であり、未知と既知の非対応を区別できない。
`src/session/subscription/send.rs` の `send_request_update` 側検証は、この値が false なら NEW_GROUP_REQUEST を拒否する。
受信側にも DYNAMIC_GROUPS の検証がある。

## 設計方針

Track Properties の取得状況と、判明した DYNAMIC_GROUPS の値を区別する。
未判明なら NEW_GROUP_REQUEST を許し、Properties を受け取って非対応と判明した場合に送信を禁止する。
INCLUDE_PROPERTIES による省略と、既知のプロパティ集合に DYNAMIC_GROUPS がない場合を整理する。
対応しない original publisher の受信時は同節の無視する規則を守る。

## 完了条件

- 未知・既知の対応・既知の非対応の 3 状態で REQUEST_UPDATE の送信可否を検証すること。
- 未判明の要求を送った後に Properties が判明しても状態が破綻しないこと。
- SUBSCRIBE 初回の要求、受信側の無視、New Group 後の outstanding 解除を保つこと。
