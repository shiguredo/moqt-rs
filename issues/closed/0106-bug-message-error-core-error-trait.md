# エラー型に core::error::Error を実装する

- Created: 2026-09-20
- Completed: 2026-09-20
- Branch: feature/fix-error-types-core-error-trait
- Polished: 2026-09-20

## 目的

`core::error::Error` を実装していない公開エラー型に実装し、それらの型を返す公開 API の結果を `?` 演算子で `Box<dyn std::error::Error + Send + Sync>` へ変換できるようにする。クレートを利用する側は、codec のエラーを自身のエラー型に包んで伝播させるために `?` を使う。現在は `MessageError` / `NameParseError` / `ObjectFieldMismatch` が `core::error::Error` を実装しておらず、利用側に `map_err` による明示的な変換を強いる。

## 現状

- `src/error.rs` の `MessageError` は `#[derive(Debug, PartialEq)]` と `impl core::fmt::Display` を持ち、`core::error::Error` の要件 (`Debug` + `Display`) を満たしている
- ただし `core::error::Error` を実装していない
- 同じクレートの `SessionError` / `SendRequestError` / `RecvRequestError` / `RecvDataStreamError` (`src/session/types.rs`) は `core::fmt::Display` と `impl core::error::Error` を実装済みである。実装されていないのは `MessageError` / `NameParseError` / `ObjectFieldMismatch` の 3 つである
- `NameParseError` (`src/name.rs`) は `pub enum` で `parse_name` が返す。`core::error::Error` も `core::fmt::Display` も実装していない
- `ObjectFieldMismatch` (`src/object_properties.rs`) は `pub struct` で `ObjectFieldTracker::observe_object_fields` が返す。`core::error::Error` も `core::fmt::Display` も実装していない
- `core::error::Error` は `Debug` と `Display` を要求するため、上記 2 型には `Display` の追加も必要である (`MessageError` は `Display` を持っているので `Error` の追加だけでよい)
- このため 3 型を返す公開 API の結果は `?` で `Box<dyn std::error::Error + Send + Sync>` へ変換できない (E0277)
  - `MessageError` を返す例: `ControlMessage::encode` / `ControlMessage::decode` / `MessageDecoder::try_decode_message` / `MessageDecoder::try_decode_varint` / `LocProperties::encode` / `LocProperties::decode` / `MsfCatalogDocument::encode` / `MsfCatalogDocument::decode`
  - `NameParseError` を返す例: `parse_name`
  - `ObjectFieldMismatch` を返す例: `ObjectFieldTracker::observe_object_fields`
- 実際に `Box<dyn std::error::Error + Send + Sync>` を返す関数の中で `msg.encode()?` と書くと、`From<MessageError>` が無いためコンパイルエラーになる
- no_std は制約にならない。`core::error::Error` は Rust 1.81 で安定化されており、MSRV は 1.93 である。`std::error::Error` は `core::error::Error` の再エクスポートなので、std 環境の `Box<dyn std::error::Error>` へも変換できるようになる
- 3 型とも `Send + Sync + 'static` を満たしている。`MessageError` の variant が持つフィールドは `u64` / `&'static str` / `String` のみ、`NameParseError` はフィールドを持たない enum、`ObjectFieldMismatch` は `u64` / `u64` / `&'static str` である

## 設計方針

- `src/error.rs` の `MessageError` に `impl core::error::Error for MessageError {}` を追加する
- `src/name.rs` の `NameParseError` と `src/object_properties.rs` の `ObjectFieldMismatch` に `core::fmt::Display` を実装し、`impl core::error::Error` を追加する。`Display` のメッセージは既存のエラー型と同じく英語にする
- 3 型とも `SessionError` / `RecvRequestError` / `RecvDataStreamError` と同じ空実装にする。他のエラー型を内包しないため `source()` は既定の `None` でよい (`SendRequestError` は内包する `SessionError` を `source()` で返しており、この事情は 3 型には当てはまらない)
- 既存の API・wire 形式・`MessageError` の `Display` の出力に影響しない

## 完了条件

- `MessageError` / `NameParseError` / `ObjectFieldMismatch` が `core::error::Error` を実装していること
- 3 つの型を返す公開 API の結果を `?` で `Box<dyn std::error::Error + Send + Sync>` へ変換できること
- 既存のテストが通り、`MessageError` の `Display` の出力が変わらないこと

## 解決方法

- `src/error.rs` の `MessageError` に `impl core::error::Error for MessageError {}` を追加した
- `src/name.rs` の `NameParseError` に `core::fmt::Display` を実装し、`impl core::error::Error` を追加した。メッセージは 8 variant すべてについて variant の doc が示す失敗内容を英語で表す
- `src/object_properties.rs` の `ObjectFieldMismatch` に `core::fmt::Display` を実装し、`impl core::error::Error` を追加した。`group_id` / `object_id` / `reason` をすべて含める
- 3 型それぞれについて、`?` で `Box<dyn std::error::Error + Send + Sync>` へ変換できること、`Display` の出力、`source()` が既定の `None` であることをテストで検証した
  - `tests/test_error.rs`: `ControlMessage::decode` のエラーを `?` で伝播させる
  - `tests/test_name.rs`: `parse_name` のエラーを `?` で伝播させる。加えて 8 variant すべての `Display` を 1 つのテストで網羅する
  - `tests/test_object_properties.rs`: `ObjectFieldTracker::observe_object_fields` のエラーを `?` で伝播させる
- `FullNameTooLong` のメッセージは `MAX_TRACK_NAME_LENGTH` 定数から生成し、定数変更に追随するようにした
