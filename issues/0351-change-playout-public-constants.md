# playout と audio_clock の定数を整理する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/change-playout-public-constants
- Polished: 2026-11-21

## 目的

playout と audio_clock の調整定数の多くが `pub` になっており、利用者向けの調整値と内部実装の詳細が区別されていない。`pub` は互換性の約束になるため、内部専用の定数を `pub(crate)` に下げて公開面を減らす。`AUDIO_PLAYOUT_BACKLOG_US` の 300 ms は既に `AUDIO_PLAYOUT_MAX_DELAY_US` として名前付き定数になっているため、本 issue ではその値 (250 ms) と振る舞いを変えない。

## 現状

- `src/playout/timeline.rs` には 34 個の `pub const` がある。定義ファイル以外のソースファイルから参照されているのは次の 17 個である。
  `TIMELINE_WINDOW_US` / `TIMELINE_JITTER_WINDOW_US` / `TIMELINE_DISCONTINUITY_US` /
  `TIMELINE_MIN_PLAYOUT_DELAY_PERCENTILE_PPM` / `TIMELINE_MAX_PRESENTATION_DELAY_MS` /
  `TIMELINE_QUEUE_HEADROOM_FRAMES` / `TIMELINE_MIN_BASE_DIFFERENCE_MS` /
  `TIMELINE_SYNC_MIN_DELTA_US` / `TIMELINE_AUDIO_DELAY_FLOOR_US` /
  `TIMELINE_MIN_COMPRESSION_DEADBAND_US` / `TIMELINE_ARRIVAL_DELAY_US` /
  `TIMELINE_BASE_DRIFT_US` / `TIMELINE_MAX_COMPENSATED_DIFFERENCE_US` /
  `TIMELINE_BASE_UNSHARED_HOLD_US` / `TIMELINE_BASE_UNSHARED_RELEASE_US` /
  `TIMELINE_BASE_DRIFT_CONFIRM_US` / `TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US`。
  これらは `src/` の他モジュール、`tests/`、`pbt/` のいずれかが参照している。
  `TIMELINE_BASE_UNSHARED_RELEASE_US` は `pbt/tests/prop_playout/timeline.rs` が参照している。
  残りの 17 個は定義ファイル以外のソースファイルから参照されていない。このうち
  `TIMELINE_P50_PERCENTILE_PPM` は定義ファイル内でも使われていない。
- `src/audio_clock.rs` の `AUDIO_TIMESTAMP_SLOPE_WINDOW_US` は定義ファイル以外のソースファイルから参照されていない。他の `AUDIO_TIMESTAMP_*` は `tests/` と `pbt/` のいずれかが参照している。
- `src/playout/stretch.rs` の `TIME_STRETCH_CORRELATION_LEN` / `TIME_STRETCH_CORRELATION_THRESHOLD` /
  `TIME_STRETCH_SILENCE_PEAK` / `TIME_STRETCH_MAX_SEAM_STEP_RATIO` は定義ファイル以外のソースファイルから
  参照されていない。`TIME_STRETCH_TAIL_CORRELATION_LEN` / `TIME_STRETCH_CONCEAL_FADE_CORRELATION` も同様である。
  `TIME_STRETCH_CORRELATION_LEN` は `tests/test_playout/stretch.rs` のコメントに名前が出るが、コードからは
  参照していない。
  `TIME_STRETCH_MIN_LAG` / `TIME_STRETCH_MAX_LAG` / `TIME_STRETCH_CONCEAL_FADE_MS` /
  `TIME_STRETCH_CONCEAL_END_GAIN` / `TIME_STRETCH_MAX_CONCEAL_US` は、`src/` の他モジュール、`tests/`、
  `pbt/` のいずれかが参照している。
- `src/playout/catch_up.rs` の `AUDIO_RECEIVE_CATCH_UP_SKIP_MIN_MS` は定義ファイル以外のソースファイルから参照されていない。他の `AUDIO_RECEIVE_CATCH_UP_*` は `tests/` と `pbt/` のいずれかが参照している。
- `src/playout/scheduler.rs` の `AUDIO_PLAYOUT_MAX_LATENESS_US` /
  `AUDIO_PLAYOUT_RESYNC_MIN_JUMP_US` は定義ファイル以外のソースファイルから参照されていない。
  他の `AUDIO_PLAYOUT_*` は、`src/` の他モジュール、`tests/`、`pbt/`、`crates/` のいずれかが
  参照している。`AUDIO_PLAYOUT_BACKLOG_US` は `AUDIO_PLAYOUT_MAX_DELAY_US -
  AUDIO_PLAYOUT_DELAY_US` であり、`300_000` は `AUDIO_PLAYOUT_MAX_DELAY_US` として既に
  名前付き定数になっている。`AUDIO_PLAYOUT_BACKLOG_US` の値は 250_000 のままである。
