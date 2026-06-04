//! Client / Server ハンドシェイクのプロパティテスト
//!
//! - 両端ハンドシェイクが必ず Established に至ること
//! - 2 度目の SETUP 受信が必ず PROTOCOL_VIOLATION になること

use pbt::common::test_runner;
use shiguredo_moqt::message::{ControlMessage, Setup};
use shiguredo_moqt::parameter::SetupOptions;
use shiguredo_moqt::{
    session::core::Session, session::types::SessionState, session::types::Transport,
};

use super::common::take_send_control;

// 両端ハンドシェイクが必ず Established に至る (AUTHORITY / PATH を使わないケース)
#[test]
fn handshake_reaches_established() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let client_is_wt = noprop::sample_bool(ctx);
        let transport = if client_is_wt {
            Transport::WebTransport
        } else {
            Transport::Quic
        };
        let client_opts = SetupOptions::new();

        let server_opts = SetupOptions::new();

        let mut client = Session::new_client(transport, client_opts)
            .expect("テストフィクスチャの前提条件を満たす");
        let mut server = Session::new_server(transport, server_opts)
            .expect("テストフィクスチャの前提条件を満たす");

        let client_setup = take_send_control(&mut client);
        let server_setup = take_send_control(&mut server);

        server
            .recv_control(client_setup)
            .expect("テストフィクスチャの前提条件を満たす");
        client
            .recv_control(server_setup)
            .expect("テストフィクスチャの前提条件を満たす");

        assert_eq!(client.state(), SessionState::Established);
        assert_eq!(server.state(), SessionState::Established);
        Ok(())
    })?;
    Ok(())
}

// 2 度目の SETUP 受信は必ず PROTOCOL_VIOLATION
#[test]
fn second_peer_setup_is_always_violation() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let client_is_wt = noprop::sample_bool(ctx);
        let transport = if client_is_wt {
            Transport::WebTransport
        } else {
            Transport::Quic
        };
        let mut client = Session::new_client(transport, SetupOptions::new())
            .expect("テストフィクスチャの前提条件を満たす");
        client
            .recv_control(ControlMessage::Setup(Setup {
                options: SetupOptions::new(),
            }))
            .expect("テストフィクスチャの前提条件を満たす");
        let err = client
            .recv_control(ControlMessage::Setup(Setup {
                options: SetupOptions::new(),
            }))
            .unwrap_err();
        assert_eq!(err.code, shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION);
        Ok(())
    })?;
    Ok(())
}
