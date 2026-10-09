# isLive が FALSE の track の targetLatency を無視する

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-target-latency-islive
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-01 §5.2.8 (Target latency) は「If isLive is FALSE, this field MUST be ignored.」
と MUST を定める。isLive が FALSE の track は VOD、または終了した live track であり、その
`targetLatency` は表示の遅れの下限として使ってはならない。現状の moq-sub は `isLive` を見ずに
`targetLatency` を反映しており、MUST を満たしていない。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `catalog_target_latency_ms` は `MsfTrack::target_latency` を
  持つ track を集めて最大値を返し、`MsfTrack::is_live` を参照しない。
- 返した値は同じファイルの `run` が `target_latency_ms` へ格納し、プレイヤーの時間軸
  (`examples/moq-sub/src/main.rs` の表示待ちの計算) が「表示の遅れの下限」として使う。
- そのため、isLive が FALSE の track だけが大きな `targetLatency` を宣言している場合、
  購読している live track の表示がその値まで遅れる。
- 同ファイルの track 選択は video / audio を codec などから選ぶが、`is_live` は見ていない。

## 設計方針

- `catalog_target_latency_ms` の対象を `is_live` が true の track に限る。
- §5.2.8 は同じ render group / alternate group の track が同一の targetLatency を持つ MUST も
  定める。値を「購読する track の renderGroup に属する track」から決める改善は、値の選択規則を
  変える別目的の変更になるため本 issue では扱わない。
- 値が無いときの現行挙動 (`None` を返し、時間軸は自分の揺らぎから求めた遅れだけを使う) は
  変えない。
- track の選択から isLive が FALSE の track を除外するかは、選択規則の変更になるため本 issue
  では扱わない (必要なら別 issue にする)。

## 完了条件

- isLive が FALSE の track だけが持つ `targetLatency` を無視することのテスト。
- isLive が true の track の `targetLatency` は従来どおり反映されることのテスト。
- `targetLatency` を持つ track が 1 つも無いときに `None` を返すことのテスト。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
