# moq-pub / moq-sub の decoder 実装の重複を解消する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-example-codec-dedup
- Polished: 2026-10-20

## 目的

`examples/moq-pub/src/decoder/` と `examples/moq-sub/src/decoder/` にほぼ同一の AV1 / H.264 / H.265 / Opus のデコード処理があり、`copy_plane` / `read_ps_nal` / `skip_ps_nal` などのヘルパも重複している。変更時に両方を直す必要があり、片方だけ直す事故が起きる。共有 crate に集約し、pub / sub の差だけを各 example 側に残す。

## 現状

- 次のモジュールがほぼ同一の実装を持つ。
  - `examples/moq-pub/src/decoder/{av1,h264,h265,opus}.rs`
  - `examples/moq-sub/src/decoder/{av1,h264,h265,opus}.rs`
- `examples/moq-pub/src/decoder.rs` と `examples/moq-sub/src/decoder.rs` の `copy_plane` / `read_ps_nal` がほぼ同一である。
  - `copy_plane` は pub が `Vec::new()`、sub が `Vec::with_capacity(width * height)` を使う。shiguredo-rust は入力バイナリのデコードで `Vec::with_capacity()` を原則使わないと定めるため、共有化するときは `Vec::new()` に揃える。
  - pub の `read_ps_nal` は `#[cfg(target_os = "macos")]` だが、sub は `examples/moq-sub/src/mp4.rs` からも使うため全プラットフォームで必要になる。
- H.264 / H.265 では `skip_ps_nal` / `parse_avc_config` / `parse_hvc_config` / `AvcConfig` / `HvcConfig` / `decode_handler` / `convert_decoded_frame` もそれぞれのモジュールに重複している。
- pub / sub の差は次のとおり。
  - pub のみ: `Av1Decoder::drain_delayed` / `Av1Decoder::reset`、`VideoDecoder::drain_delayed` / `VideoDecoder::reset`、`interleave_uv`。
  - sub のみ: `DecodedVideoFrame` の `timestamp_us` フィールド、`AudioDecoder` と `decoder` モジュールの `build_video_decoder`、VP8 / VP9 / AAC の各デコーダ、`OpusDecoder::reset` / `sample_rate` / `channels`。
  - H.264 / H.265 の設定レコード未到達時の挙動が異なる。pub はエラーを返し、sub は警告ログを出して空のフレーム列を返す。
  - sub の `H264Decoder` / `H265Decoder` は `nalu_len_bytes` フィールドを持つが、代入しているだけで読み出していない。共有化の際に削除する。
- 影響: 修正コストと不整合リスクが二重化している。

## 設計方針

- `examples/moq-codec` (package `moq-codec`、`publish = false`、`rust-version = "1.95"`) を新設し、workspace の `members` に追加する。`tokio-moq` には置かない。`tokio-moq` はトランスポート層であり、codec 依存や macOS 専用依存を持ち込むと層が混ざるためである。
- 共有 crate に置くもの:
  - AV1 / H.264 / H.265 / Opus の各デコーダと、`DecodedVideoFrame` (I420 プレーンのみ。`timestamp_us` は持たない)。
  - `copy_plane` / `read_ps_nal` / `skip_ps_nal` と、`parse_avc_config` / `parse_hvc_config` およびその設定型。
  - codec 層のエラー型。アプリの `crate::error::Error` には依存しない。
- pub / sub に残すもの:
  - pub の `VideoDecoder`、sub の `VideoDecoder` / `AudioDecoder` の Enum と codec 選択 (`build_video_decoder` / `AudioDecoder::build` など)。
  - pub の `interleave_uv`、`drain_delayed` / `reset` の呼び出し。
  - sub の `DecodedVideoFrame` (`timestamp_us: Option<i64>` を持つ公開型) と、共有 crate のフレームからの変換。変換時は `timestamp_us: None` にし、pipeline が Object の Properties から設定する現行挙動を保つ。
  - sub の VP8 / VP9 / AAC デコーダ。これらは pub と重複していないため共有 crate へ移さない。
- 差の吸収:
  - pub / sub の各 `Error` に共有 crate のエラー型からの `From` を実装し、既存の variant と表示 (例: `decode: ...` / `video toolbox: ...` / `opus: ...`) を保つ。
  - H.264 / H.265 の設定未到達時は、共有 crate が「未初期化」を表すエラーを返し、pub は既存どおりエラーに、sub は警告ログを出して空のフレーム列にする。
  - sub の `OpusDecoder::reset` / `sample_rate` / `channels` は、共有 crate の Opus デコーダが同じ操作を提供できるようにする。
  - 差の吸収は trait ではなく、pub / sub の Enum とラッパーで行う (shiguredo-rust の trait 禁止に従う)。
- 挙動は変えない。デコード結果、エラー表示、既存のログの意味を変えない。
- 既存の pub の H.264 / H.265 の encode→decode テストは削除せず、共有 crate のデコーダを対象に通す。sub の VP8 / VP9 / AAC のテストも残す。
- 共有 crate が使う codec 依存 (`shiguredo_dav1d` / `shiguredo_opus` / `shiguredo_video_toolbox`) は pub / sub と同じバージョン指定を使い、`shiguredo_video_toolbox` は macOS 限定依存として H.264 / H.265 モジュールを `#[cfg(target_os = "macos")]` にする。
- 公開項目には `///` を付け、`mod.rs` を使わないなど shiguredo-rust に従う。
- 変更対象は `examples/moq-codec` の新設、`examples/moq-pub` / `examples/moq-sub` の decoder と関連 import、workspace の `Cargo.toml`、各 example の `Cargo.toml` に限定する。

## 完了条件

- AV1 / H.264 / H.265 / Opus のデコード処理と共通ヘルパが `examples/moq-codec` に集約され、pub / sub には codec 選択、アプリ固有の差、pub の `interleave_uv`、sub の VP8 / VP9 / AAC デコーダ、テストだけが残っていること。
- pub / sub の既存の挙動 (デコード結果、エラー表示、ログ) が変わっていないこと。
- 既存の pub の H.264 / H.265 の encode→decode テストと sub の VP8 / VP9 / AAC のテストが残り、通ること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- `.github/workflows/e2e-test.yml` と同じ条件 (macOS、`MOQT_E2E_URL` 設定済み) で `cargo test -p e2e-tests -- --ignored` が通ること。
