//! MoQT セッション本体 (draft-ietf-moq-transport-21)
//!
//! `Session` 構造体と、1 本の `MOQT Transport Session` に閉じた endpoint-local な
//! 状態機械の基盤となる以下の責務を持つ:
//! - ライフサイクル (作成 / `poll_event` / `close` / `fail`)
//! - SETUP ハンドシェイク (draft §9.1 (SETUP))
//! - 制御ストリーム受信 dispatcher (`recv_control` / `recv_request` / `recv_stream_message`)
//! - Request ID API (`next_local_request_id`)
//! - REQUEST_OK / REQUEST_ERROR の送受信 (共通 dispatcher)
//!
//! data stream / datagram の送受信 state (`send_subgroup_header` / `send_subgroup_object` /
//! `send_data_stream_closed` / `recv_data_stream_stop_sending` / `recv_data_stream_type` /
//! `recv_subgroup_header` / `recv_subgroup_object` / `recv_fetch_header` /
//! `recv_object_datagram`) は `data.rs` に分離している。
//! 各メッセージ種別 (SUBSCRIBE / PUBLISH / FETCH / NAMESPACE 系 / GOAWAY) の送受信処理は
//! 兄弟モジュール (`subscription.rs` / `fetch.rs` / `namespace.rs` / `goaway.rs`) に
//! 分離している。relay 全体の routing / fan-out / cache / policy はこの型の責務ではない。

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use hashbrown::{HashMap, HashSet};

use crate::error::{
    PUBLISH_DONE_UPDATE_FAILED, REQUEST_INTERNAL_ERROR, SESSION_AUTH_TOKEN_CACHE_OVERFLOW,
    SESSION_INTERNAL_ERROR, SESSION_INVALID_AUTHORITY, SESSION_INVALID_PATH,
    SESSION_MALFORMED_AUTHORITY, SESSION_MALFORMED_PATH, SESSION_PROTOCOL_VIOLATION,
    SESSION_UNKNOWN_AUTH_TOKEN_ALIAS, is_local_error_code,
};
use crate::message::{
    ControlMessage, NAMESPACE_OK_ALLOWED_PARAMS, PublishDone, REQUEST_UPDATE_OK_ALLOWED_PARAMS,
    ReasonPhrase, Redirect, RequestError, RequestOk, Setup, TRACK_STATUS_OK_ALLOWED_PARAMS,
    common::TrackNamespace,
};
use crate::message_parameter::{AuthorizationToken, MessageParameters};
use crate::object_properties::{ObjectFieldTracker, ObjectPropertyTracker};
use crate::parameter::{SetupOptions, SetupUriValidationError};
use crate::subgroup_tracker::SubgroupTracker;
use crate::track_properties::TrackProperties;

use crate::stream::SETUP_STREAM_TYPE;

use super::auth_token_cache::AuthTokenCache;
use super::data::{IncomingDataStream, OutgoingDataStream};
use super::namespace::is_prefix_of;
use super::request_id::{RequestIdGenerator, RequestIdTracker};
use super::types::{
    DataStreamId, DeadlineTimer, Fetch, NamespacePublication, NamespaceSubscription,
    PeerGoawayInfo, RecvRequestError, RequestKind, RequestStreamEnd, Role, SendRequestError,
    SessionError, SessionEvent, SessionState, Subscription, SubscriptionState, TrackRole,
    TrackStatusEntry, TrackSubscription, Transport,
};

/// 1 本の `MOQT Transport Session` に閉じた sans-I/O 状態機械
///
/// この型が保持するのは、この endpoint から見た peer との protocol state のみ。
/// relay 全体のオーケストレーションは呼び出し側が扱う。
#[derive(Debug)]
pub(super) struct SetupState {
    pub(super) local: Option<Setup>,
    pub(super) peer: Option<Setup>,
}

#[derive(Debug)]
pub(super) struct AuthState {
    pub(super) peer_token_cache: AuthTokenCache,
}

#[derive(Debug)]
pub(super) struct RequestIdState {
    pub(super) local_generator: RequestIdGenerator,
    pub(super) peer_tracker: RequestIdTracker,
}

/// Track Alias と subscription の対応索引
///
/// draft-ietf-moq-transport-21 §3.1 (Subscriptions): "An endpoint MAY have multiple concurrent
/// subscriptions to the same Track, each identified by a unique Request ID. A publisher MAY
/// assign the same or different Track Aliases to these subscriptions."
///
/// 同一 Track への複数 subscription が同じ alias を共有できるため、alias → request_ids は
/// 1:N である。`Vec` の順序は登録順 (= Established 順) で、HashMap のイテレーション順に
/// 依存しない決定的な選択ができるようにしている。
/// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
#[derive(Debug)]
pub(super) struct AliasState {
    pub(super) my_publisher_aliases: HashMap<u64, Vec<u64>>,
    pub(super) peer_publisher_aliases: HashMap<u64, Vec<u64>>,
    pub(super) subscriptions_by_track: HashMap<(TrackNamespace, Vec<u8>, TrackRole), Vec<u64>>,
    /// キャンセル済み peer publisher alias の discard 用 tombstone
    ///
    /// draft-ietf-moq-transport-21 §3.1.2 (Track Alias): "Objects can arrive after a
    /// subscription has been cancelled. Subscribers SHOULD retain sufficient state to quickly
    /// discard these unwanted Objects, rather than treating them as belonging to an unknown
    /// Track Alias."
    ///
    /// alias に紐づく subscription が 1 つも残らなくなった時点で登録し、
    /// `peer_alias_retention_ms` の期間だけ保持する。期間中に届いた Object は
    /// [`crate::session::types::TrackDataAcceptance::Discarded`] として報告し、未知 alias と区別する。
    pub(super) peer_alias_tombstones: HashMap<u64, DeadlineTimer>,
}

/// キャンセル済み peer publisher alias の tombstone 保持期間のデフォルト値 (ms)
///
/// draft-ietf-moq-transport-21 §3.1.2 (Track Alias) は "retain sufficient state" と SHOULD で
/// 述べるのみで具体的な期間を規定していない。遅延 Object を捨てられる程度に短く、かつ
/// 通常の再確立を妨げない値として 5000ms を採用する。
/// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
pub const DEFAULT_PEER_ALIAS_RETENTION_MS: u64 = 5000;

/// datagram の object header 提供完了追跡エントリの上限 (subscription 単位の件数)
///
/// `TimingState::datagram_header_complete_ms` は再送の経過時間リセット防止のため
/// subscription の forget まで保持する。長期ライブ配信で無制限に増加するため、
/// 1 subscription あたりの件数に上限を設け、超過時は最も古い Group から丸ごと破棄する。
/// 上限超過時に破棄された範囲の再送は新規扱いで再計時される (保持期間外の再送とみなす)。
/// 30fps 換算で約 1 時間分に相当する 10 万件を採用する。
pub const MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION: usize = 100_000;

/// alias が「異なる Track」に既に使われているかを判定する
///
/// draft-ietf-moq-transport-21 §3.1.2 (Track Alias):
///
/// > The same Track Alias MUST NOT be used by a publisher to refer to two different Tracks
/// > simultaneously in the same session. If a subscriber receives a PUBLISH or SUBSCRIBE_OK
/// > that uses the same Track Alias as a different Track with an Established subscription,
/// > it MUST close the session with error DUPLICATE_TRACK_ALIAS.
///
/// 判定は Full Track Name (Track Namespace + Track Name) で行う。同一 Track なら §3.1 が
/// alias 共有を明示的に許可しているので衝突ではない。
///
/// `established_only` で 2 つの規範を撃ち分ける。
/// - 受信側 (`true`): 上記 2 文目どおり **Established** な subscription のみを対象とする。
///   Pending 状態の subscription との衝突は MUST の条件に入らない
/// - 送信側 (`false`): 1 文目の "simultaneously" に対応し、Terminated 以外を対象とする。
///   自側が違反メッセージを送出しないよう保守的に判定する
///
/// `exclude_request_id` は自分自身 (再確立や同一 request への再設定) を除外するために使う。
/// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
pub(super) fn alias_used_by_different_track(
    holders: &HashMap<u64, Vec<u64>>,
    subscriptions: &HashMap<u64, Subscription>,
    track_alias: u64,
    track_namespace: &TrackNamespace,
    track_name: &[u8],
    exclude_request_id: Option<u64>,
    established_only: bool,
) -> bool {
    let Some(ids) = holders.get(&track_alias) else {
        return false;
    };
    ids.iter().any(|id| {
        if Some(*id) == exclude_request_id {
            return false;
        }
        let Some(existing) = subscriptions.get(id) else {
            return false;
        };
        let state_matches = if established_only {
            existing.state == SubscriptionState::Established
        } else {
            existing.state != SubscriptionState::Terminated
        };
        state_matches
            && (existing.track_namespace != *track_namespace || existing.track_name != track_name)
    })
}

/// alias に紐づく request_id を追加する (重複追加はしない)
///
/// 呼び出し元が peer publisher 側の索引を扱う場合は、tombstone のクリアを忘れないよう
/// [`Session::register_peer_alias`](Session::register_peer_alias) を使うこと。
pub(super) fn insert_alias_holder(
    holders: &mut HashMap<u64, Vec<u64>>,
    track_alias: u64,
    request_id: u64,
) {
    let ids = holders.entry(track_alias).or_default();
    if !ids.contains(&request_id) {
        ids.push(request_id);
    }
}

/// alias から request_id を 1 つ外す
///
/// 残りが空になったら alias エントリ自体を削除し `true` を返す。共有 alias の
/// 他の subscription が残っている場合は `false` を返し、呼び出し側は
/// `remove_track_alias` などの alias 単位のクリーンアップを行ってはならない。
pub(super) fn remove_alias_holder(
    holders: &mut HashMap<u64, Vec<u64>>,
    track_alias: u64,
    request_id: u64,
) -> bool {
    let Some(ids) = holders.get_mut(&track_alias) else {
        return false;
    };
    ids.retain(|&id| id != request_id);
    if ids.is_empty() {
        holders.remove(&track_alias);
        return true;
    }
    false
}

#[derive(Debug)]
pub(super) struct NamespaceState {
    pub(super) publications: HashMap<u64, NamespacePublication>,
    pub(super) subscriptions: HashMap<u64, NamespaceSubscription>,
    /// request_id ごとの active な full Track Namespace (フィールド列) 集合
    ///
    /// draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions): prefix 更新後の
    /// NAMESPACE / NAMESPACE_DONE は新 prefix 相対になる。suffix 文字列だけでは prefix を跨いだ
    /// 同一性を判定できないため、受信時の prefix で解決した full namespace のフィールド列で
    /// 一意化し、重複 NAMESPACE の抑止と NAMESPACE_DONE の照合に使う。
    /// `NamespaceSubscription::active_suffixes` は現在の prefix 配下にある full namespace の
    /// suffix 投影である (投影から外れた full namespace も NAMESPACE_DONE の照合のため保持する)。
    pub(super) active_full_namespaces: HashMap<u64, HashSet<Vec<Vec<u8>>>>,
}

#[derive(Debug)]
pub(super) struct DataStreamState {
    pub(super) incoming: HashMap<DataStreamId, IncomingDataStream>,
    pub(super) outgoing: HashMap<DataStreamId, OutgoingDataStream>,
    /// 自端点が送信中の FETCH data stream (draft §11.4.1 (Fetch Header))
    ///
    /// FETCH_HEADER は Request ID のみを持ち subgroup stream とフィールド構成が異なるため、
    /// `outgoing` とは別の索引で管理する。
    pub(super) outgoing_fetch: HashMap<DataStreamId, super::data::OutgoingFetchStream>,
    /// 自端点が送信中の fill fetch stream (draft-ietf-moq-transport-21 §3.4 (Fill Semantics))
    ///
    /// FETCH_HEADER に subscription の Request ID を載せる点は FETCH 応答と共通のため
    /// [`super::data::OutgoingFetchStream`] を流用するが、紐づく `Fetch` 状態が存在せず
    /// subscription に direct に紐づく点が異なるため別の索引で管理する。
    /// 1 つの subscription に複数本が同時に開くことがある。
    pub(super) outgoing_fill: HashMap<DataStreamId, super::data::OutgoingFetchStream>,
    /// 破棄対象の受信 data stream id の保持集合。登録時点から `peer_alias_retention_ms`
    /// (既定値は [`DEFAULT_PEER_ALIAS_RETENTION_MS`]) と同じ期間だけ、破棄対象 stream の
    /// 受信・終端を no-op で吸収するために使う。対象はキャンセル由来 `Terminated`
    /// subscription に属する stream と、`forget_subscription` 後に届く新規 stream の
    /// 終端済み id である ([`Session::register_discarded_stream`] /
    /// [`Session::retain_discarded_stream_id`] 参照)。
    ///
    /// draft-ietf-moq-transport-21 §3.1.2 (Track Alias): "Objects can arrive after a
    /// subscription has been cancelled. Subscribers SHOULD retain sufficient state to
    /// quickly discard these unwanted Objects, rather than treating them as belonging
    /// to an unknown Track Alias."
    ///
    /// alias tombstone (`AliasState::peer_alias_tombstones`) には連動させない
    /// (共有 alias で `Established` subscription が残っている間は tombstone が
    /// 登録されず、連動すると掃除が発火しない)。
    pub(super) discarded: HashMap<DataStreamId, super::types::DeadlineTimer>,
}

