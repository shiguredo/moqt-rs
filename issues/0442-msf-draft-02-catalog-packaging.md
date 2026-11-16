# MSF カタログの packaging 値に catalog を追加する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.2.4 (Packaging) は packaging の許容値に `"catalog"` を追加した。カタログトラックが別のカタログトラックを参照できるようになり、トップレベルのカタログトラックだけが `"catalog"` という名前を持つ必要がある。
現行実装は `"catalog"` を未知の packaging として拒否するため、v02 のカタログを受理できない。

## 現状

- `src/msf.rs` の `MsfPackaging` は `Loc` / `MediaTimeline` / `EventTimeline` / `MoqLog` / `MoqMetrics` のみを持つ。
- `MsfPackaging::from_packaging_str` は `"catalog"` を `MessageError::InvalidCatalog` にする。`MsfPackaging::as_str` も `"catalog"` を返さない。
- `MSF_CATALOG_TRACK_NAME` は `b"catalog"` で、トップレベルのカタログトラック名の検証にだけ使われている。
- `tests/test_msf/catalog_decode.rs` の `unknown_packaging_rejected` が許容値の一覧を固定している。
- v02 §5.2.4 は許容値に `catalog` を加え、A.1.3 は「only the top-level catalog track MUST be named "catalog"」と述べる。

## 設計方針

- `MsfPackaging` に `Catalog` を追加し、`from_packaging_str` / `as_str` の両方向で `"catalog"` を扱う。
- `catalog` packaging の track に media 向けの必須フィールド (codec など) を要求しない。必須フィールドの規則は §5.2.4.1 の表に従うため、この issue では `catalog` を表に加えるだけにし、`(packaging, role)` ごとの検証は別 issue で扱う。
- トップレベルのカタログトラック名の検証 (`MSF_CATALOG_TRACK_NAME`) は現状のまま維持する。参照先カタログのトラック名は `"catalog"` に限定しない。
- ネストしたカタログの購読・再帰取得は本 issue の範囲外とする。カタログを参照する track を受理できる状態までを対象にする。

## 完了条件

- `"packaging": "catalog"` の track を含むカタログが decode / encode で往復一致すること。
- `MsfPackaging::as_str` が `Catalog` に対して `"catalog"` を返すこと。
- 既存の許容値 (`loc` / `mediatimeline` / `eventtimeline` / `moqlog` / `moqmetrics`) の挙動が変わらないこと。
- 未知の packaging 値が引き続き `MessageError::InvalidCatalog` で拒否されること。
- 上記を `tests/test_msf/catalog_decode.rs` と `tests/test_msf/encode_decode_roundtrip.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
