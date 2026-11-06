//! 共有トランスポート層のエラー型
//!
//! QUIC / WebTransport / QMux の全戻り値をこの `TransportError` に統一する。
//! 各バイナリのアプリケーションエラー型は `From<TransportError>` を実装して
//! `?` で伝播できるようにする。
//!
//! MOQT メッセージの encode / decode 失敗をどのセッション終了コードで閉じるかも、この層の
//! [`session_error_code`] に集約する (`moq-pub` / `moq-sub` / 確立前の `MoqtClient` が共有する)。

use shiguredo_moqt::error::{MessageError, SESSION_PROTOCOL_VIOLATION};

// 未知エラーコードの文脈別正規化は library の helper をそのまま公開する
// (draft-ietf-moq-transport-23 §13 (Grease) の MUST)。
pub use shiguredo_moqt::error::normalize_publish_done_code;
pub use shiguredo_moqt::error::normalize_request_error_code;

/// data stream の復号エラーをセッション終了コードへ分類する
///
/// draft-ietf-moq-transport-23 §11.3.1 (Subgroup Header) / §11.1.2 (Object Properties) は
/// 不正な Subgroup Header / Object ID delta overflow / 非 Normal status への Properties 付与に
/// セッション終了を MUST とする。一方、`MessageError::UnexpectedEof` は `try_decode_*` が
/// データ不足として `Ok(None)` に吸収するため通常は届かず、届いてもセッション終了の対象外とする
/// (`finish()` の `UnexpectedEof` は mid-object FIN として別に扱う)。
/// `MessageError::MalformedTrack` は draft §12.1 (Malformed Tracks) が該当 Track の
/// subscription / FETCH の cancel を MUST とするものであり、セッション終了の対象外とする。
pub fn data_stream_termination_code(error: &MessageError) -> Option<u64> {
    match error {
        MessageError::UnexpectedEof | MessageError::MalformedTrack(_) => None,
        other => Some(session_error_code(other)),
    }
}

/// encode / decode 失敗を閉じる終了コード
///
/// library の [`MessageError::session_termination_code`] に写像を委譲する。
/// draft-ietf-moq-transport-23 §12.2 (Session Termination Codes) の分類に従い、原因を失わない
/// 最も具体的なコードを返す (MALFORMED_KEY_VALUE / MALFORMED_AUTH_TOKEN /
/// INVALID_STREAM_HEADER / MALFORMED_OBJECT / INVALID_OBJECT_STATUS /
/// DELTA_ENCODING_OVERFLOW / FIELD_LENGTH_EXCEEDED / MALFORMED_MESSAGE / INVALID_FIELD /
/// INVALID_TRACK_NAMESPACE / UNEXPECTED_MESSAGE / INVALID_PARAMETER)。
///
/// Request ID を持つ request メッセージの不正トークンは、library のセッションが
/// REQUEST_ERROR (MALFORMED_AUTH_TOKEN) でメッセージ単位に拒否するためここには届かない。
/// ここへ届く `MalformedAuthToken` は、自側で重複トークンを組み立てた encode 失敗と、
/// peer の SETUP (確立前後を問わない) の decode 失敗である。確立前の SETUP の decode 失敗も
/// `MoqtClient::establish_*` がこの写像で終了コードを決める。
/// セッション終了を伴わない variant (draft §12.1 の `MalformedTrack`、MSF の
/// アプリケーションエラー) は `PROTOCOL_VIOLATION` にフォールバックする。
pub fn session_error_code(error: &MessageError) -> u64 {
    error
        .session_termination_code()
        .unwrap_or(SESSION_PROTOCOL_VIOLATION)
}

