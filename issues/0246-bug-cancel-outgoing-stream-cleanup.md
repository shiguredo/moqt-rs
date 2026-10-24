# cancel された outgoing stream の順序契約と追跡終端

- Created: 2026-10-13
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-cancel-outgoing-stream-cleanup
- Polished: 2026-10-25

## 目的

peer が STOP_SENDING で cancel した outgoing data stream について、0243 で wt-h2 の
`finish` / `reset` が driver の拒否 (`TransportError::StreamReset` / `StreamClosed`) を返すようにした。
その結果、アプリが cancel を検知して Session へ終端を通知する経路と、cancel の観測
(`MoqtClient::pump_once` 経由の `recv_data_stream_stop_sending`) が Session へ届く経路の順序に
よって、Session の記録 (`stopped_outgoing_subgroups`) と open stream 追跡が食い違う。

## 現状

- `src/session/data.rs` の `recv_data_stream_stop_sending` は「I/O 層は STOP_SENDING の受信を
  終端通知より先に渡す契約」を前提にしている。実際の I/O 層 (`examples/tokio-moq/src/moqt_client.rs`
  の `handle_stop_sending`) は「STOP_SENDING の観測と、RESET_STREAM / FIN による終端通知の順序は
  保証されない (別経路から届く)」と明記しており、終端通知 (`send_data_stream_closed`) が先に届いた
  ストリームへの STOP_SENDING は unknown outgoing stream として拒否され、再オープン禁止
  (`stopped_outgoing_subgroups`) を記録しない。
- 逆に、cancel の観測を待たずにアプリが `send_data_stream_closed(RequestStreamEnd::Reset)` を
  呼ぶと、`data_streams.outgoing` からエントリが消えるため、後着の観測が unknown になり
  再オープン禁止の記録が落ちる。cancel の観測は QUIC / wt-h3 / wt-h2 / QMux の全経路が
  `MoqtClient::pump_once` 経由で届く (`examples/tokio-moq/src/quic.rs` / `webtransport_h3.rs` /
  `webtransport_h2.rs` / `qmux.rs` が `StopSendingReceiver` を `MoqtClient` に渡す) ため、
  アプリの構造 (単一タスクで `finish` を待つ等) によっては観測が後着になる。
- `examples/moq-pub` は cancel を `SubgroupWriter::write_object` / `finish` / `reset_by_session` の
  失敗 (`TransportError::StreamReset` / `StreamClosed`) で検知する。`write_object` の失敗経路
  (`examples/moq-pub/src/pipeline.rs` の `is_peer_stream_reset` 分岐) は writer を捨てるだけで
  `send_data_stream_closed` を呼ばない。`finish` / `reset_by_session` も失敗時に `?` で短絡して
  通知に到達しない (`examples/moq-pub/src/stream_writer.rs`)。したがって通知を省くと、
  `has_open_outgoing_data_streams_for_request` が真のまま残り、保留中の PUBLISH_DONE がある場合に
  `maybe_flush_pending_publish_done` の PUBLISH_DONE flush 条件 (全 outgoing stream の終端) が
  満たされない。
- 0243 の完了条件からは外し、本 issue に分離した (0243 は wt-h2 の戻り値の欠陥に限定する)。

## 設計方針

- 終端済みの outgoing subgroup stream への STOP_SENDING でも再オープン禁止を記録できるようにする。
  `src/session/data.rs` の `close_outgoing_subgroup_stream` で stream_id ごとに
  `(request_id, track_alias, group_id, subgroup_id)` を保持し、`recv_data_stream_stop_sending` が
  `data_streams.outgoing` に無い場合にこの保持情報を参照して `stopped_outgoing_subgroups` へ記録する。
  あわせて、既存の open stream 経路と同じく `my_subgroups.mark_stop_sending` を呼ぶ
  (`subgroup_id` が `Some` の場合)。保持は Forward State 0→1 の REQUEST_UPDATE 受理
  (`src/session/subscription/dispatch.rs`) と `forget_subscription`
  (`src/session/subscription.rs`) で `stopped_outgoing_subgroups` と同時に破棄する。これにより
  保持期間は既存の再オープン禁止エントリと同じ寿命になる。
