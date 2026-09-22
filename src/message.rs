//! MOQT コントロールメッセージ (draft-ietf-moq-transport-21 §9 (Control Messages))
//!
//! ワイヤーフォーマット: Type (varint) | Length (u16 big-endian) | Message Body

pub mod common;

use common::{
    Location, TrackNamespace, decode_track_name, encode_track_name, validate_full_track_name,
};

use crate::{
    error::{MessageError, REQUEST_REDIRECT},
    message_parameter::{
        MessageParameters, PARAM_AUTHORIZATION_TOKEN, PARAM_EXPIRES, PARAM_FILL_PARAMETERS,
        PARAM_FILL_TIMEOUT, PARAM_FORWARD, PARAM_GROUP_ORDER, PARAM_INCLUDE_PROPERTIES,
        PARAM_LARGEST_OBJECT, PARAM_LOCATION_FILTER, PARAM_NEW_GROUP_REQUEST,
        PARAM_OBJECT_DELIVERY_TIMEOUT, PARAM_OBJECT_PROPERTY_FILTER, PARAM_OBJECTID_FILTER,
        PARAM_PRIORITY_FILTER, PARAM_RENDEZVOUS_TIMEOUT, PARAM_SUBGROUP_DELIVERY_TIMEOUT,
        PARAM_SUBGROUP_FILTER, PARAM_SUBSCRIBER_PRIORITY, PARAM_TRACK_NAMESPACE_PREFIX,
        PARAM_TRACK_PROPERTY_FILTER,
    },
    parameter::SetupOptions,
    track_properties::TrackProperties,
    varint,
};
use alloc::{
    string::{String, ToString},
    vec::Vec,
};

// ─── メッセージ型定数 ────────────────────────────────────────

const MSG_REQUEST_UPDATE: u64 = 0x02;
const MSG_SUBSCRIBE: u64 = 0x03;
const MSG_SUBSCRIBE_OK: u64 = 0x04;
const MSG_REQUEST_ERROR: u64 = 0x05;
const MSG_REQUEST_OK: u64 = 0x07;
const MSG_PUBLISH_DONE: u64 = 0x0B;
const MSG_TRACK_STATUS: u64 = 0x0D;
const MSG_GOAWAY: u64 = 0x10;
const MSG_FETCH: u64 = 0x16;
const MSG_FETCH_OK: u64 = 0x18;
const MSG_PUBLISH: u64 = 0x1D;
const MSG_PUBLISH_STATE_NOTIFY: u64 = 0x22;
const MSG_SETUP: u64 = 0x2F00; // src/stream.rs の SETUP_STREAM_TYPE と同じ値 (異なる名前空間)
// relay 専用の namespace 発見・告知機構 (本ライブラリは実装しないが Table 5 に定義済み)
const MSG_PUBLISH_NAMESPACE: u64 = 0x06;
const MSG_NAMESPACE: u64 = 0x08;
const MSG_NAMESPACE_DONE: u64 = 0x0E;
const MSG_PUBLISH_SKIPPED: u64 = 0x0F;
const MSG_SUBSCRIBE_NAMESPACE: u64 = 0x50;
const MSG_SUBSCRIBE_TRACKS: u64 = 0x51;

// ─── パラメータスコープテーブル (draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope)) ────

/// SUBSCRIBE で許可されるパラメータ型 (draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope))
///
/// 送信側の `send_subscribe` (src/session/subscription/send.rs) が request_id 発行前に
/// スコープ検証するために `pub(crate)` で公開している。
pub(crate) const SUBSCRIBE_ALLOWED_PARAMS: &[u64] = &[
    PARAM_AUTHORIZATION_TOKEN,
    // draft-ietf-moq-transport-21 §9.20.7 (RENDEZVOUS TIMEOUT Parameter):
    // "The RENDEZVOUS_TIMEOUT parameter (Parameter Type 0x04) MAY appear in a SUBSCRIBE
    // message." 本ライブラリは値を解釈しないが、定義済みパラメータのため受理する。
    PARAM_RENDEZVOUS_TIMEOUT,
    PARAM_OBJECT_DELIVERY_TIMEOUT,
    PARAM_SUBGROUP_DELIVERY_TIMEOUT,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_FORWARD,
    PARAM_LOCATION_FILTER,
    PARAM_GROUP_ORDER,
    PARAM_NEW_GROUP_REQUEST,
    PARAM_INCLUDE_PROPERTIES,
    PARAM_FILL_PARAMETERS,
    PARAM_SUBGROUP_FILTER,
    PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER,
    PARAM_OBJECT_PROPERTY_FILTER,
];

/// SUBSCRIBE_OK で許可されるパラメータ型 (draft-ietf-moq-transport-21 §9.20.17 (EXPIRES Parameter) /
/// §9.20.18 (LARGEST OBJECT Parameter) の MAY appear 列挙)
///
/// 送信側の `send_subscribe_ok` (src/session/subscription/send.rs) が状態遷移前に
/// スコープ検証するために `pub(crate)` で公開している。
pub(crate) const SUBSCRIBE_OK_ALLOWED_PARAMS: &[u64] = &[PARAM_EXPIRES, PARAM_LARGEST_OBJECT];

/// REQUEST_OK で許可されるパラメータ型 (draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope) /
/// §9.20.3 以降の各 Parameter 節 "MAY appear in" 定義から導出: AUTHORIZATION_TOKEN は非許可)
///
/// これは codec 層が REQUEST_OK のワイヤ妥当性検証に用いる許可集合である。
/// draft-ietf-moq-transport-21 §9.20 の各 "MAY appear in" で REQUEST_OK に出現しうるのは
/// `EXPIRES` / `LARGEST_OBJECT` のみだが、応答 context ごとの厳密な検証はセッション層が
/// `*_OK_ALLOWED_PARAMS` で行うため、codec 層は REQUEST_UPDATE 系のパラメータも含めて
/// 意図的に広く受理する。狭めるとデコード層エラーパスが変わる。
const REQUEST_OK_ALLOWED_PARAMS: &[u64] = &[
    PARAM_OBJECT_DELIVERY_TIMEOUT,
    PARAM_SUBGROUP_DELIVERY_TIMEOUT,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_LOCATION_FILTER,
    PARAM_EXPIRES,
    PARAM_LARGEST_OBJECT,
    PARAM_FORWARD,
    PARAM_NEW_GROUP_REQUEST,
    PARAM_SUBGROUP_FILTER,
    PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER,
    PARAM_OBJECT_PROPERTY_FILTER,
];

// ─── REQUEST_OK の応答 context 別許可パラメータ集合 ─────────────────
//
// draft-ietf-moq-transport-21 §9.3 (REQUEST_OK): REQUEST_OK (Type 0x07) は PUBLISH_OK /
// REQUEST_UPDATE_OK / TRACK_STATUS_OK が共有する単一ワイヤメッセージ。
// draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope) は許可されない context に出現したパラメータの受信を
// PROTOCOL_VIOLATION でクローズすることを MUST で要求する。各集合は draft-ietf-moq-transport-21 §9.20.3 以降の各 Parameter 節の
// "MAY appear in" スコープ定義から導出している (将来 draft 改版で変わりうる)。

/// PUBLISH_OK context で許可されるパラメータ型 (draft-ietf-moq-transport-21 §9.20.17 (EXPIRES Parameter) の MAY appear 列挙)
///
/// draft-ietf-moq-transport-21 Appendix A.1 #1790 (Subscription parameters appear in
/// REQUEST_UPDATE, not PUBLISH_OK) により、subscription 更新用パラメータは
/// REQUEST_UPDATE (要求) に出現し、PUBLISH_OK は EXPIRES のみとなる。
pub(crate) const PUBLISH_OK_ALLOWED_PARAMS: &[u64] = &[PARAM_EXPIRES];

