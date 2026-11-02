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

use moq_sub::{
    DecodedAudioFrame, DecodedVideoFrame, TimestampKind, cli, error, pipeline, wall_clock_us,
};
use shiguredo_moqt::name::serialize_namespace;
use shiguredo_moqt::playout::buffer::{MAX_PRESENTATION_LAG_US, PlayoutBuffer};
use shiguredo_moqt::playout::gap;
use shiguredo_moqt::playout::scheduler::{
    AUDIO_PLAYOUT_DELAY_US, AUDIO_PLAYOUT_STRETCH_MIN_US, AudioPlayoutBasis, AudioPlayoutDecision,
    AudioPlayoutDropReason, AudioPlayoutInput, AudioPlayoutScheduler,
};
use shiguredo_moqt::playout::splice_fade;
use shiguredo_moqt::playout::stretch;
use shiguredo_moqt::playout::timeline::{PlayoutTimeline, Track, audio_arrival_delay_us};
use shiguredo_moqt::playout::timing::{
    AudioMissReason, AudioMissTotal, AudioPlayoutGap, AudioPlayoutMiss, AudioPlayoutTimingSnapshot,
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

/// 停止時の drain で待つ上限 (マイクロ秒)
///
/// 予約済みの音を鳴らし切ってから止めるための上限である。moqt-js の
/// `AUDIO_PLAYOUT_DRAIN_MAX_MS` と同じ値にする。
const AUDIO_DRAIN_MAX_US: i64 = 300_000;

/// 停止時の drain の待ち時間に足す余裕 (マイクロ秒)
///
/// 最後の予約の終わりに届くまでの時間へこの余裕を足し、復号や出力の揺れを吸収する。
/// moqt-js の `AUDIO_PLAYOUT_DRAIN_MARGIN_MS` と同じ値である。
const AUDIO_DRAIN_MARGIN_US: i64 = 100_000;

/// 停止時の drain で状態を見に行く間隔 (マイクロ秒)
///
/// moqt-js の `AUDIO_PLAYOUT_DRAIN_POLL_MS` と同じ値である。
const AUDIO_DRAIN_POLL_US: i64 = 25_000;

/// 隙間の材料化と時間伸長へ渡す、直前の実音の履歴の長さ (マイクロ秒)
///
/// moqt-js の `AudioRenderConcealer` は実音の末尾 30 ms をリングバッファに保ち、その末尾
/// 20 ms を解析の対象、30 ms を履歴として `expandAudio` へ渡す。1 フレーム (約 20 ms) しか
/// 渡さないと、20 ms で周期補間できなかったときの「30 ms の履歴で再試行する」分岐が走らず、
/// 線形予測の解析窓も 20 ms に縮む。
const AUDIO_HISTORY_US: i64 = 30_000;

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
    // 映像トラックを購読しているか。プレイヤーは音声だけの購読で到着基準の再生の遅れに
    // 下限を足し、目標の時刻を守らない。カタログを読むまでは映像があるものとして扱い
    // (true)、pipeline が購読の有無を入れる
    let video_subscribed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let video_subscribed_for_pipeline = std::sync::Arc::clone(&video_subscribed);

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
                video_subscribed_for_pipeline,
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
            video_subscribed,
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

/// 映像の表示 tick (ループ 1 回) の周期の初期値 (マイクロ秒)
///
/// メインループは 1 ms 眠るため、最初はその周期とみなす。moqt-js 0885 の
/// `DEFAULT_REFRESH_MS` (60 Hz) に対応する値であり、moqt-rs では表示の tick がループの
/// 1 回になる。
const VIDEO_DEFAULT_REFRESH_US: i64 = 1_000;

/// 映像の表示周期の推定に使う、直近の tick 間隔の数
///
/// moqt-js 0885 の `REFRESH_SAMPLE_COUNT` と同じ値である。
const VIDEO_REFRESH_SAMPLE_COUNT: usize = 16;

/// 映像の表示先 (raw_player) が、積んだフレームを表示するまでの tick 数
///
/// raw_player は積んだフレームを次の `poll_events` で描画するため 1 tick である。
/// moqt-js 0885 の `presentationLatencyTicks` の generator 経路 (1) に対応する。
const VIDEO_PRESENTATION_LATENCY_TICKS: i64 = 1;

/// 映像の描画会計と停止判定
///
/// raw_player は映像のキュー先頭から 1 フレームずつ描画し、キュー上限を超えた分と再生時計
/// から遅れすぎた分は先頭から捨てる。どのフレームが実際に描画されたかを知るために、投入した
/// フレームの TIMESTAMP と投入時刻を投入順に保持し、`stats()` が示す描画数・破棄数の差分を
/// 先頭から取り除く。描画を観測したフレームは、推定表示時刻ではなく観測した時刻で時間軸へ
/// 記録する。
///
/// `Instant` を内部に持たず、単調増加する時刻 (マイクロ秒) を引数で受け取るため、SDL も
/// プレイヤーも使わずに単体テストできる。
#[derive(Default)]
struct VideoPlayoutAccounting {
    /// プレイヤーの映像キューに残っていると考えられるフレーム (投入順)
    pending_frames: VecDeque<PendingVideoFrame>,
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

/// プレイヤーへ投入して、まだ描画・破棄を観測していないフレーム
struct PendingVideoFrame {
    /// 壁時計の TIMESTAMP
    ///
    /// TIMESTAMP の無いフレームも、描画・破棄の差分とキューの位置をずらさないために
    /// `None` として積む。時間軸へ記録するのは `Some` のフレームだけである。
    timestamp_us: Option<i64>,
    /// 投入した時刻 (単調増加マイクロ秒)。投入から描画を観測するまでの遅れを測る
    enqueued_us: u64,
}

/// [`VideoPlayoutAccounting::observe`] が返す、描画を観測したフレーム
struct RenderedVideoFrame {
    /// 壁時計の TIMESTAMP (時間軸へ記録するのは `Some` のフレームだけ)
    timestamp_us: Option<i64>,
    /// 投入から描画を観測するまでの遅れ (マイクロ秒)
    enqueue_delay_us: u64,
}

/// [`VideoPlayoutAccounting::observe`] が返す、1 回の観測の結果
struct VideoPlayoutObservation {
    /// この観測で描画されたフレーム (描画順)
    rendered: Vec<RenderedVideoFrame>,
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
        self.pending_frames.clear();
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
    ///
    /// `now_us` は単調増加する時刻 (マイクロ秒) で、投入から描画を観測するまでの遅れを
    /// 測る起点になる。
    fn record_enqueue(&mut self, timestamp_us: Option<i64>, now_us: u64) {
        self.enqueued_since_render += 1;
        // raw_player はキューが上限に達した状態で投入されると、先頭を 1 枚捨ててから
        // 積む。その破棄は次の `stats()` の差分に現れるため、ここでは FIFO の先頭も
        // 同じように取り除き、差分を処理するときに二重に取り除かないよう数を残す
        if self.max_queue > 0 && self.pending_frames.len() >= self.max_queue {
            self.pending_frames.pop_front();
            self.unobserved_enqueue_drops += 1;
        }
        self.pending_frames.push_back(PendingVideoFrame {
            timestamp_us,
            enqueued_us: now_us,
        });
    }

    /// `stats()` の差分から描画・破棄を会計へ反映する
    ///
    /// `current_rendered` / `current_dropped` は `stats()` が示す累計値である。
    /// `now_us` は単調増加する時刻 (マイクロ秒)。描画の差分があれば、その時刻を
    /// 「最後に描画を観測した時刻」として停止判定をやり直す。描画を観測したフレームは、
    /// 投入から描画までの遅れと一緒に返す。
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
            self.pending_frames.pop_front();
        }

        // 破棄はすべて描画より前 (投入時、または描画ループの先頭) に起きるため、
        // 描画された分は破棄を取り除いた後の先頭から順に対応する
        let mut rendered_frames = Vec::new();
        for _ in 0..rendered {
            if let Some(frame) = self.pending_frames.pop_front() {
                rendered_frames.push(RenderedVideoFrame {
                    timestamp_us: frame.timestamp_us,
                    enqueue_delay_us: now_us.saturating_sub(frame.enqueued_us),
                });
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
            rendered: rendered_frames,
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
        self.pending_frames.clear();
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

/// 映像の表示周期 (tick 間隔) を実測し、目標表示時刻・許容幅・遅れの上限を決める
///
/// moqt-js 0885 の `VideoPlayoutSession.observeTick` と同じく、直近の tick 間隔の下位
/// 25% を使う。メインスレッドが詰まってコールバックが 1 回飛ぶと間隔が 2 倍になるため、
/// 中央値ではなく下位 25% を使う。moqt-rs では表示の tick がメインループの 1 回であり、
/// raw_player はその次の `poll_events` で描画する。したがって表示周期がそのまま提示遅延
/// (1 tick) の近似になる。
///
/// ここで測るのは **select の呼び出し間隔** であり、select から次の `poll_events` までの
/// 実測ではない。前者は後者より「poll から select までの処理時間」の分だけわずかに大きい
/// ため、目標表示時刻はその分だけ楽観的になる。1 tick の遅延を近似する目的では無視できる。
///
/// `Instant` を内部に持たず、壁時計のマイクロ秒を引数で受け取るため、SDL もプレイヤーも
/// 使わずに単体テストできる。
struct VideoDisplayPeriod {
    /// 直前の tick の時刻 (受信側の壁時計、マイクロ秒)。まだ観測していなければ None
    last_tick_us: Option<i64>,
    /// 直近の tick 間隔 (マイクロ秒)
    intervals_us: VecDeque<i64>,
    /// いまの表示周期 (マイクロ秒)
    refresh_us: i64,
}

impl VideoDisplayPeriod {
    /// まだ tick を観測していない状態で作る
    fn new() -> Self {
        Self {
            last_tick_us: None,
            intervals_us: VecDeque::new(),
            refresh_us: VIDEO_DEFAULT_REFRESH_US,
        }
    }

    /// tick を観測し、その時点の表示周期 (マイクロ秒) を返す
    ///
    /// 4 tick 分 (4 ms) を超える間隔は、ループが復号や描画で止まった可能性があるため、
    /// サンプルを捨てて初期値へ戻す (moqt-js 0885 と同じ)。止まった直後は表示周期を過信
    /// せず初期値の 1 ms を目標にする。その結果として目標が実際より 1 ms 程度過去になる
    /// が、遅れたフレームは `max_late_us` の逃げ道で拾うため害は小さい。サンプルが 2 つ
    /// 未満の間は初期値のままにする。
    fn observe_tick(&mut self, tick_us: i64) -> i64 {
        let Some(previous_us) = self.last_tick_us.replace(tick_us) else {
            return self.refresh_us;
        };
        let interval_us = tick_us.saturating_sub(previous_us);
        if interval_us <= 0 {
            return self.refresh_us;
        }
        if interval_us > VIDEO_DEFAULT_REFRESH_US.saturating_mul(4) {
            self.intervals_us.clear();
            self.refresh_us = VIDEO_DEFAULT_REFRESH_US;
            return self.refresh_us;
        }
        self.intervals_us.push_back(interval_us);
        if self.intervals_us.len() > VIDEO_REFRESH_SAMPLE_COUNT {
            self.intervals_us.pop_front();
        }
        if self.intervals_us.len() < 2 {
            return self.refresh_us;
        }
        self.refresh_us = estimate_refresh_us(&self.intervals_us);
        self.refresh_us
    }

    /// 目標表示時刻 (tick + 提示遅延) を求める
    fn target_us(&self, tick_us: i64) -> i64 {
        tick_us.saturating_add(
            self.refresh_us
                .saturating_mul(VIDEO_PRESENTATION_LATENCY_TICKS),
        )
    }

    /// 目標表示時刻からの許容幅 (表示周期 / 2) を求める
    fn tolerance_us(&self) -> i64 {
        self.refresh_us / 2
    }

    /// 遅れて提示してよい上限 (マイクロ秒) を求める
    ///
    /// moqt-js 0885 は `max(MAX_PRESENTATION_LAG_MS, 表示周期) + 提示遅延` を使う。表示
    /// 周期が 20 ms より長い (30 Hz 以下) ときは、到着の位相によっては 1 tick 遅れが
    /// 20 ms を超えるため、表示周期そのものも下限にする。
    fn max_late_us(&self) -> i64 {
        self.refresh_us.max(MAX_PRESENTATION_LAG_US).saturating_add(
            self.refresh_us
                .saturating_mul(VIDEO_PRESENTATION_LATENCY_TICKS),
        )
    }
}

/// tick 間隔から表示周期を推定する (下位 25%)
///
/// 空なら初期値を返す。moqt-js 0885 の `estimateRefreshMs` と同じ規則である。
fn estimate_refresh_us(intervals_us: &VecDeque<i64>) -> i64 {
    if intervals_us.is_empty() {
        return VIDEO_DEFAULT_REFRESH_US;
    }
    let mut sorted: Vec<i64> = intervals_us.iter().copied().collect();
    sorted.sort_unstable();
    let index = (sorted.len() - 1) * 25 / 100;
    sorted[index].max(1)
}

/// 単調増加するはずのカウンタの増分を求める
///
/// プレイヤーを作り直した直後など、基準を取り直す前に値が減った場合は 0 として扱う。
fn counter_increase(current: i64, previous: i64) -> u64 {
    u64::try_from(current.saturating_sub(previous)).unwrap_or(0)
}

/// 壁時計の TIMESTAMP を持つ映像フレームの TIMESTAMP (マイクロ秒) を返す
///
/// 時間軸へ観測し、表示時刻を決められるのは壁時計の TIMESTAMP だけである。TIMESTAMP が
/// 無いフレームと、TIMESCALE を持つ (メディア時刻の) フレームは、受信側の壁時計と対応
/// しないため `None` を返す。`None` を渡された表示待ちのキューは、届いた順に表示する。
fn wall_clock_timestamp(frame: &DecodedVideoFrame) -> Option<i64> {
    match frame.timestamp_kind {
        TimestampKind::WallClock => frame.timestamp_us,
        TimestampKind::None | TimestampKind::MediaTime => None,
    }
}

/// 描画を観測したフレームを、時間軸の表示実績と計測用の標本へ反映する
///
/// 実績には推定表示時刻ではなく、描画を観測した時刻 (`observed_us`) をそのまま記録する。
/// moqt-js 0885 が推定に変えたのは表示を観測する手段が無いためであり、moqt-rs には
/// `poll_events` 後の `stats()` 差分で描画を観測する手段がある。推定にすると A/V 同期の
/// ずれ (`skew_us`) がモデル出力になり、スタールや表示の遅れが映らなくなる。
///
/// 記録した時刻と `present_us` が決めた表示時刻との差 (残差) は、推定の妥当性を読むために
/// 集計だけする (判定には使わない)。壁時計の TIMESTAMP を持たないフレームは、時間軸の
/// 実績へは記録しない (音声と対応づけられない) が、投入から描画までの遅れは数える。
fn record_observed_video_frames(
    rendered: Vec<RenderedVideoFrame>,
    observed_us: i64,
    timeline: &mut PlayoutTimeline,
    enqueue_delays_us: &mut SampleWindow,
    presentation_residuals_us: &mut SampleWindow,
) {
    for frame in rendered {
        enqueue_delays_us.push(i64::try_from(frame.enqueue_delay_us).unwrap_or(i64::MAX));
        let Some(timestamp_us) = frame.timestamp_us else {
            continue;
        };
        // 残差は、実績を記録する前の時間軸が決めた表示時刻で測る
        if let Some(expected_us) = timeline.present_us(Track::Video, timestamp_us) {
            presentation_residuals_us.push(observed_us.saturating_sub(expected_us));
        }
        timeline.record_presentation(Track::Video, timestamp_us, observed_us);
    }
}

/// 標本の分布 (p50 / p95 / max、マイクロ秒)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SamplePercentiles {
    /// p50 (中央値)
    p50_us: i64,
    /// p95
    p95_us: i64,
    /// 最大値
    max_us: i64,
}

/// ログの区間ごとに標本を集め、p50 / p95 / max を求める窓
///
/// 判定には使わず、ログに出すためだけに使う。ログを出したら [`SampleWindow::clear`] で
/// 空にして、次の区間の分布を出す。
#[derive(Default)]
struct SampleWindow {
    /// 集めた標本 (マイクロ秒)
    samples_us: Vec<i64>,
}

impl SampleWindow {
    /// 標本を 1 つ足す
    fn push(&mut self, value_us: i64) {
        self.samples_us.push(value_us);
    }

    /// 標本を捨てる
    fn clear(&mut self) {
        self.samples_us.clear();
    }

    /// 標本数 (テストで境界を確かめるためだけに使う)
    #[cfg(test)]
    fn len(&self) -> usize {
        self.samples_us.len()
    }

    /// p50 / p95 / max を求める。標本が無ければ `None`
    ///
    /// 百分位は標本を昇順に並べ、`(標本数 - 1) * 百分位 / 100` 番目を取る。標本が 1 件
    /// なら p50 / p95 / max はすべてその値になる。
    fn percentiles(&self) -> Option<SamplePercentiles> {
        if self.samples_us.is_empty() {
            return None;
        }
        let mut sorted = self.samples_us.clone();
        sorted.sort_unstable();
        let percentile = |percent: usize| sorted[(sorted.len() - 1) * percent / 100];
        Some(SamplePercentiles {
            p50_us: percentile(50),
            p95_us: percentile(95),
            max_us: *sorted.last().expect("標本は空でない"),
        })
    }
}

/// 分布をログ用の文字列にする (マイクロ秒をミリ秒へ直す)
///
/// 標本が無ければ `none`。ログは英語で出す。
fn percentile_log(percentiles: Option<SamplePercentiles>) -> String {
    percentiles.map_or_else(
        || "none".to_string(),
        |percentiles| {
            format!(
                "p50={:.1}ms p95={:.1}ms max={:.1}ms",
                percentiles.p50_us as f64 / 1_000.0,
                percentiles.p95_us as f64 / 1_000.0,
                percentiles.max_us as f64 / 1_000.0,
            )
        },
    )
}

/// 映像の表示の選択の計器
///
/// `dropped` は提示しなかったフレームの数、`draw_late` は目標に合うフレームが無くて遅れて
/// 提示した回数である (逃げ道がどれだけ働いたかを読む)。判定には使わない。
#[derive(Default)]
struct VideoSelectionCounters {
    /// 提示しなかったフレームの累計
    not_shown_frames: u64,
    /// 遅れて提示した回数の累計
    draw_late_count: u64,
}

impl VideoSelectionCounters {
    /// 1 回の選択の結果を数える
    fn record(&mut self, dropped_frames: usize, draw_late: bool) {
        self.not_shown_frames = self
            .not_shown_frames
            .saturating_add(u64::try_from(dropped_frames).unwrap_or(u64::MAX));
        if draw_late {
            self.draw_late_count = self.draw_late_count.saturating_add(1);
        }
    }
}

/// 壁時計の TIMESTAMP を持つ音声フレームの TIMESTAMP (マイクロ秒) を返す
///
/// 時間軸へ観測し、表示時刻を決められるのは壁時計の TIMESTAMP だけである。TIMESTAMP が
/// 無い音とメディア時刻の音は `None` を返す。音の長さや並べ方は PTS そのものを使うため、
/// 呼び出し側は予約へは常に `pts_us` を渡す。
fn wall_clock_pts(audio: &DecodedAudioFrame) -> Option<i64> {
    match audio.timestamp_kind {
        TimestampKind::WallClock => Some(audio.pts_us),
        TimestampKind::None | TimestampKind::MediaTime => None,
    }
}

/// 実出力の先行量 (マイクロ秒) を求める
///
/// 出力へ積んだ累積サンプル数と鳴り終えた累積サンプル数の差を「積んだまま鳴っていない
/// 長さ」とし、スケジューラが予測している予約の先行量を引いた残差を、出力経路が実際に
/// 抱えている分として返す。moqt-rs は予約を未来時刻へ置かず FIFO へ即座に積むため、
/// moqt-js の `getOutputTimestamp` の式をそのまま使うことはできない
/// (`docs/port-notes/assembly.md` 3.4 節)。
///
/// `total_samples_played` は SDL のキューの残りから求めた「デバイスへ渡した」時点の値で
/// あり、実際に鳴り終えた時刻ではない。呼び出し側は積んだ直後に `process()` を呼び、
/// 自分の未 flush 分が残らないようにしてから読むこと。
///
/// サンプルレートが 0 のときと、まだ 1 音も並べていない (予約の終端が無い) ときは `None`。
fn output_lead_us(
    enqueued_samples: i64,
    played_samples: i64,
    sample_rate: u32,
    last_end_us: Option<i64>,
    now_us: i64,
) -> Option<i64> {
    let last_end_us = last_end_us?;
    if sample_rate == 0 {
        return None;
    }
    let committed_samples = enqueued_samples.saturating_sub(played_samples).max(0);
    let committed_us = committed_samples
        .saturating_mul(1_000_000)
        .saturating_div(i64::from(sample_rate));
    let scheduled_us = last_end_us.saturating_sub(now_us).max(0);
    Some(committed_us.saturating_sub(scheduled_us).max(0))
}

/// 停止時の drain で待つ時間 (マイクロ秒) を求める
///
/// 最後の予約の終わりまでの時間に余裕を足し、上限で切る。予約が無いときは余裕だけを
/// 待ち時間にするが、drain の終了判定は「鳴り終えたか」を先に見るため、実際には
/// すぐ終わる (moqt-js の `drainReserved` と同じ構造である)。
fn drain_requested_us(pending_end_us: Option<i64>, now_us: i64) -> i64 {
    let remaining_us = pending_end_us.map_or(0, |end_us| end_us.saturating_sub(now_us).max(0));
    remaining_us
        .saturating_add(AUDIO_DRAIN_MARGIN_US)
        .clamp(0, AUDIO_DRAIN_MAX_US)
}

/// 停止時の drain を終えてよいかを判定する
///
/// 予約が鳴り終わり (`now_us` が最後の予約の終端以降)、かつ出力キューが空なら終える。
/// どちらかが満たされなくても、待ち時間の上限に達したら終える。復号前遅延を移植して
/// いないため、flush した Object の復号出力を待つ条件は無い。
fn drain_finished(
    now_us: i64,
    started_us: i64,
    requested_us: i64,
    last_end_us: Option<i64>,
    enqueued_samples: i64,
    played_samples: i64,
) -> bool {
    let elapsed_us = now_us.saturating_sub(started_us);
    if elapsed_us >= requested_us {
        return true;
    }
    let output_drained = played_samples >= enqueued_samples;
    let reserved_drained = last_end_us.is_none_or(|last_end_us| now_us >= last_end_us);
    output_drained && reserved_drained
}

/// 直前の音の媒体終端から今回の音の先頭までの差 (マイクロ秒) を求める
///
/// 正なら媒体時刻がその分飛んでいる (欠落)。負なら媒体時刻が戻っている (異常だが、
/// 欠落ではない)。1 マイクロ秒以下の差を丸めの誤差として無視するかは、使う側が決める
/// ([`apply_audio_playout`] は履歴の判定に `abs() <= 1` を使い、接続フェードには
/// 2 マイクロ秒以上の飛びだけを使う)。
fn media_gap_us(previous_media_end_pts_us: i64, pts_us: i64) -> i64 {
    pts_us.saturating_sub(previous_media_end_pts_us)
}

/// 履歴として残すサンプル数を求める (マイクロ秒を切り上げる)
///
/// 30 ms を下回らないよう、端数は切り上げる。`max_us` が 0 以下、またはサンプルレートが
/// 0 のときは 0。
fn history_sample_count(max_us: i64, sample_rate: u32) -> usize {
    if max_us <= 0 || sample_rate == 0 {
        return 0;
    }
    let scaled = max_us.saturating_mul(i64::from(sample_rate));
    usize::try_from(scaled.saturating_add(999_999) / 1_000_000).unwrap_or(usize::MAX)
}

/// 実音の末尾を履歴へ足し、`max_us` ぶんだけ残す
///
/// 履歴は隙間の材料化 ([`gap::materialize`] の `previous`) と時間伸長
/// ([`stretch::preemptively_expand`] の `history`) へ渡す「直前の実音」である。次に当てはまる
/// ときは古い履歴を捨て、今回の音から作り直す。
///
/// - サンプルレートが変わった
/// - チャンネル数が変わった
/// - 媒体時刻が飛んだ (前の音の媒体終端と今回の音の先頭が 1 マイクロ秒を超えて離れた)
///
/// 空の音、チャンネル数 0、長さの揃っていない音、サンプルレート 0 は履歴へ足さない
/// (呼び出し側の取りこぼしで履歴を壊さないため)。`max_us` が 0 以下なら履歴を捨てる。
fn push_history(
    history: &mut Option<PlayedAudioHistory>,
    channels: &[Vec<f32>],
    sample_rate: u32,
    media_start_pts_us: i64,
    media_end_pts_us: i64,
    max_us: i64,
) {
    let Some(length) = channels.first().map(Vec::len) else {
        return;
    };
    if length == 0 || channels.iter().any(|channel| channel.len() != length) || sample_rate == 0 {
        return;
    }
    if max_us <= 0 {
        *history = None;
        return;
    }
    let max_samples = history_sample_count(max_us, sample_rate);
    if max_samples == 0 {
        *history = None;
        return;
    }
    // 媒体時刻が 1 マイクロ秒を超えて離れている音は、前の音と連続していない (欠落、配信元の
    // 切り替え、時刻の巻き戻り)。連続していない音を連結すると、周期補間と線形予測の解析が
    // 別の音の継ぎ目をまたいでしまうため、履歴を作り直す
    let keep_existing = history.as_ref().is_some_and(|history| {
        history.sample_rate == sample_rate
            && history.channels.len() == channels.len()
            && media_start_pts_us
                .saturating_sub(history.media_end_pts_us)
                .saturating_abs()
                <= 1
    });
    if !keep_existing {
        *history = Some(PlayedAudioHistory {
            channels: vec![Vec::new(); channels.len()],
            sample_rate,
            media_end_pts_us,
        });
    }
    let history = history.as_mut().expect("履歴を作り直した");
    for (destination, source) in history.channels.iter_mut().zip(channels) {
        destination.extend_from_slice(source);
        // 末尾の `max_samples` だけを残す。1 音ずつ足すため、落とすのは古い先頭だけである
        let excess = destination.len().saturating_sub(max_samples);
        if excess > 0 {
            destination.drain(..excess);
        }
    }
    history.media_end_pts_us = media_end_pts_us;
}

/// 履歴を今回の音の隙間の材料化と時間伸長に使えるか
///
/// サンプルレートとチャンネル数が一致し、各チャンネルの長さが揃っていて空でないこと。
/// 一致しない履歴を渡すと、前の音と今回の音で波形の対応が取れない。
fn is_history_usable(history: &PlayedAudioHistory, audio: &DecodedAudioFrame) -> bool {
    history.sample_rate == audio.sample_rate
        && history.channels.len() == usize::from(audio.channels)
        && history
            .channels
            .first()
            .is_some_and(|channel| !channel.is_empty())
        && history
            .channels
            .iter()
            .all(|channel| channel.len() == history.channels[0].len())
}

/// 予約に隙間が無いまま媒体時刻が飛んだとき、今回の音の先頭を直前の音へ接続するか
///
/// moqt-js の `playSound` と同じ条件である。隙間を材料化していない (`total_gap_us` が 0)
/// のに、直前の音の媒体終端から音の長さの半分以上が飛んでいると、波形が不連続になる。
/// そのときだけ [`splice_fade::smooth_splice_head`] で先頭を接続する。
fn should_smooth_splice(total_gap_us: i64, media_gap_us: i64, duration_us: i64) -> bool {
    total_gap_us <= 0 && media_gap_us.saturating_mul(2) >= duration_us
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
///
/// `video_subscribed` は映像トラックを購読しているかである。pipeline がカタログを読んだ
/// 時点で入れる。音声だけの購読では、到着基準の再生の遅れに下限を足し、目標の時刻を
/// 守らない並べ方にする (`AudioPlayoutAssembly::set_audio_only`)。
fn run_raw_player(
    namespace: &str,
    video_rx: std::sync::mpsc::Receiver<DecodedVideoFrame>,
    audio_rx: std::sync::mpsc::Receiver<DecodedAudioFrame>,
    display_backlog: std::sync::Arc<std::sync::atomic::AtomicI64>,
    target_latency_ms: std::sync::Arc<std::sync::atomic::AtomicI64>,
    video_subscribed: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
    // 映像は到着後すぐ表示せず、表示時刻まで保持して選ぶ。表示の tick ごとに、目標表示
    // 時刻に最も近い 1 枚を表示する (moqt-js 0885 の `selectNearest`)。保持する上限も
    // 時間軸の設定から取る。上限を変えると表示の遅れの切り下げと保持数の両方へ同じ値が効く
    let video_queue_limit = timeline.config().video_queue_limit;
    let mut video_buffer = PlayoutBuffer::<DecodedVideoFrame>::new(video_queue_limit, Track::Video);
    // 表示の tick (ループ 1 回) の周期を実測し、目標表示時刻・許容幅・遅れの上限を決める
    let mut video_display = VideoDisplayPeriod::new();

    // 音声は受信したその場で鳴らす時刻を決めて積む。積んだ順に鳴る FIFO であるため、
    // 最初の音の開始時刻に達するまでのフレームは再生機器の内部キューに先行バッファとして
    // 溜まる。順番・詰め・伸長・隙間はスケジューラが決め、その結果をここへ集める
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
    // 表示の選択の計器 (提示しなかったフレーム数と、遅れて提示した回数)
    let mut video_selection_counters = VideoSelectionCounters::default();
    // ログ用の分布。1 秒ごとのログの区間で集め、ログを出したら捨てる。判定には使わない
    let mut video_enqueue_delays_us = SampleWindow::default();
    let mut video_presentation_residuals_us = SampleWindow::default();
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
    // 直近に再生の組み立てへ反映した「音声だけの購読」か。初期値は映像ありとして扱う
    let mut applied_audio_only = false;
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

        // 映像トラックを購読しているかが届いていれば、音声だけの再生の組み立てへ反映する。
        // カタログの解決後に 1 回だけ変わる
        let audio_only_now = !video_subscribed.load(std::sync::atomic::Ordering::Relaxed);
        if audio_only_now != applied_audio_only {
            applied_audio_only = audio_only_now;
            audio_state.set_audio_only(audio_only_now);
            tracing::info!("Applied audio-only playout: {audio_only_now}");
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
            // 実際に描画されたと観測したフレームを、観測した時点の壁時計で表示の実績と
            // して残す。write 時刻 (推定表示時刻) ではなく観測を記録することで、スタールや
            // 表示の遅れが skew に現れる。投入から描画までの遅れと、表示時刻の予定との
            // 残差は、ログ用に集計する (判定には使わない)
            let now_us = wall_clock_us();
            record_observed_video_frames(
                observation.rendered,
                now_us,
                &mut timeline,
                &mut video_enqueue_delays_us,
                &mut video_presentation_residuals_us,
            );
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
        // プレイヤーが詰まっていないかを読めるようにする。提示しなかったフレーム数、
        // 遅れて提示した回数、投入から描画までの遅れと表示時刻の残差の分布も出す
        if video_player.is_some()
            && last_video_log.elapsed().as_millis() >= VIDEO_PLAYER_LOG_INTERVAL_MS
        {
            last_video_log = std::time::Instant::now();
            let enqueue_delay_log = percentile_log(video_enqueue_delays_us.percentiles());
            let presentation_residual_log =
                percentile_log(video_presentation_residuals_us.percentiles());
            tracing::info!(
                "Video player: enqueued={} rendered={} dropped={} (queue_full={} late={} resync={}) queue={} drain_recoveries={} player_resyncs={} not_shown={} draw_late={} enqueue_to_render=({}) presentation_residual=({})",
                video_enqueued_count,
                video_accounting.rendered_total(),
                video_accounting.dropped_total(),
                video_player_dropped_queue_full,
                video_player_dropped_late,
                video_player_dropped_resync,
                video_queue_size,
                video_accounting.recoveries(),
                video_player_resyncs,
                video_selection_counters.not_shown_frames,
                video_selection_counters.draw_late_count,
                enqueue_delay_log,
                presentation_residual_log,
            );
            // 分布は次のログの区間だけを出す
            video_enqueue_delays_us.clear();
            video_presentation_residuals_us.clear();
        }

        match video_rx.try_recv() {
            Ok(frame) => {
                // プレイヤーが引き取った分だけ、受信側が数える待ちフレーム数を減らす
                display_backlog.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                // 壁時計の TIMESTAMP を持つフレームだけ、復号の出力を時間軸へ観測し、
                // 表示時刻が決まるまで保持する。TIMESTAMP が無いフレームとメディア時刻の
                // フレームは表示時刻を決められないため、届いた順に表示する
                let timestamp_us = wall_clock_timestamp(&frame);
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

        // 表示の tick の周期を実測し、目標表示時刻に最も近いフレームを 1 枚表示する。
        // raw_player は積んだフレームを次の poll_events で描画するため、表示周期がそのまま
        // 提示遅延 (1 tick) になり、目標表示時刻は tick + 提示遅延になる (moqt-js 0885)
        {
            let tick_us = wall_clock_us();
            video_display.observe_tick(tick_us);
            let target_us = video_display.target_us(tick_us);
            let selection = video_buffer.select_nearest(
                target_us,
                video_display.tolerance_us(),
                video_display.max_late_us(),
                &timeline,
            );
            // 提示しなかったフレームと、遅れて提示した回数を計器に足す
            video_selection_counters.record(selection.dropped.len(), selection.draw_late);
            for dropped in selection.dropped {
                tracing::debug!(
                    "Discarded a video frame not chosen for display: {}x{}",
                    dropped.width,
                    dropped.height
                );
            }
            if let Some(frame) = selection.draw {
                if selection.draw_late {
                    tracing::debug!(
                        "Showing a video frame later than its presentation time: {}x{}",
                        frame.width,
                        frame.height
                    );
                }
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
                    // 表示待ちのキューから選んだフレームを、再生側のいまの時刻で渡す
                    let enqueue_us = start_time.elapsed().as_micros() as u64;
                    let pts_us = i64::try_from(enqueue_us).unwrap_or(i64::MAX);
                    // 対応付けの FIFO へ積む TIMESTAMP は壁時計のときだけにする。メディア
                    // 時刻のフレームを実績として記録すると、A/V 同期の推定が壊れる
                    let timestamp_us = wall_clock_timestamp(&frame);
                    match p.enqueue_video_i420(
                        &frame.y,
                        &frame.u,
                        &frame.v,
                        frame.width,
                        frame.height,
                        pts_us,
                    ) {
                        Ok(()) => {
                            video_enqueued_count += 1;
                            video_accounting.record_enqueue(timestamp_us, enqueue_us);
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
                        if has_audio_frames(&audio) {
                            let now_us = wall_clock_us();
                            // 壁時計の TIMESTAMP を持つ音だけ、時間軸へ観測する。観測の
                            // 基準には復号の出力が揃った時刻を使う
                            if let Some(pts_us) = wall_clock_pts(&audio) {
                                timeline.observe(Track::Audio, audio.decoded_us, pts_us);
                            }
                            play_decoded_audio(
                                audio_player,
                                &mut audio_state,
                                &mut timeline,
                                &audio,
                                now_us,
                            );
                        } else {
                            // 復号の出力が空の音 (AAC の priming など) は鳴らせない。時間軸への
                            // 観測と予約へ渡すと基準の学習と計器が壊れるため、その前に弾き、
                            // 長さ 0 の失敗として件数だけを記録する
                            audio_state.record_invalid_decode(wall_clock_us());
                        }
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
            if last_audio_log.elapsed().as_millis() >= AUDIO_BUFFER_LOG_INTERVAL_MS {
                last_audio_log = std::time::Instant::now();
                // player_buffer は再生機器へ積んだまま鳴っていない長さである。これが 0 に
                // 近づくと音が途切れるため、予算が足りているかをここで確認できる。
                // player_buffer_min は前回のログ以降の最小値であり、一瞬の枯れも読める。
                // concealed と compressed と expanded は実際に適用できた長さの合計であり、
                // skew_us は実績から求めた A/V のずれ (映像が音声より遅れていれば正) である。
                // 末尾の audio_state.log_fields は、鳴るはずの時刻・到着から鳴り始めるまで・
                // 予定に対する余裕 (p50 / p95)・理由別の捨て・隙間とアンダーランの計器・
                // 音声の予算 (揺らぎ・実出力の先行量・不感帯) である
                let skew_us = timeline
                    .skew_us()
                    .map_or_else(|| "none".to_string(), |skew_us| skew_us.to_string());
                let player_buffer_min = player_buffer_min_ms
                    .take()
                    .map_or_else(|| "none".to_string(), |min| format!("{min:.0}ms"));
                tracing::info!(
                    "Audio jitter buffer: player_buffer={:.0}ms player_buffer_min={} dropped={} concealed={}ms concealments={} compressed={}ms expanded={}ms skew_us={} {}",
                    audio_player.stats().audio_buffer_ms,
                    player_buffer_min,
                    audio_state.scheduler.drops(),
                    audio_state.scheduler.concealed_us() / 1000,
                    audio_state.scheduler.concealments(),
                    audio_state.scheduler.compressed_us() / 1000,
                    audio_state.scheduler.expanded_us() / 1000,
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
    // ここで記録する描画は、ループを抜ける直前に実際に観測された表示である
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
        record_observed_video_frames(
            observation.rendered,
            now_us,
            &mut timeline,
            &mut video_enqueue_delays_us,
            &mut video_presentation_residuals_us,
        );
    }

    // 投入数と描画数・破棄数を区別する。描画が止まったまま終わった場合も、
    // 投入だけが進んだことがこの行で分かる。破棄の内訳と復帰回数、提示しなかった
    // フレーム数、遅れて提示した回数、最後の区間の分布も出す
    let enqueue_delay_log = percentile_log(video_enqueue_delays_us.percentiles());
    let presentation_residual_log = percentile_log(video_presentation_residuals_us.percentiles());
    tracing::info!(
        "Player stopped after enqueued={} rendered={} dropped={} (queue_full={} late={} resync={}) video frames / {} audio chunks (queue={} drain_recoveries={} player_resyncs={} not_shown={} draw_late={} enqueue_to_render=({}) presentation_residual=({}))",
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
        video_selection_counters.not_shown_frames,
        video_selection_counters.draw_late_count,
        enqueue_delay_log,
        presentation_residual_log,
    );
    // 音声の再生を止める。まず再生を開始していれば、予約した音を鳴らし切るまで待つ
    // (停止時の drain)。待っても鳴り終わらなかった分は、record_stopped が予約したまま
    // 鳴らなかった音として計器へ残す。`AudioPlayer::stop()` は計器が読む値を失わないよう、
    // record_stopped の後に呼ぶ
    if audio_output_enabled {
        let stopped_us = wall_clock_us();
        // 枯れたまま終わった場合も、その長さを計器へ残す
        audio_state.end_underrun(stopped_us);
        if let Some(audio_player) = audio_player.as_ref()
            && let Some(result) = audio_state.drain(audio_player)
        {
            tracing::info!(
                "Audio playout drained: pending={}ms requested={}ms waited={}ms cut={}ms",
                result.pending_us_at_start / 1_000,
                result.requested_us / 1_000,
                result.waited_us / 1_000,
                result.cut_us / 1_000,
            );
        }
        let stopped_us = wall_clock_us();
        audio_state.record_stopped(stopped_us);
        tracing::info!(
            "Audio playout stopped: {}",
            audio_state.log_fields(&timeline, stopped_us)
        );
        if let Some(audio_player) = audio_player.as_ref()
            && let Err(e) = audio_player.stop()
        {
            tracing::warn!("Failed to stop audio playback: {e}");
        }
    }
    drop(video_player);
    drop(audio_player);
    // SAFETY: プレイヤーループ終了後に一度だけ呼び出す
    unsafe { raw_player::quit() };
    Ok(())
}

/// 音声の再生の組み立て (時間軸・スケジューラ・計器を 1 か所で扱う)
///
/// 1 つの音について、目標の開始時刻の決定 ([`AudioPlayoutAssembly::arrange`])、実際に
/// 適用した詰め・伸長・隙間の反映 ([`AudioPlayoutAssembly::commit`])、計器への記録を
/// ここへ集める。再生機器へ積む順序 (隙間の音、今回の音) だけは
/// [`play_decoded_audio`] が受け持つ。
///
/// 時間軸への到着の記録は受信側 ([`run_raw_player`]) が済ませているため、ここでは行わ
/// ない。時間軸から読むのは目標の開始時刻と、揺らぎ・実出力の先行量から決まる予算だけ
/// である。
///
/// 再生機器へ積むための状態 (再生を開始したか、積んだ音の位置の対応、直前に鳴らした
/// 波形) も、音声の再生の経路で 1 つに持ち回るためここに置く。
///
/// 再生を止めるときは [`AudioPlayoutAssembly::drain`] で予約を鳴らし切ってから
/// [`AudioPlayoutAssembly::record_stopped`] を呼ぶ。予約したまま鳴り始めなかった音は、
/// ここで数えないとどの統計にも現れない。
struct AudioPlayoutAssembly {
    /// 再生を開始したか
    started: bool,
    /// 最初に積んだ音の開始時刻 (マイクロ秒)。ここに達したら `play()` を呼ぶ
    playback_start_at_us: Option<i64>,
    /// 再生の開始に失敗したことを既にログに出したか
    ///
    /// 開始できるまで毎ループ試すため、失敗が続くときにログを溢れさせないようにする
    play_failure_logged: bool,
    /// 鳴らす時刻・詰め・伸長・隙間を決める
    scheduler: AudioPlayoutScheduler,
    /// 鳴るはずの時刻・到着・鳴り始めと、理由別の捨てを記録する計器
    stats: AudioPlayoutTimingStats,
    /// 直前に鳴らした実音の末尾 (複数フレームを連結した履歴)。欠落した隙間はこの末尾を
    /// 周期の繰り返しや線形予測の続き音で埋める。隙間の材料化で作った音は入れない
    real_audio_history: Option<PlayedAudioHistory>,
    /// 直前に復号した音の波形 (圧縮前)。媒体時刻が飛んだときの接続フェードに使う
    last_decoded: Option<DecodedAudioHistory>,
    /// 再生機器へ積んだ音の位置の対応
    position: AudioPlayoutPosition,
    /// 直前に積んだ音の媒体終端の PTS (マイクロ秒)。次の隙間の PTS に使う
    last_media_end_pts_us: Option<i64>,
    /// 枯れ始めた時刻 (マイクロ秒)。枯れていないときは None
    underrun_start_us: Option<i64>,
    /// 音声だけを購読しているか
    ///
    /// 映像に合わせる相手がいないため、目標の時刻を守らず到着基準で並べる。catalog の
    /// 解決後に pipeline から届く
    audio_only: bool,
    /// 直前に観測した到着基準の実出力の深さ (マイクロ秒)。ログ用
    last_arrival_depth_us: Option<i64>,
    /// 直近の停止時の drain の結果。ログ用
    last_drain: Option<AudioDrainResult>,
    /// 直前に時間軸へ実績を記録した音声 chunk の識別子
    ///
    /// 鳴っているサンプルは chunk の中で動き続けるため、chunk ごとに 1 回だけ記録する
    /// ための目印である。同じ chunk のままなら記録しない
    last_recorded_chunk_id: Option<u64>,
}

/// 直前に鳴らした実音の末尾 (複数フレームを連結した履歴)
///
/// 隙間の材料化 ([`gap::materialize`]) は `previous` の末尾 30 ms を履歴、その末尾 20 ms を
/// 解析の対象にする。時間伸長 ([`stretch::preemptively_expand`]) も直前の実音の末尾を
/// 履歴として使う。1 フレーム (約 20 ms) だけでは周期補間の再試行と線形予測の解析窓が
/// moqt-js と変わってしまうため、30 ms 以上を連結して持つ。
///
/// サンプルレート・チャンネル数が変わったときと、媒体時刻が飛んだときは捨てる
/// ([`push_history`])。隙間の材料化で作った音は入れない (実音だけを足す)。
struct PlayedAudioHistory {
    /// チャンネルごとの波形 (古い順)。各チャンネルの長さは揃っている
    channels: Vec<Vec<f32>>,
    /// サンプルレート (Hz)
    sample_rate: u32,
    /// 最後に足した実音の媒体終端の PTS (マイクロ秒)
    media_end_pts_us: i64,
}

/// 直前に復号した音 (時間圧縮の前) の波形
struct DecodedAudioHistory {
    /// チャンネルごとの波形 (S16 interleaved から直した f32。圧縮も伸長もしていない)
    channels: Vec<Vec<f32>>,
    /// サンプルレート (Hz)
    sample_rate: u32,
}

/// 停止時の drain の結果
#[derive(Debug, Clone, Copy)]
struct AudioDrainResult {
    /// 開始時にまだ鳴り終わっていなかった予約の長さ (マイクロ秒)
    pending_us_at_start: i64,
    /// 待つと決めた時間 (マイクロ秒)
    requested_us: i64,
    /// 実際に待った時間 (マイクロ秒)
    waited_us: i64,
    /// 鳴り終わらずに切った長さ (マイクロ秒)
    cut_us: i64,
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
            real_audio_history: None,
            last_decoded: None,
            position: AudioPlayoutPosition::new(),
            last_media_end_pts_us: None,
            underrun_start_us: None,
            audio_only: false,
            last_arrival_depth_us: None,
            last_drain: None,
            last_recorded_chunk_id: None,
        }
    }

    /// 音声だけの購読かどうかを設定する (catalog の解決後に 1 回だけ変わる)
    fn set_audio_only(&mut self, audio_only: bool) {
        self.audio_only = audio_only;
    }

    /// 時間軸へ実績を記録すべき音を返す (chunk ごとに 1 回だけ)
    ///
    /// 鳴っているサンプルは同じ chunk の中で動き続けるため、毎ループ記録すると同じ音を
    /// 何度も上書きしてしまう。chunk が次へ進んだときだけ返す。返した chunk は記録済みと
    /// して覚える。
    fn next_sounding_to_record(
        &mut self,
        played_samples: i64,
        sample_rate: u32,
    ) -> Option<SoundingAudio> {
        let sounding = self.position.sounding(played_samples, sample_rate)?;
        if self.last_recorded_chunk_id == Some(sounding.chunk_id) {
            return None;
        }
        self.last_recorded_chunk_id = Some(sounding.chunk_id);
        Some(sounding)
    }

    /// 1 つの音を鳴らす時刻へ予約する
    ///
    /// 時間軸から目標の開始時刻・音声の予算・到着基準の遅れ・表示の遅れ・圧縮の不感帯を
    /// 取り、スケジューラへ渡す。鳴らさないと決まったときは、その理由 (並べすぎ) と長さを
    /// 計器へ記録して `None` を返す。
    ///
    /// 鳴らす組み立てへ渡す時点で、時間圧縮と補間を使えるサンプルレートかも確かめる。
    /// 使えないレートの音は計器へ数える (鳴らし方は変えない)。
    ///
    /// `arrival_us` は到着した音がまだ鳴っていない位置であり、`now_us` と同じ壁時計の軸で
    /// ある。受信側には AudioContext の出力時計との対応が無いため、呼び出し側は到着時点の
    /// 壁時計 (`now_us`) を渡す。`arrival_us` に媒体の PTS を渡してはいけない
    /// (PTS は基準と表示の遅れの分だけ過去の別の軸であり、到着基準の遅れがその分ずれる)。
    fn arrange(
        &mut self,
        timeline: &mut PlayoutTimeline,
        audio: &DecodedAudioFrame,
        now_us: i64,
        arrival_us: i64,
        enforce_target: bool,
    ) -> Option<AudioPlayoutArrangement> {
        // 4 kHz へ間引くフィルタの表に無いサンプルレート (96 kHz など) では、ピッチの
        // 周期を求められないため時間圧縮も補間も働かない。44.1 kHz は解析用の換算で周期を
        // 求めるため使える。落ちずに「操作しない」を返して区別
        // できないので、計器に数えて気付けるようにする。数えるだけで鳴らし方は変えない
        if !stretch::is_time_stretch_available(audio.sample_rate) {
            self.stats.record_stretch_unavailable();
        }
        let duration_us = pcm_duration_us(
            audio.pcm.len(),
            usize::from(audio.channels),
            audio.sample_rate,
        );
        // 目標の開始時刻は時間軸が返す「鳴らす時刻」である。壁時計の TIMESTAMP を持たない
        // 音と、まだ基準が無い・基準がずれている音は None であり、そのときは目標に従わず
        // 到着基準で並べる
        let target_start_us =
            wall_clock_pts(audio).and_then(|pts_us| timeline.present_us(Track::Audio, pts_us));
        // 音声の予算。直近 2 秒の到着の揺らぎと直近 1 秒の実出力の先行量から決まる
        // (時間軸が 50〜100 ms に切る)。並べすぎの判定に使う
        let delay_us = timeline
            .playout_delay_us()
            .unwrap_or(0)
            .max(AUDIO_PLAYOUT_DELAY_US);
        // 目標の時刻を守るのは、映像に合わせる相手がいるときだけである。音声だけの購読では
        // 相手がいないため、目標を守らず到着基準で並べる
        let enforce_target = enforce_target && !self.audio_only;
        // 到着基準で鳴らすときの遅れ。目標を守らないとき (音声だけの購読など) は、学習した
        // 値ではなく 50 ms の下限を使う (moqt-js の `AUDIO_PLAYOUT_DELAY_SECONDS`)
        let arrival_delay_us = if enforce_target {
            audio_arrival_delay_us(delay_us)
        } else {
            AUDIO_PLAYOUT_DELAY_US
        };
        // 表示の遅れは時間軸が返す値をそのまま渡す。まだ観測が無いときは既定値を使う
        let presentation_delay_us = timeline
            .presentation_extra_delay_us()
            .unwrap_or(AUDIO_PLAYOUT_DELAY_US);
        // 時間圧縮を始める不感帯。到着の揺らぎの学習が済むまでは既定値 (20 ms)、その後は
        // 揺らぎの大きさまで細かくなる
        let stretch_min_us = timeline.compression_deadband_us(AUDIO_PLAYOUT_STRETCH_MIN_US);
        let decision = self.scheduler.schedule(AudioPlayoutInput {
            now_us,
            arrival_us,
            timestamp_us: audio.pts_us,
            duration_us,
            target_start_us,
            // 目標が決まっているときだけ目標を守る (無いときは scheduler が到着基準にする)
            enforce_target,
            delay_us,
            arrival_delay_us,
            presentation_delay_us,
            stretch_min_us: Some(stretch_min_us),
        });
        let (
            start_at_us,
            basis,
            compress_us,
            expand_us,
            conceal_us,
            total_gap_us,
            unrecoverable_gap_us,
        ) = match decision {
            AudioPlayoutDecision::Play {
                start_at_us,
                basis,
                compress_us,
                expand_us,
                // 隙間の開始位置 (`gap_start_us`) は使わない。moqt-rs の出力は FIFO であり、
                // 隙間の全長を先頭から材料化して積めば、音は連続して鳴る
                gap_start_us: _,
                conceal_us,
                // 隙間は前の音の終わりから今回の開始までの全長であり、切り捨てない。
                // 補間してよい長さは conceal_us として別に返る
                total_gap_us,
                unrecoverable_gap_us,
            } => (
                start_at_us,
                basis,
                compress_us,
                expand_us,
                conceal_us,
                total_gap_us,
                unrecoverable_gap_us,
            ),
            AudioPlayoutDecision::Drop { reason } => {
                // 鳴らさなかった理由を計器の理由へ移す。並べすぎで捨てた長さを残す
                let miss_reason = match reason {
                    AudioPlayoutDropReason::Backlog => AudioMissReason::Backlog,
                };
                tracing::warn!(
                    "Discarded an audio chunk the playout scheduler dropped (reason {reason:?}, total {})",
                    self.scheduler.drops(),
                );
                self.record_miss(
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
            // 目標を使えない、または目標を守らない指定のため、到着基準で並べた
            tracing::debug!("Scheduled an audio chunk by arrival at {start_at_us}us");
        }
        Some(AudioPlayoutArrangement {
            start_at_us,
            basis,
            duration_us,
            target_start_us,
            arrival_us,
            compress_us,
            expand_us,
            conceal_us,
            total_gap_us,
            unrecoverable_gap_us,
        })
    }

    /// 実際に積めた補間の長さを返す (呼び出し側が隙間を埋めた後に呼ぶ)
    fn confirm_concealment(&mut self, applied_us: i64) {
        self.scheduler.confirm_concealment(applied_us);
    }

    /// 予約した音へ実際に適用した詰め・伸長・隙間を、計器へ反映する
    ///
    /// 実際に詰めた長さと伸ばした長さは、呼び出し側が
    /// [`AudioPlayoutScheduler::confirm_stretch`] と
    /// [`AudioPlayoutScheduler::confirm_expansion`] で先に返している。この関数は呼び出し側が
    /// 音を出力へ書き終えた後に呼ぶ。ここでは、まず予約の終端を実際に積んだ長さで確定し
    /// ([`AudioPlayoutScheduler::confirm_playout_end`])、その後に `scheduler.last_play()` を
    /// 計器へ渡す。この順序でないと、伸長した分と実際の終端が計器の鳴る長さへ入らない。
    ///
    /// 隙間は「材料化した全長」ではなく「補間で埋まらなかった長さ」(`uncovered_us`) で
    /// 数え、そのうち予約できなかった前置きを `unrecoverable_us` として別に数える。
    fn commit(
        &mut self,
        audio: &DecodedAudioFrame,
        arrangement: &AudioPlayoutArrangement,
        applied: AppliedAudioPlayout,
        gap_enqueued: bool,
        now_us: i64,
    ) {
        // 予約した終端を、実際に積んだフレーム数で確定する。時間圧縮と時間伸長で鳴る
        // 長さが変わるため、予定の終端との差を補正する (moqt-js の confirmPlayoutEnd)
        if let Some(channel) = applied.channels.first() {
            self.scheduler.confirm_playout_end(
                arrangement
                    .start_at_us
                    .saturating_add(samples_to_us(channel.len(), audio.sample_rate)),
            );
        }
        // 鳴らすと決めた音を計器へ渡す。詰めた分・伸ばした分・実際に積んだ終端を反映した
        // 後に読むことで、実際に鳴る長さが記録される
        if let Some(play) = self.scheduler.last_play() {
            self.stats.record_play(play);
        }
        // 隙間は補間で埋まらなかった長さで数える。予約できる最も早い時刻より前だった
        // 前置きの長さは、そのうちの unrecoverable として別に数える
        if gap_enqueued {
            let uncovered_us = arrangement
                .total_gap_us
                .saturating_sub(applied.concealed_us)
                .max(0);
            self.stats.record_gap(AudioPlayoutGap {
                uncovered_us,
                unrecoverable_us: arrangement.unrecoverable_gap_us,
            });
        }
        // 音声を積み直したため、枯れていた区間はここで終わる
        self.end_underrun(now_us);
        // 実際に鳴らした実音だけを履歴へ足す。隙間の材料化で作った音は実音ではないため、
        // 次の隙間の材料化や時間伸長の履歴には入れない。履歴は 30 ms を超えた分を古い方から
        // 落とし、サンプルレート・チャンネル数・媒体時刻が変わったら作り直す
        push_history(
            &mut self.real_audio_history,
            &applied.channels,
            audio.sample_rate,
            audio.pts_us,
            audio.pts_us.saturating_add(arrangement.duration_us),
            AUDIO_HISTORY_US,
        );
        // 接続フェードには、圧縮も伸長もしていない復号直後の波形を使う
        self.last_decoded = Some(DecodedAudioHistory {
            channels: pcm_f32_to_channels(&pcm_i16_to_f32(&audio.pcm), usize::from(audio.channels)),
            sample_rate: audio.sample_rate,
        });
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
    /// 枯れは事後には埋められないため、計器に残して再生予算が足りているかを読む。
    fn end_underrun(&mut self, now_us: i64) {
        let Some(start_us) = self.underrun_start_us.take() else {
            return;
        };
        let underrun_us = now_us.saturating_sub(start_us);
        self.stats.record_underrun(underrun_us);
    }

    /// 鳴らす準備の途中で失敗した音を計器へ記録する
    ///
    /// 再生機器へ積めなかった音は鳴らない。原因 (積む要求の失敗) を理由として残す。
    fn record_error(&mut self, arrangement: &AudioPlayoutArrangement, now_us: i64) {
        self.record_miss(
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
    fn record_stopped(&mut self, now_us: i64) {
        self.stats.record_stopped(now_us);
    }

    /// 鳴らさなかった音 1 つを計器へ記録する
    fn record_miss(
        &mut self,
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
    }

    /// 鳴らせない復号出力を、長さ 0 の失敗として計器へ記録する
    ///
    /// 空の PCM は時間軸へ観測せず、予約へも渡さない (基準の学習と計器が壊れるため)。
    fn record_invalid_decode(&mut self, now_us: i64) {
        self.stats.record_miss(AudioPlayoutMiss {
            at_us: now_us,
            reason: AudioMissReason::Error,
            duration_us: 0,
            target_us: None,
            arrival_us: None,
        });
    }

    /// 計器の観測値を求める
    fn snapshot(&mut self, now_us: i64) -> AudioPlayoutTimingSnapshot {
        self.stats.snapshot(now_us)
    }

    /// 計器と時間軸の値をログ用の文字列にする
    ///
    /// 補間で埋まらなかった隙間と検出したアンダーランの回数・合計・最大、鳴るはずの時刻
    /// (絶対値)、到着から鳴り始めるまでと予定に対する余裕の分布 (p50 / p95)、理由別の
    /// 捨ての件数と長さ、時間圧縮と補間を使えないサンプルレートで受け取った音の数、
    /// 音声だけの購読か、音声の予算の内訳 (揺らぎ・実出力の先行量・不感帯)、時間伸長した
    /// 長さ、到着基準の実出力の深さ、直近の drain の結果、予約の予定から実際の出力までの
    /// 残差を出す。
    fn log_fields(&mut self, timeline: &PlayoutTimeline, now_us: i64) -> String {
        let snapshot = self.snapshot(now_us);
        let breakdown = timeline.delay_breakdown();
        let audio_delay = timeline.audio_delay_stats();
        let deadband_us = timeline.compression_deadband_us(AUDIO_PLAYOUT_STRETCH_MIN_US);
        format!(
            "audio_only={} jitter_ms={} output_lead_ms={} target_ms={} deadband_ms={} expanded_ms={} gaps={} gap_ms={} gap_max_ms={} unrecoverable_gaps={} unrecoverable_gap_ms={} underruns={} underrun_ms={} underrun_max_ms={} arrival_depth_ms={} playout last_target_us={} last_arrival_us={} last_start_us={} last_slack={}ms last_start_delay={}ms last_lateness={}ms slack_p50_p95={}ms start_delay_p50_p95={}ms lateness_p50_p95={}ms played={} played_ms={} arrival_planned={} unplanned={} missed={} missed_ms={} missed_backlog={} missed_catch_up={} missed_error={} missed_stopped={} stretch_unavailable={} audio_playout_correction={}ms drain={}",
            self.audio_only,
            duration_log(audio_delay.jitter_us),
            duration_log(audio_delay.output_lead_us),
            audio_delay.target_us / 1000,
            deadband_us / 1000,
            self.scheduler.expanded_us() / 1000,
            snapshot.gaps,
            snapshot.gap_total_us / 1000,
            snapshot.gap_max_us / 1000,
            snapshot.unrecoverable_gaps,
            snapshot.unrecoverable_gap_total_us / 1000,
            snapshot.underruns,
            snapshot.underrun_total_us / 1000,
            snapshot.underrun_max_us / 1000,
            duration_log(self.last_arrival_depth_us),
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
            breakdown.audio_playout_correction_us / 1000,
            drain_log(self.last_drain),
        )
    }

    /// 予約済みの音を鳴らし切ってから止める (停止時の drain)
    ///
    /// 停止の要求時点で予約されている音の最後が鳴り終わるまで待ち、そのあとに
    /// [`AudioPlayoutAssembly::record_stopped`] で切った量を計器へ残す。待ち時間は
    /// 「最後の予約の終わり - 現在 + 余裕 (100 ms)」を上限 (`300 ms`) で切った値である。
    ///
    /// 復号前遅延を移植していないため、「flush した Object の復号出力が揃ったか」を待つ
    /// 条件は無い。`raw_player` の出力キューが空になったことと、予約の終端を過ぎたことの
    /// 両方で終える。まだ再生を開始していないとき (`play()` を呼んでいない) は、予約が
    /// 鳴ることはないため待たない。
    ///
    /// 戻り値はログ用の結果である。`record_stopped` より前に呼ぶこと。
    fn drain(
        &mut self,
        audio_player: &raw_player::audio_player::AudioPlayer,
    ) -> Option<AudioDrainResult> {
        if !self.started {
            return None;
        }
        let started_us = wall_clock_us();
        let pending_us_at_start = self.stats.pending_us(started_us);
        let requested_us = drain_requested_us(self.stats.pending_end_us(started_us), started_us);
        let mut process_failure_logged = false;
        let mut now_us = started_us;
        loop {
            if let Err(e) = audio_player.process()
                && !process_failure_logged
            {
                process_failure_logged = true;
                tracing::warn!("Failed to process the audio queue while draining: {e}");
            }
            let stats = audio_player.stats();
            if drain_finished(
                now_us,
                started_us,
                requested_us,
                self.scheduler.last_end_us(),
                stats.total_samples_enqueued,
                stats.total_samples_played,
            ) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_micros(
                u64::try_from(AUDIO_DRAIN_POLL_US).unwrap_or(25_000),
            ));
            now_us = wall_clock_us();
        }
        let finished_us = wall_clock_us();
        let result = AudioDrainResult {
            pending_us_at_start,
            requested_us,
            waited_us: finished_us.saturating_sub(started_us),
            cut_us: self.stats.pending_us(finished_us),
        };
        self.last_drain = Some(result);
        Some(result)
    }
}

/// 1 つの音を鳴らすために決まった内容 (再生機器へ渡す前に決まるもの)
///
/// 鳴り始める時刻と使った計画そのものは、計器へ渡すために
/// `scheduler.last_play()` から読む (実際に詰めた長さ・伸ばした長さが反映されている
/// ため)。ここには鳴らせなかったときの記録と、詰め・伸長・隙間の適用に必要な値だけを持つ。
struct AudioPlayoutArrangement {
    /// 鳴り始める時刻 (マイクロ秒)。最初の 1 音では再生開始の判定にも使う
    start_at_us: i64,
    /// 鳴らす時刻を決めるのに使った計画
    basis: AudioPlayoutBasis,
    /// 音の長さ (マイクロ秒)
    duration_us: i64,
    /// 目標の開始時刻。無いこともある
    target_start_us: Option<i64>,
    /// 到着の基準 (到着した音がまだ鳴っていない位置)
    arrival_us: i64,
    /// 波形の周期で詰める長さ (マイクロ秒)。0 なら詰めない
    compress_us: i64,
    /// 到着基準の深さを戻すために伸長してよい長さ (マイクロ秒)。0 なら伸ばさない
    expand_us: i64,
    /// 隙間のうち補間してよい長さ (マイクロ秒)。0 なら補間しない
    conceal_us: i64,
    /// 前の音の終わりから今回の開始までの隙間の全長 (マイクロ秒)。0 なら隙間は無い
    total_gap_us: i64,
    /// 隙間のうち、予約できる最も早い時刻より前だった長さ (マイクロ秒)
    ///
    /// moqt-rs の出力は FIFO であり、隙間は先頭から連続して材料化できるため、材料化の
    /// 置き場所には使わない。計器の `unrecoverable` にだけ使う。
    unrecoverable_gap_us: i64,
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

/// drain の結果をログ用の文字列にする (値が無ければ none)
fn drain_log(result: Option<AudioDrainResult>) -> String {
    result.map_or_else(
        || "none".to_string(),
        |result| {
            format!(
                "pending_ms={} requested_ms={} waited_ms={} cut_ms={}",
                result.pending_us_at_start / 1_000,
                result.requested_us / 1_000,
                result.waited_us / 1_000,
                result.cut_us / 1_000,
            )
        },
    )
}

/// 再生機器へ積んだ音の (累積サンプル数, PTS) の対応
///
/// `raw_player` の再生位置は「積んだ順に進むサンプル数」でしか分からない。積んだ時点の
/// 累積サンプル数と PTS を残し、いま鳴っているサンプルの PTS をここから求める。詰めたり
/// 補間したりした後の実際のサンプル数で記録するため、宣言した PTS を線形に外挿するより
/// 正しく求まる。
///
/// 時間軸へ実績を記録するときは、その音を予約したときの開始予定も要る。予定を鳴っている
/// 時点で timeline に問い直すと、その間に動いた補償の目標が混ざり、予約時の予定でなくなる
/// ため、積んだ時点の値を chunk に残す。
struct AudioPlayoutPosition {
    /// まだ鳴り終わっていない音 (積んだ順)
    pending: VecDeque<AudioPlayoutChunk>,
    /// 次に記録する chunk の識別子
    next_chunk_id: u64,
}

/// 再生機器へ積んだ音 1 つ分の位置
struct AudioPlayoutChunk {
    /// この音の識別子。chunk ごとに 1 回だけ実績を記録するために使う
    chunk_id: u64,
    /// この音が鳴り始める累積サンプル数
    start_samples: i64,
    /// この音の PTS (マイクロ秒)
    pts_us: i64,
    /// この音のサンプル数 (チャンネルごと)
    samples: i64,
    /// この音を予約したときの予定。隙間の音は予定を持たないため None
    planned: Option<PlannedAudio>,
}

/// 予約した音の予定 (実績の記録に使う)
struct PlannedAudio {
    /// 時間軸が決めた目標の開始時刻 (受信側の壁時計、マイクロ秒)。決められなければ None
    ///
    /// 予約後に timeline の値が動いても、ここでは測り直さない。
    target_us: Option<i64>,
    /// 鳴らす時刻を決めるのに使った計画
    basis: AudioPlayoutBasis,
    /// 復号の出力が揃った時刻 (受信側の壁時計、マイクロ秒)
    ///
    /// 到着基準で並べた音の実出力の深さ (鳴り始めた壁時計 − この時刻) を測るのに使う。
    decoded_arrival_us: i64,
}

/// いま鳴っている音の値
struct SoundingAudio {
    /// この音の識別子
    chunk_id: u64,
    /// 鳴っているサンプルの PTS (マイクロ秒)
    pts_us: i64,
    /// この音の予定。隙間の音は予定を持たないため None
    planned: Option<SoundingPlanned>,
}

/// いま鳴っているサンプルの予定
struct SoundingPlanned {
    /// そのサンプルの予定の時刻 (受信側の壁時計、マイクロ秒)。予定が無ければ None
    ///
    /// 予約した開始予定に、chunk の先頭 PTS から鳴っている位置までの差を足した値である。
    /// 出力の実績 (`presented`) と組にして時間軸へ渡すと、予定から実際の出力までの残差になる
    target_us: Option<i64>,
    /// 鳴らす時刻を決めるのに使った計画
    basis: AudioPlayoutBasis,
    /// この音の復号の出力が揃った時刻 (受信側の壁時計、マイクロ秒)
    decoded_arrival_us: i64,
}

impl AudioPlayoutPosition {
    /// 何も積んでいない状態で作る
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            next_chunk_id: 0,
        }
    }

    /// 積んだ音を記録する
    ///
    /// `planned` はその音を積んだときに決まっていた予定である。予定を持たない音 (隙間) は
    /// None を渡す。
    fn record(
        &mut self,
        start_samples: i64,
        pts_us: i64,
        samples: i64,
        planned: Option<PlannedAudio>,
    ) {
        let chunk_id = self.next_chunk_id;
        self.next_chunk_id = self.next_chunk_id.saturating_add(1);
        self.pending.push_back(AudioPlayoutChunk {
            chunk_id,
            start_samples,
            pts_us,
            samples,
            planned,
        });
    }

    /// いま鳴っている音の値 (PTS と予定) を求める
    ///
    /// `played_samples` は再生機器が鳴らし終えたサンプル数。鳴り終えた音は捨てる。すべて
    /// 鳴り終えたときと、再生位置が積んだ範囲より前へ戻ったとき (デバイスの作り直し) は
    /// `None` を返す。後者は対応が切れているため、記録を捨てて次の積み込みからやり直す。
    fn sounding(&mut self, played_samples: i64, sample_rate: u32) -> Option<SoundingAudio> {
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
        let elapsed_us = samples_to_us(elapsed_samples, sample_rate);
        // 予約した開始予定に、鳴っている位置の分だけ進めた時刻が、このサンプルの予定である
        let planned = chunk.planned.as_ref().map(|planned| SoundingPlanned {
            target_us: planned
                .target_us
                .map(|target_us| target_us.saturating_add(elapsed_us)),
            basis: planned.basis,
            decoded_arrival_us: planned.decoded_arrival_us,
        });
        Some(SoundingAudio {
            chunk_id: chunk.chunk_id,
            pts_us: chunk.pts_us.saturating_add(elapsed_us),
            planned,
        })
    }
}

/// デコード済みの音声を 1 つ、スケジューラの決めた時刻へ向けて再生機器へ積む
///
/// 鳴らす時刻の決定・計器への記録は [`AudioPlayoutAssembly`] が行う。ここは再生機器へ
/// 積む順序 (隙間の音、今回の音) と、実際に適用した詰め・伸長・隙間の長さをスケジューラへ
/// 返すことを受け持つ。`now_us` は今の時刻 (受信側の壁時計)。詰めは波形の周期 1 つ分を
/// 削り、伸長は直前の実音を重ねて周期 1 つ分を足し、隙間は [`gap::materialize`] で全長を
/// 材料化する (補間できなかった分は無音で埋まる)。
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
    // 到着の基準は、この音を受け取った時点の壁時計である。scheduler の `arrival_us` は
    // `now_us` と同じ軸 (壁時計マイクロ秒) の「到着した音がまだ鳴っていない位置」であり、
    // 受信側には AudioContext の出力時計との対応が無いため、`now_us` をそのまま渡す
    // (scheduler の doc の「時計の対応が無いときは now_us と同じ値」)
    let Some(arrangement) = state.arrange(timeline, audio, now_us, now_us, true) else {
        return;
    };

    // 直前の実音の媒体終端との差。時間伸長の履歴を渡してよいかの判定と、隙間が無いのに
    // 媒体時刻が飛んだときの接続に使う
    let media_gap_us = state.real_audio_history.as_ref().map_or(0, |history| {
        media_gap_us(history.media_end_pts_us, audio.pts_us)
    });
    // 詰め・伸長・隙間の材料化を音声へ適用する。返った隙間の音は今回の音の直前へ積むため、
    // 再生機器のキューでは前の音の直後、今回の音の直前になる (キューは積んだ順に鳴る)
    let applied = apply_audio_playout(
        audio,
        state.real_audio_history.as_ref(),
        state.last_decoded.as_ref(),
        arrangement.compress_us,
        arrangement.expand_us,
        arrangement.total_gap_us,
        arrangement.conceal_us,
        media_gap_us,
    );

    // 実際に適用した詰めと伸長をスケジューラへ返す。積む前に返すのは、積めなかった音でも
    // 適用済みの長さが予約の終端へ反映されている必要があるためである (moqt-js の
    // `playSound` と同じ順序)。伸長は基準と前の音の終わりがあるときだけ反映される
    state.scheduler.confirm_stretch(applied.compressed_us);
    state.scheduler.confirm_expansion(applied.expanded_us);

    // 隙間の音を積む。PTS は直前の音の媒体終端であり、まだ一度も音を積んでいなければ
    // 今回の音の PTS から隙間の全長だけ戻した値にする
    let gap_enqueued = if applied.gap_channels.is_empty() {
        false
    } else {
        let pts_us = gap_start_pts_us(
            state.last_media_end_pts_us,
            audio.pts_us,
            arrangement.total_gap_us,
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
            // 隙間の音は予定を持たない (時間軸へ実績を記録しない)
            state
                .position
                .record(start_samples, pts_us, frames as i64, None);
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
        state.record_error(&arrangement, now_us);
        return;
    }
    state.position.record(
        start_samples,
        audio.pts_us,
        frames as i64,
        Some(PlannedAudio {
            target_us: arrangement.target_start_us,
            basis: arrangement.basis,
            decoded_arrival_us: audio.decoded_us,
        }),
    );
    // 最初に積んだ音の開始時刻を控える。メインループはこの時刻に達したら play() を呼ぶ。
    // それまでのフレームは再生機器の内部キューに先行バッファとして溜まる
    if state.playback_start_at_us.is_none() {
        state.playback_start_at_us = Some(arrangement.start_at_us);
    }
    // 次の隙間の PTS に使う、今回の音の媒体終端。詰めた分を引かないのは、この音が表現
    // している媒体上の範囲の終端だからである
    state.last_media_end_pts_us = Some(audio.pts_us.saturating_add(arrangement.duration_us));

    // 積んだ直後に process を呼び、実際に出力へ渡った長さを測れるようにする。呼ばないと
    // 自分の未 flush 分が「積んだまま鳴っていない長さ」に混ざり、実出力の先行量が過大になる
    if let Err(e) = audio_player.process() {
        tracing::warn!("Failed to process audio queue: {e}");
    }
    // 実出力の先行量を測って時間軸へ渡す。時間軸はこれを音声の予算へ入れる。まだ再生を
    // 開始していない間は、積んだ分がすべて先行バッファであり予算と無関係なので観測しない
    if state.started {
        let stats = audio_player.stats();
        if let Ok(stats_sample_rate) = u32::try_from(stats.sample_rate)
            && let Some(lead_us) = output_lead_us(
                stats.total_samples_enqueued,
                stats.total_samples_played,
                stats_sample_rate,
                state.scheduler.last_end_us(),
                now_us,
            )
        {
            timeline.observe_audio_output_lead(lead_us, now_us);
        }
    }
    // 実際に適用した詰め・伸長と、実際に積めた隙間の長さを計器へ反映する
    state.commit(audio, &arrangement, applied, gap_enqueued, now_us);
}

/// いま鳴っている音の PTS と予定を実績として時間軸へ記録する
///
/// `raw_player` は積んだ順にサンプルを鳴らすため、鳴らし終えたサンプル数と「積んだ時点の
/// 累積サンプル数」の対応から、いま鳴っているサンプルの PTS が求まる。予定時刻ではなく
/// 実際の再生位置を使うことで、`skew_us` が計画ではなく実測になる。
///
/// 時間軸へは、実際に鳴っている壁時計 (`presented`) と、その音を予約したときの開始予定に
/// 鳴っている位置の差を足した時刻 (`target`) を組にして渡す。両者の差が、予約の予定から
/// 実際の出力までの残差になり、映像の合わせ先に使われる。予定は積んだ時点の値であり、
/// ここで timeline に問い直さない。chunk ごとに 1 回だけ記録する。
///
/// 到着基準で並べた音は、実際に鳴り始めた壁時計から復号の出力が揃った時刻を引いた深さを
/// 実測し、時間軸へ渡す。これは映像を音声の到着基準へ合わせるために使う。
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
    let Ok(sample_rate) = u32::try_from(stats.sample_rate) else {
        return;
    };
    let Some(sounding) = state.next_sounding_to_record(played_samples, sample_rate) else {
        return;
    };
    // 隙間の音は実績にしない (予定も到着も持たない)
    let Some(planned) = sounding.planned else {
        return;
    };
    let presented_us = wall_clock_us();
    if planned.basis == AudioPlayoutBasis::Arrival {
        let depth_us = presented_us.saturating_sub(planned.decoded_arrival_us);
        timeline.observe_audio_arrival_delay(Some(depth_us), planned.decoded_arrival_us);
        state.last_arrival_depth_us = Some(depth_us);
    }
    if let Some(target_us) = planned.target_us {
        timeline.record_presentation_with_target(
            Track::Audio,
            sounding.pts_us,
            presented_us,
            Some(target_us),
        );
    }
}

/// スケジューラが決めた詰め・伸長・隙間の材料化を音声へ適用した結果
struct AppliedAudioPlayout {
    /// 最終的に今回の音として積む波形 (チャンネルごと、f32)
    channels: Vec<Vec<f32>>,
    /// 材料化した隙間の音 (f32 interleaved)。隙間が無いときは空
    gap_channels: Vec<f32>,
    /// 補間できた長さ (マイクロ秒)。補間できなければ 0
    concealed_us: i64,
    /// 詰めた長さ (マイクロ秒)。詰めていないときは 0
    compressed_us: i64,
    /// 伸ばした長さ (マイクロ秒)。伸ばしていないときは 0
    expanded_us: i64,
}

/// スケジューラが決めた詰め・伸長・隙間の材料化を音声へ適用する (再生機器には触れない)
///
/// 順序は moqt-js の `playSound` と同じである。まず `compress_us` まで波形の周期 1 つ分を
/// 削って今回の音を詰める。削れる長さは周期で決まり、要求された長さとは一致しないため、
/// 実際に削れた長さを `compressed_us` として返す。次に `expand_us` まで直前の実音を
/// 重ねて周期 1 つ分ずつ伸ばし、実際に伸びた長さを `expanded_us` として返す。
///
/// 隙間が無いのに媒体時刻が飛んだ音は、直前の音の末尾へ先頭を接続する。最後に
/// `total_gap_us` の隙間を [`gap::materialize`] で全長ぶん材料化する。補間できなかった分は
/// 無音で埋まり、隙間の末尾と今回の音の先頭はフェードで繋がる。`concealed_us` は補間できた
/// 長さである。
///
/// `played_history` は直前に鳴らした実音の末尾 ([`AUDIO_HISTORY_US`] 以上) であり、隙間の
/// 材料化の `previous` と時間伸長の `history` へそのまま渡す。1 フレーム (約 20 ms) だけを
/// 渡すと、20 ms で周期補間できなかったときの「30 ms の履歴で再試行する」分岐が走らず、
/// 線形予測の解析窓も 20 ms に縮む。
///
/// moqt-rs の出力は FIFO であり、隙間の全長を先頭から連続して材料化して積めば、音は
/// 詰まらずに鳴る。そのため moqt-js の「予約できる最も早い時刻より前」の前置き
/// (`unrecoverable_gap_us`) は材料化の置き場所には使わない。計器へ記録するときにだけ使う。
#[expect(clippy::too_many_arguments)]
fn apply_audio_playout(
    audio: &DecodedAudioFrame,
    played_history: Option<&PlayedAudioHistory>,
    last_decoded: Option<&DecodedAudioHistory>,
    compress_us: i64,
    expand_us: i64,
    total_gap_us: i64,
    conceal_us: i64,
    media_gap_us: i64,
) -> AppliedAudioPlayout {
    let channels = usize::from(audio.channels);
    let sample_rate = audio.sample_rate;
    // 履歴はサンプルレートとチャンネル数が一致するときだけ使える
    let usable_history = played_history.filter(|history| is_history_usable(history, audio));
    // stretch は f32 のチャンネルごとの波形を扱う。S16 interleaved から直して分ける
    let mut channel_pcm = pcm_f32_to_channels(&pcm_i16_to_f32(&audio.pcm), channels);
    // 1. 時間圧縮。削ってよい長さの上限は、目標に対して遅れている分までにする。周期が
    //    これを超える音では削らない。削ると行き過ぎて「早すぎる」側へ振れ、次の音でまた
    //    遅れて詰める往復になり、往復のたびに周期が 1 つ消える
    let mut compressed_us = 0;
    if compress_us > 0 {
        let used_frames = channel_pcm.first().map_or(0, Vec::len);
        let mut channel_slices: Vec<&mut [f32]> = channel_pcm
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let removed = stretch::compress(
            &mut channel_slices,
            sample_rate,
            us_to_samples(compress_us, sample_rate),
        );
        if removed < 0 {
            // 削った分だけ後ろを詰めた状態で使う範囲が短くなる
            let removed_frames = removed.unsigned_abs().min(used_frames);
            compressed_us = samples_to_us(removed_frames, sample_rate);
            let remaining_frames = used_frames - removed_frames;
            for channel in &mut channel_pcm {
                channel.truncate(remaining_frames);
            }
        }
    }
    // 2. 時間伸長。履歴には直前に鳴らした実音の末尾 30 ms を渡す。媒体時刻が 1 マイクロ秒
    //    以下でつながっているときだけ渡す (離れている音を重ねると継ぎ目が崩れる)
    let mut expanded_us = 0;
    if expand_us > 0 {
        let history: &[Vec<f32>] = match usable_history {
            Some(history) if media_gap_us.saturating_abs() <= 1 => &history.channels,
            _ => &[],
        };
        let expansion = stretch::preemptively_expand(
            &channel_pcm,
            history,
            sample_rate,
            us_to_samples(expand_us, sample_rate),
        );
        if expansion.added_samples > 0 && !expansion.channels.is_empty() {
            expanded_us = samples_to_us(expansion.added_samples, sample_rate);
            channel_pcm = expansion.channels;
        }
    }
    // 3. 隙間を材料化していないのに媒体時刻が飛んだときは、直前の音の末尾へ先頭を接続する
    let duration_us = pcm_duration_us(audio.pcm.len(), channels, sample_rate);
    let missing_media_gap_us = if media_gap_us > 1 { media_gap_us } else { 0 };
    if should_smooth_splice(total_gap_us, missing_media_gap_us, duration_us)
        && let Some(last_decoded) = last_decoded
        && last_decoded.sample_rate == sample_rate
        && last_decoded.channels.len() == channel_pcm.len()
    {
        let previous: Vec<&[f32]> = last_decoded
            .channels
            .iter()
            .map(|channel| channel.as_slice())
            .collect();
        let current: Vec<&[f32]> = channel_pcm
            .iter()
            .map(|channel| channel.as_slice())
            .collect();
        channel_pcm = splice_fade::smooth_splice_head(&previous, &current, sample_rate);
    }
    // 4. 隙間を材料化する。previous には直前に鳴らした実音の末尾 30 ms を渡す。返る音は
    //    要求した全長 (total_gap_us) であり、補間できなかった分は無音で埋まる。今回の音の
    //    先頭にはフェードインが掛かる
    let mut gap_channels = Vec::new();
    let mut concealed_us = 0;
    if total_gap_us > 0 && channels > 0 {
        let mut channel_slices: Vec<&mut [f32]> = channel_pcm
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let materialized = gap::materialize(
            usable_history.map(|history| history.channels.as_slice()),
            &mut channel_slices,
            sample_rate,
            total_gap_us,
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
        expanded_us,
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

/// 復号の出力に鳴らせるサンプルがあるか
///
/// 空の PCM (AAC の priming など) や、チャンネル数・サンプルレートが 0 のフレームは
/// 鳴らせない。時間軸へ観測し予約へ渡すと、基準の学習と計器が壊れるため、その前に弾く。
fn has_audio_frames(audio: &DecodedAudioFrame) -> bool {
    if audio.channels == 0 || audio.sample_rate == 0 {
        return false;
    }
    audio.pcm.len() / usize::from(audio.channels) > 0
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
    use shiguredo_moqt::playout::expansion;

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

    /// 描画差分 1 で、先頭のフレームの TIMESTAMP と投入からの遅れが返ること
    #[test]
    fn video_accounting_returns_the_rendered_frame_and_its_delay() {
        let mut accounting = VideoPlayoutAccounting::default();
        // プレイヤーを作った直後 (実績 0) を基準にする
        accounting.reset_for_player(5, 0, 0, 0);
        // 2 枚投入し、先頭の 1 枚が 10 ms 後に描画された
        accounting.record_enqueue(Some(1_000), 0);
        accounting.record_enqueue(Some(2_000), 5_000);
        let observation = accounting.observe(1, 0, 10_000);
        let rendered: Vec<(Option<i64>, u64)> = observation
            .rendered
            .iter()
            .map(|frame| (frame.timestamp_us, frame.enqueue_delay_us))
            .collect();
        assert_eq!(
            rendered,
            vec![(Some(1_000), 10_000)],
            "描画された先頭フレームの TIMESTAMP と、投入から描画までの遅れが返ること"
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

    /// 破棄差分で FIFO の先頭が取り除かれ、描画として返らないこと
    #[test]
    fn video_accounting_removes_the_dropped_frame() {
        let mut accounting = VideoPlayoutAccounting::default();
        accounting.reset_for_player(5, 0, 0, 0);
        accounting.record_enqueue(Some(1_000), 0);
        accounting.record_enqueue(Some(2_000), 5_000);
        // 先頭の 1 枚が破棄された。破棄されたフレームは描画されない
        let observation = accounting.observe(0, 1, 10_000);
        assert!(
            observation.rendered.is_empty(),
            "破棄だけでは描画されたフレームが返らないこと"
        );
        // 次の観測で描画されたのは、破棄された先頭ではなく 2 枚目
        let observation = accounting.observe(1, 1, 20_000);
        let rendered: Vec<(Option<i64>, u64)> = observation
            .rendered
            .iter()
            .map(|frame| (frame.timestamp_us, frame.enqueue_delay_us))
            .collect();
        assert_eq!(
            rendered,
            vec![(Some(2_000), 15_000)],
            "破棄されたフレームの次のフレームが描画として返ること"
        );
    }

    /// 投入時にキュー上限を超えて捨てた分を、破棄差分から二重に取り除かないこと
    #[test]
    fn video_accounting_subtracts_the_enqueue_drop() {
        let mut accounting = VideoPlayoutAccounting::default();
        // 上限 2 のキューに 3 枚投入すると、3 枚目の投入で先頭が捨てられる
        accounting.reset_for_player(2, 0, 0, 0);
        accounting.record_enqueue(Some(1_000), 0);
        accounting.record_enqueue(Some(2_000), 5_000);
        accounting.record_enqueue(Some(3_000), 10_000);
        // プレイヤーは投入時に先頭を 1 枚捨てる。FIFO は既に取り除いているため、
        // ここでさらに取り除くと描画されたフレームの TIMESTAMP を取り違える
        let observation = accounting.observe(0, 1, 10_000);
        assert!(
            observation.rendered.is_empty(),
            "破棄だけでは描画されたフレームが返らないこと"
        );
        let observation = accounting.observe(1, 1, 20_000);
        let rendered: Vec<Option<i64>> = observation
            .rendered
            .iter()
            .map(|frame| frame.timestamp_us)
            .collect();
        assert_eq!(
            rendered,
            vec![Some(2_000)],
            "キューに残った先頭のフレームが描画として返ること"
        );
    }

    /// 壁時計の TIMESTAMP を持たないフレームも、投入から描画までの遅れを数えること
    #[test]
    fn video_accounting_measures_the_delay_of_a_frame_without_a_timestamp() {
        let mut accounting = VideoPlayoutAccounting::default();
        accounting.reset_for_player(5, 0, 0, 0);
        accounting.record_enqueue(None, 1_000);
        let observation = accounting.observe(1, 0, 4_000);
        let rendered: Vec<(Option<i64>, u64)> = observation
            .rendered
            .iter()
            .map(|frame| (frame.timestamp_us, frame.enqueue_delay_us))
            .collect();
        assert_eq!(
            rendered,
            vec![(None, 3_000)],
            "TIMESTAMP が無くても投入からの遅れは返ること"
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
        accounting.record_enqueue(Some(1_000), 0);
        accounting.record_enqueue(Some(2_000), 5_000);
        // 解像度が変わってプレイヤーを作り直した。新しいプレイヤーの実績は 0 から始まる
        accounting.reset_for_player(5, 0, 0, 50_000);
        // 作り直す前のフレームは対応付けに残らない
        accounting.record_enqueue(Some(3_000), 50_000);
        let observation = accounting.observe(1, 0, 60_000);
        let rendered: Vec<(Option<i64>, u64)> = observation
            .rendered
            .iter()
            .map(|frame| (frame.timestamp_us, frame.enqueue_delay_us))
            .collect();
        assert_eq!(
            rendered,
            vec![(Some(3_000), 10_000)],
            "作り直した後に描画されたのは、作り直した後に投入したフレームであること"
        );
    }

    /// 描画が 1 秒未満しか止まっていなければ停止と判定しないこと
    #[test]
    fn video_accounting_does_not_detect_a_stall_before_the_timeout() {
        let mut accounting = VideoPlayoutAccounting::default();
        accounting.reset_for_player(5, 0, 0, 0);
        accounting.record_enqueue(Some(1_000), 0);
        accounting.record_enqueue(Some(2_000), 5_000);
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
        accounting.record_enqueue(Some(1_000), 0);
        accounting.record_enqueue(Some(2_000), 5_000);
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
        accounting.record_enqueue(Some(1_000), 0);
        accounting.record_enqueue(Some(2_000), 5_000);
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
        accounting.record_enqueue(Some(3_000), 3_000_000);
        let observation = accounting.observe(1, 0, 3_100_000);
        let rendered: Vec<(Option<i64>, u64)> = observation
            .rendered
            .iter()
            .map(|frame| (frame.timestamp_us, frame.enqueue_delay_us))
            .collect();
        assert_eq!(
            rendered,
            vec![(Some(3_000), 100_000)],
            "復帰後に描画されたのは復帰後に投入したフレームであること"
        );
    }

    /// 描画を観測したフレームだけを時間軸へ記録し、残差を集計すること
    ///
    /// 推定表示時刻ではなく、描画を観測した時刻をそのまま記録する。TIMESTAMP の無い
    /// フレームは時間軸へ記録しないが、投入から描画までの遅れは数える。
    #[test]
    fn record_observed_video_frames_records_only_the_observed_frames() {
        let mut timeline = PlayoutTimeline::new();
        let arrival_us = 10_000_000;
        let timestamp_us = 1_000_000;
        timeline.observe(Track::Video, arrival_us, timestamp_us);
        // 時間軸が決めた表示時刻との差を、記録する前の値で求める
        let expected_us = timeline
            .present_us(Track::Video, timestamp_us)
            .expect("映像の TIMESTAMP を観測したため表示時刻が決まること");
        let observed_us = arrival_us + 2_000;
        let mut enqueue_delays_us = SampleWindow::default();
        let mut presentation_residuals_us = SampleWindow::default();
        record_observed_video_frames(
            vec![
                RenderedVideoFrame {
                    timestamp_us: Some(timestamp_us),
                    enqueue_delay_us: 3_000,
                },
                RenderedVideoFrame {
                    timestamp_us: None,
                    enqueue_delay_us: 4_000,
                },
            ],
            observed_us,
            &mut timeline,
            &mut enqueue_delays_us,
            &mut presentation_residuals_us,
        );
        assert_eq!(
            enqueue_delays_us.len(),
            2,
            "壁時計の TIMESTAMP の有無によらず、投入から描画までの遅れを数えること"
        );
        assert_eq!(
            enqueue_delays_us
                .percentiles()
                .expect("標本があること")
                .max_us,
            4_000,
            "投入から描画までの遅れの最大値を返すこと"
        );
        assert_eq!(
            presentation_residuals_us.len(),
            1,
            "TIMESTAMP の無いフレームは表示時刻の残差を数えないこと"
        );
        assert_eq!(
            presentation_residuals_us
                .percentiles()
                .expect("標本があること")
                .p50_us,
            observed_us - expected_us,
            "記録した表示時刻と時間軸が決めた表示時刻との差を残差にすること"
        );
        // 描画を観測していない (推定しか無い) ときは何も記録しない
        record_observed_video_frames(
            Vec::new(),
            observed_us,
            &mut timeline,
            &mut enqueue_delays_us,
            &mut presentation_residuals_us,
        );
        assert_eq!(
            enqueue_delays_us.len(),
            2,
            "描画の観測が無ければ投入から描画までの遅れを足さないこと"
        );
        assert_eq!(
            presentation_residuals_us.len(),
            1,
            "描画の観測が無ければ表示の実績を記録しないこと"
        );
    }

    /// 標本が無ければ分布を返さないこと
    #[test]
    fn sample_window_returns_none_when_empty() {
        let window = SampleWindow::default();
        assert!(
            window.percentiles().is_none(),
            "標本が無ければ None を返すこと"
        );
        assert_eq!(
            percentile_log(None),
            "none",
            "標本が無ければ none と出すこと"
        );
    }

    /// 標本が 1 件なら p50 / p95 / max はすべてその値になること
    #[test]
    fn sample_window_returns_the_same_value_for_one_sample() {
        let mut window = SampleWindow::default();
        window.push(1_500);
        let percentiles = window.percentiles().expect("標本があること");
        assert_eq!(
            (percentiles.p50_us, percentiles.p95_us, percentiles.max_us),
            (1_500, 1_500, 1_500),
            "標本が 1 件ならすべて同じ値になること"
        );
    }

    /// 標本が複数なら昇順に並べて p50 / p95 / max を求めること
    #[test]
    fn sample_window_returns_the_percentiles_for_multiple_samples() {
        let mut window = SampleWindow::default();
        for value in [5_000, 1_000, 3_000, 2_000, 4_000] {
            window.push(value);
        }
        let percentiles = window.percentiles().expect("標本があること");
        assert_eq!(
            (percentiles.p50_us, percentiles.p95_us, percentiles.max_us),
            (3_000, 4_000, 5_000),
            "並べ替えた上で p50 / p95 / max を求めること"
        );
        assert_eq!(
            percentile_log(Some(percentiles)),
            "p50=3.0ms p95=4.0ms max=5.0ms",
            "ログ用の文字列はミリ秒で出すこと"
        );
    }

    /// 表示の選択の計器が、提示しなかった数と遅れて提示した回数を数えること
    #[test]
    fn video_selection_counters_count_dropped_and_late_frames() {
        let mut counters = VideoSelectionCounters::default();
        counters.record(0, false);
        assert_eq!(
            (counters.not_shown_frames, counters.draw_late_count),
            (0, 0),
            "何も捨てず遅れも無ければ数えないこと"
        );
        counters.record(2, true);
        assert_eq!(
            (counters.not_shown_frames, counters.draw_late_count),
            (2, 1),
            "提示しなかったフレーム数と、遅れて提示した回数を数えること"
        );
        counters.record(1, false);
        assert_eq!(
            (counters.not_shown_frames, counters.draw_late_count),
            (3, 1),
            "累計へ足し、遅れていない回は遅れに数えないこと"
        );
    }

    /// tick 間隔が無ければ表示周期は初期値になること
    #[test]
    fn estimate_refresh_us_returns_the_default_for_no_samples() {
        assert_eq!(
            estimate_refresh_us(&VecDeque::new()),
            VIDEO_DEFAULT_REFRESH_US,
            "サンプルが無ければ初期値を使うこと"
        );
    }

    /// 飛んだ tick が混ざっても、下位 25% は通常の間隔を拾うこと
    #[test]
    fn estimate_refresh_us_uses_the_lower_quartile() {
        let mut intervals = VecDeque::new();
        for _ in 0..15 {
            intervals.push_back(1_000);
        }
        // メインスレッドが詰まって 1 回飛んだ tick (2 倍の間隔) を混ぜる
        intervals.push_back(2_000);
        assert_eq!(
            estimate_refresh_us(&intervals),
            1_000,
            "2 倍に伸びた 1 回に引っ張られず、通常の間隔を拾うこと"
        );
    }

    /// 表示周期が tick 間隔から求まり、目標表示時刻と許容幅がその値から決まること
    #[test]
    fn video_display_period_tracks_the_tick_intervals() {
        let mut display = VideoDisplayPeriod::new();
        assert_eq!(
            display.observe_tick(10_000_000),
            VIDEO_DEFAULT_REFRESH_US,
            "最初の tick は初期値を使うこと"
        );
        assert_eq!(
            display.observe_tick(10_000_800),
            VIDEO_DEFAULT_REFRESH_US,
            "サンプルが 2 つ未満の間は初期値のままにすること"
        );
        assert_eq!(
            display.observe_tick(10_001_600),
            800,
            "2 つの tick 間隔から表示周期を求めること"
        );
        assert_eq!(
            display.target_us(10_001_600),
            10_001_600 + 800,
            "目標表示時刻は tick + 提示遅延 (1 tick) になること"
        );
        assert_eq!(
            display.tolerance_us(),
            400,
            "許容幅は表示周期の半分にすること"
        );
        assert_eq!(
            display.max_late_us(),
            MAX_PRESENTATION_LAG_US + 800,
            "遅れの上限は max(20 ms, 表示周期) + 提示遅延にすること"
        );
    }

    /// 4 tick 分を超える間隔では表示周期を初期値へ戻すこと
    #[test]
    fn video_display_period_resets_after_a_long_interval() {
        let mut display = VideoDisplayPeriod::new();
        display.observe_tick(0);
        display.observe_tick(1_000);
        display.observe_tick(2_000);
        assert_eq!(
            display.observe_tick(2_800),
            800,
            "通常の間隔では表示周期を更新すること"
        );
        // 4 ms を超える間隔 (ループの停止) ではサンプルを捨てて初期値へ戻す
        assert_eq!(
            display.observe_tick(10_000),
            VIDEO_DEFAULT_REFRESH_US,
            "4 tick を超える間隔で初期値へ戻すこと"
        );
        assert_eq!(
            display.target_us(10_000),
            11_000,
            "初期値の 1 tick 後ろを目標表示時刻にすること"
        );
    }

    /// 0 以下の tick 間隔は表示周期の推定に使わないこと
    #[test]
    fn video_display_period_ignores_non_positive_intervals() {
        let mut display = VideoDisplayPeriod::new();
        display.observe_tick(10_000);
        assert_eq!(
            display.observe_tick(10_000),
            VIDEO_DEFAULT_REFRESH_US,
            "同じ時刻の tick は間隔に数えないこと"
        );
        assert_eq!(
            display.observe_tick(9_999),
            VIDEO_DEFAULT_REFRESH_US,
            "巻き戻った tick は間隔に数えないこと"
        );
        assert_eq!(
            display.observe_tick(10_999),
            VIDEO_DEFAULT_REFRESH_US,
            "有効なサンプルが 2 つ未満の間は初期値のままにすること"
        );
    }

    /// 表示周期が 20 ms より長いときは、遅れて提示してよい上限を広げること
    #[test]
    fn video_display_period_widens_the_max_late_for_a_slow_refresh() {
        let display = VideoDisplayPeriod {
            last_tick_us: None,
            intervals_us: VecDeque::new(),
            refresh_us: 33_000,
        };
        assert_eq!(
            display.tolerance_us(),
            16_500,
            "許容幅は表示周期の半分にすること"
        );
        assert_eq!(
            display.max_late_us(),
            33_000 + 33_000,
            "30 fps では表示周期を下限にして 2 tick まで遅れを許すこと"
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
    /// 隙間の先頭は直前の音の末尾へ連続し、今回の音は短くならない。直前の音は本番と同じく
    /// 30 ms の履歴として渡す。
    #[test]
    fn apply_audio_playout_materializes_the_gap_from_the_previous_tail() {
        let sample_rate = 48_000;
        // 400 Hz は 48 kHz で 120 サンプル周期であり、末尾の周期がそのまま見つかる
        let period = 120;
        let previous = periodic_audio(400.0, sample_rate, 1_440);
        let previous_tail = previous[1_440 - 1];
        let history = PlayedAudioHistory {
            channels: vec![previous],
            sample_rate,
            media_end_pts_us: 0,
        };
        let audio = audio_frame(960, sample_rate);
        let applied = apply_audio_playout(&audio, Some(&history), None, 0, 0, 20_000, 20_000, 0);
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
        assert_eq!(applied.compressed_us, 0, "詰めは要求されていないこと");
        assert_eq!(applied.expanded_us, 0, "伸長は要求されていないこと");
        // 直前の音の末尾と隙間の先頭が連続していること
        assert!(
            (applied.gap_channels[0] - previous_tail).abs() < 0.1,
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
        let applied = apply_audio_playout(&audio, None, None, 0, 0, 10_000, 10_000, 0);
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
        let applied = apply_audio_playout(&audio, None, None, 0, 0, 0, 0, 0);
        assert!(
            applied.gap_channels.is_empty(),
            "隙間が無いときは材料化しないこと"
        );
    }

    /// 要求された長さだけ時間伸長すること
    ///
    /// 再同期で浅くなった到着基準の深さを戻すため、直前の実音を重ねて 1 周期ぶん伸ばす。
    /// 伸ばした長さはマイクロ秒で返し、実際に積む波形も長くなる。履歴には本番と同じく
    /// 直前の実音の末尾 30 ms を渡す。
    #[test]
    fn apply_audio_playout_expands_the_sound() {
        let sample_rate = 48_000;
        let audio = audio_frame(960, sample_rate);
        // 直前の実音 (400 Hz、30 ms) を履歴として渡し、媒体時刻がつながっている状態にする
        let history = PlayedAudioHistory {
            channels: vec![periodic_audio(400.0, sample_rate, 1_440)],
            sample_rate,
            media_end_pts_us: 0,
        };
        let applied = apply_audio_playout(&audio, Some(&history), None, 0, 5_000, 0, 0, 0);
        assert!(
            applied.expanded_us > 0,
            "周期のある音では要求された長さの範囲で伸びること expanded_us={}",
            applied.expanded_us
        );
        assert!(
            applied.expanded_us <= 5_000,
            "要求した長さを超えて伸ばさないこと expanded_us={}",
            applied.expanded_us
        );
        assert!(
            applied.channels[0].len() > 960,
            "伸びた分だけ今回の音が長くなること length={}",
            applied.channels[0].len()
        );
        assert_eq!(
            applied.compressed_us, 0,
            "伸長と圧縮は同時に要求されないこと"
        );
        // 履歴のサンプルレートが違うときは伸ばさない
        let mismatched = PlayedAudioHistory {
            channels: vec![periodic_audio(400.0, sample_rate, 1_440)],
            sample_rate: 16_000,
            media_end_pts_us: 0,
        };
        let applied = apply_audio_playout(&audio, Some(&mismatched), None, 0, 5_000, 0, 0, 0);
        assert_eq!(
            applied.expanded_us, 0,
            "履歴のサンプルレートが違う音は伸長に使わないこと"
        );
    }

    /// 隙間が無く媒体時刻が大きく飛んだとき、直前の音の末尾へ先頭を接続すること
    ///
    /// 予約に穴が無いまま媒体時刻が飛ぶと、波形の継ぎ目が段差になる。直前の音の末尾から
    /// 2 ms かけて今回の音へ移ることで、段差を聴感上消す。
    #[test]
    fn apply_audio_playout_splices_the_head_when_the_media_gap_is_large() {
        let sample_rate = 48_000;
        let audio = audio_frame(960, sample_rate);
        let original = pcm_f32_to_channels(&pcm_i16_to_f32(&audio.pcm), 1);
        // 400 Hz の音は先頭が 0 である。直前の音の末尾を 0.8 にしておくと、続けて鳴らすと
        // 0.8 から 0 へ落ちる段差になる
        let history = DecodedAudioHistory {
            channels: vec![vec![0.8f32; 480]],
            sample_rate,
        };
        // 音の長さの半分 (10 ms) 以上の媒体時刻の飛びがあり、予約の隙間は無い
        let applied = apply_audio_playout(&audio, None, Some(&history), 0, 0, 0, 0, 15_000);
        assert!(
            (applied.channels[0][0] - 0.8).abs() < 1e-6,
            "先頭が直前の音の末尾から始まること first={}",
            applied.channels[0][0]
        );
        assert_eq!(
            applied.channels[0][96], original[0][96],
            "接続フェードの後ろは元の波形のままであること"
        );
        // 媒体時刻の飛びが小さいときは接続しない
        let applied = apply_audio_playout(&audio, None, Some(&history), 0, 0, 0, 0, 1_000);
        assert_eq!(
            applied.channels[0][0], original[0][0],
            "媒体時刻の飛びが小さいときは先頭を接続しないこと"
        );
    }

    /// 接続フェードを掛ける条件が「隙間なし」と「半フレーム以上の飛び」であること
    #[test]
    fn should_smooth_splice_requires_an_uncovered_media_gap() {
        assert!(
            should_smooth_splice(0, 10_000, 20_000),
            "隙間が無く、音の長さの半分の飛びがあれば接続すること"
        );
        assert!(
            !should_smooth_splice(10_000, 10_000, 20_000),
            "隙間を材料化するときは接続しないこと"
        );
        assert!(
            !should_smooth_splice(0, 9_999, 20_000),
            "音の長さの半分未満の飛びでは接続しないこと"
        );
        assert!(
            should_smooth_splice(0, 12_000, 20_000),
            "音の長さの半分を超える飛びでは接続すること"
        );
    }

    /// 実音の履歴が末尾 30 ms だけを保つこと
    ///
    /// 空の入力、ちょうど 30 ms、30 ms を超える入力、サンプルレート変更、チャンネル数変更、
    /// 媒体時刻の飛びの境界を確かめる。
    #[test]
    fn push_history_keeps_the_last_thirty_milliseconds() {
        let sample_rate = 48_000;
        let max_us = AUDIO_HISTORY_US;
        // 30 ms は 48 kHz で 1,440 サンプル、20 ms は 960 サンプルである
        assert_eq!(history_sample_count(max_us, sample_rate), 1_440);
        assert_eq!(history_sample_count(20_000, sample_rate), 960);
        assert_eq!(history_sample_count(0, sample_rate), 0);
        assert_eq!(history_sample_count(max_us, 0), 0);

        // 空の音は履歴を作らない
        let mut history = None;
        push_history(&mut history, &[], sample_rate, 0, 20_000, max_us);
        assert!(history.is_none(), "空の音では履歴を作らないこと");

        // 30 ms 未満の音はそのまま残る
        let first = vec![vec![0.1f32; 960]];
        push_history(&mut history, &first, sample_rate, 0, 20_000, max_us);
        let kept = history.as_ref().expect("履歴を作ること");
        assert_eq!(kept.channels[0].len(), 960, "30 ms 未満はそのまま残ること");
        assert_eq!(kept.media_end_pts_us, 20_000, "媒体終端を覚えること");

        // さらに 20 ms を足すと、末尾 30 ms だけが残る (先頭 10 ms が落ちる)
        let second = vec![vec![0.2f32; 960]];
        push_history(&mut history, &second, sample_rate, 20_000, 40_000, max_us);
        let kept = history.as_ref().expect("履歴を更新すること");
        assert_eq!(kept.channels[0].len(), 1_440, "末尾 30 ms に切り詰めること");
        assert!(
            (kept.channels[0][0] - 0.1).abs() < f32::EPSILON,
            "残る先頭は前の音の 10 ms 目であること"
        );
        assert!(
            (kept.channels[0][479] - 0.1).abs() < f32::EPSILON,
            "前の音の末尾まで残ること"
        );
        assert!(
            (kept.channels[0][480] - 0.2).abs() < f32::EPSILON,
            "そのあとは後に足した音であること"
        );

        // サンプルレートが変わったら捨てる
        push_history(
            &mut history,
            &[vec![0.3f32; 480]],
            16_000,
            40_000,
            70_000,
            max_us,
        );
        let kept = history
            .as_ref()
            .expect("新しいサンプルレートで作り直すこと");
        assert_eq!(kept.sample_rate, 16_000, "新しいサンプルレートになること");
        assert_eq!(kept.channels[0].len(), 480, "前の履歴を引き継がないこと");

        // チャンネル数が変わったら捨てる
        let mut history = None;
        push_history(
            &mut history,
            &[vec![0.1f32; 480]],
            sample_rate,
            0,
            20_000,
            max_us,
        );
        push_history(
            &mut history,
            &[vec![0.2f32; 480], vec![0.2f32; 480]],
            sample_rate,
            20_000,
            40_000,
            max_us,
        );
        let kept = history
            .as_ref()
            .expect("チャンネル数が変わったら作り直すこと");
        assert_eq!(kept.channels.len(), 2, "新しいチャンネル数になること");

        // 媒体時刻が飛んだら捨てる
        let mut history = None;
        push_history(
            &mut history,
            &[vec![0.1f32; 960]],
            sample_rate,
            0,
            20_000,
            max_us,
        );
        push_history(
            &mut history,
            &[vec![0.2f32; 960]],
            sample_rate,
            40_000,
            60_000,
            max_us,
        );
        let kept = history.as_ref().expect("媒体時刻が飛んだら作り直すこと");
        assert_eq!(
            kept.channels[0].len(),
            960,
            "媒体時刻が飛んだら前の履歴を捨てること"
        );
        assert!(
            (kept.channels[0][0] - 0.2).abs() < f32::EPSILON,
            "飛んだあとの音だけを残すこと"
        );
    }

    /// 30 ms の履歴があると、20 ms では周期補間できなかった音を履歴で再試行できること
    ///
    /// 90 Hz は 48 kHz で 533.3 サンプル周期である。20 ms (960 サンプル) の末尾では探索
    /// できる最大のずらし幅 (4 kHz で 41 サンプル = 492 サンプル) より長く、周期を推定
    /// できない。30 ms (1,440 サンプル) なら探索範囲 (60 サンプル = 720 サンプル) に入る
    /// ため、履歴を使った再試行で周期補間ができる。
    #[test]
    fn expand_audio_retries_with_the_thirty_millisecond_history() {
        let sample_rate = 48_000;
        let history = periodic_audio(90.0, sample_rate, 1_440);
        let current = &history[960..];
        // 20 ms だけでは周期補間できない。履歴を渡すと 30 ms で再試行して周期補間する
        let without_history = expansion::expand_audio(&[current], sample_rate, None)
            .expect("20 ms の音は補間の対象であること");
        let with_history =
            expansion::expand_audio(&[current], sample_rate, Some(&[history.as_slice()]))
                .expect("30 ms の履歴を渡しても補間できること");
        // 30 ms の履歴に対する周期補間 (conceal) と一致することを確かめる
        let mut reference = [vec![0.0f32; 4_800]];
        let mut destination: Vec<&mut [f32]> = reference
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let filled = stretch::conceal(
            &[history.as_slice()],
            &mut destination,
            sample_rate,
            100_000,
        );
        assert!(filled > 0, "30 ms の履歴では周期補間できること");
        let difference = with_history[0]
            .iter()
            .zip(&reference[0])
            .map(|(with_history, reference)| (with_history - reference).abs())
            .fold(0.0f32, f32::max);
        assert!(
            difference < 1e-3,
            "履歴を使った補間が 30 ms の周期補間と一致すること difference={difference}"
        );
        // 履歴が無いときは周期補間にならず、結果が変わる
        let difference = without_history[0]
            .iter()
            .zip(&with_history[0])
            .map(|(without_history, with_history)| (without_history - with_history).abs())
            .fold(0.0f32, f32::max);
        assert!(
            difference > 0.05,
            "20 ms だけのときは線形予測になり、履歴を使った補間と変わること difference={difference}"
        );
    }

    /// 隙間の材料化へ渡す履歴が 30 ms あると、20 ms では補間できなかった音を補間できること
    ///
    /// 90 Hz の音では 20 ms の履歴だけでは周期補間できず線形予測になる。30 ms の履歴を
    /// 渡すと周期補間になるため、材料化した隙間の音が変わる。
    #[test]
    fn apply_audio_playout_uses_the_thirty_millisecond_history_for_the_gap() {
        let sample_rate = 48_000;
        let long = periodic_audio(90.0, sample_rate, 1_440);
        let short = long[960..].to_vec();
        let audio = audio_frame(960, sample_rate);
        let long_history = PlayedAudioHistory {
            channels: vec![long],
            sample_rate,
            media_end_pts_us: 0,
        };
        let short_history = PlayedAudioHistory {
            channels: vec![short],
            sample_rate,
            media_end_pts_us: 0,
        };
        let applied_long =
            apply_audio_playout(&audio, Some(&long_history), None, 0, 0, 20_000, 20_000, 0);
        let applied_short =
            apply_audio_playout(&audio, Some(&short_history), None, 0, 0, 20_000, 20_000, 0);
        assert_eq!(
            applied_long.gap_channels.len(),
            960,
            "30 ms の履歴でも隙間を材料化すること"
        );
        assert_eq!(
            applied_short.gap_channels.len(),
            960,
            "20 ms の履歴でも隙間を材料化すること"
        );
        let difference = applied_long
            .gap_channels
            .iter()
            .zip(&applied_short.gap_channels)
            .map(|(long, short)| (long - short).abs())
            .fold(0.0f32, f32::max);
        assert!(
            difference > 0.05,
            "30 ms の履歴では周期補間、20 ms では線形予測になり、隙間の音が変わること difference={difference}"
        );
    }

    /// 積んだ位置の対応から、いま鳴っているサンプルの PTS と予定が求まること
    #[test]
    fn audio_playout_position_follows_the_played_samples() {
        let mut position = AudioPlayoutPosition::new();
        // 0 サンプル目から 20 ms (48 kHz で 960 サンプル) の音を PTS 1_000_000、
        // 予約した開始予定 1_100_000 で積む
        position.record(
            0,
            1_000_000,
            960,
            Some(PlannedAudio {
                target_us: Some(1_100_000),
                basis: AudioPlayoutBasis::Timestamp,
                decoded_arrival_us: 1_000_000,
            }),
        );
        let sounding = position
            .sounding(0, 48_000)
            .expect("まだ鳴っていなくても先頭の値が分かること");
        assert_eq!(
            sounding.pts_us, 1_000_000,
            "まだ鳴っていなければ積んだ先頭の PTS になること"
        );
        assert_eq!(
            sounding
                .planned
                .as_ref()
                .and_then(|planned| planned.target_us),
            Some(1_100_000),
            "まだ鳴っていなければ予約した開始予定になること"
        );
        let sounding = position
            .sounding(480, 48_000)
            .expect("480 サンプル鳴った位置が分かること");
        assert_eq!(
            sounding.pts_us, 1_010_000,
            "480 サンプル (10 ms) 鳴れば PTS も 10 ms 進むこと"
        );
        assert_eq!(
            sounding
                .planned
                .as_ref()
                .and_then(|planned| planned.target_us),
            Some(1_110_000),
            "予定も鳴っている位置の分だけ進むこと"
        );
        // 次の音を続けて積む
        position.record(
            960,
            1_020_000,
            960,
            Some(PlannedAudio {
                target_us: Some(1_120_000),
                basis: AudioPlayoutBasis::Timestamp,
                decoded_arrival_us: 1_020_000,
            }),
        );
        assert_eq!(
            position
                .sounding(960, 48_000)
                .map(|sounding| sounding.pts_us),
            Some(1_020_000),
            "次の音の先頭の PTS になること"
        );
        assert_eq!(
            position.sounding(1_440, 48_000).map(|sounding| (
                sounding.pts_us,
                sounding
                    .planned
                    .as_ref()
                    .and_then(|planned| planned.target_us)
            )),
            Some((1_030_000, Some(1_130_000))),
            "次の音の途中も PTS と予定が追従すること"
        );
        assert!(
            position.sounding(1_920, 48_000).is_none(),
            "すべて鳴り終えたら分からないこと"
        );
    }

    /// 予定を持たない音 (隙間) では予定が None になること
    #[test]
    fn audio_playout_position_reports_no_plan_for_a_gap() {
        let mut position = AudioPlayoutPosition::new();
        // 隙間の音は開始予定を持たない
        position.record(0, 1_000_000, 480, None);
        let sounding = position
            .sounding(240, 48_000)
            .expect("隙間の音でも鳴っている位置は分かること");
        assert_eq!(sounding.pts_us, 1_005_000, "PTS は進むこと");
        assert!(
            sounding.planned.is_none(),
            "予定を持たない音は予定を返さないこと"
        );
    }

    /// 再生位置が戻ったら対応を捨ててやり直すこと
    #[test]
    fn audio_playout_position_clears_when_the_play_position_goes_back() {
        let mut position = AudioPlayoutPosition::new();
        position.record(
            5_000,
            2_000_000,
            960,
            Some(PlannedAudio {
                target_us: Some(2_100_000),
                basis: AudioPlayoutBasis::Arrival,
                decoded_arrival_us: 2_000_000,
            }),
        );
        // デバイスが作り直され、鳴らし終えたサンプル数が積んだ範囲より前へ戻った
        assert!(
            position.sounding(0, 48_000).is_none(),
            "対応が切れたら分からないこと"
        );
        // 捨てたあとは次の積み込みからやり直す
        position.record(
            0,
            3_000_000,
            960,
            Some(PlannedAudio {
                target_us: Some(3_100_000),
                basis: AudioPlayoutBasis::Arrival,
                decoded_arrival_us: 3_000_000,
            }),
        );
        assert_eq!(
            position.sounding(0, 48_000).map(|sounding| (
                sounding.pts_us,
                sounding
                    .planned
                    .as_ref()
                    .and_then(|planned| planned.target_us)
            )),
            Some((3_000_000, Some(3_100_000))),
            "積み直した位置から求まること"
        );
    }

    /// chunk が次へ進んだときだけ時間軸へ記録すること
    ///
    /// 鳴っているサンプルは同じ chunk の中で動き続けるため、毎ループ記録すると同じ音を
    /// 何度も上書きしてしまう。
    #[test]
    fn sounding_audio_is_recorded_once_per_chunk() {
        let mut assembly = AudioPlayoutAssembly::new();
        assembly.position.record(
            0,
            1_000_000,
            960,
            Some(PlannedAudio {
                target_us: Some(1_100_000),
                basis: AudioPlayoutBasis::Timestamp,
                decoded_arrival_us: 1_000_000,
            }),
        );
        let first = assembly
            .next_sounding_to_record(480, 48_000)
            .expect("最初は記録すること");
        let planned = first.planned.as_ref().expect("予定を持つ音であること");
        assert_eq!(first.pts_us, 1_010_000);
        assert_eq!(planned.target_us, Some(1_110_000));
        assert!(
            assembly.next_sounding_to_record(600, 48_000).is_none(),
            "同じ chunk の間は記録しないこと"
        );
        // 次の音へ進んだら記録する
        assembly.position.record(
            960,
            1_020_000,
            960,
            Some(PlannedAudio {
                target_us: Some(1_120_000),
                basis: AudioPlayoutBasis::Timestamp,
                decoded_arrival_us: 1_020_000,
            }),
        );
        let second = assembly
            .next_sounding_to_record(1_100, 48_000)
            .expect("次の chunk へ進んだら記録すること");
        assert_eq!(second.pts_us, 1_022_917);
        assert_eq!(
            second
                .planned
                .as_ref()
                .and_then(|planned| planned.target_us),
            Some(1_122_917)
        );
    }

    /// 実出力の先行量が、積んだまま鳴っていない長さから予約の先行量を引いた残差になること
    #[test]
    fn output_lead_measures_the_committed_lead() {
        // 積んだまま鳴っていない長さ = (4_800 - 960) サンプル = 80 ms、予約の先行量 = 40 ms
        assert_eq!(
            output_lead_us(4_800, 960, 48_000, Some(10_040_000), 10_000_000),
            Some(40_000),
            "積んだまま鳴っていない長さから予約の先行量を引いた残差を返すこと"
        );
        // 予約の終端より手前までしか積んでいなければ、残差は 0 である
        assert_eq!(
            output_lead_us(1_440, 960, 48_000, Some(10_010_000), 10_000_000),
            Some(0),
            "予約の先行量が積んだ長さを超えるときは 0 にすること"
        );
        // 鳴り終えた数が積んだ数を超えても負にしない
        assert_eq!(
            output_lead_us(960, 4_800, 48_000, Some(10_000_000), 10_000_000),
            Some(0),
            "鳴り終えた数が積んだ数を超えても負にしないこと"
        );
        assert_eq!(
            output_lead_us(4_800, 960, 0, Some(10_040_000), 10_000_000),
            None,
            "サンプルレートが 0 なら測れないこと"
        );
        assert_eq!(
            output_lead_us(4_800, 960, 48_000, None, 10_000_000),
            None,
            "まだ 1 音も並べていなければ測れないこと"
        );
    }

    /// drain の待ち時間が、残りの予約と余裕から決まり上限で切られること
    #[test]
    fn audio_drain_requests_the_remaining_reservation() {
        let now_us = 10_000_000;
        assert_eq!(
            drain_requested_us(Some(now_us + 50_000), now_us),
            150_000,
            "残りの予約に余裕 (100 ms) を足した時間を待つこと"
        );
        assert_eq!(
            drain_requested_us(None, now_us),
            100_000,
            "予約が無ければ余裕だけを待ち時間にすること"
        );
        assert_eq!(
            drain_requested_us(Some(now_us + 1_000_000), now_us),
            300_000,
            "上限 (300 ms) で切ること"
        );
        assert_eq!(
            drain_requested_us(Some(now_us - 50_000), now_us),
            100_000,
            "既に鳴り終えていれば余裕だけを待つこと"
        );
    }

    /// drain が予約の鳴り終わりか待ち時間の上限で終わること
    #[test]
    fn audio_drain_finishes_on_the_reservation_or_the_timeout() {
        let started_us = 10_000_000;
        assert!(
            drain_finished(
                started_us + 20_000,
                started_us,
                150_000,
                Some(10_010_000),
                960,
                960,
            ),
            "予約の終端を過ぎ、出力キューも空なら終えること"
        );
        assert!(
            !drain_finished(
                started_us + 20_000,
                started_us,
                150_000,
                Some(10_100_000),
                960,
                960,
            ),
            "予約の終端がまだ先なら終えないこと"
        );
        assert!(
            !drain_finished(
                started_us + 20_000,
                started_us,
                150_000,
                Some(10_010_000),
                1_920,
                960,
            ),
            "出力キューが空でなければ終えないこと"
        );
        assert!(
            drain_finished(
                started_us + 150_000,
                started_us,
                150_000,
                Some(10_500_000),
                1_920,
                960,
            ),
            "待ち時間の上限に達したら終えること"
        );
    }

    /// TIMESTAMP の種類が壁時計のときだけ時間軸へ渡すこと
    #[test]
    fn wall_clock_timestamp_is_used_only_for_wall_clock_frames() {
        let frame = |timestamp_kind: TimestampKind, timestamp_us: Option<i64>| DecodedVideoFrame {
            y: Vec::new(),
            u: Vec::new(),
            v: Vec::new(),
            width: 0,
            height: 0,
            timestamp_us,
            timestamp_kind,
        };
        assert_eq!(
            wall_clock_timestamp(&frame(TimestampKind::WallClock, Some(1_000))),
            Some(1_000),
            "壁時計の TIMESTAMP はそのまま使うこと"
        );
        assert_eq!(
            wall_clock_timestamp(&frame(TimestampKind::MediaTime, Some(1_000))),
            None,
            "メディア時刻の TIMESTAMP は時間軸へ渡さないこと"
        );
        assert_eq!(
            wall_clock_timestamp(&frame(TimestampKind::None, None)),
            None,
            "TIMESTAMP が無ければ渡さないこと"
        );
        assert_eq!(
            wall_clock_timestamp(&frame(TimestampKind::WallClock, None)),
            None,
            "壁時計の種類でも TIMESTAMP が無ければ渡さないこと"
        );

        let mut audio = audio_frame(960, 48_000);
        audio.pts_us = 1_234_567;
        assert_eq!(
            wall_clock_pts(&audio),
            Some(1_234_567),
            "壁時計の TIMESTAMP を持つ音は PTS を使うこと"
        );
        audio.timestamp_kind = TimestampKind::MediaTime;
        assert_eq!(
            wall_clock_pts(&audio),
            None,
            "メディア時刻の音は時間軸へ渡さないこと"
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

    /// 計器が未補間の隙間とアンダーランの枯れた長さを数えること
    ///
    /// 隙間は補間で埋まらなかった長さ (uncovered) で数え、予約できなかった前置きは
    /// unrecoverable として別に数える。アンダーランは枯れている間の検出 (毎ループ) では
    /// なく、枯れが明けたときに 1 回だけ数える。
    #[test]
    fn playout_assembly_counts_the_uncovered_gaps_and_the_underruns() {
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        let now_us = 10_000_000;
        timeline.observe(Track::Audio, now_us, 0);
        let audio = audio_frame(960, 48_000);
        // 隙間の全長は 20 ms、補間できたのは 10 ms、予約できなかった前置きは 5 ms という
        // 組み立てを反映する
        let arrangement = AudioPlayoutArrangement {
            start_at_us: now_us + 10_000,
            basis: AudioPlayoutBasis::Timestamp,
            duration_us: 20_000,
            target_start_us: Some(now_us + 10_000),
            arrival_us: now_us,
            compress_us: 0,
            expand_us: 0,
            conceal_us: 10_000,
            total_gap_us: 20_000,
            unrecoverable_gap_us: 5_000,
        };
        let applied = AppliedAudioPlayout {
            channels: pcm_f32_to_channels(&pcm_i16_to_f32(&audio.pcm), 1),
            gap_channels: vec![0.0; 480],
            concealed_us: 10_000,
            compressed_us: 0,
            expanded_us: 0,
        };
        assembly.commit(&audio, &arrangement, applied, true, now_us);
        let snapshot = assembly.snapshot(now_us);
        assert_eq!(snapshot.gaps, 1, "隙間を材料化したことを数えること");
        assert_eq!(
            snapshot.gap_total_us, 10_000,
            "補間で埋まらなかった長さだけを合計すること"
        );
        assert_eq!(
            snapshot.gap_max_us, 10_000,
            "補間で埋まらなかった最長の長さを記録すること"
        );
        assert_eq!(
            snapshot.unrecoverable_gaps, 1,
            "予約できなかった前置きを数えること"
        );
        assert_eq!(
            snapshot.unrecoverable_gap_total_us, 5_000,
            "予約できなかった前置きの長さを合計すること"
        );

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
        // 続けて枯れが無いループで呼んでも二重に数えない
        assembly.end_underrun(10_041_000);
        assert_eq!(
            assembly.snapshot(now_us).underruns,
            1,
            "枯れていないときに呼んで二重に数えないこと"
        );
    }

    /// テスト用のモノラルの音声フレームを作る (S16 interleaved)
    ///
    /// TIMESTAMP は壁時計として扱う。テストは時間軸へ観測して予算を学ばせるためである。
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
            timestamp_kind: TimestampKind::WallClock,
        }
    }

    /// スケジューラの決めた隙間と詰めが音声へ適用されること
    ///
    /// 実際のスケジューラに決定させた値で組み立てる。再生機器は使わない。
    #[test]
    fn playout_applies_the_gap_and_the_compression() {
        let sample_rate = 48_000;
        let mut scheduler = AudioPlayoutScheduler::new();
        // 直前の音 (30 ms) を鳴らし終えた状態にする
        let previous = PlayedAudioHistory {
            channels: vec![periodic_audio(400.0, sample_rate, 1_440)],
            sample_rate,
            media_end_pts_us: 0,
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
            stretch_min_us: None,
        });
        assert_eq!(
            first,
            AudioPlayoutDecision::Play {
                start_at_us: 1_000_000,
                basis: AudioPlayoutBasis::Timestamp,
                compress_us: 0,
                expand_us: 0,
                gap_start_us: None,
                conceal_us: 0,
                total_gap_us: 0,
                unrecoverable_gap_us: 0,
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
            stretch_min_us: None,
        });
        let AudioPlayoutDecision::Play {
            compress_us: 0,
            conceal_us,
            total_gap_us,
            unrecoverable_gap_us,
            ..
        } = second
        else {
            panic!("2 つ目は鳴ると決まること decision={second:?}");
        };
        assert_eq!(
            total_gap_us, 10_000,
            "1 つ目の終わりから 10 ms の隙間が空くこと"
        );
        assert_eq!(conceal_us, 10_000, "隙間は補間してよい長さであること");
        assert_eq!(
            unrecoverable_gap_us, 0,
            "予約できる時刻より前の前置きは無いこと"
        );
        let audio = audio_frame(960, sample_rate);
        let applied = apply_audio_playout(
            &audio,
            Some(&previous),
            None,
            0,
            0,
            total_gap_us,
            conceal_us,
            0,
        );
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
            stretch_min_us: None,
        });
        let AudioPlayoutDecision::Play {
            compress_us,
            total_gap_us,
            ..
        } = third
        else {
            panic!("3 つ目は鳴ると決まること decision={third:?}");
        };
        assert!(compress_us > 0, "目標を過ぎた分だけ詰めること");
        assert_eq!(
            total_gap_us, 0,
            "前の音の終わりへ繋がるため隙間は空かないこと"
        );
        let applied = apply_audio_playout(&audio, Some(&previous), None, compress_us, 0, 0, 0, 0);
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
        let previous = PlayedAudioHistory {
            channels: vec![periodic_audio(400.0, sample_rate, 1_440)],
            sample_rate,
            media_end_pts_us: 0,
        };
        let audio = audio_frame(960, sample_rate);
        let applied =
            apply_audio_playout(&audio, Some(&previous), None, 10_000, 0, 10_000, 10_000, 0);
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
        let mut channels =
            pcm_f32_to_channels(&pcm_i16_to_f32(&audio.pcm), usize::from(audio.channels));
        let removed_samples = us_to_samples(compressed_us, audio.sample_rate);
        for channel in &mut channels {
            let remaining = channel.len().saturating_sub(removed_samples);
            channel.truncate(remaining);
        }
        AppliedAudioPlayout {
            channels,
            gap_channels: Vec::new(),
            concealed_us: 0,
            compressed_us,
            expanded_us: 0,
        }
    }

    /// テスト用に、時間軸が決めた目標の時刻へ 1 つ鳴らすと予約して計器へ記録する
    ///
    /// 再生機器を使わないため、詰めも伸長も隙間の材料化も無し (`compressed_us` と
    /// `expanded_us` は 0) として反映する。
    fn commit_one_play(
        assembly: &mut AudioPlayoutAssembly,
        timeline: &mut PlayoutTimeline,
        audio: &DecodedAudioFrame,
        now_us: i64,
    ) {
        let arrangement = assembly
            .arrange(timeline, audio, now_us, now_us, true)
            .expect("目標の時刻に鳴ると決まること");
        let applied = applied_audio(audio, 0);
        assembly.scheduler.confirm_stretch(applied.compressed_us);
        assembly.scheduler.confirm_expansion(applied.expanded_us);
        assembly.commit(audio, &arrangement, applied, false, now_us);
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
        // 繋がるため、今から鳴らせる最も早い時刻ではなくその終わりへずれる。ずれは 20 ms で
        // あり、音の長さの半分 (10 ms) とキューの余裕 (10 ms) の小さい方の 10 ms だけ詰める
        // 要求が出る
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
        let applied = applied_audio(&audio, 5_000);
        assembly.scheduler.confirm_stretch(applied.compressed_us);
        assembly.scheduler.confirm_expansion(applied.expanded_us);
        assembly.commit(&audio, &arrangement, applied, false, target_us);
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
            "予定からどれだけ過ぎて鳴るかを記録すること"
        );
    }

    /// 時間圧縮と補間を使えないサンプルレートの音だけを計器へ数えること
    ///
    /// 96 kHz は 4 kHz へ間引くフィルタの表に無く、時間圧縮も補間も働かない。落ちずに
    /// 「操作しない」を返すため、数えることで気付けるようにする。表にある 48 kHz の音は
    /// 数えない (44.1 kHz は解析用の換算に対応したため使える)。数えるだけで鳴らし方
    /// (予約が返るか) を変えない。
    #[test]
    fn playout_assembly_counts_only_the_stretch_unavailable_sample_rates() {
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        let arrival_us = 10_000_000;
        // 到着の記録は受信側 (run_raw_player) が行う
        timeline.observe(Track::Audio, arrival_us, 0);
        // 96 kHz の音 (10 ms で 960 サンプル) を受け取ると数える
        let audio = audio_frame(960, 96_000);
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

    /// 時間軸から音声の予算と実出力の先行量を読めること
    ///
    /// 予算は到着の揺らぎと実出力の先行量から決まり、50〜100 ms に切られる。学習が済んで
    /// いない間は下限 (50 ms) になる。
    #[test]
    fn playout_assembly_reports_the_audio_delay_budget() {
        let mut timeline = PlayoutTimeline::new();
        timeline.observe(Track::Audio, 10_000_000, 0);
        let stats = timeline.audio_delay_stats();
        assert_eq!(
            stats.jitter_us, None,
            "まだ揺らぎを学習していなければ None であること"
        );
        assert_eq!(
            stats.target_us, 50_000,
            "学習が済むまでは予算の下限 (50 ms) になること"
        );
        // 実出力の先行量を 30 ms として観測する
        timeline.observe_audio_output_lead(30_000, 10_010_000);
        let stats = timeline.audio_delay_stats();
        assert_eq!(
            stats.output_lead_us,
            Some(30_000),
            "実出力の先行量が予算の内訳に入ること"
        );
        assert_eq!(
            stats.target_us, 50_000,
            "揺らぎが無ければ 30 ms は下限へ切り上げられること"
        );
    }

    /// 並べすぎで鳴らさないと決めた音を、理由付きで計器へ渡すこと
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
        assembly.record_error(&arrangement, now_us);
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
        assembly.record_stopped(now_us);
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
        assembly.record_stopped(play.start_at_us + 10_000);
        assert_eq!(
            assembly.snapshot(now_us).missed_by_reason.stopped,
            AudioMissTotal {
                count: 1,
                duration_us: 10_000,
            },
            "鳴り始めている音は残りの長さだけを数えること"
        );
    }

    /// 再同期後の伸長を確定し、実際に積んだ長さで予約の終端を確定すること
    ///
    /// 到着基準で再同期した直後は、遅れて到着した音を伸ばして深さを戻す。実際に伸ばした
    /// 長さを返し、積んだ波形の長さで予約の終端を確定する。
    #[test]
    fn playout_assembly_confirms_the_expansion_and_the_playout_end() {
        let sample_rate = 48_000;
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        // 音声だけの購読として、目標の時刻ではなく到着基準で並ばせる
        assembly.set_audio_only(true);
        let first_arrival_us = 10_000_000;
        let audio = audio_frame(960, sample_rate);
        assembly
            .arrange(
                &mut timeline,
                &audio,
                first_arrival_us,
                first_arrival_us,
                true,
            )
            .expect("1 音目は到着基準で鳴ると決まること");
        // 2 音目は到着が遅れ、到着基準へ再同期したうえで伸長を要求する
        let second_arrival_us = first_arrival_us + 160_000;
        let mut second = audio_frame(960, sample_rate);
        second.pts_us = 20_000;
        let arrangement = assembly
            .arrange(
                &mut timeline,
                &second,
                second_arrival_us,
                second_arrival_us,
                true,
            )
            .expect("2 音目も到着基準で鳴ると決まること");
        assert!(
            arrangement.expand_us > 0,
            "再同期の直後は伸長を要求すること expand_us={}",
            arrangement.expand_us
        );
        // 実際に 2.5 ms (120 サンプル) 伸びたものとして確定する
        let mut applied = applied_audio(&second, 0);
        for channel in &mut applied.channels {
            channel.extend(std::iter::repeat_n(0.0f32, 120));
        }
        applied.expanded_us = 2_500;
        assembly.scheduler.confirm_stretch(applied.compressed_us);
        assembly.scheduler.confirm_expansion(applied.expanded_us);
        let expected_end_us = arrangement.start_at_us
            + samples_to_us(applied.channels.first().map_or(0, Vec::len), sample_rate);
        assembly.commit(&second, &arrangement, applied, false, second_arrival_us);
        assert_eq!(
            assembly.scheduler.expanded_us(),
            2_500,
            "実際に伸ばした長さを累積へ反映すること"
        );
        assert_eq!(
            assembly.scheduler.last_end_us(),
            Some(expected_end_us),
            "実際に積んだ長さで予約の終端を確定すること"
        );
    }

    /// 計器と時間軸の値がログの文字列に出ること
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
            "audio_only=",
            "jitter_ms=",
            "output_lead_ms=",
            "target_ms=",
            "deadband_ms=",
            "expanded_ms=",
            "gaps=",
            "gap_ms=",
            "gap_max_ms=",
            "unrecoverable_gaps=",
            "unrecoverable_gap_ms=",
            "underruns=",
            "underrun_ms=",
            "arrival_depth_ms=",
            "last_target_us=",
            "last_arrival_us=",
            "last_start_us=",
            "slack_p50_p95=",
            "start_delay_p50_p95=",
            "lateness_p50_p95=",
            "missed_backlog=",
            "missed_stopped=",
            "audio_playout_correction=",
            "drain=",
        ] {
            assert!(line.contains(key), "ログに {key} が出ること line={line}");
        }
    }

    /// 音声だけの購読では、ログにその状態と 50 ms の予算が出ること
    #[test]
    fn audio_playout_log_reports_the_audio_only_budget() {
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        assembly.set_audio_only(true);
        let now_us = 10_000_000;
        timeline.observe(Track::Audio, now_us, 0);
        let audio = audio_frame(960, 48_000);
        commit_one_play(&mut assembly, &mut timeline, &audio, now_us);
        let line = assembly.log_fields(&timeline, now_us);
        assert!(
            line.contains("audio_only=true"),
            "音声だけの購読であることがログに出ること line={line}"
        );
        assert!(
            line.contains("target_ms=50"),
            "音声だけの購読でも予算の下限 (50 ms) を使うこと line={line}"
        );
        assert!(
            line.contains("deadband_ms=20"),
            "揺らぎの学習が済むまでは既定の不感帯 (20 ms) を使うこと line={line}"
        );
    }

    /// 音声だけの購読では、到着基準の再生の遅れを 50 ms に固定すること
    ///
    /// 映像に合わせる相手がいないため、揺らぎと先行量から学習した値ではなく、固定の
    /// 50 ms を使う。
    #[test]
    fn playout_assembly_uses_the_audio_only_arrival_floor() {
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        assembly.set_audio_only(true);
        let arrival_us = 10_000_000;
        timeline.observe(Track::Audio, arrival_us, 0);
        let audio = audio_frame(960, 48_000);
        let arrangement = assembly
            .arrange(&mut timeline, &audio, arrival_us, arrival_us, false)
            .expect("音声だけの購読でも鳴ると決まること");
        assert_eq!(
            arrangement.start_at_us, 10_050_000,
            "到着 + 50 ms から鳴らすこと (学習した値ではなく下限を使う)"
        );
        let play = assembly
            .scheduler
            .last_play()
            .expect("鳴らすと決めた音が残ること");
        assert_eq!(
            play.basis,
            AudioPlayoutBasis::Arrival,
            "音声だけの購読では到着基準で並べること"
        );
    }

    /// 音声の到着の揺らぎと実出力の先行量を時間軸へ学習させる
    ///
    /// 100 ms 間隔で 60 音を観測する。オフセット (到着の壁時計 − TIMESTAMP) を 0 と 40 ms で
    /// 交互に振り、基準 (10 秒窓の最小値) から 40 ms の揺らぎを作る。最後に実出力の先行量を
    /// 30 ms として観測する。
    fn learn_audio_delay(timeline: &mut PlayoutTimeline) -> i64 {
        let start_us = 10_000_000;
        let interval_us = 100_000;
        let base_offset_us = 10_000_000;
        let jitter_us = 40_000;
        for index in 0..60i64 {
            let arrival_us = start_us + index * interval_us;
            let offset_us = base_offset_us + if index % 2 == 0 { 0 } else { jitter_us };
            timeline.observe(Track::Audio, arrival_us, arrival_us - offset_us);
        }
        timeline.observe_audio_output_lead(30_000, start_us + 59 * interval_us);
        timeline
            .playout_delay_us()
            .expect("揺らぎと先行量を学習したので予算が決まること")
    }

    /// 映像を購読しているときは、揺らぎと先行量から学習した予算で目標の時刻を守ること
    #[test]
    fn playout_assembly_uses_the_learned_audio_delay() {
        let mut timeline = PlayoutTimeline::new();
        // 揺らぎ 40 ms と実出力の先行量 30 ms から、予算は 70 ms になる
        let learned_us = learn_audio_delay(&mut timeline);
        assert_eq!(
            learned_us, 70_000,
            "揺らぎと先行量を足した予算になること actual={learned_us}"
        );
        let mut assembly = AudioPlayoutAssembly::new();
        let audio = audio_frame(960, 48_000);
        let now_us = 10_000_000;
        let arrangement = assembly
            .arrange(&mut timeline, &audio, now_us, now_us, true)
            .expect("目標の時刻に鳴ると決まること");
        assert_eq!(
            arrangement.basis,
            AudioPlayoutBasis::Timestamp,
            "映像を購読しているときは目標の時刻を守ること"
        );
        assert_eq!(
            arrangement.target_start_us,
            Some(10_070_000),
            "TIMESTAMP の基準の遅れ (10 s) に予算 (70 ms) を足した時刻が目標になること"
        );
        assert_eq!(
            arrangement.start_at_us, 10_070_000,
            "目標の時刻から鳴らすこと"
        );
    }

    /// 音声だけの購読では、目標の時刻を守らず到着基準で並べること
    #[test]
    fn playout_assembly_does_not_enforce_the_target_for_audio_only() {
        let arrival_us = 10_000_000;
        let audio = audio_frame(960, 48_000);

        // 映像を購読しているときは目標の時刻を守る
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        timeline.observe(Track::Audio, arrival_us, 0);
        let arrangement = assembly
            .arrange(&mut timeline, &audio, arrival_us, arrival_us, true)
            .expect("映像を購読していれば鳴ると決まること");
        assert_eq!(
            assembly
                .scheduler
                .last_play()
                .expect("鳴らすと決めた音が残ること")
                .basis,
            AudioPlayoutBasis::Timestamp,
            "映像を購読しているときは目標の時刻を守ること"
        );
        assert_eq!(
            arrangement.start_at_us, 10_050_000,
            "目標の時刻 (基準の遅れ + 予算の下限) から鳴らすこと"
        );

        // 音声だけの購読では到着基準で並べる
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        assembly.set_audio_only(true);
        timeline.observe(Track::Audio, arrival_us, 0);
        let arrangement = assembly
            .arrange(&mut timeline, &audio, arrival_us, arrival_us, true)
            .expect("音声だけの購読でも鳴ると決まること");
        assert_eq!(
            assembly
                .scheduler
                .last_play()
                .expect("鳴らすと決めた音が残ること")
                .basis,
            AudioPlayoutBasis::Arrival,
            "音声だけの購読では目標の時刻を守らないこと"
        );
        assert_eq!(
            arrangement.start_at_us, 10_050_000,
            "到着 + 50 ms の下限から鳴らすこと"
        );
    }

    /// 到着基準の最初の音が、到着 (今) から予算の 50 ms 後ろへ置かれること
    ///
    /// scheduler の `arrival_us` は `now_us` と同じ壁時計の軸である。到着の基準に媒体の
    /// PTS を渡すと基準と表示の遅れの分だけ過去の値になり、最初の音が最小の余裕 (10 ms)
    /// まで押し上げられる。到着基準の遅れ・余裕・予定からの遅れが予算 (50 ms) と整合する
    /// ことを確かめる。
    #[test]
    fn playout_assembly_places_the_arrival_based_sound_by_the_budget() {
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        assembly.set_audio_only(true);
        let now_us = 10_000_000;
        timeline.observe(Track::Audio, now_us, 0);
        let audio = audio_frame(960, 48_000);
        commit_one_play(&mut assembly, &mut timeline, &audio, now_us);
        let play = assembly
            .scheduler
            .last_play()
            .expect("鳴らすと決めた音が残ること");
        assert_eq!(
            play.basis,
            AudioPlayoutBasis::Arrival,
            "音声だけの購読では到着基準で並べること"
        );
        assert_eq!(
            play.arrival_us, now_us,
            "到着の基準が今の壁時計であること (媒体の PTS ではない)"
        );
        assert_eq!(
            play.start_at_us,
            now_us + 50_000,
            "到着から予算の 50 ms 後ろへ置くこと (最小の余裕 10 ms へ押し上げられない)"
        );
        let snapshot = assembly.snapshot(now_us);
        assert_eq!(
            snapshot.last_slack_us,
            Some(50_000),
            "予定に対する余裕が到着の予算 (50 ms) と一致すること"
        );
        assert_eq!(
            snapshot.last_start_delay_us,
            Some(50_000),
            "到着から鳴り始めるまでが到着の予算 (50 ms) と一致すること"
        );
        assert_eq!(
            snapshot.last_lateness_us,
            Some(0),
            "到着基準の予算どおりに並ぶため、予定からの遅れは 0 であること"
        );
    }

    /// 履歴には実際に鳴らした実音だけを残し、材料化した隙間の音は入れないこと
    #[test]
    fn playout_assembly_does_not_remember_the_materialized_gap() {
        let mut timeline = PlayoutTimeline::new();
        let mut assembly = AudioPlayoutAssembly::new();
        let now_us = 10_000_000;
        timeline.observe(Track::Audio, now_us, 0);
        let audio = audio_frame(960, 48_000);
        let arrangement = assembly
            .arrange(&mut timeline, &audio, now_us, now_us, true)
            .expect("目標の時刻に鳴ると決まること");
        // 実音 20 ms と、材料化した隙間の音 (目印として -123.0 を入れる) を渡す
        let applied = AppliedAudioPlayout {
            channels: pcm_f32_to_channels(&pcm_i16_to_f32(&audio.pcm), 1),
            gap_channels: vec![-123.0f32; 480],
            concealed_us: 0,
            compressed_us: 0,
            expanded_us: 0,
        };
        assembly.commit(&audio, &arrangement, applied, true, now_us);
        let history = assembly
            .real_audio_history
            .as_ref()
            .expect("実音を履歴へ残すこと");
        assert_eq!(history.channels[0].len(), 960, "実音 20 ms だけを残すこと");
        assert_eq!(
            history.media_end_pts_us, 20_000,
            "実音の媒体終端を履歴の終端にすること"
        );
        assert!(
            history.channels[0].iter().all(|sample| *sample != -123.0),
            "材料化した隙間の音を履歴へ入れないこと"
        );
    }

    /// 鳴らせない復号出力を、時間軸へ観測する前に失敗として数えること
    #[test]
    fn empty_decoded_audio_is_rejected_before_observation() {
        let now_us = 10_000_000;
        let audio = audio_frame(960, 48_000);
        assert!(has_audio_frames(&audio), "通常の音声は鳴らせること");

        let mut empty = audio_frame(960, 48_000);
        empty.pcm.clear();
        assert!(!has_audio_frames(&empty), "空の PCM は鳴らせないこと");

        let mut no_channels = audio_frame(960, 48_000);
        no_channels.channels = 0;
        assert!(
            !has_audio_frames(&no_channels),
            "チャンネル数 0 は鳴らせないこと"
        );

        let mut no_rate = audio_frame(960, 48_000);
        no_rate.sample_rate = 0;
        assert!(
            !has_audio_frames(&no_rate),
            "サンプルレート 0 は鳴らせないこと"
        );

        // 空の復号出力は、長さ 0 の失敗として件数だけを記録する
        let mut assembly = AudioPlayoutAssembly::new();
        assembly.record_invalid_decode(now_us);
        let snapshot = assembly.snapshot(now_us);
        assert_eq!(
            snapshot.played_frames, 0,
            "鳴らせない音を鳴らしたことにしないこと"
        );
        assert_eq!(snapshot.missed_frames, 1, "鳴らせない音を計上すること");
        assert_eq!(
            snapshot.missed_by_reason.error,
            AudioMissTotal {
                count: 1,
                duration_us: 0,
            },
            "長さが分からない失敗として理由別に数えること"
        );
    }
}
