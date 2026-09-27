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
    /// 接続がクローズ済み
    ConnectionClosed,
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
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Quic(msg) => write!(f, "QUIC error: {msg}"),
            // トランスポート種別に依存しない失敗のため `QUIC:` を付けない (メッセージは生成側で組み立てる)
            Self::InvalidAuthority(msg) | Self::ResolutionFailed(msg) => write!(f, "{msg}"),
            Self::Transport(e) => write!(f, "transport error: {e}"),
            Self::Http3(e) => write!(f, "http3 error: {e}"),
            Self::ConnectionClosed => write!(f, "connection closed"),
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
        Self::Internal(e.to_string())
    }
}

impl From<shiguredo_moqt::session::types::SessionError> for TransportError {
    fn from(e: shiguredo_moqt::session::types::SessionError) -> Self {
        Self::Internal(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, TransportError>;
