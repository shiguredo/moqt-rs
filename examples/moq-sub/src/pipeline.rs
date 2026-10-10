//! subscriber の全体パイプライン
//!
//! 接続 → SETUP → カタログ購読 (FILL_PARAMETERS 付き SUBSCRIBE) → ビデオ / オーディオ SUBSCRIBE
//! → データストリーム受信
//! → デコード (AV1 / H.264 / H.265 / Opus) → フレーム送出の流れを、
//! `shiguredo_moqt::session::core::Session` を駆動する
//! [`tokio_moq::moqt_client::MoqtClient`] と結線する。
//!
//! `--mp4` 指定時は受信したエンコード済みサンプルを `mp4` モジュールの writer へ渡して
//! MP4 ファイルへ保存する。`--mp4` と `--no-play` を併用した場合はデコードをスキップして
//! 保存だけを行う。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use bytes::Bytes;
use shiguredo_moqt::error::{
    MessageError, SESSION_KEY_VALUE_FORMATTING_ERROR, SESSION_PROTOCOL_VIOLATION,
};
use shiguredo_moqt::loc::{
    LocProperties, LocPropertyValue, PROP_AUDIO_CONFIG, PROP_TIMESCALE, PROP_TIMESTAMP,
    PROP_VIDEO_CONFIG, PROP_VIDEO_FRAME_MARKING,
};
use shiguredo_moqt::pending_subgroup_buffer::{
    DEFAULT_PENDING_SUBGROUP_BUFFER_OPTIONS, PendingNotifyReason, PendingSubgroupBuffer,
};
use shiguredo_moqt::session::types::{SessionError, TrackDataAcceptance};
use shiguredo_moqt::stream::subgroup::SubgroupHeader;
use shiguredo_moqt::video_decode_order::{
    VideoDecodeOrder, VideoObjectAdmission, VideoObjectPosition, prior_object_id_gap_of,
};
use shiguredo_moqt::{
    message::common::Location, message_parameter::LocationFilter,
    message_parameter::MessageParameter, message_parameter::MessageParameterValue,
    message_parameter::MessageParameters, message_parameter::PARAM_FILL_PARAMETERS,
    message_parameter::PARAM_LOCATION_FILTER, msf::MSF_CATALOG_TRACK_NAME, msf::MsfTrack,
    name::serialize_namespace, session::types::DataStreamId, session::types::SessionEvent,
    stream::decoder::DecodedFetchEntry, stream::decoder::FetchStreamDecoder,
    stream::decoder::SubgroupStreamDecoder,
};

use crate::catalog;
use crate::cli::Config;
use crate::decoder::opus::OpusDecoder;
use crate::decoder::{self, DecodedAudioFrame, DecodedVideoFrame};
use crate::error::{Error, Result};
use crate::mp4::{
    self, AudioSample, AudioSetup, Recorder, RecorderSender, Setup, VideoSample, VideoSetup,
};
use crate::stream_reader::{self, StreamType};

/// 表示待ちの映像フレーム数の上限
///
/// これを超えたら映像の group をまるごと捨てる。デコードは表示より速く進むため、
/// 上限が無いと待ちフレームが増え続けて CPU を飽和させ、同じ runtime で動く
/// QUIC エンドポイントの I/O が飢えて受信パケットが落ちる。
/// 捨てる単位を group (stream) にするのは、途中のフレームを捨てると
/// 後続のフレームが参照フレームを失って復号できないためである。
/// 録画中 (`--mp4`) は group を捨てずにデコードだけをスキップし、録画を継続する。
const MAX_DISPLAY_BACKLOG: i64 = 20;

/// デコード済み映像フレームの送り先
struct FrameSink<'a> {
    frame_tx: &'a std::sync::mpsc::Sender<DecodedVideoFrame>,
    /// 表示待ちのフレーム数 (受信側が増やし、表示側が減らす)
    display_backlog: &'a std::sync::atomic::AtomicI64,
}

/// stream task の終了を待つ上限
///
/// STOP_SENDING を送っても peer が data stream を reset しない場合に、shutdown の完了
/// (録画中は finalize) に到達できなくならないようにするための保険。
const STREAM_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// catalog の更新を stream task から main ループへ渡すチャネルの容量
///
/// catalog は 1 Group あたり数 Object であり、容量が小さいと埋まった時点で
/// stream task が待つ (送信側の backpressure)。数 Object ぶんの余裕を持たせる。
const CATALOG_CHANNEL_SIZE: usize = 8;

/// 同時に処理する data stream 数の上限
///
/// 音声は 1 object = 1 stream、映像は 1 group = 1 stream で届く。制限しないと
/// デコードが CPU を使い切り、同じ runtime で動く QUIC エンドポイントの I/O が
/// 飢えて受信パケットが落ちる。
const MAX_CONCURRENT_STREAMS: usize = 4;
use tokio_moq::Transport;
use tokio_moq::connect_with_fallback;
use tokio_moq::error::TransportError;
use tokio_moq::host_from_authority;
use tokio_moq::moqt_client::{ClientEvent, DataPlaneHandle, MoqtClient, StreamRead};
use tokio_moq::quic;
use tokio_moq::transport;

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

/// 停滞の切り分け用の診断ログを出すかどうか。
///
/// `MOQT_STREAM_DIAG=1` を設定したときだけ有効にする。100ms ごとに
/// 受信ループの内訳を出すため、常時有効にすると計測そのものが結果を歪める。
fn stream_diag_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("MOQT_STREAM_DIAG").is_ok_and(|v| v == "1"))
}

/// 終了コード付きでセッションを閉じ、結果をログに残す
///
/// 失敗しても `?` では伝播させない (閉じた事実を `Session closed: {:#x} {}` の形式で
/// 記録して終了する)。呼び出し側は「送信して閉じた」ことを `session_terminated` に反映する。
async fn close_session(client: &mut MoqtClient, termination: SessionError) {
    if let Err(e) = client.close(termination.code, termination.reason).await {
        tracing::warn!("Failed to close session: {e}");
    }
    tracing::warn!(
        "Session closed: {:#x} {}",
        termination.code,
        termination.reason,
    );
}

/// 正常終了の後始末で STOP_SENDING を送るか
///
/// peer が subscription / session を終了させた場合 (購読は既に Terminated で、
/// draft-ietf-moq-transport-22 §6.4.2.3 (Request Cancellation and Rejection) により
/// STOP_SENDING は拒否される) と、自側で終了コード付きに閉じた場合 (session が
/// Closing / Closed で状態機械に拒否される) は送らない。
/// GOAWAY は subscription state に影響しないため、この条件には含めない。
fn should_stop_sending(peer_ended: bool, session_terminated: bool) -> bool {
    !peer_ended && !session_terminated
}

/// 正常終了の後始末で GOAWAY と正常 close (`close(0, "")`) を送るか
///
/// 自側で終了コード付きに閉じた場合は送らない (閉じた session への送信は拒否され、
/// 誤解を招く警告ログになる)。
fn should_close_gracefully(session_terminated: bool) -> bool {
    !session_terminated
}

/// decode 失敗をセッション終了コードへ写す
///
/// library の data plane が使う `session_error_from_data_message` と同じ写像である
/// (`src/session/data.rs`)。
///
/// draft-ietf-moq-transport-22 §8.3 (Key-Value-Pair Structure) が MUST を定める書式違反は
/// KEY_VALUE_FORMATTING_ERROR (0x6)、それ以外の decode 失敗 (`UnexpectedEof` /
/// `ProtocolViolation` など) は PROTOCOL_VIOLATION (0x3) で閉じる
/// (コードは §12.2 (Session Termination Codes))。
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

/// LOC Properties の decode 失敗を main ループへ伝え、セッション終了を依頼する
///
/// 呼び出し側はこの後その stream の処理を打ち切る。transport close を送出できるのは
/// `MoqtClient` を所有する main ループだけであり (`DataPlaneHandle` は `Session` しか
/// 共有していない)、stream task から直接 `Session::close` を呼ぶと main ループが
/// `SessionEvent::CloseSession` を観測できないまま accept 分岐が接続クローズを拾い、
/// `Session closed: ...` のログが落ちる。そのため終了コードと理由を渡し、close は
/// I/O 層である main ループが行う。
///
/// `try_send` が失敗するのは、別の終了依頼が既に積まれている (容量 1 のため理由を問わず
/// 2 件目は入らない) か、main ループが受信ループを終えて受信側を drop した場合である。
/// 複数の stream が同時に失敗した場合は最初の 1 件のコードで閉じる。受信ループの終了後は
/// join の前に 1 件だけ回収し、それ以降に届いた依頼は破棄される。
fn request_session_termination(
    termination_tx: &tokio::sync::mpsc::Sender<SessionError>,
    error: &MessageError,
) {
    let code = session_error_code(error);
    let reason = error.reason();
    if termination_tx
        .try_send(SessionError::new(code, reason))
        .is_err()
    {
        tracing::debug!("Session termination is already requested or the main loop has stopped");
        return;
    }
    tracing::warn!("Closing session on LOC property error: {code:#x} {reason}");
}

/// session で共有する保留バッファ
///
/// Track Alias 未確立の Subgroup ストリームを保持するバッファは、session (run) スコープで
/// 1 つ共有する。stream task ごとに持つと per-session の上限が per-stream の上限としてしか
/// 働かないためである。各 stream task は自分の entry の識別子を保持し、
/// [`shiguredo_moqt::pending_subgroup_buffer::PendingSubgroupBuffer::take_ready_for`] で
/// 自分の通知だけを引き取る。
type SharedPendingSubgroupBuffer = std::sync::Arc<tokio::sync::Mutex<PendingSubgroupBuffer>>;

/// 受け入れた data stream の処理タスクを起動する。
///
/// `permit` はこの stream の処理枠である。タスクの終了時に手放すので、
/// 処理中は同時実行数が `MAX_CONCURRENT_STREAMS` を超えない。
#[expect(clippy::too_many_arguments)]
fn spawn_stream_task(
    join_set: &mut tokio::task::JoinSet<()>,
    task_monitor: &tokio_metrics::TaskMonitor,
    stream: transport::RecvStream,
    permit: tokio::sync::OwnedSemaphorePermit,
    stream_num: u64,
    track_map: &HashMap<u64, TrackKind>,
    data_plane: &DataPlaneHandle,
    frame_tx: &std::sync::mpsc::Sender<DecodedVideoFrame>,
    audio_tx: &std::sync::mpsc::Sender<DecodedAudioFrame>,
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
    audio_params: Option<(u32, u8)>,
    display_backlog: &std::sync::Arc<std::sync::atomic::AtomicI64>,
    video_decode_order: &std::sync::Arc<tokio::sync::Mutex<VideoDecodeOrder>>,
    audio_decoder: Option<&std::sync::Arc<tokio::sync::Mutex<OpusDecoder>>>,
    audio_config_handled: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    termination_tx: &tokio::sync::mpsc::Sender<SessionError>,
    catalog_tx: &tokio::sync::mpsc::Sender<catalog::CatalogObject>,
    pending_subgroups: &SharedPendingSubgroupBuffer,
    playback: bool,
    recorder: Option<&RecorderSender>,
) {
    let track_map = track_map.clone();
    let data_plane = data_plane.clone();
    let frame_tx = frame_tx.clone();
    let audio_tx = audio_tx.clone();
    let codec = video_codec.map(str::to_owned);
    let audio_codec = audio_codec.map(str::to_owned);
    let backlog = std::sync::Arc::clone(display_backlog);
    let video_decode_order = std::sync::Arc::clone(video_decode_order);
    let audio_decoder = audio_decoder.cloned();
    let audio_config_handled = std::sync::Arc::clone(audio_config_handled);
    let termination_tx = termination_tx.clone();
    let catalog_tx = catalog_tx.clone();
    let pending_subgroups = std::sync::Arc::clone(pending_subgroups);
    let recorder = recorder.cloned();
    join_set.spawn(task_monitor.clone().instrument(async move {
        handle_incoming_stream(
            stream,
            data_plane,
            &track_map,
            codec.as_deref(),
            audio_codec.as_deref(),
            audio_params,
            &frame_tx,
            &audio_tx,
            stream_num,
            backlog,
            &video_decode_order,
            audio_decoder.as_ref(),
            &audio_config_handled,
            &termination_tx,
            &catalog_tx,
            &pending_subgroups,
            playback,
            recorder.as_ref(),
        )
        .await;
        // stream の処理が終わるまで枠を保持する
        drop(permit);
    }));
}

/// トラック種別 (data stream 処理で使用)
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
enum TrackKind {
    Video,
    Audio,
    /// カタログトラック
    ///
    /// 購読は LARGEST_OBJECT の取得と、以後に配られるカタログ (独立カタログと delta) の
    /// 受信に使う (draft-ietf-moq-msf-01 §5 (Catalog))。本 example は起動時に解決した
    /// トラックを再生し続けるため、購読で届くカタログは適用せずに捨てる。
    Catalog,
}

