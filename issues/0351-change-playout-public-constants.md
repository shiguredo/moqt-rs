# playout の内部専用定数を pub(crate) に下げる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/change-playout-public-constants
- Polished: {YYYY-MM-DD}

## 目的

playout の調整定数の多くが `pub` になっており、利用者向けの調整値と内部実装の詳細が区別されていない。`pub` は互換性の約束になるため、内部専用の定数を `pub(crate)` に下げて公開面を減らす。

## 現状

- `src/playout/timeline.rs` の `TIMELINE_WINDOW_US` / `TIMELINE_LATE_FRAMES_PER_SECOND` / `TIMELINE_QUEUE_HEADROOM_FRAMES` / `TIMELINE_BASE_DRIFT_WINDOW_US` / `TIMELINE_BASE_UNSHARED_HOLD_US` / `TIMELINE_BASE_DIFFERENCE_*` / `TIMELINE_BASE_DRIFT_CONFIRM_US` /
  `TIMELINE_BASE_UNSHARED_RELEASE_US` は他ファイルから参照されない。
- `src/audio_clock.rs` の `AUDIO_TIMESTAMP_SLOPE_WINDOW_US`、`src/playout/delay.rs` の `AUDIO_DELAY_QUANTILE` / `AUDIO_DELAY_FORGET_FACTOR` / `AUDIO_DELAY_RESAMPLE_INTERVAL_MS`、`TIME_STRETCH_*` の大半、`AUDIO_RECEIVE_CATCH_UP_SKIP_MIN_MS`、`AUDIO_PLAYOUT_RESYNC_MIN_JUMP_US` も同様。
- `src/playout/scheduler.rs` の `AUDIO_PLAYOUT_BACKLOG_US` が `300_000 - ...` とマジックナンバーを持つ。
- 影響: 公開 API 面が実態より広く、互換性維持コストが増える。

## 設計方針

- 利用者が調整すべき値だけを `pub` に残し、他は `pub(crate)` にする。破壊的変更として扱う。
- マジックナンバーを名前付き定数にする。
- `SKILL.md` と doc の定数一覧を更新する。

## 完了条件

- 内部専用定数が `pub(crate)` になっていること。
- `SKILL.md` / rustdoc の公開定数一覧が実装と一致すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` が通ること。
