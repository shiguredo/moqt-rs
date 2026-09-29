//! MP4 ファイルの再エンコード配信のリーダー
//!
//! MP4 の映像 / 音声トラックをデコードし、パイプラインへ PCM と生フレームを供給する。
//! 実時間ペーシングは行わず、パイプラインのチャネルが満杯の場合は送信側で待機する
//! (バックプレッシャー)。実時間より遅れて配信されることを許容する。
//!
//! 映像はデコード順 (DTS) でデコーダへ入力し、表示順 (PTS) でパイプラインへ供給する。
//! B フレームを含む MP4 を許容するため、デコード結果は PTS の小さい順に取り出す。
//! ループの周期は映像と音声のトラック尺の最大値とし、各トラックは周期の先頭から再開する。

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use shiguredo_audio_device::{AudioFormat, AudioFrameOwned};
use shiguredo_mp4::TrackKind;
use shiguredo_mp4::boxes::SampleEntry;
use shiguredo_mp4::demux::{Input, Mp4FileDemuxer};
use shiguredo_video_device::{PixelFormat as VideoPixelFormat, VideoFrameOwned};
use tokio::sync::mpsc;

use crate::decoder::opus::OpusDecoder;
use crate::decoder::{self, DecodedVideoFrame, VideoDecoder, interleave_uv};
use crate::error::{Error, Result};
use crate::pipeline::{AudioInput, VideoInput};

use super::{
    Mp4VideoCodec, ReaderGuard, av1_payload_with_sequence_header, average_fps,
    send_with_backpressure, video_codec_and_config,
};

/// 音声のサンプリングレート (Hz)
const AUDIO_SAMPLE_RATE: u32 = 48_000;
/// 音声のチャンネル数 (mono)
const AUDIO_CHANNELS: u8 = 1;
/// 1 フレームあたりのサンプル数 (20 ms)
const AUDIO_SAMPLES_PER_FRAME: usize = 960;

/// 再エンコード配信に使う映像トラック情報
#[derive(Debug)]
pub struct VideoInfo {
    /// 映像幅 (px)
    pub width: u32,
    /// 映像高さ (px)
    pub height: u32,
    /// 平均フレームレート (四捨五入)
    pub fps: u32,
    /// タイムスケール (1 秒あたりの timestamp 単位数)
    pub timescale: u64,
}

/// 再エンコード配信に使う音声トラック情報
#[derive(Debug)]
pub struct AudioInfo {
    /// タイムスケール (1 秒あたりの timestamp 単位数)
    pub timescale: u64,
}

/// 映像トラックの内部情報
struct VideoTrack {
    track_id: u32,
    codec: Mp4VideoCodec,
    /// デコーダ設定 (avcC / hvcC のレコード本体、AV1 は config OBUs)
    config: Vec<u8>,
    /// 映像幅 (px)
    width: u32,
    /// 映像高さ (px)
    height: u32,
    /// 平均フレームレート (四捨五入)
    fps: u32,
    timescale: u64,
    /// トラックの尺 (最終サンプルの timestamp + duration)
    duration: u64,
}

/// 音声トラックの内部情報 (Opus のみ)
struct AudioTrack {
    track_id: u32,
    timescale: u64,
    /// トラックの尺 (最終サンプルの timestamp + duration)
    duration: u64,
}

/// demux した 1 サンプル分の供給データ
struct ReencodeSample {
    /// 映像トラックのサンプルかどうか
    is_video: bool,
    /// サンプルのタイムスタンプ (DTS、トラックの timescale 単位)
    timestamp: u64,
    /// コンポジション時間オフセット (トラックの timescale 単位)
    composition_time_offset: Option<i64>,
    /// キーフレームかどうか
    keyframe: bool,
    /// 圧縮済みビットストリーム
    data: Vec<u8>,
}

/// デコード入力の PTS を表示順で出力フレームへ対応付ける待ち行列
///
/// B フレームを含む MP4 はデコード順 (DTS) と表示順 (PTS) が異なるため、デコード入力の
/// PTS を登録しておき、デコーダが表示順に出力するフレームへ最小値から順に割り当てる。
struct PtsQueue {
    heap: BinaryHeap<Reverse<u64>>,
}

impl PtsQueue {
    /// PTS 待ち行列を作る
    fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
        }
    }

    /// 入力サンプルの PTS を登録する
    ///
    /// 0 フレームを返した入力の PTS も登録し、次に出力されたフレームへ引き継ぐ。
    fn push_input(&mut self, pts: u64) {
        self.heap.push(Reverse(pts));
    }

    /// 出力フレーム 1 個分の PTS を表示順 (最小値) で取り出す
    fn take_output(&mut self) -> Option<u64> {
        self.heap.pop().map(|Reverse(pts)| pts)
    }

    /// 登録済みの PTS をすべて破棄する (周回の先頭で呼ぶ)
    fn clear(&mut self) {
        self.heap.clear();
    }
}

impl ReencodeSample {
    /// 表示時刻 (PTS、トラックの timescale 単位) を返す
    ///
    /// 編集リストは適用しないため、PTS が負になる場合は 0 に丸める。
    fn pts(&self) -> u64 {
        let pts = (self.timestamp as i64).saturating_add(self.composition_time_offset.unwrap_or(0));
        pts.max(0) as u64
    }
}

