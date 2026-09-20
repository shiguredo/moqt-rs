# moq-pub に MP4 ファイルの再エンコード配信機能を追加する

- Created: 2026-09-29
- Completed: 2026-09-29
- Branch: feature/add-publisher-mp4-reencode
- Polished: 2026-09-29

## 目的

`moq-pub` の入力はカメラ / マイクのみで、MP4 ファイルを任意の配信設定 (コーデック・ビットレート) で配信できない。MP4 ファイルをデコードし、既存のエンコーダで再エンコードして配信できるようにする。sora-rust-sdk の MP4 入力は再エンコードなしのパススルーだけで音声入力も未対応のため、この機能は `moq-pub` 独自のものになる。moq-sub が `--mp4` で保存した MP4 (AV1 / H.264 / H.265 + Opus) を入力にでき、保存と配信の往復確認ができるようになる。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `run` はカメラ / マイクまたは raden 疑似入力のフレームを `encoder::VideoEncoder` / `encoder::opus::OpusEncoder` でエンコードして送信する。デコード機能は持たない。
- `examples/moq-sub` はデコーダを持っている。映像は `decoder::build_video_decoder` (AV1 は `shiguredo_dav1d`、H.264 / H.265 は `shiguredo_video_toolbox` の macOS 限定)、音声は `decoder::opus::OpusDecoder` (`shiguredo_opus`)。
- sora-rust-sdk の MP4 入力 (`docs/INPUT_MP4.md` / `src/video_codecs/mp4.rs`) はパススルーのみで再エンコードを行わない。音声入力も pending。

## 設計方針

- 「moq-pub に MP4 ファイルのパススルー配信機能を追加する」の実装を前提とし、その MP4 リーダー (demux・実時間ペーシング・サンプル供給) を再利用する。リーダーは音声サンプルもトラック別に供給できるよう拡張する。
- CLI に `--input-mp4-reencode <PATH>` を追加する (`--input-mp4` との同時指定はエラー)。MP4 を入力のまま、再エンコードして配信する。
  - `--video-codec`: 出力コーデックの選択に使う (既定 av1)。
  - `--bitrate` / `--keyframe-interval` / `--audio-bitrate`: 再エンコードの設定として使う。
  - `--width` / `--height` / `--fps`: MP4 から自動検出するため明示指定はエラーにする。
  - `--no-video` / `--no-audio`: 指定されたトラックを配信しない (音声のみ / 映像のみの再エンコードを許容する。両方指定は既存の CLI 検証でエラーになる)。
  - `--device-id` / `--fake-capture-device` / `--audio-device-id`: 無視する (警告を出す)。
  - `--use-datagram`: 既存の分岐を再利用する。ただし moq-sub は datagram のメディアを処理しないため、完了条件の再生確認には含めない。
- 映像:
  - 入力コーデックは AV1 / H.264 / H.265 に対応する。AV1 は `shiguredo_dav1d`、H.264 / H.265 は `shiguredo_video_toolbox` (macOS 限定) でデコードする。moq-sub の `decoder` と同等のモジュールを moq-pub に追加し、`shiguredo_dav1d` を `Cargo.toml` に追加する (example 間でコードは共有しない)。
  - デコード設定は MP4 のサンプルエントリーから取る。H.264 / H.265 は avcC / hvcC を `VideoDecoder::decode` の `video_config` として渡し、AV1 は `Av1cBox.config_obus` をキーフレームサンプルの先頭に付与してからデコードする (moq-sub の AV1 デコーダは payload 内の Sequence Header を前提とするため)。設定が取れないトラックは初期化時にエラーにする。
  - デコード結果 (I420) を packed NV12 (`stride = width`、`uv_data` に UV をインターリーブ) に変換して `VideoFrameOwned` を構築し、既存の `encoder::VideoEncoder` (`--video-codec`) に渡す。既存エンコーダは stride 付き入力を取らないため、パディングの無いバッファにする。
  - 8-bit 4:2:0 以外 (10-bit 等) の映像は対象外とし、初期化時にエラーにする。
  - B フレームを含む MP4 を許容する。0179 の B フレーム拒否はパススルー送信の要件であり、リーダーは `composition_time_offset` を保持して呼び出し側に渡すよう拡張する (パススルー経路は従来どおり拒否する)。再エンコードはデコード順 (DTS) でデコードし、表示順 (PTS) でエンコードへ供給する。
  - 解像度とフレームレートは MP4 から自動検出する。可変フレームレートの MP4 は平均フレームレートに丸めて固定 fps で送る (警告を出す)。