/// パイプラインを実行する
///
/// `config` は接続先・トランスポート・購読するトラック・録画と再生の指定を持つ。
/// `frame_tx` と `audio_tx` はデコード済みフレームの受け渡しに使う。再生しない
/// 呼び出し側は受け取ったフレームを破棄すればよい。
/// `task_monitor` は生成されるタスクのメトリクスを収集する。
/// `shutdown_monitor` は graceful shutdown の契機を受信する。
/// `display_backlog` は表示待ちの映像フレーム数であり、閾値を超えると古い group を捨てる。
/// 録画中は読み切って保存するため捨てない。
/// `player_stop` は再生側の終了を伝える。sender が drop されるとパイプラインは停止する。
#[expect(clippy::too_many_arguments)]
pub async fn run(
    config: Config,
    frame_tx: std::sync::mpsc::Sender<DecodedVideoFrame>,
    audio_tx: std::sync::mpsc::Sender<DecodedAudioFrame>,
    task_monitor: tokio_metrics::TaskMonitor,
    mut shutdown_monitor: tokio_utils::ShutdownMonitor,
    display_backlog: std::sync::Arc<std::sync::atomic::AtomicI64>,
    target_latency_ms: std::sync::Arc<std::sync::atomic::AtomicI64>,
    mut player_stop: tokio::sync::oneshot::Receiver<()>,
) -> Result<()> {
    // 再生を行わない場合はデコードをスキップし、録画だけを行う
    let playback = !config.no_play;
    // 録画は専用スレッドが所有する。run の早期 return でも Recorder の Drop が finalize する。
    // 録画対象のトラック情報はカタログ取得後に確定するため、起動はその時点で行う。
    let recording_requested = config.mp4.is_some();

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
    let (mut client, mut recv_acceptor) =
        connect_with_fallback(&config.url.authority, move |socket_addr| {
            // 試行ごとに接続を組み立てるため、設定は複製して `move` で取り込む
            let cfg = connection_config.clone();
            let connect_monitor = connect_monitor.clone();
            let authority_url = cfg.url.authority.clone();
            async move {
                let connected = match cfg.transport {
                    Transport::Quic => {
                        let (connection, stop_sending_rx) =
                            quic::connect(socket_addr, server_name, cfg.cert.as_deref()).await?;
                        MoqtClient::establish_quic(
                            connection,
                            stop_sending_rx,
                            &cfg.url.path,
                            &authority_url,
                            "moq-sub",
                            &cfg.url.c4m_tokens,
                            &connect_monitor,
                        )
                        .await?
                    }
                    Transport::WtH3 => {
                        let mut client_config =
                            tokio_moq::webtransport_h3::ClientConfig::new(socket_addr, server_name)
                                // :authority は target URI の authority を URL の表記どおりに渡す (draft-ietf-webtrans-http3-16 §3.2)
                                .authority(&authority_url)
                                .enable_webtransport(
                                    shiguredo_http3::webtransport::Settings::new()
                                        .wt_enabled(shiguredo_http3::VarInt::from_static(1)),
                                )
                                // subscriber は datagram を継続受信するためバックグラウンドタスクを起動する
                                .receive_datagrams();
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
                        let wt_session = tokio_moq::webtransport_h3::WtClient::connect(
                            client_config,
                            &cfg.url.path,
                        )
                        .await?;
                        MoqtClient::establish_wt(
                            wt_session,
                            "moq-sub",
                            &cfg.url.c4m_tokens,
                            &connect_monitor,
                        )
                        .await?
                    }
                    Transport::WtH2 => {
                        let mut client_config =
                            tokio_moq::webtransport_h2::ClientConfig::new(socket_addr, server_name)
                                // :authority は target URI の authority を URL の表記どおりに渡す (draft-ietf-webtrans-http2-15 §3.2)
                                .authority(&authority_url)
                                // subscriber は datagram を継続受信するためバックグラウンドタスクを起動する
                                .receive_datagrams();
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
                        MoqtClient::establish_wt_h2(
                            wt_session,
                            "moq-sub",
                            &cfg.url.c4m_tokens,
                            &connect_monitor,
                        )
                        .await?
                    }
                };
                Ok::<_, Error>(connected)
            }
        })
        .await?;

    // CLI で §8.8 表現としてパース済みの namespace をそのまま使う
    let namespace = config.namespace.clone();
    let data_plane = client.data_plane();

    // 2. タイムアウト設定
    client.set_control_message_timeout_ms(Some(30000));
    client.set_data_stream_timeout_ms(Some(30000));
    tracing::info!(
        "Session timeouts: control={:?}ms, data={:?}ms",
        client.control_message_timeout_ms(),
        client.data_stream_timeout_ms(),
    );

    // 3. カタログを FILL_PARAMETERS 付き SUBSCRIBE で取得する
    //
    // MSF は catalog の取得に "SUBSCRIBE with a Joining FETCH (offset = 0)" を MUST とする
    // (draft-ietf-moq-msf-01 §5 (Catalog))。Joining FETCH は draft-20 で削除され
    // (draft-ietf-moq-transport-22 Appendix A.3 の "Since draft-ietf-moq-transport-19" に記録)、
    // §3.5 (Joining an Ongoing Track) が示す購読パターンに置き換わった:
    //
    // 1. catalog track を Location Filter Next Object (§9.20.9 Table 6 の Type 0x05) と
    //    FILL_PARAMETERS (§9.20.15) で SUBSCRIBE する。FILL 内側の Location Filter は
    //    現在 Group の先頭から埋める Relative Start (Type 0x01 の StartGroup=1) にする
    // 2. publisher は fill range を fill fetch stream (§3.4) で送る。購読より前に publish
    //    された独立カタログがこれで届き、以後の delta は購読で届く。どちらも同じ
    //    `CatalogState` へ到着順に適用する
    //
    // Group ID を 0 と仮定してはならない。publisher は再起動時に以前 publish したどの
    // Group ID よりも大きい値から始める MUST があり (draft-ietf-moq-msf-01 §6.1
    // (Group numbering))、Unix epoch ミリ秒を開始値にする実装が一般的である。
    //
    // C4M 認可トークンは `MoqtClient` が `moqt` クレームを見て付けるため、ここでは
    // 指定しない (draft-ietf-moq-c4m-01 §1.1)。
    let catalog_params = catalog_subscribe_parameters();
    let catalog_sub = client
        .subscribe_track_with_parameters(
            namespace.clone(),
            MSF_CATALOG_TRACK_NAME.to_vec(),
            catalog_params,
        )
        .await?;
    tracing::info!(
        "Subscribed to catalog track: alias={}, request_id={}, largest_object={:?}",
        catalog_sub.track_alias,
        catalog_sub.request_id,
        catalog_sub.largest_object,
    );

    // fill fetch stream と購読で届くカタログを読み、MSF の配置規則で適用する
    let catalog_resolution = receive_catalog(
        &mut recv_acceptor,
        &data_plane,
        catalog_sub.request_id,
        catalog_sub.track_alias,
        // delta の namespace 継承は catalog JSON の `namespace` との文字列比較になるため、
        // publisher が書く表現と同じ draft-ietf-moq-transport-22 §8.8 の表現で渡す
        serialize_namespace(&namespace),
    )
    .await?;
    let CatalogResolution {
        state: mut catalog_state,
        video: video_info,
        audio: audio_info,
        target_latency_ms: catalog_target_latency_ms,
    } = catalog_resolution;
    // カタログの targetLatency をプレイヤーへ渡す。プレイヤーは時間軸の表示の遅れの
    // 下限として反映する (draft-ietf-moq-msf-01 §5.2.8)
    if let Some(catalog_target_latency_ms) = catalog_target_latency_ms {
        target_latency_ms.store(
            catalog_target_latency_ms,
            std::sync::atomic::Ordering::Relaxed,
        );
        tracing::info!("Using catalog targetLatency: {catalog_target_latency_ms}ms");
    }
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
    // カタログの購読は、購読で届くカタログを捨てるだけの stream として扱う
    track_map.insert(catalog_sub.track_alias, TrackKind::Catalog);
    let mut video_codec: Option<String> = None;
    let mut video_request_id: Option<u64> = None;
    let mut audio_request_id: Option<u64> = None;
    // 映像 Object を復号してよいかの判定状態は購読で 1 つ持つ。Group ごとに別の Subgroup
    // ストリームで届くため (draft-ietf-moq-transport-22 §2.2 (Subgroups))、stream ごとに
    // 判定を持つと「前の Group の Object が次の Group のキーフレームより後に届く」場合を
    // 判定できない。映像のトラックを切り替えて decoder を作り直す経路を足すときは、
    // この状態を reset してから次のキーフレームで復号を始め直すこと。
    let video_decode_order = std::sync::Arc::new(tokio::sync::Mutex::new(VideoDecodeOrder::new()));

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

        // draft-20 で Joining FETCH は廃止され fill fetch stream
        // (§3.4) に置き換えられたため、過去 group の取得デモは行わず live のみ受信する。
        // 欠落範囲の補填が必要な場合は SUBSCRIBE 時に FILL_PARAMETERS を付与する。

        // REQUEST_UPDATE で video subscription の subscriber priority を変更するサンプル
        //
        // C4M 認可トークンは `MoqtClient::send_request_update` が付けるため、ここでは
        // 指定しない (draft-ietf-moq-msf-01 §11.4.3)。SUBSCRIBE と同じトークンを
        // REQUEST_UPDATE にも MUST 付与する。
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

        // ビデオデコーダの初期化確認 (再生を行わない場合はデコードしないため省略する)。
        // 録画中はデコーダが使えなくても録画を続けるため、警告に留める。
        if playback {
            match decoder::build_video_decoder(&v.codec) {
                Ok(_probe) => {
                    drop(_probe);
                    tracing::info!("Video decoder available (codec={})", v.codec);
                }
                Err(e) if recording_requested => {
                    tracing::warn!(
                        "Video decoder is unavailable; recording without video playback: {e}"
                    );
                }
                Err(e) => return Err(e),
            }
        }
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
        Some((a.samplerate, channels))
    } else {
        None
    };
    // Audio Config の検証 (codec 判定) に使う catalog の audio codec
    let audio_codec = audio_info.as_ref().map(|a| a.codec.clone());

    // 録画対象のトラック情報を確定してライタースレッドを起動する。stream task が動き出す前に
    // 起動するため、最初のサンプルより先にセットアップが反映される。
    let video_setup = if subscribe_video {
        let v = video_info.as_ref().expect("video_info is Some");
        Some(VideoSetup {
            codec: v.codec.clone(),
            fps: v.fps,
        })
    } else {
        None
    };
    let audio_setup = audio_params.map(|(sample_rate, channels)| AudioSetup {
        sample_rate,
        channels,
    });
    let recorder = match config.mp4.as_deref() {
        Some(path) => Some(Recorder::start(
            path,
            Setup {
                video: video_setup,
                audio: audio_setup,
            },
        )?),
        None => None,
    };
    let recorder_sender = recorder.as_ref().map(|recorder| recorder.sender());

    // 音声は 1 object = 1 subgroup stream で届く (LOC draft-ietf-moq-loc-04 §4.1)。
    // stream ごとにデコーダを作り直すと 20 ms ごとにデコーダ状態が失われ、
    // フレーム境界で波形が不連続になってノイズになる。
    // subscription で 1 つのデコーダを共有し、Audio Config の処理済みフラグも共有する。
    // 録画中はデコーダが使えなくても録画を続けるため、警告に留める。
    let audio_decoder = match (playback, audio_params) {
        (true, Some((sample_rate, channels))) => match OpusDecoder::new(sample_rate, channels) {
            Ok(decoder) => {
                tracing::info!(
                    "Audio decoder available (sample_rate={}, channels={})",
                    sample_rate,
                    channels
                );
                Some(std::sync::Arc::new(tokio::sync::Mutex::new(decoder)))
            }
            Err(e) if recording_requested => {
                tracing::warn!(
                    "Audio decoder is unavailable; recording without audio playback: {e}"
                );
                None
            }
            Err(e) => return Err(e),
        },
        _ => None,
    };
    let audio_config_handled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

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
                        // draft-ietf-moq-transport-22 §11 (Data Streams and Datagrams) に従いセッションクローズになる。
                        if let Err(e) = data_plane_for_datagram.recv_datagram(&payload) {
                            tracing::warn!("Failed to process datagram: {e}");
                        }
                    }
                }
                Err(e) => {
                    // セッション終了 (§6) と接続クローズは期待される終了であるため、
                    // 異常 (warn) ではなく info に揃える
                    if matches!(e, TransportError::ConnectionClosed) {
                        tracing::info!("Datagram receive error: {e}");
                    } else {
                        tracing::warn!("Datagram receive error: {e}");
                    }
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
    // 同時に処理する data stream 数を制限する。
    //
    // 制限しないと AV1 / Opus のデコードが CPU を使い切り、同じ runtime で動く
    // QUIC エンドポイントの I/O が飢えて受信パケットが落ちる。落ちたパケットで
    // relay 側の輻輳ウィンドウが最小値まで崩壊し、配送が止まる。
    //
    // 枠が埋まっている間は新しく accept した stream を `pending_streams` に積み、
    // ループの先頭で枠が空いた分だけ流し込む。`select!` の分岐の中で permit を
    // 待ってはならない。分岐の中で待つと、permit が返るまで `client.next_event()`
    // が poll されず、session の ACK 送信とタイマー処理が止まる。s2n-quic の
    // エンドポイントは `next_event()` の poll で動くため、ここが止まると受信も
    // 送信も完全に停止し、relay 側の輻輳ウィンドウが最小値へ落ちて復帰しない。
    let stream_slots = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_STREAMS));
    // Track Alias 未確立の Subgroup ストリームの保留バッファ。stream task ごとに持つと
    // per-session の上限が per-stream の上限としてしか働かず、未知 alias のストリームが
    // 枠の数だけ同時に届いたときに合計を抑えられないため、session で 1 つ共有する。
    // 各 task は自分の entry の識別子を保持して `take_ready_for` を呼ぶため、他の task の
    // entry を引き取ることはない (バッファ全体を返す `take_ready` は 1 つの待ち手だけが
    // 使う API であり、この example では使わない)。
    //
    // `Mutex` を使うのは、バッファが複数の stream task から共有され、どの操作も await を
    // またがない短い同期処理で完結するためである。状態を 1 つのタスクへ所有させてチャネルで
    // 操作を依頼する構成は、チャンク 1 つごとに往復が増えて受信経路が応答待ちになるため採らない。
    let pending_subgroups: SharedPendingSubgroupBuffer =
        std::sync::Arc::new(tokio::sync::Mutex::new(PendingSubgroupBuffer::new(
            DEFAULT_PENDING_SUBGROUP_BUFFER_OPTIONS,
        )));
    // 枠待ちの stream。accept 済みだがまだ処理を開始していない
    let mut pending_streams: std::collections::VecDeque<transport::RecvStream> =
        std::collections::VecDeque::new();
    // stream task からのセッション終了依頼。close は I/O 層である main ループが行う。
    // 送信側の原本を main ループが保持するため、受信ループが動いている間このチャネルは
    // 閉じない (`select!` の受信分岐は `Some` のみを受ける)。
    let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel::<SessionError>(1);
    // catalog の更新は状態を所有する main ループが適用する。stream task は読み出した
    // Object をこのチャネルへ渡す
    let (catalog_tx, mut catalog_rx) =
        tokio::sync::mpsc::channel::<catalog::CatalogObject>(CATALOG_CHANNEL_SIZE);
    // peer が subscription / session を終了させて受信ループを抜けたか。
    // この場合の subscription は Terminated であり、STOP_SENDING は不要である
    let mut peer_ended = false;
    // GOAWAY を受信したか。GOAWAY は subscription state に影響しないため、peer_ended とは
    // 分けて扱う (購読は Established のままなので cleanup で STOP_SENDING を送れる)
    let mut goaway_received = false;
    // 自側の判断 (LOC の書式違反など) でセッションを閉じて受信ループを抜けたか。
    // 閉じた session への GOAWAY / 正常 close は状態機械に拒否され、誤解を招く
    // 警告ログになるため送らない
    let mut session_terminated = false;
    // transport 自体のエラーで受信ループを抜けたか。後始末 (stream task の join と録画の
    // finalize) を終えてから `run` の戻り値として返す
    let mut fatal_error: Option<Error> = None;

    'main: loop {
        // 空いた枠のぶんだけ待機中の stream を処理へ回す。permit は使った分だけ
        // 取るので、枠が無いときは 1 つも取らずにすぐ抜ける
        while let Ok(permit) = std::sync::Arc::clone(&stream_slots).try_acquire_owned() {
            let Some(stream) = pending_streams.pop_front() else {
                // 枠を余分に取ってしまったので返す
                drop(permit);
                break;
            };
            total_streams += 1;
            spawn_stream_task(
                &mut join_set,
                &task_monitor,
                stream,
                permit,
                total_streams,
                &track_map,
                &data_plane,
                &frame_tx,
                &audio_tx,
                video_codec.as_deref(),
                audio_codec.as_deref(),
                audio_params,
                &display_backlog,
                &video_decode_order,
                audio_decoder.as_ref(),
                &audio_config_handled,
                &termination_tx,
                &catalog_tx,
                &pending_subgroups,
                playback,
                recorder_sender.as_ref(),
            );
        }

        // 水が多すぎるときは accept を止める。select! の分岐に入らなければ
        // s2n-quic が stream を保持する
        let accepting = pending_streams.len() < MAX_CONCURRENT_STREAMS * 4;

        tokio::select! {
            result = recv_acceptor.accept_recv_stream(), if accepting => {
                let stream = match result {
                    Ok(Some(s)) => s,
                    Ok(None) => {
                        tracing::info!("No more data streams");
                        peer_ended = true;
                        break 'main;
                    }
                    Err(e) => {
                        // WebTransport のセッション終了 (§6 の CONNECT stream の close /
                        // WT_CLOSE_SESSION) と接続クローズは期待される終了であるため、
                        // QUIC の `Ok(None)` ("No more data streams") と同じ info に揃える。
                        // この分岐は変換前の `TransportError` を持つため、variant で直接判定して
                        // 他の accept 失敗のエラーメッセージを変えない
                        if matches!(e, TransportError::ConnectionClosed) {
                            tracing::info!("Session closed by transport");
                        } else {
                            tracing::error!("Failed to accept data stream: {e}");
                        }
                        peer_ended = true;
                        break 'main;
                    }
                };
                pending_streams.push_back(stream);
            }
            Some(object) = catalog_rx.recv() => {
                // 購読で届いた catalog の更新を MSF の配置規則で適用する
                // (draft-ietf-moq-msf-01 §5 (Catalog))。起動時に解決したトラックは
                // 変更しないが、カタログの状態は購読が続く限り最新に保つ
                if let Err(e) = apply_catalog_object(&mut catalog_state, &object) {
                    tracing::warn!("Failed to apply MSF catalog update: {e}");
                }
            }
            notable = client.next_event() => {
                // 分岐本体を async ブロックに閉じ込め、`?` が `run` を抜けないようにする。
                // `client.next_event()` の結果とピア要求への応答 (`send_request_error`) は
                // どちらも transport I/O を行い、セッション終了後は
                // `TransportError::ConnectionClosed` を返す。`?` を `run` まで通すと
                // `Fatal: session closed` で異常終了し、終了時の後始末 (`close(0, "")` が
                // 送る CONNECT stream の送信側の FIN) に到達しない。
                // `break 'main` はブロックの外に出す (ブロックの中では外側のループを
                // 抜けられない) ため、ブロックは「受信ループを抜けるか」を返す。
                let outcome: Result<bool> = async {
                    match notable? {
                        Some(ClientEvent::Session(SessionEvent::GoawayReceived {
                            timeout, ..
                        })) => {
                            // draft-ietf-moq-transport-22 §9.2 (GOAWAY): GOAWAY は
                            // subscription state に影響しないため、購読を個別に終了する
                            // STOP_SENDING は送れる
                            tracing::info!("Received GOAWAY (timeout={timeout})");
                            goaway_received = true;
                            Ok(true)
                        }
                        Some(ClientEvent::Session(SessionEvent::PublishDoneReceived {
                            status_code,
                            stream_count,
                            ..
                        })) => {
                            tracing::info!(
                                "Received PUBLISH_DONE (status_code={status_code}, stream_count={stream_count})"
                            );
                            Ok(true)
                        }
                        Some(ClientEvent::Session(SessionEvent::CloseSession(err))) => {
                            tracing::warn!("Session closed: {:#x} {}", err.code, err.reason);
                            Ok(true)
                        }
                        Some(ClientEvent::Session(SessionEvent::ResetDataStream {
                            stream_id,
                            error_code,
                            ..
                        })) => {
                            // subscriber は outgoing data stream (subgroup / fill fetch) を
                            // 開かないため、reset すべき stream を持たない。届いた場合は
                            // 観測できるようにログだけ残す (draft-ietf-moq-transport-22
                            // §5.2 (Delivery Timeouts and Data Reliability))。
                            tracing::debug!(
                                "Ignoring ResetDataStream for a stream this example does not own: \
                                 stream_id={}, error_code={:#x}",
                                stream_id.0,
                                error_code
                            );
                            Ok(false)
                        }
                        Some(ClientEvent::Request(request)) => {
                            // subscriber は relay からの要求を受けない。届いた場合は
                            // NOT_SUPPORTED で拒否してハングを避ける。
                            tracing::warn!(
                                "Rejecting unexpected peer request: request_id={}",
                                request.request_id
                            );
                            client
                                .send_request_error(
                                    request.request_id,
                                    shiguredo_moqt::error::REQUEST_NOT_SUPPORTED,
                                    "subscriber does not serve requests",
                                )
                                .await?;
                            Ok(false)
                        }
                        Some(ClientEvent::RequestUpdate(update)) => {
                            // draft-ietf-moq-transport-22 §9.5 (REQUEST_UPDATE):
                            // 受信側は必ず 1 通の REQUEST_OK / REQUEST_ERROR で応答する MUST。
                            // 本 example が受信する更新は、publisher から PUBLISH で確立した
                            // 購読に対するものであり (SUBSCRIBE 起点の購読では publisher は
                            // REQUEST_UPDATE を送れない)、session 層の検証を通った更新は
                            // 購読状態へ反映済みである。
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
                    // peer が subscription / session を終了させた
                    Ok(true) => {
                        // GOAWAY は subscription state に影響しないため peer_ended にしない
                        // (cleanup で STOP_SENDING を送り、peer に stream を reset させる)
                        if !goaway_received {
                            peer_ended = true;
                        }
                        break 'main;
                    }
                    Ok(false) => {}
                    // transport がセッション終了 (§6) や接続クローズを検知した。accept 経路と
                    // 同じ正常終了として扱う (`is_transport_session_end`)。
                    // session_terminated は立てないため、GOAWAY と `close(0, "")` は送られる
                    Err(e) if is_transport_session_end(&e) => {
                        tracing::info!("Session closed by transport");
                        peer_ended = true;
                        break 'main;
                    }
                    // MOQT メッセージの encode / decode 失敗。§9 (Control Messages) と
                    // §9.20.1 (Parameter Scope) は不正なメッセージの受信を PROTOCOL_VIOLATION で
                    // 閉じることを MUST で要求する。decode は codec 層で完結し Session はこの
                    // 違反を観測できないため、I/O 層である main ループが終了コードを決めて閉じる
                    Err(Error::Moqt(e)) => {
                        let code = session_error_code(&e);
                        tracing::warn!("Closing session on MOQT message error: {code:#x} {e}");
                        close_session(&mut client, SessionError::new(code, e.reason())).await;
                        session_terminated = true;
                        // 終了コード付きで閉じた後も `run` の戻り値はエラーとして返す
                        // (moq-pub と同じ扱い。閉じた理由を呼び出し側が観測できるようにする)
                        fatal_error = Some(Error::Moqt(e));
                        break 'main;
                    }
                    // peer が bidi request stream を cancel した (§6.4.2.3)。該当 request
                    // だけが終端するため、受信と再生は継続する。subscriber は uni data stream を
                    // 送らないため、STOP_SENDING を受ける送信経路はこの bidi request stream だけである
                    Err(e) if is_peer_stream_reset(&e) => {
                        tracing::info!("Request stream was reset by peer; continuing");
                    }
                    // transport 自体のエラーは後始末 (stream task の join と録画の
                    // finalize) を終えてから `run` の戻り値として返す。接続が死んでいるため
                    // 終了コード付きの close は送れず、後段の終了依頼の回収も行わない
                    // (既知の限界)。
                    Err(e) => {
                        fatal_error = Some(e);
                        break 'main;
                    }
                }
            }
            Some(termination) = termination_rx.recv() => {
                // 自側から終了コード付きで閉じる
                close_session(&mut client, termination).await;
                session_terminated = true;
                break 'main;
            }
            _ = tick_interval.tick() => {
                let now_ms = Instant::now().duration_since(start).as_millis() as u64;
                client.tick(now_ms);
                // 停滞の切り分け用の任意診断。MOQT_STREAM_DIAG=1 のときだけ出す。
                // 100ms ごとに、stream 枠の残数・映像の表示待ち・受信済み object 数を
                // 記録し、どこで止まったかを後から追えるようにする。
                if stream_diag_enabled() {
                    let free_slots = MAX_CONCURRENT_STREAMS.saturating_sub(stream_slots.available_permits());
                    tracing::info!(
                        "STREAMDIAG t={}ms total_streams={} free_slots={} display_backlog={} active_tasks={}",
                        now_ms,
                        total_streams,
                        free_slots,
                        display_backlog.load(Ordering::Relaxed),
                        join_set.len(),
                    );
                }
            }
            _ = shutdown_monitor.recv() => {
                tracing::info!("Shutdown signal received");
                break 'main;
            }
            _ = &mut player_stop => {
                // プレイヤーの終了 (ウィンドウを閉じた等) でも録画を finalize するため、
                // pipeline を止めて run の後始末 (Recorder の join) へ進む
                tracing::info!("Player stopped, stopping pipeline");
                break 'main;
            }
        }
    }

    // select! で別の終了要因 (accept の終了 / GOAWAY / PUBLISH_DONE / peer の close /
    // shutdown など) が先に成立していても、既に届いている終了依頼があればそのコードで閉じる
    // (draft-ietf-moq-transport-22 §8.3 の MUST を終了コード 0x0 で上書きしない)。
    if fatal_error.is_none()
        && !session_terminated
        && let Ok(termination) = termination_rx.try_recv()
    {
        close_session(&mut client, termination).await;
        session_terminated = true;
    }

    // transport 自体のエラーで抜けた場合は接続が死んでいるため、GOAWAY / 正常 close は送らない
    let session_alive = fatal_error.is_none();

    // peer が subscription を終了させた場合は送信先の購読が既に Terminated であり、
    // STOP_SENDING は state machine に拒否される (draft-ietf-moq-transport-22 §3.1
    // (Subscriptions) は Pending / Established のみ STOP_SENDING を認める)。
    // stream task の join より先に送るのは、受信待ちの live stream で join が終わらない
    // ことを防ぐためである。
    // transport 自体のエラーで抜けた場合も接続が生存している可能性があるため、
    // 接続死活の判定と独立に best-effort で送る。
    // GOAWAY 起因の停止に GOING_AWAY (§12.5) ではなく CANCELLED を使うのは、
    // tokio-moq の stop_sending API がコードを指定できないためである。
    if should_stop_sending(peer_ended, session_terminated) {
        if let Some(rid) = video_request_id
            && let Err(e) = client.stop_sending(rid).await
        {
            // `should_stop_sending` のガードにより、ここでは peer_ended / session_terminated は
            // どちらも偽である (peer 起点の終了では送らず、その他の失敗だけがここに来る)
            log_failure(
                format_args!("Failed to send STOP_SENDING for video: {e}"),
                is_connection_closed(&e),
            );
        }
        if let Some(rid) = audio_request_id
            && let Err(e) = client.stop_sending(rid).await
        {
            log_failure(
                format_args!("Failed to send STOP_SENDING for audio: {e}"),
                is_connection_closed(&e),
            );
        }
        if let Err(e) = client.stop_sending(catalog_sub.request_id).await {
            log_failure(
                format_args!("Failed to send STOP_SENDING for catalog: {e}"),
                is_connection_closed(&e),
            );
        }
    }

    // transport 自体のエラーで抜けた場合は、live stream の FIN を待たずにタスクを打ち切る
    if fatal_error.is_some() {
        join_set.abort_all();
    }
    // peer が STOP_SENDING に従わない場合でも録画の finalize に到達できるよう上限を設ける
    let join_deadline = tokio::time::Instant::now() + STREAM_JOIN_TIMEOUT;
    loop {
        match tokio::time::timeout_at(join_deadline, join_set.join_next()).await {
            Ok(Some(Ok(()))) => {}
            // abort による打ち切りは異常ではない
            Ok(Some(Err(e))) if e.is_cancelled() => {}
            Ok(Some(Err(e))) => tracing::warn!("Stream task panicked: {e}"),
            Ok(None) => break,
            Err(_) => {
                tracing::warn!(
                    "Timed out waiting for stream tasks; aborting them to complete shutdown"
                );
                join_set.abort_all();
                break;
            }
        }
    }

    // 自側で終了コード付きに閉じた場合は、閉じた session へ GOAWAY / 正常 close を送らない
    if session_alive && should_close_gracefully(session_terminated) {
        if let Err(e) = client.send_goaway(Vec::new(), 5000).await {
            // `should_close_gracefully` のガードにより session_terminated はここでは偽である
            log_failure(
                format_args!("Failed to send GOAWAY: {e}"),
                peer_ended || is_connection_closed(&e),
            );
        }

        if let Err(e) = client.close(0, "").await {
            log_failure(
                format_args!("Failed to close session gracefully: {e}"),
                peer_ended || is_connection_closed(&e),
            );
        }
    }

    // 録画を finalize してライタースレッドの終了を待つ。ここまでで stream task はすべて
    // 終了している
    if let Some(recorder) = recorder
        && let Err(e) = recorder.finish()
    {
        tracing::error!("Failed to finalize MP4 recording: {e}");
        if fatal_error.is_none() {
            fatal_error = Some(e);
        }
    }

    if let Some(e) = fatal_error {
        return Err(e);
    }

    tracing::info!("Pipeline stopped: {total_streams} streams received");
    Ok(())
}

