# Track Properties の KVP 宣言長超過を KEY_VALUE_FORMATTING_ERROR に分類する

- Created: 2026-10-25
- Completed: 2026-10-25
- Branch: develop
- Polished: 2026-10-25

## 目的

draft-ietf-moq-transport-22 §8.3 (Key-Value-Pair Structure) は、既知 Type の Value または Length/Value が型の定義と一致しない場合に
KEY_VALUE_FORMATTING_ERROR (0x6) でセッションを閉じることを MUST とする。Track Properties は KVP 列でありながら、奇数 Type の宣言長
(2^16-1 以下) が残りバッファを超える場合に `MessageError::UnexpectedEof` を返すため、examples の写像で PROTOCOL_VIOLATION (0x3) に
なり MUST を満たさない。Message Parameters と SETUP Options は同じ条件を KEY_VALUE_FORMATTING_ERROR として扱うよう対応済みだが、
Track Properties は先送りされたままである。本 issue は decode 失敗の分類と、共有関数の変更に伴う encode 側の写像を扱い、
§10.7 の malformed Track 扱いへの変更は扱わない。

## 現状

- `src/track_properties.rs` の `decode_track_kv_pairs` は奇数 Type の Value で次の順に検証する。
  - `len > 65535` は `MessageError::ProtocolViolation` を返す。これは §8.3 の「2^16-1 超は PROTOCOL_VIOLATION」MUST と一致する。
  - 続く `varint::checked_len(len, buf[pos..].len())` は、宣言長が残りバイトを超えると `MessageError::UnexpectedEof` を返す。
- `decode_track_kv_pairs` は `TrackProperties::decode` のほか、`TrackProperties::encode` の `IMMUTABLE_PROPERTIES` (0x0B) 内側検証からも
  呼ばれ、エラーをそのまま返す。`TrackProperties::encode` の `# Errors` は重複と `PayloadTooLong` のみを記載している。
- `examples/tokio-moq/src/error.rs` の `session_error_code` は 3 分岐で、`KeyValueFormattingError` を `SESSION_KEY_VALUE_FORMATTING_ERROR`
  (0x6)、`MalformedAuthToken` を `SESSION_MALFORMED_AUTH_TOKEN` (0x16)、それ以外を `SESSION_PROTOCOL_VIOLATION` (0x3) に写す。
- `tests/test_track_properties.rs` は `unexpected_eof_in_bytes_value` と `inner_unparseable_rejected_on_decode` で `UnexpectedEof` を、
  `value_too_long` と `inner_value_too_long_rejected_on_decode` で `ProtocolViolation` を固定している。
- draft-ietf-moq-transport-22 §10.7 (Immutable Properties) は "A Key-Value-Pair cannot be parsed" を malformed Track の条件に挙げ、§12.1 は
  subscriber に購読 cancel を MUST とする。一方で §8.3 は既知 Type の直列化不一致にセッション終了を MUST とする。両者が重なる条件について
  仕様は優先順位を定めていない。
- 閉じた issue 0238 は Message Parameters / SETUP Options の宣言長超過を KEY_VALUE_FORMATTING_ERROR にした際、Track Properties を対象外として
  「未起票の別 issue」に先送りした。

## 設計方針

- 本 issue の範囲は decode 失敗のエラー分類の修正と、共有関数の変更に伴う `TrackProperties::encode` の写像とする。
  §10.7 の malformed Track への写像は、Track Properties の decode を Session 層へ
  移して Track / request を特定する設計変更が必要になるため対象外とし、別の設計課題として扱う。decode 層では §8.3 の
  KEY_VALUE_FORMATTING_ERROR を優先する (理由: decode 層は Track / request を特定できず購読 cancel を発行できないため、§8.3 が定める
  セッション終了の MUST を満たす経路を採る)。
- `decode_track_kv_pairs` の奇数 Type で、宣言長が 2^16-1 以下かつ残りバッファを超える場合は `MessageError::KeyValueFormattingError` を返す。
  2^16-1 超は従来どおり `ProtocolViolation`、Length フィールドの varint 自体が途中で切れる場合は `UnexpectedEof` のままとする。
  型の既知 / 未知は問わない (KVP の Length/Value フレーミングが成立しない点で同じ分類にする。既知 Type の不一致に対する §8.3 の MUST と
  同じエラーコードに揃える)。
- 入れ子の `IMMUTABLE_PROPERTIES` 内側の KVP も同じ関数で検証されるため、同じ分類にする。
- `TrackProperties::encode` はローカル入力の検証であるため、内側 KVP 検証で `KeyValueFormattingError` が返る場合は
  `MessageError::ProtocolViolation` に写す。`encode` の `# Errors` にこの条件を追記する。
- `TrackProperties::decode`・`decode_track_kv_pairs`・`TrackProperties::encode` の doc / `# Errors` を新しい分類に合わせて更新する。
- `tests/test_track_properties.rs` の `unexpected_eof_in_bytes_value` と `inner_unparseable_rejected_on_decode` の期待値を
  `KeyValueFormattingError` に更新する。`value_too_long` と `inner_value_too_long_rejected_on_decode` は `ProtocolViolation` のまま維持する。
