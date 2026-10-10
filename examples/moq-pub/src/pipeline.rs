//! publisher の全体パイプライン
//!
//! カメラ/マイクキャプチャ → 映像/音声エンコード → MoQT 送信の流れを、
//! sans-I/O な `shiguredo_moqt::session::core::Session` を駆動する
//! [`tokio_moq::moqt_client::MoqtClient`] と結線する。

use std::collections::VecDeque;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use shiguredo_audio_device::AudioFrameOwned;
use shiguredo_moqt::{
    audio_clock::{AudioTimestampClock, AudioTimestampOffsetStats},
    error::{
        MessageError, REQUEST_DOES_NOT_EXIST, REQUEST_INVALID_FILTER, REQUEST_INVALID_RANGE,
        REQUEST_NOT_SUPPORTED, SESSION_KEY_VALUE_FORMATTING_ERROR, SESSION_PROTOCOL_VIOLATION,
    },
    loc::{
        LocProperties, LocProperty, LocPropertyValue, PROP_AUDIO_CONFIG, PROP_TIMESCALE,
        PROP_TIMESTAMP, PROP_VIDEO_CONFIG, PROP_VIDEO_FRAME_MARKING,
    },
    media_clock::WallClockMapper,
    message::ControlMessage,
    message::common::Location,
    message_parameter::MessageParameters,
    msf::MSF_CATALOG_TRACK_NAME,
    name::serialize_namespace,
    session::types::DataStreamId,
    session::types::SessionEvent,
    track_properties::TrackProperties,
};
use shiguredo_video_device::VideoFrameOwned;
use tokio::sync::mpsc;

use crate::audio_capture;
use crate::capture;
use crate::catalog;
use crate::cli::{Config, VideoCodec};
use crate::datagram_writer::DatagramWriter;
use crate::encoder::opus::OpusEncoder;
use crate::encoder::{self, EncodedFrame};
use crate::error::{Error, Result};
use crate::fake_audio_capture;
use crate::fake_capture;
use crate::group_id;
use crate::mp4;
use crate::stream_writer::SubgroupWriter;
use tokio_moq::Transport;
use tokio_moq::connect_with_fallback;
use tokio_moq::error::TransportError;
use tokio_moq::host_from_authority;
use tokio_moq::moqt_client::{ClientEvent, IncomingRequest, MoqtClient, ObjectFilterOutcome};
use tokio_moq::quic;

/// 映像キャプチャの寿命管理
///
/// フィールドは読み出さないが、この値が drop されるまでキャプチャを保持する。
#[expect(dead_code, reason = "キャプチャを drop まで保持するため")]
enum VideoCaptureGuard {
    Real(shiguredo_video_device::VideoCapture),
    Fake(fake_capture::FakeCapture),
}

/// 音声キャプチャの寿命管理
///
/// フィールドは読み出さないが、この値が drop されるまでキャプチャを保持する。
#[expect(dead_code, reason = "キャプチャを drop まで保持するため")]
enum AudioCaptureGuard {
    Real(shiguredo_audio_device::AudioCapture),
    Fake(fake_audio_capture::FakeAudioCapture),
}

/// パイプラインへ渡す映像入力
///
/// カメラ / 疑似キャプチャは生フレームを渡し、MP4 パススルーはエンコード済みサンプルを、
/// MP4 再エンコードは入力のメディア時刻付きの生フレームを渡す。
pub(crate) enum VideoInput {
    /// キャプチャした生フレーム (エンコード前)
    Raw(VideoFrameOwned),
    /// エンコード済みサンプル (MP4 パススルー)
    Encoded(EncodedFrame),
    /// デコード済みの生フレーム (MP4 再エンコード)
    Reencode {
        /// 生フレーム
        frame: VideoFrameOwned,
        /// 入力トラックの timescale 単位のタイムスタンプ (PTS)
        timestamp: u64,
    },
}

/// パイプラインへ渡す音声入力
///
/// カメラ / 疑似キャプチャは生フレームを渡し、MP4 再エンコードは入力のメディア時刻付きの
/// PCM フレームを渡す。
pub(crate) enum AudioInput {
    /// キャプチャした音声フレーム
    Raw(AudioFrameOwned),
    /// デコード済みの PCM フレーム (MP4 再エンコード)
    Reencode {
        /// PCM フレーム
        frame: AudioFrameOwned,
        /// 入力トラックの timescale 単位のタイムスタンプ
        timestamp: u64,
    },
}

/// カタログトラックの Track Alias
const CATALOG_TRACK_ALIAS: u64 = 0;
/// 映像トラックの Track Alias
const VIDEO_TRACK_ALIAS: u64 = 1;
/// 音声トラックの Track Alias
const AUDIO_TRACK_ALIAS: u64 = 2;
/// デフォルトの Publisher Priority
const DEFAULT_PUBLISHER_PRIORITY: u8 = 128;
/// PUBLISH_DONE の Status Code: GOING_AWAY (draft-ietf-moq-transport-22 §16.11.3 (PUBLISH_DONE Codes))
/// シャットダウン時は GOAWAY 送出に先立ち各 subscription を GOING_AWAY で終了する。
const PUBLISH_DONE_GOING_AWAY: u64 = 0x4;
/// 音声サンプリングレート (Hz)
const AUDIO_SAMPLE_RATE: u32 = 48_000;
/// 音声チャンネル数 (mono)
const AUDIO_CHANNELS: u8 = 1;
/// MSF channelConfig (mono)
///
/// draft-ietf-moq-msf-01 §5.2.29 (Channel configuration) では string 表現。一次資料の例では "2" を使用。
const AUDIO_CHANNEL_CONFIG: &str = "1";
/// 音声トラック名
const AUDIO_TRACK_NAME: &str = "audio";
/// カタログのレンダーグループ
///
/// 音声と映像を同時に再生させるため、両トラックに同じ値を載せる
/// (draft-ietf-moq-msf-01 §5.2.11 (Render group))。
const CATALOG_RENDER_GROUP: u64 = 1;

/// カタログを送り直す間隔
///
/// draft-ietf-moq-msf-01 §5 (Catalog) は、配信網の cache から落ちうる時間が過ぎたときに
/// カタログを publish し直すことを SHOULD とする。後から購読を始めた相手は relay の cache に
/// 残っているカタログを FETCH で取得するため、cache から落ちる前に新しい Group で送り直す。
/// relay の cache が既定で保持する時間 (10 分程度) より十分短く、かつ送り直しの負荷が
/// 問題にならない 30 秒にする。
const CATALOG_REPUBLISH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// 停滞の切り分け用の診断ログを出すかどうか。
///
/// `MOQT_PUBLISH_DIAG=1` を設定したときだけ有効にする。100ms ごとに
/// publish ループの進み具合を出すため、常時有効にすると計測そのものが結果を歪める。
fn publish_diag_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("MOQT_PUBLISH_DIAG").is_ok_and(|v| v == "1"))
}

/// Session が指示した reset の対象が、この example が開いたままの subgroup stream かを返す
///
/// Session は delivery timeout による reset で stream 追跡を残し、I/O 層からの
/// `send_data_stream_closed` を待つ。一方で app 発の reset (`reset_outgoing_data_stream_*`)
/// や fill fetch stream の reset、malformed 検出では追跡を除去済みであり、通知すると
/// 未知 stream id でセッションを閉じてしまう。追跡が残る経路と除去済みの経路は
/// イベントから区別できないため、「自分が開いたままの subgroup stream に一致するか」で
/// 通知の要否を決める。
fn reset_targets_open_subgroup(
    current_video_writer: Option<&SubgroupWriter>,
    stream_id: DataStreamId,
) -> bool {
    current_video_writer.is_some_and(|writer| writer.stream_id() == stream_id)
}

