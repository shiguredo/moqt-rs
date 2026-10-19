# MSF の delta 適用の探索と複製を減らす

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-msf-apply-delta-search
- Polished: {YYYY-MM-DD}

## 目的

`apply_delta` は Add の重複判定に `Vec::contains` と `find_track_index` の線形探索を使い、Remove / Clone も毎回カタログ全体を線形探索する。さらに毎回カタログ全体を clone する COW になっている。巨大なカタログや delta の連続適用で CPU とメモリを過剰に消費する。

## 現状

- `src/msf.rs` の `apply_delta` は `batch_keys: Vec<(TrackNamespace, TrackName)>` の `contains` と `find_track_index` で重複を調べる。
- Remove / Clone も同じ線形探索である。
- `apply_delta` は呼び出しごとにカタログ全体を clone する。
- 影響: delta の件数 N と track 数 M で O(N*M)。delta を連続適用するとメモリ確保も増える。

## 設計方針

- `apply_delta` 中に解決済み `(namespace, name)` の `HashSet` / `HashMap` を維持し、重複判定と検索を定数時間にする。
- カタログの clone を減らせるか、`MsfCatalogDocument` の所有権設計を見直す。
- 巨大入力の回帰テストまたはベンチマークを追加する。

## 完了条件

- 探索が線形探索でなくなっていること。
- 既存の delta 適用テストが変わらず通ること。
- `cargo test --workspace` / `cargo test -p pbt` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