/// 共有トランスポート層のエラー型
#[derive(Debug)]
pub enum TransportError {
    /// QUIC 接続 / ストリームのエラー (詳細メッセージ付き)
    Quic(String),
    /// 接続先 URL の authority を解釈できなかった (トランスポート種別に依存しない)
    InvalidAuthority(String),
    /// 接続先の名前解決に失敗した (トランスポート種別に依存しない)
    ResolutionFailed(String),
    /// QUIC / TCP などの下位トランスポートエラー
    Transport(Box<dyn std::error::Error + Send + Sync>),
    /// HTTP/3 プロトコルエラー
    Http3(shiguredo_http3::Error),
    /// HTTP/2 プロトコルエラー
    Http2(shiguredo_http2::Error),
    /// WebTransport over HTTP/2 のセッション / ストリームエラー
    WtH2(shiguredo_http2::webtransport::WtError),
    /// QMux (draft-ietf-quic-qmux-02) の接続 / ストリームエラー
    Qmux(tokio_qmux::Error),
    /// 接続がクローズ済み
    ConnectionClosed,
    /// peer がこのストリームの送信方向を終端した (STOP_SENDING)
    ///
    /// tobi-quic は peer の STOP_SENDING を受信したストリームの送信 API (send / finish /
    /// reset) を `StreamError::Stopped` で失敗させ、tokio-qmux は同じ条件で
    /// `Error::Stopped` を返す。これは該当ストリームだけの終端でありセッションは継続するため
    /// (draft-ietf-moq-transport-23 §6.4.2.3 (Request Cancellation and Rejection))、
    /// アプリはセッション終了と区別して扱う必要がある。
    /// `error_code` は transport 依存の wire コードであり、QUIC 直接接続と QMux は QUIC の
    /// code space、WebTransport over HTTP/3 は HTTP/3 の code space になる (MOQT のコードへ
    /// 戻すには `StreamHandle::remap_stop_sending_error_code` を通す)。WebTransport over
    /// HTTP/2 は capsule が MOQT のコードをそのまま運ぶため remap は不要である
    /// (draft-ietf-webtrans-http2-16 §6.3)。受信方向の終端 (RESET_STREAM) は
    /// `RequestStreamEnd::Reset` が運ぶ。
    StreamReset {
        /// peer が載せた wire のエラーコード
        error_code: u64,
    },
    /// MOQT メッセージの encode / decode 失敗
    ///
    /// draft-ietf-moq-transport-23 §9 (Control Messages) と §9.19.1 (Parameter Scope) は
    /// 不正なメッセージの受信を終了コード付きで閉じることを MUST で要求する。終了コードは
    /// 失敗の種類に応じて `MessageError` から選ぶ (本文長の不一致は MALFORMED_MESSAGE、
    /// 宣言長の超過は FIELD_LENGTH_EXCEEDED、フィールド値の逸脱は INVALID_FIELD など)。
    /// どの終了コードで閉じるかは I/O 層 (アプリ) が決めるため、`MessageError` の
    /// variant を保って伝える。
    Moqt(shiguredo_moqt::error::MessageError),
    /// CONNECT レスポンスが 2xx 以外、または :status ヘッダー不在でセッション確立に失敗した
    /// (draft-ietf-webtrans-http3-16 §3.2)
    ConnectFailed { status: Option<u16> },
    /// アプリケーションプロトコル交渉に失敗した (draft-ietf-webtrans-http3-16 §3.3)
    ///
    /// 成功応答の `WT-Protocol` が無い / 不正 / `WT-Available-Protocols` に無い値だったことを
    /// 表す。h3 層が `WebTransportEvent::SessionClosed` で通知した終了コードを保持し、
    /// 2xx 以外の応答を表す [`Self::ConnectFailed`] と区別する。
    /// 交渉失敗では `error_code` は `WT_ALPN_ERROR` になる。
    ProtocolNegotiationFailed { error_code: u64 },
    /// ストリームがクローズ済み
    StreamClosed,
    /// 無効な状態
    InvalidState(String),
    /// 内部エラー
    Internal(String),
}

impl TransportError {
    /// 任意のエラーを `Transport` variant に畳み込むヘルパー
    pub(crate) fn transport(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Transport(e.into())
    }

    /// ストリーム送信 API のエラーを transport エラーへ写す
    ///
    /// peer が STOP_SENDING で送信方向を閉じた場合、QUIC バックエンドは送信 API を
    /// STOP_SENDING 由来のエラーで失敗させる。セッションは継続し、該当ストリームだけが
    /// 終端するため、アプリがセッション終了と切り分けられるよう [`Self::StreamReset`] にする
    /// (draft-ietf-moq-transport-23 §6.4.2.3 (Request Cancellation and Rejection))。
    /// `fallback` は STOP_SENDING 以外のエラーを畳む先であり、経路ごとの既存の表示
    /// (QUIC 直接接続は [`Self::Quic`]、WebTransport は [`Self::Transport`]) を保つために渡す。
    pub(crate) fn from_send_error(
        e: crate::quic::QuicError,
        fallback: impl FnOnce(crate::quic::QuicError) -> Self,
    ) -> Self {
        match e {
            crate::quic::QuicError::Stopped { error_code } => Self::StreamReset { error_code },
            e => fallback(e),
        }
    }

