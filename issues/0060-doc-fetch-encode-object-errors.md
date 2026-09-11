# FetchStreamEntry::encode と FetchStreamEncoder::encode_object に properties 契約と Errors を明記する

- Created: 2026-09-11
- Completed: {YYYY-MM-DD}
- Branch: feature/doc-fetch-encode-object-errors
- Polished: {YYYY-MM-DD}

## 目的

利用者が実際に呼ぶ公開 API の rustdoc から、`properties_data` の契約と返りうるエラーを読み取れるようにする。`FetchStreamObject::encode` にだけ契約が書かれている状態では、上位 API を使う利用者が空スライスや `has_properties` の組み合わせ不正を事前に知りえない。

## 現状

`src/stream/fetch.rs` の `FetchStreamEntry::encode` と `src/stream/encoder.rs` の `FetchStreamEncoder::encode_object` は `properties_data: Option<&[u8]>` を受け取るが、doc は「`has_properties` が true の場合に渡す」程度にとどまり、次が読み取れない。

- `properties_data` は Properties Length varint を含む生バイト列であり、空スライス (`Some(&[])`) は `ProtocolViolation` になること
- `has_properties` と `properties_data` の組み合わせが不正な場合に `ProtocolViolation` を返すこと
- prior 参照文脈の違反や、Datagram 起源で Subgroup ID を持つ場合も `ProtocolViolation` になること

`FetchStreamObject::encode` には 0051 で `# Errors` と契約が追加済みだが、利用者が実際に呼ぶ `FetchStreamEntry::encode` と `FetchStreamEncoder::encode_object` には伝わっていない。

根拠: `shiguredo-rust` の「公開 API（型・関数・フィールド・variant）には必ず書くこと」「発生しうるパニックやエラーの条件など、シグネチャから読み取れない契約も `///` に書くこと」。

## 設計方針

- `FetchStreamEntry::encode` と `FetchStreamEncoder::encode_object` の doc に properties 契約を追記する。
- 両 API に `# Errors` を追加し、代表的な `ProtocolViolation` 条件を列挙する。詳細な条件は `FetchStreamObject::encode` の doc を参照する形にして重複を避ける。
- 実装の挙動は変えない。

## 完了条件

- 両 API の doc に properties 契約と `# Errors` が記載されていること
- `cargo doc -p shiguredo_moqt --no-deps` が警告なく通ること
- 既存テストが壊れないこと
