# moq-sub で cmaf packaging の購読に対応する

- Created: 2026-11-23
- Completed: {YYYY-MM-DD}
- Branch: feature/add-cmsf-draft-01
- Polished: 2026-11-23

## 目的

moq-sub は `loc` 以外の track を購読対象から外している。そのため、CMSF の publisher が出す cmaf の media は再生も録画もできない。本 issue では、moq-sub が cmaf の映像 / 音声 track を購読・デコード・録画できるようにする。

前提となる issue は次のとおりである。CMSF 対応全体の方針は 0467 を参照すること。

| issue | 内容 | 本 issue での使い方 |
| --- | --- | --- |
| 0467 | cmaf packaging | カタログの decode |
| 0468 | `stream_mapping` を loc に限る | `extract_video_info` が cmaf の track で失敗しない |
| 0470 | Content Protection | 保護 track の判別 |
| 0471 | moq-pub の cmaf 配信 | E2E テストの送信側 |

壁時計の同期 (prft) は 0473 で扱う。本 issue では、cmaf の Object を `TimestampKind::MediaTime` として扱う。

## 現状

- `crates/moq-sub/src/pipeline.rs` の `select_video_track` / `select_audio_track` は、`is_loc_packaging` で `loc` 以外の track を除外する。
- カタログの更新はメディアの経路に届かない。起動時に解決した track は変更せず、カタログの状態だけを最新に保つ。
- LOC 経路では、Object の LOC property から次を得る。
  - 表示時刻: `extract_timestamp_timescale`、`loc_timestamp_us`、`timestamp_kind_of`
  - codec 設定: `extract_video_config` / `extract_audio_config`。中身は avcC / hvcC のレコード本体、AV1 の Sequence Header OBU、Opus の OpusHead、AAC の AudioSpecificConfig である
  - keyframe の判定: `is_video_keyframe`。Video Frame Marking を使い、なければ Video Config の有無で判定する。この判定は、復号順の判定と録画で共有する
- `crates/moq-sub/src/main.rs` の `wall_clock_timestamp` / `wall_clock_pts` は、`TimestampKind::WallClock` のときだけ時刻を返す。`TimestampKind::MediaTime` / `TimestampKind::None` の映像フレームは時間軸に載らず、届いた順に表示される。音声は時間軸に載らなくても、並べ方に PTS そのものを使い、PTS が逆行した Object は `AudioDeliveryOrder::admit` が捨てる。
- 音声デコーダは codec 設定を受け取らない。`crates/moq-sub/src/decoder.rs` の `AudioDecoder::build(codec, sample_rate, channels)` と `AudioDecoder::decode(data)` は、catalog の samplerate / channelConfig だけで構成する。
  - LOC の Audio Config (OpusHead) を使うのは、catalog との整合の警告 (`validate_audio_config`) と録画 (`record_audio_object` → `parse_opus_head`) だけである。
- 映像・音声の配送は Object 単位である。
  - `deliver_video_object` は 1 Object を 1 回だけ復号し、`send_decoded_frames` は Object の時刻を全フレームに載せる。復号順の判定 (`VideoDecodeOrder`) は `VideoObjectPosition` (Group ID / Object ID / keyframe / Prior Object ID Gap) で行う。Prior Object ID Gap は MOQT の Object Properties から `prior_object_id_gap_of` で読む。
  - `AudioDelivery::deliver` も、重複・購読境界・送出順の判定と復号を Object ごとに 1 回行う。
- MP4 録画 (`--mp4`、`crates/moq-sub/src/mp4.rs`) は、LOC の codec 設定から sample entry を組み立て、Object payload をそのまま sample として書く。
  - 映像: VP8 / VP9 は LOC の設定から sample entry を作れないため、録画に対応していない (`build_video_entry`)。`Writer::accept_video` は keyframe で Video Config がある sample まで待ち、`Writer::build_entry` は Video Config がなければエラーにする。
  - 音声: `build_audio_entry` が Opus の sample entry しか作らないため、Opus のみに対応する。AAC の track は `start_media_recorder` が "MP4 recording supports Opus audio only; audio is not recorded" と警告し、録画しない。`crates/moq-sub/README.md` の `--mp4` の行にも同じ制約が書かれている。
  - 録画器は sample を PTS の順に並べ、`composition_time_offset` を書かない。