/// REQUEST_UPDATE_OK context で許可されるパラメータ型 (subscription / fetch 共通)
///
/// draft-ietf-moq-transport-21 §9.20.17 (EXPIRES Parameter) / draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter) のみ許可。
pub(crate) const REQUEST_UPDATE_OK_ALLOWED_PARAMS: &[u64] = &[PARAM_EXPIRES, PARAM_LARGEST_OBJECT];

/// TRACK_STATUS_OK context で許可されるパラメータ型 (draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter) LARGEST_OBJECT のみ)
pub(crate) const TRACK_STATUS_OK_ALLOWED_PARAMS: &[u64] = &[PARAM_LARGEST_OBJECT];

// ─── REQUEST_UPDATE の context 別許可パラメータ集合 ─────────────────
//
// draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope): REQUEST_UPDATE は
// subscription / fetch / track status の 3 種で許可されるが、context ごとに
// 許可パラメータが異なる。各定数は draft-ietf-moq-transport-21 §9.20.3 以降の各 Parameter 節の
// "MAY appear in" スコープ定義から導出している (将来 draft 改版で変わりうる)。
// エンコード層の REQUEST_UPDATE_ALLOWED_PARAMS は全 context の和集合であり、
// context 別の厳密な検証はセッション層でこれらの定数を用いて行う。

/// FETCH context の REQUEST_UPDATE で許可されるパラメータ型 (draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter) / §9.20.8 (SUBSCRIBER PRIORITY Parameter))
pub(crate) const FETCH_UPDATE_ALLOWED_PARAMS: &[u64] =
    &[PARAM_AUTHORIZATION_TOKEN, PARAM_SUBSCRIBER_PRIORITY];

/// SUBSCRIBE (Subscription) context の REQUEST_UPDATE で許可されるパラメータ型
/// (draft-ietf-moq-transport-21 §3.3.2 (Range Filters): Range Filter 0x25-0x28 は
/// subscription の REQUEST_UPDATE に出現可能だが、TRACK_PROPERTY_FILTER (0x29) は
/// SUBSCRIBE_TRACKS 専用であるため除外する)
pub(crate) const SUBSCRIPTION_UPDATE_ALLOWED_PARAMS: &[u64] = &[
    PARAM_AUTHORIZATION_TOKEN,
    PARAM_OBJECT_DELIVERY_TIMEOUT,
    PARAM_SUBGROUP_DELIVERY_TIMEOUT,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_FORWARD,
    PARAM_LOCATION_FILTER,
    PARAM_NEW_GROUP_REQUEST,
    PARAM_FILL_PARAMETERS,
    PARAM_SUBGROUP_FILTER,
    PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER,
    PARAM_OBJECT_PROPERTY_FILTER,
];

/// REQUEST_UPDATE で許可されるパラメータ型 (draft-ietf-moq-transport-21 §3.3.2 (Range Filters): subscriber からの REQUEST_UPDATE に Range Filter 出現可能)
const REQUEST_UPDATE_ALLOWED_PARAMS: &[u64] = &[
    PARAM_AUTHORIZATION_TOKEN,
    PARAM_OBJECT_DELIVERY_TIMEOUT,
    PARAM_SUBGROUP_DELIVERY_TIMEOUT,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_FORWARD,
    PARAM_LOCATION_FILTER,
    PARAM_NEW_GROUP_REQUEST,
    PARAM_TRACK_NAMESPACE_PREFIX,
    PARAM_FILL_PARAMETERS,
    PARAM_SUBGROUP_FILTER,
    PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER,
    PARAM_OBJECT_PROPERTY_FILTER,
    PARAM_TRACK_PROPERTY_FILTER,
];

/// PUBLISH で許可されるパラメータ型
///
/// draft-ietf-moq-transport-21 §9.20 各 Parameter 節の出現規定 (PUBLISH を含む) から
/// 再導出した 9 種 (AUTHORIZATION_TOKEN / OBJECT_DELIVERY_TIMEOUT /
/// SUBGROUP_DELIVERY_TIMEOUT / SUBSCRIBER_PRIORITY / GROUP_ORDER /
/// LOCATION_FILTER / EXPIRES / LARGEST_OBJECT / FORWARD)。
/// Range Filter 等の PUBLISH 出現規定を持たない型は含めない。
///
/// draft-ietf-moq-transport-21 §9.18.1 (Parameters on SUBSCRIBE_TRACKS):
/// SUBSCRIBE_TRACKS 由来の PUBLISH には SUBSCRIBE_TRACKS の Parameters が
/// initial subscription parameters として明示的に載る (AUTHORIZATION TOKEN を除く)。
/// 受信した PUBLISH が SUBSCRIBE_TRACKS 起因かを判別する制御は行わず、全 PUBLISH で
/// 寛容に受理する。送信側 (アプリ) が正しく伝播する限り受信側の実害はない。
/// 伝播の構築には `MessageParameters::resulting_publish_parameters` を使う。
///
/// 送信側の `send_publish` (src/session/subscription/send.rs) が request_id 発行前に
/// スコープ検証するために `pub(crate)` で公開している。
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
pub(crate) const PUBLISH_ALLOWED_PARAMS: &[u64] = &[
    PARAM_OBJECT_DELIVERY_TIMEOUT,
    PARAM_AUTHORIZATION_TOKEN,
    PARAM_SUBGROUP_DELIVERY_TIMEOUT,
    PARAM_EXPIRES,
    PARAM_LARGEST_OBJECT,
    PARAM_FORWARD,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_LOCATION_FILTER,
    PARAM_GROUP_ORDER,
];

/// PUBLISH_STATE_NOTIFY で許可されるパラメータ型
/// (draft-ietf-moq-transport-21 §9.10 (PUBLISH_STATE_NOTIFY) /
/// §9.20.10 (LOCATION FILTER Parameter) / §9.20.18 (LARGEST OBJECT Parameter) /
/// §9.20.19 (FORWARD Parameter) の MAY appear 列挙)
///
/// 送信側の `send_publish_state_notify` (src/session/subscription/send.rs) が
/// 状態遷移前にスコープ検証するために `pub(crate)` で公開している。
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
pub(crate) const PUBLISH_STATE_NOTIFY_ALLOWED_PARAMS: &[u64] =
    &[PARAM_FORWARD, PARAM_LOCATION_FILTER, PARAM_LARGEST_OBJECT];

/// FETCH で許可されるパラメータ型
///
/// draft-ietf-moq-transport-21 §9.11 (FETCH): range は LOCATION_FILTER
/// パラメータで指定する。`PARAM_FILL_TIMEOUT` は draft-ietf-moq-transport-21
/// §9.20.6 で FETCH 直下と FILL_PARAMETERS 内の両方に出現可能なため残す。
/// Range Filter 0x25-0x28 は FETCH に出現可能 (§3.3.2 (Range Filters))。
/// TRACK_PROPERTY_FILTER (0x29) は SUBSCRIBE_TRACKS 専用のため含めない。
///
/// 送信側の `send_fetch` (src/session/fetch.rs) が request_id 発行前に
/// スコープ検証するために `pub(crate)` で公開している。
pub(crate) const FETCH_ALLOWED_PARAMS: &[u64] = &[
    PARAM_AUTHORIZATION_TOKEN,
    PARAM_FILL_TIMEOUT,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_GROUP_ORDER,
    PARAM_INCLUDE_PROPERTIES,
    PARAM_LOCATION_FILTER,
    PARAM_SUBGROUP_FILTER,
    PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER,
    PARAM_OBJECT_PROPERTY_FILTER,
];

