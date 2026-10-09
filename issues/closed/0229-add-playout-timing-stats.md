# 音声再生の計器 (鳴るはずの時刻・到着・鳴り始めと理由別の捨て) を追加する

- Created: 2026-10-10
- Completed: 2026-10-10
- Branch: feature/add-playout-timing-stats
- Polished: {YYYY-MM-DD}

## 目的

moqt-js は `src/audioPlayoutTimingStats.ts` に、音を鳴るはずの時刻・届いた時刻・鳴り始める時刻の組を記録し、予定に対する余裕 (slack) と到着から鳴り始めるまでの時間、鳴らなかった量を理由別に出す計器を持つ。これは目標遅延の閉ループ (後続の issue) の入力であり、購読の状態を解析するための値でもある。

moqt-rs の `src/playout/scheduler.rs` は `lateness_us` の直近値と `drops` の累積しか持たず、分布も理由別の会計も無い。このため「並べすぎで捨てたのか、目標から離れすぎて捨てたのか」も「到着から鳴り始めるまでに何 ms かかったのか」も分からない。

## 現状

- `AudioPlayoutScheduler` は `lateness_us` / `drops` / `compressed_us` / `concealments` / `concealed_us` の累積と直近値だけを持ち、分布 (p50 / p95 / max) を持たない
- 到着から鳴り始めるまでの時間 (start delay) と、予定に対する余裕 (slack) を記録していない
- 捨ての理由を区別していない。moqt-js は `backlog` (並べすぎ) / `catchUp` (relay の cache から追いつく途中) / `error` (鳴らす準備の失敗) / `stopped` (再生の停止) の 4 つを持つ
- 捨てた長さの累積を持たない
- 直前の issue で入る `basis` (目標の時刻で並べたか到着基準で並べたか) を計器へ渡す経路が無い
- `src/playout/timeline.rs` の `TimedWindow` は値の列だけで、直近の窓の要約 (中央値 / 95 分位 / 最大) を持たない

## 設計方針

`src/playout/timing.rs` を追加し、moqt-js の `AudioPlayoutTimingStats` と `src/timingSummary.ts` を移植する。時刻はすべて引数で受け取り、デバイス API やタイマーには触れない (Sans-I/O)。計器はライブラリに置く (組み立ては example 側に置く方針のため、example から使える公開 API とする)。

- 公開する: `AUDIO_PLAYOUT_TIMING_WINDOW_US` (10 秒)、`MAX_RECENT_AUDIO_MISSES` (30)、`AudioMissReason` (`Backlog` / `CatchUp` / `Error` / `Stopped`)、`AudioMissTotal`、`AudioMissEvent`、`AudioMiss`、`AudioPlayoutTimingSnapshot`、`AudioPlayoutTimingStats`
- メソッドは `record_play` / `record_miss` / `record_stopped` / `audio_delay_feedback` / `snapshot` / `reset` とする
- 分布は直近の窓から求め、窓の外に出た記録は分布から落とす。捨ての累積 (件数と長さ) は窓に関係なく残す
- 直近のイベントは上限 30 件とし、古い方から捨てる
- `TimingSummary` (`p50` / `p95` / `max`) と要約の関数を置く
- `AudioPlayoutScheduler` に `start_delay_us` / `slack_us` と `basis` を記録する欄を足し、計器へ渡せるようにする。捨ての理由も `Backlog` として返す
- 表示用の整形 (moqt-js の `formatAudioMissEvent` の UTC ISO 8601) は移植しない。ログの整形は example 側の責務とし、ライブラリに時刻整形の依存を増やさない
- `audio_delay_feedback` は閉ループが読む短い窓 (1 秒) の中央値と、累積の捨ての件数・長さを返す
- 後方互換は不要 (破壊的変更を恐れない)

## 完了条件

