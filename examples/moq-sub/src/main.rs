//! MOQT サブスクライバークライアント
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
//! `--transport` を省略した場合は `--url` の `msf` fragment の `connection` を使う
//! (draft-ietf-moq-msf-01 §11.1.1)。どちらにも無い場合は `quic` になる。`connection` と矛盾する
//! `--transport` はエラーになる。
//!
//! 受信とデコードの本体は lib ターゲット (`lib.rs`) にあり、このバイナリは tracing の初期化、
//! Ctrl+C と SIGTERM の待ち受け、タスクメトリクスのログ出力、tokio ランタイムの構築、フレームチャネルの
//! 準備、macOS の SDL プレイヤーの実行、終了コードの決定を行う。

use std::collections::VecDeque;

use moq_sub::{DecodedAudioFrame, DecodedVideoFrame, cli, error, pipeline, wall_clock_us};
use shiguredo_moqt::name::serialize_namespace;
use shiguredo_moqt::playout::buffer::PlayoutBuffer;
use shiguredo_moqt::playout::gap;
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

/// 出力が枯れたと判定する許容 (マイクロ秒)
///
/// 音声を処理するのは 1 ms 間隔のループであり、ループの処理時間で「最後に並べた音の
/// 終わり」をわずかに過ぎることがある。この揺れをアンダーランとして数えないための余裕で
/// ある。これを超えて過ぎたときだけ、出力が実際に枯れたものとして扱う。
const AUDIO_UNDERRUN_ALLOWANCE_US: i64 = 1_000;

/// 音声の再生の状態をログに出す間隔 (ミリ秒)
const AUDIO_BUFFER_LOG_INTERVAL_MS: u128 = 5_000;

