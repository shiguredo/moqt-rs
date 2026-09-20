# シンボリックリンク用の prek フックを削除する

- Created: 2026-09-20
- Completed: 2026-09-20
- Branch: feature/remove-symlink-hooks
- Polished: {YYYY-MM-DD}

## 目的

CI の prek ジョブが常に失敗する状態を解消する。現在は `check-hooks-apply` が `check-symlinks` を「適用対象のファイルが無い」として検出し、CI が赤いままになっている。CI が常に失敗していると、他の変更で本物の失敗が起きたときに気づけない。

## 現状

- `prek.toml` の `check-hooks-apply` (meta フック) は「全フックが少なくとも 1 ファイルに適用されること」を検証する
- 同じ `prek.toml` の `check-symlinks` (builtin) はシンボリックリンクを対象とするフックである
- moqt-rs のリポジトリにシンボリックリンクは 1 つも無いため、`check-symlinks` の適用対象は 0 件になる
- その結果 `check-hooks-apply` が `check-symlinks does not apply to this repository` で失敗する。ローカルで `prek run check-hooks-apply --all-files` を実行すると再現する
- 併せて `destroyed-symlinks` も登録されているが、シンボリックリンクを使わないリポジトリでは検査対象が無い
- develop の直近 4 回の CI (整備 / 0103-0104 / 0106 open / 0106 polished) が同じ理由で failure になっている

## 設計方針

- `prek.toml` の builtin フック一覧から `check-symlinks` と `destroyed-symlinks` を削除する
- シンボリックリンクをリポジトリで使う予定が無いため、フックを残して `check-hooks-apply` の検出力を落とす判断は取らない
- `check-hooks-apply` と `check-useless-excludes` はそのまま残す。デッドフック検出は今後も有効に働かせる
- `prek.toml` 以外のファイルは変更しない

## 完了条件

- `prek.toml` から `check-symlinks` と `destroyed-symlinks` が削除されていること
- `prek run --all-files` が成功すること
- CI の prek ジョブが成功すること

## 解決方法

- `prek.toml` の builtin フック一覧から `check-symlinks` と `destroyed-symlinks` を削除した
- シンボリックリンクをリポジトリで使わないため、フックを残して `check-hooks-apply` の検出力を落とす判断は取らない
- `prek run --all-files` が成功することを確認した (`Check hooks apply` が Passed になる)
