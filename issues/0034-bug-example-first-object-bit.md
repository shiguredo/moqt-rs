# example の SubgroupWriter が FIRST_OBJECT を正しく設定する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-example-first-object-bit

## 目的

draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header) の FIRST_OBJECT bit の主張と example の実際の送信内容を一致させる。

## 現状

`examples/moqt-publisher/src/stream_writer.rs` の `SubgroupWriter` は `first_object: true` を常に設定する。コメント自身が「Object 0 がフィルタ不通過でスキップされた場合、ワイヤ上の最初の Object は original publisher が公開した最初の Object ではなくなり、FIRST_OBJECT bit の主張と矛盾する」と認めている。`examples/moqt-publisher/src/main.rs` は draft-21 準拠を宣言している。

根拠 (draft-ietf-moq-transport-21 §11.3.1 / §2.2): FIRST_OBJECT bit は「その Subgroup の最初の Object が original publisher により公開された最初の Object である」ことを示す。

## 設計方針

最初の Object が Pass するまでヘッダ送信を遅延し、確定した時点で FIRST_OBJECT を設定する。遅延が難しい場合は、既知の制約として `examples/README.md` / SKILL に明記する。

## 完了条件

- フィルタで最初の Object がスキップされた場合でも FIRST_OBJECT が正しく設定されること、または制約がドキュメント化されていること
- example のビルドと疑似キャプチャ動作が維持されること
