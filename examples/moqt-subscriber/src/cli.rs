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
    /// 音声の出力先
    pub audio_output_device: AudioOutputDevice,
}

/// 音声の出力先
///
/// 音声トラックの受信とデコードは指定にかかわらず行い、出力だけを切り替える。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioOutputDevice {
    /// SDL のデフォルト出力デバイスへ出力する
    Default,
    /// 出力しない (スピーカーへ音を出さずに受信とデコードだけを続ける)
    None,
}

impl AudioOutputDevice {
    /// CLI の値から音声の出力先を解決する
    ///
    /// 現在のプレイヤー (raw_player) はデフォルト出力デバイスしか開けないため、
    /// デバイス名の指定は受け付けず `default` と `none` だけを受理する。
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "default" => Ok(Self::Default),
            "none" => Ok(Self::None),
            other => Err(format!(
                "unsupported audio output device: {other} (supported values: default, none)"
            )),
        }
    }
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
        .doc("Server URL (moqt://host[:port]/path[?query][#type:value] or https://host[:port]/path[?query][#type:value])")
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

    let audio_output_device: AudioOutputDevice = noargs::opt("audio-output-device")
        .ty("DEVICE")
        .doc("Audio output device (\"none\" decodes audio without playing it; supported values: default, none)")
        .default("default")
        .take(&mut args)
        .then(|o| AudioOutputDevice::parse(o.value()))?;

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
        audio_output_device,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `default` と `none` だけを受理する
    #[test]
    fn audio_output_device_parses_supported_values() {
        assert_eq!(
            AudioOutputDevice::parse("default").expect("default は受理されること"),
            AudioOutputDevice::Default
        );
        assert_eq!(
            AudioOutputDevice::parse("none").expect("none は受理されること"),
            AudioOutputDevice::None
        );
    }

    /// 未対応のデバイス名・表記ゆれ・空文字はエラーにする
    #[test]
    fn audio_output_device_rejects_unsupported_values() {
        for value in ["coreaudio", "None", "DEFAULT", ""] {
            let err = AudioOutputDevice::parse(value).expect_err("受理しないこと");
            assert!(
                err.contains("unsupported audio output device"),
                "エラーに理由が含まれること: {err}"
            );
        }
    }
}