/// Session が指示した data stream の reset を I/O 層として実施する
///
/// この example が保持する subgroup stream のうち `stream_id` に一致するものを
/// RESET_STREAM で閉じ、Session へ終端を通知する。一致しない場合 (既に閉じた stream や
/// fill fetch stream) は何もしない。Session 側で追跡を除去済みの reset 経路では
/// `send_data_stream_closed` が未知 stream id でセッションを閉じるため、
/// 自分が開いたままの stream にだけ通知する。
///
/// 戻り値は「publish ループを抜けるか」。transport の終了 (`ConnectionClosed`) は
/// セッション終了として扱い、それ以外のエラーは呼び出し元へ伝播させる。
fn reset_open_subgroup_stream(
    current_video_writer: &mut Option<SubgroupWriter>,
    stream_id: DataStreamId,
    error_code: u64,
) -> Result<bool> {
    // Session 側で追跡を除去済みの reset 経路 (app 発の reset / fill fetch stream /
    // malformed 検出) では send_data_stream_closed が未知 stream id でセッションを閉じる。
    // 自分が開いたままの stream にだけ通知する
    let applies = reset_targets_open_subgroup(current_video_writer.as_ref(), stream_id);
    if !applies {
        tracing::debug!(
            "ResetDataStream is not for an open subgroup stream of this example: stream_id={}",
            stream_id.0
        );
        return Ok(false);
    }
    let writer = current_video_writer
        .take()
        .expect("applies is true only when an open subgroup stream matches");
    match writer.reset_by_session(error_code) {
        Ok(()) => {
            tracing::info!(
                "Reset subgroup stream on session request: stream_id={}, error_code={:#x}",
                stream_id.0,
                error_code
            );
            Ok(false)
        }
        Err(e) if is_transport_session_end(&e) => {
            tracing::info!("Session closed by transport");
            Ok(true)
        }
        Err(e) if is_peer_stream_reset(&e) => {
            // peer が既に当該ストリームを cancel している。該当ストリームは終端済みであり
            // セッションは継続するため、後始末を続ける
            tracing::info!("Subgroup stream was already reset by peer");
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// パイプラインを実行する
///
/// `config` は接続先・トランスポート・送信するトラック・入力の指定を持つ。
/// `task_monitor` は生成されるタスクのメトリクスを収集する。
/// `shutdown_monitor` は graceful shutdown の契機を受信する。
pub async fn run(
    mut config: Config,
    task_monitor: tokio_metrics::TaskMonitor,
    mut shutdown_monitor: tokio_utils::ShutdownMonitor,
) -> Result<()> {
    // MP4 パススルーは接続前にファイルを読み込んで検証する
    // (不正な入力を relay の接続可否に依存せず報告するため)
    let mp4_reader: Option<mp4::Mp4VideoReader> = match config.input_mp4.as_deref() {
        Some(path) => Some(mp4::Mp4VideoReader::open(path)?),
        None => None,
    };
    // MP4 再エンコードも接続前にファイルを読み込んで検証する
    let reencode_reader: Option<mp4::reencode::Mp4ReencodeReader> =
        match config.input_mp4_reencode.as_deref() {
            Some(path) => Some(mp4::reencode::Mp4ReencodeReader::open(
                path,
                config.video_enabled,
                config.audio_enabled,
            )?),
            None => None,
        };
    // 再エンコードで映像を配信しない場合、配信できるトラックが無ければエラーにする
    if config.input_mp4_reencode.is_some()
        && let Some(reader) = reencode_reader.as_ref()
        && reader.audio_info().is_none()
    {
        if !config.video_enabled {
            return Err(Error::Other(
                "MP4 file has no publishable track (video is disabled and no supported audio track)"
                    .to_string(),
            ));
        }
        // Opus 以外の音声トラックは警告済みのため、catalog に audio を載せない
        config.audio_enabled = false;
    }

    // TLS の SNI / 証明書検証に使う server_name はポートを含めない。
    // IPv6 リテラル (`[::1]:4443` 等) でも正しく host を取り出す。
    let server_name = host_from_authority(&config.url.authority);

    // 接続先の解決は QUIC / WebTransport で共通にする。ポートが省略された URL は
    // 既定ポート 443 を使い、ホスト名は名前解決する (draft-ietf-moq-transport-22 §6.1.2)。
    // 解決結果が複数の場合は順に試し、確立できたアドレスを採用する。
    // 確立後の SETUP 以降の失敗はアドレスに依存しないため、次のアドレスは試さない。
    // `socket_addr` は試行ごとに変わるため、試行の中で接続先を組み立てる。

    // WebTransport 経路は experimental である (s2n-quic が `RESET_STREAM_AT` を送出できず、
    // WebTransport が MUST とする Reliable Size 付きの reset を行えないため)。接続の前に 1 度だけ
    // 警告し、どのトランスポートで接続するかを実行時に分かるようにする。接続先の試行ごとに
    // 出さないよう `connect_with_fallback` の外側で判定する。
    if matches!(config.transport, Transport::WtH3 | Transport::WtH2) {
        tracing::warn!(
            "WebTransport transport ({:?}) is experimental: RESET_STREAM_AT is unavailable, so stream resets may not work",
            config.transport
        );
    }

    // 1. 接続確立と SETUP ハンドシェイク
    let connect_monitor = task_monitor.clone();
    let connection_config = config.clone();
    let mut client = connect_with_fallback(&config.url.authority, move |socket_addr| {
        // 試行ごとに接続を組み立てるため、設定は複製して `move` で取り込む
        let cfg = connection_config.clone();
        let connect_monitor = connect_monitor.clone();
        let authority_url = cfg.url.authority.clone();
        async move {
            let client = match cfg.transport {
                Transport::Quic => {
                    let (connection, stop_sending_rx) =
                        quic::connect(socket_addr, server_name, cfg.cert.as_deref()).await?;
                    let (client, _acceptor) = MoqtClient::establish_quic(
                        connection,
                        stop_sending_rx,
                        &cfg.url.path,
                        &authority_url,
                        "moq-pub",
                        &cfg.url.c4m_tokens,
                        &connect_monitor,
                    )
                    .await?;
                    client
                }
                Transport::WtH3 => {
                    let mut client_config =
                        tokio_moq::webtransport_h3::ClientConfig::new(socket_addr, server_name)
                            // :authority は target URI の authority を URL の表記どおりに渡す (draft-ietf-webtrans-http3-16 §3.2)
                            .authority(&authority_url)
                            .enable_webtransport(
                                shiguredo_http3::webtransport::Settings::new()
                                    .wt_enabled(shiguredo_http3::VarInt::from_static(1)),
                            );
                    if let Some(ref cert) = cfg.cert {
                        let pem = std::fs::read_to_string(cert)?;
                        client_config = client_config.ca_cert(pem);
                    } else {
                        // 開発用: 証明書検証をスキップする (QUIC 経路と対称の警告)
                        tracing::warn!(
                            "TLS certificate verification is disabled (development mode)"
                        );
                        client_config = client_config.insecure();
                    }
                    let wt_session =
                        tokio_moq::webtransport_h3::WtClient::connect(client_config, &cfg.url.path)
                            .await?;
                    let (client, _acceptor) = MoqtClient::establish_wt(
                        wt_session,
                        "moq-pub",
                        &cfg.url.c4m_tokens,
                        &connect_monitor,
                    )
                    .await?;
                    client
                }
                Transport::WtH2 => {
                    let mut client_config =
                        tokio_moq::webtransport_h2::ClientConfig::new(socket_addr, server_name)
                            // :authority は target URI の authority を URL の表記どおりに渡す (draft-ietf-webtrans-http2-15 §3.2)
                            .authority(&authority_url);
                    if let Some(ref cert) = cfg.cert {
                        let pem = std::fs::read_to_string(cert)?;
                        client_config = client_config.ca_cert(pem);
                    } else {
                        // 開発用: 証明書検証をスキップする (QUIC 経路と対称の警告)
                        tracing::warn!(
                            "TLS certificate verification is disabled (development mode)"
                        );
                        client_config = client_config.insecure();
                    }
                    let wt_session = tokio_moq::webtransport_h2::WtH2Client::connect(
                        client_config,
                        &cfg.url.path,
                    )
                    .await?;
                    let (client, _acceptor) = MoqtClient::establish_wt_h2(
                        wt_session,
                        "moq-pub",
                        &cfg.url.c4m_tokens,
                        &connect_monitor,
                    )
                    .await?;
                    client
                }
            };
            Ok::<_, Error>(client)
        }
    })
    .await?;

    // 2. タイムアウト設定
    client.set_control_message_timeout_ms(Some(30000));
    client.set_data_stream_timeout_ms(Some(30000));
    tracing::info!(
        "Session timeouts: control={:?}ms, data={:?}ms",
        client.control_message_timeout_ms(),
        client.data_stream_timeout_ms(),
    );

    // 3. PUBLISH (video / audio / catalog)
    let data_plane = client.data_plane();
    // CLI で §8.8 表現としてパース済みの namespace をそのまま使う
    let namespace = config.namespace.clone();
    // カタログの namespace は §8.8 の表現で書く (draft-ietf-moq-msf-01 §5.2.2 (Track namespace))。
    // PUBLISH は `namespace` を move するため、先に文字列を作っておく
    let namespace_text = serialize_namespace(&namespace);
    let video_request_id = if config.video_enabled {
        Some(
            client
                .publish_track(
                    namespace.clone(),
                    config.track_name.as_bytes().to_vec(),
                    VIDEO_TRACK_ALIAS,
                )
                .await?,
        )
    } else {
        None
    };
    let audio_request_id = if config.audio_enabled {
        Some(
            client
                .publish_track(
                    namespace.clone(),
                    AUDIO_TRACK_NAME.as_bytes().to_vec(),
                    AUDIO_TRACK_ALIAS,
                )
                .await?,
        )
    } else {
        None
    };
    let catalog_request_id = client
        .publish_track(
            namespace,
            MSF_CATALOG_TRACK_NAME.to_vec(),
            CATALOG_TRACK_ALIAS,
        )
        .await?;

    // 4. エンコーダを先に生成し、catalog の codec 文字列を encoder から取得する
    // --input-mp4 の場合はエンコーダを使わず、MP4 の映像トラック情報を catalog に使う
    // --input-mp4-reencode の場合は解像度とフレームレートを MP4 から自動検出する
    let reencode_video_info = reencode_reader.as_ref().and_then(|r| r.video_info());
    let (encode_width, encode_height, encode_fps) = match &reencode_video_info {
        Some(info) => (info.width, info.height, info.fps),
        None => (config.width, config.height, config.fps),
    };
    let mut video_encoder: Option<encoder::VideoEncoder> = if config.video_enabled
        && mp4_reader.is_none()
    {
        let enc = match config.video_codec {
            VideoCodec::Av1 => encoder::VideoEncoder::Av1(Box::new(encoder::av1::Av1Encoder::new(
                encode_width,
                encode_height,
                encode_fps,
                config.bitrate,
                config.keyframe_interval,
            )?)),
            #[cfg(target_os = "macos")]
            VideoCodec::H264 => encoder::VideoEncoder::H264(encoder::h264::H264Encoder::new(
                encode_width,
                encode_height,
                encode_fps,
                config.bitrate,
                config.keyframe_interval,
            )?),
            #[cfg(target_os = "macos")]
            VideoCodec::H265 => encoder::VideoEncoder::H265(encoder::h265::H265Encoder::new(
                encode_width,
                encode_height,
                encode_fps,
                config.bitrate,
                config.keyframe_interval,
            )?),
            #[cfg(not(target_os = "macos"))]
            VideoCodec::H264 => {
                return Err(unsupported_macos_encoder("H.264"));
            }
            #[cfg(not(target_os = "macos"))]
            VideoCodec::H265 => {
                return Err(unsupported_macos_encoder("H.265"));
            }
        };
        Some(enc)
    } else {
        None
    };
    // PROP_TIMESCALE を載せる場合の値。MP4 パススルーでは MP4 のタイムスケールを、
    // 再エンコードでは入力映像トラックのタイムスケールを使う。live capture は
    // Unix epoch マイクロ秒 (LOC の既定) を送るため載せない (`None`)。
    let video_timescale: Option<u64> = match (&mp4_reader, &reencode_video_info) {
        (Some(reader), _) => Some(reader.info().timescale),
        (None, Some(info)) => Some(info.timescale),
        (None, None) => None,
    };
    // 再エンコードの入力映像トラックのタイムスケール。デコード済みフレームの PTS を
    // マイクロ秒へ換算するために使う。
    let reencode_video_timescale: Option<u64> =
        reencode_video_info.as_ref().map(|info| info.timescale);

    let mut audio_encoder: Option<OpusEncoder> = if config.audio_enabled {
        Some(OpusEncoder::new(
            AUDIO_SAMPLE_RATE,
            AUDIO_CHANNELS,
            config.audio_bitrate * 1000,
        )?)
    } else {
        None
    };
    let audio_samples_per_frame = audio_encoder.as_ref().map(|e| e.samples_per_frame());
    // PROP_TIMESCALE は再エンコードでは入力音声トラックのタイムスケールを使う。
    // live capture は Unix epoch マイクロ秒 (LOC の既定) を送るため載せない (`None`)。
    let audio_timescale: Option<u64> = if config.audio_enabled {
        reencode_reader
            .as_ref()
            .and_then(|r| r.audio_info())
            .map(|info| info.timescale)
    } else {
        None
    };
    // Opus の configuration bytes (OpusHead) を事前に構築する
    // Audio Config プロパティとして最初の audio object にだけ付与する
    let audio_opus_head = audio_encoder
        .as_ref()
        .map(|_| build_opus_head(AUDIO_SAMPLE_RATE, AUDIO_CHANNELS));

    // 5. MSF カタログを送信する
    //
    // カタログの Group ID は 0 に固定しない。draft-ietf-moq-msf-01 §6.1 (Group numbering) は
    // Track の Group ID を一意で単調増加とする MUST を課し、publisher の再起動時には
    // 以前 publish したどの Group ID よりも大きい値から始める MUST を課す。同じ Track 名で
    // 配信し直しても Group ID が衝突しないよう、Unix epoch ミリ秒から払い出す。
    let handle = client.handle();
    let mut catalog_group_id = group_id::allocate_initial_group_id()?;
    // カタログの Location。FETCH 応答と fill fetch stream の範囲判定、SUBSCRIBE_OK の
    // LARGEST_OBJECT に使う
    let mut catalog_location = Location {
        group_id: catalog_group_id,
        object_id: 0,
    };
    let catalog_json = catalog::send_catalog(catalog::CatalogParams {
        handle: &handle,
        data_plane: &data_plane,
        catalog_request_id,
        catalog_alias: CATALOG_TRACK_ALIAS,
        group_id: catalog_group_id,
        start_location: client.subscription_filter_start(catalog_request_id),
        // 音声と映像を同じ時間軸で再生するため、両トラックへ同じ値を載せる
        sync: catalog::CatalogSyncParams {
            render_group: CATALOG_RENDER_GROUP,
            target_latency_ms: u64::from(config.target_latency_ms),
        },
        // `c4m` が指定された配信は、映像 / 音声の track に authInfo を載せる (§5.2.42)
        c4m_tokens: &config.url.c4m_tokens,
        video: match (&mp4_reader, video_encoder.as_ref()) {
            (Some(reader), _) => {
                let info = reader.info();
                Some(catalog::VideoTrackParams {
                    track_name: &config.track_name,
                    namespace: &namespace_text,
                    codec: &info.codec,
                    width: info.width,
                    height: info.height,
                    fps: info.fps,
                    bitrate: info.bitrate_kbps,
                })
            }
            (None, Some(enc)) => Some(catalog::VideoTrackParams {
                track_name: &config.track_name,
                namespace: &namespace_text,
                codec: enc.catalog_codec_string(),
                width: encode_width,
                height: encode_height,
                fps: encode_fps,
                bitrate: config.bitrate,
            }),
            (None, None) => None,
        },
        audio: audio_encoder.as_ref().map(|enc| catalog::AudioTrackParams {
            track_name: AUDIO_TRACK_NAME,
            namespace: &namespace_text,
            codec: enc.catalog_codec_string(),
            samplerate: AUDIO_SAMPLE_RATE,
            channel_config: AUDIO_CHANNEL_CONFIG,
            bitrate: config.audio_bitrate,
        }),
    })
    .await?;

    // 6. 映像 / 音声入力を起動する
    //
    // MP4 リーダーの停止は Drop で行う。Drop は宣言と逆順に実行されるため、
    // 受信側 (video_input_rx / audio_input_rx) を先に閉じてから join できるよう
    // リーダーの生存管理を先に宣言する。
    let mut mp4_source: Option<mp4::ReaderGuard> = None;
    let mut reencode_source: Option<mp4::ReaderGuard> = None;
    let (video_input_tx, mut video_input_rx) = mpsc::channel::<VideoInput>(4);
    let (audio_input_tx, mut audio_input_rx) = mpsc::channel::<AudioInput>(8);
    let _video_capture: Option<VideoCaptureGuard>;
    let _audio_capture: Option<AudioCaptureGuard>;
    if let Some(reader) = reencode_reader {
        // MP4 再エンコード: リーダーが映像と音声の両方を供給する
        let video_input_tx = config.video_enabled.then_some(video_input_tx);
        let audio_input_tx = config.audio_enabled.then_some(audio_input_tx);
        reencode_source = Some(reader.start(video_input_tx, audio_input_tx)?);
        _video_capture = None;
        _audio_capture = None;
    } else {
        _video_capture = if config.video_enabled {
            match mp4_reader {
                Some(reader) => {
                    mp4_source = Some(reader.start(video_input_tx)?);
                    None
                }
                None => Some(if config.fake_capture_device {
                    VideoCaptureGuard::Fake(fake_capture::start_capture(&config, video_input_tx)?)
                } else {
                    VideoCaptureGuard::Real(capture::start_capture(&config, video_input_tx)?)
                }),
            }
        } else {
            // 送信側を drop して受信側を閉じておく (select の分岐は cfg gate でスキップ)
            drop(video_input_tx);
            None
        };

        _audio_capture = if config.audio_enabled {
            Some(if config.fake_capture_device {
                AudioCaptureGuard::Fake(fake_audio_capture::start_capture(audio_input_tx)?)
            } else {
                AudioCaptureGuard::Real(audio_capture::start_capture(&config, audio_input_tx)?)
            })
        } else {
            drop(audio_input_tx);
            None
        };
    };

    // 7. データループ
    let start = Instant::now();
    // 映像 / 音声の Group ID も 0 に固定しない (draft-ietf-moq-msf-01 §6.1 (Group numbering))。
    // 再起動時に以前の Group ID を下回らないよう、カタログと同じ規則で払い出す。
    let mut video_group_id: u64 = group_id::allocate_initial_group_id()?;
    let mut audio_group_id: u64 = group_id::allocate_initial_group_id()?;
    let mut audio_frame_count: u64 = 0;
    let mut current_video_writer: Option<SubgroupWriter> = None;
    // セッション終了を観測してループを抜けたかどうか。後始末の失敗をセッション終了に伴う
    // 失敗として扱うために使う (セッション終了後の `close` は CONNECT stream の送信失敗として
    // `Error::WebTransport` に畳まれる経路があり、エラーの variant だけでは判別できない)
    let mut session_ended = false;
    let mut audio_pcm_buf: Vec<i16> = Vec::new();
    // live capture のメディア時刻を epoch マイクロ秒へ換算する。capture の時刻の起源は
    // 音声と映像 (さらにデバイスと OS) で異なるため、それぞれ別に持つ。映像は対応を小さく
    // する向きにだけ動かす `WallClockMapper`、音声はドリフトへ窓を滑らせて追従し段差では
    // 取り直す `AudioTimestampClock` を使う
    let mut video_clock = WallClockMapper::new();
    let mut audio_clock = AudioTimestampClock::new();
    // 換算の残差 (受信時刻 − 換算した TIMESTAMP) をトラックごとに溜め、定期的に要約を出す
    let mut capture_residuals = CaptureClockResiduals::new();
    // live capture の音声バッファ先頭の時刻。1 つの capture フレームから複数の Opus
    // フレームを切り出すため、切り出し位置の時刻をサンプル数から進める。
    let mut audio_timeline = AudioCaptureTimeline::new();
    // 再エンコードで入力サンプルの PTS を出力フレームへ対応付ける待ち行列
    let mut pending_video_timestamps: VecDeque<u64> = VecDeque::new();
    // Audio Config (OpusHead) を送信済みかどうかのフラグ
    // Opus は全フレームが独立してデコード可能なため、最初の audio object だけでよい
    let mut audio_config_sent = false;
    let mut tick_interval = tokio::time::interval(std::time::Duration::from_millis(100));
    tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // カタログの送り直し。開始直後の 1 回は送信済みなので、1 間隔ぶん待ってから始める
    let mut catalog_republish_interval = tokio::time::interval_at(
        tokio::time::Instant::now() + CATALOG_REPUBLISH_INTERVAL,
        CATALOG_REPUBLISH_INTERVAL,
    );
    catalog_republish_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    tracing::info!(
        "Starting publish loop (video={}, audio={}, video_delivery=subgroup, audio_delivery={})",
        config.video_enabled,
        config.audio_enabled,
        if config.audio_datagram {
            "datagram"
        } else {
            "subgroup"
        },
    );

    'main: loop {
        tokio::select! {
            video_input = video_input_rx.recv(), if config.video_enabled => {
                let Some(video_input) = video_input else {
                    tracing::info!("Video input channel closed");
                    break;
                };
                // エンコードとフレーム送信を async ブロックに閉じ込め、`?` が `run` を
                // 抜けないようにする。フレーム送信経路 (`SubgroupWriter::new` /
                // `SubgroupWriter::write_object` / `DatagramWriter::write_object` など) は
                // セッション終了後に `TransportError::ConnectionClosed` を返すため、`?` を
                // `run` まで通すと終了時の後始末 (`PUBLISH_DONE` / `close(0, "")`) に到達しない。
                // ループを抜ける制御 (`let Some(..) = .. else { break }`) はブロックの
                // 外に出す (ブロックの中では外側のループを抜けられない)。
                let outcome: Result<()> = async {
                    // カメラ / 疑似キャプチャは生フレームをエンコードし、MP4 パススルーは
                    // エンコード済みサンプルをそのまま送り、MP4 再エンコードはデコード済みの
                    // 生フレームをエンコードする
                    // `output_timescale` は、encoder が付けた Timestamp がマイクロ秒である
                    // ことを示す。入力より出力が多いときに、その値を入力の単位へ戻すために使う
                    let (mut encoded_frames, input_timestamp, output_timescale) = match video_input {
                        VideoInput::Raw(frame) => {
                            let encoder = video_encoder.as_mut().expect("video encoder enabled");
                            // live capture の Timestamp は capture のメディア時刻を epoch
                            // マイクロ秒へ換算する。起源はカメラとマイクで異なるため、
                            // 換算は映像と音声で別々の時計が持つ
                            let timestamp_us = map_capture_timestamp_us(
                                &mut video_clock,
                                &mut capture_residuals,
                                frame.timestamp_us,
                                wall_clock_now_us()?,
                            )?;
                            (encoder.encode(&frame, timestamp_us)?, None, None)
                        }
                        VideoInput::Encoded(frame) => (vec![frame], None, None),
                        VideoInput::Reencode { frame, timestamp } => {
                            let encoder = video_encoder.as_mut().expect("video encoder enabled");
                            // 入力サンプルの PTS (入力トラックの timescale 単位) をマイクロ秒へ
                            // 換算して encoder へ渡す。encoder が載せた値は下の
                            // `assign_input_timestamps` が入力の PTS で上書きするため、
                            // 出力の Timestamp は入力トラックの timescale のままになる
                            let timescale = reencode_video_timescale
                                .expect("re-encode video input has a timescale");
                            let media_us = media_time_to_micros(timestamp, timescale);
                            (
                                encoder.encode(&frame, media_us)?,
                                Some(timestamp),
                                Some(timescale),
                            )
                        }
                    };
                    // 再エンコードでは出力フレームのタイムスタンプに入力サンプルの PTS を
                    // 使う。0 フレームを返した入力の PTS は次に出力されたフレームへ引き継ぐ
                    assign_input_timestamps(
                        &mut pending_video_timestamps,
                        input_timestamp,
                        output_timescale,
                        &mut encoded_frames,
                    );
                    for ef in encoded_frames {
                        if ef.is_keyframe {
                            // writer の作成と終端で同じ値を使うため、ここで request_id を
                            // 確定させる (`.expect()` の重複も避ける)
                            let video_request_id =
                                video_request_id.expect("video request_id enabled");
                            // 映像は 1 group = 1 unidirectional stream で送る
                            // (draft-ietf-moq-loc-04 §4.2)。datagram 配送は音声トラックだけに
                            // 指定でき、キーフレーム (IDR) が 1 QUIC datagram に収まらない
                            // 映像では使わない。
                            if let Some(writer) = current_video_writer.take() {
                                writer.finish(client.subscription_filter_start(video_request_id))?;
                            }
                            // has_properties は SUBGROUP_HEADER の PROPERTIES ビットに対応し、
                            // ヘッダと全オブジェクトで一致が必須 (draft-ietf-moq-transport-22 §11.3.1 (Subgroup Header))
                            // 映像ストリームには毎フレーム LOC プロパティを付与するため true を渡す
                            current_video_writer = Some(
                                SubgroupWriter::new(
                                    &handle,
                                    &data_plane,
                                    video_request_id,
                                    VIDEO_TRACK_ALIAS,
                                    video_group_id,
                                    DEFAULT_PUBLISHER_PRIORITY,
                                    true,
                                    client.subscription_filter_start(video_request_id),
                                )
                                .await?,
                            );
                            tracing::debug!("Started new video group {}", video_group_id);
                            video_group_id += 1;
                        }
                        let properties = build_video_loc_properties(&ef, video_timescale);
                        if let Some(writer) = current_video_writer.as_mut() {
                            let _ = writer.write_object(&ef.data, &properties).await?;
                        }
                    }
                    Ok(())
                }
                .await;
                // セッション終了は異常ではなく期待される終了である (`is_transport_session_end`)。
                // `?` で `run` を抜けず、終了時の後始末へ進む
                match outcome {
                    Ok(()) => {}
                    Err(e) if is_transport_session_end(&e) => {
                        tracing::info!("Session closed by transport");
                        session_ended = true;
                        break 'main;
                    }
                    Err(e) if is_peer_stream_reset(&e) => {
                        // peer が subgroup stream を cancel した (§11.3.2)。該当ストリームの
                        // writer を捨て、次の group id へ進める。STOP_SENDING を受けた
                        // Subgroup の再オープンは Forward State 0→1 の REQUEST_UPDATE 受理まで
                        // 禁止されるため、同じ group id (この example は subgroup id 0 固定) を
                        // 使うと `send_subgroup_header` が拒否される
                        current_video_writer = None;
                        video_group_id += 1;
                        tracing::info!(
                            "Subgroup stream was reset by peer; dropping the current video group"
                        );
                    }
                    Err(e) => {
                        // MOQT メッセージの encode / decode 失敗は終了コード付きで閉じる
                        close_on_message_error(&mut client, &e).await;
                        return Err(e);
                    }
                }
            }
            audio_input = audio_input_rx.recv(), if config.audio_enabled => {
                let Some(audio_input) = audio_input else {
                    tracing::info!("Audio input channel closed");
                    break;
                };
                // 扱いは video 分岐と同じである。音声は 20 ms ごとに新しいストリームを開くため、
                // セッション終了後に `SubgroupWriter::new` が `ConnectionClosed` を返す経路に
                // 入りやすい。
                let outcome: Result<()> = async {
                    let encoder = audio_encoder.as_mut().expect("audio encoder enabled");
                    let samples_per_frame =
                        audio_samples_per_frame.expect("audio samples_per_frame enabled");
                    // 再エンコードでは入力サンプルのタイムスタンプをそのまま使う
                    let (pcm, input_timestamp) = match audio_input {
                        AudioInput::Raw(frame) => {
                            // live capture の Timestamp は capture のメディア時刻を epoch
                            // マイクロ秒へ換算する。起源はカメラとマイクで異なるため、
                            // 換算は映像と音声で別々の時計が持つ
                            let frame_epoch_us = map_audio_capture_timestamp_us(
                                &mut audio_clock,
                                &mut capture_residuals,
                                frame.timestamp_us,
                                wall_clock_now_us()?,
                            )?;
                            // バッファに残っているサンプルはこのフレームの直前のサンプルで
                            // あるため、バッファ先頭の時刻はこのフレームの時刻から
                            // サンプル数ぶん戻った位置になる。取りこぼしたフレームがあっても
                            // その差をここで吸収でき、壁時計からずれない
                            audio_timeline.observe_frame(
                                frame_epoch_us,
                                audio_pcm_buf.len() as u64,
                                AUDIO_SAMPLE_RATE,
                            );
                            (extract_pcm_i16(&frame)?, None)
                        }
                        AudioInput::Reencode { frame, timestamp } => {
                            (extract_pcm_i16(&frame)?, Some(timestamp))
                        }
                    };
                    audio_pcm_buf.extend_from_slice(&pcm);
                    // 再エンコードでは 1 つの入力から複数チャンクを取り出しても入力の
                    // タイムスケールで 20 ms ずつ進める
                    let mut next_chunk_timestamp = input_timestamp;
                    while audio_pcm_buf.len() >= samples_per_frame {
                        let chunk: Vec<i16> = audio_pcm_buf.drain(..samples_per_frame).collect();
                        let encoded = encoder.encode(&chunk)?;
                        let timestamp = match next_chunk_timestamp {
                            Some(timestamp) => {
                                // 再エンコードの入力音声トラックのタイムスケール
                                let timescale = audio_timescale
                                    .expect("re-encode audio input has a timescale");
                                let step = samples_per_frame as u64 * timescale / (AUDIO_SAMPLE_RATE as u64);
                                next_chunk_timestamp = Some(timestamp.saturating_add(step));
                                timestamp
                            }
                            // live capture はバッファ先頭の時刻から切り出し位置までを
                            // サンプル数で進める。同じ capture フレーム由来の複数 Object が
                            // 同じ Timestamp にならない
                            None => audio_timeline.next_chunk_timestamp_us(
                                samples_per_frame as u64,
                                AUDIO_SAMPLE_RATE,
                            ),
                        };
                        // 最初の audio object にだけ Audio Config (OpusHead) を付与する。
                        // フィルタ不通過で Skip された場合は次回のオブジェクトに付与し直す
                        // (Skip 時に audio_config_sent を立てると OpusHead が永遠に届かず、
                        // フィルタを通過する後続フレームが Opus デコード不能になる)。
                        let audio_config = if audio_config_sent {
                            None
                        } else {
                            audio_opus_head.as_deref()
                        };
                        let properties =
                            build_audio_loc_properties(timestamp, audio_timescale, audio_config);
                        // LOC draft-ietf-moq-loc-04 §4.1 (Application with one audio track): 1 audio frame = 1 Object = 1 Group
                        if config.audio_datagram {
                            let mut writer = DatagramWriter::new(
                                &handle,
                                &data_plane,
                                audio_request_id.expect("audio request_id enabled"),
                                AUDIO_TRACK_ALIAS,
                                audio_group_id,
                                DEFAULT_PUBLISHER_PRIORITY,
                                config.datagram_max_size,
                            );
                            let outcome = writer.write_object(&encoded, &properties).await?;
                            if outcome == ObjectFilterOutcome::Pass {
                                audio_config_sent = true;
                            }
                        } else {
                            // has_properties は SUBGROUP_HEADER の PROPERTIES ビットに対応し、
                            // ヘッダと全オブジェクトで一致が必須 (draft-ietf-moq-transport-22 §11.3.1 (Subgroup Header))
                            // 音声ストリームには毎フレーム LOC プロパティを付与するため true を渡す
                            let audio_request_id =
                                audio_request_id.expect("audio request_id enabled");
                            let mut writer = SubgroupWriter::new(
                                &handle,
                                &data_plane,
                                audio_request_id,
                                AUDIO_TRACK_ALIAS,
                                audio_group_id,
                                DEFAULT_PUBLISHER_PRIORITY,
                                true,
                                client.subscription_filter_start(audio_request_id),
                            )
                            .await?;
                            let outcome = writer.write_object(&encoded, &properties).await?;
                            if outcome == ObjectFilterOutcome::Pass {
                                audio_config_sent = true;
                            }
                            writer.finish(client.subscription_filter_start(audio_request_id))?;
                        }
                        audio_group_id += 1;
                        audio_frame_count += 1;
                    }
                    Ok(())
                }
                .await;
                match outcome {
                    Ok(()) => {}
                    Err(e) if is_transport_session_end(&e) => {
                        tracing::info!("Session closed by transport");
                        session_ended = true;
                        break 'main;
                    }
                    Err(e) if is_peer_stream_reset(&e) => {
                        // peer が音声の subgroup stream を cancel した (§11.3.2)。該当ストリーム
                        // だけを終端し、次のフレームから新しい group で再開する。同じ group id を
                        // 使うと再オープン禁止で拒否されるため、通常経路と同じくここで進める
                        audio_group_id += 1;
                        tracing::info!(
                            "Audio subgroup stream was reset by peer; continuing with the next frame"
                        );
                    }
                    Err(e) => {
                        // MOQT メッセージの encode / decode 失敗は終了コード付きで閉じる
                        close_on_message_error(&mut client, &e).await;
                        return Err(e);
                    }
                }
            }
            notable = client.next_event() => {
                // 分岐本体を async ブロックに閉じ込め、`?` が `run` を抜けないようにする。
                // `client.next_event()` の結果とピア要求への応答 (`serve_peer_request`) は
                // どちらも transport I/O を行い、セッション終了後は
                // `TransportError::ConnectionClosed` を返す。
                // `break 'main` はブロックの外に出す (ブロックの中では外側のループを
                // 抜けられない) ため、ブロックは「ループを抜けるか」を返す。
                let outcome: Result<bool> = async {
                    match notable? {
                        Some(ClientEvent::Session(SessionEvent::GoawayReceived {
                            timeout, ..
                        })) => {
                            tracing::info!("Received GOAWAY (timeout={timeout})");
                            Ok(true)
                        }
                        Some(ClientEvent::Session(SessionEvent::CloseSession(err))) => {
                            tracing::warn!("Session closed: {:#x} {}", err.code, err.reason);
                            Ok(true)
                        }
                        Some(ClientEvent::Session(SessionEvent::PublishDoneReceived {
                            request_id,
                            ..
                        })) => {
                            tracing::info!("Peer sent PUBLISH_DONE for request {request_id}");
                            Ok(false)
                        }
                        Some(ClientEvent::Session(SessionEvent::ResetDataStream {
                            stream_id,
                            error_code,
                            ..
                        })) => {
                            // I/O 層 (この example) が当該 uni data stream を RESET_STREAM で閉じ、
                            // その後に Session へ終端を通知する。通知が保留 PUBLISH_DONE の
                            // flush 条件 (全 outgoing stream の終端) を満たす
                            // (draft-ietf-moq-transport-22 §5.2 (Delivery Timeouts and Data Reliability))。
                            reset_open_subgroup_stream(
                                &mut current_video_writer,
                                stream_id,
                                error_code,
                            )
                        }
                        Some(ClientEvent::Request(request)) => {
                            serve_peer_request(
                                &mut client,
                                request,
                                &config,
                                &catalog_json,
                                catalog_location,
                            )
                            .await?;
                            Ok(false)
                        }
                        Some(ClientEvent::Session(SessionEvent::OpenFillFetchStream {
                            request_id,
                            subscription_request_id,
                            start,
                            end,
                        })) => {
                            // draft-ietf-moq-transport-22 §3.4 (Fill Semantics) / §3.4.1:
                            // FILL_PARAMETERS 付き SUBSCRIBE / REQUEST_UPDATE を受けた publisher は
                            // uni stream を開き、起因メッセージの Request ID を載せた FETCH_HEADER に
                            // 続けて fill range の Object を送り、FIN で閉じる (FETCH_OK は送らない)。
                            // 応答できない場合は FETCH_HEADER の直後で reset して失敗を伝える
                            // (無応答にすると peer が fill を待ち続ける)
                            match client.subscription_track_name(subscription_request_id) {
                                Some(track) if track == MSF_CATALOG_TRACK_NAME => {
                                    // 送るのは現在のカタログ Object 1 つだけである。これが fill range に
                                    // 入らない場合 (過去 Group だけを指す range など) は応答できないため
                                    // reset で失敗を伝える
                                    let location = catalog_location;
                                    if location < start || location > end {
                                        tracing::info!(
                                            "Rejecting catalog fill fetch outside the requested range: request_id={request_id}, location={:?}",
                                            location,
                                        );
                                        reject_fill_fetch(&mut client, request_id).await;
                                    } else if let Err(e) = client
                                        .send_fill_fetch_response(
                                            request_id,
                                            location.group_id,
                                            location.object_id,
                                            &catalog_json,
                                        )
                                        .await
                                    {
                                        // 失敗しても publisher は停止しない。Terminated / 回収済みの
                                        // subscription は通常の失敗条件である (§3.4.1)
                                        tracing::warn!(
                                            "Failed to serve catalog fill fetch (request_id={request_id}): {e}"
                                        );
                                    } else {
                                        tracing::info!(
                                            "Served catalog fill fetch (request_id={request_id}, group={})",
                                            location.group_id,
                                        );
                                    }
                                }
                                Some(track) => {
                                    // moq-pub はカタログ以外の fill range を判定できないため、
                                    // FETCH_HEADER の直後で reset する (§3.4.1)。
                                    // video / audio への FILL_PARAMETERS 付き SUBSCRIBE もここへ来る
                                    tracing::warn!(
                                        "Rejecting fill fetch for an unsupported track: track={}",
                                        String::from_utf8_lossy(&track),
                                    );
                                    reject_fill_fetch(&mut client, request_id).await;
                                }
                                None => {
                                    // subscription を引けない経路 (回収済みなど) も reset で伝える
                                    tracing::warn!(
                                        "Rejecting fill fetch for an unresolved subscription: request_id={request_id}",
                                    );
                                    reject_fill_fetch(&mut client, request_id).await;
                                }
                            }
                            Ok(false)
                        }
                        Some(ClientEvent::RequestUpdate(update)) => {
                            // draft-ietf-moq-transport-22 §9.5 (REQUEST_UPDATE):
                            // 受信側は必ず 1 通の REQUEST_OK / REQUEST_ERROR で応答する MUST。
                            // FORWARD / LOCATION_FILTER / Range Filters の検証と購読状態への
                            // 反映は session 層が受信時に済ませているため、ここでは受理する。
                            tracing::info!(
                                "Received REQUEST_UPDATE: request_id={}",
                                update.request_id
                            );
                            if let Err(e) = client.send_request_ok(update.request_id).await {
                                tracing::warn!(
                                    "Failed to send REQUEST_OK for request {}: {e}",
                                    update.request_id
                                );
                            }
                            Ok(false)
                        }
                        _ => Ok(false),
                    }
                }
                .await;
                match outcome {
                    // セッションが終了した (peer 起点の終了通知 / 自側で検出した違反の双方)。
                    // 終了時の後始末へ進む
                    Ok(true) => {
                        session_ended = true;
                        break 'main;
                    }
                    Ok(false) => {}
                    Err(e) if is_transport_session_end(&e) => {
                        tracing::info!("Session closed by transport");
                        session_ended = true;
                        break 'main;
                    }
                    Err(e) if is_peer_stream_reset(&e) => {
                        // peer が bidi request stream を cancel した (§6.4.2.3)。該当 request
                        // だけが終端するため、パイプラインは継続する
                        tracing::info!("Request stream was reset by peer; continuing");
                    }
                    Err(e) => {
                        // MOQT メッセージの encode / decode 失敗は終了コード付きで閉じる
                        close_on_message_error(&mut client, &e).await;
                        return Err(e);
                    }
                }
            }
            _ = tick_interval.tick() => {
                let now_ms = Instant::now().duration_since(start).as_millis() as u64;
                client.tick(now_ms);
                // 停滞の切り分け用の任意診断。MOQT_PUBLISH_DIAG=1 のときだけ出す。
                // publish ループがフレームを処理し続けているかを追う
                if publish_diag_enabled() {
                    tracing::info!(
                        "PUBDIAG t={}ms video_groups={} audio_objects={}",
                        now_ms,
                        video_group_id,
                        audio_frame_count,
                    );
                }
            }
            _ = catalog_republish_interval.tick() => {
                // 独立したカタログを新しい Group の先頭に置き直す
                // (draft-ietf-moq-msf-01 §5 (Catalog) / §6.1 (Group numbering))。
                // relay の cache から落ちる前に送り直すことで、配信開始後に購読を始めた相手も
                // cache のカタログを FETCH で取得できる。
                let outcome: Result<()> = async {
                    let next_group_id = group_id::next_group_id(catalog_group_id);
                    let published = catalog::publish_catalog_object(
                        &handle,
                        &data_plane,
                        catalog_request_id,
                        CATALOG_TRACK_ALIAS,
                        next_group_id,
                        &catalog_json,
                        client.subscription_filter_start(catalog_request_id),
                    )
                    .await?;
                    catalog_group_id = next_group_id;
                    if published == ObjectFilterOutcome::Skip {
                        // どの購読にも届かなかった (Forward State 0 など)。Object は送られて
                        // いないため、FETCH 応答と LARGEST_OBJECT が指す Location は前回の
                        // 送信のままにする
                        tracing::debug!(
                            "Catalog republish at group {next_group_id} was filtered out"
                        );
                    } else {
                        catalog_location = Location {
                            group_id: next_group_id,
                            object_id: 0,
                        };
                        tracing::debug!("Republished the MSF catalog at group {next_group_id}");
                    }
                    Ok(())
                }
                .await;
                match outcome {
                    Ok(()) => {}
                    Err(e) if is_transport_session_end(&e) => {
                        tracing::info!("Session closed by transport");
                        session_ended = true;
                        break 'main;
                    }
                    Err(e) if is_peer_stream_reset(&e) => {
                        // peer がカタログの Subgroup stream を cancel した。同じ Location を
                        // 再送できないため Group だけ進め、次の間隔で送り直す
                        catalog_group_id = group_id::next_group_id(catalog_group_id);
                        tracing::info!("Catalog subgroup stream was reset by peer; continuing");
                    }
                    Err(e) => {
                        // MOQT メッセージの encode / decode 失敗は終了コード付きで閉じる
                        close_on_message_error(&mut client, &e).await;
                        return Err(e);
                    }
                }
            }
            _ = shutdown_monitor.recv() => {
                tracing::info!("Shutdown signal received");
                break;
            }
        }
    }

    // MP4 リーダーのスレッドを停止して実行時エラーを拾う
    // (open 時に全サンプルを検証しているため通常は到達しないが、異常終了を正常終了と
    // 混同しないようにする)
    let mp4_reader_result = match mp4_source.take() {
        Some(source) => source.stop(),
        None => Ok(()),
    };
    let reencode_result = match reencode_source.take() {
        Some(source) => source.stop(),
        None => Ok(()),
    };

    if let Some(writer) = current_video_writer.take() {
        // 終了時点で購読が消えていれば `None` (Location Filter 無し) と同じ扱いになり、
        // 省略があれば RESET 側に倒れる (安全側)
        writer.finish(video_request_id.and_then(|rid| client.subscription_filter_start(rid)))?;
    }

    // PUBLISH_DONE を各 request に送信してストリームを閉じる
    if let Some(rid) = video_request_id
        && let Err(e) = client
            .send_publish_done(rid, PUBLISH_DONE_GOING_AWAY, "")
            .await
    {
        log_failure(
            format_args!("Failed to send PUBLISH_DONE for video: {e}"),
            session_ended || is_connection_closed(&e),
        );
    }
    if let Some(rid) = audio_request_id
        && let Err(e) = client
            .send_publish_done(rid, PUBLISH_DONE_GOING_AWAY, "")
            .await
    {
        log_failure(
            format_args!("Failed to send PUBLISH_DONE for audio: {e}"),
            session_ended || is_connection_closed(&e),
        );
    }
    if let Err(e) = client
        .send_publish_done(catalog_request_id, PUBLISH_DONE_GOING_AWAY, "")
        .await
    {
        log_failure(
            format_args!("Failed to send PUBLISH_DONE for catalog: {e}"),
            session_ended || is_connection_closed(&e),
        );
    }

    if let Err(e) = client.send_goaway(Vec::new(), 5000).await {
        log_failure(
            format_args!("Failed to send GOAWAY: {e}"),
            session_ended || is_connection_closed(&e),
        );
    }

    if let Err(e) = client.close(0, "").await {
        log_failure(
            format_args!("Failed to close session gracefully: {e}"),
            session_ended || is_connection_closed(&e),
        );
    }

    tracing::info!("Pipeline stopped");
    mp4_reader_result.and(reencode_result)
}

/// macOS 以外で H.264 / H.265 のエンコーダを指定したときのエラーを作る
#[cfg(not(target_os = "macos"))]
fn unsupported_macos_encoder(codec: &str) -> Error {
    Error::Other(format!(
        "{codec} encoder is only available on macOS; rebuild on macOS or specify --video-codec av1"
    ))
}

/// transport がセッション終了 (WebTransport の CONNECT stream の close / WT_CLOSE_SESSION) や
/// 接続クローズを検知したことを表すエラーか
///
/// セッション終了は異常ではなく期待される終了である。data plane の送信経路
/// (`SubgroupWriter::new` / `SubgroupWriter::write_object` / `DatagramWriter::write_object`
/// など) はセッション終了後に `TransportError::ConnectionClosed` を返すため、`?` で `run` まで
/// 通すと `Fatal: session closed` で異常終了し、終了時の後始末 (`PUBLISH_DONE` /
/// `close(0, "")`) に到達しない。WebTransport で §6 が求めるのは `close(0, "")` が送る
/// CONNECT stream の FIN であり、終了検知後の GOAWAY は失敗しうる (warn ログになる)。
///
/// `From<TransportError> for Error` が `TransportError::ConnectionClosed` を
/// `Error::ConnectionClosed` に振り分けるため、variant だけで判定できる (表示文字列には
/// 依存しない)。
fn is_transport_session_end(error: &Error) -> bool {
    matches!(error, Error::ConnectionClosed)
}

/// transport エラーが peer によるストリーム終端 (送信方向への STOP_SENDING) を表すかどうか
///
/// s2n-quic は peer の STOP_SENDING を受信したストリームの送信 API を `StreamReset` で
/// 失敗させる。これは該当ストリームだけの終端でありセッションは継続するため
/// (draft-ietf-moq-transport-22 §6.4.2.3 (Request Cancellation and Rejection))、
/// pipeline は致命エラーにしない。
///
/// WebTransport over HTTP/2 は STOP_SENDING の観測経路を持たず、peer の cancel は
/// `TransportError::StreamClosed` として現れるため本判定の対象外である
/// (該当経路は従来どおり致命扱いのまま)。
fn is_peer_stream_reset(error: &Error) -> bool {
    matches!(error, Error::StreamReset { .. })
}

/// transport エラーがセッション終了 (接続クローズ) を表すかどうか
///
/// MoqtClient の後始末 API は `Error` ではなく `TransportError` を返す。
/// セッション終了の判別は `Error::ConnectionClosed` と同じ variant で行う。
fn is_connection_closed(error: &TransportError) -> bool {
    matches!(error, TransportError::ConnectionClosed)
}

/// セッション終了に伴う失敗を `info!`、それ以外を `warn!` で出す
///
/// WebTransport のセッション終了 (draft-ietf-webtrans-http3-16 §6) は異常ではないため、
/// 終了に伴う失敗を warn として出すとログから実際の異常を区別できない。
///
/// `expected` はセッション終了に伴う失敗かどうか。セッション終了後の後始末では `close` の
/// 送信失敗がセッション終了以外のエラーに畳まれる経路もあるため、呼び出し側が「セッション終了を
/// 観測して後始末に入った」ことも含めて渡す。
fn log_failure(message: std::fmt::Arguments<'_>, expected: bool) {
    if expected {
        tracing::info!("{message}");
    } else {
        tracing::warn!("{message}");
    }
}

/// MOQT メッセージの encode / decode 失敗なら終了コード付きで閉じる
///
/// draft-ietf-moq-transport-22 §9 (Control Messages) と §9.20.1 (Parameter Scope) は
/// 不正なメッセージの受信を PROTOCOL_VIOLATION で閉じることを MUST で要求する。
/// decode 失敗は codec 層で完結するため Session はこの違反を観測できず、I/O 層である
/// main ループが終了コードを決める。encode 失敗はローカル要因だが、いずれにせよ致命的な
/// 経路のため同じ写像で閉じる。`Error::Moqt` 以外は何もしない (呼び出し側が `run` の
/// 戻り値として扱う)。
async fn close_on_message_error(client: &mut MoqtClient, error: &Error) {
    let Error::Moqt(e) = error else {
        return;
    };
    let code = session_error_code(e);
    tracing::warn!("Closing session on MOQT message error: {code:#x} {e}");
    if let Err(close_err) = client.close(code, e.reason()).await {
        tracing::warn!("Failed to close session: {close_err}");
    }
}

/// decode 失敗を閉じる終了コード
///
/// library の data plane が使う `session_error_from_data_message` と同じ写像である
/// (`src/session/data.rs`)。draft-ietf-moq-transport-22 §8.3 (Key-Value-Pair Structure) が
/// MUST を定める書式違反は KEY_VALUE_FORMATTING_ERROR (0x6)、それ以外の decode 失敗は
/// PROTOCOL_VIOLATION (0x3) で閉じる (コードは §12.2 (Session Termination Codes))。
///
/// `MalformedAuthToken` は §8.9 (Authorization Token Compression) がメッセージ単位の reject を
/// MUST で求めるが、decode を中断した時点で Request ID が得られずメッセージ単位の
/// reject を送れないため、セッション終了として PROTOCOL_VIOLATION に写す。
fn session_error_code(error: &MessageError) -> u64 {
    match error {
        MessageError::KeyValueFormattingError(_) => SESSION_KEY_VALUE_FORMATTING_ERROR,
        _ => SESSION_PROTOCOL_VIOLATION,
    }
}

/// fill fetch stream を開いて FETCH_HEADER の直後で reset する
///
/// draft-ietf-moq-transport-22 §3.4.1 (Opening and Closing Fill Fetch Streams): fill fetch
/// stream には REQUEST_ERROR が無く、publisher は fill の失敗を reset で伝える。無応答にすると
/// peer が fill を待ち続けるため、応答できない場合も stream を開いて reset する。
async fn reject_fill_fetch(client: &mut MoqtClient, request_id: u64) {
    if let Err(e) = client.reject_fill_fetch(request_id).await {
        tracing::warn!("Failed to reject fill fetch (request_id={request_id}): {e}");
    }
}

/// MOQT relay が転送してきた要求 (SUBSCRIBE / FETCH) に応答する
///
/// relay は subscriber の SUBSCRIBE / FETCH を publisher 側 session の新しい
/// request stream として転送する (draft-ietf-moq-transport-22 §6.3 (Session initialization))。
/// 本 publisher は video / audio / catalog の SUBSCRIBE に SUBSCRIBE_OK を返し
/// (以後の Object は継続して送信中の subgroup stream で届く)、catalog の FETCH には
/// 要求された range を判定したうえで FETCH_OK と FETCH 応答ストリームを返す。
/// 未知の track と未対応の要求種別は REQUEST_ERROR で拒否する
/// (draft-ietf-moq-transport-22 §9.4 (REQUEST_ERROR))。
async fn serve_peer_request(
    client: &mut MoqtClient,
    request: IncomingRequest,
    config: &Config,
    catalog_json: &[u8],
    catalog_location: Location,
) -> Result<()> {
    let request_id = request.request_id;
    match request.message {
        ControlMessage::Subscribe(subscribe) => {
            let track: &[u8] = &subscribe.track_name;
            let track_alias = if config.video_enabled && track == config.track_name.as_bytes() {
                Some(VIDEO_TRACK_ALIAS)
            } else if config.audio_enabled && track == AUDIO_TRACK_NAME.as_bytes() {
                Some(AUDIO_TRACK_ALIAS)
            } else if track == MSF_CATALOG_TRACK_NAME {
                // カタログの購読を受理する。SUBSCRIBE_OK の LARGEST_OBJECT は session 層が
                // この track の publish 済み Object から補完する
                // (draft-ietf-moq-transport-22 §9.20.17 (LARGEST OBJECT Parameter))
                Some(CATALOG_TRACK_ALIAS)
            } else {
                None
            };
            match track_alias {
                Some(track_alias) => {
                    client
                        .send_subscribe_ok(
                            request_id,
                            track_alias,
                            MessageParameters::new(),
                            TrackProperties::new(),
                        )
                        .await?;
                    tracing::info!(
                        "Accepted SUBSCRIBE (request_id={request_id}, track={}, alias={track_alias})",
                        String::from_utf8_lossy(track),
                    );
                }
                None => {
                    tracing::warn!(
                        "Rejecting SUBSCRIBE for unknown track: {}",
                        String::from_utf8_lossy(track),
                    );
                    client
                        .send_request_error(request_id, REQUEST_DOES_NOT_EXIST, "unknown track")
                        .await?;
                }
            }
        }
        ControlMessage::Fetch(fetch) => {
            if fetch.track_name == MSF_CATALOG_TRACK_NAME {
                // 要求された Location Filter から range を解決し、範囲外の Object を
                // 送らないようにする (draft-ietf-moq-transport-22 §3.2 (Fetch) /
                // §3.3.1 (Location Filters) / §9.20.9 (LOCATION FILTER Parameter))。
                // wire 経路では decode 層が不正な符号化を弾くため、ここでの失敗は
                // アプリが手組みしたメッセージだけが到達する
                let filter = fetch.parameters.location_filter_typed().map_err(|e| {
                    Error::Other(format!("invalid catalog fetch location filter: {e}"))
                });
                let filter = match filter {
                    Ok(filter) => filter,
                    Err(e) => {
                        tracing::warn!("Rejecting catalog FETCH with invalid filter: {e}");
                        client
                            .send_request_error(
                                request_id,
                                REQUEST_INVALID_FILTER,
                                "invalid location filter",
                            )
                            .await?;
                        return Ok(());
                    }
                };
                match catalog::catalog_fetch_response(filter.as_ref(), catalog_location) {
                    catalog::CatalogFetchResponse::Object { end_location } => {
                        client
                            .send_fetch_ok(
                                request_id,
                                0,
                                end_location,
                                MessageParameters::new(),
                                TrackProperties::new(),
                            )
                            .await?;
                        client
                            .send_fetch_response(
                                request_id,
                                catalog_location.group_id,
                                catalog_location.object_id,
                                catalog_json,
                                None,
                            )
                            .await?;
                        tracing::info!(
                            "Served catalog FETCH (request_id={request_id}, group={})",
                            catalog_location.group_id,
                        );
                    }
                    catalog::CatalogFetchResponse::Empty { end_location } => {
                        // 要求 range に Object が無い場合は FETCH_OK と FETCH_HEADER だけの
                        // 空ストリームを返す (draft-ietf-moq-transport-22 §3.2.1)
                        client
                            .send_fetch_ok(
                                request_id,
                                0,
                                end_location,
                                MessageParameters::new(),
                                TrackProperties::new(),
                            )
                            .await?;
                        client.send_empty_fetch_response(request_id).await?;
                        tracing::info!(
                            "Served empty catalog FETCH (request_id={request_id}, end={:?})",
                            end_location,
                        );
                    }
                    catalog::CatalogFetchResponse::InvalidRange => {
                        tracing::warn!(
                            "Rejecting catalog FETCH with out-of-range filter (request_id={request_id})"
                        );
                        client
                            .send_request_error(request_id, REQUEST_INVALID_RANGE, "invalid range")
                            .await?;
                    }
                }
            } else {
                tracing::warn!(
                    "Rejecting FETCH for unknown track: {}",
                    String::from_utf8_lossy(&fetch.track_name),
                );
                client
                    .send_request_error(request_id, REQUEST_DOES_NOT_EXIST, "unknown track")
                    .await?;
            }
        }
        other => {
            tracing::warn!("Rejecting unsupported peer request: {other:?}");
            client
                .send_request_error(request_id, REQUEST_NOT_SUPPORTED, "unsupported request")
                .await?;
        }
    }
    Ok(())
}

/// 出力フレームに入力サンプルの PTS を割り当てる
///
/// 再エンコードでは出力フレームのタイムスタンプに入力サンプルの PTS を使う。
/// 0 フレームを返した入力の PTS は次に出力されたフレームへ引き継ぐ。
///
/// 入力より出力が多い場合、余ったフレームには encoder が付けたマイクロ秒の値が残る。
/// そのまま送ると入力トラックの timescale 単位の値と混ざり、購読側が別の時刻として解釈する。
/// `output_timescale` が `Some` のときは、余ったフレームの値を入力の単位へ戻す。
/// `None` のとき (live capture と、エンコード済みサンプルをそのまま送る経路) は、encoder か
/// 入力ファイルが付けた値がそのまま送る単位であるため、変換しない。
fn assign_input_timestamps(
    pending: &mut VecDeque<u64>,
    input_timestamp: Option<u64>,
    output_timescale: Option<u64>,
    frames: &mut [EncodedFrame],
) {
    if let Some(timestamp) = input_timestamp {
        pending.push_back(timestamp);
    }
    for frame in frames {
        match pending.pop_front() {
            Some(timestamp) => frame.timestamp = timestamp,
            None => {
                if let Some(timescale) = output_timescale {
                    frame.timestamp = media_time_from_micros(frame.timestamp, timescale);
                }
            }
        }
    }
}

/// フレームを読んだ時刻を Unix epoch マイクロ秒で返す
///
/// メディア時刻を壁時計へ換算する対応を取るために使う。システムの時計が Unix epoch
/// より前を指している場合だけエラーになる。
fn wall_clock_now_us() -> Result<i64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::Other(format!("system clock is before the Unix epoch: {e}")))?;
    i64::try_from(elapsed.as_micros())
        .map_err(|_| Error::Other("system clock is out of the supported range".to_string()))
}

