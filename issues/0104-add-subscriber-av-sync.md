# subscriber が LOC Timestamp と targetLatency で A/V 同期する

- Created: 2026-09-19
- Completed: {YYYY-MM-DD}
- Branch: feature/add-subscriber-av-sync
- Polished: 2026-09-21

## 目的

subscriber は音声と映像を別々にデコードし、映像の PTS にプレイヤースレッド開始からの経過時間を使っている。これは受信時刻であって、LOC Timestamp でも capture の時刻でもない。カタログの `targetLatency` も参照していない。publisher が共通の時間軸で Timestamp を送るようになったあと (0103)、subscriber がそれを基準に表示時刻を決め、カタログの `targetLatency` を反映して A/V 同期を取れるようにする。

## 前提

- ライブラリに `playout::timeline` がある。音声と映像の表示時刻を決め、A/V 同期の遅延制御も `observe` のたびに行う (呼び出し側が `sync` を呼ぶ必要はない)
- ライブラリに `playout::buffer::PlayoutBuffer` がある。映像フレームを表示時刻に合わせて選び、表示時刻を過ぎたフレームのうち最新の 1 枚を描く
- ライブラリに `playout::scheduler` と `playout::stretch::conceal` がある。鳴らす時刻、時間圧縮、隙間の補間を指示できる
- publisher が audio と video の両方に epoch マイクロ秒の Timestamp を付け、カタログに同一の `renderGroup` / `targetLatency` を載せるのは 0103 の完了後
- 0103 で `audio_pts_us` の 48_000 仮定をやめる

## 現状

- `examples/moq-sub/src/main.rs` の `run_raw_player` は映像の PTS に `start_time.elapsed()` を使う。受信からの経過時間であり、映像と音声の時間軸が別である
- `examples/moq-sub/src/decoder.rs` の `DecodedVideoFrame` は LOC の Timestamp を保持しない。`pipeline.rs` の映像の経路は Object の Timestamp と `PROP_TIMESCALE` を読んで捨てている
- 音声は `AudioJitterBuffer` が `present_us` まで保持してから `raw_player::AudioPlayer` へ積む。時間軸はこのバッファが内部に持ち、映像は観測していない
- `raw_player` の `VideoPlayer` は音声が積まれていれば音声クロックを master として同期する。`sync_threshold_us` (40 ms) を超えた差はドロップとフリーズで吸収する
- カタログの `targetLatency` は受信しても使われない
- `examples/moq-sub/src/pipeline.rs` の `MAX_CONCURRENT_STREAMS` により映像の復号は並行に走る。復号の出力は Object の順に揃わないことがある

## 設計方針

### 時間軸を 1 つにする

- `run_raw_player` が `PlayoutTimeline` を 1 つ持ち、音声と映像の復号の出力を同じ時間軸へ `observe` する。A/V 同期の遅延制御は時間軸が観測のたびに行うため、example 側で制御を書かない
- 表示時刻は `present_us(track, Timestamp)` で求める。音声も映像も「Timestamp + 基準の遅れ + 表示の遅れ」という同じ式になる

### 映像

- `DecodedVideoFrame` に Timestamp (epoch マイクロ秒) を足し、`pipeline.rs` の映像の経路で Object の Timestamp を載せる。Timescale があるとき (MP4 の経路) はその値からマイクロ秒へ換算した値であり、0103 の完了後は live の経路でも epoch マイクロ秒になる
- 復号の出力を `PlayoutBuffer::enqueue` へ積む。描画の周期ごとに `select(now_us, &timeline)` を呼び、返った `draw` の 1 枚を `VideoPlayer::enqueue_video_i420` へ渡す。`late` は捨てる
- `PlayoutBuffer` は表示時刻の昇順ではなく届いた順に保持し、表示時刻を過ぎた最新の 1 枚を描く。復号が並行に走っても表示の順は入れ替わらない
- `raw_player` の `VideoPlayer` へは表示時刻をそのまま PTS として渡し、`AudioPlayer` へ音声を積まない場合は `VideoPlayer` の内部同期に頼らず、呼び出し側が選んだ 1 枚を描かせる

### 音声

- `AudioJitterBuffer` は時間軸を内部に持つのをやめ、`run_raw_player` の時間軸を受け取る。音声の観測と映像の観測が同じ時間軸に入る
- 鳴らす時刻は `present_us`、目標遅延は `presentation_delay_us` から読む。`AudioPlayoutScheduler` の `Play` が返す `gap_start_us` / `gap_us` に従い、隙間は `stretch::conceal` で埋める。時間圧縮は `compress_us` に従う
- 揺らぎの学習 (`playout::delay`) は時間軸が持つ。jitter buffer は「鳴らす時刻まで保持する」役割に絞る

### targetLatency

- カタログの `targetLatency` を読み、`PlayoutTimeline::set_target_latency_ms` へ渡す。同じ `renderGroup` の track は同一値になる (MSF-01 §5.2.8)
- 表示時刻 = Timestamp + 基準の遅れ + max(自分の遅れ, targetLatency) になる。上限 (`max_presentation_delay_ms`) を超える分は `limited_us` で読める
- カタログに `targetLatency` が無いときは自分の遅れだけを使う (既定 0)

### 測定

- 同期の判定に `sync_diff_us` を使わない。`VideoPlayer` の `sync_threshold_us` (40 ms) で切られた後の値であり、超えた差が残るため「40 ms 以内に収まっている」ことは同期の証明にならない。音声と映像の**定数バイアス** (基準の遅れのずれ) は `sync_diff_us` に出ない
- 判定は時間軸の `skew_us` を使う。`record_presentation` で記録した表示の実績から求めた音声と映像の「表示時刻 − Timestamp」の差であり、時間軸が決めた表示時刻がそのまま出る
- 実機ではリップシンクの目視確認を併用する。`skew_us` は片方の表示が止まっている間は `None` になるため、再生中の値だけを使う

## 完了条件

- 音声と映像の表示時刻が同じ `PlayoutTimeline` で決まり、`present_us` の差が `TIMELINE_SYNC_MIN_DELTA_US` (30 ms) 以内に収まること
- 映像が `PlayoutBuffer` で選ばれ、表示時刻を過ぎて `MAX_PRESENTATION_LAG_US` を超えたフレームが捨てられること
- カタログの `targetLatency` が `set_target_latency_ms` へ渡り、表示時刻 = Timestamp + 基準の遅れ + max(自分の遅れ, targetLatency) になっていること。`targetLatency` が無いカタログでも動くこと
- 音声の隙間が `conceal` で埋まり、`AudioPlayoutScheduler` の `confirm_concealment` へ実際に埋めた長さが返ること
- 実機 (macOS) で 60 秒以上再生し、音声の再生開始後に測った `skew_us` の最大絶対値が 50 ms 以内であること。`skew_us` が `None` の区間は測定から除く
- `make test` / `make pbt` / `make clippy` / `make fmt` が通ること

## 対象外

- publisher 側の Timestamp の統一 (0103)
- 音声と映像で `renderGroup` が違うカタログへの対応 (MSF-01 §5.2.11 は同じ `renderGroup` の track を同時に表示する SHOULD とするが、group をまたぐ同期は扱わない)
- 表示の実績を使った時間軸の補正 (今は測定のみ)
