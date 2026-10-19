# example pipeline と e2e の統合経路をテストする

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/test-pipeline-integration-coverage
- Polished: {YYYY-MM-DD}

## 目的

library の tests / PBT / fuzz は厚いが、example の pipeline 統合経路 (ピア要求処理、カタログ適用、GOAWAY 移行、メディア FETCH) はほぼ未テストで、e2e も live 3 本のみである。統合経路の退行を検出できる状態にする。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `serve_peer_request` / `serve_media_fetch` / `serve_fetch_entries` にテストが無い。
- `examples/moq-sub/src/pipeline.rs` の `receive_catalog` / `resubscribe_tracks` / `run` にテストが無い。
- `e2e-tests/tests/roundtrip.rs` は映像・音声 datagram・音声冗長の 3 本で、GOAWAY 移行、フィルタ付き / pause 購読、メディア FETCH 応答、MP4 パススルー / 再エンコード、PUBLISH_DONE の track 単位処理を検証しない。
- e2e の `ATTEMPT_TIMEOUT` は GOP より長いため、FETCH が壊れていても live の次キーフレームで最初のフレームが届き得る。
- `fuzz/fuzz_targets/fuzz_session.rs` は FETCH header / entry のライフサイクルと成功応答経路を踏まない。
- 影響: 0342 以降の example 修正を安全に進められない。

## 設計方針

- `peer_harness` を使って pipeline の統合経路をテストする。受信 SUBSCRIBE のフィルタ / Forward 0 / PUBLISH_DONE、GOAWAY 移行後の再購読と FETCH、MP4 入力の PTS 間隔、Audio Config の再送を検証する。
- e2e は GOP を長くするか、最初のフレームが FETCH 経由で届いたことを時刻で判定する。
- `fuzz_session` に fetch header / entry のライフサイクルを追加する。

## 完了条件

- 上記の統合テストが追加され、既存の e2e が通ること。
- テストが実装の退行で落ちることを確認していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
