---
name: shiguredo-moqt
description: 時雨堂の Sans I/O MOQT ライブラリ shiguredo_moqt の機能・API リファレンス。draft-ietf-moq-transport-21 の制御メッセージ・セッション状態機械・data stream / datagram、LOC / MSF の codec、encode / decode やセッション駆動の実装に関する質問時に使用。
---

# shiguredo_moqt

draft-ietf-moq-transport-21 に基づく Sans I/O / no_std な Media over QUIC Transport (MOQT) ライブラリ。

## 特徴

- **Sans I/O**: I/O を完全に分離。Tokio / async-std / 同期 I/O など任意の環境で使用可能
- **no_std 対応**: `alloc` のみ必要 (`std` 非依存)
- **依存は 3 つのみ**: `hashbrown` / `noflate` / `nojson`
- **Session 状態機械**: 1 本の Transport Session に閉じた endpoint-local な状態管理
- **インクリメンタルデコード**: フレーム境界に依存せず受信バッファに蓄積しながらデコード
- **対応仕様**: MOQT `draft-21` / LOC `draft-04` / MSF `draft-01`

## バージョン情報

- crate 名: `shiguredo_moqt`
- バージョン: 2026.0.0
- Rust Edition: 2024
- 最小 Rust バージョン: 1.93
- ライセンス: Apache-2.0

**重要**: このライブラリは re-export を行わない。型は定義元モジュールから直接 import すること。

```rust
// OK
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::{SessionEvent, Transport};
use shiguredo_moqt::stream::subgroup::SubgroupHeader;
use shiguredo_moqt::message::common::TrackNamespace;

// NG (re-export は存在しない)
use shiguredo_moqt::session::Session;
use shiguredo_moqt::stream::SubgroupHeader;
```

## モジュール構成

| モジュール | 役割 |
| --- | --- |
| `message` | 制御メッセージ。`message::common` に `TrackNamespace` / `Location` |
| `message_parameter` | Message Parameters |
| `parameter` | SETUP Options |
| `decoder` | `MessageDecoder` (制御メッセージのインクリメンタルデコード) |
| `stream` | data stream / datagram のヘッダとデコーダ / エンコーダ |
| `session` | `session::core::Session` 状態機械 |
| `loc` | LOC Properties |
| `object_properties` | Object-scoped Properties |
| `track_properties` | Track Properties |
| `msf` | MSF Catalog / Timeline / URI |
| `varint` | 可変長整数 (vi64) |
| `name` | Namespace / Track Name の文字列表現 |
| `grease` | GREASE 値の生成・判定 |
| `subgroup_tracker` | Subgroup 再オープン禁止の検証 |
| `error` | エラー型とエラーコード定数 |

## クイックスタート

### 制御メッセージの encode / decode

```rust
use shiguredo_moqt::message::{ControlMessage, Setup};
use shiguredo_moqt::parameter::SetupOptions;

let msg = ControlMessage::Setup(Setup {
    options: SetupOptions::new(),
});
let bytes = msg.encode()?;

let (decoded, consumed) = ControlMessage::decode(&bytes)?;
```

### セッション

```rust
use shiguredo_moqt::parameter::SetupOptions;
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::{SessionEvent, Transport};

let mut session = Session::new_client(Transport::Quic, SetupOptions::new())?;
while let Some(event) = session.poll_event() {
    match event {
        SessionEvent::SendControl(msg) => {
            // 制御ストリームへ msg.encode()? を書き出す
        }
        SessionEvent::Established => {
            // 両側の SETUP が完了
        }
        SessionEvent::CloseSession(e) => {
            // QUIC CONNECTION_CLOSE 等で接続を閉じる
        }
        _ => {}
    }
}
```

## 制御メッセージ (`message`)

```rust
fn encode(&self) -> Result<Vec<u8>, MessageError>
fn decode(buf: &[u8]) -> Result<(ControlMessage, usize), MessageError>
```

`Type (varint) | Length (u16 big-endian) | Message Body` 形式。`decode` は `(メッセージ, 消費バイト数)` を返す。

主な variant:

| variant | 用途 |
| --- | --- |
| `Setup(Setup)` | セッション確立 |
| `Goaway(Goaway)` | セッション終了通知 |
| `RequestOk` / `RequestError` | 共通応答 (PUBLISH への PUBLISH_OK は `RequestOk` の別名) |
| `Subscribe` / `SubscribeOk` | SUBSCRIBE |
| `Publish` / `PublishDone` / `PublishSkipped` | PUBLISH |
| `PublishStateNotify` | 状態通知 |
| `RequestUpdate` | 更新要求 |
| `Fetch` / `FetchOk` | FETCH |
| `TrackStatus` | TRACK_STATUS |
| `PublishNamespace` | PUBLISH_NAMESPACE |
| `Namespace` / `NamespaceDone` | NAMESPACE 系 |
| `SubscribeNamespace` | SUBSCRIBE_NAMESPACE |
| `SubscribeTracks` | SUBSCRIBE_TRACKS |

補助型:

