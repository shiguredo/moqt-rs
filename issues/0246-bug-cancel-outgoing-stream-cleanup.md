# cancel された outgoing stream の順序契約と追跡終端

- Created: 2026-10-13
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-cancel-outgoing-stream-cleanup
- Polished: {YYYY-MM-DD}

## 目的

peer が STOP_SENDING で cancel した outgoing data stream について、0243 で wt-h2 の
`finish` / `reset` が driver の拒否 (`TransportError::StreamReset`) を返すようにした。
その結果、アプリが cancel を検知して Session へ終端を通知する経路と、cancel の観測
(`MoqtClient::pump_once` 経由の `recv_data_stream_stop_sending`) が Session へ届く経路の順序に
よって、Session の記録 (`stopped_outgoing_subgroups`) と open stream 追跡が食い違う。

## 現状

- `src/session/data.rs` の `recv_data_stream_stop_sending` は「I/O 層は STOP_SENDING の受信を
  終端通知より先に渡す契約」を前提にしている。終端通知 (`send_data_stream_closed`) が先に
  届いたストリームへの STOP_SENDING は unknown outgoing stream として拒否され、再オープン禁止
  (`stopped_outgoing_subgroups`) を記録しない。
- 逆に、cancel の観測を待たずにアプリが `send_data_stream_closed(RequestStreamEnd::Reset)` を
  呼ぶと、`data_streams.outgoing` からエントリが消えるため、後着の観測が unknown になり
  再オープン禁止の記録が落ちる。cancel の観測は 3 経路 (QUIC / wt-h3 / wt-h2) すべてが
  `MoqtClient::pump_once` 経由で届くため、アプリの構造 (単一タスクで `finish` を待つ等) によっては
  観測が必ず後着になる。
- 通知を省くと、`has_open_outgoing_data_streams_for_request` が真のまま残り、
  `maybe_flush_pending_publish_done` の PUBLISH_DONE flush 条件 (全 outgoing stream の終端) が
  満たされない。
- 0243 の完了条件からは外し、本 issue に分離した (0243 は wt-h2 の戻り値の欠陥に限定する)。

## 設計方針

- 終端済みの outgoing stream への STOP_SENDING でも再オープン禁止を記録できるようにする
  (終端時に `(request_id, track_alias, group_id, subgroup_id)` を一定期間保持する等)。
  `src/session/data.rs` の順序契約の doc と `tests/test_session/data_stream.rs` の
  `recv_data_stream_stop_sending_for_unknown_stream_does_not_close_session` を見直す。
- cancel で `finish` / `reset` が失敗したストリームの追跡終端の扱いを決める (アプリが通知するか、
  Session が cancel の観測で終端するか)。後者なら `examples/moq-pub` 側の `?` 短絡の挙動も
  そろえる。
- 3 経路 (QUIC / wt-h3 / wt-h2) で同じ状態遷移になることを完了条件にする。

## 完了条件

- 終端通知と STOP_SENDING の観測の到着順が入れ替わっても、再オープン禁止
  (`stopped_outgoing_subgroups`) が記録されること。
- cancel された outgoing stream の追跡が終端され、PUBLISH_DONE の flush 条件が満たされること。
- 上記が `tests/test_session/data_stream.rs` の単体テストで固定されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
