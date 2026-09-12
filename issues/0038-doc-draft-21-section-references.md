# コード内の draft-21 節番号・引用の誤りを一括修正する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-draft-21-section-references
- Polished: 2026-09-13

## 目的

ソースコードのコメント・doc が引用する draft-ietf-moq-transport-21 の節番号と文面を一次資料 `refs/moq/draft-ietf-moq-transport-21.txt` に一致させ、仕様トレーサビリティを回復する。実装の挙動は変えない。

## 現状

一次資料と一致しない引用が広範囲に存在する。代表例はすべて develop 上で実在を確認済み。

- `src/error.rs` の `PUBLISH_DONE_MALFORMED_TRACK` doc: `§9.9 (PUBLISH_DONE): "A relay publisher detected ... (see Section 2.4.2)."` → 引用元は §12.4、原文は `(see Section 12.1)`
- `src/session/data.rs`: `§5.1.5 (Combining Filters)` → §3.3.3、`§5.1.1 の "Objects MUST NOT be sent"` → §3.1.1、`§11.4.2 の SUBGROUP_ID_MODE 0b01` → §11.3.1
- `src/subgroup_tracker.rs`: `§11.4.3` → §11.3.2
- `src/session/subscription/send.rs`: `§5.1.2 / §5.1.4` → §3.3.1 / §3.3.2、`§11.1 (Track Alias)` → §3.1.2、`§10.9 (REQUEST_UPDATE)` → §9.5
- `src/session/fetch.rs`: `§2.5.1 (Mandatory Track Properties)` → §3.6、`§10.5 (REQUEST_OK)` → §9.3、`§5.2 (Fetch State Management)` → §3.2.1
- `src/stream/decoder.rs`: `(Section 15.9)` → §16.9、`(see Section 11.4.4)` → `(see Section 11.4.1)`
- `src/message/common.rs` / `src/message.rs` / `src/name.rs` の 4096 バイト制限: `§2.4.1 (Track Naming)` → §8.7

さらに、`(see Section 3.3.3)` を `(see Section 6.4.2.3)` に直す同種の誤引用が残っている (0028 が `src/session/data.rs` / `src/session/types.rs` の §12.1 引用について先行修正済みで、それ以外が残る)。

- `src/session/subscription/dispatch.rs`: §3.1.1 の引用にある `(see Section 3.3.3), or after receipt` → `(see Section 6.4.2.3), or until receipt` (文面も `or until` が原文)
- `tests/test_session/fetch/request_update.rs`: §3.2.1 の引用にある `(see Section 3.3.3)` → `(see Section 6.4.2.3)`
- `tests/test_session/subscription/request_update.rs`: §3.1.1 の引用にある `(see Section 3.3.3)` → `(see Section 6.4.2.3)`

同じく、`§5.1` の実際の節タイトルは `Priorities` であり、購読の許可根拠として `§5.1` を引用する箇所は §3.1 (Subscriptions) が正しい。

- `src/session/data.rs`: `(draft §5.1)` → §3.1
- `src/session/subscription/send.rs`: `§5.1 が明示的に許可している` / `§5.1 (Subscriptions)` → §3.1

文面または引用元が一次資料と一致しないその他の例として次がある。

- `src/message/common.rs` の `byte_length` doc: `"The length of a Track Namespace is the sum of the Track Namespace Field Length fields."` は §8.7 の原文であり、§2.4.1 の引用ではない
- `tests/test_message_parameter.rs`: Range Filter の検証を `§5.1.4` と引用するが、Range Filter は §3.3.2 (§5.1.4 は不存在)

## 設計方針

- 対象範囲は `src/` と `tests/` にある draft-ietf-moq-transport-21 の節番号・引用とする。`pbt/` と `examples/` は対象外とし、必要なら別 issue とする。
- 対象範囲の引用を ripgrep で全件列挙し、節番号・節タイトル・引用文を一次資料と 1 件ずつ突合して修正する。列挙漏れを防ぐため、修正後に再度全件を洗い出し、未修正が残っていないことを確認する。
- 実装の挙動は変えない (コメント・doc のみの変更)。
- 4096 バイト制限の引用および同節 (§8.7 (Track Namespace Structure)) の定義文を引用する箇所は §8.7 に修正する。同じ `§2.4.1 (Track Naming)` という表記でも、0〜32 フィールドという構造を述べる箇所は §2.4.1 が正しいため一律置換しない。
- 再発防止として、引用節タイトルの一致を検査する仕組み (prek フック等) を検討する。検査の追加は本 issue の完了条件には含めず、必要なら別 issue とする。

## 完了条件

- 上記に列挙した誤引用と、ripgrep で洗い出した同種の誤引用 (`src/` と `tests/` が対象) が一次資料と一致すること
- 4096 バイト制限の引用および §8.7 の定義文の引用は §8.7 へ修正し、0〜32 フィールド数の `§2.4.1` 引用は維持されていること
- 実装の挙動が変わらないこと
- 既存テストがすべて通ること
