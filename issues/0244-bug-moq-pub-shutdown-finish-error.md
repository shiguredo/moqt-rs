# moq-pub の終了時の subgroup stream 後始末が Fatal 終了する

- Created: 2026-10-12
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-moq-pub-shutdown-finish-error
- Polished: {YYYY-MM-DD}

## 目的

moq-pub のメインループを抜けた後の映像 subgroup stream の後始末が `?` でエラーを伝播するため、
relay からのセッション終了時に `Fatal:` と `exit(1)` で終了する。同ファイルの方針
(セッション終了に伴う失敗は info、それ以外は warn に落として後始末を続ける) から外れており、
正常な終了経路でも異常終了として扱われる。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `run` はメインループを抜けた後、
  `current_video_writer.take()` した writer の `SubgroupWriter::finish` を `?` で呼ぶ。
  この `?` は `is_transport_session_end` / `is_peer_stream_reset` の分類を通らないため、
  `examples/moq-pub/src/main.rs` の `Fatal: {e}` と `std::process::exit(1)` に至る。
- `SubgroupWriter::finish` は `SendStream::finish` / `reset` を呼ぶ。セッション終了後は
  driver の終了で送信チャネルが閉じる (wt-h2) か、接続が閉じている (QUIC / wt-h3) ため、
  これらの送信は `TransportError::ConnectionClosed` などで失敗する。
- 同ファイルの送信経路の doc は「セッション終了後に `?` で `run` まで通すと
  `Fatal: session closed` で異常終了し、終了時の後始末に到達しない」と明記しており、
  本箇所はその方針に反している。
- セッション終了時に映像を配信中だと `current_video_writer` は `None` にならない
  (`None` にするのは cancel 経路とキーフレーム時の `take()` のみ) ため、事後処理で
  `finish` が必ず呼ばれる。到達条件は relay からのセッション終了であり、peer の cancel
  単独では到達しない。
- 0213 のレビューで見つけた経路であり、0213 の修正 (peer cancel で example が終了しない)
  とは別の原因で example が終了する。

## 設計方針

- 事後処理の `SubgroupWriter::finish` の失敗を `?` で伝播させず、他の後始末と同じく
  `log_failure` で分類する (`session_ended || is_connection_closed(&e)` を info、それ以外を
  warn)。終了コードはセッション終了に応じた値にする。
- 同じ事後処理にある他の writer (`DatagramWriter` など) と扱いを揃える。
- セッション終了の検知と後始末の順序は変えない。

## 完了条件

- relay からのセッション終了時に、映像 subgroup stream の `finish` が失敗しても example が
  `Fatal:` で終了せず、info または warn を残して後始末を完了すること。
- 上記がテストで固定されていること (`session_ended` と接続クローズの分類を検証する)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
