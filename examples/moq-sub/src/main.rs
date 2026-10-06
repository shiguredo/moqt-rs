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
use shiguredo_moqt::playout::buffer::PlayoutBuffer;
use shiguredo_moqt::playout::scheduler::{
    AUDIO_PLAYOUT_DELAY_US, AudioPlayoutDecision, AudioPlayoutInput, AudioPlayoutScheduler,
};
use shiguredo_moqt::playout::stretch;
use shiguredo_moqt::playout::timeline::{PlayoutTimeline, Track};

/// 音声を再生機器へ積むときに先行させる分 (マイクロ秒)
///
/// SDL のストリームが空になると音が途切れるため、Opus の 1 フレーム (20 ms) の
/// 2 枚分を先行させて積む。目標遅延がこれより短いときは、到着した音をそのまま積む。
const AUDIO_OUTPUT_LEAD_US: i64 = 40_000;

/// jitter buffer の状態をログに出す間隔 (ミリ秒)
const AUDIO_BUFFER_LOG_INTERVAL_MS: u128 = 5_000;

/// 映像を表示待ちとして保持する上限 (枚)
///
/// 時間軸が表示の遅れの上限を決める `TimelineConfig::video_queue_limit` の既定値と同じ枚数
/// にする。デコーダの出力は復号の並行度の分だけ揺らぐため、その分を吸収できる深さが要る。
const VIDEO_QUEUED_FRAMES: usize = 24;

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
    // カタログの targetLatency (ミリ秒)。-1 は「カタログに無い」を表す。
    // pipeline がカタログを読んだ時点で入れ、プレイヤーが時間軸へ反映する
    let target_latency_ms = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(-1));
    let target_latency_ms_for_pipeline = std::sync::Arc::clone(&target_latency_ms);

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
                target_latency_ms_for_pipeline,
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
        if let Err(e) = run_raw_player(
            frame_rx,
            audio_rx,
            display_backlog,
            target_latency_ms,
            audio_output_device,
        ) {
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
    target_latency_ms: std::sync::Arc<std::sync::atomic::AtomicI64>,
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

    // 音声と映像で共有する時間軸。復号の出力を両方ともここへ観測し、表示時刻は
    // 「LOC の TIMESTAMP + 基準の遅れ + 表示の遅れ」で決まる。A/V 同期の制御も
    // 観測のたびに時間軸が行う
    let mut timeline = PlayoutTimeline::new();
    // 映像は到着後すぐ表示せず、表示時刻まで保持して選ぶ。遅れて届いたフレームは
    // 追い越させず、表示時刻を過ぎた最新の 1 枚だけを表示する
    let mut video_buffer =
        PlayoutBuffer::<DecodedVideoFrame>::new(VIDEO_QUEUED_FRAMES, Track::Video);

    // 音声は到着後すぐ鳴らさず、鳴らす時刻 (LOC の TIMESTAMP と学習した目標遅延から
    // 決まる) まで保持する。到着の揺らぎがそのまま音の途切れにならないようにするためである
    let mut audio_buffer = AudioJitterBuffer::new();
    // 保持から出した音をどの順でいつ鳴らすかは、スケジューラが決める。jitter buffer は
    // 鳴らす時刻まで保持する役割であり、順番・詰め・隙間はここへ集める
    let mut audio_scheduler = AudioPlayoutScheduler::new();
    // 直前に鳴らした音の波形。欠落した隙間をこの末尾の周期で埋める
    let mut last_played_audio: Option<PlayedAudio> = None;
    let mut released_audio: u64 = 0;
    let mut last_audio_log = std::time::Instant::now();

    // wall-clock PTS: 再生開始からの経過時間を映像 PTS として使用する
    let start_time = std::time::Instant::now();
    let mut frame_count: u64 = 0;
    let mut audio_chunk_count: u64 = 0;
    // 直近に時間軸へ反映した targetLatency (-1 は未反映)
    let mut applied_target_latency_ms: i64 = -1;
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
        // カタログの targetLatency が届いていれば、表示の遅れの下限として時間軸へ反映する。
        // カタログは再生開始後に届くため、ここで毎回確認する
        let catalog_target_latency_ms =
            target_latency_ms.load(std::sync::atomic::Ordering::Relaxed);
        if catalog_target_latency_ms >= 0 && catalog_target_latency_ms != applied_target_latency_ms
        {
            applied_target_latency_ms = catalog_target_latency_ms;
            timeline.set_target_latency_ms(catalog_target_latency_ms);
            tracing::info!("Applied targetLatency: {catalog_target_latency_ms}ms");
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
                // 復号の出力を時間軸へ観測し、表示時刻が決まるまで保持する。TIMESTAMP が
                // 無いフレームは表示時刻を決められないため、届いた順に表示する
                let timestamp_us = frame.timestamp_us;
                if let Some(timestamp_us) = timestamp_us {
                    timeline.observe(Track::Video, wall_clock_us(), timestamp_us);
                }
                for dropped in video_buffer.enqueue(frame, timestamp_us, &timeline) {
                    tracing::debug!(
                        "Discarded a video frame waiting to be displayed: {}x{}",
                        dropped.width,
                        dropped.height,
                    );
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if !video_disconnected {
                    tracing::info!("Video channel disconnected");
                    video_disconnected = true;
                    // 表示待ちに残っている分は表示されない。黙って捨てずに数を出す
                    let remaining = video_buffer.clear();
                    if !remaining.is_empty() {
                        tracing::info!(
                            "Discarded {} video frames waiting to be displayed",
                            remaining.len(),
                        );
                    }
                }
            }
        }

        // 表示時刻を過ぎたフレームのうち最新の 1 枚を表示する。遅れすぎたフレームは
        // 時間軸の上限を超えた分として捨てる
        {
            let now_us = wall_clock_us();
            let selection = video_buffer.select(now_us, &timeline);
            // 描くことになったフレームの表示時刻。表示時刻を決められないフレームでは None
            let draw_presentation_us = selection.draw_presentation_us;
            for late in selection.late {
                tracing::debug!(
                    "Discarded a late video frame: {}x{}",
                    late.width,
                    late.height
                );
            }
            if let Some(frame) = selection.draw {
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
                    // 表示時刻を過ぎたフレームを渡すため、PTS は再生側のいまの時刻にする
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
                    // 表示の実績を時間軸へ記録する。表示時刻は PlayoutBuffer が決めた値を
                    // 使い、決められないフレーム (基準がまだ無い、基準がずれている) は
                    // 実際に描いた時刻にする。TIMESTAMP の無いフレームは記録できない
                    if let Some(timestamp_us) = frame.timestamp_us {
                        timeline.record_presentation(
                            Track::Video,
                            timestamp_us,
                            draw_presentation_us.unwrap_or(now_us),
                        );
                    }
                    if frame_count.is_multiple_of(100) {
                        tracing::info!("Rendered {frame_count} video frames");
                    }
                }
            }
        }

        match audio_rx.try_recv() {
            Ok(audio) => {
                // 音声出力を行わない指定では再生機器が無いため、保持も観測もしない
                if audio_output_enabled {
                    audio_buffer.push(audio, wall_clock_us(), &mut timeline);
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
                    // 保持している音をすべて吐き出す。末尾の音を捨てないためである。
                    // 鳴らす時刻は保持から出すときと同じくスケジューラが決めるため、
                    // 目標より先すぎる音は捨てられる (捨てた数はログに出る)
                    if let Some(audio_player) = audio_player.as_ref() {
                        let now_us = wall_clock_us();
                        while let Some(audio) = audio_buffer.pop_oldest() {
                            released_audio += 1;
                            play_decoded_audio(
                                audio_player,
                                &mut audio_started,
                                &mut audio_scheduler,
                                &mut last_played_audio,
                                &mut timeline,
                                &audio,
                                now_us,
                            );
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
            // 鳴らす時刻を決めるのは、保持から出したこの時点にする。到着時に決めると、
            // 保持している間 (再生の遅れぶん) に決めた時刻が古くなり、実際に積む時点の
            // 遅れを反映できない。詰めと隙間の補間は決めた直後に適用し、適用した長さを
            // その場でスケジューラへ返す
            while let Some(audio) = audio_buffer.pop_releasable(now_us, AUDIO_OUTPUT_LEAD_US) {
                released_audio += 1;
                play_decoded_audio(
                    audio_player,
                    &mut audio_started,
                    &mut audio_scheduler,
                    &mut last_played_audio,
                    &mut timeline,
                    &audio,
                    now_us,
                );
            }
            if let Err(e) = audio_player.process() {
                tracing::warn!("Failed to process audio queue: {e}");
            }
            if last_audio_log.elapsed().as_millis() >= AUDIO_BUFFER_LOG_INTERVAL_MS {
                last_audio_log = std::time::Instant::now();
                // player_buffer は再生機器へ積んだまま鳴っていない長さである。これが 0 に
                // 近づくと音が途切れるため、目標遅延が足りているかをここで確認できる。
                // concealed と compressed は実際に適用できた長さの合計であり、skew_us は
                // 実績から求めた A/V のずれ (映像が音声より遅れていれば正) である
                let skew_us = timeline
                    .skew_us()
                    .map_or_else(|| "none".to_string(), |skew_us| skew_us.to_string());
                tracing::info!(
                    "Audio jitter buffer: target_delay={}ms pending={} player_buffer={:.0}ms released={} dropped_late={} dropped_overflow={} dropped={} concealed={}ms concealments={} compressed={}ms skew_us={}",
                    timeline.presentation_delay_us(Track::Audio).unwrap_or(0) / 1000,
                    audio_buffer.len(),
                    audio_player.stats().audio_buffer_ms,
                    released_audio,
                    audio_buffer.dropped_late(),
                    audio_buffer.dropped_overflow(),
                    audio_scheduler.drops(),
                    audio_scheduler.concealed_us() / 1000,
                    audio_scheduler.concealments(),
                    audio_scheduler.compressed_us() / 1000,
                    skew_us,
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

/// 直前に鳴らした音の波形 (欠落した隙間を埋めるのに使う)
struct PlayedAudio {
    /// チャンネルごとの波形 (f32、おおむね `[-1.0, 1.0]`)
    channels: Vec<Vec<f32>>,
    /// サンプルレート (Hz)
    sample_rate: u32,
}

/// デコード済みの音声を 1 つ、スケジューラの決めた時刻へ向けて再生機器へ積む
///
/// 鳴らす時刻は [`AudioPlayoutScheduler`] が決める。`now_us` は今の時刻 (受信側の壁時計)。
/// 隙間は直前の音の末尾を周期で繰り返して埋め、詰めは波形の周期 1 つ分を削って行う。
/// 実際に適用した長さは `confirm_stretch` と `confirm_concealment` で返す。
///
/// スケジューラが鳴らさないと決めた音は積まずに捨て、捨てた数をログに出す (鳴らせない音を
/// 積むと、その分だけ音が遅れたままになる)。
fn play_decoded_audio(
    audio_player: &raw_player::AudioPlayer,
    audio_started: &mut bool,
    audio_scheduler: &mut AudioPlayoutScheduler,
    last_played_audio: &mut Option<PlayedAudio>,
    timeline: &mut PlayoutTimeline,
    audio: &DecodedAudioFrame,
    now_us: i64,
) {
    let channels = usize::from(audio.channels);
    let sample_rate = audio.sample_rate;
    let duration_us = pcm_duration_us(audio.pcm.len(), channels, sample_rate);
    // 目標の開始時刻は時間軸が返す「鳴らす時刻」である。まだ基準が無い、または基準が
    // ずれているときは None であり、そのときは目標に従わず到着基準で並べる
    let target_start_us = timeline.present_us(Track::Audio, audio.pts_us);
    // 揺らぎから求めた音声の遅れ。まだ学習していなければ既定値を下限にする。目標が無い
    // ときは並べ方の先行分に、目標があるときは並べすぎの判定に使う
    let delay_us = timeline
        .learned_delay_us(Track::Audio)
        .max(AUDIO_PLAYOUT_DELAY_US);
    // 表示の遅れは時間軸が返す値をそのまま渡す。まだ観測が無いときは既定値を使う
    let presentation_delay_us = timeline
        .presentation_delay_us(Track::Audio)
        .unwrap_or(AUDIO_PLAYOUT_DELAY_US);
    let decision = audio_scheduler.schedule(AudioPlayoutInput {
        now_us,
        timestamp_us: audio.pts_us,
        duration_us,
        target_start_us,
        // 目標が決まっているときだけ目標を守る。無いときは到着基準で並べる
        enforce_target: target_start_us.is_some(),
        delay_us,
        presentation_delay_us,
    });
    let AudioPlayoutDecision::Play {
        start_at_us,
        compress_us,
        gap_us,
        ..
    } = decision
    else {
        tracing::warn!(
            "Discarded an audio chunk the playout scheduler dropped (total {})",
            audio_scheduler.drops(),
        );
        return;
    };

    // 隙間の補間と詰めを音声へ適用する。埋めた音は今回の音の直前へ積むため、再生機器の
    // キューでは前の音の直後、今回の音の直前になる (キューは積んだ順に鳴る)。隙間の開始
    // 時刻 (gap_start_us) は使わない。積む順で位置が決まるためである
    let applied = apply_audio_playout(audio, last_played_audio.as_ref(), gap_us, compress_us);
    if !applied.concealment.is_empty() {
        // 埋めた音は今回の TIMESTAMP の直前を占める。PTS は今回の音から埋めた長さだけ
        // 戻した値にする
        let pts_us = audio.pts_us.saturating_sub(applied.concealed_us);
        let enqueued = enqueue_pcm(
            audio_player,
            audio_started,
            &applied.concealment,
            pts_us,
            sample_rate,
            audio.channels,
        );
        // 積めた長さだけを補間したものとして返す
        audio_scheduler.confirm_concealment(if enqueued { applied.concealed_us } else { 0 });
    } else if gap_us > 0 {
        // 無音や相関の不足で周期が求まらないと埋められない。無音のまま続ける
        tracing::info!("Could not conceal the {gap_us}us gap in the audio");
        audio_scheduler.confirm_concealment(0);
    }
    audio_scheduler.confirm_stretch(applied.compressed_us);

    let pcm = pcm_channels_to_f32(
        &applied.channels,
        applied.channels.first().map_or(0, Vec::len),
    );
    if !enqueue_pcm(
        audio_player,
        audio_started,
        &pcm,
        audio.pts_us,
        sample_rate,
        audio.channels,
    ) {
        return;
    }
    // 直前に鳴らした音として保持する。次の隙間はこの音の末尾を繰り返して埋める
    *last_played_audio = Some(PlayedAudio {
        channels: applied.channels,
        sample_rate,
    });
    // 表示の実績を時間軸へ記録する。音声は再生機器のバッファへ先行して積むため、積んだ
    // 時刻は実際に鳴る時刻より最大 AUDIO_OUTPUT_LEAD_US だけ早い。スケジューラの決めた
    // 鳴らす時刻を、実際に鳴る時刻の推定として使う
    timeline.record_presentation(Track::Audio, audio.pts_us, start_at_us);
}

/// スケジューラが決めた隙間の補間と詰めを音声へ適用した結果
struct AppliedAudioPlayout {
    /// 詰めた後の今回の音 (チャンネルごと、f32)
    channels: Vec<Vec<f32>>,
    /// 埋めた隙間の音 (f32 interleaved)。埋めていないときは空
    concealment: Vec<f32>,
    /// 埋めた長さ (マイクロ秒)。埋めていないときは 0
    concealed_us: i64,
    /// 詰めた長さ (マイクロ秒)。詰めていないときは 0
    compressed_us: i64,
}

/// スケジューラが決めた隙間の補間と詰めを音声へ適用する (再生機器には触れない)
///
/// `gap_us` の隙間は `last_played_audio` の末尾を周期で繰り返して埋める。埋められなかった
/// ときは `concealment` を空にし `concealed_us` を 0 にする。`compress_us` は波形の周期
/// 1 つ分を削って詰める。削れる長さは周期 (2.5 ms 以上 15 ms 以下) で決まり、要求された
/// 長さとは一致しないため、実際に削れた長さを `compressed_us` として返す。どちらも
/// `confirm_stretch` と `confirm_concealment` へそのまま渡せる。
fn apply_audio_playout(
    audio: &DecodedAudioFrame,
    last_played_audio: Option<&PlayedAudio>,
    gap_us: i64,
    compress_us: i64,
) -> AppliedAudioPlayout {
    let channels = usize::from(audio.channels);
    let sample_rate = audio.sample_rate;
    let (concealment, concealed_us) = if gap_us > 0 {
        conceal_gap(last_played_audio, channels, sample_rate, gap_us)
            .unwrap_or_else(|| (Vec::new(), 0))
    } else {
        (Vec::new(), 0)
    };

    // stretch は f32 のチャンネルごとの波形を扱う。S16 interleaved から直して分ける
    let mut channel_pcm = pcm_f32_to_channels(&pcm_i16_to_f32(&audio.pcm), channels);
    let mut compressed_us = 0;
    let mut used_frames = channel_pcm.first().map_or(0, Vec::len);
    if compress_us > 0 {
        let mut channel_slices: Vec<&mut [f32]> = channel_pcm
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let removed = stretch::compress(&mut channel_slices, sample_rate);
        if removed < 0 {
            // 削った分だけ後ろを詰めた状態で使う範囲が短くなる
            let removed_frames = removed.unsigned_abs().min(used_frames);
            compressed_us = samples_to_us(removed_frames, sample_rate);
            used_frames -= removed_frames;
            for channel in &mut channel_pcm {
                channel.truncate(used_frames);
            }
        }
    }
    AppliedAudioPlayout {
        channels: channel_pcm,
        concealment,
        concealed_us,
        compressed_us,
    }
}

/// 直前の音の末尾を周期で繰り返して隙間を埋める
///
/// 戻り値は埋めた音 (f32 interleaved) と埋めた長さ (マイクロ秒)。埋められないときは
/// `None` であり、[`stretch::conceal`] が 0 を返したとき (無音、末尾に周期が無い、
/// 継ぎ目の段差が大きいなど) と、直前の音の形式が今の音と違うときに返る。
fn conceal_gap(
    previous: Option<&PlayedAudio>,
    channels: usize,
    sample_rate: u32,
    gap_us: i64,
) -> Option<(Vec<f32>, i64)> {
    let previous = previous?;
    // 形式が変わると周期もサンプル数も変わるため、同じ形式のときだけ埋める
    if previous.sample_rate != sample_rate || previous.channels.len() != channels {
        return None;
    }
    // stretch::conceal は埋める長さ以上の出力を要求する。マイクロ秒からサンプル数への
    // 丸めが切り捨てでも足りるよう、切り上げた長さを用意する
    let capacity = us_to_samples_ceil(gap_us, sample_rate);
    let mut concealment: Vec<Vec<f32>> = previous
        .channels
        .iter()
        .map(|_| vec![0.0f32; capacity])
        .collect();
    let references: Vec<&[f32]> = previous
        .channels
        .iter()
        .map(|channel| channel.as_slice())
        .collect();
    let mut outputs: Vec<&mut [f32]> = concealment
        .iter_mut()
        .map(|channel| channel.as_mut_slice())
        .collect();
    let samples = stretch::conceal(&references, &mut outputs, sample_rate, gap_us);
    let Ok(samples) = usize::try_from(samples) else {
        return None;
    };
    if samples == 0 {
        return None;
    }
    for channel in &mut concealment {
        channel.truncate(samples);
    }
    let applied_us = samples_to_us(samples, sample_rate);
    Some((pcm_channels_to_f32(&concealment, samples), applied_us))
}

/// f32 interleaved の音声を再生機器へ積み、最初の 1 枚で再生を開始する
///
/// 積むことができたときだけ true を返す。
fn enqueue_pcm(
    audio_player: &raw_player::AudioPlayer,
    audio_started: &mut bool,
    pcm: &[f32],
    pts_us: i64,
    sample_rate: u32,
    channels: u8,
) -> bool {
    if pcm.is_empty() {
        return false;
    }
    let pcm_bytes = pcm_i16_to_bytes(&pcm_f32_to_i16(pcm));
    if let Err(e) = audio_player.enqueue_audio(
        &pcm_bytes,
        pts_us,
        sample_rate as i32,
        channels as i32,
        raw_player::AudioFormat::S16,
    ) {
        tracing::warn!("Failed to enqueue audio chunk: {e}");
        return false;
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
    true
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

/// S16 interleaved の音声を f32 interleaved へ変換する
///
/// サンプルは `[-1.0, 1.0]` へ正規化する。割る値は 32768 であり、
/// [`pcm_f32_to_i16`] との往復で値は変わらない。
fn pcm_i16_to_f32(pcm: &[i16]) -> Vec<f32> {
    pcm.iter()
        .map(|sample| f32::from(*sample) / 32768.0)
        .collect()
}

/// f32 interleaved の音声を S16 interleaved へ変換する
///
/// `[-1.0, 1.0]` の外は飽和させ、非数は 0 にする。
fn pcm_f32_to_i16(pcm: &[f32]) -> Vec<i16> {
    pcm.iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * 32768.0).round() as i16)
        .collect()
}

/// f32 interleaved の音声をチャンネルごとの波形へ分ける
///
/// チャンネル数で割り切れない端数のサンプルは捨てる。チャンネル数が 0 のときは空を返す。
fn pcm_f32_to_channels(pcm: &[f32], channels: usize) -> Vec<Vec<f32>> {
    if channels == 0 {
        return Vec::new();
    }
    let mut result: Vec<Vec<f32>> = (0..channels).map(|_| Vec::new()).collect();
    for frame in pcm.chunks_exact(channels) {
        for (channel, sample) in result.iter_mut().zip(frame) {
            channel.push(*sample);
        }
    }
    result
}

/// チャンネルごとの波形を f32 interleaved へ戻す (先頭 `frames` フレーム)
///
/// `frames` は最も短いチャンネルの長さまでに切る。
fn pcm_channels_to_f32(channels: &[Vec<f32>], frames: usize) -> Vec<f32> {
    let frames = channels
        .iter()
        .map(Vec::len)
        .min()
        .map_or(0, |length| length.min(frames));
    let mut pcm = Vec::new();
    for frame in 0..frames {
        pcm.extend(channels.iter().map(|channel| channel[frame]));
    }
    pcm
}

/// S16 interleaved の音声の長さ (マイクロ秒) を求める
///
/// `pcm_len` はチャンネルを合わせたサンプル数である。チャンネル数で割り切れない端数は
/// 切り捨てる。チャンネル数かサンプルレートが 0 のときは 0 を返す。
fn pcm_duration_us(pcm_len: usize, channels: usize, sample_rate: u32) -> i64 {
    if channels == 0 || sample_rate == 0 {
        return 0;
    }
    let frames = pcm_len / channels;
    i64::try_from(frames)
        .unwrap_or(i64::MAX)
        .saturating_mul(1_000_000)
        / i64::from(sample_rate)
}

/// マイクロ秒に相当するサンプル数 (切り上げ)
///
/// サンプル数を必要とするバッファを確保するために使う。0 以下とサンプルレートが 0 の
/// ときは 0 を返す。
fn us_to_samples_ceil(us: i64, sample_rate: u32) -> usize {
    if us <= 0 || sample_rate == 0 {
        return 0;
    }
    let scaled = us.saturating_mul(i64::from(sample_rate));
    usize::try_from(scaled.saturating_add(999_999) / 1_000_000).unwrap_or(usize::MAX)
}

/// サンプル数をマイクロ秒へ直す (四捨五入)
///
/// `stretch` がマイクロ秒をサンプル数へ直すときと同じ丸めにする。サンプルレートが 0 の
/// ときは 0 を返す。
fn samples_to_us(samples: usize, sample_rate: u32) -> i64 {
    if sample_rate == 0 {
        return 0;
    }
    let Ok(samples) = i64::try_from(samples) else {
        return i64::MAX;
    };
    samples
        .saturating_mul(1_000_000)
        .saturating_add(i64::from(sample_rate) / 2)
        / i64::from(sample_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の周期のある音を作る
    ///
    /// 48 kHz のサンプルレートで `frequency_hz` が整数の周期になる (400 Hz なら 120
    /// サンプル) ため、`stretch::conceal` が末尾の周期をそのまま見つけられる。
    fn periodic_audio(frequency_hz: f64, sample_rate: u32, length: usize) -> Vec<f32> {
        (0..length)
            .map(|index| {
                let phase = 2.0 * std::f64::consts::PI * frequency_hz * index as f64
                    / f64::from(sample_rate);
                (phase.sin() * 0.5) as f32
            })
            .collect()
    }

    /// S16 の値が f32 を経由して元の値に戻ること
    #[test]
    fn pcm_i16_and_f32_round_trip() {
        let pcm: Vec<i16> = vec![-32768, -32767, -12345, -1, 0, 1, 12345, 32767];
        let f32_pcm = pcm_i16_to_f32(&pcm);
        assert_eq!(
            f32_pcm.first(),
            Some(&-1.0),
            "-32768 は -1.0 へ正規化すること"
        );
        assert_eq!(
            f32_pcm.get(1).copied(),
            Some(-32767.0 / 32768.0),
            "正規化は 32768 で割ること"
        );
        assert_eq!(pcm_f32_to_i16(&f32_pcm), pcm, "往復で元の値に戻ること");
    }

    /// 範囲外と非数を S16 へ直すときは飽和させること
    #[test]
    fn pcm_f32_to_i16_saturates() {
        assert_eq!(
            pcm_f32_to_i16(&[2.0, -2.0, f32::NAN]),
            vec![32767, -32768, 0],
            "範囲外は飽和させ、非数は 0 にすること"
        );
        assert_eq!(
            pcm_f32_to_i16(&[0.5, -0.5]),
            vec![16384, -16384],
            "0.5 は半振幅へ直すこと"
        );
    }

    /// interleaved とチャンネルごとの波形が往復すること
    #[test]
    fn pcm_channels_round_trip() {
        let pcm = vec![1.0f32, 2.0, 3.0, 4.0];
        let channels = pcm_f32_to_channels(&pcm, 2);
        assert_eq!(
            channels,
            vec![vec![1.0f32, 3.0], vec![2.0f32, 4.0]],
            "先頭のチャンネルから順に分けること"
        );
        assert_eq!(
            pcm_channels_to_f32(&channels, 2),
            pcm,
            "interleaved へ戻すと元の並びになること"
        );
        // モノラルでは分けずにそのまま扱う
        assert_eq!(pcm_f32_to_channels(&pcm, 1), vec![pcm.clone()]);
        assert_eq!(pcm_f32_to_channels(&pcm, 0), Vec::<Vec<f32>>::new());
    }

    /// interleaved の端数と、長さの違うチャンネルを切りそろえること
    #[test]
    fn pcm_channels_drop_the_uneven_tail() {
        assert_eq!(
            pcm_f32_to_channels(&[1.0, 2.0, 3.0], 2),
            vec![vec![1.0f32], vec![2.0f32]],
            "割り切れない端数は捨てること"
        );
        assert_eq!(
            pcm_channels_to_f32(&[vec![1.0f32, 2.0], vec![3.0f32]], 2),
            vec![1.0f32, 3.0],
            "最も短いチャンネルまでに切ること"
        );
        assert_eq!(
            pcm_channels_to_f32(&[], 4),
            Vec::<f32>::new(),
            "チャンネルが無ければ空にすること"
        );
    }

    /// 音声の長さをフレーム数からマイクロ秒で求めること
    #[test]
    fn pcm_duration_is_computed_from_the_frames() {
        assert_eq!(
            pcm_duration_us(960, 1, 48_000),
            20_000,
            "モノラルの 960 サンプルは 20 ms であること"
        );
        assert_eq!(
            pcm_duration_us(1_920, 2, 48_000),
            20_000,
            "ステレオではチャンネルぶんで割ること"
        );
        assert_eq!(
            pcm_duration_us(1_921, 2, 48_000),
            20_000,
            "割り切れない端数は切り捨てること"
        );
        assert_eq!(pcm_duration_us(0, 1, 48_000), 0, "空は 0 であること");
        assert_eq!(
            pcm_duration_us(960, 0, 48_000),
            0,
            "チャンネル数 0 は 0 であること"
        );
        assert_eq!(
            pcm_duration_us(960, 1, 0),
            0,
            "サンプルレート 0 は 0 であること"
        );
    }

    /// サンプル数とマイクロ秒の変換が四捨五入で戻ること
    #[test]
    fn sample_and_microsecond_conversions_round() {
        assert_eq!(
            us_to_samples_ceil(20_000, 48_000),
            960,
            "20 ms は 48 kHz で 960 サンプルであること"
        );
        assert_eq!(
            us_to_samples_ceil(10, 48_000),
            1,
            "1 サンプルに満たない長さも 1 サンプルへ切り上げること"
        );
        assert_eq!(us_to_samples_ceil(0, 48_000), 0, "0 は 0 であること");
        assert_eq!(us_to_samples_ceil(-1, 48_000), 0, "負の長さは 0 であること");
        assert_eq!(
            us_to_samples_ceil(1_000, 0),
            0,
            "サンプルレート 0 は 0 であること"
        );
        assert_eq!(
            samples_to_us(960, 48_000),
            20_000,
            "960 サンプルは 20 ms へ戻ること"
        );
        assert_eq!(
            samples_to_us(1, 48_000),
            21,
            "1 サンプルは四捨五入して 21 us になること"
        );
        assert_eq!(samples_to_us(0, 48_000), 0, "0 サンプルは 0 であること");
        assert_eq!(samples_to_us(960, 0), 0, "サンプルレート 0 は 0 であること");
    }

    /// 周期のある音から隙間を組み立てること
    #[test]
    fn conceal_gap_repeats_the_previous_tail() {
        let sample_rate = 48_000;
        // 400 Hz は 48 kHz で 120 サンプル周期であり、末尾の周期がそのまま見つかる
        let period = 120;
        let previous = PlayedAudio {
            channels: vec![periodic_audio(400.0, sample_rate, 4_800)],
            sample_rate,
        };
        let (pcm, applied_us) = conceal_gap(Some(&previous), 1, sample_rate, 20_000)
            .expect("周期のある音からは隙間を埋められること");
        assert_eq!(pcm.len(), 960, "20 ms ぶんの音を組み立てること");
        assert_eq!(applied_us, 20_000, "埋めた長さをマイクロ秒で返すこと");
        // 末尾の周期を繰り返していること (末尾の利得が下がるぶんだけ差が出る)
        for index in 0..pcm.len() - period {
            let difference = (pcm[index] - pcm[index + period]).abs();
            assert!(
                difference < 0.05,
                "{index} 番目が周期ぶん後の音と一致すること difference={difference}"
            );
        }
        // 組み立てた先頭は直前の音の末尾 1 周期ぶんの先頭であること
        let previous_tail = previous.channels[0][4_800 - period];
        assert!(
            (pcm[0] - previous_tail).abs() < 0.01,
            "先頭は直前の音の末尾の続きであること pcm={} tail={previous_tail}",
            pcm[0]
        );
    }

    /// 埋められないときは隙間を組み立てないこと
    #[test]
    fn conceal_gap_returns_none_when_it_cannot_fill() {
        // 無音からは周期が求まらない
        let silent = PlayedAudio {
            channels: vec![vec![0.0f32; 4_800]],
            sample_rate: 48_000,
        };
        assert!(
            conceal_gap(Some(&silent), 1, 48_000, 20_000).is_none(),
            "無音からは埋めないこと"
        );
        // 直前の音が無ければ埋められない
        assert!(
            conceal_gap(None, 1, 48_000, 20_000).is_none(),
            "直前の音が無ければ埋めないこと"
        );
        // 形式が違うと周期もサンプル数も変わるため埋めない
        let stereo = PlayedAudio {
            channels: vec![periodic_audio(400.0, 48_000, 4_800); 2],
            sample_rate: 48_000,
        };
        assert!(
            conceal_gap(Some(&stereo), 1, 48_000, 20_000).is_none(),
            "チャンネル数が違えば埋めないこと"
        );
        assert!(
            conceal_gap(Some(&stereo), 2, 16_000, 20_000).is_none(),
            "サンプルレートが違えば埋めないこと"
        );
        // 隙間の長さが 0 なら埋めない
        assert!(
            conceal_gap(Some(&stereo), 2, 48_000, 0).is_none(),
            "隙間が無ければ埋めないこと"
        );
    }

    /// テスト用のモノラルの音声フレームを作る (S16 interleaved)
    fn audio_frame(frames: usize, sample_rate: u32) -> DecodedAudioFrame {
        let pcm: Vec<i16> = periodic_audio(400.0, sample_rate, frames)
            .into_iter()
            .map(|sample| (sample * 32768.0) as i16)
            .collect();
        DecodedAudioFrame {
            pcm,
            sample_rate,
            channels: 1,
            pts_us: 0,
        }
    }

    /// スケジューラの決めた隙間と詰めが音声へ適用されること
    ///
    /// 実際のスケジューラに決定させた値で組み立てる。再生機器は使わない。
    #[test]
    fn playout_applies_the_gap_and_the_compression() {
        let sample_rate = 48_000;
        let mut scheduler = AudioPlayoutScheduler::new();
        // 直前の音 (20 ms) を鳴らし終えた状態にする
        let previous = PlayedAudio {
            channels: vec![periodic_audio(400.0, sample_rate, 960)],
            sample_rate,
        };

        // 1 つ目は目標の時刻にそのまま鳴る (詰めも隙間も無し)
        let first = scheduler.schedule(AudioPlayoutInput {
            now_us: 990_000,
            timestamp_us: 0,
            duration_us: 20_000,
            target_start_us: Some(1_000_000),
            enforce_target: true,
            delay_us: 80_000,
            presentation_delay_us: 80_000,
        });
        assert_eq!(
            first,
            AudioPlayoutDecision::Play {
                start_at_us: 1_000_000,
                compress_us: 0,
                gap_start_us: 0,
                gap_us: 0,
            },
            "1 つ目は目標の時刻にそのまま鳴ること"
        );

        // 2 つ目は 30 ms 後の TIMESTAMP なので、1 つ目の終わり (1_020_000) から
        // 10 ms の隙間が空く
        let second = scheduler.schedule(AudioPlayoutInput {
            now_us: 1_010_000,
            timestamp_us: 30_000,
            duration_us: 20_000,
            target_start_us: Some(1_030_000),
            enforce_target: true,
            delay_us: 80_000,
            presentation_delay_us: 80_000,
        });
        let AudioPlayoutDecision::Play {
            compress_us: 0,
            gap_us,
            ..
        } = second
        else {
            panic!("2 つ目は鳴ると決まること decision={second:?}");
        };
        assert_eq!(gap_us, 10_000, "1 つ目の終わりから 10 ms の隙間が空くこと");
        let audio = audio_frame(960, sample_rate);
        let applied = apply_audio_playout(&audio, Some(&previous), gap_us, 0);
        assert_eq!(applied.concealed_us, 10_000, "隙間を 10 ms 埋めること");
        assert_eq!(
            applied.concealment.len(),
            480,
            "埋めた音は 10 ms ぶんであること"
        );
        assert_eq!(applied.compressed_us, 0, "詰めは要求されていないこと");
        assert_eq!(applied.channels[0].len(), 960, "音は短くならないこと");
        scheduler.confirm_concealment(applied.concealed_us);
        assert_eq!(
            scheduler.concealed_us(),
            10_000,
            "実際に埋めた長さを返すこと"
        );

        // 3 つ目は目標を過ぎて届く。遅れの分だけ詰める
        let third = scheduler.schedule(AudioPlayoutInput {
            now_us: 1_100_000,
            timestamp_us: 50_000,
            duration_us: 20_000,
            target_start_us: Some(1_090_000),
            enforce_target: true,
            delay_us: 80_000,
            presentation_delay_us: 80_000,
        });
        let AudioPlayoutDecision::Play { compress_us, .. } = third else {
            panic!("3 つ目は鳴ると決まること decision={third:?}");
        };
        assert!(compress_us > 0, "目標を過ぎた分だけ詰めること");
        let applied = apply_audio_playout(&audio, Some(&previous), 0, compress_us);
        assert!(
            applied.compressed_us > 0,
            "実際に削れた長さを返すこと compressed_us={}",
            applied.compressed_us
        );
        assert!(
            applied.channels[0].len() < 960,
            "音を周期 1 つ分だけ短くすること length={}",
            applied.channels[0].len()
        );
        assert!(
            applied.concealment.is_empty(),
            "隙間が空かないときは埋めないこと"
        );
        scheduler.confirm_stretch(applied.compressed_us);
        assert_eq!(
            scheduler.compressed_us(),
            applied.compressed_us,
            "実際に削れた長さを返すこと"
        );
    }
}
