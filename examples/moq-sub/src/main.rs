//! MoQT サブスクライバークライアント
//!
//! リレーサーバーに接続し、指定トラックを SUBSCRIBE してデータを受信・デコード・再生する。
//! `--mp4` を指定すると受信した映像 / 音声を MP4 ファイルへ保存し、`--no-play` を併せて
//! 指定すると再生せずに受信と保存だけを行う。
//! draft-ietf-moq-transport-22、draft-ietf-moq-loc-04、draft-ietf-moq-msf-01 に準拠。
//!
//! 使い方:
//!   cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --namespace moq-example
//!   cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --namespace moq-example --mp4 out.mp4 --no-play
//!   cargo run -p moq-sub -- --url 'moqt://127.0.0.1:4443#msf:moq-example--video'
//!
//! `--namespace` を省略した場合は `--url` の `msf` fragment の track-identifier が示す
//! namespace を使う。どちらにも無い場合はエラーになる。
//!
//! 受信とデコードの本体は lib ターゲット (`lib.rs`) にあり、このバイナリは tracing の初期化、
//! Ctrl+C の待ち受け、タスクメトリクスのログ出力、tokio ランタイムの構築、フレームチャネルの
//! 準備、macOS の SDL プレイヤーの実行、終了コードの決定を行う。

use std::collections::VecDeque;

use moq_sub::jitter_buffer::AudioJitterBuffer;
use moq_sub::{DecodedAudioFrame, DecodedVideoFrame, cli, error, pipeline};
use shiguredo_moqt::name::serialize_namespace;
use shiguredo_moqt::playout::buffer::PlayoutBuffer;
use shiguredo_moqt::playout::scheduler::{
    AUDIO_PLAYOUT_DELAY_US, AudioPlayoutBasis, AudioPlayoutDecision, AudioPlayoutDropReason,
    AudioPlayoutInput, AudioPlayoutScheduler,
};
use shiguredo_moqt::playout::stretch;
use shiguredo_moqt::playout::timeline::{PlayoutTimeline, Track, audio_arrival_delay_us};
use shiguredo_moqt::playout::timing::{
    AudioMissReason, AudioMissTotal, AudioPlayoutMiss, AudioPlayoutTimingSnapshot,
    AudioPlayoutTimingStats, TimingSummary,
};

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
    // 再生ウィンドウのタイトルに出す namespace も、同じ理由で先に取り出しておく。
    // タイトルには §8.8 の表現 (例 `moq-example`) で出す
    let namespace = serialize_namespace(&config.namespace);
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
            &namespace,
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

/// 再生ウィンドウのタイトルを組み立てる
///
/// 複数の namespace を同時に購読するときに取り違えないよう、購読中の namespace を出す。
fn player_window_title(namespace: &str) -> String {
    format!("{namespace} - MoQT Subscriber")
}

