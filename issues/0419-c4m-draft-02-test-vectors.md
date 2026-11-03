# C4M の Appendix A ベクタを draft 02 に更新する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/update-c4m-draft-02-test-vectors
- Polished: 2026-11-03

## 目的

[draft-ietf-moq-c4m-02 Appendix A.1 〜A.7](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

`tests/test_c4m/vectors.rs` は draft 01 の付録を定数化し、compact 3 分割 token、HMAC alg=-4、旧 moqt label と array、cnf key 3、旧 CBOR の CAT claim 例を使う。
draft 02 は COSE_Mac0 / COSE_Sign1 全体、alg=5、cnf jkt=323、Private Use label と map を使う新しい署名付きベクタへ更新している。

## 設計方針

新しい payload_cbor_hex / cose_hex / cose_b64 を定義元から採り込み、COSE の保護領域・MAC / 署名を独立した期待値で検証する。
改行とインデントの unfolding 規則を守り、claims_edn を独自の JSON claim 表現として解釈しない。
catv / catu / catnip / catalpn などの修正と異常系も確認する。
A.5 の `moqt_admin_wildcard` は actions が 0〜8、action 0 の期待値が true のままで、§2.1 Table 1 / 0414 の既知 action 1〜9 と一致しない。公式の payload / COSE / MAC と元の期待値は保持し、この不整合を明記して action 0 の認可期待値を通常の既知 action の検証から分離する。
action 0 は `MoqtAction::from_key` で未対応になることを固定し、SETUP の alias に戻さない。同じ公式 token に含まれる既知 action は通常どおり検証し、含まれない `SubscribeTracks=9` は認可しない。9 の許可は公式ベクタを改変せず、別の補助 fixture で検証する。
A.6 は OR / NOR / AND の label がすべて暫定の 0 で、wire だけでは演算子を区別できない。元の CBOR 構造 / COSE / MAC の確認に限定し、Composite の評価実装は追加しない。raw claim として保持・署名検証できることを Composite の受理・認可成功と混同せず、型付き scope 認可では許可にならないことを固定する。
旧 JWT / compact 汎用 API のテストを残す場合は、draft 02 の公式ベクタと混ぜない。

## 完了条件

- 新付録の token・payload・MAC / 署名を実暗号で検証し、自己生成値だけの roundtrip にしないこと。
- 改変 MAC、誤った鍵、形式 / algorithm の不一致、時刻・audience の異常系を新しい入力で検証すること。
- moqt map と暫定 label の対応を先行させ、ベクタの差を実装に都合よく書き換えないこと。
- A.5 の action 0 の不整合と A.6 の暫定 Composite を通常の認可成功ケースから分離し、公式バイト列・MAC / 署名を保持すること。
- 0356 の一般的な PBT / fuzz 改善は別課題として保ち、本 issue は一次資料ベクタの追従に絞ること。

## 依存・関連

- 先行: [0412](0412-c4m-draft-02-moqt-claim-map.md)
- 先行: [0413](0413-c4m-draft-02-provisional-claim-labels.md)
- 先行: [0414](0414-c4m-draft-02-action-codes.md)
- 関連: [0356](0356-test-c4m-pbt-model-fuzz-gaps.md)
