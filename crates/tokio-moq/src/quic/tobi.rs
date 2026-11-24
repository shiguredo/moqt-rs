//! tobi-quic を使うヘルパー
//!
//! ネイティブ QUIC のバックエンドと、WebTransport over HTTP/3 の QUIC 層が共有する。
//! WebTransport は常に tobi-quic で接続するため、このモジュールはバックエンドの
//! 選択に関わらずコンパイルされる。

#[cfg(feature = "quic-tobi")]
use tobi_quic::PathEvent;
use tobi_quic::core::connection::ConnectionConfig;
#[cfg(feature = "quic-tobi")]
use tobi_quic::core::time::Duration as TobiDuration;
use tobi_quic::{Connection, DatagramConfig, ReceiveStream};

use super::{QuicError, StopSendingObserved, StopSendingReceiver};
#[cfg(feature = "quic-tobi")]
use crate::error::{Result, TransportError};
#[cfg(feature = "quic-tobi")]
use crate::multipath::{InterfaceFilter, MultipathMode, MultipathOptions};

/// 接続で広告する transport parameter を構築する
///
/// 窓の大きさは、想定する最高ビットレート (4K / 8K の 30 Mbps、2s GOP で
/// 1 group ≒ 7.5MB) を基準に決める。
///
/// - stream 窓 (16 MiB): 30 Mbps でも 2 group 以上を保持できる。credit の返却は
///   消費の半分で行うため、窓が group より小さいと group の途中で送信が止まりうる
/// - 接続窓 (48 MiB): 映像 1 本 + 音声の複数本 + catalog を同時に保持できるぶん
/// - stream 数 (1,000): relay は音声の subgroup stream を 10 秒程度で張り替えるため、
///   累積上限でも長時間の購読に耐える余裕である。FIN を受けて閉じた stream 分は
///   MAX_STREAMS で回収される
///
/// 窓を大きくする代償として、relay が窓ぶんのデータを送信バッファに溜められるため、
/// 購読の遅延の上限が「窓 ÷ ビットレート」で伸びる (30 Mbps で stream 窓 16 MiB なら
/// 最大 4.3 秒)。live でさらに低遅延が要る場合は、窓を縮めるか、消費レートに応じて
/// 窓を伸ばす auto-tune を入れる。
///
/// 遅延 ACK は RFC 9000 Section 13.2.2 の通常動作 (2 個の ack-eliciting パケットを
/// 受けるまで遅延し、max_ack_delay 以内に送る) である。tobi-quic の既定は
/// rebind 時の interop 問題を避けるため無効になっているが、この example では
/// 明示的に有効にする。既定の毎パケット即時 ACK を sora-moq の relay へ使うと、
/// relay のコネクションプロセスが ACK 処理で飽和し、RTT を処理時刻で計測している
/// (kernel 到着時刻を使わない) ため RTT 見積もりが膨らみ、pacing rate =
/// N × cwnd / smoothed_rtt が滑らかに減衰して購読が止まることを実環境で確認した。
///
/// WebTransport のクライアントも同じ値を使う。
pub(crate) fn build_transport_config() -> ConnectionConfig {
    // 遅延 ACK (RFC 9000 Section 13.2.2)。既定 (毎パケット即時 ACK) から変更する
    let mut config = ConnectionConfig {
        delayed_ack: true,
        ..Default::default()
    };

    // ピア (relay) が開ける単方向 stream の累積上限
    config.transport_params.initial_max_streams_uni = 1_000;
    // ピア (relay) が開ける双方向 stream の累積上限
    config.transport_params.initial_max_streams_bidi = 1_000;
    // 接続レベルで受け入れを許す総バイト数
    config.transport_params.initial_max_data = 48 * 1024 * 1024;
    // 単方向 stream (relay -> subscriber の data stream) 1 本あたりの窓
    config.transport_params.initial_max_stream_data_uni = 16 * 1024 * 1024;
    config.transport_params.initial_max_stream_data_bidi_local = 16 * 1024 * 1024;
    config.transport_params.initial_max_stream_data_bidi_remote = 16 * 1024 * 1024;
    // Multipath を使わない接続では、パスの追加を許可しない。tobi-quic の既定は 4 で、
    // ピアが追加パスを開けるとパス検証のためのソケットとタスクが増える。Multipath を
    // 使う場合だけ build_multipath_transport_config が上書きする。
    config.transport_params.initial_max_path_id = None;
    // s2n-quic の既定 (30 秒) に合わせる。tobi-quic の既定は 0 (無効) であり、
    // そのままでは応答しない peer との接続が残り続ける。
    config.transport_params.max_idle_timeout = 30_000;
    config
}

