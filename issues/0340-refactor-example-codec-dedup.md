# moq-pub / moq-sub の decoder 実装の重複を解消する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-example-codec-dedup
- Polished: {YYYY-MM-DD}

## 目的

`examples/moq-pub/src/decoder/` と `examples/moq-sub/src/decoder/` にほぼ同一の AV1 / H.264 / H.265 / Opus のデコード処理があり、`copy_plane` / `read_ps_nal` なども重複している。変更時に両方を直す必要があり、片方だけ直す事故が起きる。

## 現状

- `examples/moq-pub/src/decoder/{av1,h264,h265,opus}.rs` と `examples/moq-sub/src/decoder/{av1,h264,h265,opus}.rs` が重複している。
- `examples/moq-pub/src/decoder.rs` と `examples/moq-sub/src/decoder.rs` の `copy_plane` / `read_ps_nal` がほぼ同一である。
- 影響: 修正コストと不整合リスクが二重化している。

## 設計方針

- examples 配下に共有 codec crate (例 `examples/moq-codec`) を作り、pub / sub 固有の差 (`drain_delayed`、`nalu_len_bytes`、`timestamp_us`) だけを拡張点にする。
- もしくは tokio-moq の lib に共有実装を置く。
- 挙動は変えない。

## 完了条件

- decoder 実装が 1 箇所に集約され、pub / sub の差が明示されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- e2e が通ること。
