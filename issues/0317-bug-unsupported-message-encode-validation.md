# ControlMessage::Unsupported の encode で type_id と request_id を検証する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-unsupported-message-encode-validation
- Polished: {YYYY-MM-DD}

## 目的

`ControlMessage::Unsupported` は公開 enum の variant で、フィールドを自由に構築できる。encode は `type_id` と `body` を無検証で書き出し、`request_id` を参照しないため、decode で別メッセージになる wire や、対応していない型の wire を生成できる。公開 API の契約として roundtrip を保証する。

## 現状

- `src/message.rs` の `ControlMessage::encode_message_body` の `Unsupported` 分岐は `payload.extend_from_slice(body)` と `*type_id` だけを使う。
- `Unsupported { type_id: 0x03, request_id: None, body: vec![] }` のような既知型・不整合な組み合わせを encode できる。
- `request_id: Some(5)` と body 先頭の varint が一致しない値も encode でき、decode は別の request_id を返す。
- `tests/test_message.rs` は body 先頭と request_id を一致させた入力だけを固定している。

## 設計方針

- encode 時に `type_id` が `UNSUPPORTED_MESSAGE_TYPES` に含まれること、`request_id` の有無が型と一致すること、body 先頭の varint が `request_id` と一致することを検証する。
- あるいはフィールドを private にして構築時検証に寄せる (破壊的変更として扱う)。
- 不正な組み合わせの encode テストを追加する。

## 完了条件

- 不正な `Unsupported` の encode が `ProtocolViolation` または構築時エラーになること。
- 正しい `Unsupported` の roundtrip が維持されること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
