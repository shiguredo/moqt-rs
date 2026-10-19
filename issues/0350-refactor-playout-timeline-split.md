# playout/timeline.rs の定数・公開型・テストを分割する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-playout-timeline-split
- Polished: {YYYY-MM-DD}

## 目的

`src/playout/timeline.rs` が約 2,000 行になり、30 個超の定数、公開型、A/V 同期の状態機械、約 480 行のテストが 1 ファイルに混在している。責務ごとに分割して変更とレビューを容易にする。

## 現状

- `src/playout/timeline.rs` に定数、`TimelineConfig`、`PlayoutTimeline` 本体、A/V 同期の状態機械、テストが同居している。
- サブモジュール分割自体は妥当だが、定数と公開型、テストの置き場所が整理されていない。
- 影響: ファイルが大きすぎて差分レビューが難しい。

## 設計方針

- 定数と公開型を `timeline/config.rs` などへ移す。
- テストを `timeline/tests.rs` へ移す。`mod.rs` は使わない。
- 公開 API と挙動は変えない。

## 完了条件

- `timeline.rs` が本体の実装に集中していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` が通ること。