/// FETCH_OK で許可されるパラメータ型
///
/// FETCH_OK はパラメータを運べない (draft-ietf-moq-transport-21 §9.12 (FETCH_OK) の
/// Parameters フィールドは存在するが、許可されるパラメータは 0 個。
/// §9.20.1 (Parameter Scope) は許可されないパラメータを含むメッセージの受信を
/// PROTOCOL_VIOLATION で拒否する MUST を規定する)。
/// 送信側の `send_fetch_ok` (src/session/fetch.rs) が状態遷移前にスコープ検証するために
/// `pub(crate)` で公開している。
pub(crate) const FETCH_OK_ALLOWED_PARAMS: &[u64] = &[];

/// TRACK_STATUS で許可されるパラメータ型
///
/// 送信側の `send_track_status` (src/session/track_status.rs) が
/// request_id 発行前にスコープ検証するために `pub(crate)` で公開している。
pub(crate) const TRACK_STATUS_ALLOWED_PARAMS: &[u64] =
    &[PARAM_AUTHORIZATION_TOKEN, PARAM_INCLUDE_PROPERTIES];

// ─── 共通型 ──────────────────────────────────────────────────
//
// `TrackNamespace` / `Location` は message_parameter.rs からも使用されるため、
// 循環依存を避けるため `common` モジュールに定義し、ここでは再エクスポートする。

// ─── メッセージに共通するベース型 ──────────────────────────────

/// request_id と TrackNamespace をバッファにエンコードする (namespace 系メッセージの骨格前半)
///
/// SUBSCRIBE / TRACK_STATUS / PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS は
/// いずれも "Request ID (vi64) | Track Namespace" で始まる (draft-ietf-moq-transport-21 §9 各メッセージ節)。
fn encode_request_id_and_namespace(
    request_id: u64,
    ns: &TrackNamespace,
    buf: &mut Vec<u8>,
) -> Result<(), MessageError> {
    varint::encode(request_id, buf);
    ns.encode_to(buf)
}

/// request_id と TrackNamespace をバッファの `pos` 位置からデコードする (namespace 系メッセージの骨格前半)
fn decode_request_id_and_namespace(
    payload: &[u8],
    pos: &mut usize,
) -> Result<(u64, TrackNamespace), MessageError> {
    let (request_id, n) = varint::decode(&payload[*pos..])?;
    *pos += n;
    let ns = TrackNamespace::decode_from(payload, pos)?;
    Ok((request_id, ns))
}

/// Reason Phrase (draft-ietf-moq-transport-21 §8.5 (Reason Phrase Structure)) — UTF-8 文字列、最大 1024 バイト。
/// 将来 draft 改定で上限値や節番号が変更される可能性がある。
#[derive(Debug, Clone, PartialEq)]
pub struct ReasonPhrase(String);

impl ReasonPhrase {
    /// 内部文字列の参照を返す
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 文字列から ReasonPhrase を構築する
    ///
    /// 1024 バイトを超える場合は `MessageError::ReasonPhraseTooLong` を返す
    /// (draft-ietf-moq-transport-21 §8.5 (Reason Phrase Structure))。
    pub fn new(s: impl Into<String>) -> Result<Self, MessageError> {
        let s = s.into();
        if s.len() > 1024 {
            return Err(MessageError::ReasonPhraseTooLong);
        }
        Ok(Self(s))
    }

    fn encode_to(&self, buf: &mut Vec<u8>) {
        varint::encode(self.0.len() as u64, buf);
        buf.extend_from_slice(self.0.as_bytes());
    }

    fn decode_from(buf: &[u8], pos: &mut usize) -> Result<Self, MessageError> {
        let (len, n) = varint::decode(&buf[*pos..])?;
        *pos += n;
        // draft-ietf-moq-transport-21 §8.5 (Reason Phrase Structure):
        // 1024 バイトを超える Reason Phrase の受信は PROTOCOL_VIOLATION で
        // セッションを閉じなければならない (MUST)。encode 側の公開コンストラクタ
        // `ReasonPhrase::new` は入力検証として `ReasonPhraseTooLong` を返す。
        if len > 1024 {
            return Err(MessageError::ProtocolViolation(
                "reason phrase exceeds 1024 bytes",
            ));
        }
        let len = varint::checked_len(len, buf[*pos..].len())?;
        let s = core::str::from_utf8(&buf[*pos..*pos + len])
            .map_err(|_| MessageError::ProtocolViolation("reason phrase is not valid UTF-8"))?
            .to_string();
        *pos += len;
        Ok(Self(s))
    }
}

// ─── メッセージ型 ─────────────────────────────────────────────

/// SETUP メッセージ (draft-ietf-moq-transport-21 §9.1 (SETUP))
#[derive(Debug, Clone, PartialEq)]
pub struct Setup {
    /// 交換する Setup Options (draft-ietf-moq-transport-21 §9.1 (SETUP))
    pub options: SetupOptions,
}

impl Setup {
    /// 自身のペイロードを `buf` に追記する (type_id は ControlMessage 側が持つ)
    pub(crate) fn encode_message_body(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        self.options.encode(buf)?;
        Ok(())
    }

    /// `payload` 先頭から自身を構築し `(Self, 消費バイト数)` を返す
    pub(crate) fn decode_message_body(payload: &[u8]) -> Result<(Self, usize), MessageError> {
        let (options, n) = SetupOptions::decode(payload)?;
        Ok((Self { options }, n))
    }
}

/// GOAWAY メッセージ (draft-ietf-moq-transport-21 §9.2 (GOAWAY))
///
/// draft-ietf-moq-transport-21 Appendix A.2 (Since draft-ietf-moq-transport-18) #1623:
/// Request ID フィールドは削除された。Message Body は New Session URI + Timeout のみ。
#[derive(Debug, Clone, PartialEq)]
pub struct Goaway {
    /// 新しいセッション URI (最大 8192 バイト)
    pub new_session_uri: Vec<u8>,
    /// タイムアウト (varint)
    pub timeout: u64,
}

impl Goaway {
    /// 自身のペイロードを `buf` に追記する (type_id は ControlMessage 側が持つ)
    pub(crate) fn encode_message_body(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        // draft-ietf-moq-transport-21 §9.2 (GOAWAY): New Session URI は最大 8192 バイト
        if self.new_session_uri.len() > 8192 {
            return Err(MessageError::ProtocolViolation(
                "new session URI too long (max 8192)",
            ));
        }
        varint::encode(self.new_session_uri.len() as u64, buf);
        buf.extend_from_slice(&self.new_session_uri);
        varint::encode(self.timeout, buf);
        Ok(())
    }

    /// `payload` 先頭から自身を構築し `(Self, 消費バイト数)` を返す
    ///
    /// Timeout 以降に余剰バイトがある場合は呼び出し側 (`ControlMessage::decode`) の
    /// 末尾長一致検証が draft-ietf-moq-transport-21 §9 (Control Messages) に従い
    /// PROTOCOL_VIOLATION にする。
    pub(crate) fn decode_message_body(payload: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (uri_len, n) = varint::decode(&payload[pos..])?;
        pos += n;
        // draft-ietf-moq-transport-21 §9.2 (GOAWAY): New Session URI は最大 8192 バイト
        if uri_len > 8192 {
            return Err(MessageError::ProtocolViolation(
                "new session URI too long (max 8192)",
            ));
        }
        let uri_len = varint::checked_len(uri_len, payload[pos..].len())?;
        let new_session_uri = payload[pos..pos + uri_len].to_vec();
        pos += uri_len;
        let (timeout, n) = varint::decode(&payload[pos..])?;
        pos += n;
        Ok((
            Self {
                new_session_uri,
                timeout,
            },
            pos,
        ))
    }
}

