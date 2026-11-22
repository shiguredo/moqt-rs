# 未使用の依存関係を削除し、重複したテストと不要な公開項目を整理する

- Created: 2026-10-19
- Completed: 2026-11-22
- Branch: feature/remove-unused-dependencies
- Polished: 2026-11-20

## 目的

`Cargo.toml` の直接依存を実際の使用に合わせ、未使用の直接依存とテスト専用依存の区分を正す。`s2n-quic` / `shiguredo_http3` / `tracing` は他のクレート経由で依存グラフに残るため、ビルド時間や依存グラフの削減ではなく、manifest の正確性と保守性を目的とする。あわせて、重複したテストと不要な公開項目を整理する。

## 現状

- `crates/moq-pub/Cargo.toml` と `crates/moq-sub/Cargo.toml` に `s2n-quic` の直接依存は無い (削除済み)。`s2n-quic` は `tokio-moq` の `quic-s2n` feature 経由で依存グラフに残る。`src/` と `tests/` で `s2n-quic` を直接使っていない。
- `e2e-tests/Cargo.toml` の `tracing` は `e2e-tests/tests/roundtrip.rs` で使われておらず、`tracing-subscriber` だけを使っている。`tracing` は `moq-pub` / `moq-sub` 経由で依存グラフに残る。
- `crates/moq-pub/Cargo.toml` と `crates/moq-sub/Cargo.toml` の `shiguredo_http3` は `src/` ではテスト (`ErrorCode::AlpnError`) だけで使われている。`tokio-moq` が通常依存として使うため、依存グラフは変わらない。
- `pbt/Cargo.toml` の `[dev-dependencies]` はコメントだけで依存が無い。
- `crates/moq-pub/src/pipeline.rs` と `crates/moq-sub/src/pipeline.rs` の `migration_target_tests` は、
  同じ `tokio_moq::migration_target` を対象にしたテストを持つ。
  `request_stream_goaway_reports_the_request_scope` / `empty_uri_reuses_the_current_url` /
  `invalid_uri_is_rejected` の 3 本が実質重複している。
  moq-pub 側には `same_origin_uri_without_cat_carries_the_tokens_over` /
  `different_origin_uri_does_not_carry_tokens` /
  `uri_with_cat_uses_its_own_tokens` / `non_utf8_uri_is_rejected` があり、
  `cat` トークン (C4M 認可トークン) の引き継ぎと UTF-8 検証を固定している。
  moq-pub 側の `current_url` は `cat` トークンを含む URL を使うため、テスト内容は完全には同一ではない。
  `crates/tokio-moq/src/lib.rs` には `migration_target` の既存テスト
  (`migration_target_reports_request_scope` /
  `migration_target_keeps_session_for_invalid_request_uri` /
  `migration_target_errors_do_not_include_uri`) があり、
  `migration_target_reports_request_scope` は
  `request_stream_goaway_reports_the_request_scope` と実質重複している。
- `crates/tokio-moq/src/lib.rs` の `split_authority_path` / `host_from_authority`、
  `crates/tokio-moq/src/metrics.rs` の `TASK_METRICS_LOG_INTERVAL` は
  crate 外から使われず `pub` になっている。
  `split_authority_path` / `host_from_authority` は `lib.rs` 内でだけ使われ、
  `TASK_METRICS_LOG_INTERVAL` は `metrics.rs` 内でだけ使われている。

## 設計方針

- 未使用の依存を削除し、テスト専用の依存を `[dev-dependencies]` へ移す。`s2n-quic` の直接依存が無いことを確認する (現状で満たしている)。`crates/moq-pub/Cargo.toml` と `crates/moq-sub/Cargo.toml` の `shiguredo_http3` を `[dev-dependencies]` へ移す。`e2e-tests/Cargo.toml` から `tracing` を削除する。
- `shiguredo_http3` を `[dev-dependencies]` へ移す際は、用途コメントをテストでの使用 (`ErrorCode::AlpnError`) に合わせる。
- `pbt/Cargo.toml` の空の `[dev-dependencies]` セクションは削除する。dev 依存が不要である理由のコメントは残す。
- 重複したテストを 1 箇所 (tokio-moq 側) に統合する。moq-pub 側の
  `migration_target_tests` から、moq-sub 側と実質重複する 3 本
  (`request_stream_goaway_reports_the_request_scope` /
  `empty_uri_reuses_the_current_url` / `invalid_uri_is_rejected`) と、
  moq-pub 固有の 4 本 (`same_origin_uri_without_cat_carries_the_tokens_over` /
  `different_origin_uri_does_not_carry_tokens` /
  `uri_with_cat_uses_its_own_tokens` / `non_utf8_uri_is_rejected`) を
  `tokio-moq` の `migration_target` のテストへ移し、moq-pub / moq-sub の
  `migration_target_tests` は削除する。`request_stream_goaway_reports_the_request_scope`
  は tokio-moq の既存テスト `migration_target_reports_request_scope` と統合して 1 本にする。
  `cat` トークン (C4M 認可トークン) の引き継ぎと UTF-8 検証は失わない。
