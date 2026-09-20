# subscriber が LOC Timestamp と targetLatency で A/V 同期する

- Created: 2026-09-18
- Completed: {YYYY-MM-DD}
- Branch: feature/add-subscriber-av-sync
- Polished: {YYYY-MM-DD}

## 目的

受信側で音声と映像が別のクロックで再生されているため A/V 同期が成立しない。LOC Timestamp を共通の時間軸として使い、MSF の `targetLatency` に従って再生タイミングを決められるようにする。

MSF は `renderGroup` が同じ track を「同時に描画するよう設計されている」と定め (draft-ietf-moq-msf-01 §5.2.11)、`targetLatency` を「符号化から表示までの wallclock の差」と定義する (§5.2.8)。LOC の Timestamp は Timescale が無ければ Unix epoch マイクロ秒である (draft-ietf-moq-loc-04 §2.3.1.1)。表示時刻は `Timestamp + targetLatency` になる。

## 現状

- `examples/moqt-subscriber/src/main.rs` の `run_raw_player` は、映像の PTS に `start_time.elapsed()` を使う。`start_time` はプレイヤースレッド内の `Instant::now()` なので、これは**デキュー時刻**であり受信時刻でも LOC Timestamp でもない
- `examples/moqt-subscriber/src/decoder.rs` の `DecodedVideoFrame` は PTS を持たない
- 音声は `examples/moqt-subscriber/src/pipeline.rs` の `extract_timestamp_timescale` と `audio_pts_us` で LOC Timestamp から PTS を作る。ただし `audio_pts_us` は Timescale が無いとき `AUDIO_FALLBACK_SAMPLE_RATE` (48_000) を仮定する
- カタログの `targetLatency` は未使用。`receive_catalog` が返す `VideoTrackInfo` / `AudioTrackInfo` にも受け口が無く、プレイヤースレッドへ渡す経路も無い
- publisher 側が送る音声・映像の Timestamp が共通軸になるのは 0103 の完了後

### raw_player 側の前提

`examples/moqt-subscriber` は `raw_player` 2026.2.0 に再生を任せている。実装を確認した結果は次のとおり。

- `raw_player::VideoPlayer` は内蔵の `AudioPlayer` を持ち、`VideoPlayer::enqueue_audio` で音声を積み、`VideoPlayer::play()` を呼ぶと `audio_started` が立つ。以降 `VideoPlayer::render_next_frame` が**音声クロックをマスター**にして映像の表示タイミングを決める (`video_pts` と音声クロックの差が `sync_threshold_us` = 40 ms を超えるとフレームを捨てるか繰り返す)
- `audio_started` が立つのは `play()` (または再生中の `process()`) であって `enqueue_audio` ではない。音声クロックは `first_pts_us + 再生済みサンプル数` で、`first_pts_us` は最初に処理されたチャンクの PTS、再生済みサンプル数は `play()` で `resume()` してから増える
- 同期ずれは `VideoPlayerStats::sync_diff_us` で取れる
- 音声を積まないと `audio_started` が false のままになり、`first_video_pts_us + elapsed_us` の映像単独クロックにフォールバックする。この式は最初のフレームの PTS を基準にするため、**PTS を正しくしても同期は成立しない**
- **現在の購読側は `raw_player::AudioPlayer` を別インスタンスで作り、そちらへ音声を流している。`VideoPlayer::enqueue_audio` は一度も呼ばれていない**

## 設計方針

### 共通の時間軸

- `DecodedVideoFrame` に PTS を追加し、映像 Object の LOC Timestamp から算出する。`VideoDecoder::decode` に Timestamp を渡す経路が必要になる
- 音声の `audio_pts_us` を直し、Timescale が無いときは LOC-04 §2.3.1.1 の既定どおり epoch マイクロ秒として扱う。48_000 の仮定をやめる
- 0103 を先に完了させる。publisher が wall-clock の Timestamp を送らない限り共通軸にならない

### raw_player の同期機構に載せる

- **音声を `VideoPlayer::enqueue_audio` に流す。** 現在の別インスタンス `raw_player::AudioPlayer` は使わない。これで raw_player の音声マスター同期が働く
- 同期アルゴリズムを購読側で再実装しない。`sync_threshold_us` も raw_player の既定 (40 ms) をそのまま使う
- `--no-video` の経路では `VideoPlayer` を作らないため、**従来どおり別インスタンスの `raw_player::AudioPlayer` を使う**。同期する相手が無いので `VideoPlayer::enqueue_audio` へ載せる必要はない。`run_raw_player` の音声経路はこの 2 つで分岐させる

### targetLatency の反映

- **音声の enqueue と `VideoPlayer::play()` を `Timestamp + targetLatency` まで待つ。**
  最初の音声 Object の Timestamp を基準にし、`SystemTime::now()` が
  `Timestamp + targetLatency` に達するまで enqueue しない。既にその時刻を過ぎていれば待たない
- 待ってから enqueue と `play()` を同時に行うと、音声クロック
  (`first_pts_us + 再生済みサンプル数`) の起点が wall clock より `targetLatency` だけ遅れ、
  映像は `Timestamp + targetLatency` に表示される
