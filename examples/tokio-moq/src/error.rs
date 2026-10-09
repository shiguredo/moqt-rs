//! 共有トランスポート層のエラー型
//!
//! QUIC / WebTransport の全戻り値をこの `TransportError` に統一する。
//! 各バイナリのアプリケーションエラー型は `From<TransportError>` を実装して
//! `?` で伝播できるようにする。

/// 共有トランスポート層のエラー型
#[derive(Debug)]
pub enum TransportError {
    /// QUIC 接続 / ストリームのエラー (詳細メッセージ付き)
    Quic(String),
    /// 接続先 URL の authority を解釈できなかった (トランスポート種別に依存しない)
    InvalidAuthority(String),
    /// 接続先の名前解決に失敗した (トランスポート種別に依存しない)
    ResolutionFailed(String),
    /// s2n-quic トランスポートエラー
    Transport(Box<dyn std::error::Error + Send + Sync>),
    /// HTTP/3 プロトコルエラー
    Http3(shiguredo_http3::Error),
    /// HTTP/2 プロトコルエラー
    Http2(shiguredo_http2::Error),
    /// WebTransport over HTTP/2 のセッション / ストリームエラー
    WtH2(shiguredo_http2::webtransport::WtError),
    /// 接続がクローズ済み
    ConnectionClosed,
    /// peer がこのストリームの送信方向を終端した (STOP_SENDING)
    ///
    /// s2n-quic は peer の STOP_SENDING を受信したストリームの送信 API (send / finish /
    /// reset) を `StreamError::StreamReset` で失敗させる。これは該当ストリームだけの終端で
    /// ありセッションは継続するため (draft-ietf-moq-transport-22 §6.4.2.3 (Request
    /// Cancellation and Rejection))、アプリはセッション終了と区別して扱う必要がある。
    /// `error_code` は transport 依存の wire コードであり、QUIC 直接接続は QUIC の code space、
    /// WebTransport over HTTP/3 は HTTP/3 の code space になる (MOQT のコードへ戻すには
    /// `StreamHandle::remap_stop_sending_error_code` を通す)。受信方向の終端
    /// (RESET_STREAM) は `RequestStreamEnd::Reset` が運ぶ。
    StreamReset {
        /// peer が載せた wire のエラーコード
        error_code: u64,
    },
    /// MOQT メッセージの encode / decode 失敗
    ///
    /// draft-ietf-moq-transport-22 §9 (Control Messages) と §9.20.1 (Parameter Scope) は
    /// 不正なメッセージの受信を PROTOCOL_VIOLATION で閉じることを MUST で要求する。
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
    /// peer が STOP_SENDING で送信方向を閉じた場合、s2n-quic は送信 API を
    /// `StreamError::StreamReset` で失敗させる。セッションは継続し、該当ストリームだけが
    /// 終端するため、アプリがセッション終了と切り分けられるよう [`Self::StreamReset`] にする
    /// (draft-ietf-moq-transport-22 §6.4.2.3 (Request Cancellation and Rejection))。
    /// `fallback` は `StreamReset` 以外のエラーを畳む先であり、経路ごとの既存の表示
    /// (QUIC 直接接続は [`Self::Quic`]、WebTransport は [`Self::Transport`]) を保つために渡す。
    pub(crate) fn from_send_error(
        e: s2n_quic::stream::Error,
        fallback: impl FnOnce(s2n_quic::stream::Error) -> Self,
    ) -> Self {
        match e {
            s2n_quic::stream::Error::StreamReset { error, .. } => Self::StreamReset {
                error_code: error.into(),
            },
            e => fallback(e),
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

impl From<s2n_quic::connection::Error> for TransportError {
    fn from(e: s2n_quic::connection::Error) -> Self {
        Self::transport(e)
    }
}

impl From<s2n_quic::stream::Error> for TransportError {
    fn from(e: s2n_quic::stream::Error) -> Self {
        Self::transport(e)
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
}
