# MSF の必須フィールド検証が登録外 codec と mimeType を覆えていない

- Created: 2026-09-23
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-codec-detection-coverage
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-01 §5.2.18 (Codec) / §5.2.22 (Maximum Bitrate) / §5.2.28 (Audio sample rate) / §5.2.29 (Channel configuration) の MUST は「inherent codec を持つトラック」に課される。

しかし現状の検証は WEBCODECS-CODEC-REGISTRY の登録名に一致する codec 文字列だけを種別判定に使い、登録外の codec や mimeType しか手掛かりがないトラックを素通りさせている。MUST 違反のカタログを encode / decode してしまう穴を塞ぐ。

## 現状

`src/msf.rs` の `is_audio_codec` / `is_video_codec` は、固定の登録名リストとの完全一致と、`*` 付き登録名の区切り文字境界付き前方一致だけで判定する。`validate_media_track_fields` はこの 2 関数の結果と `role` だけから `require_audio` / `require_video` を決めており、`MsfTrack::mime_type` は判定に使っていない。

そのため次のカタログは MUST 違反のまま受理される。

- `codec` が `ac-3` / `ec-3` / `alac` / `aac` / `mp4v` などレジストリ未登録の audio / video codec で、bitrate / samplerate / channelConfig を欠くトラック (role なし)
- `mimeType` が `video/mp4` / `audio/mp4` などで、`codec` を持たず bitrate などを欠くトラック (role なし)

`pbt/tests/prop_msf.rs` のトラック生成器は codec を生成しないため、この穴は PBT でも検出されない。

逆に、レジストリ登録の有無だけで判定する現行規則は、`h264` / `vp9` / 短縮名などの実在 codec も判定外にしている。どこまでを「inherent codec を持つ」とみなすかは設計判断が必要である。

## 設計方針

判定根拠を 2 段階に分け、既存の登録名判定を後退させずに覆う範囲を広げる。

- 第一段: 現行どおり WEBCODECS-CODEC-REGISTRY の登録表記に一致する codec を audio / video と判定する
- 第二段: codec が登録外でも `mimeType` が `audio/` または `video/` で始まるトラックは、その種別として MUST を課す
- codec も mimeType も無いトラックは従来どおり raw data / event stream として扱い、codec に基づく要求を行わない
- role による判定 (`video` / `signlanguage` / `audio` / `audiodescription`) は維持し、codec / mimeType による判定とは両方の要求を重ねる
- 登録外 codec を無条件に audio / video とみなす案は、raw data トラックに任意の文字列 codec を付けたカタログを誤って拒否するため採らない。`mimeType` を補助根拠にするに留める
- mimeType の判定も大文字小文字を区別せず `audio/` / `video/` の接頭辞で行うかは実装時に決めるが、判定は「より厳しく拒否する」方向にのみ働かせる

## 完了条件

- `mimeType` が `audio/` / `video/` で codec を持たないトラックが bitrate (audio は samplerate / channelConfig も) を欠く場合に `InvalidCatalog` になるテストが `tests/test_msf/error_cases.rs` に追加されていること
- `mimeType` と必要なフィールドを揃えたトラックが受理されるテストが追加されていること
- 登録外 codec だけを持つ raw data トラック (mimeType なし) が codec に基づく要求を受けないテストが維持されていること
- mimeType 由来の要求でもエラーメッセージに要求の根拠が含まれること
- role と mimeType の判定が食い違う場合に両方の要求を満たす必要があるテストが追加されていること
- `make test` / `make clippy` / `make fmt` が通ること
