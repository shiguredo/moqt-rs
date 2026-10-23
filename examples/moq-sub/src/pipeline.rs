//! subscriber の全体パイプライン
//!
//! 接続 → SETUP → カタログ購読 (FILL_PARAMETERS 付き SUBSCRIBE) → ビデオ / オーディオ SUBSCRIBE
//! → データストリーム受信
//! → デコード (AV1 / H.264 / H.265 / VP8 / VP9 / Opus / AAC-LC) → フレーム送出の流れを、
//! `shiguredo_moqt::session::core::Session` を駆動する
//! [`tokio_moq::moqt_client::MoqtClient`] と結線する。
//!
//! `--mp4` 指定時は受信したエンコード済みサンプルを `mp4` モジュールの writer へ渡して
//! MP4 ファイルへ保存する。`--mp4` と `--no-play` を併用した場合はデコードをスキップして
//! 保存だけを行う。

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use bytes::Bytes;
use shiguredo_moqt::error::MessageError;
use shiguredo_moqt::loc::{
    LocProperties, LocPropertyValue, PROP_AUDIO_CONFIG, PROP_TIMESCALE, PROP_TIMESTAMP,
    PROP_VIDEO_CONFIG, PROP_VIDEO_FRAME_MARKING,
};
use shiguredo_moqt::pending_subgroup_buffer::{
    DEFAULT_PENDING_SUBGROUP_BUFFER_OPTIONS, PendingNotifyReason, PendingSubgroupBuffer,
};
use shiguredo_moqt::playout::catch_up::{AudioReceiveCatchUp, AudioReceiveCatchUpObservation};
use shiguredo_moqt::session::types::{DatagramAcceptance, SessionError, TrackDataAcceptance};
use shiguredo_moqt::stream::datagram::ObjectDatagram;
use shiguredo_moqt::stream::subgroup::SubgroupHeader;
use shiguredo_moqt::video_decode_order::{
    VideoDecodeOrder, VideoObjectAdmission, VideoObjectPosition, prior_object_id_gap_of,
};
use shiguredo_moqt::{
    message::common::Location, message::common::TrackNamespace, message_parameter::LocationFilter,
    message_parameter::MessageParameter, message_parameter::MessageParameterValue,
    message_parameter::MessageParameters, message_parameter::PARAM_FILL_PARAMETERS,
    message_parameter::PARAM_LOCATION_FILTER, message_parameter::PARAM_NEW_GROUP_REQUEST,
    msf::MSF_CATALOG_TRACK_NAME, msf::MsfTrack, name::parse_namespace, name::serialize_namespace,
    session::types::DataStreamId, session::types::SessionEvent, stream::decoder::DecodedFetchEntry,
    stream::decoder::DecodedFetchObject, stream::decoder::DecodedSubgroupObject,
    stream::decoder::FetchStreamDecoder, stream::decoder::SubgroupStreamDecoder,
};

use crate::catalog;
use crate::cli::Config;
use crate::decoder::{self, AudioDecoder, DecodedAudioFrame, DecodedVideoFrame};
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

/// 購読の Largest Object から遡って FETCH で要求する Group 数
///
/// 途中参加した購読者は購読より前に publish された Object を受け取れない。SUBSCRIBE_OK が
/// 運んだ Largest Object を基準に直近の Group を FETCH で埋める
/// (draft-ietf-moq-transport-22 §3.5.1 (Dynamically Starting New Groups) が示す
/// 「SUBSCRIBE で Largest Object を観測し、Group の先頭まで FETCH で遡る」パターン)。
/// 映像はこの範囲のキーフレームから復号を始められる。
const FETCH_RECENT_GROUPS: u64 = 4;

/// デコード済み映像フレームの送り先
struct FrameSink<'a> {
    frame_tx: &'a std::sync::mpsc::Sender<DecodedVideoFrame>,
    /// 表示待ちのフレーム数 (受信側が増やし、表示側が減らす)
    display_backlog: &'a std::sync::atomic::AtomicI64,
}

/// 音声 Object を送出してよいかの判定結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioDeliveryAdmission {
    /// 送出してよい
    Deliver,
    /// 送出済みの TIMESTAMP より古いため送出しない
    Stale,
}

/// 重複の判定のために覚える直近の音声 Object の識別子の数
///
/// datagram は再送されないため (RFC 9221 §3)、損失に備えて publisher が同じ Object を
/// 複数回送ることがある。同じ Object の 2 回目は直後に届くため、これだけ覚えておけば足りる。
const AUDIO_RECENT_OBJECT_LOCATIONS_MAX: usize = 256;

/// 音声配送の判定で落とした Object の累計
///
/// 再生経路へ流さなかった Object の内訳を、原因の切り分けに使えるようにする。落下は
/// 正常な動作 (datagram の冗長化・購読前の配信・並び替え) でも起きるため、1 件ごとの
/// debug ログだけでは傾向を掴めない。累計を 100 ms の tick で比較し、増えたときだけ
/// info で出す ([`log_audio_delivery_counters`] を参照)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AudioDeliveryCounters {
    /// 直近に届いた Object と同じ識別子だったため捨てた Object の累計
    duplicate_objects: u64,
    /// 送出済みの TIMESTAMP より古いため捨てた Object の累計
    stale_objects: u64,
    /// 購読の境界以下だったため捨てた Object の累計
    before_boundary_objects: u64,
}

/// 音声 Object を送出してよいかを、購読の境界と送出済みの TIMESTAMP で決める状態機械
///
/// 音声は 1 Object = 1 Group で届き (draft-ietf-moq-loc-04 §4.1 (Application with one audio
/// track))、購読より前に publish された Group は購読のあとに FETCH で届く。FETCH の要求 range
/// の終端は `SUBSCRIBE_OK` の Largest Object であり、draft-ietf-moq-transport-22 §3.1.4
/// (Largest Object) は Largest Object が「まだ到着しうる Object」を指すこと、§3.2 (Fetch) は
/// FETCH の range が Largest Object を含む場合にその Object を完全に含めることを定める。
/// したがって境界の位置の Object は live と FETCH の両方で届きうる。
///
/// 判定は 3 段階である。
///
/// - 直近に届いた Object と同じ識別子の 2 回目は捨てる ([`Self::is_duplicate`])。datagram は
///   再送されない (RFC 9221 §3) ため、publisher が同じ Object を n 回送ることがある。
/// - 購読の境界 (`SUBSCRIBE_OK` の Largest Object) 以下は購読より前に publish された分で
///   あり、送出しない ([`Self::is_before_boundary`])。境界の位置の Object が live と FETCH の
///   両方で届いても、どちらも境界以下であるため二重に送出されない。moqt-js の CatchUpGate が
///   `SUBSCRIBE_OK` の `LARGEST_OBJECT` を境界に cache の分を再生しない扱いと同じである。
/// - 境界より後は、送出済みの TIMESTAMP より古い Object を送出しない ([`Self::admit`])。
///   live の datagram は並び替わって届きうる (draft-ietf-moq-transport-22 §2.1.2
///   (Object States) / §2.3.1 (Group IDs)) ため、境界とは別の防御として持つ。
///
/// どちらの判定も位置と TIMESTAMP だけで決まり、時計も I/O も扱わない。購読ごとに 1 つ持ち、
/// 音声の配送経路 (datagram / subgroup stream / FETCH 応答) で共有する。落とした Object の
/// 内訳は累計 ([`Self::counters`]) に残し、原因の切り分けに使う。
#[derive(Debug)]
struct AudioDeliveryOrder {
    /// 購読の境界 (`SUBSCRIBE_OK` の Largest Object)。`None` は境界が無く、すべて live として
    /// 扱う (購読の時点で Object が publish されていない場合)
    boundary: Option<Location>,
    /// 直近に送出した音声の TIMESTAMP (マイクロ秒)。まだ送出していなければ `None`
    last_pts_us: Option<i64>,
    /// 直近に届いた Object の識別子 (古い順)。重複の判定に使う
    ///
    /// `Location` が `Hash` を実装していないため、集合を別に持たず線形に探す。高々
    /// [`AUDIO_RECENT_OBJECT_LOCATIONS_MAX`] 件であり、音声は 1 秒あたり数十件しか届かない。
    recent_locations: std::collections::VecDeque<Location>,
    /// 重複として捨てた Object の累計 (原因の切り分け用)
    duplicate_objects: u64,
    /// 送出済みの TIMESTAMP より古いため捨てた Object の累計 (原因の切り分け用)
    stale_objects: u64,
    /// 購読の境界以下として捨てた Object の累計 (原因の切り分け用)
    before_boundary_objects: u64,
}

impl AudioDeliveryOrder {
    /// 購読の境界を指定して作る (`None` は Largest Object が告知されなかった場合)
    fn new(boundary: Option<Location>) -> Self {
        Self {
            boundary,
            last_pts_us: None,
            recent_locations: std::collections::VecDeque::new(),
            duplicate_objects: 0,
            stale_objects: 0,
            before_boundary_objects: 0,
        }
    }

    /// 購読の境界を設定する
    ///
    /// `None` は `SUBSCRIBE_OK` が Largest Object を告知しなかった場合であり、すべて live と
    /// して扱う (draft-ietf-moq-transport-22 §9.20.17 (LARGEST OBJECT Parameter) は Object が
    /// publish されていれば MUST 含むと定めるため、省略は未 publish を意味する)。
    fn set_boundary(&mut self, boundary: Option<Location>) {
        self.boundary = boundary;
    }

    /// session 移行のために購読の境界だけを更新する
    ///
    /// 旧 session で最後に受信した Object の Location を新しい境界にする。移行先の購読は
    /// その位置を Location Filter の Absolute Start (0x02) にして張り直すため
    /// (draft-ietf-moq-transport-22 §3.3.1 (Location Filters) / §9.20.9 (LOCATION FILTER
    /// Parameter) Table 6。draft 由来の仕様であり将来変更されうる)、境界と同じ位置の
    /// Object だけが再送される。境界を「最後に受信した位置」にすることで、移行中に抜けた
    /// それ以降の音声は live として通り、移行前の音声を再生し直さずに済む。
    ///
    /// 送出済みの TIMESTAMP (`last_pts_us`) と直近の Object の記憶 (`recent_locations`) は
    /// 維持する。移行先は同じ publisher の続きであり TIMESTAMP は単調に進むため、維持すると
    /// 再送された Object を重複として落とし、TIMESTAMP が巻き戻る Object を Stale として
    /// 落とせる。ここで捨てると、再送された移行前の音声を再生し直すことになる。
    fn reset_for_migration(&mut self, boundary: Option<Location>) {
        self.boundary = boundary;
    }

    /// `location` が購読の境界以下かを判定する
    ///
    /// 境界が無い場合と位置が分からない場合は、境界と比べられないため `false` (live 扱い) を
    /// 返す。状態は変えない。境界以下だったときは、原因の切り分け用の累計
    /// (`before_boundary_objects`) を進める。
    fn is_before_boundary(&mut self, location: Option<Location>) -> bool {
        let (Some(boundary), Some(location)) = (self.boundary, location) else {
            return false;
        };
        // `Location` の順序は (group_id, object_id) の辞書順である (§8.2)
        if location <= boundary {
            self.before_boundary_objects += 1;
            return true;
        }
        false
    }

    /// `location` の Object が直近に届いたものと同じかを判定する
    ///
    /// 初めて見る位置なら覚えて `false` を返し、既に覚えている位置なら `true` を返す。
    /// 窓 ([`AUDIO_RECENT_OBJECT_LOCATIONS_MAX`] 件) から溢れた古い位置は忘れる。
    /// 位置が分からない Object (`None`) は判定できないため覚えずに `false` を返す。
    ///
    /// 同じ識別子の Object は copy ごとに中身が同じであるため、2 回目は復号も送出も録画も
    /// しない (捨てないと同じ音を 2 回鳴らし、媒体時刻が実時間より先へ進む)。重複と判定した
    /// ときは、原因の切り分け用の累計 (`duplicate_objects`) を進める。
    fn is_duplicate(&mut self, location: Option<Location>) -> bool {
        let Some(location) = location else {
            return false;
        };
        if self.recent_locations.contains(&location) {
            self.duplicate_objects += 1;
            return true;
        }
        self.recent_locations.push_back(location);
        if self.recent_locations.len() > AUDIO_RECENT_OBJECT_LOCATIONS_MAX {
            self.recent_locations.pop_front();
        }
        false
    }

    /// 購読の境界を返す (ログ用)
    fn boundary(&self) -> Option<Location> {
        self.boundary
    }

    /// TIMESTAMP `pts_us` の音声 Object を送出してよいかを判定する
    ///
    /// `pts_us` が `None` の Object (LOC の TIMESTAMP を持たない) は順序を判定できないため
    /// 送出する。この場合、状態は変えない。送出済みの TIMESTAMP より古いと判定したときは、
    /// 原因の切り分け用の累計 (`stale_objects`) を進める。
    fn admit(&mut self, pts_us: Option<i64>) -> AudioDeliveryAdmission {
        let Some(pts_us) = pts_us else {
            return AudioDeliveryAdmission::Deliver;
        };
        if let Some(last_pts_us) = self.last_pts_us
            && pts_us < last_pts_us
        {
            self.stale_objects += 1;
            return AudioDeliveryAdmission::Stale;
        }
        self.last_pts_us = Some(pts_us);
        AudioDeliveryAdmission::Deliver
    }

    /// 直近に送出した音声の TIMESTAMP (マイクロ秒)。まだ送出していなければ `None`
    fn last_pts_us(&self) -> Option<i64> {
        self.last_pts_us
    }

    /// 落とした Object の累計を返す
    ///
    /// 原因の切り分け用に、100 ms の tick で前回のログから増えていれば info で出す
    /// ([`log_audio_delivery_counters`] を参照)。
    fn counters(&self) -> AudioDeliveryCounters {
        AudioDeliveryCounters {
            duplicate_objects: self.duplicate_objects,
            stale_objects: self.stale_objects,
            before_boundary_objects: self.before_boundary_objects,
        }
    }
}

/// 音声配送の累計が前回のログから増えていれば info で出す
///
/// 落とした Object の内訳は原因の切り分けに使う。1 件ごとの debug ログは既定の RUST_LOG では
/// 消えるため、累計が増えたときだけ info に出す。`previous` は前回ログを出した時点の累計で、
/// 増えていないときは更新しない (同じ内容を出し続けてログを埋もれさせないため)。
fn log_audio_delivery_counters(
    previous: &mut AudioDeliveryCounters,
    current: AudioDeliveryCounters,
) {
    if current == *previous {
        return;
    }
    tracing::info!(
        "Audio delivery counters: duplicated={}, stale={}, before_boundary={}",
        current.duplicate_objects,
        current.stale_objects,
        current.before_boundary_objects,
    );
    *previous = current;
}

/// datagram で届いた音声 Object の位置
///
/// OBJECT_DATAGRAM のヘッダが位置を持つ (draft-ietf-moq-transport-22 §11.2.1)。
fn datagram_object_location(datagram: &ObjectDatagram) -> Location {
    Location {
        group_id: datagram.group_id,
        object_id: datagram.object_id,
    }
}

/// subgroup stream で届いた音声 Object の位置
///
/// Group ID は Subgroup Header が持ち (draft-ietf-moq-transport-22 §11.3.1)、Object ID は
/// Subgroup 内のデルタが解決済みの値を使う。呼び出し側が別の Group ID を渡せないよう、
/// Header そのものを受け取る。
fn subgroup_object_location(header: &SubgroupHeader, object: &DecodedSubgroupObject) -> Location {
    Location {
        group_id: header.group_id,
        object_id: object.object_id,
    }
}

/// FETCH の応答で届いた音声 Object の位置
///
/// FETCH の応答は Group をまたぐため、位置は Object ごとに届く
/// (draft-ietf-moq-transport-22 §11.4.1 (Fetch Header))。
fn fetch_object_location(object: &DecodedFetchObject) -> Location {
    Location {
        group_id: object.group_id,
        object_id: object.object_id,
    }
}

/// 音声 Object を再生経路へ流すための共有状態
///
/// 音声 Object は subgroup stream と datagram の両方で届く (draft-ietf-moq-loc-04 §4.1)。
/// 経路に依存しない値をここへまとめ、重複の判定 → 購読境界の判定 → 録画 → Audio Config の
/// 検証 → 送出順の判定 → デコード → catch-up の観測 → デコード済みフレームの送出という順序が
/// 経路ごとに食い違わないようにする。重複と境界以下は Audio Config の検証だけを行い、復号・
/// 送出・録画・catch-up の観測をしない。
struct AudioDelivery<'a> {
    /// デコード済み音声フレームの送り先
    audio_tx: &'a std::sync::mpsc::Sender<DecodedAudioFrame>,
    /// Audio Config (OpusHead) を処理済みかどうか (購読で 1 つ共有する)
    audio_config_handled: &'a std::sync::atomic::AtomicBool,
    /// catalog の codec が Opus かどうか (Audio Config の中身は codec 依存のため)
    is_opus_codec: bool,
    /// catalog が宣言したサンプルレート (Audio Config の検証に使う)
    catalog_sample_rate: u32,
    /// catalog が宣言したチャンネル数 (Audio Config の検証に使う)
    catalog_channels: u8,
    /// 録画先 (`--mp4` 指定時のみ)
    recorder: Option<&'a RecorderSender>,
    /// 書式違反によるセッション終了を main ループへ依頼するチャネル
    termination_tx: &'a tokio::sync::mpsc::Sender<SessionError>,
    /// 受信側の音声の追いつき (購読で 1 つ共有する)
    ///
    /// 復号器の中に溜まった音で遅れが固定されたら、復号器を作り直して live へ戻す。
    /// 状態はストリーム経路と datagram 経路で共有する。
    catch_up: &'a std::sync::Mutex<AudioReceiveCatchUp>,
    /// 音声の購読境界と送出順の判定 (購読で 1 つ共有する)
    ///
    /// 購読より前に publish された Group は購読のあとに FETCH で届く。`SUBSCRIBE_OK` の
    /// Largest Object 以下の音声は送出も録画も復号もしない (境界の位置の Object が live と
    /// FETCH の両方で届いても二重にならない)。同じ識別子の Object の 2 回目 (datagram の
    /// 冗長化による copy) も落とす。境界より後は、送出済みの TIMESTAMP より古い音声を落とし、
    /// 再生経路の TIMESTAMP が巻き戻らないようにする。状態は datagram 経路と
    /// subgroup stream 経路、FETCH 応答経路で共有する。
    delivery_order: &'a std::sync::Mutex<AudioDeliveryOrder>,
}