- 保持情報が残っている終端済みの stream への STOP_SENDING は `Ok(())` で吸収する。一度も開いて
  いない stream id への STOP_SENDING は従来どおり `SESSION_PROTOCOL_VIOLATION` を返し、
  セッションを閉じない (`tests/test_session/data_stream.rs` の
  `recv_data_stream_stop_sending_for_unknown_stream_does_not_close_session` の契約を維持する)。
- 順序契約の doc を実装に合わせる。`src/session/data.rs` の `recv_data_stream_stop_sending` の
  「I/O 層は STOP_SENDING の受信を終端通知より先に渡す契約」を「順序は保証されない。終端通知が
  先に届いた場合は保持情報から再オープン禁止を記録する」に改める。`tests/test_session/data_stream.rs`
  の `recv_data_stream_stop_sending_for_unknown_stream_does_not_close_session` の doc も同じ前提に
  合わせる。
- cancel で `SubgroupWriter::write_object` / `finish` / `reset_by_session` が
  `TransportError::StreamReset` または `TransportError::StreamClosed` で失敗したストリームの
  追跡終端は、アプリが `send_data_stream_closed(RequestStreamEnd::Reset)` で通知する。
  `examples/moq-pub/src/stream_writer.rs` の各メソッドで、失敗を返す前に
  `DataPlaneHandle::send_data_stream_closed` を呼ぶ。失敗経路で渡す `RequestStreamEnd::Reset` の
  `error_code` / `reliable_size` は `None` とし、後着の STOP_SENDING 観測が
  `stopped_outgoing_subgroups` に remap 後のコードを入れる。通知自体の失敗 (セッション終了など) は
  無視し、元の transport エラーを返す。これにより `examples/moq-pub/src/pipeline.rs` の
  `is_peer_stream_reset` 分岐が writer を捨てる経路でも追跡が終端され、`?` 短絡で通知に到達しない
  現状を解消する。
- Session が cancel の観測で追跡を終端する案は採らない。`recv_data_stream_stop_sending` の時点で
  アプリがまだ当該 stream へ `send_subgroup_object` を呼びうるため、open 集合から即時に除去すると
  未知 stream id のエラーになる (現行 doc の「local endpoint がまだ FIN / RESET を流していない
  可能性があるため、open stream 集合からは即時には除去しない」を維持する)。アプリは自分の
  writer の終端操作で追跡を終端する。

## 完了条件

- 終端通知と STOP_SENDING の観測の到着順が入れ替わっても、再オープン禁止
  (`stopped_outgoing_subgroups`) が記録されること。STOP_SENDING が先の順序と
  `send_data_stream_closed` が先の順序の両方を `tests/test_session/data_stream.rs` の単体テストで
  固定する。Forward 0→1 の REQUEST_UPDATE 受理後は再オープンできることもあわせて固定する。
- 保持情報が残っている終端済みの stream への STOP_SENDING は `Ok(())` で吸収され、一度も開いて
  いない stream id への STOP_SENDING は `SESSION_PROTOCOL_VIOLATION` のままセッションを閉じない
  こと。
- cancel された outgoing stream の追跡が終端され、保留中の PUBLISH_DONE がある場合に
  `maybe_flush_pending_publish_done` の flush 条件が満たされること。`send_data_stream_closed(Reset)`
  後に PUBLISH_DONE が送信されることを `tests/test_session/data_stream.rs` /
  `tests/test_session/subscription/request_update.rs` の単体テストで固定する。
- `examples/moq-pub/src/stream_writer.rs` の `write_object` / `finish` / `reset_by_session` の
  `TransportError::StreamReset` / `StreamClosed` の失敗経路で `send_data_stream_closed(Reset)` が
  呼ばれること (通知の失敗は無視し、元の transport エラーを返す)。`transport::SendStream` を要求する
  分岐は example の単体テストから観測できないため、コードで担保しレビューで確認する
  (0243 の `SubgroupWriter::finish` と同じ扱い)。
- QUIC / wt-h3 / wt-h2 / QMux の各経路で同じ Session 状態遷移になること。Session の状態遷移は
  トランスポート非依存であり、I/O 層の観測は `MoqtClient::pump_once` に集約されている。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
