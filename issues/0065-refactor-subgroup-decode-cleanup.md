# SubgroupObject::decode の Properties 再デコードとマジックナンバーを整理する

- Created: 2026-09-12
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-subgroup-decode-cleanup
- Polished: {YYYY-MM-DD}

## 目的

`SubgroupObject::decode` に残る到達不能なエラー分岐を削除し、Normal status 判定のマジックナンバーを定数に置き換えて、encode 側と共通の検証に寄せる。

## 現状

- `src/stream/subgroup.rs` の `SubgroupObject::decode` は、`checked_len` で切り出した `properties_bytes` を再度 `varint::decode` し、失敗時に `ProtocolViolation("malformed properties length in subgroup object")` を返す。切り出し時点で「Length varint + ちょうど Length バイト」であることが保証されているため、この `map_err` 分岐は到達不能である。
- 同じ箇所の `matches!(status, Some(s) if s != 0)` の `0` は `src/stream.rs` の `pub(crate) const OBJECT_STATUS_NORMAL` (0x0) と同義だが、リテラルで書かれている。
- encode 側は 0052 で `validate_properties_blob` に統一済みのため、decode 側だけ重複した再デコードが残っている。

## 設計方針

- decode 側の再デコードを `validate_properties_blob` に置き換え、到達不能なエラーメッセージを削除する。
- Normal 判定に `OBJECT_STATUS_NORMAL` を使う。
- 挙動は変えない。

## 完了条件

- 到達不能な `map_err` 分岐が削除されていること
- `OBJECT_STATUS_NORMAL` が使われていること
- 既存テスト (`tests/test_stream/subgroup_object.rs` の非 Normal status 検査等) が通ること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
