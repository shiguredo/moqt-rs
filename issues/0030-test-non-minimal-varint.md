# 非最小 varint エンコーディングの decode テストを追加する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/test-non-minimal-varint

## 目的

draft-ietf-moq-transport-21 §8.1 (Variable-Length Integers) が許容する非最小エンコーディングの decode 挙動をテストで固定し、将来の回帰を防ぐ。

## 現状

`src/varint.rs` の `decode` は `leading_ones` 方式で余剰ビットを無視し、非最小形を受理する実装になっている。一方、`tests/test_varint.rs` と `pbt/tests/prop_varint.rs` は encode → decode の最小形ラウンドトリップのみで、非最小形を直接 decode するテストがない。

根拠 (draft-ietf-moq-transport-21 §8.1):

> "Variable-length integers do not need to be encoded using the minimum number of bytes; any encoding length that can represent the value is valid."

## 設計方針

各バイト長の非最小形 (例: `0x8025` = 37、`0x8000` = 0、7 バイト非最小形) を直接 decode する固定値テストを追加する。PBT では「任意の値について長めのエンコードを組み立てて decode できる」性質を追加する。

## 完了条件

- 各バイト長で非最小形の decode テストが存在すること
- 非最小形の PBT が存在すること
- 既存の最小形ラウンドトリップテストが維持されること
