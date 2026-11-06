//! QUIC クライアント接続の確立
//!
//! TLS / ALPN / DATAGRAM を設定し、選択した QUIC 実装で接続を確立する。
//!
//! QUIC の実装は cargo feature で選ぶ。3 つから 1 つだけを有効にすること。
//!
//! - `quic-tobi` (既定): tobi-quic
//! - `quic-s2n`: s2n-quic
//! - `quic-ngtcp2`: ngtcp2 (shiguredo_ngtcp2_tokio)
//!
//! WebTransport over HTTP/3 は [tobi](tobi) を直接使うため、この選択の対象外である。
//! そのため `tobi_quic` は常に依存に含まれる。

use tokio::sync::mpsc;

use crate::error::TransportError;

/// ALPN プロトコル識別子 (draft-ietf-moq-transport-23 §6.2 (Session establishment))
///
/// WebTransport の `WT-Available-Protocols` と共通の定数を使う。
pub(crate) const ALPN: &[u8] = crate::MOQT_PROTOCOL.as_bytes();

#[cfg(not(any(
    all(
        feature = "quic-tobi",
        not(feature = "quic-s2n"),
        not(feature = "quic-ngtcp2")
    ),
    all(
        feature = "quic-s2n",
        not(feature = "quic-tobi"),
        not(feature = "quic-ngtcp2")
    ),
    all(
        feature = "quic-ngtcp2",
        not(feature = "quic-tobi"),
        not(feature = "quic-s2n")
    ),
)))]
compile_error!("exactly one QUIC backend must be selected: quic-tobi / quic-s2n / quic-ngtcp2");

/// tobi-quic を使うヘルパー
///
/// WebTransport over HTTP/3 は常に tobi-quic で接続するため、このモジュールは
/// ネイティブ QUIC のバックエンドに関わらず常にコンパイルされる。
pub(crate) mod tobi;

#[cfg(feature = "quic-tobi")]
mod tobi_backend;

#[cfg(feature = "quic-s2n")]
mod s2n;

#[cfg(feature = "quic-ngtcp2")]
mod ngtcp2;

#[cfg(feature = "quic-tobi")]
use tobi_backend as backend;

#[cfg(feature = "quic-s2n")]
use s2n as backend;

#[cfg(feature = "quic-ngtcp2")]
use ngtcp2 as backend;

// バックエンドの型は `transport` の公開 enum のフィールドとして外部へ現れるため、
// 型の到達可能性を保つ必要がある。選択されたバックエンドの型だけを再エクスポートする。
pub use backend::{Connected, Connection, RecvStream, SendStream, StreamAcceptor};
pub(crate) use backend::{close_application, connect};

#[cfg(test)]
pub(crate) use backend::{Server, start_server};

/// peer から受信した STOP_SENDING の観測結果
///
/// RFC 9000 §19.5 の STOP_SENDING を、送信ストリームを所有する MOQT 層へ渡すための値である。
/// `error_code` は wire のコードであり、WebTransport over HTTP/3 は HTTP/3 の code space を
/// 共有するため `wt_stop_sending_error_code` で MOQT のコードへ戻す
/// (draft-ietf-webtrans-http3-16 §4.4 (Resetting Data Streams))。
/// WebTransport over HTTP/2 は capsule がアプリケーションエラーコードをそのまま運ぶため
/// remap は不要である (draft-ietf-webtrans-http2-16 §6.3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopSendingObserved {
    /// STOP_SENDING の対象となったストリーム ID
    pub stream_id: u64,
    /// peer が載せた wire のエラーコード
    pub error_code: u64,
}

/// peer の STOP_SENDING の観測を受け取る receiver
///
/// 接続終了で channel が閉じると `recv` は `None` を返す。呼び出し側は receiver を落として
/// 観測を止めること (閉じた receiver を poll し続けると待たずに `None` が返り続ける)。
pub type StopSendingReceiver = mpsc::UnboundedReceiver<StopSendingObserved>;

/// QUIC バックエンドのエラー
///
/// バックエンドごとにエラー型が異なるため、MOQT 層が必要とする分類だけを持つ共通型に
/// 写す。バックエンド固有のメッセージは [`QuicError::Other`] に文字列で保持する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuicError {
    /// peer が STOP_SENDING で送信方向を終端した (RFC 9000 §19.5)
    Stopped {
        /// peer が載せたアプリケーションエラーコード
        error_code: u64,
    },
    /// peer が RESET_STREAM で受信方向を終端した (RFC 9000 §19.4)
    Reset {
        /// peer が載せたアプリケーションエラーコード
        error_code: u64,
    },
    /// 接続が終了した
    ConnectionClosed,
    /// 上記以外のエラー
    Other(String),
}