/// MP4 ファイルの再エンコード用リーダー
pub struct Mp4ReencodeReader {
    /// ファイル全体 (サンプルはこのバッファ内のオフセットを指す)
    data: Vec<u8>,
    /// ファイル全体を入力済みの demuxer
    demuxer: Mp4FileDemuxer,
    /// 映像トラック (映像を配信しない場合は None)
    video: Option<VideoTrack>,
    /// 音声トラック (音声を配信しない場合、Opus 以外の場合は None)
    audio: Option<AudioTrack>,
    /// 映像デコーダ (映像を配信しない場合は None)
    video_decoder: Option<VideoDecoder>,
    /// 音声デコーダ (音声を配信しない場合は None)
    audio_decoder: Option<OpusDecoder>,
    /// 周回の周期 (映像トラック単位)
    video_period_units: u64,
    /// 周回の周期 (音声トラック単位)
    audio_period_units: u64,
    /// 周回を重ねた分の映像タイムスタンプ加算値
    video_loop_offset: u64,
    /// 周回を重ねた分の音声タイムスタンプ加算値
    audio_loop_offset: u64,
    /// 周回の先頭で最初のキーフレームまで読み飛ばすかどうか
    need_keyframe: bool,
}

impl Mp4ReencodeReader {
    /// MP4 ファイルを開いて映像 / 音声トラックを検証する
    ///
    /// ファイル全体をメモリに読み込んで demux し、デコーダの生成と配信に必要な
    /// メタデータの算出を行う。`video_enabled` / `audio_enabled` が true のトラックが
    /// 配信対象になる。映像を配信する場合は 8-bit 4:2:0 の映像トラックが必要である。
    /// 音声を配信する場合は Opus トラックのみ対応し、それ以外のコーデックのトラックは
    /// 警告して無視する。
    pub fn open<P: AsRef<Path>>(path: P, video_enabled: bool, audio_enabled: bool) -> Result<Self> {
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

        let (video_track_id, audio_track_id) = {
            let tracks = demuxer.tracks().map_err(|e| {
                Error::Other(format!("failed to read MP4 file '{}': {e}", path.display()))
            })?;
            let video_track_id = if video_enabled {
                Some(
                    tracks
                        .iter()
                        .find(|t| t.kind == TrackKind::Video)
                        .ok_or_else(|| {
                            Error::Other(format!(
                                "MP4 file '{}' has no video track",
                                path.display()
                            ))
                        })?
                        .track_id,
                )
            } else {
                None
            };
            let audio_track_id = if audio_enabled {
                tracks
                    .iter()
                    .find(|t| t.kind == TrackKind::Audio)
                    .map(|t| t.track_id)
            } else {
                None
            };
            (video_track_id, audio_track_id)
        };
        if audio_enabled && audio_track_id.is_none() {
            tracing::warn!(
                "MP4 file '{}' has no audio track; publishing video only",
                path.display()
            );
        }

        // 全サンプルを走査して、サンプルエントリーとトラックの尺を集める
        let mut video_entry: Option<SampleEntry> = None;
        let mut audio_entry: Option<SampleEntry> = None;
        let mut video_sample_count: u64 = 0;
        let mut video_duration: u64 = 0;
        let mut video_has_keyframe = false;
        let mut audio_duration: u64 = 0;
        while let Some(sample) = demuxer
            .next_sample()
            .map_err(|e| Error::Other(format!("failed to read MP4 sample: {e}")))?
        {
            let track_id = sample.track.track_id;
            if Some(track_id) == video_track_id {
                if video_entry.is_none() {
                    video_entry = sample.sample_entry.cloned();
                }
                if sample.keyframe {
                    video_has_keyframe = true;
                }
                video_sample_count += 1;
                video_duration = sample
                    .timestamp
                    .checked_add(u64::from(sample.duration))
                    .ok_or_else(|| Error::Other("MP4 duration is out of range".to_string()))?;
            } else if Some(track_id) == audio_track_id {
                if audio_entry.is_none() {
                    audio_entry = sample.sample_entry.cloned();
                }
                audio_duration = sample
                    .timestamp
                    .checked_add(u64::from(sample.duration))
                    .ok_or_else(|| Error::Other("MP4 duration is out of range".to_string()))?;
            } else {
                continue;
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
        }

        // 映像トラック
        let (video, video_decoder) = if let Some(track_id) = video_track_id {
            let entry = video_entry.ok_or_else(|| {
                Error::Other(format!(
                    "MP4 file '{}' has no video samples",
                    path.display()
                ))
            })?;
            if !video_has_keyframe {
                return Err(Error::Other(format!(
                    "MP4 file '{}' has no keyframe in the video track",
                    path.display()
                )));
            }
            let Some((codec, config)) = video_codec_and_config(&entry)
                .map_err(|e| Error::Other(format!("MP4 file '{}': {e}", path.display())))?
            else {
                let codec = shiguredo_mp4::codec_string::from_sample_entry(&entry)
                    .unwrap_or_else(|_| "unknown".to_string());
                return Err(Error::Other(format!(
                    "unsupported video codec in MP4 file '{}': {codec} (supported: AV1 / H.264 / H.265)",
                    path.display()
                )));
            };
            validate_8bit_420(&entry)?;
            let (width, height) = entry.video_resolution().ok_or_else(|| {
                Error::Other(format!(
                    "MP4 file '{}' has no video resolution",
                    path.display()
                ))
            })?;
            let timescale = {
                let tracks = demuxer
                    .tracks()
                    .map_err(|e| Error::Other(format!("failed to read MP4 tracks: {e}")))?;
                let track = tracks
                    .iter()
                    .find(|t| t.track_id == track_id)
                    .ok_or_else(|| Error::Other("MP4 video track is missing".to_string()))?;
                u64::from(track.timescale.get())
            };
            let fps = average_fps(video_sample_count, video_duration, timescale)?;
            let decoder = build_video_decoder(codec)?;
            tracing::info!(
                "MP4 video track: codec={:?} size={}x{} fps={} timescale={} samples={}",
                codec,
                width,
                height,
                fps,
                timescale,
                video_sample_count,
            );
            (
                Some(VideoTrack {
                    track_id,
                    codec,
                    config,
                    width: u32::from(width),
                    height: u32::from(height),
                    fps,
                    timescale,
                    duration: video_duration,
                }),
                Some(decoder),
            )
        } else {
            (None, None)
        };

        // 音声トラック (Opus のみ)
        let mut audio = None;
        let mut audio_decoder = None;
        if let Some(track_id) = audio_track_id {
            let entry = audio_entry.ok_or_else(|| {
                Error::Other(format!(
                    "MP4 file '{}' has no audio samples",
                    path.display()
                ))
            })?;
            if matches!(entry, SampleEntry::Opus(_)) {
                let timescale = {
                    let tracks = demuxer
                        .tracks()
                        .map_err(|e| Error::Other(format!("failed to read MP4 tracks: {e}")))?;
                    let track = tracks
                        .iter()
                        .find(|t| t.track_id == track_id)
                        .ok_or_else(|| Error::Other("MP4 audio track is missing".to_string()))?;
                    u64::from(track.timescale.get())
                };
                let decoder = OpusDecoder::new(AUDIO_SAMPLE_RATE, AUDIO_CHANNELS)?;
                tracing::info!(
                    "MP4 audio track: codec=opus timescale={} duration={}",
                    timescale,
                    audio_duration,
                );
                audio = Some(AudioTrack {
                    track_id,
                    timescale,
                    duration: audio_duration,
                });
                audio_decoder = Some(decoder);
            } else {
                // Opus 以外の音声は対応しない (AAC 等)
                let codec = shiguredo_mp4::codec_string::from_sample_entry(&entry)
                    .unwrap_or_else(|_| "unknown".to_string());
                tracing::warn!(
                    "MP4 file '{}' has an unsupported audio codec ({codec}); publishing video only",
                    path.display()
                );
            }
        }

        // ループの周期は映像と音声のトラック尺の最大値にする
        let mut period_us = 0u64;
        if let Some(v) = &video {
            period_us = period_us.max(duration_us(v.duration, v.timescale));
        }
        if let Some(a) = &audio {
            period_us = period_us.max(duration_us(a.duration, a.timescale));
        }
        let video_period_units = video
            .as_ref()
            .map(|v| period_units(period_us, v.timescale))
            .unwrap_or(0);
        let audio_period_units = audio
            .as_ref()
            .map(|a| period_units(period_us, a.timescale))
            .unwrap_or(0);

        demuxer.seek(Duration::ZERO).map_err(|e| {
            Error::Other(format!("failed to seek MP4 file '{}': {e}", path.display()))
        })?;

        Ok(Self {
            data,
            demuxer,
            video,
            audio,
            video_decoder,
            audio_decoder,
            video_period_units,
            audio_period_units,
            video_loop_offset: 0,
            audio_loop_offset: 0,
            need_keyframe: true,
        })
    }

    /// 映像トラック情報を返す (映像を配信しない場合は None)
    pub fn video_info(&self) -> Option<VideoInfo> {
        self.video.as_ref().map(|v| VideoInfo {
            width: v.width,
            height: v.height,
            fps: v.fps,
            timescale: v.timescale,
        })
    }

    /// 音声トラック情報を返す (音声を配信しない場合は None)
    pub fn audio_info(&self) -> Option<AudioInfo> {
        self.audio.as_ref().map(|a| AudioInfo {
            timescale: a.timescale,
        })
    }

    /// デコードと供給を行う専用スレッドを開始する
    ///
    /// `video_sender` / `audio_sender` は配信するトラックのチャネルのみ渡す。
    /// 戻り値を drop すると停止要求を出してスレッドを join する。
    pub fn start(
        self,
        video_sender: Option<mpsc::Sender<VideoInput>>,
        audio_sender: Option<mpsc::Sender<AudioInput>>,
    ) -> Result<ReaderGuard> {
        if video_sender.is_some() != self.video.is_some()
            || audio_sender.is_some() != self.audio.is_some()
        {
            return Err(Error::Other(
                "MP4 re-encode reader channels do not match the enabled tracks".to_string(),
            ));
        }
        tracing::info!(
            "MP4 re-encode reader starting: video={} audio={}",
            self.video.is_some(),
            self.audio.is_some(),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("mp4-reencode".to_string())
            .spawn(move || run_reader(self, stop_thread, video_sender, audio_sender))
            .map_err(|e| Error::Other(format!("failed to spawn MP4 re-encode thread: {e}")))?;

        Ok(ReaderGuard::new(stop, handle))
    }

    /// 先頭のキーフレームまで読み飛ばして次のサンプルを返す
    ///
    /// 配信対象外のトラックのサンプルは読み飛ばす。末尾に達した場合は `Ok(None)` を返す。
    fn next_sample(&mut self) -> Result<Option<ReencodeSample>> {
        loop {
            let Some(sample) = self
                .demuxer
                .next_sample()
                .map_err(|e| Error::Other(format!("failed to read MP4 sample: {e}")))?
            else {
                return Ok(None);
            };
            let is_video = Some(sample.track.track_id) == self.video.as_ref().map(|v| v.track_id);
            let is_audio = Some(sample.track.track_id) == self.audio.as_ref().map(|a| a.track_id);
            if !is_video && !is_audio {
                continue;
            }
            if is_video && self.need_keyframe {
                if !sample.keyframe {
                    continue;
                }
                self.need_keyframe = false;
            }
            let offset = sample.data_offset as usize;
            let end = offset
                .checked_add(sample.data_size)
                .ok_or_else(|| Error::Other("MP4 sample offset is out of range".to_string()))?;
            let data = self
                .data
                .get(offset..end)
                .ok_or_else(|| Error::Other("MP4 sample offset is out of range".to_string()))?
                .to_vec();
            return Ok(Some(ReencodeSample {
                is_video,
                timestamp: sample.timestamp,
                composition_time_offset: sample.composition_time_offset,
                keyframe: sample.keyframe,
                data,
            }));
        }
    }

    /// 次の周回の先頭に戻る
    fn rewind(&mut self) -> Result<()> {
        if self.video.is_some() {
            self.video_loop_offset = self
                .video_loop_offset
                .checked_add(self.video_period_units)
                .ok_or_else(|| Error::Other("MP4 loop offset is out of range".to_string()))?;
        }
        if self.audio.is_some() {
            self.audio_loop_offset = self
                .audio_loop_offset
                .checked_add(self.audio_period_units)
                .ok_or_else(|| Error::Other("MP4 loop offset is out of range".to_string()))?;
        }
        self.demuxer
            .seek(Duration::ZERO)
            .map_err(|e| Error::Other(format!("failed to seek MP4 file: {e}")))?;
        self.need_keyframe = true;
        Ok(())
    }
}

/// 再エンコードのリーダースレッドの本体
fn run_reader(
    mut reader: Mp4ReencodeReader,
    stop: Arc<AtomicBool>,
    video_sender: Option<mpsc::Sender<VideoInput>>,
    audio_sender: Option<mpsc::Sender<AudioInput>>,
) -> Result<()> {
    // デコード結果を表示順に供給するための PTS 待ち行列
    let mut pts_queue = PtsQueue::new();
    // 20 ms 単位にバッファリングする PCM (S16 interleaved / mono)
    let mut pcm_buf: Vec<i16> = Vec::new();
    // pcm_buf の先頭のタイムスタンプ (トラックの timescale 単位)
    let mut pcm_buf_timestamp: u64 = 0;
    // pcm_buf の先頭から送信済みのサンプル数
    let mut pcm_buf_sent_samples: u64 = 0;

    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let Some(sample) = reader.next_sample()? else {
            reader.rewind()?;
            // 端数は次の周回へ繰り越さない
            pts_queue.clear();
            pcm_buf.clear();
            pcm_buf_sent_samples = 0;
            continue;
        };
        if sample.is_video {
            let video = reader
                .video
                .as_ref()
                .expect("video track enabled in next_sample");
            let decoder = reader
                .video_decoder
                .as_mut()
                .expect("video decoder enabled in start");
            let pts = sample
                .pts()
                .checked_add(reader.video_loop_offset)
                .ok_or_else(|| Error::Other("MP4 timestamp is out of range".to_string()))?;
            // AV1 は payload 側に Sequence Header が必要なため、無ければ config OBUs を付与する
            let payload = if video.codec == Mp4VideoCodec::Av1 {
                av1_payload_with_sequence_header(&video.config, sample.data)
            } else {
                sample.data
            };
            // avcC / hvcC はキーフレームで渡す (デコーダは変化時のみ適用する)
            let video_config = if video.codec != Mp4VideoCodec::Av1 && sample.keyframe {
                Some(video.config.as_slice())
            } else {
                None
            };
            let frames = decoder.decode(&payload, video_config)?;
            // 1 サンプル = 1 フレームのため、デコード順に入力した PTS を表示順に取り出す
            // (0 フレームを返した入力の PTS も登録しておく)
            pts_queue.push_input(pts);
            for frame in frames {
                let Some(frame_pts) = pts_queue.take_output() else {
                    tracing::warn!(
                        "MP4 decoder produced more frames than inputs; dropping the frame"
                    );
                    continue;
                };
                let frame = into_video_frame(frame);
                let Some(sender) = video_sender.as_ref() else {
                    continue;
                };
                let input = VideoInput::Reencode {
                    frame,
                    timestamp: frame_pts,
                };
                if !send_with_backpressure(sender, &stop, input) {
                    return Ok(());
                }
            }
        } else {
            let audio = reader
                .audio
                .as_ref()
                .expect("audio track enabled in next_sample");
            let decoder = reader
                .audio_decoder
                .as_mut()
                .expect("audio decoder enabled in start");
            let pcm = decoder.decode(&sample.data)?;
            if pcm_buf.is_empty() {
                pcm_buf_timestamp = sample
                    .timestamp
                    .checked_add(reader.audio_loop_offset)
                    .ok_or_else(|| Error::Other("MP4 timestamp is out of range".to_string()))?;
                pcm_buf_sent_samples = 0;
            }
            pcm_buf.extend_from_slice(&pcm);
            while pcm_buf.len() >= AUDIO_SAMPLES_PER_FRAME {
                let chunk: Vec<i16> = pcm_buf.drain(..AUDIO_SAMPLES_PER_FRAME).collect();
                let elapsed = (u128::from(pcm_buf_sent_samples) * u128::from(audio.timescale)
                    / u128::from(AUDIO_SAMPLE_RATE)) as u64;
                let timestamp = pcm_buf_timestamp
                    .checked_add(elapsed)
                    .ok_or_else(|| Error::Other("MP4 timestamp is out of range".to_string()))?;
                let frame = build_audio_frame(&chunk, timestamp, audio.timescale);
                let Some(sender) = audio_sender.as_ref() else {
                    continue;
                };
                let input = AudioInput::Reencode { frame, timestamp };
                if !send_with_backpressure(sender, &stop, input) {
                    return Ok(());
                }
                pcm_buf_sent_samples += AUDIO_SAMPLES_PER_FRAME as u64;
            }
        }
    }
}

/// 入力コーデックに対応するビデオデコーダを生成する
fn build_video_decoder(codec: Mp4VideoCodec) -> Result<VideoDecoder> {
    match codec {
        Mp4VideoCodec::Av1 => Ok(VideoDecoder::Av1(decoder::av1::Av1Decoder::new()?)),
        #[cfg(target_os = "macos")]
        Mp4VideoCodec::H264 => Ok(VideoDecoder::H264(decoder::h264::H264Decoder::new()?)),
        #[cfg(target_os = "macos")]
        Mp4VideoCodec::H265 => Ok(VideoDecoder::H265(decoder::h265::H265Decoder::new()?)),
        #[cfg(not(target_os = "macos"))]
        Mp4VideoCodec::H264 | Mp4VideoCodec::H265 => Err(Error::Other(
            "H.264 / H.265 input MP4 is only supported on macOS; rebuild on macOS or use an AV1 MP4"
                .to_string(),
        )),
    }
}

/// 8-bit 4:2:0 以外の映像サンプルエントリーを拒否する
fn validate_8bit_420(entry: &SampleEntry) -> Result<()> {
    let (chroma_ok, depth_ok) = match entry {
        SampleEntry::Avc1(b) => (
            b.avcc_box.chroma_format.is_none_or(|v| v.get() == 1),
            b.avcc_box
                .bit_depth_luma_minus8
                .is_none_or(|v| v.get() == 0)
                && b.avcc_box
                    .bit_depth_chroma_minus8
                    .is_none_or(|v| v.get() == 0),
        ),
        SampleEntry::Hvc1(b) => (
            b.hvcc_box.chroma_format_idc.get() == 1,
            b.hvcc_box.bit_depth_luma_minus8.get() == 0
                && b.hvcc_box.bit_depth_chroma_minus8.get() == 0,
        ),
        SampleEntry::Hev1(b) => (
            b.hvcc_box.chroma_format_idc.get() == 1,
            b.hvcc_box.bit_depth_luma_minus8.get() == 0
                && b.hvcc_box.bit_depth_chroma_minus8.get() == 0,
        ),
        SampleEntry::Av01(b) => (
            b.av1c_box.monochrome.get() == 0
                && b.av1c_box.chroma_subsampling_x.get() == 1
                && b.av1c_box.chroma_subsampling_y.get() == 1,
            b.av1c_box.high_bitdepth.get() == 0,
        ),
        // ここには来ない (コーデック判定済み)
        _ => (true, true),
    };
    if !chroma_ok || !depth_ok {
        return Err(Error::Other(
            "MP4 video track is not 8-bit 4:2:0; only 8-bit 4:2:0 is supported for re-encoding"
                .to_string(),
        ));
    }
    Ok(())
}

/// デコード済み I420 フレームを packed NV12 のフレームに変換する
///
/// 既存のエンコーダは stride 付き入力を取らないため、パディングの無いバッファにする。
fn into_video_frame(frame: DecodedVideoFrame) -> VideoFrameOwned {
    let width = frame.width;
    let height = frame.height;
    let uv = interleave_uv(&frame.u, &frame.v);
    VideoFrameOwned {
        data: frame.y,
        uv_data: Some(uv),
        width,
        height,
        stride: width,
        stride_uv: width,
        pixel_format: VideoPixelFormat::Nv12,
        timestamp_us: 0,
        pixel_buffer: None,
    }
}

/// 20 ms 分の PCM からパイプラインへ渡す音声フレームを構築する
fn build_audio_frame(pcm: &[i16], timestamp: u64, timescale: u64) -> AudioFrameOwned {
    let mut data = Vec::new();
    for sample in pcm {
        data.extend_from_slice(&sample.to_le_bytes());
    }
    // timescale は NonZeroU32 由来のため 0 にはならない
    let timestamp_us = (u128::from(timestamp) * 1_000_000 / u128::from(timescale)) as i64;
    AudioFrameOwned {
        data,
        frames: AUDIO_SAMPLES_PER_FRAME as i32,
        channels: i32::from(AUDIO_CHANNELS),
        sample_rate: AUDIO_SAMPLE_RATE as i32,
        format: AudioFormat::S16,
        timestamp_us,
    }
}

/// メディア時刻 (timescale 単位) をマイクロ秒に変換する
fn duration_us(duration: u64, timescale: u64) -> u64 {
    // timescale は NonZeroU32 由来のため 0 にはならない
    (u128::from(duration) * 1_000_000 / u128::from(timescale)) as u64
}

/// マイクロ秒をトラックの timescale 単位に変換する
fn period_units(period_us: u64, timescale: u64) -> u64 {
    (u128::from(period_us) * u128::from(timescale) / 1_000_000) as u64
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::{Seek, SeekFrom, Write};
    use std::num::NonZeroU32;
    use std::path::PathBuf;

    use shiguredo_mp4::bitstream::av1::{Av1SampleEntryConfig, build_av01_box_from_config_obus};
    use shiguredo_mp4::mux::{Mp4FileMuxer, Sample};

    use super::*;

    /// テスト用の出力パスを作る
    ///
    /// 前回クラッシュの残骸で偽陽性にならないよう、使用前に削除する。
    fn temp_path(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "moq-pub-reencode-{}-{name}.mp4",
            std::process::id()
        ));
        std::fs::remove_file(&path).ok();
        path
    }

    /// テスト用に書き出すサンプル
    struct TestSample<'a> {
        /// キーフレームかどうか
        keyframe: bool,
        /// 尺 (トラックの timescale 単位)
        duration: u32,
        /// サンプルデータ
        data: &'a [u8],
    }

    /// テスト用の 1 トラック MP4 を書き出す
    fn write_test_mp4(
        path: &Path,
        entry: &SampleEntry,
        track_kind: TrackKind,
        timescale: u32,
        samples: &[TestSample<'_>],
    ) {
        let mut muxer = Mp4FileMuxer::new().expect("MP4 muxer を作成できること");
        let initial_bytes = muxer.initial_boxes_bytes().to_vec();
        let mut file = File::create(path).expect("MP4 ファイルを作成できること");
        file.write_all(&initial_bytes)
            .expect("初期ボックスを書き込めること");
        let mut position = initial_bytes.len() as u64;
        for (i, sample) in samples.iter().enumerate() {
            file.write_all(sample.data)
                .expect("サンプルデータを書き込めること");
            muxer
                .append_sample(&Sample {
                    track_kind,
                    sample_entry: (i == 0).then(|| entry.clone()),
                    keyframe: sample.keyframe,
                    timescale: NonZeroU32::new(timescale).expect("timescale は非ゼロであること"),
                    duration: sample.duration,
                    composition_time_offset: None,
                    data_offset: position,
                    data_size: sample.data.len(),
                })
                .expect("サンプルを追加できること");
            position += sample.data.len() as u64;
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

    /// 入力 PTS は表示順 (最小値) で出力フレームへ対応付けられること
    ///
    /// B フレームではデコード順 (DTS) と表示順 (PTS) が異なる。0 フレームを返した入力の
    /// PTS も次の出力フレームへ引き継がれる。
    #[test]
    fn pts_queue_assigns_input_pts_in_display_order() {
        let mut queue = PtsQueue::new();
        // DTS 順に PTS 0, 3, 1 を入力する (B フレームがあるため表示順は 0, 1, 3)
        queue.push_input(0);
        queue.push_input(3);
        queue.push_input(1);
        assert_eq!(queue.take_output(), Some(0));
        assert_eq!(queue.take_output(), Some(1));
        // 4 番目のサンプル (PTS 2) を入力してから残りを取り出す
        queue.push_input(2);
        assert_eq!(queue.take_output(), Some(2));
        assert_eq!(queue.take_output(), Some(3));
        assert_eq!(queue.take_output(), None);

        // 0 フレームを返した入力の PTS も次の出力へ引き継がれる
        queue.push_input(100);
        queue.push_input(200);
        assert_eq!(queue.take_output(), Some(100));
        assert_eq!(queue.take_output(), Some(200));

        // 周回の先頭では破棄する
        queue.push_input(300);
        queue.clear();
        assert_eq!(queue.take_output(), None);
    }

    /// PTS はタイムスタンプとコンポジション時間オフセットの和になること
    #[test]
    fn sample_pts_adds_composition_time_offset() {
        let sample = ReencodeSample {
            is_video: true,
            timestamp: 3_000,
            composition_time_offset: Some(1_000),
            keyframe: false,
            data: Vec::new(),
        };
        assert_eq!(sample.pts(), 4_000);

        let sample = ReencodeSample {
            is_video: true,
            timestamp: 3_000,
            composition_time_offset: None,
            keyframe: false,
            data: Vec::new(),
        };
        assert_eq!(sample.pts(), 3_000);

        // 負になる PTS は 0 に丸める (編集リストは適用しない)
        let sample = ReencodeSample {
            is_video: true,
            timestamp: 100,
            composition_time_offset: Some(-1_000),
            keyframe: false,
            data: Vec::new(),
        };
        assert_eq!(sample.pts(), 0);
    }

    /// タイムスケール単位への変換が正しいこと
    #[test]
    fn media_time_conversions() {
        // 1 秒 = timescale 単位
        assert_eq!(duration_us(90_000, 90_000), 1_000_000);
        assert_eq!(period_units(1_000_000, 90_000), 90_000);
        assert_eq!(period_units(1_000_000, 1_000), 1_000);
    }

    /// 8-bit 4:2:0 以外の映像サンプルエントリーを拒否すること
    #[test]
    fn validate_8bit_420_rejects_unsupported_entries() {
        // AV1: 10-bit は拒否する
        let entry = SampleEntry::Av01(shiguredo_mp4::boxes::Av01Box {
            visual: test_visual_fields(),
            av1c_box: shiguredo_mp4::boxes::Av1cBox {
                seq_profile: shiguredo_mp4::Uint::new(0),
                seq_level_idx_0: shiguredo_mp4::Uint::new(0),
                seq_tier_0: shiguredo_mp4::Uint::new(0),
                high_bitdepth: shiguredo_mp4::Uint::new(1),
                twelve_bit: shiguredo_mp4::Uint::new(0),
                monochrome: shiguredo_mp4::Uint::new(0),
                chroma_subsampling_x: shiguredo_mp4::Uint::new(1),
                chroma_subsampling_y: shiguredo_mp4::Uint::new(1),
                chroma_sample_position: shiguredo_mp4::Uint::new(0),
                initial_presentation_delay_minus_one: None,
                config_obus: vec![0x0A, 0x00],
            },
            unknown_boxes: Vec::new(),
        });
        assert!(
            validate_8bit_420(&entry).is_err(),
            "10-bit の AV1 はエラーになること"
        );

        // AV1: 4:2:0 / 8-bit は許容する
        let entry = SampleEntry::Av01(shiguredo_mp4::boxes::Av01Box {
            visual: test_visual_fields(),
            av1c_box: shiguredo_mp4::boxes::Av1cBox {
                seq_profile: shiguredo_mp4::Uint::new(0),
                seq_level_idx_0: shiguredo_mp4::Uint::new(0),
                seq_tier_0: shiguredo_mp4::Uint::new(0),
                high_bitdepth: shiguredo_mp4::Uint::new(0),
                twelve_bit: shiguredo_mp4::Uint::new(0),
                monochrome: shiguredo_mp4::Uint::new(0),
                chroma_subsampling_x: shiguredo_mp4::Uint::new(1),
                chroma_subsampling_y: shiguredo_mp4::Uint::new(1),
                chroma_sample_position: shiguredo_mp4::Uint::new(0),
                initial_presentation_delay_minus_one: None,
                config_obus: vec![0x0A, 0x00],
            },
            unknown_boxes: Vec::new(),
        });
        assert!(
            validate_8bit_420(&entry).is_ok(),
            "8-bit 4:2:0 の AV1 は許容されること"
        );
    }

    /// テスト用の映像サンプルエントリー共通フィールドを構築する
    fn test_visual_fields() -> shiguredo_mp4::boxes::VisualSampleEntryFields {
        use shiguredo_mp4::boxes::VisualSampleEntryFields;
        VisualSampleEntryFields {
            data_reference_index: VisualSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
            width: 320,
            height: 240,
            horizresolution: VisualSampleEntryFields::DEFAULT_HORIZRESOLUTION,
            vertresolution: VisualSampleEntryFields::DEFAULT_VERTRESOLUTION,
            frame_count: VisualSampleEntryFields::DEFAULT_FRAME_COUNT,
            compressorname: VisualSampleEntryFields::NULL_COMPRESSORNAME,
            depth: VisualSampleEntryFields::DEFAULT_DEPTH,
        }
    }

    /// I420 から NV12 への UV インターリーブが正しいこと
    #[test]
    fn interleave_uv_pairs_u_and_v() {
        let u = [1u8, 3, 5];
        let v = [2u8, 4, 6];
        assert_eq!(interleave_uv(&u, &v), vec![1, 2, 3, 4, 5, 6]);
    }

    /// 音声フレームのタイムスタンプがマイクロ秒に変換されること
    #[test]
    fn audio_frame_uses_microsecond_timestamp() {
        let frame = build_audio_frame(&[0i16; AUDIO_SAMPLES_PER_FRAME], 48_000, 48_000);
        assert_eq!(frame.timestamp_us, 1_000_000);
        assert_eq!(frame.frames, AUDIO_SAMPLES_PER_FRAME as i32);
        assert_eq!(frame.channels, 1);
        assert_eq!(frame.sample_rate, 48_000);
        assert_eq!(frame.format, AudioFormat::S16);
        assert_eq!(frame.data.len(), AUDIO_SAMPLES_PER_FRAME * 2);
    }

    /// 実際の AV1 ビットストリームをデコードして表示順に供給できること
    ///
    /// moq-pub の AV1 エンコーダで生成したサンプルを MP4 に mux し、リーダーが
    /// デコードして生フレームを供給することを確認する。
    #[test]
    fn reader_decodes_av1_and_supplies_frames() {
        const WIDTH: u32 = 64;
        const HEIGHT: u32 = 64;
        const TIMESCALE: u32 = 30_000;
        const FRAME_DURATION: u32 = 1_000;

        // AV1 エンコーダで 3 フレーム生成する
        let mut encoder = crate::encoder::av1::Av1Encoder::new(WIDTH, HEIGHT, 30, 500, 60)
            .expect("AV1 エンコーダを作成できること");
        let frame = VideoFrameOwned {
            data: vec![128u8; (WIDTH * HEIGHT) as usize],
            uv_data: Some(vec![128u8; (WIDTH * HEIGHT / 2) as usize]),
            width: WIDTH as i32,
            height: HEIGHT as i32,
            stride: WIDTH as i32,
            stride_uv: WIDTH as i32,
            pixel_format: VideoPixelFormat::Nv12,
            timestamp_us: 0,
            pixel_buffer: None,
        };
        let mut samples = Vec::new();
        for _ in 0..3 {
            samples.extend(encoder.encode(&frame).expect("エンコードできること"));
        }
        assert_eq!(samples.len(), 3, "3 フレームが生成されること");

        // キーフレームの Sequence Header から av01 サンプルエントリーを構築する
        let config_obus = samples[0]
            .video_config
            .as_deref()
            .expect("キーフレームに Sequence Header があること");
        let entry = SampleEntry::Av01(
            build_av01_box_from_config_obus(
                config_obus,
                &Av1SampleEntryConfig {
                    initial_presentation_delay_minus_one: None,
                },
            )
            .expect("av01 サンプルエントリーを構築できること"),
        );

        // MP4 に mux する
        let path = temp_path("av1");
        let test_samples: Vec<TestSample<'_>> = samples
            .iter()
            .map(|sample| TestSample {
                keyframe: sample.is_keyframe,
                duration: FRAME_DURATION,
                data: &sample.data,
            })
            .collect();
        write_test_mp4(&path, &entry, TrackKind::Video, TIMESCALE, &test_samples);

        // リーダーでデコードして供給する
        let reader = Mp4ReencodeReader::open(&path, true, false).expect("MP4 を開けること");
        let info = reader.video_info().expect("映像トラック情報があること");
        assert_eq!(info.width, WIDTH);
        assert_eq!(info.height, HEIGHT);
        assert_eq!(info.fps, 30);
        assert_eq!(info.timescale, u64::from(TIMESCALE));

        let (sender, mut receiver) = mpsc::channel::<VideoInput>(4);
        let source = reader
            .start(Some(sender), None)
            .expect("スレッドを開始できること");
        let mut timestamps = Vec::new();
        for _ in 0..3 {
            let input = receiver.blocking_recv().expect("フレームを受信できること");
            let VideoInput::Reencode { frame, timestamp } = input else {
                panic!("再エンコードでは Reencode が届くこと");
            };
            assert_eq!(frame.width, WIDTH as i32);
            assert_eq!(frame.height, HEIGHT as i32);
            assert_eq!(frame.stride, WIDTH as i32);
            assert_eq!(frame.stride_uv, WIDTH as i32);
            assert_eq!(frame.data.len(), (WIDTH * HEIGHT) as usize);
            assert_eq!(
                frame.uv_data.as_ref().map(|uv| uv.len()),
                Some((WIDTH * HEIGHT / 2) as usize)
            );
            timestamps.push(timestamp);
        }
        source.stop().expect("停止できること");
        assert_eq!(
            timestamps,
            vec![0, 1_000, 2_000],
            "PTS がそのまま供給されること"
        );

        std::fs::remove_file(&path).ok();
    }

    /// 実際の Opus 音声をデコードして 20 ms 単位で供給できること
    #[test]
    fn reader_decodes_opus_and_supplies_audio_frames() {
        use crate::encoder::opus::OpusEncoder;
        use shiguredo_mp4::bitstream::opus::{ChannelCount, OpusSampleEntryConfig, build_opus_box};

        const TIMESCALE: u32 = 48_000;

        // Opus エンコーダで 3 パケット (20 ms 単位) 生成する
        let mut encoder =
            OpusEncoder::new(TIMESCALE, 1, 64_000).expect("Opus エンコーダを作成できること");
        let samples_per_frame = encoder.samples_per_frame();
        let pcm: Vec<i16> = (0..samples_per_frame)
            .map(|i| ((i as f64 * 0.1).sin() * 10_000.0) as i16)
            .collect();
        let packets: Vec<Vec<u8>> = (0..3)
            .map(|_| encoder.encode(&pcm).expect("エンコードできること"))
            .collect();

        // Opus サンプルエントリーを構築して MP4 に mux する (映像なし)
        let entry = SampleEntry::Opus(build_opus_box(&OpusSampleEntryConfig {
            channel_count: ChannelCount::Mono,
            pre_skip: 0,
            input_sample_rate: TIMESCALE,
            output_gain: 0,
        }));
        let test_samples: Vec<TestSample<'_>> = packets
            .iter()
            .map(|packet| TestSample {
                keyframe: true,
                duration: samples_per_frame as u32,
                data: packet,
            })
            .collect();
        let path = temp_path("opus");
        write_test_mp4(&path, &entry, TrackKind::Audio, TIMESCALE, &test_samples);

        // リーダーでデコードして供給する
        let reader = Mp4ReencodeReader::open(&path, false, true).expect("MP4 を開けること");
        assert!(reader.video_info().is_none(), "映像トラックは無いこと");
        assert_eq!(
            reader.audio_info().map(|info| info.timescale),
            Some(u64::from(TIMESCALE))
        );

        let (sender, mut receiver) = mpsc::channel::<AudioInput>(4);
        let source = reader
            .start(None, Some(sender))
            .expect("スレッドを開始できること");
        let mut timestamps = Vec::new();
        for _ in 0..3 {
            let input = receiver
                .blocking_recv()
                .expect("音声フレームを受信できること");
            let AudioInput::Reencode { frame, timestamp } = input else {
                panic!("再エンコードでは Reencode が届くこと");
            };
            assert_eq!(frame.frames, samples_per_frame as i32);
            assert_eq!(frame.channels, 1);
            assert_eq!(frame.sample_rate, TIMESCALE as i32);
            assert_eq!(frame.data.len(), samples_per_frame * 2);
            timestamps.push(timestamp);
        }
        source.stop().expect("停止できること");
        assert_eq!(
            timestamps,
            vec![0, samples_per_frame as u64, samples_per_frame as u64 * 2],
            "入力サンプルのタイムスタンプが使われること"
        );

        std::fs::remove_file(&path).ok();
    }
}
