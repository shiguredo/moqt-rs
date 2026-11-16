# MSF fragment の予約パラメータ c4m を cat に改名する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters) の Table 12 は、予約パラメータ名を `c4m` から `cat` に改名した。値の意味 (Base64 で符号化した C4M トークン) は変わらない。§11.5 と §11.1.3 の例も `cat=` になっている。
現行実装は `c4m` だけを予約パラメータとして扱うため、v02 の URL で `cat=` を渡しても認可トークンが取り出されず、SETUP と PUBLISH / SUBSCRIBE / FETCH / REQUEST_UPDATE にトークンが載らない。

## 現状

- `src/msf/uri.rs` の `MsfFragment::c4m_tokens` は `parameter_values("c4m")` だけを返す。
- `crates/tokio-moq/src/lib.rs` の `ServerUrl::c4m_tokens`、`parse_c4m_tokens`、`decode_c4m_token` は `c4m` 前提である。
- `crates/moq-pub` の `CatalogParams.c4m_tokens` と `C4M_AUTH_INFO_VALUE` (`"%c4m%"`) も `c4m` 前提である。
- `crates/README.md` と `crates/tokio-moq/README.md` は `#msf:<track-identifier>&c4m=<token>` の例を掲載している。
- v02 §11.1.1 は `cat` を予約パラメータとし、`c4m` を予約の一覧に含めない。§11.5 の例は `&cat=` である。

## 設計方針

- 予約パラメータ名を `cat` に変更する。`MSF` fragment のパースと `ServerUrl` のトークン抽出、moq-pub の変数、README の例をすべて `cat` に揃える。
- `c4m` を予約パラメータとして受理しない。v02 は `c4m` を予約の一覧から外しており、互換のために受理すると「予約でないパラメータを認可トークンとして扱う」危険な経路が残る。受理が必要になった場合は別 issue で判断する。
- 公開 API の `c4m_tokens` の改名は破壊的変更になる。`CODEBASE.md` の「破壊的変更は積極的に行う」に従い、別名の残置はしない。
- 変数値の `%c4m%` は `%cat%` に改名し、`crates/moq-pub` のカタログ生成を追随させる。

## 完了条件

- `#msf:namespace--name&cat=<base64>` の `cat` が認可トークンとして取り出され、SETUP と PUBLISH / SUBSCRIBE / FETCH / REQUEST_UPDATE の AUTHORIZATION_TOKEN に載ること。
- `c4m` は予約パラメータとして扱われず、トークン抽出の対象にならないこと。
- `%cat%` の変数がカタログの `authInfo` に解決されること。
- `crates/README.md` と `crates/tokio-moq/README.md` の例が `cat` に更新されていること。
- 上記を `tests/test_msf/uri.rs` と `crates/tokio-moq` のテストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
