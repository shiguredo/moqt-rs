# Media over QUIC Transport (MOQT)

[draft-ietf-moq-transport-22](https://datatracker.ietf.org/doc/html/draft-ietf-moq-transport-22) の実装状況です。

## コントロールメッセージ

- SETUP
- GOAWAY (Request ID フィールドなし、New Session URI + Timeout のみ)
- REQUEST_OK / REQUEST_ERROR
  - PUBLISH への肯定応答 PUBLISH_OK は REQUEST_OK の別名 (共有ワイヤメッセージ)
  - REQUEST_OK が運べるパラメータは応答の種類ごとの許可集合に限る (codec 層は EXPIRES / LARGEST_OBJECT の和集合で検証し、種類ごとの厳密な検証はセッション層が行う)
  - REQUEST_ERROR は `Redirect` structure 付きの REDIRECT (`0x34`) に対応
- PUBLISH / PUBLISH_DONE
  - PUBLISH は Subscription Parameters を運び、auth token のコピーを禁じる
- PUBLISH_STATE_NOTIFY
- SUBSCRIBE / SUBSCRIBE_OK / REQUEST_UPDATE
- FETCH / FETCH_OK
  - 単一形式のみ (range は LOCATION_FILTER で指定、Standalone / Joining の種別とメッセージ内 Start / End Location は廃止済み)
  - Group Order が Descending (0x02) のときは `FetchStreamDecoder::new_with_group_order` に 0x02 を渡す必要があり、ギャップの意味解釈 (非存在 / 不明の区別、末尾ギャップの FIN 判定) はアプリ責務
- TRACK_STATUS (subscriber 側の送信と応答受信、publisher 側の受信と応答送信の両方。受信側は subscription state も Track Alias も作らない)

## Data stream / Datagram

- Subgroup Header (Subgroup ID mode 含む)
- Fetch Header
- Subgroup Object / Fetch Stream Object
- Fetch の End of Non-Existent Range / End of Unknown Range / End of Timed-Out Range マーカー (Group Order が Descending (0x02) のときは Group 降順 / 同 Group 内 Object ID 昇順でデルタを解決し、End of Range (0x8C / 0x10C / 0x20C) の Group ID / Object ID は絶対値)
- Object Datagram
- Object の Delivery Mode (Subgroup / Datagram) を Object 単位で追跡し、重複 Object の Malformed 検出 (§12.1 の条件 7) に使う。送信側の強制は行わない
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
- Location Filter (range を LOCATION_FILTER で指定。§9.20.9 の Location Filter Type ごとに後続フィールドが決まる符号化で、Type 0x00 (None) と 0x05 (Next Object) を区別する)
- New Group Request
- Range Filters (Subgroup / ObjectID / Priority / Object Property / Track Property)
  - codec と MAX_FILTER_RANGES / 重複 / 構造の検証
  - session 層で Object の forward 判定 (`object_passes_filters` / `header_passes_filters`)
  - TRACK_PROPERTY_FILTER は PUBLISH の選別を行わない。draft-22 は TRACK_PROPERTY_FILTER を
    SUBSCRIBE_TRACKS の応答として返る PUBLISH の選別に使うが、本ライブラリは MoQ リレー専用機能
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
- 音声の時間圧縮・伸長と欠落の補間、目標遅延の学習、鳴らす時刻の決定、A/V 同期の遅延制御、音声と映像の共通の時間軸、表示時刻に合わせたフレームの選択 (`playout::{stretch, delay, scheduler, timeline, buffer}`)
- メディア時刻の Unix epoch の壁時計への換算 (`media_clock::WallClockMapper`)
- セッション終了 / REQUEST_ERROR / PUBLISH_DONE / Stream Reset のエラーコード (`error` モジュール)

## 未対応

MoQ リレーが担う機能は本ライブラリの対象外である (`CODEBASE.md` の「クライアントとサーバーのみで
MoQ リレーには対応しないこと」)。そのため MoQ リレー専用の以下のメッセージは実装しない。

- `PUBLISH_NAMESPACE` (`0x06`)
- `NAMESPACE` (`0x08`)
- `NAMESPACE_DONE` (`0x0E`)
- `PUBLISH_SKIPPED` (`0x0F`)
- `SUBSCRIBE_NAMESPACE` (`0x50`)
- `SUBSCRIBE_TRACKS` (`0x51`)

request stream の先頭として届く 3 種 (`PUBLISH_NAMESPACE` / `SUBSCRIBE_NAMESPACE` /
`SUBSCRIBE_TRACKS`) は §9 Table 5 に定義済みの request であるため、未知のメッセージ種別とは
扱わない。`ControlMessage::Unsupported` として受理し、本体を共通構造 (Request ID →
Track Namespace Prefix → Number of Parameters → Parameters) に従って解析したうえで、
`REQUEST_ERROR` の `NOT_SUPPORTED` (`0x3`、`.` 単独または `.session` なら `DOES_NOT_EXIST`
(`0x10`)、Range Filter が §3.3.2 に違反するなら `INVALID_FILTER` (`0x36`)) と送信方向の FIN で
拒否する (§1.6 (Modularity) の SHOULD、§2.4.3 (Reserved Namespaces) / §6.5 (Session-Level
Tracks and Namespaces) / §3.3.2 (Range Filters) の拒否は MUST)。セッションは維持する。拒否
コードの優先順は `MALFORMED_AUTH_TOKEN` > `INVALID_FILTER` > `DOES_NOT_EXIST` > `GOING_AWAY`
(自側が control GOAWAY を送信済み) > `NOT_SUPPORTED` である (§8.9 のメッセージ拒否は MUST、
§3.3.2 / §2.4.3 / §6.5 の拒否は MUST、§9.2 の GOAWAY 拒否は MAY、§1.6 の NOT_SUPPORTED は
SHOULD)。

本体を解析するため、拒否する場合でも次の検証は適用される。

- §3.3.2 (Range Filters) / §9.1.6 (MAX FILTER RANGES): 同一 (Parameter Type, SetID,
  Property Type) の Range Filter の重複と、自側 `MAX_FILTER_RANGES` を超える Range 総数は
  `INVALID_FILTER` (`0x36`) でメッセージ単位に拒否する (MUST)。FILL_PARAMETERS の内側の
  Range Filter も検証する。未対応 request は subscription を作らないため、1 メッセージ内の
  検証に限る
- §2.4.3 (Reserved Namespaces) / §6.5 (Session-Level Tracks and Namespaces): 先頭フィールドが
  `.` 単独または `.session` の Track Namespace Prefix を持つ request は `DOES_NOT_EXIST`
  (`0x10`) でメッセージ単位に拒否する (MUST)。`.foo` などその他の `.` 始まりの予約名前空間への
  request は、§2.4.3 第 1 段落が未認識の予約名前空間を Application へ渡すことを MUST とするが、
  本ライブラリには未対応 request を Application へ渡す経路が無いため `NOT_SUPPORTED` のまま
  とする
- §9.20.1 (Parameter Scope): メッセージ型ごとに許可されないパラメータの受信は
  `PROTOCOL_VIOLATION` でセッションを閉じる。`PUBLISH_NAMESPACE` / `SUBSCRIBE_NAMESPACE` が
  許可するのは AUTHORIZATION_TOKEN のみ。`SUBSCRIBE_TRACKS` は §9.18 の列挙に加えて
  §3.6.2 (Parameters on SUBSCRIBE_TRACKS) の "Any Parameter that can be specified on a
  Subscription (ie: in SUBSCRIBE) is valid in SUBSCRIBE_TRACKS" を採り、SUBSCRIBE の許可
  パラメータとの和集合とする (同節が SUBSCRIBE_TRACKS に載せられると明記する LOCATION_FILTER /
  FILL_PARAMETERS を §9.18 の列挙が含まないため。仕様が許容するメッセージでセッションを
  閉じない側を選ぶ)
- §8.9 (Authorization Token Compression): Alias Type REGISTER のトークンは拒否する場合も
  token cache へ登録し (MUST)、(Token Type, Token Value) の重複は alias 解決後まで含めて判定し
  `REQUEST_ERROR` の `MALFORMED_AUTH_TOKEN` でメッセージ単位に拒否する。alias は peer の
  token cache とメッセージ内の先行 REGISTER / DELETE から解決する。解決できない alias は
  一意性の判定対象から外し、未登録 alias の扱いは経路ごとの現行挙動のまま変えない。
  DELETE / USE_ALIAS / USE_VALUE は失敗するメッセージのパラメータとして適用しない
- 本体が共通構造に従わない場合は原因に応じたコード (`PROTOCOL_VIOLATION` /
  `KEY_VALUE_FORMATTING_ERROR`) でセッションを閉じる
- REGISTER の適用自体が失敗した場合はセッションを閉じる (`DUPLICATE_AUTH_TOKEN_ALIAS`:
  登録済み alias の再登録、`AUTH_TOKEN_CACHE_OVERFLOW`: 自側が宣言した
  `MAX_AUTH_TOKEN_CACHE_SIZE` を超える登録。REQUEST_ERROR は送らない)

応答専用の 3 種 (`NAMESPACE` / `NAMESPACE_DONE` / `PUBLISH_SKIPPED`) は応答先の request を
持たず Table 5 で "First" を持たないため、request stream の先頭および 2 通目以降のいずれでも
`SESSION_PROTOCOL_VIOLATION` でセッションを閉じる。§9 Table 5 に無い型も従来どおり
`MessageError::InvalidMessageType` で拒否する。

MoQ リレー専用の `RENDEZVOUS_TIMEOUT` (`0x04`) は §9.20.6 (RENDEZVOUS TIMEOUT Parameter) と
§16.7 (Message Parameters) の Table 14 に定義済みのため、SUBSCRIBE への出現は受理する。
ただし本ライブラリは MoQ リレーを実装しないため値を解釈せず、`Subscription` にも保持しない。
アプリは decode 済みの `MessageParameters` から読める。

## モジュール構成

| モジュール | 概要 |
| --- | --- |
| `c4m` | C4M (CAT / CWT) のトークン発行 / 検証と `moqt` クレームの認可 |
| `c4m::cat` | CAT のクレーム / トークン / 発行ビルダー |
| `c4m::cbor` | CBOR (RFC 8949) の encode / decode |
| `c4m::cose` | COSE (RFC 9052) の構造とアルゴリズム定義 |
| `c4m::crypto` | 署名 / 検証 / ハッシュの trait と鍵表現 (aws-lc-rs 実装は feature) |
| `c4m::jwk` | JWK (RFC 7517) と JWK サムプリント (RFC 7638) |
| `c4m::jwt` | JWS compact (RFC 7515) の JWT |
| `c4m::dpop` | DPoP proof の検証と発行 (draft-nandakumar-moq-generic-dpop-proof-00) |
| `decoder` | バッファ付きインクリメンタル制御メッセージデコーダー |
| `error` | コーデックエラー型とセッション終了 / REQUEST_ERROR / PUBLISH_DONE / Stream Reset の各コード |
| `grease` | GREASE 値の生成と判定ユーティリティ |
| `loc` | LOC Properties の encode / decode |
| `message` | 制御メッセージ、`TrackNamespace`、`Location` |
| `message_parameter` | Message Parameters と `AUTHORIZATION_TOKEN` |
| `msf` | MSF Catalog / Timeline / URI の encode / decode |
| `name` | Namespace / Track Name のシリアライズ表現とパース (`parse_name` / `parse_name_with_percent_encoding` / `serialize_name` / `parse_namespace` / `serialize_namespace` / `parse_track_name` / `serialize_track_name`) |
| `object_properties` | Object-scoped Properties と `IMMUTABLE_PROPERTIES` 補助デコーダー |
| `parameter` | SETUP Options の encode / decode |
| `playout` | 音声の時間圧縮・伸長と欠落の補間、目標遅延の学習、鳴らす時刻の決定、A/V 同期の遅延制御、音声と映像の共通の時間軸、表示時刻に合わせたフレームの選択 |
| `session` | 1 本の `MOQT Transport Session` に閉じた sans I/O な状態機械で、control plane に加えて request stream / data stream / datagram の state も扱う |
| `stream` | data stream ヘッダ、`SubgroupStreamDecoder`、`FetchStreamDecoder`、`FetchStreamEncoder` |
| `subgroup_tracker` | Subgroup 再オープン禁止の検証ユーティリティ |
| `track_properties` | Track Properties の encode / decode |
| `varint` | 可変長整数 (vi64) の encode / decode |

内部専用の `kvp` モジュール (delta-key 骨格) は公開 API ではありません。

## アーキテクチャ

| 層 | 主なモジュール | 概要 |
| --- | --- | --- |
| Codec / helper | `varint`, `message`, `name`, `message_parameter`, `parameter`, `decoder`, `stream`, `loc`, `msf`, `object_properties`, `track_properties`, `grease`, `playout`, `media_clock`, `error`, `c4m` | ワイヤーフォーマットの encode / decode と検証、音声再生の純粋な処理 |
| Stateful helper | `subgroup_tracker`, `session::auth_token_cache::AuthTokenCache`, `session::request_id::RequestIdGenerator`, `session::request_id::RequestIdTracker` | セッション実装で使う状態付き補助コンポーネント |
| Session | `session` | 1 本の `MOQT Transport Session` に閉じた endpoint-local な protocol state を管理する |

`session` は SETUP と Request ID 管理に加えて、SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS の bidi request stream を扱います。
uni data stream と datagram の送受信 state も扱います。
REQUEST_UPDATE / PUBLISH_DONE / STOP_SENDING、FETCH、GOAWAY ハンドシェイクと drain、delivery timeout などの deadline 管理 (`tick` 駆動) も行います。
一方で I/O、非同期処理、MoQ リレーが担う namespace 発見・告知と forwarding は含みません。