/// REQUEST_OK メッセージ (draft-ietf-moq-transport-21 §9.3 (REQUEST_OK))
#[derive(Debug, Clone, PartialEq)]
pub struct RequestOk {
    /// 応答に付随するメッセージ パラメータ (draft-ietf-moq-transport-21 §9.3 (REQUEST_OK))
    pub parameters: MessageParameters,
    /// 応答に付随するトラック プロパティ (draft-ietf-moq-transport-21 §9.3 (REQUEST_OK))
    pub track_properties: TrackProperties,
}

impl RequestOk {
    /// 自身のペイロードを `buf` に追記する (type_id は ControlMessage 側が持つ)
    pub(crate) fn encode_message_body(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        self.parameters.validate_scope(REQUEST_OK_ALLOWED_PARAMS)?;
        self.parameters.encode(buf)?;
        self.track_properties.encode(buf)?;
        Ok(())
    }

    /// `payload` 先頭から自身を構築し `(Self, 消費バイト数)` を返す
    ///
    /// 末尾の `TrackProperties::decode` は残スライス全体を消費するため、消費バイト数は
    /// 常に `payload.len()` となる (ControlMessage 側の末尾長一致検証は no-op になる)。
    pub(crate) fn decode_message_body(payload: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (parameters, n) = MessageParameters::decode(&payload[pos..])?;
        pos += n;
        parameters.validate_scope(REQUEST_OK_ALLOWED_PARAMS)?;
        let track_properties = TrackProperties::decode(&payload[pos..])?;
        pos = payload.len();
        Ok((
            Self {
                parameters,
                track_properties,
            },
            pos,
        ))
    }
}

/// リダイレクト情報 (draft-ietf-moq-transport-21 §9.4.1 (Redirect Structure))
///
/// Error Code = REDIRECT の場合のみ REQUEST_ERROR 末尾に付加される。
/// Connect URI 長が 0 なら現在のセッションの URI を使うことが推奨される (SHOULD)。
/// Track Name は namespace-scoped リクエスト (SUBSCRIBE_NAMESPACE / PUBLISH_NAMESPACE /
/// SUBSCRIBE_TRACKS) では意味を持たず空でなければならない (MUST)。
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
#[derive(Debug, Clone, PartialEq)]
pub struct Redirect {
    /// リダイレクト先の Connect URI (draft-ietf-moq-transport-21 §9.4.1 (Redirect Structure))
    pub connect_uri: Vec<u8>,
    /// リダイレクト先のトラック名前空間 (draft-ietf-moq-transport-21 §9.4.1 (Redirect Structure))
    pub track_namespace: TrackNamespace,
    /// リダイレクト先のトラック名 (draft-ietf-moq-transport-21 §9.4.1 (Redirect Structure))
    pub track_name: Vec<u8>,
}

impl Redirect {
    fn encode_to(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        varint::encode(self.connect_uri.len() as u64, buf);
        buf.extend_from_slice(&self.connect_uri);
        self.track_namespace.encode_to(buf)?;
        encode_track_name(&self.track_name, buf);
        Ok(())
    }

    fn decode_from(buf: &[u8], pos: &mut usize) -> Result<Self, MessageError> {
        // 内容検証 (server 受信時の非ゼロ Connect URI 長拒否・ namespace-scoped 要求時の
        // 非空 Track Name 拒否) はここでは行わない。どちらも decode 時点では不明な
        // 文脈 (自 role ・元リクエスト種別) を要するため、Session 層の
        // `handle_peer_request_error` (draft-ietf-moq-transport-21 §9.4.1) で検証する。
        let (uri_len, n) = varint::decode(&buf[*pos..])?;
        *pos += n;
        let uri_len = varint::checked_len(uri_len, buf[*pos..].len())?;
        let connect_uri = buf[*pos..*pos + uri_len].to_vec();
        *pos += uri_len;
        let track_namespace = TrackNamespace::decode_from(buf, pos)?;
        let track_name = decode_track_name(buf, pos)?;
        Ok(Self {
            connect_uri,
            track_namespace,
            track_name,
        })
    }
}

/// REQUEST_ERROR メッセージ (draft-ietf-moq-transport-21 §9.4 (REQUEST_ERROR))
#[derive(Debug, Clone, PartialEq)]
pub struct RequestError {
    /// エラーコード (draft-ietf-moq-transport-21 §16.11.2 (REQUEST_ERROR Codes))
    pub error_code: u64,
    /// 再送までの最小待ち時間 (ms) に 1 を足した値 (plus one)。
    /// 値 1 は即時リトライ可を表す。
    /// 値 0 は再送すべきでない (SHOULD NOT retry) ことを表す。REDIRECT 応答では
    /// 元のリクエストを as sent で再送すべきでないが、提供された URI への接続または
    /// Redirect target を使った再送 (follow) を妨げない
    /// (draft-ietf-moq-transport-21 §9.4.2 (REQUEST_ERROR Message Format)
    /// および §9.4.2 内の REDIRECT 定義)。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub retry_interval: u64,
    /// 人間可読な失敗理由 (draft-ietf-moq-transport-21 §8.5 (Reason Phrase Structure))
    pub reason: ReasonPhrase,
    /// Error Code = REDIRECT の場合のみ付加される (draft-ietf-moq-transport-21 §9.4.2 (REQUEST_ERROR Message Format))
    pub redirect: Option<Redirect>,
}

impl RequestError {
    /// 自身のペイロードを `buf` に追記する (type_id は ControlMessage 側が持つ)
    pub(crate) fn encode_message_body(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        varint::encode(self.error_code, buf);
        varint::encode(self.retry_interval, buf);
        self.reason.encode_to(buf);
        // draft-ietf-moq-transport-21 §9.4.2 (REQUEST_ERROR Message Format) "Redirect: Present only when Error Code is REDIRECT"
        // error_code と redirect の present 整合を生成側でも強制する。
        // 不整合な組み合わせは ProtocolViolation で生成を拒否する。
        match (self.error_code == REQUEST_REDIRECT, &self.redirect) {
            (true, Some(redirect)) => redirect.encode_to(buf)?,
            (true, None) => {
                return Err(MessageError::ProtocolViolation(
                    "REQUEST_ERROR with REDIRECT error code requires a Redirect",
                ));
            }
            (false, Some(_)) => {
                return Err(MessageError::ProtocolViolation(
                    "REQUEST_ERROR without REDIRECT error code must not carry a Redirect",
                ));
            }
            (false, None) => {}
        }
        Ok(())
    }

    /// `payload` 先頭から自身を構築し `(Self, 消費バイト数)` を返す
    pub(crate) fn decode_message_body(payload: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (error_code, n) = varint::decode(&payload[pos..])?;
        pos += n;
        let (retry_interval, n) = varint::decode(&payload[pos..])?;
        pos += n;
        let reason = ReasonPhrase::decode_from(payload, &mut pos)?;
        // draft-ietf-moq-transport-21 §9.4.2 (REQUEST_ERROR Message Format) "Redirect: Present only when Error Code is REDIRECT"
        // REDIRECT のときは Redirect が必須 (retry 先指示なしの REDIRECT は意味を
        // なさないため、欠落は PROTOCOL_VIOLATION)。pos == payload.len() の明示
        // チェックを省くと空スライスに対する Redirect::decode_from が UnexpectedEof を
        // 返してしまい、完了条件が要求する ProtocolViolation にならない。
        // error_code != REDIRECT で末尾に余剰がある場合は redirect = None のまま、
        // 末尾チェック (pos != payload.len()) が PROTOCOL_VIOLATION にする。
        let redirect = if error_code == REQUEST_REDIRECT {
            if pos == payload.len() {
                return Err(MessageError::ProtocolViolation(
                    "REQUEST_ERROR with REDIRECT error code must contain a Redirect",
                ));
            }
            Some(Redirect::decode_from(payload, &mut pos)?)
        } else {
            None
        };
        Ok((
            Self {
                error_code,
                retry_interval,
                reason,
                redirect,
            },
            pos,
        ))
    }
}

