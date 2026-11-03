# FILL_PARAMETERS をパラメータ数による符号化に変更する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-fill-parameters-count
- Polished: {YYYY-MM-DD}

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
