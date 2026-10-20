# 音声遅延の閉ループにアンダーランを入力する

- Created: 2026-10-20
- Completed: 2026-10-20
- Branch: develop
- Polished: {YYYY-MM-DD}
- Reporter: @voluntas

## 目的

隙間のうち予約できなかった分（アンダーラン）は事後には埋められない。現在の閉ループは実際に鳴った結果の遅れ・並べすぎの捨て・時間圧縮だけを見ており、出力が枯れた事実を使っていない。アンダーランが起きたら目標遅延を上げ、起きていない状態が続けば既存の規則と同じ速さで戻す。

## 現状

- `src/playout/feedback.rs` の `AudioDelayFeedback::update` は、観測の遅れ（p50）、並べすぎの捨て、時間圧縮の増分だけを目標の増減に使う。
- `src/playout/timing.rs` の `AudioDelayFeedbackObservation` に、出力が枯れた量を渡す経路が無い。
- 5 ms 以下の隙間や補間失敗はフェード付き無音で埋めるが、到着の揺らぎが目標遅延を超えた場合の穴は目標遅延を上げる以外に減らせない。

## 設計方針

- `AudioDelayFeedbackObservation` に `underrun_ms` を足す。example は出力の枯れを検出したら、前回の更新からの合計を渡す。
- `AudioDelayFeedbackReason` に `Underrun` を足す。`underrun_ms` が正のときは、既存の増加判断と独立に `min(underrun_ms, AUDIO_DELAY_FEEDBACK_MAX_STEP_MS)` だけ目標を上げる。既存の増加があれば大きい方を使う。
- 適用上限（明示 `targetLatency`）、拡大上限 160 ms、毎秒 1 回、戻す速さは既存の規則を変えない。
- 5 秒ログにアンダーランの回数・合計と、閉ループの理由を出す。

## 完了条件

- アンダーランだけが起きたときに目標が上がること、上限を超えないこと、アンダーランが無ければ従来と同じ結果になることを単体テストと PBT で固定する。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 参照

- `src/playout/feedback.rs` の `AudioDelayFeedback::update` / `AudioDelayFeedbackReason` / `AudioDelayFeedbackObservation`
- `src/playout/timing.rs` の `AudioPlayoutTimingStats::audio_delay_feedback`
- `src/playout/timeline.rs` の `PlayoutTimeline::observe_audio_playout`

## 解決方法

- `AudioDelayFeedbackObservation` に `underrun_ms` を、`AudioDelayFeedbackReason` に `Underrun` を追加した。
- 閉ループは `underrun_ms` が正のとき、既存の増加判断と独立に `min(underrun_ms, 最大増分)` だけ目標を上げる。既存の増加があるときは大きい方を使い、上限・毎秒 1 回・戻す速さは既存の規則を変えない。
- example は枯れが明けた長さを前回の観測からの増分にして渡す。
- 単体テストと PBT で、枯れだけで目標が上がること、上限を超えないこと、枯れが無ければ従来と同じであることを固定した。