impl std::fmt::Display for QuicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped { error_code } => {
                write!(f, "stream stopped by peer: error_code={error_code:#x}")
            }
            Self::Reset { error_code } => {
                write!(f, "stream reset by peer: error_code={error_code:#x}")
            }
            Self::ConnectionClosed => write!(f, "connection closed"),
            Self::Other(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for QuicError {}

impl From<QuicError> for TransportError {
    fn from(e: QuicError) -> Self {
        Self::Quic(e.to_string())
    }
}

impl From<tobi_quic::Error> for QuicError {
    fn from(e: tobi_quic::Error) -> Self {
        match e {
            tobi_quic::Error::Stream(tobi_quic::StreamError::Stopped { error_code }) => {
                Self::Stopped { error_code }
            }
            tobi_quic::Error::Stream(tobi_quic::StreamError::Reset { error_code, .. }) => {
                Self::Reset { error_code }
            }
            tobi_quic::Error::Connection(_) => Self::ConnectionClosed,
            tobi_quic::Error::Stream(tobi_quic::StreamError::ConnectionLost(_)) => {
                Self::ConnectionClosed
            }
            e => Self::Other(e.to_string()),
        }
    }
}

/// 接続の終了理由
///
/// テスト用の peer が、client が送った終了コードを観測するために使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseReason {
    /// アプリケーションエラーで閉じた
    Application {
        /// 終了コード
        code: u64,
    },
    /// 正常終了 (終了コード 0)
    Normal,
    /// その他の終了
    Other(String),
}

impl CloseReason {
    /// アプリケーションエラーコードを取り出す
    ///
    /// 正常終了はコード 0 として扱う。終了コードを持たない終了 (タイムアウトなど) は
    /// `None` を返す。
    pub fn application_code(&self) -> Option<u64> {
        match self {
            Self::Application { code } => Some(*code),
            Self::Normal => Some(0),
            Self::Other(_) => None,
        }
    }
}

/// パケットの診断を有効にするか (環境変数 `MOQT_PACKET_DIAG=1` で明示的に有効化)
pub(crate) fn packet_diag_enabled() -> bool {
    std::env::var("MOQT_PACKET_DIAG").is_ok_and(|v| v == "1")
}

/// 交渉されたサブプロトコルが MOQT の識別子かを検証する
///
/// draft-ietf-moq-transport-23 §6.2: draft の ALPN は `moqt-` に draft 番号を付けた値で
/// あり、draft-23 では `moqt-23` である。識別子が一致しない接続は MOQT のセッションを
/// 運べないため、確立しない。
pub(crate) fn verify_alpn(selected: Option<&[u8]>) -> std::result::Result<(), TransportError> {
    if selected == Some(ALPN) {
        return Ok(());
    }
    Err(TransportError::Quic(format!(
        "the server did not select the MOQT ALPN ({}): {}",
        crate::MOQT_PROTOCOL,
        selected.map_or_else(
            || "none".to_string(),
            |value| String::from_utf8_lossy(value).into_owned()
        )
    )))
}

/// 接続終了を表すエラーかどうか
///
/// ストリームの受理は、接続が終了すると [`QuicError::ConnectionClosed`] を返す。
/// MOQT 層は「それ以上ストリームが来ない」ことを `Ok(None)` で扱うため、
/// 接続終了だけを受理の終端として切り分ける。
pub(crate) fn is_connection_closed(error: &QuicError) -> bool {
    matches!(error, QuicError::ConnectionClosed)
}

/// テスト用サーバーの TLS 材料
///
/// バックエンドによって TLS 実装が異なる (rustls / BoringSSL) ため、DER と PEM の
/// 両方を渡す。各バックエンドは必要な方だけを使う。
#[cfg(test)]
pub(crate) struct ServerTls {
    /// サーバー証明書 (DER)
    pub(crate) cert_der: Vec<u8>,
    /// サーバー秘密鍵 (PKCS#8 DER)
    pub(crate) key_der: Vec<u8>,
    /// ALPN に載せる識別子
    pub(crate) alpn: Vec<Vec<u8>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 選択されたサブプロトコルが moqt-23 なら受理する
    #[test]
    fn verify_alpn_accepts_moqt_23() {
        verify_alpn(Some(crate::MOQT_PROTOCOL.as_bytes())).expect("moqt-23 は受理されること");
    }

