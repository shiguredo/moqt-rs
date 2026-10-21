# playout と audio_clock の定数を整理する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/change-playout-public-constants
- Polished: 2026-10-21

## 目的

playout と audio_clock の調整定数の多くが `pub` になっており、利用者向けの調整値と内部実装の詳細が区別されていない。`pub` は互換性の約束になるため、内部専用の定数を `pub(crate)` に下げて公開面を減らす。同じ定数整理として、`AUDIO_PLAYOUT_BACKLOG_US` の `300_000` を名前付き定数にして、並べすぎの合計 300 ms の意図を読めるようにする。

## 現状

- `src/playout/timeline.rs` には 26 個の `pub const` がある。`TIMELINE_BASE_UNSHARED_RELEASE_US` は
  `pbt/tests/prop_playout/timeline.rs` が参照している。
  `TIMELINE_DISCONTINUITY_US` / `TIMELINE_MAX_PRESENTATION_DELAY_MS` / `TIMELINE_SYNC_MIN_DELTA_US` /
  `TIMELINE_AUDIO_DELAY_FLOOR_US` は、`src/` の他モジュール、`tests/`、`pbt/` のいずれかが参照している。
  `TIMELINE_ARRIVAL_DELAY_US` / `TIMELINE_BASE_DRIFT_US` / `TIMELINE_MAX_COMPENSATED_DIFFERENCE_US` も同様である。
  残りの 18 個は定義ファイル以外のソースファイルから参照されていない。
- `src/audio_clock.rs` の `AUDIO_TIMESTAMP_SLOPE_WINDOW_US` は定義ファイル以外のソースファイルから参照されていない。他の `AUDIO_TIMESTAMP_*` は `tests/` と `pbt/` のいずれかが参照している。
- `src/playout/delay.rs` の `AUDIO_DELAY_HISTORY_WINDOW_MS` / `AUDIO_DELAY_QUANTILE` / `AUDIO_DELAY_FORGET_FACTOR` / `AUDIO_DELAY_RESAMPLE_INTERVAL_MS` は定義ファイル以外のソースファイルから参照されていない。`AUDIO_DELAY_BUCKETS` / `AUDIO_DELAY_BUCKET_MS` / `AUDIO_DELAY_START_MS` は、`src/` の他モジュール、`tests/`、`pbt/` のいずれかが参照している。
- `src/playout/stretch.rs` の `TIME_STRETCH_CORRELATION_LEN` / `TIME_STRETCH_CORRELATION_THRESHOLD` /
  `TIME_STRETCH_SILENCE_PEAK` / `TIME_STRETCH_MAX_SEAM_STEP_RATIO` は定義ファイル以外のソースファイルから
  参照されていない。`TIME_STRETCH_TAIL_CORRELATION_LEN` / `TIME_STRETCH_CONCEAL_FADE_CORRELATION` も同様である。
  `TIME_STRETCH_MIN_LAG` / `TIME_STRETCH_MAX_LAG` / `TIME_STRETCH_CONCEAL_FADE_MS` /
  `TIME_STRETCH_CONCEAL_END_GAIN` / `TIME_STRETCH_MAX_CONCEAL_US` は、`src/` の他モジュール、`tests/`、
  `pbt/` のいずれかが参照している。
- `src/playout/catch_up.rs` の `AUDIO_RECEIVE_CATCH_UP_SKIP_MIN_MS` は定義ファイル以外のソースファイルから参照されていない。他の `AUDIO_RECEIVE_CATCH_UP_*` は `tests/` と `pbt/` のいずれかが参照している。
- `src/playout/scheduler.rs` の `AUDIO_PLAYOUT_RESYNC_MIN_JUMP_US` は定義ファイル以外のソースファイルから参照されていない。他の `AUDIO_PLAYOUT_*` は、`src/` の他モジュール、`tests/`、`pbt/`、`examples/` のいずれかが参照している。`AUDIO_PLAYOUT_BACKLOG_US` は `300_000 - AUDIO_PLAYOUT_DELAY_US` と、並べすぎの合計 300 ms をマジックナンバーで持つ。
- `src/playout/publish_catch_up.rs` の `AUDIO_PUBLISH_CATCH_UP_CONFIRM_MS` / `AUDIO_PUBLISH_CATCH_UP_GROWTH_MS` /
  `AUDIO_PUBLISH_CATCH_UP_PENDING_TIMEOUT_MS` は定義ファイル以外のソースファイルから参照されていない。
  `AUDIO_PUBLISH_CATCH_UP_QUEUE_CONFIRM_FRAMES` / `AUDIO_PUBLISH_CATCH_UP_RESUME_MS` も同様である。
  `AUDIO_PUBLISH_CATCH_UP_COOLDOWN_MS` / `AUDIO_PUBLISH_CATCH_UP_MIN_MS` は `tests/` が参照している。
