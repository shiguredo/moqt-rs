//! 疑似音声キャプチャ
//!
//! 実マイクを掴まずに、440 Hz サイン波を 1 Hz の amplitude 変調で揺らし、
//! 48 kHz / 1ch / S16 の `AudioFrameOwned` として 20 ms (960 sample) ごとに
//! 送信する。

use std::f64::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use shiguredo_audio_device::{AudioFormat, AudioFrameOwned};
use tokio::sync::mpsc;

use crate::error::{Error, Result};

/// サンプリングレート (Hz)
const SAMPLE_RATE: i32 = 48_000;
/// チャンネル数
const CHANNELS: i32 = 1;
/// フレーム長 (サンプル数 / channel) = 20 ms @ 48 kHz
const SAMPLES_PER_FRAME: i32 = 960;
/// 基本周波数 (Hz)
const TONE_FREQ: f64 = 440.0;
/// amplitude 変調周波数 (Hz)
const AMP_MOD_FREQ: f64 = 1.0;
/// amplitude の中心値 (i16 フルスケールに対する比率)
const AMP_CENTER: f64 = 0.5;
/// amplitude 変調の振幅
const AMP_DEPTH: f64 = 0.2;

/// 疑似音声キャプチャのハンドル
///
/// Drop 時に生成スレッドへ停止を要求する。
pub struct FakeAudioCapture {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for FakeAudioCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// 疑似音声キャプチャを開始する
pub fn start_capture(sender: mpsc::Sender<AudioFrameOwned>) -> Result<FakeAudioCapture> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();

    let handle = std::thread::Builder::new()
        .name("fake-audio-capture".to_string())
        .spawn(move || run_capture_loop(stop_thread, sender))
        .map_err(|e| Error::Other(format!("failed to spawn fake audio capture thread: {e}")))?;

    tracing::info!(
        "Fake audio capture started ({} Hz sine, {} Hz, {} ch, S16, {} samples/frame)",
        TONE_FREQ as u32,
        SAMPLE_RATE,
        CHANNELS,
        SAMPLES_PER_FRAME,
    );

    Ok(FakeAudioCapture {
        stop,
        handle: Some(handle),
    })
}

/// 生成ループ本体
fn run_capture_loop(stop: Arc<AtomicBool>, sender: mpsc::Sender<AudioFrameOwned>) {
    let frame_interval =
        Duration::from_micros((SAMPLES_PER_FRAME as u64 * 1_000_000) / SAMPLE_RATE as u64);
    let mut sample_index: u64 = 0;
    let start = Instant::now();
    let mut next_deadline = start;
    let dt = 1.0 / SAMPLE_RATE as f64;

    let mut pcm = vec![0i16; SAMPLES_PER_FRAME as usize];

    while !stop.load(Ordering::Acquire) {
        for slot in pcm.iter_mut() {
            let t = sample_index as f64 * dt;
            let amp = AMP_CENTER + AMP_DEPTH * (TAU * AMP_MOD_FREQ * t).sin();
            let sample = amp * (TAU * TONE_FREQ * t).sin();
            *slot = (sample * i16::MAX as f64) as i16;
            sample_index += 1;
        }

        let timestamp_us = start.elapsed().as_micros() as i64;
        let mut data = vec![0u8; pcm.len() * 2];
        for (i, &s) in pcm.iter().enumerate() {
            let bytes = s.to_le_bytes();
            data[i * 2] = bytes[0];
            data[i * 2 + 1] = bytes[1];
        }

        let owned = AudioFrameOwned {
            data,
            frames: SAMPLES_PER_FRAME,
            channels: CHANNELS,
            sample_rate: SAMPLE_RATE,
            format: AudioFormat::S16,
            timestamp_us,
        };

        // チャネルが満杯の場合はフレームを破棄する (実キャプチャと同じ挙動)
        match sender.try_send(owned) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => break,
        }

        next_deadline += frame_interval;
        let now = Instant::now();
        if next_deadline > now {
            if crate::fake_capture::sleep_interruptibly(&stop, next_deadline - now) {
                return;
            }
        } else {
            // 生成が追いつかない場合は基準を現在時刻に戻す
            next_deadline = now;
        }
    }
}