```rust
use shiguredo_moqt::message::common::{Location, TrackNamespace};

// TrackNamespace: フィールド数 0〜32、空フィールド禁止、合計 4096 バイト以下
fn new(fields: Vec<Vec<u8>>) -> Result<TrackNamespace, MessageError>
fn fields(&self) -> &[Vec<u8>]
fn is_session_level(&self) -> bool
fn is_single_period(&self) -> bool
fn byte_length(&self) -> usize

// Location: group_id / object_id の公開フィールドを持つ。Copy / Ord
pub struct Location {
    pub group_id: u64,
    pub object_id: u64,
}

// ReasonPhrase: 1024 バイト超でエラー
fn new(s: impl Into<String>) -> Result<ReasonPhrase, MessageError>
fn as_str(&self) -> &str
```

## インクリメンタルデコード (`decoder`)

```rust
use shiguredo_moqt::decoder::MessageDecoder;

let mut decoder = MessageDecoder::new();
decoder.push(&data);
if let Some(msg) = decoder.try_decode_message()? {
    // msg を Session::recv_control 等に渡す
}
```

```rust
fn new() -> MessageDecoder
fn push(&mut self, data: &[u8])
fn try_decode_message(&mut self) -> Result<Option<ControlMessage>, MessageError>
fn try_decode_varint(&mut self) -> Result<Option<u64>, MessageError>
```

- `Ok(Some)` = デコード成功・バッファから消費済み
- `Ok(None)` = データ不足 (追加の `push` が必要)
- `Err` = デコードエラー

`try_decode_varint` は受信 uni stream の先頭 stream type の読み取りに使う。

## セッション (`session`)

### 生成と基本操作

```rust
use shiguredo_moqt::parameter::SetupOptions;
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::Transport;

fn new_client(transport: Transport, options: SetupOptions) -> Result<Session, SessionError>
fn new_server(transport: Transport, options: SetupOptions) -> Result<Session, SessionError>
fn poll_event(&mut self) -> Option<SessionEvent>
fn tick(&mut self, now_ms: u64)
fn close(&mut self, code: u64, reason: &'static str)
fn state(&self) -> SessionState
fn role(&self) -> Role
fn transport(&self) -> Transport
```

`tick(now_ms)` には単調増加ミリ秒時刻を渡す。control message / data stream / GOAWAY のタイムアウトを評価し、満了時は `CloseSession` をキューへ積む。request stream 上の GOAWAY の timeout はセッションを閉じず、当該 request の `ResetRequestStream(GOING_AWAY)` を積む。

タイマーと Auth Token:

```rust
fn control_message_timeout_ms(&self) -> Option<u64>
fn set_control_message_timeout_ms(&mut self, timeout_ms: Option<u64>)
fn data_stream_timeout_ms(&self) -> Option<u64>
fn set_data_stream_timeout_ms(&mut self, timeout_ms: Option<u64>)
fn peer_auth_token_cache(&self) -> &AuthTokenCache
fn peer_max_auth_token_cache_size(&self) -> u64
fn peer_alias_retention_ms(&self) -> u64
fn set_peer_alias_retention_ms(&mut self, retention_ms: u64)
```

### 利用者が match / 引数で使う enum

```rust
pub enum Transport { Quic, WebTransport }
pub enum Role { Client, Server }
pub enum SessionState { LocalSetupSent, Established, Closing, Closed }
pub enum TrackRole { Publisher, Subscriber }
pub enum SubscriptionInitiator { Subscriber, Publisher }
pub enum SubscriptionState { Pending, Established, Terminated }
pub enum RequestKind {
    Subscribe, Publish, Fetch, TrackStatus,
    PublishNamespace, SubscribeNamespace, SubscribeTracks,
}
pub enum RequestStreamEnd {
    Fin,
    Reset { error_code: u64, reliable_size: Option<u64> },
}
pub enum FetchState { Pending, Established, Terminated }
pub enum NamespacePublicationState { Pending, Established, Terminated }
pub enum NamespaceSubscriptionState { Pending, Established, Terminated }
pub enum TrackSubscriptionState { Pending, Established, Terminated }
pub enum TrackDataAcceptance { Accepted, UnknownTrackAlias, Discarded, FilteredOut }
pub enum DatagramAcceptance { Object(TrackDataAcceptance), Padding }
```

`DataStreamResetReason` は `fn error_code(self) -> u64` で対応する Stream Reset Error Code を取得できる。

### `SessionEvent`

`poll_event()` が返すイベント。I/O 層はこれを受けて実際の送信・切断を行う。

