# subscriber 側の Stream Count 集計に fill fetch stream を含める

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-stream-count-fill-fetch

## 目的

draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE) の Stream Count の定義に合わせ、subscriber 側の監視でも fill fetch stream を数える。overrun 判定の取りこぼしと、購読状態の早期破棄を防ぐ。

## 現状

publisher 側の `published_count` は `send_subgroup_header` と `send_fill_fetch_header` の両方で加算され、doc も「fill fetch stream を含む」と明記している。

一方 subscriber 側の `incoming_subgroup_count` / `open_incoming_subgroup_count` は subgroup しか数えない。受信 fill fetch stream は `IncomingDataStream::Fetch` として扱われるため集計に入らない (`src/session/data.rs` の `note_incoming_subgroup_stream_opened` 等)。

このため次が起きる。

- `src/session/subscription/delivery.rs` の `record_publish_done` の `stream_count_overrun` 判定が fill 分を取りこぼす
- `src/session/types.rs` の `cleanup_ready` が open 中の fill fetch stream を無視し、drain 満了時に subscription state を破棄しうる

根拠 (draft-ietf-moq-transport-21 §9.9):

> "Stream Count: An integer indicating the number of data streams the publisher opened for this subscription, including streams that contained no Objects ... and including any fill fetch streams"

> "Once the timer has expired, the receiver destroys subscription state once all open streams for the subscription have closed."

## 設計方針

受信 fill fetch stream を subscription の集計対象に含める。`recv_fetch_header` の fill 分岐で加算し、`recv_data_stream_closed` の Fetch 分岐で減算する。`cleanup_ready` の open 判定に open 中の受信 fill stream を含める。

## 完了条件

- 受信 fill fetch stream が Stream Count 集計と open 判定に含まれること
- fill fetch stream を使う購読の PUBLISH_DONE で overrun 誤判定が起きないこと
- fill fetch stream を含む回帰テストが `tests/test_session/subscription/` に追加されていること
