//! MoQT パブリッシャークライアント
//!
//! カメラ / マイクから取得した映像・音声をエンコードし、MoQT relay へ PUBLISH する。
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
//! パイプライン本体は lib ターゲット (`lib.rs`) にあり、このバイナリは tracing の初期化、
//! Ctrl+C の待ち受け、タスクメトリクスのログ出力、終了コードの決定を行う。

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

    // Ctrl+C で graceful shutdown を起動する
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Received Ctrl+C, initiating graceful shutdown");
        shutdown.shutdown().await;
    });

    if let Err(e) = pipeline::run(config, task_monitor, shutdown_monitor).await {
        tracing::error!("Fatal: {e}");
        std::process::exit(1);
    }
}
