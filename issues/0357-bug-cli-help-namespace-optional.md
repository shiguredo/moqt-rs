# moq-pub / moq-sub の --help で省略可能な --namespace が必須として表示される

- Created: 2026-10-20
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-cli-help-namespace-optional
- Polished: {YYYY-MM-DD}

## 目的

`--help` の Usage が省略可能な `--namespace` を必須として表示し、moq-pub の説明行が空になる。利用者がオプションの必須・任意を誤解しないよう、help 表示を実装に合わせる。

## 現状

- `examples/moq-pub/src/cli.rs` と `examples/moq-sub/src/cli.rs` の `--namespace` は `.example("moq-example")` を持つ。noargs の `build_usage` は `.example` を持つオプションを必須オプションとして `Usage:` に `--namespace <NS>` と表示するため、通常実行では省略可能な `--namespace` が必須に見える
- `--namespace` を省略した場合は `--url` の MSF fragment の track-identifier から namespace を解決する (`resolve_namespace`)。省略可能であることは `cli.rs` のコメントにも書かれている
- `examples/moq-pub/Cargo.toml` に `description` が無く、`examples/moq-pub/src/cli.rs` の `env!("CARGO_PKG_DESCRIPTION")` が空になるため、`moq-pub --help` の説明行が出ない。`examples/moq-sub/src/cli.rs` は `app_description` を固定文字列で設定しているためこの問題は無い

## 変更対象

- `examples/moq-pub/src/cli.rs`
- `examples/moq-sub/src/cli.rs`
- `examples/moq-pub/Cargo.toml`

## 設計方針

- `--namespace` を `Usage:` で必須として表示しない。`--namespace` の `.example` に頼らず、help モードでも namespace の解決をスキップするなどして、通常実行の `--namespace` 省略時の MSF fragment 解決は変えない
- `examples/moq-pub/Cargo.toml` に `description` を追加し、`moq-pub --help` の説明行が出るようにする
- `--url` の `.example` は必須オプションの help 表示のために必要なので残す

## 完了条件

- `moq-pub --help` と `moq-sub --help` の `Usage:` に `--namespace <NS>` が必須オプションとして表示されないこと
- `--namespace` を省略した通常実行で、`--url` の MSF fragment から namespace が解決されること
- `moq-pub --help` に `CARGO_PKG_DESCRIPTION` 由来の説明行が表示されること
- `cargo test -p moq-pub -p moq-sub` / `cargo clippy -p moq-pub -p moq-sub --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