/// GOAWAY 処理の状態
///
/// 拒否済み request id の集合 (`rejected_request_ids`) は GOING_AWAY 拒否専用ではなく
/// REQUEST_ERROR 拒否全般の共通経路のため `Session` 直下に置く
/// (request stream 上の GOAWAY は `local_sent` を立てないため、この struct の状態だけでは
/// GOING_AWAY 拒否は発生しない)。
#[derive(Debug)]
pub(super) struct GoawayState {
    pub(super) local_sent: bool,
    pub(super) request_stream_sent: HashSet<u64>,
    pub(super) request_stream_received: HashSet<u64>,
    pub(super) peer: Option<PeerGoawayInfo>,
    pub(super) local_deadline_ms: Option<u64>,
    pub(super) local_pending_timeout_ms: Option<u64>,
}

#[derive(Debug)]
pub(super) struct TimingState {
    pub(super) last_tick_ms: Option<u64>,
    pub(super) control_message_timeout_ms: Option<u64>,
    pub(super) control_message_deadlines: HashMap<u64, super::types::DeadlineTimer>,
    pub(super) data_stream_timeout_ms: Option<u64>,
    pub(super) data_stream_last_activity_ms: HashMap<DataStreamId, u64>,
    /// OBJECT_DELIVERY_TIMEOUT 追跡: (stream_id, object_id) → object header 提供完了時刻 (ms)
    /// None は tick 未到達で未確定であることを示す。最初の tick で現在時刻に確定する。
    /// draft-ietf-moq-transport-21 §5.2: "MUST retain the time at which the last header byte
    /// of every object has been either received from the upstream subscription, or provided
    /// by the original publisher application."
    /// Sans I/O 制約下では `send_subgroup_object` 呼び出し tick を object header 提供完了の近似とする。
    pub(super) object_delivery_header_complete_ms: HashMap<(DataStreamId, u64), Option<u64>>,
    /// SUBGROUP_DELIVERY_TIMEOUT 追跡: stream_id → (FIN 受信時刻 (ms), request_id)
    pub(super) subgroup_delivery_fin_ms: HashMap<DataStreamId, (Option<u64>, u64)>,
    /// Datagram OBJECT_DELIVERY_TIMEOUT 追跡: (request_id, group_id, object_id) → object header 提供完了時刻 (ms)
    /// None は tick 未到達で未確定であることを示す。最初の tick で現在時刻に確定する。
    /// draft-ietf-moq-transport-21 §5.2: "For datagrams, the implementation MUST drop the datagrams
    /// if the time elapsed exceeds OBJECT_DELIVERY_TIMEOUT." 起点は object header の最終バイト。
    /// Sans I/O 制約下では `send_object_datagram` 呼び出し tick を object header 提供完了の近似とする。
    pub(super) datagram_header_complete_ms: HashMap<(u64, u64, u64), Option<u64>>,
    /// `datagram_header_complete_ms` のうち未確定 (None) エントリのキー集合
    ///
    /// tick 時の確定走査を全エントリ走査ではなく Pending のみに限定するための索引。
    /// `send_object_datagram` で tick 未到達 (None) として作られたキーを登録し、
    /// tick 時に確定 (Some) して集合から除去する。確定済みエントリの tick 走査コストは O(1) になる。
    pub(super) datagram_pending_ms: HashSet<(u64, u64, u64)>,
    /// キャンセル済み peer publisher alias の tombstone 保持期間 (ms)
    ///
    /// draft-ietf-moq-transport-21 §3.1.2 (Track Alias) の SHOULD は「十分な期間」としか
    /// 述べておらず具体値を規定していないため、実装のデフォルトを
    /// [`DEFAULT_PEER_ALIAS_RETENTION_MS`] とし、アプリケーションが変更できるようにする。
    pub(super) peer_alias_retention_ms: u64,
}

#[derive(Debug)]
/// 1 本の `MOQT Transport Session` に閉じた endpoint-local な状態機械
///
/// I/O を持たない Sans I/O 設計で、`new_client` / `new_server` で生成し、
/// `poll_event` で取り出した指示を I/O 層が実行する。relay 固有の
/// routing / fan-out / cache / policy は含まない。
pub struct Session {
    pub(super) role: Role,
    pub(super) transport: Transport,
    pub(super) state: SessionState,
    pub(super) setup: SetupState,
    pub(super) auth: AuthState,
    pub(super) request_ids: RequestIdState,
    pub(super) subscriptions: HashMap<u64, Subscription>,
    pub(super) aliases: AliasState,
    /// Fetch の管理 (Request ID 索引)
    pub(super) fetches: HashMap<u64, Fetch>,
    pub(super) namespaces: NamespaceState,
    /// SUBSCRIBE_TRACKS の管理 (draft §9.18 (SUBSCRIBE_TRACKS))
    pub(super) track_subscriptions: HashMap<u64, TrackSubscription>,
    /// SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS の REQUEST_UPDATE で送信した
    /// TRACK_NAMESPACE_PREFIX の確定待ちキュー (request_id → 送信順)
    ///
    /// draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions): "If the update is accepted,
    /// NAMESPACE and NAMESPACE_DONE messages following the REQUEST_OK will contain Track
    /// Namespace suffixes relative to the updated prefix." REQUEST_OK を受信するまで
    /// ローカル prefix を更新せず、OK 受信時にキューの先頭から適用する。`None` は
    /// prefix 変更を含まない更新であり、REQUEST_OK との対応を送信順に保つために
    /// 1 件ずつ積む。REQUEST_ERROR 受信・bidi stream 終端・forget で破棄する。
    pub(super) pending_prefix_updates: HashMap<u64, VecDeque<Option<TrackNamespace>>>,
    /// TRACK_STATUS の管理
    pub(super) track_status_requests: HashMap<u64, TrackStatusEntry>,
    pub(super) data_streams: DataStreamState,
    /// peer publisher 由来 track ごとの object property 検証状態 (request_id → tracker)
    pub(super) peer_object_properties: HashMap<u64, ObjectPropertyTracker>,
    /// peer publisher 由来 track ごとの重複 Object フィールド一貫性追跡 (request_id → tracker)
    /// (draft §12.1 条件 6/7, §7.1)
    pub(super) peer_object_fields: HashMap<u64, ObjectFieldTracker>,
    /// peer publisher 由来 subgroup stream のライフサイクル追跡
    pub(super) peer_subgroups: SubgroupTracker,
    /// 自端点 publisher 由来 subgroup stream のライフサイクル追跡
    pub(super) my_subgroups: SubgroupTracker,
    /// bidi request stream の種別逆引きマップ (request_id → [`RequestKind`])
    ///
    /// draft-ietf-moq-transport-21 §6.3 (Session initialization) が規定する 7 種類の
    /// request について、`recv_request_stream_closed` など stream 単位の通知を
    /// dispatch するために使う。entry は各 request の生成時 (自側 `send_*` /
    /// 相手発行 `handle_peer_*`) に insert され、`forget_*` 系、および Subscribe / Publish の
    /// bidi stream 終端 (`close_subscription_on_stream_end`) と SUBSCRIBE_TRACKS の終端
    /// (`close_track_subscription_on_stream_end`) で remove される。
    pub(super) request_streams: HashMap<u64, RequestKind>,
    /// REQUEST_ERROR で拒否した、または peer のクローズ通知前に状態を破棄した request id の集合 (`request_streams` 未登録のもの)
    ///
    /// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection):
    /// "When an endpoint rejects a request without performing any application processing,
    /// it SHOULD send a REQUEST_ERROR and FIN the stream." 拒否された request は
    /// `request_streams` に登録されないため、peer がストリームを FIN / RESET_STREAM で
    /// 閉じると `recv_request_stream_closed` が unknown id として `PROTOCOL_VIOLATION` を
    /// 検出してしまう。拒否済み id のストリームクローズを no-op で吸収するために
    /// この集合を持つ (`emit_request_error` が `request_streams` 未登録の場合のみ記録する。
    /// GOAWAY 拒否 (draft §9.2) もこの共通経路で吸収される)。
    /// 登録済み request への REQUEST_ERROR (REQUEST_UPDATE 拒否等) は記録されない
    /// (クローズは `request_streams` 経由で処理される)。
    /// なお登録済み request への拒否 (fetch を `Terminated` に遷移させた上での
    /// REQUEST_ERROR) やアプリ主導の `send_request_error` は登録済みのため記録されず、
    /// アプリが peer のクローズ通知前に `forget_*` を呼ぶと遅延クローズが unknown id で
    /// fail するレースが残る (本変更前から存在する既知の挙動)。fetch の終端済み
    /// (publisher 側・データストリーム終端済み。FIN / RESET の両方の終端通知を含む)
    /// 破棄だけは `forget_fetch` が本集合へ記録して遅延クローズを吸収する。
    /// SUBSCRIBE_TRACKS の bidi stream 終端で暗黙終端した subscription のうち、まだ close
    /// 通知を受けていないものだけを `close_track_subscription_on_stream_end` が本集合へ記録し、
    /// 後続の PUBLISH bidi stream クローズを吸収する。malformed 終端も同様に
    /// `terminate_malformed_track` が記録する。
    /// クローズ通知の受信時に削除される。peer がクローズ通知を送らない場合は
    /// セッション生存中に残り続ける (サイズは「拒否後・破棄済みのうち未クローズの id 数」に比例する)。
    pub(super) rejected_request_ids: HashSet<u64>,
    pub(super) goaway: GoawayState,
    pub(super) timing: TimingState,
    /// request stream ごとの自側送信 outstanding REQUEST_UPDATE 数 (draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES))
    pub(super) outgoing_request_updates: HashMap<u64, u64>,
    /// request stream ごとの peer 送信 outstanding REQUEST_UPDATE 数 (draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES))
    pub(super) incoming_request_updates: HashMap<u64, u64>,
    /// request_id ごとの「STOP_SENDING を受けた outgoing Subgroup」集合
    ///
    /// draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams): "A publisher that
    /// receives a STOP_SENDING on a Subgroup stream SHOULD NOT attempt to open a new stream
    /// to deliver additional Objects in that Subgroup. However, if the publisher subsequently
    /// receives a REQUEST_UPDATE that changes the Forward State from 0 to 1, it MAY open a
    /// new stream ..." の再オープン禁止状態を保持する。値のタプル
    /// `(track_alias, group_id, subgroup_id)` の 3 番目の `Option<u64>` は FirstObjectId
    /// モードの未解決 subgroup_id (`None`) を表す。終端状態 (`StoppedByPeer` / `Reset`) の
    /// 上書きに影響されず、Forward State 0→1 の REQUEST_UPDATE が受理された時点または
    /// `forget_subscription` で破棄する (共有 alias の兄弟 subscription が残っていても
    /// 所有者の破棄でエントリは消えるため、STOP_SENDING による再オープン禁止は適用されなく
    /// なる。FIN で正常終了した Subgroup は tracker 側で引き続き再オープン不可)。なお
    /// REQUEST_UPDATE の受信から REQUEST_OK の送信までに記録された STOP_SENDING も
    /// 解除対象になる (順序の近似)。
    ///
    /// PUBLISH 起点 (自側 publisher) の subscription では、現状 peer subscriber の
    /// REQUEST_UPDATE に応答する経路がなく (`send_ok_for_subscription` は responder 専用)、
    /// Forward 0→1 による解除は行われない (既知の制限。SUBSCRIBE 起点では動作する)。
    pub(super) stopped_outgoing_subgroups: HashMap<u64, HashSet<(u64, u64, Option<u64>)>>,
    pub(super) events: VecDeque<SessionEvent>,
}

/// PUBLISH_DONE Stream Count の sentinel 値 (`2^64 - 1` = `u64::MAX`)
///
/// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE): publisher が subscription
/// に対して開いたデータストリーム数 (空 Subgroup を含み、fill fetch streams を
/// 含む) を正確に提供できない場合、`Stream Count` を `2^64 - 1` にセットする
/// (MUST)。stream を 1 本も開かなかった場合は MUST 0。subscriber は本値を受信
/// した場合に stream 数の比較を行わず、timeout 等で subscription state を破棄
/// することが期待される (SHOULD)。
///
/// なお現実装の `published_count` tracking は subgroup stream と fill fetch stream
/// (draft-ietf-moq-transport-21 §3.4) の両方を数える。
///
/// 将来 draft 改定で値が変更される可能性があるため、ユーザは数値直書きでは
/// なく本定数を参照すること。
pub const PUBLISH_DONE_STREAM_COUNT_UNKNOWN: u64 = u64::MAX;

/// request_id が属するテーブルの種別 (内部 dispatch 用)
///
/// 6 種類のテーブル (subscriptions / fetches / namespace_publications /
/// namespace_subscriptions / track_subscriptions / track_status_requests) のどれに
/// 属するかを識別する。`send_request_ok` / `send_request_error` /
/// `handle_peer_request_ok` / `handle_peer_request_error` の dispatch に使用する。
///
/// 公開 API 用の種別は [`super::types::RequestKind`] (bidi request stream の
/// 開始メッセージ種別) を参照。両者は役割が異なる (前者は内部の state テーブル
/// 単位、後者は bidi request stream の開始メッセージ単位で SUBSCRIBE と
/// PUBLISH を区別する)。`RequestKind` からは [`RequestTable::from_kind`] で
/// 一方向に変換する。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum RequestTable {
    Subscription,
    Fetch,
    NamespacePublication,
    NamespaceSubscription,
    TrackSubscription,
    TrackStatus,
}

