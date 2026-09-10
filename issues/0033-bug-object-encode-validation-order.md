# stream エンコードの検証順序を修正し部分書き込みを防ぐ

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-object-encode-validation-order

## 目的

不正入力で `buf` に壊れたフレームが残らないようにする。呼び出し側が `buf` を再利用しても安全にする。

## 現状

`src/stream/subgroup.rs` の `SubgroupObject::encode` と `src/stream/datagram.rs` の `ObjectDatagram::encode` は、flags / payload_length / properties を書いた後に `validate_object_status(status)?` する。不正 status では `buf` に途中までのバイトが残る。

また `SubgroupObject::encode` は `has_properties=true` かつ `properties_data=Some(&[])` を許し、Properties Length varint が書かれない (Length 欠落) ワイヤを生成しうる。非 Normal status のときだけ decode 失敗で拾われる。

## 設計方針

status の値域検証を書き込み前に行う。`Some(&[])` は明示的に拒否するか長さ 0 を補う。encode の検証を書き込み開始前に済ませる構造にする。

## 完了条件

- 不正 status の encode 失敗時に `buf` が変化しないこと
- `has_properties=true` + 空 properties が拒否または正しく長さ 0 でエンコードされること
- 回帰テストが `tests/test_stream/subgroup_object.rs` / `tests/test_stream/object_datagram.rs` に追加されていること
