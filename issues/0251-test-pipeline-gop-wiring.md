# pipeline のキーフレーム要求と Group 開始の観測をテストで守る

- Created: 2026-10-14
- Completed: {YYYY-MM-DD}
- Branch: feature/test-pipeline-gop-wiring
- Polished: {YYYY-MM-DD}

## 目的

映像 Group の長さを時間ベースで決める仕組みは、判定そのもの (`examples/moq-pub/src/gop.rs` の
`GopTracker`) をテストで固定してあるが、pipeline との結線がテストで守られていない。
`examples/moq-pub/src/pipeline.rs` の `run` の映像入力分岐から呼び出しを 1 つ削るだけで、
時間ベースのキーフレーム要求が丸ごと働かなくなる (Group はエンコーダのフレーム数上限だけで
閉じられ、その長さは実際のフレーム間隔に依存する) のに、テストはすべて通ってしまう。

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
- `pipeline::run` を走らせるテストは `e2e-tests/tests/roundtrip.rs` の
  `publish_and_subscribe_roundtrip` だけで、実 relay が必要なため `#[ignore]` であり、
  検証内容もフレームの到着数・サイズ・MP4 のサイズ・正常終了に限られる (Group 長も
  カタログも見ていない)
- 机上の変異試験では、次の 4 つがいずれもテストを落とさない
  - キーフレーム強制の引数を常に `false` にする
  - `frame_timestamp_us` を通さず出力フレームの Timestamp をそのまま使う
  - `observe_keyframe` の呼び出しを削除する (時間ベースの要求が一度も出なくなる)
  - カタログの宣言を常に `None` にする

## 設計方針

- `examples/moq-pub/src/pipeline.rs` に、出力フレーム列から Group の開始を観測する処理を
  純関数として切り出す (例: `observe_encoded_frames(tracker: &mut GopTracker, frames: &[EncodedFrame], timescale: Option<u64>) -> Option<u64>`)。`build_video_loc_properties` と同じ流儀で
  `EncodedFrame` を直接組み立てて `#[cfg(test)]` から呼べる形にし、モックは使わない
- キーフレームの要求も「判定して要求する」1 行の配線に留め、テストから到達できる形にする
- 実 relay が要る e2e テストの拡張 (購読側から Group のカデンスを測る等) は、単体テストで
  守れる範囲を広げたうえで判断する
- `pipeline::run` 全体をテスト可能にするための大きなリファクタリングはしない

## 完了条件

- 上記の 4 つの変異のいずれでも `cargo test -p moq-pub` が落ちること
- `cargo test --workspace` と `cargo clippy --workspace --all-targets -- -D warnings` が通ること
