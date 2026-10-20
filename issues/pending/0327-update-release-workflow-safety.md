# リリース workflow のタグ条件と action 固定を見直す

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/update-release-workflow-safety
- Polished: {YYYY-MM-DD}

## 目的

`.github/workflows/release.yml` は任意のタグ push で `cargo publish` を実行し、タグ名と `Cargo.toml` の version の一致も検証していない。誤ったタグでも crates.io に publish され得る。

また `shiguredo/github-actions/.github/actions/slack-notify@main` は可変参照で、リポジトリ内の他の action と異なりコミット SHA に固定されていない。

本 issue ではリリースタグの形式を限定し、タグと version の一致を publish 前に検証し、slack-notify をコミット SHA に固定する。

## 現状

- `on.push.tags` が `"*"` で、任意のタグがリリース workflow を起動する (`.github/workflows/release.yml` の `on.push.tags`)。
- `publish` ジョブは `github-release` の後に `cargo publish` を実行するが、タグ名と `Cargo.toml` の `version` を比較する検証が無い。`Cargo.toml` の `version` は `2026.0.0` である。
- `slack_notify` ジョブの `uses: shiguredo/github-actions/.github/actions/slack-notify@main` が `@main` の可変参照である。他の action (`actions/checkout@... # v7.0.1`、`rust-lang/crates-io-auth-action@... # v1.0.5`) はコミット SHA + バージョンコメントで固定されている。
- `shiguredo/github-actions` リポジトリは 2026-10-20 時点でバージョンタグ・リリースを持たない。`main` の最新コミット SHA は実装時に取得する必要がある。
- 影響: 誤操作で意図しないバージョンを publish し得る。slack-notify は可変参照のため、同じ workflow でも実行時に異なる action コードが使われ得る。

## 前提

- `CODEBASE.md` の「この指示がなくなるまでは GitHub Actions 対応はしないこと」は、`issues/0325-doc-codebase-github-actions-consistency.md` で削除する方針が示されている。本 issue は既存 workflow の保守であり新規の GitHub Actions 対応を追加するものではないが、0325 が未完了の間は本 issue の変更を行わない。

## 設計方針

- `on.push.tags` を `"20[0-9][0-9].*"` に変更し、タグ push の対象を 4 桁の年で始まるタグに限定する。GitHub Actions の filter pattern では `?` は直前の文字の 0 回または 1 回の繰り返しを意味するため、`20??.*` は `2026.0.0` に一致しない (GitHub Docs "Filter pattern cheat sheet")。`[]` の文字クラスを使う。
- `verify` ジョブを追加し、`github-release` と `publish` の前にタグ名と `Cargo.toml` の `version` を検証する。`verify` が失敗した場合は `github-release` も `publish` も実行しない。
  - `verify` は Rust toolchain が必要なため `ubuntu-24.04` で実行する (`ubuntu-slim` には Rust が無い)。
  - タグ名は `GITHUB_REF_NAME` から取得し、`^20[0-9]{2}\.[0-9]+\.[0-9]+(-canary\.[0-9]+)?$` に一致することを確認する。
  - `Cargo.toml` の `version` は `cargo metadata --no-deps --format-version 1` の `packages` から `name == "shiguredo_moqt"` の `version` を取得する。`grep` による行抽出は使わない。
  - タグ名と version が完全一致しない場合は `exit 1` で失敗させる。canary タグ (`2026.0.0-canary.1` 等) も同じ規則で `Cargo.toml` の version と一致させる。
- `github-release` は `needs: verify`、`publish` は `needs: [verify, github-release]` にする。`github-release` は現在のタグ名取得方法 (`GITHUB_REF` からの文字列置換) を維持し、canary を含むタグは prerelease とする。`slack_notify` の `needs: [github-release, publish]` と `if: always()` は変更しない。
- `slack_notify` の `uses: shiguredo/github-actions/.github/actions/slack-notify@main` を、実装時に取得した `main` の最新コミット SHA に固定する。`shiguredo/github-actions` はバージョンタグを持たないため、コメントは `# main (YYYY-MM-DD)` のように取得時点の日付を書き、バージョン番号は書かない。
- `shiguredo/github-actions` は `shiguredo-github-actions` スキルの「時雨堂のリポジトリですでに利用実績があるもの」に該当するため、action 自体は変更しない。SHA 固定はリポジトリ内の他の action と同じ運用に合わせる。
- `CODEBASE.md` の「Pull-Request やブランチを作らず develop にコミットしていくこと」「1 issue 1 commit 1 push」に従い、`Branch:` のブランチは作らず develop に直接コミットする。`CHANGES.md` にはエントリを追加しない。
- `.github/workflows/release.yml` 以外の workflow (`ci.yml` / `ci-windows.yml` / `e2e-test.yml`) は変更しない。

## 完了条件

- `on.push.tags` が `"20[0-9][0-9].*"` になり、`2026.0.0` と `2026.0.0-canary.1` の形式のタグが filter に一致すること。`Cargo.toml` の version と一致するタグでは `github-release` と `publish` が実行されること。
- `verify` ジョブがタグ名と `Cargo.toml` の version を比較し、不一致なら `github-release` と `publish` を実行しないこと。
- `slack-notify` がコミット SHA で固定され、`@main` 参照が残っていないこと。
- すべての外部 action がコミット SHA で固定されていること。
- `actionlint` などで workflow の構文を確認し、エラーが無いこと。`prek` には workflow の構文チェックが無いため、`prek run --all-files` だけではこの条件を満たさない。
- `prek run --all-files` が通ること (Markdown / TOML / Rust の既存フック)。

## pending にする理由

`CODEBASE.md` の「この指示がなくなるまでは GitHub Actions 対応はしないこと」が残っており、`.github/workflows/release.yml` の変更はこの方針に反すると解釈され得る。`issues/0325-doc-codebase-github-actions-consistency.md` がこの行を削除する issue であり、0325 の完了後に本 issue を再開する。0325 が未完了の間は、本 issue の変更を行わない。詳細は「## 前提」を参照。