    /// draft 22 の識別子と選択なしは交渉失敗にする
    ///
    /// draft の ALPN は `moqt-` に draft 番号を付けた値なので、draft 番号が異なる値は
    /// 別のプロトコルである。空の選択 (サーバーが ALPN を返さない場合) も MOQT の
    /// セッションを運べない。
    #[test]
    fn verify_alpn_rejects_other_identifiers() {
        for selected in [
            Some(&b"moqt-22"[..]),
            Some(b"moqt"),
            Some(b"h3"),
            Some(b""),
            None,
        ] {
            let error = verify_alpn(selected).expect_err("拒否されること");
            let message = match &error {
                TransportError::Quic(message) => message,
                other => panic!("TransportError::Quic を期待したが {other:?} だった"),
            };
            assert!(
                message.contains("did not select the MOQT ALPN"),
                "交渉失敗の error であること: {message}"
            );
        }
    }

    /// 接続結果を待つ上限時間 (ハングを失敗として観測する)
    const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    /// 接続が失敗することを確かめ、そのエラーを返す
    ///
    /// `Connected` は `Debug` を実装しないため、`Result::expect_err` は使えない。
    async fn expect_connect_error(addr: std::net::SocketAddr, message: &str) -> TransportError {
        match tokio::time::timeout(TEST_TIMEOUT, connect(addr, "localhost", None)).await {
            Ok(Ok(_)) => panic!("{message}"),
            Ok(Err(error)) => error,
            Err(_) => panic!("上限時間以内に完了すること"),
        }
    }

    /// ALPN を選択しないサーバーとは接続できない
    ///
    /// RFC 9001 §8.1 は、サーバーが互換なアプリケーションプロトコルを選択できない場合を
    /// `no_application_protocol` の接続エラーとすることを MUST とする。QUIC では ALPN の
    /// 選択が必須であり、ALPN を広告しないサーバーとはハンドシェイクが成立しない。
    #[tokio::test(flavor = "multi_thread")]
    async fn connect_rejects_a_peer_without_alpn_selection() {
        let mut server = crate::peer_harness::TestPeerServer::start_with_alpn(&[])
            .await
            .expect("サーバーを起動できること");
        let addr = server.local_addr();
        // サーバーは `accept()` を poll してハンドシェイクを進めるため、接続と並行して回す
        let accept = tokio::spawn(async move { server.accept().await });

        let error = expect_connect_error(addr, "ALPN を選択しない相手とは接続できないこと").await;
        assert!(
            matches!(error, TransportError::Quic(_)),
            "ハンドシェイクの失敗として返ること: {error:?}"
        );
        // ハンドシェイクは失敗するため受理は完了しない。タスクを止める
        accept.abort();
    }

    /// draft 22 の識別子だけを広告するサーバーとは接続できない
    ///
    /// draft の ALPN は `moqt-` に draft 番号を付けた値であり (draft-ietf-moq-transport-23
    /// §6.2)、draft-22 の相手は `moqt-22` しか選択できない。こちらの候補は `moqt-23` だけ
    /// なので、ALPN の交渉で失敗し、draft-22 の相手を draft-23 として受け入れない。
    #[tokio::test(flavor = "multi_thread")]
    async fn connect_rejects_a_draft_22_peer() {
        let mut server = crate::peer_harness::TestPeerServer::start_with_alpn(&[b"moqt-22"])
            .await
            .expect("サーバーを起動できること");
        let addr = server.local_addr();
        // サーバーは `accept()` を poll してハンドシェイクを進めるため、接続と並行して回す
        let accept = tokio::spawn(async move { server.accept().await });

        let error = expect_connect_error(
            addr,
            "draft 22 の識別子しか広告しない相手とは接続できないこと",
        )
        .await;
        assert!(
            matches!(error, TransportError::Quic(_)),
            "ハンドシェイクの失敗として返ること: {error:?}"
        );
        // ハンドシェイクは失敗するため受理は完了しない。タスクを止める
        accept.abort();
    }

    /// 正常終了は終了コード 0 として扱う
    #[test]
    fn close_reason_maps_normal_to_zero() {
        assert_eq!(
            CloseReason::Application { code: 7 }.application_code(),
            Some(7)
        );
        assert_eq!(CloseReason::Normal.application_code(), Some(0));
        assert_eq!(
            CloseReason::Other("timeout".into()).application_code(),
            None
        );
    }
}
