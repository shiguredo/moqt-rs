# 対応済み Property のスコープ違反を malformed Track にする

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-property-scope
- Polished: {YYYY-MM-DD}

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §8.4、§12.1 条件 8](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/object_properties.rs` の `decode_kv_pairs` は Mandatory Track Property の範囲を Object で拒否するが、通常の Track-only 型 `MAX_CACHE_DURATION` / `DEFAULT_PUBLISHER_PRIORITY` / `DYNAMIC_GROUPS` などのスコープを一括検証しない。
`src/track_properties.rs` の decoder も Object-only の既知型をスコープで検証する仕組みを持たない。

## 設計方針

このライブラリが意味を理解する Property ごとに Track / Object の許可スコープを照合し、受信時の違反を MalformedTrack として返す。
IMMUTABLE_PROPERTIES の内側にも同じスコープ検証を行う。
未知 Property をスコープ推測で拒否せず、ローカル encode 入力の不正と受信 Track の検出を区別する。

## 完了条件

- Track-only の既知型が Object に現れた場合と、対応する Object-only 型が Track に現れた場合を検証すること。
- IMMUTABLE_PROPERTIES 内も同じ規則になり、未知型と正当な両スコープ型は受理されること。
- 違反で session 全体を不必要に閉じず、既存の malformed Track の cancel・通知経路へ渡すこと。

## 依存・関連

- 関連: [0311](0311-draft-22-error-classification-consistency.md)
- 関連: [0401](0401-moqt-draft-23-property-duplicates.md)
