# subscriber が LOC Timestamp と targetLatency で A/V 同期する

- Created: 2026-09-18
- Completed: {YYYY-MM-DD}
- Branch: feature/add-subscriber-av-sync
- Polished: {YYYY-MM-DD}

## 目的

受信側で音声と映像が別のクロックで再生されているため A/V 同期が成立しない。LOC Timestamp を共通の時間軸として使い、MSF の `targetLatency` に従って再生タイミングを決められるようにする。

## 現状

- `examples/moqt-subscriber/src/main.rs` は映像の PTS に `start_time.elapsed()` (受信時刻) を使う
- `examples/moqt-subscriber/src/decoder.rs` の `DecodedVideoFrame` は PTS を持たない
- 音声は `examples/moqt-subscriber/src/pipeline.rs` の `extract_timestamp_timescale` と `audio_pts_us` で LOC Timestamp から PTS を作る
- カタログの `targetLatency` は未使用
- publisher 側が送る音声・映像の Timestamp が共通軸になるのは 0103 の完了後

## 設計方針

- `DecodedVideoFrame` に PTS を追加し、映像 Object の LOC Timestamp から算出する
- 音声をマスターにして映像の表示タイミングを調整する (libwebrtc の `StreamSynchronization` と同じ考え方)
- 表示時刻は `Timestamp + targetLatency` とする。`targetLatency` はカタログから取得し、無い場合は現在の挙動へフォールバックする
- 同期ずれの計測手段 (推定値のログまたは統計) を用意し、完了条件を判定できるようにする

## 完了条件

- 音声と映像の PTS が同じ epoch マイクロ秒軸で比較できること
- 120 秒程度の連続再生で A/V のずれが許容範囲 (例: ±50 ms) に収まること
- `targetLatency` 未設定時のフォールバックがテストされていること
- publisher 側が 0103 の完了条件を満たしていること
