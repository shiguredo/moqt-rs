# 実装状況

本ライブラリの Media over QUIC Transport (MOQT) / Low Overhead Container (LOC) / MOQT Streaming Format (MSF) の実装状況です。

## Media over QUIC Transport

[draft-ietf-moq-transport-21](https://datatracker.ietf.org/doc/html/draft-ietf-moq-transport-21) の実装状況です。

### コントロールメッセージ

- SETUP
- GOAWAY (Request ID フィールドなし、New Session URI + Timeout のみ)
- REQUEST_OK / REQUEST_ERROR
  - PUBLISH への肯定応答 PUBLISH_OK は REQUEST_OK の別名 (共有ワイヤメッセージ)
  - REQUEST_ERROR は `Redirect` structure 付きの REDIRECT (`0x34`) に対応
- PUBLISH / PUBLISH_DONE / PUBLISH_SKIPPED
  - PUBLISH は Subscription Parameters を運び、auth token のコピーを禁じる
- PUBLISH_STATE_NOTIFY
- SUBSCRIBE / SUBSCRIBE_OK / REQUEST_UPDATE
- FETCH / FETCH_OK
  - 単一形式のみ (range は LOCATION_FILTER で指定、Standalone / Joining の種別とメッセージ内 Start / End Location は廃止済み)
- PUBLISH_NAMESPACE
- NAMESPACE / NAMESPACE_DONE (同一ワイヤ形式を共有)
- SUBSCRIBE_NAMESPACE
- SUBSCRIBE_TRACKS
- TRACK_STATUS

### Data stream / Datagram

- Subgroup Header (Subgroup ID mode 含む)
- Fetch Header
- Subgroup Object / Fetch Stream Object
- Fetch の End of Non-Existent Range / End of Unknown Range / End of Timed-Out Range マーカー
- Object Datagram
- Object Status (Normal / End of Group / End of Track)
- Object Properties
- Padding (stream / datagram、stream 側は読み捨て)
- Type Flags の未知 set bit は PROTOCOL_VIOLATION として拒否する

### Setup Options

- `PATH`
- `AUTHORIZATION_TOKEN`
- `MAX_AUTH_TOKEN_CACHE_SIZE`
- `AUTHORITY`
- `MOQT_IMPLEMENTATION`
- `MAX_REQUEST_UPDATES`
- `MAX_FILTER_RANGES`

### Message Parameters

- Authorization Token
- Object Delivery Timeout (起点は last header byte)
- Subgroup Delivery Timeout
- Expires
- Fill Timeout
- Fill Parameters (fill fetch stream 対応)
- Forward
- Group Order
- Include Properties
- Largest Object (publisher の保持 MUST 要件はなし)
- Location Filter (range を LOCATION_FILTER で指定)
- New Group Request
- Range Filters (Subgroup / ObjectID / Priority / Object Property / Track Property)
  - codec と MAX_FILTER_RANGES / 重複 / 構造の検証
  - session 層で Object の forward 判定 (`object_passes_filters` / `header_passes_filters`)
  - TRACK_PROPERTY_FILTER による PUBLISH 選別 (`track_properties_pass`)
- Rendezvous Timeout
- Subscriber Priority
- Track Namespace Prefix

### Track Properties

- `OBJECT_DELIVERY_TIMEOUT`
- `SUBGROUP_DELIVERY_TIMEOUT`
- `MAX_CACHE_DURATION`
- `DEFAULT_PUBLISHER_PRIORITY`
- `DEFAULT_PUBLISHER_GROUP_ORDER`
- `DYNAMIC_GROUPS`
- `IMMUTABLE_PROPERTIES` (入れ子の検証と統合検索対応)

### Object Properties

- `OBJECT_DELIVERY_TIMEOUT`
- `SUBGROUP_DELIVERY_TIMEOUT`
- `IMMUTABLE_PROPERTIES`
- `PRIOR_GROUP_ID_GAP`
- `PRIOR_OBJECT_ID_GAP`

### その他

- GREASE 値の生成と判定 (`grease::{generate / is_grease}`)
- Namespace / Track Name のシリアライズ表現とパース (`name::{parse_name / serialize_name}`)
- Subgroup 再オープン禁止の検証 (`subgroup_tracker::SubgroupTracker`)
- セッション終了 / REQUEST_ERROR / PUBLISH_DONE / Stream Reset のエラーコード (`error` モジュール)

## Low Overhead Container

[draft-ietf-moq-loc-04](https://datatracker.ietf.org/doc/html/draft-ietf-moq-loc-04) の実装状況です。

### LOC Properties

- Timestamp (`0x10`)
- Timescale (`0x08`)
- Video Config (`0x0D`)
- Video Frame Marking (`0x09`、1-4 bytes)
- Audio Level (`0x0C`、0-255)
- Audio Config (`0x0F`)

偶数 ID は varint 値、奇数 ID は長さ付きバイト列として encode / decode します。
Public / Private の配置はアプリケーション層の責務です。

### 未対応

- §2.1 (Video Payload Format)：annexB / length prefix / parameter set を解釈しない (example は AVCC 形式と Video Config の経路のみ対応する)
- §3 (Payload Encryption)：Secure Objects 連携による暗号化と復号は未実装 (MSF 側の signaling フィールドは扱う)

## MOQT Streaming Format

[draft-ietf-moq-msf-01](https://datatracker.ietf.org/doc/html/draft-ietf-moq-msf-01) の実装状況です。

### Catalog

- Catalog (`MsfCatalog`)
  - version / generatedAt / isComplete / tracks / publishTracks / initDataList
- Delta Update (`MsfDeltaUpdate`)
  - deltaUpdate (operation object の配列：`op` = "add" / "remove" / "clone") / generatedAt
  - Full / Delta の判別は `MsfCatalogDocument` が行う
- Delta 適用 (`MsfCatalog::apply_delta`)
  - clone の継承解決 (`MsfCloneTrack::into_track`) と add / remove / clone の順次適用
  - 適用後の track name 一意性とグループ内 targetLatency / buffers 一致を再検証する
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
  - targetLatency / buffers のグループ内一致
  - isComplete=false 禁止
  - timeline の depends / mimeType 必須
  - initRef の initDataList 参照整合性
  - role に応じた codec / bitrate / samplerate / channelConfig 必須
  - lang の BCP 47 簡易検証

### Timeline

- Media Timeline (`[[pts_ms, [group_id, object_id], wallclock_ms], ...]` 形式)
- Event Timeline (`{t / l / m + data}` 形式、`data` は JSON object の生バイト列)
- Gzip 圧縮と自動展開 (`msf::TimelineEncodingOptions` / `encode_media_timeline` 等)
  - `TimelineEncodingOptions` と Timeline の encode / decode 関数は `msf` モジュール経由で利用する

### URI

- MSF URI / fragment のパース (`msf::uri`)
  - `parse_msf_uri` / `parse_msf_fragment`
  - 予約パラメータ (`connection` / `wallclock-range` / `mediatime-range` / `location-range` / `c4m`) の値型アクセサ

### 未対応

- §5.5 / §12.1 (MSF_COMPRESSION property signaling)：-01 では Track Property ID が TBD、Object Property のレジストリ登録も未定義のため wire 実装を行わない (Timeline の圧縮は gzip magic byte 検出による従来動作のまま)
- §9 / §10 (Log / Metrics track)：packaging 値のみで、payload は MOQLOG / MOQMETRICS 側の定義で未実装
- §4.3 (Content protection)：catalog の signaling は扱うが、Secure Objects による暗号化と復号は未実装

## モジュール構成

| モジュール | 概要 |
| --- | --- |
| `decoder` | バッファ付きインクリメンタル制御メッセージデコーダー |
| `error` | コーデックエラー型とセッション終了 / REQUEST_ERROR / PUBLISH_DONE / Stream Reset の各コード |
| `grease` | GREASE 値の生成と判定ユーティリティ |
| `loc` | LOC Properties の encode / decode |
| `message` | 制御メッセージ、`TrackNamespace`、`Location` |
| `message_parameter` | Message Parameters と `AUTHORIZATION_TOKEN` |
| `msf` | MSF Catalog / Timeline / URI の encode / decode |
| `name` | Namespace / Track Name のシリアライズ表現とパース (`parse_name` / `serialize_name`) |
| `object_properties` | Object-scoped Properties と `IMMUTABLE_PROPERTIES` 補助デコーダー |
| `parameter` | SETUP Options の encode / decode |
| `session` | 1 本の `MOQT Transport Session` に閉じた sans I/O な状態機械で、control plane に加えて request stream / data stream / datagram の state も扱う |
| `stream` | data stream ヘッダ、`SubgroupStreamDecoder`、`FetchStreamDecoder`、`FetchStreamEncoder` |
| `subgroup_tracker` | Subgroup 再オープン禁止の検証ユーティリティ |
| `track_properties` | Track Properties の encode / decode |
| `varint` | 可変長整数 (vi64) の encode / decode |

内部専用の `kvp` モジュール (delta-key 骨格) は公開 API ではありません。

## アーキテクチャ

| 層 | 主なモジュール | 概要 |
| --- | --- | --- |
| Codec / helper | `varint`, `message`, `name`, `message_parameter`, `parameter`, `decoder`, `stream`, `loc`, `msf`, `object_properties`, `track_properties`, `grease`, `error` | ワイヤーフォーマットの encode / decode と検証 |
| Stateful helper | `subgroup_tracker`, `session::auth_token_cache::AuthTokenCache`, `session::request_id::RequestIdGenerator`, `session::request_id::RequestIdTracker` | セッション実装で使う状態付き補助コンポーネント |
| Session | `session` | 1 本の `MOQT Transport Session` に閉じた endpoint-local な protocol state を管理する |

`session` は SETUP と Request ID 管理に加えて、SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS / PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS の bidi request stream を扱います。
uni data stream と datagram の送受信 state も扱います。
REQUEST_UPDATE / PUBLISH_DONE / STOP_SENDING、FETCH、GOAWAY ハンドシェイクと drain、delivery timeout などの deadline 管理 (`tick` 駆動) も行います。
一方で I/O、非同期処理、relay 固有の routing / fan-out / cache / policy は含みません。
