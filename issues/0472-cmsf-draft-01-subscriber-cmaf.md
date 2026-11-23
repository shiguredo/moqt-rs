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
- `extract_video_info` は catalog の width / height を必須とし、無い場合は失敗する。`VideoTrackInfo` の width / height はログにだけ使う。
- カタログの更新はメディアの経路に届かない。起動時に解決した track は変更せず、カタログの状態だけを最新に保つ。
- LOC 経路では、Object の LOC property から次を得る。
  - 表示時刻: `extract_timestamp_timescale`、`loc_timestamp_us`、`timestamp_kind_of`
  - codec 設定: `extract_video_config` / `extract_audio_config`。中身は avcC / hvcC のレコード本体、AV1 の Sequence Header OBU、Opus の OpusHead、AAC の AudioSpecificConfig である
  - 音声の Audio Config のうち OpusHead は、OpusHead の検証と MP4 録画の sample entry に使う。AAC の AudioSpecificConfig は現状使わない。`AudioDecoder::build` は catalog の samplerate / channelConfig で構成し、Audio Config は受け取らない
  - keyframe の判定: `is_video_keyframe`。Video Frame Marking を使い、なければ Video Config の有無で判定する。この判定は、復号順の判定と録画で共有する
- `crates/moq-sub/src/main.rs` の `wall_clock_timestamp` / `wall_clock_pts` は、`TimestampKind::WallClock` のときだけ時刻を返す。`TimestampKind::MediaTime` / `TimestampKind::None` のフレームは時間軸に載らず、届いた順に表示される。
- MP4 録画 (`--mp4`、`crates/moq-sub/src/mp4.rs`) は、LOC の codec 設定から sample entry を組み立て、Object payload をそのまま sample として書く。
  - 映像: VP8 / VP9 は LOC の設定から sample entry を作れないため、録画に対応していない (`build_video_entry`)。
  - 音声: Opus のみに対応する (`build_audio_entry` は `SampleEntry::Opus` だけを作る)。AAC の track は `start_media_recorder` が "MP4 recording supports Opus audio only; audio is not recorded" と警告し、録画しない。`crates/moq-sub/README.md` の `--mp4` の行にも同じ制約が書かれている。
- `shiguredo_mp4` (2026.6.0-canary.2) について:
  - `Fmp4SegmentDemuxer::handle_init_segment` は init segment を解析する。
  - `Fmp4SegmentDemuxer::handle_media_segment` は、1 回の呼び出しで moof + mdat を 1 組しか処理できない。2 組以上を含む入力はエラーになる。
  - moof の前の `prft` などは、解釈せずに読み飛ばす。
  - sample の時刻は tfdt と trun から求め、PTS は `timestamp + composition_time_offset` である。
  - moov にない track_ID の traf はエラーにする。

## CMSF-01 と MSF-02 / ISO BMFF のずれ

| 項目 | CMSF-01 | MSF-02 / ISO BMFF | 本実装の扱い |
| --- | --- | --- | --- |
| moof 内の Box 名 | §3.3: moof が「Track Box (trak) with a Track ID (track_ID)」を含む | ISO BMFF では moof が持つのは traf / tfhd の track_ID | traf / tfhd の track_ID が init segment の trak と一致すること、と読み替えて検証する |
| Object から Subgroup への割り当て | 定めない | MSF-02 §6: packaging が定めない限り Subgroup 0 に置く (SHOULD) | cmaf の track は Subgroup 0 を前提に、group mapping と同じ経路で受信する。1 Group を複数の Subgroup に分ける publisher には対応しない。moq-sub の group 経路は stream ごとに復号器を作る (`build_stream_video_decoder` の呼び出し元) ため、Subgroup をまたぐ参照が切れる |

## 設計方針

- track の選択:
  - `select_video_track` / `select_audio_track` は、`loc` と `cmaf` の両方を選べるようにする。
  - `contentProtectionRefIDs` を持つ cmaf の track は除外する (CENC の復号は範囲外)。
  - `extract_video_info` が cmaf の track で `StreamMapping::Group` を使うことは、0468 で実装済みの前提とする。この stream mapping の扱いは本 issue では変更しない。
  - cmaf の track では、width / height が catalog に無くても `extract_video_info` が失敗しないようにする。`VideoTrackInfo` の width / height は 0 とし、ログだけに使う。復号と録画は init segment の sample entry と bitstream から得る。
- init segment の読み込み:
  - 起動時に解決した track の `initRef` が指す initDataList の Base64 を decode し、track ごとの `Fmp4SegmentDemuxer::handle_init_segment` に渡す。
    - Base64 の decode には `base64ct` を使う (moq-sub の依存に追加する)。`MsfInitData` の `data` は Base64 文字列であり、`shiguredo_moqt` の `base64_decode` は非公開であるためである。
  - カタログの更新はメディアの経路に届かない (現状どおり)。そのため、購読中に init segment が変わる場合は扱わない。
