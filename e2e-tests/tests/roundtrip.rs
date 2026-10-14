//! relay を介した publish / subscribe の往復 E2E テスト
//!
//! 実 relay に publisher と subscriber を接続し、publisher が配信した映像 (stream 配送) と
//! 音声 (datagram 配送) が subscriber に届いてデコードされ、MP4 に保存されるところまでを
//! 検証する。接続先は環境変数 `MOQT_E2E_URL` で渡す。Track Namespace は `--namespace` では
//! なく `--url` の MSF fragment (`#msf:...`) から渡し、`--namespace` を省略したときの解決も
//! 併せて検証する。
//!
//! 実 relay が必要なため、通常の `cargo test` では実行しない (`#[ignore]` を付けている)。
//! `MOQT_E2E_URL` を設定したうえで `cargo test -p e2e-tests -- --ignored` を実行する。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use tokio_utils::ShutdownController;

/// テスト中のログを有効にする
///
/// CI で失敗したときに、publisher と subscriber のどちらで何が起きたかを追えるようにする。
/// `RUST_LOG` で絞り込める。既定は `info`。
fn init_tracing() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_test_writer()
            .try_init();
    });
}

/// publisher と subscriber の起動を待つ上限
const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// subscriber を起動し直す間隔
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// 1 回の起動試行で最初の映像フレームを待つ時間
///
/// 映像 Group の長さ (既定 2 秒) より長くしないと、最初の group の位相によっては
/// フレームが届かない。
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(4);
/// 映像を受け取り続ける時間
const STREAM_DURATION: Duration = Duration::from_secs(5);
/// 受け取り続けたとみなす最小フレーム数 (30 fps の 1 秒分)
const MIN_FRAMES: u64 = 30;
/// 受け取り続けたとみなす最小の音声フレーム数
///
/// publisher は 20 ms ごとに 1 Object を送るため、5 秒で 250 程度になる。relay の負荷を
/// 見込んで 1 秒分を下限にする。
const MIN_AUDIO_FRAMES: u64 = 50;
/// publisher が配信する音声のサンプルレート (moq-pub の既定値)
const AUDIO_SAMPLE_RATE: u32 = 48_000;
/// publisher が配信する音声のチャンネル数 (moq-pub の既定値)
const AUDIO_CHANNELS: u8 = 1;
/// 配信の終了間際まで届いたとみなす、最後の受信からの許容時間
///
/// 受信の間隔 (500 ms) と 30 fps のフレーム間隔 (33 ms) に対して十分な余裕を取る。
const LAST_FRAME_TIMEOUT: Duration = Duration::from_secs(1);
/// shutdown とタスクの終了を待つ上限
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);
/// publisher が生成する映像のサイズ (moq-pub の既定値)
const VIDEO_SIZE: (i32, i32) = (1280, 720);
/// MSF fragment の track-identifier に使う track name
///
/// publisher の `--track-name` の既定値と同じ値を使う。subscriber は catalog track を
/// SUBSCRIBE して video / audio を SUBSCRIBE する (Joining FETCH は draft-20 で削除された
/// ため発行しない) ので、fragment の track name は使わない。
const MSF_TRACK_NAME: &str = "video";

/// 接続先 URL を環境変数から取り出す
///
/// CI では secrets が未設定のときに空文字が渡るため、空文字も未設定として扱う。
fn e2e_url() -> String {
    match std::env::var("MOQT_E2E_URL") {
        Ok(value) if !value.is_empty() => value,
        _ => panic!(
            "MOQT_E2E_URL を設定してください (例: MOQT_E2E_URL=moqt://<HOST>/ cargo test -p e2e-tests -- --ignored)"
        ),
    }
}

/// トランスポート種別を環境変数から取り出す (既定 quic)
///
/// CI では vars が未設定のときに空文字が渡るため、空文字も未設定として扱う。
fn e2e_transport() -> String {
    match std::env::var("MOQT_E2E_TRANSPORT") {
        Ok(value) if !value.is_empty() => value,
        _ => "quic".to_string(),
    }
}