- moq-sub は Base64 の crate に依存していない。
- `shiguredo_mp4` (2026.6.0-canary.2) について:
  - `Fmp4SegmentDemuxer::handle_init_segment` は init segment を解析する。
  - `Fmp4SegmentDemuxer::handle_media_segment` は、1 回の呼び出しで moof + mdat を 1 組しか処理できない。2 組以上を含む入力はエラーになる。
  - moof の前の `prft` などは、解釈せずに読み飛ばす。
  - sample の時刻は tfdt と trun から求め、PTS は `timestamp + composition_time_offset` である。
  - moov にない track_ID の traf はエラーにする。
  - `Fmp4SegmentDemuxer::tracks` が返す `TrackInfo` は track_id / kind / duration / timescale だけを持ち、sample entry は公開しない。sample entry は demux した `Sample::sample_entry` でしか得られず、変化がなければ `None` になる。`Fmp4SegmentDemuxer` は `Clone` を実装している。
  - init segment の moov は `shiguredo_mp4::boxes::MoovBox` で decode でき、stsd の entry を直接読める。

## CMSF-01 と MSF-02 / ISO BMFF のずれ

| 項目 | CMSF-01 | MSF-02 / ISO BMFF | 本実装の扱い |
| --- | --- | --- | --- |
| moof 内の Box 名 | §3.3: moof が「Track Box (trak) with a Track ID (track_ID)」を含む | ISO BMFF では moof が持つのは traf / tfhd の track_ID | traf / tfhd の track_ID が init segment の trak と一致すること、と読み替えて検証する |
| Object から Subgroup への割り当て | 定めない | MSF-02 §6: packaging が定めない限り Subgroup 0 に置く (SHOULD) | cmaf の track は Subgroup 0 を前提に、group mapping と同じ経路で受信する。1 Group を複数の Subgroup に分ける publisher には対応しない。moq-sub の group 経路は stream ごとに復号器を作る (`build_stream_video_decoder` の呼び出し元) ため、Subgroup をまたぐ参照が切れる |

## 設計方針

- track の選択:
  - `select_video_track` / `select_audio_track` は、`loc` と `cmaf` の両方を選べるようにする。
  - `contentProtectionRefIDs` を持つ cmaf の track は除外する (CENC の復号は範囲外)。
  - `extract_video_info` が cmaf の track で `StreamMapping::Group` を使うことは、0468 で実装済みの前提とする。本 issue では変更しない。
- init segment の読み込み (起動時):
  - 起動時に解決した track の `initRef` が指す initDataList の Base64 を decode する。moq-sub に `base64ct` を依存として加える (shiguredo-rust の選定に従う)。
  - init segment の moov を `MoovBox` で decode し、次を確かめて sample entry と timescale (mdhd) を保持する。
    - media の trak (handler が vide か soun) が 1 つだけで、role (映像 / 音声) と handler が一致する。CMSF §3.1 は CMAF header を「associated with a CMAF track」とする
    - stsd の entry が 1 つだけで、moq-sub が扱える種類 (avc1 / hev1 / hvc1 / av01 / vp08 / vp09 / Opus / mp4a。shiguredo_mp4 の `SampleEntry` の variant と、moq-sub のデコーダが扱う codec に合わせる) である
    - sample entry の codec の種類が、catalog の `codec` の種類と一致する。デコーダは catalog の codec から作るためである
    - avc1 は avcC に SPS / PPS が、hev1 / hvc1 は hvcC に VPS / SPS / PPS がある。
      - moq-sub のデコーダ (`crates/moq-codec` の `parse_avc_config` / `parse_hvc_config`) は、パラメータセットのない設定を拒否する。
      - パラメータセットを in-band でだけ運ぶ track (`SampleEntry::Hev1` の doc にある「パラメータセットが in-band 前提」) には対応しない。moq-pub も、パラメータセットのない sample entry の MP4 を拒否している (`has_hevc_parameter_sets`)
  - 満たさない場合は、既存の `extract_video_info` の失敗と同じく、英語のエラーで起動を失敗させる。
  - 同じ init segment で `Fmp4SegmentDemuxer::handle_init_segment` を一度だけ呼び、初期化済みの demuxer を track ごとに保持する。stream ごとの task は、それを clone して使う。`handle_media_segment` は `&mut self` を取るので、共有して lock するより clone のほうが既存の stream ごとの処理と合う。
  - カタログの更新はメディアの経路に届かない (現状どおり)。そのため、購読中に init segment が変わる場合は扱わない。
