# moq-pub に MP4 ファイルのパススルー配信機能を追加する

- Created: 2026-09-29
- Completed: 2026-09-29
- Branch: feature/add-publisher-mp4-passthrough
- Polished: 2026-09-29

## 目的

`moq-pub` はカメラ / マイク入力のみを扱うため、既存の MP4 ファイルを配信できない。sora-rust-sdk が持つ MP4 パススルー送信 (エンコード済みサンプルを再エンコードせずに送る機能) と同等の機能を `moq-pub` に追加し、カメラ不要で再現性のある配信ソースで MoQ の動作確認や負荷試験を行えるようにする。再エンコードしないためエンコード負荷がかからず、入力の画質もそのまま保たれる。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `run` は `capture::start_capture` または `fake_capture::start_capture` から `shiguredo_video_device::VideoFrameOwned` を受け取り、`encoder::VideoEncoder::encode` でエンコードした `EncodedFrame` を `SubgroupWriter` / `DatagramWriter` で送信する。ファイル入力を読み込む経路はない。
- `examples/moq-pub/src/cli.rs` の `Config` は `--device-id` / `--fake-capture-device` / `--video-codec` などカメラ前提のオプションのみで、MP4 を指定できない。
- MSF catalog は `catalog::send_catalog` に encoder から取得した codec 文字列・幅・高さ・ fps を渡して構築しており、ファイル由来のトラック情報を渡す経路はない。
- sora-rust-sdk では `docs/INPUT_MP4.md` と `src/video_codecs/mp4.rs` が MP4 パススルー送信を提供している。`shiguredo_mp4` の demux でサンプルを取り出し、`Mp4SampleReader` / `Mp4VideoCapturer` / `Mp4PassthroughEncoder` で実時間ペーシングして送る。
- sora-rust-sdk の MP4 パススルーの制約は「映像のみ (音声は無視)」「B フレームを含む MP4 は拒否」「キーフレーム要求は無視」「末尾でループ」である。対応コーデックは H.264 / H.265 / VP8 / VP9 / AV1 で、音声入力は pending。
- `examples/moq-sub` の `build_video_decoder` が対応する映像コーデックは av01 / avc1 / hvc1 / hev1 のみで、VP8 / VP9 は再生できない。

## 設計方針

- `examples/moq-pub/Cargo.toml` に `shiguredo_mp4 = "2026.5"` を追加し、`demux::Mp4FileDemuxer` と `codec_string::from_sample_entry` を使う。
- CLI に `--input-mp4 <PATH>` を追加する。指定時はカメラ / raden 疑似キャプチャを使わず、MP4 の映像トラックを配信する。
  - コーデックは MP4 から自動判定するため `--video-codec` との同時指定はエラーにする。
  - `--width` / `--height` / `--fps` は MP4 から自動検出するため明示指定はエラーにする。
  - `--no-video` は配信対象が無くなるため同時指定はエラーにする。`--no-audio` は指定の有無にかかわらず音声トラックを配信しない (指定しても動作は変わらない)。
  - `--device-id` / `--fake-capture-device` / `--keyframe-interval` は無視する (警告を出す)。
  - `--bitrate` は MSF catalog の `bitrate` (video track に MUST) を MP4 から算出するため無視する (警告を出す)。
