//! Namespace 系 / TRACK_STATUS のプロパティテスト
//!
//! - 複数 PUBLISH_NAMESPACE の確立
//! - 複数 TRACK_STATUS の完了
//! - TrackStatusEntry.response の遷移
//! - prefix overlap 検出

use pbt::common::test_runner;
use shiguredo_moqt::message::{ControlMessage, common::TrackNamespace};
use shiguredo_moqt::message_parameter::MessageParameters;
use shiguredo_moqt::session::types::SessionEvent;
use shiguredo_moqt::session::types::{NamespacePublicationState, TrackStatusResponse};
use shiguredo_moqt::track_properties::TrackProperties;

use super::common::{establish_pair, take_send_on_stream, take_send_request};

// 任意個数の PUBLISH_NAMESPACE が両端で Established に至る
#[test]
fn multiple_publish_namespaces_all_establish() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let count = noprop::sample_usize_in(ctx, 1..8) as u32;
        let (mut client, mut server) = establish_pair();
        let mut rids = Vec::new();
        for i in 0..count {
            let ns = TrackNamespace::new(vec![format!("n{i}").into_bytes()])
                .expect("テストフィクスチャの前提条件を満たす");
            let rid = client
                .send_publish_namespace(ns, MessageParameters::new())
                .expect("テストフィクスチャの前提条件を満たす");
            rids.push(rid);
            let (_, m) = take_send_request(&mut client);
            server
                .recv_request(m)
                .expect("テストフィクスチャの前提条件を満たす");
            server
                .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
                .expect("テストフィクスチャの前提条件を満たす");
            let (_, ok) = take_send_on_stream(&mut server);
            client
                .recv_stream_message(rid, ok)
                .expect("テストフィクスチャの前提条件を満たす");
        }
        for rid in rids {
            assert_eq!(
                client
                    .namespace_publication(rid)
                    .expect("テストフィクスチャの前提条件を満たす")
                    .state,
                NamespacePublicationState::Established
            );
            assert_eq!(
                server
                    .namespace_publication(rid)
                    .expect("テストフィクスチャの前提条件を満たす")
                    .state,
                NamespacePublicationState::Established
            );
        }
        Ok(())
    })?;
    Ok(())
}

// TRACK_STATUS を任意個数発行、全て Completed
#[test]
fn multiple_track_status_all_complete() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let count = noprop::sample_usize_in(ctx, 1..8) as u32;
        let (mut client, mut server) = establish_pair();
        let mut rids = Vec::new();
        for i in 0..count {
            let ns = TrackNamespace::new(vec![format!("ts{i}").into_bytes()])
                .expect("テストフィクスチャの前提条件を満たす");
            let rid = client
                .send_track_status(ns, format!("t{i}").into_bytes(), MessageParameters::new())
                .expect("テストフィクスチャの前提条件を満たす");
            rids.push(rid);
            let (_, m) = take_send_request(&mut client);
            server
                .recv_request(m)
                .expect("テストフィクスチャの前提条件を満たす");
            server
                .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
                .expect("テストフィクスチャの前提条件を満たす");
            let (_, ok) = take_send_on_stream(&mut server);
            client
                .recv_stream_message(rid, ok)
                .expect("テストフィクスチャの前提条件を満たす");
        }
        for rid in rids {
            let response = client
                .track_status_request(rid)
                .expect("テストフィクスチャの前提条件を満たす")
                .response;
            assert_eq!(
                response,
                Some(TrackStatusResponse::Ok {
                    largest_location: None
                })
            );
        }
        Ok(())
    })?;
    Ok(())
}

