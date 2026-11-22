# example pipeline の巨大関数と引数過多を分割する

- Created: 2026-10-19
- Completed: 2026-11-22
- Branch: feature/refactor-pipeline-run-split
- Polished: 2026-11-20

## 目的

`crates/moq-sub/src/pipeline.rs` の `run` は約 2,900 行、`crates/moq-pub/src/pipeline.rs` の `run` は約 2,500 行あり、`spawn_stream_task` は 30 引数、`handle_stream_body` は約 520 行である。`#[expect(clippy::too_many_arguments)]` も `crates/moq-sub/src/pipeline.rs` の本番コードに 10 箇所ある。責務ごとに分割して変更とレビューを容易にする。

## 現状

- `crates/moq-sub/src/pipeline.rs` の `run` / `handle_stream_body` が巨大化している。`spawn_stream_task` は本体は短いが 30 引数である。
- `crates/moq-pub/src/pipeline.rs` の `run` も約 2,500 行と巨大である。
- `crates/tokio-moq/src/webtransport_h3.rs` の `connect`、`moqt_client.rs` の `drain_events` / `pump_once` も大きい。`webtransport_h2.rs` の `connect` は約 110 行で、補助関数への委譲が進んでいる。これらは本 issue の変更対象ではなく、参考情報である。
- 影響: 変更時の影響範囲が読みにくく、テストも書きにくい。

## 設計方針

- 変更対象は `crates/moq-sub/src/pipeline.rs` と `crates/moq-pub/src/pipeline.rs`、および `run` の引数変更に伴う `crates/moq-sub/src/main.rs`、`crates/moq-sub/src/pipeline.rs` の `#[cfg(test)]` モジュール内の呼び出し、`e2e-tests/tests/roundtrip.rs` の呼び出し箇所に限定する。`crates/tokio-moq` の関数は本 issue では変更しない。
- `crates/moq-sub/src/pipeline.rs` の `run` を、接続とタイムアウト設定、カタログ取得と購読、datagram 受信タスク、データストリーム受信ループ、終了処理に分割する。
- `crates/moq-pub/src/pipeline.rs` の `run` を、入力準備（MP4 読み込みと検証）、接続と PUBLISH、エンコーダ準備、カタログ送信、入力タスク起動、データループ、終了処理に分割する。
- 引数過多の関数は、共有参照をまとめたコンテキスト構造体（例: `StreamTaskContext`）に整理し、各関数が必要とする参照だけを受け取る。
  - `run` 9 引数、`spawn_stream_task` 30 引数、`start_side_session` 9 引数、`dispatch_side_session_command` 10 引数
  - `handle_incoming_stream` / `handle_stream_body` 27 引数、`decode_video_stream` 13 引数、`decode_audio_stream` 8 引数
  - `deliver_video_object` 10 引数、`read_video_fetch_stream` 12 引数
- 本 issue で分割・引数整理の対象とするのは、`run`、`spawn_stream_task`、`start_side_session`、`dispatch_side_session_command`、`handle_incoming_stream`、`handle_stream_body`、`decode_video_stream`、`decode_audio_stream`、`deliver_video_object`、`read_video_fetch_stream` である。
- `handle_stream_body` は、`StreamType`（Fetch / Padding / Subgroup）ごとの処理と、Subgroup 内の track kind（video / audio / catalog / discard）ごとの処理に分ける。datagram は `run` 内の別タスク（`datagram_task`）なので、`handle_stream_body` ではなく `run` の分割時に独立した関数へ抽出する。
- 挙動は変えない。0338 は完了済み (Completed: 2026-11-14) であり、分割前に 0338 で追加した peer_harness の統合テストが通ることを確認する。

## 完了条件

