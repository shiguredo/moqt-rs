# MSF の必須フィールド検証が登録外 codec と mimeType を覆えていない

- Created: 2026-09-23
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-codec-detection-coverage
- Polished: 2026-10-08
- Updated: 2026-10-08

## 目的

draft-ietf-moq-msf-01 の §5.2.18 (Codec) は inherent codec を持つトラックに codec を、§5.2.22 (Maximum Bitrate) は audio / video トラックに bitrate を、§5.2.28 (Audio sample rate) / §5.2.29 (Channel configuration) は audio codec が指定されたトラックに samplerate / channelConfig を要求する。

しかし現状の検証は WEBCODECS-CODEC-REGISTRY の登録名に一致する codec 文字列だけを種別判定に使い、`mimeType` を判定に使っていない。そのため登録外 codec と併せて、あるいは単独で `mimeType` が種別を示すトラックを素通りさせ、MUST 違反のカタログを encode / decode してしまう。`mimeType` を補助根拠に取り込んで判定し、MUST 違反を塞ぐ。

## 現状

`src/msf.rs` の `is_audio_codec` / `is_video_codec` は、固定の登録名リストとの完全一致と、`*` 付き登録名の区切り文字境界付き前方一致だけで判定する。`validate_media_track_fields` はこの 2 関数の結果と `role` だけから `require_audio` / `require_video` を決めており、`MsfTrack::mime_type` は判定に使っていない。

`validate_media_track_fields` は種別が audio / video と決まった後、bitrate / samplerate / channelConfig の検査より先に `track.codec.is_none()` を検査し、codec が無ければ `MUST specify codec` で拒否する。この分岐は現行では role を指定したトラックだけが到達するが、mimeType を種別判定に加えると codec を持たないトラックもここで拒否され、bitrate / samplerate / channelConfig の検査には到達しない。

そのため次のカタログは MUST 違反のまま受理される。

- `codec` が `ac-3` / `ec-3` / `alac` / `aac` / `mp4v` などレジストリ未登録の audio / video codec で、`mimeType` も `audio/` / `video/` と種別を示すトラック (role なし) が、bitrate / samplerate / channelConfig を欠く
- `mimeType` が `video/mp4` / `audio/mp4` などで、`codec` を持たず bitrate などを欠くトラック (role なし)

`pbt/tests/prop_msf.rs` のトラック生成器は codec を生成しないため、この穴は PBT でも検出されない。

一方、登録外 codec だけを手掛かりとするトラック (mimeType なし・role なし) は、実在 codec の表記と raw data トラックが任意に付けた文字列を区別できない。レジストリ登録の有無だけで判定する現行規則は `h264` / `vp9` / 短縮名などの実在 codec も判定外にしているが、無条件に audio / video とみなすと raw data トラックを誤って拒否する。どこまでを「inherent codec を持つ」とみなすかは設計判断であり、本 issue では mimeType を補助根拠にする。

## 設計方針

判定根拠を 2 段階に分け、既存の登録名判定を後退させずに覆う範囲を広げる。

