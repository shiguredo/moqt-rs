# Media over QUIC Transport (MOQT)

[draft-ietf-moq-transport-21](https://datatracker.ietf.org/doc/html/draft-ietf-moq-transport-21) の実装状況です。

## コントロールメッセージ

- SETUP
- GOAWAY (Request ID フィールドなし、New Session URI + Timeout のみ)
- REQUEST_OK / REQUEST_ERROR
  - PUBLISH への肯定応答 PUBLISH_OK は REQUEST_OK の別名 (共有ワイヤメッセージ)
  - REQUEST_ERROR は `Redirect` structure 付きの REDIRECT (`0x34`) に対応
- PUBLISH / PUBLISH_DONE
  - PUBLISH は Subscription Parameters を運び、auth token のコピーを禁じる
- PUBLISH_STATE_NOTIFY
- SUBSCRIBE / SUBSCRIBE_OK / REQUEST_UPDATE
- FETCH / FETCH_OK
  - 単一形式のみ (range は LOCATION_FILTER で指定、Standalone / Joining の種別とメッセージ内 Start / End Location は廃止済み)
- TRACK_STATUS (subscriber 側の送信と応答受信、publisher 側の受信と応答送信の両方。受信側は subscription state も Track Alias も作らない)

## Data stream / Datagram

- Subgroup Header (Subgroup ID mode 含む)
- Fetch Header
- Subgroup Object / Fetch Stream Object
- Fetch の End of Non-Existent Range / End of Unknown Range / End of Timed-Out Range マーカー
- Object Datagram
- Object Status (Normal / End of Group / End of Track)
- Object Properties
- Padding (stream / datagram、stream 側は読み捨て)
- Type Flags の未知 set bit は PROTOCOL_VIOLATION として拒否する

## Setup Options

- `PATH`
- `AUTHORIZATION_TOKEN`
- `MAX_AUTH_TOKEN_CACHE_SIZE`
- `AUTHORITY`
- `MOQT_IMPLEMENTATION`
- `MAX_REQUEST_UPDATES`
- `MAX_FILTER_RANGES`

## Message Parameters

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
  - TRACK_PROPERTY_FILTER は PUBLISH の選別を行わない。draft-21 は TRACK_PROPERTY_FILTER を
    SUBSCRIBE_TRACKS の応答として返る PUBLISH の選別に使うが、本ライブラリは relay 専用機能
    (SUBSCRIBE_TRACKS) を対象外としているため、選別の対象が存在しない (draft から削除された
    仕様ではない)
- Subscriber Priority
- Track Namespace Prefix

## Track Properties

- `OBJECT_DELIVERY_TIMEOUT`
- `SUBGROUP_DELIVERY_TIMEOUT`
- `MAX_CACHE_DURATION`
- `DEFAULT_PUBLISHER_PRIORITY`
- `DEFAULT_PUBLISHER_GROUP_ORDER`
- `DYNAMIC_GROUPS`
- `IMMUTABLE_PROPERTIES` (入れ子の検証と統合検索対応)

## Object Properties

- `OBJECT_DELIVERY_TIMEOUT`
- `SUBGROUP_DELIVERY_TIMEOUT`
- `IMMUTABLE_PROPERTIES`
- `PRIOR_GROUP_ID_GAP`
- `PRIOR_OBJECT_ID_GAP`

## その他

- GREASE 値の生成と判定 (`grease::{generate / is_grease}`)
- Namespace / Track Name のシリアライズ表現とパース (`name::{parse_name / parse_name_with_percent_encoding / serialize_name / parse_namespace / serialize_namespace / parse_track_name / serialize_track_name}`)
- Subgroup 再オープン禁止の検証 (`subgroup_tracker::SubgroupTracker`)
- セッション終了 / REQUEST_ERROR / PUBLISH_DONE / Stream Reset のエラーコード (`error` モジュール)

## 未対応

relay が担う機能は本ライブラリの対象外である (`CODEBASE.md` の「クライアントとサーバーのみで
リレーには対応しないこと」)。そのため relay 専用の以下のメッセージは実装しない。

- `PUBLISH_NAMESPACE` (`0x06`)
- `NAMESPACE` (`0x08`)
- `NAMESPACE_DONE` (`0x0E`)
- `PUBLISH_SKIPPED` (`0x0F`)
- `SUBSCRIBE_NAMESPACE` (`0x50`)
- `SUBSCRIBE_TRACKS` (`0x51`)

request stream の先頭として届く 3 種 (`PUBLISH_NAMESPACE` / `SUBSCRIBE_NAMESPACE` /
`SUBSCRIBE_TRACKS`) は §9 Table 5 に定義済みの request であるため、未知のメッセージ種別とは
扱わない。`ControlMessage::Unsupported` として受理し、`REQUEST_ERROR` の `NOT_SUPPORTED`
(`0x3`) と送信方向の FIN で拒否する (§1.5 (Modularity) の SHOULD)。セッションは維持する。
自側が control GOAWAY を送信済みなら `GOING_AWAY` を優先する。

応答専用の 3 種 (`NAMESPACE` / `NAMESPACE_DONE` / `PUBLISH_SKIPPED`) は応答先の request を
持たず Table 5 で "First" を持たないため、request stream の先頭および 2 通目以降のいずれでも
`SESSION_PROTOCOL_VIOLATION` でセッションを閉じる。§9 Table 5 に無い型も従来どおり
`MessageError::InvalidMessageType` で拒否する。

relay 専用の `RENDEZVOUS_TIMEOUT` (`0x04`) は §9.20.7 (RENDEZVOUS TIMEOUT Parameter) と
§16.7 (Message Parameters) の Table 13 に定義済みのため、SUBSCRIBE への出現は受理する。
ただし本ライブラリは relay を実装しないため値を解釈せず、`Subscription` にも保持しない。
アプリは decode 済みの `MessageParameters` から読める。
