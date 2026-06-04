# Authorization Token のエラー経路テストを追加する

- Created: 2026-09-10
- Completed: 2026-09-17
- Polished: 2026-09-17
- Branch: feature/test-auth-token-error-paths

## 目的

draft-ietf-moq-transport-21 §8.9 / §9.1.4 が要求する Auth Token のエラー経路をテストで固定し、回帰を防ぐ。

## 現状

`AuthorizationToken` の正常系 (`UseValue` の往復など) はテストされているが、次のエラー MUST 経路が未テストである。

- DELETE / USE_ALIAS の Token Alias 後に余剰バイトがある場合 → `KEY_VALUE_FORMATTING_ERROR` (`src/message_parameter.rs` の `AuthorizationToken::decode`)
- 未知の Alias Type → `KEY_VALUE_FORMATTING_ERROR`
- SETUP での DELETE / USE_ALIAS → `PROTOCOL_VIOLATION` (`validate_setup_scope`)
- (関連) `src/message_parameter.rs` / `src/parameter.rs` の encode 側値長上限

根拠:

- §8.9: "If the Token structure cannot be decoded, the receiver MUST close the Session with KEY_VALUE_FORMATTING_ERROR."
- §9.1.4: "If a server receives Alias Type DELETE (0x0) or USE_ALIAS (0x2) in a SETUP message, it MUST close the session with a PROTOCOL_VIOLATION."

## 設計方針

`MessageParameters::decode` / `SetupOptions::decode` に生バイト列を直接与える単体テストを追加する。既存の `LengthPrefixed` の 65536 拒否テストと同じ粒度にする。

## 完了条件

- 上記エラー経路が `tests/test_message_parameter.rs` / `tests/test_parameter.rs` で検証されていること
- 各エラーのワイヤ上の最終コードが仕様どおりであること

## 解決方法

Authorization Token のエラー経路を、生バイト列を直接 decode する単体テストで固定した。

`tests/test_message_parameter.rs` に `authorization_token_errors` モジュールを追加した。

- `delete_with_trailing_bytes_is_key_value_formatting_error`: DELETE (0x0) の Token Alias の後に
  余剰バイトがあると `KEY_VALUE_FORMATTING_ERROR` (§8.9 MUST)
- `use_alias_with_trailing_bytes_is_key_value_formatting_error`: USE_ALIAS (0x2) も同様
- `unknown_alias_type_is_key_value_formatting_error`: Alias Type 0x04 は未知であり
  `KEY_VALUE_FORMATTING_ERROR`
- `delete_without_alias_is_key_value_formatting_error`: Token Alias 欠落も
  `KEY_VALUE_FORMATTING_ERROR`
- `encode_rejects_token_value_over_65535_bytes` /
  `encode_accepts_token_value_at_65535_bytes`: REGISTER の Token Value の値長上限
  (§8.3 の 2^16-1 バイト) の境界。上限ちょうどは通り、1 バイト超は
  `ProtocolViolation` になる
- `encode_rejects_length_prefixed_value_over_65535_bytes`: LengthPrefixed にも同じ上限が
  適用される

`tests/test_parameter.rs` に `setup_authorization_token_scope` モジュールを追加した。

- `delete_is_protocol_violation` / `use_alias_is_protocol_violation`: SETUP で DELETE /
  USE_ALIAS を受信すると `PROTOCOL_VIOLATION` (§9.1.4 MUST)
- `delete_encode_is_protocol_violation` / `use_alias_encode_is_protocol_violation`:
  encode 側でも同じ検証が働く
- `register_and_use_value_are_accepted`: REGISTER / USE_VALUE は SETUP で許可される
- `delete_with_trailing_bytes_is_key_value_formatting_error`: Token 構造のデコード失敗は
  スコープ検証より先に `KEY_VALUE_FORMATTING_ERROR` になる

検証:

- `cargo test --test test_message_parameter` が 89 件、`cargo test --test test_parameter` が
  29 件通る
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通る
