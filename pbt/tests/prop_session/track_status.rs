//! TRACK_STATUS の EXPIRES のプロパティテスト
//!
//! - 任意の EXPIRES 値を持つ TRACK_STATUS_OK が受理され、値が正規化されて保持される
//! - EXPIRES は単発要求の終端 (FIN の交換と破棄) に影響しない

use pbt::common::test_runner;
use shiguredo_moqt::message::common::TrackNamespace;
use shiguredo_moqt::message_parameter::{
    MessageParameter, MessageParameterValue, MessageParameters, PARAM_EXPIRES,
};
use shiguredo_moqt::session::types::{RequestStreamEnd, SessionState, TrackStatusResponse};
use shiguredo_moqt::track_properties::TrackProperties;

use super::common::{establish_pair, take_send_on_stream, take_send_request};

/// EXPIRES (type 0x08) を 1 つ持つパラメータ列を作る
fn expires_params(expires_ms: u64) -> MessageParameters {
    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter {
        param_type: PARAM_EXPIRES,
        value: MessageParameterValue::VarInt(expires_ms),
    });
    parameters
}

/// 任意の EXPIRES 値が TRACK_STATUS_OK で受理され、0 だけが期限なしに正規化される
///
/// draft-ietf-moq-transport-23 §9.19.16 (EXPIRES Parameter): "If the EXPIRES parameter is
/// 0 or is not present in a message, the request does not expire or expires at an unknown
/// time." TRACK_STATUS は単発要求であるため、値は期限タイマーを起動せずに保持だけされる。
#[test]
fn track_status_ok_expires_is_recorded_and_does_not_affect_discard() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(64, |ctx| {
        let expires_ms = noprop::sample_u64_in(ctx, 0..=10_000);
        let (mut client, mut server) = establish_pair();
        let rid = client
            .send_track_status(
                TrackNamespace::new(vec![b"live".to_vec()])
                    .expect("テストフィクスチャの前提条件を満たす"),
                b"cam".to_vec(),
                MessageParameters::new(),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, message) = take_send_request(&mut client);
        server
            .recv_request(message)
            .expect("テストフィクスチャの前提条件を満たす");
        server
            .send_request_ok(rid, expires_params(expires_ms), TrackProperties::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(rid, ok)
            .expect("テストフィクスチャの前提条件を満たす");

        // EXPIRES=0 は「期限なし」として None に正規化する (不在と区別しない)
        let expected = if expires_ms == 0 {
            None
        } else {
            Some(expires_ms)
        };
        let entry = client
            .track_status_request(rid)
            .expect("テストフィクスチャの前提条件を満たす");
        assert!(
            matches!(
                entry.response,
                Some(TrackStatusResponse::Ok {
                    expires_ms: value,
                    ..
                }) if value == expected
            ),
            "受信した EXPIRES を保持すること (expires_ms={expires_ms})"
        );
        assert_eq!(
            entry.expires_ms(),
            expected,
            "仮の購読期限として参照できること (expires_ms={expires_ms})"
        );

        // 応答受領済みの単発要求は FIN の交換後に破棄できる (EXPIRES の値に依存しない)
        client
            .recv_request_stream_closed(rid, RequestStreamEnd::Fin)
            .expect("テストフィクスチャの前提条件を満たす");
        assert!(
            client.forget_track_status(rid).is_some(),
            "EXPIRES を持っても応答受領済みの entry は破棄できること (expires_ms={expires_ms})"
        );
        assert_eq!(
            client.state(),
            SessionState::Established,
            "EXPIRES の受信でセッションを閉じないこと (expires_ms={expires_ms})"
        );
        Ok(())
    })?;
    Ok(())
}
