//! 受信サンプルの MP4 録画
//!
//! MoQ で受信したエンコード済みサンプル (AV1 / H.264 / H.265 / Opus) を再エンコードせずに
//! MP4 ファイルへ書き出す。ファイル I/O と mux は専用 OS スレッドが所有し、pipeline 側は
//! [`RecorderSender`] でサンプルを送るだけにして、tokio ランタイムで blocking I/O を行わない。
//!
//! トラックのタイムスケールはマイクロ秒 (1_000_000) 固定とし、LOC の Timestamp / Timescale
//! から変換したサンプル時刻を duration の累積で表現する。

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::num::NonZeroU32;
use std::ops::Bound::{Excluded, Unbounded};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use shiguredo_mp4::TrackKind;
use shiguredo_mp4::bitstream::av1::{Av1SampleEntryConfig, build_av01_box_from_config_obus};
use shiguredo_mp4::bitstream::h264::{H264SampleEntryConfig, LengthSize, build_avc1_box};
use shiguredo_mp4::bitstream::h265::{
    H265ConstantFrameRate, H265SampleEntryConfig, build_hev1_box, build_hvc1_box,
};
use shiguredo_mp4::bitstream::opus::{ChannelCount, OpusSampleEntryConfig, build_opus_box};
use shiguredo_mp4::boxes::SampleEntry;
use shiguredo_mp4::mux::{Mp4FileMuxer, Sample};

use crate::decoder::read_ps_nal;
use crate::error::{Error, Result};

/// MP4 のトラックタイムスケール (マイクロ秒)
const TRACK_TIMESCALE: NonZeroU32 = NonZeroU32::new(1_000_000).expect("1_000_000 is non-zero");

/// 期待サンプル尺の 1.5 倍を超えて間隔が空いたサンプルを書き出さずに待つ時間 (wall-clock)
///
/// video の group は並行に処理されるため、後続 group のキーフレームが中間サンプルより先に
/// 到着し得る。そのまま「手元にある次のサンプル」を後続とみなすと duration が膨張するため、
/// 間隔が期待尺から外れたサンプルは後続の到着を待つ。この時間が経過しても到着しない場合は
/// 実際の間隔 (ギャップ) として確定する。
const REORDER_HOLD_DURATION: Duration = Duration::from_secs(1);

/// トラック開始オフセットとして許容する最大値 (マイクロ秒)
///
/// これを超える差は Timescale の混在などで時間軸が異なるとみなす。開始オフセットは 0 にし、
/// 基準時刻より前のサンプルの破棄もしない (別の時間軸として録画を続ける)。
const MAX_TRACK_START_OFFSET_US: u64 = 60_000_000;

/// catalog のフレームレートが取得できない場合の video のサンプル尺 (30 fps 相当)
const VIDEO_FALLBACK_DURATION_US: u64 = 33_333;

/// audio のサンプル尺のフォールバック (Opus の 20 ms 固定フレーム)
const AUDIO_FALLBACK_DURATION_US: u64 = 20_000;

/// 破棄を警告ログに出す上限 (これを超えたら debug に落とす)
const DROP_WARN_LIMIT: u64 = 5;

/// OpusHead (RFC 7845 §5.1) から取り出した `dOps` に載せる値
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpusHeadConfig {
    /// Channel Count
    pub channel_count: u8,
    /// Pre-skip (48 kHz 基準のサンプル数)
    pub pre_skip: u16,
    /// Input Sample Rate (参考値)
    pub input_sample_rate: u32,
    /// Output Gain (Q7.8 固定小数点)
    pub output_gain: i16,
}

/// 録画する video トラックの情報 (catalog 由来)
#[derive(Debug, Clone)]
pub(crate) struct VideoSetup {
    /// RFC 6381 の codec 文字列 (av01 / avc1 / hvc1 / hev1)
    pub codec: String,
    /// catalog のフレームレート (duration の期待値に使う)
    pub fps: u32,
}

/// 録画する audio トラックの情報 (catalog 由来)
#[derive(Debug, Clone, Copy)]
pub(crate) struct AudioSetup {
    /// サンプリングレート (Hz)
    pub sample_rate: u32,
    /// チャンネル数
    pub channels: u8,
}

/// 録画対象のトラック情報
#[derive(Debug, Clone)]
pub(crate) struct Setup {
    /// video トラック (購読していない場合は `None`)
    pub video: Option<VideoSetup>,
    /// audio トラック (購読していない場合は `None`)
    pub audio: Option<AudioSetup>,
}

/// 録画する video サンプル
#[derive(Debug)]
pub(crate) struct VideoSample {
    /// LOC の Timestamp
    pub timestamp: Option<u64>,
    /// LOC の Timescale (video はキーフレームのみ持つ)
    pub timescale: Option<u64>,
    /// キーフレームかどうか
    pub keyframe: bool,
    /// キーフレームに付与される PROP_VIDEO_CONFIG
    pub config: Option<Vec<u8>>,
    /// エンコード済みペイロード
    pub data: Vec<u8>,
}

/// 録画する audio サンプル
#[derive(Debug)]
pub(crate) struct AudioSample {
    /// LOC の Timestamp
    pub timestamp: Option<u64>,
    /// LOC の Timescale
    pub timescale: Option<u64>,
    /// 先頭サンプルに付与される Audio Config (OpusHead)
    pub config: Option<OpusHeadConfig>,
    /// エンコード済みペイロード
    pub data: Vec<u8>,
}

/// 録画対象のトラック種別
///
/// MP4 の [`TrackKind`] は字幕も取り得るが、本モジュールは video / audio のみを扱う。
/// 内部の状態管理を 2 値に閉じることで、字幕の分岐を書かなくて済むようにする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackId {
    /// 映像トラック
    Video,
    /// 音声トラック
    Audio,
}

impl From<TrackId> for TrackKind {
    fn from(id: TrackId) -> Self {
        match id {
            TrackId::Video => TrackKind::Video,
            TrackId::Audio => TrackKind::Audio,
        }
    }
}

/// ライタースレッドへ送るメッセージ
#[derive(Debug)]
enum Message {
    /// video サンプル
    Video(VideoSample),
    /// audio サンプル
    Audio(AudioSample),
    /// 録画を確定して終了する
    Finish,
}

/// サンプルに付随するコーデック設定
#[derive(Debug)]
enum SampleConfig {
    /// video の PROP_VIDEO_CONFIG (avcC / hvcC / av1C の config OBUs)
    Video(Vec<u8>),
    /// audio の Audio Config (OpusHead)
    Audio(OpusHeadConfig),
}

/// 書き出し待ちのサンプル
#[derive(Debug)]
struct PendingSample {
    /// トラック種別
    kind: TrackId,
    /// キーフレームかどうか
    keyframe: bool,
    /// コーデック設定 (設定を運ぶオブジェクトにのみ付く。サンプルエントリー構築後は無視する)
    config: Option<SampleConfig>,
    /// エンコード済みペイロード
    data: Vec<u8>,
    /// ライタースレッドが受け取った時刻 (後続到着の待ち時間の判定に使う)
    arrived_at: Instant,
}

/// サンプルエントリーの構築状態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryState {
    /// 未構築
    Pending,
    /// 構築済み
    Built,
    /// 構築に失敗して録画を止めた
    Failed,
}

/// トラックごとの録画状態
#[derive(Debug)]
struct Track {
    /// 直前に観測した LOC の Timescale
    timescale: Option<u64>,
    /// 書き出し済みサンプルのメディア時刻の終端。この値より前の時刻のサンプルは
    /// トラック内の昇順を保てないため破棄する (終端と同時刻のサンプルは受理する)
    last_flushed_end_us: Option<u64>,
    /// 直前に書き出したサンプルの尺 (基準時刻の加算を含まない)
    last_duration_us: Option<u64>,
    /// サンプルエントリーの構築状態
    entry_state: EntryState,
    /// 破棄したサンプル数
    dropped: u64,
}

impl Track {
    /// トラックの初期状態を作る
    fn new() -> Self {
        Self {
            timescale: None,
            last_flushed_end_us: None,
            last_duration_us: None,
            entry_state: EntryState::Pending,
            dropped: 0,
        }
    }
}

/// 録画のハンドル
///
/// drop 時にもライタースレッドを join して finalize するため、`run` の早期 return でも
/// ファイルは可能な限り確定される。
pub(crate) struct Recorder {
    /// ライタースレッドへ送る送信側
    sender: Option<mpsc::Sender<Message>>,
    /// ライタースレッド
    handle: Option<std::thread::JoinHandle<Result<()>>>,
}

impl Recorder {
    /// 録画を開始する
    ///
    /// ファイルは最初のサンプルを書き出す時点で作成し、既存ファイルは truncate して
    /// 上書きする。録画対象のサンプルが 1 件も無い場合はファイルを作成せず、既存ファイルも
    /// 変更しない。
    pub(crate) fn start<P: AsRef<Path>>(path: P, setup: Setup) -> Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let path = path.as_ref().to_path_buf();
        tracing::info!("MP4 recording started: path={}", path.display());
        let handle = std::thread::Builder::new()
            .name("mp4-writer".to_string())
            .spawn(move || writer_thread(receiver, path, setup))
            .map_err(|e| Error::Other(format!("failed to spawn MP4 writer thread: {e}")))?;
        Ok(Self {
            sender: Some(sender),
            handle: Some(handle),
        })
    }

    /// サンプル送信用のハンドルを返す
    pub(crate) fn sender(&self) -> RecorderSender {
        RecorderSender(
            self.sender
                .as_ref()
                .expect("recorder is not finished")
                .clone(),
        )
    }

    /// 終了してライタースレッドを待ち合わせる
    ///
    /// 終了メッセージを送ってライタースレッドを確定させるため、`RecorderSender` の clone が
    /// 残っていても待ち続けない。録画中の I/O エラーや finalize の失敗は `Err` で返す。
    pub(crate) fn finish(mut self) -> Result<()> {
        self.send_finish();
        self.sender.take();
        self.join()
    }

    /// ライタースレッドへ終了メッセージを送る
    fn send_finish(&mut self) {
        if let Some(sender) = self.sender.as_ref() {
            let _ = sender.send(Message::Finish);
        }
    }

    /// ライタースレッドの終了を待つ
    fn join(&mut self) -> Result<()> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        match handle.join() {
            Ok(result) => result,
            Err(_) => Err(Error::Other("MP4 writer thread panicked".to_string())),
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // `finish` を経由しなかった場合 (早期 return など) も finalize を試みる
        self.send_finish();
        self.sender.take();
        if let Err(e) = self.join() {
            tracing::error!("Failed to finalize MP4 recording: {e}");
        }
    }
}

