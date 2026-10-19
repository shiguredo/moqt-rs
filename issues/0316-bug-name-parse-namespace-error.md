# parse_namespace のエラーを長さ超過とフィールド不正で区別する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-name-parse-namespace-error
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §8.7 (Track Namespace Structure) は 4096 バイトを超える Track Namespace の受信を PROTOCOL_VIOLATION とする。`parse_namespace` は `TrackNamespace::new` のすべてのエラーを `NameParseError::InvalidNamespace` に写すため、4096 バイト超でも「namespace is invalid (max 32 fields)」と表示され、原因が分からない。

## 現状

- `src/name.rs` の `parse_namespace` は `TrackNamespace::new(fields).map_err(|_| NameParseError::InvalidNamespace)` とする。
- `NameParseError::InvalidNamespace` の doc と `Display` はフィールド数超過しか説明していない。
- `TrackNamespace::new` は 4096 バイト超で `ProtocolViolation` を返す。
- 影響: 4097 バイトの namespace を渡した利用者に誤った理由が返る。

## 設計方針

- 長さ超過用の variant (例 `NamespaceTooLong`) を追加するか、`InvalidNamespace` の doc / `Display` を「32 フィールド超または 4096 バイト超」に修正する。
- 4097 バイトの `parse_namespace` テストを追加する。

## 完了条件

- 4096 バイト超の namespace で、長さ超過と分かるエラーが返ること。
- 既存のフィールド数超過・空フィールド・パーセントエンコーディングのテストが変わらず通ること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
