# 映像の表示時刻に合わせてフレームを選ぶキューを追加する

- Created: 2026-10-06
- Completed: {YYYY-MM-DD}
- Branch: feature/complete-playout
- Polished: {YYYY-MM-DD}

## 目的

復号した映像フレームを到着のタイミングでそのまま表示すると、経路の到着の揺らぎが表示間隔の揺らぎ (かくつき) になる。共有の時間軸 (`src/playout/timeline.rs` の `PlayoutTimeline`) が決めた表示時刻に合わせて 1 枚を選ぶキューをライブラリに置く。0104 が扱う映像側の受け皿であり、音声の jitter buffer と対になる。

## 現状

- `src/playout/timeline.rs` の `PlayoutTimeline` は `present_us` で表示時刻を返すが、フレームを保持して表示時刻に合わせて選ぶ仕組みがライブラリに無い
- `examples/moq-sub/src/main.rs` の `run_raw_player` は、受け取った `DecodedVideoFrame` を `raw_player::VideoPlayer::enqueue_video_i420` へ到着順にそのまま積む
- 表示待ちのフレーム数は `examples/moq-sub/src/pipeline.rs` の `MAX_DISPLAY_BACKLOG` で上限を設け、超えたら group を丸ごと捨てるだけである

## 設計方針

`src/playout/buffer.rs` に `PlayoutBuffer<T>` を追加する。フレームは積んだ順のまま扱い、並べ替えない (復号の出力は TIMESTAMP の順である)。時計もデバイスも触らず、現在時刻と時間軸は呼び出し側が渡す。

- 表示時刻を過ぎたフレームのうち最新の 1 枚を描き、それより古いものを捨てる
- 表示時刻を過ぎたフレームが 2 枚以上あるときは、最新を次の選択に残してその 1 つ前を描く。配信 fps と表示周期が近いとき、位相の揺れで重なった周期と空の周期が続いても両方の周期で 1 枚ずつ描ける
- 表示時刻を過ぎたフレームのうち `MAX_PRESENTATION_LAG_MS` (20 ms、60 Hz の表示周期程度) を超えて遅れたものは捨てる。最新の 1 枚は遅れていても残す
- 表示時刻を決められないフレーム (壁時計の TIMESTAMP を持たない、または時間軸がそのトラックの TIMESTAMP を使わない) は、届いた順に 1 回の選択で 1 枚ずつ描く
- 保持数の上限 `JITTER_BUFFER_MAX_QUEUED_FRAMES` (24 枚、30 fps で表示の遅れの上限 500 ms を保持できる枚数に余裕を足した値) を超えたら古い方から捨て、捨てたフレームを返す
- 積んだときの時間軸の世代を保持し、基準を取り直したあとに残っているフレームは表示時刻を決められないものとして扱う
- 表示時刻を求める相手は `PlayoutTimeline` の映像トラックとし、呼び出しごとに渡す (キューが時間軸を借り続けると観測のたびの更新と両立しない)

## 完了条件

- `PlayoutBuffer` の単体テストがあること (表示時刻前は描かない、2 枚以上過ぎたら 1 つ前を描く、上限を超えて遅れたフレームの破棄、保持数の上限超過、世代の取り直し、表示時刻を決められないフレーム、空のとき)
- `PlayoutBuffer` の PBT があること (表示時刻の順序と選択結果の不変条件)
- `make test` (`cargo test --workspace`) と `make pbt` と `make clippy` と `make fmt` が通ること

## 参照

- `src/playout/timeline.rs` の `PlayoutTimeline` / `Track`
- draft-ietf-moq-loc-04 §2.3.1.1 (Timestamp)
- draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.11 (Render group)
