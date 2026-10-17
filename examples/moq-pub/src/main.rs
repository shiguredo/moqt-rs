//! MOQT パブリッシャークライアント
//!
//! カメラ / マイクから取得した映像・音声をエンコードし、MOQT relay へ PUBLISH する。
//! `--input-mp4` を指定した場合はカメラ / マイクを使わず、MP4 ファイルの映像トラックを
//! 再エンコードせずに PUBLISH する。`--input-mp4-reencode` を指定した場合は MP4 ファイルの
//! 映像 / 音声をデコードして再エンコードして PUBLISH する。
//! draft-ietf-moq-transport-22、draft-ietf-moq-loc-04、draft-ietf-moq-msf-01 に準拠。
//!
//! 使い方:
//!   cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --namespace moq-example --fake-capture-device
//!   cargo run -p moq-pub -- --url 'moqt://127.0.0.1:4443#msf:moq-example--video' --fake-capture-device
//!
//! `--namespace` を省略した場合は `--url` の `msf` fragment の track-identifier が示す
//! namespace を使う。どちらにも無い場合はエラーになる。
//!
//! `--transport` を省略した場合は `--url` の `msf` fragment の `connection` を使う
//! (draft-ietf-moq-msf-01 §11.1.1)。どちらにも無い場合は `quic` になる。`connection` と矛盾する
//! `--transport` はエラーになる。
//!
//! パイプライン本体は lib ターゲット (`lib.rs`) にあり、このバイナリは tracing の初期化、
//! Ctrl+C と SIGTERM の待ち受け、タスクメトリクスのログ出力、終了コードの決定を行う。

use moq_pub::{cli, pipeline};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = match cli::parse() {
        Ok(Some(config)) => config,
        Ok(None) => return,
        Err(e) => {
            eprintln!("{e:?}");
            std::process::exit(1);
        }
    };

    let task_monitor = tokio_metrics::TaskMonitor::new();
    let shutdown = tokio_utils::ShutdownController::new();
    let shutdown_monitor = shutdown.subscribe();

    // タスクメトリクスを定期的にログ出力する
    tokio_moq::metrics::spawn_task_metrics_logger(task_monitor.clone());

    // Ctrl+C (SIGINT) と SIGTERM で graceful shutdown を起動する
    tokio::spawn(async move {
        let received = wait_for_shutdown_signal().await;
        tracing::info!("Received {received}, initiating graceful shutdown");
        shutdown.shutdown().await;
    });

    if let Err(e) = pipeline::run(config, task_monitor, shutdown_monitor).await {
        tracing::error!("Fatal: {e}");
        std::process::exit(1);
    }
}

/// 終了を要求するシグナルを待ち、受け取ったシグナルの名前を返す
///
/// Ctrl+C (SIGINT) と SIGTERM を待つ。どちらも graceful shutdown の起動に使う。
/// SIGTERM を登録できない環境では Ctrl+C だけを待つ (終了の手段を残すため)。
async fn wait_for_shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(terminate) => Some(terminate),
            Err(e) => {
                tracing::warn!("Failed to listen for SIGTERM: {e}");
                None
            }
        };
        match terminate.as_mut() {
            Some(terminate) => tokio::select! {
                _ = tokio::signal::ctrl_c() => "Ctrl+C",
                _ = terminate.recv() => "SIGTERM",
            },
            None => {
                tokio::signal::ctrl_c().await.ok();
                "Ctrl+C"
            }
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
        "Ctrl+C"
    }
}
