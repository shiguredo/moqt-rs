# MSF_VERSION を draft-02 に更新する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: 2026-11-16

## 目的

`refs/moq/draft-ietf-moq-msf-02.txt` の §5.1.1 (MSF version) は、IETF Internet-Draft に追随する実装ではバージョンを `"draft-NN"` 形式で名乗ると定める。draft-02 のカタログ例は `"version": "draft-02"` を使う (Appendix A.1.1)。
現行実装の `MSF_VERSION` は `"draft-01"` で、完全カタログは version 不一致により `MessageError::InvalidCatalogVersion` で拒否される。v02 のカタログを受理できるよう、対応バージョンを draft-02 に切り替える。

## 現状

- `src/msf.rs` の `MSF_VERSION` は `"draft-01"`。
- `MsfCatalogDocument::decode` は `deltaUpdate` の有無を先に判定し、delta update は version を読まずに `decode_delta` へ分岐する。version の一致を要求するのは完全カタログだけである。
- `MsfCatalog::new` と `crates/moq-pub/src/catalog.rs` の `build_catalog` は `MSF_VERSION` を version に設定する。encode 側も draft-01 を名乗る。
- §5.1.1 は `"draft-03" refers to the -03 draft release` と例示し、subscriber は理解しない version のカタログを parse してはならない (MUST NOT) と定める。
- `draft-01` の表記が 184 行ある。内訳は `tests/test_msf` の 6 ファイル (catalog_decode 11 / encode_decode_roundtrip 34 / error_cases 86 / template 10 / track_fields 18 / variable_substitution 16)、`crates/moq-sub/src` に 5、`src/msf.rs` に 2、`README.md` に 2 である。
- 多くはカタログ JSON の version で、encode_decode_roundtrip の 4 行はバージョン変換を説明するコメント、README.md の 1 行は対応仕様のラベルである。`MSF_VERSION` を変えると、これらを持つ完全カタログの decode が失敗し、`InvalidCatalog` を期待するテストも variant 不一致で落ちる。
- `README.md` と `skills/shiguredo-moqt/SKILL.md` の MSF の版表記と仕様参照 URL も draft-02 に更新する。
- `draft-ietf-moq-msf-01` 形式の仕様参照 URL・節番号のうち、`src` / `crates` / `docs` の更新は別 issue (0441) の担当とする。`README.md` と `skills/shiguredo-moqt/SKILL.md` の MSF の版表記と URL は本 issue で更新する。

## 設計方針

- `MSF_VERSION` を `"draft-02"` にする。未知バージョンを拒否する挙動は変えず、draft-01 は受理しない。
- delta update は version を持たないため version ゲートの対象外であることを、コメントとテストで明示する。version の切り替えで delta 経路の挙動を変えない。
- カタログ JSON の version リテラル `"draft-01"` を、コード・テスト・コード例のすべてで `"draft-02"` に更新する。対象は上記 6 テストファイル、`crates/moq-sub/src/catalog.rs`、`crates/moq-sub/src/pipeline.rs`、`src/msf.rs` のモジュール doc、`README.md` の decode 例である。
- `"draft-01"` とエスケープ表記 `\"draft-01\"` の両方を対象とし、`MSF_VERSION` を参照しているかどうかではなくリテラルで検索して洗い出す。バージョン変換を説明するコメントも draft-02 に合わせる。
- `README.md` と `skills/shiguredo-moqt/SKILL.md` の MSF の版表記と仕様参照 URL も draft-02 に更新する。
- 実施順序: 0437 は 0438〜0461 の完了後に最後に実施する。v02 の MUST 対応が入る前に version だけを上げると、v02 を名乗りながら v02 の MUST を満たさないカタログを受理する期間が生じる。publisher 側 (`crates/moq-pub`) も同じ理由で最後に切り替える。

## 完了条件

- `MSF_VERSION` が `"draft-02"` であること。
- 完全カタログについて、`"draft-02"` を受理し、`"draft-01"` と `"draft-03"` を `MessageError::InvalidCatalogVersion` で拒否すること。
- version を持たない delta update が version ゲートで拒否されないこと (既存挙動の維持)。
- `MsfCatalog::new` と `crates/moq-pub/src/catalog.rs` の `build_catalog` が version に `"draft-02"` を設定すること。
- `rg 'draft-01' src/msf.rs tests/test_msf crates/moq-sub/src/catalog.rs crates/moq-sub/src/pipeline.rs README.md` の結果が、draft-01 の拒否を確認するテストだけになること。
- `README.md` と `skills/shiguredo-moqt/SKILL.md` の MSF の版表記と URL が draft-02 になっていること (C4M の draft-01 に関する記述は変更しない)。
- 上記を `tests/test_msf` の 6 ファイルで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