- Object payload の分割と検証:
  - Object payload はトップレベルの box の境界で分割し、moof + mdat の組ごとに `handle_media_segment` に渡す。CMSF §3.3 は 1 Object に複数の CMAF Chunk を許すが、`handle_media_segment` は 1 回に 1 組しか処理できないためである。moof の前にある box (prft など) は、直後の組に含めて渡す。
  - Object 内の全組を demux して検証し終えてから、復号・録画に進む。demux した sample は demuxer を借用しているので、所有する値 (mdat 内のデータ範囲・DTS・composition offset・duration・keyframe) に写してから使う。
  - Object 単位で次を検証する。違反した Object は破棄し、英語のログを出す。
    - moof + mdat を 1 組以上含む
    - 全組のすべての traf の track_ID が、init segment の media trak の track_ID と一致する (§3.3 の「MUST contain a single track」)。traf の数は問わない。ISO BMFF は 1 つの moof に同じ track の traf を複数置くことを認めている (shiguredo_mp4 の `demux_fmp4_segment.rs` のコメントによる)
    - この検査は `MoofBox` の traf の track_ID を直接照合して行う。demuxer は、moov にあっても handler が vide / soun / subt / text 以外の trak の traf を、エラーにせず黙って捨てるので、demuxer の結果だけでは single track を確かめられない
    - Group 先頭の Object (Object ID 0) の先頭 sample が keyframe である
  - 分割・検証・sample の写し出しは I/O を持たない関数に切り出し、単体テストで固定する。入力は `shiguredo_mp4::boxes` で組み立てた実際のバイト列とし、モックは使わない (AGENTS.md)。
