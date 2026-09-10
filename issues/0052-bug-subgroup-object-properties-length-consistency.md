# encode の properties blob の Properties Length と実データ長の一致を検証する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-properties-length-consistency
- Polished: {YYYY-MM-DD}

## 目的

`SubgroupObject::encode` / `FetchStreamObject::encode` が、Properties Length が実際のデータ長と一致しない `properties_data` を素通しして不正ワイヤを生成するのを防ぐ。

## 現状

0033 で `SubgroupObject::encode` は空スライス `Some(&[])` を拒否するようになったが、次の入力は検証されない。

- `Some(&[0x40])` のように Properties Length の varint が途中で切れている blob
- 宣言した Properties Length と後続データ長が一致しない blob

`status` が Normal (`None` / `Some(0)`) のときは既存の非 Normal 検査も走らないため、これらの不正 blob がそのまま `buf` に書かれて `Ok` を返し、decoder が後続バイトを Properties Length として誤読する。`FetchStreamObject::encode` も同様。

根拠: draft-ietf-moq-transport-21 §11.1.3 (Object Properties): `Properties Length (vi64)` + Properties の組で、Length は後続バイト数を表す。

## 設計方針

書き込み開始前に `varint::decode(properties_data)` で Length varint を読み、消費バイト数と Length の和が `properties_data.len()` と一致することを検証する。status の Normal / 非 Normal を問わず行う。不一致・varint 不正は `ProtocolViolation` とする。

## 完了条件

- Properties Length が実データ長と一致しない `properties_data` が `ProtocolViolation` で拒否されること
- Properties Length = 0 を含むデータ (`&[0x00]`) と、Length が一致する非空プロパティが正常にエンコードされること
- `SubgroupObject::encode` / `FetchStreamObject::encode` の両方に適用されていること
- 回帰テストが `tests/test_stream/subgroup_object.rs` / `tests/test_stream/fetch_stream_object.rs` 等に追加されていること
