# codec / bitrate / samplerate / channelConfig の MUST 検証が role 依存になっている

- Created: 2026-09-21
- Completed: 2026-09-23
- Branch: feature/fix-msf-media-track-required-fields
- Polished: 2026-09-22

## 目的

draft-ietf-moq-msf-01 §5.2.18 (Codec) / §5.2.22 (Maximum Bitrate) / §5.2.28 (Audio sample rate) / §5.2.29 (Channel configuration) は、必須条件を codec が定まるトラック (audio / video) に課しており、role フィールドの有無を条件にしていない。

> This property MUST be specified for tracks which have an inherent codec associated with them (e.g., audio and video tracks).  It is not required for raw data tracks or event streams.

> A number defining the maximum bitrate of the track, expressed in bits per second.  This property MUST be specified for audio and video tracks.

> The number of audio frame samples per second.  This property MUST accompany tracks for which audio codecs are specified.

> A string specifying the audio channel configuration.  A string is used in order to provide the flexibility to describe complex channel configurations for multi-channel and Next Generation Audio schemas.  This property MUST accompany tracks for which audio codecs are specified.

role は §5.2.6 (Track role) で Optional とされ、custom role も許されるため、MUST の判定根拠にならない。

> A string defining the role of content carried by the track.  Specified roles are described in Table 4.  These role values are case-sensitive.

> Custom roles MAY be used as long as they do not collide with the specified roles.

現状は role が `video` / `audio` / `audiodescription` のときだけ検証するため、role を省略した MUST 違反のカタログを library がそのまま encode / decode してしまう。

## 現状

`src/msf.rs` の `validate_media_track_fields` は `MsfTrack::role` から `is_audio` / `is_video` を決め、どちらでもなければ `Ok(())` を返す。
`role` が `audio` / `audiodescription` なら codec / bitrate / samplerate / channelConfig を、`video` なら codec / bitrate を要求する。
doc コメントにも「role が省略されたトラックは「inherent codec を持つ」と機械的に判定できないため、role が明示されたトラックのみを対象とする」と記録されている。

確認した挙動は次のとおり。

- role 省略 + codec `opus` のみ (bitrate / samplerate / channelConfig なし) のカタログは `MsfCatalogDocument::decode` に成功する
- role `audio` + codec `opus` で samplerate または channelConfig を欠くカタログは拒否される

`validate_media_track_fields` は `validate_full_track` と `decode_track` から直接呼ばれる。`validate_full_track` は `MsfCloneTrack::into_track` の継承解決後、delta の add (`apply_delta_in_place`)、encode の 2 経路 (`validate_full_catalog_for_encode` / `validate_delta_for_encode` の add) から呼ばれるため、encode / decode と delta 適用の全経路で同じ抜けがある。
delta の clone 断片 (`MsfCloneTrack`) 自体は `validate_clone_track_fragment` が検証し、親から継承し得るため欠如側の必須検査は行わない (継承解決後のトラックが `validate_full_track` で検証される)。
`pbt/tests/prop_msf.rs` のトラック生成器は codec を生成しないため、この抜けの影響を受けていない。

## 設計方針

