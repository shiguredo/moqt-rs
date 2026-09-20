# MOQT Streaming Format (MSF)

[draft-ietf-moq-msf-01](https://datatracker.ietf.org/doc/html/draft-ietf-moq-msf-01) の実装状況です。

## Catalog

- Catalog (`MsfCatalog`)
  - version / generatedAt / isComplete / tracks / publishTracks / initDataList
  - `removed_tracks`：削除済みトラックの履歴 (JSON には出力しない内部状態。delta update の適用時に属性不変の検査へ使う)
- Delta Update (`MsfDeltaUpdate`)
  - deltaUpdate (operation object の配列：`op` = "add" / "remove" / "clone") / generatedAt
  - Full / Delta の判別は `MsfCatalogDocument` が行う
- Delta 適用 (`MsfCatalog::apply_delta`)
  - clone の継承解決 (`MsfCloneTrack::into_track`) と add / remove / clone の順次適用
  - add するトラックは適用時点で §5.2 各フィールドの MUST を検証し、add 操作内の全トラックを検証してから追加する
  - 適用後の track name 一意性と、宣言された targetLatency / buffers のグループ内一致を再検証する (省略は player の裁量のため比較対象外、isLive=false は無視)
  - 削除済みの同じ (namespace, name) の再追加は、属性変更と isLive の false から true への変更を拒否する (§5.3 / §5.2.7)
- カタログトラック名 (`MSF_CATALOG_TRACK_NAME` = "catalog")
- Track (`MsfTrack`)：Codec / Video / Audio は draft 上の論理的な分類で、実体は単一の `MsfTrack` のフィールド
  - 共通：name / namespace / packaging / eventType / role / isLive / label / lang / targetLatency / buffers / trackDuration / renderGroup / altGroup / initRef / depends / parentName / template / maxGopDuration / maxGroupDuration
  - Codec：codec / mimeType
  - Video：width / height / displayWidth / displayHeight / framerate / timescale / bitrate / avgBitrate / temporalId / spatialId
  - Audio：samplerate / channelConfig
  - 保護 / アクセシビリティ：connectionUri / token / encryptionScheme / cipherSuite / keyId / trackBaseKey / authInfo / accessibility
- 関連型
  - clone 用 `MsfCloneTrack` (parentNamespace 対応)
  - 削除用 `MsfRemoveTrack`
  - `MsfInitData` / `MsfBuffers` / `MsfPackaging` (loc / mediatimeline / eventtimeline / moqlog / moqmetrics)
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
  - 予約パラメータ (`connection` / `wallclock-range` / `mediatime-range` / `location-range` / `c4m`) の値型アクセサ

## 未対応

- §5.5 / §12.1 (MSF_COMPRESSION property signaling)：-01 では Track Property ID が TBD、Object Property のレジストリ登録も未定義のため wire 実装を行わない (Timeline の圧縮は gzip magic byte 検出による従来動作のまま)
- §9 / §10 (Log / Metrics track)：packaging 値のみで、payload は MOQLOG / MOQMETRICS 側の定義で未実装
- §4.3 (Content protection)：catalog の signaling は扱うが、Secure Objects による暗号化と復号は未実装