- 音声:
  - MP4 の音声トラックが Opus の場合に対応する。`shiguredo_opus::Decoder` で 48 kHz / 1ch を指定して PCM にデコードし (ステレオ入力は libopus のダウンミックスに任せる)、既存の `OpusEncoder` (48 kHz / 1ch、`--audio-bitrate`) で再エンコードする。
  - デコードした PCM は `samples_per_frame` (960 サンプル = 20 ms) 単位でバッファリングしてエンコードする (capture 経路と同じ)。
  - AAC など Opus 以外の音声トラックは警告して無視する (対応は別 issue とする)。
- タイムスタンプ:
  - LOC Timestamp は入力 MP4 のサンプルタイムスタンプから pipeline が計算して付与し、エンコーダの `frame_count * timescale / fps` には依存しない。`PROP_TIMESCALE` は入力トラックの timescale を使う。
  - 映像の出力フレームには、そのフレームを生成した入力サンプルの PTS (`timestamp + composition_time_offset`) を対応付ける (0 フレームを返した入力サンプルの PTS は次に出力されたフレームへ引き継ぐ)。`EncodedFrame.timestamp` は使わない。
  - 音声は入力の先頭サンプルのタイムスタンプを起点に、エンコード済みサンプル数を加算して求める。
  - 入力 MP4 の編集リスト (`elst`) は適用しない。`shiguredo_mp4` の `Mp4FileDemuxer` は編集リストを公開せず、サンプルのタイムスタンプはトラック内の 0 起点の DTS になるためである。編集リストを持つ MP4 (例: ffmpeg が生成した AV1 + Opus) では編集リストのオフセット分だけ A/V がずれる可能性がある。moq-sub の MP4 保存は編集リストを書かないため往復確認には影響しない。編集リストの適用が必要になった時点で別 issue とする。
  - subscriber 側の A/V 同期は別 issue の範囲とし、本 issue では扱わない。
  - エンコーダのタイムスタンプ採番を変更する将来の設計 (capture のタイムスタンプを載せる等) が入っても、本設計は encoder の採番に依存しないため影響を受けない。
- フレームの供給:
  - リーダーは全サンプルを入力順に供給し、間引かない。パイプラインのチャネルが満杯の場合は送信側で待機 (backpressure) し、実時間より遅れて配信されることを許容する。
- ループ:
  - 末尾に達したら先頭に戻る。ループ周期は映像と音声のトラック尺の最大値とする。
  - 各トラックは周期の先頭から再開し、周期より短いトラックは残りを送信しない (映像または音声が無い区間になる)。端数は次の周回に繰り越さない。これにより周回ごとの A/V ずれが蓄積しない。
- `NEW_GROUP_REQUEST` は次のように扱う。
  - エンコーダの強制キーフレーム API と `DYNAMIC_GROUPS=1` の告知が実装済みであれば、要求に従って次の入力フレームで強制キーフレームを生成し、新しい group を開始する。
  - 未実装の間はパススルーと同じく次の自然なキーフレームで group を開始する (draft-ietf-moq-transport-21 §9.20.20 が許容する遅延)。

## 完了条件

- `cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --input-mp4-reencode input.mp4 --video-codec av1` で MP4 が再エンコードされて MoQ 配信され、moq-sub で再生できる。
- AV1 / H.264 / H.265 の MP4 を入力にでき、macOS 以外では AV1 + Opus の MP4 で動作する。B フレームを含む H.264 / H.265 の MP4 でも配信できる。
- Opus 以外の音声トラックでは警告を出して映像のみ配信する。`--no-video` / `--no-audio` で片方のトラックのみ配信できる。
- 8-bit 4:2:0 以外の映像では分かりやすいエラーで終了する。
- `--width` / `--height` / `--fps` の同時指定がエラーになる。
- `--input-mp4` / `--input-mp4-reencode` を指定しない場合の動作と出力が変わらない。
- moq-sub の `--mp4` で保存した MP4 を入力にして再配信し、別の moq-sub で再生できる (往復確認。moq-sub の MP4 保存の実装後に実施し、それまでは外部ツールで生成した AV1 + Opus の MP4 で代替確認する)。
- 単体テストで、MP4 のサンプルタイムスタンプから LOC プロパティへの変換、入力順のフレーム供給、ループ周期の算出を固定する。デコードとエンコードの結合は実機確認とする。
- `CHANGES.md` に ADD を追記し、`examples/README.md` の moq-pub オプション表に `--input-mp4-reencode` を追記する。
- `make test` / `make clippy` / `make fmt` が通る。

## 解決方法