- `src/playout/feedback.rs` の `AUDIO_DELAY_FEEDBACK_MARGIN_US` は定義ファイル以外のソースファイルから参照されていない。他の `AUDIO_DELAY_FEEDBACK_*` は、`src/` の他モジュール、`tests/`、`pbt/`、`examples/` のいずれかが参照している。
- `src/playout/splice_fade.rs` の `AUDIO_SPLICE_FADE_US` は定義ファイル以外のソースファイルから参照されていない。
- `src/playout/timing.rs` / `src/playout/buffer.rs` / `src/playout/gap.rs` の `pub const` は、`src/` の他モジュール、`tests/`、`pbt/`、`skills/shiguredo-moqt/SKILL.md` のいずれかが参照している。
- `skills/shiguredo-moqt/SKILL.md` は `TIMELINE_DELAY_DECAY_US_PER_SECOND` を名前で参照している。この定数を `pub(crate)` に下げる場合は、SKILL.md の記述を値または `pub` のまま残す定数へ合わせる必要がある。
- `docs/` に playout と audio_clock の公開定数一覧は無い。`skills/shiguredo-moqt/SKILL.md` の「公開定数」の表は session の定数だけを扱っている。
- 影響: 公開 API 面が実態より広く、互換性維持コストが増える。

## 設計方針

- 本 issue の対象は、`src/playout/*.rs` と `src/audio_clock.rs` の `pub const` のうち、定義ファイル以外のソースファイル (`src/`、`tests/`、`pbt/`、`examples/`) から参照されていないものとする。対象を `pub(crate)` に下げる。破壊的変更として扱う。
- 定義ファイル以外のソースファイルから参照されている定数は `pub` のまま残す。これらは `src/` の他モジュールから参照される crate 内共有の定数、または `tests/`、`pbt/`、`examples/` のいずれかが参照する定数であり、可視性を下げるにはテスト・example 側の変更が必要になる。本 issue では扱わない。特に `TIMELINE_BASE_UNSHARED_RELEASE_US` は `pbt/tests/prop_playout/timeline.rs` が参照するため、`pub` のまま残す。
- `AUDIO_PLAYOUT_BACKLOG_US` の `300_000` を名前付き定数にする。並べすぎの合計 300 ms を表す名前を付け、`AUDIO_PLAYOUT_BACKLOG_US` の値 (250 ms) と振る舞いは変えない。
- `skills/shiguredo-moqt/SKILL.md` の playout の記述から、`pub(crate)` に下げる定数の名前を外し、値または `pub` のまま残す定数で説明する。`docs/` は変更しない。rustdoc は可視性を反映するため、公開定数の一覧を別途更新する必要はない。
- `tests/`、`pbt/`、`examples/` は変更しない。対象の定数はこれらから参照されていないため、`cargo test --workspace` は変更なしで通る。
- 0349 と 0350 の完了後に実施する。0349 は catch_up と publish_catch_up の公開定数を残す前提で共通化し、0350 は `src/playout/timeline.rs` の公開定数を残す前提で分割する。両方とも、それぞれの変更範囲で公開 API を変えないという意味であり、本 issue の可視性変更を妨げない。先に完了させると、本 issue の対象ファイルと定数の位置が確定する。
- `CHANGES.md` は `CODEBASE.md` の現方針どおり更新しない。
- ソースコードに issue 番号を書かない。

## 変更対象

- `src/playout/timeline.rs`
- `src/playout/delay.rs`
- `src/playout/stretch.rs`
- `src/playout/catch_up.rs`
- `src/playout/scheduler.rs`
- `src/playout/publish_catch_up.rs`
- `src/playout/feedback.rs`
- `src/playout/splice_fade.rs`
- `src/audio_clock.rs`
- `skills/shiguredo-moqt/SKILL.md`
- `tests/`、`pbt/`、`examples/` は変更しない。

## 完了条件

- `src/playout/*.rs` と `src/audio_clock.rs` の `pub const` のうち、定義ファイル以外のソースファイルから参照されていないものが `pub(crate)` になっていること (`rg` で確認)。
- `TIMELINE_BASE_UNSHARED_RELEASE_US` など、定義ファイル以外のソースファイルから参照されている定数が `pub` のままであること。
- `AUDIO_PLAYOUT_BACKLOG_US` の `300_000` が名前付き定数になっていること。`AUDIO_PLAYOUT_BACKLOG_US` の値が 250_000 のままで、並べすぎの判定の振る舞いが変わっていないこと。
- `skills/shiguredo-moqt/SKILL.md` の playout の記述が、`pub(crate)` に下げる定数を名前で参照していないこと。
- `tests/`、`pbt/`、`examples/` を変更せずに `cargo test --workspace` が通ること。
- `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が通ること。
- `cargo doc` の `playout` と `audio_clock` の公開定数が、`pub` のまま残すと決めたものだけであること。
