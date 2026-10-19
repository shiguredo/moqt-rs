# 公開 API の doc を整備する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/doc-public-api-coverage
- Polished: {YYYY-MM-DD}

## 目的

shiguredo-rust は公開 API (型・関数・フィールド・variant) に `///` を必ず書くことを求める。現状は公開項目に doc が無い箇所、doc が別の項目に付いている箇所、doc が実装より古い箇所がある。利用者が rustdoc だけで契約を理解できる状態にする。

## 現状

- `src/object_properties.rs` の `find_varint` に `///` が無く、直前の `count_instances` の doc に「偶数型 Object Property から varint 値を取得する」という find_varint 向けの説明が混在している。
- `examples/tokio-moq/src/moqt_client.rs` の `ControlStream::new` と `SubscribeResult` の `request_id` / `track_alias` に doc が無い。
- `src/message_parameter.rs` の `LocationFilterContext::Fetch` の doc が「現状の呼び出しは subscription のみ」と書くが、fill 経路などから呼ばれている。
- `src/session/core.rs` のモジュール doc が存在しない `namespace.rs` を兄弟モジュールとして挙げている。
- `src/playout/buffer.rs` と `src/playout/timeline.rs` の同名 `playout_delay_us` が異なる意味を持ち、doc で区別されていない。

## 設計方針

- 公開 API の doc 欠落を補い、別項目に付いている doc を正しい位置へ移す。
- 実装と食い違う doc を修正する。
- 同名で意味が違うメソッドは、doc で区別するか名前を変える (名前変更は破壊的変更として別判断)。

## 完了条件

- `cargo doc` の公開 API に doc が揃っていること。
- 実装と食い違う doc が残っていないこと。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` と `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
