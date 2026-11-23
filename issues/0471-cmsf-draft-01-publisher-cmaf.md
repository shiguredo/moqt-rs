# moq-pub で cmaf packaging の配信に対応する

- Created: 2026-11-23
- Completed: {YYYY-MM-DD}
- Branch: feature/add-cmsf-draft-01
- Polished: 2026-11-23
- Refreshed: 2026-11-23

## 目的

0467 でライブラリのカタログが cmaf packaging (CMSF-01) を扱えるようになる。本 issue では、moq-pub が CMAF で media を配信できるようにする。0472 の moq-sub と組み合わせれば、CMSF の送受信を example で確かめられる。

前提は 0467 である。CMSF 対応全体の方針は 0467 を参照すること。壁時計の同期 (prft) は 0473 で扱う。それまでは、moq-sub は cmaf の Object を `TimestampKind::MediaTime` として、届いた順に表示する。

## 現状

### moq-pub

- `crates/moq-pub/src/catalog.rs` の `build_catalog` は、映像 / 音声とも `MsfPackaging::Loc` の track を作る。`alt_group` は設定しない。
  - `VideoTrackParams::video_config_in_objects` / `AudioTrackParams::audio_config_in_objects` が真の track には、type `object-property` の initDataList entry (`video-config` / `audio-config`、data は `0x0D` / `0x0F`) を指す initRef を載せる (MSF-02 §12.2)。
  - 2 つのフラグは、`pipeline.rs` の `prepare_catalog_tracks` が packaging によらずに決める。音声は常に真、`--input-mp4` の映像は常に真、エンコーダの映像は `VideoEncoder::carries_video_config` の値 (AV1 / H.264 / H.265 は真、VP8 / VP9 は偽) である。値は `CatalogVideoTrack` / `CatalogAudioTrack` を通って `build_catalog` に渡る。
  - `MsfTrack::init_ref` は track ごとに 1 つである。
- `crates/moq-pub/src/pipeline.rs` の `run` は次の順で進む。
  1. 接続し、映像 / 音声 / catalog の PUBLISH を張る
  2. エンコーダを準備する
  3. `prepare_catalog` でカタログを送る
  4. 映像 / 音声の入力を起動する
  5. `run_publisher_loop` に入る
- 入力タスクが渡す映像の値は次の 3 種類である。音声は `AudioInput::Raw` / `AudioInput::Reencode` である。
  - `VideoInput::Raw`: キャプチャしたエンコード前のフレーム (live capture)
  - `VideoInput::Encoded`: エンコード済みのサンプル (`--input-mp4` のパススルー)
  - `VideoInput::Reencode`: 入力ファイルを復号したフレームと、そのタイムスタンプ (`--input-mp4-reencode`)
- エンコードと送信は、`run_publisher_loop` の中の `handle_video_input` / `handle_audio_input` で行う。そのための状態 (`gop_tracker` / `video_clock` / `capture_residuals` / writer など) は `PublisherLoopState` が持つ。`PublisherSetup::new` がループに入る直前に、`PreparedCatalog` と一緒に組み立てる。
- `PreparedCatalog` は `handle` / `group_id` / `location` / `context` (`CatalogContext`) / `json` を持つ。
  - `handle` はカタログ専用ではなく、session の stream handle である。
  - `PublisherSetup::pipeline_state` がこれをループに渡し、映像 / 音声の Object の送信 (`publish_sink`) にも使う。
- `prepare_catalog` の動作は次のとおりである。
  - `config` / `&mut ConnectedPublisher` / `CatalogTracks` を受け取る。`CatalogTracks` は `prepare_catalog_tracks` が作る。
  - `group_id::allocate_initial_group_id` で Group ID を払い出し、`CatalogContext` を組み立てる。
  - `catalog::send_catalog` で main session の handle に直接送る。`send_catalog` は Skip をエラーにし、`prepare_catalog` は `?` でエラーを返すので、初回の送出が Pass しないと `run` が終わる。
  - `prepare_catalog_tracks` が借りる入力は、その後 `start_input_tasks` に渡されて消費される。
- `CatalogContext` は、namespace / track 名 / 同期用メタデータ / 映像・音声の値を持つ。init data は持たない。
- FETCH / SUBSCRIBE への応答は、`run_publisher_loop` の中で行う。
  - 経路は `handle_notable_event` → `handle_publisher_request_event` → `serve_peer_request` である。
  - Object のない track への FETCH は、session 層 (`src/session/fetch.rs`) が INVALID_RANGE (`track has no published objects`) で拒否する。これは publisher 役の購読があり、Largest Object をまだ観測していない場合に限る。購読がないときは受理する。
  - session 層が受理した catalog の FETCH への応答は、`catalog::catalog_fetch_response(filter, catalog: Location)` が決める。引数の Location は publish 済みカタログの Location で、必須である。`serve_peer_request` に渡す `CatalogState` も `location: Location` を持つ。
- `republish_catalog_at_next_group` を呼んでカタログを送り直す経路は 5 つある。
  - `handle_catalog_republish`: 定期再送
  - `apply_republished_request`: 同じ URL への request 単位の GOAWAY。`republish_request_on_current_session` が PUBLISH を張り直した後に送り直す
  - `handle_publisher_request_event`: catalog の SUBSCRIBE を受理した直後
  - `republish_catalog_on_migrated_session`: session 単位の移行後
  - `handle_migrated_catalog`: 別の URL への request 単位の移行後。PUBLISH_DONE、`catalog_request_id` と `catalog_publish_target` の差し替えを行った後に送り直す
