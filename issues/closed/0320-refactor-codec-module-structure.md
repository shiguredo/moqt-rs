# codec 層のモジュール責務を整理する

- Created: 2026-10-19
- Completed: 2026-11-22
- Branch: feature/refactor-codec-module-structure
- Polished: 2026-11-20

## 目的

`src/parameter.rs` が Setup Options の codec でありながら RFC 3986 の URI 検証 (userinfo / IPv4 / IPv6 / IPvFuture / path / query) を約 300 行持ち、`src/message_parameter.rs` が AuthorizationToken / LocationFilter / MessageParameters / RangeFilter の 4 系統を 1 ファイルに持つ。`src/decoder.rs` の
`MessageDecoder` は制御メッセージの外側フレームを `ControlMessage::decode` と二重に解析する。責務ごとに分割して可読性と変更容易性を上げる。

## 現状

- `src/parameter.rs` の URI 検証関数群は Setup Options の符号化とは独立した関心事であり、`SetupOptions` の本体と同居している。
- `src/message_parameter.rs` は約 2,000 行で、AuthorizationToken / LocationFilter / MessageParameters / RangeFilter の 4 系統の型・実装・共有ヘルパが 1 ファイルに混在している。テストは `tests/test_message_parameter.rs` にテーマ別モジュールとして分かれており、本 issue の変更対象ではない。
- `src/decoder.rs` の `try_decode_message` は Type (vi64) + Length (vi64) を自前で読み、`ControlMessage::decode` が同じフレームを再度読む。
- 影響: 仕様変更時の修正範囲が読みにくく、フレーム形式の変更で片方だけ直す事故が起きやすい。

## 設計方針

- URI 検証を crate 内部モジュール `src/setup_uri.rs` へ移す。`lib.rs` では `pub(crate) mod setup_uri;` とし、`pub mod` にしない。`SetupUriValidationError` は `pub(crate)` のまま `setup_uri.rs` に移し、`parameter.rs` から呼ぶ `validate_path` / `validate_authority` は `pub(crate)` にする。`session/core.rs` の import を追随させる。
- `src/parameter.rs` の `#[cfg(test)] mod tests` のうち `validate_ipv6addr` / `validate_ip_literal` を直接テストするものは `setup_uri.rs` へ移し、`SetupOptions::validate_uri_format` 経由のテストは `parameter.rs` に残す。
- `message_parameter` を `message_parameter.rs` + `message_parameter/{authorization_token,location_filter,range_filter}.rs` に分割する。`mod.rs` は使わない。
  - 公開型 (`AuthorizationToken` / `LocationFilter` / `LocationFilterContext` / `LocationFilterUpdate` / `RangeFilterSet` / `MessageParameter` / `MessageParameterValue` / `MessageParameters`) の定義は `message_parameter.rs` に残す。
  - サブモジュールには `AuthorizationToken` / `LocationFilter` / `RangeFilterSet` の `impl` と private ヘルパだけを置き、`message_parameter.rs` から `mod authorization_token;` などで宣言する。`MessageParameters` の `impl` は `message_parameter.rs` に残す。`pub use` による re-export は追加しない。これにより公開パスを変えずに公開 API を維持する。
  - 共有の定数・ヘルパ (`FILL_PARAMETERS_ALLOWED_PARAMS` / `validate_auth_token_uniqueness` など) は `message_parameter.rs` に残す。サブモジュール内のヘルパは `pub(super)` に留め、クレート外へ公開しない。
  - 親モジュールから呼ぶ `LocationFilter::decode_partial` / `LocationFilter::validate` は `pub(super)` に上げ、クレート外へ公開しない。
  - 移動するヘルパを参照するコメント (`pbt/src/common.rs` の `is_range_filter_type` の同期先) は新しいパスへ更新する。
- 制御メッセージのフレームヘッダを解析する `pub(crate)` ヘルパを `message` 側に置き、`MessageDecoder` と `ControlMessage::decode` で共用する。ヘルパは Type、payload length、header length を返し、ヘッダ未完成は `Ok(None)`、varint 不正は `Err` とする。
- payload の完成判定は呼び出し側が payload length とバッファ長から行い、`ControlMessage::decode` はヘッダ未完成または payload 未完成を `UnexpectedEof` に写す。
- `MessageDecoder` は宣言長が `max_control_message_size` を超える場合と、宣言長が `usize::MAX` を超える場合に本文を待たずに `Err` を返す現在の挙動と、「外側フレームが揃った後の body 不正は `Err`」という現在の挙動を維持する。
- 公開 API と挙動は変えない。

## 完了条件

- 上記の分割後も `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が通ること。
- `cargo build -p shiguredo_moqt --target thumbv7em-none-eabihf` が通ること。
- 公開 API のパス・型・シグネチャが変わっていないこと。`pub use` による re-export を追加していないこと。

## 解決方法

- `src/parameter.rs` の RFC 3986 URI 検証を `src/setup_uri.rs` へ移した。
  - `lib.rs` は `pub(crate) mod setup_uri;` とし、公開モジュールにしていない。
  - `SetupUriValidationError` は `pub(crate)` のまま移し、`validate_path` / `validate_authority` を
    `pub(crate)` にした。`session/core.rs` と `src/msf/uri.rs` の import を追随させた。
  - `validate_ipv6addr` / `validate_ip_literal` の直接テストは `setup_uri.rs` へ移し、
    `SetupOptions::validate_uri_format` 経由のテストは `parameter.rs` に残した。
- `src/message_parameter.rs` を `message_parameter.rs` + `message_parameter/{authorization_token,
  location_filter,range_filter}.rs` に分割した。
  - 公開型の定義と `MessageParameters` の `impl`、共有定数・ヘルパは親に残した。
  - サブモジュールには各型の `impl` と private ヘルパだけを置き、`pub use` は追加していない。
  - `LocationFilter::decode_partial` / `validate` は `pub(super)` にした。
  - `pbt/src/common.rs` の同期先コメントを `src/message_parameter/range_filter.rs` に更新した。
- `src/message.rs` に `decode_control_message_header` と `ControlMessageHeader` を追加し、
  `MessageDecoder::try_decode_message` と `ControlMessage::decode` で共用した。
  - ヘッダ未完成は `Ok(None)`、varint 不正は `Err`、payload 未完成は呼び出し側が判定する。
  - `ControlMessage::decode` はヘッダ / payload 未完成を `UnexpectedEof` に写す。
  - `MessageDecoder` の宣言長超過と、外側フレーム完備後の body 不正の挙動を維持した。
- 公開型・定数のパスは変えていない。`pub use` の追加は 0 件である。

`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、
`cargo fmt --all -- --check`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt`、
`cargo build -p shiguredo_moqt --target thumbv7em-none-eabihf`、`prek run --files` が成功した。
