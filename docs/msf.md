# MOQT Streaming Format (MSF)

[draft-ietf-moq-msf-02](https://datatracker.ietf.org/doc/html/draft-ietf-moq-msf-02) の実装状況です。

## Catalog

- Catalog (`MsfCatalog`)
  - version / generatedAt / isComplete / tracks / publishTracks / initDataList
  - `removed_tracks`：削除済みトラックの履歴 (JSON には出力しない内部状態。delta update の適用時に isLive の false→true 検査へ使う。削除のたびに最新値で上書きする。キーは解決できる場合は解決済みの `(TrackNamespace, Track Name のバイト列)`、基準 namespace が不明な場合は `None` を含む `(Option<TrackNamespace>, Track Name のバイト列)`)
- Delta Update (`MsfDeltaUpdate`)
  - deltaUpdate (operation object の配列：`op` = "add" / "remove" / "clone" / "update") / generatedAt (必須)。delta update は他の Root Catalog フィールド (`version` / `isComplete` / `tracks` / `publishTracks` / `initDataList`) を持たず、未知フィールドは無視する
  - Full / Delta の判別は `MsfCatalogDocument` が行う
  - Catalog の encode / decode は `MsfCompression` を引数に取る。`Gzip` のときだけ gzip 圧縮・展開し、`None` では magic byte があっても自動展開しない (§5.5 / §12.1.1)
  - `crates/moq-sub` の `CatalogObject::decode_with_compression` は、購読の Track Property から解決した `MsfCompression` を catalog Object のデコードへ渡す
- Delta 適用 (`MsfCatalog::apply_delta`)
  - clone の継承解決 (`MsfCloneTrack::into_track`) と add / remove / clone / update の順次適用
  - 途中で失敗した場合は適用済みの変更を逆順に巻き戻し、呼び出し前のカタログ (tracks / removedTracks / generatedAt) を保つ
  - update は `name` = `parentName` を必須とし、`parentNamespace` 省略時は catalog namespace を使う。名前 / namespace の変更と isLive の false→true を拒否し、マージ後の track を通常の MUST で検証する (§5.1.6)。§5.6.4 の name 省略例は非規範として扱う
  - clone 以外の track は、namespace を省略すると catalog track の MOQT Track Namespace を継承する。clone は clone 固有の規則 (§5.1.6) により、namespace を明示すればそれを使い、省略すれば親トラックの namespace を継承する。clone の parentNamespace を省略した場合は catalog namespace を親の namespace と仮定する (§5.2.35)。名前の比較はカタログの `/` 区切り文字列ではなく、§11.2 の写像後のバイト値で行う
  - add するトラックは適用時点で §5.2 各フィールドの MUST を検証し、add 操作内の全トラックを検証してから追加する
  - 適用後の Full Track Name の一意性と、宣言された targetLatency / buffers のグループ内一致を再検証する (省略は player の裁量のため比較対象外、isLive=false は無視)
  - 削除済みの同じ Full Track Name の再追加は属性変更を許可し、isLive の false から true への変更だけを拒否する (§5.2.7)。v02 は v01 の属性不変 MUST を削除した
- カタログトラック名 (`MSF_CATALOG_TRACK_NAME` = "catalog")
- Track (`MsfTrack`)：Codec / Video / Audio は draft 上の論理的な分類で、実体は単一の `MsfTrack` のフィールド
  - 共通：name / namespace / packaging / packagingMetadata / eventType / role / isLive / label / lang / targetLatency / buffers / trackDuration / renderGroup / altGroup / initRef / depends / parentName / template / maxGopDuration / maxGroupDuration
  - `packagingMetadata` は `;` 区切りの `key=value`。key / value の whitespace・`=`・`;`、空要素、空の key / value を拒否する (§5.2.5)。`packaging_metadata_value(key)` で最初の出現を取得する
  - `streamMapping` は `group` / `object` を表し、`MsfTrack::stream_mapping()` で解釈する (§4.1.1)。キーがない場合は `group`。moq-pub は `--stream-mapping` で選択し、カタログの `packagingMetadata` に明示する。object モードは 1 Object を 1 Subgroup に載せ、subscriber は Group 内の複数 Subgroup を Object ID 順に並べ替えて同じ復号器へ流す
  - `keyframeSpacing` は keyframe 間隔 (フレーム数) を保持する (§5.2.46)。multi-variant switching の publisher 検証 API は `crates/moq-pub/src/variant.rs` にあり、全 variant の keyframeSpacing、共有開始 Group ID、Group ID の +1、Group 先頭のキーフレームと 1 Group 1 キーフレームを検証する。Object 境界の time-alignment は複数 variant encoder が未実装のため対象外である
  - `accessibleWindowTime` は DVR ウィンドウを wallclock ms で表す (§5.2.45)。同じ altGroup の track は、すべて値を持たないか、すべて同じ値を持つこと。isLive による例外はない。timeline の trim と subscriber の取得制約は timeline 経路の実装に依存するため未実装である
  - `depends` は track name (JSON String) または renderGroup (JSON Number) の配列 (§5.2.15)。String は依存元以外の track にちょうど 1 つ、Number はカタログ内の renderGroup に一致しなければならず、未解決・同名複数・自己参照を拒否する。公開型は `MsfDependency` で、`Vec<String>` からの破壊的変更である
  - `name` は空文字列と NUL を拒否する (§5.2.3 / §11.2.1)。`depends` の String 要素、`parentName`、`parentNamespace` も同じ写像規則で検証する
  - `namespace` は draft-ietf-moq-msf-02 §5.2.2 の `/` 区切り文字列。各 field は UTF-8 で、空文字列・空 field・NUL・32 fields 超・4096 バイト超・先頭や末尾や連続する `/` を拒否する
  - namespace と name の合計 (Full Track Name) は 4,096 バイト以下でなければならない (draft-ietf-moq-transport-23 §8.7)。namespace が省略され継承先が不明な場合は name 単体だけを検証する
  - Codec：codec / mimeType
  - Video：width / height / displayWidth / displayHeight / framerate / timescale / bitrate / avgBitrate / temporalId / spatialId
  - Audio：samplerate / channelConfig
  - 保護 / アクセシビリティ：connectionURI / encryptionScheme / cipherSuite / keyId / trackBaseKey / authInfo / accessibility
    - `connectionURI` は `msf::uri::validate_moqt_endpoint_uri` で MOQT endpoint URI として検証する
      (§5.2.37 / MOQT §6.1)。指定 URI への新規接続は transport / example 層の責務である
- 名前の変換 API
  - `parse_catalog_namespace`：`/` 区切りのカタログ namespace を `TrackNamespace` に写像する
  - `serialize_catalog_namespace`：`TrackNamespace` を `/` 区切りのカタログ namespace に写像する (UTF-8 でない field・NUL・`/` を含む field は表現できない)
  - `catalog_track_name_bytes`：カタログの track name を MOQT のバイト列に写像する
  - `MsfCatalog::validate_track_names`：基準 namespace を渡して Full Track Name の一意性と 4,096 バイト制限を検証する
  - `MsfCatalogTracker::observe`：独立した Full catalog を順に観測し、`isComplete` の削除
    (§5.1.3) と `isLive` の False → True (§5.2.8) を拒否する
- 関連型
  - clone 用 `MsfCloneTrack` (parentNamespace / packagingMetadata の継承に対応)
  - media timeline は depends 必須で、renderGroup 内の全 track が対象なら renderGroup を参照し個別名を併記しない (§7.2)
  - event timeline の depends は index が MediaPtsMs を参照する場合だけ必須で、`validate_event_timeline_depends(catalog, timeline)` で検証する (§8.3)
  - 両 timeline とも mimeType の値を要求しない。renderGroup の一部だけが対象という判定は catalog からできないため publisher の責務である
  - 削除用 `MsfRemoveTrack`
  - `MsfInitData` / `MsfBuffers` / `MsfPackaging` (loc / mediatimeline / eventtimeline / moqlog / moqmetrics / catalog)
- multi-variant switching: `crates/moq-sub/src/track_switch.rs` の `select_initial_variant` が帯域と keyframeSpacing から初期購読先を選び、`switch_target` が `(G - G0) * S + N` の switch point 判定と target Group ID を返す。`TrackSwitchState::switch_to` は旧購読の解除、target の Object 0 からの受信、decoder 再初期化を含む action を返す (§6.3)
- 優先度: カタログの publisher priority は 64、カタログ購読の SUBSCRIBER_PRIORITY は 64 とし、media の既定値 128 より高くする (Appendix A.1.3 の SHOULD)
- 検証規則
  - targetLatency / buffers のグループ内一致 (宣言値のみ。isLive=false は無視)
  - isComplete=false 禁止
  - timeline の depends / mimeType 必須
  - initRef の initDataList 参照整合性
  - initDataList の type は `inline` / `track-property` / `object-property`。property 型の data は `0x` + 1〜16 桁の hex (2^62 - 1 以下) で、inline の Base64 検証は 0342 が担当する。initRef の type と property scope の整合は catalog から判定できないため検証しない
  - codec が audio / video と判定できるトラック、または mimeType が `audio/` / `video/` を示すトラックは bitrate 必須 (audio は samplerate / channelConfig も)
  - codec の判定は WEBCODECS-CODEC-REGISTRY (Registry Draft, 2026-02-12) §3 / §4 の登録表記に基づく
  - mimeType の判定は RFC 6838 §4.2 に従い大文字小文字を区別せず、`audio/` / `video/` の接頭辞で行う。登録外 codec でも mimeType が種別を示せば MUST を課し、codec も必須とする
  - role (`video` / `audio` / `audiodescription`) は codec が無い場合も従来どおり codec / bitrate (audio は samplerate / channelConfig) を要求する
  - role `signlanguage` は §5.2.7 Table 7 の visual track のため video として扱い、codec / bitrate を要求する
  - codec / role / mimeType の判定が食い違う場合は両方の要求を満たす
  - codec なし・登録外 codec は mimeType が種別を示さない限り codec に基づく要求を行わない
  - `moq-secure-objects` の cipherSuite は `aes-128-gcm-sha256` / `aes-256-gcm-sha512` / `aes-128-ctr-hmac-sha256-80` のみ
  - authInfo の scheme は重複禁止
  - encode 時は version が `draft-02` であり、framerate が有限値であること
  - `initDataList` の `inline` data と `trackBaseKey` は Base64 (RFC 4648 §4) であること
  - lang は RFC 5646 §2.1 の Language-Tag ABNF 全体で検証し、IANA Language Subtag Registry との照合は行わない (RFC 5646 §2.1 の grandfathered タグを含む。`irregular` は固定リスト、`regular` は langtag 規則で受理する)

## Timeline

- Media Timeline (`[[pts_ms, [group_id, object_id], wallclock_ms], ...]` 形式)。pts_ms は i64 の整数ミリ秒 (負値を許す) で非降順、`validate_media_timeline_format` が template 属性との併用を拒否する。`2002.0` / `2.002e3` のような整数値表現は受理し、`2002.5` は拒否する。encode は整数リテラルに正規化する
- Event Timeline (`{T / L / M + data}` 形式、`data` は任意の JSON 値の生バイト列)。header data は MSF_EVENT_TIMELINE_HEADER (0x7B) Track property で運び、track の寿命を通じて不変である。`TrackProperties::event_timeline_header` で取得し、`MsfEventTimeline::decode_with_header` は header を解釈せず entry 列だけを decode する
- Gzip 圧縮と signal に基づく展開 (`msf::MsfCompression` / `encode_media_timeline` 等)
  - Timeline の encode / decode 関数は `MsfCompression` を引数に取る。これは MSF_COMPRESSION property (0x78) の値であり、`Gzip` のときだけ gzip 圧縮・展開する
  - property が無い場合 (`MsfCompression::None`) は payload を非圧縮として扱い、gzip の magic byte があっても自動展開しない (draft-ietf-moq-msf-02 §12.1.1)

## URI

- MSF URI / fragment のパース (`msf::uri`)
  - `parse_msf_uri` / `parse_msf_fragment`
  - authority / path / query は RFC 3986 の構文で検証し、authority の userinfo は拒否する
  - track-identifier の Track Name は 1 バイト以上を必須とする (draft-ietf-moq-transport-23 §2.4.1)
  - 予約パラメータ (`connection` / `wallclock-range` / `mediatime-range` / `location-range` / `cat`) の値型アクセサ
    - `wallclock-range` / `mediatime-range` / `location-range` は
      `wallclock_range_union` / `mediatime_range_union` / `location_range_union` で
      同一 URL 内の複数範囲を union できる (§11.1.1)
- カタログの変数置換 (`msf::resolve_catalog_variables`)
  - JSON の文字列リテラルの内側 (catalog field value) だけを置換する
  - 文字列の外・オブジェクトのキー・JSON エスケープの `\u0025` に現れる `%` は拒否する (§5.4.1)

## 未対応

- §5.5 / §12.1 (MSF_COMPRESSION property signaling)：-01 では Track Property ID が TBD、Object Property のレジストリ登録も未定義のため wire 実装を行わない (Timeline の圧縮は gzip magic byte 検出による従来動作のまま)
- §9 / §10 (Log / Metrics track)：packaging 値のみで、payload は MOQLOG / MOQMETRICS 側の定義で未実装
- §4.3 (Content protection)：catalog の signaling は扱うが、Secure Objects による暗号化と復号は未実装
