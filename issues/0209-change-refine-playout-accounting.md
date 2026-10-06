# playout の再生制御の会計と測定を仕上げる

- Created: 2026-10-06
- Completed: {YYYY-MM-DD}
- Branch: feature/refine-playout-accounting
- Polished: {YYYY-MM-DD}

## 目的

0103 / 0104 で入れた再生制御に、実装時に判明した小さな穴が残っている。時間圧縮の会計と、A/V のずれの測定を実態に合わせ、example 側の細かな不整合を消す。

## 現状

- `src/playout/scheduler.rs` の `confirm_stretch` は `applied_us.clamp(0, requested_us)` で、要求より多く詰められた分を捨てている。`stretch::compress` は波形の周期 (2.5〜15 ms) 単位でしか削れないため、要求より長く削れた分はスケジューラの内部モデル (前の音の終わり) に反映されず、実際には空いている隙間を隙間と見なさないことがある
- `src/playout/stretch.rs` の `conceal` は無音では何も書かない。聴感は変わらないが、隙間を埋めなかったことが統計に出ない
- `examples/moq-sub/src/main.rs` は `record_presentation` に**予定時刻** (スケジューラが決めた `start_at_us` と、`PlayoutBuffer` が返した `draw_presentation_us`) を渡す。実際に鳴っている位置や表示した時刻ではないため、`skew_us` は「計画が守られたか」しか見ていない
- 同じファイルが、切断時の吐き出しも通常と同じスケジューラの判定に通す。目標より 220 ms 以上先の音は `Drop` になるため、配信の終わりに届いた音を捨てることがある
- 映像の表示待ちの上限が `VIDEO_QUEUED_FRAMES` (24) と `TimelineConfig::video_queue_limit` (24) の 2 か所にある
- `examples/moq-pub` は capture timestamp の換算結果と壁時計の差を毎フレーム `debug` で出すだけであり、0103 の完了条件 (音声と映像の残差の中央値の差が ±20 ms 以内) を確認するには生ログを目で集計する必要がある

## 設計方針

### 時間圧縮の会計

- `confirm_stretch` は、要求より長く詰められた分を「前の音の終わり」から引く。要求より短く詰められた分を足す既存の扱いと対称にする
- `compressed_us` は実際に詰めた長さを記録する (要求値で切らない)

### 無音の隙間

- `stretch::conceal` は無音でも要求ぶんのゼロを書き、埋めた長さを返す。周期が求まらないだけで、隙間を埋めた事実は変わらないためである
- 「周期が求まらない」「継ぎ目の段差が大きい」など、埋められない理由ごとの扱いは変えない

### A/V のずれの測定

- 音声は、再生機器へ積んだ時点の (累積サンプル数, PTS) を記録し、`raw_player::AudioPlayerStats` の `total_samples_played` から**いま鳴っているサンプルの PTS** を求める。これを `record_presentation` に渡す
- 累積サンプル数の対応は、詰めたり補間したりした後の実際のサンプル数で記録する。宣言した PTS からの線形な外挿では、詰めた分だけずれるためである
- 再生位置が戻ったとき (デバイスの作り直し) は対応を捨てて記録しない
- 映像は `PlayoutBuffer` が選んだ 1 枚を実際に再生機器へ渡した時刻を記録する (予定時刻ではない)

### 細かな不整合

- 映像の表示待ちの上限は `TimelineConfig::video_queue_limit` から取る
- 切断時の吐き出しは、目標に従わず到着順で鳴らす (`enforce_target: false`)。末尾を捨てないためである
- `examples/moq-pub` は 5 秒ごとに、トラックごとの残差の最小値と中央値を `info` で出す

## 完了条件

- `confirm_stretch` が要求より長く詰められた分を「前の音の終わり」へ反映することの単体テストがあること
- `conceal` が無音でも要求ぶんを書くことの単体テストがあること
- 音声の実績が、積んだ時点の累積サンプル数と `total_samples_played` から求まることの単体テストがあること
- 切断時の吐き出しが `enforce_target: false` を通ること
- 映像の表示待ちの上限が時間軸の設定から取られていること
- moq-pub が残差の最小値と中央値を定期的に `info` で出すこと
- `make test` / `make pbt` / `make clippy` / `make fmt` が通ること

## 参照

- `src/playout/scheduler.rs` の `confirm_stretch` / `AudioPlayoutScheduler`
- `src/playout/stretch.rs` の `conceal`
- `raw_player::AudioPlayerStats` の `total_samples_enqueued` / `total_samples_played`
- draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.11 (Render group)