impl RequestTable {
    /// bidi request stream の開始メッセージ種別 (`RequestKind`) から state テーブル単位へ
    /// 変換する一方向の対応。`Subscribe` / `Publish` は同じ `Subscription` テーブルに
    /// 対応する (多対一) ため、逆変換は定義しない。
    pub(super) fn from_kind(kind: RequestKind) -> Self {
        match kind {
            RequestKind::Subscribe | RequestKind::Publish => Self::Subscription,
            RequestKind::Fetch => Self::Fetch,
            RequestKind::PublishNamespace => Self::NamespacePublication,
            RequestKind::SubscribeNamespace => Self::NamespaceSubscription,
            RequestKind::SubscribeTracks => Self::TrackSubscription,
            RequestKind::TrackStatus => Self::TrackStatus,
        }
    }
}

impl Session {
    /// Client セッションを作成する
    ///
    /// 成功時は自側の SETUP を [`SessionEvent::SendControl`] として発行し、
    /// 状態を [`SessionState::LocalSetupSent`] にする。
    pub fn new_client(transport: Transport, options: SetupOptions) -> Result<Self, SessionError> {
        Self::new(Role::Client, transport, options)
    }

    /// Server セッションを作成する
    ///
    /// draft §6.3 (Session initialization): 各エンドポイントは自側 control stream を開いて SETUP から始める。
    /// 0-RTT の有無に応じて Client / Server どちらが先にデータを送るかが変わるため、
    /// Server も作成直後に SETUP を発行可能とする (I/O 層がトランスポートに応じて送信する)。
    pub fn new_server(transport: Transport, options: SetupOptions) -> Result<Self, SessionError> {
        Self::new(Role::Server, transport, options)
    }

    fn new(role: Role, transport: Transport, options: SetupOptions) -> Result<Self, SessionError> {
        // draft §9.1.1 (AUTHORITY), §9.1.2 (PATH): 自側 SETUP に違反がないか確認
        validate_setup_role_transport(&options, role, transport)?;
        validate_setup_uri_format(&options)?;

        // draft §9.20.3 (AUTHORIZATION TOKEN Parameter): 自側 SETUP 内の AUTHORIZATION_TOKEN の alias 重複のみ事前検出する。
        // サイズ判定 (MAX_AUTH_TOKEN_CACHE_SIZE) は受信側の上限に従うため、自側が送った
        // REGISTER が peer の cache に実際収まるかは peer SETUP 受信時まで確定しない
        // (draft §9.1.4 (AUTHORIZATION TOKEN) 最終段落)。自側 cache 自体は保持しない
        // (内容を読む処理が存在しないため) が、重複検出のためだけに使い捨ての cache へ登録する。
        let mut validation_cache = AuthTokenCache::new(u64::MAX);
        register_setup_auth_tokens(&options, &mut validation_cache)?;

        let setup = Setup { options };
        let mut events = VecDeque::new();
        events.push_back(SessionEvent::SendControl(ControlMessage::Setup(
            setup.clone(),
        )));
        let peer_role = match role {
            Role::Client => Role::Server,
            Role::Server => Role::Client,
        };
        Ok(Self {
            role,
            transport,
            state: SessionState::LocalSetupSent,
            setup: SetupState {
                local: Some(setup),
                peer: None,
            },
            auth: AuthState {
                peer_token_cache: AuthTokenCache::new(0),
            },
            request_ids: RequestIdState {
                local_generator: RequestIdGenerator::new(role),
                peer_tracker: RequestIdTracker::new(peer_role),
            },
            subscriptions: HashMap::new(),
            aliases: AliasState {
                my_publisher_aliases: HashMap::new(),
                peer_publisher_aliases: HashMap::new(),
                subscriptions_by_track: HashMap::new(),
                peer_alias_tombstones: HashMap::new(),
            },
            fetches: HashMap::new(),
            namespaces: NamespaceState {
                publications: HashMap::new(),
                subscriptions: HashMap::new(),
                active_full_namespaces: HashMap::new(),
            },
            track_subscriptions: HashMap::new(),
            pending_prefix_updates: HashMap::new(),
            track_status_requests: HashMap::new(),
            data_streams: DataStreamState {
                incoming: HashMap::new(),
                outgoing: HashMap::new(),
                outgoing_fetch: HashMap::new(),
                outgoing_fill: HashMap::new(),
                discarded: HashMap::new(),
            },
            peer_object_properties: HashMap::new(),
            peer_object_fields: HashMap::new(),
            peer_subgroups: SubgroupTracker::new(),
            my_subgroups: SubgroupTracker::new(),
            request_streams: HashMap::new(),
            rejected_request_ids: HashSet::new(),
            goaway: GoawayState {
                local_sent: false,
                request_stream_sent: HashSet::new(),
                request_stream_received: HashSet::new(),
                peer: None,
                local_deadline_ms: None,
                local_pending_timeout_ms: None,
            },
            timing: TimingState {
                last_tick_ms: None,
                control_message_timeout_ms: None,
                control_message_deadlines: HashMap::new(),
                data_stream_timeout_ms: None,
                data_stream_last_activity_ms: HashMap::new(),
                object_delivery_header_complete_ms: HashMap::new(),
                subgroup_delivery_fin_ms: HashMap::new(),
                datagram_header_complete_ms: HashMap::new(),
                datagram_pending_ms: HashSet::new(),
                peer_alias_retention_ms: DEFAULT_PEER_ALIAS_RETENTION_MS,
            },
            outgoing_request_updates: HashMap::new(),
            incoming_request_updates: HashMap::new(),
            stopped_outgoing_subgroups: HashMap::new(),
            events,
        })
    }

    /// 現在のセッション状態
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// エンドポイントの役割
    pub fn role(&self) -> Role {
        self.role
    }

    /// 下位トランスポート種別
    pub fn transport(&self) -> Transport {
        self.transport
    }

    /// 相手側 Alias Cache (peer が REGISTER したトークンで、自側が保持するもの)
    ///
    /// `max_size()` は自側の `MAX_AUTH_TOKEN_CACHE_SIZE` (draft §9.1.3 (MAX_AUTH_TOKEN_CACHE_SIZE)) で
    /// あり、peer の宣言値ではない。peer SETUP を受信すると自側 SETUP の宣言値で確定し、
    /// 受信前は 0 になる。自側が REGISTER した alias を peer が保持できる上限は
    /// `peer_max_auth_token_cache_size()` で取得する。
    ///
    /// 期限切れ token の検出はアプリ責務である。`resolve` は draft-ietf-moq-transport-21 §8.9
    /// (Authorization Token Compression): "If a receiver detects that an authorization token
    /// has expired, it MUST retain the registered Alias until it is deleted by the sender"
    /// のため期限切れ後も alias を DELETE まで解決し続ける。アプリが期限切れを検出したときは、
    /// 受信 request 文脈は `send_request_error` に `REQUEST_EXPIRED_AUTH_TOKEN`、SETUP などの
    /// セッション文脈は `close` に `SESSION_EXPIRED_AUTH_TOKEN`、data stream 文脈は
    /// `reset_outgoing_data_stream` に `DataStreamResetReason::ExpiredAuthToken`
    /// (コードは `STREAM_EXPIRED_AUTH_TOKEN`) を渡す。
    /// 詳細は [`AuthTokenCache`] の doc を参照。
    pub fn peer_auth_token_cache(&self) -> &AuthTokenCache {
        &self.auth.peer_token_cache
    }

    /// peer が SETUP で宣言した `MAX_AUTH_TOKEN_CACHE_SIZE` を返す
    ///
    /// draft-ietf-moq-transport-21 §9.1.3 (MAX_AUTH_TOKEN_CACHE_SIZE): 自側が REGISTER した
    /// token の合計サイズ (バイト) を peer が保持できる上限。peer SETUP 未受信時は暫定値として
    /// 0 を返し、peer SETUP 受信後に未宣言だった場合の 0 が仕様のデフォルト値である。
    /// 両者は値では区別できないため、アプリの purge 判断は peer SETUP 受信後
    /// (`SessionEvent::Established` の受信時など) に行う (どちらの 0 も自側が peer に対して
    /// token Alias を使用できないことを意味する)。
    /// draft-ietf-moq-transport-21 §9.1.4 (AUTHORIZATION TOKEN): "the sender MUST handle
    /// registration failures of this kind by purging any Token Aliases that failed to register
    /// based on the peer's MAX_AUTH_TOKEN_CACHE_SIZE option in SETUP (or the default value of 0)."
    /// ライブラリはメッセージを組み立てないため、アプリがこの値と容量計算
    /// (draft §9.1.3: Token 1 つあたり 16 バイト + Token Value のバイト数。登録分を合算し
    /// DELETE 分を減算する) を突き合わせ、peer の MAX に収まらない alias は register 失敗として
    /// purge し、以後その alias を使うメッセージを USE_VALUE へフォールバックする。
    /// draft-ietf-moq-transport-21 §8.9 (Authorization Token Compression): "Once a Token Alias
    /// has been registered, it cannot be re-registered by the same endpoint in the Session
    /// without first being deleted." のため、register 失敗と判断した alias は自側が当該 alias の
    /// DELETE を送って peer の登録を解除するまで再 REGISTER しない (peer 側で実際には登録が
    /// 成立していた場合、再 REGISTER は `DUPLICATE_AUTH_TOKEN_ALIAS` でセッションを閉じる。
    /// SETUP は DELETE を運べないため、解除はメッセージパラメータの DELETE で行う)。
    pub fn peer_max_auth_token_cache_size(&self) -> u64 {
        self.setup
            .peer
            .as_ref()
            .and_then(|s| s.options.max_auth_token_cache_size())
            .unwrap_or(0)
    }

    /// 制御メッセージ応答待ちタイムアウト (ms) を取得する (draft-ietf-moq-transport-21 §6.6 (Termination))
    pub fn control_message_timeout_ms(&self) -> Option<u64> {
        self.timing.control_message_timeout_ms
    }

    /// 制御メッセージ応答待ちタイムアウト (ms) を設定する (draft-ietf-moq-transport-21 §6.6 (Termination))
    ///
    /// `None` で無効化する。設定変更後に既に開始済みの deadline は影響を受けない。
    pub fn set_control_message_timeout_ms(&mut self, timeout_ms: Option<u64>) {
        self.timing.control_message_timeout_ms = timeout_ms;
    }

    /// Data Stream タイムアウト (ms) を取得する (draft-ietf-moq-transport-21 §6.6 (Termination))
    pub fn data_stream_timeout_ms(&self) -> Option<u64> {
        self.timing.data_stream_timeout_ms
    }

    /// Data Stream タイムアウト (ms) を設定する (draft-ietf-moq-transport-21 §6.6 (Termination))
    ///
    /// `None` で無効化する。設定変更後に既に記録済みの activity 時刻は影響を受けない。
    pub fn set_data_stream_timeout_ms(&mut self, timeout_ms: Option<u64>) {
        self.timing.data_stream_timeout_ms = timeout_ms;
    }

    /// 次の [`SessionEvent`] を取り出す
    ///
    /// 呼び出し側は `None` が返るまで繰り返し呼び出して I/O 層と同期する。
    pub fn poll_event(&mut self) -> Option<SessionEvent> {
        let event = self.events.pop_front();
        // CloseSession を取り出したタイミングで Closing → Closed
        if matches!(event, Some(SessionEvent::CloseSession(_)))
            && self.state == SessionState::Closing
        {
            self.state = SessionState::Closed;
        }
        event
    }

