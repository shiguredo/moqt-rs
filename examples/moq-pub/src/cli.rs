//! publisher の CLI 引数定義とパース
//!
//! noargs でオプションを定義し、[`Config`] にまとめる。

use tokio_moq::{ServerUrl, Transport};

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
    /// トランスポート種別
    pub transport: Transport,
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
    /// MP4 ファイルの映像トラックをパススルー配信する入力パス
    pub input_mp4: Option<String>,
}

/// ユーザーが明示的に指定したオプションかどうかを判定する
///
/// noargs の `Opt::is_present()` は `default()` で埋めた値でも true を返すため、
/// コマンドライン引数または環境変数で指定された (`Long` / `Short` / `Env`) 場合だけを
/// 明示指定として扱う。
fn is_explicit(opt: &noargs::Opt) -> bool {
    matches!(
        opt,
        noargs::Opt::Long { .. } | noargs::Opt::Short { .. } | noargs::Opt::Env { .. }
    )
}

/// u32 のオプションを取り出し、明示指定されたかどうかも返す
fn take_u32(
    args: &mut noargs::RawArgs,
    name: &'static str,
    ty: &'static str,
    doc: &'static str,
    default: &'static str,
) -> noargs::Result<(u32, bool)> {
    let opt = noargs::opt(name)
        .ty(ty)
        .doc(doc)
        .default(default)
        .take(args);
    let explicit = is_explicit(&opt);
    Ok((opt.then(|o| o.value().parse::<u32>())?, explicit))
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
        .doc("Server URL (moqt://; '#msf:ns--track&c4m=BASE64' sends a C4M token in SETUP)")
        .take(&mut args)
        .then(|o| {
            let v = o.value();
            if v.is_empty() {
                return Err("--url is required".to_string());
            }
            tokio_moq::parse_url(v)
        })?;

    let transport: Transport = noargs::opt("transport")
        .ty("TYPE")
        .doc("Transport (quic | wt-h3 | wt-h2)")
        .default("quic")
        .take(&mut args)
        .then(|o| Transport::parse(o.value()))?;

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

    let (width, width_explicit) = take_u32(&mut args, "width", "PX", "Video width", "1280")?;

    let (height, height_explicit) = take_u32(&mut args, "height", "PX", "Video height", "720")?;

    let (fps, fps_explicit) = take_u32(&mut args, "fps", "FPS", "Frame rate", "30")?;

    let (bitrate, bitrate_explicit) = take_u32(
        &mut args,
        "bitrate",
        "KBPS",
        "Target bitrate in kbps",
        "2000",
    )?;

    let (keyframe_interval, keyframe_interval_explicit) = take_u32(
        &mut args,
        "keyframe-interval",
        "FRAMES",
        "Keyframe interval in frames",
        "60",
    )?;

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

    let video_codec_opt = noargs::opt("video-codec")
        .ty("CODEC")
        .doc("Video codec (av1 | h264 | h265). h264/h265 require macOS")
        .default("av1")
        .take(&mut args);
    let video_codec_explicit = is_explicit(&video_codec_opt);
    let video_codec: VideoCodec = video_codec_opt.then(|o| VideoCodec::parse(o.value()))?;

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

    let input_mp4: Option<String> = noargs::opt("input-mp4")
        .ty("PATH")
        .doc("Publish the video track of an MP4 file without re-encoding (audio is not published)")
        .take(&mut args)
        .present_and_then(|o| Ok::<_, std::convert::Infallible>(o.value().to_string()))?;

    let video_enabled = !no_video;
    // --input-mp4 は映像トラックだけを配信するため、音声トラックは常に配信しない
    let audio_enabled = !no_audio && input_mp4.is_none();
    if !args.metadata().help_mode {
        if let Some(path) = input_mp4.as_deref() {
            if path.is_empty() {
                return Err(noargs::Error::other(&args, "--input-mp4 must not be empty"));
            }
            if video_codec_explicit {
                return Err(noargs::Error::other(
                    &args,
                    "--input-mp4 cannot be used with --video-codec (the codec is detected from the MP4)",
                ));
            }
            if width_explicit || height_explicit || fps_explicit {
                return Err(noargs::Error::other(
                    &args,
                    "--input-mp4 cannot be used with --width / --height / --fps (they are detected from the MP4)",
                ));
            }
            if !video_enabled {
                return Err(noargs::Error::other(
                    &args,
                    "--input-mp4 cannot be used with --no-video (the MP4 video track is the publishing source)",
                ));
            }
            // 無視するオプションは黙って捨てずに警告する
            if device_id.is_some() {
                tracing::warn!("--device-id is ignored when --input-mp4 is set");
            }
            if fake_capture_device {
                tracing::warn!("--fake-capture-device is ignored when --input-mp4 is set");
            }
            if keyframe_interval_explicit {
                tracing::warn!(
                    "--keyframe-interval is ignored when --input-mp4 is set (groups start at the keyframes in the MP4)"
                );
            }
            if bitrate_explicit {
                tracing::warn!(
                    "--bitrate is ignored when --input-mp4 is set (the catalog bitrate is computed from the MP4)"
                );
            }
        } else if !video_enabled && !audio_enabled {
            return Err(noargs::Error::other(
                &args,
                "at least one of audio or video must be enabled (do not pass both --no-video and --no-audio)",
            ));
        }
    }

    if let Some(help) = args.finish()? {
        print!("{help}");
        return Ok(None);
    }

    Ok(Some(Config {
        url,
        transport,
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
        input_mp4,
    }))
}
