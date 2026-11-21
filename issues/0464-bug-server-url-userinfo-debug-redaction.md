# ServerUrl の authority に含まれる userinfo を Debug から隠す

- Created: 2026-11-20
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-server-url-userinfo-debug-redaction
- Polished: 2026-11-20

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
- `parse_url` の userinfo の扱い (受理するか拒否するか) は変えない。`parse_url` / `ServerUrl` の扱いを変えない方針は別 issue (`issues/0424-bug-msf-uri-userinfo.md`) にも明記されているため、本 issue は `Debug` の表示と、それに伴う doc コメントの更新だけを対象にする。
- `crates/tokio-moq/README.md` の `ServerUrl` の `Debug` の説明に、userinfo を伏せることを追記する。
- `crates/tokio-moq/src/lib.rs` の `ServerUrl` の doc コメントの `Debug` の説明を、authority の userinfo を伏せる実装に合わせて更新する。`ServerUrl::authority` の doc コメントにも、`parse_url` が返す値では userinfo (`user:password@host`) を含み得ることを追記する。

## 完了条件

- `ServerUrl` の `{:?}` に、authority の userinfo (`@` より前) が文字列としても十進のバイト列としても含まれないこと。
- `@` より後ろの host と port は従来どおり表示されること。
- `authority` に `@` が含まれない場合の表示が変わらないこと。
- `MigrationTarget` と `ConnectParams` の `{:?}` でも同じ秘匿が通ること。
- 上記を `crates/tokio-moq/src/lib.rs` の `#[cfg(test)] mod tests` に追加するテストで固定すること。
- `crates/tokio-moq/README.md` の `ServerUrl` の `Debug` の説明が実装と一致すること。
- `crates/tokio-moq/src/lib.rs` の `ServerUrl` と `ServerUrl::authority` の doc コメントが、上記の `Debug` の表示と `parse_url` が返す値を説明していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

`crates/tokio-moq/src/lib.rs` の `ServerUrl` の独自 `Debug` で、authority の userinfo を実値で表示しないようにした。

- authority に `@` を含む場合、最後の `@` より前 (userinfo) を実値で表示せず、`userinfo_bytes` にバイト長だけを表示する。`@` より後ろの host / port は機密値でないため `authority` にそのまま表示する。
- authority に `@` を含まない場合は `userinfo_bytes` を表示せず、`authority` を含めて従来と同じ表示にする (完了条件「表示が変わらないこと」を満たす)。
- 判別は private ヘルパ `authority_debug_fields` に切り出した。区切りは最後の `@` とし、userinfo に `@` が含まれる入力でもパスワードの後半が表示されないようにする。
- `ServerUrl` の型 doc と `ServerUrl::authority` の doc に、`parse_url` が userinfo を含む authority を保持し得ることと、`Debug` が userinfo を実値で表示しないことを書いた。`crates/tokio-moq/README.md` にも同じ内容を追記した。
- テストは `crates/tokio-moq/src/lib.rs` の `tests` に追加した。`server_url_debug_hides_userinfo` は userinfo の実値が文字列としても十進のバイト列としても現れず、host / port とバイト長 (非 ASCII では文字数でなくバイト数) が表示されることを固定する。
`server_url_debug_hides_userinfo_before_the_last_at_sign` は複数の `@`、`server_url_debug_hides_userinfo_at_edges` は先頭・末尾の `@` を固定する。`server_url_debug_keeps_authority_without_userinfo` は `@` を含まない場合の表示が変わらないことを固定する。
`migration_and_connect_params_debug_hide_url_secrets` にも userinfo の検査を足し、`MigrationTarget` と `ConnectParams` の `Debug` が同じ秘匿を通ることを確認する。