/// 実行ごとに一意な Track Namespace を作る
///
/// 同じ relay を同時に使う実行や、前回の実行で残った配信と混ざらないようにする。
/// プロセス ID だけでは別のマシンで同時に走る実行と衝突しうるため、時刻も混ぜる。
fn e2e_namespace(test: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    // テストは同じプロセスで並列に走る。プロセス ID と時刻だけでは足りない
    // (`SystemTime::now()` の分解能は OS 依存であり、macOS ではマイクロ秒であるため、
    // 2 つのテストが同じ名前を作りうる)。どちらのテストも catalog track を PUBLISH する
    // ため、名前が同じになると後から publish した側が already_owned で拒否される。
    // テストごとの名前を混ぜる
    format!("moqt-e2e-{}-{test}-{nanos}", std::process::id())
}

/// 一時ディレクトリ
///
/// drop 時に削除する。`MOQT_E2E_KEEP=1` のときは削除せず、残した場所を出力する。
struct WorkDir {
    /// 作成したディレクトリのパス
    path: PathBuf,
}

impl WorkDir {
    /// テストごとに一意な一時ディレクトリを作る
    ///
    /// テストは同じプロセスで並列に走る。プロセス ID だけを名前に入れると、後から始めた
    /// テストが先のテストの成果物 (受信中の MP4) をディレクトリごと消してしまい、録画の
    /// 検証が「ファイルが無い」で落ちる。`name` はテストの目的を表す短い語にする。
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("moqt-e2e-{}-{name}", std::process::id()));
        // 前回の実行が残したファイルを使い回さないよう、いったん消してから作る
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("一時ディレクトリを作成できること");
        Self { path }
    }

    /// ディレクトリのパス
    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        if std::env::var("MOQT_E2E_KEEP").is_ok_and(|v| v == "1") {
            eprintln!("成果物を残しました: {}", self.path.display());
            return;
        }
        if let Err(e) = std::fs::remove_dir_all(&self.path) {
            eprintln!(
                "一時ディレクトリを削除できませんでした: {} ({e})",
                self.path.display()
            );
        }
    }
}

/// 起動中の publisher
struct Publisher {
    /// graceful shutdown を起動する
    shutdown: ShutdownController,
    /// パイプラインのタスク (結果を取り出したあとは `None` になる)
    task: Option<tokio::task::JoinHandle<moq_pub::error::Result<()>>>,
}

impl Publisher {
    /// パイプラインが終了しているかどうか
    ///
    /// subscriber が映像を受信できないときの切り分けに使う。
    fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(|task| task.is_finished())
    }

    /// 終了していればパイプラインの結果を取り出す
    async fn take_result(&mut self) -> Option<moq_pub::error::Result<()>> {
        let task = self.task.take()?;
        Some(task.await.expect("publisher のタスクが終了すること"))
    }

    /// パイプラインを停止し、正常終了したことを確認する
    async fn stop(self) {
        let Publisher { shutdown, mut task } = self;
        tokio::time::timeout(SHUTDOWN_TIMEOUT, shutdown.shutdown())
            .await
            .expect("publisher が shutdown で停止すること");
        if let Some(task) = task.take() {
            let result = task.await.expect("publisher のタスクが終了すること");
            assert!(
                result.is_ok(),
                "publisher が正常終了すること: {:?}",
                result.err()
            );
        }
    }
}

/// 起動中の subscriber
struct Subscriber {
    /// graceful shutdown を起動する
    shutdown: ShutdownController,
    /// パイプラインのタスク
    task: tokio::task::JoinHandle<moq_sub::error::Result<()>>,
    /// デコード済み映像フレームの受信側 (音声のみ購読するときは空のまま)
    frames: std::sync::mpsc::Receiver<moq_sub::DecodedVideoFrame>,
    /// デコード済み音声フレームの受信側 (映像のみ購読するときは空のまま)
    ///
    /// 購読しないトラックのチャネルも、閉じないために保持する。
    audio_frames: std::sync::mpsc::Receiver<moq_sub::DecodedAudioFrame>,
    /// 再生側の終了通知の送信側 (drop するとパイプラインが停止するため保持する)
    _player_stop: tokio::sync::oneshot::Sender<()>,
    /// 表示待ちの映像フレーム数
    ///
    /// パイプラインがフレームを送るたびに増えるため、表示側と同じようにテスト側で減らす。
    backlog: Arc<AtomicI64>,
    /// 最初に受け取った映像フレームのサイズ (幅, 高さ)。音声を待ったときは `None`
    first_frame_size: Option<(i32, i32)>,
    /// 最初に受け取った音声フレームの (サンプルレート, チャンネル数)。映像を待ったときは `None`
    first_audio_params: Option<(u32, u8)>,
}

