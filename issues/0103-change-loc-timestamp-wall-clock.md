# LOC Timestamp を wall-clock (Unix epoch マイクロ秒) に統一する

- Created: 2026-09-18
- Completed: {YYYY-MM-DD}
- Branch: feature/change-loc-timestamp-wall-clock
- Polished: 2026-09-20

## 目的

音声と映像の LOC Timestamp が別々の 0 起点メディア時間で送られているため、購読側で共通の時間軸として扱えず A/V 同期ができない。両トラックの Timestamp を capture 由来の wall-clock (Unix epoch マイクロ秒) に統一し、MSF カタログに同期用のメタデータ (`renderGroup` / `targetLatency`) を載せる。

## 現状

- `examples/moqt-publisher/src/encoder/av1.rs` の `Av1Encoder::encode`、`h264.rs` の `H264Encoder::encode`、`h265.rs` の `H265Encoder::encode` は `frame_count * timescale / fps` で Timestamp を生成する。wall-clock ではなく、フレーム落ちで実時間からずれる
- `examples/moqt-publisher/src/pipeline.rs` の `build_video_loc_properties` は `PROP_TIMESCALE` を keyframe のみに付与する。LOC-04 §2.3.1.1 は「Timescale が無ければ Unix epoch からのマイクロ秒」と解釈するため、単体で見た delta frame の Timestamp は誤った時刻になる
- `build_audio_loc_properties` は毎 Object に `PROP_TIMESCALE` を付けるが、Timestamp は `audio_frame_count * samples_per_frame` で映像とは別の 0 起点になっている
- `examples/moqt-publisher/src/catalog.rs` の `send_catalog` は `renderGroup` / `targetLatency` を設定しない。`src/msf.rs` の `MsfTrack` には `target_latency` / `render_group` があり、`validate_group_target_latency` が同一 render group 内の一致を検証する
- 入力には capture timestamp がある (`shiguredo_audio_device::AudioFrameOwned` の `timestamp_us`、`shiguredo_video_device::VideoFrameOwned` の `timestamp_us`)。**ただしその起源はプラットフォームで異なる** (次節)
- `examples/moqt-subscriber/src/main.rs` は映像の PTS に受信時刻を使い、カタログの `targetLatency` も参照しない
- `examples/moqt-subscriber/src/pipeline.rs` の `audio_pts_us` は Timescale が無いとき `AUDIO_FALLBACK_SAMPLE_RATE` (48_000) を仮定する

### capture timestamp の起源

**`timestamp_us` は Unix epoch ではない。** デバイスクレートの実装を確認した結果は次のとおり。

| 経路 | 実装 | 起源 |
| --- | --- | --- |
| 音声 macOS | `shiguredo_audio_device` の `audio_coreaudio.m` (`mHostTime` を `mach_timebase_info` で変換) | mach 絶対時刻 (monotonic) |
| 音声 Windows | `shiguredo_audio_device` の `capture_wasapi.rs` (`QueryPerformanceCounter`) | monotonic |
| 映像 macOS | `shiguredo_video_device` の `video_avf.m` (`CMSampleBufferGetPresentationTimeStamp`) | mach 絶対時刻 (monotonic) |
| 映像 Windows | `shiguredo_video_device` の `capture_mf.rs` (サンプル時刻 / 10) | monotonic |
| 映像 Linux (V4L2) | `shiguredo_video_device` の `video_v4l2.c` (`buf.timestamp`) | driver 依存 (既定は `CLOCK_MONOTONIC`。`V4L2_BUF_FLAG_TIMESTAMP_*` を見ていないため実行時に判別できない) |
| 映像 Linux (PipeWire) | `shiguredo_video_device` の `video_pipewire.c` (`header->pts` または `clock_gettime(CLOCK_MONOTONIC)`) | monotonic |
| fake capture | `examples/moqt-publisher/src/fake_capture.rs` と `fake_audio_capture.rs` | 0 起点 |

したがって **セッション開始時の epoch を加算するだけでは正しくならない。** monotonic な経路では起動時間ぶん未来へずれる。

