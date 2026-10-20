# 未使用の依存関係を削除し、重複したテストと不要な公開項目を整理する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/remove-unused-dependencies
- Polished: 2026-10-20

## 目的

`Cargo.toml` の直接依存を実際の使用に合わせ、未使用の直接依存とテスト専用依存の区分を正す。`s2n-quic` / `shiguredo_http3` / `tracing` は他のクレート経由で依存グラフに残るため、ビルド時間や依存グラフの削減ではなく、manifest の正確性と保守性を目的とする。あわせて、重複したテストと不要な公開項目を整理する。

## 現状

- `examples/moq-pub/Cargo.toml` と `examples/moq-sub/Cargo.toml` の `s2n-quic` は `src/` と `tests/` で使われておらず、`examples/tokio-moq` が同じ features で依存している。直接依存は重複しており、`s2n-quic` は `tokio-moq` 経由で依存グラフに残る。
- `e2e-tests/Cargo.toml` の `tracing` は `e2e-tests/tests/roundtrip.rs` で使われておらず、`tracing-subscriber` だけを使っている。`tracing` は `moq-pub` / `moq-sub` 経由で依存グラフに残る。
- `examples/moq-pub/Cargo.toml` と `examples/moq-sub/Cargo.toml` の `shiguredo_http3` は `src/` ではテスト (`ErrorCode::AlpnError`) だけで使われている。`tokio-moq` が通常依存として使うため、依存グラフは変わらない。
- `pbt/Cargo.toml` の `[dev-dependencies]` はコメントだけで依存が無い。
- `examples/moq-pub/src/pipeline.rs` と `examples/moq-sub/src/pipeline.rs` の `migration_target_tests` は、
  同じ `tokio_moq::migration_target` を対象にしたテストを持つ。
  `request_stream_goaway_keeps_the_session` / `empty_uri_reuses_the_current_url` /
  `invalid_uri_is_rejected` の 3 本が実質重複している。
  moq-pub 側には `uri_without_c4m_carries_the_tokens_over` /
  `uri_with_c4m_uses_its_own_tokens` / `non_utf8_uri_is_rejected` があり、
  c4m トークンの引き継ぎと UTF-8 検証を固定している。
  moq-pub 側の `current_url` は c4m トークンを含む URL を使うため、テスト内容は完全には同一ではない。
- `examples/tokio-moq/src/lib.rs` の `split_authority_path` / `host_from_authority`、
  `examples/tokio-moq/src/metrics.rs` の `TASK_METRICS_LOG_INTERVAL` は
  crate 外から使われず `pub` になっている。
  `split_authority_path` / `host_from_authority` は `lib.rs` 内でだけ使われ、
  `TASK_METRICS_LOG_INTERVAL` は `metrics.rs` 内でだけ使われている。

## 設計方針

- 未使用の依存を削除し、テスト専用の依存を `[dev-dependencies]` へ移す。`examples/moq-pub/Cargo.toml` と `examples/moq-sub/Cargo.toml` から `s2n-quic` を削除し、`shiguredo_http3` を `[dev-dependencies]` へ移す。`e2e-tests/Cargo.toml` から `tracing` を削除する。
- `shiguredo_http3` を `[dev-dependencies]` へ移す際は、用途コメントをテストでの使用 (`ErrorCode::AlpnError`) に合わせる。
- `pbt/Cargo.toml` の空の `[dev-dependencies]` セクションは削除する。dev 依存が不要である理由のコメントは残す。
- 重複したテストを 1 箇所 (tokio-moq 側) に統合する。`migration_target_tests` の 3 本の重複テストと moq-pub 固有の c4m / UTF-8 テストを `tokio-moq` の `migration_target` のテストへ移し、moq-pub / moq-sub の `migration_target_tests` は削除する。c4m トークンの引き継ぎと UTF-8 検証の検証は失わない。
- 外部から使われない項目を `pub(crate)` 以下に下げる。`split_authority_path` / `host_from_authority` は `pub(crate)` または private、`TASK_METRICS_LOG_INTERVAL` は private の `const` にする。
- `README` の依存の記述は、`tokio-moq` が引き続き `s2n-quic` / `shiguredo_http3` / `tracing` を使うため変更しない。

## 完了条件

- `cargo check --workspace` / `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- `examples/moq-pub/Cargo.toml` と `examples/moq-sub/Cargo.toml` から `s2n-quic` が消え、`shiguredo_http3` が `[dev-dependencies]` に移り、用途コメントがテストでの使用を説明していること。
- `e2e-tests/Cargo.toml` から `tracing` が消えていること。
- `pbt/Cargo.toml` の空の `[dev-dependencies]` セクションが消え、dev 依存が不要である理由のコメントが残っていること。
- `examples/moq-pub/src/pipeline.rs` と `examples/moq-sub/src/pipeline.rs` の `migration_target_tests` が削除され、`tokio-moq` 側の `migration_target` のテストに、3 本の重複テストと moq-pub 固有の c4m / UTF-8 テストが統合されていること。
- `examples/tokio-moq/src/lib.rs` の `split_authority_path` / `host_from_authority` が `pub(crate)` または private、`examples/tokio-moq/src/metrics.rs` の `TASK_METRICS_LOG_INTERVAL` が private になっていること。
- 未使用の直接依存が `cargo machete` などの未使用依存検出ツール、または `rg` による使用箇所の確認で検出されないこと。`cargo tree` は依存ツリーの確認用であり、未使用依存の検出には使わない。
- `README` の依存の記述が実態と食い違わないこと。今回の変更では `tokio-moq` が引き続き `s2n-quic` / `shiguredo_http3` / `tracing` を使うため、`README` の依存の記述は変更不要である。
