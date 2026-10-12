# KVP の宣言長超過を KEY_VALUE_FORMATTING_ERROR で閉じる

- Created: 2026-10-12
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-kvp-length-prefix-error-code
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §8.3 (Key-Value-Pair Structure) は "If a receiver understands a
Type, and the following Value or Length/Value does not match the serialization defined by that
Type, the receiver MUST close the session with error code KEY_VALUE_FORMATTING_ERROR." と定める。
型を理解したうえで長さプレフィックスが値と整合しない場合に、PROTOCOL_VIOLATION で閉じており
MUST を満たしていない。

## 現状

- `MessageParameters::decode` の長さプレフィックス付きパラメータ (AUTHORIZATION_TOKEN /
  FILL_PARAMETERS / SUBGROUP_FILTER など) は `decode_value` から `varint::checked_len` を呼び、
  宣言長が残りバッファを超える場合に `MessageError::UnexpectedEof` を返す。
- I/O 層 (library の data plane と `examples` の `session_error_code`) は `UnexpectedEof` を
  PROTOCOL_VIOLATION (0x3) に写すため、KEY_VALUE_FORMATTING_ERROR (0x6) にならない。
  確認済みの例: `count=1`, `AUTHORIZATION_TOKEN`, 宣言長 10, 残り 3 バイトのワイヤは
  `UnexpectedEof` になる。
- 宣言長が 2^16-1 を超える場合は `ProtocolViolation` で、§8.3 が MUST とする
  PROTOCOL_VIOLATION と一致している (こちらは正しい)。
- メッセージ全体がヘッダ途中で切れている場合も `UnexpectedEof` になるため、両者を
  区別できていない。

## 設計方針

- 対象は §8.3 (Figure 3) の KVP が持つ Length (型が奇数のときの値長) に限る。Message
  Parameters (`MessageParameters::decode`) と SETUP Options (`SetupOptions::decode`) は同じ
  `varint::checked_len` を通り、どちらも §8.3 の KVP として `UnexpectedEof` を返している。
- Properties 系は対象外とする。外側の Properties Length は §8.4 / §11.1.2 の framing であり
  §8.3 の Length ではない。また `ObjectProperties::decode_exact` は framing の失敗を
  `ProtocolViolation` に畳む設計で、session の data plane もそれを
  PROTOCOL_VIOLATION (0x3) として扱う (既存テストで固定)。この畳み込みを変えるかは別 issue と
  する。
- 値デコード中に判明した長さの不整合を `MessageError::KeyValueFormattingError` として返す。
  `varint::checked_len` は他の用途と共有しているため、呼び出し側で文脈に応じて写す
  (関数自体の意味は変えない)。
- メッセージのヘッダや KVP の型が読めない段階の終端は従来どおり `UnexpectedEof` のままとし、
  値の長さ不整合と区別できるようにする。
- `MessageParameters::decode` の `# Errors` にある「バッファが途中で切れている場合
  (LOCATION_FILTER の必須フィールド欠落を含む): `UnexpectedEof`」の記述を実態に合わせて
  更新する。`SetupOptions::decode` には `# Errors` 節が
  無いため、今回の挙動を書いた節を新設する。
- codec 層の変更なので、セッション / example の写像 (`session_error_code` など) は
  変更しない。

## 完了条件

- 長さプレフィックスの宣言長が残りバッファを超えるワイヤで、セッションが
  KEY_VALUE_FORMATTING_ERROR (0x6) で閉じること。Message Parameters は wire の
  request メッセージで、SETUP Options は確立後に届く SETUP で確認する (peer の初回 SETUP は
  0240 が扱うため、0238 の確認対象に含めない)。
- 宣言長が 2^16-1 を超える場合は PROTOCOL_VIOLATION (0x3) のままであること。
- `MessageParameters::decode` の `# Errors` の記述が実装と一致し、`SetupOptions::decode` に
  `# Errors` 節があること。
- 上記がテストで固定されていること。
