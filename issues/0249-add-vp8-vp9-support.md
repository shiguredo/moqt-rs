# moq-pub / moq-sub で VP8 / VP9 を扱えるようにする

- Created: 2026-10-13
- Completed: {YYYY-MM-DD}
- Branch: feature/add-vp8-vp9-support
- Polished: {YYYY-MM-DD}

## 目的

example の publisher / subscriber は AV1 / H.264 / H.265 のみに対応している。VP8 / VP9 を扱えるようにし、
libvpx しか持たないブラウザ・端末との相互接続と、H.264 / H.265 が使えない環境 (macOS 以外) での
映像配信・再生の選択肢を増やす。

## 現状

- `examples/moq-pub/src/cli.rs` の `VideoCodec` は `Av1` / `H264` / `H265` のみで、`--video-codec` の
  help と parse エラーもこの 3 つを前提にしている
- `examples/moq-pub/src/encoder.rs` の `VideoEncoder` は `Av1` / `H264` / `H265` の variant を持ち、
  codec 別の実装は `examples/moq-pub/src/encoder/` にある。H.264 / H.265 は macOS の
  Video Toolbox 限定である
- `examples/moq-sub/src/decoder.rs` の `build_video_decoder` は catalog の codec 文字列が
  `av01` / `avc1` / `hvc1` / `hev1` の場合だけデコーダを生成し、それ以外は
  "unsupported video codec" で失敗する
- library 側の `src/msf.rs` の `is_video_codec` は `vp8` と `vp09.*` を既に video codec として
  認識しており、catalog の検証は通る
- `examples/moq-sub/src/mp4.rs` の `build_video_entry` は `av01` / `avc1` / `hvc1` / `hev1` のみで、
  VP8 / VP9 の MP4 録画は "unsupported video codec for MP4 recording" で失敗する
- `examples/moq-pub` の `--input-mp4` / `--input-mp4-reencode` は VP9 トラックを
  "unsupported video codec" で拒否する (`examples/moq-pub/src/mp4.rs` の
  `reader_rejects_unsupported_video_codec` で固定されている)

## 設計方針

- エンコーダ / デコーダは shiguredo_libvpx (libvpx-rs) を使う。VP8 と VP9 を 1 つのモジュールで扱い、
  codec の違いは列挙型で切り替える
- `VideoEncoder` に `Vp8` / `Vp9` を追加し、`--video-codec vp8` / `vp9` で選べるようにする。
  macOS 限定ではないため cfg を付けない
- catalog の codec 文字列は VP8 が `vp8`、VP9 が `vp09.00.40.08` (Profile 0 / Level 4.0 / 8-bit) とする。
  AV1 の `av01.0.08M.08` と同じく固定値とし、level は 1080p30 を覆う値にする
- `PROP_VIDEO_CONFIG` は VP8 / VP9 では載せない。avcC / hvcC / Sequence Header に相当する
  codec 固有の設定データが無いためである
- `build_video_decoder` に `vp8` / `vp09` の分岐を追加し、`copy_plane` で stride を詰めて I420 にする
- 入力は既存と同じ NV12 (`VideoFrameOwned`) とし、libvpx には `ImageData::Nv12` を渡す
- Profile 0 (8-bit I420) のみを対象にする。Profile 2 (10-bit) は既存の I420 パイプラインと
  `DecodedVideoFrame` が 8-bit 前提のため対象外とし、高 bit depth のフレームはエラーにする
- スコープは live capture → 配信 → 再生までとする。MP4 録画と `--input-mp4` の VP9 入力は別 issue にする

## 完了条件

- `moq-pub --video-codec vp8` / `vp9` で live capture の映像をエンコードして配信できること
- catalog の video track の codec が `vp8` / `vp09.00.40.08` になり、MSF の検証を通ること
- moq-sub が catalog の `vp8` / `vp09.*` からデコーダを生成し、I420 のフレームを再生できること
- エンコード → デコードのラウンドトリップテストが examples にあること
- 高 bit depth (10-bit) のフレームはエラーとして扱われること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること
- ソースコードのコメントに issue 番号や issue への言及を書かないこと

## 解決方法

{対応時に記入}
