# MSF delta update のルートフィールド制約を v02 に合わせる

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.3 (Delta updates) は、delta update に `deltaUpdate` と `generatedAt` を MUST include とし、それ以外の Root Catalog フィールドを MUST NOT include と定めた。v01 は `version` と `tracks` だけを禁止していた。
現行実装は `generatedAt` を任意とし、`version` と `tracks` だけを拒否するため、v02 が禁じる `isComplete` / `publishTracks` / `initDataList` を含む delta update を受理してしまう。

## 現状

- `src/msf.rs` の `MsfDeltaUpdate.generated_at: Option<u64>` は `None` を許す。
- `decode_delta` は `version` と `tracks` を持つ delta update を拒否するが、`isComplete` / `publishTracks` / `initDataList` は検査しない。
- `validate_delta_for_encode` は operations が非空であることだけを見る。
- `tests/test_msf/catalog_decode.rs` の `delta_minimal` は `generatedAt` なしの delta update を受理することを固定している。
- v02 §5.3 は `generatedAt` を必須とし、他の Root Catalog フィールドを禁止する。未知の custom フィールドは §5 の未知フィールドの扱いに従う。

## 設計方針

- `MsfDeltaUpdate.generated_at` を `u64` に変更し、`generatedAt` を必須にする。欠如は `MessageError::InvalidCatalog` で拒否する。
- delta update で `version` / `isComplete` / `tracks` / `publishTracks` / `initDataList` を拒否する。未知のフィールドは無視する。
- `validate_delta_for_encode` に同じ制約を追加し、encode 側でも不正な delta update を作れないようにする。
- 公開型のフィールド型変更を伴う破壊的変更として doc に明記する。

## 完了条件

- `generatedAt` を持たない delta update が `MessageError::InvalidCatalog` で拒否されること。
- `isComplete` / `publishTracks` / `initDataList` のいずれかを持つ delta update が拒否されること。
- 未知の custom フィールドを持つ delta update が受理されること。
- `generatedAt` を必須にした encode が往復一致すること。
- 上記を `tests/test_msf/catalog_decode.rs` と `tests/test_msf/delta_apply.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