- 対応映像コーデックは AV1 / H.264 / H.265 とする。moq-sub が再生できない VP8 / VP9 は対象外とする。
- B フレーム (composition time offset が非ゼロのサンプル) を含む MP4 は初期化時に拒否する (sora-rust-sdk と同じ)。
- MP4 の読み込みとペーシングは専用スレッドで行い、サンプルのタイムスタンプに従って実時間で既存のパイプラインへ `EncodedFrame` を渡す。
- 末尾に達したら先頭に戻ってループする。ループ後のタイムスタンプには 1 周分のメディア尺を加算して単調増加させる。1 周分のメディア尺は最終サンプルの `timestamp + duration` とする (`TrackInfo.duration` は mdhd の値であり、編集リストやサンプル数と一致しない場合があるため使わない)。
- MP4 の読み込み (demux)・実時間ペーシング・サンプル供給は、パススルー送信から分離したリーダーとして実装する。再エンコード配信も同じリーダーを再利用できる構成にする。
- パイプラインにエンコーダを通さない映像経路を追加する (`VideoSource` のような Enum で「キャプチャ + エンコード」と「MP4 パススルー」を切り替える)。
- LOC プロパティは次の形式で付与する。MP4 のメディア時刻をそのままタイムラインに使う必要があるため、`PROP_TIMESTAMP` / `PROP_TIMESCALE` は MP4 の値を使う (カメラ入力の時刻方針とは独立した扱いとする。音声を配信しないため A/V 同期の対象外である)。
  - `PROP_TIMESTAMP` / `PROP_TIMESCALE` は MP4 のサンプルタイムスタンプと timescale をそのまま使う。
  - `PROP_VIDEO_CONFIG` は MP4 のサンプルエントリーから取り出す (H.264: `Avc1Box` の avcC、H.265: `Hvc1Box` / `Hev1Box` の hvcC、AV1: `Av01Box` の av1C の config OBUs)。キーフレームに付与する。
  - AV1 はサンプルに Sequence Header が含まれない場合があるため、キーフレームの先頭に config OBUs を付与する (moq-sub の AV1 デコーダは payload 内の Sequence Header を前提とする)。
  - `PROP_VIDEO_FRAME_MARKING` は既存と同じくキーフレームで I ビットを立てる。
- MSF catalog は `codec_string::from_sample_entry` の文字列と MP4 から取得した幅・高さ・フレームレート・ビットレートで構築する。
  - フレームレートはサンプル数と尺から平均値を求め、四捨五入して u32 に丸める (VFR の場合は警告を出す)。
  - `bitrate` は draft-ietf-moq-msf-01 §5.2.22 が video track に MUST とする最大ビットレートであり、サンプルサイズと duration から 1 秒幅のスライディングウィンドウの最大値 (bps) を算出して載せる (尺が 1 秒未満なら全尺で計算する)。この計算は payload を読まずにサンプルメタデータだけで行う。
  - 算出値は bps のため 1000 で割り、切り上げた kbps を `catalog::VideoTrackParams.bitrate` (kbps) に渡す (最大ビットレートの過小報告を避けるため切り捨てはしない)。
- group はキーフレームごとに開始する。MP4 パススルーは再エンコードしないため強制キーフレームを生成できず、`NEW_GROUP_REQUEST` を受けても即座に新しい group を開始せず、次の自然なキーフレームで開始する (draft-ietf-moq-transport-21 §9.20.20 が許容する遅延として扱う)。カメラ入力の強制キーフレーム経路とは分岐する。
- `--use-datagram` との併用は既存の分岐を再利用する。ただし moq-sub は datagram のメディアを処理しないため、完了条件の再生確認には含めない。

## 完了条件

- `cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --input-mp4 input.mp4` で MP4 の映像が MoQ 配信され、moq-sub で再生できる。
- AV1 / H.264 / H.265 の MP4 で動作する。H.264 / H.265 は macOS、AV1 は全プラットフォームで確認する。
- `--input-mp4` では音声トラックを PUBLISH せず、catalog にも audio を含めない (`--no-audio` の指定有無にかかわらず同じ)。
- B フレームを含む MP4、映像トラックを持たない MP4、未対応コーデックの MP4 では分かりやすいエラーで終了する。
- `--input-mp4` と `--video-codec` / `--width` / `--height` / `--fps` / `--no-video` の同時指定がエラーになる。
- `--input-mp4` を指定しない場合の動作と出力が変わらない。
- 単体テストで、サンプルエントリーから `PROP_VIDEO_CONFIG` への変換 (avcC / hvcC / av1C)、サンプルタイムスタンプの変換、ループ時のタイムスタンプ加算 (1 周分の尺の算出)、catalog の fps / bitrate 算出を固定する。実ファイルでのパススルー確認は moq-sub との結合で行う。
- `CHANGES.md` に ADD を追記し、`examples/README.md` の moq-pub オプション表に `--input-mp4` を追記する。
- `make test` / `make clippy` / `make fmt` が通る。

