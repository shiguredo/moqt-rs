# playout/timeline.rs の A/V 同期の実装を分割する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-playout-timeline-split
- Polished: 2026-11-21

## 目的

`src/playout/timeline.rs` は 2,149 行あり、公開定数 34 個、公開型 7 個、公開 free 関数 5 個、A/V 同期と基準の差の判定・解除の状態機械、表示の遅れの上限を求める private ヘルパが 1 ファイルに混在している。
公開 API は `playout::timeline::*` のパスを保つため `timeline.rs` に残し、A/V 同期の状態機械を `timeline/sync.rs` へ、表示の遅れの上限を求める private ヘルパを `timeline/config.rs` へ移して、変更とレビューの範囲を読みやすくする。テストは既に `tests/test_playout/timeline.rs` と `pbt/tests/prop_playout/timeline.rs` にあり、本 issue では動かさない。

## 現状

- `src/playout/timeline.rs` は 2,149 行で、次のものが同居している。
  - 公開定数 34 個 (`TIMELINE_WINDOW_US` から `TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US` までの `pub const`)。
  - private 定数 1 個 (`TIMELINE_AUDIO_PLAYOUT_CORRECTION_SMOOTHING_US`)。
  - 公開型 7 個 (`Track` / `UnsharedReason` / `AudioPlayoutDelayStats` / `TrackBreakdown` / `DelayBreakdown` / `TimelineConfig` / `PlayoutTimeline`)。
  - 公開 free 関数 5 個 (`sync_compensation_limit_us` / `audio_arrival_delay_us` / `audio_arrival_delay_us_with_min` / `playout_delay_percentile` / `playout_queue_cap_us`)。
  - private 型 5 個 (`TimedWindow` / `CatchUpCheckpoint` / `TrackState` / `PresentationRecord` / `AudioArrivalDelayObservation`)。
  - private ヘルパ 3 個 (`percentile_ppm` / `presentation_cap_us` / `is_catching_up`) と `pub(crate)` の `percentile_index`。
  - A/V 同期と基準の差の判定・解除の `impl PlayoutTimeline`。主なメソッドは次のとおりである。
    - 同期の制御: `sync_natural_presentation_us` / `align_sync_extras` / `update_sync_delays` / `update_sync_delays_to_arrival_audio` / `decay_sync_extras`
    - 共有の判定と解除: `drifted_stream` / `held_unshared_stream` / `hold_unshared_stream` / `refresh_unshared_state` / `update_unshared_release` / `can_release_unshared_hold` / `is_unshared_repeatedly`
    - 基準の差の記録と判定: `record_base_difference` / `recent_base_us` / `base_difference_drifted` / `update_base_difference_drift` / `base_difference_history` / `current_base_difference_us` / `clear_base_differences` / `base_difference_limit_us` / `base_drift_us_per_second` / `base_unsha
red_return_us`
  - `natural_delay_us_of` / `presentation_delay_us_of` / `delay_us_of` / `uncapped_delay_us_of` は同期以外の公開メソッドからも使う private ヘルパである。
- `src/playout/timeline.rs` に `#[cfg(test)] mod tests` は無い。テストは `tests/test_playout/timeline.rs` と `pbt/tests/prop_playout/timeline.rs` にあり、公開 API 経由で `playout::timeline::*` を参照している。`tests/test_playout/timeline.rs` は `TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US` も import している。
- `src/playout/timing.rs` は `crate::playout::timeline::percentile_index` を import している。
- `playout` モジュールは `buffer` / `catch_up` / `expansion` / `gap` / `predictor` / `publish_catch_up` / `resample` / `scheduler` / `splice_fade` / `stretch` / `timeline` / `timing` に分割済みだが、`timeline.rs` は分割されていない。
- 影響: ファイルが大きすぎて差分レビューが難しい。A/V 同期の変更と公開 API の変更が同じファイルの差分に混ざる。