    /// 受信した制御メッセージを処理する
    ///
    /// 制御ストリーム上で受け取れるのは SETUP と GOAWAY のみ (draft §9.1 (SETUP) / §9.2 (GOAWAY))。
    /// それ以外のメッセージは PROTOCOL_VIOLATION としてクローズする。
    ///
    /// クローズ処理中 / クローズ済みの呼び出しは `Ok(())` で no-op。
    pub fn recv_control(&mut self, msg: ControlMessage) -> Result<(), SessionError> {
        if matches!(self.state, SessionState::Closing | SessionState::Closed) {
            return Ok(());
        }
        match msg {
            ControlMessage::Setup(setup) => self.handle_peer_setup(setup),
            ControlMessage::Goaway(g) => self.handle_peer_goaway(g),
            _ => {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "unsupported control stream message",
                );
                self.fail(err.clone());
                Err(err)
            }
        }
    }

    /// peer から受け取った control stream の stream type を検証する
    ///
    /// draft §6.4.1 (Unidirectional Streams): control stream の stream type は
    /// `SETUP_STREAM_TYPE` (0x2F00) でなければならない。
    /// 受信した stream type が異なる場合は `PROTOCOL_VIOLATION` でセッションを閉じる。
    ///
    /// クローズ処理中 / クローズ済みの呼び出しは no-op。
    pub fn recv_control_stream_type(&mut self, stream_type: u64) -> Result<(), SessionError> {
        if matches!(self.state, SessionState::Closing | SessionState::Closed) {
            return Ok(());
        }
        if stream_type != SETUP_STREAM_TYPE {
            let err =
                SessionError::new(SESSION_PROTOCOL_VIOLATION, "unexpected control stream type");
            self.fail(err.clone());
            return Err(err);
        }
        Ok(())
    }

    /// peer control stream の終端 (FIN / RESET_STREAM) を session に通知する
    ///
    /// draft-ietf-moq-transport-21 §6.3 (Session initialization): control stream は
    /// session の lifetime 中に閉じてはならない。SETUP 交換中
    /// (`LocalSetupSent`) であっても、transport 層で peer control stream が
    /// 閉じた時点で `PROTOCOL_VIOLATION` として扱う。
    ///
    /// クローズ処理中 / クローズ済みの呼び出しは `Ok(())` で no-op。
    pub fn recv_control_stream_closed(
        &mut self,
        end: RequestStreamEnd,
    ) -> Result<(), SessionError> {
        if matches!(self.state, SessionState::Closing | SessionState::Closed) {
            return Ok(());
        }
        let err = match end {
            RequestStreamEnd::Fin => SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "peer control stream closed with FIN",
            ),
            RequestStreamEnd::Reset { .. } => {
                SessionError::new(SESSION_PROTOCOL_VIOLATION, "peer control stream reset")
            }
        };
        self.fail(err.clone());
        Err(err)
    }

    /// セッションを明示的にクローズする
    ///
    /// [`SessionEvent::CloseSession`] を発行し、状態を [`SessionState::Closing`] にする。
    /// 既に Closing / Closed の場合は何もしない。
    /// ローカル専用コード (`SESSION_LOCAL_FILTER_MISMATCH` / `SESSION_LOCAL_DATAGRAM_TIMEOUT`) を
    /// 渡した場合は `SESSION_INTERNAL_ERROR` に置換し、未登録値を wire に出さない。
    pub fn close(&mut self, code: u64, reason: &'static str) {
        // 公開 API の契約として、ローカル専用コードを Session Termination レジストリの
        // INTERNAL_ERROR に置換する (`fail` の最終防御とは独立に行う)
        let code = if is_local_error_code(code) {
            SESSION_INTERNAL_ERROR
        } else {
            code
        };
        self.fail(SessionError::new(code, reason));
    }

    /// パディングストリームを送信する (draft §11.5.1 (Padding Streams))
    ///
    /// I/O 層は stream type `PADDING_STREAM_TYPE` の単方向ストリームを開き、
    /// `length` バイトの 0x00 を書き込んで閉じる。
    ///
    /// §11.5.1 は "The stream begins with the stream type, followed by zero or more
    /// bytes that MUST all be set to zero." と全バイト 0x00 を MUST で要求する。
    /// Sans I/O の Session はペイロードのバイト列を保持しないため
    /// ([`SessionEvent::SendPaddingStream`] が持つのは `length` だけ)、
    /// この MUST を満たす責務は I/O 層にあり Session 側の検証は不要である。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn send_padding_stream(&mut self, length: u64) -> Result<(), SessionError> {
        self.require_established()?;
        self.events
            .push_back(SessionEvent::SendPaddingStream { length });
        Ok(())
    }

    /// パディングデータグラムを送信する (draft §11.5.2 (Padding Datagrams))
    ///
    /// I/O 層は datagram type `PADDING_DATAGRAM_TYPE` を先頭 varint として、
    /// `length` バイトの 0x00 をペイロードとするデータグラムを送信する。
    ///
    /// §11.5.2 は "The datagram contains the type followed by zero or more bytes that
    /// MUST all be set to zero." と全バイト 0x00 を MUST で要求する。
    /// [`send_padding_stream`](Self::send_padding_stream) と同じ理由で、この MUST を
    /// 満たす責務は I/O 層にある。
    pub fn send_padding_datagram(&mut self, length: u64) -> Result<(), SessionError> {
        self.require_established()?;
        self.events
            .push_back(SessionEvent::SendPaddingDatagram { length });
        Ok(())
    }

    /// 自側の次の Request ID を発行する (draft §6.4.2.1 (Request ID))
    ///
    /// `Established` 状態でのみ呼び出し可能。それ以外の状態では `PROTOCOL_VIOLATION` を返す
    /// (セッションを閉じる副作用はない。純粋な API バリデーション)。
    pub fn next_local_request_id(&mut self) -> Result<u64, SessionError> {
        if self.state != SessionState::Established {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "next_local_request_id called before session established",
            ));
        }
        Ok(self.request_ids.local_generator.next_id())
    }

    /// peer からの request stream の先頭メッセージを受けたときの検証を行う
    ///
    /// - `request_ids.peer_tracker.accept(request_id)` で parity と重複を検証 (draft §6.4.2.1 (Request ID))
    /// - control GOAWAY 送信済み (`local_sent`) なら REQUEST_ERROR(GOING_AWAY) を送信し
    ///   `Ok(false)` を返す (draft §9.2 (GOAWAY))
    ///
    /// GOAWAY 拒否経路では REQUEST_ERROR を発行する前に、当該 request のパラメータに
    /// 含まれる AUTHORIZATION_TOKEN の **REGISTER のみ** を `peer_token_cache` に反映させる
    /// (draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter):
    /// "The receiver of a message carrying an AUTHORIZATION TOKEN with Alias Type REGISTER
    /// that does not result in a Session error MUST register the Token Alias, in the token
    /// cache, even if the message fails for other reasons, including Unauthorized." (MUST))。
    /// `REQUEST_GOING_AWAY` は request error であって session error ではないため、
    /// REGISTER の MUST が適用される。DELETE / USE_ALIAS / USE_VALUE は「失敗した request の
    /// パラメータ」として peer 側で not-applied 扱いされる可能性があるため触らない
    /// (`apply_peer_message_auth_token_registers_only` の doc 参照)。
    /// REGISTER 適用が session error (`AUTH_TOKEN_CACHE_OVERFLOW` /
    /// `DUPLICATE_AUTH_TOKEN_ALIAS`) になった場合は、session error > GOING_AWAY 優先で
    /// `Err` を返し、REQUEST_ERROR は発行しない。
    /// 受理経路 (Ok(true)) では従来どおり呼び出し側 handler が `apply_peer_message_auth_tokens`
    /// を呼ぶ (二重登録を避けるため本関数では適用しない)。
    ///
    /// 致命エラー時は `fail` を呼んで session を Closing に遷移させ、
    /// エラーを返す。呼び出し側は `?` で伝播すればよい。
    ///
    /// 戻り値:
    /// - `Ok(true)`: 受理された
    /// - `Ok(false)`: GOAWAY 送信済みのため REQUEST_ERROR(GOING_AWAY) を送信済み (セッションは継続)
    /// - `Err(SessionError)`: parity/重複、または REGISTER 適用時の session error
    ///   (セッションは Closing に遷移)
    pub(super) fn accept_peer_request(
        &mut self,
        request_id: u64,
        parameters: &MessageParameters,
    ) -> Result<bool, SessionError> {
        // draft §6.4.2.1 (Request ID): parity 不正・重複は無条件で
        // INVALID_REQUEST_ID クローズ。GOING_AWAY 拒否より優先。
        if let Err(err) = self.request_ids.peer_tracker.accept(request_id) {
            self.fail(err.clone());
            return Err(err);
        }
        // draft §9.2 (GOAWAY): control GOAWAY を送信した側は
        // GOAWAY 後に到着する新規 request を GOING_AWAY で MAY 拒否。
        // REQUEST_ERROR は SendOnStream イベントとして発行し、自側の送信方向の FIN は
        // I/O 層が閉じる (draft §6.4.2.3 の SHOULD)。
        // 拒否済み id の記録は `emit_request_error` が共通経路で行う。
        if self.goaway.local_sent {
            // draft §9.20.3 の REGISTER MUST を GOING_AWAY より先に適用する。
            // MUST の対象は REGISTER のみ (DELETE / USE_ALIAS / USE_VALUE は failed request の
            // パラメータとして触らない)。REGISTER 適用中に session error が発生した場合は
            // session error 優先で Err を返し、REQUEST_ERROR は発行しない。
            self.apply_peer_message_auth_token_registers_only(parameters)?;
            self.emit_request_error(
                request_id,
                crate::error::REQUEST_GOING_AWAY,
                "peer request arrived after GOAWAY sent",
            );
            return Ok(false);
        }
        Ok(true)
    }

    /// bidi request stream の最初のメッセージを受信する
    ///
    /// draft-ietf-moq-transport-21 §6.3 (Session initialization): request stream の開始メッセージ
    /// として許されるのは SUBSCRIBE / PUBLISH / FETCH / PUBLISH_NAMESPACE /
    /// SUBSCRIBE_NAMESPACE / TRACK_STATUS / SUBSCRIBE_TRACKS の 7 種類のいずれか。
    ///
    /// draft-ietf-moq-transport-21 §6.4.2.1 (Request ID): peer の Request ID の parity 違反と
    /// 重複を検証し、違反時は `INVALID_REQUEST_ID` でセッションを `Closing` に遷移させる
    /// (検証は各ハンドラ先頭の `accept_peer_request` が 1 メッセージにつき 1 回行う)。
    /// また draft 由来ではない実装保護として、未到達 Request ID の保持上限
    /// (`MAX_OUT_OF_ORDER_REQUEST_IDS`) 超過でも同じ `INVALID_REQUEST_ID` で閉じる。
    ///
    /// draft §6.3 (Session initialization): SETUP 完了前に request stream が到着することは
    /// 許容される。本 API は session state を変えずに
    /// `RecvRequestError::BeforeSessionEstablished` を返すので、呼び出し側は
    /// buffer して後で再投入するか、その bidi stream だけを RESET_STREAM で
    /// 閉じるかを選べる。将来 draft が変更される可能性がある。
    ///
    /// 上記 7 種類以外の message type (draft §6.3 (Session initialization) 違反) は従来どおり
    /// `PROTOCOL_VIOLATION` で session を closing に遷移させる。
    pub fn recv_request(&mut self, msg: ControlMessage) -> Result<(), RecvRequestError> {
        if self.state != SessionState::Established {
            // draft §6.3 (Session initialization): SETUP 完了前の前置到着は session を落とさない。
            return Err(RecvRequestError::BeforeSessionEstablished);
        }
        match msg {
            ControlMessage::Subscribe(subscribe) => self.handle_peer_subscribe(subscribe)?,
            ControlMessage::Publish(publish) => {
                let track_alias = publish.track_alias;
                let track_namespace = publish.track_namespace.clone();
                if self.handle_peer_publish(publish)? {
                    // SUBSCRIBE_TRACKS 経由の PUBLISH の場合、対応する subscriber role の
                    // TrackSubscription を track_namespace の片方向 prefix matching で検索して
                    // active_track_aliases を更新する。REQUEST_UPDATE で送信した prefix は
                    // REQUEST_OK 受信までローカルへ反映せず、PUBLISH は REQUEST_UPDATE とは
                    // 別 bidi stream で順序保証がない。そのため確定待ちの間は旧 prefix と
                    // 確定待ち prefix の両方でマッチさせる
                    // (draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions))。
                    for (_, ts) in self.track_subscriptions.iter_mut() {
                        if ts.my_role != TrackRole::Subscriber {
                            continue;
                        }
                        let pending_matches = self
                            .pending_prefix_updates
                            .get(&ts.request_id)
                            .is_some_and(|queue| {
                                queue
                                    .iter()
                                    .flatten()
                                    .any(|prefix| is_prefix_of(prefix, &track_namespace))
                            });
                        if pending_matches || is_prefix_of(&ts.prefix, &track_namespace) {
                            ts.active_track_aliases.insert(track_alias);
                        }
                    }
                }
            }
            ControlMessage::Fetch(fetch) => self.handle_peer_fetch(fetch)?,
            ControlMessage::PublishNamespace(m) => self.handle_peer_publish_namespace(m)?,
            ControlMessage::SubscribeNamespace(m) => self.handle_peer_subscribe_namespace(m)?,
            ControlMessage::SubscribeTracks(m) => self.handle_peer_subscribe_tracks(m)?,
            ControlMessage::TrackStatus(m) => self.handle_peer_track_status(m)?,
            _ => {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "unsupported request message in current phase",
                );
                self.fail(err.clone());
                return Err(RecvRequestError::Session(err));
            }
        }
        Ok(())
    }

    /// bidi request stream の終端 (FIN / RESET_STREAM) を session に通知する
    ///
    /// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection): request の
    /// cancel / reject はストリーム方向の終端で表現される。本 API は state machine を
    /// `Terminated` 相当に進め、
    /// `SessionEvent::RequestTerminated { request_id, kind, reason }` を発行する。
    /// 呼び出し側は後続で `forget_*` を呼んでマップから除去する責務を持つ。
    ///
    /// 不明な `request_id` (既に forget 済みの場合を含む) は `PROTOCOL_VIOLATION` で
    /// session を `Closing` に遷移させる。ただし `request_streams` 未登録のまま
    /// REQUEST_ERROR で拒否した request id (draft-ietf-moq-transport-21 §6.4.2.3 "When an
    /// endpoint rejects a request without performing any application processing, it SHOULD
    /// send a REQUEST_ERROR and FIN the stream." に従う拒否。control GOAWAY 送信後の
    /// GOING_AWAY 拒否 (draft §9.2) も含む) と、終端済み fetch の破棄
    /// (`forget_fetch` が記録する publisher 側・データストリーム終端済みの request id。
    /// 詳細は `rejected_request_ids` のフィールド doc 参照)、および SUBSCRIBE_TRACKS の
    /// bidi stream 終端で暗黙終端した subscription (`close_track_subscription_on_stream_end`
    /// が記録する request id) のストリームクローズは
    /// 拒否済み・破棄済み・暗黙終端済みのため state を持たず no-op で吸収する。登録済み request への
    /// REQUEST_ERROR (REQUEST_UPDATE 拒否等) のクローズは `request_streams` 経由で処理され、
    /// `RequestTerminated` が発行される。
    /// close 通知は 1 回のみを想定しており、2 回目以降の通知は unknown id として
    /// `PROTOCOL_VIOLATION` になる。将来 draft が変更される可能性がある。
    pub fn recv_request_stream_closed(
        &mut self,
        request_id: u64,
        end: RequestStreamEnd,
    ) -> Result<(), SessionError> {
        self.clear_control_message_deadline(request_id);
        let Some(kind) = self.request_streams.get(&request_id).copied() else {
            // REQUEST_ERROR 拒否済み request id のクローズは通常フローであり
            // (draft §6.4.2.2: bidi の各方向は独立に閉じる。拒否する側は REQUEST_ERROR +
            // FIN を送る (SHOULD, §6.4.2.3) ため peer も自分の送信方向を閉じる)、
            // プロトコル違反ではない。state を持たないため集合から削除して no-op で返す。
            if self.rejected_request_ids.remove(&request_id) {
                return Ok(());
            }
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "bidi request stream close for unknown request id",
            );
            self.fail(err.clone());
            return Err(err);
        };
        // 一方向変換 (`RequestKind` -> `RequestTable`) を 1 箇所に集約し、他の request 応答の
        // dispatch と同じ state テーブル単位で分岐する
        let reason_result = match RequestTable::from_kind(kind) {
            RequestTable::Subscription => self.close_subscription_on_stream_end(request_id, end),
            RequestTable::Fetch => self.close_fetch_on_stream_end(request_id, end),
            RequestTable::NamespacePublication => {
                self.close_namespace_publication_on_stream_end(request_id, end)
            }
            RequestTable::NamespaceSubscription => {
                self.close_namespace_subscription_on_stream_end(request_id, end)
            }
            RequestTable::TrackSubscription => {
                self.close_track_subscription_on_stream_end(request_id, end)
            }
            RequestTable::TrackStatus => self.close_track_status_on_stream_end(request_id, end),
        };
        let reason = match reason_result {
            Ok(r) => r,
            Err(err) => {
                self.fail(err.clone());
                return Err(err);
            }
        };
        self.events.push_back(SessionEvent::RequestTerminated {
            request_id,
            kind,
            reason,
        });
        Ok(())
    }

    /// 既存 bidi request stream 上の応答メッセージを受信する
    ///
    /// `request_id` は I/O 層が bidi stream に紐付けて記憶した値。
    pub fn recv_stream_message(
        &mut self,
        request_id: u64,
        msg: ControlMessage,
    ) -> Result<(), SessionError> {
        if self.state != SessionState::Established {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "response received before session established",
            );
            self.fail(err.clone());
            return Err(err);
        }
        match msg {
            ControlMessage::SubscribeOk(ok) => self.handle_peer_subscribe_ok(request_id, ok),
            ControlMessage::RequestError(err) => self.handle_peer_request_error(request_id, err),
            ControlMessage::RequestUpdate(update) => {
                self.handle_peer_request_update(request_id, update)
            }
            ControlMessage::RequestOk(ok) => self.handle_peer_request_ok(request_id, ok),
            ControlMessage::PublishDone(done) => self.handle_peer_publish_done(request_id, done),
            ControlMessage::PublishStateNotify(notify) => {
                self.handle_peer_publish_state_notify(request_id, notify)
            }
            ControlMessage::FetchOk(ok) => self.handle_peer_fetch_ok(request_id, ok),
            ControlMessage::Namespace(m) => self.handle_peer_namespace(request_id, m),
            ControlMessage::NamespaceDone(m) => self.handle_peer_namespace_done(request_id, m),
            ControlMessage::Publish(_) => {
                // draft-ietf-moq-transport-21 §9 Table 5: PUBLISH (0x1D) は Request, First。
                // "Messages marked 'First' MUST be the first message on a new request stream."
                // 既存 bidi stream 上の PUBLISH はプロトコル違反。
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "PUBLISH MUST be the first message on a new request stream (draft §9 Table 5)",
                );
                self.fail(err.clone());
                Err(err)
            }
            ControlMessage::PublishSkipped(m) => self.handle_peer_publish_skipped(request_id, m),
            ControlMessage::Goaway(g) => self.handle_peer_goaway_on_request_stream(request_id, g),
            _ => {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "unsupported stream message in current phase",
                );
                self.fail(err.clone());
                Err(err)
            }
        }
    }

    /// REQUEST_OK を送信する (responder 側のみ)
    ///
    /// draft §9.3 (REQUEST_OK): REQUEST_OK は PUBLISH / REQUEST_UPDATE / TRACK_STATUS /
    /// SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS / PUBLISH_NAMESPACE への成功応答として送信される。
    /// FETCH の初回応答は FETCH_OK / REQUEST_ERROR (draft §3.2.1 (Fetch State Management)) であり REQUEST_OK ではなく、
    /// `RequestTable::Fetch` 分岐は FETCH 確立後 (FetchState::Established) の REQUEST_UPDATE_OK 経路
    /// (draft §9.5 (REQUEST_UPDATE): FETCH を REQUEST_UPDATE 対象に含め、応答を
    /// REQUEST_OK / REQUEST_ERROR と規定) として REQUEST_UPDATE に含まれる。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fn send_request_ok(
        &mut self,
        request_id: u64,
        mut parameters: MessageParameters,
        track_properties: TrackProperties,
    ) -> Result<(), SessionError> {
        self.require_established()?;

        let table = self.locate_request(request_id);

        // request_id 未発見は先に検出し、track_properties 検証より優先する
        if table.is_none() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "request_id not found for send_request_ok",
            ));
        }

        // draft §9.3 (REQUEST_OK): TRACK_STATUS_OK 以外の context では
        // TrackProperties は空でなければならない。
        // 将来 draft 改版で context 別許可が変更される可能性がある。
        if !matches!(table, Some(RequestTable::TrackStatus)) && !track_properties.is_empty() {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_OK track_properties must be empty in this context",
            ));
        }

        // draft §9.20.1 (Parameter Scope): context 別の許可パラメータ集合で
        // 検証する。Subscription context は send_ok_for_subscription で検証する。
        let context_allowed = match table {
            Some(RequestTable::TrackStatus) => Some((
                TRACK_STATUS_OK_ALLOWED_PARAMS,
                "REQUEST_OK (track_status) parameter not allowed in this context",
            )),
            Some(RequestTable::Fetch) => Some((
                REQUEST_UPDATE_OK_ALLOWED_PARAMS,
                "REQUEST_OK (request_update for fetch) parameter not allowed",
            )),
            Some(
                RequestTable::NamespacePublication
                | RequestTable::NamespaceSubscription
                | RequestTable::TrackSubscription,
            ) => Some((
                NAMESPACE_OK_ALLOWED_PARAMS,
                "REQUEST_OK (namespace/track-subscription) parameter not allowed in this context",
            )),
            Some(RequestTable::Subscription) => None,
            None => unreachable!("table.is_none() checked above"),
        };
        if let Some((allowed, message)) = context_allowed
            && parameters.validate_scope(allowed).is_err()
        {
            return Err(SessionError::new(SESSION_PROTOCOL_VIOLATION, message));
        }
        // draft-ietf-moq-transport-21 §3.3.2 (Range Filters): Range Filter を載せられるのは
        // SUBSCRIBE / FETCH / SUBSCRIBE_TRACKS / REQUEST_UPDATE のみ。REQUEST_OK 系
        // (PUBLISH_OK / REQUEST_UPDATE_OK 等) は上の context 別スコープ検証で既に
        // 弾かれているか、そもそも Range Filter を含まないため `has_range_filters()`
        // が false になり実質 no-op になる。context 判定は追加しない。
        self.validate_outgoing_range_filters(&parameters)?;

        match table {
            Some(RequestTable::Subscription) => {
                self.send_ok_for_subscription(request_id, &mut parameters)?
            }
            Some(RequestTable::Fetch) => self.send_ok_for_fetch(request_id)?,
            Some(RequestTable::NamespacePublication) => {
                self.send_ok_for_namespace_publication(request_id)?
            }
            Some(RequestTable::NamespaceSubscription) => {
                self.send_ok_for_namespace_subscription(request_id)?
            }
            Some(RequestTable::TrackSubscription) => {
                self.send_ok_for_track_subscription(request_id)?
            }
            Some(RequestTable::TrackStatus) => {
                self.send_ok_for_track_status(request_id, &mut parameters)?
            }
            None => unreachable!("table.is_none() checked above"),
        }
        // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
        // peer が TRACK_STATUS で INCLUDE_PROPERTIES=0 を指定したとき、TRACK_STATUS_OK の
        // Track Properties は存在するが空にする (SHOULD)。他 context の REQUEST_OK は
        // Track Properties を運べないため対象外 (上記の空検証で保証済み)。
        let track_properties = if matches!(table, Some(RequestTable::TrackStatus))
            && self
                .track_status_requests
                .get(&request_id)
                .is_some_and(|entry| entry.include_properties == Some(0))
        {
            TrackProperties::new()
        } else {
            track_properties
        };
        let msg = ControlMessage::RequestOk(RequestOk {
            parameters,
            track_properties,
        });
        // draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES): 応答送信で peer クレジット回復
        self.restore_incoming_request_update_credit(request_id);
        // draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS): TRACK_STATUS_OK 送信後は
        // bidi stream を FIN で閉じる (REQUEST_ERROR 側の send_request_error は既に FIN する)。
        let fin = matches!(table, Some(RequestTable::TrackStatus));
        self.events.push_back(SessionEvent::SendOnStream {
            request_id,
            message: msg,
            fin,
        });
        Ok(())
    }

    /// REQUEST_ERROR を応答送信する
    ///
    /// draft-ietf-moq-transport-21 §9.4 (REQUEST_ERROR): request 種別ごとに状態遷移が異なる。
    /// - subscription: 初回 request 失敗時は Pending → Terminated、
    ///   REQUEST_UPDATE 失敗時は Established → Terminated に遷移し、
    ///   PUBLISH_DONE(UPDATE_FAILED) を自動送信する
    ///   (draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions))。
    ///   ワイヤ順序は REQUEST_ERROR → PUBLISH_DONE で、open 中の outgoing subgroup
    ///   stream がある場合は §9.9 の MUST NOT に従い保留し全 stream 終端後に自動送信する。
    /// - fetch: 初回 request 失敗時は Pending → Terminated。
    ///   REQUEST_UPDATE 失敗時も `Established` → `Terminated` に遷移し、対応する
    ///   outgoing FETCH data stream に対して `SessionEvent::ResetDataStream` を自動発行する
    ///   (draft §9.5.1: "When a REQUEST_UPDATE fails for a FETCH, the publisher MUST reset
    ///   the FETCH data stream." (MUST))。ワイヤ順序は REQUEST_ERROR (bidi) →
    ///   RESET_STREAM (uni data)。error code は §12.5 (Stream Reset Error Codes) の
    ///   `CANCELLED` (0x1)。自動 reset により outgoing_fetch から stream エントリが除去され、
    ///   `data_stream_finished` が立つため終端通知なしでも `forget_fetch` が可能になる
    ///   (draft §3.2.1 の "A REQUEST_ERROR indicates that both endpoints can immediately
    ///   remove state." の破棄許可を行使する)。
    ///
    /// draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions): REQUEST_UPDATE 失敗時、
    /// SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS / PUBLISH_NAMESPACE では
    /// responder が bidi ストリームを閉じなければならない (MUST)。
    /// subscription の残存 outgoing subgroup stream の FIN / RESET も含め、
    /// これらの I/O 操作はアプリケーション層が本関数呼び出し後に行うこと
    /// (FETCH の data stream reset は上記のとおり Session が自動発行する)。
    ///
    /// subscription の REQUEST_UPDATE 失敗応答時は、PUBLISH_DONE (UPDATE_FAILED) を
    /// §9.9 (PUBLISH_DONE) の MUST NOT (全 stream を閉じるまで送ってはならない) に
    /// 従い、open 中の outgoing subgroup stream がある場合は保留し、全 stream 終端後
    /// (`send_data_stream_closed` / `reset_outgoing_data_stream`) に自動送信する。
    /// つまり **REQUEST_UPDATE 失敗応答後は全 stream を閉じること** (§9.5.1 の MUST)。
    /// また、保留 PUBLISH_DONE が push されるまで bidi request stream を閉じないこと
    /// (§9.9 の "A publisher sends a PUBLISH_DONE message as the final message before
    /// closing the subscription's bidi stream"。遅延により REQUEST_ERROR から時間が空く)。
    /// subscriber 側は REQUEST_UPDATE 失敗応答の REQUEST_ERROR 受信で
    /// `handle_err_for_subscription` が `Terminated` に遷移させ (draft §3.1.1 の
    /// REQUEST_ERROR 受信で subscription state を終える帰結)、遷移後の
    /// `send_request_update` は Established 要求のためエラーを返す
    /// (再 REQUEST_UPDATE の窓は閉じている)。
    ///
    /// ローカル専用コード (`SESSION_LOCAL_FILTER_MISMATCH` / `SESSION_LOCAL_DATAGRAM_TIMEOUT`) を
    /// `error_code` に渡した場合は `REQUEST_INTERNAL_ERROR` に置換し、未登録値を wire に出さない。
    pub fn send_request_error(
        &mut self,
        request_id: u64,
        error_code: u64,
        retry_interval: u64,
        reason: ReasonPhrase,
        redirect: Option<Redirect>,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        // ローカル専用コードが wire に流出しないよう、REQUEST_ERROR レジストリの
        // INTERNAL_ERROR に置換する
        let error_code = if is_local_error_code(error_code) {
            REQUEST_INTERNAL_ERROR
        } else {
            error_code
        };
        // subscription の Established 分岐で PUBLISH_DONE(UPDATE_FAILED) 自動送信、
        // fetch の Established 分岐で FETCH data stream の RESET_STREAM 自動発火が
        // 必要になるため、それぞれのスナップショットを受け取る
        let mut publish_done_stream_count: Option<u64> = None;
        let mut fetch_reset_stream_id: Option<super::types::DataStreamId> = None;
        match self.locate_request(request_id) {
            Some(RequestTable::Subscription) => {
                publish_done_stream_count = self.send_err_for_subscription(request_id)?;
            }
            Some(RequestTable::Fetch) => {
                fetch_reset_stream_id = self.send_err_for_fetch(request_id)?;
            }
            Some(RequestTable::NamespacePublication) => {
                self.send_err_for_namespace_publication(request_id)?;
            }
            Some(RequestTable::NamespaceSubscription) => {
                self.send_err_for_namespace_subscription(request_id)?;
            }
            Some(RequestTable::TrackSubscription) => {
                self.send_err_for_track_subscription(request_id)?;
            }
            Some(RequestTable::TrackStatus) => {
                self.send_err_for_track_status(request_id)?;
            }
            None => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "request_id not found for send_request_error",
                ));
            }
        }
        let msg = ControlMessage::RequestError(RequestError {
            error_code,
            retry_interval,
            reason,
            redirect,
        });
        // draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES): 応答送信で peer クレジット回復
        self.restore_incoming_request_update_credit(request_id);
        self.events.push_back(SessionEvent::SendOnStream {
            request_id,
            message: msg,
            // PUBLISH_DONE が続く場合は FIN をそちらに付け、REQUEST_ERROR 単独が最終の
            // 場合のみここで FIN する (§6.4.2.3 / §9.9)
            fin: publish_done_stream_count.is_none(),
        });
        // draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions): REQUEST_UPDATE 失敗時、
        // publisher は PUBLISH_DONE(UPDATE_FAILED) を送信する MUST。
        // draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE): "A sender MUST NOT send
        // PUBLISH_DONE until it has closed all streams it will ever open, and has no further
        // datagrams to send, for a subscription." の MUST NOT と両立するため、
        // open 中の outgoing subgroup stream がある場合は push を保留し
        // (`Subscription::pending_publish_done`)、全 stream 終端後
        // (`send_data_stream_closed` / `reset_outgoing_data_stream` 経由) に自動送信する。
        // open 中の stream がない場合は従来どおり REQUEST_ERROR 直後に push する
        // (ワイヤ順序は REQUEST_ERROR → PUBLISH_DONE)。
        if let Some(stream_count) = publish_done_stream_count {
            if self.has_open_outgoing_data_streams_for_request(request_id) {
                let subscription = self
                    .subscriptions
                    .get_mut(&request_id)
                    .expect("send_err_for_subscription guarantees key presence");
                subscription.pending_publish_done = Some(stream_count);
            } else {
                self.events.push_back(SessionEvent::SendOnStream {
                    request_id,
                    message: ControlMessage::PublishDone(PublishDone {
                        status_code: PUBLISH_DONE_UPDATE_FAILED,
                        stream_count,
                        reason: ReasonPhrase::new("").expect("empty reason phrase is always valid"),
                    }),
                    // PUBLISH_DONE が最終メッセージのため送信後に FIN する (§9.9)
                    fin: true,
                });
            }
        }
        // draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions): "When a REQUEST_UPDATE
        // fails for a FETCH, the publisher MUST reset the FETCH data stream." (MUST) を
        // Session から自動発行する。ワイヤ順序は REQUEST_ERROR (bidi) → RESET_STREAM (uni data)
        // を保つため、SendOnStream (REQUEST_ERROR) を push した後にここで発行する
        // (subscription 側の PUBLISH_DONE 遅延 push と対称)。
        // draft §12.5 (Stream Reset Error Codes): FETCH の REQUEST_UPDATE 失敗による reset は
        // publisher による cancel に該当するため `CANCELLED` (0x1) を用いる
        // (§16.11.4 のレジストリ表に定義され、Specification 列は §12.5 を指す)。
        if let Some(stream_id) = fetch_reset_stream_id {
            self.events.push_back(SessionEvent::ResetDataStream {
                stream_id,
                error_code: super::types::DataStreamResetReason::Cancelled.error_code(),
                // FETCH_HEADER 長の自動計算はしない (別途検討)
                reliable_size: None,
            });
        }
        Ok(())
    }

    /// 制御メッセージ応答待ち deadline を開始する (draft-ietf-moq-transport-21 §6.6 (Termination))
    pub(super) fn start_control_message_deadline(&mut self, request_id: u64) {
        if let Some(timeout_ms) = self.timing.control_message_timeout_ms {
            self.timing.control_message_deadlines.insert(
                request_id,
                super::types::DeadlineTimer::new(timeout_ms, self.timing.last_tick_ms),
            );
        }
    }

    /// 制御メッセージ応答待ち deadline を解除する
    pub(super) fn clear_control_message_deadline(&mut self, request_id: u64) {
        self.timing.control_message_deadlines.remove(&request_id);
    }

    pub(super) fn handle_peer_request_error(
        &mut self,
        request_id: u64,
        err: RequestError,
    ) -> Result<(), SessionError> {
        self.clear_control_message_deadline(request_id);
        // draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES): 応答受信でクレジット回復
        self.restore_outgoing_request_update_credit(request_id);
        // draft-ietf-moq-transport-21 §9.4.1 (Redirect Structure):
        // "If a server receives a Redirect with a non-zero Connect URI Length it MUST close
        // the session with a PROTOCOL_VIOLATION."
        // 将来のドラフト改訂で変更される可能性がある。
        if self.role == Role::Server
            && let Some(ref redirect) = err.redirect
            && !redirect.connect_uri.is_empty()
        {
            let e = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "server received redirect with non-zero connect URI length",
            );
            self.fail(e.clone());
            return Err(e);
        }
        let kind = self.locate_request(request_id);
        // draft-ietf-moq-transport-21 §9.4.1 (Redirect Structure):
        // namespace-scoped request (SUBSCRIBE_NAMESPACE, PUBLISH_NAMESPACE,
        // SUBSCRIBE_TRACKS) への Redirect で Track Name が non-empty の場合、
        // PROTOCOL_VIOLATION でセッションをクローズする (MUST)。
        // Track Name は namespace-scoped request では意味を持たず、
        // 必ず空でなければならない。
        if let Some(ref redirect) = err.redirect
            && !redirect.track_name.is_empty()
            && matches!(
                kind,
                Some(
                    RequestTable::NamespaceSubscription
                        | RequestTable::NamespacePublication
                        | RequestTable::TrackSubscription
                )
            )
        {
            let e = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "received redirect with non-empty track name for namespace-scoped request",
            );
            self.fail(e.clone());
            return Err(e);
        }
        let result = match kind {
            Some(RequestTable::Subscription) => self.handle_err_for_subscription(request_id),
            Some(RequestTable::Fetch) => self.handle_err_for_fetch(request_id),
            Some(RequestTable::NamespacePublication) => {
                self.handle_err_for_namespace_publication(request_id)
            }
            Some(RequestTable::NamespaceSubscription) => {
                self.handle_err_for_namespace_subscription(request_id)
            }
            Some(RequestTable::TrackSubscription) => {
                self.handle_err_for_track_subscription(request_id)
            }
            Some(RequestTable::TrackStatus) => self.handle_err_for_track_status(request_id),
            None => {
                let e = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_ERROR received for unknown request id",
                );
                self.fail(e.clone());
                Err(e)
            }
        };
        // サブディスパッチが state を正常に遷移させた場合のみイベントを発行する。
        // REQUEST_UPDATE 失敗応答で state を維持するケース (namespace 系の Established 維持分岐)
        // でもアプリが副作用を取れるように、失敗理由を常に通知する。
        // subscription / fetch は REQUEST_UPDATE 失敗応答で `Terminated` に遷移する
        // (subscription: draft §3.1.1 の REQUEST_ERROR 受信で subscription state を終える帰結)。
        if result.is_ok() {
            self.events.push_back(SessionEvent::RequestErrorReceived {
                request_id,
                error_code: err.error_code,
                retry_interval: err.retry_interval,
                reason: err.reason,
                redirect: err.redirect,
            });
        }
        result
    }

    pub(super) fn handle_peer_request_ok(
        &mut self,
        request_id: u64,
        ok: RequestOk,
    ) -> Result<(), SessionError> {
        self.clear_control_message_deadline(request_id);
        // draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES): 応答受信でクレジット回復
        self.restore_outgoing_request_update_credit(request_id);
        let table = self.locate_request(request_id);
        // draft §9.3 (REQUEST_OK): TRACK_STATUS_OK 以外の context では
        // TrackProperties は空でなければならない。違反時は PROTOCOL_VIOLATION でセッションクローズ。
        if !matches!(table, Some(RequestTable::TrackStatus)) && !ok.track_properties.is_empty() {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_OK track_properties must be empty in this context",
            );
            self.fail(err.clone());
            return Err(err);
        }
        // draft §9.20.1 (Parameter Scope): 許可されない context に出現した
        // パラメータの受信は PROTOCOL_VIOLATION でセッションをクローズしなければ
        // ならない (MUST)。REQUEST_OK は複数の応答 context で共有されるが、context
        // ごとに許可パラメータは異なる。RequestTable だけで context が確定する種別は
        // ここで context 別許可集合により検証する (旧 delivery-timeout 専用チェックを
        // 一般化して統合)。Subscription context は PUBLISH_OK と REQUEST_UPDATE_OK の
        // 区別に subscription.state が必要なため、handle_ok_for_subscription で検証する。
        let context_allowed = match table {
            Some(RequestTable::TrackStatus) => Some((
                TRACK_STATUS_OK_ALLOWED_PARAMS,
                "REQUEST_OK (track_status) parameter not allowed in this context",
            )),
            Some(RequestTable::Fetch) => Some((
                REQUEST_UPDATE_OK_ALLOWED_PARAMS,
                "REQUEST_OK (request_update for fetch) parameter not allowed in this context",
            )),
            Some(
                RequestTable::NamespacePublication
                | RequestTable::NamespaceSubscription
                | RequestTable::TrackSubscription,
            ) => Some((
                NAMESPACE_OK_ALLOWED_PARAMS,
                "REQUEST_OK (namespace/track-subscription) parameter not allowed in this context",
            )),
            Some(RequestTable::Subscription) | None => None,
        };
        if let Some((allowed, message)) = context_allowed
            && ok.parameters.validate_scope(allowed).is_err()
        {
            let err = SessionError::new(SESSION_PROTOCOL_VIOLATION, message);
            self.fail(err.clone());
            return Err(err);
        }
        match table {
            Some(RequestTable::Subscription) => {
                self.handle_ok_for_subscription(request_id, &ok.parameters)
            }
            Some(RequestTable::Fetch) => self.handle_ok_for_fetch(request_id, &ok.parameters),
            Some(RequestTable::NamespacePublication) => {
                self.handle_ok_for_namespace_publication(request_id, &ok.parameters)
            }
            Some(RequestTable::NamespaceSubscription) => {
                self.handle_ok_for_namespace_subscription(request_id, &ok.parameters)
            }
            Some(RequestTable::TrackSubscription) => {
                self.handle_ok_for_track_subscription(request_id, &ok.parameters)
            }
            Some(RequestTable::TrackStatus) => {
                self.handle_ok_for_track_status(request_id, &ok.parameters)
            }
            None => {
                let err = SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_OK received for unknown request id",
                );
                self.fail(err.clone());
                Err(err)
            }
        }
    }

    /// request_id が属するテーブルを特定する
    pub(super) fn locate_request(&self, request_id: u64) -> Option<RequestTable> {
        if self.subscriptions.contains_key(&request_id) {
            Some(RequestTable::Subscription)
        } else if self.fetches.contains_key(&request_id) {
            Some(RequestTable::Fetch)
        } else if self.namespaces.publications.contains_key(&request_id) {
            Some(RequestTable::NamespacePublication)
        } else if self.namespaces.subscriptions.contains_key(&request_id) {
            Some(RequestTable::NamespaceSubscription)
        } else if self.track_subscriptions.contains_key(&request_id) {
            Some(RequestTable::TrackSubscription)
        } else if self.track_status_requests.contains_key(&request_id) {
            Some(RequestTable::TrackStatus)
        } else {
            None
        }
    }

    pub(super) fn require_established(&self) -> Result<(), SessionError> {
        if self.state != SessionState::Established {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "operation requires session in Established state",
            ));
        }
        Ok(())
    }

    /// peer GOAWAY 受信時に `Err(SendRequestError::PeerGoawayReceived)` を返す。
    ///
    /// draft-ietf-moq-transport-21 §9.2 (GOAWAY): "Upon receiving a GOAWAY on the
    /// control stream, an endpoint SHOULD NOT initiate new requests to the peer."
    /// 自側 GOAWAY 送信 (`local_sent`) は抑制対象外。
    pub(super) fn check_peer_goaway(&self) -> Result<(), SendRequestError> {
        if self.goaway.peer.is_some() {
            return Err(SendRequestError::PeerGoawayReceived);
        }
        Ok(())
    }

    /// draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES):
    /// REQUEST_OK / REQUEST_ERROR 応答の受信時に自側 outstanding カウントを回復する。
    /// outstanding が 0 の場合 (初期 request への応答など) は何もしない。
    pub(super) fn restore_outgoing_request_update_credit(&mut self, request_id: u64) {
        if let Some(count) = self.outgoing_request_updates.get_mut(&request_id)
            && *count > 0
        {
            *count -= 1;
        }
    }

    /// draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES):
    /// REQUEST_OK / REQUEST_ERROR 応答の送信時に peer 側 outstanding カウントを回復する。
    /// outstanding が 0 の場合 (初期 request への応答など) は何もしない。
    pub(super) fn restore_incoming_request_update_credit(&mut self, request_id: u64) {
        if let Some(count) = self.incoming_request_updates.get_mut(&request_id)
            && *count > 0
        {
            *count -= 1;
        }
    }

    /// request の終端時にクレジットカウントエントリを掃除する
    ///
    /// draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES) のクレジット管理に使う
    /// `outgoing_request_updates` / `incoming_request_updates` はクレジットが残っていても
    /// request 終端後は使われないため、count の値に関係なく除去してよい。request_id は
    /// 再利用されない（自側採番は `RequestIdGenerator` の単調増加、peer 採番は
    /// `RequestIdTracker` の重複拒否で担保）ため、残ったエントリが新 request のクレジット
    /// として誤用されることはない。
    /// forget 系 API が `None` を返す場合（cleanup 可能でない・ Pending 等）は掃除しない
    /// （生きている request のクレジットを失わせると MAX_REQUEST_UPDATES の上限検出が
    /// 緩むため。forget 系 API は成功時のみ本関数を呼ぶこと）。
    pub(super) fn remove_request_update_credit_entries(&mut self, request_id: u64) {
        self.outgoing_request_updates.remove(&request_id);
        self.incoming_request_updates.remove(&request_id);
    }

    /// キャンセル済み peer publisher alias の tombstone 保持期間 (ms) を返す
    ///
    /// draft-ietf-moq-transport-21 §3.1.2 (Track Alias) の SHOULD に対応する保持期間。
    pub fn peer_alias_retention_ms(&self) -> u64 {
        self.timing.peer_alias_retention_ms
    }

    /// キャンセル済み peer publisher alias の tombstone 保持期間 (ms) を設定する
    ///
    /// `0` を設定すると tombstone は最初の `tick` で即期限切れになり、実質無効化できる。
    /// 既に登録済みの tombstone の期限は変更しない (以降の登録に新しい値を使う)。
    pub fn set_peer_alias_retention_ms(&mut self, retention_ms: u64) {
        self.timing.peer_alias_retention_ms = retention_ms;
    }

    /// peer publisher alias に subscription を登録する
    ///
    /// draft-ietf-moq-transport-21 §3.1.2 (Track Alias): tombstone 状態の alias は Established
    /// subscription を持たないため DUPLICATE_TRACK_ALIAS の対象外である。新しい PUBLISH /
    /// SUBSCRIBE_OK が同じ alias で届いたら tombstone を破棄して通常の索引へ戻す。
    pub(super) fn register_peer_alias(&mut self, track_alias: u64, request_id: u64) {
        self.aliases.peer_alias_tombstones.remove(&track_alias);
        insert_alias_holder(
            &mut self.aliases.peer_publisher_aliases,
            track_alias,
            request_id,
        );
    }

    /// peer publisher alias から subscription を 1 つ外す
    ///
    /// 最後の 1 つだった場合は discard 用 tombstone を登録し `true` を返す。
    /// 共有 alias の相手が残っている場合は `false` を返し、tombstone も登録しない。
    pub(super) fn release_peer_alias(&mut self, track_alias: u64, request_id: u64) -> bool {
        if !remove_alias_holder(
            &mut self.aliases.peer_publisher_aliases,
            track_alias,
            request_id,
        ) {
            return false;
        }
        // draft §3.1.2 (Track Alias): キャンセル直後の遅延 Object を未知 alias 扱いにせず
        // 確実に破棄できるよう、一定期間 alias を覚えておく
        let timer = DeadlineTimer::new(
            self.timing.peer_alias_retention_ms,
            self.timing.last_tick_ms,
        );
        self.aliases
            .peer_alias_tombstones
            .insert(track_alias, timer);
        true
    }

    /// discard 用 tombstone の保持期間を進め、期限切れのものを削除する
    ///
    /// draft-ietf-moq-transport-21 §3.1.2 (Track Alias) の保持期間終了後は、同じ alias の
    /// Object は従来どおり未知 alias として扱われる。
    pub(super) fn tick_peer_alias_tombstones(&mut self, now_ms: u64) {
        for timer in self.aliases.peer_alias_tombstones.values_mut() {
            timer.tick(now_ms);
        }
        self.aliases
            .peer_alias_tombstones
            .retain(|_, timer| !timer.expired);
    }

    /// tombstone が有効期間内かどうかを判定する
    pub(super) fn peer_alias_is_tombstoned(&self, track_alias: u64) -> bool {
        self.aliases
            .peer_alias_tombstones
            .get(&track_alias)
            .is_some_and(|timer| !timer.expired)
    }

    /// 自側 SETUP で宣言した MAX_FILTER_RANGES を返す (未宣言は 0)
    ///
    /// draft-ietf-moq-transport-21 §9.1.6 (MAX FILTER RANGES): "The default value is 0"。
    /// 単一メッセージの検証 ([`check_incoming_range_filters`](Self::check_incoming_range_filters)) と
    /// subscription 単位の累積検証が同じ値を見るよう、参照を 1 箇所にまとめている。
    pub(super) fn local_max_filter_ranges(&self) -> u64 {
        self.setup
            .local
            .as_ref()
            .and_then(|s| s.options.max_filter_ranges())
            .unwrap_or(0)
    }

    /// draft-ietf-moq-transport-21 §3.3.2 (Range Filters): 受信メッセージの Range Filter を検証する。
    ///
    /// - Range 総数が自側 SETUP で宣言した MAX_FILTER_RANGES を超えていないか
    /// - デルタエンコーディング溢出・(Parameter Type, SetID, Property Type) 重複がないか
    ///
    /// デフォルト値 0 では Range を 1 つ以上持つ Range Filter がすべて拒否される。
    /// 一方 Range を持たないインスタンス (Length=0 の削除指示など) は総数 0 と数えられるため、
    /// MAX_FILTER_RANGES=0 でもこの上限判定を通過して**受理される**。
    ///
    /// これは意図した選択である。§3.3.2 の該当段落は 2 文で構成される。
    ///
    /// > Range Filters are only allowed if the setup option MAX_FILTER_RANGES is non-zero,
    /// > which limits the total number of Ranges allowed in all Range Filter parameters for
    /// > a given subscription or fetch. If this limit is exceeded, an endpoint MUST reject
    /// > this with REQUEST_ERROR with error code INVALID_FILTER.
    ///
    /// 1 文目は許容そのものを MAX_FILTER_RANGES != 0 に条件づけるが、2 文目の MUST reject は
    /// 「上限超過」しか条件にしていない。つまり Range 数 0 のインスタンスは「仕様が許して
    /// いない入力だが、拒否の MUST も無い」状態である。「送信は保守的・受信は寛容」の原則に
    /// 従い、MUST で要求されていない拒否で peer のリクエストを失敗させない。
    /// 送信側 ([`validate_outgoing_range_filters`](Self::validate_outgoing_range_filters)) は
    /// §9.1.6 に literal な MUST NOT があるため拒否する。この非対称は仕様の非対称に由来する。
    ///
    /// 違反時は拒否理由 (`&'static str`) を返す。REQUEST_ERROR の送出と後始末は呼び出し元が行う
    /// (REQUEST_UPDATE の Range Filter 拒否は role によって終端方法が異なるため、
    /// 呼び出し元が subscription の文脈を知っている必要がある)。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub(super) fn check_incoming_range_filters(
        &self,
        parameters: &crate::message_parameter::MessageParameters,
    ) -> Result<(), &'static str> {
        // FILL 内側の Range Filter も検証対象に含める
        // (draft-ietf-moq-transport-21 §9.20.16)。
        let has_inner_range_filters = parameters
            .fill_parameters()
            .is_some_and(|fill| fill.has_range_filters());
        if !parameters.has_range_filters() && !has_inner_range_filters {
            return Ok(());
        }
        let local_max = self.local_max_filter_ranges();
        let count = parameters.count_range_filters();
        if count > local_max {
            return Err("Range Filters exceed MAX_FILTER_RANGES");
        }
        // draft-ietf-moq-transport-21 §3.3.2: デルタ溢出・重複検証
        if parameters.validate_range_filters().is_err() {
            return Err("Range Filter validation failed");
        }
        // draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter):
        // FILL 内側の Range Filter も外側と同様に内部構造を検証する
        // (デルタ溢出・重複は INVALID_FILTER で拒否)。内側は運搬メッセージにのみ
        // 適用される transient なスコープのため、MAX_FILTER_RANGES の累積数には
        // 含めない (累積パラメータからも除外される)。
        if let Some(fill) = parameters.fill_parameters()
            && fill.has_range_filters()
            && fill.validate_range_filters().is_err()
        {
            return Err("Range Filter validation failed inside FILL_PARAMETERS");
        }
        Ok(())
    }

    /// draft-ietf-moq-transport-21 §3.3.2 (Range Filters): 送信メッセージの Range Filter を検証する。
    ///
    /// - peer が MAX_FILTER_RANGES を宣言しているか (§9.1.6)
    /// - Range 総数が peer の SETUP で宣言した MAX_FILTER_RANGES を超えていないか
    /// - デルタ溢出・(Parameter Type, SetID, Property Type) 重複・ Property Type 偶数の内部構造検証
    ///
    /// §9.1.6 は送信側に literal な MUST NOT を課す。
    ///
    /// > The default value is 0, so if not specified, the peer MUST NOT send any such
    /// > filter parameters.
    ///
    /// 条件は "such filter **parameters**" であり Range 数ではないので、peer_max が 0 なら
    /// Range を 1 つも持たないインスタンス (Length=0 の削除指示など) も送出してはならない。
    /// 受信側 ([`check_incoming_range_filters`](Self::check_incoming_range_filters)) は
    /// Range 数 0 を受理するが、これは §3.3.2 に受信側の拒否 MUST が無いためで、
    /// 「送信は保守的・受信は寛容」に沿った意図的な非対称である。
    ///
    /// 重複した Range Filter は peer が INVALID_FILTER で拒否することが §3.3.2 で MUST とされるため、
    /// 送信前に自側で弾く。セッションは閉じず `Err` を呼び出し側へ返すだけに留める。
    ///
    /// Range Filter を載せられる送信経路すべてから呼ばれる (SUBSCRIBE / REQUEST_UPDATE /
    /// FETCH / SUBSCRIBE_TRACKS)。受信側の
    /// [`check_incoming_range_filters`](Self::check_incoming_range_filters) と対称である。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub(super) fn validate_outgoing_range_filters(
        &self,
        parameters: &crate::message_parameter::MessageParameters,
    ) -> Result<(), SessionError> {
        // FILL 内側の Range Filter も送出可否の対象に含める
        // (draft-ietf-moq-transport-21 §9.20.16)。
        let has_inner_range_filters = parameters
            .fill_parameters()
            .is_some_and(|fill| fill.has_range_filters());
        if !parameters.has_range_filters() && !has_inner_range_filters {
            return Ok(());
        }
        let peer_max = self
            .setup
            .peer
            .as_ref()
            .and_then(|s| s.options.max_filter_ranges())
            .unwrap_or(0);
        // draft-ietf-moq-transport-21 §9.1.6: peer が MAX_FILTER_RANGES を宣言していなければ
        // Range Filter パラメータ自体を送ってはならない。Range 総数が 0 のインスタンスも対象。
        if peer_max == 0 {
            return Err(SessionError::new(
                crate::error::SESSION_PROTOCOL_VIOLATION,
                "peer did not declare MAX_FILTER_RANGES",
            ));
        }
        let count = parameters.count_range_filters();
        if count > peer_max {
            return Err(SessionError::new(
                crate::error::SESSION_PROTOCOL_VIOLATION,
                "outgoing Range Filters exceed peer MAX_FILTER_RANGES",
            ));
        }
        if parameters.validate_range_filters().is_err() {
            return Err(SessionError::new(
                crate::error::SESSION_PROTOCOL_VIOLATION,
                "outgoing Range Filters are invalid",
            ));
        }
        // draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter):
        // FILL 内側の Range Filter も外側と同様に送信前に自側で弾く
        // (受信側の `check_incoming_range_filters` と対称)。
        if let Some(fill) = parameters.fill_parameters()
            && fill.has_range_filters()
            && fill.validate_range_filters().is_err()
        {
            return Err(SessionError::new(
                crate::error::SESSION_PROTOCOL_VIOLATION,
                "outgoing Range Filters are invalid inside FILL_PARAMETERS",
            ));
        }
        Ok(())
    }

    /// REQUEST_ERROR を peer 宛に送出する (draft §9.4.2 (REQUEST_ERROR Message Format))。
    /// reason は §8.5 (Reason Phrase Structure) の 1024 バイト上限以下の静的文字列を想定し、超過時は panic する。
    ///
    /// 副作用: `request_streams` 未登録の request id は拒否済み集合 (`rejected_request_ids`)
    /// に記録され、peer のストリームクローズが no-op で吸収されるようになる
    /// (記録条件の詳細は集合の doc 参照)。登録済み request への応答として呼ぶ場合は
    /// 記録されない。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    #[track_caller]
    pub(super) fn emit_request_error(&mut self, request_id: u64, error_code: u64, reason: &str) {
        // ローカル専用コードが wire に流出しないよう、REQUEST_ERROR レジストリの
        // INTERNAL_ERROR に置換する (`send_request_error` と同じ置換を最終防御として行う)
        let error_code = if is_local_error_code(error_code) {
            REQUEST_INTERNAL_ERROR
        } else {
            error_code
        };
        // 拒否済み request id のストリームクローズを no-op で吸収するため、
        // `request_streams` 未登録の場合のみ集合に記録する。
        // 登録済み request への応答としての REQUEST_ERROR (REQUEST_UPDATE 拒否等) は
        // 記録しない: `recv_request_stream_closed` は `request_streams` を先に引くため、
        // 登録済み id を記録すると集合から削除されずに残り続ける (リーク) 上、
        // `forget_*` 後の遅延クローズが no-op で吸収され、2 回目以降のクローズを
        // unknown id として fail させる保護が失われる。
        if !self.request_streams.contains_key(&request_id) {
            self.rejected_request_ids.insert(request_id);
        }
        let msg = ControlMessage::RequestError(RequestError {
            error_code,
            retry_interval: 0,
            reason: ReasonPhrase::new(reason)
                .expect("emit_request_error reason must be <= 1024 bytes"),
            redirect: None,
        });
        // draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES):
        // "Each REQUEST_OK or REQUEST_ERROR response restores one credit on that stream"。
        // プロトコル層が自動で返す REQUEST_ERROR も応答であり、peer の outstanding を回復させる。
        // outstanding が 0 の初期 request 応答 (SUBSCRIBE / FETCH 等の拒否) では no-op になる。
        self.restore_incoming_request_update_credit(request_id);
        self.events.push_back(SessionEvent::SendOnStream {
            request_id,
            message: msg,
            // 自動拒否は最終応答のため送信後に FIN する (§6.4.2.3)
            fin: true,
        });
    }

    fn handle_peer_setup(&mut self, setup: Setup) -> Result<(), SessionError> {
        // draft §9.1 (SETUP): SETUP は各エンドポイントから 1 回のみ
        if self.setup.peer.is_some() {
            let err = SessionError::new(SESSION_PROTOCOL_VIOLATION, "duplicate SETUP received");
            self.fail(err.clone());
            return Err(err);
        }

        // draft §9.1.1 (AUTHORITY), §9.1.2 (PATH): 相手 role + transport で違反がないか確認
        let peer_role = match self.role {
            Role::Client => Role::Server,
            Role::Server => Role::Client,
        };
        if let Err(err) = validate_setup_role_transport(&setup.options, peer_role, self.transport) {
            self.fail(err.clone());
            return Err(err);
        }
        if let Err(err) = validate_setup_uri_format(&setup.options) {
            self.fail(err.clone());
            return Err(err);
        }

        // draft §9.20.3 (AUTHORIZATION TOKEN Parameter) / §9.1.3 (MAX_AUTH_TOKEN_CACHE_SIZE): peer が REGISTER したトークンの保持上限は
        // 「自側」の MAX_AUTH_TOKEN_CACHE_SIZE で決まる (受信側が自身のリソースを
        // 保護するために宣言する値)。draft §9.1.4 (AUTHORIZATION TOKEN): SETUP の REGISTER が自側 MAX
        // を超えたら USE_VALUE 扱い (session は閉じない)。
        let self_max_size = self
            .setup
            .local
            .as_ref()
            .and_then(|s| s.options.max_auth_token_cache_size())
            .unwrap_or(0);
        let mut peer_cache = AuthTokenCache::new(self_max_size);
        if let Err(err) = register_setup_auth_tokens(&setup.options, &mut peer_cache) {
            self.fail(err.clone());
            return Err(err);
        }
        self.auth.peer_token_cache = peer_cache;

        self.setup.peer = Some(setup);

        // 両側完了 → Established
        self.state = SessionState::Established;
        self.events.push_back(SessionEvent::Established);
        Ok(())
    }

    pub(super) fn fail(&mut self, err: SessionError) {
        // ローカル専用コードの wire 流出を防ぐ最終防御。公開 API 側で置換済みだが、
        // crate 内の誤用にも備える
        let err = if is_local_error_code(err.code) {
            SessionError::new(SESSION_INTERNAL_ERROR, err.reason)
        } else {
            err
        };
        if matches!(self.state, SessionState::Closing | SessionState::Closed) {
            return;
        }
        self.state = SessionState::Closing;
        self.events.push_back(SessionEvent::CloseSession(err));
    }

    /// 非 SETUP メッセージに含まれる AUTHORIZATION_TOKEN を `peer_auth_token_cache` に反映する
    ///
    /// draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter) により
    /// PUBLISH / SUBSCRIBE / REQUEST_UPDATE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS /
    /// PUBLISH_NAMESPACE / TRACK_STATUS / FETCH の 8 種で AUTHORIZATION_TOKEN が出現しうる。
    /// 各呼び出し元 handler は受信メッセージの parameters から直接本関数を呼ぶ。
    ///
    /// - REGISTER: `peer_auth_token_cache.try_register` を呼び出す。
    ///   duplicate alias は `DUPLICATE_AUTH_TOKEN_ALIAS` (try_register が返す),
    ///   overflow (`Ok(false)`) は `AUTH_TOKEN_CACHE_OVERFLOW` で session を閉じる
    ///   (draft §9.20.3 (AUTHORIZATION TOKEN Parameter): 非 SETUP ではセッション終了。SETUP で閉じない例外は
    ///   §9.1.4 (AUTHORIZATION TOKEN))。
    /// - DELETE: `peer_auth_token_cache.delete(alias)` を no-op で呼び出す
    ///   (未登録 alias の DELETE に対するセッションエラー規定はない)。
    /// - USE_ALIAS: 未登録 alias への参照は `UNKNOWN_AUTH_TOKEN_ALIAS` で閉じる
    ///   (draft §9.20.3 (AUTHORIZATION TOKEN Parameter))。
    /// - USE_VALUE: cache 操作なし。
    ///
    /// Malformed Token (`MALFORMED_AUTH_TOKEN`) はコーデック層 (`message_parameter`) で
    /// 検出済みのため、本関数は形式的に健全な Token 構造のみを扱う。
    pub(super) fn apply_peer_message_auth_tokens(
        &mut self,
        parameters: &MessageParameters,
    ) -> Result<(), SessionError> {
        for token in parameters.authorization_tokens() {
            match token {
                AuthorizationToken::Register {
                    alias,
                    token_type,
                    token_value,
                } => {
                    match self.auth.peer_token_cache.try_register(
                        *alias,
                        *token_type,
                        token_value.clone(),
                    ) {
                        Ok(true) => {}
                        Ok(false) => {
                            let err = SessionError::new(
                                SESSION_AUTH_TOKEN_CACHE_OVERFLOW,
                                "peer AUTHORIZATION_TOKEN REGISTER exceeds cache size",
                            );
                            self.fail(err.clone());
                            return Err(err);
                        }
                        Err(err) => {
                            // duplicate alias
                            self.fail(err.clone());
                            return Err(err);
                        }
                    }
                }
                AuthorizationToken::Delete { alias } => {
                    self.auth.peer_token_cache.delete(*alias);
                }
                AuthorizationToken::UseAlias { alias } => {
                    if self.auth.peer_token_cache.resolve(*alias).is_none() {
                        let err = SessionError::new(
                            SESSION_UNKNOWN_AUTH_TOKEN_ALIAS,
                            "peer AUTHORIZATION_TOKEN USE_ALIAS references unknown alias",
                        );
                        self.fail(err.clone());
                        return Err(err);
                    }
                }
                AuthorizationToken::UseValue { .. } => {
                    // cache 操作なし
                }
            }
        }
        Ok(())
    }

    /// GOAWAY 拒否経路など「request 自体が失敗する」経路向けに、
    /// AUTHORIZATION_TOKEN のうち REGISTER だけを cache に反映する
    ///
    /// draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter):
    /// "The receiver of a message carrying an AUTHORIZATION TOKEN with Alias Type REGISTER
    /// that does not result in a Session error MUST register the Token Alias, in the token
    /// cache, even if the message fails for other reasons, including Unauthorized." (MUST)
    ///
    /// MUST の対象は Alias Type が **REGISTER** の Token のみ。DELETE / USE_ALIAS / USE_VALUE
    /// は「失敗した request のパラメータ」として peer 側で not-applied 扱いされる可能性が
    /// あるため、本関数は触らない (例: USE_ALIAS を適用すると未登録 alias で
    /// `UNKNOWN_AUTH_TOKEN_ALIAS` として session を kill するが、これは GOAWAY 拒否の
    /// benign 失敗を session error に格上げしてしまい、拒否前の挙動より悪化する)。
    ///
    /// REGISTER 適用は `apply_peer_message_auth_tokens` と同じく `AUTH_TOKEN_CACHE_OVERFLOW`
    /// / `DUPLICATE_AUTH_TOKEN_ALIAS` で session error になり得る。
    pub(super) fn apply_peer_message_auth_token_registers_only(
        &mut self,
        parameters: &MessageParameters,
    ) -> Result<(), SessionError> {
        for token in parameters.authorization_tokens() {
            let AuthorizationToken::Register {
                alias,
                token_type,
                token_value,
            } = token
            else {
                continue;
            };
            match self
                .auth
                .peer_token_cache
                .try_register(*alias, *token_type, token_value.clone())
            {
                Ok(true) => {}
                Ok(false) => {
                    let err = SessionError::new(
                        SESSION_AUTH_TOKEN_CACHE_OVERFLOW,
                        "peer AUTHORIZATION_TOKEN REGISTER exceeds cache size",
                    );
                    self.fail(err.clone());
                    return Err(err);
                }
                Err(err) => {
                    // duplicate alias
                    self.fail(err.clone());
                    return Err(err);
                }
            }
        }
        Ok(())
    }
}

