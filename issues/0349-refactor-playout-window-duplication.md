# playout のローリング窓と catch_up の重複実装を共通化する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-playout-window-duplication
- Polished: {YYYY-MM-DD}

## 目的

playout には「時刻付き値のローリング窓」が 4 通り、受信側と配信側の catch_up にほぼ同一の floor / limit 計算が 2 通りある。実装ごとに境界の扱いが微妙に違い、修正時に片方だけ直す事故が起きる。共通の内部ヘルパに寄せる。

## 現状

- `src/playout/timing.rs` の `TimedValues` は `times_us` / `values_us` / `head` の 3 つを手で同期する。配列を詰め直さないための最適化だが、窓の件数は数千程度で複雑さに見合わない。
- `src/playout/timeline.rs` の `TimedWindow`、`src/playout/delay.rs` の履歴、`src/audio_clock.rs` の傾き窓が同種の処理を別実装で持つ。
- `src/playout/catch_up.rs` と `src/playout/publish_catch_up.rs` の `limit_ms` の式、floor / growth の定数、confirm / cooldown の判定がほぼ同一である。
- 影響: 挙動の非対称と修正漏れ。

## 設計方針

- ローリング窓を 1 つの内部型 (`VecDeque<(i64, i64)>` ベース) に寄せ、`TimedValues` の head インデックスを削除する。
- catch_up の算術 (floor / limit / growth / confirm) を共通ヘルパに切り出し、受信側と配信側で共有する。I/O の向きの差だけをパラメータにする。
- 公開 API は変えない。

## 完了条件

- 重複実装が 1 箇所に集約され、既存の playout テストと PBT が通ること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
