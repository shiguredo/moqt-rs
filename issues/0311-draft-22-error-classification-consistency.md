# MessageError の分類と表示を一貫させる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-error-classification-consistency
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §10.7 / §12.1 は入れ子の IMMUTABLE_PROPERTIES を Malformed Track と定め、§8.3 (Key-Value-Pair Structure) は既知型の長さ不整合を KEY_VALUE_FORMATTING_ERROR と定める。現状の `MessageError` は同じ条件で variant が揃わず、`Display` も二重接頭辞になる。エラー型で購読 cancel とセッション終了を分岐する利用者が正しく判断できない。

## 現状

- `src/object_properties.rs` の `decode_kv_pairs` は入れ子 IMMUTABLE_PROPERTIES を `ProtocolViolation` で返すが、同じファイルの Prior Gap 系は `MalformedTrack` で返す。
- `src/track_properties.rs` の `decode` も入れ子 IMMUTABLE_PROPERTIES を `ProtocolViolation` で返す。
- `src/error.rs` の `MessageError::MalformedTrack` の `Display` が "malformed track: " を前置する一方、生成側も "malformed track: ..." で始まるため二重接頭辞になる。
- `src/track_properties.rs` と `src/object_properties.rs` の Properties 値長 65535 超は `MessageError::PayloadTooLong` を返すが、`src/message_parameter.rs` の同条件は `ProtocolViolation` である。0246 / 0238 とは対象が異なる (0238 は Properties を対象外と明記)。
- 影響: 公開 `ObjectProperties::decode` / `TrackProperties::decode` の分類が §12.1 の Must-cancel と一致せず、`Display` の文言も不正確になる。

## 設計方針

- Object 側の入れ子 IMMUTABLE_PROPERTIES は `MalformedTrack` に統一する。Track 側は仕様の malformed 条件が Object に限られるため、扱いを決めて doc に明記する。
- `Display` の二重接頭辞を解消し、生成側のメッセージを接頭辞なしに統一する。
- Properties の値長超過のエラー variant を 1 つに定め、encode / decode / モジュール間で揃える。
- 分類を検証するテストを追加・更新する。

## 完了条件

- 入れ子 IMMUTABLE_PROPERTIES の分類が仕様と一致し、テストで固定されていること。
- `MessageError` の `Display` に二重接頭辞が無いこと。
- Properties の値長超過のエラー variant が統一されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
