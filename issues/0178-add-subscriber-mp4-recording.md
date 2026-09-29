# moq-sub に受信メディアの MP4 保存機能を追加する

- Created: 2026-09-29
- Completed: 2026-09-29
- Branch: feature/add-subscriber-mp4-recording
- Polished: 2026-09-29

## 目的

`moq-sub` は受信した映像・音声をデコードして再生するだけで、保存する手段がない。配信されたメディアを MP4 ファイルとして保存できるようにし、録画した内容を後から再生・確認できるようにする。また、再生用の SDL ウィンドウを開けない環境 (GUI のないサーバー等) でも受信と保存を行えるようにする。

## 現状

- `examples/moq-sub/src/main.rs` の `main` はデコード済みフレームを `std::sync::mpsc` でメインスレッドへ渡し、`run_raw_player` が `raw_player` (SDL) で再生する。`raw_player::init()` を必ず呼ぶため、映像ウィンドウを開けない環境では動作しない。
- `examples/moq-sub/src/pipeline.rs` の `run` は `receive_catalog` で catalog を取得して video / audio トラックを SUBSCRIBE し、`handle_incoming_stream` → `decode_video_stream` / `decode_audio_stream` でデコードする。受信したエンコード済みペイロードはデコード後すぐに破棄され、ファイルには残らない。
- `decode_and_send` が送出したフレーム数に応じて `display_backlog` を増やし、`MAX_DISPLAY_BACKLOG` を超えた video group はデコード前に破棄する (`handle_stream_body` の `StreamType::Subgroup` / `StreamType::Fetch` の分岐)。
- LOC の `PROP_TIMESTAMP` / `PROP_TIMESCALE` は `extract_timestamp_timescale` で取り出し済みで、音声の PTS 計算 (`audio_pts_us`) に使っている。映像は PTS を使っていない。
- `PROP_VIDEO_CONFIG` / `PROP_AUDIO_CONFIG` は `extract_video_config` / `extract_audio_config` で取り出し済みで、映像デコーダの初期化と音声設定の検証に使っている。
- datagram 経由の object は `DataPlaneHandle::recv_datagram` が `DatagramAcceptance` しか返さないため、現行の moq-sub は payload を取り出していない。生バイト列は `StreamHandle::recv_datagrams` が返すため取得自体は可能だが、application 側での payload 抽出とデコードは未実装である。
- `examples/moq-sub/Cargo.toml` に MP4 関連の依存はない。

## 設計方針

- 受信したエンコード済みサンプル (AV1 / H.264 / H.265 / Opus) を再エンコードせずに MP4 へ mux する。録画対象は SUBSCRIBE したトラックに合わせる。
- `shiguredo_mp4` を `examples/moq-sub` の依存に追加する (`2026.5`)。`Mp4FileMuxer` (MP4 ファイル mux) と `bitstream` 配下のサンプルエントリー構築 API を使う。
- 専用の OS スレッド (`std::thread`) を立て、そのスレッドがファイル I/O と `Mp4FileMuxer` を所有する。pipeline 側は `std::sync::mpsc::Sender` でサンプルを送るだけにして、tokio ランタイムで blocking I/O を行わない。
  - ファイルは最初のサンプルを受信するまで open しない。録画対象のサンプルが 0 件の場合はファイルを作らず、既存ファイルの truncate もこの時点で行う。
  - `pipeline::run` は全終了経路 (正常終了・エラー・Ctrl+C) で Sender を drop してライタースレッドを join する。ライタースレッドはチャネルの切断を検知したら reorder window に残ったサンプルを flush し、`Mp4FileMuxer::finalize()` と `FinalizedBoxes::offset_and_bytes_pairs()` によるファイルへの書き戻しが完了してから終了する。
  - main は `--no-play` では pipeline スレッド (ライターの finalize を含む) の終了を待ってから終了する。プレイヤーが終了した場合 (ウィンドウを閉じた等) は shutdown を起動して pipeline スレッドの終了を待ってからプロセスを終了する。
- CLI に `--mp4 <PATH>` を追加する。指定時にのみ録画し、既存ファイルは truncate して上書きする。
- CLI に `--no-play` を追加する。指定時は `raw_player` を初期化せず、デコードも行わない (録画のみ)。`--mp4` を併せて指定しない場合は受信のみで何も保存しない旨を警告する。
- タイムスタンプは LOC の `PROP_TIMESTAMP` / `PROP_TIMESCALE` からマイクロ秒へ変換し、トラックのタイムスケールを 1_000_000 に固定して扱う。
  - `PROP_TIMESCALE` がある場合は `timestamp * 1_000_000 / timescale` で変換する。
  - video の非キーフレームは `PROP_TIMESCALE` を持たないため、トラックごとに直前に観測した timescale を保持して適用する。
  - timescale を一度も観測していないトラックでは、draft-ietf-moq-loc-04 §2.3.1.1 の既定に従い `timestamp` を Unix エポックからのマイクロ秒としてそのまま使う。