/// Multipath QUIC を有効にする transport parameter を構築する
///
/// `build_transport_config` を基に、Path ID の上限と待機パスの keep-alive を設定する。
/// パスの使い方とインターフェースのフィルタは `ClientBuilder::with_multipath` が
/// `MultipathConfig` で上書きするため、ここでは設定しない。
#[cfg(feature = "quic-tobi")]
pub(crate) fn build_multipath_transport_config(options: &MultipathOptions) -> ConnectionConfig {
    let mut config = build_transport_config();
    config.transport_params.initial_max_path_id = Some(options.initial_max_path_id);
    // 待機パスを middlebox のタイムアウトで失わないよう keep-alive を設定する
    // (RFC 10069 Section 5.9)。未指定の場合は max_idle_timeout から間隔を決める。
    config.keep_alive_interval = options.keep_alive_interval.map(tobi_duration);
    config
}

/// std の Duration を tobi-quic の Duration へ変換する
///
/// tobi-quic はマイクロ秒の u64 で保持する。u64 に収まらない値は上限へ飽和させる。
#[cfg(feature = "quic-tobi")]
fn tobi_duration(duration: std::time::Duration) -> TobiDuration {
    let micros = u64::try_from(duration.as_micros()).unwrap_or(u64::MAX);
    TobiDuration::from_micros(micros)
}

/// tokio-moq の Multipath 設定を tobi-quic の `MultipathConfig` へ変換する
///
/// `MultipathMode::Off` の場合は `None` を返す。呼び出し側は `Some` のときだけ
/// `ClientBuilder::with_multipath` を呼ぶ。
#[cfg(feature = "quic-tobi")]
pub(crate) fn build_multipath_config(
    options: &MultipathOptions,
) -> Option<tobi_quic::MultipathConfig> {
    let mode = match options.mode {
        MultipathMode::Off => return None,
        MultipathMode::Handover => tobi_quic::MultipathMode::Handover,
        MultipathMode::Aggregate => tobi_quic::MultipathMode::Aggregate,
        MultipathMode::Backup => tobi_quic::MultipathMode::Backup,
    };
    let interface_filter = match &options.interface_filter {
        InterfaceFilter::All => tobi_quic::InterfaceFilter::All,
        InterfaceFilter::Allow(names) => tobi_quic::InterfaceFilter::Allow(names.clone()),
        InterfaceFilter::Deny(names) => tobi_quic::InterfaceFilter::Deny(names.clone()),
    };
    Some(tobi_quic::MultipathConfig {
        mode,
        interface_filter,
        max_paths: options.max_paths,
        initial_max_path_id: options.initial_max_path_id,
        // 同時に検証するパス数の既定は 4 である。MOQT のメディア転送では複数パスを
        // 同時に検証する場面が限られるため、tobi-quic の既定をそのまま使う。
        max_concurrent_validations: 4,
    })
}

/// Aggregate モードで複数パスを実際に使うためのスケジューラー構成
///
/// tobi-quic の既定は MinRtt であり、最短 RTT の 1 本にだけ新しいデータを載せる。
/// そのまま Aggregate モードにしても複数パスが使われないため、帯域推定に比例して
/// 振り分ける WeightedRoundRobin と、データを受信したパスで ACK を返す SamePath を
/// 組み合わせる。帯域を使うスケジューラーは BBR 系の輻輳制御と組み合わせる必要がある
/// (tobi-quic の Multipath の資料を参照)。
#[cfg(feature = "quic-tobi")]
pub(crate) fn aggregate_scheduler_config() -> tobi_quic::SchedulerConfig {
    tobi_quic::SchedulerConfig {
        data: tobi_quic::DataSchedulerKind::WeightedRoundRobin,
        ack: tobi_quic::AckPathPolicyKind::SamePath,
        retx: tobi_quic::RetransmissionPolicyKind::SamePathThenCrossPath,
    }
}