fn main() {
    // SDL3 は初期化のときに SIGINT と SIGTERM のハンドラを自前で入れ、受け取った
    // シグナルを SDL の終了イベントへ変えてしまう。この example は SDL のイベントを
    // 回さないため、ハンドラが入ると SIGTERM が捨てられて終了できなくなる
    // (Ctrl+C も、あとから登録した tokio のハンドラが上書きするまで効かない)。
    // SDL の初期化 (`raw_player::init`) より前に環境変数で止めて、シグナルは
    // tokio のハンドラだけが扱うようにする
    //
    // SAFETY: ここはまだどのスレッドも起動していない (tokio ランタイムも
    // `raw_player` もこの後で作る)。環境変数を読むスレッドが無いため、他の
    // スレッドと競合しない
    unsafe {
        std::env::set_var("SDL_NO_SIGNAL_HANDLERS", "1");
    }

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
    // MOQT 処理は別スレッドの tokio ランタイムで動かす。
    std::thread::spawn(move || {
        let result = rt.block_on(async move {
            // タスクメトリクスを定期的にログ出力する
            tokio_moq::metrics::spawn_task_metrics_logger(task_monitor.clone());

            // Ctrl+C (SIGINT) と SIGTERM で graceful shutdown を起動する
            tokio::spawn(async move {
                let received = wait_for_shutdown_signal().await;
                tracing::info!("Received {received}, initiating graceful shutdown");
                // shutdown が完了しない場合 (peer が stream を閉じない等) に備え、
                // 2 回目のシグナルを待ちながら graceful shutdown する
                tokio::select! {
                    _ = shutdown.shutdown() => {}
                    _ = wait_for_shutdown_signal() => {
                        tracing::warn!("Received second shutdown signal, forcing exit");
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
        // プレイヤーを動かさず、pipeline の終了 (Ctrl+C / SIGTERM / relay からの切断) を待つ
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

/// 終了を要求するシグナルを待ち、受け取ったシグナルの名前を返す
///
/// Ctrl+C (SIGINT) と SIGTERM を待つ。どちらも graceful shutdown の起動に使う。
/// SIGTERM を登録できない環境では Ctrl+C だけを待つ (終了の手段を残すため)。
///
/// SDL が自前のハンドラを入れるとシグナルが SDL の終了イベントへ変わり、ここへ
/// 届かなくなる。`main` が SDL の初期化より前に `SDL_NO_SIGNAL_HANDLERS` を
/// 設定しているのはそのためである。
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

/// 再生ウィンドウのタイトルを組み立てる
///
/// どの接続先の再生かを示すため、catalog track の namespace (example が接続時に指定した
/// namespace) を出す。track が namespace を宣言している場合は購読に使う namespace が異なり得る
/// (`resolve_track_namespace` を参照)。
fn player_window_title(namespace: &str) -> String {
    format!("{namespace} - MOQT Subscriber")
}

/// 映像の描画が停止したと判定するまでの時間 (マイクロ秒)
///
/// raw_player 2026.3 は映像のみ経路で描画が止まったとき最新フレームへ復帰するが、この
/// example はそれより前のプレイヤーでも動くよう保険を持つ。描画がこの時間だけ進まなければ、
/// キューと時計の基準を捨てて次の投入から再生し直す
/// ([`raw_player::video_player::VideoPlayer::drain_video`])。
const VIDEO_STALL_TIMEOUT_US: u64 = 1_000_000;

/// 停止と判定するために、描画が止まっている間に必要な投入フレーム数
///
/// 1 枚だけでは、まだ描画の機会が来ていないだけかもしれない。2 枚以上を投入しても描画が
/// 進まなければ、プレイヤーの再生時計がずれていると判断できる。
const VIDEO_STALL_MIN_ENQUEUED: u64 = 2;

/// 映像の描画会計をログに出す間隔 (ミリ秒)
const VIDEO_PLAYER_LOG_INTERVAL_MS: u128 = 1_000;

/// 映像の描画会計と停止判定
///
/// raw_player は映像のキュー先頭から 1 フレームずつ描画し、キュー上限を超えた分と再生時計
/// から遅れすぎた分は先頭から捨てる。どのフレームが実際に描画されたかを知るために、投入した
/// フレームの TIMESTAMP を投入順に保持し、`stats()` が示す描画数・破棄数の差分を先頭から
/// 取り除く。
///
/// `Instant` を内部に持たず、単調増加する時刻 (マイクロ秒) を引数で受け取るため、SDL も
/// プレイヤーも使わずに単体テストできる。
#[derive(Default)]
struct VideoPlayoutAccounting {
    /// プレイヤーの映像キューに残っていると考えられるフレームの TIMESTAMP
    ///
    /// TIMESTAMP の無いフレームも、描画・破棄の差分とキューの位置をずらさないために
    /// `None` として積む。時間軸へ記録するのは `Some` のフレームだけである。
    pending_timestamps: VecDeque<Option<i64>>,
    /// 前回観測した `total_frames_rendered`
    last_rendered: i64,
    /// 前回観測した `dropped_frames`
    last_dropped: i64,
    /// 投入時にキュー上限を超えて先頭を捨てた数
    ///
    /// この破棄は次の `stats()` の差分にも現れる。投入の時点で FIFO からも取り除いて
    /// いるため、差分を処理するときに二重に取り除かないよう数えておく。
    unobserved_enqueue_drops: u64,
    /// プレイヤーの映像キューの上限 (0 は無制限)
    max_queue: usize,
    /// 最後に描画を観測した時刻 (単調増加マイクロ秒)
    last_render_us: u64,
    /// 最後に描画を観測してから投入したフレーム数
    enqueued_since_render: u64,
    /// drain_video で復帰した回数
    recoveries: u64,
    /// 観測した描画フレーム数の累計 (プレイヤーの作り直しをまたいで数える)
    rendered_total: u64,
    /// 観測した破棄フレーム数の累計 (プレイヤーの作り直しをまたいで数える)
    dropped_total: u64,
}

/// [`VideoPlayoutAccounting::observe`] が返す、1 回の観測の結果
struct VideoPlayoutObservation {
    /// この観測で描画されたフレームの TIMESTAMP (描画順。TIMESTAMP の無いフレームは含まない)
    rendered_timestamps: Vec<i64>,
    /// 描画が止まっていると判定したか
    stalled: bool,
    /// 最後に描画を観測してからの経過時間 (マイクロ秒)。停止していなければ 0
    stalled_for_us: u64,
}

impl VideoPlayoutAccounting {
    /// プレイヤーを作り直したときに、実績の基準と FIFO をリセットする
    ///
    /// 作り直したプレイヤーの `stats()` は 0 から始まるため、基準を取り直す。復帰回数と
    /// 描画・破棄の累計は、プレイヤーの作り直しをまたいで数え続ける。
    ///
    /// `rendered` / `dropped` は新しいプレイヤーの `stats()` が示す描画数・破棄数である。
    /// 会計はプレイヤーの型に依存させず、数値だけを受け取る (`VideoPlayerStats` の
    /// フィールドが増減しても、この会計とそのテストは影響を受けない)。
    ///
    /// `now_us` は単調増加する時刻 (マイクロ秒) で、停止判定の起点になる。
    fn reset_for_player(&mut self, max_queue: usize, rendered: i64, dropped: i64, now_us: u64) {
        self.pending_timestamps.clear();
        self.last_rendered = rendered;
        self.last_dropped = dropped;
        self.unobserved_enqueue_drops = 0;
        self.max_queue = max_queue;
        self.last_render_us = now_us;
        self.enqueued_since_render = 0;
    }

    /// プレイヤーへ投入したフレームを記録する
    ///
    /// プレイヤーが投入を受け付けたフレームについてだけ呼ぶ。投入に失敗したフレームは
    /// プレイヤーのキューに残らないため、対応付けに含めてはいけない。
    fn record_enqueue(&mut self, timestamp_us: Option<i64>) {
        self.enqueued_since_render += 1;
        // raw_player はキューが上限に達した状態で投入されると、先頭を 1 枚捨ててから
        // 積む。その破棄は次の `stats()` の差分に現れるため、ここでは FIFO の先頭も
        // 同じように取り除き、差分を処理するときに二重に取り除かないよう数を残す
        if self.max_queue > 0 && self.pending_timestamps.len() >= self.max_queue {
            self.pending_timestamps.pop_front();
            self.unobserved_enqueue_drops += 1;
        }
        self.pending_timestamps.push_back(timestamp_us);
    }

    /// `stats()` の差分から描画・破棄を会計へ反映する
    ///
    /// `current_rendered` / `current_dropped` は `stats()` が示す累計値である。
    /// `now_us` は単調増加する時刻 (マイクロ秒)。描画の差分があれば、その時刻を
    /// 「最後に描画を観測した時刻」として停止判定をやり直す。
    fn observe(
        &mut self,
        current_rendered: i64,
        current_dropped: i64,
        now_us: u64,
    ) -> VideoPlayoutObservation {
        let rendered = counter_increase(current_rendered, self.last_rendered);
        let dropped = counter_increase(current_dropped, self.last_dropped);
        self.last_rendered = current_rendered;
        self.last_dropped = current_dropped;

        // 投入時に上限超過で捨てた分は、投入の時点で FIFO から取り除いてある。同じ
        // 破棄が stats の差分にも含まれるため、その分だけ差し引いてから残りを取り除く
        let already_removed = self.unobserved_enqueue_drops.min(dropped);
        self.unobserved_enqueue_drops -= already_removed;
        for _ in 0..(dropped - already_removed) {
            self.pending_timestamps.pop_front();
        }

        // 破棄はすべて描画より前 (投入時、または描画ループの先頭) に起きるため、
        // 描画された分は破棄を取り除いた後の先頭から順に対応する
        let mut rendered_timestamps = Vec::new();
        for _ in 0..rendered {
            if let Some(Some(timestamp_us)) = self.pending_timestamps.pop_front() {
                rendered_timestamps.push(timestamp_us);
            }
        }

        self.rendered_total += rendered;
        self.dropped_total += dropped;
        if rendered > 0 {
            self.last_render_us = now_us;
            self.enqueued_since_render = 0;
        }

        let stalled_for_us = now_us.saturating_sub(self.last_render_us);
        let stalled = self.enqueued_since_render >= VIDEO_STALL_MIN_ENQUEUED
            && stalled_for_us >= VIDEO_STALL_TIMEOUT_US;
        VideoPlayoutObservation {
            rendered_timestamps,
            stalled,
            stalled_for_us: if stalled { stalled_for_us } else { 0 },
        }
    }

    /// `drain_video` で停止から復帰したことを記録する
    ///
    /// `drain_video` はキューを空にし、再生時計の基準を捨てる。ただし `stats()` の描画数・
    /// 破棄数はリセットしないため、基準だけを取り直して二重に数えないようにする。キューに
    /// 残っていたフレームは描画されないまま消えるが、プレイヤーはそれを破棄として数えない
    /// ので、ここでも破棄には加えない。
    fn note_recovered(&mut self, rendered: i64, dropped: i64, now_us: u64) {
        self.pending_timestamps.clear();
        self.unobserved_enqueue_drops = 0;
        self.last_rendered = rendered;
        self.last_dropped = dropped;
        self.last_render_us = now_us;
        self.enqueued_since_render = 0;
        self.recoveries += 1;
    }

    /// 停止から復帰した回数
    fn recoveries(&self) -> u64 {
        self.recoveries
    }

    /// 実際に描画されたと観測したフレーム数の累計
    fn rendered_total(&self) -> u64 {
        self.rendered_total
    }

    /// プレイヤーが破棄したと観測したフレーム数の累計
    fn dropped_total(&self) -> u64 {
        self.dropped_total
    }

    /// 最後に描画を観測してから投入したフレーム数
    fn enqueued_since_render(&self) -> u64 {
        self.enqueued_since_render
    }
}

/// 単調増加するはずのカウンタの増分を求める
///
/// プレイヤーを作り直した直後など、基準を取り直す前に値が減った場合は 0 として扱う。
fn counter_increase(current: i64, previous: i64) -> u64 {
    u64::try_from(current.saturating_sub(previous)).unwrap_or(0)
}

/// メインスレッドで raw_player のイベントループを実行する
///
/// macOS では SDL のウィンドウ操作がメインスレッドでしか動作しないため、
/// raw_player はメインスレッドで動かし、MOQT 処理は別スレッドで実行する。
///
/// SDL の初期化やウィンドウ・レンダラー作成に失敗する環境でも panic せず、`Error::Player` として失敗を返す。
///
/// `namespace` は購読中のトラック名前空間で、再生ウィンドウのタイトルに表示する。
///
/// `audio_output_device` が [`cli::AudioOutputDevice::None`] のときは SDL の音声出力デバイスを
/// 開かず、受信済みの音声チャンクを数えるだけにする (スピーカーへ音を出さない)。このときは
/// 時間軸への観測も再生の組み立ても行わない。
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

    let mut video_player: Option<raw_player::video_player::VideoPlayer> = None;
    // 音声出力を行わない指定では AudioPlayer を作らない。
    // raw_player の AudioPlayer は生成時点ではデバイスを開かず play() で開くが、
    // 生成自体を避けることで音声出力の経路へ一切入らないようにする。
    let audio_player = match audio_output_device {
        cli::AudioOutputDevice::Default => Some(raw_player::audio_player::AudioPlayer::new()),
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

    // 音声は受信したその場で鳴らす時刻を決めて積む。積んだ順に鳴る FIFO であるため、
    // 最初の音の開始時刻に達するまでのフレームは再生機器の内部キューに先行バッファとして
    // 溜まる。順番・詰め・隙間はスケジューラが決め、その結果をここへ集める
    let mut audio_state = AudioPlayoutAssembly::new();
    // 前回のログ以降に観測した再生機器のバッファ量の最小値 (ミリ秒)。枯れに近づいたかを
    // ログで読むために残す。再生を開始する前はバッファ量に意味が無いため数えない
    let mut player_buffer_min_ms: Option<f64> = None;
    let mut last_audio_log = std::time::Instant::now();

    // wall-clock PTS: 再生開始からの経過時間を映像 PTS として使用する
    let start_time = std::time::Instant::now();
    // プレイヤーへ投入した映像フレーム数。実際に描画された数ではない
    let mut video_enqueued_count: u64 = 0;
    // 映像の描画会計。プレイヤーを作った時点で実績の基準を取り直す
    let mut video_accounting = VideoPlayoutAccounting::default();
    // 直近の観測でプレイヤーの映像キューに残っていたフレーム数 (定期ログ用)
    let mut video_queue_size: usize = 0;
    // 直近の観測でプレイヤーが示した破棄の内訳と復帰回数 (定期ログ用)
    let mut video_player_dropped_queue_full: i64 = 0;
    let mut video_player_dropped_late: i64 = 0;
    let mut video_player_dropped_resync: i64 = 0;
    let mut video_player_resyncs: i64 = 0;
    let mut last_video_log = std::time::Instant::now();
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
                "PLAYERDIAG t={}ms enqueued={} rendered={} audio={} backlog={}",
                player_start.elapsed().as_millis(),
                video_enqueued_count,
                video_accounting.rendered_total(),
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
            let poll_result = p.poll_events();
            // 実際の描画は poll_events の中で起きる。投入した時点で数えると描画が
            // 止まっていても実績が進んでしまうため、直後に stats を読んで差分を取る
            let stats = p.stats();
            video_queue_size = stats.video_queue_size;
            // 破棄の内訳とプレイヤー側の復帰回数も、原因の切り分け用に残す
            video_player_dropped_queue_full = stats.dropped_frames_queue_full;
            video_player_dropped_late = stats.dropped_frames_late;
            video_player_dropped_resync = stats.dropped_frames_resync;
            video_player_resyncs = stats.resync_count;
            // 停止判定には単調増加する経過時間を使う (壁時計は補正で飛び得る)
            let now_mono_us = start_time.elapsed().as_micros() as u64;
            let observation = video_accounting.observe(
                stats.total_frames_rendered,
                stats.dropped_frames,
                now_mono_us,
            );
            // 描画を観測した時点で、そのフレームの TIMESTAMP を表示の実績として記録する
            let now_us = wall_clock_us();
            for timestamp_us in observation.rendered_timestamps {
                timeline.record_presentation(Track::Video, timestamp_us, now_us);
            }
            // ウィンドウを閉じた場合も、終了ログのためにここまでの実績を反映してから抜ける
            if !matches!(poll_result, Ok(true)) {
                tracing::info!("Player window closed");
                break;
            }
            if observation.stalled {
                // raw_player 2026.3 は映像のみ経路の停止から自力で復帰するが、それより前の
                // プレイヤーでも描画を再開できるよう、ここでもキューと時計の基準を捨てる
                let enqueued_since_render = video_accounting.enqueued_since_render();
                p.drain_video();
                video_accounting.note_recovered(
                    stats.total_frames_rendered,
                    stats.dropped_frames,
                    now_mono_us,
                );
                tracing::warn!(
                    "Video playback stalled: no frame rendered for {}ms with {} frames enqueued; drained the video queue to recover (recoveries={})",
                    observation.stalled_for_us / 1_000,
                    enqueued_since_render,
                    video_accounting.recoveries(),
                );
            }
        }

        // 描画の実績を 1 秒に 1 回だけ出す。投入数と描画数・破棄数を区別して、
        // プレイヤーが詰まっていないかを読めるようにする
        if video_player.is_some()
            && last_video_log.elapsed().as_millis() >= VIDEO_PLAYER_LOG_INTERVAL_MS
        {
            last_video_log = std::time::Instant::now();
            tracing::info!(
                "Video player: enqueued={} rendered={} dropped={} (queue_full={} late={} resync={}) queue={} drain_recoveries={} player_resyncs={}",
                video_enqueued_count,
                video_accounting.rendered_total(),
                video_accounting.dropped_total(),
                video_player_dropped_queue_full,
                video_player_dropped_late,
                video_player_dropped_resync,
                video_queue_size,
                video_accounting.recoveries(),
                video_player_resyncs,
            );
        }

        match video_rx.try_recv() {
            Ok(frame) => {
                // プレイヤーが引き取った分だけ、受信側が数える待ちフレーム数を減らす
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
                    let player = raw_player::video_player::VideoPlayer::new(
                        frame.width,
                        frame.height,
                        &player_window_title(namespace),
                    )?;
                    // 音声の積み込みと映像の表示を同じスレッドで行うため、表示の垂直同期
                    // (vsync) を待つとその間だけ音声の積み込みが止まり、出力が枯れる。
                    // vsync を無効にして、表示を待たずに次のループへ進めるようにする
                    if let Err(e) = player.set_vsync(0) {
                        tracing::warn!("Failed to disable video vsync: {e}");
                    }
                    player.play()?;
                    // プレイヤーを作り直すと stats が 0 に戻るため、会計の基準値と
                    // 対応付けの FIFO をこの時点でリセットする
                    let stats = player.stats();
                    video_accounting.reset_for_player(
                        player.max_video_queue_size(),
                        stats.total_frames_rendered,
                        stats.dropped_frames,
                        start_time.elapsed().as_micros() as u64,
                    );
                    video_player = Some(player);
                }

                if let Some(ref p) = video_player {
                    // 表示時刻を過ぎたフレームを渡すため、PTS は再生側のいまの時刻にする
                    let pts_us = start_time.elapsed().as_micros() as i64;
                    match p.enqueue_video_i420(
                        &frame.y,
                        &frame.u,
                        &frame.v,
                        frame.width,
                        frame.height,
                        pts_us,
                    ) {
                        Ok(()) => {
                            // 受け付けたフレームだけを会計へ積む。失敗したフレームは
                            // プレイヤーのキューに残らないため、対応付けに含めない
                            video_enqueued_count += 1;
                            video_accounting.record_enqueue(frame.timestamp_us);
                        }
                        Err(e) => tracing::warn!("Failed to enqueue video frame: {e}"),
                    }
                }
            }
        }

        // 出力が枯れていないかを、音声を処理する前に確かめる。スケジューラが並べた最後の
        // 音の終わりより今が後なら、その間は既に無音が鳴ってしまっている (事後には埋め
        // られない)。枯れている間は毎ループ note_underrun を呼び、最後に並べた音の終わり
        // を今へ進めて、枯れた区間を次の音の隙間として二重に材料化させない。枯れていた
        // 長さは、音を積み直した時点 ([`AudioPlayoutAssembly::commit`]) で 1 回だけ数える
        if audio_output_enabled && audio_state.started {
            let now_us = wall_clock_us();
            if let Some(underrun_us) = underrun_us(audio_state.scheduler.last_end_us(), now_us) {
                audio_state.note_underrun(underrun_us, now_us);
            }
        }

        // 音声は毎ループ溜まっている分をすべて取り出す。1 回に 1 件しか取り出さないと、
        // バーストで届いた音を積むのが遅れて出力が枯れる。時間軸への観測には復号の
        // 完了時刻を使う。player スレッドが取り出した時刻はループの揺れを含み、
        // 遅延の学習を乱すためである
        loop {
            match audio_rx.try_recv() {
                Ok(audio) => {
                    audio_chunk_count += 1;
                    if let Some(audio_player) = audio_player.as_ref() {
                        let now_us = wall_clock_us();
                        timeline.observe(Track::Audio, audio.decoded_us, audio.pts_us);
                        play_decoded_audio(
                            audio_player,
                            &mut audio_state,
                            &mut timeline,
                            &audio,
                            now_us,
                        );
                    }
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
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // 保持キューは無く、届いたフレームはすべて再生機器へ積み終えている。
                    // 末尾の音を捨てないための吐き出しは要らないため、切断を知らせるだけに
                    // する
                    if !audio_disconnected {
                        tracing::info!("Audio channel disconnected");
                        audio_disconnected = true;
                    }
                    break;
                }
            }
        }

        // 積んだ音声を再生機器へ渡す。最初の音の開始時刻に達するまでは play を呼ばず、
        // それまでのフレームを先行バッファとして内部キューに溜める
        if let Some(audio_player) = audio_player.as_ref() {
            let now_us = wall_clock_us();
            audio_state.try_start_playback(audio_player, now_us);
            // いま鳴っているサンプルの PTS を実績として記録する。再生を開始するまでは
            // まだ鳴っていないため、実績にしない
            if audio_state.started {
                record_sounding_audio(audio_player, &mut audio_state, &mut timeline);
                // 再生中のバッファ量の最小値を残す。0 に近づいた回数と深さをログで読む
                let buffer_ms = audio_player.stats().audio_buffer_ms;
                player_buffer_min_ms =
                    Some(player_buffer_min_ms.map_or(buffer_ms, |min| min.min(buffer_ms)));
            }
            if let Err(e) = audio_player.process() {
                tracing::warn!("Failed to process audio queue: {e}");
            }
            if last_audio_log.elapsed().as_millis() >= AUDIO_BUFFER_LOG_INTERVAL_MS {
                last_audio_log = std::time::Instant::now();
                // player_buffer は再生機器へ積んだまま鳴っていない長さである。これが 0 に
                // 近づくと音が途切れるため、目標遅延が足りているかをここで確認できる。
                // player_buffer_min は前回のログ以降の最小値であり、一瞬の枯れも読める。
                // concealed と compressed は実際に適用できた長さの合計であり、skew_us は
                // 実績から求めた A/V のずれ (映像が音声より遅れていれば正) である。
                // 末尾の audio_state.log_fields は、鳴るはずの時刻・到着から鳴り始めるまで・
                // 予定に対する余裕 (p50 / p95)・理由別の捨て・隙間とアンダーランの計器・
                // 閉ループが決めた目標遅延である
                let skew_us = timeline
                    .skew_us()
                    .map_or_else(|| "none".to_string(), |skew_us| skew_us.to_string());
                let player_buffer_min = player_buffer_min_ms
                    .take()
                    .map_or_else(|| "none".to_string(), |min| format!("{min:.0}ms"));
                tracing::info!(
                    "Audio jitter buffer: target_delay={}ms player_buffer={:.0}ms player_buffer_min={} dropped={} concealed={}ms concealments={} compressed={}ms skew_us={} {}",
                    timeline.presentation_delay_us(Track::Audio).unwrap_or(0) / 1000,
                    audio_player.stats().audio_buffer_ms,
                    player_buffer_min,
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

    // 最後の観測以降に描画・破棄された分を会計へ取り込み、終了ログを実測値にする。
    // ここで記録する描画はループを抜ける直前に実際に表示されたフレームである
    if let Some(ref p) = video_player {
        let stats = p.stats();
        video_queue_size = stats.video_queue_size;
        video_player_dropped_queue_full = stats.dropped_frames_queue_full;
        video_player_dropped_late = stats.dropped_frames_late;
        video_player_dropped_resync = stats.dropped_frames_resync;
        video_player_resyncs = stats.resync_count;
        let observation = video_accounting.observe(
            stats.total_frames_rendered,
            stats.dropped_frames,
            start_time.elapsed().as_micros() as u64,
        );
        let now_us = wall_clock_us();
        for timestamp_us in observation.rendered_timestamps {
            timeline.record_presentation(Track::Video, timestamp_us, now_us);
        }
    }

    // 投入数と描画数・破棄数を区別する。描画が止まったまま終わった場合も、
    // 投入だけが進んだことがこの行で分かる。破棄の内訳と復帰回数も出す
    tracing::info!(
        "Player stopped after enqueued={} rendered={} dropped={} (queue_full={} late={} resync={}) video frames / {} audio chunks (queue={} drain_recoveries={} player_resyncs={})",
        video_enqueued_count,
        video_accounting.rendered_total(),
        video_accounting.dropped_total(),
        video_player_dropped_queue_full,
        video_player_dropped_late,
        video_player_dropped_resync,
        audio_chunk_count,
        video_queue_size,
        video_accounting.recoveries(),
        video_player_resyncs,
    );
    // 音声の再生を止める。予約したまま鳴り始めなかった音はここで切り捨てられるため、
    // その分を計器へ記録し、最後の値 (理由別の捨てと閉ループの目標) をログに出す
    if audio_output_enabled {
        let stopped_us = wall_clock_us();
        // 枯れたまま終わった場合も、その長さを計器へ残す
        audio_state.end_underrun(stopped_us);
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

/// 音声の再生の組み立て (時間軸・スケジューラ・計器・閉ループを 1 か所で扱う)
///
/// 1 つの音について、目標の開始時刻の決定 ([`AudioPlayoutAssembly::arrange`])、実際に
/// 適用した欠落の補間と詰めの反映 ([`AudioPlayoutAssembly::commit`])、計器への記録、
/// 閉ループへの観測の引き渡しをここへ集める。再生機器へ積む順序 (隙間の音、今回の音)
/// だけは [`play_decoded_audio`] が受け持つ。
///
/// 時間軸への到着の記録は受信側 ([`run_raw_player`]) が済ませているため、ここでは行わ
/// ない。時間軸から読むのは目標の開始時刻と遅れだけである。
///
/// 再生機器へ積むための状態 (再生を開始したか、積んだ音の位置の対応、直前に鳴らした
/// 波形) も、音声の再生の経路で 1 つに持ち回るためここに置く。
///
/// 再生を止めるときは [`AudioPlayoutAssembly::record_stopped`] を呼ぶ。予約したまま
/// 鳴り始めなかった音は、ここで数えないとどの統計にも現れない。
struct AudioPlayoutAssembly {
    /// 再生を開始したか
    started: bool,
    /// 最初に積んだ音の開始時刻 (マイクロ秒)。ここに達したら `play()` を呼ぶ
    playback_start_at_us: Option<i64>,
    /// 再生の開始に失敗したことを既にログに出したか
    ///
    /// 開始できるまで毎ループ試すため、失敗が続くときにログを溢れさせないようにする
    play_failure_logged: bool,
    /// 鳴らす時刻・詰め・隙間を決める
    scheduler: AudioPlayoutScheduler,
    /// 鳴るはずの時刻・到着・鳴り始めと、理由別の捨てを記録する計器
    stats: AudioPlayoutTimingStats,
    /// 直前に鳴らした音の波形 (チャンネルごと)。欠落した隙間はこの末尾を繰り返して
    /// 埋める
    last_played: Option<Vec<Vec<f32>>>,
    /// 再生機器へ積んだ音の位置の対応
    position: AudioPlayoutPosition,
    /// 直前に積んだ音の媒体終端の PTS (マイクロ秒)。次の隙間の PTS に使う
    last_media_end_pts_us: Option<i64>,
    /// 枯れ始めた時刻 (マイクロ秒)。枯れていないときは None
    underrun_start_us: Option<i64>,
    /// 閉ループへまだ渡していない、枯れていた長さ (マイクロ秒)
    ///
    /// 枯れは 1 回の観測でまとめて閉ループへ渡す。目標を動かすのは毎秒 1 回であるため、
    /// 枯れ明けから次の観測までのぶんを足し合わせて渡す。渡したら 0 に戻す
    unfed_underrun_us: i64,
    /// 直前に観測へ渡した、累積で詰めた長さ (マイクロ秒)。差を取って圧縮の速さを求める
    last_compressed_us: i64,
}

impl AudioPlayoutAssembly {
    /// 何も鳴らしていない状態で作る
    fn new() -> Self {
        Self {
            started: false,
            playback_start_at_us: None,
            play_failure_logged: false,
            scheduler: AudioPlayoutScheduler::new(),
            stats: AudioPlayoutTimingStats::new(),
            last_played: None,
            position: AudioPlayoutPosition::new(),
            last_media_end_pts_us: None,
            underrun_start_us: None,
            unfed_underrun_us: 0,
            last_compressed_us: 0,
        }
    }

    /// 1 つの音を鳴らす時刻へ予約する
    ///
    /// 時間軸から目標の開始時刻・揺らぎの遅れ・到着基準の遅れ・表示の遅れを取り、
    /// スケジューラへ渡す。鳴らさないと決まったときは、その理由 (並べすぎ) と長さを
    /// 計器へ記録し、閉ループへ観測を渡して `None` を返す。
    ///
    /// 鳴らす組み立てへ渡す時点で、時間圧縮と補間を使えるサンプルレートかも確かめる。
    /// 使えないレートの音は計器へ数える (鳴らし方は変えない)。
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
        // 4 kHz へ間引くフィルタの表に無いサンプルレート (44.1 kHz の AAC など) では、ピッチの
        // 周期を求められないため時間圧縮も補間も働かない。落ちずに「操作しない」を返して区別
        // できないので、計器に数えて気付けるようにする。数えるだけで鳴らし方は変えない
        if !stretch::is_time_stretch_available(audio.sample_rate) {
            self.stats.record_stretch_unavailable();
        }
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
        // 到着基準で鳴らすときの遅れは、学習した遅れを [50 ms, 100 ms] に切った値にする。
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
            // 目標が決まっているときだけ目標を守る。無いときと、目標を使わない指定の
            // ときは到着基準で並べる
            enforce_target: enforce_target && target_start_us.is_some(),
            delay_us,
            arrival_delay_us,
            presentation_delay_us,
        });
        let (start_at_us, basis, compress_us, gap_us, conceal_us) = match decision {
            AudioPlayoutDecision::Play {
                start_at_us,
                basis,
                compress_us,
                // 隙間は前の音の終わりから今回の開始までの全長であり、切り捨てない。
                // 補間してよい長さは conceal_us として別に返る
                gap_us,
                conceal_us,
            } => (start_at_us, basis, compress_us, gap_us, conceal_us),
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
            start_at_us,
            duration_us,
            target_start_us,
            arrival_us,
            compress_us,
            gap_us,
            conceal_us,
        })
    }

    /// 実際に積めた補間の長さを返す (呼び出し側が隙間を埋めた後に呼ぶ)
    fn confirm_concealment(&mut self, applied_us: i64) {
        self.scheduler.confirm_concealment(applied_us);
    }

    /// 予約した音へ実際に適用した詰めと隙間を、スケジューラ・計器・閉ループへ反映する
    ///
    /// 計器へ渡す値は `confirm_stretch` の後に `scheduler.last_play()` から読む。要求した
    /// 詰める長さと実際に詰めた長さは一致せず、実際に鳴る長さを記録する必要があるためで
    /// ある。`gap_us` は材料化した隙間の全長であり、補間できた長さ (concealed_us) ではなく
    /// 無音で埋めた分も含む。`now_us` は今の時刻 (受信側の壁時計) であり、閉ループへ渡す
    /// 観測の時刻になる。
    fn commit(
        &mut self,
        timeline: &mut PlayoutTimeline,
        applied: AppliedAudioPlayout,
        gap_us: i64,
        now_us: i64,
    ) {
        self.scheduler.confirm_stretch(applied.compressed_us);
        // 鳴らすと決めた音を計器へ渡す。詰めた分を引いた後に読むことで、実際に鳴る長さが
        // 記録される
        if let Some(play) = self.scheduler.last_play() {
            self.stats.record_play(play);
        }
        // 隙間は全長で数える。補間できなかった分も無音で材料化しており、聴感上の欠落と
        // しては同じ長さだからである
        if gap_us > 0 {
            self.stats.record_gap(gap_us);
        }
        // 音声を積み直したため、枯れていた区間はここで終わる
        self.end_underrun(now_us);
        self.feed_feedback(timeline, now_us);
        // 直前に鳴らした音として保持する。次の隙間はこの音の末尾を繰り返して埋める
        self.last_played = Some(applied.channels);
    }

    /// 最初の音の開始時刻に達していれば再生を開始する
    ///
    /// 開始前でも PCM は積める (`raw_player` は再生を開始するまで内部キューに溜める) ため、
    /// 開始までのフレームが先行バッファとして蓄積される。開始に失敗したときは開始済みに
    /// せず、次のループでやり直す。
    fn try_start_playback(
        &mut self,
        audio_player: &raw_player::audio_player::AudioPlayer,
        now_us: i64,
    ) {
        if self.started || !should_start_playback(self.playback_start_at_us, now_us) {
            return;
        }
        match audio_player.play() {
            Ok(()) => {
                self.started = true;
                tracing::info!("Audio playback started");
            }
            Err(e) => {
                // 開始できるまで次のループでも試す。失敗が続くたびに警告を出すとログが
                // 溢れるため、最初の失敗だけ知らせる
                if !self.play_failure_logged {
                    self.play_failure_logged = true;
                    tracing::warn!("Failed to start audio playback: {e}");
                }
            }
        }
    }

    /// 出力が枯れていることをスケジューラへ伝える
    ///
    /// 枯れている間は毎ループ呼ぶ。`note_underrun` は最後に並べた音の終わりを今へ進める
    /// ため、枯れた区間が次の音の隙間として二重に材料化されない。計器へは枯れが明けた
    /// ときに、枯れていた長さを 1 回だけ渡す ([`AudioPlayoutAssembly::end_underrun`])。
    fn note_underrun(&mut self, underrun_us: i64, now_us: i64) {
        if self.underrun_start_us.is_none() {
            // 枯れ始めた時刻は、いまから枯れている長さだけ戻った時刻 (直前の音の終わり)
            self.underrun_start_us = Some(now_us.saturating_sub(underrun_us));
        }
        self.scheduler.note_underrun(now_us);
    }

    /// 枯れていた区間を計器へ記録する (枯れた長さを 1 回だけ数える)
    ///
    /// 記録した長さは、閉ループへ渡すまで `unfed_underrun_us` に積む。閉ループは枯れを
    /// 目標遅延の増加の根拠にする
    /// ([`crate::playout::timing::AudioDelayFeedbackObservation::underrun_ms`])。
    fn end_underrun(&mut self, now_us: i64) {
        let Some(start_us) = self.underrun_start_us.take() else {
            return;
        };
        let underrun_us = now_us.saturating_sub(start_us);
        self.stats.record_underrun(underrun_us);
        self.unfed_underrun_us = self.unfed_underrun_us.saturating_add(underrun_us);
    }

    /// 閉ループへまだ渡していない枯れの長さをミリ秒で取り出す
    ///
    /// 取り出した分は 0 に戻す。枯れていないときは 0 を返す。ミリ秒未満は切り捨てる
    /// (検出の許容が 1 ms であるため、枯れとして数えた長さは 1 ms 以上である)。
    fn take_unfed_underrun_ms(&mut self) -> i64 {
        let underrun_ms = self.unfed_underrun_us / 1_000;
        self.unfed_underrun_us = 0;
        underrun_ms
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
    /// 鳴らした結果 (予定をどれだけ過ぎて鳴ったか、並べすぎで捨てた量、詰めた長さ) から目標
    /// 遅延を決め直す。渡した観測は [`PlayoutTimeline::observe_audio_playout`] がその場で音声の
    /// 表示の遅れへ反映するため、次の音から新しい目標で並ぶ。
    ///
    /// 圧縮 (時間圧縮) は累積で数えられているため、前回の観測からの増分にして渡す。閉ループ
    /// はこれが続くときだけ目標遅延の上限を 160 ms へ広げる。購読のやり直しで累積が 0 に
    /// 戻っても負の差を作らないよう、0 で止める。
    ///
    /// アンダーランも前回の観測からの増分にして渡す。アンダーランは事後には埋められない
    /// ため、閉ループはこれを根拠に目標遅延を上げる。
    fn feed_feedback(&mut self, timeline: &mut PlayoutTimeline, now_us: i64) {
        let mut observation = self.stats.audio_delay_feedback(now_us);
        let compressed_us = self.scheduler.compressed_us();
        observation.compression_us = compressed_us.saturating_sub(self.last_compressed_us).max(0);
        self.last_compressed_us = compressed_us;
        observation.underrun_ms = self.take_unfed_underrun_ms();
        timeline.observe_audio_playout(observation);
    }

    /// 計器の観測値を求める
    fn snapshot(&mut self, now_us: i64) -> AudioPlayoutTimingSnapshot {
        self.stats.snapshot(now_us)
    }

    /// 計器と閉ループの値をログ用の文字列にする
    ///
    /// 材料化した隙間と検出したアンダーランの回数・合計・最大、鳴るはずの時刻 (絶対値)、
    /// 到着から鳴り始めるまでと予定に対する余裕の分布 (p50 / p95)、理由別の捨ての件数と
    /// 長さ、時間圧縮と補間を使えないサンプルレートで受け取った音の数、いま使っている
    /// 目標遅延とその理由、いま適用している上限と、直近に観測した圧縮の速さを出す。
    fn log_fields(&mut self, timeline: &PlayoutTimeline, now_us: i64) -> String {
        let snapshot = self.snapshot(now_us);
        let feedback = timeline.delay_breakdown().audio_delay_feedback;
        format!(
            "gaps={} gap_ms={} gap_max_ms={} underruns={} underrun_ms={} underrun_max_ms={} playout last_target_us={} last_arrival_us={} last_start_us={} last_slack={}ms last_start_delay={}ms last_lateness={}ms slack_p50_p95={}ms start_delay_p50_p95={}ms lateness_p50_p95={}ms played={} played_ms={} arrival_planned={} unplanned={} missed={} missed_ms={} missed_backlog={} missed_catch_up={} missed_error={} missed_stopped={} stretch_unavailable={} feedback_target={}ms feedback_reason={:?} feedback_change={}ms feedback_adjustments={} feedback_ceiling={}ms feedback_compression={}ms_per_second",
            snapshot.gaps,
            snapshot.gap_total_us / 1000,
            snapshot.gap_max_us / 1000,
            snapshot.underruns,
            snapshot.underrun_total_us / 1000,
            snapshot.underrun_max_us / 1000,
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
            snapshot.stretch_unavailable_frames,
            feedback.target_us / 1000,
            feedback.reason,
            feedback.last_change_us / 1000,
            feedback.adjustments,
            feedback.applied_ceiling_us / 1000,
            feedback.compression_us_per_second / 1000,
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
        audio_player: &raw_player::audio_player::AudioPlayer,
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
    /// 鳴り始める時刻 (マイクロ秒)。最初の 1 音では再生開始の判定にも使う
    start_at_us: i64,
    /// 音の長さ (マイクロ秒)
    duration_us: i64,
    /// 目標の開始時刻。無いこともある
    target_start_us: Option<i64>,
    /// 到着の基準 (到着した音がまだ鳴っていない位置)
    arrival_us: i64,
    /// 波形の周期で詰める長さ (マイクロ秒)。0 なら詰めない
    compress_us: i64,
    /// 前の音の終わりから今回の開始までの隙間の全長 (マイクロ秒)。0 なら隙間は無い
    gap_us: i64,
    /// 隙間のうち補間してよい長さ (マイクロ秒)。必ず `gap_us` 以下
    conceal_us: i64,
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
/// [`AudioPlayoutAssembly`] が行う。ここは再生機器へ積む順序 (隙間の音、今回の音) を
/// 受け持つ。`now_us` は今の時刻 (受信側の壁時計)。詰めは波形の周期 1 つ分を削り、
/// 隙間は [`gap::materialize`] で全長を材料化する (補間できなかった分は無音で埋まる)。
/// 実際に適用した長さは `confirm_concealment` と `confirm_stretch` で返す。
///
/// スケジューラが鳴らさないと決めた音は積まずに捨て、捨てた数と理由をログに出す
/// (鳴らせない音を積むと、その分だけ音が遅れたままになる)。
fn play_decoded_audio(
    audio_player: &raw_player::audio_player::AudioPlayer,
    state: &mut AudioPlayoutAssembly,
    timeline: &mut PlayoutTimeline,
    audio: &DecodedAudioFrame,
    now_us: i64,
) {
    let channels = usize::from(audio.channels);
    let sample_rate = audio.sample_rate;
    // 到着の基準は、音声出力が実際に鳴っている位置から求める。`now_us` は既に出力へ積んだ
    // 分だけ先に進んでいるため、到着基準の遅れはこの位置から数える
    let arrival_us = state.sounding_position_us(audio_player, timeline, now_us);
    let Some(arrangement) = state.arrange(timeline, audio, now_us, arrival_us, true) else {
        return;
    };

    // 隙間の材料化と詰めを音声へ適用する。返った隙間の音は今回の音の直前へ積むため、
    // 再生機器のキューでは前の音の直後、今回の音の直前になる (キューは積んだ順に鳴る)
    let applied = apply_audio_playout(
        audio,
        state.last_played.as_deref(),
        arrangement.gap_us,
        arrangement.conceal_us,
        arrangement.compress_us,
    );

    // 隙間の音を積む。PTS は直前の音の媒体終端であり、まだ一度も音を積んでいなければ
    // 今回の音の PTS から隙間の全長だけ戻した値にする
    let gap_enqueued = if applied.gap_channels.is_empty() {
        false
    } else {
        let pts_us = gap_start_pts_us(
            state.last_media_end_pts_us,
            audio.pts_us,
            arrangement.gap_us,
        );
        // 累積サンプル数は enqueue の前に読む。enqueue の後に読むと、積んだ隙間の分だけ
        // 後ろへ進んだ値になり、鳴り始める位置の対応が壊れる
        let start_samples = audio_player.stats().total_samples_enqueued;
        let frames = applied.gap_channels.len() / channels.max(1);
        if enqueue_pcm(
            audio_player,
            &applied.gap_channels,
            pts_us,
            sample_rate,
            audio.channels,
        ) {
            state.position.record(start_samples, pts_us, frames as i64);
            true
        } else {
            false
        }
    };
    // 積めた長さだけを補間したものとして返す。要求した長さを超える分は confirm が切る
    state.confirm_concealment(if gap_enqueued {
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
    // 最初に積んだ音の開始時刻を控える。メインループはこの時刻に達したら play() を呼ぶ。
    // それまでのフレームは再生機器の内部キューに先行バッファとして溜まる
    if state.playback_start_at_us.is_none() {
        state.playback_start_at_us = Some(arrangement.start_at_us);
    }
    // 次の隙間の PTS に使う、今回の音の媒体終端。詰めた分を引かないのは、この音が表現
    // している媒体上の範囲の終端だからである
    state.last_media_end_pts_us = Some(audio.pts_us.saturating_add(arrangement.duration_us));
    // 適用した詰めと隙間をスケジューラへ返し、鳴らした結果を計器と閉ループへ反映する。
    // 隙間の計器は実際に積めた長さで記録する (積めなかった分を材料化したと数えない)
    let materialized_gap_us = if gap_enqueued { arrangement.gap_us } else { 0 };
    state.commit(timeline, applied, materialized_gap_us, now_us);
}

/// いま鳴っているサンプルの PTS を実績として時間軸へ記録する
///
/// `raw_player` は積んだ順にサンプルを鳴らすため、鳴らし終えたサンプル数と「積んだ時点の
/// 累積サンプル数」の対応から、いま鳴っているサンプルの PTS が求まる。予定時刻ではなく
/// 実際の再生位置を使うことで、`skew_us` が計画ではなく実測になる。
fn record_sounding_audio(
    audio_player: &raw_player::audio_player::AudioPlayer,
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

/// スケジューラが決めた隙間の材料化と詰めを音声へ適用した結果
struct AppliedAudioPlayout {
    /// 詰めた後の今回の音 (チャンネルごと、f32)
    channels: Vec<Vec<f32>>,
    /// 材料化した隙間の音 (f32 interleaved)。隙間が無いときは空
    gap_channels: Vec<f32>,
    /// 補間できた長さ (マイクロ秒)。補間できなければ 0
    concealed_us: i64,
    /// 詰めた長さ (マイクロ秒)。詰めていないときは 0
    compressed_us: i64,
}

/// スケジューラが決めた隙間の材料化と詰めを音声へ適用する (再生機器には触れない)
///
/// まず `compress_us` まで波形の周期 1 つ分を削って今回の音を詰める。削れる長さは周期
/// (2.5 ms 以上 15 ms 以下) で決まり、要求された長さとは一致しないため、実際に削れた
/// 長さを `compressed_us` として返す。次に `gap_us` の隙間を [`gap::materialize`] で
/// 全長ぶん材料化する。補間できなかった分は無音で埋まり、隙間の末尾と今回の音の先頭は
/// フェードで繋がる。`concealed_us` は補間できた長さである。
///
/// 削ってよい長さは `compress_us` までとする。周期がこれを超える音では削らない
/// ([`stretch::compress`] が上限で判断する)。
fn apply_audio_playout(
    audio: &DecodedAudioFrame,
    last_played_audio: Option<&[Vec<f32>]>,
    gap_us: i64,
    conceal_us: i64,
    compress_us: i64,
) -> AppliedAudioPlayout {
    let channels = usize::from(audio.channels);
    let sample_rate = audio.sample_rate;
    // stretch は f32 のチャンネルごとの波形を扱う。S16 interleaved から直して分ける
    let mut channel_pcm = pcm_f32_to_channels(&pcm_i16_to_f32(&audio.pcm), channels);
    let mut compressed_us = 0;
    let mut used_frames = channel_pcm.first().map_or(0, Vec::len);
    if compress_us > 0 {
        let mut channel_slices: Vec<&mut [f32]> = channel_pcm
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        // 削ってよい長さの上限は、目標に対して遅れている分までにする。周期がこれを超える
        // 音では削らない。削ると行き過ぎて「早すぎる」側へ振れ、次の音でまた遅れて詰める
        // 往復になり、往復のたびに周期が 1 つ消える
        let removed = stretch::compress(
            &mut channel_slices,
            sample_rate,
            us_to_samples(compress_us, sample_rate),
        );
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

    // 隙間を材料化する。返る音は要求した全長 (gap_us) であり、補間できなかった分は無音で
    // 埋まる。今回の音の先頭にはフェードインが掛かる
    let mut gap_channels = Vec::new();
    let mut concealed_us = 0;
    if gap_us > 0 && channels > 0 {
        let mut channel_slices: Vec<&mut [f32]> = channel_pcm
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let materialized = gap::materialize(
            last_played_audio,
            &mut channel_slices,
            sample_rate,
            gap_us,
            conceal_us,
        );
        concealed_us = materialized.concealed_us;
        let gap_frames = materialized.channels.first().map_or(0, Vec::len);
        gap_channels = pcm_channels_to_f32(&materialized.channels, gap_frames);
    }

    AppliedAudioPlayout {
        channels: channel_pcm,
        gap_channels,
        concealed_us,
        compressed_us,
    }
}

/// 隙間の音を積むときの PTS (マイクロ秒) を求める
///
/// 隙間は直前の音の直後を占めるため、直前の音の媒体終端を使う。まだ一度も音を積んで
/// いなければ、今回の音の PTS から隙間の全長だけ戻した値にする。
fn gap_start_pts_us(last_media_end_pts_us: Option<i64>, pts_us: i64, gap_us: i64) -> i64 {
    last_media_end_pts_us.unwrap_or_else(|| pts_us.saturating_sub(gap_us))
}

/// 出力が枯れた長さを求める (枯れていなければ None)
///
/// まだ 1 音も並べていない (`last_end_us` が None) ときと、最後に並べた音の終わりから
/// [`AUDIO_UNDERRUN_ALLOWANCE_US`] 以内のときは、枯れていないものとして None を返す。
fn underrun_us(last_end_us: Option<i64>, now_us: i64) -> Option<i64> {
    let last_end_us = last_end_us?;
    let underrun_us = now_us.checked_sub(last_end_us)?;
    (underrun_us >= AUDIO_UNDERRUN_ALLOWANCE_US).then_some(underrun_us)
}

/// 最初の音の開始時刻に達して、再生を開始してよければ true
fn should_start_playback(start_at_us: Option<i64>, now_us: i64) -> bool {
    start_at_us.is_some_and(|start_at_us| now_us >= start_at_us)
}

/// f32 interleaved の音声を再生機器の内部キューへ積む
///
/// 積むことができたときだけ true を返す。再生の開始 (`play`) はここでは呼ばない。最初の
/// 音の開始時刻まで積み続けることで、先行バッファが内部キューに蓄積される。
fn enqueue_pcm(
    audio_player: &raw_player::audio_player::AudioPlayer,
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
        raw_player::audio_format::AudioFormat::S16,
    ) {
        tracing::warn!("Failed to enqueue audio chunk: {e}");
        return false;
    }
    true
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

/// マイクロ秒に相当するサンプル数 (四捨五入)
///
/// 時間圧縮で削ってよい長さの上限を求めるときに使う (`samples_to_us` と逆向きの丸め)。
/// 0 以下とサンプルレートが 0 のときは 0 を返す。
fn us_to_samples(us: i64, sample_rate: u32) -> usize {
    if us <= 0 || sample_rate == 0 {
        return 0;
    }
    let scaled = us.saturating_mul(i64::from(sample_rate));
    usize::try_from(scaled.saturating_add(500_000) / 1_000_000).unwrap_or(usize::MAX)
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
        AUDIO_DELAY_FEEDBACK_MAX_TARGET_US, AUDIO_DELAY_FEEDBACK_START_US, AudioDelayFeedbackReason,
    };

    use super::*;

    /// 再生ウィンドウのタイトルに購読中の namespace が出ること
    ///
    /// タイトルに固定の名前を出すと、どの namespace を購読しているか分からなくなる。
    #[test]
    fn player_window_title_includes_namespace() {
        assert_eq!(
            player_window_title("moq-example"),
            "moq-example - MOQT Subscriber",
            "指定した namespace がそのままタイトルへ出ること"
        );
        assert_eq!(
            player_window_title("example.2ecom"),
            "example.2ecom - MOQT Subscriber",
            "エスケープを含む §8.8 表現でもそのままタイトルへ出ること"
        );
    }

    /// 描画差分 1 で FIFO の先頭の TIMESTAMP が記録対象として返ること
    #[test]
    fn video_accounting_returns_the_rendered_timestamp() {
        let mut accounting = VideoPlayoutAccounting::default();
        // プレイヤーを作った直後 (実績 0) を基準にする
        accounting.reset_for_player(5, 0, 0, 0);
        // 2 枚投入し、先頭の 1 枚が実際に描画された
        accounting.record_enqueue(Some(1_000));
        accounting.record_enqueue(Some(2_000));
        let observation = accounting.observe(1, 0, 10_000);
        assert_eq!(
            observation.rendered_timestamps,
            vec![1_000],
            "描画されたのは先頭のフレームであり、その TIMESTAMP が記録対象として返ること"
        );
        assert!(
            !observation.stalled,
            "描画が進んでいるため停止と判定しないこと"
        );
        assert_eq!(
            accounting.rendered_total(),
            1,
            "描画数の累計に観測した分を足すこと"
        );
    }

    /// 破棄差分で FIFO の先頭が取り除かれ、記録対象にならないこと
    #[test]
    fn video_accounting_removes_the_dropped_timestamp() {
        let mut accounting = VideoPlayoutAccounting::default();
        accounting.reset_for_player(5, 0, 0, 0);
        accounting.record_enqueue(Some(1_000));
        accounting.record_enqueue(Some(2_000));
        // 先頭の 1 枚が破棄された。破棄されたフレームは描画されない
        let observation = accounting.observe(0, 1, 10_000);
        assert!(
            observation.rendered_timestamps.is_empty(),
            "破棄だけでは記録対象が返らないこと"
        );
        // 次の観測で描画されたのは、破棄された先頭ではなく 2 枚目
        let observation = accounting.observe(1, 1, 20_000);
        assert_eq!(
            observation.rendered_timestamps,
            vec![2_000],
            "破棄されたフレームの次のフレームが記録対象になること"
        );
    }

    /// 投入時にキュー上限を超えて捨てた分を、破棄差分から二重に取り除かないこと
    #[test]
    fn video_accounting_subtracts_the_enqueue_drop() {
        let mut accounting = VideoPlayoutAccounting::default();
        // 上限 2 のキューに 3 枚投入すると、3 枚目の投入で先頭が捨てられる
        accounting.reset_for_player(2, 0, 0, 0);
        accounting.record_enqueue(Some(1_000));
        accounting.record_enqueue(Some(2_000));
        accounting.record_enqueue(Some(3_000));
        // プレイヤーは投入時に先頭を 1 枚捨てる。FIFO は既に取り除いているため、
        // ここでさらに取り除くと描画されたフレームの TIMESTAMP を取り違える
        let observation = accounting.observe(0, 1, 10_000);
        assert!(
            observation.rendered_timestamps.is_empty(),
            "破棄だけでは記録対象が返らないこと"
        );
        let observation = accounting.observe(1, 1, 20_000);
        assert_eq!(
            observation.rendered_timestamps,
            vec![2_000],
            "キューに残った先頭のフレームが記録対象になること"
        );
    }

    /// プレイヤーを作り直すと実績の基準と FIFO がリセットされること
    ///
    /// 解像度が変わってプレイヤーを作り直すと stats は 0 に戻る。基準を取り直さないと
    /// 描画数の差分が負になり、作り直す前のフレームが描画として記録されてしまう。
    #[test]
    fn video_accounting_resets_for_a_new_player() {
        let mut accounting = VideoPlayoutAccounting::default();
        accounting.reset_for_player(5, 0, 0, 0);
        accounting.record_enqueue(Some(1_000));
        accounting.record_enqueue(Some(2_000));
        // 解像度が変わってプレイヤーを作り直した。新しいプレイヤーの実績は 0 から始まる
        accounting.reset_for_player(5, 0, 0, 50_000);
        // 作り直す前のフレームは対応付けに残らない
        accounting.record_enqueue(Some(3_000));
        let observation = accounting.observe(1, 0, 60_000);
        assert_eq!(
            observation.rendered_timestamps,
            vec![3_000],
            "作り直した後に描画されたのは、作り直した後に投入したフレームであること"
        );
    }

    /// 描画が 1 秒未満しか止まっていなければ停止と判定しないこと
    #[test]
    fn video_accounting_does_not_detect_a_stall_before_the_timeout() {
        let mut accounting = VideoPlayoutAccounting::default();
        accounting.reset_for_player(5, 0, 0, 0);
        accounting.record_enqueue(Some(1_000));
        accounting.record_enqueue(Some(2_000));
        let observation = accounting.observe(0, 0, 999_999);
        assert!(
            !observation.stalled,
            "描画が止まっている時間が 1 秒未満なら停止と判定しないこと"
        );
        assert_eq!(
            observation.stalled_for_us, 0,
            "停止していなければ止まっていた時間を報告しないこと"
        );
    }

    /// 描画が 1 秒以上増えず、その間に 2 フレーム以上投入していれば停止と判定すること
    #[test]
    fn video_accounting_detects_a_stall_after_the_timeout() {
        let mut accounting = VideoPlayoutAccounting::default();
        accounting.reset_for_player(5, 0, 0, 0);
        accounting.record_enqueue(Some(1_000));
        accounting.record_enqueue(Some(2_000));
        let observation = accounting.observe(0, 0, 1_000_000);
        assert!(
            observation.stalled,
            "1 秒以上描画が進まず、その間に 2 枚投入していれば停止と判定すること"
        );
        assert_eq!(
            observation.stalled_for_us, 1_000_000,
            "停止していた時間を報告すること"
        );
    }

    /// 1 秒以上描画が増えていなくても、投入が無ければ停止と判定しないこと
    #[test]
    fn video_accounting_does_not_detect_a_stall_without_enqueues() {
        let mut accounting = VideoPlayoutAccounting::default();
        accounting.reset_for_player(5, 0, 0, 0);
        let observation = accounting.observe(0, 0, 2_000_000);
        assert!(
            !observation.stalled,
            "投入が無ければ描画の機会が来ていないだけかもしれないため、停止と判定しないこと"
        );
    }

    /// 復帰後にカウンタがリセットされ、復帰回数が増えること
    #[test]
    fn video_accounting_resets_after_recovery() {
        let mut accounting = VideoPlayoutAccounting::default();
        accounting.reset_for_player(5, 0, 0, 0);
        accounting.record_enqueue(Some(1_000));
        accounting.record_enqueue(Some(2_000));
        // drain_video の呼び出し直前の実績。drain_video は描画数・破棄数を戻さない
        let observation = accounting.observe(0, 0, 1_000_000);
        assert!(
            observation.stalled,
            "復帰の前提として、1 秒以上描画が進まず 2 枚投入していれば停止と判定されること"
        );
        // drain_video でキューと時計を捨てた直後の状態を記録する
        accounting.note_recovered(0, 0, 1_000_000);
        assert_eq!(accounting.recoveries(), 1, "復帰回数を数えること");
        // 復帰で投入からの経過がリセットされるため、投入が無ければ停止と判定しない
        let observation = accounting.observe(0, 0, 3_000_000);
        assert!(
            !observation.stalled,
            "復帰後に投入が無ければ、時間が経っても停止と判定しないこと"
        );
        // FIFO も空になっているため、復帰前のフレームが描画として記録されないこと
        accounting.record_enqueue(Some(3_000));
        let observation = accounting.observe(1, 0, 3_100_000);
        assert_eq!(
            observation.rendered_timestamps,
            vec![3_000],
            "復帰後に描画されたのは復帰後に投入したフレームであること"
        );
    }

    /// テスト用の周期のある音を作る
    ///
    /// 48 kHz のサンプルレートで `frequency_hz` が整数の周期になる (400 Hz なら 120
    /// サンプル) ため、時間圧縮と隙間の補間が末尾の周期をそのまま見つけられる。
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
            us_to_samples(20_000, 48_000),
            960,
            "20 ms は 48 kHz で 960 サンプルであること"
        );
        assert_eq!(
            us_to_samples(10, 48_000),
            0,
            "1 サンプルの半分未満は 0 サンプルへ切り捨てること"
        );
        assert_eq!(
            us_to_samples(11, 48_000),
            1,
            "1 サンプルの半分を超える長さは 1 サンプルへ四捨五入すること"
        );
        assert_eq!(us_to_samples(0, 48_000), 0, "0 は 0 であること");
        assert_eq!(us_to_samples(-1, 48_000), 0, "負の長さは 0 であること");
        assert_eq!(
            us_to_samples(1_000, 0),
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

    /// 直前の音の末尾を周期で繰り返して隙間を材料化すること
    ///
    /// 隙間は要求した全長ぶん材料化され、補間できた長さが concealed_us として返る。
    /// 隙間の先頭は直前の音の末尾へ連続し、今回の音は短くならない。
    #[test]
    fn apply_audio_playout_materializes_the_gap_from_the_previous_tail() {
        let sample_rate = 48_000;
        // 400 Hz は 48 kHz で 120 サンプル周期であり、末尾の周期がそのまま見つかる
        let period = 120;
        let previous = vec![periodic_audio(400.0, sample_rate, 4_800)];
        let audio = audio_frame(960, sample_rate);
        let applied = apply_audio_playout(&audio, Some(previous.as_slice()), 20_000, 20_000, 0);
        assert_eq!(
            applied.gap_channels.len(),
            960,
            "20 ms ぶんの隙間を材料化すること"
        );
        assert_eq!(
            applied.concealed_us, 20_000,
            "補間できた長さをマイクロ秒で返すこと"
        );
        assert_eq!(applied.channels[0].len(), 960, "今回の音は短くならないこと");
        // 直前の音の末尾と隙間の先頭が連続していること (周期ぶん離れたサンプルは同位相)
        let previous_tail = previous[0][4_800 - 1];
        assert!(
            (applied.gap_channels[0] - previous_tail).abs() < 0.05,
            "隙間の先頭が直前の音の末尾の続きであること gap={} tail={previous_tail}",
            applied.gap_channels[0]
        );
        // 隙間は末尾の周期を繰り返していること。先頭は直前の音の末尾へ繋ぐフェードの
        // ため、末尾は 0 へ落とすフェードのため、その範囲を除いて確かめる
        let fade_frames = 96;
        for index in fade_frames..applied.gap_channels.len() - period - fade_frames {
            let difference =
                (applied.gap_channels[index] - applied.gap_channels[index + period]).abs();
            assert!(
                difference < 0.05,
                "{index} 番目が周期ぶん後の音と一致すること difference={difference}"
            );
        }
    }

    /// 補間できない隙間も無音で全長ぶん材料化されること
    ///
    /// 隙間を空けたままにすると、後続の音がその分だけ早く鳴って段差になる。直前の音が
    /// 無い (最初の音の前) ときも、要求された全長を無音で埋める。
    #[test]
    fn apply_audio_playout_fills_an_unconcealable_gap_with_silence() {
        let audio = audio_frame(960, 48_000);
        let applied = apply_audio_playout(&audio, None, 10_000, 10_000, 0);
        assert_eq!(
            applied.gap_channels.len(),
            480,
            "補間できなくても 10 ms ぶんの隙間を材料化すること"
        );
        assert_eq!(
            applied.concealed_us, 0,
            "補間できなかったため補間した長さは 0 であること"
        );
        assert!(
            applied.gap_channels.iter().all(|sample| *sample == 0.0),
            "補間できない隙間は無音で埋めること"
        );
        // 隙間が無ければ材料化しない
        let applied = apply_audio_playout(&audio, None, 0, 0, 0);
        assert!(
            applied.gap_channels.is_empty(),
            "隙間が無いときは材料化しないこと"
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

    /// 隙間の音を積むときの PTS を求めること
    ///
    /// 直前の音を積んでいればその媒体終端を使う。まだ一度も積んでいなければ、今回の音の
    /// PTS から隙間の全長だけ戻した値にする。
    #[test]
    fn gap_start_pts_uses_the_previous_media_end() {
        assert_eq!(
            gap_start_pts_us(Some(1_020_000), 1_030_000, 10_000),
            1_020_000,
            "直前の音の媒体終端を使うこと"
        );
        assert_eq!(
            gap_start_pts_us(None, 1_030_000, 10_000),
            1_020_000,
            "直前の音が無ければ今回の音の PTS から隙間の全長だけ戻すこと"
        );
        assert_eq!(
            gap_start_pts_us(None, 10_000, 20_000),
            -10_000,
            "隙間が音の先頭より前になるときは負の PTS になること"
        );
    }

    /// 出力が枯れた長さの判定が境界で正しいこと
    ///
    /// まだ 1 音も並べていないとき (None) と、最後に並べた音の終わりより前 (負) は枯れて
    /// いない。許容ちょうどから枯れたものとして数える。
    #[test]
    fn underrun_us_checks_the_boundaries() {
        let allowance = AUDIO_UNDERRUN_ALLOWANCE_US;
        assert_eq!(
            underrun_us(None, 10_000_000),
            None,
            "まだ 1 音も並べていなければ枯れていないこと"
        );
        assert_eq!(
            underrun_us(Some(10_000_500), 10_000_500),
            None,
            "最後の音の終わりちょうどでは枯れていないこと"
        );
        assert_eq!(
            underrun_us(Some(10_001_000), 10_000_000),
            None,
            "最後の音の終わりより前 (負) は枯れていないこと"
        );
        assert_eq!(
            underrun_us(Some(10_000_000 - (allowance - 1)), 10_000_000),
            None,
            "許容未満は枯れていないこと"
        );
        assert_eq!(
            underrun_us(Some(10_000_000 - allowance), 10_000_000),
            Some(allowance),
            "許容ちょうどから枯れたものとして数えること"
        );
        assert_eq!(
            underrun_us(Some(9_950_000), 10_000_000),
            Some(50_000),
            "過ぎた長さを枯れた長さとして返すこと"
        );
    }

    /// 再生開始の判定が最初の音の開始時刻で切り替わること
    #[test]
    fn should_start_playback_waits_for_the_first_start_time() {
        assert!(
            !should_start_playback(None, 10_000_000),
            "まだ音を積んでいなければ開始しないこと"
        );
        assert!(
            !should_start_playback(Some(10_000_001), 10_000_000),
            "最初の音の開始時刻の前は開始しないこと"
        );
        assert!(
            should_start_playback(Some(10_000_000), 10_000_000),
            "最初の音の開始時刻ちょうどで開始すること"
        );
        assert!(
            should_start_playback(Some(9_999_999), 10_000_000),
            "最初の音の開始時刻を過ぎたら開始すること"
        );
    }

    /// 計器が隙間の全長とアンダーランの枯れた長さを数えること
    ///
    /// 隙間は補間した長さではなく材料化した全長で数える。アンダーランは枯れている間の
    /// 検出 (毎ループ) ではなく、枯れが明けたときに 1 回だけ数える。
    #[test]
    fn playout_assembly_counts_the_gaps_and_the_underruns() {
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        let now_us = 10_000_000;
        timeline.observe(Track::Audio, now_us, 0);
        let audio = audio_frame(960, 48_000);
        // 隙間の全長は 20 ms、補間できたのは 10 ms だけという組み立てを反映する
        assembly.commit(
            &mut timeline,
            AppliedAudioPlayout {
                channels: pcm_f32_to_channels(
                    &pcm_i16_to_f32(&audio.pcm),
                    usize::from(audio.channels),
                ),
                gap_channels: vec![0.0; 480],
                concealed_us: 10_000,
                compressed_us: 0,
            },
            20_000,
            now_us,
        );
        let snapshot = assembly.snapshot(now_us);
        assert_eq!(snapshot.gaps, 1, "材料化した隙間を数えること");
        assert_eq!(
            snapshot.gap_total_us, 20_000,
            "補間した長さではなく材料化した全長を合計すること"
        );
        assert_eq!(snapshot.gap_max_us, 20_000, "最長の隙間を記録すること");

        // 枯れている間は毎ループ検出されるが、明けたときに 1 回だけ数える
        assembly.note_underrun(5_000, 10_020_000);
        assembly.note_underrun(1_000, 10_021_000);
        let snapshot = assembly.snapshot(now_us);
        assert_eq!(snapshot.underruns, 0, "枯れている間はまだ計上しないこと");
        assembly.end_underrun(10_040_000);
        let snapshot = assembly.snapshot(now_us);
        assert_eq!(snapshot.underruns, 1, "枯れた区間を 1 回だけ数えること");
        assert_eq!(
            snapshot.underrun_total_us, 25_000,
            "枯れ始め (10_015_000) から明ける (10_040_000) までの長さを合計すること"
        );
        assert_eq!(
            snapshot.underrun_max_us, 25_000,
            "最長の枯れた区間を記録すること"
        );
        // 枯れた長さは閉ループへ 1 回だけ渡す
        assert_eq!(
            assembly.take_unfed_underrun_ms(),
            25,
            "枯れた長さをミリ秒で閉ループへ渡すこと"
        );
        assert_eq!(
            assembly.take_unfed_underrun_ms(),
            0,
            "同じ枯れを二重に渡さないこと"
        );
        // 続けて枯れが無いループで呼んでも二重に数えない
        assembly.end_underrun(10_041_000);
        assert_eq!(
            assembly.snapshot(now_us).underruns,
            1,
            "枯れていないときに呼んで二重に数えないこと"
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
            arrival_us: 0,
            decoded_us: 0,
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
        let previous = vec![periodic_audio(400.0, sample_rate, 960)];

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
                gap_us: 0,
                conceal_us: 0,
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
            conceal_us,
            ..
        } = second
        else {
            panic!("2 つ目は鳴ると決まること decision={second:?}");
        };
        assert_eq!(gap_us, 10_000, "1 つ目の終わりから 10 ms の隙間が空くこと");
        assert_eq!(conceal_us, 10_000, "隙間は補間してよい長さであること");
        let audio = audio_frame(960, sample_rate);
        let applied = apply_audio_playout(&audio, Some(previous.as_slice()), gap_us, conceal_us, 0);
        assert_eq!(applied.concealed_us, 10_000, "隙間を 10 ms 補間すること");
        assert_eq!(
            applied.gap_channels.len(),
            480,
            "材料化した隙間は 10 ms ぶんであること"
        );
        assert_eq!(applied.compressed_us, 0, "詰めは要求されていないこと");
        assert_eq!(applied.channels[0].len(), 960, "音は短くならないこと");
        scheduler.confirm_concealment(applied.concealed_us);
        assert_eq!(
            scheduler.concealed_us(),
            10_000,
            "実際に補間した長さを返すこと"
        );

        // 3 つ目は目標を過ぎて届く。直前の音の終わり (1_050_000) へ繋げて、遅れの分だけ詰める。
        // キューの余裕 = 1_050_000 − (1_030_000 + 10_000) = 10_000 の範囲で詰める
        let third = scheduler.schedule(AudioPlayoutInput {
            now_us: 1_030_000,
            arrival_us: 1_030_000,
            timestamp_us: 50_000,
            duration_us: 20_000,
            target_start_us: Some(1_020_000),
            enforce_target: true,
            delay_us: 80_000,
            arrival_delay_us: 80_000,
            presentation_delay_us: 80_000,
        });
        let AudioPlayoutDecision::Play {
            compress_us,
            gap_us,
            ..
        } = third
        else {
            panic!("3 つ目は鳴ると決まること decision={third:?}");
        };
        assert!(compress_us > 0, "目標を過ぎた分だけ詰めること");
        assert_eq!(gap_us, 0, "前の音の終わりへ繋がるため隙間は空かないこと");
        let applied =
            apply_audio_playout(&audio, Some(previous.as_slice()), gap_us, 0, compress_us);
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
            applied.gap_channels.is_empty(),
            "隙間が空かないときは材料化しないこと"
        );
        scheduler.confirm_stretch(applied.compressed_us);
        assert_eq!(
            scheduler.compressed_us(),
            applied.compressed_us,
            "実際に削れた長さを返すこと"
        );
    }

    /// 圧縮と隙間の材料化を同時に適用し、長さの合計が合うこと
    ///
    /// 詰めた今回の音と隙間の長さの合計が「元の音の長さ + 隙間 - 詰めた長さ」になること。
    /// どちらか片方だけを適用したときの長さの勘定がずれると、後続の音がその分だけ早く
    /// 鳴るか遅れて鳴る。
    #[test]
    fn playout_applies_the_compression_and_the_gap_together() {
        let sample_rate = 48_000;
        let previous = vec![periodic_audio(400.0, sample_rate, 4_800)];
        let audio = audio_frame(960, sample_rate);
        let applied =
            apply_audio_playout(&audio, Some(previous.as_slice()), 10_000, 10_000, 10_000);
        assert_eq!(
            applied.gap_channels.len(),
            480,
            "隙間は要求どおり 10 ms ぶん材料化すること"
        );
        assert_eq!(
            applied.concealed_us, 10_000,
            "周期のある音から隙間を補間すること"
        );
        assert!(
            applied.compressed_us > 0,
            "要求した分のうち周期 1 つぶんを削ること compressed_us={}",
            applied.compressed_us
        );
        assert!(
            applied.channels[0].len() < 960,
            "圧縮した分だけ今回の音が短くなること length={}",
            applied.channels[0].len()
        );
        let played_us = samples_to_us(applied.channels[0].len(), sample_rate)
            + samples_to_us(applied.gap_channels.len(), sample_rate);
        assert_eq!(
            played_us,
            20_000 + 10_000 - applied.compressed_us,
            "詰めた音と隙間の合計が、元の長さ + 隙間 - 詰めた長さになること              current={} gap={} compressed_us={}",
            applied.channels[0].len(),
            applied.gap_channels.len(),
            applied.compressed_us,
        );
    }

    /// テスト用に、隙間を材料化せずに詰めだけを適用した結果を作る
    ///
    /// 組み立てへ渡す値は、実際に音声へ適用した長さと同じ形にする。再生機器へ積む代わりに
    /// この値をそのまま `commit` へ渡す。
    fn applied_audio(audio: &DecodedAudioFrame, compressed_us: i64) -> AppliedAudioPlayout {
        AppliedAudioPlayout {
            channels: pcm_f32_to_channels(&pcm_i16_to_f32(&audio.pcm), usize::from(audio.channels)),
            gap_channels: Vec::new(),
            concealed_us: 0,
            compressed_us,
        }
    }

    /// テスト用に、時間軸が決めた目標の時刻へ 1 つ鳴らすと予約して計器へ記録する
    ///
    /// 再生機器を使わないため、詰めも隙間の材料化も無し (`compressed_us` と `gap_us` は 0)
    /// として反映する。
    fn commit_one_play(
        assembly: &mut AudioPlayoutAssembly,
        timeline: &mut PlayoutTimeline,
        audio: &DecodedAudioFrame,
        now_us: i64,
    ) {
        assembly
            .arrange(timeline, audio, now_us, now_us, true)
            .expect("目標の時刻に鳴ると決まること");
        assembly.commit(timeline, applied_audio(audio, 0), 0, now_us);
    }

    /// 組み立てが、鳴らすと決めた音を実際に詰めた長さで計器へ記録すること
    #[test]
    fn playout_assembly_records_the_play_in_the_stats() {
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        let sample_rate = 48_000;
        let arrival_us = 10_000_000;
        // 到着の記録は受信側 (run_raw_player) が行う
        timeline.observe(Track::Audio, arrival_us, 0);
        let audio = audio_frame(960, sample_rate);
        let target_us = timeline
            .present_us(Track::Audio, audio.pts_us)
            .expect("観測したので音声の目標が決まること");
        // 先に 1 つ鳴る音を並べておく。詰めるにはキューの余裕 (直前の音の終わり −
        // (今 + 最小の余裕)) が要り、直前の音が無いときは詰めないためである。この音は目標の
        // 時刻 (target_us) に間に合い、target_us + 20 ms まで鳴る。計器へは記録しない
        let queued = assembly
            .arrange(
                &mut timeline,
                &audio,
                target_us - 10_000,
                target_us - 10_000,
                true,
            )
            .expect("先に並べる音が鳴ると決まること");
        assert_eq!(queued.duration_us, 20_000, "先に並べる音の長さを返すこと");
        // 目標の時刻に届いた音を、その時刻に予約する。直前の音の終わり (target_us + 20 ms) へ
        // 繋がるため、今から鳴らせる最も早い時刻 (今 + 10 ms) ではなくその終わりへずれる。
        // ずれは 20 ms であり、音の長さの半分 (10 ms) とキューの余裕 (10 ms) の小さい方の
        // 10 ms だけ詰める要求が出る
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
        assembly.commit(&mut timeline, applied_audio(&audio, 5_000), 0, target_us);
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
            Some(20_000),
            "到着から鳴り始めるまでを記録すること"
        );
        assert_eq!(
            snapshot.last_lateness_us,
            Some(20_000),
            "予定からどれだけ過ぎて鳴るかを記録すること (不感帯を引く前の値)"
        );
    }

    /// 時間圧縮と補間を使えないサンプルレートの音だけを計器へ数えること
    ///
    /// 44.1 kHz (AAC など) は 4 kHz へ間引くフィルタの表に無く、時間圧縮も補間も働かない。
    /// 落ちずに「操作しない」を返すため、数えることで気付けるようにする。表にある 48 kHz の
    /// 音は数えない。数えるだけで鳴らし方 (予約が返るか) を変えない。
    #[test]
    fn playout_assembly_counts_only_the_stretch_unavailable_sample_rates() {
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        let arrival_us = 10_000_000;
        // 到着の記録は受信側 (run_raw_player) が行う
        timeline.observe(Track::Audio, arrival_us, 0);
        // 44.1 kHz の音 (20 ms で 882 サンプル) を受け取ると数える
        let audio = audio_frame(882, 44_100);
        let arrangement = assembly.arrange(&mut timeline, &audio, arrival_us, arrival_us, true);
        assert!(
            arrangement.is_some(),
            "数えるだけで鳴らし方 (予約の有無) を変えないこと"
        );
        assert_eq!(
            assembly.snapshot(arrival_us).stretch_unavailable_frames,
            1,
            "時間圧縮と補間を使えないサンプルレートの音を数えること"
        );
        // 表にある 48 kHz の音は数えない (鳴らし方も変わらない)
        let audio = audio_frame(960, 48_000);
        let arrangement = assembly.arrange(&mut timeline, &audio, arrival_us, arrival_us, true);
        assert!(
            arrangement.is_some(),
            "使えるサンプルレートの音も同じく鳴らすこと"
        );
        assert_eq!(
            assembly.snapshot(arrival_us).stretch_unavailable_frames,
            1,
            "時間圧縮と補間を使えるサンプルレートの音は数えないこと"
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
            Some(0),
            "予定を過ぎて鳴った量が閉ループへ届くこと (不感帯の 20 ms を引いた後)"
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
        // 捨てた量は閉ループへ渡り、目標遅延を増やす判断に使われる。ただし閉ループの内部の
        // 目標も、実際に適用する上限 (圧縮が無いときは 100 ms) で切る。初期値がその上限と
        // 同じであるため、この観測では値が動かない (適用されない値へ育てると、遅れが収まって
        // から減らし切るまでの時間だけ復帰が遅れる)
        let feedback = timeline.delay_breakdown().audio_delay_feedback;
        assert_eq!(
            feedback.reason,
            AudioDelayFeedbackReason::Backlog,
            "並べすぎの捨てが閉ループへ届くこと"
        );
        assert_eq!(
            feedback.target_us, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
            "内部の目標が適用する上限 (100 ms) に収まること"
        );
        assert_eq!(
            feedback.applied_ceiling_us, AUDIO_DELAY_FEEDBACK_MAX_TARGET_US,
            "圧縮が無いため上限を広げないこと"
        );
        assert_eq!(
            feedback.last_change_us, 0,
            "既に適用する上限にあるため増えないこと"
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
            "gaps=",
            "gap_ms=",
            "gap_max_ms=",
            "underruns=",
            "underrun_ms=",
            "underrun_max_ms=",
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