- `republish_catalog_at_next_group` の動作は次のとおりである。
  - 送り先は `catalog_sink` で決める。
  - Group ID は送信の成否によらず進める。
  - location は Pass のときだけ更新する。
- `republish_catalog_at_group` は、指定した Group に送り、`Result<ObjectFilterOutcome>` を返すだけである。location は更新しない。
- session 単位の移行では、`replace_with_migrated_session` が旧 client を手放す。`cat_tokens_changed` が真のときだけ、`CatalogContext::json` でカタログを作り直す。
- Object payload は LOC である。`build_video_loc_properties` などが LOC property を組み立てる。
  - live capture では、LOC の Timestamp を Timescale なしで送る。値は Unix epoch からのマイクロ秒である。
  - `--input-mp4` (映像のみ) と `--input-mp4-reencode` (映像と音声) では、入力ファイル由来のメディア時刻を送る。Timescale には入力 track の timescale を使う (`video_timescale` / `audio_timescale` など)。
- 映像は 1 GOP を 1 Group にする。音声は 1 フレームを 1 Object・1 Group にする (`deliver_audio_chunk` が Object ごとに `group_id::next_group_id` で進める)。
- `republish_catalog_at_group` は、PUBLISH 購読への送出結果 (`ObjectFilterOutcome`) だけを返す。受信 SUBSCRIBE の購読者への送出結果は、失敗をログにするだけで返さない。
  - 既存の送り直し (`republish_catalog_at_next_group`) は、この戻り値が Skip のとき location を前回のままにする。
  - 購読が PAUSED のときは、session 層 (`src/session/subscription/validation.rs` の `object_passes_filters`) が Object を落とし、tokio-moq がそれを `ObjectFilterOutcome::Skip` にする。
  - session 層は、Object が購読の filter を通った時点で、Track の Largest Object を記録する。記録の関数は `src/session/subscription/delivery.rs` の `record_published_track_largest` で、`src/session/data.rs` の送出処理から呼ばれる。記録は PUBLISH 由来か SUBSCRIBE 由来かを問わない。そのため、PUBLISH 購読で Skip でも受信 SUBSCRIBE 側に届いていれば、session 層は Largest を観測済みになる。
- media の Object は、経路ごとに組み立てて送る。
  - 映像: `send_encoded_video_frames` が PUBLISH の writer に送り、Pass のとき `media_cache` (FETCH 用) に入れる。受信 SUBSCRIBE の購読者には、`send_video_to_peer_subscriptions` が LOC property を組み立て直して送る (`media_cache` には入れない)。
  - 音声: `deliver_audio_chunk` が `send_audio_datagram` / `send_audio_stream` で PUBLISH に送り、`media_cache` に入れる。受信 SUBSCRIBE の購読者には、`send_audio_to_peer_subscriptions` が購読ごとに組み立て直して送る (`media_cache` には入れない)。終了時のフェードは、`flush_audio_fade` が PUBLISH と `media_cache` にだけ送る。
- live capture の音声の Timestamp は単調に増えるとは限らない。`src/audio_clock.rs` の `AudioTimestampClock` は、補正を「下がる方向は待たずに合わせる」ので、補正が 1 フレーム分を超えて下がると、次の chunk の Timestamp が直前より小さくなる。
  - 映像も、送出する Timestamp は単調に増えるとは限らない。`src/media_clock.rs` の `WallClockMapper` が単調増加を保証するのは、入力の時刻の換算までである。エンコーダが 1 つの入力から複数の出力を返すと、`crates/moq-pub/src/encoder.rs` の `output_timestamp_us` が `入力の時刻 + 出力の番号 × フレーム間隔` を付ける。この時刻が、次の入力の時刻を超えることがある。再エンコードの `assign_input_timestamps` も同様である。
- 定期再送の `handle_catalog_republish` は、送出エラーを次のように分類する。
  - `is_transport_session_end`: session の終了として扱う
  - `is_peer_stream_reset`: 送出を継続する
  - それ以外: エラーにする
- 音声は OBJECT_DATAGRAM 配送が既定で、`--audio-subgroup` で subgroup 配送に切り替える (`crates/moq-pub/src/cli.rs`)。
  - subgroup 配送では、明示された `--datagram-max-size` / `--audio-redundancy` を警告して無視する。
  - `--stream-mapping` には既定値 (`group`) がある。現状は明示されたかどうかを判定していないが、判定には他のオプションで使っている `is_explicit` が使える。
- `crates/moq-pub/src/mp4.rs` の `Mp4VideoReader::open` は、最初の sample entry から codec 文字列と Video Config を取り出した後、sample entry 自体は保持しない。B フレームを含む MP4 は拒否する。
- H.264 / H.265 のエンコーダは `allow_frame_reordering: Some(false)` で動かす。LOC 経路は、1 frame に 1 つの Timestamp だけを送る。

### shiguredo_mp4 (2026.6.0-canary.2)