/// サンプル送信用のハンドル
///
/// clone して複数の stream task から使う。
#[derive(Clone)]
pub(crate) struct RecorderSender(mpsc::Sender<Message>);

impl RecorderSender {
    /// video サンプルを送る
    pub(crate) fn video(&self, sample: VideoSample) {
        self.send(Message::Video(sample));
    }

    /// audio サンプルを送る
    pub(crate) fn audio(&self, sample: AudioSample) {
        self.send(Message::Audio(sample));
    }

    /// メッセージを送る
    ///
    /// チャネルは無制限のため、ライタースレッドが遅れても送信側は待たない。録画は
    /// best-effort であり、ライタースレッドの終了後に届いたサンプルは破棄する。
    fn send(&self, message: Message) {
        let _ = self.0.send(message);
    }
}

/// ライタースレッドの本体
fn writer_thread(receiver: mpsc::Receiver<Message>, path: PathBuf, setup: Setup) -> Result<()> {
    let mut writer = Writer::new(path, setup);
    for message in receiver {
        match message {
            Message::Finish => break,
            Message::Video(sample) => writer.handle_video(sample),
            Message::Audio(sample) => writer.handle_audio(sample),
        }
    }
    writer.finish()
}

/// MP4 ファイルへの書き出し状態
struct Writer {
    /// 出力ファイルパス
    path: PathBuf,
    /// 録画対象のトラック情報
    setup: Setup,
    /// muxer (ファイル作成時に作る)
    muxer: Option<Mp4FileMuxer>,
    /// 出力ファイル
    file: Option<BufWriter<File>>,
    /// 次にサンプルデータを書くファイル位置
    next_position: u64,
    /// 録画を諦めた原因 (I/O エラーなど)
    failed: Option<String>,
    /// video トラックで最初のキーフレームを受け入れたか
    video_started: bool,
    /// video トラックの状態
    video: Track,
    /// audio トラックの状態
    audio: Track,
    /// audio トラックで直近に観測した OpusHead
    ///
    /// video のキーフレーム待ちで先頭の audio サンプルを破棄しても `dOps` に反映できるように
    /// 保持する。
    audio_opus_head: Option<OpusHeadConfig>,
    /// 書き出し待ちのサンプル (キーは `(pts_us, 到着順)`)
    pending: BTreeMap<(u64, u64), PendingSample>,
    /// 録画の基準時刻 (マイクロ秒)
    base_pts_us: Option<u64>,
    /// 到着順の連番
    seq: u64,
    /// 書き出したサンプル数
    written_samples: u64,
}

impl Writer {
    /// 空の状態を作る
    fn new(path: PathBuf, setup: Setup) -> Self {
        if let Some(video) = setup.video.as_ref() {
            tracing::info!(
                "MP4 recording video track: codec={}, fps={}",
                video.codec,
                video.fps
            );
        }
        if let Some(audio) = setup.audio.as_ref() {
            tracing::info!(
                "MP4 recording audio track: sample_rate={}, channels={}",
                audio.sample_rate,
                audio.channels
            );
        }
        Self {
            path,
            setup,
            muxer: None,
            file: None,
            next_position: 0,
            failed: None,
            video_started: false,
            video: Track::new(),
            audio: Track::new(),
            audio_opus_head: None,
            pending: BTreeMap::new(),
            base_pts_us: None,
            seq: 0,
            written_samples: 0,
        }
    }

    /// video メッセージを処理する
    fn handle_video(&mut self, sample: VideoSample) {
        if self.failed.is_some() {
            return;
        }
        let result = self.accept_video(sample);
        self.run_step(result);
    }

    /// audio メッセージを処理する
    fn handle_audio(&mut self, sample: AudioSample) {
        if self.failed.is_some() {
            return;
        }
        let result = self.accept_audio(sample);
        self.run_step(result);
    }

    /// 処理結果を反映する
    fn run_step(&mut self, result: Result<()>) {
        if let Err(e) = result {
            tracing::error!(
                "Failed to write MP4 file: path={} error={e}",
                self.path.display()
            );
            self.failed = Some(e.to_string());
        }
    }

    /// video サンプルを受け入れる
    fn accept_video(&mut self, sample: VideoSample) -> Result<()> {
        if self.setup.video.is_none() || self.video.entry_state == EntryState::Failed {
            return Ok(());
        }
        let Some(pts_us) = to_microseconds(
            &mut self.video.timescale,
            sample.timestamp,
            sample.timescale,
        ) else {
            self.drop_sample(TrackId::Video, "timestamp is not present");
            return Ok(());
        };
        if !self.video_started {
            // 復号できないサンプルからトラックを始めないよう、キーフレームと
            // PROP_VIDEO_CONFIG が揃った最初のサンプルまで待つ
            if !sample.keyframe || sample.config.is_none() {
                self.drop_sample(TrackId::Video, "waiting for the first keyframe");
                return Ok(());
            }
            self.video_started = true;
            if self.base_pts_us.is_none() {
                self.base_pts_us = Some(pts_us);
            }
        }
        self.insert(
            TrackId::Video,
            pts_us,
            sample.keyframe,
            sample.config.map(SampleConfig::Video),
            sample.data,
        );
        self.flush_ready(false)
    }

    /// audio サンプルを受け入れる
    fn accept_audio(&mut self, sample: AudioSample) -> Result<()> {
        if self.setup.audio.is_none() || self.audio.entry_state == EntryState::Failed {
            return Ok(());
        }
        // OpusHead は先頭のサンプルにしか付かないため、録画の可否に関わらず保持する
        if let Some(config) = sample.config {
            self.audio_opus_head = Some(config);
        }
        // video を購読している場合は、録画の基準を最初のキーフレームに合わせるため
        // それ以前の audio は録画しない
        if self.setup.video.is_some() && !self.video_started {
            self.drop_sample(TrackId::Audio, "waiting for the first video keyframe");
            return Ok(());
        }
        let Some(pts_us) = to_microseconds(
            &mut self.audio.timescale,
            sample.timestamp,
            sample.timescale,
        ) else {
            self.drop_sample(TrackId::Audio, "timestamp is not present");
            return Ok(());
        };
        self.insert(
            TrackId::Audio,
            pts_us,
            true,
            sample.config.map(SampleConfig::Audio),
            sample.data,
        );
        self.flush_ready(false)
    }

    /// サンプルを破棄して記録する
    fn drop_sample(&mut self, kind: TrackId, reason: &str) {
        let track = self.track_mut(kind);
        track.dropped += 1;
        if track.dropped <= DROP_WARN_LIMIT {
            tracing::warn!("Discarding MP4 sample ({kind:?}): {reason}");
        } else {
            tracing::debug!("Discarding MP4 sample ({kind:?}): {reason}");
        }
    }

    /// reorder buffer にサンプルを追加する
    fn insert(
        &mut self,
        kind: TrackId,
        pts_us: u64,
        keyframe: bool,
        config: Option<SampleConfig>,
        data: Vec<u8>,
    ) {
        // 書き出し済みのサンプルが覆う区間に入るサンプルは、トラック内の昇順を保てないため
        // 破棄する。その区間は直前のサンプルの duration が吸収する。
        if let Some(end) = self.track(kind).last_flushed_end_us
            && pts_us < end
        {
            self.drop_sample(kind, "arrived after the track timeline was written");
            return;
        }
        // 基準時刻より前のサンプルは MP4 のタイムラインに置けないため破棄する。
        // ただし時間軸が大きく異なる場合 (Timescale の混在など) は別の時間軸とみなして録画する。
        if let Some(base) = self.base_pts_us
            && pts_us < base
            && base - pts_us <= MAX_TRACK_START_OFFSET_US
        {
            self.drop_sample(kind, "timestamp is before the recording base");
            return;
        }
        self.seq += 1;
        self.pending.insert(
            (pts_us, self.seq),
            PendingSample {
                kind,
                keyframe,
                config,
                data,
                arrived_at: Instant::now(),
            },
        );
    }

    /// 書き出せるサンプルを timestamp 昇順で書き出す
    ///
    /// 先頭サンプルの後続が未到着の場合は書き出しを保留し、もう一方のトラックの先頭を
    /// 書き出す (片方のトラックの停止で他方が詰まらないようにする)。`finishing` では
    /// 残りを直前の duration (無ければフォールバック) で書き出す。
    fn flush_ready(&mut self, finishing: bool) -> Result<()> {
        while let Some((key, kind)) = self.front() {
            if self.try_flush(kind, key, finishing)? {
                continue;
            }
            let other = match kind {
                TrackId::Video => TrackId::Audio,
                TrackId::Audio => TrackId::Video,
            };
            let Some(other_key) = self.front_of(other) else {
                break;
            };
            if !self.try_flush(other, other_key, finishing)? {
                break;
            }
        }
        Ok(())
    }

    /// 書き出し待ちのうち最も古いサンプルを返す
    fn front(&self) -> Option<((u64, u64), TrackId)> {
        self.pending
            .first_key_value()
            .map(|(&key, sample)| (key, sample.kind))
    }

    /// 指定トラックの書き出し待ちのうち最も古いサンプルのキーを返す
    fn front_of(&self, kind: TrackId) -> Option<(u64, u64)> {
        self.pending
            .iter()
            .find(|(_, sample)| sample.kind == kind)
            .map(|(&key, _)| key)
    }

