//! codec 層のエラー型
//!
//! pub / sub のアプリケーションエラー型には依存しない。各 example は本クレートの
//! [`Error`] から自分のアプリケーションエラー型への `From` を実装し、既存の variant と
//! 表示を保つ。

use std::fmt;

/// codec 層のエラー型
#[derive(Debug)]
pub enum Error {
    /// AV1 デコードエラー
    Decode(shiguredo_dav1d::Error),
    /// H.264 / H.265 デコードエラー (Apple Video Toolbox、macOS 限定)
    #[cfg(target_os = "macos")]
    VideoToolbox(shiguredo_video_toolbox::Error),
    /// Opus デコードエラー
    Opus(shiguredo_opus::Error),
    /// デコーダの初期化に必要な設定レコードが未到達
    ///
    /// H.264 / H.265 は avcC / hvcC の到着までデコーダを初期化できない。pub はエラーに、
    /// sub は警告ログを出して空のフレーム列にするため、variant で判別できるようにする。
    NotInitialized {
        /// 未初期化だった codec の表示名 (例: "H.264")
        codec: &'static str,
        /// 初期化に必要な設定レコードの表示名 (例: "avcC")
        record: &'static str,
    },
    /// その他のエラー
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(e) => write!(f, "decode: {e}"),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(e) => write!(f, "video toolbox: {e}"),
            Self::Opus(e) => write!(f, "opus: {e}"),
            Self::NotInitialized { codec, record } => write!(
                f,
                "{codec} decoder is not initialized; the {record} record is required"
            ),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<shiguredo_dav1d::Error> for Error {
    fn from(e: shiguredo_dav1d::Error) -> Self {
        Self::Decode(e)
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

/// このクレートの処理結果
pub type Result<T> = std::result::Result<T, Error>;
