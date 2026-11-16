# MSF Event Timeline の index key を大文字化し data を任意 JSON 値にする

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §8.2 (Event Timeline data format) は、index key を小文字の `t` / `l` / `m` から大文字の `T` / `L` / `M` に変更し、`data` を JSON Object 固定から任意の JSON 値に変更した。
現行実装は小文字 key だけを読み、`data` が Object であることを要求するため、v02 の Event Timeline payload を decode できない。

## 現状

- `src/msf.rs` の `decode_event_entry` は `to_member("l")` / `to_member("t")` / `to_member("m")` で小文字 key だけを読む。v02 の `L` / `T` / `M` は index 欠如として拒否される。
- `MsfEventTimelineEntry` の `DisplayJson` は `"l"` / `"t"` / `"m"` を出力する。
- `decode_event_entry` と `validate_event_data_raw` は `to_object()` で `data` が Object であることを要求する。v02 の配列・文字列・数値・真偽値・null は拒否される。
- `tests/test_msf` と `pbt/tests/prop_msf.rs` が小文字 key と Object 前提のデータを生成している。
- v02 §8.6 の例は `T` / `L` / `M` の大文字 key を使う。

## 設計方針

- index key を `T` / `L` / `M` に変更する。decode は大文字 key だけを読み、encode も大文字 key だけを出力する。
- `data` は任意の JSON 値を保持できるようにする。既存の `data_raw` (生 JSON バイト列) を維持し、Object に限定する検証を外す。encode は受け取った生 JSON をそのまま出力する。
- 1 record に複数の index key を許さない検証と、同一 timeline 内で index 種別を統一する SHOULD の扱いは現状を維持する。SHOULD の統一検証は実装しない。
- tests と pbt の生成器を大文字 key と任意 JSON 値に更新する。§8.6 の例に相当するテストを追加する。`docs/msf.md` の Event Timeline の記述と、`MsfEventIndex` / `MsfEventTimeline` / `MsfEventTimeline::encode` の公開 doc も `T` / `L` / `M` と任意 JSON 値に更新する。

## 完了条件

- `[{"T": 1000, "data": {...}}]`、`[{"L": [0,0], "data": "text"}]`、`[{"M": 2000, "data": [1,2,3]}]` が decode / encode で往復一致すること。
- `data` が Object 以外 (配列・文字列・数値・真偽値・null) でも受理されること。
- 小文字 `t` / `l` / `m` だけを持つ record が index 欠如として `MessageError::InvalidCatalog` で拒否されること。
- 1 record に複数の index key を持つ場合が引き続き拒否されること。
- `docs/msf.md` と公開 doc が `T` / `L` / `M` と任意 JSON 値になっていること。
- 上記を `tests/test_msf` と `pbt/tests/prop_msf.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
