# README / docs / skill の draft-21 表記を draft-22 に更新する

- Created: 2026-10-02
- Completed: {YYYY-MM-DD}
- Branch: feature/update-draft-22-docs
- Polished: {YYYY-MM-DD}

## 目的

`refs/moq/draft-ietf-moq-transport-22.txt` を追加したので、利用者向けドキュメントの対応仕様表記と参照リンクを draft-22 に更新する。README や skill が draft-21 のままだと、利用者が古い仕様を前提に API を読むことになる。

## 現状

- `README.md` は対応仕様を `MOQT draft-21` と表記し、参照リンクも draft-ietf-moq-transport-21 を指す。
- `docs/moqt.md` は draft-21 の実装状況として書かれており、リンクも draft-21 である。TRACK_PROPERTY_FILTER の扱いなど一部は draft-21 の仕様を前提にした記述である。
- `skills/shiguredo-moqt/SKILL.md` は description・対応仕様・節参照・参照リンクが draft-21 である。
- 実装は 0191〜0194 で draft-22 に追従するため、本 issue はそれらの完了後に実施する。

## 設計方針

- `README.md` / `docs/moqt.md` / `skills/shiguredo-moqt/SKILL.md` の対応仕様表記・節参照・参照リンクを draft-22 に更新する。
- `docs/moqt.md` の実装状況は、draft-22 の変更点 (LOCATION_FILTER 符号化、Delivery Mode、REQUEST_OK パラメータスコープ、Fetch ギャップ) の反映状況と合わせて更新し、未対応が残る場合は「未対応」節に明記する。
- 節参照は `refs/moq/draft-ietf-moq-transport-22.txt` と突合する。
- 対応仕様の全体 (LOC / MSF / C4M など) の表記は変更しない。
- `src/lib.rs` を含む Rust コード側の表記・節参照は 0195 が扱うため、本 issue の対象外とする。

## 完了条件

- `README.md` / `docs/moqt.md` / `skills/shiguredo-moq/SKILL.md` から draft-21 の表記とリンクが消え、draft-22 になっていること
- 節参照が一次資料と一致していること
- `prek run --all-files` が通ること (markdownlint を含む)

## 解決方法

{未着手}
