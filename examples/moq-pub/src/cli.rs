//! publisher の CLI 引数定義とパース
//!
//! noargs でオプションを定義し、[`Config`] にまとめる。

use shiguredo_moqt::message::common::TrackNamespace;
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
#[derive(Clone)]
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
    /// Track Namespace (draft-ietf-moq-transport-22 §8.8 表現をパースしたもの)
    pub namespace: TrackNamespace,
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
    /// 音声トラックを datagram で送るかどうか
    ///
    /// 映像は 1 group = 1 unidirectional stream で送る (draft-ietf-moq-loc-04 §4.2)。
    /// datagram が示されているのは §4.1 の音声の例だけである。
    pub audio_datagram: bool,
    /// object datagram の上限サイズ (bytes)
    ///
    /// 1 つの QUIC パケットに収まる必要があるため (draft-ietf-moq-loc-04 §4.1)、
    /// 上限を超える object は送らずに対処法を示すエラーにする。
    pub datagram_max_size: usize,
    /// MSF カタログのターゲットレイテンシ (ms)
    ///
    /// 受信側が符号化時刻からどれだけ遅らせて表示するかを示す
    /// (draft-ietf-moq-msf-01 §5.2.8)。音声と映像で同じ値を使う。
    pub target_latency_ms: u32,
    /// MP4 ファイルの映像トラックをパススルー配信する入力パス
    pub input_mp4: Option<String>,
    /// MP4 ファイルを再エンコードして配信する入力パス
    pub input_mp4_reencode: Option<String>,
}

/// `--datagram-max-size` の既定値 (bytes)
///
/// RFC 9000 §14 (Datagram Size) は、経路が最低限支える datagram サイズを 1200 bytes
/// (QUIC パケットヘッダと AEAD タグを含む UDP payload) と定める。判定するのは MOQT の
/// OBJECT_DATAGRAM (ヘッダ + Properties + payload) の長さであり、QUIC のパケットヘッダと
/// packet number、AEAD タグ、DATAGRAM frame の type / Length は含まれないため、その分の
/// 余裕を引いた値を既定にする。
pub(crate) const DEFAULT_DATAGRAM_MAX_SIZE: usize = 1160;