| variant | 意味 |
| --- | --- |
| `SendControl(ControlMessage)` | 制御ストリームに書く |
| `SendRequest { request_id, message }` | 新規 bidi request stream を開いて書く |
| `SendOnStream { request_id, message, fin }` | 既存 bidi request stream に書く (`fin` で FIN) |
| `Established` | 両側 SETUP 完了 |
| `CloseSession(SessionError)` | 接続を閉じる |
| `RequestUpdateReceived { request_id, parameters }` | REQUEST_UPDATE 受信 |
| `PublishStateNotifyReceived { request_id, parameters }` | PUBLISH_STATE_NOTIFY 受信 |
| `RequestOkReceived { request_id, request_kind, parameters }` | REQUEST_OK 受信 |
| `FetchOkReceived { request_id, end_location, end_of_track }` | FETCH_OK 受信 |
| `PublishDoneReceived { request_id, status_code, stream_count, reason }` | PUBLISH_DONE 受信 |
| `RequestErrorReceived { request_id, error_code, retry_interval, reason, redirect }` | REQUEST_ERROR 受信 |
| `NamespaceReceived { request_id, suffix }` | NAMESPACE 受信 |
| `NamespaceDoneReceived { request_id, suffix }` | NAMESPACE_DONE 受信 |
| `PublishSkippedReceived { request_id, suffix, track_name }` | PUBLISH_SKIPPED 受信 |
| `SubscribeTracksReceived { request_id, prefix, parameters }` | SUBSCRIBE_TRACKS 受信 |
| `RequestTerminated { request_id, kind, reason }` | request が Terminated に遷移 |
| `GoawayReceived { new_session_uri, timeout, on_request_stream }` | GOAWAY 受信 |
| `SendPaddingStream { length }` | 指定長のパディングストリームを送る |
| `SendPaddingDatagram { length }` | 指定長のパディングデータグラムを送る |
| `ResetDataStream { stream_id, error_code, reliable_size }` | data stream を reset する |
| `OpenFillFetchStream { request_id }` | fill fetch stream を開く |
| `ResetRequestStream { request_id, error_code }` | request stream の送信方向を reset する |
| `StopSendingRequestStream { request_id, error_code }` | request stream の受信方向に STOP_SENDING を送る |

### 受信 API

```rust
// 制御ストリーム / bidi request stream
fn recv_control(&mut self, msg: ControlMessage) -> Result<(), SessionError>
fn recv_control_stream_type(&mut self, stream_type: u64) -> Result<(), SessionError>
fn recv_control_stream_closed(&mut self, end: RequestStreamEnd) -> Result<(), SessionError>
fn recv_request(&mut self, msg: ControlMessage) -> Result<(), RecvRequestError>
fn recv_request_stream_closed(&mut self, request_id: u64, end: RequestStreamEnd) -> Result<(), SessionError>
fn recv_stream_message(&mut self, request_id: u64, msg: ControlMessage) -> Result<(), SessionError>

// data stream / datagram
fn recv_data_stream_type(
    &mut self,
    stream_id: DataStreamId,
    stream_type: u64,
) -> Result<DataStreamType, RecvDataStreamError>
fn recv_data_stream_stop_sending(&mut self, stream_id: DataStreamId) -> Result<(), SessionError>
fn recv_subgroup_header(
    &mut self,
    stream_id: DataStreamId,
    header: &SubgroupHeader,
) -> Result<TrackDataAcceptance, SessionError>
fn recv_subgroup_object(
    &mut self,
    stream_id: DataStreamId,
    object: &DecodedSubgroupObject,
) -> Result<TrackDataAcceptance, SessionError>
fn recv_fetch_header(&mut self, stream_id: DataStreamId, header: &FetchHeader) -> Result<(), SessionError>
fn recv_fetch_entry(&mut self, stream_id: DataStreamId) -> Result<(), SessionError>
fn recv_fetch_data_stream_closed(&mut self, request_id: u64, end: RequestStreamEnd) -> Result<(), SessionError>
fn recv_datagram(&mut self, raw: &[u8]) -> Result<DatagramAcceptance, SessionError>
fn recv_object_datagram(&mut self, datagram: &ObjectDatagram) -> Result<TrackDataAcceptance, SessionError>
fn recv_padding_datagram(&mut self) -> Result<(), SessionError>
fn recv_data_stream_closed(&mut self, stream_id: DataStreamId, end: RequestStreamEnd) -> Result<(), SessionError>
fn report_mid_object_fin(&mut self, stream_id: DataStreamId) -> Result<(), SessionError>
```

`DataStreamId(pub u64)` は受信 uni data stream を呼び出し側が管理するための識別子。

`recv_subgroup_object()` は Object 単位のフィルタ再適用結果を `TrackDataAcceptance` で返す。
`FilteredOut` / `Discarded` の Object は Application へ渡さないが、wire 上の payload は
デコーダから読み出して消費する必要がある。

### 送信 API

