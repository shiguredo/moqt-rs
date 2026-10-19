# parse_namespace のエラーを長さ超過とフィールド数超過で区別する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-name-parse-namespace-error
- Polished: 2026-10-20

## 目的

draft-ietf-moq-transport-22 §8.7 (Track Namespace Structure) は、4096 バイトを超える Track Namespace または Full Track Name の受信を PROTOCOL_VIOLATION とする。`parse_namespace` は §8.8 (Representing Namespace and Track Names) のシリアライズ表現をパースして `TrackNamespace` を返すため、`TrackNamespace::new` が型不変条件として課す 4096 バイト制約を継承する。
§8.8 のパース失敗の扱い自体は application-defined であり、ここで求めるのは protocol violation ではなく、利用者が原因を判別できるエラー種別である。

現状の `parse_namespace` は `TrackNamespace::new` のすべてのエラーを `NameParseError::InvalidNamespace` に写すため、デコード後の namespace の合計バイト長が 4096 バイトを超えても「namespace is invalid (max 32 fields)」と表示され、フィールド数超過と長さ超過を区別できない。

## 現状

- `src/name.rs` の `parse_namespace` は `TrackNamespace::new(fields).map_err(|_| NameParseError::InvalidNamespace)` とする。
- `NameParseError::InvalidNamespace` の doc と `Display` はフィールド数超過 (32 超) しか説明していない。
- `TrackNamespace::new` (`src/message/common.rs`) は、デコード後の namespace の合計バイト長が `MAX_TRACK_NAME_LENGTH` (4096) を超えると `MessageError::ProtocolViolation("track namespace exceeds 4096 bytes")` を返す。
- `parse_namespace` は空フィールドを `decode_field` の前に `NameParseError::EmptyNamespaceField` で拒否しているため、`TrackNamespace::new` が `parse_namespace` から返し得るエラーは、フィールド数超過と 4096 バイト超だけである。
- 影響: デコード後の合計が 4097 バイトの namespace を渡した利用者に、フィールド数超過を意味する誤った理由が返る。たとえば `parse_namespace(&"a".repeat(4097))` は `Err(NameParseError::InvalidNamespace)` になり、`Display` は "namespace is invalid (max 32 fields)" になる。

## 設計方針

- `src/name.rs` の `NameParseError` に `NamespaceTooLong` variant を追加する。doc は「namespace の合計バイト長が 4096 バイトを超える (draft-ietf-moq-transport-22 §8.7 (Track Namespace Structure))」とし、`parse_namespace` が返すことを明記する。
- `NameParseError` の `Display` に `NamespaceTooLong` を追加し、`FullNameTooLong` と同じく `MAX_TRACK_NAME_LENGTH` を使って "namespace is too long (max {MAX_TRACK_NAME_LENGTH} bytes)" と表示する。
- `parse_namespace` は `TrackNamespace::new` を呼ぶ前に、デコード済みの `fields` の合計バイト長 `fields.iter().map(Vec::len).sum::<usize>()` を `MAX_TRACK_NAME_LENGTH` と比較する。4096 を超える場合は `NameParseError::NamespaceTooLong` を返す。長さ超過を先に検査するため、フィールド数超過と長さ超過が同時に発生した場合は `NamespaceTooLong` を返す。
- 上記の事前検査により、`parse_namespace` の `TrackNamespace::new(fields).map_err(|_| NameParseError::InvalidNamespace)` に残るエラーはフィールド数超過だけになる。`InvalidNamespace` の doc / `Display` はフィールド数超過の意味のまま変更しない。
- `parse_namespace` の doc に、4096 バイト超で `NamespaceTooLong` を返すことを追記する。
- `parse_name` / `parse_name_with_percent_encoding` の挙動は変更しない。full track name の合計長超過は既存の `NameParseError::FullNameTooLong` のままとする。`NamespaceTooLong` は `parse_namespace` だけが返す。
- `src/name.rs` は既に `MAX_TRACK_NAME_LENGTH` を import しているため、追加の公開は不要である。

## 完了条件

- `tests/test_name.rs` の `namespace_only_parse_errors` に、デコード後の合計が 4097 バイトの namespace (`"a".repeat(4097)`) を `parse_namespace` に渡すと `Err(NameParseError::NamespaceTooLong)` になるテストを追加すること。`Display` が "namespace is too long (max 4096 bytes)" になることも同じテストで検証すること。
- 4096 バイトちょうどの namespace (`"a".repeat(4096)`) は `parse_namespace` で成功することも検証し、境界の off-by-one を固定すること。
- 既存テストの 33 フィールド (`vec!["a"; 33].join("-")`、合計 33 バイト) は長さ超過を伴わないため、引き続き `Err(NameParseError::InvalidNamespace)` になること。
- 空フィールド・パーセントエンコーディング・`parse_name` の `FullNameTooLong` の既存テストが変わらず通ること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
