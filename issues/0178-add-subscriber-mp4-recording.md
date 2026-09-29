# moq-sub に受信メディアの MP4 保存機能を追加する

- Created: 2026-09-29
- Completed: {YYYY-MM-DD}
- Branch: feature/add-subscriber-mp4-recording
- Polished: {YYYY-MM-DD}

## 目的

`moq-sub` は受信した映像・音声をデコードして再生するだけで、保存する手段がない。配信されたメディアを MP4 ファイルとして保存できるようにし、録画した内容を後から再生・確認できるようにする。また、再生用の SDL ウィンドウを開けない環境 (GUI のないサーバー等) でも受信と保存を行えるようにする。

## 現状

- `examples/moq-sub/src/main.rs` の `main` はデコード済みフレームを `std::sync::mpsc` でメインスレッドへ渡し、`run_raw_player` が `raw_player` (SDL) で再生する。`raw_player::init()` を必ず呼ぶため、映像ウィンドウを開けない環境では動作しない。
- `examples/moq-sub/src/pipeline.rs` の `run` は `receive_catalog` で catalog を取得して video / audio トラックを SUBSCRIBE し、`handle_incoming_stream` → `decode_video_stream` / `decode_audio_stream` でデコードする。受信したエンコード済みペイロードはデコード後すぐに破棄され、ファイルには残らない。
- `decode_and_send` が送出したフレーム数に応じて `display_backlog` を増やし、`MAX_DISPLAY_BACKLOG` を超えた video group はデコード前に破棄する (`handle_stream_body` の `StreamType::Subgroup` / `StreamType::Fetch` の分岐)。
- LOC の `PROP_TIMESTAMP` / `PROP_TIMESCALE` は `extract_timestamp_timescale` で取り出し済みで、音声の PTS 計算 (`audio_pts_us`) に使っている。映像は PTS を使っていない。
- `PROP_VIDEO_CONFIG` / `PROP_AUDIO_CONFIG` は `extract_video_config` / `extract_audio_config` で取り出し済みで、映像デコーダの初期化と音声設定の検証に使っている。
- datagram 経由で受信した object の payload は、`DataPlaneHandle::recv_datagram` が `DatagramAcceptance` しか返さず、`ObjectDatagram` が payload を保持しないため、アプリケーションから取得できない。
- `examples/moq-sub/Cargo.toml` に MP4 関連の依存はない。

## 設計方針

- 受信したエンコード済みサンプル (AV1 / H.264 / H.265 / Opus) を再エンコードせずに MP4 へ mux する。録画対象は SUBSCRIBE したトラックに合わせる。
- `shiguredo_mp4` を `examples/moq-sub` の依存に追加する (`2026.5`)。`Mp4FileMuxer` (MP4 ファイル mux) と `bitstream` 配下のサンプルエントリー構築 API を使う。
- 専用の OS スレッド (`std::thread`) を立て、そのスレッドがファイル I/O と `Mp4FileMuxer` を所有する。pipeline 側は `std::sync::mpsc::Sender` でサンプルを送るだけにして、tokio ランタイムで blocking I/O を行わない。
- CLI に `--mp4 <PATH>` を追加する。指定時にのみ録画し、既存ファイルは truncate して上書きする。
- CLI に `--no-play` を追加する。指定時は `raw_player` を初期化せず、デコードも行わない (録画のみ)。`--mp4` を併せて指定しない場合は受信のみで何も保存しない旨を警告する。
- タイムスタンプは LOC の `PROP_TIMESTAMP` / `PROP_TIMESCALE` をマイクロ秒へ変換し、トラックのタイムスケールを 1_000_000 に固定して扱う。video の非キーフレームは `PROP_TIMESCALE` を持たないため、直前に観測した timescale を使う。
- サンプルエントリーの構築:
  - AV1 (`av01`): `PROP_VIDEO_CONFIG` の Sequence Header OBU を `bitstream::av1::build_av01_box_from_config_obus` に渡す。
  - H.264 (`avc1`): `PROP_VIDEO_CONFIG` の AVCDecoderConfigurationRecord から SPS / PPS / `lengthSizeMinusOne` を取り出し、`bitstream::h264::build_avc1_box` に渡す。
  - H.265 (`hvc1` / `hev1`): `PROP_VIDEO_CONFIG` の HEVCDecoderConfigurationRecord から VPS / SPS / PPS / `lengthSizeMinusOne` を取り出し、catalog の codec に応じて `bitstream::h265::build_hvc1_box` / `build_hev1_box` に渡す。
  - Opus: catalog の `samplerate` / `channelConfig` で `bitstream::opus::build_opus_box` を呼ぶ。先頭サンプルに `PROP_AUDIO_CONFIG` (OpusHead) があればその Channel Count / Input Sample Rate / Pre-skip / Output Gain を優先する。
