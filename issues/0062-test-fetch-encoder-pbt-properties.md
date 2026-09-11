# FetchStreamEncoder の PBT に properties 経路を追加する

- Created: 2026-09-11
- Completed: {YYYY-MM-DD}
- Branch: feature/test-fetch-encoder-pbt-properties
- Polished: {YYYY-MM-DD}

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