fn validate_setup_uri_format(options: &SetupOptions) -> Result<(), SessionError> {
    options.validate_uri_format().map_err(|err| match err {
        SetupUriValidationError::Path => {
            SessionError::new(SESSION_MALFORMED_PATH, "PATH does not conform to RFC 3986")
        }
        SetupUriValidationError::Authority => SessionError::new(
            SESSION_MALFORMED_AUTHORITY,
            "AUTHORITY does not conform to RFC 3986",
        ),
    })
}

/// SETUP Options が送信者 role + transport と整合するか検証する
///
/// draft-ietf-moq-transport-21 §9.1.1 (AUTHORITY) / §9.1.2 (PATH):
/// - AUTHORITY / PATH は Client のみ送信可能 (Server が送信すると INVALID_AUTHORITY / INVALID_PATH)
/// - AUTHORITY / PATH は QUIC native のみ (WebTransport では INVALID_AUTHORITY / INVALID_PATH)
fn validate_setup_role_transport(
    options: &SetupOptions,
    sender_role: Role,
    transport: Transport,
) -> Result<(), SessionError> {
    if options.authority().is_some() {
        if sender_role == Role::Server {
            return Err(SessionError::new(
                SESSION_INVALID_AUTHORITY,
                "server MUST NOT send AUTHORITY",
            ));
        }
        if transport == Transport::WebTransport {
            return Err(SessionError::new(
                SESSION_INVALID_AUTHORITY,
                "AUTHORITY MUST NOT be used with WebTransport",
            ));
        }
    }
    if options.path().is_some() {
        if sender_role == Role::Server {
            return Err(SessionError::new(
                SESSION_INVALID_PATH,
                "server MUST NOT send PATH",
            ));
        }
        if transport == Transport::WebTransport {
            return Err(SessionError::new(
                SESSION_INVALID_PATH,
                "PATH MUST NOT be used with WebTransport",
            ));
        }
    }
    Ok(())
}

