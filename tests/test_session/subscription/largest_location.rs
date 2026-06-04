//! LARGEST_OBJECT / largest_location 追跡のテスト
//!
//! draft-ietf-moq-transport-21 で publisher 側の Largest Location 保持 MUST 要件は
//! 削除された。publisher 側は SUBSCRIBE_OK / PUBLISH / REQUEST_UPDATE_OK の
//! LARGEST_OBJECT を `largest_location` に保存しない
//! (自側の観測値は `largest_received_location` で追跡する)。
//! subscriber 側の広告値保存は維持する。

use super::*;

/// publisher 側 send_subscribe_ok に LARGEST_OBJECT を含めても、
/// subscription.largest_location には保存されない
/// (draft-ietf-moq-transport-21 Appendix A.1 #1872)
#[test]
fn send_subscribe_ok_with_largest_object_does_not_save_largest_location() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
    };
    let (mut client, mut server) = establish_pair();
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 10,
            object: 3,
        },
    });
    server
        .send_subscribe_ok(rid, 1, params, TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location
            .is_none(),
        "publisher 側は LARGEST_OBJECT を保存しないこと"
    );
}

/// publisher 側 send_subscribe_ok に LARGEST_OBJECT を含まない場合、
/// subscription.largest_location は None のまま (draft-ietf-moq-transport-21 §3.1 (Subscriptions))
#[test]
fn send_subscribe_ok_without_largest_object_leaves_largest_location_none() {
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
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location
            .is_none()
    );
}

/// publisher 側 send_subscribe_ok が forward_state=0 で確立する場合、LARGEST_OBJECT があっても
/// largest_location には保存されない (draft-ietf-moq-transport-21 Appendix A.1 #1872: publisher MUST retain 削除)
#[test]
fn send_subscribe_ok_forward_zero_does_not_save_largest_location() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD, PARAM_LARGEST_OBJECT,
    };
    let (mut client, mut server) = establish_pair();
    // FORWARD=0 で SUBSCRIBE → publisher 側 forward_state=0 で確立する
    let mut sub_params = MessageParameters::new();
    sub_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), sub_params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // SUBSCRIBE_OK に LARGEST_OBJECT を載せても forward=0 確立なので保存されない
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 7,
            object: 2,
        },
    });
    server
        .send_subscribe_ok(rid, 1, ok_params, TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        0
    );
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location
            .is_none()
    );
}

/// publisher 側 send_publish が forward_state=1 で確立する場合でも、LARGEST_OBJECT は
/// 保存されない (draft-ietf-moq-transport-21 Appendix A.1 #1872)
#[test]
fn send_publish_forward_one_does_not_save_largest_location() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
    };
    let (mut client, _server) = establish_pair();
    // FORWARD 省略 (default 1) の PUBLISH に LARGEST_OBJECT を載せる
    let mut pub_params = MessageParameters::new();
    pub_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 8,
            object: 4,
        },
    });
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            111,
            pub_params,
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1
    );
    assert!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location
            .is_none(),
        "publisher 側は LARGEST_OBJECT を保存しないこと"
    );
}

/// publisher 側 send_publish が forward_state=0 で確立する場合、LARGEST_OBJECT があっても
/// largest_location には保存されない (draft-ietf-moq-transport-21 Appendix A.1 #1872: publisher MUST retain 削除)
#[test]
fn send_publish_forward_zero_does_not_save_largest_location() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD, PARAM_LARGEST_OBJECT,
    };
    let (mut client, _server) = establish_pair();
    // FORWARD=0 + LARGEST_OBJECT の PUBLISH
    let mut pub_params = MessageParameters::new();
    pub_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    pub_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 8,
            object: 4,
        },
    });
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            111,
            pub_params,
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        0
    );
    assert!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location
            .is_none()
    );
}

/// publisher 側 REQUEST_UPDATE_OK で Forward State が 0→1 に遷移する場合でも、
/// LARGEST_OBJECT は保存されない
/// (draft-ietf-moq-transport-21 Appendix A.1 #1872: publisher MUST retain 削除)
///
/// forward==0 で確立した subscription が REQUEST_UPDATE_OK で 0→1 になっても
/// 保存は行わないことを確認する。
#[test]
fn send_ok_for_subscription_forward_zero_to_one_does_not_save_largest_location() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD, PARAM_LARGEST_OBJECT,
    };
    let (mut client, mut server) = establish_pair();
    // FORWARD=0 で SUBSCRIBE して publisher 側 forward_state=0 で確立する
    let mut sub_params = MessageParameters::new();
    sub_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), sub_params)
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
    // forward=0 で確立したので largest_location は未保存
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        0
    );
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location
            .is_none()
    );
    // Client から REQUEST_UPDATE で FORWARD=1 (0→1) を送信する
    let mut upd_params = MessageParameters::new();
    upd_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    client
        .send_request_update(rid, upd_params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // Server が REQUEST_OK に LARGEST_OBJECT を含めて応答する。
    // 0→1 遷移でも publisher 側は保存しない。forward_state の遷移自体は行われる。
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 20,
            object: 5,
        },
    });
    server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1
    );
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location
            .is_none(),
        "publisher 側は 0→1 遷移でも保存しないこと"
    );
}

