# moq-sub の codec 種別判定を library の判定に寄せる

- Created: 2026-09-23
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-subscriber-codec-detection
- Polished: 2026-11-14
- Updated: 2026-11-14

## 目的

`shiguredo_moqt` は codec 文字列から audio / video を判定する規則を `src/msf.rs` に持つが、
`crates/moq-sub/src/pipeline.rs` の `receive_catalog` は video track の選択に独自の codec 接頭辞表を使い、
audio track の選択は `decoder::is_supported_audio_codec` のデコード可否だけで行っている。
このため、library が video / audio と判定しない登録外の接頭辞 codec (`av01x` / `opusx` など) を
example が選択し得る。audio / video の種別判定規則を library に一本化し、
example 側では種別判定とデコード可否判定を分ける。

## 現状

`src/msf.rs` の `is_audio_codec` / `is_video_codec` は、WEBCODECS-CODEC-REGISTRY の登録名の
完全一致と区切り文字境界付き前方一致で audio / video を判定する。対象は `av01` / `avc1` / `avc3` /
`hev1` / `hvc1` / `vp8` / `vp09` と、`flac` / `mp3` / `opus` / `vorbis` / `ulaw` / `alaw` / `mp4a.*` /
`pcm-*` を含む。両関数は private であり、example から呼べる公開 API は無い。

`crates/moq-sub/src/pipeline.rs` の `receive_catalog` は、`av01` / `avc1` / `hvc1` / `hev1` の
前方一致、`vp8` / `vp09` の完全一致、または `vp09.` 前方一致を満たす最初のトラックを video として選ぶ。
この表は example のデコード可否表と library の種別判定表を兼ねており、library が video と判定しない
`av01x` / `avc1x` / `hvc1x` / `hev1x` も video として選ぶ。`vp8` / `vp09` は library でも video であり、
現在の選択対象に含まれている。

`receive_catalog` の audio track の選択は `decoder::is_supported_audio_codec` だけで行い、`opus` 始まりと `mp4a` の完全一致または `mp4a.` 始まりを選択する。`decoder::is_supported_audio_codec` は `decoder.rs` の `is_opus_codec` / `is_aac_codec` を使うデコード可否判定であり、library が audio と判定しない `opusx` / `opus.2` も選択する。

`crates/moq-sub/src/pipeline.rs` の `handle_stream_body` は `decoder::is_opus_codec`
(`starts_with("opus")`) を OpusHead パースの要否判定に使う。
`crates/moq-sub/src/decoder.rs` の `build_video_decoder` は `av01` / `avc1` / `hvc1` / `hev1` の
前方一致と `vp8` / `vp09` でデコーダを選び、`AudioDecoder::build` は Opus と `mp4a.*`
(AAC、macOS のみ) を選ぶ。`crates/moq-sub/src/mp4.rs` の `build_video_entry` は
`av01` / `avc1` / `hvc1` / `hev1` のみを扱う。これらは codec 実装の選択であり、
audio / video の種別判定ではない。

`receive_catalog` の `no usable tracks found in catalog (expected av01/avc1/hvc1/hev1/opus/mp4a)` は、対応する video codec として `vp8` / `vp09` を列挙していない。

## 設計方針

audio / video の種別判定を library に一本化し、example 側に種別判定表を複製しない。種別判定とデコード可否判定は分ける。

- `shiguredo_moqt` に codec 種別を返す公開 API を追加する。codec 文字列だけを入力とし、
  `CodecKind` (`Audio` / `Video` / `Unknown`) を返す `codec_kind` を `src/msf.rs` に追加する。
  Sans I/O の性質は変えない。mimeType を加えた種別判定 (0146) は本 API に含めない
- `codec_kind` は `src/msf.rs` の `is_audio_codec` / `is_video_codec` の判定表を唯一の実装として使い、
  判定表を二重管理しない。`is_audio_codec` / `is_video_codec` は private のまま
