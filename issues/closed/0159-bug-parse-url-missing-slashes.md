# `moqt:` の後に `//` が無い URL を authority 欠落として報告する

- Created: 2026-09-24
- Completed: 2026-10-06
- Branch: feature/fix-parse-url-missing-slashes
- Polished: 2026-09-27

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

- `:` より前を scheme として取り出し、RFC 3986 §3.1 に従い大文字小文字非区別で比較する。`:` が 1 個も無い URL は scheme を取り出せないため、未対応 scheme と同じエラーにする
- scheme が `moqt` / `https` の場合に `//` が無ければ、authority の欠落として `{scheme}:// URL requires authority: {url} ...` を返す (大文字 scheme でも同じ判定)
- 未対応 scheme は従来どおり `unsupported URL scheme: {url} ...` を返す
- エラーメッセージはすべて入力 URL を含む
- scheme の比較と `Transport` の決定は既存の挙動 (大文字 scheme の受理、scheme だけを小文字化) を維持する

## 解決方法

`examples/tokio-moq/src/lib.rs` (issue 作成時点のパスは `examples/moq-transport/src/lib.rs`) の
`parse_url` を、未対応 scheme と authority の欠落を区別して報告するようにした。

- scheme の切り出しを `split_once("://")` から `split_once(':')` に変え、RFC 3986 §3.1 に従い
  大文字小文字非区別で `moqt` と比較する (`:` を含まない URL は scheme を取り出せないため従来どおり
  未対応 scheme のエラー)
- scheme が `moqt` で `//` が無い場合は、authority の欠落として
  `moqt:// URL requires authority: {url} (e.g. moqt://localhost:4443)` を返す (`MOQT:/app` のような
  大文字 scheme でも同じ)。エラーメッセージの生成は 1 箇所に集約した
- `# Errors` と正規化の doc を実装に合わせ、単体テスト
  `parse_url_rejects_url_without_authority_prefix` を追加して `moqt:/app` / `moqt:example.com/app` /
  `MOQT:/app` / `MOQT:foo` / `moqt:///app` をメッセージ完全一致で固定した。
  `parse_url_rejects_unknown_scheme` に `://host/path` を追加した
- `parse_url("moqt://")` の authority エラーと、`moqt://` / 大文字 scheme / query 内の `://` の既存挙動は
  変えていない。なお `https://` は [CHANGES.md](../../CHANGES.md) の `[CHANGE]` で廃止済みのため、
  設計方針にあった「`https` も authority 欠落として報告する」は適用せず、未対応 scheme のままとした

## 完了条件

- `moqt:/app` と `moqt:example.com/app` が authority の欠落を示すエラーになり、メッセージに入力 URL が含まれること (`MOQT:/app` のような大文字 scheme でも同じエラーになること)
- `ftp://host/path` と `://host/path` と `:` を含まない URL は未対応 scheme のエラーのままであること
- `parse_url("moqt://")` / `parse_url("https:///app")` の既存の authority エラーが変わらないこと
- `moqt://` / `https://` / 大文字 scheme / query 内の `://` の既存テストが変わらないこと
- 追加した拒否ケースのテストが `examples/moqt-transport/src/lib.rs` に追加されていること (エラーメッセージの完全一致で URL の含有も固定すること)
