# Track Properties が未判明の NEW_GROUP_REQUEST を許可する

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
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

## 解決方法

`Subscription::dynamic_groups` を bool から `DynamicGroupsSupport` (Unknown / Supported / Unsupported) へ変え、Track Properties の取得状況と判明した DYNAMIC_GROUPS の値を区別した。

- `DynamicGroupsSupport::from_local_properties` は publisher が発行する Track Properties から対応状況を求める。`from_peer_properties` は subscriber が受け取った Track Properties から求め、SUBSCRIBE で指定した INCLUDE_PROPERTIES が 0 で Track Properties が空のときは Unknown を保つ (draft-ietf-moq-transport-23 §9.19.22)。
- `send_subscribe` は送信した INCLUDE_PROPERTIES を `Subscription` に保持し、`handle_peer_subscribe_ok` が値 1 を Supported、値 0 と欠落を Unsupported として反映する。`handle_publish` / `send_publish` / `send_subscribe_ok` も Track Properties から対応状況を求める。
- `send_request_update` の NEW_GROUP_REQUEST 検証は `DynamicGroupsSupport::allows_new_group_request` (Unknown と Supported で許可、Unsupported で拒否) に変えた。Established subscription を要求する既存の状態条件は変えていない。
- 受信側は draft-ietf-moq-transport-23 §9.19.19 の「対応しない original publisher は NEW_GROUP_REQUEST を無視する」に従い、非対応と判明している Track への NEW_GROUP_REQUEST でセッションを閉じない。リレー用の outstanding 管理は追加していない。

## 完了条件の検証

テストは `tests/test_session/parameter_rules.rs` に追加・更新した。

| 完了条件 | 検証 |
| --- | --- |
| 未知・既知の対応・既知の非対応の 3 状態で REQUEST_UPDATE の送信可否を検証すること | `dynamic_groups_support_classifies_track_properties` で 3 状態の `allows_new_group_request` を、`subscribe_ok_track_properties_determine_new_group_request_permission` で実際の送信成否を検証した |
| INCLUDE_PROPERTIES=0 の空応答では未判明を維持し、通常の Properties 受領では値 1・値 0・欠落を区別して知識状態へ反映すること | `include_properties_zero_empty_response_keeps_dynamic_groups_unknown` と `subscribe_ok_track_properties_determine_new_group_request_permission` で検証した |
| 初回 SUBSCRIBE の NEW_GROUP_REQUEST 後に Properties が判明する遷移も検証すること | `new_group_request_transitions_from_unknown_to_supported` で Unknown から Supported への遷移と REQUEST_UPDATE の送信を検証した |
| SUBSCRIBE 初回の要求を保つこと | `subscribe_with_new_group_request_without_foreknowledge_accepted` を維持し、SUBSCRIBE 送信時点の状態が Unknown であることも検証した |
| 受信側の無視を保つこと | `request_update_with_new_group_request_for_unsupported_track_is_ignored` と `request_update_with_new_group_request_dynamic_groups_zero_is_ignored` で、セッションを閉じず更新として受理することを検証した |
| `GopTracker::observe_keyframe` による新 Group 後の保留要求解除を保つこと | `crates/moq-pub/src/gop.rs` の `observing_a_keyframe_satisfies_a_new_group_request` を含む既存テストが通ることを確認した (コードは変更していない) |
| リレー用の outstanding 管理を追加しないこと | 送信側は `send_request_update` の送信可否検証だけを変え、要求を追跡する状態を追加していない |
