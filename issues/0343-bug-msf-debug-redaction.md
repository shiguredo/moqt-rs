# MSF の公開型の Debug 出力から token / trackBaseKey / authInfo の実値を隠す

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-debug-redaction
- Polished: {YYYY-MM-DD}

## 目的

`MsfTrack` / `MsfCloneTrack` / `MsfCatalog` / `MsfCatalogDocument` / `MsfDeltaUpdate` / `MsfAuthInfo` は `derive(Debug)` のまま token、trackBaseKey、auth_info の生値を含む。`{:?}` をログに出すと bearer token と鍵材料が漏れる。C4M 側は長さだけを表示する独自 Debug を実装しており、方針が不整合である。

## 現状

- `src/msf.rs` の `MsfTrack` / `MsfCloneTrack` は `token` と `track_base_key` を持ち、`derive(Debug)` である。
- `MsfAuthInfo` は認証情報をそのまま `Debug` に出す。
- `src/c4m/cat.rs` の `CatToken` と `src/c4m/crypto.rs` の `CoseKey` は長さだけを出す独自 `Debug` を実装している。
- 影響: example や利用者がカタログを `{:?}` でログ出力すると機密情報が漏れる。

## 設計方針

- MSF の該当型に独自 `Debug` を実装し、`token` / `track_base_key` / `auth_info` は長さだけを表示する。
- 他のフィールドは従来どおり表示する。
- `Debug` 文字列に実値が含まれないことをテストで固定する。

## 完了条件

- 該当型の `{:?}` に token / trackBaseKey / authInfo の実値が含まれないこと。
- 上記をテストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
