//! subscriber の CLI 引数定義とパース
//!
//! noargs でオプションを定義し、[`Config`] にまとめる。

use std::time::Duration;

use shiguredo_moqt::message::common::TrackNamespace;
use tokio_moq::multipath::{
    InterfaceFilter, MultipathMode, MultipathOptions, parse_interface_names,
};
use tokio_moq::{ServerUrl, Transport};

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
    ///
    /// 指定すると、その証明書だけをルートとしてサーバー証明書を検証する。省略時は OS の
    /// システムルートストアで検証する。`insecure` と同時には指定できない。
    pub cert: Option<String>,
    /// 証明書検証をスキップする (開発用)
    ///
    /// 接続時に警告ログを出す。`cert` と同時には指定できない。
    pub insecure: bool,
    /// Multipath QUIC (RFC 10069) の設定
    ///
    /// `mode` が `Off` 以外のときだけ Multipath QUIC を広告する。有効にできるのは
    /// `quic-tobi` バックエンドのネイティブ QUIC 経路だけである。
    pub multipath: MultipathOptions,
    /// Track Namespace (draft-ietf-moq-transport-23 §8.8 表現をパースしたもの)
    pub namespace: TrackNamespace,
    /// 映像トラックを受信するかどうか
    pub video_enabled: bool,
    /// 音声トラックを受信するかどうか
    pub audio_enabled: bool,
    /// 音声の出力先
    pub audio_output_device: AudioOutputDevice,
    /// 音声の待ち時間の下限 (ミリ秒。0〜100)
    ///
    /// 到着の揺らぎと実出力の先行量から決まる自動予算の下限である。moqt-js の
    /// `audio.minDelayMs` に対応する。
    pub audio_min_delay_ms: i64,
    /// 受信した映像・音声を保存する MP4 ファイルのパス
    pub mp4: Option<String>,
    /// 再生を行わないかどうか
    pub no_play: bool,
    /// SUBSCRIBE_TRACKS を送るかどうか
    ///
    /// 有効にすると、catalog / 映像 / 音声の購読を確立したあとに namespace prefix の
    /// SUBSCRIBE_TRACKS を送る。publisher が返す PUBLISH は受理するが、受信した Object は
    /// 再生に使わず読み捨てる (既存の catalog 経由の購読は変わらない)。
    pub subscribe_tracks: bool,
    /// SUBSCRIBE に載せる RENDEZVOUS_TIMEOUT (ミリ秒)
    ///
    /// publisher がまだ居ない Track への購読を relay が保持する時間である
    /// (draft-ietf-moq-transport-23 §9.19.6 (RENDEZVOUS TIMEOUT Parameter))。0 は
    /// 「待たない」であり、relay は publisher が居なければ即座に DOES_NOT_EXIST を返す。
    /// この example は既定で 10 秒待ち、publisher の起動が購読より遅れても購読を失わない。
    pub rendezvous_timeout_ms: u64,
}