role に依存せず、codec 文字列から audio / video を判定して MUST を検証する。判定表は [WEBCODECS-CODEC-REGISTRY](https://www.w3.org/TR/webcodecs-codec-registry/) (Registry Draft, 2026-02-12 版) の Audio Codec Registry / Video Codec Registry の登録内容に一致することを確認済みである。

| 判定 | codec string (レジストリの登録表記) |
| --- | --- |
| audio | `flac` / `mp3` / `mp4a.*` / `opus` / `vorbis` / `ulaw` / `alaw` / `pcm-*` |
| video | `av01.*` / `avc1.*` / `avc3.*` / `hev1.*` / `hvc1.*` / `vp8` / `vp09.*` |

- 一致規則は登録表記ごとに次のとおりにする。`*` は可変部分 (可変サフィックス) を表し、`*` に隣接する区切り文字も導入記号として可変部分に含める
  - 登録表記が `名前` (`opus` / `vp8` / `flac` / `mp3` / `vorbis` / `ulaw` / `alaw`) なら、完全一致のみ
  - 登録表記が `名前.*` (`av01.*` / `avc1.*` / `avc3.*` / `hev1.*` / `hvc1.*` / `vp09.*` / `mp4a.*`) なら、`名前` との完全一致、または `名前.` で始まる場合
  - 登録表記が `pcm-*` なら、`pcm` との完全一致、または `pcm-` で始まる場合
- 例: `opus` / `flac` / `vp8` / `av01` / `av01.0.08M.08` / `avc1.640028` / `hvc1.1.6.L120.B0` / `mp4a` / `mp4a.40.2` / `pcm` / `pcm-s16` / `vp09.0.10.08` は一致し、`opusx` / `opus.2` / `vp8x` / `flacx` は一致しない
- `*` 付きの登録名でも登録名だけの文字列を一致として扱う。MSF のカタログ例には `"codec":"av01"` (draft-ietf-moq-msf-01 §5.6.2) があり、`av01.` に限定すると §5.2.22 の MUST を素通ししてしまう (`"codec":"opus"` (§5.6.1) は `*` なしの登録名なので完全一致で判定する)。
  `examples/moqt-subscriber/src/pipeline.rs` が `c.starts_with("av01")` などでトラック種別を判定している考え方と同じだが、区切り文字の境界を要求する点が異なる
- audio と判定したトラックに bitrate / samplerate / channelConfig を、video と判定したトラックに bitrate を要求する
- codec が無いトラックと、上記のどれにも一致しない codec のトラックは codec から種別を判定できないため、codec に基づく要求は行わない
- role がある場合の検証は維持する。role が `video` / `audio` / `audiodescription` なら、codec が無い場合も含めて従来どおり codec / bitrate (audio 系なら samplerate / channelConfig) を要求する
  - §5.2.6 Table 4 の `signlanguage` は "A visual track for hearing impaired users." と定められる visual track のため video として扱い、codec / bitrate を要求する (`caption` / `subtitle` / `mediatimeline` / `eventtimeline` / `log` / `metrics` は audio / video の codec を持たないため対象外)
- role による判定と codec による判定が食い違う場合は、両方の要求を重ねて適用する。例えば role=`video` + codec=`opus` は、video の codec / bitrate と audio の bitrate / samplerate / channelConfig をすべて要求する
- codec が無いトラックに codec に基づく要求を行わないのは §5.2.18 の "It is not required for raw data tracks or event streams." と整合する

`examples/moqt-publisher/src/catalog.rs` は role を設定せず、video に codec `av01.0.08M.08` (`--video-codec h264` / `h265` では `avc1.640028` / `hvc1.1.6.L120.B0`) と bitrate、audio に codec `opus` と samplerate / channelConfig / bitrate を設定する。いずれも上記の判定に一致するため、生成するカタログは引き続き encode に成功する。

判定表はレジストリの登録内容に一致することを確認済みである。レジストリは "Existing entries cannot be deleted or deprecated." と定めるため既存エントリが消える心配はなく、新規登録があった場合は同じ表に追記する。
MSF の参考文献は同レジストリを September 2024 版として引用しているが、本 issue は現行の Registry Draft (2026-02-12 版) の登録内容を正とする。

## 完了条件

- role 省略 + codec `opus` で samplerate を欠くカタログを `MsfCatalogDocument::decode` が拒否するテストが `tests/test_msf/error_cases.rs` に追加されていること
- role 省略 + codec `opus` で channelConfig を欠くカタログを拒否するテストが追加されていること
- role 省略 + codec `opus` で bitrate を欠くカタログを拒否するテストが追加されていること
- role 省略 + codec `av01.0.08M.08` で bitrate を欠くカタログを拒否するテストが追加されていること
- role 省略 + codec `opus` で bitrate / samplerate / channelConfig を揃えたカタログを受理するテストが追加されていること
- encode 経路 (`MsfCatalog` を手組みして `MsfCatalogDocument::encode`) でも同じ拒否になるテストが追加されていること
- codec を持たないトラック (raw data / event stream 相当) が codec / bitrate / samplerate / channelConfig を要求されないテストが維持されていること。
  `missing_role_does_not_require_media_fields` は codec も role も無いトラックを検証しているため、doc コメントを「codec が無いため種別を判定できない」旨に更新する。
  実装側の `validate_media_track_fields` の doc コメント (`role が明示されたトラックのみを対象とする`) も新しい判定規則に合わせて直す
- 一致規則に一致しない codec (`opusx` / `opus.2` など) が audio / video と判定されず、codec に基づく要求が行われないテストが追加されていること
- `pcm-*` の登録名 (`pcm-s16` など) が audio と判定されること (bitrate を与えても samplerate を欠けば拒否される) が登録名の表駆動テストで検証されていること
- role と codec の判定が食い違う場合に両方の要求が重なるテストが追加されていること (例: role=`video` + codec=`opus` で bitrate は満たしつつ samplerate / channelConfig を欠くカタログを拒否する。bitrate も欠くと bitrate の要求で落ちて重なりを検証できない)
- MSF のカタログ例と同じ登録名だけの codec (`av01`) でも video と判定されるテストが追加されていること (bitrate を欠くカタログを拒否する)
- role が `video` / `audio` のときの既存検証 (`video_role_missing_codec_rejected` / `video_role_missing_bitrate_rejected` / `audio_role_missing_samplerate_rejected` / `media_role_with_required_fields_accepted`) が維持されていること
- role が `signlanguage` のときも video として codec / bitrate を要求し、`audiodescription` のときも audio として samplerate / channelConfig を要求するテストが追加されていること
- `examples/moqt-publisher/src/catalog.rs` が生成する構成 (role なし / video: codec `av01...` と bitrate / audio: codec `opus` と samplerate と channelConfig と bitrate) のカタログが `encode` に成功すること
- `make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ること

## 解決方法

`src/msf.rs` の `validate_media_track_fields` を、role だけでなく codec 文字列からも audio / video を判定して必須フィールドを検証するよう修正した。

- `is_audio_codec` / `is_video_codec` を追加し、WEBCODECS-CODEC-REGISTRY (Registry Draft, 2026-02-12) §3 / §4 の登録表記に一致する codec を audio / video と判定する
  - `*` 付き登録名 (`av01.*` / `mp4a.*` / `pcm-*` など) は登録名単独 (`av01` / `mp4a` / `pcm`) と可変サフィックス付きを一致とし、区切り文字の境界を要求する
  - `*` なし登録名 (`opus` / `vp8` など) は完全一致のみとする
- `require_audio = role_audio || codec_audio` / `require_video = role_video || codec_video` とし、role と codec の判定が食い違う場合は両方の要求を重ねて適用する
- §5.2.6 Table 4 で "A visual track for hearing impaired users." とされる `signlanguage` を video として扱う
- エラーメッセージに要求の根拠 (`role '...'` / `codec '...'`) を含め、成功経路では文字列を組み立てない
- `examples/moqt-publisher/src/catalog.rs` の `send_catalog` から `build_catalog` を切り出し、publisher が生成する構成を単体テストできるようにした (挙動は変えない)
- `CHANGES.md` と `docs/IMPLEMENTATION.md` を新しい判定規則に追随させた

追加・更新したテスト:

- `tests/test_msf/error_cases.rs`
  - decode 拒否:
    - `codec_audio_missing_samplerate_rejected`
    - `codec_audio_missing_channel_config_rejected`
    - `codec_audio_missing_bitrate_rejected`
    - `codec_video_missing_bitrate_rejected`
    - `registry_name_only_video_codec_missing_bitrate_rejected`
    - `codec_separator_only_suffix_is_classified`
    - `codec_boundary_separator_is_required`
    - `role_and_codec_requirements_are_combined`
    - `signlanguage_role_is_treated_as_video`
    - `audiodescription_role_requires_audio_fields_rejected`
  - decode 受理:
    - `codec_audio_with_required_fields_accepted`
    - `codec_video_with_required_fields_accepted`
    - `unregistered_codec_does_not_require_media_fields`
  - 登録名の表駆動 (両方向で判定を固定):
    - `registry_audio_codec_names_are_classified`
    - `registry_video_codec_names_are_classified`
  - encode:
    - `codec_audio_missing_bitrate_rejected_on_encode`
    - `codec_audio_missing_samplerate_rejected_on_encode`
    - `codec_audio_missing_channel_config_rejected_on_encode`
    - `registry_name_only_video_codec_missing_bitrate_rejected_on_encode`
    - `raw_track_without_codec_accepted_on_encode`
  - 既存テストの更新: `video_role_missing_codec_rejected` / `video_role_missing_bitrate_rejected` / `audio_role_missing_samplerate_rejected` / `media_role_missing_fields_rejected_on_encode` にエラー文言の検証を追加し、`missing_role_does_not_require_media_fields` の doc コメントを更新
- `tests/test_msf/delta_apply.rs`
  - `apply_delta_add_codec_audio_without_samplerate_rejected`
  - `apply_delta_clone_added_audio_codec_without_samplerate_rejected`
  - `video_track` ヘルパーと継承 namespace の再追加テストに bitrate を持たせ、codec 判定で属性変更の検証が別理由で落ちないようにした
- `examples/moqt-publisher/src/catalog.rs`
  - `publisher_catalog_tracks_encode`
  - `publisher_alternate_video_codecs_encode`
  - `empty_catalog_is_rejected`

`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `cargo test -p pbt` が通ることを確認した。
