# 長寿命購読で無制限に増える追跡状態を整理する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-unbounded-state-growth
- Polished: {YYYY-MM-DD}

## 目的

`RequestIdTracker` の `above`、`SubgroupTracker` の per-(alias, group, subgroup) 状態、`FetchStreamDecoder` の per-group 状態は、長寿命の購読で無制限に増える。購読終了や Group 完了で不要になった状態を解放し、メモリ使用量を有界にする。

## 現状

- `src/request_id.rs` の `RequestIdTracker::above` は request 終了後も値が残り、単調増加する request_id で上限判定が変わらない。
- `src/subgroup_tracker.rs` は Group 完了後も per-group 状態を保持する。
- `src/stream/decoder.rs` の `FetchStreamDecoder` は Group ごとの `last_object_ids` を保持し、購読終了まで解放しない。
- 影響: 長時間の配信で状態が増え続ける。

## 設計方針

- 購読終了・Group 完了・stream 終端で不要な状態を削除する。
- 追跡状態の件数上限を設けるか、完了 Group を prune する。
- 状態が有界であることをテストで固定する。

## 完了条件

- 購読終了後に関連状態が解放されること。
- 長時間購読を模したテストで状態数が有界であること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
