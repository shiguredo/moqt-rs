# CHANGES.md が存在しない docs/MOQT-RELAY.md を参照しているのを直す

- Created: 2026-10-01
- Completed: {YYYY-MM-DD}
- Branch: feature/update-remove-stale-moqt-relay-reference
- Polished: {YYYY-MM-DD}

## 目的

CHANGES.md のエントリがリポジトリに存在しないドキュメントを参照しており、変更履歴を読んだ利用者が存在しないファイルを探すことになる。参照先を実在する記述へ直すか、記述が現状と合わない場合はエントリごと外す。

## 現状

- `CHANGES.md` の `## develop` の [UPDATE] エントリが「relay は payload と Properties を opaque として扱い、`moqt_msf` / `moqt_loc` を production 経路に配線しない理由を `docs/MOQT-RELAY.md` に記載する」と書いている
- `docs/MOQT-RELAY.md` はリポジトリに存在せず、`git log -- docs/MOQT-RELAY.md` も空である (一度もコミットされていない)
- relay 非対応の規約は `CODEBASE.md` の「クライアントとサーバーのみでリレーには対応しないこと」と、`docs/moqt.md` の「未対応」節にある
- この develop セクションはまだリリースされておらず、公開前に直せる

## 設計方針

- 参照先を実在する記述 (`docs/moqt.md` の「未対応」節、`CODEBASE.md` の規約) に置き換える。新規ドキュメントは作らない
- relay 関連機能の削除によりエントリの記述自体が現状と合わない場合は、参照だけを直さずエントリごと削除する
- リリース済みセクションは変更しない

## 完了条件

- `CHANGES.md` から存在しない `docs/MOQT-RELAY.md` への参照が消えていること
- 参照を残す場合は、置き換え先が実在するドキュメント (`docs/moqt.md` / `CODEBASE.md`) であること
- `prek run --all-files` が通ること (markdownlint を含む)

## 解決方法

{未着手}
