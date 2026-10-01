# CHANGES.md の develop セクションを種別順に整理する

- Created: 2026-10-01
- Completed: {YYYY-MM-DD}
- Branch: feature/update-changes-develop-order
- Polished: 2026-10-01

## 目的

shiguredo-changelog は `## develop` のエントリを CHANGE → ADD → UPDATE → FIX の順に記載することを定めているが、現在の develop は時系列に追記されたままになっており、種別ごとの変更を追えない。リリース前に種別順へ整理する。

## 現状

- `CHANGES.md` の `## develop` は 640 行あり、[CHANGE] / [ADD] / [UPDATE] / [FIX] が時系列で混在している (例: [ADD] と [FIX] が [CHANGE] の間に点在する)
- 作成時点のエントリは 155 件あり、本体 (develop 直下) 102 件と `### misc` 53 件に分かれている。本体は CHANGE 15 件 / ADD 10 件 / UPDATE 1 件 / FIX 76 件、`### misc` は CHANGE 1 件 / ADD 5 件 / UPDATE 34 件 / FIX 13 件である
- `### misc` は規約上「機能に直接影響しない変更 (ドキュメント追加、リファクタリング等)」を置くサブセクションだが、機能に影響するエントリが 18 件混在している ([CHANGE] 1 件、[ADD] 4 件、[FIX] 13 件。例: `RequestStreamEnd::Reset` の `error_code` を `Option<u64>` にする変更、`playout::stretch` の追加)
- 直近のコミットも末尾への追記を続けており、種別順にはなっていない
- shiguredo-changelog の「リリース時の手順」は `## develop` を `## バージョン` に変更することだけを定めており、整理の手順は無い
- `### misc` の「catalog (MSF) と LOC の relay での扱いを docs に明記する」[UPDATE] エントリ (docs/MOQT-RELAY.md を参照するもの) は `issues/0188-update-remove-stale-moqt-relay-reference.md` が削除する予定であるため、本 issue では扱わない

## 設計方針

- エントリの置き場所は shiguredo-changelog の定義に従う。機能・公開 API・プロトコル挙動に影響するエントリは `## develop` 直下、機能に直接影響しないエントリ (テスト、ドキュメント、リファクタリング、CI ツール設定など) は `### misc` とする
- 現状と配置が異なるエントリを移動する。misc から本体へ移すのは [CHANGE] 1 件、[ADD] 4 件、[FIX] 13 件の計 18 件である。本体から misc へ移す対象は無い
- `## develop` 直下のエントリを CHANGE → ADD → UPDATE → FIX の順に、`### misc` のエントリも CHANGE → ADD → UPDATE → FIX の順に並べ替える
- `### misc` サブセクションは本体の後に残す。並べ替え後の本体は CHANGE 16 件 / ADD 14 件 / UPDATE 1 件 / FIX 89 件、`### misc` は ADD 1 件 (E2E テストの追加) / UPDATE 34 件になる
- エントリの文言・種別タグ・担当者行・インデントは変更せず、エントリ単位の移動・並べ替えだけを行う
- 同種別内の順序は移動前の出現順を維持する (安定ソート)
- エントリは `- [種別]` で始まる行から、次の `- [種別]` 行の直前までを 1 ブロックとして扱い、ブロック単位で移動する
- リリース済みセクションは変更しない
- 時系列追記を正式な運用にするかは shiguredo-changelog の規定変更を伴うため、この issue では扱わない

## 完了条件

- `## develop` 直下のエントリと `### misc` のエントリが、それぞれ CHANGE → ADD → UPDATE → FIX の順に並んでいること
- 移動前後でエントリの欠落・文言変更・担当者行の欠落が無いこと (本 issue の変更でエントリ総数が増減しないこと)
- `prek run --all-files` が通ること (markdownlint を含む)

## 解決方法

{未着手}