/// WebTransport over HTTP/3 用の transport parameter を構築する
///
/// draft-ietf-webtrans-http3-17 §3.1: WebTransport の端点は空の `reset_stream_at`
/// transport parameter を広告する。ネイティブ QUIC のバックエンドは従来の
/// [`build_transport_config`] を使い、この設定を広告しない。
pub(crate) fn build_webtransport_transport_config() -> ConnectionConfig {
    let mut config = build_transport_config();
    config.transport_params.reset_stream_at = true;
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
/// tobi-quic の `receive_vec` が所有 `Vec<u8>` を返すため、バッファの管理も写しも要らない。
/// FIN とリセットの再読み取りは tobi-quic 側が冪等に扱うため、終端を覚える必要はない。
#[derive(Debug)]
pub struct RecvStream {
    /// tobi-quic の受信ストリーム
    inner: ReceiveStream,
}

impl RecvStream {
    /// 受信ストリームを包む
    pub(crate) fn new(inner: ReceiveStream) -> Self {
        Self { inner }
    }

    /// ストリーム ID を返す
    #[cfg(feature = "quic-tobi")]
    pub(crate) fn id(&self) -> u64 {
        self.inner.id()
    }

    /// データまたは FIN を受信する
    ///
    /// `Ok(Some(data))` はデータ、`Ok(None)` は FIN を表す。RESET_STREAM は
    /// [`QuicError::Reset`] として返す (Reliable Size を含む)。
    pub(crate) async fn receive(&mut self) -> std::result::Result<Option<Vec<u8>>, QuicError> {
        let chunk = self
            .inner
            .receive_vec(crate::qmux::READ_BUFFER_SIZE)
            .await
            .map_err(map_error)?;
        if !chunk.data.is_empty() {
            // FIN とデータが同じ読み取りで返ることがある。データを先に返し、
            // FIN は次の呼び出しで返す
            return Ok(Some(chunk.data));
        }
        if chunk.fin {
            return Ok(None);
        }
        // データも FIN も無い読み取りは、バッファ長が 0 のとき以外は起きない
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

/// パスの開設・検証・切り替えを観測するタスクを起動する
///
/// Multipath QUIC を有効にした接続だけが使う。イベントは診断ログに出すだけで、MOQT 層の
/// 状態には影響しない。QUIC のパスは MOQT のセッションからは透過であり、MOQT の
/// メッセージはどのパスで運ばれても同じように処理できる。
///
/// タスクは接続が終了すると終わる。パスの検証失敗と peer のパス廃棄は、接続が継続して
/// いても通信品質に影響しうるため警告で出す (RFC 10069 Section 3.4)。
#[cfg(feature = "quic-tobi")]
pub(crate) fn spawn_path_observer(connection: &Connection) {
    let connection = connection.clone();
    tokio::spawn(async move {
        // 接続が終了すると `accept_path_event` がエラーを返し、ループが終わる
        while let Ok(event) = connection.accept_path_event().await {
            log_path_event(event);
        }
    });
}

/// パスイベントをログに出す
///
/// Path ID は接続内で一意な識別子であり、ローカル / リモートのアドレスと合わせて
/// どの回線が使われているかを追えるようにする。
#[cfg(feature = "quic-tobi")]
fn log_path_event(event: PathEvent) {
    match event {
        PathEvent::Opened {
            path,
            local_addr,
            remote_addr,
        } => tracing::info!(
            path_id = path.id(),
            %local_addr,
            %remote_addr,
            "multipath path opened"
        ),
        PathEvent::Validated { path } => {
            tracing::info!(path_id = path.id(), "multipath path validated")
        }
        PathEvent::ValidationFailed { path } => {
            tracing::warn!(path_id = path.id(), "multipath path validation failed")
        }
        PathEvent::StatusChanged { path, status } => {
            tracing::debug!(
                path_id = path.id(),
                ?status,
                "multipath path status changed"
            )
        }
        PathEvent::PeerStatusChanged { path, status } => {
            tracing::debug!(
                path_id = path.id(),
                ?status,
                "multipath peer path status changed"
            )
        }
        PathEvent::Closed { path } => {
            tracing::debug!(path_id = path.id(), "multipath path closed")
        }
        PathEvent::Abandoned { path, error_code } => tracing::warn!(
            path_id = path.id(),
            error_code,
            "multipath path abandoned by peer"
        ),
        PathEvent::Blocked { maximum_path_id } => tracing::debug!(
            maximum_path_id,
            "multipath peer is blocked by the path ID limit"
        ),
        PathEvent::ObservedAddress { path, addr } => {
            tracing::debug!(
                path_id = path.id(),
                %addr,
                "multipath observed address"
            )
        }
    }
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
            // 「パケットが来ていない」のか「来ているが復号に失敗している」のかを
            // 切り分けられるよう、破棄の内訳と STOP_SENDING の取りこぼしも出す。
            // バイト数と自側の送出量も出し、ピアの送出量の推移と ACK の量を追えるようにする
            tracing::warn!(
                "PKTDIAG received={} rx_bytes={} lost={} dropped={} dropped_decryption_failure={} \
                 dropped_invalid={} stop_sending_dropped={} sent={} tx_bytes={} retransmitted={}",
                stats.received_packets,
                stats.received_bytes,
                stats.lost_packets,
                stats.packet_dropped,
                stats.packet_dropped_decryption_failure,
                stats.packet_dropped_invalid,
                stats.stop_sending_dropped,
                stats.sent_packets,
                stats.sent_bytes,
                stats.retransmitted_packets,
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

/// tobi-quic のエラーを共通の QUIC エラーへ写す
pub(crate) fn map_error(error: tobi_quic::Error) -> QuicError {
    QuicError::from(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WtH3 の transport config だけが reset_stream_at を広告する
    ///
    /// draft-ietf-webtrans-http3-17 §3.1: WebTransport の端点は空の `reset_stream_at`
    /// transport parameter を広告する。ネイティブ QUIC は広告しない。
    #[test]
    fn webtransport_config_advertises_reset_stream_at_only_for_webtransport() {
        assert!(
            build_webtransport_transport_config()
                .transport_params
                .reset_stream_at
        );
        assert!(!build_transport_config().transport_params.reset_stream_at);
    }
}