## 解決方法

- `examples/moq-pub/src/mp4.rs` を追加し、`shiguredo_mp4 = "2026.5"` の `Mp4FileDemuxer` でファイル全体を demux する `Mp4VideoReader` を実装した。`open()` で映像トラックの検証・catalog 用メタデータ (codec 文字列 / 解像度 / 平均 fps / 最大ビットレート) の算出・`PROP_VIDEO_CONFIG` の取り出しを行い、`start()` で実時間ペーシングを行う専用スレッド (`mp4-reader`) を起動する。
- パイプラインは既存のキャプチャ経路と共通の `VideoInput` enum (`Raw` / `Encoded`) を受け取るようにし、MP4 パススルーではエンコード済みサンプルをそのまま `SubgroupWriter` / `DatagramWriter` へ流す。group は MP4 のキーフレームで開始し、各周回の先頭は最初のキーフレームまで読み飛ばす。末尾に達したら先頭に戻り、タイムスタンプに 1 周分の尺 (`最終サンプルの timestamp + duration`) を加算して単調増加させる。
- ペーシングは `loop_start` を周回ごとに 1 周分進める方式にし、最終サンプルの尺も実時間として待つ。送信が遅れて周回の終端を過ぎた場合は追い上げ送信を避けるため基準を現在時刻に戻す。
- 検証は `open()` で全サンプルを走査して行う。B フレーム (composition time offset が非ゼロ)、映像トラック / キーフレーム / 映像サンプルの不在、ファイル外を指すサンプル、未対応コーデック (AV1 / H.264 / H.265 以外)、avcC / hvcC のパラメータセット不在、AV1 の Sequence Header 不在を分かりやすいエラーで拒否する。
- `PROP_VIDEO_CONFIG` は avcC / hvcC のレコード本体 (ボックスヘッダを除く) を送る。AV1 はカメラ経路と同じく Sequence Header OBU だけを送り、av1C の config OBUs に Sequence Header が無い適合ファイルにも対応するため、最初のキーフレーム payload の Sequence Header を優先して使う。キーフレーム payload に Sequence Header が含まれない場合は付与する。
- CLI に `--input-mp4 <PATH>` を追加し、`--video-codec` / `--width` / `--height` / `--fps` / `--no-video` との併用はエラーにした。
  `--device-id` / `--fake-capture-device` / `--keyframe-interval` / `--bitrate` / `--audio-device-id` / `--audio-bitrate` は無視して警告する。
  音声トラックは配信せず catalog にも含めない (`--no-audio` の指定有無にかかわらず同じ)。MP4 の読み込みは接続前に済ませ、不正な入力を relay の接続可否に依存せず報告する。
- MSF catalog の値は `codec_string::from_sample_entry` の codec 文字列、サンプルエントリーの解像度、サンプル数と尺から四捨五入した平均 fps、1 秒幅の半開区間 `(t - 1 秒, t]` のスライディングウィンドウの最大値 (周回境界も考慮) を切り上げた kbps を使う。
- テストは moq-pub に 57 件 (うち mp4 モジュール 25 件) を追加し、サンプルエントリーから `PROP_VIDEO_CONFIG` への変換 (avcC / hvcC / av1C)、タイムスタンプの実時間変換と周回時の加算、周回長の下限、最大ビットレート (境界値・周回境界)、平均 fps の丸め、AV1 の Sequence Header の取り出しと付与、各エラーパス、CLI の併用エラーと音声無効化を固定した。
- 実機確認として ffmpeg で生成した MP4 (H.264 (B フレームあり / なし)、H.265 (hvc1)、AV1 (SVT-AV1)、VP9、AAC のみ) を `--input-mp4` で読み込ませ、対応コーデックは catalog 値付きで受理され、B フレーム・未対応コーデック・映像トラック無しは期待どおり拒否されることを確認した。relay と接続した moq-sub での再生確認 (H.264 / H.265 は macOS の Video Toolbox 使用) は未実施である。
- `/review-diff-code` を 3 周 + 検証 1 周実行し、致命的・重要の指摘はすべて修正済み (最終的に 0 件)。
