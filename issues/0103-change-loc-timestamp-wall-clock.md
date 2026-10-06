# LOC Timestamp を wall-clock (Unix epoch マイクロ秒) に統一する

- Created: 2026-09-18
- Completed: {YYYY-MM-DD}
- Branch: feature/change-loc-timestamp-wall-clock
- Polished: 2026-09-21

## 目的

音声と映像の LOC Timestamp が別々の 0 起点メディア時間で送られているため、購読側で共通の時間軸として扱えず A/V 同期ができない。両トラックの Timestamp を capture 由来の wall-clock (Unix epoch マイクロ秒) に統一し、MSF カタログに同期用のメタデータ (`renderGroup` / `targetLatency`) を載せる。

## 現状

- `examples/moq-pub/src/encoder/av1.rs` の `Av1Encoder::encode`、`h264.rs` の `H264Encoder::encode`、`h265.rs` の `H265Encoder::encode` は `frame_count * timescale / fps` で Timestamp を生成する。wall-clock ではなく、フレーム落ちで実時間からずれる
- `examples/moq-pub/src/pipeline.rs` の `build_video_loc_properties` は `PROP_TIMESCALE` を keyframe のみに付与する。LOC-04 §2.3.1.1 は「Timescale が無ければ Unix epoch からのマイクロ秒」と解釈するため、単体で見た delta frame の Timestamp は誤った時刻になる
- `build_audio_loc_properties` は毎 Object に `PROP_TIMESCALE` を付けるが、Timestamp は `audio_frame_count * samples_per_frame` で映像とは別の 0 起点になっている
- `examples/moq-pub/src/catalog.rs` の `send_catalog` は `renderGroup` / `targetLatency` を設定しない。`src/msf.rs` の `MsfTrack` には `target_latency` / `render_group` があり、`validate_group_target_latency` が同一 render group 内の一致を検証する
- 入力には capture timestamp がある (`shiguredo_audio_device::AudioFrameOwned` の `timestamp_us`、`shiguredo_video_device::VideoFrameOwned` の `timestamp_us`)。**ただしその起源はプラットフォームで異なる** (次節)
- `examples/moq-sub/src/main.rs` の `run_raw_player` は映像の PTS にプレイヤースレッド開始からの経過時間 (デキュー時刻) を使う。epoch でも受信時刻でもない。カタログの `targetLatency` も参照しない
- `examples/moq-sub/src/pipeline.rs` の `audio_pts_us` は Timescale が無いとき `AUDIO_FALLBACK_SAMPLE_RATE` (48_000) を仮定する
- **`--input-mp4` / `--input-mp4-reencode` の経路では Timestamp はファイル由来のメディア時刻である。** `examples/moq-pub/src/pipeline.rs` の `video_timescale` / `audio_timescale` は MP4 / 入力トラックのタイムスケールを使い、LOC-04 §2.3.1.2 は Timescale がある場合をメディア時刻と定義する。本 issue で Timescale を外すのは live capture の経路だけにする

### capture timestamp の起源

**`timestamp_us` は Unix epoch ではない。** デバイスクレートの実装を確認した結果は次のとおり。

| 経路 | 実装 | 起源 |
| --- | --- | --- |
| 音声 macOS | `shiguredo_audio_device` の `audio_coreaudio.m` (`mHostTime` を `mach_timebase_info` で変換) | mach 絶対時刻 (monotonic) |
| 音声 Windows | `shiguredo_audio_device` の `capture_wasapi.rs` (`QueryPerformanceCounter`) | monotonic |
| 映像 macOS | `shiguredo_video_device` の `video_avf.m` (`CMSampleBufferGetPresentationTimeStamp`) | mach 絶対時刻 (monotonic) |
| 映像 Windows | `shiguredo_video_device` の `capture_mf.rs` (サンプル時刻 / 10) | Media Foundation のサンプル時刻由来。どのクロックかは実装から判別できない |
| 映像 Linux (V4L2) | `shiguredo_video_device` の `video_v4l2.c` (`buf.timestamp`) | driver が選ぶ (`CLOCK_MONOTONIC` と `CLOCK_REALTIME` のどちらもあり得る)。`V4L2_BUF_FLAG_TIMESTAMP_*` を見ていないため実行時に判別できない |
| 映像 Linux (PipeWire) | `shiguredo_video_device` の `video_pipewire.c` (`header->pts` または `clock_gettime(CLOCK_MONOTONIC)`) | monotonic |
| fake capture | `examples/moq-pub/src/fake_capture.rs` と `fake_audio_capture.rs` | 0 起点 |

## 設計方針

### epoch への正規化

