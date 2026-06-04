# LOC Timestamp を wall-clock (Unix epoch マイクロ秒) に統一する

- Created: 2026-09-18
- Completed: {YYYY-MM-DD}
- Branch: feature/change-loc-timestamp-wall-clock
- Polished: {YYYY-MM-DD}

## 目的

音声と映像の LOC Timestamp が別々の 0 起点メディア時間で送られているため、購読側で共通の時間軸として扱えず A/V 同期ができない。両トラックの Timestamp を capture 由来の wall-clock (Unix epoch マイクロ秒) に統一し、MSF カタログに同期用のメタデータ (`renderGroup` / `targetLatency`) を載せる。

## 現状

- `examples/moqt-publisher/src/encoder/av1.rs` の `Av1Encoder::encode`、`h264.rs` の `H264Encoder::encode`、`h265.rs` の `H265Encoder::encode` は `frame_count * timescale / fps` で Timestamp を生成する。wall-clock ではなく、フレーム落ちで実時間からずれる
- `examples/moqt-publisher/src/pipeline.rs` の `build_video_loc_properties` は `PROP_TIMESCALE` を keyframe のみに付与する。LOC-04 §2.3.1.1 は「Timescale が無ければ Unix epoch からのマイクロ秒」と解釈するため、単体で見た delta frame の Timestamp は誤った時刻になる
- `build_audio_loc_properties` は毎 Object に `PROP_TIMESCALE` を付けるが、Timestamp はサンプル数で映像とは別の 0 起点になっている
- `examples/moqt-publisher/src/catalog.rs` の `send_catalog` は `renderGroup` / `targetLatency` を設定しない
- 入力には capture timestamp がある (`shiguredo_audio_device::AudioFrameOwned` の `timestamp_us`、`VideoFrameOwned` の `timestamp_us`)
- `examples/moqt-subscriber/src/main.rs` は映像の PTS に受信時刻を使い、カタログの `targetLatency` も参照しない

## 設計方針

- capture の `timestamp_us` を使い、セッション開始時の epoch を加算して Unix epoch マイクロ秒に正規化する。0 起点のキャプチャ (fake capture) も同じ経路で変換する
- `PROP_TIMESCALE` は付けず、LOC の既定 (epoch マイクロ秒) で送る。付ける場合でも全 Object に付け、音声と映像で同一 anchor を保証する
- カタログに `renderGroup` (音声・映像で同一値) と `targetLatency` (同一値) を設定する (MSF-01 §5.2.8 / §5.2.11)
- カメラ由来 `timestamp_us` の起源 (epoch か monotonic か) を確認し、epoch でなければ開始時に補正する
- 受信側で Timestamp を使った A/V 同期を実装するのは 0104 で扱う

## 完了条件

- 同一セッションの音声・映像 Object の Timestamp が同じ epoch マイクロ秒軸で単調に増加すること
- 映像の delta frame を単体で見ても LOC の既定どおり epoch マイクロ秒と解釈できること
- カタログの `renderGroup` / `targetLatency` が音声・映像で同一値になっていること
- capture timestamp の epoch 正規化と Timestamp 生成が純関数としてテストされていること