- `Fmp4SegmentMuxer` の tfdt (`base_media_decode_time`) は、track ごとに 0 から始まり、`Sample.duration` の累積で進む。起点や各セグメントの tfdt を外から指定する API はない。track_ID は `tracks.len() + 1` で採番する。
- `Fmp4SegmentMuxer::init_segment_bytes` は、`create_media_segment_metadata` に渡された sample の sample entry から init segment を作る。sample を 1 つも渡していなければ `EmptyTracks` を返す。
  - init segment の ftyp は、major brand が `iso5`、compatible brands が `iso5` / `iso6` / `mp41` (と codec 由来の brand) である。
  - trex の既定値は、duration / size / flags がいずれも 0 である。
- `MoofBox` / `MfhdBox` / `TrafBox` / `TfhdBox` / `TfdtBox` / `TrunBox` / `MdatBox` と `SampleFlags::from_fields` は公開されている。
- sample entry の構築関数として、`build_av01_box` / `build_avc1_box` / `build_hev1_box` / `build_hvc1_box` / `build_vp08_box` / `build_vp09_box` / `build_opus_box` / `build_mp4a_box` がある。`build_vp09_box` は、keyframe または intra-only frame の header を要求する。
- `Fmp4SegmentDemuxer` は、size / flags を trun → tfhd → trex の順に探し、`!flags.sample_is_non_sync_sample()` を keyframe とする。

## CMSF-01 と MSF-02 / MoQT-23 のずれ

| 項目 | CMSF-01 | MSF-02 / MoQT-23 | 本実装の扱い |
| --- | --- | --- | --- |
| Object から Stream への割り当て | 定めない | §6: packaging が定めない限り Subgroup 0 に置く (SHOULD)。`streamMapping` は `loc` 専用 | cmaf の Object はすべて Subgroup 0 に置く。datagram は使わない。`packagingMetadata` は載せない |
| Group 内の keyframe 数 | §3.4: Group は 1 つ以上の独立符号化列を含む。§3.6.2 の例は Group の途中に SAP type 3 を置く | §4.3: keyframe 間隔の異なる track を同じ altGroup に置く場合、Group に複数の keyframe を置いてはならない (MUST NOT) | 1 GOP = 1 Group とし、keyframe は先頭の 1 つだけにして両方を満たす。食い違いは `docs/msf.md` の CMSF の節に記載する |
| moof 内の Box 名 | §3.3: moof が「Track Box (trak) with a Track ID (track_ID)」を含む | (ISO BMFF では moof が持つのは traf / tfhd の track_ID) | traf / tfhd の track_ID を init segment の trak と一致させる、と読み替える |

## 設計方針

### CLI と Object の形

- CLI に `--packaging <loc|cmaf>` を追加する。既定は `loc` である。cmaf は、moq-pub が扱う全 codec (AV1 / H.264 / H.265 / VP8 / VP9 / Opus / AAC) を対象にする。
- cmaf の Object は 1 sample 分の CMAF chunk とし、LOC と同じく 1 sample を 1 Object にする。
  - 映像は 1 GOP を 1 Group とし、Group 先頭の Object を keyframe にする (CMSF §3.4)。Group の境界と CMAF Fragment の境界は一致する。
  - 音声は現状どおり 1 フレームを 1 Object・1 Group にし、すべての sample を sync sample にする。
  - 映像と音声は別の CMAF track にする (CMSF §3.3 の single track)。
  - cmaf の Object には LOC property を付けない。
- Object はすべて Subgroup 0 に置く (MSF-02 §6)。
  - cmaf では、既存の警告判定より前に音声の datagram 配送を無効にする。`--audio-subgroup` の指定がなくても、音声は subgroup で送る。明示された `--datagram-max-size` / `--audio-redundancy` は、subgroup 配送のときと同じく警告して無視する。
  - `--stream-mapping` が明示されていれば (`is_explicit` で判定する)、エラーにする。

### tfdt と chunk の組み立て

- tfdt と timescale には、LOC 経路で Timestamp / Timescale として送っている値をそのまま使う。
  - live capture では、timescale を 1,000,000 とし、tfdt を capture の timestamp (Unix epoch からのマイクロ秒) とする。映像と音声が同じ時計に載るので、先頭 sample の capture 時刻の差がそのまま tfdt の差になる。
  - `--input-mp4` / `--input-mp4-reencode` では、timescale を LOC 経路の Timescale と同じ入力 track の timescale とし、tfdt を入力ファイル由来のメディア時刻とする。映像と音声は同じファイルの時間軸に載る。
  - track ごとに tfdt を逆行させない。tfdt は sample の decode time であり、chunk の並びで減ってはならない。
    - 映像・音声の両方の track で、Timestamp が直前の chunk の tfdt 以下になる場合だけ、tfdt を「直前の tfdt + 1」(timescale 単位) にし、英語の debug ログを出す。この処理は、track ごとの chunk の組み立てに置く。
    - 下限を「直前の tfdt + 名目の duration」にはしない。映像の `WallClockMapper` が保証するのは、入力の時刻の刻みがメディア時刻の差の半分より大きいことまでである。名目の duration を下限にすると、収束中や丸めの差で、逆行していない普段の chunk にも揃えが入るためである。
    - それ以外は LOC の Timestamp と同じ値を使う。
- `Fmp4SegmentMuxer` が作る media segment は、この決め方に使えない。tfdt が track ごとに 0 から始まり、duration の累積で進むため、次の 2 つが起きる。
  - 映像と音声の先頭 sample の時刻の差が失われ、開始時点で同期がずれる。
  - 名目の duration を累積すると、フレーム落ちのたびに capture 時刻とのずれが積み上がる。
