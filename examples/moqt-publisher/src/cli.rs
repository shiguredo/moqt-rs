//! publisher の CLI 引数定義とパース
//!
//! noargs でオプションを定義し、[`Config`] にまとめる。

use moqt_example_transport::ServerUrl;

/// 映像コーデック種別
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    /// AV1 (libaom, 全プラットフォーム)
    Av1,
    /// H.264 (Apple Video Toolbox, macOS 限定)
    H264,
    /// H.265 / HEVC (Apple Video Toolbox, macOS 限定)
    H265,
}

impl VideoCodec {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "av1" => Ok(Self::Av1),
            "h264" => Ok(Self::H264),
            "h265" => Ok(Self::H265),
            other => Err(format!(
                "unknown video codec '{other}' (expected av1, h264, or h265)"
            )),
        }
    }
}

/// CLI オプション
pub struct Config {
    /// 接続先 URL
    pub url: ServerUrl,
    /// TLS CA 証明書パス
    pub cert: Option<String>,
    /// カメラデバイス ID
    pub device_id: Option<String>,
    /// 映像幅
    pub width: u32,
    /// 映像高さ
    pub height: u32,
    /// フレームレート
    pub fps: u32,
    /// ターゲットビットレート kbps
    pub bitrate: u32,
    /// キーフレーム間隔 (フレーム数)
    pub keyframe_interval: u32,
    /// Track Namespace
    pub namespace: String,
    /// Track Name
    pub track_name: String,
    /// 実カメラを掴まず raden で疑似映像を生成するかどうか
    pub fake_capture_device: bool,
    /// 映像コーデック
    pub video_codec: VideoCodec,
    /// 映像トラックを送信するかどうか
    pub video_enabled: bool,
    /// 音声トラックを送信するかどうか
    pub audio_enabled: bool,
    /// 音声入力デバイス ID
    pub audio_device_id: Option<String>,
    /// 音声ターゲットビットレート kbps
    pub audio_bitrate: u32,
    /// datagram 送信を使用するかどうか
    pub use_datagram: bool,
}

pub fn parse() -> noargs::Result<Option<Config>> {
    let mut args = noargs::raw_args();
    args.metadata_mut().app_name = env!("CARGO_PKG_NAME");
    args.metadata_mut().app_description = env!("CARGO_PKG_DESCRIPTION");

    if noargs::VERSION_FLAG.take(&mut args).is_present() {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return Ok(None);
    }
    noargs::HELP_FLAG.take_help(&mut args);

    let url: ServerUrl = noargs::opt("url")
        .short('u')
        .ty("URL")
        .doc("Server URL (moqt://host:port/path or https://host:port/path)")
        .take(&mut args)
        .then(|o| {
            let v = o.value();
            if v.is_empty() {
                return Err("--url is required".to_string());
            }
            moqt_example_transport::parse_url(v)
        })?;

    let cert: Option<String> = noargs::opt("cert")
        .ty("PATH")
        .doc("TLS CA certificate path (verification is skipped if omitted)")
        .take(&mut args)
        .present_and_then(|o| Ok::<_, std::convert::Infallible>(o.value().to_string()))?;

    let device_id: Option<String> = noargs::opt("device-id")
        .ty("ID")
        .doc("Camera device ID")
        .take(&mut args)
        .present_and_then(|o| Ok::<_, std::convert::Infallible>(o.value().to_string()))?;

    let width: u32 = noargs::opt("width")
        .ty("PX")
        .doc("Video width")
        .default("1280")
        .take(&mut args)
        .then(|o| o.value().parse::<u32>())?;

    let height: u32 = noargs::opt("height")
        .ty("PX")
        .doc("Video height")
        .default("720")
        .take(&mut args)
        .then(|o| o.value().parse::<u32>())?;

    let fps: u32 = noargs::opt("fps")
        .ty("FPS")
        .doc("Frame rate")
        .default("30")
        .take(&mut args)
        .then(|o| o.value().parse::<u32>())?;

    let bitrate: u32 = noargs::opt("bitrate")
        .ty("KBPS")
        .doc("Target bitrate in kbps")
        .default("2000")
        .take(&mut args)
        .then(|o| o.value().parse::<u32>())?;

    let keyframe_interval: u32 = noargs::opt("keyframe-interval")
        .ty("FRAMES")
        .doc("Keyframe interval in frames")
        .default("60")
        .take(&mut args)
        .then(|o| o.value().parse::<u32>())?;

    let namespace: String = noargs::opt("namespace")
        .ty("NS")
        .doc("Track namespace")
        .default("kaki")
        .take(&mut args)
        .then(|o| Ok::<_, std::convert::Infallible>(o.value().to_string()))?;

    let track_name: String = noargs::opt("track-name")
        .ty("NAME")
        .doc("Track name")
        .default("video")
        .take(&mut args)
        .then(|o| Ok::<_, std::convert::Infallible>(o.value().to_string()))?;

    let fake_capture_device: bool = noargs::flag("fake-capture-device")
        .doc("Use raden-generated synthetic video and 440 Hz sine wave audio instead of real devices")
        .take(&mut args)
        .is_present();

    let video_codec: VideoCodec = noargs::opt("video-codec")
        .ty("CODEC")
        .doc("Video codec (av1 | h264 | h265). h264/h265 require macOS")
        .default("av1")
        .take(&mut args)
        .then(|o| VideoCodec::parse(o.value()))?;

    let no_video: bool = noargs::flag("no-video")
        .doc("Disable video track publication")
        .take(&mut args)
        .is_present();

    let no_audio: bool = noargs::flag("no-audio")
        .doc("Disable audio track publication")
        .take(&mut args)
        .is_present();

    let audio_device_id: Option<String> = noargs::opt("audio-device-id")
        .ty("ID")
        .doc("Audio input device ID")
        .take(&mut args)
        .present_and_then(|o| Ok::<_, std::convert::Infallible>(o.value().to_string()))?;

    let audio_bitrate: u32 = noargs::opt("audio-bitrate")
        .ty("KBPS")
        .doc("Audio target bitrate in kbps")
        .default("64")
        .take(&mut args)
        .then(|o| o.value().parse::<u32>())?;

    let use_datagram: bool = noargs::flag("use-datagram")
        .doc("Use datagram for object delivery instead of subgroup streams")
        .take(&mut args)
        .is_present();

    let video_enabled = !no_video;
    let audio_enabled = !no_audio;
    if !args.metadata().help_mode && !video_enabled && !audio_enabled {
        return Err(noargs::Error::other(
            &args,
            "at least one of audio or video must be enabled (do not pass both --no-video and --no-audio)",
        ));
    }

    if let Some(help) = args.finish()? {
        print!("{help}");
        return Ok(None);
    }

    Ok(Some(Config {
        url,
        cert,
        device_id,
        width,
        height,
        fps,
        bitrate,
        keyframe_interval,
        namespace,
        track_name,
        fake_capture_device,
        video_codec,
        video_enabled,
        audio_enabled,
        audio_device_id,
        audio_bitrate,
        use_datagram,
    }))
}
