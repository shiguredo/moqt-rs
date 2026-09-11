//! subscriber のアプリケーションエラー型
//!
//! 各依存クレートのエラーを [`Error`] に集約し、`?` で伝播できるようにする。

use std::fmt;

/// アプリケーションエラー型
#[derive(Debug)]
pub enum Error {
    /// QUIC 接続エラー
    Quic(String),
    /// MoQT プロトコルエラー
    Moqt(shiguredo_moqt::error::MessageError),
    /// AV1 デコードエラー
    Decode(shiguredo_dav1d::Error),
    /// Opus デコードエラー
    Opus(shiguredo_opus::Error),
    /// raw_player (SDL) エラー
    Player(raw_player::Error),
    /// Apple Video Toolbox エラー (H.264 / H.265 デコード)
    #[cfg(target_os = "macos")]
    VideoToolbox(shiguredo_video_toolbox::Error),
    /// I/O エラー
    Io(std::io::Error),
    /// WebTransport エラー
    WebTransport(String),
    /// その他のエラー
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Quic(msg) => write!(f, "QUIC: {msg}"),
            Self::WebTransport(msg) => write!(f, "WebTransport: {msg}"),
            Self::Moqt(e) => write!(f, "MoQT: {e}"),
            Self::Decode(e) => write!(f, "decode: {e}"),
            Self::Opus(e) => write!(f, "opus: {e}"),
            Self::Player(e) => write!(f, "player: {e}"),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(e) => write!(f, "video toolbox: {e}"),
            Self::Io(e) => write!(f, "I/O: {e}"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<shiguredo_moqt::error::MessageError> for Error {
    fn from(e: shiguredo_moqt::error::MessageError) -> Self {
        Self::Moqt(e)
    }
}

impl From<shiguredo_moqt::session::types::SessionError> for Error {
    fn from(e: shiguredo_moqt::session::types::SessionError) -> Self {
        Self::Other(e.to_string())
    }
}

impl From<shiguredo_dav1d::Error> for Error {
    fn from(e: shiguredo_dav1d::Error) -> Self {
        Self::Decode(e)
    }
}

impl From<shiguredo_opus::Error> for Error {
    fn from(e: shiguredo_opus::Error) -> Self {
        Self::Opus(e)
    }
}

impl From<raw_player::Error> for Error {
    fn from(e: raw_player::Error) -> Self {
        Self::Player(e)
    }
}

#[cfg(target_os = "macos")]
impl From<shiguredo_video_toolbox::Error> for Error {
    fn from(e: shiguredo_video_toolbox::Error) -> Self {
        Self::VideoToolbox(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<moqt_example_transport::error::TransportError> for Error {
    fn from(e: moqt_example_transport::error::TransportError) -> Self {
        // QUIC 由来のエラーは app の Quic variant に振り分け、ログ表示を共通化前と等価に保つ。
        // その他のトランスポートエラーは WebTransport variant に畳む。
        match e {
            moqt_example_transport::error::TransportError::Quic(msg) => Self::Quic(msg),
            // 統合した MoqtClient 由来の内部エラーは従来の Other 表示に合わせる
            moqt_example_transport::error::TransportError::Internal(msg) => Self::Other(msg),
            other => Self::WebTransport(other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
