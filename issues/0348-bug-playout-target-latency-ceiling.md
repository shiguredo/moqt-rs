# PlayoutTimeline::with_config でも targetLatency を閉ループの上限にする

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-playout-target-latency-ceiling
- Polished: 2026-10-21

## 目的

`PlayoutTimeline::set_target_latency_ms` は、明示された targetLatency を音声の閉ループ遅延の上限として `AudioDelayFeedback::set_ceiling_us` に設定する。同じ値を `TimelineConfig::target_latency_ms` で構築時に渡した場合、`with_config` は値を保持するだけで上限を設定しない。構築経路によって挙動が変わるのは API 契約として不正である。

## 現状

- `src/playout/timeline.rs` の `with_config` は `config.target_latency_ms` を 0 未満なら 0 に丸めて `self.config` に入れ、`AudioDelayFeedback::new()` をそのまま使う。
- `set_target_latency_ms` は同じ値から `set_ceiling_us` を呼び、明示値より閉ループが大きくならないようにする。
- `with_config(TimelineConfig { target_latency_ms: 90, .. })` では閉ループが既定の上限 (最大 160 ms) まで育ち得る。
- 閉ループの上限は setter 経路でしか固定されていない。
- 影響: 正の targetLatency が既定の上限 (100 ms、広げても 160 ms) より小さい場合、構築時にその値を渡した利用者だけ、閉ループが宣言した上限を超える (揺らぎの学習値は従来どおり明示値の対象外であり、この issue では変更しない)。

## 設計方針

- `with_config` からも同じ設定処理を呼び、`set_ceiling_us` と `refresh_unshared_state` / `align_sync_extras` を適用する。共通の内部ヘルパに集約する。構築時はまだ観測が無いため `refresh_unshared_state` / `align_sync_extras` は実質的に何もしないが、setter と同じ経路を通すことで将来の設定処理の変更にも追随させる。
- `with_config` の doc に、targetLatency が閉ループの上限になることと、0 以下は上限を外すことを明記する。
- 両経路で同じ結果になることをテストで固定する。

## 完了条件

- `with_config` に正の targetLatency を渡した場合も閉ループの上限が設定されること。`delay_breakdown().audio_delay_feedback.ceiling_us` が `Some(target_latency_ms.saturating_mul(1_000))` になることをテストで固定する。0 以下は setter と同じく上限を外す (`None`) ことも固定する。
- setter 経路との等価性がテストで固定されていること。`PlayoutTimeline::new()` で作った時間軸に `set_target_latency_ms` を呼んだ場合と `with_config`
  に同じ値を渡した場合で、音声の `observe` と、閉ループの目標が targetLatency を超えようとする `observe_audio_playout` を含む同じ観測列に対する
  `ceiling_us` / `target_us` / `learned_delay_us` / `presentation_delay_us` が一致すること。0 と負の値のときも両経路で
  `ceiling_us` が `None` になること。
- `with_config` の doc に、targetLatency が閉ループの上限になることと、0 以下は上限を外すことが書かれていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が通ること。
