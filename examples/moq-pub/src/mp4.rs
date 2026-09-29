//! MP4 ファイルの映像トラックを読み出すリーダー
//!
//! エンコード済みサンプルを再エンコードせずに配信するパススルー送信で使う。
//! ファイル全体をメモリに読み込んで `shiguredo_mp4` の `Mp4FileDemuxer` で demux し、
//! MSF catalog に必要な情報 (codec 文字列 / 解像度 / フレームレート / ビットレート) と
//! `PROP_VIDEO_CONFIG` 用の設定データを取り出す。
//! サンプルの供給は専用スレッドで行い、MP4 のタイムスタンプに従って実時間でペーシングする。
//!
//! B フレーム (composition time offset が非ゼロのサンプル) を含む MP4 は拒否する。
//! パススルーではサンプルをデコード順のまま送るため、表示順とのずれを表現できない。

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use shiguredo_mp4::Decode;
use shiguredo_mp4::Encode;
use shiguredo_mp4::TrackKind;
use shiguredo_mp4::boxes::{HvccBox, SampleEntry};
use shiguredo_mp4::codec_string;
use shiguredo_mp4::demux::{Input, Mp4FileDemuxer};
use tokio::sync::mpsc;

use crate::encoder::EncodedFrame;
use crate::error::{Error, Result};
use crate::fake_capture::sleep_interruptibly;
use crate::pipeline::VideoInput;

/// パススルー配信に必要な映像トラック情報
#[derive(Debug)]
pub struct VideoTrackInfo {
    /// RFC 6381 形式の codec 文字列 (例: `avc1.640028`)
    pub codec: String,
    /// 映像幅 (px)
    pub width: u32,
    /// 映像高さ (px)
    pub height: u32,
    /// 平均フレームレート (四捨五入)
    pub fps: u32,
    /// 最大ビットレート (kbps、切り上げ)
    pub bitrate_kbps: u32,
    /// タイムスケール (1 秒あたりの timestamp 単位数)
    pub timescale: u64,
}

/// パススルー対象の映像コーデック
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassthroughCodec {
    /// AV1
    Av1,
    /// H.264
    H264,
    /// H.265
    H265,
}

/// ビットレート算出に使うサンプルのメタデータ
struct SampleMeta {
    /// サンプルのタイムスタンプ (トラックの timescale 単位)
    timestamp: u64,
    /// サンプルの尺 (トラックの timescale 単位)
    duration: u32,
    /// サンプルデータのサイズ (バイト)
    size: u64,
}

/// demux した 1 サンプル分の供給データ
struct VideoSample {
    /// キーフレームかどうか
    keyframe: bool,
    /// サンプルのタイムスタンプ (トラックの timescale 単位)
    timestamp: u64,
    /// 圧縮済みビットストリーム
    data: Vec<u8>,
}

/// MP4 ファイルの映像トラックのリーダー
pub struct Mp4VideoReader {
    /// ファイル全体 (サンプルはこのバッファ内のオフセットを指す)
    data: Vec<u8>,
    /// ファイル全体を入力済みの demuxer
    demuxer: Mp4FileDemuxer,
    /// 配信する映像トラックの Track ID
    track_id: u32,
    /// 映像コーデック
    codec: PassthroughCodec,
    /// catalog 用のトラック情報
    info: VideoTrackInfo,
    /// `PROP_VIDEO_CONFIG` に載せる設定データ (avcC / hvcC のレコード本体、AV1 は config OBUs)
    video_config: Vec<u8>,
    /// 1 周分のメディア尺 (最終サンプルの timestamp + duration)
    loop_duration: u64,
    /// 周回を重ねた分のタイムスタンプ加算値
    loop_offset: u64,
    /// 現在の周回のペーシング基準時刻
    loop_start: Instant,
    /// 周回の先頭で最初のキーフレームまで読み飛ばすかどうか
    need_keyframe: bool,
}

impl Mp4VideoReader {
    /// MP4 ファイルを開いて映像トラックを検証する
    ///
    /// ファイル全体をメモリに読み込んで demux し、配信に必要なメタデータを集計する。
    /// 次の場合はエラーになる。
    ///
    /// - 映像トラックが無い、または映像サンプルが 1 つも無い
    /// - 対応していない映像コーデック (AV1 / H.264 / H.265 以外)
    /// - B フレームを含む (composition time offset が非ゼロのサンプルがある)
    /// - 映像トラックにキーフレームが 1 つも無い
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let data = std::fs::read(path).map_err(|e| {
            Error::Other(format!("failed to read MP4 file '{}': {e}", path.display()))
        })?;
        let mut demuxer = Mp4FileDemuxer::new();
        demuxer.handle_input(Input {
            position: 0,
            data: &data,
        });
        // ファイル全体を渡しているため、正常なファイルなら追加の入力は要求されない
        if let Some(required) = demuxer.required_input() {
            return Err(Error::Other(format!(
                "failed to read MP4 file '{}': need more data at position {}",
                path.display(),
                required.position,
            )));
        }

        let (track_id, timescale) = {
            let tracks = demuxer.tracks().map_err(|e| {
                Error::Other(format!("failed to read MP4 file '{}': {e}", path.display()))
            })?;
            let track = tracks
                .iter()
                .find(|t| t.kind == TrackKind::Video)
                .ok_or_else(|| {
                    Error::Other(format!("MP4 file '{}' has no video track", path.display()))
                })?;
            (track.track_id, u64::from(track.timescale.get()))
        };

        // 全サンプルを走査して、catalog 用の値の算出と検証を行う
        let mut metas: Vec<SampleMeta> = Vec::new();
        let mut sample_entry: Option<SampleEntry> = None;
        let mut first_keyframe_index: Option<usize> = None;
        while let Some(sample) = demuxer
            .next_sample()
            .map_err(|e| Error::Other(format!("failed to read MP4 sample: {e}")))?
        {
            if sample.track.track_id != track_id {
                continue;
            }
            // B フレームがあると、デコード順のサンプルをそのまま送るパススルーでは表示順を
            // 再現できない (composition time offset が非ゼロのサンプルが B フレーム)
            if sample
                .composition_time_offset
                .is_some_and(|offset| offset != 0)
            {
                return Err(Error::Other(format!(
                    "MP4 file '{}' contains B frames; they are not supported by passthrough publishing",
                    path.display()
                )));
            }
            if sample_entry.is_none() {
                sample_entry = sample.sample_entry.cloned();
            }
            if sample.keyframe && first_keyframe_index.is_none() {
                first_keyframe_index = Some(metas.len());
            }
            // 再生時に読めないサンプルがあれば、開始前にエラーとして報告する
            let offset = sample.data_offset as usize;
            if offset
                .checked_add(sample.data_size)
                .is_none_or(|end| end > data.len())
            {
                return Err(Error::Other(format!(
                    "MP4 file '{}' has a sample outside the file bounds",
                    path.display()
                )));
            }
            metas.push(SampleMeta {
                timestamp: sample.timestamp,
                duration: sample.duration,
                size: sample.data_size as u64,
            });
        }

