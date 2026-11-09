# Media over QUIC Transport (MOQT)

[draft-ietf-moq-transport-23](https://datatracker.ietf.org/doc/html/draft-ietf-moq-transport-23) の実装状況です。

## コントロールメッセージ

- SETUP
- GOAWAY (Request ID フィールドなし、New Session URI + Timeout のみ)
- REQUEST_OK / REQUEST_ERROR
  - PUBLISH への肯定応答 PUBLISH_OK は REQUEST_OK の別名 (共有ワイヤメッセージ)
  - REQUEST_OK が運べるパラメータは応答の種類ごとの許可集合に限る (codec 層は EXPIRES / LARGEST_OBJECT の和集合で検証し、種類ごとの厳密な検証はセッション層が行う)
  - REQUEST_ERROR は Error Details (Delta Type 付き KVP) の REDIRECT Error Detail (`0x01`) と UNSUPPORTED_PROPERTY Error Detail (`0x02`) に対応する。REDIRECT (`0x34`) では REDIRECT Detail が必須で、UNSUPPORTED_EXTENSION (`0x33`) では未対応の Mandatory Track Property の型ごとに 1 つ載せる
- PUBLISH / PUBLISH_DONE
  - PUBLISH は Subscription Parameters を運び、auth token のコピーを禁じる
- PUBLISH_STATE_NOTIFY
- SUBSCRIBE / SUBSCRIBE_OK / REQUEST_UPDATE
  - REQUEST_UPDATE は到着順に処理し、REQUEST_UPDATE_OK も受信順に返す。1 本の request stream 上で
    完結する応答のため Request ID を運ばない (§9.5)。FILL_PARAMETERS を含む REQUEST_UPDATE は
    coalescing せず 1 通ずつ適用する (§9.5 / §9.19.15)
- FETCH / FETCH_OK (FETCH への REQUEST_OK の別名であり、専用のワイヤメッセージはない。旧 FETCH_OK の Type `0x18` は予約値として拒否する)
  - 単一形式のみ (range は LOCATION_FILTER で指定、Standalone / Joining の種別とメッセージ内 Start / End Location は廃止済み)
  - Group Order が Descending (0x02) のときは `FetchStreamDecoder::new_with_group_order` に 0x02 を渡す必要があり、ギャップの意味解釈 (非存在 / 不明の区別、末尾ギャップの FIN 判定) はアプリ責務
  - 相対 Location Filter (Location Filter Type 0x01 / 0x05) を含む FETCH は受信時に REQUEST_ERROR の INVALID_RANGE (`0x11`) で拒否し、送信時は送出前に拒否する (§3.2 / §9.19.9)
- TRACK_STATUS (subscriber 側の送信と応答受信、publisher 側の受信と応答送信の両方。受信側は subscription state も Track Alias も作らない)
  - TRACK_STATUS_OK の EXPIRES は同じ Track の SUBSCRIBE_OK で提示される見込みの購読期限であり、要求自体の期限ではない。
    単発要求のため期限タイマーは起動せず、`TrackStatusEntry::expires_ms` で参照する (FIN と破棄の条件は EXPIRES の有無で変わらない)

## Data stream / Datagram

- Subgroup Header (Subgroup ID mode 含む)
- Fetch Header
- Subgroup Object / Fetch Stream Object
- Fetch の End of Non-Existent Range / End of Unknown Range / End of Timed-Out Range / End of Gone Range マーカー (Group Order が Descending (0x02) のときは Group 降順 / 同 Group 内 Object ID 昇順でデルタを解決し、End of Range (0x8C / 0x10C / 0x20C / 0x40C) の Group ID / Object ID は絶対値)
  - Gone は元の publisher が再配送できないが存在自体は否定しない状態であり (draft-ietf-moq-transport-23 §2.1.2)、Non-Existent (存在しないと確定した状態) とは区別する
- Object Datagram
- Object の Delivery Mode (Subgroup / Datagram) を Object 単位で追跡し、重複 Object の Malformed 検出 (§12.1 の条件 7) に使う。送信側の強制は行わない
- Malformed Track (§12.1) の検出は条件 1〜10 のうち、重複 Object の Payload / Immutable Properties の不一致 (条件 6)、
  Delivery Mode の不一致 (条件 7)、サポートする Property のスコープ外受信 (条件 8)、複数を許さない Property の
  重複 (条件 9)、不存在と記録した Object の受信と受信済み Object の不存在通知 (条件 10) に対応する。
  検出時は同じ Track の購読を取消し、アプリへ `MalformedTrack` を通知する (送信側の強制は行わない)
- Object Status (Normal / End of Group / End of Track)
- Object Properties
- Padding (stream / datagram、stream 側は読み捨て)
- Type Flags の未知 set bit は INVALID_STREAM_HEADER (`0x200`) として拒否する。Object Status の
  未知値と非 Normal Status への Properties は INVALID_OBJECT_STATUS (`0x202`)、Object の
  復号失敗は MALFORMED_OBJECT (`0x201`)、シリアライズ途中の FIN は TRUNCATED_OBJECT (`0x203`)

## Setup Options

- `PATH`
- `AUTHORIZATION_TOKEN`
- `MAX_AUTH_TOKEN_CACHE_SIZE`
- `AUTHORITY`
- `MOQT_IMPLEMENTATION`
- `MAX_REQUEST_UPDATES`
- `MAX_FILTER_RANGES`
- `MAX_CONTROL_MESSAGE_SIZE` (受信上限と peer へ提示する送信上限。省略時の既定値と最小値は 65535)

## Message Parameters

- Authorization Token
- Object Delivery Timeout (起点は last header byte)
- Subgroup Delivery Timeout
- Expires (すべての OK 応答で受理する。TRACK_STATUS_OK の値は仮の購読期限であり期限タイマーを起動しない。
  FETCH_OK の値は `Fetch::expires_ms` と `FetchOkReceived.expires_ms`、TRACK_STATUS_OK は受信した値を `TrackStatusEntry::expires_ms` で参照できる)
- Fill Timeout
- Fill Parameters (FILL_PARAMETERS は「Number of Parameters (vi64) + その個数の Parameters」で符号化し、
  外側の byte length ではない。内側は FILL_PARAMETERS を再帰させず、§9.19.15 の許可型に限る。
  fill fetch stream に対応する)
- Paused / Unpaused (旧 FORWARD を置き換えた PAUSED。Type `0x11`、0 = unpaused / 1 = paused、既定 0)
- Group Order
- Include Properties
- Largest Object (SUBSCRIBE_OK / PUBLISH / 購読と FETCH の REQUEST_UPDATE_OK / PUBLISH_STATE_NOTIFY では session 層が
  Track 単位で自動補完する。自動補完は利用者指定値と観測最大の大きい方を載せ、指定値を下げない。観測値は
  publisher 役 subscription の送信記録に限られ、FETCH 応答で送った Object は含まれない。TRACK_STATUS_OK は未対応。
  自側が publish した最大は publisher 役 subscription を forget した後も保持する。
  FETCH_OK は要求範囲と Largest Object から応答範囲の終端を導出し、Largest Object が要求範囲内なら
  LARGEST_OBJECT を載せて応答をそこへ切り詰め、範囲外なら取り除く。要求が End を省略している場合の End は
  Largest Object なので、自前のキャッシュから Largest Object を把握している利用者は `send_fetch_ok` に渡す)
- Location Filter (range を LOCATION_FILTER で指定。§9.19.9 の Location Filter Type ごとに後続フィールドが決まる符号化で、Type 0x00 (None) と 0x05 (Next Object) を区別する)
  - publisher の送信側は subscription に加え、FETCH 応答と fill fetch stream の Object Location を
    要求範囲 (FETCH_OK の応答範囲の終端を含む) で検証する (`send_fetch_object` が範囲外を
    `LocalFilterMismatch` で拒否する)。End of Range は Object ではないため `send_fetch_end_of_range`
    を使い、Location 検証の対象外とする。
  - FETCH 応答に対する Range Filters (§3.3.2 の Subgroup / Object ID / Priority / Object Property) の
    評価は未対応である (Location Filter のみ。FETCH Object の送信 API に Subgroup ID / Priority /
    Properties を渡す設計が必要)
- New Group Request
- Range Filters (Subgroup / ObjectID / Priority / Object Property / Track Property)
  - codec と MAX_FILTER_RANGES / 重複 / 構造の検証
  - session 層で Object の forward 判定 (`object_passes_filters` / `header_passes_filters`)
  - TRACK_PROPERTY_FILTER は PUBLISH の選別を行わない。draft-23 §3.6.2 (Parameters on
    SUBSCRIBE_TRACKS) は TRACK_PROPERTY_FILTER を SUBSCRIBE_TRACKS 自身に適用するが、本ライブラリは
    MoQ リレー専用機能 (SUBSCRIBE_TRACKS) を対象外としているため、選別の対象が存在しない
    (draft から削除された仕様ではない)
- Subscribe Tracks ID (PUBLISH のみ。SUBSCRIBE_TRACKS の Request ID を相関情報として `Subscription`
  に保持する。本ライブラリは SUBSCRIBE_TRACKS を実装しないため、対応する namespace 要求の有無は
  検証せず、要求が無い PUBLISH でも通常の検証と受理判断へ進む。SUBSCRIBE_TRACKS 起因の PUBLISH に
  だけ載せる契約であり、`resulting_publish_parameters` でも引き継がない)
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
- 空 (0 バイト長) の Track Name を拒否する (§2.4.1。`.session` の旧例外は draft から削除済み)
- `NEW_GROUP_REQUEST` は `DYNAMIC_GROUPS` を受信していなくても受理する (§3.5.1 / §9.19.19)
- Subgroup 再オープン禁止の検証 (`subgroup_tracker::SubgroupTracker`)
- 到着の揺らぎと実出力の先行量からの再生予算、鳴らす時刻の決定、A/V 同期の遅延制御、音声と映像の共通の時間軸、表示時刻に合わせたフレームの選択、時間圧縮・伸長と欠落の補間 (`playout::{timeline, scheduler, buffer, stretch, expansion, predictor, gap, splice_fade, resample, timing, catch_up}`)
- メディア時刻の Unix epoch の壁時計への換算 (`media_clock::WallClockMapper`)
- セッション終了 / REQUEST_ERROR / PUBLISH_DONE / Stream Reset のエラーコード (`error` モジュール)。
  セッション終了コードの分類は後述の「セッション終了コードの分類」を参照

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
- §9.19.1 (Parameter Scope): メッセージ型ごとに許可されないパラメータの受信は
  `INVALID_PARAMETER` (`0x305`) でセッションを閉じる。`PUBLISH_NAMESPACE` / `SUBSCRIBE_NAMESPACE` が
  許可するのは AUTHORIZATION_TOKEN のみ。`SUBSCRIBE_TRACKS` は §9.17 (SUBSCRIBE_TRACKS) が
  列挙する 14 種 (OBJECT_DELIVERY_TIMEOUT / AUTHORIZATION_TOKEN / SUBGROUP_DELIVERY_TIMEOUT /
  PAUSED / SUBSCRIBER_PRIORITY / LOCATION_FILTER / GROUP_ORDER / FILL_PARAMETERS /
  SUBGROUP_FILTER / OBJECTID_FILTER / PRIORITY_FILTER / OBJECT_PROPERTY_FILTER /
  TRACK_PROPERTY_FILTER / INCLUDE_PROPERTIES) をそのまま採る。§3.6.2 (Parameters on
  SUBSCRIBE_TRACKS) は単一 Track の SUBSCRIBE にだけ意味を持つ RENDEZVOUS_TIMEOUT /
  NEW_GROUP_REQUEST を SUBSCRIBE_TRACKS に載せられないと定めるため、SUBSCRIBE の許可集合との
  和集合にはしない。2 型は `INVALID_PARAMETER` で拒否し、セッション終了を伴う検証は
  §8.9 (Authorization Token Compression) の REGISTER 適用より先に評価するため、同じ
  メッセージの REGISTER も token cache へ残さない
- §8.9 (Authorization Token Compression): Alias Type REGISTER のトークンは拒否する場合も
  token cache へ登録し (MUST)、(Token Type, Token Value) の重複は alias 解決後まで含めて判定し
  `REQUEST_ERROR` の `MALFORMED_AUTH_TOKEN` でメッセージ単位に拒否する。alias は peer の
  token cache とメッセージ内の先行 REGISTER / DELETE から解決する。解決できない alias は
  一意性の判定対象から外し、未登録 alias の扱いは経路ごとの現行挙動のまま変えない。
  DELETE / USE_ALIAS / USE_VALUE は失敗するメッセージのパラメータとして適用しない
- 本体が共通構造に従わない場合は原因に応じたコード (`INVALID_TRACK_NAMESPACE` /
  `MALFORMED_KEY_VALUE` / `INVALID_PARAMETER` / `MALFORMED_MESSAGE` /
  `DELTA_ENCODING_OVERFLOW` / `FIELD_LENGTH_EXCEEDED`) でセッションを閉じる
- REGISTER の適用自体が失敗した場合はセッションを閉じる (`DUPLICATE_AUTH_TOKEN_ALIAS`:
  登録済み alias の再登録、`AUTH_TOKEN_CACHE_OVERFLOW`: 自側が宣言した
  `MAX_AUTH_TOKEN_CACHE_SIZE` を超える登録。REQUEST_ERROR は送らない)

応答専用の 3 種 (`NAMESPACE` / `NAMESPACE_DONE` / `PUBLISH_SKIPPED`) は応答先の request を
持たず Table 5 で "First" を持たないため、request stream の先頭および 2 通目以降のいずれでも
`UNEXPECTED_MESSAGE` (`0x303`) でセッションを閉じる。§9 Table 5 に無い型も
`MessageError::InvalidMessageType` (セッション終了コードは `UNEXPECTED_MESSAGE`) で拒否する。

MoQ リレー専用の `RENDEZVOUS_TIMEOUT` (`0x04`) は §9.19.6 (RENDEZVOUS TIMEOUT Parameter) と
§16.7 (Message Parameters) の Table 14 に定義済みのため、SUBSCRIBE への出現は受理する。
ただし本ライブラリは MoQ リレーを実装しないため値を解釈せず、`Subscription` にも保持しない。
アプリは decode 済みの `MessageParameters` から読める。SUBSCRIBE_TRACKS へは載せられない
(§3.6.2) ため、受信すると `INVALID_PARAMETER` でセッションを閉じる。`NEW_GROUP_REQUEST`
(`0x32`) も同じ扱いである (§9.19.19)。

## モジュール構成

| モジュール | 概要 |
| --- | --- |
| `c4m` | C4M (CAT / CWT) のトークン発行 / 検証、bearer の接続束縛、`moqt` クレームの認可 |
| `c4m::cat` | CAT のクレーム / トークン / 発行ビルダー / bearer 検証 |
| `c4m::cbor` | CBOR (RFC 8949) の encode / decode |
| `c4m::cose` | COSE (RFC 9052) の構造とアルゴリズム定義 |
| `c4m::crypto` | 署名 / 検証 / ハッシュの trait と鍵表現 (aws-lc-rs 実装は feature) |
| `c4m::jwk` | JWK (RFC 7517) と JWK サムプリント (RFC 7638) |
| `c4m::jwt` | JWS compact (RFC 7515) の JWT (旧 JWT 形式の DPoP proof 用) |
| `c4m::dpop` | DPoP proof の検証と発行。旧 JWT 形式と draft-02 §4.2 の CWT 形式 |
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
| `playout` | 到着の揺らぎと実出力の先行量からの再生予算、鳴らす時刻の計画と確定、A/V 同期の遅延制御、音声と映像の共通の時間軸、表示時刻に合わせたフレームの選択、時間圧縮・伸長と欠落の補間、帯域制限のレート変換 |
| `session` | 1 本の `MOQT Transport Session` に閉じた sans I/O な状態機械で、control plane に加えて request stream / data stream / datagram の state も扱う |
| `stream` | data stream ヘッダ、`SubgroupStreamDecoder`、`FetchStreamDecoder`、`FetchStreamEncoder` |
| `subgroup_tracker` | Subgroup 再オープン禁止の検証ユーティリティ |
| `track_properties` | Track Properties の encode / decode |
| `varint` | 可変長整数 (vi64) の encode / decode |

内部専用の `kvp` モジュール (delta-key 骨格) は公開 API ではありません。

## アーキテクチャ

| 層 | 主なモジュール | 概要 |
| --- | --- | --- |
| Codec / helper | `varint`, `message`, `name`, `message_parameter`, `parameter`, `decoder`, `stream`, `loc`, `msf`, `object_properties`, `track_properties`, `grease`, `playout`, `media_clock`, `error`, `c4m` | ワイヤーフォーマットの encode / decode と検証、音声再生の予算・時刻決定と DSP の純粋な処理 |
| Stateful helper | `subgroup_tracker`, `session::auth_token_cache::AuthTokenCache`, `session::request_id::RequestIdGenerator`, `session::request_id::RequestIdTracker` | セッション実装で使う状態付き補助コンポーネント |
| Session | `session` | 1 本の `MOQT Transport Session` に閉じた endpoint-local な protocol state を管理する |

`session` は SETUP と Request ID 管理に加えて、SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS の bidi request stream を扱います。
uni data stream と datagram の送受信 state も扱います。
REQUEST_UPDATE / PUBLISH_DONE / STOP_SENDING、FETCH、GOAWAY ハンドシェイクと drain、delivery timeout などの deadline 管理 (`tick` 駆動) も行います。
一方で I/O、非同期処理、MoQ リレーが担う namespace 発見・告知と forwarding は含みません。

## セッション終了コードの分類

draft-ietf-moq-transport-23 §12.2 (Session Termination Codes) / §16.12.1
(Session Termination Error Codes) のコードを `error` モジュールで定義し、違反の原因を失わない
最も具体的なコードで `Session` を閉じる。汎用の `PROTOCOL_VIOLATION` (`0x3`) は、より具体的な
コードが定められていない違反にだけ使う。

| コード | 定数 | 主な発生源 |
| --- | --- | --- |
| `0x6` | `SESSION_MALFORMED_KEY_VALUE` | KVP の Length/Value が型の定義と一致しない (§8.3) |
| `0x16` | `SESSION_MALFORMED_AUTH_TOKEN` | Authorization Token の構造が不正 (§8.9) |
| `0x200` | `SESSION_INVALID_STREAM_HEADER` | 未知のストリーム型 / データグラム型、不正な Type Flags (§6.4.1 / §11) |
| `0x201` | `SESSION_MALFORMED_OBJECT` | Object の直列化を復号できない (§11.4.1 / §11.4.2.2) |
| `0x202` | `SESSION_INVALID_OBJECT_STATUS` | 未知の Object Status、非 Normal Status への Properties (§11.1.1 / §11.1.2) |
| `0x203` | `SESSION_TRUNCATED_OBJECT` | シリアライズ途中の Object での FIN (§11.3) |
| `0x300` | `SESSION_MALFORMED_MESSAGE` | 制御メッセージを解析できない、本文長が Length と一致しない (§9) |
| `0x301` | `SESSION_INVALID_FIELD` | 制御メッセージは解析できたがフィールド値が許容範囲外 (§9.3) |
| `0x302` | `SESSION_INVALID_TRACK_NAMESPACE` | 空の Track Namespace Field、32 超のフィールド (§8.7) |
| `0x303` | `SESSION_UNEXPECTED_MESSAGE` | 未知のメッセージ型、誤ったストリーム、順序違反、重複応答 (§6.3 / §9) |
| `0x304` | `SESSION_ROLE_VIOLATION` | peer の role では送れないメッセージ / フィールド (§9.1.4 / §9.2 / §9.4.2.1) |
| `0x305` | `SESSION_INVALID_PARAMETER` | Message Parameter の未知・重複・不許可・値域外 (§9.19) |
| `0x306` | `SESSION_CLOSED_CONTROL_STREAM` | 制御ストリームの閉鎖 (§6.3) |
| `0x400` | `SESSION_DELTA_ENCODING_OVERFLOW` | デルタエンコーディングの復号結果が u64 の範囲外 (§8.3 / §9.19 / §11) |
| `0x401` | `SESSION_FIELD_LENGTH_EXCEEDED` | フィールド / 構造が最大長を超える (§8.3 / §8.7 / §9) |

長さ超過とデルタ溢出は他の分類より優先する (§12.2 の "When more than one code applies, an
endpoint uses DELTA_ENCODING_OVERFLOW or FIELD_LENGTH_EXCEEDED in preference to the other
codes.")。各 codec は長さと溢出を他の検証より先に判定する。

未知の終了コードは `normalize_session_termination_code` が `INTERNAL_ERROR` (`0x1`) に正規化する
(§13 (Grease))。定義済みコードは正規化で変化しない。

## 要求・配送終了・stream reset のエラーコード

draft-ietf-moq-transport-23 §12.3 (Request Error Codes) / §16.12.2 (REQUEST_ERROR Codes)、
§12.4 (Publish Done Codes) / §16.12.3 (PUBLISH_DONE Codes)、§12.5 (Stream Reset Error Codes) /
§16.12.4 (Stream Reset Error Codes) のコードを `error` モジュールで定数として定義する
(`REQUEST_*` / `PUBLISH_DONE_*` / `STREAM_*`)。

- REQUEST_ERROR は `UNAVAILABLE` (`0x0A`、`REQUEST_UNAVAILABLE`) を持つ。応答側が現時点では
  要求を満たせないが後で満たせる可能性があることを表し、Retry Interval で再試行可能な時刻を
  示す (§12.3)
- PUBLISH_DONE は `EXPIRED_AUTH_TOKEN` (`0x7`、`PUBLISH_DONE_EXPIRED_AUTH_TOKEN`) を持つ。
  購読を認可したトークンの有効期限が切れたために購読を終える場合、publisher は本コードを
  `send_publish_done` に渡す (§9.9 の SHOULD、§12.4)
- Stream Reset は `EXPIRED_AUTH_TOKEN` (`0x7`、`STREAM_EXPIRED_AUTH_TOKEN`) を持ち、
  `DataStreamResetReason::ExpiredAuthToken` から選べる (§12.5)
- draft-23 は REQUEST_ERROR の `PREFIX_OVERLAP` (`0x30`) と Stream Reset の
  `UNKNOWN_OBJECT_STATUS` (`0x6`) をレジストリから削除した。どちらも既知コードとして定義せず、
  受信した場合は §13 (Grease) の未知コード規則に従って `normalize_request_error_code` /
  `normalize_stream_reset_code` がそれぞれの `INTERNAL_ERROR` に正規化する
- `DataStreamResetReason` は Stream Reset Error Code を型で固定する公開 enum であり、
  削除された `UNKNOWN_OBJECT_STATUS` に対応する variant を持たない

未知コードの正規化は文脈ごとの関数 (`normalize_request_error_code` /
`normalize_publish_done_code` / `normalize_stream_reset_code`) が担う。§13 (Grease) の
"Receipt of an unknown error code in any error context (Session Termination, REQUEST_ERROR,
PUBLISH_DONE, or Data Stream Reset) MUST be treated as equivalent to INTERNAL_ERROR for that
context." に従い、既知コードはそのまま返し、未知コードはその文脈の `INTERNAL_ERROR` を返す。

FETCH 応答で Object の status を判定できない場合は stream を reset せず、対象範囲を
End of Unknown Range (`0x10C`、§11.4.2 (End of Range)) で示して残りの既知 Object の配送を
続ける (§3.2.2 (Gaps in a Fetch Stream))。subscriber は End of Unknown Range の entry を
受理した後も同じ stream から後続の既知 Object を受け取る。

## GOAWAY によるセッション移行

`Session` は GOAWAY を移行の合図として扱い、接続の張り替え自体は I/O 層とアプリに任せます (Sans-I/O)。仕様と実装の責務は次のとおりです。

- 受信側: `GoawayReceived { new_session_uri, timeout, on_request_stream }` を発行し、`peer_goaway()` でも同じ情報を参照できます。control stream の GOAWAY は session 全体、request stream の GOAWAY は当該 request だけが対象です (`on_request_stream`)。control GOAWAY の受信後、自側からの
  新規 SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS は `SendRequestError::PeerGoawayReceived` で拒否されます (draft-ietf-moq-transport-23 §9.2 の SHOULD NOT)。
- 送信側: `send_goaway` / `send_goaway_on_request_stream` で送ります。client は空の New Session URI を送る MUST、URI は 8192 バイト以下です。二重受信は `UNEXPECTED_MESSAGE` (`0x303`)、client からの非空 URI は `ROLE_VIOLATION` (`0x304`)、8192 バイト超は `FIELD_LENGTH_EXCEEDED` (`0x401`) です。`tick(now_ms)` が期限を進め、
  open request が残ったまま期限を過ぎると `CloseSession(GOAWAY_TIMEOUT)` を積みます (control stream)。request stream の GOAWAY は期限で当該 request stream を `GOING_AWAY` で reset します。
- request stream の GOAWAY の role: 送れるのは request の receiver だけです (draft-ietf-moq-transport-23 §9.2 (GOAWAY) の "Only the receiver of a request can send GOAWAY on its request stream.")。
  request の sender (initiator) が `send_goaway_on_request_stream` を呼ぶと `PROTOCOL_VIOLATION` (`0x3`) で拒否し、イベントも期限 state も作りません。自側 state に request が無い場合も
  receiver であることを確認できないため同じく拒否します。受信側では、自側が receiver (応答側) のときに sender から GOAWAY を受け取ると、
  同節の "If the receiver of a request receives a GOAWAY on that request stream, it MUST close the session with a PROTOCOL_VIOLATION." に従って
  `PROTOCOL_VIOLATION` でセッションを閉じます。sender が receiver から受け取る GOAWAY は正当であり、`GoawayReceived { on_request_stream: Some(..) }` で通知します。
  破棄済みなどで sender / receiver を判定できない場合は §9.2 の MUST の条件が成立しないため、閉じずに受理して通知します。
- drain 完了: 自側が control GOAWAY を送ったあと、open request が無くなると `GoawayDrainReady` を 1 回発行します (`goaway_drain_ready()` でも参照可能)。アプリはこの時点で NO_ERROR で閉じるのが RECOMMENDED です (§6.6.1)。期限まで待つ必要はありません。
- example の client は受信した control GOAWAY で新 session をバックグラウンドで確立し、期限付きで
  再試行しながら購読 / 配信を張り直してから旧 session を NO_ERROR で閉じます。subscriber は
  移行中に届いた catalog を fill fetch stream から `CatalogState` へ適用し、音声は最後に受信した
  Location から再開します。publisher は移行先の c4m が変わったとき catalog の authInfo を
  再構築します。request stream の GOAWAY は session を維持し、空 URI (subscriber は現在と同じ
  URL を含む) なら当該 request を再発行します。非空 URI の単一 request 移行は 2 session の
  併走が必要なため未対応です。New Session URI が空なら現在の URI を再利用します。認可トークンは
  同一 origin のときだけ引き継ぎます (draft-ietf-moq-msf-01 §11.4.4)。

送信側の運用手順:

1. 移行先 (New Session URI) を受け付けられる状態にしてから GOAWAY を送る。
2. 新規 request は GOING_AWAY で拒否しつつ、既存の購読 / 配信は継続する。
3. `GoawayDrainReady` または `goaway_drain_ready()` を観測したら NO_ERROR で閉じる。
4. 期限内に drain できない場合は `GOAWAY_TIMEOUT` で閉じる (Session が `tick` で発行する `CloseSession` に従う)。
