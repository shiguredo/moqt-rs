# ライブラリの MSRV と rust-toolchain を shiguredo-rust の規約に合わせる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msrv-toolchain
- Polished: 2026-10-20

## 目的

shiguredo-rust はライブラリの MSRV を 1.95 とし、`Cargo.toml` の `rust-version` と `rust-toolchain.toml` の `channel` を同じ値に揃えることを求める。現状は `rust-version = "1.93"`、channel 1.95 で、1.93 でのビルドは CI で検証されていない。宣言した MSRV が実質未検証になっている。本 issue では規約に合わせて MSRV を 1.95 に引き上げ、宣言と検証を一致させる。

## 現状

- ルート `Cargo.toml` と `pbt/Cargo.toml` の `rust-version` は 1.93。
- `fuzz/Cargo.toml` も `rust-version = "1.93"` である。`fuzz` はルート workspace の `exclude = ["fuzz"]` で独立した workspace だが、`shiguredo_moqt` を path 依存に持つ。
- `examples/moq-pub/Cargo.toml` / `examples/moq-sub/Cargo.toml` / `examples/tokio-moq/Cargo.toml` / `e2e-tests/Cargo.toml` の `rust-version` は 1.95。workspace の最大 `rust-version` は既に 1.95。
- `rust-toolchain.toml` の channel は 1.95。コメントに「MSRV は Cargo.toml の rust-version (ライブラリ本体と pbt は 1.93) だが、workspace の examples が使う s2n-quic が 1.95 を要求するため 1.95 に固定する。」「workspace の最大 rust-version を上げたら channel も同時に更新すること。」とある。
- 1.93 のツールチェーンで check / clippy する CI ジョブは無い。`.github/workflows/ci.yml` は `rust-toolchain.toml` の 1.95 を使い、冒頭コメントに「MSRV 検証は含めない。examples が rust-version 1.95 を要求するため workspace 単位の 1.93 検証が構造的にできない」とある。
- `README.md` は「前提条件は Rust 1.95 以降です。サンプルが使う `s2n-quic` が 1.95 を要求するためです。`shiguredo_moqt` ライブラリと `pbt` だけであれば 1.93 で構築できます。」と、ライブラリ単体の MSRV を 1.93 と案内している。
- `skills/shiguredo-moqt/SKILL.md` は「最小 Rust バージョン: 1.93」と案内している。
- 影響: 1.94 以降で安定化した API を 1.93 互換コードに誤用しても検出できない。規約上の逸脱でもある。

## 設計方針

- shiguredo-rust の「最小 Rust バージョン (MSRV) は 1.95 とすること」「`Cargo.toml` の `rust-version` に `"1.95"` を明記すること」「`rust-toolchain.toml` の `channel` には `rust-version` と同じ値 (`"1.95"`) を指定すること」に従い、MSRV を 1.95 にする。1.93 を維持する選択肢は規約に反するため採らない。
- ルート `Cargo.toml` と `pbt/Cargo.toml` の `rust-version` を 1.95 に上げる。
- `fuzz/Cargo.toml` の `rust-version` も 1.95 に上げる。`fuzz` は独立 workspace だが `shiguredo_moqt` を path 依存に持ち、ライブラリの MSRV が 1.95 になると 1.93 では依存先の `rust-version = "1.95"` によりビルドできない。1.93 の宣言を残すと虚偽になる。
- `rust-toolchain.toml` の `channel` は既に 1.95 なので変更しない。コメントは 1.93 前提を削除し、MSRV 1.95 と channel 1.95 を揃えていること、examples / e2e-tests も s2n-quic が 1.95 を要求することを示す内容に更新する。
- `README.md` の「`shiguredo_moqt` ライブラリと `pbt` だけであれば 1.93 で構築できます。」を削除し、「サンプルが使う `s2n-quic` が 1.95 を要求するためです。」を「ライブラリの MSRV と、サンプルが使う `s2n-quic` が 1.95 を要求するためです。」に更新する。前提条件が Rust 1.95 以降であることは変更しない。
- `skills/shiguredo-moqt/SKILL.md` の「最小 Rust バージョン: 1.93」を「最小 Rust バージョン: 1.95」に更新する。
- `.github/workflows/ci.yml` の冒頭コメント「MSRV 検証は含めない。examples が rust-version 1.95 を要求するため workspace 単位の 1.93 検証が構造的にできない」を、MSRV 1.95 と channel 1.95 が一致し、既存の CI が 1.95 で workspace をビルド・テストするため MSRV 1.95 が検証される旨に更新する。新しい CI ジョブは追加しない。
- `fuzz` は独立 workspace で CI の `cargo test --workspace` の対象外であり、fuzz のビルド検証は CI では行わない。`fuzz/Cargo.toml` の `rust-version` を 1.95 にするのは宣言の整合のためであり、fuzz 用の CI ジョブは追加しない。
- `CHANGES.md` にはエントリを追加しない (CODEBASE.md の「変更履歴を `CHANGES.md` に残さないこと」に従う)。
- `issues/` 内の 1.93 への言及 (例: `issues/0300-draft-22-authority-ipv6-validation.md`) は別 issue の記述であり、本 issue では変更しない。必要な場合は refresh-issue で同期する。
- CODEBASE.md の「Pull-Request やブランチを作らず develop にコミットしていくこと」「1 issue 1 commit 1 push」に従い、`Branch:` のブランチは作らず develop に直接コミットする。上記の変更は 1 コミットにまとめる。

## 完了条件

- ルート `Cargo.toml`、`pbt/Cargo.toml`、`fuzz/Cargo.toml` の `rust-version` が 1.95 になっていること。
- `rust-toolchain.toml` の `channel` が 1.95 のままで、コメントに 1.93 前提が残っていないこと。
- `README.md` と `skills/shiguredo-moqt/SKILL.md` に 1.93 を前提とした記述が残っていないこと。
- `.github/workflows/ci.yml` の冒頭コメントが、MSRV 1.95 と channel 1.95 の関係、および既存 CI が MSRV 1.95 を検証することを正しく説明していること。新しい CI ジョブが追加されていないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- `prek run --all-files` が通ること (markdownlint / tombi を含む)。
