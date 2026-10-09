# 音声再生の計器 (鳴るはずの時刻・到着・鳴り始めと理由別の捨て) を追加する

- Created: 2026-10-10
- Completed: {YYYY-MM-DD}
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

{未着手}
