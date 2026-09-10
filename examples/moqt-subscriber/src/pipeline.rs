//! subscriber の全体パイプライン
//!
//! 接続 → SETUP → カタログ FETCH → ビデオ SUBSCRIBE → データストリーム受信
//! → AV1 デコード → フレーム送出の流れを、`shiguredo_moqt::session::core::Session` を駆動する
//! [`moqt_example_transport::moqt_client::MoqtClient`] と結線する。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use bytes::Bytes;
use shiguredo_moqt::loc::{
    LocProperties, LocPropertyValue, PROP_AUDIO_CONFIG, PROP_TIMESCALE, PROP_TIMESTAMP,
    PROP_VIDEO_CONFIG,
};
use shiguredo_moqt::session::types::TrackDataAcceptance;
use shiguredo_moqt::{
    message::common::TrackNamespace, msf::MSF_CATALOG_TRACK_NAME, msf::MsfCatalog,
    msf::MsfCatalogDocument, msf::MsfTrack, session::types::DataStreamId,
    session::types::RequestStreamEnd, session::types::SessionEvent,
    stream::decoder::DecodedFetchEntry, stream::decoder::FetchStreamDecoder,
    stream::decoder::SubgroupStreamDecoder,
};

use crate::cli::Config;
use crate::decoder::opus::OpusDecoder;
use crate::decoder::{self, DecodedAudioFrame, DecodedVideoFrame};
use crate::error::{Error, Result};
use crate::stream_reader::{self, StreamType};
use moqt_example_transport::Transport;
use moqt_example_transport::host_from_authority;
use moqt_example_transport::moqt_client::{DataPlaneHandle, MoqtClient, StreamRead};
use moqt_example_transport::quic;
use moqt_example_transport::transport;

/// 音声サンプルレートが取得できなかったときのフォールバック (publisher が 48 kHz で送信する前提)
const AUDIO_FALLBACK_SAMPLE_RATE: u32 = 48_000;

/// カタログから取得したビデオトラック情報
struct VideoTrackInfo {
    track_name: String,
    codec: String,
    width: u32,
    height: u32,
    fps: u32,
}

/// カタログから取得した音声トラック情報
struct AudioTrackInfo {
    track_name: String,
    codec: String,
    samplerate: u32,
    channel_config: String,
}

/// カタログから取得した fps をスレッド間で共有する
pub type SharedFps = Arc<AtomicU32>;

/// SharedFps を生成する (デフォルト 30fps)
pub fn new_shared_fps() -> SharedFps {
    Arc::new(AtomicU32::new(30))
}

/// トラック種別 (data stream 処理で使用)
#[derive(Debug, Clone, Copy)]
enum TrackKind {
    Video,
    Audio,
}