```rust
// subscription
fn send_subscribe(
    &mut self,
    track_namespace: TrackNamespace,
    track_name: Vec<u8>,
    parameters: MessageParameters,
) -> Result<u64, SendRequestError>
fn send_publish(
    &mut self,
    track_namespace: TrackNamespace,
    track_name: Vec<u8>,
    track_alias: u64,
    parameters: MessageParameters,
    track_properties: TrackProperties,
) -> Result<u64, SendRequestError>
fn send_subscribe_ok(
    &mut self,
    request_id: u64,
    track_alias: u64,
    parameters: MessageParameters,
    track_properties: TrackProperties,
) -> Result<(), SessionError>
fn send_request_update(&mut self, request_id: u64, parameters: MessageParameters) -> Result<(), SessionError>
fn send_publish_state_notify(&mut self, request_id: u64, parameters: MessageParameters) -> Result<(), SessionError>
fn send_publish_done(
    &mut self,
    request_id: u64,
    status_code: u64,
    stream_count: u64,
    reason: ReasonPhrase,
) -> Result<(), SessionError>

// FETCH
fn send_fetch(
    &mut self,
    track_namespace: TrackNamespace,
    track_name: Vec<u8>,
    parameters: MessageParameters,
) -> Result<u64, SendRequestError>
fn send_fetch_ok(
    &mut self,
    request_id: u64,
    end_of_track: u8,
    end_location: Location,
    parameters: MessageParameters,
    track_properties: TrackProperties,
) -> Result<(), SessionError>

// namespace / TRACK_STATUS
fn send_publish_namespace(
    &mut self,
    track_namespace: TrackNamespace,
    parameters: MessageParameters,
) -> Result<u64, SendRequestError>
fn send_subscribe_namespace(
    &mut self,
    prefix: TrackNamespace,
    parameters: MessageParameters,
) -> Result<u64, SendRequestError>
fn send_namespace(&mut self, request_id: u64, suffix: TrackNamespace) -> Result<(), SessionError>
fn send_namespace_done(&mut self, request_id: u64, suffix: TrackNamespace) -> Result<(), SessionError>
fn send_subscribe_tracks(
    &mut self,
    prefix: TrackNamespace,
    parameters: MessageParameters,
) -> Result<u64, SendRequestError>
fn send_publish_skipped(
    &mut self,
    request_id: u64,
    suffix: TrackNamespace,
    track_name: Vec<u8>,
) -> Result<(), SessionError>
fn send_track_status(
    &mut self,
    track_namespace: TrackNamespace,
    track_name: Vec<u8>,
    parameters: MessageParameters,
) -> Result<u64, SendRequestError>

// 共通応答
fn send_request_ok(
    &mut self,
    request_id: u64,
    parameters: MessageParameters,
    track_properties: TrackProperties,
) -> Result<(), SessionError>
fn send_request_error(
    &mut self,
    request_id: u64,
    error_code: u64,
    retry_interval: u64,
    reason: ReasonPhrase,
    redirect: Option<Redirect>,
) -> Result<(), SessionError>
```

### data plane 送信 API

```rust
fn send_subgroup_header(
    &mut self,
    stream_id: DataStreamId,
    request_id: u64,
    header: &SubgroupHeader,
) -> Result<(), SessionError>
fn send_subgroup_object(
    &mut self,
    stream_id: DataStreamId,
    object_id: u64,
    properties_bytes: Option<&[u8]>,
) -> Result<(), SendRequestError>
fn send_data_stream_closed(&mut self, stream_id: DataStreamId, end: RequestStreamEnd) -> Result<(), SessionError>
fn send_fetch_header(&mut self, stream_id: DataStreamId, request_id: u64) -> Result<(), SessionError>
fn send_fill_fetch_header(&mut self, stream_id: DataStreamId, subscription_request_id: u64) -> Result<(), SessionError>
fn send_fetch_object(&mut self, stream_id: DataStreamId) -> Result<(), SessionError>
fn send_fetch_data_stream_closed(&mut self, stream_id: DataStreamId) -> Result<(), SessionError>
fn send_object_datagram(
    &mut self,
    request_id: u64,
    group_id: u64,
    object_id: u64,
    properties_data: Option<Vec<u8>>,
    status: Option<u64>,
) -> Result<(), SendRequestError>
fn send_data_stream_stop_sending(&mut self, stream_id: DataStreamId) -> Result<(), SessionError>
fn reset_outgoing_data_stream(
    &mut self,
    stream_id: DataStreamId,
    reason: DataStreamResetReason,
) -> Result<(), SessionError>
```

### 公開定数

| 定数 | 値 | 説明 |
| --- | --- | --- |
| `PUBLISH_DONE_STREAM_COUNT_UNKNOWN` | `u64::MAX` | Stream Count が不明な場合の sentinel |
| `DEFAULT_PEER_ALIAS_RETENTION_MS` | `5000` | キャンセル済み peer publisher alias の tombstone 保持期間 (ms) |
| `PUBLISHER_PRIORITY_DEFAULT` | `128` | DEFAULT_PUBLISHER_PRIORITY の既定値 |
| `DEFAULT_PUBLISHER_GROUP_ORDER_ASCENDING` | `0x1` | DEFAULT_PUBLISHER_GROUP_ORDER の既定値 |
| `MAX_NEW_SESSION_URI_LENGTH` | `8192` | GOAWAY の New Session URI 最大長 |

### エラー型

```rust
pub struct SessionError {
    pub code: u64,
    pub reason: &'static str,
}

pub enum SendRequestError {
    PeerGoawayReceived,
    LocalFilterMismatch,
    LocalDatagramTimeout,
    Session(SessionError),
}

pub enum RecvRequestError {
    BeforeSessionEstablished,
    Session(SessionError),
}

pub enum RecvDataStreamError {
    BeforeSessionEstablished,
    InvalidInput(SessionError),
    Session(SessionError),
}
```

