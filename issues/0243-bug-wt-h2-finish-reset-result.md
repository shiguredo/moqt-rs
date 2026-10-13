# WebTransport over HTTP/2 の finish / reset が driver の拒否を返さない

- Created: 2026-10-12
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-wt-h2-finish-reset-result
- Polished: 2026-10-13

## 目的

`WtH2SendStream::finish` / `WtH2SendStream::reset` は driver の拒否結果を受け取らないため、
peer が cancel (STOP_SENDING) したストリームへの FIN / RESET が無言で捨てられ、アプリには
成功として戻る。呼び出し元の `SubgroupWriter::finish` はその成功を受けて Session へ終端を通知する
ため、wt-h2 だけ Session の状態遷移が wt-h3 と変わる (0213 の完了条件「wt-h3 と同じ」の残差)。

## 現状

- `examples/tokio-moq/src/webtransport_h2.rs` の `WtH2SendStream::finish` は
  `Command::Send { reply: None }` を送るだけである。`Command::Send` は
  `reply: Option<oneshot::Sender<Result<()>>>` を持つが、`finish` は `None` を渡すため結果が
  返らない。
- driver の拒否判定は台帳だけではない
  (`if self.stop_sending_received.contains_key(&stream_id) || self.wt_session.stream(stream_id).is_none()`。
  driver のコメントも「FIN / reset / peer の STOP_SENDING 済みのストリームへの送信は受け付けない」
  と、台帳の有無だけを理由にしていない)。shiguredo_http2 の `WtSession` は `WT_STOP_SENDING` を
  受けると自動で `reset_stream` を呼び、送信専用 uni ストリームは送信側だけで closed になって
  台帳から消える。したがって cancel 済みのストリームは `wt_session.stream(stream_id)` が `None`
  になることで拒否される。台帳 (`DriverState::stop_sending_received`) はこの拒否の実体ではない。
- `WtH2SendStream::reset` が送る `Command::Reset` は reply フィールド自体を持たず、driver は
  `wt_session.reset_stream` の `Err` を `tracing::debug!` で捨てる。
- wt-h3 / QUIC 直接接続では送信 API が s2n-quic の `StreamError::StreamReset` を
  `TransportError::StreamReset` に写す。ただし `finish` が必ず失敗する保証はない (FIN ack 済みの
  ストリームでは現れない)。cancel の検知の唯一の経路はフレームの観測であり、
  `examples/tokio-moq/src/quic.rs` と `examples/tokio-moq/src/moqt_client.rs` の doc に明記されて
  いる。
- `examples/moq-pub/src/stream_writer.rs` の `SubgroupWriter::finish` は `self.stream.finish()?`
  の直後に `DataPlaneHandle::send_data_stream_closed(RequestStreamEnd::Fin)` を呼ぶ。wt-h2 では
  `finish` が `Ok` を返すため、peer が cancel 済みで実際には FIN が届いていないストリームを
  Session が終端済み (Fin) として記録する。
- `src/session/data.rs` の `recv_data_stream_stop_sending` は「I/O 層は STOP_SENDING の受信を
  終端通知より先に渡す契約」を前提にしている (doc に明記)。逆順ではストリームが unknown になり
  再オープン禁止 (`stopped_outgoing_subgroups`) を記録できない。この前提は cancel の観測が
  `MoqtClient::pump_once` 経由で届く 3 経路 (QUIC / wt-h3 / wt-h2) すべてに共通であり、
  wt-h2 固有ではない。
- 本 example は cancel 後に group id を進めるため draft-ietf-moq-transport-22 §11.3.2 (Closing
  Subgroup Streams) 違反には至らないが、wt-h2 だけ Session の記録が wt-h3 と一致しない。
- 0213 のレビューで挙動を doc に明記し、恒久対策を本 issue に分離した。

## 設計方針

