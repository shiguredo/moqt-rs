# moq-sub の保留ストリームが自分の SUBGROUP_HEADER の受理を確認してから処理を進める

- Created: 2026-11-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-subscriber-pending-subscriber-notify
- Polished: {YYYY-MM-DD}

## 目的

moq-sub の保留中の Subgroup ストリームの 1 本が購読の確立を検出すると、`note_subscriber(track_alias)` で同じ alias の全 entry に通知する。
通知を受けた別ストリームの task は、自分の `recv_subgroup_header` が一度も `Accepted` になっていないまま `true` を返して処理を進める。
Session 側ではそのストリームが header 未受理の状態のままであり、続く Object を `recv_subgroup_object` へ渡すと `MALFORMED_OBJECT` のセッションエラーになり得る。各ストリームが自分の header の受理を確認してから処理を進めるようにする。

## 現状

- `crates/moq-sub/src/pipeline.rs` の `register_subgroup_header` は、保留ループの中で `data_plane.recv_subgroup_header(stream_id, header)` を再試行し、`Ok(TrackDataAcceptance::Accepted)` なら `note_subscriber(header.track_alias)` を呼んで次のループへ進む。
- ループの先頭は `take_ready_for(id, ..)` で自分の entry を引き取り、理由が `PendingNotifyReason::Subscriber` ならチャンクを decoder へ積み直して `true` を返す。
  同じ alias の別ストリームの task も、`note_subscriber` の通知によってループの先頭で `Subscriber` を引き取り、自分の `recv_subgroup_header` を `Accepted` にしないまま `true` を返す。
- `register_subgroup_header` の doc は「この example は受信ループを始める前にすべての `SUBSCRIBE_OK` を処理するため、受信ループの実行中に購読が確立する経路は無く `note_subscriber` を呼ぶ箇所はない」とする。
  しかし同じ関数が再試行の `Accepted` で `note_subscriber` を呼んでいる。また受信ループの中には request 単位の GOAWAY から呼ばれる `reissue_request` があり、受信ループの実行中に購読が確立し得る。
- `src/session/data.rs` の `take_incoming_subgroup_object` は、header 未受理 (`IncomingDataStream::AwaitingHeader`) の stream への Object を `SESSION_MALFORMED_OBJECT` で拒否する。
- `src/pending_subgroup_buffer.rs` の `note_subscriber` は alias 単位の通知であり、同じ alias の全 entry を引き取り待ちにする (購読の確立は alias 単位の事象であるため、ライブラリ側の契約は正しい)。

## 設計方針

- `register_subgroup_header` は `PendingNotifyReason::Subscriber` を引き取ったとき、`true` を返す前に自分のストリームで `recv_subgroup_header` を再試行し、`Accepted` を確認してからチャンクを decoder へ積み直す。
  `Accepted` 以外 (`Discarded` / `FilteredOut` / エラー) の場合は、保留ループの再試行と同じ扱い (abort / 破棄) にする。`UnknownTrackAlias` の場合は保留を続ける。
- ライブラリの `PendingSubgroupBuffer::note_subscriber` は変更しない。
- `register_subgroup_header` の doc の「受信ループの実行中に購読が確立する経路は無く」を実態に合わせる。

## 完了条件

- 保留中のストリームが、自分の `recv_subgroup_header` が `Accepted` になる前に decoder へチャンクを渡して処理を進めないこと。
- 同じ alias の複数ストリームが保留中に購読が確立した場合、各ストリームがそれぞれ自分の header の受理を確認してから処理を進めること。
- 変更はコード上で確認する (`crates/moq-sub/src/pipeline.rs` の `mod tests` には保留経路を駆動する基盤が無く、モック・スタブも使えないため)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
