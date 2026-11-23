# moq-pub で cmaf packaging の配信に対応する

- Created: 2026-11-23
- Completed: {YYYY-MM-DD}
- Branch: feature/add-cmsf-draft-01
- Polished: {YYYY-MM-DD}

## 目的

0467 でライブラリのカタログが cmaf packaging (CMSF-01) を扱えるようになる。本 issue では、moq-pub が CMAF で media を配信できるようにする。0472 の moq-sub と組み合わせれば、CMSF の送受信を example で確かめられる。

前提は 0467 である。CMSF 対応全体の方針は 0467 を参照すること。壁時計の同期 (prft) は 0473 で扱う。それまでは、moq-sub は cmaf の Object を `TimestampKind::MediaTime` として、届いた順に表示する。

## 現状

### moq-pub

- `crates/moq-pub/src/catalog.rs` の `build_catalog` は、映像 / 音声とも `MsfPackaging::Loc` の track を作り、`init_data_list` を空にする。`alt_group` は設定しない。
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
- 既存の送り直し (`republish_catalog_at_next_group`) は、Skip を「どの購読にも届かなかった (PAUSED など)」として正常系で扱い、location を前回のままにする。購読が PAUSED のときは、session 層 (`src/session/subscription/validation.rs` の `object_passes_filters`) が Object を落とし、tokio-moq がそれを `ObjectFilterOutcome::Skip` にする。
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
- 配信中に sample entry が最初のものから変わった場合は、英語のエラーログを出して終了する。init segment を差し替えると、独立カタログの再送と購読側の再初期化が必要になり、本 issue の範囲を超えるためである。

### カタログの確定

init segment は、配信する track の最初の sample がループに届くまで作れない。エンコーダ経路では、エンコードを終えた時点で届く。現状ではエンコードが `run_publisher_loop` の中にあるため、cmaf ではカタログ未確定のままループに入る。loc では現状の順序 (ループに入る前に `prepare_catalog`) を変えない。

- 状態の持ち方:
  - `PreparedCatalog` から `handle` を外に出す。`handle` は media の送信にも使う session の handle なので、カタログの確定前からループが持つ。
  - カタログの中身は次の 2 段の `Option` で持つ。
    - `json` と `group_id` を 1 つの `Option` にまとめる。`None` がカタログ未確定を表す。
    - `location` は別の `Option` とし、カタログの Object が一度でも Pass した時点で `Some` にする。既存の `republish_catalog_at_next_group` と同じく、Group ID は送信の成否によらず進め、location は Pass のときだけ更新する。
  - 送り直しの経路は、`json` の有無 (確定済みかどうか) で動かすかを決める。FETCH と fill fetch に応答できるかどうかは、`location` の有無で決める。
  - `CatalogContext` の組み立てを `prepare_catalog` から切り出す。現状の `prepare_catalog` と同じく、`start_input_tasks` より前に `prepare_catalog_tracks` の結果から作り、ループまで持ち越す。init segment の欄は空にしておく。
- media の扱い:
  - media の Object は、カタログ確定前から通常どおり送る。最初の sample も保持せず、そのまま送る。chunk の組み立てに init segment は要らない。track_ID は、track ごとの muxer が `tracks.len() + 1` で採番するので 1 であり、timescale は前述のとおり事前に決まっている。
  - media track への要求は、確定前も通常どおり処理する。
- 確定の契機: 配信する全 track の最初の sample がループに届いた時点とする。
  - 配信する track は、映像と音声のうち有効なものである。`--no-video` / `--no-audio` で外した track と、`--input-mp4` の音声 (映像のみを配信する) は待たない。
