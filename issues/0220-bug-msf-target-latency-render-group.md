# isLive が FALSE の track の targetLatency を無視する

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-target-latency-islive
- Polished: {YYYY-MM-DD}
- Updated: 2026-10-11

## 目的

draft-ietf-moq-msf-01 §5.2.8 (Target latency) は「If isLive is FALSE, this field MUST be ignored.」
と MUST を定める。isLive が FALSE の track は VOD、または終了した live track であり、その
`targetLatency` は表示の遅れの下限として使ってはならない。`catalog_target_latency_ms` は `isLive` を
見ずに `targetLatency` を反映しており、decode 層の正規化を通らないカタログでは MUST を満たさない。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `catalog_target_latency_ms` は `MsfTrack::target_latency` を
  持つ track を集めて最大値を返し、`MsfTrack::is_live` を参照しない。
- 返した値は同じファイルの `run` が `target_latency_ms` へ格納し、`src/playout/timeline.rs` の時間軸
  (`set_target_latency_ms`。`examples/moq-sub/src/main.rs` は値を渡すだけ) が A/V 同期の基準と
  「表示の遅れの下限」、および音声目標遅延の閉ループの上限として使う。
- `MsfCatalogDocument::decode` は isLive=false の track の `targetLatency` を `None` に正規化する
  (`src/msf.rs` の `decode_track` / `decode_clone_track` / `MsfTrackUpdate::into_track`、
  `tests/test_msf/track_fields.rs` の `target_latency_ignored_when_is_live_false` で固定)。moq-sub は
  decode 経由でしかカタログを受け取らないため、§5.2.8 の MUST は library 側で既に満たされている。
- そのため、`catalog_target_latency_ms` が `is_live` を見ないことは、decode を通らないカタログ
  (テストでの手組みなど) でのみ問題になる。isLive が FALSE の track だけが大きな `targetLatency` を
  宣言しているカタログを decode した場合、購読している live track の表示がその値まで遅れることはない。
- 同ファイルの track 選択は video / audio を codec などから選ぶが、`is_live` は見ていない。

## 設計方針

- `catalog_target_latency_ms` の対象を `is_live` が true の track に限る (decode 層の正規化に対する
  二重の防御。library 側の正規化は変えない)。
- §5.2.8 は同じ render group / alternate group の track が同一の targetLatency を持つ MUST も
  定める。値を「購読する track の renderGroup に属する track」から決める改善は、値の選択規則を
  変える別目的の変更になるため本 issue では扱わない。
- 値が無いときの現行挙動 (`None` を返す。0230 以降、時間軸は音声の目標遅延を閉ループの観測から
  決め、上限指定は無い) は変えない。
- track の選択から isLive が FALSE の track を除外するかは、選択規則の変更になるため本 issue
  では扱わない (必要なら別 issue にする)。

## 完了条件

- isLive が FALSE の track だけが持つ `targetLatency` を無視することのテスト。
- isLive が true の track の `targetLatency` は従来どおり反映されることのテスト。
- `targetLatency` を持つ track が 1 つも無いときに `None` を返すことのテスト。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
