# PlayoutTimeline::with_config と set_target_latency_ms で targetLatency の適用を一致させる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-playout-target-latency-consistency
- Polished: 2026-11-21

## 目的

`PlayoutTimeline::set_target_latency_ms` と `PlayoutTimeline::with_config` は、どちらも
targetLatency を音声と映像の表示の遅れの下限として使うが、設定処理が一致していない。
`with_config` は `target_latency_ms` を保持するだけで `update_limited_us` を呼ばないため、
targetLatency が表示の遅れの上限を超える場合に構築直後の `target_latency_limited_us` が
setter 経路と異なる。また `set_target_latency_ms` は `config.target_latency_ms` を更新しないため、
`config()` が古い値を返す。構築経路によって公開 API の値が変わるのは API 契約として不正である。

## 現状

- 2026-11-01 の moqt-js 追従で閉ループ (`AudioDelayFeedback`) は撤去され、targetLatency は音声の閉ループの上限ではなく、音声と映像の表示の遅れの下限になった (`natural_delay_us_of`)。`with_config` は `target_latency_ms` を保持するため下限自体は適用される。
- `src/playout/timeline.rs` の `with_config` は `config.target_latency_ms` を 0 未満なら 0 に丸めて `self.config` と `target_latency_ms` に入れ、`limited_us` を 0 のままにする。
- `set_target_latency_ms` は `target_latency_ms` を 0 未満なら 0 に丸めて設定し、`refresh_unshared_state`、`sync_natural_presentation_us` がある場合の `align_sync_extras`、`update_limited_us` を呼ぶ。`config.target_latency_ms` は更新しない。`config()` の doc は「使っている設定」としているが、setter 経路では古い値を返す。
- そのため、`with_config(TimelineConfig { target_latency_ms: 600, ..TimelineConfig::default() })` の構築直後は `target_latency_limited_us()` が 0 だが、`PlayoutTimeline::new()` に `set_target_latency_ms(600)` を呼ぶと 100_000 になる (既定の表示の遅れの上限 500 ms を超える分)。
- `with_config` で targetLatency 90 を渡した場合と `new()` + `set_target_latency_ms(90)` では、`config().target_latency_ms` が 90 と 0 で異なる。
- 観測を始めた後は `observe` が `update_limited_us` を呼ぶため `target_latency_limited_us` は一致するが、構築直後の `target_latency_limited_us` と `config()` は setter 経路と一致しない。
- targetLatency の設定経路を直接確認する単体テストは、setter 経路の
  `target_latency_is_the_lower_bound_for_both_tracks` と、with_config の負値
  (`negative_config_values_are_clamped`) だけであり、両経路の等価性を固定していない。
  PBT の `the_audio_budget_uses_the_documented_constants` は with_config の正の値を使うが、
  targetLatency については観測後の `target_latency_limited_us` だけを確認している。
- 影響: 構築時に targetLatency を渡した利用者だけ、構築直後の計器と `config()` の値が setter 経路と異なる。

## 設計方針

- targetLatency の設定処理を private ヘルパ (`apply_target_latency_ms` など) に集約する。ヘルパは `config.target_latency_ms` と `target_latency_ms` を同じ値にし、`refresh_unshared_state`、`sync_natural_presentation_us` がある場合の `align_sync_extras`、`update_limited_us` を呼ぶ。
- `with_config` は `Self` を構築した後にヘルパを呼ぶ。`set_target_latency_ms` もヘルパを呼ぶ。これにより、構築直後の `target_latency_limited_us` と `config()` が setter 経路と一致する。
- `with_config` の doc に、targetLatency が表示の遅れの下限になることと、0 以下は下限を外すことを明記する。
- `TimelineConfig::target_latency_ms` の doc の「0 のとき」を「0 以下」に合わせる。
- `set_target_latency_ms` の doc に、0 以下の扱いと `config()` への反映を明記する。

## 変更対象

- `src/playout/timeline.rs`
- `tests/test_playout/timeline.rs`

`docs/audio-playout.md` と `skills/shiguredo-moqt/SKILL.md` は、targetLatency を表示の遅れの下限として扱う現行の記述のままでよいため変更しない。

## 完了条件

- `with_config` と `set_target_latency_ms` のどちらで targetLatency を設定しても、
  正の値 (表示の遅れの上限を超える値を含む) で両経路の
  `target_latency_ms()` / `config().target_latency_ms` / `target_latency_limited_us()`
  が一致すること。0 以下は両経路とも `target_latency_ms()` と
  `config().target_latency_ms` が 0 になり、`target_latency_limited_us()` も一致すること。
- `with_config` と `new()` + `set_target_latency_ms` で同じ targetLatency を設定したとき、同じ観測列に対する `presentation_delay_us` / `presentation_extra_delay_us` / `video_delay_us` / `target_latency_limited_us` / `audio_delay_stats().target_us` が一致すること。
- `with_config` の doc に、targetLatency が表示の遅れの下限になることと、0 以下は下限を外すことが書かれていること。
- `TimelineConfig::target_latency_ms` の doc に、0 以下の扱いが書かれていること。
- `set_target_latency_ms` の doc に、0 以下の扱いと `config()` への反映が書かれていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が通ること。
