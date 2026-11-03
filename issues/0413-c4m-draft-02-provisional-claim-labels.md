# moqt / moqt-reval の暫定 claim label を更新する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-c4m-draft-02-provisional-claim-labels
- Polished: {YYYY-MM-DD}

## 目的

[draft-ietf-moq-c4m-02 §8、Appendix A、A.5](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

`src/c4m.rs` の `CLAIM_MOQT` / `CLAIM_MOQT_REVAL` は draft 01 のベクタ由来の 327 / 328 を採用する。
`src/c4m/cat.rs` の typed key 判定・decode / encode もこの定数に依存する。
draft 02 は IANA の割当が未確定と明記し、ベクタでは Private Use の -65537 / -65538 を使う。

## 設計方針

新しいベクタの暫定 label を明示的に扱うよう、定数と CAT の処理を揃える。
Private Use の値を正式な IANA 割当として説明しない。
旧 327 / 328 を新 label の無条件 alias として解釈せず、旧版を維持する場合は版を明確に分ける。
moqt claim value の map 化とは別の変更として検証する。

## 完了条件

- 新 label の token で scope と revalidation 値を型付きに読み書きできること。
- `is_typed_claim_key` と raw claim の重複検出が新 label に一致すること。
- 未割当であることと採用する暫定値が公開説明・テストに明記されること。

## 依存・関連

- 関連: [0412](0412-c4m-draft-02-moqt-claim-map.md)
- 関連: [0419](0419-c4m-draft-02-test-vectors.md)