        if metas.is_empty() {
            return Err(Error::Other(format!(
                "MP4 file '{}' has no video samples",
                path.display()
            )));
        }
        let Some(entry) = sample_entry else {
            return Err(Error::Other(format!(
                "MP4 file '{}' has no video sample entry",
                path.display()
            )));
        };
        let Some(first_keyframe_index) = first_keyframe_index else {
            return Err(Error::Other(format!(
                "MP4 file '{}' has no keyframe in the video track",
                path.display()
            )));
        };
        // metas が空でないことは上で確認済み
        let last = metas
            .last()
            .expect("metas must not be empty; checked above");
        let loop_duration = last
            .timestamp
            .checked_add(u64::from(last.duration))
            .ok_or_else(|| Error::Other("MP4 duration is out of range".to_string()))?;

        let codec = codec_string::from_sample_entry(&entry).map_err(|e| {
            Error::Other(format!(
                "failed to detect the codec of MP4 file '{}': {e}",
                path.display()
            ))
        })?;
        let Some((passthrough_codec, video_config)) = passthrough_codec_and_config(&entry)? else {
            return Err(Error::Other(format!(
                "unsupported video codec in MP4 file '{}': {codec} (supported: AV1 / H.264 / H.265)",
                path.display()
            )));
        };
        let (width, height) = entry.video_resolution().ok_or_else(|| {
            Error::Other(format!(
                "MP4 file '{}' has no video resolution",
                path.display()
            ))
        })?;

        let sample_count = metas.len() as u64;
        let fps = average_fps(sample_count, loop_duration, timescale)?;
        let bitrate_kbps = max_bitrate_kbps(&metas, timescale, loop_duration)?;

        if first_keyframe_index > 0 {
            tracing::warn!(
                "MP4 file '{}' has {} sample(s) before the first keyframe; they are skipped while publishing",
                path.display(),
                first_keyframe_index,
            );
        }
        if metas.iter().any(|m| m.duration != metas[0].duration) {
            tracing::warn!(
                "MP4 file '{}' has variable frame durations; the catalog framerate is the average {} fps",
                path.display(),
                fps,
            );
        }
        tracing::info!(
            "MP4 loaded: path={} codec={} size={}x{} fps={} bitrate={}kbps timescale={} samples={}",
            path.display(),
            codec,
            width,
            height,
            fps,
            bitrate_kbps,
            timescale,
            sample_count,
        );

        demuxer.seek(Duration::ZERO).map_err(|e| {
            Error::Other(format!("failed to seek MP4 file '{}': {e}", path.display()))
        })?;

        Ok(Self {
            data,
            demuxer,
            track_id,
            codec: passthrough_codec,
            info: VideoTrackInfo {
                codec,
                width: u32::from(width),
                height: u32::from(height),
                fps,
                bitrate_kbps,
                timescale,
            },
            video_config,
            loop_duration,
            loop_offset: 0,
            loop_start: Instant::now(),
            need_keyframe: true,
        })
    }

    /// catalog 用の映像トラック情報を返す
    pub fn info(&self) -> &VideoTrackInfo {
        &self.info
    }

    /// 実時間ペーシングでフレームを供給する専用スレッドを開始する
    ///
    /// 戻り値を drop すると停止要求を出してスレッドを join する。
    pub fn start(self, sender: mpsc::Sender<VideoInput>) -> Result<Mp4VideoSource> {
        tracing::info!(
            "MP4 passthrough reader starting: codec={} size={}x{} fps={}",
            self.info.codec,
            self.info.width,
            self.info.height,
            self.info.fps,
        );
        let mut reader = self;
        // ペーシングの起点はスレッド開始時刻にする (catalog 送信などで open から時間が
        // 経過していても、開始直後に追い上げ送信しないようにするため)
        reader.loop_start = Instant::now();

        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("mp4-reader".to_string())
            .spawn(move || run_reader(reader, stop_thread, sender))
            .map_err(|e| Error::Other(format!("failed to spawn MP4 reader thread: {e}")))?;

        Ok(Mp4VideoSource {
            stop_flag: stop,
            handle: Some(handle),
        })
    }

    /// 次の映像フレームを実時間ペーシングで返す
    ///
    /// 停止要求を受けた場合は `Ok(None)` を返す。末尾に達したら先頭に戻り、1 周分の
    /// メディア尺をタイムスタンプに加算して単調増加させる。各周回は最初のキーフレームから
    /// 開始し、それより前のサンプルは読み飛ばす (group はキーフレームでしか開始できない)。
    fn next_frame_paced(&mut self, stop: &AtomicBool) -> Result<Option<EncodedFrame>> {
        loop {
            let Some(sample) = self.next_video_sample()? else {
                // 1 周分のメディア尺 (最終サンプルの尺を含む) が経過するまで待ってから
                // 次の周回へ入る。待たずに再開すると、周回ごとに最終サンプルの尺だけ
                // 実時間より速く進んでしまう。
                let loop_end = self
                    .loop_start
                    .checked_add(media_time_to_duration(
                        self.loop_duration,
                        self.info.timescale,
                    ))
                    .ok_or_else(|| {
                        Error::Other("MP4 pacing deadline is out of range".to_string())
                    })?;
                let now = Instant::now();
                if loop_end > now {
                    if sleep_interruptibly(stop, loop_end - now) {
                        return Ok(None);
                    }
                    // ペーシングの基準を 1 周分だけ進め、周回をまたいでも平均速度を保つ
                    self.loop_start = loop_end;
                } else {
                    // 送信が遅れて周回の終端を過ぎている場合は、追い上げ送信を避けるため
                    // 基準を現在時刻に戻す
                    self.loop_start = now;
                }
                self.rewind()?;
                continue;
            };
            let deadline = self
                .loop_start
                .checked_add(media_time_to_duration(
                    sample.timestamp,
                    self.info.timescale,
                ))
                .ok_or_else(|| Error::Other("MP4 pacing deadline is out of range".to_string()))?;
            let now = Instant::now();
            if deadline > now && sleep_interruptibly(stop, deadline - now) {
                return Ok(None);
            }

            let timestamp = sample
                .timestamp
                .checked_add(self.loop_offset)
                .ok_or_else(|| Error::Other("MP4 timestamp is out of range".to_string()))?;
            let data = if sample.keyframe && self.codec == PassthroughCodec::Av1 {
                av1_payload_with_sequence_header(&self.video_config, sample.data)
            } else {
                sample.data
            };
            let video_config = if sample.keyframe {
                Some(self.video_config.clone())
            } else {
                None
            };
            return Ok(Some(EncodedFrame {
                data,
                is_keyframe: sample.keyframe,
                timestamp,
                video_config,
            }));
        }
    }

    /// 先頭のキーフレームまで読み飛ばして次の映像サンプルを返す
    ///
    /// 末尾に達した場合は `Ok(None)` を返す。
    fn next_video_sample(&mut self) -> Result<Option<VideoSample>> {
        loop {
            let Some(sample) = self
                .demuxer
                .next_sample()
                .map_err(|e| Error::Other(format!("failed to read MP4 sample: {e}")))?
            else {
                return Ok(None);
            };
            if sample.track.track_id != self.track_id {
                continue;
            }
            if self.need_keyframe && !sample.keyframe {
                continue;
            }
            self.need_keyframe = false;

            let offset = sample.data_offset as usize;
            let end = offset
                .checked_add(sample.data_size)
                .ok_or_else(|| Error::Other("MP4 sample offset is out of range".to_string()))?;
            let data = self
                .data
                .get(offset..end)
                .ok_or_else(|| Error::Other("MP4 sample offset is out of range".to_string()))?
                .to_vec();
            return Ok(Some(VideoSample {
                keyframe: sample.keyframe,
                timestamp: sample.timestamp,
                data,
            }));
        }
    }

    /// 次の周回の先頭に戻る
    ///
    /// ペーシングの基準時刻 (`loop_start`) は呼び出し側が周回の境界で進める。
    fn rewind(&mut self) -> Result<()> {
        self.loop_offset = self
            .loop_offset
            .checked_add(self.loop_duration)
            .ok_or_else(|| Error::Other("MP4 loop offset is out of range".to_string()))?;
        self.demuxer
            .seek(Duration::ZERO)
            .map_err(|e| Error::Other(format!("failed to seek MP4 file: {e}")))?;
        self.need_keyframe = true;
        Ok(())
    }
}

