# playout/timeline.rs の A/V 同期の実装とテストを分割する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-playout-timeline-split
- Polished: 2026-10-21

## 目的

`src/playout/timeline.rs` は 2,254 行あり、公開定数 26 個、private 定数 3 個、公開型 6 個、A/V 同期と基準の差の判定・解除の状態機械、24 個の単体テスト (505 行) が 1 ファイルに混在している。公開定数と公開型は `playout::timeline::*` の公開パスを保つため `timeline.rs` に残し、A/V 同期の状態機械と単体テストを責務ごとのサブモジュールへ移して、変更とレビューの範囲を読みやすくする。

## 現状

- `src/playout/timeline.rs` は 2,254 行で、次のものが同居している。
  - 公開定数 26 個 (`TIMELINE_WINDOW_US` から `TIMELINE_BASE_UNSHARED_RELEASE_US` までの `pub const`) と `audio_arrival_delay_us`
  - private 定数 3 個 (`TIMELINE_WINDOW_MAX_ENTRIES` / `TIMELINE_BASE_DIFFERENCE_SAMPLE_INTERVAL_US` / `TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US`)
  - 公開型 6 個 (`Track` / `UnsharedReason` / `TrackBreakdown` / `DelayBreakdown` / `TimelineConfig` / `PlayoutTimeline`)
  - private 型 (`TimedWindow` / `TrackState` / `PresentationRecord`) と private ヘルパ (`video_percentile_milli` / `presentation_cap_us` / `is_catching_up` / `video_target_us`)
  - A/V 同期と基準の差の判定・解除の `impl PlayoutTimeline`。主なメソッドは次のとおりである。
    - 同期の制御: `sync_natural_presentation_us` / `align_sync_extras` / `desired_difference_us` / `target_extra_us` / `update_sync_delays` / `update_sync_delays_to_arrival_audio` / `decay_sync_extras`
    - 共有の判定と解除: `drifted_track` / `held_unshared_track` / `refresh_unshared_state` / `update_unshared_release` / `can_release_unshared_hold` / `is_unshared_repeatedly`
    - 基準の差の記録と判定: `record_base_difference` / `recent_base_us` / `base_difference_drifted` / `update_base_difference_drift` / `base_difference_history` / `current_base_difference_us` / `clear_base_differences` / `natural_delay_known_us` / `base_difference_limit_us` / `base_drift_us_per_second`
  - `#[cfg(test)] mod tests` の 24 テスト (505 行)
- `playout` モジュールは `buffer` / `catch_up` / `delay` / `feedback` / `gap` / `publish_catch_up` / `scheduler` / `splice_fade` / `stretch` / `timing` に分割済みだが、`timeline.rs` は分割されていない。
- `src/playout/timing.rs` は `crate::playout::timeline::percentile_index` を import している。`tests/test_playout/timeline.rs` と `pbt/tests/prop_playout/timeline.rs` は公開 API 経由で `playout::timeline::*` を参照している。
- 影響: ファイルが大きすぎて差分レビューが難しい。A/V 同期の変更と公開 API の変更が同じファイルの差分に混ざる。

## 設計方針

- `src/playout/timeline.rs` をモジュールのルートとして残し、`mod config;` / `mod sync;` / `#[cfg(test)] mod tests;` を宣言する。`mod.rs` は使わない。
- `src/playout/timeline/config.rs` に private 定数 `TIMELINE_BASE_DIFFERENCE_SAMPLE_INTERVAL_US` と
  `TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US`、および free 関数の private ヘルパ `video_percentile_milli` と
  `presentation_cap_us` を移す。`timeline.rs` と `timeline/sync.rs` から使う項目は `pub(super)` にし、クレート外へ公開しない。
  `PlayoutTimeline::presentation_cap_us` メソッドは `timeline.rs` に残し、`config::presentation_cap_us` を呼ぶ。
  `TIMELINE_WINDOW_MAX_ENTRIES` は `TimedWindow` の移動を扱う
  [issues/0349](0349-refactor-playout-window-duplication.md) の範囲なので、本 issue では移さない。