- そのため moq-pub は `Fmp4SegmentMuxer` を init segment の生成にだけ使い、各 chunk は `shiguredo_mp4::boxes` の box で組み立てる。
- init segment は track ごとに `Fmp4SegmentMuxer` を作って生成する。
  - 最初の sample を `create_media_segment_metadata` に 1 回渡して sample entry と timescale を学習させ、`init_segment_bytes` で init segment を得る。このとき返る media segment は使わない。
  - 映像の最初の sample は keyframe である。VP9 の sample entry は keyframe の header から作る必要があるため、この順序が必要になる。
- sample entry の出どころは次のとおりである。
  - `--input-mp4`: 元の MP4 の sample entry を使う。`Mp4VideoReader::open` が読んだ最初の sample entry を保持してループへ渡す。sample entry が 2 種類以上ある MP4 は、cmaf では `open` の時点で拒否する。
  - live capture と `--input-mp4-reencode`: エンコーダ出力から作る。
- moof の構成は次のとおりである。mfhd と traf (tfhd / tfdt / trun) で moof を組み、続けて mdat を置く。
  - mfhd: sequence_number は track ごとに 1 から増やす。
  - tfhd: track_ID には init segment の trak の track_ID (1) を使い、`default_base_is_moof` を立てる。ftyp の brand は `iso5` 系なので、このフラグを使える。
  - tfdt: 毎 chunk に書き、version 1 (64 bit) にする。Unix epoch マイクロ秒は u32 に収まらないためである。
  - trun: sample ごとに次を書く。trex の既定値は duration / size / flags が 0 で、tfhd にも既定値を置かない。そのため trun に書かないと、demux 側で size 0 になり、すべての sample が keyframe と判定される。
    - duration: 名目値とする。映像は fps から、音声は codec のフレーム長から求める。tfdt を毎 chunk 書くので、誤差は累積しない
    - size
    - flags: `SampleFlags::from_fields` で組む。keyframe は sync sample、それ以外は non-sync sample とする
  - trun の data_offset: trun に 1 つ書く値で、moof の先頭から mdat の payload までの距離とする
  - composition offset は書かない。LOC 経路と同じく、エンコーダ出力の timestamp を表示順 = 復号順として扱う前提である。H.264 / H.265 は `allow_frame_reordering: Some(false)` で、パススルーは B フレームを含む MP4 を拒否する。
- sample entry の作り方 (live capture と `--input-mp4-reencode`)。値の出どころは codec ごとに次のとおりとする。
  - H.264 / H.265:
    - 使う値は keyframe の `EncodedFrame::video_config` (avcC / hvcC のレコード本体) である。
    - このレコード本体には box header がない。そのため、先頭に 8 バイトの box header (size と `avcC` / `hvcC`) を付けてから `AvccBox` / `HvccBox` として decode する。`AvccBox::decode` は box header を要求するためである。
    - H.264: decode した `AvccBox` の SPS / PPS のリストを `build_avc1_box` に渡す。`H264SampleEntryConfig` の length_size は、`length_size_minus_one` を `LengthSize::from_length_size_minus_one` で変換して入れる。
    - H.265: `HvccBox` は VPS / SPS / PPS を `nalu_arrays` にまとめて持つので、nal_unit_type 32 / 33 / 34 の配列から取り出して `build_hvc1_box` に渡す。
      - `H265SampleEntryConfig` は、length_size を同じく変換して入れ、`avg_frame_rate` を `AVG_FRAME_RATE_UNSPECIFIED`、`constant_frame_rate` を `H265ConstantFrameRate::Unknown` とする。moq-sub の録画 (`build_video_entry`) と同じ値である。
  - AV1: keyframe の Video Config (Sequence Header OBU) を `build_av01_box_from_config_obus` に渡す。moq-sub の録画に前例がある。
  - VP8:
    - keyframe の frame header を `bitstream::vp8::parse_frame_header` で解析し、`Vp8SampleEntryConfig` の width / height に、得た幅と高さを入れる。
    - 色特性はビットストリームから得られない。そのため `colour_primaries` / `transfer_characteristics` / `matrix_coefficients` は 2 (unspecified、ISO/IEC 23001-8) とし、`video_full_range_flag` は false とする (実装判断)。
  - VP9:
    - keyframe の frame header を `bitstream::vp9::parse_frame_header` で解析して `build_vp09_box` に渡す。
    - `Vp9SampleEntryConfig` の width / height には、header の幅と高さを入れる。`level` は `LEVEL_UNDEFINED` (0) とし、色特性は VP8 と同じく 2 (unspecified) とする。
  - Opus: OpusHead を組むときと同じ値を `OpusSampleEntryConfig` に入れる。moq-pub の `encoder/opus.rs` の `build_opus_head` に合わせ、チャンネル数と入力サンプルレートはエンコーダ設定の値、pre_skip と output_gain は 0 とする。OpusHead のバイト列を解析する必要はない。
  - AAC:
    - エンコーダの AudioSpecificConfig を `bitstream::aac::parse_audio_specific_config` で解析し、`build_mp4a_box` に渡す。
    - `Mp4aSampleEntryConfig` は、`es_id` を `EsDescriptor::MIN_ES_ID`、`buffer_size_db` を 0、`max_bitrate` / `avg_bitrate` を `config.audio_bitrate` (CLI の値で単位は kbps) を 1000 倍した bps とする (実装判断)。既存の `encoder/aac.rs` や `catalog.rs` も同じく 1000 倍して bps に換算している。
