//! tobi-quic を使うヘルパー
//!
//! ネイティブ QUIC のバックエンドと、WebTransport over HTTP/3 の QUIC 層が共有する。
//! WebTransport は常に tobi-quic で接続するため、このモジュールはバックエンドの
//! 選択に関わらずコンパイルされる。

use tobi_quic::core::connection::ConnectionConfig;
use tobi_quic::{Connection, DatagramConfig, ReceiveStream, SendStream};

use super::{QuicError, StopSendingObserved, StopSendingReceiver};
#[cfg(feature = "quic-tobi")]
use crate::error::{Result, TransportError};

/// 接続で広告する transport parameter を構築する
///
/// tobi-quic の `ConnectionConfig` の既定は、この example が必要とするストリーム数と
/// ウィンドウに対して小さい (単方向 100 本 / 双方向 100 本 / 接続 1 MiB / ストリーム 64 KiB)。
/// この example は audio を 20ms ごと、video を 1 frame ごとに 1 本の subgroup stream で
/// 受けるため、stream の消費速度が上限の回復速度を上回り、購読が数秒から数十秒で止まる。
/// 上限とウィンドウを明示して余裕を持たせる。
///
/// 値は s2n-quic の既定 (100 stream / 3.75MB) に対して、この example が
/// 必要とする同時実行数 (MAX_CONCURRENT_STREAMS = 4) と 1 group あたりの
/// サイズから決めている。過大にしても受信側が accept しなければ flow control
/// で止まるだけなので、上限そのものは大きくて困らない。
///
/// WebTransport のクライアントも同じ値を使う。
pub(crate) fn build_transport_config() -> ConnectionConfig {
    let mut config = ConnectionConfig::default();
    // ピア (relay) が開ける単方向 stream の累積上限
    config.transport_params.initial_max_streams_uni = 10_000;
    // ピア (relay) が開ける双方向 stream の累積上限
    config.transport_params.initial_max_streams_bidi = 1_000;
    // 接続レベルで受け入れを許す総バイト数
    config.transport_params.initial_max_data = 64 * 1024 * 1024;
    // 単方向 stream (relay -> subscriber の data stream) 1 本あたりの窓
    config.transport_params.initial_max_stream_data_uni = 16 * 1024 * 1024;
    config.transport_params.initial_max_stream_data_bidi_local = 16 * 1024 * 1024;
    config.transport_params.initial_max_stream_data_bidi_remote = 16 * 1024 * 1024;
    // MOQT は QUIC の multipath 拡張を使わないため、パスの追加を許可しない。
    // 既定は 4 で、ピアが追加パスを開けると本 example が扱えない検証処理が走る
    config.transport_params.initial_max_path_id = None;
    // s2n-quic の既定 (30 秒) に合わせる。tobi-quic の既定は 0 (無効) であり、
    // そのままでは応答しない peer との接続が残り続ける
    config.transport_params.max_idle_timeout = 30_000;
    config
}

/// DATAGRAM を有効にする設定
///
/// MOQT は DATAGRAM (RFC 9221) で音声とメディアの一部を運ぶため、DATAGRAM を広告する。
pub(crate) fn datagram_config() -> DatagramConfig {
    DatagramConfig::default()
}

/// 受信ストリーム
///
/// tobi-quic の `receive` は、データと FIN を同じ読み取りで返すことがある。また、FIN を
/// 観測した後に再度 `receive` を呼ぶと `STREAM_STATE_ERROR` になる。MOQT 層は「もう一度
/// 呼べば FIN が返る」前提で書かれているため、FIN を観測したことをこの型で覚え、以降は
/// 接続を触らずに `None` を返す。
pub struct RecvStream {
    /// tobi-quic の受信ストリーム
    inner: tobi_quic::ReceiveStream,
    /// 読み出し用のバッファ (初回の読み出しまで確保しない)
    buf: Option<Box<[u8]>>,
    /// FIN を観測したか
    fin: bool,
}

impl RecvStream {
    /// 受信ストリームを包む
    pub(crate) fn new(inner: ReceiveStream) -> Self {
        Self {
            inner,
            buf: None,
            fin: false,
        }
    }

    /// ストリーム ID を返す
    #[cfg(feature = "quic-tobi")]
    pub(crate) fn id(&self) -> u64 {
        self.inner.id()
    }

    /// データまたは FIN を受信する
    ///
    /// `Ok(Some(data))` はデータ、`Ok(None)` は FIN を表す。RESET_STREAM は
    /// [`QuicError::Reset`] として返す。
    pub(crate) async fn receive(&mut self) -> std::result::Result<Option<Vec<u8>>, QuicError> {
        if self.fin {
            // FIN を観測した後の `receive` はストリームの状態エラーになるため呼ばない
            return Ok(None);
        }
        let buf = self
            .buf
            .get_or_insert_with(|| vec![0u8; crate::qmux::READ_BUFFER_SIZE].into_boxed_slice());
        let read = match self.inner.receive(buf).await {
            Ok(read) => read,
            Err(tobi_quic::Error::Stream(tobi_quic::StreamError::Reset { error_code, .. })) => {
                return Err(QuicError::Reset { error_code });
            }
            Err(e) => return Err(map_error(e)),
        };
        if read.len > 0 {
            // FIN とデータが同じ読み取りで返ることがある。FIN は次の呼び出しで返す
            self.fin = read.fin;
            return Ok(Some(buf[..read.len].to_vec()));
        }
        if read.fin {
            self.fin = true;
            return Ok(None);
        }
        // データも FIN も無い読み取りは、接続を進めない限り起きない。空のデータとして
        // 返すと呼び出し側が空回りするため、エラーとして扱う
        Err(QuicError::Other(
            "the stream returned neither data nor FIN".into(),
        ))
    }

