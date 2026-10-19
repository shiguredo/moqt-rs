# codec 層のモジュール責務を整理する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-codec-module-structure
- Polished: {YYYY-MM-DD}

## 目的

`src/parameter.rs` が Setup Options の codec でありながら RFC 3986 の URI 検証 (userinfo / IPv4 / IPv6 / IPvFuture / path / query) を約 300 行持ち、`src/message_parameter.rs` が AuthorizationToken / LocationFilter / MessageParameters / RangeFilter の 4 系統を 1 ファイルに持つ。`src/decoder.rs` の
`MessageDecoder` は制御メッセージの外側フレームを `ControlMessage::decode` と二重に解析する。責務ごとに分割して可読性と変更容易性を上げる。

## 現状

- `src/parameter.rs` の URI 検証関数群は Setup Options とは独立した関心事である。
- `src/message_parameter.rs` は約 2,000 行で、変更時に無関係なテストを同時に触る必要がある。
- `src/decoder.rs` の `try_decode_message` は Type + 16 bit Length を自前で読み、`ControlMessage::decode` が同じフレームを再度読む。
- 影響: 仕様変更時の修正範囲が読みにくく、フレーム形式の変更で片方だけ直す事故が起きやすい。

## 設計方針

- URI 検証を `setup_uri.rs` などの別モジュールへ移す。
- `message_parameter` を `message_parameter.rs` + `message_parameter/{authorization_token,location_filter,range_filter}.rs` に分割する。`mod.rs` は使わない。
- 制御メッセージのフレーム長を返す `pub(crate)` ヘルパを `message` 側に置き、`MessageDecoder` と `ControlMessage::decode` で共用する。
- 公開 API と挙動は変えない。

## 完了条件

- 上記の分割後も `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- `rustdoc` の警告が無いこと。
- 公開 API の re-export を追加していないこと。
