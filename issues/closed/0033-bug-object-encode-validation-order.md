# SubgroupObject の encode を書き込み前に検証し不正ワイヤと部分書き込みを防ぐ

- Created: 2026-09-10
- Completed: 2026-09-10
- Branch: feature/fix-object-encode-validation-order
- Polished: 2026-09-10

## 目的

`SubgroupObject::encode` が不正入力で `buf` に壊れたフレームを残さないようにし、`has_properties=true` なのに Properties Length が欠落した不正ワイヤを生成しないようにする。呼び出し側が `buf` を再利用しても安全にする。

## 現状

`src/stream/subgroup.rs` の `SubgroupObject::encode` は `object_id_delta` / Properties / `payload_length` を `buf` に書いた後に `validate_object_status(status)?` するため、不正 status では `buf` に途中までのバイトが残る。

また `has_properties=true` かつ `properties_data=Some(&[])` のとき、Properties の書き出し (`buf.extend_from_slice(props)`) が 0 バイトになり、
Properties Length varint が書かれない (Length 欠落) ワイヤを生成して `Ok` を返す。status が Normal (`None` または `Some(0)`) のときは
既存の properties 検査を通過してこの経路に入る。非 Normal status では `varint::decode(props)` が空スライスで失敗し encode 時点で拒否されるため、
この不具合は Normal status で発生する。

`ObjectDatagram::encode` (`src/stream/datagram.rs`) は `Result<Vec<u8>, _>` を返し内部で新規 `buf` を生成するため、失敗時に呼び出し側へ部分バイトは見えない。空 properties (`Some(&[])`) も既に拒否している。したがって本 issue の対象は `SubgroupObject::encode` に限定する。

根拠:

- draft-ietf-moq-transport-21 §11.1.2 (Object Status): 未知 status は protocol error
- §11.1.3 (Object Properties): Properties Length (vi64) が必須
- §11.3.1 (Subgroup Header): "Objects with no properties set Properties Length to 0."

到達可能性: リポジトリ内の呼び出し (`examples/moqt-publisher` / pbt) は `Some(&[])` を渡さないため、この不具合は公開 API 利用者が踏む。

## 設計方針

- `SubgroupObject::encode` の `validate_object_status(status)?` を書き込み開始前 (他の検証と同じ位置) に移す。
- `has_properties=true` かつ `properties_data == Some(&[])` を `ProtocolViolation` で明示的に拒否する。呼び出し側は `LocProperties::encode` が返す `[0x00]` (Properties Length = 0) のような Length 込みデータを渡す契約とする (既存 doc と一致)。
- `ObjectDatagram::encode` は対象外とする (部分書き込みが外部に観測されず、空 properties も既に拒否しているため)。
- 必要に応じて doc の Errors を更新し、`CHANGES.md` の `[FIX]` に記載する。

## 完了条件

- `SubgroupObject::encode` に既存バイトを含む `buf` を渡し、`payload_length == 0` かつ不正 status (`Some(0x01)` 等) で `Err` になり `buf` が呼び出し前と一致すること
- `has_properties=true` + `properties_data=Some(&[])` が `ProtocolViolation` で拒否されること
- `has_properties=true` + Properties Length = 0 を含むデータ (`&[0x00]` 等) は正常にエンコードされること
- 回帰テストが `tests/test_stream/subgroup_object.rs` に追加され、`cargo test --workspace` が通ること
- `CHANGES.md` の `[FIX]` に記載されていること

## 解決方法

`SubgroupObject::encode` の検証順序を修正し、不正ワイヤ生成と部分書き込みを防いだ。

- `validate_object_status(status)?` を全書き込みの前に移し、不正 status で `buf` に部分バイトを残さないようにした。
- `has_properties=true` かつ `properties_data` が空スライスの場合を `ProtocolViolation` で拒否し、Properties Length varint が欠落した不正ワイヤの生成を防いだ (呼び出し側は Length = 0 を含むデータを渡す契約)。
- `encode` の doc に `# Errors` を追加し、返りうるエラー条件を列挙した。
- `tests/test_stream/subgroup_object.rs` に不正 status の部分書き込み防止 (properties 経路含む)、空 properties の拒否、Normal / 非 Normal の Properties Length = 0 正常系 のテストを追加した。
- `CHANGES.md` の `[FIX]` にエントリを追加した。

残った制約 (スコープ外): `FetchStreamObject::encode` にも同型の空 properties / blob 長さ不整合の検証漏れがあり、別 issue 候補とする。また `properties_data` の Properties Length と実データ長の一致検証は本 issue のスコープ外。