- `examples/moq-pub/src/mp4/reencode.rs` を追加し、0179 の MP4 リーダー (ファイル全体の demux・サンプル走査・範囲検証・タイムスタンプ変換) を共有しつつ、映像 / 音声トラックのデコードと供給を行う `Mp4ReencodeReader` を実装した。ファイル読み込みは `mp4.rs` の共通ヘルパ (`read_mp4_file`) に集約した。
- デコーダは `examples/moq-pub/src/decoder.rs` と `decoder/{av1,h264,h265,opus}.rs` に追加した (example 間でコードは共有しない方針のため moq-sub から移植)。
  映像は AV1 を `shiguredo_dav1d`、H.264 / H.265 を `shiguredo_video_toolbox` (macOS 限定) でデコードし、I420 を packed NV12 (stride = width、UV をインターリーブ) に変換して既存のエンコーダへ供給する。
  音声は Opus のみ対応し、48 kHz / 1ch にデコードして 20 ms (960 サンプル) 単位でバッファリングする。Opus 以外の音声は警告して対象外にする。
- CLI に `--input-mp4-reencode <PATH>` を追加した。`--input-mp4` との同時指定、`--width` / `--height` / `--fps` の明示指定はエラーにし、`--device-id` / `--fake-capture-device` / `--audio-device-id` は無視して警告する。`--video-codec` / `--bitrate` / `--keyframe-interval` / `--audio-bitrate` は再エンコードの設定として使う。解像度とフレームレートは MP4 から自動検出する。
- 8-bit 4:2:0 以外の映像 (10-bit、4:2:2、4:4:4、モノクロ) と、偶数でない解像度、avcC / hvcC のパラメータセット欠落、av1C にもキーフレームにも AV1 の Sequence Header が無いファイルは初期化時にエラーにする。
- B フレームを含む MP4 に対応した。デコード順 (DTS) でデコーダへ入力し、PTS 待ち行列 (最小値割り当て) と表示順の PTS 列 (`open()` で全サンプルの PTS を昇順に保持) による並べ替えバッファ (`VideoReorder`) で表示順にエンコードへ供給する。Video Toolbox はデコード順で出力するため、この並べ替えが必要であることを実機 (B フレーム入り H.264) で確認した。周回の先頭では期待値の PTS 列に周回オフセットを反映して作り直し、先頭キーフレームより前のサンプルは表示順の対象から除外する。
- ループの周期は映像と音声のトラック尺の最大値 (表示終端 `PTS + duration` も考慮) とし、各トラックは周期の先頭から再開する。周期より短いトラックは残りを送信しない。Opus の pre-skip (RFC 7845 §4.2) は周回ごとに先頭で読み飛ばす。
- 実時間ペーシングは行わず、パイプラインのチャネルが満杯の場合は専用スレッドが待機する (バックプレッシャー)。実時間より遅れて配信されることを許容する。
- タイムスタンプは入力 MP4 のサンプルタイムスタンプから付与する。映像はデコード結果を表示順の PTS に対応付け、0 フレームを返した入力の PTS は次に出力されたフレームへ引き継ぐ (FIFO)。音声は入力のタイムスタンプを起点に 20 ms ずつ進める。`PROP_TIMESCALE` は入力トラックの timescale を使い、編集リストは適用しない。
- テストは moq-pub に 72 件 (うち reencode モジュール 13 件) を追加し、PTS 変換・表示順の並べ替え (同一 PTS と周回オフセットを含む)・周回周期の算出・8-bit 4:2:0 検証・音声フレーム構築・CLI 検証に加え、実際の AV1 / Opus エンコード → MP4 mux → デコード供給の結合テストと、映像 0.1 秒 + 音声 0.06 秒の 2 トラック MP4 による周回テスト (周期ごとのオフセットと短いトラックの打ち切り) を固定した。
- 実機確認として ffmpeg で生成した MP4 (H.264 (B フレームあり / なし)、H.265 (hvc1, 8-bit 4:2:0 / RGB)、AV1 (SVT-AV1)、VP9、AAC のみ、AV1 + Opus) を `--input-mp4-reencode` で読み込ませた。
  対応コーデックは受理され、B フレーム入り H.264 では供給タイムスタンプが表示順に並ぶことを確認した。8-bit 4:2:0 以外・未対応コーデック・映像トラック無しは期待どおり拒否される。
  relay と接続した moq-sub での再生確認 (H.264 / H.265 は macOS の Video Toolbox 使用) と、moq-sub の `--mp4` 出力を入力にした往復確認は未実施である。
- `/review-diff-code` を 2 周 + 検証 2 周実行し、致命的・重要の指摘はすべて修正済み (最終的に 0 件)。
