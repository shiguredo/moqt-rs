# PendingSubgroupBuffer の stream 終端通知を entry 単位にする

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-pending-subgroup-end-of-stream
- Polished: 2026-10-20

## 目的

`PendingSubgroupBuffer` の entry は Subgroup ストリーム 1 本に対応し、同じ Track Alias に複数の entry が
並び得る。現状の `note_end_of_stream` は Track Alias 単位で、理由が未設定の全 entry を終端扱いに
するため、1 本の stream の FIN が、まだ開いている同じ alias の別 Subgroup stream まで終端扱いにし、
example の task はその stream を `abort_subgroup_stream` して、保持済みチャンクを decoder へ渡さずに
捨てる。

## 現状

- `src/pending_subgroup_buffer.rs` の `note_end_of_stream` は `notify_alias(track_alias, EndOfStream)` を呼び、同じ alias の全 entry のうち、理由が未設定のものに理由を設定する。理由を設定するだけで、チャンク自体は捨てない。
- モジュール doc は「entry は Subgroup ストリーム 1 本に対応する」「同じ Track Alias に複数の entry が並び得る」と明記している。
- `examples/moq-sub/src/pipeline.rs` の `register_subgroup_header` (`spawn_stream_task` が起動した
  タスク内の `handle_stream_body` から呼ばれる) は、自ストリームの終端で
  `note_end_of_stream(header.track_alias)` を呼び、`EndOfStream` を受けた task は
  `abort_subgroup_stream` して終了する。同じ alias の並行 stream が保留中だと、その stream も
  abort され、保持済みチャンクは decoder へ渡らずに捨てられる。
- `tests/test_pending_subgroup_buffer.rs` の `note_end_of_stream_releases_that_alias` は alias ごとに
  1 entry しか作らないため、このケースを検出しない。同ファイルには同じ alias に複数 entry を作る
  テスト (`add_registers_entry_with_no_bytes` / `multiple_entries_on_same_alias_are_released_together`)
  があるが、いずれも `note_end_of_stream` を呼ばない。`pbt/tests/prop_pending_subgroup_buffer.rs` の
  モデルは alias 単位の通知を写しており、entry 単位の終端を検証しない。

## 設計方針

- `note_end_of_stream` の引数を `PendingSubgroupEntryId` に変更し、指定した entry のうち理由が未設定のものだけに `EndOfStream` を設定する (最初の理由を優先する現行契約を維持する)。
- 現行の example 以外のアプリケーションコードに `note_end_of_stream` の呼び出しは無く、Track 全体の終端通知の用途も無い。そのため `note_track_end` のような別 API は追加しない。
- `src/pending_subgroup_buffer.rs` の `note_end_of_stream` の doc とモジュール doc の「entry のライフサイクル」を entry 単位の通知に合わせて更新する。
- `examples/moq-sub/src/pipeline.rs` の `register_subgroup_header` は、自ストリームの終端で `note_end_of_stream(id)` を呼ぶように変更する。`take_ready_for` で自分の entry だけを引き取る現在の構造は変えない。
- `pbt/tests/prop_pending_subgroup_buffer.rs` の `NoteEndOfStream` コマンドを Track Alias から entry 指定に変更し、モデルも指定した entry だけを終端させる。同じ alias の他の entry が終端されないことをモデルで検証する。
- `tests/test_pending_subgroup_buffer.rs` に、同じ alias に複数 entry がある状態で 1 件だけ `note_end_of_stream` し、他の entry が引き取り可能にならず、保持済みチャンクも残ることを固定するテストを追加する。
- example は pipeline のテストを持たないため、example 側の修正は `note_end_of_stream(id)` への置き換えと、`abort_subgroup_stream` の呼び出し条件が変わらないことをコード上で確認する。

## 完了条件

- 同一 alias の別 stream の entry が、1 本の FIN で終端扱いにならないこと。
- example が自ストリームの FIN で他の保留 stream を abort しないこと (example の変更はコード上で確認する)。
- 上記のライブラリ側の性質を `tests/test_pending_subgroup_buffer.rs` のテストと `pbt/tests/prop_pending_subgroup_buffer.rs` の PBT で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