- サンプルエントリーの構築:
  - AV1 (`av01`): `PROP_VIDEO_CONFIG` の Sequence Header OBU を `bitstream::av1::build_av01_box_from_config_obus` に渡す。
  - H.264 (`avc1`): `PROP_VIDEO_CONFIG` の AVCDecoderConfigurationRecord から SPS / PPS / `lengthSizeMinusOne` を取り出し、`bitstream::h264::build_avc1_box` に渡す。
  - H.265 (`hvc1` / `hev1`): `PROP_VIDEO_CONFIG` の HEVCDecoderConfigurationRecord から VPS / SPS / PPS / `lengthSizeMinusOne` を取り出し、catalog の codec に応じて `bitstream::h265::build_hvc1_box` / `build_hev1_box` に渡す。
  - Opus: catalog の `samplerate` / `channelConfig` で `bitstream::opus::build_opus_box` を呼ぶ。先頭サンプルに `PROP_AUDIO_CONFIG` (OpusHead) があればその Channel Count / Input Sample Rate / Pre-skip / Output Gain を優先する。
- video トラックは「キーフレームかつ `PROP_VIDEO_CONFIG` と `PROP_TIMESTAMP` を持つ最初のサンプル」から開始する。それ以前のサンプルは録画しない。キーフレーム判定は `PROP_VIDEO_FRAME_MARKING` (RFC 9626 §3.2 の I ビット) を使い、プロパティが無い場合は `PROP_VIDEO_CONFIG` の有無で代用する。
- video のサンプルデータは length-prefixed NAL (AVCC / HVCC) 前提とし、Annex B からの変換は行わない。moq-pub が length-prefixed で送るため。
- サンプルは object の payload を読み出した直後、デコードの前に writer へ送る (録画をデコードの進捗・成否から独立させる)。
- group は並行に処理されるためサンプルの到着順は timestamp 順とは限らない。ライタースレッド側で 1 秒の reorder window を持ち、timestamp 昇順で `Mp4FileMuxer::append_sample` する。
  - 同一 timestamp のサンプルは到着順を保つ。
  - window を超えて到着したサンプル (既に確定した時刻より古いサンプル) は破棄して警告を出す。その区間の duration は直前のサンプルが吸収する。
  - 1 秒という窓幅は、video の group が既定 2 秒の GOP 単位で最大 4 stream が並行処理される (`MAX_CONCURRENT_STREAMS`) ことへの余裕として置く。
- サンプルの `duration` は同一トラックの次のサンプルの timestamp との差から求める。最後のサンプルと、差が 0 以下のサンプルは直前の duration を使い、直前の duration が無ければフォールバック (video は catalog の framerate、audio は 20 ms) を使う。
- 録画の基準時刻 (base) は次のように決める。video を購読している場合は最初の録画可能なキーフレームの Timestamp を base とし、それ以前のサンプル (audio を含む) は録画しない。video を購読していない場合は最初の audio サンプルを base とする。
- base より後に始まるトラックは、最初のサンプルの duration に (そのトラックの先頭 Timestamp - base) を加算して、2 番目以降のサンプルを base に揃える。
  - MP4 は編集リストなしでトラック先頭を 0 より後ろに置けないため、遅れて始まるトラックの先頭サンプルだけが本来より (トラック先頭 Timestamp - base) 早く提示される残差が残る (2 番目以降は正しく揃う)。
  - base を最初のキーフレームに取ることで映像の残差は 0、音声の残差は base 直後の音声サンプルまでの差 (音声が連続していれば最大 1 フレーム = 20 ms、欠落があればその欠落長) に収まる。
  - 編集リスト (elst) とフィラーサンプルの生成は行わない。
- 録画は再生の間引きと独立させる。`display_backlog` 超過で video group を破棄する経路でも object を読み出して録画し、デコードだけをスキップする。`--no-play` ではすべての group を同じ経路で録画する。
- datagram 経由の object は本 issue の対象外とする。
  - 現行の moq-sub は datagram 受信タスクで `DataPlaneHandle::recv_datagram` を呼ぶだけで、payload の取り出しもデコードも行っていない。
  - 生バイト列は `StreamHandle::recv_datagrams` が返し、`shiguredo_moqt::stream::datagram::ObjectDatagram::decode` の消費バイト数から payload を取得できるため実装は可能だが、datagram 経由メディアの処理自体を別 issue とする。
  - moq-pub の `--use-datagram` の出力は本 issue では録画できない。
- 録画失敗 (サンプルエントリー構築失敗、書き込み失敗など) はログを出して該当トラックの録画を止め、再生は継続する。

## 完了条件