## 設計方針

### epoch への正規化

- capture の現在値をその場で読む API はデバイスクレートに無い。**最初のフレームを受け取った時点で** `SystemTime::now()` とそのフレームの `timestamp_us` を組にしてアンカー (`anchor_epoch_us`, `anchor_capture_us`) を作る
- 各フレームの `timestamp_us` は `anchor_epoch_us + (timestamp_us - anchor_capture_us)` で epoch マイクロ秒へ写す
- アンカーは**音声と映像で別々に取る**。起源が経路ごとに違うため、共通のアンカーを仮定しない
- 起源が epoch の経路ではアンカーでの差がほぼ 0 になり、monotonic の経路では起動時間ぶんの差がここで相殺される。**どちらの起源でも同じ式で扱える**
- fake capture は 0 起点なので、アンカー方式でそのまま epoch に写る
- この変換は純関数として切り出し、テスト対象にする

### Timestamp の生成

- `PROP_TIMESCALE` は**付けない**。LOC-04 §2.3.1.1 の既定 (Unix epoch マイクロ秒) で送る。「付ける場合」の分岐は残さない
- encoder が持つ `frame_count` 由来の Timestamp をやめ、capture の timestamp を encoder へ渡して `EncodedFrame.timestamp` に載せる
- `VideoEncoder::timescale` / `OpusEncoder::timescale` / 各 encoder の `timescale()` は**削除する**。`pipeline.rs` の `video_timescale` / `audio_timescale` は `PROP_TIMESCALE` を載せるためだけに存在しており、載せない方針では用途が無くなる
- 音声は capture フレームと Object が 1:1 ではない。`pipeline.rs` のデータループは 1 つの `AudioFrameOwned` から複数の Opus フレームを切り出すため、**バッファ先頭の capture timestamp + 蓄積サンプル数から算出する**。同一 capture フレーム由来の複数 Object が同一 Timestamp にならないようにする

### カタログ

- `send_catalog` に `renderGroup` (音声・映像で同一値) と `targetLatency` (同一値) を設定する (MSF-01 §5.2.8 / §5.2.11)
- `targetLatency` の既定は **200 ms** とし、CLI オプションで変更できるようにする。live の example としての既定値であり、MSF-01 §5.2.8 が同じ render group の track に同一値を MUST としているため、音声と映像で同じ値を使う
- `isLive` が false のときは `targetLatency` を載せない (MSF-01 §5.2.8 の MUST)。`send_catalog` は現在 `MsfTrack::new(..., true)` で live 固定のため、この分岐は将来のための整理とする

### 受信側との関係

- 受信側で Timestamp を使った A/V 同期を実装するのは 0104 で扱う
- **0103 を単独で入れると、0104 が入るまで subscriber の音声が壊れる。** `audio_pts_us` は Timescale が無いとき 48_000 を仮定するため、epoch マイクロ秒の Timestamp をサンプル数として解釈してしまう。0104 と同時にマージするか、0103 に `audio_pts_us` の暫定対応 (Timescale が無ければ epoch マイクロ秒として扱う) を含める

## 完了条件

- 音声・映像の Timestamp が同じ epoch マイクロ秒軸で単調に増加すること
- **Timestamp が wall-clock と一致すること。** 受信側のローカル時刻と比較して許容範囲 (例: ±100 ms) に収まることを実機 1 ケース以上で確認する。単調性だけでは起動時間ぶんのずれを検出できない
- 映像の delta frame を単体で見ても LOC の既定どおり epoch マイクロ秒と解釈できること
- 音声で 1 つの capture フレームから複数 Object を切り出すとき、Timestamp が単調に増加すること
- カタログの `renderGroup` / `targetLatency` が音声・映像で同一値になっていること
- `isLive` が false の track に `targetLatency` を載せないこと
- capture timestamp の epoch 正規化と Timestamp 生成が純関数としてテストされていること
- 0104 と同時にマージしない場合は、subscriber の音声 PTS が破綻しない暫定対応が入っていること