    /// 先頭サンプルを書き出せる場合は書き出す
    ///
    /// 書き出した場合は `true`、後続の到着を待つ場合は `false` を返す。
    fn try_flush(&mut self, kind: TrackId, key: (u64, u64), finishing: bool) -> Result<bool> {
        let Some(front_waiting) = self
            .pending
            .get(&key)
            .map(|sample| sample.arrived_at.elapsed())
        else {
            return Ok(false);
        };
        let next_key = self
            .pending
            .range((Excluded(key), Unbounded))
            .find(|(_, sample)| sample.kind == kind)
            .map(|(&key, _)| key);
        // ギャップを確定するまでの待ち時間は「先頭と後続の到着の遅い方」からの経過時間にする。
        // 後続が到着した直後に中間サンプルが届く場合に、先頭の到着からの経過時間だけで
        // ギャップを確定してしまわないようにするためである。
        let waiting = match next_key {
            Some(next_key) => {
                let next_waiting = self
                    .pending
                    .get(&next_key)
                    .map(|sample| sample.arrived_at.elapsed())
                    .unwrap_or(front_waiting);
                front_waiting.min(next_waiting)
            }
            None => front_waiting,
        };
        let next_pts = next_key.map(|key| key.0);
        let expected_us = self.expected_duration_us(kind);
        let Some(content_duration_us) = content_duration_us(
            next_pts,
            key.0,
            expected_us,
            self.track(kind).last_duration_us,
            waiting,
            finishing,
        ) else {
            return Ok(false);
        };
        let Some(sample) = self.pending.remove(&key) else {
            return Ok(false);
        };

        // 最初に書き出すサンプルの時刻を録画の基準時刻にする (video を購読している
        // 場合は最初のキーフレーム受理時に設定済み)
        if self.base_pts_us.is_none() {
            self.base_pts_us = Some(key.0);
        }

        // 遅れて始まるトラックは、最初のサンプルの duration に基準時刻との差を
        // 加算して 2 番目以降を基準時刻に揃える (先頭サンプルだけは早く提示される)
        let first = self.track(kind).last_flushed_end_us.is_none();
        let offset_us = if first {
            let base = self.base_pts_us.unwrap_or(key.0);
            let diff = key.0.saturating_sub(base);
            if diff <= MAX_TRACK_START_OFFSET_US {
                diff
            } else {
                tracing::warn!(
                    "Track start offset is too large, starting the track at 0: kind={kind:?}, offset_us={diff}"
                );
                0
            }
        } else {
            0
        };

        let duration_us = content_duration_us.saturating_add(offset_us);
        {
            let track = self.track_mut(kind);
            // 破棄境界はソースのメディア時刻で持つ。先頭サンプルの offset は「トラック開始が
            // 遅れた分」でありソース時刻側の key.0 に既に含まれているため、ここでは content だけを
            // 足す (offset を足すと、遅れて始まるトラックの正当なサンプルを誤って破棄する)
            track.last_flushed_end_us = Some(key.0.saturating_add(content_duration_us));
            track.last_duration_us = Some(content_duration_us);
        }
        match self.append_sample(kind, &sample, duration_us) {
            Ok(()) => {}
            Err(AppendError::Track(e)) => {
                // サンプルエントリーが作れないトラックは録画を止める (再生は継続する)
                tracing::warn!("Stopping MP4 recording for track {kind:?}: {e}");
                self.track_mut(kind).entry_state = EntryState::Failed;
                self.pending.retain(|_, sample| sample.kind != kind);
            }
            Err(AppendError::Fatal(e)) => return Err(e),
        }
        Ok(true)
    }

    /// 1 サンプルを muxer へ追加する
    fn append_sample(
        &mut self,
        kind: TrackId,
        sample: &PendingSample,
        duration_us: u64,
    ) -> std::result::Result<(), AppendError> {
        let entry = if self.track(kind).entry_state == EntryState::Pending {
            let entry = self
                .build_entry(kind, sample.config.as_ref())
                .map_err(AppendError::Track)?;
            self.track_mut(kind).entry_state = EntryState::Built;
            Some(entry)
        } else {
            None
        };
        self.ensure_file().map_err(AppendError::Fatal)?;
        // 1 サンプルの duration が u32 を超えるのは異常な gap のみなので飽和させる
        let duration = u32::try_from(duration_us.min(u64::from(u32::MAX)))
            .expect("duration is clamped to u32::MAX");
        let file = self.file.as_mut().expect("file is opened by ensure_file");
        file.write_all(&sample.data)
            .map_err(|e| AppendError::Fatal(e.into()))?;
        let mp4_sample = Sample {
            track_kind: kind.into(),
            sample_entry: entry,
            keyframe: sample.keyframe,
            timescale: TRACK_TIMESCALE,
            duration,
            composition_time_offset: None,
            data_offset: self.next_position,
            data_size: sample.data.len(),
        };
        self.muxer
            .as_mut()
            .expect("muxer is created by ensure_file")
            .append_sample(&mp4_sample)
            .map_err(|e| {
                AppendError::Fatal(Error::Other(format!("failed to append MP4 sample: {e}")))
            })?;
        self.next_position += sample.data.len() as u64;
        self.written_samples += 1;
        Ok(())
    }

    /// サンプルエントリーを構築する
    fn build_entry(&self, kind: TrackId, config: Option<&SampleConfig>) -> Result<SampleEntry> {
        match kind {
            TrackId::Video => {
                let Some(video) = self.setup.video.as_ref() else {
                    return Err(Error::Other(
                        "video track is not configured for MP4 recording".to_string(),
                    ));
                };
                let Some(SampleConfig::Video(config)) = config else {
                    return Err(Error::Other(
                        "video sample does not carry PROP_VIDEO_CONFIG".to_string(),
                    ));
                };
                build_video_entry(&video.codec, config)
            }
            TrackId::Audio => {
                let Some(audio) = self.setup.audio.as_ref() else {
                    return Err(Error::Other(
                        "audio track is not configured for MP4 recording".to_string(),
                    ));
                };
                let head = match config {
                    Some(SampleConfig::Audio(head)) => Some(head),
                    _ => self.audio_opus_head.as_ref(),
                };
                Ok(build_audio_entry(audio.sample_rate, audio.channels, head))
            }
        }
    }

    /// 出力ファイルを (未作成なら) 作成する
    fn ensure_file(&mut self) -> Result<()> {
        if self.file.is_some() {
            return Ok(());
        }
        let muxer = Mp4FileMuxer::new()
            .map_err(|e| Error::Other(format!("failed to create MP4 muxer: {e}")))?;
        let initial_bytes = muxer.initial_boxes_bytes();
        let file = File::create(&self.path).map_err(|e| {
            Error::Other(format!(
                "failed to create MP4 file: path={}: {e}",
                self.path.display()
            ))
        })?;
        let mut file = BufWriter::new(file);
        let write_result = file.write_all(initial_bytes).and_then(|()| file.flush());
        if let Err(e) = write_result {
            // 書き出しに失敗したファイルを残さない
            drop(file);
            if let Err(remove_error) = std::fs::remove_file(&self.path) {
                tracing::warn!(
                    "Failed to remove incomplete MP4 file: path={} error={remove_error}",
                    self.path.display()
                );
            }
            return Err(e.into());
        }
        self.next_position = initial_bytes.len() as u64;
        self.muxer = Some(muxer);
        self.file = Some(file);
        tracing::info!("MP4 file created: path={}", self.path.display());
        Ok(())
    }

    /// トラックの参照を返す
    fn track(&self, kind: TrackId) -> &Track {
        match kind {
            TrackId::Video => &self.video,
            TrackId::Audio => &self.audio,
        }
    }

    /// トラックの可変参照を返す
    fn track_mut(&mut self, kind: TrackId) -> &mut Track {
        match kind {
            TrackId::Video => &mut self.video,
            TrackId::Audio => &mut self.audio,
        }
    }

    /// サンプルの期待尺を返す (後続到着の待ち判定と最後のサンプルの duration に使う)
    fn expected_duration_us(&self, kind: TrackId) -> u64 {
        match kind {
            TrackId::Video => match self.setup.video.as_ref() {
                Some(video) if video.fps > 0 => (1_000_000 / u64::from(video.fps)).max(1),
                _ => VIDEO_FALLBACK_DURATION_US,
            },
            TrackId::Audio => AUDIO_FALLBACK_DURATION_US,
        }
    }

    /// 残りのサンプルを書き出してファイルを確定する
    fn finish(&mut self) -> Result<()> {
        if self.failed.is_none() {
            // 最終 flush のエラーも salvage 経路 (書き出し済みサンプルでの finalize) に
            // 合流させる
            if let Err(e) = self.flush_ready(true) {
                tracing::error!(
                    "Failed to flush MP4 samples: path={} error={e}",
                    self.path.display()
                );
                self.failed = Some(e.to_string());
            }
        } else {
            // I/O エラー後は未書き出しのサンプルを捨て、書き出し済みのサンプルで
            // finalize を試みる (moov が無いと再生できないため)
            if !self.pending.is_empty() {
                tracing::warn!(
                    "Discarding unwritten MP4 samples after an error: path={} count={}",
                    self.path.display(),
                    self.pending.len()
                );
            }
        }
        if self.video.dropped > DROP_WARN_LIMIT {
            tracing::debug!("Dropped video samples: {}", self.video.dropped);
        }
        if self.audio.dropped > DROP_WARN_LIMIT {
            tracing::debug!("Dropped audio samples: {}", self.audio.dropped);
        }

        let finalize_result = if self.muxer.is_none() {
            // サンプルが 1 件も書き出されなかった場合のみ「ファイルを作らない」を通知する
            // (録画開始前のエラーでは self.failed に原因が残っている)
            if self.failed.is_none() {
                tracing::warn!(
                    "No MP4 samples were recorded; the file was not created: path={}",
                    self.path.display()
                );
            }
            Ok(())
        } else {
            self.finalize_file()
        };
        match (self.failed.take(), finalize_result) {
            (Some(cause), Ok(())) => Err(Error::Other(format!(
                "MP4 recording failed: path={} cause={cause}",
                self.path.display()
            ))),
            (Some(cause), Err(e)) => Err(Error::Other(format!(
                "MP4 recording failed: path={} cause={cause} finalize_error={e}",
                self.path.display()
            ))),
            (None, result) => result,
        }
    }

    /// muxer の結果をファイルへ書き戻して確定する
    fn finalize_file(&mut self) -> Result<()> {
        let path = self.path.clone();
        let io_error = |e: std::io::Error| {
            Error::Other(format!(
                "failed to finalize MP4 file: path={}: {e}",
                path.display()
            ))
        };
        let finalized = self
            .muxer
            .as_mut()
            .expect("muxer is created by ensure_file")
            .finalize()
            .map_err(|e| {
                Error::Other(format!(
                    "failed to finalize MP4 file: path={}: {e}",
                    path.display()
                ))
            })?;
        let file = self.file.as_mut().expect("file is created by ensure_file");
        for (offset, bytes) in finalized.offset_and_bytes_pairs() {
            file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
            file.write_all(bytes).map_err(io_error)?;
        }
        file.flush().map_err(io_error)?;
        tracing::info!(
            "MP4 file finalized: path={} samples={}",
            path.display(),
            self.written_samples
        );
        Ok(())
    }
}