- `cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --mp4 out.mp4` で、moq-pub が配信する video / audio (AV1 / Opus) を含む MP4 ファイルが出力され、ffprobe などの外部ツールで video / audio の 2 トラックと概ね正しい尺・同期が確認できる。
- `--no-play` を併せて指定した場合、SDL の初期化を行わずに同じ MP4 が出力される (GUI のない環境でも動作する)。
- Ctrl+C による graceful shutdown、relay からの切断、プレイヤーの終了 (ウィンドウを閉じた等)、`--no-video` / `--no-audio` 指定のそれぞれで、moov まで書き込まれた再生可能な MP4 が得られる。
- `display_backlog` 超過による video group 破棄が発生しても、録画ファイルの該当区間が欠落しない (デコードだけがスキップされる)。
- H.264 / H.265 (macOS) でも同じく MP4 が出力され、moq-sub が対応する全 video codec で再生できる。
- `--mp4` を指定しない場合の動作と出力が変わらない。
- 録画用モジュールの単体テストで、少なくとも次を固定する: AVCDecoderConfigurationRecord / HEVCDecoderConfigurationRecord のパース、OpusHead から `OpusSampleEntryConfig` への写像、timestamp / timescale からマイクロ秒への変換 (timescale 無しの既定を含む)、reorder 後の順序と window 外サンプルの破棄、duration と base オフセットの決定。
- `CHANGES.md` に ADD を追記し、`examples/README.md` の moq-sub オプション表に `--mp4` / `--no-play` を追記する。
- `make test` / `make clippy` / `make fmt` が通る。

## 解決方法

`examples/moq-sub` に受信したエンコード済みサンプルを MP4 へ保存する仕組みを追加した。

- `shiguredo_mp4 = "2026.5"` を依存に追加し、`examples/moq-sub/src/mp4.rs` に録画モジュールを新設する
  - `Recorder` が専用 OS スレッドを起動し、`Mp4FileMuxer` とファイル I/O を所有する。終了は Finish メッセージで確定するため、`RecorderSender` の clone が残っていてもハングしない
  - ファイルは最初のサンプルを書き出す時点で作成し、録画対象のサンプルが無い場合は作成しない (既存ファイルも変更しない)。`--mp4` を指定した場合は既存ファイルを上書きする
  - サンプルエントリーは AV1 (av1C の config OBUs)、H.264 (avcC)、H.265 (hvcC、codec に応じて hvc1 / hev1)、Opus (OpusHead または catalog 値) から構築する
  - 破棄した audio サンプルの OpusHead も保持し `dOps` に反映する
- `examples/moq-sub/src/cli.rs` に `--mp4 <PATH>` と `--no-play` を追加する
- `examples/moq-sub/src/main.rs` は `--no-play` で SDL を初期化せず、プレイヤー終了時に pipeline の終了 (録画の finalize を含む) を待つ。2 回目の Ctrl+C で強制終了する
- `examples/moq-sub/src/pipeline.rs` は subgroup stream と fetch 応答ストリームの object をデコード前に録画モジュールへ渡す。`--no-play` と表示待ち超過時はデコードだけをスキップして録画を継続し、decoder が使えない場合も録画を継続する
- タイムスタンプは LOC の Timestamp / Timescale をマイクロ秒へ変換する (Timescale を未観測のトラックは Unix エポックからのマイクロ秒として扱う)
- 実装時に設計方針から次の点を変更した
  - 到着順の並べ替えは 1 秒 window で確定する方式ではなく、「期待尺の 1.5 倍を超える後続サンプルは到着を待ち、wall-clock で 1 秒経過したら実際のギャップとして確定する」方式にした (window 1 秒では GOP 2 秒 + 並行 4 stream の到着順の入れ替わりを吸収できず、duration が膨張するため)
  - 同一 timestamp のサンプルは直前の duration (期待尺に clamp) を使う
  - 破棄は「書き出し済みサンプルが覆うメディア時刻より前の遅着」に限定した (到着順の入れ替わりで正当なサンプルを破棄しないため)
  - トラック開始時刻の差が 60 秒を超える場合は Timescale の混在などとみなし、オフセット 0 で録画する
  - 録画の失敗はサンプルエントリー構築失敗のみ該当トラックを停止し、I/O / finalize 失敗は `run` のエラーとして終了コード 1 にする (セッション動作中は再生を継続する)
- datagram 経由のメディアは対象外とした (現行の受信 API では payload を取得できないため)
- 録画モジュールの単体テストを追加し、一時ファイルへ書き出した MP4 を `Mp4FileDemuxer` で読み戻してトラック数・timestamp・duration・サンプルエントリーを検証する
- `CHANGES.md` と `examples/README.md` を更新する

relay と macOS 環境を使った実機確認 (ffprobe による 2 トラックの尺・同期、H.264 / H.265 の実出力、Ctrl+C / relay 切断 / ウィンドウ終了の各経路、`display_backlog` 超過中の録画) は環境が無いため未実施である。
