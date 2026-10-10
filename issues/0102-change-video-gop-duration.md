# 映像の Group 長を時間ベースにし fps に依存しないようにする

- Created: 2026-09-18
- Completed: {YYYY-MM-DD}
- Branch: feature/change-video-gop-duration
- Polished: 2026-09-27
- Updated: 2026-10-10

## 目的

映像のキーフレーム間隔がフレーム数固定のため、Group (= GOP) の実時間長が fps によって変わる。fps 5 では 12 秒、fps 120 では 0.5 秒になり、MSF カタログで宣言する `maxGopDuration` / `maxGroupDuration` (ms) も fps ごとに異なる値になり、実態を 1 つの値で表せない。時間ベースに切り替え、5 / 30 / 120 fps のいずれでも同じ Group 長を維持できるようにする。

## 現状

- `examples/moq-pub/src/cli.rs` の `keyframe_interval` は既定 60 フレームで、`fps` と連動しない
- エンコーダ (`examples/moq-pub/src/encoder/av1.rs` の `Av1Encoder::encode`、`h264.rs` の `H264Encoder::encode`、`h265.rs` の `H265Encoder::encode`) は `force_keyframe` / `force_key_frame` を常に `false` で渡しており、任意のフレームでキーフレームを要求できない
- `examples/moq-pub/src/pipeline.rs` のデータループはエンコーダが返した `is_keyframe` で Group を進めるため、Group 長 = キーフレーム間隔になっている
- `examples/moq-pub/src/catalog.rs` の `send_catalog` は `maxGopDuration` / `maxGroupDuration` を設定しない。`shiguredo_moqt` の `MsfTrack` には両フィールドが存在する
- 入力フレームには `VideoFrameOwned` の `timestamp_us` がある (`examples/moq-pub/src/fake_capture.rs` は 0 起点の連続値、カメラ経路はデバイスクレート由来で起源は経路により異なる)。経路別の起源は 0103 に一覧があり、0103 (closed、2026-10-06) の対応で pipeline が capture のメディア時刻を `WallClockMapper` で Unix epoch マイクロ秒へ換算してから `encode()` へ渡すようになった
- 0101 (closed、2026-09-18) の切り分けでは `--keyframe-interval` を小さく (30 fps で 15 フレーム = 0.5 秒) して停滞を緩和する手法が使われている。同 issue はこの緩和が 120 秒では不十分であると訂正しており、fps が変わると同じ設定でも Group 長が変わるため設定の目安が fps ごとに固定できないことが実運用の負担になっている
- MP4 経路が追加されている (0179 / 0180)。`--input-mp4` (パススルー) はエンコーダを作らず Group が MP4 のキーフレームで進み、`--input-mp4-reencode` は MP4 から検出した fps でエンコーダを作る。`--keyframe-interval` は後者でだけ使われる

## 設計方針

- CLI を `--gop-duration` (ms、既定 2000) に置き換える。既定 2000 は現行の 30 fps × 60 フレームと同値。置き換え対象は live capture と `--input-mp4-reencode` の両経路で、`--input-mp4` (パススルー) では `--keyframe-interval` と同様に無視する
- エンコーダの `encode()` (`examples/moq-pub/src/encoder.rs` の `VideoEncoder` enum と codec ごとの実装) に、既存の `timestamp_us` の次の引数としてキーフレーム強制の引数を追加し、`force_keyframe` / `force_key_frame` に反映する
- pipeline は入力フレームの `timestamp_us` を基に判定する。直前の入力フレーム (キーフレームが出力され Group を開始したフレーム) の `timestamp_us` から `gop_duration` (ms) が経過したら、次の入力フレームでキーフレーム強制を要求する (`gop_duration` は比較時に µs へ変換する)。`timestamp_us` は起源が経路により異なるため絶対値でなく差分で判定する。判定は純関数に切り出してテスト可能にする
- エンコーダのフレーム数上限 (`kf_max_dist` / `max_key_frame_interval`) は `ceil(fps × gop_duration / 1000)` フレーム (小数点以下切り上げ) を自動計算し (`--input-mp4-reencode` の fps は MP4 から検出した値を使う。以降の fps も同じ)、時間ベース要求が効かない場合の安全弁として残す。切り上げにすることで安全弁が `gop_duration` より先に発火して Group を短縮しない
- カタログに `maxGopDuration` (= `gop_duration` + 1 フレーム分 = `gop_duration` + `1000 / fps` ms、小数点以下切り上げ) を設定する。`maxGroupDuration` は同じ値とする (Group は 1 キーフレーム間隔 = 1 GOP のため)
- 0103 (LOC Timestamp の wall-clock 化、closed、2026-10-06) は完了済みで、`encode()` は既に `timestamp_us` を受け取る。0102 の判定は入力フレームの `timestamp_us` の差分のみに依存させる
- 購読者からの `NEW_GROUP_REQUEST` による即時キーフレーム要求は 0105 で扱う

## 完了条件

- 5 / 30 / 120 fps のいずれでも映像 Group が `gop_duration` (既定 2000 ms ≈ 約 2 秒) で進むこと (判定ロジックのテストと実機 1 ケース以上)
- フレーム落ちやエンコード遅延があっても Group が `gop_duration` + 1 フレーム程度で進むこと (判定は受信フレームの `timestamp_us` 差分のため、`gop_duration` 経過直後に連続してフレームが落ちた場合はその分だけ伸びる)
- カタログの `maxGopDuration` / `maxGroupDuration` が実測値と整合すること
- `examples/README.md` のオプション表が追随していること
