# MessageError に core::error::Error を実装する

- Created: 2026-09-20
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-message-error-core-error-trait
- Polished: {YYYY-MM-DD}

## 目的

`MessageError` を返す公開 API の結果を `?` 演算子で `Box<dyn std::error::Error + Send + Sync>` へ変換できるようにする。クレートを利用する側は、codec のエラーを自身のエラー型に包んで伝播させるために `?` を使う。現在は `MessageError` だけが `core::error::Error` を実装しておらず、利用側に `map_err` による明示的な変換を強いる。

## 現状

- `src/error.rs` の `MessageError` は `#[derive(Debug, PartialEq)]` と `impl core::fmt::Display` を持ち、`core::error::Error` の要件 (`Debug` + `Display`) を満たしている
- ただし `core::error::Error` を実装していない
- 同じクレートの `SessionError` / `SendRequestError` / `RecvRequestError` / `RecvDataStreamError` (`src/session/types.rs`) はすべて `impl core::error::Error` を実装済みで、`MessageError` だけが漏れている
- このため `MessageError` を返す公開 API の結果は `?` で `Box<dyn std::error::Error + Send + Sync>` へ変換できない (E0277)
  - `ControlMessage::encode` / `ControlMessage::decode`
  - `MessageDecoder::try_decode_message` / `MessageDecoder::try_decode_varint`
  - `LocProperties::encode` / `LocProperties::decode`
  - `MsfCatalogDocument::encode` / `MsfCatalogDocument::decode`
- 実際に `Box<dyn std::error::Error + Send + Sync>` を返す関数の中で `msg.encode()?` と書くと、`From<MessageError>` が無いためコンパイルエラーになる
- no_std は制約にならない。`core::error::Error` は Rust 1.81 で安定化されており、MSRV は 1.93 である。`std::error::Error` は `core::error::Error` の再エクスポートなので、std 環境の `Box<dyn std::error::Error>` へも変換できるようになる
- `MessageError` の variant が持つフィールドは `u64` / `&'static str` / `String` のみで、`Send + Sync + 'static` を満たしている

## 設計方針

- `src/error.rs` に `impl core::error::Error for MessageError {}` を追加する
- 他の 4 つのエラー型と同じ空実装にする。`MessageError` は他のエラーを内包しないため `source()` は既定の `None` でよい
- 変更は 1 行で、既存の API・wire 形式・`Display` の出力に影響しない

## 完了条件

- `MessageError` が `core::error::Error` を実装していること
- `MessageError` を返す公開 API の結果を `?` で `Box<dyn std::error::Error + Send + Sync>` へ変換できること
- 既存のテストが通り、`Display` の出力が変わらないこと
