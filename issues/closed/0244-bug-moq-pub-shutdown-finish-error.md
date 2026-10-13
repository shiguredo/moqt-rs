# moq-pub の終了時の subgroup stream 後始末が Fatal 終了する

- Created: 2026-10-12
- Completed: 2026-10-13
- Branch: feature/fix-moq-pub-shutdown-finish-error
- Polished: 2026-10-13

## 目的

moq-pub のメインループを抜けた後の映像 subgroup stream の後始末が `?` でエラーを伝播するため、
セッション終了時や Ctrl+C での終了時に `Fatal:` と `exit(1)` で終了する。同ファイルの方針
(セッション終了や peer の cancel に伴う失敗は info、それ以外は warn に落として後始末を続ける)
から外れており、正常な終了経路でも異常終了として扱われる。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `run` はメインループを抜けた後、
  `current_video_writer.take()` した writer の `SubgroupWriter::finish` を `?` で呼ぶ。この `?` は
  `is_transport_session_end` / `is_peer_stream_reset` の分類を通らないため、
  `examples/moq-pub/src/main.rs` の `Fatal: {e}` と `std::process::exit(1)` に至る。
- `SubgroupWriter::finish` は `SendStream::finish` / `reset` と
  `DataPlaneHandle::send_data_stream_closed` を呼ぶ (0243 で async 化され、peer が cancel 済みの
  ストリームでは `TransportError::StreamReset`、送信方向を閉じたストリームでは
  `TransportError::StreamClosed` を返すようになった)。
- セッション終了後に返るエラーの variant は経路ごとに異なる。
  - wt-h2: driver の終了で送信チャネルが閉じ、`TransportError::ConnectionClosed` になる。
  - wt-h3: `finish` は同期実装で、`StreamReset` または `TransportError::Transport` になる。
  - QUIC 直接接続: 送信エラーは `TransportError::Quic` に畳まれ、接続クローズを variant では
    判定できない (`issues/closed/0174-bug-expected-session-end-warn-logs.md` に既知の限界として
    明記されている)。
  したがって接続クローズの variant だけで分類できるのは wt-h2 だけで、info 判定の主役は
  後述の `session_ended` である。
- メインループを抜ける経路はセッション終了だけではない。`session_ended` を真にして抜ける経路
  (映像 / 音声の transport 終了、GOAWAY / CloseSession の受信) のほか、Ctrl+C
  (`shutdown_monitor`) と入力チャネルの終了でも後始末に到達する (このとき `session_ended` は
  偽のままである)。
- `current_video_writer` が `None` になるのは、peer の cancel 経路・キーフレーム時の `take()`・
  `reset_open_subgroup_stream` (Session の reset 指示) である。したがって後始末で `finish` が
  必ず呼ばれるとは限らない (呼ばれない場合は対象が無いだけで問題ではない)。
- peer が cancel した後に Ctrl+C / 入力終了でループを抜けた場合、後始末の `finish` は 0243 の
  変更により `StreamReset` を返す (cancel 由来の失敗は `StreamReset` に限る。0213 の設計方針どおり
  新規 variant は追加しない)。この経路は 0243 が本 issue に委譲した (`session_ended` は偽、
  接続クローズの variant でもない)。
- 同ファイルの送信経路の doc は「セッション終了後に `?` で `run` まで通すと
  `Fatal: session closed` で異常終了し、終了時の後始末に到達しない」と明記しており、本箇所は
  その方針に反している。
- セッション終了時に映像を配信中だと `current_video_writer` は `Some` のまま残るため、後始末で
  `finish` が呼ばれる。0213 のレビューで見つけた経路であり、0213 の修正 (peer cancel で example が
  終了しない) とは別の原因で example が終了する。

## 設計方針

- 後始末の `SubgroupWriter::finish` の失敗を `?` で伝播させず、`log_failure` で分類する。述語は
  `Error` を取る `is_transport_session_end` を使う (`is_connection_closed` は `TransportError`
  専用で、`SubgroupWriter::finish` が返す `Error` には適用できない)。
- 期待する失敗 (info) の範囲を、ループ内の分類と同じ規則にする。
  `session_ended || is_transport_session_end(&e) || is_peer_stream_reset(&e)`。
  0243 が委譲した cancel 後の `StreamReset` は peer による該当ストリームの終端であり
  (draft-ietf-moq-transport-22 §6.4.2.3)、ループ内でも致命エラーにしていないため info にする。
  それ以外は warn にする。FIN / RESET 送信済みで返る `StreamClosed` は `Error` に variant が
  無く (`WebTransport` に文字列として畳まれる)、peer の cancel ではないため期待する失敗には
  数えず warn のままにする。
