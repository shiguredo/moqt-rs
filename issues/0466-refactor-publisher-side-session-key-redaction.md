# moq-pub の side session キーを認可トークンの実値を含まない形にする

- Created: 2026-11-20
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-publisher-side-session-key-redaction
- Polished: {YYYY-MM-DD}

## 目的

`crates/moq-pub/src/pipeline.rs` の `side_session_key` は、side session の同一性キーを `format!("{}{}#{}", url.authority, url.path, fragment)` で組み立てる。`url.path` は query を含み、fragment は `type` と `value` を実値のまま連結するため、キーには query の token と fragment の `cat` などの認可トークンが残る。

現時点でキーをログに出していないため実害はないが、キーの doc は「この文字列はログに出さない。fragment には認可トークンが含まれうるためである。」と、ログ出力を制限している。将来 side session のログにキーを出すと認可トークンが漏れる。moq-sub の `side_session_url_key` と同じく、キーとログ表示を分離する。

## 現状

- `crates/moq-pub/src/pipeline.rs` の `side_session_key` は `format!("{}{}#{}", url.authority, url.path, fragment)` でキーを作る。`url.path` は query を含み (`crates/tokio-moq/src/lib.rs` の `ServerUrl::path`)、fragment は `type` と `value` を実値のまま連結する。
- キーは `request_migration_sessions` と `RequestMigrationEvent` の照合に使う。ログには `authority` だけを出しており、キーは出していない。
- `side_session_key_tests` は同じ URL・異なる fragment・異なる authority / path を固定するが、キーに認可トークンの実値が含まれないことは検査していない。

## 設計方針

- `side_session_key` は同一性のためのキーとして、authority / path / fragment を区別できる可読な文字列を返す。query と fragment は実値ではなく `DefaultHasher` によるハッシュを埋め込む。
- ログ表示用の関数 (`side_session_url_display` など) を追加し、`?` より前の path、query のハッシュ、fragment のハッシュ、userinfo を除いた authority だけを返す。キーは同一性判定とイベントの照合にだけ使う。
- `crates/moq-pub/src/pipeline.rs` の単体テストに、query と fragment にダミーのトークンを付けた URL を追加し、キーと表示用の値に実値が含まれないこと、query / fragment が異なる URL のキーが互いに異なることを固定する。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 完了条件

- `side_session_key` が query と fragment の認可トークンの実値を含まないこと。
- query だけが異なる URL と fragment だけが異なる URL が別の side session キーになること。
- authority / path の違いが引き続きキーに反映されること。
- 表示用の関数の出力に query / fragment の実値と authority の userinfo (`@` より前) が含まれないこと。
- 上記を `crates/moq-pub/src/pipeline.rs` の単体テストで固定すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
