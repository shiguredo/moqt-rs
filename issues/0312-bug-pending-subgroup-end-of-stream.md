# PendingSubgroupBuffer の stream 終端通知を entry 単位にする

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-pending-subgroup-end-of-stream
- Polished: {YYYY-MM-DD}

## 目的

`PendingSubgroupBuffer` の entry は Subgroup ストリーム 1 本に対応し、同じ Track Alias に複数の entry が並び得る。現状の `note_end_of_stream` は Track Alias 単位で全 entry を終端扱いにするため、1 本の stream の FIN が、まだ開いている同じ alias の別 Subgroup stream まで abort させ、保持済みチャンクを捨てる。

## 現状

- `src/pending_subgroup_buffer.rs` の `note_end_of_stream` は `notify_alias(track_alias, EndOfStream)` を呼び、同じ alias の全 entry に理由を設定する。
- モジュール doc は「entry は Subgroup ストリーム 1 本に対応する」「同じ Track Alias に複数の entry が並び得る」と明記している。
- `examples/moq-sub/src/pipeline.rs` の `spawn_stream_task` は自ストリームの終端で `note_end_of_stream(header.track_alias)` を呼び、`EndOfStream` を受けた task は `abort_subgroup_stream` して終了する。同じ alias の並行 stream が保留中だと、その stream も abort される。
- `tests/test_pending_subgroup_buffer.rs` は alias に 1 entry しか作らないため、このケースを検出しない。

## 設計方針

- `note_end_of_stream` を `PendingSubgroupEntryId` 引数に変更し、entry 単位で通知する。Track 全体の終端を表す用途が別にある場合は `note_track_end` のような別 API に分ける。
- example / PBT モデル / 単体テストを同時に更新する。
- 同じ alias に複数 entry がある状態で 1 件だけ終端するテストを追加する。

## 完了条件

- 同一 alias の別 stream の entry が、1 本の FIN で終端扱いにならないこと。
- example が自ストリームの FIN で他の保留 stream を abort しないこと。
- 上記を単体テストと PBT で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
