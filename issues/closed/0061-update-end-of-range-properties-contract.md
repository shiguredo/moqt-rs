# FetchStreamEntry::encode の EndOfRange variant で properties_data を拒否する

- Created: 2026-09-11
- Completed: 2026-09-17
- Branch: feature/update-end-of-range-properties-contract
- Polished: 2026-09-17

## 目的

`FetchStreamEntry::encode` の `EndOfNonExistentRange` / `EndOfUnknownRange` / `EndOfTimedOutRange` に `properties_data` を渡しても黙って無視される状態をなくし、呼び出し側の渡し間違いを検出できるようにする。

## 現状

`src/stream/fetch.rs` の `FetchStreamEntry::encode` は全 variant 共通の引数として `properties_data: Option<&[u8]>` を受け取るが、EndOfRange の 3 variant はこの引数を使わずに `Ok(())` を返す。`Some(...)` を渡してもエラーにならず、Properties を渡すつもりの誤った呼び出しが無言で成立する。

根拠: draft-ietf-moq-transport-21 §11.4.1.2 (End of Range): 「Subgroup ID, Priority and Properties are not present」。EndOfRange エントリに Properties フィールドは存在しない。

## 設計方針

- EndOfRange の 3 variant で `properties_data.is_some()` を `ProtocolViolation` として書き込み前に拒否する。
- `FetchStreamObject::encode` の `has_properties` と `properties_data` の整合性検査と同じ方針 (矛盾する入力は書き込み前に拒否する) に揃える。
- `FetchStreamEntry::encode` の doc に `# Errors` を追加し、EndOfRange では `properties_data` に `None` を渡す契約を明記する。
- Object variant と EndOfRange + `None` の既存挙動は変えない。

## 完了条件

- EndOfRange の各 variant + `properties_data = Some(...)` が `ProtocolViolation` で拒否されること
- EndOfRange + `None` と Object variant の既存挙動が維持されること
- 回帰テストが `tests/test_stream/fetch_stream_object.rs` 等に追加されていること
- doc の `# Errors` が実装と一致していること

## 解決方法

`src/stream/fetch.rs` の `FetchStreamEntry::encode` で、End of Range の 3 variant
(`EndOfNonExistentRange` / `EndOfUnknownRange` / `EndOfTimedOutRange`) に
`properties_data = Some(...)` が渡されたら `ProtocolViolation` を返すようにした。
判定は各 variant の書き込み前に行うため、拒否した場合 `buf` は変化しない。

- 検証は private 関数 `reject_end_of_range_properties/1` に集約した
- doc の `# Errors` に「オブジェクト以外の variant では `properties_data` に
  `Some(...)` を渡すと `ProtocolViolation` を返す (書き込みは行わない)。
  `None` なら常に `Ok(())` を返す」を追記した
- 根拠は draft-ietf-moq-transport-21 §11.4.1.2 (End of Range):
  "Subgroup ID, Priority and Properties are not present"
- `FetchStreamEncoder::encode_end_of_non_existent_range/4` などの内部呼び出しは
  いずれも `None` を渡しており、挙動は変わらない

回帰テストは `tests/test_stream/fetch_stream_object.rs` に追加した。

- `encode_end_of_range_with_properties_rejected`: 3 variant × `Some(&[0x01, 0xFF])` が
  `ProtocolViolation` になり、`buf` が変化しないこと
- `encode_end_of_range_without_properties_succeeds`: 3 variant × `None` が成功し、
  エンコード結果をデコードすると元の entry に戻ること

検証:

- `cargo test --workspace` が通る
- `cargo clippy --workspace --all-targets -- -D warnings` が警告なしで通る
- `cargo fmt --all -- --check` が通る
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が警告なしで通る