- Object payload の分割:
  - Object payload はトップレベルの box の境界で分割し、moof + mdat の組ごとに `handle_media_segment` に渡す。CMSF §3.3 は 1 Object に複数の CMAF Chunk を許すが、`handle_media_segment` は 1 回に 1 組しか処理できないためである。
  - moof の前にある box (prft など) は、直後の組に含めて渡す。
- CMSF §3.3 / §3.4 の検証:
  - Object 単位で次を検証する。違反した Object は破棄し、英語のログを出す。
    - moof + mdat を 1 組以上含む
    - track が 1 つだけで、その track_ID が init segment の trak と一致する
    - Group 先頭の Object (Object ID 0) の先頭 sample が keyframe である
- 時刻と keyframe:
  - 表示時刻は、demux した sample の PTS (tfdt と composition offset から求める) を timescale でマイクロ秒に換算して求める。これを `TimestampKind::MediaTime` として扱う。
  - sample ごとにデコーダへ渡し、その sample の PTS を、sample から出たフレームの表示時刻にする。1 Object に複数の sample がある場合も同じである。
  - 復号順の判定 (`VideoDecodeOrder`) は Object 単位のまま、Object の先頭 sample の `keyframe` を Object の keyframe として使う。
  - keyframe の判定は、demux した sample の `keyframe` を使う。`is_video_keyframe` の LOC property による判定は使わない。
- codec 設定:
  - init segment の sample entry から、LOC 経路の Video Config / Audio Config と同じ内容を作る。映像の Video Config はデコーダに渡す。音声の Audio Config のうち OpusHead は、LOC 経路と同じく OpusHead の検証に使う。AAC の AudioSpecificConfig は現状のデコーダでは使わない。cmaf の録画は init segment の sample entry をそのまま使うため、OpusHead から sample entry を作り直さない。デコーダは catalog の samplerate / channelConfig で構成する。
    - H.264 / H.265: avcC / hvcC のレコード本体
    - Opus: dOps から作る OpusHead
    - AAC: esds の AudioSpecificConfig
    - AV1: 設定は作らない。AV1 のデコーダは Video Config を使わず、payload 内の Sequence Header OBU で初期化するためである (`crates/moq-sub/src/decoder.rs`)。
  - これは、moq-pub の `crates/moq-pub/src/mp4.rs` が sample entry から Video Config を取り出す処理 (`video_codec_and_config`) と同じ考え方である。
  - sample のデータは mdat から取り出す。H.264 / H.265 は長さ前置の NAL で、LOC 経路の payload と同じ形である。
- `--mp4` 録画:
  - init segment の sample entry をそのまま使い、Object payload ではなく demux した sample のデータを書く。Recorder の Setup に init segment の sample entry を渡す経路を追加する。
  - 録画では、demux した sample の duration と composition offset を、録画の timescale (マイクロ秒) に換算して `Mp4FileMuxer` の `Sample` に渡す。これにより、B フレームを含む cmaf でも PTS を保つ。
  - cmaf の track では、AAC / VP8 / VP9 も録画の対象にする。LOC 経路でこれらを除外しているのは、LOC の設定から正しい sample entry を作れないからである。cmaf では init segment に sample entry そのものがあるので、この理由がなくなる。
  - そのため、`start_media_recorder` の「Opus のみ」の判定は、cmaf の track には適用しない。

## 完了条件

- cmaf の映像 / 音声 track を選択し、デコードできること。
- width / height の無い cmaf の映像 track も購読できること。
- 1 Object に複数の CMAF Chunk がある Object を、デコードできること。sample ごとに復号され、各フレームの表示時刻がその sample の PTS になること。
- CMSF §3.3 / §3.4 に違反した Object が破棄され、英語のログが出ること。
  - moof / mdat の欠如、track_ID の不一致、Group 先頭の非 keyframe が対象である。
- cmaf の Object の時刻が `TimestampKind::MediaTime` になること。
- `--mp4` 録画で、demux した sample と init segment の sample entry から MP4 を書けること。cmaf の track では AAC / VP8 / VP9 も録画されること。demux した sample の duration と composition offset が MP4 に反映されること。
- `contentProtectionRefIDs` を持つ track を選ばないこと。
- `e2e-tests/tests/roundtrip.rs` に、0471 の moq-pub で cmaf を publish し、moq-sub で subscribe する E2E テストを `#[ignore]` 付きで追加すること。
- `docs/msf.md` の CMSF の節に、moq-sub の対応範囲と上記「ずれ」の判断を追記すること。判断とは、moof の Box 名の読み替えと、Subgroup 0 を前提にした受信 (1 Group を複数の Subgroup に分ける publisher には対応しないこと) である。
- `crates/moq-sub/README.md` に、cmaf の track を購読・録画できることを追記すること。`--mp4` の行の制約 (音声は Opus のみ) は loc の track に限ると書き直すこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
