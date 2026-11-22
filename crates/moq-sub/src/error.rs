//! subscriber のアプリケーションエラー型
//!
//! 各依存クレートのエラーを [`Error`] に集約し、`?` で伝播できるようにする。

use std::fmt;

/// アプリケーションエラー型
#[derive(Debug)]
pub enum Error {
    /// QUIC 接続エラー
    Quic(String),
    /// MOQT プロトコルエラー
    Moqt(shiguredo_moqt::error::MessageError),
    /// AV1 デコードエラー
    Decode(shiguredo_dav1d::Error),
    /// VP8 / VP9 デコードエラー
    Vpx(shiguredo_libvpx::Error),
    /// Opus デコードエラー
    Opus(shiguredo_opus::Error),
    /// raw_player (SDL) エラー
    Player(raw_player::error::Error),
    /// Apple Video Toolbox エラー (H.264 / H.265 デコード)
    #[cfg(target_os = "macos")]
    VideoToolbox(shiguredo_video_toolbox::Error),
    /// Apple Audio Toolbox エラー (AAC-LC デコード)
    #[cfg(target_os = "macos")]
    AudioToolbox(shiguredo_audio_toolbox::Error),
    /// I/O エラー
    Io(std::io::Error),
    /// WebTransport エラー
    WebTransport(String),
    /// トランスポートがセッション終了を検知した
    ConnectionClosed,
    /// peer がこのストリームの送信方向を終端した (STOP_SENDING)
    ///
    /// セッションは継続し該当ストリームだけが終端するため、pipeline は致命エラーにしない
    /// (draft-ietf-moq-transport-23 §6.4.2.3 (Request Cancellation and Rejection))。
    StreamReset {
        /// peer が載せた wire のエラーコード
        error_code: u64,
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
            Self::Moqt(e) => write!(f, "MOQT: {e}"),
            Self::Decode(e) => write!(f, "decode: {e}"),
            Self::Vpx(e) => write!(f, "vpx: {e}"),
            Self::Opus(e) => write!(f, "opus: {e}"),
            Self::Player(e) => write!(f, "player: {e}"),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(e) => write!(f, "video toolbox: {e}"),
            #[cfg(target_os = "macos")]
            Self::AudioToolbox(e) => write!(f, "audio toolbox: {e}"),
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

impl From<shiguredo_libvpx::Error> for Error {
    fn from(e: shiguredo_libvpx::Error) -> Self {
        Self::Vpx(e)
    }
}

impl From<shiguredo_opus::Error> for Error {
    fn from(e: shiguredo_opus::Error) -> Self {
        Self::Opus(e)
    }
}

impl From<moq_codec::error::Error> for Error {
    fn from(e: moq_codec::error::Error) -> Self {
        // 共有 crate の variant を既存の variant へ写し、表示 (decode: / video toolbox: /
        // opus: ...) を保つ。初期化前の H.264 / H.265 は VideoDecoder が警告ログに変えるが、
        // その他の経路から届いた場合も既存の Other 表示にする。
        match e {
            moq_codec::error::Error::Decode(e) => Self::Decode(e),
            #[cfg(target_os = "macos")]
            moq_codec::error::Error::VideoToolbox(e) => Self::VideoToolbox(e),
            moq_codec::error::Error::Opus(e) => Self::Opus(e),
            e @ moq_codec::error::Error::NotInitialized { .. } => Self::Other(e.to_string()),
            moq_codec::error::Error::Other(msg) => Self::Other(msg),
        }
    }
}

impl From<raw_player::error::Error> for Error {
    fn from(e: raw_player::error::Error) -> Self {
        Self::Player(e)
    }
}

#[cfg(target_os = "macos")]
impl From<shiguredo_video_toolbox::Error> for Error {
    fn from(e: shiguredo_video_toolbox::Error) -> Self {
        Self::VideoToolbox(e)
    }
}

#[cfg(target_os = "macos")]
impl From<shiguredo_audio_toolbox::Error> for Error {
    fn from(e: shiguredo_audio_toolbox::Error) -> Self {
        Self::AudioToolbox(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<tokio_moq::error::TransportError> for Error {
    fn from(e: tokio_moq::error::TransportError) -> Self {
        // トランスポート種別に依存しない失敗 (Internal / InvalidAuthority /
        // ResolutionFailed / Transport / Unsupported) は Other にして `QUIC:` や
        // `WebTransport:` を付けない。セッション終了とストリーム終端は専用 variant に分け、
        // WebTransport 系は表示を保ったまま明示 match する。catch-all を置かないことで、
        // TransportError に variant が増えたときにコンパイルエラーになる。
        match e {
            tokio_moq::error::TransportError::Quic(msg) => Self::Quic(msg),
            // 統合した MoqtClient 由来の内部エラーは従来の Other 表示に合わせる
            tokio_moq::error::TransportError::Internal(msg) => Self::Other(msg),
            // catch-all に落とすと WebTransport variant になり `WebTransport:` が付くため明示する
            tokio_moq::error::TransportError::InvalidAuthority(msg)
            | tokio_moq::error::TransportError::ResolutionFailed(msg) => Self::Other(msg),
            // セッション終了は表示文字列ではなく variant で判定できるように専用にする。
            // peer 起点の終了コードは `SessionEvent::PeerClosed` としてアプリへ届くため、
            // transport のエラーとしてはセッション終了だけを伝える
            tokio_moq::error::TransportError::ConnectionClosed
            | tokio_moq::error::TransportError::PeerSessionClosed { .. } => Self::ConnectionClosed,
            // peer の STOP_SENDING によるストリーム終端も、セッション終了と区別して
            // 判定できるように専用にする (pipeline は致命エラーにしない)
            tokio_moq::error::TransportError::StreamReset { error_code } => {
                Self::StreamReset { error_code }
            }
            // encode / decode 失敗は main ループが終了コード (PROTOCOL_VIOLATION など) を
            // 決めるため MessageError の variant を保って伝える
            tokio_moq::error::TransportError::Moqt(e) => Self::Moqt(e),
            // QMux 経路のエラーは WebTransport と区別して表示する
            tokio_moq::error::TransportError::Qmux(e) => Self::Other(format!("QMux: {e}")),
            // 下位トランスポートと未実装操作は特定のトランスポートに紐づかないため Other
            tokio_moq::error::TransportError::Transport(e) => Self::Other(e.to_string()),
            tokio_moq::error::TransportError::Unsupported(msg) => Self::Other(msg),
            // WebTransport 系は表示を変えずに明示する
            tokio_moq::error::TransportError::Http3(e) => {
                Self::WebTransport(format!("http3 error: {e}"))
            }
            tokio_moq::error::TransportError::Http2(e) => {
                Self::WebTransport(format!("http2 error: {e}"))
            }
            tokio_moq::error::TransportError::WtH2(e) => {
                Self::WebTransport(format!("webtransport over http2 error: {e}"))
            }
            tokio_moq::error::TransportError::ConnectFailed { status } => {
                Self::WebTransport(match status {
                    Some(status) => format!("CONNECT failed with status {status}"),
                    None => "CONNECT failed with no :status header".to_string(),
                })
            }
            tokio_moq::error::TransportError::ProtocolNegotiationFailed { error_code } => {
                Self::WebTransport(format!(
                    "application protocol negotiation failed (error code {error_code:#010x})"
                ))
            }
            tokio_moq::error::TransportError::StreamClosed => {
                Self::WebTransport("stream closed".to_string())
            }
            tokio_moq::error::TransportError::InvalidState(msg) => {
                Self::WebTransport(format!("invalid state: {msg}"))
            }
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
            "MOQT: protocol violation: out of scope parameter"
        );
    }

    /// 下位トランスポートと未実装操作は `Other` になり `WebTransport:` を付けない
    #[test]
    fn transport_error_mapping_uses_other_for_transport_and_unsupported() {
        let unsupported = Error::from(TransportError::Unsupported("not supported".to_string()));
        assert!(matches!(unsupported, Error::Other(_)));
        assert_eq!(unsupported.to_string(), "not supported");
        let transport = Error::from(TransportError::Transport(Box::new(std::io::Error::other(
            "transport boom",
        ))));
        assert!(matches!(transport, Error::Other(_)));
        assert!(!transport.to_string().contains("WebTransport:"));
    }

    /// WebTransport 系の variant は `WebTransport:` を付けたままにする
    #[test]
    fn transport_error_mapping_keeps_webtransport_prefix() {
        let cases = [
            TransportError::ConnectFailed { status: Some(404) },
            TransportError::ProtocolNegotiationFailed { error_code: 1 },
            TransportError::StreamClosed,
            TransportError::InvalidState("bad state".to_string()),
        ];
        for case in cases {
            let err = Error::from(case);
            assert!(
                matches!(err, Error::WebTransport(_)),
                "WebTransport に振り分けられること: {err}"
            );
            assert!(err.to_string().starts_with("WebTransport: "), "{err}");
        }
    }
}
