# MsfCatalog::apply_delta を操作をまたいで原子的にする

- Created: 2026-09-10
- Completed: 2026-09-17
- Branch: feature/update-apply-delta-atomic
- Polished: {YYYY-MM-DD}

## 目的

`MsfCatalog::apply_delta` の失敗時にカタログを不変にし、呼び出し側が「`Err` ならカタログは変化していない」と期待できるようにする。

## 現状

`src/msf.rs` の `apply_delta` は `&mut self` を操作ごとに逐次書き換え、途中で失敗すると先行操作の適用結果が残る。0032 で doc に非原子性を明記したが、`Err` 時にカタログが部分的に変更されたまま残る。

例: `operations: [Add(有効), Remove(不存在)]` は `Err` を返しつつ先頭の add が `tracks` に残る。

## 設計方針

次のいずれかで原子性を確保する。

- `MsfCatalog` の複製に対して delta を適用し、全操作成功時のみ `self` に差し替える (copy-on-write)
- 全操作の検証 (per-track 検証・重複・remove 対象存在・`validate_after_delta`) を先に済ませてから適用する

成功時の適用結果と `generated_at` の更新規則は現状と同じにする。

## 完了条件

- `apply_delta` が `Err` を返したときカタログが呼び出し前と一致すること
- 成功時の適用結果 (tracks / publishTracks / initDataList / generatedAt) が現状と一致すること
- 回帰テストが `tests/test_msf/delta_apply.rs` に追加されていること

## 解決方法

`src/msf.rs` の `apply_delta` を copy-on-write にした。

- 既存の本体を `apply_delta_in_place` (private) に切り出し、`apply_delta` は `self` の複製へ
  適用してから成功時のみ `*self = working` で差し替える
- 適用後検証 (`validate_after_delta`) も複製に対して行われるため、`Err` を返したときに
  カタログは呼び出し前と完全に一致する
- 成功時の適用結果 (tracks / publish_tracks / init_data_list / generated_at) は従来と同じ
- doc の「途中で失敗した先行操作の結果はロールバックされない」という記述を、
  失敗時にカタログを変更しない旨に更新した

回帰テストを `tests/test_msf/delta_apply.rs` に 2 本追加した。

- `apply_delta_is_atomic_when_later_operation_fails`:
  `[Add(有効), Remove(不存在)]` が `Err` を返し、カタログが呼び出し前と一致すること。
  修正前 (複製を使わない実装) では失敗することを確認済み
- `apply_delta_applies_all_operations_on_success`: 複数の操作が成功した場合に
  tracks と generatedAt が従来どおり反映されること

検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo fmt --all -- --check` の通過で確認した。