/// サンプル追加時のエラー
enum AppendError {
    /// そのトラックのみ録画を止める
    Track(Error),
    /// 録画全体を止める
    Fatal(Error),
}

/// 先頭サンプルの duration を決める
///
/// `None` は「後続サンプルの到着を待つ」を意味する。
///
/// - 後続サンプルとの差が期待尺の 1.5 倍以内なら、その差を尺にする
/// - 差が大きい場合は並行処理による到着順の入れ替わりとみなして待ち、`waiting` が
///   [`REORDER_HOLD_DURATION`] を超えたら実際のギャップとして確定する
/// - 同一 timestamp のサンプルは差分が 0 になるため、直前の尺 (無ければ期待尺) を使う
/// - 後続サンプルが無い場合は、実際の停止区間を失わないよう終了時まで待つ
fn content_duration_us(
    next_pts: Option<u64>,
    pts_us: u64,
    expected_us: u64,
    last_duration_us: Option<u64>,
    waiting: Duration,
    finishing: bool,
) -> Option<u64> {
    let reuse_us = last_duration_us
        .map(|duration| duration.min(expected_us))
        .unwrap_or(expected_us);
    match next_pts {
        Some(next) if next > pts_us => {
            let gap_us = next - pts_us;
            if finishing
                || gap_us <= expected_us.saturating_add(expected_us / 2)
                || waiting >= REORDER_HOLD_DURATION
            {
                Some(gap_us)
            } else {
                None
            }
        }
        Some(_) => Some(reuse_us),
        None => {
            // 後続が来ないまま確定すると実際の停止区間がタイムラインから消えるため、
            // 終了時まで保持する (各トラックの最後尾 1 サンプルだけが保持される)
            if finishing { Some(reuse_us) } else { None }
        }
    }
}

/// LOC の Timestamp / Timescale をマイクロ秒へ変換する
///
/// - `timescale` が観測されていれば `timestamp * 1_000_000 / timescale`
/// - 観測済みの timescale があり、このサンプルが timescale を持たない場合はそれを使う
/// - 一度も timescale を観測していない場合は draft-ietf-moq-loc-04 §2.3.1.1 の既定に従い
///   Unix エポックからのマイクロ秒として扱う
///
/// `timestamp` が無い場合は `None` を返す。
fn to_microseconds(
    track_timescale: &mut Option<u64>,
    timestamp: Option<u64>,
    sample_timescale: Option<u64>,
) -> Option<u64> {
    let timestamp = timestamp?;
    if let Some(timescale) = sample_timescale
        && timescale > 0
    {
        *track_timescale = Some(timescale);
    }
    match *track_timescale {
        Some(timescale) if timescale > 0 => {
            let micros = u128::from(timestamp) * 1_000_000 / u128::from(timescale);
            Some(u64::try_from(micros).unwrap_or(u64::MAX))
        }
        _ => Some(timestamp),
    }
}

/// video のサンプルエントリーを構築する
///
/// 受信ペイロードは length-prefixed NAL (AVCC / HVCC) を前提とする。LOC-04 §2.1.3 は
/// payload が canonical (length prefix) と Annex B のどちらでもよいとし、長さ値 1 を
/// 開始コードとして解釈する SHOULD を定めるが、moq-pub は length-prefixed で送るため
/// Annex B からの変換は行わない。
fn build_video_entry(codec: &str, config: &[u8]) -> Result<SampleEntry> {
    if codec.starts_with("av01") {
        let entry = build_av01_box_from_config_obus(
            config,
            &Av1SampleEntryConfig {
                initial_presentation_delay_minus_one: None,
            },
        )
        .map_err(|e| Error::Other(format!("failed to build av01 sample entry: {e}")))?;
        Ok(SampleEntry::Av01(entry))
    } else if codec.starts_with("avc1") {
        let record = parse_avc_decoder_config_record(config)?;
        let entry = build_avc1_box(
            &record.sps_list,
            &record.pps_list,
            &H264SampleEntryConfig {
                length_size: record.length_size,
            },
        )
        .map_err(|e| Error::Other(format!("failed to build avc1 sample entry: {e}")))?;
        Ok(SampleEntry::Avc1(entry))
    } else if codec.starts_with("hvc1") || codec.starts_with("hev1") {
        let record = parse_hevc_decoder_config_record(config)?;
        let h265_config = H265SampleEntryConfig {
            length_size: record.length_size,
            avg_frame_rate: H265SampleEntryConfig::AVG_FRAME_RATE_UNSPECIFIED,
            constant_frame_rate: H265ConstantFrameRate::Unknown,
        };
        if codec.starts_with("hvc1") {
            let entry = build_hvc1_box(
                &record.vps_list,
                &record.sps_list,
                &record.pps_list,
                &h265_config,
            )
            .map_err(|e| Error::Other(format!("failed to build hvc1 sample entry: {e}")))?;
            Ok(SampleEntry::Hvc1(entry))
        } else {
            let entry = build_hev1_box(
                &record.vps_list,
                &record.sps_list,
                &record.pps_list,
                &h265_config,
            )
            .map_err(|e| Error::Other(format!("failed to build hev1 sample entry: {e}")))?;
            Ok(SampleEntry::Hev1(entry))
        }
    } else {
        Err(Error::Other(format!(
            "unsupported video codec for MP4 recording: {codec}"
        )))
    }
}

/// audio のサンプルエントリーを構築する
///
/// OpusHead が観測されていればその値を優先し、無ければ catalog の値を使って `dOps` を
/// 構築する。
fn build_audio_entry(sample_rate: u32, channels: u8, head: Option<&OpusHeadConfig>) -> SampleEntry {
    let (channels, pre_skip, input_sample_rate, output_gain) = match head {
        Some(head) => {
            let channels = if matches!(head.channel_count, 1 | 2) {
                head.channel_count
            } else {
                tracing::warn!(
                    "Unsupported OpusHead channel count {}; falling back to catalog channels {}",
                    head.channel_count,
                    channels
                );
                channels
            };
            (
                channels,
                head.pre_skip,
                head.input_sample_rate,
                head.output_gain,
            )
        }
        None => (channels, 0, sample_rate, 0),
    };
    let channel_count = if channels == 2 {
        ChannelCount::Stereo
    } else {
        if channels != 1 {
            tracing::warn!("Unsupported channel count {channels}; falling back to mono");
        }
        ChannelCount::Mono
    };
    SampleEntry::Opus(build_opus_box(&OpusSampleEntryConfig {
        channel_count,
        pre_skip,
        input_sample_rate,
        output_gain,
    }))
}

/// AVCDecoderConfigurationRecord (ISO/IEC 14496-15 §5.2.4.1.1)
///
/// PROP_VIDEO_CONFIG に載るのはボックスヘッダを含まないレコード本体である。
struct AvcDecoderConfigRecord {
    /// NAL 長フィールド幅
    length_size: LengthSize,
    /// SPS NAL のリスト
    sps_list: Vec<Vec<u8>>,
    /// PPS NAL のリスト
    pps_list: Vec<Vec<u8>>,
}

/// AVCDecoderConfigurationRecord をパースする
fn parse_avc_decoder_config_record(buf: &[u8]) -> Result<AvcDecoderConfigRecord> {
    /// SPS 数と PPS 数のフィールドまでを読める最小サイズ
    const AVC_CONFIG_MIN_LEN: usize = 7;

    if buf.len() < AVC_CONFIG_MIN_LEN {
        return Err(Error::Other(format!(
            "AVCDecoderConfigurationRecord is too short: {}",
            buf.len()
        )));
    }
    if buf[0] != 1 {
        return Err(Error::Other(format!(
            "unsupported AVCDecoderConfigurationRecord configurationVersion: {}",
            buf[0]
        )));
    }
    let length_size = LengthSize::from_length_size_minus_one(buf[4] & 0x03).map_err(|e| {
        Error::Other(format!(
            "invalid AVCDecoderConfigurationRecord lengthSize: {e}"
        ))
    })?;
    let num_sps = usize::from(buf[5] & 0x1F);
    if num_sps == 0 {
        return Err(Error::Other(
            "AVCDecoderConfigurationRecord has no SPS".to_string(),
        ));
    }
    let mut pos = 6;
    let mut sps_list = Vec::new();
    for _ in 0..num_sps {
        sps_list.push(read_ps_nal(buf, &mut pos, "AVCDecoderConfigurationRecord")?);
    }
    if pos >= buf.len() {
        return Err(Error::Other(
            "AVCDecoderConfigurationRecord is truncated before the PPS count".to_string(),
        ));
    }
    let num_pps = usize::from(buf[pos]);
    pos += 1;
    if num_pps == 0 {
        return Err(Error::Other(
            "AVCDecoderConfigurationRecord has no PPS".to_string(),
        ));
    }
    let mut pps_list = Vec::new();
    for _ in 0..num_pps {
        pps_list.push(read_ps_nal(buf, &mut pos, "AVCDecoderConfigurationRecord")?);
    }
    Ok(AvcDecoderConfigRecord {
        length_size,
        sps_list,
        pps_list,
    })
}

/// HEVCDecoderConfigurationRecord (ISO/IEC 14496-15 §8.3.3.1.2)
///
/// PROP_VIDEO_CONFIG に載るのはボックスヘッダを含まないレコード本体である。
struct HevcDecoderConfigRecord {
    /// NAL 長フィールド幅
    length_size: LengthSize,
    /// VPS NAL のリスト
    vps_list: Vec<Vec<u8>>,
    /// SPS NAL のリスト
    sps_list: Vec<Vec<u8>>,
    /// PPS NAL のリスト
    pps_list: Vec<Vec<u8>>,
}

