# moq-pub に MP4 ファイルの再エンコード配信機能を追加する

- Created: 2026-09-29
- Completed: {YYYY-MM-DD}
- Branch: feature/add-publisher-mp4-reencode
- Polished: {YYYY-MM-DD}

## 目的

`moq-pub` の入力はカメラ / マイクのみで、MP4 ファイルを任意の配信設定 (コーデック・ビットレート) で配信できない。MP4 ファイルをデコードし、既存のエンコーダで再エンコードして配信できるようにする。sora-rust-sdk の MP4 入力は再エンコードなしのパススルーだけで音声入力も未対応のため、この機能は `moq-pub` 独自のものになる。moq-sub が `--mp4` で保存した MP4 (AV1 / H.264 / H.265 + Opus) を入力にでき、保存と配信の往復確認ができるようになる。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `run` はカメラ / マイクまたは raden 疑似入力のフレームを `encoder::VideoEncoder` / `encoder::opus::OpusEncoder` でエンコードして送信する。デコード機能は持たない。
- `examples/moq-sub` はデコーダを持っている。映像は `decoder::build_video_decoder` (AV1 は `shiguredo_dav1d`、H.264 / H.265 は `shiguredo_video_toolbox` の macOS 限定)、音声は `decoder::opus::OpusDecoder` (`shiguredo_opus`)。
- sora-rust-sdk の MP4 入力 (`docs/INPUT_MP4.md` / `src/video_codecs/mp4.rs`) はパススルーのみで再エンコードを行わない。音声入力も pending。

## 設計方針

- 「moq-pub に MP4 ファイルのパススルー配信機能を追加する」の実装を前提とし、その MP4 読み込み (demux) と実時間ペーシングを再利用する。
- CLI に `--input-mp4-reencode <PATH>` を追加する (`--input-mp4` との同時指定はエラー)。MP4 を入力のまま、再エンコードして配信する。
- 映像:
  - 入力コーデックは AV1 / H.264 / H.265 に対応する。AV1 は `shiguredo_dav1d`、H.264 / H.265 は `shiguredo_video_toolbox` (macOS 限定) でデコードする。`examples/moq-sub` のデコーダと同じ構成にする。
  - デコード結果を NV12 に変換し、既存の `encoder::VideoEncoder` (`--video-codec` で選択) で再エンコードして送信する。`VideoFrameOwned` を組み立てるか、エンコーダの入力抽象化を拡張する。
  - 解像度とフレームレートは MP4 から自動検出する。`--width` / `--height` / `--fps` を同時に指定した場合はエラーにする。可変フレームレートの MP4 は平均フレームレートに丸めて固定 fps で送る (警告を出す)。
- 音声:
  - MP4 の音声トラックが Opus の場合に対応する。`shiguredo_opus::Decoder` で PCM にデコードし、既存の `OpusEncoder` (48 kHz / 1ch、`--audio-bitrate`) で再エンコードする。サンプルレート / チャンネル数はエンコーダ設定へ変換する。
  - AAC など Opus 以外の音声トラックは警告して無視する (対応は別 issue とする)。
- タイムスタンプ:
  - 映像の LOC Timestamp は既存 publisher と同じくエンコーダが採番する (`frame_count * timescale / fps`)。入力 MP4 の映像タイムスタンプは出力 fps に合わせたペーシングに使う。
  - 音声はデコードした PCM を `OpusEncoder` が 20 ms フレーム単位でエンコードし、`audio_frame_count * samples_per_frame` を Timestamp にする (既存と同じ)。
- 末尾に達したら先頭に戻ってループする。パススルー実装と同じくタイムスタンプを単調増加させる。
- キーフレーム要求 (NEW_GROUP_REQUEST) は無視する。

## 完了条件

- `cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --input-mp4-reencode input.mp4 --video-codec av1` で MP4 が再エンコードされて MoQ 配信され、moq-sub で再生できる。
- AV1 / H.264 / H.265 の MP4 を入力にでき、macOS 以外では AV1 + Opus の MP4 で動作する。
- Opus 以外の音声トラックでは警告を出して映像のみ配信する。
- `--width` / `--height` / `--fps` の同時指定がエラーになる。
- `--input-mp4` を指定しない場合の動作が変わらない。
- moq-sub の `--mp4` で保存した MP4 を入力にして再配信し、別の moq-sub で再生できる (往復確認)。
- 単体テストで、MP4 のタイムスタンプからエンコーダ入力へのフレーム供給の順序と間引きの扱いを固定する。デコードとエンコードの結合は実機確認とする。
- `CHANGES.md` に ADD を追記し、`examples/README.md` の moq-pub オプション表に `--input-mp4-reencode` を追記する。
- `make test` / `make clippy` / `make fmt` が通る。

## 解決方法

{対応後に追記する}