## 設計方針

- `src/playout/timeline.rs` をモジュールのルートとして残し、`mod config;` / `mod sync;` を宣言する。`mod.rs` は使わない。`timeline/tests.rs` は作らない (テストは既に `tests/test_playout/timeline.rs` にある)。
- `src/playout/timeline/config.rs` に free 関数の private ヘルパ `presentation_cap_us` を移す。`timeline.rs` と `timeline/sync.rs` から使うため `pub(super)` にし、クレート外へ公開しない。`PlayoutTimeline::presentation_cap_us` と `delay_cap_us` は `config::presentation_cap_us` を呼ぶ。
- `TIMELINE_BASE_DIFFERENCE_SAMPLE_INTERVAL_US` と `TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US` は公開定数であり、公開パス (`playout::timeline::*`) を保つため `timeline.rs` に残す。`timeline/sync.rs` は `super::` 経由で参照する。`TIMELINE_AUDIO_PLAYOUT_CORRECTION_SMOOTHING_US` も private のまま `timeline.rs` に残す。
`percentile_ppm` も `timeline.rs` に残す。
- `TIMELINE_WINDOW_MAX_ENTRIES` は現行実装に無い。`TimedWindow` は件数の上限を置かず、0349 も件数の上限を設けないため、本 issue では扱わない。
- `src/playout/timeline/sync.rs` に A/V 同期と基準の差の判定・解除の `impl PlayoutTimeline` を移す。移すメソッドは次のとおりである。
  - 同期の制御: `sync_natural_presentation_us` / `align_sync_extras` / `update_sync_delays` / `update_sync_delays_to_arrival_audio` / `decay_sync_extras`
  - 共有の判定と解除: `drifted_stream` / `held_unshared_stream` / `hold_unshared_stream` / `refresh_unshared_state` / `update_unshared_release` / `can_release_unshared_hold` / `is_unshared_repeatedly`
  - 基準の差の記録と判定: `record_base_difference` / `recent_base_us` / `base_difference_drifted` / `update_base_difference_drift` / `base_difference_history` / `current_base_difference_us` / `clear_base_differences` / `base_difference_limit_us` / `base_drift_us_per_second` / `base_unshare
d_return_us`
- `timeline.rs` から呼ぶメソッド (`record_base_difference` / `update_sync_delays` / `drifted_stream` / `refresh_unshared_state` / `sync_natural_presentation_us` / `align_sync_extras` / `held_unshared_stream` / `base_difference_drifted` / `current_base_difference_us` / `base_difference_li
mit_us` / `base_drift_us_per_second` / `base_unshared_return_us` / `clear_base_differences`) は `pub(super)` にし、
`sync.rs` 内だけで使うメソッド (`update_sync_delays_to_arrival_audio` / `decay_sync_extras` / `hold_unshared_stream` / `update_unshared_release` / `can_release_unshared_hold` / `is_unshared_repeatedly` / `recent_base_us` / `update_base_difference_drift` / `base_difference_history`) は priva
te のままにする。
- `PlayoutTimeline` と `TrackState` のフィールドは private のままにする (子モジュールからは参照できる)。
- 公開定数、公開型、公開 free 関数、`TrackState`、`PresentationRecord`、`AudioArrivalDelayObservation`、`CatchUpCheckpoint`、`PlayoutTimeline` の定義と公開メソッド、
同期以外の private な関数とメソッド (`percentile_ppm` / `is_catching_up` / `natural_delay_us_of` / `presentation_delay_us_of` / `delay_us_of` / `uncapped_delay_us_of` / `track_breakdown` / `update_limited_us` / `delay_cap_us` など)、`percentile_index`、
`PlayoutTimeline::audio_arrival_delay_us` は `timeline.rs` に残す。`TimedWindow` は 0349 が先行していない場合 `timeline.rs` に残す。公開定数と公開型をサブモジュールへ移すと公開パスが変わるため、`pub use` による re-export は追加しない。
- 公開 API のパス・型・シグネチャと挙動は変えない。
`src/playout.rs` / `src/playout/timing.rs` / `tests/test_playout/timeline.rs` / `pbt/tests/prop_playout/timeline.rs` / `src/playout/scheduler.rs` / `src/playout/buffer.rs` / `src/playout/catch_up.rs` / `crates/moq-sub/src/main.rs` / `docs/moqt.md` / `skills/shiguredo-moqt/SKILL.m
d` は変更しない。`docs/audio-playout.md` は `src/playout/timeline.rs` の説明を分割後のファイル構成に合わせる。
- `TimedWindow` は [issues/0349](0349-refactor-playout-window-duplication.md) が共通の窓型 (`src/playout/window.rs` など) へ移す担当である。0349 が先行した場合は `TrackState` の窓の型を 0349 が定める共通窓型として扱い、本 issue では `TimedWindow` を移動しない。
- [issues/0351](0351-change-playout-public-constants.md) の定数の可視性変更とは独立である。本 issue では公開定数の可視性を変えない。
- 各サブモジュールの先頭に責務を 1〜2 行の `//!` で書く。ソースコードのコメントに issue 番号を書かない。