/// MP4 リーダースレッドの生存管理
///
/// Drop 時に停止要求を出してスレッドを join する。実行時エラーを確認したい場合は
/// [`Mp4VideoSource::stop`] を呼ぶ。
pub struct Mp4VideoSource {
    stop_flag: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<Result<()>>>,
}

impl Mp4VideoSource {
    /// 停止要求を出してスレッドを join し、リーダーの実行結果を返す
    pub fn stop(mut self) -> Result<()> {
        self.stop_flag.store(true, Ordering::Release);
        match self.handle.take() {
            Some(handle) => handle
                .join()
                .map_err(|_| Error::Other("MP4 reader thread panicked".to_string()))?,
            None => Ok(()),
        }
    }
}

impl Drop for Mp4VideoSource {
    fn drop(&mut self) {
        // 明示的に stop() されなかった場合はここで停止する (実行結果は破棄する)
        self.stop_flag.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// リーダースレッドの本体
fn run_reader(
    mut reader: Mp4VideoReader,
    stop: Arc<AtomicBool>,
    sender: mpsc::Sender<VideoInput>,
) -> Result<()> {
    loop {
        match reader.next_frame_paced(&stop) {
            Ok(Some(frame)) => {
                if !send_frame(&sender, &stop, frame) {
                    return Ok(());
                }
            }
            // 停止要求
            Ok(None) => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

/// フレームを送信する
///
/// チャネルが満杯の場合は停止要求を見ながら再試行する。`blocking_send` は停止要求を
/// 確認できないままブロックしうるため使わない (Drop 時の join がハングする)。
/// 停止要求または受信側の終了で `false` を返す。
fn send_frame(sender: &mpsc::Sender<VideoInput>, stop: &AtomicBool, frame: EncodedFrame) -> bool {
    let mut frame = frame;
    loop {
        match sender.try_send(VideoInput::Encoded(frame)) {
            Ok(()) => return true,
            Err(mpsc::error::TrySendError::Full(returned)) => {
                // このチャネルには Encoded しか送らないため、返ってくる値も Encoded である
                let VideoInput::Encoded(returned) = returned else {
                    return false;
                };
                frame = returned;
                if sleep_interruptibly(stop, Duration::from_millis(20)) {
                    return false;
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
        }
    }
}

/// サンプルエントリーからパススルー用のコーデックと `PROP_VIDEO_CONFIG` を取り出す
///
/// 対応していないサンプルエントリーの場合は `Ok(None)` を返す。設定データに必要な
/// パラメータセットが無い場合は、受信側でトラックを構成できないためエラーにする。
///
/// - H.264: AVCDecoderConfigurationRecord 本体 (ISO/IEC 14496-15 §5.2.4.1.1)
/// - H.265: HEVCDecoderConfigurationRecord 本体 (ISO/IEC 14496-15 §8.3.3.1.2)
/// - AV1: av1C の config OBUs
///
/// avcC / hvcC は `Encode` がボックスヘッダを含めて出力するため、ヘッダを除いた本体を返す。
/// moq-sub の MP4 保存はこの形式を前提にサンプルエントリーを再構築する。
fn passthrough_codec_and_config(
    entry: &SampleEntry,
) -> Result<Option<(PassthroughCodec, Vec<u8>)>> {
    match entry {
        SampleEntry::Avc1(b) => {
            if b.avcc_box.sps_list.is_empty() || b.avcc_box.pps_list.is_empty() {
                return Err(Error::Other(
                    "avcC box has no SPS/PPS; they are required to publish the H.264 track"
                        .to_string(),
                ));
            }
            let bytes = b
                .avcc_box
                .encode_to_vec()
                .map_err(|e| Error::Other(format!("failed to encode avcC box: {e}")))?;
            Ok(Some((PassthroughCodec::H264, strip_box_header(&bytes)?)))
        }
        SampleEntry::Hvc1(b) => Ok(Some(hevc_codec_config(&b.hvcc_box)?)),
        SampleEntry::Hev1(b) => Ok(Some(hevc_codec_config(&b.hvcc_box)?)),
        SampleEntry::Av01(b) => {
            // av1C の configOBUs は 0 個も許容されるが、PROP_VIDEO_CONFIG が空だと受信側で
            // トラックを構成できないため、設定 OBU の無い MP4 は拒否する
            if b.av1c_box.config_obus.is_empty() {
                return Err(Error::Other(
                    "av1C box has no config OBUs; they are required to publish the AV1 track"
                        .to_string(),
                ));
            }
            Ok(Some((
                PassthroughCodec::Av1,
                b.av1c_box.config_obus.clone(),
            )))
        }
        _ => Ok(None),
    }
}

/// hvcC から H.265 の設定データ (HEVCDecoderConfigurationRecord 本体) を組み立てる
fn hevc_codec_config(hvcc: &HvccBox) -> Result<(PassthroughCodec, Vec<u8>)> {
    if !has_hevc_parameter_sets(hvcc) {
        return Err(Error::Other(
            "hvcC box has no VPS/SPS/PPS; they are required to publish the H.265 track".to_string(),
        ));
    }
    let bytes = hvcc
        .encode_to_vec()
        .map_err(|e| Error::Other(format!("failed to encode hvcC box: {e}")))?;
    Ok((PassthroughCodec::H265, strip_box_header(&bytes)?))
}

/// hvcC に VPS / SPS / PPS が揃っているかどうか
///
/// 受信側は hvcC から 3 つすべてを取り出す前提のため、欠けている MP4 は拒否する。
/// NAL ユニット種別は ITU-T H.265 §7.4.2.2 Table 7-1 の VPS / SPS / PPS。
fn has_hevc_parameter_sets(hvcc: &HvccBox) -> bool {
    [32u8, 33, 34].iter().all(|nal_type| {
        hvcc.nalu_arrays
            .iter()
            .any(|array| array.nal_unit_type.get() == *nal_type && !array.nalus.is_empty())
    })
}

/// ボックスのバイト列からボックスヘッダを除いた本体を返す
fn strip_box_header(box_bytes: &[u8]) -> Result<Vec<u8>> {
    let (_header, header_size) = shiguredo_mp4::BoxHeader::decode(box_bytes)
        .map_err(|e| Error::Other(format!("failed to parse MP4 box header: {e}")))?;
    Ok(box_bytes[header_size..].to_vec())
}

/// AV1 のキーフレーム payload に Sequence Header を付与する
///
/// AV1 ISOBMFF Binding は同期サンプルに Sequence Header OBU を要求するが、
/// サンプル側に含まれない MP4 にも対応するため、含まれないときだけ av1C の config OBUs を
/// 先頭に付与する。moq-sub の AV1 デコーダ (dav1d) は payload 内の Sequence Header を
/// 前提とする。
fn av1_payload_with_sequence_header(config_obus: &[u8], data: Vec<u8>) -> Vec<u8> {
    if crate::encoder::av1::extract_av1_sequence_header(&data).is_some() {
        return data;
    }
    let mut payload = Vec::new();
    payload.extend_from_slice(config_obus);
    payload.extend_from_slice(&data);
    payload
}

/// サンプル数と尺から平均フレームレートを求める (四捨五入)
fn average_fps(sample_count: u64, duration_units: u64, timescale: u64) -> Result<u32> {
    if duration_units == 0 {
        return Err(Error::Other(
            "MP4 video track has zero duration".to_string(),
        ));
    }
    // sample_count * timescale は u64 でオーバーフローしうるため u128 で計算する
    let numerator = u128::from(sample_count) * u128::from(timescale);
    let fps = (numerator + u128::from(duration_units) / 2) / u128::from(duration_units);
    let fps = u32::try_from(fps)
        .map_err(|_| Error::Other(format!("computed frame rate is out of range: {fps}")))?;
    if fps == 0 {
        return Err(Error::Other(
            "computed frame rate is zero; the MP4 video track has too few samples".to_string(),
        ));
    }
    Ok(fps)
}

/// サンプルの最大ビットレート (kbps) を求める
///
/// draft-ietf-moq-msf-01 §5.2.22 (Maximum Bitrate) は audio / video track への記載を
/// MUST とするため、1 秒幅のスライディングウィンドウ内の最大バイト数から bps を算出する。
/// 全尺が 1 秒未満の場合は全尺で平均する。周回配信ではウィンドウがファイル末尾と先頭を
/// またぐため、先頭サンプルを 1 周分ずらした列も評価する。kbps は過小報告を避けるため
/// 切り上げる。サンプルは DTS 昇順であることを前提とする。
fn max_bitrate_kbps(metas: &[SampleMeta], timescale: u64, duration_units: u64) -> Result<u32> {
    if metas.is_empty() || duration_units == 0 {
        return Err(Error::Other(
            "cannot compute the bitrate of an empty MP4 video track".to_string(),
        ));
    }
    if duration_units < timescale {
        let total: u128 = metas.iter().map(|m| u128::from(m.size)).sum();
        let bps = total * 8 * u128::from(timescale) / u128::from(duration_units);
        return Ok(kbps_from_bps(bps));
    }

    let mut window: VecDeque<(u64, u64)> = VecDeque::new();
    let mut window_bytes: u128 = 0;
    let mut max_bytes: u128 = 0;
    let samples = metas.iter().map(|m| (m.timestamp, m.size)).chain(
        metas
            .iter()
            .map(|m| (m.timestamp.saturating_add(duration_units), m.size)),
    );
    for (timestamp, size) in samples {
        window.push_back((timestamp, size));
        window_bytes += u128::from(size);
        // ウィンドウ先頭から 1 秒以上離れたサンプルを落とす
        while let Some(&(front_timestamp, front_size)) = window.front() {
            if timestamp.saturating_sub(front_timestamp) >= timescale {
                window.pop_front();
                window_bytes -= u128::from(front_size);
            } else {
                break;
            }
        }
        max_bytes = max_bytes.max(window_bytes);
    }
    Ok(kbps_from_bps(max_bytes * 8))
}

/// bps を kbps に切り上げる (0 にはしない)
fn kbps_from_bps(bps: u128) -> u32 {
    let kbps = bps.div_ceil(1000).max(1);
    u32::try_from(kbps).unwrap_or(u32::MAX)
}

/// メディア時刻 (timescale 単位) を実時間に変換する
fn media_time_to_duration(timestamp: u64, timescale: u64) -> Duration {
    // timescale は NonZeroU32 由来のため 0 にはならない
    let micros = u128::from(timestamp) * 1_000_000 / u128::from(timescale);
    Duration::from_micros(u64::try_from(micros).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::{Seek, SeekFrom, Write};
    use std::num::NonZeroU32;
    use std::path::PathBuf;

    use shiguredo_mp4::Uint;
    use shiguredo_mp4::bitstream::av1::{Av1SampleEntryConfig, build_av01_box_from_config_obus};
    use shiguredo_mp4::bitstream::h264::{H264SampleEntryConfig, LengthSize, build_avc1_box};
    use shiguredo_mp4::bitstream::h265::{
        H265ConstantFrameRate, H265SampleEntryConfig, build_hvc1_box,
    };
    use shiguredo_mp4::bitstream::opus::{ChannelCount, OpusSampleEntryConfig, build_opus_box};
    use shiguredo_mp4::boxes::{
        Av01Box, Av1cBox, Avc1Box, AvccBox, Hvc1Box, VisualSampleEntryFields, Vp09Box, VpccBox,
    };
    use shiguredo_mp4::mux::{Mp4FileMuxer, Sample};

    use super::*;

    /// shiguredo_mp4 のテストデータ (`tests/testdata/h264-sps-pps-annexb.bin`) の SPS
    /// (start code を除いた NAL)
    const H264_SPS: &[u8] = &[
        0x67, 0x64, 0x00, 0x1E, 0xAC, 0xD9, 0x40, 0xA0, 0x3D, 0xB0, 0x11, 0x00, 0x00, 0x03, 0x00,
        0x01, 0x00, 0x00, 0x03, 0x00, 0x32, 0x0F, 0x16, 0x2D, 0x96,
    ];

    /// shiguredo_mp4 のテストデータ (`tests/testdata/h264-sps-pps-annexb.bin`) の PPS
    const H264_PPS: &[u8] = &[0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0];

    /// shiguredo_mp4 のテストデータ (`tests/testdata/black-av1-config-obus.bin`) の
    /// AV1 の config OBUs (Sequence Header OBU)
    const AV1_CONFIG_OBUS: &[u8] = &[
        0x0A, 0x0B, 0x00, 0x00, 0x00, 0x24, 0xC4, 0xFF, 0xDF, 0x3F, 0xFE, 0x60, 0x10,
    ];

    /// shiguredo_mp4 のテストデータ (`tests/testdata/h265-vps-sps-pps-annexb.bin`) の VPS
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
            std::env::temp_dir().join(format!("moq-pub-mp4-{}-{name}.mp4", std::process::id()));
        std::fs::remove_file(&path).ok();
        path
    }

    /// テスト用の H.264 サンプルエントリーを構築する
    fn build_test_avc1_entry() -> SampleEntry {
        let entry = build_avc1_box(
            &[H264_SPS.to_vec()],
            &[H264_PPS.to_vec()],
            &H264SampleEntryConfig {
                length_size: LengthSize::from_length_size_minus_one(3)
                    .expect("length_size は有効な値であること"),
            },
        )
        .expect("H.264 サンプルエントリーを構築できること");
        SampleEntry::Avc1(entry)
    }

    /// テスト用の MP4 ファイルを書き出す
    ///
    /// `samples` は (キーフレームかどうか, 尺, データ) の列である。
    fn write_test_mp4(
        path: &Path,
        entry: &SampleEntry,
        track_kind: TrackKind,
        timescale: u32,
        composition_time_offsets: &[i64],
        samples: &[(bool, u32, &[u8])],
    ) {
        let mut muxer = Mp4FileMuxer::new().expect("MP4 muxer を作成できること");
        let initial_bytes = muxer.initial_boxes_bytes().to_vec();
        let mut file = File::create(path).expect("MP4 ファイルを作成できること");
        file.write_all(&initial_bytes)
            .expect("初期ボックスを書き込めること");

        let mut position = initial_bytes.len() as u64;
        for (i, (keyframe, duration, data)) in samples.iter().enumerate() {
            file.write_all(data)
                .expect("サンプルデータを書き込めること");
            let sample = Sample {
                track_kind,
                sample_entry: (i == 0).then(|| entry.clone()),
                keyframe: *keyframe,
                timescale: NonZeroU32::new(timescale).expect("timescale は非ゼロであること"),
                duration: *duration,
                composition_time_offset: composition_time_offsets.get(i).copied(),
                data_offset: position,
                data_size: data.len(),
            };
            muxer
                .append_sample(&sample)
                .expect("サンプルを追加できること");
            position += data.len() as u64;
        }

        let finalized = muxer.finalize().expect("MP4 をファイナライズできること");
        for (offset, bytes) in finalized.offset_and_bytes_pairs() {
            file.seek(SeekFrom::Start(offset))
                .expect("ファイナライズ後の位置へ移動できること");
            file.write_all(bytes)
                .expect("ファイナライズ後のボックスを書き込めること");
        }
        file.flush().expect("MP4 ファイルを書き込めること");
    }

    /// H.264 の avcC から PROP_VIDEO_CONFIG (AVCDecoderConfigurationRecord 本体) を取り出せること
    #[test]
    fn video_config_from_avc1_sample_entry() {
        let entry = build_test_avc1_entry();
        let SampleEntry::Avc1(avc1) = &entry else {
            panic!("H.264 サンプルエントリーが構築されること");
        };
        let (codec, config) = passthrough_codec_and_config(&entry)
            .expect("avcC を取り出せること")
            .expect("対応コーデックであること");
        assert_eq!(codec, PassthroughCodec::H264);

        // ボックスヘッダを含まない AVCDecoderConfigurationRecord であること
        let avcc = &avc1.avcc_box;
        let mut expected = vec![
            1,
            avcc.avc_profile_indication,
            avcc.profile_compatibility,
            avcc.avc_level_indication,
            0xFC | avcc.length_size_minus_one.get(),
            0xE0 | 1,
        ];
        expected.extend_from_slice(&(H264_SPS.len() as u16).to_be_bytes());
        expected.extend_from_slice(H264_SPS);
        expected.push(1);
        expected.extend_from_slice(&(H264_PPS.len() as u16).to_be_bytes());
        expected.extend_from_slice(H264_PPS);
        // プロファイル 66 / 77 / 88 以外ではクロマフォーマットとビット深度と SPS 拡張数が続く
        // (ISO/IEC 14496-15 §5.2.4.1.1)
        if let (Some(chroma_format), Some(luma), Some(chroma)) = (
            avcc.chroma_format,
            avcc.bit_depth_luma_minus8,
            avcc.bit_depth_chroma_minus8,
        ) {
            expected.push(0xFC | chroma_format.get());
            expected.push(0xF8 | luma.get());
            expected.push(0xF8 | chroma.get());
            expected.push(avcc.sps_ext_list.len() as u8);
        }
        assert_eq!(
            config, expected,
            "AVCDecoderConfigurationRecord 本体であること"
        );
    }

    /// H.265 の hvcC から PROP_VIDEO_CONFIG (HEVCDecoderConfigurationRecord 本体) を取り出せること
    #[test]
    fn video_config_from_hvc1_sample_entry() {
        let entry = SampleEntry::Hvc1(
            build_hvc1_box(
                &[H265_VPS.to_vec()],
                &[H265_SPS.to_vec()],
                &[H265_PPS.to_vec()],
                &H265SampleEntryConfig {
                    length_size: LengthSize::from_length_size_minus_one(3)
                        .expect("length_size は有効な値であること"),
                    avg_frame_rate: H265SampleEntryConfig::AVG_FRAME_RATE_UNSPECIFIED,
                    constant_frame_rate: H265ConstantFrameRate::Unknown,
                },
            )
            .expect("H.265 サンプルエントリーを構築できること"),
        );
        let (codec, config) = passthrough_codec_and_config(&entry)
            .expect("hvcC を取り出せること")
            .expect("対応コーデックであること");
        assert_eq!(codec, PassthroughCodec::H265);

        // configurationVersion から始まり、ボックスヘッダを含まないこと
        assert_eq!(config[0], 1, "configurationVersion は 1 であること");
        for nal in [H265_VPS, H265_SPS, H265_PPS] {
            assert!(
                config.windows(nal.len()).any(|w| w == nal),
                "HEVCDecoderConfigurationRecord に NAL が含まれること"
            );
        }
    }

    /// AV1 の av1C から config OBUs を取り出せること
    #[test]
    fn video_config_from_av01_sample_entry() {
        let entry = build_test_av01_entry();
        let (codec, config) = passthrough_codec_and_config(&entry)
            .expect("config OBUs を取り出せること")
            .expect("対応コーデックであること");
        assert_eq!(codec, PassthroughCodec::Av1);
        assert_eq!(config, AV1_CONFIG_OBUS, "config OBUs がそのまま返ること");
    }

    /// SPS / PPS を持たない avcC を拒否すること
    #[test]
    fn passthrough_codec_rejects_avcc_without_parameter_sets() {
        let entry = SampleEntry::Avc1(Avc1Box {
            visual: test_visual_fields(640, 480),
            avcc_box: AvccBox {
                avc_profile_indication: 66,
                profile_compatibility: 0,
                avc_level_indication: 30,
                length_size_minus_one: Uint::new(3),
                sps_list: Vec::new(),
                pps_list: Vec::new(),
                chroma_format: None,
                bit_depth_luma_minus8: None,
                bit_depth_chroma_minus8: None,
                sps_ext_list: Vec::new(),
            },
            unknown_boxes: Vec::new(),
        });
        assert!(
            passthrough_codec_and_config(&entry).is_err(),
            "SPS / PPS が無い avcC はエラーになること"
        );
    }

    /// VPS / SPS / PPS を持たない hvcC を拒否すること
    #[test]
    fn passthrough_codec_rejects_hvcc_without_parameter_sets() {
        let entry = SampleEntry::Hvc1(Hvc1Box {
            visual: test_visual_fields(640, 480),
            hvcc_box: HvccBox {
                general_profile_space: Uint::new(0),
                general_tier_flag: Uint::new(0),
                general_profile_idc: Uint::new(1),
                general_profile_compatibility_flags: 0,
                general_constraint_indicator_flags: Uint::new(0),
                general_level_idc: 120,
                min_spatial_segmentation_idc: Uint::new(0),
                parallelism_type: Uint::new(0),
                chroma_format_idc: Uint::new(1),
                bit_depth_luma_minus8: Uint::new(0),
                bit_depth_chroma_minus8: Uint::new(0),
                avg_frame_rate: 0,
                constant_frame_rate: Uint::new(0),
                num_temporal_layers: Uint::new(1),
                temporal_id_nested: Uint::new(1),
                length_size_minus_one: Uint::new(3),
                nalu_arrays: Vec::new(),
            },
            unknown_boxes: Vec::new(),
        });
        assert!(
            passthrough_codec_and_config(&entry).is_err(),
            "VPS / SPS / PPS が無い hvcC はエラーになること"
        );
    }

    /// config OBUs を持たない av1C を拒否すること
    #[test]
    fn passthrough_codec_rejects_av1c_without_config_obus() {
        let entry = SampleEntry::Av01(Av01Box {
            visual: test_visual_fields(640, 480),
            av1c_box: Av1cBox {
                seq_profile: Uint::new(0),
                seq_level_idx_0: Uint::new(0),
                seq_tier_0: Uint::new(0),
                high_bitdepth: Uint::new(0),
                twelve_bit: Uint::new(0),
                monochrome: Uint::new(0),
                chroma_subsampling_x: Uint::new(1),
                chroma_subsampling_y: Uint::new(1),
                chroma_sample_position: Uint::new(0),
                initial_presentation_delay_minus_one: None,
                config_obus: Vec::new(),
            },
            unknown_boxes: Vec::new(),
        });
        assert!(
            passthrough_codec_and_config(&entry).is_err(),
            "config OBUs が無い av1C はエラーになること"
        );
    }

    /// AV1 のキーフレーム: Sequence Header が無いサンプルには config OBUs を先頭に付与すること
    #[test]
    fn av1_payload_prepends_sequence_header() {
        // OBU type 6 (frame) の OBU のみを持つ payload
        let frame_only = vec![0x32, 0x00];
        let payload = av1_payload_with_sequence_header(AV1_CONFIG_OBUS, frame_only.clone());
        let mut expected = AV1_CONFIG_OBUS.to_vec();
        expected.extend_from_slice(&frame_only);
        assert_eq!(payload, expected, "Sequence Header が先頭に付与されること");

        // Sequence Header を持つ payload は変更しないこと
        let payload = av1_payload_with_sequence_header(AV1_CONFIG_OBUS, AV1_CONFIG_OBUS.to_vec());
        assert_eq!(payload, AV1_CONFIG_OBUS, "付与されないこと");
    }

    /// 平均フレームレートは四捨五入されること
    #[test]
    fn average_fps_rounds_to_nearest() {
        // 1 秒 (timescale 1000、尺 1000) に 30 サンプル
        assert_eq!(average_fps(30, 1_000, 1_000).expect("算出できること"), 30);
        // 29.5 fps は 30 に丸められる
        assert_eq!(average_fps(59, 2_000, 1_000).expect("算出できること"), 30);
        // 尺が 0 の場合はエラー
        assert!(
            average_fps(1, 0, 1_000).is_err(),
            "尺が 0 の場合はエラーになること"
        );
    }

    /// 最大ビットレートは 1 秒幅のスライディングウィンドウの最大値になること
    #[test]
    fn max_bitrate_uses_sliding_window() {
        // timescale = 1000 で、最初の 1 秒に 2000 バイト、次の 1 秒に 200 バイト
        let metas = vec![
            SampleMeta {
                timestamp: 0,
                duration: 500,
                size: 1_000,
            },
            SampleMeta {
                timestamp: 500,
                duration: 500,
                size: 1_000,
            },
            SampleMeta {
                timestamp: 1_000,
                duration: 500,
                size: 100,
            },
            SampleMeta {
                timestamp: 1_500,
                duration: 500,
                size: 100,
            },
        ];
        // 2000 バイト * 8 = 16000 bps → 16 kbps
        assert_eq!(
            max_bitrate_kbps(&metas, 1_000, 2_000).expect("算出できること"),
            16
        );
    }

    /// 全尺が 1 秒未満の場合は全尺で平均すること
    #[test]
    fn max_bitrate_uses_whole_duration_when_shorter_than_one_second() {
        let metas = vec![
            SampleMeta {
                timestamp: 0,
                duration: 250,
                size: 1_000,
            },
            SampleMeta {
                timestamp: 250,
                duration: 250,
                size: 1_000,
            },
        ];
        // 2000 バイト * 8 bps / 0.5 秒 = 32000 bps → 32 kbps
        assert_eq!(
            max_bitrate_kbps(&metas, 1_000, 500).expect("算出できること"),
            32
        );
    }

    /// 周回でファイル末尾と先頭をまたぐウィンドウも最大ビットレートに含めること
    #[test]
    fn max_bitrate_considers_loop_boundary() {
        // timescale 1000、尺 2000 のクリップ。ts=0 と ts=1500 に 1000 バイトずつ
        let metas = vec![
            SampleMeta {
                timestamp: 0,
                duration: 1_500,
                size: 1_000,
            },
            SampleMeta {
                timestamp: 1_500,
                duration: 500,
                size: 1_000,
            },
        ];
        // 周回をまたぐ [1000, 2000] の窓に 2 サンプルが入るため 2000 バイト → 16000 bps
        assert_eq!(
            max_bitrate_kbps(&metas, 1_000, 2_000).expect("算出できること"),
            16
        );
    }

    /// メディア時刻を実時間に変換できること
    #[test]
    fn media_time_converts_to_duration() {
        assert_eq!(media_time_to_duration(0, 90_000), Duration::ZERO);
        assert_eq!(
            media_time_to_duration(45_000, 90_000),
            Duration::from_millis(500)
        );
    }

    /// MP4 を読み込んで映像トラック情報を集計し、ループ時にタイムスタンプを加算すること
    #[test]
    fn reader_reads_video_track_and_loops() {
        let path = temp_path("loop");
        let entry = build_test_avc1_entry();
        let (expected_width, expected_height) =
            entry.video_resolution().expect("解像度を取得できること");
        let samples: &[(bool, u32, &[u8])] = &[
            (true, 3_000, &[0x01, 0x02, 0x03, 0x04, 0x05]),
            (false, 3_000, &[0x06, 0x07]),
            (false, 3_000, &[0x08]),
        ];
        write_test_mp4(&path, &entry, TrackKind::Video, 90_000, &[], samples);

        let mut reader = Mp4VideoReader::open(&path).expect("MP4 を開けること");
        let info = reader.info();
        assert!(info.codec.starts_with("avc1"), "codec は avc1 であること");
        assert_eq!(info.width, u32::from(expected_width));
        assert_eq!(info.height, u32::from(expected_height));
        assert_eq!(info.timescale, 90_000);
        // 3 サンプル / (9000 / 90000) 秒 = 30 fps
        assert_eq!(info.fps, 30);
        // 8 バイト * 8 bps / 0.1 秒 = 640 bps → 1 kbps
        assert_eq!(info.bitrate_kbps, 1);

        let stop = AtomicBool::new(false);
        let first = reader
            .next_frame_paced(&stop)
            .expect("フレームを読めること")
            .expect("フレームがあること");
        assert!(first.is_keyframe, "先頭はキーフレームであること");
        assert_eq!(first.timestamp, 0);
        assert!(
            first.video_config.is_some(),
            "キーフレームに設定が付与されること"
        );
        assert_eq!(first.data, vec![0x01, 0x02, 0x03, 0x04, 0x05]);

        let second = reader
            .next_frame_paced(&stop)
            .expect("フレームを読めること")
            .expect("フレームがあること");
        assert!(!second.is_keyframe, "2 番目は非キーフレームであること");
        assert_eq!(second.timestamp, 3_000);
        assert!(second.video_config.is_none());

        let third = reader
            .next_frame_paced(&stop)
            .expect("フレームを読めること")
            .expect("フレームがあること");
        assert_eq!(third.timestamp, 6_000);

        // 末尾の次は先頭に戻り、1 周分の尺 (9000) が加算される
        let looped = reader
            .next_frame_paced(&stop)
            .expect("フレームを読めること")
            .expect("フレームがあること");
        assert!(looped.is_keyframe, "周回後の先頭はキーフレームであること");
        assert_eq!(looped.timestamp, 9_000);

        std::fs::remove_file(&path).ok();
    }

    /// B フレームを含む MP4 を拒否すること
    #[test]
    fn reader_rejects_b_frames() {
        let path = temp_path("b-frames");
        let entry = build_test_avc1_entry();
        let samples: &[(bool, u32, &[u8])] = &[(true, 3_000, &[0x01]), (false, 3_000, &[0x02])];
        write_test_mp4(
            &path,
            &entry,
            TrackKind::Video,
            90_000,
            &[0, 3_000],
            samples,
        );

        let Err(error) = Mp4VideoReader::open(&path) else {
            panic!("B フレームがあるとエラーになること");
        };
        assert!(
            error.to_string().contains("B frames"),
            "B フレームが原因であることが分かること: {error}"
        );

        std::fs::remove_file(&path).ok();
    }

    /// テスト用の映像サンプルエントリー共通フィールドを構築する
    fn test_visual_fields(width: u16, height: u16) -> VisualSampleEntryFields {
        VisualSampleEntryFields {
            data_reference_index: VisualSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
            width,
            height,
            horizresolution: VisualSampleEntryFields::DEFAULT_HORIZRESOLUTION,
            vertresolution: VisualSampleEntryFields::DEFAULT_VERTRESOLUTION,
            frame_count: VisualSampleEntryFields::DEFAULT_FRAME_COUNT,
            compressorname: VisualSampleEntryFields::NULL_COMPRESSORNAME,
            depth: VisualSampleEntryFields::DEFAULT_DEPTH,
        }
    }

    /// テスト用の VP9 サンプルエントリーを構築する (未対応コーデックの検証用)
    fn build_test_vp09_entry() -> SampleEntry {
        SampleEntry::Vp09(Vp09Box {
            visual: test_visual_fields(640, 480),
            vpcc_box: VpccBox {
                profile: 0,
                level: 31,
                bit_depth: Uint::new(8),
                chroma_subsampling: Uint::new(1),
                video_full_range_flag: Uint::new(0),
                colour_primaries: 1,
                transfer_characteristics: 1,
                matrix_coefficients: 1,
                codec_initialization_data: Vec::new(),
            },
            unknown_boxes: Vec::new(),
        })
    }

    /// テスト用の AV1 サンプルエントリーを構築する
    fn build_test_av01_entry() -> SampleEntry {
        SampleEntry::Av01(
            build_av01_box_from_config_obus(
                AV1_CONFIG_OBUS,
                &Av1SampleEntryConfig {
                    initial_presentation_delay_minus_one: None,
                },
            )
            .expect("AV1 サンプルエントリーを構築できること"),
        )
    }

    /// 周回は 1 周分のメディア尺 (最終サンプルの尺を含む) 以上かかること
    ///
    /// 最終サンプルの尺を待たずに次の周回へ入ると、周回ごとにその分だけ実時間より速く進む。
    #[test]
    fn reader_loop_takes_the_whole_media_duration() {
        let path = temp_path("loop-timing");
        let entry = build_test_avc1_entry();
        // timescale 1000、500 ms のサンプル 2 個で 1 秒のクリップ
        let samples: &[(bool, u32, &[u8])] = &[(true, 500, &[0x01]), (false, 500, &[0x02])];
        write_test_mp4(&path, &entry, TrackKind::Video, 1_000, &[], samples);
        let mut reader = Mp4VideoReader::open(&path).expect("MP4 を開けること");
        let stop = AtomicBool::new(false);

        let started = Instant::now();
        let first = reader
            .next_frame_paced(&stop)
            .expect("フレームを読めること")
            .expect("フレームがあること");
        assert_eq!(first.timestamp, 0);
        let second = reader
            .next_frame_paced(&stop)
            .expect("フレームを読めること")
            .expect("フレームがあること");
        assert_eq!(second.timestamp, 500);

        // 2 周目の先頭は 1 周分の尺 (1000 ms) が経過するまで返らない。
        // 負荷による遅延を見込んで 800 ms を下限にする (最終サンプルの尺を待たない
        // 実装では 500 ms 程度で返ってしまう)。
        let looped = reader
            .next_frame_paced(&stop)
            .expect("フレームを読めること")
            .expect("フレームがあること");
        assert_eq!(looped.timestamp, 1_000);
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(800),
            "周回は 1 周分のメディア尺 (1000 ms) 以上かかること: {elapsed:?}"
        );

        std::fs::remove_file(&path).ok();
    }

    /// 映像トラックを持たない MP4 を拒否すること
    #[test]
    fn reader_rejects_mp4_without_video_track() {
        let path = temp_path("audio-only");
        let entry = SampleEntry::Opus(build_opus_box(&OpusSampleEntryConfig {
            channel_count: ChannelCount::Mono,
            pre_skip: 0,
            input_sample_rate: 48_000,
            output_gain: 0,
        }));
        let samples: &[(bool, u32, &[u8])] = &[(true, 960, &[0x01, 0x02])];
        write_test_mp4(&path, &entry, TrackKind::Audio, 48_000, &[], samples);

        let Err(error) = Mp4VideoReader::open(&path) else {
            panic!("映像トラックが無い場合はエラーになること");
        };
        assert!(
            error.to_string().contains("has no video track"),
            "映像トラックが無いことが分かること: {error}"
        );

        std::fs::remove_file(&path).ok();
    }

    /// 対応していない映像コーデックの MP4 を拒否すること
    #[test]
    fn reader_rejects_unsupported_video_codec() {
        let path = temp_path("vp9");
        let entry = build_test_vp09_entry();
        let samples: &[(bool, u32, &[u8])] = &[(true, 3_000, &[0x01])];
        write_test_mp4(&path, &entry, TrackKind::Video, 90_000, &[], samples);

        let Err(error) = Mp4VideoReader::open(&path) else {
            panic!("未対応コーデックの場合はエラーになること");
        };
        let message = error.to_string();
        assert!(
            message.contains("unsupported video codec") && message.contains("vp09"),
            "未対応コーデックであることが分かること: {error}"
        );

        std::fs::remove_file(&path).ok();
    }

    /// 先頭のキーフレームより前のサンプルを読み飛ばすこと
    #[test]
    fn reader_skips_samples_before_the_first_keyframe() {
        let path = temp_path("leading-non-keyframe");
        let entry = build_test_avc1_entry();
        let samples: &[(bool, u32, &[u8])] = &[
            (false, 3_000, &[0x01]),
            (true, 3_000, &[0x02]),
            (false, 3_000, &[0x03]),
        ];
        write_test_mp4(&path, &entry, TrackKind::Video, 90_000, &[], samples);
        let mut reader = Mp4VideoReader::open(&path).expect("MP4 を開けること");
        let stop = AtomicBool::new(false);

        let first = reader
            .next_frame_paced(&stop)
            .expect("フレームを読めること")
            .expect("フレームがあること");
        assert!(first.is_keyframe, "最初のキーフレームから始まること");
        assert_eq!(first.timestamp, 3_000);

        let second = reader
            .next_frame_paced(&stop)
            .expect("フレームを読めること")
            .expect("フレームがあること");
        assert_eq!(second.timestamp, 6_000);

        // 周回時は 1 周分の尺 (9000) と先頭キーフレームの時刻 (3000) の和になる
        let looped = reader
            .next_frame_paced(&stop)
            .expect("フレームを読めること")
            .expect("フレームがあること");
        assert_eq!(looped.timestamp, 12_000);

        std::fs::remove_file(&path).ok();
    }

    /// AV1 はキーフレームに Sequence Header を付与し、設定を LOC プロパティ用に返すこと
    #[test]
    fn reader_publishes_av1_with_sequence_header() {
        let path = temp_path("av1");
        let entry = build_test_av01_entry();
        // キーフレームのサンプルには Sequence Header を含めない (付与を確認する)
        let samples: &[(bool, u32, &[u8])] = &[
            (true, 3_000, &[0x32, 0x00]),
            (false, 3_000, &[0x32, 0x01, 0xAA]),
        ];
        write_test_mp4(&path, &entry, TrackKind::Video, 90_000, &[], samples);
        let mut reader = Mp4VideoReader::open(&path).expect("MP4 を開けること");
        assert!(
            reader.info().codec.starts_with("av01"),
            "codec は av01 であること"
        );
        let stop = AtomicBool::new(false);

        let keyframe = reader
            .next_frame_paced(&stop)
            .expect("フレームを読めること")
            .expect("フレームがあること");
        let mut expected = AV1_CONFIG_OBUS.to_vec();
        expected.extend_from_slice(&[0x32, 0x00]);
        assert_eq!(
            keyframe.data, expected,
            "config OBUs が先頭に付与されること"
        );
        assert_eq!(
            keyframe.video_config.as_deref(),
            Some(AV1_CONFIG_OBUS),
            "キーフレームに設定が付与されること"
        );

        let non_keyframe = reader
            .next_frame_paced(&stop)
            .expect("フレームを読めること")
            .expect("フレームがあること");
        assert_eq!(
            non_keyframe.data,
            vec![0x32, 0x01, 0xAA],
            "非キーフレームは変更されないこと"
        );
        assert!(non_keyframe.video_config.is_none());

        std::fs::remove_file(&path).ok();
    }

    /// MP4 として解釈できないファイルを拒否すること
    #[test]
    fn reader_rejects_invalid_mp4() {
        let path = temp_path("invalid");
        std::fs::write(&path, b"this is not an MP4 file")
            .expect("テスト用ファイルを書き込めること");

        let Err(error) = Mp4VideoReader::open(&path) else {
            panic!("MP4 として解釈できない場合はエラーになること");
        };
        assert!(
            error.to_string().contains("failed to read MP4 file"),
            "MP4 の読み込みに失敗したことが分かること: {error}"
        );

        std::fs::remove_file(&path).ok();
    }

    /// リーダースレッドを開始し、フレームを受信した後に停止できること
    #[test]
    fn reader_source_stops_after_receiving_frames() {
        let path = temp_path("start-stop");
        let entry = build_test_avc1_entry();
        let samples: &[(bool, u32, &[u8])] = &[(true, 3_000, &[0x01]), (false, 3_000, &[0x02])];
        write_test_mp4(&path, &entry, TrackKind::Video, 90_000, &[], samples);

        let reader = Mp4VideoReader::open(&path).expect("MP4 を開けること");
        let (sender, mut receiver) = mpsc::channel::<VideoInput>(4);
        let source = reader.start(sender).expect("スレッドを開始できること");
        let frame = receiver.blocking_recv().expect("フレームを受信できること");
        let VideoInput::Encoded(frame) = frame else {
            panic!("パススルーではエンコード済みフレームが届くこと");
        };
        assert_eq!(frame.timestamp, 0, "先頭フレームのタイムスタンプであること");
        source.stop().expect("停止できること");

        std::fs::remove_file(&path).ok();
    }

    /// 受信側が閉じている場合でもリーダースレッドを停止できること
    #[test]
    fn reader_source_stops_when_receiver_is_closed() {
        let path = temp_path("start-stop-closed");
        let entry = build_test_avc1_entry();
        let samples: &[(bool, u32, &[u8])] = &[(true, 3_000, &[0x01])];
        write_test_mp4(&path, &entry, TrackKind::Video, 90_000, &[], samples);

        let reader = Mp4VideoReader::open(&path).expect("MP4 を開けること");
        let (sender, receiver) = mpsc::channel::<VideoInput>(4);
        // 受信側を閉じてから開始する (送信できないことを検出してスレッドが終了する)
        drop(receiver);
        let source = reader.start(sender).expect("スレッドを開始できること");
        source.stop().expect("停止できること");

        std::fs::remove_file(&path).ok();
    }
}
