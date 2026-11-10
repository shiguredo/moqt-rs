# parse_msf_uri で authority の userinfo を拒否する

- Created: 2026-11-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-uri-userinfo
- Polished: {YYYY-MM-DD}

## 目的

`src/msf/uri.rs` の `parse_msf_uri` は authority の userinfo (`user:password@host`) を受け付け、そのまま `MsfUri::authority` に保持する。
tokio-moq の `parse_url` は userinfo を拒否しており、同じ MOQT URI の解釈がライブラリと example で食い違う。RFC 3986 §3.2.1 は userinfo の "user:password" 形式を deprecated とし、受け取った reference 中の userinfo を拒否する選択を許す。ライブラリでも拒否し、パスワードを保持・表示しないようにする。

## 現状

- `parse_msf_uri` は `moqt://` の後ろを `#` / `?` / `/` で区切り、`/` より前を authority として空でないことだけを検証する。`@` を含む authority をそのまま受け付ける。
- `MsfUri` の `Debug` は `authority` を表示するため、userinfo のパスワードが `{:?}` に出る。
- `crates/tokio-moq/src/lib.rs` の `authority_parts` は `authority.contains('@')` で userinfo を `"userinfo is not supported"` として拒否する。その doc は「draft-ietf-moq-transport-23 §6.1 は userinfo に言及せず、RFC 3986 §3.2.1 は受け取った reference 中の userinfo を reject する選択を許す」とする。
- `refs/rfc3986.txt` §3.2.1: "Use of the format "user:password" in the userinfo field is deprecated." / "Applications may choose to ignore or reject such data when it is received as part of a reference".

## 設計方針

- `parse_msf_uri` は authority に `@` を含む場合、`MessageError::InvalidCatalog` で拒否する。エラーメッセージに authority を埋め込まない。
- 拒否の根拠 (RFC 3986 §3.2.1、tokio-moq の `parse_url` と同じ扱い) を doc に書く。

## 完了条件

- `parse_msf_uri` が userinfo を含む MSF URI (`moqt://user@host/...#msf:...`、`moqt://user:secret@host/...#msf:...`) を拒否し、エラーメッセージに userinfo が含まれないこと。
- userinfo を含まない既存の MSF URI の解析結果が変わらないこと。
- 上記を `tests/test_msf/uri.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