- `src/playout/publish_catch_up.rs` の `AUDIO_PUBLISH_CATCH_UP_CONFIRM_MS` / `AUDIO_PUBLISH_CATCH_UP_GROWTH_MS` /
  `AUDIO_PUBLISH_CATCH_UP_PENDING_TIMEOUT_MS` は定義ファイル以外のソースファイルから参照されていない。
  `AUDIO_PUBLISH_CATCH_UP_QUEUE_CONFIRM_FRAMES` / `AUDIO_PUBLISH_CATCH_UP_RESUME_MS` も同様である。
  `AUDIO_PUBLISH_CATCH_UP_COOLDOWN_MS` / `AUDIO_PUBLISH_CATCH_UP_MIN_MS` は `tests/` が参照している。
- `src/playout/splice_fade.rs` の `AUDIO_SPLICE_FADE_US` は定義ファイル以外のソースファイルから参照されていない。
- `src/playout/timing.rs` / `src/playout/buffer.rs` / `src/playout/gap.rs` / `src/playout/expansion.rs` /
  `src/playout/predictor.rs` / `src/playout/resample.rs` の `pub const` は、`src/` の他モジュール、
  `tests/`、`pbt/`、`crates/` のいずれかが参照している。
- `e2e-tests/` と `fuzz/` は、対象の `pub const` を参照していない。
- `skills/shiguredo-moqt/SKILL.md` は `TIMELINE_SYNC_MIN_DELTA_US` と `TIMELINE_MAX_COMPENSATED_DIFFERENCE_US` を名前で参照している。どちらも `pub` のまま残すため、SKILL.md は変更しない。`TIMELINE_DELAY_DECAY_US_PER_SECOND` を名前で参照しているのは `docs/audio-playout.md` であり、設計の説明であって公開 API の一覧ではないため、`docs/` も変更しない。
- `docs/` に playout と audio_clock の公開定数一覧は無い。`skills/shiguredo-moqt/SKILL.md` の「公開定数」の表は session の定数だけを扱っている。
- 影響: 公開 API 面が実態より広く、互換性維持コストが増える。

## 設計方針

- 本 issue の対象は、`src/playout/*.rs` と `src/audio_clock.rs` の `pub const` のうち、定義ファイル以外のソースファイル (`src/`、`tests/`、`pbt/`、`crates/`、`e2e-tests/`、`fuzz/`) から参照されていないものとする。2026-11-21 時点で 33 個ある。このうち `TIMELINE_P50_PERCENTILE_PPM` は削除し、残りの 32 個を `pub(crate)` に下げる。破壊的変更として扱う。
- 定義ファイル以外のソースファイルから参照されている定数は `pub` のまま残す。これらは `src/` の他モジュールから参照される crate 内共有の定数、または `tests/`、`pbt/`、`crates/` のいずれかが参照する定数であり、可視性を下げるにはテスト・crates 側の変更が必要になる。本 issue では扱わない。特に `TIMELINE_BASE_UNSHARED_RELEASE_US` は `pbt/tests/prop_playout/timeline.rs` が参照するため、`pub` のまま残す。
- `TIMELINE_P50_PERCENTILE_PPM` は削除する。定義ファイル内でも使われておらず、`pub(crate)` に下げると `dead_code` 警告になるためである。公開定数の削除も破壊的変更として扱う。
- `pub(crate)` に下げる定数への公開 doc からの intra-doc link は、値の記述に置き換える。
  対象は `publish_catch_up.rs` のモジュール doc (`AUDIO_PUBLISH_CATCH_UP_RESUME_MS`)、
  `splice_fade.rs` の `cross_fade_head` / `smooth_splice_head` (`AUDIO_SPLICE_FADE_US`)、
  `stretch.rs` の `conceal` (`TIME_STRETCH_CONCEAL_FADE_CORRELATION` /
  `TIME_STRETCH_CORRELATION_THRESHOLD` / `TIME_STRETCH_MAX_SEAM_STEP_RATIO`)、
  `timeline.rs` のモジュール doc と `TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US` /
  `playout_delay_percentile` / `skew_us` (`TIMELINE_LATE_FRAMES_PER_SECOND` /
  `TIMELINE_BASE_DRIFT_STEP_US` / `TIMELINE_SKEW_WINDOW_US`)。
  `RUSTDOCFLAGS="-D warnings" cargo doc` が通ることを確認する。
