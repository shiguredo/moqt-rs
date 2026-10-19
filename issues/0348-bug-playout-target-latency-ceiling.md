# PlayoutTimeline::with_config でも targetLatency を閉ループの上限にする

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-playout-target-latency-ceiling
- Polished: {YYYY-MM-DD}

## 目的

`PlayoutTimeline::set_target_latency_ms` は、明示された targetLatency を音声の閉ループ遅延の上限として `AudioDelayFeedback::set_ceiling_us` に設定する。同じ値を `TimelineConfig::target_latency_ms` で構築時に渡した場合、`with_config` は値を保持するだけで上限を設定しない。構築経路によって挙動が変わるのは API 契約として不正である。

## 現状

- `src/playout/timeline.rs` の `with_config` は `config.target_latency_ms` を最大 0 に丸めて `self.config` に入れ、`AudioDelayFeedback::new()` をそのまま使う。
- `set_target_latency_ms` は同じ値から `set_ceiling_us` を呼び、明示値より閉ループが大きくならないようにする。
- `with_config(TimelineConfig { target_latency_ms: 90, .. })` では閉ループが既定の上限 (最大 160 ms) まで育ち得る。
- テストは setter 経路しか固定していない。
- 影響: 構築時に targetLatency を渡した利用者だけ、宣言した遅延を超える。

## 設計方針

- `with_config` からも同じ設定処理を呼び、`set_ceiling_us` と `refresh_unshared_state` / `align_sync_extras` を適用する。共通の内部ヘルパに集約する。
- `with_config` の doc に、targetLatency が閉ループの上限になることを明記する。
- 両経路で同じ結果になることをテストで固定する。

## 完了条件

- `with_config` で targetLatency を渡した場合も閉ループの上限が設定されること。
- setter 経路との等価性がテストで固定されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