/// HEVCDecoderConfigurationRecord をパースする
fn parse_hevc_decoder_config_record(buf: &[u8]) -> Result<HevcDecoderConfigRecord> {
    /// numOfArrays までを含むレコードの最小サイズ
    const HEVC_CONFIG_MIN_LEN: usize = 23;

    if buf.len() < HEVC_CONFIG_MIN_LEN {
        return Err(Error::Other(format!(
            "HEVCDecoderConfigurationRecord is too short: {}",
            buf.len()
        )));
    }
    if buf[0] != 1 {
        return Err(Error::Other(format!(
            "unsupported HEVCDecoderConfigurationRecord configurationVersion: {}",
            buf[0]
        )));
    }
    let length_size = LengthSize::from_length_size_minus_one(buf[21] & 0x03).map_err(|e| {
        Error::Other(format!(
            "invalid HEVCDecoderConfigurationRecord lengthSize: {e}"
        ))
    })?;
    let num_arrays = usize::from(buf[22]);
    let mut pos = HEVC_CONFIG_MIN_LEN;
    let mut vps_list = Vec::new();
    let mut sps_list = Vec::new();
    let mut pps_list = Vec::new();
    for _ in 0..num_arrays {
        if buf.len() < pos + 1 {
            return Err(Error::Other(
                "HEVCDecoderConfigurationRecord is truncated at an array header".to_string(),
            ));
        }
        let nal_type = buf[pos] & 0x3F;
        pos += 1;
        if buf.len() < pos + 2 {
            return Err(Error::Other(
                "HEVCDecoderConfigurationRecord is truncated at an array count".to_string(),
            ));
        }
        let count = usize::from(u16::from_be_bytes([buf[pos], buf[pos + 1]]));
        pos += 2;
        for _ in 0..count {
            let nal = read_ps_nal(buf, &mut pos, "HEVCDecoderConfigurationRecord")?;
            match nal_type {
                // ITU-T H.265 §7.4.2.2 Table 7-1
                32 => vps_list.push(nal),
                33 => sps_list.push(nal),
                34 => pps_list.push(nal),
                _ => {}
            }
        }
    }
    if vps_list.is_empty() || sps_list.is_empty() || pps_list.is_empty() {
        return Err(Error::Other(
            "HEVCDecoderConfigurationRecord must contain VPS, SPS and PPS".to_string(),
        ));
    }
    Ok(HevcDecoderConfigRecord {
        length_size,
        vps_list,
        sps_list,
        pps_list,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use shiguredo_mp4::bitstream::h264::{
        H264NalUnitType, collect_nal_units, parse_annexb_nal_units,
    };
    use shiguredo_mp4::demux::{Input, Mp4FileDemuxer};

    /// shiguredo_mp4 のテストデータ (`tests/testdata/h264-sps-pps-annexb.bin`) の
    /// SPS / PPS (Annex B、SPS 1 個 + PPS 1 個)
    const H264_SPS_PPS_ANNEXB: &[u8] = &[
        0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x1E, 0xAC, 0xD9, 0x40, 0xA0, 0x3D, 0xB0, 0x11,
        0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x03, 0x00, 0x32, 0x0F, 0x16, 0x2D, 0x96, 0x00,
        0x00, 0x00, 0x01, 0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0,
    ];

    /// shiguredo_mp4 のテストデータ (`tests/testdata/black-av1-config-obus.bin`) の
    /// AV1 の config OBUs (Sequence Header OBU)
    const AV1_CONFIG_OBUS: &[u8] = &[
        0x0A, 0x0B, 0x00, 0x00, 0x00, 0x24, 0xC4, 0xFF, 0xDF, 0x3F, 0xFE, 0x60, 0x10,
    ];

    /// shiguredo_mp4 のテストデータ (`tests/testdata/h265-vps-sps-pps-annexb.bin`) の VPS
    /// (start code を除いた NAL)
    const H265_VPS: &[u8] = &[
        0x40, 0x01, 0x0C, 0x01, 0xFF, 0xFF, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00,
        0x03, 0x00, 0x00, 0x03, 0x00, 0x5A, 0x95, 0x98, 0x09,
    ];

    /// shiguredo_mp4 のテストデータ (`tests/testdata/h265-vps-sps-pps-annexb.bin`) の SPS
    const H265_SPS: &[u8] = &[
        0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x03, 0x00, 0x5A, 0xA0, 0x05, 0x02, 0x01, 0xE1, 0x65, 0x95, 0x9A, 0x49, 0x32, 0xBC, 0x05,
        0xA0, 0x20, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00, 0x00, 0x03, 0x03, 0x21,
    ];

    /// shiguredo_mp4 のテストデータ (`tests/testdata/h265-vps-sps-pps-annexb.bin`) の PPS
    const H265_PPS: &[u8] = &[0x44, 0x01, 0xC1, 0x72, 0xB4, 0x62, 0x40];

    /// テスト用の出力パスを作る
    ///
    /// 前回クラッシュの残骸で偽陽性にならないよう、使用前に削除する。
    fn temp_path(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("moq-sub-mp4-{}-{name}.mp4", std::process::id()));
        std::fs::remove_file(&path).ok();
        path
    }

    /// MP4 を読み戻してトラック数と (トラック種別, timestamp, duration) を返す
    fn read_samples(path: &Path) -> (usize, Vec<(TrackKind, u64, u32)>) {
        let bytes = std::fs::read(path).expect("MP4 ファイルを読めること");
        let mut demuxer = Mp4FileDemuxer::new();
        while let Some(required) = demuxer.required_input() {
            let position = required.position as usize;
            demuxer.handle_input(Input {
                position: required.position,
                data: &bytes[position..],
            });
        }
        let track_count = demuxer
            .tracks()
            .expect("トラック情報を取得できること")
            .len();
        let mut samples = Vec::new();
        while let Some(sample) = demuxer.next_sample().expect("サンプルを読めること") {
            samples.push((sample.track.kind, sample.timestamp, sample.duration));
        }
        (track_count, samples)
    }

    /// テスト用の AVCDecoderConfigurationRecord を組み立てる
    fn build_test_avc_record(
        sps_list: &[&[u8]],
        pps_list: &[&[u8]],
        length_size_minus_one: u8,
    ) -> Vec<u8> {
        let mut buf = vec![
            1,
            0x64,
            0x00,
            0x1E,
            0xFC | length_size_minus_one,
            0xE0 | sps_list.len() as u8,
        ];
        for sps in sps_list {
            buf.extend_from_slice(&(sps.len() as u16).to_be_bytes());
            buf.extend_from_slice(sps);
        }
        buf.push(pps_list.len() as u8);
        for pps in pps_list {
            buf.extend_from_slice(&(pps.len() as u16).to_be_bytes());
            buf.extend_from_slice(pps);
        }
        buf
    }

    /// テスト用の HEVCDecoderConfigurationRecord を組み立てる
    fn build_test_hvcc_record(
        vps_list: &[&[u8]],
        sps_list: &[&[u8]],
        pps_list: &[&[u8]],
        length_size_minus_one: u8,
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(1);
        buf.extend_from_slice(&[0x01, 0x00, 0x00, 0x00, 0x00]);
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        buf.push(0x5A);
        buf.extend_from_slice(&[0xF0, 0x00]);
        buf.push(0xFC);
        buf.push(0xFD);
        buf.push(0xF8);
        buf.push(0xF8);
        buf.extend_from_slice(&[0x00, 0x00]);
        buf.push((3 << 3) | length_size_minus_one);
        buf.push(3);
        for (nal_type, nals) in [(32u8, vps_list), (33, sps_list), (34, pps_list)] {
            buf.push(0x80 | nal_type);
            buf.extend_from_slice(&(nals.len() as u16).to_be_bytes());
            for nal in nals {
                buf.extend_from_slice(&(nal.len() as u16).to_be_bytes());
                buf.extend_from_slice(nal);
            }
        }
        buf
    }

    /// テスト用の audio セットアップを返す
    fn test_audio_setup() -> Setup {
        Setup {
            video: None,
            audio: Some(AudioSetup {
                sample_rate: 48_000,
                channels: 1,
            }),
        }
    }

    /// timestamp が None なら変換しない
    #[test]
    fn to_microseconds_returns_none_without_timestamp() {
        let mut timescale = None;
        assert_eq!(to_microseconds(&mut timescale, None, None), None);
    }

    /// timescale があればマイクロ秒へ変換し、以降は観測値を再利用する
    #[test]
    fn to_microseconds_uses_observed_timescale() {
        let mut timescale = None;
        assert_eq!(
            to_microseconds(&mut timescale, Some(48_000), Some(48_000)),
            Some(1_000_000),
            "timescale 48000 の 48000 は 1 秒"
        );
        assert_eq!(
            to_microseconds(&mut timescale, Some(960), None),
            Some(20_000),
            "timescale を持たないサンプルは直前に観測した値を使う"
        );
    }

    /// timescale を一度も観測していなければ epoch マイクロ秒として扱う
    #[test]
    fn to_microseconds_falls_back_to_epoch_microseconds() {
        let mut timescale = None;
        assert_eq!(
            to_microseconds(&mut timescale, Some(1_700_000_000_000_000), None),
            Some(1_700_000_000_000_000),
            "LOC-04 §2.3.1.1 の既定 (Unix エポックからのマイクロ秒)"
        );
    }

    /// timescale 0 は不正なため既定 (epoch マイクロ秒) にフォールバックする
    #[test]
    fn to_microseconds_ignores_zero_timescale() {
        let mut timescale = None;
        assert_eq!(
            to_microseconds(&mut timescale, Some(1_000), Some(0)),
            Some(1_000),
            "timescale 0 は観測値として採用しない"
        );
        assert_eq!(timescale, None, "timescale 0 では状態を更新しない");
    }

    /// 巨大な timestamp でも飽和して panic しない
    #[test]
    fn to_microseconds_saturates_on_huge_timestamp() {
        let mut timescale = Some(1);
        assert_eq!(
            to_microseconds(&mut timescale, Some(u64::MAX), Some(1)),
            Some(u64::MAX),
            "u64 に収まらない値は飽和する"
        );
    }

    /// 期待尺以内のギャップはそのまま尺になる
    #[test]
    fn content_duration_uses_gap_within_expected_duration() {
        assert_eq!(
            content_duration_us(Some(20_000), 0, 20_000, None, Duration::ZERO, false),
            Some(20_000),
            "期待尺と同じギャップは確定する"
        );
    }

    /// 期待尺を大きく超えるギャップは後続の到着を待つ (到着順の入れ替わり対策)
    #[test]
    fn content_duration_waits_for_far_successor() {
        assert_eq!(
            content_duration_us(Some(2_000_000), 0, 33_333, None, Duration::ZERO, false),
            None,
            "遠い未来のサンプルは後続とみなさない"
        );
        assert_eq!(
            content_duration_us(
                Some(2_000_000),
                0,
                33_333,
                None,
                REORDER_HOLD_DURATION,
                false
            ),
            Some(2_000_000),
            "保持時間を超えたら実際のギャップとして確定する"
        );
        assert_eq!(
            content_duration_us(Some(2_000_000), 0, 33_333, None, Duration::ZERO, true),
            Some(2_000_000),
            "終了時はギャップをそのまま使う"
        );
        // 期待尺の 1.5 倍の境界 (ちょうどは確定、1 マイクロ秒超は待つ)
        assert_eq!(
            content_duration_us(Some(30_000), 0, 20_000, None, Duration::ZERO, false),
            Some(30_000),
            "期待尺の 1.5 倍ちょうどは確定する"
        );
        assert_eq!(
            content_duration_us(Some(30_001), 0, 20_000, None, Duration::ZERO, false),
            None,
            "期待尺の 1.5 倍を 1 マイクロ秒超えたら待つ"
        );
    }

    /// 同一 timestamp と後続なしは直前の尺 (無ければ期待尺) を使う
    #[test]
    fn content_duration_reuses_last_duration() {
        assert_eq!(
            content_duration_us(Some(0), 0, 20_000, Some(20_000), Duration::ZERO, false),
            Some(20_000),
            "差分 0 は直前の尺を使う"
        );
        assert_eq!(
            content_duration_us(None, 0, 20_000, Some(20_000), Duration::ZERO, true),
            Some(20_000),
            "終了時は直前の尺を使う"
        );
        assert_eq!(
            content_duration_us(None, 0, 20_000, None, Duration::ZERO, false),
            None,
            "後続が無ければ終了時まで待つ"
        );
        assert_eq!(
            content_duration_us(None, 0, 20_000, None, REORDER_HOLD_DURATION, false),
            None,
            "後続が無い場合は保持時間を超えても停止区間を失わないよう待つ"
        );
    }

    /// 直前の尺が期待尺より長い場合は期待尺に丸める (ギャップの再利用を防ぐ)
    #[test]
    fn content_duration_clamps_reused_duration() {
        assert_eq!(
            content_duration_us(None, 0, 20_000, Some(5_000_000), Duration::ZERO, true),
            Some(20_000),
            "直前の尺がギャップで膨張していても最後のサンプルには使わない"
        );
    }

    /// AVCDecoderConfigurationRecord から SPS / PPS / 長さフィールド幅を取り出せる
    #[test]
    fn parse_avc_decoder_config_record_extracts_parameter_sets() {
        let nals = parse_annexb_nal_units(H264_SPS_PPS_ANNEXB).expect("Annex B をパースできること");
        let sps_list = collect_nal_units(nals.iter().copied(), H264NalUnitType::Sps);
        let pps_list = collect_nal_units(nals.iter().copied(), H264NalUnitType::Pps);
        let record = build_test_avc_record(&sps_list, &pps_list, 3);

        let parsed = parse_avc_decoder_config_record(&record).expect("レコードをパースできること");
        assert_eq!(
            parsed.length_size,
            LengthSize::FourBytes,
            "長さフィールド幅"
        );
        assert_eq!(parsed.sps_list, sps_list, "SPS リスト");
        assert_eq!(parsed.pps_list, pps_list, "PPS リスト");
    }

    /// 切り詰められた AVCDecoderConfigurationRecord はエラーになる
    #[test]
    fn parse_avc_decoder_config_record_rejects_truncated_input() {
        assert!(
            parse_avc_decoder_config_record(&[]).is_err(),
            "空入力はエラー"
        );
        // SPS の宣言長が実データを超える
        let mut record = vec![1, 0x64, 0x00, 0x1E, 0xFF, 0xE1];
        record.extend_from_slice(&[0xFF, 0xFF]);
        assert!(
            parse_avc_decoder_config_record(&record).is_err(),
            "SPS の切り詰めはエラー"
        );
    }

    /// AVCDecoderConfigurationRecord の不正なフィールドはエラーになる
    #[test]
    fn parse_avc_decoder_config_record_rejects_invalid_fields() {
        let mut wrong_version = build_test_avc_record(&[&[0x67, 0x00]], &[&[0x68, 0x00]], 3);
        wrong_version[0] = 2;
        assert!(
            parse_avc_decoder_config_record(&wrong_version).is_err(),
            "configurationVersion 2 はエラー"
        );
        // lengthSizeMinusOne = 2 は ISO/IEC 14496-15 で予約
        let reserved_length = build_test_avc_record(&[&[0x67, 0x00]], &[&[0x68, 0x00]], 2);
        assert!(
            parse_avc_decoder_config_record(&reserved_length).is_err(),
            "予約値の lengthSize はエラー"
        );
        let no_sps = build_test_avc_record(&[], &[&[0x68, 0x00]], 3);
        assert!(
            parse_avc_decoder_config_record(&no_sps).is_err(),
            "SPS が無ければエラー"
        );
        let no_pps = build_test_avc_record(&[&[0x67, 0x00]], &[], 3);
        assert!(
            parse_avc_decoder_config_record(&no_pps).is_err(),
            "PPS が無ければエラー"
        );
    }

    /// HEVCDecoderConfigurationRecord から VPS / SPS / PPS を取り出せる
    #[test]
    fn parse_hevc_decoder_config_record_extracts_parameter_sets() {
        let vps = [0x40u8, 0x01];
        let sps = [0x42u8, 0x01, 0x02];
        let pps = [0x44u8, 0x01];
        let record = build_test_hvcc_record(&[&vps], &[&sps], &[&pps], 3);

        let parsed = parse_hevc_decoder_config_record(&record).expect("レコードをパースできること");
        assert_eq!(
            parsed.length_size,
            LengthSize::FourBytes,
            "長さフィールド幅"
        );
        assert_eq!(parsed.vps_list, vec![vps.to_vec()], "VPS リスト");
        assert_eq!(parsed.sps_list, vec![sps.to_vec()], "SPS リスト");
        assert_eq!(parsed.pps_list, vec![pps.to_vec()], "PPS リスト");
    }

    /// PPS を欠く HEVCDecoderConfigurationRecord はエラーになる
    #[test]
    fn parse_hevc_decoder_config_record_requires_all_parameter_sets() {
        let vps = [0x40u8, 0x01];
        let sps = [0x42u8, 0x01];
        let record = build_test_hvcc_record(&[&vps], &[&sps], &[], 3);
        assert!(
            parse_hevc_decoder_config_record(&record).is_err(),
            "PPS が無ければエラー"
        );
    }

    /// 切り詰められた HEVCDecoderConfigurationRecord はエラーになる
    #[test]
    fn parse_hevc_decoder_config_record_rejects_truncated_input() {
        let vps = [0x40u8, 0x01];
        let sps = [0x42u8, 0x01];
        let pps = [0x44u8, 0x01];
        let record = build_test_hvcc_record(&[&vps], &[&sps], &[&pps], 3);
        assert!(
            parse_hevc_decoder_config_record(&record[..22]).is_err(),
            "ヘッダの切り詰めはエラー"
        );
        // array の個数を 4 に偽装してデータを欠けさせる
        let mut truncated = record.clone();
        truncated[22] = 4;
        assert!(
            parse_hevc_decoder_config_record(&truncated).is_err(),
            "array の切り詰めはエラー"
        );
    }

    /// OpusHead の値が dOps に写る
    #[test]
    fn build_audio_entry_uses_opus_head_values() {
        let head = OpusHeadConfig {
            channel_count: 1,
            pre_skip: 312,
            input_sample_rate: 48_000,
            output_gain: -512,
        };
        let entry = build_audio_entry(48_000, 2, Some(&head));
        let SampleEntry::Opus(opus) = entry else {
            panic!("Opus sample entry になること");
        };
        assert_eq!(opus.audio.channelcount, 1, "Channel Count");
        assert_eq!(opus.dops_box.output_channel_count, 1, "dOps のチャンネル数");
        assert_eq!(opus.dops_box.pre_skip, 312, "Pre-skip");
        assert_eq!(opus.dops_box.input_sample_rate, 48_000, "Input Sample Rate");
        assert_eq!(opus.dops_box.output_gain, -512, "Output Gain");
    }

    /// OpusHead が無ければ catalog の値を使う
    #[test]
    fn build_audio_entry_falls_back_to_catalog_values() {
        let entry = build_audio_entry(48_000, 2, None);
        let SampleEntry::Opus(opus) = entry else {
            panic!("Opus sample entry になること");
        };
        assert_eq!(opus.audio.channelcount, 2, "catalog のチャンネル数");
        assert_eq!(opus.dops_box.pre_skip, 0, "Pre-skip は 0");
        assert_eq!(
            opus.dops_box.input_sample_rate, 48_000,
            "Input Sample Rate は catalog の値"
        );
        assert_eq!(opus.dops_box.output_gain, 0, "Output Gain は 0");
    }

    /// OpusHead のチャンネル数が未対応なら catalog の値にフォールバックする
    #[test]
    fn build_audio_entry_falls_back_on_unsupported_opus_channels() {
        let head = OpusHeadConfig {
            channel_count: 3,
            pre_skip: 0,
            input_sample_rate: 48_000,
            output_gain: 0,
        };
        let entry = build_audio_entry(48_000, 2, Some(&head));
        let SampleEntry::Opus(opus) = entry else {
            panic!("Opus sample entry になること");
        };
        assert_eq!(
            opus.audio.channelcount, 2,
            "未対応の Channel Count は catalog の値を使う"
        );
    }

    /// AV1 の config OBUs から av01 sample entry を構築できる
    #[test]
    fn build_video_entry_builds_av01_box() {
        let entry =
            build_video_entry("av01.0.08M.08", AV1_CONFIG_OBUS).expect("av01 を構築できること");
        let SampleEntry::Av01(av01) = entry else {
            panic!("Av01 sample entry になること");
        };
        assert_eq!(
            av01.av1c_box.config_obus, AV1_CONFIG_OBUS,
            "config OBUs が保持されること"
        );
        assert!(
            av01.visual.width > 0 && av01.visual.height > 0,
            "Sequence Header から解像度が導出されること"
        );
    }

    /// H.264 の avcC から avc1 sample entry を構築できる
    #[test]
    fn build_video_entry_builds_avc1_box() {
        let nals = parse_annexb_nal_units(H264_SPS_PPS_ANNEXB).expect("Annex B をパースできること");
        let sps_list = collect_nal_units(nals.iter().copied(), H264NalUnitType::Sps);
        let pps_list = collect_nal_units(nals.iter().copied(), H264NalUnitType::Pps);
        let record = build_test_avc_record(&sps_list, &pps_list, 3);

        let entry = build_video_entry("avc1.64001E", &record).expect("avc1 を構築できること");
        let SampleEntry::Avc1(avc1) = entry else {
            panic!("Avc1 sample entry になること");
        };
        assert_eq!(avc1.avcc_box.sps_list, sps_list, "SPS リスト");
        assert_eq!(avc1.avcc_box.pps_list, pps_list, "PPS リスト");
        assert_eq!(
            avc1.visual.width, 640,
            "SPS から解像度が導出されること (fixture は 640x360)"
        );
    }

    /// H.265 の hvcC から hvc1 / hev1 sample entry を構築できる
    #[test]
    fn build_video_entry_builds_hvc_boxes() {
        let record = build_test_hvcc_record(&[H265_VPS], &[H265_SPS], &[H265_PPS], 3);

        let entry = build_video_entry("hvc1.1.6.L93.B0", &record).expect("hvc1 を構築できること");
        let SampleEntry::Hvc1(hvc1) = entry else {
            panic!("Hvc1 sample entry になること");
        };
        assert_eq!(
            hvc1.hvcc_box.nalu_arrays.len(),
            3,
            "VPS / SPS / PPS の 3 配列"
        );
        assert!(
            hvc1.visual.width > 0 && hvc1.visual.height > 0,
            "SPS から解像度が導出されること"
        );

        let entry = build_video_entry("hev1.1.6.L93.B0", &record).expect("hev1 を構築できること");
        let SampleEntry::Hev1(hev1) = entry else {
            panic!("Hev1 sample entry になること");
        };
        assert!(
            hev1.visual.width > 0 && hev1.visual.height > 0,
            "SPS から解像度が導出されること"
        );
    }

    /// 未対応の video codec はエラーになる
    #[test]
    fn build_video_entry_rejects_unsupported_codec() {
        assert!(
            build_video_entry("vp09.00.10.08", &[0x00]).is_err(),
            "未対応 codec はエラー"
        );
    }

    /// video のキーフレームが来るまでファイルを作らない
    #[test]
    fn recorder_does_not_create_file_without_video_keyframe() {
        let path = temp_path("no-keyframe");
        {
            let mut writer = Writer::new(
                path.clone(),
                Setup {
                    video: Some(VideoSetup {
                        codec: "av01.0.08M.08".to_string(),
                        fps: 30,
                    }),
                    audio: None,
                },
            );
            // キーフレームでないサンプルは録画を開始しない
            writer.handle_video(VideoSample {
                timestamp: Some(0),
                timescale: Some(90_000),
                keyframe: false,
                config: None,
                data: vec![0x33; 4],
            });
            writer.finish().expect("finalize に成功すること");
        }
        assert!(
            !path.exists(),
            "キーフレームが無ければファイルを作らないこと"
        );
    }

    /// audio のみの録画で到着順が前後しても timestamp 昇順に書き出される
    #[test]
    fn recorder_writes_audio_samples_in_timestamp_order() {
        let path = temp_path("audio-order");
        {
            let mut writer = Writer::new(path.clone(), test_audio_setup());
            // 意図的に到着順を入れ替える (960 -> 0 -> 1920)
            for timestamp in [960u64, 0, 1920] {
                writer.handle_audio(AudioSample {
                    timestamp: Some(timestamp),
                    timescale: Some(48_000),
                    config: None,
                    data: vec![0xAA; 4],
                });
            }
            writer.finish().expect("finalize に成功すること");
        }

        let (track_count, samples) = read_samples(&path);
        assert_eq!(track_count, 1, "audio トラックのみ");
        assert_eq!(
            samples,
            vec![
                (TrackKind::Audio, 0, 20_000),
                (TrackKind::Audio, 20_000, 20_000),
                (TrackKind::Audio, 40_000, 20_000),
            ],
            "到着順ではなく timestamp 昇順で書き出されること"
        );
        std::fs::remove_file(&path).ok();
    }

    /// 遠い未来のサンプルが先着しても duration が膨張しない
    ///
    /// 後続 GOP のキーフレームが中間サンプルより先に到着する状況を模す。
    #[test]
    fn recorder_does_not_inflate_duration_for_far_successor() {
        let path = temp_path("far-successor");
        {
            let mut writer = Writer::new(path.clone(), test_audio_setup());
            // 0 -> 2 秒 -> 20 ms の順で到着する
            for timestamp in [0u64, 2_000_000, 20_000] {
                writer.handle_audio(AudioSample {
                    timestamp: Some(timestamp),
                    timescale: Some(1_000_000),
                    config: None,
                    data: vec![0xAA; 4],
                });
            }
            writer.finish().expect("finalize に成功すること");
        }

        let (_, samples) = read_samples(&path);
        assert_eq!(
            samples,
            vec![
                (TrackKind::Audio, 0, 20_000),
                (TrackKind::Audio, 20_000, 1_980_000),
                (TrackKind::Audio, 2_000_000, 20_000),
            ],
            "遅れて届いたサンプルでタイムラインが膨張しないこと"
        );
        std::fs::remove_file(&path).ok();
    }

    /// 同一 timestamp のサンプルを 2 件とも書き出せる
    #[test]
    fn recorder_writes_duplicate_timestamps() {
        let path = temp_path("duplicate-timestamps");
        {
            let mut writer = Writer::new(path.clone(), test_audio_setup());
            for timestamp in [0u64, 0, 960] {
                writer.handle_audio(AudioSample {
                    timestamp: Some(timestamp),
                    timescale: Some(48_000),
                    config: None,
                    data: vec![0xAA; 4],
                });
            }
            writer.finish().expect("finalize に成功すること");
        }

        let (_, samples) = read_samples(&path);
        assert_eq!(
            samples,
            vec![
                (TrackKind::Audio, 0, 20_000),
                (TrackKind::Audio, 20_000, 20_000),
                (TrackKind::Audio, 40_000, 20_000),
            ],
            "同一 timestamp で停止せず、直前の尺で書き出されること"
        );
        std::fs::remove_file(&path).ok();
    }

    /// 書き出し済みの時刻以前に到着したサンプルは破棄される
    #[test]
    fn recorder_discards_samples_written_after_timeline() {
        let path = temp_path("late-arrival");
        {
            let mut writer = Writer::new(path.clone(), test_audio_setup());
            // 0 / 20 ms / 40 ms を順に受け取り、0 と 20 ms を書き出させる
            for timestamp in [0u64, 20_000, 40_000] {
                writer.handle_audio(AudioSample {
                    timestamp: Some(timestamp),
                    timescale: Some(1_000_000),
                    config: None,
                    data: vec![0xAA; 4],
                });
            }
            // 20 ms は書き出し済みのため、10 ms のサンプルは昇順を保てず破棄される
            writer.handle_audio(AudioSample {
                timestamp: Some(10_000),
                timescale: Some(1_000_000),
                config: None,
                data: vec![0xCC; 4],
            });
            writer.finish().expect("finalize に成功すること");
        }

        let (_, samples) = read_samples(&path);
        assert_eq!(
            samples
                .iter()
                .map(|(_, timestamp, _)| *timestamp)
                .collect::<Vec<_>>(),
            vec![0, 20_000, 40_000],
            "書き出し済みの時刻以前のサンプルは含まれないこと"
        );
        std::fs::remove_file(&path).ok();
    }

    /// video のキーフレームを基準に audio の先頭オフセットを加算する
    #[test]
    fn recorder_aligns_tracks_to_the_first_video_keyframe() {
        let path = temp_path("track-offset");
        {
            let mut writer = Writer::new(
                path.clone(),
                Setup {
                    video: Some(VideoSetup {
                        codec: "av01.0.08M.08".to_string(),
                        fps: 30,
                    }),
                    audio: Some(AudioSetup {
                        sample_rate: 48_000,
                        channels: 1,
                    }),
                },
            );
            // video のキーフレームより前の audio は録画しない
            writer.handle_audio(AudioSample {
                timestamp: Some(0),
                timescale: Some(48_000),
                config: None,
                data: vec![0x00; 4],
            });
            // video のキーフレーム (timescale 90000 の 90000 = 1 秒) が基準になる
            writer.handle_video(VideoSample {
                timestamp: Some(90_000),
                timescale: Some(90_000),
                keyframe: true,
                config: Some(AV1_CONFIG_OBUS.to_vec()),
                data: vec![0x11; 4],
            });
            // 基準の 20 ms 後に始まる audio は先頭にオフセットが加算される
            for timestamp in [48_960u64, 49_920] {
                writer.handle_audio(AudioSample {
                    timestamp: Some(timestamp),
                    timescale: Some(48_000),
                    config: None,
                    data: vec![0x22; 4],
                });
            }
            writer.finish().expect("finalize に成功すること");
        }

        let (track_count, samples) = read_samples(&path);
        assert_eq!(track_count, 2, "video と audio の 2 トラック");
        let video: Vec<_> = samples
            .iter()
            .filter(|(kind, _, _)| *kind == TrackKind::Video)
            .collect();
        let audio: Vec<_> = samples
            .iter()
            .filter(|(kind, _, _)| *kind == TrackKind::Audio)
            .collect();
        assert_eq!(video.len(), 1, "video はキーフレームの 1 サンプル");
        assert_eq!(
            video[0].1, 0,
            "video トラックの先頭サンプルはトラック内の 0 から始まる"
        );
        assert_eq!(video[0].2, 33_333, "最後の video は fps 由来の期待尺");
        assert_eq!(audio.len(), 2, "基準より前の audio は録画されない");
        assert_eq!(
            audio[0].1, 0,
            "audio トラックの先頭もトラック内の 0 から始まる"
        );
        assert_eq!(
            audio[0].2, 40_000,
            "audio の先頭 duration には基準時刻との差 20 ms が加算される"
        );
        assert_eq!(audio[1].1, 40_000, "audio の 2 番目");
        assert_eq!(audio[1].2, 20_000, "最後の audio は直前の duration");
        std::fs::remove_file(&path).ok();
    }

    /// video の sample entry 構築に失敗しても audio の録画は継続する
    #[test]
    fn recorder_isolates_failed_video_entry() {
        let path = temp_path("entry-failure");
        {
            let mut writer = Writer::new(
                path.clone(),
                Setup {
                    video: Some(VideoSetup {
                        codec: "avc1.64001E".to_string(),
                        fps: 30,
                    }),
                    audio: Some(AudioSetup {
                        sample_rate: 48_000,
                        channels: 1,
                    }),
                },
            );
            // 不正な avcC (configurationVersion が 2) を持つキーフレーム
            writer.handle_video(VideoSample {
                timestamp: Some(0),
                timescale: Some(90_000),
                keyframe: true,
                config: Some(vec![2, 0x64, 0x00, 0x1E, 0xFF, 0xE1, 0x00]),
                data: vec![0x11; 4],
            });
            for timestamp in [0u64, 960] {
                writer.handle_audio(AudioSample {
                    timestamp: Some(timestamp),
                    timescale: Some(48_000),
                    config: None,
                    data: vec![0x22; 4],
                });
            }
            writer.finish().expect("finalize に成功すること");
        }

        let (track_count, samples) = read_samples(&path);
        assert_eq!(track_count, 1, "video トラックは作られない");
        assert_eq!(samples.len(), 2, "audio は録画が継続すること");
        assert!(
            samples.iter().all(|(kind, _, _)| *kind == TrackKind::Audio),
            "残るのは audio トラックのみ"
        );
        std::fs::remove_file(&path).ok();
    }

    /// Timescale が混在するトラックでも片方が全滅せず録画される
    ///
    /// LOC-04 §2.3.1.1 は Timescale が無い場合を epoch マイクロ秒と定めるため、
    /// media time のトラックと epoch マイクロ秒のトラックが混在し得る。
    #[test]
    fn recorder_keeps_tracks_with_mixed_time_bases() {
        let path = temp_path("mixed-time-bases");
        {
            let mut writer = Writer::new(
                path.clone(),
                Setup {
                    video: Some(VideoSetup {
                        codec: "av01.0.08M.08".to_string(),
                        fps: 30,
                    }),
                    audio: Some(AudioSetup {
                        sample_rate: 48_000,
                        channels: 1,
                    }),
                },
            );
            // video は media time (timescale 90000)、audio は Timescale なし (epoch マイクロ秒)
            writer.handle_video(VideoSample {
                timestamp: Some(90_000),
                timescale: Some(90_000),
                keyframe: true,
                config: Some(AV1_CONFIG_OBUS.to_vec()),
                data: vec![0x11; 4],
            });
            for timestamp in [1_700_000_000_000_000u64, 1_700_000_001_000_000] {
                writer.handle_audio(AudioSample {
                    timestamp: Some(timestamp),
                    timescale: None,
                    config: None,
                    data: vec![0x22; 4],
                });
            }
            writer.finish().expect("finalize に成功すること");
        }

        let (track_count, samples) = read_samples(&path);
        assert_eq!(track_count, 2, "video と audio の両トラックが残ること");
        assert_eq!(
            samples
                .iter()
                .filter(|(kind, _, _)| *kind == TrackKind::Audio)
                .count(),
            2,
            "audio が全滅しないこと"
        );
        std::fs::remove_file(&path).ok();
    }

    /// 書き出し済みサンプルの終端と同時刻のサンプルは受理される
    #[test]
    fn recorder_accepts_sample_at_flushed_timeline_end() {
        let path = temp_path("timeline-end");
        {
            let mut writer = Writer::new(path.clone(), test_audio_setup());
            // 0 と 20 ms を順に受け取り、0 を 20 ms で書き出す (終端は 20 ms)
            writer.handle_audio(AudioSample {
                timestamp: Some(0),
                timescale: Some(48_000),
                config: None,
                data: vec![0xAA; 4],
            });
            writer.handle_audio(AudioSample {
                timestamp: Some(960),
                timescale: Some(48_000),
                config: None,
                data: vec![0xBB; 4],
            });
            // 書き出し済み終端 (20 ms) と同時刻のサンプルは遅着でも受理される
            writer.handle_audio(AudioSample {
                timestamp: Some(960),
                timescale: Some(48_000),
                config: None,
                data: vec![0xCC; 4],
            });
            writer.finish().expect("finalize に成功すること");
        }

        let (_, samples) = read_samples(&path);
        assert_eq!(
            samples,
            vec![
                (TrackKind::Audio, 0, 20_000),
                (TrackKind::Audio, 20_000, 20_000),
                (TrackKind::Audio, 40_000, 20_000),
            ],
            "終端と同時刻のサンプルが書き出されること"
        );
        std::fs::remove_file(&path).ok();
    }

    /// video のキーフレーム待ちで破棄した audio の OpusHead が dOps に反映される
    #[test]
    fn recorder_keeps_opus_head_from_dropped_audio() {
        let path = temp_path("opus-head-retained");
        {
            let mut writer = Writer::new(
                path.clone(),
                Setup {
                    video: Some(VideoSetup {
                        codec: "av01.0.08M.08".to_string(),
                        fps: 30,
                    }),
                    audio: Some(AudioSetup {
                        sample_rate: 48_000,
                        channels: 1,
                    }),
                },
            );
            // video のキーフレームより前の audio (OpusHead 付き) は破棄されるが、head は保持される
            writer.handle_audio(AudioSample {
                timestamp: Some(0),
                timescale: Some(48_000),
                config: Some(OpusHeadConfig {
                    channel_count: 1,
                    pre_skip: 312,
                    input_sample_rate: 48_000,
                    output_gain: 0,
                }),
                data: vec![0xAA; 4],
            });
            writer.handle_video(VideoSample {
                timestamp: Some(0),
                timescale: Some(90_000),
                keyframe: true,
                config: Some(AV1_CONFIG_OBUS.to_vec()),
                data: vec![0x11; 4],
            });
            writer.handle_audio(AudioSample {
                timestamp: Some(960),
                timescale: Some(48_000),
                config: None,
                data: vec![0xBB; 4],
            });
            writer.finish().expect("finalize に成功すること");
        }

        let bytes = std::fs::read(&path).expect("MP4 ファイルを読めること");
        let mut demuxer = Mp4FileDemuxer::new();
        while let Some(required) = demuxer.required_input() {
            let position = required.position as usize;
            demuxer.handle_input(Input {
                position: required.position,
                data: &bytes[position..],
            });
        }
        demuxer.tracks().expect("トラック情報を取得できること");
        let mut audio_entry = None;
        while let Some(sample) = demuxer.next_sample().expect("サンプルを読めること") {
            if sample.track.kind == TrackKind::Audio && audio_entry.is_none() {
                audio_entry = sample.sample_entry.cloned();
            }
        }
        let Some(SampleEntry::Opus(opus)) = audio_entry else {
            panic!("audio の Opus sample entry が取得できること");
        };
        assert_eq!(
            opus.dops_box.pre_skip, 312,
            "破棄した audio の OpusHead の Pre-skip が dOps に反映されること"
        );
        std::fs::remove_file(&path).ok();
    }

    /// 遅れて始まるトラックのサンプルが offset 込みの境界で誤破棄されない
    #[test]
    fn recorder_keeps_samples_after_late_track_start() {
        let path = temp_path("late-track-start");
        {
            let mut writer = Writer::new(
                path.clone(),
                Setup {
                    video: Some(VideoSetup {
                        codec: "av01.0.08M.08".to_string(),
                        fps: 30,
                    }),
                    audio: Some(AudioSetup {
                        sample_rate: 48_000,
                        channels: 1,
                    }),
                },
            );
            // video のキーフレームが録画の基準になる
            writer.handle_video(VideoSample {
                timestamp: Some(0),
                timescale: Some(90_000),
                keyframe: true,
                config: Some(AV1_CONFIG_OBUS.to_vec()),
                data: vec![0x11; 4],
            });
            // audio は 40 ms 遅れて始まり、60 ms / 80 ms と続く (1920 / 2880 / 3840 ticks)
            for timestamp in [1_920u64, 2_880, 3_840] {
                writer.handle_audio(AudioSample {
                    timestamp: Some(timestamp),
                    timescale: Some(48_000),
                    config: None,
                    data: vec![0x22; 4],
                });
            }
            writer.finish().expect("finalize に成功すること");
        }

        let (_, samples) = read_samples(&path);
        let audio: Vec<_> = samples
            .iter()
            .filter(|(kind, _, _)| *kind == TrackKind::Audio)
            .collect();
        assert_eq!(
            audio.len(),
            3,
            "遅れて始まるトラックのサンプルが誤って破棄されないこと"
        );
        assert_eq!(
            audio[0].2, 60_000,
            "先頭サンプルだけに基準時刻との差 (40 ms) が加算される"
        );
        assert_eq!(audio[1].2, 20_000, "2 番目以降は差分がそのまま尺になる");
        std::fs::remove_file(&path).ok();
    }

    /// 録画対象サンプルが 1 件も無い場合はファイルを作らない
    #[test]
    fn recorder_does_not_create_file_without_samples() {
        let path = temp_path("no-samples");
        {
            let mut writer = Writer::new(path.clone(), test_audio_setup());
            writer.finish().expect("finalize に成功すること");
        }
        assert!(!path.exists(), "サンプルが無ければファイルを作らないこと");
    }

    /// ファイル作成に失敗した場合はエラーを返す
    #[test]
    fn recorder_reports_file_creation_failure() {
        // 存在しないディレクトリの下を出力先にする
        let path = std::env::temp_dir()
            .join(format!("moq-sub-mp4-{}-missing-dir", std::process::id()))
            .join("out.mp4");
        std::fs::remove_dir_all(path.parent().expect("parent exists")).ok();
        let mut writer = Writer::new(path.clone(), test_audio_setup());
        writer.handle_audio(AudioSample {
            timestamp: Some(0),
            timescale: Some(1_000_000),
            config: None,
            data: vec![0xAA; 4],
        });
        let result = writer.finish();
        assert!(
            result.is_err(),
            "ファイル作成に失敗したら finalize でエラーを返すこと"
        );
        assert!(!path.exists(), "ファイルは作られないこと");
    }

    /// RecorderSender の clone が生きていても finish が返る
    #[test]
    fn recorder_finishes_while_sender_clone_is_alive() {
        let path = temp_path("finish-with-clone");
        let recorder = Recorder::start(path.clone(), test_audio_setup())
            .expect("ライタースレッドを起動できること");
        let sender = recorder.sender();
        sender.audio(AudioSample {
            timestamp: Some(0),
            timescale: Some(1_000_000),
            config: None,
            data: vec![0xAA; 4],
        });
        // sender を drop せずに finish する
        recorder.finish().expect("finalize に成功すること");

        let (track_count, samples) = read_samples(&path);
        assert_eq!(track_count, 1, "audio トラックが確定すること");
        assert_eq!(samples.len(), 1, "サンプルが書き出されること");
        drop(sender);
        std::fs::remove_file(&path).ok();
    }
}
