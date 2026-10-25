# Track Properties の KVP 宣言長超過を KEY_VALUE_FORMATTING_ERROR に分類する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §8.3 (Key-Value-Pair Structure) は、既知 Type の Value または Length/Value が型の定義と一致しない場合に
KEY_VALUE_FORMATTING_ERROR (0x6) でセッションを閉じることを MUST とする。Track Properties は KVP 列でありながら、奇数 Type の宣言長が残りバッファを超える場合に
`MessageError::UnexpectedEof` を返すため、examples の写像で PROTOCOL_VIOLATION (0x3) になり MUST を満たさない。Message Parameters と SETUP Options
は同じ条件を KEY_VALUE_FORMATTING_ERROR として扱うよう対応済みだが、Track Properties は先送りされたままである。

## 現状

- `src/track_properties.rs` の `decode_track_kv_pairs` は奇数 Type の Value で `varint::checked_len(len, buf[pos..].len())`
  を使い、宣言長が残りバイトを超えると `MessageError::UnexpectedEof` を返す。値長が 2^16-1 を超える場合は `MessageError::ProtocolViolation` を返しており、これは §8.3 の
  MUST と一致する。
- `decode_track_kv_pairs` は `TrackProperties::decode` のほか、`IMMUTABLE_PROPERTIES` (0x0B) の内側検証からも呼ばれる。
- `examples/tokio-moq/src/error.rs` の `session_error_code` は `MessageError::KeyValueFormattingError` のみを
  `SESSION_KEY_VALUE_FORMATTING_ERROR` に写し、それ以外は `SESSION_PROTOCOL_VIOLATION` に写す。
- `tests/test_track_properties.rs` の `unexpected_eof_in_bytes_value` が現行の `UnexpectedEof` を固定している。
- 閉じた issue 0238 は Message Parameters / SETUP Options の宣言長超過を KEY_VALUE_FORMATTING_ERROR にした際、Track Properties を対象外として「未起票の別
  issue」に先送りした。

## 設計方針

- `decode_track_kv_pairs` の奇数 Type で、宣言長が残りバッファを超える場合は `MessageError::KeyValueFormattingError` を返す。Length フィールドの varint
  自体が途中で切れる場合は従来どおり `UnexpectedEof` のままとする (Message Parameters / SETUP Options と同じ切り分け)。
- 入れ子の `IMMUTABLE_PROPERTIES` 内側の KVP も同じ関数で検証されるため、同じ分類にする。
- `TrackProperties::decode` と `decode_track_kv_pairs` の doc / `# Errors` を新しい分類に合わせて更新する。
- `tests/test_track_properties.rs` の期待値を更新し、Length varint の途中終端が `UnexpectedEof` のままであることと、`IMMUTABLE_PROPERTIES` 内側の宣言長超過も
  `KeyValueFormattingError` になることを追加で固定する。

## 完了条件

- 奇数 Type の Track Property で宣言長が残りバイトを超えるとき、`TrackProperties::decode` が `MessageError::KeyValueFormattingError` を返すこと。
- 同じ入力の `IMMUTABLE_PROPERTIES` 内側の KVP でも `MessageError::KeyValueFormattingError` を返すこと。
- Length フィールドの varint 途中終端は `MessageError::UnexpectedEof` のままであること。
- Track Properties を含む制御メッセージの decode 失敗が、examples の `session_error_code` で KEY_VALUE_FORMATTING_ERROR (0x6) に写ること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
