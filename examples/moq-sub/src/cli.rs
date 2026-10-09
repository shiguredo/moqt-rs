//! subscriber の CLI 引数定義とパース
//!
//! noargs でオプションを定義し、[`Config`] にまとめる。

use shiguredo_moqt::message::common::TrackNamespace;
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
    /// Track Namespace (draft-ietf-moq-transport-22 §8.8 表現をパースしたもの)
    pub namespace: TrackNamespace,
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

    // `--namespace` は省略可能で、省略した場合は `--url` の msf fragment
    // (draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation)) の
    // track-identifier が示す namespace を使う。どちらにも無い場合はエラーになる。
    let namespace_opt = noargs::opt("namespace")
        .ty("NS")
        .doc("Track namespace (draft-ietf-moq-transport-22 §8.8 form; '-' separates namespace fields, e.g. moq-example). Defaults to the namespace of the MSF fragment in --url")
        // 省略可能だが、help モードで値が無いと namespace の解決に失敗するため、ヘルプ表示の
        // ための例を与える (通常の実行では、指定が無いときに例は使われない)
        .example("moq-example")
        .take(&mut args);
    let namespace_value: Option<String> = namespace_opt
        .present_and_then(|o| Ok::<_, std::convert::Infallible>(o.value().to_string()))?;
    let namespace: TrackNamespace = tokio_moq::resolve_namespace(namespace_value.as_deref(), &url)
        .map_err(|e| noargs::Error::other(&args, e))?;

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

    use shiguredo_moqt::name;

    /// テスト用の必須引数
    const BASE_ARGS: &[&str] = &[
        "--url",
        "moqt://127.0.0.1:4443",
        "--namespace",
        "moq-example",
    ];

    /// `--help` / `-h` は必須オプションを要求せずヘルプを返す
    ///
    /// 必須オプションに `.example()` を与えていないと、help モードでも `Opt::None` に
    /// なって `then()` が `MissingOpt` を返し、ヘルプが表示されない。
    #[test]
    fn help_flag_prints_help_without_required_options() {
        for flag in ["--help", "-h"] {
            assert!(
                parse_args(&[flag])
                    .expect("ヘルプはエラーにならないこと")
                    .is_none(),
                "ヘルプ表示は設定を返さないこと: {flag}"
            );
        }
    }

    /// `--namespace` に指定した値が設定に入る
    #[test]
    fn namespace_uses_the_explicit_option() {
        let config = parse_args(BASE_ARGS)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            name::serialize_namespace(&config.namespace),
            "moq-example",
            "指定した namespace が使われること"
        );
    }

    /// `--namespace` を省略した場合は `--url` の `msf` fragment の namespace を使う
    ///
    /// 以前は `kaki` を既定値にしていたため、指定を忘れると `kaki` を購読していた。
    /// 現在は `--url` の `msf` fragment から取り、どちらにも無い場合はエラーにする。
    #[test]
    fn namespace_falls_back_to_the_msf_fragment() {
        let args = ["--url", "moqt://127.0.0.1:4443#msf:spam-egg--video"];
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            name::serialize_namespace(&config.namespace),
            "spam-egg",
            "msf fragment の namespace が使われること"
        );
        assert_eq!(
            config.namespace.fields().len(),
            2,
            "#msf:spam-egg--video の '-' が namespace フィールドの区切りになること"
        );

        // MSF URI の track-identifier は URI 層の `%XX` をデータバイトとしてデコードするため、
        // `%2D` は区切りではなく 1 フィールドの `-` になる
        let args = ["--url", "moqt://127.0.0.1:4443#msf:spam%2Degg--video"];
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            config.namespace.fields().to_vec(),
            vec![b"spam-egg".to_vec()],
            "'%2D' がリテラルの '-' として 1 フィールドになること"
        );
    }

    /// `--namespace` と `msf` fragment の両方がある場合は `--namespace` を使う
    #[test]
    fn namespace_option_overrides_the_msf_fragment() {
        let args = [
            "--url",
            "moqt://127.0.0.1:4443#msf:spam-egg--video",
            "--namespace",
            "moq-example",
        ];
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            name::serialize_namespace(&config.namespace),
            "moq-example",
            "指定した namespace が msf fragment より優先されること"
        );
    }

    /// `--namespace` も `msf` fragment も無い場合はエラーになる
    #[test]
    fn namespace_is_required_without_msf_fragment() {
        for url in [
            // fragment が無い
            "moqt://127.0.0.1:4443",
            // msf 以外の fragment type
            "moqt://127.0.0.1:4443#type:moq-example--video",
            // msf fragment の namespace が 0 フィールド
            "moqt://127.0.0.1:4443#msf:--video",
        ] {
            let args = ["--url", url];
            assert!(
                parse_args(&args).is_err(),
                "namespace を解決できない場合はエラーになること: {url}"
            );
        }
    }

    /// `--namespace` の空文字はエラーになる
    ///
    /// 空文字は §8.8 では 0 フィールドの namespace になるが、example は 1 つ以上のフィールドを
    /// 持つ namespace を扱うため、CLI の時点で拒否する。
    #[test]
    fn namespace_rejects_empty_value() {
        let args = ["--url", "moqt://127.0.0.1:4443", "--namespace", ""];
        assert!(
            parse_args(&args).is_err(),
            "空の namespace はエラーになること"
        );
    }

    /// `--namespace` は §8.8 表現として解釈し、`-` を namespace フィールドの区切りにする
    ///
    /// `-` は区切りなので、フィールド内のリテラルな `-` は `.2d` として書く。
    #[test]
    fn namespace_uses_the_section_8_8_form() {
        let args = ["--url", "moqt://127.0.0.1:4443", "--namespace", "spam-egg"];
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            name::serialize_namespace(&config.namespace),
            "spam-egg",
            "'-' が区切りとして往復すること"
        );
        assert_eq!(
            config.namespace.fields().len(),
            2,
            "'spam-egg' は 2 フィールドになること"
        );

        let args = [
            "--url",
            "moqt://127.0.0.1:4443",
            "--namespace",
            "spam.2degg",
        ];
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            config.namespace.fields().to_vec(),
            vec![b"spam-egg".to_vec()],
            "'.2d' がリテラルの '-' として 1 フィールドになること"
        );
    }

    /// §8.8 として不正な `--namespace` はエラーになる
    ///
    /// `/` はリテラル表現できないため `.2f` として書く必要があり、裸のままだとエラーになる。
    #[test]
    fn namespace_rejects_non_canonical_values() {
        for value in [
            // リテラル表現できないバイト
            "moq/example",
            // 連続ハイフンによる空フィールド
            "moq--example",
            // 先頭 / 末尾のハイフンによる空フィールド
            "-moq",
            "moq-",
            // リテラル表現可能バイトの hex 化
            ".61",
            // 大文字 hex
            "moq.2Et",
        ] {
            let args = ["--url", "moqt://127.0.0.1:4443", "--namespace", value];
            assert!(
                parse_args(&args).is_err(),
                "§8.8 として不正な値はエラーになること: {value}"
            );
        }
    }

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
