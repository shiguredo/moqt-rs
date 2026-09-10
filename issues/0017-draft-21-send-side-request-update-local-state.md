# 送信側 namespace / track subscription の REQUEST_UPDATE をローカル状態へ反映する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-send-side-request-update-local-state

## 目的

REQUEST_UPDATE で Track Namespace Prefix / FORWARD / Range Filter を変更したとき、送信側のローカル状態も更新し、以後に届く PUBLISH の紐付けや購読判定が正しく行われるようにする。

## 現状

`src/session/namespace/subscribe_namespace.rs` の `send_update_for_namespace_subscription` と `src/session/namespace/track_subscription.rs` の `send_update_for_track_subscription` は、パラメータの検証のみ行いローカル状態を更新しない。

受信側の `handle_update_for_namespace_subscription` / `handle_update_for_track_subscription` は `TRACK_NAMESPACE_PREFIX` / `FORWARD` / Range Filter をローカルへ反映する。そのため、送信側で prefix を更新すると `NamespaceSubscription::prefix` / `TrackSubscription::prefix` / `forward_state` が古いまま残る。

`src/session/core.rs` は受信 PUBLISH を `is_prefix_of(&ts.prefix, &track_namespace)` で購読へ紐付けるため、prefix 更新後に届いた PUBLISH が紐付かず `active_track_aliases` に入らない。`namespace_subscription()` は不変参照のみで、Application からも補正できない。

同じ送信経路の `send_update_for_subscription` (`src/session/subscription/send.rs`) は forward / filter / priority を楽観適用しており、非対称。

根拠 (draft-ietf-moq-transport-21 §9.5.2 / §9.20.21):

> "If the update is accepted, NAMESPACE and NAMESPACE_DONE messages following the REQUEST_OK will contain Track Namespace suffixes relative to the updated prefix."

## 設計方針

`send_request_update` の namespace / track subscription 分岐でも、受信側と同じく prefix / forward / Range Filter を検証後にローカルへ適用する。REQUEST_ERROR 時は request が Terminated になるため巻き戻しは不要。

## 完了条件

- 送信側で prefix / forward / Range Filter を更新した後、ローカル状態が追随すること
- prefix 更新後に届いた PUBLISH が正しく subscription へ紐付くこと
- 送信側の prefix 更新を検証するテストが `tests/test_session/namespace/` に追加されていること
