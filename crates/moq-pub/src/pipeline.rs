//! publisher の全体パイプライン
//!
//! カメラ/マイクキャプチャ → 映像/音声エンコード → MOQT 送信の流れを、
//! sans-I/O な `shiguredo_moqt::session::core::Session` を駆動する
//! [`tokio_moq::moqt_client::MoqtClient`] と結線する。

use std::collections::{HashMap, VecDeque};
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
use tokio_moq::transport::{self, StreamHandle};
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
/// PUBLISH_DONE の Status Code: GOING_AWAY (draft-ietf-moq-transport-23 §16.12.3 (PUBLISH_DONE Codes))
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
/// draft-ietf-moq-transport-23 §9 (Control Messages): PUBLISH などの制御メッセージを送った
/// 後、peer の応答をこの時間待つ。応答を返さない peer に移行処理が引きずられないよう、
/// 移行先の session にも同じ値を設定する。
const CONTROL_MESSAGE_TIMEOUT_MS: u64 = 30_000;

/// data stream の進捗を待つ上限 (ミリ秒)
///
/// draft-ietf-moq-transport-23 §12.2 (Session Termination Codes) の DATA_STREAM_TIMEOUT に
/// 対応する。subgroup stream の書き込みが止まったままにならないよう設定する。
const DATA_STREAM_TIMEOUT_MS: u64 = 30_000;

/// GOAWAY による移行を制限する時間窓
///
/// 移行先がまた GOAWAY を返す構成では、移行を無制限に繰り返すと再接続ストームになる。
/// この時間窓の中で [`MAX_MIGRATIONS_PER_WINDOW`] 回を超える移行を拒否する。
const MIGRATION_LIMIT_WINDOW: Duration = Duration::from_secs(600);

/// 時間窓内で許す移行回数
const MAX_MIGRATIONS_PER_WINDOW: usize = 5;

/// URL ごとの side session タスクから main ループへ送るイベントのチャネル容量
///
/// side session の session イベントは 1 本の session から順に届き、Object の送信経路のように
/// まとめて届くことはない。詰まった場合は side session の受信を止めて main ループの処理を
/// 待つため、小さな容量で十分である。
const REQUEST_MIGRATION_EVENT_CHANNEL_SIZE: usize = 32;

/// URL ごとに共有する side session へ送るコマンドのチャネル容量
///
/// 移行できる track は映像 / 音声 / カタログの 3 本だけであり、同時に溜まるコマンドも
/// その本数に収まる。session の確立中に 2 本目以降のコマンドが届いても捨てないよう、
/// 全 track 分を格納できる容量にする。
const REQUEST_MIGRATION_COMMAND_CHANNEL_SIZE: usize = 4;

/// 移行の PUBLISH 失敗後に接続からやり直すときの初回待ち時間
const MIGRATION_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(100);

/// 移行の PUBLISH 失敗後に接続からやり直すときの待ち時間の上限
const MIGRATION_RETRY_MAX_DELAY: Duration = Duration::from_secs(1);

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
    reliable_size: Option<u64>,
    tracking: shiguredo_moqt::session::types::ResetTracking,
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
    match writer
        .reset_by_session(error_code, reliable_size, tracking)
        .await
    {
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
/// draft-ietf-moq-transport-23 §10.3 (MAX CACHE DURATION): 保持した Object を配れる時間を
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
/// draft-ietf-moq-transport-23 §10.6 (DYNAMIC_GROUPS) の `DYNAMIC_GROUPS=1` を告知し、
/// 購読者からの `NEW_GROUP_REQUEST` (§9.19.19) に応じて Group を動的に開始できることを
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
/// 動的 Group に対応しているかを知り (draft-ietf-moq-transport-23 §3.5.1)、REQUEST_UPDATE で
/// `NEW_GROUP_REQUEST` を送れるようになる (§9.19.19 の MUST NOT)。音声トラックには
/// `MAX_CACHE_DURATION` だけを載せ、カタログには何も載せない。
fn subscribe_ok_properties(
    track: PublishedTrack,
    mp4_passthrough: bool,
    cache_duration_ms: u64,
) -> TrackProperties {
    match track {
        PublishedTrack::Video => video_publish_properties(mp4_passthrough, cache_duration_ms),
        PublishedTrack::Audio => cache_duration_properties(cache_duration_ms),
        PublishedTrack::Catalog => TrackProperties::new(),
    }
}

/// 受信したパラメータから、映像のキーフレーム要求として受け付ける `NEW_GROUP_REQUEST` を取り出す
///
/// `can_force_keyframe` は要求を映像のキーフレーム要求として扱えるかどうかである。映像
/// トラック宛てであっても、キーフレームを強制できない経路 (MP4 パススルー) では扱わない。
/// 値が 0 (購読者が Group の情報を持たない) か、現在の Group より大きいときだけ受け付ける
/// (draft-ietf-moq-transport-23 §9.19.19)。受け付けない要求は debug ログに残す。
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

/// カタログ JSON を組み立てるための入力
///
/// 起動時と GOAWAY 移行後の双方で同じ入力から MSF カタログを組み立てる。移行先の `c4m` が
/// 変わったときは authInfo を載せ替える必要があるため
/// (draft-ietf-moq-msf-01 §5.2.42 (Authorization Info))、エンコーダ / MP4 リーダーへの参照では
/// なく、組み立てに必要な値だけを所有して保持する。
struct CatalogContext {
    /// Track Namespace の §8.8 表現
    ///
    /// draft-ietf-moq-msf-01 §5.2.2 (Track namespace)
    namespace_text: String,
    /// 映像トラック名
    track_name: String,
    /// 音声・映像で共通の同期用メタデータ
    sync: catalog::CatalogSyncParams,
    /// カタログに載せる映像トラック (映像を配信しない場合は `None`)
    video: Option<CatalogVideoTrack>,
    /// カタログに載せる音声トラック (音声を配信しない場合は `None`)
    audio: Option<CatalogAudioTrack>,
}

/// カタログに載せる映像トラックの値
struct CatalogVideoTrack {
    /// RFC 6381 形式の codec 文字列 (例: `av01.0.08M.08`)
    codec: String,
    /// 映像幅 (px)
    width: u32,
    /// 映像高さ (px)
    height: u32,
    /// フレームレート
    fps: u32,
    /// kbps
    bitrate: u32,
    /// 映像トラックの最大 Group 長 (ms)
    ///
    /// `maxGopDuration` / `maxGroupDuration` に同じ値を載せる。Group 長を宣言しない経路
    /// (MP4 パススルー) では `None`。
    max_group_duration_ms: Option<u64>,
}

/// カタログに載せる音声トラックの値
struct CatalogAudioTrack {
    /// RFC 6381 形式の codec 文字列 (例: `opus`)
    codec: String,
    /// サンプリングレート (Hz)
    samplerate: u32,
    /// MSF channelConfig (mono = "1")
    channel_config: String,
    /// kbps
    bitrate: u32,
}

impl CatalogContext {
    /// カタログの track パラメータを組み立てる
    ///
    /// 戻り値は [`catalog::VideoTrackParams`] と [`catalog::AudioTrackParams`] であり、
    /// どちらも `self` の文字列を借用する。
    fn track_params(
        &self,
    ) -> (
        Option<catalog::VideoTrackParams<'_>>,
        Option<catalog::AudioTrackParams<'_>>,
    ) {
        let video = self.video.as_ref().map(|video| catalog::VideoTrackParams {
            track_name: &self.track_name,
            namespace: &self.namespace_text,
            codec: &video.codec,
            width: video.width,
            height: video.height,
            fps: video.fps,
            bitrate: video.bitrate,
            max_group_duration_ms: video.max_group_duration_ms,
        });
        let audio = self.audio.as_ref().map(|audio| catalog::AudioTrackParams {
            track_name: AUDIO_TRACK_NAME,
            namespace: &self.namespace_text,
            codec: &audio.codec,
            samplerate: audio.samplerate,
            channel_config: &audio.channel_config,
            bitrate: audio.bitrate,
        });
        (video, audio)
    }

    /// 指定した c4m トークンでカタログ JSON を組み立てる
    ///
    /// 移行先の `c4m` が変わったとき、この関数で authInfo を載せ替える。
    /// 送信は行わない (送信は [`catalog::send_catalog`] / [`republish_catalog_at_group`])。
    fn json(&self, c4m_tokens: &[Vec<u8>]) -> Result<Vec<u8>> {
        let (video, audio) = self.track_params();
        catalog::build_catalog_json(video.as_ref(), audio.as_ref(), self.sync, c4m_tokens)
    }
}

/// カタログの送信用パラメータを組み立てる
///
/// 起動時の初回送信で使う。JSON の組み立てと送信は [`catalog::send_catalog`] が担う。
fn catalog_params<'a>(
    context: &'a CatalogContext,
    handle: &'a StreamHandle,
    data_plane: &'a DataPlaneHandle,
    catalog_request_id: u64,
    group_id: u64,
    start_location: Option<Location>,
    c4m_tokens: &'a [Vec<u8>],
) -> catalog::CatalogParams<'a> {
    let (video, audio) = context.track_params();
    catalog::CatalogParams {
        handle,
        data_plane,
        catalog_request_id,
        catalog_alias: CATALOG_TRACK_ALIAS,
        group_id,
        start_location,
        sync: context.sync,
        c4m_tokens,
        video,
        audio,
    }
}

/// 移行先の c4m トークンが変わったか
///
/// 移行先 URL の `c4m` が変わると、catalog の track に載せる authInfo を作り直す必要がある
/// (draft-ietf-moq-msf-01 §5.2.42 (Authorization Info))。authInfo は現在トークンの有無だけを
/// 見るため値の変化では JSON は変わらないが、将来 authInfo が値を参照する変更に備えて
/// 並びも含めて比較する。
fn c4m_tokens_changed(previous: &[Vec<u8>], next: &[Vec<u8>]) -> bool {
    previous != next
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
            close_session_after_failed_publish(&mut client).await;
            Err(e)
        }
    }
}

/// PUBLISH に失敗した session を閉じる (失敗はログのみ)
///
/// 確立済みの session を放置すると peer が timeout まで資源を保持するため、失敗を返す前に
/// 閉じる。close 自体の失敗は、このあと session を捨てるため致命ではない。
async fn close_session_after_failed_publish(client: &mut MoqtClient) {
    if let Err(close_error) = client.close(0, "").await {
        tracing::debug!("Failed to close the session after a failed publish: {close_error}");
    }
}

/// 移行の PUBLISH 失敗を新しい接続で再試行するか
///
/// draft-ietf-moq-transport-23 §9.2 (GOAWAY) の移行では、新しい session の PUBLISH が一時的な
/// 理由 (relay の切替直後、接続断、REQUEST_ERROR による拒否) で失敗しうる。新しい session を
/// 作り直せば解消しうるため再試行する。プロトコル違反とメッセージの encode / decode 失敗は
/// 同じコードで繰り返しても再発するため再試行しない。draft 由来の規則であり将来の draft
/// 改版で変わる可能性がある。
fn migration_publish_failure_is_retryable(error: &Error) -> bool {
    !matches!(error, Error::Moqt(_))
}

/// 移行の PUBLISH 失敗後に接続からやり直すまでの待ち時間
///
/// 指数バックオフ (初回 [`MIGRATION_RETRY_INITIAL_DELAY`]、上限
/// [`MIGRATION_RETRY_MAX_DELAY`]) を残り時間で cap する。残り時間が無い場合は再試行しない
/// (`None`)。移行は 1 クライアントの 1 本の session のための処理であり、同時に移行する複数
/// クライアントの再試行をずらす必要はないため jitter は入れない
/// ([`connect_with_retry`] のバックオフとは役割が異なる)。
fn migration_retry_delay(attempt: u32, remaining: Duration) -> Option<Duration> {
    if remaining.is_zero() {
        return None;
    }
    let factor = 1u32 << attempt.min(16);
    let delay = MIGRATION_RETRY_INITIAL_DELAY
        .saturating_mul(factor)
        .min(MIGRATION_RETRY_MAX_DELAY);
    Some(delay.min(remaining))
}

