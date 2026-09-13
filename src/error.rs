//! MOQT メッセージのエンコード/デコードで発生するエラー
//!
//! コントロールメッセージとデータストリーム ヘッダのエンコード/デコード層が返す共通エラー型。
use alloc::string::String;

/// MOQT メッセージのエンコード/デコードで発生するエラー
#[derive(Debug, PartialEq)]
pub enum MessageError {
    /// バッファが途中で終了した
    UnexpectedEof,
    /// 未知のメッセージ型 ID
    ///
    /// draft-ietf-moq-transport-21 §9 (Control Messages):
    /// "An endpoint that receives an unknown message type MUST close the session." (MUST)
    ///
    /// `ControlMessage::decode` は既にデコード済みの型を扱う Session の外側 (I/O 層) に
    /// 本エラーを伝える。`Session::recv_control` / `recv_request` は decode 済みの
    /// `ControlMessage` を受け取るため、本エラーの検出と PROTOCOL_VIOLATION による
    /// session close は I/O 層の責務である。
    InvalidMessageType(u64),
    /// ペイロード長が 65535 バイトを超えた
    PayloadTooLong,
    /// Reason Phrase が 1024 バイトを超えた (draft-ietf-moq-transport-21 §8.5 (Reason Phrase Structure))
    ReasonPhraseTooLong,
    /// Parameter エンコードが不正 (型と値の不一致、長さ超過など)
    InvalidParameter,
    /// RFC 違反 (説明付き)
    ProtocolViolation(&'static str),
    /// 既知 Key-Value-Pair の Value が定義された serialization と一致しない
    ///
    /// draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure):
    /// 受信側は KEY_VALUE_FORMATTING_ERROR (0x6) でセッションを閉じなければならない (MUST)。
    KeyValueFormattingError(&'static str),
    /// AUTHORIZATION_TOKEN が不正 (説明付き)
    ///
    /// draft-ietf-moq-transport-21 §8.9 (Authorization Token Compression):
    /// 受信側は MALFORMED_AUTH_TOKEN でメッセージを reject しなければならない。
    MalformedAuthToken(&'static str),
    /// MSF カタログの JSON フォーマットが不正 (説明付き)
    InvalidCatalog(String),
    /// MSF カタログのバージョンが未対応
    InvalidCatalogVersion(String),
    /// MSF Timeline の gzip 復号に失敗した (draft-ietf-moq-msf-01 §7.1 (Media Timeline track payload) / §8.1 (Event Timeline data format))
    GzipDecode(String),
    /// MSF Timeline の gzip 圧縮に失敗した (draft-ietf-moq-msf-01 §7.1 (Media Timeline track payload) / §8.1 (Event Timeline data format))
    GzipEncode(String),
}

impl core::fmt::Display for MessageError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnexpectedEof => write!(f, "unexpected end of buffer"),
            Self::InvalidMessageType(t) => write!(f, "invalid message type: {t:#x}"),
            Self::PayloadTooLong => write!(f, "payload too long (max 65535 bytes)"),
            Self::ReasonPhraseTooLong => write!(f, "reason phrase too long (max 1024 bytes)"),
            Self::InvalidParameter => write!(f, "invalid parameter encoding"),
            Self::ProtocolViolation(msg) => write!(f, "protocol violation: {msg}"),
            Self::KeyValueFormattingError(msg) => {
                write!(f, "key-value formatting error: {msg}")
            }
            Self::MalformedAuthToken(msg) => write!(f, "malformed auth token: {msg}"),
            Self::InvalidCatalog(msg) => write!(f, "invalid MSF catalog: {msg}"),
            Self::InvalidCatalogVersion(v) => write!(f, "unsupported MSF catalog version: {v}"),
            Self::GzipDecode(msg) => write!(f, "MSF timeline gzip decode failed: {msg}"),
            Self::GzipEncode(msg) => write!(f, "MSF timeline gzip encode failed: {msg}"),
        }
    }
}

// ─── ローカル専用エラーコード (wire には出ない) ──────────────────

/// フィルタ不一致による自端の送信拒否 (draft-ietf-moq-transport-21 §3.3.3 (Combining Filters))
///
/// **この値を wire に送出してはならない。** draft の Session Termination Error Codes
/// (§16.11.1) にも REQUEST_ERROR Codes (§16.11.2) にも存在しない実装内部の値である。
/// 割り当て済みコードと衝突しないよう u64 の上端付近を使い、誤って送出したときに
/// 意図しない値であることが一目で分かるようにしている。
///
/// §3.3.3 の "The publisher MUST forward only objects that pass all filters" に従って
/// 自端が送信を止めたことだけを表し、peer のプロトコル違反ではない。この値は
/// [`crate::session::types::SendRequestError::LocalFilterMismatch`] として返り、
/// `SessionError` には載らない。wire コードを取る公開 API に渡しても各レジストリの
/// `*_INTERNAL_ERROR` に置換される。
/// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
pub const SESSION_LOCAL_FILTER_MISMATCH: u64 = 0xFFFF_FFFF_FFFF_FF01;

