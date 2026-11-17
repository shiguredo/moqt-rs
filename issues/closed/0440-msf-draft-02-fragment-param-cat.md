# MSF fragment の予約パラメータ c4m を cat に改名する

- Created: 2026-11-16
- Completed: 2026-11-17
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters) の Table 12 は、予約パラメータ名を `c4m` から `cat` に改名した。Appendix A.1.7 も「The reserved fragment parameter "c4m" was renamed "cat"」と明記する。値の意味 (Base64 で符号化した C4M トークン) は変わらない。

現行実装は `c4m` だけを予約パラメータとして扱うため、v02 の URL で `cat=` を渡しても認可トークンが取り出されず、SETUP と PUBLISH / SUBSCRIBE / FETCH / REQUEST_UPDATE にトークンが載らない。

なお v02 は `c4m` の受理を禁止していない。本リポジトリでは、予約でないパラメータを認可トークンの供給元にしない fail-closed の方針として `c4m` を扱わない。

## 現状

- `src/msf/uri.rs` の `MsfFragment::c4m_tokens` は `parameter_values("c4m")` だけを返す。`cat` は汎用パラメータとして保持されるが、トークンとしては取り出されない。
- `crates/tokio-moq/src/lib.rs` の `ServerUrl::c4m_tokens` (公開フィールド)、`parse_c4m_tokens`、`decode_c4m_token` は `c4m` 前提である。
- `crates/moq-pub` の `CatalogParams.c4m_tokens` と `C4M_AUTH_INFO_VALUE` (`"%c4m%"`) も `c4m` 前提である。カタログ生成テストは `%c4m%` という未解決の参照文字列を期待している。
- `crates/moq-pub/src/cli.rs` と `crates/moq-sub/src/cli.rs` の `--url` help が `c4m` を案内している。`crates/README.md` と `crates/tokio-moq/README.md` も例と説明文で `c4m` / `ServerUrl::c4m_tokens` を案内している。
- `resolve_catalog_variables` を呼ぶ経路は `crates/` にない。`crates/moq-sub` は `MsfCatalogDocument::decode` を直接呼ぶため、`%cat%` の end-to-end な解決は本 issue の範囲外である。
- トークンは SETUP と PUBLISH / SUBSCRIBE / FETCH / REQUEST_UPDATE の AUTHORIZATION_TOKEN に載る。

## 設計方針

- 予約パラメータ名を `cat` に変更する。`MSF` fragment のパースと `ServerUrl` のトークン抽出、moq-pub の変数を `cat` に揃える。
- `c4m` を予約パラメータとして受理しない。v02 が禁止しているわけではないため、この判断はリポジトリの fail-closed 方針としてコードの doc と issue に明記する。
- 公開 API の `c4m_tokens` の改名は破壊的変更になる。`CODEBASE.md` の「破壊的変更は積極的に行う」に従い、別名の残置はしない。
- `C4M_AUTH_INFO_VALUE` を `"%cat%"` に改名し、カタログ生成テストの期待値を `%cat%` に更新する。変数解決の呼び出し経路の追加は行わない。
- CLI help と README の例・説明文を `cat` / `cat_tokens` に更新する。利用者が help に従って `&c4m=` を指定すると、トークンが載らないままになるため、help の更新は必須とする。
- トークンを SETUP と各制御メッセージに載せる挙動は変えない。

## 完了条件

- `#msf:namespace--name&cat=<base64>` の `cat` が認可トークンとして取り出され、SETUP と PUBLISH / SUBSCRIBE / FETCH / REQUEST_UPDATE の AUTHORIZATION_TOKEN に載ること。
- `c4m` は予約パラメータとして扱われず、トークン抽出の対象にならないこと (fail-closed 方針として doc に明記)。
- `crates/moq-pub` がカタログの `authInfo` に `"%cat%"` を載せ、既存テストの期待値が `%cat%` になっていること。
- `crates/moq-pub/src/cli.rs`、`crates/moq-sub/src/cli.rs`、`crates/README.md`、`crates/tokio-moq/README.md` の `c4m` の案内と `c4m_tokens` の記述が `cat` / `cat_tokens` に更新されていること。
- `%cat%` の end-to-end な解決は、`crates/moq-sub` の変数置換経路が未実装のため本 issue の完了条件に含めないことを doc に明記すること。
- 上記を `tests/test_msf/uri.rs`、`crates/tokio-moq` のテスト、`crates/moq-pub` のカタログ生成テストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- MSF fragment の予約パラメータを `c4m` から `cat` に変更した。`MsfFragment::c4m_tokens` を `cat_tokens` に改名し、`parameter_values("cat")` だけを返すようにした。
- `crates/tokio-moq` の `ServerUrl::c4m_tokens` を `cat_tokens` に改名し、内部の `parse_c4m_tokens` / `decode_c4m_token` も `parse_cat_tokens` / `decode_cat_token` に改名した。SETUP と PUBLISH / SUBSCRIBE / FETCH / REQUEST_UPDATE にトークンを載せる挙動は変えていない。
- 旧 `c4m` は予約パラメータとして扱わず、認可トークンの供給元にしない fail-closed 方針を doc とテストで明記した。`c4m` は未知の汎用パラメータとして保持される。
- `crates/moq-pub` の `CatalogParams::c4m_tokens` を `cat_tokens` に、`C4M_AUTH_INFO_VALUE` を `CAT_AUTH_INFO_VALUE` (`"%cat%"`) に改名し、カタログの authInfo とテストの期待値を `%cat%` に更新した。
- `crates/moq-pub/src/cli.rs`、`crates/moq-sub/src/cli.rs`、`crates/README.md`、`crates/tokio-moq/README.md`、`docs/msf.md`、`docs/moqt.md`、`docs/missing-features.md` の `c4m` / `c4m_tokens` の案内を `cat` / `cat_tokens` に更新した。
- `tests/test_msf/uri.rs` と `crates/tokio-moq` のテストに、`cat` の抽出、旧 `c4m` が認可トークンにならないこと、Base64 / percent-encoding の既存挙動を追加・追従した。`crates/moq-pub` のカタログ生成テストは `"%cat%"` を固定する。
- `%cat%` の end-to-end な解決は、moq-sub に変数置換の呼び出し経路が無いため本 issue の範囲外であることを doc に明記した。

確認コマンド: `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` が通ることを確認した。
