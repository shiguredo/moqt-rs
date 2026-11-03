# FILL_PARAMETERS をパラメータ数による符号化に変更する

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §9.19、§9.19.15](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message_parameter.rs` の `value_encoding` は `PARAM_FILL_PARAMETERS` を `LengthPrefixed` に分類する。
`decode_fill_parameters` と値のエンコード経路も長さ付き内側スコープを前提にしている。

## 設計方針

`FILL_PARAMETERS` の値を Number of Parameters (vi64) と、その個数の Parameters に変更する。
外側の byte length を残さず、外側パラメータの次エントリを内側へ巻き込まない。
内外の Delta Type の基準、内側許可集合、重複検証、再帰禁止、MAX_FILTER_RANGES の会計を維持する。
既存の型付き値 API が保持する内側パラメータの表現と codec の責務を整理する。

## 完了条件

- 0 個・複数個の内側パラメータ、外側に後続パラメータがある例の golden バイト列を確認すること。
- 個数・必須値の途中終端、未知型、内側に許されない型、再帰した FILL_PARAMETERS を検証すること。
- `tests/test_message_parameter.rs` と対応 PBT / fuzz 入力を新形式に合わせること。

## 解決方法

- `FILL_PARAMETERS` (Parameter Type 0x23) の値を、外側の byte length から
  「Number of Parameters (vi64) + その個数の Parameters」に変更した
  (draft-ietf-moq-transport-23 §9.19.15)。`ValueEncoding::FillParameters` を追加し、
  `value_encoding` の分類と encode / decode の経路を切り替えた。
- 型付き値 API `MessageParameterValue::FillParameters(MessageParameters)` は維持した。
  encode は内側 `MessageParameters::encode` の出力 (個数とデルタエンコード列) をそのまま
  値とし、decode は `MessageParameters::decode_inner(bytes, false)` の消費バイト数を
  そのまま返して個数ぶんだけ読む。外側パラメータの次エントリを内側へ巻き込まない。
- 内側スコープは独立した delta 基準 0 でデコードし、`FILL_PARAMETERS_ALLOWED_PARAMS`
  (Table 7 の 8 型) に限定した。内側・外側の重複検証、内側の値域検証、FILL_PARAMETERS の
  再帰禁止 (`allow_fill=false` と encode 側の型検査) を維持した。MAX_FILTER_RANGES の会計は
  `Session::range_filter_usage` が内側の Location Filter も合算する従来の経路を維持している。
- 内側スコープ違反のエラー文言を外側のメッセージ型スコープ違反と区別した。
- テスト: 0 個・1 個・2 個、外側の前置・後続がある場合の golden バイト列、count と値の
  途中終端、未知型、Table 7 外の型、内外の重複、入れ子 (1000 段を含む)、count 超過が
  外側エントリを巻き込む場合、内側 delta の基準が 0 であることを固定した。PBT では
  任意個の内側と外側の前置・後続を生成して encode / decode の往復と全体一致を検証した。

## 完了条件の検証

- 0 個・複数個の内側パラメータ、外側に後続パラメータがある例の golden バイト列:
  `tests/test_message_parameter.rs` の `fill_parameters` 配下の golden テストで固定した。
- 個数・必須値の途中終端、未知型、内側に許されない型、再帰した FILL_PARAMETERS:
  それぞれ `UnexpectedEof` / `ProtocolViolation` になるテストを追加した。
- `tests/test_message_parameter.rs` と対応 PBT / fuzz 入力の追随: テストと
  `pbt/tests/prop_message_parameter.rs` の sampler / property を新形式に更新した。
  fuzz は FILL_PARAMETERS 専用の seed を持たず任意バイト列入力のため変更不要だった。
