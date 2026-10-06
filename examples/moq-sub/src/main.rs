//! MoQT サブスクライバークライアント
//!
//! リレーサーバーに接続し、指定トラックを SUBSCRIBE してデータを受信・デコード・再生する。
//! `--mp4` を指定すると受信した映像 / 音声を MP4 ファイルへ保存し、`--no-play` を併せて
//! 指定すると再生せずに受信と保存だけを行う。
//! draft-ietf-moq-transport-22、draft-ietf-moq-loc-04、draft-ietf-moq-msf-01 に準拠。
//!
//! 使い方:
//!   cargo run -p moq-sub -- --url moqt://127.0.0.1:4443
//!   cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --mp4 out.mp4 --no-play
//!
//! 受信とデコードの本体は lib ターゲット (`lib.rs`) にあり、このバイナリは tracing の初期化、
//! Ctrl+C の待ち受け、タスクメトリクスのログ出力、tokio ランタイムの構築、フレームチャネルの
//! 準備、macOS の SDL プレイヤーの実行、終了コードの決定を行う。

use moq_sub::jitter_buffer::AudioJitterBuffer;
use moq_sub::{DecodedAudioFrame, DecodedVideoFrame, cli, error, pipeline};

/// 音声を再生機器へ積むときに先行させる分 (マイクロ秒)
///
/// SDL のストリームが空になると音が途切れるため、Opus の 1 フレーム (20 ms) の
/// 2 枚分を先行させて積む。目標遅延がこれより短いときは、到着した音をそのまま積む。
const AUDIO_OUTPUT_LEAD_US: i64 = 40_000;