/// SUBSCRIBE に載せる RENDEZVOUS_TIMEOUT の上限 (ミリ秒)
///
/// 制御メッセージの応答待ち (30 秒) と競合しないよう、上限を 25 秒にする。
const MAX_RENDEZVOUS_TIMEOUT_MS: u64 = 25_000;

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
    args.metadata_mut().app_description = "MOQT subscriber client";

    if noargs::VERSION_FLAG.take(&mut args).is_present() {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return Ok(None);
    }
    noargs::HELP_FLAG.take_help(&mut args);

    let url_value: String = noargs::opt("url")
        .ty("URL")
        .doc("Server URL (moqt://host[:port]/path; '#msf:ns--track&cat=BASE64' sends a C4M token in SETUP)")
        // 必須オプションは help モードでも Opt::None になり `then()` が MissingOpt を返すため、
        // ヘルプ表示のための例を与えて help モードでも先へ進めるようにする (通常の実行では
        // 例は使われず、`--url` の省略は従来どおり MissingOpt になる)
        .example("moqt://relay.example.com:4433/app#msf:moq-example--video")
        .take(&mut args)
        .then(|o| Ok::<_, std::convert::Infallible>(o.value().to_string()))?;
    // noargs の InvalidOpt はオプションの生値を表示するため、URL の検証エラーは
    // Error::other で返し、query / fragment の認可トークンを標準エラーへ反射させない
    if url_value.is_empty() {
        return Err(noargs::Error::other(&args, "--url is required"));
    }
    let url: ServerUrl = tokio_moq::parse_url(&url_value)
        .map_err(|e| noargs::Error::other(&args, format!("--url: {e}")))?;

    // `--transport` が省略された場合は `--url` の msf fragment の `connection` を使う
    // (draft-ietf-moq-msf-02 §11.1.1)。解決の規則は `tokio_moq::resolve_transport` を参照
    //
    // wt-h3 を提供しないビルドではヘルプの選択肢もビルドに合わせる。理由はビルドで異なる
    // (s2n-quic は RESET_STREAM_AT 未実装、Windows は tobi-quic 非対応) ため、Windows を先に
    // 判定する。noargs の doc は &'static str しか取れないため、定数を選ぶ。
    let transport_doc: &'static str = if cfg!(windows) {
        "Transport (quic | wt-h2 | qmux | auto; wt-h3 is not available on Windows because \
         its QUIC layer uses tobi-quic, which does not support Windows. qmux (QMux over \
         TCP+TLS) is experimental; wt-h2 depends on relay support for WT_RESET_STREAM \
         capsules). auto races the available routes with Happy Eyeballs v3 \
         (draft-ietf-happy-happyeyeballs-v3); with 'connection=q' only native QUIC is \
         raced, and with 'connection=wt' the available WebTransport routes are raced. If --transport is \
         omitted, uses the 'connection' parameter of the --url MSF fragment (q: native \
         QUIC; wt requires --transport wt-h2 because wt-h3 is unavailable); otherwise \
         defaults to quic. A conflicting --transport, or 'connection=q' with \
         'connection=wt', is an error"
    } else if tokio_moq::is_wt_h3_available() {
        "Transport (quic | wt-h3 | wt-h2 | qmux | auto; qmux (QMux over TCP+TLS) is \
         experimental; wt-h2 depends on relay support for WT_RESET_STREAM capsules). \
         auto races the available routes with Happy Eyeballs v3 \
         (draft-ietf-happy-happyeyeballs-v3); with 'connection=q' only native QUIC is \
         raced, and with 'connection=wt' the available WebTransport routes are raced. If --transport is \
         omitted, uses the 'connection' parameter of the --url MSF fragment (q: native \
         QUIC, wt: WebTransport over HTTP/3, or wt-h2 with --transport wt-h2 for \
         HTTP/2); otherwise defaults to quic. A conflicting --transport, or \
         'connection=q' with 'connection=wt', is an error"
    } else {
        "Transport (quic | wt-h2 | qmux | auto; wt-h3 is not available with the s2n-quic \
         backend because it requires RESET_STREAM_AT. qmux (QMux over TCP+TLS) is \
         experimental; wt-h2 depends on relay support for WT_RESET_STREAM capsules). \
         auto races the available routes with Happy Eyeballs v3 \
         (draft-ietf-happy-happyeyeballs-v3); with 'connection=q' only native QUIC is \
         raced, and with 'connection=wt' the available WebTransport routes are raced. If --transport is \
         omitted, uses the 'connection' parameter of the --url MSF fragment (q: native \
         QUIC; wt requires --transport wt-h2 because wt-h3 is unavailable); otherwise \
         defaults to quic. A conflicting --transport, or 'connection=q' with \
         'connection=wt', is an error"
    };
    let transport_opt: Option<Transport> = noargs::opt("transport")
        .ty("TYPE")
        .doc(transport_doc)
        .take(&mut args)
        .present_and_then(|o| Transport::parse(o.value()))?;
    let transport = tokio_moq::resolve_transport(transport_opt, &url)
        .map_err(|e| noargs::Error::other(&args, e))?;

    let cert: Option<String> = noargs::opt("cert")
        .ty("PATH")
        .doc("TLS CA certificate path (default: the OS system root store)")
        .take(&mut args)
        .present_and_then(|o| Ok::<_, std::convert::Infallible>(o.value().to_string()))?;
    let insecure: bool = noargs::flag("insecure")
        .doc("Skip TLS certificate verification (development only; cannot be used with --cert)")
        .take(&mut args)
        .is_present();
    if cert.is_some() && insecure {
        return Err(noargs::Error::other(
            &args,
            "--cert and --insecure cannot be used together (specify either a CA certificate or insecure mode)",
        ));
    }

    // Multipath QUIC (RFC 10069) は tobi-quic のネイティブ QUIC 経路だけで使える。
    // 既定は無効であり、--multipath を指定したときだけ有効にする。
    let multipath_mode: MultipathMode = noargs::opt("multipath")
        .ty("MODE")
        .doc("Multipath QUIC mode (off | handover | aggregate | backup; default off; aggregate uses WeightedRoundRobin and BBRv3; requires the quic-tobi backend and --transport quic)")
        .default("off")
        .take(&mut args)
        .then(|o| MultipathMode::parse(o.value()))?;
    let multipath_interface: Option<Vec<String>> = noargs::opt("multipath-interface")
        .ty("NAMES")
        .doc("Comma-separated network interface names to use for automatic multipath path management (default: all interfaces)")
        .take(&mut args)
        .present_and_then(|o| parse_interface_names(o.value()))?;
    let multipath_interface_deny: Option<Vec<String>> = noargs::opt("multipath-interface-deny")
        .ty("NAMES")
        .doc("Comma-separated network interface names to exclude from automatic multipath path management")
        .take(&mut args)
        .present_and_then(|o| parse_interface_names(o.value()))?;
    if multipath_interface.is_some() && multipath_interface_deny.is_some() {
        return Err(noargs::Error::other(
            &args,
            "--multipath-interface and --multipath-interface-deny cannot be used together",
        ));
    }
    let multipath_max_paths: Option<usize> = noargs::opt("multipath-max-paths")
        .ty("N")
        .doc("Maximum number of automatically opened multipath paths (1-64, default 2)")
        .take(&mut args)
        .present_and_then(|o| {
            let value: usize = o
                .value()
                .parse()
                .map_err(|_| format!("--multipath-max-paths must be an integer: {}", o.value()))?;
            if !(1..=64).contains(&value) {
                return Err("--multipath-max-paths must be between 1 and 64".to_string());
            }
            Ok(value)
        })?;
    let multipath_network_monitor_ms: Option<u64> = noargs::opt("multipath-network-monitor-ms")
        .ty("MS")
        .doc("Poll the network interfaces every MS milliseconds and open or close multipath paths automatically (requires --multipath)")
        .take(&mut args)
        .present_and_then(|o| {
            let value: u64 = o.value().parse().map_err(|_| {
                format!(
                    "--multipath-network-monitor-ms must be an integer: {}",
                    o.value()
                )
            })?;
            if value == 0 {
                return Err(
                    "--multipath-network-monitor-ms must be greater than zero".to_string()
                );
            }
            Ok(value)
        })?;
    let multipath_keep_alive_ms: Option<u64> = noargs::opt("multipath-keep-alive-ms")
        .ty("MS")
        .doc("Send keep-alives on multipath standby paths every MS milliseconds (requires --multipath)")
        .take(&mut args)
        .present_and_then(|o| {
            let value: u64 = o.value().parse().map_err(|_| {
                format!(
                    "--multipath-keep-alive-ms must be an integer: {}",
                    o.value()
                )
            })?;
            if value == 0 {
                return Err("--multipath-keep-alive-ms must be greater than zero".to_string());
            }
            Ok(value)
        })?;
    if !multipath_mode.is_enabled()
        && (multipath_interface.is_some()
            || multipath_interface_deny.is_some()
            || multipath_max_paths.is_some()
            || multipath_network_monitor_ms.is_some()
            || multipath_keep_alive_ms.is_some())
    {
        return Err(noargs::Error::other(
            &args,
            "multipath options require --multipath <off|handover|aggregate|backup>",
        ));
    }
    let interface_filter = match (multipath_interface, multipath_interface_deny) {
        (Some(names), None) => InterfaceFilter::Allow(names),
        (None, Some(names)) => InterfaceFilter::Deny(names),
        (None, None) => InterfaceFilter::All,
        (Some(_), Some(_)) => {
            return Err(noargs::Error::other(
                &args,
                "--multipath-interface and --multipath-interface-deny cannot be used together",
            ));
        }
    };
    // 自動で開くパス数の上限に合わせて、Path ID の上限も少なくとも 4 にする。
    // Path ID は接続中に使い回さないため、同時数より小さいと開き直しで枯渇する。
    let max_paths = multipath_max_paths.unwrap_or(2);
    let multipath = MultipathOptions {
        mode: multipath_mode,
        interface_filter,
        max_paths,
        initial_max_path_id: (max_paths as u32).max(4),
        network_monitor_interval: multipath_network_monitor_ms.map(Duration::from_millis),
        keep_alive_interval: multipath_keep_alive_ms.map(Duration::from_millis),
    };
    if multipath.mode.is_enabled() {
        if let Some(reason) = tokio_moq::multipath_unavailable_reason() {
            return Err(noargs::Error::other(&args, reason));
        }
        if transport != Transport::Quic {
            return Err(noargs::Error::other(
                &args,
                "multipath QUIC requires --transport quic (it is not available for WebTransport or QMux)",
            ));
        }
        multipath
            .validate()
            .map_err(|e| noargs::Error::other(&args, e))?;
    }

    // `--namespace` は省略可能で、省略した場合は `--url` の msf fragment
    // (draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation)) の
    // track-identifier が示す namespace を使う。どちらにも無い場合はエラーになる。
    let namespace_opt = noargs::opt("namespace")
        .ty("NS")
        .doc("Track namespace (draft-ietf-moq-transport-23 §8.8 form; '-' separates namespace fields, e.g. moq-example). Defaults to the namespace of the MSF fragment in --url")
        // `.example()` を与えない。noargs は例を持つオプションを必須として Usage に
        // 表示するため、省略可能な --namespace に例を与えると必須に見えてしまう。
        // help モードでは --url の例の MSF fragment から namespace を解決する。
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

    // 音声の待ち時間の下限。0〜100 ms の範囲だけを受け付ける
    let audio_min_delay_ms: i64 = noargs::opt("audio-min-delay-ms")
        .ty("MS")
        .doc("Minimum audio playout delay in milliseconds (0-100, default 50)")
        .default("50")
        .take(&mut args)
        .then(|o| {
            let value: i64 = o
                .value()
                .parse()
                .map_err(|_| format!("--audio-min-delay-ms must be an integer: {}", o.value()))?;
            if !(0..=100).contains(&value) {
                return Err("--audio-min-delay-ms must be between 0 and 100".to_string());
            }
            Ok(value)
        })?;

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

    // SUBSCRIBE_TRACKS は付加的な購読である。既定では送らず、指定したときだけ送る。
    // publisher が返す PUBLISH は受理するが、受信した Object は再生に使わず読み捨てる。
    let subscribe_tracks: bool = noargs::flag("subscribe-tracks")
        .doc("Send SUBSCRIBE_TRACKS for the namespace prefix and accept the resulting PUBLISH messages (received objects are discarded; catalog-based subscriptions are unchanged)")
        .take(&mut args)
        .is_present();

    // publisher がまだ居ない場合に relay へ購読を保持してもらう時間。0 は待たない。
    // 制御メッセージの応答待ち (30 秒) より短い上限にし、制御タイムアウトと競合させない
    let rendezvous_timeout_ms: u64 = noargs::opt("rendezvous-timeout-ms")
        .ty("MS")
        .doc("Rendezvous timeout in milliseconds for SUBSCRIBE (0-25000, default 10000): how long the relay holds a subscription while no publisher is available (draft-ietf-moq-transport-23 Section 9.19.6)")
        .default("10000")
        .take(&mut args)
        .then(|o| {
            let value: u64 = o.value().parse().map_err(|_| {
                format!(
                    "--rendezvous-timeout-ms must be an integer: {}",
                    o.value()
                )
            })?;
            if value > MAX_RENDEZVOUS_TIMEOUT_MS {
                return Err(format!(
                    "--rendezvous-timeout-ms must be between 0 and {MAX_RENDEZVOUS_TIMEOUT_MS}"
                ));
            }
            Ok(value)
        })?;

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
        insecure,
        multipath,
        namespace,
        video_enabled,
        audio_enabled,
        audio_output_device,
        audio_min_delay_ms,
        mp4,
        no_play,
        subscribe_tracks,
        rendezvous_timeout_ms,
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

    /// 音声の待ち時間の下限は既定で 50 ms である
    #[test]
    fn audio_min_delay_defaults_to_50_ms() {
        let config = parse_args(BASE_ARGS)
            .expect("引数を解釈できること")
            .expect("ヘルプではないこと");
        assert_eq!(config.audio_min_delay_ms, 50);
    }

    /// 音声の待ち時間の下限は 0〜100 ms の境界を受け付ける
    #[test]
    fn audio_min_delay_accepts_the_bounds() {
        for value in [0i64, 100] {
            let option = value.to_string();
            let mut args = BASE_ARGS.to_vec();
            args.extend(["--audio-min-delay-ms", &option]);
            let config = parse_args(&args)
                .expect("引数を解釈できること")
                .expect("ヘルプではないこと");
            assert_eq!(config.audio_min_delay_ms, value);
        }
    }

    /// 音声の待ち時間の下限は範囲外と整数でない値を拒否する
    #[test]
    fn audio_min_delay_rejects_invalid_values() {
        for value in ["-1", "101", "1.5", "abc", ""] {
            let mut args = BASE_ARGS.to_vec();
            args.extend(["--audio-min-delay-ms", value]);
            assert!(
                parse_args(&args).is_err(),
                "--audio-min-delay-ms {value} を拒否すること"
            );
        }
    }

    /// `--url` の `msf` fragment に `connection` が無いときの `--transport` の既定は `quic` である
    ///
    /// 接続種別の指定が無いときの既定は native QUIC である。
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
    /// draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters) の `connection` は client に
    /// 接続種別を MUST で指定する (`q` は native QUIC、`wt` は WebTransport)。`wt` は WebTransport の
    /// 実装を指定しないため `wt-h3` を既定にする。ただし `wt-h3` を提供しないビルドでは
    /// `connection=wt` がエラーになる。
    #[test]
    fn transport_follows_the_msf_fragment_connection() {
        let args = [
            "--url",
            "moqt://127.0.0.1:4443#msf:spam-egg--video&connection=wt",
        ];
        if tokio_moq::is_wt_h3_available() {
            let config = parse_args(&args)
                .expect("オプションの解析に成功すること")
                .expect("設定が返ること");
            assert_eq!(
                config.transport,
                Transport::WtH3,
                "connection=wt は WebTransport (既定 wt-h3) になること"
            );
        } else {
            let reason = tokio_moq::wt_h3_unavailable_reason()
                .expect("wt-h3 が利用できないビルドであること");
            let Err(error) = parse_args(&args) else {
                panic!("wt-h3 を提供しないビルドでは connection=wt の既定 wt-h3 を提供しないこと");
            };
            assert!(
                format!("{error:?}").contains(reason),
                "wt-h3 を提供しない理由が分かること: {error:?}"
            );
        }

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

    /// `--transport qmux` は QMux 経路になる
    ///
    /// QMux は MSF fragment の `connection` に対応する値を持たない (native QUIC でも
    /// WebTransport でもない) ため、fragment が接続種別を指定していないときだけ選べる。
    #[test]
    fn transport_accepts_explicit_qmux() {
        let args = [
            "--url",
            "moqt://127.0.0.1:4443#msf:spam-egg--video",
            "--transport",
            "qmux",
        ];
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            config.transport,
            Transport::Qmux,
            "--transport qmux がそのまま使われること"
        );
    }

    /// `--transport auto` は Happy Eyeballs v3 の自動選択になる
    #[test]
    fn transport_accepts_explicit_auto() {
        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--transport", "auto"]);
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            config.transport,
            Transport::Auto,
            "--transport auto がそのまま使われること"
        );
    }

    /// `--transport auto` は `connection` の要求とも両立する
    #[test]
    fn transport_accepts_auto_with_connection_requirement() {
        for connection in ["q", "wt"] {
            let url = format!("moqt://127.0.0.1:4443#msf:spam-egg--video&connection={connection}");
            let args = [
                "--url",
                url.as_str(),
                "--namespace",
                "moq-example",
                "--transport",
                "auto",
            ];
            let config = parse_args(&args)
                .expect("オプションの解析に成功すること")
                .expect("設定が返ること");
            assert_eq!(
                config.transport,
                Transport::Auto,
                "connection={connection} と --transport auto は両立すること"
            );
        }
    }

    /// `connection=wt` と整合する `--transport` の明示はそのまま使う
    ///
    /// `wt` は WebTransport の接続種別であり、実装 (HTTP/3 / HTTP/2) は `--transport` で選べる。
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
        // wt-h3 を提供しないビルドでは wt-h3 自体を提供しないため、
        // fragment との矛盾より先に wt-h3 が利用できない理由を返す
        if tokio_moq::is_wt_h3_available() {
            assert!(
                format!("{error:?}").contains("connection=q"),
                "矛盾した fragment が理由として分かること: {error:?}"
            );
        } else {
            let reason = tokio_moq::wt_h3_unavailable_reason()
                .expect("wt-h3 が利用できないビルドであること");
            assert!(
                format!("{error:?}").contains(reason),
                "wt-h3 を提供しない理由が分かること: {error:?}"
            );
        }
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

    /// `--subscribe-tracks` は既定で無効である
    ///
    /// 付加的な購読であり、指定しない限り publisher に namespace 単位の PUBLISH を
    /// 要求しないことを固定する。
    #[test]
    fn subscribe_tracks_defaults_to_disabled() {
        let config = parse_args(BASE_ARGS)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert!(
            !config.subscribe_tracks,
            "--subscribe-tracks を指定しない場合は無効であること"
        );
    }

    /// `--subscribe-tracks` を指定すると有効になる
    ///
    /// 値を持たないフラグであり、指定だけで有効になることを固定する。
    #[test]
    fn subscribe_tracks_flag_enables_the_request() {
        let mut args = BASE_ARGS.to_vec();
        args.push("--subscribe-tracks");
        let config = parse_args(&args)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert!(
            config.subscribe_tracks,
            "--subscribe-tracks を指定した場合は有効であること"
        );
    }

    /// RENDEZVOUS_TIMEOUT は既定で 10 秒である
    ///
    /// publisher の起動が購読より遅れても relay が購読を保持できるよう、既定で 0 では
    /// なく 10 秒を指定することを固定する。
    #[test]
    fn rendezvous_timeout_defaults_to_10_seconds() {
        let config = parse_args(BASE_ARGS)
            .expect("オプションの解析に成功すること")
            .expect("設定が返ること");
        assert_eq!(
            config.rendezvous_timeout_ms, 10_000,
            "RENDEZVOUS_TIMEOUT の既定値が 10 秒であること"
        );
    }

    /// `--rendezvous-timeout-ms` は 0 以上 25 秒以下を受け付ける
    ///
    /// 0 (待たない) と上限を受理し、上限を超える値は制御メッセージの応答待ちと競合する
    /// ためエラーにすることを固定する。
    #[test]
    fn rendezvous_timeout_parses_the_bounds() {
        for (value, expected) in [("0", 0), ("25000", MAX_RENDEZVOUS_TIMEOUT_MS)] {
            let mut args = BASE_ARGS.to_vec();
            args.push("--rendezvous-timeout-ms");
            args.push(value);
            let config = parse_args(&args)
                .expect("オプションの解析に成功すること")
                .expect("設定が返ること");
            assert_eq!(
                config.rendezvous_timeout_ms, expected,
                "--rendezvous-timeout-ms {value} がそのまま設定されること"
            );
        }
    }

    /// `--rendezvous-timeout-ms` は整数以外と上限超過を拒否する
    ///
    /// 値域外の指定を黙って丸めず、起動時にエラーにすることを固定する。
    #[test]
    fn rendezvous_timeout_rejects_invalid_values() {
        for value in ["-1", "not-a-number", "25001"] {
            let mut args = BASE_ARGS.to_vec();
            args.push("--rendezvous-timeout-ms");
            args.push(value);
            let err = match parse_args(&args) {
                Ok(_) => panic!("--rendezvous-timeout-ms {value} を受理しないこと"),
                Err(err) => err,
            };
            let message = format!("{err:?}");
            assert!(
                message.contains("--rendezvous-timeout-ms"),
                "エラーにオプション名が含まれること: {message}"
            );
        }
    }

    /// Multipath の既定が無効であること
    #[test]
    fn multipath_defaults_to_off() {
        let config = parse_args(BASE_ARGS)
            .expect("パースできること")
            .expect("設定が返ること");
        assert_eq!(config.multipath.mode, MultipathMode::Off);
        assert_eq!(config.multipath.interface_filter, InterfaceFilter::All);
        assert_eq!(config.multipath.max_paths, 2);
        assert_eq!(config.multipath.initial_max_path_id, 4);
        assert!(config.multipath.network_monitor_interval.is_none());
        assert!(config.multipath.keep_alive_interval.is_none());
    }

    /// Multipath の付随オプションはモードの指定を要求すること
    #[test]
    fn multipath_options_require_the_mode() {
        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--multipath-max-paths", "3"]);
        let error = match parse_args(&args) {
            Ok(_) => panic!("--multipath の無い Multipath オプションを受理しないこと"),
            Err(error) => error,
        };
        let message = format!("{error:?}");
        assert!(
            message.contains("require --multipath"),
            "モードの指定が必要であることを示すこと: {message}"
        );
    }

    /// 未知の Multipath モードを拒否すること
    #[test]
    fn multipath_rejects_unknown_modes() {
        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--multipath", "redundant"]);
        let error = match parse_args(&args) {
            Ok(_) => panic!("未知の Multipath モードを受理しないこと"),
            Err(error) => error,
        };
        let message = format!("{error:?}");
        assert!(
            message.contains("off, handover, aggregate, backup"),
            "受け付けるモードの一覧を含めること: {message}"
        );
    }

    /// インターフェースの許可と拒否を同時に指定できないこと
    #[test]
    fn multipath_rejects_allow_and_deny_interfaces_together() {
        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&[
            "--multipath",
            "handover",
            "--multipath-interface",
            "en0",
            "--multipath-interface-deny",
            "wlan0",
        ]);
        let error = match parse_args(&args) {
            Ok(_) => panic!("許可と拒否の同時指定を受理しないこと"),
            Err(error) => error,
        };
        let message = format!("{error:?}");
        assert!(
            message.contains("cannot be used together"),
            "同時指定できないことを示すこと: {message}"
        );
    }

    /// Multipath の各オプションを設定へ反映すること
    #[cfg(feature = "quic-tobi")]
    #[test]
    fn multipath_options_are_parsed() {
        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&[
            "--multipath",
            "aggregate",
            "--multipath-interface",
            "en0, wlan0",
            "--multipath-max-paths",
            "3",
            "--multipath-network-monitor-ms",
            "1000",
            "--multipath-keep-alive-ms",
            "5000",
        ]);
        let config = parse_args(&args)
            .expect("パースできること")
            .expect("設定が返ること");
        assert_eq!(config.multipath.mode, MultipathMode::Aggregate);
        assert_eq!(
            config.multipath.interface_filter,
            InterfaceFilter::Allow(vec!["en0".to_string(), "wlan0".to_string()])
        );
        assert_eq!(config.multipath.max_paths, 3);
        assert_eq!(
            config.multipath.network_monitor_interval,
            Some(Duration::from_millis(1000))
        );
        assert_eq!(
            config.multipath.keep_alive_interval,
            Some(Duration::from_millis(5000))
        );
    }

    /// Multipath は native QUIC 以外の経路では指定できないこと
    #[cfg(feature = "quic-tobi")]
    #[test]
    fn multipath_requires_native_quic() {
        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--multipath", "handover", "--transport", "wt-h2"]);
        let error = match parse_args(&args) {
            Ok(_) => panic!("WebTransport 経路の Multipath を受理しないこと"),
            Err(error) => error,
        };
        let message = format!("{error:?}");
        assert!(
            message.contains("--transport quic"),
            "native QUIC が必要であることを示すこと: {message}"
        );
    }

    /// Multipath を提供しないビルドでは指定できないこと
    #[cfg(not(feature = "quic-tobi"))]
    #[test]
    fn multipath_is_rejected_when_unavailable() {
        let mut args = BASE_ARGS.to_vec();
        args.extend_from_slice(&["--multipath", "handover"]);
        let error = match parse_args(&args) {
            Ok(_) => panic!("Multipath を提供しないビルドで指定を受理しないこと"),
            Err(error) => error,
        };
        let message = format!("{error:?}");
        assert!(
            message.contains("multipath QUIC is not available"),
            "Multipath を利用できないことを示すこと: {message}"
        );
    }
}