/// transport がセッション終了 (WebTransport の CONNECT stream の close / WT_CLOSE_SESSION) や
/// 接続クローズを検知したことを表すエラーか
///
/// セッション終了は異常ではなく期待される終了である。`client.next_event()` の中の要求応答
/// (`send_request_error` / `send_request_ok`) も transport I/O を行い、セッション終了後は
/// `TransportError::ConnectionClosed` を返すため、`?` で `run` まで通すと
/// `Fatal: session closed` で異常終了し、終了時の後始末に到達しない。WebTransport で
/// §6 が求めるのは `close(0, "")` が送る CONNECT stream の FIN であり、終了検知後の GOAWAY は
/// 失敗しうる (warn ログになる)。
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
/// MoqtClient の API は `Error` ではなく `TransportError` を直接返すものがある。
/// セッション終了の判別は `Error::ConnectionClosed` と同じ variant で行う。
fn is_connection_closed(error: &TransportError) -> bool {
    matches!(error, TransportError::ConnectionClosed)
}

/// セッション終了に伴う失敗を `info!`、それ以外を `warn!` で出す
///
/// WebTransport のセッション終了 (draft-ietf-webtrans-http3-16 §6) は異常ではないため、
/// 終了に伴う失敗を warn として出すとログから実際の異常を区別できない。受信ループの
/// accept / datagram 経路と同じ扱いに揃える。`expected` はセッション終了に伴う失敗かどうかで、
/// 後始末では `close` の送信失敗が接続クローズ以外に畳まれる経路もあるため、呼び出し側が
/// 「セッション終了を観測して後始末に入った」ことも含めて渡す。
fn log_failure(message: std::fmt::Arguments<'_>, expected: bool) {
    if expected {
        tracing::info!("{message}");
    } else {
        tracing::warn!("{message}");
    }
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

/// カタログの取得結果
struct CatalogResolution {
    /// 適用済みのカタログ状態 (購読で届く更新の適用に引き継ぐ)
    state: catalog::CatalogState,
    /// catalog から解決したビデオトラック情報
    video: Option<VideoTrackInfo>,
    /// catalog から解決したオーディオトラック情報
    audio: Option<AudioTrackInfo>,
    /// catalog の targetLatency (ミリ秒)
    target_latency_ms: Option<i64>,
}

/// カタログが届くまで待つ上限
///
/// publisher がまだ catalog を publish していない場合は、購読 (Next Object) で最初の
/// 独立したカタログが届くまで待つ。待ち続けないよう上限を設ける。
const CATALOG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// カタログ購読のメッセージパラメータを組み立てる
///
/// draft-ietf-moq-transport-22 §3.5 (Joining an Ongoing Track) の購読パターンに従い、
/// 購読の Location Filter を Next Object (§9.20.9 Table 6 の Type 0x05) にしつつ、
/// FILL_PARAMETERS (§9.20.15) で現在 Group の先頭から埋める fill を要求する
/// (内側の Location Filter は Relative Start の StartGroup=1)。
/// 内側を省略すると購読の Next Object を継承してしまい、購読より前に publish された
/// 独立カタログが fill range に入らない。
fn catalog_subscribe_parameters() -> MessageParameters {
    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LocationFilter(LocationFilter::NextObject),
    });
    let mut fill_params = MessageParameters::new();
    fill_params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LocationFilter(LocationFilter::RelativeGroup {
            start_group: 1,
        }),
    });
    parameters.push(MessageParameter {
        param_type: PARAM_FILL_PARAMETERS,
        value: MessageParameterValue::FillParameters(fill_params),
    });
    parameters
}

