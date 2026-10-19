# 未使用の依存関係と重複したテスト・公開項目を削除する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/remove-unused-dependencies
- Polished: {YYYY-MM-DD}

## 目的

ビルド時間と依存グラフを減らすため、未使用の依存と、不要な公開・重複したテストを削除する。

## 現状

- `examples/moq-pub/Cargo.toml` と `examples/moq-sub/Cargo.toml` の `s2n-quic` は `src/` で使われておらず、`examples/tokio-moq` が同じ features で依存している。
- `e2e-tests/Cargo.toml` の `tracing` は `e2e-tests/tests/roundtrip.rs` で使われておらず、`tracing-subscriber` だけを使っている。
- `examples/moq-pub/Cargo.toml` と `examples/moq-sub/Cargo.toml` の `shiguredo_http3` は `src/` ではテスト (`ErrorCode::AlpnError`) だけで使われている。
- `pbt/Cargo.toml` の `[dev-dependencies]` はコメントだけで依存が無い。
- `examples/moq-pub/src/pipeline.rs` と `examples/moq-sub/src/pipeline.rs` に同じ `migration_target` のテスト (`migration_target_tests`) がある。
- `examples/tokio-moq/src/lib.rs` の `split_authority_path` / `host_from_authority`、`examples/tokio-moq/src/metrics.rs` の `TASK_METRICS_LOG_INTERVAL` は crate 外から使われず `pub` になっている。

## 設計方針

- 未使用の依存を削除し、テスト専用の依存を `[dev-dependencies]` へ移す。
- 重複したテストを 1 箇所 (tokio-moq 側) に統合する。
- 外部から使われない項目を `pub(crate)` 以下に下げる。

## 完了条件

- `cargo check --workspace` / `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- 未使用依存が `cargo tree` / `cargo machete` 等で検出されないこと。
- `README` の依存の記述が変わった場合、その記述も更新されていること。