/// 残差の要約を出す間隔 (マイクロ秒)
const CAPTURE_RESIDUAL_REPORT_US: i64 = 5_000_000;

/// 要約に使う残差の件数 (直近のこの件数だけを見る)
const CAPTURE_RESIDUAL_SAMPLES: usize = 1_024;

/// 残差を溜めるトラック
#[derive(Debug, Clone, Copy)]
enum CaptureTrack {
    /// 映像トラック
    Video,
    /// 音声トラック
    Audio,
}

impl CaptureTrack {
    /// トラックごとの標本の添字
    fn index(self) -> usize {
        match self {
            CaptureTrack::Video => 0,
            CaptureTrack::Audio => 1,
        }
    }

    /// ログに出すトラック名
    fn name(self) -> &'static str {
        match self {
            CaptureTrack::Video => "video",
            CaptureTrack::Audio => "audio",
        }
    }
}

/// capture のメディア時刻と壁時計の差 (残差) を溜めて、定期的に要約を出す
///
/// 残差は「撮影から読むまでの遅れ」であり、[`WallClockMapper`] はその最小値を対応に使う
/// ため 0 にはならない。0103 の完了条件は音声と映像の残差の中央値の差であるが、毎フレームの
/// debug ログでは確認できないため、トラックごとに溜めて定期的に info で出す。
struct CaptureClockResiduals {
    /// トラックごとの直近の残差 (マイクロ秒)。添字は [`CaptureTrack::index`] が決める
    samples_us: [Vec<i64>; 2],
    /// 直近に要約を出した時刻 (マイクロ秒)
    last_report_us: i64,
    /// 直近の音声の補正の統計。音声トラックの換算のたびに更新する
    audio_stats: Option<AudioTimestampOffsetStats>,
}

