//! MoQT サブスクライバークライアント
//!
//! リレーサーバーに接続し、指定トラックを SUBSCRIBE してデータを受信・デコード・再生する。
//! draft-ietf-moq-transport-21、draft-ietf-moq-loc-04、draft-ietf-moq-msf-01 に準拠。
//!
//! 使い方:
//!   cargo run -p moqt-subscriber -- --url moqt://127.0.0.1:4443
mod cli;
mod decoder;
mod error;
mod pipeline;
mod stream_reader;

use decoder::{DecodedAudioFrame, DecodedVideoFrame};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
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

    // デコード済みフレーム用チャネル (映像 / 音声)
    let (frame_tx, frame_rx) = std::sync::mpsc::channel::<DecodedVideoFrame>();
    let (audio_tx, audio_rx) = std::sync::mpsc::channel::<DecodedAudioFrame>();

    // カタログから取得した fps をスレッド間で共有する
    let shared_fps = pipeline::new_shared_fps();
    // macOS では SDL のウィンドウ操作をメインスレッドで行う必要がある。
    // MoQT 処理は別スレッドの tokio ランタイムで動かす。
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        rt.block_on(async move {
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

            if let Err(e) = pipeline::run(
                config,
                frame_tx,
                audio_tx,
                shared_fps,
                task_monitor,
                shutdown_monitor,
            )
            .await
            {
                tracing::error!("Fatal: {e}");
                std::process::exit(1);
            }
        });
    });

    // メインスレッドで raw_player を動かす
    run_raw_player(frame_rx, audio_rx);
}

/// メインスレッドで raw_player のイベントループを実行する
///
/// macOS では SDL のウィンドウ操作がメインスレッドでしか動作しないため、
/// raw_player はメインスレッドで動かし、MoQT 処理は別スレッドで実行する。
fn run_raw_player(
    video_rx: std::sync::mpsc::Receiver<DecodedVideoFrame>,
    audio_rx: std::sync::mpsc::Receiver<DecodedAudioFrame>,
) {
    raw_player::init().expect("failed to init raw_player");
    tracing::info!("Player initialized, waiting for frames...");

    let mut video_player: Option<raw_player::VideoPlayer> = None;
    let audio_player = raw_player::AudioPlayer::new();
    let mut audio_started = false;

    // wall-clock PTS: 再生開始からの経過時間を映像 PTS として使用する
    let start_time = std::time::Instant::now();
    let mut frame_count: u64 = 0;
    let mut audio_chunk_count: u64 = 0;
    let mut video_disconnected = false;
    let mut audio_disconnected = false;

    loop {
        if let Some(ref p) = video_player {
            match p.poll_events() {
                Ok(true) => {}
                _ => {
                    tracing::info!("Player window closed");
                    break;
                }
            }
        }

        match video_rx.try_recv() {
            Ok(frame) => {
                let need_recreate = match video_player.as_ref() {
                    None => true,
                    Some(p) => p.width() != frame.width || p.height() != frame.height,
                };
                if need_recreate {
                    tracing::info!("Creating player window: {}x{}", frame.width, frame.height);
                    video_player = Some(
                        raw_player::VideoPlayer::new(
                            frame.width,
                            frame.height,
                            "kaki - MoQT Subscriber",
                        )
                        .expect("failed to create player"),
                    );
                    if let Some(ref p) = video_player {
                        p.play().expect("failed to play");
                    }
                }

                if let Some(ref p) = video_player {
                    let pts_us = start_time.elapsed().as_micros() as i64;
                    if let Err(e) = p.enqueue_video_i420(
                        &frame.y,
                        &frame.u,
                        &frame.v,
                        frame.width,
                        frame.height,
                        pts_us,
                    ) {
                        tracing::warn!("Failed to enqueue video frame: {e}");
                    }

                    frame_count += 1;
                    if frame_count.is_multiple_of(100) {
                        tracing::info!("Rendered {frame_count} video frames");
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if !video_disconnected {
                    tracing::info!("Video channel disconnected");
                    video_disconnected = true;
                }
            }
        }

        match audio_rx.try_recv() {
            Ok(audio) => {
                let pcm_bytes = pcm_i16_to_bytes(&audio.pcm);
                if let Err(e) = audio_player.enqueue_audio(
                    &pcm_bytes,
                    audio.pts_us,
                    audio.sample_rate as i32,
                    audio.channels as i32,
                    raw_player::AudioFormat::S16,
                ) {
                    tracing::warn!("Failed to enqueue audio chunk: {e}");
                }
                if !audio_started {
                    if let Err(e) = audio_player.play() {
                        tracing::warn!("Failed to start audio playback: {e}");
                    } else {
                        audio_started = true;
                        tracing::info!("Audio playback started");
                    }
                }
                if let Err(e) = audio_player.process() {
                    tracing::warn!("Failed to process audio queue: {e}");
                }
                audio_chunk_count += 1;
                if audio_chunk_count.is_multiple_of(100) {
                    tracing::info!("Played {audio_chunk_count} audio chunks");
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if !audio_disconnected {
                    tracing::info!("Audio channel disconnected");
                    audio_disconnected = true;
                }
            }
        }

        if video_disconnected && audio_disconnected {
            tracing::info!("All media channels disconnected, stopping player");
            break;
        }

        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    tracing::info!(
        "Player stopped after {frame_count} video frames / {audio_chunk_count} audio chunks"
    );
    drop(video_player);
    drop(audio_player);
    // SAFETY: プレイヤーループ終了後に一度だけ呼び出す
    unsafe { raw_player::quit() };
}

/// `&[i16]` をリトルエンディアンのバイト列に変換する
fn pcm_i16_to_bytes(pcm: &[i16]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(pcm.len() * 2);
    for &sample in pcm {
        buf.extend_from_slice(&sample.to_le_bytes());
    }
    buf
}
