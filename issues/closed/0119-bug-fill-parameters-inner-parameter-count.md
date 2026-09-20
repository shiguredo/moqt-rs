# FILL_PARAMETERS 内側の Number of Parameters の扱いを確定する

- Created: 2026-09-21
- Completed: 2026-09-23
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

## 解決方法

§9.20.16 の "encoded as if they were Parameters for a separate message" の "Parameters" を、各メッセージ形式が持つ
`Number of Parameters (vi64), Parameters (..)` (§9.6 (SUBSCRIBE) Figure 10、§9.11 (FETCH) Figure 15) と解釈し、
FILL_PARAMETERS の値は必ず `Number of Parameters` から始まる count 付きであると確定した。
§16.7 (Message Parameters) は登録表であり符号化を規定しないため、引用の根拠から外した。
§9.20 (Control Message Parameters) の "the block is bounded by a parameter count rather than a length" は制御メッセージ側の説明だが、
パラメータ列が count で区切られる同じ構造の補強として残した。

1. `src/message_parameter.rs` の `decode_fill_parameters` から、`bytes.is_empty()` のとき `Ok(MessageParameters::new())` を返す分岐を削除し、空バイト列は `MessageError::KeyValueFormattingError` で拒否するようにした。分岐を消すだけでは `varint::decode` の `UnexpectedEof` になるため、明示的に判定して返す
2. 返すコードは §8.3 (Key-Value-Pair Structure) の
   "If a receiver understands a Type, and the following Value or Length/Value does not match the serialization defined by that Type, the receiver MUST close the session with error code KEY_VALUE_FORMATTING_ERROR."
   を根拠にする。Type 0x23 を理解していて Length/Value が §9.20.16 の直列化 (count + Parameters) に一致しないため、この MUST がそのまま当てはまる。
   内側の並びが KVP ではなく Parameters であること (moq-wg/moq-transport#1868 の "FILL_PARAMETERS are not Key-Value Pairs") は、
   外側の FILL_PARAMETERS が Type と Length/Value を持つ Key-Value-Pair であることを否定しない
3. 余剰バイトの検査を `validate_scope` より先に行う現行順序を doc に明記した。既知型だが Table 6 に無いパラメータと余剰バイトが同時にある場合は KEY_VALUE_FORMATTING_ERROR を優先し、Table 6 外だけの場合は §9.20.16 の MUST に従い PROTOCOL_VIOLATION のままである。未知の型と入れ子の FILL_PARAMETERS は `decode_inner` の段階で先に PROTOCOL_VIOLATION になるため、この優先順位の対象外である
4. 値が途中で切れた場合 (UnexpectedEof) と count がバッファ容量を超える場合 (ProtocolViolation) は値が空の場合の規則の対象外であり、フレーミング違反として外側のパラメータブロックと同じ分類のまま扱うことを doc に明記した
5. 公開 `MessageParameters::decode` に `# Errors` を追加し、到達しうる 4 variant (`KeyValueFormattingError` / `ProtocolViolation` / `MalformedAuthToken` / `UnexpectedEof`) を原因とともに網羅列挙した。
   列挙は実測 (境界バイトの全列挙と疑似乱数入力) で到達を確認した LOCATION_FILTER の EndGroup オーバーフロー、
   Track Namespace のデコード失敗 (フィールド数超過 / 空フィールド / 4096 バイト超過) を含む。
   `MessageParameterValue::FillParameters` variant の doc には、内側 0 個が `Number of Parameters = 0` の 1 バイトになることと、
   値が空の FILL_PARAMETERS を拒否することを追記し、符号化の引用元も §16.7 から各メッセージ形式に直した
6. encode は変更していない。内側 0 個は count 0 の `0x00` 1 バイト (外側の Length は 1) であり、従来の出力と同一である

受信挙動の変更は「値が空 (Length = 0) の FILL_PARAMETERS を拒否する」1 点と、それに伴い FILL_PARAMETERS を許可しないメッセージ (SUBSCRIBE_OK 等) に空値の FILL_PARAMETERS が届いた場合の code が PROTOCOL_VIOLATION (scope 違反) から KEY_VALUE_FORMATTING_ERROR (値の形式違反) に変わる点である (値のデコードが scope 検証より先に走るため)。どちらもセッションを閉じる。

テスト:

- `tests/test_message_parameter.rs` の `mod fill_parameters` を次のように更新・追加した
  - `empty_fill_accepted` を `empty_inner_parameters_are_encoded_with_count_zero` に改名した (issue が指摘した名前と内容のずれの解消)。外側 count=1 / delta=0x23 / Length = 1 / 内側 count=0 の `[0x01, 0x23, 0x01, 0x00]` を逐語で固定し、decode で内側が空として保持されることも確認する
  - `inner_parameters_start_with_number_of_parameters` を追加し、内側 1 個 (FILL_TIMEOUT = 100) の値が `[0x01, 0x23, 0x03, 0x01, 0x0A, 0x64]` になることと、decode で `fill_timeout() == Some(100)` になることを固定した
  - `empty_value_is_key_value_formatting_error` を `empty_fill_value_is_key_value_formatting_error` に改名し、値が空 (`[0x01, 0x23, 0x00]`) のとき KeyValueFormattingError になることを固定した
  - `nested_fill_on_decode_is_protocol_violation` を `nested_fill_rejected_before_value_validation` に改名し、内側の FILL の値を空にして「入れ子の拒否が値の検証より先」であることを固定した (値の検証が先なら KEY_VALUE_FORMATTING_ERROR になる入力である)
  - `nested_fill_rejected_before_recursion` を `nested_fill_is_rejected_by_nesting_check` に改名した。深さ 1 では再帰の有無を観測できないため、このテストは「入れ子判定そのもののエラーで拒否されること (内側スコープの検証ではない)」を固定し、再帰前の拒否は `deeply_nested_fill_is_rejected_before_recursion` が固定する
  - `truncated_inner_value_is_unexpected_eof` を追加し、内側の varint が途中で切れた場合は UnexpectedEof、同じ分類が外側のパラメータブロックでも成立することを固定した
  - `inner_count_exceeding_capacity_is_protocol_violation` を追加し、内側の count がバッファ容量を超える場合は ProtocolViolation、同じ分類が外側でも成立することを固定した
  - `known_type_outside_table6_and_trailing_bytes_prefer_formatting_error` を追加し、既知型だが Table 6 に無いパラメータと余剰バイトが同時にある場合は KeyValueFormattingError、余剰バイトが無ければ ProtocolViolation になることを固定した
  - `unknown_type_and_nested_fill_are_rejected_before_trailing_bytes` を追加し、未知の型と入れ子の FILL_PARAMETERS は余剰バイトより先に ProtocolViolation で拒否されることを固定した
  - `deeply_nested_fill_is_rejected_before_recursion` を追加し、1000 段の入れ子でも 2 段目で拒否され深部を再帰的に解釈しないことを固定した (再帰先行の実装では debug ビルドのテストスレッドでスタックオーバーフローになることを変異実験で確認した)
  - `duplicate_fill_rejected` のワイヤを count 付きの値に更新した (空値のままだと新規則で先に KeyValueFormattingError になり「重複による拒否」を固定できなくなるため)
- `tests/test_message.rs` に次を追加した
  - `subscribe_with_empty_fill_parameters_is_rejected`: 空値の FILL_PARAMETERS を持つ SUBSCRIBE の wire を直接与え、メッセージ層でも KEY_VALUE_FORMATTING_ERROR がそのまま返ることを固定した (この wire は encode が生成しない不正形である旨をコメントに明記)
  - `subscribe_with_count_zero_fill_parameters_is_accepted`: API で encode した内側 0 個の SUBSCRIBE が 18 バイトの wire 全体で `... 0x01, 0x23, 0x01, 0x00` になることと、decode で空の内側として保持されることを固定した
  - `subscribe_ok_with_fill_parameters_reports_value_error_first`: FILL_PARAMETERS を許可しない SUBSCRIBE_OK に空値が届いた場合は KeyValueFormattingError、値が空でなければ従来どおり ProtocolViolation になることを固定した
- 各テストは変異実験で検出力を確認した
  - 空値チェックの削除 / 空値の分類を ProtocolViolation へ変更 / 余剰バイト検査の削除 / 余剰バイト検査と scope 検証の順序入替 / 内側の scope 検証の削除
  - encode が count を書かない / 入れ子の許可 / 入れ子の判定を値のデコード後へ移動 / 内側の UnexpectedEof を KeyValueFormattingError に変換
  - 余剰バイトの分類を ProtocolViolation へ変更 / count 超過の分類を UnexpectedEof へ変更 / 同一型重複検出の無効化
  - いずれの変異でも、対応するテストが失敗することを確認した

`CHANGES.md` の `## develop` に `[FIX]` を追加した。
