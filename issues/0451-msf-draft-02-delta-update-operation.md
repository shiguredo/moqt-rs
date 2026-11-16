# MSF delta update に update 操作を追加する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.1.6 (Delta update) は、既存 track のフィールドを変更する `"update"` 操作を追加した。update の track object は `parentName` を MUST、`parentNamespace` を MAY で持ち、宣言した属性が既存値を上書きする。
現行実装は `add` / `remove` / `clone` だけを扱い、`"update"` を未知の操作として拒否する。v02 のカタログ更新を適用できない。

## 現状

- `src/msf.rs` の `MsfDeltaOperation` は `Add` / `Remove` / `Clone` のみである。
- `decode_delta_operation` は `"update"` を `MessageError::InvalidCatalog` にする。
- `MsfCatalog::apply_delta_in_place` は add の既存名、remove の未知 track、clone の未知の親・既存名を拒否する。update の適用経路はない。
- update 操作の型が存在しないため、`parentName` を clone 以外で使う経路もない。`validate_full_track` は full track の `parentName` を拒否し、`MsfCloneTrack.parent_name` は必須である。
- 仕様の矛盾: §5.1.6 は update の track object に `parentName` を MUST とするが、§5.2.3 はすべての track object に `name` を MUST とし、§5.6.4 の例は update で `name` を省略している。

## 設計方針

- update 用の型 (`MsfUpdateTrack` など) を追加し、`parentName` を必須、`parentNamespace` を任意、変更対象のフィールドを任意とする。全フィールドを必須とする `MsfTrack` を再利用しない。
- 仕様の矛盾は勝手に決めず、実装では「update の track object は `name` を任意とし、`parentName` で対象を特定する」形にする。`name` が付いていて `parentName` と一致しない場合は `MessageError::InvalidCatalog` にする。この判断は issue と doc に残す。
- `MsfDeltaOperation` に `Update` を追加し、`decode_delta_operation` / `DisplayJson` の両方向で `"update"` を扱う。
- 適用時の検証を追加する: 対象 track が存在しない場合は illogical operation として拒否する。`isLive` の false から true への変更は §5.2.8 の MUST に従って拒否する。
- track 属性の不変検証の撤廃は別 issue で扱う。本 issue は update 操作の追加と適用までを対象にする。

## 完了条件

- `"update"` を含む delta update が decode / encode で往復一致すること。
- 存在する track の属性を update で変更でき、変更後のカタログが期待どおりになること。
- 存在しない `parentName` を指す update が `MessageError::InvalidCatalog` で拒否されること。
- `isLive` を false から true に変える update が拒否されること。
- `name` と `parentName` が矛盾する update の扱いがテストで固定されていること。
- 上記を `tests/test_msf/delta_apply.rs` と `tests/test_msf/catalog_decode.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