impl AudioDelivery<'_> {
    /// 音声 Object 1 つを再生経路へ流す
    ///
    /// `location` は Object の位置であり、購読の境界と比べて
    /// 購読より前に publish された音声を落とす。位置が分からない場合は境界と比べられないため
    /// live として扱う。直近に届いた Object と同じ識別子であれば (publisher が datagram を
    /// 冗長化して送った 2 回目)、購読の境界と比べる前に捨てる。
    ///
    /// `decoder` が `None` のとき (再生しない指定、デコーダを生成できないとき) は
    /// 録画だけを行う (購読の境界以下は Audio Config の検証だけを行い、録画も復号もしない)。
    /// 返り値は呼び出し側が処理を継続してよいかどうかで、`false` は LOC Properties の書式違反で
    /// セッション終了を依頼したか、デコード済みフレームの送り先が閉じたことを表す。
    ///
    /// Properties の書式違反は「配送する Object」だけを対象にする。フィルタ不通過などで
    /// 配送しない Object は呼び出し側がこの関数を呼ばない。
    ///
    /// 復号の出力を観測するときは、受信の入口の入力の遅れ (Object の到着 − LOC TIMESTAMP) も
    /// 渡す。判定は、出力の遅れから入力の遅れを引いた「復号が足している遅れ」で行う
    /// ([`shiguredo_moqt::playout::catch_up`] を参照)。
    async fn deliver(
        &self,
        location: Option<Location>,
        properties_bytes: Option<&[u8]>,
        payload: &[u8],
        decoder: Option<&mut AudioDecoder>,
    ) -> bool {
        // 書式違反は配送の有無や処理済みフラグに関わらず毎 Object で検出するため、
        // ここでは常に抽出する (検証は下の swap で 1 回に絞る)
        let audio_config = match extract_audio_config(properties_bytes) {
            Ok(config) => config,
            Err(e) => {
                request_session_termination(self.termination_tx, &e);
                return false;
            }
        };
        let (timestamp, timescale) = match extract_timestamp_timescale(properties_bytes) {
            Ok(ts) => ts,
            Err(e) => {
                request_session_termination(self.termination_tx, &e);
                return false;
            }
        };
        // 重複の判定と購読の境界の判定は録画より前に行う。境界以下の音声は購読より前に
        // publish された分であり、送出も録画も復号もしない (境界の位置の Object が live と
        // FETCH の両方で届いても、どちらもここで落ちるため二重にならない)。Audio Config の
        // 検証だけは Properties から行うため、この時点では return しない。
        //
        // 同じ識別子の Object を publisher が複数回送ることがある (datagram は再送されない
        // ため、RFC 9221 §3)。2 回目はここで捨て、録画・Audio Config の検証・復号・送出へ
        // 渡さない。覚えるのは最初の 1 回だけで、重複した到着では窓を変えない
        let (duplicate, before_boundary, boundary) = {
            let mut delivery_order = self
                .delivery_order
                .lock()
                .expect("audio delivery order lock is not poisoned");
            let duplicate = delivery_order.is_duplicate(location);
            // 重複は境界と比べる前に落とすため、重複した Object で境界以下の累計を二重に
            // 数えない (累計は「落とした理由」の内訳として読む)
            let before_boundary = !duplicate && delivery_order.is_before_boundary(location);
            (duplicate, before_boundary, delivery_order.boundary())
        };
        if duplicate {
            // 同じ Object の 2 回目は直後に届くため warn にはしない
            tracing::debug!("Skipping a duplicated audio object: location={location:?}");
            return true;
        }
        if before_boundary {
            // 購読のたびに数 Object 出るため warn にはしない
            tracing::debug!(
                "Skipping an audio object before the subscription boundary: \
                 location={location:?}, boundary={boundary:?}"
            );
        } else if let Some(recorder) = self.recorder {
            record_audio_object(
                recorder,
                timestamp,
                timescale,
                audio_config.as_deref(),
                self.is_opus_codec,
                payload,
            );
        }
        // Audio Config (OpusHead) を含む Object を受信した最初の 1 回のみ検証する
        // (途中参加で OpusHead を受信しない場合はスキップされる)。subgroup stream と
        // datagram のどちらで届いても 1 回で済むよう、フラグは購読で共有する
        if let Some(config_bytes) = audio_config.as_deref()
            && !self.audio_config_handled.swap(true, Ordering::AcqRel)
        {
            if self.is_opus_codec {
                match parse_opus_head(config_bytes) {
                    Some(head) => {
                        for warning in validate_audio_config(
                            &head,
                            self.catalog_sample_rate,
                            self.catalog_channels,
                        ) {
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
                // AAC (mp4a.*) の Audio Config は AudioSpecificConfig であり、OpusHead として
                // 解釈しない。AudioToolbox のデコーダは catalog のサンプルレートと
                // チャンネル数だけで構成できる
                tracing::debug!(
                    "Audio Config is not interpreted for codecs other than opus: len={}",
                    config_bytes.len(),
                );
            }
        }
        if before_boundary {
            // 境界以下の音声は復号しない。復号器は datagram / subgroup stream / FETCH 応答で
            // 1 つを共有しており、live の音声を復号したあとに古い音声で状態を進めると、次に
            // 届く live の音声が不連続になるためである (Audio Config は Properties から検証
            // できるため、復号しなくても適用は落ちない)。送出も録画もしない。catch-up の観測へも
            // 渡さない (古い TIMESTAMP で遅れが跳ね、復号器が作り直される)
            return true;
        }
        let Some(decoder) = decoder else {
            return true;
        };
        let pts_us = loc_timestamp_us(timestamp, timescale);
        // 送出順の判定はデコードの前に行う。送出済みの TIMESTAMP より古い音声で復号器の
        // 状態を進めると、次に届く live の音声が不連続になるためである。ロックは await を
        // またがない (判定だけの同期処理)
        let (admission, previous_pts_us) = {
            let mut delivery_order = self
                .delivery_order
                .lock()
                .expect("audio delivery order lock is not poisoned");
            let previous_pts_us = delivery_order.last_pts_us();
            (delivery_order.admit(pts_us), previous_pts_us)
        };
        if admission == AudioDeliveryAdmission::Stale {
            // 境界より後の live の音声が並び替わって届いた場合である (draft-ietf-moq-transport-22
            // §2.1.2 (Object States) / §2.3.1 (Group IDs))。購読より前に publish された音声は
            // 境界の判定で既に落ちている。異常ではないため warn にはしない
            tracing::debug!(
                "Skipping an audio object that is older than the delivered frame: \
                 pts_us={pts_us:?}, last_pts_us={previous_pts_us:?}"
            );
            return true;
        }
        // 受信の入口で測った入力の遅れを求めるため、復号へ渡す直前の到着の壁時計を残す。
        // 復号は入力と 1 対 1 で対応するため (Opus は同期、AAC は専用スレッドの完了待ち)、
        // この観測はそのまま復号の出力の観測と対応する
        let arrival_us = crate::wall_clock_us();
        let pcm = match decoder.decode(payload).await {
            Ok(pcm) => pcm,
            Err(e) => {
                tracing::warn!("Audio decode error: {e}, payload_len={}", payload.len());
                return true;
            }
        };
        // 復号の出力が揃った時刻を測り、フレームへ載せる。受信側のプレイヤーは時間軸を
        // この時刻で観測するため、プレイヤースレッドがキューから取り出した時刻を混ぜない
        // (復号と無関係なスレッドの揺れが遅延学習へ入る)
        let decoded_us = crate::wall_clock_us();
        // サンプルレートとチャンネル数はデコーダの出力を使う。AAC は AudioToolbox の出力が
        // ステレオ固定で catalog の channelConfig と食い違うことがあるが、そのまま再生を
        // 継続する (validate_audio_config の警告と同じ方針)
        let sample_rate = decoder.sample_rate();
        let channels = decoder.channels();
        // 復号の出力が送られた TIMESTAMP からどれだけ遅れて出てきたかを観測する。復号器の
        // 中に溜まった音で遅れが固定されたら、復号器を作り直して live へ戻す。壁時計の
        // TIMESTAMP を持つ音でだけ、受信の入口の入力の遅れ (Object の到着 − TIMESTAMP) を
        // 測って渡す。判定は、出力の遅れから入力の遅れを引いた「復号が足している遅れ」で
        // 行うため、経路が遅いだけの状態では作り直さない
        if let Some(pts_us) = pts_us {
            let now_us = crate::wall_clock_us();
            let lag_ms = now_us.saturating_sub(pts_us) / 1_000;
            let input_lag_ms = receive_input_lag_ms(timestamp, timescale, arrival_us, pts_us);
            // ロックは await をまたがない (観測と判定だけの同期処理)
            let (decision, stats) = {
                let mut catch_up = self
                    .catch_up
                    .lock()
                    .expect("audio catch up lock is not poisoned");
                let decision = catch_up.observe(AudioReceiveCatchUpObservation {
                    now_ms: now_us / 1_000,
                    lag_ms,
                    input_lag_ms,
                    duration_ms: pcm_duration_ms(pcm.len(), usize::from(channels), sample_rate),
                    timestamp_us: pts_us,
                });
                (decision, catch_up.snapshot())
            };
            if decision.rebuild_decoder {
                tracing::info!(
                    "Rebuilding the audio decoder to catch up (lag {}ms, input_lag {}ms, decode_lag {}ms, starts {})",
                    lag_ms,
                    optional_ms_log(stats.input_lag_ms),
                    optional_ms_log(stats.decode_lag_ms),
                    stats.catch_up_starts,
                );
                if let Err(e) = decoder.reset() {
                    tracing::warn!("Failed to rebuild the audio decoder: {e}");
                }
            }
        }
        let frame = DecodedAudioFrame {
            pcm,
            sample_rate,
            channels,
            pts_us: pts_us.unwrap_or(0),
            arrival_us,
            decoded_us,
        };
        self.audio_tx.send(frame).is_ok()
    }
}

/// PCM (S16 interleaved) の長さをミリ秒で返す
///
/// チャンネル数とサンプルレートが 0 のときは 0 を返す。
fn pcm_duration_ms(pcm_len: usize, channels: usize, sample_rate: u32) -> i64 {
    if channels == 0 || sample_rate == 0 {
        return 0;
    }
    let frames = pcm_len / channels;
    i64::try_from(frames)
        .unwrap_or(i64::MAX)
        .saturating_mul(1_000)
        / i64::from(sample_rate)
}

/// ミリ秒の観測値をログ用の文字列にする
///
/// 測れなかった値 (`None`) は `none` にする。追いつきの観測値は測れない場合があり、そのまま
/// 数を出すと測れなかったことがログから分からなくなるためである。
fn optional_ms_log(value: Option<i64>) -> String {
    value.map_or_else(|| "none".to_string(), |value| value.to_string())
}

/// stream task の終了を待つ上限
///
/// STOP_SENDING を送っても peer が data stream を reset しない場合に、shutdown の完了
/// (録画中は finalize) に到達できなくならないようにするための保険。
const STREAM_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 移行の切替で旧 session の stream task を打ち切ってから回収するまでの上限
///
/// 切替の前に旧世代の stream task が frame / audio の sink へ書かないことを保証するため、
/// `JoinSet::abort_all` のあとに回収する。abort は await 点で効くため、block_in_place で
/// デコード中の task が戻るまでの短い猶予としてこの上限を使う。超えた場合は警告を残して
/// 切替を続ける (残った task は旧 data_plane しか触らない)。移行はまれな出来事であり、
/// この間 main ループが止まることは許容する。
const MIGRATION_STREAM_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// GOAWAY 移行の連鎖を抑える時間窓
///
/// 移行先がまた GOAWAY を返す構成では、移行を無制限に繰り返すと再接続ストームになる。
/// 直近のこの期間に [`MIGRATION_LIMIT_MAX`] 回を超える移行は拒否する。
const MIGRATION_LIMIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(600);

/// [`MIGRATION_LIMIT_WINDOW`] の中で許す移行回数
const MIGRATION_LIMIT_MAX: usize = 5;

/// 移行の購読張り直しに失敗したときの再試行で、最初に待つ時間
const MIGRATION_RETRY_INITIAL_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// 移行の購読張り直しに失敗したときの再試行で待つ時間の上限
///
/// 期限が長い場合でも、失敗した移行先へ間隔を空けずに接続し直さないようにする。
const MIGRATION_RETRY_MAX_DELAY: std::time::Duration = std::time::Duration::from_millis(1000);

/// 移行の 1 回の購読張り直しに与える時間の上限
///
/// 残り時間の全部を 1 回の試行に使うと、張り直しがタイムアウトした時点で期限も尽きて
/// 再試行できなくなる。張り直しは SUBSCRIBE の応答待ちだけであり数秒で終わるため、
/// 1 回の試行に上限を設けて再試行の余地を残す。
const MIGRATION_RESUBSCRIBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
use tokio_moq::MigrationDeadline;
use tokio_moq::MigrationLimiter;
use tokio_moq::MigrationScope;
use tokio_moq::RetryPolicy;
use tokio_moq::Transport;
use tokio_moq::error::TransportError;
use tokio_moq::error::session_error_code;
use tokio_moq::moqt_client::{
    ClientEvent, DataPlaneHandle, MoqtClient, StreamRead, SubscribeResult,
};
use tokio_moq::transport;
use tokio_moq::webtransport::WtSessionState;

/// カタログから取得したビデオトラック情報
struct VideoTrackInfo {
    /// 購読に使う namespace
    ///
    /// draft-ietf-moq-msf-01 §5.2.2 (Track namespace) は track が宣言した namespace が
    /// catalog track の namespace を上書きすると定めるため、宣言があればそれを使う。
    namespace: TrackNamespace,
    track_name: String,
    codec: String,
    width: u32,
    height: u32,
    fps: u32,
}

/// カタログから取得した音声トラック情報
struct AudioTrackInfo {
    /// 購読に使う namespace (解決の規則は [`VideoTrackInfo::namespace`] と同じ)
    namespace: TrackNamespace,
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
    media_fetch_targets: &HashMap<u64, TrackKind>,
    data_plane: &DataPlaneHandle,
    frame_tx: &std::sync::mpsc::Sender<DecodedVideoFrame>,
    audio_tx: &std::sync::mpsc::Sender<DecodedAudioFrame>,
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
    audio_params: Option<(u32, u8)>,
    display_backlog: &std::sync::Arc<std::sync::atomic::AtomicI64>,
    video_decode_order: &std::sync::Arc<tokio::sync::Mutex<VideoDecodeOrder>>,
    audio_decoder: Option<&std::sync::Arc<tokio::sync::Mutex<AudioDecoder>>>,
    audio_config_handled: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    audio_catch_up: &std::sync::Arc<std::sync::Mutex<AudioReceiveCatchUp>>,
    audio_delivery_order: &std::sync::Arc<std::sync::Mutex<AudioDeliveryOrder>>,
    last_received_locations: &std::sync::Arc<std::sync::Mutex<LastReceivedLocations>>,
    termination_tx: &tokio::sync::mpsc::Sender<SessionError>,
    catalog_tx: &tokio::sync::mpsc::Sender<catalog::CatalogObject>,
    pending_subgroups: &SharedPendingSubgroupBuffer,
    playback: bool,
    recorder: Option<&RecorderSender>,
) {
    let track_map = track_map.clone();
    let media_fetch_targets = media_fetch_targets.clone();
    let data_plane = data_plane.clone();
    let frame_tx = frame_tx.clone();
    let audio_tx = audio_tx.clone();
    let codec = video_codec.map(str::to_owned);
    let audio_codec = audio_codec.map(str::to_owned);
    let backlog = std::sync::Arc::clone(display_backlog);
    let video_decode_order = std::sync::Arc::clone(video_decode_order);
    let audio_decoder = audio_decoder.cloned();
    let audio_config_handled = std::sync::Arc::clone(audio_config_handled);
    let audio_catch_up = std::sync::Arc::clone(audio_catch_up);
    let audio_delivery_order = std::sync::Arc::clone(audio_delivery_order);
    let last_received_locations = std::sync::Arc::clone(last_received_locations);
    let termination_tx = termination_tx.clone();
    let catalog_tx = catalog_tx.clone();
    let pending_subgroups = std::sync::Arc::clone(pending_subgroups);
    let recorder = recorder.cloned();
    join_set.spawn(task_monitor.clone().instrument(async move {
        handle_incoming_stream(
            stream,
            data_plane,
            &track_map,
            &media_fetch_targets,
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
            &audio_catch_up,
            &audio_delivery_order,
            &last_received_locations,
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
    /// 受信に使う (draft-ietf-moq-msf-01 §5 (Catalog))。購読で届くカタログは、stream task が
    /// 読み出した Object を main ループへ渡し、`CatalogState` を所有する main ループが
    /// MSF の配置規則で適用する。
    Catalog,
}

/// 移行先の session で張り直す購読の指定
#[derive(Clone)]
struct ResubscribePlan {
    /// catalog track の namespace
    catalog_namespace: TrackNamespace,
    /// 映像トラック (購読する場合)
    video: Option<SubscribeTarget>,
    /// 音声トラック (購読する場合)
    audio: Option<SubscribeTarget>,
}

/// 購読する track の指定
#[derive(Clone)]
struct SubscribeTarget {
    /// 購読に使う namespace
    namespace: TrackNamespace,
    /// track 名
    track_name: Vec<u8>,
}

/// 張り直した購読の結果
struct Resubscribed {
    /// catalog の購読
    catalog: SubscribeResult,
    /// 映像の購読 (購読する場合)
    video: Option<SubscribeResult>,
    /// 音声の購読 (購読する場合)
    audio: Option<SubscribeResult>,
    /// 移行先で発行したメディア FETCH の対応表 (Request ID → トラック種別)
    ///
    /// Request ID は session ごとに払い出されるため、旧 session の対応表は移行先では
    /// 使えない。移行に成功した時点でこの表へ差し替える
    media_fetch_targets: HashMap<u64, TrackKind>,
}

/// track ごとに最後に配送した Object の Location
///
/// セッション移行では、再購読の Location Filter を「最後に届いた Object の位置」から
/// 始めることで、移行中に publish された Object の取りこぼしを抑える
/// (draft-ietf-moq-transport-22 §3.3.1 (Location Filters) / §9.20.9 (LOCATION FILTER
/// Parameter) Table 6 の Type 0x02 (Absolute Start))。Object は各 stream task が配送経路へ
/// 流すため、`Arc<Mutex<...>>` で共有し、配送のたびに最大値を記録する。
///
/// 記録するのは「配送経路へ流した Object」に限る。フィルタで落とした Object や payload を
/// 持たない Object は、再開位置として意味を持たないため記録しない。Group ID / Object ID は
/// 単調に増えるとは限らない (Group Order が降順の publisher もいる) ため、単純な代入ではなく
/// 最大値を残す。`Location` の順序は (group_id, object_id) の辞書順である (§8.2)。
///
/// draft 由来の仕様に依存する値であり、将来変更される可能性がある。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LastReceivedLocations {
    /// catalog track で最後に配送した Object の Location
    catalog: Option<Location>,
    /// 映像 track で最後に配送した Object の Location
    video: Option<Location>,
    /// 音声 track で最後に配送した Object の Location
    audio: Option<Location>,
}

impl LastReceivedLocations {
    /// 指定した track 種別の最後の Location を返す
    fn get(&self, kind: TrackKind) -> Option<Location> {
        match kind {
            TrackKind::Catalog => self.catalog,
            TrackKind::Video => self.video,
            TrackKind::Audio => self.audio,
        }
    }

    /// 配送した Object の Location を記録する
    ///
    /// 同じ track で既に記録した位置より新しい場合だけ更新する。
    fn record(&mut self, kind: TrackKind, location: Location) {
        let slot = match kind {
            TrackKind::Catalog => &mut self.catalog,
            TrackKind::Video => &mut self.video,
            TrackKind::Audio => &mut self.audio,
        };
        *slot = Some(match *slot {
            Some(current) => current.max(location),
            None => location,
        });
    }
}

/// track ごとの最後の Location を共有状態へ記録する
///
/// stream task と main ループの両方から呼ぶ。ロックは await をまたがない短い同期処理で
/// 完結するため `std::sync::Mutex` を使う (`AudioReceiveCatchUp` などと同じ方針)。
fn record_last_location(
    last_locations: &std::sync::Mutex<LastReceivedLocations>,
    kind: TrackKind,
    location: Location,
) {
    last_locations
        .lock()
        .expect("last locations lock is not poisoned")
        .record(kind, location);
}

/// 移行先の session で購読を張り直す
///
/// catalog は FILL_PARAMETERS 付き SUBSCRIBE で張り直す (draft-ietf-moq-msf-01 §5 (Catalog)
/// が catalog の取得に求める経路)。映像は SUBSCRIBE のあとに SUBSCRIBER_PRIORITY の
/// REQUEST_UPDATE を送る (起動時と同じ手順)。音声は SUBSCRIBE だけである。
/// どちらの track も、購読のあとに直近 Group の FETCH を発行する (起動時と同じ手順)。
///
/// `last_locations` は前の session で最後に配送した Object の位置である。映像と音声は
/// これを Location Filter の Absolute Start (0x02) として SUBSCRIBE に載せ、移行中に
/// publish された Object を取得し直す (draft-ietf-moq-transport-22 §3.3.1 / §9.20.9。
/// draft 由来の仕様であり将来変更されうる)。位置が分からない場合 (まだ 1 つも配送して
/// いない) はフィルタを載せず、起動時と同じ「購読後の live のみ」になる。
///
/// catalog だけは位置を使わない。catalog は「最新の完全なカタログと、それに続く delta」を
/// 状態へ適用し続ける必要があり (draft-ietf-moq-msf-01 §5 (Catalog))、移行中に更新が
/// 抜けた場合に delta だけを再開すると状態が壊れる。起動時と同じ Next Object の購読に
/// 戻し、移行後に publish される次のカタログ (独立カタログまたは delta) を受け直す。
/// なお移行後の fill fetch stream は受信ループの stream task が読み捨てるため (起動時の
/// `receive_catalog` だけが fill を読む)、移行直後の完全なカタログの再取得は次の publish に
/// 依存する。この制約は移行前からあるものであり、本変更では広げていない。
///
/// track alias は peer が SUBSCRIBE_OK で決めるため、起動時と同じ値になる保証はない。
/// 返った alias で `track_map` を更新するのは呼び出し側の責務である。
async fn resubscribe_tracks(
    client: &mut MoqtClient,
    plan: &ResubscribePlan,
    last_locations: &LastReceivedLocations,
) -> Result<Resubscribed> {
    let catalog = client
        .subscribe_track_with_parameters(
            plan.catalog_namespace.clone(),
            MSF_CATALOG_TRACK_NAME.to_vec(),
            catalog_subscribe_parameters(),
        )
        .await?;
    tracing::info!(
        "Resubscribed to catalog track: alias={}, request_id={}",
        catalog.track_alias,
        catalog.request_id,
    );

    let mut media_fetch_targets: HashMap<u64, TrackKind> = HashMap::new();
    // catalog 購読の fill fetch stream をカタログとして適用するため、Request ID を登録する
    // (移行中に更新された catalog が fill の完全な catalog で届く)
    media_fetch_targets.insert(catalog.request_id, TrackKind::Catalog);
    let video = match &plan.video {
        Some(target) => {
            let sub = client
                .subscribe_track_with_parameters(
                    target.namespace.clone(),
                    target.track_name.clone(),
                    video_subscribe_parameters(last_locations.get(TrackKind::Video)),
                )
                .await?;
            tracing::info!(
                "Resubscribed to video track: alias={}, request_id={}, filter_start={:?}",
                sub.track_alias,
                sub.request_id,
                last_locations.get(TrackKind::Video),
            );
            // 起動時と同じく SUBSCRIBER_PRIORITY を送る。C4M 認可トークンは
            // `MoqtClient::send_request_update` が付ける (draft-ietf-moq-msf-01 §11.4.3)
            let mut params = shiguredo_moqt::message_parameter::MessageParameters::new();
            params.push(shiguredo_moqt::message_parameter::MessageParameter {
                param_type: shiguredo_moqt::message_parameter::PARAM_SUBSCRIBER_PRIORITY,
                value: shiguredo_moqt::message_parameter::MessageParameterValue::Uint8(192),
            });
            if let Err(e) = client.send_request_update(sub.request_id, params).await {
                tracing::warn!("Failed to send REQUEST_UPDATE for video subscription: {e}");
            }
            // FETCH は購読の優先度を変えたあとに発行する。先に発行すると、FETCH 応答の
            // Object が既定の SUBSCRIBER_PRIORITY で届く
            // (draft-ietf-moq-transport-22 §9.20.7 (SUBSCRIBER PRIORITY Parameter) / §9.11)
            fetch_recent_groups(
                client,
                &mut media_fetch_targets,
                target,
                TrackKind::Video,
                sub.largest_object,
            )
            .await;
            Some(sub)
        }
        None => None,
    };

    let audio = match &plan.audio {
        Some(target) => {
            let sub = client
                .subscribe_track_with_parameters(
                    target.namespace.clone(),
                    target.track_name.clone(),
                    audio_subscribe_parameters(last_locations.get(TrackKind::Audio)),
                )
                .await?;
            tracing::info!(
                "Resubscribed to audio track: alias={}, request_id={}, filter_start={:?}",
                sub.track_alias,
                sub.request_id,
                last_locations.get(TrackKind::Audio),
            );
            fetch_recent_groups(
                client,
                &mut media_fetch_targets,
                target,
                TrackKind::Audio,
                sub.largest_object,
            )
            .await;
            Some(sub)
        }
        None => None,
    };

    Ok(Resubscribed {
        catalog,
        video,
        audio,
        media_fetch_targets,
    })
}

/// 映像トラックの SUBSCRIBE に載せるメッセージパラメータ
///
/// 購読の確立と同時に新しい Group の開始を要求する。`NEW_GROUP_REQUEST=0` は「購読者が
/// Group の情報を持たない」ことを示し、publisher はできるだけ早く新しい Group を開始する
/// (draft-ietf-moq-transport-22 §9.20.19)。Group の途中で参加した購読者は、次の周期
/// キーフレームを待たずに復号を始められる。同節は DYNAMIC_GROUPS の告知が無くても
/// SUBSCRIBE に載せてよいと定める。
///
/// `last` は前の session で最後に配送した Object の位置である。移行後の再購読では
/// Location Filter の Absolute Start (Table 6 の Type 0x02) として載せ、移行中に publish
/// された Object を取得し直す (§3.3.1 (Location Filters) / §9.20.9 (LOCATION FILTER
/// Parameter))。フィルタは境界の位置の Object を含むため同じ Object が再送されうるが、
/// 映像の復号順の判定 ([`VideoDecodeOrder`]) が復号済みの位置を捨てる。draft 由来の
/// 仕様であり、将来変更される可能性がある。
fn video_subscribe_parameters(last: Option<Location>) -> MessageParameters {
    let mut parameters = MessageParameters::new();
    if let Some(last) = last {
        parameters.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LocationFilter(LocationFilter::AbsoluteStart {
                start: last,
            }),
        });
    }
    parameters.push(MessageParameter {
        param_type: PARAM_NEW_GROUP_REQUEST,
        value: MessageParameterValue::VarInt(0),
    });
    parameters
}

/// 音声トラックの SUBSCRIBE に載せるメッセージパラメータ
///
/// 起動時の購読は「購読後の live のみ」でありパラメータを載せない。移行後の再購読では、
/// [`video_subscribe_parameters`] と同じ理由で Location Filter の Absolute Start (0x02)
/// を載せ、移行中に publish された音声を取得し直す
/// (draft-ietf-moq-transport-22 §3.3.1 / §9.20.9 Table 6)。音声も境界の位置の Object が
/// 再送されうるが、[`AudioDeliveryOrder`] が直近に届いた Object の重複を捨てる。
/// draft 由来の仕様であり、将来変更される可能性がある。
fn audio_subscribe_parameters(last: Option<Location>) -> MessageParameters {
    let mut parameters = MessageParameters::new();
    if let Some(last) = last {
        parameters.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LocationFilter(LocationFilter::AbsoluteStart {
                start: last,
            }),
        });
    }
    parameters
}

/// 購読した track の直近 Group を FETCH で要求する range を決める
///
/// `largest` は SUBSCRIBE_OK が運んだ Largest Object (draft-ietf-moq-transport-22
/// §3.1.4 (Largest Object) / §9.20.17 (LARGEST_OBJECT Parameter)) である。購読より前に
/// publish された Object の存在はこれでしか分からないため、無い場合は FETCH しない。
/// 開始 Location は Largest Object の Group から `groups` 個前 (Largest Object の Group を
/// 含む) の Group の先頭、終端は Largest Object とする。両端を含む range であり
/// (§3.3.1 (Location Filters): "an inclusive range of Locations")、Group 数が足りない
/// 場合は Group 0 で飽和させる。
///
/// 根拠の仕様は draft であり、将来変更される可能性がある。
fn plan_fetch_range(largest: Option<Location>, groups: u64) -> Option<(Location, Location)> {
    let largest = largest?;
    // `groups` は Largest Object の Group を含めた数である。1 以下なら直近の Group だけを
    // 要求する (0 を渡されても underflow させない)
    let start = Location {
        group_id: largest.group_id.saturating_sub(groups.saturating_sub(1)),
        object_id: 0,
    };
    Some((start, largest))
}

/// メディア FETCH のメッセージパラメータを組み立てる
///
/// 要求 range は LOCATION_FILTER (draft-ietf-moq-transport-22 §9.20.9 (LOCATION FILTER
/// Parameter)) の Type 0x04 (Absolute Range) で指定する。Start は絶対 Location、End は
/// StartGroup からの差分 (EndGroupDelta) と EndObject で表す (同節 Table 6)。
/// `start.group_id <= end.group_id` は [`plan_fetch_range`] が保証するため差分は
/// underflow しない。
///
/// INCLUDE_PROPERTIES (§9.20.21) は載せない。同節が指定するのは OK メッセージに載る
/// Track Properties であり、応答 Object の Object Properties は常に運ばれるためである。
fn fetch_range_parameters(start: Location, end: Location) -> MessageParameters {
    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LocationFilter(LocationFilter::AbsoluteRangeWithEnd {
            start,
            end_group_delta: end.group_id.saturating_sub(start.group_id),
            end_object: end.object_id,
        }),
    });
    parameters
}

/// 購読した track の直近 Group を FETCH で要求し、対応表へ記録する
///
/// `largest` に Largest Object が無い場合は FETCH しない ([`plan_fetch_range`])。
/// 発行できたら「Request ID → トラック種別」の対応表へ記録する。FETCH_OK の受信時に
/// 記録するのではなく発行直後に記録するのは、応答 stream が FETCH_OK より先に届きうる
/// ためである。発行に失敗した場合 (Session が送信を拒否した場合を含む) は warn ログを
/// 残して live の購読を続ける (FETCH は欠落した Group を埋めるための補助であり、
/// live の受信を止める理由にはならない)。
///
/// peer が REQUEST_ERROR で拒否した場合は応答 stream が届かない。拒否は
/// `MoqtClient` がアプリへ配送しない (`SessionEvent::RequestErrorReceived` は notable
/// イベントではない) ためログに残せないが、live の購読は影響を受けない
/// (対応表に残った Request ID は、同じ session では再利用されないため誤配送しない)。
async fn fetch_recent_groups(
    client: &mut MoqtClient,
    media_fetch_targets: &mut HashMap<u64, TrackKind>,
    target: &SubscribeTarget,
    kind: TrackKind,
    largest: Option<Location>,
) {
    let Some((start, end)) = plan_fetch_range(largest, FETCH_RECENT_GROUPS) else {
        tracing::info!(
            "Skipping FETCH for the {kind:?} track: SUBSCRIBE_OK carried no Largest Object"
        );
        return;
    };
    let parameters = fetch_range_parameters(start, end);
    match client
        .fetch(
            target.namespace.clone(),
            target.track_name.clone(),
            parameters,
        )
        .await
    {
        Ok(request_id) => {
            tracing::info!(
                "Sent FETCH for the recent groups: kind={kind:?}, request_id={request_id}, \
                 start=({}, {}), end=({}, {})",
                start.group_id,
                start.object_id,
                end.group_id,
                end.object_id,
            );
            media_fetch_targets.insert(request_id, kind);
        }
        Err(e) => {
            tracing::warn!("Failed to send FETCH for the {kind:?} track: {e}");
        }
    }
}

/// 音声だけを購読しているか
///
/// 映像トラックを購読しない構成 (catalog に無い、または `--no-video`) で音声だけが有効な
/// とき true である。音声の再生はこのときだけ到着基準の再生の遅れに下限を足し、目標の
/// 時刻を守らない並べ方にする (プレイヤーの `AudioPlayoutAssembly::set_audio_only`)。
fn is_audio_only_subscription(subscribe_video: bool, subscribe_audio: bool) -> bool {
    subscribe_audio && !subscribe_video
}

/// 2 つの接続先が同じ session を指すか
///
/// request stream の GOAWAY は「New Session URI が空なら現在の session で再発行する」
/// (draft-ietf-moq-transport-22 §9.2 (GOAWAY)。draft 由来の仕様であり将来変更されうる)。
/// `migration_target` は空 URI のとき現在の URL をそのまま返すため、同じ URL かどうかで
/// 「同一 session での再発行」を判定できる。`ServerUrl` は全体の `PartialEq` を持たないため、
/// session を決める authority / path / fragment を比較する (c4m の認可トークンは同じ URL なら
/// 同じ値であり、比較しない)。
/// 同一 origin の判定ではなく URL 全体の一致を見るのは、path が違えば別の relay の
/// 別 session であり、request 1 本のために 2 session を併走させる必要があるためである。
fn same_session_url(a: &tokio_moq::ServerUrl, b: &tokio_moq::ServerUrl) -> bool {
    a.authority == b.authority && a.path == b.path && a.fragment == b.fragment
}

/// request stream の GOAWAY が対象とする track 種別を、現在の Request ID から決める
///
/// request stream 上の GOAWAY は当該 request だけの移行を求める (§9.2。draft 由来の仕様で
/// あり将来変更されうる)。どの track の request かは、現在購読している Request ID との
/// 一致で判定する。どれにも一致しない (既に終端した request など) 場合は `None` を返し、
/// 呼び出し側は何もしない。
fn goaway_request_kind(
    request_id: u64,
    catalog_request_id: u64,
    video_request_id: Option<u64>,
    audio_request_id: Option<u64>,
) -> Option<TrackKind> {
    if request_id == catalog_request_id {
        return Some(TrackKind::Catalog);
    }
    if video_request_id == Some(request_id) {
        return Some(TrackKind::Video);
    }
    if audio_request_id == Some(request_id) {
        return Some(TrackKind::Audio);
    }
    None
}

/// 移行を開始してよいかの判定結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationStart {
    /// 移行を開始してよい (連鎖防止の記録も済み)
    Start,
    /// 既に移行が進行中である
    AlreadyInProgress,
    /// 短期間に移行が集中しているため拒否した
    RateLimited,
}

/// 移行を開始してよいかを判定し、開始する場合は連鎖防止の記録を行う
///
/// 移行中は記録しない。連鎖防止の記録を「実際に開始した移行」に限ることで、移行中の
/// 2 回目の GOAWAY (draft-ietf-moq-transport-22 §9.2 (GOAWAY) は control stream の
/// GOAWAY を 1 回までとするため通常は届かないが、request stream の GOAWAY は届きうる) で
/// 記録を消費しない。draft 由来の仕様であり将来変更されうる。
fn begin_migration(
    migration_in_progress: bool,
    limiter: &mut MigrationLimiter,
    now: Instant,
) -> MigrationStart {
    if migration_in_progress {
        return MigrationStart::AlreadyInProgress;
    }
    if !limiter.check_and_record(now) {
        return MigrationStart::RateLimited;
    }
    MigrationStart::Start
}

/// 移行の再試行を続けてよいかを、移行期限と現在時刻から決める
///
/// 期限が切れていれば `false` を返し、呼び出し側は最後の失敗を返す (試行回数の上限は
/// 設けず、期限で打ち切る)。再試行の間には必ず待ち時間を入れるため、期限内でも
/// タイトループにはならない。
fn should_retry_migration(deadline: MigrationDeadline, now: Instant) -> bool {
    !deadline.expired_at(now)
}

/// 移行の再試行で待つ時間を、試行回数と残り時間から決める
///
/// 指数バックオフ (初回 [`MIGRATION_RETRY_INITIAL_DELAY`]、倍々) を
/// [`MIGRATION_RETRY_MAX_DELAY`] で頭打ちにし、残り時間を超えないよう cap する。
/// 残り時間が 0 なら 0 を返し、呼び出し側は再試行しない。
fn migration_retry_delay(attempt: u32, remaining: std::time::Duration) -> std::time::Duration {
    // 指数部を飽和させてから掛ける (巨大な attempt でもオーバーフローしない)
    let factor = 1u32 << attempt.min(16);
    MIGRATION_RETRY_INITIAL_DELAY
        .saturating_mul(factor)
        .min(MIGRATION_RETRY_MAX_DELAY)
        .min(remaining)
}

/// バックグラウンドの移行タスクへ渡す要求
struct MigrationRequest {
    /// 移行先の URL
    url: tokio_moq::ServerUrl,
    /// 接続経路 (現在の接続と同じものを使う)
    transport: Transport,
    /// TLS の CA 証明書パス (現在の接続と同じものを使う)
    cert: Option<String>,
    /// 移行先で張り直す購読の指定
    plan: ResubscribePlan,
    /// 再購読の Location Filter に使う、track ごとの最後の Location
    last_locations: LastReceivedLocations,
    /// 移行期限 (GOAWAY の Timeout から作る)
    deadline: MigrationDeadline,
}

/// バックグラウンドの移行タスクが main ループへ返す成功結果
struct MigrationOutcome {
    /// 移行先の session
    client: MoqtClient,
    /// 移行先の stream acceptor
    recv_acceptor: transport::StreamAcceptor,
    /// 移行先の data plane ハンドル
    data_plane: DataPlaneHandle,
    /// 移行先で張り直した購読
    resubscribed: Resubscribed,
    /// 移行先の URL
    url: tokio_moq::ServerUrl,
}

/// 移行の失敗理由
///
/// 失敗の種類 (接続失敗 / 期限切れ / 購読の失敗) はログの文言に含める。main ループは
/// 失敗の種類によらず旧 session の終了経路へ進むため、型で分ける必要がない。
struct MigrationFailure {
    /// ログに出す失敗の説明
    reason: String,
}

impl MigrationFailure {
    /// 説明から失敗を作る
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for MigrationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}

/// 移行タスクが main ループへ返す結果
///
/// 移行タスクは結果を必ず 1 件送る。main ループが先に receiver を drop した場合だけ送信が
/// 失敗するが、その結果は誰も読まないため無視してよい。
type MigrationResult = std::result::Result<MigrationOutcome, MigrationFailure>;

/// 移行先の session を確立し、購読を張り直す
///
/// 期限 ([`MigrationDeadline`]) は GOAWAY を受信した時刻から起算する。接続は
/// [`tokio_moq::connect_with_retry`] が期限内で指数バックオフ再試行する。
///
/// 接続のあとの購読の張り直しは、一時的な失敗 (SUBSCRIBE のタイムアウトなど) で移行全体を
/// 諦めないよう、期限が残っている限り connect からやり直す。待ち時間は
/// [`migration_retry_delay`] が指数バックオフ (上限 1 秒) と残り時間から決める。期限が
/// 切れたら最後の失敗を返し、main ループは旧 session の終了経路へ進む。
///
/// 途中まで確立した session (接続はできたが購読の張り直しに失敗した場合) は、試行ごとに
/// NO_ERROR で閉じる。放置すると peer が timeout まで資源を保持するためである。
///
/// draft-ietf-moq-transport-22 §6.6.1 (Graceful Session Migration) / §9.2 (GOAWAY) は
/// draft 由来の仕様であり、将来変更される可能性がある。
async fn migrate_to_target(
    request: MigrationRequest,
    task_monitor: &tokio_metrics::TaskMonitor,
) -> std::result::Result<MigrationOutcome, MigrationFailure> {
    let params = tokio_moq::ConnectParams {
        url: &request.url,
        transport: request.transport,
        cert: request.cert.as_deref(),
        impl_name: "moq-sub",
        // subscriber は WebTransport 経路で datagram を継続受信する
        receive_datagrams: true,
        // subscriber は datagram を送らない
        datagram_max_size: None,
    };
    let mut attempt: u32 = 0;
    loop {
        // 接続の失敗は connect_with_retry が期限内で再試行済みであるため、ここでは再試行
        // しない (同じ設定で失敗し続けるだけになる)
        let (mut new_client, new_acceptor) = tokio_moq::connect_with_retry(
            &params,
            request.deadline,
            RetryPolicy::default(),
            task_monitor,
        )
        .await
        .map_err(|e| {
            MigrationFailure::new(format!("failed to connect to the migration target: {e}"))
        })?;
        new_client.set_control_message_timeout_ms(Some(30000));
        new_client.set_data_stream_timeout_ms(Some(30000));
        let new_data_plane = new_client.data_plane();

        // 接続に要した時間は期限から差し引かれている。残り時間が無ければ張り直しはできない
        let remaining = request.deadline.remaining();
        if remaining.is_zero() {
            close_abandoned_migration_session(&mut new_client).await;
            return Err(MigrationFailure::new(
                "the migration deadline expired before resubscribing",
            ));
        }
        // 1 回の試行に上限を設けて再試行の余地を残す (残り時間が短ければ残り時間)
        let attempt_timeout = remaining.min(MIGRATION_RESUBSCRIBE_TIMEOUT);
        let resubscribed = match tokio::time::timeout(
            attempt_timeout,
            resubscribe_tracks(&mut new_client, &request.plan, &request.last_locations),
        )
        .await
        {
            Ok(Ok(resubscribed)) => resubscribed,
            Ok(Err(e)) => {
                close_abandoned_migration_session(&mut new_client).await;
                let failure =
                    MigrationFailure::new(format!("failed to resubscribe on the new session: {e}"));
                if !wait_before_migration_retry(request.deadline, attempt, &failure).await {
                    return Err(failure);
                }
                attempt = attempt.saturating_add(1);
                continue;
            }
            Err(_) => {
                close_abandoned_migration_session(&mut new_client).await;
                let failure = MigrationFailure::new(
                    "the resubscribe did not complete within the migration deadline",
                );
                if !wait_before_migration_retry(request.deadline, attempt, &failure).await {
                    return Err(failure);
                }
                attempt = attempt.saturating_add(1);
                continue;
            }
        };

        return Ok(MigrationOutcome {
            client: new_client,
            recv_acceptor: new_acceptor,
            data_plane: new_data_plane,
            resubscribed,
            url: request.url,
        });
    }
}

/// 移行の途中で確立した session を閉じる
///
/// 購読の張り直しに失敗した試行の session を放置すると、peer が GOAWAY の Timeout まで
/// 資源を保持する。NO_ERROR で閉じる (draft-ietf-moq-transport-22 §6.6.1 (Graceful Session
/// Migration)。draft 由来の仕様であり将来変更されうる)。
async fn close_abandoned_migration_session(client: &mut MoqtClient) {
    if let Err(e) = client.close(0, "").await {
        tracing::debug!(
            "Failed to close the new session after an unsuccessful migration attempt: {e}"
        );
    }
}

/// 購読の張り直しに失敗したあと、次の試行まで待つ
///
/// 期限が残っていなければ `false` を返し、呼び出し側は最後の失敗を返す。残っていれば
/// [`migration_retry_delay`] の時間だけ待って `true` を返す。タイトループを避けるため、
/// 待ち時間が 0 になる場合 (残り時間が無い) も `false` を返す。
async fn wait_before_migration_retry(
    deadline: MigrationDeadline,
    attempt: u32,
    failure: &MigrationFailure,
) -> bool {
    let remaining = deadline.remaining();
    if !should_retry_migration(deadline, Instant::now()) {
        return false;
    }
    let delay = migration_retry_delay(attempt, remaining);
    if delay.is_zero() {
        return false;
    }
    tracing::warn!(
        "Retrying the migration after a failed resubscribe (attempt={}, delay_ms={}): {failure}",
        attempt.saturating_add(1),
        delay.as_millis(),
    );
    tokio::time::sleep(delay).await;
    true
}

/// バックグラウンドの移行タスクを起動する
///
/// main ループは旧 session の受信・catalog の適用・stream の accept を続け、結果は
/// oneshot で受け取る (draft-ietf-moq-transport-22 §6.6.1 は移行をアプリケーションへ
/// 透過的に行うことを求める。draft 由来の仕様であり将来変更されうる)。送信側を
/// 呼び出し元へ返さないのは、移行タスクが結果を必ず 1 件送るためである。
fn spawn_migration(
    task_monitor: &tokio_metrics::TaskMonitor,
    request: MigrationRequest,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::oneshot::Receiver<MigrationResult>,
) {
    let connect_monitor = task_monitor.clone();
    let instrument_monitor = task_monitor.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(instrument_monitor.instrument(async move {
        let result = migrate_to_target(request, &connect_monitor).await;
        // main ループが先に終了した場合 (移行結果はもう誰も読まない) だけ失敗する
        if tx.send(result).is_err() {
            tracing::debug!("Dropping the migration result");
        }
    }));
    (handle, rx)
}

