//! MoQT パブリッシャークライアント
//!
//! カメラ / マイクから取得した映像・音声をエンコードし、MoQT relay へ PUBLISH する。
//! draft-ietf-moq-transport-21、draft-ietf-moq-loc-04、draft-ietf-moq-msf-01 に準拠。
//!
//! 使い方:
//!   cargo run -p moqt-publisher -- --url moqt://127.0.0.1:4443 --fake-capture-device
mod audio_capture;
mod capture;
mod catalog;
mod cli;
mod datagram_writer;
mod encoder;
mod error;
mod fake_audio_capture;
mod fake_capture;
mod pipeline;
mod stream_writer;

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
    {
        let task_monitor = task_monitor.clone();
        tokio::spawn(async move {
            loop {
                let intervals: Vec<_> = task_monitor.intervals().collect();
                for m in intervals {
                    tracing::info!(
                        "Task metrics: instrumented={}, dropped={}, first_poll={}, total_poll={}, idle_μs={}, scheduled_μs={}",
                        m.instrumented_count,
                        m.dropped_count,
                        m.first_poll_count,
                        m.total_poll_count,
                        m.total_idle_duration.as_micros(),
                        m.total_scheduled_duration.as_micros(),
                    );
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
    }

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
