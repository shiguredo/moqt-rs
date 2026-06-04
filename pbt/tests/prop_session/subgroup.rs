//! 受信 subgroup 経路のプロパティテスト
//!
//! draft-ietf-moq-transport-21 §3.1 (Subscriptions): "Because subscriptions can share a
//! Track Alias, the subscriber re-applies each subscription's filter to determine which
//! subscription a received Object belongs to."
//!
//! 単体テスト (`tests/test_session/subgroup_object_filter/`) がエラーパスと境界値を固定する
//! のに対し、本ファイルはサンプラーで操作列と入力の組合せを生成し、次の不変条件を検証する。
//!
//! - 受信 data stream (subgroup / fill fetch) の会計: `open_incoming_subgroup_count` の
//!   全 subscription 合計が、現在 open 中の受信 subgroup stream 数と一致し、全 stream 終端 /
//!   forget 後に 0 に戻る
//! - stream 終端 / forget 後に per-subgroup delivery timeout override が残らない
//! - 帰属先が「候補順で最初の非キャンセルかつ `object_passes_filters` 合格」で決定的である
//! - `FilteredOut` / `Discarded` の Object が subscription スコープの状態
//!   (`largest_received_location` / `ended_groups` / `end_of_track` / delivery timeout
//!   override / Object 系 tracker) を更新しない
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use pbt::common::test_runner;
use shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION;
use shiguredo_moqt::message::common::TrackNamespace;
use shiguredo_moqt::message_parameter::{
    MessageParameter, MessageParameterValue, MessageParameters, PARAM_OBJECT_DELIVERY_TIMEOUT,
    PARAM_OBJECTID_FILTER, PARAM_SUBGROUP_DELIVERY_TIMEOUT,
};
use shiguredo_moqt::object_properties::{
    ObjectProperties, ObjectProperty, ObjectPropertyValue, PROP_PRIOR_OBJECT_ID_GAP,
};
use shiguredo_moqt::parameter::{
    SETUP_OPTION_MAX_FILTER_RANGES, SetupOption, SetupOptionValue, SetupOptions,
};
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::{
    DataStreamId, RequestStreamEnd, SessionState, SubscriptionState, TrackDataAcceptance, Transport,
};
use shiguredo_moqt::stream::FETCH_HEADER_TYPE;
use shiguredo_moqt::stream::decoder::DecodedSubgroupObject;
use shiguredo_moqt::stream::fetch::FetchHeader;
use shiguredo_moqt::stream::subgroup::{SubgroupHeader, SubgroupIdMode};
use shiguredo_moqt::track_properties::TrackProperties;

use super::common::{take_send_control, take_send_on_stream, take_send_request};

/// `SUBGROUP_DELIVERY_TIMEOUT` と `OBJECT_DELIVERY_TIMEOUT` を持つ Properties バイト列
///
/// 先頭 Object に付与すると per-subgroup delivery timeout override が登録される
/// (draft-ietf-moq-transport-21 §10.1 (SUBGROUP_DELIVERY_TIMEOUT) /
/// §10.2 (OBJECT_DELIVERY_TIMEOUT))。両 Property は偶数型なので varint 値で encode する。
/// `ObjectProperties::encode` の出力は Properties Length varint を含む完全なブロックである。
fn delivery_timeout_properties(subgroup_ms: u64, object_ms: u64) -> Vec<u8> {
    encode_object_properties([
        ObjectProperty {
            prop_type: PARAM_SUBGROUP_DELIVERY_TIMEOUT,
            value: ObjectPropertyValue::VarInt(subgroup_ms),
        },
        ObjectProperty {
            prop_type: PARAM_OBJECT_DELIVERY_TIMEOUT,
            value: ObjectPropertyValue::VarInt(object_ms),
        },
    ])
}

/// `PRIOR_OBJECT_ID_GAP` (0x3E) を 1 つ持つ Properties バイト列
///
/// Object 系 tracker (`peer_object_properties`) に観測されたかどうかを、後続 Object の
/// Malformed Track 検出として観測するために使う。
fn prior_object_id_gap_properties(value: u64) -> Vec<u8> {
    encode_object_properties([ObjectProperty {
        prop_type: PROP_PRIOR_OBJECT_ID_GAP,
        value: ObjectPropertyValue::VarInt(value),
    }])
}

/// Object Property 列を Properties バイト列へ encode する
fn encode_object_properties(properties: impl IntoIterator<Item = ObjectProperty>) -> Vec<u8> {
    let mut encoded = ObjectProperties::new();
    for property in properties {
        encoded.push(property);
    }
    let mut bytes = Vec::new();
    encoded
        .encode(&mut bytes)
        .expect("テストフィクスチャの前提条件を満たす");
    bytes
}

/// OBJECTID_FILTER の Range 1 個を持つ MessageParameter (Start / End は両端含む)
fn object_id_filter(set_id: u8, start: u64, end: u64) -> MessageParameter {
    let mut bytes = vec![set_id];
    shiguredo_moqt::varint::encode(start, &mut bytes);
    shiguredo_moqt::varint::encode(end - start, &mut bytes);
    MessageParameter {
        param_type: PARAM_OBJECTID_FILTER,
        value: MessageParameterValue::LengthPrefixed(bytes),
    }
}

/// MAX_FILTER_RANGES を宣言する peer (server) 用 SetupOptions
///
/// `max_filter_options` を SETUP で宣言しない peer へ Range Filter を送ると
/// `SESSION_PROTOCOL_VIOLATION` になる。
fn max_filter_options() -> SetupOptions {
    let mut options = SetupOptions::new();
    options.push(SetupOption {
        option_type: SETUP_OPTION_MAX_FILTER_RANGES,
        value: SetupOptionValue::VarInt(8),
    });
    options
}

/// `normal_object` 相当の Object を作る (payload あり / Properties なし)
fn normal_object(object_id: u64) -> DecodedSubgroupObject {
    DecodedSubgroupObject {
        object_id,
        payload_length: 1,
        status: None,
        properties_bytes: None,
    }
}

/// Properties 付きの Object を作る
fn object_with_properties(object_id: u64, properties_bytes: Vec<u8>) -> DecodedSubgroupObject {
    DecodedSubgroupObject {
        object_id,
        payload_length: 1,
        status: None,
        properties_bytes: Some(properties_bytes),
    }
}

/// subgroup stream を登録して header を受理させる (stream type の通知も行う)
fn recv_header(
    client: &mut Session,
    stream_id: DataStreamId,
    header: &SubgroupHeader,
) -> TrackDataAcceptance {
    client
        .recv_data_stream_type(stream_id, header.encode()[0] as u64)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_subgroup_header(stream_id, header)
        .expect("テストフィクスチャの前提条件を満たす")
}

/// 1 ケース分の subscription 記述 (観測状態のモデルを含む)
struct SubscriptionSpec {
    /// この subscription の Request ID
    request_id: u64,
    /// キャンセル済みか (STOP_SENDING 送信済み)
    cancelled: bool,
    /// forget で Session から回収済みか
    recovered: bool,
    /// 受理した Object の最大位置のモデル値
    ///
    /// `record_largest_received_location` は単調最大をとるため、受理のたびに
    /// `max(既存, 今回)` で更新する。
    largest_received: Option<(u64, u64)>,
}