/// SUBSCRIBE メッセージ (draft-ietf-moq-transport-21 §9.6 (SUBSCRIBE))
#[derive(Debug, Clone, PartialEq)]
pub struct Subscribe {
    /// 購読要求を識別する Request ID (draft-ietf-moq-transport-21 §9.6 (SUBSCRIBE))
    pub request_id: u64,
    /// 購読対象トラックの名前空間 (draft-ietf-moq-transport-21 §9.6 (SUBSCRIBE))
    pub track_namespace: TrackNamespace,
    /// 購読対象のトラック名 (draft-ietf-moq-transport-21 §9.6 (SUBSCRIBE))
    pub track_name: Vec<u8>,
    /// 購読に付随するメッセージ パラメータ (draft-ietf-moq-transport-21 §9.6 (SUBSCRIBE))
    pub parameters: MessageParameters,
}

impl Subscribe {
    /// 自身のペイロードを `buf` に追記する (type_id は ControlMessage 側が持つ)
    pub(crate) fn encode_message_body(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        // draft-ietf-moq-transport-21 §8.7 (Track Namespace Structure): Full Track Name の 4096 バイト制限
        validate_full_track_name(&self.track_namespace, &self.track_name)?;
        self.parameters.validate_scope(SUBSCRIBE_ALLOWED_PARAMS)?;
        encode_request_id_and_namespace(self.request_id, &self.track_namespace, buf)?;
        encode_track_name(&self.track_name, buf);
        self.parameters.encode(buf)?;
        Ok(())
    }

    /// `payload` 先頭から自身を構築し `(Self, 消費バイト数)` を返す
    pub(crate) fn decode_message_body(payload: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (request_id, track_namespace) = decode_request_id_and_namespace(payload, &mut pos)?;
        let track_name = decode_track_name(payload, &mut pos)?;
        // draft-ietf-moq-transport-21 §8.7 (Track Namespace Structure): Full Track Name の 4096 バイト制限
        validate_full_track_name(&track_namespace, &track_name)?;
        let (parameters, n) = MessageParameters::decode(&payload[pos..])?;
        pos += n;
        parameters.validate_scope(SUBSCRIBE_ALLOWED_PARAMS)?;
        Ok((
            Self {
                request_id,
                track_namespace,
                track_name,
                parameters,
            },
            pos,
        ))
    }
}

/// SUBSCRIBE_OK メッセージ (draft-ietf-moq-transport-21 §9.7 (SUBSCRIBE_OK))
#[derive(Debug, Clone, PartialEq)]
pub struct SubscribeOk {
    /// データストリーム上でトラックを指す Track Alias (draft-ietf-moq-transport-21 §9.7 (SUBSCRIBE_OK))
    pub track_alias: u64,
    /// 応答に付随するメッセージ パラメータ (draft-ietf-moq-transport-21 §9.7 (SUBSCRIBE_OK))
    pub parameters: MessageParameters,
    /// 応答に付随するトラック プロパティ (draft-ietf-moq-transport-21 §9.7 (SUBSCRIBE_OK))
    pub track_properties: TrackProperties,
}

impl SubscribeOk {
    /// 自身のペイロードを `buf` に追記する (type_id は ControlMessage 側が持つ)
    pub(crate) fn encode_message_body(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        self.parameters
            .validate_scope(SUBSCRIBE_OK_ALLOWED_PARAMS)?;
        varint::encode(self.track_alias, buf);
        self.parameters.encode(buf)?;
        self.track_properties.encode(buf)?;
        Ok(())
    }

    /// `payload` 先頭から自身を構築し `(Self, 消費バイト数)` を返す
    ///
    /// 末尾の `TrackProperties::decode` は残スライス全体を消費するため、消費バイト数は
    /// 常に `payload.len()` となる (ControlMessage 側の末尾長一致検証は no-op になる)。
    pub(crate) fn decode_message_body(payload: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (track_alias, n) = varint::decode(&payload[pos..])?;
        pos += n;
        let (parameters, n) = MessageParameters::decode(&payload[pos..])?;
        pos += n;
        parameters.validate_scope(SUBSCRIBE_OK_ALLOWED_PARAMS)?;
        let track_properties = TrackProperties::decode(&payload[pos..])?;
        pos = payload.len();
        Ok((
            Self {
                track_alias,
                parameters,
                track_properties,
            },
            pos,
        ))
    }
}

/// REQUEST_UPDATE メッセージ (draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE))
#[derive(Debug, Clone, PartialEq)]
pub struct RequestUpdate {
    /// 更新対象の要求を指す Request ID (draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE))
    pub request_id: u64,
    /// 更新内容のメッセージ パラメータ (draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE))
    pub parameters: MessageParameters,
}

impl RequestUpdate {
    /// 自身のペイロードを `buf` に追記する (type_id は ControlMessage 側が持つ)
    pub(crate) fn encode_message_body(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        self.parameters
            .validate_scope(REQUEST_UPDATE_ALLOWED_PARAMS)?;
        varint::encode(self.request_id, buf);
        self.parameters.encode(buf)?;
        Ok(())
    }

    /// `payload` 先頭から自身を構築し `(Self, 消費バイト数)` を返す
    pub(crate) fn decode_message_body(payload: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (request_id, n) = varint::decode(&payload[pos..])?;
        pos += n;
        let (parameters, n) = MessageParameters::decode(&payload[pos..])?;
        pos += n;
        parameters.validate_scope(REQUEST_UPDATE_ALLOWED_PARAMS)?;
        Ok((
            Self {
                request_id,
                parameters,
            },
            pos,
        ))
    }
}

/// PUBLISH メッセージ (draft-ietf-moq-transport-21 §9.8 (PUBLISH))
#[derive(Debug, Clone, PartialEq)]
pub struct Publish {
    /// 公開要求を識別する Request ID (draft-ietf-moq-transport-21 §9.8 (PUBLISH))
    pub request_id: u64,
    /// 公開対象トラックの名前空間 (draft-ietf-moq-transport-21 §9.8 (PUBLISH))
    pub track_namespace: TrackNamespace,
    /// 公開対象のトラック名 (draft-ietf-moq-transport-21 §9.8 (PUBLISH))
    pub track_name: Vec<u8>,
    /// データストリーム上でトラックを指す Track Alias (draft-ietf-moq-transport-21 §9.8 (PUBLISH))
    pub track_alias: u64,
    /// 公開に付随するメッセージ パラメータ (draft-ietf-moq-transport-21 §9.8 (PUBLISH))
    pub parameters: MessageParameters,
    /// 公開に付随するトラック プロパティ (draft-ietf-moq-transport-21 §9.8 (PUBLISH))
    pub track_properties: TrackProperties,
}

impl Publish {
    /// 自身のペイロードを `buf` に追記する (type_id は ControlMessage 側が持つ)
    pub(crate) fn encode_message_body(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        // draft-ietf-moq-transport-21 §8.7 (Track Namespace Structure): Full Track Name の 4096 バイト制限
        validate_full_track_name(&self.track_namespace, &self.track_name)?;
        self.parameters.validate_scope(PUBLISH_ALLOWED_PARAMS)?;
        encode_request_id_and_namespace(self.request_id, &self.track_namespace, buf)?;
        encode_track_name(&self.track_name, buf);
        varint::encode(self.track_alias, buf);
        self.parameters.encode(buf)?;
        self.track_properties.encode(buf)?;
        Ok(())
    }

