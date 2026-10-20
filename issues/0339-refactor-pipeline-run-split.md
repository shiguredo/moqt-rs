# example pipeline の巨大関数と引数過多を分割する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-pipeline-run-split
- Polished: 2026-10-20

## 目的

`examples/moq-sub/src/pipeline.rs` の `run` は約 1,300 行、`examples/moq-pub/src/pipeline.rs` の `run` は約 1,450 行あり、`spawn_stream_task` は 24 引数、`handle_stream_body` は約 420 行である。`#[expect(clippy::too_many_arguments)]` も `examples/moq-sub/src/pipeline.rs` に 7 箇所ある。責務ごとに分割して変更とレビューを容易にする。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `run` / `handle_stream_body` が巨大化している。`spawn_stream_task` は本体は短いが 24 引数である。
- `examples/moq-pub/src/pipeline.rs` の `run` も約 1,450 行と巨大である。
- `examples/tokio-moq/src/webtransport_h3.rs` の `connect`、`moqt_client.rs` の `drain_events` / `pump_once` も大きい。`webtransport_h2.rs` の `connect` は約 110 行で、補助関数への委譲が進んでいる。これらは本 issue の変更対象ではなく、参考情報である。
- 影響: 変更時の影響範囲が読みにくく、テストも書きにくい。

## 設計方針

- 変更対象は `examples/moq-sub/src/pipeline.rs` と `examples/moq-pub/src/pipeline.rs`、および `run` の引数変更に伴う `examples/moq-sub/src/main.rs` と `e2e-tests/tests/roundtrip.rs` の呼び出し箇所に限定する。`examples/tokio-moq` の関数は本 issue では変更しない。
- `examples/moq-sub/src/pipeline.rs` の `run` を、接続とタイムアウト設定、カタログ取得と購読、datagram 受信タスク、データストリーム受信ループ、終了処理に分割する。
- `examples/moq-pub/src/pipeline.rs` の `run` を、入力準備（MP4 読み込みと検証）、接続と PUBLISH、エンコーダ準備、カタログ送信、入力タスク起動、データループ、終了処理に分割する。
- 引数過多の関数（`examples/moq-sub/src/pipeline.rs` の `run` 8 引数、`spawn_stream_task` 24 引数、`handle_incoming_stream` / `handle_stream_body` 21 引数、`decode_video_stream` / `read_video_fetch_stream` 10 引数、`deliver_video_object` 9 引数）は、共有参照をまとめたコンテキスト構造体（例: `StreamTaskContext`）に整理し、各関数が必要とする参照だけを受け取る。
  `run` の引数変更に伴い、`examples/moq-sub/src/main.rs` と `e2e-tests/tests/roundtrip.rs` の呼び出しも更新する。
- 本 issue で分割・引数整理の対象とするのは、`run`、`spawn_stream_task`、`handle_incoming_stream`、`handle_stream_body`、`decode_video_stream`、`deliver_video_object`、`read_video_fetch_stream` である。
- `handle_stream_body` は、`StreamType`（Fetch / Padding / Subgroup）ごとの処理と、Subgroup 内の track kind（video / audio）ごとの処理に分ける。datagram は `run` 内の別タスク（`datagram_task`）なので、`handle_stream_body` ではなく `run` の分割時に独立した関数へ抽出する。
- 挙動は変えない。0338 の pipeline 統合テスト（peer_harness / e2e）を先に完了させてから着手する。少なくとも分割前に、0338 の peer_harness テストが通る状態にする。

## 完了条件

- 変更対象の各関数が 1 画面程度（目安として 100 行以内）の大きさに収まっていること。
- `examples/moq-sub/src/pipeline.rs` の 7 箇所の `#[expect(clippy::too_many_arguments)]` が不要になっていること。`examples/moq-pub/src/stream_writer.rs` / `datagram_writer.rs` と `src/` 側の `#[expect]` は本 issue の対象外とする。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- 挙動が変わっていないことを e2e / peer_harness のテストで確認していること。0338 で追加する統合テストを分割後も通すこと。