/// メインスレッドで raw_player のイベントループを実行する
///
/// macOS では SDL のウィンドウ操作がメインスレッドでしか動作しないため、
/// raw_player はメインスレッドで動かし、MoQT 処理は別スレッドで実行する。
///
/// SDL の初期化やウィンドウ・レンダラー作成に失敗する環境でも panic せず、`Error::Player` として失敗を返す。
///
/// `namespace` は購読中のトラック名前空間で、再生ウィンドウのタイトルに表示する。
///
/// `audio_output_device` が [`cli::AudioOutputDevice::None`] のときは SDL の音声出力デバイスを
/// 開かず、受信済みの音声チャンクを数えるだけにする (スピーカーへ音を出さない)。このときは
/// 鳴らす時刻を決める必要が無いため、jitter buffer へも入れない。
fn run_raw_player(
    namespace: &str,
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

    // 音声と映像で共有する時間軸。復号の出力を両方ともここへ観測し、表示時刻は
    // 「LOC の TIMESTAMP + 基準の遅れ + 表示の遅れ」で決まる。A/V 同期の制御も
    // 観測のたびに時間軸が行う
    let mut timeline = PlayoutTimeline::new();
    // 映像は到着後すぐ表示せず、表示時刻まで保持して選ぶ。遅れて届いたフレームは
    // 追い越させず、表示時刻を過ぎた最新の 1 枚だけを表示する
    // 保持する上限も時間軸の設定から取る。上限を変えると表示の遅れの切り下げと保持数の
    // 両方へ同じ値が効く
    let video_queue_limit = timeline.config().video_queue_limit;
    let mut video_buffer = PlayoutBuffer::<DecodedVideoFrame>::new(video_queue_limit, Track::Video);

    // 音声は到着後すぐ鳴らさず、鳴らす時刻 (LOC の TIMESTAMP と学習した目標遅延から
    // 決まる) まで保持する。到着の揺らぎがそのまま音の途切れにならないようにするためである
    let mut audio_buffer = AudioJitterBuffer::new();
    // 保持から出した音をどの順でいつ鳴らすかは、スケジューラが決める。jitter buffer は
    // 鳴らす時刻まで保持する役割であり、順番・詰め・隙間はここへ集める
    let mut audio_state = AudioPlayoutAssembly::new();
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
                        &player_window_title(namespace),
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
                    // 表示の実績を時間軸へ記録する。予定時刻ではなく、実際に再生機器へ
                    // 渡した時刻を使う。TIMESTAMP の無いフレームは記録できない
                    if let Some(timestamp_us) = frame.timestamp_us {
                        timeline.record_presentation(Track::Video, timestamp_us, now_us);
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
                    // 配信が終わったあとは目標に従う意味が無く、目標より先の音を捨てると
                    // 末尾が欠けるため、到着順に鳴らす
                    if let Some(audio_player) = audio_player.as_ref() {
                        let now_us = wall_clock_us();
                        while let Some(audio) = audio_buffer.pop_oldest() {
                            released_audio += 1;
                            play_decoded_audio(
                                audio_player,
                                &mut audio_state,
                                &mut timeline,
                                &audio,
                                now_us,
                                false,
                            );
                        }
                    }
                }
            }
        }

        // 鳴らす時刻まで保持していた音声を再生機器へ渡す。鳴らすかどうかの判断は
        // スケジューラへ任せ、ここでは取り出すだけにする
        if let Some(audio_player) = audio_player.as_ref() {
            let now_us = wall_clock_us();
            // 鳴らす時刻を過ぎた音もここでは捨てない。遅れたまま鳴らすか、音が途切れた
            // ときに到着基準へ並べ直すかはスケジューラが音ごとに決める。
            //
            // 鳴らす時刻を決めるのは、保持から出したこの時点にする。到着時に決めると、
            // 保持している間 (再生の遅れぶん) に決めた時刻が古くなり、実際に積む時点の
            // 遅れを反映できない。詰めと隙間の補間は決めた直後に適用し、適用した長さを
            // その場でスケジューラと計器へ返す
            while let Some(audio) = audio_buffer.pop_releasable(now_us, AUDIO_OUTPUT_LEAD_US) {
                released_audio += 1;
                play_decoded_audio(
                    audio_player,
                    &mut audio_state,
                    &mut timeline,
                    &audio,
                    now_us,
                    true,
                );
            }
            // いま鳴っているサンプルの PTS を実績として記録する。予定時刻ではなく、再生機器が
            // 鳴らし終えたサンプル数から求めるため、実際の再生位置に追従する
            record_sounding_audio(audio_player, &mut audio_state, &mut timeline);
            if let Err(e) = audio_player.process() {
                tracing::warn!("Failed to process audio queue: {e}");
            }
            if last_audio_log.elapsed().as_millis() >= AUDIO_BUFFER_LOG_INTERVAL_MS {
                last_audio_log = std::time::Instant::now();
                // player_buffer は再生機器へ積んだまま鳴っていない長さである。これが 0 に
                // 近づくと音が途切れるため、目標遅延が足りているかをここで確認できる。
                // concealed と compressed は実際に適用できた長さの合計であり、skew_us は
                // 実績から求めた A/V のずれ (映像が音声より遅れていれば正) である。
                // 末尾の audio_state.log_fields は、鳴るはずの時刻・到着から鳴り始めるまで・
                // 予定に対する余裕 (p50 / p95)・理由別の捨て・閉ループが決めた目標遅延である
                let skew_us = timeline
                    .skew_us()
                    .map_or_else(|| "none".to_string(), |skew_us| skew_us.to_string());
                tracing::info!(
                    "Audio jitter buffer: target_delay={}ms pending={} player_buffer={:.0}ms released={} dropped_overflow={} dropped={} concealed={}ms concealments={} compressed={}ms skew_us={} {}",
                    timeline.presentation_delay_us(Track::Audio).unwrap_or(0) / 1000,
                    audio_buffer.len(),
                    audio_player.stats().audio_buffer_ms,
                    released_audio,
                    audio_buffer.dropped_overflow(),
                    audio_state.scheduler.drops(),
                    audio_state.scheduler.concealed_us() / 1000,
                    audio_state.scheduler.concealments(),
                    audio_state.scheduler.compressed_us() / 1000,
                    skew_us,
                    audio_state.log_fields(&timeline, now_us),
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
    // 音声の再生を止める。予約したまま鳴り始めなかった音はここで切り捨てられるため、
    // その分を計器へ記録し、最後の値 (理由別の捨てと閉ループの目標) をログに出す
    if audio_output_enabled {
        let stopped_us = wall_clock_us();
        audio_state.record_stopped(&mut timeline, stopped_us);
        tracing::info!(
            "Audio playout stopped: {}",
            audio_state.log_fields(&timeline, stopped_us)
        );
    }
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

/// 音声の再生の組み立て (時間軸・スケジューラ・計器・閉ループを 1 か所で扱う)
///
/// 1 つの音について、目標の開始時刻の決定 ([`AudioPlayoutAssembly::arrange`])、実際に
/// 適用した欠落の補間と詰めの反映 ([`AudioPlayoutAssembly::commit`])、計器への記録、
/// 閉ループへの観測の引き渡しをここへ集める。再生機器へ積む順序 (補間した音、今回の音)
/// だけは [`play_decoded_audio`] が受け持つ。
///
/// 時間軸への到着の記録は受信側 ([`AudioJitterBuffer::push`]) が済ませているため、
/// ここでは行わない。時間軸から読むのは目標の開始時刻と遅れだけである。
///
/// 再生機器へ積むための状態 (再生を開始したか、積んだ音の位置の対応、直前に鳴らした
/// 波形) も、音声の再生の経路で 1 つに持ち回るためここに置く。
///
/// 再生を止めるときは [`AudioPlayoutAssembly::record_stopped`] を呼ぶ。予約したまま
/// 鳴り始めなかった音は、ここで数えないとどの統計にも現れない。
struct AudioPlayoutAssembly {
    /// 最初の 1 枚で再生を開始したか
    started: bool,
    /// 鳴らす時刻・詰め・隙間を決める
    scheduler: AudioPlayoutScheduler,
    /// 鳴るはずの時刻・到着・鳴り始めと、理由別の捨てを記録する計器
    stats: AudioPlayoutTimingStats,
    /// 直前に鳴らした音の波形。欠落した隙間をこの末尾の周期で埋める
    last_played: Option<PlayedAudio>,
    /// 再生機器へ積んだ音の位置の対応
    position: AudioPlayoutPosition,
}

impl AudioPlayoutAssembly {
    /// 何も鳴らしていない状態で作る
    fn new() -> Self {
        Self {
            started: false,
            scheduler: AudioPlayoutScheduler::new(),
            stats: AudioPlayoutTimingStats::new(),
            last_played: None,
            position: AudioPlayoutPosition::new(),
        }
    }

    /// 1 つの音を鳴らす時刻へ予約する
    ///
    /// 時間軸から目標の開始時刻・揺らぎの遅れ・到着基準の遅れ・表示の遅れを取り、
    /// スケジューラへ渡す。鳴らさないと決まったときは、その理由 (並べすぎ) と長さを
    /// 計器へ記録し、閉ループへ観測を渡して `None` を返す。
    ///
    /// `arrival_us` は到着した音がまだ鳴っていない位置である。`now_us` は既に出力へ積んだ
    /// 分だけ先に進んでいるため、到着基準の遅れはその位置から数える。
    fn arrange(
        &mut self,
        timeline: &mut PlayoutTimeline,
        audio: &DecodedAudioFrame,
        now_us: i64,
        arrival_us: i64,
        enforce_target: bool,
    ) -> Option<AudioPlayoutArrangement> {
        let duration_us = pcm_duration_us(
            audio.pcm.len(),
            usize::from(audio.channels),
            audio.sample_rate,
        );
        // 目標の開始時刻は時間軸が返す「鳴らす時刻」である。まだ基準が無い、または基準が
        // ずれているときは None であり、そのときは目標に従わず到着基準で並べる
        let target_start_us = timeline.present_us(Track::Audio, audio.pts_us);
        // 揺らぎから求めた音声の遅れ。まだ学習していなければ既定値を下限にする。目標がある
        // ときは並べすぎの判定に使う
        let delay_us = timeline
            .learned_delay_us(Track::Audio)
            .max(AUDIO_PLAYOUT_DELAY_US);
        // 到着基準で鳴らすときの遅れは、学習した遅れを [80 ms, 100 ms] に切った値にする。
        // 学習した値には TIMESTAMP の壁時計からのずれが混じるため、そのまま使うと音がその分
        // だけ遅れて鳴る
        let arrival_delay_us = audio_arrival_delay_us(delay_us);
        // 表示の遅れは時間軸が返す値をそのまま渡す。まだ観測が無いときは既定値を使う
        let presentation_delay_us = timeline
            .presentation_delay_us(Track::Audio)
            .unwrap_or(AUDIO_PLAYOUT_DELAY_US);
        let decision = self.scheduler.schedule(AudioPlayoutInput {
            now_us,
            arrival_us,
            timestamp_us: audio.pts_us,
            duration_us,
            target_start_us,
            // 目標が決まっているときだけ目標を守る。無いときと、配信が終わったあとの
            // 吐き出し (`enforce_target` が false) では到着基準で並べる
            enforce_target: enforce_target && target_start_us.is_some(),
            delay_us,
            arrival_delay_us,
            presentation_delay_us,
        });
        let (start_at_us, basis, compress_us, gap_us) = match decision {
            AudioPlayoutDecision::Play {
                start_at_us,
                basis,
                compress_us,
                gap_us,
                // 補間する隙間の開始時刻は使わない。再生機器へ積む順で位置が決まる
                gap_start_us: _,
            } => (start_at_us, basis, compress_us, gap_us),
            AudioPlayoutDecision::Drop { reason } => {
                // 鳴らさなかった理由を計器の理由へ移す。並べすぎで捨てた長さを残すことで、
                // 閉ループが「捨てた長さぶん目標を増やす」判断に使える
                let miss_reason = match reason {
                    AudioPlayoutDropReason::Backlog => AudioMissReason::Backlog,
                };
                tracing::warn!(
                    "Discarded an audio chunk the playout scheduler dropped (reason {reason:?}, total {})",
                    self.scheduler.drops(),
                );
                self.record_miss(
                    timeline,
                    miss_reason,
                    now_us,
                    duration_us,
                    target_start_us,
                    Some(arrival_us),
                );
                return None;
            }
        };
        if basis == AudioPlayoutBasis::Arrival {
            // 目標を使えない、または目標から離れすぎて音が途切れていた。到着基準で並べた
            tracing::debug!("Scheduled an audio chunk by arrival at {start_at_us}us");
        }
        Some(AudioPlayoutArrangement {
            duration_us,
            target_start_us,
            arrival_us,
            compress_us,
            gap_us,
        })
    }

    /// 実際に積めた補間の長さを返す (呼び出し側が隙間を埋めた後に呼ぶ)
    fn confirm_concealment(&mut self, applied_us: i64) {
        self.scheduler.confirm_concealment(applied_us);
    }

    /// 予約した音へ実際に適用した詰めを、スケジューラ・計器・閉ループへ反映する
    ///
    /// 計器へ渡す値は `confirm_stretch` の後に `scheduler.last_play()` から読む。要求した
    /// 詰める長さと実際に詰めた長さは一致せず、実際に鳴る長さを記録する必要があるためで
    /// ある。`now_us` は今の時刻 (受信側の壁時計) であり、閉ループへ渡す観測の時刻になる。
    fn commit(
        &mut self,
        timeline: &mut PlayoutTimeline,
        audio: &DecodedAudioFrame,
        applied: AppliedAudioPlayout,
        now_us: i64,
    ) {
        self.scheduler.confirm_stretch(applied.compressed_us);
        // 鳴らすと決めた音を計器へ渡す。詰めた分を引いた後に読むことで、実際に鳴る長さが
        // 記録される
        if let Some(play) = self.scheduler.last_play() {
            self.stats.record_play(play);
        }
        self.feed_feedback(timeline, now_us);
        // 直前に鳴らした音として保持する。次の隙間はこの音の末尾を繰り返して埋める
        self.last_played = Some(PlayedAudio {
            channels: applied.channels,
            sample_rate: audio.sample_rate,
        });
    }

    /// 鳴らす準備の途中で失敗した音を計器へ記録する
    ///
    /// 再生機器へ積めなかった音は鳴らない。原因 (積む要求の失敗) を理由として残し、
    /// 閉ループへ観測を渡す。
    fn record_error(
        &mut self,
        timeline: &mut PlayoutTimeline,
        arrangement: &AudioPlayoutArrangement,
        now_us: i64,
    ) {
        self.record_miss(
            timeline,
            AudioMissReason::Error,
            now_us,
            arrangement.duration_us,
            arrangement.target_start_us,
            Some(arrangement.arrival_us),
        );
    }

    /// 再生を止めた分 (予約したまま鳴り始めなかった音) を計器へ記録する
    ///
    /// 購読のやり直しや再生の終了で音声の出力を閉じると、予約済みの音は鳴らないまま
    /// 切り捨てられる。既に鳴り始めている音は残りの長さだけを数える。
    fn record_stopped(&mut self, timeline: &mut PlayoutTimeline, now_us: i64) {
        self.stats.record_stopped(now_us);
        self.feed_feedback(timeline, now_us);
    }

    /// 鳴らさなかった音 1 つを計器へ記録し、閉ループへ観測を渡す
    fn record_miss(
        &mut self,
        timeline: &mut PlayoutTimeline,
        reason: AudioMissReason,
        at_us: i64,
        duration_us: i64,
        target_us: Option<i64>,
        arrival_us: Option<i64>,
    ) {
        self.stats.record_miss(AudioPlayoutMiss {
            at_us,
            reason,
            duration_us,
            target_us,
            arrival_us,
        });
        self.feed_feedback(timeline, at_us);
    }

    /// 計器の観測を閉ループへ渡し、決まった目標を時間軸へ反映させる
    ///
    /// 鳴らした結果 (予定をどれだけ過ぎて鳴ったか、並べすぎで捨てた量) から目標遅延を
    /// 決め直す。渡した観測は [`PlayoutTimeline::observe_audio_playout`] がその場で音声の
    /// 表示の遅れへ反映するため、次の音から新しい目標で並ぶ。
    fn feed_feedback(&mut self, timeline: &mut PlayoutTimeline, now_us: i64) {
        timeline.observe_audio_playout(self.stats.audio_delay_feedback(now_us));
    }

    /// 計器の観測値を求める
    fn snapshot(&mut self, now_us: i64) -> AudioPlayoutTimingSnapshot {
        self.stats.snapshot(now_us)
    }

    /// 計器と閉ループの値をログ用の文字列にする
    ///
    /// 鳴るはずの時刻 (絶対値)、到着から鳴り始めるまでと予定に対する余裕の分布 (p50 /
    /// p95)、理由別の捨ての件数と長さ、いま使っている目標遅延とその理由を出す。
    fn log_fields(&mut self, timeline: &PlayoutTimeline, now_us: i64) -> String {
        let snapshot = self.snapshot(now_us);
        let feedback = timeline.delay_breakdown().audio_delay_feedback;
        format!(
            "playout last_target_us={} last_arrival_us={} last_start_us={} last_slack={}ms last_start_delay={}ms last_lateness={}ms slack_p50_p95={}ms start_delay_p50_p95={}ms lateness_p50_p95={}ms played={} played_ms={} arrival_planned={} unplanned={} missed={} missed_ms={} missed_backlog={} missed_catch_up={} missed_error={} missed_stopped={} feedback_target={}ms feedback_reason={:?} feedback_change={}ms feedback_adjustments={}",
            time_log(snapshot.last_target_us),
            time_log(snapshot.last_arrival_us),
            time_log(snapshot.last_start_us),
            duration_log(snapshot.last_slack_us),
            duration_log(snapshot.last_start_delay_us),
            duration_log(snapshot.last_lateness_us),
            timing_summary_ms(snapshot.slack_us),
            timing_summary_ms(snapshot.start_delay_us),
            timing_summary_ms(snapshot.lateness_us),
            snapshot.played_frames,
            snapshot.played_us / 1000,
            snapshot.arrival_planned_frames,
            snapshot.unplanned_frames,
            snapshot.missed_frames,
            snapshot.missed_us / 1000,
            miss_total_log(snapshot.missed_by_reason.backlog),
            miss_total_log(snapshot.missed_by_reason.catch_up),
            miss_total_log(snapshot.missed_by_reason.error),
            miss_total_log(snapshot.missed_by_reason.stopped),
            feedback.target_us / 1000,
            feedback.reason,
            feedback.last_change_us / 1000,
            feedback.adjustments,
        )
    }

    /// 音声出力が実際に鳴っている位置 (受信側の壁時計) を、到着の基準として求める
    ///
    /// `raw_player` の `total_samples_played` と、積んだ音の (累積サンプル数, PTS) の対応から、
    /// いま鳴っている音の PTS を求める。PTS は媒体の軸であり `now_us` と同じ軸ではないため、
    /// 時間軸が同じトラックに決める時刻へ移してから使う。移せないとき (まだ一度も鳴らして
    /// いない、再生位置の対応が切れている、時間軸が音声の基準を持たない) は今の時刻を使う。
    fn sounding_position_us(
        &mut self,
        audio_player: &raw_player::AudioPlayer,
        timeline: &PlayoutTimeline,
        now_us: i64,
    ) -> i64 {
        let stats = audio_player.stats();
        if stats.sample_rate <= 0 {
            return now_us;
        }
        let Ok(sample_rate) = u32::try_from(stats.sample_rate) else {
            return now_us;
        };
        let Some(sounding_pts_us) = self
            .position
            .sounding_pts_us(stats.total_samples_played, sample_rate)
        else {
            return now_us;
        };
        // 表示の遅れを引くと、そのトラックの TIMESTAMP を壁時計へ移した値になる
        let Some(presentation_delay_us) = timeline.presentation_delay_us(Track::Audio) else {
            return now_us;
        };
        timeline
            .present_us(Track::Audio, sounding_pts_us)
            .map_or(now_us, |present_us| {
                present_us.saturating_sub(presentation_delay_us)
            })
    }
}

/// 1 つの音を鳴らすために決まった内容 (再生機器へ渡す前に決まるもの)
///
/// 鳴り始める時刻と使った計画そのものは、計器へ渡すために
/// `scheduler.last_play()` から読む (実際に詰めた長さが反映されているため)。ここには
/// 鳴らせなかったときの記録と、隙間と詰めの適用に必要な値だけを持つ。
struct AudioPlayoutArrangement {
    /// 音の長さ (マイクロ秒)
    duration_us: i64,
    /// 目標の開始時刻。無いこともある
    target_start_us: Option<i64>,
    /// 到着の基準 (到着した音がまだ鳴っていない位置)
    arrival_us: i64,
    /// 波形の周期で詰める長さ (マイクロ秒)。0 なら詰めない
    compress_us: i64,
    /// 補間する隙間の長さ (マイクロ秒)。0 なら補間しない
    gap_us: i64,
}

/// 時刻をログ用の値にする (マイクロ秒のまま。値が無ければ none)
///
/// 鳴るはずの時刻・到着の時刻・鳴り始める時刻は絶対値であり、他のログと同じ軸で読める
/// ようにするためマイクロ秒のまま出す。
fn time_log(value: Option<i64>) -> String {
    value.map_or_else(|| "none".to_string(), |value| value.to_string())
}

/// 長さをログ用のミリ秒にする (値が無ければ none)
fn duration_log(value: Option<i64>) -> String {
    value.map_or_else(|| "none".to_string(), |value| (value / 1_000).to_string())
}

/// 分布を p50 / p95 のミリ秒で表す (値が無ければ none)
fn timing_summary_ms(summary: Option<TimingSummary>) -> String {
    summary.map_or_else(
        || "none".to_string(),
        |summary| format!("{}/{}", summary.p50 / 1_000, summary.p95 / 1_000),
    )
}

/// 理由別の捨てを件数と長さ (ミリ秒) で表す
fn miss_total_log(total: AudioMissTotal) -> String {
    format!("{}/{}", total.count, total.duration_us / 1_000)
}

/// 再生機器へ積んだ音の (累積サンプル数, PTS) の対応
///
/// `raw_player` の再生位置は「積んだ順に進むサンプル数」でしか分からない。積んだ時点の
/// 累積サンプル数と PTS を残し、いま鳴っているサンプルの PTS をここから求める。詰めたり
/// 補間したりした後の実際のサンプル数で記録するため、宣言した PTS を線形に外挿するより
/// 正しく求まる。
struct AudioPlayoutPosition {
    /// まだ鳴り終わっていない音 (積んだ順)
    pending: VecDeque<AudioPlayoutChunk>,
}

/// 再生機器へ積んだ音 1 つ分の位置
struct AudioPlayoutChunk {
    /// この音が鳴り始める累積サンプル数
    start_samples: i64,
    /// この音の PTS (マイクロ秒)
    pts_us: i64,
    /// この音のサンプル数 (チャンネルごと)
    samples: i64,
}

impl AudioPlayoutPosition {
    /// 何も積んでいない状態で作る
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
        }
    }

    /// 積んだ音を記録する
    fn record(&mut self, start_samples: i64, pts_us: i64, samples: i64) {
        self.pending.push_back(AudioPlayoutChunk {
            start_samples,
            pts_us,
            samples,
        });
    }

    /// いま鳴っているサンプルの PTS (マイクロ秒)
    ///
    /// `played_samples` は再生機器が鳴らし終えたサンプル数。鳴り終えた音は捨てる。すべて
    /// 鳴り終えたときと、再生位置が積んだ範囲より前へ戻ったとき (デバイスの作り直し) は
    /// `None` を返す。後者は対応が切れているため、記録を捨てて次の積み込みからやり直す。
    fn sounding_pts_us(&mut self, played_samples: i64, sample_rate: u32) -> Option<i64> {
        while self.pending.front().is_some_and(|chunk| {
            chunk.start_samples.saturating_add(chunk.samples) <= played_samples
        }) {
            self.pending.pop_front();
        }
        let chunk = self.pending.front()?;
        let elapsed_samples = played_samples.checked_sub(chunk.start_samples)?;
        if elapsed_samples < 0 {
            self.pending.clear();
            return None;
        }
        let elapsed_samples = usize::try_from(elapsed_samples).ok()?;
        Some(
            chunk
                .pts_us
                .saturating_add(samples_to_us(elapsed_samples, sample_rate)),
        )
    }
}

/// デコード済みの音声を 1 つ、スケジューラの決めた時刻へ向けて再生機器へ積む
///
/// 鳴らす時刻の決定・計器への記録・閉ループへの観測の引き渡しは
/// [`AudioPlayoutAssembly`] が行う。ここは再生機器へ積む順序 (補間した音、今回の音) を
/// 受け持つ。`now_us` は今の時刻 (受信側の壁時計)。隙間は直前の音の末尾を周期で
/// 繰り返して埋め、詰めは波形の周期 1 つ分を削って行う。実際に適用した長さは
/// `confirm_concealment` と `confirm_stretch` で返す。
///
/// スケジューラが鳴らさないと決めた音は積まずに捨て、捨てた数と理由をログに出す
/// (鳴らせない音を積むと、その分だけ音が遅れたままになる)。
fn play_decoded_audio(
    audio_player: &raw_player::AudioPlayer,
    state: &mut AudioPlayoutAssembly,
    timeline: &mut PlayoutTimeline,
    audio: &DecodedAudioFrame,
    now_us: i64,
    enforce_target: bool,
) {
    let channels = usize::from(audio.channels);
    let sample_rate = audio.sample_rate;
    // 到着の基準は、音声出力が実際に鳴っている位置から求める。`now_us` は既に出力へ積んだ
    // 分だけ先に進んでいるため、到着基準の遅れはこの位置から数える
    let arrival_us = state.sounding_position_us(audio_player, timeline, now_us);
    let Some(arrangement) = state.arrange(timeline, audio, now_us, arrival_us, enforce_target)
    else {
        return;
    };

    // 隙間の補間と詰めを音声へ適用する。埋めた音は今回の音の直前へ積むため、再生機器の
    // キューでは前の音の直後、今回の音の直前になる (キューは積んだ順に鳴る)。隙間の開始
    // 時刻 (gap_start_us) は使わない。積む順で位置が決まるためである
    let applied = apply_audio_playout(
        audio,
        state.last_played.as_ref(),
        arrangement.gap_us,
        arrangement.compress_us,
    );
    let concealment_enqueued = if applied.concealment.is_empty() {
        if arrangement.gap_us > 0 {
            // 周期が求まらないと埋められない (無音のときは 0 で埋まる)。無音のまま続ける
            tracing::info!(
                "Could not conceal the {}us gap in the audio",
                arrangement.gap_us
            );
        }
        false
    } else {
        // 埋めた音は今回の TIMESTAMP の直前を占める。PTS は今回の音から埋めた長さだけ
        // 戻した値にする
        let pts_us = audio.pts_us.saturating_sub(applied.concealed_us);
        let enqueued = enqueue_pcm(
            audio_player,
            &mut state.started,
            &applied.concealment,
            pts_us,
            sample_rate,
            audio.channels,
        );
        if enqueued {
            let start_samples = audio_player.stats().total_samples_enqueued;
            state.position.record(
                start_samples,
                pts_us,
                (applied.concealment.len() / channels.max(1)) as i64,
            );
        }
        enqueued
    };
    // 積めた長さだけを補間したものとして返す。要求した長さを超える分は confirm が切る
    state.confirm_concealment(if concealment_enqueued {
        applied.concealed_us
    } else {
        0
    });

    // 今回の音を積む。積む前に数えた累積サンプル数が、この音の鳴り始める位置になる
    let pcm = pcm_channels_to_f32(
        &applied.channels,
        applied.channels.first().map_or(0, Vec::len),
    );
    let frames = pcm.len() / channels.max(1);
    let start_samples = audio_player.stats().total_samples_enqueued;
    if !enqueue_pcm(
        audio_player,
        &mut state.started,
        &pcm,
        audio.pts_us,
        sample_rate,
        audio.channels,
    ) {
        // 再生機器へ積めなかった音は鳴らない。鳴らす準備の途中で失敗した分として記録する
        state.record_error(timeline, &arrangement, now_us);
        return;
    }
    state
        .position
        .record(start_samples, audio.pts_us, frames as i64);
    // 適用した詰めをスケジューラへ返し、鳴らした結果を計器と閉ループへ反映する
    state.commit(timeline, audio, applied, now_us);
}

/// いま鳴っているサンプルの PTS を実績として時間軸へ記録する
///
/// `raw_player` は積んだ順にサンプルを鳴らすため、鳴らし終えたサンプル数と「積んだ時点の
/// 累積サンプル数」の対応から、いま鳴っているサンプルの PTS が求まる。予定時刻ではなく
/// 実際の再生位置を使うことで、`skew_us` が計画ではなく実測になる。
fn record_sounding_audio(
    audio_player: &raw_player::AudioPlayer,
    state: &mut AudioPlayoutAssembly,
    timeline: &mut PlayoutTimeline,
) {
    let stats = audio_player.stats();
    if stats.sample_rate <= 0 {
        return;
    }
    let played_samples = stats.total_samples_played;
    let Some(pts_us) = state
        .position
        .sounding_pts_us(played_samples, stats.sample_rate as u32)
    else {
        return;
    };
    timeline.record_presentation(Track::Audio, pts_us, wall_clock_us());
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
    use shiguredo_moqt::playout::feedback::{
        AUDIO_DELAY_FEEDBACK_START_US, AudioDelayFeedbackReason,
    };

    use super::*;

    /// 再生ウィンドウのタイトルに購読中の namespace が出ること
    ///
    /// タイトルに固定の名前を出すと、どの namespace を購読しているか分からなくなる。
    #[test]
    fn player_window_title_includes_namespace() {
        assert_eq!(
            player_window_title("moq-example"),
            "moq-example - MoQT Subscriber",
            "指定した namespace がそのままタイトルへ出ること"
        );
        assert_eq!(
            player_window_title("example.2ecom"),
            "example.2ecom - MoQT Subscriber",
            "エスケープを含む §8.8 表現でもそのままタイトルへ出ること"
        );
    }

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

    /// 積んだ位置の対応から、いま鳴っているサンプルの PTS が求まること
    #[test]
    fn audio_playout_position_follows_the_played_samples() {
        let mut position = AudioPlayoutPosition::new();
        // 0 サンプル目から 20 ms (48 kHz で 960 サンプル) の音を PTS 1_000_000 で積む
        position.record(0, 1_000_000, 960);
        assert_eq!(
            position.sounding_pts_us(0, 48_000),
            Some(1_000_000),
            "まだ鳴っていなければ積んだ先頭の PTS になること"
        );
        assert_eq!(
            position.sounding_pts_us(480, 48_000),
            Some(1_010_000),
            "480 サンプル (10 ms) 鳴れば PTS も 10 ms 進むこと"
        );
        // 次の音を続けて積む
        position.record(960, 1_020_000, 960);
        assert_eq!(
            position.sounding_pts_us(960, 48_000),
            Some(1_020_000),
            "次の音の先頭の PTS になること"
        );
        assert_eq!(
            position.sounding_pts_us(1_440, 48_000),
            Some(1_030_000),
            "次の音の途中も追従すること"
        );
        assert_eq!(
            position.sounding_pts_us(1_920, 48_000),
            None,
            "すべて鳴り終えたら分からないこと"
        );
    }

    /// 再生位置が戻ったら対応を捨ててやり直すこと
    #[test]
    fn audio_playout_position_clears_when_the_play_position_goes_back() {
        let mut position = AudioPlayoutPosition::new();
        position.record(5_000, 2_000_000, 960);
        // デバイスが作り直され、鳴らし終えたサンプル数が積んだ範囲より前へ戻った
        assert_eq!(
            position.sounding_pts_us(0, 48_000),
            None,
            "対応が切れたら分からないこと"
        );
        // 捨てたあとは次の積み込みからやり直す
        position.record(0, 3_000_000, 960);
        assert_eq!(
            position.sounding_pts_us(0, 48_000),
            Some(3_000_000),
            "積み直した位置から求まること"
        );
    }

    /// 埋められないときは隙間を組み立てないこと
    #[test]
    fn conceal_gap_returns_none_when_it_cannot_fill() {
        // 無音は周期が求まらないが、0 で埋まる (長さは要求どおり)
        let silent = PlayedAudio {
            channels: vec![vec![0.0f32; 4_800]],
            sample_rate: 48_000,
        };
        let (pcm, concealed_us) =
            conceal_gap(Some(&silent), 1, 48_000, 20_000).expect("無音でも長さは埋まること");
        assert_eq!(concealed_us, 20_000, "要求どおりの長さを埋めること");
        assert!(
            pcm.iter().all(|sample| *sample == 0.0),
            "無音の隙間は 0 で埋まること"
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
            arrival_us: 990_000,
            timestamp_us: 0,
            duration_us: 20_000,
            target_start_us: Some(1_000_000),
            enforce_target: true,
            delay_us: 80_000,
            arrival_delay_us: 80_000,
            presentation_delay_us: 80_000,
        });
        assert_eq!(
            first,
            AudioPlayoutDecision::Play {
                start_at_us: 1_000_000,
                basis: AudioPlayoutBasis::Timestamp,
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
            arrival_us: 1_010_000,
            timestamp_us: 30_000,
            duration_us: 20_000,
            target_start_us: Some(1_030_000),
            enforce_target: true,
            delay_us: 80_000,
            arrival_delay_us: 80_000,
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
            arrival_us: 1_100_000,
            timestamp_us: 50_000,
            duration_us: 20_000,
            target_start_us: Some(1_090_000),
            enforce_target: true,
            delay_us: 80_000,
            arrival_delay_us: 80_000,
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

    /// テスト用に、隙間を埋めずに詰めだけを適用した結果を作る
    ///
    /// 組み立てへ渡す値は、実際に音声へ適用した長さと同じ形にする。再生機器へ積む代わりに
    /// この値をそのまま `commit` へ渡す。
    fn applied_audio(audio: &DecodedAudioFrame, compressed_us: i64) -> AppliedAudioPlayout {
        AppliedAudioPlayout {
            channels: pcm_f32_to_channels(&pcm_i16_to_f32(&audio.pcm), usize::from(audio.channels)),
            concealment: Vec::new(),
            concealed_us: 0,
            compressed_us,
        }
    }

    /// テスト用に、時間軸が決めた目標の時刻へ 1 つ鳴らすと予約して計器へ記録する
    ///
    /// 再生機器を使わないため、詰めも補間も無し (`compressed_us` は 0) として反映する。
    fn commit_one_play(
        assembly: &mut AudioPlayoutAssembly,
        timeline: &mut PlayoutTimeline,
        audio: &DecodedAudioFrame,
        now_us: i64,
    ) {
        assembly
            .arrange(timeline, audio, now_us, now_us, true)
            .expect("目標の時刻に鳴ると決まること");
        assembly.commit(timeline, audio, applied_audio(audio, 0), now_us);
    }

    /// 組み立てが、鳴らすと決めた音を実際に詰めた長さで計器へ記録すること
    #[test]
    fn playout_assembly_records_the_play_in_the_stats() {
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        let sample_rate = 48_000;
        let arrival_us = 10_000_000;
        // 到着の記録は受信側 (jitter buffer) が行う
        timeline.observe(Track::Audio, arrival_us, 0);
        let audio = audio_frame(960, sample_rate);
        let target_us = timeline
            .present_us(Track::Audio, audio.pts_us)
            .expect("観測したので音声の目標が決まること");
        // 目標の時刻に届いた音を予約する。今から鳴らせる最も早い時刻 (10 ms) へずれ、
        // ずれた分だけ詰める要求が出る
        let arrangement = assembly
            .arrange(&mut timeline, &audio, target_us, target_us, true)
            .expect("目標に間に合う音は鳴ると決まること");
        assert_eq!(arrangement.duration_us, 20_000, "音の長さを返すこと");
        let play = assembly
            .scheduler
            .last_play()
            .expect("鳴らすと決めた音が残ること");
        assert_eq!(
            play.played_us, 10_000,
            "要求した詰める長さを引いた長さになること"
        );
        // 実際に詰められた長さは要求より短い (波形の周期でしか削れない)。実際の長さだけを
        // 記録すること
        assembly.commit(
            &mut timeline,
            &audio,
            applied_audio(&audio, 5_000),
            target_us,
        );
        let snapshot = assembly.snapshot(target_us);
        assert_eq!(snapshot.played_frames, 1, "鳴らすと決めた音を計上すること");
        assert_eq!(
            snapshot.missed_frames, 0,
            "鳴らした音を鳴らさなかったことにしないこと"
        );
        assert_eq!(
            snapshot.played_us, 15_000,
            "実際に詰めた長さを反映した鳴る長さを記録すること"
        );
        assert_eq!(
            snapshot.last_target_us,
            Some(target_us),
            "鳴るはずの時刻を記録すること"
        );
        assert_eq!(
            snapshot.last_arrival_us,
            Some(target_us),
            "到着の基準を記録すること"
        );
        assert_eq!(
            snapshot.last_slack_us,
            Some(0),
            "予定に対する余裕を記録すること"
        );
        assert_eq!(
            snapshot.last_start_delay_us,
            Some(10_000),
            "到着から鳴り始めるまでを記録すること"
        );
        assert_eq!(
            snapshot.last_lateness_us,
            Some(10_000),
            "予定からどれだけ過ぎて鳴るかを記録すること"
        );
    }

    /// 組み立てが計器の観測を閉ループへ渡し、決まった目標を時間軸へ反映すること
    #[test]
    fn playout_assembly_feeds_the_closed_loop() {
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        let arrival_us = 10_000_000;
        timeline.observe(Track::Audio, arrival_us, 0);
        assert!(
            timeline
                .delay_breakdown()
                .audio_delay_feedback
                .lateness_p50_us
                .is_none(),
            "まだ鳴らしていないので閉ループの観測が無いこと"
        );
        // 観測が届く前は、揺らぎの学習だけが目標遅延を決めている
        assert_eq!(
            timeline.learned_delay_us(Track::Audio),
            80_000,
            "学習の初期値が目標遅延になること"
        );
        let audio = audio_frame(960, 48_000);
        let target_us = timeline
            .present_us(Track::Audio, audio.pts_us)
            .expect("観測したので音声の目標が決まること");
        commit_one_play(&mut assembly, &mut timeline, &audio, target_us);
        // 閉ループの観測には、鳴らした結果 (余裕・到着から鳴り始めるまで・予定からの遅れ) が
        // そのまま出る。観測が渡っていなければどれも None のままになる
        let feedback = timeline.delay_breakdown().audio_delay_feedback;
        assert_eq!(
            feedback.slack_p50_us,
            Some(0),
            "予定に対する余裕が閉ループへ届くこと"
        );
        assert_eq!(
            feedback.start_delay_p50_us,
            Some(10_000),
            "到着から鳴り始めるまでが閉ループへ届くこと"
        );
        assert_eq!(
            feedback.lateness_p50_us,
            Some(10_000),
            "予定を過ぎて鳴った量が閉ループへ届くこと"
        );
        // 閉ループが決めた目標 (観測が届いた後は初期値の 100 ms) が時間軸へ反映され、
        // 以降の音はその目標で並ぶ
        assert_eq!(
            timeline.learned_delay_us(Track::Audio),
            AUDIO_DELAY_FEEDBACK_START_US,
            "閉ループが決めた目標が時間軸へ反映されること"
        );
    }

    /// 並べすぎで鳴らさないと決めた音を、理由付きで計器と閉ループへ渡すこと
    #[test]
    fn playout_assembly_records_the_backlog_miss() {
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        let arrival_us = 10_000_000;
        timeline.observe(Track::Audio, arrival_us, 0);
        // 目標が今から大きく先になる (並べすぎの上限を超える) TIMESTAMP の音を作る
        let mut audio = audio_frame(960, 48_000);
        audio.pts_us = 5_000_000;
        assert!(
            assembly
                .arrange(&mut timeline, &audio, arrival_us, arrival_us, true)
                .is_none(),
            "並べすぎの音は鳴らさないと決まること"
        );
        let snapshot = assembly.snapshot(arrival_us);
        assert_eq!(
            snapshot.played_frames, 0,
            "鳴らさなかった音を鳴らしたことにしないこと"
        );
        assert_eq!(snapshot.missed_frames, 1, "鳴らさなかった音を計上すること");
        assert_eq!(
            snapshot.missed_us, 20_000,
            "鳴らさなかった長さを計上すること"
        );
        assert_eq!(
            snapshot.missed_by_reason.backlog,
            AudioMissTotal {
                count: 1,
                duration_us: 20_000,
            },
            "並べすぎとして理由別に数えること"
        );
        assert_eq!(
            snapshot.recent_misses.first().map(|miss| miss.reason),
            Some(AudioMissReason::Backlog),
            "直近の捨ての理由が並べすぎであること"
        );
        // 捨てた量は閉ループへ渡り、目標遅延を増やす判断に使われる
        let feedback = timeline.delay_breakdown().audio_delay_feedback;
        assert_eq!(
            feedback.reason,
            AudioDelayFeedbackReason::Backlog,
            "並べすぎの捨てが閉ループへ届くこと"
        );
        assert_eq!(
            feedback.target_us,
            AUDIO_DELAY_FEEDBACK_START_US + 40_000,
            "捨てた長さと余白ぶん目標を増やすこと"
        );
    }

    /// 鳴らす準備に失敗した音を、理由付きで計器へ記録すること
    #[test]
    fn playout_assembly_records_the_error_miss() {
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        let now_us = 10_000_000;
        timeline.observe(Track::Audio, now_us, 0);
        let audio = audio_frame(960, 48_000);
        let arrangement = assembly
            .arrange(&mut timeline, &audio, now_us, now_us, true)
            .expect("目標の時刻に鳴ると決まること");
        // 再生機器へ積む要求が失敗した場合に相当する
        assembly.record_error(&mut timeline, &arrangement, now_us);
        let snapshot = assembly.snapshot(now_us);
        assert_eq!(
            snapshot.played_frames, 0,
            "積めなかった音を鳴らしたことにしないこと"
        );
        assert_eq!(
            snapshot.missed_by_reason.error,
            AudioMissTotal {
                count: 1,
                duration_us: 20_000,
            },
            "準備の失敗として理由別に数えること"
        );
    }

    /// 再生を止めたときに、予約したまま鳴らなかった音を記録すること
    ///
    /// 再生機器を閉じると予約済みの音は鳴らないまま切り捨てられる。ここで数えないと
    /// この分はどの統計にも現れない。
    #[test]
    fn playout_assembly_records_the_stopped_audio() {
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        let now_us = 10_000_000;
        timeline.observe(Track::Audio, now_us, 0);
        let audio = audio_frame(960, 48_000);
        commit_one_play(&mut assembly, &mut timeline, &audio, now_us);
        let play = assembly
            .scheduler
            .last_play()
            .expect("鳴らすと決めた音が残ること");
        assert!(
            play.start_at_us > now_us,
            "まだ鳴り始めていない音を予約していること"
        );
        // 鳴り始める前に止める。予約した長さの全体が鳴らなかった分になる
        assembly.record_stopped(&mut timeline, now_us);
        assert_eq!(
            assembly.snapshot(now_us).missed_by_reason.stopped,
            AudioMissTotal {
                count: 1,
                duration_us: 20_000,
            },
            "予約したまま鳴り始めなかった音を止めた分として数えること"
        );
        // 鳴り始めている音は、残りの長さだけを数える
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        timeline.observe(Track::Audio, now_us, 0);
        commit_one_play(&mut assembly, &mut timeline, &audio, now_us);
        let play = assembly
            .scheduler
            .last_play()
            .expect("鳴らすと決めた音が残ること");
        assembly.record_stopped(&mut timeline, play.start_at_us + 10_000);
        assert_eq!(
            assembly.snapshot(now_us).missed_by_reason.stopped,
            AudioMissTotal {
                count: 1,
                duration_us: 10_000,
            },
            "鳴り始めている音は残りの長さだけを数えること"
        );
    }

    /// 計器と閉ループの値がログの文字列に出ること
    #[test]
    fn audio_playout_log_reports_the_instrument() {
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        let now_us = 10_000_000;
        timeline.observe(Track::Audio, now_us, 0);
        let audio = audio_frame(960, 48_000);
        commit_one_play(&mut assembly, &mut timeline, &audio, now_us);
        // 理由別の捨て (並べすぎ) も混ぜる
        let mut late_audio = audio_frame(960, 48_000);
        late_audio.pts_us = 5_000_000;
        assert!(
            assembly
                .arrange(&mut timeline, &late_audio, now_us, now_us, true)
                .is_none(),
            "並べすぎの音は鳴らさないと決まること"
        );
        let line = assembly.log_fields(&timeline, now_us);
        for key in [
            "last_target_us=",
            "last_arrival_us=",
            "last_start_us=",
            "slack_p50_p95=",
            "start_delay_p50_p95=",
            "lateness_p50_p95=",
            "missed_backlog=",
            "missed_stopped=",
            "feedback_target=",
            "feedback_reason=",
        ] {
            assert!(line.contains(key), "ログに {key} が出ること line={line}");
        }
    }
}