/// Datagram の delivery timeout 超過による送信抑止 (draft-ietf-moq-transport-21 §5.2 (Delivery Timeouts and Data Reliability))
///
/// **この値を wire に送出してはならない。** `SESSION_LOCAL_FILTER_MISMATCH` と同様に
/// 実装内部の値であり、peer のプロトコル違反ではない。
/// §5.2 の "For datagrams, the implementation MUST drop the datagrams if the time elapsed
/// exceeds OBJECT_DELIVERY_TIMEOUT" に従って自端が送信を止めたこと
/// を表す。この値は [`crate::session::types::SendRequestError::LocalDatagramTimeout`] として
/// 返り、`SessionError` には載らない。wire コードを取る公開 API に渡しても各レジストリの
/// `*_INTERNAL_ERROR` に置換される。
/// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
pub const SESSION_LOCAL_DATAGRAM_TIMEOUT: u64 = 0xFFFF_FFFF_FFFF_FF02;

/// wire へ出してはならないローカル専用コードかを判定する
///
/// `SESSION_LOCAL_FILTER_MISMATCH` / `SESSION_LOCAL_DATAGRAM_TIMEOUT` を判定する。
/// `Session` の wire コードを取る公開 API は本関数でローカル値を検出し、
/// 対応するレジストリの `*_INTERNAL_ERROR` に置換する。
pub(crate) fn is_local_error_code(code: u64) -> bool {
    code == SESSION_LOCAL_FILTER_MISMATCH || code == SESSION_LOCAL_DATAGRAM_TIMEOUT
}

// ─── Session Termination Error Codes ──────────────────────────
//
// draft-ietf-moq-transport-21 §16.11.1 (Session Termination Error Codes)
// セッション終了時に使用されるエラーコード。
// これらのコードは QUIC CONNECTION_CLOSE や WebTransport session close で使用される。

/// エラーなし
pub const SESSION_NO_ERROR: u64 = 0x0;
/// 実装固有のエラー
pub const SESSION_INTERNAL_ERROR: u64 = 0x1;
/// 認証失敗
pub const SESSION_UNAUTHORIZED: u64 = 0x2;
/// プロトコル違反
pub const SESSION_PROTOCOL_VIOLATION: u64 = 0x3;
/// 無効な Request ID
pub const SESSION_INVALID_REQUEST_ID: u64 = 0x4;
/// 重複する Track Alias
pub const SESSION_DUPLICATE_TRACK_ALIAS: u64 = 0x5;
/// Key-Value-Pair フォーマットエラー
pub const SESSION_KEY_VALUE_FORMATTING_ERROR: u64 = 0x6;
/// 無効な PATH
pub const SESSION_INVALID_PATH: u64 = 0x8;
/// 不正な PATH フォーマット
pub const SESSION_MALFORMED_PATH: u64 = 0x9;
/// GOAWAY タイムアウト
pub const SESSION_GOAWAY_TIMEOUT: u64 = 0x10;
/// コントロールメッセージのタイムアウト
pub const SESSION_CONTROL_MESSAGE_TIMEOUT: u64 = 0x11;
/// データストリームのタイムアウト
pub const SESSION_DATA_STREAM_TIMEOUT: u64 = 0x12;
/// Auth Token キャッシュオーバーフロー
pub const SESSION_AUTH_TOKEN_CACHE_OVERFLOW: u64 = 0x13;
/// 重複する Auth Token Alias
pub const SESSION_DUPLICATE_AUTH_TOKEN_ALIAS: u64 = 0x14;
/// 不正な Auth Token 形式
pub const SESSION_MALFORMED_AUTH_TOKEN: u64 = 0x16;
/// 未知の Auth Token Alias
pub const SESSION_UNKNOWN_AUTH_TOKEN_ALIAS: u64 = 0x17;
/// 期限切れの Auth Token
pub const SESSION_EXPIRED_AUTH_TOKEN: u64 = 0x18;
/// 無効な Authority
pub const SESSION_INVALID_AUTHORITY: u64 = 0x19;
/// 不正な Authority フォーマット
pub const SESSION_MALFORMED_AUTHORITY: u64 = 0x1A;
/// REQUEST_UPDATE 上限超過 (draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES))
pub const SESSION_TOO_MANY_REQUEST_UPDATES: u64 = 0x1B;

// ─── REQUEST_ERROR Codes ──────────────────────────────────────
//
// draft-ietf-moq-transport-21 §16.11.2 (REQUEST_ERROR Codes)
// REQUEST_ERROR メッセージで使用されるエラーコード。

