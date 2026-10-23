//! publisher の全体パイプライン
//!
//! カメラ/マイクキャプチャ → 映像/音声エンコード → MOQT 送信の流れを、
//! sans-I/O な `shiguredo_moqt::session::core::Session` を駆動する
//! [`tokio_moq::moqt_client::MoqtClient`] と結線する。

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use shiguredo_audio_device::AudioFrameOwned;
use shiguredo_moqt::{
    audio_clock::{AudioTimestampClock, AudioTimestampOffsetStats},
    error::{
        REQUEST_DOES_NOT_EXIST, REQUEST_INVALID_FILTER, REQUEST_INVALID_RANGE,
        REQUEST_NOT_SUPPORTED,
    },
    loc::{
        LocProperties, LocProperty, LocPropertyValue, PROP_AUDIO_CONFIG, PROP_TIMESCALE,
        PROP_TIMESTAMP, PROP_VIDEO_CONFIG, PROP_VIDEO_FRAME_MARKING,
    },
    media_clock::WallClockMapper,
    message::ControlMessage,
    message::Fetch,
    message::common::Location,
    message::common::TrackNamespace,
    message_parameter::{LocationFilter, LocationFilterContext, MessageParameters},
    msf::MSF_CATALOG_TRACK_NAME,
    name::serialize_namespace,
    session::types::DataStreamId,
    session::types::SessionEvent,
    stream::encoder::FetchObjectInput,
    track_properties::{
        PROP_DYNAMIC_GROUPS, PROP_MAX_CACHE_DURATION, TrackProperties, TrackProperty,
        TrackPropertyValue,
    },
};
use shiguredo_video_device::VideoFrameOwned;
use tokio::sync::{mpsc, oneshot, watch};

use crate::audio_capture;
use crate::cache;
use crate::capture;
use crate::catalog;
use crate::cli::{Config, VideoCodec};
use crate::datagram_writer::DatagramWriter;
use shiguredo_moqt::playout::publish_catch_up::{
    AudioPublishCatchUp, AudioPublishCatchUpChunk, AudioPublishCatchUpFrame,
    AudioPublishCatchUpSendComplete, AudioPublishCatchUpSendStart, AudioPublishCatchUpStats,
};
use shiguredo_moqt::playout::splice_fade::{AudioSpliceEdge, fade_splice_edge_i16};

use crate::encoder::audio::AudioEncoder;
use crate::encoder::{self, EncodedFrame};
use crate::error::{Error, Result};
use crate::fake_audio_capture;
use crate::fake_capture;
use crate::gop;
use crate::group_id;
use crate::mp4;
use crate::stream_writer::SubgroupWriter;
use tokio_moq::error::TransportError;
use tokio_moq::error::session_error_code;
use tokio_moq::moqt_client::{
    ClientEvent, DataPlaneHandle, IncomingRequest, MoqtClient, ObjectFilterOutcome,
};
use tokio_moq::transport::StreamHandle;
use tokio_moq::webtransport::WtSessionState;
use tokio_moq::{
    ConnectParams, MigrationDeadline, MigrationLimiter, MigrationScope, RetryPolicy, ServerUrl,
    Transport, connect_with_retry,
};

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
pub(crate) const AUDIO_TRACK_NAME: &str = "audio";
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

/// 音声の配信の遅れの内訳をログへ出す間隔 (ミリ秒)
const AUDIO_STATS_INTERVAL_MS: u64 = 10_000;

/// 制御メッセージの応答を待つ上限 (ミリ秒)
///
/// draft-ietf-moq-transport-22 §9 (Control Messages): PUBLISH などの制御メッセージを送った
/// 後、peer の応答をこの時間待つ。応答を返さない peer に移行処理が引きずられないよう、
/// 移行先の session にも同じ値を設定する。
const CONTROL_MESSAGE_TIMEOUT_MS: u64 = 30_000;

/// data stream の進捗を待つ上限 (ミリ秒)
///
/// draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) の DATA_STREAM_TIMEOUT に
/// 対応する。subgroup stream の書き込みが止まったままにならないよう設定する。
const DATA_STREAM_TIMEOUT_MS: u64 = 30_000;

/// GOAWAY による移行を制限する時間窓
///
/// 移行先がまた GOAWAY を返す構成では、移行を無制限に繰り返すと再接続ストームになる。
/// この時間窓の中で [`MAX_MIGRATIONS_PER_WINDOW`] 回を超える移行を拒否する。
const MIGRATION_LIMIT_WINDOW: Duration = Duration::from_secs(600);