- `SendRequestError` は `as_session_error()` で `SessionError` を取り出せる (`PeerGoawayReceived` / `LocalFilterMismatch` / `LocalDatagramTimeout` は `None`)。これらは wire コードを持たないため公開 API に渡す必要はない (渡しても各レジストリの `*_INTERNAL_ERROR` に置換される)
- `RecvRequestError` / `RecvDataStreamError` は `BeforeSessionEstablished` なら session state に影響しない

## data stream / datagram (`stream`)

### 受信ストリーム種別

```rust
use shiguredo_moqt::stream::DataStreamType;

pub enum DataStreamType { Fetch, Subgroup, Padding }
```

```rust
fn classify_data_stream_type(type_id: u64) -> Option<DataStreamType>
```

### ヘッダ型

```rust
use shiguredo_moqt::stream::subgroup::{SubgroupHeader, SubgroupIdMode, SubgroupObject};
use shiguredo_moqt::stream::fetch::{
    FetchHeader, FetchPriorContext, FetchSubgroupIdMode, FetchStreamEntry, FetchStreamObject,
};
use shiguredo_moqt::stream::datagram::ObjectDatagram;

pub enum SubgroupIdMode { Zero, FirstObjectId, Explicit(u64) }
pub enum FetchSubgroupIdMode { Zero, PreviousSame, PreviousPlusOne, Explicit(u64) }
pub enum FetchPriorContext { First, NoPriorActualObject, HasPriorObject }
```

各ヘッダは `fn encode(&self) -> Vec<u8>` / `fn decode(buf: &[u8]) -> Result<(Self, usize), MessageError>` を持つ。`ObjectDatagram::decode` は 1 datagram = 1 バッファを前提とする。

`ObjectDatagram::properties_data` と `send_object_datagram` の `properties_data` は、Properties Length varint を含む生バイト列である (`SubgroupObject` / `FetchStreamObject` と同じ規約)。Datagram では Properties Length = 0 はプロトコル違反であり、encode / 送信 API が拒否する。

### デコーダ / エンコーダ

```rust
use shiguredo_moqt::stream::decoder::{SubgroupStreamDecoder, FetchStreamDecoder};
use shiguredo_moqt::stream::encoder::{FetchStreamEncoder, FetchObjectInput};

// SubgroupStreamDecoder
fn new() -> SubgroupStreamDecoder
fn push(&mut self, data: &[u8])
fn try_decode_header(&mut self) -> Result<Option<SubgroupHeader>, MessageError>
fn try_decode_object(&mut self) -> Result<Option<DecodedSubgroupObject>, MessageError>
fn try_read_payload(&mut self) -> Option<Vec<u8>>
fn consume_payload(&mut self, length: u64) -> Result<(), MessageError>
fn resolved_subgroup_id(&self) -> Option<u64>
fn finish(&self) -> Result<(), MessageError>

// FetchStreamDecoder
fn new() -> FetchStreamDecoder
fn new_with_group_order(group_order: u8) -> Result<FetchStreamDecoder, MessageError>
fn set_subgroup_final_object(
    &mut self,
    group_id: u64,
    subgroup_id: u64,
    final_object_id: u64,
) -> Result<(), MessageError>
fn push(&mut self, data: &[u8])
fn try_decode_header(&mut self) -> Result<Option<FetchHeader>, MessageError>
fn try_decode_entry(&mut self) -> Result<Option<DecodedFetchEntry>, MessageError>
fn try_read_payload(&mut self) -> Option<Vec<u8>>
fn consume_payload(&mut self, length: u64) -> Result<(), MessageError>
fn finish(&self) -> Result<(), MessageError>

// FetchStreamEncoder
fn new(request_id: u64) -> FetchStreamEncoder
fn encode_header(&self) -> Vec<u8>
fn encode_object(
    &mut self,
    input: &FetchObjectInput,
    properties_data: Option<&[u8]>,
    buf: &mut Vec<u8>,
) -> Result<(), MessageError>
fn encode_end_of_non_existent_range(&mut self, group_id: u64, object_id: u64, buf: &mut Vec<u8>) -> Result<(), MessageError>
fn encode_end_of_unknown_range(&mut self, group_id: u64, object_id: u64, buf: &mut Vec<u8>) -> Result<(), MessageError>
fn encode_end_of_timed_out_range(&mut self, group_id: u64, object_id: u64, buf: &mut Vec<u8>) -> Result<(), MessageError>
```

- `SubgroupStreamDecoder` は `try_decode_object` / `consume_payload`、`FetchStreamDecoder` は `try_decode_entry` / `consume_payload` とメソッド名が異なる
- ペイロード本体はデコーダの責務外。`payload_length` バイト分を読み出したら `consume_payload` を呼ぶ
- `FetchStreamEncoder` は前回オブジェクトとの差分 (デルタ圧縮) を内部状態で判断する。公開コンストラクタは Ascending 固定
- `FetchStreamEncoder` はペイロードを出力しない。返したバイト列の後ろに呼び出し側がペイロードを結合する

## SETUP Options (`parameter`)

