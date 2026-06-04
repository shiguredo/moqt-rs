//! Namespace 系メッセージの統合テスト
//!
//! PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / TRACK_STATUS / SUBSCRIBE_TRACKS
//! 各メッセージ種別のテストはサブモジュールに分割し、
//! 本ファイルにはいずれにも分類されないテストを置く。

use super::*;
use shiguredo_moqt::track_properties::TrackProperties;

#[path = "namespace/publish_namespace.rs"]
mod publish_namespace;
#[path = "namespace/subscribe_namespace.rs"]
mod subscribe_namespace;
#[path = "namespace/track_status.rs"]
mod track_status;
#[path = "namespace/track_subscription.rs"]
mod track_subscription;

/// Client から Server へ空 URI の GOAWAY を送って Server 側で受信
#[test]
fn goaway_client_to_server_zero_uri() {
    let (mut client, mut server) = establish_pair();
    client
        .send_goaway(Vec::new(), 5000)
        .expect("テストフィクスチャの前提条件を満たす");
    let go_msg = loop {
        match client.poll_event() {
            Some(SessionEvent::SendControl(m)) => break m,
            Some(_) => continue,
            None => panic!("SendControl イベントが期待された"),
        }
    };
    server
        .recv_control(go_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // GoawayReceived イベントで受信内容を検証する
    let mut got = false;
    while let Some(ev) = server.poll_event() {
        if let SessionEvent::GoawayReceived {
            new_session_uri,
            timeout,
            ..
        } = ev
        {
            assert!(new_session_uri.is_empty());
            assert_eq!(timeout, 5000);
            got = true;
            break;
        }
    }
    assert!(got, "GoawayReceived イベントが発行されること");
}
