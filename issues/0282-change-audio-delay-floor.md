# 音声の再生遅延の下限を 50 ms に下げる

- Created: 2026-10-16
- Completed: {YYYY-MM-DD}
- Branch: feature/change-audio-delay-floor
- Polished: {YYYY-MM-DD}

## 目的

moqt-js は 2026-10-15 の変更 (`origin/develop` の #492「音声の再生遅延と A/V 同期を実測で
決め直し、パケロス対策を入れる」) で、音声の再生遅延の下限を実リレーの実測から 80 ms から
50 ms へ下げた。対象は `src/playbackTimeline.ts` の `AUDIO_PLAYOUT_DELAY_FLOOR_MS` と
`src/audioDelayFeedback.ts` の `AUDIO_DELAY_FEEDBACK_MIN_MS` である。

実測 (実リレー、60 秒の配信と購読、下限だけを変えて交互に 3 回) では、適用中の目標は揺らぎの
学習が下限を超えていなければ下限そのものになり、次のようになった。

| 下限 | 適用中の目標 | 鳴り始め p50 | 時間圧縮 / 60 秒 | 補間 | 基準の取り直し |
| --- | --- | --- | --- | --- | --- |
| 80 ms | 80 ms | 78.6〜95.3 | 5〜9 ms | 0〜17 ms | 0 |
| 50 ms | 50 ms | 45.6〜67.7 | 32〜65 ms | 0〜8 ms | 0 |
| 30 ms | 30〜40 ms | 34.6〜56.8 | 496〜1143 ms | 0〜48 ms | 3 回中 1 回で 2 |

50 ms は時間圧縮を 60 秒で 65 ms 以下 (鳴らす音の 0.1 % 程度) に保ったまま 80 ms より 30 ms
低い。30 ms は圧縮が 1 桁増えるため採らない。moqt-rs は 80 ms のままであり、経路が静かな
ときに 30 ms 余分に遅れて鳴る。移植する。

## 現状

- `src/playout/timeline.rs` の `TIMELINE_AUDIO_DELAY_FLOOR_US` は 80_000 である。
  `audio_arrival_delay_us` が学習した遅れをこの値で下限にする。
- `src/playout/feedback.rs` の `AUDIO_DELAY_FEEDBACK_MIN_US` は 80_000 である。閉ループが
  決める目標はこの値で下限になる。moqt-js の `AUDIO_DELAY_FEEDBACK_MIN_MS` は同じ役割で
  あり、閉ループの値が到着基準の遅れと映像の合わせ先の両方に効くため、片方だけを変えると
  基準を共有できない状態で A/V のずれが残る。
- `src/playout/scheduler.rs` の `AUDIO_PLAYOUT_DELAY_US` も 80_000 である。moqt-js の
  `src/audioPlayout.ts` の `AUDIO_PLAYOUT_DELAY_SECONDS` は下限 (`AUDIO_PLAYOUT_DELAY_FLOOR_MS`)
  から導出されており、揺らぎの学習がまだ無いときの再生の遅れという同じ役割を持つ。Rust 側は
  独立した定数で 80 ms を持っているため追随させる必要がある。
- `src/playout/scheduler.rs` の `AUDIO_PLAYOUT_BACKLOG_US` は 220_000 である。moqt-js では
  `AUDIO_PLAYOUT_MAX_DELAY_SECONDS` (0.3) から下限を引いた `AUDIO_PLAYOUT_BACKLOG_SECONDS`
  であり、下限を 50 ms にすると 250 ms になる。並べすぎとみなす「再生の遅れ + 余裕」の合計は
  300 ms のままである。
- `examples/moq-sub/src/main.rs` の `arrange` のコメントは到着基準の遅れを「[80 ms, 100 ms]」
  と書いている。
- `src/playout/delay.rs` の `AUDIO_DELAY_START_MS` (80 ms) は揺らぎの学習の初期値である。
  moqt-js も `AUDIO_DELAY_START_MS` は 80 ms のままにしており、観測が 1 つでも届けば学習が
  実測から目標を動かすため、下限とは揃えない。

## 設計方針

- 3 つの下限 (`TIMELINE_AUDIO_DELAY_FLOOR_US` / `AUDIO_DELAY_FEEDBACK_MIN_US` /
  `AUDIO_PLAYOUT_DELAY_US`) を 50_000 にする。
- `AUDIO_PLAYOUT_BACKLOG_US` は 300_000 から `AUDIO_PLAYOUT_DELAY_US` を引いた値にする
  (moqt-js と同じ導出。並べすぎの合計を 300 ms に保つ)。
- `AUDIO_DELAY_START_MS` と上限 (`TIMELINE_ARRIVAL_DELAY_US` = 100 ms、
  `AUDIO_DELAY_FEEDBACK_MAX_TARGET_US` = 100 ms) は変えない。
- rustdoc に実測の表と根拠、棄却した 30 ms の理由を書く (moqt-js は同じ内容をソースの
  コメントに置いている)。
- 後方互換は不要である (破壊的変更を恐れない)。

## 完了条件

- 3 つの下限が 50 ms になり、`AUDIO_PLAYOUT_BACKLOG_US` が 250 ms になること。
- 上限 (100 ms) と `AUDIO_DELAY_START_MS` (80 ms) が変わらないこと。
- 下限の境界 (ちょうど 50 ms は切られない、50 ms 未満は 50 ms になる) が
  `tests/test_playout/{timeline,feedback,scheduler}.rs` で固定されていること。
- 並べすぎの判定が 250 ms の余裕で行われることがテストで固定されていること。
- PBT (`pbt/tests/prop_playout/`) が新しい契約に追随していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
- ソースコードに issue 番号や issue への言及を書かないこと。
