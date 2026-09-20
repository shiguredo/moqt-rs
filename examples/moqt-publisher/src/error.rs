//! publisher のアプリケーションエラー型
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
    /// カメラキャプチャエラー
    Capture(shiguredo_video_device::Error),
    /// AV1 エンコードエラー
    Encode(shiguredo_aom::Error),
    /// Video Toolbox エンコードエラー (macOS)
    #[cfg(target_os = "macos")]
    VideoToolbox(shiguredo_video_toolbox::Error),
    /// Opus エンコードエラー
    Opus(shiguredo_opus::Error),
    /// 音声キャプチャエラー
    AudioCapture(shiguredo_audio_device::Error),
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
            Self::Capture(e) => write!(f, "capture: {e:?}"),
            Self::Encode(e) => write!(f, "encode: {e}"),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(e) => write!(f, "video toolbox: {e}"),
            Self::Opus(e) => write!(f, "opus: {e}"),
            Self::AudioCapture(e) => write!(f, "audio capture: {e:?}"),
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

impl From<shiguredo_video_device::Error> for Error {
    fn from(e: shiguredo_video_device::Error) -> Self {
        Self::Capture(e)
    }
}

impl From<shiguredo_aom::Error> for Error {
    fn from(e: shiguredo_aom::Error) -> Self {
        Self::Encode(e)
    }
}

#[cfg(target_os = "macos")]
impl From<shiguredo_video_toolbox::Error> for Error {
    fn from(e: shiguredo_video_toolbox::Error) -> Self {
        Self::VideoToolbox(e)
    }
}

impl From<shiguredo_opus::Error> for Error {
    fn from(e: shiguredo_opus::Error) -> Self {
        Self::Opus(e)
    }
}

impl From<shiguredo_audio_device::Error> for Error {
    fn from(e: shiguredo_audio_device::Error) -> Self {
        Self::AudioCapture(e)
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