- cmaf の Object の入口:
  - 既存の `deliver_video_object` / `AudioDelivery::deliver` は、時刻と keyframe を LOC property から得る。そのため、cmaf の Object には別の入口の関数を設ける。入口が受け取るのは次のとおりである。
    - 分割と検証を終えた所有値の sample の列
    - track の init segment 由来の情報 (mdhd の timescale、sample entry)
    - Object の位置 (Group ID / Object ID)。音声の重複と購読境界の判定に使う
    - MOQT の Object Properties から解決した Prior Object ID Gap。復号順の判定 (`VideoObjectPosition`) に使う。この値は LOC ではなく MOQT の Object Properties (`prior_object_id_gap_of`) から読むので、cmaf の Object にも載りうる。渡さずに 0 とすると、publisher が意図して Object ID を飛ばした後の Object が `MissingReference` になり、次の keyframe まで復号されない
  - 重複・購読境界・復号順の判定、復号、送出、録画の処理は、LOC 経路と共有する内部の関数を呼ぶ。現状では、`AudioDelivery::deliver` はこれらを 1 つのメソッドにまとめているので、共有できる単位に分ける。
  - cmaf の track では、次の 5 つの呼び出し元が cmaf の入口を呼ぶように分岐させる。
    - 映像: `decode_video_stream` (group モードの live) と `read_video_fetch_stream` (FETCH 応答)。映像の FETCH は、起動時・session の移行時・request の移行時に `fetch_recent_video_groups` で発行される (SUBSCRIBE_OK に Largest Object がない場合は発行しない)。そのため FETCH 応答の経路を漏らすと、FETCH で取得した keyframe が LOC 経路で捨てられる。それでも次の GOP からはフレームが出るので、E2E では気づけない
    - 音声: `decode_audio_stream` (subgroup)、`deliver_datagram` (main ループの datagram)、`run_side_session` (request 移行の side session の datagram)
  - `ObjectModeVideo::deliver_object` (object モード) は通らない。0468 により、cmaf の track は `StreamMapping::Group` になるためである。
  - track 単位の状態は、種類ごとに作る場所と配り方を分ける。
    - 対応外の状態:
      - `prepare_delivery_state` で track ごとに 1 つ作り、既存の `audio_delivery_order` / `video_delivery_order` と同じ経路で、同じインスタンスを配る。
      - 別のインスタンスを作ると、経路をまたいで「エラーを 1 回だけ出す」と「以後すべて破棄する」が成り立たなくなる。
      - 音声 track の状態は、次の 4 つの組み立て箇所に配る。datagram で届くのは音声だけ (`plan_datagram`) なので、映像 track の状態は `DatagramTaskContext` を除く 3 か所に配る。`video_delivery_order` と同じである。
        - main ループの `StreamTaskContext`
        - `run_side_session` の `StreamTaskContext`
        - `spawn_datagram_receiver` が組み立てる `DatagramTaskContext` (`deliver_datagram` が参照する)
        - `build_side_session_shared` が作る `RequestMigrationShared` (`run_side_session` の datagram 分岐が `AudioDelivery` を組み立てるときに使う)
    - 初期化済みの demuxer:
      - カタログを解決する時点で、`extract_video_info` / `extract_audio_info` と同じ段で init segment を検証して作る。失敗は既存と同じく `receive_catalog` の `.transpose()?` で起動の失敗にする。`prepare_delivery_state` は失敗を返せないためである。
      - 作った demuxer は `ChannelSetupInputs` に載せ、対応外の状態と同じ経路で、各 task が clone する元として配る。共有して lock はしない (上記のとおり clone して使う)。
- 1 Object に複数の sample がある場合の処理単位:
  - 重複・購読境界・復号順 (`VideoDecodeOrder`) の判定は、現状どおり Object 単位とする。Object の keyframe には、先頭 sample の `keyframe` を使う。
  - 映像の復号と時刻付けは sample ごとに行う。sample を順に復号し、得たフレームにはその sample の時刻を載せる。
  - 音声は、送出順の判定 (`AudioDeliveryOrder::admit`) と復号を sample ごとに行う。重複と購読境界の判定は Object 単位のままとする。
  - 録画は sample ごとに 1 sample として書く。
- 時刻と keyframe:
  - 表示時刻は、sample の DTS (tfdt と trun から求めたもの) を、init segment の mdhd の timescale でマイクロ秒に換算して求める。catalog の `timescale` は使わない。
  - 換算は `loc_timestamp_us` と同じく u128 で中間計算し、`i64::MAX` で飽和させる。live capture の tfdt は Unix epoch マイクロ秒 (約 1.8e15) で、timescale は 1,000,000 なので、u64 の乗算では桁があふれる。
  - これを `TimestampKind::MediaTime` として扱う。
  - keyframe の判定は、demux した sample の `keyframe` を使う。`is_video_keyframe` の LOC property による判定は使わない。
