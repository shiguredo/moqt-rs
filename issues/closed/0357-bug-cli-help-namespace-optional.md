# moq-pub / moq-sub の --help で省略可能な --namespace の必須表示と説明行を修正する

- Created: 2026-10-20
- Completed: 2026-11-22
- Branch: feature/fix-cli-help-namespace-optional
- Polished: 2026-10-25

## 目的

`--help` の Usage が省略可能な `--namespace` を必須として表示し、moq-pub の説明行が出ない。利用者がオプションの必須・任意を誤解しないよう、help 表示を実装に合わせる。

## 現状

- `crates/moq-pub/src/cli.rs` と `crates/moq-sub/src/cli.rs` の `--namespace` は `.example("moq-example")` を持つ。noargs の `build_usage` は `.example` を持つオプションを必須オプションとして `Usage:` に `--namespace <NS>` と表示するため、通常実行では省略可能な `--namespace` が必須に見える
- `--namespace` を省略した場合は `--url` の MSF fragment の track-identifier から namespace を解決する (`resolve_namespace`)。省略可能であることは `cli.rs` のコメントにも書かれている
- `crates/moq-pub/Cargo.toml` に `description` が無く、`crates/moq-pub/src/cli.rs` の `env!("CARGO_PKG_DESCRIPTION")` が空になるため、`moq-pub --help` の説明行が出ない。`crates/moq-sub/src/cli.rs` は `app_description` を固定文字列で設定しているためこの問題は無い

## 変更対象

- `crates/moq-pub/src/cli.rs`
- `crates/moq-sub/src/cli.rs`
- `crates/moq-pub/Cargo.toml`
- `crates/moq-pub/tests/test_cli.rs`
- `crates/moq-sub/tests/test_cli.rs`

## 設計方針

- `--namespace` の `.example` を削除して `Usage:` に `--namespace <NS>` を表示させない。`.example` を削除すると help モードでは `resolve_namespace(None, &url)` が例の URL から namespace を解決するため、`--url` の `.example` を MSF fragment 付きの URL に変更して解決できるようにする (`Config` を返さないため解決結果は使われない)。通常実行の `--namespace` 省略時の MSF fragment 解決は変えない
- `--namespace` の `.example` を削除すると `Example:` から `--namespace moq-example` が消えるため、`--url` の `.example` を `moqt://relay.example.com:4433/app#msf:moq-example--video` に変更し、`--namespace` を省略しても MSF fragment から namespace を解決できる例にする。`--url` の `.example` 自体は必須オプションの help 表示のために残す
- `crates/moq-pub/Cargo.toml` に `description = "MOQT publisher client"` を追加し、`moq-pub --help` の説明行が出るようにする。`crates/moq-sub/src/cli.rs` の `app_description = "MOQT subscriber client"` と対にする

## 完了条件

- `moq-pub --help` と `moq-sub --help` の `Usage:` に `--url <URL>` が表示され、`--namespace <NS>` が必須オプションとして表示されないこと
- `--namespace` を省略した通常実行で、`--url` の MSF fragment から namespace が解決されること
- `moq-pub --help` と `moq-sub --help` の `Example:` に表示される `--url` が MSF fragment を含み、`--namespace` を省略しても namespace を解決できること
- `moq-pub --help` の先頭に `MOQT publisher client` が表示されること
- 上記の help 表示を `crates/moq-pub/tests/test_cli.rs` / `crates/moq-sub/tests/test_cli.rs` で固定すること
- `cargo test -p moq-pub -p moq-sub` / `cargo clippy -p moq-pub -p moq-sub --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること

## 解決方法

- `--namespace` から `.example("moq-example")` を削除した。noargs は `.example` を持つ
  オプションを必須として `Usage:` に表示するため、省略可能なオプションには例を与えない。
- `--url` の `.example` を `moqt://relay.example.com:4433/app#msf:moq-example--video` に
  変更した。help モードでは `--namespace` の指定が無いため、この MSF fragment から
  namespace を解決する。
- `crates/moq-pub/Cargo.toml` に `description = "MOQT publisher client"` を追加し、
  `moq-pub --help` の説明行が出るようにした。
- `crates/moq-pub/tests/test_cli.rs` / `crates/moq-sub/tests/test_cli.rs` で、`Usage:` 行に
  `--namespace` が含まれないこと、`Example:` が `#msf:moq-example--video` を含むこと、
  moq-pub / moq-sub の説明行が出ることを固定した。

`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、
`cargo fmt --all -- --check` が成功した。