```rust
use shiguredo_moqt::parameter::{SetupOption, SetupOptionValue, SetupOptions};

pub struct SetupOptions(/* Vec<SetupOption> */);

fn new() -> SetupOptions
fn push(&mut self, p: SetupOption)
fn len(&self) -> usize
fn is_empty(&self) -> bool
fn encode(&self, buf: &mut Vec<u8>) -> Result<(), MessageError>
fn decode(buf: &[u8]) -> Result<(SetupOptions, usize), MessageError>

// アクセサ
fn path(&self) -> Option<&[u8]>
fn authorization_tokens(&self) -> Vec<&AuthorizationToken>
fn max_auth_token_cache_size(&self) -> Option<u64>
fn authority(&self) -> Option<&[u8]>
fn max_request_updates(&self) -> Option<u64>
fn max_filter_ranges(&self) -> Option<u64>

pub struct SetupOption {
    pub option_type: u64,
    pub value: SetupOptionValue,
}

pub enum SetupOptionValue {
    VarInt(u64),
    Bytes(Vec<u8>),
    AuthorizationToken(AuthorizationToken),
}
```

Setup Option 型定数:

| 定数 | 値 |
| --- | --- |
| `SETUP_OPTION_PATH` | `0x01` |
| `SETUP_OPTION_AUTHORIZATION_TOKEN` | `0x03` |
| `SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE` | `0x04` |
| `SETUP_OPTION_AUTHORITY` | `0x05` |
| `SETUP_OPTION_MAX_FILTER_RANGES` | `0x06` |
| `SETUP_OPTION_MOQT_IMPLEMENTATION` | `0x07` |
| `SETUP_OPTION_MAX_REQUEST_UPDATES` | `0x08` |

## Message Parameters (`message_parameter`)

```rust
use shiguredo_moqt::message_parameter::{
    AuthorizationToken, MessageParameter, MessageParameters, MessageParameterValue,
};

pub struct MessageParameter {
    pub param_type: u64,
    pub value: MessageParameterValue,
}

pub enum MessageParameterValue {
    Uint8(u8),
    VarInt(u64),
    Location { group: u64, object: u64 },
    LengthPrefixed(Vec<u8>),
    FillParameters(MessageParameters),
    AuthorizationToken(AuthorizationToken),
    TrackNamespacePrefix(TrackNamespace),
}

pub struct MessageParameters(/* Vec<MessageParameter> */);

fn new() -> MessageParameters
fn push(&mut self, p: MessageParameter)
fn len(&self) -> usize
fn is_empty(&self) -> bool
fn as_slice(&self) -> &[MessageParameter]
fn encode(&self, buf: &mut Vec<u8>) -> Result<(), MessageError>
fn decode(buf: &[u8]) -> Result<(MessageParameters, usize), MessageError>
```

主なアクセサ:

```rust
fn object_delivery_timeout(&self) -> Option<u64>            // 0x02
fn subgroup_delivery_timeout(&self) -> Option<u64>          // 0x06
fn expires(&self) -> Option<u64>                            // 0x08
fn largest_object(&self) -> Option<(u64, u64)>              // 0x09
fn fill_timeout(&self) -> Option<u64>                       // 0x0A
fn forward(&self) -> Option<u8>                             // 0x10
fn subscriber_priority(&self) -> Option<u8>                 // 0x20
fn location_filter(&self) -> Option<&[u8]>                  // 0x21 (生バイト)
fn location_filter_typed(&self) -> Result<Option<LocationFilter>, MessageError>
fn group_order(&self) -> Option<u8>                         // 0x22
fn new_group_request(&self) -> Option<u64>                  // 0x32
fn track_namespace_prefix(&self) -> Option<&TrackNamespace> // 0x34
fn include_properties(&self) -> Option<u8>                  // 0x35
```

主なパラメータ型定数:

| 定数 | 値 |
| --- | --- |
| `PARAM_OBJECT_DELIVERY_TIMEOUT` | `0x02` |
| `PARAM_AUTHORIZATION_TOKEN` | `0x03` |
| `PARAM_RENDEZVOUS_TIMEOUT` | `0x04` |
| `PARAM_SUBGROUP_DELIVERY_TIMEOUT` | `0x06` |
| `PARAM_EXPIRES` | `0x08` |
| `PARAM_LARGEST_OBJECT` | `0x09` |
| `PARAM_FILL_TIMEOUT` | `0x0A` |
| `PARAM_FORWARD` | `0x10` |
| `PARAM_SUBSCRIBER_PRIORITY` | `0x20` |
| `PARAM_LOCATION_FILTER` | `0x21` |
| `PARAM_GROUP_ORDER` | `0x22` |
| `PARAM_FILL_PARAMETERS` | `0x23` |
| `PARAM_SUBGROUP_FILTER` | `0x25` |
| `PARAM_OBJECTID_FILTER` | `0x26` |
| `PARAM_PRIORITY_FILTER` | `0x27` |
| `PARAM_OBJECT_PROPERTY_FILTER` | `0x28` |
| `PARAM_TRACK_PROPERTY_FILTER` | `0x29` |
| `PARAM_NEW_GROUP_REQUEST` | `0x32` |
| `PARAM_TRACK_NAMESPACE_PREFIX` | `0x34` |
| `PARAM_INCLUDE_PROPERTIES` | `0x35` |

