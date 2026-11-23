# MSF カタログで CMSF-01 の cmaf packaging を扱う

- Created: 2026-11-23
- Completed: {YYYY-MM-DD}
- Branch: feature/add-cmsf-draft-01
- Polished: {YYYY-MM-DD}

## 目的

`refs/moq/draft-ietf-moq-cmsf-01.txt` (CMSF) は MSF を拡張し、CMAF でパッケージしたメディアを MSF のカタログ・タイムライン・ABR の枠組みで運ぶ方法を定める。現行実装は `"packaging": "cmaf"` を未知の値として拒否する。そのため、CMSF の publisher が出すカタログは、cmaf の track を 1 つでも含めば decode できない。

本 issue では、ライブラリ (`src/msf.rs`) のカタログで、cmaf packaging (CMSF §3.1 / §3.5) を扱えるようにする。CMSF 対応は次の issue に分けて進める。本 issue は 0468 を除く後続の issue の前提であり、`docs/msf.md` の CMSF の節も本 issue で新設する。

| issue | 内容 |
| --- | --- |
| 0467 (本 issue) | cmaf packaging |
| 0468 | `MsfTrack::stream_mapping` を loc に限る |
| 0469 | SAP Type timeline |
| 0470 | Content Protection |
| 0471 | moq-pub の cmaf 配信 |
| 0472 | moq-sub の cmaf 購読 |
| 0473 | prft による壁時計の同期 |
| 0474 | README / SKILL.md |

CMSF-01 が規範参照しているのは MSF-01 / MoQT-18 / LOC-02 である。一方、本リポジトリは MSF-02 / MoQT-23 / LOC-05 に準拠している (`README.md` の対応仕様、`CODEBASE.md` の「最新ドラフトに準拠すること」)。

そのため、CMSF-01 の記述をそのまま実装すると、MSF-02 と矛盾する箇所や古い節番号を参照する箇所が出る。そうした箇所では MSF-02 / MoQT-23 を優先する。判断は各 issue の「ずれ」の節に固定し、`docs/msf.md` の CMSF の節にも記載する。この方針は 0467〜0474 で共通である。

## 現状

- `src/msf.rs` の `MsfPackaging` は `Loc` / `MediaTimeline` / `EventTimeline` / `MoqLog` / `MoqMetrics` / `Catalog` しか持たない。`MsfPackaging::from_packaging_str` は `"cmaf"` を `unknown packaging value 'cmaf'` の `MessageError::InvalidCatalog` で拒否する。
- CMSF の track フィールド `maxGrpSapStartingType` / `maxObjSapStartingType` は、`MsfTrack` / `MsfCloneTrack` に存在しない。decode では未知フィールドとして無視され (MSF-02 §5 の MUST ignore)、encode では出力できない。
- `MsfCloneTrack` は clone / update の部分 track である。`packaging: Option<MsfPackaging>` (省略時は親から継承) や `depends: Option<Vec<MsfDependency>>` (`None` は親から継承) を持つ。`MsfUpdateTrack` も内部に `MsfCloneTrack` を持つ。継承の解決は `MsfCloneTrack::into_track` が行い、解決後の track を `validate_full_track` に通す。
- `validate_init_refs` は、`tracks` と `publishTracks` の initRef が、initDataList のどれかの id に一致することだけを検証する。参照先の `MsfInitDataKind` は見ない。`validate_init_data` は、`inline` の data が Base64 (`Base64::decode_vec`) として decode できることを検証する。
- `validate_media_track_fields` は、Table 6 (MSF-02 §5.2.4.1) の framerate / lang の要求を `MsfPackaging::Loc` に限って課す。role / codec / mimeType から導く bitrate / samplerate / channelConfig の要求は、packaging を問わず課す。
- コアの `shiguredo_moqt` は ISO BMFF に依存していない。`shiguredo_mp4` に依存するのは `crates/moq-pub` / `crates/moq-sub` だけである。

## CMSF-01 と MSF-02 / MoQT-23 のずれ

| 項目 | CMSF-01 | MSF-02 / MoQT-23 | 本実装の扱い |
| --- | --- | --- | --- |
| カタログの version | §5 の例は `"version": "1"` | §5.1.1: `"draft-XX"` 形式 | CMSF のカタログも `MSF_VERSION` (`"draft-02"`) を名乗る。§5 の例は非規範として扱う |
| MOQT Track の参照 | §3.2: `[MoQTransport] Sect 2.3` | MoQT-23 では §2.3 が Groups、§2.4 が Track | doc / コメントでは MoQT-23 §2.4 を参照する |
| Table 6 の行 | cmaf の行を定義しない | §5.2.4.1: 新しい packaging を定義する仕様が role ごとの行を追加する | Table 6 由来の追加必須フィールドは cmaf に課さない。role / codec / mimeType から導く条件付き必須 (bitrate など) はこれまでどおり課す。initRef は CMSF §3.1 の MUST として必須にする |

## 設計方針

- `MsfPackaging::Cmaf` を追加し、`from_packaging_str` / `as_str` の両方向で `"cmaf"` を扱う。
- CMSF §3.1: cmaf の track は initRef を必須とし、参照先の initDataList entry が `MsfInitDataKind::Inline` であることを要求する。
  - 既存の `validate_init_refs` と同じく、`tracks` と `publishTracks` の両方を対象にする。
  - decode / encode / delta 適用後 (`validate_after_delta`) のいずれでも検証する。
  - init segment の中身 (ftyp / moov) はコアでは検証しない。ISO BMFF に依存しないためである。
