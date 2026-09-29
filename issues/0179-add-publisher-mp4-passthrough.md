# moq-pub に MP4 ファイルのパススルー配信機能を追加する

- Created: 2026-09-29
- Completed: {YYYY-MM-DD}
- Branch: feature/add-publisher-mp4-passthrough
- Polished: {YYYY-MM-DD}

## 目的

`moq-pub` はカメラ / マイク入力のみを扱うため、既存の MP4 ファイルを配信できない。sora-rust-sdk が持つ MP4 パススルー送信 (エンコード済みサンプルを再エンコードせずに送る機能) と同等の機能を `moq-pub` に追加し、カメラ不要で再現性のある配信ソースで MoQ の動作確認や負荷試験を行えるようにする。再エンコードしないためエンコード負荷がかからず、入力の画質もそのまま保たれる。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `run` は `capture::start_capture` または `fake_capture::start_capture` から `shiguredo_video_device::VideoFrameOwned` を受け取り、`encoder::VideoEncoder::encode` でエンコードした `EncodedFrame` を `SubgroupWriter` / `DatagramWriter` で送信する。ファイル入力を読み込む経路はない。
- `examples/moq-pub/src/cli.rs` の `Config` は `--device-id` / `--fake-capture-device` / `--video-codec` などカメラ前提のオプションのみで、MP4 を指定できない。
- MSF catalog は `catalog::send_catalog` に encoder から取得した codec 文字列・幅・高さ・ fps を渡して構築しており、ファイル由来のトラック情報を渡す経路はない。
- sora-rust-sdk では `docs/INPUT_MP4.md` と `src/video_codecs/mp4.rs` が MP4 パススルー送信を提供している。`shiguredo_mp4` の demux でサンプルを取り出し、`Mp4SampleReader` / `Mp4VideoCapturer` / `Mp4PassthroughEncoder` で実時間ペーシングして送る。
- sora-rust-sdk の MP4 パススルーの制約は「映像のみ (音声は無視)」「B フレームを含む MP4 は拒否」「キーフレーム要求は無視」「末尾でループ」である。対応コーデックは H.264 / H.265 / VP8 / VP9 / AV1 で、音声入力は pending。
- `examples/moq-sub` の `build_video_decoder` が対応する映像コーデックは av01 / avc1 / hvc1 / hev1 のみで、VP8 / VP9 は再生できない。

## 設計方針

- `examples/moq-pub/Cargo.toml` に `shiguredo_mp4 = "2026.5"` を追加し、`demux::Mp4FileDemuxer` と `codec_string::from_sample_entry` を使う。
- CLI に `--input-mp4 <PATH>` を追加する。指定時はカメラ / raden 疑似キャプチャを使わず、MP4 の映像トラックを配信する。
  - コーデックは MP4 から自動判定するため `--video-codec` との同時指定はエラーにする。
  - `--device-id` / `--fake-capture-device` / `--bitrate` / `--keyframe-interval` は無視する (警告を出す)。
  - 音声トラックは配信しない (警告を出し、catalog にも audio を含めない)。
- 対応映像コーデックは AV1 / H.264 / H.265 とする。moq-sub が再生できない VP8 / VP9 は対象外とする。
- B フレーム (composition time offset が非ゼロのサンプル) を含む MP4 は初期化時に拒否する (sora-rust-sdk と同じ)。
- MP4 の読み込みとペーシングは専用スレッドで行い、サンプルのタイムスタンプに従って実時間で既存のパイプラインへ `EncodedFrame` を渡す。末尾に達したら先頭に戻り、ループ後のタイムスタンプには経過メディア時間を加算して単調増加させる。
- パイプラインにエンコーダを通さない映像経路を追加する (`VideoSource` のような Enum で「キャプチャ + エンコード」と「MP4 パススルー」を切り替える)。
- LOC プロパティは既存の publisher と同じ形式にする。
  - `PROP_TIMESTAMP` / `PROP_TIMESCALE` は MP4 のサンプルタイムスタンプと timescale をそのまま使う。
  - `PROP_VIDEO_CONFIG` は MP4 のサンプルエントリーから取り出す (H.264: `Avc1Box` の avcC、H.265: `Hvc1Box` / `Hev1Box` の hvcC、AV1: `Av01Box` の av1C の config OBUs)。キーフレームに付与する。
  - AV1 はサンプルに Sequence Header が含まれない場合があるため、キーフレームの先頭に config OBUs を付与する (moq-sub の AV1 デコーダは payload 内の Sequence Header を前提とする)。
  - `PROP_VIDEO_FRAME_MARKING` は既存と同じくキーフレームで I ビットを立てる。
- MSF catalog は `codec_string::from_sample_entry` の文字列と MP4 から取得した幅・高さ・フレームレートで構築する。フレームレートはサンプルの duration から求める。
- キーフレームごとに新しい group を開始する (既存の publish ループと同じ)。キーフレーム要求 (NEW_GROUP_REQUEST) は無視する (sora-rust-sdk と同じ)。
- `--use-datagram` との併用は既存の分岐を再利用する。ただし moq-sub は datagram のメディアを処理しないため、完了条件の再生確認には含めない。

## 完了条件

- `cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --input-mp4 input.mp4` で MP4 の映像が MoQ 配信され、moq-sub で再生できる。
- AV1 / H.264 / H.265 の MP4 で動作する。H.264 / H.265 は macOS、AV1 は全プラットフォームで確認する。
- B フレームを含む MP4、映像トラックを持たない MP4、未対応コーデックの MP4 では分かりやすいエラーで終了する。
- `--input-mp4` と `--video-codec` の同時指定がエラーになる。
- `--input-mp4` を指定しない場合の動作と出力が変わらない。
- 単体テストで、サンプルエントリーから `PROP_VIDEO_CONFIG` への変換 (avcC / hvcC / av1C)、サンプルタイムスタンプの変換、ループ時のタイムスタンプ加算を固定する。実ファイルでのパススルー確認は moq-sub との結合で行う。
- `CHANGES.md` に ADD を追記し、`examples/README.md` の moq-pub オプション表に `--input-mp4` を追記する。
- `make test` / `make clippy` / `make fmt` が通る。

## 解決方法

{対応後に追記する}