- 配信中に sample entry が最初のものから変わった場合は、英語のエラーログを出して終了する。init segment を差し替えると、独立カタログの再送と購読側の再初期化が必要になり、本 issue の範囲を超えるためである。
  - 検出は、live capture と `--input-mp4-reencode` の映像の keyframe ごとに行う。その keyframe から上の方法で組み直した sample entry を、最初のものと比べる (`SampleEntry` は `PartialEq` を実装している)。
  - `--input-mp4` のパススルーでは検出しない。元の MP4 の sample entry を使い、`Mp4VideoReader::open` が sample entry を 1 種類に限るためである。組み直した entry は元の entry と一致しない (unknown box や compressorname の違い、`hev1` が `hvc1` になる) ので、比べると誤って終了してしまう。
  - 音声は、エンコーダの設定が配信中に変わらないので比べない。
  - 比べる処理は I/O を持たない関数に切り出し、単体テストで固定する。
- chunk のバイト列は、1 sample につき 1 回だけ組み立てる。mfhd の sequence_number も 1 回だけ消費する。
  - 組み立てたバイト列を、同じ Object を送るすべての経路で同じものとして使う。経路は、PUBLISH の writer、受信 SUBSCRIBE の購読者 (`send_video_to_peer_subscriptions` / `send_audio_to_peer_subscriptions`)、`media_cache` (FETCH) である。
  - 終了時のフェード (`flush_audio_fade`) は、まだ送っていないチャンクを次の Group で送るもので、既に送った Object の送り直しではない。フェードの chunk も、同じ音声 track の組み立ての状態で組む。具体的には、mfhd の sequence_number を続きから使い、逆行防止に直前の tfdt を使う。組んだバイト列は PUBLISH と `media_cache` で同じものを使う。別の組み立て状態で組むと、終了時に sequence_number や tfdt が戻り得る。
  - 経路ごとに組み立てると sequence_number が食い違い、同じ Location の Object の payload が変わる。MoQT-23 §7.1 は「An endpoint that receives a duplicate Object with a different Delivery Mode, Subgroup ID, Priority or Payload MUST treat the track as Malformed.」と定める。

### カタログの確定

init segment を作るには sample entry が要る。H.264 / H.265 / AV1 / VP9 などは、エンコーダの出力を見るまで sample entry を作れない。そのため cmaf では、配信する track の最初の sample がループに届くのを一律に待つ。現状ではエンコードが `run_publisher_loop` の中にあるため、cmaf ではカタログ未確定のままループに入る。loc では現状の順序 (ループに入る前に `prepare_catalog`) を変えない。

- 状態の持ち方:
  - `PreparedCatalog` から `handle` を外に出す。`handle` は media の送信にも使う session の handle なので、カタログの確定前からループが持つ。
  - カタログの中身は次の 2 段の `Option` で持つ。
    - `json` と `group_id` を 1 つの `Option` にまとめる。`None` がカタログ未確定を表す。
    - `location` は別の `Option` とする。cmaf では、カタログの Object がいずれかの購読 (PUBLISH 購読または受信 SUBSCRIBE の購読者) に届いた時点で `Some` にする。session 層が Largest Object を記録する条件と揃えるためである。loc は従来どおり、PUBLISH 購読が Pass のときだけ更新する (下の「location の更新規則」)。Group ID は、既存の `republish_catalog_at_next_group` と同じく送信の成否によらず進める。
    - 「届いた」は、購読の filter を通ったこと (`SubgroupWriter::write_object` の filter 判定が Pass) とする。session 層が Largest を記録するのも filter を通った時点 (`src/session/data.rs` の送出処理) なので、条件が一致する。filter を通った後のワイヤへの送信や `finish` が失敗しても、届いた扱いにする。
  - 送り直しの経路は、`json` の有無 (確定済みかどうか) で動かすかを決める。FETCH と fill fetch に応答できるかどうかは、`location` の有無で決める。
  - loc でも同じ状態の型を使う。loc は、ループに入る前の `prepare_catalog` の結果から、確定済みで `location` がある状態で始める。
  - `CatalogContext` の組み立てを `prepare_catalog` から切り出す。現状の `prepare_catalog` と同じく、`start_input_tasks` より前に `prepare_catalog_tracks` の結果から作り、ループまで持ち越す。init segment の欄は空にしておく。
- media の扱い:
  - media の Object は、カタログ確定前から通常どおり送る。最初の sample も保持せず、そのまま送る。chunk の組み立てに init segment は要らない。track_ID は、track ごとの muxer が `tracks.len() + 1` で採番するので 1 であり、timescale は前述のとおり事前に決まっている。
  - media track への要求は、確定前も通常どおり処理する。
- 確定の契機: 配信する全 track の最初の sample がループに届いた時点とする。
  - 配信する track は、映像と音声のうち有効なもの (`config` の映像・音声の有効フラグ) である。`--no-video` / `--no-audio` で外した track、`--input-mp4` の音声 (映像のみを配信する)、再エンコードで対応する音声がない場合の音声は待たない。
  - `handle_video_input` / `handle_audio_input` は、各 track の最初の sample の sample entry (と timescale) を、ループの状態に記録するだけにする。確定の処理そのものは行わない。