impl Subscriber {
    /// パイプラインを停止し、正常終了したことを確認する
    async fn stop(self) {
        tokio::time::timeout(SHUTDOWN_TIMEOUT, self.shutdown.shutdown())
            .await
            .expect("subscriber が shutdown で停止すること");
        let result = tokio::time::timeout(SHUTDOWN_TIMEOUT, self.task)
            .await
            .expect("subscriber のタスクが終了すること")
            .expect("subscriber のタスクが panic しないこと");
        assert!(
            result.is_ok(),
            "subscriber が正常終了すること: {:?}",
            result.err()
        );
    }
}

/// `--url` に MSF fragment (`#msf:<track-identifier>`) を付けた URL を作る
///
/// track-identifier は §11.1.2 (MSF Namespace-Name String Encoding) の表現であり、
/// `moq-pub` / `moq-sub` は `--namespace` を省略したときにここから Track Namespace を取る。
fn msf_url(url: &str, namespace: &str, track_name: &str) -> String {
    assert!(
        !url.contains('#'),
        "MOQT_E2E_URL に fragment を含めないこと: {url}"
    );
    format!("{url}#msf:{namespace}--{track_name}")
}

/// publisher を起動する
///
/// 疑似キャプチャの映像と catalog を PUBLISH する。音声は扱わない。namespace は
/// `--namespace` ではなく `--url` の MSF fragment から渡し、省略時の解決も E2E で確かめる。
fn start_publisher(url: &str, transport: &str, namespace: &str) -> Publisher {
    let url = msf_url(url, namespace, MSF_TRACK_NAME);
    let config = moq_pub::cli::parse_args(&[
        "--url",
        url.as_str(),
        "--transport",
        transport,
        "--fake-capture-device",
        "--no-audio",
    ])
    .expect("publisher のオプションを解釈できること")
    .expect("publisher の設定が返ること");

    spawn_publisher(config)
}

/// 音声だけを datagram で配信する publisher を起動する
///
/// 音声は既定で OBJECT_DATAGRAM 配送になる
/// (draft-ietf-moq-loc-04 §4.1 (Application with one audio track))。namespace の解決は
/// [`start_publisher`] と同じく MSF fragment から行う。
fn start_audio_publisher(url: &str, transport: &str, namespace: &str) -> Publisher {
    let url = msf_url(url, namespace, MSF_TRACK_NAME);
    let config = moq_pub::cli::parse_args(&[
        "--url",
        url.as_str(),
        "--transport",
        transport,
        "--fake-capture-device",
        "--no-video",
    ])
    .expect("publisher のオプションを解釈できること")
    .expect("publisher の設定が返ること");

    spawn_publisher(config)
}

/// publisher のパイプラインを起動する
fn spawn_publisher(config: moq_pub::cli::Config) -> Publisher {
    let shutdown = ShutdownController::new();
    let task = tokio::spawn(moq_pub::pipeline::run(
        config,
        tokio_metrics::TaskMonitor::new(),
        shutdown.subscribe(),
    ));
    Publisher {
        shutdown,
        task: Some(task),
    }
}

/// subscriber が最初に待つトラック
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestTrack {
    /// 映像を購読する (音声は購読しない)
    Video,
    /// 音声を購読する (映像は購読しない)
    Audio,
}

