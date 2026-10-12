# WebTransport over HTTP/2 の finish / reset が driver の拒否を返さない

- Created: 2026-10-12
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-wt-h2-finish-reset-result
- Polished: {YYYY-MM-DD}

## 目的

`WtH2SendStream::finish` / `WtH2SendStream::reset` は driver の拒否結果を受け取らないため、
peer が cancel (STOP_SENDING) したストリームへの FIN / RESET が無言で捨てられ、アプリには
成功として戻る。wt-h3 と QUIC 直接接続では同じ操作が `TransportError::StreamReset` で失敗する
ため、cancel 後の Session の状態遷移が経路によって変わる (0213 の完了条件「wt-h3 と同じ」の
残差)。

## 現状

- `examples/tokio-moq/src/webtransport_h2.rs` の `WtH2SendStream::finish` は
  `Command::Send { reply: None }` を、`reset` は `Command::Reset` を送るだけである。driver の
  `Command::Send` は台帳 (`DriverState::stop_sending_received`) に載ったストリームへの送信を
  拒否するが、`reply` が無いため結果は返らない (`Command::Reset` も同様)。
- wt-h3 / QUIC 直接接続では、送信 API が s2n-quic の `StreamError::StreamReset` を
  `TransportError::StreamReset` に写して返すため、アプリは cancel を検知できる。
- `examples/moq-pub/src/stream_writer.rs` の `SubgroupWriter::finish` は `self.stream.finish()?`
  の直後に `DataPlaneHandle::send_data_stream_closed(RequestStreamEnd::Fin)` を呼ぶ。wt-h2 では
  `finish` が `Ok` を返すため、peer が cancel 済みで実際には FIN が届いていないストリームを
  Session が終端済み (Fin) として記録する。
- `src/session/data.rs` の `recv_data_stream_stop_sending` は「I/O 層は STOP_SENDING の受信を
  終端通知より先に渡す契約」を前提にしている。wt-h2 では cancel の観測が
  `MoqtClient::pump_once` 経由で Session へ届くため、アプリの終端通知が先に処理されると
  ストリームが unknown になり、再オープン禁止 (`stopped_outgoing_subgroups`) が記録されない。
- 本 example は cancel 後に group id を進めるため §11.3.2 違反には至らないが、Session の
  記録は wt-h3 と一致しない。0213 のレビューで挙動を doc に明記し、恒久対策を本 issue に分離した。

## 設計方針

- driver の処理済み状態を `finish` / `reset` から同期的に参照できるようにする案と、
  `finish` / `reset` を async 化して driver の reply を待つ案を比較して決める。
  - 台帳を共有する案は `WtH2SendStream` に参照を持たせる必要があり、確認と driver の処理の間に
    cancel が届く残差レースが残る。
  - async 化する案は `SendStream::finish` / `reset` と `SubgroupWriter` の呼び出し側に波及するが、
    driver の拒否を確実に返せる。
- cancel の観測 (`take_stop_sending_receiver` → `MoqtClient::handle_stop_sending`) が Session へ
  届く順序と、アプリの終端通知の順序の契約を整理する。どちらが先でも再オープン禁止が記録される
  形にするか、順序を保証できる設計にする。
- wt-h2 が experimental であることと capsule 実装の挙動は変えない。

## 完了条件

- peer が cancel したストリームへの `finish` / `reset` が `TransportError::StreamReset` を返し、
  `SubgroupWriter::finish` が Session へ Fin を通知しないこと (wt-h3 と同じ状態遷移になること)。
- cancel 後にアプリの終端通知と観測の到着順が入れ替わっても、再オープン禁止が記録されること。
- 上記が単体テストで固定されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