/// jitter buffer の状態をログに出す間隔 (ミリ秒)
const AUDIO_BUFFER_LOG_INTERVAL_MS: u128 = 5_000;

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

    // 音声出力と再生の指定はメインスレッド (プレイヤー) が使うため、config を
    // tokio ランタイムへ move する前に取り出しておく
    let audio_output_device = config.audio_output_device;
    let no_play = config.no_play;
    if no_play && config.mp4.is_none() {
        tracing::warn!("--no-play is specified without --mp4; received media will be discarded");
    }

    let task_monitor = tokio_metrics::TaskMonitor::new();
    let shutdown = tokio_utils::ShutdownController::new();
    let shutdown_monitor = shutdown.subscribe();

    // デコード済みフレーム用チャネル (映像 / 音声)
    let (frame_tx, frame_rx) = std::sync::mpsc::channel::<DecodedVideoFrame>();
    let (audio_tx, audio_rx) = std::sync::mpsc::channel::<DecodedAudioFrame>();

    // プレイヤーの終了 (ウィンドウを閉じた等) を pipeline へ伝える。--no-play では送信せず、
    // pipeline が Ctrl+C や切断で終わるまで sender を保持する。
    let (player_stop_tx, player_stop_rx) = tokio::sync::oneshot::channel::<()>();

    // 表示待ちの映像フレーム数。デコードが表示より速いと増え続け、CPU を飽和させて
    // QUIC エンドポイントの I/O を飢えさせる。受信側はこれを見て
    // 古い group を捨てる。
    let display_backlog = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let display_backlog_for_pipeline = std::sync::Arc::clone(&display_backlog);

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
    // pipeline の終了結果はメインスレッドが受け取る。録画の finalize は pipeline 内で
    // 完了するため、この結果を受け取った時点で MP4 は確定している。
    let (result_tx, result_rx) = std::sync::mpsc::channel::<error::Result<()>>();
    // macOS では SDL のウィンドウ操作をメインスレッドで行う必要がある。
    // MoQT 処理は別スレッドの tokio ランタイムで動かす。
    std::thread::spawn(move || {
        let result = rt.block_on(async move {
            // タスクメトリクスを定期的にログ出力する
            tokio_moq::metrics::spawn_task_metrics_logger(task_monitor.clone());

            // Ctrl+C で graceful shutdown を起動する
            tokio::spawn(async move {
                tokio::signal::ctrl_c().await.ok();
                tracing::info!("Received Ctrl+C, initiating graceful shutdown");
                // shutdown が完了しない場合 (peer が stream を閉じない等) に備え、
                // 2 回目の Ctrl+C を待ちながら graceful shutdown する
                tokio::select! {
                    _ = shutdown.shutdown() => {}
                    _ = tokio::signal::ctrl_c() => {
                        tracing::warn!("Received second Ctrl+C, forcing exit");
                        std::process::exit(130);
                    }
                }
            });

            pipeline::run(
                config,
                frame_tx,
                audio_tx,
                task_monitor,
                shutdown_monitor,
                display_backlog_for_pipeline,
                player_stop_rx,
            )
            .await
        });
        let _ = result_tx.send(result);
    });

    let mut failed = false;
    if no_play {
        // プレイヤーを動かさず、pipeline の終了 (Ctrl+C / relay からの切断) を待つ
        tracing::info!("Playback is disabled, waiting for the session to finish");
    } else {
        // メインスレッドで raw_player を動かす
        if let Err(e) = run_raw_player(frame_rx, audio_rx, display_backlog, audio_output_device) {
            tracing::error!("Fatal: {e}");
            failed = true;
        }
        // プレイヤーの終了を pipeline へ伝え、録画の finalize を待つ
        let _ = player_stop_tx.send(());
    }

    match result_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!("Fatal: {e}");
            failed = true;
        }
        Err(_) => {
            tracing::error!("Pipeline thread terminated unexpectedly");
            failed = true;
        }
    }
    if failed {
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
/// 開かず、受信済みの音声チャンクを数えるだけにする (スピーカーへ音を出さない)。このときは
/// 鳴らす時刻を決める必要が無いため、jitter buffer へも入れない。
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

    // 音声は到着後すぐ鳴らさず、鳴らす時刻 (LOC の TIMESTAMP と学習した目標遅延から
    // 決まる) まで保持する。到着の揺らぎがそのまま音の途切れにならないようにするためである
    let mut audio_buffer = AudioJitterBuffer::new();
    let mut released_audio: u64 = 0;
    let mut last_audio_log = std::time::Instant::now();

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
                // 音声出力を行わない指定では再生機器が無いため、保持も観測もしない
                if audio_output_enabled {
                    audio_buffer.push(audio, wall_clock_us());
                }
                audio_chunk_count += 1;
                if audio_chunk_count.is_multiple_of(100) {
                    if audio_output_enabled {
                        tracing::info!("Received {audio_chunk_count} audio chunks");
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
                    // 保持している音をすべて吐き出す。末尾の音を捨てないためである
                    if let Some(audio_player) = audio_player.as_ref() {
                        while let Some(audio) = audio_buffer.pop_oldest() {
                            enqueue_decoded_audio(audio_player, &audio, &mut audio_started);
                            released_audio += 1;
                        }
                    }
                }
            }
        }

        // 鳴らす時刻まで保持していた音声を再生機器へ渡す。遅れすぎた音は鳴らさずに捨てる
        if let Some(audio_player) = audio_player.as_ref() {
            let now_us = wall_clock_us();
            let dropped_late = audio_buffer.drop_late(now_us);
            if dropped_late > 0 {
                tracing::warn!("Discarded {dropped_late} audio chunks that are too late to play");
            }
            while let Some(audio) = audio_buffer.pop_releasable(now_us, AUDIO_OUTPUT_LEAD_US) {
                enqueue_decoded_audio(audio_player, &audio, &mut audio_started);
                released_audio += 1;
            }
            if let Err(e) = audio_player.process() {
                tracing::warn!("Failed to process audio queue: {e}");
            }
            if last_audio_log.elapsed().as_millis() >= AUDIO_BUFFER_LOG_INTERVAL_MS {
                last_audio_log = std::time::Instant::now();
                // player_buffer は再生機器へ積んだまま鳴っていない長さである。これが 0 に
                // 近づくと音が途切れるため、目標遅延が足りているかをここで確認できる
                tracing::info!(
                    "Audio jitter buffer: target_delay={}ms pending={} player_buffer={:.0}ms released={} dropped_late={} dropped_overflow={}",
                    audio_buffer.target_delay_us().unwrap_or(0) / 1000,
                    audio_buffer.len(),
                    audio_player.stats().audio_buffer_ms,
                    released_audio,
                    audio_buffer.dropped_late(),
                    audio_buffer.dropped_overflow(),
                );
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

/// デコード済みの音声を再生機器へ積み、最初の 1 枚で再生を開始する
fn enqueue_decoded_audio(
    audio_player: &raw_player::AudioPlayer,
    audio: &DecodedAudioFrame,
    audio_started: &mut bool,
) {
    let pcm_bytes = pcm_i16_to_bytes(&audio.pcm);
    if let Err(e) = audio_player.enqueue_audio(
        &pcm_bytes,
        audio.pts_us,
        audio.sample_rate as i32,
        audio.channels as i32,
        raw_player::AudioFormat::S16,
    ) {
        tracing::warn!("Failed to enqueue audio chunk: {e}");
        return;
    }
    // 空のキューで play すると SDL のデバイスを開くだけで音は出ないため、積んでから呼ぶ
    if !*audio_started {
        match audio_player.play() {
            Ok(()) => {
                *audio_started = true;
                tracing::info!("Audio playback started");
            }
            Err(e) => tracing::warn!("Failed to start audio playback: {e}"),
        }
    }
}

/// 現在時刻を Unix epoch からのマイクロ秒で返す
fn wall_clock_us() -> i64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX)
}

/// `&[i16]` をリトルエンディアンのバイト列に変換する
fn pcm_i16_to_bytes(pcm: &[i16]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(pcm.len() * 2);
    for &sample in pcm {
        buf.extend_from_slice(&sample.to_le_bytes());
    }
    buf
}