## 変更対象

- `src/playout/timeline.rs` (モジュール宣言、公開 API、状態型、同期以外の実装)
- `src/playout/timeline/config.rs` (新規)
- `src/playout/timeline/sync.rs` (新規)
- `docs/audio-playout.md` (`src/playout/timeline.rs` の説明)
- `src/playout.rs` (変更なし。`pub mod timeline;` のまま)
- `src/playout/timing.rs` (変更なし。`percentile_index` は `timeline.rs` に残す)
- `tests/test_playout/timeline.rs` / `pbt/tests/prop_playout/timeline.rs` (変更なし。公開 API のパスを変えない)
- `src/playout/scheduler.rs` / `src/playout/buffer.rs` / `src/playout/catch_up.rs` / `crates/moq-sub/src/main.rs` (変更なし。公開 API のパスを変えない)
- `docs/moqt.md` / `skills/shiguredo-moqt/SKILL.md` (変更なし。公開 API のパスを変えず、`timeline.rs` はモジュールルートとして残る)

## 完了条件

- `src/playout/timeline.rs` から A/V 同期と基準の差の判定・解除の `impl` と、`timeline/config.rs` へ移す free 関数の `presentation_cap_us` がなくなっていること (`PlayoutTimeline::presentation_cap_us` メソッドは残る)。`#[cfg(test)] mod tests` は元々無い。
- `src/playout/timeline.rs` に公開定数、公開型、公開 free 関数、`TrackState`、`PresentationRecord`、`AudioArrivalDelayObservation`、`CatchUpCheckpoint`、`PlayoutTimeline` の定義と公開メソッド、同期以外の private な関数とメソッド、`percentile_index`、`percentile_ppm`、`is_catching_up` が残っていること。
`TimedWindow` は 0349 が先行していなければ `timeline.rs` に、先行していれば 0349 が定める共通窓型のファイルにあること。
- `src/playout/timeline/config.rs` / `src/playout/timeline/sync.rs` が `src/playout/timeline.rs` から宣言され、`mod.rs` と `pub use` による re-export が追加されていないこと。`timeline/tests.rs` を作っていないこと。
- `tests/test_playout/timeline.rs` と `pbt/tests/prop_playout/timeline.rs` が変更なしで通ること。`TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US` などの公開定数のパスが変わっていないこと。
- 公開 API のパス・型・シグネチャと挙動が変わっていないこと。
- `docs/audio-playout.md` の `src/playout/timeline.rs` の説明が分割後のファイル構成と一致していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が通ること。
- `cargo build -p shiguredo_moqt --target thumbv7em-none-eabihf` が通ること。