- 第一段: 現行どおり WEBCODECS-CODEC-REGISTRY の登録表記に一致する codec を audio / video と判定する
- 第二段: codec が登録外または欠如でも、`mimeType` が `audio/` または `video/` で始まるトラックはその種別として MUST を課す
- `mimeType` が `audio/` / `video/` を示すトラックは inherent codec を持つトラック (draft-ietf-moq-msf-01 §5.2.18) とみなし、codec も必須とする。codec を持たないトラックは既存の `MUST specify codec` の分岐で拒否し、この分岐のメッセージも mimeType を根拠として示す
- 種別判定は codec の登録名一致と mimeType の接頭辞一致を OR で合成する。audio を示す根拠が 1 つ以上あれば audio の要求を、video を示す根拠が 1 つ以上あれば video の要求を課し、食い違う場合は両方の要求を重ねる (例: codec `opus` + mimeType `video/mp4` は bitrate と samplerate / channelConfig を要求する)
- mimeType の判定は draft-ietf-moq-msf-01 §5.2.19 (Mimetype) が参照する RFC 6838 §4.2 のとおり大文字小文字を区別せず `audio/` / `video/` の接頭辞で行い、判定は「より厳しく拒否する」方向にのみ働かせる (要求を減らさない)
- エラーメッセージの根拠は role / codec / mimeType のうち該当するものを列挙する (例: `mimeType 'audio/mp4'`)。`requirement_source` は role / codec の 2 値しか選べないため、根拠種別を列挙できる形に変える
- timeline の `mimeType` が `application/json` であることの完全一致検証は、本 issue の種別判定の変更対象に含めない (timeline の mimeType 検証は現行の完全一致のままとする)
- codec も mimeType も種別を示さないトラック (codec なし・登録外 codec で mimeType が `audio/` / `video/` でない) は従来どおり raw data / event stream として扱い、codec に基づく要求を行わない
- role による判定 (`video` / `signlanguage` / `audio` / `audiodescription`) は維持し、codec / mimeType による判定とは両方の要求を重ねる
- 登録外 codec を無条件に audio / video とみなす案は、raw data トラックに任意の文字列 codec を付けたカタログを誤って拒否するため採らない。`mimeType` を補助根拠にするに留める。登録外 codec のみ (mimeType なし・role なし) のトラックは本 issue の後も判定できず、codec に基づく要求を行わない (設計上の制約として明示する)

## 完了条件

- `mimeType` が `audio/` / `video/` で codec を持たないトラックが、codec 欠如
  (draft-ietf-moq-msf-01 §5.2.18 (Codec)) を根拠に `InvalidCatalog` になるテストが
  `tests/test_msf/error_cases.rs` に追加されていること (codec を持たないため bitrate / samplerate /
  channelConfig の検査には到達しない)。`issues/0144` (error_cases.rs の分割) を先行させた場合は、
  分割後の codec / role テーマのファイルへ読み替える
- 登録外 codec と `audio/` / `video/` の mimeType の組み合わせで、その種別の必須フィールドを欠くトラックが `InvalidCatalog` になるテストが追加されていること (例: codec `ac-3` + mimeType `audio/mp4` で bitrate は与え、samplerate のみを欠く)
- `mimeType` と、その種別に必要なフィールド (codec を含む) を揃えたトラックが受理されるテストが追加されていること
- 登録外 codec だけを持つ raw data トラック (mimeType なし) が codec に基づく要求を受けないテストが維持されていること
- encode 経路 (`MsfCatalog` を手組みして `MsfCatalogDocument::encode`) でも同じ拒否になるテストが追加されていること
- mimeType 由来の要求でもエラーメッセージに要求の根拠として `mimeType '<値>'` が含まれること (codec が欠如または登録外で、mimeType だけが種別の根拠になるトラックで固定する。codec 欠如時の `MUST specify codec` も mimeType を根拠として示す)
- role と mimeType の判定が食い違う場合に両方の要求を満たす必要があるテストが追加されていること (例: role `video` + codec `av01` + mimeType `audio/mp4` で bitrate を与え、samplerate / channelConfig を欠けば mimeType 側の audio 要求で拒否され、両方を揃えれば受理される。role 側が mimeType 側の要求を包摂する向きでは mimeType の寄与を判別できないため使わない)
- `docs/msf.md` の検証規則の記述が mimeType による判定と登録外 codec の扱いに追随していること
- `CHANGES.md` の `## develop` にある media track の必須フィールド検証の FIX エントリが、mimeType を補助根拠に加えたこととエラーメッセージの根拠表示の変更に追随していること (同一 develop 内の未リリース変更のため既存エントリを更新する)
- `make test` / `make clippy` / `make fmt` が通ること