/// 残差の要約 (最小値と中央値と件数)
struct ResidualSummary {
    min_us: i64,
    median_us: i64,
    samples: usize,
}

impl CaptureClockResiduals {
    /// まだ 1 件も記録していない状態で作る
    fn new() -> Self {
        Self {
            samples_us: [Vec::new(), Vec::new()],
            last_report_us: 0,
            audio_stats: None,
        }
    }

    /// 音声の補正の統計を記録する (次の要約と合わせて出す)
    fn observe_audio_stats(&mut self, stats: Option<AudioTimestampOffsetStats>) {
        self.audio_stats = stats;
    }

    /// 残差を記録し、要約を出す間隔が空いていればトラックごとの要約を返す
    ///
    /// `now_us` は換算に使った壁時計である。両方のトラックに 1 件以上あるときだけ要約を
    /// 返す。
    fn record(
        &mut self,
        track: CaptureTrack,
        difference_us: i64,
        now_us: i64,
    ) -> Option<[ResidualSummary; 2]> {
        let samples = &mut self.samples_us[track.index()];
        if samples.len() >= CAPTURE_RESIDUAL_SAMPLES {
            samples.remove(0);
        }
        samples.push(difference_us);
        if now_us.saturating_sub(self.last_report_us) < CAPTURE_RESIDUAL_REPORT_US {
            return None;
        }
        self.last_report_us = now_us;
        Some([self.summary(0)?, self.summary(1)?])
    }