// TrackStatusEntry.response の遷移
//
// - 初期: response == None (旧 Pending)
// - REQUEST_OK 受信後: response == Some(Ok { .. })
// - REQUEST_ERROR 受信後: response == Some(Error)
#[test]
fn track_status_response_starts_none_then_ok() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let ns_id = noprop::sample_usize_in(ctx, 0..1024) as u32;
        let (mut client, mut server) = establish_pair();
        let ns = TrackNamespace::new(vec![format!("ns-{ns_id}").into_bytes()])
            .expect("テストフィクスチャの前提条件を満たす");
        let rid = client
            .send_track_status(ns, b"t".to_vec(), MessageParameters::new())
            .expect("テストフィクスチャの前提条件を満たす");
        // 送信直後は応答待ち
        assert!(
            client
                .track_status_request(rid)
                .expect("テストフィクスチャの前提条件を満たす")
                .response
                .is_none()
        );
        let (_, m) = take_send_request(&mut client);
        server
            .recv_request(m)
            .expect("テストフィクスチャの前提条件を満たす");
        server
            .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(rid, ok)
            .expect("テストフィクスチャの前提条件を満たす");
        let response = client
            .track_status_request(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .response;
        assert_eq!(
            response,
            Some(TrackStatusResponse::Ok {
                largest_location: None
            })
        );
        Ok(())
    })?;
    Ok(())
}

#[test]
fn track_status_response_error_when_request_error() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        use shiguredo_moqt::message::ReasonPhrase;
        let ns_id = noprop::sample_usize_in(ctx, 0..1024) as u32;
        let (mut client, mut server) = establish_pair();
        let ns = TrackNamespace::new(vec![format!("ns-{ns_id}").into_bytes()])
            .expect("テストフィクスチャの前提条件を満たす");
        let rid = client
            .send_track_status(ns, b"t".to_vec(), MessageParameters::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, m) = take_send_request(&mut client);
        server
            .recv_request(m)
            .expect("テストフィクスチャの前提条件を満たす");
        server
            .send_request_error(
                rid,
                0x100,
                0,
                ReasonPhrase::new("nope".to_string())
                    .expect("テストフィクスチャの前提条件を満たす"),
                None,
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, err) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(rid, err)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            client
                .track_status_request(rid)
                .expect("テストフィクスチャの前提条件を満たす")
                .response
                .clone(),
            Some(TrackStatusResponse::Error)
        );
        Ok(())
    })?;
    Ok(())
}

// prefix overlap 検出: 同じ prefix を 2 回送ると PREFIX_OVERLAP 応答
#[test]
fn peer_duplicate_subscribe_namespace_gets_prefix_overlap() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        use shiguredo_moqt::error::REQUEST_PREFIX_OVERLAP;
        use shiguredo_moqt::message::SubscribeNamespace;
        let field_count = noprop::sample_usize_in(ctx, 1..5);
        let (_, mut server) = establish_pair();
        let fields: Vec<Vec<u8>> = (0..field_count)
            .map(|i| format!("p{i}").into_bytes())
            .collect();
        let prefix = TrackNamespace::new(fields).expect("テストフィクスチャの前提条件を満たす");
        server
            .recv_request(ControlMessage::SubscribeNamespace(SubscribeNamespace {
                request_id: 0,
                track_namespace_prefix: prefix.clone(),
                parameters: MessageParameters::new(),
            }))
            .expect("テストフィクスチャの前提条件を満たす");
        // draft-ietf-moq-transport-21 §9.5.2 (Updating Namespace Subscriptions): overlap 判定は active (Established) な購読のみ対象なので、
        // 最初の SUBSCRIBE_NAMESPACE に REQUEST_OK を返して Established にする
        server
            .send_request_ok(0, MessageParameters::new(), TrackProperties::default())
            .expect("テストフィクスチャの前提条件を満たす");
        server
            .recv_request(ControlMessage::SubscribeNamespace(SubscribeNamespace {
                request_id: 2,
                track_namespace_prefix: prefix,
                parameters: MessageParameters::new(),
            }))
            .expect("テストフィクスチャの前提条件を満たす");
        // 最初の REQUEST_OK を読み飛ばし、2 件目への REQUEST_ERROR を拾う
        let mut last = None;
        while let Some(ev) = server.poll_event() {
            if let SessionEvent::SendOnStream {
                request_id,
                message: ControlMessage::RequestError(e),
                ..
            } = ev
                && request_id == 2
            {
                last = Some(e.error_code);
            }
        }
        assert_eq!(last, Some(REQUEST_PREFIX_OVERLAP));
        Ok(())
    })?;
    Ok(())
}