/// 移行後に旧 session を終了するタスクを起動する
///
/// 旧 session の購読に STOP_SENDING を送り、NO_ERROR で閉じる
/// (draft-ietf-moq-transport-22 §6.6.1 (Graceful Session Migration) は、Established の
/// 購読が無くなった時点で NO_ERROR で閉じることを RECOMMENDED とする。draft 由来の仕様で
/// あり将来変更されうる)。GOAWAY の Timeout は待たない。待つと移行先の処理を始めるのが
/// 遅れ、main ループを塞ぐことにもなるためである。送信と close を別タスクにすることで
/// main ループは移行先の受信を続けられる。
fn spawn_old_session_close(
    task_monitor: &tokio_metrics::TaskMonitor,
    mut old_client: MoqtClient,
    request_ids: Vec<u64>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(task_monitor.clone().instrument(async move {
        for request_id in request_ids {
            if let Err(e) = old_client.stop_sending(request_id).await {
                // 旧 session はこの直後に閉じるため、送れなくても購読は session の終了で終わる
                tracing::warn!(
                    "Failed to send STOP_SENDING for request {request_id} during migration: {e}"
                );
            }
        }
        match old_client.close(0, "").await {
            Ok(()) => tracing::info!("Closed the old session after migration"),
            Err(e) => {
                tracing::debug!("Failed to close the old session after migration: {e}");
            }
        }
    }))
}

/// request stream の GOAWAY で再発行した購読
struct ReissuedRequest {
    /// 再発行した購読の結果
    subscription: SubscribeResult,
    /// 再発行に伴って発行したメディア FETCH の対応表 (Request ID → トラック種別)
    media_fetch_targets: HashMap<u64, TrackKind>,
}

