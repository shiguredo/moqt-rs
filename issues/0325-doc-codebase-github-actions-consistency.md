# CODEBASE.md の GitHub Actions 非対応方針と workflows の実態を一致させる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/doc-codebase-github-actions-consistency
- Polished: {YYYY-MM-DD}

## 目的

CODEBASE.md は「この指示がなくなるまでは GitHub Actions 対応はしないこと」と定めるが、リポジトリには `.github/workflows/ci.yml` / `e2e-test.yml` / `release.yml` が存在し、維持されている。規約と実態が矛盾しており、どちらに従うべきか判断できない。

## 現状

- CODEBASE.md の GitHub Actions 非対応の記述が残っている。
- `.github/workflows/` に CI / E2E / リリースの 3 つの workflow がある。
- e2e-tests/README.md も CI 実行を前提にした記述になっている。
- 影響: エージェントが CODEBASE.md を根拠に workflow を削除したり、逆に追加をためらったりする。

## 設計方針

- GitHub Actions を維持する場合は CODEBASE.md の該当行を削除または更新する。
- 非対応を維持する場合は workflow と関連ドキュメントを削除する。
- どちらを採るかはプロジェクト方針の判断であるため、実装着手時にユーザーに確認する。
- CODEBASE.md のみの変更は shiguredo-git の「整備」コミット規約に従う。

## 完了条件

- CODEBASE.md の方針とリポジトリの実態が一致していること。
- 関連する README / docs の記述も同時に整合していること。
