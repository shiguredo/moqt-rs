# MSF の公開型の Debug 出力から token / trackBaseKey / authInfo の実値を隠す

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-debug-redaction
- Polished: 2026-10-21

## 目的

MSF の公開型の `Debug` 出力には、bearer token、trackBaseKey、authInfo の生値がそのまま現れる。`{:?}` をログに出すと機密情報が漏れる。C4M 側の `CatToken` / `CoseKey` は秘密値を長さだけ表示する独自 `Debug` を実装しており、MSF 側も同じ方針に揃える。

## 現状

- `src/msf.rs` の `MsfTrack` / `MsfCloneTrack` は `token` / `track_base_key` / `auth_info` を直接持ち、`derive(Debug)` である。`MsfAuthInfo` は `scheme` と `value_raw` を持ち、`value_raw` には scheme 固有の生 JSON を保持し、`derive(Debug)` である。
- `MsfCatalog` / `MsfDeltaUpdate` / `MsfCatalogDocument` / `MsfDeltaOperation` は上記の型を内側に持つ公開コンテナで、`derive(Debug)` の出力には内側の生値が現れる。
  - `MsfCatalog` は `tracks` / `publish_tracks` / `removed_tracks` に `MsfTrack` を持つ。
  - `MsfDeltaUpdate` は `MsfDeltaOperation` を持ち、`MsfDeltaOperation::Add` / `Clone` は `MsfTrack` / `MsfCloneTrack` を持つ。
  - `MsfCatalogDocument` は `MsfCatalog` / `MsfDeltaUpdate` を持つ。
- `MsfCatalog` の構造体 doc は `removed_tracks` が `Debug` に含まれると明記しており、この挙動を変えてはいけない。
- `src/c4m/cat.rs` の `CatToken` と `src/c4m/crypto.rs` の `CoseKey` は、秘密値を長さだけ表示する独自 `Debug` を実装している。
- 影響: example や利用者がカタログやトラックを `{:?}` でログ出力すると、bearer token と鍵材料が漏れる。

## 設計方針

- 実値を直接持つ `MsfTrack` / `MsfCloneTrack` / `MsfAuthInfo` の 3 型に `core::fmt::Debug` を実装する。
  - `MsfTrack` / `MsfCloneTrack` の `token` / `track_base_key` は、`None` なら `None`、`Some` ならバイト長を表示する。
  - `MsfTrack` / `MsfCloneTrack` の `auth_info` は `MsfAuthInfo` の `Debug` をそのまま使う。`MsfAuthInfo` は `scheme` を表示し、`value_raw` はバイト長だけを表示する。
  - 上記以外のフィールドは従来どおり表示する。
- `MsfCatalog` / `MsfDeltaUpdate` / `MsfCatalogDocument` / `MsfDeltaOperation` は `derive(Debug)` のままとする。内側の 3 型の `Debug` を通じて実値が隠れるため、独自 `Debug` は不要である。`MsfCatalog` の `removed_tracks` も `MsfTrack` の `Debug` を通じて実値が隠れ、`Debug` に含まれるという既存の doc の挙動は維持される。
- `Debug` 文字列に token / trackBaseKey / authInfo の実値が含まれないことをテストで固定する。

## 完了条件

- `MsfTrack` / `MsfCloneTrack` / `MsfAuthInfo` と、これらを内側に持つ `MsfCatalog` / `MsfDeltaUpdate` / `MsfCatalogDocument` / `MsfDeltaOperation` の `{:?}` に token / trackBaseKey / authInfo の実値が含まれないこと。
- 非機密フィールドは従来どおり `Debug` に表示され、`MsfCatalog` の `Debug` は `removed_tracks` を含む既存の doc の挙動を維持すること。
- 上記をテストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
