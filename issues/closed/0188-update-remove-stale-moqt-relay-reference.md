# CHANGES.md が存在しない docs/MOQT-RELAY.md を参照するエントリを削除する

- Created: 2026-10-01
- Completed: 2026-10-06
- Branch: feature/update-remove-stale-moqt-relay-reference
- Polished: 2026-10-01

## 目的

CHANGES.md の develop セクションに、リポジトリに存在しない `docs/MOQT-RELAY.md` を参照する [UPDATE] エントリが残っている。このエントリが主張するドキュメント変更はどこにも反映されておらず、変更履歴を読んだ利用者が存在しないファイルを探すことになる。当該エントリを削除して変更履歴を事実と一致させる。

## 現状

- `CHANGES.md` の `## develop` の `### misc` に次の [UPDATE] エントリがある
  - エントリ行: 「catalog (MSF) と LOC の relay での扱いを docs に明記する」
  - サブバレット: 「relay は payload と Properties を opaque として扱い、`moqt_msf` / `moqt_loc` を production 経路に配線しない理由を `docs/MOQT-RELAY.md` に記載する」
- `docs/MOQT-RELAY.md` はリポジトリに存在しない (`docs/` は c4m.md / loc.md / moqt.md / msf.md のみ)。`git log --all -- docs/MOQT-RELAY.md` も空であり、どのブランチにも一度もコミットされていない
- 上記の理由 (payload と Properties の opaque 扱い、`moqt_msf` / `moqt_loc` の production 経路への
  非配線) は `docs/moqt.md` の「未対応」節にも、`docs/msf.md` / `docs/loc.md` にも記述がない。
  「未対応」節が述べるのは relay 専用メッセージ (`PUBLISH_NAMESPACE` / `SUBSCRIBE_NAMESPACE` /
  `SUBSCRIBE_TRACKS` / `NAMESPACE` / `NAMESPACE_DONE` / `PUBLISH_SKIPPED`) と
  `RENDEZVOUS_TIMEOUT` の扱いのみである
- `moqt_msf` / `moqt_loc` という識別子はリポジトリのどこにも存在しない (CHANGES.md の当該サブバレット以外に出現しない)
- 本ライブラリ (moqt-rs) は Sans I/O のクライアント / サーバー向けライブラリであり relay の実装を含まない (README の「I/O、非同期処理、relay が担う namespace 発見・告知と forwarding は含まない」と `docs/moqt.md` の「アーキテクチャ」節)。relay の production 経路についての文書化は本リポジトリの対象外である
- `## develop` セクションはまだリリースされておらず (CHANGES.md に `## develop` 以外の見出しが無い)、公開前に直せる

## 設計方針

- 当該 [UPDATE] エントリ (エントリ行・サブバレット・担当者行) を削除する。エントリが主張する「docs に明記する」という変更はどこにも反映されておらず、実装されていない変更を履歴に残せないため
- 参照先を `docs/moqt.md` や `CODEBASE.md` へ置き換えることはしない。「未対応」節と CODEBASE.md の規約は relay の対象外を述べるだけで、catalog / LOC を opaque として扱う理由を含まないため、置き換えると存在しない記述を参照する虚偽のエントリになる
- 新規ドキュメント (`docs/MOQT-RELAY.md`) は作らない。relay の production 経路の設計判断は本リポジトリの対象外であり、作るべき文書化対象が存在しない
- リリース済みセクションは変更しない (現状 `## develop` のみで該当なし)
- 他のエントリの文言・担当者行・順序は変更しない。`### misc` の並べ替えは `issues/0190-update-changes-develop-order.md` の対象である

## 完了条件

- `CHANGES.md` から当該 [UPDATE] エントリ 3 行 (エントリ行・サブバレット・`@voluntas` 行) が削除され、`docs/MOQT-RELAY.md` への参照が残っていないこと
- 他のエントリに変更が無いこと
- `prek run --all-files` が通ること (markdownlint を含む)

## 解決方法

`CHANGES.md` の `## develop` の `### misc` から、存在しない `docs/MOQT-RELAY.md` を参照する
[UPDATE] エントリ 3 行 (エントリ行・サブバレット・`@voluntas` 行) を削除した。

- 削除したのは「catalog (MSF) と LOC の relay での扱いを docs に明記する」のエントリである。
  主張するドキュメント変更は `docs/` のどこにも反映されておらず、参照先も存在しないため、
  変更履歴を事実と一致させる
- 参照先の置き換えと新規ドキュメントの作成は行っていない。設計方針のとおり、relay の production 経路の
  文書化は本リポジトリ (Sans I/O のクライアント / サーバー向けライブラリ) の対象外である
- 他のエントリの文言・担当者行・順序は変更していない (diff は 3 行の削除のみ)。
  `### misc` の並べ替えは別 issue の対象である
- `prek run --files CHANGES.md` (markdownlint を含む) が通ることを確認した
