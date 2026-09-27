# examples が ResetDataStream を無視して保留 PUBLISH_DONE が flush されない

- Created: 2026-09-26
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-examples-ignore-reset-data-stream
- Polished: 2026-09-27

## 目的

`SessionEvent::ResetDataStream` は「I/O 層が `stream_id` に相当する uni data stream を RESET_STREAM で閉じる」指示である (`src/session/types.rs` のイベント doc)。delivery timeout 経路では Session は stream の追跡を残したまま本イベントを発行し、I/O 層がワイヤ RESET_STREAM を送った後に `send_data_stream_closed(Reset)` で終端を通知する契約である。examples は本イベントを無視しているため、Session が「開いている」とみなす stream が閉じず、`Subscription::pending_publish_done` の flush 条件 (全 outgoing stream の終端) が満たされずに PUBLISH_DONE が送られない経路がある。

## 現状

- `SessionEvent::ResetDataStream` は `examples/moqt-transport/src/moqt_client.rs` の `MoqtClient::drain_events` で無視されている (無視群に含まれ、「本 example は受信 data stream の reset を行わないため無視する」というコメント付き)。publisher / subscriber の両 example はこの `MoqtClient` を共有するため、どちらも処理していない。各 example のパイプラインが扱う `ClientEvent` にはそもそも `ResetDataStream` が到達しない (`is_notable_event` に含まれないため、`next_event` では返らない)
- 保留 PUBLISH_DONE が残る経路: 自側が REQUEST_UPDATE の失敗応答 (`send_request_error`) を送ると、open 中の outgoing stream がある場合 `Subscription::pending_publish_done` に保留される (`send_err_for_subscription`)。このとき delivery timeout の tick (`tick_subscription_timeouts`) が `ResetDataStream` を発行しても、Session は stream を追跡から除去しないため、I/O 層が `send_data_stream_closed(Reset)` を呼ぶまで flush 条件を満たさない。examples がイベントを無視すると永遠に flush されない。この契約は `tests/test_session/subscription/request_update.rs` の `pending_publish_done_flushed_after_delivery_timeout_reset` が固定している
- Session 側の flush 判定は `Subscription::pending_publish_done` と全 outgoing data stream の終端に依存する (`src/session/data.rs` の `Session::maybe_flush_pending_publish_done`)。delivery timeout 以外の `ResetDataStream` 発行経路 (`reset_outgoing_data_stream_*` / `reset_open_fill_streams` / FETCH の REQUEST_UPDATE 失敗 reset) は Session が追跡を除去済みであり、本 issue の対象外である
- 変更が必要なのは moqt-publisher 側のみである。moqt-subscriber は outgoing data stream (subgroup / fill fetch) を開かないため、delivery timeout 由来の `ResetDataStream` は発生しない

## 設計方針

- `MoqtClient::drain_events` で `ResetDataStream` を処理し、アプリ (publisher のパイプライン) へ届ける。イベントの `stream_id` / `error_code` / `reliable_size` をそのまま使う
- publisher は開いている subgroup stream の送信ハンドルを `stream_id` で保持し、該当 stream を RESET_STREAM で閉じた後に `DataPlaneHandle::send_data_stream_closed(stream_id, RequestStreamEnd::Reset { error_code, reliable_size })` で Session へ終端を通知する。既存の API で足りるため、Session 側の API 追加は行わない (現状 publisher のパイプラインは `SubgroupWriter` を現在の 1 本しか保持しないため、`stream_id` から writer を引く手段が別途必要になる)
- 終端通知は delivery timeout で発行された subgroup stream に対してのみ行う。Session が追跡を除去済みの経路 (アプリ発の `reset_outgoing_data_stream_*`、fill fetch stream の reset、FETCH の REQUEST_UPDATE 失敗 reset) に対して `send_data_stream_closed` を呼ぶと unknown stream id でエラーになるため、イベントの経路を見分けてワイヤ reset のみを行うこと
- `maybe_flush_pending_publish_done` が fill fetch stream の終端通知 (`send_fetch_data_stream_closed`) を契機にしない判断を壊さないこと。この契約は `src/session/data.rs` の doc と、`tests/test_session/subscription/request_update.rs` の `request_error_resets_open_fill_streams_before_publish_done` が固定している (fill fetch stream は保留設定時に Session が reset するため、fill の終端通知は flush 契機にならない)
- Session 側の flush 条件・ワイヤ順序 (`ResetDataStream` → PUBLISH_DONE) は 0073 で確立済みであり変更しない。`reset_outgoing_data_stream_keeps_reset_before_publish_done` と上記の delivery timeout テストがそのまま通ることを確認条件にする
- 実機確認: OBJECT_DELIVERY_TIMEOUT (または SUBGROUP_DELIVERY_TIMEOUT) を設定した SUBSCRIBE を送る subscriber で delivery timeout を発火させ、保留中の PUBLISH_DONE が届くことを `RUST_LOG=debug` で確認する。現行の moqt-subscriber には delivery timeout パラメータを設定する手段がないため、テスト用クライアント (`tests/` のフィクスチャを流用) か一時的な改変を使う

## 完了条件

- moqt-publisher が `ResetDataStream` (delivery timeout 由来) を受け取ったとき、該当 subgroup stream を RESET_STREAM で閉じ、`send_data_stream_closed(Reset)` で Session へ通知すること。その結果、保留中の PUBLISH_DONE が 1 回だけ flush されること
- 既存の Session 側テスト `pending_publish_done_flushed_after_delivery_timeout_reset` と `pending_publish_done_flushed_on_data_stream_closed_reset` がそのまま通ること (Session 側に変更を加えないことを含む)
- `MoqtClient` が `ResetDataStream` をアプリへ届けること、および publisher の `stream_id` から writer への照合と終端通知の順序 (ワイヤ RESET_STREAM → `send_data_stream_closed`) が、example 側の単体テストまたは上記実機確認で固定されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
