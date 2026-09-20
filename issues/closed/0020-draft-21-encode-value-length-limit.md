# KVP 値のエンコードに 2^16-1 バイト上限チェックを追加する

- Created: 2026-09-10
- Completed: 2026-09-12
- Branch: feature/fix-encode-value-length-limit
- Polished: 2026-09-10

## 目的

draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure) の値長上限を encode 側でも守り、自分が decode できない KVP を公開 API が生成しないようにする。

## 現状

decode 側は値長 `> 65535` を `ProtocolViolation` で拒否しているが、encode 側に次の抜けがある。

- `src/message_parameter.rs` の `encode_value` の `AuthorizationToken` 分岐は `token.encode_to_bytes()` の長さを検証しない。`LengthPrefixed` と `FillParameters` は検証している。
- `src/parameter.rs` の `SetupOptions::encode` は `SetupOptionValue::Bytes` と `AuthorizationToken` の長さを検証しない。decode は検証している。

`ControlMessage::encode` が payload 全体の 65535 を検査するため実運用では最終的に `PayloadTooLong` になることが多いが、公開 API の `MessageParameters::encode` / `SetupOptions::encode` 単体で不正な KVP を生成できる。

根拠 (draft-ietf-moq-transport-21 §8.3):

> "The maximum length of a value is 2^16-1 bytes. If an endpoint receives a length larger than the maximum, it MUST close the session with a PROTOCOL_VIOLATION."

## 設計方針

`LengthPrefixed` と同様に、各分岐で `bytes.len() > 65535` を `MessageError::ProtocolViolation` として拒否する。encode と decode の対称性を保つ。

## 完了条件

- `AuthorizationToken` / Setup `Bytes` / Setup `AuthorizationToken` の encode が 65536 バイト以上を拒否すること
- 65535 / 65536 の境界テストが `tests/test_message_parameter.rs` / `tests/test_parameter.rs` に追加されていること

## 解決方法

値長上限 2^16-1 バイトの encode 側検証を追加し、公開 API が自分で decode できない KVP を生成しないようにした。

- `src/message_parameter.rs`: `encode_value` の `AuthorizationToken` 分岐で `encode_to_bytes()` の長さを検証し、65535 バイト超を `ProtocolViolation` で拒否するようにした。`MessageParameters::encode` の `# Errors` に値長上限などの条件を追記した。
- `src/parameter.rs`: `SetupOptions::encode` の `Bytes` と `AuthorizationToken` 分岐で値長を検証し、65535 バイト超を `ProtocolViolation` で拒否するようにした。`# Errors` に条件を追記した。
- `tests/test_message_parameter.rs`: AuthorizationToken の 65535 / 65536 境界テストを追加し、既存の LengthPrefixed 65536 テストを値長検証だけを固定できる型 (`PARAM_SUBGROUP_FILTER`) に修正した。
- `tests/test_parameter.rs`: Setup Bytes / Setup AuthorizationToken の 65535 / 65536 境界テストと、Setup Option の decode で 65536 を拒否するテストを追加した。
- `CHANGES.md` の `## develop` に `[FIX]` エントリを追加した。
