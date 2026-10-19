# SETUP Options の重複 Option Type ポリシーをテストで固定する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/test-setup-options-duplicate-policy
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §9.1 (SETUP) は「Senders MUST NOT repeat the same Option Type in a message unless the option definition explicitly allows multiple instances.」とし、受信側に「Receivers MUST allow duplicates of unknown Setup Options.」を課す。`SetupOptions` の encode と decode はこの 3
分岐を実装しているが、テストが無く、規約違反を検出できない。

## 現状

- `src/parameter.rs` の `SetupOptions::encode` は AUTHORIZATION_TOKEN 以外の同一 Option Type の重複を `InvalidParameter` にする。
- `src/parameter.rs` の `SetupOptions::decode` は既知型の重複を `ProtocolViolation` にし、未知型の重複は受理する。
- `tests/test_parameter.rs` と `pbt/tests/prop_parameter.rs` に重複を扱うテストが無い。PBT の `sample_setup_options_with` は型を dedup するため、この分岐を通らない。
- 影響: §9.1 の MUST 群が未固定で、将来の変更で規約違反を検出できない。

## 設計方針

- (a) 既知型 0x01 の重複 encode が `InvalidParameter`、(b) 既知型 0x05 の重複 decode が `ProtocolViolation`、(c) 未知型 0x09 の重複 decode が受理、(d) AUTHORIZATION_TOKEN の重複が許可、の 4 ケースをテストに追加する。
- even 型に Bytes、odd 型に VarInt を入れた encode の `InvalidParameter` も固定する。

## 完了条件

- 上記のテストが `tests/test_parameter.rs` にあり、すべて通ること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