/// subscriber を起動し、最初のフレームが届くまで待つ
///
/// publisher が catalog を PUBLISH する前に起動すると、subscriber は catalog を取得できず
/// 終了する。そのため終了した場合は理由を残して少し待ち、起動し直す。namespace は
/// `--namespace` ではなく `--url` の MSF fragment から渡し、省略時の解決も E2E で確かめる。
async fn start_subscriber(
    url: &str,
    transport: &str,
    namespace: &str,
    mp4_path: &std::path::Path,
    publisher: &mut Publisher,
    track: TestTrack,
) -> Subscriber {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mp4_path = mp4_path.to_string_lossy().to_string();
    let url = msf_url(url, namespace, MSF_TRACK_NAME);
    // 購読しないトラックは SUBSCRIBE しない。カタログには両方が載りうるため、
    // 待つトラックを明示しないと最初のフレームの到着を判定できない
    let disabled_track = match track {
        TestTrack::Video => "--no-audio",
        TestTrack::Audio => "--no-video",
    };
    loop {
        // --no-play は指定しない。no_play はデコード自体を無効にするため映像 / 音声
        // フレームが得られなくなる。SDL プレイヤーは lib に含まれないため、再生は行われない。
        let config = moq_sub::cli::parse_args(&[
            "--url",
            url.as_str(),
            "--transport",
            transport,
            disabled_track,
            "--mp4",
            &mp4_path,
        ])
        .expect("subscriber のオプションを解釈できること")
        .expect("subscriber の設定が返ること");

        let (frame_tx, frames) = std::sync::mpsc::channel();
        let (audio_tx, audio_frames) = std::sync::mpsc::channel();
        // 再生を行わないため、player_stop の送信側はここで保持し続ける
        let (player_stop, player_stop_rx) = tokio::sync::oneshot::channel();
        let shutdown = ShutdownController::new();
        // 表示側と同じようにテスト側でも減らすため、パイプラインと共有する
        let backlog = Arc::new(AtomicI64::new(0));
        // 再生を行わないためカタログの targetLatency は使わない。-1 は「カタログに無い」
        let target_latency_ms = Arc::new(AtomicI64::new(-1));
        let task = tokio::spawn(moq_sub::pipeline::run(
            config,
            frame_tx,
            audio_tx,
            tokio_metrics::TaskMonitor::new(),
            shutdown.subscribe(),
            Arc::clone(&backlog),
            target_latency_ms,
            player_stop_rx,
        ));

        // フレームの受信はブロッキングなので、他のタスクを止めないようにする
        let mut first_frame_size = None;
        let mut first_audio_params = None;
        let ready: std::result::Result<(), std::sync::mpsc::RecvTimeoutError> = match track {
            TestTrack::Video => tokio::task::block_in_place(|| {
                frames.recv_timeout(ATTEMPT_TIMEOUT).map(|frame| {
                    // 表示側と同じように、受け取った 1 枚目も待ちフレーム数から減らす
                    backlog.fetch_sub(1, Ordering::Relaxed);
                    first_frame_size = Some((frame.width, frame.height));
                })
            }),
            TestTrack::Audio => tokio::task::block_in_place(|| {
                audio_frames.recv_timeout(ATTEMPT_TIMEOUT).map(|frame| {
                    first_audio_params = Some((frame.sample_rate, frame.channels));
                })
            }),
        };
        match ready {
            Ok(()) => {
                return Subscriber {
                    shutdown,
                    task,
                    frames,
                    audio_frames,
                    _player_stop: player_stop,
                    backlog,
                    first_frame_size,
                    first_audio_params,
                };
            }
            Err(_) => {
                // catalog がまだ無いか、接続に失敗している。止めて理由を残す
                tokio::time::timeout(SHUTDOWN_TIMEOUT, shutdown.shutdown())
                    .await
                    .ok();
                let result = tokio::time::timeout(SHUTDOWN_TIMEOUT, task).await;
                let last_error = match result {
                    Ok(Ok(Err(e))) => e.to_string(),
                    Ok(Err(e)) => format!("タスクが失敗しました: {e}"),
                    Ok(Ok(Ok(()))) => "フレームが届かないまま終了しました".to_string(),
                    Err(_) => "タスクが終了しませんでした".to_string(),
                };
                // publisher が先に終了していれば subscriber 側の問題ではない
                if publisher.is_finished() {
                    let result = publisher.take_result().await;
                    panic!(
                        "subscriber がフレームを受信できないうちに publisher が終了しました: {result:?}"
                    );
                }
                assert!(
                    Instant::now() < deadline,
                    "subscriber が {READY_TIMEOUT:?} 以内にフレームを受信できませんでした (最後の失敗: {last_error})"
                );
                tokio::time::sleep(RETRY_INTERVAL).await;
            }
        }
    }
}