impl SubscriptionSpec {
    /// subscription が存在し、キャンセル由来 `Terminated` でないか
    fn is_live(&self) -> bool {
        !self.cancelled
    }

    /// 受理した Object の位置をモデルへ記録する
    fn record_largest_received(&mut self, location: (u64, u64)) {
        self.largest_received = Some(match self.largest_received {
            Some(existing) => existing.max(location),
            None => location,
        });
    }
}

/// subscription のフィルタのモデル
///
/// 本ファイルは OBJECTID_FILTER だけを使うため、header 時点の評価 (Subgroup ID と
/// Publisher Priority のみ) も Object 時点の評価も「Object ID が範囲内か」で同じ結果になる。
/// 帰属先は「候補順で最初の生きた subscription のうちフィルタに合格するもの」で決まる。
/// キャンセル済みの候補は帰属対象から除外されるが、フィルタ評価自体は行われる
/// (`Discarded` 判定に使われる)。
#[derive(Clone, Copy)]
enum FilterModel {
    /// フィルタなし (全 Object が合格する)
    Unfiltered,
    /// OBJECTID_FILTER の範囲 (両端含む)
    ObjectIdRange { start: u64, end: u64 },
}

impl FilterModel {
    /// このフィルタが Object ID を通すか
    fn passes(&self, object_id: u64) -> bool {
        match self {
            FilterModel::Unfiltered => true,
            FilterModel::ObjectIdRange { start, end } => (*start..=*end).contains(&object_id),
        }
    }
}

/// 候補順で最初に「生きたかつ Object ID 0 を通す」request_id を返す
///
/// 新規 subgroup stream の header 受理は Object ID 0 の Object を受理できる候補に紐づくため
/// (OBJECTID_FILTER は header 時点では評価されない)、header の受理可否と紐づけ先の判定に使う。
fn first_live_candidate_passing_zero(
    specs: &[SubscriptionSpec],
    filters: &[FilterModel],
) -> Option<u64> {
    specs
        .iter()
        .enumerate()
        .find(|(index, spec)| spec.is_live() && filters[*index].passes(0))
        .map(|(_, spec)| spec.request_id)
}

/// 候補全体 (登録順) の帰属判定のモデル
///
/// 候補は Session の Track Alias 索引に載っている subscription、すなわち「キャンセル由来
/// `Terminated` に落ちていない subscription」である。キャンセル (`stop_sending`) した
/// subscription は索引から外れて `forget_subscription` で回収されるため、候補に含まれない。
/// 返り値は「帰属先の request_id」と「帰属先なし (`FilteredOut`) か」。
fn model_attribution(
    specs: &[SubscriptionSpec],
    filters: &[FilterModel],
    object_id: u64,
) -> Option<u64> {
    specs
        .iter()
        .zip(filters.iter())
        .find(|(spec, filter)| spec.is_live() && filter.passes(object_id))
        .map(|(spec, _)| spec.request_id)
}

/// open 中の受信 subgroup stream 1 本分のモデル
struct StreamSpec {
    /// Session に登録した stream id
    stream_id: DataStreamId,
    /// この stream の group_id
    group_id: u64,
    /// 次の Object に使う Object ID
    ///
    /// stream 内で単調増加させる。draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 2 の
    /// `check_object_after_fin` と、最大位置が単調最大になる性質の両方を満たす。
    next_object_id: u64,
    /// header 時に紐づいた (stream を所有する) subscription の request_id
    ///
    /// header の受理判定は Object ID を評価しないため、どの候補に紐づくかは最初に受理した
    /// Object の時点で確定する。最初の Object を受理するまでは `None` とする。
    owner: Option<u64>,
    /// この stream で最後に Object を受理した subscription の request_id
    ///
    /// キャンセル時に「生きた帰属先が残っているか」の判定に使う。
    last_attributed: Option<u64>,
    /// まだ終端していないか (会計に載っているか)
    open: bool,
    /// 生きた帰属先を失ったが Session がまだ会計を保持しているか
    ///
    /// キャンセル由来 `Terminated` の subscription が所有する stream は、Session が次の
    /// Object 受信 (または終端) まで `Subgroup` variant のまま保持するため会計も残る。
    /// 生きた帰属先が無いので以後の Object は破棄対象になる。
    draining: bool,
}

