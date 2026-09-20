//! prop_session 共有ヘルパー
//!
//! Session ペアの確立とイベント取り出しの共通関数。

use shiguredo_moqt::message::ControlMessage;
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
