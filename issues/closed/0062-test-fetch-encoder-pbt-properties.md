# FetchStreamEncoder の PBT に properties 経路を追加する

- Created: 2026-09-11
- Completed: 2026-09-17
- Branch: feature/test-fetch-encoder-pbt-properties
- Polished: 2026-09-17

## 目的

`FetchStreamEncoder::encode_object` の properties 経路 (`has_properties = true` と生バイト列の組み合わせ) を PBT でカバーし、delta 圧縮と properties の組み合わせで回帰が起きないようにする。

## 現状

`pbt/tests/prop_stream/encoder.rs` の `sample_objects` は `has_properties: false` 固定で、`encode_object` に `Some(...)` を渡す経路が PBT にない。`pbt/tests/prop_stream/fetch.rs` には `FetchStreamEntry` レベルの properties roundtrip があるが、`FetchStreamEncoder` の状態遷移を伴う経路は未カバーである。

また、`FetchStreamDecoder` は `ObjectPropertyTracker::observe_object` で Properties 生バイトを `ObjectProperties::decode` に通すため、PBT で生成する properties は妥当な Key-Value-Pairs でなければならない。`length_prefixed_properties` にランダムバイトを入れる方式は malformed として拒否される。

## 設計方針

- `sample_objects` で `has_properties` を抽選し、true の場合は `ObjectProperties::push` と `ObjectProperties::encode` で妥当な Properties 生バイト列を生成して `Some(...)` として渡す。
- トラッカーの意味論的検証に抵触しない型を使う (`PRIOR_GROUP_ID_GAP` / `PRIOR_OBJECT_ID_GAP` などは使わない)。
- roundtrip (`encode_object` → `drive_fetch`) で絶対値フィールドと payload_length が保存されることを検証する。`DecodedFetchObject` は properties 生バイトを公開していないため、properties 内容の復元検証は対象外とし、その旨をコメントに残す。
- 空スライス拒否は単体テストの責務とし、正常系サンプラーでは生成しない。

## 完了条件

- properties 付きオブジェクトを含む列で `encode_object` → `drive_fetch` の roundtrip が検証されること
- `cargo test -p pbt` が通ること
- 既存の PBT が壊れないこと

## 解決方法

`pbt/tests/prop_stream/encoder.rs` の `sample_objects` に properties 経路を追加した。

- `sample_properties/1` を追加し、`has_properties` を抽選する。true の場合は
  `ObjectProperties::push` と `ObjectProperties::encode` で妥当な Properties 生バイト列
  (Properties Length varint を含む) を生成して `Some(...)` として渡す
- 使う型は `PROP_OBJECT_DELIVERY_TIMEOUT` (0x02) と `PROP_SUBGROUP_DELIVERY_TIMEOUT` (0x06)
  だけにした。どちらも object 単体で完結し、`ObjectPropertyTracker` の意味論的検証
  (受信済み object 列との整合) に抵触しない。`PRIOR_GROUP_ID_GAP` (0x3C) /
  `PRIOR_OBJECT_ID_GAP` (0x3E) は使わない
- 2 つの型をどちらも落とした場合は `Properties Length = 0` のブロックになり、
  `has_properties = true` と Properties Length 0 の組み合わせもサンプルされる。
  空スライス (`Some(&[])`) は Properties Length varint を含まない契約違反入力であり、
  正常系サンプラーでは生成しない (拒否は単体テストの責務)
- roundtrip は `encode_object` → `drive_fetch` で絶対値フィールドと payload_length が
  保存されることを検証する。`DecodedFetchObject` は Properties 生バイトを公開しないため、
  Properties の内容そのものは復元検証しない旨をコメントに残した
  (デコード時に `ObjectProperties::decode` を通ることだけが検証される)

検証:

- `cargo test -p pbt` が通る (prop_stream は 17 件)
- 新しいサンプラーが実際に回帰を検出することを確認した。`FetchStreamEncoder::encode_object`
  が `has_properties` を落とす改変を一時的に入れると `encoder::encoder_decoder_roundtrip`
  が失敗し、戻すと通る
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通る