/// request stream の GOAWAY で、現在の session に同じ購読を再発行する
///
/// draft-ietf-moq-transport-22 §9.2 (GOAWAY): request stream 上の GOAWAY は当該 request
/// だけの移行であり、session は維持する。New Session URI が空 (現在の URI の再利用) の
/// 場合だけ、同じ session で購読を再発行する。別 URI への request 単位移行は 2 session の
/// 併走が必要になるため行わない (呼び出し側が判定する)。
///
/// 旧 request stream は STOP_SENDING で閉じる (§6.4.2.3 (Request Cancellation and
/// Rejection))。購読の張り直しは移行時と同じ手順 (映像は SUBSCRIBER_PRIORITY の
/// REQUEST_UPDATE と直近 Group の FETCH、音声は直近 Group の FETCH) で行う。
/// draft 由来の仕様であり、将来変更される可能性がある。
async fn reissue_request(
    client: &mut MoqtClient,
    kind: TrackKind,
    request_id: u64,
    plan: &ResubscribePlan,
    last_locations: &LastReceivedLocations,
) -> Result<ReissuedRequest> {
    // 旧 request stream を閉じる。購読は Terminated へ遷移する
    // (draft-ietf-moq-transport-22 §6.4.2.3 (Request Cancellation and Rejection))。
    // peer が既に終端させていた場合は状態機械に拒否されるが、その場合も再発行は続ける
    if let Err(e) = client.stop_sending(request_id).await {
        tracing::warn!(
            "Failed to send STOP_SENDING for request {request_id} before re-issuing it: {e}"
        );
    }
    let mut media_fetch_targets: HashMap<u64, TrackKind> = HashMap::new();
    let subscription = match kind {
        TrackKind::Catalog => {
            let sub = client
                .subscribe_track_with_parameters(
                    plan.catalog_namespace.clone(),
                    MSF_CATALOG_TRACK_NAME.to_vec(),
                    catalog_subscribe_parameters(),
                )
                .await?;
            // 再発行した catalog 購読の fill fetch stream をカタログとして適用するため、
            // Request ID を対応表に登録する
            media_fetch_targets.insert(sub.request_id, TrackKind::Catalog);
            sub
        }
        TrackKind::Video => {
            let Some(target) = plan.video.as_ref() else {
                return Err(Error::Other(
                    "the video subscription is not configured".to_string(),
                ));
            };
            let sub = client
                .subscribe_track_with_parameters(
                    target.namespace.clone(),
                    target.track_name.clone(),
                    video_subscribe_parameters(last_locations.get(TrackKind::Video)),
                )
                .await?;
            // 起動時・移行時と同じく SUBSCRIBER_PRIORITY を送る
            let mut params = shiguredo_moqt::message_parameter::MessageParameters::new();
            params.push(shiguredo_moqt::message_parameter::MessageParameter {
                param_type: shiguredo_moqt::message_parameter::PARAM_SUBSCRIBER_PRIORITY,
                value: shiguredo_moqt::message_parameter::MessageParameterValue::Uint8(192),
            });
            if let Err(e) = client.send_request_update(sub.request_id, params).await {
                tracing::warn!("Failed to send REQUEST_UPDATE for video subscription: {e}");
            }
            fetch_recent_groups(
                client,
                &mut media_fetch_targets,
                target,
                TrackKind::Video,
                sub.largest_object,
            )
            .await;
            sub
        }
        TrackKind::Audio => {
            let Some(target) = plan.audio.as_ref() else {
                return Err(Error::Other(
                    "the audio subscription is not configured".to_string(),
                ));
            };
            let sub = client
                .subscribe_track_with_parameters(
                    target.namespace.clone(),
                    target.track_name.clone(),
                    audio_subscribe_parameters(last_locations.get(TrackKind::Audio)),
                )
                .await?;
            fetch_recent_groups(
                client,
                &mut media_fetch_targets,
                target,
                TrackKind::Audio,
                sub.largest_object,
            )
            .await;
            sub
        }
    };
    Ok(ReissuedRequest {
        subscription,
        media_fetch_targets,
    })
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
/// `target_latency_ms` はカタログの `targetLatency` を渡すセルであり、値があるときだけ格納する
/// (対象は実際に購読する track の `renderGroup` に属する track。§5.2.8)。購読する group が
/// 複数あるときは最大値をすべての購読 track に掛ける。
/// `video_subscribed` は映像トラックを購読しているかをプレイヤーへ渡すセルであり、
/// カタログを読み終えて購読の有無が決まった時点で格納する。プレイヤーは音声だけの購読で
/// 到着基準の再生の遅れに下限を足し、目標の時刻を守らない並べ方にする。カタログを読む前の
/// 初期値は呼び出し側が決める (映像があるものとして扱う)。
#[expect(clippy::too_many_arguments)]
pub async fn run(
    mut config: Config,
    frame_tx: std::sync::mpsc::Sender<DecodedVideoFrame>,
    audio_tx: std::sync::mpsc::Sender<DecodedAudioFrame>,
    task_monitor: tokio_metrics::TaskMonitor,
    mut shutdown_monitor: tokio_utils::ShutdownMonitor,
    display_backlog: std::sync::Arc<std::sync::atomic::AtomicI64>,
    target_latency_ms: std::sync::Arc<std::sync::atomic::AtomicI64>,
    video_subscribed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    mut player_stop: tokio::sync::oneshot::Receiver<()>,
) -> Result<()> {
    // 再生を行わない場合はデコードをスキップし、録画だけを行う
    let playback = !config.no_play;
    // 録画は専用スレッドが所有する。run の早期 return でも Recorder の Drop が finalize する。
    // 録画対象のトラック情報はカタログ取得後に確定するため、起動はその時点で行う。
    let recording_requested = config.mp4.is_some();

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

    // 1. 接続確立と SETUP ハンドシェイク
    //
    // 名前解決、解決したアドレスのフォールバック、transport ごとの分岐は
    // tokio_moq の共通の接続経路が担う (GOAWAY 後の移行でも同じ経路を使う)。
    let (mut client, mut recv_acceptor) = tokio_moq::connect(
        &tokio_moq::ConnectParams {
            url: &config.url,
            transport: config.transport,
            cert: config.cert.as_deref(),
            impl_name: "moq-sub",
            // subscriber は WebTransport 経路で datagram を継続受信する
            receive_datagrams: true,
            // subscriber は datagram を送らない
            datagram_max_size: None,
        },
        &task_monitor,
    )
    .await?;

    // CLI で §8.8 表現としてパース済みの namespace をそのまま使う
    // `config` から move して受け取る。購読呼び出しを catalog track の namespace に変える 1 行の
    // 変更は、move 済みの値の再利用としてコンパイルエラーになる
    let namespace = config.namespace;
    let mut data_plane = client.data_plane();

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
    // C4M 認可トークンは `MoqtClient` が URL の c4m トークンをすべて付けるため、ここでは
    // 指定しない (draft-ietf-moq-msf-01 §11.4.3 (Presenting Authorization))。
    // 移行 (GOAWAY) で catalog を張り直すときに使う namespace
    let catalog_namespace_for_migration = namespace.clone();
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
        namespace,
    )
    .await?;
    let CatalogResolution {
        state: mut catalog_state,
        video: video_info,
        audio: audio_info,
        video_latency,
        audio_latency,
    } = catalog_resolution;
    if let Some(ref v) = video_info {
        tracing::info!(
            "Catalog video track: namespace={}, name={}, codec={}, {}x{} @ {} fps",
            serialize_namespace(&v.namespace),
            v.track_name,
            v.codec,
            v.width,
            v.height,
            v.fps,
        );
    }
    if let Some(ref a) = audio_info {
        tracing::info!(
            "Catalog audio track: namespace={}, name={}, codec={}, samplerate={}, channel_config={}",
            serialize_namespace(&a.namespace),
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
    // 映像トラックを購読しているかをプレイヤーへ渡す。音声だけの購読では、プレイヤーが
    // 到着基準の再生の遅れに下限を足し、目標の時刻を守らない並べ方にする
    let audio_only_subscription = is_audio_only_subscription(subscribe_video, subscribe_audio);
    video_subscribed.store(subscribe_video, std::sync::atomic::Ordering::Relaxed);
    if audio_only_subscription {
        tracing::info!(
            "Subscribing to the audio track only; the audio playout uses the audio-only arrival delay floor"
        );
    }

    // カタログの targetLatency をプレイヤーへ渡す。プレイヤーは時間軸の表示の遅れの下限として
    // 反映する。対象は実際に購読する track の render group に属する track に限る
    // (別の render group の宣言値を購読中の track に適用しない。
    // draft-ietf-moq-msf-01 §5.2.8 は同じ render group の track が同一の targetLatency を
    // 持つことを MUST とする)
    let subscribed_latencies = subscribed_track_latencies(
        video_latency,
        audio_latency,
        subscribe_video,
        subscribe_audio,
    );
    let catalog_target_latency_ms = match catalog_state.catalog() {
        Some(catalog) => catalog_target_latency_ms(&catalog.tracks, &subscribed_latencies),
        None => None,
    };
    if let Some(catalog_target_latency_ms) = catalog_target_latency_ms {
        target_latency_ms.store(
            catalog_target_latency_ms,
            std::sync::atomic::Ordering::Relaxed,
        );
        tracing::info!("Using catalog targetLatency: {catalog_target_latency_ms}ms");
    }

    let mut track_map: HashMap<u64, TrackKind> = HashMap::new();
    // カタログの購読は TrackKind::Catalog として登録し、届いた Object は main ループが適用する
    track_map.insert(catalog_sub.track_alias, TrackKind::Catalog);
    // 自側が発行したメディア FETCH の対応表 (Request ID → トラック種別)。応答 Object は
    // 通常の再生経路へ流すが、どの track の応答かは FETCH 応答 stream の FETCH_HEADER が
    // 運ぶ Request ID でしか分からないため、ここで記録する。
    // catalog 購読の Request ID も登録する。FILL_PARAMETERS 付き SUBSCRIBE には publisher が
    // fill fetch stream を返し (draft-ietf-moq-transport-22 §3.4 (Fill Semantics))、その
    // FETCH_HEADER は起因 SUBSCRIBE の Request ID を運ぶ。移行後に catalog を張り直したとき
    // の fill をカタログとして適用するため、種別をここで対応づける。
    // 移行 (GOAWAY) では、張り直した購読に対して発行した FETCH の対応表へ差し替える
    // (Request ID は session ごとに払い出される)
    let mut media_fetch_targets: HashMap<u64, TrackKind> = HashMap::new();
    media_fetch_targets.insert(catalog_sub.request_id, TrackKind::Catalog);
    let mut video_codec: Option<String> = None;
    let mut video_request_id: Option<u64> = None;
    let mut audio_request_id: Option<u64> = None;
    // peer が SUBSCRIBE_OK で決める track alias は移行で変わりうるため、現在の値を保持する
    let mut video_alias: Option<u64> = None;
    let mut audio_alias: Option<u64> = None;
    // 音声の購読境界 (SUBSCRIBE_OK の Largest Object)。境界以下は購読より前に publish された
    // 音声であり、Audio Config の検証だけを行い、送出も録画も復号もしない
    let mut audio_largest_object: Option<Location> = None;
    // 映像 Object を復号してよいかの判定状態は購読で 1 つ持つ。Group ごとに別の Subgroup
    // ストリームで届くため (draft-ietf-moq-transport-22 §2.2 (Subgroups))、stream ごとに
    // 判定を持つと「前の Group の Object が次の Group のキーフレームより後に届く」場合を
    // 判定できない。映像のトラックを切り替えて decoder を作り直す経路を足すときは、
    // この状態を reset してから次のキーフレームで復号を始め直すこと。
    let video_decode_order = std::sync::Arc::new(tokio::sync::Mutex::new(VideoDecodeOrder::new()));

    if subscribe_video {
        let v = video_info.as_ref().expect("video_info is Some");
        let video_sub = client
            .subscribe_track_with_parameters(
                v.namespace.clone(),
                v.track_name.as_bytes().to_vec(),
                // 起動時は前の session が無いため Location Filter を載せない
                video_subscribe_parameters(None),
            )
            .await?;
        tracing::info!(
            "Subscribed to video track: alias={}, request_id={}",
            video_sub.track_alias,
            video_sub.request_id,
        );
        track_map.insert(video_sub.track_alias, TrackKind::Video);
        video_alias = Some(video_sub.track_alias);
        video_request_id = Some(video_sub.request_id);

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

        // 購読より前に publish された Group は購読では届かない。SUBSCRIBE_OK の
        // Largest Object を基準に直近の Group を FETCH で要求し、映像はその範囲の
        // キーフレームから復号を始められるようにする (draft-ietf-moq-transport-22 §3.5.1)。
        // 購読の優先度を変えたあとに発行する (§9.20.7 の SUBSCRIBER PRIORITY を
        // FETCH 応答にも効かせるため)
        fetch_recent_groups(
            &mut client,
            &mut media_fetch_targets,
            &SubscribeTarget {
                namespace: v.namespace.clone(),
                track_name: v.track_name.as_bytes().to_vec(),
            },
            TrackKind::Video,
            video_sub.largest_object,
        )
        .await;

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
            .subscribe_track(a.namespace.clone(), a.track_name.as_bytes().to_vec())
            .await?;
        tracing::info!(
            "Subscribed to audio track: alias={}, request_id={}",
            audio_sub.track_alias,
            audio_sub.request_id,
        );
        track_map.insert(audio_sub.track_alias, TrackKind::Audio);
        audio_alias = Some(audio_sub.track_alias);
        audio_request_id = Some(audio_sub.request_id);
        audio_largest_object = audio_sub.largest_object;

        // 映像と同じく、購読より前に publish された Group を FETCH で埋める
        fetch_recent_groups(
            &mut client,
            &mut media_fetch_targets,
            &SubscribeTarget {
                namespace: a.namespace.clone(),
                track_name: a.track_name.as_bytes().to_vec(),
            },
            TrackKind::Audio,
            audio_sub.largest_object,
        )
        .await;

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
    // Audio Config の解釈 (codec 判定) と音声デコーダの生成に使う catalog の audio codec
    let audio_codec = audio_info.as_ref().map(|a| a.codec.clone());

    // 移行 (GOAWAY) で購読を張り直すための指定と、現在の session の Request ID / alias
    //
    // catalog は移行時に読み直さない。購読済みの指定をそのまま使えば、移行先の session で
    // 届く Object から同じ `CatalogState` が再び更新される
    let resubscribe_plan = ResubscribePlan {
        catalog_namespace: catalog_namespace_for_migration,
        // `subscribe_video` / `subscribe_audio` は catalog から得た track がある場合だけ真に
        // なるため、ここで `Option` の条件と購読の有無が一致する
        video: video_info
            .as_ref()
            .filter(|_| subscribe_video)
            .map(|info| SubscribeTarget {
                namespace: info.namespace.clone(),
                track_name: info.track_name.as_bytes().to_vec(),
            }),
        audio: audio_info
            .as_ref()
            .filter(|_| subscribe_audio)
            .map(|info| SubscribeTarget {
                namespace: info.namespace.clone(),
                track_name: info.track_name.as_bytes().to_vec(),
            }),
    };
    let mut catalog_request_id = catalog_sub.request_id;
    let mut catalog_alias = catalog_sub.track_alias;

    // 音声は 1 object = 1 subgroup stream で届く (LOC draft-ietf-moq-loc-04 §4.1)。
    // stream ごとにデコーダを作り直すと 20 ms ごとにデコーダ状態が失われ、
    // フレーム境界で波形が不連続になってノイズになる。
    // subscription で 1 つのデコーダを共有し、Audio Config の処理済みフラグも共有する。
    // 録画中はデコーダが使えなくても録画を続けるため、警告に留める。
    //
    // デコーダは録画のセットアップより先に作る。AAC は AudioToolbox の出力がステレオ固定で
    // catalog の channelConfig と食い違うことがあり、再生と録画に渡すチャンネル数を
    // デコーダの出力に合わせるためである。
    let audio_decoder = match (playback, audio_params, audio_codec.as_deref()) {
        (true, Some((sample_rate, channels)), Some(codec)) => {
            match AudioDecoder::build(codec, sample_rate, channels) {
                Ok(decoder) => {
                    tracing::info!(
                        "Audio decoder available (codec={}, sample_rate={}, channels={})",
                        codec,
                        sample_rate,
                        decoder.channels()
                    );
                    // AAC は入力がモノラルでも AudioToolbox がステレオへアップミックスする。
                    // catalog の channelConfig と食い違っても再生と録画は継続する
                    // (Opus の OpusHead 検証と同じ方針)
                    if decoder.channels() != channels {
                        tracing::warn!(
                            "Audio decoder output channels {} do not match catalog channelConfig {} (playback continues)",
                            decoder.channels(),
                            channels,
                        );
                    }
                    Some(decoder)
                }
                Err(e) if recording_requested => {
                    tracing::warn!(
                        "Audio decoder is unavailable; recording without audio playback: {e}"
                    );
                    None
                }
                Err(e) => return Err(e),
            }
        }
        // `audio_params` が Some なら catalog の audio track は codec を持つ
        // (extract_audio_info が codec を必須とする) ため、この経路は型上の防御である
        (true, Some(_), None) => {
            tracing::warn!("Audio track has no codec; audio playback is disabled");
            None
        }
        _ => None,
    };

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
    // MP4 録画の音声トラックは Opus のみに対応する。mp4.rs の `build_audio_entry` は
    // Opus のサンプルエントリーを構築し、録画するデータは圧縮 payload そのものであるため、
    // AAC を録画するとコンテナの表記と実際のペイロードが食い違う。AAC のトラックでは
    // 音声を録画せず警告する (映像のみの MP4 になる)
    let audio_setup = match (audio_params, audio_codec.as_deref()) {
        (Some((sample_rate, channels)), Some(codec)) if decoder::is_opus_codec(codec) => {
            Some(AudioSetup {
                sample_rate,
                channels,
            })
        }
        (Some(_), Some(codec)) if config.mp4.is_some() => {
            tracing::warn!(
                "MP4 recording supports Opus audio only; audio is not recorded (catalog codec: {codec})"
            );
            None
        }
        _ => None,
    };
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

    // stream task は `Arc<Mutex<...>>` でデコーダを共有する
    let audio_decoder =
        audio_decoder.map(|decoder| std::sync::Arc::new(tokio::sync::Mutex::new(decoder)));
    let audio_config_handled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // 受信側の追いつきの状態。ストリーム経路と datagram 経路で共有する
    let audio_catch_up = Arc::new(std::sync::Mutex::new(AudioReceiveCatchUp::new()));
    // 音声の購読境界と送出順の状態。datagram 経路と subgroup stream 経路、FETCH 応答経路で
    // 共有する
    let audio_delivery_order = Arc::new(std::sync::Mutex::new(AudioDeliveryOrder::new(
        audio_largest_object,
    )));

    // 移行 (GOAWAY) では peer が決める track alias が変わりうるため、書き換え可能にする。
    // 読み出しは短い区間に限り、await をまたいでロックを保持しない
    let track_map: Arc<std::sync::RwLock<HashMap<u64, TrackKind>>> =
        Arc::new(std::sync::RwLock::new(track_map));

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

    // 6. datagram 受信タスク
    //
    // OBJECT_DATAGRAM の type 分岐と wire の書式検証は Session が行う (`recv_datagram`)。
    // Session は payload をアプリへ返さないため、メディアとして使う Object はこのタスクで
    // decode し、subgroup stream と同じ配送経路へ流す。終了依頼のチャネルを必要とするため、
    // チャネルを用意した後で起動する。
    // 移行 (GOAWAY) で session を差し替えたら、新しい handle / data_plane と alias 表を
    // このチャネルでまとめて渡す。alias は peer が SUBSCRIBE_OK で決めるため session ごとに
    // 変わりうる。handle と表を別々に更新すると、旧 session の datagram を新しい表で解釈する
    // 窓ができるため、1 つの値にして同時に切り替える。
    // タスクを起動し直さないのは、デコーダと配送経路の状態をそのまま引き継ぐためである
    let datagram_track_map = Arc::new(
        track_map
            .read()
            .expect("track_map lock must not be poisoned")
            .clone(),
    );
    // track ごとに最後に配送した Object の位置。移行後の再購読の Location Filter に使う。
    // stream task と datagram task、main ループ (catalog) が共有する
    let last_received_locations: Arc<std::sync::Mutex<LastReceivedLocations>> =
        Arc::new(std::sync::Mutex::new(LastReceivedLocations::default()));
    let (datagram_session_tx, mut datagram_session_rx) = tokio::sync::watch::channel((
        client.handle(),
        data_plane.clone(),
        Arc::clone(&datagram_track_map),
    ));
    let audio_decoder_for_datagram = audio_decoder.clone();
    let audio_config_handled_for_datagram = Arc::clone(&audio_config_handled);
    let audio_codec_for_datagram = audio_codec.clone();
    let recorder_for_datagram = recorder_sender.clone();
    let termination_tx_for_datagram = termination_tx.clone();
    let audio_tx_for_datagram = audio_tx.clone();
    let audio_catch_up_for_datagram = Arc::clone(&audio_catch_up);
    let audio_delivery_order_for_datagram = Arc::clone(&audio_delivery_order);
    let last_received_locations_for_datagram = Arc::clone(&last_received_locations);
    let datagram_task = tokio::spawn(task_monitor.clone().instrument(async move {
        // Audio Config の処理とデコードは subgroup stream と共有する。datagram の音声は
        // 購読した audio track でしか届かないため、catalog の値はここで確定できる
        // (audio track を購読していなければ audio_params は None だが、その場合
        //  track_map に Audio が入らないため配送経路には来ない)。
        let delivery = AudioDelivery {
            audio_tx: &audio_tx_for_datagram,
            audio_config_handled: &audio_config_handled_for_datagram,
            is_opus_codec: audio_codec_for_datagram
                .as_deref()
                .is_some_and(decoder::is_opus_codec),
            catalog_sample_rate: audio_params.map_or(0, |(sample_rate, _)| sample_rate),
            catalog_channels: audio_params.map_or(0, |(_, channels)| channels),
            recorder: recorder_for_datagram.as_ref(),
            termination_tx: &termination_tx_for_datagram,
            catch_up: audio_catch_up_for_datagram.as_ref(),
            delivery_order: audio_delivery_order_for_datagram.as_ref(),
        };
        // メディアとして扱えないトラックの datagram を 1 回だけ記録するための alias
        let mut unsupported_aliases: HashSet<u64> = HashSet::new();
        // 最初の datagram を受け取ったことを 1 回だけ記録する。届いていないのか、届いて
        // いるのに配送経路へ流れていないのかを切り分けるために出す
        let mut datagram_received = false;
        let (mut handle_for_datagram, mut data_plane_for_datagram, mut track_map_for_datagram) =
            datagram_session_rx.borrow_and_update().clone();
        // `recv_datagrams` は transport から datagram を取り出した後に await しないため
        // (`StreamHandle::recv_datagrams`)、select が `changed()` を選んで future を捨てても
        // 取り出し済みの datagram は失われない。新 session の datagram は新 handle 側に残る
        'receive: loop {
            let received = tokio::select! {
                received = handle_for_datagram.recv_datagrams() => received,
                changed = datagram_session_rx.changed() => {
                    if changed.is_err() {
                        // 送信側 (`run`) がこのタスクを abort する前に drop した場合に限り到達する。
                        // 受信タスクは `run` の終了時に abort されるため、通常はここへ来ない
                        break 'receive;
                    }
                    let (handle, data_plane, map) = datagram_session_rx.borrow_and_update().clone();
                    handle_for_datagram = handle;
                    data_plane_for_datagram = data_plane;
                    track_map_for_datagram = map;
                    tracing::info!("Datagram receive task switched to the migrated session");
                    continue 'receive;
                }
            };
            let datagrams = match received {
                Ok(datagrams) => datagrams,
                Err(e) => {
                    // セッション終了 (§6) と接続クローズは期待される終了であるため、
                    // 異常 (warn) ではなく info に揃える
                    if matches!(e, TransportError::ConnectionClosed) {
                        tracing::info!("Datagram receive error: {e}");
                    } else {
                        tracing::warn!("Datagram receive error: {e}");
                    }
                    // 旧 session のエラーでタスクを終わらせない。GOAWAY の移行中に旧接続が
                    // 閉じることは §9.2 が許容しており (timeout 前の close)、ここで終了すると
                    // 移行後の datagram (音声) を受信する者が居なくなる。
                    // 移行の通知 (watch の更新) を待って新しい session へ切り替える。
                    // 送信側 (`run`) が drop された場合だけ終わる (run は終了時に abort する)
                    if datagram_session_rx.changed().await.is_err() {
                        break 'receive;
                    }
                    let (handle, data_plane, map) = datagram_session_rx.borrow_and_update().clone();
                    handle_for_datagram = handle;
                    data_plane_for_datagram = data_plane;
                    track_map_for_datagram = map;
                    tracing::info!(
                        "Datagram receive task switched to the migrated session after a receive error"
                    );
                    continue 'receive;
                }
            };
            if !datagram_received && !datagrams.is_empty() {
                datagram_received = true;
                tracing::info!(
                    "Received the first datagram batch from the relay: count={}",
                    datagrams.len()
                );
            }
            // 空のバッチのときだけ受信ループを休める。`recv_datagrams` は QUIC と QMux では
            // 1 件以上届くまで待つため、バッチがあるときの sleep はそのまま受信から再生までの
            // 遅延になる。WebTransport の `take_buffered_datagrams` はバッファが空でも即座に
            // 返りうるため、空のときだけ sleep して空回りを防ぐ
            let batch_len = datagrams.len();
            for raw in datagrams {
                // type 分岐は Session が行う。未知 type は
                // draft-ietf-moq-transport-22 §11 (Data Streams and Datagrams) に従いセッションクローズになる。
                // アプリ側で decode を代行せず raw を渡すのは、wire の検証順序と終了コードを
                // Session に一本化するためである (payload は改めて decode する)
                let acceptance = match data_plane_for_datagram.recv_datagram(&raw) {
                    Ok(acceptance) => acceptance,
                    Err(e) => {
                        // Session がセッションを閉じる経路である。close は main ループが送る
                        tracing::warn!("Failed to process datagram: {e}");
                        continue;
                    }
                };
                // Padding Datagram と、フィルタを通過しなかった Object・キャンセル済み
                // subscription の Object はメディアとして扱わない
                // (draft-ietf-moq-transport-22 §3.1 (Subscriptions) / §11.5.2 (Padding Datagrams))
                let DatagramAcceptance::Object(TrackDataAcceptance::Accepted) = acceptance else {
                    continue;
                };
                // 種別の判定には、この handle と同時に受け取った表を使う
                let action = plan_datagram(&raw, &track_map_for_datagram);
                match action {
                    DatagramAction::DeliverAudio { datagram, payload } => {
                        // デコーダは subscription で 1 つを共有する。stream task と同じく
                        // デコードが終わるまでロックを保持する
                        let mut decoder = match audio_decoder_for_datagram.as_ref() {
                            Some(decoder) => Some(decoder.lock().await),
                            None => None,
                        };
                        // 移行後の再購読で使う「最後に配送した位置」を記録する
                        let location = datagram_object_location(&datagram);
                        record_last_location(
                            &last_received_locations_for_datagram,
                            TrackKind::Audio,
                            location,
                        );
                        if !delivery
                            .deliver(
                                Some(location),
                                datagram.properties_data.as_deref(),
                                payload,
                                decoder.as_deref_mut(),
                            )
                            .await
                        {
                            // LOC Properties の書式違反でセッション終了を依頼したか、
                            // 再生側が終了した
                            break 'receive;
                        }
                    }
                    // 映像とカタログの datagram は本 example では扱わない。LOC の映像は
                    // 1 group = 1 unidirectional stream で配送され (draft-ietf-moq-loc-04 §4.2)、
                    // moq-pub も datagram では送らない
                    DatagramAction::Unsupported { track_alias } => {
                        if unsupported_aliases.insert(track_alias) {
                            tracing::warn!(
                                "Ignoring media delivered as a datagram on an unsupported track: alias={track_alias}"
                            );
                        }
                    }
                    DatagramAction::Ignore => {}
                }
            }
            if should_sleep_after_datagram_batch(batch_len) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
    }));

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
    // 音声配送の累計を最後にログへ出した時点の値。100 ms の tick で増えたときだけ出す
    let mut logged_audio_delivery_counters = AudioDeliveryCounters::default();

    // バックグラウンド移行の状態
    //
    // 移行の結果は oneshot で main ループへ返す。移行のたびに作るため、受信器は `Option` で
    // 保持し、`migration_in_progress` が真の間だけ受信分岐を有効にする。
    let mut migration_rx: Option<tokio::sync::oneshot::Receiver<MigrationResult>> = None;
    // 移行の確立と購読の張り直しが進行中か。真の間は 2 回目の GOAWAY / drain で移行しない
    let mut migration_in_progress = false;
    // 進行中の移行タスク。run の終了時に打ち切る
    let mut migration_task: Option<tokio::task::JoinHandle<()>> = None;
    // 短期間に連続する移行を制限する (移行先がまた GOAWAY を返す構成への防御)
    let mut migration_limiter = MigrationLimiter::new(MIGRATION_LIMIT_WINDOW, MIGRATION_LIMIT_MAX);
    // 移行後に旧 session を終了するタスク。run の終了時に打ち切る
    let mut old_session_closers: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    // WebTransport の drain (WT_DRAIN_SESSION / HTTP/3 GOAWAY) を観測する。
    // QUIC 直接接続と QMux では drain の通知が無いため `None` になる
    let mut wt_session_state_rx: Option<tokio::sync::watch::Receiver<WtSessionState>> =
        client.wt_session_state();

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
            // alias の対応表はタスクごとに複製する (移行で書き換わっても
            // 既に処理中の stream の解釈は変わらない)
            let track_map_snapshot = track_map
                .read()
                .expect("track_map lock must not be poisoned")
                .clone();
            spawn_stream_task(
                &mut join_set,
                &task_monitor,
                stream,
                permit,
                total_streams,
                &track_map_snapshot,
                &media_fetch_targets,
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
                &audio_catch_up,
                &audio_delivery_order,
                &last_received_locations,
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
                // 移行後の再購読の Location Filter に使うため、配送した catalog Object の
                // 位置も記録する
                record_last_location(
                    &last_received_locations,
                    TrackKind::Catalog,
                    object.location,
                );
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
                            new_session_uri,
                            timeout,
                            on_request_stream,
                        })) => {
                            tracing::info!(
                                "Received GOAWAY (timeout={timeout}, on_request_stream={on_request_stream:?})"
                            );
                            // 移行の判断は tokio-moq の共通関数が持つ
                            // (draft-ietf-moq-transport-22 §6.6.1 (Graceful Session Migration) /
                            // §9.2 (GOAWAY)。draft 由来の仕様であり将来変更されうる)
                            match tokio_moq::migration_target(
                                &new_session_uri,
                                on_request_stream,
                                &config.url,
                            ) {
                                // request stream の GOAWAY で New Session URI を解釈できない
                                // 場合は session を維持する (該当 request の終端は Session の
                                // deadline に任せる)
                                Ok(None) => Ok(false),
                                Ok(Some(target)) => match target.scope {
                                    MigrationScope::Session => {
                                        // 移行の連鎖を抑え、移行中は二重に移行しない
                                        match begin_migration(
                                            migration_in_progress,
                                            &mut migration_limiter,
                                            Instant::now(),
                                        ) {
                                            MigrationStart::AlreadyInProgress => {
                                                tracing::debug!(
                                                    "Ignoring a GOAWAY received while a migration is in progress"
                                                );
                                                return Ok(false);
                                            }
                                            MigrationStart::RateLimited => {
                                                tracing::warn!(
                                                    "Refusing to migrate the session: too many migrations within {:?}",
                                                    MIGRATION_LIMIT_WINDOW,
                                                );
                                                goaway_received = true;
                                                return Ok(true);
                                            }
                                            MigrationStart::Start => {}
                                        }
                                        // 移行先 URI の msf fragment の connection が現在の
                                        // 接続経路と矛盾する場合は移行しない
                                        // (draft-ietf-moq-msf-01 §11.1.1)
                                        if let Err(e) = tokio_moq::resolve_transport(
                                            Some(config.transport),
                                            &target.url,
                                        ) {
                                            tracing::warn!(
                                                "GOAWAY migration target conflicts with the current transport: {e}"
                                            );
                                            // GOAWAY を受けた後に終了するため、購読の終了
                                            // (STOP_SENDING) を送る経路に乗せる
                                            goaway_received = true;
                                            return Ok(true);
                                        }
                                        {
                                            // control stream の GOAWAY: 移行先の確立と購読の
                                            // 張り直しはバックグラウンドのタスクへ移す。main
                                            // ループは旧 session の受信・catalog の適用・stream
                                            // の accept を続ける (§6.6.1)
                                            let deadline = MigrationDeadline::after_goaway(
                                                Instant::now(),
                                                timeout,
                                            );
                                            let last_locations = *last_received_locations
                                                .lock()
                                                .expect(
                                                    "last locations lock is not poisoned",
                                                );
                                            let request = MigrationRequest {
                                                url: target.url,
                                                transport: config.transport,
                                                cert: config.cert.clone(),
                                                plan: resubscribe_plan.clone(),
                                                last_locations,
                                                deadline,
                                            };
                                            let (task, receiver) =
                                                spawn_migration(&task_monitor, request);
                                            migration_task = Some(task);
                                            migration_rx = Some(receiver);
                                            migration_in_progress = true;
                                            tracing::info!(
                                                "Started a background migration: timeout={}ms, local_default={}, last_locations={:?}",
                                                timeout,
                                                deadline.uses_local_default(),
                                                last_locations,
                                            );
                                            Ok(false)
                                        }
                                    }
                                    MigrationScope::Request(request_id) => {
                                        // request stream の GOAWAY は当該 request だけの移行で
                                        // あり、session は維持する (§9.2)
                                        if migration_in_progress {
                                            tracing::debug!(
                                                "Ignoring a request-scoped GOAWAY received while a migration is in progress: request_id={request_id}"
                                            );
                                            Ok(false)
                                        } else if !same_session_url(&target.url, &config.url) {
                                            // 別 URI への request 単位移行は 2 session の併走が
                                            // 必要になるため行わない。session も移行しない
                                            tracing::warn!(
                                                "Cannot re-issue request {request_id} on a new session URI: migrating a single request requires two concurrent sessions"
                                            );
                                            Ok(false)
                                        } else {
                                            let Some(kind) = goaway_request_kind(
                                                request_id,
                                                catalog_request_id,
                                                video_request_id,
                                                audio_request_id,
                                            ) else {
                                                tracing::warn!(
                                                    "Ignoring a request-scoped GOAWAY for an unknown request: request_id={request_id}"
                                                );
                                                return Ok(false);
                                            };
                                            tracing::info!(
                                                "Re-issuing the {kind:?} request after a request-scoped GOAWAY: request_id={request_id}"
                                            );
                                            let last_locations = *last_received_locations
                                                .lock()
                                                .expect(
                                                    "last locations lock is not poisoned",
                                                );
                                            match reissue_request(
                                                &mut client,
                                                kind,
                                                request_id,
                                                &resubscribe_plan,
                                                &last_locations,
                                            )
                                            .await
                                            {
                                                Ok(reissued) => {
                                                    // 旧 alias を外して新しい alias を登録する
                                                    // (alias は peer が決めるため変わりうる)
                                                    let old_alias = match kind {
                                                        TrackKind::Catalog => Some(catalog_alias),
                                                        TrackKind::Video => video_alias,
                                                        TrackKind::Audio => audio_alias,
                                                    };
                                                    let new_alias =
                                                        reissued.subscription.track_alias;
                                                    {
                                                        let mut map = track_map
                                                            .write()
                                                            .expect(
                                                                "track_map lock must not be poisoned",
                                                            );
                                                        if let Some(alias) = old_alias {
                                                            map.remove(&alias);
                                                        }
                                                        map.insert(new_alias, kind);
                                                    }
                                                    match kind {
                                                        TrackKind::Catalog => {
                                                            catalog_request_id =
                                                                reissued.subscription.request_id;
                                                            catalog_alias = new_alias;
                                                        }
                                                        TrackKind::Video => {
                                                            video_request_id = Some(
                                                                reissued.subscription.request_id,
                                                            );
                                                            video_alias = Some(new_alias);
                                                        }
                                                        TrackKind::Audio => {
                                                            audio_request_id = Some(
                                                                reissued.subscription.request_id,
                                                            );
                                                            audio_alias = Some(new_alias);
                                                            // 同じ session で購読を張り直した
                                                            // ため、送出順の状態は引き継ぎ、
                                                            // 境界だけを再発行前の最後の受信
                                                            // 位置へ更新する。新しい購読の Largest
                                                            // Object を使うと再発行中に届いた音声を
                                                            // 境界以下として捨ててしまう
                                                            audio_delivery_order
                                                                .lock()
                                                                .expect(
                                                                    "audio delivery order lock is not poisoned",
                                                                )
                                                                .set_boundary(
                                                                    last_locations
                                                                        .get(TrackKind::Audio),
                                                                );
                                                        }
                                                    }
                                                    // 再発行に伴うメディア FETCH の対応表を足す。
                                                    // 同じ track の旧 Request ID は、遅延して届く
                                                    // FETCH / fill stream を誤配送しないよう先に外す
                                                    media_fetch_targets
                                                        .retain(|_, track| *track != kind);
                                                    media_fetch_targets
                                                        .extend(reissued.media_fetch_targets);
                                                    // datagram 受信タスクへ新しい alias 表を渡す
                                                    let datagram_map = Arc::new(
                                                        track_map
                                                            .read()
                                                            .expect(
                                                                "track_map lock must not be poisoned",
                                                            )
                                                            .clone(),
                                                    );
                                                    let _ = datagram_session_tx.send((
                                                        client.handle(),
                                                        data_plane.clone(),
                                                        datagram_map,
                                                    ));
                                                    tracing::info!(
                                                        "Re-issued the {kind:?} request: alias={new_alias}, request_id={}",
                                                        reissued.subscription.request_id,
                                                    );
                                                }
                                                Err(e) => {
                                                    // 再発行に失敗しても session は維持する
                                                    // (当該購読だけが失われ、live の受信は他の
                                                    // 購読で続く)
                                                    tracing::warn!(
                                                        "Failed to re-issue request {request_id} after a request-scoped GOAWAY: {e}"
                                                    );
                                                }
                                            }
                                            Ok(false)
                                        }
                                    }
                                },
                                Err(e) => {
                                    tracing::warn!(
                                        "GOAWAY carried an unusable New Session URI; terminating without migration: {e}"
                                    );
                                    goaway_received = true;
                                    Ok(true)
                                }
                            }
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
                        Some(ClientEvent::Session(SessionEvent::FetchOkReceived {
                            request_id,
                            end_location,
                            end_of_track,
                        })) => {
                            // 自側が発行したメディア FETCH の応答である。End Location と
                            // End of Track を残す。対応表への記録は応答 stream が FETCH_OK
                            // より先に届きうるため FETCH の発行時に行っており、ここでは
                            // 記録済みの種別を添えて確定した終端情報を残す
                            let kind = media_fetch_targets.get(&request_id);
                            tracing::info!(
                                "Received FETCH_OK: request_id={request_id}, kind={kind:?}, \
                                 end_location=({}, {}), end_of_track={end_of_track}",
                                end_location.group_id,
                                end_location.object_id,
                            );
                            Ok(false)
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
            result = async {
                // 移行の結果は oneshot で届く。移行していないときは発火しない
                match migration_rx.as_mut() {
                    Some(rx) => rx.await.map_err(|_| ()),
                    None => std::future::pending().await,
                }
            }, if migration_in_progress => {
                // バックグラウンドの移行タスクの結果を受ける。成功した場合はここで 1 か所に
                // まとめて移行先へ切り替える
                migration_in_progress = false;
                migration_rx = None;
                migration_task = None;
                match result {
                    Ok(Ok(outcome)) => {
                        // 旧 session の stream task を止める。abort したあとに回収することで、
                        // 切替のあと旧 data_plane しか持たない task が sink へ書かないことを
                        // 保証する (§6.6.1 の移行をアプリへ透過にするため)
                        join_set.abort_all();
                        let drained = tokio::time::timeout(
                            MIGRATION_STREAM_DRAIN_TIMEOUT,
                            async {
                                while join_set.join_next().await.is_some() {}
                            },
                        )
                        .await;
                        if drained.is_err() {
                            tracing::warn!(
                                "Timed out waiting for the old stream tasks to stop; switching the session anyway"
                            );
                        }
                        // 旧 session で受理済みの stream は新しい data_plane では処理できない
                        pending_streams.clear();
                        // 旧 session の Track Alias で保留中の Subgroup も捨てる。alias は
                        // session ごとに払い直されるため、残すと新 session の stream が
                        // 保留枠を奪われたうえ、別の track の chunk として解釈されうる
                        pending_subgroups.lock().await.reset();
                        // 旧 session の復号順と追いつきの状態を捨てる。新 session の live を
                        // 古い Group として誤って拒否しないためである
                        video_decode_order.lock().await.reset();
                        audio_catch_up
                            .lock()
                            .expect("audio catch up lock is not poisoned")
                            .reset();
                        // 音声の購読境界を「旧 session で最後に受信した Location」へ更新する。
                        // 移行先の購読はその位置を Location Filter の Absolute Start (0x02) に
                        // して張り直すため、境界と同じ位置だけが再送される。移行先の Largest
                        // Object を境界にすると、移行中に抜けた音声 (最後の受信位置より後) を
                        // 境界以下として捨ててしまう (draft-ietf-moq-transport-22 §9.20.9
                        // (LOCATION FILTER Parameter) Table 6。draft 由来の仕様であり将来
                        // 変更されうる)
                        let audio_boundary = last_received_locations
                            .lock()
                            .expect("last locations lock is not poisoned")
                            .audio;
                        audio_delivery_order
                            .lock()
                            .expect("audio delivery order lock is not poisoned")
                            .reset_for_migration(audio_boundary);
                        tracing::info!(
                            "Updated the audio subscription boundary for the migrated session: audio_boundary={audio_boundary:?}"
                        );
                        // alias 表を移行先のものへ差し替える (alias は peer が決めるため変わる)
                        let datagram_map = {
                            let mut map = track_map
                                .write()
                                .expect("track_map lock must not be poisoned");
                            map.clear();
                            map.insert(
                                outcome.resubscribed.catalog.track_alias,
                                TrackKind::Catalog,
                            );
                            if let Some(ref sub) = outcome.resubscribed.video {
                                map.insert(sub.track_alias, TrackKind::Video);
                            }
                            if let Some(ref sub) = outcome.resubscribed.audio {
                                map.insert(sub.track_alias, TrackKind::Audio);
                            }
                            Arc::new(map.clone())
                        };
                        // 移行先の WebTransport の drain を観測できるよう、receiver を差し替える
                        let new_wt_session_state = outcome.client.wt_session_state();
                        // datagram 受信タスクには、旧 session を閉じる前に新しい handle /
                        // data_plane / alias 表を渡す。閉じた後だと旧接続の受信エラーでタスクが
                        // 終了してしまう
                        let _ = datagram_session_tx.send((
                            outcome.client.handle(),
                            outcome.data_plane.clone(),
                            datagram_map,
                        ));
                        // 旧 session は STOP_SENDING を送ってから NO_ERROR で閉じる (§6.6.1 /
                        // §9.2)。GOAWAY の Timeout は待たず、main ループを塞がないよう別タスクで
                        // 行う
                        let old_request_ids: Vec<u64> = [
                            Some(catalog_request_id),
                            video_request_id,
                            audio_request_id,
                        ]
                        .into_iter()
                        .flatten()
                        .collect();
                        let old_client = std::mem::replace(&mut client, outcome.client);
                        old_session_closers.push(spawn_old_session_close(
                            &task_monitor,
                            old_client,
                            old_request_ids,
                        ));
                        // ここから先が移行先への切替である。関連する状態を 1 か所でまとめて
                        // 差し替え、旧 session の値と混ざらないようにする
                        config.url = outcome.url;
                        recv_acceptor = outcome.recv_acceptor;
                        data_plane = outcome.data_plane;
                        catalog_request_id = outcome.resubscribed.catalog.request_id;
                        catalog_alias = outcome.resubscribed.catalog.track_alias;
                        video_request_id = outcome
                            .resubscribed
                            .video
                            .as_ref()
                            .map(|sub| sub.request_id);
                        video_alias = outcome
                            .resubscribed
                            .video
                            .as_ref()
                            .map(|sub| sub.track_alias);
                        audio_request_id = outcome
                            .resubscribed
                            .audio
                            .as_ref()
                            .map(|sub| sub.request_id);
                        audio_alias = outcome
                            .resubscribed
                            .audio
                            .as_ref()
                            .map(|sub| sub.track_alias);
                        // メディア FETCH の対応表を移行先のものへ差し替える。Request ID は
                        // session ごとに払い出されるため、旧 session の表で移行先の FETCH 応答
                        // stream を振り分けることはできない
                        media_fetch_targets = outcome.resubscribed.media_fetch_targets;
                        wt_session_state_rx = new_wt_session_state;
                        tracing::info!(
                            "Migrated the session to {}",
                            config.url.authority
                        );
                    }
                    Ok(Err(failure)) => {
                        // 期限超過・再試行不能なエラーである。旧 session はまだ生きており、
                        // GOAWAY は subscription state に影響しない
                        // (draft-ietf-moq-transport-22 §9.2。draft 由来の仕様であり将来変更
                        // されうる) ため、購読を STOP_SENDING で終了してから GOAWAY /
                        // 正常 close を送る既存の後始末へ進む (peer_ended は立てない。この分岐は
                        // 直接受信ループを抜けるため、後始末の判定に使う `goaway_received` は
                        // 立てる必要がない)
                        tracing::warn!("Failed to migrate the session: {failure}");
                        break 'main;
                    }
                    Err(()) => {
                        // 移行タスクが結果を送らずに終了した。移行を続けられないため、
                        // 旧 session の終了経路へ進む
                        tracing::warn!("The migration task ended without a result");
                        break 'main;
                    }
                }
            }
            changed = async {
                // WebTransport の drain (WT_DRAIN_SESSION / HTTP/3 GOAWAY) を観測する
                // (draft-ietf-webtrans-http3-16 §4.7 / draft-ietf-webtrans-http2-15 §6.13)。
                // draft 由来の仕様であり将来変更されうる。QUIC 直接接続と QMux では
                // `wt_session_state_rx` が `None` になり、この分岐は発火しない
                match wt_session_state_rx.as_mut() {
                    Some(rx) => rx.changed().await.map_err(|_| ()),
                    None => std::future::pending().await,
                }
            } => {
                if changed.is_err() {
                    // 送信側 (セッションを所有するタスク) が消えた。以後の changed() は
                    // 即座に Err を返して空回りするため、分岐を無効化する
                    tracing::debug!(
                        "The WebTransport session state watch is closed; drain detection is disabled"
                    );
                    wt_session_state_rx = None;
                } else if let Some(state) = wt_session_state_rx
                    .as_mut()
                    .map(|rx| *rx.borrow_and_update())
                {
                    match state {
                        WtSessionState::Draining => {
                            // drain だけが届いた場合は、現在の URL を移行先として移行を
                            // 始める。GOAWAY はまだ届いていないため Timeout 0 (ローカルの
                            // 既定値) を使う
                            match begin_migration(
                                migration_in_progress,
                                &mut migration_limiter,
                                Instant::now(),
                            ) {
                                MigrationStart::AlreadyInProgress => {
                                    tracing::debug!(
                                        "Ignoring a transport drain while a migration is in progress"
                                    );
                                }
                                MigrationStart::RateLimited => {
                                    // drain は session の利用を禁じない (MAY continue)
                                    // ため、移行を始めずに GOAWAY か通常の終了を待つ
                                    tracing::warn!(
                                        "Refusing to migrate on transport drain: too many migrations within {:?}",
                                        MIGRATION_LIMIT_WINDOW,
                                    );
                                }
                                MigrationStart::Start => {
                                    let deadline =
                                        MigrationDeadline::after_goaway(Instant::now(), 0);
                                    let last_locations = *last_received_locations
                                        .lock()
                                        .expect("last locations lock is not poisoned");
                                    let request = MigrationRequest {
                                        url: config.url.clone(),
                                        transport: config.transport,
                                        cert: config.cert.clone(),
                                        plan: resubscribe_plan.clone(),
                                        last_locations,
                                        deadline,
                                    };
                                    let (task, receiver) =
                                        spawn_migration(&task_monitor, request);
                                    migration_task = Some(task);
                                    migration_rx = Some(receiver);
                                    migration_in_progress = true;
                                    tracing::info!(
                                        "Started a background migration on transport drain: local_default={}, last_locations={:?}",
                                        deadline.uses_local_default(),
                                        last_locations,
                                    );
                                }
                            }
                        }
                        // 終了の通知は通常の終了経路 (accept の終了 / next_event のエラー) に
                        // 任せる
                        WtSessionState::ClosedByPeer
                        | WtSessionState::ClosedLocally
                        | WtSessionState::ConnectionFailed => {
                            tracing::debug!(
                                "Observed the WebTransport session state {state:?}; the normal termination path handles it"
                            );
                        }
                        WtSessionState::Active => {}
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
                client.tick_now();
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
                // 音声の配送で落とした Object の累計が、前回のログから増えていれば出す。
                // 1 件ごとの debug ログは既定の RUST_LOG で消えるため、原因の切り分けには
                // 増えたときだけ info で出す
                let counters = audio_delivery_order
                    .lock()
                    .expect("audio delivery order lock is not poisoned")
                    .counters();
                log_audio_delivery_counters(&mut logged_audio_delivery_counters, counters);
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

    // 進行中の移行タスクを打ち切る。確立済みの移行先 session は `MoqtClient` の drop で
    // transport が閉じる。ここでの打ち切りは異常ではない (run 自体の終了が先に決まった)
    if let Some(handle) = migration_task.take() {
        handle.abort();
        let _ = handle.await;
        tracing::debug!("Aborted the in-flight migration task");
    }
    // 旧 session の終了タスクは best-effort である。run の終了を待たせないよう打ち切る
    // (旧 session の transport は `MoqtClient` の drop で閉じる)
    for handle in old_session_closers.drain(..) {
        handle.abort();
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
        if let Err(e) = client.stop_sending(catalog_request_id).await {
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

    // datagram 受信タスクを止める。このタスクはデコード済み音声フレームの送信側を保持して
    // いるため、残したままだとプレイヤーが「音声チャネルの切断」を観測できず、終了できない
    // (接続は既に閉じており、datagram の受信を続ける意味も無い)。
    datagram_task.abort();
    let _ = datagram_task.await;

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
/// WebTransport over HTTP/2 も driver が受領した `WT_STOP_SENDING` のエラーコードを
/// `StreamReset` として返すため本判定の対象になる (draft-ietf-webtrans-http2-15 §6.3)。
/// WebTransport over HTTP/3 の送信 API も同じ `StreamReset` を返す (コードは HTTP/3 の
/// code space のままで、MOQT のコードへ写すのは観測経路だけである)。
/// 本判定は variant のみを見るため、コードの空間に依存しない。
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

/// 受信 data stream から次のチャンクを読む
///
/// Session が DATA_STREAM_TIMEOUT で STOP_SENDING を指示した場合は、受信半に STOP_SENDING を
/// 送出して読み取りを打ち切る (`SessionEvent::StopSendingDataStream` の doc と
/// draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) を参照)。
/// セッションは閉じないため、購読と他の stream の受信は継続する。
async fn receive_registered_stream_data(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
) -> Result<Option<Bytes>> {
    tokio::select! {
        biased;
        error_code = data_plane.wait_data_stream_stop_sending(stream_id) => {
            // Session 側は破棄対象として会計を閉じている。送出に失敗しても読み取りは打ち切る
            // (peer は応答の reset を返すか、遅延した FIN で終端する)。
            // 送出の前に指示の登録を破棄し、同じ stream へ STOP_SENDING を二重に送らない
            // (同じ stream を読み直す呼び出し元 (保留 subgroup の保持ループなど) でも再送されない)。
            // 送出は QMux 経路では await を伴うため、この関数を包む timeout で中断されても
            // 登録が残らないようにする (残ると読み直しのたびに再送される)
            data_plane.clear_data_stream_stop_sending(stream_id);
            if let Err(e) = stream.stop_sending(error_code).await {
                tracing::warn!(
                    "Failed to send STOP_SENDING for timed out data stream: \
                     stream_id={}, error_code={error_code:#x} {e}",
                    stream_id.0
                );
            }
            Ok(None)
        }
        chunk = stream.receive_chunk() => match chunk? {
            transport::RecvChunk::Data(data) => Ok(Some(data)),
            transport::RecvChunk::End(end) => {
                data_plane.recv_data_stream_closed(stream_id, end)?;
                Ok(None)
            }
        },
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
    /// 解決した映像 track の targetLatency の決定に使う情報
    video_latency: Option<SelectedTrackLatency>,
    /// 解決した音声 track の targetLatency の決定に使う情報
    audio_latency: Option<SelectedTrackLatency>,
}

/// `targetLatency` の決定に使う、購読する track の情報
#[derive(Clone, Copy)]
struct SelectedTrackLatency {
    /// 宣言された render group (宣言が無ければ `None`)
    render_group: Option<u64>,
    /// track 自身の `targetLatency` (ミリ秒)
    target_latency: Option<u64>,
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
    catalog_namespace: TrackNamespace,
) -> Result<CatalogResolution> {
    let deadline = tokio::time::Instant::now() + CATALOG_TIMEOUT;
    // delta の namespace 継承は catalog JSON の `namespace` との文字列比較になるため、
    // publisher が書く表現と同じ draft-ietf-moq-transport-22 §8.8 の表現へ直す
    let mut state = catalog::CatalogState::new(serialize_namespace(&catalog_namespace));
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
                    // 外すため `send_data_stream_stop_sending` を呼ぶ (呼ばないと受信 FETCH stream が
                    // 残り、FETCH stream の data stream timeout でセッションが落ちる)
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
                    // 読み切れなかった stream を Session の会計から外す。起動時の取得は最初の
                    // `tick` より前に走るため data stream timeout の会計には入っていないが、
                    // 受信 stream と購読の後始末のために fill fetch の失敗経路と同じ扱いにする
                    Ok(Err(e)) => {
                        tracing::warn!("Failed to read catalog stream: {e}");
                        let _ = data_plane.send_data_stream_stop_sending(stream_id);
                    }
                    Err(_) => {
                        tracing::warn!(
                            "Catalog stream did not finish within {}ms",
                            CATALOG_TIMEOUT.as_millis()
                        );
                        let _ = data_plane.send_data_stream_stop_sending(stream_id);
                    }
                }
            }
        }
        // 読み終わった stream の STOP_SENDING 指示の登録を破棄する
        data_plane.clear_data_stream_stop_sending(stream_id);
    }

    let Some(catalog) = state.catalog() else {
        return Err(Error::Other(format!(
            "catalog did not arrive within {}ms",
            CATALOG_TIMEOUT.as_millis()
        )));
    };
    // targetLatency は再生側の時間軸が表示の遅れの下限に使う。どの track を購読するかは
    // 呼び出し側が決めるため、ここでは選択した track の情報だけを返し、値の決定は呼び出し側で行う
    let video_track = catalog.tracks.iter().find(|t| {
        t.codec.as_ref().is_some_and(|c| {
            c.starts_with("av01")
                || c.starts_with("avc1")
                || c.starts_with("hvc1")
                || c.starts_with("hev1")
                // VP8 は登録名のみ、VP9 は `vp09.<profile>.<level>.<bit depth>` である
                || c == "vp8"
                || c == "vp09"
                || c.starts_with("vp09.")
        })
    });
    // 音声トラックは本 example がデコードできる codec (opus / mp4a.* (AAC)) から選ぶ。
    // codec の文字列は draft-ietf-moq-msf-01 §5.2.18 (Codec) が定める RFC 6381 の表記である。
    // デコードできない codec を購読すると、録画中でも再生時にデコーダを生成できずに失敗する。
    let audio_track = catalog.tracks.iter().find(|t| {
        t.codec
            .as_deref()
            .is_some_and(decoder::is_supported_audio_codec)
    });
    let selected_latency = |track: Option<&MsfTrack>| {
        track.map(|track| SelectedTrackLatency {
            render_group: track.render_group,
            target_latency: track.target_latency,
        })
    };
    let video = video_track
        .map(|track| extract_video_info(track, &catalog_namespace))
        .transpose()?;
    let audio = audio_track
        .map(|track| extract_audio_info(track, &catalog_namespace))
        .transpose()?;
    // `state` を move する前に catalog への借用を終わらせる
    let video_latency = selected_latency(video_track);
    let audio_latency = selected_latency(audio_track);
    if video.is_none() && audio.is_none() {
        return Err(Error::Other(
            "no usable tracks found in catalog (expected av01/avc1/hvc1/hev1/opus/mp4a)"
                .to_string(),
        ));
    }
    Ok(CatalogResolution {
        state,
        video,
        audio,
        video_latency,
        audio_latency,
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
    read_catalog_fetch_objects(stream, data_plane, stream_id, &mut decoder).await
}

/// FETCH_HEADER を登録済みの fetch stream から catalog Object を読み切る
///
/// FETCH_HEADER の decode と `DataPlaneHandle::recv_fetch_header` による登録は呼び出し側の
/// 責務である。登録済みの stream へ同じヘッダを再登録すると
/// `session error` になるため、ヘッダの扱いは [`read_catalog_fetch_stream`] と
/// [`handle_stream_body`] に残し、本関数は entry のループだけを担う。読み出しの終端検知は
/// [`receive_registered_stream_data`] に任せる (同関数が `recv_data_stream_closed` で終端を
/// 通知しており、ここで改めて drain すると二重通知になる)。
async fn read_catalog_fetch_objects(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    decoder: &mut FetchStreamDecoder,
) -> Result<Vec<catalog::CatalogObject>> {
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
    read_catalog_objects(stream, data_plane, stream_id, &mut decoder, &header).await
}

/// 登録済みの catalog の Subgroup stream から Object を読み切る
///
/// ヘッダの登録 (`DataPlaneHandle::recv_subgroup_header`) は呼び出し側の責務である。
/// 登録に成功した stream へ同じヘッダを再登録すると `session error 0x3: subgroup header
/// already received for stream id` になるため、ヘッダの decode と登録は
/// [`read_catalog_subgroup_stream`] に残し、本関数は Object のループだけを担う。
/// `decoder` は呼び出し側が保持する復号器であり、`header` はその復号結果である
/// (`SubgroupStreamDecoder` は復号済みのヘッダを保持しない)。
async fn read_catalog_objects(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    decoder: &mut SubgroupStreamDecoder,
    header: &SubgroupHeader,
) -> Result<Vec<catalog::CatalogObject>> {
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
        let payload = read_catalog_payload(stream, data_plane, stream_id, decoder).await?;
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

/// 実際に購読する track の `targetLatency` の決定に使う情報を選ぶ
///
/// 購読しない track は対象から外す (`--no-video` / `--no-audio` の指定を反映する)。
fn subscribed_track_latencies(
    video: Option<SelectedTrackLatency>,
    audio: Option<SelectedTrackLatency>,
    subscribe_video: bool,
    subscribe_audio: bool,
) -> Vec<SelectedTrackLatency> {
    [
        if subscribe_video { video } else { None },
        if subscribe_audio { audio } else { None },
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// カタログの track から、購読する track の `targetLatency` (ミリ秒) を求める
///
/// 対象は購読する track の `renderGroup` に属する track に限る。`renderGroup` は
/// 「一緒に描画される track の集合」であり (draft-ietf-moq-msf-01 §5.2.11)、同じ group の
/// track は同じ `targetLatency` を持たなければならない (§5.2.8 の MUST)。購読する group が
/// 複数あるときは、時間軸が持てる値が 1 つだけであるため最大値をすべての購読 track に掛ける
/// (track ごとの値の適用は本 example の範囲外)。
///
/// 購読する track が `renderGroup` を宣言していないときはその track 自身を対象に含める。
/// 購読する track が 1 つも宣言していないとき (および `subscribed` が空のとき) は group が
/// 分からないため、従来どおり全 track を対象にする (`run` は購読する track が 1 つも無ければ
/// 先にエラーにするため、空にはならない)。値が無い track は無視し、複数の値があるときは
/// 安全側に倒して大きい方を採る。対象のどの track も持たないときは `None` (時間軸は自分の
/// 揺らぎから求めた遅れだけを使う)。
fn catalog_target_latency_ms(
    tracks: &[MsfTrack],
    subscribed: &[SelectedTrackLatency],
) -> Option<i64> {
    let values: Vec<u64> = if subscribed.iter().all(|s| s.render_group.is_none()) {
        tracks.iter().filter_map(|t| t.target_latency).collect()
    } else {
        let mut values = Vec::new();
        for s in subscribed {
            match s.render_group {
                Some(group) => values.extend(
                    tracks
                        .iter()
                        .filter(|t| t.render_group == Some(group))
                        .filter_map(|t| t.target_latency),
                ),
                None => values.extend(s.target_latency),
            }
        }
        values
    };
    values
        .into_iter()
        .max()
        .and_then(|value| i64::try_from(value).ok())
}

/// track の購読に使う namespace を解決する
///
/// draft-ietf-moq-msf-01 §5.2.2 (Track namespace) は「track が宣言した namespace は継承した
/// namespace を上書きする」と定める。宣言が無ければ catalog track の namespace を継承する
/// (MSF の継承の規則。delta 適用時の継承は [`catalog::CatalogState`] が扱う)。
///
/// 宣言された namespace は draft-ietf-moq-transport-22 §8.8 (Representing Namespace and Track
/// Names。MSF では §11.1.2 に再掲) の表現として [`parse_namespace`] で解決する。§8.8 の形式は
/// RECOMMENDED であり、draft-ietf-moq-msf-01 §5.2.2 (Track namespace) も catalog JSON の
/// `namespace` を String と定めるのみで符号化を規定しない。本 example は moq-pub が書く §8.8
/// 表現を前提とする契約であり、それ以外の表現 (MSF §5.6.1 の例のような値) を宣言したカタログでは
/// この解決に失敗して起動できない。
///
/// 解決に失敗した場合はエラーにする。カタログは publisher の契約であり、本 example の契約では
/// 解決できない namespace を publisher のバグとして扱う。黙って catalog track の namespace を
/// 継承すると、ユーザーが期待した track ではなく別の track を購読して気づかない誤動作になる。
/// 空の namespace (0 フィールド) も、example が 1 つ以上のフィールドを持つ namespace を扱う前提
/// (`tokio_moq::resolve_namespace` と同じ) から解決失敗として扱う。
///
/// 解決は `--no-video` / `--no-audio` で無効化した track でも行う (既存の必須フィールドの検証と
/// 同じ扱い)。無効化した側の解決に失敗しても example は起動しない。
fn resolve_track_namespace(track: &MsfTrack, inherited: &TrackNamespace) -> Result<TrackNamespace> {
    let namespace = match track.namespace.as_deref() {
        Some(declared) => parse_namespace(declared).map_err(|e| {
            Error::Other(format!(
                "catalog media track {:?} has an invalid namespace {declared:?}: {e} \
                 ('-' separates namespace fields; use the draft-ietf-moq-transport-22 \
                 §8.8 form)",
                track.name
            ))
        })?,
        None => inherited.clone(),
    };
    if namespace.fields().is_empty() {
        return Err(Error::Other(format!(
            "catalog media track {:?} has an empty namespace",
            track.name
        )));
    }
    Ok(namespace)
}

fn extract_video_info(
    track: &MsfTrack,
    inherited_namespace: &TrackNamespace,
) -> Result<VideoTrackInfo> {
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
        namespace: resolve_track_namespace(track, inherited_namespace)?,
        track_name: track.name.clone(),
        codec,
        width,
        height,
        fps,
    })
}

fn extract_audio_info(
    track: &MsfTrack,
    inherited_namespace: &TrackNamespace,
) -> Result<AudioTrackInfo> {
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
        namespace: resolve_track_namespace(track, inherited_namespace)?,
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
    media_fetch_targets: &HashMap<u64, TrackKind>,
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
    audio_params: Option<(u32, u8)>,
    frame_tx: &std::sync::mpsc::Sender<DecodedVideoFrame>,
    audio_tx: &std::sync::mpsc::Sender<DecodedAudioFrame>,
    stream_num: u64,
    display_backlog: std::sync::Arc<std::sync::atomic::AtomicI64>,
    video_decode_order: &std::sync::Arc<tokio::sync::Mutex<VideoDecodeOrder>>,
    audio_decoder: Option<&std::sync::Arc<tokio::sync::Mutex<AudioDecoder>>>,
    audio_config_handled: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    audio_catch_up: &std::sync::Arc<std::sync::Mutex<AudioReceiveCatchUp>>,
    audio_delivery_order: &std::sync::Arc<std::sync::Mutex<AudioDeliveryOrder>>,
    last_received_locations: &std::sync::Arc<std::sync::Mutex<LastReceivedLocations>>,
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
        &data_plane,
        track_map,
        media_fetch_targets,
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
        audio_catch_up,
        audio_delivery_order,
        last_received_locations,
        termination_tx,
        catalog_tx,
        pending_subgroups,
        playback,
        recorder,
    )
    .await;
    // stream の読みが終わったら STOP_SENDING 指示の登録を破棄する (登録を残すと
    // 指示が出た stream 数に比例してメモリが増える)
    data_plane.clear_data_stream_stop_sending(DataStreamId(raw_stream_id));
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
    data_plane: &DataPlaneHandle,
    track_map: &HashMap<u64, TrackKind>,
    media_fetch_targets: &HashMap<u64, TrackKind>,
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
    audio_params: Option<(u32, u8)>,
    frame_tx: &std::sync::mpsc::Sender<DecodedVideoFrame>,
    audio_tx: &std::sync::mpsc::Sender<DecodedAudioFrame>,
    stream_num: u64,
    display_backlog: std::sync::Arc<std::sync::atomic::AtomicI64>,
    video_decode_order: &std::sync::Arc<tokio::sync::Mutex<VideoDecodeOrder>>,
    audio_decoder: Option<&std::sync::Arc<tokio::sync::Mutex<AudioDecoder>>>,
    audio_config_handled: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    audio_catch_up: &std::sync::Arc<std::sync::Mutex<AudioReceiveCatchUp>>,
    audio_delivery_order: &std::sync::Arc<std::sync::Mutex<AudioDeliveryOrder>>,
    last_received_locations: &std::sync::Arc<std::sync::Mutex<LastReceivedLocations>>,
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
            // FETCH 応答 stream は、自側が発行したメディア FETCH の応答と、catalog 購読への
            // fill fetch stream (draft-ietf-moq-transport-22 §3.4 (Fill Semantics)) を配送経路へ
            // 流す。移行後に catalog を張り直すと fill で完全なカタログが届くため、これを
            // CatalogState へ適用する。どちらでもない stream は読み捨てる
            let mut fetch_decoder = FetchStreamDecoder::new();
            fetch_decoder.push(&buf);
            let fetch_header = loop {
                match fetch_decoder.try_decode_header() {
                    Ok(Some(header)) => break header,
                    Ok(None) => {
                        match receive_registered_stream_data(stream, data_plane, stream_id).await {
                            Ok(Some(data)) => fetch_decoder.push(&data),
                            Ok(None) => return,
                            Err(e) => {
                                log_failure(
                                    format_args!(
                                        "Stream #{stream_num}: failed to read fetch header: {e}"
                                    ),
                                    is_transport_session_end(&e),
                                );
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Stream #{stream_num}: failed to decode fetch header: {e}");
                        let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
                        return;
                    }
                }
            };
            match fetch_stream_action(fetch_header.request_id, media_fetch_targets) {
                // 自側の要求に対応しない FETCH 応答である。読み捨てる
                FetchStreamAction::Ignore => {
                    tracing::debug!(
                        "Stream #{stream_num}: dropping a fetch stream \
                         (request_id={} does not belong to this subscriber)",
                        fetch_header.request_id,
                    );
                    let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
                }
                action @ (FetchStreamAction::DeliverVideo
                | FetchStreamAction::DeliverAudio
                | FetchStreamAction::DeliverCatalog) => {
                    // 自側が発行した FETCH の応答であるため、Session には受理される
                    // (FETCH の状態と stream の会計を対応づける)
                    if let Err(e) = data_plane.recv_fetch_header(stream_id, &fetch_header) {
                        // MoqtClient の登録 API は全エラーを `TransportError::Internal` に
                        // 写すため、ここはセッション終了でも warn のままにする
                        tracing::warn!(
                            "Stream #{stream_num}: failed to register fetch header \
                             (request_id={}): {e}",
                            fetch_header.request_id,
                        );
                        let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
                        return;
                    }
                    match action {
                        FetchStreamAction::DeliverVideo => {
                            let Some(codec) = video_codec else {
                                tracing::warn!(
                                    "Stream #{stream_num}: video FETCH response arrived \
                                     but video is not configured"
                                );
                                let _ = data_plane.send_data_stream_stop_sending(stream_id);
                                let _ =
                                    drain_registered_stream_to_end(stream, data_plane, stream_id)
                                        .await;
                                return;
                            };
                            // 復号器は live の Subgroup stream と同じく stream ごとに作る。
                            // 表示待ちが上限を超えているときは live と同じ規則でデコードだけを
                            // スキップし、読み切って録画は続ける
                            let skip_decode =
                                display_backlog.load(Ordering::Relaxed) > MAX_DISPLAY_BACKLOG;
                            let mut video_decoder = match build_stream_video_decoder(
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
                                    let _ = drain_registered_stream_to_end(
                                        stream, data_plane, stream_id,
                                    )
                                    .await;
                                    return;
                                }
                            };
                            read_video_fetch_stream(
                                stream,
                                data_plane,
                                stream_id,
                                &mut fetch_decoder,
                                stream_num,
                                video_decoder.as_mut(),
                                &FrameSink {
                                    frame_tx,
                                    display_backlog: &display_backlog,
                                },
                                video_decode_order,
                                last_received_locations,
                                recorder,
                                termination_tx,
                            )
                            .await;
                        }
                        FetchStreamAction::DeliverAudio => {
                            let Some((sample_rate, channels)) = audio_params else {
                                tracing::warn!(
                                    "Stream #{stream_num}: audio FETCH response arrived \
                                     but audio is not configured"
                                );
                                let _ = data_plane.send_data_stream_stop_sending(stream_id);
                                let _ =
                                    drain_registered_stream_to_end(stream, data_plane, stream_id)
                                        .await;
                                return;
                            };
                            // 音声デコーダと Audio Config の処理済みフラグは購読で共有する
                            // (live の Subgroup stream と同じ)
                            let mut decoder = match audio_decoder {
                                Some(decoder) => Some(decoder.lock().await),
                                None => None,
                            };
                            let delivery = AudioDelivery {
                                audio_tx,
                                audio_config_handled,
                                is_opus_codec: audio_codec.is_some_and(decoder::is_opus_codec),
                                catalog_sample_rate: sample_rate,
                                catalog_channels: channels,
                                recorder,
                                termination_tx,
                                catch_up: audio_catch_up.as_ref(),
                                delivery_order: audio_delivery_order.as_ref(),
                            };
                            read_audio_fetch_stream(
                                stream,
                                data_plane,
                                stream_id,
                                &mut fetch_decoder,
                                stream_num,
                                decoder.as_deref_mut(),
                                &delivery,
                                last_received_locations,
                            )
                            .await;
                        }
                        FetchStreamAction::DeliverCatalog => {
                            // fill fetch stream で届く完全な catalog を、購読で届く catalog と
                            // 同じ `CatalogState` へ適用するため main ループへ渡す
                            // (draft-ietf-moq-msf-01 §5 (Catalog))。移行後に catalog 購読を
                            // 張り直すと、移行中に更新された catalog が fill で届く
                            match read_catalog_fetch_objects(
                                stream,
                                data_plane,
                                stream_id,
                                &mut fetch_decoder,
                            )
                            .await
                            {
                                Ok(objects) => {
                                    for object in objects {
                                        // 移行後の再購読の Location Filter に使う位置も記録する
                                        record_last_location(
                                            last_received_locations,
                                            TrackKind::Catalog,
                                            object.location,
                                        );
                                        if catalog_tx.send(object).await.is_err() {
                                            // main ループが終了した
                                            return;
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Stream #{stream_num}: failed to read catalog fetch stream: {e}"
                                    );
                                    // 読み切れなかった stream を Session の会計から外す
                                    // (既存の Fetch 失敗経路と同じ扱い。残すと timeout まで
                                    // open のままで cleanup が進まない)
                                    let _ = data_plane.send_data_stream_stop_sending(stream_id);
                                    let _ = drain_registered_stream_to_end(
                                        stream, data_plane, stream_id,
                                    )
                                    .await;
                                }
                            }
                        }
                        // 上の or パターンで配送先は決まっている
                        FetchStreamAction::Ignore => {}
                    }
                }
            }
        }
        StreamType::Padding => {
            // draft-ietf-moq-transport-22 §11.5.1 (Padding Streams):
            // パディングバイトを読み捨てる。session には recv_data_stream_type で登録済み。
            let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
        }
        StreamType::Subgroup => {
            let mut sg_decoder = SubgroupStreamDecoder::new();
            sg_decoder.push(&buf);
            let header = loop {
                match sg_decoder.try_decode_header() {
                    Ok(Some(h)) => break h,
                    Ok(None) => {
                        match receive_registered_stream_data(stream, data_plane, stream_id).await {
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
                        let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
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
                let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
                return;
            }
            if !register_subgroup_header(
                stream,
                data_plane,
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
                        let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
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
                            let _ =
                                drain_registered_stream_to_end(stream, data_plane, stream_id).await;
                            return;
                        }
                    };
                    let frames = decode_video_stream(
                        stream,
                        data_plane,
                        stream_id,
                        header.group_id,
                        &mut sg_decoder,
                        video_decode_order,
                        decoder.as_mut(),
                        &FrameSink {
                            frame_tx,
                            display_backlog: &display_backlog,
                        },
                        last_received_locations,
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
                        let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
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
                    // catalog の codec が opus の場合のみ OpusHead として解釈する。AAC (mp4a.*) の
                    // Audio Config は AudioSpecificConfig であり、AudioToolbox のデコーダは
                    // 入力のサンプルレートとチャンネル数だけで構成できるため解釈しない。
                    // 処理済みフラグは subscription で共有する: decode_audio_stream は
                    // subgroup stream ごとに呼ばれるが、OpusHead はセッションで 1 回しか
                    // 送られないため、共有しないと 2 本目以降の stream が OpusHead を
                    // 待ち続けて音声が途切れる。datagram 配送の音声とも共有する。
                    let chunks = decode_audio_stream(
                        stream,
                        data_plane,
                        stream_id,
                        &header,
                        &mut sg_decoder,
                        decoder.as_deref_mut(),
                        &AudioDelivery {
                            audio_tx,
                            audio_config_handled,
                            is_opus_codec: audio_codec.is_some_and(decoder::is_opus_codec),
                            catalog_sample_rate: sample_rate,
                            catalog_channels: channels,
                            recorder,
                            termination_tx,
                            catch_up: audio_catch_up.as_ref(),
                            delivery_order: audio_delivery_order.as_ref(),
                        },
                        last_received_locations,
                    )
                    .await;
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
                    // ヘッダは既に `register_subgroup_header` が登録済みであるため、
                    // `read_catalog_subgroup_stream` (ヘッダ登録を含む入口) は呼ばない。
                    // 呼ぶと同じ stream へ再登録となり `session error 0x3` になる。
                    // `buf` (ヘッダを含む) も渡さない。
                    match read_catalog_objects(
                        stream,
                        data_plane,
                        stream_id,
                        &mut sg_decoder,
                        &header,
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
                            // 読み切れなかった stream を Session の会計から外す。残すと timeout まで
                            // open_incoming_subgroup_count が戻らず、cleanup_ready() が false のまま
                            // になって forget_subscription と GOAWAY drain が進まない
                            // (fill fetch の失敗経路と同じ扱い)
                            let _ = data_plane.send_data_stream_stop_sending(stream_id);
                        }
                    }
                }
                None => {
                    tracing::warn!(
                        "Stream #{stream_num}: unknown track alias={}",
                        header.track_alias,
                    );
                    let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
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
/// (会計から外して読み捨て済み。wire の STOP_SENDING は送らない)。
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

/// 保留を打ち切った Subgroup ストリームを Session の会計から外し、終端の通知まで読み捨てる
///
/// `send_data_stream_stop_sending` は Session の会計を閉じる通知であり、subgroup stream では
/// wire の STOP_SENDING を送らない (wire の送出は timeout 経路で I/O 層が行う)。会計を閉じないと
/// timeout まで受信 stream が残り、購読の `cleanup_ready()` と GOAWAY drain が進まない。
/// peer が終端を返さない場合この drain は待ち続ける (既知の制限)。
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
    last_received_locations: &std::sync::Arc<std::sync::Mutex<LastReceivedLocations>>,
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
        // 移行後の再購読で使う「最後に配送した位置」を記録する。配送しない Object や
        // payload を持たない Object は再開位置として意味が無いため記録しない
        record_last_location(
            last_received_locations,
            TrackKind::Video,
            Location {
                group_id,
                object_id: obj.object_id,
            },
        );
        // 配送は live の Subgroup stream と FETCH 応答 stream で同じ関数を通す
        match deliver_video_object(
            group_id,
            obj.object_id,
            obj.properties_bytes.as_deref(),
            &payload,
            video_decode_order,
            video_decoder.as_deref_mut(),
            sink,
            recorder,
            termination_tx,
        )
        .await
        {
            VideoDelivery::Delivered(frames_delivered) => frames += frames_delivered,
            VideoDelivery::Terminated => return frames,
        }
    }
}

/// 映像 Object 1 つを配送経路へ流した結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoDelivery {
    /// 配送した (復号して送出したフレーム数)。配送しなかった場合も 0 で表す
    Delivered(u64),
    /// LOC Properties の書式違反によりセッション終了を依頼した (読み取りを打ち切る)
    Terminated,
}

/// 映像 Object 1 つを live と同じ配送経路 (録画 → 復号順の判定 → 復号 → 送出) へ流す
///
/// live の Subgroup stream と FETCH 応答 stream が共有する。`group_id` は Object が属する
/// Group であり、Subgroup stream ではヘッダの Group ID、FETCH 応答 stream では entry の
/// Group ID を使う。
///
/// `video_decode_order` は購読で共有する復号順の判定であり、`video_decoder` は stream ごとに
/// 作る復号器である。判定が `Skip` の Object (`Stale` / `MissingReference`) は復号しない
/// (キーフレームから復号を始める規則は [`VideoDecodeOrder`] が持つ)。録画は復号の可否に
/// 関わらず行う。
///
/// `video_decode_order` は FETCH 応答でも live と共有するため、live で復号中の Group より
/// 古い Group を FETCH で取得した場合、その Object は `Stale` として復号されない
/// (取得した範囲は、live の録画が始まる前に届いた分だけが録画に残る。録画は最初の
/// キーフレームを基準時刻にし、それより前の時刻の Object を破棄するためである)。
#[expect(clippy::too_many_arguments)]
async fn deliver_video_object(
    group_id: u64,
    object_id: u64,
    properties_bytes: Option<&[u8]>,
    payload: &[u8],
    video_decode_order: &std::sync::Arc<tokio::sync::Mutex<VideoDecodeOrder>>,
    video_decoder: Option<&mut decoder::VideoDecoder>,
    sink: &FrameSink<'_>,
    recorder: Option<&RecorderSender>,
    termination_tx: &tokio::sync::mpsc::Sender<SessionError>,
) -> VideoDelivery {
    // 表示時刻を決めるために LOC の Timestamp も取り出す
    // (書式違反は配送の有無や処理済みフラグに関わらず毎 Object で検出する)
    let timestamp_us = match extract_timestamp_timescale(properties_bytes) {
        Ok((timestamp, timescale)) => loc_timestamp_us(timestamp, timescale),
        Err(e) => {
            request_session_termination(termination_tx, &e);
            return VideoDelivery::Terminated;
        }
    };
    let video_config = match extract_video_config(properties_bytes) {
        Ok(config) => config,
        Err(e) => {
            request_session_termination(termination_tx, &e);
            return VideoDelivery::Terminated;
        }
    };
    if let Some(recorder) = recorder
        && let Err(e) =
            record_video_object(recorder, properties_bytes, video_config.as_deref(), payload)
    {
        request_session_termination(termination_tx, &e);
        return VideoDelivery::Terminated;
    }
    // 再生しない指定と、表示待ちの超過でデコードをスキップする場合は復号器が無い。
    // 録画だけを行って配送を終える
    let Some(video_decoder) = video_decoder else {
        return VideoDelivery::Delivered(0);
    };
    // キーフレーム判定は録画と同じ規則を使う。PROP_VIDEO_FRAME_MARKING が無い publisher の
    // 映像も復号できるように、PROP_VIDEO_CONFIG の有無で代用する。
    let is_key_frame = match is_video_keyframe(properties_bytes, video_config.as_deref()) {
        Ok(keyframe) => keyframe,
        Err(e) => {
            request_session_termination(termination_tx, &e);
            return VideoDelivery::Terminated;
        }
    };
    // Prior Object ID Gap は Object Properties から読む (Property が無ければ 0)
    let prior_object_id_gap = match prior_object_id_gap_of(properties_bytes) {
        Ok(gap) => gap,
        Err(e) => {
            request_session_termination(termination_tx, &e);
            return VideoDelivery::Terminated;
        }
    };
    // 復号してよい Object か判定する。Group ごとに別の stream で届くため、判定の状態は
    // 購読で共有する (draft-ietf-moq-transport-22 §2.1.2 (Object States))。ロックは判定の
    // 間だけ持ち、復号 (block_in_place) の間は持たない。
    let admission = {
        let mut decode_order = video_decode_order.lock().await;
        decode_order.admit(&VideoObjectPosition {
            group_id,
            object_id,
            is_key_frame,
            prior_object_id_gap,
        })
    };
    if let VideoObjectAdmission::Skip { reason } = admission {
        // 捨てるのは参照するフレームが無い Object であり、異常ではない。毎 Object で
        // warn を出すとログが埋まるため debug に留める
        tracing::debug!(
            "Skipping video object (group={group_id}, object={object_id}, reason={reason:?})",
        );
        return VideoDelivery::Delivered(0);
    }
    let frames = tokio::task::block_in_place(|| {
        decode_and_send(
            payload,
            video_config.as_deref(),
            timestamp_us,
            video_decoder,
            sink,
        )
    });
    VideoDelivery::Delivered(frames)
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

/// 受信の入口で測った入力の遅れ (ミリ秒) を求める
///
/// Object の到着の壁時計 − LOC TIMESTAMP である。受信側の追いつき
/// ([`shiguredo_moqt::playout::catch_up`]) が、復号の出力の遅れからこの値を引いた
/// 「復号が足している遅れ」で復号器を作り直すかどうかを決める。経路が遅いだけの状態では
/// この値も同じだけ上がるため作り直さず、受信側が溜めたまま戻らない状態 (この値は健全で
/// 出力の遅れだけが大きい) で作り直す。
///
/// 壁時計の TIMESTAMP を持つ音でだけ測れる。判定は [`loc_timestamp_us`] と同じ規則
/// (Timescale が無いか 0 なら Timestamp は Unix epoch からのマイクロ秒である) に従う。
/// TIMESTAMP が無い音は表示の時刻を決められず、Timescale がある音はメディア時刻であり
/// 壁時計と対応しないため、どちらも `None` を返す。`None` のときの判定は出力の遅れを
/// そのまま使う (入力の遅れを導入する前と同じ挙動)。
///
/// `timestamp` と `timescale` は LOC の Timestamp / Timescale (無い場合は `None`)、
/// `arrival_us` は Object の到着の壁時計 (マイクロ秒)、`pts_us` は TIMESTAMP から求めた
/// 時刻 (マイクロ秒) である。
fn receive_input_lag_ms(
    timestamp: Option<u64>,
    timescale: Option<u64>,
    arrival_us: i64,
    pts_us: i64,
) -> Option<i64> {
    // TIMESTAMP が無い音では測れない
    timestamp?;
    if matches!(timescale, Some(timescale) if timescale > 0) {
        // Timescale がある音はメディア時刻であり、壁時計の到着と対応しない
        return None;
    }
    Some(arrival_us.saturating_sub(pts_us) / 1_000)
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

/// datagram の受信バッチが空のときに、受信ループを休めるかを返す
///
/// `recv_datagrams` は QUIC と QMux では 1 件以上届くまで待つため、バッチがあるときの
/// sleep はそのまま受信から再生までの遅延になる。WebTransport の
/// `take_buffered_datagrams` はバッファが空でも即座に返りうるため、空のバッチのときだけ
/// sleep して空回りを防ぐ。sleep の有無を直接テストするのは難しいため、判定を純粋関数に
/// 切り出して固定する。
fn should_sleep_after_datagram_batch(batch_len: usize) -> bool {
    batch_len == 0
}

/// 受信した OBJECT_DATAGRAM をメディアとしてどう扱うか
enum DatagramAction<'a> {
    /// 音声トラックの Object。音声の配送経路へ流す
    DeliverAudio {
        /// decode 済みの OBJECT_DATAGRAM ヘッダ (Object Properties を含む)
        datagram: ObjectDatagram,
        /// 圧縮された音声 payload (datagram 全体のうち header より後ろ)
        payload: &'a [u8],
    },
    /// 本 example がメディアとして扱わないトラックの Object (映像 / カタログ)
    Unsupported {
        /// 対象トラックの alias (警告を 1 回に絞るために使う)
        track_alias: u64,
    },
    /// メディアとして扱わない Object (Object Status のみ・未知の alias・wire の書式違反)
    Ignore,
}

/// 受信した datagram をメディアとしてどう扱うかを決める
///
/// `raw` は Session が OBJECT_DATAGRAM として受け入れた datagram 全体である。payload は
/// `ObjectDatagram::decode` が返す header の消費バイト数以降であり、`raw` の一部を指す
/// (datagram は 1 メッセージ = 1 データグラムであるため、残りはすべて payload である)。
///
/// 音声以外をメディアとして扱わないのは、LOC の映像が 1 group = 1 unidirectional stream で
/// 配送されるためである (draft-ietf-moq-loc-04 §4.2)。カタログも stream で届く。
fn plan_datagram<'a>(raw: &'a [u8], track_map: &HashMap<u64, TrackKind>) -> DatagramAction<'a> {
    // Session が同じバイト列を検証済みであるため、ここでの decode 失敗は実装の不整合を表す。
    // メディアを黙って落とさないよう警告する
    let Ok((datagram, header_len)) = ObjectDatagram::decode(raw) else {
        tracing::warn!("Failed to decode an object datagram the session already accepted");
        return DatagramAction::Ignore;
    };
    // Object Status のみの Object は payload を持たない (draft-ietf-moq-transport-22 §11.2.1)
    if datagram.status.is_some() {
        return DatagramAction::Ignore;
    }
    match track_map.get(&datagram.track_alias) {
        Some(TrackKind::Audio) => DatagramAction::DeliverAudio {
            datagram,
            payload: &raw[header_len..],
        },
        Some(TrackKind::Video | TrackKind::Catalog) => DatagramAction::Unsupported {
            track_alias: datagram.track_alias,
        },
        // 未知の alias は Session が Accepted にしないため到達しない
        None => DatagramAction::Ignore,
    }
}

/// 1 本の audio subgroup ストリームから音声パケットを順次デコードする
///
/// LOC draft-ietf-moq-loc-04 §4.1 (Application with one audio track) では「1 audio chunk = 1 Object = 1 Group」と例示されているが、
/// 防御的に subgroup 内の object をすべて処理する。
///
/// 配送 (録画・Audio Config の検証・デコード・フレーム送出) は [`AudioDelivery::deliver`] が
/// 行う。datagram で届く音声も同じ関数を通る。
#[expect(clippy::too_many_arguments)]
async fn decode_audio_stream(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    header: &SubgroupHeader,
    sg_decoder: &mut SubgroupStreamDecoder,
    mut audio_decoder: Option<&mut AudioDecoder>,
    delivery: &AudioDelivery<'_>,
    last_received_locations: &std::sync::Arc<std::sync::Mutex<LastReceivedLocations>>,
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
        // 移行後の再購読で使う「最後に配送した位置」を記録する
        let location = subgroup_object_location(header, &obj);
        record_last_location(last_received_locations, TrackKind::Audio, location);
        if !delivery
            .deliver(
                Some(location),
                obj.properties_bytes.as_deref(),
                &payload,
                audio_decoder.as_deref_mut(),
            )
            .await
        {
            return chunks;
        }
        chunks += 1;
    }
}

/// FETCH 応答 stream をどう扱うか
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FetchStreamAction {
    /// 映像の FETCH 応答。映像の配送経路へ流す
    DeliverVideo,
    /// 音声の FETCH 応答。音声の配送経路へ流す
    DeliverAudio,
    /// catalog 購読の fill fetch 応答。カタログの状態へ適用する
    DeliverCatalog,
    /// 自側が発行した要求への応答ではない。読み捨てる
    Ignore,
}

/// FETCH 応答 stream の Request ID から配送先を決める
///
/// `media_fetch_targets` は自側が発行したメディア FETCH の Request ID と、catalog 購読の
/// Request ID を「Request ID → トラック種別」で記録した対応表である。FILL_PARAMETERS 付き
/// SUBSCRIBE へは publisher が fill fetch stream を返すため
/// (draft-ietf-moq-transport-22 §3.4 (Fill Semantics))、その Request ID も登録して
/// カタログとして適用する。表に無い Request ID の stream は自側の要求と対応しないため
/// 読み捨てる。
fn fetch_stream_action(
    request_id: u64,
    media_fetch_targets: &HashMap<u64, TrackKind>,
) -> FetchStreamAction {
    match media_fetch_targets.get(&request_id) {
        Some(TrackKind::Video) => FetchStreamAction::DeliverVideo,
        Some(TrackKind::Audio) => FetchStreamAction::DeliverAudio,
        Some(TrackKind::Catalog) => FetchStreamAction::DeliverCatalog,
        None => FetchStreamAction::Ignore,
    }
}

/// FETCH 応答 stream から次の entry と Object の payload を読む
///
/// `Ok(None)` は stream の終端 (FIN / RESET_STREAM / Session からの STOP_SENDING 指示) を
/// 表す。`Err` は wire の書式違反、Session への登録失敗、payload を読み切る前の終端を
/// 表し、呼び出し側が読み取りを打ち切る (ログは呼び出し側が出す)。
/// End of Range の entry は payload を持たないため payload は空になる。
async fn read_fetch_entry(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    decoder: &mut FetchStreamDecoder,
) -> Result<Option<(DecodedFetchEntry, Vec<u8>)>> {
    loop {
        let entry = match decoder.try_decode_entry()? {
            Some(entry) => entry,
            None => match receive_registered_stream_data(stream, data_plane, stream_id).await? {
                Some(data) => {
                    decoder.push(&data);
                    continue;
                }
                None => return Ok(None),
            },
        };
        // entry を読むたびに Session の activity を更新する。Object と End of Range の
        // どちらも DATA_STREAM_TIMEOUT の activity に含まれる
        // (draft-ietf-moq-transport-22 §12.2 (Session Termination Codes))。
        // 失敗は読み取りを打ち切る (この stream の会計が進まない)
        data_plane.recv_fetch_entry(stream_id)?;
        let payload = match &entry {
            // payload_length == 0 の Object (Object Status のみ) は payload を持たない
            DecodedFetchEntry::Object(obj) if obj.payload_length > 0 => loop {
                if let Some(payload) = decoder.try_read_payload() {
                    break payload;
                }
                match receive_registered_stream_data(stream, data_plane, stream_id).await? {
                    Some(data) => decoder.push(&data),
                    None => {
                        return Err(Error::Other(
                            "stream closed before fetch payload".to_string(),
                        ));
                    }
                }
            },
            _ => Vec::new(),
        };
        return Ok(Some((entry, payload)));
    }
}

/// FETCH 応答の End of Range を「取得できなかった範囲」としてログに残す
///
/// End of Range は Object ではないため配送しない。3 種は取得できなかった理由が異なる
/// (存在しない / 不明 / 期限切れ) ため、それぞれ別のメッセージで残す
/// (draft-ietf-moq-transport-22 §3.2.2 (Gaps in a Fetch Stream) /
/// §11.4.1.2 (End of Range))。Object の entry では何もしない。
fn log_end_of_range(stream_num: u64, entry: &DecodedFetchEntry) {
    let (reason, group_id, object_id) = match entry {
        DecodedFetchEntry::Object(_) => return,
        DecodedFetchEntry::EndOfNonExistentRange {
            group_id,
            object_id,
        } => ("non-existent", group_id, object_id),
        DecodedFetchEntry::EndOfUnknownRange {
            group_id,
            object_id,
        } => ("unknown", group_id, object_id),
        DecodedFetchEntry::EndOfTimedOutRange {
            group_id,
            object_id,
        } => ("timed-out", group_id, object_id),
    };
    tracing::info!(
        "Stream #{stream_num}: FETCH could not retrieve a {reason} range: \
         group={group_id}, object={object_id}"
    );
}

/// メディア FETCH 応答 stream から映像 Object を読み、live と同じ配送経路へ流す
///
/// `video_decoder` は stream ごとに作る復号器であり、キーフレームから復号を始める判定は
/// live と共有する [`VideoDecodeOrder`] が持つ。FETCH で取得した Group が live で復号中の
/// Group より古い場合、その Object は `Stale` として復号されない (録画は live の録画が
/// 始まる前に届いた分だけが残る)。
#[expect(clippy::too_many_arguments)]
async fn read_video_fetch_stream(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    decoder: &mut FetchStreamDecoder,
    stream_num: u64,
    mut video_decoder: Option<&mut decoder::VideoDecoder>,
    sink: &FrameSink<'_>,
    video_decode_order: &std::sync::Arc<tokio::sync::Mutex<VideoDecodeOrder>>,
    last_received_locations: &std::sync::Arc<std::sync::Mutex<LastReceivedLocations>>,
    recorder: Option<&RecorderSender>,
    termination_tx: &tokio::sync::mpsc::Sender<SessionError>,
) {
    loop {
        let (entry, payload) = match read_fetch_entry(stream, data_plane, stream_id, decoder).await
        {
            Ok(Some(entry)) => entry,
            // FIN / RESET_STREAM / Session からの STOP_SENDING 指示で stream が終わった
            // (live の Subgroup stream と同じ扱い。セッションは閉じない)
            Ok(None) => {
                tracing::debug!("Stream #{stream_num}: video fetch stream ended");
                return;
            }
            Err(e) => {
                log_failure(
                    format_args!("Stream #{stream_num}: failed to read a video fetch entry: {e}"),
                    is_transport_session_end(&e),
                );
                // 読み切れなかった stream を Session の会計から外す (live の Subgroup stream と
                // 同じ扱い)。残すと timeout まで open のままになり、購読の cleanup が進まない
                let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
                return;
            }
        };
        let DecodedFetchEntry::Object(obj) = &entry else {
            log_end_of_range(stream_num, &entry);
            continue;
        };
        // payload を持たない Object (Object Status のみ) は live と同じく配送しない
        if obj.payload_length == 0 {
            continue;
        }
        // 移行後の再購読で使う「最後に配送した位置」を記録する
        record_last_location(
            last_received_locations,
            TrackKind::Video,
            Location {
                group_id: obj.group_id,
                object_id: obj.object_id,
            },
        );
        match deliver_video_object(
            obj.group_id,
            obj.object_id,
            obj.properties_bytes.as_deref(),
            &payload,
            video_decode_order,
            video_decoder.as_deref_mut(),
            sink,
            recorder,
            termination_tx,
        )
        .await
        {
            VideoDelivery::Delivered(_) => {}
            VideoDelivery::Terminated => return,
        }
    }
}

/// メディア FETCH 応答 stream から音声 Object を読み、live と同じ配送経路へ流す
///
/// 配送 (録画・Audio Config の検証・デコード・フレーム送出) は live の Subgroup stream と
/// 同じ [`AudioDelivery::deliver`] が行う。
#[expect(clippy::too_many_arguments)]
async fn read_audio_fetch_stream(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    decoder: &mut FetchStreamDecoder,
    stream_num: u64,
    mut audio_decoder: Option<&mut AudioDecoder>,
    delivery: &AudioDelivery<'_>,
    last_received_locations: &std::sync::Arc<std::sync::Mutex<LastReceivedLocations>>,
) {
    loop {
        let (entry, payload) = match read_fetch_entry(stream, data_plane, stream_id, decoder).await
        {
            Ok(Some(entry)) => entry,
            // FIN / RESET_STREAM / Session からの STOP_SENDING 指示で stream が終わった
            // (live の Subgroup stream と同じ扱い。セッションは閉じない)
            Ok(None) => {
                tracing::debug!("Stream #{stream_num}: audio fetch stream ended");
                return;
            }
            Err(e) => {
                log_failure(
                    format_args!("Stream #{stream_num}: failed to read an audio fetch entry: {e}"),
                    is_transport_session_end(&e),
                );
                // 読み切れなかった stream を Session の会計から外す (live の Subgroup stream と
                // 同じ扱い)。残すと timeout まで open のままになり、購読の cleanup が進まない
                let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
                return;
            }
        };
        let DecodedFetchEntry::Object(obj) = &entry else {
            log_end_of_range(stream_num, &entry);
            continue;
        };
        // payload を持たない Object (Object Status のみ) は live と同じく配送しない
        if obj.payload_length == 0 {
            continue;
        }
        // 移行後の再購読で使う「最後に配送した位置」を記録する
        let location = fetch_object_location(obj);
        record_last_location(last_received_locations, TrackKind::Audio, location);
        if !delivery
            .deliver(
                Some(location),
                obj.properties_bytes.as_deref(),
                &payload,
                audio_decoder.as_deref_mut(),
            )
            .await
        {
            // LOC Properties の書式違反でセッション終了を依頼したか、再生側が終了した
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shiguredo_moqt::stream::encoder::FetchObjectInput;
    use shiguredo_moqt::stream::encoder::FetchStreamEncoder;
    use shiguredo_moqt::stream::subgroup::SubgroupIdMode;
    use shiguredo_moqt::stream::subgroup::SubgroupObject;

    /// GOAWAY の New Session URI から移行先を決める判断を固定する
    ///
    /// 移行の判断そのものは tokio-moq の共通関数が持つ。subscriber 側でも
    /// 「request stream の GOAWAY では移行しない」「空 URI は現在の URL の再利用」
    /// 「解釈できない URI は移行しない」を固定する。
    mod migration_target_tests {
        use tokio_moq::parse_url;

        /// 現在の URL (移行前の接続先)
        fn current_url() -> tokio_moq::ServerUrl {
            parse_url("moqt://relay.example:443/live#msf:moq-example--video")
                .expect("現在の URL をパースできること")
        }

        /// request stream の GOAWAY は当該 request だけの移行対象として返すこと
        #[test]
        fn request_stream_goaway_reports_the_request_scope() {
            let target = tokio_moq::migration_target(
                b"moqt://other.example:443/live",
                Some(3),
                &current_url(),
            )
            .expect("判断に失敗しないこと")
            .expect("移行先があること");
            assert_eq!(
                target.scope,
                tokio_moq::MigrationScope::Request(3),
                "request 単位の移行対象として返すこと"
            );
        }

        /// New Session URI が空のときは現在の URL を再利用すること
        #[test]
        fn empty_uri_reuses_the_current_url() {
            let current = current_url();
            let target = tokio_moq::migration_target(&[], None, &current)
                .expect("判断に失敗しないこと")
                .expect("移行先があること");
            assert_eq!(target.scope, tokio_moq::MigrationScope::Session);
            assert_eq!(target.url.authority, current.authority);
            assert_eq!(target.url.path, current.path);
        }

        /// 解釈できない New Session URI では移行しないこと
        #[test]
        fn invalid_uri_is_rejected() {
            let err =
                tokio_moq::migration_target(b"https://other.example/live", None, &current_url())
                    .expect_err("エラーになること");
            assert!(
                err.to_string().contains("invalid new session URI"),
                "解釈できない URI として報告すること: {err}"
            );
        }
    }

    use shiguredo_moqt::error::SESSION_KEY_VALUE_FORMATTING_ERROR;
    use shiguredo_moqt::loc::LocProperty;
    use shiguredo_moqt::message_parameter::LocationFilterUpdate;
    use shiguredo_opus::{Application, Encoder, EncoderConfig, FrameDuration};

    /// 映像トラックの SUBSCRIBE が新しい Group の開始を要求すること
    ///
    /// `NEW_GROUP_REQUEST=0` は「購読者が Group の情報を持たない」ことを示し、publisher は
    /// できるだけ早く新しい Group を開始する (draft-ietf-moq-transport-22 §9.20.19)。
    /// 起動時 (前の session が無い) は Location Filter を載せない。
    #[test]
    fn video_subscribe_parameters_request_a_new_group() {
        let parameters = video_subscribe_parameters(None);
        assert_eq!(
            parameters.new_group_request(),
            Some(0),
            "購読の確立時に新しい Group の開始を要求すること"
        );
        assert_eq!(
            parameters.len(),
            1,
            "映像トラックの SUBSCRIBE には NEW_GROUP_REQUEST だけを載せること"
        );
        assert!(
            parameters.location_filter_typed().is_ok(),
            "Location Filter の解釈が失敗しないこと"
        );
    }

    /// 移行後の再購読では最後に配送した位置を Absolute Start として載せること
    ///
    /// draft-ietf-moq-transport-22 §3.3.1 (Location Filters) / §9.20.9 (LOCATION FILTER
    /// Parameter) Table 6 の Type 0x02。移行中に publish された Object を取りこぼさない
    /// ために必要である。
    #[test]
    fn video_subscribe_parameters_use_the_last_location_as_absolute_start() {
        let last = Location {
            group_id: 12,
            object_id: 3,
        };
        let parameters = video_subscribe_parameters(Some(last));
        assert_eq!(
            parameters.new_group_request(),
            Some(0),
            "移行後の再購読でも新しい Group の開始を要求すること"
        );
        assert_eq!(
            parameters
                .location_filter_typed()
                .expect("Location Filter を解釈できること"),
            Some(LocationFilter::AbsoluteStart { start: last }),
            "最後に配送した位置を Absolute Start にすること"
        );
    }

    /// 音声の再購読も最後に配送した位置を Absolute Start として載せること
    #[test]
    fn audio_subscribe_parameters_use_the_last_location_as_absolute_start() {
        // 起動時は前の session が無いためパラメータを載せない
        let initial = audio_subscribe_parameters(None);
        assert_eq!(
            initial.len(),
            0,
            "起動時の音声トラックの SUBSCRIBE にはパラメータを載せないこと"
        );
        let last = Location {
            group_id: 7,
            object_id: 0,
        };
        let parameters = audio_subscribe_parameters(Some(last));
        assert_eq!(
            parameters
                .location_filter_typed()
                .expect("Location Filter を解釈できること"),
            Some(LocationFilter::AbsoluteStart { start: last }),
            "最後に配送した位置を Absolute Start にすること"
        );
    }

    /// track ごとの最後の Location は最大値を残すこと
    ///
    /// Group Order が降順の publisher や、FETCH で古い Object が後から届く場合に備え、
    /// 単純な代入ではなく最大値を残す。
    #[test]
    fn last_received_locations_keep_the_maximum_per_track() {
        let mut locations = LastReceivedLocations::default();
        assert_eq!(
            locations.get(TrackKind::Video),
            None,
            "初期値は None であること"
        );

        locations.record(
            TrackKind::Video,
            Location {
                group_id: 3,
                object_id: 5,
            },
        );
        locations.record(
            TrackKind::Video,
            Location {
                group_id: 3,
                object_id: 2,
            },
        );
        assert_eq!(
            locations.get(TrackKind::Video),
            Some(Location {
                group_id: 3,
                object_id: 5,
            }),
            "同じ Group では Object ID の最大値を残すこと"
        );

        locations.record(
            TrackKind::Audio,
            Location {
                group_id: 1,
                object_id: 0,
            },
        );
        assert_eq!(
            locations.get(TrackKind::Audio),
            Some(Location {
                group_id: 1,
                object_id: 0,
            }),
            "track ごとに独立して記録すること"
        );
        assert_eq!(
            locations.get(TrackKind::Video),
            Some(Location {
                group_id: 3,
                object_id: 5,
            }),
            "他の track の記録で上書きしないこと"
        );
        assert_eq!(
            locations.get(TrackKind::Catalog),
            None,
            "記録していない track は None のままであること"
        );
    }

    /// 移行時に音声の購読境界だけを「最後に受信した Location」へ更新すること
    ///
    /// 移行先の購読はその位置を Location Filter の Absolute Start にして張り直すため、
    /// 境界と同じ位置だけが再送される。移行中に抜けた Object (境界より後) は live として
    /// 通り、移行前の音声は再生し直さない。
    ///
    /// 送出済みの TIMESTAMP と直近の Object の記憶は維持し、再送された Object の重複と
    /// TIMESTAMP の巻き戻りを落とす (draft-ietf-moq-transport-22 §3.3.1 (Location Filters)
    /// / §9.20.9 (LOCATION FILTER Parameter) Table 6。draft 由来の仕様であり将来変更
    /// されうる)。
    #[test]
    fn audio_delivery_order_reset_for_migration_updates_only_the_boundary() {
        let mut order = AudioDeliveryOrder::new(Some(Location {
            group_id: 1,
            object_id: 0,
        }));
        // 旧 session で位置 5 の音声を送出済みにしておく
        let last_received = Location {
            group_id: 5,
            object_id: 0,
        };
        assert!(
            !order.is_before_boundary(Some(last_received)),
            "旧 session の境界より後であること"
        );
        assert!(
            !order.is_duplicate(Some(last_received)),
            "初めて見る位置であること"
        );
        assert_eq!(
            order.admit(Some(50_000)),
            AudioDeliveryAdmission::Deliver,
            "旧 session では送出済みであること"
        );
        assert_eq!(order.last_pts_us(), Some(50_000));

        // 移行では最後に受信した位置を新しい境界にする
        order.reset_for_migration(Some(last_received));
        assert_eq!(
            order.boundary(),
            Some(last_received),
            "境界を最後に受信した位置へ更新すること"
        );
        assert_eq!(
            order.last_pts_us(),
            Some(50_000),
            "送出済みの TIMESTAMP を維持すること"
        );
        assert!(
            order.is_duplicate(Some(last_received)),
            "直近に届いた Object の記憶を維持すること"
        );

        // 境界と同じ位置 (Absolute Start の再送) は境界以下として落とす
        assert!(
            order.is_before_boundary(Some(last_received)),
            "境界と同じ位置は再送されても落とすこと"
        );

        // 移行中に抜けた位置は、TIMESTAMP が進んでいれば送出すること
        let gap = Location {
            group_id: 6,
            object_id: 0,
        };
        assert!(
            !order.is_before_boundary(Some(gap)),
            "移行中に抜けた位置は境界より後であること"
        );
        assert_eq!(
            order.admit(Some(60_000)),
            AudioDeliveryAdmission::Deliver,
            "移行中に抜けた音声を送出すること"
        );

        // TIMESTAMP が巻き戻る Object は従来どおり Stale として落とす
        assert_eq!(
            order.admit(Some(55_000)),
            AudioDeliveryAdmission::Stale,
            "送出済みの TIMESTAMP より古い Object は落とすこと"
        );
    }

    /// request stream の GOAWAY が対象とする track を Request ID で判定すること
    #[test]
    fn goaway_request_kind_matches_the_request_id() {
        assert_eq!(
            goaway_request_kind(1, 1, Some(2), Some(3)),
            Some(TrackKind::Catalog),
            "catalog の Request ID を判定できること"
        );
        assert_eq!(
            goaway_request_kind(2, 1, Some(2), Some(3)),
            Some(TrackKind::Video),
            "映像の Request ID を判定できること"
        );
        assert_eq!(
            goaway_request_kind(3, 1, Some(2), Some(3)),
            Some(TrackKind::Audio),
            "音声の Request ID を判定できること"
        );
        assert_eq!(
            goaway_request_kind(4, 1, Some(2), Some(3)),
            None,
            "未知の Request ID は判定できないこと"
        );
        assert_eq!(
            goaway_request_kind(2, 1, None, None),
            None,
            "購読していない track の Request ID は判定できないこと"
        );
    }

    /// 空 URI の request stream GOAWAY だけ現在の session で再発行すること
    ///
    /// `migration_target` は空 URI のとき現在の URL をそのまま返す。URL が同じなら同じ
    /// session で再発行でき、違えば 2 session の併走が必要になるため移行しない。
    #[test]
    fn same_session_url_compares_authority_path_and_fragment() {
        let current = tokio_moq::parse_url("moqt://relay.example:443/live#msf:moq-example--video")
            .expect("現在の URL をパースできること");
        let reused = tokio_moq::migration_target(&[], Some(1), &current)
            .expect("判断に失敗しないこと")
            .expect("移行先があること");
        assert!(
            same_session_url(&reused.url, &current),
            "空 URI は現在の URL と同じであること"
        );

        let other_path =
            tokio_moq::parse_url("moqt://relay.example:443/other#msf:moq-example--video")
                .expect("別 path の URL をパースできること");
        assert!(
            !same_session_url(&other_path, &current),
            "path が違えば別の session であること"
        );
        let other_authority =
            tokio_moq::parse_url("moqt://relay.example:444/live#msf:moq-example--video")
                .expect("別ポートの URL をパースできること");
        assert!(
            !same_session_url(&other_authority, &current),
            "authority が違えば別の session であること"
        );
    }

    /// 移行の開始判定が二重移行と連鎖を防ぐこと
    ///
    /// 移行中に 2 回目の GOAWAY が届いても二重移行せず、時間窓内の移行回数が上限に達したら
    /// 移行しない (接続の再試行ストームへの防御)。移行中は連鎖防止の記録も消費しない。
    #[test]
    fn begin_migration_prevents_duplicate_and_chained_migrations() {
        let now = Instant::now();
        let mut limiter = tokio_moq::MigrationLimiter::new(std::time::Duration::from_secs(600), 1);
        // 移行中は記録を消費しない
        assert_eq!(
            begin_migration(true, &mut limiter, now),
            MigrationStart::AlreadyInProgress,
            "移行中は二重移行しないこと"
        );
        assert_eq!(
            begin_migration(false, &mut limiter, now),
            MigrationStart::Start,
            "移行中に消費した記録が無いため開始できること"
        );
        // 同じ時間窓では上限に達している
        assert_eq!(
            begin_migration(false, &mut limiter, now),
            MigrationStart::RateLimited,
            "上限に達したら移行しないこと"
        );
        // 時間窓を過ぎれば再び移行できる
        assert_eq!(
            begin_migration(
                false,
                &mut limiter,
                now + std::time::Duration::from_secs(601)
            ),
            MigrationStart::Start,
            "時間窓を過ぎたら移行できること"
        );
    }

    /// 移行の再試行は期限が残っている間だけ続けること
    ///
    /// 試行回数の上限は設けず、移行期限 (`MigrationDeadline`) で打ち切る。
    #[test]
    fn should_retry_migration_uses_the_deadline() {
        let base = Instant::now();
        let deadline = MigrationDeadline::after_goaway(base, 1000);
        assert!(
            should_retry_migration(deadline, base + std::time::Duration::from_millis(500)),
            "期限内なら再試行すること"
        );
        assert!(
            should_retry_migration(deadline, base + std::time::Duration::from_millis(999)),
            "期限の直前までは再試行すること"
        );
        assert!(
            !should_retry_migration(deadline, base + std::time::Duration::from_millis(1000)),
            "期限に達したら再試行しないこと"
        );
        assert!(
            !should_retry_migration(deadline, base + std::time::Duration::from_secs(2)),
            "期限を過ぎたら再試行しないこと"
        );
    }

    /// 移行の再試行の待ち時間は指数バックオフし、上限と残り時間で cap されること
    ///
    /// タイトループを避けるため、待ち時間は必ず正になる (残り時間がある場合)。
    #[test]
    fn migration_retry_delay_backs_off_and_is_capped() {
        let enough = std::time::Duration::from_secs(10);
        assert_eq!(
            migration_retry_delay(0, enough),
            std::time::Duration::from_millis(200),
            "初回は 200ms 待つこと"
        );
        assert_eq!(
            migration_retry_delay(1, enough),
            std::time::Duration::from_millis(400),
            "2 回目は倍の 400ms 待つこと"
        );
        assert_eq!(
            migration_retry_delay(2, enough),
            std::time::Duration::from_millis(800),
            "3 回目は倍の 800ms 待つこと"
        );
        assert_eq!(
            migration_retry_delay(3, enough),
            std::time::Duration::from_millis(1000),
            "上限の 1 秒で頭打ちにすること"
        );
        assert_eq!(
            migration_retry_delay(u32::MAX, enough),
            std::time::Duration::from_millis(1000),
            "試行回数が大きくてもオーバーフローせず上限で頭打ちにすること"
        );
        assert_eq!(
            migration_retry_delay(0, std::time::Duration::from_millis(50)),
            std::time::Duration::from_millis(50),
            "残り時間が短ければ残り時間を上限にすること"
        );
        assert_eq!(
            migration_retry_delay(10, std::time::Duration::from_millis(300)),
            std::time::Duration::from_millis(300),
            "バックオフより残り時間が短ければ残り時間を返すこと"
        );
        assert_eq!(
            migration_retry_delay(0, std::time::Duration::ZERO),
            std::time::Duration::ZERO,
            "残り時間が無ければ待たないこと"
        );
    }

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

    /// テスト用: render group を宣言した track を作る
    fn track_in_render_group(render_group: Option<u64>, target_latency: Option<u64>) -> MsfTrack {
        let mut track = track_with_target_latency(target_latency);
        track.render_group = render_group;
        track
    }

    /// テスト用: 購読する track の情報を作る
    fn subscribed_track(
        render_group: Option<u64>,
        target_latency: Option<u64>,
    ) -> SelectedTrackLatency {
        SelectedTrackLatency {
            render_group,
            target_latency,
        }
    }

    /// テスト用の namespace を §8.8 表現から解決する
    fn test_namespace(value: &str) -> TrackNamespace {
        parse_namespace(value).expect("namespace を解決できること")
    }

    /// テスト用の track (codec と namespace だけを設定する)
    fn track_with_namespace(codec: &str, namespace: Option<&str>) -> MsfTrack {
        let mut track = MsfTrack::new(
            "track".to_string(),
            shiguredo_moqt::msf::MsfPackaging::Loc,
            true,
        );
        track.codec = Some(codec.to_string());
        track.namespace = namespace.map(|value| value.to_string());
        track
    }

    /// namespace を宣言した track は宣言された namespace を購読に使う
    ///
    /// draft-ietf-moq-msf-01 §5.2.2 (Track namespace) は track が宣言した namespace が継承した
    /// namespace を上書きすると定める。MOQT の購読は Full Track Name に対して行うため
    /// (draft-ietf-moq-transport-22 §2.4.1 (Track Naming))、catalog track の namespace を使うと
    /// 別の track を購読するか DOES_NOT_EXIST になる。
    #[test]
    fn video_track_info_uses_declared_namespace() {
        let mut track = track_with_namespace("av01.0.04M.08", Some("moq-example-video"));
        track.width = Some(1920);
        track.height = Some(1080);
        let info = extract_video_info(&track, &test_namespace("moq-example"))
            .expect("トラック情報を取り出せること");
        assert_eq!(
            info.namespace,
            test_namespace("moq-example-video"),
            "宣言された namespace を使うこと"
        );
    }

    /// namespace を宣言しない track は catalog track の namespace を継承する
    ///
    /// draft-ietf-moq-msf-01 §5.2.2 (Track namespace) は宣言が無い場合に catalog track の
    /// namespace を継承する MUST を定める。
    #[test]
    fn video_track_info_inherits_catalog_namespace() {
        let mut track = track_with_namespace("av01.0.04M.08", None);
        track.width = Some(1920);
        track.height = Some(1080);
        let info = extract_video_info(&track, &test_namespace("moq-example"))
            .expect("トラック情報を取り出せること");
        assert_eq!(
            info.namespace,
            test_namespace("moq-example"),
            "catalog track の namespace を継承すること"
        );
    }

    /// 解決できない namespace を宣言した track はエラーにする
    ///
    /// カタログは publisher の契約であり、本 example は §8.8 (Representing Namespace and Track
    /// Names) 表現を前提とするため、解決できない namespace を publisher のバグとして扱う。
    /// 黙って catalog track の namespace を継承すると別の track を購読して気づかない誤動作になる。
    #[test]
    fn video_track_info_with_invalid_namespace_is_an_error() {
        let mut track = track_with_namespace("av01.0.04M.08", Some("moq-example--video"));
        track.width = Some(1920);
        track.height = Some(1080);
        let error = extract_video_info(&track, &test_namespace("moq-example"))
            .err()
            .expect("解決できない namespace はエラーになること");
        assert!(
            error.to_string().contains("invalid namespace"),
            "namespace の解決失敗として分かること: {error}"
        );
    }

    /// 空の namespace を宣言した映像 track はエラーにする
    ///
    /// `parse_namespace` は空文字を 0 フィールドの namespace として通すが、本 example は
    /// 1 つ以上のフィールドを持つ namespace を扱う (`tokio_moq::resolve_namespace` と同じ前提)。
    #[test]
    fn video_track_info_with_empty_namespace_is_an_error() {
        let mut track = track_with_namespace("av01.0.04M.08", Some(""));
        track.width = Some(1920);
        track.height = Some(1080);
        let error = extract_video_info(&track, &test_namespace("moq-example"))
            .err()
            .expect("空の namespace はエラーになること");
        assert!(
            error.to_string().contains("empty namespace"),
            "空の namespace として分かること: {error}"
        );
    }

    /// 空の namespace を宣言した音声 track もエラーにする (映像と同じ扱い)
    #[test]
    fn audio_track_info_with_empty_namespace_is_an_error() {
        let mut track = track_with_namespace("opus", Some(""));
        track.samplerate = Some(48000);
        track.channel_config = Some("2".to_string());
        let error = extract_audio_info(&track, &test_namespace("moq-example"))
            .err()
            .expect("空の namespace はエラーになること");
        assert!(
            error.to_string().contains("empty namespace"),
            "空の namespace として分かること: {error}"
        );
    }

    /// 音声 track でも宣言された namespace を購読に使う
    #[test]
    fn audio_track_info_uses_declared_namespace() {
        let mut track = track_with_namespace("opus", Some("moq-example-audio"));
        track.samplerate = Some(48000);
        track.channel_config = Some("2".to_string());
        let info = extract_audio_info(&track, &test_namespace("moq-example"))
            .expect("トラック情報を取り出せること");
        assert_eq!(
            info.namespace,
            test_namespace("moq-example-audio"),
            "宣言された namespace を使うこと"
        );
    }

    /// 音声 track でも catalog track の namespace を継承する
    #[test]
    fn audio_track_info_inherits_catalog_namespace() {
        let mut track = track_with_namespace("opus", None);
        track.samplerate = Some(48000);
        track.channel_config = Some("2".to_string());
        let info = extract_audio_info(&track, &test_namespace("moq-example"))
            .expect("トラック情報を取り出せること");
        assert_eq!(
            info.namespace,
            test_namespace("moq-example"),
            "catalog track の namespace を継承すること"
        );
    }

    /// 解決できない namespace を宣言した音声 track もエラーにする (映像と同じ扱い)
    #[test]
    fn audio_track_info_with_invalid_namespace_is_an_error() {
        let mut track = track_with_namespace("opus", Some("moq-example--audio"));
        track.samplerate = Some(48000);
        track.channel_config = Some("2".to_string());
        let error = extract_audio_info(&track, &test_namespace("moq-example"))
            .err()
            .expect("解決できない namespace はエラーになること");
        assert!(
            error.to_string().contains("invalid namespace"),
            "namespace の解決失敗として分かること: {error}"
        );
    }

    // カタログの targetLatency をそのまま読む
    #[test]
    fn catalog_target_latency_reads_the_value() {
        let tracks = [
            track_with_target_latency(Some(200)),
            track_with_target_latency(Some(200)),
        ];
        assert_eq!(
            catalog_target_latency_ms(&tracks, &[subscribed_track(None, None)]),
            Some(200),
            "render group を宣言していない track だけのときは全 track から値を読む"
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
            catalog_target_latency_ms(&tracks, &[subscribed_track(None, None)]),
            Some(150),
            "値を持つ track だけを見る"
        );
    }

    // どの track も targetLatency を持たなければ None になる
    #[test]
    fn catalog_target_latency_is_none_without_the_value() {
        let tracks = [track_with_target_latency(None)];
        assert_eq!(
            catalog_target_latency_ms(&tracks, &[subscribed_track(None, None)]),
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
            catalog_target_latency_ms(&tracks, &[subscribed_track(None, None)]),
            Some(300),
            "食い違うときは大きい方を採る"
        );
    }

    // i64 に収まらない値は targetLatency として扱わない
    #[test]
    fn catalog_target_latency_ignores_values_out_of_range() {
        let tracks = [track_with_target_latency(Some(u64::MAX))];
        assert_eq!(
            catalog_target_latency_ms(&tracks, &[subscribed_track(None, None)]),
            None,
            "i64 に収まらない値は無いものとして扱う"
        );
    }

    // 同じ render group の別の track が宣言した値を採る (購読 track 自身は値を持たなくてもよい)
    #[test]
    fn catalog_target_latency_reads_the_value_of_another_track_in_the_group() {
        let tracks = [
            // 購読する映像 (group 1 を宣言し targetLatency は省略)
            track_in_render_group(Some(1), None),
            // 同じ group の別の track (購読しない音声)
            track_in_render_group(Some(1), Some(500)),
        ];
        assert_eq!(
            catalog_target_latency_ms(&tracks, &[subscribed_track(Some(1), None)]),
            Some(500),
            "同じ render group の track が宣言した値を採ること"
        );
    }

    // 購読 track が group を宣言しているときは、購読しない group の値に引っ張られない
    #[test]
    fn catalog_target_latency_ignores_unsubscribed_render_groups() {
        let tracks = [
            track_in_render_group(Some(1), Some(100)),
            track_in_render_group(None, Some(200)),
            // 購読しない group 9 のより大きい値
            track_in_render_group(Some(9), Some(900)),
        ];
        assert_eq!(
            catalog_target_latency_ms(
                &tracks,
                &[
                    subscribed_track(Some(1), Some(100)),
                    subscribed_track(None, Some(200)),
                ],
            ),
            Some(200),
            "購読する track が group を宣言しているときは購読しない group の値を見ないこと"
        );
    }

    // 購読しない track は対象から外れる
    #[test]
    fn subscribed_track_latencies_excludes_tracks_not_subscribed() {
        let video = subscribed_track(Some(1), Some(100));
        let audio = subscribed_track(Some(2), Some(500));
        let only_video = subscribed_track_latencies(Some(video), Some(audio), true, false);
        assert_eq!(
            only_video.len(),
            1,
            "音声を購読しないときは音声の情報を渡さないこと"
        );
        assert_eq!(
            only_video[0].render_group,
            Some(1),
            "購読する映像の情報だけを渡すこと"
        );
        let only_audio = subscribed_track_latencies(Some(video), Some(audio), false, true);
        assert_eq!(
            only_audio.len(),
            1,
            "映像を購読しないときは映像の情報を渡さないこと"
        );
        assert_eq!(
            only_audio[0].render_group,
            Some(2),
            "購読する音声の情報だけを渡すこと"
        );
    }

    // 購読する track の render group だけが対象になる (購読しない track の group の値は採らない)
    #[test]
    fn catalog_target_latency_uses_the_subscribed_render_group() {
        let tracks = [
            // 購読する映像 (group 1)
            track_in_render_group(Some(1), Some(100)),
            // --no-audio で購読しない音声 (group 2)
            track_in_render_group(Some(2), Some(500)),
        ];
        assert_eq!(
            catalog_target_latency_ms(&tracks, &[subscribed_track(Some(1), Some(100))]),
            Some(100),
            "購読しない render group の値は採らないこと"
        );
    }

    // 購読する track が複数の render group にまたがるときは大きい方を採る
    #[test]
    fn catalog_target_latency_takes_the_larger_value_across_subscribed_groups() {
        let tracks = [
            track_in_render_group(Some(1), Some(100)),
            track_in_render_group(Some(3), Some(300)),
        ];
        assert_eq!(
            catalog_target_latency_ms(
                &tracks,
                &[
                    subscribed_track(Some(1), Some(100)),
                    subscribed_track(Some(3), Some(300)),
                ],
            ),
            Some(300),
            "時間軸は映像と音声で 1 つの値を使うため大きい方を採ること"
        );
    }

    // 片方だけが render group を宣言しているとき、宣言していない側の値も対象に残る
    #[test]
    fn catalog_target_latency_keeps_the_value_of_a_track_without_render_group() {
        let tracks = [
            track_in_render_group(Some(1), None),
            track_in_render_group(None, Some(200)),
        ];
        assert_eq!(
            catalog_target_latency_ms(
                &tracks,
                &[
                    subscribed_track(Some(1), None),
                    subscribed_track(None, Some(200)),
                ],
            ),
            Some(200),
            "render group を宣言していない購読 track の値を落とさないこと"
        );
    }

    // 購読する track が 1 つも render group を宣言していないときは全 track を見る (従来の挙動)
    #[test]
    fn catalog_target_latency_falls_back_to_all_tracks_without_render_group() {
        let tracks = [
            track_in_render_group(None, Some(100)),
            track_in_render_group(Some(2), Some(900)),
        ];
        assert_eq!(
            catalog_target_latency_ms(&tracks, &[subscribed_track(None, Some(100))]),
            Some(900),
            "group が分からないときは安全側に倒して全 track の最大値を採ること"
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

    // 壁時計の TIMESTAMP (Timescale が無い) では、到着と TIMESTAMP の差を入力の遅れとする
    #[test]
    fn receive_input_lag_ms_measures_the_wall_clock_timestamp() {
        assert_eq!(
            receive_input_lag_ms(Some(1_000_000), None, 1_250_000, 1_000_000),
            Some(250),
            "到着の壁時計から TIMESTAMP を引いた値をミリ秒で返す"
        );
        // Timescale が 0 のときも Timescale が無いものとして扱う (loc_timestamp_us と同じ規則)
        assert_eq!(
            receive_input_lag_ms(Some(1_000_000), Some(0), 1_250_000, 1_000_000),
            Some(250),
            "Timescale が 0 なら壁時計として測る"
        );
        // 到着が TIMESTAMP より前の音 (配信側の時計が先へ進んでいる) は負になる
        assert_eq!(
            receive_input_lag_ms(Some(2_000_000), None, 1_000_000, 2_000_000),
            Some(-1_000),
            "到着のほうが早いときは負の遅れになる"
        );
    }

    // TIMESTAMP が無い音と、Timescale がある (メディア時刻である) 音では測れない
    #[test]
    fn receive_input_lag_ms_returns_none_when_the_timestamp_is_not_a_wall_clock() {
        assert_eq!(
            receive_input_lag_ms(None, None, 1_250_000, 0),
            None,
            "TIMESTAMP が無い音では測れない"
        );
        assert_eq!(
            receive_input_lag_ms(None, Some(48_000), 1_250_000, 0),
            None,
            "TIMESTAMP が無い音では Timescale があっても測れない"
        );
        assert_eq!(
            receive_input_lag_ms(Some(48_000), Some(48_000), 1_250_000, 1_000_000),
            None,
            "Timescale がある音はメディア時刻であり壁時計と対応しないため測れない"
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

    /// Largest Object が無い場合は FETCH の range を決めないこと
    ///
    /// SUBSCRIBE_OK が LARGEST_OBJECT を運ばない場合、購読より前に publish された Object の
    /// 存在を知り得ないため FETCH しない。
    #[test]
    fn plan_fetch_range_requires_the_largest_object() {
        assert_eq!(
            plan_fetch_range(None, FETCH_RECENT_GROUPS),
            None,
            "Largest Object が無ければ FETCH しないこと"
        );
    }

    /// Largest Object の Group が遡る Group 数に満たない場合は Group 0 で飽和させること
    #[test]
    fn plan_fetch_range_saturates_at_group_zero() {
        // Group 1 から 3 個前を求めると負になるため、Group 0 に飽和させる
        let (start, end) = plan_fetch_range(
            Some(Location {
                group_id: 1,
                object_id: 5,
            }),
            FETCH_RECENT_GROUPS,
        )
        .expect("range を決められること");
        assert_eq!(
            start,
            Location {
                group_id: 0,
                object_id: 0
            },
            "開始は Group 0 の先頭にすること"
        );
        assert_eq!(
            end,
            Location {
                group_id: 1,
                object_id: 5
            },
            "終端は Largest Object にすること"
        );
    }

    /// Largest Object を含む直近の Group を要求すること
    #[test]
    fn plan_fetch_range_covers_the_recent_groups() {
        let (start, end) = plan_fetch_range(
            Some(Location {
                group_id: 10,
                object_id: 7,
            }),
            FETCH_RECENT_GROUPS,
        )
        .expect("range を決められること");
        // Largest Object の Group 10 から 4 Group (10, 9, 8, 7) を要求する
        assert_eq!(
            start,
            Location {
                group_id: 7,
                object_id: 0
            },
            "開始は直近 4 Group の先頭にすること"
        );
        assert_eq!(
            end,
            Location {
                group_id: 10,
                object_id: 7
            },
            "終端は Largest Object にすること"
        );
    }

    /// 遡る Group 数が 1 以下でも underflow しないこと
    ///
    /// `FETCH_RECENT_GROUPS` は 4 固定だが、純関数として 0 を渡されても
    /// Group ID を巻き戻さない (直近の Group だけを要求する)。
    #[test]
    fn plan_fetch_range_never_underflows() {
        for groups in [0, 1] {
            let (start, end) = plan_fetch_range(
                Some(Location {
                    group_id: 3,
                    object_id: 2,
                }),
                groups,
            )
            .expect("range を決められること");
            assert_eq!(
                start,
                Location {
                    group_id: 3,
                    object_id: 0
                },
                "groups={groups} でも Largest Object の Group の先頭からにすること"
            );
            assert_eq!(
                end,
                Location {
                    group_id: 3,
                    object_id: 2
                },
                "終端は Largest Object にすること"
            );
        }
    }

    /// FETCH の LOCATION_FILTER が要求 range を Absolute Range で指定すること
    ///
    /// draft-ietf-moq-transport-22 §9.20.9 Table 6 の Type 0x04 は Start を絶対 Location、
    /// End を StartGroup からの差分 (EndGroupDelta) と EndObject で表す。
    #[test]
    fn fetch_range_parameters_request_an_absolute_range() {
        let start = Location {
            group_id: 7,
            object_id: 0,
        };
        let end = Location {
            group_id: 10,
            object_id: 7,
        };
        let parameters = fetch_range_parameters(start, end);
        assert_eq!(
            parameters
                .location_filter_typed()
                .expect("LOCATION_FILTER を読めること"),
            Some(LocationFilter::AbsoluteRangeWithEnd {
                start,
                end_group_delta: 3,
                end_object: 7,
            }),
            "要求 range を Absolute Range で指定すること"
        );
        assert_eq!(
            parameters.len(),
            1,
            "FETCH には LOCATION_FILTER だけを載せること"
        );
    }

    /// FETCH 応答 stream を映像 / 音声 / カタログへ振り分けること
    #[test]
    fn fetch_stream_action_routes_media_fetches() {
        let mut media_fetch_targets = HashMap::new();
        media_fetch_targets.insert(10, TrackKind::Video);
        media_fetch_targets.insert(12, TrackKind::Audio);
        assert_eq!(
            fetch_stream_action(10, &media_fetch_targets),
            FetchStreamAction::DeliverVideo,
            "映像の FETCH 応答は映像の配送経路へ流すこと"
        );
        assert_eq!(
            fetch_stream_action(12, &media_fetch_targets),
            FetchStreamAction::DeliverAudio,
            "音声の FETCH 応答は音声の配送経路へ流すこと"
        );
    }

    /// catalog 購読の fill fetch stream はカタログの配送経路へ流すこと
    ///
    /// 移行後に catalog を張り直すと、FILL_PARAMETERS 付き SUBSCRIBE への fill fetch stream
    /// (draft-ietf-moq-transport-22 §3.4 (Fill Semantics)。draft 由来の仕様であり将来変更
    /// されうる) で完全な catalog が届く。これを読み捨てると移行中に更新された catalog が
    /// 反映されないため、カタログとして適用する。
    #[test]
    fn fetch_stream_action_delivers_the_catalog_fill() {
        let mut media_fetch_targets = HashMap::new();
        media_fetch_targets.insert(11, TrackKind::Catalog);
        assert_eq!(
            fetch_stream_action(11, &media_fetch_targets),
            FetchStreamAction::DeliverCatalog,
            "catalog 購読の fill fetch 応答はカタログへ流すこと"
        );
    }

    /// 自側が発行していない FETCH の応答 stream は読み捨てること
    ///
    /// 対応表に無い Request ID の stream は自側の要求と対応しない。
    #[test]
    fn fetch_stream_action_ignores_unknown_request_ids() {
        let mut media_fetch_targets = HashMap::new();
        media_fetch_targets.insert(10, TrackKind::Video);
        media_fetch_targets.insert(11, TrackKind::Catalog);
        assert_eq!(
            fetch_stream_action(0, &media_fetch_targets),
            FetchStreamAction::Ignore,
            "対応表に無い Request ID の stream は読み捨てること"
        );
    }

    /// テスト用: Opus の 20 ms フレームを 1 つエンコードする
    ///
    /// 入力は無音 (全 0 サンプル) とする。エンコーダは本物 (shiguredo_opus) を使う。
    fn encode_opus_silence(sample_rate: u32, channels: u8) -> Vec<u8> {
        let mut config = EncoderConfig::new(sample_rate, channels);
        config.bitrate = Some(64_000);
        config.application = Some(Application::Audio);
        config.frame_duration = Some(FrameDuration::Ms20);
        let mut encoder = Encoder::new(config).expect("Opus エンコーダを生成できること");
        let pcm = vec![0i16; encoder.frame_samples() * usize::from(channels)];
        encoder
            .encode(&pcm)
            .expect("無音の Opus フレームをエンコードできること")
    }

    /// テスト用: RFC 7845 §5.1 (Identification Header) の OpusHead を組み立てる
    ///
    /// publisher が Audio Config に載せる値と同じ形式である。
    fn opus_head(sample_rate: u32, channels: u8) -> Vec<u8> {
        let mut head = Vec::new();
        head.extend_from_slice(b"OpusHead");
        head.push(1);
        head.push(channels);
        head.extend_from_slice(&0u16.to_le_bytes());
        head.extend_from_slice(&sample_rate.to_le_bytes());
        head.extend_from_slice(&0u16.to_le_bytes());
        head.push(0);
        head
    }

    /// テスト用: 音声 Object の LOC Properties を組み立てる
    ///
    /// publisher は 1 audio chunk = 1 Object = 1 Group の datagram に Timestamp / Timescale を
    /// 載せ、最初の Object にだけ Audio Config (OpusHead) を載せる
    /// (draft-ietf-moq-loc-04 §4.1)。
    fn audio_loc_properties(
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

    /// テスト用: Timescale を載せない LOC プロパティ (壁時計の TIMESTAMP) を組み立てる
    ///
    /// Timescale が無いときの Timestamp は Unix epoch からのマイクロ秒である
    /// (draft-ietf-moq-loc-04 §2.3.1.1)。受信の入口の入力の遅れを測れる音はこの形である。
    fn audio_loc_properties_without_timescale(
        timestamp: u64,
        audio_config: Option<&[u8]>,
    ) -> LocProperties {
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(timestamp),
        });
        if let Some(config) = audio_config {
            props.push(LocProperty {
                prop_id: PROP_AUDIO_CONFIG,
                value: LocPropertyValue::Bytes(config.to_vec()),
            });
        }
        props
    }

    /// テスト用: OBJECT_DATAGRAM のワイヤ表現を組み立てる
    ///
    /// 位置は Group ID と Object ID で指定する (取り違えを検出できるようにするため)。
    fn object_datagram_wire(
        track_alias: u64,
        group_id: u64,
        object_id: u64,
        properties: Option<&LocProperties>,
        payload: &[u8],
        status: Option<u64>,
    ) -> Vec<u8> {
        let datagram = ObjectDatagram {
            track_alias,
            group_id,
            object_id,
            publisher_priority: Some(128),
            properties_data: properties
                .map(|props| props.encode().expect("LOC プロパティを encode できること")),
            end_of_group: false,
            status,
        };
        let mut raw = datagram
            .encode()
            .expect("OBJECT_DATAGRAM を encode できること");
        raw.extend_from_slice(payload);
        raw
    }

    /// テスト用: 音声 Object を配送する `AudioDelivery` を作る
    ///
    /// catalog は 48 kHz / 1ch の Opus を宣言しているものとする。`recorder` が `None` のときは
    /// 録画しない。`delivery_order` は購読で共有する送出順の状態であり、呼び出し側が
    /// `AudioDelivery` より長く保持する。
    fn test_audio_delivery<'a>(
        audio_tx: &'a std::sync::mpsc::Sender<DecodedAudioFrame>,
        audio_config_handled: &'a std::sync::atomic::AtomicBool,
        termination_tx: &'a tokio::sync::mpsc::Sender<SessionError>,
        catch_up: &'a std::sync::Mutex<AudioReceiveCatchUp>,
        delivery_order: &'a std::sync::Mutex<AudioDeliveryOrder>,
        recorder: Option<&'a RecorderSender>,
    ) -> AudioDelivery<'a> {
        AudioDelivery {
            audio_tx,
            audio_config_handled,
            is_opus_codec: true,
            catalog_sample_rate: 48_000,
            catalog_channels: 1,
            recorder,
            termination_tx,
            catch_up,
            delivery_order,
        }
    }

    /// datagram で届いた音声 Object が subgroup stream と同じ経路でデコードされること
    ///
    /// moq-pub は `--audio-datagram` で 1 audio chunk = 1 Object = 1 Group の datagram を送る
    /// (draft-ietf-moq-loc-04 §4.1)。購読側は payload を decode してデコード済みフレームを
    /// 送出する必要がある。
    #[tokio::test]
    async fn audio_datagram_object_is_decoded_into_a_frame() {
        const AUDIO_ALIAS: u64 = 7;

        let packet = encode_opus_silence(48_000, 1);
        let properties = audio_loc_properties(960, 48_000, Some(&opus_head(48_000, 1)));
        let raw = object_datagram_wire(AUDIO_ALIAS, 0, 0, Some(&properties), &packet, None);

        // 購読では audio track の alias が Audio として登録される
        let mut track_map = HashMap::new();
        track_map.insert(AUDIO_ALIAS, TrackKind::Audio);
        let DatagramAction::DeliverAudio { datagram, payload } = plan_datagram(&raw, &track_map)
        else {
            panic!("音声トラックの datagram は配送対象になること");
        };
        assert_eq!(
            payload,
            packet.as_slice(),
            "header の消費バイト数より後ろが payload であること"
        );

        let (audio_tx, audio_rx) = std::sync::mpsc::channel();
        let audio_config_handled = std::sync::atomic::AtomicBool::new(false);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_handled,
            &termination_tx,
            &catch_up,
            &delivery_order,
            None,
        );
        let mut decoder =
            AudioDecoder::build("opus", 48_000, 1).expect("Opus デコーダを生成できること");

        let deliver = delivery
            .deliver(
                Some(Location {
                    group_id: datagram.group_id,
                    object_id: datagram.object_id,
                }),
                datagram.properties_data.as_deref(),
                payload,
                Some(&mut decoder),
            )
            .await;
        assert!(deliver, "デコードに成功したときは配送を継続すること");

        let frame = audio_rx
            .try_recv()
            .expect("デコード済みの音声フレームが届くこと");
        assert_eq!(
            frame.sample_rate, 48_000,
            "サンプルレートは catalog の値であること"
        );
        assert_eq!(frame.channels, 1, "チャンネル数は catalog の値であること");
        assert!(!frame.pcm.is_empty(), "PCM が空でないこと");
        // LOC の Timestamp 960 / Timescale 48000 は 20 ms である
        assert_eq!(
            frame.pts_us, 20_000,
            "LOC の Timestamp がマイクロ秒へ換算されること"
        );
        // 復号へ渡す直前の到着時刻は、復号の出力が揃った時刻以前である
        assert!(
            frame.arrival_us <= frame.decoded_us,
            "復号へ渡す直前の到着時刻は復号の完了時刻以前であること: \
             arrival_us={}, decoded_us={}",
            frame.arrival_us,
            frame.decoded_us,
        );
        // どちらも受信側の壁時計 (Unix epoch マイクロ秒) であり、このテストの実行時刻を
        // 大きく超えない
        let now_us = crate::wall_clock_us();
        assert!(
            frame.arrival_us > 0 && frame.decoded_us <= now_us,
            "到着時刻と復号の完了時刻は受信側の壁時計であること: \
             arrival_us={}, decoded_us={}, now_us={}",
            frame.arrival_us,
            frame.decoded_us,
            now_us,
        );
        assert!(
            audio_config_handled.load(Ordering::Acquire),
            "Audio Config を処理済みとして記録すること"
        );
        assert!(
            termination_rx.try_recv().is_err(),
            "書式違反が無いためセッション終了は依頼されないこと"
        );
    }

    /// Object Status のみの Object と未知の alias はメディアとして扱わないこと
    ///
    /// Object Status のみの Object は payload を持たず (draft-ietf-moq-transport-22 §11.2.1)、
    /// 未知の alias は Session が Accepted にしない。どちらも payload を decode しない。
    #[test]
    fn plan_datagram_ignores_objects_without_media() {
        const AUDIO_ALIAS: u64 = 7;

        let mut track_map = HashMap::new();
        track_map.insert(AUDIO_ALIAS, TrackKind::Audio);

        // Object Status のみ (payload なし) の Object
        let status_only = object_datagram_wire(AUDIO_ALIAS, 0, 0, None, &[], Some(0x0));
        assert!(
            matches!(
                plan_datagram(&status_only, &track_map),
                DatagramAction::Ignore
            ),
            "Object Status のみの Object は配送しないこと"
        );

        // 購読していない alias
        let properties = audio_loc_properties(960, 48_000, None);
        let unknown_alias =
            object_datagram_wire(AUDIO_ALIAS + 1, 0, 0, Some(&properties), b"payload", None);
        assert!(
            matches!(
                plan_datagram(&unknown_alias, &track_map),
                DatagramAction::Ignore
            ),
            "未知の alias は配送しないこと"
        );
    }

    /// 映像とカタログの datagram は非対応のトラックとして報告されること
    ///
    /// LOC の映像は 1 group = 1 unidirectional stream で配送される
    /// (draft-ietf-moq-loc-04 §4.2)。moq-sub は datagram の映像 / カタログを扱わない。
    #[test]
    fn plan_datagram_reports_tracks_this_subscriber_does_not_handle() {
        let mut track_map = HashMap::new();
        track_map.insert(1, TrackKind::Video);
        track_map.insert(2, TrackKind::Catalog);

        let properties = audio_loc_properties(960, 48_000, None);
        for alias in [1u64, 2u64] {
            let raw = object_datagram_wire(alias, 0, 0, Some(&properties), b"payload", None);
            assert!(
                matches!(
                    plan_datagram(&raw, &track_map),
                    DatagramAction::Unsupported { track_alias } if track_alias == alias
                ),
                "映像 / カタログの datagram は非対応として報告すること: alias={alias}"
            );
        }
    }

    /// datagram 受信ループは空バッチのときだけ休むこと
    ///
    /// `recv_datagrams` は QUIC と QMux では 1 件以上届くまで待つため、バッチがあるときに
    /// sleep するとそのまま受信から再生までの遅延になる。WebTransport の
    /// `take_buffered_datagrams` はバッファが空でも即座に返りうるため、空のときだけ sleep して
    /// 空回りを防ぐ。
    #[test]
    fn datagram_receive_loop_sleeps_only_for_an_empty_batch() {
        assert!(
            should_sleep_after_datagram_batch(0),
            "空のバッチでは sleep して空回りを防ぐこと"
        );
        for batch_len in [1, 2, 10, 1_000] {
            assert!(
                !should_sleep_after_datagram_batch(batch_len),
                "1 件以上届いたバッチでは sleep しないこと: batch_len={batch_len}"
            );
        }
    }

    /// LOC Properties の書式違反でセッション終了を依頼すること
    ///
    /// draft-ietf-moq-transport-22 §8.3 (Key-Value-Pair Structure) は書式違反の受信を
    /// セッション終了の対象とする MUST を定める。datagram 経路も stream 経路と同じ扱いにする。
    #[tokio::test]
    async fn audio_delivery_requests_session_termination_on_malformed_loc_properties() {
        let (audio_tx, audio_rx) = std::sync::mpsc::channel();
        let audio_config_handled = std::sync::atomic::AtomicBool::new(false);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_handled,
            &termination_tx,
            &catch_up,
            &delivery_order,
            None,
        );

        // Properties Length の宣言に対して実データが足りない壊れたバイト列
        let deliver = delivery
            .deliver(Some(loc(1, 0)), Some(&[0xFF, 0xFF, 0xFF]), b"payload", None)
            .await;

        assert!(!deliver, "書式違反では配送を打ち切ること");
        let termination = termination_rx
            .try_recv()
            .expect("セッション終了が依頼されること");
        assert_eq!(
            termination.code,
            shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION,
            "書式違反のセッション終了コードであること"
        );
        assert!(
            audio_rx.try_recv().is_err(),
            "書式違反の Object はデコードしないこと"
        );
    }

    /// datagram の音声 Object が MP4 へ保存されること
    ///
    /// `--mp4` は受信した payload を再エンコードせずに mux する。datagram 経路も
    /// stream 経路と同じ `record_audio_object` を通るため、録画対象になる。
    #[tokio::test]
    async fn audio_delivery_records_a_datagram_audio_object() {
        let path =
            std::env::temp_dir().join(format!("moq-sub-datagram-audio-{}.mp4", std::process::id()));
        std::fs::remove_file(&path).ok();

        let recorder = Recorder::start(
            path.clone(),
            Setup {
                video: None,
                audio: Some(AudioSetup {
                    sample_rate: 48_000,
                    channels: 1,
                }),
            },
        )
        .expect("録画を開始できること");
        let recorder_sender = recorder.sender();

        let packet = encode_opus_silence(48_000, 1);
        let properties = audio_loc_properties(960, 48_000, Some(&opus_head(48_000, 1)));
        let (audio_tx, _audio_rx) = std::sync::mpsc::channel();
        let audio_config_handled = std::sync::atomic::AtomicBool::new(false);
        let (termination_tx, _termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_handled,
            &termination_tx,
            &catch_up,
            &delivery_order,
            Some(&recorder_sender),
        );

        // 再生しない指定と同じくデコーダ無しでも録画は行う
        let properties_bytes = properties
            .encode()
            .expect("LOC プロパティを encode できること");
        let deliver = delivery
            .deliver(Some(loc(0, 0)), Some(&properties_bytes), &packet, None)
            .await;
        assert!(deliver, "録画のみの経路でも配送を継続すること");
        recorder.finish().expect("MP4 を finalize できること");

        // 書き出した MP4 を読み戻し、audio トラック 1 本にサンプルが 1 つ入っていることを見る
        let bytes = std::fs::read(&path).expect("MP4 ファイルを読めること");
        let mut demuxer = shiguredo_mp4::demux::Mp4FileDemuxer::new();
        while let Some(required) = demuxer.required_input() {
            let position = required.position as usize;
            demuxer.handle_input(shiguredo_mp4::demux::Input {
                position: required.position,
                data: &bytes[position..],
            });
        }
        assert_eq!(
            demuxer
                .tracks()
                .expect("トラック情報を取得できること")
                .len(),
            1,
            "audio トラック 1 本の MP4 になること"
        );
        let mut samples = 0;
        while demuxer
            .next_sample()
            .expect("サンプルを読めること")
            .is_some()
        {
            samples += 1;
        }
        assert_eq!(
            samples, 1,
            "datagram で届いた音声が 1 サンプル保存されること"
        );

        std::fs::remove_file(&path).ok();
    }

    /// テスト用: 位置を作る
    fn loc(group_id: u64, object_id: u64) -> Location {
        Location {
            group_id,
            object_id,
        }
    }

    /// テスト用: LOC の TIMESTAMP と位置を指定して音声 Object を 1 つ配送する
    ///
    /// Timescale は 1_000_000 とする。Timestamp がそのままマイクロ秒になり、受信側の
    /// 壁時計と比べられる値 (draft-ietf-moq-loc-04 §2.3.1.1) をそのまま渡せる。
    async fn deliver_audio_at(
        delivery: &AudioDelivery<'_>,
        decoder: &mut AudioDecoder,
        location: Option<Location>,
        pts_us: i64,
        payload: &[u8],
    ) -> bool {
        let timestamp = u64::try_from(pts_us).expect("TIMESTAMP は 0 以上であること");
        let properties = audio_loc_properties(timestamp, 1_000_000, None);
        let properties_bytes = properties
            .encode()
            .expect("LOC プロパティを encode できること");
        delivery
            .deliver(location, Some(&properties_bytes), payload, Some(decoder))
            .await
    }

    /// 同じ識別子の音声 Object の 2 回目は録画も復号も送出もしないこと
    ///
    /// datagram は再送されない (RFC 9221 §3) ため、損失に備えて publisher が同じ Object を
    /// 複数回送ることがある。2 回目を捨てないと同じ音を 2 回鳴らし、録画も重複する。
    #[tokio::test]
    async fn audio_delivery_drops_a_duplicated_object() {
        let path = std::env::temp_dir().join(format!(
            "moq-sub-duplicated-audio-{}.mp4",
            std::process::id()
        ));
        std::fs::remove_file(&path).ok();

        let recorder = Recorder::start(
            path.clone(),
            Setup {
                video: None,
                audio: Some(AudioSetup {
                    sample_rate: 48_000,
                    channels: 1,
                }),
            },
        )
        .expect("録画を開始できること");
        let recorder_sender = recorder.sender();

        let packet = encode_opus_silence(48_000, 1);
        let (audio_tx, audio_rx) = std::sync::mpsc::channel();
        let audio_config_handled = std::sync::atomic::AtomicBool::new(false);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_handled,
            &termination_tx,
            &catch_up,
            &delivery_order,
            Some(&recorder_sender),
        );
        let mut decoder =
            AudioDecoder::build("opus", 48_000, 1).expect("Opus デコーダを生成できること");

        // 同じ Object (識別子も TIMESTAMP も同じ) を 2 回配送する。冗長化した datagram の
        // 1 回目と 2 回目がこれにあたる
        let base_us = crate::wall_clock_us();
        for copy in 0..2 {
            assert!(
                deliver_audio_at(&delivery, &mut decoder, Some(loc(5, 0)), base_us, &packet).await,
                "重複した音声でも配送を継続すること: copy={copy}"
            );
        }
        assert!(
            termination_rx.try_recv().is_err(),
            "書式違反が無いためセッション終了は依頼されないこと"
        );

        // 復号と送出は 1 回だけである。2 回目も復号していれば、同じ TIMESTAMP のフレームが
        // もう 1 つ届く
        let sent: Vec<i64> = std::iter::from_fn(|| audio_rx.try_recv().ok())
            .map(|frame| frame.pts_us)
            .collect();
        assert_eq!(
            sent,
            vec![base_us],
            "同じ識別子の 2 回目は復号も送出もしないこと (送出順={sent:?})"
        );
        assert!(
            audio_rx.try_recv().is_err(),
            "2 回目で追加のフレームを送出しないこと"
        );
        // 重複として落とした 1 件を累計に残す。原因の切り分け用の内訳である
        let counters = delivery_order
            .lock()
            .expect("audio delivery order lock is not poisoned")
            .counters();
        assert_eq!(
            counters.duplicate_objects, 1,
            "重複として落とした Object を累計に数えること"
        );
        assert_eq!(
            counters.stale_objects, 0,
            "重複の経路では stale を数えないこと"
        );
        assert_eq!(
            counters.before_boundary_objects, 0,
            "境界が無いため境界以下を数えないこと"
        );

        recorder.finish().expect("MP4 を finalize できること");

        // 書き出した MP4 を読み戻し、録画も 1 サンプルだけであることを見る
        let bytes = std::fs::read(&path).expect("MP4 ファイルを読めること");
        let mut demuxer = shiguredo_mp4::demux::Mp4FileDemuxer::new();
        while let Some(required) = demuxer.required_input() {
            let position = required.position as usize;
            demuxer.handle_input(shiguredo_mp4::demux::Input {
                position: required.position,
                data: &bytes[position..],
            });
        }
        let mut samples = 0;
        while demuxer
            .next_sample()
            .expect("サンプルを読めること")
            .is_some()
        {
            samples += 1;
        }
        assert_eq!(
            samples, 1,
            "同じ識別子の 2 回目は録画もしないこと (サンプル数={samples})"
        );

        std::fs::remove_file(&path).ok();
    }

    /// 音声配送の累計が増えたときだけログ用に更新すること
    ///
    /// 100 ms の tick で累計を前回のログと比べ、増えたときだけ info を出す。増えていないときに
    /// 同じ内容を出し続けるとログが埋もれ、原因の切り分けに使えなくなる。
    #[test]
    fn audio_delivery_counters_are_logged_only_when_they_increase() {
        let mut previous = AudioDeliveryCounters::default();
        log_audio_delivery_counters(&mut previous, AudioDeliveryCounters::default());
        assert_eq!(
            previous,
            AudioDeliveryCounters::default(),
            "増えていないときはログを出さず、前回の値を変えないこと"
        );

        let current = AudioDeliveryCounters {
            duplicate_objects: 2,
            stale_objects: 3,
            before_boundary_objects: 4,
        };
        log_audio_delivery_counters(&mut previous, current);
        assert_eq!(
            previous, current,
            "増えたときはログを出して前回の値を更新すること"
        );

        // 同じ累計をもう一度渡しても、ログは出さず前回の値も変えない
        log_audio_delivery_counters(&mut previous, current);
        assert_eq!(previous, current, "同じ累計では前回の値を変えないこと");
    }

    /// 重複の判定は直近 256 件の識別子だけを覚え、窓から溢れた古い識別子は忘れること
    ///
    /// 冗長化した datagram の 2 回目は 1 回目の直後に届くため、これだけ覚えておけば足りる。
    /// 窓を無制限にすると、長時間の配信で覚える識別子が増え続ける。
    #[test]
    fn audio_delivery_order_forgets_locations_that_fall_out_of_the_window() {
        let mut order = AudioDeliveryOrder::new(None);

        // 位置が分からない Object は重複と判定できないため、何度届いても重複としない
        assert!(
            !order.is_duplicate(None),
            "位置が分からない Object は重複としないこと"
        );
        assert!(
            !order.is_duplicate(None),
            "位置が分からない Object は 2 回目も重複としないこと"
        );

        // 窓の大きさぶんは覚えている
        for index in 0..AUDIO_RECENT_OBJECT_LOCATIONS_MAX as u64 {
            assert!(
                !order.is_duplicate(Some(loc(index, 0))),
                "初めて届いた識別子は重複としないこと: index={index}"
            );
        }
        assert!(
            order.is_duplicate(Some(loc(0, 0))),
            "窓に入っている識別子の 2 回目は重複とすること"
        );

        // 257 件目で最も古い識別子が窓から溢れ、忘れる
        assert!(
            !order.is_duplicate(Some(loc(256, 0))),
            "257 件目の識別子は重複としないこと"
        );
        assert!(
            !order.is_duplicate(Some(loc(0, 0))),
            "窓から溢れた古い識別子は覚えていないこと"
        );
        assert!(
            order.is_duplicate(Some(loc(0, 0))),
            "忘れたあとに届いた識別子は改めて覚えること"
        );

        // 落とした理由の内訳として、重複の累計だけが 2 になる
        let counters = order.counters();
        assert_eq!(
            counters.duplicate_objects, 2,
            "重複と判定した Object だけを数えること"
        );
        assert_eq!(
            counters.stale_objects, 0,
            "重複の判定では stale を数えないこと"
        );
        assert_eq!(
            counters.before_boundary_objects, 0,
            "重複の判定では境界以下を数えないこと"
        );
    }

    /// 音声の送出順の判定が、送出済みの TIMESTAMP との比較だけで決まること
    ///
    /// - 最初の Object は送出する
    /// - 進んだ TIMESTAMP は送出し、判定の基準を更新する
    /// - 古い TIMESTAMP は送出しない (判定の基準は変えない)
    /// - 同じ TIMESTAMP は送出する (同じ TIMESTAMP を持つ別の Object でありうる)
    /// - TIMESTAMP を持たない Object は送出し、判定の基準は変えない
    #[test]
    fn audio_delivery_order_admits_only_timestamps_that_do_not_rewind() {
        let mut order = AudioDeliveryOrder::new(None);
        assert_eq!(
            order.last_pts_us(),
            None,
            "最初は送出済みの TIMESTAMP を持たないこと"
        );

        assert_eq!(
            order.admit(Some(2_000)),
            AudioDeliveryAdmission::Deliver,
            "最初の Object は送出すること"
        );
        assert_eq!(
            order.last_pts_us(),
            Some(2_000),
            "送出した TIMESTAMP を基準にすること"
        );

        assert_eq!(
            order.admit(Some(2_020)),
            AudioDeliveryAdmission::Deliver,
            "進んだ TIMESTAMP は送出すること"
        );
        assert_eq!(
            order.last_pts_us(),
            Some(2_020),
            "基準を新しい TIMESTAMP へ進めること"
        );

        // FETCH の応答で購読より前の音声が届く場合がこれにあたる
        assert_eq!(
            order.admit(Some(1_920)),
            AudioDeliveryAdmission::Stale,
            "送出済みの TIMESTAMP より古い音声は送出しないこと"
        );
        assert_eq!(
            order.last_pts_us(),
            Some(2_020),
            "送出しなかった Object で基準を戻さないこと"
        );

        assert_eq!(
            order.admit(Some(2_020)),
            AudioDeliveryAdmission::Deliver,
            "同じ TIMESTAMP の音声は送出すること"
        );

        // TIMESTAMP を持たない Object は順序を判定できないため、基準を変えずに送出する
        assert_eq!(
            order.admit(None),
            AudioDeliveryAdmission::Deliver,
            "TIMESTAMP を持たない音声は送出すること"
        );
        assert_eq!(
            order.last_pts_us(),
            Some(2_020),
            "TIMESTAMP を持たない音声で基準を変えないこと"
        );
        assert_eq!(
            order.admit(Some(2_040)),
            AudioDeliveryAdmission::Deliver,
            "TIMESTAMP を持たない音声の後でも、基準より新しい音声は送出すること"
        );

        // 古い TIMESTAMP で落とした 1 件だけを stale の累計に数える
        let counters = order.counters();
        assert_eq!(
            counters.stale_objects, 1,
            "送出済みの TIMESTAMP より古い音声だけを数えること"
        );
        assert_eq!(
            counters.duplicate_objects, 0,
            "送出順の判定では重複を数えないこと"
        );
        assert_eq!(
            counters.before_boundary_objects, 0,
            "送出順の判定では境界以下を数えないこと"
        );
    }

    /// 購読のあとに FETCH で届いた古い音声が、送出済みの TIMESTAMP を巻き戻さないこと
    ///
    /// 購読より前に publish された Group は SUBSCRIBE では届かない。moq-sub は SUBSCRIBE_OK が
    /// 告知した Largest Object を基準に直近 4 Group を FETCH で要求し、応答の Object を live と
    /// 同じ配送経路へ流す (draft-ietf-moq-transport-22 §3.5.1 (Dynamically Starting New Groups))。
    /// FETCH の応答は live の Object と並行して届くため、live の音声を送出したあとに、それより
    /// 前に publish された音声が届く。音声の再生は TIMESTAMP の順を前提とする
    /// (映像と違い、復号の出力がそのまま提示の順になる) ため、古い音声は送出しない。
    ///
    /// 判定が無いと、FETCH で遡った 4 つの TIMESTAMP が再生経路へ流れ、送出済みの TIMESTAMP
    /// から巻き戻る。
    #[tokio::test]
    async fn audio_delivery_does_not_send_objects_older_than_the_delivered_one() {
        let packet = encode_opus_silence(48_000, 1);
        let (audio_tx, audio_rx) = std::sync::mpsc::channel();
        let audio_config_handled = std::sync::atomic::AtomicBool::new(false);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_handled,
            &termination_tx,
            &catch_up,
            &delivery_order,
            None,
        );
        let mut decoder =
            AudioDecoder::build("opus", 48_000, 1).expect("Opus デコーダを生成できること");

        // TIMESTAMP は受信側の壁時計と比べられる (受信側の追いつきの判定)。大きく離れていると
        // 復号器の作り直しが動くため、実際に受信するときと同じく現在時刻を基準にする
        let base_us = crate::wall_clock_us();
        // 20 ms 間隔の音声。0 から 3 が購読より前、4 以降が購読のあとである
        let pts_us = |index: i64| base_us + index * 20_000;

        // 1. live の音声 (購読後に届いた最新のもの) を送出する
        assert!(
            deliver_audio_at(&delivery, &mut decoder, None, pts_us(4), &packet).await,
            "live の音声は配送を継続すること"
        );
        // 2. FETCH の応答が、購読より前に publish された 4 つを届ける
        for index in 0..4 {
            assert!(
                deliver_audio_at(&delivery, &mut decoder, None, pts_us(index), &packet).await,
                "古い音声でも配送を継続すること"
            );
        }
        // 3. live の音声が続く
        assert!(
            deliver_audio_at(&delivery, &mut decoder, None, pts_us(5), &packet).await,
            "live の音声は配送を継続すること"
        );

        // 4. Object Status のみの Object と違反ではないため、セッション終了は依頼されない
        assert!(
            termination_rx.try_recv().is_err(),
            "書式違反が無いためセッション終了は依頼されないこと"
        );

        let sent: Vec<i64> = std::iter::from_fn(|| audio_rx.try_recv().ok())
            .map(|frame| frame.pts_us)
            .collect();
        for pair in sent.windows(2) {
            assert!(
                pair[0] <= pair[1],
                "音声の PTS が巻き戻らないこと: {} -> {}",
                pair[0],
                pair[1]
            );
        }
        assert_eq!(
            sent,
            vec![pts_us(4), pts_us(5)],
            "送出済みの TIMESTAMP より古い音声は再生経路へ流さないこと (送出順={sent:?})"
        );
        assert_eq!(
            delivery_order
                .lock()
                .expect("audio delivery order lock is not poisoned")
                .last_pts_us(),
            Some(pts_us(5)),
            "送出順の判定は live と FETCH の応答で共有すること"
        );
        // 送出済みの TIMESTAMP より古い 4 件を累計に残す
        let counters = delivery_order
            .lock()
            .expect("audio delivery order lock is not poisoned")
            .counters();
        assert_eq!(
            counters.stale_objects, 4,
            "送出済みの TIMESTAMP より古い音声を累計に数えること"
        );
        assert_eq!(
            counters.duplicate_objects, 0,
            "位置が分からない Object は重複として数えないこと"
        );
        assert_eq!(
            counters.before_boundary_objects, 0,
            "境界が無いため境界以下を数えないこと"
        );
    }

    /// 購読の境界の判定が位置だけで決まること
    ///
    /// - 境界が無いときは、すべて live として扱う (Largest Object を告知しない publisher)
    /// - 位置が分からない Object は、境界と比べられないため live として扱う
    /// - 境界と同じ位置と、境界より前の位置は「境界以下」である
    /// - 境界より後の位置は live である
    /// - 再購読で境界を更新すると、新しい境界で判定する
    #[test]
    fn audio_delivery_order_detects_objects_at_or_before_the_boundary() {
        let mut order = AudioDeliveryOrder::new(None);
        assert!(
            !order.is_before_boundary(Some(loc(100, 5))),
            "境界が無いときは live として扱うこと"
        );

        order.set_boundary(Some(Location {
            group_id: 100,
            object_id: 5,
        }));
        assert!(
            !order.is_before_boundary(None),
            "位置が分からない Object は live として扱うこと"
        );
        assert!(
            order.is_before_boundary(Some(loc(100, 5))),
            "境界と同じ位置は境界以下であること"
        );
        assert!(
            order.is_before_boundary(Some(loc(100, 4))),
            "境界より前の Object ID は境界以下であること"
        );
        assert!(
            order.is_before_boundary(Some(loc(99, 9))),
            "境界より前の Group は境界以下であること"
        );
        assert!(
            !order.is_before_boundary(Some(loc(100, 6))),
            "境界より後の Object ID は live であること"
        );
        assert!(
            !order.is_before_boundary(Some(loc(101, 0))),
            "境界より後の Group は live であること"
        );

        // GOAWAY の移行で張り直した購読の Largest Object へ更新する
        order.set_boundary(Some(Location {
            group_id: 200,
            object_id: 0,
        }));
        assert!(
            order.is_before_boundary(Some(loc(100, 6))),
            "更新前の境界より後の位置でも、更新後の境界より前なら境界以下であること"
        );
        assert!(
            order.is_before_boundary(Some(loc(200, 0))),
            "更新後の境界と同じ位置は境界以下であること"
        );
        assert!(
            !order.is_before_boundary(Some(loc(200, 1))),
            "更新後の境界より後の位置は live であること"
        );

        // 境界を外す (Largest Object を告知しない publisher へ張り直した場合)
        order.set_boundary(None);
        assert!(
            !order.is_before_boundary(Some(loc(1, 0))),
            "境界を外したあとは、すべて live として扱うこと"
        );

        // 境界以下と判定した 5 件を累計に数える (境界を外したあとの 1 件は数えない)
        let counters = order.counters();
        assert_eq!(
            counters.before_boundary_objects, 5,
            "境界以下と判定した Object だけを数えること"
        );
        assert_eq!(
            counters.duplicate_objects, 0,
            "境界の判定では重複を数えないこと"
        );
        assert_eq!(
            counters.stale_objects, 0,
            "境界の判定では stale を数えないこと"
        );
    }

    /// 購読の境界以下の音声が再生経路へ流れないこと
    ///
    /// FETCH の要求 range の終端は `SUBSCRIBE_OK` の Largest Object であり、その位置の Object は
    /// live と FETCH の両方で届きうる (draft-ietf-moq-transport-22 §3.1.4 / §3.2)。境界以下を
    /// 送出しないため、どちらで届いても再生経路へは流れない。
    ///
    /// 境界より後の live の音声は従来どおり送出し、TIMESTAMP による送出順の判定は境界より後の
    /// 分だけで働く。境界以下の音声は catch-up の観測へも渡さない。
    ///
    /// 同じ位置を 3 回届ける手順があるが、2 回目以降は同じ識別子の重複として先に落ちる
    /// (datagram の冗長化による copy と同じ扱い)。境界以下の判定そのものは、位置の異なる
    /// 手順で固定する。
    #[tokio::test]
    async fn audio_delivery_does_not_send_objects_before_the_subscription_boundary() {
        let packet = encode_opus_silence(48_000, 1);
        let (audio_tx, audio_rx) = std::sync::mpsc::channel();
        let audio_config_handled = std::sync::atomic::AtomicBool::new(false);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        // 購読の境界 (SUBSCRIBE_OK の Largest Object) は Group 100 / Object 0 とする
        delivery_order
            .lock()
            .expect("audio delivery order lock is not poisoned")
            .set_boundary(Some(Location {
                group_id: 100,
                object_id: 0,
            }));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_handled,
            &termination_tx,
            &catch_up,
            &delivery_order,
            None,
        );
        let mut decoder =
            AudioDecoder::build("opus", 48_000, 1).expect("Opus デコーダを生成できること");

        // TIMESTAMP は受信側の壁時計と比べられる (受信側の追いつきの判定)。実際に受信するときと
        // 同じく現在時刻を基準にする
        let base_us = crate::wall_clock_us();
        let pts_us = |index: i64| base_us + index * 20_000;

        // 1. 境界の位置の Object が FETCH の応答で届く
        assert!(
            deliver_audio_at(
                &delivery,
                &mut decoder,
                Some(loc(100, 0)),
                pts_us(0),
                &packet
            )
            .await,
            "境界以下の音声でも配送を継続すること"
        );
        // 2. 境界より前の Group の Object が届く
        assert!(
            deliver_audio_at(
                &delivery,
                &mut decoder,
                Some(loc(99, 3)),
                pts_us(1),
                &packet
            )
            .await,
            "境界以下の音声でも配送を継続すること"
        );

        // 境界以下の音声は復号も送出もしないため、catch-up の観測にも渡らない
        assert_eq!(
            catch_up
                .lock()
                .expect("audio catch up lock is not poisoned")
                .snapshot()
                .lag_ms,
            None,
            "境界以下の音声を catch-up の観測へ渡さないこと"
        );

        // 3. live の音声が届く
        assert!(
            deliver_audio_at(
                &delivery,
                &mut decoder,
                Some(loc(100, 1)),
                pts_us(2),
                &packet
            )
            .await,
            "live の音声は配送を継続すること"
        );
        // 4. 境界の位置の Object が live でも届く (1. と同じ位置)
        assert!(
            deliver_audio_at(
                &delivery,
                &mut decoder,
                Some(loc(100, 0)),
                pts_us(0),
                &packet
            )
            .await,
            "境界の位置の音声でも配送を継続すること"
        );
        // 5. live の音声が続く
        assert!(
            deliver_audio_at(
                &delivery,
                &mut decoder,
                Some(loc(100, 2)),
                pts_us(3),
                &packet
            )
            .await,
            "live の音声は配送を継続すること"
        );
        // 6. 境界以下の音声が、live よりずっと古い TIMESTAMP で届く (FETCH の応答が遅れた場合)。
        // 復号も観測もしないため、追いつきの遅れは live の値から動かない
        let lag_after_live = catch_up
            .lock()
            .expect("audio catch up lock is not poisoned")
            .snapshot()
            .lag_ms;
        assert!(
            deliver_audio_at(
                &delivery,
                &mut decoder,
                Some(loc(100, 0)),
                base_us - 5_000_000,
                &packet
            )
            .await,
            "境界以下の音声でも配送を継続すること"
        );
        assert_eq!(
            catch_up
                .lock()
                .expect("audio catch up lock is not poisoned")
                .snapshot()
                .lag_ms,
            lag_after_live,
            "境界以下の音声で catch-up の遅れを更新しないこと"
        );

        assert!(
            termination_rx.try_recv().is_err(),
            "書式違反が無いためセッション終了は依頼されないこと"
        );
        let sent: Vec<i64> = std::iter::from_fn(|| audio_rx.try_recv().ok())
            .map(|frame| frame.pts_us)
            .collect();
        assert_eq!(
            sent,
            vec![pts_us(2), pts_us(3)],
            "境界以下の音声は再生経路へ流さないこと (送出順={sent:?})"
        );
        assert_eq!(
            delivery_order
                .lock()
                .expect("audio delivery order lock is not poisoned")
                .last_pts_us(),
            Some(pts_us(3)),
            "境界より後の live だけで送出順の基準を進めること"
        );
        // 落とした理由の内訳を固定する。境界の位置を 3 回届けているが、2 回目以降は重複として
        // 先に落ちるため、境界以下の累計は重複と二重に数えない
        let counters = delivery_order
            .lock()
            .expect("audio delivery order lock is not poisoned")
            .counters();
        assert_eq!(
            counters.duplicate_objects, 2,
            "同じ識別子の 2 回目を重複として数えること"
        );
        assert_eq!(
            counters.before_boundary_objects, 2,
            "境界以下と判定した Object を数えること"
        );
        assert_eq!(
            counters.stale_objects, 0,
            "境界以下の音声は送出順の判定まで至らないこと"
        );
    }

    /// 受信の入口の入力の遅れが、復号の出力の遅れから引かれた「復号が足している遅れ」として
    /// 追いつきの観測へ渡ること
    ///
    /// 壁時計の TIMESTAMP (Timescale なし) を持つ音では、復号へ渡す直前の到着から入力の遅れを
    /// 測り、復号の出力の遅れから引いた値を統計へ残す。TIMESTAMP に Timescale がある
    /// (メディア時刻である) 音では測れず、出力の遅れをそのまま使う。
    #[tokio::test]
    async fn audio_delivery_passes_the_input_lag_to_the_catch_up() {
        let packet = encode_opus_silence(48_000, 1);
        let (audio_tx, audio_rx) = std::sync::mpsc::channel();
        let audio_config_handled = std::sync::atomic::AtomicBool::new(false);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_handled,
            &termination_tx,
            &catch_up,
            &delivery_order,
            None,
        );
        let mut decoder =
            AudioDecoder::build("opus", 48_000, 1).expect("Opus デコーダを生成できること");

        // 1. 壁時計の TIMESTAMP (Timescale なし) を持つ音。配送の直前に測った壁時計を
        // TIMESTAMP にするため、入力の遅れは 0 前後になる
        let base_us = crate::wall_clock_us();
        let timestamp = u64::try_from(base_us).expect("壁時計は 0 以上であること");
        let properties = audio_loc_properties_without_timescale(timestamp, None);
        let properties_bytes = properties
            .encode()
            .expect("LOC プロパティを encode できること");
        assert!(
            delivery
                .deliver(None, Some(&properties_bytes), &packet, Some(&mut decoder))
                .await,
            "壁時計の TIMESTAMP を持つ音でも配送を継続すること"
        );
        let stats = catch_up
            .lock()
            .expect("audio catch up lock is not poisoned")
            .snapshot();
        let lag_ms = stats.lag_ms.expect("復号の出力の遅れを観測すること");
        let input_lag_ms = stats
            .input_lag_ms
            .expect("壁時計の TIMESTAMP では入力の遅れを測ること");
        let decode_lag_ms = stats
            .decode_lag_ms
            .expect("「復号が足している遅れ」を残すこと");
        assert!(
            (0..=1_000).contains(&input_lag_ms),
            "配送の直前の TIMESTAMP なので入力の遅れは 0 前後になること: {input_lag_ms}"
        );
        assert_eq!(
            lag_ms - input_lag_ms,
            decode_lag_ms,
            "出力の遅れから入力の遅れを引いた値が「復号が足している遅れ」であること"
        );

        // 2. Timescale がある (メディア時刻である) 音。入力の遅れは測れないため、出力の遅れを
        // そのまま使う
        let media_pts_us = base_us + 20_000;
        let media_timestamp = u64::try_from(media_pts_us).expect("時刻は 0 以上であること");
        let properties = audio_loc_properties(media_timestamp, 1_000_000, None);
        let properties_bytes = properties
            .encode()
            .expect("LOC プロパティを encode できること");
        assert!(
            delivery
                .deliver(None, Some(&properties_bytes), &packet, Some(&mut decoder))
                .await,
            "メディア時刻の音でも配送を継続すること"
        );
        let stats = catch_up
            .lock()
            .expect("audio catch up lock is not poisoned")
            .snapshot();
        assert_eq!(
            stats.input_lag_ms, None,
            "Timescale がある音では入力の遅れを測らないこと"
        );
        assert_eq!(
            stats.decode_lag_ms, stats.lag_ms,
            "測れないときは出力の遅れをそのまま使うこと"
        );

        assert!(
            termination_rx.try_recv().is_err(),
            "書式違反が無いためセッション終了は依頼されないこと"
        );
        let sent: Vec<i64> = std::iter::from_fn(|| audio_rx.try_recv().ok())
            .map(|frame| frame.pts_us)
            .collect();
        assert_eq!(
            sent,
            vec![base_us, media_pts_us],
            "どちらの音も再生経路へ流すこと (送出順={sent:?})"
        );
    }

    /// Largest Object を告知しない購読では、すべての音声を live として送出すること
    ///
    /// draft-ietf-moq-transport-22 §9.20.17 (LARGEST OBJECT Parameter) は Object が publish
    /// されていれば LARGEST_OBJECT を MUST 含むと定めるため、省略は未 publish を意味する。
    /// 境界が無いことを理由に無音にしない。
    #[tokio::test]
    async fn audio_delivery_sends_objects_when_there_is_no_boundary() {
        let packet = encode_opus_silence(48_000, 1);
        let (audio_tx, audio_rx) = std::sync::mpsc::channel();
        let audio_config_handled = std::sync::atomic::AtomicBool::new(false);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        // 境界を設定しない (Largest Object が告知されなかった購読)
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_handled,
            &termination_tx,
            &catch_up,
            &delivery_order,
            None,
        );
        let mut decoder =
            AudioDecoder::build("opus", 48_000, 1).expect("Opus デコーダを生成できること");

        let base_us = crate::wall_clock_us();
        let pts_us = |index: i64| base_us + index * 20_000;

        // 位置が分からない Object も live として送出する
        assert!(
            deliver_audio_at(&delivery, &mut decoder, None, pts_us(0), &packet).await,
            "位置が分からない音声でも配送を継続すること"
        );
        // 位置が分かる Object も、境界が無いため live として送出する
        assert!(
            deliver_audio_at(&delivery, &mut decoder, Some(loc(1, 0)), pts_us(1), &packet).await,
            "境界が無いときの音声でも配送を継続すること"
        );

        assert!(
            termination_rx.try_recv().is_err(),
            "書式違反が無いためセッション終了は依頼されないこと"
        );
        let sent: Vec<i64> = std::iter::from_fn(|| audio_rx.try_recv().ok())
            .map(|frame| frame.pts_us)
            .collect();
        assert_eq!(
            sent,
            vec![pts_us(0), pts_us(1)],
            "境界が無いときはすべての音声を送出すること (送出順={sent:?})"
        );
    }

    /// 購読の境界以下の音声でも Audio Config の検証が行われること
    ///
    /// 境界以下は送出も録画も復号もしないが、Audio Config は Properties から検証する。
    /// 途中参加の購読では OpusHead が FETCH の応答だけで届くことがあるためである。
    #[tokio::test]
    async fn audio_delivery_validates_audio_config_before_the_subscription_boundary() {
        let packet = encode_opus_silence(48_000, 1);
        let properties = audio_loc_properties(960, 48_000, Some(&opus_head(48_000, 1)));
        let properties_bytes = properties
            .encode()
            .expect("LOC プロパティを encode できること");

        let (audio_tx, audio_rx) = std::sync::mpsc::channel();
        let audio_config_handled = std::sync::atomic::AtomicBool::new(false);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        delivery_order
            .lock()
            .expect("audio delivery order lock is not poisoned")
            .set_boundary(Some(Location {
                group_id: 10,
                object_id: 0,
            }));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_handled,
            &termination_tx,
            &catch_up,
            &delivery_order,
            None,
        );
        let mut decoder =
            AudioDecoder::build("opus", 48_000, 1).expect("Opus デコーダを生成できること");

        let deliver = delivery
            .deliver(
                Some(loc(10, 0)),
                Some(&properties_bytes),
                &packet,
                Some(&mut decoder),
            )
            .await;

        assert!(deliver, "境界以下の音声でも配送を継続すること");
        assert!(
            audio_config_handled.load(Ordering::Acquire),
            "境界以下の音声でも Audio Config を処理済みとして記録すること"
        );
        assert!(
            audio_rx.try_recv().is_err(),
            "境界以下の音声は復号済みフレームを送出しないこと"
        );
        assert!(
            termination_rx.try_recv().is_err(),
            "書式違反が無いためセッション終了は依頼されないこと"
        );
    }

    /// 購読の境界以下の音声が MP4 へ保存されないこと
    ///
    /// 録画は復号の可否に関わらず行うが、購読より前に publish された音声は購読の時点で
    /// 不要である。境界以下を録画しないため、同じ Object が live と FETCH の両方で届いても
    /// MP4 に重複が残らない。
    #[tokio::test]
    async fn audio_delivery_does_not_record_objects_before_the_subscription_boundary() {
        let path =
            std::env::temp_dir().join(format!("moq-sub-audio-boundary-{}.mp4", std::process::id()));
        std::fs::remove_file(&path).ok();

        let recorder = Recorder::start(
            path.clone(),
            Setup {
                video: None,
                audio: Some(AudioSetup {
                    sample_rate: 48_000,
                    channels: 1,
                }),
            },
        )
        .expect("録画を開始できること");
        let recorder_sender = recorder.sender();

        // 境界以下の音声 (FETCH の応答) と live の音声で TIMESTAMP と payload を変える。
        // MP4 の先頭サンプルの decode timestamp は 0 になるため、保存された Object は
        // payload で見分ける
        let boundary_below = audio_loc_properties(960, 48_000, None)
            .encode()
            .expect("LOC プロパティを encode できること");
        let live = audio_loc_properties(1_920, 48_000, None)
            .encode()
            .expect("LOC プロパティを encode できること");
        let boundary_below_payload = b"boundary-before-live";
        let live_payload = b"live";

        let (audio_tx, _audio_rx) = std::sync::mpsc::channel();
        let audio_config_handled = std::sync::atomic::AtomicBool::new(false);
        let (termination_tx, _termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        delivery_order
            .lock()
            .expect("audio delivery order lock is not poisoned")
            .set_boundary(Some(Location {
                group_id: 10,
                object_id: 0,
            }));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_handled,
            &termination_tx,
            &catch_up,
            &delivery_order,
            Some(&recorder_sender),
        );

        // 境界以下の音声は録画しない
        assert!(
            delivery
                .deliver(
                    Some(loc(10, 0)),
                    Some(&boundary_below),
                    boundary_below_payload,
                    None
                )
                .await,
            "境界以下の音声でも配送を継続すること"
        );
        // 境界より後の live の音声は録画する
        assert!(
            delivery
                .deliver(Some(loc(10, 1)), Some(&live), live_payload, None)
                .await,
            "live の音声でも配送を継続すること"
        );
        recorder.finish().expect("MP4 を finalize できること");

        // 書き出した MP4 を読み戻し、live の音声だけが 1 サンプル入っていることを見る
        let bytes = std::fs::read(&path).expect("MP4 ファイルを読めること");
        let mut demuxer = shiguredo_mp4::demux::Mp4FileDemuxer::new();
        while let Some(required) = demuxer.required_input() {
            let position = required.position as usize;
            demuxer.handle_input(shiguredo_mp4::demux::Input {
                position: required.position,
                data: &bytes[position..],
            });
        }
        let mut samples = 0;
        let mut recorded: Vec<u8> = Vec::new();
        while let Some(sample) = demuxer.next_sample().expect("サンプルを読めること") {
            samples += 1;
            let start = sample.data_offset as usize;
            recorded.extend_from_slice(&bytes[start..start + sample.data_size]);
        }
        assert_eq!(
            samples, 1,
            "境界以下の音声は保存せず、live の音声だけを保存すること"
        );
        assert_eq!(
            recorded,
            live_payload.to_vec(),
            "保存された音声が live の Object であること"
        );

        std::fs::remove_file(&path).ok();
    }

    /// 音声 Object の位置が 3 経路それぞれの Group ID と Object ID から取れること
    ///
    /// datagram / subgroup stream / FETCH 応答は位置の持ち方が異なるため、取り違えを
    /// ここで固定する。位置を取り違えると、購読の境界と比べられず境界以下の音声が
    /// 再生経路へ流れる。
    #[test]
    fn audio_object_location_comes_from_each_paths_group_and_object_id() {
        const AUDIO_ALIAS: u64 = 7;
        const GROUP_ID: u64 = 1_000;
        const OBJECT_ID: u64 = 42;

        // datagram は OBJECT_DATAGRAM のヘッダが位置を持つ
        let properties = audio_loc_properties(960, 48_000, None);
        let raw = object_datagram_wire(
            AUDIO_ALIAS,
            GROUP_ID,
            OBJECT_ID,
            Some(&properties),
            b"payload",
            None,
        );
        let mut track_map = HashMap::new();
        track_map.insert(AUDIO_ALIAS, TrackKind::Audio);
        let DatagramAction::DeliverAudio { datagram, .. } = plan_datagram(&raw, &track_map) else {
            panic!("音声トラックの datagram は配送対象になること");
        };
        assert_eq!(
            datagram_object_location(&datagram),
            loc(GROUP_ID, OBJECT_ID),
            "datagram の位置はヘッダの Group ID と Object ID であること"
        );

        // subgroup stream は Group ID を Subgroup Header が持ち、Object ID はデルタ解決済みである。
        // 実エンコーダで組み立てたバイト列を実デコーダで復号した値を使う
        let header = SubgroupHeader {
            track_alias: AUDIO_ALIAS,
            group_id: GROUP_ID,
            subgroup_id: SubgroupIdMode::Zero,
            publisher_priority: Some(128),
            has_properties: false,
            end_of_group: false,
            first_object: true,
        };
        let mut bytes = header.encode();
        let object = SubgroupObject {
            object_id_delta: OBJECT_ID,
            payload_length: 1,
            status: None,
        };
        object
            .encode(false, None, &mut bytes)
            .expect("SUBGROUP_OBJECT を encode できること");
        bytes.extend_from_slice(b"x");
        let mut decoder = SubgroupStreamDecoder::new();
        decoder.push(&bytes);
        let decoded_header = decoder
            .try_decode_header()
            .expect("SUBGROUP_HEADER を decode できること")
            .expect("SUBGROUP_HEADER が揃っていること");
        let decoded_object = decoder
            .try_decode_object()
            .expect("SUBGROUP_OBJECT を decode できること")
            .expect("SUBGROUP_OBJECT が揃っていること");
        assert_eq!(
            subgroup_object_location(&decoded_header, &decoded_object),
            loc(GROUP_ID, OBJECT_ID),
            "subgroup stream の位置は Header の Group ID と解決済みの Object ID であること"
        );

        // FETCH の応答は Group をまたぐため、位置は Object ごとに届く。
        // 同じく実エンコーダと実デコーダを通す
        let mut encoder = FetchStreamEncoder::new(1);
        let mut bytes = encoder.encode_header();
        encoder
            .encode_object(
                &FetchObjectInput {
                    group_id: GROUP_ID,
                    subgroup_id: 3,
                    object_id: OBJECT_ID,
                    publisher_priority: 128,
                    has_properties: false,
                    is_datagram_origin: true,
                    payload_length: 1,
                },
                None,
                &mut bytes,
            )
            .expect("FETCH の Object を encode できること");
        bytes.extend_from_slice(b"x");
        let mut decoder = FetchStreamDecoder::new();
        decoder.push(&bytes);
        decoder
            .try_decode_header()
            .expect("FETCH_HEADER を decode できること")
            .expect("FETCH_HEADER が揃っていること");
        let DecodedFetchEntry::Object(decoded_object) = decoder
            .try_decode_entry()
            .expect("FETCH の Object を decode できること")
            .expect("FETCH の Object が揃っていること")
        else {
            panic!("FETCH の応答の先頭は Object であること");
        };
        assert_eq!(
            fetch_object_location(&decoded_object),
            loc(GROUP_ID, OBJECT_ID),
            "FETCH の応答の位置は Object ごとの Group ID と Object ID であること"
        );
    }

    /// 境界より後の live の音声が並び替わって届いたときも、TIMESTAMP が巻き戻らないこと
    ///
    /// 境界の判定は購読より前の音声だけを落とすため、境界より後の並び替え
    /// (draft-ietf-moq-transport-22 §2.1.2 (Object States) / §2.3.1 (Group IDs)) は
    /// TIMESTAMP の判定が防ぐ。GOAWAY の移行直後などに起こりうる。
    #[tokio::test]
    async fn audio_delivery_drops_reordered_live_objects_after_the_boundary() {
        let packet = encode_opus_silence(48_000, 1);
        let (audio_tx, audio_rx) = std::sync::mpsc::channel();
        let audio_config_handled = std::sync::atomic::AtomicBool::new(false);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(Some(loc(100, 0))));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_handled,
            &termination_tx,
            &catch_up,
            &delivery_order,
            None,
        );
        let mut decoder =
            AudioDecoder::build("opus", 48_000, 1).expect("Opus デコーダを生成できること");

        let base_us = crate::wall_clock_us();
        let pts_us = |index: i64| base_us + index * 20_000;

        // 1. 境界より後の live の音声を送出する
        assert!(
            deliver_audio_at(
                &delivery,
                &mut decoder,
                Some(loc(100, 1)),
                pts_us(5),
                &packet
            )
            .await,
            "live の音声は配送を継続すること"
        );
        // 2. 境界より後の live の音声が、古い TIMESTAMP で届く (並び替え)
        assert!(
            deliver_audio_at(
                &delivery,
                &mut decoder,
                Some(loc(100, 2)),
                pts_us(3),
                &packet
            )
            .await,
            "並び替わった音声でも配送を継続すること"
        );
        // 3. live の音声が続く
        assert!(
            deliver_audio_at(
                &delivery,
                &mut decoder,
                Some(loc(100, 3)),
                pts_us(6),
                &packet
            )
            .await,
            "live の音声は配送を継続すること"
        );

        assert!(
            termination_rx.try_recv().is_err(),
            "書式違反が無いためセッション終了は依頼されないこと"
        );
        let sent: Vec<i64> = std::iter::from_fn(|| audio_rx.try_recv().ok())
            .map(|frame| frame.pts_us)
            .collect();
        assert_eq!(
            sent,
            vec![pts_us(5), pts_us(6)],
            "境界より後に並び替わって届いた古い音声は送出しないこと (送出順={sent:?})"
        );
        assert_eq!(
            delivery_order
                .lock()
                .expect("audio delivery order lock is not poisoned")
                .last_pts_us(),
            Some(pts_us(6)),
            "送出しなかった音声で基準を戻さないこと"
        );
        // 並び替わって届いた古い 1 件を stale の累計に残す
        let counters = delivery_order
            .lock()
            .expect("audio delivery order lock is not poisoned")
            .counters();
        assert_eq!(
            counters.stale_objects, 1,
            "並び替わって届いた古い音声を累計に数えること"
        );
        assert_eq!(
            counters.duplicate_objects, 0,
            "位置が異なるため重複では数えないこと"
        );
        assert_eq!(
            counters.before_boundary_objects, 0,
            "境界より後だけを届けるため境界以下では数えないこと"
        );
    }

    /// 音声だけの購読かどうかの判定
    ///
    /// 映像トラックを購読しない構成で音声だけが有効なときだけ true になる。プレイヤーは
    /// このときだけ到着基準の再生の遅れに下限を足し、目標の時刻を守らない。
    #[test]
    fn audio_only_subscription_requires_audio_without_video() {
        assert!(
            is_audio_only_subscription(false, true),
            "映像を購読せず音声だけを購読するときは音声だけの再生であること"
        );
        assert!(
            !is_audio_only_subscription(true, true),
            "映像も購読しているときは音声だけの再生ではないこと"
        );
        assert!(
            !is_audio_only_subscription(false, false),
            "何も購読していないときは音声だけの再生ではないこと"
        );
        assert!(
            !is_audio_only_subscription(true, false),
            "映像だけを購読しているときは音声だけの再生ではないこと"
        );
    }
}