    /// `payload` 先頭から自身を構築し `(Self, 消費バイト数)` を返す
    ///
    /// 末尾の `TrackProperties::decode` は残スライス全体を消費するため、消費バイト数は
    /// 常に `payload.len()` となる (ControlMessage 側の末尾長一致検証は no-op になる)。
    pub(crate) fn decode_message_body(payload: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (request_id, track_namespace) = decode_request_id_and_namespace(payload, &mut pos)?;
        let track_name = decode_track_name(payload, &mut pos)?;
        // draft-ietf-moq-transport-21 §8.7 (Track Namespace Structure): Full Track Name の 4096 バイト制限
        validate_full_track_name(&track_namespace, &track_name)?;
        let (track_alias, n) = varint::decode(&payload[pos..])?;
        pos += n;
        let (parameters, n) = MessageParameters::decode(&payload[pos..])?;
        pos += n;
        parameters.validate_scope(PUBLISH_ALLOWED_PARAMS)?;
        let track_properties = TrackProperties::decode(&payload[pos..])?;
        pos = payload.len();
        Ok((
            Self {
                request_id,
                track_namespace,
                track_name,
                track_alias,
                parameters,
                track_properties,
            },
            pos,
        ))
    }
}

/// PUBLISH_DONE メッセージ (draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE))
#[derive(Debug, Clone, PartialEq)]
pub struct PublishDone {
    /// 公開終了のステータスコード (draft-ietf-moq-transport-21 §16.11.3 (PUBLISH_DONE Codes))
    pub status_code: u64,
    /// 当該トラック用に開いたデータストリーム数。正確な数を把握できない場合は 2^64 - 1 (unknown) を送る (draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE))
    pub stream_count: u64,
    /// 人間可読な終了理由 (draft-ietf-moq-transport-21 §8.5 (Reason Phrase Structure))
    pub reason: ReasonPhrase,
}

impl PublishDone {
    /// 自身のペイロードを `buf` に追記する (type_id は ControlMessage 側が持つ)
    pub(crate) fn encode_message_body(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        varint::encode(self.status_code, buf);
        varint::encode(self.stream_count, buf);
        self.reason.encode_to(buf);
        Ok(())
    }

    /// `payload` 先頭から自身を構築し `(Self, 消費バイト数)` を返す
    pub(crate) fn decode_message_body(payload: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (status_code, n) = varint::decode(&payload[pos..])?;
        pos += n;
        let (stream_count, n) = varint::decode(&payload[pos..])?;
        pos += n;
        let reason = ReasonPhrase::decode_from(payload, &mut pos)?;
        Ok((
            Self {
                status_code,
                stream_count,
                reason,
            },
            pos,
        ))
    }
}

/// PUBLISH_STATE_NOTIFY メッセージ (draft-ietf-moq-transport-21 §9.10 (PUBLISH_STATE_NOTIFY))
///
/// publisher が subscriber 発 REQUEST_UPDATE 以外の理由で subscription 状態が
/// 変わったことを通知する一方向メッセージ。応答を要求せず、
/// MAX_REQUEST_UPDATES 制限の対象外である。Request ID を持たず、
/// 送信された subscription の bidi stream 上で運ばれる。
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
#[derive(Debug, Clone, PartialEq)]
pub struct PublishStateNotify {
    /// 通知する購読状態のメッセージ パラメータ (draft-ietf-moq-transport-21 §9.10 (PUBLISH_STATE_NOTIFY))
    pub parameters: MessageParameters,
}

impl PublishStateNotify {
    /// 自身のペイロードを `buf` に追記する (type_id は ControlMessage 側が持つ)
    pub(crate) fn encode_message_body(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        self.parameters
            .validate_scope(PUBLISH_STATE_NOTIFY_ALLOWED_PARAMS)?;
        self.parameters.encode(buf)?;
        Ok(())
    }

    /// `payload` 先頭から自身を構築し `(Self, 消費バイト数)` を返す
    pub(crate) fn decode_message_body(payload: &[u8]) -> Result<(Self, usize), MessageError> {
        let (parameters, pos) = MessageParameters::decode(payload)?;
        parameters.validate_scope(PUBLISH_STATE_NOTIFY_ALLOWED_PARAMS)?;
        Ok((Self { parameters }, pos))
    }
}

/// FETCH メッセージ (draft-ietf-moq-transport-21 §9.11 (FETCH))
#[derive(Debug, Clone, PartialEq)]
pub struct Fetch {
    /// 取得要求を識別する Request ID (draft-ietf-moq-transport-21 §9.11 (FETCH))
    pub request_id: u64,
    /// 取得対象トラックの名前空間 (draft-ietf-moq-transport-21 §9.11 (FETCH))
    pub track_namespace: TrackNamespace,
    /// 取得対象のトラック名 (draft-ietf-moq-transport-21 §9.11 (FETCH))
    pub track_name: Vec<u8>,
    /// 取得に付随するメッセージ パラメータ (draft-ietf-moq-transport-21 §9.11 (FETCH))
    pub parameters: MessageParameters,
}

impl Fetch {
    /// 自身のペイロードを `buf` に追記する (type_id は ControlMessage 側が持つ)
    pub(crate) fn encode_message_body(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        self.parameters.validate_scope(FETCH_ALLOWED_PARAMS)?;
        varint::encode(self.request_id, buf);
        // draft-ietf-moq-transport-21 §8.7 (Track Namespace Structure): Full Track Name の 4096 バイト制限
        validate_full_track_name(&self.track_namespace, &self.track_name)?;
        self.track_namespace.encode_to(buf)?;
        encode_track_name(&self.track_name, buf);
        self.parameters.encode(buf)?;
        Ok(())
    }

    /// `payload` 先頭から自身を構築し `(Self, 消費バイト数)` を返す
    pub(crate) fn decode_message_body(payload: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (request_id, n) = varint::decode(&payload[pos..])?;
        pos += n;
        let track_namespace = TrackNamespace::decode_from(payload, &mut pos)?;
        let track_name = decode_track_name(payload, &mut pos)?;
        validate_full_track_name(&track_namespace, &track_name)?;
        let (parameters, n) = MessageParameters::decode(&payload[pos..])?;
        pos += n;
        parameters.validate_scope(FETCH_ALLOWED_PARAMS)?;
        Ok((
            Self {
                request_id,
                track_namespace,
                track_name,
                parameters,
            },
            pos,
        ))
    }
}

/// FETCH_OK メッセージ (draft-ietf-moq-transport-21 §9.12 (FETCH_OK))
#[derive(Debug, Clone, PartialEq)]
pub struct FetchOk {
    /// トラック終端に達した場合に 1 となるフラグ (0 または 1 のみ、draft-ietf-moq-transport-21 §9.12 (FETCH_OK))
    pub end_of_track: u8,
    /// 取得範囲の終端位置 (draft-ietf-moq-transport-21 §9.12 (FETCH_OK))
    pub end_location: Location,
    /// 応答に付随するメッセージ パラメータ (draft-ietf-moq-transport-21 §9.12 (FETCH_OK))
    pub parameters: MessageParameters,
    /// 応答に付随するトラック プロパティ (draft-ietf-moq-transport-21 §9.12 (FETCH_OK))
    pub track_properties: TrackProperties,
}

/// FETCH_OK の End of Track 値域文言 (encode / decode で共有)
const FETCH_OK_END_OF_TRACK_RANGE: &str = "FETCH_OK end_of_track must be 0 or 1";

impl FetchOk {
    /// 自身のペイロードを `buf` に追記する (type_id は ControlMessage 側が持つ)
    pub(crate) fn encode_message_body(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        // draft-ietf-moq-transport-21 §9.12 (FETCH_OK): End of Track は 0 または 1 のみ
        if self.end_of_track > 1 {
            return Err(MessageError::ProtocolViolation(FETCH_OK_END_OF_TRACK_RANGE));
        }
        self.parameters.validate_scope(FETCH_OK_ALLOWED_PARAMS)?;
        buf.push(self.end_of_track);
        self.end_location.encode_to(buf);
        self.parameters.encode(buf)?;
        self.track_properties.encode(buf)?;
        Ok(())
    }

