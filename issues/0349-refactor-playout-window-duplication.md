# playout のローリング窓と catch_up の重複実装を共通化する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-playout-window-duplication
- Polished: 2026-10-21

## 目的

playout と audio_clock には「時刻付き値のローリング窓」が 5 通りある。また、受信側と配信側の catch_up には、`limit_ms` の式と閾値の定数という共通部分がある。実装ごとに境界の扱いが異なり、修正時に片方だけ直す事故が起きる。共通の内部ヘルパに寄せ、重複を減らし、境界の違いを明示する。ただし、意図的に異なる床の計算と開始・終了の判定は共通化しない。

## 現状

- `src/playout/timing.rs` の `TimedValues` は `times_us` / `values_us` / `head` の 3 つを手で同期する。配列を詰め直さないための最適化だが、窓の件数は数百から数千程度で複雑さに見合わない。
- 同じ種類の時刻付き値の窓が、次の 5 箇所に別実装で存在する。
  - `src/playout/timing.rs` の `TimedValues` (余裕・到着から鳴るまで・遅れの 3 用途)
  - `src/playout/timeline.rs` の `TimedWindow` (基準の遅れ・学習用・基準の差の 3 用途)
  - `src/playout/delay.rs` の `JitterDelayManager::arrivals`
  - `src/playout/catch_up.rs` の `AudioReceiveCatchUp::floor_samples`
  - `src/audio_clock.rs` の `AudioTimestampClock::observations`
- 境界の扱いが実装ごとに異なる。
  - `TimedValues::prune`、`TimedWindow::prune`、`JitterDelayManager::prune_arrivals`、`AudioTimestampClock::prune` は窓の境界を含む。
  - `TimedValues::since` は境界を含まず、`TimedWindow::min_after` は境界を含む。`AudioReceiveCatchUp::update_floor` の `floor_samples` は境界を含まない。
  - 既存のテストと PBT は `TimedValues` の `prune` と `since` の境界を固定している。他の用途も既存の挙動を変えない。
- `src/playout/catch_up.rs` と `src/playout/publish_catch_up.rs` の `limit_ms` の式 (`max(MIN_MS, floor_ms.unwrap_or(0).saturating_add(GROWTH_MS))`) と、`*_MIN_MS` (60 ms) / `*_GROWTH_MS` (40 ms) / `*_CONFIRM_MS` (100 ms) / `*_COOLDOWN_MS` (1 秒) の値は同じである。
- 一方、床の計算と開始・終了の判定は意図的に異なる。
  - 受信側の床は直近 `AUDIO_RECEIVE_CATCH_UP_FLOOR_WINDOW_MS` (5 秒) の最小値であり、観測が途切れたときは途切れる前の床を 5 秒保つ。開始は「床 + `AUDIO_RECEIVE_CATCH_UP_JUMP_MS` (100 ms) を超えたら即時」または「上限を超えた状態が `AUDIO_RECEIVE_CATCH_UP_CONFIRM_MS` 続いたら」で、クールダウンは最後に始めてから 1 秒である。
  - 配信側の床は観測した遅れの全期間の最小値である。開始は「キューが単独で上限を超えた状態が `AUDIO_PUBLISH_CATCH_UP_QUEUE_CONFIRM_FRAMES` (2 フレーム) 続いたら」または「遅れが上限を超えた状態が `AUDIO_PUBLISH_CATCH_UP_CONFIRM_MS` 続いたら」で、クールダウンは最後に再開してから 1 秒であり、キュー単独の超過には適用しない。
- 影響: 境界の意味の取り違えと、片方だけを直す修正漏れ。

## 設計方針

- ローリング窓を 1 つの `pub(crate)` の内部型 (`VecDeque<(i64, i64)>` ベース) に寄せる。時刻と値の組を保持し、`push` / `prune` / `clear` / イテレータを提供する。
- 各用途の境界の意味は変えない。`prune` は境界を含むものと含まないものがあり、`since` は境界を含まず、`min_after` は境界を含む。共通型のメソッド名または境界の指定でこの違いを明示し、既存のテスト (`snapshot_keeps_a_record_on_the_window_boundary` と PBT の `since` モデル) が固定している挙動を保つ。
- `TimedValues` の `current` / `since` は現在 `&[i64]` を返しているため、共通型のペアのイテレータから値を取り出し、`Vec<i64>` に詰めて `summarize_timings` へ渡す。`summarize_timings` は公開 API であり、`&[i64]` のシグネチャを変えない。
- 最大件数の上限を維持する。`TimedWindow` の `TIMELINE_WINDOW_MAX_ENTRIES` と `JitterDelayManager::arrivals` の `AUDIO_DELAY_MAX_ARRIVALS` は、共通型の任意の上限として指定できるようにする。
- `src/audio_clock.rs` の `observations` と `src/playout/catch_up.rs` の `floor_samples` も共通型へ寄せる。`JitterDelayManager::arrivals` は `(capture_us, arrival_us)`、`AudioTimestampClock::observations` は `(read_wall_clock_us, offset_us)`、`TimedWindow` / `TimedValues` / `floor_samples` は `(時刻, 値)` の組として扱う。
- catch_up の共通化は、`limit_ms` の式と `*_MIN_MS` / `*_GROWTH_MS` / `*_CONFIRM_MS` / `*_COOLDOWN_MS` の定数に限る。床の計算と開始・終了・クールダウンの判定は受信側と配信側で意味が異なるため共通化しない。共通ヘルパに寄せると挙動が変わる。
- 公開 API は変えない。`AudioReceiveCatchUp::limit_ms` (その値が `AudioReceiveCatchUpStats::limit_ms` に入る) と `AudioPublishCatchUp::limit_ms` の値は共通ヘルパで計算する。`AUDIO_RECEIVE_CATCH_UP_*` と `AUDIO_PUBLISH_CATCH_UP_*` の公開定数は残す。

## 変更対象

- 新設する共通の窓型 (例: `src/playout/window.rs`。`mod.rs` は使わない)
- `src/playout/timing.rs`
- `src/playout/timeline.rs`
- `src/playout/delay.rs`
- `src/playout/catch_up.rs`
- `src/playout/publish_catch_up.rs`
- `src/audio_clock.rs`
- 必要なら `src/playout.rs` (`pub(crate) mod window;` の登録)

## 完了条件

- 時刻付き値のローリング窓の保持と切り捨ての実装が共通型 1 箇所に集約され、`TimedValues` の `head` インデックスが削除されていること。
- `limit_ms` の式と `*_MIN_MS` / `*_GROWTH_MS` / `*_CONFIRM_MS` / `*_COOLDOWN_MS` の定数が共通ヘルパ 1 箇所に集約され、受信側と配信側で共有されていること。
- 受信側の床の計算 (5 秒窓と途切れ時の保持) と配信側の床の計算 (全期間の最小)、および開始・終了・クールダウンの判定が変更されていないこと。
- 既存の playout テストと PBT (境界のテストを含む) が通ること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