- `src/playout/timeline/sync.rs` に A/V 同期と基準の差の判定・解除の `impl PlayoutTimeline` を移す。
  `timeline.rs` と `timeline/tests.rs` から呼ぶメソッド (`record_base_difference` / `update_sync_delays` / `update_unshared_release` /
  `drifted_track` / `refresh_unshared_state` / `sync_natural_presentation_us` / `align_sync_extras` /
  `held_unshared_track` / `base_difference_drifted` / `current_base_difference_us` /
  `base_difference_limit_us` / `base_drift_us_per_second` / `clear_base_differences` など) は
  `pub(super)` にし、`sync.rs` 内だけで使うメソッドは private のままにする。
  `PlayoutTimeline` と `TrackState` のフィールドは private のままにする (子モジュールからは参照できる)。
- `src/playout/timeline/tests.rs` に現在の `#[cfg(test)] mod tests` の中身を移す。`tests.rs` には `mod tests { ... }` の外側の宣言を書かず、中身だけを置く。テスト名・テスト内容・テスト数は変えない。`use super::*;` で `timeline.rs` の項目を共有する。
- 公開定数、公開型、`TrackState`、`PresentationRecord`、`PlayoutTimeline` の定義と公開メソッド、同期以外の private メソッド、`percentile_index`、`audio_arrival_delay_us`、`is_catching_up`、`video_target_us` は `timeline.rs` に残す。`TimedWindow` の扱いは次の 0349 の項目に従う。公開定数と公開型をサブモジュールへ移すと公開パスが変わるため、`pub use` による re-export は追加しない。
- 公開 API のパス・型・シグネチャと挙動は変えない。`src/playout.rs` / `src/playout/timing.rs` / `tests/test_playout/timeline.rs` / `pbt/tests/prop_playout/timeline.rs` / `docs/moqt.md` / `skills/shiguredo-moqt/SKILL.md` は変更しない。
- `TimedWindow` は [issues/0349](0349-refactor-playout-window-duplication.md) が共通の窓型 (`src/playout/window.rs` など) へ移す担当である。0349 が先行した場合は `TrackState` の窓の型を 0349 が定める共通窓型として扱い、本 issue では `TimedWindow` を移動しない。
- [issues/0351](0351-change-playout-public-constants.md) の定数の可視性変更とは独立である。本 issue では公開定数の可視性を変えない。
- 各サブモジュールの先頭に責務を 1〜2 行の `//!` で書く。ソースコードのコメントに issue 番号を書かない。

## 変更対象

- `src/playout/timeline.rs` (モジュール宣言、公開 API、状態型、同期以外の実装)
- `src/playout/timeline/config.rs` (新規)
- `src/playout/timeline/sync.rs` (新規)
- `src/playout/timeline/tests.rs` (新規)
- `src/playout.rs` (変更なし。`pub mod timeline;` のまま)
- `src/playout/timing.rs` (変更なし。`percentile_index` は `timeline.rs` に残す)
- `tests/test_playout/timeline.rs` / `pbt/tests/prop_playout/timeline.rs` (変更なし。公開 API のパスを変えない)
- `src/playout/scheduler.rs` / `src/playout/feedback.rs` / `src/playout/catch_up.rs` / `src/playout/buffer.rs` / `examples/moq-sub/src/main.rs` (変更なし。公開 API のパスを変えない)
- `docs/moqt.md` / `docs/audio-playout.md` / `skills/shiguredo-moqt/SKILL.md` (変更なし。公開 API のパスを変えず、`timeline.rs` はモジュールルートとして残る)

## 完了条件

- `src/playout/timeline.rs` から `#[cfg(test)] mod tests` の本体、A/V 同期と基準の差の判定・解除の `impl`、`timeline/config.rs` へ移す private 定数と private ヘルパがなくなっていること。
- `src/playout/timeline.rs` に公開定数、公開型、`TrackState`、`PresentationRecord`、`PlayoutTimeline` の定義と公開メソッド、同期以外の private メソッド、`percentile_index`、`audio_arrival_delay_us`、`is_catching_up`、`video_target_us` が残っていること。`TimedWindow` は 0349 が先行していなければ `timeline.rs` に、先行していれば 0349 が定める共通窓型のファイルにあること。
- `src/playout/timeline/config.rs` / `src/playout/timeline/sync.rs` / `src/playout/timeline/tests.rs` が `src/playout/timeline.rs` から宣言され、`mod.rs` と `pub use` による re-export が追加されていないこと。
- 単体テストの数 (24) と内容が分割前後で変わっていないこと。`tests/test_playout/timeline.rs` と `pbt/tests/prop_playout/timeline.rs` が変更なしで通ること。
- 公開 API のパス・型・シグネチャと挙動が変わっていないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が通ること。
- `cargo build -p shiguredo_moqt --target thumbv7em-none-eabihf` が通ること。
