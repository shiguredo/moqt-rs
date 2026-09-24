# moqt-publisher のカタログテストが encoder の codec 定数を二重管理している

- Created: 2026-09-23
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-publisher-codec-constant-reference
- Polished: {YYYY-MM-DD}

## 目的

`examples/moqt-publisher/src/catalog.rs` のテストが codec 文字列をリテラルで持ち、encoder モジュールの定数と同じ値を二重管理している。encoder 側で codec 文字列を変更したときにテストが追随せず、テストが実際の送信値ではなく過去の値を通してしまう状態を解消する。

## 現状

`build_catalog` を検証する `publisher_catalog_tracks_encode` / `publisher_alternate_video_codecs_encode` は `av01.0.08M.08` / `avc1.640028` / `hvc1.1.6.L120.B0` / `opus` をリテラルで持つ。実際の送信値を決めるのは encoder モジュールの次の定数である。

- `examples/moqt-publisher/src/encoder/av1.rs` の `AV1_CATALOG_CODEC_STRING`
- `examples/moqt-publisher/src/encoder/h264.rs` の `DEFAULT_AVC_CATALOG_CODEC_STRING`
- `examples/moqt-publisher/src/encoder/h265.rs` の `DEFAULT_HEVC_CATALOG_CODEC_STRING`
- `examples/moqt-publisher/src/encoder/opus.rs` の `OPUS_CATALOG_CODEC_STRING`

いずれもモジュール外から参照できない可視性のため、テストから参照できない。

## 設計方針

- encoder モジュールの codec 定数をクレート内から参照できる可視性にする
- サブモジュールの公開範囲も合わせて調整し、`catalog` モジュールのテストから定数を参照する
- `catalog.rs` のテストはリテラルをやめ、encoder の定数を渡して `build_catalog` と `encode` を検証する
- 送信経路の codec 文字列は変えない (挙動不変のリファクタリングとする)

## 完了条件

- `catalog.rs` のテストが codec 文字列のリテラルを持たず encoder の定数を参照していること
- リファクタリング前後で `build_catalog` が生成するカタログと送信経路の挙動が変わらないこと
- `cargo test -p moqt-publisher` / `make test` / `make clippy` / `make fmt` が通ること