- `src/media_clock.rs` の `WallClockMapper` を使う。フレームを読んだ時点で `observe(media_us, wall_clock_us)` を呼び、「壁時計 − メディア時刻」の**最小値** (撮影から読むまでの遅れが最も小さいフレーム) を対応の目標にする
- `to_wall_clock_us(media_us, fallback_wall_clock_us)` で各フレームを換算する。対応を後から小さくすると換算した TIMESTAMP が前のフレームより戻るため、1 回の換算で動かす量を、前回換算したフレームとのメディア時刻の差の半分未満に抑える (ライブラリ側で実装済み)
- 1 つのフレームだけで対応を取ると、その遅れの分だけ以降の TIMESTAMP が撮影時刻より未来へずれる。最小値を使うことでこのずれを避ける
- 起源が epoch の経路でも monotonic の経路でも同じ式で扱える。monotonic の経路では起動時間ぶんの差がここで消える
- mapper は**音声と映像で別々に持つ**。起源が経路ごとに違うため、共通の対応を仮定しない
- fake capture は 0 起点なので、同じ式でそのまま epoch に写る
- 換算は純粋な処理として単体テストと PBT で検証する (ライブラリ側は実装済み)

### Timestamp の生成

- live capture の経路では `PROP_TIMESCALE` を**付けない**。LOC-04 §2.3.1.1 の既定 (Unix epoch マイクロ秒) で送る
- `--input-mp4` / `--input-mp4-reencode` の経路は現状どおり Timescale を付ける (ファイル由来のメディア時刻であり、wall-clock ではない)
- encoder が持つ `frame_count` 由来の Timestamp をやめ、換算した capture timestamp を encoder へ渡して `EncodedFrame.timestamp` に載せる
- `VideoEncoder::timescale` / `OpusEncoder::timescale` / 各 encoder の `timescale()` は live capture の経路では使わなくなる (MP4 の経路は `video_timescale` / `audio_timescale` を使い続ける)
- 音声は capture フレームと Object が 1:1 ではない。`pipeline.rs` のデータループは 1 つの `AudioFrameOwned` から複数の Opus フレームを切り出すため、**バッファ先頭の capture timestamp + 蓄積サンプル数から算出する**。同一 capture フレーム由来の複数 Object が同一 Timestamp にならないようにする

### カタログ

- `send_catalog` に `renderGroup` (音声・映像で同一値) と `targetLatency` (同一値) を設定する (MSF-01 §5.2.8 / §5.2.11)
- `targetLatency` の既定は **200 ms** とし、`--target-latency` (ms) で変更できるようにする。live の example としての既定値であり、MSF-01 §5.2.8 が同じ render group の track に同一値を MUST としているため、音声と映像で同じ値を使う
- MSF-01 §5.2.8 は `isLive` が false のとき `targetLatency` を無視する MUST としており、これは受信側の規則であって publisher の出力禁止ではない。`src/msf.rs` の `write_track_json` は decode と対称にするため `isLive=false` のとき `targetLatency` を出力しない実装になっており、この既存挙動は変えない。`send_catalog` は `MsfTrack::new(..., true)` で live 固定のため、この issue では `isLive` の分岐を追加しない

### 受信側との関係

- 受信側で Timestamp を使った A/V 同期を実装するのは 0104 で扱う。0104 は `playout::timeline` と `playout::buffer` を前提にする
- **0103 を単独で入れると subscriber の音声が壊れる。** `audio_pts_us` は Timescale が無いとき 48_000 を仮定するため、epoch マイクロ秒の Timestamp をサンプル数として解釈してしまう。0104 と同時にマージする前提は置かず、**0103 の中で `audio_pts_us` を修正する** (Timescale が無ければ Timestamp を epoch マイクロ秒として扱う)。0104 はこの修正を前提に、PTS の算出と再生タイミングの実装へ進む

## 完了条件

- 音声・映像の Timestamp が同じ epoch マイクロ秒軸で単調に増加すること
- **Timestamp が wall-clock と一致すること。** publisher 側でフレーム受信時の `SystemTime::now()` (epoch マイクロ秒) と変換後の Timestamp の差をログ出力する。この差は「撮影から読むまでの遅れ」であり、`WallClockMapper` はその最小値を対応に使うため 0 にはならない。各トラックの差が小さいことだけでは A/V のずれを検出できないため、**音声と映像の差の中央値どうしの差が ±20 ms 以内**であることを実機 1 ケース以上で確認する
- 映像の delta frame を単体で見ても LOC の既定どおり epoch マイクロ秒と解釈できること
- 音声で 1 つの capture フレームから複数 Object を切り出すとき、Timestamp が単調に増加すること
- カタログの `renderGroup` / `targetLatency` が音声・映像で同一値になっていること
- `--target-latency` (ms、既定 200) が追加され、`examples/README.md` の publisher オプション表が追随していること
- `src/msf.rs` の `write_track_json` が `isLive=false` で `targetLatency` を出力しない既存挙動を変えていないこと
- `--input-mp4` / `--input-mp4-reencode` の Timestamp と `PROP_TIMESCALE` が変わっていないこと
- capture timestamp の epoch 正規化が純関数 (`WallClockMapper`) としてテストされていること
- subscriber の `audio_pts_us` が Timescale 無しの Timestamp を epoch マイクロ秒として扱い、48_000 の仮定をやめていること

## 参照

- `src/media_clock.rs` の `WallClockMapper`
- draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.11 (Render group)
- draft-ietf-moq-loc-04 §2.3.1.1 (Timestamp) / §2.3.1.2 (Timescale)
