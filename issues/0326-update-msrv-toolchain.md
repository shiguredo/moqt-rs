# ライブラリの MSRV と rust-toolchain を shiguredo-rust の規約に合わせる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msrv-toolchain
- Polished: {YYYY-MM-DD}

## 目的

shiguredo-rust はライブラリの MSRV を 1.95 とし、`Cargo.toml` の `rust-version` と `rust-toolchain.toml` の `channel` を同じ値に揃えることを求める。現状は `rust-version = "1.93"`、channel 1.95 で、1.93 でのビルドは CI で検証されていない。宣言した MSRV が実質未検証になっている。

## 現状

- `Cargo.toml` と `pbt/Cargo.toml` の `rust-version` は 1.93。
- `rust-toolchain.toml` の channel は 1.95。examples が使う s2n-quic が 1.95 を要求するため、というコメントがある。
- 1.93 のツールチェーンで check / clippy する CI ジョブは無い。
- 影響: 1.94 以降で安定化した API を 1.93 互換コードに誤用しても検出できない。規約上の逸脱でもある。

## 設計方針

- ライブラリと pbt の `rust-version` を 1.95 に上げ、`rust-toolchain.toml` と揃える。
- 1.93 を意図した互換性宣言を維持する場合は、その理由を CODEBASE.md / README に明記し、1.93 で check する CI ジョブを追加する。
- どちらを採るかを決めて doc に残す。

## 完了条件

- `rust-version` と `channel` が規約どおりの関係になっていること。
- MSRV を宣言する場合はその値でビルドを検証する CI があること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