`AuthorizationToken` は `Delete` / `Register` / `UseAlias` / `UseValue` の variant を持つ。各メソッドは `pub(crate)` のため、利用側は variant を直接構築して `MessageParameterValue` に格納する。

## LOC Properties (`loc`)

```rust
use shiguredo_moqt::loc::{LocProperties, LocProperty, LocPropertyValue};

pub enum LocPropertyValue {
    VarInt(u64),
    Bytes(Vec<u8>),
}

pub struct LocProperty {
    pub prop_id: u64,
    pub value: LocPropertyValue,
}

fn new() -> LocProperties
fn push(&mut self, prop: LocProperty)
fn iter(&self) -> impl Iterator<Item = &LocProperty>
fn encode(&self) -> Result<Vec<u8>, MessageError>
fn decode(buf: &[u8]) -> Result<(LocProperties, usize), MessageError>

// アクセサ
fn timestamp(&self) -> Option<u64>
fn timescale(&self) -> Option<u64>
fn video_frame_marking(&self) -> Option<&[u8]>
fn audio_level(&self) -> Option<u64>
fn video_config(&self) -> Option<&[u8]>
fn audio_config(&self) -> Option<&[u8]>
```

| 定数 | 値 |
| --- | --- |
| `PROP_TIMESTAMP` | `0x10` |
| `PROP_TIMESCALE` | `0x08` |
| `PROP_VIDEO_FRAME_MARKING` | `0x09` |
| `PROP_AUDIO_LEVEL` | `0x0C` |
| `PROP_VIDEO_CONFIG` | `0x0D` |
| `PROP_AUDIO_CONFIG` | `0x0F` |

偶数 ID は varint 値、奇数 ID は長さ付きバイト列。

## Object / Track Properties

### `object_properties`

```rust
use shiguredo_moqt::object_properties::{
    ObjectProperties, ObjectProperty, ObjectPropertyValue, ObjectPropertyTracker,
    ObjectFieldTracker,
};

fn new() -> ObjectProperties
fn push(&mut self, prop: ObjectProperty)
fn encode(&self, buf: &mut Vec<u8>) -> Result<(), MessageError>
fn decode(buf: &[u8]) -> Result<(ObjectProperties, usize), MessageError>

// アクセサ
fn object_delivery_timeout(&self) -> Option<u64>
fn subgroup_delivery_timeout(&self) -> Option<u64>
fn prior_group_id_gap(&self) -> Option<u64>
fn prior_object_id_gap(&self) -> Option<u64>
fn immutable_properties(&self) -> Option<&[u8]>
fn find_varint(&self, prop_type: u64) -> Option<u64>
```

| 定数 | 値 |
| --- | --- |
| `PROP_PRIOR_GROUP_ID_GAP` | `0x3C` |
| `PROP_PRIOR_OBJECT_ID_GAP` | `0x3E` |

`PROP_OBJECT_DELIVERY_TIMEOUT` / `PROP_SUBGROUP_DELIVERY_TIMEOUT` / `PROP_IMMUTABLE_PROPERTIES` は `track_properties` から import する。

### `track_properties`

```rust
use shiguredo_moqt::track_properties::{
    TrackProperties, TrackProperty, TrackPropertyValue,
};

fn new() -> TrackProperties
fn push(&mut self, p: TrackProperty)
fn encode(&self, buf: &mut Vec<u8>) -> Result<(), MessageError>
fn decode(buf: &[u8]) -> Result<TrackProperties, MessageError>

// アクセサ
fn dynamic_groups(&self) -> Option<u64>
fn default_publisher_priority(&self) -> Option<u8>
fn default_publisher_group_order(&self) -> Option<u8>
fn object_delivery_timeout(&self) -> Option<u64>
fn subgroup_delivery_timeout(&self) -> Option<u64>
fn has_unknown_mandatory(&self) -> bool
```

| 定数 | 値 |
| --- | --- |
| `PROP_OBJECT_DELIVERY_TIMEOUT` | `0x02` |
| `PROP_MAX_CACHE_DURATION` | `0x04` |
| `PROP_SUBGROUP_DELIVERY_TIMEOUT` | `0x06` |
| `PROP_IMMUTABLE_PROPERTIES` | `0x0B` |
| `PROP_DEFAULT_PUBLISHER_PRIORITY` | `0x0E` |
| `PROP_DEFAULT_PUBLISHER_GROUP_ORDER` | `0x22` |
| `PROP_DYNAMIC_GROUPS` | `0x30` |

`TrackProperties::decode` はバッファ全体を読み、`(Self, usize)` ではなく `Self` を返す点に注意。

## MSF (`msf`)

### Catalog

