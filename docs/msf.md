# MOQT Streaming Format (MSF)

[draft-ietf-moq-msf-01](https://datatracker.ietf.org/doc/html/draft-ietf-moq-msf-01) の実装状況です。

## Catalog

- Catalog (`MsfCatalog`)
  - version / generatedAt / isComplete / tracks / publishTracks / initDataList
  - `removed_tracks`：削除済みトラックの履歴 (JSON には出力しない内部状態。delta update の適用時に属性不変の検査へ使う。キーは解決できる場合は解決済みの `(TrackNamespace, Track Name のバイト列)`、基準 namespace が不明な場合は `None` を含む `(Option<TrackNamespace>, Track Name のバイト列)`)
- Delta Update (`MsfDeltaUpdate`)
  - deltaUpdate (operation object の配列：`op` = "add" / "remove" / "clone") / generatedAt
  - Full / Delta の判別は `MsfCatalogDocument` が行う
- Delta 適用 (`MsfCatalog::apply_delta`)
  - clone の継承解決 (`MsfCloneTrack::into_track`) と add / remove / clone の順次適用
  - clone 以外の track は、namespace を省略すると catalog track の MOQT Track Namespace を継承する。clone は clone 固有の規則 (§5.1.6) により、namespace を明示すればそれを使い、省略すれば親トラックの namespace を継承する。clone の parentNamespace を省略した場合は catalog namespace を親の namespace と仮定する (§5.2.35)。名前の比較はカタログの `/` 区切り文字列ではなく、§11.2 の写像後のバイト値で行う
  - add するトラックは適用時点で §5.2 各フィールドの MUST を検証し、add 操作内の全トラックを検証してから追加する
  - 適用後の Full Track Name の一意性と、宣言された targetLatency / buffers のグループ内一致を再検証する (省略は player の裁量のため比較対象外、isLive=false は無視)
  - 削除済みの同じ Full Track Name の再追加は、属性変更と isLive の false から true への変更を拒否する (§5.3 / §5.2.7)
- カタログトラック名 (`MSF_CATALOG_TRACK_NAME` = "catalog")
- Track (`MsfTrack`)：Codec / Video / Audio は draft 上の論理的な分類で、実体は単一の `MsfTrack` のフィールド
  - 共通：name / namespace / packaging / packagingMetadata / eventType / role / isLive / label / lang / targetLatency / buffers / trackDuration / renderGroup / altGroup / initRef / depends / parentName / template / maxGopDuration / maxGroupDuration
  - `packagingMetadata` は `;` 区切りの `key=value`。key / value の whitespace・`=`・`;`、空要素、空の key / value を拒否する (§5.2.5)。`packaging_metadata_value(key)` で最初の出現を取得する
  - `accessibleWindowTime` は DVR ウィンドウを wallclock ms で表す (§5.2.45)。同じ altGroup の track は、すべて値を持たないか、すべて同じ値を持つこと。isLive による例外はない。timeline の trim と subscriber の取得制約は timeline 経路の実装に依存するため未実装である
  - `depends` は track name (JSON String) または renderGroup (JSON Number) の配列 (§5.2.15)。String は依存元以外の track にちょうど 1 つ、Number はカタログ内の renderGroup に一致しなければならず、未解決・同名複数・自己参照を拒否する。公開型は `MsfDependency` で、`Vec<String>` からの破壊的変更である
  - `name` は空文字列と NUL を拒否する (§5.2.3 / §11.2.1)。`depends` の String 要素、`parentName`、`parentNamespace` も同じ写像規則で検証する
  - `namespace` は draft-ietf-moq-msf-02 §5.2.2 の `/` 区切り文字列。各 field は UTF-8 で、空文字列・空 field・NUL・32 fields 超・4096 バイト超・先頭や末尾や連続する `/` を拒否する
  - namespace と name の合計 (Full Track Name) は 4,096 バイト以下でなければならない (draft-ietf-moq-transport-23 §8.7)。namespace が省略され継承先が不明な場合は name 単体だけを検証する
  - Codec：codec / mimeType
  - Video：width / height / displayWidth / displayHeight / framerate / timescale / bitrate / avgBitrate / temporalId / spatialId
  - Audio：samplerate / channelConfig
  - 保護 / アクセシビリティ：connectionURI / encryptionScheme / cipherSuite / keyId / trackBaseKey / authInfo / accessibility
- 名前の変換 API
  - `parse_catalog_namespace`：`/` 区切りのカタログ namespace を `TrackNamespace` に写像する
  - `serialize_catalog_namespace`：`TrackNamespace` を `/` 区切りのカタログ namespace に写像する (UTF-8 でない field・NUL・`/` を含む field は表現できない)
  - `catalog_track_name_bytes`：カタログの track name を MOQT のバイト列に写像する
  - `MsfCatalog::validate_track_names`：基準 namespace を渡して Full Track Name の一意性と 4,096 バイト制限を検証する
- 関連型
  - clone 用 `MsfCloneTrack` (parentNamespace / packagingMetadata の継承に対応)
  - media timeline は depends 必須で、renderGroup 内の全 track が対象なら renderGroup を参照し個別名を併記しない (§7.2)
  - event timeline の depends は index が MediaPtsMs を参照する場合だけ必須で、`validate_event_timeline_depends(catalog, timeline)` で検証する (§8.3)
  - 両 timeline とも mimeType の値を要求しない。renderGroup の一部だけが対象という判定は catalog からできないため publisher の責務である
  - 削除用 `MsfRemoveTrack`
  - `MsfInitData` / `MsfBuffers` / `MsfPackaging` (loc / mediatimeline / eventtimeline / moqlog / moqmetrics / catalog)
- 検証規則
  - targetLatency / buffers のグループ内一致 (宣言値のみ。isLive=false は無視)
  - isComplete=false 禁止
  - timeline の depends / mimeType 必須
  - initRef の initDataList 参照整合性
  - codec が audio / video と判定できるトラックは bitrate 必須 (audio は samplerate / channelConfig も)
  - 判定は WEBCODECS-CODEC-REGISTRY (Registry Draft, 2026-02-12) §3 / §4 の登録表記に基づく
  - role (`video` / `audio` / `audiodescription`) は codec が無い場合も従来どおり codec / bitrate (audio は samplerate / channelConfig) を要求する
  - role `signlanguage` は §5.2.6 Table 4 の visual track のため video として扱い、codec / bitrate を要求する
  - codec と role の判定が食い違う場合は両方の要求を満たす
  - codec なし・登録外 codec は codec に基づく要求を行わない
  - lang の BCP 47 簡易検証 (RFC 5646 §2.1 の grandfathered タグを含む。`irregular` は固定リスト、`regular` は langtag 規則で受理する)

## Timeline

- Media Timeline (`[[pts_ms, [group_id, object_id], wallclock_ms], ...]` 形式)
- Event Timeline (`{t / l / m + data}` 形式、`data` は JSON object の生バイト列)
- Gzip 圧縮と自動展開 (`msf::TimelineEncodingOptions` / `encode_media_timeline` 等)
  - `TimelineEncodingOptions` と Timeline の encode / decode 関数は `msf` モジュール経由で利用する

## URI

- MSF URI / fragment のパース (`msf::uri`)
  - `parse_msf_uri` / `parse_msf_fragment`
  - 予約パラメータ (`connection` / `wallclock-range` / `mediatime-range` / `location-range` / `cat`) の値型アクセサ

## 未対応

- §5.5 / §12.1 (MSF_COMPRESSION property signaling)：-01 では Track Property ID が TBD、Object Property のレジストリ登録も未定義のため wire 実装を行わない (Timeline の圧縮は gzip magic byte 検出による従来動作のまま)
- §9 / §10 (Log / Metrics track)：packaging 値のみで、payload は MOQLOG / MOQMETRICS 側の定義で未実装
- §4.3 (Content protection)：catalog の signaling は扱うが、Secure Objects による暗号化と復号は未実装
