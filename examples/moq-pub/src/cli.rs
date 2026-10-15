//! publisher の CLI 引数定義とパース
//!
//! noargs でオプションを定義し、[`Config`] にまとめる。

use shiguredo_moqt::message::common::TrackNamespace;
use shiguredo_moqt::msf::MSF_CATALOG_TRACK_NAME;
use tokio_moq::{ServerUrl, Transport};

use crate::pipeline::AUDIO_TRACK_NAME;

/// 映像コーデック種別
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    /// AV1 (libaom, 全プラットフォーム)
    Av1,
    /// H.264 (Apple Video Toolbox, macOS 限定)
    H264,
    /// H.265 / HEVC (Apple Video Toolbox, macOS 限定)
    H265,
    /// VP8 (libvpx、Windows 非対応)
    Vp8,
    /// VP9 (libvpx、Windows 非対応)
    Vp9,
}

impl VideoCodec {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "av1" => Ok(Self::Av1),
            "h264" => Ok(Self::H264),
            "h265" => Ok(Self::H265),
            "vp8" => Ok(Self::Vp8),
            "vp9" => Ok(Self::Vp9),
            other => Err(format!(
                "unknown video codec '{other}' (expected av1, h264, h265, vp8, or vp9)"
            )),
        }
    }
}

/// 音声コーデック種別
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    /// Opus (libopus, 全プラットフォーム)
    Opus,
    /// AAC-LC (Apple Audio Toolbox, macOS 限定)
    Aac,
}