- `tests/test_message.rs` に、Track Properties を含む制御メッセージ (SUBSCRIBE_OK / PUBLISH / REQUEST_OK / FETCH_OK のいずれか) の decode で
  宣言長超過の Track Property を含む場合に `KeyValueFormattingError` を返すテストを追加する。

## 完了条件

- 奇数 Type の Track Property で、2^16-1 以下の宣言長が残りバイトを超えるとき、`TrackProperties::decode` が
  `MessageError::KeyValueFormattingError` を返すこと。既知 Type の例と未知 Type の例の両方をテストで固定すること。
- 宣言長が 2^16-1 を超える場合は `MessageError::ProtocolViolation` のままであること。
- Length フィールドの varint 途中終端は `MessageError::UnexpectedEof` のままであること。
- `IMMUTABLE_PROPERTIES` 内側の KVP でも宣言長超過が `MessageError::KeyValueFormattingError` になること。
- `TrackProperties::encode` で `IMMUTABLE_PROPERTIES` 内側の KVP が宣言長超過のとき `MessageError::ProtocolViolation` を返し、
  `# Errors` にその条件が記載されていること。
- Track Properties を含む制御メッセージの decode で宣言長超過が `MessageError::KeyValueFormattingError` になり、
  examples の `session_error_code` で KEY_VALUE_FORMATTING_ERROR (0x6) に写ること (既存の写像テストと decode テストの両方で確認する)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `src/track_properties.rs` の `decode_track_kv_pairs` で、奇数 Type の宣言長が 2^16-1 以下かつ残りバイトを超える場合を
  `MessageError::KeyValueFormattingError` ("track property value length exceeds remaining buffer") に分類した。
  `varint::checked_len` + `map_err` で parameter.rs / message_parameter.rs と同じ実装に揃えた。2^16-1 超は
  `ProtocolViolation`、Length フィールドの varint 途中終端は `UnexpectedEof` のままである。
- draft §8.3 の MUST は受信側が Type を理解する場合が条件であること、未知 Type も §16.8 (Properties) の length による
  skip が成立しないため同じ分類に揃える実装方針であること、§10.7 (Immutable Properties) の malformed Track と重なる
  条件だが decode 層では Track / request を特定できず購読 cancel を発行できないため §8.3 のセッション終了を優先すること
  を doc とコメントに明記した。`MessageError::KeyValueFormattingError` の doc も実態に合わせて更新した。
- `TrackProperties::encode` は IMMUTABLE_PROPERTIES の内側 KVP 検証で `KeyValueFormattingError` が返る場合を
  `ProtocolViolation` に写し、内側 decode の `UnexpectedEof` (呼び出し側が渡した直列化の切り詰め) は透過する。
  `# Errors` を重複 / 型と値形式の不一致 / 既知 Type の値域違反 / トップレベルの `PayloadTooLong` /
  内側 KVP の `ProtocolViolation` / 内側 varint の `UnexpectedEof` に更新した。
- `tests/test_track_properties.rs` の既存テストを新しい分類に更新し、既知 Type (0x0B) と未知 Type (0x01)、
  Length varint 途中終端 (`UnexpectedEof`)、宣言長が残りバイトと一致する成功ケース、65535 (上限内) の KVF と
  65536 (上限超) の `ProtocolViolation`、正常 KVP の後の 2 個目で KVF、内側 KVP の decode / encode 写像、
  encode の内側 varint 途中終端透過、既知 Type の値域違反を固定するテストを追加した。
- `tests/test_message.rs` に、SUBSCRIBE_OK の Track Properties で宣言長超過が `KeyValueFormattingError` になる
  テストを追加した。
- `examples/tokio-moq/src/error.rs` に、同じ wire を decode して `session_error_code` が
  `SESSION_KEY_VALUE_FORMATTING_ERROR` (0x6) に写すことを確認する通しテストを追加した。

## 完了条件の検証

- 既知 Type / 未知 Type の宣言長超過が `KeyValueFormattingError`:
  `known_type_bytes_value_length_exceeds_buffer_is_key_value_formatting_error` /
  `unknown_type_bytes_value_length_exceeds_buffer_is_key_value_formatting_error`
- 2^16-1 超が `ProtocolViolation`: `value_too_long`
- Length varint 途中終端が `UnexpectedEof`: `length_varint_truncation_is_unexpected_eof`
- 内側 KVP の宣言長超過が `KeyValueFormattingError`: `inner_length_exceeding_buffer_is_key_value_formatting_error`
- `TrackProperties::encode` の内側 KVP が `ProtocolViolation`:
  `encode_maps_inner_length_exceeding_buffer_to_protocol_violation`
- message 層と examples の通し確認:
  `subscribe_ok_with_track_property_length_exceeding_payload_is_key_value_formatting_error` /
  `track_property_length_error_maps_to_key_value_formatting_error`
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` 成功
