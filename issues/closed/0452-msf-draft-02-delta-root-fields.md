# MSF delta update のルートフィールド制約を v02 に合わせる

- Created: 2026-11-16
- Completed: 2026-11-17
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.3 (Delta updates) は、delta update に `deltaUpdate` と `generatedAt` を MUST include とし、それ以外の Root Catalog フィールドを MUST NOT include と定めた。v01 は `version` と `tracks` だけを禁止していた。

現行実装は `generatedAt` を任意とし、`version` と `tracks` だけを拒否するため、v02 が禁じる `isComplete` / `publishTracks` / `initDataList` を含む delta update を受理してしまう。

## 現状

- `src/msf.rs` の `MsfDeltaUpdate.generated_at: Option<u64>` は `None` を許す。
- `decode_delta` は `version` と `tracks` を持つ delta update を拒否するが、`isComplete` / `publishTracks` / `initDataList` は検査しない。
- `validate_delta_for_encode` は operations が非空であることに加え、add の `validate_full_track` と clone の `validate_clone_track_fragment` を検証する。ルートフィールドの制約は見ない。
- `MsfDeltaUpdate` は `generated_at` と `operations` しか持たない。`generated_at` を `u64` にすると、他の Root Catalog フィールドは型として表現できなくなる。
- `generated_at` を `u64` にすると、`MsfDeltaUpdate { generated_at: None }` を構築しているテスト 69 箇所 (5 ファイル) と、`generatedAt` なしの delta JSON fixture 31 箇所 (5 ファイル) が影響を受ける。
- `tests/test_msf/catalog_decode.rs` の `delta_minimal` は `generatedAt` なしの delta update を受理することを固定している。

## 設計方針

- `MsfDeltaUpdate.generated_at` を `u64` に変更し、`generatedAt` を必須にする。欠如は `MessageError::InvalidCatalog` で拒否する。
- decode で `version` / `isComplete` / `tracks` / `publishTracks` / `initDataList` を拒否する。未知のフィールドは無視する。
- encode 側は、`u64` 化で `generatedAt` の出力が型で保証され、他の Root Catalog フィールドは `MsfDeltaUpdate` に存在しないため、ルートフィールドの追加検証は行わない。この判断を doc に明記する。
- `delta_minimal` は `generatedAt` 付きの delta update を受理するテストに更新し、`generatedAt` 欠如の拒否は別テストに分ける。
- 既存の delta JSON fixture に `generatedAt` を追加し、`remove_tracks_extra_field_rejected` などの拒否テストが `generatedAt` 欠如で先に失敗しないようにする。delta の encode 往復は `tests/test_msf/encode_decode_roundtrip.rs` に置く。
- 公開型のフィールド型変更を伴う破壊的変更として doc に明記する。

## 完了条件

- `generatedAt` を持たない delta update が `MessageError::InvalidCatalog` で拒否されること。
- `isComplete` / `publishTracks` / `initDataList` のいずれかを持つ delta update が拒否されること。
- 未知の custom フィールドを持つ delta update が受理されること。
- `generatedAt` を必須にした encode が往復一致すること。
- `tests/test_msf` の delta fixture と `MsfDeltaUpdate` の構築がすべて更新され、既存の拒否テストが本来の検証意図を保ったまま通ること。
- `delta_minimal` が `generatedAt` 付きの受理を固定し、欠如拒否が別テストで固定されていること。
- 上記を `tests/test_msf/catalog_decode.rs`、`tests/test_msf/delta_apply.rs`、`tests/test_msf/encode_decode_roundtrip.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `MsfDeltaUpdate.generated_at` を `Option<u64>` から `u64` に変更し、decode で `generatedAt` を必須にした。公開型のフィールド型変更を伴う破壊的変更である。
- delta update の decode で `version` / `isComplete` / `tracks` / `publishTracks` / `initDataList` を拒否し、未知のルートフィールドは無視するようにした。encode は `u64` 化で `generatedAt` の出力が型で保証され、他のルートフィールドは型に存在しないため追加検証を行わない。
- `delta_minimal` を `generatedAt` 付きの受理に更新し、`generatedAt` 欠如、禁止ルートフィールド、未知フィールド受理のテストを追加した。
- `tests/test_msf` の delta JSON fixture と `MsfDeltaUpdate` 構築、`pbt/tests/prop_msf.rs` / `prop_msf_delta.rs`、`crates/moq-sub` の delta fixture を `generatedAt` 必須に追従させた。既存の拒否テストは本来の検証意図を保つよう `generatedAt` を補った。
- `docs/msf.md` に delta のルートフィールド制約を追記した。

確認コマンド: `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` が通ることを確認した。
