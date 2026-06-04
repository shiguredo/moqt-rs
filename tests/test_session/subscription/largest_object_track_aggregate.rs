//! LARGEST_OBJECT を Track 単位で算出する回帰テスト
//! (draft-ietf-moq-transport-21 §3.1.3 (Largest Object) / §9.20.18 (LARGEST OBJECT Parameter))
//!
//! "It contains the largest Location ... in the Track observed by the sending endpoint ...
//!  If Objects have been published on this Track the Publisher MUST include this parameter."
//! 自側 publisher 役が応答系 (SUBSCRIBE_OK / REQUEST_UPDATE_OK / PUBLISH_STATE_NOTIFY) に載せる
//! LARGEST_OBJECT は、同一 Track の publisher 役 subscription 群の最大値
//! (`publisher_track_largest`) から算出する。2 本目の subscription が観測値を持たなくても、
//! 1 本目で publish 済みの値が反映されることを検証する。

use super::*;
use shiguredo_moqt::message::PublishStateNotify;
use shiguredo_moqt::message_parameter::{
    MessageParameter, MessageParameterValue, PARAM_FORWARD, PARAM_LARGEST_OBJECT,
};
use shiguredo_moqt::stream::subgroup::{SubgroupHeader, SubgroupIdMode};

/// server (publisher) 側で 2 本目の SUBSCRIBE を受理して Established にする
///
/// 1 本目の subscription は既に `{group, object}` を publish 済みで、2 本目は観測値を持たない。
fn establish_second_subscription(
    client: &mut Session,
    server: &mut Session,
    first_rid: u64,
    track_alias: u64,
) -> u64 {
    let second_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            second_rid,
            track_alias,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(server);
    client
        .recv_stream_message(second_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_ne!(first_rid, second_rid);
    second_rid
}

/// 1 本目の SUBSCRIBE を Established にし、server 側で {group, object} を公開する
fn establish_first_subscription_with_object(group: u64, object: u64) -> (Session, Session, u64) {
    let (mut client, mut server) = establish_pair();
    let sub_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(sub_rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let stream_id = DataStreamId(1);
    server
        .send_subgroup_header(
            stream_id,
            sub_rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: group,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, object, None)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server, sub_rid)
}

/// 2 本目の SUBSCRIBE_OK に、1 本目で公開済みの LARGEST_OBJECT が載る
#[test]
fn subscribe_ok_uses_track_wide_largest_object() {
    let (mut client, mut server, first_rid) = establish_first_subscription_with_object(3, 7);

    let second_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            second_rid,
            2,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");

    let (_, msg) = take_send_on_stream(&mut server);
    let params = match msg {
        ControlMessage::SubscribeOk(ok) => ok.parameters,
        other => panic!("SubscribeOk が期待されたが {other:?} を受け取った"),
    };
    assert_eq!(
        params.largest_object(),
        Some((3, 7)),
        "観測値を持たない 2 本目の SUBSCRIBE_OK にも同一 Track の最大値が載ること"
    );
    assert_ne!(first_rid, second_rid);
}

/// 2 本目の REQUEST_UPDATE_OK に、1 本目で公開済みの LARGEST_OBJECT が載る
#[test]
fn request_update_ok_uses_track_wide_largest_object() {
    let (mut client, mut server, first_rid) = establish_first_subscription_with_object(4, 2);
    let second_rid = establish_second_subscription(&mut client, &mut server, first_rid, 2);

    // client (subscriber) が 2 本目の subscription へ REQUEST_UPDATE を送る
    let mut update_params = MessageParameters::new();
    update_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    client
        .send_request_update(second_rid, update_params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, update_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(second_rid, update_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // server (publisher) が REQUEST_OK で応答する
    server
        .send_request_ok(second_rid, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");

    let (_, msg) = take_send_on_stream(&mut server);
    let params = match msg {
        ControlMessage::RequestOk(ok) => ok.parameters,
        other => panic!("RequestOk が期待されたが {other:?} を受け取った"),
    };
    assert_eq!(
        params.largest_object(),
        Some((4, 2)),
        "観測値を持たない 2 本目の REQUEST_UPDATE_OK にも同一 Track の最大値が載ること"
    );
}

/// 2 本目の PUBLISH_STATE_NOTIFY に、1 本目で公開済みの LARGEST_OBJECT が補われる
#[test]
fn publish_state_notify_uses_track_wide_largest_object() {
    let (mut client, mut server, first_rid) = establish_first_subscription_with_object(5, 9);
    let second_rid = establish_second_subscription(&mut client, &mut server, first_rid, 2);

    let mut notify_params = MessageParameters::new();
    notify_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    server
        .send_publish_state_notify(second_rid, notify_params)
        .expect("テストフィクスチャの前提条件を満たす");

    let (_, msg) = take_send_on_stream(&mut server);
    let params = match msg {
        ControlMessage::PublishStateNotify(PublishStateNotify { parameters }) => parameters,
        other => panic!("PublishStateNotify が期待されたが {other:?} を受け取った"),
    };
    assert_eq!(
        params.largest_object(),
        Some((5, 9)),
        "観測値を持たない 2 本目の PUBLISH_STATE_NOTIFY にも同一 Track の最大値が補われること"
    );
    assert_eq!(
        params
            .as_slice()
            .iter()
            .filter(|p| p.param_type == PARAM_LARGEST_OBJECT)
            .count(),
        1,
        "LARGEST_OBJECT は 1 件だけ載ること"
    );
}
