# コード内の draft-21 節番号・引用の誤りを一括修正する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-draft-21-section-references

## 目的

ソースコードのコメント・doc が引用する draft-ietf-moq-transport-21 の節番号と文面を一次資料 `refs/moq/draft-ietf-moq-transport-21.txt` に一致させ、仕様トレーサビリティを回復する。実装の挙動は変えない。

## 現状

一次資料と一致しない引用が広範囲に存在する。代表例:

- `src/error.rs` の `PUBLISH_DONE_MALFORMED_TRACK` doc: `§9.9 (PUBLISH_DONE): "A relay publisher detected ... (see Section 2.4.2)."` → 引用元は §12.4、原文は `(see Section 12.1)`
- `src/session/data.rs`: `§5.1.5 (Combining Filters)` → §3.3.3、`§5.1.1 の "Objects MUST NOT be sent"` → §3.1.1、`§11.4.2 の SUBGROUP_ID_MODE 0b01` → §11.3.1
- `src/subgroup_tracker.rs`: `§11.4.3` → §11.3.2
- `src/session/subscription/send.rs`: `§5.1.2 / §5.1.4` → §3.3.1 / §3.3.2、`§11.1 (Track Alias)` → §3.1.2、`§10.9 (REQUEST_UPDATE)` → §9.5
- `src/session/fetch.rs`: `§2.5.1 (Mandatory Track Properties)` → §3.6、`§10.5 (REQUEST_OK)` → §9.3、`§5.2 (Fetch State Management)` → §3.2.1
- `src/stream/decoder.rs`: `(Section 15.9)` → §16.9、`(see Section 11.4.4)` → `(see Section 11.4.1)`
- `src/message/common.rs` / `src/message.rs` / `src/name.rs` の 4096 バイト制限: `§2.4.1 (Track Naming)` → §8.7

## 設計方針

各引用を正しい節番号・文面へ修正する。あわせて再発防止として、引用節タイトルの一致を検査する仕組み (prek フック等) を検討する。検査の追加は本 issue の完了条件には含めず、必要なら別 issue とする。

## 完了条件

- 上記および同種の誤引用が一次資料と一致すること
- 実装の挙動が変わらないこと
- 既存テストがすべて通ること
