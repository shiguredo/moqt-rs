# CHANGES.md の develop セクションを種別順に整理する

- Created: 2026-10-01
- Completed: {YYYY-MM-DD}
- Branch: feature/update-changes-develop-order
- Polished: {YYYY-MM-DD}

## 目的

shiguredo-changelog は `## develop` のエントリを CHANGE → ADD → UPDATE → FIX の順に記載することを定めているが、現在の develop はリリース以降の時系列追記になっており、種別ごとの変更を追えない。リリース前に種別順へ整理する。

## 現状

- `CHANGES.md` の `## develop` は約 630 行あり、[CHANGE] / [ADD] / [UPDATE] / [FIX] が時系列で混在している (例: [ADD] と [FIX] が [CHANGE] の間に点在する)
- 直近のコミットも末尾への追記を続けており、種別順にはなっていない
- shiguredo-changelog の「リリース時の手順」は `## develop` を `## バージョン` に変更することだけを定めており、整理の手順は無い

## 設計方針

- `## develop` のエントリを CHANGE → ADD → UPDATE → FIX の順に並べ替える
- エントリの文言・担当者行・インデントは変更せず、移動だけを行う
- リリース済みセクションは変更しない
- 時系列追記を正式な運用にする場合は shiguredo-changelog 側の見直しが必要になるため、完了時にユーザーへ確認する

## 完了条件

- `## develop` のエントリが CHANGE → ADD → UPDATE → FIX の順になっていること
- 移動前後でエントリの欠落・文言変更・担当者行の欠落が無いこと
- `prek run --all-files` が通ること (markdownlint を含む)

## 解決方法

{未着手}
