//! 実マイクからの音声キャプチャ
//!
//! `shiguredo_audio_device` で入力デバイスを開き、48 kHz / 1ch のフレームを
//! チャネルへ送信する。

use shiguredo_audio_device::{AudioCapture, AudioCaptureConfig, AudioDeviceList, AudioFrameOwned};
use tokio::sync::mpsc;

use crate::cli::Config;
use crate::error::Result;

/// 音声キャプチャを開始する
///
/// 指定されたデバイスからフレームをキャプチャし、チャネルに送信する。
/// 返り値の AudioCapture を保持している間、キャプチャは継続する。
pub fn start_capture(
    config: &Config,
    sender: mpsc::Sender<AudioFrameOwned>,
) -> Result<AudioCapture> {
    if config.audio_device_id.is_none() {
        let devices = AudioDeviceList::enumerate_input()?;
        tracing::info!("Available audio input devices:");
        for device in devices.as_slice() {
            let name = device.name().unwrap_or_else(|_| "unknown".to_string());
            let id = device.unique_id().unwrap_or_else(|_| "unknown".to_string());
            tracing::info!("  - {} ({})", name, id);
        }
    }

    let capture_config = AudioCaptureConfig {
        device_id: config.audio_device_id.clone(),
        sample_rate: 48_000,
        channels: 1,
    };

    let mut capture = AudioCapture::new(capture_config, move |frame| {
        let owned = frame.to_owned();
        // チャネルが満杯の場合はフレームを破棄する
        let _ = sender.try_send(owned);
    })?;

    capture.start()?;
    tracing::info!("Audio capture started (48000 Hz, 1ch)");

    Ok(capture)
}
