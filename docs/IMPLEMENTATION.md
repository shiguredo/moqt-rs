# 実装状況

本ライブラリの対応仕様ごとの実装状況です。

| 仕様 | ドキュメント |
| --- | --- |
| Media over QUIC Transport (MOQT) | [`moqt.md`](moqt.md) |
| MOQT Streaming Format (MSF) | [`msf.md`](msf.md) |
| Low Overhead Container (LOC) | [`loc.md`](loc.md) |
| C4M (Common Access Token for MoQ) | [`c4m.md`](c4m.md) |

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
| `session` | 1 本の `MOQT Transport Session` に閉じた sans I/O な状態機械で、control plane に加えて request stream / data stream / datagram の state も扱う |
| `stream` | data stream ヘッダ、`SubgroupStreamDecoder`、`FetchStreamDecoder`、`FetchStreamEncoder` |
| `subgroup_tracker` | Subgroup 再オープン禁止の検証ユーティリティ |
| `track_properties` | Track Properties の encode / decode |
| `varint` | 可変長整数 (vi64) の encode / decode |

内部専用の `kvp` モジュール (delta-key 骨格) は公開 API ではありません。

## アーキテクチャ

| 層 | 主なモジュール | 概要 |
| --- | --- | --- |
| Codec / helper | `varint`, `message`, `name`, `message_parameter`, `parameter`, `decoder`, `stream`, `loc`, `msf`, `object_properties`, `track_properties`, `grease`, `error`, `c4m` | ワイヤーフォーマットの encode / decode と検証 |
| Stateful helper | `subgroup_tracker`, `session::auth_token_cache::AuthTokenCache`, `session::request_id::RequestIdGenerator`, `session::request_id::RequestIdTracker` | セッション実装で使う状態付き補助コンポーネント |
| Session | `session` | 1 本の `MOQT Transport Session` に閉じた endpoint-local な protocol state を管理する |

`session` は SETUP と Request ID 管理に加えて、SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS の bidi request stream を扱います。
uni data stream と datagram の送受信 state も扱います。
REQUEST_UPDATE / PUBLISH_DONE / STOP_SENDING、FETCH、GOAWAY ハンドシェイクと drain、delivery timeout などの deadline 管理 (`tick` 駆動) も行います。
一方で I/O、非同期処理、relay が担う namespace 発見・告知と forwarding は含みません。
