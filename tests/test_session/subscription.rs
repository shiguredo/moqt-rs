//! subscription 関連のテスト群
use super::*;

/// `Session::recv_stream_message` を短く呼ぶためのヘルパー
fn recv_response_helper(session: &mut Session, request_id: u64, msg: ControlMessage) {
    session
        .recv_stream_message(request_id, msg)
        .expect("テストフィクスチャの前提条件を満たす");
}

/// 注入用の REQUEST_UPDATE を作る
///
/// 送信 API が先に弾いてしまうケースを受信側で検証するため、wire を経由せず
/// 直接メッセージを組み立てる。
fn inject_request_update(request_id: u64, parameters: MessageParameters) -> ControlMessage {
    ControlMessage::RequestUpdate(shiguredo_moqt::message::RequestUpdate {
        request_id,
        parameters,
    })
}

/// Range 1 個の SUBGROUP_FILTER (SetID=0, Start=0, End_delta=1)
fn one_range_subgroup_filter() -> MessageParameters {
    let mut params = MessageParameters::new();
    params.push(super::one_range_subgroup_filter_with_set_id(0));
    params
}

#[path = "subscription/delivery.rs"]
mod delivery;
#[path = "subscription/handshake.rs"]
mod handshake;
#[path = "subscription/largest_location.rs"]
mod largest_location;
#[path = "subscription/publish_done.rs"]
mod publish_done;
#[path = "subscription/publish_params.rs"]
mod publish_params;
#[path = "subscription/publish_state_notify.rs"]
mod publish_state_notify;
#[path = "subscription/range_filter_scope_order.rs"]
mod range_filter_scope_order;
#[path = "subscription/request_update.rs"]
mod request_update;
#[path = "subscription/subscription_limits.rs"]
mod subscription_limits;
#[path = "subscription/terminated_discard.rs"]
mod terminated_discard;