- 変更対象の各関数と、分割で抽出する関数が 1 画面程度（目安として 100 行以内）の大きさに収まっていること。
- `crates/moq-sub/src/pipeline.rs` の本番コードにある 10 箇所の `#[expect(clippy::too_many_arguments)]` が不要になっていること。`#[cfg(test)]` モジュール内の `deliver_video_at` の 1 箇所と、`crates/moq-pub/src/pipeline.rs`、`crates/moq-pub/src/stream_writer.rs` / `datagram_writer.rs`、`src/` 側の `#[expect]` は本 issue の対象外とする。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- 挙動が変わっていないことを既存の e2e テストと 0338 で追加した peer_harness の統合テストで確認していること。0338 の統合テストを分割後も通すこと。

## 解決方法

### moq-sub

- `run` を 42 行のコーディネータへ分割し、接続・準備・受信ループ・終了処理を分けた。
  - `connect_and_subscribe_catalog` / `prepare_runtime` / `run_data_stream_loop`
- 受信ループの腕を関数化した。
  - `spawn_pending_streams` / `handle_client_event` / `handle_migration_result` /
    `handle_side_session_event` / `handle_webtransport_state_change` /
    `handle_session_termination` / `handle_tick`
- `handle_client_event` を `ClientEvent` の腕ごとの関数へ分け、GOAWAY は
  session スコープ / request スコープの処理に分けた。
- `prepare_runtime` を `prepare_media_state` / `subscribe_media_tracks` /
  `prepare_track_subscriptions` / `prepare_catalog_subscription` / `build_pipeline_state`
  などへ分割した。
- `prepare_stream_channels` / `subscribe_catalog` / `handle_side_session_migrated` /
  `resolve_publish_done_pending` / `handle_side_session_session_event` /
  `handle_incoming_request` / `switch_to_migrated_session` /
  `handle_webtransport_state_change` / `apply_reissued_subscription` /
  `handle_publish_done_received` / `handle_subgroup_video_stream` を
  100 行以内にした。
- `spawn_stream_task` を 30 → 6 引数、`start_side_session` を 10 → 4 引数、
  `handle_incoming_stream` / `handle_stream_body` を 27 → 3 引数などに削減した。
- 本番コードの `#[expect(clippy::too_many_arguments)]` 10 箇所を 0 にした
  (テスト内 `deliver_video_at` の 1 箇所は対象外として残置)。

### moq-pub

- `run` を 46 行にし、`PublisherSetup` にループ状態・入力タスク・接続・エンコーダ・
  カタログをまとめた。
  - 抽出: `prepare_input_sources` / `prepare_video_encoder` / `prepare_audio_encoder` /
    `connect_and_publish` / `prepare_catalog` / `start_input_tasks` /
    `PublisherLoopState::new` / `PublisherSetup::*`
- データループを `run_publisher_loop` (61 行) に分け、`PublisherPipelineState` を導入した。
  - 腕ごとの処理を `handle_video_input` / `handle_audio_input` / `handle_notable_event` /
    `handle_migration_outcome` / `handle_request_migration_event` / `handle_tick` /
    `handle_catalog_republish` / `handle_wt_session_state` へ分割した。
- 映像入力パイプラインを `encode_video_input_frames` / `send_encoded_video_frames` /
  `begin_video_group` / `send_video_to_peer_subscriptions` へ分割した。
- 音声入力パイプラインを `process_audio_chunks` / `encode_audio_chunk` /
  `prepare_audio_chunk_timing` / `deliver_audio_chunk` /
  `send_audio_datagram` / `send_audio_stream` などへ分割した。
- `finish_publisher` を `finish_publish_streams` / `finish_peer_subscriptions` へ、
  `migrate_request_on_goaway` / `republish_request_on_goaway` を判定と実行へ分割した。
- `handle_migration_outcome` / `handle_request_migration_event` を 100 行以内にした。
- 変更対象の関数と抽出した関数で 100 行超のものは残っていない
  (対象外の既存 `run_side_session` などは本 issue の対象外)。

### 検証

- `cargo test -p moq-sub`: 302 + 64 + 7 passed (peer_harness 統合テストを含む)
- `cargo test -p moq-pub`: 275 + 9 passed
- `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、
  `cargo fmt --all -- --check`、`prek run --files` が成功した。
- 実リレーが必要な e2e テストは `#[ignore]` のため通常の `cargo test` では実行していない。