/// publisher 側 REQUEST_UPDATE_OK で Forward State が 1→1 (遷移なし) の場合、
/// LARGEST_OBJECT は保存されない
/// (draft-ietf-moq-transport-21 Appendix A.1 #1872: publisher MUST retain 削除)
#[test]
fn send_ok_for_subscription_forward_one_to_one_does_not_save_largest_location() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
    };
    let (mut client, mut server) = establish_pair();
    // FORWARD 省略 (default 1) で SUBSCRIBE → forward_state=1 で確立する
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // 確立時の SUBSCRIBE_OK で LARGEST_OBJECT {10,3} を載せても保存されない
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 10,
            object: 3,
        },
    });
    server
        .send_subscribe_ok(rid, 1, ok_params, TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location
            .is_none(),
        "publisher 側は確立時に保存しないこと"
    );
    // REQUEST_UPDATE (forward 変更なし、1→1) → REQUEST_OK で別の LARGEST_OBJECT {99,99} を載せても
    // 保存されない
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut upd_ok_params = MessageParameters::new();
    upd_ok_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 99,
            object: 99,
        },
    });
    server
        .send_request_ok(rid, upd_ok_params, TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1
    );
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location
            .is_none(),
        "publisher 側は 1→1 でも保存しないこと"
    );
}

/// publisher 側で largest_received_location が設定されている状態で REQUEST_UPDATE_OK が
/// Forward State を 0→1 に遷移させる場合でも、largest_location には保存されない
/// (draft-ietf-moq-transport-21 Appendix A.1 #1872: publisher MUST retain 削除)。
/// 送出パラメータへの effective 値 (largest_received_location との max) の注入は維持する。
#[test]
fn send_ok_for_subscription_effective_largest_object_injected_without_saving() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_FORWARD, PARAM_LARGEST_OBJECT,
    };
    let (mut client, mut server) = establish_pair();
    // FORWARD=1 (デフォルト) で確立してから object を公開し、そのあと FORWARD=0 に落とす。
    // draft-ietf-moq-transport-21 §3.1 (Subscriptions): "The publisher does not send Objects if
    // the Forward State is 0" なので、forward=0 のまま object を公開することはできない。
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
    // Publisher 側でオブジェクトを公開し largest_received_location を設定する
    let stream_id = DataStreamId(10);
    server
        .send_subgroup_header(
            stream_id,
            rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: 5,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 9, None)
        .expect("テストフィクスチャの前提条件を満たす");
    // REQUEST_UPDATE で FORWARD=0 に落とし、publisher 側 forward_state=0 を作る
    let mut off_params = MessageParameters::new();
    off_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    client
        .send_request_update(rid, off_params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, off_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, off_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, off_ok) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, off_ok)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        0
    );
    // Client から REQUEST_UPDATE で FORWARD=1 (0→1) を送信する
    let mut upd_params = MessageParameters::new();
    upd_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    client
        .send_request_update(rid, upd_params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // Server が REQUEST_OK に caller の LARGEST_OBJECT (1, 0) を含めて応答する。
    // 0→1 遷移でも保存はしない。effective_largest_object は
    // largest_received_location (5, 9) を返すため、parameters は (5, 9) に調整される。
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 1,
            object: 0,
        },
    });
    server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1
    );
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location
            .is_none(),
        "publisher 側は保存しないこと"
    );
    // 送出パラメータへの effective 値の注入は維持する
    let (_, ok_msg) = take_send_on_stream(&mut server);
    match ok_msg {
        ControlMessage::RequestOk(ok) => {
            assert_eq!(
                ok.parameters.largest_object(),
                Some((5, 9)),
                "送出 LARGEST_OBJECT は effective 値に調整されること"
            );
        }
        _ => panic!("REQUEST_OK が期待される"),
    }
}