- 確定の手順: 確定の契機で、次の順に処理する。
  1. init segment を作り、`CatalogContext` に入れる。
  2. その時点の `cat_tokens` で `CatalogContext::json` を組み立てる。
  3. 現状の `prepare_catalog` と同じく、`group_id::allocate_initial_group_id` で Group ID を払い出す。`json` と `group_id` を `Some` にする。ここでカタログは確定する。
  4. `catalog_sink` → `republish_catalog_at_group` の経路で、その Group にカタログを送る。この経路を使うのは、確定前に catalog の PUBLISH が request 単位で移行していても、現在の送り先に送るためである。
  5. 結果は既存の送り直しと同じに扱う。
     - Pass: `location` を `{group_id, 0}` にする。
     - Skip: `location` を `None` のまま続ける。どの購読にも届かなかっただけ (PAUSED など) で、正常系である。確定済みなので、以後の送り直し (定期再送など) が次の Group で届ける。
     - Err: `handle_catalog_republish` と同じに分類する。`is_transport_session_end` なら session の終了として扱い、`is_peer_stream_reset` なら続け、それ以外はエラーにする。
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
- 確定前の判断は、I/O を持たない型に切り出す。既存の `gop::GopTracker` と同じく、その型の単体テストで状態遷移を固定する。疑似キャプチャの入力とイベントの到着順は `run_publisher_loop` の `select!` で決まらず、モックも使えない (AGENTS.md) ためである。切り出す判断は次のとおりである。
  - 確定の契機 (どの track の最初の sample を待つか)
  - 要求への応答 (SUBSCRIBE の受理、FETCH と fill fetch の拒否)
  - 送り直しの経路ごとに、張り直しを行いカタログの送出だけを飛ばすこと
  - 確定時の結果の扱い
- PUBLISH の張り直し・PUBLISH_DONE・送り先の更新という I/O は、既存の経路を変えずに使う。
- 確定時の送出では、その時点でループが持つ `handle` / `catalog_request_id` / `catalog_publish_target` を参照する。確定前にこれらを別の変数へ写して持ち越さない。こうすると、確定前に `replace_with_migrated_session` や request 単位の移行で差し替えられた値で送られる。
- 確定後は、init segment を `initDataList` の `inline` entry に Base64 で入れ、track の `initRef` から参照する。catalog の track には `packaging: "cmaf"` と `initRef` を載せる。`CatalogContext` が init segment を持つので、`cat_tokens_changed` で作り直すカタログにも initDataList と initRef が載る。

### 範囲外

- prft による壁時計の同期 (0473)
- init segment の ftyp に CMAF の brand を付けること
  - CMAF 本体 (ISO/IEC 23000-19) が `refs/` になく、CMAF header の brand 要件を確認できない。そのため `Fmp4SegmentMuxer` の ftyp のまま送る。
  - brand の扱いは、CMAF 本体を確認したうえで別 issue にする。
- 配信中の sample entry の変更 (init segment の差し替え)
- SAP Type timeline track と Content Protection の発行
- altGroup (switching set) の配信。`build_catalog` は `alt_group` を設定しない

## 完了条件

- `--packaging cmaf` で、init segment を `initDataList` に載せたカタログを送ること。`cat_tokens` が変わる session 移行で作り直したカタログにも、initDataList と initRef が載ること。
- 1 sample ごとの Object (moof + mdat) を Subgroup 0 で送ること。音声も subgroup で送り、Object に LOC property を付けないこと。
- tfdt と timescale が、LOC 経路で送る Timestamp / Timescale と一致すること。live capture では、tfdt が capture の Unix epoch マイクロ秒になること。
- trun の duration / size / flags が sample ごとに、data_offset が trun ごとに書かれていること。`Fmp4SegmentDemuxer` で demux した sample の時刻・サイズ・keyframe 判定が、送信側と一致すること。
- 確定前も media の Object を送り、media track への要求を処理すること。
- 切り出した型の単体テストで、次の判断を固定すること。
  - 確定の契機: 有効な track の最初の sample が揃った時点であること (`--no-audio` と `--input-mp4` では音声を待たないこと)
  - 確定前の要求への応答: catalog の SUBSCRIBE を受理し、FETCH を INVALID_RANGE、fill fetch を `reject_fill_fetch` で拒否すること
  - 確定前の送り直し: 5 経路と `cat_tokens` の作り直しで、PUBLISH の張り直しは行い、カタログの送出だけを飛ばすこと
  - 確定前の session 単位の移行: 未確定の状態を保つこと
  - 確定時の結果: 送出の前に `json` / `group_id` が確定すること。Pass で `location` が `Some` になること。Skip で `location` が `None` のまま続け、以後の送り直しで届くこと。Err は `handle_catalog_republish` と同じ分類になること
  - `location` が `None` の間は、FETCH を INVALID_RANGE、fill fetch を `reject_fill_fetch` で拒否すること
- 確定時の送出は、その時点でループが持つ `handle` / `catalog_request_id` / `catalog_publish_target` を参照すること (確定前に写した値を使わないこと)。
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