/// 実装固有のエラー
pub const REQUEST_INTERNAL_ERROR: u64 = 0x0;
/// 認証失敗
pub const REQUEST_UNAUTHORIZED: u64 = 0x1;
/// タイムアウト
pub const REQUEST_TIMEOUT: u64 = 0x2;
/// 未対応
pub const REQUEST_NOT_SUPPORTED: u64 = 0x3;
/// 不正な Auth Token 形式
pub const REQUEST_MALFORMED_AUTH_TOKEN: u64 = 0x4;
/// 期限切れの Auth Token
pub const REQUEST_EXPIRED_AUTH_TOKEN: u64 = 0x5;
/// GOAWAY 受信済み
pub const REQUEST_GOING_AWAY: u64 = 0x6;
/// 過負荷
pub const REQUEST_EXCESSIVE_LOAD: u64 = 0x9;
/// トラックまたは名前空間が存在しない
pub const REQUEST_DOES_NOT_EXIST: u64 = 0x10;
/// 無効なフィルタ範囲
pub const REQUEST_INVALID_RANGE: u64 = 0x11;
/// 不正なトラック (draft-ietf-moq-transport-21 §12.3 (Request Error Codes))
pub const REQUEST_MALFORMED_TRACK: u64 = 0x12;
/// 興味なし
pub const REQUEST_UNINTERESTED: u64 = 0x20;
/// プレフィックスの重複
pub const REQUEST_PREFIX_OVERLAP: u64 = 0x30;
/// 名前空間が大きすぎる
pub const REQUEST_NAMESPACE_TOO_LARGE: u64 = 0x31;
/// 未対応の必須拡張
pub const REQUEST_UNSUPPORTED_EXTENSION: u64 = 0x33;
/// リダイレクト
pub const REQUEST_REDIRECT: u64 = 0x34;
/// 競合するフィルタ
pub const REQUEST_CONFLICTING_FILTERS: u64 = 0x35;
/// 無効なフィルタ (draft-ietf-moq-transport-21 §3.3.2 (Range Filters))
pub const REQUEST_INVALID_FILTER: u64 = 0x36;

// ─── PUBLISH_DONE Codes ──────────────────────────────────────
//
// draft-ietf-moq-transport-21 §16.11.3 (PUBLISH_DONE Codes)
// PUBLISH_DONE メッセージで使用されるステータスコード。

/// 実装固有のエラー
pub const PUBLISH_DONE_INTERNAL_ERROR: u64 = 0x0;
/// 認証失敗
pub const PUBLISH_DONE_UNAUTHORIZED: u64 = 0x1;
/// Track 終了
pub const PUBLISH_DONE_TRACK_ENDED: u64 = 0x2;
/// GOAWAY 受信済み
pub const PUBLISH_DONE_GOING_AWAY: u64 = 0x4;
/// 遅延しすぎ
pub const PUBLISH_DONE_TOO_FAR_BEHIND: u64 = 0x5;
/// 有効期限切れ
pub const PUBLISH_DONE_EXPIRED: u64 = 0x6;
/// REQUEST_UPDATE 失敗
pub const PUBLISH_DONE_UPDATE_FAILED: u64 = 0x8;
/// 過負荷
pub const PUBLISH_DONE_EXCESSIVE_LOAD: u64 = 0x9;
/// 不正なトラック (draft-ietf-moq-transport-21 §12.4 (Publish Done Codes))
///
/// draft-ietf-moq-transport-21 §12.4 (Publish Done Codes): "A relay publisher
/// detected that the track was malformed (see Section 12.1)." relay が downstream の
/// subscription を終端するために送る。`Session` (endpoint-local) は自動送出せず、
/// relay 実装が明示的に送る前提である。
/// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
pub const PUBLISH_DONE_MALFORMED_TRACK: u64 = 0x12;

// ─── Stream Reset Error Codes ─────────────────────────────────
//
// draft-ietf-moq-transport-21 §12.5 (Stream Reset Error Codes)
// (IANA レジストリは draft-ietf-moq-transport-21 §16.11.4 (Stream Reset Error Codes))
// QUIC RESET_STREAM / RESET_STREAM_AT フレームで任意のストリームを
// リセットする際に使用されるエラーコード。draft-ietf-moq-transport-21 で全リクエスト
// ストリームに適用されるよう汎用化された。

/// 実装固有のエラー
pub const STREAM_INTERNAL_ERROR: u64 = 0x0;
/// キャンセル
pub const STREAM_CANCELLED: u64 = 0x1;
/// デリバリータイムアウト
pub const STREAM_DELIVERY_TIMEOUT: u64 = 0x2;
/// セッション終了中
pub const STREAM_SESSION_CLOSED: u64 = 0x3;
/// GOAWAY 送受信による拒否
pub const STREAM_GOING_AWAY: u64 = 0x4;
/// 遅延しすぎ
pub const STREAM_TOO_FAR_BEHIND: u64 = 0x5;
/// 未知の Object Status
pub const STREAM_UNKNOWN_OBJECT_STATUS: u64 = 0x6;
/// 認証トークンの有効期限切れ
pub const STREAM_EXPIRED_AUTH_TOKEN: u64 = 0x7;
/// 過負荷
pub const STREAM_EXCESSIVE_LOAD: u64 = 0x9;
/// 不正なトラック (draft-ietf-moq-transport-21 §12.5 (Stream Reset Error Codes))
pub const STREAM_MALFORMED_TRACK: u64 = 0x12;
