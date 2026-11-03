# C4M の Appendix A ベクタを draft 02 に更新する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/update-c4m-draft-02-test-vectors
- Polished: {YYYY-MM-DD}

## 目的

[draft-ietf-moq-c4m-02 Appendix A.1 〜A.7](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

`tests/test_c4m/vectors.rs` は draft 01 の付録を定数化し、compact 3 分割 token、HMAC alg=-4、旧 moqt label と array、cnf key 3、旧 CBOR の CAT claim 例を使う。
draft 02 は COSE_Mac0 / COSE_Sign1 全体、alg=5、cnf jkt=323、Private Use label と map を使う新しい署名付きベクタへ更新している。

## 設計方針

新しい payload_cbor_hex / cose_hex / cose_b64 を定義元から採り込み、COSE の保護領域・MAC / 署名を独立した期待値で検証する。
改行とインデントの unfolding 規則を守り、claims_edn を独自の JSON claim 表現として解釈しない。
catv / catu / catnip / catalpn などの修正と Composite・異常系も確認する。
旧 JWT / compact 汎用 API のテストを残す場合は、draft 02 の公式ベクタと混ぜない。

## 完了条件

- 新付録の token・payload・MAC / 署名を実暗号で検証し、自己生成値だけの roundtrip にしないこと。
- 改変 MAC、誤った鍵、形式 / algorithm の不一致、時刻・audience の異常系を新しい入力で検証すること。
- moqt map と暫定 label の対応を先行させ、ベクタの差を実装に都合よく書き換えないこと。
- 0356 の一般的な PBT / fuzz 改善は別課題として保ち、本 issue は一次資料ベクタの追従に絞ること。

## 依存・関連

- 先行: [0412](0412-c4m-draft-02-moqt-claim-map.md)
- 先行: [0413](0413-c4m-draft-02-provisional-claim-labels.md)
- 関連: [0356](0356-test-c4m-pbt-model-fuzz-gaps.md)
