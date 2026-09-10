# Authorization Token のエラー経路テストを追加する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
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
