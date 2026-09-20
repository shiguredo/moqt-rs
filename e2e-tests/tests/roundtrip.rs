//! relay を介した publish / subscribe の往復 E2E テスト
//!
//! 実 relay に publisher と subscriber を接続し、publisher が配信した映像が subscriber に
//! 届いて MP4 に保存されるところまでを検証する。接続先は環境変数 `MOQT_E2E_URL` で渡す。
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

/// publisher と subscriber の起動を待つ上限
const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// subscriber を起動し直す間隔
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// 1 回の起動試行で最初の映像フレームを待つ時間
///
/// キーフレーム間隔 (既定 2 秒) より長くしないと、最初の group の位相によっては
/// フレームが届かない。
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(4);
/// 映像を受け取り続ける時間
const STREAM_DURATION: Duration = Duration::from_secs(5);
/// 受け取り続けたとみなす最小フレーム数 (30 fps の 1 秒分)
const MIN_FRAMES: u64 = 30;
/// 配信の終了間際まで届いたとみなす、最後の受信からの許容時間
///
/// 受信の間隔 (500 ms) と 30 fps のフレーム間隔 (33 ms) に対して十分な余裕を取る。
const LAST_FRAME_TIMEOUT: Duration = Duration::from_secs(1);
/// shutdown とタスクの終了を待つ上限
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);
/// publisher が生成する映像のサイズ (moq-pub の既定値)
const VIDEO_SIZE: (i32, i32) = (1280, 720);

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
fn e2e_namespace() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("moqt-e2e-{}-{nanos}", std::process::id())
}

/// 一時ディレクトリ
///
/// drop 時に削除する。`MOQT_E2E_KEEP=1` のときは削除せず、残した場所を出力する。
struct WorkDir {
    /// 作成したディレクトリのパス
    path: PathBuf,
}

impl WorkDir {
    /// プロセスごとに一意な一時ディレクトリを作る
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("moqt-e2e-{}", std::process::id()));
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
    /// デコード済み映像フレームの受信側
    frames: std::sync::mpsc::Receiver<moq_sub::DecodedVideoFrame>,
    /// デコード済み音声フレームの受信側 (音声は購読しないが、チャネルを閉じないために保持する)
    _audio_frames: std::sync::mpsc::Receiver<moq_sub::DecodedAudioFrame>,
    /// 再生側の終了通知の送信側 (drop するとパイプラインが停止するため保持する)
    _player_stop: tokio::sync::oneshot::Sender<()>,
    /// 表示待ちの映像フレーム数
    ///
    /// パイプラインがフレームを送るたびに増えるため、表示側と同じようにテスト側で減らす。
    backlog: Arc<AtomicI64>,
    /// 最初に受け取った映像フレームのサイズ (幅, 高さ)
    first_frame_size: (i32, i32),
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

/// publisher を起動する
///
/// 疑似キャプチャの映像と catalog を PUBLISH する。音声は扱わない。
fn start_publisher(url: &str, transport: &str, namespace: &str) -> Publisher {
    let config = moq_pub::cli::parse_args(&[
        "--url",
        url,
        "--transport",
        transport,
        "--namespace",
        namespace,
        "--fake-capture-device",
        "--no-audio",
    ])
    .expect("publisher のオプションを解釈できること")
    .expect("publisher の設定が返ること");

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

/// subscriber を起動し、最初の映像フレームが届くまで待つ
///
/// publisher が catalog を PUBLISH する前に起動すると、subscriber は catalog を取得できず
/// 終了する。そのため終了した場合は理由を残して少し待ち、起動し直す。
async fn start_subscriber(
    url: &str,
    transport: &str,
    namespace: &str,
    mp4_path: &std::path::Path,
    publisher: &mut Publisher,
) -> Subscriber {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mp4_path = mp4_path.to_string_lossy().to_string();
    loop {
        // --no-play は指定しない。no_play はデコード自体を無効にするため映像フレームが
        // 得られなくなる。SDL プレイヤーは lib に含まれないため、再生は行われない。
        let config = moq_sub::cli::parse_args(&[
            "--url",
            url,
            "--transport",
            transport,
            "--namespace",
            namespace,
            "--no-audio",
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
        let task = tokio::spawn(moq_sub::pipeline::run(
            config,
            frame_tx,
            audio_tx,
            tokio_metrics::TaskMonitor::new(),
            shutdown.subscribe(),
            Arc::clone(&backlog),
            player_stop_rx,
        ));

        // フレームの受信はブロッキングなので、他のタスクを止めないようにする
        let received = tokio::task::block_in_place(|| frames.recv_timeout(ATTEMPT_TIMEOUT));
        match received {
            Ok(frame) => {
                // 表示側と同じように、受け取った 1 枚目も待ちフレーム数から減らす
                backlog.fetch_sub(1, Ordering::Relaxed);
                return Subscriber {
                    shutdown,
                    task,
                    frames,
                    _audio_frames: audio_frames,
                    _player_stop: player_stop,
                    backlog,
                    first_frame_size: (frame.width, frame.height),
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
                    Ok(Ok(Ok(()))) => "映像フレームが届かないまま終了しました".to_string(),
                    Err(_) => "タスクが終了しませんでした".to_string(),
                };
                // publisher が先に終了していれば subscriber 側の問題ではない
                if publisher.is_finished() {
                    let result = publisher.take_result().await;
                    panic!(
                        "subscriber が映像を受信できないうちに publisher が終了しました: {result:?}"
                    );
                }
                assert!(
                    Instant::now() < deadline,
                    "subscriber が {READY_TIMEOUT:?} 以内に映像を受信できませんでした (最後の失敗: {last_error})"
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
    let url = e2e_url();
    let transport = e2e_transport();
    let namespace = e2e_namespace();
    let dir = WorkDir::new();
    let mp4_path = dir.path().join("subscriber.mp4");

    // 1. publisher を起動して SETUP と PUBLISH を完了させる
    let mut publisher = start_publisher(&url, &transport, &namespace);

    // 2. subscriber を起動し、catalog の取得と SUBSCRIBE を完了させる
    let subscriber =
        start_subscriber(&url, &transport, &namespace, &mp4_path, &mut publisher).await;
    assert_eq!(
        subscriber.first_frame_size, VIDEO_SIZE,
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