```rust
use shiguredo_moqt::msf::{
    MsfCatalog, MsfCatalogDocument, MsfDeltaOperation, MsfDeltaUpdate,
    MsfTrack, MsfCloneTrack, MsfRemoveTrack, MsfInitData, MsfBuffers, MsfPackaging,
    MSF_CATALOG_TRACK_NAME, MSF_VERSION,
};

// Full / Delta を自動判別
let doc = MsfCatalogDocument::decode(&bytes)?;
let bytes = doc.encode()?;

pub enum MsfCatalogDocument {
    Full(MsfCatalog),
    Delta(MsfDeltaUpdate),
}

fn apply_delta(
    &mut self,
    delta: &MsfDeltaUpdate,
    catalog_namespace: Option<&str>,
) -> Result<(), MessageError>
```

`MsfTrack` は `name` / `namespace` / `packaging` / `codec` / `mime_type` / `width` / `height` / `framerate` / `bitrate` / `samplerate` / `channel_config` などを持つ単一構造体。Codec / Video / Audio は draft 上の論理的な分類。

### Timeline

```rust
use shiguredo_moqt::msf::{
    TimelineEncodingOptions, MsfMediaTimeline, MsfEventTimeline,
    encode_media_timeline, decode_media_timeline,
    encode_event_timeline, decode_event_timeline,
};

pub struct TimelineEncodingOptions { pub gzip: bool }

fn encode_media_timeline(
    timeline: &MsfMediaTimeline,
    options: TimelineEncodingOptions,
) -> Result<Vec<u8>, MessageError>
fn decode_media_timeline(data: &[u8]) -> Result<MsfMediaTimeline, MessageError>
fn encode_event_timeline(
    timeline: &MsfEventTimeline,
    options: TimelineEncodingOptions,
) -> Result<Vec<u8>, MessageError>
fn decode_event_timeline(data: &[u8]) -> Result<MsfEventTimeline, MessageError>
```

`decode_*` は先頭の gzip magic (`0x1F 0x8B`) を検出すると自動展開する。

### URI

```rust
use shiguredo_moqt::msf::uri::{parse_msf_uri, parse_msf_fragment};

fn parse_msf_uri(uri: &str) -> Result<MsfUri, MessageError>
fn parse_msf_fragment(fragment: &str) -> Result<MsfFragment, MessageError>
```

`MsfFragment` は `parameter_values` / `connection_types` / `wallclock_ranges` / `mediatime_ranges` / `location_ranges` / `c4m_tokens` のアクセサを持つ。

## その他

```rust
// varint
use shiguredo_moqt::varint;
fn encode(val: u64, buf: &mut Vec<u8>)
fn decode(buf: &[u8]) -> Result<(u64, usize), MessageError>
fn checked_len(len: u64, remaining: usize) -> Result<usize, MessageError>
fn encoded_len(val: u64) -> usize

// name
use shiguredo_moqt::name::{parse_name, serialize_name};
fn serialize_name(namespace: &TrackNamespace, track_name: &[u8]) -> String
fn parse_name(s: &str) -> Result<(TrackNamespace, Vec<u8>), NameParseError>

// grease
use shiguredo_moqt::grease::{generate, is_grease};
fn generate(n: u64) -> Option<u64>
fn is_grease(value: u64) -> bool

// subgroup_tracker
use shiguredo_moqt::subgroup_tracker::SubgroupTracker;
fn open(&mut self, track_alias: u64, group_id: u64, subgroup_id: u64) -> Result<(), SessionError>
fn mark_fin(&mut self, track_alias: u64, group_id: u64, subgroup_id: u64, last_object_id: Option<u64>) -> Result<(), SessionError>
fn mark_reset(&mut self, track_alias: u64, group_id: u64, subgroup_id: u64, reliable_size: Option<u64>)
fn mark_stop_sending(&mut self, track_alias: u64, group_id: u64, subgroup_id: u64)
fn get(&self, track_alias: u64, group_id: u64, subgroup_id: u64) -> Option<&SubgroupStreamState>
```

`MessageError` は `Clone` 不可。後段へ持ち回る場合は再構築を検討する。

## 準拠仕様

- draft-ietf-moq-transport-21 - Media over QUIC Transport
  - <https://datatracker.ietf.org/doc/html/draft-ietf-moq-transport-21>
- draft-ietf-moq-loc-04 - Media over QUIC - Low Overhead Container
  - <https://datatracker.ietf.org/doc/html/draft-ietf-moq-loc-04>
- draft-ietf-moq-msf-01 - MOQT Streaming Format
  - <https://datatracker.ietf.org/doc/html/draft-ietf-moq-msf-01>

## 注意事項

- 型は定義元モジュールから import する。re-export は存在しない
- `Session` は sans-I/O の状態機械であり、I/O・非同期処理・relay 固有の routing / fan-out / cache / policy は含まない。`poll_event()` の結果を I/O 層へ渡すのは利用側の責務
- `SendRequestError` のローカルエラー (`PeerGoawayReceived` / `LocalFilterMismatch` / `LocalDatagramTimeout`) は wire コードを持たない。wire コードとして公開 API に渡す必要はない (渡しても各レジストリの `*_INTERNAL_ERROR` に置換される)
- `ControlMessage` と各メッセージ構造体は `Clone` だが `Copy` ではない
- `MessageError` は `Clone` / `Copy` 不可
- 開発中のライブラリであり、仕様は積極的に変更される場合がある
