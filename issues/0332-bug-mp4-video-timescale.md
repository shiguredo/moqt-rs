# MP4 入力の非キーフレームにも PROP_TIMESCALE を付与する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-mp4-video-timescale
- Polished: 2026-10-20

## 目的

LOC-04 §2.3.1.1 は Timescale が無い Timestamp を Unix epoch からのマイクロ秒として解釈する。moq-pub は MP4 パススルー / 再エンコード経路で
入力トラックの timescale 単位の Timestamp を送りながら、`PROP_TIMESCALE` をキーフレームにしか付けないため、非キーフレームの PTS が
購読側で誤解釈される。MP4 入力の映像の全 Object に Timescale を付与する。Track スコープでの告知は 0125 が未実装であり、0125 も
publisher example からの送信を対象外としているため、本 issue では使わない。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `build_video_loc_properties` は `if frame.is_keyframe` の中でだけ `PROP_TIMESCALE` を push する。同じブロックで `PROP_VIDEO_CONFIG` も push している。
- MP4 入力では `video_timescale` が `Some(入力トラックの timescale)` になり、`EncodedFrame::timestamp` も入力 timescale 単位である。MP4 パススルーでは `Mp4VideoReader::info()` が返す `VideoTrackInfo::timescale`、再エンコードでは `Mp4ReencodeReader::video_info()` が返す `VideoInfo::timescale` を使う。非キーフレームは Timescale なしで送られる。
- `examples/moq-sub/src/pipeline.rs` の `extract_timestamp_timescale` は Timestamp と Timescale を取り出すだけで、Timescale が無い Object をマイクロ秒として扱うのは `loc_timestamp_us` である。`deliver_video_object` は `loc_timestamp_us` の結果を表示時刻として `decode_and_send` へ渡す。
- `examples/moq-sub/src/mp4.rs` の `to_microseconds` は観測済みの timescale を後続サンプルへ引き継ぐため、キーフレームに Timescale が付いている現状では録画経路は壊れない。再生経路の `loc_timestamp_us` は状態を持たないため、非キーフレームの PTS が誤解釈される。
- テスト `build_video_loc_properties_non_keyframe` が「非キーフレームは Timescale なし」を期待しており、誤った挙動を固定している (`props.timescale()` が `None` であることを assert している)。
- 影響: MP4 入力の映像を再生する経路で、非キーフレームの PTS がマイクロ秒として解釈され、表示時刻と A/V 同期が崩れる。

## 設計方針

- LOC-04 §6.1 (MOQ Properties Registry) Table 1 は TIMESCALE (0x08) の Scope を "Track, Object" としているため、Object Properties として毎回付与できる。
- `timescale` が `Some` のときは映像の全 Object に `PROP_TIMESCALE` を付与する。`build_video_loc_properties` の `if frame.is_keyframe` ブロックの外へ `PROP_TIMESCALE` の push を移す。`PROP_VIDEO_CONFIG` は従来どおりキーフレームのみに付与し、このブロックに残す。
- 本 issue では Track スコープでの告知は使わない。0125 は open で、`Subscription` に peer の Track Properties を保持する実装はまだ無い。0125 の設計方針も「publisher example から LOC の Track スコープ値を送る変更は本 issue の対象外とする」としている。
- 再生経路の `loc_timestamp_us` で直前の Timescale を引き継いで補うのではなく、LOC-04 §2.3.1.1 の既定に従い、publisher が映像の全 Object に Timescale を付ける。`loc_timestamp_us` は変更しない。
- live capture は Timestamp が Unix epoch マイクロ秒なので、従来どおり `timescale` を `None` とし、`PROP_TIMESCALE` を付けない。
- `build_video_loc_properties_non_keyframe` を、`timescale` が `Some(90_000)` のときに `props.timescale() == Some(90_000)` を期待するよう修正する。`build_video_loc_properties_keyframe` と `build_video_loc_properties_live_capture_has_no_timescale` は現状の期待を維持する。
- MP4 入力の PTS 間隔の統合検証は 0338 の pipeline 統合テストで扱う。e2e-tests は現状 live capture のみで、MP4 入力を起動する経路と fixture が無いため、本 issue では E2E テストを追加しない。

## 完了条件

- MP4 入力の映像の全 Object に Timescale が付与されること (`build_video_loc_properties_keyframe` と `build_video_loc_properties_non_keyframe` のテストで確認する)。
- 購読側の `loc_timestamp_us` が、Timescale 付きの Timestamp を入力トラックの timescale として換算すること (既存の換算テストで確認する)。
- live capture では `PROP_TIMESCALE` を付与しないこと (`build_video_loc_properties_live_capture_has_no_timescale` のテストで確認する)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
