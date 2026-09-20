# send_data_stream_closed の Reset でも保留 PUBLISH_DONE を flush する

- Created: 2026-09-13
- Completed: 2026-09-15
- Branch: feature/fix-flush-pending-publish-done-on-reset
- Polished: 2026-09-14

## 目的

REQUEST_UPDATE の失敗応答で保留された PUBLISH_DONE (UPDATE_FAILED) が、outgoing data stream を Reset で閉じた場合にも送信され、draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions) の MUST (REQUEST_UPDATE が失敗したら publisher は PUBLISH_DONE で subscription を終端する) を満たすようにする。

## 現状

`src/session/data.rs` の `Session::send_data_stream_closed` は、`RequestStreamEnd::Fin` のときだけ `Session::maybe_flush_pending_publish_done` を呼ぶ。`RequestStreamEnd::Reset` のときは呼ばないため、I/O 層が先に stream を reset して `send_data_stream_closed(Reset)` を直接呼ぶ経路では `Subscription::pending_publish_done` が残ったままになり、
PUBLISH_DONE が送られない。

- `reset_outgoing_data_stream_at_with_code` 経由では、`send_data_stream_closed(Reset)` の後に `ResetDataStream` イベントを push し、その後に `maybe_flush_pending_publish_done` を呼ぶため flush される。
- 一方、`examples/moqt-publisher/src/stream_writer.rs` の `finish` のように、アプリが `reset` した後に `send_data_stream_closed(Reset)` を直接呼ぶ経路では flush されない。
- 一時テストで確認した挙動: Established publisher の subscription で REQUEST_UPDATE 失敗応答 (`send_request_error`) の後に outgoing subgroup stream を `send_data_stream_closed(Reset)` で閉じても、`pending_publish_done` は `Some(1)` のまま残り、`SessionEvent::SendOnStream` の PUBLISH_DONE は発行されない (`Fin` で閉じた場合は発行される)。
- `send_data_stream_closed` の中で無条件に flush すると、`reset_outgoing_data_stream_at_with_code` 経由のときに `ResetDataStream` イベントより先に PUBLISH_DONE が push され、ワイヤ順序が PUBLISH_DONE → RESET_STREAM になって §9.9 (PUBLISH_DONE) の MUST NOT
  ("A sender MUST NOT send PUBLISH_DONE until it has closed all streams it will ever open, and has no further datagrams to send, for a subscription.") に反する。
  datagram の条件は既存実装の対象外 (datagram の終端はアプリが管理する) であり、本 issue では扱わない。

## 設計方針

- `send_data_stream_closed` の Reset 経路でも、その stream の終端確定後 (`data_streams.outgoing` からの除去後) に `maybe_flush_pending_publish_done` を呼ぶ。
- `send_data_stream_closed` は public API のまま Reset 終端でも flush し、`reset_outgoing_data_stream_at_with_code` の内部呼び出しだけが flush を抑止する。抑止の実装方法 (private helper への分離・フラグなど) は問わないが、抑止した場合は `ResetDataStream` イベントを push した後に改めて `maybe_flush_pending_publish_done` を呼び、RESET_STREAM → PUBLISH_DONE のワイヤ順序を維持する。
- flush の条件 (全 outgoing stream 終端 + Terminated + 保留あり) は既存の `maybe_flush_pending_publish_done` のまま変えない。ここでの「終端」はワイヤ上の RESET_STREAM ではなく `send_data_stream_closed` による session 追跡からの除去を指す。
- delivery timeout の tick 経路 (`tick_subscription_timeouts`) は stream を session 追跡から除去しないため、I/O 層がワイヤ RESET_STREAM を送った後に `send_data_stream_closed(Reset)` を呼ぶ契約である。`maybe_flush_pending_publish_done` の doc にある「アプリの終端通知まで保留される」はこの呼び出しを指し、doc の記述は変えない。
- FIN 経路の既存挙動は変えない。

## 完了条件

- `send_data_stream_closed(Reset)` で最後の outgoing stream を閉じた後、保留 PUBLISH_DONE が 1 回だけ自動送信されること
- `reset_outgoing_data_stream` / `reset_outgoing_data_stream_at` 経由では、保留 PUBLISH_DONE がある場合もイベント順が `ResetDataStream` → PUBLISH_DONE のままであること (この順序を固定するテストを `tests/test_session/subscription/request_update.rs` の既存 FIN 経路テストの近傍に追加する)
- delivery timeout の tick 経路でも、I/O 層が `send_data_stream_closed(Reset)` を呼んだ時点で flush 条件を満たせば PUBLISH_DONE が 1 回だけ送られること
- open 中の outgoing stream が残っている間は PUBLISH_DONE を送らないこと (§9.9 の MUST NOT) がテストで固定されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること

