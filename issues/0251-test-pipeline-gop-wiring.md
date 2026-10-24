# pipeline のキーフレーム要求・Group 観測・カタログ宣言の結線をテストで守る

- Created: 2026-10-14
- Completed: {YYYY-MM-DD}
- Branch: feature/test-pipeline-gop-wiring
- Polished: 2026-10-25

## 目的

映像 Group の長さを時間ベースで決める仕組みは、判定そのもの (`examples/moq-pub/src/gop.rs` の
`GopTracker`) をテストで固定してあるが、pipeline との結線 (キーフレーム要求・Group 開始の
観測・カタログへの宣言) がテストで守られていない。`examples/moq-pub/src/pipeline.rs` の `run`
の映像入力分岐から呼び出しを 1 つ削るだけで、その経路では時間ベースのキーフレーム要求が
丸ごと働かなくなる (Group はエンコーダのフレーム数上限だけで閉じられ、その長さは実際の
フレーム間隔に依存する) のに、テストはすべて通ってしまう。

## 現状

- `examples/moq-pub/src/gop.rs` の `GopTracker` (`request_keyframe_if_due` / `observe_keyframe`)
  と換算・上限・宣言値の純関数は `#[cfg(test)]` のテストで守られている
- 結線は `examples/moq-pub/src/pipeline.rs` の `run` の映像入力分岐にあり、次の 5 箇所が
  テストから到達できない
  - `request_keyframe_if_due` の呼び出し (live capture と MP4 再エンコードの 2 経路)
  - `observe_keyframe` の呼び出し (3 経路の共通ループ)
  - `frame_timestamp_us` による出力 Timestamp のマイクロ秒換算
  - エンコーダへ渡すキーフレーム間隔 (`keyframe_interval_frames`) の逆算
  - カタログに載せる `max_group_duration_ms` の宣言
- `pipeline::run` を走らせるテストは `e2e-tests/tests/roundtrip.rs` の 3 本
  (`publish_and_subscribe_roundtrip`、`publish_and_subscribe_audio_datagram_roundtrip`、
  `publish_and_subscribe_audio_redundancy_roundtrip`) だけで、いずれも実 relay が必要なため
  `#[ignore]` である。映像の `publish_and_subscribe_roundtrip` はフレームの到着数・サイズ・
  MP4 のサイズ・正常終了を、音声の 2 本は PTS の進行・サンプルレート・チャンネル数・MP4 の
  サイズ・正常終了を見るだけで、Group 長もカタログも見ていない
- 机上の変異試験では、次の 4 つがいずれもテストを落とさない
  - キーフレーム強制の引数を常に `false` にする
  - `frame_timestamp_us` を通さず出力フレームの Timestamp をそのまま使う
  - `observe_keyframe` の呼び出しを削除する (時間ベースの要求が一度も出なくなる)
  - カタログの宣言を常に `None` にする
- キーフレーム間隔の逆算 (`keyframe_interval_frames`) の呼び出しもテストから到達できないが、
  関数自体は `examples/moq-pub/src/gop.rs` の単体テストで固定済みであり、`encode_fps` と
  `gop_duration_ms` をそのまま渡すだけなので、本 issue の変異対象には含めない

## 設計方針

- `examples/moq-pub/src/pipeline.rs` に、出力フレーム列から Group の開始を観測する処理を
  関数として切り出す (例: `observe_encoded_frames(tracker: &mut GopTracker, frames: &[EncodedFrame], timescale: Option<u64>) -> Vec<Option<u64>>`)。戻り値は観測したキーフレームごとの
  直前 Group 長とし、`run` はその戻り値をキーフレームごとの debug ログに使う。
  `build_video_loc_properties` と同じ流儀で `EncodedFrame` を直接組み立てて `#[cfg(test)]`
  から呼べる形にし、モックは使わない
- キーフレームの要求は「判定して要求する」1 行の配線と encoder への受け渡しを 1 つの関数に
  まとめ (例: `encode_video_input(encoder: &mut VideoEncoder, tracker: &mut GopTracker, frame: &VideoFrameOwned, timestamp_us: u64) -> Result<Vec<EncodedFrame>>`)、`run` の live capture と
  MP4 再エンコードの両経路から呼ぶ。`#[cfg(test)]` から実エンコーダ (AV1) と
  `encoder::test_frame` で呼び、強制した入力がキーフレームとして出力されることを確認する
  (モックは使わない)
- カタログへ載せる `max_group_duration_ms` の宣言は、`CatalogVideoTrack` を組み立てる関数に
  切り出し (例: `catalog_video_track(codec: String, width: u32, height: u32, fps: u32, bitrate: u32, gop_duration_ms: u32) -> CatalogVideoTrack`)、`run` のエンコーダ経路から呼ぶ。
  `#[cfg(test)]` から `max_group_duration_ms` が `Some(gop::catalog_max_duration_ms(...))` に
  なることを確認する
- 実 relay が要る e2e テストの拡張 (購読側から Group のカデンスを測る等) は、単体テストで
  守れる範囲を広げたうえで判断する
- `run` から抽出した関数への呼び出しは 1 行であり、呼び出し自体の削除は単体テストでは
  検出しない。4 変異は関数内に入れ、`run` の呼び出しはレビューで確認する
- `pipeline::run` 全体をテスト可能にするための大きなリファクタリングはしない

## 完了条件

- 上記の 4 つの変異のいずれでも `cargo test -p moq-pub` が落ちること。変異は次の箇所に入れる
  - キーフレーム強制: `encode_video_input` が encoder へ渡す引数を `false` に固定する
  - `frame_timestamp_us`: `observe_encoded_frames` が `frame_timestamp_us` を通さず
    `EncodedFrame::timestamp` をそのまま使う
  - `observe_keyframe`: `observe_encoded_frames` から `observe_keyframe` の呼び出しを削除する
  - カタログ宣言: `catalog_video_track` の `max_group_duration_ms` を `None` に固定する
- `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、
  `cargo fmt --all -- --check` が通ること