    /// 受信方向へ STOP_SENDING を送出する
    pub(crate) async fn stop_sending(
        &mut self,
        error_code: u64,
    ) -> std::result::Result<(), QuicError> {
        self.inner.stop_sending(error_code).await.map_err(map_error)
    }
}

/// 接続の観測タスク
///
/// 接続を確立したら、peer の STOP_SENDING を MOQT 層へ流すタスクを 1 本起動する。
///
/// tobi-quic は受信した STOP_SENDING を送信 API の失敗 (`StreamError::Stopped`) としてしか
/// 通知しないため、書き込みを続けない request stream では cancel の検知が遅れる。これを
/// 補うため、`Connection::accept_stop_sending` を待ち続けるタスクを立てる。
///
/// 観測は診断ではなく MOQT 層へ渡す必要のある情報であるため、`MOQT_PACKET_DIAG` の有無に
/// 関わらず常に行う。
///
/// タスクは接続が終了するか receiver が落ちると終わる。
pub(crate) fn spawn_connection_observer(connection: &Connection) -> StopSendingReceiver {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let connection = connection.clone();
    tokio::spawn(async move {
        // 接続が終了すると `accept_stop_sending` がエラーを返し、ループが終わる。
        // ループの終了で sender が drop され、receiver も閉じる
        while let Ok((stream_id, error_code)) = connection.accept_stop_sending().await {
            if tx
                .send(StopSendingObserved {
                    stream_id,
                    error_code,
                })
                .is_err()
            {
                // MOQT 層が観測を止めた。これ以上イベントを溜めない
                break;
            }
        }
    });
    rx
}

/// 受信パケット数とロスト数を 1 秒ごとにログ出力する診断タスクを起動する
///
/// tobi-quic の公開 API は復号失敗の回数を通知しないため、「破棄」と「復号失敗」の
/// 内訳は取得できない。受信数とロスト数だけを追い、「パケットが来ていない」のか
/// 「来ているが損失している」のかを切り分ける。
///
/// 常時出力すると計測そのものを歪めるため、環境変数で明示的に有効化する。
pub(crate) fn spawn_packet_diagnostics(connection: &Connection) {
    if !super::packet_diag_enabled() {
        return;
    }
    tracing::warn!("packet diagnostics are enabled (MOQT_PACKET_DIAG=1)");
    let connection = connection.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                // 接続が終了したら診断も止める。カウンタは増えない
                _ = connection.closed() => return,
            }
            let stats = connection.stats().await;
            tracing::warn!(
                "PKTDIAG received={} lost={}",
                stats.received_packets,
                stats.lost_packets,
            );
        }
    });
}

/// 接続をアプリケーションエラーで閉じる
///
/// tobi-quic の `close_application` は 62 ビット varint に収まらないコードを拒否する。
/// MOQT のコードは §13 (Grease) の greasing 値を含み u64 全域を取り得るため、s2n-quic の
/// `application::Error::new(code).unwrap_or(UNKNOWN)` と同じく、収まらないコードは
/// `UNKNOWN` (0) で閉じる。
///
/// 戻り値は実際に載せたコードである。
#[cfg(feature = "quic-tobi")]
pub(crate) async fn close_application(connection: &Connection, code: u64) -> Result<u64> {
    let code = if code <= MAX_VARINT { code } else { 0 };
    connection
        .close_application(code, &[])
        .await
        .map_err(|e| TransportError::Quic(format!("failed to close the connection: {e}")))?;
    Ok(code)
}

/// QUIC の可変長整数で表せる最大値 (RFC 9000 §16)
#[cfg(feature = "quic-tobi")]
const MAX_VARINT: u64 = (1 << 62) - 1;

/// ストリームへデータをすべて送る
///
/// tobi-quic の `SendStream::send` は、フロー制御の都合で受け付けたバイト数を返し、
/// 引数の全体を送り切らないことがある。MOQT 層は「渡したデータがすべて送られた」ことを
/// 前提にしているため、すべて受け付けられるまで繰り返す。
///
/// 空の入力は送信を開始しない (`Ok(0)`)。呼び出し側が空の入力を渡した場合は、そのまま
/// 成功として扱う。
pub(crate) async fn send_all(
    stream: &mut SendStream,
    data: &[u8],
) -> std::result::Result<(), tobi_quic::Error> {
    let mut offset = 0;
    while offset < data.len() {
        let sent = stream.send(&data[offset..]).await?;
        if sent == 0 {
            // 空でない入力で 0 バイトしか受け付けないのは、接続の終了以外では起きない
            return Err(tobi_quic::Error::Internal(
                "the stream accepted no data while sending".into(),
            ));
        }
        offset += sent;
    }
    Ok(())
}

/// tobi-quic のエラーを共通の QUIC エラーへ写す
pub(crate) fn map_error(error: tobi_quic::Error) -> QuicError {
    QuicError::from(error)
}
