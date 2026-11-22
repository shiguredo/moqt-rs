use super::*;

use shiguredo_moqt::error::SESSION_CONTROL_MESSAGE_TIMEOUT;
use shiguredo_moqt::session::types::{RequestStreamEnd, SessionError};

/// 次の `SessionEvent::PeerClosed` までイベントを取り出す
///
/// peer 起点の終了は `PeerClosed`、自側起点の終了は `CloseSession` で通知される。
/// 取り出した時点でセッション状態が `Closed` になることもあわせて確認する。
fn poll_until_peer_closed(s: &mut Session) -> SessionError {
    while let Some(event) = s.poll_event() {
        if let SessionEvent::PeerClosed(error) = event {
            return error;
        }
    }
    panic!("PeerClosed イベントが期待されたが発行されなかった")
}

/// peer 起点のセッション終了をコードと理由付きで通知できる
///
/// I/O 層が transport (QUIC の CONNECTION_CLOSE / WebTransport の WT_CLOSE_SESSION) から
/// 取得した終了コードと理由を `recv_session_closed` で渡す経路を、Session の公開 API だけで
/// 固定する。取り出した時点で `Closing` から `Closed` へ遷移することも確認する。
#[test]
fn recv_session_closed_reports_peer_code_and_reason() {
    let (mut client, _) = establish_pair();
    client.recv_session_closed(SESSION_CONTROL_MESSAGE_TIMEOUT, "control message timeout");
    assert_eq!(
        client.state(),
        SessionState::Closing,
        "通知直後は終了イベントが未取り出しのため Closing であること"
    );

    let error = poll_until_peer_closed(&mut client);
    assert_eq!(
        error.code, SESSION_CONTROL_MESSAGE_TIMEOUT,
        "peer が通知した終了コードがそのまま観測できること"
    );
    assert_eq!(
        error.reason, "control message timeout",
        "peer が通知した理由がそのまま観測できること"
    );
    assert_eq!(
        client.state(),
        SessionState::Closed,
        "終了イベントを取り出した時点で Closed になること"
    );
}

/// peer 起点とローカル起点がイベントの variant で区別できる
///
/// `CloseSession` は自側が決めた終了 (I/O 層は終了コードを transport へ送る)、
/// `PeerClosed` は peer が transport 層で終了した通知 (I/O 層は送り返さない) である。
#[test]
fn peer_close_and_local_close_are_distinguished() {
    let (mut client, _) = establish_pair();
    client.close(SESSION_NO_ERROR, "local close");
    let mut local_close = None;
    while let Some(event) = client.poll_event() {
        match event {
            SessionEvent::CloseSession(error) => local_close = Some(error),
            SessionEvent::PeerClosed(_) => {
                panic!("自側の close で PeerClosed を発行しないこと")
            }
            _ => {}
        }
    }
    let local_close = local_close.expect("CloseSession が発行されること");
    assert_eq!(local_close.code, SESSION_NO_ERROR, "自側の終了コード");
    assert_eq!(local_close.reason, "local close", "自側の終了理由");

    let (mut peer_client, _) = establish_pair();
    peer_client.recv_session_closed(SESSION_CONTROL_MESSAGE_TIMEOUT, "");
    let peer_close = poll_until_peer_closed(&mut peer_client);
    assert_eq!(
        peer_close.code, SESSION_CONTROL_MESSAGE_TIMEOUT,
        "peer の終了コード"
    );
    assert_eq!(
        peer_close.reason, "",
        "transport が理由を取得できない場合の空文字列を保持すること"
    );
}

/// `recv_session_closed` の後に届く control stream の終了通知は no-op
///
/// transport の終了に伴って control stream の終端が遅れて届いても、§6.3 違反の
/// `CLOSED_CONTROL_STREAM` を重ねて通知しない (終了イベントは 1 回だけ)。
#[test]
fn control_stream_close_after_recv_session_closed_is_noop() {
    let (mut client, _) = establish_pair();
    client.recv_session_closed(SESSION_CONTROL_MESSAGE_TIMEOUT, "timeout");
    assert!(
        client
            .recv_control_stream_closed(RequestStreamEnd::Reset {
                error_code: Some(42),
                reliable_size: None,
            })
            .is_ok(),
        "終了処理中の control stream 終了通知は Ok で no-op になること"
    );
    assert!(
        client
            .recv_control_stream_closed(RequestStreamEnd::Fin)
            .is_ok(),
        "FIN の終了通知も Ok で no-op になること"
    );
    assert_eq!(
        client.state(),
        SessionState::Closing,
        "終了イベントを取り出すまでは Closing のままであること"
    );

    let mut peer_closed = 0;
    let mut local_closed = 0;
    while let Some(event) = client.poll_event() {
        match event {
            SessionEvent::PeerClosed(_) => peer_closed += 1,
            SessionEvent::CloseSession(_) => local_closed += 1,
            _ => {}
        }
    }
    assert_eq!(peer_closed, 1, "PeerClosed は 1 回だけ発行されること");
    assert_eq!(
        local_closed, 0,
        "後続の control stream 終了通知で CLOSED_CONTROL_STREAM を発行しないこと"
    );
    assert_eq!(client.state(), SessionState::Closed, "Closed になること");
}

/// 理由の長さと UTF-8 を加工せずに保持する
///
/// peer の終了理由は transport が運ぶ値であり、MOQT メッセージの Reason Phrase
/// (§8.5) の 1024 バイト上限は適用しない。transport が受け取った UTF-8 文字列を
/// 切り詰めずに保持することを固定する。
#[test]
fn recv_session_closed_preserves_long_utf8_reason() {
    let (mut client, _) = establish_pair();
    let reason = "接続がタイムアウトしました".repeat(100);
    assert!(
        reason.len() > 1024,
        "1024 バイトを超える理由で切り詰めがないことを確認できること"
    );
    client.recv_session_closed(SESSION_CONTROL_MESSAGE_TIMEOUT, reason.clone());
    let error = poll_until_peer_closed(&mut client);
    assert_eq!(error.code, SESSION_CONTROL_MESSAGE_TIMEOUT);
    assert_eq!(
        error.reason, reason,
        "長い UTF-8 の理由も加工せずに保持すること"
    );
}