    /// 1 つのトラックの要約。まだ 1 件も無ければ `None`
    fn summary(&self, index: usize) -> Option<ResidualSummary> {
        let samples = self.samples_us.get(index)?;
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.clone();
        sorted.sort_unstable();
        Some(ResidualSummary {
            min_us: sorted[0],
            median_us: sorted[sorted.len() / 2],
            samples: sorted.len(),
        })
    }
}

/// 残差の要約と音声の補正の統計をログに出す
///
/// 残差の要約は 5 秒間隔で出す。音声の補正の統計も同じ間隔に合わせて出し、既存のログに
/// 合わせてミリ秒へ換算する。
fn log_capture_clock_summary(
    residuals: &CaptureClockResiduals,
    video: ResidualSummary,
    audio: ResidualSummary,
) {
    tracing::info!(
        "Capture clock residuals: video min={}us median={}us samples={}, audio min={}us median={}us samples={}, median_difference_us={}, audio_offset {}",
        video.min_us,
        video.median_us,
        video.samples,
        audio.min_us,
        audio.median_us,
        audio.samples,
        video.median_us.saturating_sub(audio.median_us),
        format_audio_offset_stats(residuals.audio_stats),
    );
}

/// 音声の補正の統計をログ用の文字列にする (ミリ秒へ換算する)
///
/// [`AudioTimestampOffsetStats`] はマイクロ秒で返るため、残差の要約に合わせて小数第 1 位
/// までのミリ秒で出す。まだ観測が無ければ `none`。
fn format_audio_offset_stats(stats: Option<AudioTimestampOffsetStats>) -> String {
    let Some(stats) = stats else {
        return "none".to_string();
    };
    format!(
        "current={:.1}ms min={:.1}ms max={:.1}ms applied={} slope_10s={} slope_60s={} samples={}",
        millis(stats.current_us),
        millis(stats.min_us),
        millis(stats.max_us),
        stats.applied_us.map_or_else(
            || "none".to_string(),
            |applied_us| format!("{:.1}ms", millis(applied_us))
        ),
        slope_millis_per_second(stats.slope_10s_us_per_second),
        slope_millis_per_second(stats.slope_60s_us_per_second),
        stats.samples,
    )
}

/// マイクロ秒をミリ秒へ換算する (ログ用)
fn millis(value_us: i64) -> f64 {
    value_us as f64 / 1_000.0
}

