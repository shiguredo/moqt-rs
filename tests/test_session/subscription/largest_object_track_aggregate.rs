//! LARGEST_OBJECT を Track 単位で算出する回帰テスト
//! (draft-ietf-moq-transport-22 §3.1.4 (Largest Object) / §9.20.17 (LARGEST OBJECT Parameter))
//!
//! "It contains the Largest Object (Section 3.1.4) in the Track from the perspective of the
//!  sending endpoint. If Objects have been published on this Track the Publisher MUST include
//!  this parameter."
//! 自側 publisher 役が応答系 (SUBSCRIBE_OK / REQUEST_UPDATE_OK / PUBLISH_STATE_NOTIFY) に載せる
//! LARGEST_OBJECT は、live な publisher 役 subscription 群の最大値と、Track 単位で保持した
//! 自側 publish 済み最大の大きい方 (`publisher_track_largest`) から算出する。2 本目の
//! subscription が観測値を持たなくても 1 本目で publish 済みの値が反映されること、および
//! publisher 役 subscription をすべて forget した後も保持値が反映されることを検証する。

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

/// publisher 役 subscription を forget する (送信 stream の終端 → PUBLISH_DONE → forget)
///
/// draft-ietf-moq-transport-22 §9.9 (PUBLISH_DONE): Stream Count は publisher が開いた
/// data stream 数 (空の Subgroup を含む) を通知する。publisher は開いた全 stream を閉じるまで
/// PUBLISH_DONE を送れない (同節の MUST NOT)。忘れた後も LARGEST_OBJECT を載せられることを
/// 検証するための前提を作る。
fn forget_first_publisher_subscription(
    client: &mut Session,
    server: &mut Session,
    stream_id: Option<DataStreamId>,
    request_id: u64,
    published_streams: u64,
) {
    use shiguredo_moqt::message::ReasonPhrase;

    if let Some(stream_id) = stream_id {
        server
            .send_data_stream_closed(stream_id, RequestStreamEnd::Fin)
            .expect("テストフィクスチャの前提条件を満たす");
    }
    server
        .send_publish_done(
            request_id,
            0x2,
            published_streams,
            ReasonPhrase::new("ended").expect("テストフィクスチャの前提条件を満たす"),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, done_msg) = take_send_on_stream(server);
    client
        .recv_stream_message(request_id, done_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        server.forget_subscription(request_id).is_some(),
        "publisher 役 subscription を破棄できること"
    );
}

/// publisher 役 subscription を forget した後も、publish 済みの Track 最大を載せる
///
/// draft-ietf-moq-transport-22 §9.20.17 (LARGEST OBJECT Parameter): "If Objects have been
/// published on this Track the Publisher MUST include this parameter." LARGEST_OBJECT は
/// Track 単位の値であるため、publisher 役 subscription を forget しても、自側が publish した
/// 最大を保持して応答に載せる必要がある。
#[test]
fn subscribe_ok_uses_retained_largest_object_after_forget() {
    let (mut client, mut server, first_rid) = establish_first_subscription_with_object(3, 7);
    forget_first_publisher_subscription(
        &mut client,
        &mut server,
        Some(DataStreamId(1)),
        first_rid,
        1,
    );

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
        "forget 後も publish 済みの Track 最大が載ること"
    );
}

