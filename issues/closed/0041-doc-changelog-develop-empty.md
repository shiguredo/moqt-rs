# CHANGES.md の develop セクションを実態に合わせる

- Created: 2026-09-10
- Completed: 2026-09-17
- Branch: feature/doc-changelog-develop-empty

## 目的

`CHANGES.md` の `## develop` セクションを実エントリのある状態にするか、未リリース変更が無いなら見出しを整理する。

## 現状

`CHANGES.md` の `## develop` は `### misc` のみで、変更エントリが 1 件も無い。リポジトリはタグ未作成 (初版未リリース) のため書式上は許容範囲だが、`### misc` 見出しだけが残っている。

## 設計方針

未リリースの変更が無いなら `### misc` を削除する。変更があるなら `shiguredo-changelog` 規約に従って種別 (UPDATE / ADD / CHANGE / FIX) ごとに追記する。

## 完了条件

- `## develop` に空の `### misc` だけが残っていないこと
- 記載がある場合、`shiguredo-changelog` 規約の書式に従っていること

## 解決方法

本 issue の前提 (「`## develop` は `### misc` のみで変更エントリが 1 件も無い」) は既に解消
しているため、コードとドキュメントの変更は行わず、現状を確認して閉じる。

- `## develop` には種別付きのエントリがある
  (`[CHANGE]` 7 件 / `[ADD]` 1 件 / `[FIX]` 47 件。種別の順序は CHANGE → ADD → FIX で規約どおり)
- `### misc` は空ではなく、ドキュメント変更の `[UPDATE]` エントリを持つ
  (shiguredo-changelog 規約は「機能に直接影響しない変更は `### misc` に記載する」としており、
  空の見出しだけが残っている状態ではない)
- 変更履歴は未リリースの変更を `## develop` に追記する運用が定着しており、空の `### misc` は
  残っていない

確認は `CHANGES.md` の `## develop` から `### misc` までを種別ごとに集計して行った。
