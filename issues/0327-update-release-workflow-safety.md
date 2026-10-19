# リリース workflow のタグ条件と action 固定を見直す

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/update-release-workflow-safety
- Polished: {YYYY-MM-DD}

## 目的

`.github/workflows/release.yml` は任意のタグ push で `cargo publish` を実行し、タグ名と `Cargo.toml` の version の一致も検証していない。誤ったタグでも crates.io に publish され得る。また `shiguredo/github-actions/.github/actions/slack-notify@main` は可変参照で、他の action の SHA 固定方針と一致しない。

## 現状

- `on.push.tags` が `"*"` で、任意のタグがリリースを起動する。
- publish 前に `GITHUB_REF_NAME` と `Cargo.toml` の `version` を比較する検証が無い。
- slack-notify が `@main` 参照で、コミット SHA + バージョンコメントの形式になっていない。
- 影響: 誤操作で意図しないバージョンを publish し得る。サプライチェーンの再現性が無い。

## 設計方針

- タグ条件をリリースタグの形式 (例 `20??.*`) に限定する。
- publish 前にタグ名と `Cargo.toml` の version を比較し、不一致なら失敗させる。
- slack-notify をコミット SHA + バージョンコメントに固定する。
- shiguredo-github-actions の action 選定規約に適合させる。

## 完了条件

- 任意タグで publish が起動しないこと。
- タグと version の不一致で publish が失敗すること。
- すべての外部 action が SHA 固定されていること。
- workflow の構文チェック (`prek` 等) が通ること。
