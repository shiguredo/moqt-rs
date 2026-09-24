# `moqt:` の後に `//` が無い URL を authority 欠落として報告する

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-parse-url-missing-slashes
- Polished: {YYYY-MM-DD}

## 目的

`examples/moqt-transport/src/lib.rs` の `parse_url` が、未対応 scheme と `//authority` の欠落を区別して報告するようにする。
現在は `moqt:/app` も `moqt:example.com/app` も「unsupported URL scheme」と表示され、原因が分からない。

## 現状

`parse_url` は `url.split_once("://")` で scheme を取り出すため、`//` が無い URL は scheme が取れず
`unsupported URL scheme: moqt:/app (use moqt:// or https://)` になる。
実測: `moqt:/app` はこのメッセージで、publisher では `argument '--url' has an invalid value ...` として表示される。

RFC 3986 §3 の URI 構文は `//` の有無で authority の有無が決まる。
draft-ietf-moq-transport-21 §6.1 も `moqt-URI = "moqt" "://" authority path-abempty [ "?" query ]` と `//` を必須にしている。
未対応 scheme の `ftp://host/path` と、対応 scheme で `//` が無い `moqt:/app` は原因が異なる。

## 設計方針

- `:` より前を scheme として取り出し、RFC 3986 §3.1 に従い大文字小文字非区別で比較する
- scheme が `moqt` / `https` の場合に `//` が無ければ、authority の欠落として `{scheme}:// URL requires authority: {url} ...` を返す
- 未対応 scheme は従来どおり `unsupported URL scheme: {url} ...` を返す
- エラーメッセージはすべて入力 URL を含む
- scheme の比較と `Transport` の決定は既存の挙動 (大文字 scheme の受理、scheme だけを小文字化) を維持する

## 完了条件

- `moqt:/app` と `moqt:example.com/app` が authority の欠落を示すエラーになり、メッセージに入力 URL が含まれること
- `ftp://host/path` と `://host/path` は未対応 scheme のエラーのままであること
- `parse_url("moqt://")` / `parse_url("https:///app")` の既存の authority エラーが変わらないこと
- `moqt://` / `https://` / 大文字 scheme / query 内の `://` の既存テストが変わらないこと