impl AudioCodec {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "opus" => Ok(Self::Opus),
            "aac" => Ok(Self::Aac),
            other => Err(format!(
                "unknown audio codec '{other}' (expected opus or aac)"
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
    ///
    /// `--transport` の指定、無ければ `--url` の `msf` fragment の `connection` で解決する
    /// (`tokio_moq::resolve_transport`)。
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
    /// 映像の Group 長 (ms)
    ///
    /// 入力フレームの Timestamp の差分がこの値に達したらキーフレームを強制し、
    /// 新しい Group を開始する。Group は 1 キーフレーム間隔 = 1 GOP であるため、
    /// MSF カタログの `maxGopDuration` / `maxGroupDuration` の基になる
    /// (draft-ietf-moq-msf-01 §5.2.24 / §5.2.25)。
    pub gop_duration_ms: u32,
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
    /// 音声コーデック
    pub audio_codec: AudioCodec,
    /// 音声ターゲットビットレート kbps
    pub audio_bitrate: u32,
    /// 音声トラックを datagram で送るかどうか
    ///
    /// 既定は真である (draft-ietf-moq-loc-04 §4.1 (Application with one audio track) は
    /// 1 audio frame = 1 Object = 1 Group と定める)。datagram は再送されないため、遅れた
    /// 音声を届けるより落とす方が A/V のずれを残さない。映像は 1 group = 1 unidirectional
    /// stream で送る (§4.2)。`--audio-subgroup` で subgroup 配送へ切り替える。
    pub audio_datagram: bool,
    /// 音声 Object を 1 つにつき何回 datagram で送るか (1〜3)
    ///
    /// datagram は再送されない (RFC 9221 §3) ため、n 回送るとすべての copy が落ちる確率が
    /// 損失率の n 乗になる。損失の無い経路では品質が変わらない代わりに、音声の帯域と
    /// datagram の数が n 倍になる。4 以上は帯域を無駄にし、0 は音が届かないため 1〜3 に
    /// 限る。subgroup 配送 (`--audio-subgroup`) は Data Stream が再送されるため使わない。
    pub audio_redundancy: u8,
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
    /// publish した Object を保持する時間窓 (ms)
    ///
    /// 保持した Object は subscriber の FETCH で配る
    /// (draft-ietf-moq-transport-22 §3.2 (Fetch))。同じ値を Track Property の
    /// `MAX_CACHE_DURATION` (draft-ietf-moq-transport-22 §10.3) として映像 / 音声の
    /// PUBLISH と SUBSCRIBE_OK で告知する。
    pub cache_duration_ms: u64,
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

/// `--gop-duration` の既定値 (ms)
///
/// フレーム数で指定していた頃の既定 (30 fps × 60 フレーム) と同じ実時間長にする。
pub(crate) const DEFAULT_GOP_DURATION_MS: u32 = 2_000;

/// `--cache-duration` の既定値 (ms)
///
/// publish した Object を保持して FETCH で配れる時間である。subscriber が配信の途中で
/// 参加したときに、直近のキーフレームまで遡れる長さとして 5 秒にする。Group 長の既定
/// (`DEFAULT_GOP_DURATION_MS`) より長くないと、現在の Group を保持できない。
pub(crate) const DEFAULT_CACHE_DURATION_MS: u64 = 5_000;

/// `--audio-redundancy` の既定値 (回)
///
/// datagram は再送されない (RFC 9221 §3) ため、損失のある経路では同じ Object を複数回
/// 送ると届く確率が上がる。既定は 1 回であり、冗長化するかどうかは利用者が選ぶ。
pub(crate) const DEFAULT_AUDIO_REDUNDANCY: u8 = 1;

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

    // `--transport` が省略された場合は `--url` の msf fragment の `connection` を使う
    // (draft-ietf-moq-msf-01 §11.1.1)。解決の規則は `tokio_moq::resolve_transport` を参照
    let transport_opt: Option<Transport> = noargs::opt("transport")
        .ty("TYPE")
        .doc(
            "Transport (quic | wt-h3 | wt-h2; wt-h3 / wt-h2 (WebTransport) are experimental). If \
             --transport is omitted, uses the 'connection' parameter of the --url MSF fragment \
             (q: native QUIC, wt: WebTransport over HTTP/3, or wt-h2 with --transport wt-h2 for \
             HTTP/2); otherwise defaults to quic. A conflicting --transport, or 'connection=q' \
             with 'connection=wt', is an error",
        )
        .take(&mut args)
        .present_and_then(|o| Transport::parse(o.value()))?;
    let transport = tokio_moq::resolve_transport(transport_opt, &url)
        .map_err(|e| noargs::Error::other(&args, e))?;

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

    // `.default()` は `&'static str` しか取れないため、`DEFAULT_GOP_DURATION_MS` と
    // 一致する値を文字列で渡す (一致は単体テストで固定する)
    let gop_duration_opt = noargs::opt("gop-duration")
        .ty("MS")
        .doc(
            "Group duration in milliseconds. A keyframe is forced when this elapses on the input \
             timestamps, so the group length does not depend on fps. Ignored when --input-mp4 is \
             set (groups start at the keyframes in the MP4)",
        )
        .default("2000")
        .take(&mut args);
    let gop_duration_explicit = is_explicit(&gop_duration_opt);
    let gop_duration_ms = gop_duration_opt.then(|o| {
        let value = o.value();
        let ms = value
            .parse::<u32>()
            .map_err(|_| format!("invalid --gop-duration: {value}"))?;
        // 0 ms は Group 長として意味を持たず (毎フレームがキーフレームになる)、
        // エンコーダへ渡すフレーム数の安全弁も 0 フレームになってしまうため拒否する
        if ms == 0 {
            return Err(format!(
                "--gop-duration must be 1 or more milliseconds (default {}): {value}",
                DEFAULT_GOP_DURATION_MS
            ));
        }
        Ok(ms)
    })?;

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
        .doc("Video codec (av1 | h264 | h265 | vp8 | vp9). h264/h265 require macOS, vp8/vp9 are unavailable on Windows")
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

    let audio_codec_opt = noargs::opt("audio-codec")
        .ty("CODEC")
        .doc("Audio codec (opus | aac). aac requires macOS")
        .default("opus")
        .take(&mut args);
    let audio_codec: AudioCodec = audio_codec_opt.then(|o| AudioCodec::parse(o.value()))?;

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

    // 音声は OBJECT_DATAGRAM 配送が既定である。`--audio-subgroup` で subgroup 配送へ
    // 切り替える
    let audio_subgroup: bool = noargs::flag("audio-subgroup")
        .doc("Use subgroup streams for the audio track instead of datagrams")
        .take(&mut args)
        .is_present();
    let audio_datagram = !audio_subgroup;

    // `.default()` は `&'static str` しか取れないため、`DEFAULT_AUDIO_REDUNDANCY` と
    // 一致する値を文字列で渡す (一致は単体テストで固定する)
    let audio_redundancy_opt = noargs::opt("audio-redundancy")
        .ty("N")
        .doc(
            "Number of copies of each audio object sent as datagrams (1-3). Ignored with \
             --audio-subgroup, which uses subgroup streams",
        )
        .default("1")
        .take(&mut args);
    let audio_redundancy_explicit = is_explicit(&audio_redundancy_opt);
    let audio_redundancy = audio_redundancy_opt.then(|o| {
        let value = o.value();
        let count = value
            .parse::<u8>()
            .map_err(|_| format!("invalid --audio-redundancy: {value}"))?;
        // 0 は音が届かず、4 以上は音声の帯域を無駄にする
        if !(1..=3).contains(&count) {
            return Err(format!(
                "--audio-redundancy must be between 1 and 3 (default {}): {value}",
                DEFAULT_AUDIO_REDUNDANCY
            ));
        }
        Ok(count)
    })?;

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

    // `.default()` は `&'static str` しか取れないため、`DEFAULT_CACHE_DURATION_MS` と
    // 一致する値を文字列で渡す (一致は単体テストで固定する)
    let cache_duration_opt = noargs::opt("cache-duration")
        .ty("MS")
        .doc(
            "Duration in milliseconds that published objects are retained and can be served by \
             FETCH. The same value is announced as the MAX_CACHE_DURATION track property",
        )
        .default("5000")
        .take(&mut args);
    let cache_duration_ms = cache_duration_opt.then(|o| {
        let value = o.value();
        value.parse::<u64>().map_err(|_| {
            format!(
                "--cache-duration must be a non-negative integer in milliseconds (default {}): {value}",
                DEFAULT_CACHE_DURATION_MS
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
    if audio_subgroup && !audio_enabled {
        tracing::warn!("--audio-subgroup is ignored because the audio track is not published");
    }
    if datagram_max_size_explicit && (!audio_datagram || !audio_enabled) {
        tracing::warn!(
            "--datagram-max-size is ignored because the audio track is not sent as datagrams"
        );
    }
    // 冗長化は datagram 配送のときだけ意味を持つ。subgroup 配送は Data Stream が再送される
    // ため、同じ Object を 2 回以上送る意味が無い (黙って捨てずに警告する)
    if audio_redundancy_explicit && (!audio_datagram || !audio_enabled) {
        tracing::warn!(
            "--audio-redundancy is ignored because the audio track is not sent as datagrams"
        );
    }
    if !args.metadata().help_mode {
        // トラック名が音声やカタログのトラック名と衝突すると、購読の解決 (track 名 → alias) が
        // 映像側へ倒れ、別のトラックへの購読が映像のキーフレーム要求として扱われる
        if video_enabled && audio_enabled && track_name == AUDIO_TRACK_NAME {
            return Err(noargs::Error::other(
                &args,
                format!(
                    "--track-name must not be {AUDIO_TRACK_NAME} while the audio track is published"
                ),
            ));
        }
        if video_enabled && track_name.as_bytes() == MSF_CATALOG_TRACK_NAME {
            return Err(noargs::Error::other(
                &args,
                format!(
                    "--track-name must not be the catalog track name ({})",
                    String::from_utf8_lossy(MSF_CATALOG_TRACK_NAME)
                ),
            ));
        }
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
            if gop_duration_explicit {
                tracing::warn!(
                    "--gop-duration is ignored when --input-mp4 is set (groups start at the keyframes in the MP4)"
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
            if audio_subgroup {
                tracing::warn!(
                    "--audio-subgroup is ignored when --input-mp4 is set (audio is not published)"
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
        gop_duration_ms,
        namespace,
        track_name,
        fake_capture_device,
        video_codec,
        video_enabled,
        audio_enabled,
        audio_device_id,
        audio_codec,
        audio_bitrate,
        audio_datagram,
        audio_redundancy,
        datagram_max_size,
        target_latency_ms,
        cache_duration_ms,
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

    /// `--url` の `msf` fragment に `connection` が無いときの `--transport` の既定は `quic` である
    ///
    /// WebTransport 経路 (`wt-h3` / `wt-h2`) は experimental であり、既定にしてはいけない
    /// (s2n-quic が `RESET_STREAM_AT` を送出できず、reset を伴う経路が成立しないため)。
    #[test]
    fn transport_defaults_to_quic() {
        let config = parse_args(BASE_ARGS)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            config.transport,
            Transport::Quic,
            "既定は QUIC 直接接続であること"
        );
    }

    /// `--transport` を省略した場合は `--url` の `msf` fragment の `connection` を使う
    ///
    /// draft-ietf-moq-msf-01 §11.1.1 (Reserved fragment parameters) の `connection` は client に
    /// 接続種別を MUST で指定する (`q` は native QUIC、`wt` は WebTransport)。`wt` は WebTransport の
    /// 実装を指定しないため `wt-h3` を既定にする。moq-sub と同じ規則である。
    #[test]
    fn transport_follows_the_msf_fragment_connection() {
        let args = [
            "--url",
            "moqt://127.0.0.1:4443#msf:spam-egg--video&connection=wt",
        ];
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            config.transport,
            Transport::WtH3,
            "connection=wt は WebTransport (既定 wt-h3) になること"
        );

        let args = [
            "--url",
            "moqt://127.0.0.1:4443#msf:spam-egg--video&connection=q",
        ];
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            config.transport,
            Transport::Quic,
            "connection=q は native QUIC になること"
        );
    }

    /// `connection=wt` と整合する `--transport` の明示はそのまま使う
    #[test]
    fn transport_accepts_explicit_webtransport_with_fragment() {
        let args = [
            "--url",
            "moqt://127.0.0.1:4443#msf:spam-egg--video&connection=wt",
            "--transport",
            "wt-h2",
        ];
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            config.transport,
            Transport::WtH2,
            "connection=wt と --transport wt-h2 は整合し、明示が使われること"
        );
    }

    /// `connection` と矛盾する `--transport` はエラーにする
    ///
    /// `connection` は接続種別を MUST で指定するため、`--transport` で上書きできない。
    #[test]
    fn transport_rejects_explicit_conflicting_with_fragment() {
        let args = [
            "--url",
            "moqt://127.0.0.1:4443#msf:spam-egg--video&connection=wt",
            "--transport",
            "quic",
        ];
        let Err(error) = parse_args(&args) else {
            panic!("connection=wt と --transport quic はエラーになること");
        };
        assert!(
            format!("{error:?}").contains("connection=wt"),
            "矛盾した fragment が理由として分かること: {error:?}"
        );

        let args = [
            "--url",
            "moqt://127.0.0.1:4443#msf:spam-egg--video&connection=q",
            "--transport",
            "wt-h3",
        ];
        let Err(error) = parse_args(&args) else {
            panic!("connection=q と --transport wt-h3 はエラーになること");
        };
        assert!(
            format!("{error:?}").contains("connection=q"),
            "矛盾した fragment が理由として分かること: {error:?}"
        );
    }

    /// `connection=q` と `connection=wt` の同時指定はエラーにする
    ///
    /// 2 つの MUST が両立しないため、どちらかを選ぶことはできない。
    #[test]
    fn transport_rejects_conflicting_connections() {
        let args = [
            "--url",
            "moqt://127.0.0.1:4443#msf:spam-egg--video&connection=q&connection=wt",
        ];
        let Err(error) = parse_args(&args) else {
            panic!("q と wt の同時指定はエラーになること");
        };
        assert!(
            format!("{error:?}").contains("cannot be used together"),
            "両立しないことが理由として分かること: {error:?}"
        );
    }

    /// `connection` の不正値はエラーにする
    ///
    /// 値の検証は `MsfFragment::connection_types` に任せる。
    #[test]
    fn transport_rejects_invalid_connection_value() {
        let args = [
            "--url",
            "moqt://127.0.0.1:4443#msf:spam-egg--video&connection=xx",
        ];
        let Err(error) = parse_args(&args) else {
            panic!("connection の不正値はエラーになること");
        };
        assert!(
            format!("{error:?}").contains("invalid connection parameter value"),
            "connection の値の検証エラーとして分かること: {error:?}"
        );
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

    /// `--gop-duration` は既定値を使い、指定で上書き、0 と数値でない値はエラーになる
    ///
    /// 既定の 2000 ms はフレーム数で指定していた頃の既定 (30 fps × 60 フレーム) と同じ
    /// 実時間長であり、fps を変えても Group が同じ長さで進む。
    /// 0 ms は毎フレームがキーフレームになり、フレーム数の安全弁も表現できないため拒否する。
    #[test]
    fn gop_duration_is_configurable() {
        let config = parse_args(BASE_ARGS)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            config.gop_duration_ms, DEFAULT_GOP_DURATION_MS,
            "既定は 2000 ms であること"
        );

        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--gop-duration", "3000"]);
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(config.gop_duration_ms, 3_000, "指定した値が使われること");

        for value in ["0", "1x", "-1"] {
            let mut args = BASE_ARGS.to_vec();
            args.extend_from_slice(&["--gop-duration", value]);
            assert!(
                parse_args(&args).is_err(),
                "0 と数値でない値はエラーになること: {value}"
            );
        }
    }

    /// `--cache-duration` は既定値を使い、指定で上書き、数値でない値はエラーになる
    ///
    /// 既定の 5000 ms は publish した Object を保持して FETCH で配る時間であり、Group 長の
    /// 既定 (2000 ms) より長い。同じ値を Track Property の `MAX_CACHE_DURATION` として告知する
    /// (draft-ietf-moq-transport-22 §10.3)。0 は「保持しない」を意味するため受理する。
    #[test]
    fn cache_duration_is_configurable() {
        let config = parse_args(BASE_ARGS)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            config.cache_duration_ms, DEFAULT_CACHE_DURATION_MS,
            "既定は 5000 ms であること"
        );

        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--cache-duration", "15000"]);
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(config.cache_duration_ms, 15_000, "指定した値が使われること");

        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--cache-duration", "0"]);
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            config.cache_duration_ms, 0,
            "保持しない時間窓も指定できること"
        );

        for value in ["1x", "-1"] {
            let mut args = BASE_ARGS.to_vec();
            args.extend_from_slice(&["--cache-duration", value]);
            assert!(
                parse_args(&args).is_err(),
                "数値でない値と負の値はエラーになること: {value}"
            );
        }
    }

    /// 音声やカタログのトラック名と衝突する `--track-name` はエラーになる
    ///
    /// トラック名が衝突すると、購読の解決が映像側へ倒れて別のトラックの購読が映像として
    /// 扱われるため、起動時に拒否する。
    #[test]
    fn track_name_colliding_with_other_tracks_is_rejected() {
        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--track-name", "audio"]);
        assert!(
            parse_args(&args).is_err(),
            "音声トラックと衝突する名前はエラーになること"
        );

        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--track-name", "audio", "--no-audio"]);
        assert!(
            parse_args(&args).is_ok(),
            "音声トラックを配信しない場合は受理すること"
        );

        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--track-name", "catalog"]);
        assert!(
            parse_args(&args).is_err(),
            "カタログトラックと衝突する名前はエラーになること"
        );
    }

    /// 削除した `--keyframe-interval` は未定義のオプションとしてエラーになる
    ///
    /// Group 長をフレーム数で指定する形に戻さないよう、削除を回帰から守る。
    #[test]
    fn keyframe_interval_is_rejected() {
        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--keyframe-interval", "30"]);
        assert!(
            parse_args(&args).is_err(),
            "削除した --keyframe-interval はエラーになること"
        );
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

    /// 音声は既定で datagram 配送になり、`--audio-subgroup` で subgroup 配送になること
    ///
    /// datagram 配送が既定である (draft-ietf-moq-loc-04 §4.1 (Application with one audio
    /// track) は 1 audio frame = 1 Object = 1 Group と定める)。映像は 1 group = 1
    /// unidirectional stream のままである (§4.2)。
    #[test]
    fn audio_delivery_defaults_to_datagrams() {
        let config = parse_args(BASE_ARGS)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert!(
            config.audio_datagram,
            "既定では音声トラックを datagram で送ること"
        );

        let mut args = BASE_ARGS.to_vec();
        args.push("--audio-subgroup");
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert!(
            !config.audio_datagram,
            "--audio-subgroup で subgroup stream で送ること"
        );
    }

    /// `--audio-redundancy` は既定 1 回で、1〜3 の整数だけを受理すること
    ///
    /// datagram は再送されない (RFC 9221 §3) ため、n 回送るとすべての copy が落ちる確率が
    /// 損失率の n 乗になる。0 は音が届かず、4 以上は音声の帯域を無駄にするため拒否する。
    #[test]
    fn audio_redundancy_is_configurable() {
        let config = parse_args(BASE_ARGS)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            config.audio_redundancy, DEFAULT_AUDIO_REDUNDANCY,
            "既定は 1 回であること"
        );

        for value in ["1", "2", "3"] {
            let mut args = BASE_ARGS.to_vec();
            args.extend_from_slice(&["--audio-redundancy", value]);
            let config = parse_args(&args)
                .expect("オプションの解析に成功すること")
                .expect("設定が返ること");
            assert_eq!(
                config.audio_redundancy.to_string(),
                value,
                "指定した回数が使われること: {value}"
            );
        }

        for value in ["0", "4", "1x", "-1", "2.0"] {
            let mut args = BASE_ARGS.to_vec();
            args.extend_from_slice(&["--audio-redundancy", value]);
            assert!(
                parse_args(&args).is_err(),
                "範囲外と非整数はエラーになること: {value}"
            );
        }
    }

    /// `--audio-subgroup` と併用した `--audio-redundancy` は受理するが冗長化しないこと
    ///
    /// subgroup 配送は Data Stream が再送されるため、同じ Object を 2 回以上送る意味が無い。
    /// 指定そのものはエラーにせず、警告して 1 回だけ送る (値は設定に残るが、送信側は
    /// `--audio-subgroup` では使わない)。
    #[test]
    fn audio_redundancy_is_accepted_with_audio_subgroup() {
        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--audio-subgroup", "--audio-redundancy", "3"]);
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert!(
            !config.audio_datagram,
            "--audio-subgroup で subgroup stream で送ること"
        );
        assert_eq!(
            config.audio_redundancy, 3,
            "subgroup 配送でも指定そのものは受理すること"
        );
    }

    /// `--video-codec` が av1 / h264 / h265 / vp8 / vp9 を受け付けること
    ///
    /// 省略時は av1 である。VP8 / VP9 は Windows 以外で使えるため、macOS 限定の
    /// h264 / h265 と同じ扱いにしてはいけない (Windows での拒否は pipeline が担う)。
    #[test]
    fn video_codec_accepts_vp8_and_vp9() {
        let cases: &[(&[&str], VideoCodec)] = &[
            (&[], VideoCodec::Av1),
            (&["--video-codec", "av1"], VideoCodec::Av1),
            (&["--video-codec", "h264"], VideoCodec::H264),
            (&["--video-codec", "h265"], VideoCodec::H265),
            (&["--video-codec", "vp8"], VideoCodec::Vp8),
            (&["--video-codec", "vp9"], VideoCodec::Vp9),
        ];
        for (extra, expected) in cases {
            let mut args = BASE_ARGS.to_vec();
            args.extend_from_slice(extra);
            let config = parse_args(&args)
                .expect("オプションの解析に成功すること")
                .expect("設定が返ること");
            assert_eq!(
                config.video_codec, *expected,
                "codec が一致すること: {extra:?}"
            );
        }
    }

    /// 未対応の映像コーデックはエラーになること
    #[test]
    fn unknown_video_codec_is_rejected() {
        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--video-codec", "vp10"]);
        let Err(error) = parse_args(&args) else {
            panic!("未対応の codec はエラーになること");
        };
        assert!(
            format!("{error:?}").contains("unknown video codec"),
            "未対応の codec であることが分かること: {error:?}"
        );
    }

    /// `--audio-codec` が opus / aac を受け付けること
    ///
    /// 省略時は opus である。AAC は Apple Audio Toolbox を使うため macOS 限定である。
    #[test]
    fn audio_codec_accepts_opus_and_aac() {
        let cases: &[(&[&str], AudioCodec)] = &[
            (&[], AudioCodec::Opus),
            (&["--audio-codec", "opus"], AudioCodec::Opus),
            (&["--audio-codec", "aac"], AudioCodec::Aac),
        ];
        for (extra, expected) in cases {
            let mut args = BASE_ARGS.to_vec();
            args.extend_from_slice(extra);
            let config = parse_args(&args)
                .expect("オプションの解析に成功すること")
                .expect("設定が返ること");
            assert_eq!(
                config.audio_codec, *expected,
                "codec が一致すること: {extra:?}"
            );
        }
    }

    /// 未対応の音声コーデックはエラーになること
    #[test]
    fn unknown_audio_codec_is_rejected() {
        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--audio-codec", "mp3"]);
        let Err(error) = parse_args(&args) else {
            panic!("未対応の codec はエラーになること");
        };
        assert!(
            format!("{error:?}").contains("unknown audio codec"),
            "未対応の codec であることが分かること: {error:?}"
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
