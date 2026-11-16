# MSF_VERSION を draft-02 に更新する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/change-msf-version-draft-02
- Polished: {YYYY-MM-DD}

## 目的

`refs/moq/draft-ietf-moq-msf-02.txt` の §5.1.1 (MSF version) は、IETF Internet-Draft に追随する実装ではバージョンを `"draft-NN"` 形式で名乗ると定める。draft-02 のカタログは `"version": "draft-02"` を持つため、現行実装では v02 のカタログを受理できない。
v02 のカタログを decode できるようにするため、対応バージョンを draft-02 に切り替える。

## 現状

- `src/msf.rs` の `MSF_VERSION` は `"draft-01"`。
- `MsfCatalogDocument::decode` は受信した `version` が `MSF_VERSION` と異なる場合に `MessageError::InvalidCatalogVersion` を返す。このため `"draft-02"` のカタログは、フィールドの妥当性を見る前に一律で拒否される。
- `MsfCatalog::new` は生成するカタログの `version` に `MSF_VERSION` を設定する。encode 側も draft-01 を名乗る。
- `refs/moq/draft-ietf-moq-msf-02.txt` §5.1.1: `"draft-03" refers to the -03 draft release` と例示し、subscriber は理解しない version のカタログを parse してはならない (MUST NOT)。
- `docs/msf.md`、`docs/missing-features.md`、`crates/README.md`、`crates/tokio-moq/README.md` は draft-01 を参照している。

## 設計方針

- `MSF_VERSION` を `"draft-02"` にする。未知バージョンを拒否する挙動は変えず、draft-01 は受理しない。
- 他の v02 対応 issue と独立して先に反映すると、v02 を名乗りながら v02 の MUST を満たさないカタログを受理する期間が生じる。フィールド対応の issue と同じブランチ系列で扱うか、少なくともカタログ受理系の issue の完了後に反映する。この順序を issue 本文とブランチ運用で明示する。
- `MSF_VERSION` を参照するテストと doc を洗い出し、リテラル `"draft-01"` を固定している箇所を draft-02 に更新する。

## 完了条件

- `MSF_VERSION` が `"draft-02"` であること。
- `MsfCatalogDocument::decode` が `"draft-02"` のカタログを受理し、`"draft-01"` と `"draft-03"` を `MessageError::InvalidCatalogVersion` で拒否すること。
- `MsfCatalog::new` から encode したカタログの `version` が `"draft-02"` であること。
- 上記を `tests/test_msf/catalog_decode.rs` と `tests/test_msf/encode_decode_roundtrip.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
