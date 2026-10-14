# 映像の Group 長を時間ベースにし fps に依存しないようにする

- Created: 2026-09-18
- Completed: 2026-10-14
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

## 解決方法

映像の Group 長をフレーム数ではなく時間で指定できるようにした。`--keyframe-interval` (フレーム数、
既定 60) を削除し、`--gop-duration` (ms、既定 2000) を追加する。入力フレームの Timestamp の
差分が `gop_duration` に達したらキーフレームを強制して新しい Group を開始するため、fps を
変えても Group の実時間長は変わらない。

### 変更内容

- `examples/moq-pub/src/gop.rs` (新規): `GopTracker` (Group の開始時刻とキーフレーム要求の状態) と、
  ミリ秒 → マイクロ秒の換算、キーフレーム間隔のフレーム数上限 `ceil(fps × gop_duration / 1000)`、
  カタログへ宣言する値 `gop_duration + ceil(1000 / fps)` ms を置く
- `examples/moq-pub/src/cli.rs`: `--keyframe-interval` を削除し `--gop-duration` (既定 2000 ms) を
  追加する。0 と数値でない値はエラーにし、`--input-mp4` と同時に指定した場合は無視する警告を出す
- `examples/moq-pub/src/encoder.rs` と `examples/moq-pub/src/encoder/av1.rs` /
  `vpx.rs` / `h264.rs` / `h265.rs`: `encode()` にキーフレーム強制の引数を追加し、各エンコーダの
  `force_keyframe` / `force_key_frame` へ渡す。キーフレーム間隔の引数は「フレーム数上限
  (安全弁)」とし、0 はエラーにする
- `examples/moq-pub/src/pipeline.rs`: 入力フレームの Timestamp (live capture は epoch
  マイクロ秒、MP4 の再エンコードは入力トラックのタイムスケール単位を換算した値) の差分で
  キーフレームを要求する。Group を開始した出力フレームを観測するたびに基準を進め、実測の
  Group 長を debug ログに出す
- `examples/moq-pub/src/catalog.rs`: 映像トラックに `maxGopDuration` / `maxGroupDuration` を載せる
- `examples/README.md` と `e2e-tests/tests/roundtrip.rs`: `--gop-duration` の説明と用語を追随させる

### 判断と根拠

- **Group 長は時間で指定する**: Group はキーフレームで始まるため (draft-ietf-moq-loc-04 §4.2)、
  キーフレーム間隔をフレーム数で指定すると Group の実時間長が fps に依存する (5 fps で 12 秒 /
  120 fps で 0.5 秒)。MSF カタログの `maxGopDuration` / `maxGroupDuration` も fps ごとの値になり、
  1 つの値で実態を表せない
- **判定は入力フレームの Timestamp の差分で行う**: live capture の Timestamp は経路によって
  起源が異なるため、絶対値ではなく差分を使う
- **要求したキーフレームを観測するまで再要求しない**: エンコーダによっては出力が非同期に
  返るため、要求したフレームの出力が同じ呼び出しで返らないと、次の入力フレームでも要求が
  重なってキーフレームが連続し 1 フレームの Group ができる。要求を記録して観測まで抑える
- **Group 開始の基準は出力キーフレームの観測で進める**: エンコーダがフレーム数の上限で
  自主的に出したキーフレームでも基準を進めないと、その直後に時間ベースの要求が重なって
  1 フレームの Group ができる
- **安全弁のフレーム数は切り上げる**: 切り下げると、公称 fps でも安全弁が時間ベースの要求より
  先に発火して Group を短縮し得る。0 を渡すとエンコーダによって上限を設定せず既定値に委ねるか、
  上限無しとして扱われるため受け付けない
- **カタログの宣言は `gop_duration` + 1 フレーム分**: 判定は入力フレームの Timestamp の差分で
  行うため、境界の直後にフレームが無いと次の入力まで Group が伸びる。Group は 1 キーフレーム
  間隔 = 1 GOP であるため `maxGopDuration` / `maxGroupDuration` に同じ値を載せる
  (draft-ietf-moq-msf-01 §5.2.24 / §5.2.25)。入力のゆらぎが 1 フレームの切り上げ分を超える場合、
  フレームが落ちた場合、`--input-mp4-reencode` で可変フレーム長の MP4 を入力にした場合は実測が
  この値を超え得る (宣言は公称 fps で境界の 1 フレームを見込んだ値であり、Group 長の厳密な
  上限ではない)
- **MP4 パススルーでは宣言しない**: Group が MP4 のキーフレームで進み `--gop-duration` を使わない
  ため、`maxGopDuration` / `maxGroupDuration` を載せない

### テスト

- `examples/moq-pub/src/gop.rs`: 5 / 30 / 120 fps で Group が同じ実時間長で進むこと、境界
  (未満・ちょうど・超過) と要求直後に再要求しないこと、安全弁のキーフレームの直後で要求しない
  こと、フレーム落ち・ゆらぎ・時刻の巻き戻り・桁あふれ・fps 0 の扱い
- `examples/moq-pub/src/cli.rs`: `--gop-duration` の既定値・上書き・0 と数値でない値の拒否、
  削除した `--keyframe-interval` の拒否
- `examples/moq-pub/src/catalog.rs`: 同じ値が `maxGopDuration` と `maxGroupDuration` に載ることと、
  宣言しない場合に載らないこと
- `examples/moq-pub/src/encoder/av1.rs` と `vpx.rs`: 強制したフレームがキーフレームとして出力
  されること (AV1 / VP8 / VP9)
- `examples/moq-pub/src/pipeline.rs`: 出力フレームの Timestamp の換算

### スコープ外 (別 issue)

- H.264 / H.265 の強制キーフレームのエンコードテストは、依存する `shiguredo_video_toolbox`
  2026.1.1 が NV12 入力を扱えず失敗するため入れていない (同クレートの更新で解消する)
- `pipeline::run` と `GopTracker` の結線 (キーフレーム要求・観測の呼び出し位置) は、
  `pipeline::run` を走らせるテストが無く単体テストで守られていない

### 確認

- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通る
- 実 relay に `moq-pub` を接続し、Group の実測長が 5 fps で 1993〜2004 ms、30 fps で
  1982〜2002 ms、120 fps で 1995〜2000 ms (いずれも平均 2000 ms) であることを debug ログで
  確認した。カタログの宣言値 (5 fps: 2200 / 30 fps: 2034 / 120 fps: 2009 ms) は実測を上回っている
- `e2e-tests` の publish / subscribe 往復が実 relay で通る