/// subscriber-responder 経路 (my_role==Subscriber) の REQUEST_UPDATE_OK では、publisher 側の
/// 保存廃止と無関係に、Forward State が 0→1 遷移でなくても (ここでは 1→1) LARGEST_OBJECT が
/// 従来どおり largest_location に無条件保存される (send_ok_for_subscription は responder
/// 共通関数であり my_role==Subscriber でも末尾保存に到達する)
#[test]
fn send_ok_for_subscription_subscriber_responder_saves_largest_location_unconditionally() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
    };
    let (mut client, mut server) = establish_pair();
    // client=publisher-initiator が PUBLISH (default forward=1)、server=subscriber が REQUEST_OK で確立する
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            11,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // server は my_role=Subscriber (non-initiator)、forward=1、largest_location 未保存
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .my_role,
        TrackRole::Subscriber
    );
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1
    );
    assert!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location
            .is_none()
    );
    // client=publisher-initiator が REQUEST_UPDATE (forward 変更なし、1→1) を送る
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // server (subscriber, responder) が REQUEST_OK に LARGEST_OBJECT {12,8} を載せて応答する。
    // my_role==Subscriber 経路なので publisher gate の対象外、1→1 でも従来どおり保存される。
    let mut req_ok_params = MessageParameters::new();
    req_ok_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 12,
            object: 8,
        },
    });
    server
        .send_request_ok(rid, req_ok_params, TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .forward_state,
        1
    );
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location,
        Some(Location {
            group_id: 12,
            object_id: 8,
        })
    );
}

/// publisher initiator が REQUEST_UPDATE_OK の LARGEST_OBJECT を受信しても保存しない
/// (draft-ietf-moq-transport-21 Appendix A.1 #1872: publisher MUST retain 削除)
#[test]
fn initiator_publisher_ignores_largest_object_in_request_update_ok() {
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
    };
    let (mut client, mut server) = establish_pair();
    // client=publisher-initiator が PUBLISH し、server=subscriber が REQUEST_OK で確立する
    let rid = client
        .send_publish(
            ns(&[b"live"]),
            b"cam".to_vec(),
            11,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, pub_msg) = take_send_request(&mut client);
    server
        .recv_request(pub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // client=publisher-initiator が REQUEST_UPDATE を送る
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, upd_msg) = take_send_on_stream(&mut client);
    server
        .recv_stream_message(rid, upd_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // server=subscriber-responder が LARGEST_OBJECT 付き REQUEST_OK を返す。
    // client=publisher-initiator 側では保存されない。
    let mut ok_params = MessageParameters::new();
    ok_params.push(MessageParameter {
        param_type: PARAM_LARGEST_OBJECT,
        value: MessageParameterValue::Location {
            group: 12,
            object: 8,
        },
    });
    server
        .send_request_ok(rid, ok_params, TrackProperties::default())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg2) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg2)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_location
            .is_none(),
        "publisher initiator は REQUEST_UPDATE_OK の LARGEST_OBJECT を保存しないこと"
    );
}

/// 確立時に publisher 視点の Largest で相対フィルタが解決される
///
/// 配信済み {0, 0} / {0, 1} がある track への新規 SUBSCRIBE (NextObject) は
/// filter_start {0, 2} になる (先頭 {0, 0} ではなく履歴再送にならない)。
/// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): Largest Object は
/// publisher がメッセージを処理する視点で定義される。
#[test]
fn subscribe_establishment_resolves_relative_filter_with_publisher_largest() {
    use shiguredo_moqt::message::common::Location;
    use shiguredo_moqt::message_parameter::{
        LocationFilter, MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
    };
    use shiguredo_moqt::stream::subgroup::SubgroupHeader;
    use shiguredo_moqt::stream::subgroup::SubgroupIdMode;
    let (mut client, mut server) = establish_pair();
    // 既存 subscription を確立する
    let rid1 = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subscribe_ok(rid1, 1, MessageParameters::new(), TrackProperties::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid1, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    // {0, 0} / {0, 1} を配信済みにする
    let stream_id = DataStreamId(200);
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(stream_id, rid1, &header)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 0, None)
        .expect("テストフィクスチャの前提条件を満たす");
    server
        .send_subgroup_object(stream_id, 1, None)
        .expect("テストフィクスチャの前提条件を満たす");
    // 新規 SUBSCRIBE (NextObject フィルタ付き)
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(LocationFilter::NextObject.encode_to_bytes()),
    });
    let rid2 = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), params)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg2) = take_send_request(&mut client);
    server
        .recv_request(sub_msg2)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        server
            .subscription(rid2)
            .expect("テストフィクスチャの前提条件を満たす")
            .filter_start,
        Some(Location {
            group_id: 0,
            object_id: 2,
        }),
        "確立時は publisher 視点の Largest で解決されること"
    );
}
