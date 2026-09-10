# FetchStreamObject::encode の properties 検証を書き込み前に強化する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-fetch-stream-object-encode-validation
- Polished: {YYYY-MM-DD}

## 目的

`FetchStreamObject::encode` が空 properties で Properties Length 欠落の不正ワイヤを生成しうるのを防ぎ、`SubgroupObject::encode` (0033 で修正済み) と検証契約を揃える。

## 現状

`src/stream/fetch.rs` の `FetchStreamObject::encode` は `has_properties=true` かつ `properties_data=Some(&[])` を許し、`buf.extend_from_slice(props)` が 0 バイトになるため Properties Length varint が書かれないワイヤを生成しうる。`SubgroupObject::encode` で 0033 が修正した不具合と同型。

また `&mut Vec<u8>` を取るため、検証順によっては失敗時に `buf` に部分バイトが残る余地がある。

根拠:

- draft-ietf-moq-transport-21 §11.1.3 (Object Properties): Properties Length (vi64) が必須
- §11.4.1.1 (Flags): has_properties が立っている場合に Properties フィールドが present

## 設計方針

- `has_properties=true` かつ `properties_data` が空スライスの場合を `ProtocolViolation` で拒否する。
- 書き込み開始前に済ませられる検証を先に行い、失敗時に `buf` を変更しない。
- doc の Errors に返りうる条件を明記する。

## 完了条件

- `has_properties=true` + 空 properties が `ProtocolViolation` で拒否されること
- 不正入力で失敗したときに `buf` が呼び出し前と一致すること
- 正常系 (Properties Length = 0 を含むデータ、非空データ) が壊れていないこと
- 回帰テストが `tests/test_stream/fetch_stream_object.rs` 等に追加されていること