/// relay を介した publish / subscribe の往復を検証する
#[tokio::test(flavor = "multi_thread")]
#[ignore = "MOQT_E2E_URL を設定したうえで --ignored を付けて実行する"]
async fn publish_and_subscribe_roundtrip() {
    init_tracing();
    let url = e2e_url();
    let transport = e2e_transport();
    let namespace = e2e_namespace("roundtrip");
    let dir = WorkDir::new("roundtrip");
    let mp4_path = dir.path().join("subscriber.mp4");

    // 1. publisher を起動して SETUP と PUBLISH を完了させる
    let mut publisher = start_publisher(&url, &transport, &namespace);

    // 2. subscriber を起動し、catalog の取得と SUBSCRIBE を完了させる
    let subscriber = start_subscriber(
        &url,
        &transport,
        &namespace,
        &mp4_path,
        &mut publisher,
        TestTrack::Video,
    )
    .await;
    assert_eq!(
        subscriber.first_frame_size,
        Some(VIDEO_SIZE),
        "publisher が生成した映像のサイズでデコードできること"
    );

    // 3. 映像を受け取り続ける
    let mut received: u64 = 1;
    let mut last_received = Instant::now();
    let deadline = Instant::now() + STREAM_DURATION;
    while Instant::now() < deadline {
        // フレームの受信はブロッキングなので、他のタスクを止めないようにする
        match tokio::task::block_in_place(|| {
            subscriber.frames.recv_timeout(Duration::from_millis(500))
        }) {
            Ok(_) => {
                received += 1;
                last_received = Instant::now();
                // 表示側と同じように待ちフレーム数を減らす。減らさないとパイプラインが
                // 閾値 (MAX_DISPLAY_BACKLOG) を超えたとみなし、以降の group のデコードを省く。
                subscriber.backlog.fetch_sub(1, Ordering::Relaxed);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // pipeline が終了している。理由を取り出して報告する
                let result = tokio::time::timeout(SHUTDOWN_TIMEOUT, subscriber.task).await;
                panic!(
                    "映像フレームのチャネルが閉じました (pipeline が終了しました): received={received}, result={result:?}"
                );
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    assert!(
        received >= MIN_FRAMES,
        "映像フレームを継続して受信できること: received={received}"
    );
    assert!(
        last_received.elapsed() < LAST_FRAME_TIMEOUT,
        "配信の終了間際まで映像フレームが届くこと: 最後の受信から {:?} (received={received}, publisher_finished={})",
        last_received.elapsed(),
        publisher.is_finished()
    );

    // 4. 受信した映像が MP4 に保存されている
    let mp4_size = std::fs::metadata(&mp4_path)
        .expect("MP4 ファイルが作成されていること")
        .len();
    assert!(
        mp4_size > 0,
        "MP4 に映像が保存されていること: {mp4_size} バイト"
    );

    // 5. どちらも graceful shutdown で正常終了する
    subscriber.stop().await;
    publisher.stop().await;
}

/// relay を介した datagram 音声の往復を検証する
///
/// publisher は音声を OBJECT_DATAGRAM 配送にする (既定)
/// (draft-ietf-moq-loc-04 §4.1 (Application with one audio track))。subscriber が datagram の
/// payload を decode していなければ、音声フレームは 1 つも届かない。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "MOQT_E2E_URL を設定したうえで --ignored を付けて実行する"]
async fn publish_and_subscribe_audio_datagram_roundtrip() {
    init_tracing();
    let url = e2e_url();
    let transport = e2e_transport();
    let namespace = e2e_namespace("audio-datagram");
    let dir = WorkDir::new("audio-datagram");
    let mp4_path = dir.path().join("subscriber-audio.mp4");

    // 1. 音声だけを datagram で配信する publisher を起動する
    let mut publisher = start_audio_publisher(&url, &transport, &namespace);

    // 2. subscriber を起動し、最初の音声フレームが届くまで待つ
    let subscriber = start_subscriber(
        &url,
        &transport,
        &namespace,
        &mp4_path,
        &mut publisher,
        TestTrack::Audio,
    )
    .await;
    assert_eq!(
        subscriber.first_audio_params,
        Some((AUDIO_SAMPLE_RATE, AUDIO_CHANNELS)),
        "publisher が配信した音声のサンプルレートとチャンネル数でデコードできること"
    );

    // 3. 音声フレームを受け取り続ける
    let mut received: u64 = 1;
    let mut first_pts: Option<i64> = None;
    let mut last_pts: Option<i64> = None;
    let mut last_received = Instant::now();
    let deadline = Instant::now() + STREAM_DURATION;
    while Instant::now() < deadline {
        // フレームの受信はブロッキングなので、他のタスクを止めないようにする
        match tokio::task::block_in_place(|| {
            subscriber
                .audio_frames
                .recv_timeout(Duration::from_millis(500))
        }) {
            Ok(frame) => {
                received += 1;
                last_received = Instant::now();
                if first_pts.is_none() {
                    first_pts = Some(frame.pts_us);
                }
                // 音声の Timestamp は LOC の Timestamp から求めるため、到着順に進む。
                // 巻き戻ると別の Group の Object を混ぜている
                if let Some(previous) = last_pts {
                    assert!(
                        frame.pts_us >= previous,
                        "音声の PTS が巻き戻らないこと: {previous} -> {}",
                        frame.pts_us
                    );
                }
                last_pts = Some(frame.pts_us);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // pipeline が終了している。理由を取り出して報告する
                let result = tokio::time::timeout(SHUTDOWN_TIMEOUT, subscriber.task).await;
                panic!(
                    "音声フレームのチャネルが閉じました (pipeline が終了しました): received={received}, result={result:?}"
                );
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    assert!(
        received >= MIN_AUDIO_FRAMES,
        "音声フレームを継続して受信できること: received={received}"
    );
    assert!(
        last_received.elapsed() < LAST_FRAME_TIMEOUT,
        "配信の終了間際まで音声フレームが届くこと: 最後の受信から {:?} (received={received}, publisher_finished={})",
        last_received.elapsed(),
        publisher.is_finished()
    );
    // 20 ms 間隔の音声が続いていること。数フレームしか届いていない場合と、
    // 同じ Object を繰り返し届けている場合を PTS の幅で切り分ける
    let span_us = last_pts.expect("音声フレームの PTS を観測していること")
        - first_pts.expect("音声フレームの PTS を観測していること");
    assert!(
        span_us >= 1_000_000,
        "音声の PTS が 1 秒以上進むこと: {span_us}us (received={received})"
    );

    // 4. datagram で届いた音声が MP4 に保存されている
    let mp4_size = std::fs::metadata(&mp4_path)
        .expect("MP4 ファイルが作成されていること")
        .len();
    assert!(
        mp4_size > 0,
        "MP4 に datagram の音声が保存されていること: {mp4_size} バイト"
    );

    // 5. どちらも graceful shutdown で正常終了する
    subscriber.stop().await;
    publisher.stop().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同じプロセスで並列に走るテストが、互いの成果物を消さないこと
    ///
    /// テストは同じプロセスで並列に走るため、一時ディレクトリの名前をプロセス ID だけで
    /// 決めると、後から始めたテストが受信中の MP4 をディレクトリごと消してしまう。
    #[test]
    fn work_dirs_are_unique_per_test() {
        let first = WorkDir::new("first");
        let second = WorkDir::new("second");
        assert_ne!(
            first.path(),
            second.path(),
            "テストごとに別のディレクトリを使うこと"
        );

        // 片方のテストが作り直しても、もう片方の成果物が残る
        let marker = first.path().join("subscriber.mp4");
        std::fs::write(&marker, b"mp4").expect("成果物を書けること");
        let _restarted = WorkDir::new("second");
        assert!(marker.exists(), "別のテストの作り直しで消えないこと");
    }

    /// 同じプロセスで並列に走るテストが、同じ namespace を使わないこと
    ///
    /// どちらのテストも catalog track を PUBLISH するため、名前が同じになると後から
    /// publish した側が already_owned で拒否される。
    #[test]
    fn namespaces_are_unique_per_test() {
        let first = e2e_namespace("roundtrip");
        let second = e2e_namespace("audio-datagram");
        assert_ne!(first, second, "テストごとに別の namespace を使うこと");
        // 同じテスト名でも、実行ごとに別の名前になる
        let third = e2e_namespace("roundtrip");
        assert_ne!(first, third, "実行ごとに別の namespace を使うこと");
    }

    /// 同じ名前のディレクトリは、前回の実行の残骸を消してから作り直すこと
    #[test]
    fn work_dir_clears_the_previous_run() {
        let first = WorkDir::new("reused");
        let marker = first.path().join("subscriber.mp4");
        std::fs::write(&marker, b"mp4").expect("成果物を書けること");
        drop(first);

        let second = WorkDir::new("reused");
        assert!(
            !marker.exists(),
            "前回の実行が残した成果物を持ち越さないこと"
        );
        assert!(second.path().exists(), "ディレクトリは作り直されること");
    }
}