/// カタログを取得する
///
/// 次の 2 経路で届く Object を到着順に適用する
/// (draft-ietf-moq-transport-22 §3.4 (Fill Semantics) / §3.5 (Joining an Ongoing Track))。
///
/// 1. fill fetch stream (`fill_request_id` は起因 SUBSCRIBE の Request ID。FETCH_HEADER に
///    載る値と一致することを確認してから読む)。購読より前に publish された独立カタログが届く
/// 2. 購読 (Next Object) で届くカタログの Subgroup stream。以後の独立カタログと delta が届く
///
/// fill fetch stream が届かない場合 (publisher が Largest Object をまだ広告していない、
/// または fill に対応していない) も、購読で最初の独立したカタログが届くまで待つ。
/// fill の読み取りに失敗しても購読は生かしたままにして、届いた Object だけで解決を試みる。
///
/// 期限はカタログ取得の開始からの絶対時刻で、全 stream の読みに共有される。期限を超えて
/// 分割配信される fill は受信済みの Object ごと破棄する。また fill 非対応の publisher では、購読での再 publish が
/// `CATALOG_TIMEOUT` より後になると期限切れで終了する (MSF は fill を伴う購読を MUST と
/// するため、本 example は fill 非対応の publisher を対象にしない)。
///
/// これにより draft-ietf-moq-msf-01 §5 (Catalog) が求める「最新の完全なカタログと、
/// それに続く delta update」を 1 つの状態 [`catalog::CatalogState`] へまとめて適用できる。
async fn receive_catalog(
    recv_acceptor: &mut transport::StreamAcceptor,
    data_plane: &DataPlaneHandle,
    fill_request_id: u64,
    catalog_alias: u64,
    catalog_namespace: String,
) -> Result<CatalogResolution> {
    let deadline = tokio::time::Instant::now() + CATALOG_TIMEOUT;
    let mut state = catalog::CatalogState::new(catalog_namespace);
    while state.catalog().is_none() {
        let accepted =
            match tokio::time::timeout_at(deadline, recv_acceptor.accept_recv_stream()).await {
                Ok(accepted) => accepted?,
                // 期限までにカタログが決まらなかった
                Err(_) => break,
            };
        let Some(mut stream) = accepted else {
            return Err(Error::Other(
                "no more data streams while waiting for the catalog".to_string(),
            ));
        };
        let stream_id = DataStreamId(stream.stream_id());
        let mut buf = Vec::new();
        // 種別の読みにも期限を掛ける。stall した stream で待ち続けない
        let (stream_type_id, stream_type) = match tokio::time::timeout_at(
            deadline,
            stream_reader::peek_stream_type(&mut stream, &mut buf),
        )
        .await
        {
            Ok(Ok(StreamRead::Value(value))) => value,
            Ok(Ok(StreamRead::Closed(end))) => {
                // 種別を持たない stream は無視して次の stream を待つ
                tracing::debug!("Catalog wait: stream closed before type: {end:?}");
                continue;
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                tracing::warn!(
                    "Catalog wait: stream type did not arrive within {}ms",
                    CATALOG_TIMEOUT.as_millis()
                );
                break;
            }
        };
        data_plane.recv_data_stream_type(stream_id, stream_type_id)?;
        match stream_type {
            StreamType::Padding => {
                // draft-ietf-moq-transport-22 §11.5.1 (Padding Streams)
                let _ = drain_registered_stream_to_end(&mut stream, data_plane, stream_id).await;
            }
            StreamType::Fetch => {
                // fill fetch stream は FETCH_HEADER に起因 SUBSCRIBE の Request ID を載せる
                // (draft-ietf-moq-transport-22 §3.4)。購読で届くカタログと同じ状態へ適用する
                // 読みにも期限を適用する。fill fetch stream が stall すると期限が効かず、
                // 購読で届くカタログを待つ経路へ戻れなくなる
                match tokio::time::timeout_at(
                    deadline,
                    read_catalog_fetch_stream(
                        &mut stream,
                        data_plane,
                        stream_id,
                        &buf,
                        fill_request_id,
                    ),
                )
                .await
                {
                    Ok(Ok(objects)) => {
                        for object in &objects {
                            apply_catalog_object(&mut state, object)?;
                        }
                    }
                    // fill の失敗 (REQUEST_ERROR / RESET / 不一致) は購読の失敗ではない。
                    // 購読で届くカタログを待つ。読み切れなかった stream を Session の会計から
                    // 外すため STOP_SENDING を送る (送らないと受信 stream が残り、data stream
                    // timeout でセッションが落ちる)
                    Ok(Err(e)) => {
                        tracing::warn!("Catalog fill fetch failed: {e}");
                        let _ = data_plane.send_data_stream_stop_sending(stream_id);
                    }
                    Err(_) => {
                        tracing::warn!(
                            "Catalog fill fetch did not finish within {}ms",
                            CATALOG_TIMEOUT.as_millis()
                        );
                        let _ = data_plane.send_data_stream_stop_sending(stream_id);
                    }
                }
            }
            StreamType::Subgroup => {
                // 読みにも期限を掛ける。stall した stream で待ち続けない
                match tokio::time::timeout_at(
                    deadline,
                    read_catalog_subgroup_stream(
                        &mut stream,
                        data_plane,
                        stream_id,
                        &buf,
                        catalog_alias,
                    ),
                )
                .await
                {
                    Ok(Ok(objects)) => {
                        for object in &objects {
                            apply_catalog_object(&mut state, object)?;
                        }
                    }
                    Ok(Err(e)) => tracing::warn!("Failed to read catalog stream: {e}"),
                    Err(_) => tracing::warn!(
                        "Catalog stream did not finish within {}ms",
                        CATALOG_TIMEOUT.as_millis()
                    ),
                }
            }
        }
    }

    let Some(catalog) = state.catalog() else {
        return Err(Error::Other(format!(
            "catalog did not arrive within {}ms",
            CATALOG_TIMEOUT.as_millis()
        )));
    };
    // targetLatency は再生側の時間軸が表示の遅れの下限に使う。ここでは読むだけにして、
    // 呼び出し側が共有セルへ入れる
    let target_latency_ms = catalog_target_latency_ms(&catalog.tracks);
    let video = catalog
        .tracks
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
    let audio = catalog
        .tracks
        .iter()
        .find(|t| t.codec.as_ref().is_some_and(|c| c.starts_with("opus")))
        .map(extract_audio_info)
        .transpose()?;
    if video.is_none() && audio.is_none() {
        return Err(Error::Other(
            "no usable tracks found in catalog (expected av01/avc1/hvc1/hev1/opus)".to_string(),
        ));
    }
    Ok(CatalogResolution {
        state,
        video,
        audio,
        target_latency_ms,
    })
}

