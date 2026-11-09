# MP4 入力の非キーフレームにも PROP_TIMESCALE を付与する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: 2026-11-10

## 目的

draft-ietf-moq-loc-05 §2.3.1.1 (Timestamp) は Timescale が無い Timestamp を Unix epoch からのマイクロ秒 (壁時計) として解釈し、§2.3.1.2 (Timescale) は Timescale があるとき Timestamp をメディア時刻として扱う。
moq-pub は MP4 パススルー / 再エンコード経路で入力トラックの timescale 単位の Timestamp を送りながら、`PROP_TIMESCALE` をキーフレームにしか付けない。
そのため購読側は同じ映像 Track の中で、キーフレームをメディア時刻、非キーフレームを壁時計として扱い、非キーフレームの入力 timescale の値を epoch マイクロ秒と誤解釈する。MP4 入力の映像の全 Object に Timescale を付与する。

## 現状

- `crates/moq-pub/src/pipeline.rs` の `build_video_loc_properties` は `if frame.is_keyframe` の中でだけ `PROP_TIMESCALE` を push する。同じブロックで `PROP_VIDEO_CONFIG` も push している。
- MP4 入力では `video_timescale` が `Some(入力トラックの timescale)` になり、`EncodedFrame::timestamp` も入力 timescale 単位である。MP4 パススルーでは `Mp4VideoReader::info()` が返す `VideoTrackInfo::timescale`、再エンコードでは `Mp4ReencodeReader::video_info()` が返す `VideoInfo::timescale` を使う。非キーフレームは Timescale なしで送られる。
- `crates/moq-sub/src/pipeline.rs` の `deliver_video_object` は、`extract_timestamp_timescale` で取り出した Timestamp / Timescale から `timestamp_kind_of` で `TimestampKind` を決め、`loc_timestamp_us` の値とあわせて `send_decoded_frames` へ渡す。
  - `timestamp_kind_of` は Timescale が無ければ `TimestampKind::WallClock`、あれば `TimestampKind::MediaTime` を返す。
  - `crates/moq-sub/src/main.rs` の `wall_clock_timestamp` は `WallClock` のフレームだけ値を返す。その値は `timeline.observe(Track::Video, ..)` と `video_buffer.enqueue` に壁時計として入り、`MediaTime` のフレームは到着順に表示される。
  - 現状は、キーフレームが `MediaTime` として到着順に表示され、非キーフレームが `WallClock` として入力 timescale の値を epoch マイクロ秒として時間軸に入れる。同じ Track の中で扱いが混ざる。
- `crates/moq-sub/src/mp4.rs` の `to_microseconds` は観測済みの timescale を後続サンプルへ引き継ぐため、キーフレームに Timescale が付いている現状では録画経路は壊れない。
- テスト `build_video_loc_properties_non_keyframe` が「非キーフレームは Timescale なし」を期待しており、誤った挙動を固定している (`props.timescale()` が `None` であることを assert している)。
- 影響: MP4 入力の映像を再生する経路で、非キーフレームの入力 timescale の値が壁時計として映像の時間軸と表示バッファに入り、表示時刻が崩れる。
  MP4 パススルーは音声を送らない (`cli.rs` の `audio_enabled` は `input_mp4` があると偽)。再エンコードの音声は `build_audio_loc_properties` が全 Object に Timescale を付けるため `MediaTime` になる。

## 設計方針

- draft-ietf-moq-loc-05 §6.1 (MOQ Properties Registry) Table 1 は TIMESCALE (0x08) の Scope を "Track, Object" としているため、Object Properties として毎回付与できる。
- `timescale` が `Some` のときは映像の全 Object に `PROP_TIMESCALE` を付与する。`build_video_loc_properties` の `if frame.is_keyframe` ブロックの外へ `PROP_TIMESCALE` の push を移す。`PROP_VIDEO_CONFIG` は従来どおりキーフレームのみに付与し、このブロックに残す。
- 修正後は MP4 入力の映像の全 Object が `MediaTime` になり、購読側では一貫して到着順に表示される。購読側 (`timestamp_kind_of` / `loc_timestamp_us` / `wall_clock_timestamp`) は変更しない。
- 本 issue では Track スコープでの告知は使わない。closed の 0125 で受信側の公開 API (`Subscription::peer_track_properties`、`TrackProperties::find_bytes`) は入ったが、次の理由で Object ごとの付与にする。
  - moq-sub は `peer_track_properties` を参照せず、moq-pub の `subscribe_ok_properties` / `publish_video_track` も TIMESCALE を Track Properties に載せない。両者に足すのは別目的の作業である。
  - `deliver_video_object` は live と FETCH 応答で共有され、moq-pub の FETCH 応答は `media_cache.insert` に渡した Object ごとの Properties を返す。0125 は FETCH_OK の Track Properties を対象外にしている。
  - `timestamp_kind_of` は Object ごとに状態を持たずに判定するため、全 Object に Timescale があれば Track スコープを参照しなくても分類が揃う。
- live capture は Timestamp が Unix epoch マイクロ秒なので、従来どおり `timescale` を `None` とし、`PROP_TIMESCALE` を付けない。
- `build_video_loc_properties_non_keyframe` を、`timescale` が `Some(90_000)` のときに `props.timescale() == Some(90_000)` を期待するよう修正する。`build_video_loc_properties_keyframe` と `build_video_loc_properties_live_capture_has_no_timescale` は現状の期待を維持する。
- MP4 入力の PTS 間隔の統合検証は 0338 の pipeline 統合テストで扱う。e2e-tests は現状 live capture のみで、MP4 入力を起動する経路と fixture が無いため、本 issue では E2E テストを追加しない。

## 完了条件

- MP4 入力の映像の全 Object に Timescale が付与されること (`build_video_loc_properties_keyframe` と `build_video_loc_properties_non_keyframe` のテストで確認する)。
- Timescale 付きの映像 Object が購読側で `TimestampKind::MediaTime` に分類されること (既存の `timestamp_kind_of_returns_media_time_with_a_timescale` で確認する)。
- live capture では `PROP_TIMESCALE` を付与しないこと (`build_video_loc_properties_live_capture_has_no_timescale` のテストで確認する)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
