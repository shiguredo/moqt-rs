# SUBSCRIBE_TRACKS stream 終端で PUBLISH stream の request_streams を消さない

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-subscribe-tracks-stream-end-request-streams

## 目的

SUBSCRIBE_TRACKS の bidi request stream が終端した後、その stream から確立した PUBLISH の bidi request stream が終端したときに、セッションを誤って閉じないようにする。

## 現状

`src/session/namespace/track_subscription.rs` の `close_track_subscription_on_stream_end` は、`active_track_aliases` に残る alias から `peer_publisher_aliases` を引き、PUBLISH 由来 subscription の状態を `Terminated` にしたうえで `self.request_streams.remove(&sub_request_id)` を呼ぶ。

しかし `request_streams` は、`forget_subscription` (`src/session/subscription.rs`) が行うように、stream close 通知を受理できるよう forget まで保持されるべき索引である。`close_track_subscription_on_stream_end` は subscription 本体を forget しないため、peer が PUBLISH stream を FIN / RESET すると、`recv_request_stream_closed`
(`src/session/core.rs`) が `request_streams` 未登録・`rejected_request_ids` 未登録として `PROTOCOL_VIOLATION` でセッションを閉じる。

再現手順:

1. SUBSCRIBE_TRACKS を確立する
2. その prefix に一致する PUBLISH を受信して subscription を確立する
3. SUBSCRIBE_TRACKS の bidi request stream を peer が終端する
4. PUBLISH の bidi request stream を peer が終端する
5. 手順 4 で `PROTOCOL_VIOLATION` によりセッションが閉じる

根拠 (draft-ietf-moq-transport-21 §6.4.2.2): FIN は「その方向に送るメッセージが終わった」ことを示すだけで request の cancel ではない。SUBSCRIBE_TRACKS の終端で PUBLISH stream の識別情報を失う根拠は無い。

当該シーケンスのテストは存在しない。

## 設計方針

PUBLISH 由来 subscription の `request_streams` entry は `forget_subscription` に到達するまで保持する。`close_track_subscription_on_stream_end` では状態遷移と alias 解放のみ行い、`request_streams` の削除は `forget_subscription` に一任する。あるいは PUBLISH stream 終端を吸収できるよう `rejected_request_ids` に登録する。`RequestTerminated`
の `kind` が実際の確立種別 (SUBSCRIBE 由来 / PUBLISH 由来) と一致するかも合わせて確認する。

## 完了条件

- 上記再現手順でセッションが閉じないこと
- 当該シーケンスの回帰テストが `tests/test_session/` に追加されていること
