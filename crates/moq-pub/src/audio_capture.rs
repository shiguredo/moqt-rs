//! 実マイクからの音声キャプチャ
//!
//! `shiguredo_audio_device` で入力デバイスを開き、48 kHz / 1ch のフレームを
//! チャネルへ送信する。

use shiguredo_audio_device::{AudioCapture, AudioCaptureConfig, AudioDeviceList};
use tokio::sync::mpsc;

use crate::cli::Config;
use crate::error::Result;
use crate::pipeline::AudioInput;

/// 音声キャプチャを構築する
///
/// 指定されたデバイスからフレームをキャプチャする [AudioCapture] を構築する。この関数は
/// キャプチャを開始しない。開始するには [`AudioCapture::start`] を呼び、返るセッション
/// ガードをキャプチャを止めるまで保持すること。
pub fn new_capture(config: &Config, sender: mpsc::Sender<AudioInput>) -> Result<AudioCapture> {
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

    let capture = AudioCapture::new(capture_config, move |frame| {
        let owned = frame.to_owned();
        // チャネルが満杯の場合はフレームを破棄する
        let _ = sender.try_send(AudioInput::Raw(owned));
    })?;

    Ok(capture)
}
