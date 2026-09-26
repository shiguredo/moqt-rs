//! publisher の全体パイプライン
//!
//! カメラ/マイクキャプチャ → 映像/音声エンコード → MoQT 送信の流れを、
//! sans-I/O な `shiguredo_moqt::session::core::Session` を駆動する
//! [`moqt_example_transport::moqt_client::MoqtClient`] と結線する。

use std::time::Instant;

use shiguredo_audio_device::AudioFrameOwned;
use shiguredo_moqt::{
    error::{REQUEST_DOES_NOT_EXIST, REQUEST_NOT_SUPPORTED},
    loc::{
        LocProperties, LocProperty, LocPropertyValue, PROP_AUDIO_CONFIG, PROP_TIMESCALE,
        PROP_TIMESTAMP, PROP_VIDEO_CONFIG, PROP_VIDEO_FRAME_MARKING,
    },
    message::ControlMessage,
    message::common::Location,
    message::common::TrackNamespace,
    message_parameter::MessageParameters,
    msf::MSF_CATALOG_TRACK_NAME,
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
use crate::stream_writer::SubgroupWriter;
use moqt_example_transport::Transport;
use moqt_example_transport::host_from_authority;
use moqt_example_transport::moqt_client::{
    ClientEvent, IncomingRequest, MoqtClient, ObjectFilterOutcome,
};
use moqt_example_transport::quic;
use moqt_example_transport::resolve_socket_addr;

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

/// カタログトラックの Track Alias
const CATALOG_TRACK_ALIAS: u64 = 0;
/// 映像トラックの Track Alias
const VIDEO_TRACK_ALIAS: u64 = 1;
/// 音声トラックの Track Alias
const AUDIO_TRACK_ALIAS: u64 = 2;
/// デフォルトの Publisher Priority
const DEFAULT_PUBLISHER_PRIORITY: u8 = 128;
/// PUBLISH_DONE の Status Code: GOING_AWAY (draft-ietf-moq-transport-21 §16.11.3 (PUBLISH_DONE Codes))
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

/// 停滞の切り分け用の診断ログを出すかどうか。
///
/// `MOQT_PUBLISH_DIAG=1` を設定したときだけ有効にする。100ms ごとに
/// publish ループの進み具合を出すため、常時有効にすると計測そのものが結果を歪める。
fn publish_diag_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("MOQT_PUBLISH_DIAG").is_ok_and(|v| v == "1"))
}

