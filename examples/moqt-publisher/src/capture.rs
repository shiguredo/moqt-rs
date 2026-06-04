//! 実カメラからの映像キャプチャ
//!
//! `shiguredo_video_device` で入力デバイスを開き、NV12 フレームをチャネルへ送信する。

use shiguredo_video_device::{
    PixelFormat, VideoCapture, VideoCaptureConfig, VideoDeviceList, VideoFrameOwned,
};
use tokio::sync::mpsc;

use crate::cli::Config;
use crate::error::Result;

/// カメラキャプチャを開始する
///
/// 指定されたデバイスからフレームをキャプチャし、チャネルに送信する。
/// 返り値の VideoCapture を保持している間、キャプチャは継続する。
pub fn start_capture(
    config: &Config,
    sender: mpsc::Sender<VideoFrameOwned>,
) -> Result<VideoCapture> {
    // デバイスを列挙する
    if config.device_id.is_none() {
        let devices = VideoDeviceList::enumerate()?;
        tracing::info!("Available video devices:");
        for device in devices.as_slice() {
            let name = device.name().unwrap_or_else(|_| "unknown".to_string());
            let id = device.unique_id().unwrap_or_else(|_| "unknown".to_string());
            tracing::info!("  - {} ({})", name, id);
        }
    }

    let capture_config = VideoCaptureConfig {
        device_id: config.device_id.clone(),
        width: config.width as i32,
        height: config.height as i32,
        fps: config.fps as i32,
        pixel_format: Some(PixelFormat::Nv12),
    };

    let mut capture = VideoCapture::new(capture_config, move |frame| {
        let owned = frame.to_owned();
        // チャネルが満杯の場合はフレームを破棄する
        let _ = sender.try_send(owned);
    })?;

    capture.start()?;
    tracing::info!(
        "Camera capture started ({}x{} @ {} fps)",
        config.width,
        config.height,
        config.fps
    );

    Ok(capture)
}
