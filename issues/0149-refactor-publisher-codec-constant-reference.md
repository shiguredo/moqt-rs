# moq-pub のカタログテストが encoder の codec 定数を二重管理している

- Created: 2026-09-23
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-publisher-codec-constant-reference
- Polished: 2026-11-14
- Updated: 2026-11-14

## 目的

`crates/moq-pub/src/catalog.rs` のテストが codec 文字列をリテラルで持ち、encoder モジュールの定数と同じ値を二重管理している。encoder 側で codec 文字列を変更したときにテストが追随せず、テストが実際の送信値ではなく過去の値を通してしまう状態を解消する。

## 現状

`crates/moq-pub/src/catalog.rs` のテストのうち、次の 7 件が encoder の 4 つの codec 定数に対応する文字列をリテラルで持つ (合計 12 箇所)。

- `publisher_catalog_tracks_encode`: `av01.0.08M.08` / `opus`
- `publisher_catalog_sets_sync_metadata_on_both_tracks`: `av01.0.08M.08` / `opus`
- `publisher_catalog_sets_max_gop_and_group_duration`: `av01.0.08M.08`
- `publisher_catalog_sets_reserved_track_roles`: `av01.0.08M.08` / `opus`
- `publisher_alternate_video_codecs_encode`: `avc1.640028` / `hvc1.1.6.L120.B0` (同じテストは `vp8` / `vp09.00.40.08` もリテラルで持つ)
- `build_catalog_json_reflects_c4m_tokens`: `opus`
- `publisher_catalog_sets_auth_info_on_both_tracks_for_c4m`: `av01.0.08M.08` / `opus`

送信に使う codec 文字列の元になるのは encoder モジュールの次の定数である。av1 / opus は常に定数の値そのものが送信される。h264 / h265 は最初のキーフレームまでは定数の値 (`new` 時点の既定値) が送信され、最初のキーフレームで SPS から再構築した値へ置き換わる。

- `crates/moq-pub/src/encoder/av1.rs` の `AV1_CATALOG_CODEC_STRING`
- `crates/moq-pub/src/encoder/h264.rs` の `DEFAULT_AVC_CATALOG_CODEC_STRING`
- `crates/moq-pub/src/encoder/h265.rs` の `DEFAULT_HEVC_CATALOG_CODEC_STRING`
- `crates/moq-pub/src/encoder/opus.rs` の `OPUS_CATALOG_CODEC_STRING`

いずれもモジュール外から参照できない可視性のため、テストから参照できない。encoder の各サブモジュール (`encoder.rs` の `mod` 宣言) は既に `pub(crate)` なので、調整が必要なのは定数の可視性だけである。

本 issue の対象外:

- `publisher_aac_audio_codec_encodes` の `mp4a.40.2` は `crates/moq-pub/src/encoder/aac.rs` の `AAC_CATALOG_CODEC_STRING` と同値だが、`aac` モジュールは `#[cfg(target_os = "macos")]` のため、定数を参照するとテストが macOS 限定になる。このテストは cfg で限定されておらず、カタログの encode をプラットフォームに依存せず検証しているため、本 issue ではリテラルのまま残す。
- `publisher_alternate_video_codecs_encode` の `vp8` / `vp09.00.40.08` は `crates/moq-pub/src/encoder/vpx.rs` の `VpxCodec::catalog_codec_string` 内のリテラルに対応する。`vpx.rs` は codec 文字列の定数を持たないため、本 issue ではリテラルのまま残す。

## 設計方針

- 4 つの codec 定数を `pub(crate)` にし、`catalog` モジュールのテストから参照できるようにする
- `catalog.rs` のテストは、対象 4 定数に対応する codec 文字列のリテラルをやめ、encoder の定数を渡して `build_catalog` / `build_catalog_json` と `encode` を検証する
- h264 / h265 の定数を持つサブモジュールは `#[cfg(target_os = "macos")]` のため、`publisher_alternate_video_codecs_encode` の参照も同じ cfg の範囲に限る (macOS 限定のテストにする)。同テストは `vp8` / `vp09.00.40.08` も検証しているため、非 macOS ではそれらの検証が走らなくなるが、CI の workspace テストは macOS で実行されるため CI の coverage は落ちない
- 送信経路の codec 文字列は変えない (挙動不変のリファクタリングとする)

## 完了条件

- `catalog.rs` のテストが、対象 4 定数に対応する codec 文字列 (`av01.0.08M.08` / `avc1.640028` / `hvc1.1.6.L120.B0` / `opus`) のリテラルを持たず、encoder の定数を参照していること
- リファクタリング前後で `build_catalog` が生成するカタログと送信経路の挙動が変わらないこと
- `cargo test -p moq-pub` / `make test` / `make clippy` / `make fmt` が通ること