- video トラックは「キーフレームかつ `PROP_VIDEO_CONFIG` と Timestamp / Timescale を持つ最初のサンプル」から開始する。それ以前のサンプルは録画しない。キーフレーム判定は `PROP_VIDEO_FRAME_MARKING` (RFC 9626 §3.2 の I ビット) を使い、プロパティが無い場合は `PROP_VIDEO_CONFIG` の有無で代用する。
- video のサンプルデータは length-prefixed NAL (AVCC / HVCC) 前提とし、Annex B からの変換は行わない。moq-pub が length-prefixed で送るため。
- group は並行に処理されるためサンプルの到着順は timestamp 順とは限らない。ライタースレッド側で 1 秒の reorder window を持ち、timestamp 昇順で `Mp4FileMuxer::append_sample` する。
- サンプルの `duration` は同一トラックの次のサンプルの timestamp との差から求める。最後のサンプルは直前の duration を使い、フォールバックは video が catalog の framerate、audio が 20 ms とする。
- トラック間の開始時刻差は、各トラックの最初のサンプルの duration に「録画開始基準時刻との差」を加算して合わせる。
- 録画は再生の間引きと独立させる。`display_backlog` 超過で video group を破棄する経路でも object を読み出して録画し、デコードだけをスキップする。`--no-play` ではすべての group を同じ経路で録画する。
- 録画対象のサンプルが 1 件も無い場合は MP4 ファイルを作らない。
- datagram 経由の object は録画対象外とする (前述のとおり payload を取得できない)。moq-pub の `--use-datagram` の出力は録画できない。
- 録画失敗 (サンプルエントリー構築失敗、書き込み失敗など) はログを出して該当トラックの録画を止め、再生は継続する。

## 完了条件

- `cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --mp4 out.mp4` で、moq-pub が配信する video / audio (AV1 / Opus) を含む MP4 ファイルが出力され、ffprobe などの外部ツールで video / audio の 2 トラックと概ね正しい尺・同期が確認できる。
- `--no-play` を併せて指定した場合、SDL の初期化を行わずに同じ MP4 が出力される (GUI のない環境でも動作する)。
- Ctrl+C による graceful shutdown、relay からの切断、`--no-video` / `--no-audio` 指定のそれぞれで、moov まで書き込まれた再生可能な MP4 が得られる。
- `display_backlog` 超過による video group 破棄が発生しても、録画ファイルの該当区間が欠落しない (デコードだけがスキップされる)。
- H.264 / H.265 (macOS) でも同じく MP4 が出力され、moq-sub が対応する全 video codec で再生できる。
- `--mp4` を指定しない場合の動作と出力が変わらない。
- 録画用モジュールの単体テストで、少なくとも次を固定する: AVCDecoderConfigurationRecord / HEVCDecoderConfigurationRecord のパース、OpusHead から `OpusSampleEntryConfig` への写像、timestamp / timescale からマイクロ秒への変換、reorder 後の順序、duration の決定。
- `CHANGES.md` に ADD を追記し、`examples/README.md` の moq-sub オプション表に `--mp4` / `--no-play` を追記する。
- `make test` / `make clippy` / `make fmt` が通る。

## 解決方法

{対応後に追記する}
