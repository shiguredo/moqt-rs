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
    /// MP4 ファイルを再エンコードして配信する入力パス
    pub input_mp4_reencode: Option<String>,
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
    parse_from(noargs::raw_args())
}

/// 生の引数から設定をパースする
///
/// `--version` / `--help` の場合は `Ok(None)` を返す。
fn parse_from(mut args: noargs::RawArgs) -> noargs::Result<Option<Config>> {
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

    let (audio_bitrate, audio_bitrate_explicit) = take_u32(
        &mut args,
        "audio-bitrate",
        "KBPS",
        "Audio target bitrate in kbps",
        "64",
    )?;

    let use_datagram: bool = noargs::flag("use-datagram")
        .doc("Use datagram for object delivery instead of subgroup streams")
        .take(&mut args)
        .is_present();

    let input_mp4: Option<String> = noargs::opt("input-mp4")
        .ty("PATH")
        .doc("Publish the video track of an MP4 file without re-encoding (audio is not published)")
        .take(&mut args)
        .present_and_then(|o| Ok::<_, std::convert::Infallible>(o.value().to_string()))?;

    let input_mp4_reencode: Option<String> = noargs::opt("input-mp4-reencode")
        .ty("PATH")
        .doc("Decode and re-encode the video and audio tracks of an MP4 file")
        .take(&mut args)
        .present_and_then(|o| Ok::<_, std::convert::Infallible>(o.value().to_string()))?;

    let video_enabled = !no_video;
    // --input-mp4 は映像トラックだけを配信するため、音声トラックは常に配信しない
    let audio_enabled = !no_audio && input_mp4.is_none();
    if !args.metadata().help_mode {
        if input_mp4.is_some() && input_mp4_reencode.is_some() {
            return Err(noargs::Error::other(
                &args,
                "--input-mp4 and --input-mp4-reencode cannot be used together",
            ));
        }
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
            if audio_device_id.is_some() {
                tracing::warn!(
                    "--audio-device-id is ignored when --input-mp4 is set (audio is not published)"
                );
            }
            if audio_bitrate_explicit {
                tracing::warn!(
                    "--audio-bitrate is ignored when --input-mp4 is set (audio is not published)"
                );
            }
        }
        if let Some(path) = input_mp4_reencode.as_deref() {
            if path.is_empty() {
                return Err(noargs::Error::other(
                    &args,
                    "--input-mp4-reencode must not be empty",
                ));
            }
            if width_explicit || height_explicit || fps_explicit {
                return Err(noargs::Error::other(
                    &args,
                    "--input-mp4-reencode cannot be used with --width / --height / --fps (they are detected from the MP4)",
                ));
            }
            // 無視するオプションは黙って捨てずに警告する
            if device_id.is_some() {
                tracing::warn!("--device-id is ignored when --input-mp4-reencode is set");
            }
            if fake_capture_device {
                tracing::warn!("--fake-capture-device is ignored when --input-mp4-reencode is set");
            }
            if audio_device_id.is_some() {
                tracing::warn!(
                    "--audio-device-id is ignored when --input-mp4-reencode is set (audio is decoded from the MP4)"
                );
            }
        }
        if !video_enabled && !audio_enabled {
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
        input_mp4_reencode,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 引数リストをパースする (先頭はプログラム名として扱われる)
    fn parse_args(args: &[&str]) -> noargs::Result<Option<Config>> {
        let mut all = vec![env!("CARGO_PKG_NAME").to_string()];
        all.extend(args.iter().map(|arg| arg.to_string()));
        parse_from(noargs::RawArgs::new(all.into_iter()))
    }

    /// テスト用の必須引数
    const BASE_ARGS: &[&str] = &["--url", "moqt://127.0.0.1:4443"];

    /// `--input-mp4` と明示指定した映像オプションの併用がエラーになること
    #[test]
    fn input_mp4_rejects_explicit_video_options() {
        let cases: &[&[&str]] = &[
            &["--video-codec", "h264"],
            &["--width", "640"],
            &["--height", "480"],
            &["--fps", "60"],
            &["--no-video"],
        ];
        for extra in cases {
            let mut args = BASE_ARGS.to_vec();
            args.extend_from_slice(&["--input-mp4", "input.mp4"]);
            args.extend_from_slice(extra);
            assert!(
                parse_args(&args).is_err(),
                "併用はエラーになること: {args:?}"
            );
        }
    }

    /// `--input-mp4` が空文字の場合はエラーになること
    #[test]
    fn input_mp4_rejects_empty_path() {
        let args = [BASE_ARGS, &["--input-mp4", ""]].concat();
        assert!(parse_args(&args).is_err(), "空のパスはエラーになること");
    }

    /// `--input-mp4-reencode` の併用エラーと音声有効のテスト
    #[test]
    fn input_mp4_reencode_validations() {
        // --input-mp4 との同時指定はエラー
        let args = [
            BASE_ARGS,
            &["--input-mp4", "a.mp4", "--input-mp4-reencode", "b.mp4"],
        ]
        .concat();
        assert!(
            parse_args(&args).is_err(),
            "--input-mp4 との同時指定はエラーになること"
        );

        // --width / --height / --fps の明示指定はエラー
        for extra in [["--width", "640"], ["--height", "480"], ["--fps", "60"]] {
            let mut args = BASE_ARGS.to_vec();
            args.extend_from_slice(&["--input-mp4-reencode", "input.mp4"]);
            args.extend_from_slice(&extra);
            assert!(
                parse_args(&args).is_err(),
                "解像度 / フレームレートの指定はエラーになること: {args:?}"
            );
        }

        // --no-video / --no-audio は指定できる
        for extra in [&["--no-video"][..], &["--no-audio"][..]] {
            let mut args = BASE_ARGS.to_vec();
            args.extend_from_slice(&["--input-mp4-reencode", "input.mp4"]);
            args.extend_from_slice(extra);
            assert!(
                parse_args(&args).is_ok(),
                "トラック単位の無効化はエラーにならないこと: {args:?}"
            );
        }

        // --audio-bitrate / --video-codec は再エンコードの設定として使う (エラーにならない)
        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&[
            "--input-mp4-reencode",
            "input.mp4",
            "--video-codec",
            "h264",
            "--audio-bitrate",
            "96",
        ]);
        let config = parse_args(&args)
            .expect("パースできること")
            .expect("設定が返ること");
        assert!(config.audio_enabled, "音声トラックは既定で有効であること");
    }

    /// `--input-mp4` では音声トラックを配信しないこと
    #[test]
    fn input_mp4_disables_audio_track() {
        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--input-mp4", "input.mp4"]);
        let config = parse_args(&args)
            .expect("パースできること")
            .expect("設定が返ること");
        assert_eq!(config.input_mp4.as_deref(), Some("input.mp4"));
        assert!(config.video_enabled, "映像トラックは有効であること");
        assert!(!config.audio_enabled, "音声トラックは配信しないこと");
    }

    /// `--input-mp4` を指定しない場合は映像 / 音声の既定が変わらないこと
    #[test]
    fn without_input_mp4_keeps_audio_enabled() {
        let config = parse_args(BASE_ARGS)
            .expect("パースできること")
            .expect("設定が返ること");
        assert!(config.input_mp4.is_none());
        assert!(config.video_enabled, "映像トラックは有効であること");
        assert!(config.audio_enabled, "音声トラックは有効であること");
    }
}
