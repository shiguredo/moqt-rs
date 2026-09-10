# CHANGES.md の develop セクションを実態に合わせる

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
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
