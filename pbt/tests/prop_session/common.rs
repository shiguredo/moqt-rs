//! prop_session 共有ヘルパー
//!
//! Session ペアの確立とイベント取り出しの共通関数。

use shiguredo_moqt::message::ControlMessage;
use shiguredo_moqt::message::common::Location;
use shiguredo_moqt::message_parameter::MessageParameters;
use shiguredo_moqt::parameter::SetupOptions;
use shiguredo_moqt::{
    session::core::Session, session::types::SessionEvent, session::types::Transport,
};

/// Client / Server の Session ペアを確立して返す
pub(crate) fn establish_pair() -> (Session, Session) {
    let mut client = Session::new_client(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let mut server = Session::new_server(Transport::WebTransport, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let c = take_send_control(&mut client);
    let s = take_send_control(&mut server);
    server
        .recv_control(c)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(s)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server)
}

/// SendControl イベントを取り出す
pub(crate) fn take_send_control(s: &mut Session) -> ControlMessage {
    while let Some(e) = s.poll_event() {
        if let SessionEvent::SendControl(msg) = e {
            return msg;
        }
    }
    panic!("SendControl イベントが期待された")
}

/// SendRequest イベントを取り出す
pub(crate) fn take_send_request(s: &mut Session) -> (u64, ControlMessage) {
    while let Some(e) = s.poll_event() {
        if let SessionEvent::SendRequest {
            request_id,
            message,
        } = e
        {
            return (request_id, message);
        }
    }
    panic!("SendRequest イベントが期待された")
}

/// SendOnStream イベントを取り出す
pub(crate) fn take_send_on_stream(s: &mut Session) -> (u64, ControlMessage) {
    while let Some(e) = s.poll_event() {
        if let SessionEvent::SendOnStream {
            request_id,
            message,
            ..
        } = e
        {
            return (request_id, message);
        }
    }
    panic!("SendOnStream イベントが期待された")
}

/// FETCH の初回成功応答に載せるパラメータを組み立てる
///
/// draft-ietf-moq-transport-23 §9.3 (REQUEST_OK) / §9.19.17 (LARGEST OBJECT Parameter):
/// FETCH_OK が運べるのは EXPIRES と LARGEST_OBJECT である。応答範囲の終端は
/// LARGEST_OBJECT が要求範囲内ならその値、そうでなければ要求で明示した End Location に
/// なる (§3.2 (Fetch))。
pub(crate) fn fetch_ok_params(largest_object: Option<Location>) -> MessageParameters {
    fetch_ok_params_with(largest_object, MessageParameters::new())
}

/// 追加のパラメータを持つ FETCH の初回成功応答のパラメータを組み立てる
///
/// `extra` は EXPIRES 等である。LARGEST_OBJECT だけは `largest_object` で与える。
pub(crate) fn fetch_ok_params_with(
    largest_object: Option<Location>,
    mut extra: MessageParameters,
) -> MessageParameters {
    if let Some(largest) = largest_object {
        extra.set_largest_object(largest.group_id, largest.object_id);
    }
    extra
}
