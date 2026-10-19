# SubgroupTracker の状態不変条件を守る

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-subgroup-tracker-state
- Polished: {YYYY-MM-DD}

## 目的

`SubgroupTracker` は Subgroup ストリームの状態を追跡し、終端後の再オープンを禁止する。現状は RESET / STOP_SENDING の記録が正常終了 (FIN) の記録を上書きするため、公開 API 経由で正常終了後の Subgroup を再オープンできてしまう。`mark_fin` の doc が「未知キーは無視」とする一方で内部に記録する点も実装と doc が食い違う。

## 現状

- `src/subgroup_tracker.rs` の `mark_reset` / `mark_stop_sending` は既存の `ClosedFin` を上書きでき、`can_reopen` が「FIN 後は再オープン不可」としている契約に反する。
- Session の受信経路では FIN を先に処理してから RESET / STOP_SENDING を吸収するため到達は狭いが、公開 API の利用者や将来の経路では発生し得る。
- `mark_fin` は doc で「未知キーは無視」としながら `fin_last_object_ids` に挿入する。
- `ClosedFin` の `last_object_id` と `fin_last_object_ids` で同じ情報を二重に保持している。
- 影響: 終端済み Subgroup の再オープンを許し、重複受信や状態不整合を招く。

## 設計方針

- FIN 済みの状態を優先し、RESET / STOP_SENDING では上書きしない。状態を「終端の理由」の和集合として表現する。
- `mark_fin` の doc と実装を一致させ、未知キーの扱いを決める。
- `ClosedFin` の `last_object_id` を削除し、`fin_last_object_ids` に一本化する。
- 状態遷移の単体テストを追加する。

## 完了条件

- FIN 後に RESET / STOP_SENDING を受けても再オープン不可のままであること。
- `mark_fin` の doc と実装が一致していること。
- 状態遷移のテストが `tests/test_subgroup_tracker.rs` に追加されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