/// 傾き (マイクロ秒 / 秒) をログ用の文字列にする (ミリ秒 / 秒)
///
/// 傾きを出せるだけの幅が無ければ `none`。
fn slope_millis_per_second(slope_us_per_second: Option<i64>) -> String {
    match slope_us_per_second {
        Some(slope_us_per_second) => format!("{:.1}", millis(slope_us_per_second)),
        None => "none".to_string(),
    }
}

/// capture の映像のメディア時刻を Unix epoch マイクロ秒へ換算する
///
/// `capture_timestamp_us` は capture フレームのメディア時刻、`wall_clock_us` はその
/// フレームを読んだ壁時計 (epoch マイクロ秒)。対応は mapper が持つため、呼び出し側は
/// トラックごとに 1 つの [`WallClockMapper`] を保持する。
///
/// 換算結果と読んだ壁時計の差を debug ログに出す。LOC は Timescale を載せないとき
/// Timestamp を Unix epoch マイクロ秒として扱うため (draft-ietf-moq-loc-04 §2.3.1.1)、
/// 実機でこの差が許容範囲に収まっていることを確認できるようにする。
fn map_capture_timestamp_us(
    mapper: &mut WallClockMapper,
    residuals: &mut CaptureClockResiduals,
    capture_timestamp_us: i64,
    wall_clock_us: i64,
) -> Result<u64> {
    // 読んだフレームを記録してから換算する。対応は遅れが最も小さいフレームで決まる
    mapper.observe(capture_timestamp_us, wall_clock_us);
    let converted_us = mapper
        .to_wall_clock_us(capture_timestamp_us, Some(wall_clock_us))
        .ok_or_else(|| {
            Error::Other("failed to convert the capture timestamp to a wall clock".to_string())
        })?;
    let difference_us = converted_us.saturating_sub(wall_clock_us);
    tracing::debug!(
        "Capture timestamp mapped to wall clock: track={}, capture_us={}, wall_clock_us={}, difference_us={}",
        CaptureTrack::Video.name(),
        capture_timestamp_us,
        wall_clock_us,
        difference_us,
    );
    // 0103 の完了条件は「音声と映像の残差の中央値の差」である。毎フレームの debug ログでは
    // 確認できないため、定期的に要約を出す
    if let Some([video, audio]) =
        residuals.record(CaptureTrack::Video, difference_us, wall_clock_us)
    {
        log_capture_clock_summary(residuals, video, audio);
    }
    // `WallClockMapper::to_wall_clock_us` は Unix epoch より前を 0 に丸めるため、
    // 負の値は返らない
    Ok(u64::try_from(converted_us).expect("converted timestamp is not negative"))
}

/// capture の音声のメディア時刻を Unix epoch マイクロ秒へ換算する
///
/// 音声のメディア時刻は壁時計と同じ時計ではない。映像の [`WallClockMapper`] は対応を
/// 小さくする向きにだけ動かすのに対し、音声は [`AudioTimestampClock`] がドリフトへ窓を
/// 滑らせて追従し、音声の時計そのものが飛ぶ段差では直近の窓から取り直す。追従の規則が
/// 違うため、換算は [`map_capture_timestamp_us`] と分ける。
///
/// 換算結果と読んだ壁時計の差を debug ログに出す。音声の補正の統計は残差の要約と合わせて
/// 出す。
fn map_audio_capture_timestamp_us(
    clock: &mut AudioTimestampClock,
    residuals: &mut CaptureClockResiduals,
    capture_timestamp_us: i64,
    wall_clock_us: i64,
) -> Result<u64> {
    // 読んだフレームを記録してから換算する。補正は「読んだ壁時計 - メディア時刻」の窓の
    // 最小値であり、音声の時計が飛んだ段差では直近の窓から取り直す
    clock.record(wall_clock_us, capture_timestamp_us);
    let converted_us = clock.apply(capture_timestamp_us).ok_or_else(|| {
        Error::Other("failed to convert the audio timestamp to a wall clock".to_string())
    })?;
    let difference_us = converted_us.saturating_sub(wall_clock_us);
    tracing::debug!(
        "Capture timestamp mapped to wall clock: track={}, capture_us={}, wall_clock_us={}, difference_us={}",
        CaptureTrack::Audio.name(),
        capture_timestamp_us,
        wall_clock_us,
        difference_us,
    );
    // 音声の補正の統計 (現在値・最小・最大・傾き・適用中の補正・サンプル数) を残差の
    // 要約と合わせて出す
    residuals.observe_audio_stats(clock.snapshot());
    if let Some([video, audio]) =
        residuals.record(CaptureTrack::Audio, difference_us, wall_clock_us)
    {
        log_capture_clock_summary(residuals, video, audio);
    }
    // `AudioTimestampClock::apply` は Unix epoch より前を 0 に丸めるため、負の値は返らない
    Ok(u64::try_from(converted_us).expect("converted timestamp is not negative"))
}

/// サンプル数をマイクロ秒へ換算する
///
/// 音声は 1 つの capture フレームから複数の Opus フレームを切り出すため、切り出し位置の
/// 時刻をサンプル数から求める。`sample_rate` は 0 にはならない (呼び出し元は 48 kHz 固定)。
fn samples_to_micros(samples: u64, sample_rate: u32) -> u64 {
    samples.saturating_mul(1_000_000) / u64::from(sample_rate)
}

/// マイクロ秒を timescale 単位のメディア時刻へ戻す
///
/// 再エンコードで入力より出力が多かったとき、encoder が付けたマイクロ秒の値を入力トラック
/// の単位へ戻すために使う。`media_time_to_micros` の逆の換算であり、`timescale` は 0 には
/// ならない (MP4 の timescale は NonZeroU32 由来)。`u64` に収まらない値は飽和させる。
fn media_time_from_micros(micros: u64, timescale: u64) -> u64 {
    let media_time = u128::from(micros) * u128::from(timescale) / 1_000_000;
    u64::try_from(media_time).unwrap_or(u64::MAX)
}

/// timescale 単位のメディア時刻をマイクロ秒へ換算する
///
/// 再エンコードでは入力サンプルの PTS が入力トラックの timescale 単位で来るため、
/// encoder へ渡すメディア時刻 (マイクロ秒) に換算する。`timescale` は 0 にはならない
/// (MP4 の timescale は NonZeroU32 由来)。
fn media_time_to_micros(timestamp: u64, timescale: u64) -> u64 {
    let micros = u128::from(timestamp) * 1_000_000 / u128::from(timescale);
    u64::try_from(micros).unwrap_or(u64::MAX)
}

/// live capture の音声へ載せる Timestamp を組み立てる状態
///
/// 音声は 1 つの capture フレームと 1 つの Object が 1:1 ではない。バッファに残った
/// サンプルと次のフレームのサンプルを繋いで 20 ms ずつ切り出すため、切り出し位置の
/// 時刻を「バッファ先頭の時刻 + 切り出し済みサンプル数」で求める。同じ capture
/// フレームから複数の Object を切り出しても同じ Timestamp にはならない。
struct AudioCaptureTimeline {
    /// バッファ先頭のサンプルの時刻 (Unix epoch マイクロ秒)
    head_epoch_us: u64,
    /// バッファ先頭から切り出し済みのサンプル数
    consumed_samples: u64,
}

impl AudioCaptureTimeline {
    /// まだフレームを受け取っていない状態で作る
    fn new() -> Self {
        Self {
            head_epoch_us: 0,
            consumed_samples: 0,
        }
    }

    /// capture フレームをバッファへ追加する時点で基準を取り直す
    ///
    /// `buffered_samples` はこのフレームを追加する前のバッファ長 (サンプル数)。
    /// バッファに残っているサンプルはこのフレームより前のサンプルであるため、
    /// バッファ先頭の時刻はこのフレームの時刻からそのサンプル数ぶん戻った位置になる。
    /// capture フレームを取りこぼしてもその差をここで吸収でき、蓄積したずれが残らない。
    fn observe_frame(&mut self, frame_epoch_us: u64, buffered_samples: u64, sample_rate: u32) {
        self.head_epoch_us =
            frame_epoch_us.saturating_sub(samples_to_micros(buffered_samples, sample_rate));
        self.consumed_samples = 0;
    }

    /// バッファから `samples` サンプルを切り出したチャンクの Timestamp を返す
    ///
    /// 切り出したぶんだけ次のチャンクの時刻が進む。
    fn next_chunk_timestamp_us(&mut self, samples: u64, sample_rate: u32) -> u64 {
        let timestamp_us = self
            .head_epoch_us
            .saturating_add(samples_to_micros(self.consumed_samples, sample_rate));
        self.consumed_samples = self.consumed_samples.saturating_add(samples);
        timestamp_us
    }
}

/// 映像 LOC プロパティを構築する
///
/// `timescale` は入力ファイル由来のメディア時刻を送るときだけ渡す。live capture は
/// Unix epoch マイクロ秒を送るため `None` にし、`PROP_TIMESCALE` を付けない
/// (draft-ietf-moq-loc-04 §2.3.1.1 の既定)。
fn build_video_loc_properties(frame: &EncodedFrame, timescale: Option<u64>) -> LocProperties {
    let mut props = LocProperties::new();
    // RFC 9626 §3.2 (Short Extension for Non-Scalable Streams) の 1 octet 形式は
    // |S|E|I|D|0 0 0 0| (bit 7: S, bit 6: E, bit 5: I, bit 4: D、下位 4 bit は送信時 0)。
    // 1 object = 1 frame のため S と E は常に 1。I は keyframe のときのみ 1。
    // D (Discardable) は現状情報がないため 0。
    // refs/rfc9626.txt の §3.1 (Long Extension for Scalable Streams) / §3.2 を参照。
    // この節番号・ビット割当は RFC 由来であり将来変更される可能性がある。
    let frame_marking: u8 = if frame.is_keyframe { 0xE0 } else { 0xC0 };
    props.push(LocProperty {
        prop_id: PROP_VIDEO_FRAME_MARKING,
        value: LocPropertyValue::Bytes(vec![frame_marking]),
    });
    props.push(LocProperty {
        prop_id: PROP_TIMESTAMP,
        value: LocPropertyValue::VarInt(frame.timestamp),
    });
    if frame.is_keyframe {
        if let Some(timescale) = timescale {
            props.push(LocProperty {
                prop_id: PROP_TIMESCALE,
                value: LocPropertyValue::VarInt(timescale),
            });
        }
        if let Some(sh) = frame.video_config.as_deref() {
            props.push(LocProperty {
                prop_id: PROP_VIDEO_CONFIG,
                value: LocPropertyValue::Bytes(sh.to_vec()),
            });
        }
    }
    props
}

/// OpusHead パケットを構築する (RFC 7845 §5.1 (Identification Header))
///
/// Audio Config プロパティ (draft-ietf-moq-loc-04 §2.3.3.1 (Audio Config)) に載せる
/// Opus の configuration bytes を生成する。
///
/// フォーマット:
/// - Magic: "OpusHead" (8 bytes)
/// - Version: 1 (1 byte)
/// - Channel Count: channels (1 byte)
/// - Pre-skip: 0 (2 bytes, little-endian)
/// - Input Sample Rate: sample_rate (4 bytes, little-endian)
/// - Output Gain: 0 (2 bytes, little-endian)
/// - Channel Mapping Family: 0 (1 byte)
fn build_opus_head(sample_rate: u32, channels: u8) -> Vec<u8> {
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // Version
    head.push(channels); // Channel Count
    head.extend_from_slice(&0u16.to_le_bytes()); // Pre-skip
    head.extend_from_slice(&sample_rate.to_le_bytes()); // Input Sample Rate
    head.extend_from_slice(&0u16.to_le_bytes()); // Output Gain
    head.push(0); // Channel Mapping Family
    head
}

/// 音声 LOC プロパティを構築する
///
/// `timescale` は入力ファイル由来のメディア時刻を送るときだけ渡す。live capture は
/// Unix epoch マイクロ秒を送るため `None` にし、`PROP_TIMESCALE` を付けない
/// (draft-ietf-moq-loc-04 §2.3.1.1 の既定)。
///
/// `audio_config` に OpusHead バイト列を渡すと Audio Config プロパティ (0x0F) として付与する。
/// Opus は全フレームが独立してデコード可能なため、最初の audio object だけに付与すればよい。
fn build_audio_loc_properties(
    timestamp: u64,
    timescale: Option<u64>,
    audio_config: Option<&[u8]>,
) -> LocProperties {
    let mut props = LocProperties::new();
    props.push(LocProperty {
        prop_id: PROP_TIMESTAMP,
        value: LocPropertyValue::VarInt(timestamp),
    });
    if let Some(timescale) = timescale {
        props.push(LocProperty {
            prop_id: PROP_TIMESCALE,
            value: LocPropertyValue::VarInt(timescale),
        });
    }
    if let Some(config) = audio_config {
        props.push(LocProperty {
            prop_id: PROP_AUDIO_CONFIG,
            value: LocPropertyValue::Bytes(config.to_vec()),
        });
    }
    props
}

