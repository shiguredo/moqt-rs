# publisher / subscriber の --help がヘルプを表示しない

- Created: 2026-09-24
- Completed: 2026-09-30
- Branch: feature/fix-moqt-example-help-flag
- Polished: 2026-09-27

## 目的

`moqt-publisher` / `moqt-subscriber` の `--help` / `-h` がヘルプを表示せず、必須オプション欠如のエラーで終了する。利用者が CLI オプションを確認する手段が無い状態を直す。

## 現状

- `moqt-publisher --help` と `moqt-subscriber -h` はいずれも `missing '--url' option` を出力して exit 1 になる。`--version` は動作する。
- 原因は noargs の挙動で、help モードでも `default` / `example` を持たない必須オプションは `Opt::None` を返す。`then()` が `Error::MissingOpt` を返すため `args.finish()` に到達せず、ヘルプが表示されない。
- `examples/moqt-publisher/src/cli.rs` と `examples/moqt-subscriber/src/cli.rs` の `--url` はいずれも必須で `.default()` / `.example()` を持たない。
- `examples/README.md` は「全オプションは各クレートの `--help` で確認できる」と案内しているが、実際には確認できない。

## 設計方針

- `--url` に `.example(...)` を追加し、help モードでも `Opt::None` にならないようにする。noargs にヘルプ表示用の値を与えるのが目的で、必須オプションの検証 (`then()` のエラー) は通常の実行経路で維持する。
- publisher / subscriber の両方で同じ対応を行う。
- `--help` の出力に `--url` の説明が表示され、正常終了することを確認する。
- `--url` を省略した通常の実行では、これまでどおり必須オプション欠如のエラーになることを確認する。

## 完了条件

- `moqt-publisher --help` / `moqt-subscriber --help` がヘルプを表示して正常終了すること
- `-h` でも同じであること
- ヘルプに `--url` の説明 (`moqt://host[:port]/path or https://host[:port]/path`) が表示されること
- `--url` を省略した通常の実行では `missing '--url' option` でエラーになること
- `examples/README.md` の「全オプションは各クレートの `--help` で確認できる」という記述と実装が一致すること

## 解決方法

`examples/moq-pub/src/cli.rs` と `examples/moq-sub/src/cli.rs` の `--url` に `.example("moqt://relay.example.com:4433/app")` を追加した。

- noargs は help モードでも `default` / `example` を持たない必須オプションを `Opt::None` にし、`then()` が `MissingOpt` を返すため `args.finish()` に到達しなかった。`.example()` を与えると help モードでは `Opt::Example` になるため、`--url` の検証を通過して `finish()` がヘルプを返す
- 例は `parse_url` を通るため、正当な `moqt://` URL を使う。通常の実行では例は使われず、`--url` の省略は従来どおり `MissingOpt` になる
- `--url` の doc を `Server URL (moqt://host[:port]/path; ...)` に変更し、ヘルプに URL の形式が表示されるようにした

追加したテスト:

- `examples/moq-pub/tests/test_cli.rs` と `examples/moq-sub/tests/test_cli.rs` (実行ファイル経由)
  - `help_prints_help_and_exits_successfully`: `--help` / `-h` が終了コード 0 で `Usage:` と `--url` の説明と `moqt://` を出力し、`missing '--url' option` を出さないこと
  - `missing_url_is_reported_as_an_error`: `--url` を省略した実行が終了コード 1 と `--url` を含むエラーになること

実機確認:

- `moq-pub --help` / `moq-sub -h` が `Usage: ... --url <URL>`、`Example: $ ... --url moqt://relay.example.com:4433/app`、`--url, -u <URL> Server URL (moqt://host[:port]/path; ...)` を表示して exit 0
- `moq-pub` (`--url` なし) は `missing '--url' option` を表示して exit 1
- `examples/README.md` の「全オプションは各クレートの `--help` で確認できる」という記述と実装が一致するようになった

`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ることを確認した。
