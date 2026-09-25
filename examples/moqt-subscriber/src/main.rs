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

    // 音声出力の指定はプレイヤー (メインスレッド) が使うため、config を
    // tokio ランタイムへ move する前に取り出しておく
    let audio_output_device = config.audio_output_device;

    // デコード済みフレーム用チャネル (映像 / 音声)
    let (frame_tx, frame_rx) = std::sync::mpsc::channel::<DecodedVideoFrame>();
    let (audio_tx, audio_rx) = std::sync::mpsc::channel::<DecodedAudioFrame>();

    // 表示待ちの映像フレーム数。デコードが表示より速いと増え続け、CPU を飽和させて
    // QUIC エンドポイントの I/O を飢えさせる (moqt-rs 0101)。受信側はこれを見て
    // 古い group を捨てる。
    let display_backlog = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let display_backlog_for_pipeline = std::sync::Arc::clone(&display_backlog);

    // カタログから取得した fps をスレッド間で共有する
    let shared_fps = pipeline::new_shared_fps();
    // tokio runtime の構築はメインスレッドで行う。
    //
    // 別スレッドで構築すると、失敗時にそのスレッドだけが panic し、main は
    // メディアチャネルの切断でループを抜けて終了コード 0 で終わってしまう。
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("Failed to create tokio runtime: {e}");
            std::process::exit(1);
        }
    };
    // macOS では SDL のウィンドウ操作をメインスレッドで行う必要がある。
    // MoQT 処理は別スレッドの tokio ランタイムで動かす。
    std::thread::spawn(move || {
        rt.block_on(async move {
            // タスクメトリクスを定期的にログ出力する
            moqt_example_transport::metrics::spawn_task_metrics_logger(task_monitor.clone());

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
                display_backlog_for_pipeline,
            )
            .await
            {
                tracing::error!("Fatal: {e}");
                std::process::exit(1);
            }
        });
    });

    // メインスレッドで raw_player を動かす
    if let Err(e) = run_raw_player(frame_rx, audio_rx, display_backlog, audio_output_device) {
        tracing::error!("Fatal: {e}");
        std::process::exit(1);
    }
}

/// メインスレッドで raw_player のイベントループを実行する
///
/// macOS では SDL のウィンドウ操作がメインスレッドでしか動作しないため、
/// raw_player はメインスレッドで動かし、MoQT 処理は別スレッドで実行する。
///
/// SDL の初期化やウィンドウ・レンダラー作成に失敗する環境でも panic せず、`Error::Player` として失敗を返す。
///
/// `audio_output_device` が [`cli::AudioOutputDevice::None`] のときは SDL の音声出力デバイスを
/// 開かず、受信済みの音声チャンクを数えるだけにする (スピーカーへ音を出さない)。
fn run_raw_player(
    video_rx: std::sync::mpsc::Receiver<DecodedVideoFrame>,
    audio_rx: std::sync::mpsc::Receiver<DecodedAudioFrame>,
    display_backlog: std::sync::Arc<std::sync::atomic::AtomicI64>,
    audio_output_device: cli::AudioOutputDevice,
) -> error::Result<()> {
    raw_player::init()?;
    tracing::info!("Player initialized, waiting for frames...");

    let mut video_player: Option<raw_player::VideoPlayer> = None;
    // 音声出力を行わない指定では AudioPlayer を作らない。
    // raw_player の AudioPlayer は生成時点ではデバイスを開かず play() で開くが、
    // 生成自体を避けることで音声出力の経路へ一切入らないようにする。
    let audio_player = match audio_output_device {
        cli::AudioOutputDevice::Default => Some(raw_player::AudioPlayer::new()),
        cli::AudioOutputDevice::None => {
            tracing::info!("Audio output disabled, decoded audio is not played");
            None
        }
    };
    let audio_output_enabled = audio_player.is_some();
    let mut audio_started = false;

    // wall-clock PTS: 再生開始からの経過時間を映像 PTS として使用する
    let start_time = std::time::Instant::now();
    let mut frame_count: u64 = 0;
    let mut audio_chunk_count: u64 = 0;
    let mut video_disconnected = false;
    let mut audio_disconnected = false;
    // 停滞の切り分け用。MOQT_PLAYER_DIAG=1 のときだけ、1 秒ごとに
    // プレイヤーループが生きていることと表示待ちの深さを出す。
    // ループが止まったのか、ループは動いているが供給が止まったのかを分ける。
    let player_diag = std::env::var("MOQT_PLAYER_DIAG").is_ok_and(|v| v == "1");
    let mut last_diag = std::time::Instant::now();
    let player_start = std::time::Instant::now();

    loop {
        if player_diag && last_diag.elapsed() >= std::time::Duration::from_secs(1) {
            last_diag = std::time::Instant::now();
            tracing::info!(
                "PLAYERDIAG t={}ms frames={} audio={} backlog={}",
                player_start.elapsed().as_millis(),
                frame_count,
                audio_chunk_count,
                display_backlog.load(std::sync::atomic::Ordering::Relaxed),
            );
        }
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
                // 表示した分だけ受信側の待ちフレーム数を減らす
                display_backlog.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                let need_recreate = match video_player.as_ref() {
                    None => true,
                    Some(p) => p.width() != frame.width || p.height() != frame.height,
                };
                if need_recreate {
                    tracing::info!("Creating player window: {}x{}", frame.width, frame.height);
                    let player = raw_player::VideoPlayer::new(
                        frame.width,
                        frame.height,
                        "kaki - MoQT Subscriber",
                    )?;
                    player.play()?;
                    video_player = Some(player);
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
                if let Some(audio_player) = audio_player.as_ref() {
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
                }
                audio_chunk_count += 1;
                if audio_chunk_count.is_multiple_of(100) {
                    if audio_output_enabled {
                        tracing::info!("Played {audio_chunk_count} audio chunks");
                    } else {
                        tracing::info!(
                            "Decoded {audio_chunk_count} audio chunks (audio output disabled)"
                        );
                    }
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
    Ok(())
}

/// `&[i16]` をリトルエンディアンのバイト列に変換する
fn pcm_i16_to_bytes(pcm: &[i16]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(pcm.len() * 2);
    for &sample in pcm {
        buf.extend_from_slice(&sample.to_le_bytes());
    }
    buf
}
