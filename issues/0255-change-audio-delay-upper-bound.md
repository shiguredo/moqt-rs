# 音声の目標遅延に上限を入れる

- Created: 2026-10-14
- Completed: {YYYY-MM-DD}
- Branch: feature/change-audio-delay-upper-bound
- Polished: {YYYY-MM-DD}

## 目的

moqt-js の `src/audioDelayFeedback.ts` は 2026-10-14 に、実際に使う目標遅延へ上限
(`AUDIO_DELAY_FEEDBACK_MAX_TARGET_MS` の 100 ms) を入れた。

moqt-rs の閉ループ (`src/playout/feedback.rs`) が決める目標は、`targetLatency` が無ければ
300 ms まで上がる。jitter buffer の学習 (`src/playout/delay.rs`) も上限が無く、到着が
大きく跳ねた分をそのまま目標にできる。実測 (moqt-js、実リレー経由) では到着の跳ねの最大が
271 ms あり、その分だけ常に遅れて鳴り、遅延だけが増えていた。目標を 100 ms で切ると、
はみ出した分は受信側の追いつきが吸収する分担になる。

100 ms は到着基準で並べるときの遅れ (`TIMELINE_ARRIVAL_DELAY_US`) と同じ値であり、moqt-js
側も `src/playbackTimeline.ts` の `AUDIO_PLAYOUT_ARRIVAL_DELAY_MS` と同値であることを
コメントで明記している。A/V の合わせ先が 2 つの経路で食い違わないようにするためである。

## 現状

- `src/playout/feedback.rs` の `AudioDelayFeedback::target_delay_us` は、観測が無い間は
  学習値をそのまま返し、観測後は学習値と閉ループの目標の大きい方を返す。どちらにも上限が
  無い
- 閉ループの内部値 (`feedback_target_us`) は [`AUDIO_DELAY_FEEDBACK_MAX_US`] の 300 ms と
  明示設定の `targetLatency` でしか切られない
- `AudioPlayoutScheduler` へ渡す `delay_us` と、時間軸の `learned_delay_us` は
  `target_delay_us` を通るため、上限を足す箇所はこの 1 つで済む

## 設計方針

- `src/playout/feedback.rs` に `AUDIO_DELAY_FEEDBACK_MAX_TARGET_US` (100 ms) を追加する
- `AudioDelayFeedback::target_delay_us` の 2 つの return を `min` で切る
  (`AUDIO_DELAY_FEEDBACK_MAX_TARGET_US`)
- 閉ループの内部値と `AudioDelayFeedback::update` のクランプは触らない。上限は「実際に
  使う値」にだけ掛ける。内部値は 300 ms または `targetLatency` まで上がり得る
- `set_ceiling_us` で渡す `targetLatency` の扱い (閉ループの値にだけ掛け、学習値には
  掛けない) は変えない。100 ms の上限は学習値も含めて掛かる点だけが異なる
- 後方互換は不要 (破壊的変更を恐れない)

## 完了条件

- 観測が無い間も、学習値が 100 ms を超えるときは 100 ms になることがテストで固定されて
  いること
- 観測後も、学習値と閉ループの目標の大きい方が 100 ms を超えるときは 100 ms になることが
  テストで固定されていること
- 閉ループの内部値 (`feedback_target_us`) は 100 ms を超えて上がり得ることをテストで
  固定されていること (上限は実際に使う値にだけ掛かる)
- ちょうど 100 ms は切られないことがテストで固定されていること
- `pbt/tests/prop_playout/feedback.rs` が新しい契約に追随していること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

(対応時に記入)