- CMSF §3.5.2: `MsfTrack` に `max_grp_sap_starting_type` / `max_obj_sap_starting_type` (`Option<u64>`) を追加する。
  - CMSF は値域を定めていない。`maxGrpSapStartingType` は、§3.4 の MUST (Group は SAP type 1 か 2 で始まる) から 1 または 2 に限る。
  - `maxObjSapStartingType` は 1 以上に限る。track の Object には必ず Group 先頭の Object (SAP type 1 か 2) が含まれるので、最大値は 1 を下回らない。
  - 上限は課さない。SAP type の値域を定める ISO/IEC 14496-12 は `refs/` にない。根拠のない上限を設けると、他の publisher のカタログを丸ごと拒否してしまう。CMSF §3.6.1 の「0,1,2 or 3」は SAP Type timeline の data (0469) の値域であり、このフィールドの値域ではない。
  - 両方あるときは `maxObjSapStartingType` ≥ `maxGrpSapStartingType` を要求する。Group 先頭の Object は track の Object の一部なので、Object 全体の最大値は Group 先頭の最大値を下回らない。
  - 範囲外と大小関係の違反は `InvalidCatalog` にする。
  - 検証する段階は次のとおりとする。
    - 値域と大小関係は、`decode_track` と `validate_full_track` で検証する。`validate_full_track` は、delta の add (適用時と encode 時)、`MsfCloneTrack::into_track` の結果、update のマージ結果、完全カタログの encode に適用される。そのため、継承を解決した後の track にも同じ検証がかかる。
    - clone / update の断片 (`decode_clone_track` / `validate_clone_track_fragment`) では、packaging が cmaf と明示されている場合だけ、存在する値の値域を検証する。
    - packaging を省略した断片では、値域を検証しない。親が cmaf 以外なら、値は継承の解決後に捨てられるためである。
    - 大小関係は、片方の値を親から継承し得るので、断片では検証しない。継承の解決後だけで判定する。
- 2 つのフィールドは cmaf 以外の track では意味を持たない (MSF-02 §5.2.4.1 の SHOULD NOT)。
  - `MsfTrack` の decode では、cmaf 以外の track の値を CMSF のフィールドとして解釈せず、未知フィールドとして捨てる。encode では、cmaf 以外の track に値があれば `InvalidCatalog` にする。こうすると decode → encode の往復が崩れない。
  - `MsfCloneTrack` にも同じフィールドを `Option` で追加する。`None` は親から継承する。既存の `depends` と同じ慣例である。
  - clone / update の decode では、packaging が明示されていて cmaf 以外のときだけ値を捨てる。packaging を省略した場合は、親が cmaf かどうかが分からないので値を保持する。encode では、packaging が明示されていて cmaf 以外のときに値があれば `InvalidCatalog` にする。
  - 継承を解決した後の track (`MsfCloneTrack::into_track` と update のマージ結果) が cmaf でなければ、継承した値も明示した値も捨てる。cmaf の親を `packaging: "loc"` で clone する delta を、継承した CMSF フィールドを理由に拒否しないためである。
  - CMSF のフィールドを cmaf 以外で捨てる・拒否する規則は、0470 の `contentProtectionRefIDs` でも同じ形で使う。共通の判定関数として実装する。
    - 共通にするのは、packaging によって捨てるか拒否するかの判定だけである。
    - 値そのものの検証と、それをどの段階で行うかは、フィールドごとに決める。0470 の空配列の拒否も、フィールドごとの検証にあたる。
- CMSF §3.2 の switching set (altGroup を共有し、Group 番号を media time で揃える) と、§3.3 / §3.4 の payload 規則は、ライブラリでは検証しない。カタログからは判定できないためである。
- `docs/msf.md` に CMSF の節を新設し、本 issue の「ずれ」の判断と、ライブラリが検証しない規則を記載する。後続の issue は、それぞれこの節に追記する。

## 完了条件

- `"packaging": "cmaf"` の track を含む完全カタログが、decode / encode で往復一致すること。CMSF-01 §5.1 の例も同様であること。ただし例は version を `"draft-02"` にし、`...` で省略された `data` を有効な Base64 に置き換えたものとする。
- cmaf の track で次のカタログが `MessageError::InvalidCatalog` になること。対象は `tracks` と `publishTracks` の両方である。
  - initRef がない
  - initRef が `inline` 以外の entry を指す
  - `maxGrpSapStartingType` が 1〜2 の外
  - `maxObjSapStartingType` が 0
  - `maxObjSapStartingType` < `maxGrpSapStartingType`
- delta update で追加・clone・update した cmaf の track に対しても、適用後のカタログで initRef の解決と `inline` であることが検証されること。
- cmaf 以外の track が持つ SAP starting type は、decode では捨てられ、encode では拒否されること。
- clone / update で次のとおりになること。
  - packaging を省略した clone / update は SAP starting type を保持し、cmaf の親から継承できること
  - cmaf の親を `packaging: "loc"` で clone した delta が受理され、結果の track から SAP starting type が落ちていること
  - 継承の解決後に大小関係が崩れる delta が `InvalidCatalog` になること。例は、`maxObjSapStartingType: 1` を持つ cmaf の親を、packaging を省略して `maxGrpSapStartingType: 2` で clone する場合である
  - packaging を cmaf と明示した clone / update の断片で、値域外の値 (例: `maxGrpSapStartingType: 3`) が decode / encode の時点で `InvalidCatalog` になること
  - packaging を省略した clone で値域外の値を持つ場合、次のとおりになること
    - 親が loc なら受理され、値が捨てられること
    - 親が cmaf なら、継承の解決後に `InvalidCatalog` になること
- 上記を `tests/test_msf` の単体テストで固定し、`pbt/tests/prop_msf.rs` の生成器に cmaf track を加えること。
- `docs/msf.md` に CMSF の節があること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
