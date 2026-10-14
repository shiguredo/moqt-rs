# A/V 同期で足す遅延に上限を入れる

- Created: 2026-10-14
- Completed: {YYYY-MM-DD}
- Branch: feature/change-sync-extra-cap
- Polished: {YYYY-MM-DD}

## 目的

moqt-js の `src/playbackTimeline.ts` は 2026-10-13 に、A/V 同期で先行する側へ足す遅延を
上限でクランプするようにした (実測で映像へ 102.18 ms 足して上限の 100 ms を超えていた)。

moqt-rs の `PlayoutTimeline::align_sync_extras` にも同じ欠陥がある。ずれを目標の差へ
収めるとき、既に足している分を含めた表示時刻から目標を計算するため、相手側 (後行側) へ
足す量が `TIMELINE_MAX_COMPENSATED_DIFFERENCE_US` (100 ms) を超え得る。超えた分は合わせても
実際のずれは減らないまま、そのトラックの表示の遅れだけが伸びる。

## 現状

- `src/playout/timeline.rs` の `align_sync_extras` は、`target_us` から素の表示の遅れ
  (`natural_us`) を引いた値をそのまま `sync_extra_us` へ代入する。クランプが無い
- `TIMELINE_MAX_COMPENSATED_DIFFERENCE_US` は `desired_difference_us` の中で使われており、
  「素の値の差」から目標の差を決める経路は上限に収まる。しかし `align_sync_extras` は
  「既に足している分を含む表示時刻」の差から `target_us` を求めるため、上限を超え得る
- 再現する入力の例: 音声の素の表示の遅れが 1,000,000 µs、映像が 0 µs、音声へ既に
  500,000 µs 足している。`desired_difference_us` は 900,000 µs、`target_us` は
  1,500,000 − 900,000 = 600,000 µs になり、映像へ 600,000 µs を足してしまう
- 超過した分は `TrackBreakdown::sync_extra_delay_us` に現れるため、計器からは読める。
  止める仕組みだけが無い

## 設計方針

- `PlayoutTimeline::align_sync_extras` の 2 つの代入を、`target_us` から `natural_us` を
  引いた値と `TIMELINE_MAX_COMPENSATED_DIFFERENCE_US` の小さい方にする
- 足りない分は相手側の戻し (`decay_sync_extras`) に任せ、A/V のずれとして残す
- 定数は既にあるものを使い、新しい定数は作らない
- 後方互換は不要 (破壊的変更を恐れない)

## 完了条件

- 同期で足す量が `TIMELINE_MAX_COMPENSATED_DIFFERENCE_US` を超えないことがテストで
  固定されていること
- 上限に届かない場合は、ずれを目標の差へ収める従来の挙動が変わらないことがテストで
  固定されていること
- `pbt/tests/prop_playout/timeline.rs` の生成器が上限超過を踏む入力を含み、property が
  その不変条件を検証していること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

(対応時に記入)
