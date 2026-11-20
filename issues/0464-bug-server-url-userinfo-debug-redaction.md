# ServerUrl の authority に含まれる userinfo を Debug から隠す

- Created: 2026-11-20
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-server-url-userinfo-debug-redaction
- Polished: {YYYY-MM-DD}

## 目的

`crates/tokio-moq/src/lib.rs` の `ServerUrl` は独自の `Debug` を実装し、query・fragment・`cat` トークンの実値を伏せているが、`authority` は実値のまま表示する。`parse_url` は authority に userinfo (`user:password@host`) を含む URL を拒否しないため、`parse_url` の戻り値を `{:?}` で出力するとパスワードがログに残る。

実測では `moqt://alice:s3cr3t@relay.example.com/live` の `Debug` は `ServerUrl { authority: "alice:s3cr3t@relay.example.com", ... }` になる。

RFC 3986 §3.2.1 は `user:password` 形式の userinfo を deprecated とし、最初の `:` より後ろのデータを平文で表示しないこと、受け取った reference 中の userinfo を拒否してよいことを定める。同じ理由で `src/msf/uri.rs` の `parse_msf_uri` は userinfo を拒否する対応が別 issue で進んでいる。

## 現状

- `crates/tokio-moq/src/lib.rs` の `parse_url` は authority を入力のまま `ServerUrl::authority` に保持し、`@` を検証しない。接続時の `authority_parts` が `authority.contains('@')` で userinfo を拒否するが、`parse_url` の時点では保持される。
- `ServerUrl` の独自 `Debug` は `authority` を実値のまま `debug_struct` に渡している。
- `MigrationTarget` の `Debug` は `ServerUrl` の `Debug` を通るため、同じ漏えい経路になる。ただし `migration_target` は userinfo を含む移行先を拒否するため、peer 由来の値は到達しない。
- `crates/tokio-moq/README.md` は「userinfo を含まない authority の拒否メッセージには、拒否理由と入力した authority が含まれる。userinfo を含む authority はパスワードなどの機密値を含み得るため、メッセージに authority を含めない」と書き分けているが、`ServerUrl` の `Debug` については記述していない。
- 影響: `--url` に userinfo 付きの URL を渡した利用者が、接続前のログやデバッグ出力で `{:?}` を使うとパスワードが残る。接続は `authority_parts` で失敗するが、その前に出力され得る。

## 設計方針

`ServerUrl` の `Debug` で authority の userinfo を伏せる。

- `authority` に `@` が含まれる場合、`@` より前 (userinfo) を実値で表示せず、`userinfo_bytes` のようなバイト長に置き換える。`@` より後ろ (host + port) は機密値でないため従来どおり表示する。
- `authority` に `@` が含まれない場合は従来どおり実値を表示する。
- 判別のための private ヘルパ (`authority_debug_fields` など) を追加し、`Debug` の実装から呼ぶ。`derive(Debug)` は使わない (既に独自実装である)。
- `parse_url` の userinfo の扱い (受理するか拒否するか) は変えない。`parse_url` / `ServerUrl` の扱いを変えない方針は別 issue (`issues/0424-bug-msf-uri-userinfo.md`) にも明記されているため、本 issue は `Debug` の表示だけを対象にする。
- `crates/tokio-moq/README.md` の `ServerUrl` の `Debug` の説明に、userinfo を伏せることを追記する。

## 完了条件

- `ServerUrl` の `{:?}` に、authority の userinfo (`@` より前) が文字列としても十進のバイト列としても含まれないこと。
- `@` より後ろの host と port は従来どおり表示されること。
- `authority` に `@` が含まれない場合の表示が変わらないこと。
- `MigrationTarget` と `ConnectParams` の `{:?}` でも同じ秘匿が通ること。
- 上記を `crates/tokio-moq/src/lib.rs` の `#[cfg(test)] mod tests` に追加するテストで固定すること。
- `crates/tokio-moq/README.md` の `ServerUrl` の `Debug` の説明が実装と一致すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
