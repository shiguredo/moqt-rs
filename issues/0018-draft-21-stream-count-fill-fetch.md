# subscriber 側の Stream Count 集計に fill fetch stream を含める

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-stream-count-fill-fetch
- Polished: 2026-09-10

## 目的

draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE) の Stream Count の定義に合わせ、subscriber 側の監視でも fill fetch stream を数える。overrun 判定の取りこぼしと、購読状態の早期破棄を防ぐ。

## 現状

publisher 側の `published_count` は `send_subgroup_header` と `send_fill_fetch_header` の両方で加算され、doc も「fill fetch stream を含む」と明記している。

一方 subscriber 側の `incoming_subgroup_count` / `open_incoming_subgroup_count` は subgroup しか数えない。受信 fill fetch stream は `src/session/data.rs` の `recv_fetch_header` の fill 分岐 (`self.fetches` に entry がないことで判別) で `IncomingDataStream::Fetch` として登録されるが、集計に加算されない。

このため次が起きる。

- `src/session/subscription/delivery.rs` の `record_publish_done` の `stream_count_overrun` 判定 (`incoming_subgroup_count > done.stream_count`) が fill を取りこぼす。publisher が fill 込みの実際数より少ない Stream Count を宣言しても検出できない
- `src/session/types.rs` の `cleanup_ready` が `open_incoming_subgroup_count == 0` のみを見るため、open 中の fill stream が残っていても drain 満了で subscription state を破棄しうる

受信 fill stream が `data_streams.incoming` から除去される経路は 2 つあり、どちらも減算がない。

- `recv_data_stream_closed` の Fetch 分岐: fill は `fetches` に entry がないため early return し、`recv_fetch_data_stream_closed` に到達しない
- `send_data_stream_stop_sending` の Fetch 分岐: 同じく early return して incoming から remove するだけになる。fill の独立 cancel はこの経路が正規 (draft-ietf-moq-transport-21 §3.4.1 "A subscriber can cancel a fill fetch stream independently using STOP_SENDING.")

根拠 (draft-ietf-moq-transport-21 §9.9):

> "Stream Count: An integer indicating the number of data streams the publisher opened for this subscription, including streams that contained no Objects ... and including any fill fetch streams"

> "Once the timer has expired, the receiver destroys subscription state once all open streams for the subscription have closed."

## 設計方針

- 受信 fill fetch stream を subscription の集計対象に含める。加算は `recv_fetch_header` の fill 分岐で行い、`note_incoming_subgroup_stream_opened` 相当の処理を通す。関数名と `StreamCountState` のフィールド doc を fill を含む実態に合わせて更新する (フィールド名の変更は任意、doc は必須)。
- 減算は受信 fill stream が `data_streams.incoming` から除去される全経路で行う。
  - `recv_data_stream_closed` の Fetch 分岐: fill (`self.fetches` に entry がない) を判別し、early return より前に減算する
  - `send_data_stream_stop_sending` の Fetch 分岐: 同じく fill を判別して減算する。通常の FETCH 応答 stream では減算しない (request id の数値衝突で無関係な subscription のカウンタを減らさない)
- fill と通常 FETCH 応答の判別は現行と同じく `self.fetches` に entry があるかで行う。`IncomingDataStream::Fetch` にフラグを追加する場合は、`fetch_cleanup_ready` / `has_other_fetch_stream` など同 variant を match する箇所の追従が必要になるため変更対象に含める。
- `cleanup_ready` の open 判定を fill の open 数を含む形にする。`SubscriptionPublishDone::stream_count_overrun` の doc も「fill fetch stream を含む」に更新する。
- 対象外: publisher 側の `published_count` は既に対応済み。fill のオブジェクト完了検出や `forget_subscription` の閾値は変更しない。

## 完了条件

- 受信 fill fetch stream の open / close が Stream Count 集計と `cleanup_ready` の open 判定に含まれること
- fill stream を FIN / RESET で閉じた場合と `send_data_stream_stop_sending` で cancel した場合の両方で open 数が減り、drain 満了後に `cleanup_ready` が true になること
- publisher が fill 込みの実際数より少ない Stream Count を宣言した場合に `stream_count_overrun` が true になり、正しい exact 数なら false のままであること
- fill fetch stream を含む回帰テストが `tests/test_session/subscription/` に追加され、`cargo test --workspace` が通ること