- 確定を実行する場所: `run_publisher_loop` の中で、`handle_video_input` / `handle_audio_input` から戻った直後に、確定の契機を満たしたかを確かめて、専用の関数で確定する。
  - media の送信の結果処理 (`handle_video_input_outcome` など) の中では確定しない。
  - 理由は、media の結果処理が `is_peer_stream_reset` に media 固有の後始末 (現在の Group の writer を捨てて Group ID を進める) を結び付けているからである。カタログの送出エラーがそこへ流れると、カタログの stream の reset が映像の reset として扱われる。
  - 確定の関数は、カタログの送出に必要な状態をループの状態から受け取る。
    - 送出に使うもの: `client`、`handle`、`data_plane`、`catalog_request_id`、`catalog_publish_target`、`peer_subscriptions`
    - 組み立てに使うもの: `CatalogContext`、記録した sample entry、`config.url.cat_tokens`
    - 結果を書き込むもの: カタログの状態 (下の型)、`session_ended`
    - `client` / `handle` / `data_plane` は `replace_with_migrated_session` で差し替わる値なので、確定時点の値を参照する。
- 確定の手順: 確定の契機で、次の順に処理する。
  1. init segment を作り、`CatalogContext` に入れる。
  2. その時点の `cat_tokens` で `CatalogContext::json` を組み立てる。
  3. 現状の `prepare_catalog` と同じく、`group_id::allocate_initial_group_id` で Group ID を払い出す。`json` と `group_id` を `Some` にする。ここでカタログは確定する。
     - 手順 1〜3 のいずれかが失敗した場合 (init segment の生成、`CatalogContext::json`、Group ID の払い出し) は、英語のエラーで終了する。カタログを組めないまま続けても、購読者は起動できないためである。Skip を終了扱いにしない次の判断とは別で、こちらは組み立ての失敗である。
  4. `catalog_sink` → `republish_catalog_at_group` の経路で、その Group にカタログを送る。この経路を使うのは、確定前に catalog の PUBLISH が request 単位で移行していても、現在の送り先に送るためである。
     - 送出の結果は、`Result` ではなく次の 3 つを持つ結果型 (例: `CatalogPublishResult`) で返す。`Result` にすると、`Err` のときに filter の結果を呼び出し元へ渡せないためである。
       - PUBLISH 購読の filter の結果 (filter の判定まで進まなかった場合は「なし」)
       - PUBLISH 側の送出エラー (なければ「なし」)
       - 受信 SUBSCRIBE の購読者のどれかに届いたか
     - 「届いた」を filter の結果で判定するために、filter の結果を送出エラーと分けて取り出せるようにする。
       - 現状の `SubgroupWriter::write_object` (`crates/moq-pub/src/stream_writer.rs`) は、filter を通った後の SUBGROUP_HEADER や payload の送信が失敗すると、`Err` だけを返して Pass を捨てる。
       - そこで、filter の結果と送出の結果を両方返すメソッドを `SubgroupWriter` に追加する。既存の `write_object` はそのメソッドを使って現状と同じ戻り値を返し、media 経路などの他の呼び出し元の挙動は変えない。
       - `catalog::publish_catalog_object` は追加したメソッドを使い、`finish` の失敗も含めて、filter の結果と送出エラーを分けて返す。loc の初回送出の `catalog::send_catalog` もこの関数を使うので、戻り値の変更に追従させる (Skip をエラーにする挙動は変えない)。
     - `republish_catalog_at_group` と `republish_catalog_at_next_group` は、上の結果型を返すように変える。
       - PUBLISH 側の送出エラーがあれば、現状どおり受信 SUBSCRIBE の購読者への送出を行わずに返す (`.await?` と同じ順序)。
       - 受信 SUBSCRIBE 側の送出エラーは、現状どおりログにして続ける。
       - 関数の中では location を更新しない (`republish_catalog_at_next_group` の引数 `catalog_location: &mut Location` をやめる)。
       - 呼び出し元 (送り直しの 5 経路と確定の関数) は、まず結果型を下の型の「`location` の更新」に渡し、その後で PUBLISH 側の送出エラーを扱う。
       - エラーの扱いは経路ごとに現状のまま変えない。変えるのは、location の更新との順序だけである。
         - 3 分類 (`is_transport_session_end` / `is_peer_stream_reset` / それ以外) で扱うのは、`handle_catalog_republish` と確定の関数だけである。
         - 他の 4 経路 (`apply_republished_request` / `handle_publisher_request_event` / `republish_catalog_on_migrated_session` / `handle_migrated_catalog`) は、`location` を更新した後、現状どおり英語の warn ログを出して続ける。
     - location の更新規則は packaging で分ける。
       - cmaf: PUBLISH 購読の filter の結果が Pass か、受信 SUBSCRIBE の購読者のどれかに届いた場合に更新する。送出エラーの有無は問わない。session 層が Largest を記録する条件と揃えるためである。
       - loc: PUBLISH 購読の filter の結果が Pass で、かつ PUBLISH 側の送出エラーがない場合だけ更新する。現状の `if published == ObjectFilterOutcome::Pass` (`.await?` の後) と同じであり、loc の挙動はこの issue では変えない。
  5. 結果は次のように扱う。この判断は確定の関数の中で行い、media の結果処理には渡さない。
     - いずれかの購読に届いた: `location` を `{group_id, 0}` にする。
     - どの購読にも届かなかった (Ok で、PUBLISH 購読は PAUSED などで Skip、受信 SUBSCRIBE の購読者もいないか filter で落ちた): `location` を `None` のまま続け、英語のログを出す。正常系である。確定済みなので、以後の送り直し (定期再送や SUBSCRIBE の受理時の送り直し) が、いずれかの購読に届いた時点で `location` を `Some` にする。この状態では Object がどの購読の filter も通っていないので、session 層も Largest Object を観測しておらず、FETCH の扱いと食い違わない。
     - Err: `handle_catalog_republish` と同じに分類する。`is_transport_session_end` なら `session_ended` を立てて `LoopFlow::Break` を返す。`is_peer_stream_reset` なら英語のログを出して続ける。それ以外は `close_on_message_error` の後にエラーを返す。
