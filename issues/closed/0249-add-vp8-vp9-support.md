# moq-pub / moq-sub で VP8 / VP9 を扱えるようにする

- Created: 2026-10-13
- Completed: 2026-10-13
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

shiguredo_libvpx (libvpx-rs) を examples の依存に追加し、live capture → 配信 → 再生まで対応した。
対象は VP8 / VP9 のエンコードとデコードであり、MP4 の出入り口は別 issue にする。

### 変更内容

- `examples/moq-pub/Cargo.toml` / `examples/moq-sub/Cargo.toml`: `shiguredo_libvpx = "2026.1"` を追加する
- `examples/moq-pub/src/encoder/vpx.rs`: `VpxCodec` (Vp8 / Vp9) と `VpxEncoder` を追加する。
  `EncoderConfig` は NV12 入力 / `EncodingDeadline::Realtime` / `RateControlMode::Cbr` /
  `cpu_used = Some(8)` / `threads = 4` とし、`ImageData::Nv12` に `VideoFrameOwned` の
  Y プレーンと UV プレーンを渡す
- `examples/moq-pub/src/encoder.rs`: `VideoEncoder` に `Vp8` / `Vp9` の variant を追加し、
  `encode` と `catalog_codec_string` の分岐を足す
- `examples/moq-pub/src/cli.rs`: `VideoCodec` に `Vp8` / `Vp9` を追加し、`--video-codec vp8` / `vp9`
  で選べるようにする。help と parse エラーの候補も更新する
- `examples/moq-pub/src/pipeline.rs`: `VideoCodec::Vp8` / `Vp9` から `VpxEncoder` を生成する。
  macOS 限定ではないため `cfg` を付けない
- `examples/moq-pub/src/error.rs` / `examples/moq-sub/src/error.rs`: `shiguredo_libvpx::Error` を
  集約する `Vpx` variant と `From` 実装を追加する
- `examples/moq-sub/src/decoder/vpx.rs`: `VpxCodec` と `VpxDecoder` を追加する。デコード結果は
  `copy_plane` で stride を詰めて I420 (`DecodedVideoFrame`) にする
- `examples/moq-sub/src/decoder.rs`: `VideoDecoder` に `Vp8` / `Vp9` の variant を追加し、
  `build_video_decoder` が `vp8` / `vp09` (`vp09.` 始まりを含む) でデコーダを選ぶようにする
- `examples/moq-sub/src/pipeline.rs`: catalog から video track を選ぶ条件に `vp8` / `vp09` を追加する
- `examples/README.md`: 対応コーデックと `--video-codec` の記述を更新する

### 判断と根拠

- **codec 文字列を固定値にする**: VP8 は登録名のみの `vp8`、VP9 は
  `vp09.<profile>.<level>.<bit depth>` を要求する (RFC 6381 §3.3)。VP9 は
  `vp09.00.40.08` (Profile 0 / Level 4.0 / 8-bit) とした。Level 4.0 は 1920x1080 30fps を覆い、
  AV1 の `av01.0.08M.08` と同じく解像度・フレームサイズに依らない固定値である。
  libvpx-rs は確定した level を返さないため、解像度からの動的算出はしない
- **`PROP_VIDEO_CONFIG` を載せない**: VP8 / VP9 には avcC / hvcC / AV1 Sequence Header に相当する
  codec 固有の設定データが無く、必要な情報はペイロード内のフレームヘッダに含まれる。
  `EncodedFrame::video_config` は常に `None` とし、publisher はプロパティを載せない
  (MSF は video track に Video Config を要求しない)
- **Profile 0 (8-bit I420) に限定する**: 既存の I420 パイプラインと `DecodedVideoFrame` が
  8-bit 前提であるため、10-bit / 12-bit のフレームは `VpxDecoder` がエラーにする
- **`lag_in_frames = 1` を明示する**: libvpx の VP9 は先読みの既定が 25 フレームであり、
  未指定だと最初の数十フレームが出力されず live 配信の遅延になる。実測で `1` を指定すると
  各入力に対してその場で出力されることを確認した
- **library 側は変更しない**: `src/msf.rs` の `is_video_codec` は既に `vp8` と `vp09.*` を
  video codec として認識しており (`tests/test_msf/error_cases.rs` で固定済み)、catalog の
  検証はそのまま通る

### テスト

- `examples/moq-pub/src/encoder/vpx.rs`: VP8 / VP9 のエンコードがキーフレームを返すこと、
  catalog の codec 文字列が `vp8` / `vp09.00.40.08` であること、`video_config` を載せないこと、
  fps = 0 を拒否すること
- `examples/moq-sub/src/decoder/vpx.rs`: VP8 / VP9 のキーフレームをデコードして I420 が得られる
  こと (I420 の寸法とプレーンサイズを検証)、不正なペイロードを拒否すること
- `examples/moq-sub/src/decoder.rs`: catalog の codec 文字列 (`vp8` / `vp09` / `vp09.00.40.08` /
  `vp09.0.10.08`) から対応するデコーダが選ばれること、登録外の文字列 (`vp8x` / `vp09x` など) を
  拒否すること
- `examples/moq-pub/src/cli.rs`: `--video-codec vp8` / `vp9` が解析できること、未対応の
  codec (`vp10`) を拒否すること
- `examples/moq-pub/src/catalog.rs`: `vp8` / `vp09.00.40.08` の catalog が encode できること

### スコープ外 (別 issue)

- moq-sub の MP4 録画は `build_video_entry` が `av01` / `avc1` / `hvc1` / `hev1` のみで、
  VP8 / VP9 は "unsupported video codec for MP4 recording" で失敗する。shiguredo_mp4 には
  `build_vp08_box` / `build_vp09_box` があるため追加できる
- `--input-mp4` / `--input-mp4-reencode` の VP9 入力は、現行どおり
  "unsupported video codec" で拒否する (`examples/moq-pub/src/mp4.rs` の
  `reader_rejects_unsupported_video_codec` で固定済み)

### 確認

- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通る
- 実 relay での確認は relay を用意できないため未実施である。代替として、エンコード →
  デコードのラウンドトリップと catalog の codec 文字列を単体テストで固定し、`e2e-tests` は
  既存の方針どおり relay がある環境での実行に委ねる