- 終了コードはプロセスの exit code を指す。`?` を外すことでセッション終了 / Ctrl+C の経路は
  `run` が `Ok(())` を返し、`main.rs` の `Fatal:` / `exit(1)` を通らない (終了コード 0)。warn と
  判定した失敗でも終了コードは 0 のままにする。`run` の戻り値は後始末の最後で MP4 リーダーの
  停止結果も合成するため、その異常終了 (通常は到達しない) は従来どおり exit 1 のままである。
  `main.rs` の終了コードと `MoqtClient::close` に渡す値は変更しない。
- 分類の述語を純関数として抽出し、真理値表を `#[cfg(test)]` で固定する (例:
  `cleanup_finish_is_expected(session_ended: bool, error: &Error) -> bool`)。実際に `run` の後始末を
  通す検証は実 relay が必要で、`examples/tokio-moq` の peer harness は `moq-pub` から使えないため、
  0243 の前例に倣いレビューで確認する。
- セッション終了の検知と後始末の順序は変えない。

## 完了条件

- セッション終了時、および peer の cancel 後に Ctrl+C / 入力終了でループを抜けたときに、映像
  subgroup stream の `finish` が失敗しても example が `Fatal:` で終了せず (終了コード 0)、
  `log_failure` の分類を残して後始末を完了すること。期待する失敗 (`session_ended`、接続クローズ、
  peer の cancel) は info になること。
- 後始末の分類が純関数として抽出され、その真理値表が `#[cfg(test)]` で固定されていること。
  実際に `run` の後始末を通す検証は実 relay が必要なため、0243 の前例に倣いレビューで確認する。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。

## 解決方法

- `examples/moq-pub/src/pipeline.rs` のメインループ後の後始末で、`current_video_writer` の
  `SubgroupWriter::finish` の失敗を `?` で伝播させず、`log_failure` で分類するようにした
  (`Failed to finish video subgroup stream: {e}`)。これによりセッション終了 / Ctrl+C / 入力終了の
  どの経路でも後始末 (PUBLISH_DONE ×3 / GOAWAY / close) まで到達し、`main.rs` の `Fatal:` と
  `exit(1)` を通らない (終了コード 0)。
- 分類の純関数 `cleanup_finish_is_expected(session_ended: bool, error: &Error) -> bool` を追加した
  (`session_ended || is_transport_session_end(error) || is_peer_stream_reset(error)`)。述語は
  `Error` を取る `is_transport_session_end` を使う (`is_connection_closed` は `TransportError` 専用
  で、この呼び出しのエラー型と合わない)。peer の cancel は 0243 が本 issue に委譲した経路で、
  ループ内の分類と同じく info にする。FIN / RESET 送信済みのストリームが返す
  `TransportError::StreamClosed` は `Error` に variant が無く `Error::WebTransport` に畳まれるため
  本判定では拾えず、セッション終了を観測していない経路では warn のままになる (peer の cancel では
  ないため期待する失敗に数えない)。
- 真理値表のテスト `cleanup_finish_is_expected_matches_the_truth_table` を追加した (セッション終了
  の有無 × 接続クローズ / peer の cancel / `StreamClosed` / その他)。実際に `run` の後始末を通す
  検証は実 relay が必要で、`examples/tokio-moq` の peer harness は `moq-pub` から使えないため
  0243 の前例に倣いレビューで確認する。変異テストで、述語の各項を落とす変更と常に真 / 偽にする
  変更が検出されることを確認した。
- `finish` が失敗した経路では `send_data_stream_closed` が呼ばれないため Session の outgoing
  stream 追跡が残り、直後の映像の PUBLISH_DONE は `publish_done requires all outgoing streams
  closed` で拒否される (draft-ietf-moq-transport-22 §9.9 の MUST NOT)。追跡の終端は cancel の観測と
  終端通知の到着順に依存するため本 issue では扱わないことを、コードのコメントに記録した。
- `examples/moq-pub/src/main.rs` の終了コードと `MoqtClient::close` に渡す値は変更していない。
  `run` の戻り値は後始末の最後で MP4 リーダーの停止結果も合成するため、その異常終了
  (通常は到達しない) は従来どおり exit 1 のままである。
- セッション終了の検知と後始末の順序は変えていない。
- 検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` の通過と、`/review-diff-code` 3 周 (致命的 0 件) で行った。