- 現状の初回送出は Skip を終了扱いにする。cmaf の確定では、これに合わせない。cmaf の確定はループの中で起きるので、relay が catalog の購読を pause した後になり得る。そこで終了すると、LOC 経路にはない終了経路ができてしまう。
- 確定前の要求は、保留しない。
  - catalog の SUBSCRIBE は即時に受理する。catalog の SUBSCRIBE_OK は、カタログの JSON と Location に依存しない (`subscribe_ok_properties` は catalog に `TrackProperties::new()` を返す)。Object がない状態の LARGEST_OBJECT の省略は、MoQT-23 §9.19.17 で定義されている。受理した購読には、確定時の送出 (`republish_catalog_at_group` は catalog の購読者にも送る) でカタログが届く。
  - `location` が `None` の間の catalog の FETCH は、Object がない状態として INVALID_RANGE で拒否する。根拠は MoQT-23 §3.2 の「If no Objects have been published for the track ... the publisher MUST return FETCH_ERROR with error code INVALID_RANGE.」である。
    - 購読がある場合は、session 層 (`src/session/fetch.rs`) が先に拒否する。
    - 購読がなく session 層が受理した FETCH は、moq-pub が拒否する。
    - そのために、`catalog_fetch_response` の Location 引数と `CatalogState` の `location` を `Option<Location>` にし、`None` を `InvalidRange` とする。
  - catalog の fill fetch (`OpenFillFetchStream`) は、Largest Object がない間は session 層が開かない (`src/session/subscription/fill.rs`)。防御として、`location` が `None` なら `reject_fill_fetch` にする。
- 確定前の送り直しは飛ばす。
  - 送り直しの 5 経路 (`handle_catalog_republish` / `apply_republished_request` / `handle_publisher_request_event` / `republish_catalog_on_migrated_session` / `handle_migrated_catalog`) は、確定前はカタログの送出だけを飛ばす。
  - 同じ関数の中で行う PUBLISH の張り直しは、確定前も行う。具体的には、`republish_request_on_current_session` の新しい PUBLISH / `catalog_request_id` の差し替え / 旧 request への PUBLISH_DONE と、`handle_migrated_catalog` の PUBLISH_DONE / `catalog_request_id` / `catalog_publish_target` の更新である。確定時の送出は、その時点の送り先に向かう。
  - `cat_tokens_changed` のときの作り直しも、確定前は飛ばす。確定時の手順 2 が、その時点の `cat_tokens` で組み立てる。`replace_with_migrated_session` は確定前も `config.url` を更新する。
  - session 単位の移行では、移行先の session も未確定の状態のまま続け、確定した時点でカタログを送る。
- カタログの状態の判断は、I/O を持たない型に切り出す。既存の `gop::GopTracker` と同じく、その型の単体テストで状態遷移を固定する。疑似キャプチャの入力とイベントの到着順は `run_publisher_loop` の `select!` で決まらず、モックも使えない (AGENTS.md) ためである。型が持つ判断は次の 4 つに限る。
  - 確定の契機: 有効な track ごとに最初の sample を記録し、未確定 (`json` がない) のときだけ、全部揃ったかを返す
  - カタログを送ってよいか・作り直してよいか: `json` があるか。送り直しの 5 経路と、`cat_tokens_changed` のときの作り直しが参照する
  - `location` の更新: 送出の結果型を受けて、packaging ごとの規則で更新する (cmaf は「PUBLISH の filter が Pass か、受信 SUBSCRIBE のどれかに届いた」、loc は「PUBLISH の filter が Pass で送出エラーなし」)
  - FETCH と fill fetch に応答できるか: `location` があるか
- PUBLISH の張り直し・PUBLISH_DONE・送り先の更新・catalog の SUBSCRIBE の受理という I/O は、既存の経路を変えずに使う。
  - 送り直しの 5 経路は、カタログを送る直前に型の「送ってよいか」を参照し、送った結果型を型の「`location` の更新」に渡してから、PUBLISH 側の送出エラーを経路ごとの現状の扱い (上記) で処理する。
  - これらの I/O は型の判断を通らないので、型の単体テストの対象にしない。
