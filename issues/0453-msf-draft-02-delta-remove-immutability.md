# MSF delta update の track 属性不変検証を撤廃する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.3 (Delta updates) は、v01 の「Track Namespace と Track Name の組は固定の属性集合を定義し、宣言後に変更してはならない (MUST NOT)」を削除した。v02 は `update` 操作で属性を変更できる。

現行実装は v01 の MUST に従って再追加時の属性変更を拒否しており、v02 のカタログ更新を適用できない。一方で §5.2.8 (Is Live) の「False の後に True を置いてはならない」は維持する必要がある。

## 現状

- `MsfCatalog.removed_tracks` が削除済み track の履歴を保持し、`MsfCatalog::validate_readded_track` が remove 後に同じ full name を add / clone で再追加したときの属性変更を `MessageError::InvalidCatalog` で拒否する。属性の比較は `comparable_attributes` で行う。
- `MsfCatalog::apply_delta_in_place` は削除時に `removed_tracks.entry(key).or_insert(removed)` を使う。既存キーの値は更新されないため、isLive=true の track を remove → isLive=false で再追加 → remove → isLive=true で再追加すると、履歴は最初の true を保持し、false を経たことを検出できない。
- `tests/test_msf/delta_apply.rs` の `apply_delta_readd_by_clone_with_changed_attribute_rejected` などが旧 MUST を固定している。
- `docs/msf.md` と `MsfCatalog::apply_delta` / `removed_tracks` の doc が「属性不変」を前提にしている。
- 0344 は `apply_delta` 内の undo ログに `removed_tracks` の新規挿入キーを記録する設計である。挿入規則を `or_insert` から `insert` に変えると、上書き前の値を undo ログに記録しないと rollback できない。

## 設計方針

- 再追加時の属性変更を拒否する検証を削除する。`comparable_attributes` と `validate_readded_track` の属性比較を廃止する。
- `removed_tracks` の用途を「削除時点の isLive を保持する履歴」に限定する。削除のたびに最新値で更新する (`insert` にする)。再追加・update 時に、保持している isLive が false で新しい isLive が true なら §5.2.8 の MUST に従って拒否する。
- 同じ full name の重複追加は引き続き illogical operation として拒否する。
- 0344 の undo ログは、`insert` による上書き前の値も記録する必要がある。0344 の完了条件と設計を更新するか、0344 を先行させる。この依存を issue に明記する。
- `docs/msf.md` と `MsfCatalog::apply_delta` / `removed_tracks` の doc を、属性不変の撤廃と isLive 遷移の保持に合わせて更新する。

## 完了条件

- remove 後に同じ full name を add / clone で再追加し、属性を変更した delta update が適用できること。
- remove 後に `isLive` を false から true に変えて再追加した delta update が拒否されること。
- isLive=true → remove → isLive=false で再追加 → remove → isLive=true で再追加、の順で拒否されること。
- 既存 track と同じ full name の add が引き続き拒否されること。
- `removed_tracks` が削除時点の最新の isLive を保持することがテストで観測できること。
- `docs/msf.md` と `apply_delta` / `removed_tracks` の doc が更新されていること。
- 0344 の undo ログが上書き前の値を記録する前提であることが issue に明記されていること。
- 上記を `tests/test_msf/delta_apply.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
