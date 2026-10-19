# example pipeline の巨大関数と引数過多を分割する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-pipeline-run-split
- Polished: {YYYY-MM-DD}

## 目的

`examples/moq-sub/src/pipeline.rs` の `run` は約 1,300 行、`examples/moq-pub/src/pipeline.rs` の `run` は約 1,450 行あり、`spawn_stream_task` は 24 引数、`handle_stream_body` は約 450 行である。`#[expect(clippy::too_many_arguments)]` も 7 箇所ある。責務ごとに分割して変更とレビューを容易にする。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `run` / `spawn_stream_task` / `handle_stream_body` が巨大化している。
- `examples/moq-pub/src/pipeline.rs` の `run` も同様である。
- `examples/tokio-moq/src/webtransport_h2.rs` / `webtransport_h3.rs` の `connect`、`moqt_client.rs` の `drain_events` / `pump_once` も大きい。
- 影響: 変更時の影響範囲が読みにくく、テストも書きにくい。

## 設計方針

- `run` を接続、カタログ解決、購読、受信ループ、終了処理に分割する。
- 24 引数は `StreamTaskContext` のような構造体にまとめ、必要な参照だけを渡す。
- `handle_stream_body` を video / audio / datagram の処理単位に分ける。
- 挙動は変えない。0345 の e2e を先に整備するか、同時に整備する。

## 完了条件

- 関数が 1 画面程度の大きさに収まり、`too_many_arguments` の `#[expect]` が不要になっていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- 挙動が変わっていないことを e2e / peer_harness のテストで確認していること。
