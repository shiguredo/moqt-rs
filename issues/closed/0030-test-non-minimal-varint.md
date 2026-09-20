# 非最小 varint エンコーディングの decode テストを追加する

- Created: 2026-09-10
- Completed: 2026-09-17
- Polished: 2026-09-17
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

## 解決方法

非最小エンコーディングの decode を固定値テストと PBT で固定した。

`tests/test_varint.rs` に `non_minimal` モジュールを追加した。

- `encode_with_len/2`: 値を指定バイト長の varint でエンコードするテスト用ヘルパー。
  先頭バイトのプレフィックスは長さごとの値、値ビットは先頭バイトの下位 `8 - len` ビットと
  後続のビッグエンディアン表現に載せる
- `two_byte_zero_is_accepted`: `0x8000` (0 の 2 バイト形) が 2 バイト消費で 0 になる
- `two_byte_37_is_accepted`: `0x8025` (37 の 2 バイト形) が 2 バイト消費で 37 になる
- `each_length_decodes_37`: 37 を 2〜9 バイトの各長でエンコードしたものが同じ値と
  同じ消費長になる
- `each_length_decodes_the_maximum_of_the_shorter_length`: 各長の最大値
  (0x7F / 0x3FFF / 0x1FFFFF / ... / 0x00FF_FFFF_FFFF_FFFF) を 1 バイト長い形で
  エンコードしても同じ値になる
- `trailing_bytes_are_not_consumed`: 非最小形の後ろに続くバイトを消費しない
  (呼び出し側が次のフィールドを読める)

`pbt/tests/prop_varint.rs` に `non_minimal_encoding_decodes_to_the_same_value` を
追加した。任意の値について最小長から 9 バイトまでの各長の非最小形を組み立て、
decode が同じ値と同じ消費長を返すことを検証する。後続フィールドを模したバイトを
足して消費長が伸びないことも見る。

根拠は draft-ietf-moq-transport-21 §8.1: "Variable-length integers do not need to be
encoded using the minimum number of bytes; any encoding length that can represent the
value is valid."

検証:

- `cargo test --test test_varint` が 42 件通る
- `cargo test -p pbt --test prop_varint` が 3 件通る
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通る
