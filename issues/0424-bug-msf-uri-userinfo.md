# parse_msf_uri で authority の userinfo を拒否する

- Created: 2026-11-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-uri-userinfo
- Polished: 2026-11-14

## 目的

`src/msf/uri.rs` の `parse_msf_uri` は authority の userinfo (`user:password@host`) を受け付け、そのまま `MsfUri::authority` に保持する。
tokio-moq の `authority_parts` は接続時に userinfo を拒否しており、同じ MOQT URI が library の `parse_msf_uri` では受理され、tokio-moq の接続経路では拒否される。
RFC 3986 §3.2.1 は userinfo の "user:password" 形式を deprecated とし、受け取った reference 中の userinfo を拒否する選択を許す。同節は最初の `:` より後ろのデータを、`:` より後ろが空の場合を除いて平文で表示しないことを勧めており、パスワードの表示を避ける根拠になる。
draft-ietf-moq-msf-01 §11.1 は authority を "Contains the host and optional port" として説明しており、authority の説明では userinfo に言及しない。
library の `parse_msf_uri` でも拒否し、パスワードを保持・表示しないようにする。

## 現状

- `parse_msf_uri` は `moqt://` の後ろを `#` / `?` / `/` で区切り、`/` より前を authority として空でないことだけを検証する。`@` を含む authority をそのまま受け付ける。実測: `moqt://user:secret@host/path#msf:ns--catalog` は `authority = "user:secret@host"` で成功する。
- `MsfUri` の `Debug` は `authority` を表示するため、userinfo のパスワードが `{:?}` に出る。実測: `MsfUri { authority: "user:secret@host", ... }` と表示される。
- `crates/tokio-moq/src/lib.rs` の `parse_url` は authority をそのまま `ServerUrl::authority` に保持し、userinfo を拒否しない。接続時の
  `authority_parts` が `authority.contains('@')` で userinfo を `"userinfo is not supported"` として拒否する。その doc
  は「draft-ietf-moq-transport-23 §6.1 は userinfo に言及せず、RFC 3986 §3.2.1 は受け取った reference 中の userinfo を reject する選択を許す」とする。
- `ServerUrl` の `Debug` は `authority` を表示するため、`parse_url` を通した `{:?}` には userinfo が残る。本 issue は library の `parse_msf_uri` を対象にし、`ServerUrl` の扱いは変えない。
- `refs/rfc3986.txt` §3.2.1: "Use of the format "user:password" in the userinfo field is deprecated." / "Applications
  should not render as clear text any data after the first colon (":") character found within a userinfo subcomponent
  unless the data after the colon is the empty string (indicating no password)." / "Applications may choose to ignore or
  reject such data when it is received as part of a reference".
- `refs/moq/draft-ietf-moq-msf-01.txt` §11.1: "Authority: Required. Contains the host and optional port (defaulting to 443)."

## 設計方針

- `parse_msf_uri` は authority に `@` を含む場合、`MessageError::InvalidCatalog` で拒否する。エラーメッセージに authority の値を埋め込まない。
- `parse_msf_uri` の `# Errors` に authority に `@` を含む場合の `InvalidCatalog` を追加し、拒否の根拠 (RFC 3986 §3.2.1、draft-ietf-moq-msf-01 §11.1 の authority の説明、tokio-moq の `authority_parts` と同じ扱い) を書く。`MsfUri::authority` の doc に、`parse_msf_uri` が返す値では userinfo (`@`) を含まないことを書く。
- authority の `@` だけを判定し、path / query の `@` は従来どおり受理する (`@` は RFC 3986 §3.3 の `pchar` に含まれる)。`parse_url` / `ServerUrl` の扱いは変えない。

## 完了条件

- `parse_msf_uri` が userinfo を含む MSF URI (`moqt://user@host/path#msf:ns--catalog`、`moqt://user:secret@host/path#msf:ns--catalog`) を `MessageError::InvalidCatalog` で拒否し、エラーメッセージに authority の値 (userinfo) が含まれないこと。
- authority に `@` を含まない既存の MSF URI の解析結果が変わらないこと。path / query に `@` を含む MSF URI (`moqt://host/a@b#msf:ns--catalog`、`moqt://host/path?x=a@b#msf:ns--catalog`) も従来どおり受理されること。
- `parse_msf_uri` の `# Errors` に authority に `@` を含む場合が追加され、`MsfUri::authority` の doc に userinfo (`@`) を含まないことが書かれていること。
- 上記を `tests/test_msf/uri.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
