# 映像の Group 長を時間ベースにし fps に依存しないようにする

- Created: 2026-09-18
- Completed: {YYYY-MM-DD}
- Branch: feature/change-video-gop-duration
- Polished: {YYYY-MM-DD}

## 目的

映像のキーフレーム間隔がフレーム数固定のため、Group (= GOP) の実時間長が fps によって変わる。fps 5 では 12 秒、fps 120 では 0.5 秒になり、MSF カタログで宣言する `maxGopDuration` / `maxGroupDuration` (ms) とも実態が一致しない。時間ベースに切り替え、5 / 30 / 120 fps のいずれでも同じ Group 長を維持できるようにする。

## 現状

- `examples/moqt-publisher/src/cli.rs` の `keyframe_interval` は既定 60 フレームで、`fps` と連動しない
- エンコーダ (`examples/moqt-publisher/src/encoder/av1.rs` の `Av1Encoder::encode`、`h264.rs` の `H264Encoder::encode`、`h265.rs` の `H265Encoder::encode`) は `force_keyframe` / `force_key_frame` を常に `false` で渡しており、任意のフレームでキーフレームを要求できない
- `examples/moqt-publisher/src/pipeline.rs` のデータループはエンコーダが返した `is_keyframe` で Group を進めるため、Group 長 = キーフレーム間隔になっている
- `examples/moqt-publisher/src/catalog.rs` の `send_catalog` は `maxGopDuration` / `maxGroupDuration` を設定しない。`shiguredo_moqt` の `MsfTrack` には両フィールドが存在する
- 入力フレームには `VideoFrameOwned` の `timestamp_us` がある (`examples/moqt-publisher/src/fake_capture.rs` は 0 起点の連続値、カメラ経路は video-toolbox 由来)
- 0101 では `--keyframe-interval` を小さくして停滞を緩和しており、Group 長が安定しないことが実運用の負担になっている

## 設計方針

- CLI を `--gop-duration` (ms、既定 2000) に置き換える。既定 2000 は現行の 30 fps × 60 フレームと同値
- エンコーダの `encode()` にキーフレーム強制の引数を追加し、`force_keyframe` / `force_key_frame` に反映する
- pipeline は「直前に出力したキーフレームの `timestamp_us` から `gop_duration` 経過」で次のキーフレームを要求する。判定は純関数に切り出してテスト可能にする
- エンコーダのフレーム数上限 (`kf_max_dist` / `max_key_frame_interval`) は fps と `gop_duration` から自動計算し、時間ベース要求が効かない場合の安全弁として残す
- カタログに `maxGopDuration` (`gop_duration` + 1 フレーム分) と `maxGroupDuration` を設定する
- 購読者からの `NEW_GROUP_REQUEST` による即時キーフレーム要求は 0105 で扱う

## 完了条件

- 5 / 30 / 120 fps のいずれでも映像 Group が約 2 秒で進むこと (判定ロジックのテストと実機 1 ケース以上)
- フレーム落ちやエンコード遅延があっても Group が `gop_duration` + 1 フレーム以内で進むこと
- カタログの `maxGopDuration` / `maxGroupDuration` が実測値と整合すること
- `examples/README.md` のオプション表が追随していること
