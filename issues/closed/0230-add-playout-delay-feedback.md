# 音声の目標遅延を実際に鳴った結果から閉ループで決める

- Created: 2026-10-10
- Completed: 2026-10-10
- Branch: feature/add-playout-delay-feedback
- Polished: {YYYY-MM-DD}

## 目的

moqt-js は `src/audioDelayFeedback.ts` を追加し、実際に鳴った結果 (鳴り遅れ、到着から鳴り始めるまでの時間、並べすぎで捨てた累積) から目標遅延を増減する閉ループを持つ。

jitter buffer の学習 (`src/audioDelayManager.ts`) は基準が窓内で最も早い観測へ動くため、ストリーム全体が一様に遅れている分を見ない。音声の TIMESTAMP が段差でずれると、その段差を到着の揺らぎとして学習し、目標が実測で 700 ms まで膨らみ、映像の表示まで遅れていた。moqt-rs の `src/playout/delay.rs` は学習値だけを使っており、同じ問題が残っている。

## 現状

- `src/playout/delay.rs` の `JitterDelayManager` は到着の遅れの 0.95 分位から目標を求めるだけで、実際に鳴った結果を見ない
- `src/playout/timeline.rs` の `observe` は音声の表示の遅れを `delay.target_delay_ms()` だけで決める
- `set_target_latency_ms` で受けた `targetLatency` を閉ループの上限として渡す口が無い
- 計器 (前の issue) の短い窓の観測を渡す口が無い
- 基準を共有しないと判定した後に、足した遅延を戻す経路が無い (前の issue で対応する)

## 設計方針

`src/playout/feedback.rs` を追加し、moqt-js の `AudioDelayFeedback` を移植する。時刻は引数で受ける。

- 定数 (すべてミリ秒): 下限 80 / 上限 300 / 初期 100 / 調整の間隔 1 秒 / 観測の窓 1 秒 / 許容 10 / 余白 20 / 増分の下限 20 / 増分の上限 40 / 毎秒の減少量 10
- 公開する型: `AudioDelayFeedbackReason` (`Initial` / `Backlog` / `Lateness` / `Settled` / `Waiting`)、`AudioDelayFeedbackObservation`、`AudioDelayFeedbackSnapshot`
- 公開するメソッド: `set_ceiling_ms` / `ceiling` / `feedback_target_ms` / `target_delay_ms` / `update` / `snapshot` / `reset`
- 目標は jitter buffer の学習値との大きい方を採る。実際に鳴った観測を 1 つも受けていない間は学習値をそのまま返す
- 調整は毎秒 1 回だけ行う。鳴り遅れ (許容 10 ms 超) があれば増分を [20, 40] ms にクランプして増やし、並べすぎで捨てた累積が増えていれば余白 20 ms を足して同じ範囲にクランプして増やす。どちらも無ければ毎秒 10 ms の速さで減らし、負にはしない
- `targetLatency` は閉ループの目標にだけ上限として掛ける。学習値には掛けない
- `PlayoutTimeline` に `observe_audio_playout` を追加し、観測を閉ループへ渡して音声の表示の遅れをその場で取り直す。`delay_breakdown` に閉ループの状態を足す
- 購読のやり直し (`reset_track`) では学習値だけを消し、閉ループは消さない (購読のやり直しで目標を戻さない)
- 閉ループの入力は前の issue の計器から作る。計器の値が無い間は学習値のままとする
- 後方互換は不要 (破壊的変更を恐れない)

## 完了条件

- 観測が無い間は学習値を使い、観測を受けた後に閉ループの値との大きい方を使うことがテストで固定されていること
- 増減の規則 (毎秒 1 回、鳴り遅れと並べすぎでの増分のクランプ、許容内での毎秒 10 ms の減少、負にしない、下限 80 / 上限 300) が固定されていること
- `targetLatency` が閉ループの目標にだけ上限として掛かることが固定されていること
- 購読のやり直しで閉ループが消えないことが固定されていること
- PBT を追加し、分岐ごとにカバレッジゲートがあること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

- `src/playout/feedback.rs` を追加し、moqt-js の `src/audioDelayFeedback.ts` を移植した (時刻は引数で受ける Sans-I/O)
- 定数 (マイクロ秒): 下限 80 / 上限 300 / 初期 100 / 調整の間隔 1 秒 / 観測の窓 1 秒 (`crate::playout::timing` の既存定数を参照) / 許容 10 / 余白 20 / 増分の下限 20 / 増分の上限 40 / 毎秒の減少量 10 (いずれも ms の値)
- 公開: `AudioDelayFeedbackReason` (`Initial` / `Backlog` / `Lateness` / `Settled` / `Waiting`)、`AudioDelayFeedbackSnapshot`、`AudioDelayFeedback` (`set_ceiling_us` / `ceiling_us` / `feedback_target_us` / `target_delay_us` / `update` / `snapshot` / `reset`)
- 目標は jitter buffer の学習値との大きい方を採る。実際に鳴った観測を 1 つも受けていない間は学習値をそのまま返す
- 調整は毎秒 1 回だけ行う。鳴り遅れ (許容 10 ms 超) と並べすぎの累積の増分から増分を [20, 40] ms にクランプして増やし、どちらも無ければ毎秒 10 ms の速さで減らす (負にはしない)。明示された `targetLatency` は閉ループの目標にだけ上限として掛け、学習値には掛けない
- `PlayoutTimeline` に閉ループを持たせ、音声の `observe` で表示の遅れを「揺らぎの学習値と閉ループの目標の大きい方」から決める。`observe_audio_playout` を追加して観測を渡し、その場で表示の遅れを取り直す
- `set_target_latency_ms` は 0 より大きいときだけ閉ループの上限として渡す (0 は「下限にしない」の意味であるため、上限の解除として扱う)
- `reset_track(Track::Audio)` と `reset` は揺らぎの学習だけを消し、閉ループは消さない (購読のやり直しで目標を戻さない)
- `DelayBreakdown` に `audio_delay_feedback` を追加し、閉ループの状態 (目標・理由・上限・調整の回数・直近の分布) を内訳から読めるようにした
- テスト: `tests/test_playout/feedback.rs` に 13 件 (観測前は学習値 / 鳴り遅れと並べすぎでの増分のクランプ / 間隔 / 許容内の減少 / 下限と上限と明示 ceiling / 大きい方の採用 / reset で上限が残ること) を追加した
- 検証: `cargo fmt --check` / `make pbt` / `make test` / `cargo clippy --workspace --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc` が成功した。`PBT_SEED` を 17 種類変えても property が成功する
- 検証: 10 種類の欠陥 (調整の間隔の無効化 / 観測前の閉ループ利用 / 増分の上限の変更 / 明示上限の無視 / 表示の遅れの取り直し漏れ / 上限の配線漏れ / `observe` の閉ループ無視 / `reset_track` での閉ループ消去 / PBT への 2 種) を入れて、追加したテストと property がそれぞれ検出することを確認した
- 備考: 明示された上限が下限 80 ms を下回る場合は、moqt-js と同じく上限を優先する (目標が下限を下回り得る)