- composition offset (B フレーム) は対応範囲外とする。
  - 0471 の moq-pub は composition offset を書かない。moq-pub は B フレームを含む MP4 のパススルーも拒否している。
  - 録画器は PTS 順に並べて `composition_time_offset` を書かず、MediaTime の映像は届いた順 (復号順) に表示される。そのため、offset が 0 でない sample は正しく扱えない。
  - composition offset が 0 でない sample を、上の検証と同じ段で検出する。検出したら、その track を「対応外」とし、英語のエラーを 1 回だけ出す。以後、その track の Object はすべて破棄する。
    - 「対応外」の状態は track 単位で共有する。映像は Group ごとに別の stream task になり、音声は datagram と subgroup の両方から届くので、stream task の中に置くと「1 回だけ」も「以後すべて」も満たせないためである。
      - 状態は、`AudioDeliveryOrder` と同じく I/O を持たない専用の型にする。この型は「対応外か」を返すメソッドと、「対応外にする」メソッドを持つ。「対応外にする」メソッドは、初めて対応外になったときだけ真を返す。
      - 型は、既存の `prepare_delivery_state` が track ごとの状態を `Arc<Mutex<..>>` で持つのと同じ置き方で共有し、起動時に解決した track の寿命 (moq-sub のプロセスの間) だけ保つ。
      - 呼び出し側 (cmaf の入口の前段) は、分割の前に「対応外か」を確かめ、対応外なら Object を破棄する。分割と検証の関数が「track を対応外にする」を返したら「対応外にする」を呼び、真が返ったときだけ英語のエラーを出す。
    - 分割と検証の関数は、結果を 3 通りに分けて返す。sample の列を返して続ける場合、この Object だけを破棄して続ける場合 (§3.3 / §3.4 の違反)、track を対応外にする場合 (composition offset) である。
    - session は閉じない。B フレームは CMSF の違反ではないので、PROTOCOL_VIOLATION などで peer との session を閉じるのは誤りになる。
    - stream task から session を閉じる既存の経路 (`termination_tx` → `request_session_termination`) は、peer のプロトコル違反用で、既定のコードは `SESSION_PROTOCOL_VIOLATION` である。そのため、この経路は使わない。
    - 他の track の受信と録画は続ける。
    - 録画器は、映像の最初の keyframe を Writer に渡すまで音声を捨てる (`Writer::accept_audio` の "waiting for the first video keyframe")。そのため、映像の track を対応外にしたときは、録画器に映像の track を放棄したことを伝え、音声の待ちを解く。音声の track を対応外にしたときは、音声を録画しない。
- codec 設定:
  - デコーダに渡す codec 設定を作るのは、H.264 / H.265 だけである。init segment の sample entry の avcC / hvcC から、レコード本体を取り出す。これは moq-pub の `crates/moq-pub/src/mp4.rs` が sample entry から Video Config を取り出す処理 (`video_codec_and_config`) と同じ考え方である。
  - AV1 は設定を作らない。AV1 のデコーダは Video Config を使わず、payload 内の Sequence Header OBU で初期化するためである (`crates/moq-sub/src/decoder.rs`)。
  - 音声デコーダは、現状どおり catalog の samplerate / channelConfig から作る。音声の設定は作らない。
  - Opus の catalog との整合の警告 (`validate_audio_config`) は、cmaf でも行う。init segment の dOps (`DopsBox`) の出力チャンネル数と入力サンプルレートから `OpusHeadConfig` を作り、渡す。
  - sample のデータは mdat から取り出す。H.264 / H.265 は長さ前置の NAL で、LOC 経路の payload と同じ形である。
- `--mp4` 録画:
  - init segment の sample entry をそのまま使い、Object payload ではなく demux した sample のデータを書く。
  - `start_media_recorder` が組み立てる `VideoSetup` / `AudioSetup` に、cmaf の track の sample entry を載せる欄を加える。
  - sample entry を持つ track では、次のとおりにする。
    - `Writer::accept_video` は keyframe だけを開始条件にし、Video Config を待たない。
    - `Writer::build_entry` は sample entry をそのまま使う。
  - これで AV1 の cmaf 映像も録画できる。
  - cmaf の track では、AAC / VP8 / VP9 も録画の対象にする。LOC 経路の録画がこれらを扱えない理由は 2 つある。VP8 / VP9 は LOC の設定から sample entry を作れず、AAC は `build_audio_entry` が Opus の sample entry しか作らない。cmaf では init segment に sample entry そのものがあるので、どちらの理由もなくなる。
  - そのため、`start_media_recorder` の「Opus のみ」の判定は、cmaf の track には適用しない。