- `finish` / `reset` を async 化して driver の reply を待つ案を採る。
  - 台帳を `WtH2SendStream` と共有する案は、driver の拒否判定が `wt_session.stream(stream_id)` の
    状態にも依存するため、driver が参照できる新情報を増やさない。判定と返答が別タスクに分かれる
    以上「捨てられたのに `Ok` が戻る」窓は残り、本 issue が直そうとしている偽の成功そのものが
    消えない。async 化すれば driver が判定と返答を同一処理で行うため、`Ok` は「FIN / RESET を
    `WtSession` へ渡した」ことを意味する。
  - 波及範囲: `examples/tokio-moq/src/transport.rs` の `SendStream::finish` / `reset`
    (`SendStream` は trait ではなく enum なので、async 化の漏れはコンパイラが検出する)、
    `examples/tokio-moq/src/webtransport_h2.rs` の `WtH2SendStream::finish` / `reset` と
    `Command::Reset` への reply 追加、`examples/tokio-moq/src/moqt_client.rs` (11 箇所)、
    `examples/tokio-moq/src/peer_harness.rs` (3 箇所)、
    `examples/moq-pub/src/stream_writer.rs` の `SubgroupWriter::finish` / `reset_by_session`、
    `examples/moq-pub/src/pipeline.rs` と `examples/moq-pub/src/catalog.rs` の呼び出し元、
    `examples/tokio-moq/src/webtransport_h2.rs` の既存テスト内の呼び出し 3 箇所。
  - `SubgroupWriter::reset_by_session` は `reset` の失敗で `?` 短絡するため、cancel 済み
    ストリームでは Session への終端通知に到達しない。これは Session 側の追跡終端の課題
    (下記の別 issue) であり、本 issue では挙動を変えない (wt-h3 / QUIC でも同じ形である)。
  - `examples/tokio-moq/src/webtransport_h2.rs` の `finish` / `reset` の doc は「driver の台帳に
    載っている」ことを拒否の理由として書いており、実際の拒否条件 (`wt_session.stream()` の不在
    を含む) と合っていない。実装時に doc を実装へ合わせる。
  - `finish` の reply は `pending_sends` の FIFO の最後に積まれるため、先行データが flow control で
    滞留している間は返らない。既存の `WtH2SendStream::send` と同じく、接続の終端を
    `wait_until_terminated` で観測しながら待つ。
  - `reset` の拒否は台帳を参照して `TransportError::StreamReset` にする。`StreamReset` が運ぶ
    error code は peer が STOP_SENDING に載せた値とする (`examples/tokio-moq/src/error.rs` の
    doc が「peer が載せた wire のエラーコード」と定めている)。
- `SubgroupWriter::finish` は `finish` の失敗時に Session へ Fin を通知しない。`?` の短絡で
  `send_data_stream_closed(RequestStreamEnd::Fin)` に到達しない形を維持する。
- cancel されたストリームの Session 側の後始末は Session の課題であり、本 issue では扱わない
  (別 issue にする)。`src/session/data.rs` の `recv_data_stream_stop_sending` は、終端通知が先に
  届いたストリームへの STOP_SENDING を unknown outgoing stream として拒否し、再オープン禁止
  (`stopped_outgoing_subgroups`) を記録しない (「I/O 層は STOP_SENDING の受信を終端通知より先に
  渡す契約」という doc がある)。したがって次の 2 つは本 issue の範囲外である。
  - 順序が入れ替わった場合にも再オープン禁止を記録する設計 (`recv_data_stream_stop_sending` の
    変更を伴う。cancel の観測は `MoqtClient::pump_once` 経由で届く 3 経路すべてに共通)
  - cancel で `finish` / `reset` が失敗したストリームの Session 追跡の終端。`finish` の失敗時に
    `send_data_stream_closed(RequestStreamEnd::Reset { .. })` を呼ぶ案は、この通知自体が
    `data_streams.outgoing` からエントリを消すため、後着の STOP_SENDING 観測が unknown になり
    再オープン禁止の記録を落とす。順序契約の解決と一体で扱う必要がある
    (`has_open_outgoing_data_streams_for_request` が真のまま残り PUBLISH_DONE の flush 条件が
    満たされない問題も、この別 issue で扱う)。
- 0244 (moq-pub の終了時の後始末) との関係: 本 issue の変更で cancel 経路でも `finish` / `reset` が
  `StreamReset` を返すようになる。0244 はセッション終了時の後始末 (`log_failure` による失敗の分類)
  を扱い、本 issue は cancel 経路の戻り値と `SubgroupWriter::finish` の短絡を扱う。対象が分かれて
  おり、完了条件も矛盾しない。cancel 後に shutdown して事後処理の `finish` が `StreamReset` を
  返す経路は 0244 が扱う (wt-h3 / QUIC でも既存の経路) ため、0244 と同時期に入れる。
- wt-h2 が experimental であることと capsule 実装の挙動は変えない。

## 完了条件

- peer が cancel したストリームへの `finish` / `reset` が `TransportError::StreamReset` を返す
  こと (driver が拒否した場合。wt-h3 が同じ状況で reset を報告する場合と同じ状態遷移になること)。
  `StreamReset` の error code は peer が STOP_SENDING に載せた値とする。
- driver が台帳 (`stop_sending_received`) に無い理由で拒否した場合 (FIN / RESET 送信済みの
  ストリーム) は `TransportError::StreamClosed` を返す。アプリはこれを成功として扱わない
  (`SubgroupWriter::finish` は `?` で短絡し、Fin を通知しない)。
- `SubgroupWriter::finish` が `finish` の失敗時に Session へ Fin を通知しないこと (Session の
  outgoing stream 追跡の終端と再オープン禁止の記録は下記の別 issue で扱う)。
- 完了条件 1 が `examples/tokio-moq/src/webtransport_h2.rs` の
  `wt_h2_stop_sending_terminates_only_the_stream` の拡張で固定されていること。
  `SubgroupWriter::finish` の分岐は `examples/moq-pub/src/stream_writer.rs` の doc が「`transport::
  SendStream` を要求する分岐そのものは example の単体テストから観測できない」と明記しているとおり
  単体テストでは観測できないため、`?` による短絡 (Fin を通知しないこと) をコードで担保し、
  レビューで確認する。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