- 確定時の送出では、その時点でループが持つ `handle` / `catalog_request_id` / `catalog_publish_target` を参照する。確定前にこれらを別の変数へ写して持ち越さない。こうすると、確定前に `replace_with_migrated_session` や request 単位の移行で差し替えられた値で送られる。
- 確定後は、init segment を `initDataList` の `inline` entry に Base64 で入れ、track の `initRef` から参照する。
  - cmaf の track は Video Config / Audio Config を Object property で送らない (cmaf の Object には LOC property を付けない)。そのため MSF-02 §12.2 の object-property の initRef は掛からず、object-property の initDataList entry とそれを指す initRef は載せない。
  - 1 つの track が持てる initRef は 1 つ (MSF-02 §5.2.14) なので、cmaf の track の initRef は CMSF-01 §3.1 の inline entry だけを指す。0467 は、cmaf の track の initRef が inline 以外の entry を指すカタログを `InvalidCatalog` にする。
  - moq-pub は Base64 の crate に依存していないので、`base64ct` を加える (shiguredo-rust の選定に従う)。
  - catalog の track には `packaging: "cmaf"` と `initRef` を載せる。`CatalogContext` が init segment を持つので、`cat_tokens_changed` で作り直すカタログにも initDataList と initRef が載る。

### 範囲外

- prft による壁時計の同期 (0473)
- init segment の ftyp に CMAF の brand を付けること
  - CMAF 本体 (ISO/IEC 23000-19) が `refs/` になく、CMAF header の brand 要件を確認できない。そのため `Fmp4SegmentMuxer` の ftyp のまま送る。
  - brand の扱いは、CMAF 本体を確認したうえで別 issue にする。
- 配信中の sample entry の変更 (init segment の差し替え)
- SAP Type timeline track と Content Protection の発行
- altGroup (switching set) の配信。`build_catalog` は `alt_group` を設定しない

## 完了条件

- `--packaging cmaf` で、init segment を `initDataList` に載せたカタログを送ること。`cat_tokens` が変わる session 移行で作り直したカタログにも、init segment の inline entry とそれを指す initRef が載ること。cmaf のカタログの initDataList に object-property の entry がないこと。
- 1 sample ごとの Object (moof + mdat) を Subgroup 0 で送ること。音声も subgroup で送り、Object に LOC property を付けないこと。
- tfdt と timescale が、LOC 経路で送る Timestamp / Timescale と一致すること。live capture では、tfdt が capture の Unix epoch マイクロ秒になること。例外は、Timestamp が直前の chunk の tfdt 以下になる場合だけである。その場合 tfdt は「直前の tfdt + 1」になり、track ごとに逆行しないこと。直前の tfdt より大きい Timestamp (間隔が名目の duration より短い場合を含む) は、そのまま使われること。
- trun の duration / size / flags が sample ごとに、data_offset が trun ごとに書かれていること。`Fmp4SegmentDemuxer` で demux した sample の時刻・サイズ・keyframe 判定が、送信側と一致すること。
- 同じ Location の media の Object のバイト列が、すべての経路で一致すること。経路は、PUBLISH、受信 SUBSCRIBE の購読者、FETCH (`media_cache`) である。音声の終了時のフェードの chunk が、同じ track の sequence_number と tfdt の続きで組まれ、PUBLISH と `media_cache` で同じバイト列になること。
- 確定前も media の Object を送り、media track への要求を処理すること。
- 切り出した型の単体テストで、次の判断を固定すること。
  - 確定の契機: 有効な track の最初の sample が揃った時点で揃ったと返すこと (`--no-audio` と `--input-mp4` では音声を待たないこと)
  - カタログを送ってよいか・作り直してよいか: 確定前 (`json` がない) は否、確定後は可を返すこと
  - `location` の更新: cmaf では、PUBLISH の filter が Pass か受信 SUBSCRIBE のどれかに届いたら、送出エラーがあっても `Some` になること。どの購読にも届かなければ `None` のままであること。loc では、PUBLISH の filter が Pass で送出エラーがないときだけ更新されること (従来どおり)
  - FETCH と fill fetch: `location` が `None` の間は応答できない (INVALID_RANGE / `reject_fill_fetch`) と返すこと
- 確定を実行する関数について、次のとおりになること。
  - 送出エラーを `handle_catalog_republish` と同じ 3 分類で扱うこと。3 分類は `handle_catalog_republish` と共有する I/O を持たない分類関数に切り出し、その単体テストで固定する
  - media の結果処理に送出エラーを渡さないこと
  - 送出時に、その時点でループが持つ `handle` / `catalog_request_id` / `catalog_publish_target` を参照すること (確定前に写した値を使わないこと)
  - 上の 2 つは関数の構造で満たす要件である。モックを使えず、到着順も制御できないので、テストでは観測できない。コードレビューで確認する
- 手順 1〜3 が失敗したら英語のエラーで終了すること
- 配信中の sample entry の変化を検出する関数を、単体テストで固定すること (同じ設定の keyframe では変化なし、SPS や解像度が変わった keyframe では変化あり)
- 確定後の結果 (カタログの内容と FETCH への応答) を `peer_integration_tests` で確認すること。
- `--stream-mapping` を明示して `--packaging cmaf` と併用すると、エラーになること。
- sample entry について、次のとおりになること。
  - `--input-mp4` で、sample entry が 2 種類以上ある MP4 を cmaf では拒否すること
  - 配信中に sample entry が変わったら、英語のエラーで終了すること
- `docs/msf.md` の CMSF の節に、moq-pub の配信形式と「ずれ」の判断を追記すること。
- `crates/moq-pub/README.md` について、次を行うこと。
  - 実行例とオプション表に `--packaging` を追加する。
  - 注意事項に、cmaf での制約を書く。制約は、`--stream-mapping` の明示がエラーになること、音声を subgroup で送ること、`--datagram-max-size` / `--audio-redundancy` を警告して無視することである。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