- 記録と分布 (窓の内外、`MAX_RECENT_AUDIO_MISSES` の上限制、`reset`) と理由別の累積がテストで固定されていること
- `record_stopped` が、鳴り終わった音を数えず鳴り始めている音は残りの長さだけ数えることが固定されていること
- `audio_delay_feedback` が短い窓の分布と累積の捨て量を返すことが固定されていること
- `basis` に応じて到着基準の計画と計画なしを数え分けることが固定されていること
- PBT を追加し、分岐ごとにカバレッジゲートがあること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

- `src/playout/timing.rs` を追加し、moqt-js の `src/audioPlayoutTimingStats.ts` と `src/timingSummary.ts` を移植した。時刻はすべて引数で受ける (Sans-I/O)
- 公開した: `AUDIO_PLAYOUT_TIMING_WINDOW_US` (10 秒)、`MAX_RECENT_AUDIO_MISSES` (30)、`AUDIO_DELAY_FEEDBACK_WINDOW_US` (1 秒)、`AudioMissReason` (`Backlog` / `CatchUp` / `Error` / `Stopped`)、`AudioMissTotal` / `AudioMissTotals`、`AudioPlayoutMiss`、`AudioMissEvent`、`TimingSummary` と `summarize_timings`、`AudioPlayoutTimingSnapshot`、`AudioDelayFeedbackObservation`、`AudioPlayoutTimingStats`
- `AudioPlayoutTimingStats` は `record_play` / `record_miss` / `record_stopped` / `audio_delay_feedback` / `snapshot` / `reset` を持つ。分布は直近 10 秒の窓から求め、捨ての累積は窓に関係なく残す。直近のイベントは 30 件を上限とする
- `record_stopped` は鳴り終わった音を数えず、鳴り始めている音は残りの長さだけを数える
- `audio_delay_feedback` は閉ループ (後続の issue) が読む短い窓の分布と、累積の捨てた件数・長さを返す
- `AudioPlayoutScheduler` に `AudioPlayoutPlay` と `last_play` を追加し、鳴らすと決めた音 (到着・目標・鳴り始める時刻・実際に鳴る長さ・計画) をそのまま計器へ渡せるようにした。`confirm_stretch` は実際に詰めた長さで鳴る長さを直す
- `src/playout/timeline.rs` の `percentile_index` を `pub(crate)` にし、映像の表示の遅れと同じ nearest-rank 法を共用する
- 表示のための整形 (moqt-js の `formatAudioMissEvent` の UTC ISO 8601) は移植しない。記録の時刻は呼び出し側が渡した軸のままとし、ログの整形は example 側の責務にする
- テスト: `tests/test_playout/timing.rs` に 19 件 (記録と分布、窓の内外と境目、理由別の累積、直近一覧の上限、`record_stopped`、`audio_delay_feedback`、`reset`、nearest-rank、スケジューラ連携)、`tests/test_playout/scheduler.rs` に `last_play` の 4 件、`pbt/tests/prop_playout/timing.rs` に 2 件 (モデル照合と分岐のカバレッジゲート、短い窓の閉ループ観測) を追加した
- 検証: `cargo fmt --check` / `make pbt` / `make test` / `cargo clippy --workspace --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc` が成功した。`PBT_SEED` を 1 / 42 / 2026 に変えても property が成功する。`src/playout/timing.rs` の行カバレッジは 100% である
- 検証: 9 種類の欠陥 (窓の prune の無効化 / 直近一覧の上限 / 停止時の長さの数え方 / 計画なしの判定の反転 / 閉ループの窓の取り違え / p95 の添字 / 詰めの反映 3 種) を入れて、追加したテストと property がそれぞれ検出することを確認した
- 備考: `record_play` は `AudioPlayoutPlay` を 1 つ受け取る形にした。`played_us` は `confirm_stretch` を呼んだ後に読む契約であり、呼ばずに次の `schedule` を呼んだ場合は適用されなかったものとして音の長さ全体になる (doc に明記)