- **`play()` を待ち合わせ完了まで呼ばない。** `render_next_frame` は `audio_started` が false の
  間 `first_video_pts_us + elapsed_us` の映像単独クロックで描画するため、待ち合わせ中に
  `play()` すると映像だけが `targetLatency` ぶん先行する。音声開始後はその先行ぶんが
  「早すぎる」フレームとして再描画待ちになり、`targetLatency` のあいだ映像が止まる。
  待ち合わせ中は映像フレームをキューに溜めるだけにする
- **`play()` を待ち合わせ完了まで呼ばない。** `render_next_frame` は `audio_started` が false の間 `first_video_pts_us + elapsed_us` の映像単独クロックで描画するため、待ち合わせ中に `play()` すると映像だけが `targetLatency` ぶん先行し、音声開始時に「遅すぎる」フレームとして捨てられる。待ち合わせ中は映像フレームをキューに溜めるだけにする
- PTS は書き換えない。遅らせるのは enqueue と `play()` の時刻だけである (`first_pts_us` は最初の音声 Object の Timestamp のまま)
- `targetLatency` はミリ秒 (MSF-01 §5.2.8)、Timestamp はマイクロ秒 (LOC-04 §2.3.1.1)。**`target_latency * 1000` をマイクロ秒として扱う**
- 映像の PTS には `targetLatency` を加算しない。加算すると音声との相対関係が崩れる
- `targetLatency` が無い場合は待たずに積む。表示時刻は `Timestamp` のまま (MSF-01 §5.2.8 は、live で `targetLatency` が無い場合に player が遅延を選んでよいとしている)
- `isLive` が false のときは `targetLatency` を無視する (MSF-01 §5.2.8 の MUST)。`src/msf.rs` の decode が `isLive=false` で `target_latency` を `None` に正規化するため、`MsfTrack::target_latency` を読む限り自動的に満たされる

### 保持量と測定

- `VideoPlayer::set_max_video_queue_size` を `targetLatency` に合わせて設定する。既定は 5 フレームで、30 fps / 200 ms では足りない (`targetLatency` ぶんのフレームを保持する必要がある)
- 同期ずれは `VideoPlayerStats::sync_diff_us` を一定間隔でサンプリングし、その最大絶対値で判定する。自前の計測を実装しない
- `examples/moqt-subscriber/src/pipeline.rs` の `MAX_DISPLAY_BACKLOG` は映像 group を丸ごと捨てる経路であり、同期ずれの測定と干渉する。測定手順でこの経路が働いていないことを確認する

### 変更対象

- `examples/moqt-subscriber/src/pipeline.rs`: `audio_pts_us` のフォールバック、`extract_timestamp_timescale` を使った映像 Timestamp の取り出し、`VideoTrackInfo` / `AudioTrackInfo` への `target_latency` 追加、`receive_catalog` の返り値
- `examples/moqt-subscriber/src/decoder.rs` と `decoder/{av1,h264,h265}.rs`: `DecodedVideoFrame` への PTS 追加と `VideoDecoder::decode` のシグネチャ
- `examples/moqt-subscriber/src/main.rs`: `run_raw_player` の音声経路 (`VideoPlayer::enqueue_audio` へ載せ替え)、`targetLatency` の待ち合わせ、`set_max_video_queue_size`、`start_time.elapsed()` の削除
- FETCH 経路 (`handle_fetch_stream` の `FetchStreamDecoder`) は Properties を保持しないため映像 PTS を付けられない。`DecodedVideoFrame.pts_us` は `i64` とし、**FETCH 経路では映像を扱わない**ことを前提とする (現状もカタログの FETCH のみで、映像は Subgroup 経由)。映像を FETCH でも扱う必要が出たら別 issue にする。番兵値は入れない

## 完了条件

- 音声と映像の PTS が同じ epoch マイクロ秒軸で比較できること
- **音声が `VideoPlayer::enqueue_audio` に載り、`VideoPlayerStats::sync_diff_us` が意味のある値を返すこと。** 映像単独クロックへのフォールバックが起きていないこと
- 120 秒程度の連続再生で `sync_diff_us` を 1 秒間隔でサンプルした最大絶対値が許容範囲 (例: ±50 ms) に収まること。測定時の fps と、音声あり / `--no-video` のどちらで測ったかを記録すること
- **測定は音声の再生開始後から行う。** 音声開始前は `get_audio_clock_us` が 0 を返し、映像は映像単独クロックで描画されるため `sync_diff_us` が意味を持たない。開始前の過渡を判定に含めない
- `targetLatency` が `Timestamp + targetLatency` の表示時刻として効いていること。`targetLatency` を変えると表示が遅れることを実機で確認すること
- `targetLatency` 未設定時のフォールバック (待たずに積む) がテストされていること
- 1 つの capture フレームから複数 Object を切り出す場合を含め、音声 PTS が単調に増加すること
- `make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ること

## 参照

- draft-ietf-moq-msf-01 §5.2.8 (Target latency: 符号化から表示までの wallclock の差。ミリ秒。`isLive` が false なら無視する MUST。同じ render group の track は同一値でなければならない)
- draft-ietf-moq-msf-01 §5.2.11 (Render group: 同じ group の track は同時に描画する SHOULD)
- draft-ietf-moq-loc-04 §2.3.1.1 (Timestamp: Timescale が無ければ Unix epoch マイクロ秒)
- `raw_player` 2026.2.0 の `VideoPlayer::enqueue_audio` / `VideoPlayer::render_next_frame` / `VideoPlayerStats::sync_diff_us` / `VideoPlayer::set_max_video_queue_size`
