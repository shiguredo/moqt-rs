# A/V 同期の不感帯を 5 ms に下げる

- Created: 2026-10-16
- Completed: {YYYY-MM-DD}
- Branch: feature/change-sync-deadband
- Polished: {YYYY-MM-DD}

## 目的

moqt-js は 2026-10-15 の変更 (`origin/develop` の #492) で、A/V 同期の不感帯
(`src/playbackTimeline.ts` の `SYNC_MIN_DELTA_MS`) を 30 ms から 5 ms へ下げた。

実測 (実リレーの同じ条件で、不感帯だけを 30 ms と 5 ms にして交互に 2 回、250 ms ごとに
60 秒観測。`skewMs` は負が「音声が映像より遅れる」側) は次のとおりである。

| 不感帯 | skewMs の p50 | p95 | 絶対値の最大 | 映像の表示待ち p50 | 映像の取りこぼし |
| --- | --- | --- | --- | --- | --- |
| 30 ms | −34 / −35 ms | −15 / −16 ms | 49 / 48 ms | 95 / 96 ms | 18 / 19 |
| 5 ms | −16 / −17 ms | +2 / −13 ms | 24 / 19 ms | 112 / 112 ms | 7 / 19 |

30 ms のままだと、実測のずれ (−34 ms) は ITU-R BT.1359-1 の許容 (音声の遅れ 30 ms、進み
22.5 ms) を外れる。5 ms にすると許容の内側に入る (−16 ms)。代わりに映像は 16 ms 余計に
待つ (95 → 112 ms) が、表示の遅れの上限 (`TIMELINE_MAX_PRESENTATION_DELAY_MS` = 500 ms)
には遠い。映像の取りこぼしは増えていない。

moqt-rs は `TIMELINE_SYNC_MIN_DELTA_US` が 30_000 のままであり、実測で許容を外れるずれが
残り得る。移植する。

## 現状

- `src/playout/timeline.rs` の `TIMELINE_SYNC_MIN_DELTA_US` は 30_000 である。libwebrtc の
  `video/stream_synchronization.cc` の `kMinDeltaMs` と同じ値であることが根拠になっている。
- `src/playout/scheduler.rs` の `AUDIO_PLAYOUT_STRETCH_MIN_US` (20 ms) の rustdoc は
  「時計の対応付けが意図して無視する幅 (`TIMELINE_SYNC_MIN_DELTA_US` の 30 ms) の内側で
  吸収できる遅れである」と書いている。不感帯を 5 ms にするとこの記述は成り立たなくなる。
- moqt-js 側も `src/audioPlayout.ts` の `AUDIO_PLAYOUT_STRETCH_MIN_SECONDS` のコメントに
  30 ms への言及が残っている (不感帯の変更に追随していない)。

## 設計方針

- `TIMELINE_SYNC_MIN_DELTA_US` を 5_000 にする。rustdoc に実測の表と根拠、libwebrtc の
  30 ms をそのまま採らなかった理由を書く。
- `AUDIO_PLAYOUT_STRETCH_MIN_US` の値 (20 ms) は変えない。値の根拠は moqt-js の実測
  (遅れが 20 ms 未満のときは詰めない。60 秒の時間圧縮が 2727 ms から 722〜780 ms へ減り、
  鳴り始めの遅れは 98 ms から 113〜114 ms へ僅かに伸びるだけ) であり、同期の不感帯とは
  独立である。ただし「不感帯の内側」という記述は事実に合わせて直す。moqt-js の追随漏れを
  そのまま写さない。
- 後方互換は不要である (破壊的変更を恐れない)。

## 完了条件

- `TIMELINE_SYNC_MIN_DELTA_US` が 5 ms になること。
- 不感帯の境界 (5 ms ちょうどは動かさない、5 ms を超えたら合わせる) が
  `tests/test_playout/timeline.rs` で固定されていること。
- rustdoc の実測の表、採らなかった 30 ms の理由、`AUDIO_PLAYOUT_STRETCH_MIN_US` の根拠の
  記述が事実と一致していること。
- PBT (`pbt/tests/prop_playout/`) が新しい契約に追随していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
- ソースコードに issue 番号や issue への言及を書かないこと。
