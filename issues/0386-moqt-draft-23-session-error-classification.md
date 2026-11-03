# セッション終了コードを draft 23 の具体的な分類へ移行する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-session-error-classification
- Polished: {YYYY-MM-DD}

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §8.3、§8.7、§8.9、§9、§11、§12.2、§16.12.1](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/error.rs` の定数と `normalize_session_termination_code` は draft 22 のコード集合である。
`src/session/`、各 codec、`crates/tokio-moq/src/error.rs` の `session_error_code` とそれを使う `moqt_client.rs` の handshake 終了処理は、多くの異なる違反を PROTOCOL_VIOLATION にまとめている。
KEY_VALUE_FORMATTING_ERROR の旧名も公開されている。

## 設計方針

INVALID_STREAM_HEADER / MALFORMED_OBJECT / INVALID_OBJECT_STATUS / TRUNCATED_OBJECT、MALFORMED_MESSAGE / INVALID_FIELD / INVALID_TRACK_NAMESPACE / UNEXPECTED_MESSAGE / ROLE_VIOLATION / INVALID_PARAMETER / CLOSED_CONTROL_STREAM、DELTA_ENCODING_OVERFLOW / FIELD_LENGTH_EXCEEDED を定義し、原因を失わず各受信経路から送出する。
`0x06` を MALFORMED_KEY_VALUE に改称する。
認証構造の不正は MALFORMED_AUTH_TOKEN とし、delta overflow と長さ超過が他の分類に優先する規則を実装する。
汎用 PROTOCOL_VIOLATION が明示される条件は保つ。

## 完了条件

- 定義済みコードが正規化で INTERNAL_ERROR へ落ちず、未知コードの扱いを保つこと。
- データ・制御・共有構造の各違反について wire 入力と直接 Session API 入力を検証すること。
- control stream の閉鎖、重複応答、wrong role、パラメータ違反、宣言長超過・delta overflow のコードを確認すること。
- 既存の 0311 は入れ子 Property などの分類整合性を扱う別課題として参照し、draft 23 の追加コードと対応表を本 issue で扱うこと。

## 依存・関連

- 関連: [0311](0311-draft-22-error-classification-consistency.md)
