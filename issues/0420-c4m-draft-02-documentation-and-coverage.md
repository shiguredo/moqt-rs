# C4M draft 02 の仕様参照と認可 API の説明を更新する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/update-c4m-draft-02-documentation-and-coverage
- Polished: 2026-11-03

## 目的

[draft-ietf-moq-c4m-02](../refs/moq/draft-ietf-moq-c4m-02.txt) の全文差分に沿って、
公開する認可 API と利用上の責務を説明する。

## 現状

src/c4m.rs と各サブモジュール、docs/c4m.md、skills/shiguredo-moqt/SKILL.md は draft 01 の
JWT proof と moqt scopes 配列、旧ベクタを前提にする。
crates/tokio-moq/src/lib.rs の c4m_authorization_token は token をそのまま運ぶが、
token の運搬と認可・proof 検証を区別した説明が必要である。
README.md の対応仕様、Cargo.toml の C4M 依存コメント、docs/missing-features.md の基準版・CWT proof 未実装表・節番号にも draft 01 の記述が残る。

## 設計方針

実装の追従に合わせ、署名検証だけ・claim validation・scope 認可・DPoP / replay の検証を区別する。
「token を decode できる」ことを「action を認可できる」こととして説明しない。
I/O、時計、接続識別子、replay state の所有者を明記する。
暫定 claim label と未確定の proof の運搬方法を正式仕様として確定させない。
未確定の actx / ath 番号や AUTHORIZATION_TOKEN 内の独自 proof envelope を黙って割り当てない。

## 変更対象

- `README.md` の C4M の対応仕様・使用例
- `docs/c4m.md` と `docs/missing-features.md` の C4M の対応版・実装状況・未対応項目
- `skills/shiguredo-moqt/SKILL.md` の C4M の対応版・API・使用例・仕様参照
- `src/c4m.rs` と各サブモジュールの公開 API・モジュール説明、`Cargo.toml` の C4M 依存コメント
- `crates/tokio-moq/src/lib.rs` の `c4m_authorization_token` の token 運搬に関する説明

0404 が扱う MOQT の説明更新とは C4M の範囲で分担し、未対応項目は先行 issue の実装結果を確認して更新する。
0412〜0419 が完了しても、0345 / 0346 / 0356 の既知ギャップまで解消済みとは扱わない。各課題の実装状況を確認し、残る制約と利用側の追加検証を説明する。0345 が未完了なら、サンプルでは非ゼロの moqt-reval を無条件に認可せず、利用側での拒否または再検証能力に応じた判定・実行を示す。

## draft 01 → 02 の差分対照

| 変更 | 追跡先・判定 |
| --- | --- |
| moqt claim value の map 化、key 0 の scopes | 0412 |
| 未知 map key がある claim set の fail-closed | 0412 |
| 付録の Private Use claim label -65537 / -65538 | 0413 |
| CLIENT_SETUP / SERVER_SETUP の SETUP=1 への統合、SUBSCRIBE_TRACKS=9 | 0414 |
| catreplay=1 の接続束縛、cti / exp 必須、切断後も期限まで保持 | 0415 |
| catreplay=0 / 省略 / 2 の違い、複数検証者間で state が必要という説明 | 0415 と本 issue。分散状態の実装は対象外 |
| JWT から COSE_Sign1 の CWT proof へ変更 | 0416 |
| protected alg / typ / kccs、cti / iat / actx / ath の CBOR 表現 | 0416 |
| COSE_Key の ckt=5 束縛と jkt の互換性 | 0417 |
| proof なしの cnf 付き token の禁止、raw token bytes の ath | 0418 |
| action ごとの bytes の完全一致、cti replay・鮮度 | 0418 |
| Appendix A の CBOR / COSE ベクタ、alg / MAC / 署名・異常系の更新 | 0419 |
| token 取得側の DPoP は RFC 9449 の方式を継続 | 本 issue。認可サーバーの実装は追加しない |
| 無関係の deployment 間の鍵共有を避ける、proof と token の message size を考慮 | 本 issue。0379 のサイズ上限の説明と整合させる |
| generic DPoP draft への依存削除、節移動、参照・IANA の更新 | 本 issue。汎用 JWT 機能を残す場合の位置付けも説明する |
| moqt-reval の基本的な再検証規則 | 本文の意味は継続。0345 の既存課題を重複起票しない |

## 完了条件

- README.md、docs/c4m.md、docs/missing-features.md、公開 SKILL、モジュール説明とサンプル、Cargo.toml の C4M 依存コメントが実装済みの draft 02 API に一致すること。
- `c4m_authorization_token` が token bytes を運ぶだけで認可・proof 検証を行わないことを、利用側が判断できる説明にすること。
- 必須の検証と呼び出し側に残る責務、暫定番号・未対応項目を利用者が判断できること。
- 旧 JWT / compact / key 3 の互換 API を残す場合、現在の C4M の標準形式と混同しないこと。
- 0345 / 0346 / 0356 と新しい追従 issue の関係が明確であること。
- 既知ギャップが未完了の場合も対応済みと断定せず、特に非ゼロ moqt-reval を既定の `CatClaims::validate` だけで受理してよいと説明しないこと。

## 依存・関連

- 先行: [0412](0412-c4m-draft-02-moqt-claim-map.md)
- 先行: [0413](0413-c4m-draft-02-provisional-claim-labels.md)
- 先行: [0414](0414-c4m-draft-02-action-codes.md)
- 先行: [0415](0415-c4m-draft-02-connection-scoped-bearer-replay.md)
- 先行: [0416](0416-c4m-draft-02-cwt-dpop-proof-codec.md)
- 先行: [0417](0417-c4m-draft-02-cose-key-thumbprint-binding.md)
- 先行: [0418](0418-c4m-draft-02-cwt-dpop-validation.md)
- 先行: [0419](0419-c4m-draft-02-test-vectors.md)
- 関連: [0345](0345-bug-c4m-moqt-reval.md)
- 関連: [0346](0346-bug-c4m-dpop-validation.md)
- 関連: [0356](0356-test-c4m-pbt-model-fuzz-gaps.md)
