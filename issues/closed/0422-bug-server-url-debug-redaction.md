# tokio-moq の ServerUrl の Debug 出力から token を含む値を隠す

- Created: 2026-11-10
- Completed: 2026-11-20
- Branch: feature/fix-server-url-debug-redaction
- Polished: 2026-11-14

## 目的

`crates/tokio-moq/src/lib.rs` の `ServerUrl` は `derive(Debug)` であり、`{:?}` で出力すると URL の query と fragment に含まれる認可トークンが現れる。
MSF の公開型 (`MsfUri` / `MsfFragmentParameter` など) は Debug で値を伏せるようにしたが、`ServerUrl` は生の文字列とトークンのバイト列をそのまま持つ。ログ出力で機密情報が漏れないよう、同じ方針に揃える。

## 現状

- `ServerUrl` は `#[derive(Debug, Clone)]` で、次のフィールドを持つ。
  - `authority`: ホスト名または IP アドレス (ポートを含む場合がある)。token を含まないため表示する。
  - `path`: パス部分 (query を含み、fragment を含まない)。query のパラメータは token を含み得る (draft-ietf-moq-msf-01 §11.4.2 "Tokens MAY also be supplied as parameters in the connection URI.")。
  - `fragment: Option<MoqtFragment>`: `MoqtFragment` (`derive(Debug)`) の `value` は percent-encoding されたままの fragment の生文字列で、`c4m` などの token パラメータを含む。
  - `msf: Option<MsfFragment>`: `MsfFragmentParameter` の `Debug` で値は伏せられる。
  - `c4m_tokens: Vec<Vec<u8>>`: `c4m` パラメータから取り出した C4M 認可トークンのバイト列。
- `MigrationTarget` (`derive(Debug)`) は `url: ServerUrl` を、`ConnectParams` (`derive(Debug)`) は `url: &ServerUrl` を持つ。
- 現時点で `crates/` に `ServerUrl` / `MigrationTarget` / `ConnectParams` を `{:?}` で出力している箇所は無い。moq-pub / moq-sub の `Config` は `Debug` を持たない。
- `src/c4m/cat.rs` の `CatToken` と `src/c4m/crypto.rs` の `CoseKey`、`src/msf.rs` の `MsfTrack` などは、秘密値を長さだけ表示する独自 `Debug` を実装している。

## 設計方針

- `MoqtFragment` と `ServerUrl` に独自 `core::fmt::Debug` を実装する。表示形式は既存の前例 (`CatToken` / `CoseKey` / `MsfTrack`) と同じにする。
  - 伏せる `Option` でない文字列・バイト列は `_bytes` を付けた名前で長さを表示する (`MoqtFragment::value` は `value_bytes`)。
  - `ServerUrl::path` は query を含むため、query 部分を伏せる。`path` は `?` より前だけを表示し、query は `MsfUri` と同じく `query: Option<usize>` (バイト長) を表示する。
  - `c4m_tokens` は `c4m_token_count: <件数>` と `c4m_token_bytes: [<各トークンのバイト長>, ...]` を表示する。
  - `msf` は `MsfFragment` の `Debug` をそのまま使う。`authority` は表示する (userinfo は `authority_parts` が接続時に拒否するため、`parse_url` の戻り値に `@` を含む場合があるが、本 issue では token の秘匿に限定する)。
- `MigrationTarget` と `ConnectParams` は `derive(Debug)` のままでよい (`ServerUrl` の独自 `Debug` を通るため、内側の値は伏せられる)。
- 独自 `Debug` は `..` を使わずに全フィールドを分解してから `debug_struct` に渡し、フィールドの表示漏れをコンパイラが検出できるようにする。

## 完了条件

- `ServerUrl` / `MoqtFragment` / `MigrationTarget` / `ConnectParams` の `{:?}` に、query の値、fragment の生文字列、C4M トークンの値が、文字列としても十進のバイト列としても含まれないこと。
- 伏せたフィールドは長さを表示し、表示するフィールド (`authority`、query より前の path など) は従来どおり表示されること。
- 上記を `crates/tokio-moq/src/lib.rs` の `#[cfg(test)] mod tests` に追加するテストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

現在のコードは C4M draft-02 / MSF draft-02 に移行しており、予約パラメータは `c4m` ではなく `cat` である。本 issue の `c4m` は `cat` として対応した。

- `crates/tokio-moq/src/lib.rs` の `ServerUrl` と `MoqtFragment` から `derive(Debug)` を外し、独自の `core::fmt::Debug` を実装した。`..` を使わずに全フィールドを分解するため、フィールドを追加したときの表示漏れをコンパイラが検出できる。
- `MoqtFragment` の `Debug` は `fragment_type` を表示し、`value` は実値ではなくバイト長を `value_bytes` として表示する。
- `ServerUrl` の `Debug` は `authority` と path の `?` より前を表示し、`query: Option<usize>` に query のバイト長、`fragment` は `MoqtFragment` の `Debug`、`msf` は `MsfFragment` の `Debug` (`cat` の値は既存の `MsfFragmentParameter` の `Debug` が伏せる)、`cat_token_count` に `cat` の件数、`cat_token_bytes` に各トークンのバイト長の配列を表示する。
- `MigrationTarget` と `ConnectParams` は `derive(Debug)` のままで、内側の `ServerUrl` の `Debug` を通るため自動的に秘匿される。
- テストは `crates/tokio-moq/src/lib.rs` の単体テストに追加した。query の値と全体、fragment 全体、`cat` の Base64 表現、デコード済みトークンの十進列が、文字列としても十進のバイト列としても表示されないことを固定し、伏せたフィールドの長さと非機密フィールドの表示 (query なし、複数 `?`、複数 `cat`、`msf` 以外の fragment を含む) も固定した。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ることを確認した。
