# AuthorizationToken の Debug が認可トークンの値を出力する

- Created: 2026-11-20
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-authorization-token-debug-redaction
- Polished: 2026-11-20

## 目的

`src/message_parameter.rs` の `AuthorizationToken` は `#[derive(Debug)]` であり、`REGISTER` / `USE_VALUE` の `token_value` をそのまま表示する。実測では `UseValue { token_type: 1, token_value: [222, 173, 190, 239] }` になり、十進のバイト列として認可トークンが現れる。

`AuthorizationToken` は `src/parameter.rs` の `SetupOptionValue::AuthorizationToken` に包まれ、`SetupOptions` の `Debug` を通じて `crates/tokio-moq/src/moqt_client.rs` の `MoqtClient::peer_setup_options` から公開 API として取り出せる。
実測では `SetupOptions([SetupOption { option_type: 3, value: AuthorizationToken(UseValue { token_type: 1, token_value: [222, 173, 190, 239] }) }])` になる。

利用者が `tracing::debug!("peer options: {options:?}")` のようなログを書くと、認可トークンがそのまま残る。ライブラリの他の認可情報を保持する型 (`MsfFragmentParameter` / `MsfUri` / `tokio_moq::ServerUrl` / `CatToken` / `CoseKey`) は、実値を伏せて長さを表示する方針に揃えている。`AuthorizationToken` と `AuthTokenCache` がこの方針から外れている。

## 現状

- `src/message_parameter.rs` の `AuthorizationToken` は `#[derive(Debug, Clone, PartialEq)]` で、`Register` / `UseValue` が `token_value: Vec<u8>` を持つ。
- `token_value` は draft-ietf-moq-transport-23 §9.1.4 (AUTHORIZATION TOKEN) の Token Value であり、種別固有のバイト列である。C4M の認可トークン (CAT) のように、それ自体が認可情報になる値を想定している。
- `src/parameter.rs` の `SetupOptionValue` と `SetupOption`、`SetupOptions` はすべて `#[derive(Debug)]` であり、内側の `AuthorizationToken` の `Debug` をそのまま使う。
- `src/message_parameter.rs` の `MessageParameterValue` と `MessageParameters` も `#[derive(Debug)]` であり、同じ `AuthorizationToken` の `Debug` を通る。
- `crates/tokio-moq/src/moqt_client.rs` の `MoqtClient::peer_setup_options` は `SetupOptions` を clone して `Option<SetupOptions>` として返し、`src/session/core.rs` の `Session::peer_setup_options` は `Option<&SetupOptions>` を返す公開 API であり、利用者側の `Debug` 出力に到達する。
- `src/session/auth_token_cache.rs` の `AuthTokenCache` も `#[derive(Debug)]` で REGISTER されたトークンの実値を `entries: BTreeMap<u64, (u64, Vec<u8>)>` に保持し、`Session` の `Debug` から到達できる。`Session::peer_auth_token_cache` から公開 API として取り出せる。同じ認可トークンの漏えいであるため、本 issue でまとめて直す。
- 影響: 利用者のログ出力で認可トークンが漏れる。ライブラリ内には現時点で `SetupOptions` を `{:?}` で出力する箇所はないが、公開型であるため利用側の使い方に依存する。

## 設計方針

`AuthorizationToken` に独自の `core::fmt::Debug` を実装する。表示形式は既存の前例 (`MsfFragmentParameter` / `MsfUri` / `tokio_moq::ServerUrl`) に揃える。

- `DELETE` / `USE_ALIAS` は `alias` だけを持つため、従来どおり `alias` を表示する。
- `REGISTER` / `USE_VALUE` は `token_type` を表示し、`token_value` は実値ではなく `token_value_bytes` としてバイト長を表示する。
- variant 名は従来の `Debug` と同じ表記 (`Delete` / `Register` / `UseAlias` / `UseValue`) にし、利用者が構造を追えるようにする。
- フィールドを追加したときに表示漏れをコンパイラが検出できるよう、`..` を使わずに全フィールドを分解してから `debug_struct` に渡す。
- `SetupOptionValue` / `SetupOption` / `SetupOptions` と `MessageParameterValue` / `MessageParameter` / `MessageParameters` は `derive(Debug)` のままでよい (内側の `AuthorizationToken` の独自 `Debug` を通るため、値は伏せられる)。
- `Register` と `UseValue` の `token_value` の doc に、`Debug` が実値を表示しないことを明記する。
- `src/session/auth_token_cache.rs` の `AuthTokenCache` にも独自の `Debug` を実装し、`entries` は `alias` / `token_type` / `token_value_bytes` を表示する。`total_size` と `max_size` は従来どおり表示する。`Session::peer_auth_token_cache` の doc にも秘匿方針を書く。

## 完了条件

- `AuthorizationToken::Register` と `AuthorizationToken::UseValue` の `{:?}` に、`token_value` のバイト列が文字列としても十進のバイト列としても含まれないこと。
- 伏せたフィールドはバイト長を表示し、`alias` と `token_type` は従来どおり表示されること。
- `SetupOptions` に `AuthorizationToken` を積んだ `{:?}` でも、`token_value` の実値が含まれないこと。
- `AuthorizationToken::Delete` と `AuthorizationToken::UseAlias` の `{:?}` が従来どおり `alias` を表示すること。
- 上記を `tests/test_message_parameter.rs` (`AuthorizationToken` の `Debug`) と `tests/test_parameter.rs` (`SetupOptions` の `Debug`)、`tests/test_session/auth_token.rs` (`AuthTokenCache` の `Debug`) に追加するテストで固定すること。
- テストの実値の非含有を検査するアサートの失敗メッセージには、Debug 出力をそのまま含めないこと (漏えいした実値を CI のログに書かないため)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