    /// QMux (tokio-qmux) のエラーを transport エラーへ写す
    ///
    /// peer が STOP_SENDING を送った後の送信 API は `Error::Stopped` を返す。QUIC の
    /// STOP_SENDING と同じく該当ストリームだけの終端でありセッションは継続する
    /// ため、[`Self::StreamReset`] にして切断と区別する (draft-ietf-moq-transport-23
    /// §6.4.2.3 (Request Cancellation and Rejection))。
    pub(crate) fn from_qmux(e: tokio_qmux::Error) -> Self {
        match e {
            tokio_qmux::Error::Stopped(error_code) => Self::StreamReset { error_code },
            // 接続が閉じた場合は、セッション終了として variant で判定する経路
            // (moq-pub / moq-sub の `is_transport_session_end` 等) があるため、QUIC 経路と
            // 同じ ConnectionClosed に写す。`StreamFinished` は終端済みストリームへの書き込みで
            // 返るストリーム単位の失敗であり (tokio-qmux の `SendStream::write`)、セッション
            // 終了ではないため写像しない
            tokio_qmux::Error::ConnectionClosed => Self::ConnectionClosed,
            e => Self::Qmux(e),
        }
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Quic(msg) => write!(f, "QUIC error: {msg}"),
            // トランスポート種別に依存しない失敗のため `QUIC:` を付けない (メッセージは生成側で組み立てる)
            Self::InvalidAuthority(msg) | Self::ResolutionFailed(msg) => write!(f, "{msg}"),
            Self::Transport(e) => write!(f, "transport error: {e}"),
            Self::Http3(e) => write!(f, "http3 error: {e}"),
            Self::Http2(e) => write!(f, "http2 error: {e}"),
            Self::WtH2(e) => write!(f, "webtransport over http2 error: {e}"),
            Self::Qmux(e) => write!(f, "QMux error: {e}"),
            Self::ConnectionClosed => write!(f, "connection closed"),
            Self::StreamReset { error_code } => {
                write!(f, "stream reset by peer (error code {error_code:#x})")
            }
            Self::Moqt(e) => write!(f, "MOQT message error: {e}"),
            Self::ConnectFailed { status } => match status {
                Some(s) => write!(f, "CONNECT failed with status {s}"),
                None => write!(f, "CONNECT failed with no :status header"),
            },
            Self::ProtocolNegotiationFailed { error_code } => write!(
                f,
                "application protocol negotiation failed (error code {error_code:#010x})"
            ),
            Self::StreamClosed => write!(f, "stream closed"),
            Self::InvalidState(msg) => write!(f, "invalid state: {msg}"),
            Self::Internal(msg) => write!(f, "internal error: {msg}"),
        }
    }
}

impl std::error::Error for TransportError {}

impl From<shiguredo_http3::Error> for TransportError {
    fn from(e: shiguredo_http3::Error) -> Self {
        Self::Http3(e)
    }
}

impl From<shiguredo_http2::Error> for TransportError {
    fn from(e: shiguredo_http2::Error) -> Self {
        Self::Http2(e)
    }
}

impl From<shiguredo_http2::webtransport::WtError> for TransportError {
    fn from(e: shiguredo_http2::webtransport::WtError) -> Self {
        Self::WtH2(e)
    }
}

impl From<tokio_qmux::Error> for TransportError {
    fn from(e: tokio_qmux::Error) -> Self {
        Self::from_qmux(e)
    }
}

impl From<shiguredo_moqt::error::MessageError> for TransportError {
    fn from(e: shiguredo_moqt::error::MessageError) -> Self {
        // encode / decode の失敗はアプリが終了コード (PROTOCOL_VIOLATION など) を決めるため
        // variant を保つ
        Self::Moqt(e)
    }
}

