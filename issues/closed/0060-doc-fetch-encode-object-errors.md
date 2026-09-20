# FetchStreamEntry::encode と FetchStreamEncoder::encode_object に properties 契約と Errors を明記する

- Created: 2026-09-11
- Completed: 2026-09-17
- Branch: feature/doc-fetch-encode-object-errors
- Polished: 2026-09-17

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

## 解決方法

`src/stream/fetch.rs` の `FetchStreamEntry::encode` と `src/stream/encoder.rs` の
`FetchStreamEncoder::encode_object` の doc に properties 契約と `# Errors` を追記した。
実装の挙動は変更していない。

`FetchStreamEntry::encode`:

- `properties_data` が Properties Length varint を含む生バイト列であること
  (draft-ietf-moq-transport-21 §11.1.3 (Object Properties)) を明記した
- プロパティが空でも Properties Length = 0 を含むデータ (`&[0x00]` など) を渡すこと、
  空スライス (`Some(&[])`) は契約違反入力であることを明記した
- `Object` variant 以外は `properties_data` を使い常に `Ok(())` を返すことを明記した
- `# Errors` に Datagram 起源の Subgroup ID、`has_properties` と `properties_data` の
  組み合わせ不正、Properties Length の不正・長さ不一致、prior 参照不正を列挙し、
  詳細は [`FetchStreamObject::encode`] の doc を参照する形にして重複を避けた

`FetchStreamEncoder::encode_object`:

- `FetchStreamEntry::encode` と同じ properties 契約を明記した
- `# Errors` に、この API 固有の `ProtocolViolation` (同一 Group で Object ID が増加しない、
  Group Order と逆方向の Group ID 変化による delta のアンダーフロー、同一 Subgroup での
  Publisher Priority 変化) を列挙し、加えて `FetchStreamObject::encode` の doc を参照した
- 成功時に内部の prior 状態を更新することを明記した

`# Errors` に挙げた条件は `encode_object` の実装と `FetchStreamObject::encode` /
`validate_properties_blob` (`src/stream.rs`) の実装を読み、実際に `ProtocolViolation` を
返す経路だけを記載した。到達しない経路 (Object ID delta のアンダーフロー、
`group_order` の未検証値) は列挙していない。

検証:

- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が警告なく通る
- `cargo fmt --all -- --check` が通る
- ドキュメントのみの変更であり既存テストに影響しない (`cargo test --workspace` で確認)