/// パイプラインを実行する
///
/// `task_monitor` は生成されるタスクのメトリクスを収集する。
/// `shutdown_monitor` は graceful shutdown の契機を受信する。
pub async fn run(
    config: Config,
    task_monitor: tokio_metrics::TaskMonitor,
    mut shutdown_monitor: tokio_utils::ShutdownMonitor,
) -> Result<()> {
    // TLS の SNI / 証明書検証に使う server_name はポートを含めない。
    // IPv6 リテラル (`[::1]:4443` 等) でも正しく host を取り出す。
    let server_name = host_from_authority(&config.url.authority);

    // 接続先の解決は QUIC / WebTransport で共通にする。ポートが省略された URL は
    // 既定ポート 443 を使い、ホスト名は名前解決する (draft-ietf-moq-transport-21 §6.1.2)。
    let socket_addr = resolve_socket_addr(&config.url.authority).await?;

    // 1. 接続確立と SETUP ハンドシェイク
    let mut client = match config.url.transport {
        Transport::Quic => {
            let connection =
                quic::connect(socket_addr, server_name, config.cert.as_deref()).await?;
            let (client, _acceptor) = MoqtClient::establish_quic(
                connection,
                &config.url.path,
                &config.url.authority,
                "moqt-publisher",
                &task_monitor,
            )
            .await?;
            client
        }
        Transport::WebTransport => {
            let mut client_config =
                moqt_example_transport::webtransport::ClientConfig::new(socket_addr, server_name)
                    // :authority は target URI の authority を URL の表記どおりに渡す (draft-ietf-webtrans-http3-16 §3.2)
                    .authority(&config.url.authority)
                    .enable_webtransport(
                        shiguredo_http3::webtransport::Settings::new()
                            .wt_enabled(shiguredo_http3::VarInt::from_static(1)),
                    );
            if let Some(ref cert) = config.cert {
                let pem = std::fs::read_to_string(cert)?;
                client_config = client_config.ca_cert(pem);
            } else {
                // 開発用: 証明書検証をスキップする (QUIC 経路と対称の警告)
                tracing::warn!("TLS certificate verification is disabled (development mode)");
                client_config = client_config.insecure();
            }
            let wt_session = moqt_example_transport::webtransport::WtClient::connect(
                client_config,
                &config.url.path,
            )
            .await?;
            let (client, _acceptor) =
                MoqtClient::establish_wt(wt_session, "moqt-publisher", &task_monitor).await?;
            client
        }
    };

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
    let namespace = TrackNamespace::new(vec![config.namespace.as_bytes().to_vec()])?;
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

    // 3. エンコーダを先に生成し、catalog の codec 文字列を encoder から取得する
    let mut video_encoder: Option<encoder::VideoEncoder> = if config.video_enabled {
        let enc = match config.video_codec {
            VideoCodec::Av1 => encoder::VideoEncoder::Av1(Box::new(encoder::av1::Av1Encoder::new(
                config.width,
                config.height,
                config.fps,
                config.bitrate,
                config.keyframe_interval,
            )?)),
            #[cfg(target_os = "macos")]
            VideoCodec::H264 => encoder::VideoEncoder::H264(encoder::h264::H264Encoder::new(
                config.width,
                config.height,
                config.fps,
                config.bitrate,
                config.keyframe_interval,
            )?),
            #[cfg(target_os = "macos")]
            VideoCodec::H265 => encoder::VideoEncoder::H265(encoder::h265::H265Encoder::new(
                config.width,
                config.height,
                config.fps,
                config.bitrate,
                config.keyframe_interval,
            )?),
            #[cfg(not(target_os = "macos"))]
            VideoCodec::H264 => {
                return Err(Error::Other(
                    "H.264 encoder is only available on macOS; rebuild on macOS or specify --video-codec av1".to_string(),
                ));
            }
            #[cfg(not(target_os = "macos"))]
            VideoCodec::H265 => {
                return Err(Error::Other(
                    "H.265 encoder is only available on macOS; rebuild on macOS or specify --video-codec av1".to_string(),
                ));
            }
        };
        Some(enc)
    } else {
        None
    };
    let video_timescale = video_encoder.as_ref().map(|e| e.timescale());

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
    let audio_timescale = audio_encoder.as_ref().map(|e| e.timescale());
    // Opus の configuration bytes (OpusHead) を事前に構築する
    // Audio Config プロパティとして最初の audio object にだけ付与する
    let audio_opus_head = audio_encoder
        .as_ref()
        .map(|_| build_opus_head(AUDIO_SAMPLE_RATE, AUDIO_CHANNELS));

    // 4. MSF カタログを送信する
    let handle = client.handle();
    let catalog_json = catalog::send_catalog(catalog::CatalogParams {
        handle: &handle,
        data_plane: &data_plane,
        catalog_request_id,
        catalog_alias: CATALOG_TRACK_ALIAS,
        start_location: client.subscription_filter_start(catalog_request_id),
        video: video_encoder.as_ref().map(|enc| catalog::VideoTrackParams {
            track_name: &config.track_name,
            namespace: &config.namespace,
            codec: enc.catalog_codec_string(),
            width: config.width,
            height: config.height,
            fps: config.fps,
            bitrate: config.bitrate,
        }),
        audio: audio_encoder.as_ref().map(|enc| catalog::AudioTrackParams {
            track_name: AUDIO_TRACK_NAME,
            namespace: &config.namespace,
            codec: enc.catalog_codec_string(),
            samplerate: AUDIO_SAMPLE_RATE,
            channel_config: AUDIO_CHANNEL_CONFIG,
            bitrate: config.audio_bitrate,
        }),
    })
    .await?;

    // 5. キャプチャ起動
    let (video_frame_tx, mut video_frame_rx) = mpsc::channel::<VideoFrameOwned>(4);
    let _video_capture: Option<VideoCaptureGuard> = if config.video_enabled {
        Some(if config.fake_capture_device {
            VideoCaptureGuard::Fake(fake_capture::start_capture(&config, video_frame_tx)?)
        } else {
            VideoCaptureGuard::Real(capture::start_capture(&config, video_frame_tx)?)
        })
    } else {
        // 送信側を drop して受信側を閉じておく (select の分岐は cfg gate でスキップ)
        drop(video_frame_tx);
        None
    };

    let (audio_frame_tx, mut audio_frame_rx) = mpsc::channel::<AudioFrameOwned>(8);
    let _audio_capture: Option<AudioCaptureGuard> = if config.audio_enabled {
        Some(if config.fake_capture_device {
            AudioCaptureGuard::Fake(fake_audio_capture::start_capture(audio_frame_tx)?)
        } else {
            AudioCaptureGuard::Real(audio_capture::start_capture(&config, audio_frame_tx)?)
        })
    } else {
        drop(audio_frame_tx);
        None
    };

    // 6. データループ
    let start = Instant::now();
    let mut video_group_id: u64 = 0;
    let mut audio_group_id: u64 = 0;
    let mut audio_frame_count: u64 = 0;
    let mut current_video_writer: Option<SubgroupWriter> = None;
    let mut current_video_datagram_writer: Option<DatagramWriter> = None;
    let mut audio_pcm_buf: Vec<i16> = Vec::new();
    // Audio Config (OpusHead) を送信済みかどうかのフラグ
    // Opus は全フレームが独立してデコード可能なため、最初の audio object だけでよい
    let mut audio_config_sent = false;
    let mut tick_interval = tokio::time::interval(std::time::Duration::from_millis(100));
    tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    tracing::info!(
        "Starting publish loop (video={}, audio={}, datagram={})",
        config.video_enabled,
        config.audio_enabled,
        config.use_datagram,
    );

    'main: loop {
        tokio::select! {
            video_frame = video_frame_rx.recv(), if config.video_enabled => {
                let Some(video_frame) = video_frame else {
                    tracing::info!("Video capture channel closed");
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
                    let encoder = video_encoder.as_mut().expect("video encoder enabled");
                    let timescale = video_timescale.expect("video timescale enabled");
                    let encoded_frames = encoder.encode(&video_frame)?;
                    for ef in encoded_frames {
                        if ef.is_keyframe {
                            // datagram モードと Subgroup モードの両方で使うため、ここで request_id を
                            // 確定させる (`.expect()` の重複も避ける)
                            let video_request_id =
                                video_request_id.expect("video request_id enabled");
                            if config.use_datagram {
                                // datagram モード: 前の writer は特に finalize しない
                                current_video_datagram_writer = Some(DatagramWriter::new(
                                    &handle,
                                    &data_plane,
                                    video_request_id,
                                    VIDEO_TRACK_ALIAS,
                                    video_group_id,
                                    DEFAULT_PUBLISHER_PRIORITY,
                                ));
                            } else {
                                if let Some(writer) = current_video_writer.take() {
                                    writer.finish(
                                        client.subscription_filter_start(video_request_id),
                                    )?;
                                }
                                // has_properties は SUBGROUP_HEADER の PROPERTIES ビットに対応し、
                                // ヘッダと全オブジェクトで一致が必須 (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header))
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
                            }
                            tracing::debug!("Started new video group {}", video_group_id);
                            video_group_id += 1;
                        }
                        let properties = build_video_loc_properties(&ef, timescale);
                        if config.use_datagram {
                            if let Some(writer) = current_video_datagram_writer.as_mut() {
                                // video はオブジェクト単位で独立のため、Skip の結果は無視してよい
                                let _ = writer.write_object(&ef.data, &properties).await?;
                            }
                        } else {
                            if let Some(writer) = current_video_writer.as_mut() {
                                let _ = writer.write_object(&ef.data, &properties).await?;
                            }
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
                        break 'main;
                    }
                    Err(e) => return Err(e),
                }
            }
            audio_frame = audio_frame_rx.recv(), if config.audio_enabled => {
                let Some(audio_frame) = audio_frame else {
                    tracing::info!("Audio capture channel closed");
                    break;
                };
                // 扱いは video 分岐と同じである。音声は 20 ms ごとに新しいストリームを開くため、
                // セッション終了後に `SubgroupWriter::new` が `ConnectionClosed` を返す経路に
                // 入りやすい。
                let outcome: Result<()> = async {
                    let encoder = audio_encoder.as_mut().expect("audio encoder enabled");
                    let samples_per_frame =
                        audio_samples_per_frame.expect("audio samples_per_frame enabled");
                    let timescale = audio_timescale.expect("audio timescale enabled");
                    let pcm = extract_pcm_i16(&audio_frame)?;
                    audio_pcm_buf.extend_from_slice(&pcm);
                    while audio_pcm_buf.len() >= samples_per_frame {
                        let chunk: Vec<i16> = audio_pcm_buf.drain(..samples_per_frame).collect();
                        let encoded = encoder.encode(&chunk)?;
                        let timestamp = audio_frame_count * samples_per_frame as u64;
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
                            build_audio_loc_properties(timestamp, timescale, audio_config);
                        // LOC draft-ietf-moq-loc-04 §4.1 (Application with one audio track): 1 audio frame = 1 Object = 1 Group
                        if config.use_datagram {
                            let mut writer = DatagramWriter::new(
                                &handle,
                                &data_plane,
                                audio_request_id.expect("audio request_id enabled"),
                                AUDIO_TRACK_ALIAS,
                                audio_group_id,
                                DEFAULT_PUBLISHER_PRIORITY,
                            );
                            let outcome = writer.write_object(&encoded, &properties).await?;
                            if outcome == ObjectFilterOutcome::Pass {
                                audio_config_sent = true;
                            }
                        } else {
                            // has_properties は SUBGROUP_HEADER の PROPERTIES ビットに対応し、
                            // ヘッダと全オブジェクトで一致が必須 (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header))
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
                        break 'main;
                    }
                    Err(e) => return Err(e),
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
                        Some(ClientEvent::Request(request)) => {
                            serve_peer_request(&mut client, request, &config, &catalog_json)
                                .await?;
                            Ok(false)
                        }
                        Some(ClientEvent::RequestUpdate(update)) => {
                            // draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE):
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
                    Ok(true) => break 'main,
                    Ok(false) => {}
                    Err(e) if is_transport_session_end(&e) => {
                        tracing::info!("Session closed by transport");
                        break 'main;
                    }
                    Err(e) => return Err(e),
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
            _ = shutdown_monitor.recv() => {
                tracing::info!("Shutdown signal received");
                break;
            }
        }
    }

    if let Some(writer) = current_video_writer.take() {
        // 終了時点で購読が消えていれば `None` (Location Filter 無し) と同じ扱いになり、
        // 省略があれば RESET 側に倒れる (安全側)
        writer.finish(video_request_id.and_then(|rid| client.subscription_filter_start(rid)))?;
    }
    // datagram writer は明示的な finalize 不要
    drop(current_video_datagram_writer);

    // PUBLISH_DONE を各 request に送信してストリームを閉じる
    if let Some(rid) = video_request_id
        && let Err(e) = client
            .send_publish_done(rid, PUBLISH_DONE_GOING_AWAY, "")
            .await
    {
        tracing::warn!("Failed to send PUBLISH_DONE for video: {e}");
    }
    if let Some(rid) = audio_request_id
        && let Err(e) = client
            .send_publish_done(rid, PUBLISH_DONE_GOING_AWAY, "")
            .await
    {
        tracing::warn!("Failed to send PUBLISH_DONE for audio: {e}");
    }
    if let Err(e) = client
        .send_publish_done(catalog_request_id, PUBLISH_DONE_GOING_AWAY, "")
        .await
    {
        tracing::warn!("Failed to send PUBLISH_DONE for catalog: {e}");
    }

    if let Err(e) = client.send_goaway(Vec::new(), 5000).await {
        tracing::warn!("Failed to send GOAWAY: {e}");
    }

    if let Err(e) = client.close(0, "").await {
        tracing::warn!("Failed to close session gracefully: {e}");
    }

    tracing::info!("Pipeline stopped");
    Ok(())
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

/// MOQT relay が転送してきた要求 (SUBSCRIBE / FETCH) に応答する
///
/// relay は subscriber の SUBSCRIBE / FETCH を publisher 側 session の新しい
/// request stream として転送する (draft-ietf-moq-transport-21 §6.3 (Session initialization))。
/// 本 publisher は video / audio の SUBSCRIBE に SUBSCRIBE_OK を返し
/// (以後の Object は継続して送信中の subgroup stream で届く)、catalog の FETCH には
/// FETCH_OK と FETCH 応答ストリームを返す。
/// 未知の track と未対応の要求種別は REQUEST_ERROR で拒否する
/// (draft-ietf-moq-transport-21 §9.4 (REQUEST_ERROR))。
async fn serve_peer_request(
    client: &mut MoqtClient,
    request: IncomingRequest,
    config: &Config,
    catalog_json: &[u8],
) -> Result<()> {
    let request_id = request.request_id;
    match request.message {
        ControlMessage::Subscribe(subscribe) => {
            let track: &[u8] = &subscribe.track_name;
            let track_alias = if config.video_enabled && track == config.track_name.as_bytes() {
                Some(VIDEO_TRACK_ALIAS)
            } else if config.audio_enabled && track == AUDIO_TRACK_NAME.as_bytes() {
                Some(AUDIO_TRACK_ALIAS)
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
                client
                    .send_fetch_ok(
                        request_id,
                        0,
                        Location {
                            group_id: 0,
                            object_id: 0,
                        },
                        MessageParameters::new(),
                        TrackProperties::new(),
                    )
                    .await?;
                client
                    .send_fetch_response(request_id, 0, 0, catalog_json, None)
                    .await?;
                tracing::info!("Served catalog FETCH (request_id={request_id})");
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

/// 映像 LOC プロパティを構築する
fn build_video_loc_properties(frame: &EncodedFrame, timescale: u64) -> LocProperties {
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
        props.push(LocProperty {
            prop_id: PROP_TIMESCALE,
            value: LocPropertyValue::VarInt(timescale),
        });
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
/// 音声には keyframe 概念が無いため、毎フレームに `PROP_TIMESCALE` を付与する。
/// これにより、subscriber がどの group から再生開始しても `timestamp` を
/// メディア時刻として解釈できる。
///
/// `audio_config` に OpusHead バイト列を渡すと Audio Config プロパティ (0x0F) として付与する。
/// Opus は全フレームが独立してデコード可能なため、最初の audio object だけに付与すればよい。
fn build_audio_loc_properties(
    timestamp: u64,
    timescale: u64,
    audio_config: Option<&[u8]>,
) -> LocProperties {
    let mut props = LocProperties::new();
    props.push(LocProperty {
        prop_id: PROP_TIMESTAMP,
        value: LocPropertyValue::VarInt(timestamp),
    });
    props.push(LocProperty {
        prop_id: PROP_TIMESCALE,
        value: LocPropertyValue::VarInt(timescale),
    });
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
    use moqt_example_transport::error::TransportError;

    /// keyframe の映像 LOC プロパティ: Video Frame Marking / Timestamp / Timescale / Video Config が付与され encode できること
    #[test]
    fn build_video_loc_properties_keyframe() {
        let frame = EncodedFrame {
            data: vec![0xAA, 0xBB],
            is_keyframe: true,
            timestamp: 3000,
            video_config: Some(vec![0x01, 0x02]),
        };
        let props = build_video_loc_properties(&frame, 90_000);
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
            "keyframe では Timescale が付与されること"
        );
        assert_eq!(
            props.video_config(),
            Some([0x01u8, 0x02].as_slice()),
            "keyframe では Video Config が付与されること"
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
        let props = build_video_loc_properties(&frame, 90_000);
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
    #[test]
    fn build_audio_loc_properties_with_audio_config() {
        let opus_head = build_opus_head(48_000, 1);
        let props = build_audio_loc_properties(960, 48_000, Some(&opus_head));
        assert_eq!(props.timestamp(), Some(960), "Timestamp が付与されること");
        assert_eq!(
            props.timescale(),
            Some(48_000),
            "Timescale が付与されること"
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
        let props = build_audio_loc_properties(1920, 48_000, None);
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
}
