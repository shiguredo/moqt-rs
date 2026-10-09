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
    /// AV1 デコードエラー
    Decode(shiguredo_dav1d::Error),
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
    /// トランスポートがセッション終了を検知した
    ConnectionClosed,
    /// peer がこのストリームの送信方向を終端した (STOP_SENDING)
    ///
    /// セッションは継続し該当ストリームだけが終端するため、pipeline は致命エラーにしない
    /// (draft-ietf-moq-transport-22 §6.4.2.3 (Request Cancellation and Rejection))。
    StreamReset {
        /// peer が載せた wire のエラーコード
        error_code: u64,
    },
    /// object datagram が上限サイズを超えた
    DatagramTooLarge {
        /// MOQT の OBJECT_DATAGRAM (ヘッダ + Properties + payload) の実サイズ (bytes)
        size: usize,
        /// 許容する上限 (bytes)
        max_size: usize,
    },
    /// その他のエラー
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Quic(msg) => write!(f, "QUIC: {msg}"),
            Self::WebTransport(msg) => write!(f, "WebTransport: {msg}"),
            Self::ConnectionClosed => write!(f, "session closed"),
            Self::StreamReset { error_code } => {
                write!(f, "stream reset by peer (error code {error_code:#x})")
            }
            Self::DatagramTooLarge { size, max_size } => write!(
                f,
                "object datagram is too large: {size} bytes (limit {max_size} bytes). \
                 Disable --audio-datagram or increase --datagram-max-size"
            ),
            Self::Moqt(e) => write!(f, "MoQT: {e}"),
            Self::Capture(e) => write!(f, "capture: {e:?}"),
            Self::Encode(e) => write!(f, "encode: {e}"),
            Self::Decode(e) => write!(f, "decode: {e}"),
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

impl From<tokio_moq::error::TransportError> for Error {
    fn from(e: tokio_moq::error::TransportError) -> Self {
        // Quic だけを app の Quic variant に振り分ける。トランスポート種別に依存しない
        // 失敗 (Internal / InvalidAuthority / ResolutionFailed) は Other にして
        // `QUIC:` を付けない。セッション終了は専用の ConnectionClosed に分け、
        // それ以外は WebTransport variant に畳む (HTTP/3 と HTTP/2 の両方)。
        match e {
            tokio_moq::error::TransportError::Quic(msg) => Self::Quic(msg),
            // 統合した MoqtClient 由来の内部エラーは従来の Other 表示に合わせる
            tokio_moq::error::TransportError::Internal(msg) => Self::Other(msg),
            // catch-all に落とすと WebTransport variant になり `WebTransport:` が付くため明示する
            tokio_moq::error::TransportError::InvalidAuthority(msg)
            | tokio_moq::error::TransportError::ResolutionFailed(msg) => Self::Other(msg),
            // セッション終了は表示文字列ではなく variant で判定できるように専用にする
            tokio_moq::error::TransportError::ConnectionClosed => Self::ConnectionClosed,
            // peer の STOP_SENDING によるストリーム終端も、セッション終了と区別して
            // 判定できるように専用にする (pipeline は致命エラーにしない)
            tokio_moq::error::TransportError::StreamReset { error_code } => {
                Self::StreamReset { error_code }
            }
            // encode / decode 失敗は main ループが終了コード (PROTOCOL_VIOLATION など) を
            // 決めるため MessageError の variant を保って伝える
            tokio_moq::error::TransportError::Moqt(e) => Self::Moqt(e),
            other => Self::WebTransport(other.to_string()),
        }
    }
}

/// このクレートの処理結果
pub type Result<T> = std::result::Result<T, Error>;

// publisher と subscriber で表示方針を揃えるため、もう一方の error.rs と同じテストを持つ
#[cfg(test)]
mod tests {
    use super::*;
    use tokio_moq::error::TransportError;

    /// datagram の上限超過は実サイズと上限と対処を表示する
    ///
    /// 原因不明の Fatal 終了にしないための情報が利用者に見えることを固定する。
    #[test]
    fn datagram_too_large_reports_size_limit_and_remedy() {
        let message = Error::DatagramTooLarge {
            size: 1161,
            max_size: 1160,
        }
        .to_string();
        assert_eq!(
            message,
            "object datagram is too large: 1161 bytes (limit 1160 bytes). \
             Disable --audio-datagram or increase --datagram-max-size",
            "実サイズと上限と対処が分かるメッセージであること"
        );
    }

    /// トランスポート種別に依存しない失敗は `Other` になり `QUIC:` が付かない
    #[test]
    fn transport_error_mapping_omits_quic_prefix() {
        let cases = [
            TransportError::InvalidAuthority(
                "invalid server address ':4443': empty host".to_string(),
            ),
            TransportError::ResolutionFailed(
                "failed to resolve 'relay.invalid': no address".to_string(),
            ),
        ];
        for case in cases {
            let err = Error::from(case);
            assert!(
                matches!(err, Error::Other(_)),
                "Other に振り分けられること: {err}"
            );
            assert!(
                !err.to_string().contains("QUIC"),
                "QUIC を付けないこと: {err}"
            );
        }
    }

    /// QUIC 由来のエラーは `Quic` variant のまま `QUIC:` を付ける
    #[test]
    fn transport_error_mapping_keeps_quic_prefix() {
        let err = Error::from(TransportError::Quic("connection failed".to_string()));
        assert!(
            matches!(err, Error::Quic(_)),
            "Quic に振り分けられること: {err}"
        );
        assert_eq!(err.to_string(), "QUIC: connection failed");
    }

    /// セッション終了は専用の `ConnectionClosed` variant になり `WebTransport:` を付けない
    #[test]
    fn transport_error_mapping_uses_connection_closed_variant() {
        let err = Error::from(TransportError::ConnectionClosed);
        assert!(
            matches!(err, Error::ConnectionClosed),
            "ConnectionClosed に振り分けられること: {err}"
        );
        assert_eq!(err.to_string(), "session closed");
    }

    /// peer のストリーム終端は専用の `StreamReset` variant になりセッション終了と区別できる
    ///
    /// 該当ストリームだけの終端でありセッションは継続するため、pipeline が
    /// `ConnectionClosed` と同じ扱いをしないよう variant で判定できる必要がある。
    #[test]
    fn transport_error_mapping_uses_stream_reset_variant() {
        let err = Error::from(TransportError::StreamReset { error_code: 0x1 });
        assert!(
            matches!(err, Error::StreamReset { error_code } if error_code == 0x1),
            "StreamReset に振り分けられること: {err}"
        );
        assert_eq!(
            err.to_string(),
            "stream reset by peer (error code 0x1)",
            "wire のコードが表示に残ること"
        );
    }

    /// encode / decode 失敗は `Moqt` variant になり、main ループが終了コードを決められる
    #[test]
    fn transport_error_mapping_keeps_message_error_variant() {
        let err = Error::from(TransportError::Moqt(
            shiguredo_moqt::error::MessageError::ProtocolViolation("out of scope parameter"),
        ));
        assert!(
            matches!(err, Error::Moqt(_)),
            "Moqt に振り分けられること: {err}"
        );
        assert_eq!(
            err.to_string(),
            "MoQT: protocol violation: out of scope parameter"
        );
    }
}