- `AUDIO_PLAYOUT_BACKLOG_US` の `300_000` は、2026-11-01 の変更で `AUDIO_PLAYOUT_MAX_DELAY_US` として既に名前付き定数になっている。本 issue では値を変えず (250 ms)、`AUDIO_PLAYOUT_MAX_DELAY_US` と `AUDIO_PLAYOUT_BACKLOG_US` が `pub` のまま残ることを確認するだけにする。
- `skills/shiguredo-moqt/SKILL.md` は `pub` のまま残す定数だけを参照しているため変更しない。`docs/audio-playout.md` は `pub(crate)` に下げる定数名も参照しているが、設計の説明であり公開 API の一覧ではないため変更しない。rustdoc は可視性を反映するため、公開定数の一覧を別途更新する必要はない (ただし上記の intra-doc link は更新する)。
- `tests/`、`pbt/`、`crates/`、`e2e-tests/`、`fuzz/` は変更しない。対象の定数はこれらから参照されていないため、`cargo test --workspace` は変更なしで通る。
- 0349 と 0350 の完了後に実施する。0349 は catch_up と publish_catch_up の公開定数を残す前提で共通化し、0350 は `src/playout/timeline.rs` の公開定数を残す前提で分割する。両方とも、それぞれの変更範囲で公開 API を変えないという意味であり、本 issue の可視性変更を妨げない。2026-11-21 時点では両方とも open であるため、実装は両方の完了後に行う。完了後に `rg` で定数の位置と参照状況を再確認する。
- `CHANGES.md` は `CODEBASE.md` の現方針どおり更新しない。
- ソースコードに issue 番号を書かない。

## 変更対象

- `src/playout/timeline.rs`
- `src/playout/stretch.rs`
- `src/playout/catch_up.rs`
- `src/playout/scheduler.rs`
- `src/playout/publish_catch_up.rs`
- `src/playout/splice_fade.rs`
- `src/audio_clock.rs`
- `src/playout/expansion.rs` / `src/playout/predictor.rs` / `src/playout/resample.rs` / `src/playout/timing.rs` / `src/playout/buffer.rs` / `src/playout/gap.rs` は、すべての `pub const` が参照されているため変更しない。
- `tests/`、`pbt/`、`crates/`、`e2e-tests/`、`fuzz/` は変更しない。
- `skills/shiguredo-moqt/SKILL.md` は変更しない。
- `docs/` は変更しない。

## 完了条件

- `src/playout/*.rs` と `src/audio_clock.rs` の `pub const` のうち、定義ファイル以外のソースファイルから参照されていないものが `pub(crate)` になっていること (`rg` で確認)。2026-11-21 時点で 33 個が対象であり、このうち `TIMELINE_P50_PERCENTILE_PPM` は削除し、残りの 32 個を `pub(crate)` に下げる。0349 / 0350 完了後に参照状況を再確認し、対象が変わっていればそれに従う。
- `TIMELINE_P50_PERCENTILE_PPM` が削除されていること。
- `TIMELINE_BASE_UNSHARED_RELEASE_US` など、定義ファイル以外のソースファイルから参照されている定数が `pub` のままであること。
- `AUDIO_PLAYOUT_BACKLOG_US` の `300_000` が `AUDIO_PLAYOUT_MAX_DELAY_US` として名前付き定数になっており、`AUDIO_PLAYOUT_BACKLOG_US` の値が 250_000 のままで、並べすぎの判定の振る舞いが変わっていないこと。
- `skills/shiguredo-moqt/SKILL.md` の playout の記述が、`pub(crate)` に下げる定数を名前で参照していないこと。
- `docs/` を変更していないこと。
- `tests/`、`pbt/`、`crates/`、`e2e-tests/`、`fuzz/` を変更せずに `cargo test --workspace` が通ること。
- `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- 公開 doc から `pub(crate)` に下げる定数への intra-doc link が残っておらず、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が通ること。
- `cargo doc` の `playout` と `audio_clock` の公開定数が、`pub` のまま残すと決めたものだけであること。