- `crates/moq-sub` は公開 API で audio / video の種別を判定し、`receive_catalog` 内の video codec 接頭辞表を削除する。audio track の選択も公開 API の種別判定を通す
- 種別判定とデコード可否判定を分ける。video は library が video と判定し、かつ `decoder.rs` が対応する codec だけを選ぶ。audio は library が audio と判定し、かつ `decoder::is_supported_audio_codec` が真の codec だけを選ぶ。`decoder.rs` に `is_supported_video_codec` を追加し、`build_video_decoder` と同じ codec 対応表を共有する (codec 文字列の対応表であり、種別判定表ではない)
- `decoder.rs` の codec 分岐 (`starts_with`)、`mp4.rs` の `build_video_entry` の codec 分岐、OpusHead パースの要否判定 (`decoder::is_opus_codec`) は、種別判定ではなく codec 実装の選択であるため残す
- `crates/moq-pub` の codec 文字列は変更しない
- 挙動が変わる点を明確にする。`av01` / `avc1` / `hvc1` / `hev1` / `vp8` / `vp09` と
  `opus` / `mp4a.*` の選択結果は従来と同じ。library が種別を判定するが example がデコードできない
  `avc3` や `flac` などは従来どおり選択しない。library が種別を判定しない登録外の接頭辞 codec
  (`av01x` / `avc1x` / `hvc1x` / `hev1x` / `opusx` / `opus.2`) は選択しなくなる。
  `av01.` / `mp4a.` のような区切り文字だけの codec は library の境界規則で一致するため
  従来どおり選択する
- デコード可否は codec 文字列の対応表で判定し、プラットフォームによるデコーダの可用性は従来どおり選択条件に含めない
- `receive_catalog` の `no usable tracks` エラーメッセージの対応 codec 列挙を、現在の選択対象 (`av01` / `avc1` / `hvc1` / `hev1` / `vp8` / `vp09` / `opus` / `mp4a`) に合わせる
- 0334 (moq-sub のカタログ解釈を MSF に合わせる) が先に実装された場合は、選択が `select_video_track` / `select_audio_track` に切り出されているため、その関数内の codec 判定を置き換える。未実装の場合は、テストのために `receive_catalog` から同じ純粋関数を切り出す
- MP4 録画は `mp4.rs` の `build_video_entry` が `av01` / `avc1` / `hvc1` / `hev1` のみ、`build_audio_entry` が Opus のみに対応する。本 issue の選択は録画可否ではなくデコード可否で行い、VP8 / VP9 の MP4 録画対応は本 issue の範囲外とする

## 完了条件

- `shiguredo_moqt` に codec 種別の公開 API が追加され、`tests/test_msf/` のテストで固定されていること。テストは登録名の完全一致・区切り文字付きの前方一致・区切り文字の無い登録外 codec (`av01x` / `opusx` など) を公開 API に対して検証すること
- 追加した公開 API が `src/msf.rs` の `is_audio_codec` / `is_video_codec` と同じ結果を返し、判定表が二重管理されていないこと
- `crates/moq-sub/src/pipeline.rs` の `receive_catalog` (0334 または本 issue で選択を `select_video_track` / `select_audio_track` に切り出した場合はその関数) から codec 文字列の接頭辞表による種別判定が消え、library の公開 API を使うこと
- 種別判定の置き換え後、`av01` / `avc1` / `hvc1` / `hev1` / `vp8` / `vp09` と `opus` / `mp4a.*` のトラックを従来どおり選択し、library が種別を判定するがデコードできない `avc3` / `flac` などのトラックだけのカタログでは該当する video / audio トラックを選択しないこと
- library が種別を判定しない `av01x` / `avc1x` / `hvc1x` / `hev1x` / `opusx` / `opus.2` だけのカタログでは、該当する video / audio トラックを選択しないこと
- `receive_catalog` の `no usable tracks` エラーメッセージが現在の選択対象 codec を列挙していること
- `make test` / `make clippy` / `make fmt` が通ること
