//! 受信 subgroup Object への Object 単位フィルタ再適用のテスト
//!
//! draft-ietf-moq-transport-21 §3.1 (Subscriptions):
//! "Because subscriptions can share a Track Alias, the subscriber re-applies each
//! subscription's filter to determine which subscription a received Object belongs to."
//!
//! §3.3.3 (Combining Filters): "Pass = Forward AND Location Filters AND Range Filters"。
//! header 受理時に評価できるフィルタだけでは Object の帰属先を確定できないため、
//! Object 受信時に候補すべてへフィルタを再適用する。本モジュールはその振り分けと、
//! フィルタ不通過 Object が subscription スコープの状態を更新しないことを検証する。
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
//!
//! 構成 (関心ごとに子モジュールへ分割する):
//! - `routing`: フィルタによる Object の振り分けとフィルタ不通過 Object の扱い
//! - `cancellation`: キャンセル済み subscription への Object の破棄と stream 会計
//! - `group_end`: END_OF_GROUP + FIN による Group 終端確定の帰属先への反映
//! - `override_lifecycle`: 先頭 Object の delivery timeout override の登録と削除
//! - `priority`: 候補ごとの SUBGROUP_HEADER priority による PRIORITY_FILTER の解決
//!
//! subscribe 確立 / SubgroupHeader 生成 / DecodedSubgroupObject 生成 / フィルタ
//! パラメータ生成の共通ヘルパは `tests/test_session.rs` に置く。

use super::*;
use shiguredo_moqt::message_parameter::{LocationFilter, PARAM_LOCATION_FILTER};

/// PUBLISH_DONE (stream_count 0) を送受信し、drain timer を満了させる
///
/// publisher が申告した stream_count 0 より受信 stream 数が多いため
/// `stream_count_overrun` が立ち、open 中の受信 stream が残っていれば
/// `cleanup_ready` は false になる。キャンセル由来 `Terminated` の `cleanup_ready` が
/// 常に true になる性質に依存せず、open 数の計上漏れを検出できる。
fn send_publish_done_and_expire_drain(client: &mut Session, server: &mut Session, rid: u64) {
    server
        .send_publish_done(
            rid,
            0x2,
            0,
            shiguredo_moqt::message::ReasonPhrase::new("ended")
                .expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("PUBLISH_DONE の送信に成功すること");
    let (_, done_msg) = take_send_on_stream(server);
    client
        .recv_stream_message(rid, done_msg)
        .expect("PUBLISH_DONE の受信に成功すること");
    // drain timer を満了させる (delivery timeout 未設定なら 0 で即満了になる)
    client.tick(1_000);
}

/// 同一 alias を共有する 3 subscription を確立して (client, rid1, rid2, rid3) を返す
///
/// 候補の評価順は登録順 (rid1 → rid2 → rid3) になる。
fn establish_shared_alias_three(
    alias: u64,
    parameters1: MessageParameters,
    parameters2: MessageParameters,
    parameters3: MessageParameters,
) -> (Session, u64, u64, u64) {
    let (mut client, mut server) =
        establish_pair_with_options(SetupOptions::new(), max_filter_options());
    let rid1 = send_and_recv_subscribe_with_parameters(
        &mut client,
        &mut server,
        &[b"live"],
        b"cam",
        parameters1,
    );
    let rid2 = send_and_recv_subscribe_with_parameters(
        &mut client,
        &mut server,
        &[b"live"],
        b"cam",
        parameters2,
    );
    let rid3 = send_and_recv_subscribe_with_parameters(
        &mut client,
        &mut server,
        &[b"live"],
        b"cam",
        parameters3,
    );
    complete_subscribe(&mut client, &mut server, rid1, alias);
    complete_subscribe(&mut client, &mut server, rid2, alias);
    complete_subscribe(&mut client, &mut server, rid3, alias);
    (client, rid1, rid2, rid3)
}

/// client (subscriber) 側で subgroup stream を登録し、header の受理結果を返す
fn recv_header(
    client: &mut Session,
    stream_id: DataStreamId,
    header: &SubgroupHeader,
) -> TrackDataAcceptance {
    client
        .recv_data_stream_type(stream_id, header.encode()[0] as u64)
        .expect("subgroup stream type の通知に成功すること");
    client
        .recv_subgroup_header(stream_id, header)
        .expect("subgroup header の受理に成功すること")
}

/// PRIOR_OBJECT_ID_GAP (0x3E) を 1 つ持つ Object Properties のバイト列を作る
fn prior_object_id_gap_properties(value: u64) -> Vec<u8> {
    encode_properties([ObjectProperty {
        prop_type: PROP_PRIOR_OBJECT_ID_GAP,
        value: ObjectPropertyValue::VarInt(value),
    }])
}

/// LOCATION_FILTER パラメータを作る
fn location_filter_parameter(filter: LocationFilter) -> MessageParameter {
    MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(filter.encode_to_bytes()),
    }
}

#[path = "subgroup_object_filter/routing.rs"]
mod routing;

#[path = "subgroup_object_filter/cancellation.rs"]
mod cancellation;

#[path = "subgroup_object_filter/group_end.rs"]
mod group_end;

#[path = "subgroup_object_filter/override_lifecycle.rs"]
mod override_lifecycle;

#[path = "subgroup_object_filter/priority.rs"]
mod priority;
