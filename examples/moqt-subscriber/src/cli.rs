//! subscriber の CLI 引数定義とパース
//!
//! noargs でオプションを定義し、[`Config`] にまとめる。

use moqt_example_transport::ServerUrl;

/// CLI オプション
pub struct Config {
    /// 接続先 URL
    pub url: ServerUrl,
    /// TLS CA 証明書パス
    pub cert: Option<String>,
    /// Track Namespace
    pub namespace: String,
    /// 映像トラックを受信するかどうか
    pub video_enabled: bool,
    /// 音声トラックを受信するかどうか
    pub audio_enabled: bool,
}

pub fn parse() -> noargs::Result<Option<Config>> {
    let mut args = noargs::raw_args();
    args.metadata_mut().app_name = env!("CARGO_PKG_NAME");
    args.metadata_mut().app_description = "MoQT subscriber client";

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

    let namespace: String = noargs::opt("namespace")
        .ty("NS")
        .doc("Track namespace")
        .default("kaki")
        .take(&mut args)
        .then(|o| Ok::<_, std::convert::Infallible>(o.value().to_string()))?;

    let no_video: bool = noargs::flag("no-video")
        .doc("Disable video track subscription")
        .take(&mut args)
        .is_present();

    let no_audio: bool = noargs::flag("no-audio")
        .doc("Disable audio track subscription")
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
        namespace,
        video_enabled,
        audio_enabled,
    }))
}
