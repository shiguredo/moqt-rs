# FILL_PARAMETERS 内側の Number of Parameters の扱いを確定する

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-fill-parameters-inner-parameter-count
- Polished: 2026-09-21

## 目的

draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter) は FILL_PARAMETERS の値を「別メッセージの Parameters として符号化する」とだけ規定し、内側に `Number of Parameters` を含むかが一意に読めない。
読み方は 2 通りあり、両者は排他的なワイヤ形式になる (値 `0x00` が「内側 0 個」を意味するか、空バイト列が「内側 0 個」を意味するか)。
相互運用では符号化の解釈が割れるとセッション断 (KEY_VALUE_FORMATTING_ERROR) につながる。符号化を 1 つに確定し、encode と decode をその規則に揃える。

## 現状

- `src/message_parameter.rs` の `MessageParameters::encode` は `varint::encode(sorted.len(), buf)` で `Number of Parameters` を書く。`encode_value` の `MessageParameterValue::FillParameters` 腕は `inner.encode(&mut inner_buf)` の結果を長さ付きで書くため、FILL_PARAMETERS の値は必ず count 付きになる
- `src/message_parameter.rs` の `decode_fill_parameters` は `bytes.is_empty()` のとき内側なしとして `MessageParameters::new()` を返し、非空なら `MessageParameters::decode_inner` で count 付きとして読む。書き手は count 付きしか作らず、読み手は count 付きと空バイト列の両方を受ける。規則が一致していない
- `tests/test_message_parameter.rs` の `mod fill_parameters` にある `empty_fill_accepted` は `MessageParameters::new()` を encode / decode するテストであり、実際に固定しているのは count 0 の値 (`0x00` の 1 バイト) である。空バイト列の値は通していない。テスト名と固定している入力がずれている
- 空バイト列の FILL_PARAMETERS を値として使っている既存テストが 3 件ある (`duplicate_fill_rejected` / `nested_fill_on_decode_is_protocol_violation` / `nested_fill_rejected_before_recursion`)。いずれも空バイト列の受理そのものを固定する目的ではないが、規則変更の影響を受けるため更新対象に含める
- 内側 0 個の値は count 0 の `0x00` 1 バイトであり、その外側の Length varint は 1 (`0x01`) になる。内側 1 個 (FILL_TIMEOUT = 100) の値は `0x01 0x0A 0x64` の 3 バイトになる

## 設計方針

- 規範を確認する。§9.20.16 の該当文は次のとおり。

  > Its value is a sequence of Parameters that apply to the fill fetch stream (see Section 3.4), encoded as if they were Parameters for a separate message (see Section 16.7).

- §9.20.16 が参照する §16.7 は IANA の登録表 (Message Parameters) であり、符号化は規定していない。符号化の規範は §9.20 と各メッセージ形式にある
- 「別メッセージの Parameters として符号化する」の Parameters は、各メッセージ形式が持つ `Number of Parameters (vi64), Parameters (..) ...` を指す。§9.20 (Control Message Parameters) にも次の記述がある。

  > Because unknown parameters cannot be skipped, the block is bounded by a parameter count rather than a length.

- §8.3 (Key-Value-Pair Structure) は、既知 Type の値が定義された serialization と一致しない場合を MUST で拒否するとしている。

  > If a receiver understands a Type, and the following Value or Length/Value does not match the serialization defined by that Type, the receiver MUST close the session with error code KEY_VALUE_FORMATTING_ERROR.

- WG の経緯も同じ結論を支持する。moq-wg/moq-transport の [PR #1868](https://github.com/moq-wg/moq-transport/pull/1868) (title: "FILL_PARAMETERS are not Key-Value Pairs"、body: "They are Parameters, and encoded as such.") で、旧文 "Its value is a block of Key-Value Pairs" が現行文に置き換わった。count なしの KVP ブロックという読みは明示的に否定されている
- よって「`Number of Parameters` を含む count 付き」を唯一の符号化として確定する。encode は現状どおりとし、decode の「空バイト列 = 内側なし」を削除して、空バイト列の値は §8.3 に従い KEY_VALUE_FORMATTING_ERROR として拒否する
- 空バイト列の拒否は `decode_fill_parameters` で明示的に `MessageError::KeyValueFormattingError` を返す。`bytes.is_empty()` の分岐を削除するだけでは `MessageParameters::decode_inner` から `varint::decode` が `UnexpectedEof` を返し、§8.3 が求める KEY_VALUE_FORMATTING_ERROR にならない
- 他実装の符号化の確認は本 issue の結論を変えない。§9.20.16 の「別メッセージの Parameters として符号化する」と各メッセージ形式の `Number of Parameters`、§8.3 の MUST により空バイト列の拒否で確定する。他実装が空バイト列を送出することが判明した場合は、受理範囲を広げるかを別 issue で扱う
- 変更対象は `src/message_parameter.rs` の `decode_fill_parameters` とその doc コメント、`tests/test_message_parameter.rs` の `mod fill_parameters` である。session 層は `fill_parameters()` を呼ぶだけで空バイト列の分岐を持たないため変更しない
- 実装後、`empty_fill_accepted` のテスト名と内容を確定した規則に合わせる。count 0 を表す値が `0x00` の 1 バイトであること、内側 1 個が `1` の count とパラメータ列になることを逐語で固定する
- `src/message_parameter.rs` の `decode_fill_parameters` の doc コメントにある「空バイト列は内側パラメータなしとして受け付ける」を確定した規則に合わせて書き換える

## 完了条件

- 確定した符号化に合わせて encode / decode のテストが更新され、内側 1 個の FILL_PARAMETERS の値が `Number of Parameters = 1` とパラメータ列になることを固定していること
- 内側 0 個の FILL_PARAMETERS の値が count 0 (`0x00` の 1 バイト) であり、その Length varint が 1 (`0x01`) であることを固定していること
- 空バイト列の FILL_PARAMETERS が KEY_VALUE_FORMATTING_ERROR で拒否されることがテストで固定されていること
- encode → decode の往復結果と、規則から導いたワイヤバイト列が一致することをテストしていること
- 空バイト列を値として使っている既存テスト (`duplicate_fill_rejected` / `nested_fill_on_decode_is_protocol_violation` / `nested_fill_rejected_before_recursion`) の期待値が新しい規則に合わせて更新されていること
