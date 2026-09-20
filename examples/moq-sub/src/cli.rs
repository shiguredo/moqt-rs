//! subscriber の CLI 引数定義とパース
//!
//! noargs でオプションを定義し、[`Config`] にまとめる。

use tokio_moq::{ServerUrl, Transport};

/// CLI オプション
#[derive(Clone)]
pub struct Config {
    /// 接続先 URL
    pub url: ServerUrl,
    /// トランスポート種別
    pub transport: Transport,
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
    /// 受信した映像・音声を保存する MP4 ファイルのパス
    pub mp4: Option<String>,
    /// 再生を行わないかどうか
    pub no_play: bool,
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

/// コマンドライン引数を解釈して [`Config`] を返す
///
/// ヘルプ表示やバージョン表示では `None` を返す。
pub fn parse() -> noargs::Result<Option<Config>> {
    parse_from(noargs::raw_args())
}

/// オプションの配列から設定を組み立てる
///
/// プログラム名は含めない (`--url` などのオプションだけを渡す)。コマンドライン以外
/// (E2E テストや組み込み用途) から同じオプションで設定を作るときに使う。
pub fn parse_args(args: &[&str]) -> noargs::Result<Option<Config>> {
    let mut argv = vec![env!("CARGO_PKG_NAME").to_string()];
    argv.extend(args.iter().map(|arg| (*arg).to_string()));
    parse_from(noargs::RawArgs::new(argv.into_iter()))
}

/// [`noargs::RawArgs`] を解釈して [`Config`] を返す
fn parse_from(mut args: noargs::RawArgs) -> noargs::Result<Option<Config>> {
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
        .doc("Server URL (moqt://host[:port]/path; '#msf:ns--track&c4m=BASE64' sends a C4M token in SETUP)")
        // 必須オプションは help モードでも Opt::None になり `then()` が MissingOpt を返すため、
        // ヘルプ表示のための例を与えて help モードでも先へ進めるようにする (通常の実行では
        // 例は使われず、`--url` の省略は従来どおり MissingOpt になる)
        .example("moqt://relay.example.com:4433/app")
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

    let mp4: Option<String> = noargs::opt("mp4")
        .ty("PATH")
        .doc("Save received video and audio to an MP4 file (overwritten if it exists)")
        .take(&mut args)
        .present_and_then(|o| Ok::<_, std::convert::Infallible>(o.value().to_string()))?;
    if mp4.as_deref() == Some("") {
        return Err(noargs::Error::other(&args, "--mp4 must not be empty"));
    }

    let no_play: bool = noargs::flag("no-play")
        .doc("Disable playback (do not initialize SDL and do not decode media)")
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
        transport,
        cert,
        namespace,
        video_enabled,
        audio_enabled,
        audio_output_device,
        mp4,
        no_play,
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
