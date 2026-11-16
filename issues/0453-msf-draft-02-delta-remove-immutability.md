# MSF delta update の track 属性不変検証を撤廃する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.3 (Delta updates) は、v01 の「Track Namespace と Track Name の組は固定の属性集合を定義し、宣言後に変更してはならない (MUST NOT)」を削除した。v02 は `update` 操作で属性を変更できる。
現行実装は v01 の MUST に従って再追加時の属性変更を拒否しており、v02 のカタログ更新を適用できない。

## 現状

- `MsfCatalog.removed_tracks` が削除済み track の履歴を保持し、`MsfCatalog::validate_readded_track` が remove 後に同じ full name を add / clone で再追加したときの属性変更を `MessageError::InvalidCatalog` で拒否する。
- 属性の比較は `comparable_attributes` で行っている。
- `tests/test_msf/delta_apply.rs` の `apply_delta_readd_by_clone_with_changed_attribute_rejected` などが旧 MUST を固定している。
- v02 §5.2.8 (Is Live) は「A True value MUST NOT follow a False value in a catalog update」を維持しており、この制約は残す必要がある。
- `issues/0344-perf-msf-apply-delta-search.md` は `removed_tracks` を undo 用の履歴として使う設計を扱っている。履歴の用途を整理しないと、検証撤廃と探索最適化が衝突する。

## 設計方針

- 再追加時の属性変更を拒否する検証を削除する。`comparable_attributes` と `validate_readded_track` の属性比較を廃止し、`removed_tracks` は削除済み track の再追加を検出する用途に限定する。
- §5.2.8 の `isLive` の false から true への変更禁止は維持し、add / clone / update のいずれの経路でも検証する。
- 同じ full name の重複追加は引き続き illogical operation として拒否する。
- `issues/0344-perf-msf-apply-delta-search.md` の設計と衝突しないよう、履歴の用途をこの issue の完了条件に含めて明記する。

## 完了条件

- remove 後に同じ full name を add / clone で再追加し、属性を変更した delta update が適用できること。
- remove 後に `isLive` を false から true に変えて再追加した delta update が拒否されること。
- 既存 track と同じ full name の add が引き続き拒否されること。
- `removed_tracks` の用途が属性比較ではなく再追加検出に限定され、doc に明記されていること。
- 上記を `tests/test_msf/delta_apply.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