/// 同一 Track Alias を共有する subscription 群を確立する
///
/// `tests/test_session/subgroup_object_filter` と同じ手順で、登録順に候補が並ぶ
/// subscription を確立する。
///
/// Range Filter を送るには peer (server) が SETUP で MAX_FILTER_RANGES を宣言している
/// 必要がある (draft-ietf-moq-transport-21 §9.1.6 (MAX FILTER RANGES)) ため、SETUP 交換は
/// `establish_pair` ではなく本関数で行う。
fn establish_shared_alias_subs(
    alias: u64,
    filters: &[FilterModel],
) -> (Session, Vec<SubscriptionSpec>) {
    let mut client = Session::new_client(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let mut server = Session::new_server(Transport::WebTransport, max_filter_options())
        .expect("テストフィクスチャの前提条件を満たす");
    let client_setup = take_send_control(&mut client);
    let server_setup = take_send_control(&mut server);
    server
        .recv_control(client_setup)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(server_setup)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut specs = Vec::new();
    for filter in filters {
        let mut parameters = MessageParameters::new();
        if let FilterModel::ObjectIdRange { start, end } = *filter {
            parameters.push(object_id_filter(0, start, end));
        }
        let rid = client
            .send_subscribe(
                TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                b"cam".to_vec(),
                parameters,
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, subscribe) = take_send_request(&mut client);
        server
            .recv_request(subscribe)
            .expect("テストフィクスチャの前提条件を満たす");
        server
            .send_subscribe_ok(rid, alias, MessageParameters::new(), TrackProperties::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(rid, ok)
            .expect("テストフィクスチャの前提条件を満たす");
        specs.push(SubscriptionSpec {
            request_id: rid,
            cancelled: false,
            recovered: false,
            largest_received: None,
        });
    }
    // SUBSCRIBE が Established になったことを確認する (フィルタが peer の宣言を超える場合は
    // ここで失敗する)
    for spec in &specs {
        assert_eq!(
            client
                .subscription(spec.request_id)
                .expect("テストフィクスチャの前提条件を満たす")
                .state,
            SubscriptionState::Established
        );
    }
    (client, specs)
}

/// 全 subscription の `open_incoming_subgroup_count` 合計を返す
///
/// キャンセル済み subscription も Session 上は forget まで残るため合計に含める
/// (会計の移管・解放が漏れていれば値がずれる)。
fn total_open_incoming_subgroup_count(client: &Session) -> u64 {
    client
        .subscriptions()
        .map(|subscription| subscription.stream_counts.open_incoming_subgroup_count)
        .sum()
}

/// 全会計の不変条件を検証する
///
/// - `open_incoming_subgroup_count` の合計が現在 open 中の受信 subgroup stream 数と一致する
/// - 0 本の subscription に会計が張り付かない (stream を 1 本も持たない subscription の
///   open 数は常に 0)
fn assert_accounting(client: &Session, streams: &[StreamSpec]) {
    let expected = streams.iter().filter(|stream| stream.open).count() as u64;
    let actual = total_open_incoming_subgroup_count(client);
    assert_eq!(
        actual, expected,
        "open 中の受信 subgroup stream 数の合計がモデルと一致すること"
    );
}

/// Session 側で Terminated になった subscription をモデルへ同期する
///
/// 任意バイトの Properties による Malformed Track など、Session が subscription を
/// 終端した場合にモデルのキャンセル状態を追随させる。以後の Object は帰属対象から除外され、
/// この subscription が所有する stream は帰属実績が無ければ破棄対象として扱われる。
fn sync_cancelled_from_session(
    client: &Session,
    specs: &mut [SubscriptionSpec],
    filters: &[FilterModel],
    streams: &mut [StreamSpec],
) {
    // Session 側で Terminated になった subscription を集める
    let newly_cancelled: Vec<u64> = specs
        .iter()
        .filter(|spec| {
            !spec.cancelled
                && client
                    .subscription(spec.request_id)
                    .is_some_and(|subscription| subscription.state == SubscriptionState::Terminated)
        })
        .map(|spec| spec.request_id)
        .collect();
    if newly_cancelled.is_empty() {
        return;
    }
    for spec in specs.iter_mut() {
        if newly_cancelled.contains(&spec.request_id) {
            spec.cancelled = true;
        }
    }
    // 生きた帰属先が 1 つも残らなければ、以後の Object 受信で破棄対象になり会計が解放される。
    // 帰属先が別に生きている stream は帰属先にとって生きたままなので解放しない
    let has_live_candidate = first_live_candidate_passing_zero(specs, filters).is_some();
    if !has_live_candidate {
        for stream in streams.iter_mut() {
            if stream.open
                && stream
                    .owner
                    .is_some_and(|owner| newly_cancelled.contains(&owner))
                && stream.last_attributed.is_some()
            {
                stream.draining = true;
            }
        }
    }
}

/// per-subgroup delivery timeout override の 1 エントリ
///
/// キーは `(group_id, subgroup_id)`、値は `(subgroup_timeout, object_timeout)`。
type SubgroupOverride = ((u64, u64), (Option<u64>, Option<u64>));

/// subscription スコープの状態のスナップショット
///
/// `FilteredOut` / `Discarded` の Object が更新してはならない公開フィールドをまとめて比較する。
/// Object 系 tracker (`peer_object_properties` / `peer_object_fields`) は非公開のため、
/// 別テスト (`discarded_object_does_not_reach_object_trackers`) で後続 Object の
/// Malformed Track 検出として観測する。
#[derive(Debug, PartialEq)]
struct SubscriptionStateSnapshot {
    /// (group_id, object_id) に正規化した最大受信 Location
    largest_received: Option<(u64, u64)>,
    /// Group 終端 (存在しない最小 Object ID) の一覧
    ended_groups: Vec<(u64, u64)>,
    /// End of Track の位置
    end_of_track: Option<(u64, u64)>,
    /// per-subgroup delivery timeout override の一覧
    subgroup_overrides: Vec<SubgroupOverride>,
    /// 受信 data stream の累計数
    incoming_subgroup_count: u64,
    /// open 中の受信 data stream 数
    open_incoming_subgroup_count: u64,
    /// Forward State
    forward_state: u8,
    /// Terminated か
    terminated: bool,
}

/// 1 subscription 分のスナップショットを取る
fn snapshot_subscription(client: &Session, request_id: u64) -> SubscriptionStateSnapshot {
    let subscription = client
        .subscription(request_id)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut ended_groups: Vec<(u64, u64)> = subscription
        .ended_groups
        .iter()
        .map(|(&group_id, &object_id)| (group_id, object_id))
        .collect();
    ended_groups.sort_unstable();
    let mut subgroup_overrides: Vec<SubgroupOverride> = subscription
        .delivery_timeouts
        .subgroup_overrides
        .iter()
        .map(|(&key, &value)| (key, value))
        .collect();
    subgroup_overrides.sort_unstable();
    SubscriptionStateSnapshot {
        largest_received: subscription
            .largest_received_location
            .as_ref()
            .map(|location| (location.group_id, location.object_id)),
        ended_groups,
        end_of_track: subscription
            .end_of_track
            .as_ref()
            .map(|location| (location.group_id, location.object_id)),
        subgroup_overrides,
        incoming_subgroup_count: subscription.stream_counts.incoming_subgroup_count,
        open_incoming_subgroup_count: subscription.stream_counts.open_incoming_subgroup_count,
        forward_state: subscription.forward_state,
        terminated: subscription.state == SubscriptionState::Terminated,
    }
}

/// 操作列の 1 操作
enum Operation {
    /// 新しい subgroup stream を開いて header を受理させる
    OpenStream { group_id: u64, subgroup_id: u64 },
    /// 既存 stream に Object を送る (Object ID は stream 内で単調増加させる)
    RecvObject {
        stream_index: usize,
        properties: Option<Vec<u8>>,
    },
    /// 既存 stream を終端する
    CloseStream { stream_index: usize, reset: bool },
    /// subscription をキャンセルする (STOP_SENDING 送信)
    CancelSubscription { sub_index: usize },
    /// subscription を forget する (cleanup_ready な場合のみ成功する)
    ForgetSubscription { sub_index: usize },
}

/// 1 操作分の適用結果
struct StepOutcome {
    /// 返り値の受理判定 (Object 受信操作のみ `Some`)
    acceptance: Option<TrackDataAcceptance>,
    /// stream 終端 / forget により open 中の受信 stream が減ったか
    stream_released: bool,
}

/// subscription のキャンセルをモデルへ反映する
///
/// キャンセル由来 `Terminated` に属する stream は、以後の Object 受信時に生きた帰属先が
/// 無ければ破棄対象となって会計が解放される (Session は Object 1 個の時点で初めて
/// `Discarded` へ置き換えるため、本モデルも受理済み Object を持つ stream だけを解放する)。
/// 生きた帰属先が別に残っている stream は帰属先にとって生きたままなので解放しない。
/// 返り値は会計が解放された stream が 1 本以上あったか。
fn cancel_subscription(
    specs: &mut [SubscriptionSpec],
    filters: &[FilterModel],
    sub_index: usize,
    streams: &mut [StreamSpec],
) -> bool {
    specs[sub_index].cancelled = true;
    let forgotten = specs[sub_index].request_id;
    let has_live_candidate = first_live_candidate_passing_zero(specs, filters).is_some();
    let mut released = false;
    if !has_live_candidate {
        for stream in streams.iter_mut() {
            // 帰属実績のある stream だけが Session 側で破棄対象になり会計が解放される。
            // 帰属実績の無い stream は Session の `incoming` に残り計数も維持される
            if stream.open && stream.owner == Some(forgotten) && stream.last_attributed.is_some() {
                stream.draining = true;
                released = true;
            }
        }
    }
    released
}

/// open 中の stream から 1 本を一様に選ぶ
fn sample_live_stream(ctx: &mut noprop::TestCaseContext, streams: &[StreamSpec]) -> Option<usize> {
    let live: Vec<usize> = streams
        .iter()
        .enumerate()
        .filter(|(_, stream)| stream.open)
        .map(|(index, _)| index)
        .collect();
    if live.is_empty() {
        return None;
    }
    let choice = noprop::sample_usize_in(ctx, 0..live.len());
    Some(live[choice])
}

/// 操作列の適用に必要な可変状態
///
/// 引数が多くなりすぎないよう、1 ケース分の Session とモデルをまとめて持つ。
struct CaseState<'a> {
    /// 操作対象の Session
    client: &'a mut Session,
    /// 確立した subscription の Track Alias
    alias: u64,
    /// subscription のモデル
    specs: &'a mut [SubscriptionSpec],
    /// subscription ごとのフィルタのモデル
    filters: &'a [FilterModel],
    /// open 中の受信 subgroup stream のモデル
    streams: &'a mut Vec<StreamSpec>,
    /// 次に使う stream id
    next_stream_id: &'a mut u64,
}

/// 1 操作を Session とモデルの両方へ適用する
fn apply_operation(
    ctx: &mut noprop::TestCaseContext,
    state: &mut CaseState<'_>,
    operation: Operation,
) -> StepOutcome {
    let client = &mut *state.client;
    let alias = state.alias;
    let specs = &mut *state.specs;
    let filters = state.filters;
    let streams = &mut *state.streams;
    let next_stream_id = &mut *state.next_stream_id;
    match operation {
        Operation::OpenStream {
            group_id,
            subgroup_id,
        } => {
            let header = SubgroupHeader {
                track_alias: alias,
                group_id,
                subgroup_id: SubgroupIdMode::Explicit(subgroup_id),
                publisher_priority: Some(noprop::sample_u8(ctx)),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            };
            let stream_id = DataStreamId(*next_stream_id);
            *next_stream_id += 1;
            let acceptance = recv_header(client, stream_id, &header);
            if acceptance == TrackDataAcceptance::Accepted {
                streams.push(StreamSpec {
                    stream_id,
                    group_id,
                    next_object_id: 0,
                    owner: None,
                    last_attributed: None,
                    open: true,
                    draining: false,
                });
            } else {
                // 全候補がキャンセル済みなら新規 stream は破棄対象になり会計に載らない
                assert_eq!(
                    acceptance,
                    TrackDataAcceptance::Discarded,
                    "全候補キャンセル済みの header は Discarded になること"
                );
            }
            StepOutcome {
                acceptance: None,
                stream_released: false,
            }
        }
        Operation::RecvObject {
            stream_index,
            properties,
        } => {
            let stream = &mut streams[stream_index];
            // Object ID は stream ごとに単調増加させる。最大位置が単調最大になる性質と、
            // 同一 Subgroup の再オープンで `check_object_after_fin` を踏まない条件を満たす
            let object_id = stream.next_object_id;
            stream.next_object_id += 1;
            let stream_id = stream.stream_id;
            let group_id = stream.group_id;
            let object = match properties {
                Some(bytes) => object_with_properties(object_id, bytes),
                None => normal_object(object_id),
            };
            let result = client.recv_subgroup_object(stream_id, &object);

            // 帰属判定のモデル: 候補全体 (登録順) を評価し、最初に通過した生きた
            // subscription へ帰属する。合格がキャンセル由来のみなら Discarded、
            // どの候補も通過しなければ FilteredOut
            let expected_owner = model_attribution(specs, filters, object_id);
            let expected_acceptance = if expected_owner.is_some() {
                TrackDataAcceptance::Accepted
            } else {
                TrackDataAcceptance::FilteredOut
            };
            let any_cancelled = specs.iter().any(|spec| !spec.is_live());

            // 任意バイトの Properties は Malformed Track として subscription を終端しうる。
            // エラー時は受理判定を行わず、モデルだけを Session の観測値へ同期する
            match result {
                Ok(acceptance) => {
                    // キャンセル済み subscription は Track Alias 索引から外れて候補でなくなり、
                    // header 受理済みの stream は破棄対象へ切り替わる。破棄対象化の時点は
                    // Session 内部の判断に依存するため、キャンセルを伴うケースは受理判定の
                    // 一致を要求せず、会計の不変条件だけを検証する
                    if !any_cancelled {
                        assert_eq!(
                            acceptance, expected_acceptance,
                            "Object ID {object_id} の帰属判定が候補順のモデルと一致すること (group_id {group_id})"
                        );
                    }
                    // 受理された場合は必ず生きた候補が帰属先になる
                    if acceptance == TrackDataAcceptance::Accepted {
                        assert!(
                            expected_owner.is_some(),
                            "Accepted の帰属先は生きた候補であること (Object ID {object_id})"
                        );
                    }
                    // 受理がモデルの予測と一致した場合にだけ、帰属先の状態をモデルへ記録する
                    if acceptance == TrackDataAcceptance::Accepted
                        && acceptance == expected_acceptance
                        && let Some(expected_owner) = expected_owner
                    {
                        specs
                            .iter_mut()
                            .find(|spec| spec.request_id == expected_owner)
                            .expect("帰属先の subscription が存在すること")
                            .record_largest_received((group_id, object_id));
                        // header 時に確定できなかった所有者と、最後に受理した帰属先を記録する
                        streams[stream_index].owner = Some(expected_owner);
                        streams[stream_index].last_attributed = Some(expected_owner);
                    }
                }
                Err(_) => {
                    for spec in specs.iter_mut() {
                        spec.largest_received = client
                            .subscription(spec.request_id)
                            .and_then(|subscription| {
                                subscription.largest_received_location.as_ref()
                            })
                            .map(|location| (location.group_id, location.object_id));
                    }
                }
            }
            // 受理 / フィルタ不通過 / 破棄のいずれでも、最大位置は帰属先だけが単調に更新する
            for spec in specs.iter().filter(|spec| !spec.recovered) {
                let observed = client
                    .subscription(spec.request_id)
                    .and_then(|subscription| subscription.largest_received_location.as_ref())
                    .map(|location| (location.group_id, location.object_id));
                assert_eq!(
                    observed, spec.largest_received,
                    "largest_received_location がモデルと一致すること (request_id {})",
                    spec.request_id
                );
            }
            StepOutcome {
                acceptance: result.ok(),
                stream_released: false,
            }
        }
        Operation::CloseStream {
            stream_index,
            reset,
        } => {
            let stream = &mut streams[stream_index];
            let end = if reset {
                RequestStreamEnd::Reset {
                    error_code: 0,
                    reliable_size: None,
                }
            } else {
                RequestStreamEnd::Fin
            };
            client
                .recv_data_stream_closed(stream.stream_id, end)
                .expect("終端処理に成功すること");
            stream.open = false;
            stream.draining = false;
            StepOutcome {
                acceptance: None,
                stream_released: true,
            }
        }
        Operation::CancelSubscription { sub_index } => {
            assert!(
                specs[sub_index].is_live(),
                "キャンセル対象は生きた subscription であること"
            );
            client
                .stop_sending(specs[sub_index].request_id)
                .expect("Established の subscription は stop_sending できること");
            let released = cancel_subscription(specs, filters, sub_index, streams);
            StepOutcome {
                acceptance: None,
                stream_released: released,
            }
        }
        Operation::ForgetSubscription { sub_index } => {
            if specs[sub_index].is_live() {
                // キャンセルしていない subscription は cleanup_ready にならないため、
                // forget は失敗してモデルは変化しない
                assert!(
                    client
                        .forget_subscription(specs[sub_index].request_id)
                        .is_none(),
                    "Established の subscription は forget できないこと"
                );
                return StepOutcome {
                    acceptance: None,
                    stream_released: false,
                };
            }
            let forgotten = specs[sub_index].request_id;
            // open 中の stream を持つ subscription は `cleanup_ready` が false のため
            // forget できない。その場合 Session は何も変えないので、モデルも変えない
            if client.forget_subscription(forgotten).is_none() {
                return StepOutcome {
                    acceptance: None,
                    stream_released: false,
                };
            }
            // 回収した subscription は Session から消えるため、モデルも観測対象から外す
            specs[sub_index].recovered = true;
            specs[sub_index].largest_received = None;
            let mut released = false;
            match specs
                .iter()
                .find(|candidate| candidate.is_live())
                .map(|candidate| candidate.request_id)
            {
                // 生きた他候補へ移管される (会計も移管先へ移る)
                Some(target) => {
                    for stream in streams.iter_mut().filter(|stream| stream.open) {
                        if stream.owner == Some(forgotten) {
                            stream.owner = Some(target);
                        }
                    }
                }
                // 移管先が無ければ session は stream を除去する。会計は
                // `register_discarded_stream` を通った (帰属実績がある) stream だけが解放され、
                // 帰属実績の無い stream は Session 側の `incoming` から消えても計数が残る
                None => {
                    for stream in streams.iter_mut().filter(|stream| stream.open) {
                        if stream.last_attributed.is_none() {
                            continue;
                        }
                        stream.open = false;
                        stream.draining = false;
                        released = true;
                    }
                }
            }
            StepOutcome {
                acceptance: None,
                stream_released: released,
            }
        }
    }
}

/// 受信 subgroup stream の会計不変条件を検証する
///
/// 受信 subgroup stream の open / Object 受信 / 終端 / 破棄対象化と subscription の
/// キャンセル / forget の任意の組合せで、`open_incoming_subgroup_count` の合計が現在 open
/// 中の stream 数と一致し続けることを検証する。会計が漏れると `cleanup_ready()` が
/// `open_incoming_subgroup_count == 0` を要求するために subscription が永久に回収できない
/// (subscription リーク)。
#[test]
fn subgroup_receive_stream_accounting() -> noprop::TestResult {
    // 各分岐の観測をカバレッジゲートで検証する
    let accepted_seen = std::cell::Cell::new(false);
    let filtered_out_seen = std::cell::Cell::new(false);
    let discarded_seen = std::cell::Cell::new(false);
    let close_seen = std::cell::Cell::new(false);
    let forget_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let alias = noprop::sample_u64_in(ctx, 1..1024);
        let sub_count = noprop::sample_usize_in(ctx, 2..=3);
        // 先頭の候補は unfiltered にして受理経路を必ず観測できるようにし、以降は
        // OBJECTID_FILTER を設定して FilteredOut の候補を作る
        let filters: Vec<FilterModel> = (0..sub_count)
            .map(|index| {
                if index == 0 {
                    FilterModel::Unfiltered
                } else {
                    let start = noprop::sample_u64_in(ctx, 0..16);
                    let end = start + noprop::sample_u64_in(ctx, 0..8);
                    FilterModel::ObjectIdRange { start, end }
                }
            })
            .collect();
        let (mut client, mut specs) = establish_shared_alias_subs(alias, &filters);

        let op_count = noprop::sample_usize_in(ctx, 8..24);
        let mut streams: Vec<StreamSpec> = Vec::new();
        let mut next_stream_id: u64 = 1;
        let mut next_group_id: u64 = 0;

        for _ in 0..op_count {
            // 実行可能な操作だけを候補にし、重み付きで 1 つ選ぶ。open 中の stream が無い
            // ときは subscription の状態だけを動かし (open / cancel / forget)、あるときは
            // stream の操作だけを行う
            let open_stream_count = streams.iter().filter(|stream| stream.open).count();
            let operation = if open_stream_count == 0 {
                match noprop::sample_weighted_index(ctx, &[6, 2, 2]) {
                    0 => {
                        let group_id = next_group_id;
                        next_group_id += 1;
                        Operation::OpenStream {
                            group_id,
                            subgroup_id: next_group_id,
                        }
                    }
                    1 => match specs.iter().position(|spec| spec.is_live()) {
                        Some(index) => Operation::CancelSubscription { sub_index: index },
                        // 全てキャンセル済みなら forget に倒す
                        None => {
                            let index = noprop::sample_usize_in(ctx, 0..sub_count);
                            Operation::ForgetSubscription { sub_index: index }
                        }
                    },
                    _ => {
                        let index = noprop::sample_usize_in(ctx, 0..sub_count);
                        Operation::ForgetSubscription { sub_index: index }
                    }
                }
            } else {
                match noprop::sample_weighted_index(ctx, &[4, 6, 3]) {
                    0 => {
                        let group_id = next_group_id;
                        next_group_id += 1;
                        Operation::OpenStream {
                            group_id,
                            subgroup_id: next_group_id,
                        }
                    }
                    1 => {
                        let stream_index = sample_live_stream(ctx, &streams)
                            .expect("open 中の stream が 1 本以上あること");
                        // Properties は decode 失敗経路 (Malformed Track) を含めない
                        // 正常な形だけを生成し、会計の不変条件だけを観測する
                        // (Properties のエラーパスは単体テストで固定する)
                        let properties = if noprop::sample_ratio(ctx, noprop::Ratio::one_nth(4)) {
                            Some(delivery_timeout_properties(
                                noprop::sample_u64_in(ctx, 1..1_000),
                                noprop::sample_u64_in(ctx, 1..1_000),
                            ))
                        } else {
                            None
                        };
                        Operation::RecvObject {
                            stream_index,
                            properties,
                        }
                    }
                    _ => {
                        let stream_index = sample_live_stream(ctx, &streams)
                            .expect("open 中の stream が 1 本以上あること");
                        Operation::CloseStream {
                            stream_index,
                            reset: noprop::sample_bool(ctx),
                        }
                    }
                }
            };

            let outcome = apply_operation(
                ctx,
                &mut CaseState {
                    client: &mut client,
                    alias,
                    specs: &mut specs,
                    filters: &filters,
                    streams: &mut streams,
                    next_stream_id: &mut next_stream_id,
                },
                operation,
            );
            if let Some(acceptance) = outcome.acceptance {
                match acceptance {
                    TrackDataAcceptance::Accepted => accepted_seen.set(true),
                    TrackDataAcceptance::FilteredOut => filtered_out_seen.set(true),
                    TrackDataAcceptance::Discarded => discarded_seen.set(true),
                    _ => {}
                }
            }
            if outcome.stream_released {
                close_seen.set(true);
            }
            sync_cancelled_from_session(&client, &mut specs, &filters, &mut streams);
            assert_accounting(&client, &streams);
        }

        // 残った stream を全て終端する。全 stream 終端 / forget 後に会計が 0 に戻り、
        // delivery timeout override が残らないことを検証する
        for stream in streams.iter().filter(|stream| stream.open) {
            let end = if noprop::sample_bool(ctx) {
                RequestStreamEnd::Fin
            } else {
                RequestStreamEnd::Reset {
                    error_code: 0,
                    reliable_size: None,
                }
            };
            client
                .recv_data_stream_closed(stream.stream_id, end)
                .expect("終端処理に成功すること");
        }
        for spec in specs.iter().filter(|spec| !spec.is_live()) {
            if client.forget_subscription(spec.request_id).is_some() {
                forget_seen.set(true);
            }
        }
        assert_eq!(
            total_open_incoming_subgroup_count(&client),
            0,
            "全 stream 終端 / forget 後に open 中の受信 stream 数が 0 になること"
        );
        for spec in specs.iter().filter(|spec| spec.is_live()) {
            let subscription = client
                .subscription(spec.request_id)
                .expect("キャンセルしていない subscription は残ること");
            assert!(
                subscription.delivery_timeouts.subgroup_overrides.is_empty(),
                "全 stream 終端後に delivery timeout override が残らないこと (request_id {})",
                spec.request_id
            );
            assert_eq!(
                subscription.stream_counts.open_incoming_subgroup_count, 0,
                "全 stream 終端後に open 中の受信 stream 数が 0 になること (request_id {})",
                spec.request_id
            );
        }
        assert_eq!(client.state(), SessionState::Established);
        Ok(())
    })?;
    assert!(
        accepted_seen.get(),
        "Object が受理されるケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        filtered_out_seen.get(),
        "Object が FilteredOut になるケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        discarded_seen.get(),
        "Object が Discarded になるケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        close_seen.get(),
        "stream 終端で会計が戻るケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        forget_seen.get(),
        "forget で subscription が回収されるケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// 帰属先が候補順で決定的であることを検証する
///
/// 各 subscription に互いに重複しない OBJECTID_FILTER (2 個ずつの連続値) を設定し、任意の
/// Object ID に対する帰属先を一意に定める。どの候補も合格しない場合は `FilteredOut` になり、
/// subscription スコープの状態を更新しない。
#[test]
fn subgroup_object_attribution_is_deterministic() -> noprop::TestResult {
    // 受理とフィルタ不通過の両方の観測をカバレッジゲートで検証する
    let accepted_seen = std::cell::Cell::new(false);
    let filtered_out_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let alias = noprop::sample_u64_in(ctx, 1..1024);
        let sub_count = noprop::sample_usize_in(ctx, 2..=4);
        // 互いに重複しない区間を登録順に割り当てる。各区間は 2 個の連続値を持つ
        let base = noprop::sample_u64_in(ctx, 0..4);
        let filters: Vec<FilterModel> = (0..sub_count)
            .map(|index| {
                let start = base + 2 * index as u64;
                FilterModel::ObjectIdRange {
                    start,
                    end: start + 1,
                }
            })
            .collect();
        // 全区間の外側の Object ID も生成できるように上限を広げる
        let object_id_upper = base + 2 * sub_count as u64 + noprop::sample_u64_in(ctx, 1..6);
        let (mut client, specs) = establish_shared_alias_subs(alias, &filters);

        let stream_count = noprop::sample_usize_in(ctx, 1..3);
        for (stream_index, stream_id) in (0..stream_count).zip(1u64..) {
            let header = SubgroupHeader {
                track_alias: alias,
                group_id: stream_index as u64,
                subgroup_id: SubgroupIdMode::Explicit(stream_index as u64),
                publisher_priority: Some(noprop::sample_u8(ctx)),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            };
            let stream_id = DataStreamId(stream_id);
            assert_eq!(
                recv_header(&mut client, stream_id, &header),
                TrackDataAcceptance::Accepted,
                "OBJECTID_FILTER は header 時点では評価されないため header は受理されること"
            );

            let object_count = noprop::sample_usize_in(ctx, 2..6);
            // 最大位置が単調最大になるため、Object ID は stream 内で単調増加させる
            let mut object_id = noprop::sample_u64_in(ctx, 0..object_id_upper);
            for _ in 0..object_count {
                let before: Vec<SubscriptionStateSnapshot> = specs
                    .iter()
                    .map(|spec| snapshot_subscription(&client, spec.request_id))
                    .collect();
                let acceptance = client
                    .recv_subgroup_object(stream_id, &normal_object(object_id))
                    .expect("Object 受信に失敗しないこと");

                let expected_owner = model_attribution(&specs, &filters, object_id);
                let expected = if expected_owner.is_some() {
                    TrackDataAcceptance::Accepted
                } else {
                    TrackDataAcceptance::FilteredOut
                };
                assert_eq!(
                    acceptance, expected,
                    "Object ID {object_id} の帰属判定が候補順のモデルと一致すること"
                );

                for (index, spec) in specs.iter().enumerate() {
                    let after = snapshot_subscription(&client, spec.request_id);
                    if expected_owner == Some(spec.request_id) {
                        assert_eq!(
                            after.largest_received,
                            Some((stream_index as u64, object_id)),
                            "帰属先 (request_id {}) だけが largest_received_location を更新すること",
                            spec.request_id
                        );
                    } else {
                        assert_eq!(
                            after, before[index],
                            "帰属先以外の subscription スコープの状態が更新されないこと (request_id {})",
                            spec.request_id
                        );
                    }
                }
                if expected_owner.is_some() {
                    accepted_seen.set(true);
                } else {
                    filtered_out_seen.set(true);
                }
                // 次の Object は区間をまたいで進め、通過先が切り替わる組合せも作る
                object_id += noprop::sample_u64_in(ctx, 1..(object_id_upper + 1));
            }
            client
                .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
                .expect("終端処理に成功すること");
        }
        assert_accounting(&client, &[]);
        assert_eq!(client.state(), SessionState::Established);
        Ok(())
    })?;
    assert!(
        accepted_seen.get(),
        "Object が帰属先へ受理されるケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        filtered_out_seen.get(),
        "どの候補も通過しない Object が FilteredOut になるケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// フィルタ不通過 / 破棄の Object が Object 系 tracker に到達しないことを検証する
///
/// `peer_object_properties` (`ObjectPropertyTracker`) は非公開のため、PRIOR_OBJECT_ID_GAP を
/// 持つ Object を観測させたうえで、その gap に入る Object が Malformed Track として検出される
/// かどうかで到達を観測する。フィルタ不通過 / 破棄の Object を tracker が観測していれば、
/// 後続 Object が `PROTOCOL_VIOLATION` で subscription を終端する。
/// 参照ケースとして、キャンセルせずフィルタにも通す Object では同じ入力が Malformed Track に
/// なることも確認し、「観測されない」ことが tracker の未到達によるものであることを示す。
#[test]
fn discarded_object_does_not_reach_object_trackers() -> noprop::TestResult {
    // 破棄されるケースと tracker に観測される参照ケースの観測を検証する
    let observed_seen = std::cell::Cell::new(false);
    let unobserved_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let alias = noprop::sample_u64_in(ctx, 1..1024);
        // discard_kind 0: フィルタ不通過 (FilteredOut) / 1: 全候補キャンセル済み (Discarded)
        // / 2: キャンセルもフィルタもしない参照ケース (tracker に観測される)
        let discard_kind = noprop::sample_usize_in(ctx, 0..3);
        let (filter, stream_acceptance) = match discard_kind {
            // gap 宣言 Object (Object ID 5) は範囲外、gap 内の Object (Object ID 3) は範囲内。
            // header 時点では OBJECTID_FILTER を評価しないため header は受理される
            0 => (
                FilterModel::ObjectIdRange { start: 1, end: 4 },
                TrackDataAcceptance::Accepted,
            ),
            // 全候補キャンセル済みのため header も Object も破棄対象になる
            1 => (FilterModel::Unfiltered, TrackDataAcceptance::Discarded),
            // 参照ケース: どちらも通過する
            _ => (FilterModel::Unfiltered, TrackDataAcceptance::Accepted),
        };
        let (mut client, specs) = establish_shared_alias_subs(alias, &[filter]);
        if discard_kind == 1 {
            client
                .stop_sending(specs[0].request_id)
                .expect("Established の subscription は stop_sending できること");
        }

        let stream_id = DataStreamId(1);
        let header = SubgroupHeader {
            track_alias: alias,
            group_id: 0,
            subgroup_id: SubgroupIdMode::Explicit(0),
            publisher_priority: Some(128),
            has_properties: false,
            end_of_group: false,
            first_object: false,
        };
        assert_eq!(
            recv_header(&mut client, stream_id, &header),
            stream_acceptance,
            "header の受理判定が期待どおりであること"
        );

        // gap 宣言 Object: PRIOR_OBJECT_ID_GAP=5 は Object ID 1..5 が存在しないことを
        // 宣言する。宣言 Object 自身は Object ID 5 とする
        let gap_object = object_with_properties(5, prior_object_id_gap_properties(5));
        let gap_acceptance = client
            .recv_subgroup_object(stream_id, &gap_object)
            .expect("gap 宣言 Object の受信に失敗しないこと");
        assert_eq!(
            gap_acceptance,
            match discard_kind {
                0 => TrackDataAcceptance::FilteredOut,
                1 => TrackDataAcceptance::Discarded,
                _ => TrackDataAcceptance::Accepted,
            },
            "gap 宣言 Object の受理判定が期待どおりであること"
        );

        // gap 内の Object ID 3 を送る。gap 宣言 Object が tracker に観測されていれば
        // Malformed Track (PROTOCOL_VIOLATION) で subscription が終端される
        match client.recv_subgroup_object(stream_id, &normal_object(3)) {
            Ok(acceptance) => {
                assert!(
                    discard_kind != 2,
                    "参照ケースでは gap 内の Object が Malformed Track になること"
                );
                // gap 宣言 Object が tracker に観測されていなければ、gap 内の Object も
                // Malformed Track にならず、帰属すれば受理・破棄対象なら Discarded になる
                assert!(
                    matches!(
                        acceptance,
                        TrackDataAcceptance::Accepted | TrackDataAcceptance::Discarded
                    ),
                    "破棄された Object は tracker を更新しないこと"
                );
                unobserved_seen.set(true);
            }
            Err(err) => {
                assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
                assert_eq!(
                    discard_kind, 2,
                    "破棄された gap 宣言 Object が tracker に観測されてはならない"
                );
                assert_eq!(
                    client
                        .subscription(specs[0].request_id)
                        .expect("subscription が存在すること")
                        .state,
                    SubscriptionState::Terminated,
                    "Malformed Track は該当 subscription だけを終端すること"
                );
                observed_seen.set(true);
            }
        }
        assert_eq!(
            client.state(),
            SessionState::Established,
            "Malformed Track でセッションは閉じないこと"
        );
        Ok(())
    })?;
    assert!(
        observed_seen.get(),
        "tracker に観測されて Malformed Track になる参照ケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        unobserved_seen.get(),
        "破棄された Object が tracker を更新しないケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// fuzz の `Op` が生成する操作列と同じ手順で subgroup 経路が panic しないことを検証する
///
/// `fuzz/fuzz_targets/fuzz_session.rs` は入力バイト列を `SubgroupHeader::decode` /
/// `FetchHeader::decode` してから Session へ渡し、decode 済みの構造体を
/// `recv_subgroup_object` に渡す。ランダムな操作列では header 受理まで到達しないことが多い
/// ため、受理済み subscriber を用意したうえで「decode した header を通知し、decode 済みの
/// Object を通知する」手順そのものを property として反復し、panic と会計の破壊がないことを
/// 確認する。
#[test]
fn decoded_subgroup_header_and_object_do_not_break_session() -> noprop::TestResult {
    // 受理・フィルタ不通過・破棄の各分岐が 1 回以上観測されることを検証する
    let accepted_seen = std::cell::Cell::new(false);
    let filtered_out_seen = std::cell::Cell::new(false);
    let discarded_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let alias = noprop::sample_u64_in(ctx, 1..1024);
        // 候補ごとに通す Object ID の範囲を変え、同じ stream でも Object ごとに
        // 受理 / フィルタ不通過が切り替わるようにする
        let filters = vec![
            FilterModel::ObjectIdRange { start: 0, end: 3 },
            FilterModel::ObjectIdRange { start: 8, end: 11 },
        ];
        let (mut client, specs) = establish_shared_alias_subs(alias, &filters);

        let stream_id = DataStreamId(1);
        let header = SubgroupHeader {
            track_alias: alias,
            group_id: 0,
            subgroup_id: SubgroupIdMode::Explicit(0),
            publisher_priority: Some(128),
            has_properties: false,
            end_of_group: false,
            first_object: false,
        };
        // fuzz と同じくエンコード済みバイト列から decode してから通知する
        let encoded = header.encode();
        let (decoded, consumed) =
            SubgroupHeader::decode(&encoded).expect("encode した header は decode できること");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, header);
        // fuzz と同じく stream type は header の先頭バイトから作る
        client
            .recv_data_stream_type(stream_id, encoded[0] as u64)
            .expect("subgroup stream type の通知に成功すること");
        assert_eq!(
            client
                .recv_subgroup_header(stream_id, &decoded)
                .expect("subgroup header の受理に失敗しないこと"),
            TrackDataAcceptance::Accepted
        );

        // fuzz と同じく decode 済み構造体を直接渡す (payload 長 1 / status なし)。
        // Object ID 0..=3 は 1 番目、8..=11 は 2 番目の候補が通り、間の ID はどの候補も通らない
        let object_count = noprop::sample_usize_in(ctx, 8..14);
        for object_id in 0..object_count as u64 {
            let acceptance = client
                .recv_subgroup_object(stream_id, &normal_object(object_id))
                .expect("subgroup object の受信に失敗しないこと");
            let expected_owner = model_attribution(&specs, &filters, object_id);
            let expected = if expected_owner.is_some() {
                TrackDataAcceptance::Accepted
            } else {
                TrackDataAcceptance::FilteredOut
            };
            assert_eq!(
                acceptance, expected,
                "Object ID {object_id} の受理判定が候補順のモデルと一致すること"
            );
            match acceptance {
                TrackDataAcceptance::Accepted => accepted_seen.set(true),
                TrackDataAcceptance::FilteredOut => filtered_out_seen.set(true),
                TrackDataAcceptance::Discarded => discarded_seen.set(true),
                _ => {}
            }
        }

        // 全候補キャンセル後の header / object は破棄対象として no-op で吸収される
        for spec in &specs {
            client
                .stop_sending(spec.request_id)
                .expect("Established の subscription は stop_sending できること");
        }
        let stream_id2 = DataStreamId(2);
        client
            .recv_data_stream_type(stream_id2, encoded[0] as u64)
            .expect("subgroup stream type の通知に成功すること");
        assert_eq!(
            client
                .recv_subgroup_header(stream_id2, &decoded)
                .expect("破棄対象 header の受理に失敗しないこと"),
            TrackDataAcceptance::Discarded
        );
        assert_eq!(
            client
                .recv_subgroup_object(stream_id2, &normal_object(0))
                .expect("破棄対象 object の受信に失敗しないこと"),
            TrackDataAcceptance::Discarded
        );
        discarded_seen.set(true);

        // 終端後は会計が 0 に戻る
        for stream_id in [stream_id, stream_id2] {
            client
                .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
                .expect("終端処理に成功すること");
        }
        assert_accounting(&client, &[]);
        assert_eq!(client.state(), SessionState::Established);
        Ok(())
    })?;
    assert!(
        accepted_seen.get(),
        "Object が受理されるケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        filtered_out_seen.get(),
        "Object が FilteredOut になるケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        discarded_seen.get(),
        "Object が Discarded になるケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// 受信 subgroup / fill fetch stream の会計と delivery timeout override のライフサイクル
///
/// 先頭 Object の SUBGROUP_DELIVERY_TIMEOUT / OBJECT_DELIVERY_TIMEOUT が帰属先
/// subscription の per-subgroup override として登録され、stream 終端 / forget 後に
/// 残留しないことを検証する。fill fetch stream も Stream Count の open 数に含まれるため
/// (draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE))、subgroup stream と同じ会計を通る
/// ことを合わせて検証する。
#[test]
fn subgroup_delivery_timeout_override_lifecycle() -> noprop::TestResult {
    // override の登録 / 削除と fill fetch の会計の観測をカバレッジゲートで検証する
    let registered_seen = std::cell::Cell::new(false);
    let removed_seen = std::cell::Cell::new(false);
    let fill_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let alias = noprop::sample_u64_in(ctx, 1..1024);
        let (mut client, specs) = establish_shared_alias_subs(alias, &[FilterModel::Unfiltered]);
        let rid = specs[0].request_id;
        let subgroup_ms = noprop::sample_u64_in(ctx, 1..1_000);
        let object_ms = noprop::sample_u64_in(ctx, 1..1_000);

        // subgroup stream を開き、先頭 Object に delivery timeout Property を付ける
        let stream_id = DataStreamId(1);
        let group_id = noprop::sample_u64_in(ctx, 0..8);
        let subgroup_id = noprop::sample_u64_in(ctx, 0..8);
        let header = SubgroupHeader {
            track_alias: alias,
            group_id,
            subgroup_id: SubgroupIdMode::Explicit(subgroup_id),
            publisher_priority: Some(128),
            has_properties: true,
            end_of_group: false,
            first_object: false,
        };
        assert_eq!(
            recv_header(&mut client, stream_id, &header),
            TrackDataAcceptance::Accepted
        );
        assert_eq!(
            total_open_incoming_subgroup_count(&client),
            1,
            "header 受理で open 中の受信 stream 数が 1 になること"
        );
        let first_object_id = noprop::sample_u64_in(ctx, 0..8);
        assert_eq!(
            client
                .recv_subgroup_object(
                    stream_id,
                    &object_with_properties(
                        first_object_id,
                        delivery_timeout_properties(subgroup_ms, object_ms)
                    )
                )
                .expect("Object 受信に失敗しないこと"),
            TrackDataAcceptance::Accepted
        );
        assert_eq!(
            client
                .subscription(rid)
                .expect("subscription が存在すること")
                .delivery_timeouts
                .subgroup_overrides
                .get(&(group_id, subgroup_id)),
            Some(&(Some(subgroup_ms), Some(object_ms))),
            "先頭 Object の delivery timeout が per-subgroup override として登録されること"
        );
        registered_seen.set(true);

        // FIN で終端すると override が削除され、会計が戻る
        client
            .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
            .expect("終端処理に成功すること");
        assert!(
            client
                .subscription(rid)
                .expect("subscription が存在すること")
                .delivery_timeouts
                .subgroup_overrides
                .is_empty(),
            "stream 終端で override が削除されること"
        );
        assert_eq!(
            total_open_incoming_subgroup_count(&client),
            0,
            "stream 終端で open 中の受信 stream 数が 0 に戻ること"
        );
        removed_seen.set(true);

        // fill fetch stream も同じ会計を通る
        let fill_stream_id = DataStreamId(2);
        client
            .recv_data_stream_type(fill_stream_id, FETCH_HEADER_TYPE)
            .expect("fetch stream type の通知に成功すること");
        client
            .recv_fetch_header(fill_stream_id, &FetchHeader { request_id: rid })
            .expect("fill fetch の FETCH_HEADER は受理されること");
        assert_eq!(
            total_open_incoming_subgroup_count(&client),
            1,
            "fill fetch stream も open 中の受信 stream 数に含まれること"
        );
        client
            .recv_data_stream_closed(fill_stream_id, RequestStreamEnd::Fin)
            .expect("fill fetch stream の終端処理に成功すること");
        assert_eq!(
            total_open_incoming_subgroup_count(&client),
            0,
            "fill fetch stream の終端で open 中の受信 stream 数が 0 に戻ること"
        );
        fill_seen.set(true);

        // 未終端のまま subscription をキャンセル / forget しても override が残らない
        let stream_id2 = DataStreamId(3);
        let header2 = SubgroupHeader {
            track_alias: alias,
            group_id: group_id + 1,
            subgroup_id: SubgroupIdMode::Explicit(subgroup_id + 1),
            publisher_priority: Some(128),
            has_properties: true,
            end_of_group: false,
            first_object: false,
        };
        assert_eq!(
            recv_header(&mut client, stream_id2, &header2),
            TrackDataAcceptance::Accepted
        );
        assert_eq!(
            client
                .recv_subgroup_object(
                    stream_id2,
                    &object_with_properties(
                        100,
                        delivery_timeout_properties(subgroup_ms, object_ms)
                    )
                )
                .expect("Object 受信に失敗しないこと"),
            TrackDataAcceptance::Accepted
        );
        assert!(
            !client
                .subscription(rid)
                .expect("subscription が存在すること")
                .delivery_timeouts
                .subgroup_overrides
                .is_empty(),
            "2 本目の stream でも override が登録されること"
        );
        client
            .stop_sending(rid)
            .expect("Established の subscription は stop_sending できること");
        client
            .forget_subscription(rid)
            .expect("キャンセル由来 Terminated は cleanup_ready で forget できること");
        assert!(client.subscription(rid).is_none());
        assert_eq!(
            total_open_incoming_subgroup_count(&client),
            0,
            "forget で open 中の受信 stream 数が 0 に戻ること"
        );
        assert_eq!(client.state(), SessionState::Established);
        Ok(())
    })?;
    assert!(
        registered_seen.get(),
        "delivery timeout override が登録されるケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        removed_seen.get(),
        "stream 終端で override が削除されるケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        fill_seen.get(),
        "fill fetch stream の会計を検証するケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}