    /// `payload` 先頭から自身を構築し `(Self, 消費バイト数)` を返す
    ///
    /// 末尾の `TrackProperties::decode` は残スライス全体を消費するため、消費バイト数は
    /// 常に `payload.len()` となる (ControlMessage 側の末尾長一致検証は no-op になる)。
    pub(crate) fn decode_message_body(payload: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        if payload[pos..].is_empty() {
            return Err(MessageError::UnexpectedEof);
        }
        let end_of_track = payload[pos];
        // draft-ietf-moq-transport-21 §9.12 (FETCH_OK): End of Track は 0 または 1 のみ
        if end_of_track > 1 {
            return Err(MessageError::ProtocolViolation(FETCH_OK_END_OF_TRACK_RANGE));
        }
        pos += 1;
        let end_location = Location::decode_from(payload, &mut pos)?;
        let (parameters, n) = MessageParameters::decode(&payload[pos..])?;
        pos += n;
        parameters.validate_scope(FETCH_OK_ALLOWED_PARAMS)?;
        let track_properties = TrackProperties::decode(&payload[pos..])?;
        pos = payload.len();
        Ok((
            Self {
                end_of_track,
                end_location,
                parameters,
                track_properties,
            },
            pos,
        ))
    }
}

/// TRACK_STATUS メッセージ (draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS))
#[derive(Debug, Clone, PartialEq)]
pub struct TrackStatus {
    /// 状態照会要求を識別する Request ID (draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS))
    pub request_id: u64,
    /// 照会対象トラックの名前空間 (draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS))
    pub track_namespace: TrackNamespace,
    /// 照会対象のトラック名 (draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS))
    pub track_name: Vec<u8>,
    /// 照会に付随するメッセージ パラメータ (draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS))
    pub parameters: MessageParameters,
}

impl TrackStatus {
    /// 自身のペイロードを `buf` に追記する (type_id は ControlMessage 側が持つ)
    pub(crate) fn encode_message_body(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        // draft-ietf-moq-transport-21 §8.7 (Track Namespace Structure): Full Track Name の 4096 バイト制限
        validate_full_track_name(&self.track_namespace, &self.track_name)?;
        self.parameters
            .validate_scope(TRACK_STATUS_ALLOWED_PARAMS)?;
        encode_request_id_and_namespace(self.request_id, &self.track_namespace, buf)?;
        encode_track_name(&self.track_name, buf);
        self.parameters.encode(buf)?;
        Ok(())
    }

    /// `payload` 先頭から自身を構築し `(Self, 消費バイト数)` を返す
    pub(crate) fn decode_message_body(payload: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (request_id, track_namespace) = decode_request_id_and_namespace(payload, &mut pos)?;
        let track_name = decode_track_name(payload, &mut pos)?;
        // draft-ietf-moq-transport-21 §8.7 (Track Namespace Structure): Full Track Name の 4096 バイト制限
        validate_full_track_name(&track_namespace, &track_name)?;
        let (parameters, n) = MessageParameters::decode(&payload[pos..])?;
        pos += n;
        parameters.validate_scope(TRACK_STATUS_ALLOWED_PARAMS)?;
        Ok((
            Self {
                request_id,
                track_namespace,
                track_name,
                parameters,
            },
            pos,
        ))
    }
}

// ─── ControlMessage ──────────────────────────────────────────

/// 本ライブラリが実装しない定義済み request メッセージ型 (draft-ietf-moq-transport-21 §9 Table 5)
///
/// relay 専用の namespace 発見・告知機構のうち、request stream の先頭として届く型と
/// その応答の型である。request として届くのは先頭 3 種 (PUBLISH_NAMESPACE /
/// SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) で、残り 3 種は応答専用である。
pub(crate) const UNSUPPORTED_MESSAGE_TYPES: &[u64] = &[
    MSG_PUBLISH_NAMESPACE,
    MSG_SUBSCRIBE_NAMESPACE,
    MSG_SUBSCRIBE_TRACKS,
    MSG_NAMESPACE,
    MSG_NAMESPACE_DONE,
    MSG_PUBLISH_SKIPPED,
];

/// `UNSUPPORTED_MESSAGE_TYPES` のうち本体が Request ID (vi64) で始まる型
pub(crate) const UNSUPPORTED_MESSAGE_TYPES_WITH_REQUEST_ID: &[u64] = &[
    MSG_PUBLISH_NAMESPACE,
    MSG_SUBSCRIBE_NAMESPACE,
    MSG_SUBSCRIBE_TRACKS,
];

/// MOQT コントロールメッセージの列挙型
#[derive(Debug, Clone, PartialEq)]
pub enum ControlMessage {
    /// SETUP メッセージ (draft-ietf-moq-transport-21 §9.1 (SETUP))
    Setup(Setup),
    /// GOAWAY メッセージ (draft-ietf-moq-transport-21 §9.2 (GOAWAY))
    Goaway(Goaway),
    /// REQUEST_OK メッセージ (draft-ietf-moq-transport-21 §9.3 (REQUEST_OK))
    RequestOk(RequestOk),
    /// REQUEST_ERROR メッセージ (draft-ietf-moq-transport-21 §9.4 (REQUEST_ERROR))
    RequestError(RequestError),
    /// SUBSCRIBE メッセージ (draft-ietf-moq-transport-21 §9.6 (SUBSCRIBE))
    Subscribe(Subscribe),
    /// SUBSCRIBE_OK メッセージ (draft-ietf-moq-transport-21 §9.7 (SUBSCRIBE_OK))
    SubscribeOk(SubscribeOk),
    /// REQUEST_UPDATE メッセージ (draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE))
    RequestUpdate(RequestUpdate),
    /// PUBLISH メッセージ (draft-ietf-moq-transport-21 §9.8 (PUBLISH))
    Publish(Publish),
    /// PUBLISH_DONE メッセージ (draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE))
    PublishDone(PublishDone),
    /// PUBLISH_STATE_NOTIFY メッセージ (draft-ietf-moq-transport-21 §9.10 (PUBLISH_STATE_NOTIFY))
    PublishStateNotify(PublishStateNotify),
    /// FETCH メッセージ (draft-ietf-moq-transport-21 §9.11 (FETCH))
    Fetch(Fetch),
    /// FETCH_OK メッセージ (draft-ietf-moq-transport-21 §9.12 (FETCH_OK))
    FetchOk(FetchOk),
    /// TRACK_STATUS メッセージ (draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS))
    TrackStatus(TrackStatus),
    /// 定義済みだが本ライブラリが実装しない制御メッセージ
    ///
    /// draft-ietf-moq-transport-21 §1.5 (Modularity): "Limited endpoints SHOULD respond to any
    /// unsupported messages with the appropriate NOT_SUPPORTED error code, rather than ignoring
    /// them." §9 の Table 5 に定義済みの型であって本ライブラリが扱わないものを、型 ID と
    /// 本体の生バイト列のまま保持する。Table 5 に無い型は従来どおり
    /// `MessageError::InvalidMessageType` で拒否する。
    ///
    /// 対象は relay 専用の namespace 発見・告知機構 (CODEBASE.md の方針で実装しない) の
    /// PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS と、その応答である
    /// NAMESPACE / NAMESPACE_DONE / PUBLISH_SKIPPED である。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    Unsupported {
        /// 制御メッセージ型 ID (draft-ietf-moq-transport-21 §9 Table 5)
        type_id: u64,
        /// 本体が Request ID (vi64) で始まる型 (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE /
        /// SUBSCRIBE_TRACKS) のときだけ `Some`。応答専用の型 (NAMESPACE / NAMESPACE_DONE /
        /// PUBLISH_SKIPPED) は Request ID を持たないため `None`
        request_id: Option<u64>,
        /// Length の後ろの生バイト列 (Request ID を含む)
        body: Vec<u8>,
    },
}