/// forget 後も Track 最大が単調に更新される
///
/// 保持した最大 (3, 7) より新しい (4, 2) を別 subscription で publish した後は、
/// Track 最大が (4, 2) になる。
#[test]
fn retained_largest_object_updates_monotonically_across_subscriptions() {
    let (mut client, mut server, first_rid) = establish_first_subscription_with_object(3, 7);
    forget_first_publisher_subscription(
        &mut client,
        &mut server,
        Some(DataStreamId(1)),
        first_rid,
        1,
    );

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
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(second_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    let stream_id = DataStreamId(2);
    server
        .send_subgroup_header(
            stream_id,
            second_rid,
            &SubgroupHeader {
                track_alias: 2,
                group_id: 4,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 2, None)
        .expect("テストフィクスチャの前提条件を満たす");

    // 2 本目も forget し、live な publisher 役 subscription が無い状態にする
    forget_first_publisher_subscription(&mut client, &mut server, Some(stream_id), second_rid, 1);

    let third_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(
            third_rid,
            3,
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
        Some((4, 2)),
        "保持値と新しい送信値の大きい方が載ること"
    );
}

/// アプリ指定の LARGEST_OBJECT は保持値との max で扱う
#[test]
fn subscribe_ok_merges_app_supplied_and_retained_largest_object() {
    let (mut client, mut server, first_rid) = establish_first_subscription_with_object(3, 7);
    forget_first_publisher_subscription(
        &mut client,
        &mut server,
        Some(DataStreamId(1)),
        first_rid,
        1,
    );

    // 観測値より小さいアプリ指定値は観測値まで引き上げる
    let second_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut parameters = MessageParameters::new();
    parameters.set_largest_object(1, 0);
    server
        .send_subscribe_ok(second_rid, 2, parameters, TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_on_stream(&mut server);
    let params = match msg {
        ControlMessage::SubscribeOk(ok) => ok.parameters,
        other => panic!("SubscribeOk が期待されたが {other:?} を受け取った"),
    };
    assert_eq!(
        params.largest_object(),
        Some((3, 7)),
        "小さいアプリ指定値は観測値まで引き上げること"
    );

    // 観測値より大きいアプリ指定値はそのまま載せる
    let third_rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut parameters = MessageParameters::new();
    parameters.set_largest_object(9, 5);
    server
        .send_subscribe_ok(third_rid, 3, parameters, TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_on_stream(&mut server);
    let params = match msg {
        ControlMessage::SubscribeOk(ok) => ok.parameters,
        other => panic!("SubscribeOk が期待されたが {other:?} を受け取った"),
    };
    assert_eq!(
        params.largest_object(),
        Some((9, 5)),
        "大きいアプリ指定値はそのまま載せること"
    );
}

/// Object を publish していない Track では自動補完しない (アプリ指定値は保持する)
#[test]
fn subscribe_ok_without_published_objects_has_no_auto_largest_object() {
    let (mut client, mut server) = establish_pair();

    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_on_stream(&mut server);
    let params = match msg {
        ControlMessage::SubscribeOk(ok) => ok.parameters,
        other => panic!("SubscribeOk が期待されたが {other:?} を受け取った"),
    };
    assert_eq!(
        params.largest_object(),
        None,
        "publish 実績が無い Track では自動補完しないこと"
    );

    // アプリ指定値は publish 実績が無くてもそのまま載る
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut parameters = MessageParameters::new();
    parameters.set_largest_object(5, 5);
    server
        .send_subscribe_ok(rid, 2, parameters, TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, msg) = take_send_on_stream(&mut server);
    let params = match msg {
        ControlMessage::SubscribeOk(ok) => ok.parameters,
        other => panic!("SubscribeOk が期待されたが {other:?} を受け取った"),
    };
    assert_eq!(
        params.largest_object(),
        Some((5, 5)),
        "アプリ指定値はそのまま載せること"
    );
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

/// datagram だけで publish した Track でも、forget 後は Track 最大を載せる
///
/// `send_object_datagram` 経路の Track 単位保持を固定する。datagram は stream を開かない
/// ため PUBLISH_DONE の Stream Count は 0 である。
#[test]
fn subscribe_ok_uses_retained_largest_object_after_datagram_forget() {
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");

    // (3, 1) を datagram で publish し、より古い (2, 9) を送っても最大は (3, 1) のまま
    server
        .send_object_datagram(rid, 3, 1, None, None, false)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_object_datagram(rid, 2, 9, None, None, false)
        .expect("テストフィクスチャの前提条件を満たす");
    forget_first_publisher_subscription(&mut client, &mut server, None, rid, 0);

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
        Some((3, 1)),
        "datagram 経路でも forget 後に publish 済みの Track 最大が載ること"
    );
}