/// 時間窓内で許す移行回数
const MAX_MIGRATIONS_PER_WINDOW: usize = 5;

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
async fn reset_open_subgroup_stream(
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
    match writer.reset_by_session(error_code).await {
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

/// `MAX_CACHE_DURATION` を告知する Track Properties を作る
///
/// draft-ietf-moq-transport-22 §10.3 (MAX CACHE DURATION): 保持した Object を配れる時間を
/// ミリ秒で示す。この example は `--cache-duration` の時間窓だけ保持するため、同じ値を
/// 告知する。告知しないと購読者と relay が「いつまで FETCH で配れるか」を知れない。
fn cache_duration_properties(cache_duration_ms: u64) -> TrackProperties {
    let mut properties = TrackProperties::new();
    properties.push(TrackProperty {
        prop_type: PROP_MAX_CACHE_DURATION,
        value: TrackPropertyValue::VarInt(cache_duration_ms),
    });
    properties
}

/// 映像トラックに載せる Track Properties
///
/// draft-ietf-moq-transport-22 §10.6 (DYNAMIC_GROUPS) の `DYNAMIC_GROUPS=1` を告知し、
/// 購読者からの `NEW_GROUP_REQUEST` (§9.20.19) に応じて Group を動的に開始できることを
/// 示す。購読者は `DYNAMIC_GROUPS=1` を知らないと REQUEST_UPDATE で `NEW_GROUP_REQUEST` を
/// 送れないため、PUBLISH と SUBSCRIBE_OK の両方で告知する (§3.5.1)。音声とカタログの
/// トラックには載せない。
///
/// `MAX_CACHE_DURATION` は映像 / 音声のどちらにも載せる (保持するのはメディアだけである)。
fn video_track_properties(cache_duration_ms: u64) -> TrackProperties {
    let mut properties = cache_duration_properties(cache_duration_ms);
    properties.push(TrackProperty {
        prop_type: PROP_DYNAMIC_GROUPS,
        value: TrackPropertyValue::VarInt(1),
    });
    properties
}

/// 映像トラックの PUBLISH に載せる Track Properties を入力モードから決める
///
/// MP4 パススルーはエンコーダを使わないため、購読者からの `NEW_GROUP_REQUEST` に応じて
/// Group を開始できない。できないことを告知しないよう、`DYNAMIC_GROUPS` は映像を
/// エンコードするときだけ載せる。`MAX_CACHE_DURATION` はパススルーでも保持するため載せる。
fn video_publish_properties(mp4_passthrough: bool, cache_duration_ms: u64) -> TrackProperties {
    if mp4_passthrough {
        cache_duration_properties(cache_duration_ms)
    } else {
        video_track_properties(cache_duration_ms)
    }
}

/// SUBSCRIBE_OK に載せる Track Properties
///
/// 映像トラックには PUBLISH と同じ `DYNAMIC_GROUPS=1` を載せる。購読者はこの値で publisher が
/// 動的 Group に対応しているかを知り (draft-ietf-moq-transport-22 §3.5.1)、REQUEST_UPDATE で
/// `NEW_GROUP_REQUEST` を送れるようになる (§9.20.19 の MUST NOT)。音声トラックには
/// `MAX_CACHE_DURATION` だけを載せ、カタログには何も載せない。
fn subscribe_ok_properties(
    track_alias: u64,
    mp4_passthrough: bool,
    cache_duration_ms: u64,
) -> TrackProperties {
    if track_alias == VIDEO_TRACK_ALIAS {
        video_publish_properties(mp4_passthrough, cache_duration_ms)
    } else if track_alias == AUDIO_TRACK_ALIAS {
        cache_duration_properties(cache_duration_ms)
    } else {
        TrackProperties::new()
    }
}

/// 受信したパラメータから、映像のキーフレーム要求として受け付ける `NEW_GROUP_REQUEST` を取り出す
///
/// `can_force_keyframe` は要求を映像のキーフレーム要求として扱えるかどうかである。映像
/// トラック宛てであっても、キーフレームを強制できない経路 (MP4 パススルー) では扱わない。
/// 値が 0 (購読者が Group の情報を持たない) か、現在の Group より大きいときだけ受け付ける
/// (draft-ietf-moq-transport-22 §9.20.19)。受け付けない要求は debug ログに残す。
/// 呼び出し側は購読が成立した後に [`gop::GopTracker::request_new_group`] を呼ぶ。
fn accepted_new_group_request(
    can_force_keyframe: bool,
    parameters: &MessageParameters,
    started_video_group_id: Option<u64>,
) -> Option<u64> {
    let next_group_id = parameters.new_group_request()?;
    if !can_force_keyframe {
        tracing::debug!(
            "Ignoring NEW_GROUP_REQUEST that cannot force a video keyframe: \
             next_group_id={next_group_id}"
        );
        return None;
    }
    if gop::accepts_new_group_request(next_group_id, started_video_group_id) {
        return Some(next_group_id);
    }
    tracing::debug!(
        "Ignoring NEW_GROUP_REQUEST that does not ask for a future group: \
         next_group_id={next_group_id}, current={started_video_group_id:?}"
    );
    None
}

/// 確立済みの publisher session
///
/// SETUP の交換と映像 / 音声 / カタログの PUBLISH まで終えた状態である。初回接続と GOAWAY 後の
/// 移行の双方がこの値を作り、main ループが 1 箇所で参照先を差し替える。
struct PublisherSession {
    /// MOQT クライアント
    client: MoqtClient,
    /// data plane 通知用ハンドル
    data_plane: DataPlaneHandle,
    /// 映像 PUBLISH の request id (映像を配信しない場合は `None`)
    video_request_id: Option<u64>,
    /// 音声 PUBLISH の request id (音声を配信しない場合は `None`)
    audio_request_id: Option<u64>,
    /// カタログ PUBLISH の request id
    catalog_request_id: u64,
}

/// 接続に使う設定を組み立てる
///
/// 初回接続と GOAWAY 後の移行で同じ値を使う。publisher は datagram を受信しない。
fn publisher_connect_params<'a>(config: &'a Config, url: &'a ServerUrl) -> ConnectParams<'a> {
    ConnectParams {
        url,
        transport: config.transport,
        cert: config.cert.as_deref(),
        impl_name: "moq-pub",
        // publisher は datagram を受信しない
        receive_datagrams: false,
        // QMux 経路で音声の OBJECT_DATAGRAM がピアの上限に収まるかを確立時に確認する。
        // datagram を送らない設定では確認しない (`--datagram-max-size` が使われないため)
        datagram_max_size: (config.audio_enabled && config.audio_datagram)
            .then_some(config.datagram_max_size),
    }
}

/// 確立直後の client に publisher 用のタイムアウトを設定する
///
/// 接続経路によらず同じ値を使う。応答待ちの内部ループは `tick_now` で期限を進めるため、
/// 応答を返さない peer でもここで設定した期限で失敗する。
fn configure_publisher_session(client: &mut MoqtClient) {
    client.set_control_message_timeout_ms(Some(CONTROL_MESSAGE_TIMEOUT_MS));
    client.set_data_stream_timeout_ms(Some(DATA_STREAM_TIMEOUT_MS));
}

/// 映像トラックの PUBLISH を発行し、request id を返す
///
/// MP4 パススルーはキーフレームを要求できないため `DYNAMIC_GROUPS` を告知しない
/// ([`video_publish_properties`])。
async fn publish_video_track(
    client: &mut MoqtClient,
    config: &Config,
    namespace: &TrackNamespace,
) -> Result<u64> {
    Ok(client
        .publish_track(
            namespace.clone(),
            config.track_name.as_bytes().to_vec(),
            VIDEO_TRACK_ALIAS,
            video_publish_properties(config.input_mp4.is_some(), config.cache_duration_ms),
        )
        .await?)
}

/// 音声トラックの PUBLISH を発行し、request id を返す
///
/// 音声も同じ時間窓だけ保持するため、映像と同じ `MAX_CACHE_DURATION` を告知する。
async fn publish_audio_track(
    client: &mut MoqtClient,
    config: &Config,
    namespace: &TrackNamespace,
) -> Result<u64> {
    Ok(client
        .publish_track(
            namespace.clone(),
            AUDIO_TRACK_NAME.as_bytes().to_vec(),
            AUDIO_TRACK_ALIAS,
            cache_duration_properties(config.cache_duration_ms),
        )
        .await?)
}

/// カタログトラックの PUBLISH を発行し、request id を返す
async fn publish_catalog_track(client: &mut MoqtClient, namespace: &TrackNamespace) -> Result<u64> {
    Ok(client
        .publish_track(
            namespace.clone(),
            MSF_CATALOG_TRACK_NAME.to_vec(),
            CATALOG_TRACK_ALIAS,
            TrackProperties::new(),
        )
        .await?)
}

/// 映像 / 音声 / カタログの PUBLISH を発行する
///
/// 途中で失敗した場合に session を閉じられるよう、client は呼び出し側が所有したままにする。
/// 戻り値は (映像 request id, 音声 request id, カタログ request id)。
async fn publish_all_tracks(
    client: &mut MoqtClient,
    config: &Config,
    namespace: &TrackNamespace,
) -> Result<(Option<u64>, Option<u64>, u64)> {
    let video_request_id = if config.video_enabled {
        Some(publish_video_track(client, config, namespace).await?)
    } else {
        None
    };
    let audio_request_id = if config.audio_enabled {
        Some(publish_audio_track(client, config, namespace).await?)
    } else {
        None
    };
    let catalog_request_id = publish_catalog_track(client, namespace).await?;
    Ok((video_request_id, audio_request_id, catalog_request_id))
}

/// 接続を確立し、映像 / 音声 / catalog の PUBLISH を張る
///
/// 初回接続に使う。SETUP まで終えた `MoqtClient` と、その接続に紐づく `DataPlaneHandle` と
/// 各 track の Request ID を返す。
async fn establish_publisher_session(
    config: &Config,
    url: &ServerUrl,
    namespace: &TrackNamespace,
    task_monitor: &tokio_metrics::TaskMonitor,
) -> Result<PublisherSession> {
    let (mut client, _acceptor) =
        tokio_moq::connect(&publisher_connect_params(config, url), task_monitor).await?;
    configure_publisher_session(&mut client);

    let data_plane = client.data_plane();
    // PUBLISH の途中で失敗した場合、確立済みの session を放置すると peer が timeout まで
    // 資源を保持するため、閉じてからエラーを返す
    match publish_all_tracks(&mut client, config, namespace).await {
        Ok((video_request_id, audio_request_id, catalog_request_id)) => Ok(PublisherSession {
            client,
            data_plane,
            video_request_id,
            audio_request_id,
            catalog_request_id,
        }),
        Err(e) => {
            if let Err(close_error) = client.close(0, "").await {
                tracing::debug!(
                    "Failed to close the session after a failed publish: {close_error}"
                );
            }
            Err(e)
        }
    }
}

/// 移行期限付きで接続し、映像 / 音声 / catalog の PUBLISH を張る
///
/// draft-ietf-moq-transport-22 §6.6.1 (Graceful Session Migration) / §9.2 (GOAWAY):
/// GOAWAY の Timeout を移行期限として扱い、期限まで接続を再試行する。PUBLISH の応答待ちにも
/// 残り時間を上限として使い、期限を過ぎた移行を打ち切る。接続で時間を使った分は期限 (絶対
/// 時刻) から自然に差し引かれる。途中まで確立した session は失敗時に閉じる。
/// draft 由来の規則であり将来の draft 改版で変わる可能性がある。
async fn establish_publisher_session_with_retry(
    config: &Config,
    url: &ServerUrl,
    namespace: &TrackNamespace,
    task_monitor: &tokio_metrics::TaskMonitor,
    deadline: MigrationDeadline,
) -> Result<PublisherSession> {
    let (mut client, _acceptor) = connect_with_retry(
        &publisher_connect_params(config, url),
        deadline,
        RetryPolicy::default(),
        task_monitor,
    )
    .await?;
    configure_publisher_session(&mut client);

    let data_plane = client.data_plane();
    let published = tokio::time::timeout(
        deadline.remaining(),
        publish_all_tracks(&mut client, config, namespace),
    )
    .await;
    let published = match published {
        Ok(published) => published,
        Err(_) => {
            // 期限切れ。途中まで確立した session を閉じてから失敗を返す
            if let Err(close_error) = client.close(0, "").await {
                tracing::debug!(
                    "Failed to close the session after the migration deadline expired: \
                     {close_error}"
                );
            }
            return Err(Error::Other(
                "the migration deadline expired while publishing tracks".to_string(),
            ));
        }
    };
    match published {
        Ok((video_request_id, audio_request_id, catalog_request_id)) => Ok(PublisherSession {
            client,
            data_plane,
            video_request_id,
            audio_request_id,
            catalog_request_id,
        }),
        Err(e) => {
            if let Err(close_error) = client.close(0, "").await {
                tracing::debug!(
                    "Failed to close the session after a failed publish: {close_error}"
                );
            }
            Err(e)
        }
    }
}

/// パイプラインを実行する
///
/// `config` は接続先・トランスポート・送信するトラック・入力の指定を持つ。
/// `task_monitor` は生成されるタスクのメトリクスを収集する。
/// `shutdown_monitor` は graceful shutdown の契機を受信する。
///
/// Windows で実マイクから音声を配信する場合、呼び出し側のスレッドで
/// `shiguredo_audio_device::initialize_thread` を呼び、返り値をパイプラインの終了まで
/// 保持しておく必要がある (macOS / Linux では不要)。この関数自体はスレッドの初期化を行わない。
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

    // WebTransport 経路は experimental である (s2n-quic が `RESET_STREAM_AT` を送出できず、
    // WebTransport が MUST とする Reliable Size 付きの reset を行えないため)。接続の前に 1 度だけ
    // 警告し、どのトランスポートで接続するかを実行時に分かるようにする。
    if matches!(config.transport, Transport::WtH3 | Transport::WtH2) {
        tracing::warn!(
            "WebTransport transport ({:?}) is experimental: RESET_STREAM_AT is unavailable, so stream resets may not work",
            config.transport
        );
    }

    // QMux 経路も experimental である。DATAGRAM は経路での損失を下位の TCP が再送するため
    // 起きないが、TCP の再送で遅延が伸びる。QMux は速やかに配送できない DATAGRAM の破棄を
    // 許すため (draft-ietf-quic-qmux-02 Section 6)、受信側のバッファが溢れると欠落しうる
    // (`dropped_datagrams` を警告ログに出して追えるようにしている)。
    // `RESET_STREAM_AT` を実装しないため `reliable_size` は常に `None` になる。
    if config.transport == Transport::Qmux {
        tracing::warn!(
            "QMux transport is experimental: DATAGRAM and stream resets are carried over TCP, so latency can grow when the connection is congested"
        );
    }

    // CLI で §8.8 表現としてパース済みの namespace をそのまま使う
    let namespace = config.namespace.clone();
    // カタログの namespace は §8.8 の表現で書く (draft-ietf-moq-msf-01 §5.2.2 (Track namespace))
    let namespace_text = serialize_namespace(&namespace);

    // 1. 接続確立と SETUP ハンドシェイク、2. タイムアウト設定、
    // 3. PUBLISH (video / audio / catalog)
    //
    // 名前解決、解決したアドレスのフォールバック、transport ごとの分岐は
    // tokio_moq の共通の接続経路が担う (GOAWAY 後の移行でも同じ経路を使う)。
    let PublisherSession {
        mut client,
        mut data_plane,
        mut video_request_id,
        mut audio_request_id,
        mut catalog_request_id,
    } = establish_publisher_session(&config, &config.url, &namespace, &task_monitor).await?;
    tracing::info!(
        "Session timeouts: control={:?}ms, data={:?}ms",
        client.control_message_timeout_ms(),
        client.data_stream_timeout_ms(),
    );

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
        // エンコーダへ渡すキーフレーム間隔は、時間ベースの要求が効かない場合の安全弁として
        // `--gop-duration` から逆算する
        let keyframe_interval_frames =
            gop::keyframe_interval_frames(encode_fps, config.gop_duration_ms);
        let enc = match config.video_codec {
            VideoCodec::Av1 => encoder::VideoEncoder::Av1(Box::new(encoder::av1::Av1Encoder::new(
                encode_width,
                encode_height,
                encode_fps,
                config.bitrate,
                keyframe_interval_frames,
            )?)),
            #[cfg(target_os = "macos")]
            VideoCodec::H264 => encoder::VideoEncoder::H264(encoder::h264::H264Encoder::new(
                encode_width,
                encode_height,
                encode_fps,
                config.bitrate,
                keyframe_interval_frames,
            )?),
            #[cfg(target_os = "macos")]
            VideoCodec::H265 => encoder::VideoEncoder::H265(encoder::h265::H265Encoder::new(
                encode_width,
                encode_height,
                encode_fps,
                config.bitrate,
                keyframe_interval_frames,
            )?),
            #[cfg(not(target_os = "macos"))]
            VideoCodec::H264 => {
                return Err(unsupported_macos_encoder("H.264"));
            }
            #[cfg(not(target_os = "macos"))]
            VideoCodec::H265 => {
                return Err(unsupported_macos_encoder("H.265"));
            }
            #[cfg(not(target_os = "windows"))]
            VideoCodec::Vp8 => encoder::VideoEncoder::Vp8(encoder::vpx::VpxEncoder::new(
                encoder::vpx::VpxCodec::Vp8,
                encode_width,
                encode_height,
                encode_fps,
                config.bitrate,
                keyframe_interval_frames,
            )?),
            #[cfg(not(target_os = "windows"))]
            VideoCodec::Vp9 => encoder::VideoEncoder::Vp9(encoder::vpx::VpxEncoder::new(
                encoder::vpx::VpxCodec::Vp9,
                encode_width,
                encode_height,
                encode_fps,
                config.bitrate,
                keyframe_interval_frames,
            )?),
            #[cfg(target_os = "windows")]
            VideoCodec::Vp8 => {
                return Err(unsupported_windows_encoder("VP8"));
            }
            #[cfg(target_os = "windows")]
            VideoCodec::Vp9 => {
                return Err(unsupported_windows_encoder("VP9"));
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

    let mut audio_encoder: Option<AudioEncoder> = if config.audio_enabled {
        Some(AudioEncoder::new(
            config.audio_codec,
            AUDIO_SAMPLE_RATE,
            AUDIO_CHANNELS,
            config.audio_bitrate,
        )?)
    } else {
        None
    };
    // 音声の追いつきの判定 (配信ごとに 1 つ)。符号化が実時間に追いつかなくなるとキューが
    // 溜まり、遅れが受信側の基準を押し上げるため、遅れたチャンクは符号化へ渡さずに捨てる
    let mut audio_publish_catch_up = AudioPublishCatchUp::new();
    // 直近にログへ出した、捨てたチャンクの数。状態が変わったときだけログに出す
    let mut audio_catch_up_logged_dropped_frames: u64 = 0;
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
    // codec 固有の configuration bytes (Opus は OpusHead、AAC-LC は AudioSpecificConfig) を
    // 事前に構築する。Audio Config プロパティとして最初の audio object にだけ付与する
    let audio_config = audio_encoder
        .as_ref()
        .map(|e| e.audio_config(AUDIO_SAMPLE_RATE, AUDIO_CHANNELS))
        .transpose()?;

    // 5. MSF カタログを送信する
    //
    // カタログの Group ID は 0 に固定しない。draft-ietf-moq-msf-01 §6.1 (Group numbering) は
    // Track の Group ID を一意で単調増加とする MUST を課し、publisher の再起動時には
    // 以前 publish したどの Group ID よりも大きい値から始める MUST を課す。同じ Track 名で
    // 配信し直しても Group ID が衝突しないよう、Unix epoch ミリ秒から払い出す。
    // 移行 (GOAWAY) で差し替えるため、writer の生成に使う handle も可変にする
    let mut handle = client.handle();
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
                    // MP4 パススルーは Group が MP4 のキーフレームで進み、--gop-duration を
                    // 使わないため、Group 長を宣言しない。MP4 から最大キーフレーム間隔を
                    // 算出するには VideoTrackInfo に値を持たせる必要がある (本変更の範囲外)
                    max_group_duration_ms: None,
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
                // 時間ベースの判定は入力フレームの Timestamp の差分で行うため、宣言は
                // 公称 fps で境界の 1 フレーム分を見込んだ値にする
                max_group_duration_ms: Some(gop::catalog_max_duration_ms(
                    encode_fps,
                    config.gop_duration_ms,
                )),
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
    // 直前に開始した映像 Group の ID。NEW_GROUP_REQUEST (§9.20.19) の値が未来の Group を
    // 指しているかを判定するために使う
    let mut started_video_group_id: Option<u64> = None;
    let mut audio_group_id: u64 = group_id::allocate_initial_group_id()?;
    let mut audio_frame_count: u64 = 0;
    let mut current_video_writer: Option<SubgroupWriter> = None;
    // 映像 Group の開始時刻を追跡する。キーフレームの要求はエンコーダを使う経路
    // (live capture / MP4 再エンコード) でだけ行い、MP4 パススルーでは Group 長の観測
    // (debug ログ) にだけ使う
    let mut gop_tracker = gop::GopTracker::new(config.gop_duration_ms);
    // セッション終了を観測してループを抜けたかどうか。後始末の失敗をセッション終了に伴う
    // 失敗として扱うために使う (セッション終了後の `close` は CONNECT stream の送信失敗として
    // `Error::WebTransport` に畳まれる経路があり、エラーの variant だけでは判別できない)
    let mut session_ended = false;
    let mut audio_pcm_buf: Vec<i16> = Vec::new();
    // 棄却で飛んだ境目をフェードで繋ぐために保持しているチャンク (PCM と TIMESTAMP)。
    // 捨てるかどうかは次のチャンクの判定を見るまで分からないため、符号化へ渡すチャンクを
    // 1 つ保持する (先読みの分だけ符号化が 1 フレーム遅れる)
    let mut audio_fade_pending: Option<(Vec<i16>, u64)> = None;
    // 直前にチャンクを捨てたかどうか。捨てた直後のチャンクの先頭を落とす
    let mut audio_fade_next_head = false;
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
    // Audio Config を送信済みかどうかのフラグ
    // Opus は全フレームが独立してデコード可能なため、最初の audio object だけでよい
    let mut audio_config_sent = false;
    // 直近に音声の配信の遅れの内訳を出した時刻 (開始からのミリ秒)
    let mut audio_stats_logged_ms: u64 = 0;
    // publish したメディアの保持。subscriber の FETCH に配る Object と End of Unknown Range を
    // 決めるために使う (draft-ietf-moq-transport-22 §3.2 (Fetch))。GOAWAY で session を移行しても
    // 保持は続ける (移行先の購読者も直近の Object を FETCH で取得できる)
    let mut media_cache = cache::MediaCache::new(config.cache_duration_ms);
    let mut tick_interval = tokio::time::interval(std::time::Duration::from_millis(100));
    tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // カタログの送り直し。開始直後の 1 回は送信済みなので、1 間隔ぶん待ってから始める
    let mut catalog_republish_interval = tokio::time::interval_at(
        tokio::time::Instant::now() + CATALOG_REPUBLISH_INTERVAL,
        CATALOG_REPUBLISH_INTERVAL,
    );
    catalog_republish_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // バックグラウンド移行の受信口。移行していないときは `None` であり、select の分岐は
    // [`wait_migration_result`] が永久 pending にして実質無効にする
    let mut migration_rx: Option<oneshot::Receiver<MigrationTaskResult>> = None;
    // 短期間に連続する GOAWAY 移行を制限する (移行先がまた GOAWAY を返す構成での
    // 再接続ストームを防ぐ)
    let mut migration_limiter =
        MigrationLimiter::new(MIGRATION_LIMIT_WINDOW, MAX_MIGRATIONS_PER_WINDOW);
    // WebTransport セッションの drain 状態を購読する。QUIC 直接接続と QMux では `None`
    let mut wt_session_state_rx = client.wt_session_state();

    tracing::info!(
        "Starting publish loop (video={}, audio={}, video_delivery=subgroup, audio_delivery={}, cache_duration={}ms)",
        config.video_enabled,
        config.audio_enabled,
        if config.audio_datagram {
            "datagram"
        } else {
            "subgroup"
        },
        media_cache.duration_ms(),
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
                            // 目標の Group 長を時間で決めるため、直前のキーフレームから
                            // `--gop-duration` が経過した入力フレームでキーフレームを強制する
                            let force_keyframe =
                                gop_tracker.request_keyframe_if_due(timestamp_us);
                            (
                                encoder.encode(&frame, timestamp_us, force_keyframe)?,
                                None,
                                None,
                            )
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
                            let force_keyframe = gop_tracker.request_keyframe_if_due(media_us);
                            (
                                encoder.encode(&frame, media_us, force_keyframe)?,
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
                            // Group を開始したフレームを観測し、次のキーフレーム要求の基準に
                            // する。エンコーダが時間ベースの要求ではなくフレーム数の上限で
                            // キーフレームを出した場合もここで基準が進み、その直後に時間
                            // ベースの要求が重なって 1 フレームの Group になることを防ぐ
                            let previous_duration_us = gop_tracker
                                .observe_keyframe(frame_timestamp_us(ef.timestamp, video_timescale));
                            // writer の作成と終端で同じ値を使うため、ここで request_id を
                            // 確定させる (`.expect()` の重複も避ける)
                            let video_request_id =
                                video_request_id.expect("video request_id enabled");
                            // 映像は 1 group = 1 unidirectional stream で送る
                            // (draft-ietf-moq-loc-04 §4.2)。datagram 配送は音声トラックだけに
                            // 指定でき、キーフレーム (IDR) が 1 QUIC datagram に収まらない
                            // 映像では使わない。
                            if let Some(writer) = current_video_writer.take() {
                                writer
                                .finish(client.subscription_filter_start(video_request_id))
                                .await?;
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
                            // 「現在の Group ID」は Session へ登録できた Group を記録する。
                            // writer の作成前に記録すると、直前の Group の終端 (peer の
                            // STOP_SENDING など) や writer の作成が失敗した場合に、送られて
                            // いない Group を現在の Group として NEW_GROUP_REQUEST を
                            // 誤って無視し得る
                            started_video_group_id = Some(video_group_id);
                            // 直前の Group の長さを出し、カタログへ宣言した
                            // maxGopDuration / maxGroupDuration と実機で突き合わせられるようにする
                            if let Some(previous_duration_us) = previous_duration_us {
                                tracing::debug!(
                                    "Previous video group lasted {} ms",
                                    previous_duration_us / 1_000
                                );
                            }
                            tracing::debug!("Started new video group {}", video_group_id);
                            video_group_id += 1;
                        }
                        let properties = build_video_loc_properties(&ef, video_timescale);
                        if let Some(writer) = current_video_writer.as_mut() {
                            // 保持する Location と SUBGROUP_OBJECT に載る Object ID を一致させる
                            // ため、書き込む前に writer が使う Location を取る
                            let object_location = writer.next_object_location();
                            let outcome = writer.write_object(&ef.data, &properties).await?;
                            if outcome == ObjectFilterOutcome::Pass {
                                // フィルタを通過して実際に送った Object だけを保持する。送って
                                // いない Object を FETCH で配ると購読のフィルタを迂回する
                                media_cache.insert(
                                    &namespace,
                                    config.track_name.as_bytes(),
                                    object_location,
                                    &properties,
                                    &ef.data,
                                )?;
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
                    // フェードのために符号化へ渡すチャンクを 1 つ保持している。送らないと
                    // 最後の 1 フレームが失われるため、符号化してから閉じる
                    let outcome: Result<()> = async {
                        let Some((chunk, timestamp)) = audio_fade_pending.take() else {
                            return Ok(());
                        };
                        let encoder = audio_encoder.as_mut().expect("audio encoder enabled");
                        let encoded = encoder.encode(&chunk).await?;
                        let audio_config = if audio_config_sent {
                            None
                        } else {
                            audio_config.as_deref()
                        };
                        let properties =
                            build_audio_loc_properties(timestamp, audio_timescale, audio_config);
                        if config.audio_datagram {
                            let mut writer = DatagramWriter::new(
                                &handle,
                                &data_plane,
                                audio_request_id.expect("audio request_id enabled"),
                                AUDIO_TRACK_ALIAS,
                                audio_group_id,
                                DEFAULT_PUBLISHER_PRIORITY,
                                config.datagram_max_size,
                                config.audio_redundancy,
                            );
                            let object_location = writer.next_object_location();
                            let outcome = writer.write_object(&encoded, &properties).await?;
                            if outcome == ObjectFilterOutcome::Pass {
                                audio_config_sent = true;
                                media_cache.insert(
                                    &namespace,
                                    AUDIO_TRACK_NAME.as_bytes(),
                                    object_location,
                                    &properties,
                                    &encoded,
                                )?;
                            }
                        } else {
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
                            let object_location = writer.next_object_location();
                            let outcome = writer.write_object(&encoded, &properties).await?;
                            if outcome == ObjectFilterOutcome::Pass {
                                audio_config_sent = true;
                                media_cache.insert(
                                    &namespace,
                                    AUDIO_TRACK_NAME.as_bytes(),
                                    object_location,
                                    &properties,
                                    &encoded,
                                )?;
                            }
                            writer
                                .finish(client.subscription_filter_start(audio_request_id))
                                .await?;
                        }
                        Ok(())
                    }
                    .await;
                    if let Err(e) = outcome {
                        tracing::warn!("Failed to flush the held audio chunk: {e}");
                    }
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
                    // 再エンコードでは TIMESTAMP が入力のタイムスケールであり壁時計ではない。
                    // 読み出しの遅れを測れないため、追いつきの判定をしない
                    let is_reencode = input_timestamp.is_some();
                    while audio_pcm_buf.len() >= samples_per_frame {
                        // TIMESTAMP を先に決める。読み出しの遅れは「読んだ壁時計 − TIMESTAMP」
                        // であり、判定に要る
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
                        // 読み出しの遅れと符号化のキューから、このチャンクを符号化するかを
                        // 決める。live capture の TIMESTAMP は音声の時計の補正を通した epoch
                        // マイクロ秒であるため、補正は 0 として渡す
                        let now_us = wall_clock_now_us()?;
                        let duration_us = samples_to_micros(samples_per_frame as u64, AUDIO_SAMPLE_RATE);
                        // TIMESTAMP と長さは、判定・符号化・送信の記録で同じ値を使う
                        let send_timestamp_us = i64::try_from(timestamp).unwrap_or(i64::MAX);
                        let send_duration_us = i64::try_from(duration_us).unwrap_or(i64::MAX);
                        let accepted = audio_publish_catch_up.evaluate(AudioPublishCatchUpFrame {
                            now_ms: now_us / 1_000,
                            timestamp_us: send_timestamp_us,
                            read_wall_clock_us: now_us,
                            applied_offset_us: if is_reencode { None } else { Some(0) },
                            duration_us: Some(send_duration_us),
                        });
                        // 捨てる場合もバッファからは取り出す (符号化へ渡さない)
                        let chunk: Vec<i16> = audio_pcm_buf.drain(..samples_per_frame).collect();
                        if !accepted {
                            let stats = audio_publish_catch_up.snapshot();
                            // 状態が変わったときだけログに出す (毎フレーム出すとログが埋まる)
                            if stats.dropped_frames != audio_catch_up_logged_dropped_frames {
                                audio_catch_up_logged_dropped_frames = stats.dropped_frames;
                                tracing::warn!(
                                    "Dropping audio chunks to catch up (dropped_frames={}, dropped_ms={}, lag_ms={:?}, pending_ms={}, starts={})",
                                    stats.dropped_frames,
                                    stats.dropped_ms,
                                    stats.lag_ms,
                                    stats.pending_ms,
                                    stats.catch_up_starts,
                                );
                            }
                        }
                        // 棄却で飛んだ境目をフェードで繋ぐ。保持しているチャンクを符号化する
                        // 直前に、次のチャンクの判定を見て末尾を落とす。捨てた直後のチャンクは
                        // 先頭を落とす
                        let to_encode = match (audio_fade_pending.take(), accepted) {
                            (Some((mut held, held_timestamp)), false) => {
                                fade_splice_edge_i16(
                                    &mut [held.as_mut_slice()],
                                    AUDIO_SAMPLE_RATE,
                                    AudioSpliceEdge::Tail,
                                );
                                audio_fade_next_head = true;
                                Some((held, held_timestamp))
                            }
                            (Some((held, held_timestamp)), true) => {
                                audio_fade_pending = Some((
                                    fade_chunk_head_if_dropped(chunk, &mut audio_fade_next_head),
                                    timestamp,
                                ));
                                Some((held, held_timestamp))
                            }
                            (None, false) => {
                                audio_fade_next_head = true;
                                None
                            }
                            (None, true) => {
                                audio_fade_pending = Some((
                                    fade_chunk_head_if_dropped(chunk, &mut audio_fade_next_head),
                                    timestamp,
                                ));
                                None
                            }
                        };
                        let Some((chunk, timestamp)) = to_encode else {
                            continue;
                        };
                        let encoded = encoder.encode(&chunk).await?;
                        audio_publish_catch_up.record_encoded_chunk(AudioPublishCatchUpChunk {
                            timestamp_us: i64::try_from(timestamp).unwrap_or(i64::MAX),
                            duration_us: Some(i64::try_from(duration_us).unwrap_or(i64::MAX)),
                        });
                        // 最初の audio object にだけ Audio Config を付与する。
                        // フィルタ不通過で Skip された場合は次回のオブジェクトに付与し直す
                        // (Skip 時に audio_config_sent を立てると Audio Config が永遠に届かず、
                        // フィルタを通過する後続フレームが Opus デコード不能になる)。
                        let audio_config = if audio_config_sent {
                            None
                        } else {
                            audio_config.as_deref()
                        };
                        let properties =
                            build_audio_loc_properties(timestamp, audio_timescale, audio_config);
                        // LOC draft-ietf-moq-loc-04 §4.1 (Application with one audio track): 1 audio frame = 1 Object = 1 Group
                        if config.audio_datagram {
                            // datagram の送信は完了が返らない (fire-and-forget)。書き込みの
                            // 戻りを完了とみなすため、投入の直前の時刻を控える
                            let queued_wall_clock_ms = wall_clock_now_us()? / 1_000;
                            let mut writer = DatagramWriter::new(
                                &handle,
                                &data_plane,
                                audio_request_id.expect("audio request_id enabled"),
                                AUDIO_TRACK_ALIAS,
                                audio_group_id,
                                DEFAULT_PUBLISHER_PRIORITY,
                                config.datagram_max_size,
                                config.audio_redundancy,
                            );
                            let object_location = writer.next_object_location();
                            let outcome = writer.write_object(&encoded, &properties).await?;
                            if outcome == ObjectFilterOutcome::Pass {
                                audio_config_sent = true;
                                media_cache.insert(
                                    &namespace,
                                    AUDIO_TRACK_NAME.as_bytes(),
                                    object_location,
                                    &properties,
                                    &encoded,
                                )?;
                                // 送れなかった Object (フィルタで Skip) は送信のキューへ
                                // 入っていないため記録しない。TIMESTAMP が壁時計でない
                                // (再エンコード) 音も、読んだ時刻との差が遅れにならない
                                // ため記録しない
                                if !is_reencode {
                                    record_audio_send(
                                        &mut audio_publish_catch_up,
                                        send_timestamp_us,
                                        send_duration_us,
                                        queued_wall_clock_ms,
                                        wall_clock_now_us()? / 1_000,
                                    );
                                }
                            }
                        } else {
                            // has_properties は SUBGROUP_HEADER の PROPERTIES ビットに対応し、
                            // ヘッダと全オブジェクトで一致が必須 (draft-ietf-moq-transport-22 §11.3.1 (Subgroup Header))
                            // 音声ストリームには毎フレーム LOC プロパティを付与するため true を渡す
                            let audio_request_id =
                                audio_request_id.expect("audio request_id enabled");
                            // ストリームの生成と書き込みの待ち (backpressure) を含めて測る
                            let queued_wall_clock_ms = wall_clock_now_us()? / 1_000;
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
                            let object_location = writer.next_object_location();
                            let outcome = writer.write_object(&encoded, &properties).await?;
                            if outcome == ObjectFilterOutcome::Pass {
                                audio_config_sent = true;
                                media_cache.insert(
                                    &namespace,
                                    AUDIO_TRACK_NAME.as_bytes(),
                                    object_location,
                                    &properties,
                                    &encoded,
                                )?;
                            }
                            writer
                                .finish(client.subscription_filter_start(audio_request_id))
                                .await?;
                            // 送れなかった Object (フィルタで Skip) は記録しない。TIMESTAMP が
                            // 壁時計でない (再エンコード) 音も、読んだ時刻との差が遅れに
                            // ならないため記録しない
                            if outcome == ObjectFilterOutcome::Pass && !is_reencode {
                                record_audio_send(
                                    &mut audio_publish_catch_up,
                                    send_timestamp_us,
                                    send_duration_us,
                                    queued_wall_clock_ms,
                                    wall_clock_now_us()? / 1_000,
                                );
                            }
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
                            new_session_uri,
                            timeout,
                            on_request_stream,
                        })) => {
                            tracing::info!(
                                "Received GOAWAY (timeout={timeout}, on_request_stream={on_request_stream:?})"
                            );
                            match tokio_moq::migration_target(
                                &new_session_uri,
                                on_request_stream,
                                &config.url,
                            ) {
                                // request stream 単位の GOAWAY で URI が不正な場合は session を
                                // 維持し、当該 request の終端は peer の reset に任せる
                                Ok(None) => Ok(false),
                                Ok(Some(target)) => {
                                    match goaway_action(target.scope, new_session_uri.is_empty()) {
                                        GoawayAction::RepublishRequest(request_id) => {
                                            // draft-ietf-moq-transport-22 §9.2 (GOAWAY): 空の
                                            // New Session URI は「現在の session で当該 request を
                                            // 再発行する」ことを意味する。session は維持する
                                            //
                                            // session 全体の移行が進行中なら、当該 request は移行先の
                                            // session で張り直される。drain 中の session に新しい
                                            // request を作らないよう、ここでは再発行しない
                                            // (§9.2: control stream の GOAWAY を受けた側は新しい
                                            // request を開始しないことが SHOULD)
                                            if migration_rx.is_some() {
                                                tracing::info!(
                                                    "Ignoring a request-scoped GOAWAY while a session migration is in progress: request_id={request_id}"
                                                );
                                                return Ok(false);
                                            }
                                            match republish_request_on_current_session(
                                                &mut client,
                                                &config,
                                                &namespace,
                                                request_id,
                                                &mut video_request_id,
                                                &mut audio_request_id,
                                                &mut catalog_request_id,
                                                &mut current_video_writer,
                                                &mut gop_tracker,
                                            )
                                            .await
                                            {
                                                Ok(PublishTrack::Catalog) => {
                                                    // カタログは購読を新しく張り直すため、Object を
                                                    // 次の Group で送り直す
                                                    // (draft-ietf-moq-msf-01 §5 (Catalog) /
                                                    // §6.1 (Group numbering))
                                                    if let Err(e) =
                                                        republish_catalog_at_next_group(
                                                            &client,
                                                            &handle,
                                                            &data_plane,
                                                            catalog_request_id,
                                                            &catalog_json,
                                                            &mut catalog_group_id,
                                                            &mut catalog_location,
                                                        )
                                                        .await
                                                    {
                                                        tracing::warn!(
                                                            "Failed to republish the catalog after re-issuing its PUBLISH: {e}"
                                                        );
                                                    }
                                                }
                                                Ok(PublishTrack::Audio) => {
                                                    // 新しい購読の最初の音声 Object に Audio Config を
                                                    // 載せ直す
                                                    audio_config_sent = false;
                                                }
                                                Ok(PublishTrack::Video) => {}
                                                Ok(PublishTrack::Unknown) => {}
                                                Err(e) => {
                                                    tracing::warn!(
                                                        "Failed to re-issue the request after a request-scoped GOAWAY: {e}"
                                                    );
                                                }
                                            }
                                            Ok(false)
                                        }
                                        GoawayAction::IgnoreRequestScope(request_id) => {
                                            // 移行先 URI が指定された request 単位移行は 2 session の
                                            // 併走が必要である。この example は 1 session しか持たない
                                            // ため移行せず、ログを残す (§9.2)
                                            tracing::warn!(
                                                "Ignoring a request-scoped GOAWAY with a new session URI: request_id={request_id}, migrating a single request needs two concurrent sessions"
                                            );
                                            Ok(false)
                                        }
                                        GoawayAction::MigrateSession => {
                                            if migration_rx.is_some() {
                                                // 移行の最中に 2 回目の GOAWAY が来ても二重移行しない
                                                tracing::info!(
                                                    "Ignoring a GOAWAY while a session migration is in progress"
                                                );
                                                return Ok(false);
                                            }
                                            // 移行の連鎖を制限する。拒否したら既存の終了経路へ進む
                                            if !migration_limiter.check_and_record(Instant::now()) {
                                                tracing::warn!(
                                                    "Refusing to migrate: too many session migrations in the configured window"
                                                );
                                                return Ok(true);
                                            }
                                            // 移行先 URI の msf fragment の connection が現在の接続経路と
                                            // 矛盾しないかを確認する (draft-ietf-moq-msf-01 §11.1.1)。
                                            // 矛盾する場合は移行せず、従来どおり終了する
                                            if let Err(e) = tokio_moq::resolve_transport(
                                                Some(config.transport),
                                                &target.url,
                                            ) {
                                                tracing::warn!(
                                                    "GOAWAY migration target conflicts with the current transport: {e}"
                                                );
                                                return Ok(true);
                                            }
                                            // draft-ietf-moq-transport-22 §6.6.1 (Graceful Session
                                            // Migration) / §9.2 (GOAWAY) / §7.6.1 (Graceful Publisher
                                            // Relay Switchover): 新しい session の確立と PUBLISH の
                                            // 再発行はバックグラウンドのタスクで行い、その間も旧 session
                                            // で Object を送り続ける。結果は oneshot で受け取る。
                                            // 期限は GOAWAY の Timeout (0 はローカル既定値)。
                                            // draft 由来の規則であり将来の draft 改版で変わる可能性がある
                                            let deadline = MigrationDeadline::after_goaway(
                                                Instant::now(),
                                                timeout,
                                            );
                                            tracing::info!(
                                                "Starting a background session migration to {} (timeout={timeout}ms)",
                                                target.url.authority
                                            );
                                            migration_rx = Some(spawn_migration_task(
                                                config.clone(),
                                                namespace.clone(),
                                                target.url,
                                                deadline,
                                                task_monitor.clone(),
                                            ));
                                            // 新 session の PUBLISH が成立するまで旧 session の配信を
                                            // 止めない (§7.6.1)
                                            Ok(false)
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "GOAWAY carried an unusable New Session URI; terminating without migration: {e}"
                                    );
                                    Ok(true)
                                }
                            }
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
                            .await
                        }
                        Some(ClientEvent::Request(request)) => {
                            serve_peer_request(
                                &mut client,
                                request,
                                &config,
                                CatalogState {
                                    json: &catalog_json,
                                    location: catalog_location,
                                },
                                &mut gop_tracker,
                                started_video_group_id,
                                &mut media_cache,
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
                            // NEW_GROUP_REQUEST (§9.20.19) は新しい Group の開始
                            // (ランダムアクセス点) を要求する。映像の購読に限るのは、他の
                            // トラックの要求で映像のキーフレームを強制しないためである
                            // (`parameters` は購読の累積パラメータであり、複数の更新が合体すると
                            // 以前の要求が再び現れ得る。値 0 は常に受け付けのため、その場合も
                            // 要求として扱う)
                            // 対象の購読が映像トラックかどうかは Track 名で解決する。購読は
                            // 自側の PUBLISH と peer の SUBSCRIBE の両方で確立し得るため、
                            // Request ID の空間では判定できない
                            let is_video_subscription = client
                                .subscription_track_name(update.request_id)
                                .is_some_and(|track_name| {
                                    track_name.as_slice() == config.track_name.as_bytes()
                                });
                            let new_group_request = accepted_new_group_request(
                                is_video_subscription && config.input_mp4.is_none(),
                                &update.parameters,
                                started_video_group_id,
                            );
                            match client.send_request_ok(update.request_id).await {
                                Ok(()) => {
                                    // 購読が成立している要求だけを受け付ける
                                    if let Some(next_group_id) = new_group_request {
                                        gop_tracker.request_new_group();
                                        tracing::info!(
                                            "Received NEW_GROUP_REQUEST: next_group_id={next_group_id}"
                                        );
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to send REQUEST_OK for request {}: {e}",
                                        update.request_id
                                    );
                                }
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
            migration_outcome = wait_migration_result(&mut migration_rx) => {
                match migration_outcome {
                    Some(Ok(migrated)) => {
                        // 移行完了: 旧 session から新 session へ参照先をここでまとめて差し替える。
                        // draft-ietf-moq-transport-22 §7.6.1 (Graceful Publisher Relay Switchover):
                        // 新 session で PUBLISH が成立するまで旧 session の配信を止めない。
                        // ここに来た時点で PUBLISH は成立している
                        async {
                            // 旧 session の映像 writer を終端する (§11.3.2)。終端しないと
                            // open 中の outgoing stream が残り、旧 session の PUBLISH_DONE が
                            // 拒否される (§9.9)。filter start は差し替え前の client から取る
                            if let Some(writer) = current_video_writer.take() {
                                let start_location = video_request_id.and_then(|request_id| {
                                    client.subscription_filter_start(request_id)
                                });
                                if let Err(e) = writer.finish(start_location).await {
                                    tracing::debug!(
                                        "Failed to finish the outgoing video subgroup stream during migration: {e}"
                                    );
                                }
                            }
                            let MigratedSession { url, session } = migrated;
                            let PublisherSession {
                                client: new_client,
                                data_plane: new_data_plane,
                                video_request_id: new_video_request_id,
                                audio_request_id: new_audio_request_id,
                                catalog_request_id: new_catalog_request_id,
                            } = session;
                            // 旧 session の終了 (PUBLISH_DONE と NO_ERROR の close) は main ループを
                            // 止めないよう別タスクに任せる。GOAWAY の Timeout 分は待たない
                            let old_client = std::mem::replace(&mut client, new_client);
                            let old_video_request_id =
                                std::mem::replace(&mut video_request_id, new_video_request_id);
                            let old_audio_request_id =
                                std::mem::replace(&mut audio_request_id, new_audio_request_id);
                            let old_catalog_request_id = std::mem::replace(
                                &mut catalog_request_id,
                                new_catalog_request_id,
                            );
                            // 以降の送信は新しい session の handle / data plane を使う
                            data_plane = new_data_plane;
                            handle = client.handle();
                            config.url = url;
                            wt_session_state_rx = client.wt_session_state();
                            // 新しい session の最初の音声 Object に Audio Config を載せ直す
                            // (Audio Config は最初にフィルタを通過した Object にだけ載せるため)
                            audio_config_sent = false;
                            // 新しい session では旧 Group を継続できないため、次のキーフレームで
                            // 新しい Group を開始する。エンコーダ経路は次のフレームでキーフレームを
                            // 要求し、切替の空白を短くする
                            gop_tracker.request_new_group();
                            let monitor = task_monitor.clone();
                            tokio::spawn(monitor.instrument(async move {
                                shutdown_old_session(
                                    old_client,
                                    old_video_request_id,
                                    old_audio_request_id,
                                    old_catalog_request_id,
                                )
                                .await;
                            }));
                            tracing::info!(
                                "Migrated the publisher session to {}",
                                config.url.authority
                            );
                            // 新しい session でカタログを次の Group で送り直す
                            // (draft-ietf-moq-msf-01 §5 (Catalog) / §6.1 (Group numbering))
                            if let Err(e) = republish_catalog_at_next_group(
                                &client,
                                &handle,
                                &data_plane,
                                catalog_request_id,
                                &catalog_json,
                                &mut catalog_group_id,
                                &mut catalog_location,
                            )
                            .await
                            {
                                tracing::warn!(
                                    "Failed to republish the catalog on the new session: {e}"
                                );
                            }
                        }
                        .await;
                        migration_rx = None;
                    }
                    Some(Err(reason)) => {
                        tracing::warn!("Session migration failed: {reason}");
                        // 移行を諦め、既存の終了経路 (後始末) へ進む
                        session_ended = true;
                        break 'main;
                    }
                    None => {}
                }
            }
            wt_state = wait_wt_session_change(&mut wt_session_state_rx) => {
                match wt_state {
                    // 通常の利用状態。次の変化を待つ
                    Some(WtSessionState::Active) => {}
                    Some(WtSessionState::Draining) => {
                        // draft-ietf-webtrans-http3-16 §4.7 / draft-ietf-webtrans-http2-15 §6.13:
                        // drain は「できるだけ早く終了する」合図である。MOQT の GOAWAY を待たずに
                        // 現在の URL を移行先として移行を開始する。GOAWAY の Timeout が無いため
                        // 期限はローカル既定値 (Timeout 0 と同じ扱い) を使う
                        if migration_rx.is_some() {
                            tracing::info!(
                                "Observed a WebTransport drain while a session migration is in progress"
                            );
                        } else if !migration_limiter.check_and_record(Instant::now()) {
                            // 移行の連鎖を制限する。拒否したら既存の終了経路へ進む
                            tracing::warn!(
                                "Refusing to migrate on a WebTransport drain: too many session migrations in the configured window"
                            );
                            session_ended = true;
                            break 'main;
                        } else {
                            let target_url = config.url.clone();
                            let deadline = MigrationDeadline::after_goaway(Instant::now(), 0);
                            tracing::info!(
                                "Starting a background session migration on a WebTransport drain to {}",
                                target_url.authority
                            );
                            migration_rx = Some(spawn_migration_task(
                                config.clone(),
                                namespace.clone(),
                                target_url,
                                deadline,
                                task_monitor.clone(),
                            ));
                        }
                    }
                    Some(
                        WtSessionState::ClosedByPeer
                        | WtSessionState::ClosedLocally
                        | WtSessionState::ConnectionFailed,
                    ) => {
                        // セッションの終了は data plane の送信失敗や `next_event` のセッション
                        // 終了として通常の終了経路で処理する
                        tracing::debug!(
                            "Observed a terminal WebTransport session state: {wt_state:?}"
                        );
                    }
                    None => {
                        // 送信側が消えた (transport の終了)。通常の終了経路に任せる。
                        // `changed()` は送信側の消滅後は即座に失敗するため、分岐を無効化して
                        // select が空回りしないようにする
                        wt_session_state_rx = None;
                    }
                }
            }
            _ = tick_interval.tick() => {
                let now_ms = Instant::now().duration_since(start).as_millis() as u64;
                client.tick_now();
                // 音声の遅れがどの段で溜まっているかを定期的に 1 行で出す。遅れが伸びている
                // 途中でも読めるようにするため、チャンクを捨てたときのログとは別に出す
                if config.audio_enabled
                    && now_ms.saturating_sub(audio_stats_logged_ms) >= AUDIO_STATS_INTERVAL_MS
                {
                    audio_stats_logged_ms = now_ms;
                    tracing::info!(
                        "{}",
                        format_audio_publish_stats(&audio_publish_catch_up.snapshot())
                    );
                }
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
                let outcome = republish_catalog_at_next_group(
                    &client,
                    &handle,
                    &data_plane,
                    catalog_request_id,
                    &catalog_json,
                    &mut catalog_group_id,
                    &mut catalog_location,
                )
                .await;
                match outcome {
                    Ok(_) => {}
                    Err(e) if is_transport_session_end(&e) => {
                        tracing::info!("Session closed by transport");
                        session_ended = true;
                        break 'main;
                    }
                    Err(e) if is_peer_stream_reset(&e) => {
                        // peer がカタログの Subgroup stream を cancel した。送信前に採番は
                        // 進めてあるため、同じ Location を再送せず次の間隔で送り直す
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
        // 省略があれば RESET 側に倒れる (安全側)。
        // 失敗は `?` で `run` まで通さない。セッション終了・接続クローズ・peer の cancel に
        // 伴う失敗を致命エラーにすると、正常な終了経路でも `Fatal:` と exit(1) になって後始末が
        // 途中で切れる (期待する失敗の範囲をループ内の分類と同じにする。ループ内は想定外の
        // エラーを致命にするが、後始末は warn に落として続ける点が異なる)。
        // なお `finish` が失敗した経路では [`DataPlaneHandle::send_data_stream_closed`] が
        // 呼ばれないため Session の outgoing stream 追跡が残り、直後の映像の PUBLISH_DONE は
        // publish_done requires all outgoing streams closed で拒否される
        // (draft-ietf-moq-transport-22 §9.9 の MUST NOT)。セッション終了を観測していない経路
        // (peer の cancel 後に Ctrl+C / 入力終了でループを抜けた場合) では warn になる。
        // 追跡の終端は cancel の観測と終端通知の到着順に依存するため、本 example では扱わない
        if let Err(e) = writer
            .finish(video_request_id.and_then(|rid| client.subscription_filter_start(rid)))
            .await
        {
            log_failure(
                format_args!("Failed to finish video subgroup stream: {e}"),
                cleanup_finish_is_expected(session_ended, &e),
            );
        }
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

/// GOAWAY ごとに publisher が取る動作
///
/// draft-ietf-moq-transport-22 §6.6.1 (Graceful Session Migration) / §9.2 (GOAWAY) の
/// 適用範囲と New Session URI から決まる。draft 由来の規則であり将来の draft 改版で
/// 変わる可能性がある。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoawayAction {
    /// control stream の GOAWAY。session 全体をバックグラウンドで移行する
    MigrateSession,
    /// request stream の GOAWAY (空 URI)。当該 PUBLISH を現在の session で再発行する
    RepublishRequest(u64),
    /// request stream の GOAWAY だが New Session URI が指定されている
    ///
    /// 1 つの request だけを別 session へ移すには 2 session の併走が必要であり、
    /// この example は 1 session しか持たないため移行しない。
    IgnoreRequestScope(u64),
}

/// GOAWAY の適用範囲と New Session URI から publisher の動作を決める
///
/// draft-ietf-moq-transport-22 §9.2 (GOAWAY): "Upon receiving a GOAWAY on a request stream,
/// the endpoint SHOULD re-issue that specific request on a session at the specified URI (or
/// the current session if no URI is provided), and close the old request stream using the
/// appropriate mechanism"。URI が空なら現在の session で再発行し、非空なら別 session が要る。
/// draft 由来の規則であり将来の draft 改版で変わる可能性がある。
fn goaway_action(scope: MigrationScope, new_session_uri_is_empty: bool) -> GoawayAction {
    match scope {
        MigrationScope::Session => GoawayAction::MigrateSession,
        MigrationScope::Request(request_id) => {
            if new_session_uri_is_empty {
                GoawayAction::RepublishRequest(request_id)
            } else {
                GoawayAction::IgnoreRequestScope(request_id)
            }
        }
    }
}

/// publisher が PUBLISH している track
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishTrack {
    /// 映像トラック
    Video,
    /// 音声トラック
    Audio,
    /// MSF カタログトラック
    Catalog,
    /// この publisher の PUBLISH ではない (配信していない track など)
    Unknown,
}

/// request id がどの PUBLISH に対応するかを返す
///
/// request stream の GOAWAY は送信済みの PUBLISH の request id で通知されるため、
/// 再発行する track をこの対応で決める。request id は session 内で一意であり、
/// 映像 / 音声 / カタログの PUBLISH が同じ id を持つことはない。
fn publish_track_for_request(
    request_id: u64,
    video_request_id: Option<u64>,
    audio_request_id: Option<u64>,
    catalog_request_id: u64,
) -> PublishTrack {
    if video_request_id == Some(request_id) {
        PublishTrack::Video
    } else if audio_request_id == Some(request_id) {
        PublishTrack::Audio
    } else if catalog_request_id == request_id {
        PublishTrack::Catalog
    } else {
        PublishTrack::Unknown
    }
}

/// 移行タスクが確立した新しい session
struct MigratedSession {
    /// 移行先として使った URL (切替時に `config.url` へ反映する)
    url: ServerUrl,
    /// 確立済みの publisher session
    session: PublisherSession,
}

/// バックグラウンド移行タスクの結果
///
/// 失敗の理由は main ループがログに出すだけであるため、`Error` を task 境界へ持ち出さず
/// 表示文字列で運ぶ (oneshot で送る型を `Send` に保つ)。
type MigrationTaskResult = std::result::Result<MigratedSession, String>;

/// バックグラウンドで publisher session を移行する
///
/// draft-ietf-moq-transport-22 §6.6.1 (Graceful Session Migration) / §7.6.1 (Graceful
/// Publisher Relay Switchover): 新しい session の確立と PUBLISH の再発行はバックグラウンドで
/// 行い、その間も呼び出し側は旧 session で Object を送り続ける。期限 (GOAWAY の Timeout) を
/// 過ぎたら移行を打ち切る。draft 由来の規則であり将来の draft 改版で変わる可能性がある。
async fn migrate_publisher_session(
    config: Config,
    namespace: TrackNamespace,
    target_url: ServerUrl,
    deadline: MigrationDeadline,
    task_monitor: tokio_metrics::TaskMonitor,
) -> MigrationTaskResult {
    match establish_publisher_session_with_retry(
        &config,
        &target_url,
        &namespace,
        &task_monitor,
        deadline,
    )
    .await
    {
        Ok(session) => {
            tracing::info!(
                "Established the new session for migration: authority={}",
                target_url.authority
            );
            Ok(MigratedSession {
                url: target_url,
                session,
            })
        }
        Err(e) => {
            tracing::warn!("Failed to establish the new session for migration: {e}");
            Err(e.to_string())
        }
    }
}

/// 移行タスクを起動し、結果を受け取る oneshot を返す
///
/// 移行タスクは新しい session を確立して PUBLISH を張り、結果を oneshot で main ループへ返す。
/// main ループは結果を受け取るまで旧 session で配信を続ける (§7.6.1)。
fn spawn_migration_task(
    config: Config,
    namespace: TrackNamespace,
    target_url: ServerUrl,
    deadline: MigrationDeadline,
    task_monitor: tokio_metrics::TaskMonitor,
) -> oneshot::Receiver<MigrationTaskResult> {
    let (tx, rx) = oneshot::channel();
    // 移行タスクのメトリクスも同じモニタで集計する
    let monitor = task_monitor.clone();
    tokio::spawn(monitor.instrument(async move {
        let result =
            migrate_publisher_session(config, namespace, target_url, deadline, task_monitor).await;
        // main ループが既に終了している場合は結果を破棄する
        let _ = tx.send(result);
    }));
    rx
}

/// 移行後の旧 session を終了する
///
/// draft-ietf-moq-transport-22 §9.2 (GOAWAY) / §6.6.1 (Graceful Session Migration):
/// 旧 session の PUBLISH に PUBLISH_DONE を送り、open 中の outgoing stream が無い状態で
/// NO_ERROR で閉じる。呼び出し側はこの関数を呼ぶ前に映像の subgroup stream を終端している。
/// GOAWAY の Timeout 分は待たない (移行に要した時間は GOAWAY の期限 (絶対時刻) から既に
/// 差し引かれている)。close は main ループを止めないよう別タスクで行う。
async fn shutdown_old_session(
    mut client: MoqtClient,
    video_request_id: Option<u64>,
    audio_request_id: Option<u64>,
    catalog_request_id: u64,
) {
    // PUBLISH した track を PUBLISH_DONE で終端する。映像の subgroup stream は呼び出し側が
    // 終端済みであり、全 outgoing stream が閉じているため PUBLISH_DONE を送れる (§9.9)
    for (track, request_id) in [
        ("video", video_request_id),
        ("audio", audio_request_id),
        ("catalog", Some(catalog_request_id)),
    ] {
        let Some(request_id) = request_id else {
            continue;
        };
        if let Err(e) = client
            .send_publish_done(request_id, PUBLISH_DONE_GOING_AWAY, "")
            .await
        {
            tracing::debug!(
                "Failed to send PUBLISH_DONE for the {track} track on the old session: {e}"
            );
        }
    }
    // draft-ietf-moq-transport-22 §6.6.1: 新しい session へ移行したため旧 session は
    // NO_ERROR で閉じる。GOAWAY を送った peer もこの close で drain を終えられる
    if let Err(e) = client.close(0, "").await {
        tracing::debug!("Failed to close the old session after migration: {e}");
    }
}

/// バックグラウンド移行タスクの完了を待つ
///
/// 移行していないとき (`None`) は永久に pending にして select の分岐を実質無効にする。
async fn wait_migration_result(
    receiver: &mut Option<oneshot::Receiver<MigrationTaskResult>>,
) -> Option<MigrationTaskResult> {
    let Some(receiver) = receiver.as_mut() else {
        std::future::pending::<()>().await;
        return None;
    };
    // 送信側が結果を送らずに終了した (移行タスクのパニックなど) 場合も失敗として扱う
    Some(match receiver.await {
        Ok(result) => result,
        Err(e) => Err(e.to_string()),
    })
}

/// WebTransport セッション状態の変化を待つ
///
/// `None` (QUIC 直接接続 / QMux) は drain の通知が無いため永久に pending にして分岐を
/// 実質無効にする。送信側が消えた場合も `None` を返し、呼び出し側は通常の終了経路に任せる。
async fn wait_wt_session_change(
    receiver: &mut Option<watch::Receiver<WtSessionState>>,
) -> Option<WtSessionState> {
    let Some(receiver) = receiver.as_mut() else {
        std::future::pending::<()>().await;
        return None;
    };
    match receiver.changed().await {
        Ok(()) => Some(*receiver.borrow_and_update()),
        Err(_) => None,
    }
}

/// カタログを指定 Group で送り直す
///
/// draft-ietf-moq-msf-01 §5 (Catalog): 独立したカタログは新しい Group の先頭 (Object 0) に
/// 置く。JSON は起動時に組み立てたものを再利用する (MAX_CACHE_DURATION などの告知値は
/// 接続先が変わっても同じである)。
async fn republish_catalog_at_group(
    client: &MoqtClient,
    handle: &StreamHandle,
    data_plane: &DataPlaneHandle,
    catalog_request_id: u64,
    catalog_group: u64,
    catalog_json: &[u8],
) -> Result<ObjectFilterOutcome> {
    catalog::publish_catalog_object(
        handle,
        data_plane,
        catalog_request_id,
        CATALOG_TRACK_ALIAS,
        catalog_group,
        catalog_json,
        client.subscription_filter_start(catalog_request_id),
    )
    .await
}

/// カタログを次の Group で送り直し、送信できた場合は現在の Location を更新する
///
/// Group ID は送信の成否によらず先に進める。同じ Location を 2 度送ると購読側で重複として
/// 扱われ、失敗した Group を再送すると同じ Group の Object が二重に登録されうるためである
/// (draft-ietf-moq-msf-01 §6.1 (Group numbering))。
async fn republish_catalog_at_next_group(
    client: &MoqtClient,
    handle: &StreamHandle,
    data_plane: &DataPlaneHandle,
    catalog_request_id: u64,
    catalog_json: &[u8],
    catalog_group_id: &mut u64,
    catalog_location: &mut Location,
) -> Result<ObjectFilterOutcome> {
    let next_group_id = group_id::next_group_id(*catalog_group_id);
    *catalog_group_id = next_group_id;
    let published = republish_catalog_at_group(
        client,
        handle,
        data_plane,
        catalog_request_id,
        next_group_id,
        catalog_json,
    )
    .await?;
    if published == ObjectFilterOutcome::Pass {
        *catalog_location = Location {
            group_id: next_group_id,
            object_id: 0,
        };
        tracing::debug!("Republished the MSF catalog at group {next_group_id}");
    } else {
        // どの購読にも届かなかった (Forward State 0 など)。Object は送られていないため、
        // FETCH 応答と LARGEST_OBJECT が指す Location は前回のままにする
        tracing::debug!("Catalog republish at group {next_group_id} was filtered out");
    }
    Ok(published)
}

/// 旧 request を PUBLISH_DONE で終端する (失敗はログのみ)
///
/// draft-ietf-moq-transport-22 §9.2 (GOAWAY): request stream の GOAWAY を受けたら当該 request を
/// 再発行し、旧 request stream を "FIN, stream reset, or PUBLISH_DONE" のいずれかで閉じる。
/// GOAWAY を送った peer は timeout 後に当該 stream を reset するため、ここでの失敗は致命では
/// ない。draft 由来の規則であり将来の draft 改版で変わる可能性がある。
async fn send_publish_done_best_effort(
    client: &mut MoqtClient,
    request_id: u64,
    track: &'static str,
) {
    if let Err(e) = client
        .send_publish_done(request_id, PUBLISH_DONE_GOING_AWAY, "")
        .await
    {
        tracing::debug!(
            "Failed to send PUBLISH_DONE for the {track} request {request_id} on a request-scoped GOAWAY: {e}"
        );
    }
}

/// request stream の GOAWAY で当該 PUBLISH を現在の session で再発行する
///
/// draft-ietf-moq-transport-22 §9.2 (GOAWAY): request stream の GOAWAY は、指定 URI (空なら
/// 現在の session) で当該 request を再発行することを求める。ここでは空 URI の場合だけを扱い、
/// request id を新しい PUBLISH のものへ差し替える。Track Alias は track ごとに固定であり、
/// 同じ track の再 PUBLISH では同じ alias を使い続ける (§3.1.3 (Track Alias))。旧 request は
/// PUBLISH_DONE で終端する。draft 由来の規則であり将来の draft 改版で変わる可能性がある。
///
/// 戻り値は再発行した track である。カタログは再発行後に Object を送り直す必要があり、
/// 映像は新しい Group を開始する必要があるため、呼び出し側が戻り値を見て続きを行う。
#[expect(
    clippy::too_many_arguments,
    reason = "main ループが持つ PUBLISH ごとの状態をまとめて更新するため"
)]
async fn republish_request_on_current_session(
    client: &mut MoqtClient,
    config: &Config,
    namespace: &TrackNamespace,
    request_id: u64,
    video_request_id: &mut Option<u64>,
    audio_request_id: &mut Option<u64>,
    catalog_request_id: &mut u64,
    current_video_writer: &mut Option<SubgroupWriter>,
    gop_tracker: &mut gop::GopTracker,
) -> Result<PublishTrack> {
    let track = publish_track_for_request(
        request_id,
        *video_request_id,
        *audio_request_id,
        *catalog_request_id,
    );
    match track {
        PublishTrack::Video => {
            // 新しい PUBLISH の確立を待ってから旧 request を終端する。その間も旧 request の
            // 購読には届き続ける (§7.6.1 の publisher switchover)
            let new_request_id = publish_video_track(client, config, namespace).await?;
            // 旧 request の subgroup stream を先に閉じる。open 中の stream が残っていると
            // PUBLISH_DONE が拒否される (§9.9)
            if let Some(writer) = current_video_writer.take() {
                let start_location =
                    (*video_request_id).and_then(|rid| client.subscription_filter_start(rid));
                if let Err(e) = writer.finish(start_location).await {
                    tracing::debug!(
                        "Failed to finish the outgoing video subgroup stream after a request-scoped GOAWAY: {e}"
                    );
                }
            }
            let previous_request_id = video_request_id.replace(new_request_id);
            // 新しい request では次のキーフレームから新しい Group を開始する。エンコーダ経路は
            // 次のフレームでキーフレームを要求し、切替の空白を 1 GOP より短くする
            gop_tracker.request_new_group();
            if let Some(previous_request_id) = previous_request_id {
                send_publish_done_best_effort(client, previous_request_id, "video").await;
            }
        }
        PublishTrack::Audio => {
            let new_request_id = publish_audio_track(client, config, namespace).await?;
            let previous_request_id = audio_request_id.replace(new_request_id);
            if let Some(previous_request_id) = previous_request_id {
                send_publish_done_best_effort(client, previous_request_id, "audio").await;
            }
        }
        PublishTrack::Catalog => {
            let new_request_id = publish_catalog_track(client, namespace).await?;
            let previous_request_id = std::mem::replace(catalog_request_id, new_request_id);
            send_publish_done_best_effort(client, previous_request_id, "catalog").await;
        }
        PublishTrack::Unknown => {
            tracing::warn!(
                "Received a request-scoped GOAWAY for a request that is not a publisher request: request_id={request_id}"
            );
        }
    }
    Ok(track)
}

/// macOS 以外で H.264 / H.265 のエンコーダを指定したときのエラーを作る
#[cfg(not(target_os = "macos"))]
fn unsupported_macos_encoder(codec: &str) -> Error {
    Error::Other(format!(
        "{codec} encoder is only available on macOS; rebuild on macOS or specify --video-codec av1"
    ))
}

/// Windows で VP8 / VP9 のエンコーダを指定したときのエラーを作る
///
/// libvpx の Windows ビルド (MSVC) は動作しないため、Windows では VP8 / VP9 を
/// エンコードできない。
#[cfg(target_os = "windows")]
fn unsupported_windows_encoder(codec: &str) -> Error {
    Error::Other(format!(
        "{codec} encoder is not available on Windows; specify --video-codec av1"
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
/// WebTransport over HTTP/2 も driver が受領した `WT_STOP_SENDING` のエラーコードを
/// `StreamReset` として返すため本判定の対象になる (draft-ietf-webtrans-http2-15 §6.3)。
/// WebTransport over HTTP/3 の送信 API も同じ `StreamReset` を返す (コードは HTTP/3 の
/// code space のままで、MOQT のコードへ写すのは観測経路だけである)。
/// 本判定は variant のみを見るため、コードの空間に依存しない。
fn is_peer_stream_reset(error: &Error) -> bool {
    matches!(error, Error::StreamReset { .. })
}

/// 後始末での `finish` の失敗を `info!` で出すべきかどうか
///
/// 期待する失敗の範囲をループ内の分類と同じにする。セッション終了を観測していれば
/// (`session_ended`) どの失敗も期待される。観測していなくても transport によるセッション終了
/// (`Error::ConnectionClosed`) と peer による該当ストリームの cancel (`Error::StreamReset`) は
/// 期待される失敗である。cancel は peer の STOP_SENDING を観測した場合に限られるため
/// `StreamReset` だけが対象になる (draft-ietf-moq-transport-22 §6.4.2.3)。
/// ループ内は想定外のエラーを致命エラーにして `run` を終わらせるが、後始末はそれらを warn に
/// 落として続ける点が異なる。
///
/// peer の cancel でも、driver が台帳に peer のコードを持たないままストリームを閉じている
/// 場合の拒否は `TransportError::StreamClosed` になる。これは [`Error`] に variant が無く
/// `Error::WebTransport` に文字列として畳まれるため、`session_ended` が偽のときは本判定では
/// 拾えず warn のままになる (peer の cancel ではないので期待される失敗に数えない)。
fn cleanup_finish_is_expected(session_ended: bool, error: &Error) -> bool {
    session_ended || is_transport_session_end(error) || is_peer_stream_reset(error)
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

/// 現在 publish しているカタログの状態
///
/// カタログの JSON と、それが publish されている Location は常に組で扱う (FETCH 応答には
/// 「どの Location の Object を返すか」と「その中身」の両方が要る)。
#[derive(Debug, Clone, Copy)]
struct CatalogState<'a> {
    /// 最後に publish した MSF カタログの JSON
    json: &'a [u8],
    /// そのカタログ Object の Location
    location: Location,
}

/// MOQT relay が転送してきた要求 (SUBSCRIBE / FETCH) に応答する
///
/// relay は subscriber の SUBSCRIBE / FETCH を publisher 側 session の新しい
/// request stream として転送する (draft-ietf-moq-transport-22 §6.3 (Session initialization))。
/// 本 publisher は video / audio / catalog の SUBSCRIBE に SUBSCRIBE_OK を返し
/// (以後の Object は継続して送信中の subgroup stream で届く)、catalog とメディア (映像 /
/// 音声) の FETCH には要求された range を判定したうえで FETCH_OK と FETCH 応答ストリームを返す。
/// 未知の track と未対応の要求種別は REQUEST_ERROR で拒否する
/// (draft-ietf-moq-transport-22 §9.4 (REQUEST_ERROR))。
///
/// `gop_tracker` と `started_video_group_id` は、SUBSCRIBE に載った `NEW_GROUP_REQUEST`
/// (draft-ietf-moq-transport-22 §9.20.19) を映像のキーフレーム要求へ配線するために使う。
/// `media_cache` は publish 済みメディアの保持であり、メディアの FETCH に配る Object と
/// End of Unknown Range を決めるために使う。
async fn serve_peer_request(
    client: &mut MoqtClient,
    request: IncomingRequest,
    config: &Config,
    catalog: CatalogState<'_>,
    gop_tracker: &mut gop::GopTracker,
    started_video_group_id: Option<u64>,
    media_cache: &mut cache::MediaCache,
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
                    // 購読者は DYNAMIC_GROUPS を知らずに SUBSCRIBE へ NEW_GROUP_REQUEST を
                    // 載せられる (§9.20.19)。Group の途中で参加した購読者が新しい Group を
                    // 要求する経路であり、映像トラックのときだけ受け付ける。MP4 パススルーは
                    // キーフレームを要求できないため受け付けない
                    let new_group_request = accepted_new_group_request(
                        track_alias == VIDEO_TRACK_ALIAS && config.input_mp4.is_none(),
                        &subscribe.parameters,
                        started_video_group_id,
                    );
                    // 映像トラックの SUBSCRIBE_OK にも PUBLISH と同じ DYNAMIC_GROUPS を載せる。
                    // 購読者はこの値で publisher が動的 Group に対応しているかを知り
                    // (draft-ietf-moq-transport-22 §3.5.1)、REQUEST_UPDATE で
                    // NEW_GROUP_REQUEST を送れるようになる (§9.20.19 の MUST NOT)
                    let subscribe_properties = subscribe_ok_properties(
                        track_alias,
                        config.input_mp4.is_some(),
                        config.cache_duration_ms,
                    );
                    client
                        .send_subscribe_ok(
                            request_id,
                            track_alias,
                            MessageParameters::new(),
                            subscribe_properties,
                        )
                        .await?;
                    tracing::info!(
                        "Accepted SUBSCRIBE (request_id={request_id}, track={}, alias={track_alias})",
                        String::from_utf8_lossy(track),
                    );
                    // 購読が成立している要求だけを受け付ける
                    if let Some(next_group_id) = new_group_request {
                        gop_tracker.request_new_group();
                        tracing::info!(
                            "Received NEW_GROUP_REQUEST in SUBSCRIBE: next_group_id={next_group_id}"
                        );
                    }
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
                match catalog::catalog_fetch_response(filter.as_ref(), catalog.location) {
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
                                catalog.location.group_id,
                                catalog.location.object_id,
                                catalog.json,
                                None,
                            )
                            .await?;
                        tracing::info!(
                            "Served catalog FETCH (request_id={request_id}, group={})",
                            catalog.location.group_id,
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
                serve_media_fetch(client, request_id, &fetch, config, media_cache).await?;
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

/// FETCH の LOCATION_FILTER から要求 range を決める
///
/// draft-ietf-moq-transport-22 §3.2 (Fetch) / §3.3.1 (Location Filters) / §9.20.9 (LOCATION
/// FILTER Parameter): Start の省略時は track の先頭 `{0, 0}`、End の省略時は Largest Object
/// である。要求が Largest Object より先を指しても publisher が持つのは Largest Object まで
/// であるため、`end` は Largest Object でクランプする。
///
/// Largest Object が無い (まだ 1 件も publish していない) track でも、境界の解決を Session と
/// 揃えて先頭側に解決する。この場合に配れる Object は無く、応答の判断は対象外の track と
/// 同じになる。
fn fetch_request_range(
    filter: Option<&LocationFilter>,
    largest: Option<Location>,
) -> (Location, Location) {
    let start = filter
        .and_then(|filter| filter.effective_start_location(largest.as_ref()))
        .unwrap_or(Location {
            group_id: 0,
            object_id: 0,
        });
    let end = filter
        .and_then(|filter| {
            filter.effective_end_location(largest.as_ref(), LocationFilterContext::Fetch)
        })
        .or(largest)
        .unwrap_or(start);
    let end = match largest {
        Some(largest) => end.min(largest),
        None => end,
    };
    (start, end)
}

/// メディア (映像 / 音声) の FETCH に応答する
///
/// 配る Object と End of Unknown Range は [`cache::MediaCache::plan`] が決める。ここでは要求
/// range の解決と、FETCH_OK と 1 本の FETCH 応答ストリームの書き出しを行う
/// (draft-ietf-moq-transport-22 §3.2 (Fetch) / §11.4 (Fetch Streams))。
/// 応答は「End of Unknown Range (必要なら) → Object → (全滅なら End of Unknown Range)」を
/// 順に書き、FIN で閉じる。
async fn serve_media_fetch(
    client: &mut MoqtClient,
    request_id: u64,
    fetch: &Fetch,
    config: &Config,
    media_cache: &mut cache::MediaCache,
) -> Result<()> {
    let track_name = fetch.track_name.as_slice();
    // draft-ietf-moq-transport-22 §3.3.3 (Range Filters) は publisher に「すべてのフィルタを
    // 通過した Object だけを送る」MUST を課す。この example はメディアの保持にフィルタ評価を
    // 持たないため、フィルタ付きの FETCH は昇順の保持範囲を返さず拒否する (§1.6 の
    // 「未対応のメッセージには NOT_SUPPORTED で応答する」に従う)
    if fetch.parameters.has_range_filters() {
        tracing::warn!(
            "Rejecting media FETCH with a range filter (request_id={request_id}, track={}): filters are not supported",
            fetch.track_name.len(),
        );
        client
            .send_request_error(
                request_id,
                REQUEST_NOT_SUPPORTED,
                "range filters are not supported",
            )
            .await?;
        return Ok(());
    }
    // この example が PUBLISH しているメディア track かどうか。catalog は呼び出し元が扱う
    let published = (config.video_enabled && track_name == config.track_name.as_bytes())
        || (config.audio_enabled && track_name == AUDIO_TRACK_NAME.as_bytes());
    // 要求された Location Filter から range を解決する。wire 経路では decode 層が不正な
    // 符号化を弾くため、ここでの失敗はアプリが手組みしたメッセージだけが到達する
    let filter = match fetch.parameters.location_filter_typed() {
        Ok(filter) => filter,
        Err(e) => {
            tracing::warn!(
                "Rejecting media FETCH with invalid filter (request_id={request_id}): {e}"
            );
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
    let (start, end) = fetch_request_range(
        filter.as_ref(),
        media_cache.largest(&config.namespace, track_name),
    );
    let plan = media_cache.plan(
        published,
        &config.namespace,
        track_name,
        fetch.parameters.group_order(),
        start,
        end,
    );
    let entries = plan.entries();
    match &plan {
        cache::FetchPlan::NoPublishedObjects => {
            // draft-ietf-moq-transport-22 §3.2 は「Object が publish されていない track への
            // FETCH」に INVALID_RANGE を返す MUST を課す
            tracing::warn!(
                "Rejecting FETCH for a track without published objects: {}",
                String::from_utf8_lossy(track_name),
            );
            client
                .send_request_error(
                    request_id,
                    REQUEST_INVALID_RANGE,
                    "no objects have been published for the track",
                )
                .await?;
        }
        cache::FetchPlan::EmptyRange => {
            // 要求 range に存在する Object が無い。FETCH_OK を返し、FETCH_HEADER だけの
            // stream を FIN で閉じる (draft-ietf-moq-transport-22 §3.2.1)
            client
                .send_fetch_ok(
                    request_id,
                    0,
                    end,
                    MessageParameters::new(),
                    TrackProperties::new(),
                )
                .await?;
            let writer = client.open_fetch_response(request_id).await?;
            writer.finish().await?;
            tracing::info!(
                "Served media FETCH with an empty range (request_id={request_id}, track={}, end={end:?})",
                String::from_utf8_lossy(track_name),
            );
        }
        cache::FetchPlan::UnknownTrack => {
            tracing::warn!(
                "Rejecting FETCH for unknown track: {}",
                String::from_utf8_lossy(track_name),
            );
            client
                .send_request_error(request_id, REQUEST_DOES_NOT_EXIST, "unknown track")
                .await?;
        }
        cache::FetchPlan::UnsupportedGroupOrder => {
            // draft-ietf-moq-transport-22 §3.2.1 (Fetch) は要求された group order で送る MUST を
            // 課す。降順で返す実装を持たないため、昇順で返さず拒否する
            tracing::warn!(
                "Rejecting FETCH with a descending group order (request_id={request_id}, track={})",
                String::from_utf8_lossy(track_name),
            );
            client
                .send_request_error(
                    request_id,
                    REQUEST_NOT_SUPPORTED,
                    "descending group order is not supported",
                )
                .await?;
        }
        cache::FetchPlan::AllUnknown { end_location } => {
            // 要求 range 全体が不明である。FETCH_OK の End Location に置く End of Unknown
            // Range を 1 件だけ書く (draft-ietf-moq-transport-22 §11.4.1.2 (End of Range))
            serve_fetch_entries(client, request_id, track_name, *end_location, &entries).await?;
        }
        cache::FetchPlan::Serve { .. } => {
            // FETCH_OK の End Location は要求 range の終端 (Largest Object でクランプ済み)
            serve_fetch_entries(client, request_id, track_name, end, &entries).await?;
        }
    }
    Ok(())
}

/// FETCH_OK を返し、entry を 1 本の応答 stream に書いて FIN する
///
/// entry の並びは [`cache::FetchPlan::entries`] が決める。End of Range は位置に意味を持つ
/// ため、並びの決定と書き出しを分けてある (並びは単体テストで固定する)。
async fn serve_fetch_entries(
    client: &mut MoqtClient,
    request_id: u64,
    track_name: &[u8],
    end_location: Location,
    entries: &[cache::FetchResponseEntry<'_>],
) -> Result<()> {
    client
        .send_fetch_ok(
            request_id,
            0,
            end_location,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .await?;
    let mut writer = client.open_fetch_response(request_id).await?;
    for entry in entries {
        match entry {
            cache::FetchResponseEntry::NonExistent(location) => {
                writer.write_end_of_non_existent_range(*location).await?;
            }
            cache::FetchResponseEntry::Unknown(location) => {
                writer.write_end_of_unknown_range(*location).await?;
            }
            cache::FetchResponseEntry::Object(object) => {
                // Publisher Priority は元の配送と同じ固定値にする。FETCH 応答では同じ Subgroup の
                // Priority を変えられない (draft-ietf-moq-transport-22 §12.1 (Malformed Tracks))。
                // 元の配送が datagram でも、応答は 1 本の stream に載せるため Subgroup ID 0 の
                // Object として書く
                let input = FetchObjectInput {
                    group_id: object.location.group_id,
                    subgroup_id: 0,
                    object_id: object.location.object_id,
                    publisher_priority: DEFAULT_PUBLISHER_PRIORITY,
                    // 保持する Properties は常に Properties Length varint を含む
                    has_properties: true,
                    is_datagram_origin: false,
                    payload_length: object.payload.len() as u64,
                };
                writer
                    .write_object(&input, &object.payload, Some(&object.properties_data))
                    .await?;
            }
        }
    }
    writer.finish().await?;
    tracing::info!(
        "Served media FETCH (request_id={request_id}, track={}, entries={}, end={end_location:?})",
        String::from_utf8_lossy(track_name),
        entries.len(),
    );
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

/// 出力フレームの Timestamp をマイクロ秒へ換算する
///
/// live capture はエンコーダが載せた値がそのまま Unix epoch マイクロ秒であり、MP4 の
/// 経路は入力トラックのタイムスケール単位である (`PROP_TIMESCALE` に載せる値と同じ)。
/// 時間ベースのキーフレーム要求は入力フレームのマイクロ秒と比較するため、単位を揃える。
fn frame_timestamp_us(timestamp: u64, timescale: Option<u64>) -> u64 {
    match timescale {
        Some(timescale) => media_time_to_micros(timestamp, timescale),
        None => timestamp,
    }
}

/// 音声の配信の遅れの内訳を 1 行にまとめる
///
/// 読み出し・符号化のキュー・送信のキューのどこで遅れが溜まっているかを、1 行で読める
/// ようにする。
fn format_audio_publish_stats(stats: &AudioPublishCatchUpStats) -> String {
    format!(
        "Audio publish stats: read_lag={}ms pending={}ms/{} lag={:?}ms limit={}ms floor={:?}ms dropped={}({}ms) catching_up={} starts={} send_queue={}ms/{} send_lag={:?} max_send_lag={:?}",
        stats.read_lag_ms,
        stats.pending_ms,
        stats.pending_frames,
        stats.lag_ms,
        stats.limit_ms,
        stats.floor_ms,
        stats.dropped_frames,
        stats.dropped_ms,
        stats.catching_up,
        stats.catch_up_starts,
        stats.send_queue_ms,
        stats.send_queue_frames,
        stats.send_lag_ms,
        stats.max_send_lag_ms,
    )
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

/// 音声 Object の送信の遅れを記録する
///
/// 受信側の到着の遅れは「配信側が足した分」と「経路」の合計である。前者を測らないと、
/// 到着の裾が配信側の符号化や送信の詰まりから来ているのか経路から来ているのかを分けられず、
/// どちらを直すべきかを判断できない。そのため、送信のキューへ入れた時刻と送出が終わった
/// 時刻を残す。
///
/// datagram の送信は完了が返らない (fire-and-forget) ため、書き込みの戻りを完了とみなす。
/// そのため `send_queue_ms` は datagram では 0 のままになる (writer の `write` は投入で
/// 解決し、ワイヤに出た時刻は分からない)。測っているのは「読んでから投入するまでの遅れ」
/// である (`AudioPublishCatchUp::record_send_start` / `record_send_complete` を参照)。
///
/// `queued_wall_clock_ms` と `completed_wall_clock_ms` は壁時計のミリ秒で渡す。他の記録
/// (`AudioPublishCatchUp::evaluate` の `now_ms` と、完了が返らない記録を捨てる判定) と
/// 同じ時計に揃えるためである。`timestamp_us` は送るフレームの TIMESTAMP であり、記録の
/// 対応づけに使う。1 つのフレームの開始と完了を同じ値で対応させるため、符号化のキューへ
/// 入れた記録 (`AudioPublishCatchUp::record_encoded_chunk`) が返す「覆った最初の投入の
/// TIMESTAMP」は使わない。
fn record_audio_send(
    catch_up: &mut AudioPublishCatchUp,
    timestamp_us: i64,
    duration_us: i64,
    queued_wall_clock_ms: i64,
    completed_wall_clock_ms: i64,
) {
    catch_up.record_send_start(AudioPublishCatchUpSendStart {
        now_ms: queued_wall_clock_ms,
        timestamp_us,
        captured_wall_clock_ms: timestamp_us / 1_000,
        duration_ms: duration_us / 1_000,
    });
    catch_up.record_send_complete(AudioPublishCatchUpSendComplete {
        now_ms: completed_wall_clock_ms,
        timestamp_us,
        completed_wall_clock_ms,
    });
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
/// ため 0 にはならない。音声と映像の残差の中央値の差は毎フレームの debug ログでは
/// 確認できないため、トラックごとに溜めて定期的に info で出す。
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
    // 音声と映像の残差の中央値の差は、毎フレームの debug ログでは確認できないため、
    // 定期的に要約を出す
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

/// 直前にチャンクを捨てていれば、その直後のチャンクの先頭を落とす
///
/// 捨てた境目の波形が飛ぶとクリックとして聞こえるため、繋ぎ目を滑らかにする。落としたら
/// 印を消す (落とすのは 1 チャンクだけである)。
fn fade_chunk_head_if_dropped(mut chunk: Vec<i16>, next_head: &mut bool) -> Vec<i16> {
    if *next_head {
        *next_head = false;
        fade_splice_edge_i16(
            &mut [chunk.as_mut_slice()],
            AUDIO_SAMPLE_RATE,
            AudioSpliceEdge::Head,
        );
    }
    chunk
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

/// 音声 LOC プロパティを構築する
///
/// `timescale` は入力ファイル由来のメディア時刻を送るときだけ渡す。live capture は
/// Unix epoch マイクロ秒を送るため `None` にし、`PROP_TIMESCALE` を付けない
/// (draft-ietf-moq-loc-04 §2.3.1.1 の既定)。
///
/// `audio_config` に codec 固有の configuration bytes (Opus は OpusHead、AAC-LC は
/// AudioSpecificConfig) を渡すと Audio Config プロパティ (0x0F) として付与する。
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

    // `--cache-duration` の既定値は告知値の期待に使う
    use crate::cli::DEFAULT_CACHE_DURATION_MS;

    /// GOAWAY の New Session URI から移行先を決める判断を固定する
    mod migration_target_tests {
        use super::*;
        use tokio_moq::parse_url;

        /// 現在の URL (移行前の接続先)
        fn current_url() -> ServerUrl {
            parse_url("moqt://relay.example:443/live?token=abc#msf:moq-example--video&c4m=QUJD")
                .expect("現在の URL をパースできること")
        }

        /// request stream の GOAWAY は当該 request だけの移行対象として返すこと
        ///
        /// draft-ietf-moq-transport-22 §6.6.1 は request stream の GOAWAY を
        /// 「その request だけを移し、残りの session はそのままにする」と定める。
        /// 移行するかどうかは呼び出し側が決める。
        #[test]
        fn request_stream_goaway_reports_the_request_scope() {
            let current = current_url();
            let target =
                tokio_moq::migration_target(b"moqt://other.example:443/live", Some(7), &current)
                    .expect("判断に失敗しないこと")
                    .expect("移行先があること");
            assert_eq!(
                target.scope,
                tokio_moq::MigrationScope::Request(7),
                "request 単位の移行対象として返すこと"
            );
            assert_eq!(target.url.authority, "other.example:443");
        }

        /// New Session URI が空のときは現在の URL を再利用すること
        ///
        /// draft-ietf-moq-transport-22 §9.2 (GOAWAY) は「URI が 0 バイトなら現在の URI を
        /// 再利用する」と定める。
        #[test]
        fn empty_uri_reuses_the_current_url() {
            let current = current_url();
            let target = tokio_moq::migration_target(&[], None, &current)
                .expect("判断に失敗しないこと")
                .expect("移行先があること");
            assert_eq!(target.scope, tokio_moq::MigrationScope::Session);
            assert_eq!(target.url.authority, current.authority);
            assert_eq!(target.url.path, current.path);
            assert_eq!(target.url.c4m_tokens, current.c4m_tokens);
        }

        /// 同一 origin の New Session URI で c4m が無いときは元のトークンを引き継ぐこと
        ///
        /// draft-ietf-moq-msf-01 §11.4.3 は track に紐づくトークンを同じ track の制御
        /// メッセージへ載せる MUST を課すため、移行先でも載せ続ける必要がある。
        #[test]
        fn same_origin_uri_without_c4m_carries_the_tokens_over() {
            let current = current_url();
            let target = tokio_moq::migration_target(
                b"moqt://relay.example:443/other#msf:moq-example--video",
                None,
                &current,
            )
            .expect("判断に失敗しないこと")
            .expect("移行先があること");
            assert_eq!(target.url.authority, "relay.example:443");
            assert_eq!(target.url.path, "/other");
            assert_eq!(
                target.url.c4m_tokens, current.c4m_tokens,
                "同一 origin なら移行先に c4m が無くても元のトークンを引き継ぐこと"
            );
        }

        /// 別 origin の New Session URI には c4m を引き継がないこと
        ///
        /// 移行先が別ホストのときは認可トークンを送らない。移行先で別途取得する
        /// (draft-ietf-moq-msf-01 §11.4.4)。
        #[test]
        fn different_origin_uri_does_not_carry_tokens() {
            let current = current_url();
            let target = tokio_moq::migration_target(
                b"moqt://other.example:443/live#msf:moq-example--video",
                None,
                &current,
            )
            .expect("判断に失敗しないこと")
            .expect("移行先があること");
            assert_eq!(target.url.authority, "other.example:443");
            assert!(
                target.url.c4m_tokens.is_empty(),
                "別 origin へ認可トークンを送らないこと"
            );
        }

        /// New Session URI の fragment に c4m があるときはそちらを使うこと
        #[test]
        fn uri_with_c4m_uses_its_own_tokens() {
            let current = current_url();
            let target = tokio_moq::migration_target(
                b"moqt://other.example:443/live#msf:moq-example--video&c4m=WFla",
                None,
                &current,
            )
            .expect("判断に失敗しないこと")
            .expect("移行先があること");
            assert_eq!(target.url.c4m_tokens, vec![b"XYZ".to_vec()]);
        }

        /// UTF-8 でない New Session URI はエラーにすること
        #[test]
        fn non_utf8_uri_is_rejected() {
            let current = current_url();
            let err = tokio_moq::migration_target(&[0xff, 0xfe], None, &current)
                .expect_err("エラーになること");
            assert!(
                err.to_string().contains("not UTF-8"),
                "UTF-8 でない URI として報告すること: {err}"
            );
        }

        /// 解釈できない New Session URI はエラーにすること
        ///
        /// scheme が `moqt://` でない URI は移行先にしない (移行失敗として扱う)。
        #[test]
        fn invalid_uri_is_rejected() {
            let current = current_url();
            let err = tokio_moq::migration_target(b"https://other.example/live", None, &current)
                .expect_err("エラーになること");
            assert!(
                err.to_string().contains("invalid new session URI"),
                "解釈できない URI として報告すること: {err}"
            );
        }
    }

    /// GOAWAY の適用範囲と New Session URI から publisher の動作を決める判断を固定する
    ///
    /// draft-ietf-moq-transport-22 §6.6.1 (Graceful Session Migration) / §9.2 (GOAWAY)。
    mod goaway_action_tests {
        use super::*;

        /// control stream の GOAWAY は session 全体の移行になること
        ///
        /// New Session URI の有無にかかわらず、control stream の GOAWAY は session 全体を
        /// 移行する (§6.6.1)。
        #[test]
        fn control_stream_goaway_migrates_the_session() {
            assert_eq!(
                goaway_action(MigrationScope::Session, true),
                GoawayAction::MigrateSession,
                "空の New Session URI でも session 全体を移行すること"
            );
            assert_eq!(
                goaway_action(MigrationScope::Session, false),
                GoawayAction::MigrateSession,
                "New Session URI 付きでも session 全体を移行すること"
            );
        }

        /// request stream の GOAWAY で URI が空なら現在の session で再発行すること
        ///
        /// draft-ietf-moq-transport-22 §9.2 (GOAWAY) は、URI が無い場合は「現在の session」で
        /// 当該 request を再発行することを求める。
        #[test]
        fn request_stream_goaway_without_a_uri_republishes_the_request() {
            assert_eq!(
                goaway_action(MigrationScope::Request(7), true),
                GoawayAction::RepublishRequest(7),
                "空 URI は現在の session での再発行になること"
            );
        }

        /// request stream の GOAWAY に URI がある場合は移行しないこと
        ///
        /// 1 つの request だけを別 session へ移すには 2 session の併走が必要であり、
        /// この example は 1 session しか持たないため移行しない (§9.2)。
        #[test]
        fn request_stream_goaway_with_a_uri_is_ignored() {
            assert_eq!(
                goaway_action(MigrationScope::Request(7), false),
                GoawayAction::IgnoreRequestScope(7),
                "移行先 URI 付きの request 単位移行は移行しないこと"
            );
        }
    }

    /// request id から再発行する track を引く判断を固定する
    mod publish_track_tests {
        use super::*;

        /// 映像 / 音声 / カタログの request id をそれぞれの track として引くこと
        #[test]
        fn publish_track_for_request_maps_each_request_id() {
            assert_eq!(
                publish_track_for_request(10, Some(10), Some(11), 12),
                PublishTrack::Video,
                "映像の request id は映像として引くこと"
            );
            assert_eq!(
                publish_track_for_request(11, Some(10), Some(11), 12),
                PublishTrack::Audio,
                "音声の request id は音声として引くこと"
            );
            assert_eq!(
                publish_track_for_request(12, Some(10), Some(11), 12),
                PublishTrack::Catalog,
                "カタログの request id はカタログとして引くこと"
            );
        }

        /// 配信していない track の request id は Unknown になること
        ///
        /// 音声を配信しない場合 (`audio_request_id` が `None`) や、publisher が送っていない
        /// request の id は再発行の対象にしない。
        #[test]
        fn publish_track_for_request_reports_unknown_for_other_requests() {
            assert_eq!(
                publish_track_for_request(9, Some(10), None, 12),
                PublishTrack::Unknown,
                "どの PUBLISH の id でもない request は Unknown にすること"
            );
            assert_eq!(
                publish_track_for_request(11, Some(10), None, 12),
                PublishTrack::Unknown,
                "音声を配信しないときは音声の id と一致しないこと"
            );
        }
    }
    // `NEW_GROUP_REQUEST` の判定はテストでのみ直接組み立てる
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_NEW_GROUP_REQUEST,
    };
    // セッション終了の判定は `Error` の variant で行うため、`TransportError` はテストでのみ使う
    use tokio_moq::error::TransportError;

    /// 音声の配信の遅れの内訳が、段ごとの値をすべて出すこと
    ///
    /// 遅れがどの段 (読み出し / 符号化のキュー / 送信のキュー) で溜まっているかを CI の
    /// ログから読めるようにするため、どの値も欠かさず出す。
    #[test]
    fn audio_publish_stats_reports_every_stage() {
        let stats = AudioPublishCatchUpStats {
            policy: shiguredo_moqt::playout::publish_catch_up::AudioPublishCatchUpPolicy::Drop,
            dropped_frames: 3,
            dropped_ms: 60,
            lag_ms: Some(120),
            floor_ms: Some(20),
            max_lag_ms: Some(200),
            pending_ms: 100,
            read_lag_ms: 20,
            limit_ms: 60,
            pending_frames: 5,
            catching_up: true,
            catch_up_starts: 2,
            send_queue_ms: 40,
            send_queue_frames: 2,
            send_lag_ms: Some(30),
            max_send_lag_ms: Some(50),
        };
        let line = format_audio_publish_stats(&stats);
        for expected in [
            "read_lag=20ms",
            "pending=100ms/5",
            "lag=Some(120)ms",
            "limit=60ms",
            "floor=Some(20)ms",
            "dropped=3(60ms)",
            "catching_up=true",
            "starts=2",
            "send_queue=40ms/2",
            "send_lag=Some(30)",
            "max_send_lag=Some(50)",
        ] {
            assert!(
                line.contains(expected),
                "内訳に {expected} が含まれること: {line}"
            );
        }
    }

    /// 音声 Object の送信の遅れが、読んでから送り終えるまでとして記録されること
    ///
    /// 受信側の到着の遅れを「配信側が足した分」と「経路」に分けるために要る値である。
    /// 捕捉の時刻は TIMESTAMP (LOC TIMESTAMP、Unix epoch マイクロ秒) から取り、完了は送出の
    /// 直後の壁時計から取る。
    #[test]
    fn audio_send_records_the_delay_from_capture_to_completion() {
        let mut catch_up = AudioPublishCatchUp::new();
        // 捕捉は 1_700_000_000_000 ms、送出の完了はその 10 ms 後である
        record_audio_send(
            &mut catch_up,
            1_700_000_000_000_000,
            20_000,
            1_700_000_000_000,
            1_700_000_000_010,
        );
        let stats = catch_up.snapshot();
        assert_eq!(
            stats.send_lag_ms,
            Some(10),
            "読んでから送り終えるまでを測ること"
        );
        assert_eq!(stats.max_send_lag_ms, Some(10), "最大も同じ値になること");
        assert_eq!(stats.send_queue_frames, 0, "完了した分は待ちに残らないこと");
        assert_eq!(stats.send_queue_ms, 0, "待ちの長さも残らないこと");
    }

    /// 送信の遅れが、観測した最大を保つこと
    ///
    /// 到着の裾を分解するときは、直近の値ではなく最大を見る。ばらつきの原因を追うためである。
    #[test]
    fn audio_send_keeps_the_max_delay() {
        let mut catch_up = AudioPublishCatchUp::new();
        record_audio_send(
            &mut catch_up,
            1_700_000_000_000_000,
            20_000,
            1_700_000_000_000,
            1_700_000_000_010,
        );
        record_audio_send(
            &mut catch_up,
            1_700_000_000_020_000,
            20_000,
            1_700_000_000_020,
            1_700_000_000_060,
        );
        let stats = catch_up.snapshot();
        assert_eq!(stats.send_lag_ms, Some(40), "直近の値を返すこと");
        assert_eq!(stats.max_send_lag_ms, Some(40), "観測した最大を保つこと");
        assert_eq!(stats.send_queue_frames, 0, "完了した分は待ちに残らないこと");
    }

    /// 送出が終わっていない音は、送信の待ちとして見えること
    ///
    /// datagram では書き込みの戻りを完了とみなすため待ちは残らないが、ストリームでは
    /// 書き込みの待ち (backpressure) の間だけ待ちに数える。
    #[test]
    fn audio_send_shows_the_queue_until_completion() {
        let mut catch_up = AudioPublishCatchUp::new();
        catch_up.record_send_start(AudioPublishCatchUpSendStart {
            now_ms: 1_700_000_000_000,
            timestamp_us: 1_700_000_000_000_000,
            captured_wall_clock_ms: 1_700_000_000_000,
            duration_ms: 20,
        });
        let stats = catch_up.snapshot();
        assert_eq!(stats.send_queue_frames, 1, "完了前は待ちに数えること");
        assert_eq!(stats.send_queue_ms, 20, "完了前は長さも待ちに数えること");
        assert_eq!(stats.send_lag_ms, None, "完了前は遅れを出さないこと");
    }

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

    /// MP4 パススルーでは DYNAMIC_GROUPS を告知しないこと
    ///
    /// パススルーはエンコーダを使わずキーフレームを要求できないため、購読者に
    /// `NEW_GROUP_REQUEST` を送れると告知しない (draft-ietf-moq-transport-22 §10.6)。
    /// `MAX_CACHE_DURATION` は保持するため、パススルーでも告知する。
    #[test]
    fn video_publish_properties_skip_dynamic_groups_for_mp4_passthrough() {
        assert!(
            video_publish_properties(true, DEFAULT_CACHE_DURATION_MS)
                .dynamic_groups()
                .is_none(),
            "パススルーでは告知しないこと"
        );
        assert_eq!(
            video_publish_properties(false, DEFAULT_CACHE_DURATION_MS).dynamic_groups(),
            Some(1),
            "エンコードする場合は告知すること"
        );
        for mp4_passthrough in [true, false] {
            assert_eq!(
                video_publish_properties(mp4_passthrough, DEFAULT_CACHE_DURATION_MS)
                    .find_varint(PROP_MAX_CACHE_DURATION),
                Some(DEFAULT_CACHE_DURATION_MS),
                "パススルーでも MAX_CACHE_DURATION は告知すること: {mp4_passthrough}"
            );
        }
    }

    /// 告知する `MAX_CACHE_DURATION` が `--cache-duration` と同じ値であること
    ///
    /// draft-ietf-moq-transport-22 §10.3 (MAX CACHE DURATION): 保持した Object を配れる時間を
    /// ミリ秒で示す。告知値と実際の保持が食い違うと、購読者と relay が配れる期間を誤解する。
    /// 映像 / 音声の PUBLISH と SUBSCRIBE_OK のどの経路でも同じ値を載せる。
    #[test]
    fn max_cache_duration_follows_the_cache_duration() {
        for cache_duration_ms in [0, 1, 5_000, 60_000] {
            assert_eq!(
                video_publish_properties(false, cache_duration_ms)
                    .find_varint(PROP_MAX_CACHE_DURATION),
                Some(cache_duration_ms),
                "映像の PUBLISH に同じ値を載せること: {cache_duration_ms}"
            );
            assert_eq!(
                cache_duration_properties(cache_duration_ms).find_varint(PROP_MAX_CACHE_DURATION),
                Some(cache_duration_ms),
                "音声の PUBLISH に同じ値を載せること: {cache_duration_ms}"
            );
            assert_eq!(
                subscribe_ok_properties(VIDEO_TRACK_ALIAS, false, cache_duration_ms)
                    .find_varint(PROP_MAX_CACHE_DURATION),
                Some(cache_duration_ms),
                "映像の SUBSCRIBE_OK に同じ値を載せること: {cache_duration_ms}"
            );
            assert_eq!(
                subscribe_ok_properties(AUDIO_TRACK_ALIAS, false, cache_duration_ms)
                    .find_varint(PROP_MAX_CACHE_DURATION),
                Some(cache_duration_ms),
                "音声の SUBSCRIBE_OK に同じ値を載せること: {cache_duration_ms}"
            );
            assert_eq!(
                subscribe_ok_properties(CATALOG_TRACK_ALIAS, false, cache_duration_ms)
                    .find_varint(PROP_MAX_CACHE_DURATION),
                None,
                "カタログの SUBSCRIBE_OK には載せないこと: {cache_duration_ms}"
            );
        }
    }

    /// FETCH の要求 range が Location Filter と Largest Object から決まること
    ///
    /// draft-ietf-moq-transport-22 §3.2 (Fetch) / §3.3.1 (Location Filters) / §9.20.9
    /// (LOCATION FILTER Parameter): 省略時は Start = `{0, 0}` / End = Largest Object である。
    /// 要求が Largest Object より先を指しても配れるのは Largest Object までなので、End は
    /// Largest Object でクランプする。
    #[test]
    fn fetch_request_range_follows_the_location_filter_and_the_largest_object() {
        let largest = Location {
            group_id: 10,
            object_id: 5,
        };

        // フィルタ省略 (unfiltered)
        assert_eq!(
            fetch_request_range(None, Some(largest)),
            (
                Location {
                    group_id: 0,
                    object_id: 0,
                },
                largest
            ),
            "省略時は track の先頭から Largest Object までであること"
        );

        // Absolute Start: End 省略時は Largest Object (Fetch の既定)
        let filter = LocationFilter::AbsoluteStart {
            start: Location {
                group_id: 3,
                object_id: 2,
            },
        };
        assert_eq!(
            fetch_request_range(Some(&filter), Some(largest)),
            (
                Location {
                    group_id: 3,
                    object_id: 2
                },
                largest
            ),
            "Start だけの指定は Largest Object までであること"
        );

        // Absolute Range: End Object まで
        let filter = LocationFilter::AbsoluteRangeWithEnd {
            start: Location {
                group_id: 3,
                object_id: 2,
            },
            end_group_delta: 1,
            end_object: 4,
        };
        assert_eq!(
            fetch_request_range(Some(&filter), Some(largest)),
            (
                Location {
                    group_id: 3,
                    object_id: 2
                },
                Location {
                    group_id: 4,
                    object_id: 4
                }
            ),
            "指定された End までであること"
        );

        // Largest Object より先を指す End はクランプする
        let filter = LocationFilter::AbsoluteRangeWithEnd {
            start: Location {
                group_id: 3,
                object_id: 2,
            },
            end_group_delta: 99,
            end_object: 4,
        };
        assert_eq!(
            fetch_request_range(Some(&filter), Some(largest)),
            (
                Location {
                    group_id: 3,
                    object_id: 2
                },
                largest
            ),
            "End は Largest Object でクランプすること"
        );

        // Relative Start: Largest Group から遡る
        let filter = LocationFilter::RelativeGroup { start_group: 1 };
        assert_eq!(
            fetch_request_range(Some(&filter), Some(largest)),
            (
                Location {
                    group_id: 10,
                    object_id: 0
                },
                largest
            ),
            "相対 Start は Largest Object の Group から解決すること"
        );

        // Largest Object が無い track は先頭側に解決する (配れる Object は無い)
        assert_eq!(
            fetch_request_range(None, None),
            (
                Location {
                    group_id: 0,
                    object_id: 0
                },
                Location {
                    group_id: 0,
                    object_id: 0
                }
            ),
            "Largest Object が無ければ先頭に解決すること"
        );
        let filter = LocationFilter::AbsoluteStart {
            start: Location {
                group_id: 3,
                object_id: 2,
            },
        };
        assert_eq!(
            fetch_request_range(Some(&filter), None),
            (
                Location {
                    group_id: 3,
                    object_id: 2
                },
                Location {
                    group_id: 3,
                    object_id: 2
                }
            ),
            "Largest Object が無ければ End も Start に解決すること"
        );
    }

    /// SUBSCRIBE_OK の Track Properties が映像トラックだけ DYNAMIC_GROUPS を告知すること
    ///
    /// 購読者は SUBSCRIBE_OK の値で publisher の対応を知る
    /// (draft-ietf-moq-transport-22 §3.5.1)。音声とカタログには載せない。
    #[test]
    fn subscribe_ok_properties_announce_dynamic_groups_for_the_video_track() {
        assert_eq!(
            subscribe_ok_properties(VIDEO_TRACK_ALIAS, false, DEFAULT_CACHE_DURATION_MS)
                .dynamic_groups(),
            Some(1),
            "映像トラックには告知すること"
        );
        assert!(
            subscribe_ok_properties(VIDEO_TRACK_ALIAS, true, DEFAULT_CACHE_DURATION_MS)
                .dynamic_groups()
                .is_none(),
            "MP4 パススルーでは告知しないこと"
        );
        for track_alias in [AUDIO_TRACK_ALIAS, CATALOG_TRACK_ALIAS] {
            assert!(
                subscribe_ok_properties(track_alias, false, DEFAULT_CACHE_DURATION_MS)
                    .dynamic_groups()
                    .is_none(),
                "音声とカタログには DYNAMIC_GROUPS を載せないこと: {track_alias}"
            );
        }
        assert_eq!(
            subscribe_ok_properties(CATALOG_TRACK_ALIAS, false, DEFAULT_CACHE_DURATION_MS).len(),
            0,
            "カタログには何も載せないこと"
        );
    }

    /// キーフレームを強制できる要求だけを受け付けること
    ///
    /// 値が 0 か現在の Group より大きいときだけ受け付ける
    /// (draft-ietf-moq-transport-22 §9.20.19)。音声やカタログのトラック宛ての要求、および
    /// キーフレームを強制できない経路 (MP4 パススルー) の要求では映像のキーフレームを
    /// 強制しない。
    #[test]
    fn accepted_new_group_request_checks_the_track_and_the_value() {
        let parameters = |next_group_id: u64| {
            let mut parameters = MessageParameters::new();
            parameters.push(MessageParameter {
                param_type: PARAM_NEW_GROUP_REQUEST,
                value: MessageParameterValue::VarInt(next_group_id),
            });
            parameters
        };
        let current = Some(100);

        assert_eq!(
            accepted_new_group_request(true, &parameters(0), current),
            Some(0),
            "値 0 は受け付けること"
        );
        assert_eq!(
            accepted_new_group_request(true, &parameters(101), current),
            Some(101),
            "現在の Group より大きい値は受け付けること"
        );
        assert_eq!(
            accepted_new_group_request(true, &parameters(100), current),
            None,
            "現在の Group と同じ値は受け付けないこと"
        );
        assert_eq!(
            accepted_new_group_request(true, &parameters(99), current),
            None,
            "過去の Group への要求は受け付けないこと"
        );
        assert_eq!(
            accepted_new_group_request(false, &parameters(0), current),
            None,
            "キーフレームを強制できない要求は受け付けないこと"
        );
        assert_eq!(
            accepted_new_group_request(true, &MessageParameters::new(), current),
            None,
            "要求が無い場合は受け付けないこと"
        );
    }

    /// 映像トラックの PUBLISH に DYNAMIC_GROUPS=1 を載せること
    ///
    /// 購読者は DYNAMIC_GROUPS=1 を知らないと REQUEST_UPDATE で NEW_GROUP_REQUEST を
    /// 送れない (draft-ietf-moq-transport-22 §9.20.19)。音声とカタログのトラックには
    /// 載せないため、空の Track Properties では告知されないことも確かめる。
    #[test]
    fn video_track_properties_announce_dynamic_groups() {
        let properties = video_track_properties(DEFAULT_CACHE_DURATION_MS);
        assert_eq!(
            properties.dynamic_groups(),
            Some(1),
            "映像トラックの PUBLISH に DYNAMIC_GROUPS=1 を載せること"
        );
        assert_eq!(
            properties.len(),
            2,
            "映像トラックは DYNAMIC_GROUPS と MAX_CACHE_DURATION を載せること"
        );
        assert!(
            TrackProperties::new().dynamic_groups().is_none(),
            "空の Track Properties では告知しないこと"
        );
    }

    /// 出力フレームの Timestamp がタイムスケール単位からマイクロ秒へ換算されること
    ///
    /// live capture はマイクロ秒をそのまま載せるため換算しない。MP4 の経路では
    /// 入力トラックのタイムスケール単位であり、時間ベースの判定にはマイクロ秒が要る。
    #[test]
    fn frame_timestamp_is_converted_to_microseconds() {
        assert_eq!(
            frame_timestamp_us(1_700_000_000_000_000, None),
            1_700_000_000_000_000,
            "live capture はマイクロ秒をそのまま使うこと"
        );
        assert_eq!(
            frame_timestamp_us(9_000, Some(90_000)),
            100_000,
            "90000 タイムスケールの 9000 が 100000 マイクロ秒になること"
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

    /// Audio Config 付きの音声 LOC プロパティ: Timestamp / Timescale / Audio Config が付与され encode できること
    ///
    /// MP4 の経路では入力ファイルのメディア時刻を入力トラックの timescale で送る。
    #[test]
    fn build_audio_loc_properties_with_audio_config() {
        let opus_head = crate::encoder::opus::build_opus_head(48_000, 1);
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

    /// 後始末の `finish` の失敗を info にする条件 (真理値表)
    ///
    /// 期待される失敗はセッション終了・ transport によるセッション終了・ peer の cancel であり、
    /// それ以外は warn にする。FIN / RESET 送信済みのストリームが返す
    /// `TransportError::StreamClosed` は `Error` に variant が無いため warn のままになる。
    ///
    /// 呼び出し側 (`run` の後始末) は実 relay が必要なため本テストでは観測できず、レビューで
    /// 確認する (`session_ended` を渡していること、真 = `info!` の写像)。
    #[test]
    fn cleanup_finish_is_expected_matches_the_truth_table() {
        let stream_reset = Error::from(TransportError::StreamReset { error_code: 0x1 });
        let connection_closed = Error::from(TransportError::ConnectionClosed);
        let stream_closed = Error::from(TransportError::StreamClosed);
        let other = Error::Other("encode failed".to_string());

        // セッション終了を観測していれば、どの失敗も期待される
        for error in [&stream_reset, &connection_closed, &stream_closed, &other] {
            assert!(
                cleanup_finish_is_expected(true, error),
                "セッション終了後の失敗は期待されること: {error}"
            );
        }

        // セッション終了を観測していなくても、接続クローズと peer の cancel は期待される
        assert!(
            cleanup_finish_is_expected(false, &connection_closed),
            "接続クローズは期待されること"
        );
        assert!(
            cleanup_finish_is_expected(false, &stream_reset),
            "peer の cancel は期待されること"
        );

        // それ以外は warn のままにする
        assert!(
            !cleanup_finish_is_expected(false, &stream_closed),
            "FIN / RESET 送信済みのストリームへの送信は期待される失敗に数えないこと: {stream_closed}"
        );
        assert!(
            !cleanup_finish_is_expected(false, &other),
            "その他の失敗は期待される失敗に数えないこと: {other}"
        );
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