/// 移行期限付きで接続し、映像 / 音声 / catalog の PUBLISH を張る
///
/// draft-ietf-moq-transport-23 §6.6.1 (Graceful Session Migration) / §9.2 (GOAWAY):
/// GOAWAY の Timeout を移行期限として扱う。接続は [`connect_with_retry`] が期限まで再試行し、
/// PUBLISH が失敗した場合は確立済みの session を閉じて接続からやり直す (期限が残っている間
/// のみ。待ち時間は [`migration_retry_delay`])。PUBLISH の応答待ちにも残り時間を上限として
/// 使い、期限を過ぎた移行を打ち切る。接続や PUBLISH で時間を使った分は期限 (絶対時刻) から
/// 自然に差し引かれる。期限切れの場合は最後の失敗を返す。
/// draft 由来の規則であり将来の draft 改版で変わる可能性がある。
async fn establish_publisher_session_with_retry(
    config: &Config,
    url: &ServerUrl,
    namespace: &TrackNamespace,
    task_monitor: &tokio_metrics::TaskMonitor,
    deadline: MigrationDeadline,
) -> Result<PublisherSession> {
    let mut attempt = 0u32;
    // 期限切れでループを抜けたときの最後の失敗。ループ本体は成功か接続失敗で return するか、
    // 失敗を記録してから break するため、ループを抜けた時点で必ず設定されている
    let mut last_error: Option<Error>;
    loop {
        // 接続の失敗は connect_with_retry が期限まで再試行し尽くしている。同じ条件で
        // ここからやり直しても成功しえないため、その失敗はそのまま返す
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
        match published {
            Ok(Ok((video_request_id, audio_request_id, catalog_request_id))) => {
                return Ok(PublisherSession {
                    client,
                    data_plane,
                    video_request_id,
                    audio_request_id,
                    catalog_request_id,
                });
            }
            Ok(Err(e)) => {
                // 確立済みの session を閉じてから、期限が残っていれば接続からやり直す
                close_session_after_failed_publish(&mut client).await;
                if !migration_publish_failure_is_retryable(&e) {
                    return Err(e);
                }
                tracing::warn!("Publishing tracks on the new session failed: {e}");
                last_error = Some(e);
            }
            Err(_) => {
                // 期限切れ。途中まで確立した session を閉じてから、残り時間があればやり直す
                close_session_after_failed_publish(&mut client).await;
                tracing::warn!("The migration deadline expired while publishing tracks");
                last_error = Some(Error::Other(
                    "the migration deadline expired while publishing tracks".to_string(),
                ));
            }
        }
        // 期限が残っている間だけ、指数バックオフを挟んで接続からやり直す
        let Some(delay) = migration_retry_delay(attempt, deadline.remaining()) else {
            break;
        };
        tracing::info!(
            "Retrying the migration from a new connection in {delay:?} (retry={})",
            attempt.saturating_add(1)
        );
        tokio::time::sleep(delay).await;
        attempt = attempt.saturating_add(1);
    }
    // 期限切れ。最後の失敗を返す
    match last_error {
        Some(error) => Err(error),
        None => Err(Error::Other(
            "the migration deadline expired before the session was established".to_string(),
        )),
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
            VideoCodec::Vp8 => encoder::VideoEncoder::Vp8(encoder::vpx::VpxEncoder::new(
                encoder::vpx::VpxCodec::Vp8,
                encode_width,
                encode_height,
                encode_fps,
                config.bitrate,
                keyframe_interval_frames,
            )?),
            VideoCodec::Vp9 => encoder::VideoEncoder::Vp9(encoder::vpx::VpxEncoder::new(
                encoder::vpx::VpxCodec::Vp9,
                encode_width,
                encode_height,
                encode_fps,
                config.bitrate,
                keyframe_interval_frames,
            )?),
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
    // カタログの組み立てに使う入力を保持する。GOAWAY 移行で移行先の c4m が変わったときに
    // 同じ入力から authInfo を載せ替えるため、エンコーダ / MP4 リーダーではなく値を所有する
    let catalog_context = CatalogContext {
        namespace_text,
        track_name: config.track_name.clone(),
        // 音声と映像を同じ時間軸で再生するため、両トラックへ同じ値を載せる
        sync: catalog::CatalogSyncParams {
            render_group: CATALOG_RENDER_GROUP,
            target_latency_ms: u64::from(config.target_latency_ms),
        },
        video: match (&mp4_reader, video_encoder.as_ref()) {
            (Some(reader), _) => {
                let info = reader.info();
                Some(CatalogVideoTrack {
                    codec: info.codec.clone(),
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
            (None, Some(enc)) => Some(CatalogVideoTrack {
                codec: enc.catalog_codec_string().to_string(),
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
        audio: audio_encoder.as_ref().map(|enc| CatalogAudioTrack {
            codec: enc.catalog_codec_string().to_string(),
            samplerate: AUDIO_SAMPLE_RATE,
            channel_config: AUDIO_CHANNEL_CONFIG.to_string(),
            bitrate: config.audio_bitrate,
        }),
    };
    // `c4m` が指定された配信は、映像 / 音声の track に authInfo を載せる (§5.2.42)
    let mut catalog_json = catalog::send_catalog(catalog_params(
        &catalog_context,
        &handle,
        &data_plane,
        catalog_request_id,
        catalog_group_id,
        client.subscription_filter_start(catalog_request_id),
        &config.url.c4m_tokens,
    ))
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
    // 実マイクのキャプチャは、実行中のセッションを表すガードが AudioCapture を借用するため、
    // AudioCapture とガードを別々のローカル変数として保持する。宣言と逆順に drop されるので、
    // キャプチャを止めるガードの drop が AudioCapture の drop より先に起きる。
    #[expect(
        unused_assignments,
        reason = "実キャプチャを使わない経路では初期値の None を読まないため"
    )]
    let mut audio_capture: Option<shiguredo_audio_device::AudioCapture> = None;
    let mut _audio_capture_session: Option<shiguredo_audio_device::AudioCaptureSession<'_>> = None;
    // 疑似音声キャプチャの寿命管理 (読み出さないが drop されるまで保持する)
    let _fake_audio_capture: Option<fake_audio_capture::FakeAudioCapture>;
    let _video_capture: Option<VideoCaptureGuard>;
    if let Some(reader) = reencode_reader {
        // MP4 再エンコード: リーダーが映像と音声の両方を供給する
        let video_input_tx = config.video_enabled.then_some(video_input_tx);
        let audio_input_tx = config.audio_enabled.then_some(audio_input_tx);
        reencode_source = Some(reader.start(video_input_tx, audio_input_tx)?);
        _video_capture = None;
        _fake_audio_capture = None;
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

        if config.audio_enabled {
            if config.fake_capture_device {
                _fake_audio_capture = Some(fake_audio_capture::start_capture(audio_input_tx)?);
            } else {
                _fake_audio_capture = None;
                audio_capture = Some(audio_capture::new_capture(&config, audio_input_tx)?);
                let capture = audio_capture
                    .as_mut()
                    .expect("audio capture is set just above");
                _audio_capture_session = Some(capture.start()?);
                tracing::info!("Audio capture started (48000 Hz, 1ch)");
            }
        } else {
            drop(audio_input_tx);
            _fake_audio_capture = None;
        }
    };

    // 7. データループ
    let start = Instant::now();
    // 映像 / 音声の Group ID も 0 に固定しない (draft-ietf-moq-msf-01 §6.1 (Group numbering))。
    // 再起動時に以前の Group ID を下回らないよう、カタログと同じ規則で払い出す。
    let mut video_group_id: u64 = group_id::allocate_initial_group_id()?;
    // 直前に開始した映像 Group の ID。NEW_GROUP_REQUEST (§9.19.19) の値が未来の Group を
    // 指しているかを判定するために使う
    let mut started_video_group_id: Option<u64> = None;
    let mut audio_group_id: u64 = group_id::allocate_initial_group_id()?;
    let mut audio_frame_count: u64 = 0;
    let mut current_video_writer: Option<SubgroupWriter> = None;
    // 受信 SUBSCRIBE で確立した購読の送出状態 (draft-ietf-moq-transport-23 §3.1)。
    // Track Alias は PUBLISH の固定値 (0 / 1 / 2) の次から採番する
    let mut peer_subscriptions: Vec<PeerSubscription> = Vec::new();
    let mut next_peer_alias: u64 = AUDIO_TRACK_ALIAS + 1;
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
    // 決めるために使う (draft-ietf-moq-transport-23 §3.2 (Fetch))。GOAWAY で session を移行しても
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
    // 進行中の移行タスク。run の終了時に打ち切る
    let mut migration_task: Option<tokio::task::JoinHandle<()>> = None;
    // request stream の GOAWAY で別 URL の session へ PUBLISH を移す side session から
    // イベントを受け取るチャネル。送信側 (`request_migration_tx`) は main ループが保持し
    // 続けるため、受信は常に待てる
    let (request_migration_tx, mut request_migration_rx) =
        mpsc::channel::<RequestMigrationEvent>(REQUEST_MIGRATION_EVENT_CHANNEL_SIZE);
    // 進行中の request 単位移行 side session のタスク。run の終了時に打ち切る
    let mut request_migration_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    // 移行先 URL ごとの side session。同じ移行先 URL を指す複数の PUBLISH は 1 本の
    // side session を共有し、2 本目以降は slot が持つチャネルへ PUBLISH の再発行を依頼する
    let mut request_migration_sessions: HashMap<String, SideSessionSlot> = HashMap::new();
    // 確立処理中の track。同じ track の移行を二重に開始しないために持つ
    let mut migrating_tracks: Vec<PublishTrack> = Vec::new();
    // request 単位移行で side session へ PUBLISH を移した track の送出先。移行していない
    // track は `None` であり、main ループが持つ現在の session の handle / data plane を使う
    let mut video_publish_target: Option<PublishTarget> = None;
    let mut audio_publish_target: Option<PublishTarget> = None;
    let mut catalog_publish_target: Option<PublishTarget> = None;
    // 移行後に旧 session を終了するタスク。run の終了時に打ち切る
    let mut old_session_closers: Vec<tokio::task::JoinHandle<()>> = Vec::new();
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
                            // request 単位移行で side session へ移した track は、その session
                            // の handle / data plane で Object を送る ([`publish_sink`])
                            let (video_handle, video_data_plane) = publish_sink(
                                video_publish_target.as_ref(),
                                &handle,
                                &data_plane,
                            );
                            // 映像は 1 group = 1 unidirectional stream で送る
                            // (draft-ietf-moq-loc-05 §4.2)。datagram 配送は音声トラックだけに
                            // 指定でき、キーフレーム (IDR) が 1 QUIC datagram に収まらない
                            // 映像では使わない。
                            if let Some(writer) = current_video_writer.take() {
                                writer
                                    .finish(publish_start_location(
                                        &client,
                                        video_publish_target.as_ref(),
                                        video_request_id,
                                    ))
                                    .await?;
                            }
                            // 受信 SUBSCRIBE の購読 (video) の writer も同じ Group 境界で
                            // 閉じて開き直す (受信 SUBSCRIBE は次の Group のキーフレームの
                            // 先頭から配信するため。Group の途中で受理した購読は次の Group
                            // から届く)。PUBLISH 済みの購読と同じ Object を送る
                            // (draft-ietf-moq-transport-23 §3.1 の MUST)
                            for peer in peer_subscriptions.iter_mut() {
                                if peer.track != PublishedTrack::Video {
                                    continue;
                                }
                                if let Some(writer) = peer.writer.take()
                                    && let Err(e) = writer
                                        .finish(
                                            client.subscription_filter_start(peer.request_id),
                                        )
                                        .await
                                {
                                    tracing::debug!(
                                        "Failed to finish a peer video stream: request_id={}, {e}",
                                        peer.request_id
                                    );
                                }
                                if !peer.ended {
                                    let writer = SubgroupWriter::new(
                                        &handle,
                                        &data_plane,
                                        peer.request_id,
                                        peer.alias,
                                        video_group_id,
                                        DEFAULT_PUBLISHER_PRIORITY,
                                        true,
                                        client.subscription_filter_start(peer.request_id),
                                    )
                                    .await?;
                                    peer.writer = Some(writer);
                                }
                            }
                            // has_properties は SUBGROUP_HEADER の PROPERTIES ビットに対応し、
                            // ヘッダと全オブジェクトで一致が必須 (draft-ietf-moq-transport-23 §11.3.1 (Subgroup Header))
                            // 映像ストリームには毎フレーム LOC プロパティを付与するため true を渡す
                            current_video_writer = Some(
                                SubgroupWriter::new(
                                    video_handle,
                                    video_data_plane,
                                    video_request_id,
                                    VIDEO_TRACK_ALIAS,
                                    video_group_id,
                                    DEFAULT_PUBLISHER_PRIORITY,
                                    true,
                                    publish_start_location(
                                        &client,
                                        video_publish_target.as_ref(),
                                        video_request_id,
                                    ),
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
                        // 受信 SUBSCRIBE の購読にも同じ Object を送る
                        // (draft-ietf-moq-transport-23 §3.1 の "the publisher MUST send the
                        // Object once for each matching subscription")。フィルタと PAUSED は
                        // Session が subscription ごとに評価するため、Pass の成否は送信側の
                        // 状態に使わない (media_cache / 送信統計は PUBLISH 購読の writer の
                        // Pass を基準にする)。購読が reset された stream だけを捨てる
                        for peer in peer_subscriptions.iter_mut() {
                            if peer.track != PublishedTrack::Video || peer.ended {
                                continue;
                            }
                            let Some(writer) = peer.writer.as_mut() else {
                                continue;
                            };
                            if let Err(e) = writer.write_object(&ef.data, &properties).await {
                                if is_peer_stream_reset(&e) {
                                    tracing::info!(
                                        "Peer video stream was reset by peer; dropping the subscription: request_id={}",
                                        peer.request_id
                                    );
                                    peer.writer = None;
                                    peer.ended = true;
                                } else {
                                    return Err(e);
                                }
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
                        // Subgroup の再オープンは購読を再開する REQUEST_UPDATE (PAUSED 1→0) の受理まで
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
                        // request 単位移行で side session へ移した track は、その session の
                        // handle / data plane で Object を送る ([`publish_sink`])
                        let (audio_handle, audio_data_plane) = publish_sink(
                            audio_publish_target.as_ref(),
                            &handle,
                            &data_plane,
                        );
                        if config.audio_datagram {
                            let mut writer = DatagramWriter::new(
                                audio_handle,
                                audio_data_plane,
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
                                audio_handle,
                                audio_data_plane,
                                audio_request_id,
                                AUDIO_TRACK_ALIAS,
                                audio_group_id,
                                DEFAULT_PUBLISHER_PRIORITY,
                                true,
                                publish_start_location(
                                    &client,
                                    audio_publish_target.as_ref(),
                                    audio_request_id,
                                ),
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
                                .finish(publish_start_location(
                                    &client,
                                    audio_publish_target.as_ref(),
                                    audio_request_id,
                                ))
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
                        // request 単位移行で side session へ移した track は、その session の
                        // handle / data plane で Object を送る ([`publish_sink`])
                        let (audio_handle, audio_data_plane) = publish_sink(
                            audio_publish_target.as_ref(),
                            &handle,
                            &data_plane,
                        );
                        // LOC draft-ietf-moq-loc-05 §4.1 (Application with one audio track): 1 audio frame = 1 Object = 1 Group
                        if config.audio_datagram {
                            // datagram の送信は完了が返らない (fire-and-forget)。書き込みの
                            // 戻りを完了とみなすため、投入の直前の時刻を控える
                            let queued_wall_clock_ms = wall_clock_now_us()? / 1_000;
                            let mut writer = DatagramWriter::new(
                                audio_handle,
                                audio_data_plane,
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
                            // ヘッダと全オブジェクトで一致が必須 (draft-ietf-moq-transport-23 §11.3.1 (Subgroup Header))
                            // 音声ストリームには毎フレーム LOC プロパティを付与するため true を渡す
                            let audio_request_id =
                                audio_request_id.expect("audio request_id enabled");
                            // ストリームの生成と書き込みの待ち (backpressure) を含めて測る
                            let queued_wall_clock_ms = wall_clock_now_us()? / 1_000;
                            let mut writer = SubgroupWriter::new(
                                audio_handle,
                                audio_data_plane,
                                audio_request_id,
                                AUDIO_TRACK_ALIAS,
                                audio_group_id,
                                DEFAULT_PUBLISHER_PRIORITY,
                                true,
                                publish_start_location(
                                    &client,
                                    audio_publish_target.as_ref(),
                                    audio_request_id,
                                ),
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
                                .finish(publish_start_location(
                                    &client,
                                    audio_publish_target.as_ref(),
                                    audio_request_id,
                                ))
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
                        // 受信 SUBSCRIBE の購読 (音声) にも同じ Object を送る
                        // (draft-ietf-moq-transport-23 §3.1 の MUST)。フィルタと PAUSED は
                        // Session が subscription ごとに評価する。Audio Config は購読ごとの
                        // 最初の Object に付与する (`audio_config_sent` を購読ごとに持つ)。
                        // datagram モードの購読は Object ごとに writer を作り、subgroup
                        // モードの購読は 1 object = 1 stream で PUBLISH 側と同じ形で送る。
                        // 送信統計 (`record_audio_send`) は PUBLISH 購読の Pass を基準に
                        // するためここでは行わない
                        for peer in peer_subscriptions.iter_mut() {
                            if peer.track != PublishedTrack::Audio || peer.ended {
                                continue;
                            }
                            let audio_config_for_peer = if peer.audio_config_sent {
                                None
                            } else {
                                audio_config
                            };
                            let peer_properties = build_audio_loc_properties(
                                timestamp,
                                audio_timescale,
                                audio_config_for_peer,
                            );
                            if config.audio_datagram {
                                let mut writer = DatagramWriter::new(
                                    &handle,
                                    &data_plane,
                                    peer.request_id,
                                    peer.alias,
                                    audio_group_id,
                                    DEFAULT_PUBLISHER_PRIORITY,
                                    config.datagram_max_size,
                                    config.audio_redundancy,
                                );
                                let outcome =
                                    writer.write_object(&encoded, &peer_properties).await;
                                match outcome {
                                    Ok(outcome) => {
                                        if outcome == ObjectFilterOutcome::Pass {
                                            peer.audio_config_sent = true;
                                        }
                                    }
                                    Err(e) => {
                                        if is_peer_stream_reset(&e) {
                                            tracing::info!(
                                                "Peer audio datagram was reset by peer; dropping the subscription: request_id={}",
                                                peer.request_id
                                            );
                                            peer.ended = true;
                                        } else {
                                            return Err(e);
                                        }
                                    }
                                }
                            } else {
                                let writer_result = SubgroupWriter::new(
                                    &handle,
                                    &data_plane,
                                    peer.request_id,
                                    peer.alias,
                                    audio_group_id,
                                    DEFAULT_PUBLISHER_PRIORITY,
                                    true,
                                    client.subscription_filter_start(peer.request_id),
                                )
                                .await;
                                match writer_result {
                                    Ok(mut writer) => {
                                        let (write_error, write_passed) =
                                            match writer
                                                .write_object(&encoded, &peer_properties)
                                                .await
                                            {
                                                Ok(outcome) => {
                                                    (
                                                        None,
                                                        outcome == ObjectFilterOutcome::Pass,
                                                    )
                                                }
                                                Err(e) => (Some(e), false),
                                            };
                                        let finish_error = writer
                                            .finish(client.subscription_filter_start(peer.request_id))
                                            .await
                                            .err();
                                        if let Some(e) = write_error.or(finish_error)
                                        {
                                            if is_peer_stream_reset(&e) {
                                                tracing::info!(
                                                    "Peer audio stream was reset by peer; dropping the subscription: request_id={}",
                                                    peer.request_id
                                                );
                                                peer.ended = true;
                                            } else {
                                                return Err(e);
                                            }
                                        } else if write_passed {
                                            peer.audio_config_sent = true;
                                        }
                                    }
                                    Err(e) => {
                                        if is_transport_session_end(&e) {
                                            return Err(e);
                                        }
                                        tracing::warn!(
                                            "Failed to open a peer audio stream: request_id={}, {e}",
                                            peer.request_id
                                        );
                                    }
                                }
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
                                    match goaway_action(
                                        target.scope,
                                        tokio_moq::same_session_url(&target.url, &config.url),
                                    ) {
                                        GoawayAction::RepublishRequest(request_id) => {
                                            // draft-ietf-moq-transport-23 §9.2 (GOAWAY): 空の
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
                                            // request 単位移行で side session へ PUBLISH を移した
                                            // track は、main ループが持つ request id が side
                                            // session のものになっている。旧 session の GOAWAY
                                            // は移行前の request id で届くため、移行済みの
                                            // track は `PublishTarget` が保持する旧 id で照合する
                                            let kind = publish_track_for_main_goaway(
                                                request_id,
                                                video_request_id,
                                                audio_request_id,
                                                catalog_request_id,
                                                video_publish_target.as_ref(),
                                                audio_publish_target.as_ref(),
                                                catalog_publish_target.as_ref(),
                                            );
                                            if track_migrated(
                                                kind,
                                                video_publish_target.is_some(),
                                                audio_publish_target.is_some(),
                                                catalog_publish_target.is_some(),
                                            ) {
                                                tracing::info!(
                                                    "Ignoring a request-scoped GOAWAY for a track that was migrated to another session: request_id={request_id}, kind={kind:?}"
                                                );
                                                return Ok(false);
                                            }
                                            // 移行の確立中は旧 session で当該 PUBLISH を
                                            // 再発行しない (同じ track を 2 か所で PUBLISH しない)
                                            if migrating_tracks.contains(&kind) {
                                                tracing::info!(
                                                    "Ignoring a request-scoped GOAWAY for a track that is being migrated to another session: request_id={request_id}, kind={kind:?}"
                                                );
                                                return Ok(false);
                                            }
                                            match republish_request_on_current_session(
                                                &mut client,
                                                &config,
                                                &namespace,
                                                request_id,
                                                kind,
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
                                                    let sink = catalog_sink(
                                                        &client,
                                                        catalog_publish_target.as_ref(),
                                                        &handle,
                                                        &data_plane,
                                                        catalog_request_id,
                                                    );
                                                    if let Err(e) =
                                                        republish_catalog_at_next_group(
                                                            &client,
                                                            sink,
                                                            &handle,
                                                            &data_plane,
                                                            catalog_request_id,
                                                            &catalog_json,
                                                            &mut catalog_group_id,
                                                            &mut catalog_location,
                                                            &peer_subscriptions,
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
                                        GoawayAction::MigrateRequest(request_id) => {
                                            // draft-ietf-moq-transport-23 §9.2 (GOAWAY): request
                                            // stream の GOAWAY の New Session URI が別の URL を
                                            // 指す場合、当該 request だけをその session で再発行
                                            // する。この example の main ループは 1 つの
                                            // `MoqtClient` しか所有できないため、専用タスクが
                                            // side session を確立して PUBLISH を張り、main ループ
                                            // がその session へ Object を送る (§9.2)。同じ移行先
                                            // URL を指す複数の PUBLISH は 1 本の side session を
                                            // 共有する
                                            if migration_rx.is_some() {
                                                // session 全体の移行が進行中なら、その移行先で
                                                // 全 track を張り直す。drain 中の session で
                                                // side session を作らない
                                                tracing::info!(
                                                    "Ignoring a request-scoped GOAWAY with a new session URI while a session migration is in progress: request_id={request_id}"
                                                );
                                                return Ok(false);
                                            }
                                            let kind = publish_track_for_main_goaway(
                                                request_id,
                                                video_request_id,
                                                audio_request_id,
                                                catalog_request_id,
                                                video_publish_target.as_ref(),
                                                audio_publish_target.as_ref(),
                                                catalog_publish_target.as_ref(),
                                            );
                                            if kind == PublishTrack::Unknown {
                                                tracing::warn!(
                                                    "Ignoring a request-scoped GOAWAY with a new session URI for a request that is not a publisher request: request_id={request_id}"
                                                );
                                                return Ok(false);
                                            }
                                            if track_migrated(
                                                kind,
                                                video_publish_target.is_some(),
                                                audio_publish_target.is_some(),
                                                catalog_publish_target.is_some(),
                                            ) {
                                                tracing::info!(
                                                    "Ignoring a request-scoped GOAWAY for a track that was already migrated to another session: request_id={request_id}, kind={kind:?}"
                                                );
                                                return Ok(false);
                                            }
                                            if migrating_tracks.contains(&kind) {
                                                // 移行の確立中に 2 回目の GOAWAY が来ても二重に
                                                // 開始しない
                                                tracing::info!(
                                                    "Ignoring a request-scoped GOAWAY for a track that is already being migrated: request_id={request_id}, kind={kind:?}"
                                                );
                                                return Ok(false);
                                            }
                                            // 移行先 URI の msf fragment の connection が
                                            // 現在の接続経路と矛盾する場合は移行しない
                                            // (draft-ietf-moq-msf-01 §11.1.1)
                                            if let Err(e) = tokio_moq::resolve_transport(
                                                Some(config.transport),
                                                &target.url,
                                            ) {
                                                tracing::warn!(
                                                    "GOAWAY migration target conflicts with the current transport: {e}"
                                                );
                                                return Ok(false);
                                            }
                                            // 期限は GOAWAY の Timeout (0 はローカル既定値)。
                                            // 接続は期限まで再試行されるが、PUBLISH の再発行は
                                            // 1 回だけ試す
                                            let deadline = MigrationDeadline::after_goaway(
                                                Instant::now(),
                                                timeout,
                                            );
                                            let command = SideSessionCommand::Publish {
                                                kind,
                                                old_request_id: request_id,
                                                deadline,
                                            };
                                            // 同じ移行先 URL の side session が既にあれば、その
                                            // session で PUBLISH を再発行する。1 本の side session
                                            // を同じ URL を指す複数の track で共有する
                                            let key = side_session_key(&target.url);
                                            let existing = request_migration_sessions
                                                .get(&key)
                                                .map(|slot| slot.commands.clone());
                                            if let Some(commands) = existing {
                                                // 受信側のタスクが既に終了している場合は送信に
                                                // 失敗する。Ended の到着を待たずに終了経路へ進む
                                                if commands.send(command).await.is_err() {
                                                    tracing::warn!(
                                                        "Failed to send a PUBLISH migration command to the existing side session: authority={}, kind={kind:?}",
                                                        target.url.authority
                                                    );
                                                    request_migration_sessions.remove(&key);
                                                    return Ok(true);
                                                }
                                                tracing::info!(
                                                    "Requested the {kind:?} PUBLISH migration on the existing side session: authority={}, request_id={request_id}",
                                                    target.url.authority
                                                );
                                            } else {
                                                tracing::info!(
                                                    "Starting a background request migration to {}: kind={kind:?}, request_id={request_id}, timeout={timeout}ms, local_default={}",
                                                    target.url.authority,
                                                    deadline.uses_local_default(),
                                                );
                                                let (task, commands) = spawn_request_migration(
                                                    &task_monitor,
                                                    RequestMigrationRequest {
                                                        url: target.url,
                                                        config: config.clone(),
                                                        namespace: namespace.clone(),
                                                    },
                                                    command,
                                                    request_migration_tx.clone(),
                                                );
                                                request_migration_tasks.push(task);
                                                request_migration_sessions.insert(
                                                    key,
                                                    SideSessionSlot { commands },
                                                );
                                            }
                                            migrating_tracks.push(kind);
                                            // 新 session の PUBLISH が成立するまで旧 session の
                                            // 配信を止めない (§7.7.1)
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
                                            // draft-ietf-moq-transport-23 §6.6.1 (Graceful Session
                                            // Migration) / §9.2 (GOAWAY) / §7.7.1 (Graceful Publisher
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
                                            let (task, receiver) = spawn_migration_task(
                                                config.clone(),
                                                namespace.clone(),
                                                target.url,
                                                deadline,
                                                task_monitor.clone(),
                                            );
                                            migration_task = Some(task);
                                            migration_rx = Some(receiver);
                                            // 新 session の PUBLISH が成立するまで旧 session の配信を
                                            // 止めない (§7.7.1)
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
                            reliable_size,
                            tracking,
                        })) => {
                            // I/O 層 (この example) が当該 uni data stream を RESET_STREAM で閉じ、
                            // その後に Session へ終端を通知する。通知が保留 PUBLISH_DONE の
                            // flush 条件 (全 outgoing stream の終端) を満たす
                            // (draft-ietf-moq-transport-23 §5.2 (Delivery Timeouts and Data Reliability))。
                            reset_open_subgroup_stream(
                                &mut current_video_writer,
                                stream_id,
                                error_code,
                                reliable_size,
                                tracking,
                            )
                            .await
                        }
                        Some(ClientEvent::Request(request)) => {
                            let accepted = serve_peer_request(
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
                                &mut next_peer_alias,
                            )
                            .await?;
                            if let Some(accepted) = accepted {
                                peer_subscriptions.push(PeerSubscription {
                                    request_id: accepted.request_id,
                                    track: accepted.track,
                                    alias: accepted.alias,
                                    writer: None,
                                    audio_config_sent: false,
                                    ended: false,
                                });
                                // カタログの購読は、受理した時点で現在のカタログ Object を
                                // 新しい Group で送り直す (draft-ietf-moq-msf-01 §5: 独立した
                                // カタログは新しい Group の先頭に置く MUST)。次の定周期の
                                // republish を待つと購読直後の相手にカタログが届かない
                                if accepted.track == PublishedTrack::Catalog
                                    && let Err(e) = republish_catalog_at_next_group(
                                        &client,
                                        catalog_sink(
                                            &client,
                                            catalog_publish_target.as_ref(),
                                            &handle,
                                            &data_plane,
                                            catalog_request_id,
                                        ),
                                        &handle,
                                        &data_plane,
                                        catalog_request_id,
                                        &catalog_json,
                                        &mut catalog_group_id,
                                        &mut catalog_location,
                                        &peer_subscriptions,
                                    )
                                    .await
                                {
                                    tracing::warn!(
                                        "Failed to send the catalog to a newly accepted peer subscription: {e}"
                                    );
                                }
                            }
                            Ok(false)
                        }
                        Some(ClientEvent::Session(SessionEvent::OpenFillFetchStream {
                            request_id,
                            subscription_request_id,
                            start,
                            end,
                        })) => {
                            // draft-ietf-moq-transport-23 §3.4 (Fill Semantics) / §3.4.1:
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
                            // draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE):
                            // 受信側は必ず 1 通の REQUEST_OK / REQUEST_ERROR で応答する MUST。
                            // PAUSED / LOCATION_FILTER / Range Filters の検証と購読状態への
                            // 反映は session 層が受信時に済ませているため、ここでは受理する。
                            tracing::info!(
                                "Received REQUEST_UPDATE: request_id={}",
                                update.request_id
                            );
                            // NEW_GROUP_REQUEST (§9.19.19) は新しい Group の開始
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
                        // draft-ietf-moq-transport-23 §7.7.1 (Graceful Publisher Relay Switchover):
                        // 新 session で PUBLISH が成立するまで旧 session の配信を止めない。
                        // ここに来た時点で PUBLISH は成立している
                        async {
                            // 旧 session の映像 writer を終端する (§11.3.2)。終端しないと
                            // open 中の outgoing stream が残り、旧 session の PUBLISH_DONE が
                            // 拒否される (§9.9)。filter start は差し替え前の client から取る
                            // (request 単位移行で side session へ移した track は、main ループ
                            // から購読状態を観測できないため `None` になる)
                            if let Some(writer) = current_video_writer.take() {
                                let start_location = video_request_id.and_then(|request_id| {
                                    publish_start_location(
                                        &client,
                                        video_publish_target.as_ref(),
                                        request_id,
                                    )
                                });
                                if let Err(e) = writer.finish(start_location).await {
                                    tracing::debug!(
                                        "Failed to finish the outgoing video subgroup stream during migration: {e}"
                                    );
                                }
                            }
                            // request 単位移行の side session が残っていれば止める。session
                            // 全体の移行はすべての track を新しい session で張り直すため、
                            // 同じ track が 2 つの session から送られないようにする
                            for task in request_migration_tasks.drain(..) {
                                task.abort();
                                let _ = task.await;
                                tracing::debug!(
                                    "Aborted the request migration task after a full session migration"
                                );
                            }
                            // 打ち切ったタスクが送信済みのイベントを捨てる。移行先の session の
                            // request id を古い side session のものへ差し替えないためである
                            while request_migration_rx.try_recv().is_ok() {}
                            // URL ごとの side session の slot も破棄する。全 track を新しい
                            // session で張り直すため、side session はもう使わない
                            request_migration_sessions.clear();
                            // request 単位移行で旧 session の PUBLISH を既に終端した track は、
                            // 旧 session の終了処理でも PUBLISH_DONE を送らない。main ループが
                            // 持つ request id は side session のものになっており、旧 session の
                            // 別の request を終端しうるためである
                            let video_was_migrated = video_publish_target.is_some();
                            let audio_was_migrated = audio_publish_target.is_some();
                            let catalog_was_migrated = catalog_publish_target.is_some();
                            // request 単位移行の送出先を戻す。以降は新しい session の
                            // handle / data plane を使う
                            video_publish_target = None;
                            audio_publish_target = None;
                            catalog_publish_target = None;
                            migrating_tracks.clear();
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
                            // 移行先の c4m が変わると catalog の track の authInfo を載せ替える
                            // 必要がある (draft-ietf-moq-msf-01 §5.2.42 (Authorization Info))。
                            // `config.url` を新しい URL に更新する前に比較する
                            if c4m_tokens_changed(&config.url.c4m_tokens, &url.c4m_tokens) {
                                match catalog_context.json(&url.c4m_tokens) {
                                    Ok(rebuilt) => {
                                        catalog_json = rebuilt;
                                        tracing::info!(
                                            "Rebuilt the MSF catalog for the migrated session's c4m tokens"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "Failed to rebuild the MSF catalog for the migrated session's c4m tokens: {e}"
                                        );
                                    }
                                }
                            }
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
                            // 旧 session の受信 SUBSCRIBE 購読の writer を閉じる
                            // (open 中の outgoing stream が残っていると旧 session の
                            // PUBLISH_DONE が拒否される。§9.9)。Session は入れ替わるため
                            // 受信 SUBSCRIBE の購読も旧 session で終端される
                            for peer in peer_subscriptions.iter_mut() {
                                peer.ended = true;
                                if let Some(writer) = peer.writer.take()
                                    && let Err(e) = writer
                                        .finish(
                                            old_client
                                                .subscription_filter_start(peer.request_id),
                                        )
                                        .await
                                {
                                    tracing::debug!(
                                        "Failed to finish a peer subscription stream during migration: request_id={}, {e}",
                                        peer.request_id
                                    );
                                }
                            }
                            let peer_request_ids: Vec<u64> = peer_subscriptions
                                .iter()
                                .map(|peer| peer.request_id)
                                .collect();
                            // 旧 session の終了は main ループを止めないよう別タスクに任せる。
                            // JoinHandle は run の終了時に打ち切るため保持する
                            old_session_closers.push(spawn_old_session_close(
                                &task_monitor,
                                old_client,
                                if video_was_migrated {
                                    None
                                } else {
                                    old_video_request_id
                                },
                                if audio_was_migrated {
                                    None
                                } else {
                                    old_audio_request_id
                                },
                                if catalog_was_migrated {
                                    None
                                } else {
                                    Some(old_catalog_request_id)
                                },
                                peer_request_ids,
                            ));
                            // 受信 SUBSCRIBE の購読は旧 session の PUBLISH_DONE で終端したため、
                            // 新 session では送出しない (移行後に再確立しない。購読者が必要なら
                            // 再度 SUBSCRIBE する)
                            peer_subscriptions.clear();
                            tracing::info!(
                                "Migrated the publisher session to {}",
                                config.url.authority
                            );
                            // 新しい session でカタログを次の Group で送り直す
                            // (draft-ietf-moq-msf-01 §5 (Catalog) / §6.1 (Group numbering))
                            if let Err(e) = republish_catalog_at_next_group(
                                &client,
                                catalog_sink(
                                    &client,
                                    catalog_publish_target.as_ref(),
                                    &handle,
                                    &data_plane,
                                    catalog_request_id,
                                ),
                                &handle,
                                &data_plane,
                                catalog_request_id,
                                &catalog_json,
                                &mut catalog_group_id,
                                &mut catalog_location,
                                &peer_subscriptions,
                            )
                            .await
                            {
                                tracing::warn!(
                                    "Failed to republish the catalog on the new session: {e}"
                                );
                            }
                        }
                        .await;
                        // 結果を受け取ったタスクは完了している。JoinHandle を手放してよい
                        migration_task = None;
                        migration_rx = None;
                    }
                    Some(Err(reason)) => {
                        tracing::warn!("Session migration failed: {reason}");
                        migration_task = None;
                        // 移行を諦め、既存の終了経路 (後始末) へ進む
                        session_ended = true;
                        break 'main;
                    }
                    None => {}
                }
            }
            Some(event) = request_migration_rx.recv() => {
                match event {
                    RequestMigrationEvent::Migrated {
                        url,
                        kind,
                        old_request_id,
                        new_request_id,
                        new_track_alias,
                        data_plane: migrated_data_plane,
                        handle: migrated_handle,
                    } => {
                        // side session の確立と PUBLISH の再発行に成功した。旧 session の
                        // 当該 PUBLISH を終端し、以降の Object は side session へ送る
                        // draft-ietf-moq-transport-23 §9.2 (GOAWAY) の "close the old
                        // request stream using the appropriate mechanism"
                        match kind {
                            PublishTrack::Video => {
                                // open 中の subgroup stream が残っていると PUBLISH_DONE が
                                // 拒否されるため (§9.9)、先に writer を終端する
                                if let Some(writer) = current_video_writer.take() {
                                    let start_location = video_request_id.and_then(|request_id| {
                                        client.subscription_filter_start(request_id)
                                    });
                                    if let Err(e) = writer.finish(start_location).await {
                                        tracing::debug!(
                                            "Failed to finish the outgoing video subgroup stream after a request migration: {e}"
                                        );
                                    }
                                }
                                send_publish_done_best_effort(&mut client, old_request_id, "video")
                                    .await;
                                video_request_id = Some(new_request_id);
                                // 新しい session では旧 Group を継続できないため、次の
                                // キーフレームで新しい Group を開始する。映像の writer は
                                // Group の先頭 (キーフレーム) でだけ作るため、ここでは
                                // 作らず、次のキーフレームで side session の handle /
                                // data plane から作り直す
                                gop_tracker.request_new_group();
                                video_publish_target = Some(PublishTarget {
                                    old_request_id,
                                    handle: migrated_handle,
                                    data_plane: migrated_data_plane,
                                });
                            }
                            PublishTrack::Audio => {
                                send_publish_done_best_effort(&mut client, old_request_id, "audio")
                                    .await;
                                audio_request_id = Some(new_request_id);
                                // 新しい session の最初の音声 Object に Audio Config を
                                // 載せ直す
                                audio_config_sent = false;
                                audio_publish_target = Some(PublishTarget {
                                    old_request_id,
                                    handle: migrated_handle,
                                    data_plane: migrated_data_plane,
                                });
                            }
                            PublishTrack::Catalog => {
                                send_publish_done_best_effort(
                                    &mut client,
                                    old_request_id,
                                    "catalog",
                                )
                                .await;
                                catalog_request_id = new_request_id;
                                catalog_publish_target = Some(PublishTarget {
                                    old_request_id,
                                    handle: migrated_handle,
                                    data_plane: migrated_data_plane,
                                });
                                // 新しい session でカタログを次の Group で送り直す
                                // (draft-ietf-moq-msf-01 §5 (Catalog) /
                                // §6.1 (Group numbering))
                                let sink = catalog_sink(
                                    &client,
                                    catalog_publish_target.as_ref(),
                                    &handle,
                                    &data_plane,
                                    catalog_request_id,
                                );
                                if let Err(e) = republish_catalog_at_next_group(
                                    &client,
                                    sink,
                                    &handle,
                                    &data_plane,
                                    catalog_request_id,
                                    &catalog_json,
                                    &mut catalog_group_id,
                                    &mut catalog_location,
                                    &peer_subscriptions,
                                )
                                .await
                                {
                                    tracing::warn!(
                                        "Failed to republish the catalog on the request migration side session: {e}"
                                    );
                                }
                            }
                            PublishTrack::Unknown => {}
                        }
                        migrating_tracks.retain(|track| *track != kind);
                        tracing::info!(
                            "Migrated the {kind:?} PUBLISH to another session: authority={}, old_request_id={old_request_id}, new_request_id={new_request_id}, track_alias={new_track_alias}",
                            url.authority
                        );
                    }
                    RequestMigrationEvent::Ended { url } => {
                        // side session を共有するすべての track が送れなくなる。対象 URL の
                        // slot を削除し、パイプラインを終了する。`*_publish_target` を
                        // `None` にすると「未移行」の意味になり、main session の別 request
                        // の id で送信してしまうため、control GOAWAY の移行失敗と同じく
                        // パイプラインを終了する扱いにする
                        request_migration_sessions.remove(&side_session_key(&url));
                        tracing::warn!(
                            "The request migration side session ended: authority={}; ending the pipeline",
                            url.authority
                        );
                        session_ended = true;
                        break 'main;
                    }
                    RequestMigrationEvent::Failed {
                        url,
                        kind,
                        old_request_id,
                        reason,
                    } => {
                        // 1 本の PUBLISH の再発行失敗である。control GOAWAY の移行失敗と
                        // 同じく警告を出し、旧 session の終了経路へ進む
                        tracing::warn!(
                            "Failed to migrate the {kind:?} PUBLISH to another session: authority={}, request_id={old_request_id}: {reason}",
                            url.authority
                        );
                        session_ended = true;
                        break 'main;
                    }
                    RequestMigrationEvent::SessionEvent { url, event } => {
                        // 移行済みの PUBLISH について side session で観測したイベントを
                        // ログに残す。side session の後始末は main ループが所有する
                        // `MoqtClient` では行えないため、記録だけに留める
                        match event {
                            SessionEvent::PublishDoneReceived {
                                request_id,
                                status_code,
                                stream_count,
                                reason,
                            } => {
                                tracing::info!(
                                    "Received PUBLISH_DONE on the request migration side session: authority={}, request_id={request_id}, status_code={status_code}, stream_count={stream_count}, reason={}",
                                    url.authority,
                                    reason.as_str(),
                                );
                            }
                            SessionEvent::RequestErrorReceived {
                                request_id,
                                error_code,
                                retry_interval,
                                reason,
                                ..
                            } => {
                                tracing::warn!(
                                    "Received REQUEST_ERROR on the request migration side session: authority={}, request_id={request_id}, error_code={error_code:#x}, retry_interval={retry_interval}, reason={}",
                                    url.authority,
                                    reason.as_str(),
                                );
                            }
                            SessionEvent::GoawayReceived {
                                new_session_uri,
                                timeout,
                                on_request_stream,
                            } => {
                                // side session の GOAWAY はさらに移行しない。移行対象の
                                // 購読は relay が管理するため、ログだけ残す
                                tracing::warn!(
                                    "Received GOAWAY on the request migration side session: authority={}, timeout={timeout}, on_request_stream={on_request_stream:?}, new_session_uri_len={}",
                                    url.authority,
                                    new_session_uri.len(),
                                );
                            }
                            SessionEvent::CloseSession(err) => {
                                tracing::warn!(
                                    "The request migration side session closed: authority={}, {:#x} {}",
                                    url.authority,
                                    err.code,
                                    err.reason,
                                );
                            }
                            other => {
                                tracing::debug!(
                                    "Ignoring a session event on the request migration side session: authority={}, event={other:?}",
                                    url.authority,
                                );
                            }
                        }
                    }
                }
            }
            wt_state = wait_wt_session_change(&mut wt_session_state_rx) => {
                match wt_state {
                    // 通常の利用状態。次の変化を待つ
                    Some(WtSessionState::Active) => {}
                    Some(WtSessionState::Draining) => {
                        // draft-ietf-webtrans-http3-16 §4.7 / draft-ietf-webtrans-http2-16 §6.13:
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
                            let (task, receiver) = spawn_migration_task(
                                config.clone(),
                                namespace.clone(),
                                target_url,
                                deadline,
                                task_monitor.clone(),
                            );
                            migration_task = Some(task);
                            migration_rx = Some(receiver);
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
                    catalog_sink(
                        &client,
                        catalog_publish_target.as_ref(),
                        &handle,
                        &data_plane,
                        catalog_request_id,
                    ),
                    &handle,
                    &data_plane,
                    catalog_request_id,
                    &catalog_json,
                    &mut catalog_group_id,
                    &mut catalog_location,
                    &peer_subscriptions,
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

    // 進行中の移行タスクを打ち切って回収する。確立済みの移行先 session は `MoqtClient` の
    // drop で transport が閉じる。run 自体の終了が先に決まった場合であり異常ではない
    if let Some(handle) = migration_task.take() {
        handle.abort();
        let _ = handle.await;
        tracing::debug!("Aborted the in-flight migration task");
    }
    // 旧 session の終了タスクは best-effort である。run の終了を待たせないよう打ち切って
    // 回収する (旧 session の transport は `MoqtClient` の drop で閉じる)
    for handle in old_session_closers.drain(..) {
        handle.abort();
        let _ = handle.await;
        tracing::debug!("Aborted the old session close task");
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
        // (draft-ietf-moq-transport-23 §9.9 の MUST NOT)。セッション終了を観測していない経路
        // (peer の cancel 後に Ctrl+C / 入力終了でループを抜けた場合) では warn になる。
        // 追跡の終端は cancel の観測と終端通知の到着順に依存するため、本 example では扱わない
        if let Err(e) = writer
            .finish(video_request_id.and_then(|rid| {
                publish_start_location(&client, video_publish_target.as_ref(), rid)
            }))
            .await
        {
            log_failure(
                format_args!("Failed to finish video subgroup stream: {e}"),
                cleanup_finish_is_expected(session_ended, &e),
            );
        }
    }

    // PUBLISH_DONE を各 request に送信してストリームを閉じる。request 単位移行で side
    // session へ PUBLISH を移した track は、旧 session の PUBLISH を移行時に終端済みであり、
    // main ループが持つ request id は side session のものになっている。旧 session の
    // `client` にその id で送ると別の request を終端しうるため、移行済みの track には送らない
    if video_publish_target.is_none()
        && let Some(rid) = video_request_id
        && let Err(e) = client
            .send_publish_done(rid, PUBLISH_DONE_GOING_AWAY, "")
            .await
    {
        log_failure(
            format_args!("Failed to send PUBLISH_DONE for video: {e}"),
            session_ended || is_connection_closed(&e),
        );
    }
    if audio_publish_target.is_none()
        && let Some(rid) = audio_request_id
        && let Err(e) = client
            .send_publish_done(rid, PUBLISH_DONE_GOING_AWAY, "")
            .await
    {
        log_failure(
            format_args!("Failed to send PUBLISH_DONE for audio: {e}"),
            session_ended || is_connection_closed(&e),
        );
    }
    if catalog_publish_target.is_none()
        && let Err(e) = client
            .send_publish_done(catalog_request_id, PUBLISH_DONE_GOING_AWAY, "")
            .await
    {
        log_failure(
            format_args!("Failed to send PUBLISH_DONE for catalog: {e}"),
            session_ended || is_connection_closed(&e),
        );
    }

    // 受信 SUBSCRIBE で確立した購読の writer を閉じ、PUBLISH_DONE を送る
    // (draft-ietf-moq-transport-23 §6.4.2.2 の MUST / §9.9 の MUST NOT。終端済みの購読
    // (`ended`) には送らない)
    for peer in peer_subscriptions.iter_mut() {
        if peer.ended {
            continue;
        }
        peer.ended = true;
        if let Some(writer) = peer.writer.take()
            && let Err(e) = writer
                .finish(client.subscription_filter_start(peer.request_id))
                .await
        {
            // 終端に伴う失敗と接続クローズだけを info に落とす (`cleanup_finish_is_expected`
            // と `Error::ConnectionClosed` の判定)
            log_failure(
                format_args!(
                    "Failed to finish peer subscription stream (request_id={}): {e}",
                    peer.request_id
                ),
                session_ended || cleanup_finish_is_expected(session_ended, &e),
            );
        }
        if let Err(e) = client
            .send_publish_done(peer.request_id, PUBLISH_DONE_GOING_AWAY, "")
            .await
        {
            log_failure(
                format_args!(
                    "Failed to send PUBLISH_DONE for peer subscribe (request_id={}): {e}",
                    peer.request_id
                ),
                session_ended || is_connection_closed(&e),
            );
        }
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

    // request 単位移行の side session を打ち切る。side session の `MoqtClient` はタスクの
    // 終了で drop され、transport が閉じる。移行した PUBLISH の PUBLISH_DONE は旧 session の
    // 終端時に送信済みであり、side session はこの example の終了とともに閉じてよい
    for task in request_migration_tasks.drain(..) {
        task.abort();
        let _ = task.await;
        tracing::debug!("Aborted the request migration task");
    }
    // URL ごとの side session の slot も破棄する。main ループはこれ以降 side session へ
    // PUBLISH の再発行を依頼しない
    request_migration_sessions.clear();

    tracing::info!("Pipeline stopped");
    mp4_reader_result.and(reencode_result)
}

/// GOAWAY ごとに publisher が取る動作
///
/// draft-ietf-moq-transport-23 §6.6.1 (Graceful Session Migration) / §9.2 (GOAWAY) の
/// 適用範囲と New Session URI から決まる。draft 由来の規則であり将来の draft 改版で
/// 変わる可能性がある。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoawayAction {
    /// control stream の GOAWAY。session 全体をバックグラウンドで移行する
    MigrateSession,
    /// request stream の GOAWAY だが、New Session URI が現在の URL と同じである
    ///
    /// 当該 PUBLISH を現在の session で再発行する。
    RepublishRequest(u64),
    /// request stream の GOAWAY で、New Session URI が別の URL を指している
    ///
    /// 当該 PUBLISH だけを専用タスクが確立した side session で再発行する。同じ移行先 URL を
    /// 指す複数の PUBLISH は 1 本の side session を共有する。
    MigrateRequest(u64),
}

/// GOAWAY の適用範囲と New Session URI から publisher の動作を決める
///
/// draft-ietf-moq-transport-23 §9.2 (GOAWAY): "Upon receiving a GOAWAY on a request stream,
/// the endpoint SHOULD re-issue that specific request on a session at the specified URI (or
/// the current session if no URI is provided), and close the old request stream using the
/// appropriate mechanism"。New Session URI が空か現在の URL と同じなら現在の session で
/// 再発行し、別の URL なら side session を確立して移行する。draft 由来の規則であり将来の
/// draft 改版で変わる可能性がある。
fn goaway_action(scope: MigrationScope, same_session: bool) -> GoawayAction {
    match scope {
        MigrationScope::Session => GoawayAction::MigrateSession,
        MigrationScope::Request(request_id) => {
            if same_session {
                GoawayAction::RepublishRequest(request_id)
            } else {
                GoawayAction::MigrateRequest(request_id)
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
/// GOAWAY は送信済みの PUBLISH の request id で通知されるため、再発行する track をこの
/// 対応で決める。request id は session 内で一意であり、映像 / 音声 / カタログの PUBLISH が
/// 同じ id を持つことはない。
///
/// 渡す request id は **main session のもの**でなければならない。request 単位移行で
/// side session へ PUBLISH を移した track は main ループが持つ `*_request_id` が side
/// session のものに差し替わっているため、GOAWAY の照合には
/// [`publish_track_for_main_goaway`] を使う。
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

/// main session の GOAWAY の request id から track を引く
///
/// request 単位移行で side session へ PUBLISH を移した track は、main ループが持つ
/// `*_request_id` が side session の id に差し替わっている。main session の GOAWAY は
/// 移行前の request id で届くため、移行済みの track は [`PublishTarget::old_request_id`]
/// で照合し、移行していない track は `*_request_id` (そのまま main session の id) で
/// 照合する。side session の request id は session ごとに 0 から採番されるため、未移行の
/// track の request id と数値が一致しうる。この区別をしないと、未移行の track の GOAWAY
/// を移行済みの別 track のものと取り違える。
fn publish_track_for_main_goaway(
    request_id: u64,
    video_request_id: Option<u64>,
    audio_request_id: Option<u64>,
    catalog_request_id: u64,
    video_publish_target: Option<&PublishTarget>,
    audio_publish_target: Option<&PublishTarget>,
    catalog_publish_target: Option<&PublishTarget>,
) -> PublishTrack {
    let video_request_id = match video_publish_target {
        Some(target) => Some(target.old_request_id),
        None => video_request_id,
    };
    let audio_request_id = match audio_publish_target {
        Some(target) => Some(target.old_request_id),
        None => audio_request_id,
    };
    let catalog_request_id = match catalog_publish_target {
        Some(target) => target.old_request_id,
        None => catalog_request_id,
    };
    publish_track_for_request(
        request_id,
        video_request_id,
        audio_request_id,
        catalog_request_id,
    )
}

/// track の固定 Track Alias を返す
///
/// 同じ track は session を移しても同じ Track Alias を使い続ける (§3.1.3 (Track Alias))。
/// `PublishTrack::Unknown` は PUBLISH を持たないため alias も無い。
fn track_alias(kind: PublishTrack) -> Option<u64> {
    match kind {
        PublishTrack::Video => Some(VIDEO_TRACK_ALIAS),
        PublishTrack::Audio => Some(AUDIO_TRACK_ALIAS),
        PublishTrack::Catalog => Some(CATALOG_TRACK_ALIAS),
        PublishTrack::Unknown => None,
    }
}

/// track が request 単位移行で side session へ PUBLISH を移しているか
///
/// 移行した track は main ループが持つ request id が side session のものになっており、
/// 旧 session の request id とは対応しない。main session の GOAWAY を照合するときは
/// [`publish_track_for_main_goaway`] が [`PublishTarget::old_request_id`] で旧 id を引く
/// ため、この判定で移行済みの track を対象から外す。
fn track_migrated(
    kind: PublishTrack,
    video_migrated: bool,
    audio_migrated: bool,
    catalog_migrated: bool,
) -> bool {
    match kind {
        PublishTrack::Video => video_migrated,
        PublishTrack::Audio => audio_migrated,
        PublishTrack::Catalog => catalog_migrated,
        PublishTrack::Unknown => false,
    }
}

/// 同じ side session を共有する移行先 URL のキーを作る
///
/// 1 本の side session を共有できるのは、接続先が同じ URL の PUBLISH だけである。
/// [`tokio_moq::same_session_url`] と同じく authority・path・fragment で識別する。
/// fragment まで含めるのは、MSF fragment が track の namespace と認可トークン (c4m) を
/// 変えうるためである。path は空か `/` で始まるため、authority との境界は曖昧にならない。
///
/// この文字列はログに出さない。fragment には認可トークンが含まれうるためである。
fn side_session_key(url: &ServerUrl) -> String {
    let fragment = match &url.fragment {
        Some(fragment) => format!("{}:{}", fragment.fragment_type, fragment.value),
        None => String::new(),
    };
    format!("{}{}#{}", url.authority, url.path, fragment)
}

/// PUBLISH した track の Object を送る session のハンドル一式
///
/// request stream の GOAWAY で別 URL の session へ PUBLISH を移した track は、main ループが
/// 持つ session ではなく side session の handle / data plane で Object を送る。移行していない
/// track は `None` であり、main ループが持つ handle / data plane を使う。
struct PublishTarget {
    /// 移行前の main session の request id
    ///
    /// main ループが持つ `*_request_id` は移行後に side session の id へ差し替わる。
    /// main session の GOAWAY を照合するときはこの id を使う
    /// ([`publish_track_for_main_goaway`])。
    old_request_id: u64,
    /// side session でストリームを開くハンドル
    handle: StreamHandle,
    /// side session の Session を駆動するハンドル
    data_plane: DataPlaneHandle,
}

/// track の Object を送る handle と data plane を選ぶ
///
/// side session へ移行した track (`Some`) はその session のものを、移行していない track は
/// main ループが持つ現在の session のものを返す。
fn publish_sink<'a>(
    migrated: Option<&'a PublishTarget>,
    handle: &'a StreamHandle,
    data_plane: &'a DataPlaneHandle,
) -> (&'a StreamHandle, &'a DataPlaneHandle) {
    match migrated {
        Some(target) => (&target.handle, &target.data_plane),
        None => (handle, data_plane),
    }
}

/// track の Object を送るときの購読の Start Location を返す
///
/// request 単位移行で side session へ移した track は、side session の購読状態を main ループ
/// から観測できない。移行直後の PUBLISH はフィルタ無しで確立しており、side session の
/// `MoqtClient` を所有するタスクだけが REQUEST_UPDATE を処理するため、ここでは `None`
/// (Location Filter 無し) を返す。`None` は省略した Object を配送対象とみなすため、writer の
/// 終端は RESET 側に倒れる (安全側)。
fn publish_start_location(
    client: &MoqtClient,
    migrated: Option<&PublishTarget>,
    request_id: u64,
) -> Option<Location> {
    if migrated.is_some() {
        None
    } else {
        client.subscription_filter_start(request_id)
    }
}

/// カタログの Object を送る session と、送信時点の購読の Start Location
///
/// カタログは受信 SUBSCRIBE の購読にも同じ Object を送るため、PUBLISH の送出先と受信
/// SUBSCRIBE の送出先が異なりうる (前者だけが side session へ移行しうる)。
struct CatalogSink<'a> {
    /// ストリームを開くハンドル
    handle: &'a StreamHandle,
    /// Session を駆動するハンドル
    data_plane: &'a DataPlaneHandle,
    /// 送信時点の購読の Start Location (`None` は Location Filter 無し)
    start_location: Option<Location>,
}

/// カタログの Object を送る session を選ぶ
///
/// `migrated` が `Some` なら side session の handle / data plane を使う
/// ([`publish_sink`] と同じ規則)。Start Location は [`publish_start_location`] で決める。
fn catalog_sink<'a>(
    client: &MoqtClient,
    migrated: Option<&'a PublishTarget>,
    handle: &'a StreamHandle,
    data_plane: &'a DataPlaneHandle,
    catalog_request_id: u64,
) -> CatalogSink<'a> {
    let (handle, data_plane) = publish_sink(migrated, handle, data_plane);
    CatalogSink {
        handle,
        data_plane,
        start_location: publish_start_location(client, migrated, catalog_request_id),
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
/// draft-ietf-moq-transport-23 §6.6.1 (Graceful Session Migration) / §7.7.1 (Graceful
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

/// 移行タスクを起動し、JoinHandle と結果を受け取る oneshot を返す
///
/// 移行タスクは新しい session を確立して PUBLISH を張り、結果を oneshot で main ループへ返す。
/// main ループは結果を受け取るまで旧 session で配信を続ける (§7.7.1)。
/// JoinHandle は run の終了時に打ち切るために呼び出し側が保持する
/// ([`std::mem::drop`] すると detach され、移行先 session が close されないまま残る)。
fn spawn_migration_task(
    config: Config,
    namespace: TrackNamespace,
    target_url: ServerUrl,
    deadline: MigrationDeadline,
    task_monitor: tokio_metrics::TaskMonitor,
) -> (
    tokio::task::JoinHandle<()>,
    oneshot::Receiver<MigrationTaskResult>,
) {
    let (tx, rx) = oneshot::channel();
    // 移行タスクのメトリクスも同じモニタで集計する
    let monitor = task_monitor.clone();
    let handle = tokio::spawn(monitor.instrument(async move {
        let result =
            migrate_publisher_session(config, namespace, target_url, deadline, task_monitor).await;
        // main ループが既に終了している場合 (移行結果はもう誰も読まない) だけ失敗する
        if tx.send(result).is_err() {
            tracing::debug!("Dropping the migration result");
        }
    }));
    (handle, rx)
}

/// 移行後の旧 session を終了するタスクを起動する
///
/// 旧 session の PUBLISH に PUBLISH_DONE を送り、NO_ERROR で閉じる
/// (draft-ietf-moq-transport-23 §6.6.1 (Graceful Session Migration) / §9.9 (PUBLISH_DONE))。
/// GOAWAY の Timeout は待たない。待つと main ループを塞ぐためである。戻り値の JoinHandle は
/// run の終了時に打ち切るために呼び出し側が保持する (detach すると run 終了後も旧 session の
/// close が残る)。
fn spawn_old_session_close(
    task_monitor: &tokio_metrics::TaskMonitor,
    old_client: MoqtClient,
    video_request_id: Option<u64>,
    audio_request_id: Option<u64>,
    catalog_request_id: Option<u64>,
    peer_request_ids: Vec<u64>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(task_monitor.clone().instrument(async move {
        shutdown_old_session(
            old_client,
            video_request_id,
            audio_request_id,
            catalog_request_id,
            peer_request_ids,
        )
        .await;
    }))
}

/// 移行後の旧 session を終了する
///
/// draft-ietf-moq-transport-23 §9.2 (GOAWAY) / §6.6.1 (Graceful Session Migration):
/// 旧 session の PUBLISH に PUBLISH_DONE を送り、open 中の outgoing stream が無い状態で
/// NO_ERROR で閉じる。呼び出し側はこの関数を呼ぶ前に映像の subgroup stream を終端している。
/// GOAWAY の Timeout 分は待たない (移行に要した時間は GOAWAY の期限 (絶対時刻) から既に
/// 差し引かれている)。close は main ループを止めないよう別タスクで行う。
async fn shutdown_old_session(
    mut client: MoqtClient,
    video_request_id: Option<u64>,
    audio_request_id: Option<u64>,
    catalog_request_id: Option<u64>,
    peer_request_ids: Vec<u64>,
) {
    // PUBLISH した track を PUBLISH_DONE で終端する。映像の subgroup stream は呼び出し側が
    // 終端済みであり、全 outgoing stream が閉じているため PUBLISH_DONE を送れる (§9.9)。
    // 受信 SUBSCRIBE で確立した購読にも PUBLISH_DONE を送る
    // (draft-ietf-moq-transport-23 §6.4.2.2 の MUST)
    let mut pairs: Vec<(&str, Option<u64>)> = vec![
        ("video", video_request_id),
        ("audio", audio_request_id),
        ("catalog", catalog_request_id),
    ];
    for request_id in peer_request_ids {
        pairs.push(("peer subscribe", Some(request_id)));
    }
    for (track, request_id) in pairs {
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
    // draft-ietf-moq-transport-23 §6.6.1: 新しい session へ移行したため旧 session は
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
///
/// `sink` はカタログの PUBLISH を送る session である。request 単位移行で side session へ
/// 移した場合はそちらへ、移行していない場合は main ループが持つ現在の session へ送る。
/// 受信 SUBSCRIBE の購読は旧 session に残るため、`peer_handle` / `peer_data_plane` へ送る。
#[expect(
    clippy::too_many_arguments,
    reason = "カタログ本体と受信 SUBSCRIBE で送出先の session が異なりうるため"
)]
async fn republish_catalog_at_group(
    client: &MoqtClient,
    sink: CatalogSink<'_>,
    peer_handle: &StreamHandle,
    peer_data_plane: &DataPlaneHandle,
    catalog_request_id: u64,
    catalog_group: u64,
    catalog_json: &[u8],
    peer_subscriptions: &[PeerSubscription],
) -> Result<ObjectFilterOutcome> {
    let outcome = catalog::publish_catalog_object(
        sink.handle,
        sink.data_plane,
        catalog_request_id,
        CATALOG_TRACK_ALIAS,
        catalog_group,
        catalog_json,
        sink.start_location,
    )
    .await?;
    // 受信 SUBSCRIBE で確立した catalog 購読にも同じ Object を送る
    // (draft-ietf-moq-transport-23 §3.1 の "the publisher MUST send the Object once for each
    // matching subscription")。各購読は独立した stream で受けるため、同じ Group の先頭に
    // 置ける。失敗しても main の catalog 送出を壊さない (購読が終端済みの可能性が高い)
    for peer in peer_subscriptions
        .iter()
        .filter(|peer| peer.track == PublishedTrack::Catalog && !peer.ended)
    {
        if let Err(e) = catalog::publish_catalog_object(
            peer_handle,
            peer_data_plane,
            peer.request_id,
            peer.alias,
            catalog_group,
            catalog_json,
            client.subscription_filter_start(peer.request_id),
        )
        .await
        {
            tracing::debug!(
                "Failed to send the catalog to a peer subscription: request_id={}, {e}",
                peer.request_id
            );
        }
    }
    Ok(outcome)
}

/// カタログを次の Group で送り直し、送信できた場合は現在の Location を更新する
///
/// Group ID は送信の成否によらず先に進める。同じ Location を 2 度送ると購読側で重複として
/// 扱われ、失敗した Group を再送すると同じ Group の Object が二重に登録されうるためである
/// (draft-ietf-moq-msf-01 §6.1 (Group numbering))。
#[expect(
    clippy::too_many_arguments,
    reason = "main ループが持つ catalog 送出の状態と受信 SUBSCRIBE の一覧を渡すため"
)]
async fn republish_catalog_at_next_group(
    client: &MoqtClient,
    sink: CatalogSink<'_>,
    peer_handle: &StreamHandle,
    peer_data_plane: &DataPlaneHandle,
    catalog_request_id: u64,
    catalog_json: &[u8],
    catalog_group_id: &mut u64,
    catalog_location: &mut Location,
    peer_subscriptions: &[PeerSubscription],
) -> Result<ObjectFilterOutcome> {
    let next_group_id = group_id::next_group_id(*catalog_group_id);
    *catalog_group_id = next_group_id;
    let published = republish_catalog_at_group(
        client,
        sink,
        peer_handle,
        peer_data_plane,
        catalog_request_id,
        next_group_id,
        catalog_json,
        peer_subscriptions,
    )
    .await?;
    if published == ObjectFilterOutcome::Pass {
        *catalog_location = Location {
            group_id: next_group_id,
            object_id: 0,
        };
        tracing::debug!("Republished the MSF catalog at group {next_group_id}");
    } else {
        // どの購読にも届かなかった (PAUSED=1 など)。Object は送られていないため、
        // FETCH 応答と LARGEST_OBJECT が指す Location は前回のままにする
        tracing::debug!("Catalog republish at group {next_group_id} was filtered out");
    }
    Ok(published)
}

/// 旧 request を PUBLISH_DONE で終端する (失敗はログのみ)
///
/// draft-ietf-moq-transport-23 §9.2 (GOAWAY): request stream の GOAWAY を受けたら当該 request を
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
/// draft-ietf-moq-transport-23 §9.2 (GOAWAY): request stream の GOAWAY は、指定 URI (空なら
/// 現在の session) で当該 request を再発行することを求める。ここでは New Session URI が現在の
/// URL と同じ場合を扱い、request id を新しい PUBLISH のものへ差し替える。Track Alias は
/// track ごとに固定であり、同じ track の再 PUBLISH では同じ alias を使い続ける
/// (§3.1.3 (Track Alias))。旧 request は PUBLISH_DONE で終端する。draft 由来の規則であり
/// 将来の draft 改版で変わる可能性がある。
///
/// `kind` は呼び出し側が [`publish_track_for_request`] で解決した track である。request 単位
/// 移行で side session へ移した track は、main ループが持つ request id が side session の
/// ものになっているため、呼び出し側が対象から外してから呼ぶ。
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
    kind: PublishTrack,
    video_request_id: &mut Option<u64>,
    audio_request_id: &mut Option<u64>,
    catalog_request_id: &mut u64,
    current_video_writer: &mut Option<SubgroupWriter>,
    gop_tracker: &mut gop::GopTracker,
) -> Result<PublishTrack> {
    match kind {
        PublishTrack::Video => {
            // 新しい PUBLISH の確立を待ってから旧 request を終端する。その間も旧 request の
            // 購読には届き続ける (§7.7.1 の publisher switchover)
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
    Ok(kind)
}

/// URL ごとに共有する side session へ送るコマンド
///
/// main ループは移行先 URL ごとに 1 本の side session を確立し、同じ URL を指す複数の
/// PUBLISH の移行要求をこのコマンドでまとめて送る。[`SideSessionCommand::Publish`] を
/// 受けたタスクは、確立済みの side session で PUBLISH を再発行する。
#[derive(Debug, Clone, Copy)]
enum SideSessionCommand {
    /// 移行対象の PUBLISH を side session で再発行する
    Publish {
        /// 移行する track 種別
        kind: PublishTrack,
        /// 旧 session の request id (PUBLISH_DONE による終端は main ループが行う)
        old_request_id: u64,
        /// この PUBLISH の移行期限 (GOAWAY の Timeout から作る)
        deadline: MigrationDeadline,
    },
}

/// 移行先 URL ごとの side session
///
/// 同じ URL を指す複数の PUBLISH は、ここが持つコマンド送信チャネルで 1 本の side
/// session を共有する。main ループは送信に失敗したときと side session の終了を観測した
/// ときに slot を削除する。
struct SideSessionSlot {
    /// side session へ PUBLISH の再発行を依頼するチャネル
    commands: mpsc::Sender<SideSessionCommand>,
}

/// URL ごとに共有する request 移行 side session から main ループへ送るイベント
///
/// side session の確立と PUBLISH の再発行の結果と、side session で観測した session
/// イベントを main ループへ渡す。main ループは `MoqtClient` を 1 つだけ所有するため、
/// 移行対象の PUBLISH は専用タスクが確立した side session で扱う
/// (draft-ietf-moq-transport-23 §9.2 (GOAWAY) の "re-issue that specific request on a
/// session at the specified URI"。draft 由来の仕様であり将来変更されうる)。
///
/// 同じ移行先 URL の PUBLISH は 1 本の side session を共有するため、どの side session の
/// イベントかを `url` で識別する。
enum RequestMigrationEvent {
    /// side session の確立と PUBLISH の再発行に成功した
    Migrated {
        /// 移行先の URL (side session を共有する単位)
        url: ServerUrl,
        /// 移行した track 種別
        kind: PublishTrack,
        /// 旧 session の request id (main ループが PUBLISH_DONE で終端する)
        old_request_id: u64,
        /// side session で再発行した PUBLISH の request id
        new_request_id: u64,
        /// 再発行に使った Track Alias (track ごとに固定)
        new_track_alias: u64,
        /// side session の Object 送信用ハンドル
        data_plane: DataPlaneHandle,
        /// side session のストリーム開設用ハンドル
        handle: StreamHandle,
    },
    /// 1 本の PUBLISH の再発行失敗 (期限切れ・接続失敗・再発行の失敗)
    ///
    /// session 全体の終了 ([`Self::Ended`]) と区別する。
    Failed {
        /// 移行先の URL (side session を共有する単位)
        url: ServerUrl,
        /// 移行できなかった track 種別
        kind: PublishTrack,
        /// 移行できなかった旧 session の request id
        old_request_id: u64,
        /// ログに出す失敗の説明
        reason: String,
    },
    /// side session で観測した session イベント (PUBLISH_DONE / REQUEST_ERROR / GOAWAY / close)
    ///
    /// 1 本の side session は複数の track を扱う。PUBLISH_DONE / REQUEST_ERROR は request id を
    /// 持つためどの track のものか分かるが、session 全体のイベント (GOAWAY / close) は track に
    /// 紐づかない。そのためここでは track 種別を持たない。
    SessionEvent {
        /// イベントの発生元の side session の URL
        url: ServerUrl,
        /// side session で観測したイベント
        event: SessionEvent,
    },
    /// side session が終了した。その session を共有する移行済みの track はもう送れない
    Ended {
        /// 終了した side session の URL
        url: ServerUrl,
    },
}

/// URL ごとに共有する request 移行 side session を確立するための要求
struct RequestMigrationRequest {
    /// 移行先の URL (GOAWAY の New Session URI)
    url: ServerUrl,
    /// 接続と PUBLISH に使う設定 (main ループの設定を複製する)
    config: Config,
    /// 送信する namespace
    namespace: TrackNamespace,
}

/// side session タスクの `tokio::select!` が返す「何が起きたか」
///
/// `MoqtClient` はタスクが 1 つだけ所有するため、`select!` の腕では `client` を借用する
/// future だけを待ち、結果を受けて `client` を使う処理は `select!` の外で行う。その
/// 受け渡しに使う。腕ごとに完了した事実だけを持ち、判定は select の後で行う。
enum SideSessionStep {
    /// `MoqtClient::next_event` の結果
    Event(std::result::Result<Option<ClientEvent>, TransportError>),
    /// side session が data stream を受理した結果
    Accepted(std::result::Result<Option<transport::RecvStream>, TransportError>),
    /// main ループからの PUBLISH 再発行要求 (チャネルが閉じたら `None`)
    Command(Option<SideSessionCommand>),
    /// 100 ms 周期の tick
    Tick,
}

/// URL ごとに共有する request 移行 side session を起動する
///
/// タスクは移行に成功した後も side session の受信を続け、同じ URL を指す 2 本目以降の
/// PUBLISH をコマンドで受け取って同じ session で再発行する。main ループは JoinHandle を
/// 保持し、run の終了時と session 全体の移行成功時に abort する (session 全体の移行が
/// 成功したときは、移行先の session が全 track を張り直すため abort する)。イベントは
/// 複数のタスクで共有するチャネルへ送る。
///
/// 戻り値のコマンド送信チャネルは main ループが URL ごとの [`SideSessionSlot`] に保持する。
fn spawn_request_migration(
    task_monitor: &tokio_metrics::TaskMonitor,
    request: RequestMigrationRequest,
    initial: SideSessionCommand,
    events: mpsc::Sender<RequestMigrationEvent>,
) -> (
    tokio::task::JoinHandle<()>,
    mpsc::Sender<SideSessionCommand>,
) {
    let (commands_tx, commands_rx) =
        mpsc::channel::<SideSessionCommand>(REQUEST_MIGRATION_COMMAND_CHANNEL_SIZE);
    let connect_monitor = task_monitor.clone();
    let instrument_monitor = task_monitor.clone();
    let handle = tokio::spawn(instrument_monitor.instrument(async move {
        run_request_migration(request, initial, commands_rx, connect_monitor, events).await;
    }));
    (handle, commands_tx)
}

/// 1 本の PUBLISH の再発行失敗を main ループへ通知する
///
/// main ループが既に終了している場合 (移行結果を読む者がいない) は通知できないが、
/// side session はこのタスクの終了で閉じるため、失敗を捨ててよい。
async fn send_request_migration_failure(
    events: &mpsc::Sender<RequestMigrationEvent>,
    url: &ServerUrl,
    kind: PublishTrack,
    old_request_id: u64,
    reason: String,
) {
    let _ = events
        .send(RequestMigrationEvent::Failed {
            url: url.clone(),
            kind,
            old_request_id,
            reason,
        })
        .await;
}

/// side session で 1 本の PUBLISH を再発行し、成功を main ループへ通知する
///
/// 期限が残っていなければ PUBLISH の応答を待たずに失敗として扱う。期限の途中で応答待ちを
/// 打ち切る場合も、session を放置すると peer が timeout まで資源を保持するため NO_ERROR で
/// 閉じる。1 本の PUBLISH の失敗は session 全体の終了ではないため
/// [`RequestMigrationEvent::Failed`] で報告し、戻り値 `false` で呼び出し側に side session を
/// 終えさせる (失敗した track の PUBLISH はもう送れない)。
async fn reissue_publish_on_side_session(
    client: &mut MoqtClient,
    request: &RequestMigrationRequest,
    events: &mpsc::Sender<RequestMigrationEvent>,
    command: SideSessionCommand,
) -> bool {
    let SideSessionCommand::Publish {
        kind,
        old_request_id,
        deadline,
    } = command;

    let remaining = deadline.remaining();
    if remaining.is_zero() {
        close_session_after_failed_publish(client).await;
        send_request_migration_failure(
            events,
            &request.url,
            kind,
            old_request_id,
            "the migration deadline expired before re-issuing the PUBLISH".to_string(),
        )
        .await;
        return false;
    }
    let published = tokio::time::timeout(remaining, async {
        match kind {
            PublishTrack::Video => {
                publish_video_track(client, &request.config, &request.namespace).await
            }
            PublishTrack::Audio => {
                publish_audio_track(client, &request.config, &request.namespace).await
            }
            PublishTrack::Catalog => publish_catalog_track(client, &request.namespace).await,
            // 移行の対象は PUBLISH した track だけである。main ループは Unknown を
            // 移行に回さないため到達しない
            PublishTrack::Unknown => Err(Error::Other(
                "a request-scoped GOAWAY migration requires a published track".to_string(),
            )),
        }
    })
    .await;
    let new_request_id = match published {
        Ok(Ok(new_request_id)) => new_request_id,
        Ok(Err(e)) => {
            close_session_after_failed_publish(client).await;
            send_request_migration_failure(
                events,
                &request.url,
                kind,
                old_request_id,
                format!("failed to re-issue the PUBLISH on the new session: {e}"),
            )
            .await;
            return false;
        }
        Err(_) => {
            close_session_after_failed_publish(client).await;
            send_request_migration_failure(
                events,
                &request.url,
                kind,
                old_request_id,
                "the PUBLISH re-issue did not complete within the migration deadline".to_string(),
            )
            .await;
            return false;
        }
    };
    let Some(new_track_alias) = track_alias(kind) else {
        // Unknown は上の PUBLISH で Failed になるため到達しない
        close_session_after_failed_publish(client).await;
        send_request_migration_failure(
            events,
            &request.url,
            kind,
            old_request_id,
            "the migrated PUBLISH has no track alias".to_string(),
        )
        .await;
        return false;
    };
    let data_plane = client.data_plane();
    let handle = client.handle();
    tracing::info!(
        "Re-issued the PUBLISH on the request migration side session: authority={}, kind={kind:?}, old_request_id={old_request_id}, new_request_id={new_request_id}, track_alias={new_track_alias}",
        request.url.authority
    );
    events
        .send(RequestMigrationEvent::Migrated {
            url: request.url.clone(),
            kind,
            old_request_id,
            new_request_id,
            new_track_alias,
            data_plane,
            handle,
        })
        .await
        .is_ok()
}

/// URL ごとに共有する request 移行 side session タスクの本体
///
/// 次の順に進む。
///
/// 1. 移行先の session を [`connect_with_retry`] で確立する。接続の設定・期限・指数的な
///    再試行は control GOAWAY の移行 ([`establish_publisher_session_with_retry`]) と同じで
///    ある。期限は最初の PUBLISH の移行期限 (GOAWAY の Timeout) を使う。
/// 2. main ループと同じタイムアウトを設定する。
/// 3. 最初の PUBLISH を再発行する。期限が残っていなければ接続を閉じて失敗にする。
/// 4. 成功を `Migrated` として main ループへ通知し、side session の受信ループを回す。
/// 5. 受信ループでは同じ URL を指す追加の PUBLISH をコマンドで受け取り、同じ session で
///    再発行する。session イベントの転送と 100 ms 周期の tick も行い、peer request は
///    REQUEST_NOT_SUPPORTED で拒否する。この example は side session で peer の要求を
///    処理しない (移行した PUBLISH の購読は relay が管理する)。
///
/// 1 本の PUBLISH の再発行失敗 (`Failed`) と session 全体の終了 (`Ended`) を区別して
/// main ループへ通知する。session が終了した場合は、その session を共有する移行済みの
/// track がすべて送れなくなるため `Ended` を送る。期限 ([`MigrationDeadline`]) は GOAWAY の
/// Timeout から作る。期限内に接続と再発行を終えられなければ `Failed` を送って終了し、
/// main ループは control GOAWAY の移行失敗と同じ終了経路へ進む。移行に成功した後の期限は
/// 旧 session を peer が閉じる時刻であり、side session の寿命ではないため `Failed` にしない。
///
/// draft-ietf-moq-transport-23 §9.2 (GOAWAY) は draft 由来の仕様であり、将来変更されうる。
async fn run_request_migration(
    request: RequestMigrationRequest,
    initial: SideSessionCommand,
    mut commands: mpsc::Receiver<SideSessionCommand>,
    task_monitor: tokio_metrics::TaskMonitor,
    events: mpsc::Sender<RequestMigrationEvent>,
) {
    // 1. 移行先の session を確立する
    //
    // 接続の失敗は connect_with_retry が期限内で再試行済みであるため、ここでは再試行しない
    // (同じ設定で失敗し続けるだけになる)。認可トークンの引き継ぎは migration_target が
    // 済ませている (同一 origin のときだけ引き継ぐ)。
    //
    // 期限は最初の PUBLISH のものを使う。同じ URL の 2 本目以降の GOAWAY は、確立済みの
    // session にコマンドとして届くため、ここでの接続期限には影響しない。
    let SideSessionCommand::Publish { deadline, .. } = initial;
    let params = publisher_connect_params(&request.config, &request.url);
    let connected =
        tokio_moq::connect_with_retry(&params, deadline, RetryPolicy::default(), &task_monitor)
            .await;
    let (mut client, mut recv_acceptor) = match connected {
        Ok(connected) => connected,
        Err(e) => {
            // 接続できなかった。この session で移行する最初の PUBLISH を Failed として
            // 報告する。待ち行列に残ったコマンドの track も移行できないが、main ループは
            // Failed を受けてパイプラインを終了するため、個別には報告しない
            let SideSessionCommand::Publish {
                kind,
                old_request_id,
                ..
            } = initial;
            send_request_migration_failure(
                &events,
                &request.url,
                kind,
                old_request_id,
                format!("failed to connect to the migration target: {e}"),
            )
            .await;
            return;
        }
    };
    tracing::info!(
        "Established the request migration side session: authority={}",
        request.url.authority
    );

    // 2. main ループと同じタイムアウトを設定する
    configure_publisher_session(&mut client);

    // 3. 最初の PUBLISH を再発行する
    //
    // 失敗は Failed として報告済みである。session を確立できていても移行先として使え
    // ないため、Ended は送らずにこのタスクを終える
    if !reissue_publish_on_side_session(&mut client, &request, &events, initial).await {
        return;
    }

    // 4. side session の受信ループ
    //
    // tick の周期は main ループと同じ 100 ms にする。`MoqtClient` の tick は session の
    // タイマー (control message timeout / data stream timeout / GOAWAY deadline) を進める
    // ため、ここが止まると side session の進行も止まる。
    let mut tick_interval = tokio::time::interval(Duration::from_millis(100));
    tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    'receive: loop {
        let step = tokio::select! {
            event = client.next_event() => SideSessionStep::Event(event),
            accepted = recv_acceptor.accept_recv_stream() => {
                SideSessionStep::Accepted(accepted)
            }
            command = commands.recv() => SideSessionStep::Command(command),
            _ = tick_interval.tick() => SideSessionStep::Tick,
        };
        match step {
            SideSessionStep::Event(Ok(Some(ClientEvent::Session(event)))) => {
                // PUBLISH_DONE / REQUEST_ERROR / GOAWAY / close は main ループへ渡す。
                // main ループが先に終了した場合は移行結果を読む者がいないため、side
                // session の受信を終える
                if events
                    .send(RequestMigrationEvent::SessionEvent {
                        url: request.url.clone(),
                        event,
                    })
                    .await
                    .is_err()
                {
                    break 'receive;
                }
            }
            SideSessionStep::Event(Ok(Some(ClientEvent::Request(peer_request)))) => {
                // side session では main ループと同じく peer request を処理しない。応答を
                // 返さないと peer の control message timeout を招くため拒否する
                tracing::warn!(
                    "Rejecting an unexpected peer request on the request migration side session: authority={}, request_id={}",
                    request.url.authority,
                    peer_request.request_id
                );
                if let Err(e) = client
                    .send_request_error(
                        peer_request.request_id,
                        REQUEST_NOT_SUPPORTED,
                        "publisher does not serve requests on the request migration side session",
                    )
                    .await
                {
                    tracing::warn!(
                        "Failed to reject a peer request on the request migration side session: authority={}, {e}",
                        request.url.authority
                    );
                    break 'receive;
                }
            }
            SideSessionStep::Event(Ok(Some(ClientEvent::RequestUpdate(update)))) => {
                // draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE): 受信側は必ず 1 通の
                // REQUEST_OK / REQUEST_ERROR で応答する MUST
                tracing::info!(
                    "Received REQUEST_UPDATE on the request migration side session: authority={}, request_id={}",
                    request.url.authority,
                    update.request_id
                );
                if let Err(e) = client.send_request_ok(update.request_id).await {
                    tracing::warn!(
                        "Failed to send REQUEST_OK on the request migration side session: authority={}, {e}",
                        request.url.authority
                    );
                    break 'receive;
                }
            }
            SideSessionStep::Event(Ok(None)) => {
                tracing::info!(
                    "The request migration side session ended: authority={}",
                    request.url.authority
                );
                break 'receive;
            }
            SideSessionStep::Event(Err(e)) => {
                tracing::warn!(
                    "Request migration side session event error: authority={}, {e}",
                    request.url.authority
                );
                break 'receive;
            }
            SideSessionStep::Accepted(Ok(Some(mut stream))) => {
                // この example は side session で data stream を処理しない。受け取ったまま
                // 放置すると peer が flow control で待ち続けるため、未対応を表すコードで
                // 拒否する
                tracing::warn!(
                    "Rejecting an unexpected data stream on the request migration side session: authority={}, stream_id={}",
                    request.url.authority,
                    stream.stream_id()
                );
                // data stream の reset code は §12.5 の CANCELLED を使う
                // (REQUEST_NOT_SUPPORTED は §12.3 の REQUEST_ERROR コードであり、
                // §12.5 の 0x3 は SESSION_CLOSED を意味する)
                if let Err(e) = stream.stop_sending(0x1).await {
                    tracing::debug!(
                        "Failed to stop an unexpected data stream on the request migration side session: authority={}, {e}",
                        request.url.authority
                    );
                }
            }
            SideSessionStep::Accepted(Ok(None)) => {
                tracing::info!(
                    "No more data streams on the request migration side session: authority={}",
                    request.url.authority
                );
                break 'receive;
            }
            SideSessionStep::Accepted(Err(e)) => {
                // セッション終了と接続クローズは期待される終了であるため info に揃える
                if is_connection_closed(&e) {
                    tracing::info!(
                        "Request migration side session closed by transport: authority={}, {e}",
                        request.url.authority
                    );
                } else {
                    tracing::warn!(
                        "Failed to accept a data stream on the request migration side session: authority={}, {e}",
                        request.url.authority
                    );
                }
                break 'receive;
            }
            SideSessionStep::Tick => {
                // main ループと同じく、session のタイマーを進める
                client.tick_now();
            }
            SideSessionStep::Command(Some(command)) => {
                // 同じ移行先 URL を指す 2 本目以降の PUBLISH は、確立済みの session で
                // 再発行する。1 本の再発行の失敗は Failed として報告済みであり、session
                // 全体の終了ではないため Ended は送らない。main ループは Failed を受けて
                // パイプラインを終了するため、このタスクもここで終える
                if !reissue_publish_on_side_session(&mut client, &request, &events, command).await {
                    return;
                }
            }
            SideSessionStep::Command(None) => {
                // main ループが slot を破棄した (run の終了)。新しい PUBLISH の依頼は
                // 来ないため side session を終える
                tracing::debug!(
                    "The request migration command channel closed: authority={}",
                    request.url.authority
                );
                break 'receive;
            }
        }
    }
    // side session を失ったことを main ループへ伝える。この session を共有する移行済みの
    // track はすべて送れなくなるためである
    let _ = events
        .send(RequestMigrationEvent::Ended {
            url: request.url.clone(),
        })
        .await;
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
/// peer の STOP_SENDING を受信したストリームの送信 API は失敗し、I/O 層はそれを
/// [`Error::StreamReset`] に写す (QUIC 直接接続は `tobi-quic` の `StreamError::Stopped`、
/// QMux は `Error::Stopped`)。これは該当ストリームだけの終端でありセッションは継続するため
/// (draft-ietf-moq-transport-23 §6.4.2.3 (Request Cancellation and Rejection))、
/// pipeline は致命エラーにしない。
///
/// WebTransport over HTTP/2 も driver が受領した `WT_STOP_SENDING` のエラーコードを
/// `StreamReset` として返すため本判定の対象になる (draft-ietf-webtrans-http2-16 §6.3)。
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
/// `StreamReset` だけが対象になる (draft-ietf-moq-transport-23 §6.4.2.3)。
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
/// draft-ietf-moq-transport-23 §9 (Control Messages) と §9.19.1 (Parameter Scope) は
/// 不正なメッセージの受信を終了コード付きで閉じることを MUST で要求する。
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
/// draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams): fill fetch
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

/// この example が配信している track の種別
///
/// 受信した SUBSCRIBE / FETCH の Full Track Name を [`published_track`] で照合した結果である。
/// 受理した受信 SUBSCRIBE の情報
///
/// draft-ietf-moq-transport-23 §3.1: 受信 SUBSCRIBE の購読は、自側が送った PUBLISH 購読と
/// 並行して Object を送る (追加の subgroup stream / datagram / PUBLISH_DONE)。Track Alias
/// は購読ごとに採番する (§3.1 の "A publisher MAY assign the same or different Track
/// Aliases to these subscriptions.")。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AcceptedPeerSubscribe {
    /// 受信 SUBSCRIBE の Request ID
    request_id: u64,
    /// 配信する track
    track: PublishedTrack,
    /// SUBSCRIBE_OK に載せた Track Alias (購読ごとに一意)
    alias: u64,
}

/// 受信 SUBSCRIBE で確立した購読の送出状態
///
/// `writer` は subgroup 送出の購読 (映像 / 音声 subgroup / catalog) が持つ。音声 datagram
/// は Object ごとに `DatagramWriter` を作るため保持しない。`ended` は購読が終端した
/// (peer の STOP_SENDING / RESET_STREAM、Session の回収) ことを表し、送出を止める。
struct PeerSubscription {
    request_id: u64,
    track: PublishedTrack,
    alias: u64,
    writer: Option<SubgroupWriter>,
    audio_config_sent: bool,
    ended: bool,
}

/// この example が配信している track の種別
///
/// 受信した SUBSCRIBE / FETCH の Full Track Name を [`published_track`] で照合した結果である。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishedTrack {
    /// 映像 track (`--track-name`)
    Video,
    /// 音声 track ([`AUDIO_TRACK_NAME`])
    Audio,
    /// MSF カタログ track ([`MSF_CATALOG_TRACK_NAME`])
    Catalog,
}

/// 受信した Full Track Name がこの example の配信している track かを判定する
///
/// draft-ietf-moq-transport-23 §2.4.1 (Track Naming): Track は Track Namespace と Track Name から
/// なる Full Track Name で識別され、各フィールドはバイト単位で比較する。namespace が
/// `config.namespace` と完全一致しない要求は、track 名が同じでも別の track であるため `None` を
/// 返す。配信していない映像 / 音声も `None` になる。catalog は映像・音声の有無に関係なく常に
/// PUBLISH するため、namespace が一致すれば `Some(Catalog)` を返す。
///
/// `--track-name` は catalog / audio (音声を配信する場合) と重ならないことを CLI が保証するため、
/// 判定の順序で結果は変わらない。この節番号はドラフト由来であり将来変更される可能性がある。
fn published_track(
    config: &Config,
    namespace: &TrackNamespace,
    name: &[u8],
) -> Option<PublishedTrack> {
    if *namespace != config.namespace {
        return None;
    }
    if config.video_enabled && name == config.track_name.as_bytes() {
        Some(PublishedTrack::Video)
    } else if config.audio_enabled && name == AUDIO_TRACK_NAME.as_bytes() {
        Some(PublishedTrack::Audio)
    } else if name == MSF_CATALOG_TRACK_NAME {
        Some(PublishedTrack::Catalog)
    } else {
        None
    }
}

/// MOQT relay が転送してきた要求 (SUBSCRIBE / FETCH) に応答する
///
/// relay は subscriber の SUBSCRIBE / FETCH を publisher 側 session の新しい
/// request stream として転送する (draft-ietf-moq-transport-23 §6.3 (Session initialization))。
/// 本 publisher は video / audio / catalog の SUBSCRIBE に SUBSCRIBE_OK を返し
/// (以後の Object は継続して送信中の subgroup stream で届く)、catalog とメディア (映像 /
/// 音声) の FETCH には要求された range を判定したうえで FETCH_OK と FETCH 応答ストリームを返す。
/// 未知の track と未対応の要求種別は REQUEST_ERROR で拒否する
/// (draft-ietf-moq-transport-23 §9.4 (REQUEST_ERROR))。
///
/// `gop_tracker` と `started_video_group_id` は、SUBSCRIBE に載った `NEW_GROUP_REQUEST`
/// (draft-ietf-moq-transport-23 §9.19.19) を映像のキーフレーム要求へ配線するために使う。
/// `media_cache` は publish 済みメディアの保持であり、メディアの FETCH に配る Object と
/// End of Unknown Range を決めるために使う。
#[expect(
    clippy::too_many_arguments,
    reason = "main ループが持つ peer 受付の状態をまとめて渡すため"
)]
async fn serve_peer_request(
    client: &mut MoqtClient,
    request: IncomingRequest,
    config: &Config,
    catalog: CatalogState<'_>,
    gop_tracker: &mut gop::GopTracker,
    started_video_group_id: Option<u64>,
    media_cache: &mut cache::MediaCache,
    next_peer_alias: &mut u64,
) -> Result<Option<AcceptedPeerSubscribe>> {
    let request_id = request.request_id;
    match request.message {
        ControlMessage::Subscribe(subscribe) => {
            let track_name: &[u8] = &subscribe.track_name;
            // Track Name だけでなく Track Namespace も照合する。別 namespace の SUBSCRIBE を
            // 受理すると、別の track の購読として自分のコンテンツを配ってしまう
            let published = published_track(config, &subscribe.track_namespace, track_name);
            match published {
                Some(track) => {
                    // 受信 SUBSCRIBE ごとに一意な Track Alias を採番する
                    // (draft-ietf-moq-transport-23 §3.1: "A publisher MAY assign the same or
                    // different Track Aliases to these subscriptions.")。購読ごとの stream は
                    // `SubgroupTracker::open` が同一 (track_alias, group_id, subgroup_id) の
                    // 並行 stream を PROTOCOL_VIOLATION で拒否するため (§11.3 の SHOULD NOT)、
                    // alias を分けて header field values を分ける
                    let peer_alias = *next_peer_alias;
                    *next_peer_alias = next_peer_alias
                        .checked_add(1)
                        .expect("peer track alias counter overflowed");
                    // 購読者は DYNAMIC_GROUPS を知らずに SUBSCRIBE へ NEW_GROUP_REQUEST を
                    // 載せられる (§9.19.19)。Group の途中で参加した購読者が新しい Group を
                    // 要求する経路であり、映像トラックのときだけ受け付ける。MP4 パススルーは
                    // キーフレームを要求できないため受け付けない
                    let new_group_request = accepted_new_group_request(
                        track == PublishedTrack::Video && config.input_mp4.is_none(),
                        &subscribe.parameters,
                        started_video_group_id,
                    );
                    // 映像トラックの SUBSCRIBE_OK にも PUBLISH と同じ DYNAMIC_GROUPS を載せる。
                    // 購読者はこの値で publisher が動的 Group に対応しているかを知り
                    // (draft-ietf-moq-transport-23 §3.5.1)、REQUEST_UPDATE で
                    // NEW_GROUP_REQUEST を送れるようになる (§9.19.19 の MUST NOT)
                    let subscribe_properties = subscribe_ok_properties(
                        track,
                        config.input_mp4.is_some(),
                        config.cache_duration_ms,
                    );
                    client
                        .send_subscribe_ok(
                            request_id,
                            peer_alias,
                            MessageParameters::new(),
                            subscribe_properties,
                        )
                        .await?;
                    tracing::info!(
                        "Accepted SUBSCRIBE (request_id={request_id}, track={}, alias={peer_alias})",
                        String::from_utf8_lossy(track_name),
                    );
                    // 購読が成立している要求だけを受け付ける
                    if let Some(next_group_id) = new_group_request {
                        gop_tracker.request_new_group();
                        tracing::info!(
                            "Received NEW_GROUP_REQUEST in SUBSCRIBE: next_group_id={next_group_id}"
                        );
                    }
                    return Ok(Some(AcceptedPeerSubscribe {
                        request_id,
                        track,
                        alias: peer_alias,
                    }));
                }
                None => {
                    tracing::warn!(
                        "Rejecting SUBSCRIBE for unknown track: namespace={}, track={}",
                        serialize_namespace(&subscribe.track_namespace),
                        String::from_utf8_lossy(track_name),
                    );
                    client
                        .send_request_error(request_id, REQUEST_DOES_NOT_EXIST, "unknown track")
                        .await?;
                }
            }
        }
        ControlMessage::Fetch(fetch) => {
            // Track Name だけでなく Track Namespace も照合する。別 namespace の FETCH に自分の
            // namespace のコンテンツを返さない (draft-ietf-moq-transport-23 §2.4.1)
            let Some(published) =
                published_track(config, &fetch.track_namespace, &fetch.track_name)
            else {
                tracing::warn!(
                    "Rejecting FETCH for unknown track: namespace={}, track={}",
                    serialize_namespace(&fetch.track_namespace),
                    String::from_utf8_lossy(&fetch.track_name),
                );
                client
                    .send_request_error(request_id, REQUEST_DOES_NOT_EXIST, "unknown track")
                    .await?;
                return Ok(None);
            };
            if published == PublishedTrack::Catalog {
                // 要求された Location Filter から range を解決し、範囲外の Object を
                // 送らないようにする (draft-ietf-moq-transport-23 §3.2 (Fetch) /
                // §3.3.1 (Location Filters) / §9.19.9 (LOCATION FILTER Parameter))。
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
                        return Ok(None);
                    }
                };
                match catalog::catalog_fetch_response(filter.as_ref(), catalog.location) {
                    catalog::CatalogFetchResponse::Object { end_location } => {
                        // 要求範囲にカタログ自身の Location (この track の Largest Object) が
                        // 含まれるため、LARGEST_OBJECT として渡して応答範囲をそこへ切り詰める
                        // (draft-ietf-moq-transport-23 §3.2 (Fetch))
                        client
                            .send_fetch_ok(
                                request_id,
                                fetch_ok_parameters(Some(end_location)),
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
                        // 空ストリームを返す (draft-ietf-moq-transport-23 §3.2.1 (Fetch Object
                        // Delivery))。Largest Object は要求範囲外なので LARGEST_OBJECT は載せず、
                        // 応答範囲の終端は要求の End Location になる (§3.2 (Fetch))
                        client
                            .send_fetch_ok(
                                request_id,
                                fetch_ok_parameters(None),
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
    Ok(None)
}

/// FETCH の初回成功応答に載せるパラメータを組み立てる
///
/// draft-ietf-moq-transport-23 §9.3 (REQUEST_OK): FETCH_OK が運べるのは EXPIRES と
/// LARGEST_OBJECT である。Largest Object が要求範囲内なら応答はそこへ切り詰められ、
/// LARGEST_OBJECT を載せなければならない (§3.2 (Fetch))。この example は Session を経由せず
/// に Object を書くため Session は Track の Largest Object を観測できない。自前のキャッシュが
/// 把握している値をここで渡す。
fn fetch_ok_parameters(largest_in_range: Option<Location>) -> MessageParameters {
    let mut parameters = MessageParameters::new();
    if let Some(largest) = largest_in_range {
        parameters.set_largest_object(largest.group_id, largest.object_id);
    }
    parameters
}

/// FETCH の LOCATION_FILTER から要求 range を決める
///
/// draft-ietf-moq-transport-23 §3.2 (Fetch) / §3.3.1 (Location Filters) / §9.19.9 (LOCATION
/// FILTER Parameter): Start の省略時は track の先頭 `{0, 0}`、End の省略時は Largest Object
/// である。要求が Largest Object より先を指しても publisher が持つのは Largest Object まで
/// であるため、`end` は Largest Object でクランプする。
///
/// Largest Object が無い (まだ 1 件も publish していない) track でも、境界の解決を Session と
/// 揃えて先頭側に解決する。この場合に配れる Object は無く、応答は cache に一度も入っていない
/// track ([`cache::FetchPlan::UnknownTrack`]) として決まる。
///
/// 戻り値の `end` は Largest Object でクランプ済みなので、`largest == end` が
/// 「Largest Object が要求範囲内」の判定になる。範囲内なら FETCH_OK に LARGEST_OBJECT を
/// 載せて応答をそこへ切り詰める MUST があり、範囲外なら要求の End Location が応答範囲の
/// 終端になる (§3.2 (Fetch))。
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
/// (draft-ietf-moq-transport-23 §3.2 (Fetch) / §11.4 (Fetch Streams))。
/// 応答は「End of Unknown Range (必要なら) → Object → (全滅なら End of Unknown Range)」を
/// 順に書き、FIN で閉じる。
///
/// 呼び出し元の [`serve_peer_request`] は、Full Track Name がこの example の配信している
/// 映像 / 音声 track であることを [`published_track`] で検証済みである。そのため namespace は
/// `config.namespace` と一致し、ここでは track の配信有無を判定しない。
async fn serve_media_fetch(
    client: &mut MoqtClient,
    request_id: u64,
    fetch: &Fetch,
    config: &Config,
    media_cache: &mut cache::MediaCache,
) -> Result<()> {
    let track_name = fetch.track_name.as_slice();
    // draft-ietf-moq-transport-23 §3.3.3 (Combining Filters) は publisher に「すべてのフィルタを
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
    let largest = media_cache.largest(&config.namespace, track_name);
    let (start, end) = fetch_request_range(filter.as_ref(), largest);
    // draft-ietf-moq-transport-23 §3.2 (Fetch): Largest Object が要求範囲内なら応答は
    // そこへ切り詰められ、LARGEST_OBJECT を FETCH_OK に載せなければならない。
    // `end` は Largest Object でクランプ済みのため、一致すれば範囲内である。
    let largest_in_range = largest.filter(|largest| *largest == end);
    let plan = media_cache.plan(
        &config.namespace,
        track_name,
        fetch.parameters.group_order(),
        start,
        end,
    );
    let entries = plan.entries();
    match &plan {
        cache::FetchPlan::NoPublishedObjects => {
            // draft-ietf-moq-transport-23 §3.2 は「Object が publish されていない track への
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
            // stream を FIN で閉じる (draft-ietf-moq-transport-23 §3.2.1 (Fetch Object
            // Delivery))
            client
                .send_fetch_ok(
                    request_id,
                    fetch_ok_parameters(largest_in_range),
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
                "Rejecting FETCH for a track that has never been cached: {}",
                String::from_utf8_lossy(track_name),
            );
            client
                .send_request_error(request_id, REQUEST_DOES_NOT_EXIST, "unknown track")
                .await?;
        }
        cache::FetchPlan::UnsupportedGroupOrder => {
            // draft-ietf-moq-transport-23 §3.2.1 (Fetch) は要求された group order で送る MUST を
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
        cache::FetchPlan::AllUnknown { .. } => {
            // 要求 range 全体が不明である。End of Unknown Range を 1 件だけ書く
            // (draft-ietf-moq-transport-23 §11.4.2 (End of Range))。応答範囲の終端は要求と
            // LARGEST_OBJECT から Session が決めるため、ここでは要求 range の終端を渡す
            serve_fetch_entries(
                client,
                request_id,
                track_name,
                end,
                largest_in_range,
                &entries,
            )
            .await?;
        }
        cache::FetchPlan::Serve { .. } => {
            // 応答範囲の終端は要求 range の終端 (Largest Object でクランプ済み)
            serve_fetch_entries(
                client,
                request_id,
                track_name,
                end,
                largest_in_range,
                &entries,
            )
            .await?;
        }
    }
    Ok(())
}

/// FETCH_OK を返し、entry を 1 本の応答 stream に書いて FIN する
///
/// entry の並びは [`cache::FetchPlan::entries`] が決める。End of Range は位置に意味を持つ
/// ため、並びの決定と書き出しを分けてある (並びは単体テストで固定する)。
///
/// `end_location` は応答範囲の終端 (ログ用)、`largest_in_range` は要求範囲内にある
/// Largest Object である。FETCH_OK の応答範囲は Session が要求と LARGEST_OBJECT から導出する
/// ため、範囲内の Largest Object だけを LARGEST_OBJECT として渡す
/// (draft-ietf-moq-transport-23 §3.2 (Fetch))。
async fn serve_fetch_entries(
    client: &mut MoqtClient,
    request_id: u64,
    track_name: &[u8],
    end_location: Location,
    largest_in_range: Option<Location>,
    entries: &[cache::FetchResponseEntry<'_>],
) -> Result<()> {
    client
        .send_fetch_ok(
            request_id,
            fetch_ok_parameters(largest_in_range),
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
                // Priority を変えられない (draft-ietf-moq-transport-23 §12.1 (Malformed Tracks))。
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
/// Timestamp を Unix epoch マイクロ秒として扱うため (draft-ietf-moq-loc-05 §2.3.1.1)、
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
/// (draft-ietf-moq-loc-05 §2.3.1.1 の既定)。
///
/// `timescale` を渡すときはキーフレームに限らず全 Object に `PROP_TIMESCALE` を付ける。
/// Timescale の無い Timestamp は Unix epoch マイクロ秒として解釈されるため
/// (draft-ietf-moq-loc-05 §2.3.1.1 / §2.3.1.2)、一部の Object にだけ付けると購読側が同じ
/// Track の中でメディア時刻と壁時計を取り違える。TIMESCALE の Scope は "Track, Object" であり
/// (§6.1 Table 1)、Object ごとに付与できる。この節番号はドラフト由来であり将来変更される
/// 可能性がある。
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
    if let Some(timescale) = timescale {
        props.push(LocProperty {
            prop_id: PROP_TIMESCALE,
            value: LocPropertyValue::VarInt(timescale),
        });
    }
    // Video Config はデコーダの構成情報であり、キーフレームにだけ付ける
    if frame.is_keyframe
        && let Some(sh) = frame.video_config.as_deref()
    {
        props.push(LocProperty {
            prop_id: PROP_VIDEO_CONFIG,
            value: LocPropertyValue::Bytes(sh.to_vec()),
        });
    }
    props
}

/// 音声 LOC プロパティを構築する
///
/// `timescale` は入力ファイル由来のメディア時刻を送るときだけ渡す。live capture は
/// Unix epoch マイクロ秒を送るため `None` にし、`PROP_TIMESCALE` を付けない
/// (draft-ietf-moq-loc-05 §2.3.1.1 の既定)。
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
        /// draft-ietf-moq-transport-23 §6.6.1 は request stream の GOAWAY を
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
        /// draft-ietf-moq-transport-23 §9.2 (GOAWAY) は「URI が 0 バイトなら現在の URI を
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

    /// 受信した SUBSCRIBE / FETCH の Full Track Name の照合を固定する
    ///
    /// draft-ietf-moq-transport-23 §2.4.1 (Track Naming): Track は Track Namespace と Track Name
    /// の組で識別される。track 名だけで判定すると、別 namespace の要求に自分の namespace の
    /// コンテンツを返してしまう。
    mod published_track_tests {
        use super::*;

        /// `--namespace moq-example` を含むテスト用の設定を組み立てる
        fn config_with(extra: &[&str]) -> Config {
            let mut args = vec![
                "--url",
                "moqt://127.0.0.1:4443",
                "--namespace",
                "moq-example",
            ];
            args.extend_from_slice(extra);
            crate::cli::parse_args(&args)
                .expect("オプションの解析に成功すること")
                .expect("設定が返ること")
        }

        /// フィールド列から Track Namespace を作る
        ///
        /// `--namespace moq-example` は §8.8 の表現として `["moq", "example"]` の 2 フィールドになる。
        fn namespace(fields: &[&str]) -> TrackNamespace {
            TrackNamespace::new(
                fields
                    .iter()
                    .map(|field| field.as_bytes().to_vec())
                    .collect(),
            )
            .expect("テスト用の namespace を作れること")
        }

        /// 自分の namespace の配信中の track は種別を返すこと
        #[test]
        fn own_namespace_returns_the_published_track() {
            let config = config_with(&[]);
            let own = namespace(&["moq", "example"]);
            assert_eq!(
                published_track(&config, &own, b"video"),
                Some(PublishedTrack::Video),
                "既定の映像 track 名は Video であること"
            );
            assert_eq!(
                published_track(&config, &own, AUDIO_TRACK_NAME.as_bytes()),
                Some(PublishedTrack::Audio),
                "音声 track は Audio であること"
            );
            assert_eq!(
                published_track(&config, &own, MSF_CATALOG_TRACK_NAME),
                Some(PublishedTrack::Catalog),
                "カタログ track は Catalog であること"
            );
            assert_eq!(
                published_track(&config, &own, b"unknown"),
                None,
                "配信していない track 名は None であること"
            );
        }

        /// 別 namespace の要求は track 名が同じでも None を返すこと
        #[test]
        fn other_namespace_returns_none() {
            let config = config_with(&[]);
            // 別の namespace、区切りの異なる namespace、自分の namespace の前方一致や延長はどれも別物である
            let others = [
                namespace(&["other", "example"]),
                namespace(&["moq-example"]),
                namespace(&["moq", "example", "sub"]),
                namespace(&["moq"]),
            ];
            for other in &others {
                for name in [
                    b"video".as_slice(),
                    AUDIO_TRACK_NAME.as_bytes(),
                    MSF_CATALOG_TRACK_NAME,
                ] {
                    assert_eq!(
                        published_track(&config, other, name),
                        None,
                        "別 namespace の {} は None であること: {other:?}",
                        String::from_utf8_lossy(name)
                    );
                }
            }
        }

        /// 配信していない映像 / 音声は None を返し、カタログは常に返すこと
        #[test]
        fn disabled_media_returns_none_but_catalog_remains() {
            let own = namespace(&["moq", "example"]);

            let config = config_with(&["--no-video"]);
            assert_eq!(
                published_track(&config, &own, b"video"),
                None,
                "映像を配信しない場合は映像 track が None であること"
            );
            assert_eq!(
                published_track(&config, &own, AUDIO_TRACK_NAME.as_bytes()),
                Some(PublishedTrack::Audio),
                "音声は配信を続けること"
            );
            assert_eq!(
                published_track(&config, &own, MSF_CATALOG_TRACK_NAME),
                Some(PublishedTrack::Catalog),
                "映像が無くてもカタログは返すこと"
            );

            let config = config_with(&["--no-audio"]);
            assert_eq!(
                published_track(&config, &own, AUDIO_TRACK_NAME.as_bytes()),
                None,
                "音声を配信しない場合は音声 track が None であること"
            );
            assert_eq!(
                published_track(&config, &own, b"video"),
                Some(PublishedTrack::Video),
                "映像は配信を続けること"
            );

            // 音声を配信しない場合は映像 track 名に audio を使える。判定の順序によらず Video になる
            let config = config_with(&["--no-audio", "--track-name", "audio"]);
            assert_eq!(
                published_track(&config, &own, AUDIO_TRACK_NAME.as_bytes()),
                Some(PublishedTrack::Video),
                "--track-name audio の映像 track は Video であること"
            );
            assert_eq!(
                published_track(&config, &own, MSF_CATALOG_TRACK_NAME),
                Some(PublishedTrack::Catalog),
                "音声が無くてもカタログは返すこと"
            );
        }

        /// 映像 track 名は `--track-name` に従うこと
        #[test]
        fn custom_video_track_name_is_matched() {
            let config = config_with(&["--track-name", "camera"]);
            let own = namespace(&["moq", "example"]);
            assert_eq!(
                published_track(&config, &own, b"camera"),
                Some(PublishedTrack::Video),
                "--track-name の名前が Video であること"
            );
            assert_eq!(
                published_track(&config, &own, b"video"),
                None,
                "既定名はもう配信していないため None であること"
            );
        }
    }

    /// GOAWAY の適用範囲と New Session URI から publisher の動作を決める判断を固定する
    ///
    /// draft-ietf-moq-transport-23 §6.6.1 (Graceful Session Migration) / §9.2 (GOAWAY)。
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

        /// request stream の GOAWAY が現在の session を指すなら再発行すること
        ///
        /// draft-ietf-moq-transport-23 §9.2 (GOAWAY) は、URI が無い場合は「現在の session」で
        /// 当該 request を再発行することを求める。URI が現在の URL と同じ場合も session は
        /// 変わらないため、同じく再発行する。
        #[test]
        fn request_stream_goaway_for_the_current_session_republishes_the_request() {
            assert_eq!(
                goaway_action(MigrationScope::Request(7), true),
                GoawayAction::RepublishRequest(7),
                "空 URI は現在の session での再発行になること"
            );
        }

        /// request stream の GOAWAY が別の session を指すなら当該 request を移行すること
        ///
        /// draft-ietf-moq-transport-23 §9.2 (GOAWAY) は、New Session URI が指定された場合は
        /// その session で当該 request を再発行することを SHOULD とする。main ループは
        /// 専用タスクが確立した side session で当該 PUBLISH だけを再発行する。同じ移行先
        /// URL を指す複数の PUBLISH は 1 本の side session を共有する。
        #[test]
        fn request_stream_goaway_for_another_session_migrates_the_request() {
            assert_eq!(
                goaway_action(MigrationScope::Request(7), false),
                GoawayAction::MigrateRequest(7),
                "移行先 URI 付きの request 単位移行は side session へ移行すること"
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

        /// track ごとに固定の Track Alias を返すこと
        ///
        /// 同じ track は session を移しても同じ Track Alias を使い続ける
        /// (§3.1.3 (Track Alias))。Unknown は PUBLISH を持たないため alias も無い。
        #[test]
        fn track_alias_is_fixed_per_track() {
            assert_eq!(
                track_alias(PublishTrack::Video),
                Some(VIDEO_TRACK_ALIAS),
                "映像は映像用の alias であること"
            );
            assert_eq!(
                track_alias(PublishTrack::Audio),
                Some(AUDIO_TRACK_ALIAS),
                "音声は音声用の alias であること"
            );
            assert_eq!(
                track_alias(PublishTrack::Catalog),
                Some(CATALOG_TRACK_ALIAS),
                "カタログはカタログ用の alias であること"
            );
            assert_eq!(
                track_alias(PublishTrack::Unknown),
                None,
                "PUBLISH を持たない track は alias を返さないこと"
            );
        }

        /// 移行済みの track だけを移行済みと判定すること
        ///
        /// 移行済みの track は main ループが持つ request id が side session のものになって
        /// いるため、旧 session の GOAWAY の照合から外す。
        #[test]
        fn track_migrated_only_reports_the_migrated_track() {
            assert!(
                track_migrated(PublishTrack::Video, true, false, false),
                "映像が移行済みなら映像を移行済みと判定すること"
            );
            assert!(
                track_migrated(PublishTrack::Audio, false, true, false),
                "音声が移行済みなら音声を移行済みと判定すること"
            );
            assert!(
                track_migrated(PublishTrack::Catalog, false, false, true),
                "カタログが移行済みならカタログを移行済みと判定すること"
            );
            assert!(
                !track_migrated(PublishTrack::Video, false, true, true),
                "映像が移行していなければ他の track の移行に影響されないこと"
            );
            assert!(
                !track_migrated(PublishTrack::Unknown, true, true, true),
                "Unknown は移行済みと判定しないこと"
            );
        }
    }

    /// 同じ side session を共有する移行先 URL のキーを固定する
    ///
    /// 同じ URL を指す PUBLISH は 1 本の side session を共有し、URL が違えば別の session を
    /// 使う。キーは接続先を決める authority・path と、認可トークン (c4m) を含みうる
    /// fragment から作る。
    mod side_session_key_tests {
        use super::*;

        /// テスト用の URL をパースする
        fn url(text: &str) -> ServerUrl {
            tokio_moq::parse_url(text).expect("テスト用の URL をパースできること")
        }

        /// authority・path・fragment が同じ URL は同じキーになること
        #[test]
        fn same_target_url_shares_the_key() {
            assert_eq!(
                side_session_key(&url("moqt://relay.example:443/live#msf:moq-example--video")),
                side_session_key(&url("moqt://relay.example:443/live#msf:moq-example--video")),
                "同じ移行先 URL は同じ side session を共有すること"
            );
        }

        /// fragment が違う URL は別のキーになること
        ///
        /// MSF fragment は track の namespace と認可トークン (c4m) を変えうるため、
        /// authority と path が同じでも fragment が違えば別の side session にする。
        #[test]
        fn different_fragment_uses_another_key() {
            assert_ne!(
                side_session_key(&url("moqt://relay.example:443/live#msf:moq-example--video")),
                side_session_key(&url("moqt://relay.example:443/live#msf:moq-example--audio")),
                "fragment が違う URL は別の side session にすること"
            );
            assert_ne!(
                side_session_key(&url("moqt://relay.example:443/live#msf:moq-example--video")),
                side_session_key(&url("moqt://relay.example:443/live")),
                "fragment の有無でも別の side session にすること"
            );
        }

        /// authority または path が違う URL は別のキーになること
        #[test]
        fn different_authority_or_path_uses_another_key() {
            assert_ne!(
                side_session_key(&url("moqt://relay.example:443/live")),
                side_session_key(&url("moqt://other.example:443/live")),
                "authority が違う URL は別の side session にすること"
            );
            assert_ne!(
                side_session_key(&url("moqt://relay.example:443/live")),
                side_session_key(&url("moqt://relay.example:443/other")),
                "path が違う URL は別の side session にすること"
            );
        }
    }

    /// 移行先の c4m トークンに合わせてカタログ JSON を組み立て直す判断を固定する
    ///
    /// draft-ietf-moq-msf-01 §5.2.42 (Authorization Info): 移行先 URL の `c4m` が変わると
    /// catalog の track に載せる authInfo も変わる。
    mod catalog_rebuild_tests {
        use super::*;

        /// テスト用の CatalogContext (音声トラック 1 本だけを持つ)
        fn audio_only_context() -> CatalogContext {
            CatalogContext {
                namespace_text: "moq-example".to_string(),
                track_name: "video".to_string(),
                sync: catalog::CatalogSyncParams {
                    render_group: CATALOG_RENDER_GROUP,
                    target_latency_ms: 200,
                },
                video: None,
                audio: Some(CatalogAudioTrack {
                    codec: "opus".to_string(),
                    samplerate: AUDIO_SAMPLE_RATE,
                    channel_config: AUDIO_CHANNEL_CONFIG.to_string(),
                    bitrate: 64,
                }),
            }
        }

        /// c4m トークンの有無で authInfo が変わること
        ///
        /// 移行先の c4m が変わったとき、同じ入力から authInfo を載せ替えた JSON を
        /// 組み立てられることを固定する。
        #[test]
        fn catalog_context_json_reflects_c4m_tokens() {
            let context = audio_only_context();

            let without_tokens = context.json(&[]).expect("c4m 無しでも組み立てられること");
            let text = String::from_utf8(without_tokens).expect("UTF-8 の JSON であること");
            assert!(
                !text.contains("authInfo"),
                "c4m が無いときは authInfo を載せないこと: {text}"
            );

            let with_tokens = context
                .json(&[b"publisher-token".to_vec()])
                .expect("c4m 有りでも組み立てられること");
            let text = String::from_utf8(with_tokens).expect("UTF-8 の JSON であること");
            assert!(
                text.contains(r#""authInfo":{"cat":"%c4m%"}"#),
                "c4m があるときは authInfo を載せること: {text}"
            );
            assert!(
                !text.contains("publisher-token"),
                "配信者のトークンそのものはカタログに載せないこと: {text}"
            );
        }

        /// context の映像 / 音声の値がカタログのパラメータへそのまま渡ること
        ///
        /// 起動時のカタログ送信を context 経由に整理しても、初回の送信内容 (track 名 /
        /// namespace / codec / 解像度 / bitrate など) が変わらないことを固定する。
        #[test]
        fn catalog_context_track_params_carry_the_values() {
            let context = CatalogContext {
                namespace_text: "moq-example".to_string(),
                track_name: "video".to_string(),
                sync: catalog::CatalogSyncParams {
                    render_group: CATALOG_RENDER_GROUP,
                    target_latency_ms: 200,
                },
                video: Some(CatalogVideoTrack {
                    codec: "av01.0.08M.08".to_string(),
                    width: 1920,
                    height: 1080,
                    fps: 30,
                    bitrate: 5_000,
                    max_group_duration_ms: Some(2_000),
                }),
                audio: Some(CatalogAudioTrack {
                    codec: "opus".to_string(),
                    samplerate: AUDIO_SAMPLE_RATE,
                    channel_config: AUDIO_CHANNEL_CONFIG.to_string(),
                    bitrate: 64,
                }),
            };

            let (video, audio) = context.track_params();
            let video = video.expect("映像トラックのパラメータがあること");
            assert_eq!(video.track_name, "video", "映像の track 名が渡ること");
            assert_eq!(
                video.namespace, "moq-example",
                "映像の namespace が渡ること"
            );
            assert_eq!(
                video.codec, "av01.0.08M.08",
                "映像の codec 文字列が渡ること"
            );
            assert_eq!(video.width, 1920, "映像の幅が渡ること");
            assert_eq!(video.height, 1080, "映像の高さが渡ること");
            assert_eq!(video.fps, 30, "映像のフレームレートが渡ること");
            assert_eq!(video.bitrate, 5_000, "映像の bitrate が渡ること");
            assert_eq!(
                video.max_group_duration_ms,
                Some(2_000),
                "映像の最大 Group 長が渡ること"
            );

            let audio = audio.expect("音声トラックのパラメータがあること");
            assert_eq!(
                audio.track_name, AUDIO_TRACK_NAME,
                "音声の track 名が渡ること"
            );
            assert_eq!(
                audio.namespace, "moq-example",
                "音声の namespace が渡ること"
            );
            assert_eq!(audio.codec, "opus", "音声の codec 文字列が渡ること");
            assert_eq!(
                audio.samplerate, AUDIO_SAMPLE_RATE,
                "音声のサンプリングレートが渡ること"
            );
            assert_eq!(
                audio.channel_config, AUDIO_CHANNEL_CONFIG,
                "音声の channelConfig が渡ること"
            );
            assert_eq!(audio.bitrate, 64, "音声の bitrate が渡ること");
        }

        /// c4m トークンの並びが同じときだけ再構築しないこと
        #[test]
        fn c4m_tokens_changed_compares_the_token_list() {
            assert!(
                !c4m_tokens_changed(&[b"abc".to_vec()], &[b"abc".to_vec()]),
                "同じ並びなら再構築しないこと"
            );
            assert!(!c4m_tokens_changed(&[], &[]), "両方空なら再構築しないこと");
            assert!(
                c4m_tokens_changed(&[], &[b"abc".to_vec()]),
                "トークンが増えたら再構築すること"
            );
            assert!(
                c4m_tokens_changed(&[b"abc".to_vec()], &[]),
                "トークンが消えたら再構築すること"
            );
            assert!(
                c4m_tokens_changed(&[b"abc".to_vec()], &[b"abd".to_vec()]),
                "トークンの値が変わったら再構築すること"
            );
        }
    }

    /// 移行の PUBLISH 失敗時に接続からやり直す判断と待ち時間を固定する
    ///
    /// draft-ietf-moq-transport-23 §9.2 (GOAWAY) の移行は期限 (Timeout) までに完了させる
    /// 必要がある。待ち時間は期限で cap し、期限が無ければ再試行しない。
    mod migration_retry_tests {
        use super::*;

        /// 指数バックオフで待ち時間が伸び、上限で頭打ちになること
        #[test]
        fn migration_retry_delay_grows_and_is_capped() {
            let remaining = std::time::Duration::from_secs(10);
            assert_eq!(
                migration_retry_delay(0, remaining),
                Some(MIGRATION_RETRY_INITIAL_DELAY),
                "初回は初期待ち時間であること"
            );
            assert_eq!(
                migration_retry_delay(1, remaining),
                Some(MIGRATION_RETRY_INITIAL_DELAY * 2),
                "2 回目は初期待ち時間の 2 倍になること"
            );
            assert_eq!(
                migration_retry_delay(10, remaining),
                Some(MIGRATION_RETRY_MAX_DELAY),
                "上限を超えないこと"
            );
            assert_eq!(
                migration_retry_delay(0, std::time::Duration::from_millis(50)),
                Some(std::time::Duration::from_millis(50)),
                "残り時間が短ければ残り時間で cap すること"
            );
            assert_eq!(
                migration_retry_delay(0, std::time::Duration::ZERO),
                None,
                "残り時間が無ければ再試行しないこと"
            );
        }

        /// プロトコル違反だけは新しい接続でも再試行しないこと
        ///
        /// プロトコル違反とメッセージの encode / decode 失敗は、新しい session でも同じコードで
        /// 再発するため再試行しない。セッション終了や REQUEST_ERROR による拒否は新しい接続で
        /// 解消しうるため再試行する。
        #[test]
        fn migration_publish_failure_retryability() {
            assert!(
                migration_publish_failure_is_retryable(&Error::ConnectionClosed),
                "セッション終了は新しい接続で再試行すること"
            );
            assert!(
                migration_publish_failure_is_retryable(&Error::Other(
                    "request rejected: code=0x4".to_string()
                )),
                "REQUEST_ERROR による拒否は新しい接続で再試行すること"
            );
            assert!(
                !migration_publish_failure_is_retryable(&Error::Moqt(
                    shiguredo_moqt::error::MessageError::ProtocolViolation("invalid catalog")
                )),
                "プロトコル違反は同じコードで再発するため再試行しないこと"
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
    /// `NEW_GROUP_REQUEST` を送れると告知しない (draft-ietf-moq-transport-23 §10.6)。
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
    /// draft-ietf-moq-transport-23 §10.3 (MAX CACHE DURATION): 保持した Object を配れる時間を
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
                subscribe_ok_properties(PublishedTrack::Video, false, cache_duration_ms)
                    .find_varint(PROP_MAX_CACHE_DURATION),
                Some(cache_duration_ms),
                "映像の SUBSCRIBE_OK に同じ値を載せること: {cache_duration_ms}"
            );
            assert_eq!(
                subscribe_ok_properties(PublishedTrack::Audio, false, cache_duration_ms)
                    .find_varint(PROP_MAX_CACHE_DURATION),
                Some(cache_duration_ms),
                "音声の SUBSCRIBE_OK に同じ値を載せること: {cache_duration_ms}"
            );
            assert_eq!(
                subscribe_ok_properties(PublishedTrack::Catalog, false, cache_duration_ms)
                    .find_varint(PROP_MAX_CACHE_DURATION),
                None,
                "カタログの SUBSCRIBE_OK には載せないこと: {cache_duration_ms}"
            );
        }
    }

    /// FETCH の要求 range が Location Filter と Largest Object から決まること
    ///
    /// draft-ietf-moq-transport-23 §3.2 (Fetch) / §3.3.1 (Location Filters) / §9.19.9
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
    /// (draft-ietf-moq-transport-23 §3.5.1)。音声とカタログには載せない。
    #[test]
    fn subscribe_ok_properties_announce_dynamic_groups_for_the_video_track() {
        assert_eq!(
            subscribe_ok_properties(PublishedTrack::Video, false, DEFAULT_CACHE_DURATION_MS)
                .dynamic_groups(),
            Some(1),
            "映像トラックには告知すること"
        );
        assert!(
            subscribe_ok_properties(PublishedTrack::Video, true, DEFAULT_CACHE_DURATION_MS)
                .dynamic_groups()
                .is_none(),
            "MP4 パススルーでは告知しないこと"
        );
        for track in [PublishedTrack::Audio, PublishedTrack::Catalog] {
            assert!(
                subscribe_ok_properties(track, false, DEFAULT_CACHE_DURATION_MS)
                    .dynamic_groups()
                    .is_none(),
                "音声とカタログには DYNAMIC_GROUPS を載せないこと: {track:?}"
            );
        }
        assert_eq!(
            subscribe_ok_properties(PublishedTrack::Catalog, false, DEFAULT_CACHE_DURATION_MS)
                .len(),
            0,
            "カタログには何も載せないこと"
        );
    }

    /// キーフレームを強制できる要求だけを受け付けること
    ///
    /// 値が 0 か現在の Group より大きいときだけ受け付ける
    /// (draft-ietf-moq-transport-23 §9.19.19)。音声やカタログのトラック宛ての要求、および
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
    /// 送れない (draft-ietf-moq-transport-23 §9.19.19)。音声とカタログのトラックには
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
    /// (draft-ietf-moq-loc-05 §2.3.1.1)。delta frame を単体で見ても同じ解釈になる。
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

    /// 非 keyframe の映像 LOC プロパティ: Video Frame Marking / Timestamp / Timescale が付与され、
    /// Video Config は付与されずに encode できること
    ///
    /// MP4 の経路では非 keyframe にも Timescale を付ける。付けないと購読側が Timestamp を
    /// Unix epoch マイクロ秒として解釈する (draft-ietf-moq-loc-05 §2.3.1.1)。
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
            Some(90_000),
            "MP4 の経路では非 keyframe にも Timescale が付与されること"
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
    /// (draft-ietf-moq-loc-05 §2.3.1.1)。
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