/// fill fetch stream (FETCH 系の uni stream) を読み切り、catalog Object を取り出す
///
/// 読み出しの終端検知は `receive_registered_stream_data` に任せる。同関数が
/// `recv_data_stream_closed` で終端を通知しており、Session は受信 fetch stream の終端を
/// `recv_fetch_data_stream_closed` へ dispatch して FETCH 状態に記録する。ここで改めて
/// drain したり fetch の終端を通知したりすると二重通知になり
/// 「unknown stream id」「fetch stream event on terminated fetch」で失敗する。
async fn read_catalog_fetch_stream(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    buf: &[u8],
    expected_request_id: u64,
) -> Result<Vec<catalog::CatalogObject>> {
    let mut decoder = FetchStreamDecoder::new();
    decoder.push(buf);
    let fetch_header = loop {
        if let Some(h) = decoder.try_decode_header()? {
            break h;
        }
        match receive_registered_stream_data(stream, data_plane, stream_id).await? {
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
        let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
        return Err(Error::Other(format!(
            "unexpected fetch request_id: expected={expected_request_id}, got={}",
            fetch_header.request_id,
        )));
    }
    let mut objects = Vec::new();
    loop {
        let entry = match decoder.try_decode_entry()? {
            Some(e) => e,
            None => match receive_registered_stream_data(stream, data_plane, stream_id).await? {
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
                match receive_registered_stream_data(stream, data_plane, stream_id).await? {
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
        // Object 以外 (End of Range 系) は catalog の Object ではない
        let DecodedFetchEntry::Object(obj) = &entry else {
            continue;
        };
        let location = Location {
            group_id: obj.group_id,
            object_id: obj.object_id,
        };
        objects.push(catalog::CatalogObject::decode(location, &payload)?);
    }
    Ok(objects)
}

/// catalog の Subgroup stream を読み切り、catalog Object を取り出す
///
/// `catalog_alias` 以外の track alias の stream は catalog のものではないため読み捨てる。
async fn read_catalog_subgroup_stream(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    buf: &[u8],
    catalog_alias: u64,
) -> Result<Vec<catalog::CatalogObject>> {
    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(buf);
    let header = loop {
        match decoder.try_decode_header()? {
            Some(header) => break header,
            None => match receive_registered_stream_data(stream, data_plane, stream_id).await? {
                Some(data) => decoder.push(&data),
                None => {
                    return Err(Error::Other(
                        "stream closed before subgroup header".to_string(),
                    ));
                }
            },
        }
    };
    if header.track_alias != catalog_alias {
        tracing::debug!(
            "Catalog: subgroup stream for another track alias={}",
            header.track_alias,
        );
        let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
        return Ok(Vec::new());
    }
    match data_plane.recv_subgroup_header(stream_id, &header) {
        Ok(TrackDataAcceptance::Accepted) => {}
        Ok(TrackDataAcceptance::UnknownTrackAlias) => {
            return Err(Error::Other(format!(
                "unknown track alias for the catalog: {}",
                header.track_alias
            )));
        }
        Ok(TrackDataAcceptance::FilteredOut | TrackDataAcceptance::Discarded) => {
            let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
            return Ok(Vec::new());
        }
        Err(e) => {
            return Err(Error::Other(format!(
                "failed to register subgroup header: {e}"
            )));
        }
    }
    let mut objects = Vec::new();
    loop {
        let object = match decoder.try_decode_object()? {
            Some(object) => object,
            None => match receive_registered_stream_data(stream, data_plane, stream_id).await? {
                Some(data) => {
                    decoder.push(&data);
                    continue;
                }
                None => break,
            },
        };
        let accepted = match data_plane.recv_subgroup_object(stream_id, &object) {
            Ok(TrackDataAcceptance::Accepted) => true,
            // header 受理済み stream では `UnknownTrackAlias` は返らない契約だが、
            // 型上あり得るため防御的に読み捨てる
            Ok(
                TrackDataAcceptance::UnknownTrackAlias
                | TrackDataAcceptance::FilteredOut
                | TrackDataAcceptance::Discarded,
            ) => false,
            Err(e) => {
                return Err(Error::Other(format!(
                    "failed to register catalog object: {e}"
                )));
            }
        };
        if object.payload_length == 0 {
            continue;
        }
        // 配送しない Object でも payload は wire 上に存在するため読み出して消費する。
        // 残すと次の `try_decode_object` が ProtocolViolation になる
        let payload = read_catalog_payload(stream, data_plane, stream_id, &mut decoder).await?;
        if !accepted {
            continue;
        }
        let location = Location {
            group_id: header.group_id,
            object_id: object.object_id,
        };
        objects.push(catalog::CatalogObject::decode(location, &payload)?);
    }
    Ok(objects)
}

/// Subgroup stream から次の Object の payload を読み出す
async fn read_catalog_payload(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    decoder: &mut SubgroupStreamDecoder,
) -> Result<Vec<u8>> {
    loop {
        if let Some(payload) = decoder.try_read_payload() {
            return Ok(payload);
        }
        match receive_registered_stream_data(stream, data_plane, stream_id).await? {
            Some(data) => decoder.push(&data),
            None => {
                return Err(Error::Other(
                    "stream closed before catalog payload".to_string(),
                ));
            }
        }
    }
}

/// 受信した catalog Object を状態へ適用し、結果をログに出す
///
/// draft-ietf-moq-msf-01 §5 (Catalog) の配置規則の判定は [`catalog::CatalogState::apply`]
/// が行う。適用しなかった Object は理由付きで debug に出す。
fn apply_catalog_object(
    state: &mut catalog::CatalogState,
    object: &catalog::CatalogObject,
) -> Result<()> {
    let location = object.location;
    match state.apply(object)? {
        catalog::CatalogApplyOutcome::Replaced => {
            if let Some(catalog) = state.catalog() {
                tracing::info!(
                    "Applied MSF catalog (group={}, object={}): tracks=[{}]",
                    location.group_id,
                    location.object_id,
                    catalog::track_names(catalog),
                );
            }
        }
        catalog::CatalogApplyOutcome::DeltaApplied => {
            if let Some(catalog) = state.catalog() {
                tracing::info!(
                    "Applied MSF catalog delta (group={}, object={}): tracks=[{}]",
                    location.group_id,
                    location.object_id,
                    catalog::track_names(catalog),
                );
            }
        }
        catalog::CatalogApplyOutcome::Ignored(reason) => {
            tracing::debug!(
                "Ignored MSF catalog object (group={}, object={}): {reason}",
                location.group_id,
                location.object_id,
            );
        }
    }
    Ok(())
}

/// カタログの track から `targetLatency` (ミリ秒) を求める
///
/// 同じ `renderGroup` の track は同じ値でなければならない (draft-ietf-moq-msf-01 §5.2.8)。
/// 値が無い track は無視し、複数の値があるときは安全側に倒して大きい方を採る。どの track も
/// 持たないときは `None` (時間軸は自分の揺らぎから求めた遅れだけを使う)。
fn catalog_target_latency_ms(tracks: &[MsfTrack]) -> Option<i64> {
    tracks
        .iter()
        .filter_map(|track| track.target_latency)
        .max()
        .and_then(|value| i64::try_from(value).ok())
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
    display_backlog: std::sync::Arc<std::sync::atomic::AtomicI64>,
    video_decode_order: &std::sync::Arc<tokio::sync::Mutex<VideoDecodeOrder>>,
    audio_decoder: Option<&std::sync::Arc<tokio::sync::Mutex<OpusDecoder>>>,
    audio_config_handled: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    termination_tx: &tokio::sync::mpsc::Sender<SessionError>,
    catalog_tx: &tokio::sync::mpsc::Sender<catalog::CatalogObject>,
    pending_subgroups: &SharedPendingSubgroupBuffer,
    playback: bool,
    recorder: Option<&RecorderSender>,
) {
    let started = Instant::now();
    let raw_stream_id = stream.stream_id();
    if stream_diag_enabled() {
        tracing::info!("STREAMDIAG stream #{stream_num} enter: stream_id={raw_stream_id}");
    }
    handle_stream_body(
        &mut stream,
        data_plane,
        track_map,
        video_codec,
        audio_codec,
        audio_params,
        frame_tx,
        audio_tx,
        stream_num,
        display_backlog,
        video_decode_order,
        audio_decoder,
        audio_config_handled,
        termination_tx,
        catalog_tx,
        pending_subgroups,
        playback,
        recorder,
    )
    .await;
    // 診断時は stream ごとの滞在時間を残す。枠を長時間占有する stream を特定する
    if stream_diag_enabled() {
        tracing::info!(
            "STREAMDIAG stream #{stream_num} exit: stream_id={raw_stream_id} elapsed_ms={}",
            started.elapsed().as_millis()
        );
    }
}

/// video stream の decoder を必要に応じて生成する
///
/// 再生しない場合と、表示待ちの超過でデコードをスキップする場合は `None` を返す。
/// 録画中は decoder が使えなくても録画を継続するため `None` を返して警告する。
fn build_stream_video_decoder(
    playback: bool,
    skip_decode: bool,
    codec: &str,
    stream_num: u64,
    recording: bool,
) -> Result<Option<decoder::VideoDecoder>> {
    if !playback || skip_decode {
        if skip_decode {
            tracing::debug!("Stream #{stream_num}: skipping video decode (recording continues)");
        }
        return Ok(None);
    }
    match decoder::build_video_decoder(codec) {
        Ok(decoder) => Ok(Some(decoder)),
        Err(e) if recording => {
            tracing::warn!(
                "Stream #{stream_num}: failed to create video decoder; recording without video playback: {e}"
            );
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// `handle_incoming_stream` の本体。
///
/// 早期 return が多いため、呼び出し側で入口と出口を 1 か所にまとめて記録できる
/// ように本体を分けている。
#[expect(clippy::too_many_arguments)]
async fn handle_stream_body(
    stream: &mut transport::RecvStream,
    data_plane: DataPlaneHandle,
    track_map: &HashMap<u64, TrackKind>,
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
    audio_params: Option<(u32, u8)>,
    frame_tx: &std::sync::mpsc::Sender<DecodedVideoFrame>,
    audio_tx: &std::sync::mpsc::Sender<DecodedAudioFrame>,
    stream_num: u64,
    display_backlog: std::sync::Arc<std::sync::atomic::AtomicI64>,
    video_decode_order: &std::sync::Arc<tokio::sync::Mutex<VideoDecodeOrder>>,
    audio_decoder: Option<&std::sync::Arc<tokio::sync::Mutex<OpusDecoder>>>,
    audio_config_handled: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    termination_tx: &tokio::sync::mpsc::Sender<SessionError>,
    catalog_tx: &tokio::sync::mpsc::Sender<catalog::CatalogObject>,
    pending_subgroups: &SharedPendingSubgroupBuffer,
    playback: bool,
    recorder: Option<&RecorderSender>,
) {
    let stream_id = DataStreamId(stream.stream_id());
    let mut buf = Vec::new();
    let (stream_type_id, stream_type) =
        match stream_reader::peek_stream_type(stream, &mut buf).await {
            Ok(StreamRead::Value(value)) => value,
            Ok(StreamRead::Closed(end)) => {
                tracing::debug!("Stream #{stream_num}: closed before type: {end:?}");
                return;
            }
            Err(e) => {
                log_failure(
                    format_args!("Stream #{stream_num}: failed to peek stream type: {e}"),
                    is_transport_session_end(&e),
                );
                return;
            }
        };
    if let Err(e) = data_plane.recv_data_stream_type(stream_id, stream_type_id) {
        // MoqtClient の登録 API は全エラーを `TransportError::Internal` に写すため、
        // ここはセッション終了でも warn のままにする
        tracing::warn!("Stream #{stream_num}: failed to register data stream type: {e}");
        return;
    }
    match stream_type {
        StreamType::Fetch => {
            // 本 example は FETCH を発行しない。ここに来る Fetch stream は FILL_PARAMETERS 付き
            // SUBSCRIBE への fill fetch stream (draft-ietf-moq-transport-22 §3.4 (Fill Semantics))
            // がカタログ解決後に届いたものであり、購読 (Subgroup) でも同じカタログが届くため
            // 読み捨てる
            tracing::debug!("Stream #{stream_num}: dropping a fill fetch stream");
            let _ = drain_registered_stream_to_end(stream, &data_plane, stream_id).await;
        }
        StreamType::Padding => {
            // draft-ietf-moq-transport-22 §11.5.1 (Padding Streams):
            // パディングバイトを読み捨てる。session には recv_data_stream_type で登録済み。
            let _ = drain_registered_stream_to_end(stream, &data_plane, stream_id).await;
        }
        StreamType::Subgroup => {
            let mut sg_decoder = SubgroupStreamDecoder::new();
            sg_decoder.push(&buf);
            let header = loop {
                match sg_decoder.try_decode_header() {
                    Ok(Some(h)) => break h,
                    Ok(None) => {
                        match receive_registered_stream_data(stream, &data_plane, stream_id).await {
                            Ok(Some(data)) => sg_decoder.push(&data),
                            Ok(None) => return,
                            Err(e) => {
                                log_failure(
                                    format_args!(
                                        "Stream #{stream_num}: failed to read subgroup header: {e}"
                                    ),
                                    is_transport_session_end(&e),
                                );
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Stream #{stream_num}: failed to read subgroup header: {e}");
                        let _ =
                            drain_registered_stream_to_end(stream, &data_plane, stream_id).await;
                        return;
                    }
                }
            };
            // 再生の間引きで video group を破棄するのは録画していない場合だけにする。
            // 録画中は group を読み切って録画し、デコードだけをスキップする。
            let skip_decode = track_map.get(&header.track_alias) == Some(&TrackKind::Video)
                && display_backlog.load(Ordering::Relaxed) > MAX_DISPLAY_BACKLOG;
            if skip_decode && recorder.is_none() {
                tracing::debug!(
                    "Stream #{stream_num}: dropping video group (display backlog={})",
                    display_backlog.load(Ordering::Relaxed)
                );
                let _ = data_plane.send_data_stream_stop_sending(stream_id);
                let _ = drain_registered_stream_to_end(stream, &data_plane, stream_id).await;
                return;
            }
            if !register_subgroup_header(
                stream,
                &data_plane,
                stream_id,
                &header,
                &mut sg_decoder,
                stream_num,
                pending_subgroups,
            )
            .await
            {
                return;
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
                        let _ =
                            drain_registered_stream_to_end(stream, &data_plane, stream_id).await;
                        return;
                    };
                    let mut decoder = match build_stream_video_decoder(
                        playback,
                        skip_decode,
                        codec,
                        stream_num,
                        recorder.is_some(),
                    ) {
                        Ok(decoder) => decoder,
                        Err(e) => {
                            tracing::warn!(
                                "Stream #{stream_num}: failed to create video decoder: {e}"
                            );
                            let _ = data_plane.send_data_stream_stop_sending(stream_id);
                            let _ = drain_registered_stream_to_end(stream, &data_plane, stream_id)
                                .await;
                            return;
                        }
                    };
                    let frames = decode_video_stream(
                        stream,
                        &data_plane,
                        stream_id,
                        header.group_id,
                        &mut sg_decoder,
                        video_decode_order,
                        decoder.as_mut(),
                        &FrameSink {
                            frame_tx,
                            display_backlog: &display_backlog,
                        },
                        recorder,
                        termination_tx,
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
                        let _ =
                            drain_registered_stream_to_end(stream, &data_plane, stream_id).await;
                        return;
                    };
                    // デコーダは subscription で共有する。stream ごとに作ると 20 ms ごとに
                    // 状態が失われ、フレーム境界で波形が不連続になる。
                    // 再生を行わない場合はロックを取らずに録画だけを行う。
                    let mut decoder = match audio_decoder {
                        Some(decoder) => Some(decoder.lock().await),
                        None => None,
                    };
                    // Audio Config の中身はコーデック依存 (LOC draft-ietf-moq-loc-04 §2.3.3.1 (Audio Config)) のため、
                    // catalog の codec が opus の場合のみ OpusHead として解釈する。
                    // 処理済みフラグは subscription で共有する: decode_audio_stream は
                    // subgroup stream ごとに呼ばれるが、OpusHead はセッションで 1 回しか
                    // 送られないため、共有しないと 2 本目以降の stream が OpusHead を
                    // 待ち続けて音声が途切れる。
                    let is_opus_codec = audio_codec.is_some_and(|c| c.starts_with("opus"));
                    // Audio Config (OpusHead) は subscription で 1 回だけ処理する
                    let mut handled =
                        audio_config_handled.load(std::sync::atomic::Ordering::Acquire);
                    let chunks = decode_audio_stream(
                        stream,
                        &data_plane,
                        stream_id,
                        &mut sg_decoder,
                        decoder.as_deref_mut(),
                        audio_tx,
                        &mut handled,
                        is_opus_codec,
                        sample_rate,
                        channels,
                        recorder,
                        termination_tx,
                    )
                    .await;
                    if handled {
                        audio_config_handled.store(true, std::sync::atomic::Ordering::Release);
                    }
                    tracing::debug!(
                        "Stream #{stream_num}: audio group {} complete ({chunks} chunks)",
                        header.group_id,
                    );
                }
                Some(TrackKind::Catalog) => {
                    // 購読の確立後に配られたカタログ。読み出した Object を main ループへ渡し、
                    // 状態 (CatalogState) を所有する main ループが MSF の配置規則で適用する。
                    // 状態を stream task 側に持たせないのは、複数の Group の stream が
                    // 同時に走りうるためである。
                    match read_catalog_subgroup_stream(
                        stream,
                        &data_plane,
                        stream_id,
                        &buf,
                        header.track_alias,
                    )
                    .await
                    {
                        Ok(objects) => {
                            for object in objects {
                                if catalog_tx.send(object).await.is_err() {
                                    // main ループが終了した
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Stream #{stream_num}: failed to read catalog stream: {e}"
                            );
                        }
                    }
                }
                None => {
                    tracing::warn!(
                        "Stream #{stream_num}: unknown track alias={}",
                        header.track_alias,
                    );
                    let _ = drain_registered_stream_to_end(stream, &data_plane, stream_id).await;
                }
            }
        }
    }
}

/// Subgroup header を Session へ登録する
///
/// Track Alias が未確立 (`UnknownTrackAlias`) の場合は、draft-ietf-moq-transport-22
/// §3.1.3.1 (Unknown Track Alias) の "buffer it briefly" に従い、購読が確立するまで
/// 短時間ストリームを保持する。保持の上限とタイムアウトは
/// [`shiguredo_moqt::pending_subgroup_buffer::PendingSubgroupBuffer`] が管理する。
///
/// 保持している間に読んだチャンクは、確立を検出した時点で `decoder` へ積み直す。decoder は
/// header を復号済みであり、保留中はストリームを読んでも decoder へ積んでいないため、
/// 積み直すだけで続きから復号できる。
///
/// 保留バッファは session で 1 つ共有する。この関数は自分の entry の識別子を保持し、
/// `take_ready_for` で自分の通知だけを引き取るため、他の stream task が保持する entry を
/// 引き取ることはない。バッファを task ローカルに持つと per-session の上限が per-stream の
/// 上限としてしか働かない。
///
/// 購読の確立は 2 つの経路で検出する。
///
/// - `recv_subgroup_header` の再試行が `Accepted` を返す (Session が alias を解決できる)
/// - 呼び出し側が `note_subscriber` を呼び、`take_ready_for` が `Subscriber` を返す
///
/// この example は受信ループを始める前にすべての `SUBSCRIBE_OK` を処理するため、
/// 受信ループの実行中に購読が確立する経路は無く `note_subscriber` を呼ぶ箇所はない。
/// そのため再試行で検出する (将来、実行中に購読を追加する場合は、確立した時点で
/// `note_subscriber` を呼べば保持している chunk を直ちに引き取れる)。
///
/// 戻り値が false の場合は header を登録できず、ストリームを破棄した
/// (STOP_SENDING を送って読み捨て済み)。
///
/// 保留は stream task の同時実行枠を最大 `timeout_us` (既定 5 秒) 占有する。未知の alias の
/// ストリームが枠の数だけ同時に届くと、その間は新しいストリームの処理が始まらない。
/// 保持量は per-stream の上限に加え、session 共有のバッファの per-session の上限でも抑えられ、
/// 占有は期限で解消するため、無制限には増えない。
async fn register_subgroup_header(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    header: &SubgroupHeader,
    decoder: &mut SubgroupStreamDecoder,
    stream_num: u64,
    pending_subgroups: &SharedPendingSubgroupBuffer,
) -> bool {
    match data_plane.recv_subgroup_header(stream_id, header) {
        Ok(TrackDataAcceptance::Accepted) => return true,
        Ok(TrackDataAcceptance::UnknownTrackAlias) => {}
        // draft-ietf-moq-transport-22 §3.1.3 (Track Alias): キャンセル済み subscription へ
        // 遅れて届いた Object。未知 alias と違い確実に不要なので保持しない。
        Ok(TrackDataAcceptance::Discarded) => {
            tracing::debug!(
                "Stream #{stream_num}: discarding objects for cancelled track alias={}",
                header.track_alias,
            );
            abort_subgroup_stream(stream, data_plane, stream_id).await;
            return false;
        }
        // draft-ietf-moq-transport-22 §3.1 (Subscriptions): フィルタ再適用の結果
        // どの subscription にも属さなかった Object。セッションは閉じずに捨てる。
        Ok(TrackDataAcceptance::FilteredOut) => {
            tracing::debug!(
                "Stream #{stream_num}: object filtered out for track alias={}",
                header.track_alias,
            );
            abort_subgroup_stream(stream, data_plane, stream_id).await;
            return false;
        }
        Err(e) => {
            // MoqtClient の登録 API は全エラーを `TransportError::Internal` に写すため、
            // ここはセッション終了でも warn のままにする
            tracing::warn!("Stream #{stream_num}: failed to register subgroup header: {e}");
            return false;
        }
    }

    // 保留経路。バッファは session で共有し、この task は自分の entry の識別子だけを保持する。
    // 引き取りは `take_ready_for` で行うため、他の task の entry を引き取ることはない
    // (バッファ全体を返す `take_ready` は 1 つの待ち手だけが使う API であり、この example
    // では使わない)。
    let options = DEFAULT_PENDING_SUBGROUP_BUFFER_OPTIONS;
    let added_us = monotonic_now_us();
    let id = {
        let mut pending = pending_subgroups.lock().await;
        pending.add(header.track_alias, added_us)
    };
    let deadline_us = added_us.saturating_add(options.timeout_us);
    tracing::debug!(
        "Stream #{stream_num}: holding subgroup stream until the track alias={} is established",
        header.track_alias,
    );
    loop {
        // 通知済み (購読の確立 / 期限切れ / 上限超過 / 終端) の自分の entry を引き取る。
        // 引き取りと削除を 1 回のロックで行い、ロックを await またぎにしない
        let taken = {
            let mut pending = pending_subgroups.lock().await;
            let ready = pending.take_ready_for(id, monotonic_now_us());
            if let Some(ready) = &ready {
                pending.remove(ready.id);
            }
            ready
        };
        if let Some(ready) = taken {
            match ready.reason {
                PendingNotifyReason::Subscriber => {
                    // 購読が確立した。保持したチャンクを decoder へ戻して続きを読む
                    let buffered: usize = ready.chunks.iter().map(Vec::len).sum();
                    for chunk in &ready.chunks {
                        decoder.push(chunk);
                    }
                    tracing::debug!(
                        "Stream #{stream_num}: released the held subgroup stream \
                         (track alias={}, buffered={} bytes)",
                        ready.track_alias,
                        buffered,
                    );
                    return true;
                }
                // 購読が確立しないまま保持の上限を過ぎた。破棄した理由を残す
                PendingNotifyReason::Timeout => {
                    tracing::warn!(
                        "Stream #{stream_num}: dropping the held subgroup stream \
                         (track alias={} was not established within {} ms)",
                        ready.track_alias,
                        options.timeout_us / 1000,
                    );
                }
                PendingNotifyReason::OverflowPerStream => {
                    tracing::warn!(
                        "Stream #{stream_num}: dropping the held subgroup stream \
                         (per-stream buffer overflow: track alias={}, limit={} bytes)",
                        ready.track_alias,
                        options.per_stream_max_bytes,
                    );
                }
                PendingNotifyReason::OverflowPerSession => {
                    tracing::warn!(
                        "Stream #{stream_num}: dropping the held subgroup stream \
                         (per-session buffer overflow: track alias={}, limit={} bytes)",
                        ready.track_alias,
                        options.per_session_max_bytes,
                    );
                }
                // session が閉じた。以降の受信は失敗するため保留を打ち切る
                PendingNotifyReason::SessionClose => {
                    tracing::debug!(
                        "Stream #{stream_num}: session closed while holding the subgroup stream \
                         (track alias={})",
                        ready.track_alias,
                    );
                }
                // FIN / RESET_STREAM でストリームが終わった
                PendingNotifyReason::EndOfStream => {
                    tracing::debug!(
                        "Stream #{stream_num}: stream ended while holding the subgroup stream \
                         (track alias={})",
                        ready.track_alias,
                    );
                }
            }
            abort_subgroup_stream(stream, data_plane, stream_id).await;
            return false;
        }

        // 購読の確立を再試行する。確立していれば Session が alias を解決して受理する
        match data_plane.recv_subgroup_header(stream_id, header) {
            Ok(TrackDataAcceptance::Accepted) => {
                // 確立を検出した。entry を引き取り待ちにして次のループでチャンクを戻す
                pending_subgroups
                    .lock()
                    .await
                    .note_subscriber(header.track_alias);
                continue;
            }
            Ok(TrackDataAcceptance::UnknownTrackAlias) => {}
            Ok(TrackDataAcceptance::Discarded) => {
                tracing::debug!(
                    "Stream #{stream_num}: discarding objects for cancelled track alias={}",
                    header.track_alias,
                );
                pending_subgroups.lock().await.remove(id);
                abort_subgroup_stream(stream, data_plane, stream_id).await;
                return false;
            }
            Ok(TrackDataAcceptance::FilteredOut) => {
                tracing::debug!(
                    "Stream #{stream_num}: object filtered out for track alias={}",
                    header.track_alias,
                );
                pending_subgroups.lock().await.remove(id);
                abort_subgroup_stream(stream, data_plane, stream_id).await;
                return false;
            }
            Err(e) => {
                tracing::warn!("Stream #{stream_num}: failed to register subgroup header: {e}");
                pending_subgroups.lock().await.remove(id);
                return false;
            }
        }

        // 期限までは読み続けて保持する。読まないと peer のフロー制御が止まり、確立が
        // 遅れたぶんだけ転送が滞る (draft-ietf-moq-transport-22 §3.1.3.1 は
        // "withhold stream flow control" も MAY とするが、本 example は上限付きで読む)
        let remaining_us = deadline_us.saturating_sub(monotonic_now_us());
        if remaining_us <= 0 {
            // 期限切れ。次のループで take_ready_for が Timeout を返す
            continue;
        }
        match tokio::time::timeout(
            std::time::Duration::from_micros(remaining_us as u64),
            receive_registered_stream_data(stream, data_plane, stream_id),
        )
        .await
        {
            Ok(Ok(Some(data))) => pending_subgroups.lock().await.push(id, &data),
            // ストリームが終端した。entry に終端を通知し、次のループで引き取る
            Ok(Ok(None)) => pending_subgroups
                .lock()
                .await
                .note_end_of_stream(header.track_alias),
            Ok(Err(e)) => {
                log_failure(
                    format_args!("Stream #{stream_num}: failed to read held subgroup stream: {e}"),
                    is_transport_session_end(&e),
                );
                pending_subgroups.lock().await.remove(id);
                return false;
            }
            // 期限切れ。次のループで take_ready_for が Timeout を返す
            Err(_) => {}
        }
    }
}

/// 保留を打ち切った Subgroup ストリームを停止して読み捨てる
///
/// STOP_SENDING を送ってから終端まで読む。送らないと peer が送信を続け、受信ループの
/// 枠と帯域を不要に消費する。
async fn abort_subgroup_stream(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
) {
    let _ = data_plane.send_data_stream_stop_sending(stream_id);
    let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
}

/// 単調増加するマイクロ秒時刻
///
/// [`shiguredo_moqt::pending_subgroup_buffer::PendingSubgroupBuffer`] は Sans-I/O のため
/// 時刻を引数 (マイクロ秒の `i64`) で受ける。example ではプロセス起動からの経過時間を渡す。
fn monotonic_now_us() -> i64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    i64::try_from(start.elapsed().as_micros()).unwrap_or(i64::MAX)
}

/// 1 本の Subgroup ストリームから映像 Object を取り出して復号する
///
/// `group_id` は Subgroup ヘッダの Group ID である。Subgroup ストリームの Object はすべて
/// 同じ Group に属する (draft-ietf-moq-transport-22 §2.2 (Subgroups)) ため、Object ごとに
/// 読む必要はない。
/// `video_decode_order` は購読で共有する復号順の判定である。復号の直前に通し、復号してよい
/// Object だけを decoder へ渡す。
#[expect(clippy::too_many_arguments)]
async fn decode_video_stream(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    group_id: u64,
    sg_decoder: &mut SubgroupStreamDecoder,
    video_decode_order: &std::sync::Arc<tokio::sync::Mutex<VideoDecodeOrder>>,
    mut video_decoder: Option<&mut decoder::VideoDecoder>,
    sink: &FrameSink<'_>,
    recorder: Option<&RecorderSender>,
    termination_tx: &tokio::sync::mpsc::Sender<SessionError>,
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
                            log_failure(
                                format_args!("Failed to read object: {e}"),
                                is_transport_session_end(&e),
                            );
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
                    log_failure(
                        format_args!("Failed to read object payload: {e}"),
                        is_transport_session_end(&e),
                    );
                    return frames;
                }
            }
        };
        // 配送しない Object (`FilteredOut` / `Discarded`) は Properties を解釈しない。
        // 書式違反の検出は配送する Object に限る (フィルタ済みの Object まで
        // セッションを閉じる理由にするのは本 example の用途に対して過剰なため)。
        if !deliver {
            continue;
        }
        // 表示時刻を決めるために LOC の Timestamp も取り出す
        // (書式違反は配送の有無や処理済みフラグに関わらず毎 Object で検出する)
        let timestamp_us = match extract_timestamp_timescale(obj.properties_bytes.as_deref()) {
            Ok((timestamp, timescale)) => loc_timestamp_us(timestamp, timescale),
            Err(e) => {
                request_session_termination(termination_tx, &e);
                return frames;
            }
        };
        let video_config = match extract_video_config(obj.properties_bytes.as_deref()) {
            Ok(config) => config,
            Err(e) => {
                request_session_termination(termination_tx, &e);
                return frames;
            }
        };
        if let Some(recorder) = recorder
            && let Err(e) = record_video_object(
                recorder,
                obj.properties_bytes.as_deref(),
                video_config.as_deref(),
                &payload,
            )
        {
            request_session_termination(termination_tx, &e);
            return frames;
        }
        let Some(video_decoder) = video_decoder.as_deref_mut() else {
            continue;
        };
        // キーフレーム判定は録画と同じ規則を使う。PROP_VIDEO_FRAME_MARKING が無い publisher の
        // 映像も復号できるように、PROP_VIDEO_CONFIG の有無で代用する。
        let is_key_frame =
            match is_video_keyframe(obj.properties_bytes.as_deref(), video_config.as_deref()) {
                Ok(keyframe) => keyframe,
                Err(e) => {
                    request_session_termination(termination_tx, &e);
                    return frames;
                }
            };
        // Prior Object ID Gap は Object Properties から読む (Property が無ければ 0)
        let prior_object_id_gap = match prior_object_id_gap_of(obj.properties_bytes.as_deref()) {
            Ok(gap) => gap,
            Err(e) => {
                request_session_termination(termination_tx, &e);
                return frames;
            }
        };
        // 復号してよい Object か判定する。Group ごとに別の stream で届くため、判定の状態は
        // 購読で共有する (draft-ietf-moq-transport-22 §2.1.2 (Object States))。ロックは判定の
        // 間だけ持ち、復号 (block_in_place) の間は持たない。
        let admission = {
            let mut decode_order = video_decode_order.lock().await;
            decode_order.admit(&VideoObjectPosition {
                group_id,
                object_id: obj.object_id,
                is_key_frame,
                prior_object_id_gap,
            })
        };
        if let VideoObjectAdmission::Skip { reason } = admission {
            // 捨てるのは参照するフレームが無い Object であり、異常ではない。毎 Object で
            // warn を出すとログが埋まるため debug に留める
            tracing::debug!(
                "Skipping video object (group={group_id}, object={}, reason={reason:?})",
                obj.object_id,
            );
            continue;
        }
        frames += tokio::task::block_in_place(|| {
            decode_and_send(
                &payload,
                video_config.as_deref(),
                timestamp_us,
                video_decoder,
                sink,
            )
        });
    }
}

fn decode_and_send(
    payload: &[u8],
    video_config: Option<&[u8]>,
    timestamp_us: Option<i64>,
    decoder: &mut decoder::VideoDecoder,
    sink: &FrameSink<'_>,
) -> u64 {
    let decoded = match decoder.decode(payload, video_config) {
        Ok(frames) => frames,
        Err(e) => {
            tracing::warn!("Video decode error: {e}, payload_len={}", payload.len());
            return 0;
        }
    };
    let mut frames: u64 = 0;
    for mut frame in decoded {
        // 表示時刻を決めるのは時間軸であり、TIMESTAMP は Object からここで載せる
        frame.timestamp_us = timestamp_us;
        frames += 1;
        if sink.frame_tx.send(frame).is_err() {
            return frames;
        }
        sink.display_backlog.fetch_add(1, Ordering::Relaxed);
    }
    frames
}

/// LOC Properties の抽出結果
///
/// example の `Result` は `crate::error::Result` (エラー型固定) であるため、
/// 抽出関数は `MessageError` を返す `std::result::Result` を使う。
type ExtractResult<T> = std::result::Result<T, MessageError>;

/// object の properties バイト列から PROP_VIDEO_CONFIG を取り出す
///
/// `Ok(None)` は「Properties が無い、または PROP_VIDEO_CONFIG を持たない」を意味する。
/// `LocProperties::decode` の失敗は `Err` で返し、呼び出し側が
/// draft-ietf-moq-transport-22 §8.3 (Key-Value-Pair Structure) の MUST に従って
/// セッションを閉じる。書式違反を「プロパティ無し」に潰さない。
fn extract_video_config(properties_bytes: Option<&[u8]>) -> ExtractResult<Option<Vec<u8>>> {
    let Some(bytes) = properties_bytes else {
        return Ok(None);
    };
    let (props, _) = LocProperties::decode(bytes)?;
    for p in props.iter() {
        if p.prop_id == PROP_VIDEO_CONFIG
            && let LocPropertyValue::Bytes(ref b) = p.value
        {
            return Ok(Some(b.clone()));
        }
    }
    Ok(None)
}

/// object の properties バイト列から PROP_TIMESTAMP / PROP_TIMESCALE を取り出す
///
/// `Ok((None, None))` は「Properties が無い、または Timestamp / Timescale を持たない」を
/// 意味する。`LocProperties::decode` の失敗は `Err` で返し、呼び出し側が
/// draft-ietf-moq-transport-22 §8.3 (Key-Value-Pair Structure) の MUST に従って
/// セッションを閉じる。書式違反を「プロパティ無し」に潰さない。
fn extract_timestamp_timescale(
    properties_bytes: Option<&[u8]>,
) -> ExtractResult<(Option<u64>, Option<u64>)> {
    let Some(bytes) = properties_bytes else {
        return Ok((None, None));
    };
    let (props, _) = LocProperties::decode(bytes)?;
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
    Ok((ts, tscale))
}

/// object の properties バイト列から PROP_VIDEO_FRAME_MARKING の I ビットを取り出す
///
/// `Ok(None)` は「Properties が無い、または PROP_VIDEO_FRAME_MARKING を持たない」を意味する。
/// RFC 9626 §3.2 (Short Extension for Non-Scalable Streams) の 1 octet 形式は
/// `|S|E|I|D|0 0 0 0|` であり、I ビット (0x20) が独立フレーム (キーフレーム) を表す。
/// この節番号・ビット割当は RFC 由来であり将来変更される可能性がある。
fn extract_video_keyframe(properties_bytes: Option<&[u8]>) -> ExtractResult<Option<bool>> {
    let Some(bytes) = properties_bytes else {
        return Ok(None);
    };
    let (props, _) = LocProperties::decode(bytes)?;
    for p in props.iter() {
        if p.prop_id == PROP_VIDEO_FRAME_MARKING
            && let LocPropertyValue::Bytes(ref b) = p.value
        {
            return Ok(Some(b.first().is_some_and(|v| v & 0x20 != 0)));
        }
    }
    Ok(None)
}

/// video object がキーフレームかどうかを判定する
///
/// キーフレーム判定は PROP_VIDEO_FRAME_MARKING (I ビット) を使い、プロパティが無い場合は
/// PROP_VIDEO_CONFIG の有無で代用する (Group の先頭がキーフレームである publisher の映像も
/// 復号できるようにするため)。同じ規則を録画と復号順の判定で共有する。
/// 書式違反は `Err` で返し、呼び出し側がセッションを閉じる。
fn is_video_keyframe(
    properties_bytes: Option<&[u8]>,
    video_config: Option<&[u8]>,
) -> ExtractResult<bool> {
    Ok(match extract_video_keyframe(properties_bytes)? {
        Some(keyframe) => keyframe,
        None => video_config.is_some(),
    })
}

/// video object を録画用に writer へ送る
///
/// キーフレーム判定は [`is_video_keyframe`] に従う。Timestamp / Timescale の書式違反は
/// `Err` で返し、呼び出し側がセッションを閉じる。
fn record_video_object(
    recorder: &RecorderSender,
    properties_bytes: Option<&[u8]>,
    video_config: Option<&[u8]>,
    payload: &[u8],
) -> ExtractResult<()> {
    let keyframe = is_video_keyframe(properties_bytes, video_config)?;
    let (timestamp, timescale) = extract_timestamp_timescale(properties_bytes)?;
    recorder.video(VideoSample {
        timestamp,
        timescale,
        keyframe,
        config: video_config.map(<[u8]>::to_vec),
        data: payload.to_vec(),
    });
    Ok(())
}

/// audio object を録画用に writer へ送る
///
/// Audio Config (OpusHead) は opus codec の場合のみ解釈する (中身はコーデック依存のため)。
fn record_audio_object(
    recorder: &RecorderSender,
    timestamp: Option<u64>,
    timescale: Option<u64>,
    audio_config: Option<&[u8]>,
    is_opus_codec: bool,
    payload: &[u8],
) {
    let config = if is_opus_codec {
        audio_config.and_then(parse_opus_head)
    } else {
        None
    };
    recorder.audio(AudioSample {
        timestamp,
        timescale,
        config,
        data: payload.to_vec(),
    });
}

/// LOC の Timestamp と Timescale から表示に使う時刻 (マイクロ秒) を計算する
///
/// Timestamp は `u64` 全域を取りうるため、`as i64` キャストや `* 1_000_000` の乗算で
/// オーバーフローしないよう `u128` で中間計算し、結果が `i64` に収まらない場合は
/// `i64::MAX` に飽和させる。
///
/// `Timescale` が無いときの Timestamp は Unix epoch からのマイクロ秒である
/// (draft-ietf-moq-loc-04 §2.3.1.1)。音声のサンプル数でもフレーム数でもないため、
/// サンプルレートを仮定した換算はしない。Timescale が 0 のときも同じ扱いにする。
/// `timestamp` が無いときは `None` を返す。
fn loc_timestamp_us(timestamp: Option<u64>, timescale: Option<u64>) -> Option<i64> {
    let timestamp = timestamp?;
    let micros = match timescale {
        Some(timescale) if timescale > 0 => {
            u128::from(timestamp) * 1_000_000 / u128::from(timescale)
        }
        _ => u128::from(timestamp),
    };
    Some(i64::try_from(micros).unwrap_or(i64::MAX))
}

/// OpusHead (RFC 7845 §5.1) をパースする
///
/// magic "OpusHead"・ version 1 ・ 19 バイト以上・ Channel Count > 0 を検証する。
/// パース失敗 (magic 不一致・ version ≠ 1 ・ 19 バイト未満・ Channel Count = 0 等) は
/// `None` を返す (呼び出し側で警告して検証をスキップする)。
/// RFC 7845 §5.1 は "SHOULD accept any stream with a version number of '15' or less" と
/// 後方互換受理を推奨するが、現行 publisher は version 1 のみ送信するため、ここでは
/// version ≠ 1 をパース失敗として扱う (厳格化)。
fn parse_opus_head(bytes: &[u8]) -> Option<mp4::OpusHeadConfig> {
    if bytes.len() < 19 || &bytes[0..8] != b"OpusHead" || bytes[8] != 1 {
        return None;
    }
    let channel_count = bytes[9];
    if channel_count == 0 {
        return None;
    }
    let pre_skip = u16::from_le_bytes([bytes[10], bytes[11]]);
    let input_sample_rate = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    let output_gain = i16::from_le_bytes([bytes[16], bytes[17]]);
    Some(mp4::OpusHeadConfig {
        channel_count,
        pre_skip,
        input_sample_rate,
        output_gain,
    })
}

/// object の properties バイト列から PROP_AUDIO_CONFIG のバイト列を取り出す
///
/// `Ok(None)` は「Properties が無い、または PROP_AUDIO_CONFIG を持たない」を意味する。
/// `LocProperties::decode` の失敗は `Err` で返し、呼び出し側が
/// draft-ietf-moq-transport-22 §8.3 (Key-Value-Pair Structure) の MUST に従って
/// セッションを閉じる。書式違反を「プロパティ無し」に潰さない。
fn extract_audio_config(properties_bytes: Option<&[u8]>) -> ExtractResult<Option<Vec<u8>>> {
    let Some(bytes) = properties_bytes else {
        return Ok(None);
    };
    let (props, _) = LocProperties::decode(bytes)?;
    for p in props.iter() {
        if p.prop_id == PROP_AUDIO_CONFIG
            && let LocPropertyValue::Bytes(b) = &p.value
        {
            return Ok(Some(b.clone()));
        }
    }
    Ok(None)
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
    head: &mp4::OpusHeadConfig,
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
    mut opus_decoder: Option<&mut OpusDecoder>,
    audio_tx: &std::sync::mpsc::Sender<DecodedAudioFrame>,
    audio_config_handled: &mut bool,
    is_opus_codec: bool,
    catalog_sample_rate: u32,
    catalog_channels: u8,
    recorder: Option<&RecorderSender>,
    termination_tx: &tokio::sync::mpsc::Sender<SessionError>,
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
                            log_failure(
                                format_args!("Failed to read audio object: {e}"),
                                is_transport_session_end(&e),
                            );
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
                    log_failure(
                        format_args!("Failed to read audio payload: {e}"),
                        is_transport_session_end(&e),
                    );
                    return chunks;
                }
            }
        };
        // 配送しない Object (`FilteredOut` / `Discarded`) と payload を持たない Object は
        // Properties を解釈しないため、書式違反の検出対象外である (映像経路と同じ)。
        if !deliver {
            continue;
        }
        // 書式違反は配送の有無や処理済みフラグに関わらず毎 Object で検出するため、
        // ここでは常に抽出する (検証は下の `!*audio_config_handled` で 1 回に絞る)
        let audio_config = match extract_audio_config(obj.properties_bytes.as_deref()) {
            Ok(config) => config,
            Err(e) => {
                request_session_termination(termination_tx, &e);
                return chunks;
            }
        };
        let (timestamp, timescale) =
            match extract_timestamp_timescale(obj.properties_bytes.as_deref()) {
                Ok(ts) => ts,
                Err(e) => {
                    request_session_termination(termination_tx, &e);
                    return chunks;
                }
            };
        if let Some(recorder) = recorder {
            record_audio_object(
                recorder,
                timestamp,
                timescale,
                audio_config.as_deref(),
                is_opus_codec,
                &payload,
            );
        }
        // Audio Config (OpusHead) を含むオブジェクトを受信した最初の 1 回のみ検証する
        // (途中参加で OpusHead を受信しない場合はスキップされる)
        if !*audio_config_handled && let Some(config_bytes) = audio_config {
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
        let Some(opus_decoder) = opus_decoder.as_deref_mut() else {
            continue;
        };
        let decoded = tokio::task::block_in_place(|| opus_decoder.decode(&payload));
        let pcm = match decoded {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Opus decode error: {e}, payload_len={}", payload.len());
                continue;
            }
        };
        let sample_rate = opus_decoder.sample_rate();
        let channels = opus_decoder.channels();
        let pts_us = loc_timestamp_us(timestamp, timescale).unwrap_or(0);
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

    use shiguredo_moqt::message_parameter::LocationFilterUpdate;

    /// テスト用: targetLatency 付きの track を作る
    fn track_with_target_latency(target_latency: Option<u64>) -> MsfTrack {
        let mut track = MsfTrack::new(
            "video".to_string(),
            shiguredo_moqt::msf::MsfPackaging::Loc,
            true,
        );
        track.target_latency = target_latency;
        track
    }

    // カタログの targetLatency をそのまま読む
    #[test]
    fn catalog_target_latency_reads_the_value() {
        let tracks = [
            track_with_target_latency(Some(200)),
            track_with_target_latency(Some(200)),
        ];
        assert_eq!(
            catalog_target_latency_ms(&tracks),
            Some(200),
            "同じ render group の track は同じ値を持つ"
        );
    }

    // targetLatency を持つ track が 1 つだけでも読む
    #[test]
    fn catalog_target_latency_ignores_tracks_without_the_value() {
        let tracks = [
            track_with_target_latency(None),
            track_with_target_latency(Some(150)),
        ];
        assert_eq!(
            catalog_target_latency_ms(&tracks),
            Some(150),
            "値を持つ track だけを見る"
        );
    }

    // どの track も targetLatency を持たなければ None になる
    #[test]
    fn catalog_target_latency_is_none_without_the_value() {
        let tracks = [track_with_target_latency(None)];
        assert_eq!(
            catalog_target_latency_ms(&tracks),
            None,
            "値が無ければ時間軸は自分の遅れだけを使う"
        );
    }

    // 値が食い違うときは大きい方を採る (表示の遅れを短くしないため)
    #[test]
    fn catalog_target_latency_takes_the_larger_value() {
        let tracks = [
            track_with_target_latency(Some(100)),
            track_with_target_latency(Some(300)),
        ];
        assert_eq!(
            catalog_target_latency_ms(&tracks),
            Some(300),
            "食い違うときは大きい方を採る"
        );
    }

    // i64 に収まらない値は targetLatency として扱わない
    #[test]
    fn catalog_target_latency_ignores_values_out_of_range() {
        let tracks = [track_with_target_latency(Some(u64::MAX))];
        assert_eq!(
            catalog_target_latency_ms(&tracks),
            None,
            "i64 に収まらない値は無いものとして扱う"
        );
    }

    // timestamp が None なら時刻は決まらない
    #[test]
    fn loc_timestamp_us_returns_none_when_timestamp_is_none() {
        assert_eq!(
            loc_timestamp_us(None, Some(48_000)),
            None,
            "timestamp が None なら時刻は決まらない"
        );
    }

    // timescale が 0 のときは Timescale が無いものとして扱い、Timestamp をマイクロ秒とする
    #[test]
    fn loc_timestamp_us_treats_zero_timescale_as_microseconds() {
        assert_eq!(
            loc_timestamp_us(Some(100), Some(0)),
            Some(100),
            "timescale が 0 なら Timestamp をそのままマイクロ秒として扱う"
        );
    }

    // timescale が None のときは Timestamp を Unix epoch からのマイクロ秒として扱う
    // (draft-ietf-moq-loc-04 §2.3.1.1)
    #[test]
    fn loc_timestamp_us_treats_missing_timescale_as_microseconds() {
        assert_eq!(
            loc_timestamp_us(Some(1_700_000_000_000_000), None),
            Some(1_700_000_000_000_000),
            "timescale が None なら Timestamp は epoch マイクロ秒である"
        );
    }

    // 通常の値で正しくマイクロ秒に変換する
    #[test]
    fn loc_timestamp_us_converts_normal_values() {
        // 1_000_000 / 1_000_000 * 1_000_000 = 1_000_000
        assert_eq!(
            loc_timestamp_us(Some(1_000_000), Some(1_000_000)),
            Some(1_000_000),
            "timestamp と timescale が等しければ 1 秒 (1_000_000 us)"
        );
        // 音声のサンプル数として解釈される例: 48_000 / 48_000 = 1 秒
        assert_eq!(
            loc_timestamp_us(Some(48_000), Some(48_000)),
            Some(1_000_000),
            "Timescale があるときはその時間軸の値として換算する"
        );
    }

    // 巨大な Timestamp でもオーバーフローせず i64::MAX に飽和する (旧実装は debug で panic した)
    #[test]
    fn loc_timestamp_us_saturates_on_huge_timestamp() {
        // 1e13 * 1_000_000 = 1e19 > i64::MAX (約 9.2e18) なので飽和する
        assert_eq!(
            loc_timestamp_us(Some(10_000_000_000_000), Some(1)),
            Some(i64::MAX),
            "巨大な Timestamp は i64::MAX に飽和する"
        );
        // u64::MAX でも panic せず飽和する
        assert_eq!(
            loc_timestamp_us(Some(u64::MAX), Some(1)),
            Some(i64::MAX),
            "u64::MAX でも panic せず飽和する"
        );
        // Timescale が無いときは換算しないため、そのまま飽和する
        assert_eq!(
            loc_timestamp_us(Some(u64::MAX), None),
            Some(i64::MAX),
            "Timescale が無いときも i64::MAX に飽和する"
        );
    }

    // 巨大な Timescale でも符号ラップせず正しく計算する
    #[test]
    fn loc_timestamp_us_does_not_wrap_on_huge_timescale() {
        // u64::MAX * 1_000_000 / u64::MAX = 1_000_000 (u128 中間計算でオーバーフローしない)
        assert_eq!(
            loc_timestamp_us(Some(u64::MAX), Some(u64::MAX)),
            Some(1_000_000),
            "巨大な Timescale でも符号ラップせず正しく計算する"
        );
    }

    // i64 範囲に収まる大きな値は飽和せずそのまま返す
    #[test]
    fn loc_timestamp_us_returns_large_value_without_saturation() {
        // 9e12 * 1_000_000 = 9e18 < i64::MAX (約 9.223e18) なので飽和しない
        assert_eq!(
            loc_timestamp_us(Some(9_000_000_000_000), Some(1)),
            Some(9_000_000_000_000_000_000),
            "i64 に収まる大きな値は飽和せずそのまま返す"
        );
    }

    /// テスト用: OpusHead を構築する (RFC 7845 §5.1 のフォーマット)
    ///
    /// publisher 側 (examples/moq-pub の `build_opus_head`) と同じ形式。
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
    /// (テストデータの拡張部は Channel Mapping Family = 1 の想定で、Stream Count 1 /
    /// Coupled Count 1 / Channel Mapping [0, 1] の 4 バイトを追加している)
    #[test]
    fn parse_opus_head_accepts_longer_head() {
        let mut head = build_test_opus_head(48_000, 2);
        head.extend_from_slice(&[1, 1, 0, 1]); // Channel Mapping Family = 1 + mapping (C=2 で 4 バイト)
        let parsed = parse_opus_head(&head).expect("19 バイト超の OpusHead は受理されること");
        assert_eq!(parsed.input_sample_rate, 48_000);
        assert_eq!(parsed.channel_count, 2);
    }

    /// OpusHead パース: Pre-skip / Output Gain を取り出せる (RFC 7845 §5.1)
    #[test]
    fn parse_opus_head_extracts_pre_skip_and_output_gain() {
        let mut head = Vec::with_capacity(19);
        head.extend_from_slice(b"OpusHead");
        head.push(1);
        head.push(2);
        head.extend_from_slice(&312u16.to_le_bytes()); // Pre-skip
        head.extend_from_slice(&48_000u32.to_le_bytes()); // Input Sample Rate
        head.extend_from_slice(&(-512i16).to_le_bytes()); // Output Gain
        head.push(0);
        let parsed = parse_opus_head(&head).expect("正しい OpusHead はパースできること");
        assert_eq!(parsed.pre_skip, 312, "Pre-skip が取り出せること");
        assert_eq!(parsed.output_gain, -512, "Output Gain が取り出せること");
    }

    /// PROP_VIDEO_FRAME_MARKING の I ビットでキーフレームを判定できる (RFC 9626 §3.2)
    #[test]
    fn extract_video_keyframe_reads_i_bit() {
        use shiguredo_moqt::loc::LocProperty;

        // 1 octet 形式は |S|E|I|D|0 0 0 0| であり、I ビット (0x20) が独立フレームを表す
        for (marking, expected) in [
            (0xE0u8, true),
            (0xC0, false),
            (0x20, true),
            (0x00, false),
            (0xA0, true),
        ] {
            let mut props = LocProperties::new();
            props.push(LocProperty {
                prop_id: PROP_VIDEO_FRAME_MARKING,
                value: LocPropertyValue::Bytes(vec![marking]),
            });
            let bytes = props
                .encode()
                .expect("テストフィクスチャの前提条件を満たす");
            assert_eq!(
                extract_video_keyframe(Some(&bytes)),
                Ok(Some(expected)),
                "marking={marking:#04x} の I ビット判定"
            );
        }
    }

    /// PROP_VIDEO_FRAME_MARKING が無ければ Ok(None)、切り詰めはエラー
    #[test]
    fn extract_video_keyframe_handles_absent_and_truncated() {
        let props = LocProperties::new();
        let bytes = props
            .encode()
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            extract_video_keyframe(Some(&bytes)),
            Ok(None),
            "持たなければ Ok(None) であること"
        );
        assert_eq!(
            extract_video_keyframe(None),
            Ok(None),
            "properties 自体が無ければ Ok(None) であること"
        );
        assert_eq!(
            extract_video_keyframe(Some(&[0xFF, 0xFF, 0xFF])),
            Err(MessageError::UnexpectedEof),
            "切り詰められた properties は UnexpectedEof であること"
        );
    }

    /// キーフレーム判定は PROP_VIDEO_FRAME_MARKING を優先し、無ければ PROP_VIDEO_CONFIG で代用する
    ///
    /// 録画と復号順の判定が同じ規則を使うことを固定する。
    #[test]
    fn is_video_keyframe_falls_back_to_video_config() {
        use shiguredo_moqt::loc::LocProperty;

        // PROP_VIDEO_FRAME_MARKING があればその I ビットを使う (PROP_VIDEO_CONFIG の有無より優先)
        let mut marked = LocProperties::new();
        marked.push(LocProperty {
            prop_id: PROP_VIDEO_FRAME_MARKING,
            value: LocPropertyValue::Bytes(vec![0xC0]),
        });
        let marked_bytes = marked
            .encode()
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            is_video_keyframe(Some(&marked_bytes), Some(&[0x01])),
            Ok(false),
            "I ビットが立っていなければキーフレームとしないこと"
        );

        // PROP_VIDEO_FRAME_MARKING が無ければ PROP_VIDEO_CONFIG の有無で代用する
        let props = LocProperties::new();
        let bytes = props
            .encode()
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            is_video_keyframe(Some(&bytes), Some(&[0x01])),
            Ok(true),
            "Video Config を持つ Object をキーフレームとみなすこと"
        );
        assert_eq!(
            is_video_keyframe(Some(&bytes), None),
            Ok(false),
            "Video Config を持たなければキーフレームとしないこと"
        );
        assert_eq!(
            is_video_keyframe(None, Some(&[0x01])),
            Ok(true),
            "Properties が無くても Video Config があればキーフレームとみなすこと"
        );

        // 書式違反は「キーフレームでない」に潰さない
        assert_eq!(
            is_video_keyframe(Some(&[0xFF, 0xFF, 0xFF]), None),
            Err(MessageError::UnexpectedEof),
            "切り詰められた properties は UnexpectedEof であること"
        );
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
        assert_eq!(
            extract_audio_config(Some(&encoded)),
            Ok(Some(head)),
            "Audio Config のバイト列が取り出せること"
        );
    }

    /// PROP_AUDIO_CONFIG の抽出: 無ければ Ok(None)
    #[test]
    fn extract_audio_config_returns_none_when_absent() {
        let props = LocProperties::new();
        let encoded = props.encode().expect("encode に成功すること");
        assert_eq!(
            extract_audio_config(Some(&encoded)),
            Ok(None),
            "Audio Config が無ければ Ok(None) であること"
        );
        assert_eq!(
            extract_audio_config(None),
            Ok(None),
            "properties 自体が無ければ Ok(None) であること"
        );
    }

    /// PROP_AUDIO_CONFIG の抽出: 切り詰められた properties は UnexpectedEof
    ///
    /// `[0xFF, 0xFF, 0xFF]` は 8 バイト形 varint の途中終端であり、プロパティ無しの
    /// `Ok(None)` とは区別される (呼び出し側はセッションを閉じる)。
    #[test]
    fn extract_audio_config_reports_eof_on_truncated_properties() {
        assert_eq!(
            extract_audio_config(Some(&[0xFF, 0xFF, 0xFF])),
            Err(MessageError::UnexpectedEof),
            "切り詰められた properties は UnexpectedEof であること"
        );
    }

    /// PROP_VIDEO_CONFIG の抽出: LOC の Public Properties から parameter set を取り出せる
    ///
    /// draft-ietf-moq-loc-04 §2.2 (MOQ Object Mapping) は LOC の Public Properties を
    /// MOQ Object Properties に載せると規定する。Subgroup 経路と FETCH 経路の両方が
    /// この関数を通して H.264/H.265 の AVCDecoderConfigurationRecord を取り出す。
    #[test]
    fn extract_video_config_returns_config() {
        use shiguredo_moqt::loc::LocProperty;

        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_VIDEO_CONFIG,
            value: LocPropertyValue::Bytes(vec![0x01, 0x64, 0x00, 0x1F]),
        });
        let bytes = props
            .encode()
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            extract_video_config(Some(&bytes)),
            Ok(Some(vec![0x01, 0x64, 0x00, 0x1F])),
            "PROP_VIDEO_CONFIG のバイト列を取り出せること"
        );
    }

    /// LOC の書式違反 (Video Frame Marking の宣言長 5) は KeyValueFormattingError
    ///
    /// draft-ietf-moq-transport-22 §8.3 (Key-Value-Pair Structure) は、理解している型の
    /// Length/Value が定義と一致しない場合に KEY_VALUE_FORMATTING_ERROR (0x6) で
    /// セッションを閉じる MUST を定める。Video Frame Marking の Length は 1-4 バイト
    /// (draft-ietf-moq-loc-04 §2.3.2.2 (Video Frame Marking)) である。
    /// 書式違反をプロパティ無しに潰さないことを固定する。
    #[test]
    fn extract_video_config_reports_key_value_formatting_error() {
        // LocProperties::encode は違反値を拒否するため、生バイト列を組み立てる
        // (Properties Length = 7 = delta 1 バイト + Length 1 バイト + 値 5 バイト)
        let mut bytes = vec![0x07, 0x09, 0x05];
        bytes.extend_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05]);
        let err = extract_video_config(Some(&bytes)).expect_err("書式違反はエラーになること");
        assert!(
            matches!(err, MessageError::KeyValueFormattingError(_)),
            "Video Frame Marking の宣言長違反は KeyValueFormattingError であること: {err:?}"
        );
    }

    /// PROP_VIDEO_CONFIG の抽出: Properties 自体が無ければ Ok(None)
    #[test]
    fn extract_video_config_returns_none_when_absent() {
        assert_eq!(
            extract_video_config(None),
            Ok(None),
            "properties 自体が無ければ Ok(None) であること"
        );
    }

    /// PROP_VIDEO_CONFIG の抽出: 切り詰められた properties は UnexpectedEof
    #[test]
    fn extract_video_config_reports_eof_on_truncated_properties() {
        assert_eq!(
            extract_video_config(Some(&[0xFF, 0xFF, 0xFF])),
            Err(MessageError::UnexpectedEof),
            "切り詰められた properties は UnexpectedEof であること"
        );
    }

    /// PROP_VIDEO_CONFIG の抽出: 他の Bytes プロパティしか無ければ Ok(None)
    ///
    /// PROP_AUDIO_CONFIG も奇数 ID の Bytes 値であるため、prop_id を見ずに値型だけで
    /// 判定する実装でも取り出せてしまう。prop_id の判定が効いていることを固定する。
    #[test]
    fn extract_video_config_ignores_other_bytes_properties() {
        use shiguredo_moqt::loc::LocProperty;

        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_AUDIO_CONFIG,
            value: LocPropertyValue::Bytes(vec![0xAA, 0xBB]),
        });
        let bytes = props
            .encode()
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            extract_video_config(Some(&bytes)),
            Ok(None),
            "PROP_AUDIO_CONFIG しか無ければ Ok(None) であること"
        );
    }

    /// PROP_VIDEO_CONFIG の抽出: 持たない場合は Ok(None)
    #[test]
    fn extract_video_config_returns_none_without_config() {
        use shiguredo_moqt::loc::LocProperty;

        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(1),
        });
        let bytes = props
            .encode()
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            extract_video_config(Some(&bytes)),
            Ok(None),
            "VIDEO_CONFIG を持たない Properties は Ok(None) であること"
        );
    }

    /// LOC の書式違反 (Audio Level 256) は KeyValueFormattingError
    ///
    /// Audio Level は 8 bit 範囲 (draft-ietf-moq-loc-04 §2.3.3.2 (Audio Level)) のため、256 は
    /// §8.3 (Key-Value-Pair Structure) の書式違反になる。Audio Config / Timestamp /
    /// Timescale の抽出も同じ扱いであることを固定する。
    #[test]
    fn extract_audio_fields_report_key_value_formatting_error() {
        // Properties Length = 3 / delta=0x0C (Audio Level) / vi64 の 256 (2 バイト形)
        let mut bytes = vec![0x03, 0x0C];
        shiguredo_moqt::varint::encode(256, &mut bytes);
        assert_eq!(
            usize::from(bytes[0]),
            bytes.len() - 1,
            "宣言した Properties Length と実データ長が一致すること"
        );
        let err = extract_audio_config(Some(&bytes)).expect_err("書式違反はエラーになること");
        assert!(
            matches!(err, MessageError::KeyValueFormattingError(_)),
            "Audio Level の値域違反は KeyValueFormattingError であること: {err:?}"
        );
        let err =
            extract_timestamp_timescale(Some(&bytes)).expect_err("書式違反はエラーになること");
        assert!(
            matches!(err, MessageError::KeyValueFormattingError(_)),
            "Timestamp / Timescale の抽出でも同じエラーになること: {err:?}"
        );
    }

    /// Timestamp / Timescale の抽出: 無ければ Ok((None, None))
    #[test]
    fn extract_timestamp_timescale_returns_none_when_absent() {
        let props = LocProperties::new();
        let encoded = props.encode().expect("encode に成功すること");
        assert_eq!(
            extract_timestamp_timescale(Some(&encoded)),
            Ok((None, None)),
            "Timestamp / Timescale が無ければ Ok((None, None)) であること"
        );
        assert_eq!(
            extract_timestamp_timescale(None),
            Ok((None, None)),
            "properties 自体が無ければ Ok((None, None)) であること"
        );
    }

    /// 終了要因ごとに後始末の送信有無が決まること
    #[test]
    fn cleanup_decisions_follow_termination_cause() {
        // 通常終了 (peer も自側も終了していない) は STOP_SENDING と GOAWAY / 正常 close を送る
        assert!(
            should_stop_sending(false, false),
            "通常終了では STOP_SENDING を送ること"
        );
        assert!(
            should_close_gracefully(false),
            "通常終了では GOAWAY と正常 close を送ること"
        );

        // peer が終了させた場合は STOP_SENDING だけを送らない
        assert!(
            !should_stop_sending(true, false),
            "peer が終了させた場合は STOP_SENDING を送らないこと"
        );
        assert!(
            should_close_gracefully(false),
            "peer が終了させた場合も GOAWAY と正常 close は送ること"
        );

        // 自側で終了コード付きに閉じた場合はどちらも送らない
        assert!(
            !should_stop_sending(false, true),
            "自側で閉じた場合は STOP_SENDING を送らないこと"
        );
        assert!(
            !should_close_gracefully(true),
            "自側で閉じた場合は GOAWAY と正常 close を送らないこと"
        );

        // 回収経路では peer の終了と自側の終了が同時に立ち得る
        assert!(
            !should_stop_sending(true, true),
            "両方が終了した場合は STOP_SENDING を送らないこと"
        );
        assert!(
            !should_close_gracefully(true),
            "両方が終了した場合は GOAWAY と正常 close を送らないこと"
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
    /// セッション終了として扱わないエラーは後始末 (stream task の join と録画の finalize) を
    /// 終えてから `run` の戻り値として返る (終了コード 1)。
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
            Error::Moqt(MessageError::UnexpectedEof),
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

    /// LOC の decode 失敗がセッション終了コードへ写ること
    ///
    /// draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) の
    /// KEY_VALUE_FORMATTING_ERROR (0x6) と PROTOCOL_VIOLATION (0x3) を使い分ける。
    #[test]
    fn session_error_code_maps_decode_failures() {
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

    /// 検出した書式違反が main ループへ終了依頼として届くこと
    ///
    /// stream task は `request_session_termination` で終了コードと理由を送り、
    /// main ループが `MoqtClient::close` を呼ぶ。ここでは送信までを固定する
    /// (close の実行は transport を必要とするため実機確認で扱う)。
    #[tokio::test]
    async fn request_session_termination_sends_error_and_keeps_first_request() {
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel::<SessionError>(1);

        // 書式違反は KEY_VALUE_FORMATTING_ERROR (0x6) として送られる
        let error = MessageError::KeyValueFormattingError("LOC property value is out of range");
        request_session_termination(&termination_tx, &error);

        // 容量 1 のため、依頼が積まれている間の 2 件目は届かない (panic もしない)
        request_session_termination(&termination_tx, &MessageError::UnexpectedEof);

        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), termination_rx.recv())
                .await
                .expect("終了依頼が送られること"),
            Some(SessionError::new(
                SESSION_KEY_VALUE_FORMATTING_ERROR,
                "LOC property value is out of range",
            )),
            "先に積まれた書式違反の依頼が届き、後続の依頼で置き換わらないこと"
        );
    }

    /// 検証: 整合する場合は警告が 0 件
    #[test]
    fn validate_audio_config_matching_values_no_warnings() {
        let head = mp4::OpusHeadConfig {
            channel_count: 1,
            input_sample_rate: 48_000,
            pre_skip: 0,
            output_gain: 0,
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
        let head = mp4::OpusHeadConfig {
            channel_count: 1,
            input_sample_rate: 44_100,
            pre_skip: 0,
            output_gain: 0,
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
        let head_zero = mp4::OpusHeadConfig {
            channel_count: 1,
            input_sample_rate: 0,
            pre_skip: 0,
            output_gain: 0,
        };
        assert!(
            validate_audio_config(&head_zero, 48_000, 1).is_empty(),
            "Input Sample Rate 0 (unspecified) は不一致としないこと"
        );
    }

    /// 検証: Channel Count 不一致で警告が出ること
    #[test]
    fn validate_audio_config_channel_mismatch_warns() {
        let head = mp4::OpusHeadConfig {
            channel_count: 2,
            input_sample_rate: 48_000,
            pre_skip: 0,
            output_gain: 0,
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
        let head = mp4::OpusHeadConfig {
            channel_count: 2,
            input_sample_rate: 44_100,
            pre_skip: 0,
            output_gain: 0,
        };
        let warnings = validate_audio_config(&head, 48_000, 1);
        assert_eq!(
            warnings.len(),
            2,
            "両方の不一致で警告が 2 件であること: {warnings:?}"
        );
    }

    /// カタログ購読が FILL_PARAMETERS 付きで、fill range が現在 Group の先頭からであること
    ///
    /// draft-ietf-moq-transport-22 §3.5 (Joining an Ongoing Track): 購読の Location Filter は
    /// Next Object、FILL_PARAMETERS 内側の Location Filter は現在 Group の先頭 (Relative Start の
    /// StartGroup=1)。内側を省略すると購読の Next Object を継承し、購読より前に publish された
    /// 独立カタログが fill range に入らない (同 §3.4 (Fill Semantics))。
    #[test]
    fn catalog_subscribe_parameters_request_a_fill_from_the_group_start() {
        let parameters = catalog_subscribe_parameters();
        assert_eq!(
            parameters
                .location_filter_typed()
                .expect("LOCATION_FILTER を読めること"),
            Some(LocationFilter::NextObject),
            "購読は Next Object から始めること"
        );
        let fill = parameters
            .fill_parameters()
            .expect("FILL_PARAMETERS を持つこと");
        assert_eq!(
            fill.location_filter_update()
                .expect("内側の LOCATION_FILTER を読めること"),
            LocationFilterUpdate::Set(LocationFilter::RelativeGroup { start_group: 1 }),
            "fill range は現在 Group の先頭からにすること"
        );
    }
}