## 完了条件

- cmaf の映像 / 音声 track を選択し、デコードできること。
- init segment の前提 (media trak が 1 つ、stsd の entry が 1 つで扱える種類、catalog の codec と一致、avcC / hvcC にパラメータセットがある) を満たさない track で、英語のエラーで起動が失敗すること。
- I/O を持たない関数の単体テストで、次を固定すること。入力は `shiguredo_mp4::boxes` で組み立てた実際のバイト列とする。
  - 複数の CMAF Chunk (moof + mdat の組) を持つ payload の分割と demux、prft を含む組の分割
  - §3.3 / §3.4 の違反 (moof / mdat の欠如、track_ID の不一致、Group 先頭の非 keyframe) で Object を破棄すること
  - composition offset が 0 でない sample を検出したときに、分割と検証の関数が「track を対応外にする」を返すこと (3 通りの結果を返すこと)
  - 対応外の状態の型で、初めて対応外にしたときだけ真が返ること、対応外になった後は「対応外か」が真を返し続けること (以後すべて破棄することと、エラーを 1 回だけ出すことの根拠)
  - media trak 以外の track_ID の traf を含む Object を破棄すること (同じ track の traf が複数あるだけなら受理すること)
  - DTS のマイクロ秒換算 (live capture の epoch マイクロ秒と timescale 1,000,000 で桁があふれないこと)
- 1 Object に複数の sample がある場合に、sample ごとに復号・時刻付け・録画され、復号順の判定は Object 単位で行われること。これは cmaf の入口の関数のテストで固定する。
  - 入力は `shiguredo_mp4::boxes` で組んだ複数 sample の payload とし、既存の `deliver_video_at` (LOC 専用) と同じ形の cmaf 用の補助関数から渡す。
  - 映像は実デコーダ (`build_video_decoder("vp8")` など)、録画は実録画器 (`Recorder::start`)、音声は実際の Opus エンコーダの出力を使う。既存の LOC 経路のテストと同じである。
  - 0471 の moq-pub は 1 Object に 1 sample しか載せないので、E2E では確かめられないためである。
- cmaf の Object の時刻が `TimestampKind::MediaTime` になること。
- cmaf の track で、5 つの呼び出し元 (`decode_video_stream` / `read_video_fetch_stream` / `decode_audio_stream` / `deliver_datagram` / `run_side_session`) がすべて cmaf の入口を呼ぶこと。対応外の状態が、音声は 4 か所、映像は `DatagramTaskContext` を除く 3 か所に、同じインスタンスとして配られていること。初期化済みの demuxer が、カタログの解決時に作られ、同じ経路で clone の元として配られていること。この分岐と配り方は配線の要件で、テストで経路ごとに観測しにくいので、コードレビューで確認する。
- `--mp4` 録画で、demux した sample と init segment の sample entry から MP4 を書けること。cmaf の track では AV1 / AAC / VP8 / VP9 も録画されること。映像の track が最初の keyframe より前に対応外になっても、音声が録画されること。
- `contentProtectionRefIDs` を持つ track を選ばないこと。
- `e2e-tests/tests/roundtrip.rs` に、0471 の moq-pub で cmaf を publish し、moq-sub で subscribe する E2E テストを `#[ignore]` 付きで追加すること。
- `docs/msf.md` の CMSF の節に、moq-sub の受信側の判断を追記すること。追記するのは、受信時に traf / tfhd の track_ID を init segment と照合すること、Subgroup 0 を前提にした受信 (1 Group を複数の Subgroup に分ける publisher には対応しないこと)、composition offset を対応範囲外とすることである。moof の Box 名の読み替えそのものは、0471 で記載済みの前提とする。
- `crates/moq-sub/README.md` に、cmaf の track を購読・録画できることを追記すること。`--mp4` の行の制約 (音声は Opus のみ) は loc の track に限ると書き直すこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