## 解決方法

`send_data_stream_closed` の Reset 経路でも保留 PUBLISH_DONE (UPDATE_FAILED) を flush するようにした。

- `src/session/data.rs`: `send_data_stream_closed` の内部処理を `close_outgoing_subgroup_stream` に切り出した。subgroup stream の追跡を終端し、flush 判断に使う request_id を `Option<u64>` で返す (flush はしない)。
- `src/session/data.rs`: public `send_data_stream_closed` は FIN / RESET のどちらでも「終端 → `maybe_flush_pending_publish_done`」を呼ぶ。
- `src/session/data.rs`: `reset_outgoing_data_stream_at_with_code` は「終端 → `ResetDataStream` イベント push → `maybe_flush_pending_publish_done`」の順序を保つ (ワイヤ順序 RESET_STREAM → PUBLISH_DONE を維持し、§9.9 の MUST NOT を満たす)。
- `src/session/data.rs`: これに伴い、`reset_outgoing_data_stream_at_with_code` にあった `contains_key` による重複 lookup と、到達不能だった未知 stream エラーの二重実装を削除した。
- `src/session/data.rs`: `send_fetch_data_stream_closed` (fill fetch stream の終端通知) が flush 契機にならない根拠を doc に明記した。
- `src/session/data.rs`: 保留の発生源である `send_request_error` の失敗応答は、subscription を Terminated にする際に `send_err_for_subscription` が §3.4.1 の MUST で open 中の fill fetch stream を先に reset する。
- `src/session/data.rs`: そのため保留中に open であり続ける stream は subgroup stream だけになる (Terminated 中は `send_fill_fetch_header` も拒否する)。この前提は `request_error_resets_open_fill_streams_before_publish_done` で固定した。
- `src/session/types.rs` / `src/session/core.rs` / `src/session/subscription/dispatch.rs`: 保留条件の記述を実装に合わせ、「open 中の outgoing data stream (subgroup / fill fetch)」に統一し、flush 契機として `recv_data_stream_stop_sending` も列挙した。
- `tests/test_session.rs`: SUBSCRIBE パラメータを指定して購読を確立する `establish_subscribe_track_with_params` を追加し、既存 `establish_subscribe_track` をその委譲にした (既存呼び出しのシグネチャは不変)。
- `tests/test_session/subscription/request_update.rs`: 回帰テスト 5 件を追加した。
  - `pending_publish_done_flushed_on_data_stream_closed_reset`: `send_data_stream_closed(Reset)` で最後の outgoing stream を閉じると PUBLISH_DONE (UPDATE_FAILED, fin 付き) が 1 回だけ送信され、保留情報が残らない (完了条件 1)
  - `pending_publish_done_flushed_once_after_last_stream_reset`: open 中の stream が残る間は送らず、最後の 1 本を閉じたときに 1 回だけ送る (§9.9 の MUST NOT。完了条件 4)
  - `reset_outgoing_data_stream_keeps_reset_before_publish_done`: `reset_outgoing_data_stream` と `reset_outgoing_data_stream_at` の両経路で、イベント順が `ResetDataStream` → `PUBLISH_DONE` のままであることと `_at` の `reliable_size` がイベントに乗ることを固定する (完了条件 2)
  - `pending_publish_done_flushed_after_delivery_timeout_reset`: delivery timeout の tick 経路では PUBLISH_DONE を送らず、I/O 層が `send_data_stream_closed(Reset)` を呼んだ時点で 1 回だけ送る (完了条件 3)
  - `request_error_resets_open_fill_streams_before_publish_done`: REQUEST_UPDATE 失敗応答で Terminated になるとき open 中の fill fetch stream が先に reset される前提を固定する回帰ガード
- 検証: `cargo test --workspace` (42 スイート) / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ることを確認した。
- 検証: 修正前の改訂で新規テストを実行すると、完了条件 1・3・4 に対応する 3 件が PUBLISH_DONE 未送信で失敗し、回帰ガード 2 件は通ることを実測した。
- `CHANGES.md` の `## develop` に `[FIX]` エントリを追加した。
