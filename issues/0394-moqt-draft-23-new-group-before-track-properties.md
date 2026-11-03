# Track Properties が未判明の NEW_GROUP_REQUEST を許可する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-new-group-before-track-properties
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.1、§9.19.19、§9.19.22、§10.6](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/session/types.rs` の `Subscription::dynamic_groups` は bool であり、未知と既知の非対応を区別できない。
`src/session/subscription/send.rs` の `send_request_update` 側検証は、この値が false なら NEW_GROUP_REQUEST を拒否する。
受信側にも DYNAMIC_GROUPS の検証がある。

## 設計方針

Track Properties の取得状況と、判明した DYNAMIC_GROUPS の値を区別する。
未判明なら NEW_GROUP_REQUEST を許し、Properties を受け取って非対応と判明した場合に送信を禁止する。
subscriber 側は Properties 受信前を未判明とする。`send_subscribe` で要求時の INCLUDE_PROPERTIES を保持し、0 を指定して空の Properties を受け取った場合も未判明を維持する。それ以外の Properties 受領では、DYNAMIC_GROUPS が 1 なら既知の対応、0 または欠落なら既知の非対応とする。
publisher 側は自身の Track Properties から対応状況を判断し、INCLUDE_PROPERTIES によって peer への提示を省略してもローカルの対応情報を失わない。
REQUEST_UPDATE の送信に Established subscription を要求する既存の状態条件は維持し、Pending からの更新送信は追加しない。
対応しない original publisher の受信時は同節の無視する規則を守る。

## 完了条件

- 未知・既知の対応・既知の非対応の 3 状態で REQUEST_UPDATE の送信可否を検証すること。
- INCLUDE_PROPERTIES=0 の空応答では未判明を維持し、通常の Properties 受領では値 1・値 0・欠落を区別して知識状態へ反映すること。初回 SUBSCRIBE の NEW_GROUP_REQUEST 後に Properties が判明する遷移も検証すること。
- SUBSCRIBE 初回の要求、受信側の無視、`crates/moq-pub/src/gop.rs` の `GopTracker::observe_keyframe` による新 Group 後の保留要求解除を保つこと。リレー用の outstanding 管理は追加しないこと。