impl ControlMessage {
    /// メッセージ全体 (Type + Length + Message Body) をエンコードして返す
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let (type_id, payload) = self.encode_message_body()?;
        let payload_len = payload.len();
        if payload_len > 65535 {
            return Err(MessageError::PayloadTooLong);
        }

        let mut buf = Vec::new();
        varint::encode(type_id, &mut buf);
        buf.push((payload_len >> 8) as u8);
        buf.push(payload_len as u8);
        buf.extend_from_slice(&payload);
        Ok(buf)
    }

    /// バッファの先頭からメッセージをデコードし `(ControlMessage, 消費バイト数)` を返す
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;

        let (type_id, n) = varint::decode(&buf[pos..])?;
        pos += n;

        if buf[pos..].len() < 2 {
            return Err(MessageError::UnexpectedEof);
        }
        let payload_len = ((buf[pos] as usize) << 8) | buf[pos + 1] as usize;
        pos += 2;

        if buf[pos..].len() < payload_len {
            return Err(MessageError::UnexpectedEof);
        }
        let payload = &buf[pos..pos + payload_len];
        pos += payload_len;

        let msg = Self::decode_message_body(type_id, payload)?;
        Ok((msg, pos))
    }

    /// ペイロードをエンコードして `(type_id, payload_bytes)` を返す
    ///
    /// 各アームは type_id の決定とペイロード生成の構造体メソッドへの委譲のみを行う。
    /// バリデーションやフィールド単位の直列化は各構造体の `encode_message_body` が持つ。
    fn encode_message_body(&self) -> Result<(u64, Vec<u8>), MessageError> {
        let mut payload = Vec::new();
        let type_id = match self {
            Self::Setup(m) => {
                m.encode_message_body(&mut payload)?;
                MSG_SETUP
            }
            Self::Goaway(m) => {
                m.encode_message_body(&mut payload)?;
                MSG_GOAWAY
            }
            Self::RequestOk(m) => {
                m.encode_message_body(&mut payload)?;
                MSG_REQUEST_OK
            }
            Self::RequestError(m) => {
                m.encode_message_body(&mut payload)?;
                MSG_REQUEST_ERROR
            }
            Self::Subscribe(m) => {
                m.encode_message_body(&mut payload)?;
                MSG_SUBSCRIBE
            }
            Self::SubscribeOk(m) => {
                m.encode_message_body(&mut payload)?;
                MSG_SUBSCRIBE_OK
            }
            Self::RequestUpdate(m) => {
                m.encode_message_body(&mut payload)?;
                MSG_REQUEST_UPDATE
            }
            Self::Publish(m) => {
                m.encode_message_body(&mut payload)?;
                MSG_PUBLISH
            }
            Self::PublishDone(m) => {
                m.encode_message_body(&mut payload)?;
                MSG_PUBLISH_DONE
            }
            Self::PublishStateNotify(m) => {
                m.encode_message_body(&mut payload)?;
                MSG_PUBLISH_STATE_NOTIFY
            }
            Self::Fetch(m) => {
                m.encode_message_body(&mut payload)?;
                MSG_FETCH
            }
            Self::FetchOk(m) => {
                m.encode_message_body(&mut payload)?;
                MSG_FETCH_OK
            }
            Self::TrackStatus(m) => {
                m.encode_message_body(&mut payload)?;
                MSG_TRACK_STATUS
            }
            Self::Unsupported { type_id, body, .. } => {
                // decode で保持した本体をそのまま書き戻す (Request ID を含む)
                payload.extend_from_slice(body);
                *type_id
            }
        };
        Ok((type_id, payload))
    }

    /// ペイロードバイト列から型 ID に対応するメッセージをデコードする
    ///
    /// 各アームは対応構造体の `decode_message_body` 呼び出しとバリアント構築のみを行う。
    /// 未知 type_id 弾きはここに残す (type_id → 構造体のディスパッチは構造体側に置けないため)。
    fn decode_message_body(type_id: u64, payload: &[u8]) -> Result<Self, MessageError> {
        let (msg, consumed) = match type_id {
            MSG_SETUP => {
                let (m, n) = Setup::decode_message_body(payload)?;
                (Self::Setup(m), n)
            }
            MSG_GOAWAY => {
                let (m, n) = Goaway::decode_message_body(payload)?;
                (Self::Goaway(m), n)
            }
            MSG_REQUEST_OK => {
                let (m, n) = RequestOk::decode_message_body(payload)?;
                (Self::RequestOk(m), n)
            }
            MSG_REQUEST_ERROR => {
                let (m, n) = RequestError::decode_message_body(payload)?;
                (Self::RequestError(m), n)
            }
            MSG_SUBSCRIBE => {
                let (m, n) = Subscribe::decode_message_body(payload)?;
                (Self::Subscribe(m), n)
            }
            MSG_SUBSCRIBE_OK => {
                let (m, n) = SubscribeOk::decode_message_body(payload)?;
                (Self::SubscribeOk(m), n)
            }
            MSG_REQUEST_UPDATE => {
                let (m, n) = RequestUpdate::decode_message_body(payload)?;
                (Self::RequestUpdate(m), n)
            }
            MSG_PUBLISH => {
                let (m, n) = Publish::decode_message_body(payload)?;
                (Self::Publish(m), n)
            }
            MSG_PUBLISH_DONE => {
                let (m, n) = PublishDone::decode_message_body(payload)?;
                (Self::PublishDone(m), n)
            }
            MSG_PUBLISH_STATE_NOTIFY => {
                let (m, n) = PublishStateNotify::decode_message_body(payload)?;
                (Self::PublishStateNotify(m), n)
            }
            MSG_FETCH => {
                let (m, n) = Fetch::decode_message_body(payload)?;
                (Self::Fetch(m), n)
            }
            MSG_FETCH_OK => {
                let (m, n) = FetchOk::decode_message_body(payload)?;
                (Self::FetchOk(m), n)
            }
            MSG_TRACK_STATUS => {
                let (m, n) = TrackStatus::decode_message_body(payload)?;
                (Self::TrackStatus(m), n)
            }
            // draft-ietf-moq-transport-21 §1.5 (Modularity): Table 5 に定義済みで本ライブラリが
            // 実装しない型は、未知型として拒否せず NOT_SUPPORTED で応答できるようにする。
            // 本体は解析せず生バイト列のまま保持する。
            id if UNSUPPORTED_MESSAGE_TYPES.contains(&id) => {
                let request_id = if UNSUPPORTED_MESSAGE_TYPES_WITH_REQUEST_ID.contains(&id) {
                    if payload.is_empty() {
                        return Err(MessageError::ProtocolViolation(
                            "unsupported request message has no request id",
                        ));
                    }
                    let (rid, _) = varint::decode(payload)?;
                    Some(rid)
                } else {
                    None
                };
                (
                    Self::Unsupported {
                        type_id: id,
                        request_id,
                        body: payload.to_vec(),
                    },
                    payload.len(),
                )
            }
            _ => return Err(MessageError::InvalidMessageType(type_id)),
        };

        // 全メッセージ共通の末尾長一致検証: ペイロード長と実際の消費バイト数が一致しない場合は RFC 違反
        if consumed != payload.len() {
            return Err(MessageError::ProtocolViolation(
                "payload length does not match message content",
            ));
        }

        Ok(msg)
    }
}
