# moq-pub のカタログテストが encoder の codec 定数を二重管理している

- Created: 2026-09-23
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-publisher-codec-constant-reference
- Polished: 2026-09-28

## 目的

`examples/moq-pub/src/catalog.rs` のテストが codec 文字列をリテラルで持ち、encoder モジュールの定数と同じ値を二重管理している。encoder 側で codec 文字列を変更したときにテストが追随せず、テストが実際の送信値ではなく過去の値を通してしまう状態を解消する。

## 現状

`build_catalog` を検証する `publisher_catalog_tracks_encode` / `publisher_alternate_video_codecs_encode` は
`av01.0.08M.08` / `avc1.640028` / `hvc1.1.6.L120.B0` / `opus` をリテラルで持つ。送信に使う codec 文字列の元になるのは
encoder モジュールの次の定数である。av1 / opus は常に定数の値そのものが送信される。h264 / h265 は最初のキーフレームまでは
定数の値 (`new` 時点の既定値) が送信され、最初のキーフレームで SPS から再構築した値へ置き換わる。

- `examples/moq-pub/src/encoder/av1.rs` の `AV1_CATALOG_CODEC_STRING`
- `examples/moq-pub/src/encoder/h264.rs` の `DEFAULT_AVC_CATALOG_CODEC_STRING`
- `examples/moq-pub/src/encoder/h265.rs` の `DEFAULT_HEVC_CATALOG_CODEC_STRING`
- `examples/moq-pub/src/encoder/opus.rs` の `OPUS_CATALOG_CODEC_STRING`

いずれもモジュール外から参照できない可視性のため、テストから参照できない。encoder の各サブモジュール (`encoder.rs` の `mod` 宣言) は既に `pub(crate)` なので、調整が必要なのは定数の可視性だけである。

## 設計方針

- 4 つの codec 定数を `pub(crate)` にし、`catalog` モジュールのテストから参照できるようにする
- `catalog.rs` のテストはリテラルをやめ、encoder の定数を渡して `build_catalog` と `encode` を検証する
- h264 / h265 の定数を持つサブモジュールは `#[cfg(target_os = "macos")]` のため、`publisher_alternate_video_codecs_encode` の参照も同じ cfg の範囲に限る (macOS 限定のテストにする)
- 送信経路の codec 文字列は変えない (挙動不変のリファクタリングとする)

## 完了条件

- `catalog.rs` のテストが codec 文字列のリテラルを持たず encoder の定数を参照していること
- リファクタリング前後で `build_catalog` が生成するカタログと送信経路の挙動が変わらないこと
- `cargo test -p moq-pub` / `make test` / `make clippy` / `make fmt` が通ること