/// `--target-latency` の既定値 (ms)
///
/// live 配信の example としての値。音声と映像で同じ値を使うため、カタログの
/// `targetLatency` にも同じ値を載せる (draft-ietf-moq-msf-01 §5.2.8)。
pub(crate) const DEFAULT_TARGET_LATENCY_MS: u32 = 200;

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
        .doc("Transport (quic | wt-h3 | wt-h2; wt-h3 / wt-h2 (WebTransport) are experimental)")
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

    // `.default()` は `&'static str` しか取れないため、`DEFAULT_DATAGRAM_MAX_SIZE` と
    // 一致する値を文字列で渡す (一致は単体テストで固定する)
    let datagram_max_size_opt = noargs::opt("datagram-max-size")
        .ty("BYTES")
        .doc("Maximum size of an object datagram in bytes")
        .default("1160")
        .take(&mut args);
    let datagram_max_size_explicit = is_explicit(&datagram_max_size_opt);
    let datagram_max_size = datagram_max_size_opt.then(|o| {
        let value = o.value();
        let size = value
            .parse::<usize>()
            .map_err(|_| format!("invalid --datagram-max-size: {value}"))?;
        // 上限は datagram frame の最大値 (RFC 9221 §3 の max_datagram_frame_size と同じ 65535) で足りる
        if !(1..=65535).contains(&size) {
            return Err(format!(
                "--datagram-max-size must be between 1 and 65535 (default {}): {value}",
                DEFAULT_DATAGRAM_MAX_SIZE
            ));
        }
        Ok(size)
    })?;

    let audio_datagram: bool = noargs::flag("audio-datagram")
        .doc("Use datagrams for the audio track instead of subgroup streams")
        .take(&mut args)
        .is_present();

    // `.default()` は `&'static str` しか取れないため、`DEFAULT_TARGET_LATENCY_MS` と
    // 一致する値を文字列で渡す (一致は単体テストで固定する)
    let target_latency_opt = noargs::opt("target-latency")
        .ty("MS")
        .doc("Target latency in milliseconds for the catalog")
        .default("200")
        .take(&mut args);
    let target_latency_ms = target_latency_opt.then(|o| {
        let value = o.value();
        value.parse::<u32>().map_err(|_| {
            format!(
                "--target-latency must be a non-negative integer in milliseconds (default {}): {value}",
                DEFAULT_TARGET_LATENCY_MS
            )
        })
    })?;

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

    // datagram 配送を使わない場合、上限の指定は意味を持たない (黙って捨てずに警告する)
    if audio_datagram && !audio_enabled {
        tracing::warn!("--audio-datagram is ignored because the audio track is not published");
    }
    if datagram_max_size_explicit && (!audio_datagram || !audio_enabled) {
        tracing::warn!(
            "--datagram-max-size is ignored because the audio track is not sent as datagrams"
        );
    }
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
            if audio_datagram {
                tracing::warn!(
                    "--audio-datagram is ignored when --input-mp4 is set (audio is not published)"
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
        audio_datagram,
        datagram_max_size,
        target_latency_ms,
        input_mp4,
        input_mp4_reencode,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use shiguredo_moqt::name;

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

    /// `--url` を省略した通常の実行は従来どおりエラーになる
    ///
    /// ヘルプ表示用の `.example()` が必須オプションの検証を緩めないことを固定する。
    #[test]
    fn missing_url_is_still_an_error() {
        assert!(
            parse_args(&[]).is_err(),
            "必須オプションの欠如はエラーになること"
        );
    }

    /// テスト用の必須引数
    const BASE_ARGS: &[&str] = &[
        "--url",
        "moqt://127.0.0.1:4443",
        "--namespace",
        "moq-example",
    ];

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
    /// 以前は `kaki` を既定値にしていたため、指定を忘れると `kaki` へ配信していた。
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

    /// `--datagram-max-size` は既定値を使い、指定で上書き、範囲外と数値でない値はエラーになる
    ///
    /// 既定値は RFC 9000 §14 (Datagram Size) が定める経路の最小 datagram サイズ (1200 = QUIC
    /// パケットヘッダと AEAD タグを含む UDP payload) から、QUIC のパケットヘッダと
    /// DATAGRAM frame のヘッダ分を引いた値である。
    #[test]
    fn datagram_max_size_is_configurable() {
        let config = parse_args(BASE_ARGS)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            config.datagram_max_size, DEFAULT_DATAGRAM_MAX_SIZE,
            "既定は経路の最小 datagram サイズに収まる値であること"
        );

        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--datagram-max-size", "1400"]);
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(config.datagram_max_size, 1400, "指定した上限が使われること");

        for value in ["1x", "0", "65536"] {
            let mut args = BASE_ARGS.to_vec();
            args.extend_from_slice(&["--datagram-max-size", value]);
            assert!(
                parse_args(&args).is_err(),
                "範囲外と数値でない上限はエラーになること: {value}"
            );
        }
    }

    /// `--target-latency` は既定値を使い、指定で上書き、数値でない値はエラーになる
    ///
    /// 既定の 200 ms は live 配信の example としての値である。受信側はこの値ぶん
    /// 遅らせて表示し、音声と映像で同じ値を使う (draft-ietf-moq-msf-01 §5.2.8)。
    #[test]
    fn target_latency_is_configurable() {
        let config = parse_args(BASE_ARGS)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            config.target_latency_ms, DEFAULT_TARGET_LATENCY_MS,
            "既定は 200 ms であること"
        );

        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--target-latency", "500"]);
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(config.target_latency_ms, 500, "指定した値が使われること");

        for value in ["1x", "-1"] {
            let mut args = BASE_ARGS.to_vec();
            args.extend_from_slice(&["--target-latency", value]);
            assert!(
                parse_args(&args).is_err(),
                "数値でない値と負の値はエラーになること: {value}"
            );
        }
    }

    /// 削除した `--use-datagram` は未定義のオプションとしてエラーになる
    ///
    /// 映像と音声の一括指定に戻さないよう、削除を回帰から守る。
    #[test]
    fn use_datagram_is_rejected() {
        let mut args = BASE_ARGS.to_vec();
        args.push("--use-datagram");
        assert!(
            parse_args(&args).is_err(),
            "削除した --use-datagram はエラーになること"
        );
    }

    /// `--audio-datagram` は既定で無効、指定で有効になる
    ///
    /// datagram 配送は音声トラックだけに指定できる (映像は 1 group = 1 unidirectional stream。
    /// draft-ietf-moq-loc-04 §4.2)。
    #[test]
    fn audio_datagram_flag_is_opt_in() {
        let config = parse_args(BASE_ARGS)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert!(
            !config.audio_datagram,
            "既定では音声トラックも subgroup stream で送ること"
        );

        let mut args = BASE_ARGS.to_vec();
        args.push("--audio-datagram");
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert!(
            config.audio_datagram,
            "指定で音声トラックを datagram で送ること"
        );
    }

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
