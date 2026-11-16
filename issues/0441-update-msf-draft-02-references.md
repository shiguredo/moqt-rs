# MSF の仕様参照を draft-02 に更新する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02-references
- Polished: {YYYY-MM-DD}

## 目的

`refs/moq/draft-ietf-moq-msf-02.txt` へ参照仕様を更新したため、コード中の仕様参照 (コメント・doc) とドキュメントの記述を v02 に合わせる。
draft-02 は節番号が大規模に振り直されており、現行の `draft-ietf-moq-msf-01 §X` 形式の参照は v02 では別の節を指す。参照が誤ったまま残ると、実装判断の根拠を追えなくなる。

## 現状

- `src/` と `crates/` に `draft-ietf-moq-msf-01` への参照が 86 ファイル・600 箇所超ある。節番号付きの参照は 587 箇所で、その約半数 (290 箇所) は v02 で別の節を指す。
- 主な移動: `§5.2.8` (Target latency) は `§5.2.9`、`§5.2.5` 以降の track 系フィールドは全面的に +1、`§4.3` (暗号化) は `§4.4`、`§11.4` (認可) は `§11.5`、`§8.1` (Event Timeline data format) は `§8.2`。
- `docs/msf.md` は draft-01 へのリンクと節番号を掲載している。`docs/missing-features.md` は MSF の準拠版を draft-01 と記載し、`SUBSCRIBE_DONE` の記述が v02 の `PUBLISH_DONE` と一致していない。
- `docs/moqt.md`、`crates/README.md`、`crates/tokio-moq/README.md` にも draft-01 の節番号付き参照がある。

## 設計方針

- 参照先を `draft-ietf-moq-msf-02` に置き換え、節番号を v02 の対応節に更新する。節番号の対応は v01 と v02 の目次比較で機械的に洗い出し、内容が同じ節だけを置換する。内容自体が変わった節は、新しい要件を説明する文言に直す (機能実装は別 issue)。
- コードの挙動は変えない。コメントと doc の更新に限定する。
- 機能 issue の実装で該当箇所を触るため、本 issue は機能対応の後半に実施する。先行して実施すると、機能 issue の変更と広範囲にコンフリクトする。
- `docs/missing-features.md` の `SUBSCRIBE_DONE` の記述を `PUBLISH_DONE` に直し、未対応機能の一覧を v02 の節番号で整理する。

## 完了条件

- `rg 'draft-ietf-moq-msf-01' src crates docs` が 0 件であること。
- コード中の MSF 節番号付き参照が、v02 の目次と一致する節を指していること。少なくとも `§5.2.8`→`§5.2.9`、`§4.3`→`§4.4`、`§11.4`→`§11.5`、`§8.1`→`§8.2` の移動が反映されていること。
- `docs/msf.md` の準拠版リンクと実装状況の節番号、`docs/missing-features.md` の準拠版と未対応一覧、crate README の準拠版が draft-02 になっていること。
- コードの挙動が変わっていないこと (diff がコメント・doc・Markdown のみ)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