/// パイプラインを実行する
///
/// `task_monitor` は生成されるタスクのメトリクスを収集する。
/// `shutdown_monitor` は graceful shutdown の契機を受信する。
pub async fn run(
    config: Config,
    frame_tx: std::sync::mpsc::Sender<DecodedVideoFrame>,
    audio_tx: std::sync::mpsc::Sender<DecodedAudioFrame>,
    shared_fps: SharedFps,
    task_monitor: tokio_metrics::TaskMonitor,
    mut shutdown_monitor: tokio_utils::ShutdownMonitor,
) -> Result<()> {
    // TLS の SNI / 証明書検証に使う server_name はポートを含めない。
    // IPv6 リテラル (`[::1]:4443` 等) でも正しく host を取り出す。
    let server_name = host_from_authority(&config.url.authority);

    // 1. 接続確立と SETUP ハンドシェイク
    let (mut client, mut recv_acceptor) = match config.url.transport {
        Transport::Quic => {
            let connection =
                quic::connect(&config.url.authority, server_name, config.cert.as_deref()).await?;
            MoqtClient::establish_quic(
                connection,
                &config.url.path,
                &config.url.authority,
                "moqt-subscriber",
                &task_monitor,
            )
            .await?
        }
        Transport::WebTransport => {
            let socket_addr: std::net::SocketAddr = config.url.authority.parse().map_err(|e| {
                Error::Other(format!(
                    "invalid server address '{}': {e}",
                    config.url.authority
                ))
            })?;
            let mut client_config =
                moqt_example_transport::webtransport::ClientConfig::new(socket_addr, server_name)
                    // :authority はポート込みの target URI authority を使う (draft-ietf-webtrans-http3-16 §3.2)
                    .authority(&config.url.authority)
                    .enable_webtransport(
                        shiguredo_http3::webtransport::Settings::new()
                            .wt_enabled(shiguredo_http3::VarInt::from_static(1)),
                    )
                    // subscriber は datagram を継続受信するためバックグラウンドタスクを起動する
                    .receive_datagrams();
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
            MoqtClient::establish_wt(wt_session, "moqt-subscriber", &task_monitor).await?
        }
    };

    let namespace = TrackNamespace::new(vec![config.namespace.as_bytes().to_vec()])?;
    let data_plane = client.data_plane();

    // 2. タイムアウト設定
    client.set_control_message_timeout_ms(Some(30000));
    client.set_data_stream_timeout_ms(Some(30000));
    tracing::info!(
        "Session timeouts: control={:?}ms, data={:?}ms",
        client.control_message_timeout_ms(),
        client.data_stream_timeout_ms(),
    );

    // 3. FETCH でカタログを取得 (range {0, 0} - {0, 1} を LOCATION_FILTER で指定)
    let mut catalog_params = shiguredo_moqt::message_parameter::MessageParameters::new();
    catalog_params.push(shiguredo_moqt::message_parameter::MessageParameter {
        param_type: shiguredo_moqt::message_parameter::PARAM_LOCATION_FILTER,
        value: shiguredo_moqt::message_parameter::MessageParameterValue::LengthPrefixed(
            shiguredo_moqt::message_parameter::LocationFilter::AbsoluteRangeWithEnd {
                start: shiguredo_moqt::message::common::Location {
                    group_id: 0,
                    object_id: 0,
                },
                end_group_delta: 0,
                end_object: 1,
            }
            .encode_to_bytes(),
        ),
    });
    let catalog_fetch = client
        .fetch(
            namespace.clone(),
            MSF_CATALOG_TRACK_NAME.to_vec(),
            catalog_params,
        )
        .await?;
    tracing::info!(
        "Catalog FETCH_OK received: request_id={}",
        catalog_fetch.request_id
    );

    // カタログの FETCH 応答 data stream を受信
    let (video_info, audio_info) =
        receive_catalog(&mut recv_acceptor, &data_plane, catalog_fetch.request_id).await?;
    if let Some(ref v) = video_info {
        tracing::info!(
            "Catalog video track: name={}, codec={}, {}x{} @ {} fps",
            v.track_name,
            v.codec,
            v.width,
            v.height,
            v.fps,
        );
    }
    if let Some(ref a) = audio_info {
        tracing::info!(
            "Catalog audio track: name={}, codec={}, samplerate={}, channel_config={}",
            a.track_name,
            a.codec,
            a.samplerate,
            a.channel_config,
        );
    }
    if let Some(ref v) = video_info {
        shared_fps.store(v.fps, Ordering::Relaxed);
    }

    // 3. トラックを SUBSCRIBE する (cli で無効化されたトラックは skip)
    let subscribe_video = config.video_enabled && video_info.is_some();
    let subscribe_audio = config.audio_enabled && audio_info.is_some();
    if !subscribe_video && !subscribe_audio {
        return Err(Error::Other(
            "no tracks to subscribe (catalog does not provide enabled video/audio track)"
                .to_string(),
        ));
    }

    let mut track_map: HashMap<u64, TrackKind> = HashMap::new();
    let mut video_codec: Option<String> = None;
    let mut video_request_id: Option<u64> = None;
    let mut audio_request_id: Option<u64> = None;

    if subscribe_video {
        let v = video_info.as_ref().expect("video_info is Some");
        let video_sub = client
            .subscribe_track(namespace.clone(), v.track_name.as_bytes().to_vec())
            .await?;
        tracing::info!(
            "Subscribed to video track: alias={}, request_id={}",
            video_sub.track_alias,
            video_sub.request_id,
        );
        track_map.insert(video_sub.track_alias, TrackKind::Video);
        video_request_id = Some(video_sub.request_id);

        // draft-ietf-moq-transport-21 で Joining FETCH は廃止され fill fetch stream
        // (§3.4) に置き換えられたため、過去 group の取得デモは行わず live のみ受信する。
        // 欠落範囲の補填が必要な場合は SUBSCRIBE 時に FILL_PARAMETERS を付与する。

        // REQUEST_UPDATE で video subscription の subscriber priority を変更するサンプル
        let mut params = shiguredo_moqt::message_parameter::MessageParameters::new();
        params.push(shiguredo_moqt::message_parameter::MessageParameter {
            param_type: shiguredo_moqt::message_parameter::PARAM_SUBSCRIBER_PRIORITY,
            value: shiguredo_moqt::message_parameter::MessageParameterValue::Uint8(192),
        });
        if let Err(e) = client
            .send_request_update(video_sub.request_id, params)
            .await
        {
            tracing::warn!("Failed to send REQUEST_UPDATE for video subscription: {e}");
        }

        // ビデオデコーダの初期化確認
        let _probe = decoder::build_video_decoder(&v.codec)?;
        drop(_probe);
        tracing::info!("Video decoder available (codec={})", v.codec);
        video_codec = Some(v.codec.clone());
    }

    let audio_params: Option<(u32, u8)> = if subscribe_audio {
        let a = audio_info.as_ref().expect("audio_info is Some");
        let audio_sub = client
            .subscribe_track(namespace.clone(), a.track_name.as_bytes().to_vec())
            .await?;
        tracing::info!(
            "Subscribed to audio track: alias={}, request_id={}",
            audio_sub.track_alias,
            audio_sub.request_id,
        );
        track_map.insert(audio_sub.track_alias, TrackKind::Audio);
        audio_request_id = Some(audio_sub.request_id);

        let channels = match a.channel_config.as_str() {
            "1" => 1u8,
            "2" => 2u8,
            other => {
                return Err(Error::Other(format!(
                    "unsupported audio channel_config: {other}"
                )));
            }
        };
        // Opus デコーダの初期化確認
        let _probe = OpusDecoder::new(a.samplerate, channels)?;
        drop(_probe);
        tracing::info!(
            "Audio decoder available (codec={}, sample_rate={}, channels={})",
            a.codec,
            a.samplerate,
            channels
        );
        Some((a.samplerate, channels))
    } else {
        None
    };
    // Audio Config の検証 (codec 判定) に使う catalog の audio codec
    let audio_codec = audio_info.as_ref().map(|a| a.codec.clone());

    let track_map: Arc<HashMap<u64, TrackKind>> = Arc::new(track_map);

    // 6. datagram 受信タスク
    let data_plane_for_datagram = data_plane.clone();
    let handle_for_datagram = client.handle();
    tokio::spawn(task_monitor.clone().instrument(async move {
        loop {
            match handle_for_datagram.recv_datagrams().await {
                Ok(datagrams) => {
                    for payload in datagrams {
                        // type 分岐は Session が行う。未知 type は
                        // draft-ietf-moq-transport-21 §11 (Data Streams and Datagrams) に従いセッションクローズになる。
                        if let Err(e) = data_plane_for_datagram.recv_datagram(&payload) {
                            tracing::warn!("Failed to process datagram: {e}");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Datagram receive error: {e}");
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }));

    // 7. データストリーム受信ループ
    let start = Instant::now();
    let mut tick_interval = tokio::time::interval(std::time::Duration::from_millis(100));
    tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut total_streams: u64 = 0;
    let mut join_set = tokio::task::JoinSet::new();

    'main: loop {
        tokio::select! {
            result = recv_acceptor.accept_recv_stream() => {
                let stream = match result {
                    Ok(Some(s)) => s,
                    Ok(None) => {
                        tracing::info!("No more data streams");
                        break 'main;
                    }
                    Err(e) => {
                        tracing::error!("Failed to accept data stream: {e}");
                        break 'main;
                    }
                };
                total_streams += 1;
                let stream_num = total_streams;
                let track_map = Arc::clone(&track_map);
                let data_plane = data_plane.clone();
                let frame_tx = frame_tx.clone();
                let audio_tx = audio_tx.clone();
                let codec = video_codec.clone();
                let audio_codec = audio_codec.clone();
                let params = audio_params;
                join_set.spawn(task_monitor.clone().instrument(async move {
                    handle_incoming_stream(
                        stream,
                        data_plane,
                        &track_map,
                        codec.as_deref(),
                        audio_codec.as_deref(),
                        params,
                        &frame_tx,
                        &audio_tx,
                        stream_num,
                    )
                    .await;
                }));
            }
            notable = client.next_event() => {
                match notable? {
                    Some(SessionEvent::GoawayReceived { timeout, .. }) => {
                        tracing::info!("Received GOAWAY (timeout={timeout})");
                        break 'main;
                    }
                    Some(SessionEvent::PublishDoneReceived { status_code, stream_count, .. }) => {
                        tracing::info!(
                            "Received PUBLISH_DONE (status_code={status_code}, stream_count={stream_count})"
                        );
                        break 'main;
                    }
                    Some(SessionEvent::CloseSession(err)) => {
                        tracing::warn!("Session closed: {:#x} {}", err.code, err.reason);
                        break 'main;
                    }
                    _ => {}
                }
            }
            _ = tick_interval.tick() => {
                let now_ms = Instant::now().duration_since(start).as_millis() as u64;
                client.tick(now_ms);
            }
            _ = shutdown_monitor.recv() => {
                tracing::info!("Shutdown signal received");
                break 'main;
            }
        }
    }

    while let Some(result) = join_set.join_next().await {
        if let Err(e) = result {
            tracing::warn!("Stream task panicked: {e}");
        }
    }

    if let Some(rid) = video_request_id
        && let Err(e) = client.stop_sending(rid).await
    {
        tracing::warn!("Failed to send STOP_SENDING for video: {e}");
    }
    if let Some(rid) = audio_request_id
        && let Err(e) = client.stop_sending(rid).await
    {
        tracing::warn!("Failed to send STOP_SENDING for audio: {e}");
    }

    if let Err(e) = client.send_goaway(Vec::new(), 5000).await {
        tracing::warn!("Failed to send GOAWAY: {e}");
    }

    if let Err(e) = client.close(0, "").await {
        tracing::warn!("Failed to close session gracefully: {e}");
    }

    tracing::info!("Pipeline stopped: {total_streams} streams received");
    Ok(())
}

async fn receive_registered_stream_data(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
) -> Result<Option<Bytes>> {
    match stream.receive_chunk().await? {
        transport::RecvChunk::Data(data) => Ok(Some(data)),
        transport::RecvChunk::End(end) => {
            data_plane.recv_data_stream_closed(stream_id, end)?;
            Ok(None)
        }
    }
}

async fn drain_registered_stream_to_end(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
) -> Result<()> {
    loop {
        if receive_registered_stream_data(stream, data_plane, stream_id)
            .await?
            .is_none()
        {
            return Ok(());
        }
    }
}

/// カタログの FETCH 応答ストリームを受信してパースする
async fn receive_catalog(
    recv_acceptor: &mut transport::StreamAcceptor,
    data_plane: &DataPlaneHandle,
    expected_request_id: u64,
) -> Result<(Option<VideoTrackInfo>, Option<AudioTrackInfo>)> {
    // Padding stream が割り込んだ場合は読み捨てて次の stream を待つ。
    let (mut stream, stream_id, buf) = loop {
        let mut stream = recv_acceptor
            .accept_recv_stream()
            .await?
            .ok_or_else(|| Error::Other("no catalog fetch stream received".to_string()))?;
        let stream_id = DataStreamId(stream.stream_id());
        let mut buf = Vec::new();
        let (stream_type_id, stream_type) =
            match stream_reader::peek_stream_type(&mut stream, &mut buf).await? {
                StreamRead::Value(value) => value,
                StreamRead::Closed(end) => {
                    return Err(Error::Other(format!(
                        "catalog stream closed before type: {end:?}"
                    )));
                }
            };
        data_plane.recv_data_stream_type(stream_id, stream_type_id)?;
        if matches!(stream_type, StreamType::Padding) {
            // draft-ietf-moq-transport-21 §11.5.1 (Padding Streams):
            // パディングバイトを読み捨てて次の stream を待つ。
            let _ = drain_registered_stream_to_end(&mut stream, data_plane, stream_id).await;
            continue;
        }
        if !matches!(stream_type, StreamType::Fetch) {
            let _ = drain_registered_stream_to_end(&mut stream, data_plane, stream_id).await;
            return Err(Error::Other(
                "catalog stream is not a fetch response".to_string(),
            ));
        }
        break (stream, stream_id, buf);
    };
    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&buf);
    let fetch_header = loop {
        if let Some(h) = decoder.try_decode_header()? {
            break h;
        }
        match receive_registered_stream_data(&mut stream, data_plane, stream_id).await? {
            Some(data) => decoder.push(&data),
            None => {
                return Err(Error::Other(
                    "stream closed before fetch header".to_string(),
                ));
            }
        }
    };
    data_plane.recv_fetch_header(stream_id, &fetch_header)?;
    if fetch_header.request_id != expected_request_id {
        let _ = drain_registered_stream_to_end(&mut stream, data_plane, stream_id).await;
        return Err(Error::Other(format!(
            "unexpected fetch request_id: expected={expected_request_id}, got={}",
            fetch_header.request_id,
        )));
    }
    // カタログトラックのオブジェクトを順に読み、Full で置き換え、Delta を適用する。
    // draft-ietf-moq-msf-01 §5 (Catalog): 最初の Object (Object ID 0) が独立カタログ、
    // 以降 (Object ID >= 1) が delta update。
    let mut catalog: Option<MsfCatalog> = None;
    loop {
        let entry = match decoder.try_decode_entry()? {
            Some(e) => e,
            None => match receive_registered_stream_data(&mut stream, data_plane, stream_id).await?
            {
                Some(data) => {
                    decoder.push(&data);
                    continue;
                }
                None => break,
            },
        };
        data_plane.recv_fetch_entry(stream_id)?;
        let payload = match &entry {
            DecodedFetchEntry::Object(obj) if obj.payload_length > 0 => loop {
                if let Some(p) = decoder.try_read_payload() {
                    break p;
                }
                match receive_registered_stream_data(&mut stream, data_plane, stream_id).await? {
                    Some(data) => decoder.push(&data),
                    None => {
                        return Err(Error::Other(
                            "stream closed before catalog payload".to_string(),
                        ));
                    }
                }
            },
            _ => Vec::new(),
        };
        let document = MsfCatalogDocument::decode(&payload).map_err(|e| {
            Error::Other(format!(
                "failed to parse MSF catalog: {e}, raw={}",
                String::from_utf8_lossy(&payload),
            ))
        })?;
        match document {
            MsfCatalogDocument::Full(full) => {
                tracing::info!("Received MSF catalog: {full:?}");
                catalog = Some(full);
            }
            MsfCatalogDocument::Delta(delta) => {
                // example の publisher は track に明示 namespace を付けるため、
                // catalog namespace は省略 (None) でも親トラックを解決できる。
                let base = catalog.as_mut().ok_or_else(|| {
                    Error::Other("received delta catalog before an independent catalog".to_string())
                })?;
                base.apply_delta(&delta, None)
                    .map_err(|e| Error::Other(format!("failed to apply MSF delta catalog: {e}")))?;
                tracing::info!("Applied MSF delta catalog: {delta:?}");
            }
        }
    }
    drain_registered_stream_to_end(&mut stream, data_plane, stream_id).await?;
    let catalog = catalog.ok_or_else(|| Error::Other("empty catalog fetch stream".to_string()))?;
    let tracks = &catalog.tracks;
    let video_info = tracks
        .iter()
        .find(|t| {
            t.codec.as_ref().is_some_and(|c| {
                c.starts_with("av01")
                    || c.starts_with("avc1")
                    || c.starts_with("hvc1")
                    || c.starts_with("hev1")
            })
        })
        .map(extract_video_info)
        .transpose()?;
    let audio_info = tracks
        .iter()
        .find(|t| t.codec.as_ref().is_some_and(|c| c.starts_with("opus")))
        .map(extract_audio_info)
        .transpose()?;
    if video_info.is_none() && audio_info.is_none() {
        return Err(Error::Other(
            "no usable tracks found in catalog (expected av01/avc1/hvc1/hev1/opus)".to_string(),
        ));
    }
    Ok((video_info, audio_info))
}

fn extract_video_info(track: &MsfTrack) -> Result<VideoTrackInfo> {
    let codec = track
        .codec
        .clone()
        .ok_or_else(|| Error::Other("catalog video track missing codec".to_string()))?;
    let width = track
        .width
        .ok_or_else(|| Error::Other("catalog track missing width".to_string()))?
        as u32;
    let height = track
        .height
        .ok_or_else(|| Error::Other("catalog track missing height".to_string()))?
        as u32;
    let fps = track.framerate.map(|f| f as u32).unwrap_or(30);
    Ok(VideoTrackInfo {
        track_name: track.name.clone(),
        codec,
        width,
        height,
        fps,
    })
}

fn extract_audio_info(track: &MsfTrack) -> Result<AudioTrackInfo> {
    let codec = track
        .codec
        .clone()
        .ok_or_else(|| Error::Other("catalog audio track missing codec".to_string()))?;
    let samplerate = track
        .samplerate
        .ok_or_else(|| Error::Other("catalog audio track missing samplerate".to_string()))?
        as u32;
    let channel_config = track
        .channel_config
        .clone()
        .ok_or_else(|| Error::Other("catalog audio track missing channelConfig".to_string()))?;
    Ok(AudioTrackInfo {
        track_name: track.name.clone(),
        codec,
        samplerate,
        channel_config,
    })
}

#[expect(clippy::too_many_arguments)]
async fn handle_incoming_stream(
    mut stream: transport::RecvStream,
    data_plane: DataPlaneHandle,
    track_map: &HashMap<u64, TrackKind>,
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
    audio_params: Option<(u32, u8)>,
    frame_tx: &std::sync::mpsc::Sender<DecodedVideoFrame>,
    audio_tx: &std::sync::mpsc::Sender<DecodedAudioFrame>,
    stream_num: u64,
) {
    let stream_id = DataStreamId(stream.stream_id());
    let mut buf = Vec::new();
    let (stream_type_id, stream_type) =
        match stream_reader::peek_stream_type(&mut stream, &mut buf).await {
            Ok(StreamRead::Value(value)) => value,
            Ok(StreamRead::Closed(end)) => {
                tracing::debug!("Stream #{stream_num}: closed before type: {end:?}");
                return;
            }
            Err(e) => {
                tracing::warn!("Stream #{stream_num}: failed to peek stream type: {e}");
                return;
            }
        };
    if let Err(e) = data_plane.recv_data_stream_type(stream_id, stream_type_id) {
        tracing::warn!("Stream #{stream_num}: failed to register data stream type: {e}");
        return;
    }
    match stream_type {
        StreamType::Fetch => {
            // 本 example はカタログ取得の FETCH のみ発行する。
            // カタログ応答は receive_catalog で消費済みのため、ここに来る Fetch は想定外である。
            let Some(codec) = video_codec else {
                tracing::warn!("Stream #{stream_num}: fetch stream received without video codec");
                let _ = data_plane.send_data_stream_stop_sending(stream_id);
                let _ = drain_registered_stream_to_end(&mut stream, &data_plane, stream_id).await;
                return;
            };
            let mut decoder = match decoder::build_video_decoder(codec) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("Stream #{stream_num}: failed to create video decoder: {e}");
                    let _ = data_plane.send_data_stream_stop_sending(stream_id);
                    let _ =
                        drain_registered_stream_to_end(&mut stream, &data_plane, stream_id).await;
                    return;
                }
            };
            handle_fetch_stream(
                &mut stream,
                &data_plane,
                stream_id,
                &buf,
                &mut decoder,
                frame_tx,
                stream_num,
            )
            .await;
        }
        StreamType::Padding => {
            // draft-ietf-moq-transport-21 §11.5.1 (Padding Streams):
            // パディングバイトを読み捨てる。session には recv_data_stream_type で登録済み。
            let _ = drain_registered_stream_to_end(&mut stream, &data_plane, stream_id).await;
        }
        StreamType::Subgroup => {
            let mut sg_decoder = SubgroupStreamDecoder::new();
            sg_decoder.push(&buf);
            let header = loop {
                match sg_decoder.try_decode_header() {
                    Ok(Some(h)) => break h,
                    Ok(None) => {
                        match receive_registered_stream_data(&mut stream, &data_plane, stream_id)
                            .await
                        {
                            Ok(Some(data)) => sg_decoder.push(&data),
                            Ok(None) => return,
                            Err(e) => {
                                tracing::warn!(
                                    "Stream #{stream_num}: failed to read subgroup header: {e}"
                                );
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Stream #{stream_num}: failed to read subgroup header: {e}");
                        let _ = drain_registered_stream_to_end(&mut stream, &data_plane, stream_id)
                            .await;
                        return;
                    }
                }
            };
            match data_plane.recv_subgroup_header(stream_id, &header) {
                Ok(TrackDataAcceptance::Accepted) => {}
                Ok(TrackDataAcceptance::UnknownTrackAlias) => {
                    tracing::warn!(
                        "Stream #{stream_num}: session rejected unknown track alias={}",
                        header.track_alias,
                    );
                    let _ = data_plane.send_data_stream_stop_sending(stream_id);
                    let _ =
                        drain_registered_stream_to_end(&mut stream, &data_plane, stream_id).await;
                    return;
                }
                // draft-ietf-moq-transport-21 §3.1.2 (Track Alias): キャンセル済み subscription へ
                // 遅れて届いた Object。未知 alias と違い確実に不要なので静かに捨てる。
                Ok(TrackDataAcceptance::Discarded) => {
                    tracing::debug!(
                        "Stream #{stream_num}: discarding objects for cancelled track alias={}",
                        header.track_alias,
                    );
                    let _ = data_plane.send_data_stream_stop_sending(stream_id);
                    let _ =
                        drain_registered_stream_to_end(&mut stream, &data_plane, stream_id).await;
                    return;
                }
                // draft-ietf-moq-transport-21 §3.1 (Subscriptions): フィルタ再適用の結果
                // どの subscription にも属さなかった Object。セッションは閉じずに捨てる。
                Ok(TrackDataAcceptance::FilteredOut) => {
                    tracing::debug!(
                        "Stream #{stream_num}: object filtered out for track alias={}",
                        header.track_alias,
                    );
                    let _ = data_plane.send_data_stream_stop_sending(stream_id);
                    let _ =
                        drain_registered_stream_to_end(&mut stream, &data_plane, stream_id).await;
                    return;
                }
                Err(e) => {
                    tracing::warn!("Stream #{stream_num}: failed to register subgroup header: {e}");
                    return;
                }
            }
            let kind = track_map.get(&header.track_alias);
            tracing::debug!(
                "Stream #{stream_num}: alias={}, group={}, kind={kind:?}",
                header.track_alias,
                header.group_id,
            );
            match kind {
                Some(TrackKind::Video) => {
                    let Some(codec) = video_codec else {
                        tracing::warn!(
                            "Stream #{stream_num}: video stream arrived but video is not configured"
                        );
                        let _ = data_plane.send_data_stream_stop_sending(stream_id);
                        let _ = drain_registered_stream_to_end(&mut stream, &data_plane, stream_id)
                            .await;
                        return;
                    };
                    let mut decoder = match decoder::build_video_decoder(codec) {
                        Ok(d) => d,
                        Err(e) => {
                            tracing::warn!(
                                "Stream #{stream_num}: failed to create video decoder: {e}"
                            );
                            let _ = data_plane.send_data_stream_stop_sending(stream_id);
                            let _ =
                                drain_registered_stream_to_end(&mut stream, &data_plane, stream_id)
                                    .await;
                            return;
                        }
                    };
                    let frames = decode_video_stream(
                        &mut stream,
                        &data_plane,
                        stream_id,
                        &mut sg_decoder,
                        &mut decoder,
                        frame_tx,
                    )
                    .await;
                    tracing::debug!(
                        "Stream #{stream_num}: video group {} complete ({frames} frames)",
                        header.group_id,
                    );
                }
                Some(TrackKind::Audio) => {
                    let Some((sample_rate, channels)) = audio_params else {
                        tracing::warn!(
                            "Stream #{stream_num}: audio stream arrived but audio is not configured"
                        );
                        let _ = data_plane.send_data_stream_stop_sending(stream_id);
                        let _ = drain_registered_stream_to_end(&mut stream, &data_plane, stream_id)
                            .await;
                        return;
                    };
                    let mut decoder = match OpusDecoder::new(sample_rate, channels) {
                        Ok(d) => d,
                        Err(e) => {
                            tracing::warn!(
                                "Stream #{stream_num}: failed to create opus decoder: {e}"
                            );
                            let _ = data_plane.send_data_stream_stop_sending(stream_id);
                            let _ =
                                drain_registered_stream_to_end(&mut stream, &data_plane, stream_id)
                                    .await;
                            return;
                        }
                    };
                    // Audio Config の中身はコーデック依存 (LOC draft-ietf-moq-loc-04 §2.3.3.1 (Audio Config)) のため、
                    // catalog の codec が opus の場合のみ OpusHead として解釈する。
                    // 処理済みフラグは本関数レベルで保持する (decode_audio_stream は
                    // subgroup stream ごとに呼ばれるが、現行 publisher は OpusHead を
                    // セッションで 1 回しか送らないため実質 1 回きりになる)。
                    let is_opus_codec = audio_codec.is_some_and(|c| c.starts_with("opus"));
                    let mut audio_config_handled = false;
                    let chunks = decode_audio_stream(
                        &mut stream,
                        &data_plane,
                        stream_id,
                        &mut sg_decoder,
                        &mut decoder,
                        audio_tx,
                        &mut audio_config_handled,
                        is_opus_codec,
                        sample_rate,
                        channels,
                    )
                    .await;
                    tracing::debug!(
                        "Stream #{stream_num}: audio group {} complete ({chunks} chunks)",
                        header.group_id,
                    );
                }
                None => {
                    tracing::warn!(
                        "Stream #{stream_num}: unknown track alias={}",
                        header.track_alias,
                    );
                    let _ =
                        drain_registered_stream_to_end(&mut stream, &data_plane, stream_id).await;
                }
            }
        }
    }
}

async fn handle_fetch_stream(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    buf: &[u8],
    video_decoder: &mut decoder::VideoDecoder,
    frame_tx: &std::sync::mpsc::Sender<DecodedVideoFrame>,
    stream_num: u64,
) {
    let mut decoder = FetchStreamDecoder::new();
    decoder.push(buf);
    let fetch_header = loop {
        match decoder.try_decode_header() {
            Ok(Some(h)) => break h,
            Ok(None) => match receive_registered_stream_data(stream, data_plane, stream_id).await {
                Ok(Some(data)) => decoder.push(&data),
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!("Stream #{stream_num}: failed to read fetch header: {e}");
                    return;
                }
            },
            Err(e) => {
                tracing::warn!("Stream #{stream_num}: failed to read fetch header: {e}");
                let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
                return;
            }
        }
    };
    if let Err(e) = data_plane.recv_fetch_header(stream_id, &fetch_header) {
        tracing::warn!("Stream #{stream_num}: failed to register fetch header: {e}");
        return;
    }
    tracing::info!(
        "Stream #{stream_num}: FETCH response (request_id={})",
        fetch_header.request_id,
    );
    let mut frames: u64 = 0;
    loop {
        let entry = loop {
            match decoder.try_decode_entry() {
                Ok(Some(e)) => break e,
                Ok(None) => match receive_registered_stream_data(stream, data_plane, stream_id)
                    .await
                {
                    Ok(Some(data)) => decoder.push(&data),
                    Ok(None) => {
                        tracing::info!("Stream #{stream_num}: FETCH complete ({frames} frames)");
                        if let Err(e) = data_plane.recv_fetch_data_stream_closed(
                            fetch_header.request_id,
                            RequestStreamEnd::Fin,
                        ) {
                            tracing::warn!(
                                "Stream #{stream_num}: failed to notify fetch closed: {e}"
                            );
                        }
                        return;
                    }
                    Err(e) => {
                        tracing::warn!("Stream #{stream_num}: stream error: {e}");
                        return;
                    }
                },
                Err(e) => {
                    tracing::warn!("Stream #{stream_num}: failed to read fetch entry: {e}");
                    let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
                    return;
                }
            }
        };
        // エントリデコード直後 (payload 読み込み前) に Session へ通知する。
        // draft-ietf-moq-transport-21 §6.6 (Termination) の activity は object header の
        // 到着時点を指すため、読み込み完了時点を記録すると deadline が読み込み時間ぶん
        // 後ろへずれる (payload 読み込み中は更新されない既知の限界。Session 側の
        // subgroup 経路と同じ設計。この節番号・規則は draft 由来であり将来 draft 改定で
        // 変わる可能性がある)。
        if let Err(e) = data_plane.recv_fetch_entry(stream_id) {
            tracing::warn!("Stream #{stream_num}: failed to register fetch entry: {e}");
            let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
            return;
        }
        if let DecodedFetchEntry::Object(ref obj) = entry
            && obj.payload_length > 0
        {
            let payload = loop {
                if let Some(p) = decoder.try_read_payload() {
                    break p;
                }
                match receive_registered_stream_data(stream, data_plane, stream_id).await {
                    Ok(Some(data)) => decoder.push(&data),
                    Ok(None) => {
                        if let Err(e) = data_plane.recv_fetch_data_stream_closed(
                            fetch_header.request_id,
                            RequestStreamEnd::Fin,
                        ) {
                            tracing::warn!(
                                "Stream #{stream_num}: failed to notify fetch closed: {e}"
                            );
                        }
                        return;
                    }
                    Err(e) => {
                        tracing::warn!("Stream #{stream_num}: stream error: {e}");
                        return;
                    }
                }
            };
            // FetchStreamDecoder は properties_bytes を保持しないため video_config は渡せない
            // (AV1 は Sequence Header が payload 内に含まれるため問題なし。H.264/H.265 は
            // Subgroup 経由でのみ動作する)
            frames += decode_and_send(&payload, None, video_decoder, frame_tx);
        }
    }
}

async fn decode_video_stream(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    sg_decoder: &mut SubgroupStreamDecoder,
    video_decoder: &mut decoder::VideoDecoder,
    frame_tx: &std::sync::mpsc::Sender<DecodedVideoFrame>,
) -> u64 {
    let mut frames: u64 = 0;
    loop {
        // 帰属判定の結果。`FilteredOut` / `Discarded` の Object は Application へ渡さない。
        let (obj, deliver) = loop {
            match sg_decoder.try_decode_object() {
                Ok(Some(o)) => {
                    let deliver = match data_plane.recv_subgroup_object(stream_id, &o) {
                        Ok(TrackDataAcceptance::Accepted) => true,
                        // header 受理済み stream では `UnknownTrackAlias` は返らない契約だが、
                        // 型上あり得るため防御的に破棄する
                        Ok(
                            TrackDataAcceptance::UnknownTrackAlias
                            | TrackDataAcceptance::FilteredOut
                            | TrackDataAcceptance::Discarded,
                        ) => false,
                        Err(e) => {
                            tracing::warn!("Failed to register video object: {e}");
                            return frames;
                        }
                    };
                    break (o, deliver);
                }
                Ok(None) => {
                    match receive_registered_stream_data(stream, data_plane, stream_id).await {
                        Ok(Some(data)) => sg_decoder.push(&data),
                        Ok(None) => return frames,
                        Err(e) => {
                            tracing::warn!("Failed to read object: {e}");
                            return frames;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to decode object: {e}");
                    let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
                    return frames;
                }
            }
        };
        if obj.payload_length == 0 {
            continue;
        }
        // `FilteredOut` / `Discarded` の Object でも payload は wire 上に存在するため
        // 読み出して消費する。残すと次の `try_decode_object` が ProtocolViolation になる。
        let payload = loop {
            if let Some(p) = sg_decoder.try_read_payload() {
                break p;
            }
            match receive_registered_stream_data(stream, data_plane, stream_id).await {
                Ok(Some(data)) => sg_decoder.push(&data),
                Ok(None) => return frames,
                Err(e) => {
                    tracing::warn!("Failed to read object payload: {e}");
                    return frames;
                }
            }
        };
        if !deliver {
            continue;
        }
        let video_config = extract_video_config(obj.properties_bytes.as_deref());
        frames += decode_and_send(&payload, video_config.as_deref(), video_decoder, frame_tx);
    }
}

fn decode_and_send(
    payload: &[u8],
    video_config: Option<&[u8]>,
    decoder: &mut decoder::VideoDecoder,
    frame_tx: &std::sync::mpsc::Sender<DecodedVideoFrame>,
) -> u64 {
    let decoded = match decoder.decode(payload, video_config) {
        Ok(frames) => frames,
        Err(e) => {
            tracing::warn!("Video decode error: {e}, payload_len={}", payload.len());
            return 0;
        }
    };
    let mut frames: u64 = 0;
    for frame in decoded {
        frames += 1;
        if frame_tx.send(frame).is_err() {
            return frames;
        }
    }
    frames
}

/// object の properties バイト列から PROP_VIDEO_CONFIG を取り出す
fn extract_video_config(properties_bytes: Option<&[u8]>) -> Option<Vec<u8>> {
    let bytes = properties_bytes?;
    let (props, _) = LocProperties::decode(bytes).ok()?;
    for p in props.iter() {
        if p.prop_id == PROP_VIDEO_CONFIG
            && let LocPropertyValue::Bytes(ref b) = p.value
        {
            return Some(b.clone());
        }
    }
    None
}

/// object の properties バイト列から PROP_TIMESTAMP / PROP_TIMESCALE を取り出す
fn extract_timestamp_timescale(properties_bytes: Option<&[u8]>) -> (Option<u64>, Option<u64>) {
    let Some(bytes) = properties_bytes else {
        return (None, None);
    };
    let Ok((props, _)) = LocProperties::decode(bytes) else {
        return (None, None);
    };
    let mut ts = None;
    let mut tscale = None;
    for p in props.iter() {
        match p.prop_id {
            PROP_TIMESTAMP => {
                if let LocPropertyValue::VarInt(v) = p.value {
                    ts = Some(v);
                }
            }
            PROP_TIMESCALE => {
                if let LocPropertyValue::VarInt(v) = p.value {
                    tscale = Some(v);
                }
            }
            _ => {}
        }
    }
    (ts, tscale)
}

/// LOC の Timestamp と Timescale から音声 PTS (マイクロ秒) を計算する
///
/// Timestamp は `u64` 全域を取りうるため、`as i64` キャストや `* 1_000_000` の乗算で
/// オーバーフローしないよう `u128` で中間計算し、結果が `i64` に収まらない場合は
/// `i64::MAX` に飽和させる。`timestamp` が無い、または `timescale` が 0 の場合は 0 を返す。
fn audio_pts_us(timestamp: Option<u64>, timescale: Option<u64>) -> i64 {
    let effective_scale = timescale.unwrap_or(u64::from(AUDIO_FALLBACK_SAMPLE_RATE));
    match timestamp {
        Some(ts) if effective_scale > 0 => {
            let pts = u128::from(ts) * 1_000_000 / u128::from(effective_scale);
            i64::try_from(pts).unwrap_or(i64::MAX)
        }
        _ => 0,
    }
}

/// OpusHead (RFC 7845 §5.1) のパース結果
struct ParsedOpusHead {
    /// Channel Count (バイト 9)
    channel_count: u8,
    /// Input Sample Rate (バイト 12-15, little-endian)。0 は unspecified
    input_sample_rate: u32,
}

/// OpusHead (RFC 7845 §5.1) をパースする
///
/// magic "OpusHead"・ version 1 ・ 19 バイト以上・ Channel Count > 0 を検証する。
/// パース失敗 (magic 不一致・ version ≠ 1 ・ 19 バイト未満・ Channel Count = 0 等) は
/// `None` を返す (呼び出し側で警告して検証をスキップする)。
/// RFC 7845 §5.1 は "SHOULD accept any stream with a version number of '15' or less" と
/// 後方互換受理を推奨するが、現行 publisher は version 1 のみ送信するため、ここでは
/// version ≠ 1 をパース失敗として扱う (厳格化)。
fn parse_opus_head(bytes: &[u8]) -> Option<ParsedOpusHead> {
    if bytes.len() < 19 || &bytes[0..8] != b"OpusHead" || bytes[8] != 1 {
        return None;
    }
    let channel_count = bytes[9];
    if channel_count == 0 {
        return None;
    }
    let input_sample_rate = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    Some(ParsedOpusHead {
        channel_count,
        input_sample_rate,
    })
}

/// object の properties バイト列から PROP_AUDIO_CONFIG のバイト列を取り出す
///
/// LocProperties のデコード失敗は `None` を返し、検証は静かにスキップされる
/// (不正 properties は `extract_timestamp_timescale` と同じ扱い)。
fn extract_audio_config(properties_bytes: Option<&[u8]>) -> Option<Vec<u8>> {
    let bytes = properties_bytes?;
    let Ok((props, _)) = LocProperties::decode(bytes) else {
        return None;
    };
    for p in props.iter() {
        if p.prop_id == PROP_AUDIO_CONFIG
            && let LocPropertyValue::Bytes(b) = &p.value
        {
            return Some(b.clone());
        }
    }
    None
}

/// OpusHead と catalog の整合を検証し、不一致の警告メッセージを返す (デコードは停止しない)
///
/// 戻り値の各要素は呼び出し側で警告ログとして出力される。
/// - Input Sample Rate の不一致: RFC 7845 §5.1 により情報提供目的であり再生に影響しない
///   ("This field is _not_ the sample rate to use for playback of the encoded data.")。
///   値 0 (unspecified) は不一致としない。警告文に「再生には影響しない」旨を含める
/// - Channel Count の不一致: libopus が自動でチャンネル変換して再生を継続するため
///   エラーにはならないが、原因不明の音質変化になるため警告で知らせる
fn validate_audio_config(
    head: &ParsedOpusHead,
    catalog_sample_rate: u32,
    catalog_channels: u8,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if head.input_sample_rate != 0 && head.input_sample_rate != catalog_sample_rate {
        warnings.push(format!(
            "Audio Config (OpusHead) input sample rate {} does not match catalog sample rate {} (does not affect playback)",
            head.input_sample_rate,
            catalog_sample_rate,
        ));
    }
    if head.channel_count != catalog_channels {
        warnings.push(format!(
            "Audio Config (OpusHead) channel count {} does not match catalog channelConfig {} (libopus will convert channels; this does not fail playback)",
            head.channel_count,
            catalog_channels,
        ));
    }
    warnings
}
/// 1 本の audio subgroup ストリームから Opus パケットを順次デコードする
///
/// LOC draft-ietf-moq-loc-04 §4.1 (Application with one audio track) では「1 audio chunk = 1 Object = 1 Group」と例示されているが、
/// 防御的に subgroup 内の object をすべて処理する。
///
/// `audio_config_handled` は呼び出し側 (handle_incoming_stream。ストリームごとに呼ばれる) で
/// 保持する処理済みフラグで、PROP_AUDIO_CONFIG を含むオブジェクトを受信した最初の 1 回のみ
/// OpusHead と catalog の整合を検証する。パース失敗時もフラグは立てる
/// (不正 OpusHead で警告を 1 回に抑える)。現行 publisher は OpusHead をセッションで 1 回しか
/// 送らない (publisher 側の `audio_config_sent` フラグ) ため、実質セッション全体で 1 回きりになる。
/// `is_opus_codec` が false の場合は OpusHead を解釈せず検証をスキップする
/// (Audio Config の中身はコーデック依存のため。catalog の audio track は opus のみ選択される
/// (receive_catalog の `starts_with("opus")` 抽出) ためコードパス上常に true だが、
/// 非 opus codec 対応時の防御的ガードとして判定を残す)。
#[expect(clippy::too_many_arguments)]
async fn decode_audio_stream(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    sg_decoder: &mut SubgroupStreamDecoder,
    opus_decoder: &mut OpusDecoder,
    audio_tx: &std::sync::mpsc::Sender<DecodedAudioFrame>,
    audio_config_handled: &mut bool,
    is_opus_codec: bool,
    catalog_sample_rate: u32,
    catalog_channels: u8,
) -> u64 {
    let mut chunks: u64 = 0;
    loop {
        // 帰属判定の結果。`FilteredOut` / `Discarded` の Object は Application へ渡さない。
        let (obj, deliver) = loop {
            match sg_decoder.try_decode_object() {
                Ok(Some(o)) => {
                    let deliver = match data_plane.recv_subgroup_object(stream_id, &o) {
                        Ok(TrackDataAcceptance::Accepted) => true,
                        // header 受理済み stream では `UnknownTrackAlias` は返らない契約だが、
                        // 型上あり得るため防御的に破棄する
                        Ok(
                            TrackDataAcceptance::UnknownTrackAlias
                            | TrackDataAcceptance::FilteredOut
                            | TrackDataAcceptance::Discarded,
                        ) => false,
                        Err(e) => {
                            tracing::warn!("Failed to register audio object: {e}");
                            return chunks;
                        }
                    };
                    break (o, deliver);
                }
                Ok(None) => {
                    match receive_registered_stream_data(stream, data_plane, stream_id).await {
                        Ok(Some(data)) => sg_decoder.push(&data),
                        Ok(None) => return chunks,
                        Err(e) => {
                            tracing::warn!("Failed to read audio object: {e}");
                            return chunks;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to decode audio object: {e}");
                    let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
                    return chunks;
                }
            }
        };
        // payload_length == 0 のオブジェクト (Object Status のみ) は実データが無く、
        // Audio Config を付与する意味がない (現行 publisher は実 payload 付きオブジェクトに
        // 付与する) ため、検証の前にスキップする
        if obj.payload_length == 0 {
            continue;
        }
        // `FilteredOut` / `Discarded` の Object でも payload は wire 上に存在するため
        // 読み出して消費する。残すと次の `try_decode_object` が ProtocolViolation になる。
        let payload = loop {
            if let Some(p) = sg_decoder.try_read_payload() {
                break p;
            }
            match receive_registered_stream_data(stream, data_plane, stream_id).await {
                Ok(Some(data)) => sg_decoder.push(&data),
                Ok(None) => return chunks,
                Err(e) => {
                    tracing::warn!("Failed to read audio payload: {e}");
                    return chunks;
                }
            }
        };
        if !deliver {
            continue;
        }
        // Audio Config (OpusHead) を含むオブジェクトを受信した最初の 1 回のみ検証する
        // (途中参加で OpusHead を受信しない場合はスキップされる)
        if !*audio_config_handled
            && let Some(config_bytes) = extract_audio_config(obj.properties_bytes.as_deref())
        {
            *audio_config_handled = true;
            if is_opus_codec {
                match parse_opus_head(&config_bytes) {
                    Some(head) => {
                        for warning in
                            validate_audio_config(&head, catalog_sample_rate, catalog_channels)
                        {
                            tracing::warn!("{warning}");
                        }
                    }
                    None => {
                        tracing::warn!(
                            "Failed to parse Audio Config (OpusHead): len={}, head={:02x?}",
                            config_bytes.len(),
                            &config_bytes[..config_bytes.len().min(8)],
                        );
                    }
                }
            } else {
                tracing::debug!("Audio Config received but codec is not opus; skipped validation");
            }
        }
        let (timestamp, timescale) = extract_timestamp_timescale(obj.properties_bytes.as_deref());
        let pcm = match opus_decoder.decode(&payload) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Opus decode error: {e}, payload_len={}", payload.len());
                continue;
            }
        };
        let sample_rate = opus_decoder.sample_rate();
        let channels = opus_decoder.channels();
        let pts_us = audio_pts_us(timestamp, timescale);
        let frame = DecodedAudioFrame {
            pcm,
            sample_rate,
            channels,
            pts_us,
        };
        if audio_tx.send(frame).is_err() {
            return chunks;
        }
        chunks += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // timestamp が None なら PTS は 0 になる
    #[test]
    fn audio_pts_us_returns_zero_when_timestamp_is_none() {
        assert_eq!(
            audio_pts_us(None, Some(48_000)),
            0,
            "timestamp が None なら 0"
        );
    }

    // timescale が 0 ならゼロ除算を避けて PTS は 0 になる
    #[test]
    fn audio_pts_us_returns_zero_when_timescale_is_zero() {
        assert_eq!(audio_pts_us(Some(100), Some(0)), 0, "timescale が 0 なら 0");
    }

    // timescale が None ならフォールバック (48 kHz) で計算する
    #[test]
    fn audio_pts_us_uses_fallback_sample_rate_when_timescale_is_none() {
        // 48000 / 48000 * 1_000_000 = 1_000_000 (1 秒)
        assert_eq!(
            audio_pts_us(Some(48_000), None),
            1_000_000,
            "timescale が None なら 48 kHz フォールバックで計算する"
        );
    }

    // 通常の値で正しくマイクロ秒に変換する
    #[test]
    fn audio_pts_us_converts_normal_values() {
        // 1_000_000 / 1_000_000 * 1_000_000 = 1_000_000
        assert_eq!(
            audio_pts_us(Some(1_000_000), Some(1_000_000)),
            1_000_000,
            "timestamp と timescale が等しければ 1 秒 (1_000_000 us)"
        );
    }

    // 巨大な Timestamp でもオーバーフローせず i64::MAX に飽和する (旧実装は debug で panic した)
    #[test]
    fn audio_pts_us_saturates_on_huge_timestamp() {
        // 1e13 * 1_000_000 = 1e19 > i64::MAX (約 9.2e18) なので飽和する
        assert_eq!(
            audio_pts_us(Some(10_000_000_000_000), Some(1)),
            i64::MAX,
            "巨大な Timestamp は i64::MAX に飽和する"
        );
        // u64::MAX でも panic せず飽和する
        assert_eq!(
            audio_pts_us(Some(u64::MAX), Some(1)),
            i64::MAX,
            "u64::MAX でも panic せず飽和する"
        );
    }

    // 巨大な Timescale でも符号ラップせず正しく計算する
    #[test]
    fn audio_pts_us_does_not_wrap_on_huge_timescale() {
        // u64::MAX * 1_000_000 / u64::MAX = 1_000_000 (u128 中間計算でオーバーフローしない)
        assert_eq!(
            audio_pts_us(Some(u64::MAX), Some(u64::MAX)),
            1_000_000,
            "巨大な Timescale でも符号ラップせず正しく計算する"
        );
    }

    // i64 範囲に収まる大きな値は飽和せずそのまま返す
    #[test]
    fn audio_pts_us_returns_large_value_without_saturation() {
        // 9e12 * 1_000_000 = 9e18 < i64::MAX (約 9.223e18) なので飽和しない
        assert_eq!(
            audio_pts_us(Some(9_000_000_000_000), Some(1)),
            9_000_000_000_000_000_000,
            "i64 に収まる大きな値は飽和せずそのまま返す"
        );
    }

    /// テスト用: OpusHead を構築する (RFC 7845 §5.1 のフォーマット)
    ///
    /// publisher 側 (examples/moqt-publisher の `build_opus_head`) と同じ形式。
    fn build_test_opus_head(sample_rate: u32, channels: u8) -> Vec<u8> {
        let mut head = Vec::with_capacity(19);
        head.extend_from_slice(b"OpusHead");
        head.push(1);
        head.push(channels);
        head.extend_from_slice(&0u16.to_le_bytes());
        head.extend_from_slice(&sample_rate.to_le_bytes());
        head.extend_from_slice(&0u16.to_le_bytes());
        head.push(0);
        head
    }

    /// OpusHead パース: 正常系 (Input Sample Rate / Channel Count が取り出せる)
    #[test]
    fn parse_opus_head_extracts_fields() {
        let head = build_test_opus_head(48_000, 1);
        let parsed = parse_opus_head(&head).expect("正しい OpusHead はパースできること");
        assert_eq!(
            parsed.input_sample_rate, 48_000,
            "Input Sample Rate が取り出せること"
        );
        assert_eq!(parsed.channel_count, 1, "Channel Count が取り出せること");
    }

    /// OpusHead パース: 19 バイト未満は失敗
    #[test]
    fn parse_opus_head_rejects_short_head() {
        let head = build_test_opus_head(48_000, 1);
        assert!(
            parse_opus_head(&head[..18]).is_none(),
            "19 バイト未満はパース失敗すること"
        );
    }

    /// OpusHead パース: magic 不一致は失敗
    #[test]
    fn parse_opus_head_rejects_bad_magic() {
        let mut head = build_test_opus_head(48_000, 1);
        head[0] = b'X';
        assert!(
            parse_opus_head(&head).is_none(),
            "magic 不一致はパース失敗すること"
        );
    }

    /// OpusHead パース: version ≠ 1 は失敗
    #[test]
    fn parse_opus_head_rejects_bad_version() {
        let mut head = build_test_opus_head(48_000, 1);
        head[8] = 2;
        assert!(
            parse_opus_head(&head).is_none(),
            "version ≠ 1 はパース失敗すること"
        );
    }

    /// OpusHead パース: Channel Count = 0 は失敗
    #[test]
    fn parse_opus_head_rejects_zero_channel_count() {
        let head = build_test_opus_head(48_000, 0);
        assert!(
            parse_opus_head(&head).is_none(),
            "Channel Count = 0 はパース失敗すること"
        );
    }

    /// OpusHead パース: 19 バイト超 (Channel Mapping Family = 1 等) は受理される
    ///
    /// RFC 7845 §5.1 のヘッダは 19 バイト以上であり、拡張フィールドは無視してよい。
    /// (テストデータの拡張部は Channel Mapping Family = 1 の想定で、形式は不問の
    /// 追加バイトとして扱う)
    #[test]
    fn parse_opus_head_accepts_longer_head() {
        let mut head = build_test_opus_head(48_000, 2);
        head.extend_from_slice(&[1, 0, 0, 0]); // Channel Mapping Family = 1 + mapping (C=2 で 4 バイト)
        let parsed = parse_opus_head(&head).expect("19 バイト超の OpusHead は受理されること");
        assert_eq!(parsed.input_sample_rate, 48_000);
        assert_eq!(parsed.channel_count, 2);
    }

    /// PROP_AUDIO_CONFIG の抽出: 付与されていればバイト列が取り出せる
    #[test]
    fn extract_audio_config_returns_bytes_when_present() {
        use shiguredo_moqt::loc::{LocProperty, LocPropertyValue, PROP_AUDIO_CONFIG};
        let head = build_test_opus_head(48_000, 1);
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_AUDIO_CONFIG,
            value: LocPropertyValue::Bytes(head.clone()),
        });
        let encoded = props.encode().expect("encode に成功すること");
        let extracted = extract_audio_config(Some(&encoded));
        assert_eq!(
            extracted,
            Some(head),
            "Audio Config のバイト列が取り出せること"
        );
    }

    /// PROP_AUDIO_CONFIG の抽出: 無ければ None
    #[test]
    fn extract_audio_config_returns_none_when_absent() {
        let props = LocProperties::new();
        let encoded = props.encode().expect("encode に成功すること");
        assert_eq!(
            extract_audio_config(Some(&encoded)),
            None,
            "Audio Config が無ければ None であること"
        );
        assert_eq!(
            extract_audio_config(None),
            None,
            "properties 自体が無ければ None であること"
        );
    }

    /// PROP_AUDIO_CONFIG の抽出: 壊れた properties は None (検証はスキップされる)
    #[test]
    fn extract_audio_config_returns_none_on_bad_properties() {
        assert_eq!(
            extract_audio_config(Some(&[0xFF, 0xFF, 0xFF])),
            None,
            "壊れた properties は None であること"
        );
    }

    /// 検証: 整合する場合は警告が 0 件
    #[test]
    fn validate_audio_config_matching_values_no_warnings() {
        let head = ParsedOpusHead {
            channel_count: 1,
            input_sample_rate: 48_000,
        };
        let warnings = validate_audio_config(&head, 48_000, 1);
        assert!(
            warnings.is_empty(),
            "整合する場合は警告が 0 件であること: {warnings:?}"
        );
    }

    /// 検証: Input Sample Rate 不一致で警告が出ること (値 0 は unspecified のため不一致としない)
    #[test]
    fn validate_audio_config_sample_rate_mismatch_warns() {
        let head = ParsedOpusHead {
            channel_count: 1,
            input_sample_rate: 44_100,
        };
        let warnings = validate_audio_config(&head, 48_000, 1);
        assert_eq!(
            warnings.len(),
            1,
            "Sample Rate 不一致で警告が 1 件であること"
        );
        assert!(
            warnings[0].contains("input sample rate 44100")
                && warnings[0].contains("catalog sample rate 48000")
                && warnings[0].contains("does not affect playback"),
            "警告に OpusHead と catalog の両方の値と再生非影響の旨が含まれること: {}",
            warnings[0],
        );
        // unspecified (0) は不一致としない
        let head_zero = ParsedOpusHead {
            channel_count: 1,
            input_sample_rate: 0,
        };
        assert!(
            validate_audio_config(&head_zero, 48_000, 1).is_empty(),
            "Input Sample Rate 0 (unspecified) は不一致としないこと"
        );
    }

    /// 検証: Channel Count 不一致で警告が出ること
    #[test]
    fn validate_audio_config_channel_mismatch_warns() {
        let head = ParsedOpusHead {
            channel_count: 2,
            input_sample_rate: 48_000,
        };
        let warnings = validate_audio_config(&head, 48_000, 1);
        assert_eq!(
            warnings.len(),
            1,
            "Channel Count 不一致で警告が 1 件であること"
        );
        assert!(
            warnings[0].contains("channel count 2") && warnings[0].contains("channelConfig 1"),
            "警告に OpusHead と catalog の両方の値が含まれること: {}",
            warnings[0],
        );
    }

    /// 検証: Sample Rate と Channel Count の両方が不一致なら警告が 2 件
    #[test]
    fn validate_audio_config_both_mismatch_warns_twice() {
        let head = ParsedOpusHead {
            channel_count: 2,
            input_sample_rate: 44_100,
        };
        let warnings = validate_audio_config(&head, 48_000, 1);
        assert_eq!(
            warnings.len(),
            2,
            "両方の不一致で警告が 2 件であること: {warnings:?}"
        );
    }
}