/// `AudioFrameOwned` から i16 PCM を取り出す
///
/// デバイス由来のフォーマットは `S16` または `F32`。`F32` の場合は
/// `[-1.0, 1.0]` を `i16` フルスケールに線形マッピングする。
fn extract_pcm_i16(frame: &AudioFrameOwned) -> Result<Vec<i16>> {
    if let Some(s) = frame.as_s16() {
        return Ok(s.to_vec());
    }
    if let Some(f) = frame.as_f32() {
        let mut out = Vec::with_capacity(f.len());
        for &v in f {
            let s = (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            out.push(s);
        }
        return Ok(out);
    }
    Err(Error::Other(
        "audio frame format is neither S16 nor F32".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    // セッション終了の判定は `Error` の variant で行うため、`TransportError` はテストでのみ使う
    use tokio_moq::error::TransportError;

    /// Session の reset 指示が open 中の subgroup stream に一致するかを判定できること
    ///
    /// `SubgroupWriter` は I/O ハンドルを必要とするためテストから構築できないが、
    /// 判定は `Option<&SubgroupWriter>` の `None` 側で固定できる。
    /// 一致する場合の判定は実機確認 (delivery timeout の発火) で確認する。
    #[test]
    fn reset_targets_open_subgroup_without_open_stream() {
        let stream_id = DataStreamId(7);
        assert!(
            !reset_targets_open_subgroup(None, stream_id),
            "open 中の stream が無ければ通知しないこと"
        );
    }

    /// keyframe の映像 LOC プロパティ: Video Frame Marking / Timestamp / Timescale / Video Config が付与され encode できること
    ///
    /// MP4 の経路では入力ファイルのメディア時刻を入力トラックの timescale で送るため、
    /// Timescale を載せる。
    #[test]
    fn build_video_loc_properties_keyframe() {
        let frame = EncodedFrame {
            data: vec![0xAA, 0xBB],
            is_keyframe: true,
            timestamp: 3000,
            video_config: Some(vec![0x01, 0x02]),
        };
        let props = build_video_loc_properties(&frame, Some(90_000));
        // RFC 9626 §3.2: keyframe → S=1, E=1, I=1 → 0xE0
        assert_eq!(
            props.video_frame_marking(),
            Some([0xE0u8].as_slice()),
            "keyframe の Video Frame Marking は [0xE0] であること"
        );
        assert_eq!(props.timestamp(), Some(3000), "Timestamp が付与されること");
        assert_eq!(
            props.timescale(),
            Some(90_000),
            "MP4 の経路では keyframe に Timescale が付与されること"
        );
        assert_eq!(
            props.video_config(),
            Some([0x01u8, 0x02].as_slice()),
            "keyframe では Video Config が付与されること"
        );
        props.encode().expect("LOC プロパティは encode できること");
    }

    /// live capture の映像 LOC プロパティ: keyframe でも Timescale が付与されないこと
    ///
    /// LOC は Timescale が無ければ Timestamp を Unix epoch マイクロ秒として扱う
    /// (draft-ietf-moq-loc-04 §2.3.1.1)。delta frame を単体で見ても同じ解釈になる。
    #[test]
    fn build_video_loc_properties_live_capture_has_no_timescale() {
        let frame = EncodedFrame {
            data: vec![0xAA, 0xBB],
            is_keyframe: true,
            timestamp: 1_700_000_000_000_000,
            video_config: Some(vec![0x01, 0x02]),
        };
        let props = build_video_loc_properties(&frame, None);
        assert_eq!(
            props.timestamp(),
            Some(1_700_000_000_000_000),
            "Timestamp が付与されること"
        );
        assert_eq!(
            props.timescale(),
            None,
            "live capture では keyframe でも Timescale を付与しないこと"
        );
        props.encode().expect("LOC プロパティは encode できること");
    }

    /// 非 keyframe の映像 LOC プロパティ: Video Frame Marking と Timestamp のみが付与され encode できること
    #[test]
    fn build_video_loc_properties_non_keyframe() {
        let frame = EncodedFrame {
            data: vec![0xCC],
            is_keyframe: false,
            timestamp: 3033,
            video_config: None,
        };
        let props = build_video_loc_properties(&frame, Some(90_000));
        // RFC 9626 §3.2: 非 keyframe → S=1, E=1, I=0 → 0xC0
        assert_eq!(
            props.video_frame_marking(),
            Some([0xC0u8].as_slice()),
            "非 keyframe の Video Frame Marking は [0xC0] であること"
        );
        assert_eq!(props.timestamp(), Some(3033), "Timestamp が付与されること");
        assert_eq!(
            props.timescale(),
            None,
            "非 keyframe では Timescale は付与されないこと"
        );
        assert_eq!(
            props.video_config(),
            None,
            "非 keyframe では Video Config は付与されないこと"
        );
        props.encode().expect("LOC プロパティは encode できること");
    }

    /// OpusHead 構築: RFC 7845 §5.1 (Identification Header) のフォーマットで 19 バイトのパケットが生成されること
    #[test]
    fn build_opus_head_format() {
        let head = build_opus_head(48_000, 1);
        // 全 19 バイトであること
        assert_eq!(head.len(), 19, "OpusHead は 19 バイトであること");
        // Magic: "OpusHead"
        assert_eq!(&head[0..8], b"OpusHead", "Magic は OpusHead であること");
        // Version: 1
        assert_eq!(head[8], 1, "Version は 1 であること");
        // Channel Count
        assert_eq!(head[9], 1, "Channel Count は 1 であること");
        // Pre-skip: 0 (little-endian)
        assert_eq!(&head[10..12], &[0x00, 0x00], "Pre-skip は 0 であること");
        // Input Sample Rate: 48000 (little-endian)
        assert_eq!(
            &head[12..16],
            &48_000u32.to_le_bytes(),
            "Input Sample Rate は 48000 であること"
        );
        // Output Gain: 0 (little-endian)
        assert_eq!(&head[16..18], &[0x00, 0x00], "Output Gain は 0 であること");
        // Channel Mapping Family: 0
        assert_eq!(head[18], 0, "Channel Mapping Family は 0 であること");
    }

    /// Audio Config 付きの音声 LOC プロパティ: Timestamp / Timescale / Audio Config が付与され encode できること
    ///
    /// MP4 の経路では入力ファイルのメディア時刻を入力トラックの timescale で送る。
    #[test]
    fn build_audio_loc_properties_with_audio_config() {
        let opus_head = build_opus_head(48_000, 1);
        let props = build_audio_loc_properties(960, Some(48_000), Some(&opus_head));
        assert_eq!(props.timestamp(), Some(960), "Timestamp が付与されること");
        assert_eq!(
            props.timescale(),
            Some(48_000),
            "MP4 の経路では Timescale が付与されること"
        );
        // Audio Config が OpusHead バイト列であること
        assert_eq!(
            props.audio_config(),
            Some(opus_head.as_slice()),
            "Audio Config に OpusHead が付与されること"
        );
        props.encode().expect("LOC プロパティは encode できること");
    }

    /// Audio Config なしの音声 LOC プロパティ: Timestamp / Timescale のみが付与され encode できること
    #[test]
    fn build_audio_loc_properties_without_audio_config() {
        let props = build_audio_loc_properties(1920, Some(48_000), None);
        assert_eq!(props.timestamp(), Some(1920), "Timestamp が付与されること");
        assert_eq!(
            props.timescale(),
            Some(48_000),
            "Timescale が付与されること"
        );
        assert_eq!(
            props.audio_config(),
            None,
            "Audio Config は付与されないこと"
        );
        props.encode().expect("LOC プロパティは encode できること");
    }

    /// live capture の音声 LOC プロパティ: Timestamp のみで Timescale が付与されないこと
    ///
    /// LOC は Timescale が無ければ Timestamp を Unix epoch マイクロ秒として扱う
    /// (draft-ietf-moq-loc-04 §2.3.1.1)。
    #[test]
    fn build_audio_loc_properties_live_capture_has_no_timescale() {
        let props = build_audio_loc_properties(1_700_000_000_000_000, None, None);
        assert_eq!(
            props.timestamp(),
            Some(1_700_000_000_000_000),
            "Timestamp が付与されること"
        );
        assert_eq!(
            props.timescale(),
            None,
            "live capture では Timescale を付与しないこと"
        );
        props.encode().expect("LOC プロパティは encode できること");
    }

    /// 残差の要約が、トラックごとの最小値と中央値を返すこと
    #[test]
    fn capture_clock_residuals_summarize_both_tracks() {
        let mut residuals = CaptureClockResiduals::new();
        // 要約を出す間隔の中では返さない
        assert!(
            residuals
                .record(CaptureTrack::Video, 30_000, 1_000_000)
                .is_none()
        );
        assert!(
            residuals
                .record(CaptureTrack::Audio, 10_000, 2_000_000)
                .is_none()
        );
        assert!(
            residuals
                .record(CaptureTrack::Video, 20_000, 3_000_000)
                .is_none()
        );
        assert!(
            residuals
                .record(CaptureTrack::Video, 40_000, 4_000_000)
                .is_none()
        );
        // 5 秒ぶん経過すると、映像は 4 件の中央値、音声は 1 件の値になる
        let [video, audio] = residuals
            .record(CaptureTrack::Video, 50_000, 6_000_000)
            .expect("間隔が空けば要約を返すこと");
        assert_eq!(video.min_us, 20_000, "映像の最小値");
        assert_eq!(video.median_us, 40_000, "映像の中央値");
        assert_eq!(video.samples, 4, "映像の件数");
        assert_eq!(audio.min_us, 10_000, "音声の最小値");
        assert_eq!(audio.median_us, 10_000, "音声の中央値");
        assert_eq!(audio.samples, 1, "音声の件数");
    }

    /// 片方のトラックしか観測していなければ要約を出さないこと
    #[test]
    fn capture_clock_residuals_need_both_tracks() {
        let mut residuals = CaptureClockResiduals::new();
        assert!(
            residuals
                .record(CaptureTrack::Video, 30_000, 10_000_000)
                .is_none(),
            "音声が無ければ要約を出さないこと"
        );
        assert!(
            residuals
                .record(CaptureTrack::Audio, 10_000, 16_000_000)
                .is_some(),
            "両方そろえば要約を出すこと"
        );
    }

    /// 音声の補正の統計をミリ秒へ換算してログに出せること
    #[test]
    fn format_audio_offset_stats_converts_to_millis() {
        let stats = AudioTimestampOffsetStats {
            current_us: 310_000,
            min_us: 305_000,
            max_us: 810_000,
            slope_10s_us_per_second: Some(50_000),
            slope_60s_us_per_second: None,
            applied_us: Some(805_000),
            samples: 42,
        };
        let text = format_audio_offset_stats(Some(stats));
        assert!(
            text.contains("current=310.0ms"),
            "現在値がミリ秒で出ること: {text}"
        );
        assert!(
            text.contains("min=305.0ms"),
            "最小値がミリ秒で出ること: {text}"
        );
        assert!(
            text.contains("max=810.0ms"),
            "最大値がミリ秒で出ること: {text}"
        );
        assert!(
            text.contains("applied=805.0ms"),
            "適用中の補正がミリ秒で出ること: {text}"
        );
        assert!(
            text.contains("slope_10s=50.0"),
            "傾きがミリ秒 / 秒で出ること: {text}"
        );
        assert!(
            text.contains("slope_60s=none"),
            "傾きが求まらなければ none と出ること: {text}"
        );
        assert!(text.contains("samples=42"), "サンプル数が出ること: {text}");
    }

    /// 音声の補正の統計が無いときは none と出すこと
    #[test]
    fn format_audio_offset_stats_without_observation() {
        assert_eq!(
            format_audio_offset_stats(None),
            "none",
            "観測が無ければ none と出すこと"
        );
    }

    /// capture のメディア時刻を epoch マイクロ秒へ換算できること
    ///
    /// fake capture のメディア時刻は 0 起点であり、読んだ壁時計を基準にした対応で写る。
    #[test]
    fn map_capture_timestamp_converts_zero_based_media_time() {
        let mut mapper = WallClockMapper::new();
        let wall_clock_us = 1_700_000_000_000_000;
        let mut residuals = CaptureClockResiduals::new();
        let timestamp_us = map_capture_timestamp_us(&mut mapper, &mut residuals, 0, wall_clock_us)
            .expect("換算できること");
        assert_eq!(
            timestamp_us, wall_clock_us as u64,
            "0 起点のメディア時刻が読んだ壁時計へ写ること"
        );

        // 同じ間隔で進む次のフレームも壁時計に追随すること
        let timestamp_us =
            map_capture_timestamp_us(&mut mapper, &mut residuals, 33_333, wall_clock_us + 33_333)
                .expect("換算できること");
        assert_eq!(
            timestamp_us,
            (wall_clock_us + 33_333) as u64,
            "次のフレームも読んだ壁時計へ写ること"
        );
    }

    /// 起源の違う音声と映像の Timestamp が同じ epoch マイクロ秒軸に載ること
    ///
    /// capture のメディア時刻の起源は経路ごとに違う (mach 絶対時刻 / monotonic / 0 起点)。
    /// トラックごとに別の時計を持ち、それぞれフレームを読んだ壁時計へ写すため、
    /// 起源が違っても同じ epoch マイクロ秒軸に載る。
    #[test]
    fn video_and_audio_timestamps_share_the_epoch_axis() {
        let base_us: i64 = 1_700_000_000_000_000;
        let mut video_clock = WallClockMapper::new();
        let mut audio_clock = AudioTimestampClock::new();
        let mut residuals = CaptureClockResiduals::new();

        // 映像は 0 起点のメディア時刻、音声は起動から 5 秒進んだ monotonic なメディア時刻
        let mut video_timestamps = Vec::new();
        let mut audio_timestamps = Vec::new();
        for index in 0..30i64 {
            let video_media_us = index * 33_333;
            video_timestamps.push(
                map_capture_timestamp_us(
                    &mut video_clock,
                    &mut residuals,
                    video_media_us,
                    base_us + video_media_us,
                )
                .expect("換算できること"),
            );
            let audio_media_us = 5_000_000 + index * 20_000;
            audio_timestamps.push(
                map_audio_capture_timestamp_us(
                    &mut audio_clock,
                    &mut residuals,
                    audio_media_us,
                    base_us + index * 20_000,
                )
                .expect("換算できること"),
            );
        }

        assert!(
            video_timestamps.windows(2).all(|pair| pair[0] < pair[1]),
            "映像の Timestamp が単調に増加すること"
        );
        assert!(
            audio_timestamps.windows(2).all(|pair| pair[0] < pair[1]),
            "音声の Timestamp が単調に増加すること"
        );
        // 同じ時点のフレームが同じ epoch マイクロ秒になること
        assert_eq!(
            (video_timestamps[0], audio_timestamps[0]),
            (base_us as u64, base_us as u64),
            "起源の違う 2 トラックが同じ時点から始まること"
        );
        for (index, timestamp_us) in video_timestamps.iter().enumerate() {
            assert_eq!(
                *timestamp_us,
                (base_us + index as i64 * 33_333) as u64,
                "映像の Timestamp が読んだ壁時計に一致すること: index={index}"
            );
        }
        for (index, timestamp_us) in audio_timestamps.iter().enumerate() {
            assert_eq!(
                *timestamp_us,
                (base_us + index as i64 * 20_000) as u64,
                "音声の Timestamp が読んだ壁時計に一致すること: index={index}"
            );
        }
    }

    /// 読み取りの遅れが小さいフレームに合わせて対応が収束すること
    ///
    /// 1 フレーム目を大きく遅れて読んでも、遅れの小さいフレームが届くにつれて換算した
    /// Timestamp が読んだ壁時計へ一致していく。収束の途中でも前のフレームより戻らない。
    #[test]
    fn map_capture_timestamp_converges_to_wall_clock() {
        let mut mapper = WallClockMapper::new();
        let base = 1_700_000_000_000_000;
        let frame_interval_us: u64 = 33_333;
        // 1 フレーム目は 100 ms 遅れて読んだものとする
        let mut residuals = CaptureClockResiduals::new();
        let mut previous = map_capture_timestamp_us(&mut mapper, &mut residuals, 0, base + 100_000)
            .expect("換算できること");
        assert_eq!(
            previous,
            (base + 100_000) as u64,
            "最初は記録した対応をそのまま使うこと"
        );

        // 以降は 10 ms 遅れで読み続ける (対応の目標が 10 ms 遅れへ小さくなる)
        for index in 1..=10u64 {
            let media_us = i64::try_from(index * frame_interval_us).expect("範囲内であること");
            let wall_clock_us = base + 10_000 + media_us;
            let timestamp_us =
                map_capture_timestamp_us(&mut mapper, &mut residuals, media_us, wall_clock_us)
                    .expect("換算できること");
            assert!(
                timestamp_us > previous,
                "収束の途中でも換算した時刻が戻らないこと: index={index}"
            );
            previous = timestamp_us;
            if index == 10 {
                assert_eq!(
                    timestamp_us, wall_clock_us as u64,
                    "対応が収束して読んだ壁時計と一致すること"
                );
            }
        }
        assert_eq!(
            mapper.offset_us(),
            Some(base + 10_000),
            "対応が遅れの最も小さいフレームへ収束すること"
        );
    }

    /// 桁あふれせず、Unix epoch より前を返さないこと
    #[test]
    fn map_audio_capture_timestamp_is_never_negative() {
        let mut clock = AudioTimestampClock::new();
        let mut residuals = CaptureClockResiduals::new();
        let timestamp_us =
            map_audio_capture_timestamp_us(&mut clock, &mut residuals, -10_000, -5_000)
                .expect("換算できること");
        assert_eq!(timestamp_us, 0, "Unix epoch より前は 0 にすること");
    }

    /// サンプル数をマイクロ秒へ換算できること
    #[test]
    fn samples_to_micros_converts_sample_counts() {
        assert_eq!(samples_to_micros(960, 48_000), 20_000, "20 ms になること");
        assert_eq!(samples_to_micros(0, 48_000), 0, "0 サンプルは 0 であること");
        assert_eq!(
            samples_to_micros(1, 48_000),
            20,
            "端数は切り捨てること (48 kHz の 1 サンプルは 20.8 us)"
        );
    }

    /// timescale 単位のメディア時刻をマイクロ秒へ換算できること
    #[test]
    fn media_time_to_micros_converts_media_time() {
        assert_eq!(
            media_time_to_micros(90_000, 90_000),
            1_000_000,
            "1 秒になること"
        );
        assert_eq!(media_time_to_micros(0, 48_000), 0, "0 は 0 であること");
        assert_eq!(
            media_time_to_micros(u64::MAX, 1),
            u64::MAX,
            "桁あふれは飽和させること"
        );
    }

    /// 1 つの capture フレームから複数の Object を切り出すと Timestamp が進むこと
    ///
    /// バッファ先頭の時刻から切り出し済みサンプル数ぶん進めるため、同じ capture
    /// フレーム由来の複数 Object が同じ Timestamp にならない。
    #[test]
    fn audio_timeline_advances_timestamps_within_a_capture_frame() {
        let mut timeline = AudioCaptureTimeline::new();
        let head_epoch_us = 1_700_000_000_000_000;
        timeline.observe_frame(head_epoch_us, 0, 48_000);

        let timestamps: Vec<u64> = (0..3)
            .map(|_| timeline.next_chunk_timestamp_us(960, 48_000))
            .collect();
        assert_eq!(
            timestamps,
            vec![
                head_epoch_us,
                head_epoch_us + 20_000,
                head_epoch_us + 40_000
            ],
            "20 ms ずつ進むこと"
        );
        assert!(
            timestamps.windows(2).all(|pair| pair[0] < pair[1]),
            "Timestamp が単調に増加すること"
        );
    }

    /// バッファに残ったサンプルの先頭時刻を capture フレームの時刻から逆算すること
    #[test]
    fn audio_timeline_derives_head_from_buffered_samples() {
        let mut timeline = AudioCaptureTimeline::new();
        // 前のフレームのサンプルが 480 (10 ms) 残っている状態で次のフレームが届く
        let frame_epoch_us = 1_700_000_000_020_000;
        timeline.observe_frame(frame_epoch_us, 480, 48_000);
        assert_eq!(
            timeline.next_chunk_timestamp_us(960, 48_000),
            frame_epoch_us - 10_000,
            "バッファ先頭はフレームの時刻から残りサンプル数ぶん前になること"
        );
        assert_eq!(
            timeline.next_chunk_timestamp_us(960, 48_000),
            frame_epoch_us + 10_000,
            "切り出したぶんだけ次のチャンクが進むこと"
        );
    }

    /// capture フレームを取りこぼしてもバッファ先頭の時刻が壁時計に追随すること
    ///
    /// 蓄積サンプル数だけで進めると取りこぼしたぶんがずれ続けるため、フレームを
    /// 受け取るたびに基準を取り直す。
    #[test]
    fn audio_timeline_follows_wall_clock_across_dropped_frames() {
        let mut timeline = AudioCaptureTimeline::new();
        let first_epoch_us = 1_700_000_000_000_000;
        timeline.observe_frame(first_epoch_us, 0, 48_000);
        assert_eq!(
            timeline.next_chunk_timestamp_us(960, 48_000),
            first_epoch_us,
            "最初のフレームはその時刻で切り出すこと"
        );

        // 200 ms ぶんのフレームを取りこぼして次のフレームが届く
        let second_epoch_us = first_epoch_us + 200_000;
        timeline.observe_frame(second_epoch_us, 0, 48_000);
        assert_eq!(
            timeline.next_chunk_timestamp_us(960, 48_000),
            second_epoch_us,
            "取りこぼしたぶんを蓄積せず、届いたフレームの時刻で切り出すこと"
        );
    }

    /// `TransportError` の接続クローズだけをセッション終了として扱う
    ///
    /// MoqtClient の後始末 API は `Error` ではなく `TransportError` を返すため、`Error` 側の
    /// 判定 (`is_transport_session_end`) とは別に variant を固定する。接続クローズ以外は
    /// 異常として warn で出す。
    #[test]
    fn is_connection_closed_only_matches_connection_closed() {
        assert!(
            is_connection_closed(&TransportError::ConnectionClosed),
            "接続クローズはセッション終了として扱うこと"
        );

        for error in [
            TransportError::StreamClosed,
            TransportError::Quic("connection failed".to_string()),
            TransportError::ConnectFailed { status: Some(404) },
            TransportError::ProtocolNegotiationFailed {
                error_code: shiguredo_http3::webtransport::ErrorCode::AlpnError as u64,
            },
            TransportError::InvalidState("invalid state".to_string()),
            TransportError::Internal("internal".to_string()),
        ] {
            assert!(
                !is_connection_closed(&error),
                "接続クローズ以外はセッション終了として扱わないこと: {error}"
            );
        }
    }

    /// 接続クローズだけをセッション終了として扱い、他のエラーは異常として扱う
    ///
    /// セッション終了として扱わないエラーは `?` で `run` を抜ける (終了コード 1)。
    /// 判定は `Error` の variant で行うため、`WebTransport` に畳まれる他の transport エラーが
    /// 混ざらないことを固定する。
    #[test]
    fn is_transport_session_end_only_matches_connection_closed() {
        assert!(
            is_transport_session_end(&Error::from(TransportError::ConnectionClosed)),
            "接続クローズはセッション終了として扱うこと"
        );

        for error in [
            TransportError::StreamClosed,
            TransportError::Quic("connection failed".to_string()),
            TransportError::ConnectFailed { status: Some(404) },
            // プロトコル交渉の失敗は通信の前提が揃わなかった異常終了であり、セッション終了ではない
            TransportError::ProtocolNegotiationFailed {
                error_code: shiguredo_http3::webtransport::ErrorCode::AlpnError as u64,
            },
            TransportError::InvalidState("invalid state".to_string()),
            TransportError::Internal("internal".to_string()),
        ] {
            let error = Error::from(error);
            assert!(
                !is_transport_session_end(&error),
                "transport の他のエラーはセッション終了として扱わないこと: {error}"
            );
        }

        for error in [
            // `WebTransport` に畳まれるエラーはセッション終了ではない
            Error::WebTransport("stream closed".to_string()),
            Error::Other("other".to_string()),
            Error::Io(std::io::Error::other("io failed")),
            Error::Moqt(shiguredo_moqt::error::MessageError::UnexpectedEof),
        ] {
            assert!(
                !is_transport_session_end(&error),
                "transport 以外のエラーはセッション終了として扱わないこと: {error}"
            );
        }
    }

    /// peer のストリーム終端だけを `is_peer_stream_reset` が真とする
    ///
    /// 該当ストリームだけの終端でありセッションは継続するため、pipeline は致命エラーに
    /// しない。セッション終了 (`ConnectionClosed`) は別扱いのままであることも固定する。
    #[test]
    fn is_peer_stream_reset_only_matches_stream_reset() {
        assert!(
            is_peer_stream_reset(&Error::from(TransportError::StreamReset {
                error_code: 0x1
            })),
            "peer のストリーム終端は真とすること"
        );
        for error in [
            Error::from(TransportError::ConnectionClosed),
            Error::from(TransportError::Quic("connection failed".to_string())),
            Error::Other("other".to_string()),
        ] {
            assert!(
                !is_peer_stream_reset(&error),
                "ストリーム終端以外は偽とすること: {error}"
            );
        }
    }

    /// テスト用のエンコード済みフレームを構築する
    fn test_encoded_frame(timestamp: u64) -> EncodedFrame {
        EncodedFrame {
            data: vec![0x01],
            is_keyframe: false,
            timestamp,
            video_config: None,
        }
    }

    /// encode / decode 失敗がセッション終了コードへ写ること
    ///
    /// draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) の
    /// KEY_VALUE_FORMATTING_ERROR (0x6) と PROTOCOL_VIOLATION (0x3) を使い分ける。
    #[test]
    fn session_error_code_maps_message_failures() {
        assert_eq!(
            session_error_code(&MessageError::KeyValueFormattingError(
                "test formatting error"
            )),
            SESSION_KEY_VALUE_FORMATTING_ERROR,
            "書式違反は KEY_VALUE_FORMATTING_ERROR (0x6) であること"
        );
        assert_eq!(
            session_error_code(&MessageError::ProtocolViolation("test violation")),
            SESSION_PROTOCOL_VIOLATION,
            "プロトコル違反は PROTOCOL_VIOLATION (0x3) であること"
        );
        // メッセージを持たない失敗も PROTOCOL_VIOLATION として扱う
        assert_eq!(
            session_error_code(&MessageError::UnexpectedEof),
            SESSION_PROTOCOL_VIOLATION,
            "切り詰めは PROTOCOL_VIOLATION (0x3) であること"
        );
    }

    /// 再エンコードの入力 PTS が出力フレームへ引き継がれること
    ///
    /// 0 フレームを返した入力の PTS は次に出力されたフレームへ割り当てる。
    #[test]
    fn assign_input_timestamps_follows_input_pts() {
        let mut pending = VecDeque::new();
        assign_input_timestamps(&mut pending, Some(1_000), Some(48_000), &mut []);
        assert_eq!(pending.len(), 1, "0 フレームでも PTS が残ること");

        let mut frames = vec![test_encoded_frame(10), test_encoded_frame(20)];
        assign_input_timestamps(&mut pending, Some(2_000), Some(48_000), &mut frames);
        assert_eq!(frames[0].timestamp, 1_000, "引き継いだ PTS が使われること");
        assert_eq!(frames[1].timestamp, 2_000, "入力の PTS が使われること");
        assert!(pending.is_empty(), "割り当て後は空になること");

        // 入力タイムスタンプが無く、送る単位も encoder のものであれば、採番を保持する
        let mut frames = vec![test_encoded_frame(30)];
        assign_input_timestamps(&mut pending, None, None, &mut frames);
        assert_eq!(frames[0].timestamp, 30, "エンコーダの採番が残ること");
    }

    // 入力より出力が多いときは、余ったフレームを encoder のマイクロ秒から入力の単位へ戻す
    #[test]
    fn assign_input_timestamps_converts_extra_frames_to_the_input_timescale() {
        let mut pending = VecDeque::new();
        // 入力 1 つに対して 2 フレームが出た。encoder は先頭にマイクロ秒 (2_000)、
        // 次に 1 フレーム間隔ぶん (35_000 = 29_000 + 6_000) を付ける
        let mut frames = vec![test_encoded_frame(2_000), test_encoded_frame(35_000)];
        assign_input_timestamps(&mut pending, Some(200), Some(1_000_000), &mut frames);
        assert_eq!(frames[0].timestamp, 200, "入力の PTS が使われること");
        assert_eq!(
            frames[1].timestamp, 35_000,
            "timescale が 1_000_000 ならマイクロ秒と同じ値になること"
        );

        // 48 kHz なら 35_000 マイクロ秒は 1_680 サンプル
        let mut pending = VecDeque::new();
        let mut frames = vec![test_encoded_frame(2_000), test_encoded_frame(35_000)];
        assign_input_timestamps(&mut pending, Some(200), Some(48_000), &mut frames);
        assert_eq!(frames[0].timestamp, 200, "入力の PTS が使われること");
        assert_eq!(
            frames[1].timestamp, 1_680,
            "余ったフレームを入力の単位へ戻すこと"
        );
    }

    // live capture とエンコード済みサンプルの経路では単位を変えない
    #[test]
    fn assign_input_timestamps_keeps_the_unit_without_a_timescale() {
        let mut pending = VecDeque::new();
        let mut frames = vec![test_encoded_frame(1_700_000_000_000_000)];
        assign_input_timestamps(&mut pending, None, None, &mut frames);
        assert_eq!(
            frames[0].timestamp, 1_700_000_000_000_000,
            "epoch マイクロ秒のまま送ること"
        );
    }

    // マイクロ秒から入力の単位へ戻す換算
    #[test]
    fn media_time_from_micros_converts_and_saturates() {
        assert_eq!(
            media_time_from_micros(1_000_000, 48_000),
            48_000,
            "1 秒は timescale ぶんの値になること"
        );
        assert_eq!(
            media_time_from_micros(20_000, 48_000),
            960,
            "20 ms は 960 サンプルになること"
        );
        assert_eq!(
            media_time_from_micros(u64::MAX, u64::MAX),
            u64::MAX,
            "u64 に収まらない値は飽和すること"
        );
    }
}