/// SETUP Options 内の AUTHORIZATION_TOKEN を cache に登録する
///
/// draft-ietf-moq-transport-21 §9.1.4 (AUTHORIZATION TOKEN):
/// - REGISTER が MAX_AUTH_TOKEN_CACHE_SIZE を超えた場合は USE_VALUE として扱う
///   (AUTH_TOKEN_CACHE_OVERFLOW では閉じない)
/// - DELETE / USE_ALIAS は SETUP では許可されない。parameter.rs のデコード段階で
///   既に弾かれているため、本関数に到達することは通常ないが、防御として検出する
///   (到達した場合は PROTOCOL_VIOLATION)
fn register_setup_auth_tokens(
    options: &SetupOptions,
    cache: &mut AuthTokenCache,
) -> Result<(), SessionError> {
    for token in options.authorization_tokens() {
        match token {
            AuthorizationToken::Register {
                alias,
                token_type,
                token_value,
            } => {
                // Ok(false) = 超過 (SETUP では USE_VALUE 扱いで無視する)
                cache.try_register(*alias, *token_type, token_value.clone())?;
            }
            AuthorizationToken::UseValue { .. } => {
                // cache 操作なし
            }
            AuthorizationToken::Delete { .. } | AuthorizationToken::UseAlias { .. } => {
                return Err(SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "DELETE / USE_ALIAS not allowed in SETUP",
                ));
            }
        }
    }
    Ok(())
}
