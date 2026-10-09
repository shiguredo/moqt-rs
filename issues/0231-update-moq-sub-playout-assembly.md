# moq-sub の音声再生の組み立てを最新の再生制御に追従する

- Created: 2026-10-10
- Completed: {YYYY-MM-DD}
- Branch: feature/update-moq-sub-playout-assembly
- Polished: {YYYY-MM-DD}

## 目的

moqt-js は音声再生の組み立てを `src/audioPlayoutSession.ts` に集約し、時間軸への記録・目標の決定・予約・欠落の補間・計器への記録・閉ループへの引き渡しを 1 か所で行う。ライブラリと devtools が別々に持つと、到着基準の遅れ・閉ループ・計器の修正が 2 か所になるためである。

moqt-rs では `examples/moq-sub/src/main.rs` の `play_decoded_audio` / `AudioPlaybackState` / `AudioPlayoutPosition` / `conceal_gap` が同じ組み立てを手書きで持つ。時間軸・スケジューラ・計器・閉ループの移植 (先行する 4 件) に追随できておらず、example から新しい制御を使えない。

## 現状

- `examples/moq-sub/src/main.rs` の `play_decoded_audio` が、時間軸への記録から予約・補間・統計の更新までを持つ
- 到着の基準点 (実際に鳴っている位置) をスケジューラへ渡していない
- 計器 (鳴るはずの時刻・到着・鳴り始めと理由別の捨て) を使っていない
- 目標遅延の閉ループへ観測を渡していない
- 音声出力の時計と壁時計の対応 (moqt-js の `AudioClockBridge` 相当) を、`raw_player` の統計 (`audio_buffer_ms` / `total_samples_played` / `sample_rate`) から作っていない
- `examples/moq-sub/src/jitter_buffer.rs` の `AudioJitterBuffer` は `playout::scheduler` の定数と独自のリードで動く

## 設計方針

ライブラリの新しい API を使う形へ example の組み立てを整理する。組み立ては example 側に置く (ライブラリにデバイス依存を持ち込まない)。

- 時間軸への到着の記録、目標の開始時刻の決定、スケジューラでの予約、欠落の補間、計器への記録を 1 つの関数 (または構造体) にまとめる
- 到着の基準点は `raw_player` の統計から求め、スケジューラへ渡す
- 計器の値を閉ループへ渡し、閉ループが決めた目標を時間軸へ反映する
- 音声出力の時計と壁時計の対応は、出力のバッファ (`audio_buffer_ms`) と再生済みサンプル数から作る。ブラウザの `AudioContext` の時計は存在しないため、この対応を example の境界にする
- 計器と閉ループの値をログに出す (鳴り遅れ、到着から鳴り始めるまでの時間、予定に対する余裕、理由別の捨て、いま使っている目標遅延)
- `jitter_buffer.rs` は新しい入力と判定に追随させる
- 表示用の整形は example 側に置く

## 完了条件

- `examples/moq-sub` がビルドし、起動して音声を再生できること
- 到着の基準点・計器・閉ループが example から使われていること
- 計器と閉ループの値がログで確認できること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

{未着手}