- 外部から使われない項目を `pub(crate)` 以下に下げる。`split_authority_path` / `host_from_authority` は `pub(crate)` または private、`TASK_METRICS_LOG_INTERVAL` は private の `const` にする。
- `crates/README.md` と `crates/tokio-moq/README.md` の依存・バックエンドの記述は、`tokio-moq` が引き続き `s2n-quic` / `shiguredo_http3` / `tracing` を使うため変更しない。ルート `README.md` の「依存は ... のみ」は `shiguredo_moqt` の依存であり、今回の変更の対象外である。
- `CHANGES.md` は更新しない (CODEBASE.md の「変更履歴を `CHANGES.md` に残さないこと」に従う)。
- CODEBASE.md の「Pull-Request やブランチを作らず develop にコミットしていくこと」「1 issue 1 commit 1 push」に従い、`Branch:` のブランチは作らず develop に直接コミットし、1 コミットにまとめる。

## 完了条件

- `cargo check --workspace` / `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が macOS 上で通ること (crates 配下の crate が macOS 専用依存を持つため、Linux では workspace 全体をビルドできない)。
- 変更したファイルを指定した `prek run --files` が通ること (tombi を含む)。
- `crates/moq-pub/Cargo.toml` と `crates/moq-sub/Cargo.toml` に `s2n-quic` の直接依存が無いこと (現状で満たしている)。`shiguredo_http3` が `[dev-dependencies]` に移り、用途コメントがテストでの使用を説明していること。
- `e2e-tests/Cargo.toml` から `tracing` が消えていること。
- `pbt/Cargo.toml` の空の `[dev-dependencies]` セクションが消え、dev 依存が不要である理由のコメントが残っていること。
- `crates/moq-pub/src/pipeline.rs` と `crates/moq-sub/src/pipeline.rs` の
  `migration_target_tests` が削除され、`tokio-moq` 側の `migration_target` の
  テストに、3 本の重複テストと moq-pub 固有の 4 本
  (`same_origin_uri_without_cat_carries_the_tokens_over` /
  `different_origin_uri_does_not_carry_tokens` /
  `uri_with_cat_uses_its_own_tokens` / `non_utf8_uri_is_rejected`) が
  統合されていること。`request_stream_goaway_reports_the_request_scope` は
  `migration_target_reports_request_scope` と統合され、同じ振る舞いの
  テストが重複して残っていないこと。
- `crates/tokio-moq/src/lib.rs` の `split_authority_path` / `host_from_authority` が `pub(crate)` または private、`crates/tokio-moq/src/metrics.rs` の `TASK_METRICS_LOG_INTERVAL` が private になっていること。
- 未使用の直接依存が `cargo machete` などの未使用依存検出ツール、または `rg` による使用箇所の確認で検出されないこと。`cargo tree` は依存ツリーの確認用であり、未使用依存の検出には使わない。
- `crates/README.md` と `crates/tokio-moq/README.md` の依存・バックエンドの記述が実態と食い違わないこと。今回の変更では `tokio-moq` が引き続き `s2n-quic` / `shiguredo_http3` / `tracing` を使うため、これらの README の依存の記述は変更不要である。
- `CHANGES.md` にエントリを追加していないこと。
- `Branch:` のブランチを作らず、1 コミットにまとめていること。

## 解決方法

- `crates/moq-pub` / `crates/moq-sub` の `shiguredo_http3` を `[dev-dependencies]` へ移し、
  テストでの `ErrorCode::AlpnError` 使用をコメントで明記した。
- `e2e-tests` から未使用の `tracing` を削除した。
- `pbt` の空の `[dev-dependencies]` を削除し、dev 依存が不要な理由を `[dependencies]` に移した。
- `crates/tokio-moq/src/lib.rs` の `split_authority_path` / `host_from_authority` と、
  `metrics.rs` の `TASK_METRICS_LOG_INTERVAL` を非公開にした。
- moq-pub / moq-sub の `migration_target_tests` を削除し、moq-pub 固有の 4 テストと
  空 URI のテストを `tokio-moq` 側の `migration_target` テストへ統合した。
  3 本の重複テストは `tokio-moq` の既存テストで担保する。

`cargo check --workspace` / `cargo test --workspace` /
`cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` と
`prek run --files` が成功した。
