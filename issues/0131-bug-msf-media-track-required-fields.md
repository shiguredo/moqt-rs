# codec / bitrate / samplerate / channelConfig の MUST 検証が role 依存になっている

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-media-track-required-fields
- Polished: {YYYY-MM-DD}

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

`validate_media_track_fields` は `validate_full_track` (encode と delta の add / clone) と `decode_track` から呼ばれるため、encode / decode の両経路で同じ抜けがある。`pbt/tests/prop_msf.rs` のトラック生成器は codec を生成しないため、この抜けの影響を受けていない。

## 設計方針

role に依存せず、codec 文字列から audio / video を判定して MUST を検証する。判定は [WEBCODECS-CODEC-REGISTRY](https://www.w3.org/TR/webcodecs-codec-registry/) の codec string を前方一致で照合する。`examples/moqt-subscriber/src/pipeline.rs` が `c.starts_with("av01")` などでトラック種別を判定しているのと同じ方式にする。

| 判定 | codec string |
| --- | --- |
| audio | `flac` / `mp3` / `mp4a.` / `opus` / `vorbis` / `ulaw` / `alaw` / `pcm-` |
| video | `av01.` / `avc1.` / `avc3.` / `hev1.` / `hvc1.` / `vp8` / `vp09.` |

- audio と判定したトラックに bitrate / samplerate / channelConfig を、video と判定したトラックに bitrate を要求する
- codec が無いトラックと、上記のどれにも一致しない codec のトラックは codec から種別を判定できないため、codec に基づく要求は行わない
- role がある場合の検証は維持する。role が `video` / `audio` / `audiodescription` なら、codec が無い場合も含めて従来どおり codec / bitrate (audio 系なら samplerate / channelConfig) を要求する
- role による判定と codec による判定が食い違う場合は、両方の要求を重ねて適用する
- codec が無いトラックを「inherent codec を持たない raw data / event stream」として扱うのは §5.2.18 の "It is not required for raw data tracks or event streams." と整合する

`examples/moqt-publisher/src/catalog.rs` は role を設定せず、video に codec `av01...` と bitrate、audio に codec `opus` と samplerate / channelConfig / bitrate を設定する。いずれも上記の判定に一致するため、生成するカタログは引き続き encode に成功する。

判定表は上記レジストリの Audio Codec Registry / Video Codec Registry の登録内容に合わせて拡張する。`refs/` には同レジストリの写しが無いため、実装時に登録内容を確認する。

## 完了条件

- role 省略 + codec `opus` で samplerate を欠くカタログを `MsfCatalogDocument::decode` が拒否するテストが `tests/test_msf/error_cases.rs` に追加されていること
- role 省略 + codec `opus` で channelConfig を欠くカタログを拒否するテストが追加されていること
- role 省略 + codec `opus` で bitrate を欠くカタログを拒否するテストが追加されていること
- role 省略 + codec `av01.0.08M.08` で bitrate を欠くカタログを拒否するテストが追加されていること
- role 省略 + codec `opus` で bitrate / samplerate / channelConfig を揃えたカタログを受理するテストが追加されていること
- encode 経路 (`MsfCatalog` を手組みして `MsfCatalogDocument::encode`) でも同じ拒否になるテストが追加されていること
- codec を持たないトラック (raw data / event stream 相当) が codec / bitrate / samplerate / channelConfig を要求されないテストが維持されていること (`missing_role_does_not_require_media_fields` の doc コメントは新しい判定規則に合わせて更新する)
- role が `video` / `audio` のときの既存検証 (`video_role_missing_codec_rejected` / `video_role_missing_bitrate_rejected` / `audio_role_missing_samplerate_rejected` / `media_role_with_required_fields_accepted`) が維持されていること
- `examples/moqt-publisher/src/catalog.rs` が生成する構成 (role なし / video: codec `av01...` と bitrate / audio: codec `opus` と samplerate と channelConfig と bitrate) のカタログが `encode` に成功すること
- `make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ること
