# CODEBASE.md の GitHub Actions 非対応方針と workflows の実態を一致させる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/update-codebase-github-actions-consistency
- Polished: 2026-10-20

## 目的

CODEBASE.md は「この指示がなくなるまでは GitHub Actions 対応はしないこと」と定めるが、リポジトリには 4 つの GitHub Actions workflow が存在し、維持されている。規約と実態が矛盾しており、エージェントがどちらに従うべきか判断できない。本 issue では実態に合わせて規約側の記述を削除し、workflow の維持を妨げない状態にする。

## 現状

- CODEBASE.md の「## ルール」に次の 1 行が残っている。
  - 「この指示がなくなるまでは GitHub Actions 対応はしないこと」
- 同じ CODEBASE.md の「## MOQT」は次の記述で CI の存在を前提にしている。
  - 「実際の MoQ リレーが必要な E2E テストは `#[ignore]` を付け、CI が `--ignored` を付けて必ず実行すること (`e2e-tests/`)。通常の `cargo test` では実行しない」
- AGENTS.md の「GitHub Actions について」節は次の記述で GitHub Actions の利用を前提にしている。
  - 「GitHub Actions を作成・変更・レビューするときは `shiguredo-github-actions` スキルを参照すること」
- `.github/workflows/` には次の 4 つの workflow がある。
  - `ci.yml`: develop / feature ブランチへの push と pull_request で、prek / test / no-std-doc を実行する。
  - `ci-windows.yml`: `workflow_dispatch` のみで、Windows 上で moq-pub / moq-sub のコンパイルを確認する。
  - `e2e-test.yml`: develop / feature ブランチへの push と pull_request で、`secrets.TEST_MOQT_URI` が設定されている場合に E2E テストを実行する。
  - `release.yml`: タグ push で GitHub Release を作成し、crates.io へ publish し、Slack へ通知する。
- これらの workflow は CODEBASE.md の当該行の追記 (2026-10-10 の `28e3210`) 後も維持・追加されている。
  - `695d926` (2026-10-10): `ci.yml` の `j178/prek-action` を v3.0.1 に更新する。
  - `7ee3569` (2026-10-19): `ci-windows.yml` を新規追加する。
  - `cd6c33f` / `f297e07` (2026-10-19): `ci-windows.yml` のビルド対象とジョブ名を更新する。
- `e2e-tests/README.md` の「## CI」節は「`.github/workflows/e2e-test.yml` がこのテストを実行します。」と明記しており、workflow を前提にしている。
- `docs/windows-audio.md` は「- Windows の CI はコンパイル確認のみで、実デバイスの実行をしない (`.github/workflows/ci-windows.yml`)。実マイクを使う手動 E2E の手順を用意する。」と明記しており、workflow を前提にしている。
- 影響: `issues/0156-update-example-rustdoc-ci.md` は「なお `CODEBASE.md` は「この指示がなくなるまでは GitHub Actions 対応はしないこと」を定めており (2026-10-10 の整備で追記)、CI ジョブを追加する本 issue は現時点では着手できない。」と明記しており、非対応方針が実際に CI 追加作業を止めている。また、エージェントが既存 workflow を規約違反として削除する方向に判断するおそれがある。

## 設計方針

- GitHub Actions を維持する方針を採る。既存の 4 workflow は現行の CI / E2E / リリース / Windows の手動確認で使われており、削除するとこれらの機能が失われる。
- 非対応方針を維持する選択肢は採らない。CODEBASE.md 自身の MOQT 節が CI の存在を前提にしており、AGENTS.md も GitHub Actions の利用を前提にしている。workflow を削除すると、CODEBASE.md の E2E テスト運用と AGENTS.md の GitHub Actions 前提に整合しなくなる。
- CODEBASE.md の「## ルール」から `- この指示がなくなるまでは GitHub Actions 対応はしないこと` の 1 行だけを削除する。他の 3 行 (WebAssembly / CHANGES.md / Pull-Request とブランチ) は変更しない。
- `.github/workflows/ci.yml` / `ci-windows.yml` / `e2e-test.yml` / `release.yml` は変更しない。
- CODEBASE.md のみの変更なので、shiguredo-git の「整備」コミット規約に従い、develop に直接コミットする (`Branch:` のブランチは作らない)。コミットメッセージは `整備` とし、本文は書かない。
- 関連する README / docs (`e2e-tests/README.md` / `docs/windows-audio.md` / ルート `README.md` / `skills/shiguredo-moqt/SKILL.md`) は、GitHub Actions を維持する前提と矛盾しないことを確認する。現状は矛盾していないため変更しない。変更が必要な場合は別 issue にする (1 issue 1 commit の規約を守るため)。
- CHANGES.md にはエントリを追加しない (CODEBASE.md の「変更履歴を `CHANGES.md` に残さないこと」に従う)。

## 完了条件

- CODEBASE.md の「## ルール」から `- この指示がなくなるまでは GitHub Actions 対応はしないこと` の 1 行が削除されていること。
- CODEBASE.md の他のルール (WebAssembly / CHANGES.md / Pull-Request とブランチ) と、MOQT 節の CI 前提の記述が変更されていないこと。
- `.github/workflows/ci.yml` / `ci-windows.yml` / `e2e-test.yml` / `release.yml` が削除・変更されていないこと。
- `e2e-tests/README.md` の「## CI」節と `docs/windows-audio.md` の Windows CI の記述が変更されておらず、workflow を維持する前提と整合していること。
- ルート `README.md` と `skills/shiguredo-moqt/SKILL.md` が変更されておらず、GitHub Actions 非対応を前提とした記述がないこと。
- CHANGES.md にエントリが追加されていないこと。
- コミットメッセージが `整備` で、本文がないこと。
- `prek run --all-files` が通ること (markdownlint を含む)。
