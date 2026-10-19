# MP4 入力の非キーフレームにも PROP_TIMESCALE を付与する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-mp4-video-timescale
- Polished: {YYYY-MM-DD}

## 目的

LOC-04 §2.3.1.1 は Timescale が無い Timestamp を Unix epoch からのマイクロ秒として解釈する。moq-pub は MP4 パススルー / 再エンコード経路で入力トラックの timescale 単位の Timestamp を送りながら、`PROP_TIMESCALE` をキーフレームにしか付けないため、非キーフレームの PTS が購読側で誤解釈される。全 Object に Timescale を付与するか、Track スコープで告知する。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `build_video_loc_properties` は `if frame.is_keyframe` の中でだけ `PROP_TIMESCALE` を push する。
- MP4 入力では `video_timescale` が `Some(入力トラックの timescale)` になり、`EncodedFrame::timestamp` も入力 timescale 単位である。非キーフレームは Timescale なしで送られる。
- `examples/moq-sub/src/pipeline.rs` の `extract_timestamp_timescale` は Timescale が無い Object をマイクロ秒として扱う。
- テスト `build_video_loc_properties_non_keyframe` が「非キーフレームは Timescale なし」を期待しており、誤った挙動を固定している。
- 影響: MP4 入力の映像で PTS が壊れ、表示時刻・A/V 同期が崩れる。

## 設計方針

- `timescale` が `Some` のときは全 Object に `PROP_TIMESCALE` を付与する。Track スコープでの告知に切り替える場合は、未実装の Track スコープ LOC プロパティ (0125) の状況を確認してから決める。
- live capture は Timestamp がマイクロ秒なので従来どおり `None` とする。
- テストを修正し、MP4 入力の E2E でフレーム間隔 / PTS を検証する。

## 完了条件

- MP4 入力の全 Object に Timescale が付与されること (または Track スコープで告知されていること)。
- 購読側で PTS が入力トラックの timescale として解釈されること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