impl From<shiguredo_moqt::session::types::SessionError> for TransportError {
    fn from(e: shiguredo_moqt::session::types::SessionError) -> Self {
        Self::Internal(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, TransportError>;

#[cfg(test)]
mod tests {
    use super::*;
    use shiguredo_moqt::error::{SESSION_FIELD_LENGTH_EXCEEDED, SESSION_MALFORMED_KEY_VALUE};

    /// `MessageError` は variant を保って伝わる
    ///
    /// decode / encode 失敗の終了コードはアプリが決めるため、`Internal` に畳むと
    /// 判別できなくなる。
    #[test]
    fn message_error_keeps_variant() {
        let err = TransportError::from(shiguredo_moqt::error::MessageError::ProtocolViolation(
            "out of scope parameter",
        ));
        assert!(
            matches!(err, TransportError::Moqt(_)),
            "Moqt に振り分けられること: {err}"
        );
        assert_eq!(
            err.to_string(),
            "MOQT message error: protocol violation: out of scope parameter"
        );
    }

    /// Track Properties の宣言長超過が decode で MALFORMED_KEY_VALUE になり、
    /// session_error_code がセッション終了コード 0x6 に写すこと
    ///
    /// draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) の MUST を、
    /// wire の decode からセッション終了コードへの写像まで通しで固定する。
    #[test]
    fn track_property_length_error_maps_to_malformed_key_value() {
        use shiguredo_moqt::message::ControlMessage;
        // SUBSCRIBE_OK (0x04), Length = 7 (vi64), Track Alias = 0, Parameters: count=0,
        // Track Properties: delta=0x0B (既知の奇数型 IMMUTABLE_PROPERTIES) /
        // Length=10 / 実体 3 バイト
        let wire = [0x04, 0x07, 0x00, 0x00, 0x0B, 0x0A, 0x01, 0x02, 0x03];
        let err = ControlMessage::decode(&wire)
            .expect_err("Track Properties の宣言長超過は decode に失敗すること");
        assert_eq!(
            session_error_code(&err),
            SESSION_MALFORMED_KEY_VALUE,
            "書式違反は MALFORMED_KEY_VALUE (0x6) であること"
        );
    }

    /// encode / decode 失敗が原因を失わずセッション終了コードへ写ること
    ///
    /// draft-ietf-moq-transport-23 §12.2 (Session Termination Codes) の分類を
    /// variant ごとに固定する。制御メッセージの解析失敗とフレーミング違反が同じ
    /// PROTOCOL_VIOLATION に畳まれないことを確認する。
    #[test]
    fn session_error_code_maps_message_failures() {
        use shiguredo_moqt::error::{
            SESSION_DELTA_ENCODING_OVERFLOW, SESSION_INVALID_FIELD, SESSION_INVALID_OBJECT_STATUS,
            SESSION_INVALID_PARAMETER, SESSION_INVALID_STREAM_HEADER,
            SESSION_INVALID_TRACK_NAMESPACE, SESSION_MALFORMED_AUTH_TOKEN,
            SESSION_MALFORMED_MESSAGE, SESSION_MALFORMED_OBJECT, SESSION_PROTOCOL_VIOLATION,
            SESSION_ROLE_VIOLATION, SESSION_UNEXPECTED_MESSAGE,
        };
        let cases: &[(MessageError, u64, &str)] = &[
            (
                MessageError::MalformedKeyValue("test kvp"),
                SESSION_MALFORMED_KEY_VALUE,
                "KVP の直列化不整合は MALFORMED_KEY_VALUE (0x6)",
            ),
            (
                MessageError::MalformedAuthToken("test malformed token"),
                SESSION_MALFORMED_AUTH_TOKEN,
                "不正トークンは MALFORMED_AUTH_TOKEN (0x16)",
            ),
            (
                MessageError::FieldLengthExceeded("test field length exceeded"),
                SESSION_FIELD_LENGTH_EXCEEDED,
                "上限超過は FIELD_LENGTH_EXCEEDED (0x401)",
            ),
            (
                MessageError::PayloadTooLong,
                SESSION_FIELD_LENGTH_EXCEEDED,
                "KVP 値長の上限超過も FIELD_LENGTH_EXCEEDED (0x401)",
            ),
            (
                MessageError::ReasonPhraseTooLong,
                SESSION_FIELD_LENGTH_EXCEEDED,
                "Reason Phrase の長さ超過も FIELD_LENGTH_EXCEEDED (0x401)",
            ),
            (
                MessageError::MalformedMessage("test malformed message"),
                SESSION_MALFORMED_MESSAGE,
                "本文長の不一致は MALFORMED_MESSAGE (0x300)",
            ),
            (
                MessageError::UnexpectedEof,
                SESSION_MALFORMED_MESSAGE,
                "制御メッセージの途中終端は MALFORMED_MESSAGE (0x300)",
            ),
            (
                MessageError::InvalidField("test invalid field"),
                SESSION_INVALID_FIELD,
                "フィールド値の範囲外は INVALID_FIELD (0x301)",
            ),
            (
                MessageError::InvalidTrackNamespace("test namespace"),
                SESSION_INVALID_TRACK_NAMESPACE,
                "Track Namespace の構造違反は INVALID_TRACK_NAMESPACE (0x302)",
            ),
            (
                MessageError::InvalidMessageType(0x3F),
                SESSION_UNEXPECTED_MESSAGE,
                "未知のメッセージ型は UNEXPECTED_MESSAGE (0x303)",
            ),
            (
                MessageError::RoleViolation("test role"),
                SESSION_ROLE_VIOLATION,
                "role 違反は ROLE_VIOLATION (0x304)",
            ),
            (
                MessageError::InvalidParameter,
                SESSION_INVALID_PARAMETER,
                "Message Parameter の違反は INVALID_PARAMETER (0x305)",
            ),
            (
                MessageError::InvalidStreamHeader("test header"),
                SESSION_INVALID_STREAM_HEADER,
                "ストリームヘッダの不正は INVALID_STREAM_HEADER (0x200)",
            ),
            (
                MessageError::MalformedObject("test object"),
                SESSION_MALFORMED_OBJECT,
                "Object の復号失敗は MALFORMED_OBJECT (0x201)",
            ),
            (
                MessageError::InvalidObjectStatus("test status"),
                SESSION_INVALID_OBJECT_STATUS,
                "Object Status の不正は INVALID_OBJECT_STATUS (0x202)",
            ),
            (
                MessageError::DeltaEncodingOverflow("test delta"),
                SESSION_DELTA_ENCODING_OVERFLOW,
                "デルタ溢出は DELTA_ENCODING_OVERFLOW (0x400)",
            ),
            (
                MessageError::ProtocolViolation("test violation"),
                SESSION_PROTOCOL_VIOLATION,
                "汎用のプロトコル違反は PROTOCOL_VIOLATION (0x3)",
            ),
        ];
        for (error, expected, message) in cases {
            assert_eq!(session_error_code(error), *expected, "{message}: {error:?}");
        }
    }

    /// data stream の復号エラー分類: 終了対象と対象外が区別されること
    #[test]
    fn data_stream_termination_code_classification() {
        use shiguredo_moqt::error::{MessageError, SESSION_PROTOCOL_VIOLATION};
        assert_eq!(
            super::data_stream_termination_code(&MessageError::ProtocolViolation("bad header")),
            Some(SESSION_PROTOCOL_VIOLATION)
        );
        assert_eq!(
            super::data_stream_termination_code(&MessageError::MalformedKeyValue("bad kvp")),
            Some(shiguredo_moqt::error::SESSION_MALFORMED_KEY_VALUE)
        );
        assert_eq!(
            super::data_stream_termination_code(&MessageError::MalformedAuthToken("bad token")),
            Some(shiguredo_moqt::error::SESSION_MALFORMED_AUTH_TOKEN)
        );
        // データプレーンの delta overflow / status 不正も原因を失わない
        assert_eq!(
            super::data_stream_termination_code(&MessageError::DeltaEncodingOverflow("bad delta")),
            Some(shiguredo_moqt::error::SESSION_DELTA_ENCODING_OVERFLOW)
        );
        assert_eq!(
            super::data_stream_termination_code(&MessageError::InvalidObjectStatus("bad status")),
            Some(shiguredo_moqt::error::SESSION_INVALID_OBJECT_STATUS)
        );
        assert_eq!(
            super::data_stream_termination_code(&MessageError::UnexpectedEof),
            None
        );
        assert_eq!(
            super::data_stream_termination_code(&MessageError::MalformedTrack("malformed")),
            None
        );
    }
}
