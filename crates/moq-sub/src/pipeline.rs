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

use crate::group_switch::{GroupSwitchHandle, SharedGroupSwitchGate};
use bytes::Bytes;
use shiguredo_moqt::error::{MessageError, STREAM_CANCELLED};
use shiguredo_moqt::loc::LocProperties;
// 抽出関数は LocProperties の accessor 経由で値を読むため、pipeline 本体ではプロパティ ID と
// 値型を使わない。テストのフィクスチャ構築だけで使う。
#[cfg(test)]
use shiguredo_moqt::loc::{
    LocPropertyValue, PROP_AUDIO_CONFIG, PROP_AUDIO_LEVEL, PROP_TIMESCALE, PROP_TIMESTAMP,
    PROP_VIDEO_CONFIG, PROP_VIDEO_FRAME_MARKING,
};
use shiguredo_moqt::pending_subgroup_buffer::{
    DEFAULT_PENDING_SUBGROUP_BUFFER_OPTIONS, PendingNotifyReason, PendingSubgroupBuffer,
};
use shiguredo_moqt::playout::catch_up::{AudioReceiveCatchUp, AudioReceiveCatchUpObservation};
use shiguredo_moqt::session::types::{
    DatagramAcceptance, RequestStreamEnd, SessionError, TrackDataAcceptance,
};
use shiguredo_moqt::stream::datagram::ObjectDatagram;
use shiguredo_moqt::stream::subgroup::SubgroupHeader;
use shiguredo_moqt::video_decode_order::{
    VideoDecodeOrder, VideoObjectAdmission, VideoObjectPosition, prior_object_id_gap_of,
};
use shiguredo_moqt::{
    message::ControlMessage, message::Publish, message::common::Location,
    message::common::TrackNamespace, message_parameter::LocationFilter,
    message_parameter::MessageParameter, message_parameter::MessageParameterValue,
    message_parameter::MessageParameters, message_parameter::PARAM_FILL_PARAMETERS,
    message_parameter::PARAM_LOCATION_FILTER, message_parameter::PARAM_NEW_GROUP_REQUEST,
    msf::MSF_CATALOG_TRACK_NAME, msf::MsfTrack, name::parse_namespace, name::serialize_namespace,
    name::serialize_track_name, session::types::DataStreamId, session::types::SessionEvent,
    stream::decoder::DecodedFetchEntry, stream::decoder::DecodedSubgroupObject,
    stream::decoder::FetchStreamDecoder, stream::decoder::SubgroupStreamDecoder,
    stream::fetch::FetchHeader,
};

use crate::catalog;
use crate::cli::Config;
use crate::decoder::{self, AudioDecoder, DecodedAudioFrame, DecodedVideoFrame, TimestampKind};
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

/// 購読の Largest Object から遡って映像の FETCH で要求する Group 数
///
/// 途中参加した購読者は購読より前に publish された Object を受け取れない。SUBSCRIBE_OK が
/// 運んだ Largest Object を基準に直近の Group を FETCH で埋める
/// (draft-ietf-moq-transport-23 §3.5.1 (Dynamically Starting New Groups) が示す
/// 「SUBSCRIBE で Largest Object を観測し、Group の先頭まで FETCH で遡る」パターン)。
/// 映像はこの範囲のキーフレームから復号を始められる。音声の明示 FETCH は発行しないため、
/// この値を使うのは映像だけである。
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
/// 音声は 1 Object = 1 Group で届き (draft-ietf-moq-loc-05 §4.1 (Application with one audio
/// track))、購読より前に publish された Group は relay の replay で届きうる。購読の境界は
/// `SUBSCRIBE_OK` が運んだ Largest Object であり (draft-ietf-moq-transport-23 §3.1.4
/// (Largest Object) / §9.19.17 (LARGEST OBJECT Parameter))、境界以下は購読より前の配信で
/// ある。音声の明示 FETCH は発行しないが、境界の判定は relay の replay への防御として残す
/// (将来 fill を入れる場合は、fill range が Largest Object を超えず (draft-ietf-moq-transport-23
/// §3.4 (Fill Semantics))、起動時の購読では fill で届く Object がすべて境界以下になるため、
/// 境界の規則を同時に見直す必要がある)。
///
/// 判定は 3 段階である。
///
/// - 直近に届いた Object と同じ識別子の 2 回目は捨てる ([`Self::is_duplicate`])。datagram は
///   再送されない (RFC 9221 §3) ため、publisher が同じ Object を n 回送ることがある。
/// - 購読の境界 (`SUBSCRIBE_OK` の Largest Object) 以下は購読より前に publish された分で
///   あり、送出しない ([`Self::is_before_boundary`])。境界の位置の Object は live と replay の
///   両方で届きうるが、どちらも境界以下であるため二重に送出されない。moqt-js の CatchUpGate
///   が `SUBSCRIBE_OK` の `LARGEST_OBJECT` を境界に cache の分を再生しない扱いと同じである。
/// - 境界より後は、送出済みの TIMESTAMP より古い Object を送出しない ([`Self::admit`])。
///   live の datagram は並び替わって届きうる (draft-ietf-moq-transport-23 §2.2 (Subgroups) /
///   §2.3.1 (Group IDs)) ため、境界とは別の防御として持つ。
///
/// どちらの判定も位置と TIMESTAMP だけで決まり、時計も I/O も扱わない。購読ごとに 1 つ持ち、
/// 音声の配送経路 (datagram / subgroup stream) で共有する。落とした Object の
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
    /// して扱う (draft-ietf-moq-transport-23 §9.19.17 (LARGEST OBJECT Parameter) は Object が
    /// publish されていれば MUST 含むと定めるため、省略は未 publish を意味する)。
    fn set_boundary(&mut self, boundary: Option<Location>) {
        self.boundary = boundary;
    }

    /// session 移行のために購読の境界だけを更新する
    ///
    /// 旧 session で最後に受信した Object の Location を新しい境界にする。移行先の購読は
    /// その位置を Location Filter の Absolute Start (0x02) にして張り直すため
    /// (draft-ietf-moq-transport-23 §3.3.1 (Location Filters) / §9.19.9 (LOCATION FILTER
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

/// 映像 Object を表示経路へ送出してよいかを購読の境界で決める状態機械
///
/// 音声の [`AudioDeliveryOrder`] と同じく、購読より前に publish された映像 Object (relay の
/// replay や FETCH の応答で届く) は表示しない。購読の境界は `SUBSCRIBE_OK` が運んだ Largest
/// Object であり (draft-ietf-moq-transport-23 §3.1.4 (Largest Object) / §9.19.17 (LARGEST
/// OBJECT Parameter))、境界以下は購読より前の配信である。境界の位置の Object は live と
/// FETCH の両方で届きうるため、どちらも境界以下として落とし二重送出を防ぐ。
///
/// 映像は音声と違い、境界以下でも復号は行う。境界以降のフレームが境界以下のフレームを参照
/// しうるためである (moqt-js の CatchUpGate と同じ扱い)。判定は復号の後、`frame_tx` への
/// 送出前に行う。
///
/// 判定は Location の比較だけで決まり、時計も I/O も扱わない。live の Subgroup stream と
/// FETCH 応答で同じ状態を共有する。`Location` の順序は (group_id, object_id) の辞書順で
/// ある (§8.2 (Location Structure))。Group ID が時間とともに増える track を前提とする
/// (§2.3.1 (Group IDs) は original publisher に SHOULD で求める。比較できない publisher の
/// track では本 example の境界判定は意味を持たない)。
///
/// 境界は後退させない ([`Self::advance_boundary`])。再購読では「最後に受信した Location」を
/// 候補にするが、replay の受信途中や映像を 1 つも受信していない間は候補が現在の境界より
/// 前になったり `None` になったりするため、そのまま設定すると購読より前の Object が再送
/// されたときに表示経路へ戻る。進めた境界は「その位置まで受信済み」を意味するため、境界
/// より前で未受信の Object が後から届いても表示しない (out-of-order の取りこぼしは許容する。
/// draft-ietf-moq-transport-23 §2.2 (Subgroups))。
#[derive(Debug)]
struct VideoDeliveryOrder {
    /// 購読の境界 (`SUBSCRIBE_OK` の Largest Object、再購読では最後に受信した Location)。
    /// `None` は境界が無く、すべて live として扱う (購読の時点で Object が publish されて
    /// いない場合)
    boundary: Option<Location>,
    /// 購読の境界以下として送出しなかった Object の累計 (原因の切り分け用)
    ///
    /// 境界判定まで到達した Object だけを数える。復号器が無い経路 (再生しない指定、表示待ち
    /// 超過) と、[`VideoDecodeOrder`] が復号しない Object は含まない。累計が 1 種類だけなのは、
    /// 重複と並び替えの判定を [`VideoDecodeOrder`] が担うためである。境界以下を落とすのは
    /// 正常な動作 (relay の replay や FETCH の応答) でも起きるため、1 件ごとの debug ログ
    /// だけでは傾向を掴めない。累計を 100 ms の tick で比較し、増えたときだけ info で出す
    /// ([`log_video_boundary_counter`] を参照)。
    before_boundary_objects: u64,
}

impl VideoDeliveryOrder {
    /// 購読の境界を指定して作る (`None` は Largest Object が告知されなかった場合)
    ///
    /// 初期化専用である。作成後の境界の更新は [`Self::advance_boundary`] だけを使う
    /// (単調性を迂回して境界を後退させないため)。
    fn new(boundary: Option<Location>) -> Self {
        Self {
            boundary,
            before_boundary_objects: 0,
        }
    }

    /// 購読の境界を、現在の境界より後ろの位置にだけ進める
    ///
    /// `candidate` が `None` の場合は何もしない (旧 session で映像 Object を 1 つも受信して
    /// いない場合の `None` は候補が無いことを意味し、既に知っている境界を消す理由にはなら
    /// ない)。後退させない理由は型の doc を参照。
    fn advance_boundary(&mut self, candidate: Option<Location>) {
        let Some(candidate) = candidate else {
            return;
        };
        self.boundary = Some(match self.boundary {
            Some(current) => current.max(candidate),
            None => candidate,
        });
    }

    /// `location` が購読の境界以下かを判定する
    ///
    /// 境界が無い場合は比較できないため `false` (live 扱い) を返す。境界以下だったときは、
    /// 原因の切り分け用の累計 (`before_boundary_objects`) を進める。音声と違い位置を
    /// `Option` で受けないのは、映像の配送経路 (live の Subgroup stream と FETCH 応答) が
    /// 常に Location を持つためである。
    fn is_before_boundary(&mut self, location: Location) -> bool {
        let Some(boundary) = self.boundary else {
            return false;
        };
        // `Location` の順序は (group_id, object_id) の辞書順である (§8.2)
        if location <= boundary {
            self.before_boundary_objects += 1;
            return true;
        }
        false
    }

    /// 現在の購読の境界を返す (ログ用)
    fn boundary(&self) -> Option<Location> {
        self.boundary
    }

    /// 購読の境界以下として送出しなかった Object の累計を返す
    fn before_boundary_objects(&self) -> u64 {
        self.before_boundary_objects
    }
}

/// 映像の購読境界以下として送出しなかった Object の累計が、前回のログから増えていれば info で出す
///
/// 累計が増えたときだけ info に出す。`previous` は前回ログを出した時点の累計で、増えていない
/// ときは更新しない (同じ内容を出し続けてログを埋もれさせないため)。累計の意味は
/// [`VideoDeliveryOrder::before_boundary_objects`] を参照。
fn log_video_boundary_counter(previous: &mut u64, current: u64) {
    if current == *previous {
        return;
    }
    tracing::info!("Video delivery counters: before_boundary={current}");
    *previous = current;
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
/// OBJECT_DATAGRAM のヘッダが位置を持つ (draft-ietf-moq-transport-23 §11.2.1)。
fn datagram_object_location(datagram: &ObjectDatagram) -> Location {
    Location {
        group_id: datagram.group_id,
        object_id: datagram.object_id,
    }
}

/// subgroup stream で届いた音声 Object の位置
///
/// Group ID は Subgroup Header が持ち (draft-ietf-moq-transport-23 §11.3.1)、Object ID は
/// Subgroup 内のデルタが解決済みの値を使う。呼び出し側が別の Group ID を渡せないよう、
/// Header そのものを受け取る。
fn subgroup_object_location(header: &SubgroupHeader, object: &DecodedSubgroupObject) -> Location {
    Location {
        group_id: header.group_id,
        object_id: object.object_id,
    }
}

/// 音声 Object を再生経路へ流すための共有状態
///
/// 音声 Object は subgroup stream と datagram の両方で届く (draft-ietf-moq-loc-05 §4.1)。
/// 経路に依存しない値をここへまとめ、重複の判定 → 購読境界の判定 → 録画 → Audio Config の
/// 検証 → 送出順の判定 → デコード → catch-up の観測 → デコード済みフレームの送出という順序が
/// 経路ごとに食い違わないようにする。重複と境界以下は Audio Config の検証だけを行い、復号・
/// 送出・録画・catch-up の観測をしない。
struct AudioDelivery<'a> {
    /// デコード済み音声フレームの送り先
    audio_tx: &'a std::sync::mpsc::Sender<DecodedAudioFrame>,
    /// Audio Config の処理 (Opus は検証、AAC は非解釈) を試みた session の世代
    /// (購読で 1 つ共有する)
    ///
    /// 移行のたびに main ループが session の世代を進める。自分が読む session の世代が
    /// これより新しければ、その session で最初に届いた Audio Config として処理する。
    audio_config_validated_generation: &'a std::sync::atomic::AtomicU64,
    /// この配送経路が読む session の世代
    session_generation: u64,
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
    /// 購読より前に publish された Group は relay の replay で届きうる。`SUBSCRIBE_OK` の
    /// Largest Object 以下の音声は送出も録画も復号もしない (境界の位置の Object が live と
    /// replay の両方で届いても二重にならない)。同じ識別子の Object の 2 回目 (datagram の
    /// 冗長化による copy) も落とす。境界より後は、送出済みの TIMESTAMP より古い音声を落とし、
    /// 再生経路の TIMESTAMP が巻き戻らないようにする。状態は datagram 経路と
    /// subgroup stream 経路で共有する。
    delivery_order: &'a std::sync::Mutex<AudioDeliveryOrder>,
}

/// この Audio Config を処理すべきか
///
/// 処理を試みた session の世代を単調非減少に記録し、自分の session の世代がそれより新しければ
/// true を返す。`fetch_max` は更新前の値を返すため、移行前の session の Object が後から
/// 届いても、移行後の session の記録を巻き戻さない。
fn claim_audio_config_generation(
    audio_config_validated_generation: &std::sync::atomic::AtomicU64,
    session_generation: u64,
) -> bool {
    audio_config_validated_generation.fetch_max(session_generation, Ordering::AcqRel)
        < session_generation
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
    /// セッション終了を依頼したか、デコード済みフレームの送り先が閉じたことを表す。malformed
    /// Track (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1
    /// (Malformed Tracks) 条件 9) はセッション終了を依頼せず、この Object だけを捨てて `true`
    /// を返す ([`skip_object_on_properties_error`])。
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
        // 書式違反は配送の有無や検証済み世代に関わらず毎 Object で検出するため、
        // ここでは常に抽出する (検証は下の世代判定で session ごとに 1 回に絞る)
        let audio_config = match extract_audio_config(properties_bytes) {
            Ok(config) => config,
            Err(e) => return skip_object_on_properties_error(self.termination_tx, &e),
        };
        let (timestamp, timescale) = match extract_timestamp_timescale(properties_bytes) {
            Ok(ts) => ts,
            Err(e) => return skip_object_on_properties_error(self.termination_tx, &e),
        };
        // TIMESTAMP の種類を求める。壁時計の TIMESTAMP を持つ音だけが、受信側の時間軸へ
        // 観測して表示時刻を決められる (TIMESCALE を持つ音はメディア時刻である)
        let timestamp_kind = timestamp_kind_of(timestamp, timescale);
        // 重複の判定と購読の境界の判定は録画より前に行う。境界以下の音声は購読より前に
        // publish された分であり、送出も録画も復号もしない (境界の位置の Object が live と
        // relay の replay の両方で届いても、どちらもここで落ちるため二重にならない)。
        // Audio Config の検証だけは Properties から行うため、この時点では return しない。
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
        // Audio Config (OpusHead) を含む Object を受信した session ごとに、最初の 1 回だけ
        // 処理する (途中参加で OpusHead を受信しない場合はスキップされる)。subgroup stream と
        // datagram のどちらで届いても 1 回で済むよう、状態は購読で共有する。
        // 処理済みの世代と自分の session の世代を比べるため、移行前の session の Object が
        // 後から届いても、移行後の session の処理状態を巻き戻さない。
        if let Some(config_bytes) = audio_config.as_deref()
            && claim_audio_config_generation(
                self.audio_config_validated_generation,
                self.session_generation,
            )
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
            // 境界以下の音声は復号しない。復号器は datagram と subgroup stream で 1 つを
            // 共有しており、live の音声を復号したあとに古い音声で状態を進めると、次に
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
            // 境界より後の live の音声が並び替わって届いた場合である (draft-ietf-moq-transport-23
            // §2.2 (Subgroups) / §2.3.1 (Group IDs))。購読より前に publish された音声は
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
            let lag_us = now_us.saturating_sub(pts_us);
            let input_lag_us = receive_input_lag_us(timestamp, timescale, arrival_us, pts_us);
            // 端末間遅延 (publisher が載せた壁時計の TIMESTAMP から受信まで) を 5 秒に
            // 1 回 debug で出す。relay が peer 窓のぶんだけ送信バッファへデータを溜めて
            // いるとこの値が伸びるため、受信側が広告する窓の大きさの影響を見る診断である。
            // TIMESTAMP が壁時計でない音では測れない (input_lag_us が None)
            if let Some(lag_us) = input_lag_us {
                log_audio_end_to_end_lag(arrival_us, lag_us);
            }
            // ロックは await をまたがない (観測と判定だけの同期処理)
            let (decision, stats) = {
                let mut catch_up = self
                    .catch_up
                    .lock()
                    .expect("audio catch up lock is not poisoned");
                let decision = catch_up.observe(AudioReceiveCatchUpObservation {
                    now_us,
                    lag_us,
                    input_lag_us,
                    duration_us: pcm_duration_us(pcm.len(), usize::from(channels), sample_rate),
                    timestamp_us: pts_us,
                });
                (decision, catch_up.snapshot())
            };
            if decision.rebuild_decoder {
                tracing::info!(
                    "Rebuilding the audio decoder to catch up (lag {}ms, input_lag {}ms, decode_lag {}ms, starts {})",
                    lag_us / 1_000,
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
            timestamp_kind,
        };
        self.audio_tx.send(frame).is_ok()
    }
}

/// PCM (S16 interleaved) の長さをマイクロ秒で返す
///
/// チャンネル数とサンプルレートが 0 のときは 0 を返す。端数は四捨五入する
/// (moqt-js の `numberOfFrames / sampleRate * 1_000_000` を `Math.round` するのと同じ向き)。
fn pcm_duration_us(pcm_len: usize, channels: usize, sample_rate: u32) -> i64 {
    if channels == 0 || sample_rate == 0 {
        return 0;
    }
    let frames = pcm_len / channels;
    i64::try_from(frames)
        .unwrap_or(i64::MAX)
        .saturating_mul(1_000_000)
        .saturating_add(i64::from(sample_rate) / 2)
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

/// request 単位移行タスクが main ループへイベントを渡すチャネルの容量
///
/// 流れるのは移行の結果と、移行後に side session で観測した session イベントだけであり、
/// まとめて届くことはない。詰まった場合は side session の受信を止めて main ループの処理を
/// 待つため、小さな容量で十分である。
const REQUEST_MIGRATION_EVENT_CHANNEL_SIZE: usize = 32;

/// 同一 URL の side session へ再発行の要求を送るチャネルの容量
///
/// side session は同じ URL を指す request の移行を相乗りさせるため、要求が続けて届く
/// ことがある。処理は 1 件ずつであり、詰まった場合は main ループが送信で待つ。少数の
/// 余裕を持たせる。
const SIDE_SESSION_COMMAND_CHANNEL_SIZE: usize = 8;

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
use tokio_moq::error::{
    PUBLISH_DONE_GOING_AWAY, PUBLISH_DONE_TRACK_ENDED, data_stream_termination_code,
    normalize_publish_done_code, session_error_code,
};
use tokio_moq::moqt_client::{
    ClientEvent, DataPlaneHandle, FetchHeaderRegistration, FetchHeaderResolution, MoqtClient,
    StreamRead, SubscribeResult,
};
use tokio_moq::same_session_url;
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
/// draft-ietf-moq-transport-23 §6.4.2.3 (Request Cancellation and Rejection) により
/// STOP_SENDING は拒否される) と、自側で終了コード付きに閉じた場合 (session が
/// Closing / Closed で状態機械に拒否される) は送らない。
/// GOAWAY は subscription state に影響しないため、この条件には含めない。
fn should_stop_sending(peer_ended: bool, session_terminated: bool) -> bool {
    !peer_ended && !session_terminated
}

/// shutdown 時の STOP_SENDING 対象を判定する
///
/// 対象は「まだ active な購読」のみである。PUBLISH_DONE を受けた購読
/// (`active_requests` から外れた request id) と、`video_request_id` などを参照できても
/// 終端済みで active でない購読は、`Session::stop_sending` が Terminated を拒否するため
/// 対象外にする (0100 型の警告を再発させない)。全部 media track が恒久終了して
/// `peer_ended` を立てる終了経路では `should_stop_sending` が偽になるため、外側の
/// 全体ゲートで送らない。残った catalog は session close で終わる。
fn stop_sending_targets(
    video_request_id: Option<u64>,
    audio_request_id: Option<u64>,
    catalog_request_id: Option<u64>,
    active_requests: &HashSet<u64>,
) -> Vec<(TrackKind, u64)> {
    let mut targets = Vec::new();
    if let Some(rid) = video_request_id
        && active_requests.contains(&rid)
    {
        targets.push((TrackKind::Video, rid));
    }
    if let Some(rid) = audio_request_id
        && active_requests.contains(&rid)
    {
        targets.push((TrackKind::Audio, rid));
    }
    if let Some(rid) = catalog_request_id
        && active_requests.contains(&rid)
    {
        targets.push((TrackKind::Catalog, rid));
    }
    targets
}

/// 全 media track が恒久的に終了し、移行待ちも無いか
///
/// catalog だけが残っていても media が無ければパイプラインを終える (catalog の終了では
/// media が終了した扱いにしない)。media の移行待ち (`migration_pending_tracks` の Video /
/// Audio) がある間は、移行先で再開する可能性があるため終えない。catalog が GoingAway で
/// `migration_pending_tracks` に載っていても、この判定には影響しない。
///
/// 購読していない track (`subscribe_video` / `subscribe_audio` が偽) は、終了を待たずに
/// 終了済みとして扱う。
fn all_media_tracks_ended(
    subscribe_video: bool,
    subscribe_audio: bool,
    permanently_ended_tracks: &HashSet<TrackKind>,
    migration_pending_tracks: &HashSet<TrackKind>,
) -> bool {
    let video_done = permanently_ended_tracks.contains(&TrackKind::Video);
    let audio_done = permanently_ended_tracks.contains(&TrackKind::Audio);
    let media_done = (video_done || !subscribe_video) && (audio_done || !subscribe_audio);
    let media_migration_pending = migration_pending_tracks
        .iter()
        .any(|kind| matches!(kind, TrackKind::Video | TrackKind::Audio));
    media_done && !media_migration_pending
}

/// 正常終了の後始末で GOAWAY と正常 close (`close(0, "")`) を送るか
///
/// 自側で終了コード付きに閉じた場合は送らない (閉じた session への送信は拒否され、
/// 誤解を招く警告ログになる)。
fn should_close_gracefully(session_terminated: bool) -> bool {
    !session_terminated
}

/// LOC Properties の書式違反を受けてセッション終了を main ループへ依頼する
///
/// draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) は書式違反の受信を
/// セッション終了の対象とする MUST を定める。Properties の解釈経路 (`deliver` 系) は
/// [`request_session_termination_for_decode_error`] の分類対象外であり、
/// `MessageError::data_plane_session_termination_code` で終了コードを決める
/// (Object の直列化の破損は MALFORMED_OBJECT / MALFORMED_KEY_VALUE などに写る)。
///
/// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1 (Malformed
/// Tracks) 条件 9 の malformed Track はここへ渡さない ([`skip_object_on_properties_error`]
/// が Object を捨てるだけに留める)。セッション終了を依頼された場合、呼び出し側はこの後
/// その stream の処理を打ち切る。transport close を送出できるのは `MoqtClient` を所有する
/// main ループだけであり (`DataPlaneHandle` は `Session` しか共有していない)、stream task から
/// 直接 `Session::close` を呼ぶと main ループが `SessionEvent::CloseSession` を観測できない
/// まま accept 分岐が接続クローズを拾い、`Session closed: ...` のログが落ちる。そのため終了
/// コードと理由を渡し、close は I/O 層である main ループが行う。
///
/// `try_send` が失敗するのは、別の終了依頼が既に積まれている (容量 1 のため理由を問わず
/// 2 件目は入らない) か、main ループが受信ループを終えて受信側を drop した場合である。
/// 複数の stream が同時に失敗した場合は最初の 1 件のコードで閉じる。受信ループの終了後は
/// join の前に 1 件だけ回収し、それ以降に届いた依頼は破棄される。
fn request_session_termination(
    termination_tx: &tokio::sync::mpsc::Sender<SessionError>,
    error: &MessageError,
) {
    // データプレーンの decode エラーは Object のシリアライズ単位で分類する
    // (draft-ietf-moq-transport-23 §12.2 (Session Termination Codes))。
    let code = error
        .data_plane_session_termination_code()
        .unwrap_or(shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION);
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

/// Properties の再 decode 失敗を分類し、この Object を捨てて配送を続けるかを返す
///
/// 返り値は「この Object だけを捨てて配送を続けるか」である。
///
/// - `MalformedTrack`: draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) /
///   §12.1 (Malformed Tracks) 条件 9 の malformed Track は、セッション終了ではなく同じ
///   Track の購読・fetch の cancel を求める。Session の data plane が同じ Object Properties
///   を decode して既に cancel しているため、ここでセッションを閉じると無関係な Track まで
///   巻き込む。セッション終了は依頼せず、`true` を返してこの Object を捨てる
/// - それ以外: draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) の書式違反で
///   あり、[`request_session_termination`] へ渡して `false` を返す (配送を打ち切る)
///
/// どちらの場合もアプリへは配送しない (Properties を解釈できない Object は正しい表示時刻や
/// コーデック設定を持たない)。
fn skip_object_on_properties_error(
    termination_tx: &tokio::sync::mpsc::Sender<SessionError>,
    error: &MessageError,
) -> bool {
    if let MessageError::MalformedTrack(reason) = error {
        tracing::warn!("Skipping an object of a malformed track: {reason}");
        return true;
    }
    request_session_termination(termination_tx, error);
    false
}

/// data stream の decoder が返した復号エラーを受けてセッション終了を main ループへ依頼する
///
/// draft-ietf-moq-transport-23 §11.3.1 (Subgroup Header) / §11.1.2 (Object Properties) の
/// MUST に従い、不正な header / Object / Properties はセッション終了を要求する。
/// `UnexpectedEof` と `MalformedTrack` は
/// [`tokio_moq::error::data_stream_termination_code`] が `None` を返すため依頼しない
/// (前者はデータ不足として decoder が吸収し、後者は §12.1 の cancel 対象である)。
fn request_session_termination_for_decode_error(
    termination_tx: &tokio::sync::mpsc::Sender<SessionError>,
    error: &MessageError,
) {
    let Some(code) = data_stream_termination_code(error) else {
        return;
    };
    let reason = error.reason();
    if termination_tx
        .try_send(SessionError::new(code, reason))
        .is_err()
    {
        tracing::debug!("Session termination is already requested or the main loop has stopped");
        return;
    }
    tracing::warn!("Closing session on data stream decode error: {code:#x} {reason}");
}

/// peer request を拒否するときの REQUEST_ERROR コードと理由を返す
///
/// draft-ietf-moq-transport-23 §9.8 (PUBLISH): "A subscriber receiving a PUBLISH for a Track
/// it does not wish to receive SHOULD send PUBLISH_ERROR with error code UNINTERESTED"。
/// PUBLISH_ERROR は §1.5 (Response Message Naming) の shorthand で wire 上は REQUEST_ERROR
/// である。UNINTERESTED (0x20) は PUBLISH / PUBLISH_NAMESPACE への応答用 (§12.3) であり、
/// それ以外の request は従来どおり NOT_SUPPORTED とする。
fn request_rejection(message: &ControlMessage) -> (u64, &'static str) {
    match message {
        ControlMessage::Publish(_) => (
            shiguredo_moqt::error::REQUEST_UNINTERESTED,
            "subscriber is not interested in this track",
        ),
        _ => (
            shiguredo_moqt::error::REQUEST_NOT_SUPPORTED,
            "subscriber does not serve requests",
        ),
    }
}

/// SUBSCRIBE_TRACKS の結果として届いた PUBLISH を受理するか
///
/// draft-ietf-moq-transport-23 §9.19.21 (SUBSCRIBE_TRACKS_ID Parameter) は、PUBLISH が
/// 起因する namespace 購読の Request ID を運ぶ。自側が送った SUBSCRIBE_TRACKS の Request ID と
/// 一致する場合だけ受理する。SUBSCRIBE_TRACKS を送っていない状態や、値が一致しない PUBLISH は
/// 従来どおり不要な PUBLISH として拒否する。
fn accepts_subscribe_tracks_publish(publish: &Publish, tracks_request_id: Option<u64>) -> bool {
    match tracks_request_id {
        Some(request_id) => publish.parameters.subscribe_tracks_id() == Some(request_id),
        None => false,
    }
}

/// 既に SUBSCRIBE している Full Track Name の PUBLISH か
///
/// `subscribed` は catalog / 映像 / 音声で購読している Full Track Name の一覧である。
/// 同じ Track を SUBSCRIBE と PUBLISH の両方で購読すると同じ Object を 2 系統で受け取るため、
/// 重複を判別して Object を捨てるために使う。
fn is_already_subscribed_publish(
    publish: &Publish,
    subscribed: &[(TrackNamespace, Vec<u8>)],
) -> bool {
    subscribed.iter().any(|(namespace, track_name)| {
        namespace == &publish.track_namespace && track_name == &publish.track_name
    })
}

/// session で共有する保留バッファ
///
/// Track Alias 未確立の Subgroup ストリームを保持するバッファは、session (run) スコープで
/// 1 つ共有する。stream task ごとに持つと per-session の上限が per-stream の上限としてしか
/// 働かないためである。各 stream task は自分の entry の識別子を保持し、
/// [`shiguredo_moqt::pending_subgroup_buffer::PendingSubgroupBuffer::take_ready_for`] で
/// 自分の通知だけを引き取る。
type SharedPendingSubgroupBuffer = std::sync::Arc<tokio::sync::Mutex<PendingSubgroupBuffer>>;

/// data stream の同時処理枠
///
/// `MAX_CONCURRENT_STREAMS` 本を上限に stream task が 1 枠ずつ保持する。CPU を使わない
/// 待機 (PUBLISH の到着を待つ fill の保留) の間は [`StreamSlot::release`] で枠を手放し、
/// 待機が明けたら [`StreamSlot::reacquire`] で取り直す。枠を保持したまま待つと、保留が
/// 上限本そろったときに他の stream の処理が保留の期限まで止まるためである。手放している
/// 間もセマフォの総数は変わらないので、同時に処理する stream 数は上限を超えない。
struct StreamSlot {
    /// 枠の総数を管理するセマフォ (session スコープで共有)
    semaphore: std::sync::Arc<tokio::sync::Semaphore>,
    /// 現在保持している枠 (手放している間は `None`)
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl StreamSlot {
    fn new(
        semaphore: std::sync::Arc<tokio::sync::Semaphore>,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Self {
        Self {
            semaphore,
            permit: Some(permit),
        }
    }

    /// 枠を手放す
    fn release(&mut self) {
        self.permit = None;
    }

    /// 手放した枠を取り直す
    async fn reacquire(&mut self) {
        if self.permit.is_some() {
            return;
        }
        // セマフォは session の生存中に close されないため、取得の失敗は起こらない
        self.permit = Some(
            std::sync::Arc::clone(&self.semaphore)
                .acquire_owned()
                .await
                .expect("stream slot semaphore must not be closed"),
        );
    }
}

/// FETCH 応答の配送先の対応表
///
/// 自側が発行した FETCH と、catalog 購読の fill fetch の Request ID から配送先を引く。
/// request 単位移行の side session では再発行のたびに新しい Request ID を足すため、
/// 移行タスクと複数の stream task から `Arc<Mutex<...>>` で共有する。ロックは await を
/// またがない短い処理に限る。
type SharedFetchTargets = std::sync::Arc<std::sync::Mutex<HashMap<u64, FetchTarget>>>;

/// FETCH 応答の対応表をロックする
///
/// ロックの失敗 (poison) はプログラムの不具合であり、受信を続けても正しい配送ができない
/// ため `expect` で停止する。ロックを保持したまま await しないこと。
fn lock_fetch_targets(
    fetch_targets: &SharedFetchTargets,
) -> std::sync::MutexGuard<'_, HashMap<u64, FetchTarget>> {
    fetch_targets
        .lock()
        .expect("fetch targets lock is not poisoned")
}

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
    stream_slots: &std::sync::Arc<tokio::sync::Semaphore>,
    stream_num: u64,
    track_map: &HashMap<u64, TrackKind>,
    fetch_targets: &SharedFetchTargets,
    data_plane: &DataPlaneHandle,
    frame_tx: &std::sync::mpsc::Sender<DecodedVideoFrame>,
    audio_tx: &std::sync::mpsc::Sender<DecodedAudioFrame>,
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
    audio_params: Option<(u32, u8)>,
    display_backlog: &std::sync::Arc<std::sync::atomic::AtomicI64>,
    video_decode_order: &std::sync::Arc<tokio::sync::Mutex<VideoDecodeOrder>>,
    video_delivery_order: &std::sync::Arc<std::sync::Mutex<VideoDeliveryOrder>>,
    group_switch: &std::sync::Arc<SharedGroupSwitchGate>,
    audio_decoder: Option<&std::sync::Arc<tokio::sync::Mutex<AudioDecoder>>>,
    audio_config_validated_generation: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    session_generation: u64,
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
    // 対応表は side session の再発行で書き換わるため、参照ではなく Arc を clone して
    // 配送先の判定をヘッダを読んだ時点の内容で行う
    let fetch_targets = std::sync::Arc::clone(fetch_targets);
    let data_plane = data_plane.clone();
    let frame_tx = frame_tx.clone();
    let audio_tx = audio_tx.clone();
    let codec = video_codec.map(str::to_owned);
    let audio_codec = audio_codec.map(str::to_owned);
    let backlog = std::sync::Arc::clone(display_backlog);
    let video_decode_order = std::sync::Arc::clone(video_decode_order);
    let video_delivery_order = std::sync::Arc::clone(video_delivery_order);
    // 世代は stream task を作った時点で固定する。session 移行で世代を進めた後に旧い task が
    // 呼んでも、新しい世代の判定に影響しないようにするためである
    let group_switch = GroupSwitchHandle::new(std::sync::Arc::clone(group_switch));
    let audio_decoder = audio_decoder.cloned();
    let audio_config_validated_generation =
        std::sync::Arc::clone(audio_config_validated_generation);
    let audio_catch_up = std::sync::Arc::clone(audio_catch_up);
    let audio_delivery_order = std::sync::Arc::clone(audio_delivery_order);
    let last_received_locations = std::sync::Arc::clone(last_received_locations);
    let termination_tx = termination_tx.clone();
    let catalog_tx = catalog_tx.clone();
    let pending_subgroups = std::sync::Arc::clone(pending_subgroups);
    let recorder = recorder.cloned();
    let slots = std::sync::Arc::clone(stream_slots);
    join_set.spawn(task_monitor.clone().instrument(async move {
        let mut slot = StreamSlot::new(slots, permit);
        handle_incoming_stream(
            stream,
            data_plane,
            &track_map,
            &fetch_targets,
            codec.as_deref(),
            audio_codec.as_deref(),
            audio_params,
            &frame_tx,
            &audio_tx,
            stream_num,
            &mut slot,
            backlog,
            &video_decode_order,
            &video_delivery_order,
            &group_switch,
            audio_decoder.as_ref(),
            &audio_config_validated_generation,
            session_generation,
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
        drop(slot);
    }));
}

/// トラック種別 (data stream 処理で使用)
#[derive(PartialEq, Eq, Debug, Clone, Copy, Hash)]
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
    /// SUBSCRIBE_TRACKS の結果として受理した PUBLISH の購読
    ///
    /// この example は catalog が示す映像・音声だけを再生するため、namespace prefix の
    /// 購読で増えた track の Object は再生・録画に使わない。alias の解決と stream の会計は
    /// Session に任せ、受信した Object は読み捨てる。
    Discard,
}

/// PUBLISH_DONE の status code から決める購読の disposition
///
/// draft-ietf-moq-transport-23 §12.4 (Publish Done Codes)。`TRACK_ENDED` (0x2) は
/// publisher 側 track の正常終端、`GOING_AWAY` (0x4) は session 移行に伴う終端を示し、
/// それ以外 (未知 code を含む) はエラーの終端である。未知 code は §13 (Grease) の MUST
/// により INTERNAL_ERROR 相当に正規化され、分類もエラーになる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishDoneDisposition {
    TrackEnded,
    GoingAway,
    Error,
}

/// status code から [`PublishDoneDisposition`] を決める
///
/// 呼び出し側はログ表示のために `normalize_publish_done_code` を通すが、分類結果には
/// 影響しない (未知 code はどのみちエラーになる)。本関数内では正規化しない。
fn publish_done_disposition(status_code: u64) -> PublishDoneDisposition {
    match status_code {
        PUBLISH_DONE_TRACK_ENDED => PublishDoneDisposition::TrackEnded,
        PUBLISH_DONE_GOING_AWAY => PublishDoneDisposition::GoingAway,
        _ => PublishDoneDisposition::Error,
    }
}

/// 移行先の session で張り直す購読の指定
///
/// catalog は PUBLISH_DONE で恒久終了した場合 (最後の `catalog_state` を使う場合) は
/// `None` になる。
#[derive(Clone)]
struct ResubscribePlan {
    /// catalog track の namespace (購読する場合)
    catalog_namespace: Option<TrackNamespace>,
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

/// メディア FETCH を再発行するための指定
///
/// request stream の GOAWAY は request 単位の移行を求める (draft-ietf-moq-transport-23
/// §9.2 (GOAWAY))。FETCH は同じ要求内容で発行し直す必要があるため、発行時の namespace /
/// track name とメッセージパラメータ (要求 range を含む) を Request ID とともに保持する。
/// 仕様は draft 由来であり将来変更されうる。
#[derive(Clone)]
struct FetchRequestTarget {
    /// FETCH を発行する track の指定 (namespace と track name)
    target: SubscribeTarget,
    /// FETCH に載せたメッセージパラメータ (要求 range を含む)
    parameters: MessageParameters,
}

/// 張り直した購読の結果
///
/// catalog は `plan.catalog_namespace` が `None` (PUBLISH_DONE で恒久終了した場合) の
/// 場合は `None` になり、最後の `catalog_state` を使う。
struct Resubscribed {
    /// catalog の購読 (購読する場合)
    catalog: Option<SubscribeResult>,
    /// 映像の購読 (購読する場合)
    video: Option<SubscribeResult>,
    /// 音声の購読 (購読する場合)
    audio: Option<SubscribeResult>,
    /// 移行先で発行した FETCH の対応表 (Request ID → 配送先)
    ///
    /// Request ID は session ごとに払い出されるため、旧 session の対応表は移行先では
    /// 使えない。移行に成功した時点でこの表へ差し替える
    fetch_targets: HashMap<u64, FetchTarget>,
    /// 移行先で発行した FETCH の再発行に必要な指定 (Request ID → 要求内容)
    ///
    /// 移行先の Request ID で request stream の GOAWAY が届いたときに、同じ要求を
    /// 発行し直せるようにする
    fetch_requests: HashMap<u64, FetchRequestTarget>,
}

/// GOAWAY による移行開始時の plan 組み立てと、恒久終了扱いにする終了待ち entry の列挙
///
/// `base` は起動時の購読指定である。plan は次の規則で構成する。
///
/// - `permanently_ended_tracks` に含まれる track (cleanup 完了済みの恒久終了) は
///   plan から除外する。catalog が恒久終了した場合は catalog の購読も張り直さず、
///   最後の `catalog_state` を使う (`ResubscribePlan.catalog_namespace` を `None` にする)
/// - 終了待ち entry (`pending`) の disposition も参照する。`TrackEnded` / `Error` の
///   entry は plan から除外し、返り値の `resolve_as_ended` に載せて呼び出し側が
///   `permanently_ended_tracks` へ移して entry を解消させる (移行開始から cleanup 完了まで
///   の窓で plan に載せないため)。`GoingAway` の entry は plan に含める (移行先で張り
///   直すため; base は恒久終了でない限り plan に載っている)。`GoingAway` の entry の
///   解消は移行成功時に行う
///
/// draft-ietf-moq-transport-23 §12.4 の status code による track の恒久終了を移行経路で
/// 侵入させないための規則である。
fn plan_for_migration(
    base: &ResubscribePlan,
    permanently_ended_tracks: &HashSet<TrackKind>,
    pending: &HashMap<u64, (TrackKind, PublishDoneDisposition)>,
) -> (ResubscribePlan, Vec<(u64, TrackKind)>) {
    // cleanup 完了済みの恒久終了と、終了待ち entry の TrackEnded / Error を plan から
    // 外す対象として集める
    let mut exclude_kinds: HashSet<TrackKind> = HashSet::new();
    let mut resolve_as_ended: Vec<(u64, TrackKind)> = Vec::new();
    for kind in permanently_ended_tracks {
        exclude_kinds.insert(*kind);
    }
    for (request_id, (kind, disposition)) in pending {
        // SUBSCRIBE_TRACKS の結果として受理した PUBLISH は移行先で張り直さない。
        // track の恒久終了として記録すると媒体トラックの終了判定に混ざるため、
        // 計画からは外すだけにする (移行成功時に publish_done_pending ごと破棄される)
        if *kind == TrackKind::Discard {
            continue;
        }
        match disposition {
            PublishDoneDisposition::TrackEnded | PublishDoneDisposition::Error => {
                // 新たに恒久終了扱いにする track (既に恒久終了済みなら 2 つ目以降の
                // PUBLISH_DONE で解消だけ起こる) を解消対象に載せる。cleanup 完了を
                // 待たずに plan から外す
                exclude_kinds.insert(*kind);
                resolve_as_ended.push((*request_id, *kind));
            }
            PublishDoneDisposition::GoingAway => {
                // plan に含める (base が恒久終了でなければ載る)
            }
        }
    }
    let plan = ResubscribePlan {
        catalog_namespace: if exclude_kinds.contains(&TrackKind::Catalog) {
            None
        } else {
            base.catalog_namespace.clone()
        },
        video: if exclude_kinds.contains(&TrackKind::Video) {
            None
        } else {
            base.video.clone()
        },
        audio: if exclude_kinds.contains(&TrackKind::Audio) {
            None
        } else {
            base.audio.clone()
        },
    };
    (plan, resolve_as_ended)
}

/// request stream の GOAWAY を受けた request の再発行の可否
///
/// 終端済み購読 (終了待ち entry あり / tick 解消済み) が request stream の GOAWAY を後着
/// させたときの判定を 1 か所にまとめる (`publish_done_pending` が tick で空になった後に
/// 判定しても状態が崩れないよう、`permanently_ended_tracks` も参照する)。
///
/// - `permanently_ended_tracks` に含まれる track は恒久終了したため再発行しない
///   (`None` を返す)
/// - 終了待ち entry の disposition が `TrackEnded` / `Error` の request は恒久終了のため
///   再発行しない。entry は解消しない (cleanup 経路と後続の plan 組み立ての disposition
///   参照を維持する)
/// - 終了待ち entry の disposition が `GoingAway` の request と、entry が無い request
///   (Established または tick 解消済みの移行待ち) は再発行する
fn reissue_request_target(
    kind: TrackKind,
    pending: Option<PublishDoneDisposition>,
    permanently_ended_tracks: &HashSet<TrackKind>,
) -> Option<TrackKind> {
    if permanently_ended_tracks.contains(&kind) {
        return None;
    }
    match pending {
        Some(PublishDoneDisposition::TrackEnded) | Some(PublishDoneDisposition::Error) => None,
        Some(PublishDoneDisposition::GoingAway) | None => Some(kind),
    }
}

/// request の再発行の前に STOP_SENDING を送るか
///
/// `Session::stop_sending` は Terminated を拒否するため、PUBLISH_DONE 済みの購読 (終了待ち
/// entry が残っている場合) と、tick で解消済みの移行待ち (`migration_pending_tracks` に
/// 載っている kind) には送らない (0100 型の警告を再発させない)。Established の通常
/// 再発行では送る。`permanently_ended_tracks` に含まれる track は再発行しないため
/// 判定対象外である (安全側として送らない)。
fn reissue_send_stop_sending(
    pending: Option<PublishDoneDisposition>,
    permanently_ended_tracks: &HashSet<TrackKind>,
    migration_pending_tracks: &HashSet<TrackKind>,
    kind: TrackKind,
) -> bool {
    if permanently_ended_tracks.contains(&kind) {
        return false;
    }
    // 移行待ち (tick 解消済み) の subscription は PUBLISH_DONE 受信時点で Terminated に
    // 遷移しているため、送っても拒否される
    match pending {
        Some(PublishDoneDisposition::TrackEnded) | Some(PublishDoneDisposition::Error) => false,
        Some(PublishDoneDisposition::GoingAway) => false,
        None if migration_pending_tracks.contains(&kind) => false,
        None => true,
    }
}

/// GOAWAY または transport drain による再購読の経路
#[derive(Debug, Clone, Copy)]
enum ResubscribeRoute {
    /// session 移行。旧 session の stream task を止めて新しい data plane へ切り替える経路
    SessionMigration,
    /// request stream 単位の再発行。同じ session で購読を張り直す経路
    RequestReissue,
}

/// 再購読で購読の境界の候補になる位置
///
/// どちらを使うかは経路で決まる ([`select_video_subscription_boundary`])。どちらも
/// `Option<Location>` であり、呼び出し側で取り違えても型では検出できないため、名前付きの
/// フィールドで渡す。
#[derive(Debug, Clone, Copy)]
struct VideoBoundaryCandidates {
    /// 再購読の開始時に Location Filter へ渡した開始位置
    snapshot: Option<Location>,
    /// 再購読の完了後に読み直した最後の受信位置
    ///
    /// session 移行では旧 session の stream task を止めて切り替える直前、request stream 単位の
    /// 再発行では再発行の完了後に読み直した値である。
    reread: Option<Location>,
}

/// 再購読のあとに映像へ適用する購読の境界の候補を選ぶ
///
/// GOAWAY または transport drain による再購読では、新しい `SUBSCRIBE_OK` の Largest Object を
/// 境界にしない。Largest Object は「publisher がそのメッセージを処理した時点で、その Track で
/// 最大の Location」であるため (draft-ietf-moq-transport-23 §3.1.4 (Largest Object))、移行中・
/// 再発行中に publish された Object を境界以下として捨ててしまう。境界は経路ごとに次の位置を
/// 使う。
///
/// - session 移行は、旧 session の stream task を止めて新しい data plane へ切り替える直前に
///   `last_received_locations` から読み直した値 (`reread`) を使う。再購読の開始時に
///   Location Filter へ渡したスナップショット (`snapshot`) を使うと、移行中に旧 session で
///   受信した Object を新しい購読の再送分として送出し直す
/// - request stream 単位の再発行は、再発行の前に取得した最後に受信した Location
///   (`snapshot`) を使う。`record_last_location` は Object の送出前に記録するため、再発行の
///   完了後に読み直した値を境界にすると、記録済みだがまだ送出していない Object を境界以下
///   として落とす競合がある
///
/// この選択は `VideoDeliveryOrder::advance_boundary` に渡され、境界は後退しない。記録が
/// 送出前に行われるため、切り替え直前に記録された Object が送出されないまま境界以下になる
/// 競合は残るが、対象は高々数 Object である。
///
/// draft 由来の仕様に依存する判断であり、将来変更される可能性がある。
fn select_video_subscription_boundary(
    route: ResubscribeRoute,
    candidates: VideoBoundaryCandidates,
) -> Option<Location> {
    match route {
        ResubscribeRoute::SessionMigration => candidates.reread,
        ResubscribeRoute::RequestReissue => candidates.snapshot,
    }
}

/// track ごとに最後に受信した Object の Location
///
/// セッション移行では、再購読の Location Filter を「最後に届いた Object の位置」から
/// 始めることで、移行中に publish された Object の取りこぼしを抑える
/// (draft-ietf-moq-transport-23 §3.3.1 (Location Filters) / §9.19.9 (LOCATION FILTER
/// Parameter) Table 6 の Type 0x02 (Absolute Start))。Object は各 stream task が配送経路へ
/// 流すため、`Arc<Mutex<...>>` で共有し、配送のたびに最大値を記録する。
///
/// 記録するのは「配送経路へ流した Object」に限る。フィルタで落とした Object や payload を
/// 持たない Object は、再開位置として意味を持たないため記録しない。購読の境界以下として
/// 送出しなかった Object も、購読より前に publish された分を含むため再購読の開始位置として
/// 記録する。Group ID / Object ID は単調に増えるとは限らない (Group Order が降順の
/// publisher もいる) ため、単純な代入ではなく最大値を残す。`Location` の順序は
/// (group_id, object_id) の辞書順である (§8.2)。
///
/// draft 由来の仕様に依存する値であり、将来変更される可能性がある。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LastReceivedLocations {
    /// catalog track で最後に受信した Object の Location
    catalog: Option<Location>,
    /// 映像 track で最後に受信した Object の Location
    video: Option<Location>,
    /// 音声 track で最後に受信した Object の Location
    audio: Option<Location>,
}

impl LastReceivedLocations {
    /// 指定した track 種別の最後の Location を返す
    fn get(&self, kind: TrackKind) -> Option<Location> {
        match kind {
            TrackKind::Catalog => self.catalog,
            TrackKind::Video => self.video,
            TrackKind::Audio => self.audio,
            // SUBSCRIBE_TRACKS 由来の購読は再開位置を使わない
            TrackKind::Discard => None,
        }
    }

    /// 受信した Object の Location を記録する
    ///
    /// 同じ track で既に記録した位置より新しい場合だけ更新する。
    fn record(&mut self, kind: TrackKind, location: Location) {
        let slot = match kind {
            TrackKind::Catalog => &mut self.catalog,
            TrackKind::Video => &mut self.video,
            TrackKind::Audio => &mut self.audio,
            // 購読の再開に使わない track は位置を記録しない
            TrackKind::Discard => return,
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
/// 映像だけ、購読のあとに直近 Group の FETCH を発行する (起動時と同じ手順)。音声の明示
/// FETCH は発行しない。
///
/// `last_locations` は前の session で最後に受信した Object の位置である。映像と音声は
/// これを Location Filter の Absolute Start (0x02) として SUBSCRIBE に載せ、移行中に
/// publish された Object を取得し直す (draft-ietf-moq-transport-23 §3.3.1 / §9.19.9。
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
///
/// catalog の購読は `plan.catalog_namespace` が `None` の場合は張り直さない
/// (PUBLISH_DONE で catalog が恒久終了した場合は、最後の `catalog_state` を使う)。
async fn resubscribe_tracks(
    client: &mut MoqtClient,
    plan: &ResubscribePlan,
    last_locations: &LastReceivedLocations,
) -> Result<Resubscribed> {
    let catalog = match &plan.catalog_namespace {
        Some(namespace) => {
            let sub = client
                .subscribe_track_with_parameters(
                    namespace.clone(),
                    MSF_CATALOG_TRACK_NAME.to_vec(),
                    catalog_subscribe_parameters(),
                )
                .await?;
            tracing::info!(
                "Resubscribed to catalog track: alias={}, request_id={}",
                sub.track_alias,
                sub.request_id,
            );
            Some(sub)
        }
        None => None,
    };

    let mut fetch_targets: HashMap<u64, FetchTarget> = HashMap::new();
    // FETCH の Request ID → 再発行に必要な指定。request stream の GOAWAY で同じ要求を
    // 発行し直せるようにする
    let mut fetch_requests: HashMap<u64, FetchRequestTarget> = HashMap::new();
    // catalog 購読の fill fetch stream をカタログとして適用するため、Request ID を登録する
    // (移行中に更新された catalog が fill の完全な catalog で届く)
    if let Some(sub) = &catalog {
        fetch_targets.insert(sub.request_id, FetchTarget::Catalog);
    }
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
            // (draft-ietf-moq-transport-23 §9.19.7 (SUBSCRIBER PRIORITY Parameter) / §9.11)
            if let Some((request_id, request)) =
                fetch_recent_video_groups(client, target, sub.largest_object).await
            {
                fetch_targets.insert(request_id, FetchTarget::Video);
                fetch_requests.insert(request_id, request);
            }
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
            Some(sub)
        }
        None => None,
    };

    Ok(Resubscribed {
        catalog,
        video,
        audio,
        fetch_targets,
        fetch_requests,
    })
}

/// 映像トラックの SUBSCRIBE に載せるメッセージパラメータ
///
/// 購読の確立と同時に新しい Group の開始を要求する。`NEW_GROUP_REQUEST=0` は「購読者が
/// Group の情報を持たない」ことを示し、publisher はできるだけ早く新しい Group を開始する
/// (draft-ietf-moq-transport-23 §9.19.19)。Group の途中で参加した購読者は、次の周期
/// キーフレームを待たずに復号を始められる。同節は DYNAMIC_GROUPS の告知が無くても
/// SUBSCRIBE に載せてよいと定める。
///
/// `last` は前の session で最後に受信した Object の位置である。移行後の再購読では
/// Location Filter の Absolute Start (Table 6 の Type 0x02) として載せ、移行中に publish
/// された Object を取得し直す (§3.3.1 (Location Filters) / §9.19.9 (LOCATION FILTER
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
/// (draft-ietf-moq-transport-23 §3.3.1 / §9.19.9 Table 6)。音声も境界の位置の Object が
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

/// 購読した映像 track の直近 Group を FETCH で要求する range を決める
///
/// `largest` は SUBSCRIBE_OK が運んだ Largest Object (draft-ietf-moq-transport-23
/// §3.1.4 (Largest Object) / §9.19.17 (LARGEST_OBJECT Parameter)) である。購読より前に
/// publish された Object の存在はこれでしか分からないため、無い場合は FETCH しない。
/// 開始 Location は Largest Object の Group を含む `groups` 個の Group の先頭
/// (Largest Object の Group が G なら G - (groups - 1))、終端は Largest Object とする。
/// 両端を含む range であり (§3.3.1 (Location Filters): "an inclusive range of Locations")、
/// Group 数が足りない場合は Group 0 で飽和させる。
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
/// 要求 range は LOCATION_FILTER (draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER
/// Parameter)) の Type 0x04 (Absolute Range) で指定する。Start は絶対 Location、End は
/// StartGroup からの差分 (EndGroupDelta) と EndObject で表す (同節 Table 6)。
/// `start.group_id <= end.group_id` は [`plan_fetch_range`] が保証するため差分は
/// underflow しない。
///
/// INCLUDE_PROPERTIES (§9.19.22) は載せない。同節が指定するのは OK メッセージに載る
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

/// メディアの購読の track 種別を FETCH の配送先へ写す
///
/// `FetchTarget` を持つのは映像と catalog だけであり、音声は `None` になる (音声の明示
/// FETCH は発行しない)。再発行で同じ track の旧 Request ID を対応表から外すときに使う。
fn fetch_target_of(kind: TrackKind) -> Option<FetchTarget> {
    match kind {
        TrackKind::Video => Some(FetchTarget::Video),
        TrackKind::Catalog => Some(FetchTarget::Catalog),
        TrackKind::Audio | TrackKind::Discard => None,
    }
}

/// 購読した映像 track の直近 Group を FETCH で要求する
///
/// 音声の明示 FETCH は発行しない。音声は 1 Object = 1 Group で届き (draft-ietf-moq-loc-05
/// §4.1)、起動時の実測で明示 FETCH の起動・再生・録画への寄与が無かった。残る OpusHead の
/// 取得も再生に必須ではなく、録画は catalog の値で代替できる。映像だけを対象にすることは
/// 関数名 (`fetch_recent_video_groups`) で示す。
///
/// `largest` に Largest Object が無い場合は FETCH しない ([`plan_fetch_range`])。発行できたら
/// 「Request ID → 配送先」の対応表へ記録できるよう、Request ID と再発行に必要な指定
/// ([`FetchRequestTarget`]) を返す。FETCH_OK の受信時に記録するのではなく発行直後に
/// 記録するのは、応答 stream が FETCH_OK より先に届きうるためである。発行に失敗した場合
/// (Session が送信を拒否した場合を含む) は warn ログを残して live の購読を続ける
/// (FETCH は欠落した Group を埋めるための補助であり、live の受信を止める理由にはならない)。
///
/// peer が REQUEST_ERROR で拒否した場合は応答 stream が届かない。拒否は `MoqtClient::next_event`
/// の `SessionEvent::RequestErrorReceived` で届き、メインループがログに残して対応表から
/// 当該 Request ID を外す。live の購読は影響を受けない。
async fn fetch_recent_video_groups(
    client: &mut MoqtClient,
    target: &SubscribeTarget,
    largest: Option<Location>,
) -> Option<(u64, FetchRequestTarget)> {
    let Some((start, end)) = plan_fetch_range(largest, FETCH_RECENT_GROUPS) else {
        tracing::info!(
            "Skipping FETCH for the video track: SUBSCRIBE_OK carried no Largest Object"
        );
        return None;
    };
    let parameters = fetch_range_parameters(start, end);
    match client
        .fetch(
            target.namespace.clone(),
            target.track_name.clone(),
            parameters.clone(),
        )
        .await
    {
        Ok(request_id) => {
            tracing::info!(
                "Sent FETCH for the recent groups: request_id={request_id}, \
                 start=({}, {}), end=({}, {})",
                start.group_id,
                start.object_id,
                end.group_id,
                end.object_id,
            );
            Some((
                request_id,
                FetchRequestTarget {
                    target: target.clone(),
                    parameters,
                },
            ))
        }
        Err(e) => {
            tracing::warn!("Failed to send FETCH for the video track: {e}");
            None
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

/// request stream の GOAWAY が対象とする track 種別を、現在の Request ID から決める
///
/// request stream 上の GOAWAY は当該 request だけの移行を求める (§9.2。draft 由来の仕様で
/// あり将来変更されうる)。どの track の request かは、現在購読している Request ID との
/// 一致で判定する。どれにも一致しない (既に終端した request など) 場合は `None` を返し、
/// 呼び出し側は何もしない。
fn goaway_request_kind(
    request_id: u64,
    catalog_request_id: Option<u64>,
    video_request_id: Option<u64>,
    audio_request_id: Option<u64>,
) -> Option<TrackKind> {
    // catalog が既に恒久終了 (`catalog_request_id = None`) の場合は照合しない
    // (移行先で media の request id が 0 から採番され、旧 session の catalog request id
    // 0 と衝突するため)
    if catalog_request_id == Some(request_id) {
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

/// request stream の GOAWAY が対象とする request の種別
///
/// SUBSCRIBE 系の request は [`goaway_request_kind`] が Request ID から種別を決める。
/// FETCH は応答の配送先を持たないため、再発行に必要な指定を保持する
/// [`FetchRequestTarget`] の対応表で判定する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoawayRequestTarget {
    /// SUBSCRIBE 系の request (catalog / 映像 / 音声)
    Subscribe(TrackKind),
    /// メディア FETCH の request
    Fetch,
}

/// request stream の GOAWAY が対象とする request を、現在の状態から決める
///
/// SUBSCRIBE 系の照合を先に行い、一致しなければ FETCH の対応表を照合する。どちらにも
/// 一致しない (既に終端した request など) 場合は `None` を返し、呼び出し側は何もしない。
fn goaway_request_target(
    request_id: u64,
    catalog_request_id: Option<u64>,
    video_request_id: Option<u64>,
    audio_request_id: Option<u64>,
    fetch_requests: &HashMap<u64, FetchRequestTarget>,
) -> Option<GoawayRequestTarget> {
    if let Some(kind) = goaway_request_kind(
        request_id,
        catalog_request_id,
        video_request_id,
        audio_request_id,
    ) {
        return Some(GoawayRequestTarget::Subscribe(kind));
    }
    fetch_requests
        .contains_key(&request_id)
        .then_some(GoawayRequestTarget::Fetch)
}

/// side session を共有する単位のキー
///
/// side session は接続先 (authority / path / fragment) ごとに 1 本にする。キーは
/// [`tokio_moq::same_session_url`] と同じ比較対象をすべて含む可読な URL 表現にする。
/// fragment の type と value は `:` で連結するが、`:` は RFC 3986 §3.5 の fragment の
/// 文字集合に含まれるため、type と value の組が異なればキーも異なる。
///
/// fragment には c4m の認可トークンが含まれうるため、キーには fragment のハッシュだけを
/// 入れ、ログに出してもトークンが漏れないようにする。同じ fragment なら同じキーになり、
/// 異なる fragment (別トラック、別トークン) は別のキーになる。
fn side_session_url_key(url: &tokio_moq::ServerUrl) -> String {
    use std::hash::{Hash, Hasher};
    match &url.fragment {
        Some(fragment) => {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            fragment.fragment_type.hash(&mut hasher);
            fragment.value.hash(&mut hasher);
            format!(
                "moqt://{}{}#{:016x}",
                url.authority,
                url.path,
                hasher.finish()
            )
        }
        None => format!("moqt://{}{}", url.authority, url.path),
    }
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
/// 2 回目の GOAWAY (draft-ietf-moq-transport-23 §9.2 (GOAWAY) は control stream の
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
/// draft-ietf-moq-transport-23 §6.6.1 (Graceful Session Migration) / §9.2 (GOAWAY) は
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
/// 資源を保持する。NO_ERROR で閉じる (draft-ietf-moq-transport-23 §6.6.1 (Graceful Session
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
/// oneshot で受け取る (draft-ietf-moq-transport-23 §6.6.1 は移行をアプリケーションへ
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
/// (draft-ietf-moq-transport-23 §6.6.1 (Graceful Session Migration) は、Established の
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
    /// 再発行に伴って発行した FETCH の対応表 (Request ID → 配送先)
    fetch_targets: HashMap<u64, FetchTarget>,
    /// 再発行に伴って発行した FETCH の再発行に必要な指定 (Request ID → 要求内容)
    fetch_requests: HashMap<u64, FetchRequestTarget>,
}

/// request stream の GOAWAY で、現在の session に同じ購読を再発行する
///
/// draft-ietf-moq-transport-23 §9.2 (GOAWAY): request stream 上の GOAWAY は当該 request
/// だけの移行であり、session は維持する。New Session URI が空 (現在の URI の再利用) の
/// 場合は同じ session で購読を再発行する。別 URI への移行は、専用タスクが確立した side
/// session でこの関数を呼ぶ (経路の判定は呼び出し側が行う)。
///
/// 旧 request stream は STOP_SENDING で閉じる (§6.4.2.3 (Request Cancellation and
/// Rejection))。ただし発行済みの購読が既に Terminated の場合 (PUBLISH_DONE の
/// `GOING_AWAY` 受信済みの再発行) は送らない (`Session::stop_sending` は Terminated を
/// 拒否し、0100 型の警告が出るため)。購読の張り直しは移行時と同じ手順 (映像は
/// SUBSCRIBER_PRIORITY の REQUEST_UPDATE と直近 Group の FETCH、音声は SUBSCRIBE だけ)
/// で行う。draft 由来の仕様であり、将来変更される可能性がある。
async fn reissue_request(
    client: &mut MoqtClient,
    kind: TrackKind,
    request_id: u64,
    plan: &ResubscribePlan,
    last_locations: &LastReceivedLocations,
    send_stop_sending: bool,
) -> Result<ReissuedRequest> {
    // 旧 request stream を閉じる。購読は Terminated へ遷移する
    // (draft-ietf-moq-transport-23 §6.4.2.3 (Request Cancellation and Rejection))。
    // peer が既に終端させていた場合は状態機械に拒否されるが、その場合も再発行は続ける
    if send_stop_sending && let Err(e) = client.stop_sending(request_id).await {
        tracing::warn!(
            "Failed to send STOP_SENDING for request {request_id} before re-issuing it: {e}"
        );
    }
    let mut fetch_targets: HashMap<u64, FetchTarget> = HashMap::new();
    // 再発行に伴う FETCH も request 単位の移行の対象になるため、再発行に必要な指定を
    // 対応表へ記録する
    let mut fetch_requests: HashMap<u64, FetchRequestTarget> = HashMap::new();
    let subscription = match kind {
        TrackKind::Catalog => {
            let Some(namespace) = &plan.catalog_namespace else {
                return Err(Error::Other(
                    "the catalog subscription is not configured".to_string(),
                ));
            };
            let sub = client
                .subscribe_track_with_parameters(
                    namespace.clone(),
                    MSF_CATALOG_TRACK_NAME.to_vec(),
                    catalog_subscribe_parameters(),
                )
                .await?;
            // 再発行した catalog 購読の fill fetch stream をカタログとして適用するため、
            // Request ID を対応表に登録する
            fetch_targets.insert(sub.request_id, FetchTarget::Catalog);
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
            if let Some((fetch_request_id, fetch_request)) =
                fetch_recent_video_groups(client, target, sub.largest_object).await
            {
                fetch_targets.insert(fetch_request_id, FetchTarget::Video);
                fetch_requests.insert(fetch_request_id, fetch_request);
            }
            sub
        }
        TrackKind::Audio => {
            let Some(target) = plan.audio.as_ref() else {
                return Err(Error::Other(
                    "the audio subscription is not configured".to_string(),
                ));
            };
            client
                .subscribe_track_with_parameters(
                    target.namespace.clone(),
                    target.track_name.clone(),
                    audio_subscribe_parameters(last_locations.get(TrackKind::Audio)),
                )
                .await?
        }
        // PUBLISH 起点の購読は publisher が開始したものであり、自側からは再発行できない
        TrackKind::Discard => {
            return Err(Error::Other(
                "a PUBLISH-originated subscription cannot be re-issued".to_string(),
            ));
        }
    };
    Ok(ReissuedRequest {
        subscription,
        fetch_targets,
        fetch_requests,
    })
}

/// FETCH の旧 request stream を STOP_SENDING と RESET_STREAM で終端する
///
/// draft-ietf-moq-transport-23 §3.2.4 (Fetch State Management) は、FETCH を取り消す
/// subscriber に bidi request stream への STOP_SENDING を MUST で求める。終端に失敗しても
/// セッションは閉じず、新しい session での再発行は旧 request stream の終端を待たない。
/// draft 由来の仕様であり将来変更されうる。
async fn stop_fetch_request_stream(client: &mut MoqtClient, old_request_id: u64) {
    if let Err(e) = client.stop_fetch_sending(old_request_id).await {
        tracing::warn!(
            "Could not terminate the FETCH request stream with STOP_SENDING: \
             request_id={old_request_id}, error={e}"
        );
    }
}

/// request stream の GOAWAY で、現在の session にメディア FETCH を再発行する
///
/// `send_stop_sending` が真のときは旧 request stream を STOP_SENDING で閉じる
/// (draft-ietf-moq-transport-23 §6.4.2.3 (Request Cancellation and Rejection))。
/// side session では旧 session の request を同じ関数で終端できないため、呼び出し側が
/// 偽を渡す。FETCH は namespace / track name / メッセージパラメータ (要求 range を含む) を
/// そのまま使って発行し直す。draft 由来の仕様であり、将来変更される可能性がある。
async fn reissue_fetch(
    client: &mut MoqtClient,
    old_request_id: u64,
    target: &SubscribeTarget,
    parameters: &MessageParameters,
    send_stop_sending: bool,
) -> Result<u64> {
    if send_stop_sending {
        stop_fetch_request_stream(client, old_request_id).await;
    }
    let request_id = client
        .fetch(
            target.namespace.clone(),
            target.track_name.clone(),
            parameters.clone(),
        )
        .await?;
    Ok(request_id)
}

/// request stream の GOAWAY で別 URL の session へ移行する要求を扱うタスクのイベント
///
/// side session の確立と request の再発行の結果と、移行後に side session で観測した session
/// イベントを main ループへ渡す。main ループは `MoqtClient` を 1 つだけ所有するため、
/// 移行対象の request は専用タスクが確立した side session で受信する
/// (draft-ietf-moq-transport-23 §9.2 (GOAWAY) の "re-issue that specific request on a
/// session at the specified URI"。draft 由来の仕様であり将来変更されうる)。
///
/// side session は接続先 URL ごとに 1 本であり、同じ URL を指す複数の request の移行を
/// 1 本の session に相乗りさせる。そのためイベントには、どの side session のものかを
/// 識別する URL を付ける。
#[derive(Debug)]
enum RequestMigrationEvent {
    /// side session で request の再発行に成功した
    Migrated {
        /// side session の URL ([`side_session_url_key`] のキー)
        url: String,
        /// 再発行した要求の種別
        request: MigratedRequest,
        /// 旧 session の request の Request ID (main ループが STOP_SENDING で終端する)
        old_request_id: u64,
        /// side session で再発行した request の Request ID
        new_request_id: u64,
    },
    /// 期限切れ・接続失敗・再発行失敗
    Failed {
        /// side session の URL ([`side_session_url_key`] のキー)
        url: String,
        /// 移行できなかった要求の種別
        request: MigratedRequest,
        /// 移行できなかった旧 session の Request ID
        old_request_id: u64,
        /// ログに出す失敗の説明
        reason: String,
    },
    /// side session で観測した session イベント (PUBLISH_DONE / REQUEST_ERROR / GOAWAY / close)
    SessionEvent {
        /// side session の URL ([`side_session_url_key`] のキー)
        url: String,
        /// イベントの対象が移行した購読なら、その track 種別
        ///
        /// side session は複数の request を扱うため、Request ID から track を解決できない
        /// イベント (GOAWAY / close) は `None` になる。
        kind: Option<TrackKind>,
        /// side session で観測したイベント
        event: SessionEvent,
    },
    /// side session のタスクが終了した
    ///
    /// 移行済みの request は配送されなくなるが、他の track の受信は続ける。main ループは
    /// 同じ URL の slot を捨て、以後の request 単位移行で新しい session を起動できるようにする。
    Ended {
        /// side session の URL ([`side_session_url_key`] のキー)
        url: String,
        /// side session の世代 (同じ URL で作り直した session との取り違えを防ぐ)
        generation: u64,
        /// ログに出す終了理由
        reason: String,
    },
}

/// 再発行に成功した要求の種別
///
/// main ループは購読と FETCH で状態の更新方法が異なるため、成功イベントでも区別する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigratedRequest {
    /// SUBSCRIBE 系の要求 (catalog / 映像 / 音声)
    Subscribe(TrackKind),
    /// メディア FETCH の要求
    Fetch,
}

/// side session へ送る再発行の要求
///
/// main ループは request stream の GOAWAY を受けるたびに、この要求を URL ごとの side
/// session へ送る。side session タスクは要求を 1 件ずつ処理し、結果を main ループへ返す。
enum SideSessionCommand {
    /// SUBSCRIBE 系の request を再発行する
    Reissue {
        /// 再発行する track 種別
        kind: TrackKind,
        /// 旧 session の request の Request ID (終端は main ループが行う)
        old_request_id: u64,
        /// 再発行する購読の指定
        plan: ResubscribePlan,
        /// 再購読の Location Filter に使う、track ごとの最後の Location
        last_locations: LastReceivedLocations,
        /// この要求の移行期限 (GOAWAY の Timeout から作る)
        deadline: MigrationDeadline,
    },
    /// メディア FETCH を再発行する
    ReissueFetch {
        /// 旧 session の request の Request ID (終端は main ループが行う)
        old_request_id: u64,
        /// 再発行する FETCH の track の指定 (namespace と track name)
        target: SubscribeTarget,
        /// FETCH に載せたメッセージパラメータ (要求 range を含む)
        parameters: MessageParameters,
        /// この要求の移行期限 (GOAWAY の Timeout から作る)
        deadline: MigrationDeadline,
    },
}

impl SideSessionCommand {
    /// この要求の移行期限
    fn deadline(&self) -> MigrationDeadline {
        match self {
            Self::Reissue { deadline, .. } | Self::ReissueFetch { deadline, .. } => *deadline,
        }
    }

    /// 旧 session の request の Request ID
    fn old_request_id(&self) -> u64 {
        match self {
            Self::Reissue { old_request_id, .. } | Self::ReissueFetch { old_request_id, .. } => {
                *old_request_id
            }
        }
    }

    /// ログと [`RequestMigrationEvent`] に載せる要求の種別
    fn migrated_request(&self) -> MigratedRequest {
        match self {
            Self::Reissue { kind, .. } => MigratedRequest::Subscribe(*kind),
            Self::ReissueFetch { .. } => MigratedRequest::Fetch,
        }
    }

    /// ログに出す要求の説明
    fn describe(&self) -> String {
        match self {
            Self::Reissue {
                kind,
                old_request_id,
                ..
            } => format!("{kind:?} subscription (old_request_id={old_request_id})"),
            Self::ReissueFetch {
                old_request_id,
                target,
                ..
            } => format!(
                "FETCH namespace={}, track={} (old_request_id={old_request_id})",
                serialize_namespace(&target.namespace),
                serialize_track_name(&target.track_name),
            ),
        }
    }
}

/// 同一 URL の side session の進行状態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SideSessionState {
    /// 接続と最初の request の再発行が進行中
    Connecting,
    /// session が確立済みで、追加の request を同じ session で再発行できる
    Established,
}

/// 同一 URL の side session を共有するためのスロット
struct SideSessionSlot {
    /// side session へ再発行の要求を送るチャネル
    commands: tokio::sync::mpsc::Sender<SideSessionCommand>,
    /// side session の進行状態
    state: SideSessionState,
    /// side session の世代
    ///
    /// `Ended` の世代と照合し、同じ URL で作り直した session の slot を誤って
    /// 捨てないようにする。
    generation: u64,
}

/// side session タスクへ渡す接続の指定
struct SideSessionRequest {
    /// 移行先の URL (GOAWAY の New Session URI)
    url: tokio_moq::ServerUrl,
    /// 接続経路 (現在の接続と同じものを使う)
    transport: Transport,
    /// TLS の CA 証明書パス (現在の接続と同じものを使う)
    cert: Option<String>,
    /// 接続の期限 (最初の再発行要求の GOAWAY の Timeout から作る)
    ///
    /// 追加の要求は確立済みの session に相乗りするため、接続の期限は最初の要求で決まる。
    deadline: MigrationDeadline,
    /// 最初に再発行する要求
    initial: SideSessionCommand,
}

/// side session タスクへ渡す共有状態
///
/// main ループが所有する状態をタスクへ clone して渡す。`MoqtClient` は side session を
/// 確立するタスクが 1 つだけ所有し、main ループは旧 session の `MoqtClient` を所有し続ける。
struct RequestMigrationShared {
    /// デコード済み映像フレームの送り先 (main ループと同じプレイヤーへ渡す)
    frame_tx: std::sync::mpsc::Sender<DecodedVideoFrame>,
    /// デコード済み音声フレームの送り先 (main ループと同じプレイヤーへ渡す)
    audio_tx: std::sync::mpsc::Sender<DecodedAudioFrame>,
    /// catalog の更新を main ループへ渡すチャネル (state は main ループが所有する)
    catalog_tx: tokio::sync::mpsc::Sender<catalog::CatalogObject>,
    /// 書式違反によるセッション終了を main ループへ依頼するチャネル
    termination_tx: tokio::sync::mpsc::Sender<SessionError>,
    /// 音声デコーダ (購読で 1 つ共有する)
    audio_decoder: Option<std::sync::Arc<tokio::sync::Mutex<AudioDecoder>>>,
    /// 映像の復号順の判定状態 (購読で 1 つ共有する)
    video_decode_order: std::sync::Arc<tokio::sync::Mutex<VideoDecodeOrder>>,
    /// 映像の購読境界と送出順の判定状態 (購読で 1 つ共有する)
    video_delivery_order: std::sync::Arc<std::sync::Mutex<VideoDeliveryOrder>>,
    /// 表示待ちの映像フレーム数 (受信側が増やし、表示側が減らす)
    display_backlog: std::sync::Arc<std::sync::atomic::AtomicI64>,
    /// Group の切り替え gate (購読で 1 つ共有する)
    group_switch: std::sync::Arc<SharedGroupSwitchGate>,
    /// 受信側の音声の追いつき (購読で 1 つ共有する)
    audio_catch_up: std::sync::Arc<std::sync::Mutex<AudioReceiveCatchUp>>,
    /// 音声の購読境界と送出順の判定 (購読で 1 つ共有する)
    audio_delivery_order: std::sync::Arc<std::sync::Mutex<AudioDeliveryOrder>>,
    /// track ごとに最後に受信した Object の位置 (再購読の Location Filter に使う)
    last_received_locations: std::sync::Arc<std::sync::Mutex<LastReceivedLocations>>,
    /// Audio Config の処理を試みた session の世代 (購読で 1 つ共有する)
    audio_config_validated_generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// side session の世代
    ///
    /// side session は旧 session より新しい世代にして、side session で最初に届いた
    /// Audio Config を処理させる (`Audio Config の検証済み世代` と比べる)。
    generation: u64,
    /// Track Alias 未確立の Subgroup ストリームの保留バッファ (main ループと共有する)
    pending_subgroups: SharedPendingSubgroupBuffer,
    /// 録画先 (`--mp4` 指定時のみ)
    recorder: Option<RecorderSender>,
    /// 再生するか
    playback: bool,
    /// 映像の codec (catalog の値)
    video_codec: Option<String>,
    /// 音声の codec (catalog の値)
    audio_codec: Option<String>,
    /// 音声のサンプルレートとチャンネル数 (catalog の値)
    audio_params: Option<(u32, u8)>,
    /// data stream の同時処理枠 (main ループと共有する)
    stream_slots: std::sync::Arc<tokio::sync::Semaphore>,
}

/// side session タスクの `tokio::select!` が返す「何が起きたか」
///
/// `MoqtClient` はタスクが 1 つだけ所有するため、`select!` の腕では `client` を借用する
/// future だけを待ち、結果を受けて `client` を使う処理は `select!` の外で行う。その
/// 受け渡しに使う。腕ごとに完了した事実だけを持ち、判定は select の後で行う。
enum SideSessionStep {
    /// main ループから届いた再発行要求 (チャネルが閉じたら `None`)
    Command(Option<SideSessionCommand>),
    /// `MoqtClient::next_event` の結果
    Event(std::result::Result<Option<ClientEvent>, TransportError>),
    /// side session が data stream を受理した結果
    Accepted(std::result::Result<Option<transport::RecvStream>, TransportError>),
    /// side session の datagram 受信結果 (受信タスクが終了したら `None`)
    Datagrams(Option<std::result::Result<Vec<Vec<u8>>, TransportError>>),
    /// 100 ms 周期の tick
    Tick,
    /// side session で検出した書式違反による終了依頼
    Termination(SessionError),
}

/// 同一 URL の side session を起動する
///
/// 戻り値はタスクのハンドルと、追加の再発行要求を送るチャネルである。main ループは URL
/// ごとにこのチャネルを保持し、同じ URL への request 単位移行を 1 本の session に
/// 相乗りさせる。タスクは移行に成功した後も side session の受信を続けるため、main ループの
/// 終了時 (または control GOAWAY による session 移行の成功時) に abort する。
fn spawn_side_session(
    task_monitor: &tokio_metrics::TaskMonitor,
    request: SideSessionRequest,
    shared: RequestMigrationShared,
    events: tokio::sync::mpsc::Sender<RequestMigrationEvent>,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::Sender<SideSessionCommand>,
) {
    let connect_monitor = task_monitor.clone();
    let instrument_monitor = task_monitor.clone();
    let (command_tx, command_rx) = tokio::sync::mpsc::channel(SIDE_SESSION_COMMAND_CHANNEL_SIZE);
    let handle = tokio::spawn(instrument_monitor.instrument(async move {
        run_side_session(request, shared, connect_monitor, events, command_rx).await;
    }));
    (handle, command_tx)
}

/// side session を開始して main ループの slot に登録する
///
/// 世代は side session の識別子として使う。main ループは side session を起動するたびに
/// 新しい世代を払い出す。
#[expect(clippy::too_many_arguments)]
fn start_side_session(
    sessions: &mut HashMap<String, SideSessionSlot>,
    tasks: &mut Vec<tokio::task::JoinHandle<()>>,
    task_monitor: &tokio_metrics::TaskMonitor,
    transport: Transport,
    cert: Option<String>,
    events: &tokio::sync::mpsc::Sender<RequestMigrationEvent>,
    url: tokio_moq::ServerUrl,
    initial: SideSessionCommand,
    shared: RequestMigrationShared,
) {
    let key = side_session_url_key(&url);
    let deadline = initial.deadline();
    let generation = shared.generation;
    tracing::info!(
        "Starting a request migration side session: url={key}, request={}",
        initial.describe(),
    );
    let (handle, commands) = spawn_side_session(
        task_monitor,
        SideSessionRequest {
            url,
            transport,
            cert,
            deadline,
            initial,
        },
        shared,
        events.clone(),
    );
    tasks.push(handle);
    sessions.insert(
        key,
        SideSessionSlot {
            commands,
            state: SideSessionState::Connecting,
            generation,
        },
    );
}

/// request 単位移行の要求を、URL ごとの side session へ送る
///
/// 同じ URL の side session があればその session に相乗りさせる。無ければ新しい side
/// session を起動する。既存の slot のタスクが既に終了していた場合 (送信に失敗した場合) は、
/// `Ended` の到着を待たずに slot を捨てて新しい session を起動し、要求をやり直す。
///
/// side session を起動するときだけ、世代を 1 つ進めてから `build_shared` で共有状態を
/// 組み立てる。Audio Config の検証済み世代と比べる世代は session ごとに新しくする必要が
/// あるため、相乗りする場合は現在の世代を消費しない。
#[expect(clippy::too_many_arguments)]
async fn dispatch_side_session_command<F>(
    sessions: &mut HashMap<String, SideSessionSlot>,
    tasks: &mut Vec<tokio::task::JoinHandle<()>>,
    task_monitor: &tokio_metrics::TaskMonitor,
    transport: Transport,
    cert: Option<String>,
    events: &tokio::sync::mpsc::Sender<RequestMigrationEvent>,
    url: tokio_moq::ServerUrl,
    command: SideSessionCommand,
    session_generation: &mut u64,
    build_shared: F,
) where
    F: Fn(u64) -> RequestMigrationShared,
{
    let key = side_session_url_key(&url);
    if let Some(slot) = sessions.get_mut(&key) {
        tracing::info!(
            "Sharing the request migration side session: url={key}, state={:?}, request={}",
            slot.state,
            command.describe(),
        );
        if let Err(tokio::sync::mpsc::error::SendError(command)) = slot.commands.send(command).await
        {
            // side session のタスクは終了済みである。Ended の到着を待たずに slot を捨て、
            // 新しい session で要求をやり直す
            tracing::debug!("Replacing a closed request migration side session: url={key}");
            sessions.remove(&key);
            *session_generation = session_generation.saturating_add(1);
            start_side_session(
                sessions,
                tasks,
                task_monitor,
                transport,
                cert,
                events,
                url,
                command,
                build_shared(*session_generation),
            );
        }
        return;
    }
    *session_generation = session_generation.saturating_add(1);
    start_side_session(
        sessions,
        tasks,
        task_monitor,
        transport,
        cert,
        events,
        url,
        command,
        build_shared(*session_generation),
    );
}

/// 再発行の失敗を `Failed` として main ループへ送る
async fn send_migration_failed(
    events: &tokio::sync::mpsc::Sender<RequestMigrationEvent>,
    url: &str,
    command: &SideSessionCommand,
    reason: &str,
) {
    // main ループが先に終了した場合だけ送信に失敗する。その結果は誰も読まないため無視する
    let _ = events
        .send(RequestMigrationEvent::Failed {
            url: url.to_string(),
            request: command.migrated_request(),
            old_request_id: command.old_request_id(),
            reason: reason.to_string(),
        })
        .await;
}

/// 終了した side session に残っている要求を `Failed` として main ループへ返す
///
/// `Ended` を先に送ると main ループは同じ URL の slot を捨てる。slot の送信側が drop される
/// までチャネルに届いた要求を回収し、再発行されないまま黙って失われる要求を無くす。
async fn drain_failed_side_session_commands(
    events: &tokio::sync::mpsc::Sender<RequestMigrationEvent>,
    url: &str,
    reason: &str,
    commands: &mut tokio::sync::mpsc::Receiver<SideSessionCommand>,
) {
    while let Some(command) = commands.recv().await {
        send_migration_failed(events, url, &command, reason).await;
    }
}

/// side session で 1 件の再発行要求を処理する
///
/// 成功したら `Migrated` を、失敗したら `Failed` を main ループへ送る。1 件の要求の失敗は
/// side session 全体の失敗ではないため、処理を続ける (session 全体の失敗は接続失敗と受信
/// ループの終了で扱う)。戻り値が `false` になるのは、main ループが先に終了してイベントを
/// 送れなかった場合だけである。
async fn reissue_on_side_session(
    client: &mut MoqtClient,
    events: &tokio::sync::mpsc::Sender<RequestMigrationEvent>,
    url: &str,
    command: &SideSessionCommand,
    track_map: &mut HashMap<u64, TrackKind>,
    subscription_requests: &mut HashMap<u64, TrackKind>,
    fetch_targets: &SharedFetchTargets,
) -> bool {
    let remaining = command.deadline().remaining();
    if remaining.is_zero() {
        send_migration_failed(
            events,
            url,
            command,
            "the migration deadline expired before re-issuing the request",
        )
        .await;
        return true;
    }
    match command {
        SideSessionCommand::Reissue {
            kind,
            old_request_id,
            plan,
            last_locations,
            ..
        } => {
            // 旧 request の終端は main ループが `stop_sending` で行うため、STOP_SENDING は
            // 送らない
            let reissued = match tokio::time::timeout(
                remaining,
                reissue_request(client, *kind, *old_request_id, plan, last_locations, false),
            )
            .await
            {
                Ok(Ok(reissued)) => reissued,
                Ok(Err(e)) => {
                    let reason = format!("failed to re-issue the request on the new session: {e}");
                    send_migration_failed(events, url, command, &reason).await;
                    return true;
                }
                Err(_) => {
                    let reason =
                        "the request re-issue did not complete within the migration deadline";
                    send_migration_failed(events, url, command, reason).await;
                    return true;
                }
            };
            // 再発行した購読の Track Alias は peer が決めるため、受信中の stream と以後の
            // data stream の両方で解決できるよう、この side session の表へ足す
            track_map.insert(reissued.subscription.track_alias, *kind);
            subscription_requests.insert(reissued.subscription.request_id, *kind);
            // 再発行に伴って発行した FETCH (catalog の fill と映像の直近 Group) の応答を
            // この side session で配送できるよう、Request ID を対応表へ足す
            lock_fetch_targets(fetch_targets).extend(reissued.fetch_targets);
            tracing::info!(
                "Re-issued the request on the request migration side session: url={url}, \
                 kind={kind:?}, old_request_id={old_request_id}, new_request_id={}, \
                 alias={}, filter_start={:?}",
                reissued.subscription.request_id,
                reissued.subscription.track_alias,
                last_locations.get(*kind),
            );
            if events
                .send(RequestMigrationEvent::Migrated {
                    url: url.to_string(),
                    request: MigratedRequest::Subscribe(*kind),
                    old_request_id: *old_request_id,
                    new_request_id: reissued.subscription.request_id,
                })
                .await
                .is_err()
            {
                return false;
            }
            true
        }
        SideSessionCommand::ReissueFetch {
            old_request_id,
            target,
            parameters,
            ..
        } => {
            // 旧 request の終端は main ループが `stop_sending` で行うため、STOP_SENDING は
            // 送らない
            match tokio::time::timeout(
                remaining,
                reissue_fetch(client, *old_request_id, target, parameters, false),
            )
            .await
            {
                Ok(Ok(new_request_id)) => {
                    // 応答 data stream を配送できるよう、新しい Request ID をこの side
                    // session の対応表へ足す
                    lock_fetch_targets(fetch_targets).insert(new_request_id, FetchTarget::Video);
                    tracing::info!(
                        "Re-issued the media FETCH on the request migration side session: \
                         url={url}, old_request_id={old_request_id}, \
                         new_request_id={new_request_id}, namespace={}, track={}",
                        serialize_namespace(&target.namespace),
                        serialize_track_name(&target.track_name),
                    );
                    if events
                        .send(RequestMigrationEvent::Migrated {
                            url: url.to_string(),
                            request: MigratedRequest::Fetch,
                            old_request_id: *old_request_id,
                            new_request_id,
                        })
                        .await
                        .is_err()
                    {
                        return false;
                    }
                }
                Ok(Err(e)) => {
                    let reason = format!("failed to re-issue the FETCH on the new session: {e}");
                    send_migration_failed(events, url, command, &reason).await;
                }
                Err(_) => {
                    let reason =
                        "the FETCH re-issue did not complete within the migration deadline";
                    send_migration_failed(events, url, command, reason).await;
                }
            }
            true
        }
    }
}

/// side session の本体
///
/// 次の順に進む。
///
/// 1. 移行先の session を [`tokio_moq::connect_with_retry`] で確立する。接続の設定・
///    接続は期限まで指数的に再試行する。接続に失敗した場合は、処理待ちの要求をすべて
///    `Failed` として main ループへ返す。
/// 2. main ループと同じタイムアウトを設定する。
/// 3. 最初の要求を [`reissue_on_side_session`] で再発行し、続く要求をチャネルから受けて
///    同じ session で再発行する。
/// 4. side session の受信ループを回す。session イベントの転送、data stream の処理、音声
///    datagram の配送、100 ms 周期の tick を行う。
///
/// `MoqtClient` をこのタスクが 1 つだけ所有するため、`tokio::select!` の腕では「何が
/// 起きたか」だけを返し、`client` を使う処理は `select!` の外で行う。
///
/// 期限 ([`MigrationDeadline`]) は GOAWAY の Timeout から作る。期限内に接続と再発行を
/// 終えられなければ `Failed` を送る。移行に成功した後の期限は、旧 session を peer が閉じる
/// 時刻であり、side session の寿命ではないため `Failed` にしない。
///
/// draft-ietf-moq-transport-23 §9.2 (GOAWAY) は draft 由来の仕様であり、将来変更され
/// うる。
async fn run_side_session(
    request: SideSessionRequest,
    shared: RequestMigrationShared,
    task_monitor: tokio_metrics::TaskMonitor,
    events: tokio::sync::mpsc::Sender<RequestMigrationEvent>,
    mut commands: tokio::sync::mpsc::Receiver<SideSessionCommand>,
) {
    let url = side_session_url_key(&request.url);
    let generation = shared.generation;

    // 1. 移行先の session を確立する
    //
    // 接続の失敗は connect_with_retry が期限内で再試行済みであるため、ここでは再試行しない
    // (同じ設定で失敗し続けるだけになる)。認可トークンの引き継ぎは migration_target が
    // 済ませている (同一 origin のときだけ引き継ぐ)。
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
    let (mut client, mut recv_acceptor) = match tokio_moq::connect_with_retry(
        &params,
        request.deadline,
        RetryPolicy::default(),
        &task_monitor,
    )
    .await
    {
        Ok(connected) => connected,
        Err(e) => {
            let reason = format!("failed to connect to the migration target: {e}");
            send_migration_failed(&events, &url, &request.initial, &reason).await;
            // Ended を先に送って main ループへ slot を捨てさせる。続けてチャネルに残った
            // 要求を回収し、接続の失敗で再発行できなかった要求をすべて報告する
            let _ = events
                .send(RequestMigrationEvent::Ended {
                    url: url.clone(),
                    generation,
                    reason: reason.clone(),
                })
                .await;
            drain_failed_side_session_commands(&events, &url, &reason, &mut commands).await;
            return;
        }
    };

    // 2. main ループと同じタイムアウトを設定する
    client.set_control_message_timeout_ms(Some(30000));
    client.set_data_stream_timeout_ms(Some(30000));

    // 3. side session の受信状態
    //
    // 移行した購読の Track Alias は peer が決めるため、再発行のたびにこの表へ足す。
    // stream task は spawn した時点の表を複製して使う (main ループと同じ)。
    let mut track_map: HashMap<u64, TrackKind> = HashMap::new();
    // 移行した購読の Request ID → track 種別。side session で観測した PUBLISH_DONE などを
    // main ループの track に結び付ける
    let mut subscription_requests: HashMap<u64, TrackKind> = HashMap::new();
    // FETCH 応答の配送先。ReissueFetch のたびに新しい Request ID を足す
    let fetch_targets: SharedFetchTargets =
        std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
    if !reissue_on_side_session(
        &mut client,
        &events,
        &url,
        &request.initial,
        &mut track_map,
        &mut subscription_requests,
        &fetch_targets,
    )
    .await
    {
        // main ループが先に終了した。side session はこのタスクの終了で閉じる
        return;
    }

    // 4. side session の受信ループ
    //
    // tick の周期は main ループと同じ 100 ms にする。`MoqtClient` の tick は session の
    // タイマー (control message timeout / data stream timeout / GOAWAY deadline) を進める
    // ため、ここが止まると side session の進行も止まる。
    //
    // side session の stream task が検出した書式違反は、この side session を閉じる。
    // main session の終了依頼チャネルへ流すと、違反していない main session を閉じてしまう
    let (side_termination_tx, mut side_termination_rx) =
        tokio::sync::mpsc::channel::<SessionError>(1);
    let mut tick_interval = tokio::time::interval(std::time::Duration::from_millis(100));
    tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let data_plane = client.data_plane();
    // datagram の受信には handle を直接使う (client を借用しないため select の腕に置ける)
    let handle = client.handle();
    // datagram の受信は専用タスクへ分ける。100 ms の tick と同じ `select!` で
    // `handle.recv_datagrams()` を待つと、tick のたびに future が捨てられ、
    // ngtcp2 の driver が受け取り待ちの datagram を破棄してしまう。
    //
    // side session に音声の購読が相乗りすることがあるため、移行対象の種別によらず受ける
    // (音声の購読が無ければ `plan_datagram` が配送先なしとして捨てる)
    let (datagram_tx, mut datagram_rx) =
        tokio::sync::mpsc::channel::<std::result::Result<Vec<Vec<u8>>, TransportError>>(8);
    let datagram_task = tokio::spawn(async move {
        loop {
            match handle.recv_datagrams().await {
                Ok(batch) => {
                    // WebTransport の受信はバッファが空でも即座に空のバッチを返す。
                    // 空バッチを送ると consumer が 10 ms sleep を繰り返し、実際の
                    // datagram の配送が遅れるため、ここで休めてから次回へ進む
                    if batch.is_empty() {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        continue;
                    }
                    if datagram_tx.send(Ok(batch)).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = datagram_tx.send(Err(e)).await;
                    break;
                }
            }
        }
    });
    let mut join_set = tokio::task::JoinSet::new();
    let mut pending_streams: std::collections::VecDeque<transport::RecvStream> =
        std::collections::VecDeque::new();
    let mut stream_num: u64 = 0;
    // メディアとして扱えないトラックの datagram を 1 回だけ記録するための alias
    let mut unsupported_aliases: HashSet<u64> = HashSet::new();
    // main ループからの要求チャネルが閉じたか。閉じても移行済みの request の受信は続ける
    let mut commands_open = true;
    // side session を抜けた理由。`break 'receive` のすべての経路で代入する
    let end_reason: String;
    'receive: loop {
        // 空いた枠のぶんだけ待機中の stream を処理へ回す。permit は使った分だけ取るので、
        // 枠が無いときは 1 つも取らずにすぐ抜ける (main ループと同じ手順)
        while let Ok(permit) = std::sync::Arc::clone(&shared.stream_slots).try_acquire_owned() {
            let Some(stream) = pending_streams.pop_front() else {
                // 枠を余分に取ってしまったので返す
                drop(permit);
                break;
            };
            stream_num += 1;
            spawn_stream_task(
                &mut join_set,
                &task_monitor,
                stream,
                permit,
                &shared.stream_slots,
                stream_num,
                &track_map,
                &fetch_targets,
                &data_plane,
                &shared.frame_tx,
                &shared.audio_tx,
                shared.video_codec.as_deref(),
                shared.audio_codec.as_deref(),
                shared.audio_params,
                &shared.display_backlog,
                &shared.video_decode_order,
                &shared.video_delivery_order,
                &shared.group_switch,
                shared.audio_decoder.as_ref(),
                &shared.audio_config_validated_generation,
                shared.generation,
                &shared.audio_catch_up,
                &shared.audio_delivery_order,
                &shared.last_received_locations,
                &side_termination_tx,
                &shared.catalog_tx,
                &shared.pending_subgroups,
                shared.playback,
                shared.recorder.as_ref(),
            );
        }

        // 水が多すぎるときは accept を止める。select の分岐に入らなければ接続が stream を
        // 保持する (main ループと同じ)
        let accepting = pending_streams.len() < MAX_CONCURRENT_STREAMS * 4;

        let step = tokio::select! {
            command = commands.recv(), if commands_open => SideSessionStep::Command(command),
            event = client.next_event() => SideSessionStep::Event(event),
            accepted = recv_acceptor.accept_recv_stream(), if accepting => {
                SideSessionStep::Accepted(accepted)
            }
            received = datagram_rx.recv() => SideSessionStep::Datagrams(received),
            termination = side_termination_rx.recv() => match termination {
                Some(termination) => SideSessionStep::Termination(termination),
                None => {
                    end_reason = "the side session termination channel closed".to_string();
                    break 'receive;
                }
            },
            _ = tick_interval.tick() => SideSessionStep::Tick,
        };
        match step {
            SideSessionStep::Command(Some(command)) => {
                if !reissue_on_side_session(
                    &mut client,
                    &events,
                    &url,
                    &command,
                    &mut track_map,
                    &mut subscription_requests,
                    &fetch_targets,
                )
                .await
                {
                    // main ループが先に終了した。side session はこのタスクの終了で閉じる
                    end_reason = "the main loop stopped receiving side session events".to_string();
                    break 'receive;
                }
            }
            SideSessionStep::Command(None) => {
                // main ループが slot を捨てた。移行済みの request の受信は続ける
                commands_open = false;
            }
            SideSessionStep::Event(received) => match received {
                Ok(Some(ClientEvent::Session(event))) => {
                    // PUBLISH_DONE / REQUEST_ERROR は移行した購読の Request ID で届くため、
                    // 対象の track 種別をここで解決して main ループへ渡す
                    let kind = match &event {
                        SessionEvent::PublishDoneReceived { request_id, .. }
                        | SessionEvent::RequestErrorReceived { request_id, .. } => {
                            subscription_requests.get(request_id).copied()
                        }
                        _ => None,
                    };
                    // REQUEST_ERROR で拒否された FETCH は応答 stream が届かないため、
                    // 配送先の対応表から外す
                    if let SessionEvent::RequestErrorReceived { request_id, .. } = &event {
                        let mut targets = lock_fetch_targets(&fetch_targets);
                        if matches!(targets.get(request_id), Some(FetchTarget::Video)) {
                            targets.remove(request_id);
                            tracing::debug!(
                                "Removed a rejected FETCH from the side session fetch targets: \
                                 request_id={request_id}"
                            );
                        }
                    }
                    // PUBLISH_DONE / REQUEST_ERROR / GOAWAY / close は main ループへ渡す。
                    // main ループが先に終了した場合は移行結果を読む者がいないため、
                    // side session の受信を終える
                    if events
                        .send(RequestMigrationEvent::SessionEvent {
                            url: url.clone(),
                            kind,
                            event,
                        })
                        .await
                        .is_err()
                    {
                        end_reason =
                            "the main loop stopped receiving side session events".to_string();
                        break 'receive;
                    }
                }
                // side session では main ループと同じく peer request を処理しない。応答を
                // 返さないと peer の control message timeout を招くため拒否する
                Ok(Some(ClientEvent::Request(request))) => {
                    let (code, reason) = request_rejection(&request.message);
                    tracing::warn!(
                        "Rejecting unexpected peer request on the request migration side session: \
                         request_id={} code={code:#x}",
                        request.request_id
                    );
                    if let Err(e) = client
                        .send_request_error(request.request_id, code, reason)
                        .await
                    {
                        tracing::warn!(
                            "Failed to reject a peer request on the request migration side session: {e}"
                        );
                        end_reason =
                            "failed to reject a peer request on the side session".to_string();
                        break 'receive;
                    }
                }
                // REQUEST_UPDATE は必ず 1 通の REQUEST_OK / REQUEST_ERROR で応答する MUST
                Ok(Some(ClientEvent::RequestUpdate(update))) => {
                    tracing::info!(
                        "Received REQUEST_UPDATE on the request migration side session: \
                         request_id={}",
                        update.request_id
                    );
                    if let Err(e) = client.send_request_ok(update.request_id).await {
                        tracing::warn!(
                            "Failed to send REQUEST_OK on the request migration side session: {e}"
                        );
                        end_reason = "failed to answer a peer request update on the side session"
                            .to_string();
                        break 'receive;
                    }
                }
                Ok(None) => {
                    end_reason = "the side session ended".to_string();
                    break 'receive;
                }
                Err(e) => {
                    end_reason = format!("the side session event loop failed: {e}");
                    break 'receive;
                }
            },
            SideSessionStep::Accepted(received) => match received {
                Ok(Some(stream)) => pending_streams.push_back(stream),
                Ok(None) => {
                    end_reason = "no more data streams on the side session".to_string();
                    break 'receive;
                }
                Err(e) => {
                    // セッション終了と接続クローズは期待される終了であるため info に揃える
                    if matches!(e, TransportError::ConnectionClosed) {
                        tracing::info!("Request migration side session closed by transport: {e}");
                    } else {
                        tracing::warn!(
                            "Failed to accept a data stream on the request migration side session: {e}"
                        );
                    }
                    end_reason = format!("failed to accept a data stream on the side session: {e}");
                    break 'receive;
                }
            },
            SideSessionStep::Datagrams(None) => {
                // datagram 受信タスクが終了した (side session の終了など)
                end_reason = "the datagram receive task ended".to_string();
                break 'receive;
            }
            SideSessionStep::Datagrams(Some(received)) => match received {
                Ok(datagrams) => {
                    let batch_len = datagrams.len();
                    for raw in datagrams {
                        // type 分岐と wire の書式検証は Session が行う (main ループと同じ)
                        let acceptance = match data_plane.recv_datagram(&raw) {
                            Ok(acceptance) => acceptance,
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to process a datagram on the request migration side \
                                     session: {e}"
                                );
                                continue;
                            }
                        };
                        let DatagramAcceptance::Object(TrackDataAcceptance::Accepted) = acceptance
                        else {
                            continue;
                        };
                        match plan_datagram(&raw, &track_map) {
                            DatagramAction::DeliverAudio { datagram, payload } => {
                                // 音声の配送経路は main ループの datagram タスクと同じ状態を
                                // 共有する。session の世代は side session のものを渡す
                                let delivery = AudioDelivery {
                                    audio_tx: &shared.audio_tx,
                                    audio_config_validated_generation: &shared
                                        .audio_config_validated_generation,
                                    session_generation: shared.generation,
                                    is_opus_codec: shared
                                        .audio_codec
                                        .as_deref()
                                        .is_some_and(decoder::is_opus_codec),
                                    catalog_sample_rate: shared
                                        .audio_params
                                        .map_or(0, |(sample_rate, _)| sample_rate),
                                    catalog_channels: shared
                                        .audio_params
                                        .map_or(0, |(_, channels)| channels),
                                    recorder: shared.recorder.as_ref(),
                                    termination_tx: &shared.termination_tx,
                                    catch_up: shared.audio_catch_up.as_ref(),
                                    delivery_order: shared.audio_delivery_order.as_ref(),
                                };
                                // デコーダは購読で 1 つを共有する (main ループと同じ)
                                let mut decoder = match shared.audio_decoder.as_ref() {
                                    Some(decoder) => Some(decoder.lock().await),
                                    None => None,
                                };
                                // 移行後の再購読で使う「最後に受信した位置」を記録する
                                let location = datagram_object_location(&datagram);
                                record_last_location(
                                    &shared.last_received_locations,
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
                                    // 再生側が終了した。main ループの終了経路に任せる
                                    end_reason = "the audio delivery requested the session to stop"
                                        .to_string();
                                    break 'receive;
                                }
                            }
                            // 移行対象が音声でない side session では、他の alias の datagram は
                            // 届かない。届いた場合は 1 回だけ記録する
                            DatagramAction::Unsupported { track_alias } => {
                                if unsupported_aliases.insert(track_alias) {
                                    tracing::warn!(
                                        "Ignoring media delivered as a datagram on an unsupported \
                                         track of the request migration side session: \
                                         alias={track_alias}"
                                    );
                                }
                            }
                            DatagramAction::Ignore => {}
                        }
                    }
                    // 空のバッチのときだけ受信ループを休める (main ループと同じ)
                    if should_sleep_after_datagram_batch(batch_len) {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
                Err(e) => {
                    if matches!(e, TransportError::ConnectionClosed) {
                        tracing::info!(
                            "Request migration side session datagram receive stopped: {e}"
                        );
                    } else {
                        tracing::warn!(
                            "Request migration side session datagram receive error: {e}"
                        );
                    }
                    end_reason = format!("the side session datagram receive failed: {e}");
                    break 'receive;
                }
            },
            SideSessionStep::Tick => {
                // main ループと同じく、session のタイマーを進める
                client.tick_now();
            }
            SideSessionStep::Termination(termination) => {
                // side session で見つけた書式違反は side session を閉じる
                close_session(&mut client, termination).await;
                end_reason =
                    "the side session was closed after a data stream format violation".to_string();
                break 'receive;
            }
        }
    }
    // datagram の受信タスクを止める。join_set が drop され、side session の stream task も
    // 打ち切られる
    datagram_task.abort();
    let _ = datagram_task.await;
    tracing::info!("The request migration side session ended: url={url}, reason={end_reason}");
    // 移行済みの request は配送されなくなる。main ループへ終了を伝えて slot を捨てさせる
    let _ = events
        .send(RequestMigrationEvent::Ended {
            url: url.clone(),
            generation,
            reason: end_reason.clone(),
        })
        .await;
    // Ended の処理で slot が捨てられるまでに届いた要求も、再発行されないまま失われないよう
    // 失敗として返す
    drain_failed_side_session_commands(&events, &url, &end_reason, &mut commands).await;
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
    // SUBSCRIBE_TRACKS の prefix と、GOAWAY 移行後に同じ prefix で送り直すための複製。
    // `namespace` は catalog の購読で move するため、先に複製を取っておく
    let subscribe_tracks_namespace = namespace.clone();
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
    // (draft-ietf-moq-transport-23 Appendix A.4 の "Since draft-ietf-moq-transport-19" に記録)、
    // §3.5 (Joining an Ongoing Track) が示す購読パターンに置き換わった:
    //
    // 1. catalog track を Location Filter Next Object (§9.19.9 Table 6 の Type 0x05) と
    //    FILL_PARAMETERS (§9.19.15) で SUBSCRIBE する。FILL 内側の Location Filter は
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

    // 現在受信前提の購読の request id。PUBLISH_DONE を受けた request はここから外し、
    // shutdown 時の STOP_SENDING 対象を絞る
    let mut active_requests: HashSet<u64> = HashSet::new();
    active_requests.insert(catalog_sub.request_id);
    // PUBLISH_DONE で Terminated になった購読の終了待ち
    //
    // draft-ietf-moq-transport-23 §9.9 (PUBLISH_DONE) は購読単位の終端であり、
    // §12.4 (Publish Done Codes) は status ごとに意味が異なる。1 track の終了で
    // `peer_ended` を立ててパイプライン全体を終了させるのではなく、終了待ちの
    // request を Session の cleanup_ready の完了まで追い、cleanup できた track だけを
    // 終了させる
    let mut publish_done_pending: HashMap<u64, (TrackKind, PublishDoneDisposition)> =
        HashMap::new();
    // 恒久的に終了した track (media に加えて catalog も) と、移行待ち
    // (GOING_AWAY) の track。全 media 終了の判定は video / audio のみを見るため、
    // catalog がこの集合に載っても media の終了判定には影響しない
    //
    // 起動時の購読 (catalog / video / audio) は、購読を確立したときに active_requests へ
    // 登録する。reissue / session 移行の経路は張り直した new request_id を登録する。reissue
    // 成功時 / session 移行成功時に `migration_pending_tracks` は解除される
    let mut permanently_ended_tracks: HashSet<TrackKind> = HashSet::new();
    let mut migration_pending_tracks: HashSet<TrackKind> = HashSet::new();

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
    // 自側が発行した FETCH (映像と catalog の fill) の対応表 (Request ID → 配送先)。
    // 応答 Object は通常の再生経路へ流すが、どの track の応答かは FETCH 応答 stream の
    // FETCH_HEADER が運ぶ Request ID でしか分からないため、ここで記録する。
    // catalog 購読の Request ID も登録する。FILL_PARAMETERS 付き SUBSCRIBE には publisher が
    // fill fetch stream を返し (draft-ietf-moq-transport-23 §3.4 (Fill Semantics))、その
    // FETCH_HEADER は起因 SUBSCRIBE の Request ID を運ぶ。移行後に catalog を張り直したとき
    // の fill をカタログとして適用するため、種別をここで対応づける。
    // 移行 (GOAWAY) では、張り直した購読に対して発行した FETCH の対応表へ差し替える
    // (Request ID は session ごとに払い出される)
    //
    // request 単位移行の side session は、再発行のたびに新しい Request ID をこの表へ足す。
    // stream task がヘッダを読んだ時点の内容で配送先を決められるよう、`Arc<Mutex<...>>`
    // で共有する (ロックは await をまたがない)。
    let mut fetch_targets: SharedFetchTargets =
        std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
    lock_fetch_targets(&fetch_targets).insert(catalog_sub.request_id, FetchTarget::Catalog);
    // 自側が発行したメディア FETCH の再発行に必要な指定 (Request ID → 要求内容)。
    // request stream の GOAWAY で同じ namespace / track name / range の FETCH を
    // 発行し直せるようにする
    let mut fetch_requests: HashMap<u64, FetchRequestTarget> = HashMap::new();
    let mut video_codec: Option<String> = None;
    let mut video_request_id: Option<u64> = None;
    let mut audio_request_id: Option<u64> = None;
    // peer が SUBSCRIBE_OK で決める track alias は移行で変わりうるため、現在の値を保持する
    let mut video_alias: Option<u64> = None;
    let mut audio_alias: Option<u64> = None;
    // 音声の購読境界 (SUBSCRIBE_OK の Largest Object)。境界以下は購読より前に publish された
    // 音声であり、Audio Config の検証だけを行い、送出も録画も復号もしない
    let mut audio_largest_object: Option<Location> = None;
    // 映像の購読境界 (SUBSCRIBE_OK の Largest Object)。境界以下は購読より前に publish された
    // 映像であり、復号はするが表示しない
    let mut video_largest_object: Option<Location> = None;
    // 映像 Object を復号してよいかの判定状態は購読で 1 つ持つ。Group ごとに別の Subgroup
    // ストリームで届くため (draft-ietf-moq-transport-23 §2.2 (Subgroups))、stream ごとに
    // 判定を持つと「前の Group の Object が次の Group のキーフレームより後に届く」場合を
    // 判定できない。映像のトラックを切り替えて decoder を作り直す経路を足すときは、
    // この状態を reset してから次のキーフレームで復号を始め直すこと。
    let video_decode_order = std::sync::Arc::new(tokio::sync::Mutex::new(VideoDecodeOrder::new()));
    // Group の切り替えで次の Group の映像 Object を保留する gate は購読で 1 つ持つ。
    // stream task 間で共有し、session 移行の直前に世代を進めて状態を捨てる
    let group_switch = std::sync::Arc::new(SharedGroupSwitchGate::new());

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
        active_requests.insert(video_sub.request_id);
        video_largest_object = video_sub.largest_object;

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
        // キーフレームから復号を始められるようにする (draft-ietf-moq-transport-23 §3.5.1)。
        // 購読の優先度を変えたあとに発行する (§9.19.7 の SUBSCRIBER PRIORITY を
        // FETCH 応答にも効かせるため)
        if let Some((request_id, request)) = fetch_recent_video_groups(
            &mut client,
            &SubscribeTarget {
                namespace: v.namespace.clone(),
                track_name: v.track_name.as_bytes().to_vec(),
            },
            video_sub.largest_object,
        )
        .await
        {
            lock_fetch_targets(&fetch_targets).insert(request_id, FetchTarget::Video);
            fetch_requests.insert(request_id, request);
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
            .subscribe_track_with_parameters(
                a.namespace.clone(),
                a.track_name.as_bytes().to_vec(),
                // 起動時は前の session が無く、パイプラインが載せるパラメータは無い
                audio_subscribe_parameters(None),
            )
            .await?;
        tracing::info!(
            "Subscribed to audio track: alias={}, request_id={}",
            audio_sub.track_alias,
            audio_sub.request_id,
        );
        track_map.insert(audio_sub.track_alias, TrackKind::Audio);
        audio_alias = Some(audio_sub.track_alias);
        audio_request_id = Some(audio_sub.request_id);
        active_requests.insert(audio_sub.request_id);
        audio_largest_object = audio_sub.largest_object;

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

    // SUBSCRIBE_TRACKS は catalog / 映像 / 音声の購読を確立したあとに送る。この example は
    // catalog が示す track だけを再生するため、受理する PUBLISH は既存の購読と重複する
    // track が中心になるが、catalog に載らない track も受理して観測できる。
    // 付加的な購読であり、失敗しても既存の再生は止めない。
    //
    // 既に SUBSCRIBE している Full Track Name を PUBLISH でも購読すると同じ Object を
    // 2 系統で受け取るため、重複判定に使う一覧をここで作る。
    let mut subscribed_tracks: Vec<(TrackNamespace, Vec<u8>)> = vec![(
        subscribe_tracks_namespace.clone(),
        MSF_CATALOG_TRACK_NAME.to_vec(),
    )];
    if subscribe_video && let Some(info) = &video_info {
        subscribed_tracks.push((info.namespace.clone(), info.track_name.as_bytes().to_vec()));
    }
    if subscribe_audio && let Some(info) = &audio_info {
        subscribed_tracks.push((info.namespace.clone(), info.track_name.as_bytes().to_vec()));
    }
    let mut tracks_request_id: Option<u64> = None;
    if config.subscribe_tracks {
        match client
            .subscribe_tracks(subscribe_tracks_namespace.clone(), MessageParameters::new())
            .await
        {
            Ok(request_id) => {
                tracks_request_id = Some(request_id);
                tracing::info!(
                    "Sent SUBSCRIBE_TRACKS: request_id={request_id}, namespace={}",
                    serialize_namespace(&subscribe_tracks_namespace),
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to send SUBSCRIBE_TRACKS; continuing without the namespace subscription: {e}"
                );
            }
        }
    }
    // SUBSCRIBE_TRACKS の結果として受理した PUBLISH の Request ID → Track Alias。
    // PUBLISH_DONE の drain 完了後に alias を track_map から外すために保持する
    let mut ignored_publish_requests: HashMap<u64, u64> = HashMap::new();

    // 移行 (GOAWAY) で購読を張り直すための指定と、現在の session の Request ID / alias
    //
    // catalog は移行時に読み直さない。購読済みの指定をそのまま使えば、移行先の session で
    // 届く Object から同じ `CatalogState` が再び更新される。catalog が PUBLISH_DONE で
    // 恒久終了した場合は catalog_namespace を `None` にして張り直さない
    // (`permanently_ended_tracks` は本時点では空のため初期は購読を継続する)
    let resubscribe_plan = ResubscribePlan {
        catalog_namespace: Some(catalog_namespace_for_migration),
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
    // 現在の catalog 購読の request id と alias。恒久終了後に session 移行で張り直さない
    // 場合は旧 session の値が新 session の request id (0 から採番) と衝突するため、
    // `None` にして照合対象から外す (media の `video_request_id` / `audio_request_id` と
    // 同じ `Option` の契約にする)
    let mut catalog_request_id: Option<u64> = Some(catalog_sub.request_id);
    let mut catalog_alias: Option<u64> = Some(catalog_sub.track_alias);

    // 音声は 1 object = 1 subgroup stream で届く (LOC draft-ietf-moq-loc-05 §4.1)。
    // stream ごとにデコーダを作り直すと 20 ms ごとにデコーダ状態が失われ、
    // フレーム境界で波形が不連続になってノイズになる。
    // subscription で 1 つのデコーダを共有し、Audio Config の検証済み世代も共有する。
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
    // session の世代。移行のたびに進め、Audio Config の検証済み世代と比べる。
    // 1 から始め、検証済み世代は 0 (未検証) から始めることで、最初の Audio Config を処理する
    let mut session_generation: u64 = 1;
    let audio_config_validated_generation =
        std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    // 受信側の追いつきの状態。ストリーム経路と datagram 経路で共有する
    let audio_catch_up = Arc::new(std::sync::Mutex::new(AudioReceiveCatchUp::new()));
    // 音声の購読境界と送出順の状態。datagram 経路と subgroup stream 経路で共有する
    let audio_delivery_order = Arc::new(std::sync::Mutex::new(AudioDeliveryOrder::new(
        audio_largest_object,
    )));
    // 映像の購読境界の状態。live の Subgroup stream と FETCH 応答経路で共有する
    let video_delivery_order = Arc::new(std::sync::Mutex::new(VideoDeliveryOrder::new(
        video_largest_object,
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
    // が poll されず、session の ACK 送信とタイマー処理が止まる。QUIC の endpoint は
    // バックグラウンドで駆動しているが、MOQT session の進行は `next_event()` の poll に
    // 依存するため、ここが止まると受信も送信も完全に停止し、relay 側の輻輳ウィンドウが
    // 最小値へ落ちて復帰しない。
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
    // track ごとに最後に受信した Object の位置。移行後の再購読の Location Filter に使う。
    // stream task と datagram task、main ループ (catalog) が共有する
    let last_received_locations: Arc<std::sync::Mutex<LastReceivedLocations>> =
        Arc::new(std::sync::Mutex::new(LastReceivedLocations::default()));
    // 最後の値は session 移行のたびに進む epoch である。datagram 受信タスクは、届いた
    // Object をどの session のものとして Audio Config の検証状態に反映するかをこの値で決める。
    let (datagram_session_tx, mut datagram_session_rx) = tokio::sync::watch::channel((
        client.handle(),
        data_plane.clone(),
        Arc::clone(&datagram_track_map),
        session_generation,
    ));
    let audio_decoder_for_datagram = audio_decoder.clone();
    let audio_config_validated_generation_for_datagram =
        Arc::clone(&audio_config_validated_generation);
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
        // メディアとして扱えないトラックの datagram を 1 回だけ記録するための alias
        let mut unsupported_aliases: HashSet<u64> = HashSet::new();
        // 最初の datagram を受け取ったことを 1 回だけ記録する。届いていないのか、届いて
        // いるのに配送経路へ流れていないのかを切り分けるために出す
        let mut datagram_received = false;
        let (
            mut handle_for_datagram,
            mut data_plane_for_datagram,
            mut track_map_for_datagram,
            mut generation_for_datagram,
        ) = datagram_session_rx.borrow_and_update().clone();
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
                    let (handle, data_plane, map, epoch) =
                        datagram_session_rx.borrow_and_update().clone();
                    handle_for_datagram = handle;
                    data_plane_for_datagram = data_plane;
                    track_map_for_datagram = map;
                    generation_for_datagram = epoch;
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
                    let (handle, data_plane, map, epoch) =
                        datagram_session_rx.borrow_and_update().clone();
                    handle_for_datagram = handle;
                    data_plane_for_datagram = data_plane;
                    track_map_for_datagram = map;
                    generation_for_datagram = epoch;
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
                // draft-ietf-moq-transport-23 §11 (Data Streams and Datagrams) に従いセッションクローズになる。
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
                // (draft-ietf-moq-transport-23 §3.1 (Subscriptions) / §11.5.2 (Padding Datagrams))
                let DatagramAcceptance::Object(TrackDataAcceptance::Accepted) = acceptance else {
                    continue;
                };
                // 種別の判定には、この handle と同時に受け取った表を使う
                let action = plan_datagram(&raw, &track_map_for_datagram);
                match action {
                    DatagramAction::DeliverAudio { datagram, payload } => {
                        // session の世代 は切り替えのたびに変わるため、配送のたびに作る
                        let delivery = AudioDelivery {
                            audio_tx: &audio_tx_for_datagram,
                            audio_config_validated_generation:
                                &audio_config_validated_generation_for_datagram,
                            session_generation: generation_for_datagram,
                            is_opus_codec: audio_codec_for_datagram
                                .as_deref()
                                .is_some_and(decoder::is_opus_codec),
                            catalog_sample_rate: audio_params
                                .map_or(0, |(sample_rate, _)| sample_rate),
                            catalog_channels: audio_params.map_or(0, |(_, channels)| channels),
                            recorder: recorder_for_datagram.as_ref(),
                            termination_tx: &termination_tx_for_datagram,
                            catch_up: audio_catch_up_for_datagram.as_ref(),
                            delivery_order: audio_delivery_order_for_datagram.as_ref(),
                        };
                        // デコーダは subscription で 1 つを共有する。stream task と同じく
                        // デコードが終わるまでロックを保持する
                        let mut decoder = match audio_decoder_for_datagram.as_ref() {
                            Some(decoder) => Some(decoder.lock().await),
                            None => None,
                        };
                        // 移行後の再購読で使う「最後に受信した位置」を記録する
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
                            // 再生側が終了した。malformed Track はこの Object を捨てて
                            // 継続するため `true` が返る
                            break 'receive;
                        }
                    }
                    // 映像とカタログの datagram は本 example では扱わない。LOC の映像は
                    // 1 group = 1 unidirectional stream で配送され (draft-ietf-moq-loc-05 §4.2)、
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
    // 映像の購読境界以下として落とした Object の累計を最後にログへ出した値。同じく tick で
    // 増えたときだけ出す
    let mut logged_video_boundary_objects: u64 = 0;

    // バックグラウンド移行の状態
    //
    // 移行の結果は oneshot で main ループへ返す。移行のたびに作るため、受信器は `Option` で
    // 保持し、`migration_in_progress` が真の間だけ受信分岐を有効にする。
    let mut migration_rx: Option<tokio::sync::oneshot::Receiver<MigrationResult>> = None;
    // 移行の確立と購読の張り直しが進行中か。真の間は 2 回目の GOAWAY / drain で移行しない
    let mut migration_in_progress = false;
    // 進行中の移行タスク。run の終了時に打ち切る
    let mut migration_task: Option<tokio::task::JoinHandle<()>> = None;
    // 進行中の移行で再購読の Location Filter に渡したスナップショット。
    // VideoBoundaryCandidates の snapshot として選択関数へ渡し、選ばれた候補と実効境界は
    // 切替時にログへ出す (session 移行では再読み値が選ばれるため境界の決定には影響しない。
    // select_video_subscription_boundary を参照)
    let mut migration_boundary_snapshot: Option<LastReceivedLocations> = None;
    // 短期間に連続する移行を制限する (移行先がまた GOAWAY を返す構成への防御)
    let mut migration_limiter = MigrationLimiter::new(MIGRATION_LIMIT_WINDOW, MIGRATION_LIMIT_MAX);
    // 移行後に旧 session を終了するタスク。run の終了時に打ち切る
    let mut old_session_closers: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // request 単位の移行 (別 URL の request stream GOAWAY) の状態
    //
    // 当該 request だけを専用タスクが確立した side session で受信し、移行の結果と side
    // session のイベントは共有の mpsc で main ループへ返す。side session は接続先 URL
    // ごとに 1 本であり、同じ URL を指す複数の request の移行を 1 本の session に
    // 相乗りさせる。別の URL の side session は同時に何本あってもよい。
    let mut request_migration_sessions: HashMap<String, SideSessionSlot> = HashMap::new();
    // 進行中の side session のタスク。run の終了時と、session 全体の移行に成功した
    // ときにすべて打ち切る
    let mut request_migration_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    // 全 side session のイベントを 1 本に集約する。受信器は main ループが 1 つだけ持つ。
    // session 全体の移行に成功したときは、abort 済みの side session が送信済みのイベントを
    // 捨てるため、チャネルを作り直す (sender は main ループだけが持つ)
    let (mut request_migration_tx, mut request_migration_rx) =
        tokio::sync::mpsc::channel::<RequestMigrationEvent>(REQUEST_MIGRATION_EVENT_CHANNEL_SIZE);
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
                &stream_slots,
                total_streams,
                &track_map_snapshot,
                &fetch_targets,
                &data_plane,
                &frame_tx,
                &audio_tx,
                video_codec.as_deref(),
                audio_codec.as_deref(),
                audio_params,
                &display_backlog,
                &video_decode_order,
                &video_delivery_order,
                &group_switch,
                audio_decoder.as_ref(),
                &audio_config_validated_generation,
                session_generation,
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
        // 接続が stream を保持する
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
                            // (draft-ietf-moq-transport-23 §6.6.1 (Graceful Session Migration) /
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
                                                plan: {
                                                    let (plan, resolve_as_ended) =
                                                        plan_for_migration(
                                                            &resubscribe_plan,
                                                            &permanently_ended_tracks,
                                                            &publish_done_pending,
                                                        );
                                                    for (request_id, kind) in
                                                        resolve_as_ended
                                                    {
                                                        // 移行開始時点で恒久終了扱いに切り替える
                                                        // (cleanup 完了を待たずに plan から外す)
                                                        publish_done_pending
                                                            .remove(&request_id);
                                                        permanently_ended_tracks.insert(kind);
                                                    }
                                                    plan
                                                },
                                                last_locations,
                                                deadline,
                                            };
                                            migration_boundary_snapshot = Some(last_locations);
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
                                            return Ok(false);
                                        }
                                        if ignored_publish_requests.contains_key(&request_id) {
                                            // SUBSCRIBE_TRACKS の結果として受理した PUBLISH は
                                            // publisher 起点の購読であり、自側からは再発行できない。
                                            // 当該 request の終端は Session の期限に任せる
                                            tracing::info!(
                                                "Ignoring a request-scoped GOAWAY for a PUBLISH-originated subscription: request_id={request_id}"
                                            );
                                            return Ok(false);
                                        }
                                        // SUBSCRIBE 系の request とメディア FETCH のどちらか
                                        let Some(request_target) = goaway_request_target(
                                            request_id,
                                            catalog_request_id,
                                            video_request_id,
                                            audio_request_id,
                                            &fetch_requests,
                                        ) else {
                                            tracing::warn!(
                                                "Ignoring a request-scoped GOAWAY for an unknown request: request_id={request_id}"
                                            );
                                            return Ok(false);
                                        };
                                        let deadline =
                                            MigrationDeadline::after_goaway(Instant::now(), timeout);
                                        // 移行先が別 URL の場合は、当該 request だけを
                                        // 専用タスクが確立した side session で受信する。
                                        // 旧 session はそのまま維持し、他の request と
                                        // data plane の受信を続ける (§9.2)。同じ URL を指す
                                        // GOAWAY は現在の session での再発行を意味する
                                        // (`migration_target` の契約)
                                        let new_session_url =
                                            !same_session_url(&target.url, &config.url);
                                        // 移行先 URI の msf fragment の connection が現在の
                                        // 接続経路と矛盾する場合は移行しない
                                        // (draft-ietf-moq-msf-01 §11.1.1)
                                        if new_session_url
                                            && let Err(e) = tokio_moq::resolve_transport(
                                                Some(config.transport),
                                                &target.url,
                                            )
                                        {
                                            tracing::warn!(
                                                "GOAWAY migration target conflicts with the current transport: {e}"
                                            );
                                            return Ok(false);
                                        }
                                        // side session へ渡す共有状態を組み立てる。世代だけは
                                        // side session の起動時に決まるため引数で受ける
                                        let shared_for_side_session =
                                            |generation: u64| RequestMigrationShared {
                                                frame_tx: frame_tx.clone(),
                                                audio_tx: audio_tx.clone(),
                                                catalog_tx: catalog_tx.clone(),
                                                termination_tx: termination_tx.clone(),
                                                audio_decoder: audio_decoder.clone(),
                                                video_decode_order: std::sync::Arc::clone(
                                                    &video_decode_order,
                                                ),
                                                video_delivery_order: std::sync::Arc::clone(
                                                    &video_delivery_order,
                                                ),
                                                display_backlog: std::sync::Arc::clone(
                                                    &display_backlog,
                                                ),
                                                group_switch: std::sync::Arc::clone(&group_switch),
                                                audio_catch_up: std::sync::Arc::clone(
                                                    &audio_catch_up,
                                                ),
                                                audio_delivery_order: std::sync::Arc::clone(
                                                    &audio_delivery_order,
                                                ),
                                                last_received_locations: std::sync::Arc::clone(
                                                    &last_received_locations,
                                                ),
                                                audio_config_validated_generation:
                                                    std::sync::Arc::clone(
                                                        &audio_config_validated_generation,
                                                    ),
                                                generation,
                                                pending_subgroups: std::sync::Arc::clone(
                                                    &pending_subgroups,
                                                ),
                                                recorder: recorder_sender.clone(),
                                                playback,
                                                video_codec: video_codec.clone(),
                                                audio_codec: audio_codec.clone(),
                                                audio_params,
                                                stream_slots: std::sync::Arc::clone(&stream_slots),
                                            };
                                        match request_target {
                                            GoawayRequestTarget::Subscribe(kind) => {
                                                // 終端済み購読の request stream GOAWAY の扱いを
                                                // 1 か所で決める (`reissue_request_target` 参照)。
                                                // tick の entry 解消と後着 GOAWAY の順序で
                                                // `publish_done_pending` が空になっても、
                                                // `permanently_ended_tracks` /
                                                // `migration_pending_tracks` の状態で判定が
                                                // 崩れない
                                                let Some(kind) = reissue_request_target(
                                                    kind,
                                                    publish_done_pending
                                                        .get(&request_id)
                                                        .map(|(_, disposition)| *disposition),
                                                    &permanently_ended_tracks,
                                                ) else {
                                                    return Ok(false);
                                                };
                                                let last_locations = *last_received_locations
                                                    .lock()
                                                    .expect(
                                                        "last locations lock is not poisoned",
                                                    );
                                                if new_session_url {
                                                    tracing::info!(
                                                        "Requesting a request migration to a new session: kind={kind:?}, request_id={request_id}, timeout={timeout}ms, local_default={}, last_location={:?}",
                                                        deadline.uses_local_default(),
                                                        last_locations.get(kind),
                                                    );
                                                    // side session の世代は旧 session より
                                                    // 新しくする。Audio Config の検証済み
                                                    // 世代と比べ、side session で最初に届いた
                                                    // Audio Config を処理させる。世代は
                                                    // dispatch が session を起動するときだけ
                                                    // 1 本のカウンタから払い出す
                                                    dispatch_side_session_command(
                                                        &mut request_migration_sessions,
                                                        &mut request_migration_tasks,
                                                        &task_monitor,
                                                        config.transport,
                                                        config.cert.clone(),
                                                        &request_migration_tx,
                                                        target.url.clone(),
                                                        SideSessionCommand::Reissue {
                                                            kind,
                                                            old_request_id: request_id,
                                                            plan: resubscribe_plan.clone(),
                                                            last_locations,
                                                            deadline,
                                                        },
                                                        &mut session_generation,
                                                        shared_for_side_session,
                                                    )
                                                    .await;
                                                    return Ok(false);
                                                }
                                                tracing::info!(
                                                    "Re-issuing the {kind:?} request after a request-scoped GOAWAY: request_id={request_id}"
                                                );
                                                // PUBLISH_DONE 済みの購読 (GoingAway の entry あり)
                                                // は `Session::stop_sending` が Terminated を拒否する
                                                // ため送らない (tick 解消済みの移行待ちも送らない)
                                                let send_stop_sending = reissue_send_stop_sending(
                                                    publish_done_pending
                                                        .get(&request_id)
                                                        .map(|(_, disposition)| *disposition),
                                                    &permanently_ended_tracks,
                                                    &migration_pending_tracks,
                                                    kind,
                                                );
                                                match reissue_request(
                                                    &mut client,
                                                    kind,
                                                    request_id,
                                                    &resubscribe_plan,
                                                    &last_locations,
                                                    send_stop_sending,
                                                )
                                                .await
                                                {
                                                Ok(reissued) => {
                                                    // 旧 alias を外して新しい alias を登録する
                                                    // (alias は peer が決めるため変わりうる)
                                                    let old_alias = match kind {
                                                        TrackKind::Catalog => catalog_alias,
                                                        TrackKind::Video => video_alias,
                                                        TrackKind::Audio => audio_alias,
                                                        // 再発行は catalog / 映像 / 音声だけを
                                                        // 対象とするため通常は到達しない
                                                        TrackKind::Discard => ignored_publish_requests
                                                            .get(&request_id)
                                                            .copied(),
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
                                                                Some(reissued.subscription.request_id);
                                                            catalog_alias = Some(new_alias);
                                                        }
                                                        TrackKind::Video => {
                                                            video_request_id = Some(
                                                                reissued.subscription.request_id,
                                                            );
                                                            video_alias = Some(new_alias);
                                                            // 同じ session で購読を張り直した
                                                            // ため、復号順と購読境界の状態は
                                                            // 引き継ぎ、境界だけを更新する。
                                                            // 候補は再発行の前に取得した最後の
                                                            // 受信位置である (選び方は
                                                            // select_video_subscription_boundary)
                                                            let video_boundary_candidates =
                                                                VideoBoundaryCandidates {
                                                                    snapshot: last_locations
                                                                        .get(TrackKind::Video),
                                                                    reread: last_received_locations
                                                                        .lock()
                                                                        .expect(
                                                                            "last locations lock is not poisoned",
                                                                        )
                                                                        .get(TrackKind::Video),
                                                                };
                                                            let video_boundary_candidate =
                                                                select_video_subscription_boundary(
                                                                    ResubscribeRoute::RequestReissue,
                                                                    video_boundary_candidates,
                                                                );
                                                            let video_boundary = {
                                                                let mut order = video_delivery_order
                                                                    .lock()
                                                                    .expect(
                                                                        "video delivery order lock is not poisoned",
                                                                    );
                                                                order.advance_boundary(
                                                                    video_boundary_candidate,
                                                                );
                                                                order.boundary()
                                                            };
                                                            tracing::info!(
                                                                "Updated the video subscription boundary for the re-issued request: candidates={video_boundary_candidates:?}, candidate={video_boundary_candidate:?}, boundary={video_boundary:?}"
                                                            );
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
                                                        TrackKind::Discard => {
                                                            // `reissue_request` は PUBLISH 起点の
                                                            // 購読を再発行しないため到達しない。
                                                            // 万一到達しても状態を変えない
                                                            tracing::warn!(
                                                                "Re-issued a PUBLISH-originated subscription unexpectedly: request_id={request_id}"
                                                            );
                                                        }
                                                    }
                                                    // 再発行に伴う FETCH の対応表を足す。同じ
                                                    // track の旧 Request ID は、遅延して届く
                                                    // FETCH / fill stream を誤配送しないよう先に外す
                                                    {
                                                        let mut targets =
                                                            lock_fetch_targets(&fetch_targets);
                                                        targets.retain(|_, target| {
                                                            Some(*target) != fetch_target_of(kind)
                                                        });
                                                        targets.extend(reissued.fetch_targets);
                                                    }
                                                    // 映像の購読を張り直すと、その再発行に伴う
                                                    // FETCH は上の表へ入る。旧 session の映像
                                                    // FETCH の再発行用の指定は対象が入れ替わるため
                                                    // 捨てる
                                                    if kind == TrackKind::Video {
                                                        fetch_requests.clear();
                                                    }
                                                    fetch_requests.extend(reissued.fetch_requests);
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
                                                        session_generation,
                                                    ));
                                                    // 再発行後の request の active と pending 管理
                                                    active_requests.remove(&request_id);
                                                    active_requests
                                                        .insert(reissued.subscription.request_id);
                                                    publish_done_pending.remove(&request_id);
                                                    // 移行待ち (GoingAway 済み) の track は再発行で
                                                    // active に戻るため、集合から外す
                                                    migration_pending_tracks.remove(&kind);
                                                    tracing::info!(
                                                        "Re-issued the {kind:?} request: alias={new_alias}, request_id={}",
                                                        reissued.subscription.request_id,
                                                    );
                                                }
                                                Err(e) => {
                                                    // 再発行に失敗しても session は維持する
                                                    // (当該購読だけが失われ、live の受信は他の
                                                    // 購読で続く)。旧 request の購読は再発行の前の
                                                    // STOP_SENDING 送信で終端済みであるため、
                                                    // shutdown 時の STOP_SENDING 対象から外す
                                                    active_requests.remove(&request_id);
                                                    tracing::warn!(
                                                        "Failed to re-issue request {request_id} after a request-scoped GOAWAY: {e}"
                                                    );
                                                }
                                                }
                                            }
                                            GoawayRequestTarget::Fetch => {
                                                // 映像トラックが恒久終了している場合、FETCH は
                                                // 発行し直さない (応答を使う経路が無い)
                                                if permanently_ended_tracks
                                                    .contains(&TrackKind::Video)
                                                {
                                                    tracing::info!(
                                                        "Ignoring a request-scoped GOAWAY for a FETCH of the permanently ended video track: request_id={request_id}"
                                                    );
                                                    return Ok(false);
                                                }
                                                let Some(fetch_request) =
                                                    fetch_requests.get(&request_id).cloned()
                                                else {
                                                    // `goaway_request_target` が存在を確認して
                                                    // いるため到達しない
                                                    return Ok(false);
                                                };
                                                if new_session_url {
                                                    tracing::info!(
                                                        "Requesting a FETCH migration to a new session: request_id={request_id}, timeout={timeout}ms, local_default={}",
                                                        deadline.uses_local_default(),
                                                    );
                                                    dispatch_side_session_command(
                                                        &mut request_migration_sessions,
                                                        &mut request_migration_tasks,
                                                        &task_monitor,
                                                        config.transport,
                                                        config.cert.clone(),
                                                        &request_migration_tx,
                                                        target.url.clone(),
                                                        SideSessionCommand::ReissueFetch {
                                                            old_request_id: request_id,
                                                            target: fetch_request.target,
                                                            parameters: fetch_request.parameters,
                                                            deadline,
                                                        },
                                                        &mut session_generation,
                                                        shared_for_side_session,
                                                    )
                                                    .await;
                                                    return Ok(false);
                                                }
                                                // 同じ URL を指す GOAWAY は現在の session での
                                                // 再発行を意味するため、旧 request を終端して
                                                // 同じ要求内容で発行し直す
                                                let reissued = reissue_fetch(
                                                    &mut client,
                                                    request_id,
                                                    &fetch_request.target,
                                                    &fetch_request.parameters,
                                                    true,
                                                )
                                                .await;
                                                // 旧 Request ID の応答を誤配送しないよう先に外す
                                                lock_fetch_targets(&fetch_targets)
                                                    .remove(&request_id);
                                                fetch_requests.remove(&request_id);
                                                match reissued {
                                                    Ok(new_request_id) => {
                                                        lock_fetch_targets(&fetch_targets)
                                                            .insert(
                                                                new_request_id,
                                                                FetchTarget::Video,
                                                            );
                                                        fetch_requests
                                                            .insert(new_request_id, fetch_request);
                                                        tracing::info!(
                                                            "Re-issued the FETCH after a request-scoped GOAWAY: old_request_id={request_id}, new_request_id={new_request_id}"
                                                        );
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!(
                                                            "Failed to re-issue FETCH request {request_id} after a request-scoped GOAWAY: {e}"
                                                        );
                                                    }
                                                }
                                                return Ok(false);
                                            }
                                        }
                                        Ok(false)
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
                            request_id,
                            status_code,
                            stream_count,
                            ..
                        })) => {
                            // draft-ietf-moq-transport-23 §9.9 (PUBLISH_DONE) はその
                            // subscription の終端であり、§12.4 (Publish Done Codes) は
                            // status ごとに意味が異なる。1 track の終了でパイプライン全体を
                            // 停止させない。Session は終端済み subscription を drain timer
                            // と Stream Count の完了条件を持って回収するため、example 側は
                            // それを待ってから track を終端させる
                            let disposition = publish_done_disposition(
                                normalize_publish_done_code(status_code),
                            );
                            tracing::info!(
                                "Received PUBLISH_DONE (status_code={status_code}, \
                                 normalized_status_code={}, stream_count={stream_count}, \
                                 disposition={:?}, request_id={request_id})",
                                normalize_publish_done_code(status_code),
                                disposition,
                            );
                            // SUBSCRIBE_TRACKS の結果として受理した購読の PUBLISH_DONE は、
                            // 映像・音声の終了判定に使わない。Session の drain が終わるまで
                            // alias を残し、tick で track_map から外す
                            if ignored_publish_requests.contains_key(&request_id) {
                                publish_done_pending
                                    .insert(request_id, (TrackKind::Discard, disposition));
                                tracing::info!(
                                    "PUBLISH_DONE for a PUBLISH-originated subscription: \
                                     request_id={request_id}, disposition={disposition:?}"
                                );
                                return Ok(false);
                            }
                            match goaway_request_kind(
                                request_id,
                                catalog_request_id,
                                video_request_id,
                                audio_request_id,
                            ) {
                                Some(kind) => {
                                    // 終了待ちの request を Session の cleanup_ready の
                                    // 完了まで追う。shutdown 時の STOP_SENDING はこの
                                    // request に送らないため active_requests から外す
                                    active_requests.remove(&request_id);
                                    match publish_done_pending.get(&request_id) {
                                        Some((_, existing)) if *existing == disposition => {
                                            // 同じ request に重複した PUBLISH_DONE。
                                            // 既存の disposition は維持する
                                        }
                                        Some((_, existing)) => {
                                            // disposition が変わった (通常は起こらない)。
                                            // 既存の disposition と異なる PUBLISH_DONE であった
                                            // 場合は新しい disposition で上書きする
                                            tracing::warn!(
                                                "PUBLISH_DONE disposition conflicts with a pending PUBLISH_DONE: request_id={request_id}, existing={existing:?}, new={:?}",
                                                disposition,
                                            );
                                            publish_done_pending.insert(
                                                request_id,
                                                (kind, disposition),
                                            );
                                        }
                                        None => {
                                            publish_done_pending.insert(
                                                request_id,
                                                (kind, disposition),
                                            );
                                        }
                                    }
                                    if let Some((_, disposition)) =
                                        publish_done_pending.get(&request_id)
                                    {
                                        let message = match disposition {
                                            PublishDoneDisposition::Error => {
                                                "error status; the track will end after drain".to_string()
                                            }
                                            PublishDoneDisposition::TrackEnded => {
                                                "the track ended".to_string()
                                            }
                                            PublishDoneDisposition::GoingAway => {
                                                "the session is going away; the track will be re-established after a migration".to_string()
                                            }
                                        };
                                        // エラーだけを warn にする (正常終端と移行待ちは info)。
                                        // PUBLISH_DONE 送信直後に GOAWAY が届くことがあるため、
                                        // PUBLISH_DONE を移行のトリガーにしない
                                        if PublishDoneDisposition::Error == *disposition {
                                            tracing::warn!("PUBLISH_DONE error: {message}");
                                        } else {
                                            tracing::info!("PUBLISH_DONE: {message}");
                                        }
                                    }
                                }
                                // 移行で request_id は session ごとに変わりうるため、現在の
                                // request_id に対応しないものは無視する。
                                None => {
                                    tracing::debug!(
                                        "Ignoring a PUBLISH_DONE for an unknown request: request_id={request_id}"
                                    );
                                }
                            }
                            Ok(false)
                        }
                        Some(ClientEvent::Session(SessionEvent::FetchOkReceived {
                            request_id,
                            end_location,
                            ..
                        })) => {
                            // 自側が発行したメディア FETCH の応答である。FETCH_OK は応答範囲の
                            // 終端 (LARGEST_OBJECT または要求の End Location) だけを示し、
                            // Track / Group の終端はデータストリームの End of Track /
                            // End of Group で通知される (draft-ietf-moq-transport-23
                            // §3.2.2 (Gaps in a Fetch Stream) / §11.4.2.2 (Prior Object
                            // Fields))。そのため旧 FETCH_OK の End of Track フラグは読まず、
                            // 対応表への記録は応答 stream が FETCH_OK より先に届きうるため
                            // FETCH の発行時に行っておく。
                            // 配送先は共有の対応表で引く。request 単位移行の side session が
                            // 再発行した FETCH は side session 側の表にだけ入るため、ここで
                            // 見つからなくても異常ではない
                            let kind = lock_fetch_targets(&fetch_targets)
                                .get(&request_id)
                                .copied();
                            tracing::info!(
                                "Received FETCH_OK: request_id={request_id}, kind={kind:?}, \
                                 end_location=({}, {})",
                                end_location.group_id,
                                end_location.object_id,
                            );
                            Ok(false)
                        }
                        Some(ClientEvent::Session(SessionEvent::RequestErrorReceived {
                            request_id,
                            error_code,
                            retry_interval,
                            reason,
                            error_details,
                        })) => {
                            // 自側が送った request が peer に拒否された
                            // (draft-ietf-moq-transport-23 §9.4 (REQUEST_ERROR))。映像のメディア
                            // FETCH の拒否なら応答 stream は届かないため、対応表から外す。
                            // 対応表には catalog の購読 (`FetchTarget::Catalog`) の Request ID も
                            // 入るため、値で映像 FETCH かを判定する。それ以外 (映像 SUBSCRIBE への
                            // REQUEST_UPDATE の失敗など) はログに残すだけで、対応表を変えない。
                            // この example はリダイレクトに追随せず、再試行もしない
                            let video_fetch = {
                                let mut targets = lock_fetch_targets(&fetch_targets);
                                if matches!(targets.get(&request_id), Some(FetchTarget::Video)) {
                                    targets.remove(&request_id);
                                    true
                                } else {
                                    false
                                }
                            };
                            if video_fetch {
                                fetch_requests.remove(&request_id);
                            }
                            // SUBSCRIBE_TRACKS が拒否された場合は、以後に同じ
                            // SUBSCRIBE_TRACKS_ID を持つ PUBLISH を受理しないよう対応を外す
                            if tracks_request_id == Some(request_id) {
                                tracks_request_id = None;
                            }
                            tracing::warn!(
                                "Received REQUEST_ERROR: request_id={request_id}, \
                                 video_fetch={video_fetch}, error_code={error_code:#x}, \
                                 retry_interval={retry_interval}, reason={}, redirect={}",
                                reason.as_str(),
                                error_details.redirect().is_some(),
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
                            // 観測できるようにログだけ残す (draft-ietf-moq-transport-23
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
                            // SUBSCRIBE_TRACKS の結果として届いた PUBLISH だけは受理する。
                            //
                            // draft-ietf-moq-transport-23 §3.6 (Subscribing to Tracks by
                            // Prefix) / §9.8 (PUBLISH): publisher は namespace prefix に一致
                            // する track を PUBLISH し、購読者は PUBLISH_OK (REQUEST_OK) で
                            // 応答する。Track Alias は PUBLISH が運ぶ値をそのまま使う。
                            if let ControlMessage::Publish(publish) = &request.message
                                && accepts_subscribe_tracks_publish(publish, tracks_request_id)
                            {
                                let alias = publish.track_alias;
                                let namespace = publish.track_namespace.clone();
                                let track_name = publish.track_name.clone();
                                let duplicate = is_already_subscribed_publish(
                                    publish,
                                    &subscribed_tracks,
                                );
                                // 既存の購読と同じ alias が使われると、data stream がどちらの
                                // 購読のものか alias だけでは区別できない。同じ track に対する
                                // alias の共有は §3.1.3 が許可しているため、既存の再生経路を
                                // 壊さないよう、この PUBLISH は受理しない
                                let alias_taken = track_map
                                    .read()
                                    .expect("track_map lock must not be poisoned")
                                    .contains_key(&alias);
                                if alias_taken {
                                    tracing::warn!(
                                        "Rejecting a PUBLISH that reuses an existing track alias: \
                                         request_id={}, namespace={}, track={}, alias={alias}",
                                        request.request_id,
                                        serialize_namespace(&namespace),
                                        serialize_track_name(&track_name),
                                    );
                                    client
                                        .send_request_error(
                                            request.request_id,
                                            shiguredo_moqt::error::REQUEST_UNINTERESTED,
                                            "track alias is already used by another subscription",
                                        )
                                        .await?;
                                    return Ok(false);
                                }
                                // PUBLISH_OK を送る前に alias を登録する。publisher は
                                // PUBLISH_OK の直後に data stream を開きうるため、登録が
                                // 遅れると未知 alias として保留されてしまう
                                let datagram_map = {
                                    let mut map = track_map
                                        .write()
                                        .expect("track_map lock must not be poisoned");
                                    map.insert(alias, TrackKind::Discard);
                                    Arc::new(map.clone())
                                };
                                match client.send_request_ok(request.request_id).await {
                                    Ok(()) => {
                                        ignored_publish_requests.insert(request.request_id, alias);
                                        // datagram 受信タスクの alias 表も差し替える
                                        let _ = datagram_session_tx.send((
                                            client.handle(),
                                            data_plane.clone(),
                                            datagram_map,
                                            session_generation,
                                        ));
                                        tracing::info!(
                                            "Accepted PUBLISH for SUBSCRIBE_TRACKS: request_id={}, \
                                             namespace={}, track={}, alias={alias}, \
                                             duplicate={duplicate}",
                                            request.request_id,
                                            serialize_namespace(&namespace),
                                            serialize_track_name(&track_name),
                                        );
                                    }
                                    Err(e) => {
                                        // 受理に失敗した PUBLISH の alias を残すと、届いた
                                        // stream を読み捨てる対象として保持し続けるため、
                                        // 登録を取り消す
                                        let datagram_map = {
                                            let mut map = track_map
                                                .write()
                                                .expect("track_map lock must not be poisoned");
                                            map.remove(&alias);
                                            Arc::new(map.clone())
                                        };
                                        let _ = datagram_session_tx.send((
                                            client.handle(),
                                            data_plane.clone(),
                                            datagram_map,
                                            session_generation,
                                        ));
                                        tracing::warn!(
                                            "Failed to accept PUBLISH for SUBSCRIBE_TRACKS: \
                                             request_id={}, {e}",
                                            request.request_id,
                                        );
                                    }
                                }
                                return Ok(false);
                            }
                            // subscriber は上記以外の peer request を処理しない。PUBLISH は
                            // 「不要」、それ以外は「未対応」として拒否する。
                            let (code, reason) = request_rejection(&request.message);
                            tracing::warn!(
                                "Rejecting unexpected peer request: request_id={} code={code:#x}",
                                request.request_id
                            );
                            client
                                .send_request_error(request.request_id, code, reason)
                                .await?;
                            Ok(false)
                        }
                        Some(ClientEvent::RequestUpdate(update)) => {
                            // draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE):
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
                        Some(ClientEvent::Session(SessionEvent::PublishSkippedReceived {
                            request_id,
                            track_namespace,
                            track_name,
                        })) => {
                            // draft-ietf-moq-transport-23 §9.18 (PUBLISH_SKIPPED):
                            // SUBSCRIBE_TRACKS に一致したが購読を作らなかった track である。
                            // 状態は変えず、観測できるようにログだけ残す
                            tracing::info!(
                                "Received PUBLISH_SKIPPED: request_id={request_id}, \
                                 namespace={}, track={}",
                                serialize_namespace(&track_namespace),
                                serialize_track_name(&track_name),
                            );
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
                    // MOQT メッセージの encode / decode 失敗。draft-ietf-moq-transport-23 §9
                    // (Control Messages) と §9.19.1 (Parameter Scope) は不正なメッセージの受信を
                    // 終了コード付きで閉じることを MUST で要求する。decode は codec 層で完結し
                    // Session はこの違反を観測できないため、I/O 層である main ループが終了コードを
                    // 決めて閉じる
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
                        // request 単位移行の side session をすべて止める。移行先の session は
                        // 全 track を張り直すため、同じ track を 2 つの session から受信
                        // しないようにする
                        let side_session_count = request_migration_tasks.len();
                        for handle in request_migration_tasks.drain(..) {
                            handle.abort();
                            let _ = handle.await;
                        }
                        request_migration_sessions.clear();
                        // abort 済みの side session が送信済みのイベントは、移行後の状態を
                        // 壊しうる (古い request id で購読の状態を消すなど)。集約チャネルを
                        // 作り直して、これらのイベントを捨てる
                        let (new_tx, new_rx) = tokio::sync::mpsc::channel::<RequestMigrationEvent>(
                            REQUEST_MIGRATION_EVENT_CHANNEL_SIZE,
                        );
                        request_migration_tx = new_tx;
                        request_migration_rx = new_rx;
                        if side_session_count > 0 {
                            tracing::info!(
                                "Stopped the request migration side sessions after a full session migration: count={side_session_count}"
                            );
                        }
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
                        // Group の切り替え gate も初期化する。旧 session の stream task は
                        // abort しても await 点でしか止まらず、drain の打ち切り後も動き得る
                        // ため、世代を進めて旧い task の begin_stream / end_stream /
                        // admit を無視する
                        group_switch.reset();
                        audio_catch_up
                            .lock()
                            .expect("audio catch up lock is not poisoned")
                            .reset();
                        // 音声の購読境界を「旧 session で最後に受信した Location」へ更新する。
                        // 移行先の購読はその位置を Location Filter の Absolute Start (0x02) に
                        // して張り直すため、境界と同じ位置だけが再送される。移行先の Largest
                        // Object を境界にすると、移行中に抜けた音声 (最後の受信位置より後) を
                        // 境界以下として捨ててしまう (draft-ietf-moq-transport-23 §9.19.9
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
                        // 映像の購読境界も更新する。候補は旧 session の stream task を止めた
                        // あとに読み直した最後の受信位置である (選び方は
                        // select_video_subscription_boundary)
                        let video_boundary_candidates = VideoBoundaryCandidates {
                            snapshot: migration_boundary_snapshot
                                .take()
                                .and_then(|locations| locations.video),
                            reread: last_received_locations
                                .lock()
                                .expect("last locations lock is not poisoned")
                                .video,
                        };
                        let video_boundary_candidate = select_video_subscription_boundary(
                            ResubscribeRoute::SessionMigration,
                            video_boundary_candidates,
                        );
                        let video_boundary = {
                            let mut order = video_delivery_order
                                .lock()
                                .expect("video delivery order lock is not poisoned");
                            order.advance_boundary(video_boundary_candidate);
                            order.boundary()
                        };
                        tracing::info!(
                            "Updated the video subscription boundary for the migrated session: candidates={video_boundary_candidates:?}, candidate={video_boundary_candidate:?}, boundary={video_boundary:?}"
                        );
                        // alias 表を移行先のものへ差し替える (alias は peer が決めるため変わる)
                        let datagram_map = {
                            let mut map = track_map
                                .write()
                                .expect("track_map lock must not be poisoned");
                            map.clear();
                            if let Some(ref sub) = outcome.resubscribed.catalog {
                                map.insert(sub.track_alias, TrackKind::Catalog);
                            }
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
                        // 新しい session の世代 を進める。Audio Config の検証済み世代 と
                        // 比べることで、移行前の session の遅延 Object が新しい session の
                        // 検証状態を進めないようにする
                        let new_generation = {
                            session_generation = session_generation.saturating_add(1);
                            session_generation
                        };
                        let _ = datagram_session_tx.send((
                            outcome.client.handle(),
                            outcome.data_plane.clone(),
                            datagram_map,
                            new_generation,
                        ));
                        // 旧 session は STOP_SENDING を送ってから NO_ERROR で閉じる (§6.6.1 /
                        // §9.2)。GOAWAY の Timeout は待たず、main ループを塞がないよう別タスクで
                        // 行う。PUBLISH_DONE で Terminated になった購読と Session が回収済み
                        // の購読は STOP_SENDING を送らない (active_requests から外れている)
                        let old_request_ids: Vec<u64> =
                            active_requests.iter().copied().collect();
                        // 終了待ち entry は Session 入れ替えで request_id がすべて新しくなるため
                        // 解消する (張り直した track と旧 request_id のどちらも、移行後の poll で
                        // 誤って終了しない)
                        publish_done_pending.clear();
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
                        catalog_request_id = outcome
                            .resubscribed
                            .catalog
                            .as_ref()
                            .map(|sub| sub.request_id);
                        catalog_alias = outcome
                            .resubscribed
                            .catalog
                            .as_ref()
                            .map(|sub| sub.track_alias);
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
                        fetch_targets = std::sync::Arc::new(std::sync::Mutex::new(
                            outcome.resubscribed.fetch_targets,
                        ));
                        fetch_requests = outcome.resubscribed.fetch_requests;
                        // 移行先の subscription を active 側に載せ替える
                        active_requests.clear();
                        if let Some(ref sub) = outcome.resubscribed.catalog {
                            active_requests.insert(sub.request_id);
                        }
                        if let Some(ref sub) = outcome.resubscribed.video {
                            active_requests.insert(sub.request_id);
                        }
                        if let Some(ref sub) = outcome.resubscribed.audio {
                            active_requests.insert(sub.request_id);
                        }
                        // 移行先で張り直した media track は再開したため、移行待ちをクリアする
                        // (恒久終了の track は維持する)
                        migration_pending_tracks.clear();
                        wt_session_state_rx = new_wt_session_state;
                        tracing::info!(
                            "Migrated the session to {}",
                            config.url.authority
                        );
                        // SUBSCRIBE_TRACKS は session 単位の namespace 要求であり、移行先で
                        // 張り直す必要がある。PUBLISH 起点の購読と Request ID は旧 session の
                        // ものなので破棄する (track_map は上の差し替えで catalog / 映像 / 音声
                        // だけになっている)
                        ignored_publish_requests.clear();
                        tracks_request_id = None;
                        if config.subscribe_tracks {
                            match client
                                .subscribe_tracks(
                                    subscribe_tracks_namespace.clone(),
                                    MessageParameters::new(),
                                )
                                .await
                            {
                                Ok(request_id) => {
                                    tracks_request_id = Some(request_id);
                                    tracing::info!(
                                        "Re-sent SUBSCRIBE_TRACKS on the migrated session: \
                                         request_id={request_id}"
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to re-send SUBSCRIBE_TRACKS on the migrated \
                                         session: {e}"
                                    );
                                }
                            }
                        }
                    }
                    Ok(Err(failure)) => {
                        // 期限超過・再試行不能なエラーである。旧 session はまだ生きており、
                        // GOAWAY は subscription state に影響しない
                        // (draft-ietf-moq-transport-23 §9.2。draft 由来の仕様であり将来変更
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
            Some(event) = request_migration_rx.recv() => {
                match event {
                    RequestMigrationEvent::Migrated {
                        url,
                        request,
                        old_request_id,
                        new_request_id,
                    } => {
                        // side session の進行状態を「確立済み」へ進める。同じ URL の次の
                        // request はこの session に相乗りする
                        if let Some(slot) = request_migration_sessions.get_mut(&url) {
                            slot.state = SideSessionState::Established;
                        }
                        match request {
                            MigratedRequest::Subscribe(kind) => {
                                // 旧 session の request stream を閉じる (§9.2 の "close the
                                // old request stream using the appropriate mechanism")。
                                // 購読が既に Terminated の場合は状態機械に拒否されるが、
                                // 移行自体は成功しているため警告に留める
                                if let Err(e) = client.stop_sending(old_request_id).await {
                                    tracing::warn!(
                                        "Failed to send STOP_SENDING for the migrated request {old_request_id}: {e}"
                                    );
                                }
                                // 旧 alias を外す。以後の Object は side session が配送する
                                let old_alias = match kind {
                                    TrackKind::Catalog => catalog_alias.take(),
                                    TrackKind::Video => video_alias.take(),
                                    TrackKind::Audio => audio_alias.take(),
                                    // 移行は catalog / 映像 / 音声だけを対象とするため
                                    // 到達しない
                                    TrackKind::Discard => None,
                                };
                                {
                                    let mut map = track_map
                                        .write()
                                        .expect("track_map lock must not be poisoned");
                                    if let Some(alias) = old_alias {
                                        map.remove(&alias);
                                    }
                                }
                                // 以後その track は side session が配送するため、main ループの
                                // 再発行・期限処理の対象から外す。request id を `None` にすると
                                // `goaway_request_kind` の照合対象からも外れる
                                match kind {
                                    TrackKind::Catalog => catalog_request_id = None,
                                    TrackKind::Video => video_request_id = None,
                                    TrackKind::Audio => audio_request_id = None,
                                    TrackKind::Discard => {}
                                }
                                active_requests.remove(&old_request_id);
                                // 移行の開始から成功までの間に旧 session で PUBLISH_DONE を
                                // 受けていた場合は disposition を引き継ぐ。track の恒久終了を
                                // 移行成功で失わないようにするためである
                                let pending_disposition = publish_done_pending
                                    .remove(&old_request_id)
                                    .map(|(_, disposition)| disposition);
                                migration_pending_tracks.remove(&kind);
                                // 旧 session のこの track の FETCH は side session の再発行で
                                // 置き換わるため、main ループの対応表から外す
                                lock_fetch_targets(&fetch_targets).retain(|_, target| {
                                    Some(*target) != fetch_target_of(kind)
                                });
                                // 映像の購読を移行すると、それに伴う FETCH も side session が
                                // 発行し直す。main ループに残った FETCH の再発行用の指定は
                                // 対象を失っているため捨てる
                                if kind == TrackKind::Video {
                                    fetch_requests.clear();
                                }
                                match pending_disposition {
                                    Some(
                                        PublishDoneDisposition::TrackEnded
                                        | PublishDoneDisposition::Error,
                                    ) => {
                                        tracing::warn!(
                                            "The {kind:?} track ended permanently during the request migration"
                                        );
                                        permanently_ended_tracks.insert(kind);
                                        if all_media_tracks_ended(
                                            subscribe_video,
                                            subscribe_audio,
                                            &permanently_ended_tracks,
                                            &migration_pending_tracks,
                                        ) {
                                            tracing::info!(
                                                "All media tracks ended permanently: ending the pipeline"
                                            );
                                            peer_ended = true;
                                            break 'main;
                                        }
                                    }
                                    Some(PublishDoneDisposition::GoingAway) | None => {}
                                }
                                // datagram 受信タスクへ新しい alias 表を渡す。旧 session の
                                // 遅延した datagram を移行後の track として配送しないためである
                                let datagram_map = Arc::new(
                                    track_map
                                        .read()
                                        .expect("track_map lock must not be poisoned")
                                        .clone(),
                                );
                                let _ = datagram_session_tx.send((
                                    client.handle(),
                                    data_plane.clone(),
                                    datagram_map,
                                    session_generation,
                                ));
                                tracing::info!(
                                    "Migrated the {kind:?} request to the new session: \
                                     url={url}, old_request_id={old_request_id}, \
                                     new_request_id={new_request_id}"
                                );
                            }
                            MigratedRequest::Fetch => {
                                // 旧 session の request stream を閉じる。応答は side session が
                                // 受けるため、main ループの対応表から外す
                                stop_fetch_request_stream(&mut client, old_request_id).await;
                                lock_fetch_targets(&fetch_targets).remove(&old_request_id);
                                fetch_requests.remove(&old_request_id);
                                tracing::info!(
                                    "Migrated the media FETCH request to the new session: \
                                     url={url}, old_request_id={old_request_id}, \
                                     new_request_id={new_request_id}"
                                );
                            }
                        }
                    }
                    RequestMigrationEvent::Failed {
                        url,
                        request,
                        old_request_id,
                        reason,
                    } => match request {
                        MigratedRequest::Subscribe(kind) => {
                            // 期限切れ・接続失敗・再発行失敗である。control GOAWAY の移行
                            // 失敗と同じく警告を出し、旧 session の終了経路へ進む
                            tracing::warn!(
                                "Failed to migrate the {kind:?} request to a new session: \
                                 url={url}, request_id={old_request_id}: {reason}"
                            );
                            break 'main;
                        }
                        MigratedRequest::Fetch => {
                            // FETCH は欠落した Group を補う補助であり、移行に失敗しても購読は
                            // 続く。旧 session の request を終端し、対応表から外す
                            tracing::warn!(
                                "Failed to migrate the FETCH request to a new session: \
                                 url={url}, request_id={old_request_id}: {reason}"
                            );
                            stop_fetch_request_stream(&mut client, old_request_id).await;
                            lock_fetch_targets(&fetch_targets).remove(&old_request_id);
                            fetch_requests.remove(&old_request_id);
                        }
                    },
                    RequestMigrationEvent::SessionEvent { url, kind, event } => match event {
                        SessionEvent::PublishDoneReceived {
                            request_id,
                            status_code,
                            stream_count,
                            ..
                        } => {
                            // 移行済みの購読の終端である。main ループの PUBLISH_DONE と同じ
                            // 分類でログに残し、恒久終了だけ状態へ反映する。side session の
                            // 購読の cleanup は main ループが所有する `MoqtClient` では観測
                            // できないため、drain 完了は待たない
                            let disposition = publish_done_disposition(
                                normalize_publish_done_code(status_code),
                            );
                            tracing::info!(
                                "Received PUBLISH_DONE on the request migration side session: \
                                 url={url}, kind={kind:?}, request_id={request_id}, \
                                 status_code={status_code}, \
                                 normalized_status_code={}, \
                                 stream_count={stream_count}, \
                                 disposition={disposition:?}",
                                normalize_publish_done_code(status_code),
                            );
                            match (kind, disposition) {
                                (
                                    Some(kind),
                                    PublishDoneDisposition::TrackEnded
                                    | PublishDoneDisposition::Error,
                                ) => {
                                    permanently_ended_tracks.insert(kind);
                                    // 全 media track が終了したら pipeline を終える
                                    // (main ループの tick の終了判定と同じ規則)
                                    if matches!(kind, TrackKind::Video | TrackKind::Audio)
                                        && all_media_tracks_ended(
                                            subscribe_video,
                                            subscribe_audio,
                                            &permanently_ended_tracks,
                                            &migration_pending_tracks,
                                        )
                                    {
                                        tracing::info!(
                                            "All media tracks ended permanently: ending the pipeline"
                                        );
                                        peer_ended = true;
                                        break 'main;
                                    }
                                }
                                (Some(kind), PublishDoneDisposition::GoingAway) => {
                                    // request stream の GOAWAY は受けるが、side session の
                                    // 購読をさらに移行する経路は持たない
                                    tracing::info!(
                                        "The migrated {kind:?} track is going away on the side session: url={url}"
                                    );
                                }
                                (None, _) => {
                                    tracing::warn!(
                                        "PUBLISH_DONE on the side session does not belong to a migrated subscription: url={url}, request_id={request_id}"
                                    );
                                }
                            }
                        }
                        SessionEvent::RequestErrorReceived {
                            request_id,
                            error_code,
                            retry_interval,
                            reason,
                            ..
                        } => {
                            tracing::warn!(
                                "Received REQUEST_ERROR on the request migration side session: \
                                 url={url}, kind={kind:?}, request_id={request_id}, \
                                 error_code={error_code:#x}, \
                                 retry_interval={retry_interval}, reason={}",
                                reason.as_str(),
                            );
                        }
                        SessionEvent::GoawayReceived {
                            new_session_uri,
                            timeout,
                            on_request_stream,
                        } => {
                            tracing::warn!(
                                "Received GOAWAY on the request migration side session: \
                                 url={url}, timeout={timeout}, \
                                 on_request_stream={on_request_stream:?}, \
                                 new_session_uri_len={}",
                                new_session_uri.len(),
                            );
                        }
                        SessionEvent::CloseSession(err) => {
                            tracing::warn!(
                                "The request migration side session closed: url={url}, \
                                 {:#x} {}",
                                err.code,
                                err.reason,
                            );
                        }
                        other => {
                            tracing::debug!(
                                "Ignoring a session event on the request migration side session: \
                                 url={url}, event={other:?}"
                            );
                        }
                    },
                    RequestMigrationEvent::Ended {
                        url,
                        generation,
                        reason,
                    } => {
                        // 同じ URL で作り直した side session の slot を消さないよう、
                        // 世代が一致する場合だけ slot を捨てる
                        let same_session = request_migration_sessions
                            .get(&url)
                            .is_some_and(|slot| slot.generation == generation);
                        if same_session {
                            request_migration_sessions.remove(&url);
                        }
                        // 移行済みの track は配送されなくなるが、他の track の受信は続ける
                        tracing::warn!(
                            "The request migration side session ended: url={url}, \
                             generation={generation}, same_session={same_session}: {reason}"
                        );
                    }
                }
            }
            changed = async {
                // WebTransport の drain (WT_DRAIN_SESSION / HTTP/3 GOAWAY) を観測する
                // (draft-ietf-webtrans-http3-16 §4.7 / draft-ietf-webtrans-http2-16 §6.13)。
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
                                        plan: {
                                            let (plan, resolve_as_ended) = plan_for_migration(
                                                &resubscribe_plan,
                                                &permanently_ended_tracks,
                                                &publish_done_pending,
                                            );
                                            for (request_id, kind) in resolve_as_ended {
                                                // 移行開始時点で恒久終了扱いに切り替える
                                                // (cleanup 完了を待たずに plan から外す)
                                                publish_done_pending.remove(&request_id);
                                                permanently_ended_tracks.insert(kind);
                                            }
                                            plan
                                        },
                                        last_locations,
                                        deadline,
                                    };
                                    migration_boundary_snapshot = Some(last_locations);
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
                // 映像の購読境界以下として落とした Object の累計も、増えていれば出す
                let video_before_boundary = video_delivery_order
                    .lock()
                    .expect("video delivery order lock is not poisoned")
                    .before_boundary_objects();
                log_video_boundary_counter(
                    &mut logged_video_boundary_objects,
                    video_before_boundary,
                );

                // PUBLISH_DONE で Terminated になった購読の cleanup 状態を 100 ms tick で
                // 観測する。`Some(true)` (drain timer 満了または Stream Count 超過かつ open
                // stream 0) または `None` (Session が回収済み) になった時点で track を終了させる
                let mut ready_to_end: Vec<(u64, TrackKind, PublishDoneDisposition)> = Vec::new();
                for (request_id, (kind, disposition)) in publish_done_pending.iter() {
                    match client.subscription_cleanup_ready(*request_id) {
                        Some(true) | None => {
                            ready_to_end.push((*request_id, *kind, *disposition));
                        }
                        Some(false) => {
                            // drain 中。late-arriving object を受信経路で処理して待つ
                        }
                    }
                }
                for (request_id, kind, disposition) in ready_to_end {
                    publish_done_pending.remove(&request_id);
                    let alias = match kind {
                        TrackKind::Catalog => catalog_alias,
                        TrackKind::Video => video_alias,
                        TrackKind::Audio => audio_alias,
                        TrackKind::Discard => {
                            ignored_publish_requests.get(&request_id).copied()
                        }
                    };
                    // track_map と datagram 受信タスクの alias 表の両方から該当 alias を
                    // 除去する。catalog は fill fetch stream が Request ID で振り分けられる
                    // ため `fetch_targets` からも除去する
                    {
                        let mut map = track_map
                            .write()
                            .expect("track_map lock must not be poisoned");
                        if let Some(alias) = alias {
                            map.remove(&alias);
                        }
                        let map_for_datagram = map.clone();
                        let _ = datagram_session_tx.send((
                            client.handle(),
                            data_plane.clone(),
                            Arc::new(map_for_datagram),
                            session_generation,
                        ));
                    }
                    match kind {
                        TrackKind::Catalog => {
                            // fill fetch stream は Request ID で振り分けられるため、
                            // 対応表から外す
                            lock_fetch_targets(&fetch_targets).remove(&request_id);
                            // catalog も恒久終了の状態機械に乗せる (`TrackEnded` / `Error`
                            // は恒久終了として記録し、移行先で購読し直さない。最後の
                            // `catalog_state` を使う)。`GoingAway` は移行待ちとして
                            // `migration_pending_tracks` に登録する (後着の request stream
                            // GOAWAY で tick で解消済みの Terminated 済み購読へ STOP_SENDING
                            // を送らないための記録。再発行成功時と session 移行成功時に集合から
                            // 外れる。全 media 終了の判定は video / audio のみを見るため
                            // catalog の登録は media の終了判定に影響しない)
                            match disposition {
                                PublishDoneDisposition::TrackEnded
                                | PublishDoneDisposition::Error => {
                                    permanently_ended_tracks.insert(kind);
                                }
                                PublishDoneDisposition::GoingAway => {
                                    migration_pending_tracks.insert(kind);
                                }
                            }
                            tracing::info!(
                                "Catalog subscription ended by PUBLISH_DONE (disposition={:?}, request_id={request_id})",
                                disposition,
                            );
                        }
                        TrackKind::Video | TrackKind::Audio => match disposition {
                            PublishDoneDisposition::TrackEnded
                            | PublishDoneDisposition::Error => {
                                permanently_ended_tracks.insert(kind);
                                tracing::info!(
                                    "Track ended permanently by PUBLISH_DONE: kind={kind:?}, \
                                     request_id={request_id}"
                                );
                            }
                            PublishDoneDisposition::GoingAway => {
                                migration_pending_tracks.insert(kind);
                                tracing::info!(
                                    "Track pending migration by PUBLISH_DONE: kind={kind:?}, \
                                     request_id={request_id}"
                                );
                            }
                        },
                        TrackKind::Discard => {
                            // 映像・音声の終了判定には使わない。後続の PUBLISH_DONE を
                            // 別の購読として扱わないよう、対応表から外す
                            ignored_publish_requests.remove(&request_id);
                            tracing::info!(
                                "PUBLISH-originated subscription ended: \
                                 request_id={request_id}, disposition={disposition:?}"
                            );
                        }
                    }
                    // 全 media track が恒久的に終了し、移行待ちも無ければ pipeline を終える
                    if matches!(kind, TrackKind::Video | TrackKind::Audio)
                        && all_media_tracks_ended(
                            subscribe_video,
                            subscribe_audio,
                            &permanently_ended_tracks,
                            &migration_pending_tracks,
                        )
                    {
                        tracing::info!(
                            "All media tracks ended permanently: ending the pipeline"
                        );
                        peer_ended = true;
                        break 'main;
                    }
                }
                // SUBSCRIBE_TRACKS 由来の購読は PUBLISH_DONE だけでなく、publisher による
                // request stream の reset でも終端しうる。PUBLISH_DONE の drain 待ちに載って
                // いるものは上のループが扱うため、それ以外で Session の回収が済んだ購読の
                // alias を track_map から外す
                let ended_discard: Vec<u64> = ignored_publish_requests
                    .iter()
                    .filter(|(request_id, _)| !publish_done_pending.contains_key(request_id))
                    .filter(|(request_id, _)| {
                        matches!(
                            client.subscription_cleanup_ready(**request_id),
                            Some(true) | None
                        )
                    })
                    .map(|(request_id, _)| *request_id)
                    .collect();
                if !ended_discard.is_empty() {
                    let datagram_map = {
                        let mut map = track_map
                            .write()
                            .expect("track_map lock must not be poisoned");
                        for request_id in &ended_discard {
                            if let Some(alias) = ignored_publish_requests.remove(request_id) {
                                map.remove(&alias);
                            }
                        }
                        Arc::new(map.clone())
                    };
                    let _ = datagram_session_tx.send((
                        client.handle(),
                        data_plane.clone(),
                        datagram_map,
                        session_generation,
                    ));
                    tracing::info!(
                        "Removed the ended PUBLISH-originated subscriptions: request_ids={ended_discard:?}"
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

    // 進行中の移行タスクを打ち切る。確立済みの移行先 session は `MoqtClient` の drop で
    // transport が閉じる。ここでの打ち切りは異常ではない (run 自体の終了が先に決まった)
    if let Some(handle) = migration_task.take() {
        handle.abort();
        let _ = handle.await;
        tracing::debug!("Aborted the in-flight migration task");
    }
    // request 単位移行の side session もすべて打ち切る。各 side session の `MoqtClient` は
    // タスクの終了で drop され、transport が閉じる
    let side_session_count = request_migration_tasks.len();
    for handle in request_migration_tasks.drain(..) {
        handle.abort();
        let _ = handle.await;
    }
    request_migration_sessions.clear();
    if side_session_count > 0 {
        tracing::debug!(
            "Aborted the in-flight request migration side sessions: count={side_session_count}"
        );
    }
    // 旧 session の終了タスクは best-effort である。run の終了を待たせないよう打ち切る
    // (旧 session の transport は `MoqtClient` の drop で閉じる)
    for handle in old_session_closers.drain(..) {
        handle.abort();
    }

    // select! で別の終了要因 (accept の終了 / GOAWAY / PUBLISH_DONE / peer の close /
    // shutdown など) が先に成立していても、既に届いている終了依頼があればそのコードで閉じる
    // (draft-ietf-moq-transport-23 §8.3 の MUST を終了コード 0x0 で上書きしない)。
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
    // STOP_SENDING は state machine に拒否される (draft-ietf-moq-transport-23 §3.1
    // (Subscriptions) は Pending / Established のみ STOP_SENDING を認める)。
    // stream task の join より先に送るのは、受信待ちの live stream で join が終わらない
    // ことを防ぐためである。
    // transport 自体のエラーで抜けた場合も接続が生存している可能性があるため、
    // 接続死活の判定と独立に best-effort で送る。
    // GOAWAY 起因の停止に GOING_AWAY (§12.5) ではなく CANCELLED を使うのは、
    // tokio-moq の stop_sending API がコードを指定できないためである。
    if should_stop_sending(peer_ended, session_terminated) {
        // 対象は「まだ active な購読」だけである ([`stop_sending_targets`] を参照)。PUBLISH
        // DONE で Terminated になった購読と Session が回収済みの購読は拒否されるため、
        // 0100 型の警告を出さない
        for (kind, rid) in stop_sending_targets(
            video_request_id,
            audio_request_id,
            catalog_request_id,
            &active_requests,
        ) {
            if let Err(e) = client.stop_sending(rid).await {
                // `should_stop_sending` のガードにより、ここでは peer_ended / session_terminated は
                // どちらも偽である (peer 起点の終了では送らず、その他の失敗だけがここに来る)
                log_failure(
                    format_args!("Failed to send STOP_SENDING for {kind:?}: {e}"),
                    is_connection_closed(&e),
                );
            }
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
/// Session が STOP_SENDING を指示した場合 (open な subgroup stream の DATA_STREAM_TIMEOUT、
/// subscriber 役 subscription への REQUEST_ERROR 送信 (§9.8 (PUBLISH))、購読状態の削除
/// (§9.9 (PUBLISH_DONE))) は、受信半に STOP_SENDING を送出して読み取りを打ち切る
/// (`SessionEvent::StopSendingDataStream` の doc と
/// draft-ietf-moq-transport-23 §12.2 (Session Termination Codes) を参照)。
/// セッションは閉じないため、購読と他の stream の受信は継続する。
/// 登録済み data stream の受信結果
enum RegisteredStreamData {
    /// データチャンク
    Data(Bytes),
    /// FIN / RESET_STREAM による終端
    End(RequestStreamEnd),
    /// Session が STOP_SENDING を指示した (timeout / 購読削除など)
    StoppedBySession,
}

/// 登録済み data stream から次のデータまたは終端種別を受け取る
///
/// 終端種別を呼び出し側へ返すのは、FIN の途中終了だけを mid-object FIN として報告し、
/// RESET_STREAM (§11.3.2 の正常経路) と STOP_SENDING 指示を区別するためである。
async fn receive_registered_stream_data_outcome(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
) -> Result<RegisteredStreamData> {
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
                    "Failed to send STOP_SENDING for data stream: \
                     stream_id={}, error_code={error_code:#x} {e}",
                    stream_id.0
                );
            }
            Ok(RegisteredStreamData::StoppedBySession)
        }
        chunk = stream.receive_chunk() => match chunk? {
            transport::RecvChunk::Data(data) => Ok(RegisteredStreamData::Data(data)),
            transport::RecvChunk::End(end) => {
                data_plane.recv_data_stream_closed(stream_id, end)?;
                Ok(RegisteredStreamData::End(end))
            }
        },
    }
}

/// 登録済み data stream から次のデータを受け取る
///
/// 終端種別を必要としない呼び出し元 (header 待ちループなど) 用の従来 API。
async fn receive_registered_stream_data(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
) -> Result<Option<Bytes>> {
    match receive_registered_stream_data_outcome(stream, data_plane, stream_id).await? {
        RegisteredStreamData::Data(data) => Ok(Some(data)),
        RegisteredStreamData::End(_) | RegisteredStreamData::StoppedBySession => Ok(None),
    }
}

/// 保持経路専用の読み出し。`End` を観測しても Session への会計通知を遅らせる
///
/// [`receive_registered_stream_data_outcome`] と同じ select 構造で、`RecvChunk::End` を観測して
/// も `recv_data_stream_closed` を呼ばない。購読の確立前に FIN まで届いた stream は Session 会
/// 計に残ったまま保持され、保持の解除後に decode ループが `End(Fin)` を観測して初めて
/// `recv_data_stream_closed` を 1 度だけ通る。保持の破棄 (abort 経路) では、Session への会計
/// 通知は `abort_subgroup_stream` の `send_data_stream_stop_sending` が行い、
/// `recv_data_stream_closed` は呼ばない。`draft-ietf-moq-transport-23` §3.1.3.1 は未知 alias
/// の stream を "buffer it briefly" することを許す。
async fn receive_pending_hold_stream_outcome(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
) -> Result<RegisteredStreamData> {
    tokio::select! {
        biased;
        error_code = data_plane.wait_data_stream_stop_sending(stream_id) => {
            // Session 側は破棄対象として会計を閉じている。読み出しを打ち切り、
            // 保持経路の破棄 (abort 経路) に任せる。送出の前に指示の登録を破棄し、
            // 同じ stream へ STOP_SENDING を二重に送らない
            data_plane.clear_data_stream_stop_sending(stream_id);
            if let Err(e) = stream.stop_sending(error_code).await {
                tracing::warn!(
                    "Failed to send STOP_SENDING for data stream: \
                     stream_id={}, error_code={error_code:#x} {e}",
                    stream_id.0
                );
            }
            Ok(RegisteredStreamData::StoppedBySession)
        }
        chunk = stream.receive_chunk() => match chunk? {
            transport::RecvChunk::Data(data) => Ok(RegisteredStreamData::Data(data)),
            // draft-ietf-moq-transport-23 §3.1.3.1 の保持経路では、FIN の会計通知
            // (`recv_data_stream_closed`) の時点を保持の解除後に移すため、ここでは呼ばない。
            // 終端種別は呼び出し側で分類する
            transport::RecvChunk::End(end) => Ok(RegisteredStreamData::End(end)),
        },
    }
}

/// [`abort_subgroup_stream`] の読み捨て。Session への会計通知を遅らせる読み出しを使う
///
/// 保持した stream を破棄する経路から呼ばれる。`recv_data_stream_closed` を呼ばないため、
/// FIN / RESET / STOP_SENDING 指示のいずれの終端でも会計は 2 度通知されない
/// (保持が FIN を session に未通知のまま abort した場合、会計は
/// `send_data_stream_stop_sending` で閉じられる)。
async fn drain_registered_stream_quietly(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
) -> Result<()> {
    loop {
        match receive_pending_hold_stream_outcome(stream, data_plane, stream_id).await? {
            RegisteredStreamData::Data(_) => continue,
            RegisteredStreamData::End(_) | RegisteredStreamData::StoppedBySession => {
                return Ok(());
            }
        }
    }
}

/// `PendingNotifyReason::Subscriber` で起きた task の自分の `recv_subgroup_header` 再試行結果
///
/// 検出元 (自分のループ再試行で `Accepted` を観測した task) は再試行を行わないため `None`
/// になる。Session は受理済みの stream への 2 度目の SUBGROUP_HEADER
/// (`draft-ietf-moq-transport-23` §12.2 の `UNEXPECTED_MESSAGE` (0x303)) を拒否するため、
/// 再試行は検出先だけが行う。
enum PendingSubscriberRetry {
    /// 自分の再試行が `Accepted`
    Accepted,
    /// 自分の再試行が `UnknownTrackAlias`
    UnknownTrackAlias,
    /// 自分の再試行が `Discarded`
    Discarded,
    /// 自分の再試行が `FilteredOut`
    FilteredOut,
    /// 自分の再試行がエラーで終わった (エラーメッセージを含む)
    Error(String),
}

/// `Subscriber` で引き取った entry の処理の分類
///
/// 検出元は受理済みのため復帰する。検出先は自分の header の受理を確認してから復帰する
/// (`draft-ietf-moq-transport-23` §3.1 (Subscriptions) の Object 配信前の header 受理前提)。
enum PendingSubscriberAction {
    /// 保持したチャンクを decoder へ戻して処理を続ける
    Resume,
    /// task ローカルで保留を続ける (entry は引き取り済み・削除済みのためバッファに戻せない)
    HoldLocal,
    /// 会計を閉じて破棄する (`abort_subgroup_stream` を呼ぶ)
    AbortDiscard,
    /// 会計を閉じずに破棄する (ループ再試行の `Err` 分岐と同じ扱い)
    DiscardQuietly,
}

/// 検出元フラグと自分の再試行結果から `Subscriber` 引き取りの処理分類を返す
///
/// 検出元 (検出元フラグが true) は、既に自分で `Accepted` を観測して `note_subscriber` を呼んだ
/// task であるため、entry を引き取った時点で自分の stream は session の受理済み状態であり、
/// 常に復帰する。検出先は引取ったあとに自分の再試行結果で処理を続けるか破棄するかを決める。
/// `None` は検出先で再試行が行われなかった場合 (呼び出し誤り) を表すが、安全性のため
/// 検出元と同じ復帰ではなく破棄にする。
fn pending_subscriber_action(
    detected_self: bool,
    retry: Option<&PendingSubscriberRetry>,
) -> PendingSubscriberAction {
    if detected_self {
        return PendingSubscriberAction::Resume;
    }
    match retry {
        Some(PendingSubscriberRetry::Accepted) => PendingSubscriberAction::Resume,
        Some(PendingSubscriberRetry::UnknownTrackAlias) => PendingSubscriberAction::HoldLocal,
        Some(PendingSubscriberRetry::Discarded | PendingSubscriberRetry::FilteredOut) => {
            PendingSubscriberAction::AbortDiscard
        }
        Some(PendingSubscriberRetry::Error(_)) | None => PendingSubscriberAction::DiscardQuietly,
    }
}

/// 保持ループでの終端観測の処理分類
///
/// FIN は保持を継続する (`draft-ietf-moq-transport-23` §3.1.3.1 の保持の目的を FIN 済みの
/// stream でも果たすため)。Session への会計通知は保持の解除後の decode ループで 1 度だけ
/// 行う。RESET_STREAM と Session の STOP_SENDING 指示による終端は、stream の内容が
/// 欠けている可能性があるため既存どおり破棄する (`draft-ietf-moq-transport-23` §11.3.2
/// (Closing Subgroup Streams) は FIN 受信後の RESET の無視を許すが、保持時の判断は
/// 保守的に破棄とする)。
enum PendingHoldEnd {
    /// FIN を観測した。保持を継続し、Session への会計通知は解除後に遅らせる
    KeepHoldingWithFin,
    /// RESET_STREAM / Session の STOP_SENDING 指示。既存の終端分岐どおり abort する
    AbortDiscard,
}

/// 終端種別 (または Session の STOP_SENDING 指示) から保持ループの処理を分類する
///
/// `end` が `None` の場合は Session の STOP_SENDING 指示 (`StoppedBySession`) を表す。
fn pending_hold_end_decision(end: Option<&RequestStreamEnd>) -> PendingHoldEnd {
    match end {
        Some(RequestStreamEnd::Fin) => PendingHoldEnd::KeepHoldingWithFin,
        None | Some(RequestStreamEnd::Reset { .. }) => PendingHoldEnd::AbortDiscard,
    }
}

/// task ローカルで保持するチャンクと期限
///
/// entry は intake の直後に削除されるため、チャンクと期限の管理を task に移す。
/// `already_fin` は intake 時点で FIN を観測済みであることを伝える (保持の解除後に
/// decode ループが session への会計通知を行うため、FIN は遅延する)。
struct LocalPendingHold {
    chunks: Vec<Vec<u8>>,
    deadline_us: i64,
    already_fin: bool,
}

/// FIN 済みの保留と task ローカル保持の周期待ち間隔 (マイクロ秒)
///
/// 保持ループ (`register_subgroup_header`) と task ローカル保持
/// (`continue_pending_subscriber_locally`) の FIN 済み待機の両方で使う。FIN 済みの stream
/// は読み出しをしないため、購読の確立 (`recv_subgroup_header` 再試行) と `note_subscriber`
/// 通知を定周期で観測する。読み出しを停止し、poll 間隔と deadline 判定で待つため busy loop
/// を作らない。
const LOCAL_PENDING_HOLD_POLL_INTERVAL_US: i64 = 10_000;

/// `Subscriber` で起きた task が `UnknownTrackAlias` だった保留を task ローカルで続ける
///
/// entry は intake の直後に削除済みのため、バッファ側からは通知が再び届かない
/// (`take_ready_for` は `None`、`push` は no-op、`expire` による `Timeout` は発火しない)。
/// そのためチャンクと期限を task ローカルで持ち替え、自分の `recv_subgroup_header` の再試行で
/// 確立を待つ。期限 (`deadline_us`) は intake 時点の値を使い、entry の再追加による期限延長は
/// 行わない。
///
/// 継続中は stream を読み続け、チャンクをバッファに積まないで task 内で保持する。このため
/// チャンクは per-session 上限の集計から外れ、per-stream 上限のみ task 側で適用する
/// (超えたときは既存の overflow 分岐と同じ警告を出して破棄する)。読み出しは session の
/// 会計通知を遅らせる `receive_pending_hold_stream_outcome` を使う。読み出しが終端
/// (`End` / `StoppedBySession` 指示) を観測したら、終端種別を task 内で記録して読み出しを
/// 止める。RESET_STREAM と Session の STOP_SENDING 指示による終端は、ストリームの内容が
/// 欠けている可能性があるため既存の終端分岐と同じく破棄する (abort 経路の
/// `send_data_stream_stop_sending` で会計を閉じるため、unknown stream id の警告は出ない)。
/// FIN は保持を継続する (`read_ended_in_fin`)。
/// FIN を観測した保持は、読み出しを止めて自分の再試行で確立を待ち、確立したら保持チャンクを
/// decoder へ戻す (Session への会計通知は decode ループが 1 度だけ行う)。入力時に既に FIN を
/// 観測していた保持を出し入れするため、`LocalPendingHold::already_fin` を引く。期限切れなら
/// 期限分岐と同じく破棄する (会計は `send_data_stream_stop_sending` で閉じる)。
///
/// 待ち順序は「期限付きの読み出し、そのあとで自分の再試行」である (既存の保留ループとは
/// 逆)。入口の直前に検出先の再試行で `UnknownTrackAlias` を観測済みのため、直後の重複
/// 呼び出しを避ける。保持中にこの stream でチャンクが届かない場合、復帰と破棄の判断は
/// 最初の読み出しが終わったあとに行われるため、確立済みでも残り期限のぶんだけ遅くなり得る。
async fn continue_pending_subscriber_locally(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    header: &SubgroupHeader,
    decoder: &mut SubgroupStreamDecoder,
    stream_num: u64,
    hold: LocalPendingHold,
) -> bool {
    let LocalPendingHold {
        chunks: mut held,
        deadline_us,
        already_fin,
    } = hold;
    let options = DEFAULT_PENDING_SUBGROUP_BUFFER_OPTIONS;
    let mut held_bytes: usize = held.iter().map(Vec::len).sum();
    // FIN を観測して読み出しを止めた状態。終端種別の task 内での記録である
    // (`draft-ietf-moq-transport-23` §3.1.3.1 は保持で順序入れ替わりを処理する MAY を与える)。
    // FIN 済みの保持は読み出しを止めて自分の再試行で確立を待つ。確立したら保持チャンクを
    // decoder へ戻し、decode ループが FIN の session 会計通知 (recv_data_stream_closed) を
    // 1 度だけ行う。期限までは保持を続け、期限切れは Deadline 分岐と同じく破棄する
    let mut read_ended_in_fin = already_fin;
    loop {
        let remaining_us = deadline_us.saturating_sub(monotonic_now_us());
        if remaining_us <= 0 {
            // 期限までに自分の header が受理されなかった。既存の Timeout 分岐と同じく破棄する
            tracing::warn!(
                "Stream #{stream_num}: dropping the locally held subgroup stream \
                 (track alias={} was not established within {} ms)",
                header.track_alias,
                options.timeout_us / 1000,
            );
            abort_subgroup_stream(stream, data_plane, stream_id).await;
            return false;
        }

        if !read_ended_in_fin {
            match tokio::time::timeout(
                std::time::Duration::from_micros(remaining_us.saturating_sub(1) as u64),
                receive_pending_hold_stream_outcome(stream, data_plane, stream_id),
            )
            .await
            {
                Ok(Ok(RegisteredStreamData::Data(data))) => {
                    if held_bytes.saturating_add(data.len()) > options.per_stream_max_bytes {
                        tracing::warn!(
                            "Stream #{stream_num}: dropping the locally held subgroup stream \
                             (per-stream buffer overflow: track alias={}, limit={} bytes)",
                            header.track_alias,
                            options.per_stream_max_bytes,
                        );
                        abort_subgroup_stream(stream, data_plane, stream_id).await;
                        return false;
                    }
                    held_bytes += data.len();
                    held.push(data.to_vec());
                }
                // 終端。この読み出しは session への会計通知を遅らせるため、FIN はここでは
                // 通知せず、終端種別を記録して以後の読み出しを止める (保持の解除後に
                // decode ループが `End(Fin)` を観測して初めて session 会計を閉じる)
                Ok(Ok(RegisteredStreamData::End(RequestStreamEnd::Fin))) => {
                    tracing::debug!(
                        "Stream #{stream_num}: stream ended with FIN while holding the \
                         subgroup stream (track alias={})",
                        header.track_alias,
                    );
                    read_ended_in_fin = true;
                }
                // RESET_STREAM。session への会計通知はここでは行わない (stream は Session 会
                // 計に残っているため、`abort_subgroup_stream` の
                // `send_data_stream_stop_sending` で会計を閉じ、unknown stream id の警告は
                // 出ない)
                Ok(Ok(RegisteredStreamData::End(RequestStreamEnd::Reset { .. }))) => {
                    tracing::debug!(
                        "Stream #{stream_num}: stream reset while holding the subgroup stream \
                         (track alias={})",
                        header.track_alias,
                    );
                    abort_subgroup_stream(stream, data_plane, stream_id).await;
                    return false;
                }
                Ok(Ok(RegisteredStreamData::StoppedBySession)) => {
                    tracing::debug!(
                        "Stream #{stream_num}: subgroup stream stopped by session instruction \
                         while holding (track alias={})",
                        header.track_alias,
                    );
                    abort_subgroup_stream(stream, data_plane, stream_id).await;
                    return false;
                }
                Ok(Err(e)) => {
                    log_failure(
                        format_args!(
                            "Stream #{stream_num}: failed to read held subgroup stream: {e}"
                        ),
                        is_transport_session_end(&e),
                    );
                    return false;
                }
                // 期限切れは loop 先頭の remaining 判定で処理する
                Err(_) => {}
            }
        }

        // FIN 済み (read_ended_in_fin) のときは読み出しをしない。終端済み stream は FIN を
        // 即時に再配信するため読み出しを loop に入れると busy loop になる。再試行の周期
        // 待ち (poll 間隔) と STOP_SENDING 指示の監視を行い、期限は loop 先頭の deadline 判定
        // で破棄へ収束させる
        if read_ended_in_fin {
            tokio::select! {
                biased;
                stop_code = data_plane.wait_data_stream_stop_sending(stream_id) => {
                    // STOP_SENDING 指示。session が指示で会計を閉じているため、既存の
                    // 終端分岐と同じく破棄する (既存の読み出しヘルパと同じく指示の登録を
                    // 破棄して stream を止める)
                    data_plane.clear_data_stream_stop_sending(stream_id);
                    if let Err(e) = stream.stop_sending(stop_code).await {
                        tracing::warn!(
                            "Failed to send STOP_SENDING for data stream: \
                             stream_id={}, error_code={stop_code:#x} {e}",
                            stream_id.0
                        );
                    }
                    abort_subgroup_stream(stream, data_plane, stream_id).await;
                    return false;
                }
                _ = tokio::time::sleep(std::time::Duration::from_micros(
                    remaining_us
                        .min(LOCAL_PENDING_HOLD_POLL_INTERVAL_US)
                        .saturating_sub(1)
                        as u64,
                )) => {}
            }
        }

        // 自分の再試行で確立を確認する。確立していれば保持したチャンクを decoder へ戻す
        // (FIN 保持中に `UnknownTrackAlias` が続く限り、この再試行が唯一の確立検出手段である)
        match data_plane.recv_subgroup_header(stream_id, header) {
            Ok(TrackDataAcceptance::Accepted) => {
                for chunk in &held {
                    decoder.push(chunk);
                }
                tracing::debug!(
                    "Stream #{stream_num}: released the locally held subgroup stream \
                     (track alias={}, buffered={} bytes)",
                    header.track_alias,
                    held_bytes,
                );
                return true;
            }
            // 保留を続ける。entry は削除済みのため、次の loop の読み出しと再試行で待つ
            Ok(TrackDataAcceptance::UnknownTrackAlias) => {}
            // draft-ietf-moq-transport-23 §3.1.3 (Track Alias): キャンセル済み subscription の
            // alias に届いた不要 Object。session を閉じずに破棄する (既存の再試行分岐と同じ)
            Ok(TrackDataAcceptance::Discarded) => {
                tracing::debug!(
                    "Stream #{stream_num}: discarding objects for cancelled track alias={}",
                    header.track_alias,
                );
                abort_subgroup_stream(stream, data_plane, stream_id).await;
                return false;
            }
            // draft-ietf-moq-transport-23 §3.1 (Subscriptions): フィルタ再適用の結果どの
            // subscription にも属さなかった Object。session を閉じずに捨てる (既存の再試行
            // 分岐と同じ)
            Ok(TrackDataAcceptance::FilteredOut) => {
                tracing::debug!(
                    "Stream #{stream_num}: object filtered out for track alias={}",
                    header.track_alias,
                );
                abort_subgroup_stream(stream, data_plane, stream_id).await;
                return false;
            }
            // 既存の再試行分岐の `Err` と同じく会計を閉じずに破棄する
            Err(e) => {
                tracing::warn!("Stream #{stream_num}: failed to register subgroup header: {e}");
                return false;
            }
        }
    }
}

/// 途中終了した FIN を `Session::report_mid_object_fin` へ報告すべきか
///
/// draft-ietf-moq-transport-23 §11.3 (Subgroup Streams): FIN の途中終了は
/// TRUNCATED_OBJECT でセッションを閉じる。
/// RESET_STREAM は §11.3.2 (Closing Subgroup Streams) の正常経路であり、header 未受信の
/// `finish()` も §11.3 の「シリアライズ途中の Object」ではないため報告しない。
fn mid_object_fin_should_report(
    end: &RequestStreamEnd,
    finish: &core::result::Result<(), MessageError>,
) -> bool {
    matches!(end, RequestStreamEnd::Fin) && matches!(finish, Err(MessageError::UnexpectedEof))
}

/// decoder の `finish()` 結果を見て、必要なときだけ mid-object FIN を報告する
fn report_mid_object_fin_if_needed(
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    end: &RequestStreamEnd,
    finish: &core::result::Result<(), MessageError>,
) {
    if !mid_object_fin_should_report(end, finish) {
        return;
    }
    if let Err(e) = data_plane.report_mid_object_fin(stream_id) {
        tracing::warn!(
            "Failed to report mid-object FIN: stream_id={}, {e}",
            stream_id.0
        );
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
/// draft-ietf-moq-transport-23 §3.5 (Joining an Ongoing Track) の購読パターンに従い、
/// 購読の Location Filter を Next Object (§9.19.9 Table 6 の Type 0x05) にしつつ、
/// FILL_PARAMETERS (§9.19.15) で現在 Group の先頭から埋める fill を要求する
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
/// (draft-ietf-moq-transport-23 §3.4 (Fill Semantics) / §3.5 (Joining an Ongoing Track))。
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
    // publisher が書く表現と同じ draft-ietf-moq-transport-23 §8.8 の表現へ直す
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
                // draft-ietf-moq-transport-23 §11.5.1 (Padding Streams)
                let _ = drain_registered_stream_to_end(&mut stream, data_plane, stream_id).await;
            }
            StreamType::Fetch => {
                // fill fetch stream は FETCH_HEADER に起因 SUBSCRIBE の Request ID を載せる
                // (draft-ietf-moq-transport-23 §3.4)。購読で届くカタログと同じ状態へ適用する
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

/// 破棄する fetch stream の会計を解放し、peer に STOP_SENDING を送出する
///
/// 保留の期限・上限の超過では [`DataPlaneHandle`] が Session の会計を解放済みであるため、
/// `recv_data_stream_closed` を送ると「unknown stream id」になる。ただし期限到達と PUBLISH の
/// 受理が競合した場合は受理済みの stream を取り逃す可能性があるため、`send_data_stream_stop_sending`
/// で会計を解放し直す (未登録ならエラーになるだけで副作用はない)。
/// peer へは「この stream は不要」を STOP_SENDING で伝える (draft-ietf-moq-transport-23 §12.5
/// (Stream Reset Error Codes) の CANCELLED)。
async fn stop_sending_discarded_fetch_stream(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
) {
    let _ = data_plane.send_data_stream_stop_sending(stream_id);
    if let Err(e) = stream.stop_sending(STREAM_CANCELLED).await {
        tracing::warn!(
            "Failed to send STOP_SENDING for a discarded fetch stream: stream_id={}, {e}",
            stream_id.0
        );
    }
}

/// FETCH_HEADER を Session に登録し、受理されるまで待つ
///
/// draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams): FILL_PARAMETERS
/// 付き PUBLISH を送った publisher は初回 fill の fill fetch stream を開く。PUBLISH と fill の
/// 到着順序は保証されないため、Session が未登録の Request ID を返した場合は
/// [`DataPlaneHandle::wait_fetch_header_registration`] で PUBLISH の受理 (または保留の期限・
/// 上限の超過) を待つ。保留の期限・上限を超えた場合は stream と Session の会計が
/// `DataPlaneHandle` 側で破棄されるため、本関数は `false` を返して読み取りを打ち切らせる。
///
/// 読み切り (`drain_registered_stream_to_end`) は呼び出し側の責務である。登録できなかった
/// stream は Session の会計が既に無いため、drain せずに STOP_SENDING で打ち切る。
async fn register_fetch_header(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    fetch_header: &FetchHeader,
) -> Result<bool> {
    match data_plane.recv_fetch_header(stream_id, fetch_header)? {
        FetchHeaderRegistration::Accepted => Ok(true),
        FetchHeaderRegistration::Pending(pending) => {
            match data_plane.wait_fetch_header_registration(pending).await {
                FetchHeaderResolution::Accepted => Ok(true),
                FetchHeaderResolution::Discarded => {
                    tracing::warn!(
                        "Discarded a fetch stream whose request was not registered before the \
                         hold expired: stream_id={}, request_id={}",
                        stream_id.0,
                        fetch_header.request_id,
                    );
                    stop_sending_discarded_fetch_stream(stream, data_plane, stream_id).await;
                    Ok(false)
                }
            }
        }
        FetchHeaderRegistration::Rejected => {
            tracing::warn!(
                "Discarded a fetch stream because too many fetch headers are awaiting their \
                 PUBLISH: stream_id={}, request_id={}",
                stream_id.0,
                fetch_header.request_id,
            );
            stop_sending_discarded_fetch_stream(stream, data_plane, stream_id).await;
            Ok(false)
        }
    }
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
    // 自側が発行した購読の fill であるため Request ID は既知だが、PUBLISH 起点の fill と
    // 同じ経路を通す (登録の成否を 1 か所に集約する)
    if !register_fetch_header(stream, data_plane, stream_id, &fetch_header).await? {
        return Err(Error::Other(format!(
            "fetch header was discarded before registration: request_id={}",
            fetch_header.request_id,
        )));
    }
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
            None => match receive_registered_stream_data_outcome(stream, data_plane, stream_id)
                .await?
            {
                RegisteredStreamData::Data(data) => {
                    decoder.push(&data);
                    continue;
                }
                RegisteredStreamData::End(end) => {
                    report_mid_object_fin_if_needed(data_plane, stream_id, &end, &decoder.finish());
                    break;
                }
                RegisteredStreamData::StoppedBySession => break,
            },
        };
        data_plane.recv_fetch_entry(stream_id, &entry)?;
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
        // Object 以外 (End of Range / End of Group / End of Track) は catalog の Object では
        // ないため捨てる。marker は直前の Object が Group / Track の終端であることを示すだけで
        // (draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields))、catalog の値そのものを
        // 持たない。
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
        match receive_registered_stream_data_outcome(stream, data_plane, stream_id).await? {
            RegisteredStreamData::Data(data) => decoder.push(&data),
            RegisteredStreamData::End(end) => {
                report_mid_object_fin_if_needed(data_plane, stream_id, &end, &decoder.finish());
                return Err(Error::Other(
                    "stream closed before catalog payload".to_string(),
                ));
            }
            RegisteredStreamData::StoppedBySession => {
                return Err(Error::Other(
                    "stream stopped before catalog payload".to_string(),
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
/// 宣言された namespace は draft-ietf-moq-transport-23 §8.8 (Representing Namespace and Track
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
                 ('-' separates namespace fields; use the draft-ietf-moq-transport-23 \
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
    fetch_targets: &SharedFetchTargets,
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
    audio_params: Option<(u32, u8)>,
    frame_tx: &std::sync::mpsc::Sender<DecodedVideoFrame>,
    audio_tx: &std::sync::mpsc::Sender<DecodedAudioFrame>,
    stream_num: u64,
    slot: &mut StreamSlot,
    display_backlog: std::sync::Arc<std::sync::atomic::AtomicI64>,
    video_decode_order: &std::sync::Arc<tokio::sync::Mutex<VideoDecodeOrder>>,
    video_delivery_order: &std::sync::Arc<std::sync::Mutex<VideoDeliveryOrder>>,
    group_switch: &GroupSwitchHandle,
    audio_decoder: Option<&std::sync::Arc<tokio::sync::Mutex<AudioDecoder>>>,
    audio_config_validated_generation: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    session_generation: u64,
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
        fetch_targets,
        video_codec,
        audio_codec,
        audio_params,
        frame_tx,
        audio_tx,
        stream_num,
        slot,
        display_backlog,
        video_decode_order,
        video_delivery_order,
        group_switch,
        audio_decoder,
        audio_config_validated_generation,
        session_generation,
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
    fetch_targets: &SharedFetchTargets,
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
    audio_params: Option<(u32, u8)>,
    frame_tx: &std::sync::mpsc::Sender<DecodedVideoFrame>,
    audio_tx: &std::sync::mpsc::Sender<DecodedAudioFrame>,
    stream_num: u64,
    slot: &mut StreamSlot,
    display_backlog: std::sync::Arc<std::sync::atomic::AtomicI64>,
    video_decode_order: &std::sync::Arc<tokio::sync::Mutex<VideoDecodeOrder>>,
    video_delivery_order: &std::sync::Arc<std::sync::Mutex<VideoDeliveryOrder>>,
    group_switch: &GroupSwitchHandle,
    audio_decoder: Option<&std::sync::Arc<tokio::sync::Mutex<AudioDecoder>>>,
    audio_config_validated_generation: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    session_generation: u64,
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
            // fill fetch stream (draft-ietf-moq-transport-23 §3.4 (Fill Semantics)) を配送経路へ
            // 流す。移行後に catalog を張り直すと fill で完全なカタログが届くため、これを
            // CatalogState へ適用する。どちらでもない stream は Session へ登録だけ行って
            // 読み捨てる (読み捨ての判定を登録より先に行うと、自側が発行していない
            // Request ID を載せた PUBLISH 起点の初回 fill を失う。§3.4.1 (Opening and Closing
            // Fill Fetch Streams))
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
                        request_session_termination_for_decode_error(termination_tx, &e);
                        tracing::warn!("Stream #{stream_num}: failed to decode fetch header: {e}");
                        let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
                        return;
                    }
                }
            };
            // Session への登録を配送先の判定より先に行う。fill fetch stream の Request ID は
            // 起因メッセージのものであり、PUBLISH 起点の初回 fill は fetch_targets に現れない
            // (fetch_targets は自側が発行した要求の Request ID だけを持つ)。登録を先に行うことで、
            // PUBLISH の受理待ちの保留・会計の解放を DataPlaneHandle に一任できる。
            // 保留待ちは PUBLISH の到着を最長 5 秒待つため、その間は stream の同時処理枠を
            // 手放す。枠を保持したまま待つと、保留が MAX_CONCURRENT_STREAMS 本そろったときに
            // 他の stream の処理が保留の期限まで止まる。
            slot.release();
            let registered =
                match register_fetch_header(stream, data_plane, stream_id, &fetch_header).await {
                    Ok(registered) => registered,
                    Err(e) => {
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
                };
            slot.reacquire().await;
            if !registered {
                // 保留の期限・上限の超過で破棄済みである。Session の会計も解放済み
                return;
            }
            // 配送先の判定は、FETCH ヘッダを読んだ時点の対応表で行う。request 単位移行の
            // side session では再発行のたびに表が増えるため、ロックを取って複製する
            // (ロックは await をまたがない)
            let fetch_targets = lock_fetch_targets(fetch_targets).clone();
            match fetch_stream_action(fetch_header.request_id, &fetch_targets) {
                // Session は受理したが、このアプリに配送先が無い FETCH 応答である。読み捨てる
                FetchStreamAction::Ignore => {
                    tracing::debug!(
                        "Stream #{stream_num}: dropping a fetch stream \
                         (request_id={} does not belong to this subscriber)",
                        fetch_header.request_id,
                    );
                    let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
                }
                action @ (FetchStreamAction::DeliverVideo | FetchStreamAction::DeliverCatalog) => {
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
                                video_delivery_order,
                                last_received_locations,
                                recorder,
                                termination_tx,
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
                                    if let Error::Moqt(error) = &e {
                                        request_session_termination_for_decode_error(
                                            termination_tx,
                                            error,
                                        );
                                    }
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
            // draft-ietf-moq-transport-23 §11.5.1 (Padding Streams):
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
                        request_session_termination_for_decode_error(termination_tx, &e);
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
                    // gate を通すのは再生する (decoder を持つ) stream だけである。録画のみの
                    // 場合は復号しないため、保留する理由が無い
                    let gate_enabled = decoder.is_some();
                    if gate_enabled {
                        group_switch.begin_stream(header.group_id, stream_id.0);
                    }
                    let frames = decode_video_stream(
                        stream,
                        data_plane,
                        stream_id,
                        header.group_id,
                        &mut sg_decoder,
                        video_decode_order,
                        video_delivery_order,
                        group_switch,
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
                    // decode_video_stream の return 経路 (FIN / RESET_STREAM / STOP_SENDING /
                    // 読み取りエラー / 配送の打ち切り) はすべてここで吸収し、begin と end を
                    // 1 対 1 にする
                    if gate_enabled {
                        group_switch.end_stream(header.group_id, stream_id.0);
                    }
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
                    // Audio Config の中身はコーデック依存 (LOC draft-ietf-moq-loc-05 §2.3.3.1 (Audio Config)) のため、
                    // catalog の codec が opus の場合のみ OpusHead として解釈する。AAC (mp4a.*) の
                    // Audio Config は AudioSpecificConfig であり、AudioToolbox のデコーダは
                    // 入力のサンプルレートとチャンネル数だけで構成できるため解釈しない。
                    // 検証済み世代は subscription で共有する: decode_audio_stream は
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
                            audio_config_validated_generation,
                            session_generation,
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
                            if let Error::Moqt(error) = &e {
                                request_session_termination_for_decode_error(termination_tx, error);
                            }
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
                Some(TrackKind::Discard) => {
                    // SUBSCRIBE_TRACKS の結果として受理した購読である。Session への header 登録と
                    // stream の会計は済んでいるため、Object を読み捨てて会計だけ閉じる
                    tracing::debug!(
                        "Stream #{stream_num}: discarding objects for a PUBLISH-originated subscription: alias={}",
                        header.track_alias,
                    );
                    let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
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
/// Track Alias が未確立 (`UnknownTrackAlias`) の場合は、draft-ietf-moq-transport-23
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
/// - `recv_subgroup_header` の再試行が `Accepted` を返す (Session が alias を解決できる)。
///   この task は自分で header を受理した (検出元)。`note_subscriber` で同じ alias の全
///   entry に通知したあと、次のループで自分の entry を引き取ったら再試行せずに復帰する。
///   受理済みの stream への 2 度目の SUBGROUP_HEADER は session の契約
///   (`draft-ietf-moq-transport-23` §12.2 の `UNEXPECTED_MESSAGE` (0x303)) で拒否されるため、
///   検出元での再試行は行わない
/// - 別の task が検出元となった場合 (受信ループ実行中の `reissue_request` による購読確立や、
///   同一 alias の並行 stream task による確立検出)、その通知で起きた task (検出先) は
///   `Subscriber` を引き取った時点では自分の stream の header が未受理であり、復帰の前に
///   自分の `recv_subgroup_header` を再試行して `Accepted` を確認する
///
/// 検出先の再試行の処理:
///
/// - `Accepted` → チャンクを戻して復帰する。この経路で `note_subscriber` を呼ぶ必要は無い
///   (各 task が自分の再試行で検出するため)
/// - `UnknownTrackAlias` → 保留を続ける。entry は引き取りの直後に削除されるため、チャンクと
///   期限を task ローカルで保持する (`continue_pending_subscriber_locally` 参照)
/// - `Discarded` / `FilteredOut` → 通常の再試行分岐と同じく `abort_subgroup_stream` で会計を
///   閉じて破棄する
/// - エラー → 通常の再試行分岐の `Err` と同じく会計を閉じずに破棄する
///
/// `note_subscriber` は理由設定済みの entry に対して no-op であり、entry の引き取りは 1 回限り
/// であるため、複数 task が各自の再試行でいくら `Accepted` を観測しても重複処理は起こらない。
///
/// FIN の終端通知は遅らせる (`draft-ietf-moq-transport-23` §3.1.3.1 の保持)。保持中に FIN を
/// 読んだ場合は entry を理由未設定のまま維持して読み出しを止め、検出先の再試行と同じく
/// 自分の `recv_subgroup_header` 再試行で確立を待つ (購読の確立は別 task の `note_subscriber`
/// でも起こる)。確立後に保持チャンクを decoder へ戻すと、decode ループが `End(Fin)` を
/// 観測して初めて session の `recv_data_stream_closed` を 1 度だけ通るため、FIN の会計
/// 通知は二重にならない。期限切れ・RESET_STREAM / STOP_SENDING 指示による abort では
/// 会計を `send_data_stream_stop_sending` で閉じるため、FIN の通知は成功経路・破棄経路の
/// どちらでも 1 回以内である。移植元の moqt-js (外部リポジトリ) は FIN の時点で subscriber
/// が居なければ entry を破棄する。本 example は保持を継続して購読の確立を待つ点が異なる
/// (`draft-ietf-moq-transport-23` §3.1.3.1 の保持の目的を FIN 済みの stream でも果たすため)。
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
        // draft-ietf-moq-transport-23 §3.1.3 (Track Alias): キャンセル済み subscription へ
        // 遅れて届いた Object。未知 alias と違い確実に不要なので保持しない。
        Ok(TrackDataAcceptance::Discarded) => {
            tracing::debug!(
                "Stream #{stream_num}: discarding objects for cancelled track alias={}",
                header.track_alias,
            );
            abort_subgroup_stream(stream, data_plane, stream_id).await;
            return false;
        }
        // draft-ietf-moq-transport-23 §3.1 (Subscriptions): フィルタ再適用の結果
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
    // 検出元フラグ。自分のループ再試行で `Accepted` を観測した task は、引き取り後に
    // 自分の再試行を行ってはいけない (session は受理済み stream への 2 度目の
    // SUBGROUP_HEADER を UNEXPECTED_MESSAGE (0x303) で拒否する)
    let mut detected_self = false;
    // FIN を観測して読み出しを止めた状態。保持チャンクは捨てず、Session への会計通知は
    // 保持解除後の decode ループで行うため、entry は理由未設定のまま保持する
    let mut hold_fin = false;
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
                    // 検出元は自分で `Accepted` を観測済みのため再試行せずに復帰する。
                    // 検出先は自分の `recv_subgroup_header` を再試行して受理を確認してから
                    // 復帰する (session は受理済み stream への 2 度目の SUBGROUP_HEADER を
                    // UNEXPECTED_MESSAGE (0x303) で拒否する)
                    let buffered: usize = ready.chunks.iter().map(Vec::len).sum();
                    let retry = if detected_self {
                        None
                    } else {
                        Some(match data_plane.recv_subgroup_header(stream_id, header) {
                            Ok(TrackDataAcceptance::Accepted) => PendingSubscriberRetry::Accepted,
                            Ok(TrackDataAcceptance::UnknownTrackAlias) => {
                                PendingSubscriberRetry::UnknownTrackAlias
                            }
                            Ok(TrackDataAcceptance::Discarded) => PendingSubscriberRetry::Discarded,
                            Ok(TrackDataAcceptance::FilteredOut) => {
                                PendingSubscriberRetry::FilteredOut
                            }
                            Err(e) => PendingSubscriberRetry::Error(e.to_string()),
                        })
                    };
                    return match pending_subscriber_action(detected_self, retry.as_ref()) {
                        PendingSubscriberAction::Resume => {
                            // 購読が確立した。保持したチャンクを decoder へ戻して続きを読む
                            for chunk in &ready.chunks {
                                decoder.push(chunk);
                            }
                            tracing::debug!(
                                "Stream #{stream_num}: released the held subgroup stream \
                                 (track alias={}, buffered={} bytes)",
                                ready.track_alias,
                                buffered,
                            );
                            true
                        }
                        PendingSubscriberAction::HoldLocal => {
                            // UnknownTrackAlias。entry は削除済みのため task ローカルで
                            // 保留を続ける
                            let hold = LocalPendingHold {
                                chunks: ready.chunks,
                                deadline_us,
                                already_fin: hold_fin,
                            };
                            continue_pending_subscriber_locally(
                                stream, data_plane, stream_id, header, decoder, stream_num, hold,
                            )
                            .await
                        }
                        PendingSubscriberAction::AbortDiscard => {
                            // 既存の再試行分岐と同じく会計を閉じて破棄する。entry は
                            // 引き取り済み・削除済みのため remove は不要である
                            if let Some(PendingSubscriberRetry::Discarded) = retry {
                                tracing::debug!(
                                    "Stream #{stream_num}: discarding objects for cancelled \
                                     track alias={}",
                                    ready.track_alias,
                                );
                            }
                            if let Some(PendingSubscriberRetry::FilteredOut) = retry {
                                tracing::debug!(
                                    "Stream #{stream_num}: object filtered out for \
                                     track alias={}",
                                    ready.track_alias,
                                );
                            }
                            abort_subgroup_stream(stream, data_plane, stream_id).await;
                            false
                        }
                        PendingSubscriberAction::DiscardQuietly => {
                            // 既存の再試行分岐の `Err` と同じく会計を閉じずに破棄する
                            let message = match &retry {
                                Some(PendingSubscriberRetry::Error(e)) => e.clone(),
                                _ => "recv_subgroup_header retry did not run".to_string(),
                            };
                            tracing::warn!(
                                "Stream #{stream_num}: failed to register subgroup header: \
                                 {message}"
                            );
                            false
                        }
                    };
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
                // FIN / RESET_STREAM / Session の STOP_SENDING 指示でストリームが終わった
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
                // 確立を検出した (検出元)。entry を引き取り待ちにして次のループでチャンクを戻す
                detected_self = true;
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
        // 遅れたぶんだけ転送が滞る (draft-ietf-moq-transport-23 §3.1.3.1 は
        // "withhold stream flow control" も MAY とするが、本 example は上限付きで読む)
        let remaining_us = deadline_us.saturating_sub(monotonic_now_us());
        if remaining_us <= 0 {
            // 期限切れ。次のループで take_ready_for が Timeout を返す
            continue;
        }
        // FIN を観測したあとは読み出しを止める。待機は保持の期限 (`deadline_us`) までの
        // 周期待ち (`LocalPendingHold` 用の poll 間隔) と STOP_SENDING 指示の監視で行い、
        // 終端済み stream の即時再配信による busy loop を作らない。購読の確立は次のループの
        // 自分の再試行 (`recv_subgroup_header`) と、他 task の `note_subscriber` で観測する
        if hold_fin {
            tokio::select! {
                biased;
                stop_code = data_plane.wait_data_stream_stop_sending(stream_id) => {
                    // STOP_SENDING 指示。既存の読み出しヘルパと同じく指示の登録を破棄して
                    // stream を止め、終端分岐 (abort) で会計を閉じる
                    data_plane.clear_data_stream_stop_sending(stream_id);
                    if let Err(e) = stream.stop_sending(stop_code).await {
                        tracing::warn!(
                            "Failed to send STOP_SENDING for data stream: \
                             stream_id={}, error_code={stop_code:#x} {e}",
                            stream_id.0
                        );
                    }
                    pending_subgroups.lock().await.note_end_of_stream(id);
                }
                _ = tokio::time::sleep(std::time::Duration::from_micros(
                    remaining_us.min(LOCAL_PENDING_HOLD_POLL_INTERVAL_US).saturating_sub(1)
                        as u64,
                )) => {}
            }
            continue;
        }
        match tokio::time::timeout(
            std::time::Duration::from_micros(remaining_us as u64),
            receive_pending_hold_stream_outcome(stream, data_plane, stream_id),
        )
        .await
        {
            Ok(Ok(RegisteredStreamData::Data(data))) => {
                pending_subgroups.lock().await.push(id, &data)
            }
            Ok(Ok(RegisteredStreamData::End(end))) => {
                match pending_hold_end_decision(Some(&end)) {
                    PendingHoldEnd::KeepHoldingWithFin => {
                        // FIN。保持チャンクは捨てず、Session への会計通知を遅らせる。
                        // "通知は最初の 1 回だけ有効" の契約により `note_end_of_stream` を
                        // 呼ばない (後から `note_subscriber` で引き取りできるようにする)。
                        // 読み出しは止める (終端済み stream は FIN を即時に再配信する)
                        hold_fin = true;
                    }
                    PendingHoldEnd::AbortDiscard => {
                        // RESET_STREAM。ストリームの内容が欠けている可能性があるため既存
                        // どおり破棄する。session への会計通知は abort 経路の
                        // `send_data_stream_stop_sending` で行い、ここでは呼ばない
                        pending_subgroups.lock().await.note_end_of_stream(id);
                    }
                }
            }
            Ok(Ok(RegisteredStreamData::StoppedBySession)) => {
                // Session の STOP_SENDING 指示 (DATA_STREAM_TIMEOUT など)。既存の読み出し
                // ヘルパが指示の登録を破棄して stream を止めたため、終端分岐 (abort) で
                // 会計を閉じる
                pending_subgroups.lock().await.note_end_of_stream(id);
            }
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
    // 読み捨てには Session 通知を遅らせる読み出しを使う。FIN を session に未通知のまま
    // abort した stream では、会計は `send_data_stream_stop_sending` で 1 度だけ閉じる
    let _ = drain_registered_stream_quietly(stream, data_plane, stream_id).await;
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
/// 同じ Group に属する (draft-ietf-moq-transport-23 §2.2 (Subgroups)) ため、Object ごとに
/// 読む必要はない。
/// `video_decode_order` は購読で共有する復号順の判定である。復号の直前に通し、復号してよい
/// Object だけを decoder へ渡す。
/// `video_delivery_order` は購読で共有する購読境界の判定である。復号の後、`frame_tx` への
/// 送出前に通す。
/// `group_switch` は購読で共有する Group の切り替え gate である。前の Group の stream が
/// 開いている間は、この Object を payload ごと保持したまま待ち、解除後に同じ decoder で
/// 復号する。保留中は「最後に受信した位置」を記録しない (移行で保留を捨てたときに、新しい
/// 購読の Location Filter がその位置から取り直せるようにするため)。
#[expect(clippy::too_many_arguments)]
async fn decode_video_stream(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    group_id: u64,
    sg_decoder: &mut SubgroupStreamDecoder,
    video_decode_order: &std::sync::Arc<tokio::sync::Mutex<VideoDecodeOrder>>,
    video_delivery_order: &std::sync::Arc<std::sync::Mutex<VideoDeliveryOrder>>,
    group_switch: &GroupSwitchHandle,
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
                    match receive_registered_stream_data_outcome(stream, data_plane, stream_id)
                        .await
                    {
                        Ok(RegisteredStreamData::Data(data)) => sg_decoder.push(&data),
                        Ok(RegisteredStreamData::End(end)) => {
                            report_mid_object_fin_if_needed(
                                data_plane,
                                stream_id,
                                &end,
                                &sg_decoder.finish(),
                            );
                            return frames;
                        }
                        Ok(RegisteredStreamData::StoppedBySession) => return frames,
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
                    request_session_termination_for_decode_error(termination_tx, &e);
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
            match receive_registered_stream_data_outcome(stream, data_plane, stream_id).await {
                Ok(RegisteredStreamData::Data(data)) => sg_decoder.push(&data),
                Ok(RegisteredStreamData::End(end)) => {
                    report_mid_object_fin_if_needed(
                        data_plane,
                        stream_id,
                        &end,
                        &sg_decoder.finish(),
                    );
                    return frames;
                }
                Ok(RegisteredStreamData::StoppedBySession) => return frames,
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
        // 移行後の再購読で使う「最後に受信した位置」を記録する。配送しない Object や
        // payload を持たない Object は再開位置として意味が無いため記録しない。再生する
        // 場合は保留の解除を待ってから記録する (保留を捨てた移行で、新しい購読の
        // Location Filter がその位置から取り直せるようにするため)
        if video_decoder.is_some() {
            wait_until_video_deliverable(
                group_switch,
                group_id,
                obj.object_id,
                last_received_locations,
            )
            .await;
        } else {
            record_last_location(
                last_received_locations,
                TrackKind::Video,
                Location {
                    group_id,
                    object_id: obj.object_id,
                },
            );
        }
        // 配送は live の Subgroup stream と FETCH 応答 stream で同じ関数を通す
        match deliver_video_object(
            group_id,
            obj.object_id,
            obj.properties_bytes.as_deref(),
            &payload,
            video_decode_order,
            video_delivery_order,
            video_decoder.as_deref_mut(),
            sink,
            recorder,
            termination_tx,
        )
        .await
        {
            VideoDelivery::Delivered(frames_delivered) => frames += frames_delivered,
            VideoDelivery::MalformedTrack | VideoDelivery::Terminated => return frames,
        }
    }
}

/// Group の切り替えの保留が解除されるのを待ち、解除されたら「最後に受信した位置」を記録する
///
/// 前の Group の stream が開いている間は、次の Group の Object を保留する。保留中は、その
/// Object を読んだ stream task が payload を持ったまま待ち、解除後に同じ decoder で復号する
/// (decoder は stream ごとに作られ、1 stream = 1 Group であるため、別の task の decoder で
/// 復号すると参照フレームが無い)。
///
/// 記録は解除の後に行う。保留したまま記録すると、session 移行で保留を捨てたときに、新しい
/// 購読の Location Filter がその位置から取り直せない。
async fn wait_until_video_deliverable(
    group_switch: &GroupSwitchHandle,
    group_id: u64,
    object_id: u64,
    last_received_locations: &std::sync::Arc<std::sync::Mutex<LastReceivedLocations>>,
) {
    group_switch.wait_until_deliver(group_id).await;
    record_last_location(
        last_received_locations,
        TrackKind::Video,
        Location {
            group_id,
            object_id,
        },
    );
}

/// 映像 Object 1 つを配送経路へ流した結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoDelivery {
    /// 配送した (復号して送出したフレーム数)。配送しなかった場合も 0 で表す
    Delivered(u64),
    /// LOC Properties が malformed Track を示した (セッション終了は依頼せず、読み取りを打ち切る)
    ///
    /// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1 (Malformed
    /// Tracks) 条件 9 はセッションを閉じず購読・fetch の cancel を求める。このためセッション
    /// 終了は依頼しない。Session の data plane も同じ Object Properties を decode するが、
    /// FETCH 応答 stream でこの結果になるのは Session が受理した後に LOC の再 decode だけが
    /// malformed Track と判定した場合である。したがって「Session が既に cancel している」
    /// ことは当てにできず、読み取りを打ち切った stream を Session の会計から外す後始末は
    /// 呼び出し側が行う。
    MalformedTrack,
    /// LOC Properties の書式違反によりセッション終了を依頼した (読み取りを打ち切る)
    Terminated,
}

/// Properties の再 decode 失敗を [`VideoDelivery`] の扱いへ写す
///
/// 分類の根拠は [`skip_object_on_properties_error`] を参照。
fn video_delivery_on_properties_error(
    termination_tx: &tokio::sync::mpsc::Sender<SessionError>,
    error: &MessageError,
) -> VideoDelivery {
    if skip_object_on_properties_error(termination_tx, error) {
        VideoDelivery::MalformedTrack
    } else {
        VideoDelivery::Terminated
    }
}

/// 映像 Object 1 つを live と同じ配送経路 (録画 → 復号順の判定 → 復号 → 購読境界の判定 →
/// 送出) へ流す
///
/// live の Subgroup stream と FETCH 応答 stream が共有する。`group_id` は Object が属する
/// Group であり、Subgroup stream ではヘッダの Group ID、FETCH 応答 stream では entry の
/// Group ID を使う。
///
/// `video_decode_order` は購読で共有する復号順の判定であり、`video_decoder` は stream ごとに
/// 作る復号器である。判定が `Skip` の Object (`Stale` / `MissingReference`) は復号しない
/// (キーフレームから復号を始める規則は [`VideoDecodeOrder`] が持つ)。録画は復号の可否に
/// 関わらず行う。`video_decode_order` は FETCH 応答でも live と共有するため、live で復号中の
/// Group より古い Group を FETCH で取得した場合、その Object は `Stale` として復号されない
/// (取得した範囲は、live の録画が始まる前に届いた分だけが録画に残る。録画は最初の
/// キーフレームを基準時刻にし、それより前の時刻の Object を破棄するためである)。
///
/// `video_delivery_order` は購読で共有する購読境界の判定である。境界以下の Object は復号は
/// するが `frame_tx` へ送出しない ([`VideoDeliveryOrder`])。録画は境界の判定より前に行うため、
/// 境界以下の Object も録画の対象になる。
#[expect(clippy::too_many_arguments)]
async fn deliver_video_object(
    group_id: u64,
    object_id: u64,
    properties_bytes: Option<&[u8]>,
    payload: &[u8],
    video_decode_order: &std::sync::Arc<tokio::sync::Mutex<VideoDecodeOrder>>,
    video_delivery_order: &std::sync::Arc<std::sync::Mutex<VideoDeliveryOrder>>,
    video_decoder: Option<&mut decoder::VideoDecoder>,
    sink: &FrameSink<'_>,
    recorder: Option<&RecorderSender>,
    termination_tx: &tokio::sync::mpsc::Sender<SessionError>,
) -> VideoDelivery {
    // 表示時刻を決めるために LOC の Timestamp も取り出す
    // (書式違反は配送の有無や処理済みフラグに関わらず毎 Object で検出する)
    let (timestamp_us, timestamp_kind) = match extract_timestamp_timescale(properties_bytes) {
        Ok((timestamp, timescale)) => (
            loc_timestamp_us(timestamp, timescale),
            timestamp_kind_of(timestamp, timescale),
        ),
        Err(e) => return video_delivery_on_properties_error(termination_tx, &e),
    };
    let video_config = match extract_video_config(properties_bytes) {
        Ok(config) => config,
        Err(e) => return video_delivery_on_properties_error(termination_tx, &e),
    };
    if let Some(recorder) = recorder
        && let Err(e) =
            record_video_object(recorder, properties_bytes, video_config.as_deref(), payload)
    {
        return video_delivery_on_properties_error(termination_tx, &e);
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
        Err(e) => return video_delivery_on_properties_error(termination_tx, &e),
    };
    // Prior Object ID Gap は Object Properties から読む (Property が無ければ 0)
    let prior_object_id_gap = match prior_object_id_gap_of(properties_bytes) {
        Ok(gap) => gap,
        Err(e) => return video_delivery_on_properties_error(termination_tx, &e),
    };
    // 復号してよい Object か判定する。Group ごとに別の stream で届くため、判定の状態は
    // 購読で共有する (draft-ietf-moq-transport-23 §2.2 (Subgroups))。ロックは判定の
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
    // 復号は購読の境界の判定より前に行う。境界以下の Object も、境界以降のフレームが参照する
    // 可能性があるため復号する (moqt-js の CatchUpGate と同じ扱い)
    let decoded = tokio::task::block_in_place(|| {
        decode_video_frames(payload, video_config.as_deref(), video_decoder)
    });
    // 購読の境界の判定は復号の後、`frame_tx` への送出前に行う。境界以下は購読より前に
    // publish された Object であり、表示しない (録画は境界の判定より前に行うため残る)
    let (before_boundary, boundary) = {
        let mut order = video_delivery_order
            .lock()
            .expect("video delivery order lock is not poisoned");
        let boundary = order.boundary();
        (
            order.is_before_boundary(Location {
                group_id,
                object_id,
            }),
            boundary,
        )
    };
    if before_boundary {
        tracing::debug!(
            "Skipping video display before the subscription boundary (group={group_id}, object={object_id}, boundary={boundary:?})",
        );
        return VideoDelivery::Delivered(0);
    }
    let frames = send_decoded_frames(decoded, timestamp_us, timestamp_kind, sink);
    VideoDelivery::Delivered(frames)
}

/// 映像 Object 1 つを復号する
///
/// 復号エラーは warn を残して空のフレーム列を返す (1 Object の復号失敗で stream 全体の
/// 読み取りを打ち切らない)。
fn decode_video_frames(
    payload: &[u8],
    video_config: Option<&[u8]>,
    decoder: &mut decoder::VideoDecoder,
) -> Vec<DecodedVideoFrame> {
    match decoder.decode(payload, video_config) {
        Ok(frames) => frames,
        Err(e) => {
            tracing::warn!("Video decode error: {e}, payload_len={}", payload.len());
            Vec::new()
        }
    }
}

/// 復号済みの映像フレームに表示時刻を載せて `frame_tx` へ送出する
///
/// 送出したフレーム数を返す。送り先が閉じた場合はその時点で打ち切る。
fn send_decoded_frames(
    decoded: Vec<DecodedVideoFrame>,
    timestamp_us: Option<i64>,
    timestamp_kind: TimestampKind,
    sink: &FrameSink<'_>,
) -> u64 {
    let mut frames: u64 = 0;
    for mut frame in decoded {
        // 表示時刻を決めるのは時間軸であり、TIMESTAMP と種類は Object からここで載せる
        frame.timestamp_us = timestamp_us;
        frame.timestamp_kind = timestamp_kind;
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
///
/// IMMUTABLE_PROPERTIES (0x0B) の内側だけにある LOC 固有の値域・長さ違反 (Audio Level の
/// 8 bit 超過や Video Frame Marking の長さ違反など) は、`LocProperties` の getter が `None`
/// に写すため、抽出関数は `Ok(None)` を返す。最上位リストの構造破損は
/// `LocProperties::decode` が `Err` にし、内側の構造破損は `LocProperties::decode` では
/// 検出せず getter が `None` に写す (Session 経路では `ObjectProperties::decode` が先に
/// 拒否する)。内側だけの LOC 固有検証 (Audio Level の値域や Video Frame Marking の長さ) で
/// `MalformedKeyValue` を返すのは別課題である (docs/missing-features.md §4.1)。
/// CODEC_STRING (0x11) の不正 UTF-8 は `LocProperties::decode` が解析できる範囲で
/// `MalformedKeyValue` として返すが、このファイルの抽出関数は CODEC_STRING を扱わない。
type ExtractResult<T> = std::result::Result<T, MessageError>;

/// object の properties バイト列から PROP_VIDEO_CONFIG を取り出す
///
/// `Ok(None)` は「Properties が無い、または PROP_VIDEO_CONFIG を持たない」を意味する。
/// `LocProperties::decode` の失敗は `Err` で返し、
/// 失敗の分類 (セッション終了を依頼するか、malformed Track として Object を捨てるか) は
/// [`skip_object_on_properties_error`] が行う。書式違反を「プロパティ無し」に潰さない。
/// 未知 ID の複数インスタンスは draft-ietf-moq-transport-23 §8.4 (Track and Object
/// Properties) の "MUST allow multiple instances of the Property" により正当であり、
/// `LocProperties::decode` は受理する。単一に限られる既知 ID の重複は §12.1 (Malformed
/// Tracks) 条件 9 の malformed Track である。
///
/// draft-ietf-moq-loc-05 §3.1.2 (Immutable Properties) に従い IMMUTABLE_PROPERTIES (0x0B)
/// の内側だけに Video Config を持つ Object もあるため、`LocProperties::video_config`
/// (最上位と内側を探索する) で構成情報を取得する。この節番号は draft 由来であり、将来の
/// draft 改版で変わる可能性がある。
fn extract_video_config(properties_bytes: Option<&[u8]>) -> ExtractResult<Option<Vec<u8>>> {
    let Some(bytes) = properties_bytes else {
        return Ok(None);
    };
    let (props, _) = LocProperties::decode(bytes)?;
    Ok(props.video_config().map(<[u8]>::to_vec))
}

/// object の properties バイト列から PROP_TIMESTAMP / PROP_TIMESCALE を取り出す
///
/// `Ok((None, None))` は「Properties が無い、または Timestamp / Timescale を持たない」を
/// 意味する。`LocProperties::decode` の失敗は `Err` で返し、
/// 失敗の分類 (セッション終了を依頼するか、malformed Track として Object を捨てるか) は
/// [`skip_object_on_properties_error`] が行う。書式違反を「プロパティ無し」に潰さない。
/// 未知 ID の複数インスタンスは draft-ietf-moq-transport-23 §8.4 (Track and Object
/// Properties) の "MUST allow multiple instances of the Property" により正当であり、
/// `LocProperties::decode` は受理する。単一に限られる既知 ID の重複は §12.1 (Malformed
/// Tracks) 条件 9 の malformed Track である。
///
/// draft-ietf-moq-loc-05 §3.1.2 (Immutable Properties) に従い IMMUTABLE_PROPERTIES (0x0B)
/// の内側だけに Timestamp / Timescale を持つ Object もあるため、`LocProperties::timestamp` /
/// `LocProperties::timescale` (最上位と内側を探索する) で表示時刻を取得する。この節番号は
/// draft 由来であり、将来の draft 改版で変わる可能性がある。
fn extract_timestamp_timescale(
    properties_bytes: Option<&[u8]>,
) -> ExtractResult<(Option<u64>, Option<u64>)> {
    let Some(bytes) = properties_bytes else {
        return Ok((None, None));
    };
    let (props, _) = LocProperties::decode(bytes)?;
    Ok((props.timestamp(), props.timescale()))
}

/// object の properties バイト列から PROP_VIDEO_FRAME_MARKING の I ビットを取り出す
///
/// `Ok(None)` は「Properties が無い、または PROP_VIDEO_FRAME_MARKING を持たない」を意味する。
/// RFC 9626 §3.2 (Short Extension for Non-Scalable Streams) の 1 octet 形式は
/// `|S|E|I|D|0 0 0 0|` であり、I ビット (0x20) が独立フレーム (キーフレーム) を表す。
/// この節番号・ビット割当は RFC 由来であり将来変更される可能性がある。
///
/// draft-ietf-moq-loc-05 §3.1.2 (Immutable Properties) に従い IMMUTABLE_PROPERTIES (0x0B)
/// の内側だけに Video Frame Marking を持つ Object もあるため、
/// `LocProperties::video_frame_marking` (最上位と内側を探索する) で I ビットを判定する。
/// この節番号は draft 由来であり、将来の draft 改版で変わる可能性がある。
fn extract_video_keyframe(properties_bytes: Option<&[u8]>) -> ExtractResult<Option<bool>> {
    let Some(bytes) = properties_bytes else {
        return Ok(None);
    };
    let (props, _) = LocProperties::decode(bytes)?;
    Ok(props
        .video_frame_marking()
        .map(|b| b.first().is_some_and(|v| v & 0x20 != 0)))
}

/// video object がキーフレームかどうかを判定する
///
/// キーフレーム判定は PROP_VIDEO_FRAME_MARKING (I ビット) を使い、プロパティが無い場合は
/// PROP_VIDEO_CONFIG の有無で代用する (Group の先頭がキーフレームである publisher の映像も
/// 復号できるようにするため)。同じ規則を録画と復号順の判定で共有する。
/// 書式違反は `Err` で返し、分類は [`skip_object_on_properties_error`] が行う。
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
/// `Err` で返し、分類は [`skip_object_on_properties_error`] が行う。
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
/// (draft-ietf-moq-loc-05 §2.3.1.1)。音声のサンプル数でもフレーム数でもないため、
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

/// LOC の Timestamp と Timescale から TIMESTAMP の種類を求める
///
/// moqt-js の `createMediaSubscriber` と同じ規則である。TIMESTAMP が無ければ
/// [`TimestampKind::None`]、TIMESCALE が無ければ TIMESTAMP は Unix epoch からの
/// マイクロ秒 ([`TimestampKind::WallClock`])、TIMESCALE があれば publisher のメディア時刻
/// ([`TimestampKind::MediaTime`]) である (draft-ietf-moq-loc-05 §2.3.1.1)。
/// 受信側の時間軸へ観測して表示時刻を決められるのは壁時計の TIMESTAMP だけである。
///
/// TIMESCALE が 0 のときも「TIMESCALE がある」ものとしてメディア時刻にする
/// ([`loc_timestamp_us`] は 0 をマイクロ秒として扱うが、この判定だけは
/// 「TIMESCALE が無い」という条件で分ける)。
fn timestamp_kind_of(timestamp: Option<u64>, timescale: Option<u64>) -> TimestampKind {
    match timestamp {
        None => TimestampKind::None,
        Some(_) if timescale.is_none() => TimestampKind::WallClock,
        Some(_) => TimestampKind::MediaTime,
    }
}

/// 受信の入口で測った入力の遅れ (マイクロ秒) を求める
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
/// 端末間遅延 (publisher の壁時計 TIMESTAMP から受信まで) の定期ログ
///
/// ストリーム経路と datagram 経路の両方から呼ばれるため、最後に出力した時刻を
/// プロセス全体で共有して 5 秒に 1 回に制限する。単調時計ではなく壁時計で比較するが、
/// 診断用の間隔判定であり、NTP のずれで間隔が多少伸び縮みしても害はない。
///
/// 出力レベルは debug である。relay が peer 窓のぶんだけ送信バッファへデータを
/// 溜めているとこの値が伸びるため、受信側が広告する窓の大きさの影響を見る診断である。
/// 通常の運用では出さない (`RUST_LOG=debug` を指定したときだけ出る)。
///
/// `arrival_us` は観測の基準に使う壁時計 (ログの間隔判定にも使う)。
/// `lag_us` は Object の到着時刻 − TIMESTAMP である。
fn log_audio_end_to_end_lag(arrival_us: i64, lag_us: i64) {
    static LAST_LOGGED_AT_MS: std::sync::atomic::AtomicI64 =
        std::sync::atomic::AtomicI64::new(i64::MIN);
    const LOG_INTERVAL_MS: i64 = 5_000;

    let now_ms = arrival_us / 1_000;
    let last_ms = LAST_LOGGED_AT_MS.load(std::sync::atomic::Ordering::Relaxed);
    // 未出力の初期値 (i64::MIN) との減算を飽和させる。壁時計の経過が負になったとき
    // (時計の巻き戻り) も出力を抑える方向に倒れる
    if now_ms.saturating_sub(last_ms) < LOG_INTERVAL_MS {
        return;
    }
    // 複数の経路が同時に通っても 1 回だけ出るようにする。負けた経路は次の機会に出す
    if LAST_LOGGED_AT_MS
        .compare_exchange(
            last_ms,
            now_ms,
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        )
        .is_err()
    {
        return;
    }
    tracing::debug!(
        "Audio end-to-end lag: {}ms (object timestamp to arrival)",
        lag_us / 1_000
    );
}

fn receive_input_lag_us(
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
    Some(arrival_us.saturating_sub(pts_us))
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
/// `LocProperties::decode` の失敗は `Err` で返し、
/// 失敗の分類 (セッション終了を依頼するか、malformed Track として Object を捨てるか) は
/// [`skip_object_on_properties_error`] が行う。書式違反を「プロパティ無し」に潰さない。
/// 未知 ID の複数インスタンスは draft-ietf-moq-transport-23 §8.4 (Track and Object
/// Properties) の "MUST allow multiple instances of the Property" により正当であり、
/// `LocProperties::decode` は受理する。単一に限られる既知 ID の重複は §12.1 (Malformed
/// Tracks) 条件 9 の malformed Track である。
///
/// draft-ietf-moq-loc-05 §3.1.2 (Immutable Properties) に従い IMMUTABLE_PROPERTIES (0x0B)
/// の内側だけに Audio Config を持つ Object もあるため、`LocProperties::audio_config`
/// (最上位と内側を探索する) で構成情報を取得する。この節番号は draft 由来であり、将来の
/// draft 改版で変わる可能性がある。
fn extract_audio_config(properties_bytes: Option<&[u8]>) -> ExtractResult<Option<Vec<u8>>> {
    let Some(bytes) = properties_bytes else {
        return Ok(None);
    };
    let (props, _) = LocProperties::decode(bytes)?;
    Ok(props.audio_config().map(<[u8]>::to_vec))
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
/// 配送されるためである (draft-ietf-moq-loc-05 §4.2)。カタログも stream で届く。
fn plan_datagram<'a>(raw: &'a [u8], track_map: &HashMap<u64, TrackKind>) -> DatagramAction<'a> {
    // Session が同じバイト列を検証済みであるため、ここでの decode 失敗は実装の不整合を表す。
    // メディアを黙って落とさないよう警告する
    let Ok((datagram, header_len)) = ObjectDatagram::decode(raw) else {
        tracing::warn!("Failed to decode an object datagram the session already accepted");
        return DatagramAction::Ignore;
    };
    // Object Status のみの Object は payload を持たない (draft-ietf-moq-transport-23 §11.2.1)
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
        // SUBSCRIBE_TRACKS 由来の購読は再生・録画に使わない
        Some(TrackKind::Discard) => DatagramAction::Ignore,
        // 未知の alias は Session が Accepted にしないため到達しない
        None => DatagramAction::Ignore,
    }
}

/// 1 本の audio subgroup ストリームから音声パケットを順次デコードする
///
/// LOC draft-ietf-moq-loc-05 §4.1 (Application with one audio track) では「1 audio chunk = 1 Object = 1 Group」と例示されているが、
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
                    match receive_registered_stream_data_outcome(stream, data_plane, stream_id)
                        .await
                    {
                        Ok(RegisteredStreamData::Data(data)) => sg_decoder.push(&data),
                        Ok(RegisteredStreamData::End(end)) => {
                            report_mid_object_fin_if_needed(
                                data_plane,
                                stream_id,
                                &end,
                                &sg_decoder.finish(),
                            );
                            return chunks;
                        }
                        Ok(RegisteredStreamData::StoppedBySession) => return chunks,
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
                    request_session_termination_for_decode_error(delivery.termination_tx, &e);
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
            match receive_registered_stream_data_outcome(stream, data_plane, stream_id).await {
                Ok(RegisteredStreamData::Data(data)) => sg_decoder.push(&data),
                Ok(RegisteredStreamData::End(end)) => {
                    report_mid_object_fin_if_needed(
                        data_plane,
                        stream_id,
                        &end,
                        &sg_decoder.finish(),
                    );
                    return chunks;
                }
                Ok(RegisteredStreamData::StoppedBySession) => return chunks,
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
        // 移行後の再購読で使う「最後に受信した位置」を記録する
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

/// FETCH 応答 stream の配送先
///
/// 自側が FETCH を発行するのは映像と catalog (catalog 購読の fill fetch) だけである。音声の
/// 明示 FETCH は発行しないため、音声を配送先に持たないことを型で保証する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FetchTarget {
    /// 映像のメディア FETCH
    Video,
    /// catalog 購読の fill fetch
    Catalog,
}

/// FETCH 応答 stream をどう扱うか
///
/// メディアの FETCH は映像だけを対象にする。音声の明示 FETCH は起動・再購読・再発行の
/// いずれでも発行しない (購読境界の規則により起動・再生・録画への寄与が無いため。音声の
/// 購読境界の規則は将来の fill 経路の防御として残す)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FetchStreamAction {
    /// 映像の FETCH 応答。映像の配送経路へ流す
    DeliverVideo,
    /// catalog 購読の fill fetch 応答。カタログの状態へ適用する
    DeliverCatalog,
    /// 配送先が無い、または自側が発行していない要求の応答。読み捨てる
    Ignore,
}

/// FETCH 応答 stream の Request ID から配送先を決める
///
/// `fetch_targets` は自側が発行した映像のメディア FETCH の Request ID と、catalog 購読の
/// Request ID を「Request ID → 配送先」で記録した対応表である。FILL_PARAMETERS 付き
/// SUBSCRIBE へは publisher が fill fetch stream を返すため
/// (draft-ietf-moq-transport-23 §3.4 (Fill Semantics))、その Request ID も登録して
/// カタログとして適用する。
///
/// 表に無い Request ID でも「自側の要求と対応しない」とは限らない。PUBLISH 起点の初回 fill は
/// peer の PUBLISH の Request ID を載せるため (§3.4.1 (Opening and Closing Fill Fetch
/// Streams)。このアプリは PUBLISH に UNINTERESTED を返すため配送先は無い)、表に現れない。
/// したがって本関数の `Ignore` は「このアプリに配送先が無い」ことだけを意味し、Session への
/// 登録 (FETCH_HEADER の受理と stream の会計) は呼び出し側が本関数より先に行う。
/// 音声の明示 FETCH は発行しないため、音声の Request ID も登録されない。
fn fetch_stream_action(
    request_id: u64,
    fetch_targets: &HashMap<u64, FetchTarget>,
) -> FetchStreamAction {
    match fetch_targets.get(&request_id) {
        Some(FetchTarget::Video) => FetchStreamAction::DeliverVideo,
        Some(FetchTarget::Catalog) => FetchStreamAction::DeliverCatalog,
        None => FetchStreamAction::Ignore,
    }
}

/// FETCH 応答 stream から次の entry と Object の payload を読む
///
/// `Ok(None)` は stream の終端 (FIN / RESET_STREAM / Session からの STOP_SENDING 指示) を
/// 表す。`Err` は wire の書式違反、Session への登録失敗、payload を読み切る前の終端を
/// 表し、呼び出し側が読み取りを打ち切る (ログは呼び出し側が出す)。
/// End of Range / End of Group / End of Track の entry は payload を持たないため
/// payload は空になる (draft-ietf-moq-transport-23 §11.4.2 (End of Range) /
/// §11.4.2.2 (Prior Object Fields))。
async fn read_fetch_entry(
    stream: &mut transport::RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    decoder: &mut FetchStreamDecoder,
) -> Result<Option<(DecodedFetchEntry, Vec<u8>)>> {
    loop {
        let entry = match decoder.try_decode_entry()? {
            Some(entry) => entry,
            None => match receive_registered_stream_data_outcome(stream, data_plane, stream_id)
                .await?
            {
                RegisteredStreamData::Data(data) => {
                    decoder.push(&data);
                    continue;
                }
                RegisteredStreamData::End(end) => {
                    report_mid_object_fin_if_needed(data_plane, stream_id, &end, &decoder.finish());
                    return Ok(None);
                }
                RegisteredStreamData::StoppedBySession => return Ok(None),
            },
        };
        // entry を読むたびに Session の activity を更新する。Object と End of Range /
        // marker のどれも DATA_STREAM_TIMEOUT の activity に含まれる
        // (draft-ietf-moq-transport-23 §12.2 (Session Termination Codes))。
        // 失敗は読み取りを打ち切る (この stream の会計が進まない)
        data_plane.recv_fetch_entry(stream_id, &entry)?;
        let payload = match &entry {
            // payload_length == 0 の Object (Object Status のみ) は payload を持たない
            DecodedFetchEntry::Object(obj) if obj.payload_length > 0 => loop {
                if let Some(payload) = decoder.try_read_payload() {
                    break payload;
                }
                match receive_registered_stream_data_outcome(stream, data_plane, stream_id).await? {
                    RegisteredStreamData::Data(data) => decoder.push(&data),
                    RegisteredStreamData::End(end) => {
                        report_mid_object_fin_if_needed(
                            data_plane,
                            stream_id,
                            &end,
                            &decoder.finish(),
                        );
                        return Err(Error::Other(
                            "stream closed before fetch payload".to_string(),
                        ));
                    }
                    RegisteredStreamData::StoppedBySession => {
                        return Err(Error::Other(
                            "stream stopped before fetch payload".to_string(),
                        ));
                    }
                }
            },
            _ => Vec::new(),
        };
        return Ok(Some((entry, payload)));
    }
}

/// FETCH 応答の Object 以外の entry をログに残す
///
/// Object 以外の entry は配送しない。End of Range の 4 種は取得できなかった理由が異なる
/// (存在しない / 不明 / 期限切れ / Gone) ため、それぞれ別のメッセージで残す
/// (draft-ietf-moq-transport-23 §3.2.2 (Gaps in a Fetch Stream) /
/// §11.4.2 (End of Range))。Gone は元の publisher が再配送できないだけで存在自体は
/// 否定されていない状態であり (同 §2.1.2 (Object States))、存在しないと確定した
/// non-existent と混同しないよう別の理由として扱う。
/// End of Group / End of Track は直前の Object が Group / Track の最終 Object であることを
/// 示す marker であり (同 §11.4.2.2 (Prior Object Fields))、応答の終端としてログに残すだけで
/// 配送しない。Object の entry では何もしない。
fn log_fetch_boundary(stream_num: u64, entry: &DecodedFetchEntry) {
    let (reason, group_id, object_id) = match entry {
        DecodedFetchEntry::Object(_) => return,
        DecodedFetchEntry::EndOfNonExistentRange {
            group_id,
            object_id,
        } => ("non-existent", *group_id, *object_id),
        DecodedFetchEntry::EndOfUnknownRange {
            group_id,
            object_id,
        } => ("unknown", *group_id, *object_id),
        DecodedFetchEntry::EndOfTimedOutRange {
            group_id,
            object_id,
        } => ("timed-out", *group_id, *object_id),
        DecodedFetchEntry::EndOfGoneRange {
            group_id,
            object_id,
        } => ("gone", *group_id, *object_id),
        DecodedFetchEntry::EndOfGroup { group_id } => {
            tracing::info!("Stream #{stream_num}: FETCH group ended: group={group_id}");
            return;
        }
        DecodedFetchEntry::EndOfTrack { group_id } => {
            tracing::info!("Stream #{stream_num}: FETCH track ended: group={group_id}");
            return;
        }
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
/// 始まる前に届いた分だけが残る)。購読の境界の判定も live と共有する
/// ([`VideoDeliveryOrder`])。
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
    video_delivery_order: &std::sync::Arc<std::sync::Mutex<VideoDeliveryOrder>>,
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
            log_fetch_boundary(stream_num, &entry);
            continue;
        };
        // payload を持たない Object (Object Status のみ) は live と同じく配送しない
        if obj.payload_length == 0 {
            continue;
        }
        // 移行後の再購読で使う「最後に受信した位置」を記録する
        record_last_location(
            last_received_locations,
            TrackKind::Video,
            Location {
                group_id: obj.group_id,
                object_id: obj.object_id,
            },
        );
        let delivery = deliver_video_object(
            obj.group_id,
            obj.object_id,
            obj.properties_bytes.as_deref(),
            &payload,
            video_decode_order,
            video_delivery_order,
            video_decoder.as_deref_mut(),
            sink,
            recorder,
            termination_tx,
        )
        .await;
        if video_fetch_stream_needs_cleanup(delivery) {
            // malformed Track はセッション終了ではなく購読・fetch の cancel で済ませるが
            // (draft-ietf-moq-transport-23 §12.1 (Malformed Tracks))、FETCH 応答 stream を
            // Session の会計に残したままだと DATA_STREAM_TIMEOUT でセッションが閉じる
            // (同 §12.2 (Session Termination Codes))。読み切れなかった stream を会計から
            // 外す (他の Err 経路と同じ後始末)
            let _ = data_plane.send_data_stream_stop_sending(stream_id);
            let _ = drain_registered_stream_to_end(stream, data_plane, stream_id).await;
            return;
        }
        // セッション終了を依頼した場合は main ループが接続を閉じる。依頼が通らなかった
        // 場合も main ループは既に停止しているため、stream の後始末は要らない
        if matches!(delivery, VideoDelivery::Terminated) {
            return;
        }
    }
}

/// FETCH 応答の映像配送を打ち切った後、stream を Session の会計から外すかを返す
///
/// malformed Track はセッションを閉じず、同じ Track の購読・fetch の cancel で扱う
/// (draft-ietf-moq-transport-23 §12.1 (Malformed Tracks))。ただし FETCH 応答 stream を
/// Session の `data_streams.incoming` に残したままだと、同 §12.2 (Session Termination
/// Codes) の DATA_STREAM_TIMEOUT でセッションが閉じる。STOP_SENDING で stream を会計から
/// 外し、残りのバイトを読み切る必要がある。
///
/// セッション終了を依頼した [`VideoDelivery::Terminated`] と、配送を続ける
/// [`VideoDelivery::Delivered`] では後始末しない。
fn video_fetch_stream_needs_cleanup(delivery: VideoDelivery) -> bool {
    matches!(delivery, VideoDelivery::MalformedTrack)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shiguredo_moqt::stream::subgroup::SubgroupIdMode;
    use shiguredo_moqt::stream::subgroup::SubgroupObject;

    /// `Subscriber` 引き取りの処理分類を固定する
    ///
    /// 検出元 (自分の再試行で `Accepted` を観測した task) は、session は受理済みの stream に
    /// 対する 2 度目の SUBGROUP_HEADER を UNEXPECTED_MESSAGE (0x303) で拒否するため、
    /// 常に再試行せずに復帰する。検出先は自分の再試行結果で処理を決める。
    mod pending_subscriber_action_tests {
        use super::*;

        /// 検出元は再試行結果が既に定まっていない case でも復帰すること
        #[test]
        fn detected_self_resumes_without_retry() {
            assert!(
                matches!(
                    pending_subscriber_action(true, None),
                    PendingSubscriberAction::Resume
                ),
                "検出元は再試行せず復帰すること"
            );
        }

        /// 検出先が Accept したなら復帰すること
        #[test]
        fn cross_wake_with_accepted_resumes() {
            assert!(
                matches!(
                    pending_subscriber_action(false, Some(&PendingSubscriberRetry::Accepted)),
                    PendingSubscriberAction::Resume
                ),
                "自分の再試行の受理では復帰すること"
            );
        }

        /// 検出先が未知 alias なら task ローカルの保留継続になること
        #[test]
        fn cross_wake_with_unknown_alias_holds_locally() {
            assert!(
                matches!(
                    pending_subscriber_action(
                        false,
                        Some(&PendingSubscriberRetry::UnknownTrackAlias)
                    ),
                    PendingSubscriberAction::HoldLocal
                ),
                "未知 alias で保留を続けること"
            );
        }

        /// 検出先が破棄判定なら会計を閉じること
        #[test]
        fn cross_wake_with_discard_aborts() {
            assert!(
                matches!(
                    pending_subscriber_action(false, Some(&PendingSubscriberRetry::Discarded)),
                    PendingSubscriberAction::AbortDiscard
                ),
                "Discarded は会計を閉じて破棄すること"
            );
            assert!(
                matches!(
                    pending_subscriber_action(false, Some(&PendingSubscriberRetry::FilteredOut)),
                    PendingSubscriberAction::AbortDiscard
                ),
                "FilteredOut は会計を閉じて破棄すること"
            );
        }

        /// 検出先の再試行がエラーのときは会計を閉じずに破棄すること
        #[test]
        fn cross_wake_with_error_discards_quietly() {
            assert!(
                matches!(
                    pending_subscriber_action(
                        false,
                        Some(&PendingSubscriberRetry::Error(
                            "subgroup header received for unknown stream id".to_string()
                        ))
                    ),
                    PendingSubscriberAction::DiscardQuietly
                ),
                "エラーは会計を閉じずに破棄すること (既存の Err 分岐と同じ)"
            );
        }

        /// 検出先で再試行が行われていない (誤用) のときは破棄に倒すこと
        #[test]
        fn cross_wake_without_retry_discards_quietly() {
            assert!(
                matches!(
                    pending_subscriber_action(false, None),
                    PendingSubscriberAction::DiscardQuietly
                ),
                "再試行の無い検出先は破棄に倒すこと"
            );
        }
    }

    /// 保持ループの終端観測の処理分類を固定する
    ///
    /// FIN は保持を継続する (`draft-ietf-moq-transport-23` §3.1.3.1 の保持の目的を FIN 済み
    /// stream でも果たす)。RESET_STREAM と Session の STOP_SENDING 指示は stream の内容が
    /// 欠けている可能性があるため既存どおり abort する。
    mod pending_hold_end_decision_tests {
        use super::*;

        /// FIN は保持を継続すること
        #[test]
        fn fin_keeps_holding() {
            assert!(
                matches!(
                    pending_hold_end_decision(Some(&RequestStreamEnd::Fin)),
                    PendingHoldEnd::KeepHoldingWithFin
                ),
                "FIN は保持を継続すること"
            );
        }

        /// RESET_STREAM は既存どおり abort すること
        #[test]
        fn reset_aborts() {
            assert!(
                matches!(
                    pending_hold_end_decision(Some(&RequestStreamEnd::Reset {
                        error_code: None,
                        reliable_size: None,
                    })),
                    PendingHoldEnd::AbortDiscard
                ),
                "RESET_STREAM は abort すること"
            );
        }

        /// Session の STOP_SENDING 指示 (end が None) は既存どおり abort すること
        #[test]
        fn stop_sending_indication_aborts() {
            assert!(
                matches!(
                    pending_hold_end_decision(None),
                    PendingHoldEnd::AbortDiscard
                ),
                "STOP_SENDING 指示は abort すること"
            );
        }
    }

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

    use shiguredo_moqt::error::SESSION_MALFORMED_KEY_VALUE;
    use shiguredo_moqt::loc::LocProperty;
    use shiguredo_moqt::message_parameter::LocationFilterUpdate;
    use shiguredo_opus::{Application, Encoder, EncoderConfig, FrameDuration};

    /// 映像トラックの SUBSCRIBE が新しい Group の開始を要求すること
    ///
    /// `NEW_GROUP_REQUEST=0` は「購読者が Group の情報を持たない」ことを示し、publisher は
    /// できるだけ早く新しい Group を開始する (draft-ietf-moq-transport-23 §9.19.19)。
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
        assert_eq!(
            parameters
                .location_filter_typed()
                .expect("Location Filter の解釈が失敗しないこと"),
            None,
            "起動時の映像トラックの SUBSCRIBE には Location Filter を載せないこと"
        );
        assert!(
            parameters.fill_parameters().is_none(),
            "映像トラックの SUBSCRIBE には FILL_PARAMETERS を載せないこと"
        );
    }

    /// 移行後の再購読では最後に受信した位置を Absolute Start として載せること
    ///
    /// draft-ietf-moq-transport-23 §3.3.1 (Location Filters) / §9.19.9 (LOCATION FILTER
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
            "最後に受信した位置を Absolute Start にすること"
        );
        assert!(
            parameters.fill_parameters().is_none(),
            "映像の再購読にも FILL_PARAMETERS を載せないこと"
        );
    }

    /// 音声の再購読も最後に受信した位置を Absolute Start として載せること
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
            "最後に受信した位置を Absolute Start にすること"
        );
        assert!(
            parameters.fill_parameters().is_none(),
            "音声の再購読にも FILL_PARAMETERS を載せないこと"
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
    /// TIMESTAMP の巻き戻りを落とす (draft-ietf-moq-transport-23 §3.3.1 (Location Filters)
    /// / §9.19.9 (LOCATION FILTER Parameter) Table 6。draft 由来の仕様であり将来変更
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
            goaway_request_kind(1, Some(1), Some(2), Some(3)),
            Some(TrackKind::Catalog),
            "catalog の Request ID を判定できること"
        );
        assert_eq!(
            goaway_request_kind(2, Some(1), Some(2), Some(3)),
            Some(TrackKind::Video),
            "映像の Request ID を判定できること"
        );
        assert_eq!(
            goaway_request_kind(3, Some(1), Some(2), Some(3)),
            Some(TrackKind::Audio),
            "音声の Request ID を判定できること"
        );
        assert_eq!(
            goaway_request_kind(4, Some(1), Some(2), Some(3)),
            None,
            "未知の Request ID は判定できないこと"
        );
        assert_eq!(
            goaway_request_kind(2, Some(1), None, None),
            None,
            "購読していない track の Request ID は判定できないこと"
        );
        assert_eq!(
            goaway_request_kind(0, None, Some(0), None),
            Some(TrackKind::Video),
            "catalog が恒久終了している場合は照合対象から外すこと (移行先で media request id 0 との衝突を防ぐ)"
        );
    }

    /// request stream の GOAWAY が FETCH の Request ID も対象にできること
    ///
    /// FETCH は応答の配送先を持たないため、購読の Request ID との照合では判定できない。
    /// 再発行に必要な指定を保持する対応表で判定する。購読と FETCH の両方に同じ Request ID が
    /// 現れることは Session の採番では起きないが、照合の優先順位を固定しておく。
    #[test]
    fn goaway_request_target_matches_fetches() {
        let fetch_request = FetchRequestTarget {
            target: SubscribeTarget {
                namespace: TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("名前空間を作れること"),
                track_name: b"video".to_vec(),
            },
            parameters: MessageParameters::new(),
        };
        let mut fetch_requests = HashMap::new();
        fetch_requests.insert(9, fetch_request);
        assert_eq!(
            goaway_request_target(1, Some(1), Some(2), Some(3), &fetch_requests),
            Some(GoawayRequestTarget::Subscribe(TrackKind::Catalog)),
            "購読の Request ID を購読として判定すること"
        );
        assert_eq!(
            goaway_request_target(9, Some(1), Some(2), Some(3), &fetch_requests),
            Some(GoawayRequestTarget::Fetch),
            "FETCH の Request ID を FETCH として判定すること"
        );
        assert_eq!(
            goaway_request_target(4, Some(1), Some(2), Some(3), &fetch_requests),
            None,
            "どちらの対応表にも無い Request ID は判定できないこと"
        );
        assert_eq!(
            goaway_request_target(2, Some(1), Some(2), Some(3), &fetch_requests),
            Some(GoawayRequestTarget::Subscribe(TrackKind::Video)),
            "購読と FETCH の両方に一致する場合は購読を優先すること"
        );
    }

    /// side session を共有するキーが接続先の違いを区別すること
    ///
    /// 同じ URL を指す request だけが 1 本の side session を共有する。authority / path /
    /// fragment のどれかが違えば別の session になる (`same_session_url` と同じ比較対象)。
    #[test]
    fn side_session_url_key_identifies_the_connection_target() {
        let base = tokio_moq::parse_url("moqt://relay.example:443/live#msf:moq-example--video")
            .expect("URL をパースできること");
        let same = tokio_moq::parse_url("moqt://relay.example:443/live#msf:moq-example--video")
            .expect("同じ URL をパースできること");
        assert_eq!(
            side_session_url_key(&base),
            side_session_url_key(&same),
            "同じ接続先は同じキーになること"
        );
        assert!(
            !side_session_url_key(&base).contains("moq-example--video"),
            "fragment の値をキーに含めないこと (c4m の認可トークンをログに出さない)"
        );
        let other_fragment =
            tokio_moq::parse_url("moqt://relay.example:443/live#msf:moq-example--audio")
                .expect("別 fragment の URL をパースできること");
        assert_ne!(
            side_session_url_key(&base),
            side_session_url_key(&other_fragment),
            "fragment が異なる URL は別の side session にすること"
        );
        let other_path =
            tokio_moq::parse_url("moqt://relay.example:443/other#msf:moq-example--video")
                .expect("別 path の URL をパースできること");
        assert_ne!(
            side_session_url_key(&base),
            side_session_url_key(&other_path),
            "path が違えば別のキーになること"
        );
        let other_authority =
            tokio_moq::parse_url("moqt://relay.example:444/live#msf:moq-example--video")
                .expect("別ポートの URL をパースできること");
        assert_ne!(
            side_session_url_key(&base),
            side_session_url_key(&other_authority),
            "authority が違えば別のキーになること"
        );
        let other_fragment =
            tokio_moq::parse_url("moqt://relay.example:443/live#msf:moq-example--audio")
                .expect("別 fragment の URL をパースできること");
        assert_ne!(
            side_session_url_key(&base),
            side_session_url_key(&other_fragment),
            "fragment が違えば別のキーになること"
        );
        let no_fragment = tokio_moq::parse_url("moqt://relay.example:443/live")
            .expect("fragment 無しの URL をパースできること");
        assert_eq!(
            side_session_url_key(&no_fragment),
            "moqt://relay.example:443/live",
            "fragment が無いときは path までをキーにすること"
        );
    }

    /// PUBLISH_DONE の status code から disposition を判定すること
    ///
    /// draft-ietf-moq-transport-23 §12.4 (Publish Done Codes)。`TRACK_ENDED` (0x2) は
    /// 正常終端、`GOING_AWAY` (0x4) は移行に伴う終端、それ以外はエラー終結である。
    /// 未知 code は §13 (Grease) の MUST に従い INTERNAL_ERROR に正規化されるため、
    /// 分類はエラーとなる (UNKNOWN code 単体の分類の上限は session library が担う)。
    #[test]
    fn publish_done_disposition_matches_the_status_code() {
        assert_eq!(
            publish_done_disposition(PUBLISH_DONE_TRACK_ENDED),
            PublishDoneDisposition::TrackEnded,
            "TRACK_ENDED は正常終端として扱うこと"
        );
        assert_eq!(
            publish_done_disposition(PUBLISH_DONE_GOING_AWAY),
            PublishDoneDisposition::GoingAway,
            "GOING_AWAY は移行待ちとして扱うこと"
        );
        // その他の既知 code (INTERNAL_ERROR 0x0 / UNAUTHORIZED 0x1 / TOO_FAR_BEHIND 0x5 /
        // EXPIRED 0x6 / EXPIRED_AUTH_TOKEN 0x7 / UPDATE_FAILED 0x8 / EXCESSIVE_LOAD 0x9 /
        // MALFORMED_TRACK 0x12) はエラーとして扱う
        for code in [0x0, 0x1, 0x5, 0x6, 0x7, 0x8, 0x9, 0x12] {
            assert_eq!(
                publish_done_disposition(code),
                PublishDoneDisposition::Error,
                "エラーの status code をエラー disposition にすること: code={code:#}"
            );
        }
        // 未知 code は §13 (Grease) MUST で INTERNAL_ERROR (0x0) に正規化される。
        // 呼び出し側は normalize_publish_done_code を先に通すため、分類はエラーになる
        assert_eq!(
            publish_done_disposition(normalize_publish_done_code(0x18)),
            PublishDoneDisposition::Error,
            "未知 code は INTERNAL_ERROR に正規化してエラー disposition にすること"
        );
    }

    /// 空 URI の request stream GOAWAY だけ現在の session で再発行すること
    ///
    /// `migration_target` は空 URI のとき現在の URL をそのまま返す。URL が同じなら同じ
    /// session で再発行し、違えば専用タスクが確立した side session へ移行する。
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
    /// (draft-ietf-moq-transport-23 §2.4.1 (Track Naming))、catalog track の namespace を使うと
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

    // TIMESTAMP が無ければ種類も無い
    #[test]
    fn timestamp_kind_of_returns_none_without_a_timestamp() {
        assert_eq!(
            timestamp_kind_of(None, None),
            TimestampKind::None,
            "TIMESTAMP が無ければ種類は無い"
        );
        assert_eq!(
            timestamp_kind_of(None, Some(48_000)),
            TimestampKind::None,
            "TIMESTAMP が無ければ TIMESCALE があっても種類は無い"
        );
    }

    // TIMESCALE が無い TIMESTAMP は壁時計の時刻である
    #[test]
    fn timestamp_kind_of_returns_wall_clock_without_a_timescale() {
        assert_eq!(
            timestamp_kind_of(Some(1_700_000_000_000_000), None),
            TimestampKind::WallClock,
            "TIMESCALE が無ければ TIMESTAMP は Unix epoch からのマイクロ秒である"
        );
    }

    // TIMESCALE がある TIMESTAMP は publisher のメディア時刻である
    #[test]
    fn timestamp_kind_of_returns_media_time_with_a_timescale() {
        assert_eq!(
            timestamp_kind_of(Some(48_000), Some(48_000)),
            TimestampKind::MediaTime,
            "TIMESCALE があれば TIMESTAMP はメディア時刻である"
        );
        assert_eq!(
            timestamp_kind_of(Some(100), Some(0)),
            TimestampKind::MediaTime,
            "TIMESCALE が 0 でも TIMESCALE があるものとしてメディア時刻にする"
        );
    }

    // timescale が None のときは Timestamp を Unix epoch からのマイクロ秒として扱う
    // (draft-ietf-moq-loc-05 §2.3.1.1)
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
    fn receive_input_lag_us_measures_the_wall_clock_timestamp() {
        assert_eq!(
            receive_input_lag_us(Some(1_000_000), None, 1_250_000, 1_000_000),
            Some(250_000),
            "到着の壁時計から TIMESTAMP を引いた値をマイクロ秒で返す"
        );
        // Timescale が 0 のときも Timescale が無いものとして扱う (loc_timestamp_us と同じ規則)
        assert_eq!(
            receive_input_lag_us(Some(1_000_000), Some(0), 1_250_000, 1_000_000),
            Some(250_000),
            "Timescale が 0 なら壁時計として測る"
        );
        // 到着が TIMESTAMP より前の音 (配信側の時計が先へ進んでいる) は負になる
        assert_eq!(
            receive_input_lag_us(Some(2_000_000), None, 1_000_000, 2_000_000),
            Some(-1_000_000),
            "到着のほうが早いときは負の遅れになる"
        );
    }

    // TIMESTAMP が無い音と、Timescale がある (メディア時刻である) 音では測れない
    #[test]
    fn receive_input_lag_us_returns_none_when_the_timestamp_is_not_a_wall_clock() {
        assert_eq!(
            receive_input_lag_us(None, None, 1_250_000, 0),
            None,
            "TIMESTAMP が無い音では測れない"
        );
        assert_eq!(
            receive_input_lag_us(None, Some(48_000), 1_250_000, 0),
            None,
            "TIMESTAMP が無い音では Timescale があっても測れない"
        );
        assert_eq!(
            receive_input_lag_us(Some(48_000), Some(48_000), 1_250_000, 1_000_000),
            None,
            "Timescale がある音はメディア時刻であり壁時計と対応しないため測れない"
        );
    }

    /// テスト用: OpusHead を構築する (RFC 7845 §5.1 のフォーマット)
    ///
    /// publisher 側 (crates/moq-pub の `build_opus_head`) と同じ形式。
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
    /// draft-ietf-moq-loc-05 §2.2 (MOQ Object Mapping) は LOC の Public Properties を
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

    /// LOC の書式違反 (Video Frame Marking の宣言長 5) は MalformedKeyValue
    ///
    /// draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) は、理解している型の
    /// Length/Value が定義と一致しない場合に MALFORMED_KEY_VALUE (0x6) で
    /// セッションを閉じる MUST を定める。Video Frame Marking の Length は 1-4 バイト
    /// (draft-ietf-moq-loc-05 §2.3.2.2 (Video Frame Marking)) である。
    /// 書式違反をプロパティ無しに潰さないことを固定する。
    #[test]
    fn extract_video_config_reports_key_value_formatting_error() {
        // LocProperties::encode は違反値を拒否するため、生バイト列を組み立てる
        // (Properties Length = 7 = delta 1 バイト + Length 1 バイト + 値 5 バイト)
        let mut bytes = vec![0x07, 0x09, 0x05];
        bytes.extend_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05]);
        let err = extract_video_config(Some(&bytes)).expect_err("書式違反はエラーになること");
        assert!(
            matches!(err, MessageError::MalformedKeyValue(_)),
            "Video Frame Marking の宣言長違反は MalformedKeyValue であること: {err:?}"
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

    /// LOC の書式違反 (Audio Level 256) は MalformedKeyValue
    ///
    /// Audio Level は 8 bit 範囲 (draft-ietf-moq-loc-05 §2.3.3.2 (Audio Level)) のため、256 は
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
            matches!(err, MessageError::MalformedKeyValue(_)),
            "Audio Level の値域違反は MalformedKeyValue であること: {err:?}"
        );
        let err =
            extract_timestamp_timescale(Some(&bytes)).expect_err("書式違反はエラーになること");
        assert!(
            matches!(err, MessageError::MalformedKeyValue(_)),
            "Timestamp / Timescale の抽出でも同じエラーになること: {err:?}"
        );
    }

    /// Timestamp / Timescale の抽出: 最上位リストに値がある場合
    ///
    /// accessor への置き換えで、最上位リストの値と戻り値の順序 (Timestamp, Timescale) の
    /// 配線が壊れていないことを固定する。
    #[test]
    fn extract_timestamp_timescale_reads_top_level() {
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(7_200_000),
        });
        props.push(LocProperty {
            prop_id: PROP_TIMESCALE,
            value: LocPropertyValue::VarInt(90_000),
        });
        let encoded = props.encode().expect("encode に成功すること");
        assert_eq!(
            extract_timestamp_timescale(Some(&encoded)),
            Ok((Some(7_200_000), Some(90_000))),
            "最上位リストの Timestamp / Timescale を取得できること"
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

    /// テスト用: IMMUTABLE_PROPERTIES (0x0B) の内側だけに値を持つ properties バイト列を
    /// 組み立てる
    ///
    /// IMMUTABLE_PROPERTIES の値は外側の Properties Length を伴わない delta-key KVP 列で
    /// ある (draft-ietf-moq-transport-23 §10.7 (Immutable Properties) / §8.3
    /// (Key-Value-Pair Structure))。内側のプロパティ列は `LocProperties::encode` で正規の
    /// KVP 表現を作り、先頭の Properties Length だけを読み飛ばして 0x0B の値に格納する。
    /// 外側の mutable リストには 0x0B だけを置く。
    fn immutable_only_properties_bytes(inner: &LocProperties) -> Vec<u8> {
        use shiguredo_moqt::track_properties::PROP_IMMUTABLE_PROPERTIES;

        let encoded_inner = inner
            .encode()
            .expect("テストフィクスチャの内側 KVP の encode は成功する");
        let (_, length_field_len) = shiguredo_moqt::varint::decode(&encoded_inner)
            .expect("Properties Length の varint を読めること");

        let mut outer = LocProperties::new();
        outer.push(LocProperty {
            prop_id: PROP_IMMUTABLE_PROPERTIES,
            value: LocPropertyValue::Bytes(encoded_inner[length_field_len..].to_vec()),
        });
        outer
            .encode()
            .expect("テストフィクスチャの外側 KVP の encode は成功する")
    }

    /// IMMUTABLE_PROPERTIES (0x0B) の内側に任意のバイト列を持つ properties を組み立てる
    ///
    /// `LocProperties::encode` はローカル入力の検証として 0x0B の内側の構造・値域を検証する
    /// ため、破損した内側を意図的に持つフィクスチャは wire 形式を直接組み立てる。
    /// `video_config` を渡すと最上位リストに Video Config (0x0D) も置く。
    fn properties_with_inner_immutable(inner: &[u8], video_config: Option<&[u8]>) -> Vec<u8> {
        use shiguredo_moqt::track_properties::PROP_IMMUTABLE_PROPERTIES;

        let mut kvp = Vec::new();
        shiguredo_moqt::varint::encode(PROP_IMMUTABLE_PROPERTIES, &mut kvp);
        shiguredo_moqt::varint::encode(inner.len() as u64, &mut kvp);
        kvp.extend_from_slice(inner);
        if let Some(video_config) = video_config {
            shiguredo_moqt::varint::encode(PROP_VIDEO_CONFIG - PROP_IMMUTABLE_PROPERTIES, &mut kvp);
            shiguredo_moqt::varint::encode(video_config.len() as u64, &mut kvp);
            kvp.extend_from_slice(video_config);
        }

        let mut wire = Vec::new();
        shiguredo_moqt::varint::encode(kvp.len() as u64, &mut wire);
        wire.extend_from_slice(&kvp);
        wire
    }

    /// IMMUTABLE_PROPERTIES の内側だけにある Video Config を取り出せること
    ///
    /// IMMUTABLE_PROPERTIES (0x0B) の内側だけに値がある Object を固定する。
    #[test]
    fn extract_video_config_reads_immutable_properties() {
        let config = vec![0x01, 0x64, 0x00, 0x1F];
        let mut inner = LocProperties::new();
        inner.push(LocProperty {
            prop_id: PROP_VIDEO_CONFIG,
            value: LocPropertyValue::Bytes(config.clone()),
        });
        let bytes = immutable_only_properties_bytes(&inner);
        assert_eq!(
            extract_video_config(Some(&bytes)),
            Ok(Some(config)),
            "IMMUTABLE_PROPERTIES の内側の Video Config を取り出せること"
        );
    }

    /// IMMUTABLE_PROPERTIES の内側だけにある Timestamp / Timescale を取り出せること
    ///
    /// display time を内側だけに置く publisher の Object を固定する。
    #[test]
    fn extract_timestamp_timescale_reads_immutable_properties() {
        let mut inner = LocProperties::new();
        inner.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(7_200_000),
        });
        inner.push(LocProperty {
            prop_id: PROP_TIMESCALE,
            value: LocPropertyValue::VarInt(90_000),
        });
        let bytes = immutable_only_properties_bytes(&inner);
        assert_eq!(
            extract_timestamp_timescale(Some(&bytes)),
            Ok((Some(7_200_000), Some(90_000))),
            "IMMUTABLE_PROPERTIES の内側の Timestamp / Timescale を取り出せること"
        );
    }

    /// IMMUTABLE_PROPERTIES の内側だけにある Audio Config を取り出せること
    ///
    /// Audio Config を内側だけに置く publisher の Object を固定する。
    #[test]
    fn extract_audio_config_reads_immutable_properties() {
        let head = build_test_opus_head(48_000, 1);
        let mut inner = LocProperties::new();
        inner.push(LocProperty {
            prop_id: PROP_AUDIO_CONFIG,
            value: LocPropertyValue::Bytes(head.clone()),
        });
        let bytes = immutable_only_properties_bytes(&inner);
        assert_eq!(
            extract_audio_config(Some(&bytes)),
            Ok(Some(head)),
            "IMMUTABLE_PROPERTIES の内側の Audio Config を取り出せること"
        );
    }

    /// IMMUTABLE_PROPERTIES の内側だけにある Video Frame Marking でキーフレーム判定できること
    ///
    /// Video Frame Marking を内側だけに置く publisher の Object を固定する。
    #[test]
    fn extract_video_keyframe_reads_immutable_properties() {
        for (marking, expected) in [(0x20u8, true), (0xC0, false)] {
            let mut inner = LocProperties::new();
            inner.push(LocProperty {
                prop_id: PROP_VIDEO_FRAME_MARKING,
                value: LocPropertyValue::Bytes(vec![marking]),
            });
            let bytes = immutable_only_properties_bytes(&inner);
            assert_eq!(
                extract_video_keyframe(Some(&bytes)),
                Ok(Some(expected)),
                "IMMUTABLE_PROPERTIES の内側の Video Frame Marking (marking={marking:#04x}) から I ビットを判定できること"
            );
        }
    }

    /// IMMUTABLE_PROPERTIES の内側だけにある LOC 固有の値域違反は Ok(None) になる
    ///
    /// 内側だけにある LOC 固有の検証 (Audio Level の 8 bit 超過など) は `LocProperties` の
    /// getter が `None` に写すため、抽出関数は「プロパティ無し」として `Ok(None)` を返す。
    /// この既知の制約 (docs/missing-features.md §4.1) を固定し、将来のエラー分類対応で
    /// 挙動が変わったときに気づけるようにする。
    #[test]
    fn malformed_inner_loc_property_is_reported_as_absent() {
        let mut inner_bytes = Vec::new();
        shiguredo_moqt::varint::encode(PROP_AUDIO_LEVEL, &mut inner_bytes);
        shiguredo_moqt::varint::encode(256, &mut inner_bytes); // 0-255 の範囲外
        let bytes = properties_with_inner_immutable(&inner_bytes, None);

        // 内側の走査が値域違反で失敗するため、どの抽出関数も「値が無い」として扱う。
        assert_eq!(
            extract_timestamp_timescale(Some(&bytes)),
            Ok((None, None)),
            "内側の値域違反は Timestamp / Timescale 無しとして扱われること"
        );
        assert_eq!(
            extract_video_config(Some(&bytes)),
            Ok(None),
            "内側の値域違反は Video Config 無しとして扱われること"
        );
        assert_eq!(
            extract_video_keyframe(Some(&bytes)),
            Ok(None),
            "内側の値域違反は Video Frame Marking 無しとして扱われること"
        );
        assert_eq!(
            extract_audio_config(Some(&bytes)),
            Ok(None),
            "内側の値域違反は Audio Config 無しとして扱われること"
        );
    }

    /// 内側の Video Frame Marking 長さ違反はキーフレーム代用に落ちる
    ///
    /// 内側だけの Video Frame Marking が 1-4 バイトの長さ違反を持つ場合、getter が `None` に
    /// 写すため `extract_video_keyframe` は `Ok(None)` を返し、`is_video_keyframe` は Video
    /// Config の有無でキーフレームを代用する。この既知の制約 (docs/missing-features.md §4.1)
    /// がアプリ可視の挙動に与える影響を固定する。
    #[test]
    fn malformed_inner_video_frame_marking_falls_back_to_video_config() {
        let mut inner_bytes = Vec::new();
        shiguredo_moqt::varint::encode(PROP_VIDEO_FRAME_MARKING, &mut inner_bytes);
        shiguredo_moqt::varint::encode(5, &mut inner_bytes); // 1-4 バイトの範囲外
        inner_bytes.extend_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05]);

        // 最上位の Video Config は decode も取得もできる (内側の破損とは独立)。
        let bytes = properties_with_inner_immutable(&inner_bytes, Some(&[0xAA]));

        let video_config =
            extract_video_config(Some(&bytes)).expect("Video Config は取得できること");
        assert_eq!(video_config, Some(vec![0xAA]));
        assert_eq!(
            extract_video_keyframe(Some(&bytes)),
            Ok(None),
            "内側の Video Frame Marking 長さ違反は判定不能として扱われること"
        );
        assert!(
            is_video_keyframe(Some(&bytes), video_config.as_deref())
                .expect("Video Config があるため判定は成功すること"),
            "Video Frame Marking を取得できない場合は Video Config の有無で代用されること"
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

    /// shutdown 時の STOP_SENDING 対象が active な購読に絞られること
    ///
    /// PUBLISH_DONE を受けた購読 (`active_requests` から外れた request id) と、request id
    /// が残っていても active でない購読は対象外にする (library (`shiguredo_moqt`) の
    /// `Session` は Terminated を拒否するため)。active な購読は送信対象に含める。
    #[test]
    fn stop_sending_targets_follow_active_requests() {
        let mut active_requests = HashSet::new();
        active_requests.insert(3);
        active_requests.insert(5);

        // active な request id (audio 5 / catalog 3) だけが対象になる。video 2 は active
        // から外れている (PUBLISH_DONE を受けた購読)
        let targets = stop_sending_targets(Some(2), Some(5), Some(3), &active_requests);
        assert_eq!(
            targets,
            vec![(TrackKind::Audio, 5), (TrackKind::Catalog, 3)],
            "active な購読だけを送信対象とすること"
        );

        // 全部の request id が active でなければ対象は空である (送らない)
        let empty: HashSet<u64> = HashSet::new();
        assert!(
            stop_sending_targets(Some(2), Some(5), Some(3), &empty).is_empty(),
            "active でない購読は対象外とすること"
        );

        // video / audio を購読していない場合 (request id が None) は対象に含まれない
        let targets = stop_sending_targets(None, None, Some(3), &active_requests);
        assert_eq!(
            targets,
            vec![(TrackKind::Catalog, 3)],
            "購読していない track は対象外とすること"
        );
    }

    /// 全 media track の終了判定が購読の有無・移行待ち・catalog を正しく扱うこと
    ///
    /// 移行済み track の PUBLISH_DONE と main ループの tick の両方から使う判定である。
    /// media の移行待ちがある間は終えない。catalog の恒久終了と移行待ちは media の判定に
    /// 影響しない。購読していない track は終了済みとして扱う。
    #[test]
    fn all_media_tracks_ended_requires_both_media_tracks_without_migration() {
        let none: HashSet<TrackKind> = HashSet::new();

        // 両方を購読していて両方が恒久終了したら終える
        let mut ended = HashSet::new();
        ended.insert(TrackKind::Video);
        ended.insert(TrackKind::Audio);
        assert!(
            all_media_tracks_ended(true, true, &ended, &none),
            "両方の media track が恒久終了したら終えること"
        );

        // 片方だけでは終えない
        let mut video_only = HashSet::new();
        video_only.insert(TrackKind::Video);
        assert!(
            !all_media_tracks_ended(true, true, &video_only, &none),
            "片方の media track が残っている間は終えないこと"
        );

        // media の移行待ちがある間は終えない
        let mut pending = HashSet::new();
        pending.insert(TrackKind::Audio);
        assert!(
            !all_media_tracks_ended(true, true, &ended, &pending),
            "media の移行待ちがある間は終えないこと"
        );

        // catalog の恒久終了と移行待ちは media の判定に影響しない
        let mut catalog = HashSet::new();
        catalog.insert(TrackKind::Catalog);
        assert!(
            all_media_tracks_ended(true, true, &ended, &catalog),
            "catalog の状態は media の終了判定に影響しないこと"
        );

        // 購読していない track は終了済みとして扱う (音声だけの購読)
        let mut audio_only = HashSet::new();
        audio_only.insert(TrackKind::Audio);
        assert!(
            all_media_tracks_ended(false, true, &audio_only, &none),
            "購読していない映像を待たずに終えること"
        );
    }

    /// 移行開始時の plan 組み立てと恒久終了化の対象が disposition を参照すること
    ///
    /// 終了待ち entry の `TrackEnded` / `Error` は plan から除外し (`permanently_ended_tracks`
    /// へ移す対象として返す)、`GoingAway` の entry は plan に含める。
    #[test]
    fn plan_for_migration_follows_pending_dispositions() {
        let base = ResubscribePlan {
            catalog_namespace: Some(
                TrackNamespace::new(vec![b"live".to_vec()]).expect("名前空間を作れること"),
            ),
            video: Some(SubscribeTarget {
                namespace: TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("名前空間を作れること"),
                track_name: b"video".to_vec(),
            }),
            audio: None,
        };

        // 終了待ちが無ければ base の plan がそのまま使われる
        let empty: HashSet<TrackKind> = HashSet::new();
        let pending: HashMap<u64, (TrackKind, PublishDoneDisposition)> = HashMap::new();
        let (plan, resolve) = plan_for_migration(&base, &empty, &pending);
        assert!(plan.catalog_namespace.is_some(), "catalog を含めること");
        assert!(plan.video.is_some(), "video を含めること");
        assert!(plan.audio.is_none(), "購読していない audio は含めないこと");
        assert!(resolve.is_empty(), "解消対象が無いこと");

        // video の PUBLISH_DONE(TRACK_ENDED) が終了待ちのまま移行が始まった場合:
        // plan から video を外し、恒久終了化の対象として返す
        let mut pending = HashMap::new();
        pending.insert(2, (TrackKind::Video, PublishDoneDisposition::TrackEnded));
        let (plan, resolve) = plan_for_migration(&base, &empty, &pending);
        assert!(
            plan.video.is_none(),
            "恒久終了予定の video を plan から外すこと"
        );
        assert!(plan.catalog_namespace.is_some(), "catalog は維持すること");
        assert_eq!(
            resolve,
            vec![(2, TrackKind::Video)],
            "恒久終了化して entry を解消する対象として返すこと"
        );

        // PUBLISH_DONE(GOING_AWAY) は plan に含める (移行先で張り直す)。base に track が
        // ある GoingAway ケースで「含める」を固定する (外して解消もしない regress を
        // 検出できるようにする)
        let mut pending = HashMap::new();
        pending.insert(5, (TrackKind::Video, PublishDoneDisposition::GoingAway));
        let (plan, resolve) = plan_for_migration(&base, &empty, &pending);
        assert!(
            plan.video.is_some(),
            "移行待ち (GoingAway) の track は plan に含めること"
        );
        assert!(resolve.is_empty(), "GoingAway は恒久終了化しないこと");

        // PUBLISH_DONE の Error disposition は TrackEnded と同じ恒久終了扱いであること
        let mut pending = HashMap::new();
        pending.insert(7, (TrackKind::Video, PublishDoneDisposition::Error));
        let (plan, resolve) = plan_for_migration(&base, &empty, &pending);
        assert!(
            plan.video.is_none(),
            "エラー status の track を plan から外すこと"
        );
        assert_eq!(
            resolve,
            vec![(7, TrackKind::Video)],
            "エラー status の track を恒久終了化する対象として返すこと"
        );

        // cleanup 完了済みの恒久終了 track は plan から外れる (entry は解消済み)
        let mut ended: HashSet<TrackKind> = HashSet::new();
        ended.insert(TrackKind::Video);
        let pending: HashMap<u64, (TrackKind, PublishDoneDisposition)> = HashMap::new();
        let (plan, resolve) = plan_for_migration(&base, &ended, &pending);
        assert!(
            plan.video.is_none(),
            "恒久終了済みの video を plan から外すこと"
        );
        assert!(
            resolve.is_empty(),
            "既に恒久終了済みの entry は解消対象外とすること"
        );

        // SUBSCRIBE_TRACKS 由来の購読 (Discard) は移行先で張り直さないため、plan にも
        // 恒久終了化の対象にも載せない (媒体トラックの終了判定に混ざらないこと)
        let mut pending = HashMap::new();
        pending.insert(9, (TrackKind::Discard, PublishDoneDisposition::TrackEnded));
        let (plan, resolve) = plan_for_migration(&base, &empty, &pending);
        assert!(plan.catalog_namespace.is_some(), "catalog は維持すること");
        assert!(plan.video.is_some(), "video は維持すること");
        assert!(
            resolve.is_empty(),
            "PUBLISH 起点の購読は恒久終了化しないこと"
        );
    }

    /// 終端済み購読の request stream GOAWAY の再発行判定が disposition と状態を参照すること
    ///
    /// tick の entry 解消 (恒久終了 / 移行待ち) と後着 GOAWAY の順序で
    /// `publish_done_pending` が空になっても判定が崩れないこと。
    #[test]
    fn reissue_request_target_and_send_stop_sending_follow_states() {
        let empty: HashSet<TrackKind> = HashSet::new();

        // 終了待ち entry なし (Established の通常再発行): 再発行して STOP_SENDING を送る
        assert_eq!(
            reissue_request_target(TrackKind::Video, None, &empty),
            Some(TrackKind::Video),
            "active な購読は再発行すること"
        );
        assert!(
            reissue_send_stop_sending(None, &empty, &empty, TrackKind::Video),
            "Established の通常再発行では STOP_SENDING を送ること"
        );

        // 終了待ち entry が TrackEnded / Error: 再発行しない (STOP_SENDING も送らない)
        assert_eq!(
            reissue_request_target(
                TrackKind::Video,
                Some(PublishDoneDisposition::TrackEnded),
                &empty
            ),
            None,
            "恒久終了予定の購読は再発行しないこと"
        );
        assert!(
            !reissue_send_stop_sending(
                Some(PublishDoneDisposition::TrackEnded),
                &empty,
                &empty,
                TrackKind::Video
            ),
            "Terminated 済みの購読へ STOP_SENDING を送らないこと"
        );

        // 終了待ち entry が GoingAway: 再発行するが STOP_SENDING は送らない
        assert_eq!(
            reissue_request_target(
                TrackKind::Audio,
                Some(PublishDoneDisposition::GoingAway),
                &empty
            ),
            Some(TrackKind::Audio),
            "移行待ちの購読は同じ session で再発行すること"
        );
        assert!(
            !reissue_send_stop_sending(
                Some(PublishDoneDisposition::GoingAway),
                &empty,
                &empty,
                TrackKind::Audio
            ),
            "Terminated 済みの購読へ STOP_SENDING を送らないこと"
        );

        // tick 解消済みの移行待ち (entry 無し、migration_pending_tracks に載っている):
        // 再発行するが STOP_SENDING は送らない (Terminated 済み)
        let mut migration_pending: HashSet<TrackKind> = HashSet::new();
        migration_pending.insert(TrackKind::Audio);
        assert_eq!(
            reissue_request_target(TrackKind::Audio, None, &empty),
            Some(TrackKind::Audio),
            "移行待ち解消済みの購読も再発行すること"
        );
        assert!(
            !reissue_send_stop_sending(None, &empty, &migration_pending, TrackKind::Audio),
            "tick 解消済みの Terminated 済み購読へ STOP_SENDING を送らないこと"
        );

        // catalog の GoingAway (tick 解消済み) も移行待ち集合で保護される。Terminated
        // 済みのため STOP_SENDING は送らない (media と同一の規則)
        let mut catalog_pending: HashSet<TrackKind> = HashSet::new();
        catalog_pending.insert(TrackKind::Catalog);
        assert_eq!(
            reissue_request_target(TrackKind::Catalog, None, &empty),
            Some(TrackKind::Catalog),
            "tick 解消済みの catalog 移行待ちも再発行すること"
        );
        assert!(
            !reissue_send_stop_sending(None, &empty, &catalog_pending, TrackKind::Catalog),
            "tick 解消済みの Terminated 済み catalog へ STOP_SENDING を送らないこと"
        );

        // 恒久終了済みの track は再発行しない
        let mut ended: HashSet<TrackKind> = HashSet::new();
        ended.insert(TrackKind::Video);
        assert_eq!(
            reissue_request_target(TrackKind::Video, None, &ended),
            None,
            "恒久終了済みの track を再発行しないこと"
        );
        assert!(
            !reissue_send_stop_sending(None, &ended, &empty, TrackKind::Video),
            "恒久終了済みの購読へ STOP_SENDING を送らないこと"
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

        // 書式違反は MALFORMED_KEY_VALUE (0x6) として送られる
        let error = MessageError::MalformedKeyValue("LOC property value is out of range");
        request_session_termination(&termination_tx, &error);

        // 容量 1 のため、依頼が積まれている間の 2 件目は届かない (panic もしない)
        request_session_termination(&termination_tx, &MessageError::UnexpectedEof);

        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), termination_rx.recv())
                .await
                .expect("終了依頼が送られること"),
            Some(SessionError::new(
                SESSION_MALFORMED_KEY_VALUE,
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
    /// draft-ietf-moq-transport-23 §3.5 (Joining an Ongoing Track): 購読の Location Filter は
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
    /// draft-ietf-moq-transport-23 §9.19.9 Table 6 の Type 0x04 は Start を絶対 Location、
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

    /// FETCH 応答 stream を映像 / カタログへ振り分けること
    ///
    /// 音声のメディア FETCH は発行しないため、対応表の値 (`FetchTarget`) に音声は無い。
    /// catalog 購読の fill fetch stream はカタログとして適用する (移行後に catalog を張り
    /// 直すと、FILL_PARAMETERS 付き SUBSCRIBE への fill fetch stream で完全な catalog が
    /// 届く。読み捨てると移行中に更新された catalog が反映されない。
    /// draft-ietf-moq-transport-23 §3.4 (Fill Semantics)。draft 由来の仕様であり将来変更
    /// されうる)。
    #[test]
    fn fetch_stream_action_routes_media_fetches() {
        let mut fetch_targets = HashMap::new();
        fetch_targets.insert(10, FetchTarget::Video);
        fetch_targets.insert(11, FetchTarget::Catalog);
        assert_eq!(
            fetch_stream_action(10, &fetch_targets),
            FetchStreamAction::DeliverVideo,
            "映像の FETCH 応答は映像の配送経路へ流すこと"
        );
        assert_eq!(
            fetch_stream_action(11, &fetch_targets),
            FetchStreamAction::DeliverCatalog,
            "catalog 購読の fill fetch 応答はカタログへ流すこと"
        );
    }

    /// 対応表に無い Request ID は配送先なしとして扱うこと
    ///
    /// `fetch_targets` は自側が発行した要求 (映像 FETCH と catalog 購読) の Request ID だけを
    /// 持つ。PUBLISH 起点の初回 fill は peer の PUBLISH の Request ID を載せるため
    /// (draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams))、表に
    /// 現れない。`Ignore` は「このアプリに配送先が無い」ことだけを意味し、Session への登録
    /// (FETCH_HEADER の受理と stream の会計) は [`handle_stream_body`] が本判定より先に
    /// [`DataPlaneHandle::recv_fetch_header`] で行う。この順序により、表に無いことだけを理由に
    /// PUBLISH 起点の fill を Session へ届けずに失うことはない。
    #[test]
    fn fetch_stream_action_ignores_unknown_request_ids() {
        let mut fetch_targets = HashMap::new();
        fetch_targets.insert(10, FetchTarget::Video);
        fetch_targets.insert(11, FetchTarget::Catalog);
        assert_eq!(
            fetch_stream_action(0, &fetch_targets),
            FetchStreamAction::Ignore,
            "対応表に無い Request ID の stream は読み捨てること"
        );
    }

    /// 保留待ちの間は同時処理枠を手放し、待機後に取り直すこと
    ///
    /// fill の保留は PUBLISH の到着を最長 5 秒待つ。その間も枠を保持すると、保留が
    /// `MAX_CONCURRENT_STREAMS` 本そろったときに他の stream の処理が止まる。
    /// 手放している間はセマフォの空きが増え、取り直すと元に戻ることを固定する。
    #[tokio::test]
    async fn stream_slot_releases_and_reacquires_permit() {
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let permit = std::sync::Arc::clone(&semaphore)
            .acquire_owned()
            .await
            .expect("テストフィクスチャの前提条件を満たす");
        let mut slot = StreamSlot::new(std::sync::Arc::clone(&semaphore), permit);
        assert_eq!(semaphore.available_permits(), 0, "枠を保持していること");
        slot.release();
        assert_eq!(semaphore.available_permits(), 1, "待機の前に枠を手放すこと");
        slot.reacquire().await;
        assert_eq!(
            semaphore.available_permits(),
            0,
            "待機の後に枠を取り直すこと"
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
    /// (draft-ietf-moq-loc-05 §4.1)。
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
    /// (draft-ietf-moq-loc-05 §2.3.1.1)。受信の入口の入力の遅れを測れる音はこの形である。
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
    /// `AudioDelivery` より長く保持する。`session_generation` はこの配送経路が読む session の
    /// 世代であり、`audio_config_validated_generation` と比べて Audio Config の処理可否を決める。
    fn test_audio_delivery<'a>(
        audio_tx: &'a std::sync::mpsc::Sender<DecodedAudioFrame>,
        audio_config_validated_generation: &'a std::sync::atomic::AtomicU64,
        session_generation: u64,
        termination_tx: &'a tokio::sync::mpsc::Sender<SessionError>,
        catch_up: &'a std::sync::Mutex<AudioReceiveCatchUp>,
        delivery_order: &'a std::sync::Mutex<AudioDeliveryOrder>,
        recorder: Option<&'a RecorderSender>,
    ) -> AudioDelivery<'a> {
        AudioDelivery {
            audio_tx,
            audio_config_validated_generation,
            session_generation,
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
    /// (draft-ietf-moq-loc-05 §4.1)。購読側は payload を decode してデコード済みフレームを
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
        let audio_config_validated_generation = std::sync::atomic::AtomicU64::new(0);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_validated_generation,
            1,
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
        assert_eq!(
            audio_config_validated_generation.load(Ordering::Acquire),
            1,
            "Audio Config を検証済みとして記録すること"
        );
        assert!(
            termination_rx.try_recv().is_err(),
            "書式違反が無いためセッション終了は依頼されないこと"
        );
    }

    /// Object Status のみの Object と未知の alias はメディアとして扱わないこと
    ///
    /// Object Status のみの Object は payload を持たず (draft-ietf-moq-transport-23 §11.2.1)、
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
    /// (draft-ietf-moq-loc-05 §4.2)。moq-sub は datagram の映像 / カタログを扱わない。
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

    /// data stream の復号エラー分類: 終了対象のみ依頼されること
    #[tokio::test]
    async fn decode_error_requests_session_termination_only_for_terminating_errors() {
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel::<SessionError>(1);
        // 対象外: UnexpectedEof と MalformedTrack は依頼しない
        request_session_termination_for_decode_error(&termination_tx, &MessageError::UnexpectedEof);
        request_session_termination_for_decode_error(
            &termination_tx,
            &MessageError::MalformedTrack("malformed track"),
        );
        assert!(
            termination_rx.try_recv().is_err(),
            "対象外の復号エラーではセッション終了を依頼しないこと"
        );

        // 対象: ProtocolViolation は PROTOCOL_VIOLATION (0x3) で依頼する
        request_session_termination_for_decode_error(
            &termination_tx,
            &MessageError::ProtocolViolation("bad subgroup header"),
        );
        let termination = termination_rx
            .try_recv()
            .expect("セッション終了が依頼されること");
        assert_eq!(
            termination.code,
            shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION,
            "PROTOCOL_VIOLATION の終了コードであること"
        );
    }

    /// 途中終了の FIN だけを mid-object FIN として報告すること
    #[test]
    fn mid_object_fin_reports_only_fin_with_unexpected_eof() {
        use shiguredo_moqt::error::MessageError;
        assert!(
            mid_object_fin_should_report(&RequestStreamEnd::Fin, &Err(MessageError::UnexpectedEof)),
            "FIN + UnexpectedEof は報告すること"
        );
        assert!(
            !mid_object_fin_should_report(&RequestStreamEnd::Fin, &Ok(())),
            "Object 境界の FIN は報告しないこと"
        );
        assert!(
            !mid_object_fin_should_report(
                &RequestStreamEnd::Reset {
                    error_code: Some(1),
                    reliable_size: None,
                },
                &Err(MessageError::UnexpectedEof)
            ),
            "RESET_STREAM は §11.3.2 の正常経路であり報告しないこと"
        );
    }

    /// peer request の拒否コード: PUBLISH だけ UNINTERESTED になり、それ以外は NOT_SUPPORTED
    #[test]
    fn request_rejection_uses_uninterested_only_for_publish() {
        use shiguredo_moqt::error::{REQUEST_NOT_SUPPORTED, REQUEST_UNINTERESTED};
        use shiguredo_moqt::message::{Publish, RequestOk};
        use shiguredo_moqt::track_properties::TrackProperties;
        let publish = ControlMessage::Publish(Publish {
            request_id: 1,
            track_namespace: TrackNamespace::new(Vec::new())
                .expect("空の namespace を作成できること"),
            track_name: b"cam".to_vec(),
            track_alias: 700,
            parameters: MessageParameters::new(),
            track_properties: TrackProperties::new(),
        });
        assert_eq!(
            request_rejection(&publish).0,
            REQUEST_UNINTERESTED,
            "PUBLISH は UNINTERESTED で拒否すること"
        );
        let other = ControlMessage::RequestOk(RequestOk {
            parameters: MessageParameters::new(),
            track_properties: TrackProperties::new(),
        });
        assert_eq!(
            request_rejection(&other).0,
            REQUEST_NOT_SUPPORTED,
            "PUBLISH 以外は NOT_SUPPORTED で拒否すること"
        );
    }

    /// LOC Properties の書式違反でセッション終了を依頼すること
    ///
    /// draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) は書式違反の受信を
    /// セッション終了の対象とする MUST を定める。datagram 経路も stream 経路と同じ扱いにする。
    #[tokio::test]
    async fn audio_delivery_requests_session_termination_on_malformed_loc_properties() {
        let (audio_tx, audio_rx) = std::sync::mpsc::channel();
        let audio_config_validated_generation = std::sync::atomic::AtomicU64::new(0);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_validated_generation,
            1,
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
            shiguredo_moqt::error::SESSION_MALFORMED_OBJECT,
            "Object の直列化を復号できないため MALFORMED_OBJECT であること"
        );
        assert!(
            audio_rx.try_recv().is_err(),
            "書式違反の Object はデコードしないこと"
        );
    }

    /// テスト用: 同じ LOC Property ID を繰り返す Properties の wire 表現を作る
    ///
    /// LOC Properties は `Properties Length (varint) | Key-Value-Pairs...` であり、同じ ID は
    /// delta=0 で表現する。単一に限られる既知 ID を繰り返すと malformed Track
    /// (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1 (Malformed
    /// Tracks) 条件 9)、未知 ID を繰り返すと正当な複数インスタンスになる。
    fn duplicate_loc_properties(prop_id: u64, values: &[u64]) -> Vec<u8> {
        let mut inner = Vec::new();
        shiguredo_moqt::varint::encode(prop_id, &mut inner);
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                shiguredo_moqt::varint::encode(0, &mut inner);
            }
            shiguredo_moqt::varint::encode(*value, &mut inner);
        }
        let mut block = Vec::new();
        shiguredo_moqt::varint::encode(inner.len() as u64, &mut block);
        block.extend_from_slice(&inner);
        block
    }

    /// 音声配送は既知 LOC Property の重複をセッション終了へ変換しないこと
    ///
    /// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1 (Malformed
    /// Tracks) 条件 9 はセッション終了ではなく購読・fetch の cancel を求める。Session の
    /// data plane が同じ Object Properties を decode して cancel 済みのため、ここでは
    /// セッション終了を依頼せず、この Object だけを捨てて配送を続ける。
    #[tokio::test]
    async fn audio_delivery_does_not_request_session_termination_on_malformed_track() {
        let (audio_tx, audio_rx) = std::sync::mpsc::channel();
        let audio_config_validated_generation = std::sync::atomic::AtomicU64::new(0);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_validated_generation,
            1,
            &termination_tx,
            &catch_up,
            &delivery_order,
            None,
        );

        // TIMESTAMP (0x10) を 2 インスタンス持つ Object Properties
        let properties = duplicate_loc_properties(0x10, &[1_000, 2_000]);
        let deliver = delivery
            .deliver(Some(loc(1, 0)), Some(&properties), b"payload", None)
            .await;

        assert!(
            deliver,
            "malformed Track はこの Object だけを捨てて配送を続けること"
        );
        assert!(
            termination_rx.try_recv().is_err(),
            "malformed Track でセッション終了を依頼しないこと"
        );
        assert!(
            audio_rx.try_recv().is_err(),
            "malformed Track の Object はデコードしないこと"
        );
    }

    /// 音声配送は未知 LOC Property の複数インスタンスを受理すること
    ///
    /// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) の "MUST allow multiple
    /// instances of the Property" / §16.8 (Properties) の "Endpoints MUST ignore unknown
    /// Property types"。
    #[tokio::test]
    async fn audio_delivery_accepts_unknown_duplicate_loc_properties() {
        let (audio_tx, audio_rx) = std::sync::mpsc::channel();
        let audio_config_validated_generation = std::sync::atomic::AtomicU64::new(0);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_validated_generation,
            1,
            &termination_tx,
            &catch_up,
            &delivery_order,
            None,
        );

        // 未知 ID (0x1000) を同値・異値で 2 インスタンス持つ Object Properties
        let properties = duplicate_loc_properties(0x1000, &[1, 2]);
        let deliver = delivery
            .deliver(Some(loc(1, 0)), Some(&properties), b"payload", None)
            .await;

        assert!(deliver, "未知 ID の重複は配送を妨げないこと");
        assert!(
            termination_rx.try_recv().is_err(),
            "未知 ID の重複でセッション終了を依頼しないこと"
        );
        assert!(
            audio_rx.try_recv().is_err(),
            "デコーダが無いためフレームは送出しないこと"
        );
    }

    /// 映像配送は既知 LOC Property の重複をセッション終了へ変換しないこと
    ///
    /// 音声配送と同じく、malformed Track はセッション終了ではなく配送の打ち切りになる。
    #[tokio::test]
    async fn video_delivery_does_not_request_session_termination_on_malformed_track() {
        let (frame_tx, frame_rx) = std::sync::mpsc::channel();
        let display_backlog = std::sync::atomic::AtomicI64::new(0);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let decode_order = std::sync::Arc::new(tokio::sync::Mutex::new(VideoDecodeOrder::new()));
        let delivery_order =
            std::sync::Arc::new(std::sync::Mutex::new(VideoDeliveryOrder::new(None)));
        let sink = FrameSink {
            frame_tx: &frame_tx,
            display_backlog: &display_backlog,
        };

        // VIDEO_CONFIG (0x0D) を 2 インスタンス持つ Object Properties を手組みする
        // (奇数型は長さ付きバイト列であり、`duplicate_loc_properties` は varint 値専用のため)
        let mut inner = Vec::new();
        shiguredo_moqt::varint::encode(0x0D, &mut inner);
        shiguredo_moqt::varint::encode(1, &mut inner);
        inner.push(0xAA);
        shiguredo_moqt::varint::encode(0, &mut inner);
        shiguredo_moqt::varint::encode(1, &mut inner);
        inner.push(0xBB);
        let mut properties = Vec::new();
        shiguredo_moqt::varint::encode(inner.len() as u64, &mut properties);
        properties.extend_from_slice(&inner);

        let result = deliver_video_object(
            10,
            0,
            Some(&properties),
            b"payload",
            &decode_order,
            &delivery_order,
            None,
            &sink,
            None,
            &termination_tx,
        )
        .await;

        assert!(
            matches!(result, VideoDelivery::MalformedTrack),
            "malformed Track として配送を打ち切ること: {result:?}"
        );
        assert!(
            termination_rx.try_recv().is_err(),
            "malformed Track でセッション終了を依頼しないこと"
        );
        assert!(
            frame_rx.try_recv().is_err(),
            "malformed Track の Object は送出しないこと"
        );
    }

    /// FETCH 応答の映像配送が malformed Track で打ち切られたら stream を後始末すること
    ///
    /// FETCH 応答 stream を Session の会計に残したままだと、DATA_STREAM_TIMEOUT
    /// (30 秒) でセッションが閉じる。malformed Track はセッション終了ではなく
    /// 購読・fetch の cancel で扱うため、STOP_SENDING と drain で会計から外す必要がある。
    /// 配送の継続とセッション終了の依頼では後始末しないことも固定する。
    #[test]
    fn video_fetch_stream_cleanup_follows_the_delivery_outcome() {
        assert!(
            video_fetch_stream_needs_cleanup(VideoDelivery::MalformedTrack),
            "malformed Track では STOP_SENDING と drain で stream を会計から外すこと"
        );
        assert!(
            !video_fetch_stream_needs_cleanup(VideoDelivery::Delivered(1)),
            "配送を続ける Object では stream を打ち切らないこと"
        );
        assert!(
            !video_fetch_stream_needs_cleanup(VideoDelivery::Terminated),
            "セッション終了を依頼したら stream の後始末はしないこと"
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
        let audio_config_validated_generation = std::sync::atomic::AtomicU64::new(0);
        let (termination_tx, _termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_validated_generation,
            1,
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
    /// 壁時計と比べられる値 (draft-ietf-moq-loc-05 §2.3.1.1) をそのまま渡せる。
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
        let audio_config_validated_generation = std::sync::atomic::AtomicU64::new(0);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_validated_generation,
            1,
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

        // 並び替わって、送出済みの TIMESTAMP より古い音声が遅れて届く場合がこれにあたる
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

    /// live の音声が並び替わって届いても、送出済みの TIMESTAMP を巻き戻さないこと
    ///
    /// live の datagram は並び替わって届きうる (draft-ietf-moq-transport-23 §2.2 (Subgroups) /
    /// §2.3.1 (Group IDs))。音声の再生は TIMESTAMP の順を前提とする
    /// (映像と違い、復号の出力がそのまま提示の順になる) ため、古い音声は送出しない。
    ///
    /// 判定が無いと、遅れて届いた古い TIMESTAMP が再生経路へ流れ、送出済みの TIMESTAMP
    /// から巻き戻る。
    #[tokio::test]
    async fn audio_delivery_does_not_send_objects_older_than_the_delivered_one() {
        let packet = encode_opus_silence(48_000, 1);
        let (audio_tx, audio_rx) = std::sync::mpsc::channel();
        let audio_config_validated_generation = std::sync::atomic::AtomicU64::new(0);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_validated_generation,
            1,
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
        // 2. 並び替わって、購読後に届いた音声より古い TIMESTAMP の音声が遅れて届く
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
            "送出順の判定は datagram と subgroup stream の経路で共有すること"
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
    /// 購読の境界は `SUBSCRIBE_OK` の Largest Object であり、その位置の Object は live と relay の
    /// replay の両方で届きうる (draft-ietf-moq-transport-23 §3.1.4)。境界以下を送出しないため、
    /// どちらで届いても再生経路へは流れない。
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
        let audio_config_validated_generation = std::sync::atomic::AtomicU64::new(0);
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
            &audio_config_validated_generation,
            1,
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

        // 1. 境界の位置の Object が relay の replay で届く
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
        // 6. 境界以下の音声が、live よりずっと古い TIMESTAMP で届く (replay が遅れた場合)。
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
        let audio_config_validated_generation = std::sync::atomic::AtomicU64::new(0);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_validated_generation,
            1,
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
        // そのまま使う (負の遅れは健全な状態として 0 に寄せる)
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
            stats.decode_lag_ms,
            stats.lag_ms.map(|lag_ms| lag_ms.max(0)),
            "測れないときは出力の遅れをそのまま使い、負の遅れは 0 に寄せること"
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
    /// draft-ietf-moq-transport-23 §9.19.17 (LARGEST OBJECT Parameter) は Object が publish
    /// されていれば LARGEST_OBJECT を MUST 含むと定めるため、省略は未 publish を意味する。
    /// 境界が無いことを理由に無音にしない。
    #[tokio::test]
    async fn audio_delivery_sends_objects_when_there_is_no_boundary() {
        let packet = encode_opus_silence(48_000, 1);
        let (audio_tx, audio_rx) = std::sync::mpsc::channel();
        let audio_config_validated_generation = std::sync::atomic::AtomicU64::new(0);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        // 境界を設定しない (Largest Object が告知されなかった購読)
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_validated_generation,
            1,
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
    /// OpusHead は publisher が最初の音声 Object にだけ載せ、その Object は購読の境界以下と
    /// して落ちるためである。
    #[tokio::test]
    async fn audio_delivery_validates_audio_config_before_the_subscription_boundary() {
        let packet = encode_opus_silence(48_000, 1);
        let properties = audio_loc_properties(960, 48_000, Some(&opus_head(48_000, 1)));
        let properties_bytes = properties
            .encode()
            .expect("LOC プロパティを encode できること");

        let (audio_tx, audio_rx) = std::sync::mpsc::channel();
        let audio_config_validated_generation = std::sync::atomic::AtomicU64::new(0);
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
            &audio_config_validated_generation,
            1,
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
        assert_eq!(
            audio_config_validated_generation.load(Ordering::Acquire),
            1,
            "境界以下の音声でも Audio Config を検証済みとして記録すること"
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
    /// 不要である。境界以下を録画しないため、同じ Object が live と relay の replay の両方で
    /// 届いても MP4 に重複が残らない。
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

        // 境界以下の音声 (relay の replay) と live の音声で TIMESTAMP と payload を変える。
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
        let audio_config_validated_generation = std::sync::atomic::AtomicU64::new(0);
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
            &audio_config_validated_generation,
            1,
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

    /// 音声 Object の位置が datagram / subgroup stream それぞれの Group ID と Object ID から
    /// 取れること
    ///
    /// 2 つの経路は位置の持ち方が異なるため、取り違えをここで固定する。位置を取り違えると、
    /// 購読の境界と比べられず境界以下の音声が再生経路へ流れる。
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
    }

    /// 境界より後の live の音声が並び替わって届いたときも、TIMESTAMP が巻き戻らないこと
    ///
    /// 境界の判定は購読より前の音声だけを落とすため、境界より後の並び替え
    /// (draft-ietf-moq-transport-23 §2.2 (Subgroups) / §2.3.1 (Group IDs)) は
    /// TIMESTAMP の判定が防ぐ。GOAWAY の移行直後などに起こりうる。
    #[tokio::test]
    async fn audio_delivery_drops_reordered_live_objects_after_the_boundary() {
        let packet = encode_opus_silence(48_000, 1);
        let (audio_tx, audio_rx) = std::sync::mpsc::channel();
        let audio_config_validated_generation = std::sync::atomic::AtomicU64::new(0);
        let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
        let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
        let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(Some(loc(100, 0))));
        let delivery = test_audio_delivery(
            &audio_tx,
            &audio_config_validated_generation,
            1,
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

    /// 映像の購読境界の判定を固定する
    ///
    /// 購読より前に publish された映像は relay の replay や FETCH の応答で届く。境界以下の
    /// 映像は復号はするが表示しない、という規則を実デコーダと MP4 の読み戻しで確認する。
    /// モックやスタブは使わない。
    mod video_delivery_boundary_tests {
        use super::*;

        /// テスト用: 購読境界の状態を共有する `Arc` を作る
        fn delivery_order_with_boundary(
            boundary: Option<Location>,
        ) -> std::sync::Arc<std::sync::Mutex<VideoDeliveryOrder>> {
            std::sync::Arc::new(std::sync::Mutex::new(VideoDeliveryOrder::new(boundary)))
        }

        /// テスト用: 映像 Object の LOC Properties を組み立てる
        ///
        /// `video_config` を渡すと PROP_VIDEO_CONFIG を載せる。キーフレームは
        /// PROP_VIDEO_FRAME_MARKING の I ビット (0x20) で示す (RFC 9626 §3.2)。
        fn video_loc_properties(
            timestamp: u64,
            timescale: u64,
            video_config: Option<&[u8]>,
            is_keyframe: bool,
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
            props.push(LocProperty {
                prop_id: PROP_VIDEO_FRAME_MARKING,
                value: LocPropertyValue::Bytes(vec![if is_keyframe { 0x20 } else { 0x00 }]),
            });
            if let Some(config) = video_config {
                props.push(LocProperty {
                    prop_id: PROP_VIDEO_CONFIG,
                    value: LocPropertyValue::Bytes(config.to_vec()),
                });
            }
            props
        }

        /// テスト用: 実エンコーダで VP8 のキーフレームと後続の inter frame を 1 つずつ作る
        ///
        /// 実デコーダで復号できる本物のフレームを使う。
        pub(super) fn encode_vp8_frames() -> (Vec<u8>, Vec<u8>) {
            use std::num::NonZeroUsize;

            use shiguredo_libvpx::{
                CodecConfig, EncodeOptions, Encoder, EncoderConfig, ImageData, ImageFormat,
                Vp8Config,
            };

            const WIDTH: usize = 64;
            const HEIGHT: usize = 48;
            // 単色ではなく位置に応じて値を変え、実デコーダを通ったことを確かめられるようにする
            let mut y = vec![0u8; WIDTH * HEIGHT];
            for row in 0..HEIGHT {
                for col in 0..WIDTH {
                    y[row * WIDTH + col] = ((row + col) % 200) as u8;
                }
            }
            let u = vec![96u8; WIDTH / 2 * (HEIGHT / 2)];
            let v = vec![160u8; WIDTH / 2 * (HEIGHT / 2)];
            let mut config = EncoderConfig::new(
                WIDTH,
                HEIGHT,
                ImageFormat::I420,
                CodecConfig::Vp8(Vp8Config::default()),
            );
            // 2 フレーム目を inter frame にするため、キーフレームの自動挿入を止める
            config.keyframe_interval = NonZeroUsize::new(300);
            config.lag_in_frames = NonZeroUsize::new(1);
            let mut encoder = Encoder::new(config).expect("テスト用エンコーダを作成できること");
            let image = ImageData::I420 {
                y: &y,
                u: &u,
                v: &v,
            };
            encoder
                .encode(
                    &image,
                    &EncodeOptions {
                        force_keyframe: true,
                    },
                )
                .expect("テスト用キーフレームをエンコードできること");
            // 次の encode の前に、next_frame が None を返すまで取り出す
            let mut keyframe = None;
            while let Some(frame) = encoder.next_frame() {
                if keyframe.is_none() {
                    assert!(
                        frame.is_keyframe(),
                        "テスト用フレームがキーフレームであること"
                    );
                    keyframe = Some(frame.data().to_vec());
                }
            }
            let keyframe = keyframe.expect("キーフレームのエンコード結果が得られること");
            encoder
                .encode(
                    &image,
                    &EncodeOptions {
                        force_keyframe: false,
                    },
                )
                .expect("テスト用 inter frame をエンコードできること");
            let mut inter_frame = None;
            while let Some(frame) = encoder.next_frame() {
                if inter_frame.is_none() {
                    assert!(
                        !frame.is_keyframe(),
                        "2 つ目のテスト用フレームが inter frame であること"
                    );
                    inter_frame = Some(frame.data().to_vec());
                }
            }
            let inter_frame = inter_frame.expect("inter frame のエンコード結果が得られること");
            (keyframe, inter_frame)
        }

        /// テスト用: MP4 を読み戻し、(timestamp, payload) の列を返す
        fn read_mp4_samples(path: &std::path::Path) -> Vec<(u64, Vec<u8>)> {
            let bytes = std::fs::read(path).expect("MP4 ファイルを読めること");
            let mut demuxer = shiguredo_mp4::demux::Mp4FileDemuxer::new();
            while let Some(required) = demuxer.required_input() {
                let position = required.position as usize;
                demuxer.handle_input(shiguredo_mp4::demux::Input {
                    position: required.position,
                    data: &bytes[position..],
                });
            }
            let mut samples = Vec::new();
            while let Some(sample) = demuxer.next_sample().expect("サンプルを読めること")
            {
                let start = sample.data_offset as usize;
                samples.push((
                    sample.timestamp,
                    bytes[start..start + sample.data_size].to_vec(),
                ));
            }
            samples
        }

        /// テスト用: 映像 Object を 1 つ配送経路へ流した結果を返す
        ///
        /// `deliver_video_object` の引数が多いため、テストで位置・時刻・ペイロードだけを
        /// 変えて呼べるようにまとめる。再生するテストでは実デコーダを渡し、録画だけの
        /// テストでは `None` を渡す。
        #[expect(clippy::too_many_arguments)]
        pub(super) async fn deliver_video_at(
            delivery_order: &std::sync::Arc<std::sync::Mutex<VideoDeliveryOrder>>,
            decode_order: &std::sync::Arc<tokio::sync::Mutex<VideoDecodeOrder>>,
            decoder: Option<&mut decoder::VideoDecoder>,
            recorder: Option<&RecorderSender>,
            sink: &FrameSink<'_>,
            termination_tx: &tokio::sync::mpsc::Sender<SessionError>,
            location: Location,
            timestamp: u64,
            timescale: u64,
            video_config: Option<&[u8]>,
            is_keyframe: bool,
            payload: &[u8],
        ) -> VideoDelivery {
            let properties = video_loc_properties(timestamp, timescale, video_config, is_keyframe)
                .encode()
                .expect("LOC プロパティを encode できること");
            deliver_video_object(
                location.group_id,
                location.object_id,
                Some(&properties),
                payload,
                decode_order,
                delivery_order,
                decoder,
                sink,
                recorder,
                termination_tx,
            )
            .await
        }

        /// 購読の境界以下の位置は表示しないこと
        ///
        /// 境界と同じ位置も含める。境界の位置の Object は live と FETCH の両方で届きうる
        /// ため、どちらも落とさないと二重に表示される。`Location` の順序は (group_id,
        /// object_id) の辞書順である (draft-ietf-moq-transport-23 §8.2)。
        #[test]
        fn video_delivery_order_detects_objects_at_or_before_the_boundary() {
            let mut order = VideoDeliveryOrder::new(Some(loc(100, 5)));
            assert!(
                order.is_before_boundary(loc(100, 4)),
                "同じ Group の境界より前の Object は境界以下であること"
            );
            assert!(
                order.is_before_boundary(loc(100, 5)),
                "境界と同じ位置の Object も境界以下であること"
            );
            assert!(
                order.is_before_boundary(loc(99, 100)),
                "前の Group の Object は境界以下であること"
            );
            assert!(
                !order.is_before_boundary(loc(100, 6)),
                "同じ Group の境界より後の Object は live であること"
            );
            assert!(
                !order.is_before_boundary(loc(101, 0)),
                "次の Group の Object は live であること"
            );
        }

        /// 映像配送の累計が増えたときだけログ用に更新すること
        ///
        /// 100 ms の tick で累計を前回のログと比べ、増えたときだけ info を出す。増えていない
        /// ときに同じ内容を出し続けるとログが埋もれ、原因の切り分けに使えなくなる。
        #[test]
        fn video_boundary_counter_is_logged_only_when_it_increases() {
            let mut previous = 0;
            log_video_boundary_counter(&mut previous, 0);
            assert_eq!(previous, 0, "増えていないときは前回の値を変えないこと");

            log_video_boundary_counter(&mut previous, 3);
            assert_eq!(previous, 3, "増えたときは前回の値を更新すること");

            // 同じ累計をもう一度渡しても、前回の値は変えない
            log_video_boundary_counter(&mut previous, 3);
            assert_eq!(previous, 3, "同じ累計では前回の値を変えないこと");
        }

        /// 再購読で境界が後退しないこと
        ///
        /// replay の受信途中では「最後に受信した Location」が起動時の Largest Object より
        /// 前になりうる。境界を後退させると、購読より前に publish された Object が再送された
        /// ときに表示経路へ戻る。候補が `None` の場合も既知の境界を消さない。
        #[test]
        fn video_delivery_order_advances_the_boundary_without_moving_backwards() {
            // 境界が無ければ最初の Object も live である
            let mut order = VideoDeliveryOrder::new(None);
            assert!(
                !order.is_before_boundary(loc(0, 0)),
                "境界が無ければ最初の Object も live であること"
            );

            // 後ろの位置の候補では境界が進む
            order.advance_boundary(Some(loc(10, 2)));
            assert!(
                order.is_before_boundary(loc(10, 2)),
                "進めた境界と同じ位置は境界以下であること"
            );
            assert!(
                !order.is_before_boundary(loc(10, 3)),
                "進めた境界より後は live であること"
            );

            // 前の位置の候補では境界が後退しない
            order.advance_boundary(Some(loc(10, 1)));
            assert!(
                order.is_before_boundary(loc(10, 2)),
                "前の位置の候補では境界が後退しないこと"
            );

            // 候補が無くても既知の境界を消さない
            order.advance_boundary(None);
            assert!(
                order.is_before_boundary(loc(10, 2)),
                "候補が無くても境界を消さないこと"
            );

            // 同じ Group 内の後ろの位置でも境界が進む
            order.advance_boundary(Some(loc(10, 5)));
            assert!(
                order.is_before_boundary(loc(10, 5)),
                "同じ Group 内で進めた境界と同じ位置は境界以下であること"
            );
            assert!(
                !order.is_before_boundary(loc(10, 6)),
                "同じ Group 内で進めた境界より後は live であること"
            );

            // さらに後ろの候補では境界が進む
            order.advance_boundary(Some(loc(20, 0)));
            assert!(
                order.is_before_boundary(loc(10, 2)),
                "進んだ境界以下の位置は境界以下であること"
            );
            assert!(
                !order.is_before_boundary(loc(20, 1)),
                "進んだ境界より後は live であること"
            );
            assert_eq!(
                order.before_boundary_objects(),
                5,
                "境界以下と判定した Object の累計を数えること"
            );
        }

        /// session 移行の映像の境界は再読み値を選ぶこと
        ///
        /// 再購読の開始時に Location Filter へ渡したスナップショットを使うと、移行中に
        /// 旧 session で受信した Object を新しい購読の再送分として送出し直す。
        #[test]
        fn session_migration_uses_the_reread_location_as_the_video_boundary() {
            let snapshot = loc(10, 1);
            let reread = loc(10, 7);
            assert_eq!(
                select_video_subscription_boundary(
                    ResubscribeRoute::SessionMigration,
                    VideoBoundaryCandidates {
                        snapshot: Some(snapshot),
                        reread: Some(reread),
                    },
                ),
                Some(reread),
                "session 移行では切り替え直前に読み直した値を候補にすること"
            );
            assert_eq!(
                select_video_subscription_boundary(
                    ResubscribeRoute::SessionMigration,
                    VideoBoundaryCandidates {
                        snapshot: None,
                        reread: Some(reread),
                    },
                ),
                Some(reread),
                "スナップショットが無くても再読み値を候補にすること"
            );
            assert_eq!(
                select_video_subscription_boundary(
                    ResubscribeRoute::SessionMigration,
                    VideoBoundaryCandidates {
                        snapshot: Some(snapshot),
                        reread: None,
                    },
                ),
                None,
                "再読み値が無ければ候補が無いこと"
            );
        }

        /// request stream 単位の再発行の映像の境界はスナップショットを選ぶこと
        ///
        /// `record_last_location` は Object の送出前に記録するため、再発行の完了後に
        /// 読み直した値を境界にすると、記録済みだがまだ送出していない Object を境界以下
        /// として落とす競合がある。
        #[test]
        fn request_reissue_uses_the_snapshot_location_as_the_video_boundary() {
            let snapshot = loc(10, 1);
            let reread = loc(10, 7);
            assert_eq!(
                select_video_subscription_boundary(
                    ResubscribeRoute::RequestReissue,
                    VideoBoundaryCandidates {
                        snapshot: Some(snapshot),
                        reread: Some(reread),
                    },
                ),
                Some(snapshot),
                "request 単位の再発行では再発行前のスナップショットを候補にすること"
            );
            assert_eq!(
                select_video_subscription_boundary(
                    ResubscribeRoute::RequestReissue,
                    VideoBoundaryCandidates {
                        snapshot: Some(snapshot),
                        reread: None,
                    },
                ),
                Some(snapshot),
                "再読み値が無くてもスナップショットを候補にすること"
            );
            assert_eq!(
                select_video_subscription_boundary(
                    ResubscribeRoute::RequestReissue,
                    VideoBoundaryCandidates {
                        snapshot: None,
                        reread: Some(reread),
                    },
                ),
                None,
                "スナップショットが無ければ候補が無いこと"
            );
        }

        /// 購読の境界以下の映像が `frame_tx` へ送出されないこと
        ///
        /// live の Subgroup stream と FETCH 応答 stream が共有する `deliver_video_object` を
        /// 実 VP8 エンコーダと実デコーダで通す。境界以下でも復号は行う (境界以降のフレームが
        /// 参照しうるため) が、表示経路へは送らない。
        #[tokio::test(flavor = "multi_thread")]
        async fn video_delivery_does_not_send_objects_before_the_subscription_boundary() {
            let (payload, _) = encode_vp8_frames();
            let (frame_tx, frame_rx) = std::sync::mpsc::channel();
            let display_backlog = std::sync::atomic::AtomicI64::new(0);
            let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
            let decode_order =
                std::sync::Arc::new(tokio::sync::Mutex::new(VideoDecodeOrder::new()));
            let delivery_order = delivery_order_with_boundary(Some(loc(10, 0)));
            let mut decoder =
                decoder::build_video_decoder("vp8").expect("VP8 デコーダを生成できること");
            let sink = FrameSink {
                frame_tx: &frame_tx,
                display_backlog: &display_backlog,
            };

            let result = deliver_video_at(
                &delivery_order,
                &decode_order,
                Some(&mut decoder),
                None,
                &sink,
                &termination_tx,
                loc(10, 0),
                960,
                48_000,
                None,
                true,
                &payload,
            )
            .await;

            assert!(
                matches!(result, VideoDelivery::Delivered(0)),
                "境界以下の映像は送出しないこと"
            );
            assert!(
                frame_rx.try_recv().is_err(),
                "境界以下の映像は frame_tx へ送出しないこと"
            );
            assert!(
                termination_rx.try_recv().is_err(),
                "書式違反が無いためセッション終了は依頼されないこと"
            );
            assert_eq!(
                display_backlog.load(Ordering::Relaxed),
                0,
                "表示待ちへ積まないこと"
            );
            assert_eq!(
                decode_order.lock().await.decoding_group_id(),
                Some(10),
                "境界以下でも復号するため復号順の状態は進むこと"
            );
            assert_eq!(
                delivery_order
                    .lock()
                    .expect("映像の購読境界のロックが毒されていないこと")
                    .before_boundary_objects(),
                1,
                "境界以下として落とした Object の累計に残すこと"
            );
        }

        /// 購読の境界以下の映像を復号していること
        ///
        /// 境界以下のキーフレームを復号しないと、境界より後の inter frame を復号できない。
        /// 境界判定を復号の前に移す誤りを検出するため、境界以下のキーフレームと、その直後の
        /// 境界より後の inter frame を実エンコーダと実デコーダで通す。
        #[tokio::test(flavor = "multi_thread")]
        async fn video_delivery_decodes_objects_before_the_subscription_boundary() {
            let (keyframe, inter_frame) = encode_vp8_frames();
            let (frame_tx, frame_rx) = std::sync::mpsc::channel();
            let display_backlog = std::sync::atomic::AtomicI64::new(0);
            let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
            let decode_order =
                std::sync::Arc::new(tokio::sync::Mutex::new(VideoDecodeOrder::new()));
            let delivery_order = delivery_order_with_boundary(Some(loc(10, 0)));
            let mut decoder =
                decoder::build_video_decoder("vp8").expect("VP8 デコーダを生成できること");
            let sink = FrameSink {
                frame_tx: &frame_tx,
                display_backlog: &display_backlog,
            };

            // 境界以下のキーフレームは表示しないが復号する
            let below = deliver_video_at(
                &delivery_order,
                &decode_order,
                Some(&mut decoder),
                None,
                &sink,
                &termination_tx,
                loc(10, 0),
                960,
                48_000,
                None,
                true,
                &keyframe,
            )
            .await;
            assert!(
                matches!(below, VideoDelivery::Delivered(0)),
                "境界以下のキーフレームは送出しないこと"
            );
            assert!(
                frame_rx.try_recv().is_err(),
                "境界以下のキーフレームは frame_tx へ送出しないこと"
            );

            // 境界より後の inter frame は、境界以下のキーフレームを復号していなければ復号できない
            let above = deliver_video_at(
                &delivery_order,
                &decode_order,
                Some(&mut decoder),
                None,
                &sink,
                &termination_tx,
                loc(10, 1),
                1_920,
                48_000,
                None,
                false,
                &inter_frame,
            )
            .await;
            assert!(
                matches!(above, VideoDelivery::Delivered(1)),
                "境界以下のキーフレームを復号していれば inter frame を送出できること: {above:?}"
            );
            assert!(
                frame_rx.try_recv().is_ok(),
                "境界より後の inter frame が frame_tx へ送出されること"
            );
            assert!(
                termination_rx.try_recv().is_err(),
                "書式違反が無いためセッション終了は依頼されないこと"
            );
        }

        /// 購読の境界より後の live の映像が従来どおり送出されること
        #[tokio::test(flavor = "multi_thread")]
        async fn video_delivery_sends_live_objects_after_the_subscription_boundary() {
            let (payload, _) = encode_vp8_frames();
            let (frame_tx, frame_rx) = std::sync::mpsc::channel();
            let display_backlog = std::sync::atomic::AtomicI64::new(0);
            let (termination_tx, _termination_rx) = tokio::sync::mpsc::channel(1);
            let decode_order =
                std::sync::Arc::new(tokio::sync::Mutex::new(VideoDecodeOrder::new()));
            let delivery_order = delivery_order_with_boundary(Some(loc(10, 0)));
            let mut decoder =
                decoder::build_video_decoder("vp8").expect("VP8 デコーダを生成できること");
            let sink = FrameSink {
                frame_tx: &frame_tx,
                display_backlog: &display_backlog,
            };

            // TIMESCALE 48000 の 1920 は 40 ms である
            let result = deliver_video_at(
                &delivery_order,
                &decode_order,
                Some(&mut decoder),
                None,
                &sink,
                &termination_tx,
                loc(10, 1),
                1_920,
                48_000,
                None,
                true,
                &payload,
            )
            .await;

            assert!(
                matches!(result, VideoDelivery::Delivered(1)),
                "境界より後の映像は 1 フレーム送出すること: {result:?}"
            );
            let frame = frame_rx
                .try_recv()
                .expect("境界より後の映像は frame_tx へ送出されること");
            assert_eq!(
                frame.timestamp_us,
                Some(40_000),
                "TIMESTAMP と TIMESCALE から求めた表示時刻を載せること"
            );
            assert_eq!(
                frame.timestamp_kind,
                TimestampKind::MediaTime,
                "TIMESCALE がある映像はメディア時刻であること"
            );
            assert_eq!(
                display_backlog.load(Ordering::Relaxed),
                1,
                "表示待ちを 1 つ増やすこと"
            );
        }

        /// 購読の境界が無い場合は映像を送出すること
        #[tokio::test(flavor = "multi_thread")]
        async fn video_delivery_sends_objects_when_there_is_no_boundary() {
            let (payload, _) = encode_vp8_frames();
            let (frame_tx, frame_rx) = std::sync::mpsc::channel();
            let display_backlog = std::sync::atomic::AtomicI64::new(0);
            let (termination_tx, _termination_rx) = tokio::sync::mpsc::channel(1);
            let decode_order =
                std::sync::Arc::new(tokio::sync::Mutex::new(VideoDecodeOrder::new()));
            let delivery_order = delivery_order_with_boundary(None);
            let mut decoder =
                decoder::build_video_decoder("vp8").expect("VP8 デコーダを生成できること");
            let sink = FrameSink {
                frame_tx: &frame_tx,
                display_backlog: &display_backlog,
            };

            let result = deliver_video_at(
                &delivery_order,
                &decode_order,
                Some(&mut decoder),
                None,
                &sink,
                &termination_tx,
                loc(10, 0),
                960,
                48_000,
                None,
                true,
                &payload,
            )
            .await;

            assert!(
                matches!(result, VideoDelivery::Delivered(1)),
                "境界が無ければ映像を送出すること: {result:?}"
            );
            assert!(
                frame_rx.try_recv().is_ok(),
                "境界が無ければ frame_tx へ送出すること"
            );
        }

        /// 購読の境界以下の映像も録画の対象になること
        ///
        /// 録画は境界の判定より前に行う。replay が運ぶキーフレームを録画の基準にする現在の
        /// 挙動を、MP4 の先頭サンプルが replay の Object であることで固定する。再生しない
        /// 指定 (`--no-play`) と同じく、復号器が無くても録画は行う。
        #[tokio::test]
        async fn video_delivery_records_objects_before_the_subscription_boundary() {
            // shiguredo_mp4 のテストデータと同じ AV1 の Sequence Header OBU。MP4 の sample
            // entry を作るために必要である (VP8 は MP4 録画に対応していない)
            const AV1_CONFIG_OBUS: &[u8] = &[
                0x0A, 0x0B, 0x00, 0x00, 0x00, 0x24, 0xC4, 0xFF, 0xDF, 0x3F, 0xFE, 0x60, 0x10,
            ];
            const REPLAY_PAYLOAD: &[u8] = b"replay";
            const LIVE_PAYLOAD: &[u8] = b"live";

            let path = std::env::temp_dir()
                .join(format!("moq-sub-video-boundary-{}.mp4", std::process::id()));
            std::fs::remove_file(&path).ok();
            let recorder = Recorder::start(
                path.clone(),
                Setup {
                    video: Some(VideoSetup {
                        codec: "av01.0.08M.08".to_string(),
                        fps: 30,
                    }),
                    audio: None,
                },
            )
            .expect("録画を開始できること");
            let recorder_sender = recorder.sender();

            let (frame_tx, _frame_rx) = std::sync::mpsc::channel();
            let display_backlog = std::sync::atomic::AtomicI64::new(0);
            let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
            let decode_order =
                std::sync::Arc::new(tokio::sync::Mutex::new(VideoDecodeOrder::new()));
            let delivery_order = delivery_order_with_boundary(Some(loc(10, 0)));
            let sink = FrameSink {
                frame_tx: &frame_tx,
                display_backlog: &display_backlog,
            };

            // 境界以下の replay のキーフレームを録画する
            let replay_result = deliver_video_at(
                &delivery_order,
                &decode_order,
                None,
                Some(&recorder_sender),
                &sink,
                &termination_tx,
                loc(10, 0),
                960,
                48_000,
                Some(AV1_CONFIG_OBUS),
                true,
                REPLAY_PAYLOAD,
            )
            .await;
            assert!(
                matches!(replay_result, VideoDelivery::Delivered(0)),
                "再生しない指定では復号も送出もしないこと"
            );
            // 境界より後の live のキーフレームを録画する
            deliver_video_at(
                &delivery_order,
                &decode_order,
                None,
                Some(&recorder_sender),
                &sink,
                &termination_tx,
                loc(10, 1),
                1_920,
                48_000,
                Some(AV1_CONFIG_OBUS),
                true,
                LIVE_PAYLOAD,
            )
            .await;
            recorder.finish().expect("MP4 を finalize できること");

            assert!(
                termination_rx.try_recv().is_err(),
                "書式違反が無いためセッション終了は依頼されないこと"
            );
            let samples = read_mp4_samples(&path);
            assert_eq!(
                samples.len(),
                2,
                "境界以下と境界より後の 2 サンプルを録画すること"
            );
            assert_eq!(
                samples[0].1,
                REPLAY_PAYLOAD.to_vec(),
                "先頭サンプルは replay のキーフレームであり、録画の基準になること"
            );
            assert_eq!(
                samples[1].1,
                LIVE_PAYLOAD.to_vec(),
                "境界より後の live の Object も録画すること"
            );
            // MP4 のトラックの timescale は 1,000,000 であり、TIMESTAMP の差 (20 ms) が
            // サンプル時刻の差になる。先頭サンプルはトラック内の 0 から始まる
            assert_eq!(
                samples[0].0, 0,
                "先頭サンプルはトラック内の 0 から始まること"
            );
            assert_eq!(
                samples[1].0 - samples[0].0,
                20_000,
                "replay と live の TIMESTAMP の差がサンプル時刻の差になること"
            );

            std::fs::remove_file(&path).ok();
        }
    }

    /// Group の切り替え gate の配線のテスト
    ///
    /// stream task 間の結線 (`begin_stream` / `end_stream` の呼び出し) は
    /// `crate::group_switch` の協調テストと、`decode_video_stream` の戻りで `end_stream` を
    /// 呼ぶことのコード確認で担保する。ここでは、保留が解除されるまで配送せず、解除後に
    /// キーフレーム → inter frame の順で `frame_tx` へ送出することを固定する。
    mod group_switch_tests {
        use super::super::*;
        use super::loc;
        use super::video_delivery_boundary_tests::{deliver_video_at, encode_vp8_frames};

        /// 保留中は配送せず、解除後にキーフレームから順に配送すること
        ///
        /// 実エンコーダで作ったキーフレームと inter frame を、live と同じ `deliver_video_at`
        /// 経由で `frame_tx` へ流す。保留中は `frame_tx` に何も届かず、解除後にキーフレーム、
        /// inter frame の順で 1 枚ずつ届くことを確かめる。
        #[tokio::test(flavor = "multi_thread")]
        async fn held_video_object_is_delivered_in_order_after_release() {
            let (keyframe, inter_frame) = encode_vp8_frames();
            let gate = std::sync::Arc::new(SharedGroupSwitchGate::new());
            // stream task と同じく、購読の世代を固定したハンドルで操作する
            let handle = GroupSwitchHandle::new(std::sync::Arc::clone(&gate));
            let (frame_tx, frame_rx) = std::sync::mpsc::channel();
            let display_backlog = std::sync::atomic::AtomicI64::new(0);
            let (termination_tx, mut termination_rx) = tokio::sync::mpsc::channel(1);
            let decode_order =
                std::sync::Arc::new(tokio::sync::Mutex::new(VideoDecodeOrder::new()));
            let delivery_order =
                std::sync::Arc::new(std::sync::Mutex::new(VideoDeliveryOrder::new(None)));
            let sink = FrameSink {
                frame_tx: &frame_tx,
                display_backlog: &display_backlog,
            };

            // 前の Group 10 の Object を配送して復号を始め、その stream を開いたままにする
            handle.begin_stream(10, 100);
            handle.wait_until_deliver(10).await;
            let mut decoder =
                decoder::build_video_decoder("vp8").expect("VP8 デコーダを生成できること");

            // 次の Group 11 の stream を開き、キーフレームを保留する。保留中は、この
            // キーフレームを読んだ stream task が復号せずに待つ
            handle.begin_stream(11, 110);
            let waiter = {
                let handle = handle.clone();
                tokio::spawn(async move { handle.wait_until_deliver(11).await })
            };
            // 前の Group の stream が終端したら解除し、キーフレーム → inter frame の順に
            // live と同じ配送経路へ流す。保留中の待機は、時計を止めた
            // held_video_object_is_not_recorded_before_release で固定する
            handle.end_stream(10, 100);
            tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
                .await
                .expect("前の Group の stream が終端したら保留を解除すること")
                .expect("待機 task が panic しないこと");
            let key_result = deliver_video_at(
                &delivery_order,
                &decode_order,
                Some(&mut decoder),
                None,
                &sink,
                &termination_tx,
                loc(11, 0),
                960,
                48_000,
                None,
                true,
                &keyframe,
            )
            .await;
            assert!(
                matches!(key_result, VideoDelivery::Delivered(1)),
                "解除後にキーフレームを 1 枚送出すること"
            );
            let inter_result = deliver_video_at(
                &delivery_order,
                &decode_order,
                Some(&mut decoder),
                None,
                &sink,
                &termination_tx,
                loc(11, 1),
                1_000,
                48_000,
                None,
                false,
                &inter_frame,
            )
            .await;
            assert!(
                matches!(inter_result, VideoDelivery::Delivered(1)),
                "キーフレームの後に inter frame を 1 枚送出すること"
            );

            // 送出順を固定する。キーフレームを復号していなければ inter frame は復号できない
            let first = frame_rx.try_recv().expect("キーフレームが送出されること");
            let second = frame_rx.try_recv().expect("inter frame が送出されること");
            assert!(
                frame_rx.try_recv().is_err(),
                "送出するのは 2 枚だけであること"
            );
            assert!(
                first.timestamp_us < second.timestamp_us,
                "キーフレーム → inter frame の順に送出すること: first={:?} second={:?}",
                first.timestamp_us,
                second.timestamp_us
            );
            assert!(
                termination_rx.try_recv().is_err(),
                "書式違反が無いためセッション終了は依頼されないこと"
            );
            assert_eq!(
                display_backlog.load(Ordering::Relaxed),
                2,
                "表示待ちへ 2 枚積むこと"
            );
        }

        /// 保留した Object の位置を、解除されるまで「最後に受信した位置」に記録しないこと
        ///
        /// 記録が解除より前だと、session 移行で保留を捨てたときに新しい購読の Location
        /// Filter がその位置から取り直せない。
        #[tokio::test(flavor = "current_thread", start_paused = true)]
        async fn held_video_object_is_not_recorded_before_release() {
            let gate = std::sync::Arc::new(SharedGroupSwitchGate::new());
            let handle = GroupSwitchHandle::new(std::sync::Arc::clone(&gate));
            let last_received_locations =
                std::sync::Arc::new(std::sync::Mutex::new(LastReceivedLocations::default()));

            // 前の Group 10 の Object を配送して復号を始め、その stream を開いたままにする
            handle.begin_stream(10, 100);
            handle.wait_until_deliver(10).await;
            handle.begin_stream(11, 110);

            let mut waiter = {
                let handle = handle.clone();
                let last_received_locations = std::sync::Arc::clone(&last_received_locations);
                tokio::spawn(async move {
                    wait_until_video_deliverable(&handle, 11, 0, &last_received_locations).await
                })
            };
            let still_waiting =
                tokio::time::timeout(std::time::Duration::from_millis(1), &mut waiter).await;
            assert!(still_waiting.is_err(), "保留中は記録しないこと");
            assert_eq!(
                last_received_locations
                    .lock()
                    .expect("last locations lock is not poisoned")
                    .video,
                None,
                "保留中の Object を「最後に受信した位置」に記録しないこと"
            );

            handle.end_stream(10, 100);
            tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
                .await
                .expect("前の Group の stream が終端したら保留を解除すること")
                .expect("待機 task が panic しないこと");
            assert_eq!(
                last_received_locations
                    .lock()
                    .expect("last locations lock is not poisoned")
                    .video,
                Some(Location {
                    group_id: 11,
                    object_id: 0,
                }),
                "解除後に「最後に受信した位置」を記録すること"
            );
        }
    }

    /// SUBSCRIBE_TRACKS 由来の PUBLISH の受理判定を固定する
    mod subscribe_tracks_publish_tests {
        use super::*;
        use shiguredo_moqt::message::Publish;
        use shiguredo_moqt::message_parameter::PARAM_SUBSCRIBE_TRACKS_ID;
        use shiguredo_moqt::track_properties::TrackProperties;

        /// PUBLISH を組み立てる
        ///
        /// `subscribe_tracks_id` が `Some` のときだけ SUBSCRIBE_TRACKS_ID (0x36) を載せる。
        /// 値 `None` は「SUBSCRIBE_TRACKS に起因しない PUBLISH」を表す。
        fn publish(subscribe_tracks_id: Option<u64>) -> Publish {
            let mut parameters = MessageParameters::new();
            if let Some(value) = subscribe_tracks_id {
                parameters.push(MessageParameter {
                    param_type: PARAM_SUBSCRIBE_TRACKS_ID,
                    value: MessageParameterValue::VarInt(value),
                });
            }
            Publish {
                request_id: 5,
                track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テスト用の namespace を作れること"),
                track_name: b"video".to_vec(),
                track_alias: 3,
                parameters,
                track_properties: TrackProperties::new(),
            }
        }

        /// 送信した SUBSCRIBE_TRACKS の Request ID と一致する PUBLISH を受理すること
        #[test]
        fn matching_subscribe_tracks_id_is_accepted() {
            assert!(
                accepts_subscribe_tracks_publish(&publish(Some(9)), Some(9)),
                "SUBSCRIBE_TRACKS_ID が送信した Request ID と一致する PUBLISH は受理すること"
            );
        }

        /// Request ID が一致しない PUBLISH は受理しないこと
        #[test]
        fn different_subscribe_tracks_id_is_rejected() {
            assert!(
                !accepts_subscribe_tracks_publish(&publish(Some(10)), Some(9)),
                "SUBSCRIBE_TRACKS_ID が一致しない PUBLISH は拒否すること"
            );
        }

        /// SUBSCRIBE_TRACKS を送っていない状態では PUBLISH を受理しないこと
        #[test]
        fn publish_is_rejected_without_a_tracks_request() {
            assert!(
                !accepts_subscribe_tracks_publish(&publish(Some(9)), None),
                "SUBSCRIBE_TRACKS を送っていない状態では PUBLISH を拒否すること"
            );
        }

        /// SUBSCRIBE_TRACKS_ID を持たない PUBLISH は受理しないこと
        #[test]
        fn publish_without_subscribe_tracks_id_is_rejected() {
            assert!(
                !accepts_subscribe_tracks_publish(&publish(None), Some(9)),
                "SUBSCRIBE_TRACKS_ID を持たない PUBLISH は拒否すること"
            );
        }

        /// SUBSCRIBE 済みの Full Track Name を重複として判定すること
        #[test]
        fn subscribed_full_track_name_is_a_duplicate() {
            let target = publish(Some(9));
            let subscribed = vec![
                (
                    TrackNamespace::new(vec![b"live".to_vec()])
                        .expect("テスト用の namespace を作れること"),
                    b"video".to_vec(),
                ),
                (
                    TrackNamespace::new(vec![b"live".to_vec()])
                        .expect("テスト用の namespace を作れること"),
                    b"audio".to_vec(),
                ),
            ];
            assert!(
                is_already_subscribed_publish(&target, &subscribed),
                "namespace と track 名の両方が一致する PUBLISH は重複と判定すること"
            );
        }

        /// track 名だけが一致しても、namespace が違えば重複ではないこと
        #[test]
        fn different_namespace_is_not_a_duplicate() {
            let target = publish(Some(9));
            let subscribed = vec![(
                TrackNamespace::new(vec![b"other".to_vec()])
                    .expect("テスト用の namespace を作れること"),
                b"video".to_vec(),
            )];
            assert!(
                !is_already_subscribed_publish(&target, &subscribed),
                "namespace が異なる PUBLISH は重複と判定しないこと"
            );
        }

        /// 購読一覧が空なら重複ではないこと
        #[test]
        fn empty_subscriptions_have_no_duplicate() {
            assert!(
                !is_already_subscribed_publish(&publish(Some(9)), &[]),
                "購読一覧が空のときは重複と判定しないこと"
            );
        }
    }

    /// 移行をまたぐ Audio Config の処理を session の世代で固定する
    mod audio_config_generation_tests {
        use super::*;

        /// 同じ世代では 1 回だけ処理し、新しい世代では処理し直すこと
        ///
        /// `fetch_max` の戻り値で判定するため、低い世代の遅延配送が高い世代の記録を
        /// 巻き戻さないことも同時に固定する。
        #[test]
        fn claim_audio_config_generation_is_monotonic() {
            let validated = std::sync::atomic::AtomicU64::new(0);
            assert!(
                claim_audio_config_generation(&validated, 1),
                "最初の session の Audio Config は処理すること"
            );
            assert!(
                !claim_audio_config_generation(&validated, 1),
                "同じ session の Audio Config は 1 回だけ処理すること"
            );
            assert!(
                claim_audio_config_generation(&validated, 2),
                "移行後の session の Audio Config は処理し直すこと"
            );
            assert!(
                !claim_audio_config_generation(&validated, 1),
                "旧 session の Audio Config で検証済み世代を巻き戻さないこと"
            );
            assert!(
                !claim_audio_config_generation(&validated, 2),
                "移行後の session の Audio Config は 1 回だけ処理すること"
            );
        }

        /// 旧 session の Audio Config が移行後の session の記録を巻き戻さないこと
        ///
        /// 配送経路から `AudioDelivery::deliver` を通しても、旧世代の遅延 Object が
        /// 検証済み世代を進めないことを確認する。移行分岐から世代を進める配線は統合テストで
        /// 確認する。
        #[tokio::test]
        async fn old_session_audio_config_does_not_validate_the_new_generation() {
            let (audio_tx, _audio_rx) = std::sync::mpsc::channel();
            let audio_config_validated_generation = std::sync::atomic::AtomicU64::new(0);
            let (termination_tx, _termination_rx) = tokio::sync::mpsc::channel(1);
            let catch_up = std::sync::Mutex::new(AudioReceiveCatchUp::new());
            let delivery_order = std::sync::Mutex::new(AudioDeliveryOrder::new(None));
            let packet = encode_opus_silence(48_000, 1);
            let properties = audio_loc_properties(960, 48_000, Some(&opus_head(48_000, 1)));
            let properties_bytes = properties
                .encode()
                .expect("LOC プロパティを encode できること");

            // 世代 1 の session で Audio Config を処理する
            let delivery = test_audio_delivery(
                &audio_tx,
                &audio_config_validated_generation,
                1,
                &termination_tx,
                &catch_up,
                &delivery_order,
                None,
            );
            assert!(
                delivery
                    .deliver(Some(loc(0, 0)), Some(&properties_bytes), &packet, None)
                    .await,
                "Audio Config 付きの音声 Object は配送を継続すること"
            );
            assert_eq!(
                audio_config_validated_generation.load(Ordering::Acquire),
                1,
                "世代 1 の Audio Config を処理済みとして記録すること"
            );

            // 新しい session (世代 2) の Audio Config を処理し直す
            let migrated = test_audio_delivery(
                &audio_tx,
                &audio_config_validated_generation,
                2,
                &termination_tx,
                &catch_up,
                &delivery_order,
                None,
            );
            assert!(
                migrated
                    .deliver(Some(loc(1, 0)), Some(&properties_bytes), &packet, None)
                    .await,
                "移行後の Audio Config 付き Object も配送を継続すること"
            );
            assert_eq!(
                audio_config_validated_generation.load(Ordering::Acquire),
                2,
                "移行後の session の Audio Config を処理済みとして記録すること"
            );

            // 移行後に旧 session (世代 1) の遅延 Object が届いても記録は巻き戻らない
            let stale = test_audio_delivery(
                &audio_tx,
                &audio_config_validated_generation,
                1,
                &termination_tx,
                &catch_up,
                &delivery_order,
                None,
            );
            assert!(
                stale
                    .deliver(Some(loc(2, 0)), Some(&properties_bytes), &packet, None)
                    .await,
                "旧 session の Audio Config 付き Object も配送を継続すること"
            );
            assert_eq!(
                audio_config_validated_generation.load(Ordering::Acquire),
                2,
                "旧 session の Audio Config で検証済み世代を巻き戻さないこと"
            );
        }
    }
}
