# README / docs / skill の draft-21 表記を draft-22 に更新する

- Created: 2026-10-02
- Completed: {YYYY-MM-DD}
- Branch: feature/update-draft-22-docs
- Polished: 2026-10-03

## 目的

`refs/moq/draft-ietf-moq-transport-22.txt` を追加したので、利用者向けドキュメントの対応仕様表記と参照リンクを draft-22 に更新する。README や skill が draft-21 のままだと、利用者が古い仕様を前提に API を読むことになる。

## 現状

- `README.md` は対応仕様を `MOQT draft-21` と表記し、参照リンクも draft-ietf-moq-transport-21 を指す。
- `docs/moqt.md` は draft-21 の実装状況として書かれており、リンクも draft-21 である。TRACK_PROPERTY_FILTER の扱いなど一部は draft-21 の仕様を前提にした記述である。
- `skills/shiguredo-moqt/SKILL.md` は description・対応仕様・節参照・参照リンクが draft-21 である。
- `examples/README.md` は「draft-ietf-moq-transport-21、draft-ietf-moq-loc-04、draft-ietf-moq-msf-01、draft-ietf-moq-c4m-01 に準拠」と表記し、§6.1.1 / §6.1.2 / §6.2 / §6.2.1 / §16.2 の節参照を draft 21 として書いている (0195 は利用者向けドキュメントとして本 issue が扱う旨を明記している)。
- 実装は 0191〜0194 で draft-22 に追従するため、本 issue はそれらの完了後に実施する。

## 設計方針

- `README.md` / `docs/moqt.md` / `skills/shiguredo-moqt/SKILL.md` / `examples/README.md` の対応仕様表記・節参照・参照リンクを draft-22 に更新する。
- `examples/README.md` のプロトコル識別子 `moqt-21` (ALPN / `WT-Available-Protocols`) は draft の表記ではなく実装の識別子であり、コード側定数 (`examples/tokio-moq/src/lib.rs` の `MOQT_PROTOCOL`) と 0195 の設計方針に従って維持する。本 issue の対象は付随する節参照と「準拠」表記のみである。
- `docs/moqt.md` の実装状況は、draft-22 の変更点 (LOCATION_FILTER 符号化、Delivery Mode、REQUEST_OK パラメータスコープ、Fetch ギャップ) の反映状況と合わせて更新し、未対応が残る場合は「未対応」節に明記する。
- 節参照は `refs/moq/draft-ietf-moq-transport-22.txt` と突合する。draft-22 で番号が変わる節 (例: RENDEZVOUS TIMEOUT は §9.20.6、Modularity は §1.6、§16.7 (Message Parameters) の表は Table 14) と、番号が変わらないが draft ラベルだけ変わる節 (例: §6.1.1 / §6.1.2 / §6.2 / §6.4.2.2 / §6.4.2.3 / §9 Table 5 / §11.3.1 / §11.4.1 / §11.4.1.1) を区別する。
- `docs/msf.md` / `docs/loc.md` / `docs/c4m.md` に moq-transport の draft 表記は無く、対象外とする。
- 対応仕様の全体 (LOC / MSF / C4M など) の表記は変更しない。
- `src/lib.rs` を含む Rust コード側の表記・節参照は 0195 が扱うため、本 issue の対象外とする。

## 完了条件

- `README.md` / `docs/moqt.md` / `skills/shiguredo-moqt/SKILL.md` / `examples/README.md` から draft-21 の表記とリンクが消え、draft-22 になっていること (プロトコル識別子 `moqt-21` は実装と一致させるため対象外)
- 節参照が一次資料と一致していること (draft ラベルを含む)
- `prek run --all-files` が通ること (markdownlint を含む)

## 解決方法

{未着手}
